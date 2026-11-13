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

`Bus::tick_pwm` follows the MSM66207 user's manual, chapter 10 (pp. 96–109).
`PWCON0/1` share one layout (Fig. 10-2):

| bits | field |
|---|---|
| 7 | IRQ status: this PWM's overflow requested the shared interrupt |
| 6 | enable overflow requests (IRQ bit 13, shared by both PWMs) |
| 5 | output polarity: 0 high active, 1 low active |
| 4 | RUN |
| 3..2 | mode: `00` auto reload timer, `01` free-run counter, `1x` PWM |
| 1..0 | width / time-base tap: 8 bits + TBC7, 10 + TBC9, 12 + TBC11, 16 + TBC15 |

Each tap's period is exactly `2^bits` machine cycles (256, 1024, 4096, 65536;
51.2 µs to 13.1072 ms at 10 MHz).

- **PWM mode.** `PWMC` counts machine cycles in the selected width. The tap's
  falling edge loads `PWMR` into `PWMC` and sets the output F/F; the `PWMC`
  overflow resets it and raises the request. The output is active for
  `2^bits − PWMR` cycles of every `2^bits`. An overflow that coincides with the
  tap edge keeps the F/F reset, so `PWMR = 0` is a 0 % duty after one fully
  active period (Fig. 10-8) and 100 % is not reachable.
- **Auto reload timer mode.** `PWMC` counts tap periods in the selected width
  and reloads from `PWMR` on overflow (Table 10-5).
- **Free-run counter mode.** Same clock, fixed 16 bits, no reload.

Unused upper `PWMR` bits read as ones (Table 10-2).

