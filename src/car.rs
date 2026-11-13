//! The ECU box and the car it is wired to, as plain data.
//! A car file is `key = value` lines; `#` starts a comment.

use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq)] pub enum EcuModel { P28, P30 }
#[derive(Debug, Clone, PartialEq)] pub enum Gearbox { Manual, Automatic }
#[derive(Debug, Clone, PartialEq)] pub enum O2Sensor { None, OneWire, Heated }
#[derive(Debug, Clone, PartialEq)] pub enum Iacv { Stock, None, Ohms(f64) }

#[derive(Debug, Clone, PartialEq)]
pub struct Setup {
    pub ecu: EcuModel,
    pub board_gearbox: Gearbox,
    pub gearbox: Gearbox,
    pub vtec: bool,
    pub knock_sensor: bool,
    pub o2_sensor: O2Sensor,
    pub iacv: Iacv,
    pub egr: bool,
    pub eld: bool,
    pub speed_sensor: bool,
    pub ac: bool,
    pub power_steering_switch: bool,
    pub preset: &'static str,
    /// Keys the user supplied.
    pub given: BTreeSet<&'static str>,
}

pub const KEYS: [&str; 12] = [
    "ecu", "board_gearbox", "gearbox", "vtec", "knock_sensor", "o2_sensor",
    "iacv", "egr", "eld", "speed_sensor", "ac", "power_steering_switch",
];
pub const PRESETS: [&str; 3] = ["stock-p28-manual", "stock-p28-automatic", "stock-p30"];

fn parse_bool(v: &str) -> Result<bool, String> {
    match v { "yes" => Ok(true), "no" => Ok(false), _ => Err(format!("expected yes or no, got \"{v}\"")) }
}
fn parse_gearbox(v: &str) -> Result<Gearbox, String> {
    match v {
        "manual" => Ok(Gearbox::Manual),
        "automatic" => Ok(Gearbox::Automatic),
        _ => Err(format!("expected manual or automatic, got \"{v}\"")),
    }
}
fn yes_no(b: bool) -> &'static str { if b { "yes" } else { "no" } }
fn gearbox_text(g: &Gearbox) -> &'static str {
    match g { Gearbox::Manual => "manual", Gearbox::Automatic => "automatic" }
}

/// The preset a `preset = <name>` value names.
fn preset_value(value: &str) -> Result<&'static str, String> {
    let mut v = value.trim();
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') { v = v[1..v.len() - 1].trim(); }
    Setup::preset(v).map(|s| s.preset)
}

/// The preset a car file starts from, when it has a `preset = <name>` line.
/// Errors are "line N: ...". `preset` is not one of `KEYS`.
pub fn file_preset(text: &str) -> Result<Option<&'static str>, String> {
    let mut found = None;
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.starts_with('#') { continue; }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim().eq_ignore_ascii_case("preset") {
                let p = preset_value(value).map_err(|e| format!("line {}: {e}", i + 1))?;
                found.get_or_insert(p);
            }
        }
    }
    Ok(found)
}

impl Setup {
    pub fn preset(name: &str) -> Result<Setup, String> {
        let name = name.trim().to_ascii_lowercase();
        let (preset, ecu, gearbox, knock_sensor) = match name.as_str() {
            "stock-p28-manual" => (PRESETS[0], EcuModel::P28, Gearbox::Manual, false),
            "stock-p28-automatic" => (PRESETS[1], EcuModel::P28, Gearbox::Automatic, false),
            "stock-p30" => (PRESETS[2], EcuModel::P30, Gearbox::Manual, true),
            _ => return Err(format!("unknown preset \"{name}\"; known presets: {}", PRESETS.join(", "))),
        };
        Ok(Setup {
            ecu, board_gearbox: gearbox.clone(), gearbox, vtec: true, knock_sensor,
            o2_sensor: O2Sensor::Heated, iacv: Iacv::Stock, egr: false, eld: true,
            speed_sensor: true, ac: true, power_steering_switch: true,
            preset, given: BTreeSet::new(),
        })
    }

