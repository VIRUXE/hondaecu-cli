// OKI MSM66207 table-driven instruction decoder.
//
// The 66207 encodes the *addressing mode* in the first byte and the *operation*
// in a later byte (e.g. `C5 EB 98 46` is `MOVB 0ebh, #046h`, where C5 selects the
// direct-byte operand and 98 selects MOV-immediate). A flat match on the opcode
// byte therefore cannot decode this ISA -- patterns have to be matched whole.
//
// Patterns come from FULL_OPCODES (full_decoder.rs), generated from the
// authoritative 66207.op spec.
//
// The DD flag (word/byte mode) is part of the decode context: some encodings are
// shared between a word form and a byte form and are told apart only by DD --
// 0x18 is `ADC A, er0` when DD=1 but `ADCB A, r0` when DD=0. Instructions marked
// 'S'/'R' in the table set/reset DD for the instructions that follow.

use std::sync::OnceLock;

use crate::full_decoder::FULL_OPCODES;
use crate::operand::takes_pointer_operand;

/// Longest encoding in the table, used to bound the fetch window.
pub const MAX_INSN_LEN: usize = 6;

/// Immediate/displacement values captured while matching a pattern.
#[derive(Debug, Clone, Copy, Default)]
pub struct Fields {
    pub n8: u8,
    pub n16: u16,
    pub s8: i8,
    pub rel8: i8,
    pub addr16: u16,
    /// Second immediate, for forms carrying two (`N'8`, `N'16`).
    pub n8_alt: u8,
    pub n16_alt: u16,
}

#[derive(Debug, Clone)]
pub struct Decoded {
    /// Index into FULL_OPCODES.
    pub index: usize,
    pub mnemonic: &'static str,
    pub len: usize,
    pub fields: Fields,
    /// DD after this instruction, when it forces a mode; None leaves it unchanged.
    pub dd_after: Option<bool>,
    /// INT (internal-memory) machine cycles for this encoding, per `int_cycles`.
    /// For conditional branches this is the *not-taken* cost; `exec` adds the
    /// taken penalty at run time.
    pub cycles: u16,
    /// What the instruction costs on top of `cycles` when its operands or the
    /// system stack are in external data memory, per `ext_cycles`.
    pub ext: ExtCycles,
}

/// Where a memory operand lives. `exec` turns it into an address to decide
/// whether the operand is in external data memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// `rN` / `erN`: the local register bank.
    Bank,
    /// `off N8` / `off N'8`: the LRB-paged byte or word.
    OffPage,
    OffPageAlt,
    /// `[DP]`
    AtDp,
    /// `S8[USP]`
    UspIdx,
    /// `N16[X1]` / `N16[X2]`, and the `N'16` spellings that take the
    /// displacement from the instruction's first 16-bit field.
    X1Idx,
    X2Idx,
    X1IdxAlt,
    X2IdxAlt,
}

/// The manual's EXT columns as additions to the INT count.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExtCycles {
    /// For the first and second operand: where it lives, and what the
    /// instruction costs extra when that place is external.
    pub operands: [Option<(Place, u16)>; 2],
    /// Extra cost when the system stack is external.
    pub stack: u16,
}

/// EXT surcharges of one instruction, from the `Ext` columns of the manual's
/// Instruction List and the per-instruction pages.
///
/// The columns are additive: each place that can be external has its own
/// cost, and a row with two of them external costs the sum. With `w` the
/// operand width in bytes, a place costs
///
/// * `2w` when the instruction only reads it or only stores into it: loads,
///   `ST`, anything into the accumulator or a CPU register, a register or
///   `N8` moved into memory, tests, and pointers fetched from memory;
/// * `4w` when the instruction is encoded with the object prefix and writes
///   the object: `MOV obj, A`, `MOV obj, #imm`, `CLR`, exchanges, shifts,
///   increments and `<alu> obj, A` -- compares included, though they write
///   nothing;
/// * `4w + 1` for `<alu> obj, #imm`;
/// * 6 for a bit written into a byte (`SB`, `RB`, `SBR`, `RBR`, `MB obj, C`);
/// * one more when the place is `off N8`.
///
/// `<alu> obj, off N8` splits the other way round in the manual -- `2w` for
/// the object, `4w + 1` for the `off N8` source -- and is encoded as printed.
/// An external system stack costs four cycles per word pushed or popped.
///
/// Two cells are not taken from the page: `ADDB A, [DP]` prints a dash
/// (p. 3-16), and `MOVB off N8, A` is 11 in Table 3-4 but 13 on page 3-95,
/// the value of the row beneath it; 11 is used.
fn ext_cycles(mnemonic: &str, len: usize) -> ExtCycles {
    let (op, operands) = mnemonic.split_once(' ').unwrap_or((mnemonic, ""));
    let mut args = operands.split(',').map(str::trim);
    let first = without_bit(args.next().unwrap_or(""));
    let second = without_bit(args.next().unwrap_or(""));

    let pointer = takes_pointer_operand(op);
    let place = |arg: &str| if pointer { pointer_place(arg) } else { data_place(arg) };
    let (p1, p2) = (place(first), place(second));
    // Width in bytes of the data the operation moves.
    let w: u16 = if is_byte_operation(op) { 1 } else { 2 };
    let immediate = second.starts_with('#');

    // (cost of the first place, cost of the second place, cost of the stack),
    // before the `off N8` cycle.
    let (c1, c2, stack) = match op {
        "L" | "LB" | "ST" | "STB" => (0, 2 * w, 0),
        "MOV" | "MOVB" => {
            // `MOVB rN, #N8` has a short two-byte form that only stores.
            let short_store = immediate && len == 2;
            let object_prefix = (second == "A" || immediate) && !short_store;
            (if object_prefix { 4 * w } else { 2 * w }, 2 * w, 0)
        }
        "CLR" | "CLRB" => (4 * w, 0, 0),
        "XCHG" | "XCHGB" | "XNBL" => (0, 4 * w, 0),
        "ROL" | "ROLB" | "ROR" | "RORB" | "SLL" | "SLLB" | "SRL" | "SRLB" | "SRA" | "SRAB"
        | "INC" | "INCB" | "DEC" | "DECB" => (4 * w, 0, 0),
        "ADD" | "ADDB" | "ADC" | "ADCB" | "SUB" | "SUBB" | "SBC" | "SBCB" | "AND" | "ANDB"
        | "OR" | "ORB" | "XOR" | "XORB" | "CMP" | "CMPB" => {
            if first == "A" {
                (0, 2 * w, 0)
            } else if immediate {
                (4 * w + 1, 0, 0)
            } else if second == "A" {
                (4 * w, 0, 0)
            } else {
                // `obj, off N8`; the `off N8` cycle is added below.
                (2 * w, 4 * w, 0)
            }
        }
        "SB" | "RB" | "SBR" | "RBR" => (6, 0, 0),
        "TBR" | "JBS" | "JBR" => (2, 0, 0),
        "MB" | "MBR" => {
            if first == "C" {
                (0, 2, 0)
            } else {
                (6, 0, 0)
            }
        }
        // The pointer is a word wherever it leads.
        "J" => (4, 0, 0),
        "CAL" => (4, 0, 4),
        "LC" | "CMPC" => (0, 4, 0),
        "LCB" | "CMPCB" => (0, if p2 == Some(Place::Bank) { 2 } else { 4 }, 0),
        "SCAL" | "VCAL" | "RT" | "PUSHS" | "POPS" => (0, 0, 4),
        "RTI" => (0, 0, 16),
        _ => (0, 0, 0),
    };

    // MUL and DIV name no operands; theirs are fixed slots of the register
    // bank (er0/er1/er2, or r0/r1 for the byte forms).
    let implicit_bank = match op {
        "MUL" => 8,
        "MULB" => 2,
        "DIV" => 16,
        "DIVB" => 4,
        _ => 0,
    };
    if implicit_bank != 0 {
        return ExtCycles {
            operands: [Some((Place::Bank, implicit_bank)), None],
            stack: 0,
        };
    }

    let charge = |place: Option<Place>, cycles: u16| match place {
        Some(p) if cycles != 0 => {
            let off = matches!(p, Place::OffPage | Place::OffPageAlt) as u16;
            Some((p, cycles + off))
        }
        _ => None,
    };
    ExtCycles {
        operands: [charge(p1, c1), charge(p2, c2)],
        stack,
    }
}

