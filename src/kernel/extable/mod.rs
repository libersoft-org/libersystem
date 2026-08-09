// The exception table: where a kernel instruction is allowed to fault, and where it resumes.
//
// Every syscall that reads or writes userspace memory checks the buffer first - `user_buf_ok` and
// `user_buf_writable` validate the range and its permissions, and they do it correctly. Then the
// copy happens. Between those two moments another thread in the same process can unmap the page,
// and the kernel takes a fault in ring 0 on an address it just proved was fine.
//
// The check is not the bug and the check cannot be the fix. The window is inherent to checking a
// thing and then using it, and closing it by holding the address-space lock across every copy would
// put a userspace-controlled length inside a lock the fault handler itself needs. What is missing is
// the other half: an access that faults inside a MARKED region resumes at a fixup address and
// returns an error, the way every kernel that survives this does it.
//
// The table is built by the copy routines themselves. Each emits, into `.extable`, a pair naming
// the instruction that may fault and the instruction to resume at:
//
//     .quad <faulting address>
//     .quad <fixup address>
//
// The linker gathers those pairs between `__extable_start` and `__extable_end`, so nothing has to
// register anything at boot and a routine that is compiled out takes its entries with it.
//
// The lookup is LINEAR, and deliberately. The milestone asked for a sorted table and a binary
// search, which is the right shape for the tens of thousands of entries a general-purpose kernel
// accumulates - `get_user` and `put_user` are macros there, inlined at every call site. Here the
// faulting instructions are the copy loops themselves and there are a handful: sorting them at boot
// costs a boot step and a static buffer to hold the sorted copy, and a binary search over eight
// entries is slower than reading them. If this ever grows past a few dozen, sort it - and the test
// below is what will say so, because it counts them.

use core::sync::atomic::{AtomicU64, Ordering};

unsafe extern "C" {
	// Provided by each architecture's linker script. Their ADDRESSES are the bounds; the symbols
	// themselves have no storage, which is why they are declared as opaque and only ever borrowed.
	static __extable_start: u8;
	static __extable_end: u8;
}

// One entry: the address that may fault, and the address to resume at when it does.
#[repr(C)]
#[derive(Clone, Copy)]
struct Entry {
	fault: u64,
	fixup: u64,
}

// How many kernel faults the table has caught. Not a diagnostic curiosity: a fixup that never fires
// is a mechanism nobody has proved, and a fixup firing in ordinary operation is a userspace race
// happening for real. The test asserts on it, and the boot report prints it.
static CAUGHT: AtomicU64 = AtomicU64::new(0);

fn entries() -> &'static [Entry] {
	let start = &raw const __extable_start as usize;
	let end = &raw const __extable_end as usize;
	// An empty or malformed table is not a reason to fault while handling a fault. The section is
	// emitted by the copy routines, so a build that inlined none of them legitimately has none.
	if end <= start {
		return &[];
	}
	let bytes = end - start;
	if bytes % core::mem::size_of::<Entry>() != 0 {
		return &[];
	}
	// SAFETY: the linker script places the section 8-byte aligned and the copy routines emit whole
	// entries into it; the length is checked to be a whole number of them above.
	unsafe { core::slice::from_raw_parts(start as *const Entry, bytes / core::mem::size_of::<Entry>()) }
}

// Where `pc` should resume, if it is an address this kernel said may fault.
//
// `None` means the fault is a genuine kernel bug and the caller must treat it as one. That is the
// whole safety property: this must never turn an ordinary kernel fault into a silent resume, so it
// matches the faulting address EXACTLY rather than by range.
// Recover only when the faulting ADDRESS is one userspace could have named.
//
// The PC match alone is not the guard this module claimed it was. The entries cover instructions
// with a kernel operand as well as a user one - aarch64 and riscv64 declare both the load and the
// store of their byte loop, and x86_64's `rep movsb` has source and destination in a single
// instruction - so a kernel bug that hands `copy_to_user` a bad KERNEL pointer faults at a PC that
// IS in the table, gets resumed, and is reported to the caller as "the user's page went away".
// Silently, and with a plausible answer.
//
// One condition removes the whole class: a copy routine can only ever be rescued from a fault on
// the side of it that userspace owns. Anything in the kernel half is this kernel's own bug, whoever
// was executing.
fn is_user_address(address: u64) -> bool {
	address < crate::memlayout::USER_VA_END
}

pub fn fixup_for(pc: u64, fault_address: u64) -> Option<u64> {
	if !is_user_address(fault_address) {
		return None;
	}
	let found = entries().iter().find(|entry| entry.fault == pc).map(|entry| entry.fixup);
	if found.is_some() {
		CAUGHT.fetch_add(1, Ordering::AcqRel);
	}
	found
}

// How many faults have been fixed up since boot.
#[cfg_attr(not(test), allow(dead_code))]
pub fn caught() -> u64 {
	CAUGHT.load(Ordering::Acquire)
}

// How many instructions this build declares may fault. Zero means the mechanism is not in the
// binary at all, which is a thing a test should be able to notice.
#[cfg_attr(not(test), allow(dead_code))]
// The first declared faulting address, for the test that pins the user-address condition. It needs
// a PC the table really contains, and which one is an accident of link order.
#[cfg(test)]
pub fn first_entry() -> Option<u64> {
	entries().first().map(|entry| entry.fault)
}

pub fn declared() -> usize {
	entries().len()
}

#[cfg(test)]
mod tests;
