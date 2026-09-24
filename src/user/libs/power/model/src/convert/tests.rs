// Every expected value below is stated from the physics or from the specification's own words, not
// computed by the function under test: 298.2 K is 25.05 degrees Celsius, 3600 joules is one
// watt-hour, 7200 ampere-seconds are two ampere-hours.

use super::*;

fn field(bits: u8, logical_min: i32, logical_max: i32, unit: u32, exponent: i8) -> Field {
	Field { bits, logical_min, logical_max, null_state: false, unit, exponent }
}

// The HID Power Device class's units, as its descriptors spell them.
const VOLT: u32 = 0x00f0_d121;
const AMPERE: u32 = 0x0010_0001;
const WATT: u32 = 0x0000_d121;
const JOULE: u32 = 0x0000_e121;
const AMPERE_SECOND: u32 = 0x0010_1001;
const KELVIN: u32 = 0x0001_0001;
const SECOND: u32 = 0x0000_1001;

#[test]
fn acpi_milli_units_become_micro_units_and_the_sentinel_never_becomes_a_number() {
	// 50 000 mWh is 50 Wh is 50 000 000 uWh.
	assert_eq!(acpi_milli(50_000), Tagged::Known(50_000_000));
	assert_eq!(acpi_milli(0), Tagged::Known(0), "an empty battery is a known zero");
	// 0xFFFFFFFF is ACPI's "unknown", honoured before the multiplication.
	assert_eq!(acpi_milli(ACPI_UNKNOWN), Tagged::Unknown);
	// The largest value the specification defines, and the first one past it.
	assert_eq!(acpi_milli(0x7fff_ffff), Tagged::Known(2_147_483_647_000));
	assert_eq!(acpi_milli(0x8000_0000), Tagged::Invalid(InvalidReason::Range));
	assert_eq!(acpi_milli(0xffff_fffe), Tagged::Invalid(InvalidReason::Range));
}

#[test]
fn the_power_unit_decides_energy_or_charge_and_nothing_else_is_a_unit() {
	assert_eq!(acpi_quantity(0), Some(Quantity::Energy));
	assert_eq!(acpi_quantity(1), Some(Quantity::Charge));
	assert_eq!(acpi_quantity(2), None);
	assert_eq!(acpi_quantity(ACPI_UNKNOWN), None);
}

#[test]
fn the_charge_state_bits_are_mutually_exclusive() {
	assert_eq!(acpi_charge_state(0b000), ChargeState::Idle);
	assert_eq!(acpi_charge_state(0b001), ChargeState::Discharging);
	assert_eq!(acpi_charge_state(0b010), ChargeState::Charging);
	assert_eq!(acpi_charge_state(0b011), ChargeState::Invalid, "charging and discharging at once is a contradiction");
	// The critical bit is not a charge state and does not disturb one.
	assert_eq!(acpi_charge_state(0b101), ChargeState::Discharging);
}

#[test]
fn a_rate_is_signed_by_its_direction_and_unknown_without_one() {
	// 1500 mW discharging is 1.5 W out of storage.
	assert_eq!(acpi_rate(1500, 0b01), Tagged::Known(-1_500_000));
	assert_eq!(acpi_rate(1500, 0b10), Tagged::Known(1_500_000));
	assert_eq!(acpi_rate(0, 0b00), Tagged::Known(0), "idle at zero is a known zero");
	assert_eq!(acpi_rate(700, 0b00), Tagged::Unknown, "a nonzero rate on an idle battery has no direction");
	assert_eq!(acpi_rate(700, 0b11), Tagged::Invalid(InvalidReason::Contradiction));
	assert_eq!(acpi_rate(ACPI_UNKNOWN, 0b01), Tagged::Unknown);
	assert_eq!(acpi_rate(0x9000_0000, 0b01), Tagged::Invalid(InvalidReason::Range));
}

#[test]
fn an_absolute_reading_is_tenths_of_a_kelvin_and_a_relative_one_is_an_offset() {
	// 298.2 K is 25.05 degrees Celsius.
	assert_eq!(acpi_temperature(2982, false), Tagged::Known(25_050));
	// 273.1 K is five hundredths of a degree below freezing.
	assert_eq!(acpi_temperature(2731, false), Tagged::Known(-50));
	assert_eq!(acpi_temperature(0, false), Tagged::Known(-273_150), "absolute zero is a reading, if an unlikely one");
	assert_eq!(acpi_temperature(ACPI_UNKNOWN, false), Tagged::Unknown);
	// Relative: five kelvin below the critical trip, as a signed offset, with no Celsius invented.
	assert_eq!(acpi_temperature((-50i32) as u32, true), Tagged::Known(-5_000));
	assert_eq!(acpi_temperature(30, true), Tagged::Known(3_000));
	// Every pattern is an offset on a relative zone: all ones is minus a tenth.
	assert_eq!(acpi_temperature(ACPI_UNKNOWN, true), Tagged::Known(-100));
}

