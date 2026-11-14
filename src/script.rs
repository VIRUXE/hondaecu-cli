//! One fixed sequence of driving conditions run on the bench, recording what
//! an outside observer of the box would see in each stage.

use crate::bench::{Bench, Settling, Stop};
use crate::blink::{complete_runs, decode, full_period, Lamp};
use crate::car::Setup;
use crate::wiring::{apply_engine, apply_static, apply_switches, Switches, Unmodelled};

/// Engine inputs are refreshed this often.
pub const SLICE_SECONDS: f64 = 0.01;

#[derive(Debug, Clone)]
pub struct Stage {
    pub name: &'static str,
    pub seconds: f64,
    /// Start, end; linear in time.
    pub rpm: (f64, f64),
    pub map_kpa: f64,
    pub tps_pct: f64,
    pub ect_celsius: f64,
    pub speed_kmh: f64,
    pub switches: Switches,
}

#[derive(Debug, Clone)]
pub struct StageResult {
    pub name: &'static str,
    pub from: f64,
    pub to: f64,
    /// Integral of rpm / 120 over the stage.
    pub engine_cycles: f64,
    pub injector_opens: [u64; 4],
    pub pulse_us: [u32; 4],
    pub ignitions: u64,
    pub fuel_pump_on_seconds: f64,
    pub mil_lit_at_end: bool,
    pub ac_clutch_seen: bool,
    pub iacv_duty_pct: f32,
    /// rpm of the last opening followed by two engine cycles with none.
    pub fuel_stops_rpm: Option<f64>,
    /// rpm when port C bit 0 first went from low to high inside the stage.
    /// This reports a switch-on edge only: `None` does not mean VTEC was off
    /// when the stage began.
    pub vtec_on_rpm: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub stages: Vec<StageResult>,
    pub settling: Settling,
    pub board_programmed: bool,
    pub stop: Option<(f64, Stop)>,
    /// Reads, writes, last address.
    pub unmapped: (u64, u64, Option<u16>),
    pub ppi_unsupported_mode: bool,
    /// The lamp was seen lit during `key on` (from the board log): the bulb
    /// check, the only sign that the lamp works at all. Without it a dark
    /// lamp says nothing about faults.
    pub bulb_check_seen: bool,
    pub mil_first_lit_after_bulb_check: Option<f64>,
    /// The lamp never went out after power-on: lit from the bulb check to
    /// the end of `key on`. `mil_first_lit_after_bulb_check` is then the end
    /// of `key on`.
    pub mil_never_went_out: bool,
    /// The lamp was lit at the end of the drive: lit at the end of the last
    /// stage before `code read-out`, or lit for more than half of that
    /// stage (a lamp blinking then is not dark). False when the run did not
    /// get that far.
    pub mil_lit_at_end_of_drive: bool,
    /// Each lit interval that starts after the bulb check and before `code
    /// read-out` (or the stop), from the board log. One still open at the
    /// start of the read-out (or at the stop) ends there. With
    /// `mil_never_went_out` the first starts at the end of `key on`.
    pub mil_lit_spans: Vec<(f64, f64)>,
    pub lamp: Lamp,
    /// The read-out reached `READ_OUT_MAX_SECONDS` while the lamp was still
    /// blinking and before its sequence had been seen to repeat: `lamp` may
    /// lack codes.
    pub lamp_list_may_be_incomplete: bool,
    pub seconds: f64,
    /// Car values the board model could not represent, with the reason
    /// (`wiring::apply_static`).
    pub unmodelled: Vec<Unmodelled>,
}

/// Coolant temperature in every stage but `cold idle`.
const WARM_ECT_CELSIUS: f64 = 88.0;
/// The stage whose start opens the fault-code read window.
const CODE_READ_OUT: &str = "code read-out";
/// An opening followed by this many engine cycles without another is where
/// fuel stopped.
const FUEL_STOP_CYCLES: f64 = 2.0;
/// The read-out runs its 40 s stage, then, while the lamp has blinked
/// complete codes whose sequence has not yet been seen to repeat, further
/// slices of this length, up to `READ_OUT_MAX_SECONDS` in all.
const READ_OUT_SLICE_SECONDS: f64 = 1.0;
pub const READ_OUT_MAX_SECONDS: f64 = 180.0;

