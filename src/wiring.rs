//! Applies a `Setup` (what is wired to the box) to the board model (`Bus`).
//!
//! A value the board model cannot represent is not guessed: `apply_static`
//! returns it as unmodelled, with the reason, and the verdict then cannot be
//! "worth burning".

use crate::bus::{
    Bus, P28_IN_AC_SWITCH, P28_IN_AT_GEAR_DRIVE_3, P28_IN_AT_GEAR_DRIVE_4,
    P28_IN_AT_GEAR_POSITION, P28_IN_BRAKE_RELEASED, P28_IN_STARTER, P28_MUX_U5_ADCR1,
    P28_PA_SERVICE_CONNECTOR, P28_U5_ELD_SELECT, P28_U5_GEARBOX_SELECT,
};
use crate::car::{EcuModel, Gearbox, Iacv, O2Sensor, Setup};
use crate::engine::EngineState;
use crate::ppi82c55::PORT_A;

/// 3.2 V on the 10-bit gearbox strap input: an automatic.
pub const GEARBOX_STRAP_AUTOMATIC_COUNTS: u16 = 650;
/// The strap reads low on a manual (`Bus::new`).
const GEARBOX_STRAP_MANUAL_COUNTS: u16 = 0;

/// Driver-operated inputs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Switches {
    pub starter: bool,
    pub ac: bool,
    pub service_connector: bool,
    pub in_gear: bool,
    pub brake: bool,
}

