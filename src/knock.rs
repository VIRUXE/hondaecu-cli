//! The knock board behind connector FC1, and what the ROM sees of it.
//!
//! A P30 box carries a knock board on FC1; a P28 box leaves FC1 empty
//! (Bomarc sheet 5, drawn for a P28 board 02D01720-1500: everything between
//! FC1 and the board is dashed except the pull resistors). What the ROM
//! requires of the board is read from the stock P30 mask-203 image
//! (`docs/CPU_CORE.md`, "P30 box"); what follows is a model of a board that
//! meets it, with the assumptions stated.
//!
//! **The exchange.** Once per CKP tooth the ROM's tooth routine reads 82C55
//! port A (0x4DFF) and rotates bit 0, then bit 1, into the word at `1D0h`
//! (0x4E00..0x4E05); on the tooth where its count of teeth since TDC (`D2h`)
//! is 0 it moves the word to `F0h` and clears it (0x4E0C..0x4E10). A frame is
//! therefore the six pairs read at counts 1, 2, 3, 4, 5, 0, landing in bits
//! 4..15 of `F0h`, pair k (1..6) in bits `2k + 2` (port A bit 0) and `2k + 3`
//! (bit 1). The ROM decodes it at 0x0CAA..0x0CD0:
//!
//! - bits 4 and 5 must both be 1, and the five bit-1 bits (7, 9, ..15) and the
//!   five bit-0 bits (6, 8, ..14) must each hold an even number of ones; else
//!   `233h.7` is set (frame bad);
//! - bit 8 goes to `232h.7`, which 0x3D5E turns into `9Ah.2`, code 23 (the
//!   knock-sensor code, table 6C99h index 19 = 17h);
//! - bits 9..15 are the knock level (`243h`, 0x0CED..0x0D06);
//! - bits 6 and 7 are used for nothing but the parity count, so they are the
//!   board's parity bits.
//!
//! Port A bit 2 is the board's heartbeat: 0x27F7..0x2824 reloads `2E8h` with
//! 0Ah whenever the bit has changed since the last pass, and sets `9Ah.3`
//! (code 24, index 20 = 18h) once `2E8h`, counted down every pass, reaches 0.
//! Passes are about 10 ms apart (observed), so the bit must change at least
//! once in about 100 ms. With a bad frame (`233h.7`) the reload is withheld
//! unless `ADh` is above 0Dh (0x280F..0x2815).
//!
//! The ROM also sends a frame of its own on port C bits 7 and 3 (0x4E1C..
//! 0x4E23), two bits per tooth from `EEh`; the model does not read it.
//!
//! **What the board gets from the box** (sheets 4 and 5): FC1 pin 3 is the
//! shaped CKP signal ("H", HIC pin 17), pin 14 the shaped CYP signal ("I",
//! HIC pin 12), pin 2 the reset line through R107, pin 1 +5 V, pins 11 and
//! 12 ground, pin 10 the sensor (connector D10). So the board can count
//! teeth and find cylinder 1, and that is how the model keeps in step.

/// What sits on connector FC1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KnockBoard {
    /// Nothing: a P28 box. A knock sensor wired to the car reaches only FC1
    /// pin 10 (sheet 5, connector D10), so with no board the ROM cannot see
    /// whether one is fitted.
    #[default]
    Absent,
    /// A P30 box with the sensor unplugged.
    FittedNoSensor,
    /// A P30 box with a working sensor.
    FittedWithSensor,
}

/// Port A bits the knock board drives.
pub const KNOCK_PA_DATA: u8 = 0b011;
pub const KNOCK_PA_HEARTBEAT: u8 = 0b100;

/// The reply frame, pair k (1..6) in bits `2(k-1)` (port A bit 0) and
/// `2(k-1) + 1` (bit 1). A healthy board: the two start bits, level 0 (no
/// knock: the script has no responding engine), no fault, parity even.
const FRAME_HEALTHY: u16 = 0b00_0000_0011;
/// The sensor-fault frame: start bits, fault bit (`F0h` bit 8, pair 3 bit 0)
/// and its parity bit (`F0h` bit 6, pair 2 bit 0), level 0.
const FRAME_NO_SENSOR: u16 = 0b01_0111;

/// Heartbeat half period. Assumption: the real rate is not documented; the
/// ROM accepts any change at least every ~100 ms (see the module comment).
/// 20 ms is inside that window and keeps at most one level change between
/// two of the ROM's passes, so no change is ever missed.
pub const HEARTBEAT_HALF_PERIOD_CYCLES: u64 = crate::bus::CPU_HZ / 50;

/// The board's view of the crank: CKP teeth since the last CYP edge.
#[derive(Debug, Clone, Default)]
pub struct KnockLink {
    teeth_since_cyp: Option<u32>,
    /// The pair on port A bits 0 and 1 until the next tooth.
    pair: u8,
}

impl KnockLink {
    /// A CYP edge (FC1 pin 14).
    pub fn cyp(&mut self) {
        self.teeth_since_cyp = Some(0);
    }

