# `check`: is this ROM valid for this ECU, and does it throw codes?

Design, 2026-11-13. First of three projects (see "The bigger picture").

Statements marked **(owner)** are what the project owner said. Statements
marked **(my call)** are choices made while writing this up that the owner has
not been asked to confirm one by one. Everything else is a fact about the
current tree, checked on the day this was written.

## Why

**(owner)** The purpose of the whole project is an ECU simulator: someone has a
ROM file and wants to know, before burning a chip and fitting it to their car,
whether a physical ECU would run it and what would happen, so that they do not
find out from a car that runs badly or shows error codes.

**(owner)** The main question is whether the ROM is built right for the ECU:
that it is valid and can run a basic setup. It is not about measuring whether
the tune is good. Fuelling is not judged.

**(owner)** The ROM alone is not enough input. Cars differ (no knock sensor, a
one-wire or a heated O2 sensor, a different IACV, manual or automatic), so the
tool also needs a description of the car, and it must cover both manual and
automatic.

Today the tool cannot do this:

- `hondaecu-cli test` claims to be an "850+ matrix exhaustive ROM suite". A
  file of random bytes scores 82 of 84 on it. Most of its checks are
  hard-coded `passed: true` or test that a ROM byte is not `0xFF`
  (`src/suite.rs`, `src/dtc.rs::DtcEvaluator`).
- `replay` prints numbers and says "no behavior pass/fail implied".
- The checks that do mean something are the `stock_p28_*` cargo tests in
  `src/replay.rs` (idles without faults, each unplugged load latches its own
  fault, MIL blinks with the service connector grounded, fuel cut above the
  rev limit). They are wired to one ROM and are not reachable from the CLI.

## What gets built

One command that runs a ROM on a modelled ECU box connected to a described car
and prints a verdict:

```
hondaecu-cli check my-rom.bin --ecu p28 --car my-car.toml
```

### Three ways to give it the inputs (owner: interactive, arguments or config files are all fine)

1. **Interactive.** Run with only a ROM on a terminal and it asks: which ECU
   box, then a short series of questions about the car, each with a default.
   At the end it offers to save the answers as a car file.
2. **Arguments.** Every answer can be given on the command line, so nothing is
   asked.
3. **Car file.** A file holding the answers, reusable across ROMs.

**(my call)** Precedence, lowest to highest: built-in preset, car file,
command-line arguments. Interactive mode asks only for what none of those
supplied. When standard input is not a terminal, or `--no-prompt` is given,
nothing is asked and anything not supplied takes the preset's value; the
report lists which values were defaulted.

**(my call)** The car file is a flat list of `key = value` lines that is also
valid TOML, parsed by hand. The crate has no dependencies today and this keeps
it that way. The builder may add a TOML crate instead if hand parsing turns out
worse than a dependency.

**(my call)** `test` stops being a separate command. `suite.rs` and the
synthetic evaluators in `dtc.rs` are deleted; the code-number-to-name catalogue
in `dtc.rs` stays because the report needs names. `test` remains as an alias of
`check` so existing habits and scripts land on the real thing.

### The ECU box

**(owner)** P28 and P30 only for now; they are basically the same board.

The box is described by:

| setting | values | what it changes on the board model |
|---|---|---|
| model | `p28`, `p30` | P30 has the knock board on connector FC1; P28 does not |
| board gearbox strap | `manual`, `automatic` | the RP17/RP18 resistor pair on mux input U5/X1, which the ROM reads to decide which gearbox it is in (`docs/CPU_CORE.md`, "P28 external analog mux") |

The other board-configuration straps (U5/X4..X7, U6/X1, X4, X5, X6) keep the
levels `Bus::new` sets today for both models. Their meanings are not known.

Any 32768-byte ROM can be fed in, including ones written for another box (a
P72 image boots and injects on the P28 model today). `--ecu` says which box
the chip is going into, not which box the ROM was written for.

### The car description

Each row is something the ROM electrically watches or reads, so a mismatch
shows up as behaviour or a code. Codes in the last column are what Honda OBD1
practice leads one to expect; they are **not** requirements on the tool. The
tool reports what the ROM does.

