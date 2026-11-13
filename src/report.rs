//! Renders a `Report` as plain text for a tuner and as JSON for tools.

use crate::car::{Setup, KEYS};
use crate::verdict::{Line, Report, Section, State, Verdict};

/// The last column the dot leader fills (counted from 1); text starts two columns later.
const LEADER_END: usize = 28;

fn summary_word(state: State) -> &'static str {
    match state {
        State::Ok => "yes",
        State::Problem => "no",
        State::CouldNotTell => "could not tell",
    }
}

fn state_name(state: State) -> &'static str {
    match state {
        State::Ok => "ok",
        State::Problem => "problem",
        State::CouldNotTell => "could_not_tell",
    }
}

/// `<prefix> <dots> <text>` with the text starting at the same column for every row.
fn leader_row(prefix: &str, text: &str) -> String {
    let used = prefix.chars().count();
    if used + 2 > LEADER_END {
        return format!("{prefix} {text}");
    }
    format!("{prefix} {} {text}", ".".repeat(LEADER_END - used - 1))
}

/// What a line's text starts with in the text report.
fn line_state_prefix(state: State) -> &'static str {
    match state {
        State::Ok => "ok: ",
        State::Problem => "PROBLEM: ",
        State::CouldNotTell => "could not tell: ",
    }
}

fn verdict_text(v: &Verdict) -> String {
    match v {
        Verdict::WorthBurning => "WORTH BURNING".to_string(),
        Verdict::DoNotBurn(r) => format!("DO NOT BURN - {r}"),
        Verdict::CouldNotTell(r) => format!("COULD NOT TELL - {r}"),
    }
}

