// resource_probe - the M39 governed component.
//
// ResourceManager launches this program into a bounded sub-Domain it controls, sets a
// memory budget on that Domain, then drives the probe to demonstrate that the kernel
// enforces the budget and that going over it is handled gracefully rather than crashing
// the component.
//
// The probe holds one channel to the manager (its bootstrap). On each command it
// allocates one-page memory objects in a tight loop, keeping every handle alive, until a
// create is refused with ERR_RESOURCE_EXHAUSTED - the kernel's first-class over-budget
// error. It does not fault or exit on that refusal: it stops, reports DONE, and waits for
// the next command. When the manager raises the budget at runtime and commands again, the
// probe continues allocating into the new headroom. Between commands it parks blocked on
// its channel, holding all its objects alive, so the manager's live usage reads reflect
// real consumption.
//
// The probe is deliberately heap-free (it never touches `alloc`), so its baseline Domain
// charge is exactly its eagerly-mapped image and stack: every later page the Domain
// accounts is one of these explicit one-page objects, which is what makes the manager's
// budget arithmetic exact.

#![no_std]
#![no_main]

use rt::*;

// Each create asks for one byte, which the kernel rounds up to and charges as exactly one
// page - so the number of objects the probe holds equals the number of pages it has been
// granted out of its Domain's budget.
const OBJECT_SIZE: u64 = 1;

// The most objects the probe will hold across all rounds. The budget is always far smaller
// than this, so the probe stops on the budget (ERR_RESOURCE_EXHAUSTED), never on a full
// array - the array is only a fixed, heap-free backing store that keeps the handles alive.
const CAPACITY: usize = 64;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 64] = [0u8; 64];

	// The handles of every object allocated so far, kept alive (never closed) so the
	// charge against the Domain persists. `held` carries across commands, so a later round
	// allocates into the headroom a raised budget opened rather than starting over.
	let mut held: [u64; CAPACITY] = [0u64; CAPACITY];
	let mut count: usize = 0;

	loop {
		// Wait for the manager's next command. The manager drops the channel (or never
		// sends again) when it is done, which parks us here holding our objects alive.
		match unsafe { recv_blocking(bootstrap, &mut buf) } {
			Received::Message { .. } => {}
			Received::Closed => exit(),
		}

		// Allocate one-page objects until the kernel refuses one (we are over the Domain's
		// memory budget). A refusal is a typed error, not a crash: we stop and keep every
		// object we did get, then report DONE and wait for the next command.
		while count < CAPACITY {
			let handle: i64 = unsafe { memory_object_create(OBJECT_SIZE) };
			if handle < 0 {
				break;
			}
			held[count] = handle as u64;
			count += 1;
		}

		unsafe {
			send_blocking(bootstrap, b"DONE", 0);
		}
	}
}
