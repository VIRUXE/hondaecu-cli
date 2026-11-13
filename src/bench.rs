// The one instruction step every runner shares, and a bench that runs a ROM
// on the modelled board while logging what an outside observer could see.

use crate::bus::{Bus, CPU_HZ, P28_PC_MIL};
use crate::cpu::Cpu;
use crate::engine::EngineState;
use crate::exec::{self, ExecError};
use crate::interrupts::InterruptController;
use crate::ppi82c55::{PORT_B, PORT_C};

/// Why execution cannot continue.
#[derive(Debug, Clone, PartialEq)]
pub enum Stop {
    /// The ROM holds bytes that are not an instruction.
    UndefinedOpcode { pc: u16, byte: u8 },
    /// A real instruction this emulator has no semantics for yet.
    Unimplemented { pc: u16, mnemonic: &'static str },
}

impl std::fmt::Display for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Stop::UndefinedOpcode { pc, byte } => {
                write!(f, "undefined opcode {byte:#04X} at {pc:#06X}")
            }
            Stop::Unimplemented { pc, mnemonic } => {
                write!(f, "unimplemented instruction `{mnemonic}` at {pc:#06X}")
            }
        }
    }
}

impl From<ExecError> for Stop {
    fn from(error: ExecError) -> Self {
        match error {
            ExecError::UndefinedOpcode { pc, byte } => Stop::UndefinedOpcode { pc, byte },
            ExecError::Unimplemented { pc, mnemonic } => Stop::Unimplemented { pc, mnemonic },
        }
    }
}

/// Execute one instruction, advance timers, emit crank/VSS edges when `crank`
/// is true, dispatch interrupts. Returns the mnemonic executed. On failure the
/// CPU is marked halted.
pub fn step_machine(
    cpu: &mut Cpu,
    bus: &mut Bus,
    engine: &mut EngineState,
    crank: bool,
) -> Result<&'static str, Stop> {
    step_machine_dispatched(cpu, bus, engine, crank).map(|(mnemonic, _)| mnemonic)
}

/// `step_machine`, also reporting whether an interrupt was dispatched.
pub fn step_machine_dispatched(
    cpu: &mut Cpu,
    bus: &mut Bus,
    engine: &mut EngineState,
    crank: bool,
) -> Result<(&'static str, bool), Stop> {
    let before = cpu.cycles;
    let decoded = match exec::step(cpu, bus) {
        Ok(decoded) => decoded,
        Err(error) => {
            cpu.halted = true;
            return Err(error.into());
        }
    };
    let mut irq = bus.tick_timers((cpu.cycles - before) as u32);
    if crank {
        irq |= engine.check_distributor_pulses(bus, cpu.cycles, CPU_HZ);
    }
    let dispatched = InterruptController::handle_pending_interrupts(cpu, bus, irq);
    Ok((decoded.mnemonic, dispatched))
}

/// A BRK the ROM executed; `pc` is the address of the BRK itself.
#[derive(Debug, Clone, PartialEq)]
pub struct Break {
    pub t: f64,
    pub pc: u16,
}

/// Everything about a run that is visible from outside the CPU.
#[derive(Debug, Default)]
pub struct RunLog {
    /// The first `BREAKS_KEPT` of them; `break_count` carries on past that.
    pub breaks: Vec<Break>,
    /// Every BRK executed, including those not kept in `breaks`.
    pub break_count: u64,
    /// The BRKs of `break_count` executed inside the boot window.
    pub boot_break_count: u64,
    pub stop: Option<(f64, Stop)>,
    /// `(t, port B, port C)` on every change once both are outputs.
    pub board: Vec<(f64, u8, u8)>,
    pub watchdog_starved_at: Option<f64>,
}

/// `RunLog::breaks` stops growing here so a ROM that breaks on every
/// instruction cannot exhaust memory.
pub const BREAKS_KEPT: usize = 4096;

/// A boot that takes longer than this, or breaks more often than
/// `BOOT_BREAK_LIMIT` inside it, is not a boot.
pub const BOOT_WINDOW_SECONDS: f64 = 1.0;
pub const BOOT_BREAK_LIMIT: usize = 16;

