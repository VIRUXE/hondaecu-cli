// Honda OBD1 D16Z6 / P28 Engine Simulator

use crate::bus::{
    Bus, IRQ_INT0, IRQ_INT1, P28_ADC_MAP, P28_ADC_TPS, P28_CKP_CAPTURE_TIMER,
    P28_MUX_U5_ADCR1, P28_MUX_U6_ADCR0, P28_U5_ECT_SELECT, P28_U6_HEGO_SELECT,
    P28_U6_IAT_SELECT, P28_U6_VBATT_SELECT, P28_VBATT_COUNTS_PER_VOLT,
};

/// Stock Honda IAT sensor, (deg C, volts at the ECU), from the pgmfi wiki's
/// "Intake Air Temperature Sensor" page (values attributed there to AEM).
const IAT_VOLTS: [(f64, f64); 12] = [
    (-10.0, 4.992),
    (-5.0, 4.368),
    (1.0, 3.900),
    (9.0, 3.276),
    (18.0, 2.808),
    (27.0, 2.340),
    (38.0, 1.872),
    (51.0, 1.404),
    (66.0, 0.936),
    (83.0, 0.468),
    (95.0, 0.156),
    (100.0, 0.0),
];

/// Linear interpolation through `IAT_VOLTS`, clamped at both ends.
fn iat_volts(celsius: f64) -> f64 {
    let (first, last) = (IAT_VOLTS[0], IAT_VOLTS[IAT_VOLTS.len() - 1]);
    if celsius <= first.0 {
        return first.1;
    }
    if celsius >= last.0 {
        return last.1;
    }
    let i = IAT_VOLTS.iter().position(|&(t, _)| t >= celsius).unwrap();
    let ((t0, v0), (t1, v1)) = (IAT_VOLTS[i - 1], IAT_VOLTS[i]);
    v0 + (v1 - v0) * (celsius - t0) / (t1 - t0)
}

/// Stock Honda OBD1 coolant sensor, (deg C, volts at the ECU).
///
/// Source: eCtune's `temperatureSensorCurve` (`ectune-cobrartp`,
/// `source/eCtuneBin/Calibration.cs`), which gives deg C for each 8-bit ECU
/// reading 00h..FFh and which eCtune applies to both ECT and IAT. Every 16th
/// byte is kept (one-byte error at most against the full curve); its volts
/// are the middle of that byte's ADC bin, `(4 b + 2) / 1023 * 5`.
///
/// Cross-checks: Ed Gibson's lab-mapped table on the pgmfi wiki
/// ("Engine Coolant Temperature" page, '93 CRX, 0..100 deg C only) agrees
/// within 1.3 deg C from 20 to 100 deg C; and the ROM's coolant thresholds
/// on RAM C1h fall at plausible temperatures: 44h (VTEC allowed below it,
/// 0x11C7) at 59.7 deg C, 3Bh (fail-safe) at 65.4, 2Eh at 75.1, 20h at 88.8
/// (docs/CHECK.md). Not shared with `IAT_VOLTS`, whose AEM-attributed values
/// differ from this curve.
const ECT_VOLTS: [(f64, f64); 17] = [
    (-26.2, 4.995), // FFh
    (-18.1, 4.702), // F0h
    (-9.4, 4.389),  // E0h
    (-1.1, 4.076),  // D0h
    (6.7, 3.763),   // C0h
    (13.8, 3.451),  // B0h
    (20.4, 3.138),  // A0h
    (26.7, 2.825),  // 90h
    (32.9, 2.512),  // 80h
    (39.1, 2.199),  // 70h
    (45.8, 1.887),  // 60h
    (53.2, 1.574),  // 50h
    (62.1, 1.261),  // 40h
    (73.5, 0.948),  // 30h
    (88.8, 0.635),  // 20h
    (110.2, 0.323), // 10h
    (141.0, 0.010), // 00h
];

/// Linear interpolation through `ECT_VOLTS`, clamped at both ends.
fn ect_volts(celsius: f64) -> f64 {
    let (first, last) = (ECT_VOLTS[0], ECT_VOLTS[ECT_VOLTS.len() - 1]);
    if celsius <= first.0 {
        return first.1;
    }
    if celsius >= last.0 {
        return last.1;
    }
    let i = ECT_VOLTS.iter().position(|&(t, _)| t >= celsius).unwrap();
    let ((t0, v0), (t1, v1)) = (ECT_VOLTS[i - 1], ECT_VOLTS[i]);
    v0 + (v1 - v0) * (celsius - t0) / (t1 - t0)
}

