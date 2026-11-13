// OKI 66207 executor.
//
// Instructions are decoded by `decoder` into a FULL_OPCODES index, and the
// operand text for that index is parsed by `operand` into an Arg tree. This
// module evaluates that tree, so each mnemonic is implemented once regardless
// of how many addressing-mode encodings it has.
//
// Conventions taken from the ISA and confirmed against dasm662's listings:
//   * Word ops act on the full 16-bit accumulator; byte ops (mnemonic ending
//     in B) act on its low half.
//   * CF is set on *borrow* by SUB/SBC/CMP, so the unsigned conditions are
//     JLT = CF, JGE = !CF, JGT = !CF && !ZF, JLE = CF || ZF.
//   * `LC`/`CMPC` read code space; everything else reads data space.

use crate::bus::{Bus, EXTERNAL_DATA_START};
use crate::cpu::Cpu;
use crate::decoder::{decode, Decoded, Place};
use crate::operand::{table, Arg, Mem, Parsed, Reg};

#[derive(Debug)]
pub enum ExecError {
    /// No FULL_OPCODES pattern matched the bytes at PC.
    UndefinedOpcode { pc: u16, byte: u8 },
    /// Decoded fine, but this module has no semantics for it yet.
    Unimplemented { pc: u16, mnemonic: &'static str },
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecError::UndefinedOpcode { pc, byte } => {
                write!(f, "undefined opcode {byte:#04X} at {pc:#06X}")
            }
            ExecError::Unimplemented { pc, mnemonic } => {
                write!(f, "unimplemented instruction `{mnemonic}` at {pc:#06X}")
            }
        }
    }
}

pub struct Exec<'a> {
    pub cpu: &'a mut Cpu,
    pub bus: &'a mut Bus,
    d: Decoded,
    /// Set when a *conditional* branch (Jcc / JBS / JBR / JRNZ) actually
    /// transfers control. The manual charges four extra INT cycles for a taken
    /// conditional branch over the not-taken cost stored in `Decoded::cycles`;
    /// `step` reads this to add them.
    branch_taken: bool,
}

impl<'a> Exec<'a> {
    // ---- operand address / value plumbing -------------------------------

    /// X1/X2/DP/USP are not CPU registers: they live in RAM at 0x0080 in the
    /// pointing-register set selected by PSW's SCB field (manual Fig. 1-5).
    fn preg_addr(&self, r: Reg) -> Option<u16> {
        let slot = match r {
            Reg::X1 => 0,
            Reg::X2 => 2,
            Reg::Dp => 4,
            Reg::Usp => 6,
            _ => return None,
        };
        Some(0x0080 + self.cpu.scb() * 8 + slot)
    }

    fn reg(&mut self, r: Reg) -> u16 {
        if let Some(a) = self.preg_addr(r) {
            return self.bus.read_data_u16(a);
        }
        match r {
            Reg::A => self.cpu.a,
            Reg::Ssp => self.cpu.ssp,
            Reg::Lrb => self.cpu.lrb,
            Reg::Psw => self.cpu.psw_u16(),
            Reg::PswL => self.cpu.psw_u16() & 0xFF,
            Reg::PswH => self.cpu.psw_u16() >> 8,
            _ => unreachable!("pointing registers handled above"),
        }
    }

    fn set_reg(&mut self, r: Reg, v: u16) {
        if let Some(a) = self.preg_addr(r) {
            self.bus.write_data_u16(a, v);
            return;
        }
        match r {
            Reg::A => self.cpu.a = v,
            Reg::Ssp => self.cpu.ssp = v,
            Reg::Lrb => self.cpu.lrb = v,
            Reg::Psw => self.cpu.set_psw_u16(v),
            Reg::PswL => {
                let p = self.cpu.psw_u16();
                self.cpu.set_psw_u16((p & 0xFF00) | (v & 0xFF));
            }
            Reg::PswH => {
                let p = self.cpu.psw_u16();
                self.cpu.set_psw_u16((p & 0x00FF) | (v << 8));
            }
            _ => unreachable!("pointing registers handled above"),
        }
    }

    /// SSP, LRB, PSW and the accumulator are memory-mapped at 0x00..0x07, so
    /// `CLR off(PSW)` (a write to 0x0004) has to land on the real PSW rather
    /// than on plain RAM. Returns the aliased register and the byte offset
    /// within it.
    fn alias(addr: u16) -> Option<(Reg, bool)> {
        let reg = match addr & !1 {
            0x00 => Reg::Ssp,
            0x02 => Reg::Lrb,
            0x04 => Reg::Psw,
            0x06 => Reg::A,
            _ => return None,
        };
        Some((reg, addr & 1 == 1))
    }

    fn load(&mut self, addr: u16, byte: bool) -> u16 {
        if let Some((r, high)) = Self::alias(addr) {
            let v = self.reg(r);
            return if !byte {
                v
            } else if high {
                v >> 8
            } else {
                v & 0xFF
            };
        }
        if byte {
            self.bus.read_data_u8(addr) as u16
        } else {
            self.bus.read_data_u16(addr)
        }
    }

    fn store(&mut self, addr: u16, byte: bool, v: u16) {
        if let Some((r, high)) = Self::alias(addr) {
            let cur = self.reg(r);
            let next = if !byte {
                v
            } else if high {
                (cur & 0x00FF) | (v << 8)
            } else {
                (cur & 0xFF00) | (v & 0xFF)
            };
            self.set_reg(r, next);
            return;
        }
        if byte {
            self.bus.write_data_u8(addr, v as u8);
        } else {
            self.bus.write_data_u16(addr, v);
        }
    }

    /// Effective address of a memory operand.
    fn ea(&mut self, m: Mem) -> u16 {
        let f = self.d.fields;
        match m {
            Mem::Direct => f.n8 as u16,
            Mem::DirectAlt => f.n8_alt as u16,
            Mem::OffPage => self.cpu.off_page(f.n8),
            Mem::OffPageAlt => self.cpu.off_page(f.n8_alt),
            Mem::AtReg(r) => self.reg(r),
            Mem::AtEr(n) => {
                let a = self.cpu.bank_base().wrapping_add(n as u16 * 2);
                self.bus.read_data_u16(a)
            }
            Mem::IdxUsp => { let u = self.reg(Reg::Usp); u.wrapping_add(f.s8 as u16) }
            Mem::IdxReg(r) => self.reg(r).wrapping_add(f.n16),
            // The base word goes through `load`, not straight to the bus:
            // the P28 indexes its fault tables with `LCB A, 6C99h[ACC]`
            // (B5 06 AB 99 6C), where "RAM 06h" is the accumulator itself.
            Mem::IdxMemN8 => {
                let base = self.load(f.n8 as u16, false);
                base.wrapping_add(f.n16)
            }
            Mem::IdxMemOff => {
                let a = self.cpu.off_page(f.n8);
                let base = self.load(a, false);
                base.wrapping_add(f.n16)
            }
            Mem::Abs16 => f.n16,
        }
    }

    /// Read an operand as a value. `code` selects code space for LC/CMPC.
    fn read(&mut self, a: &Arg, byte: bool, code: bool) -> u16 {
        let f = self.d.fields;
        match a {
            Arg::Reg(Reg::A) if byte => self.cpu.a & 0xFF,
            Arg::Reg(r) => self.reg(*r),
            Arg::Er(n) => {
                let addr = self.cpu.bank_base().wrapping_add(*n as u16 * 2);
                self.bus.read_data_u16(addr)
            }
            Arg::R(n) => {
                let addr = self.cpu.bank_base().wrapping_add(*n as u16);
                self.bus.read_data_u8(addr) as u16
            }
            Arg::Carry => self.cpu.cf as u16,
            Arg::ImmN8 => f.n8 as u16,
            Arg::ImmN16 => f.n16,
            Arg::ImmN8Alt => f.n8_alt as u16,
            Arg::ImmN16Alt => f.n16_alt,
            Arg::Mem(m) => {
                let addr = self.ea(*m);
                match (byte, code) {
                    (true, true) => self.bus.read_code_u8(addr) as u16,
                    (false, true) => self.bus.read_code_u16(addr),
                    (b, false) => self.load(addr, b),
                }
            }
            // Bit operands are byte objects whatever DD says (manual p. 3-77),
            // which matters for odd aliases such as ACCH (address 07h).
            Arg::Bit(inner, n) => (self.read(inner, true, code) >> n) & 1,
            Arg::Addr16 => f.addr16,
            Arg::Rel8 => f.rel8 as u16,
            Arg::Lit(n) => *n as u16,
        }
    }

