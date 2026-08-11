// OKI MSM66207 Memory Bus & Peripheral Simulation

use std::fs;
use std::io;

pub const ROM_SIZE: usize = 32768; // 32KB ROM space (0x0000..0x7FFF)
// Modeled data space: on-chip SFR/RAM through 0x047F plus external data RAM.
pub const RAM_SIZE: usize = 4096;

// SFR Offsets
pub const SFR_ASSP: u16 = 0x00;
pub const SFR_ALRB: u16 = 0x02;
pub const SFR_PSW: u16 = 0x04;
pub const SFR_ACC: u16 = 0x06;
pub const SFR_IRQ: u16 = 0x18;
pub const SFR_IE: u16 = 0x1A;
pub const SFR_EXICON: u16 = 0x1C;

// IRQ/IE bit numbers correspond one-for-one with the 16 maskable vector-table
// entries at code addresses 0x0008..0x0027 (datasheet "Vector Table Area").
pub const IRQ_INT0: u16 = 0;
pub const IRQ_SERIAL_RX: u16 = 1;
pub const IRQ_TM0: u16 = 5;
pub const IRQ_TM1: u16 = 7;
pub const IRQ_TM2_OVF: u16 = 8;
pub const IRQ_TM2: u16 = 9;
pub const IRQ_TM3: u16 = 11;
pub const IRQ_ADC: u16 = 12;
pub const IRQ_PWM: u16 = 13;
pub const IRQ_INT1: u16 = 15;

// Ports. Each port has a data register, a mode register (PxIO: 1 = output,
// 0 = input; reset 00H) and, for P2..P4, a secondary-function register
// (PxSF: 1 = the pin is driven by the on-chip peripheral). Confirmed against
// the MSM66207 datasheet SFR map and the P28 ROM's own init at 0x2292..0x2304,
// which writes P0IO/P1IO/P2IO = 0FFh for the three driver ports and
// P4IO = 00Dh / P4SF = 0FCh, leaving P4.1 a plain GPIO *input*.
pub const SFR_P0: u16 = 0x20;
pub const SFR_P0IO: u16 = 0x21;
pub const SFR_P1: u16 = 0x22;
pub const SFR_P1IO: u16 = 0x23;
pub const SFR_P2: u16 = 0x24;
pub const SFR_P2IO: u16 = 0x25;
pub const SFR_P2SF: u16 = 0x26;
pub const SFR_P3: u16 = 0x28;
pub const SFR_P3IO: u16 = 0x29;
pub const SFR_P3SF: u16 = 0x2A;
pub const SFR_P4: u16 = 0x2C;
pub const SFR_P4IO: u16 = 0x2D;
pub const SFR_P4SF: u16 = 0x2E;
pub const SFR_P5: u16 = 0x2F;

/// (data, mode, secondary-function) for P0..P4. `None` where the port has no
/// secondary-function register.
const PORTS: [(u16, u16, Option<u16>); 5] = [
    (SFR_P0, SFR_P0IO, None),
    (SFR_P1, SFR_P1IO, None),
    (SFR_P2, SFR_P2IO, Some(SFR_P2SF)),
    (SFR_P3, SFR_P3IO, Some(SFR_P3SF)),
    (SFR_P4, SFR_P4IO, Some(SFR_P4SF)),
];

/// Index into `Bus::port_pins` for P4.
pub const PORT4: usize = 4;
/// P4.1 is wired as a plain GPIO input on the P28 board (P4SF = 0FCh leaves
/// bits 0 and 1 as general I/O, P4IO = 00Dh makes bit 1 an input). The ROM
/// reads it at 0x221D to decide whether to run the power-on self-test at all,
/// and the NMI handler at 0x004E polls it before entering STOP mode -- so a
/// high level means "power is going away, shut down". Low = running.
pub const P4_RUN_SENSE: u8 = 1;

pub const SFR_TM0: u16 = 0x30;
pub const SFR_TMR0: u16 = 0x32;
pub const SFR_TM1: u16 = 0x34;
pub const SFR_TMR1: u16 = 0x36;
pub const SFR_TM2: u16 = 0x38;
pub const SFR_TMR2: u16 = 0x3A;
pub const SFR_TM3: u16 = 0x3C;
pub const SFR_TMR3: u16 = 0x3E;

