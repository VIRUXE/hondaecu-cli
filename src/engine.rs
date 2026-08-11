// Honda OBD1 D16Z6 / P28 Engine Simulator

use crate::bus::{
    Bus, IRQ_INT0, IRQ_INT1, P28_ADC_MAP, P28_ADC_TPS, P28_MUX_U5_ADCR1,
    P28_MUX_U6_ADCR0, P28_U5_ECT_SELECT, P28_U6_HEGO_SELECT,
};

// Existing battery-monitor model. Its board net has not yet been traced with
// the same confidence as MAP/TPS/EGR, so it is intentionally not a public P28
// wiring constant.
const ADC_VBATT_MODEL_CHANNEL: usize = 5;

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
    pub last_int0_cycle: u64,
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
        }
    }

    /// Update the P28 board's analog inputs from physical sensor parameters.
    /// U5/U6 are external CD4051s selected by P2.5..P2.7. MAP and TPS use
    /// confirmed direct pins; the existing battery model remains on AI5 while
    /// that board net awaits a full trace.
    pub fn sync_sensors_to_bus(&self, bus: &mut Bus) {
        // MAP: P5.6/AI6 (10..250 kPa -> 0.5V..4.5V -> 102..920 counts)
        let map_counts = ((self.map_kpa - 10.0) / 240.0 * 818.0 + 102.0).clamp(0.0, 1023.0) as u16;
        bus.adc_inputs[P28_ADC_MAP] = map_counts;

        // TPS: P5.7/AI7 (0..100% -> 0.5V..4.5V -> 102..920 counts)
        let tps_counts = (self.tps_pct / 100.0 * 818.0 + 102.0).clamp(0.0, 1023.0) as u16;
        bus.adc_inputs[P28_ADC_TPS] = tps_counts;

        // ECT: U5/X2. High temperature lowers the NTC voltage.
        // High temp = low resistance = low voltage = low ADC count
        let ect_counts = (1023.0 - ((self.ect_celsius + 40.0) / 160.0 * 900.0)).clamp(0.0, 1023.0) as u16;
        bus.p28_mux_inputs[P28_MUX_U5_ADCR1][P28_U5_ECT_SELECT] = ect_counts;

        // The IAT field remains available to scenarios and telemetry, but its
        // final P28 mux/ADC endpoint is not yet verified from the schematic.

        // Primary HEGO: conditioned by U22, then U6/X0.
        let o2_counts = (self.o2_volts / 5.0 * 1023.0).clamp(0.0, 1023.0) as u16;
        bus.p28_mux_inputs[P28_MUX_U6_ADCR0][P28_U6_HEGO_SELECT] = o2_counts;

        // Battery monitor: retain the existing AI5 model pending a full net trace.
        let vbatt_counts = (self.vbatt_volts / 20.0 * 1023.0).clamp(0.0, 1023.0) as u16;
        bus.adc_inputs[ADC_VBATT_MODEL_CHANNEL] = vbatt_counts;

        // VSS is a conditioned digital pulse input on the P28, not an ADC
        // channel. `speed_kmh` remains available to scenarios and telemetry.

        // VTEC spool valve. On the real board this output is driven by the ASIC
        // (schematic pin A25, active-low low-side driver), not a directly
        // modeled OKI GPIO, so it cannot be pin-sensed from the emulated ports.
        // Derive engagement from the documented P28 crossover conditions and
        // the oil-pressure switch input instead.
        bus.vtec_solenoid_active = self.vtec_engaged(bus.vtec_pressure_switch);
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

    /// Check if distributor CKP (INT0) or TDC (INT1) pulse should fire based on
    /// elapsed CPU cycles, and raise the CYP cylinder-1 marker on the bus's
    /// transition-detector status once per camshaft revolution.
    pub fn check_distributor_pulses(
        &mut self,
        bus: &mut Bus,
        total_cycles: u64,
        cpu_freq_hz: u64,
    ) -> u16 {
        if self.rpm <= 0.0 {
            return 0;
        }

        // Honda OBD1 distributors provide 24 CKP edges per cam revolution,
        // i.e. 12 per crank revolution. TDC provides four edges per cam
        // revolution, so one TDC edge follows every six CKP edges. CYP provides
        // one edge per cam revolution (every 24 CKP) identifying cylinder #1;
        // it is not an interrupt but a transition-detector input the ROM polls,
        // so it is delivered by setting SFR_TRNS bit 0 rather than an IRQ.
        let cycles_per_rev = (cpu_freq_hz as f64 * 60.0) / self.rpm;
        let cycles_per_ckp = cycles_per_rev / 12.0;

        let elapsed = total_cycles - self.last_int0_cycle;
        if elapsed as f64 >= cycles_per_ckp {
            self.last_int0_cycle = total_cycles;
            self.ckp_pulse_count += 1;

            let mut irq: u16 = 1 << IRQ_INT0;

            if self.ckp_pulse_count % 6 == 0 {
                self.tdc_pulse_count += 1;
                irq |= 1 << IRQ_INT1;
            }
            // CYP marks cylinder #1 once per camshaft revolution, phased to the
            // cylinder-1 TDC edge (every 24th CKP, which is also a TDC). The
            // transition detector latches it; the TDC ISR reads and clears it.
            if self.ckp_pulse_count % 24 == 0 {
                bus.signal_cyp();
            }

            return irq;
        }

        0
    }
}