    fn write(&mut self, a: &Arg, byte: bool, v: u16) {
        match a {
            Arg::Reg(Reg::A) if byte => self.cpu.a = (self.cpu.a & 0xFF00) | (v & 0xFF),
            Arg::Reg(r) => self.set_reg(*r, v),
            Arg::Er(n) => {
                let addr = self.cpu.bank_base().wrapping_add(*n as u16 * 2);
                self.bus.write_data_u16(addr, v);
            }
            Arg::R(n) => {
                let addr = self.cpu.bank_base().wrapping_add(*n as u16);
                self.bus.write_data_u8(addr, v as u8);
            }
            Arg::Carry => self.cpu.cf = v & 1 != 0,
            Arg::Mem(m) => {
                let addr = self.ea(*m);
                self.store(addr, byte, v);
            }
            Arg::Bit(inner, n) => {
                // Byte-wide read-modify-write: see the read side.
                let cur = self.read(inner, true, false);
                let next = if v & 1 != 0 {
                    cur | (1 << n)
                } else {
                    cur & !(1 << n)
                };
                self.write(inner, true, next);
            }
            // Immediates and branch targets are never write destinations.
            _ => {}
        }
    }

    // ---- external-memory timing -----------------------------------------

    /// Address a `Place` resolves to with the registers as they stand.
    fn place_addr(&mut self, place: Place) -> u16 {
        let f = self.d.fields;
        match place {
            Place::Bank => self.cpu.bank_base(),
            Place::OffPage => self.cpu.off_page(f.n8),
            Place::OffPageAlt => self.cpu.off_page(f.n8_alt),
            Place::AtDp => self.reg(Reg::Dp),
            Place::UspIdx => self.reg(Reg::Usp).wrapping_add(f.s8 as u16),
            Place::X1Idx => self.reg(Reg::X1).wrapping_add(f.n16),
            Place::X2Idx => self.reg(Reg::X2).wrapping_add(f.n16),
            Place::X1IdxAlt => self.reg(Reg::X1).wrapping_add(f.n16_alt),
            Place::X2IdxAlt => self.reg(Reg::X2).wrapping_add(f.n16_alt),
        }
    }

    /// Cycles the manual's EXT columns add for this instruction: the cost of
    /// each operand that is in external data memory, and of the system stack
    /// if that is. Must run before the instruction changes any register.
    fn external_cycles(&mut self) -> u16 {
        let ext = self.d.ext;
        let mut cycles = 0;
        for (place, cost) in ext.operands.into_iter().flatten() {
            if self.place_addr(place) >= EXTERNAL_DATA_START {
                cycles += cost;
            }
        }
        if ext.stack != 0 && self.cpu.ssp >= EXTERNAL_DATA_START {
            cycles += ext.stack;
        }
        cycles
    }

    // ---- flag helpers ----------------------------------------------------

    fn set_zf(&mut self, v: u16, byte: bool) {
        self.cpu.zf = if byte { v & 0xFF == 0 } else { v == 0 };
    }

    fn mask(v: u16, byte: bool) -> u16 {
        if byte {
            v & 0xFF
        } else {
            v
        }
    }

    // ---- stack -----------------------------------------------------------

    fn push_sys(&mut self, v: u16) {
        let a = self.cpu.ssp;
        self.bus.write_data_u16(a, v);
        self.cpu.ssp = self.cpu.ssp.wrapping_sub(2);
    }

    fn pop_sys(&mut self) -> u16 {
        self.cpu.ssp = self.cpu.ssp.wrapping_add(2);
        self.bus.read_data_u16(self.cpu.ssp)
    }

    // ---- main dispatch ---------------------------------------------------

