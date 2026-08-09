# OKI MSM66207 CPU Core — Findings & Implementation Notes

This document records how the emulator's CPU core works, why the previous one
was replaced, and which facts are backed by primary sources versus inferred.

Everything here was established against primary documentation and the OEM
`P28-230` ROM. Where something is a guess, it says so.

---

## 1. Why the previous core was replaced

`src/interpreter.rs` decoded instructions with a flat `match opcode_byte`.
Measured against `dasm662` (the reference disassembler from `asm662`) over
`P28-230.bin`:

| metric | old decoder |
|---|---|
| correct mnemonic | **10.5 %** |
| correct instruction length | **29.0 %** |

That is not a set of isolated bugs — a flat opcode match cannot decode this ISA
at all (see §2).

### The failure chain

1. `0xC5` was decoded as a 2-byte `JLE rel8`. It is really the first byte of a
   4-byte `MOVB N'8, #N8`.
2. That is the **first instruction of the reset vector** (`int_start`, 0x21E2),
   so execution diverged immediately.
3. `MOV SSP, #047Eh` at 0x21ED therefore never ran, leaving SSP at its power-on
   default.
4. Stray `RET`/`RETI` bytes in the garbage instruction stream popped an empty
   stack, walking SSP *upward* through the entire 16-bit space.
5. Data-space writes mask to `addr & 0xFFF`, so the runaway stack wrote straight
   through the SFR block.

Every published number the emulator produced was stack noise. The giveaway:
`injector_pulse_width_us` values were always exactly `2 × (pc+1)` — pushed
return addresses landing on `PWMR0` (0x72). VTEC "activated" because a stack
byte hit P2 bit 2.

---

## 2. Instruction encoding: addressing mode first, operation second

The 66207 encodes the **addressing mode in the first byte** and the
**operation in a later byte**:

```
C5 EB 98 46   =  MOVB 0EBh, #046h
│  │  │  └── immediate
│  │  └───── 98 = MOV-immediate      (the operation)
│  └──────── EB = operand address
└─────────── C5 = "direct byte operand"  (the addressing mode)
```

Common first-byte prefixes:

| byte | operand | byte | operand |
|---|---|---|---|
| `20+N` | `rN` | `A2` | `PSWH` |
| `44+N` | `erN` | `A3` | `PSWL` |
| `90` / `91` | `X1` / `X2` | `A4` | `LRB` |
| `92` | `DP` | `B0` / `B1` | `N16[X1]` / `N16[X2]` |
| `A0` / `A1` | `SSP` / `USP` | `B2` | `[DP]` |
| `B3` | `S8[USP]` | `B4` / `B5` | `off N8` / `N8` |
| `C4` / `C5` | `off N8` / `N8` (byte) | | |

Because of this, patterns must be matched **whole**. That is what
`src/decoder.rs` does, against the 2623-entry `FULL_OPCODES` table.

> The correct table already existed in `src/full_decoder.rs`, generated from the
> spec — but was entirely unused (`mod full_decoder;` declared, never called,
> with `#![allow(dead_code)]` hiding it). The fix was wiring, not authoring.

### DD gates decoding

`DD` (data descriptor) selects word vs byte mode and **disambiguates encodings**:
`0x18` is `ADC A, er0` when DD=1 but `ADCB A, r0` when DD=0.

In `66207.op` the flag column `-XY` encodes this in `X`:

| `X` | meaning |
|---|---|
| `1` | word-mode only |
| `0` | byte-mode only |
| `U` | either |
| `S` | **sets** DD to 1 |
| `R` | **resets** DD to 0 |

DD is therefore decoder *context*, threaded through `decode(dd, …)`.

Short `+N` register forms cover **er0–er3** (4); byte `rN` forms cover
**r0–r7** (8).

---

## 3. Register and memory model

Several things here are counter-intuitive and were the source of real bugs.

### Registers are memory-mapped SFRs

| address | register |
|---|---|
| `0x00` | SSP |
| `0x02` | LRB |
| `0x04` | PSW |
| `0x06` | ACC (A) |

This matters: the ROM's `CLR off(PSW)` at 0x21F4 is a *write to address 0x0004*
and must clear the real PSW, not plain RAM. `exec.rs::alias()` handles this.

### X1 / X2 / DP / USP are **not** CPU registers

They live in RAM as one of eight **pointing register sets** `PR0..PR7` at
`0x0080 + SCB*8`, where `SCB` is **PSW bits 0–2**:

```
PR0 @ 0x0080   X1 +0   X2 +2   DP +4   USP +6
PR1 @ 0x0088
...
PR7 @ 0x00B8
```

Implemented in `exec.rs::preg_addr()`. (The old core's
`0x0080 + lrb*16` was a garbled memory of this.)

### LRB is 13-bit

Bits 5–12 select bank/page; bits 0–4 select the local-register base:

```rust
bank_base = ((LRB >> 5) << 8) | ((LRB & 0x1F) << 3)   // r0..r7 / er0..er3
off_N8    = ((LRB >> 5) << 8) | N8                    // `off N8` operands
```

Verified against `dasm662`, which prints LRB as `%01X%02X` of exactly these two
fields.

### PSW layout

| bits | contents |
|---|---|
| 0–2 | **SCB** — pointing-register set select |
| 3, 6, 7 | unimplemented, **read as 1** |
| 4, 5, 9 | user flags |
| 8 | **MIE** — master interrupt enable |
| 12 | **DD** — data descriptor |
| 13 | **HC** — half carry |
| 14 | **ZF** — zero |
| 15 | **CF** — carry |

The read-as-1 bits are pinned by the ROM's own self-test at 0x2265: it writes
`0x55` to PSWL and requires `0xDD` back, then writes `0xAA` and requires `0xEA`.

MIE globally gates all maskable interrupts in addition to the per-source `IE`
register. Interrupt entry saves PSW and clears MIE; `RTI` restores it. The
short hardware datasheet calls bit 8 "MIP", but the full MSM66201/207 user's
manual names it MIE and explicitly defines this enable/disable behavior.

DD is not part of PSWL. The same self-test writes `0xAA` to PSWL and then
requires the next instruction to stay in byte mode. The flag positions above
are given explicitly by the MSM66201/66P201/66207/66P207 datasheet.

---

## 4. Bit addressing has two distinct forms

| form | instructions | bit selected by |
|---|---|---|
| **Direct** | `SB` `RB` `MB` `JBS` `JBR` | encoded in the opcode (`…28+n`) |
| **Register indirect** | `SBR` `RBR` `TBR` `MBR` | **accumulator bits 0–2** |

The register-indirect forms carry *no* bit index in their encoding. The manual
(ch. 2 §3) specifies the bit location as "the bits from 0 to 2 of the
accumulator". The ROM makes this visible: `LB A, #002h` immediately before a
loop polling `MBR C, off(P4)`.

### Flag behaviour

- `SB` / `RB` / `SBR` / `RBR` / `TBR` are **test-and-modify**: ZF reports the
  bit's *previous* value. The ROM depends on this at 0x2340, where
  `RB off(IRQH).5` is followed by `JNE` to branch on whether that interrupt
  request was already pending.
- `MB` / `MBR` move a bit through carry and do **not** modify ZF.

---

## 5. System stack and interrupt frame

The system stack does not use a decrement-before-write convention:

```
push: (SSP) <- value; SSP <- SSP - 2
pop:  SSP <- SSP + 2; value <- (SSP)
```

On maskable interrupt entry, hardware saves `PC`, `A`, `LRB`, and `PSW` in
that order, then clears MIE. The `RTI` instruction restores `PSW`, `LRB`, `A`,
and `PC` in that order and advances SSP by eight bytes.

This is specified directly by the 66K instruction manual's RTI page (3-126).
It matters to normal execution: every P28 ISR selects its own LRB. Saving only
PC/PSW leaves the ISR's bank active after return, causing an `off 0258h` store
to hit SFR `0058h` (ADSCAN) and reset-loop the launch-enabled test ROM before
it can schedule an injector pulse.

---

## 6. Peripherals

### Port 4 pin map (MSM66207 datasheet)

| pin | function |
|---|---|
| P4.0 | TM0CK — timer 0 clock **input** |
| P4.1 | TM1CK — timer 1 clock **input** |
| **P4.2** | **PWM0 output** |
| **P4.3** | **PWM1 output** |
| P4.4–P4.7 | TRNS0–TRNS3 |

Only the two PWM outputs are driven from inside the chip, and only when `P4SF`
enables the special function for that bit.

### The PWM model

`PWMR` is a **reload value, not a duty compare**. The counter counts up; on
overflow it reloads from `PWMR` and flips the output pin.

Evidence: the ROM writes `PWMC0` and `PWMR0` to the *same* value (`0xFF00`),
which only makes sense for a reload. That gives a 256-count half period, which
fits the timeout budgets its own power-on check allows.

### The ROM's power-on peripheral check (0x2316–0x2343)

```
LB A, #002h              ; bit index 2 -> PWM0
2318: wait for P4.<A> LOW    (0x177 poll budget)
2323: wait for P4.<A> HIGH   (0x177)
232E: wait for P4.<A> LOW    (0x0B2)
2339: INCB ACC ; CMPB A,#4 ; JNE 2318   -> repeats for bit 3 (PWM1)
2340: RB off(IRQH).5 ; JNE 234A         -> success
2345: MOVB 0EBh,#04Ch ; BRK             -> failure marker + reset
```

Any timeout writes `0x4C` to RAM `0xEB` — the ROM's **"self-test failed"
marker** — and soft-resets. `0xEB` is the boot-mode marker generally
(`0x46` first boot, `0x47` second phase, `0x4E`, `0x4C` = failed).

