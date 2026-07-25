// OKI MSM66207 Instruction Decoder and Interpreter
// Executes raw 66207 machine code instructions line-by-line

use crate::cpu::Cpu;
use crate::bus::Bus;

#[derive(Debug, Clone)]
pub struct DisasmInstruction {
    pub pc: u16,
    pub bytes: Vec<u8>,
    pub mnemonic: String,
}

pub struct Interpreter;

impl Interpreter {
    /// Step one single instruction from CPU PC address
    pub fn step(cpu: &mut Cpu, bus: &mut Bus) -> DisasmInstruction {
        let start_pc = cpu.pc;
        let b0 = bus.read_code_u8(cpu.pc);
        cpu.pc = cpu.pc.wrapping_add(1);

        let mut bytes = vec![b0];

        let mnemonic = match b0 {
            // NOP
            0x00 => "NOP".to_string(),

            // RETI: Return from Interrupt (pop PSW, then pop PC)
            0x01 => {
                let psw = Self::pop_system_u16(cpu, bus);
                cpu.set_psw_u16(psw);
                cpu.pc = Self::pop_system_u16(cpu, bus);
                "RETI".to_string()
            }

            // RET: Return from Subroutine (pop PC)
            0x02 => {
                cpu.pc = Self::pop_system_u16(cpu, bus);
                "RET".to_string()
            }

            // DI: Disable Interrupts
            0x03 => {
                cpu.ie = false;
                "DI".to_string()
            }

            // EI: Enable Interrupts
            0x04 => {
                cpu.ie = true;
                "EI".to_string()
            }

            // ADD A, erN (0x08..0x0B)
            0x08..=0x0B => {
                let r = b0 - 0x08;
                let val = Self::read_er(cpu, bus, r);
                let (res, carry) = cpu.a.overflowing_add(val);
                cpu.a = res;
                cpu.zf = cpu.a == 0;
                cpu.cf = carry;
                format!("ADD A, er{}", r)
            }

            // SUB A, erN (0x0C..0x0F)
            0x0C..=0x0F => {
                let r = b0 - 0x0C;
                let val = Self::read_er(cpu, bus, r);
                let (res, borrow) = cpu.a.overflowing_sub(val);
                cpu.a = res;
                cpu.zf = cpu.a == 0;
                cpu.cf = borrow; // CF=1 on borrow!
                format!("SUB A, er{}", r)
            }

            // AND A, erN (0x10..0x13)
            0x10..=0x13 => {
                let r = b0 - 0x10;
                let val = Self::read_er(cpu, bus, r);
                cpu.a &= val;
                cpu.zf = cpu.a == 0;
                format!("AND A, er{}", r)
            }

            // OR A, erN (0x14..0x17)
            0x14..=0x17 => {
                let r = b0 - 0x14;
                let val = Self::read_er(cpu, bus, r);
                cpu.a |= val;
                cpu.zf = cpu.a == 0;
                format!("OR A, er{}", r)
            }

            // ADC A, erN (0x18..0x1B)
            0x18..=0x1B => {
                let r = b0 - 0x18;
                let val = Self::read_er(cpu, bus, r);
                let c = if cpu.cf { 1 } else { 0 };
                let (res1, c1) = cpu.a.overflowing_add(val);
                let (res2, c2) = res1.overflowing_add(c);
                cpu.a = res2;
                cpu.zf = cpu.a == 0;
                cpu.cf = c1 || c2;
                format!("ADC A, er{}", r)
            }

            // SBC A, erN (0x1C..0x1F)
            0x1C..=0x1F => {
                let r = b0 - 0x1C;
                let val = Self::read_er(cpu, bus, r);
                let b = if cpu.cf { 1 } else { 0 };
                let (res1, b1) = cpu.a.overflowing_sub(val);
                let (res2, b2) = res1.overflowing_sub(b);
                cpu.a = res2;
                cpu.zf = cpu.a == 0;
                cpu.cf = b1 || b2;
                format!("SBC A, er{}", r)
            }

            // VCAL N (0x28..0x3B): Vector Call via vector table at 0x0028 + 2*N
            0x28..=0x3B => {
                let vec_idx = b0 - 0x28;
                let vec_addr = 0x0028 + (vec_idx as u16 * 2);
                let target = bus.read_code_u16(vec_addr);
                Self::push_system_u16(cpu, bus, cpu.pc);
                cpu.pc = target;
                format!("VCAL {:#04X} -> {:#06X}", vec_idx, target)
            }

            // MUL A, erN (0x40..0x43)
            0x40..=0x43 => {
                let r = b0 - 0x40;
                let val = Self::read_er(cpu, bus, r) as u32;
                let prod = (cpu.a as u32) * val;
                cpu.a = (prod & 0xFFFF) as u16;
                Self::write_er(cpu, bus, 1, ((prod >> 16) & 0xFFFF) as u16);
                cpu.zf = prod == 0;
                format!("MUL A, er{}", r)
            }

            // DIV A, erN (0x44..0x47)
            0x44..=0x47 => {
                let r = b0 - 0x44;
                let div = Self::read_er(cpu, bus, r) as u32;
                let num = ((Self::read_er(cpu, bus, 0) as u32) << 16) | (cpu.a as u32);
                if div != 0 {
                    let quot = num / div;
                    let rem = num % div;
                    cpu.a = (quot & 0xFFFF) as u16;
                    Self::write_er(cpu, bus, 1, (rem & 0xFFFF) as u16);
                }
                format!("DIV A, er{}", r)
            }

            // INC erN / DEC erN (0x48..0x4F)
            0x48..=0x4B => {
                let r = b0 - 0x48;
                let val = Self::read_er(cpu, bus, r).wrapping_add(1);
                Self::write_er(cpu, bus, r, val);
                cpu.zf = val == 0;
                format!("INC er{}", r)
            }
            0x4C..=0x4F => {
                let r = b0 - 0x4C;
                let val = Self::read_er(cpu, bus, r).wrapping_sub(1);
                Self::write_er(cpu, bus, r, val);
                cpu.zf = val == 0;
                format!("DEC er{}", r)
            }

            // PUSHS erN / POPS erN (0x50..0x57)
            0x50..=0x53 => {
                let r = b0 - 0x50;
                let val = Self::read_er(cpu, bus, r);
                Self::push_system_u16(cpu, bus, val);
                format!("PUSHS er{}", r)
            }
            0x54..=0x57 => {
                let r = b0 - 0x54;
                let val = Self::pop_system_u16(cpu, bus);
                Self::write_er(cpu, bus, r, val);
                format!("POPS er{}", r)
            }

            // SJ offset: Short Jump Relative (0x70..0x7F)
            0x70..=0x7F => {
                let offset = (b0 & 0x0F) as i8;
                let target = (cpu.pc as i32 + offset as i32) as u16;
                cpu.pc = target;
                format!("SJ {:#06X}", target)
            }

            // 0x86: ADD A, #N16
            0x86 => {
                let n16 = Self::fetch_u16(cpu, bus, &mut bytes);
                cpu.dd = true;
                let (res, c) = cpu.a.overflowing_add(n16);
                cpu.a = res;
                cpu.zf = res == 0;
                cpu.cf = c;
                format!("ADD A, #{:#06X}", n16)
            }

            // 0x87: ADD A, off N8
            0x87 => {
                let n8 = Self::fetch_u8(cpu, bus, &mut bytes);
                let addr = Self::page_addr(cpu, n8);
                let val = bus.read_data_u16(addr);
                cpu.dd = true;
                let (res, c) = cpu.a.overflowing_add(val);
                cpu.a = res;
                cpu.zf = res == 0;
                cpu.cf = c;
                format!("ADD A, off {:#04X}", n8)
            }

            // 0x8E: L A, #N16
            0x8E => {
                let n16 = Self::fetch_u16(cpu, bus, &mut bytes);
                cpu.dd = true;
                cpu.a = n16;
                format!("L A, #{:#06X}", n16)
            }

            // 0x8F: L A, off N8
            0x8F => {
                let n8 = Self::fetch_u8(cpu, bus, &mut bytes);
                let addr = Self::page_addr(cpu, n8);
                cpu.dd = true;
                cpu.a = bus.read_data_u16(addr);
                format!("L A, off {:#04X}", n8)
            }

            // 0x90: L X1, #N16
            0x90 => {
                let n16 = Self::fetch_u16(cpu, bus, &mut bytes);
                cpu.x1 = n16;
                format!("L X1, #{:#06X}", n16)
            }

            // 0x91: L X2, #N16
            0x91 => {
                let n16 = Self::fetch_u16(cpu, bus, &mut bytes);
                cpu.x2 = n16;
                format!("L X2, #{:#06X}", n16)
            }

            // 0x92: L DP, #N16
            0x92 => {
                let n16 = Self::fetch_u16(cpu, bus, &mut bytes);
                cpu.dp = n16;
                format!("L DP, #{:#06X}", n16)
            }

            // 0x96: LB A, #N8
            0x96 => {
                let n8 = Self::fetch_u8(cpu, bus, &mut bytes);
                cpu.dd = false;
                cpu.set_al(n8);
                format!("LB A, #{:#04X}", n8)
            }

            // 0x97: LB A, off N8
            0x97 => {
                let n8 = Self::fetch_u8(cpu, bus, &mut bytes);
                let addr = Self::page_addr(cpu, n8);
                cpu.dd = false;
                let val = bus.read_data_u8(addr);
                cpu.set_al(val);
                format!("LB A, off {:#04X}", n8)
            }

            // 0x9E: LB A, N8
            0x9E => {
                let n8 = Self::fetch_u8(cpu, bus, &mut bytes);
                cpu.dd = false;
                let val = bus.read_data_u8(n8 as u16);
                cpu.set_al(val);
                format!("LB A, {:#04X}", n8)
            }

            // 0x9F: STB A, off N8
            0x9F => {
                let n8 = Self::fetch_u8(cpu, bus, &mut bytes);
                let addr = Self::page_addr(cpu, n8);
                bus.write_data_u8(addr, cpu.al());
                format!("STB A, off {:#04X}", n8)
            }

            // 0xA0: L SSP, #N16
            0xA0 => {
                let n16 = Self::fetch_u16(cpu, bus, &mut bytes);
                cpu.ssp = n16;
                format!("L SSP, #{:#06X}", n16)
            }

            // 0xA1: L USP, #N16
            0xA1 => {
                let n16 = Self::fetch_u16(cpu, bus, &mut bytes);
                cpu.usp = n16;
                format!("L USP, #{:#06X}", n16)
            }

            // 0xA4: L LRB, #N8
            0xA4 => {
                let n8 = Self::fetch_u8(cpu, bus, &mut bytes);
                cpu.lrb = n8 & 0x07;
                format!("L LRB, #{:#04X}", n8)
            }

            // 0xA8: J N16 (Absolute Jump 16-bit)
            0xA8 => {
                let n16 = Self::fetch_u16(cpu, bus, &mut bytes);
                cpu.pc = n16;
                format!("J {:#06X}", n16)
            }

            // 0xA9: CAL N16 (Absolute Call 16-bit)
            0xA9 => {
                let target = Self::fetch_u16(cpu, bus, &mut bytes);
                Self::push_system_u16(cpu, bus, cpu.pc);
                cpu.pc = target;
                format!("CAL {:#06X}", target)
            }

            // 0xAA: J [DP] (Indirect Jump)
            0xAA => {
                let target = bus.read_code_u16(cpu.dp);
                cpu.pc = target;
                format!("J [DP] -> {:#06X}", target)
            }

            // 0xAB: CAL [DP] (Indirect Call)
            0xAB => {
                let target = bus.read_code_u16(cpu.dp);
                Self::push_system_u16(cpu, bus, cpu.pc);
                cpu.pc = target;
                format!("CAL [DP] -> {:#06X}", target)
            }

            // Indexed & Indirect Loads 0xB0..0xB3
            0xB0 => { // L A, N16[X1]
                let n16 = Self::fetch_u16(cpu, bus, &mut bytes);
                let addr = n16.wrapping_add(cpu.x1);
                cpu.a = bus.read_data_u16(addr);
                cpu.dd = true;
                format!("L A, {:#06X}[X1]", n16)
            }
            0xB1 => { // L A, N16[X2]
                let n16 = Self::fetch_u16(cpu, bus, &mut bytes);
                let addr = n16.wrapping_add(cpu.x2);
                cpu.a = bus.read_data_u16(addr);
                cpu.dd = true;
                format!("L A, {:#06X}[X2]", n16)
            }
            0xB2 => { // L A, [DP]
                cpu.a = bus.read_data_u16(cpu.dp);
                cpu.dd = true;
                "L A, [DP]".to_string()
            }
            0xB3 => { // L A, S8[USP]
                let s8 = Self::fetch_i8(cpu, bus, &mut bytes);
                let addr = (cpu.usp as i32 + s8 as i32) as u16;
                cpu.a = bus.read_data_u16(addr);
                cpu.dd = true;
                format!("L A, {}[USP]", s8)
            }

            // Conditional Branches 0xC0..0xC7
            0xC0 => { // JZ / JEQ
                let offset = Self::fetch_i8(cpu, bus, &mut bytes);
                let target = (cpu.pc as i32 + offset as i32) as u16;
                if cpu.zf { cpu.pc = target; }
                format!("JZ {:#06X}", target)
            }
            0xC1 => { // JNZ / JNE
                let offset = Self::fetch_i8(cpu, bus, &mut bytes);
                let target = (cpu.pc as i32 + offset as i32) as u16;
                if !cpu.zf { cpu.pc = target; }
                format!("JNZ {:#06X}", target)
            }
            0xC2 => { // JC / JLT
                let offset = Self::fetch_i8(cpu, bus, &mut bytes);
                let target = (cpu.pc as i32 + offset as i32) as u16;
                if cpu.cf { cpu.pc = target; }
                format!("JC {:#06X}", target)
            }
            0xC3 => { // JNC / JGE
                let offset = Self::fetch_i8(cpu, bus, &mut bytes);
                let target = (cpu.pc as i32 + offset as i32) as u16;
                if !cpu.cf { cpu.pc = target; }
                format!("JNC {:#06X}", target)
            }
            0xC4 => { // JGT (ZF==0 && CF==0)
                let offset = Self::fetch_i8(cpu, bus, &mut bytes);
                let target = (cpu.pc as i32 + offset as i32) as u16;
                if !cpu.zf && !cpu.cf { cpu.pc = target; }
                format!("JGT {:#06X}", target)
            }
            0xC5 => { // JLE (ZF==1 || CF==1)
                let offset = Self::fetch_i8(cpu, bus, &mut bytes);
                let target = (cpu.pc as i32 + offset as i32) as u16;
                if cpu.zf || cpu.cf { cpu.pc = target; }
                format!("JLE {:#06X}", target)
            }

            // 0xC8..0xCF: JRNZ rel (Decrements DPL, branches if DPL != 0)
            0xC8..=0xCF => {
                let offset = (b0 as i16) - 0xC8i16;
                let new_dpl = cpu.dpl().wrapping_sub(1);
                cpu.set_dpl(new_dpl);
                let target = (cpu.pc as i32 + offset as i32) as u16;
                if new_dpl != 0 {
                    cpu.pc = target;
                }
                format!("JRNZ DPL, {:#06X}", target)
            }

            // 0xD8..0xDF: JBR off N8.n, rel8 (Jump if Bit n of Page Byte N8 is 0)
            0xD8..=0xDF => {
                let bit = b0 - 0xD8;
                let n8 = Self::fetch_u8(cpu, bus, &mut bytes);
                let offset = Self::fetch_i8(cpu, bus, &mut bytes);
                let addr = Self::page_addr(cpu, n8);
                let val = bus.read_data_u8(addr);
                let target = (cpu.pc as i32 + offset as i32) as u16;
                if (val & (1 << bit)) == 0 {
                    cpu.pc = target;
                }
                format!("JBR off {:#04X}.{}, {:#06X}", n8, bit, target)
            }

            // 0xE8..0xEF: JBS off N8.n, rel8 (Jump if Bit n of Page Byte N8 is 1)
            0xE8..=0xEF => {
                let bit = b0 - 0xE8;
                let n8 = Self::fetch_u8(cpu, bus, &mut bytes);
                let offset = Self::fetch_i8(cpu, bus, &mut bytes);
                let addr = Self::page_addr(cpu, n8);
                let val = bus.read_data_u8(addr);
                let target = (cpu.pc as i32 + offset as i32) as u16;
                if (val & (1 << bit)) != 0 {
                    cpu.pc = target;
                }
                format!("JBS off {:#04X}.{}, {:#06X}", n8, bit, target)
            }

            // ROM Table Lookup Instructions 0xE0..0xE7
            0xE0 => { // LC A, N16[X1]
                let n16 = Self::fetch_u16(cpu, bus, &mut bytes);
                let addr = n16.wrapping_add(cpu.x1);
                cpu.a = bus.read_code_u16(addr);
                cpu.dd = true;
                format!("LC A, {:#06X}[X1]", n16)
            }
            0xE1 => { // LC A, N16[X2]
                let n16 = Self::fetch_u16(cpu, bus, &mut bytes);
                let addr = n16.wrapping_add(cpu.x2);
                cpu.a = bus.read_code_u16(addr);
                cpu.dd = true;
                format!("LC A, {:#06X}[X2]", n16)
            }
            0xE2 => { // LC A, [DP]
                cpu.a = bus.read_code_u16(cpu.dp);
                cpu.dd = true;
                "LC A, [DP]".to_string()
            }
            0xE7 => { // LCB A, N16[X1]
                let n16 = Self::fetch_u16(cpu, bus, &mut bytes);
                let addr = n16.wrapping_add(cpu.x1);
                let b = bus.read_code_u8(addr);
                cpu.set_al(b);
                cpu.dd = false;
                format!("LCB A, {:#06X}[X1]", n16)
            }

            // 0xF8: EXTND (Sign extend AL to A)
            0xF8 => {
                let sign = (cpu.al() as i8) as i16;
                cpu.a = sign as u16;
                cpu.zf = cpu.a == 0;
                "EXTND".to_string()
            }

            // Default / Generic fallback decoder
            _ => {
                format!("DB {:#04X}", b0)
            }
        };

        cpu.cycles += 2;
        cpu.instructions += 1;

        DisasmInstruction {
            pc: start_pc,
            bytes,
            mnemonic,
        }
    }