/// Whether a ROM settled after power-on, judged from behaviour alone.
#[derive(Debug, Clone, PartialEq)]
pub enum Settling {
    Settled { boot_breaks: usize },
    /// More than `BOOT_BREAK_LIMIT` BRKs inside the boot window: `breaks`
    /// of them (inside the window only), the last at `last_pc`.
    NeverSettles { breaks: usize, last_pc: u16 },
    /// BRKs after the boot window: `count` of them, the times of those
    /// kept in the log (`BREAKS_KEPT`), `pc` of the first.
    ResetsLater { times: Vec<f64>, pc: u16, count: u64 },
    /// The watchdog went unserved since this time.
    Hung { since: f64 },
    Stopped,
}

impl RunLog {
    /// `Stopped` wins, then `NeverSettles`, then `ResetsLater`, then `Hung`.
    /// `_now` is the ECU time of the end of the run.
    pub fn settling(&self, _now: f64) -> Settling {
        if self.stop.is_some() {
            return Settling::Stopped;
        }
        let boot_kept: Vec<&Break> =
            self.breaks.iter().filter(|b| b.t < BOOT_WINDOW_SECONDS).collect();
        // The kept list is the first `BREAKS_KEPT`, so it holds the boot
        // window's breaks until they overflow it; the counters go on.
        let boot_breaks = (self.boot_break_count as usize).max(boot_kept.len());
        if boot_breaks > BOOT_BREAK_LIMIT {
            let last_pc = boot_kept.last().map_or(0, |b| b.pc);
            return Settling::NeverSettles { breaks: boot_breaks, last_pc };
        }
        let later: Vec<&Break> =
            self.breaks.iter().filter(|b| b.t >= BOOT_WINDOW_SECONDS).collect();
        if let Some(first) = later.first() {
            let count = self.break_count.saturating_sub(self.boot_break_count).max(later.len() as u64);
            return Settling::ResetsLater { times: later.iter().map(|b| b.t).collect(), pc: first.pc, count };
        }
        if let Some(since) = self.watchdog_starved_at {
            return Settling::Hung { since };
        }
        Settling::Settled { boot_breaks }
    }
}

/// The real overflow period is on manual pages missing from the scan; this is
/// an assumption.
pub const WATCHDOG_STARVED_SECONDS: f64 = 0.5;

pub struct Bench {
    pub cpu: Cpu,
    pub bus: Bus,
    pub engine: EngineState,
    pub log: RunLog,
}

impl Bench {
    /// Panics unless `rom.len() == 32768`. PC starts at the reset vector, t = 0.
    /// Callers sync the sensors to the bus themselves.
    pub fn new(rom: &[u8]) -> Bench {
        let mut bus = Bus::new();
        bus.rom.copy_from_slice(rom);
        let mut cpu = Cpu::new();
        cpu.pc = bus.read_code_u16(0x0000);
        Bench { cpu, bus, engine: EngineState::new(), log: RunLog::default() }
    }

    /// Same file rules as `Bus::load_rom_file`.
    pub fn from_file(path: &str) -> std::io::Result<Bench> {
        let mut loader = Bus::new();
        loader.load_rom_file(path)?;
        Ok(Bench::new(&loader.rom))
    }

    /// True once the ROM has programmed the board's PPI (ports are outputs).
    pub fn board_programmed(&self) -> bool {
        self.bus.p28_outputs().is_some()
    }

    pub fn seconds(&self) -> f64 {
        self.cpu.cycles as f64 / CPU_HZ as f64
    }

    /// Run `seconds` of ECU time. Returns false if execution stopped
    /// (`log.stop` is set).
    pub fn run(&mut self, seconds: f64) -> bool {
        self.run_observed(seconds, |_| {})
    }