    fn run(&mut self, p: &Parsed) -> Result<(), ExecError> {
        let byte = p.byte_width;
        let args = &p.args;
        // Base name with any byte-width suffix removed, so ADD/ADDB share an arm.
        let base = if byte {
            &p.op[..p.op.len() - 1]
        } else {
            p.op
        };

        match base {
            "NOP" => {}

            // ---- data movement -------------------------------------------
            // L/LB load the accumulator; ST/STB store it; MOV/MOVB are general.
            // L/LB set ZF from the loaded value (manual pp. 3-69, 3-70); MOV
            // affects no flags (p. 3-96). The ROM branches directly off loads,
            // e.g. `LB A, off(135h)` / `JNE` at 0x5C4F.
            "L" => {
                let v = self.read(&args[1], byte, false);
                self.set_zf(v, byte);
                self.write(&args[0], byte, v);
            }
            "MOV" => {
                let v = self.read(&args[1], byte, false);
                self.write(&args[0], byte, v);
            }
            "ST" => {
                let v = self.read(&args[0], byte, false);
                self.write(&args[1], byte, v);
            }
            // Load from code (ROM) space -- table lookups. LC/LCB also set ZF
            // from the loaded value (manual pp. 3-71..3-76).
            "LC" => {
                let v = self.read(&args[1], byte, true);
                self.set_zf(v, byte);
                self.write(&args[0], byte, v);
            }
            "XCHG" => {
                let x = self.read(&args[0], byte, false);
                let y = self.read(&args[1], byte, false);
                self.write(&args[0], byte, y);
                self.write(&args[1], byte, x);
            }
            // Only the accumulator forms touch a flag: `CLR A` / `CLRB A` set
            // ZF to 1 (manual pp. 3-31, 3-33); every other object leaves the
            // flags alone (pp. 3-32, 3-34).
            "CLR" => {
                self.write(&args[0], byte, 0);
                if args[0] == Arg::Reg(Reg::A) {
                    self.cpu.zf = true;
                }
            }

            // ---- arithmetic / logic --------------------------------------
            "ADD" | "ADC" | "SUB" | "SBC" | "CMP" | "CMPC" => {
                // CMP/CMPC discard the result and only set flags. CMPC's source
                // is code space.
                let code = base == "CMPC";
                let lhs = self.read(&args[0], byte, false);
                let rhs = self.read(&args[1], byte, code);
                let carry_in = match base {
                    "ADC" | "SBC" => self.cpu.cf as u32,
                    _ => 0,
                };
                let (a, b) = (lhs as u32, rhs as u32);
                let width_mask: u32 = if byte { 0xFF } else { 0xFFFF };
                // Half carry is the carry (borrow) between bits 3 and 4. The
                // manual shows it for byte operations (DAA/DAS examples, pp.
                // 3-50 and 3-53); the word forms are taken to report the same
                // bit position.
                let (res, carry, half) = match base {
                    "ADD" | "ADC" => {
                        let r = (a & width_mask) + (b & width_mask) + carry_in;
                        (
                            r & width_mask,
                            r > width_mask,
                            (a & 0xF) + (b & 0xF) + carry_in > 0xF,
                        )
                    }
                    _ => {
                        let r = (a & width_mask)
                            .wrapping_sub(b & width_mask)
                            .wrapping_sub(carry_in);
                        (
                            r & width_mask,
                            (a & width_mask) < (b & width_mask) + carry_in,
                            (a & 0xF) < (b & 0xF) + carry_in,
                        )
                    }
                };
                self.cpu.cf = carry;
                self.set_zf(res as u16, byte);
                // The compares change ZF and CF only (Tables 3-12, 3-13, 3-28
                // and 3-29); ADD/ADC/SUB/SBC also change HC (Tables 3-14..3-21).
                if !matches!(base, "CMP" | "CMPC") {
                    self.cpu.hc = half;
                    self.write(&args[0], byte, res as u16);
                }
            }
            "AND" | "OR" | "XOR" => {
                let lhs = self.read(&args[0], byte, false);
                let rhs = self.read(&args[1], byte, false);
                let res = match base {
                    "AND" => lhs & rhs,
                    "OR" => lhs | rhs,
                    _ => lhs ^ rhs,
                };
                let res = Self::mask(res, byte);
                self.set_zf(res, byte);
                self.write(&args[0], byte, res);
            }
            "INC" | "DEC" => {
                let v = self.read(&args[0], byte, false);
                let res = if base == "INC" {
                    v.wrapping_add(1)
                } else {
                    v.wrapping_sub(1)
                };
                let res = Self::mask(res, byte);
                // INC/DEC change ZF and HC and leave CF alone (Table 3-11).
                self.cpu.hc = if base == "INC" {
                    v & 0xF == 0xF
                } else {
                    v & 0xF == 0
                };
                // "If LRB is the addressing object in any addressing mode, ZF
                // will not be affected" (INC, p. 3-60).
                if !(base == "INC" && args[0] == Arg::Reg(Reg::Lrb)) {
                    self.set_zf(res, byte);
                }
                self.write(&args[0], byte, res);
            }
            "MUL" => {
                // MUL/MULB have no source operands in their mnemonics. The
                // operands and destinations are fixed by the ISA:
                //
                //   MUL   (er1, A) <- A * er0
                //   MULB  A        <- AL * r0
                //
                // See the MSM66201 Instruction Manual, chapter 3, pp. 3-100
                // and 3-101. Treating these as ordinary two-operand
                // instructions used to index an empty argument list as soon
                // as the OEM ROM reached its first fuel calculation.
                if byte {
                    let product = self
                        .read(&Arg::Reg(Reg::A), true, false)
                        .wrapping_mul(self.read(&Arg::R(0), true, false));
                    self.cpu.a = product;
                    self.set_zf(product, false);
                } else {
                    let product = (self.cpu.a as u32)
                        .wrapping_mul(self.read(&Arg::Er(0), false, false) as u32);
                    self.cpu.a = product as u16;
                    self.write(&Arg::Er(1), false, (product >> 16) as u16);
                    self.cpu.zf = product == 0;
                }
            }
            "DIV" => {
                // DIV/DIVB likewise use fixed registers:
                //
                //   DIV   (er0, A) <- (er0, A) / er2; er1 <- remainder
                //   DIVB  A        <- A / r0;         r1  <- remainder
                //
                // Divide-by-zero results are undefined on the chip; only CF=1
                // is specified. Preserve the operands in that case.
                if byte {
                    let divisor = self.read(&Arg::R(0), true, false);
                    if divisor == 0 {
                        self.cpu.cf = true;
                    } else {
                        let dividend = self.cpu.a;
                        let quotient = dividend / divisor;
                        let remainder = dividend % divisor;
                        self.cpu.a = quotient;
                        self.write(&Arg::R(1), true, remainder);
                        self.cpu.cf = false;
                        self.set_zf(quotient, false);
                    }
                } else {
                    let divisor = self.read(&Arg::Er(2), false, false) as u32;
                    if divisor == 0 {
                        self.cpu.cf = true;
                    } else {
                        let dividend =
                            ((self.read(&Arg::Er(0), false, false) as u32) << 16)
                                | self.cpu.a as u32;
                        let quotient = dividend / divisor;
                        let remainder = dividend % divisor;
                        self.write(&Arg::Er(0), false, (quotient >> 16) as u16);
                        self.cpu.a = quotient as u16;
                        self.write(&Arg::Er(1), false, remainder as u16);
                        self.cpu.cf = false;
                        self.cpu.zf = quotient == 0;
                    }
                }
            }
            "EXTND" => {
                // Sign-extend the low byte across the accumulator.
                let lo = (self.cpu.a & 0xFF) as u8;
                self.cpu.a = lo as i8 as i16 as u16;
            }
            // SWAP/SWAPB take no operand: they act on the accumulator,
            // exchanging its two bytes (word) or its two nibbles (byte).
            "SWAP" => {
                let a = self.cpu.a;
                self.cpu.a = if byte {
                    (a & 0xFF00) | ((a & 0x0F) << 4) | ((a >> 4) & 0x0F)
                } else {
                    a.rotate_right(8)
                };
            }

            // ---- shifts and rotates --------------------------------------
            "ROL" | "ROR" | "SLL" | "SRL" | "SRA" => {
                let v = self.read(&args[0], byte, false);
                let bits = if byte { 8 } else { 16 };
                let msb = 1u32 << (bits - 1);
                let v32 = v as u32 & (msb * 2 - 1);
                let cin = self.cpu.cf as u32;
                let (res, carry) = match base {
                    // ROL/ROR rotate through carry: the old CF enters the
                    // vacated bit and the shifted-out bit becomes CF (manual
                    // pp. 3-116..3-123). The ROM chains them after SLL/SRL to
                    // shift 32-bit products, e.g. at 0x4FBF.
                    "ROL" => (((v32 << 1) | cin) & (msb * 2 - 1), v32 & msb != 0),
                    "ROR" => ((v32 >> 1) | (cin << (bits - 1)), v32 & 1 != 0),
                    "SLL" => ((v32 << 1) & (msb * 2 - 1), v32 & msb != 0),
                    "SRL" => (v32 >> 1, v32 & 1 != 0),
                    // Arithmetic right shift keeps the sign bit.
                    _ => ((v32 >> 1) | (v32 & msb), v32 & 1 != 0),
                };
                // Shifts and rotates change CF only (Tables 3-8..3-10).
                self.cpu.cf = carry;
                self.write(&args[0], byte, res as u16);
            }

            // ---- carry and bit operations --------------------------------
            "SC" => self.cpu.cf = true,
            "RC" => self.cpu.cf = false,
            // Bit set/reset are test-and-modify. ZF follows the normal zero
            // convention: it is one when the bit's previous value was zero.
            // MSM66201 Instruction Manual pp. 3-114, 3-115 and 3-127.
            "SB" | "RB" => {
                let old = self.read(&args[0], byte, false) & 1;
                self.cpu.zf = old == 0;
                self.write(&args[0], byte, (base == "SB") as u16);
            }
            // MB moves a bit; whichever side is C decides the direction. It
            // does not affect ZF (manual pp. 3-77 and 3-78).
            "MB" => {
                if args[0] == Arg::Carry {
                    let v = self.read(&args[1], byte, false) & 1;
                    self.cpu.cf = v != 0;
                } else {
                    let v = self.cpu.cf as u16;
                    self.write(&args[0], byte, v);
                }
            }
            // Same move, with the bit selected indirectly by A[0:2]. MBR also
            // leaves ZF unchanged (manual pp. 3-79 and 3-80).
            "MBR" => {
                let mask = 1u16 << (self.cpu.a & 0x07);
                if args[0] == Arg::Carry {
                    let v = self.read(&args[1], true, false);
                    let b = (v & mask != 0) as u16;
                    self.cpu.cf = b != 0;
                } else {
                    let b = self.cpu.cf as u16;
                    let v = self.read(&args[0], true, false);
                    let next = if b != 0 { v | mask } else { v & !mask };
                    self.write(&args[0], true, next);
                }
            }
            // "Register Indirect Bit Addressing": these carry no bit index in
            // the encoding; the manual (ch.2 sec.3) specifies the bit location
            // as "the bits from 0 to 2 of the accumulator".
            "SBR" | "RBR" | "TBR" => {
                let mask = 1u16 << (self.cpu.a & 0x07);
                let v = self.read(&args[0], true, false);
                self.cpu.zf = v & mask == 0;
                match base {
                    "SBR" => self.write(&args[0], true, v | mask),
                    "RBR" => self.write(&args[0], true, v & !mask),
                    // TBR tests only.
                    _ => {}
                }
            }

            // ---- stack ---------------------------------------------------
            "PUSHS" => {
                let v = self.read(&args[0], byte, false);
                self.push_sys(v);
            }
            "POPS" => {
                let v = self.pop_sys();
                self.write(&args[0], byte, v);
            }
            "PUSHU" => {
                let v = self.read(&args[0], byte, false);
                let u = self.reg(Reg::Usp).wrapping_sub(2);
                self.set_reg(Reg::Usp, u);
                self.bus.write_data_u16(u, v);
            }

            // ---- control flow --------------------------------------------
            "J" => {
                // `J addr16` is absolute; `J [reg]` takes the operand's value.
                self.cpu.pc = self.read(&args[0], false, false);
            }
            "SJ" => {
                let off = self.d.fields.rel8 as i16;
                self.cpu.pc = self.cpu.pc.wrapping_add(off as u16);
            }
            "CAL" => {
                let target = self.read(&args[0], false, false);
                let ret = self.cpu.pc;
                self.push_sys(ret);
                self.cpu.pc = target;
            }
            "SCAL" => {
                let off = self.d.fields.rel8 as i16;
                let ret = self.cpu.pc;
                self.push_sys(ret);
                self.cpu.pc = self.cpu.pc.wrapping_add(off as u16);
            }
            "VCAL" => {
                // Vector call through the table at 0x0028.
                let n = self.read(&args[0], false, false);
                let ret = self.cpu.pc;
                self.push_sys(ret);
                self.cpu.pc = self.bus.read_code_u16(0x0028 + n * 2);
            }
            "RT" => self.cpu.pc = self.pop_sys(),
            "RTI" => {
                // MSM66201 instruction manual, RTI (3-126): hardware restores
                // PSW, LRB, A and PC in that order and advances SSP by eight.
                let psw = self.pop_sys();
                let lrb = self.pop_sys();
                let a = self.pop_sys();
                let pc = self.pop_sys();
                self.cpu.set_psw_u16(psw);
                self.cpu.lrb = lrb;
                self.cpu.a = a;
                self.cpu.pc = pc;
            }
            "JEQ" | "JNE" | "JLT" | "JGE" | "JGT" | "JLE" => {
                let (z, c) = (self.cpu.zf, self.cpu.cf);
                let take = match base {
                    "JEQ" => z,
                    "JNE" => !z,
                    "JLT" => c,
                    "JGE" => !c,
                    "JGT" => !c && !z,
                    _ => c || z,
                };
                if take {
                    let off = self.d.fields.rel8 as i16;
                    self.cpu.pc = self.cpu.pc.wrapping_add(off as u16);
                    self.branch_taken = true;
                }
            }
            "JBS" | "JBR" => {
                let bit = self.read(&args[0], byte, false) & 1;
                let take = if base == "JBS" { bit == 1 } else { bit == 0 };
                if take {
                    let off = self.d.fields.rel8 as i16;
                    self.cpu.pc = self.cpu.pc.wrapping_add(off as u16);
                    self.branch_taken = true;
                }
            }
            "JRNZ" => {
                // The loop counter is the low byte of DP only: "DPL <- DPL - 1;
                // IF DPL != 0 THEN PC <- address" (manual p. 3-68). DPH is not
                // touched and no flag changes.
                let dp = self.read(&args[0], false, false);
                let v = dp.wrapping_sub(1) & 0x00FF;
                self.write(&args[0], false, (dp & 0xFF00) | v);
                if v != 0 {
                    let off = self.d.fields.rel8 as i16;
                    self.cpu.pc = self.cpu.pc.wrapping_add(off as u16);
                    self.branch_taken = true;
                }
            }
            "BRK" => {
                let ret = self.cpu.pc;
                let psw = self.cpu.psw_u16();
                self.push_sys(psw);
                self.push_sys(ret);
                self.cpu.pc = self.bus.read_code_u16(0x0002);
            }

            // ---- misc ----------------------------------------------------
            "DAA" | "DAS" => {
                // Decimal adjust of AL after a byte add/subtract (manual pp.
                // 3-49..3-53). Two byte-long steps: +/-6 on the low nibble if
                // it exceeds 9 or HC is set, then +/-6 on the high nibble if
                // it exceeds 9 or C is set -- C as it stands after step one.
                // A carry out of bit 7 in either step sets C and nothing
                // clears it, so a carry from the preceding add survives. ZF is
                // not affected.
                let add = base == "DAA";
                let mut v = self.cpu.a & 0xFF;
                let mut carry = self.cpu.cf;
                let mut half = false;
                if (v & 0x0F) > 9 || self.cpu.hc {
                    if add {
                        half = (v & 0x0F) + 6 > 0x0F;
                        v += 0x06;
                    } else {
                        v = v.wrapping_sub(0x06);
                    }
                    carry |= v > 0xFF;
                    v &= 0xFF;
                }
                if (v >> 4) > 9 || carry {
                    v = if add { v + 0x60 } else { v.wrapping_sub(0x60) };
                    carry |= v > 0xFF;
                    v &= 0xFF;
                }
                self.cpu.cf = carry;
                // DAA leaves HC as step one produced it; DAS always clears it.
                self.cpu.hc = add && half;
                self.cpu.a = (self.cpu.a & 0xFF00) | v;
            }
            "XNBL" => {
                // Exchange the nibbles of the accumulator low byte with memory.
                let m = self.read(&args[0], true, false);
                let a = self.cpu.a & 0xFF;
                self.write(&args[0], true, (m & 0xF0) | (a & 0x0F));
                self.cpu.a = (self.cpu.a & 0xFF00) | ((a & 0xF0) | (m & 0x0F));
            }
            "SMOVI" => {
                // Block move [DP] -> [X1], post-incrementing both.
                let (src, dst) = (self.reg(Reg::Dp), self.reg(Reg::X1));
                let v = self.bus.read_data_u8(src);
                self.bus.write_data_u8(dst, v);
                self.set_reg(Reg::Dp, src.wrapping_add(1));
                self.set_reg(Reg::X1, dst.wrapping_add(1));
            }

            _ => {
                return Err(ExecError::Unimplemented {
                    pc: self.cpu.pc,
                    mnemonic: self.d.mnemonic,
                })
            }
        }
        Ok(())
    }
}

