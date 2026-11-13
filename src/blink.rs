//! Reading fault codes the way a mechanic does: with the service connector
//! grounded the ROM flashes each stored code on the MIL, and this module
//! turns the lamp's on/off edges back into code numbers.

/// What the lamp showed over a read window.
#[derive(Debug, Clone, PartialEq)]
pub enum Lamp {
    /// Dark for the whole window.
    Off,
    /// Lit for the whole window.
    Steady,
    /// Distinct code numbers, ascending.
    Codes(Vec<u8>),
    /// It changed, but no complete run of blinks could be read.
    Unreadable,
}

// Measured on the stock P28 ROM (IACV unplugged, connector grounded from
// reset, 800 rpm, 30 s). After the key-on bulb check the lamp repeats
// "long, short x4" every 7.8 s:
//   lit:  long blink 1.325, 1.331, 1.331 s; short blinks 0.300 to 0.319 s
//   dark: between blinks 0.406 to 0.513 s; between repeats 3.481 to 3.488 s
// The constants sit midway between the classes (0.31..1.33 and 0.51..3.48).

/// A blink lit at least this long is a tens digit; a shorter one is a ones digit.
pub const LONG_BLINK_MIN_SECONDS: f64 = 0.82;
/// A dark gap at least this long ends a code.
pub const CODE_GAP_MIN_SECONDS: f64 = 2.0;

