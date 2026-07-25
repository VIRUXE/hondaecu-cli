# v0.1.0 - Honda OBD1 ECU (OKI MSM66207) Emulator & Testing CLI

First official release of **`hondaecu-cli`**, a high-performance command-line emulator and ROM testing suite in Rust for Honda OBD1 ECUs (P28, P30, P72, PR4).

### 🚀 Highlights & Features

- **Byte-Precise OKI MSM66207 Virtual CPU Core**: Interprets raw machine code opcode bytes line-by-line using a virtual register file (`A`, `DP`, `X1`, `X2`, `USP`, `SSP`, `LRB`, `er0`–`er3`, `r0`–`r7`) with word/byte accumulator mode switching (`DD` flag) and borrow-correct carry flags (`CF`).
- **Microsecond-Accurate ISR Timing**: Simulates hardware timers (`TM0`–`TM3`, `TMR0`–`TMR3`) and distributor position pulse interrupts (`INT0` CKP crankshaft pulse and `INT1` TDC/CYP cylinder position pulse).
- **ADC Hardware & Sensor Simulation**: Simulates analog-to-digital conversion (`ADSCAN`, `ADCR0`–`ADCR7`) for engine sensors: MAP, TPS, ECT, IAT, O2, Battery Voltage, and VSS.
- **VTEC Spool Valve & Pressure Switch**: Models VTEC spool valve solenoid engagement outputs and oil pressure switch feedback hysteresis logic.
- **UART Serial Datalogging Protocol**: Implements serial RX/TX protocol handler (vector `0x01FD`, Baud rate 38400, command frames like `0x20` sensor payload data).
- **85-Test Exhaustive ROM Matrix Suite**: Sweeps 400 fuel map grid cells, 400 ignition map timing cells, environmental trims (cold start, battery dead-time, overheat retard), and rev limiters.
- **Complete 30-DTC OBD1 Honda Fault Code Support**: Covers all official Honda OBD1 fault codes (DTC 0 through DTC 92).
- **Interactive REPL Console**: Live interactive shell to step instructions, set RPM/MAP/TPS parameters, dump memory, and monitor calculated fuel injection pulse width and IACV duty cycles.
- **Blazing Performance**: Executes **9.01+ Million Instructions per second** (~9.01 MHz virtual CPU speed) in Rust.

### 📦 Assets
- `hondaecu-cli-v0.1.0-x86_64-unknown-linux-gnu.tar.gz` (Pre-compiled x86_64 Linux Binary)
