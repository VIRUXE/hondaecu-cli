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

### VTEC never engages at 88 °C coolant

`vtec_on_rpm` is `None` in `full throttle` (2000 to 9000 rpm, 100 % TPS,
100 km/h): port C bit 0 never went high during the run. The ROM's VTEC
decision at 0x11BE..0x11E2 drops to `RB P1.0` (0x11E2) at 0x11C7,
`CMPB 0C1h, #044h / JGE`: RAM `C1h` must be below 44h. `C1h` follows the
ECT input. Held at 6000 rpm, 100 % TPS, 100 km/h after a warm boot:

| `ect_celsius` | `C1h` | PC0 went high |
|---|---|---|
| 88 | 49h | no |
| 95 | 43h | yes |
| 110 | 2Eh | yes (and later off again via 0x11B9, not traced) |

`EngineState::sync_sensors_to_bus` maps ECT linearly
(`1023 - (ect + 40) / 160 * 900` counts), a placeholder rather than Honda's
thermistor curve. On that line 88 °C sits on the cold side of the ROM's VTEC
gate, which needs about 94 °C or more. This is a model limitation, not a ROM
fault; the brief's test does not assert VTEC, so neither the model nor the
script's 88 °C was changed. A verdict on VTEC from this script would be
wrong until the ECT curve is replaced.

### Also observed (all within the brief's assertions)

- Fuel pump on for 2.06 s in `key on`.
- `warm idle`: 53.34 engine cycles, injector openings 53/54/53/53,
  214 ignitions (4 x 53.34 = 213.3).
- `full throttle`: fuel stops at 7420 rpm.
- `lift off` (6000 to 1500 rpm, TPS 0, 25 kPa): no injector opens for the
  whole stage, so `fuel_stops_rpm` is `None` there (there is no opening for a
  gap to follow).
- No MIL edge between the bulb check (lit 0.04 to 2.11 s) and the read-out.