/// `edges`: (seconds, lit) changes; `from`..`to` is the read window, opened when
/// the service connector was grounded. Codes are distinct, ascending.
///
/// Only edges inside the window count; the lamp's state at `from` is whatever
/// the last edge at or before `from` left it (dark if there is none). A blink
/// or gap already under way at `from` is measured from `from`. The stretch
/// still open at `to` is never counted, so a run without its closing gap is
/// dropped.
pub fn decode(edges: &[(f64, bool)], from: f64, to: f64) -> Lamp {
    let mut lit = edges.iter().take_while(|e| e.0 <= from).last().map_or(false, |e| e.1);
    let inside: Vec<&(f64, bool)> = edges.iter().filter(|e| e.0 > from && e.0 < to).collect();
    if inside.is_empty() {
        return if lit { Lamp::Steady } else { Lamp::Off };
    }

    let mut codes: Vec<u32> = Vec::new();
    let (mut tens, mut ones) = (0u32, 0u32);
    let mut since = from;
    for &&(t, now_lit) in &inside {
        let length = t - since;
        if lit {
            if length >= LONG_BLINK_MIN_SECONDS {
                tens += 1;
            } else {
                ones += 1;
            }
        } else if length >= CODE_GAP_MIN_SECONDS && tens + ones > 0 {
            codes.push(tens * 10 + ones);
            (tens, ones) = (0, 0);
        }
        lit = now_lit;
        since = t;
    }
    // Still dark at `to`: the gap that ends the last run counts once it has
    // lasted long enough. A blink still lit at `to` is never counted.
    if !lit && to - since >= CODE_GAP_MIN_SECONDS && tens + ones > 0 {
        codes.push(tens * 10 + ones);
    }

    if codes.iter().any(|&c| c > 99) {
        return Lamp::Unreadable;
    }
    let mut codes: Vec<u8> = codes.into_iter().map(|c| c as u8).collect();
    codes.sort_unstable();
    codes.dedup();
    if codes.is_empty() { Lamp::Unreadable } else { Lamp::Codes(codes) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::Bench;

    const LONG: f64 = LONG_BLINK_MIN_SECONDS * 1.6;
    const SHORT: f64 = LONG_BLINK_MIN_SECONDS * 0.4;
    const PAUSE: f64 = CODE_GAP_MIN_SECONDS * 0.2;
    const GAP: f64 = CODE_GAP_MIN_SECONDS * 1.5;

    /// Dark from 0, then each code as `tens` long and `ones` short blinks
    /// followed by a code gap. Returns the edges and the end time.
    fn lamp_showing(codes: &[(u32, u32)]) -> (Vec<(f64, bool)>, f64) {
        let mut edges = vec![(0.0, false)];
        let mut t = GAP;
        for &(tens, ones) in codes {
            let blinks = (0..tens).map(|_| LONG).chain((0..ones).map(|_| SHORT));
            for (i, length) in blinks.enumerate() {
                if i > 0 {
                    t += PAUSE;
                }
                edges.push((t, true));
                t += length;
                edges.push((t, false));
            }
            t += GAP;
        }
        (edges, t)
    }

    #[test]
    fn one_long_and_four_short_is_14() {
        let (edges, end) = lamp_showing(&[(1, 4)]);
        assert_eq!(decode(&edges, 0.0, end), Lamp::Codes(vec![14]));
    }

    #[test]
    fn three_short_is_3() {
        let (edges, end) = lamp_showing(&[(0, 3)]);
        assert_eq!(decode(&edges, 0.0, end), Lamp::Codes(vec![3]));
    }

    #[test]
    fn one_long_is_10() {
        let (edges, end) = lamp_showing(&[(1, 0)]);
        assert_eq!(decode(&edges, 0.0, end), Lamp::Codes(vec![10]));
    }

    #[test]
    fn repeated_codes_are_distinct_and_ascending() {
        let (edges, end) = lamp_showing(&[(1, 4), (0, 3), (1, 4), (0, 3)]);
        assert_eq!(decode(&edges, 0.0, end), Lamp::Codes(vec![3, 14]));
    }

    #[test]
    fn a_final_run_without_its_closing_gap_is_dropped() {
        let (edges, end) = lamp_showing(&[(0, 3), (1, 4)]);
        assert_eq!(decode(&edges, 0.0, end - GAP * 0.9), Lamp::Codes(vec![3]));
        let (edges, end) = lamp_showing(&[(1, 4)]);
        assert_eq!(decode(&edges, 0.0, end - GAP * 0.9), Lamp::Unreadable);
    }

    #[test]
    fn a_lamp_lit_for_the_whole_window_is_steady() {
        assert_eq!(decode(&[(0.0, true)], 5.0, 30.0), Lamp::Steady);
        // The bulb check's edge falls before the window.
        assert_eq!(decode(&[(0.0, false), (1.0, true)], 5.0, 30.0), Lamp::Steady);
    }

    #[test]
    fn a_lamp_dark_for_the_whole_window_is_off() {
        assert_eq!(decode(&[], 0.0, 30.0), Lamp::Off);
        assert_eq!(decode(&[(0.0, true), (2.0, false)], 5.0, 30.0), Lamp::Off);
    }

    #[test]
    fn edges_before_the_window_do_not_count() {
        // Bulb check: lit 0..2 s, then the code from 10 s on.
        let (mut edges, end) = lamp_showing(&[(1, 4)]);
        for e in edges.iter_mut() {
            e.0 += 10.0;
        }
        edges.splice(0..1, [(0.0, true), (2.0, false)]);
        assert_eq!(decode(&edges, 5.0, end + 10.0), Lamp::Codes(vec![14]));
    }

    #[test]
    fn blinks_that_never_close_with_a_gap_are_unreadable() {
        let edges = [(1.0, true), (1.0 + SHORT, false), (1.0 + SHORT + PAUSE, true)];
        assert_eq!(decode(&edges, 0.0, 10.0), Lamp::Unreadable);
    }

    /// The stock P28 on the bench, connector grounded from reset, the part
    /// named by `unplug` taken off, 800 rpm for 30 s. The bulb check ends at
    /// about 5.4 s, so the window opens at 6 s.
    fn stock_p28_lamp(unplug: impl FnOnce(&mut Bench)) -> Lamp {
        let setup = crate::car::Setup::preset("stock-p28-manual").unwrap();
        let mut bench = Bench::from_file(&crate::fixtures::p28_rom_path()).unwrap();
        bench.engine.rpm = 800.0;
        bench.engine.sync_sensors_to_bus(&mut bench.bus);
        crate::wiring::apply_static(&mut bench.bus, &setup);
        unplug(&mut bench);
        let jumper = crate::wiring::Switches { service_connector: true, ..Default::default() };
        crate::wiring::apply_switches(&mut bench.bus, &setup, &jumper);
        bench.run(30.0);
        decode(&bench.mil_edges(), 6.0, bench.seconds())
    }

    #[test]
    #[cfg_attr(not(feature = "p28-rom"), ignore = "needs the stock P28 ROM: enable feature p28-rom")]
    fn stock_p28_with_the_iacv_unplugged_blinks_14() {
        let lamp = stock_p28_lamp(|bench| bench.bus.iacv_connected = false);
        assert_eq!(lamp, Lamp::Codes(vec![14]));
    }

    #[test]
    #[cfg_attr(not(feature = "p28-rom"), ignore = "needs the stock P28 ROM: enable feature p28-rom")]
    fn stock_p28_with_the_o2_heater_unplugged_blinks_one_code() {
        // The ROM stores index 27 for this fault and blinks 41, which is
        // Honda's number for the O2 sensor heater.
        let lamp = stock_p28_lamp(|bench| bench.bus.o2_heater_connected = false);
        assert_eq!(lamp, Lamp::Codes(vec![41]));
    }
}