/// Finds `StageResult::fuel_stops_rpm`: the rpm of the last injector opening
/// followed by at least `FUEL_STOP_CYCLES` engine cycles with no opening. A
/// gap still open at the end counts once it has lasted that long.
#[derive(Debug, Clone)]
pub struct FuelStop {
    opens: u64,
    /// Engine cycles and rpm at the latest opening.
    last_open: Option<(f64, f64)>,
    stopped_rpm: Option<f64>,
}

impl FuelStop {
    /// `opens`: the total injector openings so far, before the stage.
    pub fn new(opens: u64) -> FuelStop {
        FuelStop { opens, last_open: None, stopped_rpm: None }
    }

    /// Feed the current rpm, the engine cycles since the stage began and the
    /// total injector openings so far.
    pub fn observe(&mut self, rpm: f64, engine_cycles: f64, opens: u64) {
        if opens != self.opens {
            self.opens = opens;
            self.last_open = Some((engine_cycles, rpm));
        } else if let Some((at, open_rpm)) = self.last_open
            && engine_cycles - at >= FUEL_STOP_CYCLES
        {
            self.stopped_rpm = Some(open_rpm);
        }
    }

    pub fn result(&self) -> Option<f64> {
        self.stopped_rpm
    }
}

/// The lamp over the read-out: `read_out` is its start and whether it ran its
/// full length. A read-out cut short or never reached has not been read.
fn read_out_lamp(edges: &[(f64, bool)], read_out: Option<(f64, bool)>, end: f64) -> Lamp {
    match read_out {
        Some((from, true)) => decode(edges, from, end),
        _ => Lamp::Unreadable,
    }
}

pub fn stages(setup: &Setup) -> Vec<Stage> {
    let still = |name, seconds, rpm: f64, map_kpa, switches| Stage {
        name,
        seconds,
        rpm: (rpm, rpm),
        map_kpa,
        tps_pct: 0.0,
        ect_celsius: WARM_ECT_CELSIUS,
        speed_kmh: 0.0,
        switches,
    };
    let none = Switches::default();
    let in_gear = Switches { in_gear: true, ..Switches::default() };

    let mut list = vec![
        still("key on", 6.0, 0.0, 100.0, none.clone()),
        still("cranking", 1.5, 300.0, 90.0, Switches { starter: true, ..Switches::default() }),
        Stage { ect_celsius: 0.0, ..still("cold idle", 3.0, 1500.0, 45.0, none.clone()) },
        still("warm idle", 8.0, 800.0, 35.0, none.clone()),
    ];
    if setup.ac {
        list.push(still("idle, A/C on", 4.0, 800.0, 40.0, Switches { ac: true, ..Switches::default() }));
    }
    list.extend([
        Stage {
            tps_pct: 15.0,
            speed_kmh: 60.0,
            ..still("cruise", 4.0, 2500.0, 45.0, in_gear.clone())
        },
        Stage {
            rpm: (2000.0, 9000.0),
            tps_pct: 100.0,
            speed_kmh: 100.0,
            ..still("full throttle", 14.0, 0.0, 98.0, in_gear.clone())
        },
        // Long enough for the stock ROM's speed-sensor check to store its
        // code. 0x3C74..0x3C8B sets 99h.4 (code 17) only while no VSS period
        // has been measured (231h.3), deceleration fuel cut is on (21Ch.4,
        // copied from 11Ch.4 set at 0x18BD), the rpm code 236h is below B0h
        // (0x3C7B; about 3300 rpm, observed) and the battery C3h is above 44h.
        // Its debounce count is 2Dh (6C68h index 13, loaded into 2C1h at
        // 0x49D3), observed to step down once every 0.10 s, and a hold at
        // 3000 rpm stored code 17 5.1 s after entering the window. Fuel cut
        // was observed on throughout 6000..1000 rpm at closed throttle. A
        // linear 6000 -> 1500 rpm ramp spends 1750 / 4500 of its length
        // below 3250 rpm: 8.6 s of 22 s, about 1.7 times the 5.1 s.
        Stage {
            rpm: (6000.0, 1500.0),
            speed_kmh: 80.0,
            ..still("lift off", 22.0, 0.0, 25.0, in_gear)
        },
        still("warm idle again", 8.0, 800.0, 35.0, none),
        still(
            CODE_READ_OUT,
            40.0,
            800.0,
            35.0,
            Switches { service_connector: true, ..Switches::default() },
        ),
    ]);
    list
}

