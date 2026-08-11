// Honda P28 / OBD1 ECU ROM and hardware sanity checks.

use crate::bus::{
    Bus, IRQ_INT0, IRQ_SERIAL_RX, IRQ_TM0, SFR_IE, SFR_P2, SFR_TCON0, SFR_TM0, SFR_TMR0,
};
use crate::cpu::Cpu;
use crate::engine::EngineState;
use crate::interpreter::Interpreter;
use crate::interrupts::InterruptController;
use crate::replay::ReplayEngine;

pub struct TestResult {
    pub name: String,
    pub passed: bool,
    pub details: String,
}

pub struct EcuTestSuite;

impl EcuTestSuite {
    pub fn run_full_suite(rom_path: &str) -> Vec<TestResult> {
        let mut results = Vec::new();

        println!("============================================================");
        println!("  STOCK P28-230 ROM & MSM66207 HARDWARE SANITY SUITE ");
        println!("  Target ROM: {}", rom_path);
        println!("============================================================");

        let start_time = std::time::Instant::now();

        // Each result below performs an observable check. Calibration-table,
        // DTC and compensation claims are intentionally excluded until the
        // ROM's resulting state is decoded and asserted rather than inferred.
        let rom_check = Self::test_rom_checksum(rom_path);
        let rom_is_usable = rom_check.passed;
        results.push(rom_check);
        if !rom_is_usable {
            println!(
                "Stopped after ROM preflight: subsequent checks require an exact, checksum-valid image."
            );
            return results;
        }
        results.push(Self::test_vector_table(rom_path));
        results.push(Self::test_reset_boot(rom_path));
        results.push(Self::test_adc_sensors());
        results.push(Self::test_interrupt_dispatch(rom_path));
        results.push(Self::test_output_driver_measurement());
        results.push(Self::test_uart_rx_irq());

        let elapsed = start_time.elapsed();
        println!("Executed {} tests in {:.2?}", results.len(), elapsed);

        results
    }

    fn test_rom_checksum(rom_path: &str) -> TestResult {
        let mut bus = Bus::new();
        if let Err(e) = bus.load_rom_file(rom_path) {
            return TestResult {
                name: "ROM File Load & Checksum".to_string(),
                passed: false,
                details: format!("Failed to read file: {}", e),
            };
        }

        let sum: u32 = bus.rom.iter().map(|&b| b as u32).sum();
        let mod8 = (sum % 256) as u8;
        let chk_byte = bus.rom[0x7FFF];

        let passed = mod8 == 0;
        let details = format!(
            "ROM Size: {} bytes, 8-bit Modulo Sum: 0x{:02X} (Checksum Byte at 0x7FFF: 0x{:02X})",
            bus.rom.len(),
            mod8,
            chk_byte
        );

        TestResult {
            name: "ROM File Load & Checksum".to_string(),
            passed,
            details,
        }
    }

    fn test_vector_table(rom_path: &str) -> TestResult {
        let mut bus = Bus::new();
        let _ = bus.load_rom_file(rom_path);

        let reset_vec = bus.read_code_u16(0x0000);
        let nmi_vec = bus.read_code_u16(0x0006);
        let int0_vec = bus.read_code_u16(0x0008);
        let serial_rx_vec = bus.read_code_u16(0x000A);
        let tm0_vec = bus.read_code_u16(0x0012);

        let targets = [reset_vec, nmi_vec, int0_vec, serial_rx_vec, tm0_vec];
        // These are the stock P28-230 vector targets and are retained by the
        // launch ROM fixtures used by this project. This prevents an arbitrary
        // checksum-balanced 32 KiB blob from being called a P28 ROM.
        let expected = [0x21E2, 0x003C, 0x01D3, 0x01FD, 0x0086];
        let valid = targets == expected;

        let details = format!(
            "Reset: {:#06X}, NMI: {:#06X}, INT0: {:#06X}, serial RX: {:#06X}, TM0: {:#06X}",
            reset_vec, nmi_vec, int0_vec, serial_rx_vec, tm0_vec
        );

        TestResult {
            name: "P28 Vector Table Audit".to_string(),
            passed: valid,
            details,
        }
    }

