//! Acceptance: the whole `check` pipeline (script, wiring, verdict) on the
//! stock P28-230 ROM, manual and automatic, and on the stock P30 mask-203
//! ROM, and on community images (HTS base, ERM baseline, P72, lyonnel).
//! Every test needs its ROM image (feature `p28-rom`, `p30-rom`, `hts-rom`,
//! `erm-rom`, `p72-rom` or `launch-rom`); each full run is about 20 s of
//! wall time.

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
fn case_6_no_speed_sensor_stores_code_17() {
    // The VSS check at 0x3C74..0x3C8B sets 99h.4 while no VSS period has
    // been measured (231h.3) during deceleration fuel cut (21Ch.4) below
    // rpm code B0h (0x3C7B); `lift off` is long enough for its debounce
    // (script::stages). Observed: the MIL lights 17.4 s into `lift off`
    // (about 2440 rpm), code 17 is stored (6C99h index 13 = 11h) and
    // blinked. VTEC never engages without a speed signal.
    let (result, report, bench) = one_change("speed_sensor", "no");
    assert!(
        matches!(&report.verdict, Verdict::DoNotBurn(why) if why.starts_with("check-engine light: lit at")),
        "{report:#?}"
    );
    let lift = stage(&result, "lift off");
    let lit = result.mil_first_lit_after_bulb_check.unwrap();
    assert!(lit > lift.from && lit < lift.to, "{lit} not in lift off");
    assert_eq!(result.lamp, Lamp::Codes(vec![17]));
    assert_eq!(report.codes, vec![17]);
    assert_eq!(stored_codes(&bench), vec![17]);
    assert_eq!(stage(&result, "full throttle").vtec_on_rpm, None);
}

#[test]
#[cfg_attr(not(feature = "p28-rom"), ignore = "needs the stock P28 ROM: enable feature p28-rom")]
fn case_6_no_vtec_stores_code_21() {
    // With the solenoid unplugged its feedback, port A bit 5, reads
    // "powered" (Bomarc sheet 8) while the ROM keeps PC0 low. The check at
    // 0x3D0C..0x3D21 (behind the VTEC option flag 216h.4, 0x3D06) compares
    // the two and sets 9Ah.0, code 21 (6C99h index 17 = 15h).
    // The fault is stored 1.2 s after power-on, inside the bulb check, so
    // the lamp never goes out: lit at the end of `key on`.
    let (result, report, bench) = one_change("vtec", "no");
    assert!(
        matches!(&report.verdict, Verdict::DoNotBurn(why) if why.starts_with("check-engine light: never went out after power-on")),
        "{report:#?}"
    );
    assert_eq!(result.mil_first_lit_after_bulb_check, Some(stage(&result, "key on").to));
    assert!(result.mil_never_went_out);
    assert_eq!(result.lamp, Lamp::Codes(vec![21]));
    assert_eq!(report.codes, vec![21]);
    assert_eq!(stored_codes(&bench), vec![21]);
    assert_eq!(stage(&result, "full throttle").vtec_on_rpm, None);
}

#[test]
#[cfg_attr(not(feature = "p28-rom"), ignore = "needs the stock P28 ROM: enable feature p28-rom")]
fn case_6_no_o2_sensor_stores_the_heater_code_and_is_unmodelled() {
    // The sensor's level is unmodelled, the missing heater is not: as with
    // a one-wire sensor, 0x2094..0x20CB sets 9Bh.2, stored bit 27, blinked
    // as 41. That problem outranks the car line's could-not-tell.
    let (result, report, bench) = one_change("o2_sensor", "none");
    let car = &report.runs.lines[0];
    assert_eq!((car.label, car.state), ("car as described", State::CouldNotTell));
    assert!(car.text.contains("o2_sensor = none"), "{}", car.text);
    assert!(matches!(&report.verdict, Verdict::DoNotBurn(_)), "{report:#?}");
    assert_eq!(result.lamp, Lamp::Codes(vec![41]));
    assert_eq!(stored_codes(&bench), vec![27]);
}

