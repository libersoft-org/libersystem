// The kernel's own handlers for WIRED interrupts, on the two backends whose controllers deliver
// them by number rather than by vector.
//
// WHY THIS IS NOT THE MSI REGISTRY NEXT DOOR. `msi::MsiRegistry` binds a device interrupt to a
// USERSPACE driver: it reserves a slot for a process, signals an `Interrupt` object, and its whole
// lifecycle is about what happens when that process dies. Nothing here has a process. These are the
// lines the KERNEL itself answers - today exactly one, the PCI hot-plug line - and the handler is a
// function pointer in this binary that must run inside the interrupt, before the controller is told
// the source is done.
//
// AND IT IS NOT x86_64's TABLE EITHER, which is indexed by vector because that port's controller
// hands the CPU a vector it chose. A GIC hands over an INTID and an APLIC's MSI hands over an EID,
// and neither is an index into anything - so the number is stored beside the handler and looked up.
// Eight rows, because a machine has a handful of wired lines the kernel answers and a table sized
// for every INTID a GIC can raise would be a kilobyte of pointers to nothing.
//
// A LINE IS SHARED AND THE HANDLER MUST ASSUME IT. Two devices on one INTID are indistinguishable
// from inside the handler, so a handler registered here is one that looks at every source it could
// be, not at the one it hopes asserted.

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

/// What a wired handler is: it is told the number it was raised for, which is what lets one
/// function answer for several rows.
pub type Handler = fn(u32);

// The number no controller raises, which is what an empty row holds. A GIC's INTIDs start at 16 for
// anything a device can assert and an APLIC's EIDs start at 1, so zero is free on both.
const EMPTY: u32 = 0;

/// The wired lines this kernel answers, and what answers them.
pub struct Wired<const N: usize> {
	number: [AtomicU32; N],
	handler: [AtomicUsize; N],
}

impl<const N: usize> Default for Wired<N> {
	fn default() -> Self {
		Self::new()
	}
}

impl<const N: usize> Wired<N> {
	pub const fn new() -> Self {
		Self { number: [const { AtomicU32::new(EMPTY) }; N], handler: [const { AtomicUsize::new(0) }; N] }
	}

	/// Answer `number` with `handler` from now on. `false` when the table is full, which is a
	/// machine with more wired lines than this kernel answers and is said rather than ignored.
	///
	/// REGISTERING THE SAME NUMBER TWICE REPLACES, and does not take a second row. A port that
	/// walked its hot-plug ports twice would otherwise fill the table with one line.
	pub fn register(&self, number: u32, handler: Handler) -> bool {
		if number == EMPTY {
			return false;
		}
		for row in 0..N {
			if self.number[row].load(Ordering::Acquire) == number {
				self.handler[row].store(handler as usize, Ordering::Release);
				return true;
			}
		}
		for row in 0..N {
			// THE HANDLER GOES IN FIRST AND THE NUMBER LAST, because the number is what makes the
			// row live: an interrupt arriving between the two would otherwise find a row claiming
			// to answer for a line and a null pointer to answer it with.
			if self.number[row].load(Ordering::Acquire) != EMPTY {
				continue;
			}
			self.handler[row].store(handler as usize, Ordering::Release);
			if self.number[row].compare_exchange(EMPTY, number, Ordering::AcqRel, Ordering::Acquire).is_ok() {
				return true;
			}
		}
		false
	}

	/// Run the handler for `number`, and say whether there was one. `false` is what lets a caller
	/// go on and offer the same interrupt to the MSI path, which is where every other one belongs.
	pub fn dispatch(&self, number: u32) -> bool {
		if number == EMPTY {
			return false;
		}
		for row in 0..N {
			if self.number[row].load(Ordering::Acquire) != number {
				continue;
			}
			let raw = self.handler[row].load(Ordering::Acquire);
			if raw == 0 {
				return false;
			}
			// SAFETY: `raw` is non-zero, so it was written by `register` from a `Handler`, which is
			// the only writer of this array. The store is released and this load acquires it.
			let handler: Handler = unsafe { core::mem::transmute::<usize, Handler>(raw) };
			handler(number);
			return true;
		}
		false
	}

	/// Whether `number` has a handler, for a caller reporting what it armed.
	pub fn answers(&self, number: u32) -> bool {
		number != EMPTY && (0..N).any(|row| self.number[row].load(Ordering::Acquire) == number && self.handler[row].load(Ordering::Acquire) != 0)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use core::sync::atomic::AtomicU64;

	static SEEN: AtomicU64 = AtomicU64::new(0);

	fn record(number: u32) {
		SEEN.fetch_add(1 << (number % 63), Ordering::SeqCst);
	}

	fn other(number: u32) {
		SEEN.fetch_add(1 << ((number % 63) + 1), Ordering::SeqCst);
	}

	crate::tagged_test!(a_registered_number_reaches_its_handler_and_an_unregistered_one_does_not, [Kernel, Interrupt], id = "kernel.arch.common.wired.a_registered_number_reaches_its_handler", covers = ["kernel"]);
	fn a_registered_number_reaches_its_handler_and_an_unregistered_one_does_not() {
		let wired: Wired<4> = Wired::new();
		SEEN.store(0, Ordering::SeqCst);
		assert!(wired.register(35, record));
		assert!(wired.dispatch(35), "the row answers");
		assert_eq!(SEEN.load(Ordering::SeqCst), 1 << 35);
		assert!(!wired.dispatch(36), "a line nobody registered is left for the caller to place");
		assert_eq!(SEEN.load(Ordering::SeqCst), 1 << 35, "and its handler did not run");
	}

	// The number no controller raises is not a row: a table that accepted it would answer for an
	// empty row, which is every line that ever fails to match.
	crate::tagged_test!(zero_is_not_a_line, [Kernel, Interrupt], id = "kernel.arch.common.wired.zero_is_not_a_line", covers = ["kernel"]);
	fn zero_is_not_a_line() {
		let wired: Wired<4> = Wired::new();
		assert!(!wired.register(0, record));
		assert!(!wired.dispatch(0));
		assert!(!wired.answers(0));
	}

	// Four ports on one line is the ordinary case - a shared INTx - and it must not take four rows.
	crate::tagged_test!(the_same_number_registered_again_replaces_rather_than_fills_the_table, [Kernel, Interrupt], id = "kernel.arch.common.wired.the_same_number_replaces", covers = ["kernel"]);
	fn the_same_number_registered_again_replaces_rather_than_fills_the_table() {
		let wired: Wired<2> = Wired::new();
		SEEN.store(0, Ordering::SeqCst);
		for _ in 0..8 {
			assert!(wired.register(35, record));
		}
		assert!(wired.register(36, record), "the second row is still free");
		assert!(wired.register(35, other), "and the first is replaced, not duplicated");
		assert!(wired.dispatch(35));
		assert_eq!(SEEN.load(Ordering::SeqCst), 1 << 36, "the replacement ran and the original did not");
	}

	// A machine with more wired lines than this kernel answers is refused rather than silently
	// dropping the last one, which is a line that looks armed and is not.
	crate::tagged_test!(a_full_table_refuses_instead_of_forgetting, [Kernel, Interrupt], id = "kernel.arch.common.wired.a_full_table_refuses", covers = ["kernel"]);
	fn a_full_table_refuses_instead_of_forgetting() {
		let wired: Wired<2> = Wired::new();
		assert!(wired.register(35, record));
		assert!(wired.register(36, record));
		assert!(!wired.register(37, record));
		assert!(!wired.answers(37));
		assert!(wired.answers(35) && wired.answers(36));
	}
}
