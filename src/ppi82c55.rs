// OKI MSM82C55A-2 programmable peripheral interface, mode 0 only.
//
// The P28 board hangs one of these on the external data bus. The stock ROM
// programs it once (0x51E4: control word 90h) and then uses port A as eight
// digital inputs and ports B and C as sixteen latched outputs.
//
// Source: `66k-assembler-docs/82C55A-2RS.pdf`.

/// Register index 0..3, the value on the chip's A1:A0 pins.
pub const PORT_A: u8 = 0;
pub const PORT_B: u8 = 1;
pub const PORT_C: u8 = 2;
pub const CONTROL: u8 = 3;

/// Control word bit 7: 1 = mode definition, 0 = port C bit set/reset.
const MODE_SET: u8 = 1 << 7;
/// Mode-definition bits 6..5 (group A) and 2 (group B). Non-zero selects the
/// strobed modes 1 and 2, which this model does not implement.
const MODE_BITS: u8 = 0b0110_0100;
/// Mode-definition direction bits; 1 = input.
const DIR_A: u8 = 1 << 4;
const DIR_C_UPPER: u8 = 1 << 3;
const DIR_B: u8 = 1 << 1;
const DIR_C_LOWER: u8 = 1 << 0;

/// Control word after RESET: mode 0, every port an input (datasheet p. 9).
pub const RESET_CONTROL: u8 = MODE_SET | DIR_A | DIR_C_UPPER | DIR_B | DIR_C_LOWER;

pub struct Ppi82c55 {
    /// Last mode-definition word written, or `RESET_CONTROL`.
    pub control: u8,
    /// Output registers of ports A, B and C.
    latch: [u8; 3],
    /// Levels driven onto the port pins from outside the chip. Only bits
    /// whose port (or port C half) is an input are ever read.
    pub pins: [u8; 3],
    /// A mode word selected mode 1 or 2. The ports keep behaving as mode 0.
    pub unsupported_mode: bool,
}

impl Ppi82c55 {
    pub fn new() -> Self {
        Self {
            control: RESET_CONTROL,
            latch: [0; 3],
            pins: [0; 3],
            unsupported_mode: false,
        }
    }

    /// Bits of `port` that are currently inputs.
    fn input_mask(&self, port: u8) -> u8 {
        let input = |bit: u8| self.control & bit != 0;
        match port {
            PORT_A if input(DIR_A) => 0xFF,
            PORT_B if input(DIR_B) => 0xFF,
            PORT_C => {
                (if input(DIR_C_UPPER) { 0xF0 } else { 0 })
                    | (if input(DIR_C_LOWER) { 0x0F } else { 0 })
            }
            _ => 0,
        }
    }

    /// Level on a port's pins: the output register where the port drives
    /// them, the external level where it does not.
    pub fn port(&self, port: u8) -> u8 {
        let p = port as usize;
        let inputs = self.input_mask(port);
        (self.pins[p] & inputs) | (self.latch[p] & !inputs)
    }

    /// A port's output register, if the whole port is currently an output.
    pub fn output(&self, port: u8) -> Option<u8> {
        (self.input_mask(port) == 0).then(|| self.latch[port as usize])
    }

    /// CPU read. In mode 0 an output port reads back its own register, which
    /// the P28 ROM relies on: its integrity check at 0x28D6 writes ports B and
    /// C and faults (code 4Dh) unless both read back unchanged.
    ///
    /// Reading the control register is not a defined operation on this part;
    /// `None` tells the bus nothing drove the data lines.
    pub fn read(&self, reg: u8) -> Option<u8> {
        match reg {
            PORT_A | PORT_B | PORT_C => Some(self.port(reg)),
            _ => None,
        }
    }

    pub fn write(&mut self, reg: u8, val: u8) {
        match reg {
            // A write always loads the output register; it only reaches the
            // pins while the port is an output.
            PORT_A | PORT_B | PORT_C => self.latch[reg as usize] = val,
            _ if val & MODE_SET != 0 => {
                self.control = val;
                self.unsupported_mode = val & MODE_BITS != 0;
                // Datasheet p. 12: a mode write clears the port A and C output
                // registers and leaves port B undefined. Port B is cleared
                // here too; the ROM rewrites both B and C straight after.
                self.latch = [0; 3];
            }
            _ => {
                // Bit set/reset: bits 3..1 select a port C bit, bit 0 is its
                // new value.
                let mask = 1u8 << ((val >> 1) & 0x07);
                if val & 1 != 0 {
                    self.latch[PORT_C as usize] |= mask;
                } else {
                    self.latch[PORT_C as usize] &= !mask;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_leaves_every_port_an_input() {
        let mut ppi = Ppi82c55::new();
        ppi.pins = [0x12, 0x34, 0x56];
        ppi.write(PORT_B, 0xFF);

        assert_eq!(ppi.read(PORT_A), Some(0x12));
        assert_eq!(ppi.read(PORT_B), Some(0x34));
        assert_eq!(ppi.read(PORT_C), Some(0x56));
    }

    #[test]
    fn p28_mode_word_makes_a_an_input_and_b_c_readable_outputs() {
        let mut ppi = Ppi82c55::new();
        ppi.pins = [0xA5, 0xFF, 0xFF];
        ppi.write(CONTROL, 0x90);
        ppi.write(PORT_B, 0xEB);
        ppi.write(PORT_C, 0x44);
        ppi.write(PORT_A, 0x00);

        assert_eq!(ppi.read(PORT_A), Some(0xA5), "port A follows its pins");
        assert_eq!(ppi.read(PORT_B), Some(0xEB), "port B reads back its latch");
        assert_eq!(ppi.read(PORT_C), Some(0x44), "port C reads back its latch");
        assert!(!ppi.unsupported_mode);
        assert_eq!(ppi.read(CONTROL), None);
    }

    #[test]
    fn mode_write_clears_the_output_registers() {
        let mut ppi = Ppi82c55::new();
        ppi.write(CONTROL, 0x90);
        ppi.write(PORT_B, 0xFF);
        ppi.write(PORT_C, 0xFF);
        ppi.write(CONTROL, 0x90);

        assert_eq!(ppi.port(PORT_B), 0);
        assert_eq!(ppi.port(PORT_C), 0);
    }

    #[test]
    fn port_c_halves_have_separate_directions() {
        let mut ppi = Ppi82c55::new();
        ppi.pins[PORT_C as usize] = 0xA5;
        // Port C upper input, lower output.
        ppi.write(CONTROL, 0x88);
        ppi.write(PORT_C, 0x3C);

        assert_eq!(ppi.read(PORT_C), Some(0xAC));
    }

    #[test]
    fn bit_set_reset_changes_one_port_c_bit() {
        let mut ppi = Ppi82c55::new();
        ppi.write(CONTROL, 0x90);
        ppi.write(CONTROL, 0x0B); // set bit 5
        assert_eq!(ppi.port(PORT_C), 0x20);
        ppi.write(CONTROL, 0x0A); // reset bit 5
        assert_eq!(ppi.port(PORT_C), 0x00);
    }

    #[test]
    fn strobed_modes_are_flagged_not_emulated() {
        let mut ppi = Ppi82c55::new();
        ppi.write(CONTROL, 0xB0); // group A mode 1
        assert!(ppi.unsupported_mode);
    }
}
