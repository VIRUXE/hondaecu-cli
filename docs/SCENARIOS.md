# Honda OBD1 ECU Scripted Stimulus & Fault-Injection Guide

`hondaecu-cli replay` feeds scripted sensor conditions into the virtual OKI
MSM66207 frame by frame. The emulator advances them on its 5 MHz machine-cycle
timeline, but this is not a real-time or closed-loop engine/chassis simulation.
Fuel, ignition, idle-control, VTEC, and DTC behavior are inspection targets.

## How to Interpret a Replay

A preset establishes only the **input stimulus**: RPM, MAP, TPS, temperatures,
voltages, switches, and vehicle speed over time. Scripted RPM and speed are not
ECU outputs. A response is verified only when a run observes an independent,
documented RAM/SFR bit or physical-output mapping and compares it with a stated
expectation.

The stock-P28 analog path currently supports MAP on direct AI6, TPS on AI7,
ECT on U5/X2→ADCR1, primary HEGO on U6/X0→ADCR0, IAT on U6/X2 and battery
voltage on U6/X7 (the last two identified from the ROM and the pgmfi wiki, not
the schematic). IACV duty is measured from the ROM's own PWM output against a
modeled valve. `vtec_active` depends on emulator-side or unresolved board
mappings. Injector telemetry is measured on the modeled output stage described
in [`INJECTION.md`](INJECTION.md).

---

## 📌 Available Scenario Presets

### ⚠️ Fault & Error Injection Presets (verification targets)

| Preset Name | Injected condition | Candidate ROM response to investigate |
|---|---|---|
| **`error-map-failure`** | MAP input drops to 0.0 kPa under load. | Candidate DTC 3/5 and MAP substitution; establish the ROM bits, delay, and strategy. |
| **`error-ect-overheat`** | ECT input is driven to 135°C+. | Candidate DTC 6 and fuel/ignition changes; establish polarity, threshold, and delay. |
| **`error-tps-short`** | TPS ADC input is forced low under high MAP. | Candidate DTC 7 and substitution behavior; 0% TPS alone is not proof of a short. |
| **`error-ckp-distributor-loss`** | Scripted RPM drops to zero, stopping generated distributor edges. | Candidate CKP/TDC timeout response; the stimulus does not prove fuel or spark cut. |
| **`error-vtec-oil-pressure-loss`** | High RPM/TPS is scripted while an emulator pressure-switch flag remains open. | Candidate DTC 22 only after the switch/output board mapping is established. |
| **`error-alt-low-voltage`** | Battery input (U6/X7) drops to 8.5 V. | Investigate voltage compensation; this input is not the confirmed U5/X0 ELD signal. |
| **`error-o2-lean-stuck`** | Primary HEGO input remains at 0.02 V. | Candidate DTC 1/43 and trim-state response; establish closed-loop prerequisites and delay. |

---

### 🏎️ Normal Driving & Performance Presets

| Preset Name | Injected trace | ROM behavior to inspect |
|---|---|---|
| **`overrun-decel`** | High RPM/WOT followed by TPS closure and scripted RPM decay. | Look for DFCO and fuel restoration; no 1100-RPM threshold is assumed. |
| **`overrun-downhill`** | Extended scripted coast with throttle blips. | Look for injector cut/restoration and transient response. |
| **`accel-stomp`** | Cruise inputs followed by an instant 100% TPS command. | Look for transient fuel response; no derivative multiplier is assumed. |
| **`drag-pass`** | Scripted staged launch, gear pulls, shift lifts, and overrun. | Inspect limiter, fuel, VTEC, and DFCO state; the scripted path proves none of them. |
| **`launch-control-2step`** | Dedicated 2-Step stimulus: idle → WOT stab → stationary RPM bounce → clutch drop/launch → 1st gear pull → off-throttle abort | Compare ECU outputs across stationary, rolling and abort phases on a ROM that contains launch control. |
| **`vtec-hysteresis`** | RPM oscillates around a scripted 4800-RPM point. | Inspect candidate VTEC state only after board mapping is established. |
| **`electrical-load-idle`** | Scripted 800-RPM idle while load flags and voltage cycle. | Inspect IACV/injector response; output mappings remain provisional. |
| **`heat-soak-start`** | Restart inputs with ECT 98°C and IAT 60°C. | Inspect fuel/timing changes; the IAT endpoint (U6/X2) is from the pgmfi wiki, not the schematic. |
| **`dyno-pull`** | Scripted WOT sweep from 2000 to 8200 RPM. | Inspect ROM table access and output transitions; scripted RPM does not prove VTEC. |
| **`cold-start`** | Scripted -10°C start and warm-up to 85°C. | Inspect ECT-dependent output/state changes; no enrichment constant is assumed. |

---

## 🛠️ Detailed Fault & Error Breakdown

### 1. MAP Sensor Disconnect (`error-map-failure`)

- **Fault Injection**: At $t = 750\,\text{ms}$, MAP sensor signal drops to $0.0\,\text{kPa}$ while engine is under $45\%$ TPS load.
- **Observation Required**: Trace direct AI6/ADCR6 into this ROM's diagnostic branch and identify its threshold and qualification delay.
- **Candidate DTC**: DTC 3/5; confirm the actual ROM address and bits.
- **Candidate Failsafe**: Look for substituted MAP or TPS-based calculation; Alpha-N fallback is not established.

```bash
# Linux / macOS
./hondaecu-cli replay P28-230.bin error-map-failure

# Windows (PowerShell / CMD)
.\hondaecu-cli.exe replay P28-230.bin error-map-failure
```

---

### 2. Coolant Sensor Open / Overheat (`error-ect-overheat`)