    fn fetch_u8(cpu: &mut Cpu, bus: &mut Bus, bytes: &mut Vec<u8>) -> u8 {
        let b = bus.read_code_u8(cpu.pc);
        bytes.push(b);
        cpu.pc = cpu.pc.wrapping_add(1);
        b
    }

    fn fetch_i8(cpu: &mut Cpu, bus: &mut Bus, bytes: &mut Vec<u8>) -> i8 {
        Self::fetch_u8(cpu, bus, bytes) as i8
    }

    fn fetch_u16(cpu: &mut Cpu, bus: &mut Bus, bytes: &mut Vec<u8>) -> u16 {
        let l = Self::fetch_u8(cpu, bus, bytes) as u16;
        let h = Self::fetch_u8(cpu, bus, bytes) as u16;
        l | (h << 8)
    }

    fn page_addr(cpu: &Cpu, offset: u8) -> u16 {
        let page = ((cpu.lrb as u16 & 0x07) << 5) << 8;
        page | (offset as u16)
    }

    fn push_system_u16(cpu: &mut Cpu, bus: &mut Bus, val: u16) {
        cpu.ssp = cpu.ssp.wrapping_sub(2);
        bus.write_data_u16(cpu.ssp, val);
    }

    fn pop_system_u16(cpu: &mut Cpu, bus: &mut Bus) -> u16 {
        let val = bus.read_data_u16(cpu.ssp);
        cpu.ssp = cpu.ssp.wrapping_add(2);
        val
    }

    fn read_er(cpu: &Cpu, bus: &mut Bus, r: u8) -> u16 {
        let addr = 0x0080 + ((cpu.lrb as u16) * 16) + (r as u16 * 2);
        bus.read_data_u16(addr)
    }

    fn write_er(cpu: &Cpu, bus: &mut Bus, r: u8, val: u16) {
        let addr = 0x0080 + ((cpu.lrb as u16) * 16) + (r as u16 * 2);
        bus.write_data_u16(addr, val);
    }
}
