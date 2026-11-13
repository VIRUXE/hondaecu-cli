//! Turns what a scripted run observed into three-state lines and one verdict.

use crate::bench::{Settling, Stop};
use crate::blink::Lamp;
use crate::car::Setup;
use crate::dtc::code_name;
use crate::script::{RunResult, StageResult};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum State {
    Ok,
    Problem,
    CouldNotTell,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub label: &'static str,
    pub state: State,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub title: &'static str,
    pub state: State,
    pub lines: Vec<Line>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    WorthBurning,
    /// The first problem, as "<label>: <text>".
    DoNotBurn(String),
    /// The first thing that could not be told, as "<label>: <text>".
    CouldNotTell(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub verdict: Verdict,
    pub valid: Section,
    pub runs: Section,
    /// The fault codes the lamp blinked; empty when it blinked none.
    pub codes: Vec<u8>,
    pub not_judged: &'static [&'static str],
}

impl Report {
    /// 0 worth burning, 1 do not burn, 3 could not tell.
    pub fn exit_status(&self) -> i32 {
        match self.verdict {
            Verdict::WorthBurning => 0,
            Verdict::DoNotBurn(_) => 1,
            Verdict::CouldNotTell(_) => 3,
        }
    }
}

const NOT_JUDGED: &[&str] = &["fuelling", "ignition timing", "anything that needs a responding engine"];
const PUMP_PRIME_SECONDS: (f64, f64) = (0.5, 5.0);
const OPENINGS_TOLERANCE: f64 = 2.0;
const IGNITIONS_PER_CYCLE: f64 = 3.0;
const SWEEP_TOP_RPM: f64 = 9000.0;
/// Fewer engine cycles than this in the warm idle stage say nothing about it.
pub const MIN_IDLE_ENGINE_CYCLES: f64 = 10.0;
const READ_OUT_STAGE: &str = "code read-out";

/// The run got through the fault-code read-out without stopping early.
fn reached_read_out(result: &RunResult) -> bool {
    result.stop.is_none() && stage(result, READ_OUT_STAGE).is_some()
}

fn line(label: &'static str, state: State, text: impl Into<String>) -> Line {
    Line { label, state, text: text.into() }
}

fn ok(label: &'static str, text: impl Into<String>) -> Line {
    line(label, State::Ok, text)
}

fn problem(label: &'static str, text: impl Into<String>) -> Line {
    line(label, State::Problem, text)
}

fn could_not_tell(label: &'static str, text: impl Into<String>) -> Line {
    line(label, State::CouldNotTell, text)
}

fn section(title: &'static str, lines: Vec<Line>) -> Section {
    let state = if lines.iter().any(|l| l.state == State::Problem) {
        State::Problem
    } else if lines.iter().any(|l| l.state == State::CouldNotTell) {
        State::CouldNotTell
    } else {
        State::Ok
    };
    Section { title, state, lines }
}

fn stage<'a>(result: &'a RunResult, name: &str) -> Option<&'a StageResult> {
    result.stages.iter().find(|s| s.name == name)
}

/// The line for a stage-based check, or "could not tell" when the stage never ran.
fn staged(
    result: &RunResult,
    label: &'static str,
    stage_name: &str,
    check: impl FnOnce(&StageResult) -> Line,
) -> Line {
    match stage(result, stage_name) {
        Some(s) => check(s),
        None => could_not_tell(label, "the run stopped before this"),
    }
}

fn executes(result: &RunResult) -> Line {
    match &result.stop {
        None => ok("executes", "ran without hitting an invalid instruction"),
        Some((t, s @ Stop::UndefinedOpcode { .. })) => problem("executes", format!("{s} at {t:.1} s")),
        Some((t, s @ Stop::Unimplemented { .. })) => {
            could_not_tell("executes", format!("{s} at {t:.1} s: the emulator cannot go on"))
        }
    }
}