#[test]
fn a_logical_value_takes_its_signedness_from_the_logical_minimum() {
	assert_eq!(hid_logical(0xff, &field(8, -128, 127, 0, 0)), Tagged::Known(-1));
	assert_eq!(hid_logical(0xff, &field(8, 0, 255, 0, 0)), Tagged::Known(255));
	// The same bits in a wider field are a different number: the length decides.
	assert_eq!(hid_logical(0x800, &field(12, -2048, 2047, 0, 0)), Tagged::Known(-2048));
	assert_eq!(hid_logical(0x800, &field(16, -32768, 32767, 0, 0)), Tagged::Known(2048));
	assert_eq!(hid_logical(0xffff_ffff, &field(32, i32::MIN, i32::MAX, 0, 0)), Tagged::Known(-1));
}

#[test]
fn a_value_outside_its_field_is_no_data_or_invalid_but_never_a_measurement() {
	// Bits past the field's width were not extracted from it.
	assert_eq!(hid_logical(0x100, &field(8, 0, 255, 0, 0)), Tagged::Invalid(InvalidReason::Range));
	// Out of the logical range: no data with a null state, invalid without one.
	let mut nullable = field(8, 0, 100, 0, 0);
	nullable.null_state = true;
	assert_eq!(hid_logical(0xff, &nullable), Tagged::Unknown);
	assert_eq!(hid_logical(0xff, &field(8, 0, 100, 0, 0)), Tagged::Invalid(InvalidReason::Range));
	// A field of no width, or wider than a report item can be, or a range upside down.
	assert_eq!(hid_logical(0, &field(0, 0, 1, 0, 0)), Tagged::Invalid(InvalidReason::Range));
	assert_eq!(hid_logical(0, &field(33, 0, 1, 0, 0)), Tagged::Invalid(InvalidReason::Range));
	assert_eq!(hid_logical(0, &field(8, 10, 1, 0, 0)), Tagged::Invalid(InvalidReason::Contradiction));
}

#[test]
fn hid_units_convert_exactly_at_the_canonical_unit() {
	// The volt is 10^7 of HID's g cm^2 s^-3 A^-1: 230 at exponent 7 is 230 V.
	assert_eq!(hid_convert(230, VOLT, 7, Canonical::Microvolt), Tagged::Known(230_000_000));
	// At exponent 5 the value is in hundredths of a volt: 1234 is 12.34 V.
	assert_eq!(hid_convert(1234, VOLT, 5, Canonical::Microvolt), Tagged::Known(12_340_000));
	// -150 hundredths of an ampere is -1.5 A.
	assert_eq!(hid_convert(-150, AMPERE, -2, Canonical::Microamp), Tagged::Known(-1_500_000));
	// 450 W.
	assert_eq!(hid_convert(450, WATT, 7, Canonical::Microwatt), Tagged::Known(450_000_000));
	// 3600 J is one watt-hour; 7200 ampere-seconds are two ampere-hours.
	assert_eq!(hid_convert(3600, JOULE, 7, Canonical::MicrowattHour), Tagged::Known(1_000_000));
	assert_eq!(hid_convert(7200, AMPERE_SECOND, 0, Canonical::MicroampHour), Tagged::Known(2_000_000));
	// 300 K is 26.85 degrees Celsius; 298.2 K, at exponent -1, is 25.05.
	assert_eq!(hid_convert(300, KELVIN, 0, Canonical::MillidegreeCelsius), Tagged::Known(26_850));
	assert_eq!(hid_convert(2982, KELVIN, -1, Canonical::MillidegreeCelsius), Tagged::Known(25_050));
	assert_eq!(hid_convert(1800, SECOND, 0, Canonical::Second), Tagged::Known(1800));
}

#[test]
fn truncation_is_toward_zero_and_only_at_the_final_unit() {
	// One ampere-second is 277.77... uAh: 277, and minus one is -277, not -278.
	assert_eq!(hid_convert(1, AMPERE_SECOND, 0, Canonical::MicroampHour), Tagged::Known(277));
	assert_eq!(hid_convert(-1, AMPERE_SECOND, 0, Canonical::MicroampHour), Tagged::Known(-277));
	// 273.1499 K is -0.0001 degrees Celsius: a tenth of a millidegree below zero, which truncates to 0.
	// Rounding in millikelvin first would give 273149 mK and then -1 - the error the rule is about.
	assert_eq!(hid_convert(2_731_499, KELVIN, -4, Canonical::MillidegreeCelsius), Tagged::Known(0));
	assert_eq!(hid_convert(2_731_501, KELVIN, -4, Canonical::MillidegreeCelsius), Tagged::Known(0));
	assert_eq!(hid_convert(2_731_400, KELVIN, -4, Canonical::MillidegreeCelsius), Tagged::Known(-10));
}