/// Byte-wide operations: the `B` forms, and everything that works on one bit.
fn is_byte_operation(op: &str) -> bool {
    matches!(
        op,
        "LB" | "STB" | "MOVB" | "CLRB" | "XCHGB" | "XNBL" | "ROLB" | "RORB" | "SLLB" | "SRLB"
            | "SRAB" | "INCB" | "DECB" | "ADDB" | "ADCB" | "SUBB" | "SBCB" | "ANDB" | "ORB"
            | "XORB" | "CMPB"
    )
}

/// Where a data operand lives, if it is in memory that can be external.
/// CPU registers, immediates and `N8` (always on-chip) are not.
fn data_place(arg: &str) -> Option<Place> {
    Some(match arg {
        "off N8" => Place::OffPage,
        "off N'8" => Place::OffPageAlt,
        "[DP]" => Place::AtDp,
        "S8[USP]" => Place::UspIdx,
        "N16[X1]" => Place::X1Idx,
        "N16[X2]" => Place::X2Idx,
        "N'16[X1]" => Place::X1IdxAlt,
        "N'16[X2]" => Place::X2IdxAlt,
        bank if is_bank_register(bank) => Place::Bank,
        _ => return None,
    })
}

/// Where the pointer of a jump, call or ROM-table read is fetched from.
/// `[DP]`, `[X1]` and `N16[X1]` use the register itself and touch no memory;
/// one more bracket layer, or a base held in `off N8`, does.
fn pointer_place(arg: &str) -> Option<Place> {
    match arg.strip_prefix('[').and_then(|a| a.strip_suffix(']')) {
        Some(inner) => data_place(inner),
        None => (arg == "N16[off N8]").then_some(Place::OffPage),
    }
}

/// `rN` / `erN`: a slot of the local register bank.
fn is_bank_register(arg: &str) -> bool {
    let digit = arg.strip_prefix("er").or_else(|| arg.strip_prefix('r'));
    digit.is_some_and(|d| d.len() == 1 && d.as_bytes()[0].is_ascii_digit())
}

/// Drop a bit selector: `[DP].3` addresses the same byte as `[DP]`.
fn without_bit(arg: &str) -> &str {
    match arg.rsplit_once('.') {
        Some((base, bit)) if bit.len() == 1 => base,
        _ => arg,
    }
}

/// INT (internal-memory) machine-cycle count for one instruction, from the
/// MSM66201/66207 instruction manual's "Instruction List" cycle tables
/// (chapter 3 §3, the `Int*1 Int*2` column).
///
/// The count has three parts, and the tables are regular enough that they
/// reproduce every row:
///
/// 1. **Two cycles per instruction byte.**
/// 2. **The addressing mode of the memory operand** (`mode_cycles`): `[DP]`
///    costs two more and `S8[USP]` one more; register, direct, `off N8` and
///    `N16[X1]` operands cost nothing extra.
/// 3. **The operation** (`operation_cycles`): nothing for plain moves, one for
///    exchanges, increments and memory shifts, more for the multi-cycle
///    operations, ROM-table reads, bit writes and control transfers. For the
///    arithmetic, logic and compare group it depends on the operand
///    combination, not on the operation.
///
/// A *taken* conditional branch adds four more cycles, which `exec` applies at
/// run time since only it knows the branch was taken. The unit tests carry the
/// manual's value for each addressing variant of each operation class.
///
/// These are the manual's `Int` columns: every data operand in on-chip memory.
/// Where the program is fetched from does not matter (the ROM-table pages give
/// the same count for internal and external ROM). Operands in *external data*
/// memory cost more; `ext_cycles` holds the difference.
///
/// `PUSHU` and `SMOVI` are in the opcode table but not in the manual's list.
/// `PUSHU` is charged like `PUSHS`, `SMOVI` two cycles per byte.
fn int_cycles(mnemonic: &str, len: usize) -> u16 {
    let (op, operands) = mnemonic.split_once(' ').unwrap_or((mnemonic, ""));
    let mut args = operands.split(',').map(str::trim);
    let first = args.next().unwrap_or("");
    let second = args.next().unwrap_or("");

    2 * len as u16 + mode_cycles(op, first, second) + operation_cycles(op, first, second, len)
}

/// Extra cycles for reaching the memory operand.
fn mode_cycles(op: &str, first: &str, second: &str) -> u16 {
    let pointer = takes_pointer_operand(op);
    [first, second]
        .into_iter()
        .map(|arg| {
            let arg = without_bit(arg);
            let arg = match arg.strip_prefix('[').and_then(|a| a.strip_suffix(']')) {
                Some(inner) if pointer => inner,
                _ => arg,
            };
            match arg {
                "[DP]" => 2,
                "S8[USP]" => 1,
                _ => 0,
            }
        })
        .sum()
}

/// Extra cycles for the operation itself.
fn operation_cycles(op: &str, first: &str, second: &str, len: usize) -> u16 {
    match op {
        // Multiply / divide (manual pp. 3-100, 3-101, 3-57, 3-58).
        "MUL" => 23,
        "MULB" => 15,
        "DIV" => 43,
        "DIVB" => 25,
        // Interrupt / subroutine return and software trap.
        "RTI" => 13,
        "BRK" => 11,
        "RT" => 5,
        "VCAL" => 9,
        // Decimal adjust.
        "DAA" | "DAS" => 4,
        // ROM (code-space) reads. A byte read is two cycles cheaper than a
        // word read, except through a memory-held index (`N16[off N8]`,
        // `N16[N8]`), where the manual gives 17 for both widths (p. 3-76).
        "LC" | "CMPC" => 7,
        "LCB" | "CMPCB" => {
            if matches!(second, "N16[off N8]" | "N16[N8]") {
                7
            } else {
                5
            }
        }
        // Short call / short jump and the stack ops.
        "SCAL" => 5,
        "SJ" => 4,
        "PUSHS" | "PUSHU" => 1,
        "POPS" => 2,
        // Bit set / reset rewrite the byte. Testing one (TBR) costs nothing
        // beyond reading it.
        "SB" | "RB" | "SBR" | "RBR" => 3,
        // Move-bit: reading a bit into carry is cheap; writing one back is not.
        "MB" | "MBR" => {
            if first == "C" {
                1
            } else {
                6
            }
        }
        // Decrement-and-branch carries a base surcharge even when not taken.
        "JRNZ" => 3,
        // Call / jump: indirect forms cost more than the absolute form.
        "CAL" => {
            if first.starts_with('[') {
                4
            } else {
                3
            }
        }
        "J" => {
            if first.starts_with('[') {
                2
            } else {
                1
            }
        }
        "XCHG" | "XCHGB" | "XNBL" => 1,
        "INC" | "INCB" | "DEC" | "DECB" => 1,
        // Shifting the accumulator is a plain one-byte instruction; shifting
        // anything else costs one more.
        "ROL" | "ROLB" | "ROR" | "RORB" | "SLL" | "SLLB" | "SRL" | "SRLB" | "SRA" | "SRAB" => {
            if first == "A" {
                0
            } else {
                1
            }
        }
        // Arithmetic, logic and compare all share one pattern:
        //   A, <register bank>  one-byte form        +1
        //   A, <anything else>                        0
        //   <obj>, A   and   <obj>, off N8           +1
        //   <obj>, #immediate                         0
        "ADD" | "ADDB" | "ADC" | "ADCB" | "SUB" | "SUBB" | "SBC" | "SBCB" | "AND" | "ANDB"
        | "OR" | "ORB" | "XOR" | "XORB" | "CMP" | "CMPB" => {
            if first == "A" {
                (len == 1) as u16
            } else {
                matches!(second, "A" | "off N8") as u16
            }
        }
        // Moves, loads, stores, clears, SWAP/EXTND/NOP/SC/RC, conditional
        // branches: two cycles per byte plus the addressing mode.
        _ => 0,
    }
}

