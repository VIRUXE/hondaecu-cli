// Operand model for the OKI 66207.
//
// FULL_OPCODES stores each instruction as display text ("MOV DP, #N16",
// "JBS off N8.5, rel8"). Rather than hand-writing 2623 execution arms, the
// operand text is parsed once into this small tree and evaluated generically.
//
// Addressing notes that are easy to get wrong:
//   * `N8` is an absolute low-RAM/SFR address (0x00..0xFF).
//   * `off N8` is LRB-paged:  ((LRB >> 5) << 8) | N8.
//   * `rN` / `erN` live in RAM at the local register bank, whose base is
//     ((LRB >> 5) << 8) | ((LRB & 0x1F) << 3).
//   * `LC`/`CMPC` address *code* space; every other form addresses data space.

use std::collections::HashSet;
use std::sync::OnceLock;

use crate::full_decoder::FULL_OPCODES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reg {
    A,
    Dp,
    X1,
    X2,
    Usp,
    Ssp,
    Lrb,
    Psw,
    PswL,
    PswH,
}

/// How to compute an effective address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mem {
    /// `N8` -- absolute low RAM / SFR.
    Direct,
    /// `N'8` -- second immediate used as an address.
    DirectAlt,
    /// `off N8` -- LRB-paged.
    OffPage,
    /// `off N'8`
    OffPageAlt,
    /// `[reg]`
    AtReg(Reg),
    /// `[erN]`
    AtEr(u8),
    /// `S8[USP]`
    IdxUsp,
    /// `N16[reg]`
    IdxReg(Reg),
    /// `N16[N8]` -- base is the word held at RAM `N8`.
    IdxMemN8,
    /// `N16[off N8]` -- base is the word held at the LRB-paged `off N8`.
    IdxMemOff,
    /// `N16` used directly as an address (code-space loads).
    Abs16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
    Reg(Reg),
    /// `erN` -- 16-bit register-bank slot.
    Er(u8),
    /// `rN` -- 8-bit register-bank slot.
    R(u8),
    Carry,
    ImmN8,
    ImmN16,
    ImmN8Alt,
    ImmN16Alt,
    Mem(Mem),
    /// `<arg>.<bit>`
    Bit(Box<Arg>, u8),
    Addr16,
    Rel8,
    /// Bare integer, as in `VCAL 3`.
    Lit(u8),
}

#[derive(Debug, Clone)]
pub struct Parsed {
    /// Base mnemonic, e.g. "MOVB".
    pub op: &'static str,
    pub args: Vec<Arg>,
    /// Byte-width operation (mnemonic ends in B, and is not a bit/branch op).
    pub byte_width: bool,
}

fn parse_reg(s: &str) -> Option<Reg> {
    Some(match s {
        "A" => Reg::A,
        "DP" => Reg::Dp,
        "X1" => Reg::X1,
        "X2" => Reg::X2,
        "USP" => Reg::Usp,
        "SSP" => Reg::Ssp,
        "LRB" => Reg::Lrb,
        "PSW" => Reg::Psw,
        "PSWL" => Reg::PswL,
        "PSWH" => Reg::PswH,
        _ => return None,
    })
}

fn parse_arg(s: &str) -> Option<Arg> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    // Bit suffix: "<base>.<n>". Split from the right so "N16[X1].3" works.
    if let Some((base, bit)) = s.rsplit_once('.')
        && bit.len() == 1
        && let Some(n) = bit.chars().next().unwrap().to_digit(8)
    {
        return Some(Arg::Bit(Box::new(parse_arg(base)?), n as u8));
    }

    // Immediates.
    if let Some(rest) = s.strip_prefix('#') {
        return Some(match rest {
            "N8" => Arg::ImmN8,
            "N16" => Arg::ImmN16,
            "N'8" => Arg::ImmN8Alt,
            "N'16" => Arg::ImmN16Alt,
            _ => return None,
        });
    }

    // Indirect through a register or register-bank slot: "[DP]", "[er0]".
    if s.starts_with('[') && s.ends_with(']') {
        let inner = &s[1..s.len() - 1];
        if let Some(r) = parse_reg(inner) {
            return Some(Arg::Mem(Mem::AtReg(r)));
        }
        if let Some(n) = inner.strip_prefix("er").and_then(|d| d.parse().ok()) {
            return Some(Arg::Mem(Mem::AtEr(n)));
        }
        // "[[DP]]", "[off N8]", "[N8]", "[S8[USP]]", "[N16[X1]]": the extra
        // bracket layer is display notation for jump/call targets, where the
        // value fetched *is* the destination. The addressing itself is the
        // inner form.
        return parse_arg(inner);
    }

    // Indexed: "N16[X1]", "S8[USP]", "N16[N8]".
    if s.ends_with(']')
        && let Some(open) = s.find('[')
    {
        let (disp, base) = (&s[..open], &s[open + 1..s.len() - 1]);
        return Some(Arg::Mem(match (disp, base) {
            ("S8", "USP") => Mem::IdxUsp,
            ("N16", "N8") => Mem::IdxMemN8,
            ("N16", "off N8") => Mem::IdxMemOff,
            ("N16", b) => Mem::IdxReg(parse_reg(b)?),
            ("N'16", b) => Mem::IdxReg(parse_reg(b)?),
            _ => return None,
        }));
    }

    // LRB-paged direct.
    if let Some(rest) = s.strip_prefix("off ") {
        return Some(Arg::Mem(match rest.trim() {
            "N8" => Mem::OffPage,
            "N'8" => Mem::OffPageAlt,
            _ => return None,
        }));
    }

    if let Some(r) = parse_reg(s) {
        return Some(Arg::Reg(r));
    }
    if s == "C" {
        return Some(Arg::Carry);
    }
    if let Some(n) = s.strip_prefix("er").and_then(|d| d.parse().ok()) {
        return Some(Arg::Er(n));
    }
    if let Some(n) = s
        .strip_prefix('r')
        .filter(|d| d.len() == 1)
        .and_then(|d| d.parse().ok())
    {
        return Some(Arg::R(n));
    }

    Some(match s {
        "addr16" => Arg::Addr16,
        "rel8" => Arg::Rel8,
        "N8" => Arg::Mem(Mem::Direct),
        "N'8" => Arg::Mem(Mem::DirectAlt),
        "N16" => Arg::Mem(Mem::Abs16),
        _ => {
            let n: u8 = s.parse().ok()?;
            Arg::Lit(n)
        }
    })
}