    /// Set one key from text and record it as given.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let key_lc = key.trim().to_ascii_lowercase();
        let name: &'static str = KEYS.iter().copied().find(|k| *k == key_lc)
            .ok_or_else(|| format!("unknown key \"{}\"; known keys: {}", key.trim(), KEYS.join(", ")))?;
        let mut v = value.trim();
        if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') { v = v[1..v.len() - 1].trim(); }
        let v = v.to_ascii_lowercase();
        let v = v.as_str();
        match name {
            "ecu" => self.ecu = match v {
                "p28" => EcuModel::P28,
                "p30" => EcuModel::P30,
                _ => return Err(format!("expected p28 or p30, got \"{v}\"")),
            },
            "board_gearbox" => self.board_gearbox = parse_gearbox(v)?,
            "gearbox" => self.gearbox = parse_gearbox(v)?,
            "vtec" => self.vtec = parse_bool(v)?,
            "knock_sensor" => self.knock_sensor = parse_bool(v)?,
            "o2_sensor" => self.o2_sensor = match v {
                "none" => O2Sensor::None,
                "one-wire" => O2Sensor::OneWire,
                "heated" => O2Sensor::Heated,
                _ => return Err(format!("expected none, one-wire or heated, got \"{v}\"")),
            },
            "iacv" => self.iacv = match v {
                "stock" => Iacv::Stock,
                "none" => Iacv::None,
                _ => match v.parse::<f64>() {
                    Ok(r) if r.is_finite() && r > 0.0 => Iacv::Ohms(r),
                    _ => return Err(format!("expected stock, none or a positive number of ohms, got \"{v}\"")),
                },
            },
            "egr" => self.egr = parse_bool(v)?,
            "eld" => self.eld = parse_bool(v)?,
            "speed_sensor" => self.speed_sensor = parse_bool(v)?,
            "ac" => self.ac = parse_bool(v)?,
            "power_steering_switch" => self.power_steering_switch = parse_bool(v)?,
            _ => unreachable!(),
        }
        self.given.insert(name);
        Ok(())
    }

    /// Apply a car file. Errors are "line N: ...".
    pub fn apply_file(&mut self, text: &str) -> Result<(), String> {
        for (i, raw) in text.lines().enumerate() {
            let n = i + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') { continue; }
            let (key, value) = line.split_once('=')
                .ok_or_else(|| format!("line {n}: expected `key = value`, got \"{line}\""))?;
            if key.trim().eq_ignore_ascii_case("preset") {
                preset_value(value).map_err(|e| format!("line {n}: {e}"))?;
                continue;
            }
            self.set(key, value).map_err(|e| format!("line {n}: {e}"))?;
        }
        Ok(())
    }

    pub fn to_file_text(&self) -> String {
        let mut out = String::new();
        for key in KEYS { out.push_str(&format!("{key} = {}\n", self.value_text(key))); }
        out
    }

    /// A car file for saving: each given key as `key = value`, each defaulted key
    /// as a comment, so a re-run reports the same defaulted list.
    pub fn to_saved_file_text(&self) -> String {
        let mut out = format!("# car file
preset = {}
", self.preset);
        for key in KEYS {
            if self.given.contains(key) {
                out.push_str(&format!("{key} = {}
", self.value_text(key)));
            } else {
                out.push_str(&format!("# {key} = {}   (preset default)
", self.value_text(key)));
            }
        }
        out
    }

    /// KEYS not supplied by the user, in KEYS order.
    pub fn defaulted(&self) -> Vec<&'static str> {
        KEYS.iter().copied().filter(|k| !self.given.contains(k)).collect()
    }

    pub fn value_text(&self, key: &str) -> String {
        match key {
            "ecu" => match self.ecu { EcuModel::P28 => "p28", EcuModel::P30 => "p30" }.to_string(),
            "board_gearbox" => gearbox_text(&self.board_gearbox).to_string(),
            "gearbox" => gearbox_text(&self.gearbox).to_string(),
            "vtec" => yes_no(self.vtec).to_string(),
            "knock_sensor" => yes_no(self.knock_sensor).to_string(),
            "o2_sensor" => match self.o2_sensor {
                O2Sensor::None => "none", O2Sensor::OneWire => "one-wire", O2Sensor::Heated => "heated",
            }.to_string(),
            "iacv" => match self.iacv {
                Iacv::Stock => "stock".to_string(),
                Iacv::None => "none".to_string(),
                Iacv::Ohms(r) => format!("{r}"),
            },
            "egr" => yes_no(self.egr).to_string(),
            "eld" => yes_no(self.eld).to_string(),
            "speed_sensor" => yes_no(self.speed_sensor).to_string(),
            "ac" => yes_no(self.ac).to_string(),
            "power_steering_switch" => yes_no(self.power_steering_switch).to_string(),
            _ => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn a_hand_typed_car_file_is_accepted() {
        let mut s = Setup::preset("stock-p28-manual").unwrap();
        s.apply_file("# my civic\r\n\r\nknock_sensor = no  \r\no2_sensor = \"one-wire\"\r\nIACV = 9.5\r\n").unwrap();
        assert!(matches!(s.o2_sensor, O2Sensor::OneWire));
        assert!(matches!(s.iacv, Iacv::Ohms(r) if r == 9.5));
        assert_eq!(s.defaulted(), ["ecu", "board_gearbox", "gearbox", "vtec", "egr", "eld",
                                   "speed_sensor", "ac", "power_steering_switch"]);
    }
    #[test] fn a_bad_line_is_refused_with_its_number() {
        let mut s = Setup::preset("stock-p28-manual").unwrap();
        assert!(s.apply_file("vtec = yes\nturbo = yes\n").unwrap_err().starts_with("line 2:"));
        assert!(s.apply_file("vtec = maybe\n").unwrap_err().starts_with("line 1:"));
        assert!(s.apply_file("iacv = -3\n").unwrap_err().starts_with("line 1:"));
        assert!(s.apply_file("vtec yes\n").unwrap_err().starts_with("line 1:"));
    }
    #[test] fn a_saved_file_reads_back_the_same() {
        let mut a = Setup::preset("stock-p30").unwrap();
        a.set("iacv", "none").unwrap();
        let mut b = Setup::preset("stock-p28-manual").unwrap();
        b.apply_file(&a.to_file_text()).unwrap();
        for key in KEYS { assert_eq!(a.value_text(key), b.value_text(key), "{key}"); }
    }
    #[test] fn a_saved_file_keeps_defaulted_keys_as_comments() {
        let mut a = Setup::preset("stock-p30").unwrap();
        a.set("iacv", "none").unwrap();
        a.set("vtec", "no").unwrap();
        let text = a.to_saved_file_text();
        assert!(text.starts_with("# car file
preset = stock-p30
") || text.starts_with("# car file
preset = stock-p30
"));
        assert_eq!(file_preset(&text).unwrap(), Some("stock-p30"));
        assert!(text.contains("iacv = none") && text.contains("vtec = no"));
        assert!(text.contains("# ecu = p30   (preset default)"));
        let mut b = Setup::preset("stock-p30").unwrap();
        b.apply_file(&text).unwrap();
        assert_eq!(b.defaulted(), a.defaulted());
        assert_eq!(b.given, a.given);
        for key in KEYS { assert_eq!(a.value_text(key), b.value_text(key), "{key}"); }
    }
    #[test] fn a_preset_line_selects_the_starting_preset() {
        let text = "preset = stock-p30
";
        assert_eq!(file_preset(text).unwrap(), Some("stock-p30"));
        let mut s = Setup::preset(file_preset(text).unwrap().unwrap()).unwrap();
        s.apply_file(text).unwrap();
        let p30 = Setup::preset("stock-p30").unwrap();
        for key in KEYS { assert_eq!(s.value_text(key), p30.value_text(key), "{key}"); }
        assert_eq!(s.preset, "stock-p30");
        assert_eq!(s.defaulted().len(), KEYS.len(), "preset is not a key");
        // Quoted and mixed case, like other values.
        assert_eq!(file_preset("PRESET = \"Stock-P28-Automatic\"
").unwrap(), Some("stock-p28-automatic"));
        // No line: as before.
        assert_eq!(file_preset("vtec = no
# preset = stock-p30
").unwrap(), None);
    }
    #[test] fn an_unknown_preset_in_a_file_is_a_line_error() {
        let e = file_preset("vtec = no
preset = stock-p72
").unwrap_err();
        assert!(e.starts_with("line 2:") && e.contains("stock-p28-manual"), "{e}");
        let mut s = Setup::preset("stock-p28-manual").unwrap();
        assert!(s.apply_file("preset = nope
").unwrap_err().starts_with("line 1:"));
    }
    #[test] fn an_unknown_preset_is_an_error_naming_the_known_ones() {
        assert!(Setup::preset("stock-p72").unwrap_err().contains("stock-p28-manual"));
    }
}
