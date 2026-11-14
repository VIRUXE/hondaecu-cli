# ROM check

`hondaecu-cli check <rom.bin>` answers one question for a tuner: is this ROM
image worth burning to a chip and putting in the car? It runs the ROM on the
modelled P28 box (with a knock board on FC1 for a P30 box), wired to the car
described in a "car" setup, drives the box
through one fixed script of conditions (about 110 s of ECU time, up to 250 s
when the code read-out runs long; about half a minute of wall time, more with
a long read-out), looks only at what an observer outside the box could see, and
prints one of three verdicts. `test` is an alias that runs exactly the same.

It does not judge fuelling, ignition timing, or anything that needs a
responding engine. Every `ok` line comes from something the executing ROM was
observed to do; no result is a constant.

## Running it

```
hondaecu-cli check <rom.bin> [--ecu p28|p30] [--board-gearbox manual|automatic]
                             [--car <preset-name|file>] [--set key=value]...
                             [--report <file>] [--no-prompt] [--no-why]
```

The ROM is opened and size-checked first, before any question is asked. Real
dumps are messier than 32768 clean bytes, so the loader is not strict:

| file size | what is used |
|---|---|
| exactly 32768 bytes | all of it |
| 32769 to 65535 bytes | the first 32768 bytes; the rest is ignored (a stray `FF` or `00` byte, a text note from whoever dumped it) |
| exactly 65536 bytes (a 32 KiB ROM burned to a 27C512) | either half if the two are identical; otherwise the half that is not entirely `FF` or entirely `00`. Two halves holding different data are refused |
| anything else (under 32768 or over 65536 bytes) | refused, stating the size and that a ROM image is 32768 bytes |

A refusal is exit status 2. Whenever bytes were ignored or a half was chosen,
the report says so in plain words: a `ROM note:` line under the header (for
example `ROM note: ignored 1 extra byte at the end (FFh)`, `ignored 92 extra
bytes at the end`, `used the upper half of a 64 KiB image (the lower half is
blank)`) and a `rom_note` field in the JSON (`null` when there is none). The
other commands (`trace`, `replay`, `run`, `disasm`, `interactive`, `datalog`)
print the same note once to stderr. The rule is one function, `bus::rom_image`.

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

1. **Preset.** `--car stock-p28-manual | stock-p28-automatic | stock-p30`. A
   car file may name its own starting preset with a `preset = <name>` line
   (one of the three above, else a `line N:` error), and the questions ask for
   one. When no preset is named in any of these places, the base follows the
   `ecu` the other inputs resolve to, whichever gave it (`--ecu`, `--set
   ecu=`, or a car file's `ecu =` line): `stock-p30` for p30, else
   `stock-p28-manual`. So `--ecu p30` and a car file holding only `ecu = p30`
   check the same car (`check::build_setup`). Until 2026-11 a file's `ecu =
   p30` left the base at `stock-p28-manual`, whose `knock_sensor = no` made
   the factory P30 image do not burn (code 23) where `--ecu p30` said worth
   burning. `preset` is not one of the twelve values and is never listed as
   defaulted.
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
   Type a file name, or press Enter to skip: ` (not offered once the input
   has ended). Enter, `n` or `no` (any case) skips; `y` or `yes` asks `File
   name: `; anything else is the file name. An existing file is overwritten
   only after `"<name>" exists. Overwrite? (yes/no) [no]: ` is answered yes.
   A save that fails prints the error and the run goes on; the exit status
   is the verdict's. The
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
| 2 | bad command line, unreadable ROM or car file, or the `--report` file could not be written (the verdict is still printed). A car file the questions could not save does not change the status |
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
| code read-out | 40.0 to 180.0 | 800 | 35 | 0 | 0 | service connector grounded |

Engine inputs are refreshed every 10 ms of ECU time.

**How long the read-out runs.** At least 40 s. If by then the lamp has
blinked complete codes, the read-out goes on in 1 s slices until the ordered
list of complete codes shows one full cycle repeated (`blink::full_period`:
the list is periodic over at least two whole periods; the ROM blinks each
stored code once per cycle, so a cycle holds distinct codes), and stops at
180 s of read-out (`script::READ_OUT_MAX_SECONDS`) whatever it has seen. A
read-out with no complete code ends at 40 s. The codes reported are those of
one full cycle. If 180 s pass without two full cycles, the codes found are
reported and the `codes` line ends with "(the list may be incomplete: the
light was still showing new codes when the read-out ended)". Before
2026-11 the read-out was always 40 s: with five faults stored (`iacv = none`,
`eld = no`, `o2_sensor = one-wire`, `vtec = no`, `board_gearbox =
automatic`) the stock ROM stores 14, 19, 20, 21 and bit 27 (blinked 41), and
the report listed only 14, 19, 20; the five-code cycle is longer than 40 s.
It now lists all five (acceptance `five_stored_faults_are_all_blinked`).

## The lines

Two sections, `VALID FOR THIS ECU` and `RUNS A BASIC SETUP`. Every line is in
one of three states: **ok**, **problem**, **could not tell**. An instruction
the emulator does not implement is "could not tell"; an opcode the processor
does not define is a "problem". A section is a problem if any line is, else
could-not-tell if any line is, else ok. The rules below are those of
`verdict.rs`. The text report shows each line's state before its text,
`ok: `, `PROBLEM: ` or `could not tell: `, for example

```
  warm idle ................ ok: injectors opened 53, 54, 53, 53 times and 214 sparks in 53.3 engine cycles
```

and each section row its summary, `yes`, `no` or `could not tell`.

A line is ok only when its positive evidence was seen; the absence of a bad
sign is not evidence. When the run stopped (an undefined opcode or an
instruction the emulator lacks), the line of the stage it stopped in reads
"could not tell: the run stopped during this stage" and the lines of stages
it never reached "the run stopped before this": a stage cut short is not
judged as if it had run.

**Valid for this ECU**

| line | ok when | problem when | could not tell when |
|---|---|---|---|
| executes | the run reached the end without stopping | it hit an undefined opcode | it hit an instruction the emulator does not implement |
| settles after power-on | at most 16 BRKs in the first second, none later, watchdog served | more than 16 BRKs in the first second ("never settles: N resets in the first second", N counted inside that second only); a BRK after the first second (a reset: "resets at 5.3 s, 10.6 s, 15.9 s and 17 more times (program address 237Dh)", the first three times and a count of the rest); the watchdog unserved for 0.5 s | the run stopped before this could be judged |
| talks only to this board | the board was programmed and no access fell outside it | the board was never programmed, in a run that did not stop | any unmapped external read or write, or the 82C55 put in a mode the emulator lacks; the board unprogrammed in a run that stopped ("the run stopped before the board was set up") |