- **Fault Injection**: At $t = 750\,\text{ms}$, ECT thermistor voltage drops $<0.2\,\text{V}$ (reading $>135^\circ\text{C}$).
- **Observation Required**: Trace U5/X2 through ADCR1 and the stock sampler into the ECT diagnostic logic.
- **Candidate DTC**: DTC 6; confirm the actual ROM address, bit, threshold, and delay.
- **Candidate Failsafe**: Compare independently observed fuel/ignition state; no fixed retard or enrichment is assumed.

```bash
# Linux / macOS
./hondaecu-cli replay P28-230.bin error-ect-overheat

# Windows (PowerShell / CMD)
.\hondaecu-cli.exe replay P28-230.bin error-ect-overheat
```

---

### 3. VTEC Low Oil Pressure Fault (`error-vtec-oil-pressure-loss`)

- **Fault Injection**: Scripted RPM reaches 5,500 RPM under WOT while the emulator pressure-switch flag remains open.
- **Observation Required**: First establish the MCU/board mappings for command and feedback; no 200 ms delay is treated as verified.
- **Candidate DTC**: DTC 22; confirm the ROM address, bit, and qualification logic.
- **Candidate Failsafe**: A VTEC transition or low-cam limiter is not currently asserted.

```bash
# Linux / macOS
./hondaecu-cli replay P28-230.bin error-vtec-oil-pressure-loss

# Windows (PowerShell / CMD)
.\hondaecu-cli.exe replay P28-230.bin error-vtec-oil-pressure-loss
```

---

### 4. Primary O2 Stuck Lean (`error-o2-lean-stuck`)

- **Fault Injection**: U6/X0 primary HEGO remains at 0.02 V. Fuel-trim movement is a response to observe, not an injected condition.
- **Observation Required**: Establish closed-loop entry conditions, timers, and trim representation for the selected ROM.
- **Candidate DTC**: DTC 1/43; confirm the actual ROM addresses and bits.
- **Candidate Failsafe**: A +25% limit or open-loop fallback is not assumed.

```bash
# Linux / macOS
./hondaecu-cli replay P28-230.bin error-o2-lean-stuck

# Windows (PowerShell / CMD)
.\hondaecu-cli.exe replay P28-230.bin error-o2-lean-stuck
```

---

### 5. Dedicated 2-Step Launch-Control Stimulus (`launch-control-2step`)

This is an input trace, not a closed-loop chassis simulation. It feeds RPM,
speed and sensor values to a ROM so its output response can be inspected. The
RPM rise above 4200 is scripted and therefore does **not**, by itself, prove
that a limiter released. Use a ROM that actually contains a 2-step patch; the
OEM P28-230 image is a stock calibration and is not a positive 2-step fixture.
The CLI detects that exact stock image and fails fast with an unsupported-ROM
error instead of reporting a false pass.

- **$t = 0$–$1.0\text{s}$**: Idle at 900 RPM, 0% TPS, 0 km/h — brake/clutch held, baseline before the stab.
- **$t = 1.0$–$1.3\text{s}$**: Driver stabs to 100% TPS while stationary; RPM ramps 900 → 4200 approaching the 2-step ceiling.
- **$t = 1.3$–$5.0\text{s}$**: **2-Step Rev Limiter Bounce** — RPM sawtooths ±250 around 4200 (period 0.4s) at 100% TPS / 0 km/h, simulating the fuel/spark-cut oscillation of a held limiter.
- **$t = 5.0$–$5.2\text{s}$**: Clutch drop / launch — speed rises 0 → 15 km/h while RPM holds near 4200.
- **$t = 5.2$–$8.0\text{s}$**: 1st gear pull stimulus — RPM climbs 4200 → 8200 as speed rises 15 → 90 km/h.
- **$t = 8.0$–$8.3\text{s}$**: Off-throttle abort stimulus — TPS snaps to 0% at high RPM / high speed.
- **$t = 8.3$–$9.5\text{s}$**: Coast down to a stop, TPS held at 0%.

```bash
# Linux / macOS
./hondaecu-cli replay P28-230.bin launch-control-2step launch_2step_results.csv

# Windows (PowerShell / CMD)
.\hondaecu-cli.exe replay P28-230.bin launch-control-2step launch_2step_results.csv
```

The CSV includes scripted `speed_kmh` and an injector-duration field: the
width of the last completed pulse on the modeled injector outputs (P2.0–P2.3
clocked out by TM0IO, see [`INJECTION.md`](INJECTION.md)), or 0 when no
injector opened during the last full engine cycle, i.e. the ROM is cutting
fuel. A meaningful assertion must compare independently observed pulses
between phases, not the scripted RPM input. The output stage is derived from
the ROM and still awaits confirmation against the P28 schematic.

---

## 📝 Writing Custom CSV Trace Files

You can export log files from **Honda Tuning Suite (HTS)**, **Crome**, **Neptune**, or **Hondata**, or construct custom CSV traces.

### CSV Header Requirement

Every input column below is a stimulus. Its presence cannot verify a limiter,
fuel cut, VTEC transition, DTC, or failsafe.
```csv
timestamp_ms,rpm,map_kpa,tps_pct,ect_celsius,iat_celsius,o2_volts,vbatt_volts,speed_kmh
0,800,30.0,0.0,85.0,25.0,0.45,14.2,0.0
100,2500,45.0,20.0,85.0,25.0,0.45,14.2,30.0
200,4800,95.0,100.0,85.0,25.0,0.85,14.1,80.0
300,7500,98.0,100.0,85.0,25.0,0.85,14.1,140.0
```

### Command
```bash
# Linux / macOS
./hondaecu-cli replay P28-230.bin my_custom_trace.csv results.csv

# Windows (PowerShell / CMD)
.\hondaecu-cli.exe replay P28-230.bin my_custom_trace.csv results.csv
```