/// Run every stage of `stages(setup)` on a freshly built `bench`. Scripting
/// stops as soon as execution does; the stage it stopped in is kept, cut
/// short at the stop. The read-out may run past its 40 s
/// (`read_until_the_codes_repeat`).
pub fn run(bench: &mut Bench, setup: &Setup) -> RunResult {
    let unmodelled = apply_static(&mut bench.bus, setup);
    apply_engine(&mut bench.engine, setup);
    let mut results = Vec::new();
    // Start of the read-out and whether it ran its full length.
    let mut read_out: Option<(f64, bool)> = None;
    let mut lamp_list_may_be_incomplete = false;
    for stage in stages(setup) {
        let start = bench.seconds();
        let (mut result, mut running) = run_stage(bench, setup, &stage);
        if stage.name == CODE_READ_OUT && running {
            let (more, still_running, cut) = read_until_the_codes_repeat(bench, setup, &stage, start);
            for r in more {
                extend(&mut result, r);
            }
            running = still_running;
            lamp_list_may_be_incomplete = cut;
        }
        results.push(result);
        if stage.name == CODE_READ_OUT {
            read_out = Some((start, running));
        }
        if !running {
            break;
        }
    }

    let end = bench.seconds();
    let edges = bench.mil_edges();
    let key_on_end = results.first().map_or(end, |s| s.to);
    let search_to = read_out.map_or(end, |r| r.0);
    let (mil_first_lit_after_bulb_check, mil_never_went_out) =
        mil_lit_after_bulb_check(&edges, key_on_end, search_to, end);
    // The read-out is the last stage, so the one before it ends the drive.
    let final_stage = read_out.and_then(|_| results.iter().rev().nth(1)).map(|s| (s.from, s.to));
    let (mil_lit_spans, mil_lit_at_end_of_drive) = lamp_after_bulb_check(
        &edges,
        key_on_end,
        search_to,
        end,
        mil_never_went_out,
        final_stage,
    );
    let lamp = read_out_lamp(&edges, read_out, end);

    RunResult {
        stages: results,
        settling: bench.log.settling(end),
        board_programmed: bench.board_programmed(),
        stop: bench.log.stop.clone(),
        unmapped: (
            bench.bus.external_unmapped_reads,
            bench.bus.external_unmapped_writes,
            bench.bus.external_last_unmapped,
        ),
        ppi_unsupported_mode: bench.bus.ppi.unsupported_mode,
        bulb_check_seen: bulb_check_seen(&edges, key_on_end),
        mil_first_lit_after_bulb_check,
        mil_never_went_out,
        mil_lit_at_end_of_drive,
        mil_lit_spans,
        lamp,
        lamp_list_may_be_incomplete,
        seconds: end,
        unmodelled,
    }
}

/// Keeps reading after the 40 s read-out `stage` (begun at `start`) while
/// the lamp has blinked complete codes whose sequence has not yet been seen
/// to repeat (`blink::full_period`), in slices of `READ_OUT_SLICE_SECONDS`,
/// until `READ_OUT_MAX_SECONDS` of read-out in all. A read-out with no
/// complete code ends at 40 s. Returns the extra slices, whether execution
/// is still going, and whether the cap was reached before a full repeat.
fn read_until_the_codes_repeat(
    bench: &mut Bench,
    setup: &Setup,
    stage: &Stage,
    start: f64,
) -> (Vec<StageResult>, bool, bool) {
    let slice = Stage { seconds: READ_OUT_SLICE_SECONDS, ..stage.clone() };
    let extra = ((READ_OUT_MAX_SECONDS - stage.seconds) / READ_OUT_SLICE_SECONDS).round() as usize;
    let mut more = Vec::new();
    for _ in 0..extra {
        let runs = complete_runs(&bench.mil_edges(), start, bench.seconds());
        if runs.is_empty() || full_period(&runs).is_some() {
            return (more, true, false);
        }
        let (result, running) = run_stage(bench, setup, &slice);
        more.push(result);
        if !running {
            return (more, false, false);
        }
    }
    let runs = complete_runs(&bench.mil_edges(), start, bench.seconds());
    let cut = !runs.is_empty() && full_period(&runs).is_none();
    (more, true, cut)
}