fn settles(result: &RunResult) -> Line {
    const L: &str = "settles after power-on";
    match &result.settling {
        Settling::Settled { boot_breaks: 0 } => ok(L, "settled, no resets"),
        Settling::Settled { boot_breaks } => ok(L, format!("settled after {boot_breaks} resets during start-up")),
        Settling::NeverSettles { breaks, last_pc } => {
            problem(L, format!("never settles: {breaks} resets in the first second (last at program address {last_pc:X}h)"))
        }
        Settling::ResetsLater { times, pc } => {
            let at: Vec<String> = times.iter().map(|t| format!("{t:.1} s")).collect();
            problem(L, format!("resets at {} (program address {pc:X}h)", at.join(", ")))
        }
        Settling::Hung { since } => problem(L, format!("stopped responding at {since:.1} s (watchdog not serviced)")),
        Settling::Stopped => could_not_tell(L, "the run stopped before this could be judged"),
    }
}

fn own_board(result: &RunResult) -> Line {
    const L: &str = "talks only to this board";
    if !result.board_programmed {
        return problem(L, "the board was never programmed");
    }
    let (reads, writes, last) = result.unmapped;
    if reads + writes > 0 {
        let at = last.map(|a| format!(", last at {a:04X}h")).unwrap_or_default();
        return could_not_tell(L, format!("{reads} unmapped reads and {writes} unmapped writes{at}"));
    }
    if result.ppi_unsupported_mode {
        return could_not_tell(L, "the 82C55 was put in a mode the emulator does not support");
    }
    ok(L, "no access outside the board")
}

fn fuel_pump(result: &RunResult) -> Line {
    staged(result, "fuel pump prime", "key on", |s| {
        let on = s.fuel_pump_on_seconds;
        let (lo, hi) = PUMP_PRIME_SECONDS;
        if on >= lo && on <= hi && on < s.to - s.from {
            ok("fuel pump prime", format!("on for {on:.1} s, then off"))
        } else {
            problem("fuel pump prime", format!("on for {on:.1} s of {:.1} s (wanted {lo} to {hi} s, then off)", s.to - s.from))
        }
    })
}

fn cranking(result: &RunResult) -> Line {
    staged(result, "cranking", "cranking", |s| {
        if s.engine_cycles.is_nan() || s.engine_cycles == 0.0 {
            return could_not_tell("cranking", "the engine did not turn in this stage");
        }
        let silent: Vec<String> = s
            .injector_opens
            .iter()
            .enumerate()
            .filter(|(_, n)| **n == 0)
            .map(|(i, _)| (i + 1).to_string())
            .collect();
        if silent.is_empty() {
            ok("cranking", format!("all four injectors fired ({} openings)", counts(&s.injector_opens)))
        } else {
            problem("cranking", format!("{} never fired while cranking", injectors(&silent)))
        }
    })
}

/// "4, 4, 4, 4".
fn counts(opens: &[u64]) -> String {
    opens.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(", ")
}

/// "injector 2" or "injectors 2, 3" from 1-based numbers.
fn injectors(numbers: &[String]) -> String {
    let word = if numbers.len() == 1 { "injector" } else { "injectors" };
    format!("{word} {}", numbers.join(", "))
}

fn warm_idle(result: &RunResult) -> Line {
    staged(result, "warm idle", "warm idle", |s| {
        let cycles = s.engine_cycles;
        if cycles.is_nan() || cycles < MIN_IDLE_ENGINE_CYCLES {
            return could_not_tell("warm idle", format!("the engine turned only {cycles:.1} cycles in this stage"));
        }
        for (i, n) in s.injector_opens.iter().enumerate() {
            if *n == 0 || (*n as f64 - cycles).abs() > OPENINGS_TOLERANCE {
                return problem(
                    "warm idle",
                    format!("injector {} opened {n} times in {cycles:.1} engine cycles", i + 1),
                );
            }
        }
        if s.ignitions == 0 || (s.ignitions as f64) < IGNITIONS_PER_CYCLE * cycles {
            return problem(
                "warm idle",
                format!("{} sparks in {cycles:.1} engine cycles (wanted at least {IGNITIONS_PER_CYCLE} per cycle)", s.ignitions),
            );
        }
        ok(
            "warm idle",
            format!("injectors opened {} times and {} sparks in {cycles:.1} engine cycles", counts(&s.injector_opens), s.ignitions),
        )
    })
}

fn ac_idle(result: &RunResult) -> Option<Line> {
    stage(result, "idle, A/C on").map(|s| {
        if s.ac_clutch_seen {
            ok("idle, A/C on", "the A/C clutch was engaged")
        } else {
            problem("idle, A/C on", "the A/C clutch was never engaged")
        }
    })
}

