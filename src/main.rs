#![allow(dead_code, unused_imports)]

// Honda OBD1 ECU (OKI MSM66207) Emulator & Testing CLI

mod cpu;
mod bus;
mod engine;
mod interpreter;
mod interrupts;
mod dtc;
mod suite;
mod interactive;

use std::env;
use std::process;
use crate::cpu::Cpu;
use crate::bus::Bus;
use crate::engine::EngineState;
use crate::interpreter::Interpreter;
use crate::interrupts::InterruptController;
use crate::suite::EcuTestSuite;
use crate::interactive::InteractiveShell;

fn print_usage() {
    println!("Honda OBD1 ECU (OKI MSM66207) Emulator & Testing CLI");
    println!("Usage:");
    println!("  hondaecu-cli test <rom.bin>                    Run automated ECU ROM test suite");
    println!("  hondaecu-cli interactive <rom.bin>             Launch interactive simulation REPL console");
    println!("  hondaecu-cli run <rom.bin> [cycles] [rpm]      Simulate ECU execution for N cycles");
    println!("  hondaecu-cli disasm <rom.bin> [addr] [count]   Disassemble ROM instructions from target address");
    println!();
    println!("Examples:");
    println!("  hondaecu-cli test P28-230.bin");
    println!("  hondaecu-cli interactive P28-230.bin");
    println!("  hondaecu-cli run P28-230.bin 50000 3000");
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_usage();
        process::exit(1);
    }

    match args[1].as_str() {
        "test" => {
            let rom_path = args.get(2).map(|s| s.as_str()).unwrap_or("P28-230.bin");
            let results = EcuTestSuite::run_full_suite(rom_path);

            println!("\nTest Results Summary:");
            let mut total_passed = 0;
            for res in &results {
                let status = if res.passed {
                    total_passed += 1;
                    "[PASS]"
                } else {
                    "[FAIL]"
                };
                println!("  {:7} {} - {}", status, res.name, res.details);
            }
            println!("\nTotal: {}/{} tests passed ({:.1}%)\n",
                     total_passed, results.len(), (total_passed as f32 / results.len() as f32) * 100.0);

            if total_passed < results.len() {
                process::exit(1);
            }
        }

        "interactive" | "repl" => {
            let rom_path = args.get(2).map(|s| s.as_str()).unwrap_or("P28-230.bin");
            if let Err(e) = InteractiveShell::run(rom_path) {
                eprintln!("Interactive shell error: {}", e);
                process::exit(1);
            }
        }

        "run" | "sim" | "simulate" => {
            let rom_path = args.get(2).map(|s| s.as_str()).unwrap_or("P28-230.bin");
            let cycles: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(100_000);
            let target_rpm: f64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(3000.0);

            println!("Running ECU Emulation on '{}' for {} cycles at {:.0} RPM...", rom_path, cycles, target_rpm);

            let mut bus = Bus::new();
            if let Err(e) = bus.load_rom_file(rom_path) {
                eprintln!("Failed to load ROM: {}", e);
                process::exit(1);
            }

            let mut cpu = Cpu::new();
            let mut engine = EngineState::new();

            cpu.pc = bus.read_code_u16(0x0000);
            engine.rpm = target_rpm;

            let start = std::time::Instant::now();
            let mut interrupts_handled = 0;

            for cycle in 0..cycles {
                engine.sync_sensors_to_bus(&mut bus);
                Interpreter::step(&mut cpu, &mut bus);

                let timer_irq = bus.tick_timers(2);
                let dist_irq = engine.check_distributor_pulses(cycle, 12_000_000);

                if InterruptController::handle_pending_interrupts(&mut cpu, &mut bus, timer_irq | dist_irq) {
                    interrupts_handled += 1;
                }
            }

            let duration = start.elapsed();
            let ips = (cpu.instructions as f64) / duration.as_secs_f64();

            println!("Emulation Completed!");
            println!("  Total Cycles     : {}", cpu.cycles);
            println!("  Instructions     : {}", cpu.instructions);
            println!("  Execution Time   : {:.3?}", duration);
            println!("  Performance      : {:.2} instructions/sec ({:.2} MHz virtual CPU)", ips, ips / 1_000_000.0);
            println!("  Interrupts Handled: {}", interrupts_handled);
            println!("  Calculated Injector Pulse Width: {} us", bus.injector_pulse_width_us);
            println!("  IACV Idle Valve Duty Cycle     : {:.1} %", bus.iacv_duty_cycle_pct);
        }

        "disasm" => {
            let rom_path = args.get(2).map(|s| s.as_str()).unwrap_or("P28-230.bin");
            let start_addr: u16 = args.get(3).and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok()).unwrap_or(0x0000);
            let count: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(25);

            let mut bus = Bus::new();
            if let Err(e) = bus.load_rom_file(rom_path) {
                eprintln!("Failed to load ROM: {}", e);
                process::exit(1);
            }

            let mut cpu = Cpu::new();
            cpu.pc = start_addr;

            println!("Disassembly of '{}' starting at {:#06X}:", rom_path, start_addr);
            for _ in 0..count {
                let ins = Interpreter::step(&mut cpu, &mut bus);
                let hex = ins.bytes.iter().map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(" ");
                println!("  {:#06X}: {:18} ; {}", ins.pc, ins.mnemonic, hex);
            }
        }

        _ => {
            print_usage();
            process::exit(1);
        }
    }
}