    /// `run`, calling `observe` after every instruction.
    pub fn run_observed(&mut self, seconds: f64, mut observe: impl FnMut(&mut Bench)) -> bool {
        if self.log.stop.is_some() {
            return false;
        }
        let end = self.cpu.cycles + (seconds * CPU_HZ as f64) as u64;
        let starved_after = (WATCHDOG_STARVED_SECONDS * CPU_HZ as f64) as u64;
        while self.cpu.cycles < end {
            let pc = self.cpu.pc;
            let crank = self.engine.rpm > 0.0;
            match step_machine(&mut self.cpu, &mut self.bus, &mut self.engine, crank) {
                Ok(mnemonic) => {
                    if mnemonic == "BRK" {
                        let t = self.seconds();
                        self.log.break_count += 1;
                        if t < BOOT_WINDOW_SECONDS {
                            self.log.boot_break_count += 1;
                        }
                        if self.log.breaks.len() < BREAKS_KEPT {
                            self.log.breaks.push(Break { t, pc });
                        }
                    }
                }
                Err(stop) => {
                    let t = self.seconds();
                    self.log.stop = Some((t, stop));
                    return false;
                }
            }

            if let (Some(b), Some(c)) = (self.bus.ppi.output(PORT_B), self.bus.ppi.output(PORT_C)) {
                if self.log.board.last().map(|&(_, lb, lc)| (lb, lc)) != Some((b, c)) {
                    let t = self.seconds();
                    self.log.board.push((t, b, c));
                }
            }

            if self.log.watchdog_starved_at.is_none() {
                let served = self.bus.watchdog_last_service_cycle.unwrap_or(0);
                if self.bus.elapsed_cycles.saturating_sub(served) >= starved_after {
                    self.log.watchdog_starved_at = Some(self.seconds());
                }
            }

            observe(self);
        }
        true
    }