What `talks only to this board` covers: accesses to external addresses
the board does not decode, and the 82C55 modes the emulator lacks. **What it
does not cover:** a ROM that uses an on-chip peripheral of the MSM66207 the
model does not implement (a special function register with no model behind
it) is not detected; the design asked for "could not tell, naming the
address", which needs a map of which SFRs are modelled and was not built.
Such a ROM can pass this line and be judged on whatever the unmodelled
register returns.

**Runs a basic setup**

| line | ok when | problem when | could not tell when |
|---|---|---|---|
| car as described | (never ok; the line is absent when every car value is modelled) | never | some value is unmodelled; the line names each key and the reason |
| fuel pump prime | in `key on` the pump was on for 0.5 to 5 s and then off | otherwise | the run stopped during or before the stage |
| cranking | the engine turned and every injector output opened | an output never opened | the engine did not turn, or the run stopped during or before the stage |
| warm idle | at least 10 engine cycles; each output opened within 2 of the cycle count; ignitions at least 3 per cycle | an output opened 0 times or more than 2 away from the cycle count; too few ignitions | fewer than 10 engine cycles in the stage (`MIN_IDLE_ENGINE_CYCLES`), or the run stopped during or before it |
| idle, A/C on | the clutch was engaged (the line exists only with `ac = yes`) | it never was | the run stopped during or before the stage |
| full throttle sweep | some injector opened in the stage; the line reports where fuel stopped and where VTEC switched on. Only injector openings are watched: with no fuel stop it reads "fuel never stopped below 9000 rpm (a limiter that cuts only spark is not looked for)" | no injector opened in the stage: "no injector opened during the full-throttle sweep" (an all-FFh or all-00h chip read "fuel never stopped" here before 2026-11) | the run stopped during or before the stage |
| check-engine light | the read-out was reached, the lamp was seen lit in `key on` (the bulb check), and it is dark at the end of the drive: "never lit after the bulb check" when it never lit again, otherwise what was seen, counted by the stage each lit span started in ("off at the end of the drive; it lit 4 times during key on and once during full throttle (a start-up flash or a shift light looks like this)") | the lamp is lit at the end of the drive (also in a run that stopped inside the read-out, whose drive was whole): "never went out after power-on" when it was lit without a break from power-on to the end of the drive, otherwise "on at the end of the drive (lit since t s)" with t the start of the last lit span; or the run stopped before the read-out while the lamp was lit after the bulb check ("lit at t s and still lit when the run stopped at u s") | the run stopped before the end of the read-out with the lamp dark at the end of the drive or at the stop ("the run stopped before this"); or the lamp was never lit in `key on` ("the check-engine light never came on at key-on, so it cannot show a fault") |
| codes | the lamp was seen lit in `key on`, steady with the connector grounded, and dark at the end of the drive: "none blinked (lamp steady with the connector grounded)" | one or more codes blinked, each shown with its name from the catalogue in `dtc.rs`; or nothing blinked although the check-engine light was lit at the end of the drive ("the ROM is showing a fault it does not blink") | first, the run did not get through the read-out ("the run stopped before this", whatever the lamp showed); then, short of a problem above: the lamp never lit in `key on` (the same text as the check-engine light); the lamp blinked but no code could be read; the lamp stayed dark with the connector grounded ("the light stayed dark with the service connector grounded; every ROM seen so far lights it or blinks"); the lamp changed but no complete code could be read |

When the read-out stopped at 180 s before the blinked sequence had repeated,
the `codes` line ends with "(the list may be incomplete: the light was still
showing new codes when the read-out ended)".

The `codes` line lists the faults the ROM showed on this run, not every
fault the car has: a stored fault can change how the ROM runs the engine and
keep another check from ever running its course (on the stock P28 a stored
14 hides 17; "One code hides another" below).

**The bulb check must be seen.** `RunResult::bulb_check_seen` is true when
the board log shows the lamp lit at some point in `key on`. Without it a
lamp that stays dark is not evidence of anything, so both lamp lines are
could not tell, "the check-engine light never came on at key-on, so it cannot
show a fault", unless a problem was actually observed (codes blinked, or the
lamp lit at the end of the drive). Until 2026-11 nothing required it: the
final review took the stock P28-230 image, replaced its only two lamp writes
(`MB P1.4, C` at 0x36CA, the read-out display, and 0x42AE, everything else;
3 bytes each in the listing) by NOPs and fixed the byte sum at 0x7FFF, and
`check` said WORTH BURNING with `iacv = none`, `eld = no` and `o2_sensor =
one-wire` with `vtec = no`, where the unpatched ROM stores 14, 20 and 41 + 21:
"never lit after the bulb check" and "none blinked" were both ok for a lamp
that never lit at all. An all-FFh and an all-00h chip also showed both lamp
lines ok. The same patched image, derived in the test, is now COULD NOT TELL
with code 14 stored (acceptance
`a_rom_that_never_lights_the_lamp_is_not_worth_burning_on_a_car_where_it_stores_a_fault`).

**A dark read-out is could not tell.** Every ROM run so far lights the lamp
with the connector grounded: steady with nothing stored, blinking otherwise
(and steady with code 24 on the P30 and P72 code, which is caught by the
lamp lit at the end of the drive). The earlier rule took a dark read-out as
"none blinked", ok, on the guess that a tuned ROM might switch the display
off; the reviewer set the stock ROM's option byte 6011h (read at 0x36BF) to
FFh and the read-out was still steady, so that guess has no evidence, and a
dark read-out is now "could not tell".

