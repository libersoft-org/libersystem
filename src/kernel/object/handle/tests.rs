use super::{Handle, HandleError, HandleTable};
use crate::object::rights::Rights;
use crate::object::tests::TestObject;

crate::tagged_test!(capability_grants_no_operation_beyond_rights, [Handle, Object, Kernel], id = "kernel.object.handle.capability_grants_no_operation_beyond_rights", covers = ["kernel"]);
fn capability_grants_no_operation_beyond_rights() {
	// Property: a handle grants no operation beyond the rights it carries. Across many random
	// granted-rights sets and random probe rights, a rights-checked lookup succeeds exactly
	// when the probe is a subset of the granted set - never a superset. (Fixed-seed xorshift,
	// so the run is deterministic.)
	let mut seed: u64 = 0x5eed_1238_d38a_77c1;
	let mut next = || -> u64 {
		seed ^= seed << 13;
		seed ^= seed >> 7;
		seed ^= seed << 17;
		seed
	};
	let mut table = HandleTable::new();
	for _ in 0..512 {
		let granted = Rights::from_bits(next() as u32);
		let probe = Rights::from_bits(next() as u32);
		let handle = table.insert_object(TestObject::new(1), granted, 0);
		assert_eq!(table.lookup(handle, probe).is_ok(), granted.contains(probe), "a lookup must succeed iff the probe rights are a subset of the granted rights");
		table.close(handle).expect("close");
	}
}

crate::tagged_test!(capability_attenuation_only_narrows, [Handle, Object, Kernel], id = "kernel.object.handle.capability_attenuation_only_narrows", covers = ["kernel"]);
fn capability_attenuation_only_narrows() {
	// Property: duplicating a capability can only narrow it, never widen it. Across many
	// random grants (carrying the DUPLICATE right) and random requests, duplication succeeds
	// exactly when the request is a subset of the grant, and the derived handle carries
	// exactly the requested rights - no right the original lacked, and none outside the
	// request. There is no path by which a derived capability gains authority.
	let mut seed: u64 = 0xabcd_0042_1357_9bdf;
	let mut next = || -> u64 {
		seed ^= seed << 13;
		seed ^= seed >> 7;
		seed ^= seed << 17;
		seed
	};
	let mut table = HandleTable::new();
	for _ in 0..512 {
		let granted = Rights::from_bits(next() as u32) | Rights::DUPLICATE;
		let requested = Rights::from_bits(next() as u32);
		let handle = table.insert_object(TestObject::new(2), granted, 0);
		match table.duplicate(handle, requested) {
			Ok(duplicate) => {
				// Duplication is allowed only when the request is within the grant...
				assert!(granted.contains(requested), "duplication widened the rights beyond the original");
				// ...and the derived handle carries exactly the requested rights, never more.
				let probe = Rights::from_bits(next() as u32);
				assert_eq!(table.lookup(duplicate, probe).is_ok(), requested.contains(probe), "the derived capability carries exactly the requested rights");
				table.close(duplicate).expect("close duplicate");
			}
			Err(_) => {
				// The grant carries DUPLICATE, so the only reason to refuse is that the request
				// asked for a right outside the grant - widening, which is forbidden.
				assert!(!granted.contains(requested), "duplication refused a request that was within the grant");
			}
		}
		table.close(handle).expect("close");
	}
}

crate::tagged_test!(no_ambient_authority_fresh_table_empty, [Handle, Object, Kernel], id = "kernel.object.handle.no_ambient_authority_fresh_table_empty", covers = ["kernel"]);
fn no_ambient_authority_fresh_table_empty() {
	// A newly created handle table holds nothing: a process begins with no ambient authority
	// and can reach only capabilities explicitly handed to it. The table is empty, and every
	// lookup into it - across a wide range of handle values - is rejected as a bad handle.
	let table = HandleTable::new();
	assert_eq!(table.len(), 0, "a fresh handle table must be empty");
	let mut seed: u64 = 0x0f0f_1234_dead_c0de;
	let mut next = || -> u64 {
		seed ^= seed << 13;
		seed ^= seed >> 7;
		seed ^= seed << 17;
		seed
	};
	for _ in 0..256 {
		let handle = Handle::from_raw(next());
		assert!(matches!(table.lookup(handle, Rights::NONE), Err(HandleError::BadHandle)), "an empty table must resolve no handle");
	}
}

