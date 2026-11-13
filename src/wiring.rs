//! Applies a `Setup` (what is wired to the box) to the board model (`Bus`).
//!
//! Only the hooks the board model already has are covered. Everything else in
//! `Setup` (knock sensor, EGR, ELD, speed sensor, power-steering switch, A/T
//! gear position, IACV resistance) is left alone here rather than guessed.

use crate::bus::{
    Bus, P28_IN_AC_SWITCH, P28_IN_BRAKE_RELEASED, P28_IN_STARTER, P28_MUX_U5_ADCR1,
    P28_PA_SERVICE_CONNECTOR, P28_U5_GEARBOX_SELECT,
};
use crate::car::{Gearbox, Iacv, O2Sensor, Setup};
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

/// Straps and plugs that do not change during a run. Call once after Bench::new.
pub fn apply_static(bus: &mut Bus, setup: &Setup) {
    bus.p28_mux_inputs[P28_MUX_U5_ADCR1][P28_U5_GEARBOX_SELECT] = match setup.board_gearbox {
        Gearbox::Manual => GEARBOX_STRAP_MANUAL_COUNTS,
        Gearbox::Automatic => GEARBOX_STRAP_AUTOMATIC_COUNTS,
    };
    if !setup.vtec {
        bus.vtec_solenoid_connected = false;
        bus.set_vtec_pressure_switch_closed(false);
    }
    bus.o2_heater_connected = matches!(setup.o2_sensor, O2Sensor::Heated);
    match setup.iacv {
        Iacv::Stock => bus.iacv_connected = true,
        Iacv::None => bus.iacv_connected = false,
        Iacv::Ohms(_) => {}
    }
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
    // sw.in_gear: A/T position inputs are Task 11.
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
        assert_eq!(bus.vtec_pressure_switch_closed(), fresh.vtec_pressure_switch_closed());
    }
}
