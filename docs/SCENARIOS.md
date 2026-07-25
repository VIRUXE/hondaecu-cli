# Honda OBD1 ECU Real-Life Engine Runtime Scenarios & Fault Injection Playback Guide

`hondaecu-cli` features a real-time ECU log playback and scripted trace engine (`hondaecu-cli replay`). This subsystem feeds dynamic sensor conditions into the virtual OKI MSM66207 CPU and hardware peripherals frame-by-frame, observing how the ROM binary calculates fuel pulse widths ($T_{inj}$), ignition advance ($\theta_{spark}$), idle air control duty cycle (IACV), VTEC solenoid activation, and Diagnostic Trouble Codes (DTCs).

---

## 📌 Available Scenario Presets

### ⚠️ Fault & Error Injection Presets (Testing DTC Diagnostics & Failsafes)

| Preset Name | Simulated Sensor Fault / Anomaly | Diagnostic DTC Triggered | ROM Emergency Failsafe & Limp Mode |
|---|---|---|---|
| **`error-map-failure`** | MAP sensor vacuum line pops off under load (0.0 kPa) | **DTC 3** (MAP High/Low) & **DTC 5** (MAP Range) | Enters Speed-Density Alpha-N fallback using TPS sensor matrix. |
| **`error-ect-overheat`** | ECT sensor open circuit / thermistor unplugged (135°C+) | **DTC 6** (ECT Temp Sensor) | Initiates **Thermal Safety Ignition Retard** (-10° BTDC) & rich coolant protection. |
| **`error-tps-short`** | TPS grounds out (0% TPS) while engine MAP is at 95 kPa | **DTC 7** (TPS Throttle Sensor Bounds) | Defaults TPS to fail-safe 50% value to prevent severe lean stumble. |
| **`error-ckp-distributor-loss`** | Crankshaft CKP distributor pulse drops out at 4500 RPM | **DTC 4** (CKP Position Sensor) | Emergency fuel cut-off & ignition spark disable to protect engine. |
| **`error-vtec-oil-pressure-loss`** | VTEC commanded at 5500 RPM but oil pressure switch stays open (0 psi) | **DTC 22** (VTEC Pressure Switch) | Disengages VTEC solenoid output; limits engine revs to low-cam profile. |
| **`error-alt-low-voltage`** | Alternator brownout / dying battery voltage (Vbatt drops to 8.5V) | **DTC 20** (ELD / Electrical Load) | Injector dead-time compensation ($T_{dead}$) spikes to maximum pulse width. |
| **`error-o2-lean-stuck`** | Primary O2 sensor stuck at 0.02V lean despite +25% fuel trim | **DTC 1** (O2 Sensor) & **DTC 43** (Fuel System) | Disables closed-loop fuel correction; locks ECU into open-loop safety tables. |

---

### 🏎️ Normal Driving & Performance Presets

| Preset Name | Description & Real-World Condition | Primary ECU Physics Tested |
|---|---|---|
| **`overrun-decel`** | High RPM WOT snap closed to 0% TPS; engine braking down to 800 RPM | **Deceleration Fuel Cut-Off (DFCO)** ($T_{inj} = 0\,\mu\text{s}$) when RPM > 1100 & TPS = 0%, followed by anti-stall fuel re-injection. |
| **`overrun-downhill`** | Extended downhill coasting with intermittent heel-toe throttle blips | Intermittent DFCO cut-off, transient blip enrichment, and engine braking vacuum transitions. |
| **`accel-stomp`** | Steady cruise (2500 RPM, 15% TPS) followed by instant 100% TPS WOT stomp | **Transient Acceleration Tip-In Enrichment** ($\frac{d\text{TPS}}{dt}$ & $\Delta\text{MAP}$ derivative enrichment multipliers). |
| **`drag-pass`** | 1/4 Mile Drag Strip Pass (2-Step launch → 1st → 2nd → 3rd → 4th → trap overrun) | **Launch Control 2-Step Limiter**, high-load fuel delivery, VTEC high-cam crossover, gear-shift lifts, and high-RPM trap DFCO. |
| **`vtec-hysteresis`** | RPM oscillating around 4800 RPM threshold with VTEC oil pressure fault injection | VTEC Spool Valve engagement criteria (RPM $\ge$ 4800, TPS $\ge$ 20%, ECT $\ge$ 60°C) and DTC 22 pressure switch hysteresis. |
| **`electrical-load-idle`** | Engine idling at 800 RPM while AC compressor and headlights cycle ON/OFF | **Idle Air Control Valve (IACV) Load Compensation** & alternator voltage dip dead-time compensation ($T_{dead}$). |
| **`heat-soak-start`** | Hot engine restart after sitting (ECT = 98°C, IAT = 60°C) | Hot-start anti-percolation fuel enrichment and IAT air density trim corrections. |
| **`dyno-pull`** | Full WOT Dyno Sweep from 2000 RPM to 8200 RPM | 3D Fuel & Ignition matrix interpolation across low-cam and high-cam VTEC maps. |
| **`cold-start`** | Engine startup in cold ambient (-10°C) warming up to operating temp (85°C) | Cold-start ECT enrichment multiplier ($K_{ect}$) and fast-idle thermal valve / IACV warmup position. |

