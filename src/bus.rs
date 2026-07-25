// OKI MSM66207 Memory Bus & Peripheral Simulation

use std::fs;
use std::io;

pub const ROM_SIZE: usize = 32768; // 32KB ROM space (0x0000..0x7FFF)
pub const RAM_SIZE: usize = 4096;  // 4KB Data RAM & SFR space (0x0000..0x0FFF)

// SFR Offsets
pub const SFR_ASSP: u16 = 0x00;
pub const SFR_ALRB: u16 = 0x02;
pub const SFR_PSW: u16  = 0x04;
pub const SFR_ACC: u16  = 0x06;
pub const SFR_IRQ: u16  = 0x18;
pub const SFR_IE: u16   = 0x1A;
pub const SFR_EXION: u16= 0x1C;

pub const SFR_P0: u16   = 0x20;
pub const SFR_P1: u16   = 0x22;
pub const SFR_P2: u16   = 0x24;
pub const SFR_P3: u16   = 0x28;
pub const SFR_P4: u16   = 0x2C;
pub const SFR_P5: u16   = 0x2F;

pub const SFR_TM0: u16  = 0x30;
pub const SFR_TMR0: u16 = 0x32;
pub const SFR_TM1: u16  = 0x34;
pub const SFR_TMR1: u16 = 0x36;
pub const SFR_TM2: u16  = 0x38;
pub const SFR_TMR2: u16 = 0x3A;
pub const SFR_TM3: u16  = 0x3C;
pub const SFR_TMR3: u16 = 0x3E;

pub const SFR_TCON0: u16 = 0x40;
pub const SFR_TCON1: u16 = 0x41;
pub const SFR_TCON2: u16 = 0x42;
pub const SFR_TCON3: u16 = 0x43;

pub const SFR_ADSCAN: u16 = 0x58;
pub const SFR_ADSEL: u16  = 0x59;
pub const SFR_ADCR0: u16  = 0x60;

pub const SFR_PWMC0: u16  = 0x70;
pub const SFR_PWMR0: u16  = 0x72;
pub const SFR_PWMC1: u16  = 0x74;
pub const SFR_PWMR1: u16  = 0x76;
pub const SFR_PWCON0: u16 = 0x78;
pub const SFR_PWCON1: u16 = 0x7A;

pub const SFR_SRBUF: u16  = 0x54;
pub const SFR_STBUF: u16  = 0x51;

pub struct Bus {
    pub rom: Vec<u8>,
    pub ram: [u8; RAM_SIZE],
    
    // Virtual ADC Channels (0..7)
    pub adc_inputs: [u16; 8],
    
    // PWM output measurements
    pub injector_pulse_width_us: u32,
    pub iacv_duty_cycle_pct: f32,

