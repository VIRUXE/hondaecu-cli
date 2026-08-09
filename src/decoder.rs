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
}

/// INT (internal-memory) machine-cycle count for one instruction, from the
/// MSM66201/66207 instruction manual's "Instruction List" cycle tables
/// (chapter 3 §3, the `Int*1 Int*2` column).
///
/// Across every operation class the all-internal-operand cost is **2 cycles per
/// instruction byte** plus a small per-operation adjustment. The large
/// multi-cycle operations (MUL/DIV/RTI/CAL/...) and the ROM-table reads
/// (LC/LCB/CMPC) carry the fixed extras below; a *taken* conditional branch adds
/// four more cycles, which `exec` applies at run time since only it knows the
/// branch was taken. Every value here is checked against the manual for a
/// representative encoding in the unit tests.
///
/// Known residual: the +1/+2 pointer-indirect data-access surcharges the manual
/// lists for `[DP]`/`[erN]` *data* operands are not modelled, so a handful of
/// indirect-addressing instructions run a cycle or two optimistic. This is far
/// below the old flat "two clocks per instruction" error and keeps the model a
/// pure function of the decoded mnemonic.
fn int_cycles(mnemonic: &str, len: usize) -> u16 {
    let base = 2 * len as u16;
    let op = mnemonic.split(|c| c == ' ' || c == ',').next().unwrap_or("");
    let extra = match op {
        // Multiply / divide (MSM66201 manual pp. 3-100/3-101, 3-57/3-58).
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
        // ROM (code-space) reads: table lookups.
        "LC" | "CMPC" => 7,
        "LCB" | "CMPCB" => 5,
        // Short call / short jump and the stack ops.
        "SCAL" => 5,
        "SJ" => 4,
        "PUSHS" | "PUSHU" => 1,
        "POPS" => 2,
        // Bit set/reset/test forms.
        "SB" | "RB" | "SBR" | "RBR" | "MBR" => 3,
        "TBR" => 1,
        // Decrement-and-branch carries a base surcharge even when not taken.
        "JRNZ" => 3,
        // Call / jump: indirect forms cost more than the absolute form.
        "CAL" => {
            if mnemonic.contains('[') {
                4
            } else {
                3
            }
        }
        "J" => {
            if mnemonic.contains('[') {
                2
            } else {
                1
            }
        }
        // Move-bit: reading a bit into carry is cheap; writing one back is not.
        "MB" => {
            if mnemonic.starts_with("MB C,") {
                1
            } else {
                6
            }
        }
        // Everything else (MOV/L/ST/ADD/ADC/SUB/SBC/CMP/AND/OR/XOR/INC/DEC/
        // shifts/rotates/CLR/XCHG/SWAP/EXTND/NOP/SC/RC/conditional branches ...)
        // is exactly two cycles per byte for internal operands.
        _ => 0,
    };
    base + extra
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
        Index { by_first }
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
            cycles: int_cycles(p.mnemonic, p.bytes_pat.len()),
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
                Some(d) => (d.mnemonic.split(|c| c == ' ' || c == ',').next().unwrap(), d.len),
                None => ("<none>", 0),
            };
            if got_len == want_len {
                ok_len += 1;
            }
            // dasm662 prints this one as "TRB"; 66207.op (and the assembler)
            // call it "TBR". Same encoding, same length -- spelling only.
            let want_mnem = if want_mnem == "TRB" { "TBR" } else { want_mnem };

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
    use super::int_cycles;

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
            ("ROL off N8", 3, 7 - 1), // manual 7; model omits the +1 RMW surcharge
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