/// TDC edges fall half a tooth after CKP teeth 0, 6, 12 and 18 of the cam
/// revolution; CYP falls half a tooth after tooth `CYP_AFTER_TOOTH`.
///
/// The half-tooth offsets are a working choice, but the spacing between the
/// two is the ROM's: its per-tooth handler counts teeth since the last TDC
/// edge in RAM D2h (0x0417..0x0466) and, on the tooth where it sees CYP,
/// requires that count to be 3 (0x04B5), else it raises `9Dh.2` (fault index
/// 2Bh, code 9). A TDC edge after tooth 0 is seen at tooth 1 (count 0), so
/// CYP has to be seen at tooth 4, i.e. fall after tooth 3.
const TDC_AFTER_TOOTH: u32 = 0;
const CYP_AFTER_TOOTH: u32 = 3;

/// Honda's speed sensor gives four pulses per turn of a 637 rev/km speedometer
/// drive. Used only to give INT0 a plausible rate; not derived from the ROM.
const VSS_PULSES_PER_KM: f64 = 4.0 * 637.0;


#[derive(Debug, Clone)]
pub struct EngineState {
    pub rpm: f64,
    pub map_kpa: f64,          // Manifold Absolute Pressure (10..250 kPa)
    pub tps_pct: f64,          // Throttle Position (0..100 %)
    pub ect_celsius: f64,      // Engine Coolant Temperature (-40..120 °C)
    pub iat_celsius: f64,      // Intake Air Temperature (-40..120 °C)
    pub o2_volts: f64,         // Oxygen Sensor Voltage (0.0..1.0 V)
    pub vbatt_volts: f64,      // Battery Voltage (0.0..18.0 V)
    pub speed_kmh: f64,        // Vehicle Speed (0..250 km/h)
    
    pub ckp_pulse_count: u64,
    pub tdc_pulse_count: u64,
    /// Cycle of the most recent distributor half-tooth edge.
    pub last_int0_cycle: u64,
    /// Position within the cam revolution, in half CKP teeth (0..48).
    half_tooth_count: u32,
    next_half_tooth_cycle: Option<f64>,
    next_vss_cycle: Option<f64>,
}

impl EngineState {
    pub fn new() -> Self {
        Self {
            rpm: 800.0,            // Normal idle RPM
            map_kpa: 30.0,         // Normal idle vacuum (~30 kPa)
            tps_pct: 0.0,          // Closed throttle
            ect_celsius: 85.0,     // Operating temp
            iat_celsius: 25.0,     // Ambient air temp
            o2_volts: 0.45,        // Stoichiometric lambda
            vbatt_volts: 14.2,     // Alternator running voltage
            speed_kmh: 0.0,
            ckp_pulse_count: 0,
            tdc_pulse_count: 0,
            last_int0_cycle: 0,
            half_tooth_count: 0,
            next_half_tooth_cycle: None,
            next_vss_cycle: None,
        }
    }

    /// Update the P28 board's analog inputs from physical sensor parameters.
    /// U5/U6 are external CD4051s selected by P2.5..P2.7. MAP and TPS use
    /// confirmed direct pins; battery voltage and IAT are on U6 (see the
    /// `P28_U6_*` constants for the evidence).
    pub fn sync_sensors_to_bus(&self, bus: &mut Bus) {
        // MAP: P5.6/AI6 (10..250 kPa -> 0.5V..4.5V -> 102..920 counts)
        let map_counts = ((self.map_kpa - 10.0) / 240.0 * 818.0 + 102.0).clamp(0.0, 1023.0) as u16;
        bus.adc_inputs[P28_ADC_MAP] = map_counts;

        // TPS: P5.7/AI7 (0..100% -> 0.5V..4.5V -> 102..920 counts)
        let tps_counts = (self.tps_pct / 100.0 * 818.0 + 102.0).clamp(0.0, 1023.0) as u16;
        bus.adc_inputs[P28_ADC_TPS] = tps_counts;

        // ECT: U5/X2, through the sourced thermistor curve (`ECT_VOLTS`).
        let ect_counts = (ect_volts(self.ect_celsius) / 5.0 * 1023.0).clamp(0.0, 1023.0) as u16;
        bus.p28_mux_inputs[P28_MUX_U5_ADCR1][P28_U5_ECT_SELECT] = ect_counts;

        // IAT: U6/X2.
        let iat_counts = (iat_volts(self.iat_celsius) / 5.0 * 1023.0).clamp(0.0, 1023.0) as u16;
        bus.p28_mux_inputs[P28_MUX_U6_ADCR0][P28_U6_IAT_SELECT] = iat_counts;

        // Primary HEGO: conditioned by U22, then U6/X0.
        let o2_counts = (self.o2_volts / 5.0 * 1023.0).clamp(0.0, 1023.0) as u16;
        bus.p28_mux_inputs[P28_MUX_U6_ADCR0][P28_U6_HEGO_SELECT] = o2_counts;

        // Battery voltage: U6/X7, 10.4 eight-bit counts per volt.
        let vbatt_counts =
            (self.vbatt_volts * P28_VBATT_COUNTS_PER_VOLT * 4.0).clamp(0.0, 1023.0) as u16;
        bus.p28_mux_inputs[P28_MUX_U6_ADCR0][P28_U6_VBATT_SELECT] = vbatt_counts;

        // VSS is a conditioned digital pulse input on the P28, not an ADC
        // channel. `speed_kmh` remains available to scenarios and telemetry.

        // VTEC spool valve, scenario-side model: engagement derived from the
        // documented P28 crossover conditions and the oil-pressure switch.
        // What the ROM itself commands is 82C55 port C bit 0, available as
        // `Bus::p28_outputs().vtec_solenoid`; the two are not reconciled yet.
        bus.vtec_solenoid_active = self.vtec_engaged(bus.vtec_pressure_switch_closed());
    }

