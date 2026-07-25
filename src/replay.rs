// ECU Datalog CSV Replay & Scripted Trace Engine

use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use crate::cpu::Cpu;
use crate::bus::Bus;
use crate::engine::EngineState;
use crate::interpreter::Interpreter;
use crate::interrupts::InterruptController;

#[derive(Debug, Clone)]
pub struct LogFrame {
    pub timestamp_ms: u64,
    pub rpm: f64,
    pub map_kpa: f64,
    pub tps_pct: f64,
    pub ect_celsius: f64,
    pub iat_celsius: f64,
    pub o2_volts: f64,
    pub vbatt_volts: f64,
    pub speed_kmh: f64,
    pub note: &'static str,
}

#[derive(Debug, Clone)]
pub struct ReplayResultFrame {
    pub timestamp_ms: u64,
    pub rpm: f64,
    pub map_kpa: f64,
    pub tps_pct: f64,
    pub ect_celsius: f64,
    pub injector_pw_us: u32,
    pub iacv_duty_pct: f32,
    pub vtec_active: bool,
    pub note: &'static str,
}

pub struct ReplayEngine;

impl ReplayEngine {
    /// Play a CSV log file or built-in scenario preset through the ECU emulator
    pub fn replay(rom_path: &str, log_source: &str, out_csv_path: Option<&str>) -> io::Result<()> {
        let frames = if log_source.ends_with(".csv") {
            Self::parse_csv_file(log_source)?
        } else {
            Self::generate_preset_scenario(log_source)?
        };

        let mut bus = Bus::new();
        bus.load_rom_file(rom_path)?;

        let mut cpu = Cpu::new();
        cpu.pc = bus.read_code_u16(0x0000);
        cpu.ie = true;
        bus.write_data_u16(0x1A, 0xFFFF); // Enable IE

        let mut engine = EngineState::new();
        let mut results = Vec::new();

        println!("============================================================");
        println!("  HONDA ECU LOG PLAYBACK ENGINE ");
        println!("  Target ROM    : {}", rom_path);
        println!("  Log Source    : {}", log_source);
        println!("  Total Frames  : {}", frames.len());
        println!("============================================================");
        println!("{:>6}ms | {:>6} RPM | {:>5} kPa | {:>4}% TPS | {:>5}°C | {:>8} us | {:>6}% IACV | VTEC       | Phase / Note",
                 "Time", "RPM", "MAP", "TPS", "ECT", "Inj PW", "Duty");
        println!("---------------------------------------------------------------------------------------------------------");

        let mut last_ms: u64 = 0;

        for frame in &frames {
            let dt_ms = if frame.timestamp_ms > last_ms {
                frame.timestamp_ms - last_ms
            } else {
                50 // Default 50ms per frame
            };
            last_ms = frame.timestamp_ms;

            // Update virtual engine sensors
            engine.rpm = frame.rpm;
            engine.map_kpa = frame.map_kpa;
            engine.tps_pct = frame.tps_pct;
            engine.ect_celsius = frame.ect_celsius;
            engine.iat_celsius = frame.iat_celsius;
            engine.o2_volts = frame.o2_volts;
            engine.vbatt_volts = frame.vbatt_volts;
            engine.speed_kmh = frame.speed_kmh;
            engine.sync_sensors_to_bus(&mut bus);

            // Calculate cycles for dt_ms (12 MHz CPU = 12,000 cycles per ms)
            let frame_cycles = dt_ms * 12_000;
            let start_cycle = cpu.cycles;

            while cpu.cycles - start_cycle < frame_cycles {
                Interpreter::step(&mut cpu, &mut bus);
                cpu.ie = true; // Keep interrupts active

                let t_irq = bus.tick_timers(2);
                let d_irq = engine.check_distributor_pulses(cpu.cycles, 12_000_000);
                InterruptController::handle_pending_interrupts(&mut cpu, &mut bus, t_irq | d_irq);
                if cpu.halted {
                    break;
                }
            }

            let res = ReplayResultFrame {
                timestamp_ms: frame.timestamp_ms,
                rpm: frame.rpm,
                map_kpa: frame.map_kpa,
                tps_pct: frame.tps_pct,
                ect_celsius: frame.ect_celsius,
                injector_pw_us: bus.injector_pulse_width_us,
                iacv_duty_pct: bus.iacv_duty_cycle_pct,
                vtec_active: bus.vtec_solenoid_active,
                note: frame.note,
            };

            let vtec_str = if res.vtec_active { "[VTEC ON]" } else { "        " };
            println!("{:6}ms | {:6.0} RPM | {:5.1} kPa | {:4.0}% TPS | {:5.1}°C | {:8} us | {:6.1}% | {} | {}",
                     res.timestamp_ms, res.rpm, res.map_kpa, res.tps_pct, res.ect_celsius,
                     res.injector_pw_us, res.iacv_duty_pct, vtec_str, res.note);

            results.push(res);
        }

        println!("---------------------------------------------------------------------------------------------------------");
        println!("Log Replay Completed! Played {} frames successfully.", results.len());

        // Save output CSV if requested
        if let Some(out_path) = out_csv_path {
            let mut file = File::create(out_path)?;
            writeln!(file, "timestamp_ms,rpm,map_kpa,tps_pct,ect_celsius,injector_pw_us,iacv_duty_pct,vtec_active,note")?;
            for r in &results {
                writeln!(file, "{},{:.1},{:.1},{:.1},{:.1},{},{:.1},{},\"{}\"",
                         r.timestamp_ms, r.rpm, r.map_kpa, r.tps_pct, r.ect_celsius,
                         r.injector_pw_us, r.iacv_duty_pct, r.vtec_active as u8, r.note)?;
            }
            println!("Exported replay results to '{}'", out_path);
        }

        Ok(())
    }

