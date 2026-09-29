use super::*;
use crate::schema::{Provenance, ValueState};

fn linear(m: i16, b: i16, b_exp: i8, r_exp: i8, format: Format) -> Linear {
	Linear { m, b, b_exp, r_exp, linearization: 0, format }
}

// THE RECORD'S BYTES, AT THEIR OFFSETS: ten-bit M and B split across two bytes each, and the two four-bit exponents
// packed into one, all signed.
#[test]
fn the_factors_are_read_from_the_record_signed() {
	// M = -2 (0x3FE: 0xFE, top bits 0b11), B = 5, R exp = -3 (0xD), B exp = 1.
	let factors = [0x00, 0xFE, 0b1100_0000, 0x05, 0x00, 0x00, 0xD1];
	let found = Linear::from_record(0b1000_0000, &factors);
	assert_eq!(found, Linear { m: -2, b: 5, b_exp: 1, r_exp: -3, linearization: 0, format: Format::TwosComplement });
	assert_eq!(signed(0x200, 10), -512);
	assert_eq!(signed(0x1FF, 10), 511);
	assert_eq!(Format::from_units(0xC0), Format::None);
}

#[test]
fn the_conversion_is_exact_in_milli_units_for_every_exponent_sign() {
	// y = x: 45 degrees is 45000 millidegrees.
	assert_eq!(linear(1, 0, 0, 0, Format::Unsigned).value(45, 3), Tagged::Known(45_000));
	// y = (x + 5 10^-1): a negative B exponent keeps the half.
	assert_eq!(linear(1, 5, -1, 0, Format::Unsigned).value(20, 3), Tagged::Known(20_500));
	// y = 5 x 10^-1.
	assert_eq!(linear(5, 0, 0, -1, Format::Unsigned).value(100, 3), Tagged::Known(50_000));
	// y = (2 x + 3 10^2) 10^-2, in millivolts: x = 100 gives 5 V.
	assert_eq!(linear(2, 3, 2, -2, Format::Unsigned).value(100, 3), Tagged::Known(5_000));
	// TRUNCATED TOWARD ZERO ONCE: 7 x 10^-4 at x = 1 is 0.7 milli-units.
	assert_eq!(linear(7, 0, 0, -4, Format::Unsigned).value(1, 3), Tagged::Known(0));
	assert_eq!(linear(-7, 0, 0, -4, Format::Unsigned).value(3, 3), Tagged::Known(-2));
}

#[test]
fn a_reading_is_signed_as_the_record_says() {
	assert_eq!(linear(1, 0, 0, 0, Format::Unsigned).raw(0xFE), Some(254));
	assert_eq!(linear(1, 0, 0, 0, Format::OnesComplement).raw(0xFE), Some(-1));
	assert_eq!(linear(1, 0, 0, 0, Format::OnesComplement).raw(0x05), Some(5));
	assert_eq!(linear(1, 0, 0, 0, Format::TwosComplement).raw(0xFE), Some(-2));
	assert_eq!(linear(1, 0, 0, 0, Format::TwosComplement).value(0xF6, 3), Tagged::Known(-10_000));
}

#[test]
fn a_non_linear_record_and_a_sensor_without_a_reading_are_unsupported_and_an_overflow_is_invalid() {
	let mut formula = linear(1, 0, 0, 0, Format::Unsigned);
	formula.linearization = 0x01;
	assert_eq!(formula.value(10, 3), Tagged::Unsupported, "ln(y) is the BMC's formula, not a linear one");
	formula.linearization = 0x70;
	assert_eq!(formula.value(10, 3), Tagged::Unsupported);
	assert_eq!(linear(1, 0, 0, 0, Format::None).value(10, 3), Tagged::Unsupported);
	assert_eq!(linear(511, 511, 7, 7, Format::Unsigned).value(255, 25), Tagged::Invalid(InvalidReason::Overflow));
}

#[test]
fn every_temperature_unit_reaches_millidegrees_celsius() {
	let identity = linear(1, 0, 0, 0, Format::Unsigned);
	assert_eq!(millidegrees(&identity, TemperatureUnit::Celsius, 40), Tagged::Known(40_000));
	assert_eq!(millidegrees(&identity, TemperatureUnit::Fahrenheit, 212), Tagged::Known(100_000));
	assert_eq!(millidegrees(&identity, TemperatureUnit::Kelvin, 255), Tagged::Known(-18_150));
	assert_eq!(temperature_unit(4), None, "volts are not a temperature");
}

#[test]
fn a_reading_the_bmc_disowns_is_unknown() {
	let convert = |raw: u8| Tagged::Known(i64::from(raw));
	assert_eq!(reading(&[40, 0xC0, 0xC0]).map(|found| found.value(convert)), Some(Tagged::Known(40)));
	assert_eq!(reading(&[40, 0xE0]).map(|found| found.value(convert)), Some(Tagged::Unknown), "reading unavailable");
	assert_eq!(reading(&[40, 0x80]).map(|found| found.value(convert)), Some(Tagged::Unknown), "scanning disabled is bit 6 CLEAR");
	assert_eq!(reading(&[40]), None);
}

fn sensor(reading: Option<Reading>) -> TemperatureSensor {
	TemperatureSensor { linear: linear(1, 0, 0, 0, Format::Unsigned), base_unit: 1, upper_non_critical: Some(70), upper_critical: Some(85), upper_non_recoverable: Some(95), reading }
}

// THE ZONE: the upper non-critical, critical and non-recoverable thresholds as passive, hot and critical trips; the
// BMC's own comparison reported; the converted values compared when the BMC sent none.
#[test]
fn a_temperature_sensor_is_a_thermal_zone_with_its_thresholds_as_trips() {
	let zone = thermal(&sensor(Some(Reading { raw: 40, unavailable: false, scanning_disabled: false, comparison: Some(0xC0) })));
	assert_eq!((zone.kind, zone.temperature.state, zone.temperature.value), (SourceKind::ThermalZone, ValueState::Known, 40_000));
	let trips: Vec<_> = zone.trips.iter().map(|trip| (trip.kind, trip.temperature.value)).collect();
	assert_eq!(trips, [(TripKind::Passive, 70_000), (TripKind::Hot, 85_000), (TripKind::Critical, 95_000)]);
	assert_eq!(zone.alarms.iter().map(|alarm| (alarm.kind, alarm.state, alarm.provenance)).collect::<Vec<_>>(), [(AlarmKind::OverTemperature, Tristate::No, Provenance::Reported)]);
	let hot = thermal(&sensor(Some(Reading { raw: 90, unavailable: false, scanning_disabled: false, comparison: Some(0xD8) })));
	assert_eq!(hot.alarms[0].state, Tristate::Yes, "the BMC said at or above upper critical");
	let silent = thermal(&sensor(Some(Reading { raw: 90, unavailable: false, scanning_disabled: false, comparison: None })));
	assert_eq!((silent.alarms[0].state, silent.alarms[0].provenance), (Tristate::Yes, Provenance::Derived));
	let unread = thermal(&sensor(None));
	assert_eq!(unread.temperature.state, ValueState::Unknown);
	let volts = thermal(&TemperatureSensor { base_unit: 4, ..sensor(None) });
	assert_eq!(volts.temperature.state, ValueState::Invalid);
}
