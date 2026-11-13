//! The `check` command: run a ROM on the modelled ECU box wired to a described
//! car and print one verdict. `test` is an alias.

use std::io::{BufRead, Write};

use crate::bench::Bench;
use crate::car::{file_preset, EcuModel, Setup, KEYS, PRESETS};
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

/// The default shown at the preset question (asked only without `--car`):
/// what the base would be from the command line alone.
fn default_preset(args: &Args) -> &'static str {
    let p30 = args
        .sets
        .iter()
        .any(|(k, v)| k.trim().eq_ignore_ascii_case("ecu") && v.trim().eq_ignore_ascii_case("p30"));
    if p30 { PRESETS[2] } else { PRESETS[0] }
}

/// `--car` names a preset rather than a file.
fn named_preset(args: &Args) -> Option<&str> {
    args.car.as_deref().filter(|c| PRESETS.contains(&c.trim().to_ascii_lowercase().as_str()))
}

/// The car file's name, when `--car` gave a file.
fn car_file_name(args: &Args) -> Option<String> {
    match (&args.car, named_preset(args)) {
        (Some(path), None) => Some(rom_name(path)),
        _ => None,
    }
}

/// Preset, then the car file, then `--set` and the shorthands, in that order.
/// `chosen_preset` is the answer to the preset question, when it was asked.
/// When no preset is named anywhere (`--car`, the file's `preset =` line, the
/// question), the base is `stock-p30` if the resolved `ecu` is p30, from
/// whichever source gave it, else `stock-p28-manual`.
fn build_setup(args: &Args, chosen_preset: Option<&str>) -> Result<Setup, String> {
    let named_preset = named_preset(args);
    let file_text = match (&args.car, named_preset) {
        (Some(path), None) => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read car file {path}: {e}"))?;
            // Windows Notepad saves UTF-8 with a byte-order mark.
            Some((path, text.strip_prefix('\u{feff}').unwrap_or(&text).to_string()))
        }
        _ => None,
    };
    // The file's own `preset` line decides the base for that file.
    let from_file = match &file_text {
        Some((path, text)) => file_preset(text).map_err(|e| format!("car file {path}: {e}"))?,
        None => None,
    };
    let build = |base: &str| -> Result<Setup, String> {
        let mut setup = Setup::preset(base)?;
        if let Some((path, text)) = &file_text {
            setup.apply_file(text).map_err(|e| format!("car file {path}: {e}"))?;
        }
        for (key, value) in &args.sets {
            setup.set(key, value).map_err(|e| format!("--set {key}: {e}"))?;
        }
        Ok(setup)
    };
    match named_preset.or(from_file).or(chosen_preset) {
        Some(base) => build(base),
        None => {
            let setup = build(PRESETS[0])?;
            if setup.ecu == EcuModel::P30 { build(PRESETS[2]) } else { Ok(setup) }
        }
    }
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
/// Returns true when the input ended before every question was answered.
pub fn ask(setup: &mut Setup, input: &mut impl BufRead, out: &mut impl Write) -> bool {
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
                return true;
            }
        }
    }
    false
}

/// The preset chosen, if any, and whether the input ended.
fn ask_preset(args: &Args, input: &mut impl BufRead, out: &mut impl Write) -> (Option<&'static str>, bool) {
    let default = default_preset(args);
    let text = format!("Start from which car? ({})", PRESETS.join("/"));
    let check = |v: &str| Setup::preset(v).map(|_| ());
    match prompt(input, out, &text, default, &check) {
        Reply::Answer(v) => (PRESETS.iter().copied().find(|p| p.eq_ignore_ascii_case(&v)), false),
        Reply::Default => (None, false),
        Reply::EndOfInput => (None, true),
    }
}

/// One line of input, trimmed; `None` at the end of input.
fn read_answer(input: &mut impl BufRead, out: &mut impl Write) -> Option<String> {
    let _ = out.flush();
    let mut line = String::new();
    if !matches!(input.read_line(&mut line), Ok(n) if n > 0) {
        let _ = writeln!(out);
        return None;
    }
    Some(line.trim().to_string())
}