    fn parse_csv_file(path: &str) -> io::Result<Vec<LogFrame>> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut frames = Vec::new();

        for (idx, line) in reader.lines().enumerate() {
            let l = line?;
            if idx == 0 && l.to_lowercase().contains("rpm") {
                continue; // Skip CSV header line
            }
            let parts: Vec<&str> = l.split(',').collect();
            if parts.len() >= 4 {
                let ms: u64 = parts[0].trim().parse().unwrap_or(idx as u64 * 50);
                let rpm: f64 = parts[1].trim().parse().unwrap_or(800.0);
                let map: f64 = parts[2].trim().parse().unwrap_or(30.0);
                let tps: f64 = parts[3].trim().parse().unwrap_or(0.0);
                let ect: f64 = parts.get(4).and_then(|s| s.trim().parse().ok()).unwrap_or(85.0);
                let iat: f64 = parts.get(5).and_then(|s| s.trim().parse().ok()).unwrap_or(25.0);
                let o2: f64  = parts.get(6).and_then(|s| s.trim().parse().ok()).unwrap_or(0.45);
                let vbatt: f64 = parts.get(7).and_then(|s| s.trim().parse().ok()).unwrap_or(14.2);
                let speed: f64 = parts.get(8).and_then(|s| s.trim().parse().ok()).unwrap_or(0.0);

                frames.push(LogFrame {
                    timestamp_ms: ms,
                    rpm,
                    map_kpa: map,
                    tps_pct: tps,
                    ect_celsius: ect,
                    iat_celsius: iat,
                    o2_volts: o2,
                    vbatt_volts: vbatt,
                    speed_kmh: speed,
                    note: "CSV Datalog Frame",
                });
            }
        }

