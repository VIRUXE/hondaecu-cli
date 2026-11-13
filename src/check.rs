//! The `check` command: run a ROM on the modelled ECU box wired to a described
//! car and print one verdict. `test` is an alias.

use std::io::{BufRead, Write};

use crate::bench::Bench;
use crate::car::{Setup, KEYS, PRESETS};
use crate::report;
use crate::script;
use crate::verdict;

pub const USAGE: &str = "\
Usage: hondaecu-cli check <rom.bin> [options]
  --ecu p28|p30                      ECU model (shorthand for --set ecu=...)
  --board-gearbox manual|automatic   gearbox the board is set up for (shorthand for --set board_gearbox=...)
  --car <preset|file>                start from a preset (stock-p28-manual, stock-p28-automatic, stock-p30) or a car file
  --set key=value                    set one car value; may be repeated
  --report <file>                    also write the result as JSON
  --no-prompt                        never ask questions; anything not given takes its default
Exit status: 0 worth burning, 1 do not burn, 2 bad command line or unreadable file, 3 could not tell.";

#[derive(Debug, Clone, PartialEq)]
pub struct Args {
    pub rom: String,
    pub car: Option<String>,
    pub sets: Vec<(String, String)>,
    pub report: Option<String>,
    pub no_prompt: bool,
}

/// `args[0]` is the command name (`check` or `test`) and is skipped.
pub fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut rom = None;
    let mut car = None;
    let mut sets = Vec::new();
    let mut report = None;
    let mut no_prompt = false;
    let mut it = args.iter().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |flag: &str| {
            it.next().cloned().ok_or_else(|| format!("{flag} needs a value"))
        };
        match arg.as_str() {
            "--ecu" => sets.push(("ecu".to_string(), value("--ecu")?)),
            "--board-gearbox" => sets.push(("board_gearbox".to_string(), value("--board-gearbox")?)),
            "--car" => car = Some(value("--car")?),
            "--report" => report = Some(value("--report")?),
            "--no-prompt" => no_prompt = true,
            "--set" => {
                let v = value("--set")?;
                let (k, val) = v
                    .split_once('=')
                    .ok_or_else(|| format!("--set needs key=value, got \"{v}\""))?;
                sets.push((k.to_string(), val.to_string()));
            }
            flag if flag.starts_with('-') => return Err(format!("unknown option \"{flag}\"")),
            path => {
                if rom.replace(path.to_string()).is_some() {
                    return Err(format!("unexpected extra argument \"{path}\""));
                }
            }
        }
    }
    let rom = rom.ok_or_else(|| "no ROM file given".to_string())?;
    Ok(Args { rom, car, sets, report, no_prompt })
}

/// What the preset would be when the user names none.
fn default_preset(args: &Args) -> &'static str {
    let p30 = args
        .sets
        .iter()
        .any(|(k, v)| k.trim().eq_ignore_ascii_case("ecu") && v.trim().eq_ignore_ascii_case("p30"));
    if p30 { PRESETS[2] } else { PRESETS[0] }
}

/// Preset, then the car file, then `--set` and the shorthands, in that order.
/// `chosen_preset` is the answer to the preset question, when it was asked.
fn build_setup(args: &Args, chosen_preset: Option<&str>) -> Result<Setup, String> {
    let named_preset = args
        .car
        .as_deref()
        .filter(|c| PRESETS.contains(&c.trim().to_ascii_lowercase().as_str()));
    let mut setup = Setup::preset(named_preset.or(chosen_preset).unwrap_or(default_preset(args)))?;
    if let (Some(path), None) = (&args.car, named_preset) {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read car file {path}: {e}"))?;
        // Windows Notepad saves UTF-8 with a byte-order mark.
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        setup.apply_file(text).map_err(|e| format!("car file {path}: {e}"))?;
    }
    for (key, value) in &args.sets {
        setup.set(key, value).map_err(|e| format!("--set {key}: {e}"))?;
    }
    Ok(setup)
}

/// A board that was not described is assumed to be set up for the car's gearbox.
fn settle_board_gearbox(setup: &mut Setup) {
    if !setup.given.contains("board_gearbox") {
        setup.board_gearbox = setup.gearbox.clone();
    }
}

