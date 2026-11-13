//! Acceptance: the whole `check` pipeline (script, wiring, verdict) on the
//! stock P28-230 ROM, manual and automatic. Every test needs the ROM image
//! (feature `p28-rom`); each full run is about 20 s of wall time.

use crate::bench::Bench;
use crate::blink::Lamp;
use crate::bus::P28_PB_LOCKUP_SOLENOID;
use crate::car::Setup;
use crate::script::{run, RunResult};
use crate::verdict::{judge, Report, State, Verdict};

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

/// The gates of both code-19 checks are open at the end of the run: 216h.3
/// set (automatic; 0x3C92 for 0x3C98, 0x5F5C for 0x3CAF), 214h.2 clear
/// (code 19 not yet stored; 0x3C95, 0x5F5F) and 227h.2 clear (0x3C8F,
/// 0x3CA9). So "no code 19" means the checks ran and passed, not that they
/// were skipped. Read at the end only: the script gives no per-instruction
/// view.
fn assert_lockup_checks_enabled(bench: &Bench, case: &str) {
    let r = &bench.bus.ram;
    assert_ne!(r[0x216] & 0x08, 0, "{case}: 216h.3");
    assert_eq!(r[0x214] & 0x04, 0, "{case}: 214h.2");
    assert_eq!(r[0x227] & 0x04, 0, "{case}: 227h.2");
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
    // 216h.3: the ROM ran as an automatic, with the code-19 checks enabled.
    assert_lockup_checks_enabled(&bench, "automatic");
    // The car is really driven as an automatic: with D4 selected the routine
    // at 0x32A8..0x3387 engaged lock-up solenoid A (MB P0.4, C at 0x3387,
    // copied to port B) at some point, and the lock-up feedback checks at
    // 0x3C98 (99h.6) and 0x3CAF (9Ch.7), both code 19, stayed clean.
    assert!(bench.log.board.iter().any(|&(_, b, _)| b & P28_PB_LOCKUP_SOLENOID != 0));
}

/// `stock-p28-manual` with one key changed, run and judged.
fn one_change(key: &str, value: &str) -> (RunResult, Report, Bench) {
    let mut setup = preset("stock-p28-manual");
    setup.set(key, value).unwrap();
    check_bench(Bench::from_file(&rom()).unwrap(), &setup)
}

fn stage<'a>(result: &'a RunResult, name: &str) -> &'a crate::script::StageResult {
    result.stages.iter().find(|s| s.name == name).unwrap()
}

#[test]
#[cfg_attr(not(feature = "p28-rom"), ignore = "needs the stock P28 ROM: enable feature p28-rom")]
fn case_6_one_wire_o2_sensor_stores_the_heater_code() {
    // The heater is unplugged, so its driver feedback (port A bit 6) stays 0
    // while the ROM powers it (PC2 low); 0x2094..0x20CB sets 9Bh.2. The ROM
    // stores fault bit 27 (6C99h index 27 = 1Bh) and blinks it as 41.
    let (result, report, bench) = one_change("o2_sensor", "one-wire");
    // Observed: the MIL lights at 8.8 s, in `cold idle`.
    assert!(
        matches!(&report.verdict, Verdict::DoNotBurn(why) if why.starts_with("check-engine light: lit at")),
        "{report:#?}"
    );
    assert_eq!(result.lamp, Lamp::Codes(vec![41]));
    assert_eq!(report.codes, vec![41]);
    assert_eq!(stored_codes(&bench), vec![27]);
}

#[test]
#[cfg_attr(not(feature = "p28-rom"), ignore = "needs the stock P28 ROM: enable feature p28-rom")]
fn case_6_no_eld_stores_code_20() {
    // U5/X0 open reads E8h (wiring::ELD_OPEN_COUNTS); the check at
    // 0x3CF1..0x3D02 accepts 0Eh..DCh only, so 99h.7 is set and code 20
    // stored (6C99h index 16 = 14h). The ROM does not light the MIL for it.
    let (result, report, bench) = one_change("eld", "no");
    assert!(matches!(&report.verdict, Verdict::DoNotBurn(why) if why.starts_with("codes: 20")), "{report:#?}");
    assert_eq!(result.lamp, Lamp::Codes(vec![20]));
    assert_eq!(result.mil_first_lit_after_bulb_check, None);
    assert_eq!(stored_codes(&bench), vec![20]);
}

#[test]
#[cfg_attr(not(feature = "p28-rom"), ignore = "needs the stock P28 ROM: enable feature p28-rom")]
fn case_6_no_speed_sensor_stores_nothing_in_this_script() {
    // The VSS check at 0x3C74..0x3C8B sets 99h.4 only with no VSS period
    // (231h.3) during deceleration fuel cut (21Ch.4, a copy of 11Ch.4 from
    // 0x18BD), and code 17 needs 45 passes of the debounce (6C68h index
    // 13 = 2Dh). A development probe saw 99h.4 set for about 1.75 s of the
    // 4 s `lift off` stage, and code 17 stored after 5.1 s of fuel cut held
    // at 3000 rpm. So in this script nothing is stored (docs/CHECK.md).
    // VTEC never engages without a speed signal.
    let (result, report, bench) = one_change("speed_sensor", "no");
    assert_eq!(report.verdict, Verdict::WorthBurning, "{report:#?}");
    assert_eq!(result.lamp, Lamp::Steady);
    assert_eq!(stored_codes(&bench), Vec::<u8>::new());
    assert_eq!(stage(&result, "full throttle").vtec_on_rpm, None);
}

