// ECU Datalog CSV Replay & Scripted Trace Engine

use crate::bus::Bus;
use crate::cpu::Cpu;
use crate::engine::EngineState;
use crate::interpreter::Interpreter;
use crate::interrupts::InterruptController;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};

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
    pub speed_kmh: f64,
    pub injector_pw_us: u32,
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
        if log_source == "launch-control-2step" && Self::is_stock_p28_230(&bus.rom) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "launch-control-2step requires a launch-enabled ROM; the OEM P28-230 ROM does not contain launch control",
            ));
        }

        let mut engine = EngineState::new();
        if let Some(frame) = frames.first() {
            Self::apply_frame_to_engine(frame, &mut engine);
        }
        engine.sync_sensors_to_bus(&mut bus);

        let mut cpu = Cpu::new();
        cpu.pc = bus.read_code_u16(0x0000);
        Self::boot_to_runtime(&mut cpu, &mut bus)?;

        let mut results = Vec::new();

        println!("============================================================");
        println!("  NOTE: timestamps use approximate instruction timing; see docs/CPU_CORE.md.");
        println!("  NOTE: VTEC/IACV telemetry is omitted until board pin mappings are verified.");
        println!("  HONDA ECU LOG PLAYBACK ENGINE ");
        println!("  Target ROM    : {}", rom_path);
        println!("  Log Source    : {}", log_source);
        println!("  Total Frames  : {}", frames.len());
        println!("============================================================");
        if log_source == "launch-control-2step" {
            println!(
                "  NOTE: launch-control-2step is an open-loop stimulus; use a launch-enabled ROM."
            );
        }
        println!(
            "{:>6}ms | {:>6} RPM | {:>5} kPa | {:>4}% TPS | {:>5}°C | {:>5} km/h | {:>8} us | Phase / Note",
            "Time", "RPM", "MAP", "TPS", "ECT", "Speed", "Inj PW"
        );
        println!(
            "---------------------------------------------------------------------------------------------------------------------"
        );

        let mut last_ms: u64 = 0;

        for frame in &frames {
            let dt_ms = if frame.timestamp_ms > last_ms {
                frame.timestamp_ms - last_ms
            } else {
                50 // Default 50ms per frame
            };
            last_ms = frame.timestamp_ms;

            Self::apply_frame_to_engine(frame, &mut engine);
            engine.sync_sensors_to_bus(&mut bus);
            Self::apply_fault_overrides(log_source, frame, &mut bus);

            // Approximate a 12 MHz budget. Exact per-instruction cycle counts
            // are not implemented yet (documented in docs/CPU_CORE.md).
            let frame_cycles = dt_ms * 12_000;
            let start_cycle = cpu.cycles;

            while cpu.cycles - start_cycle < frame_cycles {
                let instruction_start = cpu.cycles;
                let instruction = Interpreter::step(&mut cpu, &mut bus);
                if cpu.halted {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "ROM execution stopped during replay at {:#06X}: {}",
                            instruction.pc, instruction.mnemonic
                        ),
                    ));
                }

                let elapsed_ticks = (cpu.cycles - instruction_start) as u32;
                let t_irq = bus.tick_timers(elapsed_ticks);
                let d_irq = engine.check_distributor_pulses(cpu.cycles, 12_000_000);
                InterruptController::handle_pending_interrupts(&mut cpu, &mut bus, t_irq | d_irq);
            }

            let res = ReplayResultFrame {
                timestamp_ms: frame.timestamp_ms,
                rpm: frame.rpm,
                map_kpa: frame.map_kpa,
                tps_pct: frame.tps_pct,
                ect_celsius: frame.ect_celsius,
                speed_kmh: frame.speed_kmh,
                injector_pw_us: bus.injector_pulse_width_us,
                note: frame.note,
            };

            println!(
                "{:6}ms | {:6.0} RPM | {:5.1} kPa | {:4.0}% TPS | {:5.1}°C | {:5.1} km/h | {:8} us | {}",
                res.timestamp_ms,
                res.rpm,
                res.map_kpa,
                res.tps_pct,
                res.ect_celsius,
                res.speed_kmh,
                res.injector_pw_us,
                res.note
            );

            results.push(res);
        }

        println!(
            "---------------------------------------------------------------------------------------------------------------------"
        );
        println!(
            "Replay completed: processed {} stimulus frames (no behavior pass/fail implied).",
            results.len()
        );

        // Save output CSV if requested
        if let Some(out_path) = out_csv_path {
            let mut file = File::create(out_path)?;
            writeln!(
                file,
                "timestamp_ms,rpm,map_kpa,tps_pct,ect_celsius,speed_kmh,injector_pw_us,note"
            )?;
            for r in &results {
                writeln!(
                    file,
                    "{},{:.1},{:.1},{:.1},{:.1},{:.1},{},\"{}\"",
                    r.timestamp_ms,
                    r.rpm,
                    r.map_kpa,
                    r.tps_pct,
                    r.ect_celsius,
                    r.speed_kmh,
                    r.injector_pw_us,
                    r.note
                )?;
            }
            println!("Exported replay results to '{}'", out_path);
        }

        Ok(())
    }

    /// Run the ROM's power-on hardware checks. Callers configure the board
    /// inputs first, so the external U5/U6 mux scan sees the same sensor and
    /// reference network that subsequent runtime conversions use.
    pub(crate) fn boot_to_runtime(cpu: &mut Cpu, bus: &mut Bus) -> io::Result<()> {
        let Some(ready_pc) = Self::runtime_ready_pc(&bus.rom) else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "ROM does not contain a recognized P28 runtime-entry signature",
            ));
        };

        for _ in 0..5_000_000 {
            if cpu.pc == ready_pc {
                return Ok(());
            }

            let instruction_start = cpu.cycles;
            let instruction = Interpreter::step(cpu, bus);
            if cpu.halted {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "ROM execution stopped during boot at {:#06X}: {}",
                        instruction.pc, instruction.mnemonic
                    ),
                ));
            }
            let elapsed_ticks = (cpu.cycles - instruction_start) as u32;
            let irq = bus.tick_timers(elapsed_ticks);
            InterruptController::handle_pending_interrupts(cpu, bus, irq);
        }

        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("ROM did not finish its power-on checks (target {ready_pc:#06X})"),
        ))
    }

    /// Apply electrical failures that cannot be represented by the normal
    /// physical-unit sensor fields. For example, 0% throttle is a valid 0.5 V
    /// TPS signal; a ground short must drive the ADC channel itself to zero.
    fn apply_fault_overrides(log_source: &str, frame: &LogFrame, bus: &mut Bus) {
        if log_source == "error-tps-short" && frame.timestamp_ms >= 750 {
            bus.adc_inputs[crate::bus::P28_ADC_TPS] = 0;
        }

        if log_source == "error-vtec-oil-pressure-loss" {
            bus.vtec_pressure_switch = frame.timestamp_ms < 750;
        }
    }

    fn apply_frame_to_engine(frame: &LogFrame, engine: &mut EngineState) {
        engine.rpm = frame.rpm;
        engine.map_kpa = frame.map_kpa;
        engine.tps_pct = frame.tps_pct;
        engine.ect_celsius = frame.ect_celsius;
        engine.iat_celsius = frame.iat_celsius;
        engine.o2_volts = frame.o2_volts;
        engine.vbatt_volts = frame.vbatt_volts;
        engine.speed_kmh = frame.speed_kmh;
    }

    /// Locate the instruction immediately after the ROM's timer-integrity
    /// failure BRK. The RAM marker byte moves between P28-derived ROMs, so its
    /// direct address is wildcarded rather than hard-coding P28's 0xEB.
    fn runtime_ready_pc(rom: &[u8]) -> Option<u16> {
        rom.windows(6)
            .position(|w| {
                w[0] == 0xC5
                    && w[2] == 0x98
                    && w[3] == 0x4B
                    && w[4] == 0xFF
                    && (0x10..=0x17).contains(&w[5])
            })
            .and_then(|offset| u16::try_from(offset + 5).ok())
    }

    fn is_stock_p28_230(rom: &[u8]) -> bool {
        // FNV-1a is used only as a stable local identity check, not for
        // security. Any launch-control patch changes the fingerprint and is
        // allowed through to the stimulus runner.
        let fingerprint = rom.iter().fold(0xCBF2_9CE4_8422_2325u64, |hash, byte| {
            (hash ^ *byte as u64).wrapping_mul(0x0000_0100_0000_01B3)
        });
        fingerprint == 0x1AA8_776D_484A_5AD5
    }

    fn parse_csv_file(path: &str) -> io::Result<Vec<LogFrame>> {
        let file = File::open(path)?;
        Self::parse_csv(BufReader::new(file))
    }

    fn parse_csv(reader: impl BufRead) -> io::Result<Vec<LogFrame>> {
        let mut frames = Vec::new();

        for (idx, line) in reader.lines().enumerate() {
            let l = line?;
            let line_number = idx + 1;
            if l.trim().is_empty() {
                continue;
            }
            let lowercase = l.to_lowercase();
            if frames.is_empty() && lowercase.contains("timestamp") && lowercase.contains("rpm") {
                continue;
            }
            let parts: Vec<&str> = l.split(',').collect();
            if parts.len() < 4 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("CSV line {line_number} has fewer than four fields"),
                ));
            }

            let invalid = |field: &str| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid {field} on CSV line {line_number}"),
                )
            };
            let ms = parts[0]
                .trim()
                .parse()
                .map_err(|_| invalid("timestamp_ms"))?;
            let rpm = parts[1].trim().parse().map_err(|_| invalid("rpm"))?;
            let map = parts[2].trim().parse().map_err(|_| invalid("map_kpa"))?;
            let tps = parts[3].trim().parse().map_err(|_| invalid("tps_pct"))?;
            let ect = Self::optional_csv_f64(&parts, 4, 85.0, line_number, "ect_celsius")?;
            let iat = Self::optional_csv_f64(&parts, 5, 25.0, line_number, "iat_celsius")?;
            let o2 = Self::optional_csv_f64(&parts, 6, 0.45, line_number, "o2_volts")?;
            let vbatt = Self::optional_csv_f64(&parts, 7, 14.2, line_number, "vbatt_volts")?;
            let speed = Self::optional_csv_f64(&parts, 8, 0.0, line_number, "speed_kmh")?;

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

        if frames.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "CSV contains no data frames",
            ));
        }

        Ok(frames)
    }

    fn optional_csv_f64(
        parts: &[&str],
        index: usize,
        default: f64,
        line_number: usize,
        field: &str,
    ) -> io::Result<f64> {
        let Some(value) = parts.get(index).map(|value| value.trim()) else {
            return Ok(default);
        };
        if value.is_empty() {
            return Ok(default);
        }
        value.parse().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid {field} on CSV line {line_number}"),
            )
        })
    }

    fn generate_preset_scenario(preset: &str) -> io::Result<Vec<LogFrame>> {
        let mut frames = Vec::new();

        match preset {
            // =========================================================================
            // FAULT STIMULI. Notes name response targets; they do not assert
            // that the ROM set a DTC or entered a failsafe.
            // =========================================================================

            // Error Scenario 1: MAP Sensor Hose Disconnect / Ground Short Under Load (DTC 3 / DTC 5)
            "error-map-failure" => {
                for i in 0..=50 {
                    let ms = i * 50;
                    let is_failed = i >= 15;
                    let map = if is_failed {
                        0.0
                    } else {
                        45.0 + (i as f64 * 1.5)
                    };
                    let note = if is_failed {
                        "MAP DISCONNECT STIMULUS (0.0 kPa; inspect DTC 3/5 state)"
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
                        "ECT HIGH/OPEN-CIRCUIT STIMULUS (135°C; inspect DTC 6 and ignition output)"
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
                        "TPS GROUND-SHORT STIMULUS ACTIVE (ADC1 forced to 0 counts)"
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
                        "CKP/RPM DROPOUT STIMULUS (inspect DTC 4/8 and injector output)"
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
                        "VTEC OIL-PRESSURE-SWITCH STIMULUS OPEN (DTC response not asserted)"
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
                        "LOW-VOLTAGE STIMULUS (8.5V; inspect compensation and DTC state)"
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
                        "O2 STUCK-LEAN STIMULUS (0.02V; inspect trim and DTC state)"
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
                    let map = if i < 5 {
                        95.0
                    } else {
                        18.0 + (1.0 - pct) * 5.0
                    }; // High engine vacuum on overrun (18 kPa)
                    let note = if i < 5 {
                        "WOT High RPM"
                    } else if rpm > 1100.0 {
                        "Overrun input region (inspect injector output for DFCO)"
                    } else {
                        "Low-RPM overrun region (inspect injector re-engagement)"
                    };

                    frames.push(LogFrame {
                        timestamp_ms: ms,
                        rpm,
                        map_kpa: map,
                        tps_pct: tps,
                        ect_celsius: 88.0,
                        iat_celsius: 30.0,
                        o2_volts: if rpm > 1100.0 && tps == 0.0 {
                            0.02
                        } else {
                            0.45
                        },
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
                    let note = if is_blip {
                        "Throttle Blip / Heel-Toe"
                    } else {
                        "Downhill overrun stimulus (inspect injector output)"
                    };

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
                    let is_stomp = (10..=30).contains(&i);
                    let tps = if is_stomp { 100.0 } else { 15.0 };
                    let map = if is_stomp { 98.0 } else { 38.0 };
                    let rpm = 2500.0
                        + (if is_stomp {
                            (i - 10) as f64 * 150.0
                        } else {
                            0.0
                        });
                    let note = if i < 10 {
                        "Steady Cruise (15% TPS)"
                    } else if i == 10 {
                        "RAPID TPS TIP-IN STIMULUS (inspect transient fueling)"
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
                        (
                            4500.0,
                            95.0,
                            100.0,
                            0.0,
                            "Stationary high-load launch stimulus",
                        )
                    } else if sec < 3.0 {
                        (
                            4500.0 + (sec - 1.0) * 1750.0,
                            98.0,
                            100.0,
                            (sec - 1.0) * 35.0,
                            "1st Gear Full Acceleration",
                        )
                    } else if sec < 3.2 {
                        (5200.0, 30.0, 0.0, 70.0, "1st -> 2nd Gear Shift (Lift)")
                    } else if sec < 5.5 {
                        (
                            5200.0 + (sec - 3.2) * 1300.0,
                            99.0,
                            100.0,
                            70.0 + (sec - 3.2) * 25.0,
                            "2nd Gear Full Acceleration (inspect VTEC output)",
                        )
                    } else if sec < 5.7 {
                        (5500.0, 30.0, 0.0, 125.0, "2nd -> 3rd Gear Shift (Lift)")
                    } else if sec < 8.5 {
                        (
                            5500.0 + (sec - 5.7) * 950.0,
                            100.0,
                            100.0,
                            125.0 + (sec - 5.7) * 20.0,
                            "3rd Gear High-RPM Region (inspect VTEC output)",
                        )
                    } else {
                        (
                            8100.0 - (sec - 8.5) * 2000.0,
                            20.0,
                            0.0,
                            180.0,
                            "Finish-line overrun stimulus (inspect DFCO)",
                        )
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
                    let note = if ac_on {
                        "Electrical-load stimulus (inspect idle-control response)"
                    } else {
                        "Idle Normal (AC OFF)"
                    };

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
                    let note = if i < 5 {
                        "Hot Heat-Soak Cranking"
                    } else {
                        "Hot-idle temperature trace (inspect fueling response)"
                    };

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
                        note: if rpm >= 4800.0 {
                            "WOT above candidate VTEC region (inspect output)"
                        } else {
                            "WOT below candidate VTEC region"
                        },
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

            // Launch-control stimulus. The imposed RPM oscillation is an input
            // trace for inspecting limiter behavior, not evidence that the ROM
            // generated the oscillation itself.
            "launch-control-2step" => {
                for i in 0..=190 {
                    let ms = i * 50;
                    let sec = ms as f64 / 1000.0;

                    let (rpm, map, tps, speed, note) = if sec < 1.0 {
                        (900.0, 30.0, 0.0, 0.0, "Pre-Launch Idle (Brake Held)")
                    } else if sec < 1.3 {
                        let pct = (sec - 1.0) / 0.3;
                        (
                            900.0 + pct * 3300.0,
                            60.0 + pct * 35.0,
                            100.0,
                            0.0,
                            "Throttle Stab - Candidate 2-Step Region",
                        )
                    } else if sec < 5.0 {
                        let osc = (2.0 * std::f64::consts::PI * (sec - 1.3) / 0.4).sin();
                        (
                            4200.0 + osc * 250.0,
                            95.0,
                            100.0,
                            0.0,
                            "Scripted Stationary RPM Oscillation (inspect limiter output)",
                        )
                    } else if sec < 5.2 {
                        let pct = (sec - 5.0) / 0.2;
                        (
                            4200.0,
                            96.0,
                            100.0,
                            pct * 15.0,
                            "Launch - Clutch Drop / Wheel Speed Rising",
                        )
                    } else if sec < 8.0 {
                        let pct = (sec - 5.2) / 2.8;
                        (
                            4200.0 + pct * 4000.0,
                            99.0,
                            100.0,
                            15.0 + pct * 75.0,
                            "1st Gear Acceleration (inspect limiter release)",
                        )
                    } else if sec < 8.3 {
                        (
                            8200.0 - (sec - 8.0) * 3000.0,
                            20.0,
                            0.0,
                            90.0,
                            "Off-Throttle Abort (inspect re-latch behavior)",
                        )
                    } else {
                        let pct = (sec - 8.3) / 1.2;
                        (
                            7300.0 - pct * 5900.0,
                            25.0,
                            0.0,
                            (90.0 - pct * 90.0).max(0.0),
                            "Coast Down After Aborted Launch",
                        )
                    };

                    frames.push(LogFrame {
                        timestamp_ms: ms,
                        rpm,
                        map_kpa: map,
                        tps_pct: tps,
                        ect_celsius: 88.0,
                        iat_celsius: 27.0,
                        o2_volts: if tps > 50.0 { 0.85 } else { 0.15 },
                        vbatt_volts: 14.1,
                        speed_kmh: speed,
                        note,
                    });
                }
            }

            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown replay preset '{preset}'"),
                ));
            }
        }

        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use super::ReplayEngine;
    use crate::bus::{Bus, P28_MUX_U5_ADCR1, P28_U5_BOOT_CHECK_SELECT};
    use crate::cpu::Cpu;
    use crate::engine::EngineState;
    use crate::interpreter::Interpreter;
    use crate::interrupts::InterruptController;

    fn external_rom(env_name: &str, candidates: &[&str]) -> std::path::PathBuf {
        if let Some(path) = std::env::var_os(env_name).map(std::path::PathBuf::from) {
            return path;
        }
        candidates
            .iter()
            .map(std::path::PathBuf::from)
            .find(|path| path.is_file())
            .unwrap_or_else(|| {
                panic!(
                    "external ROM fixture unavailable; set {env_name} or provide one of: {}",
                    candidates.join(", ")
                )
            })
    }

    #[test]
    #[ignore = "requires an external OEM P28 ROM fixture"]
    fn finds_p28_runtime_entry_after_the_hardware_checks() {
        let path = external_rom(
            "HONDAECU_P28_ROM",
            &["../P28-230.bin", "../decomp/p28-230/P28_recompiled.bin"],
        );
        let rom = std::fs::read(path).expect("read OEM P28 ROM");

        assert_eq!(ReplayEngine::runtime_ready_pc(&rom), Some(0x3946));
        assert!(ReplayEngine::is_stock_p28_230(&rom[..32_768]));
    }

    #[test]
    #[ignore = "requires an external OEM P28 ROM fixture"]
    fn stock_p28_boots_with_live_sensor_inputs_through_the_mux() {
        let path = external_rom(
            "HONDAECU_P28_ROM",
            &["../P28-230.bin", "../decomp/p28-230/P28_recompiled.bin"],
        );
        let mut bus = Bus::new();
        bus.load_rom_file(path.to_str().expect("UTF-8 ROM path"))
            .expect("load OEM P28 ROM");

        let mut engine = EngineState::new();
        engine.map_kpa = 100.0;
        engine.tps_pct = 50.0;
        engine.ect_celsius = 85.0;
        engine.sync_sensors_to_bus(&mut bus);

        let mut cpu = Cpu::new();
        cpu.pc = bus.read_code_u16(0);
        ReplayEngine::boot_to_runtime(&mut cpu, &mut bus)
            .expect("stock P28 should pass its mux self-test");

        assert_eq!(cpu.pc, 0x3946);
        // The ROM stores U5/ADCR1 samples at 0x03C6 + P2[7:5]. Slot 3 is the
        // input read by the boot check at 0x530C; its clean board-model default
        // is mid-scale, which reads as 0x80 in ADCR1H.
        let boot_check_high =
            (bus.p28_mux_inputs[P28_MUX_U5_ADCR1][P28_U5_BOOT_CHECK_SELECT] >> 2) as u8;
        assert_eq!(bus.ram[0x03C6 + P28_U5_BOOT_CHECK_SELECT], boot_check_high);
        assert_ne!(bus.ram[0x00EB], 0x49, "boot fault 0x49 must not latch");
    }

    #[test]
    #[ignore = "requires an external launch-control ROM fixture"]
    fn finds_launch_rom_runtime_entry_after_the_hardware_checks() {
        let path = external_rom(
            "HONDAECU_LAUNCH_ROM",
            &["../test-roms/lyonnel-2.0-launch-ok.bin"],
        );
        let rom = std::fs::read(path).expect("read launch-control ROM");

        assert_eq!(ReplayEngine::runtime_ready_pc(&rom), Some(0x3475));
    }

    #[test]
    fn launch_preset_contains_stationary_hold_release_and_abort_phases() {
        let frames =
            ReplayEngine::generate_preset_scenario("launch-control-2step").expect("preset");

        assert!(frames.iter().any(|f| {
            f.timestamp_ms >= 1300
                && f.timestamp_ms < 5000
                && f.speed_kmh == 0.0
                && f.tps_pct == 100.0
        }));
        assert!(
            frames
                .iter()
                .any(|f| f.timestamp_ms >= 5200 && f.speed_kmh > 0.0 && f.rpm > 4200.0)
        );
        assert!(
            frames
                .iter()
                .any(|f| f.timestamp_ms >= 8000 && f.speed_kmh > 0.0 && f.tps_pct == 0.0)
        );
    }

    #[test]
    fn unknown_preset_is_rejected_instead_of_silently_running_idle() {
        let error = ReplayEngine::generate_preset_scenario("dyno-pul")
            .expect_err("misspelled preset should fail");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("dyno-pul"));
    }

    #[test]
    fn malformed_or_empty_csv_is_rejected() {
        let malformed = "timestamp_ms,rpm,map_kpa,tps_pct\n0,nope,30,0\n";
        let error = ReplayEngine::parse_csv(std::io::Cursor::new(malformed))
            .expect_err("invalid numeric field should fail");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

        let header_only = "timestamp_ms,rpm,map_kpa,tps_pct\n";
        let error = ReplayEngine::parse_csv(std::io::Cursor::new(header_only))
            .expect_err("header-only CSV should fail");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn tps_short_preset_forces_adc_below_valid_closed_throttle() {
        let frames = ReplayEngine::generate_preset_scenario("error-tps-short").expect("preset");
        let frame = frames
            .iter()
            .find(|frame| frame.timestamp_ms >= 750)
            .expect("fault frame");
        let mut bus = Bus::new();
        let mut engine = EngineState::new();
        engine.tps_pct = frame.tps_pct;
        engine.sync_sensors_to_bus(&mut bus);
        assert_eq!(
            bus.adc_inputs[crate::bus::P28_ADC_TPS],
            102,
            "0% TPS remains a valid 0.5 V signal"
        );

        ReplayEngine::apply_fault_overrides("error-tps-short", frame, &mut bus);

        assert_eq!(bus.adc_inputs[crate::bus::P28_ADC_TPS], 0);
    }

    #[test]
    #[ignore = "requires an external launch-control ROM fixture"]
    fn launch_rom_reaches_runtime_and_drives_an_injector() {
        let path = external_rom(
            "HONDAECU_LAUNCH_ROM",
            &["../test-roms/lyonnel-2.0-launch-ok.bin"],
        );
        let mut bus = Bus::new();
        bus.load_rom_file(path.to_str().expect("UTF-8 ROM path"))
            .expect("load launch-control ROM");
        let mut cpu = Cpu::new();
        cpu.pc = bus.read_code_u16(0);
        let mut engine = EngineState::new();
        engine.rpm = 3_500.0;
        engine.map_kpa = 100.0;
        engine.tps_pct = 100.0;
        engine.ect_celsius = 88.0;
        engine.speed_kmh = 0.0;
        engine.sync_sensors_to_bus(&mut bus);

        let mut reached_runtime = false;
        let mut dispatched = 0u64;

        for _ in 0..1_600_000 {
            if cpu.pc == 0x2295 || cpu.pc == 0x05EA {
                reached_runtime = true;
            }
            assert!(
                !matches!(
                    cpu.pc,
                    0x0098
                        | 0x22AE
                        | 0x235E
                        | 0x240F
                        | 0x2488
                        | 0x32EE
                        | 0x33C7
                        | 0x3474
                        | 0x450B
                        | 0x4C98
                        | 0x4CE5
                ),
                "launch ROM entered a BRK fault at {:#06X}",
                cpu.pc
            );

            let instruction = Interpreter::step(&mut cpu, &mut bus);
            assert!(
                !cpu.halted,
                "halted at {:#06X}: {}",
                instruction.pc, instruction.mnemonic
            );
            let irq = bus.tick_timers(2) | engine.check_distributor_pulses(cpu.cycles, 12_000_000);
            dispatched +=
                InterruptController::handle_pending_interrupts(&mut cpu, &mut bus, irq) as u64;

            if reached_runtime && bus.injector_pulse_width_us > 0 {
                break;
            }
        }

        assert!(reached_runtime, "launch ROM never reached its runtime loop");
        assert!(dispatched > 0, "launch ROM never accepted an interrupt");
        assert!(
            bus.injector_pulse_width_us > 0,
            "launch ROM never completed an injector pulse"
        );
    }
}