| key | values | board model hook today | expected if it does not match |
|---|---|---|---|
| `gearbox` | `manual`, `automatic` | lock-up solenoid feedbacks (port A.4, `4700h`.3), park/neutral and gear-drive inputs (`4700h`.5..7) | code 19, idle and fuel-cut differences |
| `vtec` | `yes`, `no` | `vtec_solenoid_connected`, `set_vtec_pressure_switch_closed` | codes 21, 22 |
| `knock_sensor` | `yes`, `no` | **none**: port A.0..2 and PC3/PC7 go to FC1 and nothing is modelled behind them | code 23 |
| `o2_sensor` | `none`, `one-wire`, `heated` | HEGO voltage on U6/X0; `o2_heater_connected` | codes 1, 41 |
| `iacv` | `stock`, `none`, or a coil resistance in ohms | `iacv_connected`; the current loop uses `I = duty x Vbatt / R` with one fixed gain | code 14 |
| `egr` | `yes`, `no` | AI3 input exists, no valve model | code 12 |
| `eld` | `yes`, `no` | U5/X0 input exists | code 20 |
| `speed_sensor` | `yes`, `no` | VSS pulses on INT0 | code 17 |
| `ac`, `power_steering_switch` | `yes`, `no` | `4700h`.2, port A.3 | no code; idle behaviour |

Injectors, igniter, distributor, MAP, TPS, ECT, IAT and battery are always
present and healthy in this project. Unplugging them one at a time is what the
existing `stock_p28_latches_a_fault_for_each_unplugged_load` test does and it
stays a developer test.

**(my call)** Built-in presets, selectable by name and used as the defaults in
interactive mode: a stock VTEC manual car for the P28, the same car as an
automatic, and a stock car for the P30. Their exact contents (which of EGR,
ELD and so on a stock D16Z6 or B16A car has) are the builder's to research and
write down with sources.

### What a run does

Conditions are **scripted** in this project: the RPM and sensor values follow
the script whatever the ROM does. An engine that responds to the ROM is
project 3.

**(my call)** The run is the same fixed sequence for every ROM, roughly:

1. Key on, engine stopped, for a few seconds.
2. Cranking with the starter signal held.
3. Cold idle, then warm idle.
4. Warm idle with the A/C switched on, where the car has A/C.
5. Light cruise.
6. Full throttle, RPM swept upward past where any stock ROM cuts fuel.
7. Lift off from high RPM.
8. Warm idle again, long enough for slow fault debounces.
9. Service connector grounded, to read the codes.

Exact RPM, load, temperatures and durations are the builder's, with two
constraints. The warm-idle time must be long enough for every monitor the
stock ROM has to store its fault (the known ones take a little over five
seconds). And the whole run must be long enough that the stock ROM completes
at least one full pass of its background checksum (512 blocks, one block per
main-loop pass, `hondaromeditor/roms/p28-230/README.md`, "ROM checksum").

### What it observes

Only things that exist on the outside of the processor, so they mean the same
for any ROM regardless of where it keeps its variables:

- execution stopped on an undefined or unimplemented instruction, with the
  address;
- resets: how many, when, and whether they stop;
- external bus accesses that land on no modelled device
  (`Bus::external_unmapped_reads` / `_writes`), and the 82C55 being put in a
  mode the model does not support;
- injector outputs: openings per output per engine cycle, order, pulse width;
- ignition output: pulses on TM3IO per engine cycle;
- fuel-pump relay, MIL, A/C clutch, fan, VTEC solenoid, IACV duty;
- the RPM at which injection stops in the sweep and at which the VTEC output
  switches. These are reported as observations. Comparing them with stock is
  project 2.

### How codes are read

**(owner agreed, my proposal)** The way a mechanic reads them: ground the
service check connector (82C55 port A bit 7) and decode the MIL's blinks
(port C bit 4) into code numbers, long blinks for tens and short for ones.
This works on any ROM without knowing where it keeps its fault memory.
`stock_p28_blinks_the_mil_with_the_service_connector_grounded` shows the stock
ROM blinking; nothing decodes the blinks into numbers yet.

Note that the number the ROM stores is not always the number it blinks: the
stock ROM stores index 27 for the O2-heater fault, and how that becomes what a
driver sees was never traced (`docs/CPU_CORE.md`, "How a fault is stored").
The blink decoder is therefore the source of truth for the report.

For ROMs that keep the stock fault words at RAM `212h..215h`, those remain
available as a cross-check in developer tests. The report does not depend on
them.

A lit MIL with no code blinked (the backup-MCU heartbeat case does this) is
reported as exactly that.

### The report

Printed as plain text, readable by a tuner who is not a programmer.

**(owner)** The bottom line: the tool must validate that a given ROM will work
on a P28 or P30 board, so the user avoids wasting time burning a chip and
turning the car on. So the report opens with one verdict, before any detail:
worth burning, not worth burning (with the first reason), or could not tell.
"Worth burning" requires both headlines below to be clean and no codes.