/// The question shown for a key: its meaning in plain words and the accepted answers.
fn question(key: &str) -> &'static str {
    match key {
        "ecu" => "ECU model? (p28/p30)",
        "board_gearbox" => "Gearbox the ECU board is set up for? (manual/automatic)",
        "gearbox" => "Gearbox in the car? (manual/automatic)",
        "vtec" => "VTEC engine? (yes/no)",
        "knock_sensor" => "Knock sensor fitted? (yes/no)",
        "o2_sensor" => "Oxygen sensor? (none/one-wire/heated)",
        "iacv" => "Idle air control valve? (stock/none/resistance in ohms)",
        "egr" => "EGR valve lift sensor fitted? (yes/no)",
        "eld" => "Electrical load detector fitted? (yes/no)",
        "speed_sensor" => "Vehicle speed sensor connected? (yes/no)",
        "ac" => "Air conditioning fitted? (yes/no)",
        "power_steering_switch" => "Power steering pressure switch fitted? (yes/no)",
        _ => "?",
    }
}

const ATTEMPTS: usize = 3;

enum Reply {
    Answer(String),
    Default,
    EndOfInput,
}

/// Ask until `check` accepts the answer, at most `ATTEMPTS` times. An empty
/// line takes the default; so does running out of attempts.
fn prompt(
    input: &mut impl BufRead,
    out: &mut impl Write,
    text: &str,
    default: &str,
    check: &dyn Fn(&str) -> Result<(), String>,
) -> Reply {
    for attempt in 1..=ATTEMPTS {
        let _ = write!(out, "{text} [{default}]: ");
        let _ = out.flush();
        let mut line = String::new();
        if !matches!(input.read_line(&mut line), Ok(n) if n > 0) {
            let _ = writeln!(out);
            return Reply::EndOfInput;
        }
        let answer = line.trim();
        if answer.is_empty() {
            return Reply::Default;
        }
        match check(answer) {
            Ok(()) => return Reply::Answer(answer.to_string()),
            Err(why) if attempt < ATTEMPTS => {
                let _ = writeln!(out, "  {why}. Try again.");
            }
            Err(why) => {
                let _ = writeln!(out, "  {why}. Taking the default, {default}.");
            }
        }
    }
    Reply::Default
}

/// Ask for every key not yet given. EOF or an empty line takes the default; an
/// invalid answer is asked again, at most three times, then takes the default.
pub fn ask(setup: &mut Setup, input: &mut impl BufRead, out: &mut impl Write) {
    // The board's gearbox is asked last, after the car's, and defaults to it.
    let order = KEYS.iter().copied().filter(|k| *k != "board_gearbox").chain(["board_gearbox"]);
    for key in order {
        if setup.given.contains(key) {
            continue;
        }
        if key == "board_gearbox" {
            settle_board_gearbox(setup);
        }
        let default = setup.value_text(key);
        let probe = setup.clone();
        let check = |v: &str| probe.clone().set(key, v);
        match prompt(input, out, question(key), &default, &check) {
            Reply::Answer(v) => {
                let _ = setup.set(key, &v);
            }
            Reply::Default => {}
            Reply::EndOfInput => {
                let _ = writeln!(out, "Input ended; the remaining answers take their defaults.");
                return;
            }
        }
    }
}

fn ask_preset(args: &Args, input: &mut impl BufRead, out: &mut impl Write) -> Option<&'static str> {
    let default = default_preset(args);
    let text = format!("Start from which car? ({})", PRESETS.join("/"));
    let check = |v: &str| Setup::preset(v).map(|_| ());
    match prompt(input, out, &text, default, &check) {
        Reply::Answer(v) => PRESETS.iter().copied().find(|p| p.eq_ignore_ascii_case(&v)),
        _ => None,
    }
}

fn rom_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned())
}