fn hex_byte(tok: &str) -> Option<u8> {
    let b = tok.as_bytes();
    if b.len() == 2 && b.iter().all(|c| c.is_ascii_hexdigit()) {
        u8::from_str_radix(tok, 16).ok()
    } else {
        None
    }
}

/// Number of fixed (non-wildcard) bytes in a pattern -- how specific it is.
fn specificity(pat: &[&str]) -> usize {
    pat.iter().filter(|t| hex_byte(t).is_some()).count()
}

struct Index {
    /// Pattern indices bucketed by leading opcode byte, most specific first.
    by_first: Vec<Vec<u32>>,
    /// `int_cycles` of every FULL_OPCODES entry, worked out once: it parses the
    /// operand text, which is too slow to repeat for each executed instruction.
    cycles: Vec<u16>,
    /// `ext_cycles` of every entry.
    ext: Vec<ExtCycles>,
}

fn index() -> &'static Index {
    static INDEX: OnceLock<Index> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut by_first: Vec<Vec<u32>> = vec![Vec::new(); 256];
        for (i, p) in FULL_OPCODES.iter().enumerate() {
            if let Some(first) = p.bytes_pat.first().and_then(|t| hex_byte(t)) {
                by_first[first as usize].push(i as u32);
            }
        }
        // Match the most constrained pattern first so a shorter, less specific
        // encoding can never shadow a longer one that also fits.
        for bucket in by_first.iter_mut() {
            bucket.sort_by(|&a, &b| {
                let (pa, pb) = (&FULL_OPCODES[a as usize], &FULL_OPCODES[b as usize]);
                specificity(pb.bytes_pat)
                    .cmp(&specificity(pa.bytes_pat))
                    .then(pb.bytes_pat.len().cmp(&pa.bytes_pat.len()))
            });
        }
        let cycles = FULL_OPCODES
            .iter()
            .map(|p| int_cycles(p.mnemonic, p.bytes_pat.len()))
            .collect();
        let ext = FULL_OPCODES
            .iter()
            .map(|p| ext_cycles(p.mnemonic, p.bytes_pat.len()))
            .collect();
        Index {
            by_first,
            cycles,
            ext,
        }
    })
}

/// Decode one instruction. `fetch(i)` supplies the byte at offset `i` from the
/// instruction start. `dd` is the current word/byte mode.
pub fn decode(dd: bool, fetch: impl Fn(usize) -> u8) -> Option<Decoded> {
    let first = fetch(0);
    let idx = index();

    'pattern: for &pi in &idx.by_first[first as usize] {
        let p = &FULL_OPCODES[pi as usize];

        // DD gate: '1' and '0' forms exist only in their respective mode.
        match p.dd_mode {
            '1' if !dd => continue,
            '0' if dd => continue,
            _ => {}
        }

        let mut f = Fields::default();
        for (off, tok) in p.bytes_pat.iter().enumerate() {
            let b = fetch(off);
            if let Some(expect) = hex_byte(tok) {
                if b != expect {
                    continue 'pattern;
                }
                continue;
            }
            match *tok {
                "N8" => f.n8 = b,
                "NL" => f.n16 = (f.n16 & 0xFF00) | b as u16,
                "NH" => f.n16 = (f.n16 & 0x00FF) | ((b as u16) << 8),
                "S8" => f.s8 = b as i8,
                "rel8" => f.rel8 = b as i8,
                "addrl" => f.addr16 = (f.addr16 & 0xFF00) | b as u16,
                "addrh" => f.addr16 = (f.addr16 & 0x00FF) | ((b as u16) << 8),
                "N'8" => f.n8_alt = b,
                "N'L" => f.n16_alt = (f.n16_alt & 0xFF00) | b as u16,
                "N'H" => f.n16_alt = (f.n16_alt & 0x00FF) | ((b as u16) << 8),
                // Unknown placeholder: treat as a wildcard rather than
                // silently mis-decoding.
                _ => {}
            }
        }

        return Some(Decoded {
            index: pi as usize,
            mnemonic: p.mnemonic,
            len: p.bytes_pat.len(),
            fields: f,
            dd_after: match p.dd_mode {
                'S' => Some(true),
                'R' => Some(false),
                _ => None,
            },
            cycles: idx.cycles[pi as usize],
            ext: idx.ext[pi as usize],
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// The P28 reset path. The old flat `match opcode` decoder read the leading
    /// C5 as a 2-byte `JLE rel8` and diverged on the very first instruction, so
    /// `MOV SSP, #047Eh` never ran and the stack stayed at its power-on default
    /// and then ran away. Pin the whole sequence.
    #[test]
    fn decodes_reset_sequence() {
        // int_start @ 0x21E2, DD=0 out of reset.
        let code: &[u8] = &[
            0xC5, 0xEB, 0x98, 0x46, // MOVB 0EBh, #046h
            0xC5, 0x9F, 0x09, // RB 09Fh.1
            0xC5, 0x11, 0x98, 0x3C, // MOVB WDT, #03Ch
            0xA0, 0x98, 0x7E, 0x04, // MOV SSP, #047Eh
        ];
        let expect = [
            ("MOVB", 4, "N'8, #N8"),
            ("RB", 3, "N8.1"),
            ("MOVB", 4, "N'8, #N8"),
            ("MOV", 4, "SSP, #N16"),
        ];

        let mut pc = 0usize;
        let mut dd = false;
        for (mnem, len, ops) in expect {
            let d = decode(dd, |i| code.get(pc + i).copied().unwrap_or(0))
                .unwrap_or_else(|| panic!("no decode at offset {pc}"));
            assert_eq!(d.mnemonic, format!("{mnem} {ops}"), "at offset {pc}");
            assert_eq!(d.len, len, "length at offset {pc}");
            if let Some(v) = d.dd_after {
                dd = v;
            }
            pc += d.len;
        }

        // The one that matters: SSP is loaded with 0x047E, not left at 0x07FE.
        let d = decode(dd, |i| code[11 + i]).unwrap();
        assert_eq!(d.fields.n16, 0x047E);
    }

    /// `E7 N8` is the short `off N8` form of OR, like 87/97/A7/B7/C7/D7/F7 for
    /// the other seven accumulator operations: a word OR when DD is set, a
    /// byte OR when it is clear. 66207.op lists it for the byte form only.
    #[test]
    fn e7_is_a_word_or_when_dd_is_set() {
        let code = [0xE7u8, 0x14];
        let fetch = |i: usize| code.get(i).copied().unwrap_or(0);

        let word = decode(true, fetch).expect("E7 decodes in word mode");
        assert_eq!((word.mnemonic, word.len, word.cycles), ("OR A, off N8", 2, 4));

        let byte = decode(false, fetch).expect("E7 decodes in byte mode");
        assert_eq!((byte.mnemonic, byte.len, byte.cycles), ("ORB A, off N8", 2, 4));
    }

    /// `XOR A, erN` is commented out of 66207.op because the manual prints
    /// OR's code for it. It is the object prefix `44+N` followed by `F2`,
    /// like `XORB A, rN` (`20+N, F2`) and every other `XOR A, obj`.
    #[test]
    fn xor_with_a_register_bank_word_decodes() {
        let code = [0x45u8, 0xF2];
        let d = decode(true, |i| code.get(i).copied().unwrap_or(0)).expect("44+N F2 decodes");
        assert_eq!((d.mnemonic, d.len, d.cycles), ("XOR A, er1", 2, 4));
    }

    /// Decode every instruction dasm662 found in the P28-230 OEM ROM and compare
    /// mnemonic and length. The fixture carries a 6-byte window per instruction
    /// so this test needs no ROM image.
    #[test]
    fn matches_dasm662_ground_truth() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/p28_dasm662.tsv");
        let text = std::fs::read_to_string(path).expect("fixture");

        let (mut total, mut ok_len, mut ok_mnem) = (0usize, 0usize, 0usize);
        let mut failures: BTreeMap<String, (usize, String)> = BTreeMap::new();

        for line in text.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let f: Vec<&str> = line.split('\t').collect();
            assert!(f.len() >= 5, "malformed fixture row: {line}");
            let addr = u16::from_str_radix(f[0], 16).unwrap();
            let dd = f[1] == "1";
            let want_len: usize = f[2].parse().unwrap();
            let want_mnem = f[3];
            let win: Vec<u8> = (0..f[4].len() / 2)
                .map(|i| u8::from_str_radix(&f[4][i * 2..i * 2 + 2], 16).unwrap())
                .collect();

            total += 1;
            let got = decode(dd, |i| win.get(i).copied().unwrap_or(0));

            let (got_mnem, got_len) = match &got {
                Some(d) => (d.mnemonic.split([' ', ',']).next().unwrap(), d.len),
                None => ("<none>", 0),
            };
            if got_len == want_len {
                ok_len += 1;
            }
            // dasm662 prints this one as "TRB"; 66207.op (and the assembler)
            // call it "TBR". Same encoding, same length -- spelling only.
            let want_mnem = if want_mnem == "TRB" { "TBR" } else { want_mnem };
            // dasm662 is built from the same 66207.op, which has E7 as a
            // byte OR in either DD state. In word mode it is `OR A, off N8`
            // (see `e7_is_a_word_or_when_dd_is_set`); the ROM has one, at 5FDD.
            let want_mnem = if want_mnem == "ORB" && dd && win.first() == Some(&0xE7) {
                "OR"
            } else {
                want_mnem
            };

            if got_mnem.eq_ignore_ascii_case(want_mnem) {
                ok_mnem += 1;
            } else {
                let key = format!("{want_mnem} -> {got_mnem}");
                let e = failures.entry(key).or_insert((0, String::new()));
                e.0 += 1;
                if e.1.is_empty() {
                    e.1 = format!("{addr:04X} dd={} {}", f[1], f[4]);
                }
            }
        }

        eprintln!("decoded {total} instructions: {ok_mnem} mnemonic, {ok_len} length");
        for (k, (n, ex)) in &failures {
            eprintln!("  {n:5}x  {k}   e.g. {ex}");
        }
        assert_eq!(ok_mnem, total, "mnemonic mismatches vs dasm662");
        assert_eq!(ok_len, total, "length mismatches vs dasm662");
    }
}