Note that `BRK` at 0x5A8A is *by design*: both paths fall into it. The ROM
soft-resets between init phases on purpose.

---

## 7. Implementation layout

| file | role |
|---|---|
| `src/full_decoder.rs` | generated 2623-entry opcode table (data only) |
| `src/decoder.rs` | whole-pattern, DD-aware matcher → `Decoded` |
| `src/operand.rs` | parses operand *text* once into an `Arg` tree |
| `src/exec.rs` | evaluates the tree; each mnemonic implemented once |

Parsing operand text (`"JBS off N8.5, rel8"`) into a tree means each mnemonic is
implemented once regardless of how many addressing-mode encodings it has — 85
mnemonics cover all 2623 patterns.

Unknown instructions return a typed `ExecError` rather than silently doing
something plausible. That is deliberate, and it caught three real bugs:

- `byte_width` was `op.ends_with('B')`, so **`SUB`** parsed as base `"SU"`.
  A trailing `B` only means byte-variant if dropping it leaves a real mnemonic.
- `SWAP` / `SWAPB` take **no operand** (accumulator implied).
- `SB`/`RB` flag behaviour (§4).

---

## 8. Verification

```bash
cargo test --release
```

| test | what it proves |
|---|---|
| `decoder::matches_dasm662_ground_truth` | **10914 / 10914** instructions match `dasm662` on mnemonic *and* length |
| `decoder::decodes_reset_sequence` | pins the exact reset path that broke before |
| `operand::every_pattern_parses` | all 2623 patterns parse |
| `exec::oem_rom_passes_its_cpu_self_test` | the OEM ROM clears **its own** power-on CPU self-test and the PWM/Port-4 check |

The ground-truth fixture `tests/fixtures/p28_dasm662.tsv` is self-contained: it
carries a 6-byte decode window and the DD state per instruction, so the test
needs no ROM image.

The self-test being ROM-driven is the strongest signal available — it re-passed
unchanged after the pointing registers moved into RAM, SCB appeared, DD moved to
bit 12, and bit indexing changed.

### Regenerating the fixture

```bash
co -p 66207.op,v > 66207.op          # asm662/src/66207.op is an RCS archive
asm662/bin/dasm662 P28-230.bin out.asm
# listing lines carry:  ; ADDR DD LRB ? HEXBYTES
```

---

## 8. Current state and known gaps

**Working.** Every CLI command now reaches the table-driven core through the
small `interpreter.rs` compatibility adapter. The ROM clears its CPU,
PWM/Port-4, ADC-completion and timer-integrity checks with no undefined
opcodes. The long boot trace executes 60 million instructions without a BRK.

**Known gaps.**

1. Instruction cycle counts are still approximated; peripheral time advances
   by two clocks per executed instruction.
2. The replay harness does not model the P28 board's external analog
   multiplexer. It runs the ROM's power-on checks with neutral ADC inputs, then
   applies scenario sensor values.
3. Board-level VTEC and IACV pin mappings are not asserted from MCU
   documentation alone. Injector duration is measured from the ROM's
   active-low P2.0–P2.3 driver transitions rather than inferred from PWMR.

The datasheet gives real INT/EXT cycle counts per instruction (chapter 3) for a
future timing-accuracy pass.

---

## 9. Sources

| source | use |
|---|---|
| **OKI MSM66201 Instruction Manual** (266 pp) | CPU architecture, PSW/SCB/DD, bit addressing, per-instruction flags & cycles |
| **OKI MSM66207 datasheet** (20 pp) | pin functions, Port 4 map, SFR list |
| **P28 board schematic** — *Engine Control Computer for Honda/Acura, 37820-P28-A51*, Bomarc, 8 sheets | board wiring; sheet 6 is the MCU sheet |
| `asm662/src/66207.op` | authoritative opcode table (RCS archive — `co -p` it) |
| `asm662/bin/dasm662` | reference disassembler, ground truth for decoding |

Local copies of every attachment on the HondaBase 66k page are stored in
`../66k-assembler-docs/`, including the original archives and extracted page
images.

Original locations:

- Instruction manual and datasheets — HondaBase 66k library:
  `https://www.hondabase.com/pgmfi/wiki/library/66k-assembler-docs`
- Datasheet — `hondabase/files-archive` on GitHub,
  `ecu-components/OKI 66207 Diagram.pdf`
- Schematic — `files.hondabase.com` ("P28 Schematics.pdf")

All three are scanned PDFs: they must be read page-by-page as images, not
fetched as text.

> Two corrections worth remembering: Port 4 is **PWM**, not timer outputs (an
> earlier misreading of a blurry schematic), and the register-indirect bit
> instructions use a bit **index** in A[0:2], not a bit mask. Both were caught
> only by going to primary documentation. Check the manual before inferring
> behaviour from ROM code.