The bulb check is the lamp lit from power-on until the first lit-to-dark edge
in `key on`. A lamp that is still lit at the end of `key on` without having
gone dark never ended its bulb check: it counts as lit after the bulb check,
at the end of `key on` (`script::mil_lit_after_bulb_check`), and, if it stays
lit to the end of the drive, the line reads "never went out after power-on"
(a lamp that went out later and lit again reads "on at the end of the drive
(lit since t s)"; until 2026-11 such a lamp read "never went out after
power-on (still lit at 6.0 s)", naming only the end of `key on`). Before this rule
a fault the ROM stores inside the first two seconds held the lamp lit from
power-on and the line read "never lit": the stock P28 with `vtec = no` (code
21 stored at 1.2 s) and on an automatic board in a manual car (code 19 at
0.9 s) were judged do-not-burn on their codes only. Both now fail on this
line. (The P30 image on a P28 box does not depend on the rule: its lamp goes
out at 2.11 s and lights again at 2.21 s, "on at the end of the drive (lit
since 2.2 s)".)

**The end of the drive** is the end of the last stage before `code
read-out` (`warm idle again`). The lamp's lit spans after the bulb check
(`RunResult::mil_lit_spans`, from the board log; a span still open at the
read-out ends there) and its state at the end of the drive
(`mil_lit_at_end_of_drive`) come from `script::lamp_after_bulb_check`. The
lamp counts as lit at the end of the drive when it is lit at that instant
or was lit for more than half of `warm idle again`: one instant would make
a lamp blinking then a coin flip. A stored fault keeps the lamp
lit: every fault seen so far (codes 14, 17, 19, 21, 23, 24, 41) holds it lit
to the end of the drive, and code 20 does not light it at all but is
blinked. A ROM's own lamp display goes dark again, so a lamp that lit and
went out is reported but is not a problem. Until 2026-11 any lit edge after
the bulb check was a problem ("lit at t s, after the bulb check"); that read
the lyonnel ROM's start-up flashes and shift light as a fault ("The three
do-not-burn results, traced"). Residual risk: a ROM that stores a fault,
lights the lamp, and then puts it out again without blinking the code would
now pass this line; none has been seen.

A steady lamp at the read-out is not proof that nothing is stored. The stock
P28 and P30 ROMs show it when nothing is stored (0x36C9, below), and the P30
ROM also shows it with code 24 stored: with `214h.7` set the read-out routine
skips the blink sequence (0x3634) and holds the lamp lit. So the `codes` line
says only "none blinked", and when the check-engine light is lit at the end
of the drive a quiet read-out is a problem: "none blinked, although the
check-engine light is on (the ROM is showing a fault it does not blink)".

Lamp reading (`blink.rs`): with the connector grounded the ROM flashes each
stored code; a blink lit at least 0.82 s is a tens digit and a shorter one is
a ones digit, and a dark gap of at least 2.0 s ends a code. A lamp lit without
a break for at least 10 s at the end of the window with no complete run in it
is `Steady`. `blink::complete_runs` gives the complete codes in the order
blinked, and `blink::full_period` finds the cycle once it has been seen twice
("How long the read-out runs", above).

### The three verdicts

- **WORTH BURNING** (exit 0): every line is ok.
- **DO NOT BURN** (exit 1): at least one line is a problem. The verdict names
  the first one, in the order printed.
- **COULD NOT TELL** (exit 3): nothing is a problem, but at least one line is
  could-not-tell. The verdict names the first one.

A problem always wins over a could-not-tell. A line's `ok` means only that
what the script looked at looked right; the `NOT JUDGED` line says what it
did not look at.

## Why the light is on

When the lamp is lit at the end of the drive (`mil_lit_at_end_of_drive`),
the text report ends with a section `WHY THE LIGHT IS ON`, and the JSON
report gains `why_the_light_is_on`, an array of levels (`at`, `address`,
`instruction`, `did`, `path`, `followed`, `others`, `stopped_because`,
`copy`). It answers "where does the ROM decide to light the lamp, and on
what": the instruction that drove the lamp bit, then each cause behind it,
with addresses, disassembly, values and times, back to an input from outside
the CPU or a constant in the ROM. It is an **automatic trace**: at each level
a rule of thumb picks what to follow, and the section says so. It adds no
line, and it cannot change the verdict or the exit status. `--no-why` skips
it. A run whose lamp is dark at the end costs nothing extra.

`why.rs` holds it. It knows no ROM's addresses: it works on any image the
emulator runs.

### How it is traced

**Provenance.** While a `Tracer` rides on the bench (`Bench::tracer`), the
bus logs the data reads and writes of the instruction executing
(`Bus::access`), and after each instruction the tracer records, for every bit
of on-chip data memory (`0x0000..0x047F`), the instruction that last changed
it, the one that last wrote it, and the last such write and change that was
not the byte counting on itself (a `DEC`, a shift, an `XCHG`): for a counter,
its last load. Times are ECU time; instructions are numbered by
`Cpu::instructions`. Without a tracer the bench pays one test per instruction
and the bus one per access; the plain `check` run is as fast as before.

**The path.** Near a chosen instruction the tracer also keeps a ring of the
last 512 instructions (`RING`): address, decoding context, whether a
conditional branch was taken, the registers and flags it reads and writes
(`registers`), and the data it read with each bit's provenance at that
moment. The brief suggested 64; the IACV and fault-store chains below need
the reads of a whole fault-store pass (a loop over a dozen fault bits) or of
an IACV controller pass with two calls to a clamp routine, which do not fit
in 64. Only the instructions before the chosen one are kept, so the cost is
a few megabytes at most, not a whole run.

**The walk.** The emulator is deterministic, so `explain` re-runs the same
drive (same ROM, same car) and pauses at the instant it wants:

1. The first run goes to the end of the last lit span (the start of the
   read-out) and finds the last instruction that set bit 4 of P1, the ROM's
   image of the lamp bit (82C55 port C bit 4), or failing that one that wrote
   port C with bit 4 set, in the last second. The last *write* is used, not
   the last change: a ROM that lit the lamp in the bulb check and keeps it
   lit for a fault changes the bit at power-on and rewrites it every pass
   after. That instruction is level 1.
2. At each level the path into the instruction is cut to what belongs to it:
   interrupt handlers that ran in between are left out (and a level inside a
   handler stops at its entry); calls and returns are followed through.
3. A backward **slice** marks (`*`) the instructions the level's instruction
   depends on: one that writes a register, flag or byte still needed, and a
   conditional branch whose other way could have avoided the path or changed
   what it computed. Whether a branch matters is decided by following every
   route of the code from its other way (`rejoins`): it does not matter when
   all routes come back to the path without skipping a marked instruction or
   setting a needed register; a route into `BRK` (a sanity check resetting
   the CPU) does not count; inside a routine the path returns from, a return
   goes back where the path returned.
4. **What to follow** (`choose`). Among the data the marked instructions
   read: an input from outside the CPU ends the chain (an external address,
   an A/D result, the input pins of a port, the transition detectors, the
   interrupt requests; named from the board model where it has a name).
   Otherwise on-chip data, followed to its source: for a register of the
   local bank (r0..r7, scratch) its last write, for anything else its last
   change, and for a byte that counted on itself since, its last load. The
   order: data written by another routine first, the routine's own
   bookkeeping (written by code within 40h bytes of an instruction of the
   path) after it, a write of a bare constant (a loop index set up) last;
   within each, the read nearest the instruction first, then the most recent
   source. Otherwise a ROM constant read by `LC`/`LCB`/`CMPC` ends the
   chain. A pure copy (`MOV`/`L`/`ST` and nothing else marked) is shown but
   does not count toward the depth.
5. The next level re-runs to the instruction followed. A choice whose chain
   ends without an input or a ROM constant is given up for the next
   candidate (three per level, 24 re-runs in all), and the level says what
   was tried. The walk stops at six levels.

Every level prints the instruction and its time, what it did, up to twelve
instructions before it (and up to six earlier marked ones), the reads and
writes with resolved addresses, what was followed, and what else was read.
`HONDAECU_WHY_DEBUG=1` prints every level tried, abandoned ones included, on
standard error.

### What it gave on the images at hand

- **Stock P30 (mask 203) on a P28 box**: `42AEh MB P1.4, C` (218h.5 read at
  `4284h`), `4AFAh SB 218h.5` (from 214h at `4AF3h`), `51C5h SBR 212h[X1]`
  (fault 24 stored at 2.19 s; followed 9Ah bit 3 read at `4991h`),
  `2824h MB 9Ah.3, C` at 0.16 s, stopped at the read of `0F00h` bit 2 at
  `2800h`, the knock-board heartbeat. The level also lists 2E8h, "set by
  2818h at 0.057 s, then counted on by itself": the countdown and its only
  reload. This is the chain traced by hand.
- **Stock P28 with `iacv = none`**: `42AEh`, `4AFAh`, `51C5h` (code 14),
  `3C71h MB 99h.3, C` at 7.53 s (the IACV current check; followed 2BDh read
  at `3C6Ch`), `28CCh STB A, 2BDh` (the IACV controller: the on-time clamped
  at a limit; it first tried the target current 25Ah, whose source `31C8h`
  leads nowhere), `0138h ST A, er2` in the timer-1 handler, stopped at the
  A/D result of AI4, the IACV coil current.
- **P07-225**: `38B3h MB P1.4, C`, `3EFEh` (216h.5), `44F3h` (fault stored
  at 5.16 s in 212h bit 5), `358Bh MB 0B2h.1, C` at 0.04 s (from 22Fh bit 4),
  `452Ah STB A, 22Fh`, stopped at the read of `0F00h`, 82C55 port A, as a
  whole byte (`XORB A, #037h` between: bit 4 inverted).

### Limits

- The choice is a heuristic built on these three chains. The rejected
  rules, for the record: the most recent change first followed the fault
  store's own bookkeeping (0EAh) on the IACV image; a flag set by a bit
  instruction first followed the IACV check's enable gate (`217h.5`, the end
  of cranking) instead of its trigger. Gates and triggers both decide a
  check; the trace prefers the last test before the decision. Where it goes
  elsewhere, the "also read" list names the alternatives with their sources
  so a person can follow another by hand.
