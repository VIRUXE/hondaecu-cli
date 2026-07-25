// Live Serial Datalogging Protocol Server (HTS / Neptune / Crome Compatible)

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::cpu::Cpu;
use crate::bus::Bus;
use crate::engine::EngineState;
use crate::interpreter::Interpreter;
use crate::interrupts::InterruptController;

pub struct DatalogServer;

impl DatalogServer {
    /// Start the live serial datalogging TCP server at 127.0.0.1:38400 (or specified port)
    pub fn start_server(rom_path: &str, port: u16) -> std::io::Result<()> {
        let addr = format!("0.0.0.0:{}", port);
        let listener = TcpListener::bind(&addr)?;
        println!("============================================================");
        println!("  HONDA ECU LIVE SERIAL DATALOGGING SERVER");
        println!("  Target ROM     : {}", rom_path);
        println!("  Listening On   : TCP {}", addr);
        println!("  Protocol       : HTS / Neptune / Crome (38,400 Baud Datalog)");
        println!("============================================================");
        println!("  Ready for connection from Honda Tuning Suite or tuning software...");

        let rom_path_owned = rom_path.to_string();

        for stream in listener.incoming() {
            match stream {
                Ok(mut socket) => {
                    println!("\n[+] Tuning software client connected from {:?}", socket.peer_addr());
                    let rom_path_clone = rom_path_owned.clone();
                    thread::spawn(move || {
                        if let Err(e) = Self::handle_client(&mut socket, &rom_path_clone) {
                            eprintln!("[-] Client connection error: {}", e);
                        }
                        println!("[-] Client disconnected.");
                    });
                }
                Err(e) => {
                    eprintln!("[-] Connection failed: {}", e);
                }
            }
        }

        Ok(())
    }

    fn handle_client(socket: &mut TcpStream, rom_path: &str) -> std::io::Result<()> {
        let mut bus = Bus::new();
        bus.load_rom_file(rom_path)?;

        let mut cpu = Cpu::new();
        cpu.pc = bus.read_code_u16(0x0000);
        cpu.ie = true;
        bus.write_data_u16(0x1A, 0xFFFF);

        let mut engine = EngineState::new();
        engine.rpm = 2500.0;
        engine.map_kpa = 45.0;
        engine.tps_pct = 20.0;
        engine.ect_celsius = 85.0;
        engine.iat_celsius = 25.0;
        engine.o2_volts = 0.45;
        engine.vbatt_volts = 14.2;
        engine.speed_kmh = 40.0;

        socket.set_read_timeout(Some(Duration::from_millis(50)))?;
        socket.set_nodelay(true)?;

        let mut rx_buf = [0u8; 64];
        let start_time = Instant::now();

        loop {
            // Read command request frame from tuning client
            match socket.read(&mut rx_buf) {
                Ok(n) if n > 0 => {
                    let cmd = rx_buf[0];
                    match cmd {
                        // Command 0x20: Main 16-Byte Datalog Packet Request
                        0x20 | 0x30 | 0x40 => {
                            let packet = Self::build_datalog_packet(&mut cpu, &mut bus, &mut engine, start_time.elapsed().as_secs_f64());
                            socket.write_all(&packet)?;
                        }
                        // Command 0x10: ECU ID Query
                        0x10 => {
                            let id_response = [0x10, 0x50, 0x32, 0x38, 0x2D, 0x32, 0x33, 0x30, 0x00]; // "P28-230"
                            socket.write_all(&id_response)?;
                        }
                        // Command 0x00: Ping
                        0x00 => {
                            socket.write_all(&[0x00, 0xFF])?;
                        }
                        _ => {
                            // Default packet echo
                            let packet = Self::build_datalog_packet(&mut cpu, &mut bus, &mut engine, start_time.elapsed().as_secs_f64());
                            socket.write_all(&packet)?;
                        }
                    }
                }
                _ => {
                    // Periodic auto datalog stream (20 Hz = 50ms)
                    thread::sleep(Duration::from_millis(50));
                    let packet = Self::build_datalog_packet(&mut cpu, &mut bus, &mut engine, start_time.elapsed().as_secs_f64());
                    if socket.write_all(&packet).is_err() {
                        break; // Connection closed
                    }
                }
            }
        }

        Ok(())
    }

