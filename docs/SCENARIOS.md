# Honda OBD1 ECU Scripted Stimulus & Fault-Injection Guide

`hondaecu-cli replay` feeds scripted sensor conditions into the virtual OKI
MSM66207 frame by frame. Its timestamps use approximate instruction timing;
this is not a real-time or closed-loop engine simulation. Fuel, ignition,
idle-control, VTEC, and DTC behavior are **inspection targets** only.

## How to Interpret a Replay

The preset definition establishes only the **injected stimulus**: values such as
RPM, MAP, TPS, temperature, voltage, switch state, and vehicle speed over time.
RPM and speed are scripted inputs, not results of a closed-loop engine or chassis
model.

A ROM response is verified only when the run observes it independently—for
example, a documented RAM/SFR state change, DTC bit, or output-pin transition—and
an assertion compares that observation with a stated expectation. Record the ROM
identity and the address, bit, or pin used by the assertion. A scripted sensor,
RPM, or speed value cannot also serve as evidence that the ECU caused it.

The OKI 66K manuals document CPU, ADC, serial, timer, port, and interrupt
behavior. They do **not** document Honda's ROM-specific DTC assignments,
calibration thresholds, failsafe policy, or ECU-board signal wiring. Claims in
those areas remain hypotheses until they are traced in the selected ROM and, for
physical outputs, checked against the board mapping.

---

## 📌 Available Scenario Presets

### ⚠️ Fault & Error Injection Presets (DTC/Failsafe Verification Targets)

| Preset Name | Injected Sensor Fault / Anomaly | DTC to Check | Failsafe/Output to Check |
|---|---|---|---|
| **`error-map-failure`** | MAP input drops to 0.0 kPa under load. | Candidate **DTC 3/5**; verify the ROM's actual DTC bits and qualification delay. | Check for a MAP-substitution strategy; Alpha-N fallback is not yet established. |
| **`error-ect-overheat`** | ECT input is driven to 135°C+. | Candidate **DTC 6**; verify the ROM's threshold, polarity, and delay. | Check ignition/fuel outputs; a fixed -10° retard and enrichment are not yet established. |
| **`error-tps-short`** | The modeled TPS ADC channel is forced to ground under high MAP; 0% TPS alone would remain a valid 0.5 V signal. | Candidate **DTC 7**; verify the ROM's plausibility logic. | Check the value used by fuel logic; a substitution strategy is not established. |
| **`error-ckp-distributor-loss`** | Scripted RPM drops to zero, stopping generated distributor edges. | Candidate **DTC 4/8**; verify the ROM's timeout and DTC state. | Check independently observed outputs; the trace does not prove a fuel cut. |
| **`error-vtec-oil-pressure-loss`** | High-RPM/TPS values are injected and an emulator-side pressure-switch flag is opened. Its MCU/board connection is not implemented. | Candidate **DTC 22** only after the input mapping exists. | VTEC and limiter behavior cannot currently be asserted. |
| **`error-alt-low-voltage`** | Battery-voltage input drops to 8.5 V. | Candidate **DTC 20**; verify whether this ROM associates it with the injected signal. | Check measured injector timing; maximum dead-time compensation is not yet established. |
| **`error-o2-lean-stuck`** | Primary O2 input is held at 0.02 V. | Candidate **DTC 1/43**; verify closed-loop preconditions, delay, and DTC bits. | Check trim/control state; a +25% limit and open-loop fallback are not yet established. |

---

### 🏎️ Normal Driving & Performance Presets

| Preset Name | Injected Trace | ROM Behavior to Inspect |
|---|---|---|
| **`overrun-decel`** | High RPM/WOT followed by scripted TPS closure and RPM decay. | Check for DFCO and later fuel restoration; the 1100-RPM threshold must be established per ROM. |
| **`overrun-downhill`** | Extended scripted coast with throttle blips. | Check measured injector response for cut and transient enrichment. |
| **`accel-stomp`** | Steady cruise inputs followed by an instant 100% TPS command. | Check measured fuel response for transient enrichment; no derivative multiplier is assumed. |
| **`drag-pass`** | Scripted staged launch, gear pulls, shift lifts, and overrun. | Check limiter, fuel, VTEC, and DFCO outputs; the scripted launch/RPM path proves none of them. |
| **`launch-control-2step`** | Dedicated 2-Step stimulus: idle → WOT stab → stationary RPM bounce → clutch drop/launch → 1st gear pull → off-throttle abort | Compare ECU outputs across stationary, rolling and abort phases on a ROM that contains launch control. |
| **`electrical-load-idle`** | Scripted 800-RPM idle while electrical-load inputs cycle. | Check independently observed IACV and injector timing response. |
| **`heat-soak-start`** | Scripted restart with ECT 98°C and IAT 60°C. | Check fuel/timing changes; no anti-percolation strategy is assumed. |
| **`dyno-pull`** | Scripted WOT sweep from 2000 to 8200 RPM. | Check ROM table use and output transitions; scripted RPM does not prove VTEC crossover. |
| **`cold-start`** | Scripted -10°C start and warm-up to 85°C. | Check injector and idle-control outputs for temperature-dependent behavior. |