#[cfg(test)]
mod cycle_tests {
    use super::{ext_cycles, int_cycles, ExtCycles};

    /// Spot-check the INT machine-cycle model against exact values read from the
    /// MSM66201/66207 instruction manual's Instruction List (chapter 3 §3). Each
    /// row is (mnemonic, byte length, manual INT cycles).
    #[test]
    fn int_cycles_match_the_manual() {
        let cases = [
            // Cheap data moves / arithmetic: two cycles per byte.
            ("MOVB rN, A", 2, 4),
            ("MOV erN, N16[X1]", 4, 8),
            ("MOV off N8, #N16", 5, 10),
            ("ADD A, off N8", 2, 4),
            ("CMP A, N8", 3, 6),
            ("CLR A", 1, 2),
            ("NOP", 1, 2),
            // Rotates / shifts.
            ("ROL A", 1, 2),
            ("ROL off N8", 3, 7),
            // Multiply / divide.
            ("MUL", 2, 27),
            ("MULB", 2, 19),
            ("DIV", 2, 47),
            ("DIVB", 2, 29),
            // ROM-table reads.
            ("LC A, N16", 4, 15),
            ("LCB A, [er0]", 2, 9),
            ("CMPC A, [er0]", 2, 11),
            // Control transfer.
            ("SJ rel8", 2, 8),
            ("J addr16", 3, 7),
            ("J [er0]", 2, 6),
            ("CAL addr16", 3, 9),
            ("CAL [er0]", 2, 8),
            ("SCAL rel8", 2, 9),
            ("VCAL addr16", 1, 11),
            ("RT", 1, 7),
            ("RTI", 1, 15),
            ("BRK", 1, 13),
            // Conditional branches: not-taken cost (exec adds +4 when taken).
            ("JEQ rel8", 2, 4),
            ("JBS off N8.0, rel8", 3, 6),
            ("JRNZ DP, rel8", 2, 7),
            // Bit / stack.
            ("RB off N8.1", 2, 7),
            ("MB C, N8.0", 2, 5),
            ("PUSHS A", 1, 3),
            ("POPS A", 1, 4),
            ("DAA", 1, 6),
        ];
        for (mnem, len, want) in cases {
            assert_eq!(int_cycles(mnem, len), want, "cycles for `{mnem}`");
        }
    }

    /// INT cycles the decoder attaches to the table entry spelled `mnemonic`.
    /// Going through FULL_OPCODES keeps the byte length honest and fails loudly
    /// on a spelling the table does not contain.
    fn table_cycles(mnemonic: &str) -> u16 {
        let p = crate::full_decoder::FULL_OPCODES
            .iter()
            .find(|p| p.mnemonic == mnemonic)
            .unwrap_or_else(|| panic!("`{mnemonic}` is not in FULL_OPCODES"));
        int_cycles(p.mnemonic, p.bytes_pat.len())
    }

    fn assert_manual(table: &str, rows: &[(&str, u16)]) {
        let wrong: Vec<String> = rows
            .iter()
            .filter(|(m, want)| table_cycles(m) != *want)
            .map(|(m, want)| format!("`{m}`: model {}, manual {want}", table_cycles(m)))
            .collect();
        assert!(wrong.is_empty(), "{table}:\n  {}", wrong.join("\n  "));
    }

    // The rows below are the `Int*1 Int*2` column of the instruction manual's
    // Instruction List (Tables 3-1..3-37, manual pp. 3-179..3-213), one row per
    // addressing variant. `[DP]` costs two cycles and `S8[USP]` one on top of
    // two cycles per byte; an indexed `N16[X1]` operand costs nothing extra.

    // ---- EXT columns: operands in external data memory --------------------

    fn table_ext(mnemonic: &str) -> ExtCycles {
        let p = crate::full_decoder::FULL_OPCODES
            .iter()
            .find(|p| p.mnemonic == mnemonic)
            .unwrap_or_else(|| panic!("`{mnemonic}` is not in FULL_OPCODES"));
        ext_cycles(p.mnemonic, p.bytes_pat.len())
    }

