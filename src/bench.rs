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
    /// Every BRK executed.
    pub breaks: Vec<Break>,
    pub stop: Option<(f64, Stop)>,
    /// `(t, port B, port C)` on every change once both are outputs.
    pub board: Vec<(f64, u8, u8)>,
    pub watchdog_starved_at: Option<f64>,
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
                        self.log.breaks.push(Break { t, pc });
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
    fn run_advances_ecu_time_by_the_requested_amount() {
        let mut bench = Bench::new(&rom_with(&[0xCB, 0xFE]));
        bench.run(0.25);
        assert!((bench.seconds() - 0.25).abs() < 0.001);
    }
}