#[test]
#[cfg_attr(not(feature = "p28-rom"), ignore = "needs the stock P28 ROM: enable feature p28-rom")]
fn case_6_values_the_model_cannot_represent_could_not_tell() {
    // One run for the unmodelled values that leave the board as it is, so
    // as not to spend a 20 s run on each (`o2_sensor = none` has its own
    // run: its missing heater adds a code). The run is the stock one, so
    // the only line that is not ok is the car line: COULD NOT TELL, exit 3.
    let mut setup = preset("stock-p28-manual");
    let values = [
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
    assert!(
        matches!(&report.verdict, Verdict::CouldNotTell(why) if why.starts_with("car as described: ")),
        "{report:#?}"
    );
    assert_eq!(report.exit_status(), 3);
    assert!(report.valid.lines.iter().chain(&report.runs.lines[1..]).all(|l| l.state == State::Ok), "{report:#?}");
    assert_eq!(result.lamp, Lamp::Steady);
    assert_eq!(stored_codes(&bench), Vec::<u8>::new());
}

fn p30_rom() -> String {
    crate::fixtures::p30_rom_path()
}

/// `stock-p30` with `changes` applied, run on the stock P30 image and judged.
fn p30_check(changes: &[(&str, &str)]) -> (RunResult, Report, Bench) {
    let mut setup = preset("stock-p30");
    for (key, value) in changes {
        setup.set(key, value).unwrap();
    }
    check_bench(Bench::from_file(&p30_rom()).unwrap(), &setup)
}

/// The knock checks were live at the end of the run: option byte 6002h (FFh
/// in the stock P30 image) set 227h.6 at 0x556E, which opens the heartbeat
/// check at 0x27F7 and the code-23 check at 0x3D44. Read at the end only.
fn assert_knock_checks_enabled(bench: &Bench) {
    assert_ne!(bench.bus.ram[0x227] & 0x40, 0, "227h.6");
}

#[test]
#[cfg_attr(not(feature = "p30-rom"), ignore = "needs the stock P30 ROM: enable feature p30-rom")]
fn case_3_stock_p30_on_a_p30_box_is_worth_burning() {
    // docs/CPU_CORE.md, "P30 box"; the board is `knock.rs`.
    let (result, report, bench) = p30_check(&[]);
    assert_eq!(report.verdict, Verdict::WorthBurning, "{report:#?}");
    assert_eq!(result.lamp, Lamp::Steady);
    assert_eq!(stored_codes(&bench), Vec::<u8>::new());
    assert_knock_checks_enabled(&bench);
    let r = &bench.bus.ram;
    // The board's last frame was accepted (0x0CAA..0x0CCC: start bits and
    // parity; 233h.7 clear) and reported no sensor fault (232h.7 clear).
    assert_eq!(r[0x233] & 0x80, 0, "233h.7");
    assert_eq!(r[0x232] & 0x80, 0, "232h.7");
    // The heartbeat keeps the countdown at 2E8h from running out (0x27F7).
    assert_ne!(r[0x2E8], 0, "2E8h");
}

#[test]
#[cfg_attr(not(feature = "p30-rom"), ignore = "needs the stock P30 ROM: enable feature p30-rom")]
fn stock_p30_without_its_knock_sensor_stores_code_23() {
    // The board reports the missing sensor in frame bit 8 (`knock.rs`: the
    // only path to code 23 in the ROM, and code 23 is documented as covering
    // an unplugged sensor; an assumption, not an independent check). 0x0CD0
    // copies it to 232h.7; 0x3D44..0x3D61 sets 9Ah.2 with the board alive
    // (2E8h non-zero) and the frame good (233h.7 clear); 6C99h index 19 =
    // 17h, code 23. Observed: the MIL lights at 8.6 s, in `cold idle`, and
    // the read-out blinks 23. Matches Honda practice.
    let (result, report, bench) = p30_check(&[("knock_sensor", "no")]);
    assert!(
        matches!(&report.verdict, Verdict::DoNotBurn(why) if why.starts_with("check-engine light: lit at")),
        "{report:#?}"
    );
    assert_eq!(result.lamp, Lamp::Codes(vec![23]));
    assert_eq!(report.codes, vec![23]);
    assert_eq!(stored_codes(&bench), vec![23]);
    assert_knock_checks_enabled(&bench);
}

#[test]
#[cfg_attr(not(feature = "p30-rom"), ignore = "needs the stock P30 ROM: enable feature p30-rom")]
fn stock_p30_on_a_p28_box_holds_the_lamp_lit_and_blinks_nothing() {
    // No knock board: port A bit 2 never changes, so 0x27F7..0x2824 never
    // reloads 2E8h and sets 9Ah.3 (6C99h index 20 = 18h, code 24) from the
    // first pass; it is stored about 2 s after power-on (214h.7). Honda
    // practice expects code 23; the ROM stores code 24 and blinks nothing:
    // with 214h.7 set the read-out routine skips the blink sequence (0x3634)
    // and holds the lamp lit, so the read-out looks like the no-code display
    // (docs/CHECK.md, "The P30 box"). The lamp lit after the bulb check is
    // what makes this a problem, and it turns the quiet read-out into a
    // problem on the codes line too.
    let (result, report, bench) = p30_check(&[("ecu", "p28")]);
    assert!(
        matches!(&report.verdict, Verdict::DoNotBurn(why) if why.starts_with("check-engine light: lit at")),
        "{report:#?}"
    );
    // Observed: the bulb check ends at 2.11 s and the lamp lights again at
    // 2.21 s.
    let lit = result.mil_first_lit_after_bulb_check.unwrap();
    assert!((lit - 2.21).abs() < 0.05, "{lit}");
    assert!(!result.mil_never_went_out);
    assert_eq!(result.lamp, Lamp::Steady);
    assert!(report.codes.is_empty());
    let codes = report.runs.lines.iter().find(|l| l.label == "codes").unwrap();
    assert_eq!(codes.state, State::Problem);
    assert_eq!(
        codes.text,
        "none blinked, although the check-engine light is on (the ROM is showing a fault it does not blink)"
    );
    assert_eq!(stored_codes(&bench), vec![24]);
    assert_knock_checks_enabled(&bench);
}

fn hts_rom() -> String {
    crate::fixtures::hts_rom_path()
}

#[test]
#[cfg_attr(not(feature = "hts-rom"), ignore = "needs the HTS base ROM: enable feature hts-rom")]
fn hts_base_rom_runs() {
    // docs/CHECK.md, "HTS base ROM": the ROM fuels on the model. On this
    // fault-free run its launch control was seen to arm only as the engine
    // first turns (222h.3, set from power-on, not yet cleared), and it drops
    // below 2001 rpm; the script cranks at 300 rpm and idles before it goes
    // above 2000, so no stage meets it. 222h.3 also follows 218h.6 (some
    // stored faults) and 230h.6 (not traced), which would arm it at any
    // rpm; this fault-free script cannot show that.
    let (result, report, bench) = check_bench(Bench::from_file(&hts_rom()).unwrap(), &preset("stock-p28-manual"));
    assert_eq!(report.verdict, Verdict::WorthBurning, "{report:#?}");
    assert_eq!(result.lamp, Lamp::Steady);
    assert_eq!(stored_codes(&bench), Vec::<u8>::new());
    // Launch flag 409h.3 clear at the end (written at 0x5BF7 / 0x5BFB).
    assert_eq!(bench.bus.ram[0x409] & 0x08, 0, "409h.3");
}

/// The HTS base image (patched by `patch`) on a bench held at `rpm`, 90 kPa,
/// `tps` %, 88 C, standing still, from power-on; after `idle_first` seconds
/// at 800 rpm if that is non-zero. Returns the openings per output in the
/// last 2 s, and RAM 409h (launch flag) and E6h (spark cut, bit 0) at the end.
fn hts_bench(patch: &[(usize, u8)], idle_first: f64, rpm: f64, tps: f64) -> ([u64; 4], u8, u8) {
    let mut image = std::fs::read(hts_rom()).unwrap();
    image.truncate(32768);
    for &(addr, value) in patch {
        image[addr] = value;
    }
    let mut bench = Bench::new(&image);
    bench.engine.map_kpa = 90.0;
    bench.engine.tps_pct = tps;
    bench.engine.ect_celsius = 88.0;
    bench.engine.iat_celsius = 30.0;
    bench.engine.vbatt_volts = 14.2;
    bench.engine.speed_kmh = 0.0;
    if idle_first > 0.0 {
        bench.engine.rpm = 800.0;
        bench.engine.sync_sensors_to_bus(&mut bench.bus);
        assert!(bench.run(idle_first));
    }
    bench.engine.rpm = rpm;
    bench.engine.sync_sensors_to_bus(&mut bench.bus);
    let before = bench.bus.injector_open_count;
    assert!(bench.run(2.0));
    let after = bench.bus.injector_open_count;
    let opens = std::array::from_fn(|i| after[i] - before[i]);
    (opens, bench.bus.ram[0x409], bench.bus.ram[0xE6])
}

#[test]
#[cfg_attr(not(feature = "hts-rom"), ignore = "needs the HTS base ROM: enable feature hts-rom")]
fn hts_base_rom_launch_control_cuts_fuel_above_2000_rpm_from_a_standstill() {
    // docs/CHECK.md, "HTS base ROM". 0x5BBB..0x5BFB: with 6150h non-zero
    // (FFh) and 615Dh non-zero (FFh) the launch flag 409h.3 is set whenever
    // the routine runs with 222h.3 set, at any rpm (here: from power-on until
    // shortly after the engine starts turning; 222h.3 also follows 218h.6,
    // set by some stored faults, and 230h.6, not traced), and once set it
    // stays while the period
    // C4h is at most 3A9h (2001 rpm) and TPS D1h is at least 60h. 0x5B45
    // then makes 3A9h the rev limit (0x1880), and with 6126h 00h the limit
    // cuts fuel (0x189C..0x18A2, PSWL.5). An engine that is at 3000 rpm with
    // the throttle open from power-on, as `trace` spins it, never leaves the
    // launch limiter.
    // Observed: one opening per output in the run (the flag is first set
    // 0.06 s after power-on). The bound is <= 1, not 0 as `trace injcount`
    // prints, because this count starts at power-on: `trace` boots to the
    // runtime loop and runs 0.5 s more before it starts counting, so that
    // one opening falls outside its window.
    let (opens, flag, _) = hts_bench(&[], 0.0, 3000.0, 65.0);
    assert!(opens.iter().all(|&n| n <= 1), "{opens:?}");
    assert_ne!(flag & 0x08, 0, "409h.3");

    // Just above the limit (3A9h, 2001 rpm) it is held too: 2100 rpm.
    let (opens, flag, _) = hts_bench(&[], 0.0, 2100.0, 65.0);
    assert!(opens.iter().all(|&n| n <= 1), "{opens:?}");
    assert_ne!(flag & 0x08, 0, "409h.3 at 2100 rpm");

    // Just below the limit it fuels: 1900 rpm, flag dropped.
    let (opens, flag, _) = hts_bench(&[], 0.0, 1900.0, 65.0);
    assert!(opens.iter().all(|&n| n >= 25), "{opens:?}");
    assert_eq!(flag & 0x08, 0, "409h.3 at 1900 rpm");

    // Throttle under the D1h gate: the flag drops and it fuels at 3000.
    let (opens, flag, _) = hts_bench(&[], 0.0, 3000.0, 30.0);
    assert!(opens.iter().all(|&n| n >= 40), "{opens:?}");
    assert_eq!(flag & 0x08, 0, "409h.3 at 30 % TPS");

    // Started and idled first, the engine is not launching: it fuels at 3000.
    let (opens, flag, _) = hts_bench(&[], 2.0, 3000.0, 65.0);
    assert!(opens.iter().all(|&n| n >= 40), "{opens:?}");
    assert_eq!(flag & 0x08, 0, "409h.3 after idle");

    // 6126h non-zero: the limiter no longer cuts fuel, only spark (E6h.0,
    // set at 0x1898 with 6125h non-zero whatever 6126h is, keeps the
    // ignition compare from being armed at 0x019B / 0x01E9 / 0x0251), so the
    // injectors keep firing while the launch flag is still set. That is the
    // lyonnel ROM's value.
    let (opens, flag, spark_cut) = hts_bench(&[(0x6126, 0xFF)], 0.0, 3000.0, 65.0);
    assert!(opens.iter().all(|&n| n >= 40), "{opens:?}");
    assert_ne!(flag & 0x08, 0, "409h.3 with 6126h FFh");
    // 0x1889..0x1898: with 6125h FFh and 5F88h 00h, SB E6h.0 when over the
    // limit (carry set from the compare at 0x1880).
    assert_ne!(spark_cut & 0x01, 0, "E6h.0 with 6126h FFh");
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
            // Stored 0.9 s after power-on, inside the bulb check: the lamp
            // never goes out, lit at the end of `key on`.
            assert!(
                matches!(&report.verdict, Verdict::DoNotBurn(why) if why.starts_with("check-engine light: never went out after power-on")),
                "{case}: {report:#?}"
            );
            assert_eq!(result.mil_first_lit_after_bulb_check, Some(stage(&result, "key on").to), "{case}");
            assert!(result.mil_never_went_out, "{case}");
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

// The three community images `check` judged do-not-burn without a traced
// reason (docs/CHECK.md, "The three do-not-burn results, traced").

fn erm_rom() -> String {
    crate::fixtures::erm_rom_path()
}

/// The ERM image with its byte sum brought to 0 by one byte in the erased
/// fill at 0x6761..0x7C49: 0x7000, which no instruction in the listing
/// addresses (only the checksum reads it), from FFh to 06h, the sum being F9h.
fn erm_with_good_checksum() -> Vec<u8> {
    let mut image = std::fs::read(erm_rom()).unwrap();
    image.truncate(32768);
    let sum = image.iter().fold(0u8, |s, &b| s.wrapping_add(b));
    assert_eq!(sum, 0xF9, "byte sum of the ERM image as distributed");
    assert_eq!(image[0x7000], 0xFF);
    image[0x7000] = 0xFF_u8.wrapping_sub(sum); // 06h
    assert_eq!(image.iter().fold(0u8, |s, &b| s.wrapping_add(b)), 0);
    image
}

#[test]
#[cfg_attr(not(feature = "erm-rom"), ignore = "needs the UberData ERM baseline: enable feature erm-rom")]
fn erm_baseline_resets_on_its_own_checksum() {
    // The ROM sums its own 32 KB, 64 bytes per main-loop pass (0x29E4..0x2A00,
    // `LC A, [X1]` over code memory), 512 passes for the whole image. At the
    // end, a non-zero sum with option byte 547Eh at 00h (the pgmfi wiki's
    // P72 page: "Debug Mode", FFh enables) stores 48h in F5h (0x2A23) and
    // jumps to 0x237A: SB B7h.1, BRK at 0x237D. The image's bytes sum to F9h
    // and its 547Eh is 00h, so it resets after every full pass: on a real
    // chip as here. Observed: every 5.3 s.
    let image = std::fs::read(erm_rom()).unwrap();
    assert_eq!(image[0x547E], 0x00, "547Eh");
    let (result, report, bench) = check_bench(Bench::new(&image[..32768]), &preset("stock-p28-manual"));
    assert!(
        matches!(&report.verdict, Verdict::DoNotBurn(why) if why.starts_with("settles after power-on: resets at 5.3 s, 10.6 s")),
        "{report:#?}"
    );
    match &result.settling {
        crate::bench::Settling::ResetsLater { times, pc } => {
            assert_eq!(*pc, 0x237D);
            assert_eq!(times.len(), 20, "{times:?}");
            for w in times.windows(2) {
                assert!((w[1] - w[0] - 5.29).abs() < 0.05, "{times:?}");
            }
        }
        s => panic!("{s:?}"),
    }
    assert!(bench.log.breaks.iter().filter(|b| b.t > 1.0).all(|b| b.pc == 0x237D));

    // The reason byte: 0x2A23 (F5h = 48h, the checksum) runs just before the
    // first reset, and no other path into 0x2371 (44h watchdog interrupt,
    // 45h unused interrupt, 4Dh port check, 4Fh IE check) does.
    let mut bench = Bench::new(&image[..32768]);
    let setup = preset("stock-p28-manual");
    crate::wiring::apply_static(&mut bench.bus, &setup);
    crate::wiring::apply_engine(&mut bench.engine, &setup);
    bench.engine.rpm = 800.0;
    bench.engine.map_kpa = 35.0;
    bench.engine.ect_celsius = 88.0;
    bench.engine.sync_sensors_to_bus(&mut bench.bus);
    let mut entries = Vec::new();
    assert!(bench.run_observed(6.0, |b| {
        if [0x2367, 0x236D, 0x2A23, 0x2C0F, 0x366F].contains(&b.cpu.pc) {
            entries.push((b.seconds(), b.cpu.pc));
        }
    }));
    let first_reset = bench.log.breaks.iter().find(|b| b.t > 1.0).unwrap().t;
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].1, 0x2A23);
    assert!(first_reset > entries[0].0 && first_reset - entries[0].0 < 0.001, "{entries:?} {first_reset}");
}