/// Fetch, decode and execute one instruction at PC.
pub fn step(cpu: &mut Cpu, bus: &mut Bus) -> Result<Decoded, ExecError> {
    let pc = cpu.pc;
    let d = match decode(cpu.dd, |i| bus.read_code_u8(pc.wrapping_add(i as u16))) {
        Some(d) => d,
        None => {
            return Err(ExecError::UndefinedOpcode {
                pc,
                byte: bus.read_code_u8(pc),
            })
        }
    };

    // PC advances past the instruction before execution, so rel8 targets and
    // pushed return addresses are relative to the *next* instruction.
    cpu.pc = pc.wrapping_add(d.len as u16);
    // `cycles` now tracks real INT machine cycles (decoder::int_cycles), not
    // instruction bytes, so peripheral time and injector pulse widths measured
    // from it use the 5 MHz machine-cycle clock and come out in microseconds.
    cpu.cycles += d.cycles as u64;
    cpu.instructions += 1;

    let parsed = match &table()[d.index] {
        Some(p) => p,
        None => {
            return Err(ExecError::Unimplemented {
                pc,
                mnemonic: d.mnemonic,
            })
        }
    };

    let dd_after = d.dd_after;
    let mut ex = Exec {
        cpu,
        bus,
        d: d.clone(),
        branch_taken: false,
    };
    // Operands in external data memory cost the manual's EXT count. Where
    // they are is decided now, before the instruction moves a pointer.
    let external = ex.external_cycles();
    ex.cpu.cycles += external as u64;
    ex.run(parsed)?;
    // Read the flag before touching `cpu` again so the reborrow held by `ex`
    // ends cleanly.
    let branch_taken = ex.branch_taken;

    // L/LB-class instructions leave the word/byte mode set for what follows.
    if let Some(v) = dd_after {
        cpu.dd = v;
    }
    // A taken conditional branch costs four cycles more than the fall-through.
    if branch_taken {
        cpu.cycles += 4;
    }
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpu_with_local_bank() -> (Cpu, Bus) {
        let mut cpu = Cpu::new();
        // Keep r0..r7 away from the memory-mapped CPU registers at 0x00.
        cpu.lrb = 0x20;
        (cpu, Bus::new())
    }

    /// Machine cycles `step` charges for one instruction, against the
    /// instruction manual's INT column, for one representative of each path:
    /// arithmetic, memory operands, branches both ways, call, return and RTI.
    #[test]
    fn step_charges_the_manuals_machine_cycles() {
        // (encoding, DD, ZF, DP, mnemonic, cycles)
        let cases: &[(&[u8], bool, bool, u16, &str, u64)] = &[
            (&[0x86, 0x34, 0x12], true, false, 0x0300, "ADD A, #N16", 6),
            (&[0x08], true, false, 0x0300, "ADD A, er0", 3),
            (&[0xF4, 0x40], false, false, 0x0300, "LB A, off N8", 4),
            (&[0xE2], false, false, 0x0300, "L A, [DP]", 4),
            (&[0xB2, 0x8A], true, false, 0x0300, "MOV [DP], A", 6),
            (&[0xB2, 0x16], true, false, 0x0300, "INC [DP]", 7),
            (&[0xC2, 0xB7], false, false, 0x0300, "ROLB [DP]", 7),
            // Conditional branches: fall through, then taken (+4).
            (&[0xC9, 0x10], false, false, 0x0300, "JEQ rel8", 4),
            (&[0xC9, 0x10], false, true, 0x0300, "JEQ rel8", 8),
            (&[0xE8, 0x40, 0x10], false, false, 0x0300, "JBS off N8.0, rel8", 6),
            // DPL counts 01h -> 00h and falls through; 02h -> 01h and loops.
            (&[0x30, 0x10], false, false, 0x0301, "JRNZ DP, rel8", 7),
            (&[0x30, 0x10], false, false, 0x0302, "JRNZ DP, rel8", 11),
            (&[0x31, 0x10], false, false, 0x0300, "SCAL rel8", 9),
            (&[0x32, 0x00, 0x30], false, false, 0x0300, "CAL addr16", 9),
            (&[0x01], false, false, 0x0300, "RT", 7),
            (&[0x02], false, false, 0x0300, "RTI", 15),
        ];

        for (code, dd, zf, dp, mnemonic, cycles) in cases {
            let (mut cpu, mut bus) = cpu_with_local_bank();
            cpu.dd = *dd;
            cpu.zf = *zf;
            cpu.ssp = 0x0400;
            bus.write_data_u16(0x0084, *dp); // DP in pointing-register set 0
            bus.rom[..code.len()].copy_from_slice(code);

            let decoded = step(&mut cpu, &mut bus)
                .unwrap_or_else(|e| panic!("`{mnemonic}` must execute: {e}"));

            assert_eq!(decoded.mnemonic, *mnemonic);
            assert_eq!(cpu.cycles, *cycles, "machine cycles for `{mnemonic}`");
        }
    }

    /// `step` charges the manual's EXT count for whichever of an instruction's
    /// places is in external data memory: the object, the `off N8` page, the
    /// register bank, the system stack. The first three rows are the forms
    /// the stock P28 ROM uses on the 82C55 and the switch buffer.
    #[test]
    fn external_operands_cost_the_manuals_ext_cycles() {
        const ON_CHIP: u16 = 0x0020; // LRB: page 1, bank at 0100h
        const OFF_CHIP: u16 = 0x00A0; // LRB: page 5, bank at 0500h
        // (encoding, LRB, DP, SSP, mnemonic, cycles)
        let cases: &[(&[u8], u16, u16, u16, &str, u64)] = &[
            (&[0xF2], ON_CHIP, 0x0300, 0x0400, "LB A, [DP]", 4),
            (&[0xF2], ON_CHIP, 0x4700, 0x0400, "LB A, [DP]", 6),
            (&[0xD2], ON_CHIP, 0x0300, 0x0400, "STB A, [DP]", 4),
            (&[0xD2], ON_CHIP, 0x2F00, 0x0400, "STB A, [DP]", 6),
            (&[0xC2, 0x2A], ON_CHIP, 0x0300, 0x0400, "MB C, [DP].2", 7),
            (&[0xC2, 0x2A], ON_CHIP, 0x0F00, 0x0400, "MB C, [DP].2", 9),
            // Read-modify-write of an external word.
            (&[0xB2, 0x16], ON_CHIP, 0x0300, 0x0400, "INC [DP]", 7),
            (&[0xB2, 0x16], ON_CHIP, 0x2F00, 0x0400, "INC [DP]", 15),
            // Two places: neither, the object, the `off N8` source, both.
            (&[0xC2, 0xF3, 0x40], ON_CHIP, 0x0300, 0x0400, "XORB [DP], off N8", 9),
            (&[0xC2, 0xF3, 0x40], ON_CHIP, 0x2F00, 0x0400, "XORB [DP], off N8", 11),
            (&[0xC2, 0xF3, 0x40], OFF_CHIP, 0x0300, 0x0400, "XORB [DP], off N8", 14),
            (&[0xC2, 0xF3, 0x40], OFF_CHIP, 0x2F00, 0x0400, "XORB [DP], off N8", 16),
            // The register bank is memory too.
            (&[0xB2, 0x48], ON_CHIP, 0x0300, 0x0400, "MOV er0, [DP]", 6),
            (&[0xB2, 0x48], ON_CHIP, 0x2F00, 0x0400, "MOV er0, [DP]", 10),
            (&[0xB2, 0x48], OFF_CHIP, 0x0300, 0x0400, "MOV er0, [DP]", 10),
            (&[0xB2, 0x48], OFF_CHIP, 0x2F00, 0x0400, "MOV er0, [DP]", 14),
            (&[0x90, 0x35], ON_CHIP, 0x0300, 0x0400, "MUL", 27),
            (&[0x90, 0x35], OFF_CHIP, 0x0300, 0x0400, "MUL", 35),
            // A pointer fetched from memory.
            (&[0xB4, 0x40, 0x22], ON_CHIP, 0x0300, 0x0400, "J [off N8]", 8),
            (&[0xB4, 0x40, 0x22], OFF_CHIP, 0x0300, 0x0400, "J [off N8]", 13),
            // The system stack.
            (&[0x32, 0x00, 0x30], ON_CHIP, 0x0300, 0x0400, "CAL addr16", 9),
            (&[0x32, 0x00, 0x30], ON_CHIP, 0x0300, 0xFFFE, "CAL addr16", 13),
            (&[0x02], ON_CHIP, 0x0300, 0x0400, "RTI", 15),
            (&[0x02], ON_CHIP, 0x0300, 0xFFF0, "RTI", 31),
            // `N'16[X1]` takes its displacement from the first 16-bit field,
            // not from the immediate that follows (X1 is zero here).
            (&[0xB0, 0x00, 0x03, 0x98, 0x00, 0x47], ON_CHIP, 0x0300, 0x0400, "MOV N'16[X1], #N16", 12),
            (&[0xB0, 0x00, 0x47, 0x98, 0x00, 0x03], ON_CHIP, 0x0300, 0x0400, "MOV N'16[X1], #N16", 20),
        ];

        for (code, lrb, dp, ssp, mnemonic, cycles) in cases {
            let mut cpu = Cpu::new();
            let mut bus = Bus::new();
            cpu.lrb = *lrb;
            cpu.ssp = *ssp;
            bus.write_data_u16(0x0084, *dp); // DP in pointing-register set 0
            bus.rom[..code.len()].copy_from_slice(code);

            let decoded = step(&mut cpu, &mut bus)
                .unwrap_or_else(|e| panic!("`{mnemonic}` must execute: {e}"));

            assert_eq!(decoded.mnemonic, *mnemonic);
            assert_eq!(
                cpu.cycles, *cycles,
                "`{mnemonic}` with LRB = {lrb:04X}h, DP = {dp:04X}h, SSP = {ssp:04X}h"
            );
        }
    }

    #[test]
    fn mul_uses_the_implicit_accumulator_and_er0_operands() {
        let (mut cpu, mut bus) = cpu_with_local_bank();
        cpu.a = 0xFFFF;
        bus.write_data_u16(cpu.bank_base(), 2);
        bus.rom[0..2].copy_from_slice(&[0x90, 0x35]); // MUL

        let decoded = step(&mut cpu, &mut bus).expect("MUL must execute");

        assert_eq!(decoded.mnemonic, "MUL");
        assert_eq!(cpu.a, 0xFFFE);
        assert_eq!(bus.read_data_u16(cpu.bank_base() + 2), 1);
        assert!(!cpu.zf);
    }

    #[test]
    fn mulb_writes_the_full_sixteen_bit_product_to_a() {
        let (mut cpu, mut bus) = cpu_with_local_bank();
        cpu.a = 0xAB10;
        bus.write_data_u8(cpu.bank_base(), 0x10);
        bus.rom[0..2].copy_from_slice(&[0xA2, 0x34]); // MULB

        step(&mut cpu, &mut bus).expect("MULB must execute");

        assert_eq!(cpu.a, 0x0100);
        assert!(!cpu.zf);
    }

    #[test]
    fn div_uses_er0_a_as_the_dividend_and_er2_as_the_divisor() {
        let (mut cpu, mut bus) = cpu_with_local_bank();
        cpu.a = 0;
        let bank = cpu.bank_base();
        bus.write_data_u16(bank, 2); // upper half of the 32-bit dividend
        bus.write_data_u16(bank + 4, 2); // er2 divisor
        bus.rom[0..2].copy_from_slice(&[0x90, 0x37]); // DIV

        step(&mut cpu, &mut bus).expect("DIV must execute");

        assert_eq!(bus.read_data_u16(bank), 1); // upper quotient
        assert_eq!(cpu.a, 0); // lower quotient
        assert_eq!(bus.read_data_u16(bank + 2), 0); // remainder in er1
        assert!(!cpu.cf);
        assert!(!cpu.zf);
    }

    #[test]
    fn divb_writes_the_quotient_to_a_and_remainder_to_r1() {
        let (mut cpu, mut bus) = cpu_with_local_bank();
        cpu.a = 1000;
        let bank = cpu.bank_base();
        bus.write_data_u8(bank, 30);
        bus.rom[0..2].copy_from_slice(&[0xA2, 0x36]); // DIVB

        step(&mut cpu, &mut bus).expect("DIVB must execute");

        assert_eq!(cpu.a, 33);
        assert_eq!(bus.read_data_u8(bank + 1), 10);
        assert!(!cpu.cf);
        assert!(!cpu.zf);
    }

    #[test]
    fn divide_by_zero_sets_carry_without_destroying_operands() {
        let (mut cpu, mut bus) = cpu_with_local_bank();
        cpu.a = 0x1234;
        let bank = cpu.bank_base();
        bus.write_data_u16(bank, 0x5678);
        bus.write_data_u16(bank + 2, 0x9ABC);
        bus.write_data_u16(bank + 4, 0);
        bus.rom[0..2].copy_from_slice(&[0x90, 0x37]); // DIV

        step(&mut cpu, &mut bus).expect("DIV by zero still completes");

        assert!(cpu.cf);
        assert_eq!(cpu.a, 0x1234);
        assert_eq!(bus.read_data_u16(bank), 0x5678);
        assert_eq!(bus.read_data_u16(bank + 2), 0x9ABC);
    }

    #[test]
    fn reset_bit_sets_zero_from_the_previous_bit_value() {
        let (mut cpu, mut bus) = cpu_with_local_bank();
        bus.rom[0..3].copy_from_slice(&[0xC5, 0x19, 0x0C]); // RB IRQH.4
        bus.write_data_u16(crate::bus::SFR_IRQ, 1 << crate::bus::IRQ_ADC);

        step(&mut cpu, &mut bus).expect("RB must execute");

        assert!(!cpu.zf, "the previous bit was one");
        assert_eq!(
            bus.read_data_u16(crate::bus::SFR_IRQ) & (1 << crate::bus::IRQ_ADC),
            0
        );

        cpu.pc = 0;
        step(&mut cpu, &mut bus).expect("RB must execute a second time");
        assert!(cpu.zf, "the previous bit was zero");
    }

    #[test]
    fn move_bit_does_not_modify_zero() {
        let (mut cpu, mut bus) = cpu_with_local_bank();
        cpu.set_psw_u16(1 << 8); // PSWH.0 / MIE
        cpu.zf = true;
        bus.rom[0..2].copy_from_slice(&[0xA2, 0x28]); // MB C, PSWH.0

        step(&mut cpu, &mut bus).expect("MB must execute");

        assert!(cpu.cf);
        assert!(cpu.zf);
    }

    /// L/LB/LC/LCB set ZF from the loaded value (MSM66201 Instruction Manual
    /// pp. 3-69..3-76). The P28 ROM branches straight off a load everywhere
    /// (`LB A, off(135h)` / `JNE` at 0x5C4F decides whether the start-phase
    /// countdown is finished), so a stale ZF keeps sequential injection off.
    #[test]
    fn accumulator_loads_set_zero_from_the_loaded_value() {
        let (mut cpu, mut bus) = cpu_with_local_bank();
        bus.rom[0..2].copy_from_slice(&[0x77, 0x00]); // LB A, #000h
        bus.rom[2..4].copy_from_slice(&[0x77, 0x05]); // LB A, #005h
        bus.rom[4..7].copy_from_slice(&[0x67, 0x00, 0x00]); // L A, #00000h
        bus.rom[7..10].copy_from_slice(&[0x67, 0x55, 0x55]); // L A, #05555h
        bus.rom[10..14].copy_from_slice(&[0x90, 0x9D, 0x20, 0x00]); // LCB A, 00020h
        bus.rom[14..18].copy_from_slice(&[0x90, 0x9D, 0x21, 0x00]); // LCB A, 00021h
        bus.rom[0x20] = 0x00;
        bus.rom[0x21] = 0x7F;

        cpu.pc = 0;
        for (expect_zero, what) in [
            (true, "LB zero"),
            (false, "LB nonzero"),
            (true, "L zero"),
            (false, "L nonzero"),
            (true, "LCB zero"),
            (false, "LCB nonzero"),
        ] {
            cpu.zf = !expect_zero;
            step(&mut cpu, &mut bus).expect("load must execute");
            assert_eq!(cpu.zf, expect_zero, "{what}");
        }
    }

    /// ROL/ROR rotate *through* carry and affect only CF (manual pp.
    /// 3-116..3-123). The ROM builds 32-bit shifts out of them: 0x4FB9 scales
    /// every injector pulse width by 5/4 with `MUL; SRL er1; ROR A; SRL er1;
    /// ROR A`, so a plain rotate leaks bit 0 of the product into bit 15.
    #[test]
    fn rotates_go_through_carry_and_leave_zero_alone() {
        let (mut cpu, mut bus) = cpu_with_local_bank();
        bus.rom[0] = 0x43; // ROR A
        bus.rom[1] = 0x43; // ROR A
        bus.rom[2] = 0x33; // ROL A
        bus.rom[3] = 0x43; // RORB A (DD = 0)

        cpu.dd = true;
        cpu.a = 0x2229;
        cpu.cf = false;
        cpu.zf = true;
        step(&mut cpu, &mut bus).expect("ROR A");
        assert_eq!(cpu.a, 0x1114, "carry-in 0 enters bit 15");
        assert!(cpu.cf, "bit 0 leaves through carry");
        assert!(cpu.zf, "ZF is not affected");

        step(&mut cpu, &mut bus).expect("ROR A");
        assert_eq!(cpu.a, 0x888A, "carry-in 1 enters bit 15");
        assert!(!cpu.cf);

        cpu.cf = true;
        step(&mut cpu, &mut bus).expect("ROL A");
        assert_eq!(cpu.a, 0x1115, "carry-in 1 enters bit 0");
        assert!(cpu.cf, "bit 15 leaves through carry");

        cpu.dd = false;
        cpu.a = 0xAB01;
        cpu.cf = false;
        step(&mut cpu, &mut bus).expect("RORB A");
        assert_eq!(cpu.a, 0xAB00, "byte rotate leaves ACCH alone");
        assert!(cpu.cf);
    }

    /// Bit operands are byte objects regardless of DD (manual p. 3-77: "the
    /// specified bit of the operand obj (byte long)"). In word mode the ROM's
    /// saturating signed add at 0x4F7F tests the sign with `MB C, ACCH.7`;
    /// reading the whole accumulator there tested bit 7 of ACCL instead and
    /// zeroed the injector pulse width whenever a correction's low byte was
    /// 80h or more.
    #[test]
    fn bit_operands_address_one_byte_in_word_mode() {
        let (mut cpu, mut bus) = cpu_with_local_bank();
        bus.rom[0..3].copy_from_slice(&[0xC5, 0x07, 0x2F]); // MB C, ACCH.7
        bus.rom[3..6].copy_from_slice(&[0xC5, 0x07, 0x2F]); // MB C, ACCH.7
        bus.rom[6..9].copy_from_slice(&[0xC5, 0x07, 0x0F]); // RB ACCH.7

        cpu.dd = true;
        cpu.a = 0x00FD;
        cpu.cf = true;
        step(&mut cpu, &mut bus).expect("MB");
        assert!(!cpu.cf, "ACCH.7 of 00FDh is clear");

        cpu.a = 0x8001;
        step(&mut cpu, &mut bus).expect("MB");
        assert!(cpu.cf, "ACCH.7 of 8001h is set");

        cpu.a = 0x80FF;
        step(&mut cpu, &mut bus).expect("RB");
        assert_eq!(cpu.a, 0x00FF, "only ACCH.7 is cleared");
        assert!(!cpu.zf, "the previous bit was one");
    }

    /// MOV/MOVB leave ZF alone (manual p. 3-96 lists no affected flags).
    #[test]
    fn mov_does_not_modify_zero() {
        let (mut cpu, mut bus) = cpu_with_local_bank();
        bus.rom[0..4].copy_from_slice(&[0xC5, 0xEB, 0x98, 0x00]); // MOVB 0EBh, #000h
        cpu.zf = false;

        step(&mut cpu, &mut bus).expect("MOVB must execute");

        assert!(!cpu.zf);
    }

    // ---- flag audit against the manual's instruction list ----------------
    //
    // MSM66201 Instruction Manual, Tables 3-1..3-36 (pp. 3-179..3-212): the
    // ZF/CF/HC/DD columns say which flags each instruction form changes. The
    // tests below pin one flag group each.

    /// Place `code` at 0000h and execute exactly one instruction from it.
    fn exec_one(cpu: &mut Cpu, bus: &mut Bus, code: &[u8]) -> Decoded {
        bus.rom[..code.len()].copy_from_slice(code);
        cpu.pc = 0;
        step(cpu, bus).expect("instruction must execute")
    }

    /// `LCB A, 6C99h[ACC]` (B5 06 AB 99 6C) indexes a code table with the
    /// accumulator: the "RAM 06h" base is the live register, not the unused
    /// RAM byte behind it. The P28 maps a fault index to its code this way.
    #[test]
    fn table_read_indexed_by_the_accumulator_uses_the_live_register() {
        let (mut cpu, mut bus) = cpu_with_local_bank();
        bus.rom[0x6C99] = 0x0B;
        bus.rom[0x6C99 + 0x24] = 0x0F;
        cpu.dd = false;
        cpu.a = 0x0024;

        exec_one(&mut cpu, &mut bus, &[0xB5, 0x06, 0xAB, 0x99, 0x6C]);

        assert_eq!(cpu.a & 0xFF, 0x0F);
    }

    /// SLL/SRL/SRA and their byte forms change CF only (Tables 3-9, 3-10).
    #[test]
    fn shifts_affect_carry_only() {
        let (mut cpu, mut bus) = cpu_with_local_bank();
        for (dd, code, a, want_a, what) in [
            (true, 0x53u8, 0x8000u16, 0x0000u16, "SLL A"),
            (true, 0x63, 0x0001, 0x0000, "SRL A"),
            (true, 0x73, 0x0001, 0x0000, "SRA A"),
            (false, 0x53, 0xAB80, 0xAB00, "SLLB A"),
            (false, 0x63, 0xAB01, 0xAB00, "SRLB A"),
            (false, 0x73, 0xAB01, 0xAB00, "SRAB A"),
        ] {
            cpu.dd = dd;
            cpu.a = a;
            cpu.cf = false;
            cpu.zf = false;
            let d = exec_one(&mut cpu, &mut bus, &[code]);
            assert_eq!(d.mnemonic, what);
            assert_eq!(cpu.a, want_a, "{what}");
            assert!(cpu.cf, "{what}: the shifted-out bit lands in CF");
            assert!(!cpu.zf, "{what}: ZF is not affected by a zero result");
        }

        cpu.dd = true;
        cpu.a = 0x0002;
        cpu.zf = true;
        exec_one(&mut cpu, &mut bus, &[0x63]); // SRL A
        assert_eq!(cpu.a, 0x0001);
        assert!(cpu.zf, "ZF is not affected by a nonzero result either");
    }

    /// `CLR A` and `CLRB A` set ZF to 1; clearing any other object leaves it
    /// alone (Table 3-6, pp. 3-31..3-34).
    #[test]
    fn clearing_the_accumulator_sets_zero() {
        let (mut cpu, mut bus) = cpu_with_local_bank();

        cpu.a = 0x1234;
        cpu.zf = false;
        exec_one(&mut cpu, &mut bus, &[0xF9]); // CLR A
        assert_eq!(cpu.a, 0);
        assert!(cpu.zf, "CLR A");
        assert!(cpu.dd, "CLR A sets DD");

        cpu.a = 0x1234;
        cpu.zf = false;
        exec_one(&mut cpu, &mut bus, &[0xFA]); // CLRB A
        assert_eq!(cpu.a, 0x1200);
        assert!(cpu.zf, "CLRB A");
        assert!(!cpu.dd, "CLRB A resets DD");

        bus.write_data_u16(cpu.bank_base(), 0x5555);
        cpu.zf = false;
        exec_one(&mut cpu, &mut bus, &[0x44, 0x15]); // CLR er0
        assert_eq!(bus.read_data_u16(cpu.bank_base()), 0);
        assert!(!cpu.zf, "CLR er0 affects no flags");
    }

    /// ADD/ADC/SUB/SBC produce the half carry: the carry (borrow) between
    /// bits 3 and 4 (Tables 3-14..3-21; the DAA example on p. 3-50 shows
    /// 87h + 95h leaving H = 0).
    #[test]
    fn add_and_subtract_produce_half_carry() {
        let (mut cpu, mut bus) = cpu_with_local_bank();
        // (code, A, carry in, expected HC, what)
        let byte_cases: [(&[u8], u16, bool, bool, &str); 9] = [
            (&[0x86, 0x01], 0x0F, false, true, "ADDB 0Fh + 01h"),
            (&[0x86, 0x01], 0x10, false, false, "ADDB 10h + 01h"),
            (&[0x86, 0x95], 0x87, false, false, "ADDB 87h + 95h"),
            (&[0x96, 0x00], 0x0F, true, true, "ADCB 0Fh + 00h + C"),
            (&[0x96, 0x00], 0x0F, false, false, "ADCB 0Fh + 00h"),
            (&[0xA6, 0x01], 0x10, false, true, "SUBB 10h - 01h"),
            (&[0xA6, 0x01], 0x1F, false, false, "SUBB 1Fh - 01h"),
            (&[0xB6, 0x00], 0x10, true, true, "SBCB 10h - 00h - C"),
            (&[0xB6, 0x00], 0x10, false, false, "SBCB 10h - 00h"),
        ];
        for (code, a, cin, want_hc, what) in byte_cases {
            cpu.dd = false;
            cpu.a = a;
            cpu.cf = cin;
            cpu.hc = !want_hc;
            exec_one(&mut cpu, &mut bus, code);
            assert_eq!(cpu.hc, want_hc, "{what}");
        }

        let word_cases: [(&[u8], u16, bool, &str); 4] = [
            (&[0x86, 0x01, 0x00], 0x000F, true, "ADD 000Fh + 0001h"),
            (&[0x86, 0x00, 0x01], 0x0F00, false, "ADD 0F00h + 0100h"),
            (&[0xA6, 0x01, 0x00], 0x0010, true, "SUB 0010h - 0001h"),
            (&[0xA6, 0x00, 0x01], 0x1000, false, "SUB 1000h - 0100h"),
        ];
        for (code, a, want_hc, what) in word_cases {
            cpu.dd = true;
            cpu.a = a;
            cpu.cf = false;
            cpu.hc = !want_hc;
            exec_one(&mut cpu, &mut bus, code);
            assert_eq!(cpu.hc, want_hc, "{what}");
        }
    }

    /// CMP/CMPB change ZF and CF but not HC (Tables 3-28, 3-29).
    #[test]
    fn compare_leaves_half_carry_alone() {
        let (mut cpu, mut bus) = cpu_with_local_bank();
        for hc in [true, false] {
            cpu.dd = false;
            cpu.a = 0x10;
            cpu.hc = hc;
            exec_one(&mut cpu, &mut bus, &[0xC6, 0x01]); // CMPB A, #001h
            assert_eq!(cpu.hc, hc);
            assert!(!cpu.cf);
            assert!(!cpu.zf);
        }
    }

    /// INC/DEC change ZF and HC and leave CF alone (Table 3-11).
    #[test]
    fn increment_and_decrement_produce_half_carry_not_carry() {
        let (mut cpu, mut bus) = cpu_with_local_bank();
        let r0 = cpu.bank_base();
        // (code, r0 before, r0 after, expected HC, what)
        for (code, before, after, want_hc, what) in [
            (0xA8u8, 0x0Fu8, 0x10u8, true, "INCB 0Fh"),
            (0xA8, 0x10, 0x11, false, "INCB 10h"),
            (0xA8, 0xFF, 0x00, true, "INCB FFh"),
            (0xB8, 0x10, 0x0F, true, "DECB 10h"),
            (0xB8, 0x11, 0x10, false, "DECB 11h"),
        ] {
            bus.write_data_u8(r0, before);
            cpu.hc = !want_hc;
            cpu.cf = true;
            exec_one(&mut cpu, &mut bus, &[code]);
            assert_eq!(bus.read_data_u8(r0), after, "{what}");
            assert_eq!(cpu.hc, want_hc, "{what}");
            assert_eq!(cpu.zf, after == 0, "{what}");
            assert!(cpu.cf, "{what}: CF is not affected");
        }
    }

    /// "If LRB is the addressing object in any addressing mode, ZF will not
    /// be affected" (INC, p. 3-60).
    #[test]
    fn incrementing_lrb_leaves_zero_alone() {
        let (mut cpu, mut bus) = cpu_with_local_bank();
        cpu.lrb = 0xFFFF;
        cpu.zf = false;
        exec_one(&mut cpu, &mut bus, &[0xFD]); // INC LRB
        assert_eq!(cpu.lrb, 0);
        assert!(!cpu.zf);
    }

    /// DAA/DAS change CF and HC but not ZF, and CF is sticky: the worked
    /// examples on pp. 3-50 and 3-53 both end with C still 1.
    #[test]
    fn decimal_adjust_follows_the_manual_examples() {
        let (mut cpu, mut bus) = cpu_with_local_bank();

        // 87 + 95: ADDB leaves AL = 1Ch, C = 1, H = 0; DAA gives 82h, C = 1,
        // H = 1 (the +6 on the low nibble carries out of bit 3).
        cpu.dd = false;
        cpu.a = 0x87;
        exec_one(&mut cpu, &mut bus, &[0x86, 0x95]); // ADDB A, #095h
        assert_eq!((cpu.a & 0xFF, cpu.cf, cpu.hc), (0x1C, true, false));
        cpu.zf = true;
        exec_one(&mut cpu, &mut bus, &[0x93]); // DAA
        assert_eq!(cpu.a & 0xFF, 0x82);
        assert!(cpu.cf, "the carry into the next digit survives");
        assert!(cpu.hc);
        assert!(cpu.zf, "DAA does not affect ZF");

        // 87 - 95: SUBB leaves AL = F2h, C = 1, H = 0; DAS gives 92h, C = 1,
        // H = 0.
        cpu.a = 0x87;
        exec_one(&mut cpu, &mut bus, &[0xA6, 0x95]); // SUBB A, #095h
        assert_eq!((cpu.a & 0xFF, cpu.cf, cpu.hc), (0xF2, true, false));
        cpu.zf = true;
        exec_one(&mut cpu, &mut bus, &[0x94]); // DAS
        assert_eq!(cpu.a & 0xFF, 0x92);
        assert!(cpu.cf);
        assert!(!cpu.hc);
        assert!(cpu.zf, "DAS does not affect ZF");

        // 15 - 06 = 09: the low-nibble borrow sets H, DAS repairs the digit
        // and always clears H.
        cpu.a = 0x15;
        exec_one(&mut cpu, &mut bus, &[0xA6, 0x06]); // SUBB A, #006h
        assert_eq!((cpu.a & 0xFF, cpu.cf, cpu.hc), (0x0F, false, true));
        exec_one(&mut cpu, &mut bus, &[0x94]); // DAS
        assert_eq!(cpu.a & 0xFF, 0x09);
        assert!(!cpu.cf);
        assert!(!cpu.hc, "DAS: HC <- 0");
    }

    /// `JRNZ DP` counts the *low byte* of DP: "DPL <- DPL - 1; IF DPL != 0
    /// THEN PC <- address" (p. 3-68). DPH is not part of the counter and no
    /// flag changes.
    #[test]
    fn jrnz_counts_the_low_byte_of_dp() {
        let (mut cpu, mut bus) = cpu_with_local_bank();
        let dp = 0x0084; // DP in pointing-register set 0
        cpu.zf = false;

        bus.write_data_u16(dp, 0x0101);
        exec_one(&mut cpu, &mut bus, &[0x30, 0x10]); // JRNZ DP, +10h
        assert_eq!(bus.read_data_u16(dp), 0x0100, "DPH is left alone");
        assert_eq!(cpu.pc, 0x0002, "DPL reached zero: fall through");

        bus.write_data_u16(dp, 0x0200);
        exec_one(&mut cpu, &mut bus, &[0x30, 0x10]);
        assert_eq!(
            bus.read_data_u16(dp),
            0x02FF,
            "DPL wraps without borrowing from DPH"
        );
        assert_eq!(cpu.pc, 0x0012, "DPL is nonzero: branch");
        assert!(!cpu.zf, "JRNZ affects no flags");
    }

    /// Boot the OEM P28 ROM and require it to clear its own power-on CPU
    /// self-test. That routine (0x2226..0x228B) exercises SSP, LRB, RAM and
    /// PSW read-back, and jumps to 0x2292 only if every check passes -- so
    /// reaching it means the executor agrees with the silicon on the parts of
    /// the machine the ROM itself knows how to verify.
    #[test]
    fn oem_rom_passes_its_cpu_self_test() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../P28-230.bin");
        let mut bus = Bus::new();
        if bus.load_rom_file(path).is_err() {
            eprintln!("ROM not present at {path}; skipping");
            return;
        }

        let mut cpu = Cpu::new();
        cpu.pc = bus.read_code_u16(0x0000);
        assert_eq!(cpu.pc, 0x21E2, "reset vector");

        let mut reached_self_test_ok = false;
        let mut ssp_after_init = None;
        let mut reached_peripheral_ok = false;
        for _ in 0..200_000 {
            // 0x2345 writes the "self-test failed" marker and falls into BRK.
            assert_ne!(cpu.pc, 0x2345, "ROM reported self-test failure");
            if cpu.pc == 0x2292 {
                reached_self_test_ok = true;
                ssp_after_init = Some(cpu.ssp);
            }
            // Past the PWM/Port-4 check at 0x2340..0x2343.
            if cpu.pc == 0x234A {
                reached_peripheral_ok = true;
                break;
            }
            let before = cpu.cycles;
            if let Err(e) = step(&mut cpu, &mut bus) {
                panic!("{e} after {} instructions", cpu.instructions);
            }
            bus.tick_timers((cpu.cycles - before) as u32);
        }

        assert!(
            reached_self_test_ok,
            "ROM never reached 0x2292; self-test failed after {} instructions",
            cpu.instructions
        );
        // The ROM sets SSP to 0x047E during reset; the self-test must leave it
        // there rather than the power-on default the old decoder never replaced.
        assert_eq!(ssp_after_init, Some(0x047E), "stack pointer after self-test");
        assert!(
            reached_peripheral_ok,
            "ROM never cleared the PWM / Port-4 check after {} instructions",
            cpu.instructions
        );
    }

    /// Diagnostic, not an assertion: run the ROM for a long time and report
    /// where it spends it. Run with
    /// `cargo test --release boot_trace -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn boot_trace() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../P28-230.bin");
        let mut bus = Bus::new();
        if bus.load_rom_file(path).is_err() {
            eprintln!("ROM not present at {path}; skipping");
            return;
        }
        let mut cpu = Cpu::new();
        cpu.pc = bus.read_code_u16(0x0000);

        let mut hits: std::collections::HashMap<u16, u64> = std::collections::HashMap::new();
        let mut first_reset_at = None;
        let mut brks = 0;
        for i in 0..60_000_000u64 {
            *hits.entry(cpu.pc).or_default() += 1;
            if cpu.pc == 0x21E2 && i > 0 && first_reset_at.is_none() {
                first_reset_at = Some(i);
            }
            if cpu.pc == 0x3828 && brks == 0 {
                let r = |b: &mut Bus, a: u16| b.read_data_u8(a);
                eprintln!(
                    "cfg check i={i}: SSP={:#06X} [0400]={:#06X} PSW={:#06X} LRB={:#06X}",
                    cpu.ssp,
                    bus.read_data_u16(0x400),
                    cpu.psw_u16(),
                    cpu.lrb
                );
                for (name, a, want) in [
                    ("P0IO", 0x21u16, 0xFFu8), ("P1IO", 0x23, 0xFF),
                    ("P2IO", 0x25, 0xFF), ("P2SF", 0x26, 0x07),
                    ("P3IO", 0x29, 0xB1), ("P3SF", 0x2A, 0xFF),
                    ("P4IO", 0x2D, 0x0D), ("P4SF", 0x2E, 0xF4),
                    ("TCON0", 0x40, 0x93), ("TCON1", 0x41, 0x53),
                    ("TCON2", 0x42, 0x92), ("TCON3", 0x43, 0x93),
                ] {
                    let got = r(&mut bus, a);
                    let cmp = if a >= 0x40 { got & 0xF3 } else { got };
                    if cmp != want {
                        eprintln!("  {name} = {got:#04X} (masked {cmp:#04X}), want {want:#04X}");
                    }
                }
            }
            if (cpu.pc == 0x4FCE || cpu.pc == 0x4FD0 || cpu.pc == 0x4FD2) && brks == 0 {
                eprintln!(
                    "{:#06X} dd={} cf={} psw={:#06X}",
                    cpu.pc,
                    cpu.dd,
                    cpu.cf,
                    cpu.psw_u16()
                );
            }
            let pc = cpu.pc;
            let is_brk = bus.read_code_u8(pc) == 0xFF;
            let before = cpu.cycles;
            if let Err(e) = step(&mut cpu, &mut bus) {
                eprintln!("STOP: {e} after {} instructions", cpu.instructions);
                break;
            }
            if is_brk && brks < 5 {
                brks += 1;
                eprintln!(
                    "BRK at {pc:#06X} i={i} fault code 0EBh = {:#04X}",
                    bus.read_data_u8(0xEB)
                );
            }
            bus.tick_timers((cpu.cycles - before) as u32);
        }
        let mut top: Vec<_> = hits.iter().map(|(k, v)| (*v, *k)).collect();
        top.sort_unstable_by(|a, b| b.cmp(a));
        eprintln!(
            "distinct PCs: {}  restarted at: {:?}  main-loop hits: {}  instructions: {}",
            hits.len(),
            first_reset_at,
            hits.get(&0x0569).copied().unwrap_or(0),
            cpu.instructions
        );
        eprintln!("hottest PCs:");
        for (n, pc) in top.iter().take(20) {
            eprintln!("  {pc:#06X}  {n}");
        }
        let mut visited: Vec<u16> = hits.keys().copied().collect();
        visited.sort_unstable();
        eprintln!("highest PC reached: {:#06X}", visited.last().unwrap());
    }
}
