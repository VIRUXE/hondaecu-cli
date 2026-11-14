//! Why the light is on: an automatic trace, run only when a `check` drive ends
//! with the check-engine lamp lit, from the instruction that lit the lamp back
//! through the data that decided it.
//!
//! Three parts:
//!
//! * `Tracer` rides on a `Bench` (`Bench::tracer`). For every bit of on-chip
//!   data memory it keeps the instruction that last changed it
//!   (`Provenance`), and near a chosen instant a ring of the last `RING`
//!   instructions with the branches they took and the data they read. It
//!   only observes: the bus logs the accesses of the instruction being
//!   executed (`Bus::access`) and nothing else changes. Without a tracer the
//!   bench pays one test of `Bench::tracer` per instruction and the bus one
//!   test of `Bus::access` per access.
//! * `explain` re-runs the same drive (the emulator is deterministic) as
//!   often as it needs, each time pausing at the instant of one instruction,
//!   and walks back one cause per level.
//! * `text_section` / `json_levels` render the levels.
//!
//! The choice of what to follow is a heuristic (`choose`). docs/CHECK.md,
//! "Why the light is on", describes it and its limits.

use std::collections::VecDeque;

use crate::bench::{step_machine_with, Bench, Stop};
use crate::bus::{p28_ppi_register, Bus, DataWrite, CPU_HZ, EXTERNAL_DATA_START, RAM_SIZE, SFR_P1};
use crate::car::Setup;
use crate::cpu::Cpu;
use crate::decoder::{self, Decoded};
use crate::engine::EngineState;
use crate::operand::{table, Arg};
use crate::ppi82c55::PORT_C;
use crate::script::{self, RunResult};

/// Instructions kept in the path ring.
pub const RING: usize = 512;
/// Levels walked at most.
pub const MAX_DEPTH: usize = 6;
/// Path lines shown before the instruction of a level.
pub const PATH_LINES: usize = 12;
/// The lamp: 82C55 port C bit 4, kept by the ROM in the P1 latch.
const LAMP_BIT: u8 = 4;
/// A lamp write older than this before the instant looked at is not what
/// keeps the lamp lit.
const LAMP_RECENT_SECONDS: f64 = 1.0;
/// A writer this close to an instruction of the path is the routine's own
/// code: what it wrote is the routine's bookkeeping.
const OWN_CODE: u16 = 0x40;
/// Distinct writers remembered per byte.
const WRITERS: usize = 4;

/// One executed instruction, as provenance remembers it. `index` is the
/// instruction count after it (`Cpu::instructions`); 0 is "never".
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Stamp {
    pub pc: u16,
    pub index: u64,
    pub cycles: u64,
    /// The write read the same byte first and wrote it back changed: a
    /// counter or a shift register working on itself, or an exchange.
    pub counted: bool,
    /// Operation of the instruction (`DEC`, `XCHG`, ...).
    pub op: &'static str,
    /// A byte write whose value came from the instruction itself (an
    /// immediate or a clear): it read no data and no register.
    pub constant: bool,
}

impl Stamp {
    pub fn is_set(&self) -> bool {
        self.index != 0
    }
    pub fn seconds(&self) -> f64 {
        self.cycles as f64 / CPU_HZ as f64
    }
}

/// For every bit of `0x0000..RAM_SIZE`.
pub struct Provenance {
    /// The instruction that last changed the bit.
    pub changed: Vec<Stamp>,
    /// The instruction that last wrote the bit, changed or not: where the
    /// value read came from. A bit instruction writes one bit; any other
    /// write all eight.
    pub written: Vec<Stamp>,
    /// The last write of the bit that was not the byte counting on itself or
    /// an exchange: for a counter, the last load; otherwise `written`.
    pub origin: Vec<Stamp>,
    /// The same for changes: the last change that was not counting or an
    /// exchange (the ROM's RAM refresh swaps every byte out and back).
    pub origin_change: Vec<Stamp>,
    /// Per byte, the latest distinct writers (pc, index), most recent first.
    pub writers: Vec<[(u16, u64); WRITERS]>,
}

impl Provenance {
    fn new() -> Provenance {
        Provenance {
            changed: vec![Stamp::default(); RAM_SIZE * 8],
            written: vec![Stamp::default(); RAM_SIZE * 8],
            origin: vec![Stamp::default(); RAM_SIZE * 8],
            origin_change: vec![Stamp::default(); RAM_SIZE * 8],
            writers: vec![[(0, 0); WRITERS]; RAM_SIZE],
        }
    }

    /// Record the writes of one instruction. `mask` gives, per write, the
    /// bits the instruction meant to write (a bit instruction writes one bit
    /// of the byte it rewrites); `counted` whether it read the byte first (a
    /// counter, a shift, an exchange).
    pub fn record(&mut self, stamp: Stamp, writes: &[(DataWrite, u8, bool)]) {
        for &(w, mask, counted) in writes {
            if w.addr >= EXTERNAL_DATA_START {
                continue;
            }
            let byte = w.addr as usize;
            let changed = w.old ^ w.new;
            let s = Stamp { counted, ..stamp };
            for bit in 0..8 {
                let i = byte * 8 + bit;
                if changed & (1 << bit) != 0 {
                    self.changed[i] = s;
                    if !counted {
                        self.origin_change[i] = s;
                    }
                }
                if (mask | changed) & (1 << bit) != 0 {
                    self.written[i] = s;
                    // An exchange never makes an origin: the ROM's RAM
                    // refresh swaps every byte out and back.
                    if !counted {
                        self.origin[i] = s;
                    }
                }
            }
            if mask != 0 {
                let list = &mut self.writers[byte];
                let keep: Vec<(u16, u64)> =
                    list.iter().copied().filter(|&(pc, i)| i != 0 && pc != stamp.pc).collect();
                *list = [(0, 0); WRITERS];
                list[0] = (stamp.pc, stamp.index);
                for (slot, w) in list.iter_mut().skip(1).zip(keep) {
                    *slot = w;
                }
            }
        }
    }
}

/// A data read of a recorded instruction, with the provenance of each bit
/// as it stood when it was read.
#[derive(Debug, Clone, PartialEq)]
pub struct Read {
    pub addr: u16,
    pub value: u8,
    pub outside: u8,
    /// Bits that count as an input of the instruction: one for a bit test,
    /// none for a pointer register, a stack pop or the byte a bit
    /// instruction rewrites, all otherwise.
    pub mask: u8,
    /// Per bit: where the value came from; default for external reads.
    pub stamps: [Source; 8],
}

/// Provenance of one bit as it stood when it was read.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Source {
    pub written: Stamp,
    pub origin: Stamp,
    pub changed: Stamp,
    pub origin_change: Stamp,
}

/// One instruction in the path ring.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub index: u64,
    pub cycles: u64,
    pub pc: u16,
    /// Decoding context (`Cpu::dd`) when it was fetched.
    pub dd: bool,
    /// Operation without its byte suffix (`SB`, `JBS`, `L`, ...).
    pub op: &'static str,
    pub len: u8,
    /// For a conditional branch, whether it was taken.
    pub branch: Option<bool>,
    /// An interrupt was entered right after it.
    pub interrupted: bool,
    pub bit: Option<u8>,
    /// First address of the local register bank (r0..r7) it ran with.
    pub bank: u16,
    /// Registers and flags read and written (`REG_A`, `REG_C`, `REG_Z`).
    pub uses: u8,
    pub defs: u8,
    /// For a conditional branch, the address it did not go to.
    pub alt: Option<u16>,
    pub reads: Vec<Read>,
    /// ROM bytes read by LC / CMPC (not instruction fetches).
    pub rom_reads: Vec<(u16, u8)>,
    pub writes: Vec<DataWrite>,
}

impl Step {
    fn is_call(&self) -> bool {
        matches!(self.op, "CAL" | "SCAL" | "VCAL")
    }
}

/// The last instruction seen to drive the lamp on, and the ring as it stood
/// then when the ring was being kept.
#[derive(Debug, Clone)]
pub struct LampWrite {
    pub stamp: Stamp,
    pub ring: Option<Vec<Step>>,
}

pub struct Tracer {
    pub prov: Provenance,
    pub ring: VecDeque<Step>,
    pub capacity: usize,
    /// The ring is kept from this instruction index or this cycle count on,
    /// whichever comes first.
    pub ring_from_index: u64,
    pub ring_from_cycles: u64,
    pub stop_at_index: Option<u64>,
    pub stop_at_cycles: Option<u64>,
    /// The stop was reached: the bench runs no further instruction.
    pub paused: bool,
    /// Last instruction that set the lamp bit of the P1 latch.
    pub p1_lamp: Option<LampWrite>,
    /// Last instruction that wrote 82C55 port C with the lamp bit set.
    pub port_c_lamp: Option<LampWrite>,
}

pub const REG_A: u8 = 1;
pub const REG_C: u8 = 2;
pub const REG_Z: u8 = 4;

fn arg_register(a: &Arg) -> u8 {
    use crate::operand::Reg;
    match a {
        Arg::Reg(Reg::A) => REG_A,
        Arg::Carry => REG_C,
        Arg::Reg(Reg::Psw | Reg::PswH) => REG_C | REG_Z,
        // PSWH.7 is CF and PSWH.6 ZF.
        Arg::Bit(inner, n) if **inner == Arg::Reg(Reg::PswH) => match n {
            7 => REG_C,
            6 => REG_Z,
            _ => 0,
        },
        _ => 0,
    }
}