Shape, not final wording:

```
ROM: my-rom.bin   ECU: P28 manual   Car: my-car.toml (stock-p28-manual, knock_sensor defaulted)

VERDICT: DO NOT BURN - sets code 23 (knock sensor) on this car

VALID FOR THIS ECU ......... yes
  executes ................. ok
  settles after power-on ... ok (2 resets, as the stock ROM does)
  talks only to this board . ok
RUNS A BASIC SETUP ......... no
  fuel pump prime .......... ok (2.1 s)
  cranking ................. ok
  warm idle ................ ok, all four injectors, spark every firing
  A/C on at idle ........... ok
  full throttle sweep ...... fuel stops at 6100 rpm
  check-engine light ....... on from 14 s
  codes .................... 23 knock sensor
NOT JUDGED: fuelling, ignition timing, anything that needs a responding engine
```

Every line is one of three things: **ok**, a **problem** stated as what was
seen, or **could not tell** with the reason. "Could not tell" is a real
outcome, not a pass.

**(my call)** `--report <file>` also writes the same result as JSON, written
by hand like the car file is read. Exit status: 0 when valid and running with
no codes, 1 when anything is a problem, 2 for a bad command line or unreadable
file, 3 when nothing is a problem but something could not be told. A "could
not tell" on any line makes its headline "could not tell" unless another line
under it is a problem.

**(my call)** An instruction the emulator does not implement, or use of an
on-chip peripheral the model lacks, is a gap in the emulator and is reported
as "could not tell", naming the address. An opcode the processor itself does
not define is a problem with the ROM.

A ROM file that is not 32768 bytes (one trailing `00` allowed, as today) is
refused with a sentence saying why; `ecu/bins/p06.bin` and `p1j.bin` are
examples.

## What would make this a failure even if it works

- **A check that can pass without the ROM having done something.** This is
  what `test` is today. Every "ok" must come from an observed output of the
  executing ROM.
- **A model tuned until the expected code appears.** Where the tool's result
  and Honda practice disagree (for example, a P30 ROM in a box without a knock
  board not blinking 23), that is a finding to investigate and write down, not
  a number to adjust until it agrees. The repo already works this way; see the
  IACV sense gain in `docs/CPU_CORE.md`.
- **Silent defaults.** A value the user did not give must be visible in the
  report.
- **"Valid" for a ROM the model cannot actually judge.** If the ROM touches
  hardware the model does not have, the answer is "could not tell", naming the
  address.

## How we know it tells the truth

These are acceptance cases. They need the ROM images, which cannot be
redistributed, so they are opt-in the way `p28-rom` and `launch-rom` are
today (feature plus environment variable; a missing image fails, it never
silently passes).

1. Stock P28 ROM, P28 manual box, matching stock car: valid, runs, no codes.
2. The same on an automatic box with an automatic car.
3. Stock P30 ROM, P30 box, matching stock car: valid, runs, no codes.
4. Random bytes: not valid, stops on an instruction or never settles.
5. Stock P28 ROM with one calibration byte changed and the checksum left
   wrong: the tool reports the repeated reset. The stock ROM resets on a bad
   checksum unless option byte `6011h` is non-zero.
6. Stock P28 ROM with each car item switched away from stock, one at a time:
   the result differs from case 1 in a way that is written down in the test
   with the reason. Where it is a code, the blink decoder reports it. An item
   that cannot change the result on that box (the knock sensor on a box with
   no knock board, say) is recorded as "no difference" with the reason, not
   forced to differ.
7. Box and car disagreeing about the gearbox, both ways round: recorded.

One case must be **explained before the tool is trusted**, because it is
exactly the kind of ROM the tool is for:

- `hondaecu/hts_D16Z6Baserom.bin` boots on the current P28 model and never
  opens an injector at 3000 rpm (`trace ... injcount 3000 2.0` prints
  `INJ 0 0 0 0`), while `P28-ERMbaseline.bin` and
  `lyonnel-2.0-launch-ok.bin`, also with non-zero checksums, inject normally.
  Either that ROM really would not fuel on a P28 box, or the board model is
  missing something it needs. The builder finds out which and writes it down.
  If it is the model, the model is fixed; if it is the ROM, it becomes
  acceptance case 8.

  Where to start: it hinges on ROM byte `6126h`. That byte is `00` in the HTS
  base ROM, `FF` in the lyonnel ROM and `D0` in the stock P28. Setting it to
  `FF` in a copy of the HTS base ROM gives 50 openings on each output in two
  seconds at 3000 rpm. What the byte means, and why `00` stops fuelling, is
  not known.

