// Interactive REPL Shell for Honda ECU Simulation & Debugging

use std::io::{self, Write};
use crate::cpu::Cpu;
use crate::bus::Bus;
use crate::engine::EngineState;
use crate::interpreter::Interpreter;
use crate::interrupts::InterruptController;

pub struct InteractiveShell;

impl InteractiveShell {
    pub fn run(rom_path: &str) -> io::Result<()> {
        let mut bus = Bus::new();
        if let Err(e) = bus.load_rom_file(rom_path) {
            println!("Error loading ROM file '{}': {}", rom_path, e);
            return Ok(());
        }

        let mut cpu = Cpu::new();
        let mut engine = EngineState::new();

        // Boot from reset vector
        cpu.pc = bus.read_code_u16(0x0000);

        println!("============================================================");
        println!("  HONDA OBD1 ECU INTERACTIVE EMULATOR REPL");
        println!("  Loaded ROM: {}", rom_path);
        println!("  Type 'help' for commands, 'step' to single-step, 'run' to execute.");
        println!("============================================================");

        let stdin = io::stdin();
        let mut line = String::new();

        loop {
            engine.sync_sensors_to_bus(&mut bus);

            print!("ecu [{:#06X}]> ", cpu.pc);
            io::stdout().flush()?;

            line.clear();
            if stdin.read_line(&mut line)? == 0 {
                break;
            }

            let parts: Vec<&str> = line.trim().split_whitespace().collect();
            if parts.is_empty() {
                continue;
            }

            match parts[0] {
                "help" | "?" => {
                    println!("Available Commands:");
                    println!("  step [N]          - Step N instructions (default 1)");
                    println!("  run [N]           - Execute N cycles (default 10000)");
                    println!("  regs              - Print CPU registers and PSW flags");
                    println!("  status            - Print ECU & virtual engine telemetry");
                    println!("  set rpm <val>     - Set simulated engine RPM (e.g. set rpm 3000)");
                    println!("  set map <val>     - Set manifold pressure in kPa (e.g. set map 45)");
                    println!("  set tps <val>     - Set throttle position % (e.g. set tps 25)");
                    println!("  set ect <val>     - Set engine coolant temp °C (e.g. set ect 85)");
                    println!("  set iat <val>     - Set intake air temp °C (e.g. set iat 25)");
                    println!("  dump <addr> [cnt] - Dump data RAM / SFR memory");
                    println!("  disasm [addr] [cnt]- Disassemble code from address");
                    println!("  irq <num>         - Trigger manual interrupt vector");
                    println!("  reset             - Reset CPU to initial boot state");
                    println!("  exit | quit       - Quit emulator");
                }

                "step" | "s" => {
                    let count: usize = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
                    for _ in 0..count {
                        let ins = Interpreter::step(&mut cpu, &mut bus);
                        println!("  {:#06X}: {:18} ; {}", ins.pc, ins.mnemonic, Self::hex_bytes(&ins.bytes));
                        let t_irq = bus.tick_timers(2);
                        let d_irq = engine.check_distributor_pulses(cpu.cycles, 12_000_000);
                        InterruptController::handle_pending_interrupts(&mut cpu, &mut bus, t_irq | d_irq);
                    }
                }

                "run" | "r" => {
                    let count: usize = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(10_000);
                    println!("Running for {} cycles...", count);
                    let start_cycles = cpu.cycles;
                    while cpu.cycles - start_cycles < count as u64 {
                        Interpreter::step(&mut cpu, &mut bus);
                        let t_irq = bus.tick_timers(2);
                        let d_irq = engine.check_distributor_pulses(cpu.cycles, 12_000_000);
                        InterruptController::handle_pending_interrupts(&mut cpu, &mut bus, t_irq | d_irq);
                        if cpu.halted {
                            println!("CPU Halted.");
                            break;
                        }
                    }
                    println!("Executed {} cycles. Current PC: {:#06X}", cpu.cycles - start_cycles, cpu.pc);
                }

                "regs" => {
                    println!("CPU Register File:");
                    println!("  PC : {:#06X}   A  : {:#06X} (AL:{:#04X}, AH:{:#04X})", cpu.pc, cpu.a, cpu.al(), cpu.ah());
                    println!("  DP : {:#06X}   X1 : {:#06X}   X2 : {:#06X}", cpu.dp, cpu.x1, cpu.x2);
                    println!("  SSP: {:#06X}   USP: {:#06X}   LRB: {:#04X}", cpu.ssp, cpu.usp, cpu.lrb);
                    println!("  PSW: {:#06X} [ ZF:{} CF:{} HC:{} DD:{} IE:{} ]",
                             cpu.psw_u16(), cpu.zf as u8, cpu.cf as u8, cpu.hc as u8, cpu.dd as u8, cpu.ie as u8);
                    println!("  Cycles: {}, Instructions: {}", cpu.cycles, cpu.instructions);
                }

                "status" => {
                    println!("ECU & Virtual Engine Telemetry:");
                    println!("  Engine RPM       : {:.1} RPM", engine.rpm);
                    println!("  MAP Pressure     : {:.1} kPa", engine.map_kpa);
                    println!("  TPS Position     : {:.1} %", engine.tps_pct);
                    println!("  ECT Temp         : {:.1} °C", engine.ect_celsius);
                    println!("  IAT Temp         : {:.1} °C", engine.iat_celsius);
                    println!("  O2 Sensor        : {:.2} V", engine.o2_volts);
                    println!("  Battery Voltage  : {:.1} V", engine.vbatt_volts);
                    println!("  Distributor Pulses: CKP: {}, TDC: {}", engine.ckp_pulse_count, engine.tdc_pulse_count);
                    println!("  Calculated Injector Pulse Width : {} us", bus.injector_pulse_width_us);
                    println!("  IACV Duty Cycle                 : {:.1} %", bus.iacv_duty_cycle_pct);
                }

                "set" => {
                    if parts.len() >= 3 {
                        let val: f64 = parts[2].parse().unwrap_or(0.0);
                        match parts[1] {
                            "rpm" => engine.rpm = val,
                            "map" => engine.map_kpa = val,
                            "tps" => engine.tps_pct = val,
                            "ect" => engine.ect_celsius = val,
                            "iat" => engine.iat_celsius = val,
                            _ => println!("Unknown parameter. Use: rpm, map, tps, ect, iat"),
                        }
                        println!("Updated {} to {}", parts[1], val);
                    } else {
                        println!("Usage: set <rpm|map|tps|ect|iat> <value>");
                    }
                }

                "dump" => {
                    if parts.len() >= 2 {
                        let addr = Self::parse_hex_or_dec(parts[1]);
                        let cnt = parts.get(2).map(|s| Self::parse_hex_or_dec(s)).unwrap_or(32);
                        for i in (0..cnt).step_by(16) {
                            let curr = addr + i;
                            print!("{:04X}: ", curr);
                            for j in 0..16 {
                                if i + j < cnt {
                                    print!("{:02X} ", bus.read_data_u8((curr + j) as u16));
                                }
                            }
                            println!();
                        }
                    } else {
                        println!("Usage: dump <addr> [count]");
                    }
                }

                "disasm" => {
                    let addr = parts.get(1).map(|s| Self::parse_hex_or_dec(s)).unwrap_or(cpu.pc as u32) as u16;
                    let cnt = parts.get(2).map(|s| Self::parse_hex_or_dec(s)).unwrap_or(10);
                    let mut temp_cpu = cpu.clone();
                    temp_cpu.pc = addr;
                    for _ in 0..cnt {
                        let ins = Interpreter::step(&mut temp_cpu, &mut bus);
                        println!("  {:#06X}: {:18} ; {}", ins.pc, ins.mnemonic, Self::hex_bytes(&ins.bytes));
                    }
                }

                "reset" => {
                    cpu = Cpu::new();
                    cpu.pc = bus.read_code_u16(0x0000);
                    println!("CPU Reset to vector {:#06X}", cpu.pc);
                }

                "exit" | "quit" | "q" => {
                    println!("Exiting emulator.");
                    break;
                }

                _ => {
                    println!("Unknown command: '{}'. Type 'help' for available commands.", parts[0]);
                }
            }
        }

        Ok(())
    }

    fn hex_bytes(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(" ")
    }

    fn parse_hex_or_dec(s: &str) -> u32 {
        if s.starts_with("0x") || s.starts_with("0X") {
            u32::from_str_radix(&s[2..], 16).unwrap_or(0)
        } else {
            s.parse().unwrap_or(0)
        }
    }
}