#[test]
#[cfg_attr(not(feature = "erm-rom"), ignore = "needs the UberData ERM baseline: enable feature erm-rom")]
fn erm_baseline_with_a_good_checksum_needs_a_knock_board() {
    // With the sum corrected the resets stop and the A/C line passes (the
    // clutch was missed only because a reset fell in that stage). What is
    // left is the P72 code's knock check: option byte 5467h = FFh (the
    // wiki's P72 page: "Knock Sensor", enable=FF) sets 227h.6 (0x535E), and
    // 0x29A5..0x29D2 then wants the knock board's heartbeat on port A bit 2,
    // as the P30 does at 0x27F7. On a P28 box it never comes: B2h.3 is set,
    // fault bit 23 (214h.7) stored at about 2.2 s, and the read-out skips
    // the blink (0x3529 `JBS 214h.7`, the P30's 0x3634) and holds the lamp
    // lit. On the P30 box (P28 board plus knock board) it is worth burning.
    let image = erm_with_good_checksum();
    let (result, report, bench) = check_bench(Bench::new(&image), &preset("stock-p28-manual"));
    assert!(
        matches!(&report.verdict, Verdict::DoNotBurn(why) if why.starts_with("check-engine light: lit at")),
        "{report:#?}"
    );
    assert!(matches!(result.settling, crate::bench::Settling::Settled { .. }), "{:?}", result.settling);
    let ac = report.runs.lines.iter().find(|l| l.label == "idle, A/C on").unwrap();
    assert_eq!(ac.state, State::Ok);
    let lit = result.mil_first_lit_after_bulb_check.unwrap();
    assert!((lit - 2.2).abs() < 0.05, "{lit}");
    assert_eq!(result.lamp, Lamp::Steady);
    assert_eq!(stored_codes(&bench), vec![24]);
    assert_ne!(bench.bus.ram[0x227] & 0x40, 0, "227h.6");

    let (_, report, bench) = check_bench(Bench::new(&image), &preset("stock-p30"));
    assert_eq!(report.verdict, Verdict::WorthBurning, "{report:#?}");
    assert_eq!(stored_codes(&bench), Vec::<u8>::new());
}

