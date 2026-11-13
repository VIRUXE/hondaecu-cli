// OKI MSM66207 Memory Bus & Peripheral Simulation

use std::fs;
use std::io;

pub const ROM_SIZE: usize = 32768; // 32KB ROM space (0x0000..0x7FFF)
pub const RAM_SIZE: usize = 4096;  // 4KB Data RAM & SFR space (0x0000..0x0FFF)

/// Machine-cycle rate of the OKI core, in Hz. `cpu.cycles` counts machine
/// cycles (from `decoder::int_cycles`, which are the manual's INT cycle counts),
/// so this is the rate at which `cpu.cycles` advances in real time and the one
/// clock every other time base must share: the distributor/crank generator, the
/// replay frame budget, and the injector pulse-width conversion.
///
/// The datasheet gives "minimum instruction cycle : 400ns @ 10MHz", and the
/// fastest instruction (NOP) is two machine cycles (Table 3-37), so one machine
/// cycle is 200ns and the machine-cycle rate is 5MHz = CLK / 2. The emulator
/// previously timed the crank at 12MHz while counting machine cycles, which made
/// the ROM read RPM 2.4x low and never enter normal running injection.
pub const CPU_HZ: u64 = 5_000_000;
/// Machine cycles per microsecond (`CPU_HZ / 1_000_000`).
pub const CYCLES_PER_US: u64 = CPU_HZ / 1_000_000;
/// A/D conversion time at the modeled machine-cycle rate. The MSM66207 manual
/// specifies 64 microseconds (640 oscillator clocks at the P28 clock); one
/// modeled machine cycle is two oscillator clocks, so this is 320 cycles.
pub const ADC_CONVERSION_CYCLES: u32 = (64 * CYCLES_PER_US) as u32;

// SFR Offsets
pub const SFR_ASSP: u16 = 0x00;
pub const SFR_ALRB: u16 = 0x02;
pub const SFR_PSW: u16  = 0x04;
pub const SFR_ACC: u16  = 0x06;
pub const SFR_IRQ: u16  = 0x18;
pub const SFR_IE: u16   = 0x1A;
pub const SFR_EXION: u16= 0x1C;

// IRQ/IE bit numbers correspond one-for-one with the 16 maskable vector-table
// entries at code addresses 0x0008..0x0027 (datasheet "Vector Table Area").
pub const IRQ_INT0: u16 = 0;
pub const IRQ_SERIAL_RX: u16 = 1;
// Per the MSM66207 user's manual Table 17-1, IRQ bit 3 is the serial
// transmission-ready / baud-rate-generator overflow interrupt. The P28 ROM
// uses it as a *software* interrupt: the timer-2 capture handler (CKP tooth,
// 0x014D) ends with `ORB off(IRQ), #008h` at 0x7674, which requests bit 3 so
// the heavier per-tooth crank-angle work at 0x03DC runs at lower priority.
// No peripheral model is needed for it; the ROM's own write sets the flag.
pub const IRQ_STGEN: u16 = 3;
// Table 17-1 IRQ bit map. The four 16-bit timers each raise TWO interrupts: an
// overflow (even bit) and an "event" (odd bit: a capture-register edge or a
// realtime-output compare match, depending on the timer's mode).
pub const IRQ_TM0_OVF: u16 = 4;
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
pub const SFR_P0: u16   = 0x20;
pub const SFR_P0IO: u16 = 0x21;
pub const SFR_P1: u16   = 0x22;
pub const SFR_P1IO: u16 = 0x23;
pub const SFR_P2: u16   = 0x24;
pub const SFR_P2IO: u16 = 0x25;
pub const SFR_P2SF: u16 = 0x26;
pub const SFR_P3: u16   = 0x28;
pub const SFR_P3IO: u16 = 0x29;
pub const SFR_P3SF: u16 = 0x2A;
pub const SFR_P4: u16   = 0x2C;
pub const SFR_P4IO: u16 = 0x2D;
pub const SFR_P4SF: u16 = 0x2E;
pub const SFR_P5: u16   = 0x2F;

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

pub const SFR_TM0: u16  = 0x30;
pub const SFR_TMR0: u16 = 0x32;
pub const SFR_TM1: u16  = 0x34;
pub const SFR_TMR1: u16 = 0x36;
pub const SFR_TM2: u16  = 0x38;
pub const SFR_TMR2: u16 = 0x3A;
pub const SFR_TM3: u16  = 0x3C;
pub const SFR_TMR3: u16 = 0x3E;

/// TCON bits 1..0 (MSM66207 user's manual Fig. 9-2).
pub const TIMER_MODE_AUTO_RELOAD: u8 = 0b00;
pub const TIMER_MODE_CLOCK_OUTPUT: u8 = 0b01;
pub const TIMER_MODE_CAPTURE: u8 = 0b10;
pub const TIMER_MODE_REALTIME_OUTPUT: u8 = 0b11;
/// TCON bit 2: TROUT in realtime output mode, TCERR in capture mode.
pub const TCON_TROUT: u8 = 1 << 2;
/// TCON bit 4: the counter runs.
pub const TCON_RUN: u8 = 1 << 4;

/// Timer whose TMnIO pin (P3.6/TM2IO) receives the distributor's 24-tooth
/// crank-angle (CKP) signal on the P28. The ROM sets P3IO = B1h (P3.6 input),
/// P3SF = FFh and TCON2 = 82h (capture register mode), and its handler for the
/// timer-2 event (0x014D) feeds the 0..23 tooth counter at RAM 12Ah.
pub const P28_CKP_CAPTURE_TIMER: usize = 2;

pub const SFR_TCON0: u16 = 0x40;
pub const SFR_TCON1: u16 = 0x41;
pub const SFR_TCON2: u16 = 0x42;
pub const SFR_TCON3: u16 = 0x43;

/// Transition-detector status register. The four transition detectors latch a
/// falling edge on their P4.4..P4.7 (TRNS0..3) inputs into a status bit here;
/// the ROM polls and read-clears it (there is no transition-detector interrupt
/// -- see Table 17-1). The ROM treats bit 0 as the once-per-camshaft-revolution
/// cylinder-#1 (CYP) marker: after each CKP tooth it does `RB TRNSIT.0` at
/// 0x7678 and re-zeroes its 0..23 tooth counter (RAM 12Ah); the backup TDC
/// handler does the same at 0x03B8. Without it the ROM never identifies
/// cylinder 1 and injection never sequences.
pub const SFR_TRNS: u16 = 0x46;