pub const SFR_TCON0: u16 = 0x40;
pub const SFR_TCON1: u16 = 0x41;
pub const SFR_TCON2: u16 = 0x42;
pub const SFR_TCON3: u16 = 0x43;

pub const SFR_ADSCAN: u16 = 0x58;
pub const SFR_ADSEL: u16 = 0x59;
pub const SFR_ADCR0: u16 = 0x60;

pub const SFR_PWMC0: u16 = 0x70;
pub const SFR_PWMR0: u16 = 0x72;
pub const SFR_PWMC1: u16 = 0x74;
pub const SFR_PWMR1: u16 = 0x76;
pub const SFR_PWCON0: u16 = 0x78;
pub const SFR_PWCON1: u16 = 0x7A;

pub const SFR_SRBUF: u16 = 0x55;
pub const SFR_STBUF: u16 = 0x51;

pub struct Bus {
    pub rom: Vec<u8>,
    pub ram: [u8; RAM_SIZE],

    // Virtual ADC Channels (0..7)
    pub adc_inputs: [u16; 8],

    // PWM output measurements
    pub injector_pulse_width_us: u32,
    pub iacv_duty_cycle_pct: f32,
    pub elapsed_cycles: u64,
    injector_started_at: [Option<u64>; 4],

    // Board-level signals retained for telemetry. Their MCU pin mappings have
    // not yet been verified against the P28 board schematic, so the core does
    // not infer them from P2.0..P2.3 (those are injector-driver transitions).
    pub fuel_pump_active: bool,
    pub mil_check_engine_light: bool,
    pub vtec_solenoid_active: bool,
    pub vtec_pressure_switch: bool,
    pub ac_clutch_active: bool, // P2.4: A/C Compressor Clutch Relay Driver
    pub radiator_fan_active: bool, // P2.5: Radiator Cooling Fan Relay Driver
    pub purge_solenoid_active: bool, // P2.6: EVAP Purge Control Solenoid Driver

    /// PWM0/PWM1 output pin state, surfaced on P4.2 / P4.3.
    pub pwm_out: [bool; 2],

    /// Fractional prescaler remainder for TM0..TM3.
    pub timer_accum: [u32; 4],

    /// Remaining CPU clocks in the current A/D conversion. `ADSCAN.4` starts
    /// continuous scan conversions; each completion raises IRQH.4, which the
    /// P28 boot routine polls at 0x52C5.
    pub adc_cycles_remaining: Option<u32>,

    /// Levels driven onto P0..P4 from outside the chip. Only consulted for
    /// bits the ROM has configured as inputs; output bits read back their own
    /// latch, and secondary-function bits read the peripheral.
    pub port_pins: [u8; 5],

    // Serial Datalogging UART Buffers
    pub serial_rx_queue: Vec<u8>,
    pub serial_tx_queue: Vec<u8>,
}

impl Bus {
    pub fn new() -> Self {
        let mut bus = Self {
            rom: vec![0xFF; ROM_SIZE],
            ram: [0; RAM_SIZE],
            adc_inputs: [512; 8], // Default to ~2.5V (mid scale 10-bit)
            injector_pulse_width_us: 0,
            iacv_duty_cycle_pct: 0.0,
            elapsed_cycles: 0,
            injector_started_at: [None; 4],
            fuel_pump_active: false,
            mil_check_engine_light: false,
            vtec_solenoid_active: false,
            vtec_pressure_switch: true, // Normal oil pressure closed
            ac_clutch_active: false,
            radiator_fan_active: false,
            purge_solenoid_active: false,
            pwm_out: [false; 2],
            // All external inputs idle low. In particular P4.1 low means the
            // ECU is powered and should run; high sends it to STOP mode.
            port_pins: [0; 5],
            timer_accum: [0; 4],
            adc_cycles_remaining: None,
            serial_rx_queue: Vec::new(),
            serial_tx_queue: Vec::new(),
        };
        // Initialize default SFR values
        bus.write_data_u16(SFR_ASSP, 0x07FE);
        bus
    }