- Data passed in CPU registers (A, the flags) is followed only inside the
  path; a value that sits in A across more than the ring is lost.
- Writes the hardware makes (A/D results, timers, request flags) have no
  instruction behind them: such reads are inputs or, for timers, timer
  control, the serial unit and the PWM counters, are listed but not
  followed. An interrupt's own stack frame is not attributed.
- A byte read whole is followed to the byte's last change (the bit with
  the latest source decides). A bit is named only when that change was a bit
  instruction, which changed that bit alone: "214h (= 80h), read whole at
  4AF3h; the last of its bits to change was bit 7, 1 at that read, set by
  51C5h". A byte last written whole (a load, a store, a countdown) names
  none: "2E8h (= 01h), read at 5210h, set by 2818h at 0.057 s, then counted
  on by itself". Until 2026-11 every byte read whole named the bit the
  search landed on, "(= 01h; its bit 1 is the one followed)", although for a
  whole write that bit is arbitrary and here was 0 in the value shown.
- A whole-byte input names the byte; when the level was entered for one
  named bit, the stop says which bit that was, as a lead to check, not a
  finding.
- The re-runs cost time: about 30 to 60 s more on top of the run on the
  images above.

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
- **Read-out length.** At least 40 s, then 1 s slices until the blinked
  cycle has been seen twice, at most 180 s (`script.rs`). A ROM whose code
  cycle is longer than 90 s would be cut at 180 s and its list flagged as
  possibly incomplete.
