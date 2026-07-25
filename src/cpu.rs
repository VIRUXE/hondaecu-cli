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
    pub lrb: u8,      // Local Register Bank pointer (0..7)
    
    // PSW Flags
    pub zf: bool,     // Zero Flag
    pub cf: bool,     // Carry / Borrow Flag (CF=1 on borrow for SUB/CMP!)
    pub hc: bool,     // Half Carry Flag
    pub dd: bool,     // Data Width Mode: true = 16-bit Word mode, false = 8-bit Byte mode
    pub ie: bool,     // Global Interrupt Enable Flag
    
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
            zf: false,
            cf: false,
            hc: false,
            dd: true, // Defaults to 16-bit Word mode on boot
            ie: false,
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

    pub fn psw_u16(&self) -> u16 {
        let mut psw: u16 = 0;
        if self.zf { psw |= 1 << 0; }
        if self.cf { psw |= 1 << 1; }
        if self.hc { psw |= 1 << 2; }
        if self.dd { psw |= 1 << 3; }
        if self.ie { psw |= 1 << 4; }
        psw |= (self.lrb as u16 & 0x07) << 5;
        psw
    }

    pub fn set_psw_u16(&mut self, val: u16) {
        self.zf = (val & (1 << 0)) != 0;
        self.cf = (val & (1 << 1)) != 0;
        self.hc = (val & (1 << 2)) != 0;
        self.dd = (val & (1 << 3)) != 0;
        self.ie = (val & (1 << 4)) != 0;
        self.lrb = ((val >> 5) & 0x07) as u8;
    }

    pub fn update_zf_u8(&mut self, val: u8) {
        self.zf = val == 0;
    }

    pub fn update_zf_u16(&mut self, val: u16) {
        self.zf = val == 0;
    }
}
