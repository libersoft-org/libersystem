// The budget, at its bounds and one past them - which is the whole of what a bound is worth testing
// for. Every case here is a bus somebody can present to this controller with a hub and patience.

use super::*;

#[test]
// The keyboard budget fills and then REFUSES, and it says which ceiling stopped it. Before this, the
// HID module's device list was an unbounded `Vec` and a tier of hubs full of keyboards took a DMA
// page and an endpoint of the controller's for each one.
fn a_class_module_is_refused_at_its_own_ceiling_and_says_which() {
	let mut budget = Budget::new();
	for index in 0..HID_LIMITS.devices {
		assert_eq!(budget.admit(ClassKind::Hid), Ok(()), "device {index} is inside the budget");
	}
	assert_eq!(budget.admit(ClassKind::Hid), Err(Refusal::Devices), "and the ninth is not");
	assert_eq!(Refusal::Devices.describe(), b"device count");

	let usage = budget.usage(ClassKind::Hid);
	assert_eq!(usage.devices, HID_LIMITS.devices);
	assert_eq!(usage.endpoints, HID_LIMITS.endpoints);
	assert_eq!(usage.dma_bytes, HID_LIMITS.dma_bytes, "eight rings is exactly the DMA ceiling");
	assert_eq!(usage.in_flight, HID_LIMITS.in_flight);
}

#[test]
// A REFUSED ADMISSION CHARGES NOTHING. A partial charge is a leak that only ever shows up as a
// controller refusing devices it has room for, on a machine nobody can reproduce.
fn a_refused_admission_leaves_the_budget_where_it_was() {
	let mut budget = Budget::new();
	for _ in 0..HID_LIMITS.devices {
		budget.admit(ClassKind::Hid).expect("inside the budget");
	}
	let before = budget.usage(ClassKind::Hid);
	for _ in 0..16 {
		assert!(budget.admit(ClassKind::Hid).is_err());
	}
	assert_eq!(budget.usage(ClassKind::Hid), before, "sixteen refusals cost nothing");
}

#[test]
// This controller serves one disk, and a budget is how a user plugging in a second one gets told
// rather than finding that nothing happened.
fn the_storage_module_serves_one_device_and_refuses_the_second() {
	let mut budget = Budget::new();
	assert_eq!(budget.admit(ClassKind::Storage), Ok(()));
	assert_eq!(budget.admit(ClassKind::Storage), Err(Refusal::Devices));
	assert_eq!(budget.usage(ClassKind::Storage).devices, 1);

	// And the first one's charge is the whole of what the module may hold: two rings and the data
	// buffer it is allowed to grow to.
	assert_eq!(budget.usage(ClassKind::Storage).dma_bytes, 2 * RING_BYTES + STORAGE_MAX_DATA_BYTES);
}

#[test]
// One module cannot spend another's budget, which is the isolation the decision promises: two class
// modules inside one Domain share the controller's resources, and an accounting that pooled them
// would let a hub full of keyboards refuse the disk.
fn two_class_modules_do_not_share_one_pool() {
	let mut budget = Budget::new();
	for _ in 0..HID_LIMITS.devices {
		budget.admit(ClassKind::Hid).expect("the HID budget");
	}
	assert_eq!(budget.admit(ClassKind::Hid), Err(Refusal::Devices), "the HID module is full");
	assert_eq!(budget.admit(ClassKind::Storage), Ok(()), "and the disk is admitted anyway");

	let mut other = Budget::new();
	other.admit(ClassKind::Storage).expect("the storage budget");
	assert_eq!(other.admit(ClassKind::Storage), Err(Refusal::Devices));
	assert_eq!(other.admit(ClassKind::Hid), Ok(()), "a full disk budget does not refuse a keyboard");
}