fn sweep(result: &RunResult) -> Line {
    staged(result, "full throttle sweep", "full throttle", |s| {
        let fuel = match s.fuel_stops_rpm {
            Some(r) => format!("fuel stops at {r:.0} rpm"),
            None => format!("fuel never stopped below {SWEEP_TOP_RPM:.0} rpm"),
        };
        let vtec = match s.vtec_on_rpm {
            Some(r) => format!("VTEC switches on at {r:.0} rpm"),
            None => "VTEC never switched on".to_string(),
        };
        ok("full throttle sweep", format!("{fuel}, {vtec}"))
    })
}

fn check_engine_light(result: &RunResult) -> Line {
    match result.mil_first_lit_after_bulb_check {
        Some(t) => problem("check-engine light", format!("lit at {t:.1} s, after the bulb check")),
        None if !reached_read_out(result) => could_not_tell("check-engine light", "the run stopped before this"),
        None => ok("check-engine light", "never lit after the bulb check"),
    }
}

fn codes(result: &RunResult) -> Line {
    match &result.lamp {
        Lamp::Off | Lamp::Steady if !reached_read_out(result) => {
            could_not_tell("codes", "the run stopped before this")
        }
        Lamp::Codes(v) if v.is_empty() => {
            could_not_tell("codes", "the lamp blinked but no code could be read")
        }
        Lamp::Off => ok("codes", "none"),
        Lamp::Steady => ok("codes", "none (lamp steady with the connector grounded: no code stored)"),
        Lamp::Codes(v) => {
            let each: Vec<String> = v
                .iter()
                .map(|n| match code_name(*n) {
                    Some(name) => format!("{n} {name}"),
                    None => n.to_string(),
                })
                .collect();
            problem("codes", each.join(", "))
        }
        Lamp::Unreadable => could_not_tell("codes", "the lamp changed but no complete code could be read"),
    }
}

/// "Could not tell" naming every car value the board model could not
/// represent, or nothing when there is none. Never ok: having modelled the car
/// is not something the ROM was observed to do.
fn car_as_described(result: &RunResult) -> Option<Line> {
    if result.unmodelled.is_empty() {
        return None;
    }
    let each: Vec<String> = result.unmodelled.iter().map(|(key, why)| format!("{key} = {why}")).collect();
    Some(could_not_tell(
        "car as described",
        format!("not modelled, so the run is not of this car: {}", each.join("; ")),
    ))
}

