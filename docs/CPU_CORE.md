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

### Bit operands are always one byte

A bit operand addresses a bit of a **byte** object whatever `DD` says (manual
p. 3-77). That matters for odd addresses that alias a register half: under
`DD = 1`, `MB C, ACCH.7` (address `07h`) must read the high accumulator byte.
The executor used to read bit operands at the current data width, so the ROM's
signed add at 0x4F7F tested bit 7 of ACCL instead. `exec.rs` now forces byte
width on both the read and the read-modify-write side.

### Flag behaviour

- `L` / `LB` / `LC` / `LCB` set ZF from the loaded value (manual pp. 3-69 …
  3-76). `MOV` / `MOVB` and `ST` / `STB` affect no flags. The ROM branches
  straight off loads, e.g. `LB A, off(135h)` / `JNE` at 0x5C4F.
- `ROL` / `ROR` (and the byte forms) rotate **through carry** and affect CF
  only (pp. 3-116 … 3-123). The ROM chains them after `SLL`/`SRL` to shift
  32-bit values, e.g. the ×5/4 pulse-width scaler at 0x4FB9.
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

This reload/toggle model is sufficient to explore the stock ROM's current boot
path, but it is not a complete implementation of the manual's PWM modes. It
must not be used to infer P28 duty cycle or board-output behavior.

### Timers (user's manual ch. 9, pp. 84–93)

`TCON0..3` share one layout (Fig. 9-2):

| bits | field |
|---|---|
| 7–5 | clock: `000` ½CLK = 1 machine cycle, then 2, 4, 8, 16, 64, 256 cycles; `111` external (not modeled) |
| 4 | RUN |
| 3 | TRBUF |
| 2 | TROUT (mode D) / TCERR (mode C) |
| 1–0 | mode: `00` auto reload, `01` clock output, `10` capture register, `11` realtime output |

- **Auto reload / clock output**: on overflow `TM ← TMR`; overflow interrupt
  only.
- **Capture register**: `TM` free-runs; a falling edge on `TMnIO` loads
  `TMR ← TM` and requests the event interrupt. TCERR reports a full counter
  lap since the previous event. `Bus::timer_capture_edge` is the pin.
- **Realtime output**: `TM` free-runs; when `TM == TMR`, `TROUT ← TRBUF`, the
  level appears on `TMnIO`, and the event interrupt is requested.

Every mode raises the overflow interrupt on the even IRQ bit below the event
bit. The stock P28 runs TM0/TM2/TM3 at 3.2 µs and TM1 at 800 ns; TM2 is in
capture mode with P3.6 an input, the others in realtime output mode. An
earlier revision decoded only bits 6–5 as the clock and ran every timer twice
too fast with a compare event in all modes.

### Injector output stage and crank inputs

P2.0–P2.3 hold the next injector state, clocked to the outputs by a rising
edge of timer 0's TROUT (`TM0IO`, P3.4). CKP is the timer-2 capture input, TDC
is INT1, CYP is TRNS0, and INT0 carries the speed signal. These are derived
from the ROM; see [`INJECTION.md`](INJECTION.md) for the evidence.

### P28 external analog mux

The P28 board places two CD4051 analog muxes in front of the first two ADC
inputs. Bomarc schematic 37820-P28-A51 sheet 3 gives the wiring:

| board path | MCU input | shared select |
|---|---|---|
| U6 common | P5.0 / AI0 / ADCR0 | P2.5=A, P2.6=B, P2.7=C |
| U5 common | P5.1 / AI1 / ADCR1 | P2.5=A, P2.6=B, P2.7=C |

Confirmed endpoints are U6/X0 primary HEGO, U6/X3 barometric pressure,
U5/X0 ELD, U5/X2 ECT, and U5/X3 a boot-check input whose external meaning is
unresolved. The direct inputs confirmed so far are AI3 optional EGR, AI6 MAP,
and AI7 TPS. The scenario model still has an IAT field, but its P28 ADC/mux
endpoint is not yet asserted.

The stock sampler at `0x52DE..0x52F7` derives the select index from `P2 >> 5`,
stores ADCR1H at `0x03C6[index]` and ADCR0H at `0x03BE[index]`, then advances
the shared selector with `P2 += 0x20`. ADCR results stay latched while the next
selection settles. One conversion is modeled as the documented 64 us: 320
machine cycles at 5 MHz, equal to 640 clocks of the 10 MHz oscillator.

The boot check at `0x530C` reads U5/X3 (`0x03C9`) and faults with marker `0x49`
when it is outside the accepted range. It is not ELD, and no sensor-specific
meaning is assigned to it merely to make boot pass.

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
| `exec::oem_rom_passes_its_cpu_self_test` | when the external stock ROM is present, observes it reaching the modeled Port-4 check |
| `exec::accumulator_loads_set_zero_from_the_loaded_value`, `exec::rotates_go_through_carry_and_leave_zero_alone`, `exec::bit_operands_address_one_byte_in_word_mode` | the three manual-specified semantics the injection path depends on |
| `bus::timer_clock_select_follows_the_manual`, `bus::auto_reload_mode_…`, `bus::capture_mode_…` | timer clock select and modes per ch. 9 |
| `bus::injector_state_is_clocked_out_by_the_timer_zero_realtime_output` | the injector output stage |
| `replay::stock_p28_sustains_sequential_injection` | with the stock ROM present: one opening per output per engine cycle, in rotation, at 300 / 800 / 3000 / 6500 rpm |
| `replay::stock_p28_cuts_fuel_above_its_rev_limit` | with the stock ROM present: no openings at 7600 rpm |
| `replay::stock_p28_boots_with_live_sensor_inputs_through_the_mux` | when the stock ROM is present, checks the mux sampler and absence of boot marker `0x49` |