/// The CPU registers and flags an instruction reads and writes (memory is
/// in its recorded accesses). Follows `exec.rs`.
pub fn registers(op: &str, args: &[Arg], carry_dest: bool) -> (u8, u8) {
    let a0 = args.first().map_or(0, arg_register);
    let a1 = args.get(1).map_or(0, arg_register);
    match op {
        "L" | "LC" => (a1, a0 | REG_Z),
        "MOV" => (a1, a0),
        "ST" => (a0, a1),
        "CLR" => (0, a0 | if a0 & REG_A != 0 { REG_Z } else { 0 }),
        "XCHG" => (a0 | a1, a0 | a1),
        "ADD" | "SUB" => (a0 | a1, a0 | REG_C | REG_Z),
        "ADC" | "SBC" => (a0 | a1 | REG_C, a0 | REG_C | REG_Z),
        "CMP" | "CMPC" => (a0 | a1, REG_C | REG_Z),
        "AND" | "OR" | "XOR" => (a0 | a1, a0 | REG_Z),
        "INC" | "DEC" => (a0, a0 | REG_Z),
        "MUL" => (REG_A, REG_A | REG_Z),
        "DIV" => (REG_A, REG_A | REG_C | REG_Z),
        "EXTND" | "SWAP" => (REG_A, REG_A),
        "ROL" | "ROR" => (a0 | REG_C, a0 | REG_C),
        "SLL" | "SRL" | "SRA" => (a0, a0 | REG_C),
        "SC" | "RC" => (0, REG_C),
        "SB" | "RB" => (0, REG_Z | a0),
        "MB" if carry_dest => (a1, REG_C),
        "MB" => (REG_C, a0),
        "MBR" if carry_dest => (REG_A | a1, REG_C),
        "MBR" => (REG_A | REG_C, a0),
        "SBR" | "RBR" | "TBR" => (REG_A | a0, REG_Z | a0),
        "JEQ" | "JNE" => (REG_Z, 0),
        "JLT" | "JGE" => (REG_C, 0),
        "JGT" | "JLE" => (REG_C | REG_Z, 0),
        "JBS" | "JBR" => (a0, 0),
        "DAA" | "DAS" => (REG_A | REG_C, REG_A | REG_C),
        "XNBL" => (REG_A, REG_A),
        "POPS" => (0, a0),
        "PUSHS" => (a0, 0),
        _ => (0, 0),
    }
}

const CONDITIONAL: [&str; 9] = ["JEQ", "JNE", "JLT", "JGE", "JGT", "JLE", "JBS", "JBR", "JRNZ"];

impl Tracer {
    pub fn new() -> Tracer {
        Tracer {
            prov: Provenance::new(),
            ring: VecDeque::with_capacity(RING),
            capacity: RING,
            ring_from_index: u64::MAX,
            ring_from_cycles: u64::MAX,
            stop_at_index: None,
            stop_at_cycles: None,
            paused: false,
            p1_lamp: None,
            port_c_lamp: None,
        }
    }

    /// `bench::step_machine`, recording.
    pub fn step(
        &mut self,
        cpu: &mut Cpu,
        bus: &mut Bus,
        engine: &mut EngineState,
        crank: bool,
    ) -> Result<&'static str, Stop> {
        let log = bus.access.get_or_insert_with(Box::default);
        log.clear();
        log.on = true;
        let dd = cpu.dd;
        let mut kept = false;
        let result = step_machine_with(cpu, bus, engine, crank, |cpu, bus, pc, d| {
            if let Some(log) = bus.access.as_deref_mut() {
                log.on = false;
            }
            kept = self.after_exec(cpu, bus, pc, dd, d);
        });
        if let Some(log) = bus.access.as_deref_mut() {
            log.on = false;
        }
        let (mnemonic, dispatched) = result?;
        if kept && dispatched
            && let Some(last) = self.ring.back_mut()
        {
            last.interrupted = true;
        }
        if self.stop_at_index.is_some_and(|n| cpu.instructions >= n)
            || self.stop_at_cycles.is_some_and(|c| cpu.cycles >= c)
        {
            self.paused = true;
        }
        Ok(mnemonic)
    }

    /// Returns whether the instruction went into the ring.
    fn after_exec(&mut self, cpu: &Cpu, bus: &Bus, pc: u16, dd: bool, d: &Decoded) -> bool {
        let Some(log) = bus.access.as_deref() else { return false };
        let Some(parsed) = table()[d.index].as_ref() else { return false };
        let op: &'static str = if parsed.byte_width { &parsed.op[..parsed.op.len() - 1] } else { parsed.op };
        let index = cpu.instructions;
        let (uses, defs) = registers(op, &parsed.args, false);
        let _ = defs;
        let pointer_base_now = 0x0080 + cpu.scb() * 8;
        let constant = !matches!(op, "SB" | "RB" | "SBR" | "RBR" | "MB" | "MBR")
            && uses == 0
            && !log.reads.iter().any(|r| !(pointer_base_now..pointer_base_now + 8).contains(&r.addr));
        let stamp = Stamp { pc, index, cycles: cpu.cycles, counted: false, op, constant };

        // What kind of bit instruction it is, and which bit.
        let fixed_bit = parsed.args.iter().find_map(|a| match a {
            Arg::Bit(_, n) => Some(*n),
            _ => None,
        });
        let indirect = matches!(op, "MBR" | "SBR" | "RBR" | "TBR");
        let bit = if indirect { Some((cpu.a & 7) as u8) } else { fixed_bit };
        let carry_dest = parsed.args.first() == Some(&Arg::Carry);
        let bit_write = match op {
            "SB" | "RB" | "SBR" | "RBR" => true,
            "MB" | "MBR" => !carry_dest,
            _ => false,
        };
        let bit_read = matches!(op, "JBS" | "JBR" | "TBR") || (matches!(op, "MB" | "MBR") && carry_dest);

        let pointer_base = 0x0080 + cpu.scb() * 8;
        let stack_pop = matches!(op, "RT" | "RTI" | "POPS");
        let written: Vec<u16> = log.writes.iter().map(|w| w.addr).collect();

        let writes: Vec<(DataWrite, u8, bool)> = log
            .writes
            .iter()
            .map(|&w| {
                let mask = if bit_write { bit.map_or(0, |b| 1 << b) } else { 0xFF };
                let counted = !bit_write && !stack_pop && log.reads.iter().any(|r| r.addr == w.addr);
                let counted = counted || op == "XCHG";
                (w, mask, counted)
            })
            .collect();

        let keep = self.ring_from_index <= index || self.ring_from_cycles <= cpu.cycles;
        if keep {
            let reads = log
                .reads
                .iter()
                .map(|r| {
                    let pointer = (pointer_base..pointer_base + 8).contains(&r.addr);
                    let mask = if pointer || stack_pop || (bit_write && written.contains(&r.addr)) {
                        0
                    } else if bit_read {
                        bit.map_or(0xFF, |b| 1 << b)
                    } else {
                        0xFF
                    };
                    let mut stamps = [Source::default(); 8];
                    if r.addr < EXTERNAL_DATA_START {
                        for (b, s) in stamps.iter_mut().enumerate() {
                            let i = r.addr as usize * 8 + b;
                            *s = Source {
                                written: self.prov.written[i],
                                origin: self.prov.origin[i],
                                changed: self.prov.changed[i],
                                origin_change: self.prov.origin_change[i],
                            };
                        }
                    }
                    Read { addr: r.addr, value: r.value, outside: r.outside, mask, stamps }
                })
                .collect();
            let rom_reads = if matches!(op, "LC" | "CMPC") {
                log.code
                    .borrow()
                    .iter()
                    .copied()
                    .filter(|&(a, _)| a.wrapping_sub(pc) as usize >= d.len)
                    .collect()
            } else {
                Vec::new()
            };
            let branch = CONDITIONAL
                .contains(&op)
                .then(|| cpu.pc != pc.wrapping_add(d.len as u16));
            let next = pc.wrapping_add(d.len as u16);
            let alt = branch.map(|taken| if taken { next } else { next.wrapping_add(d.fields.rel8 as u16) });
            let (uses, defs) = registers(op, &parsed.args, carry_dest);
            if self.ring.len() == self.capacity {
                self.ring.pop_front();
            }
            self.ring.push_back(Step {
                index,
                cycles: cpu.cycles,
                pc,
                dd,
                op,
                len: d.len as u8,
                branch,
                interrupted: false,
                bit,
                bank: cpu.bank_base(),
                uses,
                defs,
                alt,
                reads,
                rom_reads,
                writes: log.writes.clone(),
            });
        }

        self.prov.record(stamp, &writes);

        // The lamp.
        let lamp = 1u8 << LAMP_BIT;
        let p1_on = log.writes.iter().any(|w| {
            w.addr == SFR_P1
                && w.new & lamp != 0
                && if bit_write { bit == Some(LAMP_BIT) } else { op != "AND" }
        });
        let port_c_on = log.writes.iter().any(|w| {
            w.addr >= EXTERNAL_DATA_START && matches!(p28_ppi_register(w.addr), Some(2) | Some(3))
        }) && bus.ppi.output(PORT_C).is_some_and(|c| c & lamp != 0);
        if p1_on || port_c_on {
            let write = LampWrite { stamp, ring: keep.then(|| self.ring.iter().cloned().collect()) };
            if p1_on {
                self.p1_lamp = Some(write.clone());
            }
            if port_c_on {
                self.port_c_lamp = Some(write);
            }
        }
        keep
    }
}

impl Default for Tracer {
    fn default() -> Self {
        Tracer::new()
    }
}

// ---- the walk ------------------------------------------------------------

