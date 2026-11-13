#!/usr/bin/env python3
"""
Generates 100% full-coverage OKI 66207 Rust Opcode Table & Decoder
from authoritative 66207.op table (2,623 instructions).
"""

import re
import sys

records = []

def process(instr, dd, cond, opc):
    for pat, rng in ((r"erN'", 4), (r"erN", 4), (r"rN'", 8), (r"rN", 8)):
        m = re.match(r"^(.*)%s(.*)$" % pat, instr)
        if m:
            i0, i1 = m.group(1), m.group(2)
            plus = r"\+N'" if pat.endswith("'") else r"\+N"
            letter = "er" if pat.startswith("er") else "r"
            for j, o in enumerate(opc):
                mm = re.match(r"([0-9A-F]{2})%s($|[^'])" % plus, o + " ")
                if mm and (o.endswith("+N'") == pat.endswith("'")) and (o[-2:] == "+N" or o[-3:] == "+N'"):
                    base = int(mm.group(1), 16)
                    for i in range(rng):
                        nopc = list(opc); nopc[j] = "%02X" % (base + i)
                        process(f"{i0}{letter}{i}{i1}", dd, cond, nopc)
                    return
    m = re.match(r"^(.*)\.n(.*)$", instr)
    if m:
        i0, i1 = m.group(1), m.group(2)
        for j, o in enumerate(opc):
            mm = re.match(r"([0-9A-F]{2})\+n$", o)
            if mm:
                base = int(mm.group(1), 16)
                for i in range(8):
                    nopc = list(opc); nopc[j] = "%02X" % (base + i)
                    process(f"{i0}.{i}{i1}", dd, cond, nopc)
                return
    records.append((instr.rstrip(), dd, cond, opc))

op_path = "../ghidra-oki66207/src/66207.op"
for ln in open(op_path).read().splitlines():
    if ln.startswith(";"): continue
    m = re.match(r"^([^-]*)-(.)(.) (.*)$", ln)
    if not m: continue
    instr, dd, cond, ops = m.groups()
    process(instr.rstrip(), dd, cond, [x.strip() for x in ops.split(",")])

print(f"Loaded {len(records)} opcode records.")


def correct(records):
    """Corrections to 66207.op.

    E7,N8 -- 66207.op has it as `ORB A, off N8` in either DD state and gives
    the word form only the long encoding B4,N8,E2. The eight accumulator
    operations share one layout (x6 = immediate, x7 = off N8: 87 ADD, 97 ADC,
    A7 SUB, B7 SBC, C7 CMP, D7 AND, E7 OR, F7 XOR), each a word operation with
    DD set and a byte operation with DD clear. The instruction manual's
    `OR A, obj` page (3-103) prints the long code but the two-byte form's
    count of 4 cycles, and the stock P28 ROM uses E7 in word mode at 0x5FDD
    to OR its two 16-bit fault words. So E7 is `OR A, off N8` when DD is set.
    The long form stays: the manual lists it.
    """
    out = []
    for instr, dd, cond, opc in records:
        if instr == "ORB A, off N8" and opc == ["E7", "N8"]:
            assert dd == "U", "66207.op changed; review this correction"
            out.append(("OR A, off N8", "1", cond, opc))
            out.append((instr, "0", cond, opc))
        else:
            out.append((instr, dd, cond, opc))
    return out


spec_count = len(records)
records = correct(records)
print(f"{len(records)} records after corrections.")

out_rs = "src/full_decoder.rs"
with open(out_rs, "w") as f:
    f.write("// 100% Full-Coverage OKI 66207 Opcode Table & Decoder\n")
    f.write(f"// Generated automatically from 66207.op ({spec_count:,} instructions)\n")
    f.write("// plus the corrections in tools/gen_full_decoder.py\n\n")
    f.write("use crate::cpu::Cpu;\n")
    f.write("use crate::bus::Bus;\n")
    f.write("use crate::interpreter::DisasmInstruction;\n\n")
    f.write("#[derive(Debug, Clone)]\n")
    f.write("pub struct OpcodePattern {\n")
    f.write("    pub mnemonic: &'static str,\n")
    f.write("    pub dd_mode: char,\n")
    f.write("    pub bytes_pat: &'static [&'static str],\n")
    f.write("}\n\n")
    f.write(f"pub const FULL_OPCODE_COUNT: usize = {len(records)};\n\n")
    f.write("pub const FULL_OPCODES: &[OpcodePattern] = &[\n")
    for instr, dd, cond, opc in records:
        pat_str = ", ".join(f'"{o}"' for o in opc)
        esc_instr = instr.replace('"', '\\"')
        f.write(f'    OpcodePattern {{ mnemonic: "{esc_instr}", dd_mode: \'{dd}\', bytes_pat: &[{pat_str}] }},\n')
    f.write("];\n")

print(f"Generated {out_rs} successfully!")