#[test]
fn a_unit_that_measures_something_else_or_nothing_does_not_convert() {
	assert_eq!(hid_convert(230, 0, 7, Canonical::Microvolt), Tagged::Invalid(InvalidReason::Unit), "unitless is not volts");
	// English linear (system nibble 3) is not SI linear, whatever its exponents say.
	assert_eq!(hid_convert(230, (VOLT & !0xf) | 3, 7, Canonical::Microvolt), Tagged::Invalid(InvalidReason::Unit));
	// CHARGE IS NOT ENERGY: an ampere-second field asked for watt-hours is refused, not multiplied by a
	// voltage somebody happened to have.
	assert_eq!(hid_convert(7200, AMPERE_SECOND, 0, Canonical::MicrowattHour), Tagged::Invalid(InvalidReason::Unit));
	assert_eq!(hid_convert(1, 0x0100_0001, 0, Canonical::Microamp), Tagged::Invalid(InvalidReason::Unit), "luminous intensity is no quantity of power");
	assert!(hid_unit_is(AMPERE_SECOND, Canonical::MicroampHour));
	assert!(!hid_unit_is(AMPERE_SECOND, Canonical::MicrowattHour));
}

#[test]
fn an_overflow_is_invalid_and_not_a_clamped_value() {
	// 2^31 - 1 amperes times ten to the seventh is past what 64 bits of microamps hold.
	assert_eq!(hid_convert(i32::MAX as i64, AMPERE, 7, Canonical::Microamp), Tagged::Invalid(InvalidReason::Overflow));
	assert_eq!(hid_convert(i32::MIN as i64, AMPERE, 7, Canonical::Microamp), Tagged::Invalid(InvalidReason::Overflow));
	// And the same value one power of ten smaller fits.
	assert_eq!(hid_convert(1, AMPERE, 7, Canonical::Microamp), Tagged::Known(10_000_000_000_000));
}

#[test]
fn percentages_are_basis_points() {
	assert_eq!(hid_basis_points(45, 0, 0), Tagged::Known(4500));
	// 455 tenths of a percent is 45.5 percent.
	assert_eq!(hid_basis_points(455, 0, -1), Tagged::Known(4550));
	assert_eq!(hid_basis_points(-1, 0, 0), Tagged::Invalid(InvalidReason::Range));
	assert_eq!(hid_basis_points(45, SECOND, 0), Tagged::Invalid(InvalidReason::Unit));
}

#[test]
fn a_capacity_is_read_in_its_unit_or_its_capacity_mode_and_the_two_must_agree() {
	// Unitless, CapacityMode 0: 2500 mAh is 2 500 000 uAh.
	assert_eq!(hid_capacity(2500, 0, 0, Some(0)), HidCapacity::Quantity(Quantity::Charge, Tagged::Known(2_500_000)));
	// CapacityMode 1: 2500 mWh.
	assert_eq!(hid_capacity(2500, 0, 0, Some(1)), HidCapacity::Quantity(Quantity::Energy, Tagged::Known(2_500_000)));
	// CapacityMode 2: a percentage.
	assert_eq!(hid_capacity(80, 0, 0, Some(2)), HidCapacity::Percent(Tagged::Known(8000)));
	assert_eq!(hid_capacity(1, 0, 0, Some(3)), HidCapacity::None(Tagged::Unsupported), "boolean-only support measures no capacity");
	assert_eq!(hid_capacity(2500, 0, 0, None), HidCapacity::None(Tagged::Invalid(InvalidReason::Unit)), "a unitless field with no mode declares nothing");
	// Its own Unit: 7200 ampere-seconds are two ampere-hours...
	assert_eq!(hid_capacity(7200, AMPERE_SECOND, 0, Some(0)), HidCapacity::Quantity(Quantity::Charge, Tagged::Known(2_000_000)));
	// ...and a mode that says energy contradicts it.
	assert_eq!(hid_capacity(7200, AMPERE_SECOND, 0, Some(1)), HidCapacity::Quantity(Quantity::Charge, Tagged::Invalid(InvalidReason::Contradiction)));
	assert_eq!(hid_capacity(7200, SECOND, 0, Some(0)), HidCapacity::None(Tagged::Invalid(InvalidReason::Unit)));
	assert_eq!(hid_capacity(-1, 0, 0, Some(0)), HidCapacity::None(Tagged::Invalid(InvalidReason::Range)));
}