#[test]
#[cfg_attr(not(feature = "p28-rom"), ignore = "needs the stock P28 ROM: enable feature p28-rom")]
fn case_6_values_the_model_cannot_represent_could_not_tell() {
    // One run for every value wiring::apply_static reports as unmodelled,
    // so as not to spend a 20 s run on each. None of them changes the board
    // except `o2_sensor = none`, whose missing heater is modelled: as with a
    // one-wire sensor the ROM stores the heater fault and blinks 41, and that
    // problem outranks the car line's could-not-tell.
    let mut setup = preset("stock-p28-manual");
    let values = [
        ("ecu", "p30"),
        ("knock_sensor", "yes"),
        ("o2_sensor", "none"),
        ("iacv", "9.5"),
        ("egr", "yes"),
        ("power_steering_switch", "no"),
    ];
    for (key, value) in values {
        setup.set(key, value).unwrap();
    }
    let (result, report, bench) = check_bench(Bench::from_file(&rom()).unwrap(), &setup);
    let car = &report.runs.lines[0];
    assert_eq!(car.label, "car as described");
    assert_eq!(car.state, State::CouldNotTell);
    for (key, value) in values {
        assert!(car.text.contains(&format!("{key} = {value}")), "{key}: {}", car.text);
    }
    assert!(matches!(&report.verdict, Verdict::DoNotBurn(_)), "{report:#?}");
    for l in &report.runs.lines[1..] {
        let want = if matches!(l.label, "check-engine light" | "codes") { State::Problem } else { State::Ok };
        assert_eq!(l.state, want, "{}: {}", l.label, l.text);
    }
    assert_eq!(result.lamp, Lamp::Codes(vec![41]));
    assert_eq!(stored_codes(&bench), vec![27]);
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
fn case_7_gearbox_mismatch() {
    // docs/CHECK.md, "Gearbox mismatch".
    //
    // Automatic board, manual car: code 19, as in Honda practice. With no
    // solenoids the RM11 pull-ups hold both lock-up driver outputs high
    // (Bomarc sheet 8), so both feedbacks read "powered" (port A bit 4 = 0,
    // 4700h bit 3 = 0) while the ROM keeps both drivers off: no position
    // switch, so 211h.5/6/7 read 0 and the lock-up routine (0x32A8: P/N,
    // 0x32AB: D3, 0x32AE: D4) never commands lock-up (PB4 low, 381h 0). The
    // checks that compare each feedback with that off command,
    // 0x3C98..0x3CA5 (99h.6) and 0x3CAF..0x3CDD (9Ch.7), both code 19 in the
    // table at 6C99h, fail. Once code 19 is stored, 214h.2 closes both
    // checks (0x3C95, 0x5F5F).
    //
    // Manual board, automatic car: 216h.3 is clear, so 0x3C92 and 0x5F5C skip
    // both lock-up checks. The position inputs reach the ROM; observed, lock-up
    // A is never engaged and nothing is stored.
    for (board, car) in [("automatic", "manual"), ("manual", "automatic")] {
        let mut setup = preset("stock-p28-manual");
        setup.set("board_gearbox", board).unwrap();
        setup.set("gearbox", car).unwrap();
        let (result, report, bench) = check_bench(Bench::from_file(&rom()).unwrap(), &setup);
        let case = format!("board {board}, car {car}");
        let lockup_engaged = bench.log.board.iter().any(|&(_, b, _)| b & P28_PB_LOCKUP_SOLENOID != 0);
        assert!(!lockup_engaged, "{case}");
        // Lock-up B's duty (381h, the compare value of the software PWM on
        // P4.3 at 0x0337) is 0 at the end: solenoid B is not being driven.
        // Read at the end only; the script gives no per-instruction view.
        assert_eq!(bench.bus.ram[0x381], 0, "{case}: 381h");
        if board == "automatic" {
            assert!(
                matches!(&report.verdict, Verdict::DoNotBurn(why) if why.starts_with("codes: 19")),
                "{case}: {report:#?}"
            );
            assert_eq!(result.lamp, Lamp::Codes(vec![19]), "{case}");
            assert_eq!(stored_codes(&bench), vec![19], "{case}");
            // 216h.3 set: the ROM ran as an automatic.
            assert_ne!(bench.bus.ram[0x216] & 0x08, 0, "{case}: 216h.3");
        } else {
            assert_eq!(report.verdict, Verdict::WorthBurning, "{case}: {report:#?}");
            assert!(report.codes.is_empty(), "{case}");
            assert_eq!(result.lamp, Lamp::Steady, "{case}");
            assert_eq!(stored_codes(&bench), Vec::<u8>::new(), "{case}");
            assert_eq!(bench.bus.ram[0x216] & 0x08, 0, "{case}: 216h.3");
        }
    }
}
