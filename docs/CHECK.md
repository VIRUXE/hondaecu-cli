# ROM check

`hondaecu-cli check <rom.bin>` answers one question for a tuner: is this ROM
image worth burning to a chip and putting in the car? It runs the ROM on the
modelled P28 box (with a knock board on FC1 for a P30 box), wired to the car
described in a "car" setup, drives the box
through one fixed script of conditions (about 110 s of ECU time, roughly 25 s of
wall time), looks only at what an observer outside the box could see, and
prints one of three verdicts. `test` is an alias that runs exactly the same.

It does not judge fuelling, ignition timing, or anything that needs a
responding engine. Every `ok` line comes from something the executing ROM was
observed to do; no result is a constant.

## Running it

```
hondaecu-cli check <rom.bin> [--ecu p28|p30] [--board-gearbox manual|automatic]
                             [--car <preset-name|file>] [--set key=value]...
                             [--report <file>] [--no-prompt]
```

The ROM is opened and size-checked first: 32768 bytes, optionally followed by
one 00 byte; anything else is refused with exit status 2 before any question is
asked.

### Describing the car

A car is twelve values (`car.rs`, `KEYS`):

| key | answers | meaning |
|---|---|---|
| `ecu` | `p28`, `p30` | ECU model |
| `board_gearbox` | `manual`, `automatic` | gearbox the board is strapped for |
| `gearbox` | `manual`, `automatic` | gearbox in the car |
| `vtec` | `yes`, `no` | VTEC engine |
| `knock_sensor` | `yes`, `no` | knock sensor fitted |
| `o2_sensor` | `none`, `one-wire`, `heated` | oxygen sensor |
| `iacv` | `stock`, `none`, ohms | idle air control valve |
| `egr` | `yes`, `no` | EGR lift sensor |
| `eld` | `yes`, `no` | electrical load detector |
| `speed_sensor` | `yes`, `no` | vehicle speed sensor |
| `ac` | `yes`, `no` | air conditioning |
| `power_steering_switch` | `yes`, `no` | power steering pressure switch |

Four ways to supply them, applied in this order, a later one overriding an
earlier one:

1. **Preset.** `--car stock-p28-manual | stock-p28-automatic | stock-p30`. With
   no `--car`, the preset is `stock-p30` when an `ecu` of `p30` is given with
   `--ecu` or `--set`, else `stock-p28-manual`. A car file may name its own starting preset with a `preset = <name>` line
   (one of the three above, else a `line N:` error); that line, not the rule
   above, then decides the base for that file. `preset` is not one of the
   twelve values and is never listed as defaulted. Without the line, a file
   that says `ecu = p30` does not change the preset it starts from.
2. **Car file.** `--car <path>` for anything that is not a preset name. Lines
   are `key = value`; `#` starts a comment; blank lines are ignored; values may
   be quoted. A file saved by `check` starts with `preset = <name>`. A leading UTF-8 byte-order mark (Notepad writes one) is ignored.
   A bad line is refused with its number and exit status 2.
3. **Arguments.** `--set key=value`, repeatable, and the shorthands `--ecu`
   and `--board-gearbox`, in command-line order.
4. **Questions.** When standard input is a terminal and `--no-prompt` is not
   given, `check` first asks which preset to start from (unless `--car` was
   given), then asks every key not yet given, each as plain words with the
   accepted answers and the default in brackets:
   `Knock sensor fitted? (yes/no) [no]: `. Enter takes the default; so does
   the end of input. A wrong answer is asked again, three times in all, then
   takes the default. Afterwards it offers `Save these answers to a file?
   [path, or Enter to skip]` (not offered once the input has ended). The
   file keeps every key you answered as `key = value` and every key you took
   the default for as a comment, `# key = value   (preset default)`, and its
   first line is `preset = <name>`, so running it again with `--car` checks the
   same car and reports the same defaulted list.

A value the user did not supply is **defaulted**, and the report heading lists
those keys: `(defaulted: ...)`, or `(defaulted: all values, from the preset)`
when none was given. Taking a default does not make a value given.
`board_gearbox`, when not given, is set to the car's `gearbox` after all inputs
are applied; it stays in the defaulted list.