/// Offers to save the answers as a car file. Enter, `n` or `no` skips; `y`
/// or `yes` asks for the name; anything else is the name. An existing file
/// is overwritten only on a yes. A failed save is reported and changes
/// nothing else: the verdict still decides the exit status.
fn offer_save(setup: &Setup, input: &mut impl BufRead, out: &mut impl Write, err: &mut impl Write) {
    let _ = write!(out, "Save these answers to a file? Type a file name, or press Enter to skip: ");
    let Some(answer) = read_answer(input, out) else { return };
    let name = match answer.to_ascii_lowercase().as_str() {
        "" | "n" | "no" => return,
        "y" | "yes" => {
            let _ = write!(out, "File name: ");
            match read_answer(input, out) {
                Some(name) if !name.is_empty() => name,
                _ => return,
            }
        }
        _ => answer,
    };
    if std::path::Path::new(&name).exists() {
        let _ = write!(out, "\"{name}\" exists. Overwrite? (yes/no) [no]: ");
        let yes = read_answer(input, out).is_some_and(|a| a.eq_ignore_ascii_case("yes") || a.eq_ignore_ascii_case("y"));
        if !yes {
            let _ = writeln!(out, "Not saved.");
            return;
        }
    }
    match std::fs::write(&name, setup.to_saved_file_text()) {
        Ok(()) => {
            let _ = writeln!(out, "Saved to {name}. Use it again with --car {name}");
        }
        Err(e) => {
            let _ = writeln!(err, "cannot write {name}: {e}");
        }
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

    let mut ended = false;
    let chosen = if interactive && args.car.is_none() {
        let (chosen, eof) = ask_preset(&args, input, out);
        ended = eof;
        chosen
    } else {
        None
    };
    let mut setup = match build_setup(&args, chosen) {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(err, "{e}");
            return 2;
        }
    };
    let mut status = 0;
    if interactive && ended {
        let _ = writeln!(out, "Input ended; the remaining answers take their defaults.");
    } else if interactive {
        ended = ask(&mut setup, input, out);
    }
    settle_board_gearbox(&mut setup);
    if interactive {
        // Nothing more is offered once the input has ended.
        if !ended {
            offer_save(&setup, input, out, err);
        }
        let _ = writeln!(out, "Running the ROM (about half a minute)...");
    }

    let result = script::run(&mut bench, &setup);
    let report = verdict::judge(&result, &setup);
    let name = rom_name(&args.rom);
    let car_file = car_file_name(&args);
    let _ = write!(out, "{}", report::text(&report, &setup, &name, car_file.as_deref()));
    if let Some(path) = &args.report {
        if let Err(e) = std::fs::write(path, report::json(&report, &setup, &name, car_file.as_deref())) {
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
        assert!(!out.contains("Save these answers"), "no save offer once the input has ended: {out}");
        assert!(out.contains("Input ended; the remaining answers take their defaults.\nRunning"), "{out}");

        // Ended right at the preset question: same.
        let (_, out, _) = drive(&["check", &path], "", true);
        assert!(!out.contains("Save these answers") && out.contains("Input ended"), "{out}");
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
    fn ecu_p30_from_any_source_gives_the_same_setup() {
        let file = temp_file(b"ecu = p30\n");
        let setups: Vec<Setup> = [
            &["check", "rom.bin", "--ecu", "p30"][..],
            &["check", "rom.bin", "--set", "ecu=p30"],
            &["check", "rom.bin", "--car", &file],
        ]
        .iter()
        .map(|a| build_setup(&parse_args(&strings(a)).unwrap(), None).unwrap())
        .collect();
        for s in &setups {
            assert_eq!(s, &setups[0]);
        }
        assert_eq!(setups[0].preset, "stock-p30");
        assert!(setups[0].knock_sensor);
        assert_eq!(setups[0].defaulted(), Setup::preset("stock-p30").unwrap().defaulted()[1..].to_vec());

        // A preset that is named still wins over the ecu.
        let file = temp_file(b"preset = stock-p28-manual\necu = p30\n");
        let s = build_setup(&parse_args(&strings(&["check", "rom.bin", "--car", &file])).unwrap(), None).unwrap();
        assert_eq!(s.preset, "stock-p28-manual");
        assert!(!s.knock_sensor);
        let args = parse_args(&strings(&["check", "rom.bin", "--ecu", "p30"])).unwrap();
        assert_eq!(build_setup(&args, Some("stock-p28-manual")).unwrap().preset, "stock-p28-manual");
    }

    /// The interactive run up to the save offer, every question taking its
    /// default, then `save` as the lines answering the offer.
    fn save_offer(save: &[&str]) -> (i32, String, String) {
        let rom = temp_file(&vec![0xB4u8; 32768]);
        let input = "\n".repeat(1 + KEYS.len()) + &save.iter().map(|l| format!("{l}\n")).collect::<String>();
        drive(&["check", &rom], &input, true)
    }

    #[test]
    fn no_at_the_save_offer_skips_in_any_case() {
        for answer in ["n", "no", "NO", "N", ""] {
            let (status, out, err) = save_offer(&[answer]);
            assert_eq!(status, 1, "{answer}");
            assert!(err.is_empty(), "{answer}: {err}");
            assert!(out.contains("Save these answers to a file? Type a file name, or press Enter to skip: "), "{out}");
            assert!(!out.contains("Saved to"), "{answer}: {out}");
            assert!(!std::path::Path::new(answer).exists() || answer.is_empty(), "{answer} written");
        }
    }

    #[test]
    fn yes_at_the_save_offer_asks_for_the_name() {
        let saved = unique_path("yes.txt");
        let (_, out, _) = save_offer(&["Yes", &saved]);
        assert!(out.contains("File name: "), "{out}");
        assert!(out.contains(&format!("Saved to {saved}.")), "{out}");
        assert!(std::fs::read_to_string(&saved).unwrap().contains("preset = stock-p28-manual"));
        // Enter at the name: nothing saved.
        let (_, out, _) = save_offer(&["y", ""]);
        assert!(out.contains("File name: ") && !out.contains("Saved to"), "{out}");
    }

    #[test]
    fn an_existing_file_is_overwritten_only_on_yes() {
        let saved = temp_file(b"keep me\n");
        for answer in ["", "no", "maybe"] {
            let (_, out, _) = save_offer(&[&saved, answer]);
            assert!(out.contains(&format!("\"{saved}\" exists. Overwrite? (yes/no) [no]: ")), "{out}");
            assert!(out.contains("Not saved.") && !out.contains("Saved to"), "{out}");
            assert_eq!(std::fs::read_to_string(&saved).unwrap(), "keep me\n", "{answer}");
        }
        let (_, out, _) = save_offer(&[&saved, "yes"]);
        assert!(out.contains(&format!("Saved to {saved}.")), "{out}");
        assert!(std::fs::read_to_string(&saved).unwrap().contains("preset = "));
    }

    #[test]
    fn a_failed_save_says_why_and_keeps_the_verdicts_exit_status() {
        let (status, out, err) = save_offer(&["no-such-dir/car.txt"]);
        assert!(err.contains("cannot write no-such-dir/car.txt"), "{err}");
        assert!(out.contains("VERDICT: DO NOT BURN"), "{out}");
        assert_eq!(status, 1, "the verdict's status, not 2");
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
        assert!(out.contains("Save these answers to a file? Type a file name, or press Enter to skip: "));
        // Only the Enter-pressed key is defaulted, and it followed the car's gearbox.
        assert!(out.contains("(defaulted: board_gearbox)"), "{out}");
        assert!(out.contains("ECU: P28 automatic"), "{out}");

        let mut back = Setup::preset("stock-p28-manual").unwrap();
        back.apply_file(&std::fs::read_to_string(&saved).unwrap()).unwrap();
        assert_eq!(back.gearbox, Gearbox::Automatic);
        // board_gearbox is a comment in the file; a run settles it from the gearbox.
        assert_eq!(back.value_text("board_gearbox"), "manual");
        assert!(!back.vtec && back.knock_sensor && back.egr);
        assert!(!back.eld && !back.speed_sensor && !back.ac && !back.power_steering_switch);
        assert_eq!(back.o2_sensor, O2Sensor::OneWire);
        assert_eq!(back.iacv, Iacv::Ohms(9.5));
        assert_eq!(back.defaulted(), ["board_gearbox"], "a saved file keeps defaulted keys as comments");

        // The file runs the same check without questions.
        let (status2, out2, _) = drive(&["check", &rom, "--car", &saved, "--no-prompt"], "", false);
        assert_eq!(status2, 1);
        assert!(out2.contains("VERDICT: DO NOT BURN"));
        let header = |s: &str| s.lines().find(|l| l.starts_with("ROM:")).unwrap().split_once("(defaulted").unwrap().1.to_string();
        assert_eq!(header(&out2), header(&out), "the re-run reports the same defaulted list");
    }

    #[test]
    fn a_saved_file_reruns_the_same_car_from_a_non_default_preset() {
        let rom = temp_file(&vec![0xB4u8; 32768]);
        let saved = unique_path("p30.txt");
        let json1 = unique_path("first.json");
        let json2 = unique_path("second.json");
        // stock-p30; gearbox and egr answered, everything else takes the default.
        let answers = ["stock-p30", "", "automatic", "", "", "", "", "no", "", "", "", "", "", saved.as_str()]
            .join("\n")
            + "\n";
        let (_, out, err) = drive(&["check", &rom, "--report", &json1], &answers, true);
        assert!(err.is_empty(), "{err}");
        let file = std::fs::read_to_string(&saved).unwrap();
        assert!(file.contains("preset = stock-p30"), "{file}");
        assert!(file.contains("# knock_sensor = yes"), "{file}");

        let (_, out2, err2) = drive(&["check", &rom, "--car", &saved, "--no-prompt", "--report", &json2], "", false);
        assert!(err2.is_empty(), "{err2}");
        let header = |s: &str| s.lines().find(|l| l.starts_with("ROM:")).unwrap().to_string();
        // The re-run's header names the file as well as its preset.
        let file_name = rom_name(&saved);
        assert_eq!(header(&out2), header(&out).replace("Car: stock-p30", &format!("Car: {file_name} (preset stock-p30)")));
        assert!(header(&out).contains("Car: stock-p30 "), "{}", header(&out));
        let setup_of = |path: &str| {
            let j = std::fs::read_to_string(path).unwrap();
            let start = j.find("\"setup\"").unwrap();
            j[start..j.find("\"defaulted\"").unwrap()].to_string()
        };
        assert_eq!(setup_of(&json1), setup_of(&json2));
        assert!(setup_of(&json2).contains("\"ecu\": \"p30\"") && setup_of(&json2).contains("\"knock_sensor\": \"yes\""));
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