Two things are interpretations rather than quotations: the time base counter is
taken to be zero at chip time zero, and bit 7 is set only when bit 6 let the
overflow raise a request (the manual says bit 7 "shows if an overflow interrupt
request has occurred").

What the P28 does with it, from the ROM alone:

- `0x22E9..0x22FA` programs both PWMs to PWM mode, 12 bits, low active, running
  (`PWCON0 = 3Eh`, `PWCON1 = 7Eh`), with `PWMC = PWMR = FF00h`. In 12 bits that
  is `F00h`: a 51.2 µs low pulse every 819.2 µs. Writing `FF00h` rather than
  `0F00h` matches the read-as-ones rule above.
- After the power-on check it writes `PWMR0 = PWMR1 = FFFFh` (one-count pulse)
  and clears `P4SF.3`, turning P4.3 back into a plain port bit.
- The PWM interrupt handler at `0x031E` therefore runs every 819.2 µs
  (2441 times in 2 s of emulated time). It steps a 0..99 counter and copies a
  compare result to `P4.3`: a software PWM with a 100-step, 81.92 ms period.
  Which load P4.3 drives is not established here.
- The configuration-integrity check at `0x3897` accepts `PWCON0 & 7Bh = 3Ah`
  and `PWCON1 & 7Bh = 7Ah`, i.e. it ignores exactly bit 7 (status) and bit 2
  (don't-care in PWM mode).

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

### P28 external data bus

Data addresses from `0x0480` up are not on the chip. `Bus` decodes them
separately from the 1152-byte on-chip array (SFRs plus RAM, `0x0000..0x047F`)
and no longer folds them onto it. The stock ROM uses exactly five external data
addresses, which belong to two devices:

| address | device | direction | ROM use |
|---|---|---|---|
| `4700h` | switch-input buffer | read | 0x2211 (boot), 0x2584, 0x2943, 0x3CBE |
| `0F00h` | 82C55 port A | read | 0x2097, 0x2800, 0x290D, 0x3C9B, 0x4DFF, 0x51F9 |
| `1F00h` | 82C55 port B | write, read back | 0x28E2, 0x51EF |
| `2F00h` | 82C55 port C | write, read back | 0x1039, 0x20B0, 0x28FC, 0x4E2D, 0x51F5 |
| `3F00h` | 82C55 control | write | 0x51E9 |

The 82C55 is identified from the ROM, not from the schematic: the init at
`0x51E4` writes `90h` to `3F00h`, which is that part's mode word for "mode 0,
port A input, ports B and C output", then loads `1F00h` and `2F00h` and reads
`0F00h`. The chip's A1:A0 are therefore address lines A13:A12. `ppi82c55.rs`
models mode 0 from the MSM82C55A-2 datasheet (`82C55A-2RS.pdf`): all ports
are inputs after reset, a mode write clears the output registers, and an
output port reads back its own register. Modes 1 and 2 are flagged, not
emulated.

Only those five addresses are decoded. The board certainly decodes fewer
address lines, so both devices have mirrors, but the ROM does not use them and
the decode logic has not been read off the schematic. Any other external access
returns `FFh` (a placeholder for an undriven bus), is dropped on write, and is
counted in `Bus::external_unmapped_reads` / `external_unmapped_writes`;
`replay::stock_p28_external_bus_is_fully_decoded` requires both to stay zero.

**The outputs.** P0 and P1 carry the multiplexed address/data bus on this
board, so their SFR latches drive nothing. The ROM uses them as shadow
registers for the 82C55: it changes a bit in `P0` or `P1` and then copies the
byte out (`P0` to port B, `P1` to port C). The refresh at 0x28D6 does this for
both ports, reads each back, and raises fault `4Dh` on a mismatch. The earlier
4 KiB mask aliased all four 82C55 addresses to RAM `0F00h`, so a port A read
returned whatever had last been written to port B or C.

**Signal names.** The schematic is still not available here. The names below
come from the pgmfi.org wiki archive, which is community reverse engineering:

- [P30 (Rom #203)](https://www.hondabase.com/pgmfi/wiki/library/p30): a RAM map naming bits of `210h`, `211h`, `P0`
  and `P1` with ECU connector pins. Its patch addresses coincide with this
  ROM image (the `JEQ 2867` at 0x2855, `JEQ 3C71` at 0x3C6E, the `216h.4`
  tests at 0x3D06 and 0x3D27, the `#44h` at 0x11CA, the flag at `6010h`), so
  it describes this code base.
- [82C55](https://www.hondabase.com/pgmfi/wiki/library/82c55): an electrical trace of the 82C55 pins on OBD1
  Civic/Integra boards, not specific to the P28.
- [P72](https://www.hondabase.com/pgmfi/wiki/library/p72): the same layout on a sister ECU.

Where the ROM can check a name it does: the wiki's O2-heater pair is the PA6 /
PC2 comparison at 0x2094, its VTEC solenoid feedback and pressure switch are
the bits tested at 0x3D15 and 0x3D34 behind the VTEC enable flag, and its
knock-diagnostic pins are the bit-serial pair at 0x4E1F / 0x4E23.

Five names are also exercised by running the stock ROM (`replay.rs`,
`stock_p28_*` tests), marked **tested** below:

- starter: held from reset, every injection opens all four injectors together;
- VTEC pressure switch: open with VTEC off sets the fault flag `9Ah.1`;
- A/C switch and clutch: PB0 drops about 1.3 s after the switch closes;
- MIL and service connector: PC4 is lit at reset, and with PA7 grounded it
  blinks;
- fuel pump: see below.

`Bus::p28_board_outputs()` returns the two output bytes and
`Bus::p28_outputs()` the named ones, decoded to on/off. The bus telemetry
fields `fuel_pump_active`, `mil_check_engine_light`, `ac_clutch_active`,
`radiator_fan_active` and `purge_solenoid_active` follow them.
`vtec_solenoid_active` is still the scenario-side model in `engine.rs`; the
ROM-driven state is `p28_outputs().vtec_solenoid`.

| bit | init | written at | load (source) | ROM note |
|---|---|---|---|---|
| PB0 (`P0.0`) | 1 | 0x432F | A/C clutch relay, pin A15, 0 = on (P30); **tested** | inverted into the status image at 0x4880 |
| PB1 (`P0.1`) | 1 | 0x5FC3 | purge solenoid, pin A20, 0 = on (P30) | inverted at 0x57BB |
| PB2 (`P0.2`) | 0 | 0x443B, 0x4462 | alternator control, pin A16, 1 = pulled to ground (P30) | inverted at 0x57D1 |
| PB3 (`P0.3`) | 1 | 0x43A0, 0x43D1, 0x43E7 | radiator fan, 0 = on (P30) | inverted at 0x48A5 |
| PB4 (`P0.4`) | 0 | 0x3387 | A/T lock-up solenoid A via IC15 (82C55) | not inverted at 0x489C |
| PB5 (`P0.5`) | 1 | 0x448E, 0x47E8 | intake air bypass / shift light / lock-up (82C55, P72) | inverted at 0x57C4 |
| PB6 (`P0.6`) | 1 | 0x2744 (toggle), 0x274A, 0x2752 | pull-up only (82C55) | toggled on a timed cadence gated by the fault words at `212h`/`214h`; not inverted at 0x57CB |
| PB7 (`P0.7`) | 1 | 0x427E | fuel-pump relay, 0 = on (ROM behaviour; the 82C55 page only traces the pin to the 7U016 custom IC); **tested** | inverted at 0x4887 |
| PC0 (`P1.0`) | 0 | 0x11E2, 0x1206, 0x1220 | VTEC solenoid driver IC14 (82C55, P72) | set together with `11Fh.2`, 1 = on; not inverted at 0x48A0 |
| PC1 (`P1.1`) | 0 | 0x11B9, 0x11BE | second VTEC driver IC13 (82C55, P72) | |
| PC2 (`P1.2`) | 1 | 0x102A, 0x20A1, 0x44CA | O2 heater (82C55) | inverted at 0x4869; compared with port A bit 6 at 0x2094 |
| PC3 (`P1.3`) | 0 | 0x4E23 | knock-board diagnostic (82C55) | bit-serial data out of `1CEh` (0x4E1C) |
| PC4 (`P1.4`) | 0 | 0x36CA, 0x42AE | MIL, 1 = lit (82C55); **tested** | not inverted at 0x4870 |
| PC5 (`P1.5`) | 0 | 0x36AE | pull-up only (82C55) | |
| PC6 (`P1.6`) | 1 | 0x44F3 | EGR solenoid (82C55) | inverted at 0x4875 |
| PC7 (`P1.7`) | 0 | 0x4E1F | knock-board diagnostic (82C55) | bit-serial data out of `1CEh` |

"init" is the value loaded at 0x2297 (`P0 = EBh`, `P1 = 44h`). The status
image is the block the ROM assembles at 0x4823..0x48B9 into RAM
`39Ch..3A0h`. The wiki's P30 page gives PC1, not PC0, as the VTEC solenoid
with "0 = on"; the ROM's own `11Fh.2` bookkeeping says PC0 high is VTEC on, so
that polarity is not taken from the wiki. PB6 is listed as an unconnected
pull-up; the toggle at 0x2744 was not reached in any run here, and the
service-connector blink codes come out on PC4, not PB6.

A mode write clears the 82C55 output registers, so between the mode word at
0x51E9 and the ROM's first port B write a few microseconds later every
active-low driver is on. The model reproduces that glitch.

**The fuel-pump relay is PB7, active low.** The wiki does not name it. The
ROM does, by behaviour: at 0x4272..0x427E it clears `P0.7` while the timer at
`2BCh` is running, while the starter input is set, or while `217h.4` is
clear. In the emulator PB7 is low from reset; with the engine stopped it goes
high after 2.07 s, and with the engine turning it stays low. A two-second
prime followed by "on while the engine turns" is the fuel-pump relay and
nothing else on this ECU. What is still unconfirmed is the path from the
7U016 to connector pins A7/A8, which needs the schematic.

P2.4 is not a load driver either: the ROM toggles it next to every watchdog
reload (0x5220..0x522A), so it strobes something outside the chip. The model
no longer reports it as the A/C clutch.

**The inputs.** Both input bytes are polarity-corrected and kept in RAM:

- `211h = DATA[4700h] XOR 1Ah` (0x2585, 0x2944), so bits 1, 3 and 4 are active
  low as read.
- `210h = port A XOR 38h` (0x51FA, 0x290E), so bits 3, 4 and 5 are active low
  as read.

| bit | signal (source), level as read | read by (through the RAM image unless marked raw) |
|---|---|---|
| `4700h`.0 | starter, pin B9: 1 = powered (P30); **tested** | raw at 0x2212 into `9Fh.0`; 0x3A0C, 0x3A21, 0x3BDC, 0x4278, 0x42A0, 0x483C |
| `4700h`.1 | VTEC pressure switch, pin D6: 1 = grounded (P30); **tested** | 0x0AB8, 0x3D34, 0x5EAB |
| `4700h`.2 | A/C switch, pin B5: 1 = grounded (P30); **tested** | 0x2A93, 0x2C47, 0x4306, 0x4431, 0x4838, 0x5D1E |
| `4700h`.3 | not named | raw at 0x3CC6 only |
| `4700h`.4 | brake switch, pin D2: 0 = powered (P30) | 0x0D61, 0x2946, 0x295C, 0x43F6, 0x4479, 0x47DE, 0x4830 |
| `4700h`.5 | A/T gear position, pin B7: 1 = grounded (P30) | 0x0D58, 0x2A97, 0x2B05, 0x2BA2, 0x2E81, 0x32A8, 0x3BE4, 0x440F, 0x45CE, 0x47AB, 0x482C, 0x4858, 0x59E5, 0x5CF8 |
| `4700h`.6 | not named; an A/T input (see below) | 0x32AB, 0x32D7, 0x3301, 0x3353, 0x33BE, 0x33D2, 0x4828 |
| `4700h`.7 | not named; an A/T input (see below) | 0x32AE, 0x32FE, 0x4824 |
| port A.0, .1 | resistor pairs R112/R113 (82C55) | raw at 0x4E00..0x4E05: shifted bit-serially into `1D0h` |
| port A.2 | resistor R114 (82C55) | raw at 0x2800 into `232h.6` |
| port A.3 | power-steering pressure switch, pin B8: 0 = grounded (P30, P28); the 82C55 page instead guesses an A/D-board diagnostic | 0x2A8F, 0x2D60, 0x2D73, 0x5EB4 |
| port A.4 | feedback from lock-up driver IC15 (82C55) | raw at 0x3C9C into `99h.6` |
| port A.5 | VTEC solenoid feedback, pin A4: 0 = powered (P30, 82C55) | 0x3D15, 0x3D21 |
| port A.6 | O2-heater driver feedback (82C55) | raw at 0x2098: compared with `A0h.5`, drives PC2 |
| port A.7 | service check connector, pin D4: 1 = grounded (P30); **tested**; the 82C55 page instead guesses an A/D-board diagnostic | 0x0952, 0x0B98, 0x25E3, 0x36B1, 0x3B89, 0x4281, 0x4850 |

Addresses are the bit tests as dasm662 resolved them (`off(00210h)`,
`off(00211h)`). The named masks are `P28_IN_*`, `P28_PA_*`, `P28_PB_*` and
`P28_PC_*` in `bus.rs`.

`4700h` bits 6 and 7 are read only by the routine at 0x32A8..0x3387, together
with the A/T gear-position bit and vehicle speed, and that routine's result is
PB4, the A/T lock-up solenoid. They are automatic-transmission inputs; which
ones is not established. Bit 3 is read once, raw, at 0x3CC6.

Port A bit 5 is not a free input in the model. The ROM compares the VTEC
solenoid feedback with its own command (0x3D0C..0x3D21), so
`Bus::drive_p28_feedback` holds the bit low while PC0 drives a connected
solenoid and high otherwise; `Bus::vtec_solenoid_connected = false` unplugs
it. The O2-heater (PA6) and lock-up (PA4) feedbacks are not modeled.
`Bus::set_vtec_pressure_switch_closed` drives `4700h` bit 1.

`Bus::p28_input_port` and `Bus::ppi.pins[PORT_A]` hold the two bytes as the ROM
reads them. They reset to `1Ah` and `38h`, the values the ROM stores as `00h`.
With the wiki's names that is key on, engine stopped, manual gearbox: starter
off, VTEC pressure switch closed (the ROM itself requires `211h.1 = 0` while
VTEC is off, 0x3D34), A/C off, brake released, VTEC solenoid off, service
connector open. Two things in it are not established: the unnamed bits, and
whether the power-steering switch rests open or grounded. Before this model
`4700h` read `00h`, which the ROM saw as VTEC pressure switch open and brake
pressed.

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

1. Board-output mappings beyond the injectors and the named 82C55 bits are
   incomplete (IACV, ignition, P4.0, P2.4 …), including what the PWM0 pin and
   the software PWM on P4.3 drive. The time base counter exists only as the PWM tap phase; it is
   not a readable register.
2. The injector output latch and the CKP/TDC/CYP/VSS pin assignment are derived
   from the ROM, not read off the schematic. Output-to-cylinder mapping and
   real sensor phasing are not established. External timer clocking (`111b`)
   is not modeled.
3. The P28 external devices -- the switch-input buffer at DATA[`0x4700`] and
   the 82C55 at `0x0F00..0x3F00` -- are modeled. Their signal names come from
   the pgmfi wiki and from ROM behaviour, not the schematic; three input bits
   are unnamed, only the VTEC solenoid feedback is closed-loop, and address
   mirrors are not decoded. At 800 rpm and light load the ROM sets a fault bit
   (`212h` bit 10) and lights the MIL within a second; which input it is
   unhappy with has not been traced.
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
and restored at `0x2426`. DATA[`0x4700`].0 is `Bus::p28_input_port` bit 0,
the starter signal according to the pgmfi wiki (1 while cranking), so it
resets to 0. It must not be replaced with an invented value in `0x0320`.

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