    /// VTEC spool-valve engagement per the documented P28 low-to-high cam
    /// crossover conditions (docs/P28_ARCHITECTURE.md): all must hold at once.
    pub fn vtec_engaged(&self, oil_pressure_switch: bool) -> bool {
        self.rpm >= 4800.0
            && self.tps_pct >= 20.0
            && self.ect_celsius >= 60.0
            && self.speed_kmh >= 5.0
            && oil_pressure_switch
    }

    /// Advance the distributor and vehicle-speed sensors to `total_cycles` and
    /// deliver their edges to the pins the stock P28 ROM reads them on:
    ///
    /// | signal | rate | MCU input | ROM consumer |
    /// |---|---|---|---|
    /// | CKP | 24 / cam rev | P3.6/TM2IO, timer-2 capture | 0x014D -> soft IRQ 3 -> 0x03DC |
    /// | TDC | 4 / cam rev | P3.3/INT1 | polled as IRQH.7; 0x037B in backup mode |
    /// | CYP | 1 / cam rev | P4.4/TRNS0 | polled as TRNSIT.0 |
    /// | VSS | speed | P3.2/INT0 | 0x01D3 (period capture off TM2) |
    ///
    /// The CKP/TDC/CYP assignment was derived from the ROM -- the timer-2
    /// event chain counts RAM 12Ah from 0 to 23 and re-zeroes it on TRNSIT.0,
    /// while 0x0403 expects IRQH.7 once every six of those events -- and it
    /// is what Bomarc sheets 2, 4 and 6 show. See docs/P28_ARCHITECTURE.md,
    /// "Crank-angle inputs".
    ///
    /// Returns the IRQ bits raised. At most one distributor half-tooth is
    /// emitted per call; callers invoke this after every instruction, which
    /// is far finer than a half tooth at any realistic engine speed.
    pub fn check_distributor_pulses(
        &mut self,
        bus: &mut Bus,
        total_cycles: u64,
        cpu_freq_hz: u64,
    ) -> u16 {
        let mut irq = self.check_vss_pulse(total_cycles, cpu_freq_hz);
        if self.rpm <= 0.0 {
            self.next_half_tooth_cycle = None;
            return irq;
        }

        // 12 CKP teeth per crank revolution; the model steps in half teeth so
        // TDC and CYP can sit between CKP edges like the separate reluctors
        // they are, instead of racing a CKP edge in the same instant.
        let cycles_per_half_tooth = (cpu_freq_hz as f64 * 60.0) / self.rpm / 24.0;
        let now = total_cycles as f64;
        let due = *self.next_half_tooth_cycle.get_or_insert(now + cycles_per_half_tooth);
        if now < due {
            return irq;
        }
        // Schedule from the edge's nominal time, not from `now`, so that
        // instruction granularity does not stretch the period (issue #5). If
        // the engine speed jumped far enough to leave the schedule behind,
        // re-anchor rather than emit a burst.
        let next = due + cycles_per_half_tooth;
        self.next_half_tooth_cycle = Some(if next <= now { now + cycles_per_half_tooth } else { next });
        self.last_int0_cycle = total_cycles;

        let half = self.half_tooth_count;
        self.half_tooth_count = (half + 1) % 48;
        let tooth = half / 2;
        if half % 2 == 0 {
            self.ckp_pulse_count += 1;
            irq |= bus.timer_capture_edge(P28_CKP_CAPTURE_TIMER);
        } else {
            if tooth % 6 == TDC_AFTER_TOOTH {
                self.tdc_pulse_count += 1;
                irq |= 1 << IRQ_INT1;
            }
            if tooth == CYP_AFTER_TOOTH {
                bus.signal_cyp();
            }
        }
        irq
    }