- **Lamp at the end of the drive.** Lit at the end of `warm idle again`, or
  lit for more than half of it (`script.rs`).
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
verdict `Steady` means no code blinked, which is not the same as no code
stored (the P30 ROM holds the same steady lamp with code 24 stored; "The
bulb check" above and "The P30 box"). A dark read-out (`Off`) is could not
tell ("A dark read-out is could not tell", under "The lines").

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

### One code hides another: a stored 14 hides 17

`--set speed_sensor=no` blinks 17; `--set iacv=none --set speed_sensor=no`
blinks only 14, and only 14 is stored. Fault 17 is **never raised**: `99h.4`
stays clear for the whole run, so the debounce pass (0x49A0..0x49D9, one
shared slot `2A7h`/`2C1h`) never sees it. Nothing is raised and then dropped
or stored and not blinked.

What stops it is the fuel-cut decision at 0x17D0..0x18BD. With code 14
stored (`113h.5`, set with `213h.5` at 0x51C1), or the IACV check's flag
`99h.3` raised, it does not take the normal route (0x17DF, 0x1803):

```
17D0  RB   PSWL.4               ; deceleration fuel cut off
17D2  RB   PSWL.5               ; the other fuel cut off
17D7  JBS  113h.5, 17E5         ; code 14 stored
17DA  MB   C, 099h.3 ; JLT 17E5 ; or the IACV fault raised now
17E5  JBS  112h.6, 17FB         ; code 7 (throttle sensor) stored: 80h
17E8  MB   C, 098h.2 ; JLT 17FB
17ED  CMPB 0B9h, #2Bh ; JGE 1803 ; throttle byte (AI7) at least 2Bh: normal route
17F3  MOV  X1, #6B30h ; VCAL 2   ; limit by throttle: 2Bh -> 62h, 6Bh -> A9h
17FD  CMPB A, 12Dh ; JGE 1856   ; rpm code at or below the limit: no cut here
1801  SJ   1846
1846  SB   PSWL.5               ; above it: fuel cut, 11Ch.5 (0x18B8)
```

So with the throttle closed the ROM cuts fuel whenever the rpm code `12Dh`
is above 62h (released at about 1530 rpm on the script's sweep), through
`11Ch.5`; it never reaches 0x18B1 `SB PSWL.4`, the only place the
deceleration fuel-cut flag `11Ch.4` is set (copied at 0x18BD, then to
`21Ch.4`). At or below 62h it goes to 0x1856, which needs `126h.2` (rpm
code above 73h, 0x17C4..0x17CD) to go on towards 0x18B1, so it ends at
0x185C instead. The VSS check takes its carry from `21Ch.4` (0x3C88 `MB C,
21Ch.4`, stored at 0x3C8B), so with code 14 stored it can never raise
`99h.4` at closed throttle. Observed on the two runs, `lift off`: without
the IACV fault 0x18B1 ran 1995 times, `21Ch.4` was set, `99h.4` raised from
about 3500 rpm and code 17 stored at 58 s; with it 0x1846 ran 1985 times,
0x18B1 never in the whole run, `21Ch` read 6Ch/6Dh (bit 5 set, bit 4
clear), and `98h..9Bh` stayed 0. Code 14 was stored at 12.7 s, long before
`lift off`.

**This is the ROM's behaviour**, not the script's or the board's: the path
depends on RAM the ROM writes itself (`113h.5`, `11Ch`, `21Ch`) and on the
throttle byte and engine speed, which in `lift off` are what a real car
coasting with the throttle shut gives. A real car with both faults would
also show only 14, until code 14 is cleared and the valve repaired; on the
next drive 17 would appear. Whether the normal route can set `11Ch.4` with
the throttle byte at 2Bh or more (throttle partly open) was not traced.
Acceptance `a_stored_iacv_fault_keeps_the_rom_from_seeing_a_missing_speed_sensor`
pins it: the codes, then 8 s held at 3000 rpm, closed throttle, 25 kPa, in
gear, in which 0x3C88 is reached (every other gate open), 0x1846 runs,
0x18B1 never does and the carry stored at 0x3C8B is never set. The same
hold without the IACV fault (and with 17 cleared) reaches 0x18B1 and raises
`99h.4`.

**What this means for a report.** The `codes` line lists the faults the ROM
stored and blinked on this run, not every fault the car has. A stored fault
can change how the ROM runs the engine (here, fuel cut) and so keep another
check's conditions from ever being met; that fault then does not appear
until the first is cleared.

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
| `stock-p30`, `ecu = p28` (no knock board) | do not burn: MIL lit again at 2.2 s, 0.1 s after the bulb check ended | stored: **24** (fault bit 23, `214h.7`); blinked: none | port A bit 2 never changes, so 0x27F7..0x2824 never reloads `2E8h` and sets `9Ah.3` from its first pass (0.06 s); code 24 is stored at about 2.2 s. With `214h.7` set the read-out routine skips the blink sequence (0x3634 `JBS 214h.7`) and holds the lamp lit, so the grounded connector shows what looks like the no-code display; the `codes` line reads "none blinked, although the check-engine light is on" and is a problem. Same result whatever the pull-ups give on bits 0..2 | **disagrees**: practice expects code 23. The ROM stores 24, which it never blinks, and keeps the MIL lit |

The last row is the ROM's own behaviour; the model was not changed to bring
out code 23.

**Residual risk in `knock_sensor = no` on a P30 box.** The board's answer to
an unplugged sensor (frame bit 8) is inferred, not measured: bit 8 is the
ROM's only path to code 23, and code 23 is documented as covering an
unplugged sensor. The stock run therefore shows that the ROM's path works,
not that a real board takes it. For a tuned ROM with the 0x3D44 check
disabled, `ecu = p30`, `knock_sensor = no` is judged on that assumed board
behaviour: if a real board signalled the missing sensor some other way (in
the level bits, say, or by changing its heartbeat), such a ROM could react
to it differently on the car than on the model.

A P30 ROM in a P28 box keeps the check-engine light on and
answers the service connector with a steady lamp, which a driver would read
as "no code stored".

## HTS base ROM

`hondaecu/hts_D16Z6Baserom.bin`, the Honda Tuning Suite D16Z6 base ROM
(SHA-256 `3f1ba3ea...300e3d0`; its bytes sum to E3h, not 0, and it does not
reset for it). The design recorded that it "boots on the current P28 model
and never opens an injector at 3000 rpm" (`trace ... injcount 3000 2` prints
`INJ 0 0 0 0`), and that setting ROM byte `6126h` from `00` to `FF` (the
lyonnel ROM's value) makes it inject. Neither of the two explanations the
plan offered holds: the ROM fuels on the model, and the model was not
changed. What stopped the injectors is the ROM's **launch control**, which
the `trace` bench sets off by spinning the engine at 3000 rpm with the
throttle at 65 % from the instant of power-on.

**`check` says worth burning** (`stock-p28-manual`): fuel pump 2.1 s, all four
injectors in cranking, `warm idle` 54/53/53/53 openings in 53.3 engine
cycles, fuel stops at 7420 rpm and VTEC at 4755 rpm in `full throttle`, no
code. Acceptance `hts_base_rom_runs` (feature `hts-rom`).

**What `6126h` is.** The HTS code differs from the stock P28's here (the
stock ROM's byte at `6126h` is calibration data, `D0`). HTS's rev limiter,
0x186B..0x18A4 (the stock one is 0x1803..0x1846):

```
1871  MB   C, P4.0
1874  JLT  187C
1876  JBS  11Bh.7, 187C
1879  CAL  5B45              ; HTS: limit in A, resume in DP (below)
187C  JBR  124h.5, 1880      ; already cutting: compare with the resume value
187F  L    A, DP
1880  CMP  0C4h, A           ; C4h = engine period, rpm = 1875000 / period
1883  LCB  A, 6125h          ; FFh
1887  JEQ  189A
1889  MOV  DP, #0E6h
188C  LCB  A, 5F88h          ; 00h
1890  JEQ  1896
...
1896  JGE  189A
1898  SB   [DP].0            ; E6h.0: spark cut (0x019B, 0x01E9, 0x0251 do not
                             ; arm the ignition compare while it is set)
189A  JGE  18A6              ; under the limit
189C  LCB  A, 6126h
18A0  JNE  18E4              ; 6126h non-zero: no fuel cut
18A2  SB   PSWL.5            ; -> 124h.5 (0x18F6): fuel cut
```

So `6126h` decides whether this limiter cuts fuel as well as spark: `00`
cuts both, anything else spark only. The period scale is eCtune's
(`Calibration.PeriodToRpm`, 1875000 / period; C4h read 0271h = 625 at
3000 rpm).

**The launch control.** 0x5B45 returns the limit. With RAM `409h.3` set it
is 3A9h (937, **2001 rpm**), the larger period of that and the normal limit
(`192h`); the resume value is the limit plus the word at `6029h`. `409h.3`
is the launch flag, written at 0x5BBB..0x5BFB:

- off (cleared) when option byte `6150h` is 00h; it is FFh in this ROM
  (00h in lyonnel, which installs its own launch routine at 0x5F14 instead,
  called from 0x1879);
- with `615Dh` non-zero (FFh here) it is set whenever the routine runs with
  `222h.3` set, with no rpm or throttle gate (0x5BC9). `222h.3` is written
  at 0x3E0B by 0x3DE2..0x3E0B (skipped while `40Eh.4` and `221h.6` are both
  set). It is set in three cases:
  - `218h.6` set (0x3DEC). 0x4532..0x4541 computes it, reached from 0x451E
    only when `218h.7` is set (a stored fault in `212h..215h`, or `2EDh.3` /
    `2EEh.4`), as `(212h & FFFDh) != 0 || (214h & 14F5h) != 0`; 0x4520
    clears it otherwise. So some stored faults set it.
  - `230h.6` set (0x3DF9). It is written at 0x2978 from a countdown at
    `397h` that `TRNSIT.3` reloads (0x2967..0x297B); what `TRNSIT.3`
    detects was not traced.
  - otherwise, `2C6h` non-zero (0x3E06).

  Observed, on a fault-free run: `222h.3` set from power-on and still set
  after 1 s at 0 rpm; cleared 0.09 s after power-on with the engine turning
  at 3000 rpm; clear after 2 s of idle. The flag was not set during 1 s at
  0 rpm, and was set on the first pass after the engine began to turn
  (0.0065 s later). Which of the three cases held `222h.3` there was not
  traced;
- otherwise it is cleared when the period is above 3A9h (below 2001 rpm) or
  throttle `D1h` is below 60h (D1h read 56h at 30 % and 9Eh at 65 % TPS on
  the model), kept if already set, and else set only when `BBh` reaches
  `6151h` or `615Eh` (chosen by coolant `D9h` against `615Fh`). `BBh`
  follows MAP (2Ah at 30 kPa, 5Dh at 90, 66h at 100) and both thresholds
  are FFh in this ROM, so that second way in is closed on an unboosted
  engine: the tuning tool's values to fill in.

So in this base ROM, on a fault-free run, launch control was seen to arm
only as the engine first turns; it then holds the engine at 2000 rpm, by
fuel and spark cut, until the revs fall below 2000 or the throttle closes
below the D1h gate. That this is the only way in is not established.
Whenever `222h.3` is set the flag is set at any rpm, and `222h.3` also
follows `218h.6` (a stored fault in the bitmaps above) and `230h.6` (whose
trigger was not traced). A running car with such a fault would then likely
meet the 2001 rpm fuel-and-spark limit while driving. That was not run.
Observed on the bench
(acceptance `hts_base_rom_launch_control_cuts_fuel_above_2000_rpm_from_a_standstill`,
90 kPa, 88 C, standing still, from power-on unless said):

| bench | openings per output in 2 s | `409h.3` |
|---|---|---|
| 3000 rpm, 65 % TPS | 1 (the flag is first set 0.06 s after power-on) | set |
| 2100 rpm, 65 % TPS | 1 | set |
| 1900 rpm, 65 % TPS | at least 25 | clear |
| 3000 rpm, 30 % TPS | at least 40 | clear |
| 2 s at 800 rpm, then 3000 rpm, 65 % TPS | at least 40 | clear |
| 3000 rpm, 65 % TPS, `6126h` = FFh | at least 40 (spark cut only) | set |

A car cannot be at 3000 rpm with the throttle open in the first tenth of a
second after the key is turned, so a real P28 box would fuel this ROM: the
zero-injection result was the `trace` bench's start (engine already at the
asked rpm before the ROM's first pass), not the ROM and not the board
model. `6126h` = `00` is not a defect: cutting fuel on a rev limiter is
what the stock ROM does too. Setting it to `FF`, as earlier work did, did
not "fix" anything; it moved the limiter to spark only, so the injectors
counted on the bench kept firing while the launch limiter was still on.

**For a user of `check`.** The report for this ROM is true as printed: the
script cranks at 300 rpm and idles before it goes above 2000 rpm, so
launch control never meets a stage, and nothing in the report claims it
was checked. `check` does not exercise launch control at all (a standing
start with the throttle held above 2000 rpm is not in the script); that is
one of the things the `NOT JUDGED` line covers only in general. Nor would
it reveal a launch limit triggered by a stored fault (`218h.6`, above): the
script stores none on this ROM, so a car with a fault in those bitmaps could
meet a 2001 rpm limit that `check` never shows. Anyone
using `trace` on a ROM with launch control should idle the engine first,
or read a dead output at speed as possibly the launch limiter.

## Results on the images at hand, 2026-11

Every 32 KB image in the workspace through `check --no-prompt`, release
build of the commit that adds this section, paths relative to
`hondaecu-cli`. The P30 images were run on both boxes, the others on the
default P28 box with `stock-p28-manual`. Recorded, not acted on: none of
these runs changed the model or the verdict rules. The lamp rule changed afterwards ("The end of the drive" under "The lines"), and the final review changed the report's wording (each line now shows its state; the reset list is cut after three times); the rows whose text changed show the current text. No verdict in this table changed with the final review.

| command | exit | verdict line | first problem or could-not-tell line |
|---|---|---|---|
| `check ../P28-230.bin` | 0 | `VERDICT: WORTH BURNING` | none (fuel stops at 7420 rpm, VTEC at 4875 rpm) |
| `check ../../ecu/bins/p30_203.bin --ecu p30` | 0 | `VERDICT: WORTH BURNING` | none (fuel stops at 8305 rpm, VTEC at 5685 rpm) |
| `check ../../ecu/bins/p30_203.bin --ecu p28` | 1 | `VERDICT: DO NOT BURN - check-engine light: on at the end of the drive (lit since 2.2 s)` | `check-engine light ....... PROBLEM: on at the end of the drive (lit since 2.2 s)` |
| `check ../../ecu/bins/p30_usdm_203.bin --ecu p30` | 0 | `VERDICT: WORTH BURNING` | none (fuel stops at 7505 rpm, VTEC at 5315 rpm) |
| `check ../../ecu/bins/p30_usdm_203.bin --ecu p28` | 0 | `VERDICT: WORTH BURNING` | none (same figures as on the P30 box) |
| `check ../../ecu/bins/p72.bin` | 1 | `VERDICT: DO NOT BURN - check-engine light: on at the end of the drive (lit since 2.2 s)` | `check-engine light ....... PROBLEM: on at the end of the drive (lit since 2.2 s)`. True for a P28 box: knock enabled, no knock board, code 24 ("The three do-not-burn results, traced", P72) |
| `check ../test-roms/P28-ERMbaseline.bin` | 1 | `VERDICT: DO NOT BURN - settles after power-on: resets at 5.3 s, 10.6 s, 15.9 s and 17 more times (program address 237Dh)` (20 resets, every 5.3 s) | `settles after power-on ... PROBLEM: resets at 5.3 s, ...`. Since the final review its `codes` line is `could not tell: the light stayed dark with the service connector grounded; ...`: the lamp stays dark through the read-out, in which the ROM resets every 5.3 s (not traced further; the verdict does not depend on it). True: the ROM's own checksum fails (sum F9h, debug byte 547Eh = 00h); behind it, the P72 code's knock check, as for `p72.bin` (same section, ERM baseline) |
| `check ../test-roms/lyonnel-2.0-launch-ok.bin` | 0 | `VERDICT: WORTH BURNING` | none. `check-engine light ....... ok: off at the end of the drive; it lit 4 times during key on and once during full throttle (a start-up flash or a shift light looks like this)`. No fault is stored; the lamp is a key-on flash display and a shift light. Until the lamp rule changed this was `DO NOT BURN - check-engine light: lit at 2.7 s, after the bulb check` (same section, lyonnel). Same result with `--ecu p30` |
| `check ../hts_D16Z6Baserom.bin` | 0 | `VERDICT: WORTH BURNING` | none (fuel stops at 7420 rpm, VTEC at 4755 rpm) |
| `check ../../ecu/bins/p06.bin` | 0 | `VERDICT: WORTH BURNING` | none. 32769 bytes; `ROM note: ignored 1 extra byte at the end (FFh)` (before the loader took any size but 32768 or 32769 with a final `00`, this file was refused) |
| `check ../../ecu/bins/p1j.bin` | 1 | `VERDICT: DO NOT BURN - check-engine light: on at the end of the drive (lit since 2.1 s)` | `ROM note: ignored 92 extra bytes at the end` (a 92-byte text note after the ROM; was refused as 32860 bytes) |

No run ended `COULD NOT TELL`. Things these runs show that nothing here
explains yet:

- **`p30_203.bin` on the P30 box** now reads fuel stop 8305 rpm and VTEC
  5685 rpm; "What the stock P30 does" above gives 8310 and 5690, which is
  what the same image shows today on the P28 box. 5 rpm is one step of the
  sweep; which change moved it was not traced.
- **`p72.bin`**, **`P28-ERMbaseline.bin`** and **`lyonnel-2.0-launch-ok.bin`**:
  traced in the next section.
- **`lyonnel-2.0-launch-ok.bin`**'s `full throttle sweep` line read "fuel
  never stopped below 9000 rpm": true, but its limiter does not cut fuel
  (`6126h` = FFh, see "HTS base ROM"). A `trace` snapshot found the
  spark-cut flag `E6h.0` set at 7500 rpm but clear at 8500 rpm, and lyonnel
  replaces the call at 0x1879 with its own routine at 0x5F14, so where (or
  whether) its limiter cuts spark is not traced. A tuner could have read
  that line as "no rev limiter", which is not established either way. The
  line now says that a spark-only limiter is not looked for.

## The three do-not-burn results, traced

Each image was run through `check --no-prompt` as in the table above, then
on the P30 box (`--ecu p30`, knock sensor fitted and not), with RAM read at
the end of the run and, where needed, a bench run watching program
addresses. Disassembly of the two `test-roms` images is from their `.asm`
listings, checked against `hondaecu-cli disasm`. Acceptance cases:
`erm_baseline_resets_on_its_own_checksum`,
`erm_baseline_with_a_good_checksum_needs_a_knock_board` (feature `erm-rom`),
`p72_needs_a_knock_board` (`p72-rom`) and
`lyonnel_flashes_the_lamp_and_uses_it_as_a_shift_light_and_is_worth_burning` (`launch-rom`). No model or
emulator file changed.

| image | `check` said | what the ROM does | verdict for the setup run |
|---|---|---|---|
| `P28-ERMbaseline.bin` | resets every 5.3 s | its own checksum fails and it resets itself; behind that, the knock check of `p72.bin` | **true** for the file as distributed, on any box |
| `p72.bin` | lamp lit at 2.2 s, nothing blinked | knock control enabled; no knock board on a P28 box, code 24 stored | **true** for a P28 box; worth burning on a box with a knock board |
| `lyonnel-2.0-launch-ok.bin` | lamp lit at 2.7 s, nothing blinked | stores no fault; the lamp gives four pulses after the bulb check and is a shift light above 8408 rpm | **was wrong**: the old lamp rule took a display for a fault. With the rule changed (lamp judged at the end of the drive) it is worth burning |

### ERM baseline: its own checksum

`P28-ERMbaseline.bin` is P72 code, not P28-230 code: it differs from
`p72.bin` in 1579 bytes, mostly tables and option bytes, some at the end of
the erased area 0x6761..0x7C49 (the README in `test-roms` places UberData's
launch code at 0x6E00), plus a few code changes (for example a jump at
0x2173 into new code at 0x6576). The vectors (0x0000..0x0037), the reset
and checksum code (0x2360..0x2A2F), the read-out code around 0x3529 and the
option copy at 0x5350..0x538F are byte for byte the P72's. The stock P28 addresses in the brief
(0x282A..0x2866, 0x2855, RAM `EBh`, option `6011h`) do not apply; the same
mechanism is here:

```
29E4  CLR X2 ; MOV er0, 39Ah[X2]      ; er0 = block number 0..1FFh
29EA  ... MUL ; MOV X1, A             ; X1 = block x 64
29F0  MOV DP, #20h ; MOVB r0, 39Ch[X2]  ; 32 words; r0 = running sum
29F7  LC A, [X1] ; ADDB A, ACCH ; ADDB r0, A ; INC X1 ; INC X1 ; JRNZ DP, 29F7
2A06  INC 39Ah[X2] ; CMP 39Ah[X2], #200h ; JNE 2A2A   ; not the last block
2A12  CLR 39Ah[X2] ; LB A, r0 ; JEQ 2A2A               ; sum 0: fine
2A19  CLRB 39Ch[X2] ; LCB A, 547Eh ; JNE 2A2A          ; 547Eh non-zero: ignore
2A23  MOVB F5h, #48h ; J 237A
237A  SB B7h.1
237D  BRK
```

One 64-byte block per main-loop pass, 512 passes for the 32 KB; the 8-bit
sum of all of it must be 0, unless option byte `547Eh` is non-zero. The
pgmfi wiki's P72 page names `547Eh` "Debug Mode (0xFF enables, 0x00
disables)". `F5h` is the reset reason the boot code reads after the BRK
(0x2393): 44h from the watchdog vector (0x236D), 45h from the unused
interrupt vectors (0x2367), 4Dh and 4Fh from two run-time checks (0x2C0F,
0x366F), 48h from this one.

In the image as distributed the bytes sum to **F9h** and `547Eh` is **00h**.
Observed on the model: 0x2A23 runs 5.29 s after power-on, the BRK at 0x237D
follows within a millisecond, and no other path into the reset code runs;
then the same after every pass, 20 times in the script, 5.29 s apart. The
path depends on nothing the board model supplies: the ROM reads its own
code memory with `LC` and adds bytes. A real MSM66207 executing this image
sums the same bytes and resets once per full checksum pass (5.3 s on the
model; the period on a car depends on its main-loop rate, which was not
measured), so **"do not burn" is true of this file as distributed**.

The control for the emulator's summation: `p72.bin`, the same checksum code,
sums to 0 and settles with no reset (`p72_needs_a_knock_board`), and
correcting one byte of the ERM image (below) stops its resets
(`erm_baseline_with_a_good_checksum_needs_a_knock_board`). So the emulated
sum is the image's byte sum, and the reset follows from it alone.

Nothing in the workspace says why the distributed file is in this state; a
likely reading, not checked, is that it is meant to be opened in UberData,
which would write a correct checksum when it saves. Setting `547Eh` instead
is not an equivalent fix: the byte is also read at 0x3539, 0x35BA, 0x408B,
0x4279 and 0x53A7, which were not traced. Burned as downloaded, it resets
once per checksum pass.

The `idle, A/C on` problem in the same run is a side effect: a reset fell in
that stage. With the sum corrected (one byte of the erased fill, 0x7000, from
FFh to 06h) the resets stop and the A/C clutch is engaged; what remains is
the next finding, as for `p72.bin`: lamp lit at 2.2 s, code 24 stored, nothing
blinked, do not burn on the P28 box; on the P30 box with a knock sensor,
worth burning. So the tuner's real fix is two edits in the tuning tool:
a correct checksum and, for a P28 box, knock control off.

### P72: knock control on, no knock board

The P72 code carries the P30's knock checks, moved: option byte `5467h`
(FFh in both P72-code images; the wiki's P72 page: "Knock Sensor
(enable=FF/disable=00)", RAM `227h.6` "Knock Sensor Enable Flag") is copied
into `227h.6` at 0x535E, and 0x29A5..0x29D2 is the P30's heartbeat check at
0x27F7 with its RAM moved: port A bit 2 against `232h.6`, the countdown at
`2FCh` (the P30's `2E8h`) reloaded with 0Ah unless the frame is bad and
`C5h` (the P30's `ADh`) is at most 0Dh, the fault bit `B2h.3` (the P30's
`9Ah.3`). On a P28 box the heartbeat
never comes: observed `2FCh` = 0, `214h` = 80h (fault bit 23, code 24), the
lamp lit again 0.1 s after the bulb check (2.19 s), and the read-out holds it
lit without blinking: 0x3529 `JBS 214h.7` is the P30's 0x3634. The same as
the P30 image on a P28 box ("What the stock P30 does"), for the same
reason. This needs no knock board model beyond the one the P30 already has.

| setup | verdict | codes |
|---|---|---|
| `stock-p28-manual` | do not burn: lamp lit at 2.2 s | stored 24, blinked none |
| `stock-p30` (`--ecu p30`, knock sensor fitted) | worth burning (fuel stops at 8140 rpm, VTEC at 4440 rpm) | none |
| `stock-p30`, `knock_sensor = no` | do not burn: lamp lit at 9.6 s | 23 blinked and stored |

The P72 is the Integra GS-R box (wiki), whose engine has a knock sensor, and
the image ships with knock control on, so the box it is made for has a knock
board; the model's P30 box (P28 board plus knock board) is the closest
setup, and on it the image runs clean. Whether the P72 board's knock
interface is the P30's is inferred from the code being the same, not from a
P72 schematic. **"Do not burn" is true for the P28 box it was run on**: in a
P28 the lamp stays lit and the read-out shows no code. The wiki's P72 page
offers "P72 ROM with knock disabled + good checksum" for that use.

### lyonnel: the lamp is a display, not a fault

The lyonnel image (HTS code, see "HTS base ROM"; knock option `61F3h`, copied
into `227h.6` at 0x4EFE, is 00h, so no knock check) stores **no fault**: `212h..215h`
are 0 at the end of every run, and the read-out shows the steady no-code
lamp. Its lamp is driven by 0x3DE1..0x3E0B, which computes the lamp request
into `222h.3`, and 0x5D91..0x5DC9, which copies it to P1.4 unless a shift
light overrides it. (So `222h.3` in HTS code is the lamp request; the same
bit is an input of the HTS launch routine, "HTS base ROM".) Two things in
it are not in the stock ROM, and `check` sees both:

- **Key-on pulses.** The bulb check is the counter `2C6h`, counted down every
  0.1 s (0x4C04), lamp lit while it is non-zero. When it reaches 0 with
  `217h.5` set (engine not running), 0x5A71..0x5A8B reloads it with 4 and
  counts the reload in `413h`, until `413h` reaches option byte `6020h`. In
  lyonnel `6020h` = **04h** (00h in the HTS base ROM): after the bulb check
  ends at 2.60 s the lamp gives four pulses, lit 0.3 s and dark 0.1 s, from
  2.70 s to 4.24 s, then stays dark. What the display is for is not
  documented in the workspace.
- **Shift light.** With option byte `6142h` non-zero (FFh) and `6143h` zero,
  0x5DAB..0x5DC9 lights the lamp while the engine period `C4h` is below the
  word at `6144h`, 00DFh: above 1875000 / 223 = **8408 rpm**. Observed lit
  from 39.36 s to 40.53 s, the top of `full throttle`, and on a bench at
  8600 and 8900 rpm, never at 8300.

**What `check` said, and why it changed.** The lamp rule at the time took
the first lit edge after the bulb check as a fault: "DO NOT BURN -
check-engine light: lit at 2.7 s, after the bulb check", and then called the
quiet read-out "the ROM is showing a fault it does not blink". With `6020h`
set to 00h the verdict became "lit at 39.4 s" (the shift light); with
`6142h` also 00h it was **worth burning**, with every line ok. So the
emulator and the board model were not at fault, and a real P28 box would
show the same lamp; the verdict was wrong for this ROM, because the rule
assumed that any lamp lit after the bulb check means a stored fault.

The rule now judges the lamp at the end of the drive ("The end of the
drive" under "The lines"): a stored fault keeps the lamp lit, and both of
these displays go dark again. Today: `check lyonnel-2.0-launch-ok.bin`
exits 0, **WORTH BURNING**, with `check-engine light ....... ok: off at the end
of the drive; it lit 4 times during key on and once during full throttle (a
start-up flash or a shift light looks like this)` and `codes ...... ok: none
blinked (lamp steady with the connector grounded)`; the same on the P30 box
(`--ecu p30`, knock sensor fitted: the knock option is off, so the board is
not consulted). The acceptance case asserts that report, the four flashes
in `key on`, and the shift light lighting at 8408 to 8460 rpm on the sweep
and going dark again within 0.2 s of the start of `lift off`.

Two other lyonnel observations: the fuel pump prime is 2.6 s (2.1 s on the
others), the same length as its bulb check (lamp dark at 2.60 s), not traced
further; and its rev limiter cuts spark only, which the `full throttle
sweep` line now says it does not look for.