---

## 🛠️ Detailed Fault & Error Breakdown

### 1. MAP Sensor Disconnect (`error-map-failure`)
- **Fault Injection**: At $t = 750\,\text{ms}$, MAP sensor signal drops to $0.0\,\text{kPa}$ while engine is under $45\%$ TPS load.
- **ROM Diagnostic Action**: Evaluated by ADC ISR. Detects voltage $<0.2\,\text{V}$ ($<102$ ADC counts).
- **DTC Triggered**: **DTC 3** (MAP High/Low) & **DTC 5** (MAP Circuit Range).
- **Failsafe**: Switches fuel calculation from Speed-Density (MAP vs RPM) to Alpha-N (TPS vs RPM).

```bash
cargo run --release -- replay P28-230.bin error-map-failure
```

---

### 2. Coolant Sensor Open / Overheat (`error-ect-overheat`)
- **Fault Injection**: At $t = 750\,\text{ms}$, ECT thermistor voltage drops $<0.2\,\text{V}$ (reading $>135^\circ\text{C}$).
- **ROM Diagnostic Action**: Evaluated by ADC ISR.
- **DTC Triggered**: **DTC 6** (Engine Coolant Temperature Circuit).
- **Failsafe**: Initiates safety ignition retard (retards spark $-10^\circ$ BTDC) and enriches fuel to prevent engine melt.

```bash
cargo run --release -- replay P28-230.bin error-ect-overheat
```

---

### 3. VTEC Low Oil Pressure Fault (`error-vtec-oil-pressure-loss`)
- **Fault Injection**: Engine revs to $5,500\,\text{RPM}$ under WOT. ECU energizes VTEC spool valve solenoid output, but VTEC oil pressure switch contacts fail to close ($0\,\text{psi}$ pressure).
- **ROM Diagnostic Action**: Evaluated after 200 ms hysteresis timer.
- **DTC Triggered**: **DTC 22** (VTEC Oil Pressure Switch Fault).
- **Failsafe**: De-energizes VTEC spool valve; locks rev limiter to low-cam profile.

```bash
cargo run --release -- replay P28-230.bin error-vtec-oil-pressure-loss
```

---

### 4. Primary O2 Stuck Lean (`error-o2-lean-stuck`)
- **Fault Injection**: O2 sensor reads $0.02\,\text{V}$ lean continuously. Closed-loop fuel trim ramps up to maximum (+25%).
- **ROM Diagnostic Action**: Evaluated after closed-loop integration timer expires.
- **DTC Triggered**: **DTC 1** (Primary O2 Sensor) & **DTC 43** (Fuel System Lean Limit Exceeded).
- **Failsafe**: Disables closed-loop feedback; locks ECU into safe open-loop base tables.

```bash
cargo run --release -- replay P28-230.bin error-o2-lean-stuck
```

---

## 📝 Writing Custom CSV Trace Files

You can export log files from **Honda Tuning Suite (HTS)**, **Crome**, **Neptune**, or **Hondata**, or construct custom CSV traces.

### CSV Header Requirement
```csv
timestamp_ms,rpm,map_kpa,tps_pct,ect_celsius,iat_celsius,o2_volts,vbatt_volts,speed_kmh
0,800,30.0,0.0,85.0,25.0,0.45,14.2,0.0
100,2500,45.0,20.0,85.0,25.0,0.45,14.2,30.0
200,4800,95.0,100.0,85.0,25.0,0.85,14.1,80.0
300,7500,98.0,100.0,85.0,25.0,0.85,14.1,140.0
```

### Command
```bash
cargo run --release -- replay P28-230.bin my_custom_trace.csv results.csv
```
