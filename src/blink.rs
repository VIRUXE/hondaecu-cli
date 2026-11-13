//! Reading fault codes the way a mechanic does: with the service connector
//! grounded the ROM flashes each stored code on the MIL, and this module
//! turns the lamp's on/off edges back into code numbers.

/// What the lamp showed over a read window.
#[derive(Debug, Clone, PartialEq)]
pub enum Lamp {
    /// Dark for the whole window.
    Off,
    /// Lit for the whole window, or lit without a break for at least
    /// `STEADY_MIN_SECONDS` up to the end of a window holding no complete
    /// run. With the service connector grounded, this is how the stock P28
    /// ROM shows "no codes stored": at 0x36B1..0x36CA, with the connector
    /// bit `210h.7` set, the stored-fault words `31Ah` and `31Ch` zero and
    /// option byte `6011h` zero, it sets carry and writes it to PC4 (MIL lit).
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
/// A lamp lit without a break this long at the end of the window, with no
/// complete run in it, is `Steady`; far longer than the 1.33 s long blink.
pub const STEADY_MIN_SECONDS: f64 = 10.0;

/// `edges`: (seconds, lit) changes; `from`..`to` is the read window, opened when
/// the service connector was grounded. Codes are distinct, ascending.
///
/// A run counts only if its first blink rises inside the window and the dark
/// stretch before that rise lasted at least `CODE_GAP_MIN_SECONDS`. That
/// stretch may have begun before `from` (dark since power-on counts as long
/// enough). So a lamp lit at `from` (a bulb check, or a blink cut by `from`)
/// is never a blink, and blinks after a too-short dark stretch are skipped
/// until a qualifying gap appears. At `to`, a lit stretch still open is never
/// counted, and a run counts only if the dark stretch after it has lasted a
/// full gap, so a run cut by `to` is dropped.
///
/// Lit for the whole window is `Steady`; no rise inside the window and not
/// lit throughout is `Off`. Rises but no complete qualifying run is `Steady`
/// when the lamp has been lit without a break for `STEADY_MIN_SECONDS` at
/// `to`, else `Unreadable`. Any complete run makes it `Codes`.
pub fn decode(edges: &[(f64, bool)], from: f64, to: f64) -> Lamp {
    let before = edges.iter().rposition(|e| e.0 <= from);
    let mut lit = before.map_or(false, |i| edges[i].1);
    // When the lamp last went dark; a lamp dark since power-on (the first
    // edge, or none) counts as having been dark forever.
    let mut dark_since = match before {
        Some(i) if i > 0 && !lit => edges[i].0,
        _ => f64::NEG_INFINITY,
    };
    let inside: Vec<&(f64, bool)> = edges.iter().filter(|e| e.0 > from && e.0 < to).collect();
    if inside.is_empty() {
        return if lit { Lamp::Steady } else { Lamp::Off };
    }

    let mut codes: Vec<u32> = Vec::new();
    let (mut tens, mut ones) = (0u32, 0u32);
    let mut counting = false;
    let mut saw_rise = false;
    let mut lit_since = from;
    for &&(t, now_lit) in &inside {
        if now_lit && !lit {
            saw_rise = true;
            let qualifies = t - dark_since >= CODE_GAP_MIN_SECONDS;
            if counting && qualifies && tens + ones > 0 {
                codes.push(tens * 10 + ones);
            }
            if qualifies {
                (tens, ones) = (0, 0);
            }
            counting |= qualifies;
            lit_since = t;
        } else if !now_lit && lit {
            if counting {
                if t - lit_since >= LONG_BLINK_MIN_SECONDS {
                    tens += 1;
                } else {
                    ones += 1;
                }
            }
            dark_since = t;
        }
        lit = now_lit;
    }
    // Still dark at `to`: the gap that ends the last run counts once it has
    // lasted long enough.
    if !lit && counting && tens + ones > 0 && to - dark_since >= CODE_GAP_MIN_SECONDS {
        codes.push(tens * 10 + ones);
    }

    if !saw_rise {
        return Lamp::Off;
    }
    if codes.iter().any(|&c| c > 99) {
        return Lamp::Unreadable;
    }
    let mut codes: Vec<u8> = codes.into_iter().map(|c| c as u8).collect();
    codes.sort_unstable();
    codes.dedup();
    if !codes.is_empty() {
        Lamp::Codes(codes)
    } else if lit && to - lit_since >= STEADY_MIN_SECONDS {
        Lamp::Steady
    } else {
        Lamp::Unreadable
    }
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
    fn a_lamp_lit_just_after_the_window_opens_and_kept_lit_is_steady() {
        // The stock ROM's "no codes stored" with the connector grounded.
        let edges = [(0.0, true), (2.0, false), (50.07, true)];
        assert_eq!(decode(&edges, 50.0, 90.0), Lamp::Steady);
    }

    #[test]
    fn a_short_lit_tail_after_incomplete_blinks_is_unreadable() {
        // Blinks 1 s after a bulb check (too soon to start a run), then lit
        // for the last 3 s of the window.
        let edges = [
            (0.0, true),
            (5.0, false),
            (6.0, true),
            (6.0 + SHORT, false),
            (6.0 + SHORT + PAUSE, true),
            (6.0 + 2.0 * SHORT + PAUSE, false),
            (17.0, true),
        ];
        assert_eq!(decode(&edges, 5.5, 20.0), Lamp::Unreadable);
    }

    #[test]
    fn a_complete_code_then_a_long_lit_tail_is_codes() {
        let (mut edges, end) = lamp_showing(&[(1, 4)]);
        edges.push((end, true));
        let to = end + STEADY_MIN_SECONDS * 2.0;
        assert_eq!(decode(&edges, 0.0, to), Lamp::Codes(vec![14]));
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

    #[test]
    fn a_window_opening_during_a_bulb_check_ignores_it() {
        let (mut edges, end) = lamp_showing(&[(1, 4), (1, 4)]);
        for e in edges.iter_mut() {
            e.0 += 10.0;
        }
        edges.splice(0..1, [(0.0, true), (8.0, false)]);
        assert_eq!(decode(&edges, 3.0, end + 10.0), Lamp::Codes(vec![14]));
    }

    #[test]
    fn a_window_opening_after_the_long_blink_does_not_read_4() {
        let (edges, end) = lamp_showing(&[(1, 4), (1, 4)]);
        let from = edges[2].0 + PAUSE / 2.0;
        assert_eq!(decode(&edges, from, end), Lamp::Codes(vec![14]));
    }

    #[test]
    fn a_window_opening_inside_a_short_blink_does_not_read_5() {
        let (edges, end) = lamp_showing(&[(1, 4), (1, 4)]);
        let from = edges[3].0 + SHORT / 2.0;
        assert_eq!(decode(&edges, from, end), Lamp::Codes(vec![14]));
    }

    #[test]
    fn a_window_opening_mid_run_with_no_later_repeat_is_unreadable() {
        let (edges, end) = lamp_showing(&[(1, 4)]);
        let from = edges[2].0 + PAUSE / 2.0;
        assert_eq!(decode(&edges, from, end), Lamp::Unreadable);
    }

    /// The stock P28 on the bench, connector grounded from reset, the part
    /// named by `unplug` taken off, 800 rpm for 30 s. The bulb check ends at
    /// about 5.4 s, so the window opens at 6 s.
    fn stock_p28_lamp(from: f64, unplug: impl FnOnce(&mut Bench)) -> Lamp {
        let setup = crate::car::Setup::preset("stock-p28-manual").unwrap();
        let mut bench = Bench::from_file(&crate::fixtures::p28_rom_path()).unwrap();
        bench.engine.rpm = 800.0;
        bench.engine.sync_sensors_to_bus(&mut bench.bus);
        crate::wiring::apply_static(&mut bench.bus, &setup);
        unplug(&mut bench);
        let jumper = crate::wiring::Switches { service_connector: true, ..Default::default() };
        crate::wiring::apply_switches(&mut bench.bus, &setup, &jumper);
        bench.run(30.0);
        decode(&bench.mil_edges(), from, bench.seconds())
    }

    #[test]
    #[cfg_attr(not(feature = "p28-rom"), ignore = "needs the stock P28 ROM: enable feature p28-rom")]
    fn stock_p28_with_the_iacv_unplugged_blinks_14() {
        let lamp = stock_p28_lamp(6.0, |bench| bench.bus.iacv_connected = false);
        assert_eq!(lamp, Lamp::Codes(vec![14]));
    }

    // Window opens where the caller cannot choose: inside the bulb check
    // (lit 0.04 to 5.37 s) and inside the long blink of a run (8.14 to 9.46 s).
    #[test]
    #[cfg_attr(not(feature = "p28-rom"), ignore = "needs the stock P28 ROM: enable feature p28-rom")]
    fn stock_p28_reads_14_when_the_window_opens_during_the_bulb_check() {
        let lamp = stock_p28_lamp(2.0, |bench| bench.bus.iacv_connected = false);
        assert_eq!(lamp, Lamp::Codes(vec![14]));
    }

    #[test]
    #[cfg_attr(not(feature = "p28-rom"), ignore = "needs the stock P28 ROM: enable feature p28-rom")]
    fn stock_p28_reads_14_when_the_window_opens_inside_a_run() {
        let lamp = stock_p28_lamp(9.0, |bench| bench.bus.iacv_connected = false);
        assert_eq!(lamp, Lamp::Codes(vec![14]));
    }

    #[test]
    #[cfg_attr(not(feature = "p28-rom"), ignore = "needs the stock P28 ROM: enable feature p28-rom")]
    fn stock_p28_with_the_o2_heater_unplugged_blinks_one_code() {
        // The ROM stores index 27 for this fault and blinks 41, which is
        // Honda's number for the O2 sensor heater.
        let lamp = stock_p28_lamp(6.0, |bench| bench.bus.o2_heater_connected = false);
        assert_eq!(lamp, Lamp::Codes(vec![41]));
    }
}