/// Adds a later slice `b` of the same stage to `a`.
fn extend(a: &mut StageResult, b: StageResult) {
    a.to = b.to;
    a.engine_cycles += b.engine_cycles;
    for i in 0..4 {
        a.injector_opens[i] += b.injector_opens[i];
    }
    a.pulse_us = b.pulse_us;
    a.ignitions += b.ignitions;
    a.fuel_pump_on_seconds += b.fuel_pump_on_seconds;
    a.mil_lit_at_end = b.mil_lit_at_end;
    a.ac_clutch_seen |= b.ac_clutch_seen;
    a.iacv_duty_pct = b.iacv_duty_pct;
    a.fuel_stops_rpm = b.fuel_stops_rpm.or(a.fuel_stops_rpm);
    a.vtec_on_rpm = a.vtec_on_rpm.or(b.vtec_on_rpm);
}

/// The lamp was seen lit at some point up to `key_on_end`, the end of `key
/// on` (or the stop, for a run that stopped inside it).
fn bulb_check_seen(edges: &[(f64, bool)], key_on_end: f64) -> bool {
    edges.iter().any(|e| e.1 && e.0 <= key_on_end)
}

/// Seconds the lamp was lit between `from` and `to`.
fn lit_seconds(edges: &[(f64, bool)], from: f64, to: f64) -> f64 {
    let mut lit = edges.iter().rev().find(|e| e.0 <= from).is_some_and(|e| e.1);
    let (mut t, mut total) = (from, 0.0);
    for &(at, now) in edges.iter().filter(|e| e.0 > from && e.0 < to) {
        if lit {
            total += at - t;
        }
        (t, lit) = (at, now);
    }
    if lit {
        total += to - t;
    }
    total
}

/// When the MIL was first lit after the bulb check, looking up to
/// `search_to`. The bulb check ends at the first lit-to-dark edge in `key
/// on` (which ends at `key_on_end`). A lamp still lit at the end of `key on`
/// without having gone dark never ended its bulb check: it counts as lit at
/// `key_on_end`, provided the run went on past `key on` (`end`), and the
/// second value is true. Otherwise the search starts at the end of `key on`.
fn mil_lit_after_bulb_check(
    edges: &[(f64, bool)],
    key_on_end: f64,
    search_to: f64,
    end: f64,
) -> (Option<f64>, bool) {
    let went_dark = edges
        .windows(2)
        .find(|w| w[0].1 && !w[1].1 && w[1].0 <= key_on_end)
        .map(|w| w[1].0);
    let lit_at_key_on_end = edges.iter().rev().find(|e| e.0 <= key_on_end).is_some_and(|e| e.1);
    if went_dark.is_none() && lit_at_key_on_end && key_on_end < end {
        return (Some(key_on_end), true);
    }
    let bulb_check_end = went_dark.unwrap_or(key_on_end);
    let lit = edges
        .windows(2)
        .find(|w| !w[0].1 && w[1].1 && w[1].0 > bulb_check_end && w[1].0 < search_to)
        .map(|w| w[1].0);
    (lit, false)
}

/// The lamp's lit intervals after the bulb check, and whether it is lit at
/// the end of the drive. A span starts at a dark-to-lit edge after the bulb
/// check (as in `mil_lit_after_bulb_check`) and before `search_to` (the
/// start of the read-out, or the end of a run that never got there), and
/// ends at the next dark edge; one still open at `search_to` ends there. A
/// lamp that `never_went_out` has one span from `key_on_end`, the moment
/// `mil_lit_after_bulb_check` counts it lit. `final_stage` is the last stage
/// before the read-out, `(from, to)`, or `None` when the run did not reach
/// the read-out: the drive did not end, so the lamp is not lit at its end.
/// Otherwise the lamp is lit at the end of the drive when it is lit at `to`
/// or was lit for more than half of the stage: the state at one instant
/// would make a blinking lamp a coin flip.
fn lamp_after_bulb_check(
    edges: &[(f64, bool)],
    key_on_end: f64,
    search_to: f64,
    end: f64,
    never_went_out: bool,
    final_stage: Option<(f64, f64)>,
) -> (Vec<(f64, f64)>, bool) {
    let lit_at = |t: f64| edges.iter().rev().find(|e| e.0 <= t).is_some_and(|e| e.1);
    let bulb_check_end = edges
        .windows(2)
        .find(|w| w[0].1 && !w[1].1 && w[1].0 <= key_on_end)
        .map_or(key_on_end, |w| w[1].0);
    let mut starts: Vec<f64> = Vec::new();
    if never_went_out && key_on_end < end {
        starts.push(key_on_end);
    }
    starts.extend(
        edges
            .windows(2)
            .filter(|w| !w[0].1 && w[1].1 && w[1].0 > bulb_check_end && w[1].0 < search_to)
            .map(|w| w[1].0),
    );
    let spans = starts
        .into_iter()
        .map(|from| {
            let to = edges
                .iter()
                .find(|e| e.0 > from && !e.1)
                .map_or(search_to, |e| e.0.min(search_to));
            (from, to)
        })
        .collect();
    let lit_at_end_of_drive = final_stage
        .is_some_and(|(from, to)| lit_at(to) || lit_seconds(edges, from, to) > (to - from) / 2.0);
    (spans, lit_at_end_of_drive)
}