/// One level of the chain.
#[derive(Debug, Clone, PartialEq)]
pub struct Level {
    /// ECU time of the instruction, seconds.
    pub at: f64,
    pub address: u16,
    pub instruction: String,
    /// What the instruction did that made it this level.
    pub did: String,
    /// The instructions before it, oldest first, the instruction last.
    pub path: Vec<String>,
    pub followed: Option<String>,
    pub others: Vec<String>,
    pub stopped_because: Option<String>,
    /// The instruction only copied data: nothing else on its path counts.
    /// Such a level does not count toward `MAX_DEPTH`.
    pub copy: bool,
}

/// The whole trace. `levels` may be empty when `note` says why.
#[derive(Debug, Clone, PartialEq)]
pub struct Why {
    pub from: String,
    pub levels: Vec<Level>,
    pub note: Option<String>,
}

/// Run the drive again on `rom` with a tracer configured by `tracer`.
fn traced_run(rom: &[u8], setup: &Setup, tracer: Tracer) -> Box<Tracer> {
    let mut bench = Bench::new(rom);
    bench.tracer = Some(Box::new(tracer));
    script::run(&mut bench, setup);
    bench.tracer.take().expect("the tracer stays on the bench")
}

/// Re-run to the instant of instruction `index`, keeping the ring up to it.
fn ring_at(rom: &[u8], setup: &Setup, index: u64) -> Vec<Step> {
    let mut t = Tracer::new();
    t.ring_from_index = index.saturating_sub(RING as u64 - 1);
    t.stop_at_index = Some(index);
    let t = traced_run(rom, setup, t);
    t.ring.into_iter().filter(|s| s.index <= index).collect()
}

/// Explain the lamp at the end of the drive. `result` is the plain run of
/// the same `rom` and `setup`.
pub fn explain(rom: &[u8], setup: &Setup, result: &RunResult) -> Why {
    let empty = |note: &str| Why { from: String::new(), levels: Vec::new(), note: Some(note.to_string()) };
    let Some(&(_, until)) = result.mil_lit_spans.last() else {
        return empty("the run log has no lit span to start from.");
    };
    let until_cycles = (until * CPU_HZ as f64) as u64;
    let recent = (LAMP_RECENT_SECONDS * CPU_HZ as f64) as u64;
    let mut first = Tracer::new();
    first.stop_at_cycles = Some(until_cycles);
    first.ring_from_cycles = until_cycles.saturating_sub(recent / 5);
    let first = traced_run(rom, setup, first);
    let is_recent = |w: &&LampWrite| w.stamp.cycles + recent >= until_cycles;
    let (start, what) = match (first.p1_lamp.as_ref().filter(is_recent), first.port_c_lamp.as_ref().filter(is_recent)) {
        (Some(w), _) => (w.clone(), "set P1.4, the ROM's image of the lamp bit (82C55 port C bit 4)".to_string()),
        (None, Some(w)) => (w.clone(), "wrote 82C55 port C with the lamp bit (bit 4) set".to_string()),
        (None, None) => {
            return empty(&format!(
                "no instruction drove the lamp bit on in the second before {until:.2} s, so there is no instruction to start from."
            ));
        }
    };
    let from = format!(
        "the last instruction before {until:.2} s, when the lit span that lasts to the end of the drive ends, that {what}"
    );
    let ring = start.ring.unwrap_or_else(|| ring_at(rom, setup, start.stamp.index));
    let did = format!("{what}, at the last time before {until:.2} s");
    walk(rom, setup, ring, from, Some(did), ring_at)
}

/// Candidates tried at one level before giving up on it.
const TRIES: usize = 3;
/// Re-runs the whole walk may spend.
const RERUNS: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Outcome {
    /// Ended at an input or a ROM constant.
    Answered,
    /// Ended at the depth limit or a re-run that went wrong.
    NoAnswer,
    /// Nothing on the path could be followed.
    DeadEnd,
}

struct Walker<'a, F> {
    rom: &'a [u8],
    setup: &'a Setup,
    rerun: F,
    reruns: usize,
}

impl<F: FnMut(&[u8], &Setup, u64) -> Vec<Step>> Walker<'_, F> {
    /// The levels from the instruction the ring ends at. A candidate that
    /// leads to a dead end is given up for the next one, up to `TRIES`.
    /// `entered` is the byte and bit the level was followed for.
    fn descend(
        &mut self,
        ring: Vec<Step>,
        did: Option<String>,
        entered: (u16, Option<u8>),
        depth: usize,
        visited: &mut Vec<u16>,
    ) -> (Vec<Level>, Outcome) {
        let rom = self.rom;
        let target = ring.last().cloned().expect("a ring ends at its instruction");
        visited.push(target.pc);
        let context = context(&ring);
        let member = slice(rom, &context);
        let mut level = Level {
            at: target.cycles as f64 / CPU_HZ as f64,
            address: target.pc,
            instruction: disassemble(rom, target.pc, target.dd),
            did: did.unwrap_or_else(|| writes_text(&target)),
            path: shown_lines(rom, &context, &member),
            followed: None,
            others: Vec::new(),
            stopped_because: None,
            copy: is_copy(&target, &member),
        };
        let choice = choose(&context, &member, visited);
        if std::env::var_os("HONDAECU_WHY_DEBUG").is_some() {
            eprintln!("[why] depth {depth} {:04X}h {} copy={} reruns left {}", target.pc, level.instruction, level.copy, self.reruns);
            for c in &choice.all {
                eprintln!("[why]     {:?} {} follow {:04X}h@{} const={}", c.kind, c.describe(), c.follow.pc, c.follow.index, c.follow.constant);
            }
            if let Decision::Stop { why, .. } = &choice.decision {
                eprintln!("[why]   stop: {why}");
            }
        }
        // A copy is a step on the way, not a decision.
        let depth_after = if level.copy { depth } else { depth + 1 };
        let others = |chosen: Option<&Candidate>| -> Vec<String> {
            choice.all.iter().filter(|c| Some(*c) != chosen).map(|c| c.describe()).collect()
        };
        let result = match &choice.decision {
            Decision::Stop { why, on } => {
                level.others = others(on.as_ref());
                let mut why = why.clone();
                // A whole byte read from outside: say which bit this level
                // was entered for, as a lead, not a finding.
                if let (Some(c), Some(b)) = (on.as_ref().filter(|c| c.kind == Kind::Input && c.bit.is_none()), entered.1) {
                    let name = board_name(c.addr, Some(b)).map_or(String::new(), |n| format!(" ({n})"));
                    why.push_str(&format!(
                        " This level was entered for {:03X}h bit {b}: if the byte is carried bit for bit, that is {:04X}h bit {b}{name}; check the instructions between.",
                        entered.0, c.addr
                    ));
                }
                level.stopped_because = Some(why);
                (vec![level], if on.is_some() { Outcome::Answered } else { Outcome::DeadEnd })
            }
            Decision::Follow(list) if depth_after > MAX_DEPTH => {
                level.others = others(None);
                level.stopped_because = Some(format!(
                    "depth limit of {MAX_DEPTH} levels reached; the chain stopped here without an answer. Next would have been {}.",
                    list[0].describe()
                ));
                (vec![level], Outcome::NoAnswer)
            }
            Decision::Follow(list) => {
                let mut tried: Vec<String> = Vec::new();
                let mut first: Option<(Candidate, Vec<Level>, Outcome)> = None;
                let mut found = None;
                for c in list.iter().take(TRIES) {
                    if self.reruns == 0 {
                        break;
                    }
                    self.reruns -= 1;
                    let next = (self.rerun)(rom, self.setup, c.follow.index);
                    if next.last().map(|s| s.index) != Some(c.follow.index) {
                        tried.push(format!("{} (the re-run did not reach {:04X}h again)", c.name(), c.follow.pc));
                        continue;
                    }
                    let entered = (c.addr, c.bit_named());
                    let (sub, outcome) = self.descend(next, Some(c.changed_text()), entered, depth_after, visited);
                    if outcome == Outcome::Answered {
                        found = Some((c.clone(), sub, outcome));
                        break;
                    }
                    tried.push(format!(
                        "{} first, which ended at {:04X}h without an answer",
                        c.name(),
                        sub.last().map_or(c.follow.pc, |l| l.address)
                    ));
                    first.get_or_insert((c.clone(), sub, outcome));
                }
                let tried_text =
                    if tried.is_empty() { String::new() } else { format!(" (tried {})", tried.join("; then ")) };
                match (found, first) {
                    (Some((c, sub, outcome)), _) => {
                        level.followed = Some(format!("{}{tried_text}", c.follow_text()));
                        level.others = others(Some(&c));
                        let mut levels = vec![level];
                        levels.extend(sub);
                        (levels, outcome)
                    }
                    (None, Some((c, sub, outcome))) => {
                        level.followed = Some(format!(
                            "{}{tried_text}; every candidate tried ended without an answer, this is the first",
                            c.follow_text()
                        ));
                        level.others = others(Some(&c));
                        let mut levels = vec![level];
                        levels.extend(sub);
                        (levels, outcome)
                    }
                    (None, None) => {
                        level.others = others(None);
                        level.stopped_because = Some(format!(
                            "no candidate could be followed{tried_text}; the chain stopped here without an answer."
                        ));
                        (vec![level], Outcome::NoAnswer)
                    }
                }
            }
        };
        visited.pop();
        result
    }
}

/// The instruction moves data and nothing else on its path counts: a
/// plain load, store or move that is the only instruction of the slice.
fn is_copy(target: &Step, member: &[bool]) -> bool {
    matches!(target.op, "MOV" | "ST" | "L")
        && target.reads.iter().any(|r| r.mask != 0)
        && member.iter().filter(|&&m| m).count() == 1
}