fn p72_rom() -> String {
    crate::fixtures::p72_rom_path()
}

#[test]
#[cfg_attr(not(feature = "p72-rom"), ignore = "needs the P72 ROM: enable feature p72-rom")]
fn p72_needs_a_knock_board() {
    // The ERM baseline is this code (1579 bytes differ, calibration and
    // options); the P72 image sums to 0 and does not reset. Knock option
    // 5467h = FFh, so on a P28 box: the heartbeat check at 0x29A5..0x29D2
    // fails, fault bit 23 (214h.7) stored at about 2.2 s, no blink
    // (0x3529), lamp held lit. The P72 (Integra GS-R) is a knock-board box;
    // the model's P30 box carries the same board: worth burning with the
    // sensor, code 23 without (the board's frame bit 8, as for the P30).
    let rom = p72_rom();
    let image = std::fs::read(&rom).unwrap();
    assert_eq!(image[0x5467], 0xFF, "5467h");
    let (result, report, bench) = check_rom_bench(&rom, &preset("stock-p28-manual"));
    assert!(
        matches!(&report.verdict, Verdict::DoNotBurn(why) if why.starts_with("check-engine light: lit at")),
        "{report:#?}"
    );
    let lit = result.mil_first_lit_after_bulb_check.unwrap();
    assert!((lit - 2.2).abs() < 0.05, "{lit}");
    assert_eq!(result.lamp, Lamp::Steady);
    assert!(report.codes.is_empty());
    assert_eq!(stored_codes(&bench), vec![24]);
    assert_ne!(bench.bus.ram[0x227] & 0x40, 0, "227h.6");

    let (_, report, bench) = check_rom_bench(&rom, &preset("stock-p30"));
    assert_eq!(report.verdict, Verdict::WorthBurning, "{report:#?}");
    assert_eq!(stored_codes(&bench), Vec::<u8>::new());

    let mut setup = preset("stock-p30");
    setup.set("knock_sensor", "no").unwrap();
    let (result, _, bench) = check_rom_bench(&rom, &setup);
    assert_eq!(result.lamp, Lamp::Codes(vec![23]));
    assert_eq!(stored_codes(&bench), vec![23]);
}