    fn test_reset_boot(rom_path: &str) -> TestResult {
        let mut bus = Bus::new();
        let _ = bus.load_rom_file(rom_path);
        let mut cpu = Cpu::new();

        let reset_target = bus.read_code_u16(0x0000);
        cpu.pc = reset_target;

        let (passed, details) = match ReplayEngine::boot_to_runtime(&mut cpu, &mut bus) {
            Ok(()) => (
                true,
                format!(
                    "Reached runtime entry {:#06X} from reset vector {:#06X} after {} instructions; SSP: {:#06X}",
                    cpu.pc, reset_target, cpu.instructions, cpu.ssp
                ),
            ),
            Err(error) => (false, format!("Power-on path failed: {error}")),
        };

        TestResult {
            name: "Power-On Path Reaches Runtime".to_string(),
            passed,
            details,
        }
    }

    fn test_adc_sensors() -> TestResult {
        let mut bus = Bus::new();
        let mut engine = EngineState::new();

        engine.map_kpa = 100.0;
        engine.tps_pct = 50.0;
        engine.sync_sensors_to_bus(&mut bus);
        bus.trigger_adc_conversion();

        let adcr0 = bus.read_data_u16(0x60);
        let adcr1 = bus.read_data_u16(0x62);

        let passed = adcr0 == bus.adc_inputs[0] << 6 && adcr1 == bus.adc_inputs[1] << 6;
        let details = format!(
            "MAP 100kPa -> ADCR0: {}, TPS 50% -> ADCR1: {}",
            adcr0, adcr1
        );

        TestResult {
            name: "ADC Hardware Conversion & Sensor Sweep".to_string(),
            passed,
            details,
        }
    }

    fn test_interrupt_dispatch(rom_path: &str) -> TestResult {
        let mut bus = Bus::new();
        let _ = bus.load_rom_file(rom_path);
        let mut cpu = Cpu::new();
        let mut engine = EngineState::new();

        cpu.pc = bus.read_code_u16(0x0000);
        cpu.set_mie(true);
        bus.write_data_u16(SFR_IE, (1 << IRQ_INT0) | (1 << IRQ_TM0));
        bus.write_data_u16(SFR_TM0, 999);
        bus.write_data_u16(SFR_TMR0, 1000);
        bus.write_data_u8(SFR_TCON0, 0x10);
        engine.rpm = 7500.0;

        // TM0 uses the /4 clock at this control setting, so four CPU clocks
        // advance it onto the compare value. At 7,500 RPM, one CKP edge is
        // emitted every 8,000 CPU clocks.
        let timer_irq = bus.tick_timers(4);
        let dist_irq = engine.check_distributor_pulses(8_000, 12_000_000);
        let dispatched = InterruptController::handle_pending_interrupts(
            &mut cpu,
            &mut bus,
            timer_irq | dist_irq,
        );

        let passed =
            timer_irq & (1 << IRQ_TM0) != 0 && dist_irq & (1 << IRQ_INT0) != 0 && dispatched;
        let details = format!(
            "TM0 IRQ: {}, INT0 CKP IRQ: {}, controller dispatched: {}",
            timer_irq & (1 << IRQ_TM0) != 0,
            dist_irq & (1 << IRQ_INT0) != 0,
            dispatched
        );

        TestResult {
            name: "Timer/CKP Interrupt Dispatch Plumbing".to_string(),
            passed,
            details,
        }
    }

    fn test_output_driver_measurement() -> TestResult {
        let mut bus = Bus::new();

        // The four injector drivers are active-low on P2. Measure a 120-clock
        // pulse at 12 MHz and verify that it is surfaced as 10 microseconds.
        bus.write_data_u8(SFR_P2, 0xFF);
        bus.write_data_u8(SFR_P2, 0xFE);
        bus.tick_timers(120);
        bus.write_data_u8(SFR_P2, 0xFF);

        let passed = bus.injector_pulse_width_us == 10;
        let details = format!(
            "Active-low P2 injector pulse: {} us (expected 10 us)",
            bus.injector_pulse_width_us
        );

        TestResult {
            name: "Injector Output Driver Pulse Measurement".to_string(),
            passed,
            details,
        }
    }

    fn test_uart_rx_irq() -> TestResult {
        let mut bus = Bus::new();

        let irq = bus.push_serial_rx_byte(0x20);

        let passed = (irq & (1 << IRQ_SERIAL_RX)) != 0;
        let details = format!(
            "Serial RX byte 0x20 queued and raised IRQ bit {} (P28 handler {:#06X})",
            IRQ_SERIAL_RX, 0x01FD
        );

        TestResult {
            name: "UART Serial RX Buffer & Interrupt Plumbing".to_string(),
            passed,
            details,
        }
    }
}