/// The walk proper, from the ring that ends at the first level's
/// instruction. `rerun` gives the ring ending at an instruction index.
fn walk(
    rom: &[u8],
    setup: &Setup,
    ring: Vec<Step>,
    from: String,
    did: Option<String>,
    rerun: impl FnMut(&[u8], &Setup, u64) -> Vec<Step>,
) -> Why {
    if ring.is_empty() {
        let note = Some("the re-run kept no instruction to start from.".to_string());
        return Why { from, levels: Vec::new(), note };
    }
    let mut walker = Walker { rom, setup, rerun, reruns: RERUNS };
    let (levels, _) = walker.descend(ring, did, (SFR_P1, Some(LAMP_BIT)), 1, &mut Vec::new());
    Why { from, levels, note: None }
}

/// The instructions of the ring that belong to the path into its last
/// one, oldest first, the last one included: interrupt handlers that ran in
/// between are left out, and an interrupt entry ends the path when the last
/// instruction is in the handler. Calls and returns are followed through:
/// a callee computes what its caller uses.
pub fn context(ring: &[Step]) -> Vec<&Step> {
    let Some(last) = ring.last() else { return Vec::new() };
    let mut out = vec![last];
    let mut i = ring.len() - 1;
    while i > 0 {
        i -= 1;
        let s = &ring[i];
        if s.interrupted {
            // The instruction after this one was a handler's first: we are
            // in that handler.
            break;
        }
        if s.op == "RTI" {
            // Skip back over the handler to the instruction it interrupted.
            let mut depth = 1;
            let mut found = false;
            while i > 0 {
                i -= 1;
                if ring[i].op == "RTI" {
                    depth += 1;
                } else if ring[i].interrupted {
                    depth -= 1;
                    if depth == 0 {
                        found = true;
                        break;
                    }
                }
            }
            if !found {
                break;
            }
            out.push(&ring[i]);
            continue;
        }
        out.push(s);
    }
    out.reverse();
    out
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// On-chip data the ROM wrote; `follow` is the write to follow.
    OnChip,
    /// Decided outside the chip or by a peripheral.
    Input,
    /// A ROM byte read by LC / CMPC.
    RomConstant,
}

/// Something the path read.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub kind: Kind,
    pub addr: u16,
    /// The bit, when one bit decides (a bit test, or the bit followed).
    pub bit: Option<u8>,
    pub value: u8,
    /// Where it was read.
    pub at_pc: u16,
    /// Position in the shown path (later is nearer the instruction).
    pub pos: usize,
    /// The last write of the bit followed, and its last change.
    pub written: Stamp,
    pub changed: Stamp,
    pub follow: Stamp,
    /// For a whole byte, the bit whose source is followed: the one with
    /// the latest source, so that source is the byte's last change. Named
    /// only when that change was a bit instruction (`bit_named`).
    pub last_bit: Option<u8>,
}

impl Candidate {
    fn name(&self) -> String {
        let place = if self.kind == Kind::RomConstant {
            format!("ROM {:04X}h", self.addr)
        } else if self.addr >= EXTERNAL_DATA_START {
            format!("{:04X}h", self.addr)
        } else {
            format!("{:03X}h", self.addr)
        };
        match self.bit {
            Some(b) => format!("{place} bit {b} (= {})", (self.value >> b) & 1),
            None => format!("{place} (= {:02X}h)", self.value),
        }
    }

    /// The bit worth naming: the one the instruction tested, or, for a byte
    /// read whole, the bit whose change is the source followed when a bit
    /// instruction made it (that change touched this bit alone) and nothing
    /// counted on the byte since. A byte written whole has no bit of its
    /// own to name: every bit came from that write, and which one the
    /// search happened to land on (`last_bit`) means nothing.
    fn bit_named(&self) -> Option<u8> {
        self.bit.or(self.last_bit.filter(|_| is_bit_write(self.follow.op) && !self.counting()))
    }

    /// The name and where it was read; for a byte read whole whose last
    /// change was one bit, which bit and its value at the read.
    fn read_text(&self) -> String {
        match (self.bit, self.bit_named()) {
            (None, Some(b)) => format!(
                "{}, read whole at {:04X}h; the last of its bits to change was bit {b}, {} at that read",
                self.name(),
                self.at_pc,
                (self.value >> b) & 1
            ),
            _ => format!("{}, read at {:04X}h", self.name(), self.at_pc),
        }
    }

    /// The byte was changed after its last load by the byte counting on
    /// itself (not by an exchange that put the value back).
    fn counting(&self) -> bool {
        self.written.counted && self.written.op != "XCHG" && self.written.index != self.follow.index
    }

    fn source(&self) -> String {
        let verb = if self.written.index == self.changed.index { "set" } else { "written" };
        let mut s = format!("{verb} by {:04X}h at {:.3} s", self.follow.pc, self.follow.seconds());
        if self.counting() {
            s.push_str(&format!(
                ", then counted on by itself (last at {:04X}h, {:.3} s)",
                self.written.pc,
                self.written.seconds()
            ));
        }
        if self.changed.is_set() && self.changed.index < self.follow.index {
            s.push_str(&format!(
                "; it has held this value since {:04X}h at {:.3} s",
                self.changed.pc,
                self.changed.seconds()
            ));
        }
        s
    }

    pub fn describe(&self) -> String {
        match self.kind {
            Kind::OnChip if !self.follow.is_set() => {
                format!("{}, read at {:04X}h, not written since power-on", self.name(), self.at_pc)
            }
            Kind::OnChip => format!("{}, {}", self.read_text(), self.source()),
            Kind::Input => {
                let board = board_name(self.addr, self.bit).map_or(String::new(), |n| format!(": {n}"));
                format!("{}, read at {:04X}h, from outside the CPU{board}", self.name(), self.at_pc)
            }
            Kind::RomConstant => format!("{}, read at {:04X}h, a constant in the ROM", self.name(), self.at_pc),
        }
    }

    fn follow_text(&self) -> String {
        format!("{}, {}", self.read_text(), self.source())
    }

    /// What the next level's instruction did, said from this level.
    fn changed_text(&self) -> String {
        let what = match self.bit_named() {
            Some(b) => format!("{:03X}h bit {b}", self.addr),
            None => format!("{:03X}h", self.addr),
        };
        if self.counting() {
            format!("last loaded {what}; it counted on by itself from there to the value read")
        } else {
            format!("last set {what} before it was read")
        }
    }
}

pub enum Decision {
    /// What to follow, best first.
    Follow(Vec<Candidate>),
    /// The chain ends here, at `on` when it ends at an input or a ROM
    /// constant (then it is answered).
    Stop { why: String, on: Option<Candidate> },
}

pub struct Choice {
    pub decision: Decision,
    /// Every candidate, nearest the instruction first.
    pub all: Vec<Candidate>,
}

/// Every route the code can take from `from`, each followed until it
/// reaches an address in `later` (the rest of the path). Calls are assumed
/// to return; a return goes to `returns_to`, where the path itself
/// returned, when the branch is inside a routine the path returns from; a route into `BRK` (a reset: a sanity check failing) is
/// dropped. Returns the positions in `later` the routes reach, or None when
/// some route leaves the code that can be followed (a return, an indirect
/// jump), sets one of the `needed` registers or flags, or the exploration
/// grows past its budget: then the branch may well have kept the path from
/// happening, or changed what it computed.
pub fn rejoins(
    rom: &[u8],
    from: u16,
    dd: bool,
    later: &[u16],
    needed: u8,
    returns_to: Option<u16>,
) -> Option<Vec<usize>> {
    const BUDGET: usize = 256;
    let mut seen: Vec<u16> = Vec::new();
    let mut queue: VecDeque<(u16, bool)> = VecDeque::from([(from, dd)]);
    let mut reached: Vec<usize> = Vec::new();
    while let Some((pc, dd)) = queue.pop_front() {
        if let Some(j) = later.iter().position(|&p| p == pc) {
            if !reached.contains(&j) {
                reached.push(j);
            }
            continue;
        }
        if seen.contains(&pc) {
            continue;
        }
        seen.push(pc);
        if seen.len() > BUDGET {
            return None;
        }
        let d = decoder::decode(dd, |i| rom[(pc as usize + i) & (rom.len() - 1)])?;
        let parsed = table()[d.index].as_ref()?;
        let next = pc.wrapping_add(d.len as u16);
        let dd = d.dd_after.unwrap_or(dd);
        let op = if parsed.byte_width { &parsed.op[..parsed.op.len() - 1] } else { parsed.op };
        let carry_dest = parsed.args.first() == Some(&Arg::Carry);
        if registers(op, &parsed.args, carry_dest).1 & needed != 0 {
            // This way would have set a register or flag the path needs.
            return None;
        }
        match parsed.op {
            "SJ" => queue.push_back((next.wrapping_add(d.fields.rel8 as u16), dd)),
            "J" if parsed.args.first() == Some(&Arg::Addr16) => queue.push_back((d.fields.addr16, dd)),
            op if CONDITIONAL.contains(&op) => {
                queue.push_back((next, dd));
                queue.push_back((next.wrapping_add(d.fields.rel8 as u16), dd));
            }
            // A route into a reset is a sanity check failing, not another
            // way the path could have gone: it is dropped.
            "BRK" => {}
            // Inside a callee, a return goes where the path returned.
            "RT" if returns_to.is_some() => queue.push_back((returns_to.unwrap_or(0), dd)),
            "J" | "RT" | "RTI" => return None,
            _ => queue.push_back((next, dd)),
        }
    }
    Some(reached)
}

