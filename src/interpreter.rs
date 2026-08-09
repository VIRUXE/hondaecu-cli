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