**Which values change the run today** (`wiring.rs`, `script.rs`). Every
value either changes the board model or is **unmodelled**: `wiring::apply_static`
returns it with the reason, and the verdict adds a could-not-tell line
`car as described`, first in `RUNS A BASIC SETUP`, naming each one. With that
line the verdict cannot be worth burning; a problem still outranks it. The
line is absent when every value is modelled. Key by key (evidence in "The car
values, one by one" below):

| key = value | effect on the model |
|---|---|
| `ecu = p28` | no knock board: FC1's inputs sit on their pull-ups (`knock.rs`) |
| `ecu = p30` | the knock board on FC1 (`knock.rs`; "The P30 box" below) |
| `board_gearbox` | the gearbox strap input |
| `gearbox = automatic` | the A/T position inputs (park/neutral out of gear, D4 in gear; "The automatic gearbox" below) and the lock-up solenoids on their drivers |
| `gearbox = manual` | no position switch and no lock-up solenoids: on an automatic board both lock-up feedbacks read "powered" |
| `vtec = no` | VTEC solenoid unplugged (its feedback reads "powered"), pressure switch open |
| `knock_sensor = yes` on a P30 | the board reports a healthy sensor |
| `knock_sensor = no` on a P30 | the board reports the sensor missing (frame bit 8; an assumption, see "The P30 box") |
| `knock_sensor` on a P28 | none: with no board the sensor reaches nothing the ROM reads (sheet 5) |
| `o2_sensor = heated` | heater plugged in |
| `o2_sensor = one-wire` | heater unplugged |
| `o2_sensor = none` | heater unplugged; the sensor input level is **unmodelled** |
| `iacv = stock`, `none` | valve plugged or unplugged |
| `iacv = <ohms>` | **unmodelled**: no source for the stock valve's resistance |
| `egr = no` | AI3 reads the battery divider |
| `egr = yes` | **unmodelled**: the lift sensor's level has no source |
| `eld = yes` | U5/X0 at mid scale, as before |
| `eld = no` | U5/X0 at the open-input level, 4.55 V |
| `speed_sensor = no` | no pulses on INT0 |
| `ac = no` | no A/C stage and no A/C switch |
| `power_steering_switch = yes` | port A bit 3 at its old rest level (raw 1), as before |
| `power_steering_switch = no` | **unmodelled**: the schematic and the pgmfi wiki disagree on the level |

### Exit status

| status | meaning |
|---|---|
| 0 | worth burning |
| 1 | do not burn |
| 2 | bad command line, unreadable ROM or car file, or the report/save file could not be written (the verdict is still printed) |
| 3 | could not tell, and nothing found wrong |

`--report <file>` also writes the result as JSON (`report.rs`).

## The script

`script::stages` in `script.rs`. Coolant is 88 C in every stage but
`cold idle`. Rpm is constant in a stage unless a range is given. Stages run
back to back; if execution stops, the stage it stopped in is kept, cut short,
and scripting ends.

| stage | s | rpm | MAP kPa | TPS % | km/h | switches |
|---|---|---|---|---|---|---|
| key on | 6.0 | 0 | 100 | 0 | 0 | none |
| cranking | 1.5 | 300 | 90 | 0 | 0 | starter |
| cold idle (ECT 0 C) | 3.0 | 1500 | 45 | 0 | 0 | none |
| warm idle | 8.0 | 800 | 35 | 0 | 0 | none |
| idle, A/C on (only when `ac = yes`) | 4.0 | 800 | 40 | 0 | 0 | A/C |
| cruise | 4.0 | 2500 | 45 | 15 | 60 | in gear |
| full throttle | 14.0 | 2000 to 9000 | 98 | 100 | 100 | in gear |
| lift off | 22.0 | 6000 to 1500 | 25 | 0 | 80 | in gear |
| warm idle again | 8.0 | 800 | 35 | 0 | 0 | none |
| code read-out | 40.0 | 800 | 35 | 0 | 0 | service connector grounded |

Engine inputs are refreshed every 10 ms of ECU time.

## The lines

Two sections, `VALID FOR THIS ECU` and `RUNS A BASIC SETUP`. Every line is in
one of three states: **ok**, **problem**, **could not tell**. An instruction
the emulator does not implement is "could not tell"; an opcode the processor
does not define is a "problem". A section is a problem if any line is, else
could-not-tell if any line is, else ok. The rules below are those of
`verdict.rs`.

**Valid for this ECU**

| line | ok when | problem when | could not tell when |
|---|---|---|---|
| executes | the run reached the end without stopping | it hit an undefined opcode | it hit an instruction the emulator does not implement |
| settles after power-on | at most 16 BRKs in the first second, none later, watchdog served | more than 16 BRKs in the first second; a BRK after the first second (a reset); the watchdog unserved for 0.5 s | the run stopped before this could be judged |
| talks only to this board | the board was programmed and no access fell outside it | the board was never programmed | any unmapped external read or write, or the 82C55 put in a mode the emulator lacks |

**Runs a basic setup**

| line | ok when | problem when | could not tell when |
|---|---|---|---|
| car as described | (never ok; the line is absent when every car value is modelled) | never | some value is unmodelled; the line names each key and the reason |
| fuel pump prime | in `key on` the pump was on for 0.5 to 5 s and then off | otherwise | the run stopped before the stage |
| cranking | the engine turned and every injector output opened | an output never opened | the engine did not turn, or the run stopped before the stage |
| warm idle | at least 10 engine cycles; each output opened within 2 of the cycle count; ignitions at least 3 per cycle | an output opened 0 times or more than 2 away from the cycle count; too few ignitions | fewer than 10 engine cycles in the stage (`MIN_IDLE_ENGINE_CYCLES`), or the run stopped before it |
| idle, A/C on | the clutch was engaged (the line exists only with `ac = yes`) | it never was | (not applicable) |
| full throttle sweep | always ok when the stage ran; it reports where fuel stopped and where VTEC switched on | never | the run stopped before the stage |
| check-engine light | never lit after the bulb check and the read-out was reached | lit at any time between the bulb check and the read-out | not lit, but the run stopped before the read-out |
| codes | lamp off, or steady with the connector grounded (a steady lamp during the read-out means no code is stored) | one or more codes blinked; each is shown with its name from the catalogue in `dtc.rs` | the lamp blinked but no code could be read; the lamp changed but no complete code could be read; the read-out was not reached |

The bulb check is the lamp lit from power-on until the first lit-to-dark edge
in `key on`. A lamp that is still lit at the end of `key on` without having
gone dark never ended its bulb check: it counts as lit after the bulb check,
at the end of `key on` (`script::mil_lit_after_bulb_check`). Before this
rule a fault the ROM stores inside the first two seconds held the lamp lit
from power-on and the line read "never lit": the stock P28 with `vtec = no`
(code 21 stored at 1.2 s) and on an automatic board in a manual car (code 19
at 0.9 s) were judged do-not-burn on their codes only, and the P30 image on a
P28 box (code 24, which the ROM never blinks) was worth burning. All three
now fail on this line.

Lamp reading (`blink.rs`): with the connector grounded the ROM flashes each
stored code; a blink lit at least 0.82 s is a tens digit and a shorter one is
a ones digit, and a dark gap of at least 2.0 s ends a code. A lamp lit without
a break for at least 10 s at the end of the window with no complete run in it
is `Steady`.

### The three verdicts

- **WORTH BURNING** (exit 0): every line is ok.
- **DO NOT BURN** (exit 1): at least one line is a problem. The verdict names
  the first one, in the order printed.
- **COULD NOT TELL** (exit 3): nothing is a problem, but at least one line is
  could-not-tell. The verdict names the first one.

A problem always wins over a could-not-tell. A line's `ok` means only that
what the script looked at looked right; the `NOT JUDGED` line says what it
did not look at.

## Assumptions

Numbers chosen here rather than measured on a car or taken from a document:

- **Watchdog threshold.** The watchdog counts as unserved after 0.5 s
  (`WATCHDOG_STARVED_SECONDS` in `bench.rs`). The real overflow period is on
  manual pages missing from the scan.
- **Boot window.** A boot is one second with at most 16 BRKs
  (`BOOT_WINDOW_SECONDS`, `BOOT_BREAK_LIMIT`).
- **Blink timing.** 0.82 s separates a tens blink from a ones blink, 2.0 s of
  dark ends a code, 10 s lit without a break is a steady lamp
  (`LONG_BLINK_MIN_SECONDS`, `CODE_GAP_MIN_SECONDS`, `STEADY_MIN_SECONDS` in
  `blink.rs`).
- **Verdict thresholds.** Fuel pump prime 0.5 to 5 s, injector openings within
  2 of the engine cycles, 3 ignitions per cycle, 10 engine cycles minimum for
  `warm idle` (`verdict.rs`). Fuel stop: an opening followed by two engine
  cycles with none (`script.rs`).
- **Static wiring.** The values listed under "Which values change the run
  today" are applied to the board; the unmodelled ones make the verdict could
  not tell. The A/T position inputs are a model of the switch, not a measured
  one. A working ELD and the stock power-steering switch keep the model's old
  levels (mid scale; port A bit 3 raw 1), which are not measured either.
- **Coolant at 88 C.** Every stage but `cold idle`; see the VTEC finding below
  for why the curve matters.

## Findings while building the script

Measured on the stock P28-230 ROM, `stock-p28-manual` setup, one run of
`script::run` (92.5 s of ECU time, with the 4 s `lift off` the script had then; the times below are from that run).

### With no fault stored, the grounded connector holds the MIL lit

The script's expectation was `Lamp::Off` in `code read-out`. The ROM does
something else: 0.07 s after the connector is grounded (52.50 s) the MIL
lights (52.58 s) and stays lit until the end of the run (92.51 s), with no
blink. The full MIL edge list for the run is
`(0.044 dark) (0.044 lit) (2.105 dark) (52.577 lit)`. `blink::decode` sees one
rise inside the window that never closes and returns `Unreadable`.

The ROM path, at 0x36B1..0x36CD (run while `210h.7`, the inverted service
connector bit, is set):

```
36B1  JBR  off(210h).7, 36CD     ; connector open: leave the MIL alone
36B4  MOV  DP, #031Ah
36B7  L    A, [DP]               ; stored-fault copy 31Ah..31Bh
36B8  JNE  36CA                  ; a code is stored: MIL = C (the blink pattern)
36BC  L    A, [DP+2]             ; 31Ch..31Dh
36BD  JNE  36CA
36BF  LCB  A, 06011h             ; option byte
36C3  JEQ  36C9
36C5  LB   A, 0E7h
36C7  JNE  36CA
36C9  SC                         ; no code stored: carry set
36CA  MB   P1.4, C               ; PC4, the MIL
```

At the end of the run RAM `210h` = C0h (bit 7 set), the stored-fault words
`212h..215h` and `31Ah..31Dh` are all 00h, and ROM `6011h` = 00h. So with no
code stored the ROM takes 0x36C9 and holds the MIL lit for as long as the
connector stays grounded. That is the ROM's own "no codes" display; it is not
caused by the script, the wiring or the model, and the model was not changed.

This matches Honda practice: with the service connector jumpered and no code
stored, the lamp stays on steadily. The plan's expected `Lamp::Off` was
wrong. `blink::decode` now returns `Steady` for a lamp lit without a break
for at least `STEADY_MIN_SECONDS` (10 s) at the end of a window holding no
complete run. Before this change it returned `Unreadable`, because the rise
lands inside the window. The script test expects `Steady`, and for the
verdict either `Off` or `Steady` means no code blinked.

### Resolved: VTEC never engaged at 88 °C coolant

`vtec_on_rpm` was `None` in `full throttle` (2000 to 9000 rpm, 100 % TPS,
100 km/h): port C bit 0 never went high during the run. The ROM's VTEC
decision at 0x11BE..0x11E2 drops to `RB P1.0` (0x11E2) at 0x11C7,
`CMPB 0C1h, #044h / JGE`: RAM `C1h` must be below 44h. `C1h` is the high
byte of the U5/X2 sample (`3C8h`), passed through the slew filter at 0x5248,
which follows the raw byte with a two-count dead band (it settles 2 below a
rising input and 2 above a falling one). Held at 6000 rpm, 100 % TPS,
100 km/h after a warm boot, on the old model:

| `ect_celsius` | `C1h` | PC0 went high |
|---|---|---|
| 88 | 49h | no |
| 95 | 43h | yes |
| 110 | 2Eh | yes (and later off again via 0x11B9, not traced) |

The cause was the model: `EngineState::sync_sensors_to_bus` mapped ECT on a
straight line (`1023 - (ect + 40) / 160 * 900` counts), a placeholder that
put 88 °C on the cold side of the ROM's gate.

**Curve now used** (`ECT_VOLTS` / `ect_volts` in `engine.rs`): eCtune's
`temperatureSensorCurve` (`ectune-cobrartp/source/eCtuneBin/Calibration.cs`),
which gives °C for each 8-bit ECU reading and which eCtune applies to both
ECT and IAT. Every 16th byte is kept (at most one byte off the full curve),
at the middle of that byte's ADC bin.

**Cross-checks.**

- The pgmfi wiki, "Engine Coolant Temperature"
  (`hondabase.com/pgmfi/wiki/library/engine-coolant-temperature`): Ed
  Gibson's table for a '93 CRX, sensor resistance measured in a lab from 0 to
  100 °C and ECU bytes mapped by applying equivalent resistances on the car.
  It agrees with eCtune within 1.3 °C from 20 to 100 °C (3.2 °C at worst,
  near 8 °C); below 0 °C it has no data. The page's attachment is the Honda
  shop-manual resistance chart. Gibson's table was not used as the model
  because it stops at 0 °C, and the DTC 6 stimulus (`dtc.rs`, -40 °C must
  read above 900 counts) needs the cold end.
- The ROM's own coolant thresholds on `C1h`, read on the curve:

  | `C1h` | where | eCtune | Gibson |
  |---|---|---|---|
  | 44h | VTEC allowed below it (0x11C7) | 59.7 °C | 60.5 °C |
  | 3Bh | fail-safe value on a sensor fault (0x3BB8) | 65.4 °C | 66.6 °C |
  | 35h | purge allowed below it (0x4338) | 69.6 °C | 70.8 °C |
  | 34h | 0x0DAC, 0x190B and others | 70.3 °C | 71.5 °C |
  | 2Eh | 0x0D8B, 0x0D64 and others | 75.1 °C | 75.8 °C |
  | 20h | 0x06C9 | 88.8 °C | 88.9 °C |

  A VTEC gate near 60 °C fits the usual statement of Honda's VTEC
  conditions (engine warm, about 60 °C); that statement is not separately
  sourced here.

**Now observed.** At 88 °C `C1h` = 23h (`trace ... snap 800 2 C0 C1`; the raw
byte is 21h, and the filter settles 2 above it coming down from the
fail-safe 3Bh). In the script VTEC switches on at **4875 rpm** in
`full throttle`: the low-cam window (0x117B, `DP = 6432h`) sets `128h.1`
when the log-rpm code `12Dh` exceeds CDh (4812 rpm), and the first code
above it, CEh, is 4875 rpm. The README of the P28-230 source project gives
4812 rpm for this window.

**VTEC pressure switch.** Once VTEC engaged, the run first stored code 22
(VTEC system): the MIL lit at 35.43 s, about 3 s after VTEC came on, with
fault word `212h..215h` = `00 00 20 00` (bit 21). The ROM's check at
0x3D34..0x3D40 takes `C = 211h.1` (`211h = DATA[4700h] XOR 1Ah`, so 1 =
switch open): with `21Fh.2` clear (VTEC off) an open switch sets `9Ah.1`,
with `21Fh.2` set (VTEC on) a *closed* one does. A Honda VTEC pressure
switch is normally closed and opens under the oil pressure the solenoid
admits; the model's switch was static, never opened by the solenoid, because
no run had reached VTEC before. `Bus` now presents the switch open at
`4700h` while the solenoid is powered (`vtec_solenoid_connected` and PC0,
the same condition as the solenoid feedback); `p28_input_port` stays the
switch's rest state, so a switch set open still reads open. Assumption: the
switch opens the instant the solenoid is driven; real oil pressure takes
time to build, and how much delay the ROM's check tolerates is not
established (see the monitors table in `CPU_CORE.md`). With it the script
runs clean: VTEC at 4875 rpm, no MIL after the bulb check, `Lamp::Steady`.

No other stock-ROM test moved.

### Open: `IAT_VOLTS` disagrees with the curve eCtune uses for IAT

eCtune applies the same `temperatureSensorCurve` to ECT and IAT. The
intake-air table in `engine.rs` (`IAT_VOLTS`, from the pgmfi wiki's
"Intake Air Temperature Sensor" page, values attributed there to AEM) reads
much colder than that curve for the same voltage:

| `IAT_VOLTS` | volts | byte | eCtune |
|---|---|---|---|
| 1 °C | 3.900 | C7h | 3.4 °C |
| 18 °C | 2.808 | 8Fh | 27.1 °C |
| 27 °C | 2.340 | 77h | 36.4 °C |
| 51 °C | 1.404 | 47h | 58.0 °C |
| 66 °C | 0.936 | 2Fh | 74.3 °C |
| 83 °C | 0.468 | 17h | 99.9 °C |
| 100 °C | 0.000 | 00h | 141.0 °C |

So the voltage the model gives for 27 °C air is what eCtune's curve reads as
36.4 °C; the gap grows toward the hot end.
Which is right for this sensor is not settled; `IAT_VOLTS` is unchanged.

### Also observed (all within the brief's assertions)

- Fuel pump on for 2.06 s in `key on`.
- `warm idle`: 53.34 engine cycles, injector openings 53/54/53/53,
  214 ignitions (4 x 53.34 = 213.3).
- `full throttle`: fuel stops at 7420 rpm.
- `lift off` (6000 to 1500 rpm, TPS 0, 25 kPa): no injector opens for the
  whole stage, so `fuel_stops_rpm` is `None` there (there is no opening for a
  gap to follow).
- No MIL edge between the bulb check (lit 0.04 to 2.11 s) and the read-out.

## The automatic gearbox

Added with the acceptance tests (`acceptance.rs`). Measured on the stock
P28-230 ROM, one run of `script::run` per setup.

**Car side.** With `gearbox = automatic`, `wiring::apply_switches` drives the
A/T position switch on `4700h`: bit 5 (park/neutral) when the stage is not in
gear, bit 7 (D4) when it is (`cruise`, `full throttle`, `lift off`); bit 6
(D3) is never selected. The ROM keeps these three bits uninverted in `211h`
(`211h = 4700h XOR 1Ah`). Their meaning is read from the ROM:
0x32A8..0x32AE tests `211h.5`, `.6`, `.7` in that order as the gates of the
lock-up routine, and 0x0D55..0x0D79 applies an automatic's in-gear idle
compensation (brake pressed) only with `211h.5` clear, so `211h.5 = 1` is
park/neutral. A manual car leaves all three at 0.

**Board side.** `4700h` bit 3, the feedback from the driver of lock-up
solenoid B (U16), now has a model: it reads low while P4.3 is high and high
otherwise (`Bus::p28_input_port_as_read`). Derivation, from the ROM alone:

```
0336  STB  A, r0          ; LRB 70h: r0 = 380h, a counter 0..63h (timer ISR)
0337  CMPB A, r1          ; r1 = 381h, the duty, reloaded from DFh at wrap
0338  MB   P4.3, C        ; P4.3 high while counter < duty
...
3CAF  MOV  DP, #0380h     ; automatic only (0x5F5C: 216h.3), 214h.2 clear
3CB9  L    A, [DP]        ; er0 = counter, duty
3CBE  LB   A, [4700h]
3CC6  ANDB r7, #08h       ; bit 3, raw
3CCE  SUBB A, r1          ; counter >= duty (P4.3 low): bit 3 must be 1
3CD6  ADDB A, #0Ah        ; counter more than 10 below duty (P4.3 high):
                          ; bit 3 must be 0
3CDD  SB   09Ch.7         ; else fault, code 19 (table 6C99h)
```

P4SF.3 is cleared at 0x2351 after the power-on check, so P4.3 is a plain
output from then on. **Assumption:** P4.3 is the line into U16 and the
feedback inverts it like the lock-up A feedback (port A bit 4, the inverse of
PB4). Both are inferred from this check; the schematic trace from P4.3 to U16
was not made. On the stock run the duty `381h` stays 0, so solenoid B is
never driven and only the "off" half of the check is exercised.

**Observed, `stock-p28-automatic`:** worth burning, `Lamp::Steady`, fault
words `212h..215h` all 0, `216h` = 5Bh (bit 3, automatic, from the strap at
0x55C5). Lock-up solenoid A (PB4, 0x3387) engages in `cruise` (24.2 s) and
drops at the start of `full throttle` (26.6 s); the lock-up checks at 0x3C98
and 0x3CAF ran about 8400 times each without raising a fault.

### Gearbox mismatch

Honda practice: an automatic ECU in a manual car stores code 19 (lock-up
solenoid). The run now does too (acceptance case 7).

| board | car | verdict | codes | why |
|---|---|---|---|---|
| automatic | manual | do not burn | 19 | no solenoids, so both lock-up feedbacks read "powered" (below) while the ROM keeps both drivers off: no position switch, `211h.5/.6/.7` all 0, so 0x32A8..0x32AE never commands lock-up (PB4 low the whole run, duty `381h` 0). 0x3C98..0x3CA5 (`99h.6`) and 0x3CAF..0x3CDD (`9Ch.7`) compare each feedback with that off command and fail; both are code 19 in the table at 6C99h |
| manual | automatic | worth burning | none | `216h.3` clear: 0x3C92 and 0x5F5C skip both lock-up checks; PB4 is never driven. The ROM never looks at the lock-up feedbacks, so a missing driver or solenoid cannot reach it; the mismatch reaches the ROM only through the A/T position inputs. Modelled, not reported as unmodelled. (The pgmfi wiki's auto/manual page says a manual board lacks IC16, the lock-up B driver.) |

How the feedbacks read with no solenoid, from Bomarc sheet 8: each lock-up
driver (U15 for A, U16 for B, "5050" parts; VTEC's U14 is the same) has its
output pin wired to the connector (A4, A5; A25 for VTEC) and, on the board,
to RM11 (M5565): an 8.2 kohm pull-up from RM11 pin 1, a supply rail, and an
18 kohm / 10 kohm divider into the base of a 2SC2785 with its emitter
grounded (Q28 for A, Q27 for B, Q26 for VTEC). The collector, pulled up in
RM13 (10 kohm), is the feedback: Q28 to 82C55 pin 42 (port A bit 4), Q27 to
ID3 (`4700h` bit 3), Q26 to pin 41 (port A bit 5). With a solenoid connected
and the driver off the coil holds the output near ground: transistor off,
bit 1. Driver on: output at the supply, bit 0. No solenoid: the pull-up alone
sets the output at about 0.77 of the rail (8.2 k against 28 k), the base at
about 0.28 of it, enough to turn the transistor on at 5 V or more, so the bit
reads 0 whatever the driver does. That is the open-load detection the
pull-ups exist for. **Assumption:** a "5050" that is off does not itself pull
its output down; its data sheet was not found.

`Bus::lockup_solenoids_connected` (set to `gearbox = automatic` by
`wiring::apply_static`) applies this; `Bus::vtec_solenoid_connected` now
follows the same circuit, so an unplugged VTEC solenoid reads "powered" too
(it read "not powered" before). Observed with `vtec = no` (acceptance case
`case_6_no_vtec_stores_code_21`): do not burn, code 21 blinked and stored.
The feedback, port A bit 5, reads "powered" while the ROM keeps PC0 low, and
0x3D0C..0x3D21 (behind the VTEC option flag `216h.4`, 0x3D06) sets `9Ah.0`;
VTEC never switches on. The run before the change was not recorded.

## Where the preset values come from

`stock-p28-manual`, key by key. "Option byte" means the byte at 6001h..600Ah
that 0x5560..0x55B3 copies, bit 7 into a RAM flag; the fault code of each
check bit is the table at 6C99h, indexed by `(byte - 98h) x 8 + bit + 1`
(this reproduces every pairing in the monitor table of `CPU_CORE.md`).

| key | value | source |
|---|---|---|
| `ecu` | p28 | the image is P28-230; pgmfi wiki "P28" page: 92-95 Civic Si/EX and del Sol, SOHC VTEC D16Z6 |
| `board_gearbox` | manual | `CPU_CORE.md`: mask 230 is the manual P28-A00; the manual strap (U5/X1 low, board photographs) is what the ROM needs for `216h.3` clear (0x55C5) |
| `gearbox` | manual | follows `board_gearbox` for a stock car; nothing more specific found |
| `vtec` | yes | option byte 6001h = FFh sets `216h.4` (0x5566), which enables the VTEC solenoid and pressure-switch checks (0x3D06, 0x3D27); the wiki's P28 page names `216h.4` "Vtec Enable Ram Flag"; D16Z6 is a VTEC engine (wiki) |
| `knock_sensor` | no | option byte 6002h = 00h clears `227h.6` (0x556E), and 0x3D44 then skips the check that sets `9Ah.2`, code 23 (observed: `227h` = 90h) |
| `o2_sensor` | heated | option byte 6003h = FFh sets `216h.6` (0x5576), copied to `116h` (0x03F7, observed `116h` = 53h); `116h.6` gates the heater check at 0x2087..0x20CB (`9Bh.2`, code 27). A stock car must therefore have a heater; the wiki's P28 page lists an "O2 Heater Jump Routine" to disable it |
| `iacv` | stock | the IACV current check 0x3C52..0x3C71 (`99h.3`, code 14) has no option-byte gate (its gate `217h.5` is not one of 0x5560's flags); with `iacv = none` the run stores code 14 |
| `egr` | no | the stock ROM cannot raise code 12: 0x29AA `CMPB off(236h), #0` never sets carry, so `99h.0` (0x29B6) and `226h.7` stay clear, and `226h.7` clear skips the lift check that sets `99h.1` (0x2A02..0x2A23). The ROM does not care; whether the stock car had EGR is **unsourced** |
| `eld` | yes | option byte 6005h = FFh sets `219h.1` (0x5586), which gates the ELD check 0x3CE2..0x3D02 (`99h.7`, code 20); the wiki's P28 page lists an "ELD Sensor Enable" option |
| `speed_sensor` | yes | the VSS check 0x3C74..0x3C8B (`99h.4`, code 17) has no option-byte gate, so a stock car has a VSS; the wiki's P28 page lists the VSS RAM value |
| `ac` | yes | the ROM drives an A/C clutch from the A/C switch (tested, `replay.rs`), but has no option byte for it; that the stock car has A/C is **unsourced** |
| `power_steering_switch` | yes | the wiki's P28 page names the PSP input (`210h.3`, pin B8) and the ROM reads it (0x2A8F and others); that the stock car has the switch fitted is **unsourced** |

No source disagreed with `car.rs`, so no preset value changed. The wiki's P28
page gives other ROM addresses for its option bytes (60E6h, 60E7h, 60F2h);
they do not match this image, whose options are at 6001h..600Ah, but the RAM
flag it names for VTEC (`216h.4`) does.

## The car values, one by one

Each row below was run on the stock P28-230 ROM with only that key changed
from `stock-p28-manual` (`acceptance.rs`, cases 6 and 7), except the
unmodelled ones, which share one run. Schematic references are to the
original Bomarc scan (37820-P28-A51), not the restored images.

| key = value | level chosen | source | stock ROM did | Honda practice |
|---|---|---|---|---|
| `o2_sensor = one-wire` | heater unplugged (`o2_heater_connected = false`); sensor input as before | nothing new | do not burn: MIL lit at 8.8 s, code **41** blinked; the stored fault bit is 27 (`9Bh.2` from 0x2094..0x20CB, table 6C99h index 27 = 1Bh), so the ROM blinks stored bit 27 as 41 | matches (code 41, O2 heater) |
| `o2_sensor = none` | heater unplugged as above; the sensor input is **unmodelled** | sheet 3: D16 goes through R4 (10 kohm) to the + input of U22a (NJM2904), a voltage follower into U6/X0 through 1 kohm; only C2 and C3 sit on that node, nothing biases it. With no sensor the level is set by the op-amp's input bias current, which no source here gives | code 41 from the heater (it outranks the car line) | n/a |
| `iacv = <ohms>` | **unmodelled** | no stock coil resistance was found: not in the eCtune source, not on the pgmfi wiki's IACV page. See below | n/a | n/a |
| `egr = no` (stock) | AI3 = the U6/X7 battery level | sheet 3: D17 reaches AI3 (MCU pin 57, P5.3) through 100 kohm in RM2 and is tied by another RM2 resistor to RM2 pin 6, the battery divider node that feeds U6/X7 through R51 (10 kohm). With D17 open, AI3 sits at that node | unchanged from before: worth burning (case 1). See "What the stock ROM does with AI3" below | matches (no EGR code on a car without EGR) |
| `egr = yes` | **unmodelled** | the lift sensor's resting voltage has no source here | n/a | n/a |
| `eld = no` | U5/X0 = 930 counts, 4.55 V (E8h) | sheet 3: D18 has R136 (1 kohm, 1 %) to +5 V and R138 (10 kohm) to ground, then R137 (100 kohm) into U5/X0; open, 5 x 10/11 = 4.55 V. Caveat: R136's value is typed on the scan, not hand-lettered like the rest, so someone added it later; with R136 above about 1.6 kohm the open level would be inside the ROM's range and no code would be stored | do not burn: code **20**, no MIL. 0x3CE2..0x3D02 (gated by option flag `219h.1`, `217h.6`, `214h.3` clear and battery `C3h` at least 69h) sets `99h.7` when `3C6h` is above DCh or below 0Eh | matches (code 20, ELD) |
| `speed_sensor = no` | no INT0 pulses (`EngineState::speed_sensor_fitted`) | nothing new | do not burn: MIL lit 17.4 s into `lift off` (about 2440 rpm), code **17** stored and blinked; VTEC never switches on. See below | matches (code 17) |
| `power_steering_switch = no` | **unmodelled** | sheet 3 and the wiki disagree; see below | n/a | n/a |
| `knock_sensor` on a P28 | no board (`Absent`) either way | option byte 6002h = 00h clears `227h.6` (0x556E), so 0x27F7 and 0x3D44 skip both knock checks; and with no board the sensor reaches nothing (sheet 5: D10 goes only to FC1 pin 10) | no difference | matches |
| `ecu = p30` | knock board fitted (`knock.rs`) | "The P30 box" below | not run: with `227h.6` clear the P28-230 image skips both checks that read the board (0x27F7, 0x0CA1 via `233h.6`, 0x3D44) | n/a |
| `gearbox = manual` on an automatic board | no lock-up solenoids | sheet 8, see "Gearbox mismatch" | do not burn, code **19** | matches |
| `gearbox = automatic` on a manual board | position inputs only | the ROM never reads the lock-up feedbacks with `216h.3` clear | worth burning | matches the ROM; practice says nothing specific |

### `speed_sensor = no` and the length of `lift off`

The VSS check at 0x3C74..0x3C8B sets `99h.4` only when battery `C3h` is
above 44h, the rpm code `236h` is below B0h (0x3C7B; observed about
3300 rpm: set at 3250, never at 3500), code 17 is not yet stored (`214h.0`),
no VSS period has been measured (`231h.3`, set at 0x24CF and 0x27E6, cleared
when a period is measured at 0x278D) and deceleration fuel cut is active
(`21Ch.4`, a copy of `11Ch.4`, written at 0x18BD). The debounce counter
`2C1h` is loaded with 2Dh (6C68h index 13) at 0x49D3 and was observed to
step down once every 0.10 s; holding the conditions at 3000 rpm stored code
17 5.1 s after entering them. Fuel cut was observed on at closed throttle
and 25 kPa throughout a falling sweep from 6000 to 1000 rpm.

With the old 4 s `lift off` the window lasted about 1.75 s and nothing was
stored, so a car without a speed sensor came out worth burning only because
the stage was short. `lift off` is now 22 s, still a linear 6000 to 1500 rpm
ramp at 25 kPa, closed throttle, 80 km/h, in gear: it spends 1750/4500 of its
length below 3250 rpm, 8.6 s, about 1.7 times the 5.1 s. Observed with no
speed sensor: the MIL lights 17.4 s into the stage (about 2440 rpm), code 17
is stored and blinked (6C99h index 13 = 11h); do not burn. With a speed
sensor the stock runs (cases 1, 2, the script test) store nothing in the
longer stage.

The longer run moved the read-out later, and one run (`eld = no`) then
opened the read window 0.09 s before the lamp lit for 0.61 s: a long blink
of the ROM's free-running sequence cut by the grounding. `blink::decode`
read that as a code 1, because it counted the lamp's dark stretch before
the window as the gap that starts a run. It now counts that gap from the
window's start at the earliest, so the first run read is one that follows a
full gap inside the window (`blink.rs`).

### What the stock ROM does with AI3

0x29A1 reads AI3 (`ADCR3H`) into `2F5h` and `r0`. `2F5h` is only copied out
(0x5501). The lift check is closed: `226h.7` is the carry of
`CMPB off(236h), #0` at 0x29AA, always 0, so `99h.0` stays clear and 0x2A02
skips the lift check. 0x29E0..0x29E6 stores `2F2h = r0 - 2F4h`, clamped at
0, where the base `2F4h` is written at 0x29DE from 0x29D3..0x29DD: that code
would take `r0`, but only past two compares of `r0` with the immediate FFh
(`CMPB A, r0` at 0x29D5 and 0x29DA), which never let it through, so the
stock ROM stores FFh there (as at 0x2500 and 0x5543) and `2F2h` is 0
whatever AI3 reads. `2F2h` is read at 0x0A4A (compared with FFh, automatic
only), 0x2129 (compared with FFh) and 0x2A36 (`LB A, 2F2h; SUBB A, 2F3h`),
which runs whenever the EGR target `2F3h` (written at 0x21B7) is not zero
and feeds the EGR control path from 0x2A2A on. In the stock ROM that path
therefore sees 0, not AI3, so moving the stock car's AI3 level cannot reach
it. A ROM with other values in those immediates would learn the base from
AI3 and its EGR control would then depend on the AI3 level; the verdict
does not observe the EGR solenoid output either way.
### `power_steering_switch`: the schematic and the wiki disagree

Sheet 3: connector B13 is pulled up by R101 (22 kohm) and drives, through
R102 (4.7 kohm), the base of a PNP 2SA1175 (Q41) whose emitter is on +5 V
(R99, 22 kohm, base to emitter; C87 across them). Its collector, with R100
(4.7 kohm) to ground, is the output "C (6)", which sheet 6 places by the
82C55 without drawing its pin. Read that way, a grounded switch turns Q41 on
and the bit reads 1; an open switch, or no switch, leaves it off and the bit
reads 0, so `210h.3` (bit 3 of `port A XOR 38h`) is 1 with nothing on B13.
The pgmfi wiki's P30 page says the opposite: "If pin B8 (PSP) is grounded,
0x0210.3 will read 1". Unless the path on sheet 6 inverts again (it is not
drawn), the two cannot both hold. The ROM does not settle it: it has no
fault code for the switch, and a development run with port A bit 3 at 0
(the schematic's open level) was worth burning with idle-valve duties within
0.2 points of the stock run's in every stage. So `no` stays unmodelled. The
stock car's switch keeps the model's old rest level, raw 1, which by the
schematic is a grounded switch and by the wiki an open one; which state a
Honda P/S switch rests in is not sourced here either.

### `iacv = <ohms>`: why a resistance could not be trusted far anyway

The model's IACV current is `I = duty x Vbatt x P28_IACV_SENSE_PER_VOLT`,
and that gain is not a measured board constant: it was picked (0.14) inside
the window of about 0.10 to 0.20 of full scale per volt that the ROM's own
limit tables at 6B24h and 6B2Ah allow (`CPU_CORE.md`, "IACV current loop").
A fitted coil of R ohms would scale the current by `R_stock / R`. Even with a
sourced `R_stock`, a ratio of up to about 1.4 either way could be absorbed by
where in that window the real gain sits, so a resistance-based result could
only say something about coils far from stock (roughly below 0.7 or above
1.4 times its resistance), and nothing about the edge cases between. Without
`R_stock` the value is unmodelled.

## The P30 box

### Which image is the factory P30

Two P30 mask-203 images are in the workspace, `ecu/bins/p30_203.bin` and
`ecu/bins/p30_usdm_203.bin`. Both are 32768 bytes and sum to 0; they differ
in 1068 bytes.

| | `p30_203.bin` | `p30_usdm_203.bin` |
|---|---|---|
| SHA-256 | `c3decae2...61dfa` | `e0fff820...b4206` |
| pgmfi wiki, "ECU Definition Codes" | byte for byte the attachment `203stock.bin`, "Common ROM for development" (files.hondabase.com download 120); the table lists mask 203 as JDM P30, Civic B16A DOHC VTEC, "common dev rom" | byte for byte the attachment `P30USDMCivicD16Y8203.BIN`, "P2P fuel and ignition on P30" (download 165): a P30 image retuned for a USDM D16Y8 |
| code at 0x0209..0x02DC | identical to P28-230 | rewritten, with FFh fill after it |
| code at 0x285A | identical to P28-230 | `J 2867h` and FFh fill: a patch |
| byte 0x7FFF | FFh, as in P28-230 | 04h: a checksum correction |
| option bytes | `6002h` FFh (knock), `6003h` FFh (O2 heater), `6004h` 00h, `6005h` FFh (ELD) | `6002h`, `6003h`, `6005h` 00h; `6004h` FFh (not traced) |
| on the model, `check` | see below | worth burning on both `stock-p28-manual` and `stock-p30`: with `6002h` 00h the knock checks never run |

The first is the factory image: it matches the wiki's stock upload, and the
second carries a patch, a rewritten routine, a checksum fix and three
options switched off. The fixture `p30-rom` is `p30_203.bin`.

### What the stock P30 does

Measured on `p30_203.bin`, one run per setup (`acceptance.rs`). How the
board is modelled, and what the ROM checks, is in `CPU_CORE.md`, "P30 box".

| setup | verdict | codes | why | Honda practice |
|---|---|---|---|---|
| `stock-p30` (P30 box, sensor fitted) | worth burning | none | the board's heartbeat keeps `2E8h` reloaded and its frames pass (`233h.7` clear, `232h.7` clear). VTEC at 5690 rpm, fuel stops at 8310 rpm | matches |
| `stock-p30`, `knock_sensor = no` | do not burn: MIL lit at 8.6 s, in `cold idle` | **23** blinked and stored | 0x3D44..0x3D61 sets `9Ah.2` from frame bit 8. That the board sets bit 8 for an unplugged sensor is the model's assumption (bit 8 is the ROM's only path to code 23, and code 23 is documented as covering an unplugged sensor), so this run confirms the ROM's path, not the board | matches, by construction |
| `stock-p30`, `ecu = p28` (no knock board) | do not burn: MIL lit again at 2.2 s, 0.1 s after the bulb check ended | stored: **24** (fault bit 23, `214h.7`); blinked: none | port A bit 2 never changes, so 0x27F7..0x2824 never reloads `2E8h` and sets `9Ah.3` from its first pass (0.06 s); code 24 is stored at about 2.2 s. With `214h.7` set the read-out routine skips the blink sequence (0x3634 `JBS 214h.7`) and holds the lamp lit, so the grounded connector shows what looks like the no-code display; the `codes` line therefore reads "none". Same result whatever the pull-ups give on bits 0..2 | **disagrees**: practice expects code 23. The ROM stores 24, which it never blinks, and keeps the MIL lit |

The last row is the ROM's own behaviour; the model was not changed to bring
out code 23. A P30 ROM in a P28 box keeps the check-engine light on and
answers the service connector with a steady lamp, which a driver would read
as "no code stored".