/// Port B and C levels at `t` from `log.board`, or `None` before the ROM
/// made them outputs.
fn board_at(board: &[(f64, u8, u8)], t: f64) -> Option<(u8, u8)> {
    board.iter().rev().find(|e| e.0 <= t).map(|&(_, b, c)| (b, c))
}

/// Returns the stage's observations and whether execution is still going.
fn run_stage(bench: &mut Bench, setup: &Setup, stage: &Stage) -> (StageResult, bool) {
    use crate::bus::{P28_PB_AC_CLUTCH_OFF, P28_PB_FUEL_PUMP_OFF, P28_PC_VTEC_SOLENOID};

    let from = bench.seconds();
    let opens_before = bench.bus.injector_open_count;
    let ignitions_before = bench.bus.ignition_event_count;
    let board_before = bench.log.board.len();

    let slices = (stage.seconds / SLICE_SECONDS).round() as usize;
    let mut engine_cycles = 0.0;
    let mut fuel_stop = FuelStop::new(bench.bus.injector_open_count.iter().sum());
    let mut vtec_on_rpm = None;
    let mut running = true;
    for i in 0..slices {
        let progress = i as f64 / slices as f64;
        let rpm = stage.rpm.0 + (stage.rpm.1 - stage.rpm.0) * progress;
        let engine = &mut bench.engine;
        engine.rpm = rpm;
        engine.map_kpa = stage.map_kpa;
        engine.tps_pct = stage.tps_pct;
        engine.ect_celsius = stage.ect_celsius;
        engine.speed_kmh = stage.speed_kmh;
        bench.engine.sync_sensors_to_bus(&mut bench.bus);
        apply_switches(&mut bench.bus, setup, &stage.switches);

        let slice_start = bench.seconds();
        let cycles_at_start = engine_cycles;
        let board_seen = bench.log.board.len();
        running = bench.run_observed(SLICE_SECONDS, |b| {
            let cycles = cycles_at_start + (b.seconds() - slice_start) * rpm / 120.0;
            fuel_stop.observe(rpm, cycles, b.bus.injector_open_count.iter().sum());
        });
        engine_cycles = cycles_at_start + (bench.seconds() - slice_start) * rpm / 120.0;

        if vtec_on_rpm.is_none() {
            let board = &bench.log.board;
            let low_before = board_seen == 0 || board[board_seen - 1].2 & P28_PC_VTEC_SOLENOID == 0;
            let rose = board[board_seen..].iter().scan(low_before, |was_low, e| {
                let high = e.2 & P28_PC_VTEC_SOLENOID != 0;
                let rise = *was_low && high;
                *was_low = !high;
                Some(rise)
            });
            if rose.into_iter().any(|r| r) {
                vtec_on_rpm = Some(rpm);
            }
        }
        if !running {
            break;
        }
    }
    let to = bench.seconds();

    // Board levels over the stage: the level held at `from`, then each change.
    let board = &bench.log.board;
    let mut levels: Vec<(f64, u8, u8)> = Vec::new();
    if let Some((b, c)) = board_at(&board[..board_before], from) {
        levels.push((from, b, c));
    }
    levels.extend(board[board_before..].iter().copied());
    let mut fuel_pump_on_seconds = 0.0;
    for (i, &(t, b, _)) in levels.iter().enumerate() {
        let until = levels.get(i + 1).map_or(to, |e| e.0);
        if b & P28_PB_FUEL_PUMP_OFF == 0 {
            fuel_pump_on_seconds += until - t;
        }
    }
    let ac_clutch_seen = levels.iter().any(|&(_, b, _)| b & P28_PB_AC_CLUTCH_OFF == 0);

    let opens_after = bench.bus.injector_open_count;
    let result = StageResult {
        name: stage.name,
        from,
        to,
        engine_cycles,
        injector_opens: std::array::from_fn(|i| opens_after[i] - opens_before[i]),
        pulse_us: bench.bus.injector_last_pulse_us,
        ignitions: bench.bus.ignition_event_count - ignitions_before,
        fuel_pump_on_seconds,
        mil_lit_at_end: bench.bus.p28_outputs().is_some_and(|o| o.mil),
        ac_clutch_seen,
        iacv_duty_pct: bench.bus.iacv_duty_cycle_pct,
        fuel_stops_rpm: fuel_stop.result(),
        vtec_on_rpm,
    };
    (result, running)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ac_stage_is_left_out_for_a_car_without_ac() {
        let mut s = Setup::preset("stock-p28-manual").unwrap();
        assert!(stages(&s).iter().any(|st| st.name == "idle, A/C on"));
        s.set("ac", "no").unwrap();
        assert!(!stages(&s).iter().any(|st| st.name == "idle, A/C on"));
    }

    /// Feed `FuelStop` in steps of a tenth of an engine cycle up to `end`
    /// cycles, with rpm = 1000 x cycles, opening an injector at every
    /// half cycle where `fuelled(cycles)` holds.
    fn fuel_stop_over(end: f64, fuelled: impl Fn(f64) -> bool) -> Option<f64> {
        let mut detector = FuelStop::new(100);
        let mut opens = 100;
        for step in 0..=(end * 10.0).round() as u32 {
            let cycles = step as f64 / 10.0;
            if step % 5 == 0 && fuelled(cycles) {
                opens += 1;
            }
            detector.observe(1000.0 * cycles, cycles, opens);
        }
        detector.result()
    }

    #[test]
    fn fuel_stop_reports_the_last_opening_of_a_clean_cut() {
        assert_eq!(fuel_stop_over(20.0, |c| c <= 7.0), Some(7000.0));
    }

    #[test]
    fn fuel_stop_after_cut_resume_and_cut_reports_the_second_cut() {
        let fuelled = |c: f64| c <= 5.0 || (9.0..=12.0).contains(&c);
        assert_eq!(fuel_stop_over(20.0, fuelled), Some(12000.0));
    }

    #[test]
    fn fuel_stop_ignores_a_gap_shorter_than_two_cycles() {
        // Gap 3.0 -> 4.5 is 1.5 cycles; the real cut follows 10.0.
        let fuelled = |c: f64| c <= 3.0 || (4.5..=10.0).contains(&c);
        assert_eq!(fuel_stop_over(20.0, fuelled), Some(10000.0));
    }

    #[test]
    fn fuel_stop_keeps_the_last_cut_when_fuel_resumes_to_the_end() {
        let fuelled = |c: f64| c <= 5.0 || c >= 9.0;
        assert_eq!(fuel_stop_over(20.0, fuelled), Some(5000.0));
    }

    #[test]
    fn fuel_stop_counts_a_gap_open_at_the_end_only_once_it_is_two_cycles() {
        assert_eq!(fuel_stop_over(11.5, |c| c <= 10.0), None);
        assert_eq!(fuel_stop_over(12.0, |c| c <= 10.0), Some(10000.0));
    }

    #[test]
    fn fuel_that_never_stops_gives_none() {
        assert_eq!(fuel_stop_over(20.0, |_| true), None);
    }

    #[test]
    fn a_stage_with_no_opening_gives_none() {
        assert_eq!(fuel_stop_over(20.0, |_| false), None);
    }

    #[test]
    fn a_read_out_cut_short_or_never_reached_is_unreadable() {
        let lit_since = [(0.0, false), (50.07, true)];
        assert_eq!(read_out_lamp(&lit_since, Some((50.0, true)), 90.0), Lamp::Steady);
        assert_eq!(read_out_lamp(&lit_since, Some((50.0, false)), 90.0), Lamp::Unreadable);
        assert_eq!(read_out_lamp(&[], Some((50.0, false)), 60.0), Lamp::Unreadable);
        assert_eq!(read_out_lamp(&[], None, 30.0), Lamp::Unreadable);
    }

    #[test]
    fn a_lamp_that_never_goes_out_in_key_on_is_lit_after_the_bulb_check() {
        // Lit at power-on and never dark again (a ROM holding a fault from
        // its first second): the bulb check never ended.
        let held = [(0.04, false), (0.05, true)];
        assert_eq!(mil_lit_after_bulb_check(&held, 6.0, 70.0, 110.0), (Some(6.0), true));
        // The usual bulb check, then a fault later.
        let later = [(0.04, true), (2.1, false), (8.5, true)];
        assert_eq!(mil_lit_after_bulb_check(&later, 6.0, 70.0, 110.0), (Some(8.5), false));
        // The usual bulb check and nothing after it.
        assert_eq!(mil_lit_after_bulb_check(&later[..2], 6.0, 70.0, 110.0), (None, false));
        // The run stopped inside key on: nothing to say yet.
        assert_eq!(mil_lit_after_bulb_check(&held, 3.0, 3.0, 3.0), (None, false));
    }

    // Stage boundaries of the stock script: key on ends at 6.0, full
    // throttle is 26.5..40.5, lift off 40.5..62.5, the read-out starts at
    // 70.5 and the run ends at 110.5.
    const KEY_ON_END: f64 = 6.0;
    /// `warm idle again`, the last stage before the read-out.
    const FINAL_STAGE: (f64, f64) = (62.5, READ_OUT);
    const READ_OUT: f64 = 70.5;
    const END: f64 = 110.5;

    fn lamp_after(edges: &[(f64, bool)]) -> (Vec<(f64, f64)>, bool) {
        let never = mil_lit_after_bulb_check(edges, KEY_ON_END, READ_OUT, END).1;
        lamp_after_bulb_check(edges, KEY_ON_END, READ_OUT, END, never, Some(FINAL_STAGE))
    }

    #[test]
    fn the_bulb_check_is_seen_only_when_the_lamp_lit_in_key_on() {
        assert!(bulb_check_seen(&[(0.04, false), (0.044, true), (2.1, false)], KEY_ON_END));
        // Lit from power-on and never dark again.
        assert!(bulb_check_seen(&[(0.04, true)], KEY_ON_END));
        // Never lit, or no lamp edge at all (the board never programmed).
        assert!(!bulb_check_seen(&[(0.04, false)], KEY_ON_END));
        assert!(!bulb_check_seen(&[], KEY_ON_END));
        // Lit only after key on: no bulb check.
        assert!(!bulb_check_seen(&[(0.04, false), (8.0, true)], KEY_ON_END));
    }

    /// The lamp blinking through the final stage, `on` s lit then `off` s
    /// dark from 62.5 s, with the bulb check before it.
    fn blinking_to_the_read_out(on: f64, off: f64) -> Vec<(f64, bool)> {
        let mut edges = vec![(0.04, true), (2.1, false)];
        let blinks = ((READ_OUT - FINAL_STAGE.0) / (on + off)).round() as usize;
        for i in 0..blinks {
            let t = FINAL_STAGE.0 + i as f64 * (on + off);
            edges.push((t, true));
            edges.push((t + on, false));
        }
        edges
    }

    #[test]
    fn a_lamp_blinking_at_the_end_of_the_drive_counts_as_lit_when_mostly_lit() {
        // 0.3 s lit, 0.1 s dark: dark at 70.5 s (the last blink ends at
        // 70.4 s) but lit for three quarters of the stage.
        let edges = blinking_to_the_read_out(0.3, 0.1);
        assert!(!edges.iter().rev().find(|e| e.0 <= READ_OUT).unwrap().1, "dark at the end instant");
        assert!(lamp_after(&edges).1);
        // 0.1 s lit, 0.3 s dark, dark at the end: a quarter of the stage.
        let edges = blinking_to_the_read_out(0.1, 0.3);
        assert!(!lamp_after(&edges).1);
    }

    #[test]
    fn four_short_pulses_after_the_bulb_check_are_four_spans_and_dark_at_the_end() {
        // The lyonnel ROM's key-on display, then the steady read-out lamp.
        let edges = [
            (0.04, true),
            (2.6, false),
            (2.7, true),
            (3.0, false),
            (3.1, true),
            (3.4, false),
            (3.5, true),
            (3.8, false),
            (3.9, true),
            (4.2, false),
            (70.6, true),
        ];
        assert_eq!(
            lamp_after(&edges),
            (vec![(2.7, 3.0), (3.1, 3.4), (3.5, 3.8), (3.9, 4.2)], false)
        );
    }

    #[test]
    fn a_pulse_in_full_throttle_that_ends_before_lift_off_is_one_span() {
        let edges = [(0.04, true), (2.1, false), (39.4, true), (40.4, false), (70.6, true)];
        assert_eq!(lamp_after(&edges), (vec![(39.4, 40.4)], false));
    }

    #[test]
    fn a_lamp_that_lights_in_lift_off_and_stays_lit_is_open_to_the_read_out() {
        let edges = [(0.04, true), (2.1, false), (57.9, true)];
        assert_eq!(lamp_after(&edges), (vec![(57.9, READ_OUT)], true));
    }

    #[test]
    fn a_lamp_that_never_went_out_is_one_span_from_the_end_of_key_on() {
        let edges = [(0.04, false), (0.05, true)];
        assert_eq!(lamp_after(&edges), (vec![(KEY_ON_END, READ_OUT)], true));
    }

    #[test]
    fn a_run_that_stops_before_the_read_out_has_no_end_of_drive() {
        // Lit at 25 s and still lit when the run stopped at 30 s: the span
        // ends at the stop, and the drive never ended.
        let edges = [(0.04, true), (2.1, false), (25.0, true)];
        assert_eq!(
            lamp_after_bulb_check(&edges, KEY_ON_END, 30.0, 30.0, false, None),
            (vec![(25.0, 30.0)], false)
        );
        // Nothing after the bulb check.
        assert_eq!(
            lamp_after_bulb_check(&edges[..2], KEY_ON_END, 30.0, 30.0, false, None),
            (vec![], false)
        );
    }

    #[test]
    fn a_rom_that_stops_gives_a_partial_result_not_a_panic() {
        let mut bench = Bench::new(&vec![0xB4; 32768]);
        let r = run(&mut bench, &Setup::preset("stock-p28-manual").unwrap());
        assert!(r.stop.is_some() && r.stages.len() <= 1);
    }

    #[test]
    #[cfg_attr(not(feature = "p28-rom"), ignore = "needs the stock P28 ROM: enable feature p28-rom")]
    fn stock_p28_runs_the_whole_script_cleanly() {
        let setup = Setup::preset("stock-p28-manual").unwrap();
        let mut bench = Bench::from_file(&crate::fixtures::p28_rom_path()).unwrap();
        let r = run(&mut bench, &setup);
        assert!(matches!(r.settling, Settling::Settled { .. }) && r.stop.is_none());
        assert_eq!((r.unmapped.0, r.unmapped.1), (0, 0));
        let stage = |n: &str| r.stages.iter().find(|s| s.name == n).unwrap();
        assert!((1.8..=2.3).contains(&stage("key on").fuel_pump_on_seconds));
        let idle = stage("warm idle");
        for n in idle.injector_opens {
            assert!((n as f64 - idle.engine_cycles).abs() <= 2.0, "{n}");
        }
        assert!((idle.ignitions as f64 - 4.0 * idle.engine_cycles).abs() <= 8.0);
        assert!(stage("idle, A/C on").ac_clutch_seen);
        assert!(stage("full throttle")
            .fuel_stops_rpm
            .is_some_and(|rpm| (7000.0..=7500.0).contains(&rpm)));
        // Observed: 4875 rpm. The window at 6432h (0x117B; hondaromeditor
        // README: engage 4812 rpm when MAP is high, 5500 rpm regardless) sets
        // 128h.1 when the log-rpm code 12Dh exceeds CDh (CMPB A, 12Dh at
        // 0x1193), i.e. above 4812 rpm; the first code above it, CEh, is
        // 4875 rpm. The warm-coolant gate is C1h < 44h (0x11C7), about
        // 60 deg C on the sourced ECT curve.
        assert!(stage("full throttle")
            .vtec_on_rpm
            .is_some_and(|rpm| (4812.0..=4950.0).contains(&rpm)));
        assert_eq!(r.mil_first_lit_after_bulb_check, None);
        // With the connector grounded and no code stored, the ROM holds the
        // MIL lit (0x36B1..0x36CA: 31Ah and 31Ch zero, option byte 6011h
        // zero -> SC, MB P1.4, C): Honda's "no codes" display.
        assert_eq!(r.lamp, Lamp::Steady);
    }
}