    // VTEC Status
    pub vtec_solenoid_active: bool,
    pub vtec_pressure_switch: bool,

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
            vtec_solenoid_active: false,
            vtec_pressure_switch: true, // Normal oil pressure closed
            serial_rx_queue: Vec::new(),
            serial_tx_queue: Vec::new(),
        };
        // Initialize default SFR values
        bus.write_data_u16(SFR_ASSP, 0x07FE);
        bus
    }

    pub fn load_rom_file(&mut self, path: &str) -> io::Result<()> {
        let data = fs::read(path)?;
        if data.len() == 32769 {
            // Trim 1 trailing byte if 32769
            self.rom.copy_from_slice(&data[..ROM_SIZE]);
        } else if data.len() >= ROM_SIZE {
            self.rom.copy_from_slice(&data[..ROM_SIZE]);
        } else {
            self.rom[..data.len()].copy_from_slice(&data);
        }
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
        if addr >= SFR_ADCR0 && addr < SFR_ADCR0 + 16 {
            let channel = ((addr - SFR_ADCR0) / 2) as usize;
            let val = self.adc_inputs[channel];
            if (addr % 2) == 0 {
                return (val & 0xFF) as u8;
            } else {
                return ((val >> 8) & 0xFF) as u8;
            }
        } else if addr == SFR_SRBUF {
            if !self.serial_rx_queue.is_empty() {
                return self.serial_rx_queue.remove(0);
            }
        } else if addr == SFR_P2 {
            // Inject VTEC pressure switch bit (bit 3 of P2)
            let raw_p2 = self.ram[SFR_P2 as usize];
            if self.vtec_pressure_switch {
                return raw_p2 | (1 << 3);
            } else {
                return raw_p2 & !(1 << 3);
            }
        }
        
        self.ram[idx]
    }

    pub fn read_data_u16(&mut self, addr: u16) -> u16 {
        let l = self.read_data_u8(addr) as u16;
        let h = self.read_data_u8(addr.wrapping_add(1)) as u16;
        l | (h << 8)
    }

    // Data Space Write (RAM & SFRs)
    pub fn write_data_u8(&mut self, addr: u16, val: u8) {
        let idx = (addr as usize) & (RAM_SIZE - 1);
        self.ram[idx] = val;

        // SFR side-effects
        if addr == SFR_ADSCAN {
            // Trigger ADC conversion scan
            self.trigger_adc_conversion();
        } else if addr == SFR_PWMR0 || addr == SFR_PWMR0 + 1 {
            let pwm0 = self.read_data_u16(SFR_PWMR0);
            self.injector_pulse_width_us = (pwm0 as u32) * 2; // 0.5us resolution
        } else if addr == SFR_PWMR1 || addr == SFR_PWMR1 + 1 {
            let pwm1 = self.read_data_u16(SFR_PWMR1);
            self.iacv_duty_cycle_pct = ((pwm1 as f32) / 65535.0) * 100.0;
        } else if addr == SFR_P2 || addr == SFR_P3 || addr == 0x005E || addr == 0x004A || addr == 0x0060 {
            // Check VTEC Solenoid engagement bit (Bit 1 of P3, Bit 2 of P2, Bit 3 of 0x005E, Bit 0 of 0x004A, or Bit 2 of 0x0060)
            let p2 = self.ram[SFR_P2 as usize];
            let p3 = self.ram[SFR_P3 as usize];
            let ram_5e = self.ram[0x005E];
            let ram_4a = self.ram[0x004A];
            let ram_60 = self.ram[0x0060];
            self.vtec_solenoid_active = (p2 & (1 << 2)) != 0 || (p3 & (1 << 1)) != 0 || (ram_5e & (1 << 3)) != 0 || (ram_4a & (1 << 0)) != 0 || (ram_60 & (1 << 2)) != 0;
        } else if addr == SFR_STBUF {
            self.serial_tx_queue.push(val);
        }
    }

    pub fn write_data_u16(&mut self, addr: u16, val: u16) {
        self.write_data_u8(addr, (val & 0xFF) as u8);
        self.write_data_u8(addr.wrapping_add(1), ((val >> 8) & 0xFF) as u8);
    }

    pub fn trigger_adc_conversion(&mut self) {
        // Copy virtual ADC inputs to ADCR0..ADCR7 SFRs
        for i in 0..8 {
            let val = self.adc_inputs[i];
            let addr = SFR_ADCR0 + (i as u16 * 2);
            self.ram[addr as usize] = (val & 0xFF) as u8;
            self.ram[(addr + 1) as usize] = ((val >> 8) & 0xFF) as u8;
        }
    }

    pub fn push_serial_rx_byte(&mut self, byte: u8) -> u16 {
        self.serial_rx_queue.push(byte);
        1 << 6 // Trigger Serial RX Interrupt (bit 6 of IRQ)
    }

    // Hardware timer step
    pub fn tick_timers(&mut self, cycles: u32) -> u16 {
        let mut irq_flags: u16 = 0;

        // Timer 0
        let tm0 = self.read_data_u16(SFR_TM0);
        let tmr0 = self.read_data_u16(SFR_TMR0);
        let (new_tm0, overflow0) = tm0.overflowing_add(cycles as u16);
        self.write_data_u16(SFR_TM0, new_tm0);
        if overflow0 || (tm0 < tmr0 && new_tm0 >= tmr0) {
            irq_flags |= 1 << 2; // TM0 ISR
        }

        // Timer 1 (Periodic ADC)
        let tm1 = self.read_data_u16(SFR_TM1);
        let tmr1 = self.read_data_u16(SFR_TMR1);
        let (new_tm1, overflow1) = tm1.overflowing_add(cycles as u16);
        self.write_data_u16(SFR_TM1, new_tm1);
        if overflow1 || (tm1 < tmr1 && new_tm1 >= tmr1) {
            irq_flags |= 1 << 3; // TM1 ISR
            self.trigger_adc_conversion();
        }

        // Timer 2 (Injection scheduler)
        let tm2 = self.read_data_u16(SFR_TM2);
        let tmr2 = self.read_data_u16(SFR_TMR2);
        let (new_tm2, overflow2) = tm2.overflowing_add(cycles as u16);
        self.write_data_u16(SFR_TM2, new_tm2);
        if overflow2 || (tm2 < tmr2 && new_tm2 >= tmr2) {
            irq_flags |= 1 << 4; // TM2 ISR
        }

        // Timer 3 (Aux timer)
        let tm3 = self.read_data_u16(SFR_TM3);
        let tmr3 = self.read_data_u16(SFR_TMR3);
        let (new_tm3, overflow3) = tm3.overflowing_add(cycles as u16);
        self.write_data_u16(SFR_TM3, new_tm3);
        if overflow3 || (tm3 < tmr3 && new_tm3 >= tmr3) {
            irq_flags |= 1 << 7; // TM3 ISR
        }

        irq_flags
    }
}
