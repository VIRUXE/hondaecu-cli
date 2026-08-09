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

**Working.** Every CLI command reaches the table-driven core through the small
`interpreter.rs` compatibility adapter. The ROM clears its CPU, PWM/Port-4,
ADC-completion and timer-integrity checks with no undefined opcodes, and the
long boot trace executes 60 million instructions without a BRK. Peripheral time
now advances by the datasheet's real machine-cycle counts per instruction
(`decoder.rs::int_cycles`, from the chapter-3 INT table), the machine-cycle
clock is pinned at 5 MHz (`bus::CPU_HZ`, = CLK/2 derived from the 400 ns /
NOP=2-cycle datasheet minimum), all four timer overflow interrupts are
delivered, and the CYP cylinder-1 marker is delivered as the polled
transition-detector bit `SFR_TRNS (0x46).0`. Driven by a crank-angle
CKP/CYP/TDC generator, the OEM P28 ROM computes a real per-injection pulse
width (~5–6 ms at 5500 rpm), measured from its active-low P2.0–P2.3 driver
transitions.

**Known gaps.**

1. The replay harness does not model the P28 board's external analog
   multiplexer. It runs the ROM's power-on checks with neutral ADC inputs, then
   applies scenario sensor values. (Feeding direct TPS/MAP into the mux
   self-test otherwise trips boot fault 0x49.)
2. Board-level VTEC and IACV pin mappings are not asserted from MCU
   documentation alone; injector duration is measured from P2.0–P2.3 rather than
   inferred from PWMR. `bus.rs` annotates P2.0 as **both** injector 0 and the
   fuel-pump relay driver — an unresolved port-mapping conflict.
3. **Sustained running injection is not achieved.** At a steady 5500 rpm the ROM
   fires all four injectors simultaneously only ~1×/second (a batch prime)
   instead of the tens-to-hundreds/second a running engine needs. Traced
   exhaustively (see the `injection-sustain-investigation` session memory and
   task #5):
   - Timing, RPM capture (TM2 software input-capture, Δ≈3413 counts/TDC) and the
     per-TDC pulse computation (TDC ISR `0x05D0` → page-relative pulse word,
     ~0x0BEA) are all correct. The pulse is computed 177×/s; it is *not* stuck.
   - Injector-ON is driven only from the main loop (`0x4BCC`), which completes
     ~1–2 passes/second because each pass runs ~424k instructions across a
     background ROM checksum (`0x2839`; `JRNZ DP` with DP=0 → up to 65536 iters)
     and fuel/ignition map interpolation (`0x4EC0` axis search + `0x4E77` lerp).
   - Ruled out: the artificial stimulus (a realistic crank→catch→run RPM ramp
     does not help), ISR starvation (~50/50 main/ISR with correct
     interrupt-depth attribution), a spin/hang (a `0x2459` max-count delay loop
     was only a side effect of feeding rpm=0), and resets/boot re-runs (0 hits
     on the reset vector / self-test entry at steady rpm; not halted).
   - **Datasheet validation (the peripheral models are essentially correct, so
     they are *not* the cause).** Against `MSM66201/66P201/66207/66P207` (SFR
     map) and the searchable family user manuals: the timer/ADC SFR addresses
     match exactly; the timer clock-select field lives in the **high** control
     bits (CK2/1/0 = bits 7/6/5), so the emulator's bits-5–6 prescaler is the
     right region for this chip generation (the ML66517's bits-0–2 layout is a
     newer generation and does *not* apply). Two real but non-causal gaps: the
     family's **auto-reload** timer mode (overflow → TM←TMR + IRQ) is not
     modeled — but it governs periodic timer interrupts, not the main-loop
     computation; and A/D conversion is 64 *cycles* here vs the datasheet's
     64 µs — which makes the ROM *faster*, not slower. The exact TM0–3 mode-select
     bits are on pages 132–146, which are the pages **missing** from the only
     66207 user manual.
   - **Reframed conclusion.** The emulator is **not** grossly inflating work: the
     fuel/ignition interpolation runs at a healthy ~0.5–2 kHz (≈1900
     axis-searches/s, ~5 iterations each). The earlier "424k instructions/pass"
     measured a slow *outer* loop; the inner computation rate is normal. So
     injection firing ~1×/s is a **gating/logic condition in the ROM's injection
     state machine** (the arm at `0x4B35`/`0x4BCC` and its page-relative state
     bytes 0xBF/0xB6/…), not a performance or peripheral bug. That gate — read
     with LRB-page-aware effective addresses — is the next thing to trace.

> **Addressing caveat that invalidated a whole prior localization:** the `off
> N8` operand is LRB-page-relative — `off_page(n) = ((LRB>>5)<<8) | n` in
> `cpu.rs` — **not** absolute. Reading `bus.ram[N8]` directly for an
> `off`-addressed ROM variable inspects the wrong byte. An earlier "pulse word
> 0xC6 stuck at 0x8000 / countdown 0xC0–0xC7 decrements too slowly" conclusion
> was an artifact of exactly this; the real pulse lives at page-relative `0x1C6`
> and is computed correctly. Instrument via effective addresses (or by hooking
> the interpreter's `ea()`), never via absolute RAM.

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
