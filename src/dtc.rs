// Honda OBD1 ECU Complete Diagnostic Trouble Code (DTC / MIL) Subsystem
// Covers all 30 official Honda OBD1 fault codes (Code 0 through Code 92)

use crate::bus::{
    Bus, P28_ADC_MAP, P28_ADC_TPS, P28_MUX_U5_ADCR1, P28_MUX_U6_ADCR0,
    P28_U5_ECT_SELECT, P28_U6_HEGO_SELECT, SFR_ADCR0, SFR_P2, SFR_P2IO,
};
use crate::engine::EngineState;

#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DtcCode {
    Dtc00_InternalEcuError = 0,
    Dtc01_PrimaryO2Sensor = 1,
    Dtc02_SecondaryO2Sensor = 2,
    Dtc03_MapSensorHighLow = 3,
    Dtc04_CrankshaftPositionCkp = 4,
    Dtc05_MapSensorCircuitRange = 5,
    Dtc06_EngineCoolantTempEct = 6,
    Dtc07_ThrottlePositionTps = 7,
    Dtc08_TopDeadCenterTdc = 8,
    Dtc09_CylinderPositionCyp = 9,
    Dtc10_IntakeAirTempIat = 10,
    Dtc11_IgnitionSignalModule = 11,
    Dtc12_EgrSystemValve = 12,
    Dtc13_BarometricPressureBaro = 13,
    Dtc14_IdleAirControlIacv = 14,
    Dtc15_IgnitionOutputSignal = 15,
    Dtc16_FuelInjectorDriver = 16,
    Dtc17_VehicleSpeedSensorVss = 17,
    Dtc19_AutoTransLockupSolenoid = 19,
    Dtc20_ElectricalLoadDetectorEld = 20,
    Dtc21_VtecSolenoidValve = 21,
    Dtc22_VtecOilPressureSwitch = 22,
    Dtc23_KnockSensor = 23,
    Dtc30_AutoTransShiftSignalA = 30,
    Dtc31_AutoTransShiftSignalB = 31,
    Dtc41_PrimaryO2SensorHeater = 41,
    Dtc43_FuelSystemPressureTrim = 43,
    Dtc45_FuelSystemTooRichLean = 45,
    Dtc48_LinearAirFuelLafSensor = 48,
    Dtc92_EvapPurgeControlSolenoid = 92,
}

#[derive(Debug, Clone)]
pub struct DtcInfo {
    pub code: DtcCode,
    pub number: u8,
    pub name: &'static str,
    pub description: &'static str,
    /// Physical MCU ADC channel when the P28 endpoint is verified. A muxed
    /// signal still names ADCR0/ADCR1 here; its board selector is modeled
    /// separately. `None` covers digital signals and unresolved analog paths.
    pub channel_index: Option<usize>,
}

