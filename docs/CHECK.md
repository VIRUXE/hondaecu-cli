# ROM check

## Findings while building the script

Measured on the stock P28-230 ROM, `stock-p28-manual` setup, one run of
`script::run` (92.5 s of ECU time).

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