    /// Vehicle-speed sensor on INT0. The ROM's INT0 handler (0x01D3) measures
    /// the edge period against TM2 with an overflow extension, which fits a
    /// slow speed signal and not the 24-tooth crank signal.
    fn check_vss_pulse(&mut self, total_cycles: u64, cpu_freq_hz: u64) -> u16 {
        if self.speed_kmh <= 0.0 {
            self.next_vss_cycle = None;
            return 0;
        }
        let pulses_per_second = self.speed_kmh * VSS_PULSES_PER_KM / 3600.0;
        let period = cpu_freq_hz as f64 / pulses_per_second;
        let now = total_cycles as f64;
        let due = *self.next_vss_cycle.get_or_insert(now + period);
        if now < due {
            return 0;
        }
        let next = due + period;
        self.next_vss_cycle = Some(if next <= now { now + period } else { next });
        1 << IRQ_INT0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iat_table_interpolates_and_clamps() {
        assert_eq!(iat_volts(-40.0), 4.992);
        assert_eq!(iat_volts(120.0), 0.0);
        assert_eq!(iat_volts(27.0), 2.340);
        // Halfway between 18 C (2.808 V) and 27 C (2.340 V).
        assert!((iat_volts(22.5) - 2.574).abs() < 1e-9);
    }

    #[test]
    fn ect_table_interpolates_and_clamps() {
        assert_eq!(ect_volts(-40.0), 4.995);
        assert_eq!(ect_volts(160.0), 0.010);
        assert_eq!(ect_volts(88.8), 0.635);
        // Halfway between 62.1 C (1.261 V) and 73.5 C (0.948 V).
        assert!((ect_volts(67.8) - 1.1045).abs() < 1e-9);
    }

    /// The ROM allows VTEC only while RAM C1h, the high byte of the U5/X2
    /// sample, is below 44h (0x11C7). A warm engine must read below that;
    /// a freezing one far above it.
    #[test]
    fn a_warm_engine_reads_low_on_u5_x2() {
        let count_at = |celsius: f64| {
            let mut bus = Bus::new();
            let mut engine = EngineState::new();
            engine.ect_celsius = celsius;
            engine.sync_sensors_to_bus(&mut bus);
            bus.p28_mux_inputs[P28_MUX_U5_ADCR1][P28_U5_ECT_SELECT]
        };
        let vtec_gate = 0x44 * 4;
        assert!(count_at(88.0) < vtec_gate, "{}", count_at(88.0));
        assert!(count_at(0.0) > 3 * vtec_gate, "{}", count_at(0.0));
        // The sampled table lands on the source curve's own byte at 88.8 C.
        assert_eq!(count_at(88.8) >> 2, 0x20);
    }

    /// The distributor is scheduled on the machine-cycle clock. However the
    /// caller's instruction-sized steps fall, one second of cycles carries
    /// one second of teeth: 12 CKP and 2 TDC per crank revolution.
    #[test]
    fn distributor_cadence_follows_the_machine_cycle_clock() {
        use crate::bus::CPU_HZ;

        for rpm in [800.0, 3000.0, 6000.0, 9000.0] {
            let mut bus = Bus::new();
            let mut engine = EngineState::new();
            engine.rpm = rpm;

            // Steps from the shortest instruction (2 cycles) to the longest (47).
            let (mut cycles, mut step) = (0u64, 2u64);
            while cycles < CPU_HZ {
                engine.check_distributor_pulses(&mut bus, cycles, CPU_HZ);
                step = if step >= 47 { 2 } else { step + 5 };
                cycles += step;
            }

            let revs = rpm / 60.0;
            let (ckp, tdc) = (engine.ckp_pulse_count as f64, engine.tdc_pulse_count as f64);
            assert!((ckp - revs * 12.0).abs() <= 1.0, "{rpm} rpm: {ckp} CKP edges in a second");
            assert!((tdc - revs * 2.0).abs() <= 1.0, "{rpm} rpm: {tdc} TDC edges in a second");
        }
    }

    #[test]
    fn battery_and_iat_land_on_analog_mux_u6() {
        let mut bus = Bus::new();
        let mut engine = EngineState::new();
        engine.vbatt_volts = 12.0;
        engine.iat_celsius = 27.0;
        engine.sync_sensors_to_bus(&mut bus);

        // 12 V at 10.4 eight-bit counts per volt is 7Ch, the ROM's 12 V
        // table breakpoint region (7Dh/7Eh).
        assert_eq!(bus.p28_mux_inputs[P28_MUX_U6_ADCR0][P28_U6_VBATT_SELECT] >> 2, 0x7C);
        assert_eq!(bus.p28_mux_inputs[P28_MUX_U6_ADCR0][P28_U6_IAT_SELECT], 478);
    }
}