/// `car_file` is the name of the car file used, if any.
pub fn text(report: &Report, setup: &Setup, rom_name: &str, car_file: Option<&str>) -> String {
    let car = match car_file {
        Some(file) => format!("{file} (preset {})", setup.preset),
        None => setup.preset.to_string(),
    };
    let mut out = format!(
        "ROM: {rom_name}   ECU: {} {}   Car: {car}",
        setup.value_text("ecu").to_uppercase(),
        setup.value_text("board_gearbox"),
    );
    let defaulted = setup.defaulted();
    if defaulted.len() == KEYS.len() {
        out.push_str(" (defaulted: all values, from the preset)");
    } else if !defaulted.is_empty() {
        out.push_str(&format!(" (defaulted: {})", defaulted.join(", ")));
    }
    out.push_str("\n\n");
    out.push_str(&format!("VERDICT: {}\n\n", verdict_text(&report.verdict)));
    for section in [&report.valid, &report.runs] {
        out.push_str(&leader_row(section.title, summary_word(section.state)));
        out.push('\n');
        for l in &section.lines {
            let text = format!("{}{}", line_state_prefix(l.state), l.text);
            out.push_str(&leader_row(&format!("  {}", l.label), &text));
            out.push('\n');
        }
    }
    out.push_str(&format!("NOT JUDGED: {}\n", report.not_judged.join(", ")));
    out
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn string_array(items: impl IntoIterator<Item = String>) -> String {
    let items: Vec<String> = items.into_iter().map(|s| quote(&s)).collect();
    format!("[{}]", items.join(", "))
}

fn section_json(s: &Section) -> String {
    let lines: Vec<String> = s
        .lines
        .iter()
        .map(|l| {
            format!(
                "        {{\"label\": {}, \"state\": {}, \"text\": {}}}",
                quote(l.label),
                quote(state_name(l.state)),
                quote(&l.text)
            )
        })
        .collect();
    let lines = if lines.is_empty() { "[]".to_string() } else { format!("[\n{}\n      ]", lines.join(",\n")) };
    format!(
        "    {{\n      \"title\": {},\n      \"state\": {},\n      \"lines\": {}\n    }}",
        quote(s.title),
        quote(state_name(s.state)),
        lines
    )
}

/// `car_file` is the name of the car file used, if any.
pub fn json(report: &Report, setup: &Setup, rom_name: &str, car_file: Option<&str>) -> String {
    let (verdict, reason) = match &report.verdict {
        Verdict::WorthBurning => ("worth_burning", "null".to_string()),
        Verdict::DoNotBurn(r) => ("do_not_burn", quote(r)),
        Verdict::CouldNotTell(r) => ("could_not_tell", quote(r)),
    };
    let setup_fields: Vec<String> =
        KEYS.iter().map(|k| format!("    {}: {}", quote(k), quote(&setup.value_text(k)))).collect();
    let codes: Vec<String> = report.codes.iter().map(|c| c.to_string()).collect();
    let mut out = String::from("{\n");
    out.push_str(&format!("  \"rom\": {},\n", quote(rom_name)));
    out.push_str(&format!("  \"preset\": {},\n", quote(setup.preset)));
    out.push_str(&format!("  \"car_file\": {},\n", car_file.map_or_else(|| "null".to_string(), quote)));
    out.push_str(&format!("  \"setup\": {{\n{}\n  }},\n", setup_fields.join(",\n")));
    out.push_str(&format!(
        "  \"defaulted\": {},\n",
        string_array(setup.defaulted().into_iter().map(String::from))
    ));
    out.push_str(&format!("  \"verdict\": {},\n", quote(verdict)));
    out.push_str(&format!("  \"reason\": {reason},\n"));
    out.push_str(&format!(
        "  \"sections\": [\n{},\n{}\n  ],\n",
        section_json(&report.valid),
        section_json(&report.runs)
    ));
    out.push_str(&format!("  \"codes\": [{}],\n", codes.join(", ")));
    out.push_str(&format!(
        "  \"not_judged\": {}\n",
        string_array(report.not_judged.iter().map(|s| s.to_string()))
    ));
    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::car::Setup;
    use crate::verdict::{Line, Report, Section, State, Verdict};

    fn line(label: &'static str, state: State, text: &str) -> Line {
        Line { label, state, text: text.to_string() }
    }

    fn report() -> Report {
        Report {
            verdict: Verdict::DoNotBurn("codes: 23 Knock Sensor (KS)".to_string()),
            valid: Section {
                title: "VALID FOR THIS ECU",
                state: State::Ok,
                lines: vec![
                    line("executes", State::Ok, "ok"),
                    line("settles after power-on", State::Ok, "ok, 2 resets during boot"),
                ],
            },
            runs: Section {
                title: "RUNS A BASIC SETUP",
                state: State::Problem,
                lines: vec![
                    line("warm idle", State::Ok, "ok"),
                    line("codes", State::Problem, "23 Knock Sensor (KS)"),
                ],
            },
            codes: vec![23],
            not_judged: &["fuelling", "ignition timing"],
        }
    }

    fn setup() -> Setup {
        Setup::preset("stock-p28-manual").unwrap()
    }

    #[test]
    fn the_verdict_is_the_first_thing_after_the_header() {
        let out = text(&report(), &setup(), "my-rom.bin", None);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("ROM: my-rom.bin   ECU: P28 manual   Car: stock-p28-manual"));
        assert_eq!(lines[1], "");
        assert_eq!(lines[2], "VERDICT: DO NOT BURN - codes: 23 Knock Sensor (KS)");
        assert_eq!(lines[3], "");
    }

    #[test]
    fn defaulted_values_are_named_in_the_header() {
        let mut s = setup();
        s.set("knock_sensor", "no").unwrap();
        let out = text(&report(), &s, "r.bin", None);
        let header = out.lines().next().unwrap();
        assert!(header.contains(" (defaulted: "), "{header}");
        assert!(header.contains("egr"), "{header}");
        assert!(!header.contains("knock_sensor"), "{header}");
        let none_defaulted = {
            let mut s = setup();
            for k in KEYS {
                let v = s.value_text(k);
                s.set(k, &v).unwrap();
            }
            s
        };
        let header = text(&report(), &none_defaulted, "r.bin", None).lines().next().unwrap().to_string();
        assert!(!header.contains("defaulted"), "{header}");
    }

    #[test]
    fn all_values_defaulted_is_one_phrase() {
        let header = text(&report(), &setup(), "r.bin", None).lines().next().unwrap().to_string();
        assert!(header.ends_with("(defaulted: all values, from the preset)"), "{header}");
        assert!(!header.contains("egr"), "{header}");
    }

    #[test]
    fn a_problem_line_shows_what_was_seen() {
        let out = text(&report(), &setup(), "r.bin", None);
        assert!(out.contains("  codes ........"), "{out}");
        assert!(out.lines().any(|l| l.ends_with(" PROBLEM: 23 Knock Sensor (KS)")), "{out}");
        assert!(out.contains("RUNS A BASIC SETUP ......... no"), "{out}");
        assert!(out.contains("VALID FOR THIS ECU ......... yes"), "{out}");
        assert!(out.contains("NOT JUDGED: fuelling, ignition timing"), "{out}");
    }

    #[test]
    fn text_aligns_every_row_at_the_same_column() {
        let mut r = report();
        r.valid.lines.push(line("a label that is far too long for the leader", State::Ok, "ok"));
        r.runs.state = State::CouldNotTell;
        let out = text(&r, &setup(), "r.bin", None);
        assert!(out.contains("RUNS A BASIC SETUP ......... could not tell"), "{out}");
        assert!(out.contains("  a label that is far too long for the leader ok: ok\n"), "{out}");
        for l in ["  executes ................. ok: ok", "  settles after power-on ... ok: ok, 2 resets during boot"] {
            assert!(out.contains(l), "{l}\n{out}");
        }
    }

    #[test]
    fn every_line_shows_its_state_before_its_text() {
        let mut r = report();
        r.runs.lines = vec![
            line(
                "warm idle",
                State::Ok,
                "injectors opened 53, 54, 53, 53 times and 214 sparks in 53.3 engine cycles",
            ),
            line("codes", State::Problem, "14 IACV Idle Control Valve"),
            line("check-engine light", State::CouldNotTell, "the run stopped before this"),
        ];
        let out = text(&r, &setup(), "r.bin", None);
        for l in [
            "  warm idle ................ ok: injectors opened 53, 54, 53, 53 times and 214 sparks in 53.3 engine cycles\n",
            "  codes .................... PROBLEM: 14 IACV Idle Control Valve\n",
            "  check-engine light ....... could not tell: the run stopped before this\n",
        ] {
            assert!(out.contains(l), "{l}\n{out}");
        }
        // Section rows keep their summary word.
        assert!(out.contains("RUNS A BASIC SETUP ......... no\n"), "{out}");
    }

    #[test]
    fn the_header_names_the_car_file_and_the_json_the_preset() {
        let out = text(&report(), &setup(), "r.bin", Some("my-civic.txt"));
        assert!(out.starts_with("ROM: r.bin   ECU: P28 manual   Car: my-civic.txt (preset stock-p28-manual)"), "{out}");
        let j = json(&report(), &setup(), "r.bin", Some("my-civic.txt"));
        assert!(j.contains("  \"preset\": \"stock-p28-manual\",\n"), "{j}");
        assert!(j.contains("  \"car_file\": \"my-civic.txt\",\n"), "{j}");
        let j = json(&report(), &setup(), "r.bin", None);
        assert!(j.contains("  \"preset\": \"stock-p28-manual\",\n") && j.contains("  \"car_file\": null,\n"), "{j}");
    }

    #[test]
    fn json_escapes_quotes_and_backslashes() {
        let out = json(&report(), &setup(), "a\"b\\c.bin", None);
        assert!(out.contains(r#"a\"b\\c.bin"#), "{out}");
    }

    #[test]
    fn json_escapes_newline_tab_and_control_characters() {
        let mut r = report();
        r.runs.lines.push(line("odd", State::Problem, "q\" b\\ n\n t\t c\u{1} é"));
        let out = json(&r, &setup(), "r.bin", None);
        assert!(out.contains(r#"q\" b\\ n\n t\t c\u0001 é"#), "{out}");
        // Raw newlines are only layout between tokens, never inside a string.
        for l in out.lines() {
            assert_eq!(l.matches('"').count() - l.matches("\\\"").count() & 1, 0, "{l}");
        }
        assert!(!out.contains('\t'));
        assert!(!out.contains('\u{1}'));
    }

    #[test]
    fn json_has_every_setup_key() {
        let out = json(&report(), &setup(), "r.bin", None);
        for k in KEYS {
            assert!(out.contains(&format!("\"{k}\": \"{}\"", setup().value_text(k))), "{k}\n{out}");
        }
    }

    #[test]
    fn json_carries_verdict_states_codes_and_not_judged() {
        let out = json(&report(), &setup(), "r.bin", None);
        assert!(out.contains("\"verdict\": \"do_not_burn\""), "{out}");
        assert!(out.contains("\"reason\": \"codes: 23 Knock Sensor (KS)\""), "{out}");
        assert!(out.contains("\"codes\": [23]"), "{out}");
        assert!(out.contains("\"state\": \"problem\""), "{out}");
        assert!(out.contains("\"not_judged\": [\"fuelling\", \"ignition timing\"]"), "{out}");
        let mut r = report();
        r.verdict = Verdict::WorthBurning;
        r.runs.state = State::CouldNotTell;
        let out = json(&r, &setup(), "r.bin", None);
        assert!(out.contains("\"verdict\": \"worth_burning\""), "{out}");
        assert!(out.contains("\"reason\": null"), "{out}");
        assert!(out.contains("\"state\": \"could_not_tell\""), "{out}");
    }
}