/// Which instructions of the shown path the last one depends on, walking
/// back: an instruction that writes a register, flag or byte still needed
/// is in, and what it read becomes needed; a conditional branch is in when
/// some route from its other way does not come back to the path (it
/// decided whether the path got here), or comes back past an instruction
/// that is in (it decided whether that one ran).
pub fn slice(rom: &[u8], shown: &[&Step]) -> Vec<bool> {
    let n = shown.len();
    let mut member = vec![false; n];
    let Some(target) = shown.last() else { return member };
    member[n - 1] = true;
    let mut regs = target.uses;
    let mut mem: Vec<u16> = target.reads.iter().filter(|r| r.mask != 0).map(|r| r.addr).collect();
    for i in (0..n - 1).rev() {
        let s = shown[i];
        if let Some(alt) = s.alt {
            let later: Vec<u16> = shown[i + 1..].iter().map(|t| t.pc).collect();
            // Where the path returns from the routine the branch is in.
            let mut calls = 0usize;
            let mut returned_at = None;
            for (k, t) in shown[i + 1..].iter().enumerate() {
                if t.is_call() {
                    calls += 1;
                } else if t.op == "RT" {
                    if calls == 0 {
                        returned_at = Some(i + 1 + k);
                        break;
                    }
                    calls -= 1;
                }
            }
            let returns_to = returned_at.and_then(|r| shown.get(r + 1)).map(|t| t.pc);
            let relevant = match (rejoins(rom, alt, s.dd, &later, regs, returns_to), returned_at) {
                (Some(reached), _) => reached.iter().any(|&j| member[i + 1..i + 1 + j].iter().any(|&m| m)),
                // Inside a routine the path returns from, a branch whose
                // other way cannot be followed matters only when something
                // the instruction depends on happens before that return.
                (None, Some(r)) => member[i + 1..=r].iter().any(|&m| m),
                (None, None) => true,
            };
            if relevant {
                member[i] = true;
                regs |= s.uses;
                mem.extend(s.reads.iter().filter(|r| r.mask != 0).map(|r| r.addr));
            }
            continue;
        }
        let mem_hit = s.writes.iter().any(|w| mem.contains(&w.addr));
        if s.defs & regs != 0 || mem_hit {
            member[i] = true;
            regs = (regs & !s.defs) | s.uses;
            mem.retain(|a| !s.writes.iter().any(|w| w.addr == *a));
            mem.extend(s.reads.iter().filter(|r| r.mask != 0).map(|r| r.addr));
        }
    }
    member
}

/// Everything the instructions of the shown path marked in `member` read.
/// An input from outside the CPU: an external address, an A/D result, the
/// input pins of a port, a transition detector or an interrupt request.
/// Timers, timer control, the serial unit and the PWM counters are on-chip
/// state no instruction wrote: listed, not followed, not an answer.
fn is_input(addr: u16) -> bool {
    addr >= EXTERNAL_DATA_START
        || matches!(addr, 0x18..=0x19 | 0x20..=0x2F | 0x46..=0x47 | 0x60..=0x6F)
}

