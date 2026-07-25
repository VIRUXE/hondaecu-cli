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
        println!("{:>6}ms | {:>6} RPM | {:>5} kPa | {:>4}% TPS | {:>5}°C | {:>8} us | {:>6}% IACV | VTEC",
                 "Time", "RPM", "MAP", "TPS", "ECT", "Inj PW", "Duty");
        println!("-------------------------------------------------------------------------------");

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
            };

            let vtec_str = if res.vtec_active { "[VTEC ON]" } else { "        " };
            println!("{:6}ms | {:6.0} RPM | {:5.1} kPa | {:4.0}% TPS | {:5.1}°C | {:8} us | {:6.1}% | {}",
                     res.timestamp_ms, res.rpm, res.map_kpa, res.tps_pct, res.ect_celsius,
                     res.injector_pw_us, res.iacv_duty_pct, vtec_str);

            results.push(res);
        }

        println!("-------------------------------------------------------------------------------");
        println!("Log Replay Completed! Played {} frames successfully.", results.len());

        // Save output CSV if requested
        if let Some(out_path) = out_csv_path {
            let mut file = File::create(out_path)?;
            writeln!(file, "timestamp_ms,rpm,map_kpa,tps_pct,ect_celsius,injector_pw_us,iacv_duty_pct,vtec_active")?;
            for r in &results {
                writeln!(file, "{},{:.1},{:.1},{:.1},{:.1},{},{:.1},{}",
                         r.timestamp_ms, r.rpm, r.map_kpa, r.tps_pct, r.ect_celsius,
                         r.injector_pw_us, r.iacv_duty_pct, r.vtec_active as u8)?;
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
                });
            }
        }

        Ok(frames)
    }

    fn generate_preset_scenario(preset: &str) -> io::Result<Vec<LogFrame>> {
        let mut frames = Vec::new();

        match preset {
            "dyno-pull" => {
                // Dyno Pull from 2000 RPM to 8200 RPM over 5 seconds (50ms frames = 100 frames)
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
                        o2_volts: 0.85, // WOT rich AFR
                        vbatt_volts: 14.1,
                        speed_kmh: pct * 180.0,
                    });
                }
            }

            "cold-start" => {
                // Cold start from -10°C to 85°C over 10 seconds
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
                    });
                }
            }

            "overheat" => {
                // Thermal overheat test from 85°C to 118°C
                for i in 0..=50 {
                    let ms = i * 100;
                    let pct = i as f64 / 50.0;
                    let ect = 85.0 + pct * 33.0;

                    frames.push(LogFrame {
                        timestamp_ms: ms,
                        rpm: 4000.0,
                        map_kpa: 60.0,
                        tps_pct: 30.0,
                        ect_celsius: ect,
                        iat_celsius: 35.0,
                        o2_volts: 0.45,
                        vbatt_volts: 14.0,
                        speed_kmh: 80.0,
                    });
                }
            }

            _ => {
                // Generic idle scenario fallback
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
                    });
                }
            }
        }

        Ok(frames)
    }
}