/// A car value the board model cannot represent: the key and why.
pub type Unmodelled = (&'static str, String);

/// Straps and plugs that do not change during a run. Call once after Bench::new.
///
/// Returns the values in `setup` this model cannot represent, each with the
/// reason. A run with any of them cannot be worth burning (`verdict::judge`).
pub fn apply_static(bus: &mut Bus, setup: &Setup) -> Vec<Unmodelled> {
    let mut unmodelled: Vec<Unmodelled> = Vec::new();
    let mut not = |key: &'static str, reason: &str| unmodelled.push((key, reason.to_string()));

    if setup.ecu == EcuModel::P30 {
        not("ecu", "p30: the P30 box's knock board is not modelled");
    }
    if setup.knock_sensor {
        not("knock_sensor", "yes: the knock board and its sensor input are not modelled");
    }
    bus.p28_mux_inputs[P28_MUX_U5_ADCR1][P28_U5_GEARBOX_SELECT] = match setup.board_gearbox {
        Gearbox::Manual => GEARBOX_STRAP_MANUAL_COUNTS,
        Gearbox::Automatic => GEARBOX_STRAP_AUTOMATIC_COUNTS,
    };
    // Only an automatic gearbox has lock-up solenoids. On an automatic board
    // their absence reaches the ROM through the driver feedbacks
    // (`Bus::drive_p28_feedback`); on a manual board the ROM never reads
    // them (216h.3 clear: 0x3C92, 0x5F5C), so an automatic car there
    // changes only the A/T position inputs (`apply_switches`).
    bus.lockup_solenoids_connected = setup.gearbox == Gearbox::Automatic;
    if !setup.vtec {
        bus.vtec_solenoid_connected = false;
        bus.set_vtec_pressure_switch_closed(false);
    }
    bus.o2_heater_connected = matches!(setup.o2_sensor, O2Sensor::Heated);
    if setup.o2_sensor == O2Sensor::None {
        not("o2_sensor", "none: the level the oxygen-sensor input reads with no sensor is not established");
    }
    match setup.iacv {
        Iacv::Stock => bus.iacv_connected = true,
        Iacv::None => bus.iacv_connected = false,
        Iacv::Ohms(r) => not(
            "iacv",
            &format!("{r} ohms: the stock valve's resistance has no source to scale it against"),
        ),
    }
    // Without a sensor AI3 sits on the battery divider (`EngineState::
    // sync_sensors_to_bus`); with one, the lift sensor's level is unsourced.
    bus.egr_fitted = setup.egr;
    if setup.egr {
        not("egr", "yes: the level an EGR lift sensor presents on AI3 has no source");
    }
    // A working detector is left at the model's mid scale.
    if !setup.eld {
        bus.p28_mux_inputs[P28_MUX_U5_ADCR1][P28_U5_ELD_SELECT] = ELD_OPEN_COUNTS;
    }
    if !setup.power_steering_switch {
        not(
            "power_steering_switch",
            "no: the schematic and the pgmfi wiki disagree on the level the input reads with the switch open",
        );
    }
    unmodelled
}

/// U5/X0, the ELD input, with nothing on connector D18 (Bomarc sheet 3): R136
/// (1 kohm, 1 %) to +5 V and R138 (10 kohm) to ground, into the mux through
/// R137 (100 kohm, no load to speak of): 5 V x 10 / 11 = 4.55 V, E8h. The
/// ROM's ELD check at 0x3CEB..0x3D02 accepts 0Eh..DCh only. Caveat: R136's
/// value is printed in type on the scan, not in the drawing's hand
/// lettering, so it was added later by whoever annotated the scan; with
/// R136 above about 1.6 kohm the open level would fall inside the ROM's range.
pub const ELD_OPEN_COUNTS: u16 = 930;

/// Car parts that live on the engine model rather than the board. Call once
/// after Bench::new.
pub fn apply_engine(engine: &mut EngineState, setup: &Setup) {
    engine.speed_sensor_fitted = setup.speed_sensor;
}

fn set_bit(byte: &mut u8, mask: u8, on: bool) {
    if on {
        *byte |= mask;
    } else {
        *byte &= !mask;
    }
}

/// Driver-operated inputs. Call after every EngineState::sync_sensors_to_bus.
/// Idempotent: sets and clears only the bits it owns.
pub fn apply_switches(bus: &mut Bus, setup: &Setup, sw: &Switches) {
    // Raw 1 = cranking.
    set_bit(&mut bus.p28_input_port, P28_IN_STARTER, sw.starter);
    // Raw 1 = A/C requested; only when the car has A/C.
    set_bit(&mut bus.p28_input_port, P28_IN_AC_SWITCH, setup.ac && sw.ac);
    // Raw 0 = pedal pressed.
    set_bit(&mut bus.p28_input_port, P28_IN_BRAKE_RELEASED, !sw.brake);
    // Raw 1 = jumper fitted.
    set_bit(
        &mut bus.ppi.pins[PORT_A as usize],
        P28_PA_SERVICE_CONNECTOR,
        sw.service_connector,
    );
    // A/T position switch, raw 1 = that position (the ROM keeps the bits
    // uninverted in 211h): park/neutral out of gear, D4 in gear. That 211h.5
    // = 1 is park/neutral is read from 0x0D55..0x0D79, which applies an
    // automatic's in-gear idle compensation (with the brake pressed) only
    // with it clear. 0x32A8..0x32AE reads 211h.5, .6, .7 as P/N, D3, D4 in
    // that order. D3 is never selected; the script drives in D4. A manual
    // car has no such switch.
    let automatic = setup.gearbox == Gearbox::Automatic;
    let port = &mut bus.p28_input_port;
    set_bit(port, P28_IN_AT_GEAR_POSITION, automatic && !sw.in_gear);
    set_bit(port, P28_IN_AT_GEAR_DRIVE_4, automatic && sw.in_gear);
    set_bit(port, P28_IN_AT_GEAR_DRIVE_3, false);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stock() -> Setup {
        Setup::preset("stock-p28-manual").unwrap()
    }

    #[test]
    fn automatic_board_strap_reads_3_2_volts() {
        let mut bus = Bus::new();
        let mut s = stock();
        s.board_gearbox = Gearbox::Automatic;
        apply_static(&mut bus, &s);
        assert_eq!(
            bus.p28_mux_inputs[P28_MUX_U5_ADCR1][P28_U5_GEARBOX_SELECT],
            GEARBOX_STRAP_AUTOMATIC_COUNTS
        );
    }

    #[test]
    fn manual_board_strap_reads_low() {
        let mut bus = Bus::new();
        bus.p28_mux_inputs[P28_MUX_U5_ADCR1][P28_U5_GEARBOX_SELECT] = 650;
        apply_static(&mut bus, &stock());
        assert_eq!(bus.p28_mux_inputs[P28_MUX_U5_ADCR1][P28_U5_GEARBOX_SELECT], 0);
    }

    #[test]
    fn no_vtec_unplugs_the_solenoid_and_opens_the_pressure_switch() {
        let mut bus = Bus::new();
        let mut s = stock();
        s.vtec = false;
        apply_static(&mut bus, &s);
        assert!(!bus.vtec_solenoid_connected);
        assert!(!bus.vtec_pressure_switch_closed());
    }

    #[test]
    fn heated_o2_sensor_plugs_the_heater() {
        let mut bus = Bus::new();
        bus.o2_heater_connected = false;
        let mut s = stock();
        s.o2_sensor = O2Sensor::Heated;
        apply_static(&mut bus, &s);
        assert!(bus.o2_heater_connected);
    }

    #[test]
    fn one_wire_o2_sensor_has_no_heater() {
        let mut bus = Bus::new();
        let mut s = stock();
        s.o2_sensor = O2Sensor::OneWire;
        apply_static(&mut bus, &s);
        assert!(!bus.o2_heater_connected);
    }

    #[test]
    fn no_o2_sensor_has_no_heater() {
        let mut bus = Bus::new();
        let mut s = stock();
        s.o2_sensor = O2Sensor::None;
        apply_static(&mut bus, &s);
        assert!(!bus.o2_heater_connected);
    }

    #[test]
    fn missing_iacv_unplugs_it() {
        let mut bus = Bus::new();
        let mut s = stock();
        s.iacv = Iacv::None;
        apply_static(&mut bus, &s);
        assert!(!bus.iacv_connected);
    }

    #[test]
    fn stock_iacv_is_plugged() {
        let mut bus = Bus::new();
        bus.iacv_connected = false;
        apply_static(&mut bus, &stock());
        assert!(bus.iacv_connected);
    }

    #[test]
    fn starter_sets_its_input_bit() {
        let mut bus = Bus::new();
        let sw = Switches { starter: true, ..Switches::default() };
        apply_switches(&mut bus, &stock(), &sw);
        assert_ne!(bus.p28_input_port & P28_IN_STARTER, 0);
    }

    #[test]
    fn ac_switch_sets_its_bit_only_when_the_car_has_ac() {
        let sw = Switches { ac: true, ..Switches::default() };
        let mut bus = Bus::new();
        let mut s = stock();
        s.ac = true;
        apply_switches(&mut bus, &s, &sw);
        assert_ne!(bus.p28_input_port & P28_IN_AC_SWITCH, 0);

        let mut bus = Bus::new();
        s.ac = false;
        apply_switches(&mut bus, &s, &sw);
        assert_eq!(bus.p28_input_port & P28_IN_AC_SWITCH, 0);
    }

    #[test]
    fn service_connector_sets_its_port_a_bit() {
        let mut bus = Bus::new();
        let sw = Switches { service_connector: true, ..Switches::default() };
        apply_switches(&mut bus, &stock(), &sw);
        assert_ne!(bus.ppi.pins[PORT_A as usize] & P28_PA_SERVICE_CONNECTOR, 0);
    }

    #[test]
    fn brake_pressed_clears_the_released_bit() {
        let mut bus = Bus::new();
        let sw = Switches { brake: true, ..Switches::default() };
        apply_switches(&mut bus, &stock(), &sw);
        assert_eq!(bus.p28_input_port & P28_IN_BRAKE_RELEASED, 0);
    }

    #[test]
    fn a_released_switch_releases_its_bit() {
        let mut bus = Bus::new();
        let s = {
            let mut s = stock();
            s.ac = true;
            s
        };
        let on = Switches { starter: true, ac: true, service_connector: true, in_gear: false, brake: true };
        apply_switches(&mut bus, &s, &on);
        apply_switches(&mut bus, &s, &on);
        apply_switches(&mut bus, &s, &Switches::default());
        let fresh = Bus::new();
        assert_eq!(bus.p28_input_port, fresh.p28_input_port);
        assert_eq!(bus.ppi.pins[PORT_A as usize], fresh.ppi.pins[PORT_A as usize]);
    }

    const AT_BITS: u8 = P28_IN_AT_GEAR_POSITION | P28_IN_AT_GEAR_DRIVE_3 | P28_IN_AT_GEAR_DRIVE_4;

    #[test]
    fn an_automatic_car_is_in_park_or_neutral_out_of_gear_and_in_d4_in_gear() {
        let s = Setup::preset("stock-p28-automatic").unwrap();
        let mut bus = Bus::new();
        apply_switches(&mut bus, &s, &Switches::default());
        assert_eq!(bus.p28_input_port & AT_BITS, P28_IN_AT_GEAR_POSITION);
        apply_switches(&mut bus, &s, &Switches { in_gear: true, ..Switches::default() });
        assert_eq!(bus.p28_input_port & AT_BITS, P28_IN_AT_GEAR_DRIVE_4);
        apply_switches(&mut bus, &s, &Switches::default());
        assert_eq!(bus.p28_input_port & AT_BITS, P28_IN_AT_GEAR_POSITION);
    }

    #[test]
    fn a_manual_car_has_no_gear_position_inputs() {
        let mut bus = Bus::new();
        bus.p28_input_port |= AT_BITS;
        apply_switches(&mut bus, &stock(), &Switches { in_gear: true, ..Switches::default() });
        assert_eq!(bus.p28_input_port & AT_BITS, 0);
    }

    /// The keys `apply_static` reports for `s`.
    fn unmodelled_keys(s: &Setup) -> Vec<&'static str> {
        apply_static(&mut Bus::new(), s).into_iter().map(|(k, _)| k).collect()
    }

    /// `stock-p28-manual` with one key changed.
    fn with(key: &str, value: &str) -> Setup {
        let mut s = stock();
        s.set(key, value).unwrap();
        s
    }

    #[test]
    fn the_stock_p28_presets_are_fully_modelled() {
        assert_eq!(unmodelled_keys(&stock()), Vec::<&str>::new());
        assert_eq!(unmodelled_keys(&Setup::preset("stock-p28-automatic").unwrap()), Vec::<&str>::new());
    }

    #[test]
    fn every_unmodelled_value_names_its_key_and_a_reason() {
        for (key, value) in [
            ("ecu", "p30"),
            ("knock_sensor", "yes"),
            ("o2_sensor", "none"),
            ("iacv", "9.5"),
            ("egr", "yes"),
            ("power_steering_switch", "no"),
        ] {
            let got = apply_static(&mut Bus::new(), &with(key, value));
            assert_eq!(got.len(), 1, "{key} = {value}: {got:?}");
            assert_eq!(got[0].0, key);
            assert!(got[0].1.starts_with(value), "{key}: {}", got[0].1);
        }
    }

    #[test]
    fn a_p30_box_is_unmodelled_and_so_is_its_knock_sensor() {
        let p30 = Setup::preset("stock-p30").unwrap();
        assert_eq!(unmodelled_keys(&p30), ["ecu", "knock_sensor"]);
    }

    #[test]
    fn modelled_values_are_not_reported() {
        for (key, value) in [
            ("knock_sensor", "no"),
            ("vtec", "no"),
            ("o2_sensor", "one-wire"),
            ("iacv", "none"),
            ("egr", "no"),
            ("eld", "no"),
            ("speed_sensor", "no"),
            ("ac", "no"),
        ] {
            assert_eq!(unmodelled_keys(&with(key, value)), Vec::<&str>::new(), "{key} = {value}");
        }
    }

    #[test]
    fn a_manual_car_has_no_lockup_solenoids_on_either_board() {
        for board in ["manual", "automatic"] {
            let mut s = stock();
            s.set("board_gearbox", board).unwrap();
            let mut bus = Bus::new();
            bus.lockup_solenoids_connected = true;
            assert_eq!(apply_static(&mut bus, &s), Vec::new(), "{board}");
            assert!(!bus.lockup_solenoids_connected, "{board}");
        }
    }

    #[test]
    fn an_automatic_car_has_its_lockup_solenoids_on_either_board() {
        // On a manual board the ROM never looks at the lock-up feedbacks
        // (216h.3 clear: 0x3C92, 0x5F5C), so the mismatch reaches it only
        // through the A/T position inputs.
        for board in ["manual", "automatic"] {
            let mut s = stock();
            s.set("gearbox", "automatic").unwrap();
            s.set("board_gearbox", board).unwrap();
            let mut bus = Bus::new();
            assert_eq!(apply_static(&mut bus, &s), Vec::new(), "{board}");
            assert!(bus.lockup_solenoids_connected, "{board}");
        }
    }

    #[test]
    fn no_eld_presents_the_open_input_level() {
        let mut bus = Bus::new();
        apply_static(&mut bus, &with("eld", "no"));
        assert_eq!(bus.p28_mux_inputs[P28_MUX_U5_ADCR1][P28_U5_ELD_SELECT], ELD_OPEN_COUNTS);
        // 4.55 V: above the ROM's DCh limit (0x3CF4).
        assert!(ELD_OPEN_COUNTS >> 2 > 0xDC);
        let mut bus = Bus::new();
        apply_static(&mut bus, &stock());
        assert_eq!(bus.p28_mux_inputs[P28_MUX_U5_ADCR1][P28_U5_ELD_SELECT], Bus::new().p28_mux_inputs[P28_MUX_U5_ADCR1][P28_U5_ELD_SELECT]);
    }

    #[test]
    fn egr_wiring_follows_the_setup() {
        let mut bus = Bus::new();
        apply_static(&mut bus, &with("egr", "yes"));
        assert!(bus.egr_fitted);
        apply_static(&mut bus, &stock());
        assert!(!bus.egr_fitted);
    }

    #[test]
    fn no_speed_sensor_takes_it_off_the_engine() {
        let mut engine = EngineState::new();
        apply_engine(&mut engine, &with("speed_sensor", "no"));
        assert!(!engine.speed_sensor_fitted);
        apply_engine(&mut engine, &stock());
        assert!(engine.speed_sensor_fitted);
    }

    #[test]
    fn the_stock_manual_setup_leaves_a_new_bus_unchanged() {
        let mut bus = Bus::new();
        let s = stock();
        apply_static(&mut bus, &s);
        apply_switches(&mut bus, &s, &Switches::default());
        let fresh = Bus::new();
        assert_eq!(bus.p28_mux_inputs, fresh.p28_mux_inputs);
        assert_eq!(bus.p28_input_port, fresh.p28_input_port);
        assert_eq!(bus.ppi.pins[PORT_A as usize], fresh.ppi.pins[PORT_A as usize]);
        assert_eq!(bus.iacv_connected, fresh.iacv_connected);
        assert_eq!(bus.o2_heater_connected, fresh.o2_heater_connected);
        assert_eq!(bus.vtec_solenoid_connected, fresh.vtec_solenoid_connected);
        assert_eq!(bus.lockup_solenoids_connected, fresh.lockup_solenoids_connected);
        assert_eq!(bus.egr_fitted, fresh.egr_fitted);
        assert_eq!(bus.vtec_pressure_switch_closed(), fresh.vtec_pressure_switch_closed());
    }
}