pub fn candidates(shown: &[&Step], member: &[bool]) -> Vec<Candidate> {
    let indices: Vec<u64> = shown.iter().map(|s| s.index).collect();
    let mut out: Vec<Candidate> = Vec::new();
    for (pos, s) in shown.iter().enumerate() {
        if !member[pos] {
            continue;
        }
        for r in &s.reads {
            // Bits an earlier instruction of the path wrote are its doing;
            // its own reads are candidates in their place.
            let local: u8 = shown[..pos]
                .iter()
                .flat_map(|t| t.writes.iter().filter(|w| w.addr == r.addr).map(move |_| t))
                .fold(0, |m, t| m | if is_bit_write(t.op) { t.bit.map_or(0, |b| 1 << b) } else { 0xFF });
            let r = &Read { mask: r.mask & !local, ..r.clone() };
            let inputs = if is_input(r.addr) { r.mask & r.outside } else { 0 };
            if inputs != 0 {
                let bit = (inputs.count_ones() == 1).then(|| inputs.trailing_zeros() as u8);
                out.push(Candidate {
                    kind: Kind::Input,
                    addr: r.addr,
                    bit,
                    value: r.value,
                    at_pc: s.pc,
                    pos,
                    written: Stamp::default(),
                    changed: Stamp::default(),
                    follow: Stamp::default(),
                    last_bit: None,
                });
            }
            let own = r.mask & !r.outside;
            if own == 0 || r.addr >= EXTERNAL_DATA_START {
                continue;
            }
            // The bit whose source is the latest write from outside the
            // shown path; a write inside it is followed through that
            // instruction's own reads, which are shown too.
            let mut best: Option<(u8, Stamp, Stamp, Stamp)> = None;
            let mut never: Option<u8> = None;
            for b in 0..8u8 {
                if own & (1 << b) == 0 {
                    continue;
                }
                let Source { written, origin, changed, origin_change } = r.stamps[b as usize];
                // A register of the local bank is scratch: its value came
                // from its last write. Anything else is state: what matters
                // is when it took the value it has, its last change.
                let register = (s.bank..s.bank + 8).contains(&r.addr);
                let (written, origin) = if register { (written, origin) } else { (changed, origin_change) };
                if !written.is_set() {
                    never.get_or_insert(b);
                    continue;
                }
                if indices.contains(&written.index) {
                    continue;
                }
                let follow = if !written.counted {
                    written
                } else if origin.is_set() {
                    origin
                } else {
                    continue;
                };
                if best.is_none_or(|(_, _, _, f)| follow.index > f.index) {
                    best = Some((b, written, changed, follow));
                }
            }
            let single = own.count_ones() == 1;
            if let Some((b, written, changed, follow)) = best {
                out.push(Candidate {
                    kind: Kind::OnChip,
                    addr: r.addr,
                    bit: single.then_some(b),
                    value: r.value,
                    at_pc: s.pc,
                    pos,
                    written,
                    changed,
                    follow,
                    last_bit: (!single).then_some(b),
                });
            } else if let Some(b) = never.filter(|_| single) {
                out.push(Candidate {
                    kind: Kind::OnChip,
                    addr: r.addr,
                    bit: Some(b),
                    value: r.value,
                    at_pc: s.pc,
                    pos,
                    written: Stamp::default(),
                    changed: Stamp::default(),
                    follow: Stamp::default(),
                    last_bit: None,
                });
            }
        }
        for &(addr, value) in &s.rom_reads {
            out.push(Candidate {
                kind: Kind::RomConstant,
                addr,
                bit: None,
                value,
                at_pc: s.pc,
                pos,
                written: Stamp::default(),
                changed: Stamp::default(),
                follow: Stamp::default(),
                last_bit: None,
            });
        }
    }
    // One entry per place: the read nearest the instruction.
    let mut seen: Vec<(Kind, u16, Option<u8>)> = Vec::new();
    out.reverse();
    out.retain(|c| {
        let key = (c.kind.clone(), c.addr, c.bit);
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
    out
}

/// The heuristic. In order:
///
/// 1. An input from outside the CPU on the shown path ends the chain: the
///    one read nearest the instruction is named.
/// 2. Otherwise follow on-chip data. Its source write is the change itself,
///    or for a byte that counts on itself (a counter, a shift register) the
///    last load. Data written by another routine comes first, data written
///    by code next to the path (`OWN_CODE`) after it, a write of a bare
///    constant last; within each, the most recent source first. Writes by the shown path
///    are not candidates: their own reads are. Instructions already shown
///    as a level are not followed again.
/// 3. Otherwise a ROM constant on the path ends the chain.
/// 4. Otherwise the chain stops without an answer.
///
/// The walk tries the candidates of step 2 in order: one whose chain ends
/// without an answer (a dead end) is given up for the next.
pub fn choose(shown: &[&Step], member: &[bool], visited: &[u16]) -> Choice {
    let all = candidates(shown, member);
    if let Some(input) = all.iter().find(|c| c.kind == Kind::Input) {
        let why = format!("the path reads an input: {}.", input.describe());
        let on = Some(input.clone());
        return Choice { decision: Decision::Stop { why, on }, all };
    }
    let mut on_chip: Vec<Candidate> = all
        .iter()
        .filter(|c| c.kind == Kind::OnChip && c.follow.is_set() && !visited.contains(&c.follow.pc))
        .cloned()
        .collect();
    // Data another routine produced comes first; the routine's own
    // bookkeeping (written by code next to an instruction of the path)
    // after it, and writes of a bare constant (a loop index set up, a
    // clear) last: following one leads nowhere. Within each, the most
    // recent source first (cause precedes effect), then the read nearest
    // the instruction.
    let own = |c: &Candidate| shown.iter().any(|s| s.pc.abs_diff(c.follow.pc) <= OWN_CODE);
    on_chip.sort_by(|a, b| {
        a.follow
            .constant
            .cmp(&b.follow.constant)
            .then(own(a).cmp(&own(b)))
            .then(b.pos.cmp(&a.pos))
            .then(b.follow.index.cmp(&a.follow.index))
    });
    if !on_chip.is_empty() {
        return Choice { decision: Decision::Follow(on_chip), all };
    }
    if let Some(constant) = all.iter().find(|c| c.kind == Kind::RomConstant) {
        let why = format!(
            "nothing on-chip left to follow; the path is decided by a ROM constant: {}.",
            constant.describe()
        );
        let on = Some(constant.clone());
        return Choice { decision: Decision::Stop { why, on }, all };
    }
    let why = if all.iter().any(|c| c.kind == Kind::OnChip && c.follow.is_set()) {
        "every write this path depends on is already a level above; the chain stopped here without an answer."
    } else {
        "nothing this path read was written by an earlier instruction; the chain stopped here without an answer."
    };
    Choice { decision: Decision::Stop { why: why.to_string(), on: None }, all }
}

// ---- rendering -------------------------------------------------------------

pub fn disassemble(rom: &[u8], pc: u16, dd: bool) -> String {
    let fetch = |i: usize| rom[(pc as usize + i) & (rom.len() - 1)];
    match decoder::decode(dd, fetch) {
        Some(d) => decoder::format(&d, pc.wrapping_add(d.len as u16)),
        None => format!("DB 0{:02X}h", fetch(0)),
    }
}

fn read_text(r: &Read, bit: Option<u8>) -> Option<String> {
    if r.mask == 0 {
        return None;
    }
    let out = if r.outside & r.mask != 0 { " (outside)" } else { "" };
    let width = if r.addr >= EXTERNAL_DATA_START { 4 } else { 3 };
    Some(match bit.filter(|_| r.mask.count_ones() == 1) {
        Some(b) => format!("{:0width$X}h.{b}={}{out}", r.addr, (r.value >> b) & 1),
        None => format!("{:0width$X}h={:02X}{out}", r.addr, r.value),
    })
}

fn path_line(rom: &[u8], s: &Step, is_target: bool, member: bool) -> String {
    let marker = if is_target {
        ">"
    } else if member {
        "*"
    } else {
        " "
    };
    let branch = match s.branch {
        Some(true) => "taken",
        Some(false) => "not taken",
        None => "",
    };
    let mut reads: Vec<String> = s.reads.iter().filter_map(|r| read_text(r, s.bit)).collect();
    reads.extend(s.rom_reads.iter().map(|(a, v)| format!("ROM {a:04X}h={v:02X}")));
    let writes: Vec<String> = s
        .writes
        .iter()
        .map(|w| {
            if w.addr >= EXTERNAL_DATA_START {
                format!("{:04X}h<-{:02X}", w.addr, w.new)
            } else {
                format!("{:03X}h:{:02X}->{:02X}", w.addr, w.old, w.new)
            }
        })
        .collect();
    let mut line = format!("{marker} {:04X}h  {:<28} {:<9}", s.pc, disassemble(rom, s.pc, s.dd), branch);
    if !reads.is_empty() {
        line.push_str(&format!(" read {}", reads.join(" ")));
    }
    if !writes.is_empty() && (is_target || s.writes.len() <= 2) {
        line.push_str(&format!(" wrote {}", writes.join(" ")));
    }
    line.trim_end().to_string()
}

fn writes_text(s: &Step) -> String {
    let w: Vec<String> = s
        .writes
        .iter()
        .map(|w| {
            if w.addr >= EXTERNAL_DATA_START {
                format!("{:04X}h <- {:02X}h", w.addr, w.new)
            } else {
                format!("{:03X}h {:02X}h -> {:02X}h", w.addr, w.old, w.new)
            }
        })
        .collect();
    if w.is_empty() { "wrote no data".to_string() } else { format!("wrote {}", w.join(", ")) }
}

/// What the board model says is at an address, for an input.
pub fn board_name(addr: u16, bit: Option<u8>) -> Option<String> {
    const PORT_A: [&str; 8] = [
        "knock-board reply line on connector FC1 (pull-up only with no knock board)",
        "knock-board reply line on connector FC1 (pull-up only with no knock board)",
        "knock-board heartbeat on connector FC1 (pull-up only with no knock board)",
        "power-steering pressure switch",
        "lock-up solenoid A driver feedback",
        "VTEC solenoid driver feedback",
        "O2-heater driver feedback",
        "service check connector",
    ];
    const SWITCHES: [&str; 8] = [
        "starter signal",
        "VTEC oil-pressure switch",
        "A/C switch",
        "lock-up solenoid B driver feedback",
        "brake switch",
        "A/T park/neutral",
        "A/T gear drive 3",
        "A/T gear drive 4",
    ];
    const ANALOG: [&str; 8] = [
        "AI0, the U6 mux (O2, IAT, barometric pressure, battery voltage, board resistors; selected by P2.5..P2.7)",
        "AI1, the U5 mux (ELD, ECT, gearbox and other board resistors; selected by P2.5..P2.7)",
        "AI2, alternator FR",
        "AI3, EGR lift sensor (the battery divider when none is fitted)",
        "AI4, IACV coil current",
        "AI5, not identified",
        "AI6, MAP sensor",
        "AI7, throttle position sensor",
    ];
    let bit_name = |names: &[&str; 8]| bit.map(|b| format!(" bit {b}, {}", names[b as usize])).unwrap_or_default();
    if addr >= EXTERNAL_DATA_START {
        if addr == crate::bus::P28_INPUT_PORT {
            return Some(format!("the switch-input buffer at 4700h{}", bit_name(&SWITCHES)));
        }
        return Some(match p28_ppi_register(addr) {
            Some(0) => format!("82C55 port A{}", bit_name(&PORT_A)),
            Some(1) => "82C55 port B (outputs, read back)".to_string(),
            Some(2) => "82C55 port C (outputs, read back)".to_string(),
            Some(_) => "82C55 control register".to_string(),
            None => "an external address no device in the model answers (reads FFh)".to_string(),
        });
    }
    Some(match addr {
        0x18 | 0x19 => "the interrupt request flags (IRQ)".to_string(),
        0x30..=0x3F => "a timer register".to_string(),
        0x40..=0x43 => "a timer control register (TCON)".to_string(),
        0x46 => format!(
            "the transition detectors (TRNS){}",
            bit.map(|b| match b {
                1 => " bit 1, the ignition monitor (P4.5)",
                2 => " bit 2, the injector monitor (P4.6)",
                3 => " bit 3, the heartbeat (P4.7)",
                _ => "",
            })
            .unwrap_or("")
        ),
        0x60..=0x6F => format!("A/D result {}", ANALOG[((addr - 0x60) / 2) as usize]),
        0x20..=0x2F => "a port's input pins".to_string(),
        _ => return None,
    })
}

fn is_bit_write(op: &str) -> bool {
    matches!(op, "SB" | "RB" | "SBR" | "RBR" | "MB" | "MBR")
}

/// The last `PATH_LINES` instructions of the path and the instruction, and
/// before them up to `EARLIER_LINES` earlier ones it depends on, with `...`
/// where lines are left out.
fn shown_lines(rom: &[u8], context: &[&Step], member: &[bool]) -> Vec<String> {
    const EARLIER_LINES: usize = 6;
    let n = context.len();
    let tail = n.saturating_sub(PATH_LINES + 1);
    let mut keep: Vec<usize> = (0..tail).filter(|&i| member[i]).collect();
    keep = keep.split_off(keep.len().saturating_sub(EARLIER_LINES));
    keep.extend(tail..n);
    let mut out = Vec::new();
    let mut last: Option<usize> = None;
    if keep.first().is_some_and(|&i| i > 0) {
        out.push("  ...".to_string());
    }
    for &i in &keep {
        if last.is_some_and(|l| i > l + 1) {
            out.push("  ...".to_string());
        }
        let s = context[i];
        out.push(path_line(rom, s, i == n - 1, member[i]));
        last = Some(i);
    }
    out
}

/// Lines of "also read" shown per level in the text report.
const OTHERS_SHOWN: usize = 8;

/// The section of the text report.
pub fn text_section(why: &Why) -> String {
    let mut out = String::from(
        "WHY THE LIGHT IS ON (automatic trace: at each level the program picks what to follow by a rule of thumb; check it against the evidence shown)\n",
    );
    if let Some(note) = &why.note {
        out.push_str(&format!("  No trace: {note}\n"));
        return out;
    }
    out.push_str(&format!("  Starts from {}.\n", why.from));
    out.push_str("  Each level: the instruction, the path into it (> marks it, * the instructions it depends on), what was followed next.\n");
    out.push_str("  What is followed: among the data the starred instructions read, data written by another routine before the\n");
    out.push_str("  routine's own bookkeeping, the read nearest the instruction first; a choice that ends without reaching an\n");
    out.push_str("  input or a ROM constant is given up for the next. The trace stops at an input from outside the CPU.\n");
    out.push_str("  Addresses in reads and writes are resolved (off 018h is 218h when LRB points at page 2).\n");
    for (i, l) in why.levels.iter().enumerate() {
        out.push_str(&format!(
            "\n  {}. {:04X}h  {}   at {:.3} s\n     {}\n",
            i + 1,
            l.address,
            l.instruction,
            l.at,
            l.did
        ));
        out.push_str("     path:\n");
        for p in &l.path {
            out.push_str(&format!("      {p}\n"));
        }
        if let Some(f) = &l.followed {
            out.push_str(&format!("     followed: {f}\n"));
        }
        if !l.others.is_empty() {
            out.push_str("     also read:\n");
            for o in l.others.iter().take(OTHERS_SHOWN) {
                out.push_str(&format!("       - {o}\n"));
            }
            if l.others.len() > OTHERS_SHOWN {
                out.push_str(&format!(
                    "       ... and {} more (all are in the JSON report)\n",
                    l.others.len() - OTHERS_SHOWN
                ));
            }
        }
        if let Some(s) = &l.stopped_because {
            out.push_str(&format!("     stopped: {s}\n"));
        }
    }
    out
}

fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn strings(items: &[String]) -> String {
    format!("[{}]", items.iter().map(|s| quote(s)).collect::<Vec<_>>().join(", "))
}

/// The `why_the_light_is_on` value of the JSON report: an array of levels.
pub fn json_levels(why: &Why) -> String {
    let opt = |s: &Option<String>| s.as_deref().map_or_else(|| "null".to_string(), quote);
    let levels: Vec<String> = why
        .levels
        .iter()
        .map(|l| {
            format!(
                "    {{\"at\": {:.6}, \"address\": {}, \"instruction\": {}, \"did\": {}, \"path\": {}, \"followed\": {}, \"others\": {}, \"stopped_because\": {}, \"copy\": {}}}",
                l.at,
                quote(&format!("{:04X}h", l.address)),
                quote(&l.instruction),
                quote(&l.did),
                strings(&l.path),
                opt(&l.followed),
                strings(&l.others),
                opt(&l.stopped_because),
                l.copy
            )
        })
        .collect();
    if levels.is_empty() { "[]".to_string() } else { format!("[\n{}\n  ]", levels.join(",\n")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(pc: u16, index: u64) -> Stamp {
        Stamp { pc, index, cycles: index * 4, counted: false, op: "MOV", constant: false }
    }

    #[test]
    fn a_write_that_changes_two_bits_records_the_instruction_for_both_and_no_other() {
        let mut prov = Provenance::new();
        let first = stamp(0x1000, 5);
        prov.record(first, &[(DataWrite { addr: 0x150, old: 0x00, new: 0xFF }, 0xFF, false)]);
        let second = stamp(0x2000, 9);
        // 0xFF -> 0xF6: bits 0 and 3 change.
        prov.record(second, &[(DataWrite { addr: 0x150, old: 0xFF, new: 0xF6 }, 0xFF, false)]);
        for bit in 0..8 {
            let expected = if bit == 0 || bit == 3 { second } else { first };
            assert_eq!(prov.changed[0x150 * 8 + bit], expected, "bit {bit}");
            // Every bit was written by the second store.
            assert_eq!(prov.written[0x150 * 8 + bit], second, "bit {bit}");
            assert_eq!(prov.origin[0x150 * 8 + bit], second, "bit {bit}");
        }
        // A neighbouring byte is untouched; the writers are listed newest first.
        assert!(!prov.changed[0x151 * 8].is_set());
        assert_eq!(prov.writers[0x150][0], (0x2000, 9));
        assert_eq!(prov.writers[0x150][1], (0x1000, 5));
        // A bit instruction writes its own bit only.
        let third = stamp(0x3000, 12);
        prov.record(third, &[(DataWrite { addr: 0x150, old: 0xF6, new: 0xF6 }, 0x10, false)]);
        assert_eq!(prov.written[0x150 * 8 + 4], third);
        assert_eq!(prov.written[0x150 * 8 + 5], second);
        assert_eq!(prov.changed[0x150 * 8 + 4], first, "rewriting the same value is not a change");
    }

    #[test]
    fn counting_keeps_the_origin_and_an_exchange_never_makes_one() {
        let mut prov = Provenance::new();
        let load = stamp(0x1000, 5);
        prov.record(load, &[(DataWrite { addr: 0x2E8, old: 0x00, new: 0x0A }, 0xFF, false)]);
        let dec = Stamp { op: "DEC", ..stamp(0x5210, 7) };
        prov.record(dec, &[(DataWrite { addr: 0x2E8, old: 0x0A, new: 0x09 }, 0xFF, true)]);
        // 0Ah -> 09h: bit 1, set by the load, changed by counting. Its last
        // change is the DEC; its origin, of writes and of changes, the load.
        let bit1 = 0x2E8 * 8 + 1;
        assert_eq!(prov.changed[bit1], Stamp { counted: true, ..dec });
        assert_eq!(prov.written[bit1], Stamp { counted: true, ..dec });
        assert_eq!(prov.origin[bit1], load);
        assert_eq!(prov.origin_change[bit1], load);
        let xchg = Stamp { op: "XCHG", counted: true, ..stamp(0x529E, 8) };
        prov.record(xchg, &[(DataWrite { addr: 0x300, old: 0x00, new: 0x55 }, 0xFF, true)]);
        assert!(!prov.origin[0x300 * 8].is_set(), "a RAM refresh exchange is not a source");
        assert!(!prov.origin_change[0x300 * 8].is_set());
    }

    /// Reset vector to 0100h, `code` there, BRK elsewhere.
    fn rom_with(code: &[u8]) -> Vec<u8> {
        let mut rom = vec![0xFF; 32768];
        rom[0] = 0x00;
        rom[1] = 0x01;
        rom[0x100..0x100 + code.len()].copy_from_slice(code);
        rom
    }

    /// Run `rom` on a bench until instruction `index` with a tracer whose
    /// ring holds `capacity` instructions, all kept.
    fn traced(rom: &[u8], capacity: usize, index: u64, input: u8) -> Tracer {
        let mut bench = Bench::new(rom);
        bench.bus.p28_input_port = input;
        let mut tracer = Tracer::new();
        tracer.capacity = capacity;
        tracer.ring_from_index = 0;
        tracer.stop_at_index = Some(index);
        bench.tracer = Some(Box::new(tracer));
        bench.run(0.01);
        *bench.tracer.take().unwrap()
    }

    #[test]
    fn the_ring_keeps_the_last_instructions_in_order_with_their_branches() {
        let rom = rom_with(&[
            0x95, // 0100 RC
            0xCA, 0x01, // 0101 JLT 0104: not taken
            0x85, // 0103 SC
            0xCA, 0x01, // 0104 JLT 0107: taken
            0xFF, // 0106 never runs
            0xCB, 0xF7, // 0107 SJ 0100
        ]);
        let t = traced(&rom, 4, 10, 0);
        assert!(t.paused);
        let ring: Vec<(u64, u16, Option<bool>)> = t.ring.iter().map(|s| (s.index, s.pc, s.branch)).collect();
        // Five per loop: RC, JLT, SC, JLT, SJ; instruction 10 is the second SJ.
        assert_eq!(
            ring,
            vec![(7, 0x0101, Some(false)), (8, 0x0103, None), (9, 0x0104, Some(true)), (10, 0x0107, None)]
        );
        assert_eq!(t.ring[0].alt, Some(0x0104));
        assert_eq!(t.ring[2].alt, Some(0x0106));
        assert_eq!(t.ring[1].defs, REG_C);
    }

    #[test]
    fn a_path_records_the_data_it_read_with_where_it_came_from() {
        let rom = rom_with(&[
            0x77, 0x5A, // 0100 LB A, #5Ah
            0xD5, 0xA0, // 0102 STB A, 0A0h
            0xF5, 0xA0, // 0104 LB A, 0A0h
            0xCB, 0xF8, // 0106 SJ 0100
        ]);
        let t = traced(&rom, 8, 3, 0);
        let read = &t.ring.back().unwrap().reads[0];
        assert_eq!((read.addr, read.value, read.outside, read.mask), (0xA0, 0x5A, 0, 0xFF));
        // Bit 1 of 5Ah was set by the store at 0102h, instruction 2.
        assert_eq!((read.stamps[1].changed.pc, read.stamps[1].changed.index), (0x0102, 2));
        assert_eq!(read.stamps[1].written, read.stamps[1].changed);
        assert!(!read.stamps[0].changed.is_set(), "bit 0 never changed");
        // ...but it was written, with all the others, by the same store.
        assert_eq!(read.stamps[0].written.pc, 0x0102);
    }

    /// An input bit is read from the switch buffer at 4700h, copied to the
    /// RAM flag A0h.0, and a second routine tests the flag and sets P1.4.
    fn lamp_rom() -> Vec<u8> {
        rom_with(&[
            0x62, 0x00, 0x47, // 0100 MOV DP, #4700h
            0xC2, 0x2A, // 0103 MB C, [DP].2
            0xC5, 0xA0, 0x38, // 0105 MB 0A0h.0, C
            0x95, // 0108 RC
            0xD8, 0xA0, 0x01, // 0109 JBR off 0A0h.0, 010Dh
            0x85, // 010C SC
            0xC5, 0x22, 0x3C, // 010D MB P1.4, C
            0xCB, 0xEE, // 0110 SJ 0100
        ])
    }

    /// The walk with a ring of `capacity` instructions.
    fn walk_lamp_rom(capacity: usize) -> Why {
        let rom = lamp_rom();
        // The switch buffer's bit 2 reads 1.
        let input = 0x04;
        let first = traced(&rom, capacity, 40, input);
        let start = first.p1_lamp.expect("the ROM sets P1.4");
        let setup = Setup::preset("stock-p28-manual").unwrap();
        let ring = start.ring.expect("the ring was kept");
        walk(&rom, &setup, ring, "test".to_string(), None, |rom, _, index| {
            traced(rom, capacity, index, input).ring.into_iter().collect()
        })
    }

    #[test]
    fn a_whole_walk_names_the_three_instructions_and_stops_at_the_external_read() {
        // A ring of four instructions holds one routine: the walk has to
        // re-run to the instant the flag was set.
        let why = walk_lamp_rom(4);
        let addresses: Vec<u16> = why.levels.iter().map(|l| l.address).collect();
        assert_eq!(addresses, vec![0x010D, 0x0105], "{why:#?}");
        assert!(why.levels[0].followed.as_deref().unwrap().starts_with("0A0h bit 0 (= 1), read at 0109h"), "{why:#?}");
        let stop = why.levels[1].stopped_because.as_deref().unwrap();
        assert!(stop.contains("4700h bit 2 (= 1), read at 0103h"), "{stop}");
        assert!(stop.contains("the switch-input buffer at 4700h bit 2, A/C switch"), "{stop}");
        let text = text_section(&why);
        assert!(text.starts_with("WHY THE LIGHT IS ON (automatic trace"), "{text}");
        assert!(text.contains("> 010Dh  MB 022h.4, C"), "{text}");
        let json = json_levels(&why);
        assert!(json.contains("\"address\": \"0105h\"") && json.contains("\"stopped_because\": \"the path reads an input"), "{json}");
    }

    #[test]
    fn with_both_routines_in_the_ring_one_level_shows_the_whole_chain() {
        let why = walk_lamp_rom(RING);
        assert_eq!(why.levels.len(), 1, "{why:#?}");
        let level = &why.levels[0];
        for line in ["* 0103h  MB C, [DP].2", "* 0105h  MB 0A0h.0, C", "* 0109h  JBR off 0A0h.0", "> 010Dh"] {
            assert!(level.path.iter().any(|l| l.starts_with(line)), "{line}: {:#?}", level.path);
        }
        assert!(level.stopped_because.as_deref().unwrap().contains("4700h bit 2 (= 1), read at 0103h"));
    }

    fn step(index: u64, pc: u16, op: &'static str, uses: u8, defs: u8) -> Step {
        Step {
            index,
            cycles: index * 4,
            pc,
            dd: false,
            op,
            len: 2,
            branch: None,
            interrupted: false,
            bit: None,
            bank: 0x0200,
            uses,
            defs,
            alt: None,
            reads: Vec::new(),
            rom_reads: Vec::new(),
            writes: Vec::new(),
        }
    }

    fn read(addr: u16, value: u8, mask: u8, source: Stamp) -> Read {
        Read { addr, value, outside: 0, mask, stamps: [Source { written: source, origin: source, changed: source, origin_change: source }; 8] }
    }

    /// A ROM whose only instruction is RT at 0300h: a branch whose other way
    /// goes there leaves the code that can be followed, so it decided.
    fn rt_rom() -> Vec<u8> {
        let mut rom = vec![0xFF; 32768];
        rom[0x300] = 0x01;
        rom
    }

    #[test]
    fn the_choice_follows_data_from_another_routine_before_bookkeeping_and_constants() {
        // 1000 JBS 150h.0 (not taken, other way 0300h: decides)
        // 1002 JBS 160h.1 (not taken, other way 0300h: decides)
        // 1004 JBS 170h.2 (not taken, other way 0300h: decides)
        // 1006 SC
        // 1008 MB P1.4, C
        let mut b1 = step(1, 0x1000, "JBS", 0, 0);
        b1.branch = Some(false);
        b1.alt = Some(0x0300);
        b1.bit = Some(0);
        // Written by the routine's own code (1010h, next to the path).
        b1.reads = vec![read(0x150, 0x01, 0x01, stamp(0x1010, 0x90))];
        let mut b2 = b1.clone();
        b2.index = 2;
        b2.pc = 0x1002;
        b2.bit = Some(1);
        // Written elsewhere, earlier than the bookkeeping.
        b2.reads = vec![read(0x160, 0x02, 0x02, stamp(0x3C71, 0x20))];
        let mut b3 = b1.clone();
        b3.index = 3;
        b3.pc = 0x1004;
        b3.bit = Some(2);
        // A bare constant, the most recent of all.
        b3.reads = vec![read(0x170, 0x04, 0x04, Stamp { constant: true, ..stamp(0x5000, 0xA0) })];
        let sc = step(4, 0x1006, "SC", 0, REG_C);
        let mut mb = step(5, 0x1008, "MB", REG_C, 0);
        mb.bit = Some(4);
        let path = [b1, b2, b3, sc, mb];
        let shown: Vec<&Step> = path.iter().collect();
        let member = slice(&rt_rom(), &shown);
        assert_eq!(member, vec![true, true, true, true, true]);
        let choice = choose(&shown, &member, &[]);
        let Decision::Follow(list) = choice.decision else { panic!("expected a candidate") };
        let order: Vec<u16> = list.iter().map(|c| c.addr).collect();
        assert_eq!(order, vec![0x160, 0x150, 0x170]);
    }

    #[test]
    fn an_input_on_the_path_ends_the_chain_and_reads_outside_the_slice_do_not_count() {
        let mut ext = step(1, 0x2000, "MB", 0, REG_C);
        ext.bit = Some(2);
        ext.reads = vec![Read { addr: 0x0F00, value: 0x04, outside: 0xFF, mask: 0x04, stamps: Default::default() }];
        // Overwrites C: the external read above no longer counts.
        let rc = step(2, 0x2002, "RC", 0, REG_C);
        let mut ld = step(3, 0x2003, "LB", 0, REG_A | REG_Z);
        ld.reads = vec![read(0x2E8, 0x00, 0xFF, stamp(0x5210, 0x50))];
        let mut jne = step(4, 0x2005, "JNE", REG_Z, 0);
        jne.branch = Some(false);
        jne.alt = Some(0x0300);
        let mut target = step(5, 0x2007, "MB", REG_C, 0);
        target.bit = Some(3);
        let path = [ext, rc, ld, jne, target];
        let shown: Vec<&Step> = path.iter().collect();
        let member = slice(&rt_rom(), &shown);
        assert_eq!(member, vec![false, true, true, true, true]);
        let choice = choose(&shown, &member, &[]);
        assert!(matches!(&choice.decision, Decision::Follow(list) if list[0].addr == 0x2E8));

        // With the carry coming from the external read, the chain stops there.
        let path = [path[0].clone(), path[2].clone(), path[3].clone(), path[4].clone()];
        let shown: Vec<&Step> = path.iter().collect();
        let member = slice(&rt_rom(), &shown);
        let choice = choose(&shown, &member, &[]);
        match choice.decision {
            Decision::Stop { why, on } => {
                assert!(on.is_some_and(|c| c.addr == 0x0F00));
                assert!(why.contains("0F00h bit 2 (= 1), read at 2000h") && why.contains("82C55 port A bit 2"), "{why}");
            }
            Decision::Follow(_) => panic!("an input on the path must end the chain"),
        }
    }

    #[test]
    fn a_byte_read_whole_names_a_bit_only_when_one_bit_changed_last() {
        // The P30 countdown 2E8h: loaded with 0Ah by 2818h, counted down by
        // 5210h to 01h, then read whole. Bits 1 and 3 were last changed by
        // counting and came from the load; bit 0 has no load behind it. The
        // search lands on bit 1, which is 0 in the 01h read: naming it said
        // "2E8h (= 01h; its bit 1 is the one followed)". The load wrote the
        // whole byte, so no bit is named.
        let load = Stamp { op: "MOVB", ..stamp(0x2818, 0x10) };
        let dec = Stamp { op: "DEC", counted: true, ..stamp(0x5210, 0x60) };
        let mut ld = step(3, 0x2003, "LB", 0, REG_A | REG_Z);
        let mut countdown = Read { addr: 0x2E8, value: 0x01, outside: 0, mask: 0xFF, stamps: Default::default() };
        for b in [0, 1, 3] {
            let origin = if b == 0 { Stamp::default() } else { load };
            countdown.stamps[b] = Source { written: dec, origin, changed: dec, origin_change: origin };
        }
        ld.reads = vec![countdown];
        let shown = [&ld];
        let c = &candidates(&shown, &[true])[0];
        assert_eq!((c.addr, c.bit, c.last_bit, c.follow.pc), (0x2E8, None, Some(1), 0x2818));
        assert_eq!(c.bit_named(), None);
        let text = c.describe();
        assert!(text.starts_with("2E8h (= 01h), read at 2003h, set by 2818h at "), "{text}");
        assert!(text.contains(", then counted on by itself (last at 5210h, "), "{text}");
        assert!(!text.contains("bit"), "{text}");
        assert_eq!(c.changed_text(), "last loaded 2E8h; it counted on by itself from there to the value read");

        // The fault word 214h = 80h read whole, its bit 7 set by SBR 51C5h
        // after the rest of the byte was cleared by a store: that bit is
        // the byte's last change, and it is named with its value.
        let clear = Stamp { op: "STB", ..stamp(0x0400, 0x05) };
        let sbr = Stamp { op: "SBR", ..stamp(0x51C5, 0x50) };
        let mut word = Read { addr: 0x214, value: 0x80, outside: 0, mask: 0xFF, stamps: Default::default() };
        for (b, source) in word.stamps.iter_mut().enumerate() {
            let s = if b == 7 { sbr } else { clear };
            *source = Source { written: s, origin: s, changed: s, origin_change: s };
        }
        ld.reads = vec![word];
        let shown = [&ld];
        let c = &candidates(&shown, &[true])[0];
        assert_eq!((c.bit, c.last_bit, c.bit_named()), (None, Some(7), Some(7)));
        let text = c.describe();
        assert!(
            text.starts_with(
                "214h (= 80h), read whole at 2003h; the last of its bits to change was bit 7, 1 at that read, set by 51C5h at "
            ),
            "{text}"
        );
        assert_eq!(c.changed_text(), "last set 214h bit 7 before it was read");
    }

    #[test]
    fn a_branch_whose_other_way_only_resets_or_rejoins_does_not_count() {
        // The other way of the first branch is BRK (the fill): a sanity
        // check. The target needs nothing it read.
        let mut check = step(1, 0x2000, "JNE", REG_Z, 0);
        check.branch = Some(false);
        check.alt = Some(0x0400);
        let mut cmp = step(0, 0x1FFE, "CMP", 0, REG_C | REG_Z);
        cmp.reads = vec![read(0x350, 0x00, 0xFF, stamp(0x4000, 0x50))];
        let sb = step(2, 0x2002, "SB", 0, REG_Z);
        let path = [cmp, check, sb];
        let shown: Vec<&Step> = path.iter().collect();
        assert_eq!(slice(&rt_rom(), &shown), vec![false, false, true]);
    }
}