crate::tagged_test!(a_close_all_racing_a_transfer_never_frees_the_same_slot_twice, [Handle, Object, Kernel], id = "kernel.object.handle.a_close_all_racing_a_transfer_never_frees_the_same_slot_twice", covers = ["kernel"]);
fn a_close_all_racing_a_transfer_never_frees_the_same_slot_twice() {
	// A process torn down while one of its handles is mid-transfer.
	//
	// `close_all` used to push EVERY index onto the free list, including a slot whose capability
	// was already taken for transfer and whose `commit_taken` / `restore_taken` had yet to run. The
	// slot was then on the free list AND about to be written by the transfer's completion, so the
	// next `insert` handed out an index that something else already owned - two handles, one slot,
	// each believing it holds a different object.
	//
	// The property is exactly "no index appears twice", which is checkable without racing anything:
	// take a capability for transfer, close the table underneath it, complete the transfer, and
	// look at the free list.
	let mut table = HandleTable::new();
	let keep = table.insert_object(TestObject::new(1), Rights::ALL, 0);
	let moving = table.insert_object(TestObject::new(2), Rights::ALL, 0);
	assert!(table.lookup(keep, Rights::NONE).is_ok() && table.lookup(moving, Rights::NONE).is_ok(), "both handles are installed");

	let taken = table.take_for_transfer(moving, Rights::ALL).expect("the capability is taken for transfer");
	// The owner dies here, with the transfer still in flight.
	table.close_all();
	// And the transfer completes afterwards, as its contract requires exactly one of these to.
	table.commit_taken(moving);
	drop(taken);

	let mut seen = alloc::vec::Vec::new();
	for index in table.free_indices_for_test() {
		assert!(!seen.contains(&index), "slot {index} is on the free list twice: a later insert would hand out one slot as two handles");
		seen.push(index);
	}
	// And the slot really is reusable exactly once.
	let reused = table.insert_object(TestObject::new(3), Rights::ALL, 0);
	let again = table.insert_object(TestObject::new(4), Rights::ALL, 0);
	assert!(reused != again, "two inserts must not answer with the same handle");
}

crate::tagged_test!(closing_a_handle_never_needs_an_allocation, [Handle, Object, Kernel], id = "kernel.object.handle.closing_a_handle_never_needs_an_allocation", covers = ["kernel"]);
fn closing_a_handle_never_needs_an_allocation() {
	// `try_place` reserved room in `slots` and NOT in `free`, so a fresh slot was appended with no
	// room for the index it would carry when it was closed. Every `free.push` on the closing paths
	// then allocated - and none of them has anywhere to report a failure. A ring-3 process that
	// exhausted the heap and closed a handle could end the kernel; teardown could do it at the point
	// in a process's life where there is least left to recover with.
	//
	// The property is a capacity invariant, which is checkable without an allocator failure:
	// `free.capacity() >= slots.len()` after every insertion means no closing push can ever grow it.
	let mut table = HandleTable::new();
	for n in 1..=64u64 {
		let _ = table.insert_object(TestObject::new(n), Rights::ALL, 0);
		assert!(table.free_capacity_for_test() >= table.slot_count_for_test(), "after {n} inserts the free list has room for {} of {} slots: closing one would allocate", table.free_capacity_for_test(), table.slot_count_for_test());
	}

	// And it still holds once slots are recycled rather than appended, which is the case the old
	// comment described and the only one it was true for.
	let mut handles = alloc::vec::Vec::new();
	for n in 100..=120u64 {
		handles.push(table.insert_object(TestObject::new(n), Rights::ALL, 0));
	}
	for handle in handles {
		table.close(handle).expect("an installed handle closes");
		assert!(table.free_capacity_for_test() >= table.slot_count_for_test(), "the invariant survives recycling");
	}

	// Bulk teardown rebuilds the whole free list, which is the largest push run there is.
	table.close_all();
	assert!(table.free_capacity_for_test() >= table.slot_count_for_test(), "close_all rebuilt the free list within the reserved capacity");
}