    /// Rows whose only variable is one memory operand (or the stack): the
    /// manual's EXT count with that operand in external memory.
    fn assert_manual_ext(table: &str, rows: &[(&str, u16)]) {
        let wrong: Vec<String> = rows
            .iter()
            .filter_map(|(m, want)| {
                let ext = table_ext(m);
                let charged = ext.operands.iter().flatten().count() + (ext.stack != 0) as usize;
                let got = table_cycles(m)
                    + ext.operands.iter().flatten().map(|(_, c)| c).sum::<u16>()
                    + ext.stack;
                (charged != 1 || got != *want)
                    .then(|| format!("`{m}`: model {got} ({charged} charged), manual {want}"))
            })
            .collect();
        assert!(wrong.is_empty(), "{table}:\n  {}", wrong.join("\n  "));
    }

    /// Rows with two places that can be external. The columns are the
    /// manual's: second external, first external, both. `None` is a dash. For
    /// the calls the manual's second "operand" is the system stack.
    fn assert_manual_ext_pairs(table: &str, rows: &[(&str, [Option<u16>; 3])]) {
        let wrong: Vec<String> = rows
            .iter()
            .filter_map(|(m, want)| {
                let ext = table_ext(m);
                let int = table_cycles(m);
                let first = ext.operands[0].map_or(0, |(_, c)| c);
                let second = ext.operands[1].map_or(ext.stack, |(_, c)| c);
                let got = [int + second, int + first, int + first + second];
                let bad = want.iter().zip(got).any(|(w, g)| w.is_some_and(|w| w != g));
                bad.then(|| format!("`{m}`: model {got:?}, manual {want:?}"))
            })
            .collect();
        assert!(wrong.is_empty(), "{table}:\n  {}", wrong.join("\n  "));
    }

    #[test]
    fn external_reads_and_plain_stores_cost_two_cycles_per_byte() {
        assert_manual_ext(
            "EXT, loads, stores and reads",
            &[
                ("L A, er0", 6),
                ("L A, off N8", 9),
                ("L A, [DP]", 8),
                ("L A, S8[USP]", 9),
                ("L A, N16[X1]", 10),
                ("LB A, r0", 4),
                ("LB A, off N8", 7),
                ("LB A, [DP]", 6),
                ("LB A, S8[USP]", 7),
                ("LB A, N16[X1]", 8),
                ("ST A, er0", 6),
                ("ST A, off N8", 9),
                ("ST A, [DP]", 8),
                ("ST A, S8[USP]", 9),
                ("ST A, N16[X1]", 10),
                ("STB A, r0", 4),
                ("STB A, off N8", 7),
                ("STB A, [DP]", 6),
                ("STB A, S8[USP]", 7),
                ("STB A, N16[X2]", 8),
                ("MOV A, er0", 8),
                ("MOV A, off N8", 11),
                ("MOV A, [DP]", 10),
                ("MOV A, S8[USP]", 11),
                ("MOV A, N16[X1]", 12),
                ("MOV DP, er0", 8),
                ("MOV DP, off N8", 11),
                ("MOV DP, [DP]", 10),
                ("MOV X1, S8[USP]", 11),
                ("MOV USP, N16[X1]", 12),
                ("MOV PSW, [DP]", 10),
                ("MOV SSP, er0", 8),
                ("MOV LRB, off N8", 11),
                ("MOVB A, r0", 6),
                ("MOVB A, off N8", 9),
                ("MOVB A, [DP]", 8),
                ("MOVB A, S8[USP]", 9),
                ("MOVB A, N16[X1]", 10),
                ("MOVB PSWH, r0", 6),
                ("MOVB PSWH, off N8", 9),
                ("MOVB PSWL, N16[X1]", 10),
                // A register, or `N8` (always on-chip), stored into memory.
                ("MOV er0, DP", 8),
                ("MOV er0, N8", 10),
                ("MOV off N8, DP", 11),
                ("MOV off N8, N'8", 13),
                ("MOVB r0, PSWH", 6),
                ("MOVB r0, N8", 8),
                ("MOVB off N8, PSWH", 9),
                ("MOVB off N8, N'8", 11),
                // The short immediate-to-register form is a plain store too.
                ("MOVB r0, #N8", 6),
                // Arithmetic, logic and compare into the accumulator.
                ("ADD A, er0", 7),
                ("ADD A, off N8", 9),
                ("ADD A, [DP]", 10),
                ("ADD A, S8[USP]", 11),
                ("ADD A, N16[X1]", 12),
                ("ADDB A, r0", 5),
                ("ADDB A, off N8", 7),
                ("ADDB A, S8[USP]", 9),
                ("ADDB A, N16[X1]", 10),
                ("ADC A, [DP]", 10),
                ("CMP A, er0", 7),
                ("CMP A, off N8", 9),
                ("CMPB A, N16[X1]", 10),
                ("XOR A, [DP]", 10),
                ("XOR A, er0", 8),
                ("XORB A, r0", 6),
            ],
        );
    }

    #[test]
    fn external_writes_through_the_object_prefix_cost_four_cycles_per_byte() {
        assert_manual_ext(
            "EXT, moves into memory, clears, exchanges, shifts, increments",
            &[
                ("MOV er0, A", 12),
                ("MOV off N8, A", 15),
                ("MOV [DP], A", 14),
                ("MOV S8[USP], A", 15),
                ("MOV N16[X1], A", 16),
                ("MOV er0, #N16", 16),
                ("MOV off N8, #N16", 19),
                ("MOV [DP], #N16", 18),
                ("MOV S8[USP], #N16", 19),
                ("MOV N'16[X1], #N16", 20),
                ("MOVB r0, A", 8),
                // Table 3-4 gives 11; the page for `MOVB off N8, obj` (3-95)
                // prints 13, the value of the row below it.
                ("MOVB off N8, A", 11),
                ("MOVB [DP], A", 10),
                ("MOVB S8[USP], A", 11),
                ("MOVB N16[X1], A", 12),
                ("MOVB off N'8, #N8", 13),
                ("MOVB [DP], #N8", 12),
                ("MOVB S8[USP], #N8", 13),
                ("MOVB N16[X1], #N8", 14),
                ("CLR er0", 12),
                ("CLR off N8", 15),
                ("CLR [DP]", 14),
                ("CLR S8[USP]", 15),
                ("CLR N16[X1]", 16),
                ("CLRB r0", 8),
                ("CLRB off N8", 11),
                ("CLRB [DP]", 10),
                ("CLRB N16[X1]", 12),
                ("XCHG A, er0", 13),
                ("XCHG A, off N8", 16),
                ("XCHG A, [DP]", 15),
                ("XCHG A, S8[USP]", 16),
                ("XCHG A, N16[X1]", 17),
                ("XCHGB A, r0", 9),
                ("XCHGB A, off N8", 12),
                ("XCHGB A, [DP]", 11),
                ("XCHGB A, N16[X1]", 13),
                ("XNBL A, off N8", 10),
                ("ROL er0", 13),
                ("ROL off N8", 16),
                ("ROL [DP]", 15),
                ("ROL S8[USP]", 16),
                ("ROL N16[X1]", 17),
                ("ROLB r0", 9),
                ("ROLB off N8", 12),
                ("ROLB [DP]", 11),
                ("ROLB N16[X1]", 13),
                ("SLL off N8", 16),
                ("SRLB [DP]", 11),
                ("SRA er0", 13),
                ("INC er0", 13),
                ("INC off N8", 16),
                ("INC [DP]", 15),
                ("INC S8[USP]", 16),
                ("INC N16[X1]", 17),
                ("INCB r0", 7),
                ("INCB off N8", 12),
                ("INCB [DP]", 11),
                ("INCB N16[X1]", 13),
                ("DEC er0", 13),
                ("DECB r0", 7),
            ],
        );
    }

