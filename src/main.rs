#![allow(dead_code, unused_imports)]

// Honda OBD1 ECU (OKI MSM66207) Emulator & Testing CLI

mod car;
mod check;
mod wiring;
mod cpu;
mod bus;
mod knock;
mod bench;
mod blink;
mod script;
mod verdict;
mod report;
mod why;
mod ppi82c55;
mod engine;
mod interpreter;
mod interrupts;
mod dtc;
mod full_decoder;
mod decoder;
mod operand;
mod exec;
mod interactive;
mod replay;
mod datalog_server;
#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod acceptance;

use std::env;
use std::io::{self, IsTerminal};
use std::process;
use crate::cpu::Cpu;
use crate::bus::Bus;
use crate::engine::EngineState;
use crate::interpreter::Interpreter;
use crate::interrupts::InterruptController;
use crate::interactive::InteractiveShell;
use crate::replay::ReplayEngine;
use crate::datalog_server::DatalogServer;

fn print_usage() {
    println!("hondaecu-cli {} - Honda OBD1 ECU (OKI MSM66207) Emulator & Testing CLI", env!("CARGO_PKG_VERSION"));
    println!("Usage:");
    println!("  hondaecu-cli check <rom.bin> [options]                    Run the ROM on a described car and say if it is worth burning");
    println!("      options: --ecu p28|p30  --board-gearbox manual|automatic  --car <preset|file>");
    println!("               --set key=value  --report <file>  --no-prompt  --no-why   (`test` does the same)");
    println!("      exit status: 0 worth burning, 1 do not burn, 2 bad command line or file, 3 could not tell");
    println!("  hondaecu-cli datalog <rom.bin> [port]                     Start live serial datalogging TCP server (Port 38400)");
    println!("  hondaecu-cli replay <rom.bin> <log.csv|preset> [out.csv]  Replay CSV datalog / engine scenario through ECU");
    println!("  hondaecu-cli interactive <rom.bin>                        Launch interactive simulation REPL console");
    println!("  hondaecu-cli run <rom.bin> [cycles] [rpm]                 Simulate ECU execution for N cycles");
    println!("  hondaecu-cli disasm <rom.bin> [addr] [count]              Disassemble ROM instructions from target address");
    println!("  hondaecu-cli trace <rom.bin> <mode> <rpm> <secs> [args]   Emit runtime trace for Python analysis");
    println!("      modes: ramwatch <a,..> | pcwatch <p,..> | pccount <p,..> | pchist [n] | snap <lo> <hi> | exec <lo> <hi> | injcount");
    println!("  hondaecu-cli --version                                    Print the version");
    println!();
    println!("Presets for 'replay': dyno-pull, overrun-decel, accel-stomp, drag-pass, launch-control-2step, error-map-failure, error-ect-overheat, cold-start");
    println!();
    println!("Examples:");
    println!("  ./hondaecu-cli datalog P28-230.bin 38400");
    println!("  ./hondaecu-cli replay P28-230.bin dyno-pull");
    println!("  ./hondaecu-cli check P28-230.bin");
    println!("  ./hondaecu-cli check P28-230.bin --car stock-p28-automatic --set knock_sensor=no --no-prompt");
    println!("  ./hondaecu-cli interactive P28-230.bin");
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_usage();
        process::exit(1);
    }

    match args[1].as_str() {
        "--version" | "-V" | "version" => {
            println!("hondaecu-cli {}", env!("CARGO_PKG_VERSION"));
        }

        "datalog" | "server" => {
            let rom_path = args.get(2).map(|s| s.as_str()).unwrap_or("P28-230.bin");
            let port: u16 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(38400);

            if let Err(e) = DatalogServer::start_server(rom_path, port) {
                eprintln!("Error starting datalog server: {}", e);
                process::exit(1);
            }
        }

        "replay" | "play" | "log" => {
            let rom_path = args.get(2).map(|s| s.as_str()).unwrap_or("P28-230.bin");
            let log_source = args.get(3).map(|s| s.as_str()).unwrap_or("dyno-pull");
            let out_csv = args.get(4).map(|s| s.as_str());

            if let Err(e) = ReplayEngine::replay(rom_path, log_source, out_csv) {
                eprintln!("Error playing datalog: {}", e);
                process::exit(1);
            }
        }

        "check" | "test" => {
            let stdin = io::stdin();
            let interactive = stdin.is_terminal();
            let status = check::run(
                &args[1..],
                &mut stdin.lock(),
                interactive,
                &mut io::stdout(),
                &mut io::stderr(),
            );
            process::exit(status);
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
            match bus.load_rom_file(rom_path) {
                Ok(Some(note)) => eprintln!("ROM note: {note}"),
                Ok(None) => {}
                Err(e) => {
                    eprintln!("Failed to load ROM: {}", e);
                    process::exit(1);
                }
            }

            let mut cpu = Cpu::new();
            let mut engine = EngineState::new();

            cpu.pc = bus.read_code_u16(0x0000);
            engine.rpm = target_rpm;

            let start = std::time::Instant::now();
            let mut interrupts_handled = 0;

            for _ in 0..cycles {
                engine.sync_sensors_to_bus(&mut bus);
                match bench::step_machine_dispatched(&mut cpu, &mut bus, &mut engine, true) {
                    Ok((_, true)) => interrupts_handled += 1,
                    Ok(_) => {}
                    Err(stop) => {
                        eprintln!("Execution stopped: {stop}");
                        break;
                    }
                }
            }

            let duration = start.elapsed();
            let ips = (cpu.instructions as f64) / duration.as_secs_f64();

            println!("Emulation Completed!");
            println!("  Total Cycles     : {}", cpu.cycles);
            println!("  Instructions     : {}", cpu.instructions);
            println!("  Execution Time   : {:.3?}", duration);
            println!("  Host throughput  : {:.2} instructions/sec", ips);
            println!("  Interrupts Handled: {}", interrupts_handled);
            println!("  Calculated Injector Pulse Width: {} us", bus.injector_pulse_width_us);
            println!("  IACV Idle Valve Duty Cycle     : {:.1} %", bus.iacv_duty_cycle_pct);
        }

        "disasm" => {
            let rom_path = args.get(2).map(|s| s.as_str()).unwrap_or("P28-230.bin");
            let start_addr: u16 = args.get(3).and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok()).unwrap_or(0x0000);
            let count: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(25);

            let mut bus = Bus::new();
            match bus.load_rom_file(rom_path) {
                Ok(Some(note)) => eprintln!("ROM note: {note}"),
                Ok(None) => {}
                Err(e) => {
                    eprintln!("Failed to load ROM: {}", e);
                    process::exit(1);
                }
            }

            let mut cpu = Cpu::new();
            cpu.pc = start_addr;

            println!("Disassembly of '{}' starting at {:#06X}:", rom_path, start_addr);
            for _ in 0..count {
                let ins = Interpreter::disassemble(&mut cpu, &bus);
                let hex = ins.bytes.iter().map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(" ");
                println!("  {:#06X}: {:18} ; {}", ins.pc, ins.mnemonic, hex);
            }
        }

        "trace" => {
            // trace <rom> <mode> <rpm> <secs> [args...]
            // modes: ramwatch <a,..> | pcwatch <p,..> | pccount <p,..> |
            //        pchist [n] | snap <lo> <hi> | exec <lo> <hi>
            let rom_path = args.get(2).map(|s| s.as_str()).unwrap_or("P28-230.bin");
            let mode = args.get(3).map(|s| s.as_str()).unwrap_or("pchist");
            let rpm: f64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(5500.0);
            let secs: f64 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(1.0);
            let rest: Vec<String> = args.iter().skip(6).cloned().collect();
            if let Err(e) = ReplayEngine::trace(rom_path, mode, rpm, secs, &rest) {
                eprintln!("trace error: {}", e);
                process::exit(1);
            }
        }

        _ => {
            print_usage();
            process::exit(1);
        }
    }
}
