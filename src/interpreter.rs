// OKI MSM66207 instruction front end used by the CLI.
//
// Decoding and execution live in the table-driven `decoder` and `exec`
// modules. Keeping this small adapter preserves the CLI's existing
// DisasmInstruction API while ensuring every command runs the same core that
// is checked against dasm662 and the OEM ROM's power-on self-test.

use crate::bus::Bus;
use crate::cpu::Cpu;
use crate::{decoder, exec};

#[derive(Debug, Clone)]
pub struct DisasmInstruction {
    pub pc: u16,
    pub bytes: Vec<u8>,
    pub mnemonic: String,
}

pub struct Interpreter;

impl Interpreter {
    /// Decode one instruction and advance linearly without executing it.
    /// Only DD changes, because it is decoder context carried by L/LB-class
    /// instructions; registers, memory, ports and branch targets are untouched.
    pub fn disassemble(cpu: &mut Cpu, bus: &Bus) -> DisasmInstruction {
        let pc = cpu.pc;
        let Some(decoded) = decoder::decode(cpu.dd, |offset| {
            bus.read_code_u8(pc.wrapping_add(offset as u16))
        }) else {
            cpu.pc = cpu.pc.wrapping_add(1);
            let byte = bus.read_code_u8(pc);
            return DisasmInstruction {
                pc,
                bytes: vec![byte],
                mnemonic: format!("DB 0{byte:02X}h"),
            };
        };

        let bytes = (0..decoded.len)
            .map(|offset| bus.read_code_u8(pc.wrapping_add(offset as u16)))
            .collect();
        cpu.pc = pc.wrapping_add(decoded.len as u16);
        let mnemonic = decoder::format(&decoded, cpu.pc);
        if let Some(dd) = decoded.dd_after {
            cpu.dd = dd;
        }

        DisasmInstruction {
            pc,
            bytes,
            mnemonic,
        }
    }

    /// Decode and execute one instruction at the CPU's current PC.
    ///
    /// An execution error halts the CPU and is returned as the displayed
    /// mnemonic. The surrounding CLI loops already stop on `cpu.halted`, so a
    /// bad opcode is visible instead of being treated as a one-byte no-op and
    /// corrupting all subsequent state.
    pub fn step(cpu: &mut Cpu, bus: &mut Bus) -> DisasmInstruction {
        let pc = cpu.pc;

        match exec::step(cpu, bus) {
            Ok(decoded) => {
                let bytes = (0..decoded.len)
                    .map(|offset| bus.read_code_u8(pc.wrapping_add(offset as u16)))
                    .collect();
                let mnemonic = decoder::format(&decoded, pc.wrapping_add(decoded.len as u16));

                DisasmInstruction {
                    pc,
                    bytes,
                    mnemonic,
                }
            }
            Err(error) => {
                cpu.halted = true;
                DisasmInstruction {
                    pc,
                    bytes: vec![bus.read_code_u8(pc)],
                    mnemonic: format!("ERROR: {error}"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disassembly_is_linear_and_does_not_take_branches() {
        let mut bus = Bus::new();
        bus.rom[..4].copy_from_slice(&[0xCB, 0x10, 0x77, 0x01]); // SJ +16; LB A,#1
        let mut cpu = Cpu::new();

        let branch = Interpreter::disassemble(&mut cpu, &bus);

        assert!(branch.mnemonic.starts_with("SJ "));
        assert_eq!(cpu.pc, 2, "linear disassembly must not jump to 0x12");
        let next = Interpreter::disassemble(&mut cpu, &bus);
        assert!(next.mnemonic.starts_with("LB "));
        assert_eq!(cpu.pc, 4);
    }
}