        Ok(frames)
    }

    fn generate_preset_scenario(preset: &str) -> io::Result<Vec<LogFrame>> {
        let mut frames = Vec::new();

        match preset {
            // =========================================================================
            // FAULT & ERROR INJECTION SCENARIOS (Testing DTCs & Safety Failsafes)
            // =========================================================================

            // Error Scenario 1: MAP Sensor Hose Disconnect / Ground Short Under Load (DTC 3 / DTC 5)
            "error-map-failure" => {
                for i in 0..=50 {
                    let ms = i * 50;
                    let is_failed = i >= 15;
                    let map = if is_failed { 0.0 } else { 45.0 + (i as f64 * 1.5) };
                    let note = if is_failed {
                        "ERROR: MAP SENSOR DISCONNECT (0.0 kPa -> DTC 3 & DTC 5 Triggered!)"
                    } else {
                        "Normal Acceleration Under Load"
                    };

                    frames.push(LogFrame {
                        timestamp_ms: ms,
                        rpm: 3500.0,
                        map_kpa: map,
                        tps_pct: 45.0,
                        ect_celsius: 85.0,
                        iat_celsius: 25.0,
                        o2_volts: 0.45,
                        vbatt_volts: 14.1,
                        speed_kmh: 60.0,
                        note,
                    });
                }
            }

            // Error Scenario 2: ECT Engine Coolant Sensor Open Circuit / Severe Overheat (DTC 6)
            "error-ect-overheat" => {
                for i in 0..=50 {
                    let ms = i * 50;
                    let is_failed = i >= 15;
                    let ect = if is_failed { 135.0 } else { 85.0 }; // Extreme temperature / disconnected thermistor
                    let note = if is_failed {
                        "ERROR: ECT SENSOR OPEN CIRCUIT / OVERHEAT (135°C -> DTC 6 & Thermal Safety Retard Active!)"
                    } else {
                        "Normal Engine Temperature"
                    };

                    frames.push(LogFrame {
                        timestamp_ms: ms,
                        rpm: 3000.0,
                        map_kpa: 40.0,
                        tps_pct: 20.0,
                        ect_celsius: ect,
                        iat_celsius: 25.0,
                        o2_volts: 0.45,
                        vbatt_volts: 14.2,
                        speed_kmh: 50.0,
                        note,
                    });
                }
            }

            // Error Scenario 3: TPS Sensor Short Circuit under High MAP (DTC 7)
            "error-tps-short" => {
                for i in 0..=50 {
                    let ms = i * 50;
                    let is_shorted = i >= 15;
                    let tps = if is_shorted { 0.0 } else { 80.0 }; // TPS drops to 0% while MAP is 95 kPa
                    let note = if is_shorted {
                        "ERROR: TPS GROUND SHORT (0% TPS vs 95 kPa MAP -> DTC 7 Triggered!)"
                    } else {
                        "High Load Throttle"
                    };

                    frames.push(LogFrame {
                        timestamp_ms: ms,
                        rpm: 4200.0,
                        map_kpa: 95.0,
                        tps_pct: tps,
                        ect_celsius: 85.0,
                        iat_celsius: 25.0,
                        o2_volts: 0.85,
                        vbatt_volts: 14.0,
                        speed_kmh: 90.0,
                        note,
                    });
                }
            }

            // Error Scenario 4: Distributor CKP Sensor Signal Dropout (DTC 4 / DTC 8)
            "error-ckp-distributor-loss" => {
                for i in 0..=50 {
                    let ms = i * 50;
                    let is_loss = i >= 20;
                    let note = if is_loss {
                        "ERROR: DISTRIBUTOR CKP PULSE DROPOUT (RPM Signal Lost -> DTC 4 & Emergency Fuel Cut!)"
                    } else {
                        "Cruising at 4500 RPM"
                    };

                    frames.push(LogFrame {
                        timestamp_ms: ms,
                        rpm: if is_loss { 0.0 } else { 4500.0 },
                        map_kpa: 50.0,
                        tps_pct: 30.0,
                        ect_celsius: 85.0,
                        iat_celsius: 25.0,
                        o2_volts: 0.45,
                        vbatt_volts: 14.1,
                        speed_kmh: 80.0,
                        note,
                    });
                }
            }

            // Error Scenario 5: Low Oil Pressure VTEC Spool Valve Fault (DTC 22)
            "error-vtec-oil-pressure-loss" => {
                for i in 0..=50 {
                    let ms = i * 50;
                    let is_vtec_rpm = i >= 15;
                    let rpm = if is_vtec_rpm { 5500.0 } else { 4000.0 };
                    let note = if is_vtec_rpm {
                        "ERROR: VTEC COMMANDED BUT OIL PRESSURE SWITCH OPEN (0 psi -> DTC 22 Triggered!)"
                    } else {
                        "Low Cam Acceleration"
                    };

                    frames.push(LogFrame {
                        timestamp_ms: ms,
                        rpm,
                        map_kpa: 95.0,
                        tps_pct: 100.0,
                        ect_celsius: 85.0,
                        iat_celsius: 25.0,
                        o2_volts: 0.85,
                        vbatt_volts: 14.0,
                        speed_kmh: 100.0,
                        note,
                    });
                }
            }

            // Error Scenario 6: Alternator Failure & Severe Battery Voltage Dip (DTC 20)
            "error-alt-low-voltage" => {
                for i in 0..=50 {
                    let ms = i * 50;
                    let is_dip = i >= 15;
                    let vbatt = if is_dip { 8.5 } else { 14.2 };
                    let note = if is_dip {
                        "ERROR: ALTERNATOR BROWNOUT (8.5V Vbatt -> DTC 20 & Max Dead-Time Pulse Width Spike!)"
                    } else {
                        "Normal Alternator Output (14.2V)"
                    };

                    frames.push(LogFrame {
                        timestamp_ms: ms,
                        rpm: 1200.0,
                        map_kpa: 35.0,
                        tps_pct: 0.0,
                        ect_celsius: 85.0,
                        iat_celsius: 25.0,
                        o2_volts: 0.45,
                        vbatt_volts: vbatt,
                        speed_kmh: 0.0,
                        note,
                    });
                }
            }

            // Error Scenario 7: Stuck Lean Primary O2 Sensor (DTC 1 & DTC 43)
            "error-o2-lean-stuck" => {
                for i in 0..=50 {
                    let ms = i * 50;
                    let is_stuck = i >= 10;
                    let o2 = if is_stuck { 0.02 } else { 0.45 }; // 0.02V stuck lean
                    let note = if is_stuck {
                        "ERROR: O2 SENSOR STUCK LEAN (0.02V -> Closed-Loop Trim Maxed out -> DTC 1 & DTC 43 Triggered!)"
                    } else {
                        "Normal Closed Loop Operation"
                    };

                    frames.push(LogFrame {
                        timestamp_ms: ms,
                        rpm: 2800.0,
                        map_kpa: 40.0,
                        tps_pct: 20.0,
                        ect_celsius: 85.0,
                        iat_celsius: 25.0,
                        o2_volts: o2,
                        vbatt_volts: 14.1,
                        speed_kmh: 50.0,
                        note,
                    });
                }
            }

            // =========================================================================
            // NORMAL OPERATING SCENARIOS
            // =========================================================================

            // Overrun Deceleration Fuel Cut-Off (DFCO) Scenario
            "overrun-decel" => {
                for i in 0..=80 {
                    let ms = i * 50;
                    let pct = i as f64 / 80.0;
                    let rpm = 6500.0 - pct * 5700.0; // Decelerates 6500 -> 800 RPM
                    let tps = if i < 5 { 100.0 } else { 0.0 }; // Throttle snaps shut
                    let map = if i < 5 { 95.0 } else { 18.0 + (1.0 - pct) * 5.0 }; // High engine vacuum on overrun (18 kPa)
                    let note = if i < 5 {
                        "WOT High RPM"
                    } else if rpm > 1100.0 {
                        "Overrun DFCO (Fuel Cut Active)"
                    } else {
                        "DFCO Re-engagement / Anti-Stall Catch"
                    };

                    frames.push(LogFrame {
                        timestamp_ms: ms,
                        rpm,
                        map_kpa: map,
                        tps_pct: tps,
                        ect_celsius: 88.0,
                        iat_celsius: 30.0,
                        o2_volts: if rpm > 1100.0 && tps == 0.0 { 0.02 } else { 0.45 },
                        vbatt_volts: 14.2,
                        speed_kmh: (rpm / 6500.0) * 120.0,
                        note,
                    });
                }
            }

            // Downhill Engine Braking
            "overrun-downhill" => {
                for i in 0..=100 {
                    let ms = i * 50;
                    let cycle_phase = (i % 25) as f64 / 25.0;
                    let is_blip = cycle_phase > 0.7 && cycle_phase < 0.9;
                    let rpm = 4500.0 - (i as f64 * 25.0) + (if is_blip { 800.0 } else { 0.0 });
                    let tps = if is_blip { 25.0 } else { 0.0 };
                    let map = if is_blip { 55.0 } else { 20.0 };
                    let note = if is_blip { "Throttle Blip / Heel-Toe" } else { "Downhill Engine Braking (Overrun Cut)" };

                    frames.push(LogFrame {
                        timestamp_ms: ms,
                        rpm: rpm.max(800.0),
                        map_kpa: map,
                        tps_pct: tps,
                        ect_celsius: 85.0,
                        iat_celsius: 25.0,
                        o2_volts: if is_blip { 0.50 } else { 0.05 },
                        vbatt_volts: 14.1,
                        speed_kmh: 90.0,
                        note,
                    });
                }
            }

            // Accel Stomp
            "accel-stomp" => {
                for i in 0..=50 {
                    let ms = i * 50;
                    let is_stomp = i >= 10 && i <= 30;
                    let tps = if is_stomp { 100.0 } else { 15.0 };
                    let map = if is_stomp { 98.0 } else { 38.0 };
                    let rpm = 2500.0 + (if is_stomp { (i - 10) as f64 * 150.0 } else { 0.0 });
                    let note = if i < 10 {
                        "Steady Cruise (15% TPS)"
                    } else if i == 10 {
                        "RAPID TPS TIP-IN STOMP (Transient Enrichment)"
                    } else if i <= 30 {
                        "WOT Acceleration"
                    } else {
                        "Lift Back to Cruise"
                    };

                    frames.push(LogFrame {
                        timestamp_ms: ms,
                        rpm,
                        map_kpa: map,
                        tps_pct: tps,
                        ect_celsius: 85.0,
                        iat_celsius: 28.0,
                        o2_volts: if is_stomp { 0.90 } else { 0.45 },
                        vbatt_volts: 14.0,
                        speed_kmh: 50.0 + (i as f64),
                        note,
                    });
                }
            }

            // Drag Pass
            "drag-pass" => {
                for i in 0..=120 {
                    let ms = i * 50;
                    let sec = ms as f64 / 1000.0;

                    let (rpm, map, tps, speed, note) = if sec < 1.0 {
                        (4500.0, 95.0, 100.0, 0.0, "2-Step Launch Control (Stationary)")
                    } else if sec < 3.0 {
                        (4500.0 + (sec - 1.0) * 1750.0, 98.0, 100.0, (sec - 1.0) * 35.0, "1st Gear Full Acceleration")
                    } else if sec < 3.2 {
                        (5200.0, 30.0, 0.0, 70.0, "1st -> 2nd Gear Shift (Lift)")
                    } else if sec < 5.5 {
                        (5200.0 + (sec - 3.2) * 1300.0, 99.0, 100.0, 70.0 + (sec - 3.2) * 25.0, "2nd Gear Full Acceleration [VTEC]")
                    } else if sec < 5.7 {
                        (5500.0, 30.0, 0.0, 125.0, "2nd -> 3rd Gear Shift (Lift)")
                    } else if sec < 8.5 {
                        (5500.0 + (sec - 5.7) * 950.0, 100.0, 100.0, 125.0 + (sec - 5.7) * 20.0, "3rd Gear Power Band [VTEC]")
                    } else {
                        (8100.0 - (sec - 8.5) * 2000.0, 20.0, 0.0, 180.0, "Trap Finish Line High RPM Overrun (DFCO)")
                    };

                    frames.push(LogFrame {
                        timestamp_ms: ms,
                        rpm,
                        map_kpa: map,
                        tps_pct: tps,
                        ect_celsius: 86.0,
                        iat_celsius: 26.0,
                        o2_volts: if tps > 50.0 { 0.88 } else { 0.10 },
                        vbatt_volts: 14.1,
                        speed_kmh: speed,
                        note,
                    });
                }
            }

            // Electrical Load Idle
            "electrical-load-idle" => {
                for i in 0..=60 {
                    let ms = i * 100;
                    let ac_on = (i / 15) % 2 == 1;
                    let vbatt = if ac_on { 12.1 } else { 14.2 };
                    let map = if ac_on { 42.0 } else { 30.0 };
                    let note = if ac_on { "AC Compressor ON (IACV Duty Spike & Idle Compensation)" } else { "Idle Normal (AC OFF)" };

                    frames.push(LogFrame {
                        timestamp_ms: ms,
                        rpm: 800.0,
                        map_kpa: map,
                        tps_pct: 0.0,
                        ect_celsius: 92.0,
                        iat_celsius: 35.0,
                        o2_volts: 0.45,
                        vbatt_volts: vbatt,
                        speed_kmh: 0.0,
                        note,
                    });
                }
            }

            // Heat Soak Start
            "heat-soak-start" => {
                for i in 0..=40 {
                    let ms = i * 100;
                    let pct = i as f64 / 40.0;
                    let ect = 98.0 - pct * 8.0;
                    let iat = 60.0 - pct * 25.0;
                    let note = if i < 5 { "Hot Heat-Soak Cranking" } else { "Hot Idle Anti-Percolation Enrichment" };

                    frames.push(LogFrame {
                        timestamp_ms: ms,
                        rpm: 850.0,
                        map_kpa: 34.0,
                        tps_pct: 0.0,
                        ect_celsius: ect,
                        iat_celsius: iat,
                        o2_volts: 0.48,
                        vbatt_volts: 13.5,
                        speed_kmh: 0.0,
                        note,
                    });
                }
            }

            // Dyno Pull
            "dyno-pull" => {
                for i in 0..=100 {
                    let ms = i * 50;
                    let pct = i as f64 / 100.0;
                    let rpm = 2000.0 + pct * 6200.0;
                    let map = 30.0 + pct * 70.0;
                    let tps = if pct > 0.1 { 100.0 } else { pct * 1000.0 };

                    frames.push(LogFrame {
                        timestamp_ms: ms,
                        rpm,
                        map_kpa: map,
                        tps_pct: tps,
                        ect_celsius: 85.0,
                        iat_celsius: 25.0,
                        o2_volts: 0.85,
                        vbatt_volts: 14.1,
                        speed_kmh: pct * 180.0,
                        note: if rpm >= 4800.0 { "WOT VTEC High Cam Pull" } else { "WOT Low Cam Pull" },
                    });
                }
            }

            // Cold Start
            "cold-start" => {
                for i in 0..=100 {
                    let ms = i * 100;
                    let pct = i as f64 / 100.0;
                    let ect = -10.0 + pct * 95.0;
                    let rpm = 1400.0 - pct * 600.0;

                    frames.push(LogFrame {
                        timestamp_ms: ms,
                        rpm,
                        map_kpa: 35.0,
                        tps_pct: 0.0,
                        ect_celsius: ect,
                        iat_celsius: 10.0,
                        o2_volts: 0.45,
                        vbatt_volts: 13.8,
                        speed_kmh: 0.0,
                        note: "Cold Start Warmup Curve",
                    });
                }
            }

            // Fallback
            _ => {
                for i in 0..=20 {
                    frames.push(LogFrame {
                        timestamp_ms: i * 100,
                        rpm: 800.0,
                        map_kpa: 30.0,
                        tps_pct: 0.0,
                        ect_celsius: 85.0,
                        iat_celsius: 25.0,
                        o2_volts: 0.45,
                        vbatt_volts: 14.2,
                        speed_kmh: 0.0,
                        note: "Idle Steady State",
                    });
                }
            }
        }

        Ok(frames)
    }
}
