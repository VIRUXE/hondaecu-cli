# Honda OBD1 ECU (OKI MSM66207) Emulator & ROM Testing Suite in Rust

[![License](https://img.shields.io/badge/license-Unlicense-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.97%2B-orange.svg)](https://www.rust-lang.org/)

An experimental command-line emulator and ROM-inspection suite for the stock
Honda P28 ECU, powered by an OKI MSM66207 16-bit CPU core and a developing
peripheral/board model.

---

## 📚 Documentation Index

- 📖 [**Scripted Stimulus & Fault-Injection Guide (`docs/SCENARIOS.md`)**](docs/SCENARIOS.md): Preset and CSV inputs, plus the boundary between an injected condition and an observed ROM response.
- 🧠 [**OKI MSM66207 CPU Core — Findings & Implementation Notes (`docs/CPU_CORE.md`)**](docs/CPU_CORE.md): Instruction decoding/execution, machine-cycle timing, register and memory models, peripherals, stock-P28 observations, trace tooling, and known gaps.
- 💉 [**Stock P28 Injection (`docs/INJECTION.md`)**](docs/INJECTION.md): How the ROM schedules and drives the injectors, the six defects that kept the emulator from sustaining injection, and how each was verified.
- 📐 [**Honda P28 ECU Architecture Notes (`docs/P28_ARCHITECTURE.md`)**](docs/P28_ARCHITECTURE.md): MSM66207 memory mapping, the P28 analog front end, boot traces, interrupt vectors, and clearly labelled control-strategy hypotheses.

---

## 📌 Architectural Overview

```mermaid
flowchart TD
    ROM["Raw 32KB ECU ROM Image<br/>(P28-230.bin / Custom ROM)"] --> Bus
    
    subgraph Core ["hondaecu-cli Core Engine"]
        CPU["OKI MSM66207 Virtual CPU<br/>PC, A, DP, X1, X2, USP, SSP, LRB, PSW"]
        Interp["Instruction Interpreter & Decoder<br/>Line-by-line Byte Execution"]
        Bus["Memory Bus & SFR Space<br/>Code Space (0..32KB)<br/>Data Space (0..4KB SFRs/RAM)"]
        Timers["Hardware Timers (TM0..TM3)<br/>& ADC Hardware (ADCR0..ADCR7)"]
        ISRs["ISR Interrupt Dispatcher<br/>(TM2 capture CKP, INT1 TDC, INT0 VSS, TM0..3, Serial RX)"]
    end
    
    subgraph EngineSim ["Virtual Engine Peripheral Simulation"]
        Engine["Engine Dynamics<br/>RPM, MAP, TPS, ECT, IAT, O2, Vbatt, VSS"]
        Signals["Distributor Pulses (CKP/TDC)<br/>& Analog Voltage Conversion"]
    end
    
    subgraph Modes ["CLI Modes & Subcommands"]
        Test["ROM and Hardware<br/>Sanity Checks"]
        Run["Timed Simulation Run"]
        REPL["Interactive REPL Shell<br/>(Step, Inspect, Set MAP/RPM, Dump)"]
        Disasm["Linear Disassembler"]
        Trace["Boot and Runtime<br/>Trace Modes"]
    end

    Bus <--> CPU
    CPU <--> Interp
    EngineSim <--> Bus
    Interp <--> ISRs
    ISRs <--> Timers
    Core --> Modes
```

---

## 🔥 Key Features

- **Table-driven MSM66207 core**: Whole instruction patterns are decoded with
  `DD`-aware byte/word selection. Machine-cycle counts come from the manual's
  instruction list for every addressing variant, on-chip and external, with
  taken-branch penalties. Interrupt entry is charged 17 cycles, a figure
  derived from the manual's `RTI` and `VCAL` counts because no available
  document states it.
- **One 5 MHz machine-cycle clock domain**: CPU execution, timers, distributor
  scheduling, replay time, and pulse-width conversion share the same modeled
  clock. Host instructions per second are only a performance measurement.
- **P28 analog front end**: The two board-level 4051 muxes feed ADCR0/ADCR1 and
  share P2.5/P2.6/P2.7 as A/B/C. Direct inputs include optional EGR on AI3,
  MAP on AI6, and TPS on AI7. Conversion takes 64 us: 320 modeled machine
  cycles, equal to 640 oscillator clocks.
- **Timers per the MSM66207 manual**: The 3-bit clock select and all four
  modes (auto reload, clock output, capture register, realtime output) follow
  chapter 9 of the user's manual. CKP arrives as a timer-2 capture edge, TDC on
  INT1, CYP on the transition detector, and VSS on INT0.
- **Sequential injection on the stock ROM**: The injector output stage is
  modeled as the ROM drives it -- P2.0-P2.3 staged, then clocked out by timer
  0's realtime output -- giving one pulse per cylinder per engine cycle from
  cranking speed to the ROM's own rev limit. See `docs/INJECTION.md`.
- **Inspection tools**: Linear disassembly, an interactive shell, replay, an
  experimental TCP datalog server, and boot/runtime `trace` modes for PC/RAM
  watches, histograms, snapshots, execution ranges, and per-output injector
  opening counts and pulse widths.
- **Tracked decoder ground truth**: `tests/fixtures/p28_dasm662.tsv` exercises
  mnemonic and length agreement for 10,914 stock-P28 instruction windows. It
  does not certify every execution semantic, peripheral, or board connection.
- **DTC reference catalog**: The catalog is metadata. Scenario inputs and
  synthetic evaluator checks are not proof that stock firmware qualified or
  stored a DTC.

---

## Accuracy and Test Scope

The current tree can execute the stock P28 image through its modeled power-on
path with live mux inputs, but it is not a physical-ECU validator.

- U6 feeds ADCR0 and U5 feeds ADCR1. Confirmed endpoints are U6/X0 HEGO,
  U6/X3 barometric pressure, U5/X0 ELD, U5/X2 ECT, U5/X3 an unresolved
  boot-check signal, direct AI3 optional EGR, AI6 MAP, and AI7 TPS. Battery
  voltage (U6/X7), IAT (U6/X2), the gearbox configuration input (U5/X1) and
  IACV current (AI4) are identified from the ROM and the pgmfi wiki.
- The stock sampler at `0x52DE..0x52F7` stores ADCR0 high bytes at
  `0x03BE[index]` and ADCR1 high bytes at `0x03C6[index]` while advancing the
  shared selector.
- The stock ROM sustains sequential injection: one opening per injector output
  per engine cycle, in rotation, from 300 rpm to its fuel cut between 7000 and
  7500 rpm. Pulse widths come from the ROM's own tables acting on modeled
  sensor voltages; they are plausible, not validated against a datalog.
- The injector output stage (P2.0–P2.3 clocked by TM0IO into a 74HC175) and
  the crank-signal pin assignment (CKP on TM2IO, TDC on INT1, CYP on TRNS0,
  VSS on INT0) were derived from the ROM and are confirmed by the Bomarc
  schematic. The output-to-cylinder mapping and the real sensor phasing are
  not established.
- Fuel-pump, MIL, A/C clutch, fan and purge telemetry follow the board's
  82C55 outputs, whose wiring is read off the schematic; `vtec_active` is
  still emulator-side logic. The feedback paths the ROM monitors (IACV
  current, ignition and injector monitors, backup-MCU heartbeat) are models
  with stated assumptions; see `docs/CPU_CORE.md`.
- PWM0/PWM1 follow the user's manual (PWM, auto reload and free-run modes),
  but which board loads P4.2 and P4.3 drive is not established.
- External data addresses above the 4 KiB backing store currently alias
  (including the digital input port at `0x4700`).
- Three executor flag bugs were found that the decoder ground-truth test cannot
  see. The remaining mnemonics have not been audited against the manual.
- Replay presets are scripted stimuli. They do not by themselves prove a DTC,
  limiter, failsafe, fuel strategy, or physical output. The TCP datalog protocol
  is experimental.

Run the Rust suite with:

```bash
cargo test --release
```

The tracked `dasm662` fixture is included and runs normally. Some stock-ROM and
launch-ROM tests look for non-distributable images outside this repository and
return without exercising the ROM when those files are absent; read their test
output and source before treating a green run as ROM-backed evidence.

---

## ⚡ Quickstart & Installation

### Option 1: Download Standalone Binaries (No Rust Required!)
Download the pre-compiled binary for your operating system from [**GitHub Releases**](https://github.com/VIRUXE/hondaecu-cli/releases):

- **Windows (`.exe`)**: Download `hondaecu-cli-windows-x86_64.zip`, extract, and open Command Prompt or PowerShell.
- **Linux (`x86_64`)**: Download `hondaecu-cli-v0.1.0-x86_64-unknown-linux-gnu.tar.gz` and extract `hondaecu-cli`.

### Option 2: Building from Source (Rust Developers)
```bash
git clone https://github.com/VIRUXE/hondaecu-cli.git
cd hondaecu-cli
cargo build --release
```

---

## 📖 Usage Examples

> **Note**: For Windows, replace `./hondaecu-cli` with `.\hondaecu-cli.exe`.

### 1. Run Stock P28 ROM and Hardware Sanity Checks
```bash
# Linux / macOS
./hondaecu-cli test P28-230.bin

# Windows (PowerShell / CMD)
.\hondaecu-cli.exe test P28-230.bin
```

### 2. ECU Datalog CSV Replay & Scripted Trace Playback
Replay a CSV log or scripted sensor preset through the emulator and export its
observed output fields. Scenario names describe stimuli, not verified ROM
responses:
```bash
# Replay built-in dyno pull preset (2000 -> 8200 RPM WOT)
./hondaecu-cli replay P28-230.bin dyno-pull output_results.csv

# Replay a MAP-fault stimulus; inspect the ROM response separately
./hondaecu-cli replay P28-230.bin error-map-failure

# Replay custom CSV datalog file
./hondaecu-cli replay P28-230.bin my_ecu_log.csv output_results.csv
```

CSV Format (`my_ecu_log.csv`):
```csv
timestamp_ms,rpm,map_kpa,tps_pct,ect_celsius,iat_celsius,o2_volts,vbatt_volts,speed_kmh
0,800,30,0,85,25,0.45,14.2,0
100,2200,45,15,85,25,0.45,14.2,20
200,4800,95,100,85,25,0.85,14.1,80
```

Available presets include `dyno-pull`, `overrun-decel`, `accel-stomp`,
`drag-pass`, `launch-control-2step`, the `error-*` stimuli, `cold-start`,
`electrical-load-idle`, and `heat-soak-start`. See
[`docs/SCENARIOS.md`](docs/SCENARIOS.md) for the complete list.

### 3. Live Serial Datalogging Protocol Server
Start the experimental TCP datalogging server on port `38400` for protocol
development. Client compatibility and real-time cadence are not guaranteed:
```bash
# Linux / macOS
./hondaecu-cli datalog P28-230.bin 38400

# Windows (PowerShell / CMD)
.\hondaecu-cli.exe datalog P28-230.bin 38400
```

### 4. ECU Simulation Run
Execute 100,000 instructions with a scripted 3000 RPM input:
```bash
./hondaecu-cli run P28-230.bin 100000 3000
```

### 5. Interactive Debugging REPL
Launch the interactive shell to single-step, inspect registers, and manipulate virtual engine parameters in real time:
```bash
./hondaecu-cli interactive P28-230.bin
```

Interactive commands:
```text
ecu [0x21E2]> step 5
ecu [0x21EA]> regs
ecu [0x21EA]> set rpm 6500
ecu [0x21EA]> set map 100
ecu [0x21EA]> set tps 100
ecu [0x21EA]> status
ecu [0x21EA]> dump 0x0060 16
```

### 6. Machine Code Disassembler
Linearly decode instructions starting at a hexadecimal ROM offset without
executing them:
```bash
./hondaecu-cli disasm P28-230.bin 0x21E2 20
```

### 7. Boot and Runtime Trace

The `trace` command emits machine-readable PC/RAM observations. Runtime modes
include `ramwatch`, `pcwatch`, `pccount`, `pchist`, `snap`, `exec`, and
`injcount`; `bootpc` and `bootram` inspect the raw power-on path.

`injcount` prints `INJ o0 o1 o2 o3 total n` (openings per injector output in
the window) and `INJPW_US w0 w1 w2 w3` (last completed pulse width per output,
in microseconds). `ramwatch` lines end with the machine-cycle count.

```bash
./hondaecu-cli trace P28-230.bin injcount 3000 1.0
./hondaecu-cli trace P28-230.bin pchist 5500 1.0 25
./hondaecu-cli trace P28-230.bin bootpc 0 0 0x220E,0x2211,0x2213,0x2426
```

---

## 🚦 Honda OBD1 Diagnostic Trouble Code (DTC) Matrix

The table is reference metadata. `CATALOGUED` does not mean the emulator has
observed the stock ROM qualifying or storing that DTC:

| DTC Code | Component Name | Description & Fault Condition | Status |
|---|---|---|---|
| **DTC 0** | ECU Internal ROM | Corrupt Checksum / Modulo Sum Mismatch (Solid CEL) | **CATALOGUED** |
| **DTC 1** | Primary O2 Sensor | Signal out of range (<0.1V / >1.1V / Open circuit) | **CATALOGUED** |
| **DTC 2** | Secondary O2 Sensor | Secondary O2 circuit fault (JDM / Lean spot) | **CATALOGUED** |
| **DTC 3** | MAP Sensor High/Low | Manifold Absolute Pressure sensor out of bounds | **CATALOGUED** |
| **DTC 4** | CKP Position Sensor | Crankshaft position pulse signal missing at high RPM | **CATALOGUED** |
| **DTC 5** | MAP Sensor Range | Vacuum mismatch vs engine RPM/TPS | **CATALOGUED** |
| **DTC 6** | ECT Engine Temp | Coolant temp sensor open (<0.2V) or shorted (>4.8V) | **CATALOGUED** |
| **DTC 7** | TPS Throttle Sensor | Throttle position voltage out of bounds (<0.3V or >4.8V) | **CATALOGUED** |
| **DTC 8** | TDC Sensor Pulses | Top Dead Center distributor pulse sync fault | **CATALOGUED** |
| **DTC 9** | CYP Sensor Pulses | Cylinder position pulse phase fault | **CATALOGUED** |
| **DTC 10** | IAT Intake Air Temp | Air temp voltage open (<0.2V) or shorted (>4.8V) | **CATALOGUED** |
| **DTC 11** | Ignition Module | Distributor igniter module pulse missing | **CATALOGUED** |
| **DTC 12** | EGR System | EGR valve position sensor out of range | **CATALOGUED** |
| **DTC 13** | BARO Sensor | Atmospheric pressure sensor internal circuit fault | **CATALOGUED** |
| **DTC 14** | IACV Idle Valve | Idle Air Control Valve open/short circuit | **CATALOGUED** |
| **DTC 15** | Ignition Coil Output | Ignition coil primary circuit failure | **CATALOGUED** |
| **DTC 16** | Injector Driver | Fuel injector driver transistor open/short | **CATALOGUED** |
| **DTC 17** | VSS Speed Sensor | Missing speed pulse while RPM > 2000 & high MAP | **CATALOGUED** |
| **DTC 19** | A/T Lockup Solenoid | Automatic transmission lockup solenoid circuit fault | **CATALOGUED** |
| **DTC 20** | ELD Load Detector | Fuse box ELD current sensor out of range | **CATALOGUED** |
| **DTC 21** | VTEC Solenoid | VTEC solenoid coil open/short circuit | **CATALOGUED** |
| **DTC 22** | VTEC Pressure Switch | Low oil pressure / switch open when VTEC commanded | **CATALOGUED** |
| **DTC 23** | Knock Sensor (KS) | Knock sensor circuit open or signal noise fault | **CATALOGUED** |
| **DTC 30** | A/T Shift Signal A | Automatic transmission shift solenoid A circuit | **CATALOGUED** |
| **DTC 31** | A/T Shift Signal B | Automatic transmission shift solenoid B circuit | **CATALOGUED** |
| **DTC 41** | O2 Sensor Heater | Oxygen sensor heater element circuit open/short | **CATALOGUED** |
| **DTC 43** | Fuel Supply System | Fuel pressure or O2 trim lean limit exceeded | **CATALOGUED** |
| **DTC 45** | Fuel System Rich/Lean | Air/Fuel ratio out of closed-loop correction range | **CATALOGUED** |
| **DTC 48** | LAF Wideband Sensor | Linear air-fuel ratio sensor circuit fault (Civic VX) | **CATALOGUED** |
| **DTC 92** | EVAP Purge Solenoid | Evaporative emissions purge solenoid circuit | **CATALOGUED** |

---

## 📂 Project Directory Structure

```text
hondaecu-cli/
├── Cargo.toml
├── README.md
├── LICENSE
└── src/
    ├── main.rs         # CLI Entrypoint & subcommand dispatcher
    ├── cpu.rs          # OKI MSM66207 CPU Register File & PSW Flags
    ├── bus.rs          # Memory Bus (32KB ROM / 4KB RAM), Timers, SFRs, ADC, PWM
    ├── engine.rs       # Virtual Engine Simulator, Sensor Signals, Distributor Pulses
    ├── interpreter.rs  # Machine Code Opcode Decoder & Interpreter
    ├── interrupts.rs   # ISR Vector Dispatcher (TM0-TM3, INT0, INT1, Serial RX)
    ├── dtc.rs          # Honda OBD1 DTC metadata and selected synthetic checks
    ├── suite.rs        # Observable ROM and hardware sanity checks
    └── interactive.rs  # Interactive Debugging REPL Shell
```

---

## 📜 License

This is free and unencumbered software released into the public domain under [The Unlicense](LICENSE).