fn check_rom_bench(path: &str, setup: &Setup) -> (RunResult, Report, Bench) {
    check_bench(Bench::from_file(path).unwrap_or_else(|e| panic!("{path}: {e}")), setup)
}

fn launch_rom() -> String {
    crate::fixtures::launch_rom_path()
}

#[test]
#[cfg_attr(not(feature = "launch-rom"), ignore = "needs the launch-control ROM: enable feature launch-rom")]
fn lyonnel_lamp_shows_two_displays_and_no_fault() {
    // The lyonnel ROM stores no fault. Its lamp (HTS code: 0x3DE1..0x3E0B
    // computes the lamp request into 222h.3, 0x5D91..0x5DC9 drives P1.4)
    // does two things the stock ROM does not:
    // - after the bulb check (2C6h counted down every 0.1 s, 0x4C04), while
    //   217h.5 is set (engine not running), 0x5A71..0x5A8B reloads 2C6h
    //   with 4 as many times as option byte 6020h says (04h here, 00h in the
    //   HTS base ROM): four 0.3 s pulses from 2.7 s;
    // - with option byte 6142h non-zero (FFh) and 6143h zero the lamp is a
    //   shift light: lit while the period C4h is below the word at 6144h
    //   (00DFh, 8408 rpm), observed lit from 39.4 s, the top of `full
    //   throttle`.
    // `check` reads the first as a fault ("lit at 2.7 s"), which is wrong for
    // this ROM: the verdict below is the tool's current reading, pinned so
    // that a change to the lamp rule shows up here (docs/CHECK.md).
    let rom = launch_rom();
    let image = std::fs::read(&rom).unwrap();
    assert_eq!((image[0x6020], image[0x6142], image[0x6143]), (0x04, 0xFF, 0x00));
    let (result, report, bench) = check_rom_bench(&rom, &preset("stock-p28-manual"));
    assert!(
        matches!(&report.verdict, Verdict::DoNotBurn(why) if why.starts_with("check-engine light: lit at 2.7 s")),
        "{report:#?}"
    );
    assert_eq!(stored_codes(&bench), Vec::<u8>::new());
    assert_eq!(result.lamp, Lamp::Steady);
    let edges = bench.mil_edges();
    let key_on = stage(&result, "key on");
    let lit_in_key_on = edges.iter().filter(|e| e.1 && e.0 > 1.0 && e.0 < key_on.to).count();
    assert_eq!(lit_in_key_on, 4, "{edges:?}");
    let full = stage(&result, "full throttle");
    let shift = edges.iter().find(|e| e.1 && e.0 > key_on.to).unwrap().0;
    assert!(shift > full.from + 12.0 && shift < full.to, "{edges:?}");

    // Both options off: nothing else lights the lamp, worth burning.
    let mut patched = image[..32768].to_vec();
    patched[0x6020] = 0x00;
    patched[0x6142] = 0x00;
    let (result, report, _) = check_bench(Bench::new(&patched), &preset("stock-p28-manual"));
    assert_eq!(report.verdict, Verdict::WorthBurning, "{report:#?}");
    assert_eq!(result.mil_first_lit_after_bulb_check, None);
}