    pub fn load_rom_file(&mut self, path: &str) -> io::Result<()> {
        let data = fs::read(path)?;
        let image = match data.as_slice() {
            image if image.len() == ROM_SIZE => image,
            // Some P28 dumps carry one zero byte after the addressable 32 KiB
            // image. Accept only that exact, known footer; never truncate an
            // arbitrary larger file or pad a short one.
            image if image.len() == ROM_SIZE + 1 && image[ROM_SIZE] == 0 => &image[..ROM_SIZE],
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "expected a {ROM_SIZE}-byte ROM image (optionally followed by one 00 byte), got {} bytes",
                        data.len()
                    ),
                ));
            }
        };
        self.rom.copy_from_slice(image);
        Ok(())
    }

    // Code Space Read (ROM)
    pub fn read_code_u8(&self, addr: u16) -> u8 {
        let idx = (addr as usize) & (ROM_SIZE - 1);
        self.rom[idx]
    }

    pub fn read_code_u16(&self, addr: u16) -> u16 {
        let l = self.read_code_u8(addr) as u16;
        let h = self.read_code_u8(addr.wrapping_add(1)) as u16;
        l | (h << 8)
    }

    // Data Space Read (RAM & SFRs)
    pub fn read_data_u8(&mut self, addr: u16) -> u8 {
        let idx = (addr as usize) & (RAM_SIZE - 1);

        // Handle special SFR reads
        if addr == SFR_SRBUF {
            if !self.serial_rx_queue.is_empty() {
                return self.serial_rx_queue.remove(0);
            }
        } else if addr == SFR_P2SF {
            // P2SF bits 0..2 are hardwired: those P2 pins have no general-I/O
            // mode. The datasheet gives P2SF a reset value of 07H (every other
            // port control register resets to 00H), and the ROM proves the bits
            // are not writable -- it does `CLRB P2SF` at 0x22A9 and then, in
            // the periodic configuration-integrity check at 0x3856, requires
            // P2SF to read back 007h. Letting the write stick makes that check
            // fail and the ROM BRKs at 0x3896 with fault code 050h.
            return self.ram[SFR_P2SF as usize] | 0x07;
        }
        if let Some(port) = PORTS.iter().position(|(d, _, _)| *d == addr) {
            return self.read_port(port);
        }

        self.ram[idx]
    }

    /// Read a port the way the silicon does, one bit at a time:
    ///   * secondary function enabled -> the on-chip peripheral drives the pin
    ///   * else configured as output  -> the port's own output latch
    ///   * else                       -> the external level on the pin
    ///
    /// Getting this wrong is not subtle. With every bit read back from the
    /// latch, the ROM sees its own `MOVB P4, #0F7h` at 0x22E5 as a high on
    /// P4.1 and takes the NMI/STOP path forever.
    fn read_port(&self, port: usize) -> u8 {
        let (data, mode, sf) = PORTS[port];
        let latch = self.ram[data as usize];
        let dir = self.ram[mode as usize];
        let sf_bits = sf.map_or(0, |a| {
            let raw = self.ram[a as usize];
            if a == SFR_P2SF { raw | 0x07 } else { raw }
        });
        let pins = self.port_pins[port];

        let mut v = 0u8;
        for bit in 0..8 {
            let m = 1u8 << bit;
            let level = if sf_bits & m != 0 {
                self.secondary_fn_level(port, bit).unwrap_or(latch & m != 0)
            } else if dir & m != 0 {
                latch & m != 0
            } else {
                self.input_level(port, bit).unwrap_or(pins & m != 0)
            };
            if level {
                v |= m;
            }
        }
        v
    }

    /// Named external inputs, for pins whose harness signal this model tracks
    /// as a field rather than as a raw bit in `port_pins`. Only consulted when
    /// the ROM has left the pin configured as an input -- the P28 sets
    /// P2IO = 0FFh, so on this ROM P2.3 is a driver output and the VTEC
    /// pressure switch never reads back here.
    fn input_level(&self, port: usize, bit: u8) -> Option<bool> {
        match (port, bit) {
            (2, 3) => Some(self.vtec_pressure_switch),
            _ => None,
        }
    }

    /// Level driven by the on-chip peripheral assigned to a pin, for the pins
    /// this model actually simulates. Port 4's map (MSM66207 datasheet):
    /// P4.0/TM0CK, P4.1/TM1CK, P4.2/PWM0, P4.3/PWM1, P4.4-7/TRNS0-3. Only the
    /// two PWM outputs are driven from inside the chip; the ROM's power-on
    /// check polls P4.2 to confirm PWM0 is running.
    fn secondary_fn_level(&self, port: usize, bit: u8) -> Option<bool> {
        match (port, bit) {
            (PORT4, 2) => Some(self.pwm_out[0]),
            (PORT4, 3) => Some(self.pwm_out[1]),
            _ => None,
        }
    }

    pub fn read_data_u16(&mut self, addr: u16) -> u16 {
        let l = self.read_data_u8(addr) as u16;
        let h = self.read_data_u8(addr.wrapping_add(1)) as u16;
        l | (h << 8)
    }

    // Data Space Write (RAM & SFRs)
    pub fn write_data_u8(&mut self, addr: u16, val: u8) {
        // ADCR0..ADCR7 are read-only. Conversion completion updates their
        // backing latches directly in `trigger_adc_conversion`.
        if (SFR_ADCR0..SFR_ADCR0 + 16).contains(&addr) {
            return;
        }

        let idx = (addr as usize) & (RAM_SIZE - 1);
        let old = self.ram[idx];
        self.ram[idx] = val;

        // SFR side-effects
        if addr == SFR_ADSCAN {
            // ADSCAN.4 is the start/scan bit. Conversion is deliberately not
            // completed in this write: the firmware clears IRQH.4 immediately
            // after starting and then waits for hardware to set it again.
            self.adc_cycles_remaining = if val & 0x10 != 0 {
                // At the emulated 12 MHz clock, the MSM66207 manual specifies
                // 640 clocks per conversion (320 only below 5 MHz).
                Some(640)
            } else {
                None
            };
        } else if addr == SFR_P2 || addr == SFR_P3 {
            // P2.0..P2.3 are the four active-low injector drivers. PWMR0 is
            // an on-chip PWM reload register used by the firmware's scheduler;
            // it is not an injector pulse-width register. Measure the actual
            // output transitions instead.
            if addr == SFR_P2 {
                for bit in 0..4 {
                    let mask = 1u8 << bit;
                    match (old & mask != 0, val & mask != 0) {
                        (true, false) => {
                            self.injector_started_at[bit] = Some(self.elapsed_cycles);
                        }
                        (false, true) => {
                            if let Some(start) = self.injector_started_at[bit].take() {
                                let clocks = self.elapsed_cycles.saturating_sub(start);
                                self.injector_pulse_width_us =
                                    ((clocks + 6) / 12).min(u32::MAX as u64) as u32;
                            }
                        }
                        _ => {}
                    }
                }
            }

            let p2 = self.ram[SFR_P2 as usize];
            self.ac_clutch_active = (p2 & (1 << 4)) != 0;
            self.radiator_fan_active = (p2 & (1 << 5)) != 0;
            self.purge_solenoid_active = (p2 & (1 << 6)) != 0;
        } else if addr == SFR_STBUF {
            self.serial_tx_queue.push(val);
        }
    }

    pub fn write_data_u16(&mut self, addr: u16, val: u16) {
        self.write_data_u8(addr, (val & 0xFF) as u8);
        self.write_data_u8(addr.wrapping_add(1), ((val >> 8) & 0xFF) as u8);
    }

    /// A/D conversion result for a channel, in the layout the ROM expects.
    ///
    /// The converter is 10-bit (datasheet, "10-bit A/D converter : 8 channels")
    /// but the result register is 16 bits, and the ROM treats the *high byte*
    /// alone as a full-scale 8-bit sensor reading: the plausibility check at
    /// 0x530C accepts `ADCR1H` in 078h..088h -- a window centred on mid-scale
    /// 080h -- or >= 0FCh. That only holds if the 10-bit result is
    /// left-justified in the register, i.e. bits 15..6. Storing it
    /// right-justified makes a mid-scale input read back as 002h and the ROM
    /// BRKs at 0x532B on a "sensor out of range" fault.
    pub fn adc_result(&self, channel: usize) -> u16 {
        (self.adc_inputs[channel] & 0x03FF) << 6
    }

    pub fn trigger_adc_conversion(&mut self) {
        // Latch every channel into ADCR0..ADCR7.
        for i in 0..8 {
            let val = self.adc_result(i);
            let addr = SFR_ADCR0 + (i as u16 * 2);
            self.ram[addr as usize] = (val & 0xFF) as u8;
            self.ram[(addr + 1) as usize] = (val >> 8) as u8;
        }
    }

    pub fn push_serial_rx_byte(&mut self, byte: u8) -> u16 {
        self.serial_rx_queue.push(byte);
        1 << IRQ_SERIAL_RX
    }

    // Hardware timer step
    pub fn tick_timers(&mut self, cycles: u32) -> u16 {
        self.elapsed_cycles = self.elapsed_cycles.wrapping_add(cycles as u64);

        // (counter SFR, reload SFR, control SFR, IRQ bit)
        const TIMERS: [(u16, u16, u16, u16); 4] = [
            (SFR_TM0, SFR_TMR0, SFR_TCON0, IRQ_TM0),
            (SFR_TM1, SFR_TMR1, SFR_TCON1, IRQ_TM1),
            (SFR_TM2, SFR_TMR2, SFR_TCON2, IRQ_TM2),
            (SFR_TM3, SFR_TMR3, SFR_TCON3, IRQ_TM3),
        ];

        let mut irq_flags: u16 = 0;

        // 16-bit PWM: the counter counts up, and on overflow reloads from the
        // PWM register and flips the output pin. PWMR is therefore a reload
        // value, not a duty compare -- which is why the ROM writes PWMC0 and
        // PWMR0 the same (0xFF00), giving a 256-count half period. That rate
        // matches the budgets its own power-on check allows when it watches
        // P4.2/P4.3 go low->high->low (0x177/0x177/0xB2 poll iterations).
        for (i, (c, r)) in [(SFR_PWMC0, SFR_PWMR0), (SFR_PWMC1, SFR_PWMR1)]
            .iter()
            .enumerate()
        {
            let (v, overflow) = self.read_data_u16(*c).overflowing_add(cycles as u16);
            let v = if overflow {
                self.pwm_out[i] = !self.pwm_out[i];
                irq_flags |= 1 << IRQ_PWM;
                self.read_data_u16(*r).wrapping_add(v)
            } else {
                v
            };
            self.ram[*c as usize] = (v & 0xFF) as u8;
            self.ram[(*c + 1) as usize] = (v >> 8) as u8;
        }

        if let Some(remaining) = self.adc_cycles_remaining {
            if cycles >= remaining {
                self.trigger_adc_conversion();
                irq_flags |= 1 << IRQ_ADC;
                // Scan mode continues producing one completion per conversion
                // until firmware clears ADSCAN.4.
                self.adc_cycles_remaining = if self.ram[SFR_ADSCAN as usize] & 0x10 != 0 {
                    Some(640)
                } else {
                    None
                };
            } else {
                self.adc_cycles_remaining = Some(remaining - cycles);
            }
        }

        for (i, (tm_sfr, tmr_sfr, tcon_sfr, irq_bit)) in TIMERS.iter().enumerate() {
            let tcon = self.ram[*tcon_sfr as usize];
            // TCON bit 4 is the run bit; a stopped timer neither counts nor
            // toggles its pin.
            if tcon & (1 << 4) == 0 {
                continue;
            }

            // Prescaler. The datasheet summary does not break TCON down to
            // bits, but the ROM's periodic timer-integrity check at
            // 0x38D8..0x393F states the relationship outright: it samples
            // TM0..TM3, samples again a few instructions later, and requires
            // d1 ~= 4 * d2 and d0 ~= d2 (each within 2 counts), with every
            // delta nonzero. The only TCON bits that differ between timer 1
            // (053h) and timers 0/2/3 (093h/092h/093h) are 6..5 -- 01b for
            // timer 1, 00b for the rest -- so those two bits are the clock
            // select, and the field has to make timer 1 run exactly 4x faster.
            // Only encodings 00b and 01b are attested by this ROM.
            let div = match (tcon >> 5) & 0x03 {
                0b00 => 4,
                0b01 => 1,
                other => 4 >> other.min(2), // unattested; keep monotonic
            };
            self.timer_accum[i] += cycles;
            let steps = self.timer_accum[i] / div;
            self.timer_accum[i] %= div;
            if steps == 0 {
                continue;
            }

            let tm = self.read_data_u16(*tm_sfr);
            let tmr = self.read_data_u16(*tmr_sfr);
            let (new_tm, overflow) = tm.overflowing_add(steps as u16);
            // TMR is a compare register, not a reload value. TM remains a
            // free-running 16-bit counter after a match. Subtracting TMR here
            // made identically clocked TM2/TM3 drift apart, which the ROM's
            // timer-integrity check at 0x38D8 detects.
            let matched = if overflow {
                tm < tmr || new_tm >= tmr
            } else {
                tm < tmr && new_tm >= tmr
            };
            self.write_data_u16(*tm_sfr, new_tm);

            if matched {
                irq_flags |= 1 << *irq_bit;
                // Compare match flips the timer output pin, which is what the
                // ROM watches on Port 4.
                let _ = i;
                if *tm_sfr == SFR_TM1 {
                    self.trigger_adc_conversion();
                }
            }
            if overflow && *tm_sfr == SFR_TM2 {
                irq_flags |= 1 << IRQ_TM2_OVF;
            }
        }

        if irq_flags != 0 {
            let irq = self.ram[SFR_IRQ as usize] as u16
                | ((self.ram[(SFR_IRQ + 1) as usize] as u16) << 8)
                | irq_flags;
            self.ram[SFR_IRQ as usize] = irq as u8;
            self.ram[(SFR_IRQ + 1) as usize] = (irq >> 8) as u8;
        }

        irq_flags
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adc_start_completes_later_and_sets_irqh_bit_four() {
        let mut bus = Bus::new();
        bus.adc_inputs[0] = 0x0155;
        bus.write_data_u16(SFR_IRQ, 0);
        bus.write_data_u8(SFR_ADSCAN, 0x10);

        assert_eq!(bus.read_data_u16(SFR_IRQ), 0);
        assert_eq!(bus.read_data_u16(SFR_ADCR0), 0);
        assert_eq!(bus.tick_timers(638) & (1 << IRQ_ADC), 0);
        assert_eq!(bus.read_data_u16(SFR_ADCR0), 0);
        assert_ne!(bus.tick_timers(2) & (1 << IRQ_ADC), 0);
        assert_ne!(bus.read_data_u16(SFR_IRQ) & (1 << IRQ_ADC), 0);
        assert_eq!(bus.read_data_u16(SFR_ADCR0), 0x0155 << 6);
    }

    #[test]
    fn adc_result_registers_ignore_software_writes() {
        let mut bus = Bus::new();

        bus.write_data_u16(SFR_ADCR0, 0xFFFF);

        assert_eq!(bus.read_data_u16(SFR_ADCR0), 0);
    }

    #[test]
    fn injector_width_is_measured_from_active_low_p2_transitions() {
        let mut bus = Bus::new();
        bus.write_data_u8(SFR_P2, 0xFF);
        bus.write_data_u8(SFR_P2, 0xFE);
        bus.tick_timers(120);
        bus.write_data_u8(SFR_P2, 0xFF);

        assert_eq!(bus.injector_pulse_width_us, 10);
    }

    #[test]
    fn serial_receive_byte_is_consumed_from_srbuf_not_srcon() {
        let mut bus = Bus::new();
        bus.write_data_u8(0x54, 0xA5); // SRCON remains an ordinary control register.
        bus.push_serial_rx_byte(0x20);

        assert_eq!(bus.read_data_u8(0x54), 0xA5);
        assert_eq!(bus.read_data_u8(SFR_SRBUF), 0x20);
        assert!(bus.serial_rx_queue.is_empty());
    }
}
