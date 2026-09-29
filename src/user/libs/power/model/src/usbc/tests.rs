use super::*;
use crate::canon::validate;
use crate::schema::{Alarm, Provenance, ValueState};

fn partner(voltage: Reading<u64>, current: Reading<i64>) -> UsbC {
	UsbC { attached: Some(true), sinking: Some(true), voltage, current, fault: None }
}

#[test]
fn a_48_volt_contract_reading_50_9_volts_is_known_and_canonical() {
	let state = usb_c(&partner(Reading::Value(50_900_000), Reading::Value(-4_800_000)));
	assert_eq!((state.voltage.state, state.voltage.value), (ValueState::Known, 50_900_000));
	assert_eq!((state.current.state, state.current.value), (ValueState::Known, -4_800_000), "delivered out of the partner into this machine");
	assert_eq!((state.present, state.online), (Tristate::Yes, Tristate::Yes));
	assert_eq!(validate(&state), Ok(()));
}

#[test]
fn a_reading_past_60_volts_is_invalid_from_the_adapter_and_refused_when_a_record_claims_it_known() {
	let state = usb_c(&partner(Reading::Value(60_000_001), Reading::Unsupported));
	assert_eq!((state.voltage.state, state.voltage.reason), (ValueState::Invalid, InvalidReason::Range));
	assert_eq!(validate(&state), Ok(()), "an invalid reading is a canonical record");
	let mut forged = state.clone();
	forged.voltage = measure_u64(Tagged::Known(60_000_001));
	assert_eq!(validate(&forged), Err(InvalidReason::Range));
	forged.voltage = measure_u64(Tagged::Known(60_000_000));
	assert_eq!(validate(&forged), Ok(()), "the bound itself is a reading");
}

#[test]
fn a_field_the_kind_does_not_have_is_refused() {
	let good = usb_c(&partner(Reading::Value(5_000_000), Reading::Unsupported));
	let mut runtime = good.clone();
	runtime.runtime = measure_u64(Tagged::Known(3600));
	assert_eq!(validate(&runtime), Err(InvalidReason::Contradiction));
	let mut charge = good.clone();
	charge.charge = crate::schema::ChargeState::Charging;
	assert_eq!(validate(&charge), Err(InvalidReason::Contradiction));
	let mut alarm = good.clone();
	alarm.alarms.push(Alarm { kind: AlarmKind::Overload, state: Tristate::Yes, provenance: Provenance::Reported });
	assert_eq!(validate(&alarm), Err(InvalidReason::Contradiction));
	let mut control = good.clone();
	control.controls.schedule_off = true;
	assert_eq!(validate(&control), Err(InvalidReason::Contradiction));
	let mut temperature = good;
	temperature.temperature = crate::canon::temperature(crate::schema::TemperatureReference::Absolute, Tagged::Unknown);
	assert_eq!(validate(&temperature), Err(InvalidReason::Contradiction), "an unknown temperature is still a temperature the kind does not have");
}

#[test]
fn an_unplugged_connector_and_a_detach_publish_no_known_measurement_even_with_vbus_about_zero() {
	// The controller's VBUS register reads about 0 V on an empty connector: not a partner's, so unknown.
	let unplugged = UsbC { attached: Some(false), sinking: None, voltage: Reading::Value(12_000), current: Reading::Value(0), fault: Some(false) };
	let state = usb_c(&unplugged);
	assert_eq!((state.present, state.online), (Tristate::No, Tristate::No));
	assert_eq!((state.voltage.state, state.current.state), (ValueState::Unknown, ValueState::Unknown));
	assert_eq!(validate(&state), Ok(()), "an absent source with no known value is canonical");
	// A transport that measures nothing keeps its measurements unsupported, attached or not.
	let plain = UsbC { attached: Some(false), sinking: None, voltage: Reading::Unsupported, current: Reading::Unsupported, fault: None };
	let state = usb_c(&plain);
	assert_eq!((state.voltage.state, state.current.state), (ValueState::Unsupported, ValueState::Unsupported));
	assert!(state.alarms.is_empty(), "no fault is invented where the transport reports none");
}

#[test]
fn a_silent_transport_knows_neither_presence_nor_supply() {
	let silent = UsbC { attached: None, sinking: None, voltage: Reading::Unknown, current: Reading::Unsupported, fault: None };
	let state = usb_c(&silent);
	assert_eq!((state.present, state.online, state.voltage.state), (Tristate::Unknown, Tristate::Unknown, ValueState::Unknown));
	assert_eq!(validate(&state), Ok(()));
}

#[test]
fn a_vbus_fault_is_a_reported_source_fault() {
	let faulted = UsbC { fault: Some(true), ..partner(Reading::Value(15_000_000), Reading::Unsupported) };
	let state = usb_c(&faulted);
	assert_eq!(state.alarms, [Alarm { kind: AlarmKind::SourceFault, state: Tristate::Yes, provenance: Provenance::Reported }]);
	assert_eq!(validate(&state), Ok(()));
}
