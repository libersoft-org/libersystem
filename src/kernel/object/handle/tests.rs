use super::{Handle, HandleError, HandleTable};
use crate::object::rights::Rights;
use crate::object::tests::TestObject;

crate::tagged_test!(capability_grants_no_operation_beyond_rights, [Handle, Object, Kernel]);
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

crate::tagged_test!(capability_attenuation_only_narrows, [Handle, Object, Kernel]);
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

crate::tagged_test!(no_ambient_authority_fresh_table_empty, [Handle, Object, Kernel]);
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
