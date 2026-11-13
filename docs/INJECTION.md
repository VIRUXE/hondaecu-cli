# Stock P28 Injection: How the ROM Drives It and Why the Emulator Did Not

This document records the investigation that restored sustained sequential
injection on the stock `P28-230` ROM (GitHub issue #6), what was wrong, what
the ROM actually does, and how each claim was established.

Sources are primary documents and the ROM itself. Where a statement is derived
from ROM behavior rather than from the board schematic, it says so.

---

## 1. Symptom

Before the fix, the stock ROM opened each injector output a handful of times
after key-on and then stopped:

| engine speed | injector openings in 2 s (4 outputs) | expected |
|---|---|---|
| 800 rpm | 0 | 53 |
| 3000 rpm | 0 | 200 |
| 5500 rpm | 4 | 367 |

After the fix (`trace … injcount`, 1 s window, 90 kPa, 65 % TPS, 88 °C):

| engine speed | openings per output | expected | pulse width |
|---|---|---|---|
| 300 rpm | 2–3 | 2.5 | ~12 ms |
| 800 rpm | 6–7 | 6.7 | ~7.8 ms |
| 1500 rpm | 12–13 | 12.5 | ~5.9 ms |
| 3000 rpm | 25 | 25 | ~5.9 ms |
| 5000 rpm | 42 | 41.7 | ~7.3 ms |
| 6500 rpm | 54–55 | 54.2 | ~7.0 ms |
| 7000 rpm | 58–59 | 58.3 | ~7.0 ms |
| 7500 rpm | 0 | 0 (ROM fuel cut) | — |

The outputs open in a fixed rotation (0 → 1 → 2 → 3), once per engine cycle
each.

---

## 2. Root causes

There was no single cause. Six independent defects each stopped injection on
their own; they are listed in the order the ROM reaches them.

### 2.1 `L` / `LB` / `LC` / `LCB` did not set ZF

MSM66201 Instruction Manual pp. 3-69 … 3-76 and Table 3-1 (p. 3-179): the four
accumulator loads set ZF from the loaded value. `MOV` affects no flags
(p. 3-96). The executor implemented `L` and `MOV` in one arm and set no flags
for either.

The ROM branches directly off loads everywhere. The one that mattered:

```
5C4F  LB   A, off(135h)     ; start-phase countdown
5C51  JNE  5C56             ; still counting -> 2041 decrements it
5C53  J    1FEA             ; finished
```

With a stale ZF the countdown at RAM `135h` ran straight through zero
(`01 → 00 → FF …`), so the ROM never left its start phase and never set
`120h.2`, the "sequential injection armed" flag (§3.2).

### 2.2 `ROL` / `ROR` did not rotate through carry

Manual pp. 3-116 … 3-123: all rotates include the carry, and affect CF only.
The executor rotated the operand onto itself and also rewrote ZF.

The ROM builds 32-bit shifts out of `SRL`/`ROR` pairs. Every running pulse
width passes through this ×5/4 scaler:

```
4FB9  MOV  er0, #0005h
4FBD  MUL                    ; er1:A = A * 5
4FBF  SRL  er1 ; ROR A       ; 32-bit >> 1
4FC2  SRL  er1 ; ROR A       ; 32-bit >> 1
```

A plain rotate fed bit 0 of the product into bit 15, so `06D5h × 5 / 4` came
out as `488Ah` instead of `088Ah`. Widths ran away and wrapped to zero within
six engine cycles.

### 2.3 Bit operands were word-wide in word mode

Manual p. 3-77: a bit operand addresses "the specified bit of the operand obj
(byte long)". The executor read and wrote bit operands with the current data
width, so under `DD = 1` an operand at an odd aliased address returned the
whole register.

The ROM's saturating signed add (`VCAL 5`, used to apply fuel corrections)
tests the sign with `ACCH.7`:

```
4F7F  MB   C, ACCH.7         ; address 07h
4F82  JLT  4F8C              ; negative correction
4F84  ADD  A, er3 …          ; positive: saturate high
4F8C  ADD  A, er3
4F8D  JLT  4F90
4F8F  CLR  A                 ; negative and no carry: clamp to 0
```

Reading the full accumulator tested bit 7 of **ACCL**. Any correction whose
low byte was `80h` or more was treated as negative and the pulse width clamped
to zero.

### 2.4 Timer control register was mis-decoded

The emulator took the clock select from `TCON` bits 6–5, treated every timer
as free-running with a compare event, and did not implement the mode field.
Earlier notes said the relevant manual pages were missing; they are present
(MSM66207 user's manual ch. 9, pp. 84–93, in
`../66k-assembler-docs/extracted/66207-users-manual-incomplete/`).

`TCON` per Fig. 9-2 (p. 85):

| bits | field | meaning |
|---|---|---|
| 7–5 | clock | `000` ½CLK (200 ns), `001` 400 ns, `010` 800 ns, `011` 1.6 µs, `100` 3.2 µs, `101` 12.8 µs, `110` 51.2 µs, `111` external (timers 0/1) |
| 4 | RUN | counter runs |
| 3 | TRBUF | mode D: value loaded into TROUT on a match |
| 2 | TROUT / TCERR | mode D: level on TMnIO. Mode C: counter lapped between events |
| 1–0 | mode | `00` A auto reload, `01` B clock output, `10` C capture register, `11` D realtime output |

What the P28 programs at 0x22B0 … 0x22C9:

| timer | TCON | clock | mode | TMnIO pin (P3IO = B1h) |
|---|---|---|---|---|
| TM0 | `8Bh` | 3.2 µs | D realtime output | P3.4 output |
| TM1 | `4Fh` | 800 ns | D realtime output | P3.5 output |
| TM2 | `82h` | 3.2 µs | **C capture** | **P3.6 input** |
| TM3 | `8Fh` | 3.2 µs | D realtime output | P3.7 output |

Consequences of the old model: timers 0/2/3 ran twice too fast (1.6 µs), timer
2 raised compare events it cannot have in capture mode, and no pin edge could
ever load `TMR2`.

### 2.5 The crank signal was delivered to the wrong pin

The engine model raised the 24-tooth CKP signal as `INT0`. The ROM takes it on
**TM2IO** (timer-2 capture, IRQ bit 9):

```
014D  timer-2 event handler: per-tooth bookkeeping, MAP sample (ADCR6 -> A2h)
0172  J 7674
7674  ORB  off(IRQ), #008h   ; request IRQ bit 3 by software
7678  RB   TRNSIT.0          ; CYP seen?
767D  SB   09Fh.2
7680  RTI
03DC  IRQ-3 handler: crank-angle state machine
046C  … INCB off(12Ah) / CMPB off(12Ah), #17h   ; tooth counter 0..23
```

With no timer-2 capture events the ROM saw no teeth. Its CKP-activity counter
(RAM `1A9h`, reloaded to `2Dh` by each tooth) ran down, and the supervisor at
0x37BC switched the interrupt mask from `2BABh` to `A9A3h`: `INT1` enabled,
IRQ 3 and the timer-2 event disabled. That is a TDC-only fallback. Everything
previously observed about injection — including the "injector-ON is driven
only from the main loop" conclusion — was observed in that fallback mode. It
also needs a minimum engine speed, which is why nothing happened below about
1300 rpm.

`INT0` is not a crank input. Its handler (0x01D3) measures the edge period
against TM2 with an overflow extension, which fits a slow signal; the model
now puts the vehicle-speed sensor there (assignment not schematic-verified).

### 2.6 P2.0–P2.3 are not the injector pins

The emulator measured injector pulses from writes to P2 bits 0–3. With defects
2.1–2.5 fixed that showed ~20 µs "pulses", twice per cycle above 4200 rpm.

The ROM stages the **next** injector state on P2.0–P2.3 (active low) and
clocks it into the output stage with a rising edge of timer 0's realtime
output (TM0IO, P3.4):

| action | code | mechanism |
|---|---|---|
| open now | 0x4BCC `ANDB P2, A`; 0x4BD6 `ORB TCON0, #4`; 0x4BDF `ANDB TCON0, #0FBh` | hand-made TROUT pulse |
| close at a set time | 0x4C19 / 0x4C28 … write the end time to `TMR0`; 0x4CB5 `ORB P2, A` stages the closed state | TRBUF = 1, so the compare match raises TROUT by itself and clocks the state out with no interrupt latency |
| after a close | 0x0086 handler: `ANDB TCON0, #0FBh`, advance the three-entry close queue (`1B8h`/`1BAh`/`1BCh`), stage the next state (0x00C3 / 0x0103) | |
| fuel cut | 0x4C00 `ORB P2, #0Fh`; 0x4C03 `SB TCON0.2`; 0x4C10 `RB TCON0.2` | hand-made TROUT pulse |

`Bus::latch_injector_outputs` models exactly this: on a 0 → 1 transition of
`TCON0.2` (software write or compare match) it copies P2.0–P2.3 to the
injector outputs, provided P3.4 is configured as a TM0IO output. Pulse widths
and opening counts are measured on those outputs.

This is derived from the ROM. The latch has not been identified on the Bomarc
schematic, and the mapping from output number to cylinder is not established:
the outputs open in the order 0, 1, 2, 3, which presumably follows the firing
order.

---

## 3. What the ROM does, end to end

Addresses are stock `P28-230`. RAM addresses are absolute.

### 3.1 Crank-angle inputs

| signal | rate | MCU input | ROM use |
|---|---|---|---|
| CKP | 24 / cam rev | P3.6 / TM2IO, capture | 0x014D → soft IRQ 3 → 0x03DC; tooth counter `12Ah` (0..23) |
| TDC | 4 / cam rev | P3.3 / INT1 | polled as `IRQH.7` (0x0403, 0x044A, 0x37C2); vectored (0x037B) only in fallback mode |
| CYP | 1 / cam rev | P4.4 / TRNS0 | polled as `TRNSIT.0` (0x7678, 0x03B8); re-zeroes `12Ah` |
| VSS | speed | P3.2 / INT0 | 0x01D3 period capture |

The emulator steps the distributor in half teeth: CKP on the tooth, TDC half a
tooth after teeth 0/6/12/18, CYP half a tooth after tooth 3. Those phases are
a working choice that the ROM accepts; the real reluctor phasing has not been
measured.

### 3.2 Start phase and arming

| RAM | meaning |
|---|---|
| `135h` | start-phase countdown. Initialized to 4 at 0x3984; decremented once per pass at 0x2048 while nonzero |
| `120h.2` | sequential injection armed. Set at 0x0549 / 0x768F once `135h` is 0 |
| `134h` | cylinder index 0..3 |
| `1B6h` | rotating mask: which output opens next |
| `1BEh` / `1B7h` / `1BFh` | close-queue head mask / staged output state / outputs currently open |
| `1C0h`…`1C6h` | pulse width per upcoming event, in 3.2 µs timer-0 counts |
| `3AEh`…`3B4h` | computed pulse width per cylinder |
| `0F2h` | IE shadow: `2BABh` normal crank mode, `A9A3h` TDC-only fallback |
| `217h.2` | fallback mode active |

A width below `C0h` counts (0.6 ms) is treated as no fuel: 0x4BAE branches to
the cut path at 0x4BFA.

### 3.3 The boot latch `9Fh.0` is not the injection gate

Issue #6 was opened on the theory that `9Fh.0`, loaded at boot from bit 0 of
external data `4700h`, gates injection, because forcing it produced 708
openings per second. That was the start-phase path re-firing every pass, not
normal running.

What `9Fh.0` does:

- 0x2041: when set, forces the start countdown `135h` to 4.
- 0x1407: when clear, the cranking pulse is quartered (`SRL A` twice).
- 0x1437: when set, the cranking pulse is copied to all four outputs
  unconditionally.
- 0x1508: cleared (`RB`) on the first running-fuel pass; if it had been set,
  `135h` is cleared too.

So it selects a full-width, all-cylinder first pulse and is consumed once.
Injection is sustained with it clear, which is what the emulator supplies:
`Bus::p28_input_port` bit 0 resets to 0. The pgmfi wiki names that bit the
starter signal (1 while cranking), which fits: a full-width pulse on all four
cylinders, consumed once, is a cranking prime.

`4700h` itself is a digital input port: 0x2581, 0x2940 and 0x3CBB read it,
XOR it with `1Ah` and keep the result in `211h`. It and the board's 82C55 are
described in [`CPU_CORE.md`](CPU_CORE.md), "P28 external data bus".

---

## 4. Verification

```bash
cargo test --release
./target/release/hondaecu-cli trace ../P28-230.bin injcount 3000 1
```

| test | proves |
|---|---|
| `exec::accumulator_loads_set_zero_from_the_loaded_value` | §2.1 |
| `exec::mov_does_not_modify_zero` | §2.1 (MOV unchanged) |
| `exec::rotates_go_through_carry_and_leave_zero_alone` | §2.2 |
| `exec::bit_operands_address_one_byte_in_word_mode` | §2.3 |
| `bus::timer_clock_select_follows_the_manual` | §2.4 |
| `bus::auto_reload_mode_reloads_from_the_timer_register_on_overflow` | §2.4 |
| `bus::capture_mode_latches_the_counter_on_a_pin_edge_only` | §2.4, §2.5 |
| `bus::injector_state_is_clocked_out_by_the_timer_zero_realtime_output` | §2.6 |
| `replay::stock_p28_sustains_sequential_injection` | one opening per output per engine cycle, in rotation, 1–20 ms, at 300 / 800 / 3000 / 6500 rpm, in normal crank mode |
| `replay::stock_p28_cuts_fuel_above_its_rev_limit` | no openings at 7600 rpm |

The two `replay::stock_p28_*` tests need the non-distributable ROM at
`../P28-230.bin` and return early without it.

`trace … injcount` now prints the opening count per output and the last pulse
width per output (`INJPW_US`), both measured on the latched outputs.
`trace … ramwatch` appends the machine-cycle count to each line. `replay`
reports a pulse width of 0 while the ROM is cutting fuel instead of repeating
the last width.

---

## 5. What is still not established

- The injector output latch and the CKP → TM2IO, TDC → INT1, CYP → TRNS0,
  VSS → INT0 assignments are ROM-derived, not read off the schematic.
- Output-number-to-cylinder mapping and the real CKP/TDC/CYP phasing.
- Pulse widths are produced by the ROM's own tables from modeled sensor
  voltages. They are plausible, not validated against a datalog; in particular
  the ECT/IAT/battery transfer functions in `engine.rs` are approximations.
- The signals on external data `4700h` and the 82C55 are named from the pgmfi
  wiki, not the schematic; `4700h` bits 3, 6 and 7 are unnamed.
- The executor's flag behavior was wrong in three places that the decoder
  ground-truth test cannot see. The remaining mnemonics have not been audited
  against the manual's summary tables (pp. 3-179 onward).
- The exact fuel-cut speed (between 7000 and 7500 rpm at these inputs) has not
  been compared with the calibration.