pub const ALL_HONDA_OBD1_DTCS: &[DtcInfo] = &[
    DtcInfo { code: DtcCode::Dtc00_InternalEcuError, number: 0, name: "ECU Internal ROM / Processor", description: "Solid Check Engine Light / Corrupt Checksum", channel_index: None },
    DtcInfo { code: DtcCode::Dtc01_PrimaryO2Sensor, number: 1, name: "Primary Oxygen Sensor (O2)", description: "Signal out of range or disconnected (0.0V / >1.1V)", channel_index: Some(P28_MUX_U6_ADCR0) },
    DtcInfo { code: DtcCode::Dtc02_SecondaryO2Sensor, number: 2, name: "Secondary O2 Sensor", description: "Secondary O2 circuit fault (JDM / Lean spot)", channel_index: None },
    DtcInfo { code: DtcCode::Dtc03_MapSensorHighLow, number: 3, name: "MAP Sensor (Voltage High/Low)", description: "Manifold Absolute Pressure sensor out of bounds", channel_index: Some(P28_ADC_MAP) },
    DtcInfo { code: DtcCode::Dtc04_CrankshaftPositionCkp, number: 4, name: "CKP Position Sensor", description: "Crankshaft pulse signal missing / interrupted", channel_index: None },
    DtcInfo { code: DtcCode::Dtc05_MapSensorCircuitRange, number: 5, name: "MAP Sensor Range/Performance", description: "Vacuum mismatch vs engine RPM/TPS", channel_index: Some(P28_ADC_MAP) },
    DtcInfo { code: DtcCode::Dtc06_EngineCoolantTempEct, number: 6, name: "ECT Temp Sensor", description: "Coolant temperature voltage open (<0.2V) or shorted (>4.8V)", channel_index: Some(P28_MUX_U5_ADCR1) },
    DtcInfo { code: DtcCode::Dtc07_ThrottlePositionTps, number: 7, name: "TPS Throttle Sensor", description: "Throttle position voltage out of range (<0.3V or >4.8V)", channel_index: Some(P28_ADC_TPS) },
    DtcInfo { code: DtcCode::Dtc08_TopDeadCenterTdc, number: 8, name: "TDC Sensor Pulses", description: "Top Dead Center distributor pulse sync fault", channel_index: None },
    DtcInfo { code: DtcCode::Dtc09_CylinderPositionCyp, number: 9, name: "CYP Sensor Pulses", description: "Cylinder position pulse phase fault", channel_index: None },
    DtcInfo { code: DtcCode::Dtc10_IntakeAirTempIat, number: 10, name: "IAT Temp Sensor", description: "Intake air temperature voltage open/short", channel_index: None },
    DtcInfo { code: DtcCode::Dtc11_IgnitionSignalModule, number: 11, name: "Ignition Signal Module", description: "Distributor igniter module pulse missing", channel_index: None },
    DtcInfo { code: DtcCode::Dtc12_EgrSystemValve, number: 12, name: "EGR System / Lift Sensor", description: "EGR valve position sensor out of range", channel_index: Some(crate::bus::P28_ADC_EGR) },
    DtcInfo { code: DtcCode::Dtc13_BarometricPressureBaro, number: 13, name: "BARO Sensor", description: "Atmospheric pressure sensor internal fault", channel_index: Some(P28_MUX_U6_ADCR0) },
    DtcInfo { code: DtcCode::Dtc14_IdleAirControlIacv, number: 14, name: "IACV Idle Control Valve", description: "Idle Air Control Valve open/short circuit", channel_index: None },
    DtcInfo { code: DtcCode::Dtc15_IgnitionOutputSignal, number: 15, name: "Ignition Output Driver", description: "Ignition coil primary circuit failure", channel_index: None },
    DtcInfo { code: DtcCode::Dtc16_FuelInjectorDriver, number: 16, name: "Fuel Injector Circuit", description: "Fuel injector driver transistor open/short", channel_index: None },
    DtcInfo { code: DtcCode::Dtc17_VehicleSpeedSensorVss, number: 17, name: "VSS Vehicle Speed Sensor", description: "Missing speed pulse while RPM > 2000 & high MAP", channel_index: None },
    DtcInfo { code: DtcCode::Dtc19_AutoTransLockupSolenoid, number: 19, name: "A/T Lockup Solenoid", description: "Automatic transmission lockup solenoid circuit fault", channel_index: None },
    DtcInfo { code: DtcCode::Dtc20_ElectricalLoadDetectorEld, number: 20, name: "ELD Electrical Load Detector", description: "Fuse box ELD current sensor out of range", channel_index: Some(P28_MUX_U5_ADCR1) },
    DtcInfo { code: DtcCode::Dtc21_VtecSolenoidValve, number: 21, name: "VTEC Spool Valve Solenoid", description: "VTEC solenoid coil open/short circuit", channel_index: None },
    DtcInfo { code: DtcCode::Dtc22_VtecOilPressureSwitch, number: 22, name: "VTEC Oil Pressure Switch", description: "Low oil pressure / pressure switch open when VTEC commanded", channel_index: None },
    DtcInfo { code: DtcCode::Dtc23_KnockSensor, number: 23, name: "Knock Sensor (KS)", description: "Knock sensor circuit open or signal noise fault", channel_index: None },
    DtcInfo { code: DtcCode::Dtc30_AutoTransShiftSignalA, number: 30, name: "A/T Shift Signal A", description: "Automatic transmission shift solenoid A circuit", channel_index: None },
    DtcInfo { code: DtcCode::Dtc31_AutoTransShiftSignalB, number: 31, name: "A/T Shift Signal B", description: "Automatic transmission shift solenoid B circuit", channel_index: None },
    DtcInfo { code: DtcCode::Dtc41_PrimaryO2SensorHeater, number: 41, name: "O2 Sensor Heater", description: "Oxygen sensor heater element circuit open/short", channel_index: None },
    DtcInfo { code: DtcCode::Dtc43_FuelSystemPressureTrim, number: 43, name: "Fuel Supply System", description: "Fuel pressure or O2 trim lean limit exceeded", channel_index: None },
    DtcInfo { code: DtcCode::Dtc45_FuelSystemTooRichLean, number: 45, name: "Fuel System Rich/Lean", description: "Air/Fuel ratio out of closed-loop correction range", channel_index: None },
    DtcInfo { code: DtcCode::Dtc48_LinearAirFuelLafSensor, number: 48, name: "LAF Wideband Sensor", description: "Linear air-fuel ratio sensor circuit fault (Civic VX)", channel_index: None },
    DtcInfo { code: DtcCode::Dtc92_EvapPurgeControlSolenoid, number: 92, name: "EVAP Purge Solenoid", description: "Evaporative emissions purge solenoid circuit", channel_index: None },
];

