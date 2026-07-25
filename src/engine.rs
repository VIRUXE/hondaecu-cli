// Honda OBD1 D16Z6 / P28 Engine Simulator

use crate::bus::Bus;

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

    /// Update ADC channels on the Bus from current physical sensor parameters
    pub fn sync_sensors_to_bus(&self, bus: &mut Bus) {
        // Channel 0: MAP Sensor (10..250 kPa -> 0.5V..4.5V -> 102..920 ADC counts)
        let map_counts = ((self.map_kpa - 10.0) / 240.0 * 818.0 + 102.0).clamp(0.0, 1023.0) as u16;
        bus.adc_inputs[0] = map_counts;

        // Channel 1: TPS Sensor (0..100% -> 0.5V..4.5V -> 102..920 ADC counts)
        let tps_counts = (self.tps_pct / 100.0 * 818.0 + 102.0).clamp(0.0, 1023.0) as u16;
        bus.adc_inputs[1] = tps_counts;

        // Channel 2: ECT Temp Sensor (NTC thermistor mapping)
        // High temp = low resistance = low voltage = low ADC count
        let ect_counts = (1023.0 - ((self.ect_celsius + 40.0) / 160.0 * 900.0)).clamp(0.0, 1023.0) as u16;
        bus.adc_inputs[2] = ect_counts;

        // Channel 3: IAT Temp Sensor
        let iat_counts = (1023.0 - ((self.iat_celsius + 40.0) / 160.0 * 900.0)).clamp(0.0, 1023.0) as u16;
        bus.adc_inputs[3] = iat_counts;

        // Channel 4: O2 Sensor (0.0..1.0V -> 0..205 ADC counts)
        let o2_counts = (self.o2_volts / 5.0 * 1023.0).clamp(0.0, 1023.0) as u16;
        bus.adc_inputs[4] = o2_counts;

        // Channel 5: Battery Voltage (Scaled voltage divider 0..18V -> 0..1023)
        let vbatt_counts = (self.vbatt_volts / 20.0 * 1023.0).clamp(0.0, 1023.0) as u16;
        bus.adc_inputs[5] = vbatt_counts;

        // Channel 6: Vehicle Speed Sensor / Aux
        let vss_counts = (self.speed_kmh / 250.0 * 1023.0).clamp(0.0, 1023.0) as u16;
        bus.adc_inputs[6] = vss_counts;
    }

    /// Check if distributor CKP (INT0) or TDC/CYP (INT1) pulse should fire based on elapsed CPU cycles
    pub fn check_distributor_pulses(&mut self, total_cycles: u64, cpu_freq_hz: u64) -> u16 {
        if self.rpm <= 0.0 {
            return 0;
        }

        // 4-cylinder 4-stroke engine: 2 ignition events per revolution
        // CKP fires every 180 degrees of crankshaft rotation
        let cycles_per_rev = (cpu_freq_hz as f64 * 60.0) / self.rpm;
        let cycles_per_ckp = cycles_per_rev / 2.0;

        let elapsed = total_cycles - self.last_int0_cycle;
        if elapsed as f64 >= cycles_per_ckp {
            self.last_int0_cycle = total_cycles;
            self.ckp_pulse_count += 1;

            let mut irq: u16 = 1 << 5; // INT0 (CKP) interrupt bit

            // TDC pulse fires every 4 CKP pulses (once per 720 degrees cycle per cylinder)
            if self.ckp_pulse_count % 4 == 0 {
                self.tdc_pulse_count += 1;
                irq |= 1 << 9; // INT1 (TDC/CYP) interrupt bit
            }

            return irq;
        }

        0
    }
}