---

## 🛠️ Detailed Fault & Error Breakdown

### 1. MAP Sensor Disconnect (`error-map-failure`)

- **Fault Injection**: At $t = 750\,\text{ms}$, MAP sensor signal drops to $0.0\,\text{kPa}$ while engine is under $45\%$ TPS load.
- **Observation Required**: Trace the resulting ADCR value into this ROM's diagnostic branch and identify its threshold and qualification delay.
- **Expected DTC to Verify**: Candidate **DTC 3/5**; confirm the actual ROM DTC address and bits.
- **Expected Failsafe to Verify**: Check fuel calculations for a substituted MAP or TPS-based strategy; Alpha-N fallback is not assumed.

```bash
# Linux / macOS
./hondaecu-cli replay P28-230.bin error-map-failure

# Windows (PowerShell / CMD)
.\hondaecu-cli.exe replay P28-230.bin error-map-failure
```

---

### 2. Coolant Sensor Open / Overheat (`error-ect-overheat`)

- **Fault Injection**: At $t = 750\,\text{ms}$, ECT thermistor voltage drops $<0.2\,\text{V}$ (reading $>135^\circ\text{C}$).
- **Observation Required**: Trace the ADCR value through this ROM's ECT diagnostic logic and record its threshold and delay.
- **Expected DTC to Verify**: Candidate **DTC 6**; confirm the actual ROM DTC address and bit.
- **Expected Failsafe to Verify**: Compare independently observed fuel and ignition outputs before and after injection; no fixed retard or enrichment is assumed.

```bash
# Linux / macOS
./hondaecu-cli replay P28-230.bin error-ect-overheat

# Windows (PowerShell / CMD)
.\hondaecu-cli.exe replay P28-230.bin error-ect-overheat
```

---

### 3. VTEC Low Oil Pressure Fault (`error-vtec-oil-pressure-loss`)

- **Fault Injection**: Scripted RPM reaches $5,500\,\text{RPM}$ under WOT while the VTEC oil-pressure-switch input remains open. VTEC command state is an output to observe, not part of the stimulus.
- **Observation Required**: Establish this ROM's command/switch qualification logic and delay; 200 ms is not treated as verified.
- **Expected DTC to Verify**: Candidate **DTC 22**; confirm the actual ROM DTC address and bit.
- **Expected Failsafe to Verify**: Check VTEC output transitions and independently measured fuel/ignition behavior; a low-cam limiter is not assumed.

```bash
# Linux / macOS
./hondaecu-cli replay P28-230.bin error-vtec-oil-pressure-loss

# Windows (PowerShell / CMD)
.\hondaecu-cli.exe replay P28-230.bin error-vtec-oil-pressure-loss
```

---

### 4. Primary O2 Stuck Lean (`error-o2-lean-stuck`)

- **Fault Injection**: O2 sensor input remains at $0.02\,\text{V}$. Fuel-trim movement is a ROM response to observe, not an injected condition.
- **Observation Required**: Establish closed-loop entry conditions, the integration timer, and the trim representation for this ROM.
- **Expected DTC to Verify**: Candidate **DTC 1/43**; confirm the actual ROM DTC addresses and bits.
- **Expected Failsafe to Verify**: Check control-state and fuel-output changes; a +25% limit or open-loop fallback is not assumed.

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

The CSV includes scripted `speed_kmh` and an injector-duration value that the
emulator currently derives from active-low P2.0–P2.3 transitions. A meaningful
assertion must compare observed transitions between the stationary, rolling,
and abort phases; it must not assert against the scripted RPM input. Treating
those pins as physical injector outputs also depends on separately verified P28
board wiring, which the OKI CPU manual does not define.

---

## 📝 Writing Custom CSV Trace Files

You can export log files from **Honda Tuning Suite (HTS)**, **Crome**, **Neptune**, or **Hondata**, or construct custom CSV traces.

### CSV Header Requirement

Every column in the input CSV below is a stimulus supplied to the emulator. A
value appearing here is not an ECU output and cannot verify a limiter, fuel cut,
VTEC transition, DTC, or failsafe.

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
