// OKI MSM66207 / 66201 CPU Registers & Flags
// Used in Honda OBD1 ECUs (P28, P30, P72, etc.)

#[derive(Debug, Clone)]
pub struct Cpu {
    pub pc: u16,      // Program Counter
    pub a: u16,       // 16-bit Accumulator (Word mode: A, Byte mode: AL/AH)
    pub dp: u16,      // Data Pointer (DPH, DPL)
    pub x1: u16,      // Index Register 1
    pub x2: u16,      // Index Register 2
    pub usp: u16,     // User Stack Pointer
    pub ssp: u16,     // System Stack Pointer
    // Local Register Bank pointer. This is its own 16-bit register (SFR 0x02),
    // NOT a field packed into PSW: bits 5..7 select the RAM page and bits 0..4
    // select the bank within it. See bank_base() / off_page().
    pub lrb: u16,

    // PSW Flags
    pub zf: bool,     // Zero Flag
    pub cf: bool,     // Carry / Borrow Flag (CF=1 on borrow for SUB/CMP!)
    pub hc: bool,     // Half Carry Flag
    pub dd: bool,     // Data Width Mode: true = 16-bit Word mode, false = 8-bit Byte mode
    // PSW bits with no named flag. The ROM uses several as scratch (PSWH.0 is
    // MIE), so they must survive PUSHS/POPS and interrupt entry/exit rather
    // than being dropped.
    pub psw_other: u16,

    // Execution statistics
    pub cycles: u64,
    pub instructions: u64,
    pub halted: bool,
}

impl Cpu {
    pub fn new() -> Self {
        Self {
            pc: 0x0000,
            a: 0,
            dp: 0,
            x1: 0,
            x2: 0,
            usp: 0x0700,
            ssp: 0x07FE,
            lrb: 0,
            psw_other: 0,
            zf: false,
            cf: false,
            hc: false,
            dd: false, // Reset PSW is 0x0CC8; DD starts in byte mode.
            cycles: 0,
            instructions: 0,
            halted: false,
        }
    }

    pub fn al(&self) -> u8 {
        (self.a & 0xFF) as u8
    }

    pub fn ah(&self) -> u8 {
        ((self.a >> 8) & 0xFF) as u8
    }

    pub fn set_al(&mut self, val: u8) {
        self.a = (self.a & 0xFF00) | (val as u16);
    }

    pub fn set_ah(&mut self, val: u8) {
        self.a = (self.a & 0x00FF) | ((val as u16) << 8);
    }

    pub fn dpl(&self) -> u8 {
        (self.dp & 0xFF) as u8
    }

    pub fn dph(&self) -> u8 {
        ((self.dp >> 8) & 0xFF) as u8
    }

    pub fn set_dpl(&mut self, val: u8) {
        self.dp = (self.dp & 0xFF00) | (val as u16);
    }

    pub fn set_dph(&mut self, val: u8) {
        self.dp = (self.dp & 0x00FF) | ((val as u16) << 8);
    }

    // PSW layout, from the MSM66201/66P201/66207/66P207 datasheet, p. 9:
    //   bits 0-2  SCB (System Control Base) -- selects pointing register set
    //             PR0..PR7, which is where X1/X2/DP/USP physically live
    //             (manual Fig. 1-5). Plain storage as far as the CPU core is
    //             concerned; exec.rs reads it to resolve those registers.
    //   bits 15-12 CF, ZF, HC, DD
    //   bits 9,5,4 user flags; bit 8 master interrupt enable (MIE)
    //
    // The short hardware datasheet labels bit 8 "MIP", but the full
    // MSM66201/207 user's manual calls it MIE and defines 1 = all maskable
    // interrupts enabled, 0 = disabled. Interrupt entry saves PSW and clears
    // MIE; RTI restores the saved PSW.
    // Bits 3, 6, 7, 10 and 11 are not implemented and read as 1. This yields
    // the documented reset PSW of 0x0CC8 with every writable flag cleared.
    const PSW_READS_ONE: u16 = 0x0CC8;
    /// Bits PSW keeps verbatim after excluding the four named arithmetic/data
    /// flags. Leaving a named flag in this shadow copy would let a stale bit
    /// override its live `Cpu` boolean when `psw_u16()` combines the fields.
    const PSW_STORAGE: u16 =
        (1 << 9) | (1 << 8) | (1 << 5) | (1 << 4) | 0b0000_0111;

