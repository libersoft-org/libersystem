use super::*;
use alloc::string::String;
use alloc::vec;

fn package(values: &[u64]) -> Value {
	Value::Package(values.iter().map(|value| Value::Integer(*value)).collect())
}

#[test]
fn a_node_s_class_is_its_id() {
	assert_eq!(class_of([&b"PNP0C0A"[..]]), Some(Class::Battery));
	assert_eq!(class_of([&b"LSFX0B00"[..], &b"ACPI0003"[..]]), Some(Class::Ac), "a vendor _HID with the class as a _CID");
	assert_eq!(class_of([&b"THERMALZONE"[..]]), Some(Class::ThermalZone));
	assert_eq!(class_of([&b"PNP0C0D"[..]]), None, "a lid is none of them");
}

// `_BIX` AND `_BIF` AT THEIR OFFSETS, and each refused by name when it is not what it says it is.
#[test]
fn a_battery_s_information_is_read_at_the_specification_s_offsets() {
	// _BIX revision 1: revision, unit, design, last full, technology, voltage, warning, low, cycles, accuracy, four
	// times, two granularities, then four strings and the swapping capability.
	let mut bix_value = vec![
		Value::Integer(1),
		Value::Integer(0),
		Value::Integer(50_000),
		Value::Integer(48_000),
		Value::Integer(1),
		Value::Integer(11_100),
		Value::Integer(5_000),
		Value::Integer(2_000),
	];
	bix_value.extend((0..8).map(|_| Value::Integer(0)));
	bix_value.extend(["model", "serial", "LION", "LiberSoft"].iter().map(|text| Value::String(String::from(*text))));
	bix_value.push(Value::Integer(0));
	let information = bix(&Value::Package(bix_value)).expect("a revision 1 _BIX");
	assert_eq!(information, Information { power_unit: 0, design_capacity: 50_000, last_full_capacity: 48_000, design_warning: 5_000, design_low: 2_000 });
	let mut bif_value = vec![
		Value::Integer(1),
		Value::Integer(4_400),
		Value::Integer(4_200),
		Value::Integer(1),
		Value::Integer(11_100),
		Value::Integer(440),
		Value::Integer(200),
		Value::Integer(10),
		Value::Integer(10),
	];
	bif_value.extend(["model", "serial", "LION", "LiberSoft"].iter().map(|text| Value::String(String::from(*text))));
	assert_eq!(bif(&Value::Package(bif_value)).expect("a _BIF"), Information { power_unit: 1, design_capacity: 4_400, last_full_capacity: 4_200, design_warning: 440, design_low: 200 });
	assert_eq!(bif(&package(&[1, 2, 3])), Err(Refusal::Shape("_BIF has fewer than thirteen elements")));
	assert_eq!(bix(&Value::Integer(7)), Err(Refusal::Shape("_BIX is not a package")));
	let mut wrong = vec![Value::Integer(1), Value::String(String::from("mW"))];
	wrong.extend((0..19).map(|_| Value::Integer(0)));
	assert_eq!(bix(&Value::Package(wrong)), Err(Refusal::NotInteger("_BIX's power unit")));
}

#[test]
fn a_battery_s_status_and_the_adapter_and_zone_integers() {
	assert_eq!(bst(&package(&[0b10, 5_000, 24_000, 11_100])), Ok(Status { state: 0b10, rate: 5_000, remaining: 24_000, voltage: 11_100 }));
	assert_eq!(bst(&package(&[0, 0, 0])), Err(Refusal::Shape("_BST has fewer than four elements")));
	// A 64-bit integer past 32 bits is the specification's "unknown", never its low half.
	assert_eq!(bst(&package(&[0, 0x1_0000_0005, 1, 1])).map(|status| status.rate), Ok(u32::MAX));
	assert_eq!(integer(&Value::Integer(3_002)), Ok(3_002));
	assert_eq!(integer(&Value::Buffer(vec![1])), Err(Refusal::NotInteger("the method's result")));
	// THE MODEL'S ADAPTER DECIDES WHAT THEY MEAN: a present battery discharging, and a zone at 27.1 C.
	let normalised = power_model::acpi::battery(&battery(Some(0x1F), &Information { power_unit: 0, design_capacity: 50_000, last_full_capacity: 48_000, design_warning: 5_000, design_low: 2_000 }, &Status { state: 0b01, rate: 5_000, remaining: 24_000, voltage: 11_100 }));
	assert_eq!(normalised.present, proto::system::Tristate::Yes);
	let zone = Zone { temperature: 3_002, relative: false, critical: Some(3_732), hot: None, passive: Some(3_632), active: vec![3_532] };
	let thermal = power_model::acpi::thermal(&zone.thermal());
	assert_eq!(thermal.trips.len(), 3, "critical, passive and one active trip");
}

// A STORM IS BOUNDED: the first notification refreshes, every one inside the bar is owed and folded into ONE refresh
// when the bar lifts.
#[test]
fn a_storm_of_notifications_refreshes_once_per_interval() {
	let mut coalescer = Coalescer::default();
	assert!(coalescer.notified(100), "the first refreshes at once");
	assert_eq!(coalescer.due_at(), None);
	for tick in 101..100 + REFRESH_TICKS {
		assert!(!coalescer.notified(tick), "inside the bar, owed");
	}
	assert_eq!(coalescer.due_at(), Some(100 + REFRESH_TICKS));
	assert!(!coalescer.take_due(100 + REFRESH_TICKS - 1), "not before its time");
	assert!(coalescer.take_due(100 + REFRESH_TICKS), "one refresh for all of them");
	assert!(!coalescer.take_due(100 + REFRESH_TICKS + 1), "and only one");
	assert_eq!(coalescer.coalesced, REFRESH_TICKS - 2, "every one past the first owed was folded");
	assert!(!coalescer.notified(100 + REFRESH_TICKS + 1), "the refresh bars the next interval too");
	assert!(coalescer.take_due(100 + 2 * REFRESH_TICKS));
}