## Leads from the design review

Found by the reviewer who read this document against the tree. The first was
re-run and confirmed (above); the other two were not re-run.

- On today's P28 model `ecu/bins/p30_203.bin` latches one fault bit
  (`214h` = `80h`) after about 9 s, and `p30_usdm_203.bin` latches none.
  That bears on which image is the factory one and on what a P30 ROM misses
  on a P28 box.
- A stock image with one calibration byte flipped takes the reset path at
  0x21DE about every 5.3 s (512 blocks at roughly 97 main-loop passes per
  second), and keeps injecting between resets. So acceptance case 5 looks
  like "runs, but resets every five seconds", not "does not boot", and the
  run needs more than 5.3 s at one condition to see it.
- The emulator runs at about real time: 20.5 s of ECU time took 21.7 s. A
  full scripted run is around a minute and the acceptance cases are fifteen
  to twenty such runs. Speeding the bench up is worth planning for.

## Work this implies that is not just wiring

- **One bench.** The step loop (execute, tick timers, generate crank edges,
  dispatch interrupts) exists in `main.rs`, in `replay.rs::run_frame` and
  twice more in the test helpers. `check` needs one bench that holds the box,
  the car and the ROM, runs a scripted condition for a stretch of ECU time and
  hands observations out. The existing `stock_p28_*` tests move onto it.
- **Boot without a P28 fingerprint.** `replay.rs::runtime_ready_pc` finds the
  end of the power-on checks by matching a byte pattern from the stock ROM,
  and returns "ready" at once when the pattern is absent. `check` must decide
  that a ROM has settled from behaviour (resets stop, outputs come alive), not
  from a pattern.
- **Resets and hangs as results.** `BRK` soft resets are part of a normal
  stock boot. The bench has to count them and tell a boot sequence from a
  reset loop. The watchdog timer is not modelled as a reset source today (the
  ROM's `WDT` writes are decoded, nothing times out), so a ROM that hangs
  would simply sit there; the bench needs to notice a ROM that stops
  servicing its watchdog.
- **The knock board.** Nothing is modelled behind FC1. The ROM talks to it
  bit-serially through PC3/PC7 and port A.0..2 (0x4E00..0x4E23). It has to be
  modelled the way the other monitors were: from what the ROM checks, with
  the assumptions stated, for "fitted with a healthy sensor", "fitted with no
  sensor" and "no board".
- **The car items with no hook yet**: O2 sensor absent or one-wire, IACV of a
  different resistance, EGR, ELD, speed sensor absent, automatic gearbox
  inputs driven from the script (park/neutral, gear position).
- **The blink decoder.**
- **Whatever else differs between a P28 and a P30 box.** Only the knock board
  is known. The P30 ROM's own external accesses, run on the P28 model with the
  unmapped-access counters, are the first evidence to collect.
- **Which P30 image is the factory one.** `ecu/bins/` has `p30_203.bin` and
  `p30_usdm_203.bin`; they differ. The pgmfi wiki's ROM mask list
  (`ecu-definition-codes`) is where to look.

## Left to the builder

Module layout, names, the exact script values and durations, the wording of
the report, the preset contents, how the interactive questions are phrased,
and the order the work is done in, provided the acceptance cases hold.

README and `docs/SCENARIOS.md` are updated to describe `check` and to drop the
claims `test` made.

## Not in this project

- Judging fuelling, air-fuel ratio, ignition timing values or power **(owner)**.
- An engine that responds to the ROM (project 3).
- Saying what a ROM does differently from stock (project 2).
- Boxes other than P28 and P30 **(owner: for now)**.
- A window or web interface. It is a CLI tool **(owner)**.
- Editing or repairing ROMs.
- The TCP datalog server and `replay`, which stay as they are.

## The bigger picture

**(owner agreed)** Three projects, in this order:

1. **This one.** Valid for the ECU, runs a basic setup under scripted
   conditions, codes.
2. **Different from stock.** **(owner)** Tell the user which sensors are
   disabled and which parameters differ: a different MAP sensor, different
   injectors, whatever is not as the stock ROM has it. **(my proposal)**
   Mostly by experiment: run the stock ROM and the test ROM through the same
   situations on the bench from project 1 and compare what they do.
3. **An engine that acts like an engine.** **(owner)** When the ROM stops
   fuelling, the engine should behave as an engine would. It needs the spark
   output and a model of the engine; because the engine's real breathing is
   unknown, it assumes the engine breathes as the stock map expects and still
   does not judge fuelling.