pub const SFR_ADSCAN: u16 = 0x58;
pub const SFR_ADSEL: u16  = 0x59;
pub const SFR_ADCR0: u16  = 0x60;

/// P28 board-level CD4051 banks. U6 feeds P5.0/AI0 (ADCR0), while U5 feeds
/// P5.1/AI1 (ADCR1). Both parts share A/B/C selects on P2.5/P2.6/P2.7.
pub const P28_MUX_U6_ADCR0: usize = 0;
pub const P28_MUX_U5_ADCR1: usize = 1;

/// Sensor positions confirmed from Bomarc 37820-P28-A51, sheet 3.
pub const P28_U5_ELD_SELECT: usize = 0;
pub const P28_U5_ECT_SELECT: usize = 2;
/// U5/X3 is sampled by the ROM's fault-0x49 boot check. Its external signal
/// semantics remain unresolved.
pub const P28_U5_BOOT_CHECK_SELECT: usize = 3;
pub const P28_U6_HEGO_SELECT: usize = 0;
pub const P28_U6_BARO_SELECT: usize = 3;

/// Direct (unmultiplexed) P5 analog inputs on the P28 board.
pub const P28_ADC_EGR: usize = 3;
pub const P28_ADC_MAP: usize = 6;
pub const P28_ADC_TPS: usize = 7;

pub const SFR_PWMC0: u16  = 0x70;
pub const SFR_PWMR0: u16  = 0x72;
pub const SFR_PWMC1: u16  = 0x74;
pub const SFR_PWMR1: u16  = 0x76;
pub const SFR_PWCON0: u16 = 0x78;
pub const SFR_PWCON1: u16 = 0x7A;

// PWCON0/PWCON1 layout, MSM66207 user's manual Fig. 10-2 (p. 99).
/// Bit 7: this PWM's counter overflow has requested the shared interrupt.
pub const PWCON_IRQ: u8 = 1 << 7;
/// Bit 6: counter overflows request IRQ bit 13.
pub const PWCON_ERQ: u8 = 1 << 6;
/// Bit 5: the output pin is low while the output F/F is set.
pub const PWCON_INV: u8 = 1 << 5;
/// Bit 4: the counter runs.
pub const PWCON_RUN: u8 = 1 << 4;
/// Bit 3: PWM mode (bit 2 is then a don't-care).
pub const PWCON_PWM_MODE: u8 = 1 << 3;
/// Bit 2, with bit 3 clear: free-run counter mode instead of auto reload.
pub const PWCON_FREE_RUN: u8 = 1 << 2;

/// (counter, register, control) SFRs of PWM0 and PWM1.
const PWMS: [(u16, u16, u16); 2] = [
    (SFR_PWMC0, SFR_PWMR0, SFR_PWCON0),
    (SFR_PWMC1, SFR_PWMR1, SFR_PWCON1),
];

/// Bits of PWMC/PWMR in use for PWCON bits 1..0 (Table 10-2). The matching
/// time-base tap -- TBC7, 9, 11 or 15 -- has a period of exactly `2^bits`
/// machine cycles (Table 10-4: 51.2 us, 204.8 us, 819.2 us, 13.1072 ms at
/// 10 MHz), which is what lets PWMR express duty as a fraction of the period.
fn pwm_bits(pwcon: u8) -> u32 {
    [8, 10, 12, 16][(pwcon & 0x03) as usize]
}

pub const SFR_SRBUF: u16  = 0x55;
pub const SFR_STBUF: u16  = 0x51;

pub struct Bus {
    pub rom: Vec<u8>,
    pub ram: [u8; RAM_SIZE],
    
    // Direct MCU ADC-pin levels (P5.0..P5.7). P28 channels 0 and 1 are driven
    // by the external muxes below and do not sample these two array entries.
    pub adc_inputs: [u16; 8],

    /// External P28 analog mux inputs, indexed [U6/U5][select]. The first bank
    /// drives ADCR0 and the second drives ADCR1. Values are 10-bit ADC counts.
    pub p28_mux_inputs: [[u16; 8]; 2],
    
    // PWM output measurements
    /// Width of the most recently completed injector pulse, any cylinder.
    pub injector_pulse_width_us: u32,
    /// Width of the most recently completed pulse on each injector output.
    pub injector_last_pulse_us: [u32; 4],
    /// Number of times each injector output has opened.
    pub injector_open_count: [u64; 4],
    /// `elapsed_cycles` at the most recent injector opening, if any.
    pub injector_last_open_cycle: Option<u64>,
    pub iacv_duty_cycle_pct: f32,
    pub elapsed_cycles: u64,
    injector_started_at: [Option<u64>; 4],
    /// State of the four injector drivers, bit n = output n, active low
    /// (0 = injector open). See `latch_injector_outputs`.
    injector_outputs: u8,

    // Board-output telemetry. None of these is pin-sensed from a verified
    // P28 net: P2.0..P2.3 are the injector latch data and P2.5..P2.7 the
    // analog mux selects, so the fields below are scenario-side state only.
    pub fuel_pump_active: bool,
    pub mil_check_engine_light: bool,
    pub vtec_solenoid_active: bool,    // set from engine conditions, see EngineState::vtec_engaged
    pub vtec_pressure_switch: bool,
    pub ac_clutch_active: bool,
    pub radiator_fan_active: bool,
    pub purge_solenoid_active: bool,

    /// PWM0/PWM1 output F/F (Fig. 10-1): set at the head of each PWM period,
    /// reset by the counter overflow. See `pwm_pin_level` for the pin.
    pwm_active: [bool; 2],

    /// Fractional prescaler remainder for TM0..TM3.
    pub timer_accum: [u32; 4],

    /// Capture-register mode only: TM has run a full lap past TMR since the
    /// last capture event. Reported as TCERR (TCON bit 2) at the next event.
    capture_lapped: [bool; 4],

    /// Remaining machine cycles in the current A/D conversion. `ADSCAN.4` starts
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
            // Board reference dividers and unspecified external inputs start
            // at mid-scale. EngineState replaces the modeled sensor slots.
            p28_mux_inputs: [[512; 8]; 2],
            injector_pulse_width_us: 0,
            injector_last_pulse_us: [0; 4],
            injector_open_count: [0; 4],
            injector_last_open_cycle: None,
            injector_outputs: 0x0F,
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
            pwm_active: [false; 2],
            // All external inputs idle low. In particular P4.1 low means the
            // ECU is powered and should run; high sends it to STOP mode.
            port_pins: [0; 5],
            timer_accum: [0; 4],
            capture_lapped: [false; 4],
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