/// The whole command. Returns the exit status.
pub fn run(
    args: &[String],
    input: &mut impl BufRead,
    interactive: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> i32 {
    if args.iter().skip(1).any(|a| a == "--help" || a == "-h") {
        let _ = writeln!(out, "{USAGE}");
        return 0;
    }
    let args = match parse_args(args) {
        Ok(a) => a,
        Err(e) => {
            let _ = writeln!(err, "{e}\n{USAGE}");
            return 2;
        }
    };
    // Opened and size-checked before any question is asked.
    let mut bench = match Bench::from_file(&args.rom) {
        Ok(b) => b,
        Err(e) => {
            let _ = writeln!(err, "cannot use ROM {}: {e}", args.rom);
            return 2;
        }
    };
    let interactive = interactive && !args.no_prompt;

    let chosen = if interactive && args.car.is_none() { ask_preset(&args, input, out) } else { None };
    let mut setup = match build_setup(&args, chosen) {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(err, "{e}");
            return 2;
        }
    };
    let mut status = 0;
    if interactive {
        ask(&mut setup, input, out);
    }
    settle_board_gearbox(&mut setup);
    if interactive {
        let check = |_: &str| Ok(());
        if let Reply::Answer(path) =
            prompt(input, out, "Save these answers to a file? [path, or Enter to skip]", "", &check)
        {
            match std::fs::write(&path, setup.to_file_text()) {
                Ok(()) => {
                    let _ = writeln!(out, "Saved to {path}. Use it again with --car {path}");
                }
                Err(e) => {
                    let _ = writeln!(err, "cannot write {path}: {e}");
                    status = 2;
                }
            }
        }
        let _ = writeln!(out, "Running the ROM (about 20 seconds)...");
    }

    let result = script::run(&mut bench, &setup);
    let report = verdict::judge(&result, &setup);
    let name = rom_name(&args.rom);
    let _ = write!(out, "{}", report::text(&report, &setup, &name));
    if let Some(path) = &args.report {
        if let Err(e) = std::fs::write(path, report::json(&report, &setup, &name)) {
            let _ = writeln!(err, "cannot write report {path}: {e}");
            status = 2;
        }
    }
    if status == 0 { report.exit_status() } else { status }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::car::{Gearbox, Iacv, O2Sensor};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn drive(args: &[&str], input: &str, interactive: bool) -> (i32, String, String) {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let status = run(&args, &mut input.as_bytes(), interactive, &mut out, &mut err);
        (status, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
    }

    fn unique_path(tag: &str) -> String {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir()
            .join(format!("hondaecu-check-{}-{n}-{tag}", std::process::id()))
            .to_string_lossy()
            .into_owned()
    }

    fn temp_file(bytes: &[u8]) -> String {
        let path = unique_path("file");
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// A 64-bit LCG; no dependency.
    fn pseudo_random(len: usize, seed: u64) -> Vec<u8> {
        let mut x = seed;
        (0..len)
            .map(|_| {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (x >> 56) as u8
            })
            .collect()
    }

    fn strings(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_missing_rom_is_one_sentence_and_status_2() {
        let (status, out, err) = drive(&["check", "no-such-file.bin"], "", false);
        assert_eq!(status, 2);
        assert!(out.is_empty() && err.lines().count() == 1);
    }

    #[test]
    fn a_rom_of_the_wrong_size_is_refused_before_anything_runs() {
        let path = temp_file(&[0u8; 100]);
        let (status, _, err) = drive(&["check", &path], "", false);
        assert_eq!(status, 2);
        assert!(err.contains("32768"));
    }

    #[test]
    fn a_directory_as_the_rom_is_status_2() {
        let (status, _, _) = drive(&["check", "."], "", false);
        assert_eq!(status, 2);
    }

    #[test]
    fn input_ending_mid_questions_takes_defaults_and_says_so() {
        let path = temp_file(&vec![0xB4u8; 32768]);
        let (status, out, _) = drive(&["check", &path], "stock-p28-manual\nno\n", true);
        assert_eq!(status, 1);
        assert!(out.contains("Input ended") && out.contains("defaulted:") && out.contains("VERDICT: DO NOT BURN"));
    }

    #[test]
    fn an_unwritable_report_path_still_prints_the_verdict() {
        let path = temp_file(&vec![0xB4u8; 32768]);
        let (status, out, err) =
            drive(&["check", &path, "--no-prompt", "--report", "no-such-dir/r.json"], "", false);
        assert_eq!(status, 2);
        assert!(out.contains("VERDICT:") && !err.is_empty());
    }

    #[test]
    fn random_bytes_are_not_worth_burning() {
        let path = temp_file(&pseudo_random(32768, 7));
        let (status, out, _) = drive(&["check", &path, "--no-prompt"], "", false);
        assert_eq!(status, 1);
        assert!(out.contains("VERDICT: DO NOT BURN"));
    }

    #[test]
    fn test_is_an_alias_of_check() {
        let path = temp_file(&vec![0xB4u8; 32768]);
        let a = drive(&["check", &path, "--no-prompt"], "", false);
        let b = drive(&["test", &path, "--no-prompt"], "", false);
        assert_eq!(a, b);
        assert!(a.1.contains("VERDICT:"));
    }

    #[test]
    fn an_unknown_flag_is_status_2_with_usage() {
        let (status, out, err) = drive(&["check", "rom.bin", "--frobnicate"], "", false);
        assert_eq!(status, 2);
        assert!(out.is_empty());
        assert!(err.contains("--frobnicate") && err.contains("Usage:"));
        // Other command-line mistakes get the same treatment.
        for bad in [&["check"][..], &["check", "a.bin", "b.bin"], &["check", "a.bin", "--set", "vtec"], &["check", "a.bin", "--car"]] {
            let (status, _, err) = drive(bad, "", false);
            assert_eq!(status, 2, "{bad:?}");
            assert!(err.contains("Usage:"), "{bad:?}");
        }
    }

    #[test]
    fn set_overrides_the_car_file_which_overrides_the_preset() {
        let file = temp_file("# test car\ngearbox = manual\nvtec = no\nknock_sensor = yes\n".as_bytes());
        let args = parse_args(&strings(&[
            "check", "rom.bin", "--car", &file, "--set", "vtec=yes", "--board-gearbox", "automatic",
        ]))
        .unwrap();
        // The preset is automatic; the file says manual; --set beats the file.
        let s = build_setup(&args, Some("stock-p28-automatic")).unwrap();
        assert_eq!(s.gearbox, Gearbox::Manual, "file beats the (ignored) preset answer");
        assert!(s.vtec, "--set beats the file");
        assert!(s.knock_sensor, "file beats the preset default");
        assert_eq!(s.board_gearbox, Gearbox::Automatic, "shorthand is a --set");

        // A named preset comes first of all, and the file is not read for it.
        let args = parse_args(&strings(&["check", "rom.bin", "--car", "stock-p28-automatic"])).unwrap();
        let s = build_setup(&args, None).unwrap();
        assert_eq!(s.gearbox, Gearbox::Automatic);
        assert!(s.given.is_empty());

        // Through the whole command, the report records the values used.
        let rom = temp_file(&vec![0xB4u8; 32768]);
        let json = unique_path("report.json");
        let (_, _, err) = drive(
            &["check", &rom, "--car", &file, "--set", "vtec=yes", "--no-prompt", "--report", &json],
            "",
            false,
        );
        assert!(err.is_empty(), "{err}");
        let json = std::fs::read_to_string(&json).unwrap();
        assert!(json.contains("\"vtec\": \"yes\"") && json.contains("\"gearbox\": \"manual\""));
        assert!(json.contains("\"knock_sensor\": \"yes\""));
    }

    #[test]
    fn a_car_file_may_start_with_a_byte_order_mark() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"vtec = no\r\n");
        let file = temp_file(&bytes);
        let args = parse_args(&strings(&["check", "rom.bin", "--car", &file])).unwrap();
        let s = build_setup(&args, None).unwrap();
        assert!(!s.vtec);
        assert!(s.given.contains("vtec"));
    }

    #[test]
    fn a_bad_car_file_or_value_is_status_2_naming_the_cause() {
        let rom = temp_file(&vec![0xB4u8; 32768]);
        let bad = temp_file(b"vtec = yes\nturbo = yes\n");
        let (status, out, err) = drive(&["check", &rom, "--car", &bad, "--no-prompt"], "", false);
        assert_eq!(status, 2);
        assert!(out.is_empty() && err.contains("line 2"));
        let (status, _, err) = drive(&["check", &rom, "--car", "no-such-car-file", "--no-prompt"], "", false);
        assert_eq!(status, 2);
        assert!(err.contains("no-such-car-file"));
        let (status, _, err) = drive(&["check", &rom, "--set", "vtec=maybe", "--no-prompt"], "", false);
        assert_eq!(status, 2);
        assert!(err.contains("vtec"));
    }

    #[test]
    fn ecu_p30_starts_from_the_p30_preset() {
        let args = parse_args(&strings(&["check", "rom.bin", "--ecu", "p30"])).unwrap();
        let s = build_setup(&args, None).unwrap();
        assert_eq!(s.preset, "stock-p30");
        assert!(s.knock_sensor);
    }

    #[test]
    fn a_wrong_answer_is_asked_again_then_defaulted() {
        // Invalid, then valid: asked again.
        let mut s = Setup::preset("stock-p28-manual").unwrap();
        let mut out = Vec::new();
        ask(&mut s, &mut "p99\np30\n".as_bytes(), &mut out);
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("expected p28 or p30") && out.contains("Try again"));
        assert!(s.given.contains("ecu"));
        assert_eq!(s.value_text("ecu"), "p30");

        // Three invalid answers: the default, and the next line answers the next question.
        let mut s = Setup::preset("stock-p28-manual").unwrap();
        let mut out = Vec::new();
        ask(&mut s, &mut "x\nx\nx\nautomatic\n".as_bytes(), &mut out);
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("Taking the default, p28"));
        assert!(!s.given.contains("ecu"));
        assert_eq!(s.value_text("ecu"), "p28");
        assert_eq!(s.gearbox, Gearbox::Automatic);
        assert!(s.given.contains("gearbox"));
    }

    #[test]
    fn a_question_is_plain_words_with_answers_and_default() {
        let mut s = Setup::preset("stock-p28-manual").unwrap();
        let mut out = Vec::new();
        ask(&mut s, &mut "".as_bytes(), &mut out);
        let out = String::from_utf8(out).unwrap();
        assert!(out.starts_with("ECU model? (p28/p30) [p28]: "), "{out}");
        // Only the first question is shown once the input has ended.
        assert_eq!(out.matches(": ").count(), 1);

        let mut s = Setup::preset("stock-p28-manual").unwrap();
        let mut out = Vec::new();
        let answers = "\n".repeat(3);
        ask(&mut s, &mut answers.as_bytes(), &mut out);
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("VTEC engine? (yes/no) [yes]: "));
        assert!(s.given.is_empty(), "taking a default is not giving a value");
    }

    #[test]
    fn answers_already_given_are_not_asked_again() {
        let mut s = Setup::preset("stock-p28-manual").unwrap();
        s.set("ecu", "p28").unwrap();
        let mut out = Vec::new();
        ask(&mut s, &mut "".as_bytes(), &mut out);
        assert!(!String::from_utf8(out).unwrap().contains("ECU model?"));
    }

    #[test]
    fn the_questions_are_asked_and_the_answers_saved_to_a_file_that_reads_back() {
        let rom = temp_file(&vec![0xB4u8; 32768]);
        let saved = unique_path("car.txt");
        let answers = [
            "stock-p28-automatic", // preset
            "p28",                 // ecu
            "automatic",           // gearbox
            "no",                  // vtec
            "yes",                 // knock_sensor
            "one-wire",            // o2_sensor
            "9.5",                 // iacv
            "yes",                 // egr
            "no",                  // eld
            "no",                  // speed_sensor
            "no",                  // ac
            "no",                  // power_steering_switch
            "",                    // board_gearbox: Enter takes the default
            saved.as_str(),        // the save offer
        ]
        .join("\n")
            + "\n";
        let (status, out, err) = drive(&["check", &rom], &answers, true);
        assert!(err.is_empty(), "{err}");
        assert_eq!(status, 1, "the 0xB4 ROM is not worth burning");
        assert!(out.contains("Start from which car? (stock-p28-manual/stock-p28-automatic/stock-p30) [stock-p28-manual]: "));
        assert!(out.contains("Knock sensor fitted? (yes/no) [no]: "));
        assert!(out.contains("Save these answers to a file? [path, or Enter to skip]"));
        // Only the Enter-pressed key is defaulted, and it followed the car's gearbox.
        assert!(out.contains("(defaulted: board_gearbox)"), "{out}");
        assert!(out.contains("ECU: P28 automatic"), "{out}");

        let mut back = Setup::preset("stock-p28-manual").unwrap();
        back.apply_file(&std::fs::read_to_string(&saved).unwrap()).unwrap();
        assert_eq!(back.gearbox, Gearbox::Automatic);
        assert_eq!(back.board_gearbox, Gearbox::Automatic);
        assert!(!back.vtec && back.knock_sensor && back.egr);
        assert!(!back.eld && !back.speed_sensor && !back.ac && !back.power_steering_switch);
        assert_eq!(back.o2_sensor, O2Sensor::OneWire);
        assert_eq!(back.iacv, Iacv::Ohms(9.5));
        assert!(back.defaulted().is_empty(), "a saved file states every key");

        // The file runs the same check without questions.
        let (status2, out2, _) = drive(&["check", &rom, "--car", &saved, "--no-prompt"], "", false);
        assert_eq!(status2, 1);
        assert!(out2.contains("VERDICT: DO NOT BURN") && !out2.contains("defaulted:"));
    }

    #[test]
    fn no_prompt_never_asks_even_on_a_terminal() {
        let rom = temp_file(&vec![0xB4u8; 32768]);
        let (_, out, _) = drive(&["check", &rom, "--no-prompt"], "p30\n", true);
        assert!(!out.contains("Start from which car?") && !out.contains("ECU model?"));
    }

    #[test]
    fn a_board_not_described_follows_the_cars_gearbox() {
        let rom = temp_file(&vec![0xB4u8; 32768]);
        let json = unique_path("g.json");
        drive(&["check", &rom, "--car", "stock-p28-automatic", "--no-prompt", "--report", &json], "", false);
        let json = std::fs::read_to_string(&json).unwrap();
        assert!(json.contains("    \"board_gearbox\": \"automatic\""), "{json}");
        let defaulted = json.lines().find(|l| l.contains("\"defaulted\"")).unwrap();
        assert!(defaulted.contains("\"board_gearbox\""), "still listed as defaulted: {defaulted}");
    }
}
