// Honda OBD1 ECU Complete Diagnostic Trouble Code (DTC / MIL) Subsystem
// Covers all 30 official Honda OBD1 fault codes (Code 0 through Code 92).
// Only the catalogue of names: check reads the codes the ROM itself blinks.

use crate::bus::{P28_ADC_EGR, P28_ADC_MAP, P28_ADC_TPS, P28_MUX_U5_ADCR1, P28_MUX_U6_ADCR0};

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
    DtcInfo { code: DtcCode::Dtc12_EgrSystemValve, number: 12, name: "EGR System / Lift Sensor", description: "EGR valve position sensor out of range", channel_index: Some(P28_ADC_EGR) },
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

/// The name of a Honda OBD1 fault code, if the table has it.
pub fn code_name(number: u8) -> Option<&'static str> {
    ALL_HONDA_OBD1_DTCS.iter().find(|d| d.number == number).map(|d| d.name)
}