        // Handle special SFR reads. ADCR0..ADCR7 come from the backing latches
        // below; live input changes are not visible until conversion completes.
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
        if let Some((_, _, con)) = PWMS.iter().find(|(_, r, _)| *r + 1 == addr) {
            // Table 10-2: PWMR bits above the width selected by PWCON bits
            // 1..0 read as ones. This is why the ROM can write FF00h for a
            // 12-bit F00h.
            let unused = !(((1u32 << pwm_bits(self.ram[*con as usize])) - 1) as u16);
            return self.ram[idx] | (unused >> 8) as u8;
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
    /// check polls P4.2 and P4.3 to confirm both PWMs are running.
    fn secondary_fn_level(&self, port: usize, bit: u8) -> Option<bool> {
        match (port, bit) {
            (PORT4, 2) => Some(self.pwm_pin_level(0)),
            (PORT4, 3) => Some(self.pwm_pin_level(1)),
            _ => None,
        }
    }

    /// Level on the PWM0 (P4.2) or PWM1 (P4.3) pin: the output F/F, inverted
    /// when PWCON bit 5 selects a low-active output.
    pub fn pwm_pin_level(&self, pwm: usize) -> bool {
        let low_active = self.ram[PWMS[pwm].2 as usize] & PWCON_INV != 0;
        self.pwm_active[pwm] != low_active
    }

    /// Advance one PWM by `cycles` machine cycles starting at chip time
    /// `start`; returns whether it requested the shared PWM interrupt.
    ///
    /// MSM66207 user's manual chapter 10. The time base counter is taken to be
    /// zero at chip time zero, so a tap of period P falls at every multiple of
    /// P; the manual says PWM start and the taps are not synchronized (Fig.
    /// 10-9), and that is how the ROM's own start time lands here.
    ///
    /// PWM mode (pp. 101-105): PWMC counts machine cycles in the selected
    /// width. The tap's falling edge loads PWMR into PWMC and sets the output
    /// F/F; the PWMC overflow resets it. The output is therefore active for
    /// `2^bits - PWMR` cycles of every `2^bits`. An overflow that coincides
    /// with the tap edge wins: the F/F stays reset, which is what makes
    /// PWMR = 0 a 0 % duty after one fully active period (Fig. 10-8).
    ///
    /// Auto reload timer mode (pp. 105-108): PWMC counts tap periods in the
    /// selected width and reloads from PWMR on overflow. Free-run counter
    /// mode (p. 108): the same clock, a fixed 16 bits, no reload. Neither
    /// touches the output F/F.
    fn tick_pwm(&mut self, pwm: usize, start: u64, cycles: u32) -> bool {
        let (c_sfr, r_sfr, con_sfr) = PWMS[pwm];
        let con = self.ram[con_sfr as usize];
        if con & PWCON_RUN == 0 {
            return false;
        }
        let read = |ram: &[u8; RAM_SIZE], sfr: u16| {
            ram[sfr as usize] as u16 | (ram[sfr as usize + 1] as u16) << 8
        };

        let period = 1u64 << pwm_bits(con);
        let end = start + cycles as u64;
        let pwm_mode = con & PWCON_PWM_MODE != 0;
        let free_run = !pwm_mode && con & PWCON_FREE_RUN != 0;
        let mask = if free_run { 0xFFFF } else { (period - 1) as u16 };
        let stored = read(&self.ram, c_sfr);
        let reload = (read(&self.ram, r_sfr) & mask) as u64;
        let mut count = (stored & mask) as u64;
        let mut overflowed = false;

        if pwm_mode {
            let mut t = start;
            loop {
                let overflow_at = t + (period - count);
                let tap_at = (t / period + 1) * period;
                let next = overflow_at.min(tap_at);
                if next > end {
                    count += end - t;
                    break;
                }
                if overflow_at == next {
                    overflowed = true;
                    self.pwm_active[pwm] = false;
                    count = 0;
                }
                if tap_at == next {
                    count = reload;
                    if overflow_at != next {
                        self.pwm_active[pwm] = true;
                    }
                }
                t = next;
            }
        } else {
            for _ in start / period..end / period {
                count += 1;
                if count > mask as u64 {
                    overflowed = true;
                    count = if free_run { 0 } else { reload };
                }
            }
        }

        let stored = (stored & !mask) | count as u16;
        self.ram[c_sfr as usize] = stored as u8;
        self.ram[c_sfr as usize + 1] = (stored >> 8) as u8;

        // Bit 6 gates the request; bit 7 tells the shared handler which PWM
        // it came from (p. 100). Software clears bit 7.
        let requested = overflowed && con & PWCON_ERQ != 0;
        if requested {
            self.ram[con_sfr as usize] |= PWCON_IRQ;
        }
        requested
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
                Some(ADC_CONVERSION_CYCLES)
            } else {
                None
            };
        } else if addr == SFR_P2 || addr == SFR_P3 {
            // P2.0..P2.3 are only the *data* for the injector output latch;
            // see `latch_injector_outputs`.
            let p2 = self.ram[SFR_P2 as usize];
            self.ac_clutch_active = (p2 & (1 << 4)) != 0;
            // P2.5..P2.7 are the shared A/B/C selects for U5 and U6, not fan
            // or purge outputs. Those board-output mappings remain unresolved.
        } else if addr == SFR_STBUF {
            self.serial_tx_queue.push(val);
        } else if addr == SFR_TCON0 && old & TCON_TROUT == 0 && val & TCON_TROUT != 0 {
            // Software strobe: the ROM sets TROUT by hand (0x4BD6, 0x4C03) to
            // clock a new injector state out immediately.
            self.latch_injector_outputs();
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
        (self.adc_input(channel) & 0x03FF) << 6
    }