/// A trailing B only means "byte variant" when dropping it leaves another real
/// mnemonic: MOVB -> MOV, LB -> L. It must NOT fire for SUB (-> "SU"), SB, RB
/// or MB, whose B is part of the name.
fn is_byte_variant(op: &str, all: &HashSet<&str>) -> bool {
    op.len() > 1 && op.ends_with('B') && all.contains(&op[..op.len() - 1])
}

fn parse_one(text: &'static str, all: &HashSet<&str>) -> Option<Parsed> {
    let (op, rest) = match text.split_once(' ') {
        Some((o, r)) => (o, r),
        None => (text, ""),
    };
    let mut args = Vec::new();
    if !rest.trim().is_empty() {
        // Operands are comma-separated, and no operand form contains a comma.
        for part in rest.split(',') {
            args.push(parse_arg(part)?);
        }
    }
    Some(Parsed {
        op,
        byte_width: is_byte_variant(op, all),
        args,
    })
}

/// Parsed form of every FULL_OPCODES entry, indexed identically.
pub fn table() -> &'static [Option<Parsed>] {
    static TABLE: OnceLock<Vec<Option<Parsed>>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let all: HashSet<&str> = FULL_OPCODES
            .iter()
            .map(|p| p.mnemonic.split(' ').next().unwrap())
            .collect();
        FULL_OPCODES
            .iter()
            .map(|p| parse_one(p.mnemonic, &all))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every instruction in the table must parse; a silent None would turn into
    /// an unexecutable instruction at runtime.
    #[test]
    fn every_pattern_parses() {
        let unparsed: Vec<&str> = FULL_OPCODES
            .iter()
            .zip(table())
            .filter(|(_, p)| p.is_none())
            .map(|(o, _)| o.mnemonic)
            .collect();
        assert!(
            unparsed.is_empty(),
            "{} unparsed operand forms, e.g. {:?}",
            unparsed.len(),
            &unparsed[..unparsed.len().min(15)]
        );
    }

    fn parse(text: &'static str) -> Option<Parsed> {
        let all: HashSet<&str> = FULL_OPCODES
            .iter()
            .map(|p| p.mnemonic.split(' ').next().unwrap())
            .collect();
        parse_one(text, &all)
    }

    #[test]
    fn parses_representative_forms() {
        let p = parse("JBS off N8.5, rel8").unwrap();
        assert_eq!(p.op, "JBS");
        assert!(!p.byte_width, "JBS ends in S, not a width marker");
        assert_eq!(
            p.args,
            vec![Arg::Bit(Box::new(Arg::Mem(Mem::OffPage)), 5), Arg::Rel8]
        );

        let p = parse("MOVB N'8, #N8").unwrap();
        assert!(p.byte_width);
        assert_eq!(p.args, vec![Arg::Mem(Mem::DirectAlt), Arg::ImmN8]);

        // A trailing B that is part of the name must not mean byte width.
        for op in [
            "RB N8.1",
            "TBR [DP]",
            "SUB A, #N16",
            "SB r0.2",
            "MB C, [DP].1",
        ] {
            assert!(!parse(op).unwrap().byte_width, "{op}");
        }
        for op in ["SUBB A, #N8", "MOVB r0, #N8", "LB A, [DP]"] {
            assert!(parse(op).unwrap().byte_width, "{op}");
        }

        assert_eq!(
            parse("LC A, N16[X1]").unwrap().args,
            vec![Arg::Reg(Reg::A), Arg::Mem(Mem::IdxReg(Reg::X1))]
        );
    }
}
