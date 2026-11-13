//! Acceptance: the whole `check` pipeline (script, wiring, verdict) on the
//! stock P28-230 ROM, manual and automatic. Every test needs the ROM image
//! (feature `p28-rom`); each full run is about 20 s of wall time.

use crate::bench::Bench;
use crate::blink::Lamp;
use crate::bus::P28_PB_LOCKUP_SOLENOID;
use crate::car::Setup;
use crate::script::{run, RunResult};
use crate::verdict::{judge, Report, Verdict};

/// Run `check`'s script on `bench` and judge it; the bench is returned so a
/// test can look at RAM afterwards.
fn check_bench(mut bench: Bench, setup: &Setup) -> (RunResult, Report, Bench) {
    let result = run(&mut bench, setup);
    let report = judge(&result, setup);
    (result, report, bench)
}

/// What `hondaecu-cli check <path>` computes for `setup`.
pub fn check_rom(path: &str, setup: &Setup) -> (RunResult, Report) {
    let bench = Bench::from_file(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let (result, report, _) = check_bench(bench, setup);
    (result, report)
}

/// The stored fault codes, from the fault word at RAM `212h..215h`: bit n of
/// the little-endian 32-bit value is code n + 1 (0x51B9). A developer
/// cross-check of the blink decoder, not something a user sees.
fn stored_codes(bench: &Bench) -> Vec<u8> {
    let r = &bench.bus.ram;
    let word = u32::from_le_bytes([r[0x212], r[0x213], r[0x214], r[0x215]]);
    (0..32).filter(|n| word & (1 << n) != 0).map(|n| n as u8 + 1).collect()
}

fn preset(name: &str) -> Setup {
    Setup::preset(name).unwrap()
}

fn rom() -> String {
    crate::fixtures::p28_rom_path()
}

#[test]
#[cfg_attr(not(feature = "p28-rom"), ignore = "needs the stock P28 ROM: enable feature p28-rom")]
fn case_1_stock_p28_manual_is_worth_burning() {
    let (_, report) = check_rom(&rom(), &preset("stock-p28-manual"));
    assert_eq!(report.verdict, Verdict::WorthBurning, "{report:#?}");
}

#[test]
#[cfg_attr(not(feature = "p28-rom"), ignore = "needs the stock P28 ROM: enable feature p28-rom")]
fn case_2_stock_p28_automatic_is_worth_burning() {
    let setup = preset("stock-p28-automatic");
    let (result, report, bench) = check_bench(Bench::from_file(&rom()).unwrap(), &setup);
    assert_eq!(report.verdict, Verdict::WorthBurning, "{report:#?}");
    assert_eq!(result.lamp, Lamp::Steady);
    assert_eq!(stored_codes(&bench), Vec::<u8>::new());
    // 0x55C5..0x55D0 copies the gearbox strap (U5/X1, RAM 3C7h bit 7) into
    // 216h.3: the ROM ran as an automatic.
    assert_ne!(bench.bus.ram[0x216] & 0x08, 0);
    // The car is really driven as an automatic: with D4 selected the routine
    // at 0x32A8..0x3387 engaged lock-up solenoid A (MB P0.4, C at 0x3387,
    // copied to port B) at some point, and the lock-up feedback checks at
    // 0x3C98 (99h.6) and 0x3CAF (9Ch.7), both code 19, stayed clean.
    assert!(bench.log.board.iter().any(|&(_, b, _)| b & P28_PB_LOCKUP_SOLENOID != 0));
}

#[test]
#[cfg_attr(not(feature = "p28-rom"), ignore = "needs the stock P28 ROM: enable feature p28-rom")]
fn case_5_bad_checksum_resets() {
    let mut image = std::fs::read(rom()).unwrap();
    image.truncate(32768);
    image[0x7FFF] ^= 0x01; // unused fill byte; the sum is no longer 0
    let (_, report, _) = check_bench(Bench::new(&image), &preset("stock-p28-manual"));
    match &report.verdict {
        // verdict::settles: "resets at <times> (PC <pc>h)".
        Verdict::DoNotBurn(why) => assert!(why.starts_with("settles after power-on: resets at"), "{why}"),
        v => panic!("{v:?}"),
    }
}

#[test]
#[cfg_attr(not(feature = "p28-rom"), ignore = "needs the stock P28 ROM: enable feature p28-rom")]
fn case_7_gearbox_disagreement_is_recorded() {
    // Neither mismatch stores a code (docs/CHECK.md, "Gearbox mismatch").
    //
    // Automatic board, manual car: no position switch, so 211h.5/6/7 all
    // read 0 and the lock-up routine (0x32A8: P/N, 0x32AB: D3, 0x32AE: D4)
    // never commands lock-up; 22Fh.0 stays clear and PB4 low, and lock-up B's
    // duty 381h stays 0. The checks that store code 19 compare each driver's
    // feedback with that off command: 0x3C98..0x3CA5 (port A bit 4, 99h.6)
    // and 0x3CAF..0x3CDD (4700h bit 3, 9Ch.7). The model's feedback follows
    // the driver whether or not a solenoid is on the end of it, so both pass.
    // Honda practice expects code 19 here; the disagreement is recorded.
    //
    // Manual board, automatic car: 216h.3 is clear, so 0x3C92 and 0x5F5C skip
    // both lock-up checks. The position inputs reach the ROM; observed, lock-up
    // A is never engaged.
    for (board, car) in [("automatic", "manual"), ("manual", "automatic")] {
        let mut setup = preset("stock-p28-manual");
        setup.set("board_gearbox", board).unwrap();
        setup.set("gearbox", car).unwrap();
        let (result, report, bench) = check_bench(Bench::from_file(&rom()).unwrap(), &setup);
        let case = format!("board {board}, car {car}");
        assert_eq!(report.verdict, Verdict::WorthBurning, "{case}: {report:#?}");
        assert!(report.codes.is_empty(), "{case}");
        assert_eq!(result.lamp, Lamp::Steady, "{case}");
        assert_eq!(stored_codes(&bench), Vec::<u8>::new(), "{case}");
        let lockup_engaged = bench.log.board.iter().any(|&(_, b, _)| b & P28_PB_LOCKUP_SOLENOID != 0);
        assert!(!lockup_engaged, "{case}");
    }
}