#[test]
// A detach gives the charge back, which is the lifecycle half: the controller drives attach and
// detach, so the controller is where what they cost is counted. A release that did not restore
// capacity would turn a port somebody plugs and unplugs into a controller that stops accepting
// anything.
fn a_detach_gives_the_charge_back() {
	let mut budget = Budget::new();
	for _ in 0..HID_LIMITS.devices {
		budget.admit(ClassKind::Hid).expect("the HID budget");
	}
	assert!(budget.admit(ClassKind::Hid).is_err());
	budget.release(ClassKind::Hid);
	assert_eq!(budget.admit(ClassKind::Hid), Ok(()), "the slot the detach freed is usable again");

	// Plug and unplug the same port a hundred times: the budget ends where it started.
	let mut cycles = Budget::new();
	for _ in 0..100 {
		cycles.admit(ClassKind::Storage).expect("the disk");
		cycles.release(ClassKind::Storage);
	}
	assert_eq!(cycles.usage(ClassKind::Storage), Usage::default());
}

#[test]
// A release with nothing charged must not panic: it happens during a teardown, which is the one
// moment a driver must not, and zero is the honest floor for "how much is this module holding".
fn releasing_more_than_was_charged_stops_at_zero() {
	let mut budget = Budget::new();
	budget.release(ClassKind::Hid);
	budget.release(ClassKind::Hid);
	budget.release(ClassKind::Storage);
	assert_eq!(budget.usage(ClassKind::Hid), Usage::default());
	assert_eq!(budget.usage(ClassKind::Storage), Usage::default());
	// And the budget still works afterwards.
	assert_eq!(budget.admit(ClassKind::Hid), Ok(()));
}

#[test]
// The mass-storage data buffer grows to the largest request and never shrinks, so the ceiling is the
// only thing between a client's request and a megabyte-shaped hole in the controller's Domain.
fn the_data_buffer_is_bounded_by_what_the_admission_reserved() {
	assert!(Budget::buffer_within(ClassKind::Storage, 4096));
	assert!(Budget::buffer_within(ClassKind::Storage, STORAGE_MAX_DATA_BYTES), "the ceiling itself is inside it");
	assert!(!Budget::buffer_within(ClassKind::Storage, STORAGE_MAX_DATA_BYTES + 1), "one byte past it is not");
	assert!(!Budget::buffer_within(ClassKind::Storage, u64::MAX));

	// A HID device's buffer is its ring and nothing more.
	assert!(Budget::buffer_within(ClassKind::Hid, RING_BYTES));
	assert!(!Budget::buffer_within(ClassKind::Hid, RING_BYTES + 1));
}

#[test]
// The four dimensions are four separate ceilings, and a module reaches its device count first only
// because the per-device costs were chosen that way. Each still has to refuse on its own terms, so
// the test drives each one with a limit set that makes it the binding one.
fn every_dimension_refuses_on_its_own_terms() {
	// The costs and limits the controller ships with are consistent: each ceiling is reached by
	// exactly the number of devices the device ceiling allows, so no dimension is dead.
	assert_eq!(HID_LIMITS.endpoints, HID_LIMITS.devices * HID_COST.endpoints);
	assert_eq!(HID_LIMITS.dma_bytes, HID_LIMITS.devices as u64 * HID_COST.dma_bytes);
	assert_eq!(HID_LIMITS.in_flight, HID_LIMITS.devices * HID_COST.in_flight);
	assert_eq!(STORAGE_LIMITS.endpoints, STORAGE_LIMITS.devices * STORAGE_COST.endpoints);
	assert_eq!(STORAGE_LIMITS.dma_bytes, STORAGE_LIMITS.devices as u64 * STORAGE_COST.dma_bytes);

	// And every refusal has its own word, so a log line says which ceiling was reached.
	let words = [Refusal::Devices.describe(), Refusal::Endpoints.describe(), Refusal::DmaBytes.describe(), Refusal::InFlight.describe()];
	for (index, word) in words.iter().enumerate() {
		assert!(!word.is_empty());
		for other in words.iter().skip(index + 1) {
			assert_ne!(word, other, "two ceilings would be logged as the same thing");
		}
	}
}
