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

use crate::bus::Bus;
use crate::cpu::Cpu;
use crate::decoder::{decode, Decoded};
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
            Mem::IdxMemN8 => {
                let base = self.bus.read_data_u16(f.n8 as u16);
                base.wrapping_add(f.n16)
            }
            Mem::IdxMemOff => {
                let a = self.cpu.off_page(f.n8);
                let base = self.bus.read_data_u16(a);
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
            "CLR" => self.write(&args[0], byte, 0),

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
                let (res, carry) = match base {
                    "ADD" | "ADC" => {
                        let r = (a & width_mask) + (b & width_mask) + carry_in;
                        (r & width_mask, r > width_mask)
                    }
                    _ => {
                        let r = (a & width_mask)
                            .wrapping_sub(b & width_mask)
                            .wrapping_sub(carry_in);
                        ((r & width_mask), (a & width_mask) < (b & width_mask) + carry_in)
                    }
                };
                self.cpu.cf = carry;
                self.set_zf(res as u16, byte);
                if !matches!(base, "CMP" | "CMPC") {
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
                self.set_zf(res, byte);
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
                self.cpu.cf = carry;
                // The rotates affect CF only.
                if !matches!(base, "ROL" | "ROR") {
                    self.set_zf(res as u16, byte);
                }
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
                // Decrement the named register and branch while it is non-zero.
                let v = self.read(&args[0], false, false).wrapping_sub(1);
                self.write(&args[0], false, v);
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
                // Decimal adjust after add/subtract, on the low byte.
                let mut v = (self.cpu.a & 0xFF) as u16;
                let adjust = if (v & 0x0F) > 9 || self.cpu.hc { 6 } else { 0 };
                v = if base == "DAA" {
                    v.wrapping_add(adjust)
                } else {
                    v.wrapping_sub(adjust)
                };
                let adjust_hi = if (v >> 4) > 9 || self.cpu.cf { 0x60 } else { 0 };
                v = if base == "DAA" {
                    v.wrapping_add(adjust_hi)
                } else {
                    v.wrapping_sub(adjust_hi)
                };
                self.cpu.cf = v > 0xFF;
                self.cpu.a = (self.cpu.a & 0xFF00) | (v & 0xFF);
                self.set_zf(v, true);
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
