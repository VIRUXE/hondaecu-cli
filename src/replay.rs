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
    pub speed_kmh: f64,
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
        println!("{:>6}ms | {:>6} RPM | {:>5} kPa | {:>4}% TPS | {:>5}°C | {:>5} km/h | {:>8} us | {:>6}% IACV | VTEC       | Phase / Note",
                 "Time", "RPM", "MAP", "TPS", "ECT", "Speed", "Inj PW", "Duty");
        println!("---------------------------------------------------------------------------------------------------------------------");

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

            // Convert the frame interval to modeled 5 MHz machine cycles.
            let frame_cycles = dt_ms * (crate::bus::CPU_HZ / 1000);
            let start_cycle = cpu.cycles;

            while cpu.cycles - start_cycle < frame_cycles {
                let before = cpu.cycles;
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

                let t_irq = bus.tick_timers((cpu.cycles - before) as u32);
                let d_irq = engine.check_distributor_pulses(&mut bus, cpu.cycles, crate::bus::CPU_HZ);
                InterruptController::handle_pending_interrupts(&mut cpu, &mut bus, t_irq | d_irq);
            }

            let res = ReplayResultFrame {
                timestamp_ms: frame.timestamp_ms,
                rpm: frame.rpm,
                map_kpa: frame.map_kpa,
                tps_pct: frame.tps_pct,
                ect_celsius: frame.ect_celsius,
                speed_kmh: frame.speed_kmh,
                // 0 while the ROM is cutting fuel, rather than the stale width
                // of the last pulse before the cut.
                injector_pw_us: bus.live_injector_pulse_width_us(frame.rpm),
                iacv_duty_pct: bus.iacv_duty_cycle_pct,
                vtec_active: bus.vtec_solenoid_active,
                note: frame.note,
            };

            let vtec_str = if res.vtec_active { "[VTEC ON]" } else { "        " };
            println!("{:6}ms | {:6.0} RPM | {:5.1} kPa | {:4.0}% TPS | {:5.1}°C | {:5.1} km/h | {:8} us | {:6.1}% | {} | {}",
                     res.timestamp_ms, res.rpm, res.map_kpa, res.tps_pct, res.ect_celsius, res.speed_kmh,
                     res.injector_pw_us, res.iacv_duty_pct, vtec_str, res.note);

            results.push(res);
        }

        println!("---------------------------------------------------------------------------------------------------------------------");
        println!(
            "Replay completed: processed {} stimulus frames (no behavior pass/fail implied).",
            results.len()
        );

        // Save output CSV if requested
        if let Some(out_path) = out_csv_path {
            let mut file = File::create(out_path)?;
            writeln!(file, "timestamp_ms,rpm,map_kpa,tps_pct,ect_celsius,speed_kmh,injector_pw_us,iacv_duty_pct,vtec_active,note")?;
            for r in &results {
                writeln!(file, "{},{:.1},{:.1},{:.1},{:.1},{:.1},{},{:.1},{},\"{}\"",
                         r.timestamp_ms, r.rpm, r.map_kpa, r.tps_pct, r.ect_celsius,
                         r.speed_kmh, r.injector_pw_us, r.iacv_duty_pct, r.vtec_active as u8, r.note)?;
            }
            println!("Exported replay results to '{}'", out_path);
        }

        Ok(())
    }

    /// Run the ROM's power-on hardware checks. Callers configure and sync the
    /// board inputs first, so the mux scan observes the same sensor state that
    /// subsequent runtime conversions use.
    fn boot_to_runtime(cpu: &mut Cpu, bus: &mut Bus) -> io::Result<()> {
        let Some(ready_pc) = Self::runtime_ready_pc(&bus.rom) else {
            return Ok(());
        };

        for _ in 0..5_000_000 {
            if cpu.pc == ready_pc {
                return Ok(());
            }

            let before = cpu.cycles;
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
            let irq = bus.tick_timers((cpu.cycles - before) as u32);
            InterruptController::handle_pending_interrupts(cpu, bus, irq);
        }

        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("ROM did not finish its power-on checks (target {ready_pc:#06X})"),
        ))
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

    /// Python-drivable execution-trace primitive for emulator/ROM exploration.
    /// Boots the ROM to runtime, holds `rpm` with warm running sensors, warms
    /// up 0.5 s, then runs `secs` of engine-time emitting whitespace-delimited
    /// lines to stdout for the requested `mode`:
    ///   ramwatch <a,a,..> : `RAMW pc addr old new cycle` on each change (hex; cycle decimal)
    ///   pcwatch  <p,p,..> : `PCW pc A LRB DP X1 cf` each time PC hits
    ///   pccount  <p,p,..> : `PCCNT pc count` at end
    ///   pchist   [n]      : `PCHIST pc count` top-n at end
    ///   snap     <lo> <hi>: `SNAP lo <hexbytes>` at end
    ///   exec     <lo> <hi>: `EXEC pc mnemonic` for executed PCs in [lo,hi]
    /// All addresses are absolute bus.ram indices (remember `off N8` variables
    /// live at ((LRB>>5)<<8)|N8, not N8).
    pub fn trace(rom_path: &str, mode: &str, rpm: f64, secs: f64, rest: &[String]) -> io::Result<()> {
        let hz = crate::bus::CPU_HZ;
        let hx = |s: &str| u16::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok();
        let list = |s: Option<&String>| -> Vec<u16> {
            s.map(|s| s.split(',').filter_map(|x| hx(x)).collect()).unwrap_or_default()
        };

        let mut bus = Bus::new();
        bus.load_rom_file(rom_path)?;
        let mut cpu = Cpu::new();
        cpu.pc = bus.read_code_u16(0x0000);

        // Boot-phase tracing intentionally uses the board defaults so bootpc
        // and bootram expose the raw power-on path without runtime stimuli.
        if mode == "bootpc" || mode == "bootram" {
            let wpc = list(rest.first());
            let wram: Vec<usize> = list(rest.first()).iter().map(|&a| a as usize)
                .filter(|&a| a < bus.ram.len()).collect();
            let mut pram: Vec<u8> = wram.iter().map(|&a| bus.ram[a]).collect();
            let ready = Self::runtime_ready_pc(&bus.rom);
            for _ in 0..8_000_000u64 {
                if Some(cpu.pc) == ready || cpu.halted { break; }
                let pc = cpu.pc;
                if mode == "bootpc" && wpc.contains(&pc) {
                    let pb = (0x0080 + cpu.scb() * 8) as usize;
                    let dp = bus.ram[pb + 4] as u16 | ((bus.ram[pb + 5] as u16) << 8);
                    let x1 = bus.ram[pb] as u16 | ((bus.ram[pb + 1] as u16) << 8);
                    println!("PCW {:04X} A={:04X} LRB={:04X} DP={:04X} X1={:04X} cf={}",
                        pc, cpu.a, cpu.lrb, dp, x1, cpu.cf as u8);
                }
                let before = cpu.cycles;
                Interpreter::step(&mut cpu, &mut bus);
                if mode == "bootram" {
                    for (i, &a) in wram.iter().enumerate() {
                        let now = bus.ram[a];
                        if now != pram[i] {
                            println!("RAMW {:04X} {:04X} {:02X} {:02X}", pc, a, pram[i], now);
                            pram[i] = now;
                        }
                    }
                }
                let irq = bus.tick_timers((cpu.cycles - before) as u32);
                InterruptController::handle_pending_interrupts(&mut cpu, &mut bus, irq);
            }
            return Ok(());
        }

        let mut engine = EngineState::new();
        engine.rpm = rpm;
        engine.map_kpa = 90.0;
        engine.tps_pct = 65.0;
        engine.ect_celsius = 88.0;
        engine.iat_celsius = 30.0;
        engine.vbatt_volts = 14.2;
        engine.sync_sensors_to_bus(&mut bus);

        Self::boot_to_runtime(&mut cpu, &mut bus)?;

        let step = |cpu: &mut Cpu, bus: &mut Bus, engine: &mut EngineState| -> crate::interpreter::DisasmInstruction {
            let before = cpu.cycles;
            let ins = Interpreter::step(cpu, bus);
            let t = bus.tick_timers((cpu.cycles - before) as u32);
            let d = engine.check_distributor_pulses(bus, cpu.cycles, hz);
            InterruptController::handle_pending_interrupts(cpu, bus, t | d);
            ins
        };

        let warm = cpu.cycles + hz / 2;
        while cpu.cycles < warm && !cpu.halted {
            step(&mut cpu, &mut bus, &mut engine);
        }

        let watch_ram: Vec<usize> = if mode == "ramwatch" {
            list(rest.first()).iter().map(|&a| a as usize).filter(|&a| a < bus.ram.len()).collect()
        } else {
            vec![]
        };
        let mut prev_ram: Vec<u8> = watch_ram.iter().map(|&a| bus.ram[a]).collect();
        let watch_pc = list(rest.first());
        let lo = rest.first().and_then(|s| hx(s)).unwrap_or(0);
        let hi = rest.get(1).and_then(|s| hx(s)).unwrap_or(0);
        let mut pc_counts: std::collections::HashMap<u16, u64> = std::collections::HashMap::new();
        let mut hist: std::collections::HashMap<u16, u64> = std::collections::HashMap::new();
        let mut exec_lines = 0u64;
        // FORCE=addr:val env hook: pin a RAM byte each step (gate-hypothesis tests).
        let force: Option<(usize, u8)> = std::env::var("FORCE").ok().and_then(|s| {
            let (a, v) = s.split_once(':')?;
            Some((
                usize::from_str_radix(a.trim().trim_start_matches("0x"), 16).ok()?,
                u8::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok()?,
            ))
        });
        let opens_before = bus.injector_open_count;

        let end = cpu.cycles + (secs * hz as f64) as u64;
        while cpu.cycles < end && !cpu.halted {
            if let Some((a, v)) = force { if a < bus.ram.len() { bus.ram[a] = v; } }
            let pc = cpu.pc;
            match mode {
                "pchist" => { *hist.entry(pc).or_default() += 1; }
                "pccount" if watch_pc.contains(&pc) => { *pc_counts.entry(pc).or_default() += 1; }
                "pcwatch" if watch_pc.contains(&pc) => {
                    let pb = (0x0080 + cpu.scb() * 8) as usize;
                    let dp = bus.ram[pb + 4] as u16 | ((bus.ram[pb + 5] as u16) << 8);
                    let x1 = bus.ram[pb] as u16 | ((bus.ram[pb + 1] as u16) << 8);
                    println!("PCW {:04X} A={:04X} LRB={:04X} DP={:04X} X1={:04X} cf={}",
                        pc, cpu.a, cpu.lrb, dp, x1, cpu.cf as u8);
                }
                _ => {}
            }
            let ins = step(&mut cpu, &mut bus, &mut engine);
            if mode == "exec" && pc >= lo && pc <= hi && exec_lines < 400 {
                println!("EXEC {:04X} {}", pc, ins.mnemonic);
                exec_lines += 1;
            }
            if mode == "ramwatch" {
                for (i, &a) in watch_ram.iter().enumerate() {
                    let now = bus.ram[a];
                    if now != prev_ram[i] {
                        println!("RAMW {:04X} {:04X} {:02X} {:02X} {}", pc, a, prev_ram[i], now, cpu.cycles);
                        prev_ram[i] = now;
                    }
                }
            }
        }

        match mode {
            "injcount" => {
                // Counted on the latched injector outputs, not raw P2 writes.
                let opens: Vec<u64> = (0..4)
                    .map(|i| bus.injector_open_count[i] - opens_before[i])
                    .collect();
                let tot: u64 = opens.iter().sum();
                println!("INJ {} {} {} {} total {}", opens[0], opens[1], opens[2], opens[3], tot);
                let pw = bus.injector_last_pulse_us;
                println!("INJPW_US {} {} {} {}", pw[0], pw[1], pw[2], pw[3]);
            }
            "pccount" => for pc in &watch_pc {
                println!("PCCNT {:04X} {}", pc, pc_counts.get(pc).copied().unwrap_or(0));
            },
            "pchist" => {
                let n: usize = rest.first().and_then(|s| s.parse().ok()).unwrap_or(25);
                let mut v: Vec<_> = hist.into_iter().collect();
                v.sort_by(|a, b| b.1.cmp(&a.1));
                for (pc, c) in v.into_iter().take(n) { println!("PCHIST {pc:04X} {c}"); }
            }
            "snap" => {
                let bytes: String = (lo..=hi).map(|a| format!("{:02X}", bus.ram[a as usize])).collect();
                println!("SNAP {lo:04X} {bytes}");
            }
            _ => {}
        }
        Ok(())
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

            // Launch Control 2-Step Rev Limiter Test
            // Isolates the 2-step limiter behavior that `drag-pass` only touches briefly:
            // idle -> throttle stab into the stationary limiter -> sustained bounce
            // (brake/clutch held, TPS pinned, speed=0) -> clutch drop / launch ->
            // 2-step release under load -> an off-throttle abort to confirm the
            // limiter does NOT re-latch once the car is rolling.
            "launch-control-2step" => {
                for i in 0..=190 {
                    let ms = i * 50;
                    let sec = ms as f64 / 1000.0;

                    let (rpm, map, tps, speed, note) = if sec < 1.0 {
                        (900.0, 30.0, 0.0, 0.0, "Pre-Launch Idle (Brake Held)")
                    } else if sec < 1.3 {
                        let pct = (sec - 1.0) / 0.3;
                        (900.0 + pct * 3300.0, 60.0 + pct * 35.0, 100.0, 0.0, "Throttle Stab - Approaching 2-Step Limiter")
                    } else if sec < 5.0 {
                        let osc = (2.0 * std::f64::consts::PI * (sec - 1.3) / 0.4).sin();
                        (4200.0 + osc * 250.0, 95.0, 100.0, 0.0, "2-Step Rev Limiter Bounce (Stationary, Brake/Clutch Held)")
                    } else if sec < 5.2 {
                        let pct = (sec - 5.0) / 0.2;
                        (4200.0, 96.0, 100.0, pct * 15.0, "Launch - Clutch Drop / Wheel Speed Rising")
                    } else if sec < 8.0 {
                        let pct = (sec - 5.2) / 2.8;
                        (4200.0 + pct * 4000.0, 99.0, 100.0, 15.0 + pct * 75.0, "1st Gear Full Acceleration (2-Step Released)")
                    } else if sec < 8.3 {
                        (8200.0 - (sec - 8.0) * 3000.0, 20.0, 0.0, 90.0, "Off-Throttle Abort (Speed Nonzero - 2-Step Must Stay Disengaged)")
                    } else {
                        let pct = (sec - 8.3) / 1.2;
                        (7300.0 - pct * 5900.0, 25.0, 0.0, (90.0 - pct * 90.0).max(0.0), "Coast Down After Aborted Launch")
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

    #[test]
    fn finds_p28_runtime_entry_after_the_hardware_checks() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../P28-230.bin");
        let Ok(rom) = std::fs::read(path) else {
            return;
        };

        assert_eq!(ReplayEngine::runtime_ready_pc(&rom), Some(0x3946));
        assert!(ReplayEngine::is_stock_p28_230(&rom[..32_768]));
    }

    #[test]
    fn stock_p28_boots_with_live_sensor_inputs_through_the_mux() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../P28-230.bin");
        let mut bus = Bus::new();
        if bus.load_rom_file(path).is_err() {
            eprintln!("stock P28 ROM not present at {path}; skipping");
            return;
        }

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
        let boot_check_high =
            (bus.p28_mux_inputs[P28_MUX_U5_ADCR1][P28_U5_BOOT_CHECK_SELECT] >> 2) as u8;
        assert_eq!(bus.ram[0x03C6 + P28_U5_BOOT_CHECK_SELECT], boot_check_high);
        assert_ne!(bus.ram[0x00EB], 0x49, "boot fault 0x49 must not latch");
    }

    /// Boot the stock ROM, let it settle for half a second at `rpm`, then run
    /// `engine_cycles` full engine cycles. Returns the injector outputs in
    /// the order they opened, the last pulse width per output, and the bus.
    fn run_stock_p28(rpm: f64, engine_cycles: u32) -> Option<(Vec<usize>, [u32; 4], Bus)> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../P28-230.bin");
        let mut bus = Bus::new();
        if bus.load_rom_file(path).is_err() {
            eprintln!("stock P28 ROM not present at {path}; skipping");
            return None;
        }
        let hz = crate::bus::CPU_HZ;
        let mut engine = EngineState::new();
        engine.rpm = rpm;
        engine.map_kpa = 90.0;
        engine.tps_pct = 65.0;
        engine.ect_celsius = 88.0;
        engine.sync_sensors_to_bus(&mut bus);

        let mut cpu = Cpu::new();
        cpu.pc = bus.read_code_u16(0);
        ReplayEngine::boot_to_runtime(&mut cpu, &mut bus).expect("stock P28 boots");

        let settle = cpu.cycles + hz / 2;
        let end = settle + (engine_cycles as f64 * hz as f64 * 120.0 / rpm) as u64;
        let mut order = Vec::new();
        let mut counted = bus.injector_open_count;
        while cpu.cycles < end {
            let before = cpu.cycles;
            let instruction = Interpreter::step(&mut cpu, &mut bus);
            assert!(
                !cpu.halted,
                "halted at {:#06X}: {}",
                instruction.pc,
                instruction.mnemonic
            );
            let irq = bus.tick_timers((cpu.cycles - before) as u32)
                | engine.check_distributor_pulses(&mut bus, cpu.cycles, hz);
            InterruptController::handle_pending_interrupts(&mut cpu, &mut bus, irq);

            if cpu.cycles < settle {
                counted = bus.injector_open_count;
                continue;
            }
            for (output, seen) in counted.iter_mut().enumerate() {
                if bus.injector_open_count[output] != *seen {
                    *seen = bus.injector_open_count[output];
                    order.push(output);
                }
            }
        }
        let widths = bus.injector_last_pulse_us;
        Some((order, widths, bus))
    }

    /// Issue #6: the stock ROM must inject once per cylinder per engine
    /// cycle, in a fixed rotating order, from cranking speed to the top of
    /// the rev range -- not a handful of prime pulses.
    #[test]
    fn stock_p28_sustains_sequential_injection() {
        for rpm in [300.0, 800.0, 3000.0, 6500.0] {
            let Some((order, widths, bus)) = run_stock_p28(rpm, 10) else {
                return;
            };

            // Ten engine cycles give 40 openings; allow one cycle of phase
            // slack at each end of the window.
            assert!(
                (36..=44).contains(&order.len()),
                "{rpm} rpm: {} injector openings in ten engine cycles",
                order.len()
            );
            for pair in order.windows(2) {
                assert_eq!(
                    pair[1],
                    (pair[0] + 1) % 4,
                    "{rpm} rpm: outputs must open in rotation, got {order:?}"
                );
            }
            for (output, width) in widths.iter().enumerate() {
                assert!(
                    (1_000..=20_000).contains(width),
                    "{rpm} rpm: output {output} pulse width {width} us"
                );
            }

            // The ROM must be in its normal crank mode (IE shadow 2BABh: CKP
            // through the timer-2 capture event), not the INT1-only mode
            // (A9A3h) it falls back to when it sees no CKP teeth.
            let ie_shadow = bus.ram[0x00F2] as u16 | ((bus.ram[0x00F3] as u16) << 8);
            assert_eq!(ie_shadow, 0x2BAB, "{rpm} rpm: crank-signal mode");
        }
    }

    #[test]
    fn stock_p28_cuts_fuel_above_its_rev_limit() {
        let Some((order, _, bus)) = run_stock_p28(7600.0, 10) else {
            return;
        };

        assert!(order.is_empty(), "openings above the rev limit: {order:?}");
        assert_eq!(bus.injector_outputs(), 0x0F, "all injectors closed");
        assert_eq!(bus.live_injector_pulse_width_us(7600.0), 0);
    }

    #[test]
    fn finds_launch_rom_runtime_entry_after_the_hardware_checks() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../test-roms/lyonnel-2.0-launch-ok.bin"
        );
        let Ok(rom) = std::fs::read(path) else {
            return;
        };

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
        assert!(frames
            .iter()
            .any(|f| f.timestamp_ms >= 5200 && f.speed_kmh > 0.0 && f.rpm > 4200.0));
        assert!(frames
            .iter()
            .any(|f| f.timestamp_ms >= 8000 && f.speed_kmh > 0.0 && f.tps_pct == 0.0));
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
    fn launch_rom_reaches_runtime_and_drives_an_injector() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../test-roms/lyonnel-2.0-launch-ok.bin"
        );
        let mut bus = Bus::new();
        if bus.load_rom_file(path).is_err() {
            eprintln!("launch ROM not present at {path}; skipping");
            return;
        }
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

            let before = cpu.cycles;
            let instruction = Interpreter::step(&mut cpu, &mut bus);
            assert!(
                !cpu.halted,
                "halted at {:#06X}: {}",
                instruction.pc,
                instruction.mnemonic
            );
            let t_irq = bus.tick_timers((cpu.cycles - before) as u32);
            let irq = t_irq | engine.check_distributor_pulses(&mut bus, cpu.cycles, crate::bus::CPU_HZ);
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