    #[test]
    fn external_alu_destinations_cost_a_read_and_a_write() {
        assert_manual_ext(
            "EXT, arithmetic, logic and compare on a memory object",
            &[
                // Accumulator source. Compare is charged like the others.
                ("SUB er0, A", 13),
                ("SUB [DP], A", 15),
                ("SUB N16[X1], A", 17),
                ("ADDB r0, A", 9),
                ("ADDB off N8, A", 12),
                ("ADDB [DP], A", 11),
                ("ADDB S8[USP], A", 12),
                ("ADDB N16[X1], A", 13),
                ("CMP er0, A", 13),
                ("CMP [DP], A", 15),
                ("CMPB r0, A", 9),
                ("CMPB off N8, A", 12),
                ("CMPB N16[X1], A", 13),
                // Immediate source: one cycle more again.
                ("ADD er0, #N16", 17),
                ("ADD off N8, #N16", 20),
                ("ADD [DP], #N16", 19),
                ("ADD S8[USP], #N16", 20),
                ("ADD N'16[X1], #N16", 21),
                ("ADDB r0, #N8", 11),
                ("ADDB off N'8, #N8", 14),
                ("ADDB [DP], #N8", 13),
                ("ADDB S8[USP], #N8", 14),
                ("ADDB N16[X1], #N8", 15),
                ("CMP er0, #N16", 17),
                ("CMP [DP], #N16", 19),
                ("CMPB r0, #N8", 11),
                ("CMPB [DP], #N8", 13),
                ("XORB S8[USP], #N8", 14),
            ],
        );
    }

    #[test]
    fn external_bit_operations_cost_two_to_read_and_six_to_write() {
        assert_manual_ext(
            "EXT, bit operations and bit branches",
            &[
                ("SBR r0", 13),
                ("SBR off N8", 16),
                ("SBR [DP]", 15),
                ("SBR S8[USP]", 16),
                ("SBR N16[X1]", 17),
                ("RBR r0", 13),
                ("TBR r0", 6),
                ("TBR off N8", 9),
                ("TBR [DP]", 8),
                ("TBR S8[USP]", 9),
                ("TBR N16[X1]", 10),
                ("MBR C, r0", 7),
                ("MBR C, off N8", 10),
                ("MBR C, [DP]", 9),
                ("MBR C, S8[USP]", 10),
                ("MBR C, N16[X1]", 11),
                ("MBR r0, C", 16),
                ("MBR off N8, C", 19),
                ("MBR [DP], C", 18),
                ("MBR S8[USP], C", 19),
                ("MBR N16[X1], C", 20),
                ("SB r0.0", 13),
                ("SB off N8.0", 16),
                ("SB [DP].0", 15),
                ("SB S8[USP].0", 16),
                ("SB N16[X1].0", 17),
                ("RB r0.0", 13),
                ("RB [DP].7", 15),
                ("MB C, r0.0", 7),
                ("MB C, off N8.3", 10),
                ("MB C, [DP].2", 9),
                ("MB C, S8[USP].0", 10),
                ("MB C, N16[X1].0", 11),
                ("MB r0.0, C", 16),
                ("MB off N8.0, C", 19),
                ("MB [DP].0, C", 18),
                ("MB S8[USP].0, C", 19),
                ("MB N16[X1].0, C", 20),
                // Not taken; a taken branch adds four as it does on-chip.
                ("JBS off N8.0, rel8", 9),
                ("JBR off N8.7, rel8", 9),
            ],
        );
    }

    #[test]
    fn external_pointers_stack_and_register_bank_are_charged() {
        assert_manual_ext(
            "EXT, pointers held in memory, the system stack, MUL/DIV operands",
            &[
                ("J [er0]", 10),
                ("J [off N8]", 13),
                ("J [[DP]]", 12),
                ("J [S8[USP]]", 13),
                ("J [N16[X1]]", 14),
                ("LC A, [er0]", 15),
                ("LC A, [off N8]", 18),
                ("LC A, [[DP]]", 17),
                ("LC A, [S8[USP]]", 18),
                ("LC A, [N16[X1]]", 19),
                ("LC A, N16[off N8]", 22),
                // The byte table read through `[erN]` costs two, not four
                // (manual p. 3-75: 9 -> 11).
                ("LCB A, [er0]", 11),
                ("LCB A, [off N8]", 16),
                ("LCB A, [[DP]]", 15),
                ("LCB A, [S8[USP]]", 16),
                ("LCB A, [N16[X1]]", 17),
                ("LCB A, N16[off N8]", 22),
                ("CMPC A, [er0]", 15),
                ("CMPC A, [off N8]", 18),
                ("CMPCB A, [er0]", 11),
                ("CMPCB A, [[DP]]", 15),
                ("CMPCB A, N16[off N8]", 22),
                ("PUSHS A", 7),
                ("PUSHS LRB", 7),
                ("POPS A", 8),
                ("POPS LRB", 8),
                ("SCAL rel8", 13),
                ("CAL addr16", 13),
                ("CAL [DP]", 12),
                ("VCAL 0", 15),
                ("RT", 11),
                ("RTI", 31),
                ("MUL", 35),
                ("MULB", 21),
                ("DIV", 63),
                ("DIVB", 33),
            ],
        );
    }

    #[test]
    fn two_external_places_add_up() {
        assert_manual_ext_pairs(
            "EXT, forms with two memory operands",
            &[
                ("MOV er0, er1", [None, None, Some(12)]),
                ("MOV er0, off N8", [None, None, Some(15)]),
                ("MOV er0, [DP]", [Some(10), Some(10), Some(14)]),
                ("MOV er0, S8[USP]", [Some(11), Some(11), Some(15)]),
                ("MOV er0, N16[X1]", [Some(12), Some(12), Some(16)]),
                ("MOV off N8, er0", [None, None, Some(15)]),
                ("MOV off N8, off N'8", [None, None, Some(18)]),
                ("MOV off N8, [DP]", [Some(12), Some(13), Some(17)]),
                ("MOV off N8, S8[USP]", [Some(13), Some(14), Some(18)]),
                ("MOV off N8, N16[X1]", [Some(14), Some(15), Some(19)]),
                ("MOVB r0, r1", [None, None, Some(8)]),
                ("MOVB r0, off N8", [None, None, Some(11)]),
                ("MOVB r0, [DP]", [Some(8), Some(8), Some(10)]),
                ("MOVB r0, S8[USP]", [Some(9), Some(9), Some(11)]),
                ("MOVB r0, N16[X1]", [Some(10), Some(10), Some(12)]),
                ("MOVB off N8, r0", [None, None, Some(11)]),
                ("MOVB off N8, off N'8", [None, None, Some(14)]),
                ("MOVB off N8, [DP]", [Some(10), Some(11), Some(13)]),
                ("MOVB off N8, S8[USP]", [Some(11), Some(12), Some(14)]),
                ("MOVB off N8, N16[X1]", [Some(12), Some(13), Some(15)]),
                // `obj, off N8`: the manual charges the object two per byte
                // and the `off N8` source four per byte plus one.
                ("ADD er0, off N8", [None, None, Some(20)]),
                ("ADD DP, off N8", [Some(16), None, None]),
                ("ADD off N'8, off N8", [None, None, Some(23)]),
                ("ADD [DP], off N8", [Some(18), Some(13), Some(22)]),
                ("ADD S8[USP], off N8", [Some(19), Some(14), Some(23)]),
                ("ADD N16[X1], off N8", [Some(20), Some(15), Some(24)]),
                ("ADC N16[X2], off N8", [Some(20), Some(15), Some(24)]),
                ("ADDB r0, off N8", [None, None, Some(14)]),
                ("ADDB PSWH, off N8", [Some(12), None, None]),
                ("ADDB off N'8, off N8", [None, None, Some(17)]),
                ("XORB [DP], off N8", [Some(14), Some(11), Some(16)]),
                ("XORB S8[USP], off N8", [Some(15), Some(12), Some(17)]),
                ("XORB N16[X1], off N8", [Some(16), Some(13), Some(18)]),
                ("CMP [DP], off N8", [Some(18), Some(13), Some(22)]),
                // Calls: the pointer operand, then the system stack.
                ("CAL [er0]", [Some(12), Some(12), Some(16)]),
                ("CAL [off N8]", [Some(14), Some(15), Some(19)]),
                ("CAL [[DP]]", [Some(14), Some(14), Some(18)]),
                ("CAL [S8[USP]]", [Some(15), Some(15), Some(19)]),
                ("CAL [N16[X1]]", [Some(16), Some(16), Some(20)]),
            ],
        );
    }