The tracked ground-truth fixture `tests/fixtures/p28_dasm662.tsv` is
self-contained: it carries a six-byte decode window and DD state per
instruction, so that decoder test needs no ROM image. Mnemonic/length agreement
does not validate every execution semantic or cycle count.

The ROM-backed tests look for non-distributable images outside this repository
and currently return early when an image is absent. A green test run is
ROM-backed evidence only when the required image was actually present. Even
then, reaching the observed checkpoints does not certify every peripheral or
physical board connection.

### Regenerating the fixture

```bash
co -p 66207.op,v > 66207.op          # asm662/src/66207.op is an RCS archive
asm662/bin/dasm662 P28-230.bin out.asm
# listing lines carry:  ; ADDR DD LRB ? HEXBYTES
```

---

## 9. Current state and known gaps

Every execution command reaches the table-driven core through
`interpreter.rs`. Instruction timing now uses the manual's internal-memory
machine-cycle counts and taken-branch penalties, and CPU/peripheral/replay time
shares a 5 MHz machine-cycle clock. A few documented pointer-indirect
surcharges remain optimistic, so this is a substantially improved timing model,
not a claim of cycle-perfect hardware emulation.

The four timers follow the user's manual: 3-bit clock select and the auto
reload, clock output, capture register and realtime output modes. The
crank-angle generator delivers CKP as a timer-2 capture edge, TDC on INT1 and
CYP on the transition detector, and the stock ROM sustains sequential
injection through the modeled injector output stage (see
[`INJECTION.md`](INJECTION.md)). The stock-P28 mux is selected by P2.5--P2.7, conversion
completes after 64 us (320 machine cycles / 640 oscillator clocks), and ADCR
results are latched and read-only.

The CLI includes boot and runtime trace modes. `bootpc` and `bootram` expose
the raw power-on path; runtime `ramwatch`, `pcwatch`, `pccount`, `pchist`,
`snap`, `exec`, and `injcount` run with configured warm sensor inputs.
`injcount` reports openings and the last pulse width per injector output. Trace
addresses are absolute data-space addresses: an `off N8` operand resolves
through the active LRB page, not plain address `N8`.

**Known gaps.**

1. PWM remains a reload/toggle approximation, and board-output mappings other
   than the injectors are incomplete (fuel pump, MIL, VTEC, IACV …).
2. The injector output latch and the CKP/TDC/CYP/VSS pin assignment are derived
   from the ROM, not read off the schematic. Output-to-cylinder mapping and
   real sensor phasing are not established. External timer clocking (`111b`)
   is not modeled.
3. The P28 external digital input at DATA[`0x4700`] is not modeled separately;
   the 4 KiB backing array aliases it to `0x0700`. This is distinct from the
   analog mux and does not block injection.
4. Executor flag semantics have only been checked against the manual for the
   instructions the boot and injection paths exposed. A full audit against the
   summary tables (manual pp. 3-179 onward) is outstanding.
5. The IAT analog endpoint and several U5/U6 positions remain unresolved. ADC
   scan-mode details beyond the stock sampler's observed behavior are
   simplified.
6. Scenario names, DTC metadata, and synthetic evaluators are research aids,
   not firmware-level proof of a DTC, failsafe, calibration strategy, or
   physical output. TCP datalog compatibility remains experimental.

### Boot-latch correction: `0x0320` is not the source

A cold-boot trace shows the clear loop zeroing `0x0320` at `0x2403`, after
which `0x241D..0x2423` only applies `0x0320 &= 0xF0`. That RAM location is
part of later DTC/status state; it does not provide the pre-mux boot latch.

The saved bit at `0x9F.0` instead comes from external data: `0x220E` selects
`0x4700`, `0x2211` reads it, `0x2212` moves bit 0 into carry, and
`0x2213` stores carry into `0x9F.0`. The bit is preserved across RAM clear
and restored at `0x2426`. Modeling DATA[`0x4700`].0 is a separate digital
board-input task; it must not be replaced with an invented value in `0x0320`.

`0x9F.0` is not an injection enable. It selects a full-width, all-cylinder
first pulse and forces the start countdown, and the ROM consumes it on the
first running-fuel pass (0x1508). Sequential injection is armed by RAM
`0x120.2` once the start countdown at `0x135` reaches zero; see
[`INJECTION.md`](INJECTION.md) §3.

---
## 10. Sources

| source | use |
|---|---|
| **OKI MSM66201 Instruction Manual** (266 pp) | CPU architecture, PSW/SCB/DD, bit addressing, per-instruction flags & cycles |
| **OKI MSM66207 datasheet** (20 pp) | pin functions, Port 4 map, SFR list |
| **OKI MSM66207 user's manual** (incomplete scan), ch. 9 pp. 84–93 | timer control register layout, clock select, the four timer modes |
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