    pub const PSW_CF_BIT: u16 = 1 << 15;
    pub const PSW_ZF_BIT: u16 = 1 << 14;
    pub const PSW_HC_BIT: u16 = 1 << 13;
    pub const PSW_DD_BIT: u16 = 1 << 12;
    pub const PSW_MIE_BIT: u16 = 1 << 8;

    pub fn psw_u16(&self) -> u16 {
        let mut psw: u16 = (self.psw_other & Self::PSW_STORAGE) | Self::PSW_READS_ONE;
        if self.cf { psw |= Self::PSW_CF_BIT; }
        if self.zf { psw |= Self::PSW_ZF_BIT; }
        if self.hc { psw |= Self::PSW_HC_BIT; }
        if self.dd { psw |= Self::PSW_DD_BIT; }
        psw
    }

    pub fn set_psw_u16(&mut self, val: u16) {
        self.cf = (val & Self::PSW_CF_BIT) != 0;
        self.zf = (val & Self::PSW_ZF_BIT) != 0;
        self.hc = (val & Self::PSW_HC_BIT) != 0;
        self.dd = (val & Self::PSW_DD_BIT) != 0;
        self.psw_other = val & Self::PSW_STORAGE;
    }

    pub fn mie(&self) -> bool {
        self.psw_other & Self::PSW_MIE_BIT != 0
    }

    pub fn set_mie(&mut self, enabled: bool) {
        if enabled {
            self.psw_other |= Self::PSW_MIE_BIT;
        } else {
            self.psw_other &= !Self::PSW_MIE_BIT;
        }
    }

    /// System Control Base: which pointing-register set (PR0..PR7) is live.
    pub fn scb(&self) -> u16 {
        self.psw_other & 0x07
    }

    /// Base address of the local register bank holding r0..r7 / er0..er3.
    pub fn bank_base(&self) -> u16 {
        ((self.lrb >> 5) << 8) | ((self.lrb & 0x1F) << 3)
    }

    /// Resolve an `off N8` operand: the LRB page with N8 as the offset.
    pub fn off_page(&self, n8: u8) -> u16 {
        ((self.lrb >> 5) << 8) | n8 as u16
    }

    pub fn update_zf_u8(&mut self, val: u8) {
        self.zf = val == 0;
    }

    pub fn update_zf_u16(&mut self, val: u16) {
        self.zf = val == 0;
    }
}

#[cfg(test)]
mod tests {
    use super::Cpu;

    #[test]
    fn psw_uses_the_documented_flag_bits() {
        let mut cpu = Cpu::new();
        cpu.cf = true;
        cpu.zf = true;
        cpu.hc = true;
        cpu.dd = true;

        assert_eq!(cpu.psw_u16() & 0xF000, 0xF000);

        cpu.set_psw_u16(0xA000);
        assert!(cpu.cf);
        assert!(!cpu.zf);
        assert!(cpu.hc);
        assert!(!cpu.dd);
    }

    #[test]
    fn psw_preserves_user_and_priority_flags() {
        let mut cpu = Cpu::new();
        cpu.set_psw_u16(0x0337);

        assert_eq!(cpu.psw_u16(), 0x0FFF);
    }

    #[test]
    fn reset_psw_starts_in_documented_byte_mode() {
        let cpu = Cpu::new();

        assert_eq!(cpu.psw_u16(), 0x0CC8);
        assert!(!cpu.dd);
    }

    #[test]
    fn mie_is_psw_bit_eight() {
        let mut cpu = Cpu::new();
        assert!(!cpu.mie());

        cpu.set_mie(true);
        assert_eq!(cpu.psw_u16() & Cpu::PSW_MIE_BIT, Cpu::PSW_MIE_BIT);

        cpu.set_psw_u16(cpu.psw_u16() & !Cpu::PSW_MIE_BIT);
        assert!(!cpu.mie());
    }
}