    /// Operands that cannot be in external memory carry no surcharge.
    #[test]
    fn registers_immediates_and_direct_addresses_are_never_external() {
        for m in [
            "L A, #N16", "L A, DP", "L A, N8", "MOV DP, #N16", "MOV DP, X1", "ADD A, N8",
            "ROL A", "ROL DP", "INC DP", "CLR A", "TBR N8", "SB PSWH.0", "J addr16", "J [DP]",
            "J [N8]", "LC A, N16", "LC A, [X1]", "LC A, N16[X1]", "LCB A, N16[N8]", "SJ rel8",
            "JEQ rel8", "JRNZ DP, rel8", "NOP", "BRK",
        ] {
            assert_eq!(table_ext(m), ExtCycles::default(), "`{m}`");
        }
    }

    #[test]
    fn data_transfers_charge_the_addressing_mode() {
        assert_manual(
            "Tables 3-1..3-6, data transfer and clear",
            &[
                ("L A, er0", 2),
                ("L A, off N8", 4),
                ("L A, [DP]", 4),
                ("L A, S8[USP]", 5),
                ("L A, N16[X1]", 6),
                ("LB A, [DP]", 4),
                ("LB A, S8[USP]", 5),
                ("ST A, [DP]", 4),
                ("ST A, S8[USP]", 5),
                ("ST A, N16[X2]", 6),
                ("STB A, [DP]", 4),
                ("MOV A, [DP]", 6),
                ("MOV A, S8[USP]", 7),
                ("MOV A, N16[X1]", 8),
                ("MOV [DP], A", 6),
                ("MOV S8[USP], A", 7),
                ("MOV N16[X1], A", 8),
                ("MOV er0, #N16", 8),
                ("MOV off N8, #N16", 10),
                ("MOV [DP], #N16", 10),
                ("MOV S8[USP], #N16", 11),
                ("MOV N'16[X1], #N16", 12),
                ("MOV er0, [DP]", 6),
                ("MOV er0, S8[USP]", 7),
                ("MOV er0, N16[X1]", 8),
                ("MOV off N8, er0", 6),
                ("MOV off N8, off N'8", 8),
                ("MOV off N8, [DP]", 8),
                ("MOV off N8, S8[USP]", 9),
                ("MOV DP, [DP]", 6),
                ("MOV X1, S8[USP]", 7),
                ("MOVB A, [DP]", 6),
                ("MOVB [DP], A", 6),
                ("MOVB S8[USP], A", 7),
                ("MOVB off N'8, #N8", 8),
                ("MOVB [DP], #N8", 8),
                ("MOVB S8[USP], #N8", 9),
                ("MOVB N16[X1], #N8", 10),
                ("MOVB r0, [DP]", 6),
                ("MOVB r0, S8[USP]", 7),
                ("CLR er0", 4),
                ("CLR [DP]", 6),
                ("CLR S8[USP]", 7),
                ("CLR N16[X1]", 8),
                ("CLRB [DP]", 6),
            ],
        );
    }

    #[test]
    fn exchanges_cost_one_cycle_more_than_a_move() {
        assert_manual(
            "Table 3-6, exchange",
            &[
                ("XCHG A, er0", 5),
                ("XCHG A, DP", 5),
                ("XCHG A, off N8", 7),
                ("XCHG A, [DP]", 7),
                ("XCHG A, S8[USP]", 8),
                ("XCHG A, N16[X1]", 9),
                ("XCHGB A, r0", 5),
                ("XCHGB A, [DP]", 7),
                ("XNBL A, off N8", 5),
                ("SWAP", 2),
                ("SWAPB", 2),
            ],
        );
    }

    #[test]
    fn memory_shifts_and_rotates_cost_one_cycle_more_than_the_accumulator_form() {
        assert_manual(
            "Tables 3-8..3-10, rotate and shift",
            &[
                ("ROL A", 2),
                ("ROL er0", 5),
                ("ROL DP", 5),
                ("ROL off N8", 7),
                ("ROL N8", 7),
                ("ROL [DP]", 7),
                ("ROL S8[USP]", 8),
                ("ROL N16[X1]", 9),
                ("ROLB A", 2),
                ("ROLB r0", 5),
                ("ROLB [DP]", 7),
                ("ROR off N8", 7),
                ("RORB S8[USP]", 8),
                ("SLL er0", 5),
                ("SLL [DP]", 7),
                ("SLLB N16[X2]", 9),
                ("SRL S8[USP]", 8),
                ("SRLB r0", 5),
                ("SRA [DP]", 7),
                ("SRAB A", 2),
                ("SRAB off N8", 7),
            ],
        );
    }

    #[test]
    fn increments_cost_one_cycle_more_than_two_per_byte() {
        assert_manual(
            "Table 3-11, increment/decrement",
            &[
                ("INC er0", 5),
                ("INC DP", 3),
                ("INC USP", 5),
                ("INC LRB", 3),
                ("INC off N8", 7),
                ("INC [DP]", 7),
                ("INC S8[USP]", 8),
                ("INC N16[X1]", 9),
                ("INCB r0", 3),
                ("INCB PSWH", 5),
                ("INCB [DP]", 7),
                ("DEC X1", 3),
                ("DEC [DP]", 7),
                ("DEC S8[USP]", 8),
                ("DECB r0", 3),
                ("DECB N16[X2]", 9),
            ],
        );
    }

    #[test]
    fn rom_table_reads_charge_the_pointer_fetch() {
        assert_manual(
            "Tables 3-12 and 3-13, ROM table reference",
            &[
                ("LC A, N16", 15),
                ("LC A, [er0]", 11),
                ("LC A, [DP]", 11),
                ("LC A, [off N8]", 13),
                ("LC A, [[DP]]", 13),
                ("LC A, [S8[USP]]", 14),
                ("LC A, [N16[X1]]", 15),
                ("LC A, N16[DP]", 15),
                ("LC A, N16[off N8]", 17),
                ("LCB A, N16", 13),
                ("LCB A, [er0]", 9),
                ("LCB A, [X1]", 9),
                ("LCB A, [off N8]", 11),
                ("LCB A, [[DP]]", 11),
                ("LCB A, [S8[USP]]", 12),
                ("LCB A, [N16[X1]]", 13),
                ("LCB A, N16[X1]", 13),
                // The memory-based index costs the byte forms as much as the
                // word forms (manual p. 3-76: 17, not 15).
                ("LCB A, N16[off N8]", 17),
                ("LCB A, N16[N8]", 17),
                ("CMPC A, [er0]", 11),
                ("CMPC A, [[DP]]", 13),
                ("CMPC A, N16[off N8]", 17),
                ("CMPCB A, [er0]", 9),
                ("CMPCB A, [S8[USP]]", 12),
                ("CMPCB A, N16[USP]", 13),
                ("CMPCB A, N16[N8]", 17),
            ],
        );
    }