    /// A CKP tooth (FC1 pin 3): put the next pair on the lines.
    ///
    /// Phase: in the engine model CYP falls between teeth 3 and 4 and the ROM
    /// reads its own tooth count `D2h` as 3 on the tooth after it (0x04B5
    /// requires that, `engine.rs`). So the m-th tooth after CYP is read at
    /// count `(m + 2) mod 6`, which is pair `(m + 1) mod 6 + 1`. Assumption:
    /// the real board keeps the same phase; it must, for the ROM to see its
    /// start bits, but its own timing is not documented. Before the first
    /// CYP the board does not know the phase and holds both lines low.
    pub fn tooth(&mut self, board: KnockBoard) {
        let Some(m) = self.teeth_since_cyp.as_mut() else {
            self.pair = 0;
            return;
        };
        *m += 1;
        let k = (*m + 1) % 6 + 1;
        let frame = match board {
            KnockBoard::Absent => 0,
            KnockBoard::FittedNoSensor => FRAME_NO_SENSOR,
            KnockBoard::FittedWithSensor => FRAME_HEALTHY,
        };
        self.pair = ((frame >> (2 * (k - 1))) & 0b11) as u8;
    }

    /// Port A bits 0..2 as the board leaves them at machine cycle `now`.
    ///
    /// With nothing on FC1, each line sits on its pull-up (sheet 5: R111 on
    /// the bit 0 line into R112, R110 on bit 1 into R113, R109 22 kohm on bit 2
    /// into R114 2.2 kohm; drawn solid, i.e. fitted, unlike the parts that
    /// exist only with a board), so all three read 1. The ROM's verdict does
    /// not hang on that level: bit 2 never changes, so 0x27F7 never reloads
    /// `2E8h` and code 24 follows whatever the three levels are.
    pub fn port_a(&self, board: KnockBoard, now: u64) -> u8 {
        match board {
            KnockBoard::Absent => KNOCK_PA_DATA | KNOCK_PA_HEARTBEAT,
            KnockBoard::FittedNoSensor | KnockBoard::FittedWithSensor => {
                let beat = (now / HEARTBEAT_HALF_PERIOD_CYCLES) % 2 == 1;
                self.pair | if beat { KNOCK_PA_HEARTBEAT } else { 0 }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 12-bit word the ROM captures in `F0h` (bits 4..15) when `link`
    /// is stepped through one frame, read the way 0x4DFF..0x4E05 reads it:
    /// on each tooth, rotate bit 0 then bit 1 in from the top.
    fn captured(board: KnockBoard) -> u16 {
        let mut link = KnockLink::default();
        link.cyp();
        // Teeth 1..3 after CYP are read at counts 3, 4, 5; tooth 4 at count
        // 0, which ends a frame the board was not yet in step for.
        for _ in 0..4 {
            link.tooth(board);
        }
        let mut word: u16 = 0;
        for _ in 0..6 {
            link.tooth(board);
            let a = link.port_a(board, 0);
            word = (word >> 1) | (((a & 1) as u16) << 15);
            word = (word >> 1) | ((((a >> 1) & 1) as u16) << 15);
        }
        word
    }

    /// 0x0CAA..0x0CD0: start bits, then even parity in each lane.
    fn accepted(word: u16) -> bool {
        let lane = |first: u32| (0..5).map(|i| (word >> (first + 2 * i)) & 1).sum::<u16>() % 2 == 0;
        word & 0x30 == 0x30 && lane(6) && lane(7)
    }

    #[test]
    fn a_healthy_board_sends_start_bits_and_nothing_else() {
        let w = captured(KnockBoard::FittedWithSensor);
        assert_eq!(w, 0x0030);
        assert!(accepted(w));
    }

    #[test]
    fn a_board_without_its_sensor_sets_the_fault_bit_with_good_parity() {
        let w = captured(KnockBoard::FittedNoSensor);
        assert_eq!(w & 0x0100, 0x0100, "{w:04X}");
        assert_eq!(w >> 9, 0, "level");
        assert!(accepted(w), "{w:04X}");
    }

    #[test]
    fn with_no_board_every_line_sits_on_its_pull_up() {
        let link = KnockLink::default();
        for now in [0, HEARTBEAT_HALF_PERIOD_CYCLES, 7 * HEARTBEAT_HALF_PERIOD_CYCLES] {
            assert_eq!(link.port_a(KnockBoard::Absent, now), 0b111);
        }
        assert!(!accepted(0xFFF0));
    }

    #[test]
    fn a_fitted_board_beats_on_bit_2_and_an_absent_one_does_not() {
        let link = KnockLink::default();
        for board in [KnockBoard::FittedNoSensor, KnockBoard::FittedWithSensor] {
            let a = link.port_a(board, 0) & KNOCK_PA_HEARTBEAT;
            let b = link.port_a(board, HEARTBEAT_HALF_PERIOD_CYCLES) & KNOCK_PA_HEARTBEAT;
            assert_ne!(a, b, "{board:?}");
        }
    }

    #[test]
    fn before_the_first_cyp_the_board_holds_its_data_lines_low() {
        let mut link = KnockLink::default();
        link.tooth(KnockBoard::FittedWithSensor);
        assert_eq!(link.port_a(KnockBoard::FittedWithSensor, 0) & KNOCK_PA_DATA, 0);
    }
}