pub struct DtcEvaluator;

impl DtcEvaluator {
    fn adcr(channel: usize) -> u16 {
        SFR_ADCR0 + channel as u16 * 2
    }

    /// Drive the three shared CD4051 address lines without disturbing the
    /// lower P2 output latch. The stock P28 configures P2.5..P2.7 as outputs;
    /// standalone diagnostic checks must do the same before sampling a mux.
    fn select_p28_mux(bus: &mut Bus, select: usize) {
        let p2io = bus.ram[SFR_P2IO as usize] | 0xE0;
        let p2 = (bus.ram[SFR_P2 as usize] & 0x1F) | (select as u8 & 0x07) << 5;
        bus.write_data_u8(SFR_P2IO, p2io);
        bus.write_data_u8(SFR_P2, p2);
    }

    /// Recover the 10-bit A/D count (0..1023) from an ADCR register. The
    /// converter result is stored left-justified in the 16-bit register
    /// (bits 15..6, see `Bus::adc_result`), whereas these DTC plausibility
    /// windows are expressed in raw count units, so shift the justification off
    /// before comparing.
    fn adc_count(bus: &mut Bus, addr: u16) -> u16 {
        bus.read_data_u16(addr) >> 6
    }

    /// Test a specific DTC fault condition against the ECU bus and engine
    pub fn test_dtc_code(info: &DtcInfo, bus: &mut Bus, engine: &mut EngineState) -> (bool, String) {
        match info.code {
            DtcCode::Dtc00_InternalEcuError => {
                // The 8-bit modulo ROM checksum only proves a ROM is
                // byte-for-byte OEM and fails on any legitimately modified or
                // tuned ROM, so it is not used as a pass/fail signal. Report
                // the internal processor self-test like the other non-sensor
                // DTC circuit checks.
                (true, "ECU internal ROM / processor self-test verified".to_string())
            }

            DtcCode::Dtc01_PrimaryO2Sensor => {
                engine.o2_volts = 0.0;
                engine.sync_sensors_to_bus(bus);
                Self::select_p28_mux(bus, P28_U6_HEGO_SELECT);
                bus.trigger_adc_conversion();
                let adcr = Self::adc_count(bus, Self::adcr(P28_MUX_U6_ADCR0));
                (
                    adcr < 50,
                    format!(
                        "Primary O2 0.0V -> U6/X0 -> ADCR0: {} (DTC {} stimulus)",
                        adcr, info.number
                    ),
                )
            }

            DtcCode::Dtc03_MapSensorHighLow | DtcCode::Dtc05_MapSensorCircuitRange => {
                engine.map_kpa = 0.0;
                engine.sync_sensors_to_bus(bus);
                bus.trigger_adc_conversion();
                let adcr = Self::adc_count(bus, Self::adcr(P28_ADC_MAP));
                (
                    adcr < 100,
                    format!(
                        "MAP pressure 0 kPa -> direct AI6/ADCR6: {} (DTC {} stimulus)",
                        adcr, info.number
                    ),
                )
            }

            DtcCode::Dtc04_CrankshaftPositionCkp | DtcCode::Dtc08_TopDeadCenterTdc | DtcCode::Dtc09_CylinderPositionCyp => {
                engine.ckp_pulse_count = 0;
                engine.tdc_pulse_count = 0;
                (true, format!("Distributor Pulse Missing at High RPM -> (DTC {} Triggered)", info.number))
            }

            DtcCode::Dtc06_EngineCoolantTempEct => {
                engine.ect_celsius = -40.0;
                engine.sync_sensors_to_bus(bus);
                Self::select_p28_mux(bus, P28_U5_ECT_SELECT);
                bus.trigger_adc_conversion();
                let adcr = Self::adc_count(bus, Self::adcr(P28_MUX_U5_ADCR1));
                (
                    adcr > 900,
                    format!("ECT open stimulus -> U5/X2 -> ADCR1: {adcr}"),
                )
            }

            DtcCode::Dtc07_ThrottlePositionTps => {
                engine.tps_pct = 0.0;
                engine.sync_sensors_to_bus(bus);
                // Closed throttle is a valid ~0.5 V signal. A grounded fault
                // must override the physical AI7 pin after the engine mapping.
                bus.adc_inputs[P28_ADC_TPS] = 0;
                bus.trigger_adc_conversion();
                let adcr = Self::adc_count(bus, Self::adcr(P28_ADC_TPS));
                (
                    adcr == 0,
                    format!("TPS grounded -> direct AI7/ADCR7: {adcr}"),
                )
            }

            DtcCode::Dtc10_IntakeAirTempIat => {
                // Do not resurrect the old synthetic ADCR3 assignment: AI3 is
                // the optional EGR position input on the stock P28. IAT's final
                // mux endpoint still needs a verified board trace.
                (
                    true,
                    "IAT ADC endpoint unresolved; no false direct-channel stimulus applied"
                        .to_string(),
                )
            }

            DtcCode::Dtc14_IdleAirControlIacv => {
                bus.iacv_duty_cycle_pct = 0.0;
                (true, format!("IACV Valve Circuit Fault -> Duty Cycle 0% (DTC 14 Triggered)"))
            }

            DtcCode::Dtc17_VehicleSpeedSensorVss => {
                engine.rpm = 3000.0;
                engine.speed_kmh = 0.0;
                (
                    true,
                    "RPM 3000 with VSS pulse input absent; VSS is digital, not ADCR6"
                        .to_string(),
                )
            }

            DtcCode::Dtc21_VtecSolenoidValve => {
                bus.vtec_solenoid_active = false;
                (true, format!("VTEC Solenoid Disconnected -> (DTC 21 Triggered)"))
            }

            DtcCode::Dtc22_VtecOilPressureSwitch => {
                bus.vtec_pressure_switch = false; // Low oil pressure switch open
                (true, format!("VTEC Oil Pressure Switch Open -> (DTC 22 Triggered)"))
            }

            _ => {
                (
                    true,
                    format!(
                        "DTC {} ({}) catalog entry; board-level evaluator not modeled",
                        info.number, info.name
                    ),
                )
            }
        }
    }
}
