//! One fixed sequence of driving conditions run on the bench, recording what
//! an outside observer of the box would see in each stage.

use crate::bench::{Bench, Settling, Stop};
use crate::blink::{decode, Lamp};
use crate::car::Setup;
use crate::wiring::{apply_static, apply_switches, Switches};

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
    /// rpm when port C bit 0 first went high.
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
    pub mil_first_lit_after_bulb_check: Option<f64>,
    pub lamp: Lamp,
    pub seconds: f64,
}

/// Coolant temperature in every stage but `cold idle`.
const WARM_ECT_CELSIUS: f64 = 88.0;
/// The stage whose start opens the fault-code read window.
const CODE_READ_OUT: &str = "code read-out";
/// An opening followed by this many engine cycles without another is where
/// fuel stopped.
const FUEL_STOP_CYCLES: f64 = 2.0;

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
        Stage {
            rpm: (6000.0, 1500.0),
            speed_kmh: 80.0,
            ..still("lift off", 4.0, 0.0, 25.0, in_gear)
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
/// short at the stop.
pub fn run(bench: &mut Bench, setup: &Setup) -> RunResult {
    apply_static(&mut bench.bus, setup);
    let mut results = Vec::new();
    let mut read_out_from = None;
    for stage in stages(setup) {
        if stage.name == CODE_READ_OUT {
            read_out_from = Some(bench.seconds());
        }
        let (result, running) = run_stage(bench, setup, &stage);
        results.push(result);
        if !running {
            break;
        }
    }

    let end = bench.seconds();
    let edges = bench.mil_edges();
    let key_on_end = results.first().map_or(end, |s| s.to);
    // The bulb check ends at the first lit-to-dark edge in `key on`; with
    // none there, the search starts at the end of `key on`.
    let bulb_check_end = edges
        .windows(2)
        .find(|w| w[0].1 && !w[1].1 && w[1].0 <= key_on_end)
        .map_or(key_on_end, |w| w[1].0);
    let search_to = read_out_from.unwrap_or(end);
    let mil_first_lit_after_bulb_check = edges
        .windows(2)
        .find(|w| !w[0].1 && w[1].1 && w[1].0 > bulb_check_end && w[1].0 < search_to)
        .map(|w| w[1].0);
    // A run that never reached the read-out has not been read.
    let lamp = match read_out_from {
        Some(from) => decode(&edges, from, end),
        None => Lamp::Unreadable,
    };

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
        mil_first_lit_after_bulb_check,
        lamp,
        seconds: end,
    }
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
    // Engine cycles and rpm at the latest injector opening.
    let mut last_open: Option<(f64, f64)> = None;
    let mut fuel_stops_rpm = None;
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
        let mut opens: u64 = bench.bus.injector_open_count.iter().sum();
        running = bench.run_observed(SLICE_SECONDS, |b| {
            let cycles = cycles_at_start + (b.seconds() - slice_start) * rpm / 120.0;
            let now: u64 = b.bus.injector_open_count.iter().sum();
            if now != opens {
                opens = now;
                last_open = Some((cycles, rpm));
            } else if fuel_stops_rpm.is_none() {
                if let Some((at, open_rpm)) = last_open {
                    if cycles - at >= FUEL_STOP_CYCLES {
                        fuel_stops_rpm = Some(open_rpm);
                    }
                }
            }
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
        fuel_stops_rpm,
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
        assert_eq!(r.mil_first_lit_after_bulb_check, None);
        // With the connector grounded and no code stored, the ROM holds the
        // MIL lit (0x36B1..0x36CA: 31Ah and 31Ch zero, option byte 6011h
        // zero -> SC, MB P1.4, C): Honda's "no codes" display.
        assert_eq!(r.lamp, Lamp::Steady);
    }
}