    /// MIL (port C bit 4) levels, one entry per change, from `log.board`.
    pub fn mil_edges(&self) -> Vec<(f64, bool)> {
        let mut edges: Vec<(f64, bool)> = Vec::new();
        for &(t, _, c) in &self.log.board {
            let on = c & P28_PC_MIL != 0;
            if edges.last().map(|e| e.1) != Some(on) {
                edges.push((t, on));
            }
        }
        edges
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rom_with(code: &[u8]) -> Vec<u8> {
        // reset vector -> 0x0100, code there
        let mut rom = vec![0xFF; 32768];
        rom[0] = 0x00;
        rom[1] = 0x01;
        rom[0x100..0x100 + code.len()].copy_from_slice(code);
        rom
    }

    #[test]
    fn an_undefined_opcode_stops_the_run_and_is_logged() {
        let mut bench = Bench::new(&rom_with(&[0xB4])); // 0xB4 is undefined
        assert!(!bench.run(0.01));
        assert!(matches!(
            bench.log.stop,
            Some((_, Stop::UndefinedOpcode { pc: 0x0100, byte: 0xB4 }))
        ));
    }

    #[test]
    fn every_brk_is_logged_with_its_address() {
        let mut rom = rom_with(&[0xFF]); // BRK at 0x0100
        rom[2] = 0x00;
        rom[3] = 0x01; // BRK vector -> 0x0100 again
        let mut bench = Bench::new(&rom);
        bench.run(0.001);
        assert!(bench.log.breaks.len() > 10 && bench.log.breaks.iter().all(|b| b.pc == 0x0100));
    }

    #[test]
    fn a_rom_that_never_writes_the_watchdog_is_reported_starved() {
        let mut bench = Bench::new(&rom_with(&[0xCB, 0xFE])); // SJ to itself
        assert!(bench.run(0.6));
        assert!(bench.log.watchdog_starved_at.is_some_and(|t| (0.5..0.52).contains(&t)));
    }

    #[test]
    fn a_blank_chip_never_settles() {
        for fill in [0xFFu8, 0x00] {
            let mut bench = Bench::new(&vec![fill; 32768]);
            bench.run(1.5);
            assert!(
                matches!(
                    bench.log.settling(bench.seconds()),
                    Settling::NeverSettles { .. } | Settling::Stopped | Settling::Hung { .. }
                ),
                "fill {fill:02X}"
            );
        }
    }

    #[test]
    fn breaks_inside_the_boot_window_are_boot() {
        let log = RunLog {
            breaks: vec![Break { t: 0.01, pc: 0x5A8A }, Break { t: 0.03, pc: 0x5A8A }],
            ..Default::default()
        };
        assert!(matches!(log.settling(6.0), Settling::Settled { boot_breaks: 2 }));
    }

    #[test]
    fn a_break_after_the_boot_window_is_a_reset() {
        let log = RunLog {
            breaks: vec![Break { t: 0.01, pc: 0x5A8A }, Break { t: 5.3, pc: 0x21DF }],
            ..Default::default()
        };
        assert!(matches!(log.settling(6.0), Settling::ResetsLater { pc: 0x21DF, .. }));
    }

    #[test]
    fn a_rom_that_breaks_on_every_instruction_keeps_a_bounded_log() {
        let mut rom = rom_with(&[0xFF]);
        rom[2] = 0x00;
        rom[3] = 0x01;
        let mut bench = Bench::new(&rom);
        bench.run(0.05);
        assert_eq!(bench.log.breaks.len(), BREAKS_KEPT);
        assert!(bench.log.break_count > BREAKS_KEPT as u64);
        assert!(matches!(
            bench.log.settling(bench.seconds()),
            Settling::NeverSettles { breaks, .. } if breaks as u64 == bench.log.break_count
        ));
    }

    #[test]
    fn never_settles_counts_only_the_first_seconds_breaks() {
        // Breaking on every instruction for 1.5 s: the count reported is
        // the boot window's, not the whole run's.
        let mut rom = rom_with(&[0xFF]);
        rom[2] = 0x00;
        rom[3] = 0x01;
        let mut bench = Bench::new(&rom);
        bench.run(1.5);
        let log = &bench.log;
        assert!(log.boot_break_count > BREAKS_KEPT as u64 && log.boot_break_count < log.break_count);
        assert!(matches!(
            log.settling(bench.seconds()),
            Settling::NeverSettles { breaks, last_pc: 0x0100 } if breaks as u64 == log.boot_break_count
        ));
    }

    #[test]
    fn resets_later_counts_every_later_break_not_only_those_kept() {
        let log = RunLog {
            breaks: vec![Break { t: 5.3, pc: 0x237D }, Break { t: 10.6, pc: 0x237D }],
            break_count: 20,
            boot_break_count: 0,
            ..Default::default()
        };
        assert!(matches!(log.settling(110.0), Settling::ResetsLater { count: 20, pc: 0x237D, .. }));
    }

    #[test]
    #[cfg_attr(not(feature = "p28-rom"), ignore = "needs the stock P28 ROM: enable feature p28-rom")]
    fn stock_p28_settles_and_programs_the_board() {
        let mut bench = Bench::from_file(&crate::fixtures::p28_rom_path()).unwrap();
        EngineState::new().sync_sensors_to_bus(&mut bench.bus);
        bench.engine.rpm = 0.0;
        bench.run(6.0);
        assert!(matches!(bench.log.settling(6.0), Settling::Settled { .. }));
        assert!(bench.board_programmed() && bench.log.watchdog_starved_at.is_none());
    }

    #[test]
    #[cfg_attr(not(feature = "p28-rom"), ignore = "needs the stock P28 ROM: enable feature p28-rom")]
    fn stock_p28_with_a_bad_checksum_resets_later() {
        let mut rom = std::fs::read(crate::fixtures::p28_rom_path()).unwrap();
        rom.truncate(32768);
        rom[0x7FFF] ^= 0x01; // unused fill byte; sum no longer 0
        let mut bench = Bench::new(&rom);
        EngineState::new().sync_sensors_to_bus(&mut bench.bus);
        bench.engine.rpm = 0.0;
        bench.run(12.0);
        assert!(matches!(bench.log.settling(12.0), Settling::ResetsLater { .. }));
    }

    #[test]
    fn run_advances_ecu_time_by_the_requested_amount() {
        let mut bench = Bench::new(&rom_with(&[0xCB, 0xFE]));
        bench.run(0.25);
        assert!((bench.seconds() - 0.25).abs() < 0.001);
    }
}