pub fn judge(result: &RunResult, _setup: &Setup) -> Report {
    let valid = section(
        "VALID FOR THIS ECU",
        vec![executes(result), settles(result), own_board(result)],
    );
    let mut run_lines: Vec<Line> = car_as_described(result).into_iter().collect();
    run_lines.extend([fuel_pump(result), cranking(result), warm_idle(result)]);
    run_lines.extend(ac_idle(result));
    run_lines.extend([sweep(result), check_engine_light(result), codes(result)]);
    let runs = section("RUNS A BASIC SETUP", run_lines);

    let all = || valid.lines.iter().chain(runs.lines.iter());
    let reason = |l: &Line| format!("{}: {}", l.label, l.text);
    let verdict = if let Some(l) = all().find(|l| l.state == State::Problem) {
        Verdict::DoNotBurn(reason(l))
    } else if let Some(l) = all().find(|l| l.state == State::CouldNotTell) {
        Verdict::CouldNotTell(reason(l))
    } else {
        Verdict::WorthBurning
    };
    let codes = match &result.lamp {
        Lamp::Codes(v) => v.clone(),
        _ => Vec::new(),
    };
    Report { verdict, valid, runs, codes, not_judged: NOT_JUDGED }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage(name: &'static str, from: f64, to: f64) -> StageResult {
        StageResult {
            name,
            from,
            to,
            engine_cycles: 0.0,
            injector_opens: [0; 4],
            pulse_us: [0; 4],
            ignitions: 0,
            fuel_pump_on_seconds: 0.0,
            mil_lit_at_end: false,
            ac_clutch_seen: false,
            iacv_duty_pct: 0.0,
            fuel_stops_rpm: None,
            vtec_on_rpm: None,
        }
    }

    fn clean() -> RunResult {
        let mut key_on = stage("key on", 0.0, 6.0);
        key_on.fuel_pump_on_seconds = 2.0;
        let mut cranking = stage("cranking", 6.0, 7.5);
        cranking.engine_cycles = 3.75;
        cranking.injector_opens = [3, 3, 3, 3];
        let mut warm = stage("warm idle", 10.0, 18.0);
        warm.engine_cycles = 53.3;
        warm.injector_opens = [53, 54, 53, 52];
        warm.ignitions = 213;
        let mut ac = stage("idle, A/C on", 18.0, 22.0);
        ac.ac_clutch_seen = true;
        let mut full = stage("full throttle", 26.0, 40.0);
        full.fuel_stops_rpm = Some(7420.0);
        full.vtec_on_rpm = Some(4875.0);
        RunResult {
            stages: vec![key_on, cranking, warm, ac, full, stage("code read-out", 50.0, 90.0)],
            settling: Settling::Settled { boot_breaks: 0 },
            board_programmed: true,
            stop: None,
            unmapped: (0, 0, None),
            ppi_unsupported_mode: false,
            mil_first_lit_after_bulb_check: None,
            lamp: Lamp::Off,
            seconds: 90.0,
            unmodelled: Vec::new(),
        }
    }

    #[test]
    fn an_unmodelled_car_value_is_the_first_runs_line_and_could_not_tell() {
        let mut c = clean();
        c.unmodelled = vec![
            ("eld", "no: the level is not established".to_string()),
            ("egr", "yes: not established".to_string()),
        ];
        let r = judge(&c, &setup());
        let first = &r.runs.lines[0];
        assert_eq!(first.label, "car as described");
        assert_eq!(first.state, State::CouldNotTell);
        assert!(first.text.contains("eld = no: the level is not established"), "{}", first.text);
        assert!(first.text.contains("egr = yes: not established"), "{}", first.text);
        assert_eq!(r.runs.state, State::CouldNotTell);
        assert_eq!(r.exit_status(), 3);
        match &r.verdict {
            Verdict::CouldNotTell(why) => assert!(why.starts_with("car as described: "), "{why}"),
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn a_fully_modelled_car_has_no_car_line() {
        let r = judge(&clean(), &setup());
        assert!(r.runs.lines.iter().all(|l| l.label != "car as described"));
        assert_eq!(r.runs.lines[0].label, "fuel pump prime");
    }

    #[test]
    fn a_problem_still_outranks_an_unmodelled_car() {
        let mut c = clean();
        c.unmodelled = vec![("eld", "no: not established".to_string())];
        c.lamp = Lamp::Codes(vec![20]);
        let r = judge(&c, &setup());
        assert_eq!(r.exit_status(), 1);
        assert!(matches!(&r.verdict, Verdict::DoNotBurn(why) if why.starts_with("codes: ")));
    }

    fn setup() -> Setup {
        Setup::preset("stock-p28-manual").unwrap()
    }

    fn find<'a>(r: &'a Report, label: &str) -> &'a Line {
        r.valid
            .lines
            .iter()
            .chain(r.runs.lines.iter())
            .find(|l| l.label == label)
            .unwrap_or_else(|| panic!("no line {label}"))
    }

    fn stage_mut<'a>(r: &'a mut RunResult, name: &str) -> &'a mut StageResult {
        r.stages.iter_mut().find(|s| s.name == name).unwrap()
    }

    #[test]
    fn a_clean_run_is_worth_burning() {
        let r = judge(&clean(), &setup());
        assert_eq!(r.verdict, Verdict::WorthBurning);
        assert_eq!(r.exit_status(), 0);
        assert_eq!(r.valid.state, State::Ok);
        assert_eq!(r.runs.state, State::Ok);
        assert_eq!(r.valid.title, "VALID FOR THIS ECU");
        assert_eq!(r.runs.title, "RUNS A BASIC SETUP");
        assert_eq!(
            r.not_judged,
            &["fuelling", "ignition timing", "anything that needs a responding engine"]
        );
        assert!(r.codes.is_empty());
    }

    #[test]
    fn undefined_opcode_is_a_problem_and_unimplemented_could_not_tell() {
        let mut c = clean();
        c.stop = Some((3.0, Stop::UndefinedOpcode { pc: 0x1234, byte: 0xFF }));
        let r = judge(&c, &setup());
        assert_eq!(find(&r, "executes").state, State::Problem);
        assert_eq!(r.valid.state, State::Problem);
        assert_eq!(r.exit_status(), 1);

        let mut c = clean();
        c.stop = Some((3.0, Stop::Unimplemented { pc: 0x1234, mnemonic: "DAA" }));
        let r = judge(&c, &setup());
        assert_eq!(find(&r, "executes").state, State::CouldNotTell);
        assert!(find(&r, "executes").text.contains("DAA"));
        assert_eq!(r.valid.state, State::CouldNotTell);
    }

    #[test]
    fn an_emulator_gap_is_could_not_tell_not_a_pass() {
        let mut c = clean();
        c.stop = Some((3.0, Stop::Unimplemented { pc: 0x1234, mnemonic: "DAA" }));
        let r = judge(&c, &setup());
        assert_eq!(r.exit_status(), 3);
        assert!(matches!(r.verdict, Verdict::CouldNotTell(_)));
    }

    #[test]
    fn settling_problems_and_stopped() {
        let cases = [
            Settling::NeverSettles { breaks: 9, last_pc: 0x21DF },
            Settling::ResetsLater { times: vec![5.3, 10.6], pc: 0x21DF },
            Settling::Hung { since: 4.0 },
        ];
        for s in cases {
            let mut c = clean();
            c.settling = s;
            let r = judge(&c, &setup());
            assert_eq!(find(&r, "settles after power-on").state, State::Problem);
            assert_eq!(r.exit_status(), 1);
        }
        let mut c = clean();
        c.settling = Settling::ResetsLater { times: vec![5.3, 10.6], pc: 0x21DF };
        let r = judge(&c, &setup());
        assert_eq!(find(&r, "settles after power-on").text, "resets at 5.3 s, 10.6 s (program address 21DFh)");

        let mut c = clean();
        c.settling = Settling::Stopped;
        let r = judge(&c, &setup());
        assert_eq!(find(&r, "settles after power-on").state, State::CouldNotTell);
        assert_eq!(r.exit_status(), 3);
    }

    #[test]
    fn board_and_bus_lines() {
        let mut c = clean();
        c.board_programmed = false;
        let r = judge(&c, &setup());
        assert_eq!(find(&r, "talks only to this board").state, State::Problem);
        assert_eq!(r.exit_status(), 1);

        let mut c = clean();
        c.unmapped = (2, 1, Some(0x9000));
        let r = judge(&c, &setup());
        let l = find(&r, "talks only to this board");
        assert_eq!(l.state, State::CouldNotTell);
        assert!(l.text.contains("9000"));
        assert_eq!(r.exit_status(), 3);

        let mut c = clean();
        c.ppi_unsupported_mode = true;
        let r = judge(&c, &setup());
        assert_eq!(find(&r, "talks only to this board").state, State::CouldNotTell);
    }

    #[test]
    fn fuel_pump_prime() {
        for (secs, ok) in [(0.2, false), (0.5, true), (5.0, true), (6.0, false)] {
            let mut c = clean();
            stage_mut(&mut c, "key on").fuel_pump_on_seconds = secs;
            let r = judge(&c, &setup());
            let want = if ok { State::Ok } else { State::Problem };
            assert_eq!(find(&r, "fuel pump prime").state, want, "{secs}");
            assert_eq!(r.exit_status(), if ok { 0 } else { 1 });
        }
    }

    #[test]
    fn the_ok_texts_are_plain_words_with_every_number() {
        let r = judge(&clean(), &setup());
        assert_eq!(find(&r, "executes").text, "ran without hitting an invalid instruction");
        assert_eq!(find(&r, "settles after power-on").text, "settled, no resets");
        assert_eq!(find(&r, "cranking").text, "all four injectors fired (3, 3, 3, 3 openings)");
        assert_eq!(
            find(&r, "warm idle").text,
            "injectors opened 53, 54, 53, 52 times and 213 sparks in 53.3 engine cycles"
        );
        let mut c = clean();
        c.settling = Settling::Settled { boot_breaks: 2 };
        assert_eq!(find(&judge(&c, &setup()), "settles after power-on").text, "settled after 2 resets during start-up");
        c.settling = Settling::Hung { since: 3.2 };
        assert_eq!(
            find(&judge(&c, &setup()), "settles after power-on").text,
            "stopped responding at 3.2 s (watchdog not serviced)"
        );
    }

    #[test]
    fn cranking_needs_every_output() {
        let mut c = clean();
        stage_mut(&mut c, "cranking").injector_opens = [3, 0, 3, 3];
        let r = judge(&c, &setup());
        let l = find(&r, "cranking");
        assert_eq!(l.state, State::Problem);
        assert!(l.text.contains("injector 2"));
        assert_eq!(r.runs.state, State::Problem);
        assert_eq!(r.exit_status(), 1);
    }

    #[test]
    fn warm_idle_openings_and_ignitions() {
        let mut c = clean();
        stage_mut(&mut c, "warm idle").injector_opens = [53, 0, 53, 53];
        let r = judge(&c, &setup());
        assert_eq!(find(&r, "warm idle").state, State::Problem);
        assert_eq!(find(&r, "warm idle").text, "injector 2 opened 0 times in 53.3 engine cycles");

        let mut c = clean();
        stage_mut(&mut c, "warm idle").ignitions = 100;
        let r = judge(&c, &setup());
        assert_eq!(find(&r, "warm idle").state, State::Problem);
        assert!(find(&r, "warm idle").text.contains("100"));
        assert_eq!(r.exit_status(), 1);
    }

    #[test]
    fn ac_line_only_with_the_stage() {
        let mut c = clean();
        stage_mut(&mut c, "idle, A/C on").ac_clutch_seen = false;
        let r = judge(&c, &setup());
        assert_eq!(find(&r, "idle, A/C on").state, State::Problem);
        assert_eq!(r.exit_status(), 1);

        let mut c = clean();
        c.stages.retain(|s| s.name != "idle, A/C on");
        let r = judge(&c, &setup());
        assert!(r.runs.lines.iter().all(|l| l.label != "idle, A/C on"));
        assert_eq!(r.exit_status(), 0);
    }

    #[test]
    fn a_missing_stage_is_could_not_tell() {
        for (name, label) in [
            ("key on", "fuel pump prime"),
            ("cranking", "cranking"),
            ("warm idle", "warm idle"),
            ("full throttle", "full throttle sweep"),
        ] {
            let mut c = clean();
            c.stages.retain(|s| s.name != name);
            let r = judge(&c, &setup());
            let l = find(&r, label);
            assert_eq!(l.state, State::CouldNotTell, "{label}");
            assert_eq!(l.text, "the run stopped before this");
            assert_eq!(r.exit_status(), 3);
        }
    }

    #[test]
    fn full_throttle_sweep_texts() {
        let r = judge(&clean(), &setup());
        let l = find(&r, "full throttle sweep");
        assert_eq!(l.state, State::Ok);
        assert_eq!(l.text, "fuel stops at 7420 rpm, VTEC switches on at 4875 rpm");

        let mut c = clean();
        let f = stage_mut(&mut c, "full throttle");
        f.fuel_stops_rpm = None;
        f.vtec_on_rpm = None;
        let r = judge(&c, &setup());
        let l = find(&r, "full throttle sweep");
        assert_eq!(l.state, State::Ok);
        assert_eq!(l.text, "fuel never stopped below 9000 rpm, VTEC never switched on");
        assert_eq!(r.exit_status(), 0);
    }

    #[test]
    fn check_engine_light() {
        let mut c = clean();
        c.mil_first_lit_after_bulb_check = Some(12.5);
        let r = judge(&c, &setup());
        assert_eq!(find(&r, "check-engine light").state, State::Problem);
        assert!(find(&r, "check-engine light").text.contains("12.5"));
        assert_eq!(r.exit_status(), 1);
    }

    #[test]
    fn codes_line() {
        let r = judge(&clean(), &setup());
        assert_eq!(find(&r, "codes").state, State::Ok);
        assert_eq!(find(&r, "codes").text, "none");

        let mut c = clean();
        c.lamp = Lamp::Steady;
        let r = judge(&c, &setup());
        assert_eq!(find(&r, "codes").state, State::Ok);
        assert_eq!(
            find(&r, "codes").text,
            "none (lamp steady with the connector grounded: no code stored)"
        );
        assert_eq!(r.exit_status(), 0);

        let mut c = clean();
        c.lamp = Lamp::Codes(vec![23, 200]);
        let r = judge(&c, &setup());
        let l = find(&r, "codes");
        assert_eq!(l.state, State::Problem);
        assert!(l.text.contains("23"));
        assert!(l.text.contains(code_name(23).unwrap()));
        assert!(l.text.contains("200"));
        assert_eq!(r.codes, vec![23, 200]);
        assert_eq!(r.exit_status(), 1);

        let mut c = clean();
        c.lamp = Lamp::Unreadable;
        let r = judge(&c, &setup());
        assert_eq!(find(&r, "codes").state, State::CouldNotTell);
        assert_eq!(r.exit_status(), 3);
    }

    #[test]
    fn a_problem_outranks_could_not_tell() {
        let mut c = clean();
        c.unmapped = (1, 0, Some(0x9000));
        c.lamp = Lamp::Codes(vec![23]);
        let r = judge(&c, &setup());
        assert_eq!(r.exit_status(), 1);
        assert_eq!(r.valid.state, State::CouldNotTell);
        assert_eq!(r.runs.state, State::Problem);
        match &r.verdict {
            Verdict::DoNotBurn(why) => {
                assert!(why.starts_with("codes: "));
                assert!(why.contains("23"));
            }
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn a_dead_warm_idle_is_not_ok() {
        let mut c = clean();
        let w = stage_mut(&mut c, "warm idle");
        w.engine_cycles = 0.0;
        w.injector_opens = [0; 4];
        w.ignitions = 0;
        let r = judge(&c, &setup());
        assert_eq!(find(&r, "warm idle").state, State::CouldNotTell);
        assert_eq!(r.exit_status(), 3);

        let mut c = clean();
        stage_mut(&mut c, "warm idle").engine_cycles = f64::NAN;
        let r = judge(&c, &setup());
        assert_eq!(find(&r, "warm idle").state, State::CouldNotTell);

        let mut c = clean();
        stage_mut(&mut c, "warm idle").injector_opens = [53, 53, 0, 53];
        let r = judge(&c, &setup());
        assert_eq!(find(&r, "warm idle").state, State::Problem);
        assert!(find(&r, "warm idle").text.contains("injector 3 opened 0 times"));

        let mut c = clean();
        stage_mut(&mut c, "warm idle").ignitions = 0;
        assert_eq!(find(&judge(&c, &setup()), "warm idle").state, State::Problem);
    }

    #[test]
    fn cranking_without_a_turning_engine_is_could_not_tell() {
        let mut c = clean();
        stage_mut(&mut c, "cranking").engine_cycles = 0.0;
        assert_eq!(find(&judge(&c, &setup()), "cranking").state, State::CouldNotTell);
    }

    #[test]
    fn an_early_stop_does_not_pass_the_lamp_lines() {
        let mut c = clean();
        c.stages.retain(|s| matches!(s.name, "key on" | "cranking" | "warm idle"));
        c.stop = Some((20.0, Stop::Unimplemented { pc: 0x1234, mnemonic: "DAA" }));
        let r = judge(&c, &setup());
        for label in ["check-engine light", "codes"] {
            assert_eq!(find(&r, label).state, State::CouldNotTell, "{label}");
            assert_eq!(find(&r, label).text, "the run stopped before this");
        }
        assert_ne!(r.exit_status(), 0);

        c.mil_first_lit_after_bulb_check = Some(12.0);
        let r = judge(&c, &setup());
        assert_eq!(find(&r, "check-engine light").state, State::Problem);
        assert_eq!(r.exit_status(), 1);
    }

    #[test]
    fn an_empty_code_list_is_could_not_tell() {
        let mut c = clean();
        c.lamp = Lamp::Codes(vec![]);
        let r = judge(&c, &setup());
        assert_eq!(find(&r, "codes").state, State::CouldNotTell);
        assert!(r.codes.is_empty());
    }

    #[test]
    fn code_name_looks_up_the_table() {
        assert!(code_name(1).is_some());
        assert_eq!(code_name(250), None);
    }
}
