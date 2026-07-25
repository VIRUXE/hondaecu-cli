# Honda OBD1 ECU Real-Life Engine Runtime Scenarios & Playback Guide

`hondaecu-cli` features a real-time ECU log playback and scripted trace engine (`hondaecu-cli replay`). This subsystem feeds dynamic sensor conditions into the virtual OKI MSM66207 CPU and hardware peripherals frame-by-frame, observing how the ROM binary calculates fuel pulse widths ($T_{inj}$), ignition advance ($\theta_{spark}$), idle air control duty cycle (IACV), VTEC solenoid activation, and Diagnostic Trouble Codes (DTCs).

---

## 📌 Available Scenario Presets

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

## 🚀 Detailed Scenario Breakdown & ECU Operation

### 1. Overrun Deceleration Fuel Cut-Off (`overrun-decel`)

#### Real-World Physics
When the driver lets off the gas pedal at high RPM (e.g., shifting down or engine braking into a turn), throttle position drops to 0% and intake manifold vacuum spikes (MAP drops to 18–22 kPa).

#### ECU ROM Operation
1. **Fuel Cut Active**: When `TPS == 0%` and `RPM > 1100 RPM`, the ECU cuts fuel injection completely ($T_{inj} = 0\,\mu\text{s}$). This improves fuel economy and engine braking efficiency.
2. **Re-injection Catch**: As engine speed decelerates to ~1100 RPM, the ECU re-initiates fuel injection to prevent the engine from stalling as it settles back into idle (800 RPM).

```bash
cargo run --release -- replay P28-230.bin overrun-decel
```

---

### 2. Accel Tip-In Enrichment (`accel-stomp`)

#### Real-World Physics
Opening the throttle plate rapidly causes a sudden drop in manifold vacuum and a rush of air into the intake plenum. Because liquid fuel clings to the intake manifold walls (wall wetting effect), the air moves faster than the fuel film.

#### ECU ROM Operation
The ECU detects the rapid rate of change of throttle position ($\Delta\text{TPS}$) and manifold pressure ($\Delta\text{MAP}$). It immediately adds an extra burst of fuel width ($T_{accel}$) on top of the base fuel lookup to prevent a lean stumble or hesitation.

```bash
cargo run --release -- replay P28-230.bin accel-stomp
```

---

### 3. Drag Strip 1/4 Mile Pass (`drag-pass`)

#### Real-World Physics
Simulates a full quarter-mile drag race from a stationary launch to the trap finish line.

#### ECU ROM Operation
- **0.0s – 1.0s**: Vehicle stationary (`VSS = 0`), throttle 100%. 2-step launch limiter holds engine at 4500 RPM.
- **1.0s – 3.0s**: 1st gear full acceleration.
- **3.0s – 3.2s**: Quick clutch lift for 1st → 2nd gear shift. MAP drops, fuel cuts briefly.
- **3.2s – 5.5s**: 2nd gear pull. RPM passes 4800 RPM; ECU energizes VTEC solenoid output.
- **8.5s+**: Finish line trap. Throttle snaps closed at 8100 RPM; ECU enters high-RPM overrun fuel cut.

```bash
cargo run --release -- replay P28-230.bin drag-pass drag_output.csv
```

---

### 4. Electrical Load & AC Compensation (`electrical-load-idle`)

#### Real-World Physics
When the AC compressor clutch engages or headlights turn on, the mechanical load on the engine increases and battery voltage drops (14.2V → 12.1V).

#### ECU ROM Operation
1. **IACV Duty Cycle Spike**: To maintain a steady 800 RPM idle, the ECU increases the Idle Air Control Valve (IACV) PWM duty cycle from ~30% to ~55-70%.
2. **Battery Dead-Time Trim**: Lower battery voltage slows down injector pintle opening times. The ECU adds dead-time compensation ($T_{dead}$) to prevent the engine from running lean.

```bash
cargo run --release -- replay P28-230.bin electrical-load-idle
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