    #[test]
    fn alu_forms_charge_the_operand_combination() {
        assert_manual(
            "Tables 3-14..3-29, arithmetic, logic and compare",
            &[
                // Accumulator destination: the one-byte register-bank form
                // costs three, every other form two per byte plus the mode.
                ("ADD A, #N16", 6),
                ("ADD A, er0", 3),
                ("ADD A, DP", 4),
                ("ADD A, off N8", 4),
                ("ADD A, N8", 6),
                ("ADD A, [DP]", 6),
                ("ADD A, S8[USP]", 7),
                ("ADD A, N16[X1]", 8),
                ("ADDB A, #N8", 4),
                ("ADDB A, r0", 3),
                ("ADC A, er0", 3),
                ("SUB A, er0", 3),
                ("SBCB A, r0", 3),
                ("ANDB A, r0", 3),
                ("OR A, er0", 3),
                ("CMP A, er0", 3),
                ("CMPB A, r0", 3),
                // XOR has no one-byte form.
                ("XOR A, er0", 4),
                ("XORB A, r0", 4),
                ("XOR A, DP", 4),
                ("XOR A, [DP]", 6),
                // Memory destination, accumulator source: one cycle more.
                ("ADD er0, A", 5),
                ("ADD off N8, A", 7),
                ("ADD [DP], A", 7),
                ("ADD S8[USP], A", 8),
                ("ADD N16[X1], A", 9),
                ("ADDB r0, A", 5),
                ("ADDB [DP], A", 7),
                ("ADC DP, A", 5),
                ("SUBB S8[USP], A", 8),
                ("ORB N16[X1], A", 9),
                ("XOR [DP], A", 7),
                ("CMP er0, A", 5),
                ("CMP [DP], A", 7),
                ("CMPB S8[USP], A", 8),
                // Memory destination, `off N8` source: one cycle more.
                ("ADD er0, off N8", 7),
                ("ADD DP, off N8", 7),
                ("ADD off N'8, off N8", 9),
                ("ADD [DP], off N8", 9),
                ("ADD S8[USP], off N8", 10),
                ("ADD N16[X1], off N8", 11),
                ("ADDB r0, off N8", 7),
                ("ADDB [DP], off N8", 9),
                ("SBCB [DP], off N8", 9),
                ("XORB r0, off N8", 7),
                ("XORB [DP], off N8", 9),
                ("XORB S8[USP], off N8", 10),
                ("XORB N16[X1], off N8", 11),
                ("CMP [DP], off N8", 9),
                ("CMPB N16[X1], off N8", 11),
                // Memory destination, immediate source: nothing extra.
                ("ADD er0, #N16", 8),
                ("ADD off N8, #N16", 10),
                ("ADD [DP], #N16", 10),
                ("ADD S8[USP], #N16", 11),
                ("ADD N'16[X1], #N16", 12),
                ("ADDB r0, #N8", 6),
                ("ADDB off N'8, #N8", 8),
                ("ADDB [DP], #N8", 8),
                ("ADDB S8[USP], #N8", 9),
                ("ADDB N16[X1], #N8", 10),
                ("XORB [DP], #N8", 8),
                ("CMP [DP], #N16", 10),
                ("CMPB [DP], #N8", 8),
                ("CMPB S8[USP], #N8", 9),
            ],
        );
    }

    #[test]
    fn bit_operations_charge_the_operation_and_the_addressing_mode() {
        assert_manual(
            "Tables 3-32 and 3-33, bit operations",
            &[
                ("SBR r0", 7),
                ("SBR off N8", 9),
                ("SBR [DP]", 9),
                ("SBR S8[USP]", 10),
                ("RBR N16[X1]", 11),
                // Testing a bit costs no more than reading the byte.
                ("TBR r0", 4),
                ("TBR off N8", 6),
                ("TBR [DP]", 6),
                ("TBR S8[USP]", 7),
                ("TBR N16[X1]", 8),
                // Bit to carry is cheap, carry to bit is not -- with the bit
                // number in the accumulator (MBR) exactly as with a literal (MB).
                ("MBR C, r0", 5),
                ("MBR C, off N8", 7),
                ("MBR C, [DP]", 7),
                ("MBR C, S8[USP]", 8),
                ("MBR C, N16[X1]", 9),
                ("MBR r0, C", 10),
                ("MBR off N8, C", 12),
                ("MBR [DP], C", 12),
                ("MBR S8[USP], C", 13),
                ("MBR N16[X1], C", 14),
                ("SB r0.0", 7),
                ("SB off N8.0", 9),
                ("SB [DP].0", 9),
                ("SB S8[USP].0", 10),
                ("SB N16[X1].0", 11),
                ("RB [DP].7", 9),
                ("MB C, r0.0", 5),
                ("MB C, off N8.3", 7),
                ("MB C, [DP].0", 7),
                ("MB C, S8[USP].0", 8),
                ("MB r0.0, C", 10),
                ("MB [DP].0, C", 12),
                ("MB S8[USP].0, C", 13),
                ("MB N16[X1].0, C", 14),
            ],
        );
    }

    #[test]
    fn indirect_jumps_and_calls_charge_the_pointer_fetch() {
        assert_manual(
            "Tables 3-34..3-37, jump, call and other",
            &[
                ("SJ rel8", 8),
                ("J addr16", 7),
                ("J [er0]", 6),
                ("J [DP]", 6),
                ("J [off N8]", 8),
                ("J [[DP]]", 8),
                ("J [S8[USP]]", 9),
                ("J [N16[X1]]", 10),
                ("SCAL rel8", 9),
                ("CAL addr16", 9),
                ("CAL [er0]", 8),
                ("CAL [DP]", 8),
                ("CAL [off N8]", 10),
                ("CAL [[DP]]", 10),
                ("CAL [S8[USP]]", 11),
                ("CAL [N16[X1]]", 12),
                ("VCAL 0", 11),
                ("RT", 7),
                ("RTI", 15),
                ("BRK", 13),
                ("JEQ rel8", 4),
                ("JBS off N8.0, rel8", 6),
                ("JBR off N8.7, rel8", 6),
                ("JRNZ DP, rel8", 7),
                ("PUSHS A", 3),
                ("PUSHS LRB", 3),
                ("POPS A", 4),
                ("POPS LRB", 4),
                ("DAA", 6),
                ("DAS", 6),
                ("EXTND", 2),
                ("SC", 2),
                ("RC", 2),
                ("NOP", 2),
                ("MUL", 27),
                ("MULB", 19),
                ("DIV", 47),
                ("DIVB", 29),
            ],
        );
    }
}

/// Render a decoded instruction with its immediates substituted in.
pub fn format(d: &Decoded, pc_after: u16) -> String {
    let f = &d.fields;
    let mut s = d.mnemonic.to_string();
    // Longest placeholders first so shorter ones cannot corrupt them.
    for (tok, val) in [
        ("addr16", format!("0{:04X}h", f.addr16)),
        ("N'16", format!("0{:04X}h", f.n16_alt)),
        ("N'8", format!("0{:02X}h", f.n8_alt)),
        ("N16", format!("0{:04X}h", f.n16)),
        ("rel8", format!("0{:04X}h", pc_after.wrapping_add(f.rel8 as u16))),
        ("S8", format!("{}", f.s8)),
        ("N8", format!("0{:02X}h", f.n8)),
    ] {
        if let Some(pos) = s.find(tok) {
            s.replace_range(pos..pos + tok.len(), &val);
        }
    }
    s
}