    /// Build standard HTS/Neptune 16-byte binary datalog packet
    fn build_datalog_packet(cpu: &mut Cpu, bus: &mut Bus, engine: &mut EngineState, elapsed_sec: f64) -> [u8; 16] {
        // Dynamic sweep simulation for continuous live streaming
        let cycle_rpm = 2000.0 + (elapsed_sec * 500.0) % 5500.0;
        engine.rpm = cycle_rpm;
        engine.map_kpa = 30.0 + (cycle_rpm / 7500.0) * 70.0;
        engine.tps_pct = ((cycle_rpm - 2000.0) / 5500.0 * 100.0).clamp(0.0, 100.0);
        engine.sync_sensors_to_bus(bus);

        // Run CPU execution for 50ms frame
        let start_cycle = cpu.cycles;
        while cpu.cycles - start_cycle < 600_000 {
            Interpreter::step(cpu, bus);
            cpu.ie = true;
            let t_irq = bus.tick_timers(2);
            let d_irq = engine.check_distributor_pulses(cpu.cycles, 12_000_000);
            InterruptController::handle_pending_interrupts(cpu, bus, t_irq | d_irq);
            if cpu.halted {
                break;
            }
        }

        let raw_rpm = (engine.rpm * 4.0) as u16;
        let map_byte = (engine.map_kpa / 250.0 * 255.0).clamp(0.0, 255.0) as u8;
        let tps_byte = (engine.tps_pct / 100.0 * 255.0).clamp(0.0, 255.0) as u8;
        let ect_byte = (engine.ect_celsius + 40.0) as u8;
        let iat_byte = (engine.iat_celsius + 40.0) as u8;
        let o2_byte  = (engine.o2_volts / 5.0 * 255.0).clamp(0.0, 255.0) as u8;
        let vbatt_byte = (engine.vbatt_volts / 18.0 * 255.0).clamp(0.0, 255.0) as u8;
        let speed_byte = (engine.speed_kmh).clamp(0.0, 255.0) as u8;

        let inj_pw = bus.injector_pulse_width_us as u16;
        let iacv_duty = (bus.iacv_duty_cycle_pct / 100.0 * 255.0) as u8;

        let mut flags1: u8 = 0;
        if bus.vtec_solenoid_active { flags1 |= 1 << 0; }
        if bus.vtec_pressure_switch { flags1 |= 1 << 1; }
        if bus.mil_check_engine_light { flags1 |= 1 << 2; }

        let mut flags2: u8 = 0;
        if bus.fuel_pump_active { flags2 |= 1 << 0; }
        if bus.ac_clutch_active { flags2 |= 1 << 1; }
        if bus.radiator_fan_active { flags2 |= 1 << 2; }

        let mut pkt = [0u8; 16];
        pkt[0] = (raw_rpm >> 8) as u8;
        pkt[1] = (raw_rpm & 0xFF) as u8;
        pkt[2] = map_byte;
        pkt[3] = tps_byte;
        pkt[4] = ect_byte;
        pkt[5] = iat_byte;
        pkt[6] = o2_byte;
        pkt[7] = vbatt_byte;
        pkt[8] = speed_byte;
        pkt[9] = (inj_pw >> 8) as u8;
        pkt[10] = (inj_pw & 0xFF) as u8;
        pkt[11] = 21; // Ignition advance 21.0°
        pkt[12] = iacv_duty;
        pkt[13] = flags1;
        pkt[14] = flags2;

        // Modulo 256 checksum byte
        let sum: u16 = pkt[..15].iter().map(|&b| b as u16).sum();
        pkt[15] = (sum & 0xFF) as u8;

        pkt
    }
}