    /// Resolve the voltage present at an MCU analog pin. The ROM drives the
    /// shared P28 mux select through P2.5..P2.7. Sampling at conversion
    /// completion leaves the previous ADCR result latched during settling.
    pub fn adc_input(&self, channel: usize) -> u16 {
        match channel {
            0 | 1 => {
                let select = (self.read_port(2) >> 5) as usize & 0x07;
                self.p28_mux_inputs[channel][select]
            }
            _ => self.adc_inputs[channel],
        }
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

    /// Latch a CYP (cylinder-#1) transition on TRNS0, exactly as the transition
    /// detector does when the distributor's CYP signal edges on P4.4. The ROM
    /// read-clears SFR_TRNS bit 0 in its TDC ISR; this is a hardware-set status
    /// bit, so it is OR-ed straight into the latch rather than going through a
    /// CPU write.
    pub fn signal_cyp(&mut self) {
        self.ram[SFR_TRNS as usize] |= 0x01;
    }

    /// Injector output stage of the P28, as the stock ROM drives it.
    ///
    /// P2.0..P2.3 are not the injector pins. The ROM prepares the *next*
    /// injector state on them (active low) and clocks it into the output
    /// stage with a rising edge of timer 0's realtime output, TM0IO/P3.4:
    ///
    ///   * open now: `ANDB P2, A` then `ORB TCON0, #4` / `ANDB TCON0, #0FBh`
    ///     at 0x4BCC..0x4BDF -- a hand-made TROUT pulse;
    ///   * close at an exact time: write the end time to TMR0 and pre-load P2
    ///     with the closed state (0x4CB5, 0x00C3). TRBUF is 1 (TCON0 = 8Bh),
    ///     so the compare match itself raises TROUT and clocks the state out
    ///     with no interrupt latency. The TM0 handler at 0x0086 only clears
    ///     TROUT and stages the following state;
    ///   * fuel cut: `ORB P2, #0Fh` then `SB TCON0.2` / `RB TCON0.2` at 0x4C00.
    ///
    /// Reading the raw P2 latch instead shows ~20 us "pulses", because the
    /// closed state is staged right after the opening strobe. This is derived
    /// from the ROM; the latch part itself has not been identified on the
    /// schematic.
    fn latch_injector_outputs(&mut self) {
        // The strobe only reaches the pin when P3.4 is a TM0IO output.
        if self.ram[SFR_P3SF as usize] & 0x10 == 0 || self.ram[SFR_P3IO as usize] & 0x10 == 0 {
            return;
        }
        let old = self.injector_outputs;
        let new = self.ram[SFR_P2 as usize] & 0x0F;
        self.injector_outputs = new;
        for bit in 0..4 {
            let mask = 1u8 << bit;
            match (old & mask != 0, new & mask != 0) {
                (true, false) => {
                    self.injector_started_at[bit] = Some(self.elapsed_cycles);
                    self.injector_open_count[bit] += 1;
                    self.injector_last_open_cycle = Some(self.elapsed_cycles);
                }
                (false, true) => {
                    if let Some(start) = self.injector_started_at[bit].take() {
                        let cycles = self.elapsed_cycles.saturating_sub(start);
                        let us = ((cycles + CYCLES_PER_US / 2) / CYCLES_PER_US)
                            .min(u32::MAX as u64) as u32;
                        self.injector_last_pulse_us[bit] = us;
                        self.injector_pulse_width_us = us;
                    }
                }
                _ => {}
            }
        }
    }

    /// Injector driver state, bit n = output n, active low (0 = open).
    pub fn injector_outputs(&self) -> u8 {
        self.injector_outputs
    }

    /// Width of the latest completed injector pulse, or 0 when no injector
    /// has opened within the last full engine cycle (two crank revolutions)
    /// at `rpm` -- i.e. the ROM is cutting fuel, so the last width is stale.
    pub fn live_injector_pulse_width_us(&self, rpm: f64) -> u32 {
        if rpm <= 0.0 {
            return 0;
        }
        let engine_cycle = (CPU_HZ as f64 * 120.0 / rpm) as u64;
        match self.injector_last_open_cycle {
            Some(opened) if self.elapsed_cycles.saturating_sub(opened) <= engine_cycle => {
                self.injector_pulse_width_us
            }
            _ => 0,
        }
    }

    /// Falling edge on TMnIO (P3.4 + n). In capture register mode the timer
    /// counter is loaded into the timer register and the timer's event
    /// interrupt is requested; TCERR (TCON bit 2) reports whether the counter
    /// ran a full lap since the previous event (user's manual p. 90). The pin
    /// only reaches the timer when it is an input handed to the secondary
    /// function. Returns the IRQ bits raised, for the interrupt controller.
    pub fn timer_capture_edge(&mut self, timer: usize) -> u16 {
        const EVENT_IRQ: [u16; 4] = [IRQ_TM0, IRQ_TM1, IRQ_TM2, IRQ_TM3];
        let tcon_sfr = (SFR_TCON0 + timer as u16) as usize;
        let tcon = self.ram[tcon_sfr];
        let pin = 1u8 << (4 + timer);
        let is_capture_input = self.ram[SFR_P3SF as usize] & pin != 0
            && self.ram[SFR_P3IO as usize] & pin == 0;
        if tcon & 0x03 != TIMER_MODE_CAPTURE || tcon & TCON_RUN == 0 || !is_capture_input {
            return 0;
        }

        let tm_sfr = (SFR_TM0 + timer as u16 * 4) as usize;
        self.ram[tm_sfr + 2] = self.ram[tm_sfr];
        self.ram[tm_sfr + 3] = self.ram[tm_sfr + 1];
        let tcerr = (self.capture_lapped[timer] as u8) << 2;
        self.capture_lapped[timer] = false;
        self.ram[tcon_sfr] = (tcon & !0x04) | tcerr;

        let irq = 1u16 << EVENT_IRQ[timer];
        let latched = self.ram[SFR_IRQ as usize] as u16
            | ((self.ram[(SFR_IRQ + 1) as usize] as u16) << 8)
            | irq;
        self.ram[SFR_IRQ as usize] = latched as u8;
        self.ram[(SFR_IRQ + 1) as usize] = (latched >> 8) as u8;
        irq
    }

    pub fn push_serial_rx_byte(&mut self, byte: u8) -> u16 {
        self.serial_rx_queue.push(byte);
        1 << IRQ_SERIAL_RX
    }

    // Hardware timer step
    pub fn tick_timers(&mut self, cycles: u32) -> u16 {
        let started_at = self.elapsed_cycles;
        self.elapsed_cycles = self.elapsed_cycles.wrapping_add(cycles as u64);

        // (counter SFR, reload SFR, control SFR, IRQ bit)
        const TIMERS: [(u16, u16, u16, u16); 4] = [
            (SFR_TM0, SFR_TMR0, SFR_TCON0, IRQ_TM0),
            (SFR_TM1, SFR_TMR1, SFR_TCON1, IRQ_TM1),
            (SFR_TM2, SFR_TMR2, SFR_TCON2, IRQ_TM2),
            (SFR_TM3, SFR_TMR3, SFR_TCON3, IRQ_TM3),
        ];

        let mut irq_flags: u16 = 0;
        let mut strobe_injectors = false;

        for pwm in 0..PWMS.len() {
            if self.tick_pwm(pwm, started_at, cycles) {
                irq_flags |= 1 << IRQ_PWM;
            }
        }

        if let Some(remaining) = self.adc_cycles_remaining {
            if cycles >= remaining {
                self.trigger_adc_conversion();
                irq_flags |= 1 << IRQ_ADC;
                // Scan mode continues producing one completion per conversion
                // until firmware clears ADSCAN.4.
                self.adc_cycles_remaining = if self.ram[SFR_ADSCAN as usize] & 0x10 != 0 {
                    Some(ADC_CONVERSION_CYCLES)
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

            // TCON layout, MSM66207 user's manual Fig. 9-2 (p. 85):
            //   bits 7..5  clock select: 1/2 CLK, then TBC taps
            //   bit  4     RUN
            //   bit  3     TRBUF (mode D)
            //   bit  2     TROUT (mode D) / TCERR (mode C)
            //   bits 1..0  mode: 00 auto reload, 01 clock output,
            //              10 capture register, 11 realtime output
            //
            // The clock periods are given at 10 MHz as 200 ns, 400 ns, 800 ns,
            // 1.6 us, 3.2 us, 12.8 us and 51.2 us; one machine cycle is 200 ns.
            // The P28 runs TM0/TM2/TM3 at 3.2 us (TCON = 8xh) and TM1 at
            // 800 ns (4Fh), which is the exact 4:1 ratio its timer-integrity
            // check at 0x38D8 demands. External clocking (111b, timers 0 and 1
            // only) is not modeled.
            let div = match tcon >> 5 {
                0b000 => 1,
                0b001 => 2,
                0b010 => 4,
                0b011 => 8,
                0b100 => 16,
                0b101 => 64,
                0b110 => 256,
                _ => continue,
            };
            self.timer_accum[i] += cycles;
            let steps = self.timer_accum[i] / div;
            self.timer_accum[i] %= div;
            if steps == 0 {
                continue;
            }

            let tm = self.read_data_u16(*tm_sfr);
            let tmr = self.read_data_u16(*tmr_sfr);
            let (mut new_tm, overflow) = tm.overflowing_add(steps as u16);
            // Did TM become equal to TMR during these steps?
            let matched = if overflow {
                tm < tmr || new_tm >= tmr
            } else {
                tm < tmr && new_tm >= tmr
            };

            match tcon & 0x03 {
                // Modes A/B (pp. 87-89): on overflow TM is reloaded from TMR.
                // There is no compare event.
                TIMER_MODE_AUTO_RELOAD | TIMER_MODE_CLOCK_OUTPUT => {
                    if overflow {
                        new_tm = tmr.wrapping_add(new_tm);
                    }
                }
                // Mode C (p. 90): TM free-runs and TMR is written only by a
                // falling edge on TMnIO (`timer_capture_edge`). Remember a
                // full counter lap between events for TCERR.
                TIMER_MODE_CAPTURE => {
                    if matched {
                        self.capture_lapped[i] = true;
                    }
                }
                // Mode D (p. 92): TM free-runs; when it equals TMR, TRBUF is
                // loaded into TROUT (driven on TMnIO) and the event interrupt
                // is requested. TMR is a compare value, never a reload --
                // subtracting it made identically clocked TM2/TM3 drift apart,
                // which the ROM's check at 0x38D8 detects.
                _ => {
                    if matched {
                        irq_flags |= 1 << *irq_bit;
                        let trout = (tcon >> 3) & 1;
                        self.ram[*tcon_sfr as usize] = (tcon & !TCON_TROUT) | (trout << 2);
                        if i == 0 && tcon & TCON_TROUT == 0 && trout != 0 {
                            strobe_injectors = true;
                        }
                    }
                }
            }
            self.write_data_u16(*tm_sfr, new_tm);

            // Every mode raises the counter-overflow interrupt on the even bit
            // just below the timer's event bit (Table 17-1): TM0->4, TM1->6,
            // TM2->8, TM3->10.
            if overflow {
                irq_flags |= 1 << (*irq_bit - 1);
            }
        }

        if strobe_injectors {
            self.latch_injector_outputs();
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
        bus.p28_mux_inputs[P28_MUX_U6_ADCR0][0] = 0x0155;
        bus.write_data_u16(SFR_IRQ, 0);
        bus.write_data_u8(SFR_ADSCAN, 0x10);

        assert_eq!(bus.read_data_u16(SFR_IRQ), 0);
        assert_eq!(bus.read_data_u16(SFR_ADCR0), 0);
        assert_eq!(
            bus.tick_timers(ADC_CONVERSION_CYCLES - 1) & (1 << IRQ_ADC),
            0
        );
        assert_eq!(bus.read_data_u16(SFR_ADCR0), 0);
        assert_ne!(bus.tick_timers(1) & (1 << IRQ_ADC), 0);
        assert_ne!(bus.read_data_u16(SFR_IRQ) & (1 << IRQ_ADC), 0);
        assert_eq!(bus.read_data_u16(SFR_ADCR0), 0x0155 << 6);
    }

    #[test]
    fn p28_shared_select_lines_route_both_mux_banks() {
        let mut bus = Bus::new();
        bus.write_data_u8(SFR_P2IO, 0xFF);
        for select in 0..8 {
            bus.p28_mux_inputs[P28_MUX_U6_ADCR0][select] = 100 + select as u16;
            bus.p28_mux_inputs[P28_MUX_U5_ADCR1][select] = 700 + select as u16;
        }

        for select in 0..8 {
            bus.write_data_u8(SFR_P2, (select as u8) << 5);
            bus.trigger_adc_conversion();

            assert_eq!(bus.read_data_u16(SFR_ADCR0), (100 + select as u16) << 6);
            assert_eq!(bus.read_data_u16(SFR_ADCR0 + 2), (700 + select as u16) << 6);
        }
    }

    #[test]
    fn mux_selection_settles_before_the_next_conversion_latches() {
        let mut bus = Bus::new();
        bus.write_data_u8(SFR_P2IO, 0xFF);
        bus.p28_mux_inputs[P28_MUX_U6_ADCR0][0] = 0x0100;
        bus.p28_mux_inputs[P28_MUX_U6_ADCR0][1] = 0x0200;
        bus.write_data_u8(SFR_P2, 0x00);
        bus.write_data_u8(SFR_ADSCAN, 0x10);

        assert_ne!(
            bus.tick_timers(ADC_CONVERSION_CYCLES) & (1 << IRQ_ADC),
            0
        );
        assert_eq!(bus.read_data_u16(SFR_ADCR0), 0x0100 << 6);

        bus.write_data_u8(SFR_P2, 0x20);
        assert_eq!(
            bus.tick_timers(ADC_CONVERSION_CYCLES - 1) & (1 << IRQ_ADC),
            0
        );
        assert_eq!(bus.read_data_u16(SFR_ADCR0), 0x0100 << 6);
        assert_ne!(bus.tick_timers(1) & (1 << IRQ_ADC), 0);
        assert_eq!(bus.read_data_u16(SFR_ADCR0), 0x0200 << 6);
    }

    #[test]
    fn p28_mux_does_not_change_direct_adc_channels() {
        let mut bus = Bus::new();
        bus.adc_inputs[2] = 0x02AA;
        bus.write_data_u8(SFR_P2IO, 0xFF);
        bus.write_data_u8(SFR_P2, 0xE0);

        bus.trigger_adc_conversion();

        assert_eq!(bus.read_data_u16(SFR_ADCR0 + 4), 0x02AA << 6);
    }

    #[test]
    fn timer_one_compare_does_not_bypass_the_adc_schedule() {
        let mut bus = Bus::new();
        bus.adc_inputs[2] = 0x0100;
        bus.trigger_adc_conversion();
        bus.adc_inputs[2] = 0x0200;
        bus.write_data_u16(SFR_TM1, 0);
        bus.write_data_u16(SFR_TMR1, 1);
        // Compare events exist only in realtime output mode.
        bus.write_data_u8(SFR_TCON1, TCON_RUN | TIMER_MODE_REALTIME_OUTPUT);

        assert_ne!(bus.tick_timers(1) & (1 << IRQ_TM1), 0);
        assert_eq!(bus.read_data_u16(SFR_ADCR0 + 4), 0x0100 << 6);
    }

    #[test]
    fn adc_result_registers_ignore_software_writes() {
        let mut bus = Bus::new();

        bus.write_data_u16(SFR_ADCR0, 0xFFFF);

        assert_eq!(bus.read_data_u16(SFR_ADCR0), 0);
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

    /// Put the ports and timer 0 in the state the P28 ROM's init leaves them.
    fn bus_with_p28_injector_stage() -> Bus {
        let mut bus = Bus::new();
        bus.write_data_u8(SFR_P2IO, 0xFF);
        bus.write_data_u8(SFR_P2, 0x1F);
        bus.write_data_u8(SFR_P3IO, 0xB1);
        bus.write_data_u8(SFR_P3SF, 0xFF);
        bus.write_data_u8(SFR_TCON0, 0x8B | TCON_RUN);
        bus
    }

    #[test]
    fn injector_state_is_clocked_out_by_the_timer_zero_realtime_output() {
        let mut bus = bus_with_p28_injector_stage();
        bus.write_data_u16(SFR_TM0, 0);
        bus.write_data_u16(SFR_TMR0, 100);

        // Open injector 0 the way 0x4BCC..0x4BDF does: stage, strobe by hand.
        bus.write_data_u8(SFR_P2, 0x1E);
        assert_eq!(bus.injector_outputs(), 0x0F, "P2 alone does not move the outputs");
        bus.write_data_u8(SFR_TCON0, 0x9B | TCON_TROUT);
        bus.write_data_u8(SFR_TCON0, 0x9B);
        assert_eq!(bus.injector_outputs(), 0x0E);

        // Stage the closed state; nothing happens until TM0 reaches TMR0.
        bus.write_data_u8(SFR_P2, 0x1F);
        bus.tick_timers(99 * 16);
        assert_eq!(bus.injector_outputs(), 0x0E);
        let irq = bus.tick_timers(16);
        assert_ne!(irq & (1 << IRQ_TM0), 0, "the match also requests the TM0 event");
        assert_eq!(bus.injector_outputs(), 0x0F);
        assert_ne!(bus.ram[SFR_TCON0 as usize] & TCON_TROUT, 0, "TROUT <- TRBUF");

        // 100 counts of 3.2 us.
        assert_eq!(bus.injector_last_pulse_us[0], 320);
        assert_eq!(bus.injector_pulse_width_us, 320);
        assert_eq!(bus.injector_open_count, [1, 0, 0, 0]);
    }

    #[test]
    fn timer_clock_select_follows_the_manual() {
        // (TCON clock field, machine cycles per count) -- Fig. 9-2.
        for (ck, cycles) in [(0u8, 1u32), (1, 2), (2, 4), (3, 8), (4, 16), (5, 64), (6, 256)] {
            let mut bus = Bus::new();
            bus.write_data_u8(SFR_TCON1, (ck << 5) | TCON_RUN | 0x03);
            bus.write_data_u16(SFR_TMR1, 0x8000);
            bus.tick_timers(cycles * 10);
            assert_eq!(bus.read_data_u16(SFR_TM1), 10, "clock select {ck}");
        }
    }

    #[test]
    fn auto_reload_mode_reloads_from_the_timer_register_on_overflow() {
        let mut bus = Bus::new();
        bus.write_data_u8(SFR_TCON1, TCON_RUN | TIMER_MODE_AUTO_RELOAD);
        bus.write_data_u16(SFR_TMR1, 0xFF00);
        bus.write_data_u16(SFR_TM1, 0xFFFE);

        let irq = bus.tick_timers(3);

        assert_eq!(bus.read_data_u16(SFR_TM1), 0xFF01);
        assert_ne!(irq & (1 << (IRQ_TM1 - 1)), 0, "overflow interrupt");
        assert_eq!(irq & (1 << IRQ_TM1), 0, "no compare event in mode A");
    }

    #[test]
    fn capture_mode_latches_the_counter_on_a_pin_edge_only() {
        let mut bus = Bus::new();
        bus.write_data_u8(SFR_P3IO, 0xB1);
        bus.write_data_u8(SFR_P3SF, 0xFF);
        bus.write_data_u8(SFR_TCON2, 0x82 | TCON_RUN);
        bus.write_data_u16(SFR_TMR2, 5);

        // Passing TMR2 is not an event in capture mode.
        assert_eq!(bus.tick_timers(16 * 20) & (1 << IRQ_TM2), 0);
        assert_eq!(bus.read_data_u16(SFR_TMR2), 5);

        let irq = bus.timer_capture_edge(2);
        assert_ne!(irq & (1 << IRQ_TM2), 0);
        assert_eq!(bus.read_data_u16(SFR_TMR2), 20);
        assert_ne!(bus.read_data_u16(SFR_IRQ) & (1 << IRQ_TM2), 0);
        assert_ne!(bus.ram[SFR_TCON2 as usize] & TCON_TROUT, 0, "TCERR: lapped TMR2");

        bus.tick_timers(16);
        bus.timer_capture_edge(2);
        assert_eq!(bus.ram[SFR_TCON2 as usize] & TCON_TROUT, 0, "TCERR clears");

        // A timer in realtime output mode ignores its pin.
        bus.write_data_u8(SFR_TCON3, 0x8F | TCON_RUN);
        assert_eq!(bus.timer_capture_edge(3), 0);
    }

    // ---- PWM0/PWM1: MSM66207 user's manual chapter 10 (pp. 96-109) ----
    //
    // The tests write PWCON as raw bytes on purpose, so they pin the bit
    // layout of Fig. 10-2 rather than whatever the constants happen to say:
    //   80h IRQ status, 40h overflow-request enable, 20h low active, 10h RUN,
    //   0Ch mode (00 auto reload, 01 free run, 1x PWM), 03h width / TBC tap.

    /// PWM0 with its pin routed to P4.2, configured at chip time 0.
    fn pwm0(pwcon: u8, pwmc: u16, pwmr: u16) -> Bus {
        let mut bus = Bus::new();
        bus.write_data_u8(SFR_P4SF, 0x0C);
        bus.write_data_u16(SFR_PWMC0, pwmc);
        bus.write_data_u16(SFR_PWMR0, pwmr);
        bus.write_data_u8(SFR_PWCON0, pwcon);
        bus
    }

    fn pwm0_pin(bus: &mut Bus) -> bool {
        bus.read_data_u8(SFR_P4) & 0x04 != 0
    }

    /// Advance one machine cycle at a time and count the cycles that end with
    /// P4.2 high.
    fn pwm0_high_cycles(bus: &mut Bus, cycles: u32) -> u32 {
        (0..cycles)
            .filter(|_| {
                bus.tick_timers(1);
                pwm0_pin(bus)
            })
            .count() as u32
    }

    #[test]
    fn pwm_mode_sets_the_output_on_the_tbc_tap_and_resets_it_on_counter_overflow() {
        // 8 bits: TBC7, one period = 256 machine cycles (51.2 us, Table 10-4).
        let mut bus = pwm0(0x18, 0x80, 0xC0);

        // The counter was started mid-range; its first overflow (t = 128) only
        // confirms the output F/F in its reset state (Fig. 10-6).
        bus.tick_timers(255);
        assert!(!pwm0_pin(&mut bus), "inactive before the first tap edge");

        // Fig. 10-5: the tap's falling edge loads PWMR and sets the F/F.
        bus.tick_timers(1);
        assert!(pwm0_pin(&mut bus), "set at the head of the period");
        assert_eq!(bus.read_data_u16(SFR_PWMC0) & 0xFF, 0xC0, "PWMC <- PWMR");

        // 100h - C0h = 64 counts of one machine cycle each.
        bus.tick_timers(63);
        assert!(pwm0_pin(&mut bus));
        bus.tick_timers(1);
        assert!(!pwm0_pin(&mut bus), "reset by the PWMC overflow");

        bus.tick_timers(191);
        assert!(!pwm0_pin(&mut bus));
        bus.tick_timers(1);
        assert!(pwm0_pin(&mut bus), "next period starts 256 cycles after the last");
    }

    #[test]
    fn pwm_mode_period_and_counter_width_follow_pwcon_bits_1_0() {
        // Fig. 10-4 / Table 10-4: 8, 10, 12 and 16 bits on TBC7, 9, 11, 15.
        for (ck, bits) in [(0u8, 8u32), (1, 10), (2, 12), (3, 16)] {
            let period = 1u32 << bits;
            // Ten counts short of overflow, in however many bits are used.
            let mut bus = pwm0(0x18 | ck, 0, 0u16.wrapping_sub(10));
            bus.tick_timers(2 * period);

            assert_eq!(
                pwm0_high_cycles(&mut bus, 2 * period),
                20,
                "PWCON bits 1-0 = {ck}: 10 active cycles per {period}-cycle period"
            );
        }
    }

    #[test]
    fn pwm_register_zero_gives_one_full_period_and_then_zero_duty() {
        // Fig. 10-8, upper trace: PWR = FF, then 00, then 00.
        let mut bus = pwm0(0x18, 0x80, 0xFF);
        bus.tick_timers(300);
        bus.write_data_u16(SFR_PWMR0, 0x00);

        // Head of the next period (t = 512): PWMC <- 00, output set. The
        // counter now needs exactly one period to overflow.
        bus.tick_timers(212);
        assert!(pwm0_pin(&mut bus));
        bus.tick_timers(255);
        assert!(pwm0_pin(&mut bus), "active for the whole period");

        // p. 103: an overflow that coincides with the tap's falling edge
        // holds the F/F reset, so from here on the duty is 0 %.
        bus.tick_timers(1);
        assert!(!pwm0_pin(&mut bus));
        assert_eq!(pwm0_high_cycles(&mut bus, 4 * 256), 0);
    }

    #[test]
    fn pwm_duty_spans_one_count_to_one_count_short_of_the_period() {
        for (pwmr, active) in [(0xFFu16, 1u32), (0x80, 128), (0x01, 255)] {
            let mut bus = pwm0(0x18, 0x80, pwmr);
            bus.tick_timers(2 * 256);

            assert_eq!(
                pwm0_high_cycles(&mut bus, 2 * 256),
                2 * active,
                "PWMR = {pwmr:02X}h"
            );
        }
    }

    #[test]
    fn pwm_low_active_output_idles_high_and_pulses_low() {
        // Bit 5 set, counter stopped: the F/F is reset, so the pin rests high.
        let mut stopped = pwm0(0x28, 0x80, 0xC0);
        assert!(pwm0_pin(&mut stopped));

        let mut bus = pwm0(0x38, 0x80, 0xC0);
        bus.tick_timers(2 * 256);
        assert_eq!(pwm0_high_cycles(&mut bus, 2 * 256), 2 * (256 - 64));
    }

    #[test]
    fn stopped_pwm_neither_counts_nor_drives_nor_interrupts() {
        let mut bus = pwm0(0x48, 0x80, 0xC0);
        bus.write_data_u16(SFR_IRQ, 0);

        assert_eq!(bus.tick_timers(70_000) & (1 << IRQ_PWM), 0);

        assert_eq!(bus.read_data_u16(SFR_PWMC0) & 0xFF, 0x80);
        assert!(!pwm0_pin(&mut bus));
        assert_eq!(bus.read_data_u16(SFR_IRQ) & (1 << IRQ_PWM), 0);
    }

    #[test]
    fn pwm_overflow_requests_the_shared_interrupt_only_when_enabled() {
        // Bit 6 set: the overflow requests IRQ bit 13 and bit 7 records which
        // of the two PWMs it came from (p. 100).
        let mut bus = pwm0(0x58, 0xFE, 0xC0);
        bus.write_data_u16(SFR_IRQ, 0);
        assert_eq!(bus.tick_timers(1) & (1 << IRQ_PWM), 0);
        assert_ne!(bus.tick_timers(1) & (1 << IRQ_PWM), 0);
        assert_ne!(bus.read_data_u16(SFR_IRQ) & (1 << IRQ_PWM), 0);
        assert_eq!(bus.read_data_u8(SFR_PWCON0) & 0x80, 0x80);
        assert_eq!(bus.read_data_u8(SFR_PWCON1) & 0x80, 0, "PWM1 did not overflow");

        // Bit 6 clear: same overflow, no request and no status.
        let mut bus = pwm0(0x18, 0xFE, 0xC0);
        bus.write_data_u16(SFR_IRQ, 0);
        assert_eq!(bus.tick_timers(2) & (1 << IRQ_PWM), 0);
        assert_eq!(bus.read_data_u16(SFR_IRQ) & (1 << IRQ_PWM), 0);
        assert_eq!(bus.read_data_u8(SFR_PWCON0) & 0x80, 0);
    }

    #[test]
    fn pwm_auto_reload_timer_mode_counts_tap_periods_and_reloads_from_pwmr() {
        // Mode bits 3-2 = 00, bits 1-0 = 00: an 8-bit counter clocked by TBC7
        // (Fig. 10-10). Table 10-5: overflow period = 51.2 us x (256 - D).
        let mut bus = pwm0(0x50, 0xFE, 0xF0);

        assert_eq!(bus.tick_timers(511) & (1 << IRQ_PWM), 0);
        assert_eq!(bus.read_data_u16(SFR_PWMC0) & 0xFF, 0xFF, "one count per 256 cycles");
        assert_ne!(bus.tick_timers(1) & (1 << IRQ_PWM), 0);
        assert_eq!(bus.read_data_u16(SFR_PWMC0) & 0xFF, 0xF0, "reloaded from PWMR");

        assert_eq!(bus.tick_timers(16 * 256 - 1) & (1 << IRQ_PWM), 0);
        assert_ne!(bus.tick_timers(1) & (1 << IRQ_PWM), 0, "modulo-16 from then on");
        assert!(!pwm0_pin(&mut bus), "the output F/F belongs to PWM mode only");
    }

    #[test]
    fn pwm_free_run_counter_mode_is_sixteen_bits_wide_and_never_reloads() {
        // Mode bits 3-2 = 01 (Fig. 10-12): PWMC is fixed at 16 bits and bits
        // 1-0 only pick the clock, here TBC9 = 1024 machine cycles.
        let mut bus = pwm0(0x55, 0xFFFE, 0x1234);

        assert_eq!(bus.tick_timers(1024) & (1 << IRQ_PWM), 0);
        assert_eq!(bus.read_data_u16(SFR_PWMC0), 0xFFFF);
        assert_ne!(bus.tick_timers(1024) & (1 << IRQ_PWM), 0);
        assert_eq!(bus.read_data_u16(SFR_PWMC0), 0x0000, "modulo 65536, PWMR unused");
    }

    #[test]
    fn unused_pwm_register_bits_read_as_ones() {
        // Table 10-2.
        for (ck, read) in [(0u8, 0xFF12u16), (1, 0xFC12), (2, 0xF012), (3, 0x0012)] {
            let mut bus = pwm0(0x08 | ck, 0, 0x0012);
            assert_eq!(bus.read_data_u16(SFR_PWMR0), read, "PWCON bits 1-0 = {ck}");
            assert_eq!(bus.read_data_u8(SFR_PWMR0 + 1), (read >> 8) as u8);
        }
    }

    /// PWM1 as the P28 ROM programs it at 0x22E9..0x22FA.
    fn p28_pwm1() -> Bus {
        let mut bus = Bus::new();
        bus.write_data_u8(SFR_P4SF, 0xFC);
        bus.write_data_u8(SFR_PWCON1, 0x7E);
        bus.write_data_u16(SFR_PWMC1, 0xFF00);
        bus.write_data_u16(SFR_PWMR1, 0xFF00);
        bus
    }

    #[test]
    fn p28_pwm_setup_is_a_low_pulse_of_256_cycles_every_4096() {
        // 7Eh = request enabled, low active, RUN, PWM mode, 12 bits / TBC11.
        // FF00h in 12 bits is F00h: 100h counts to overflow.
        let mut bus = p28_pwm1();
        bus.tick_timers(2 * 4096);

        let low = (0..2 * 4096)
            .filter(|_| {
                bus.tick_timers(1);
                bus.read_data_u8(SFR_P4) & 0x08 == 0
            })
            .count();

        assert_eq!(low, 2 * 256);
    }

    #[test]
    fn pwm_state_does_not_depend_on_how_the_elapsed_time_is_sliced() {
        let mut fine = p28_pwm1();
        let mut coarse = p28_pwm1();

        for chunk in [1u32, 7, 300, 3788, 4096, 5000, 13, 70_000, 255, 4097] {
            let mut fine_irq = false;
            for _ in 0..chunk {
                fine_irq |= fine.tick_timers(1) & (1 << IRQ_PWM) != 0;
            }
            let coarse_irq = coarse.tick_timers(chunk) & (1 << IRQ_PWM) != 0;

            assert_eq!(coarse_irq, fine_irq, "request during a {chunk}-cycle step");
            assert_eq!(
                coarse.read_data_u16(SFR_PWMC1),
                fine.read_data_u16(SFR_PWMC1),
                "PWMC1 after a {chunk}-cycle step"
            );
            assert_eq!(
                coarse.read_data_u8(SFR_P4) & 0x08,
                fine.read_data_u8(SFR_P4) & 0x08,
                "P4.3 after a {chunk}-cycle step"
            );
        }
    }
}
