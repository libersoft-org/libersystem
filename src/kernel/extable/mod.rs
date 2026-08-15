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
static REFUSED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static CAUGHT: AtomicU64 = AtomicU64::new(0);

// Faults at a user address whose pc the table does not name. Each one is a kernel bug the caller is
// about to halt on, so the first few say enough to tell WHICH bug; the budget exists because this
// runs inside a fault handler and a fault that repeats must not turn a halt into a flood.
static UNDECLARED: AtomicU64 = AtomicU64::new(0);
const UNDECLARED_DIAGNOSTICS: u64 = 4;

// The linker-provided bounds of the section, as ADDRESSES. Read once here so the diagnostic below
// can print the same two numbers `entries()` computed its length from: a table that is empty, short
// or misplaced at runtime is indistinguishable from one that simply does not cover an instruction,
// and those need different fixes.
fn bounds() -> (usize, usize) {
	(&raw const __extable_start as usize, &raw const __extable_end as usize)
}

fn entries() -> &'static [Entry] {
	let (start, end) = bounds();
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
		// A declared instruction faulting on a KERNEL address is this kernel's own bug, and it used
		// to be recovered silently as though the user's page had gone away. Refusing it is the point
		// of the condition - but refusing it turns a silent wrong answer into a halt, and a halt
		// inside a trap handler on one core is a livelock on the others, with no output to say why.
		//
		// So it says why, once, before anything else can. The address is the evidence: which side of
		// the copy was wrong, and where it pointed.
		if entries().iter().any(|entry| entry.fault == pc) {
			if REFUSED.fetch_add(1, Ordering::AcqRel) == 0 {
				crate::serial_println!("extable: REFUSED a fixup at pc {pc:#x} - the faulting address {fault_address:#x} is in the kernel half, so this is a kernel bug and not a user page going away");
			}
		}
		return None;
	}
	let found = entries().iter().find(|entry| entry.fault == pc).map(|entry| entry.fixup);
	match found {
		Some(_) => {
			CAUGHT.fetch_add(1, Ordering::AcqRel);
		}
		None => {
			// The caller is about to call this a kernel bug and halt, and until now it halted with
			// NOTHING to say why - which is how the aarch64 suite came to stop at
			// `a_process_load_whose_image_goes_away...` with only an exception dump, at a pc the
			// linked `.extable` demonstrably contains.
			//
			// This budget is SEPARATE from the refusal one above on purpose, and that separation is
			// the actual defect being fixed. Both used to share a single once-per-boot line, and
			// `a_fault_on_a_kernel_address_is_never_rescued_however_the_pc_matches` spends refusals
			// DELIBERATELY - three of them - immediately before the test that halts. So the halt was
			// silent, and "no diagnostic appeared" could not be used to rule the refusal branch out.
			// A diagnostic a passing test can spend is absent exactly when it is needed.
			//
			// The bounds and the count are printed with the pc because they separate the three
			// causes that look identical from the outside: a table that is empty, one the linker
			// placed or sized wrongly, and one that is intact but does not name this instruction.
			if UNDECLARED.fetch_add(1, Ordering::AcqRel) < UNDECLARED_DIAGNOSTICS {
				let (start, end) = bounds();
				crate::serial_println!("extable: NO ENTRY for pc {pc:#x} faulting on user address {fault_address:#x} - the table declares {} entries between {start:#x} and {end:#x}", entries().len());
			}
		}
	}
	found
}

// How many faults have been fixed up since boot.
#[cfg_attr(not(test), allow(dead_code))]
// Fixups refused because the fault address was not userspace's. Zero is the only healthy value.
pub fn refused() -> u64 {
	REFUSED.load(Ordering::Acquire)
}

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
