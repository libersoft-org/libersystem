// lifecheck - observe what ran before this program's first line, and what runs after its last.
//
// WHAT IT IS FOR. The pinned foreign configuration's converged closure names one constructor and one
// destructor, which makes static initialisation a mechanism this system ADMITS. An admitted
// mechanism needs its contract observed rather than described, and none of it is observable from
// outside a process: a constructor runs before the console is adopted, and a destructor runs after
// the last line of the program. So this program carries its own constructor and destructor, reads
// the record its PROVIDER's constructor left, and prints what it finds.
//
// THE FOUR THINGS IT SHOWS, one per mode:
//   order     the sequence recorded by the two constructors, which must read provider-then-consumer
//   errno     the address the provider gets and the address this program gets, which must be one
//   exit      the destructors on a NORMAL exit, printed live and in reverse of the construction
//   crash     the same program faulting instead of exiting, where neither destructor prints

#![no_std]
#![no_main]

extern crate alloc;

use proto::system::LaunchContext;
use rt::*;

unsafe extern "C" {
	fn liber_lifecycle_note(mark: u8);
	fn liber_lifecycle_sequence() -> *const u8;
	fn liber_lifecycle_incomplete() -> i32;
	fn liber_lifecycle_partial() -> i32;
	fn liber_lifecycle_set_reporter(report: extern "C" fn(*const u8));
	fn liber_lifecycle_errno_slot() -> *const core::ffi::c_void;
	fn liber_lifecycle_errno_location() -> *mut i32;
}

// THE CONSUMER'S OWN CONSTRUCTOR, placed in `.init_array` the way a C object's would be.
//
// `#[used]` IS WHAT KEEPS IT. Nothing references this static, so without it the compiler is entitled
// to drop the whole entry - and a constructor the linker never saw is not a constructor that failed
// to run, it is one that was never there.
extern "C" fn consumer_constructed() {
	// SAFETY: the provider is fully loaded and relocated before any constructor runs - it is mapped
	// first, in dependency order, and its own constructor has already run by the time this does.
	unsafe { liber_lifecycle_note(b'C') };
}

extern "C" fn consumer_destroyed() {
	print(b"lifecheck: consumer destructor\n");
}

// WHAT THE PROVIDER PRINTS THROUGH. It has no console of its own, so the ORDER of the two
// destructors is only visible if both can say something - and this is the consumer lending it the
// one it adopted.
extern "C" fn report(line: *const u8) {
	// SAFETY: the provider passes a NUL-terminated string literal out of its own image.
	unsafe {
		let mut len = 0usize;
		while *line.add(len) != 0 && len < 128 {
			len += 1;
		}
		print(core::slice::from_raw_parts(line, len));
	}
	print(b"\n");
}

#[used]
#[unsafe(link_section = ".init_array")]
static CONSTRUCTOR: extern "C" fn() = consumer_constructed;

#[used]
#[unsafe(link_section = ".fini_array")]
static DESTRUCTOR: extern "C" fn() = consumer_destroyed;

fn sequence() -> &'static [u8] {
	// SAFETY: the provider's buffer is a static array with a NUL terminator that its own constructor
	// wrote before this program's entry point could run.
	unsafe {
		let start = liber_lifecycle_sequence();
		let mut len = 0usize;
		while *start.add(len) != 0 && len < 32 {
			len += 1;
		}
		core::slice::from_raw_parts(start, len)
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	inherit_stdout(bootstrap);
	let mode: &[u8] = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => {
			let bytes = context.arguments.clone().into_bytes();
			if bytes.is_empty() { b"order" } else { leaked(bytes) }
		}
		None => b"order",
	};

	print(b"lifecheck: sequence=");
	print(sequence());
	print(b"\n");
	// WHAT A CONSTRUCTOR THAT DID NOT FINISH LEFT BEHIND. The provider sets the marker before the
	// step that could fail and clears it after, so this distinguishes "never started" from "started
	// and did not finish" - which is what a partial initialisation IS.
	print(if unsafe { liber_lifecycle_incomplete() } == 0 { b"lifecheck: provider initialisation complete\n" } else { b"lifecheck: provider initialisation INCOMPLETE\n" });
	// AND WHAT THE CONSTRUCTOR THAT DID NOT FINISH LEFT BEHIND. It is a different question from the
	// one above: that one asks whether the completing constructor completed, this one asks whether
	// anything stopped half way - and a partial initialisation is exactly the state where the first
	// answer is yes and the second is too.
	print(if unsafe { liber_lifecycle_partial() } == 0 { b"lifecheck: no partial initialisation\n" } else { b"lifecheck: partial initialisation left behind\n" });
	// SAFETY: the provider is loaded and relocated; the reporter is a plain function pointer into
	// this image, which outlives every destructor that could call it.
	unsafe { liber_lifecycle_set_reporter(report) };

	// SAFETY: both are calls into already-relocated images; neither dereferences what it returns.
	let (provider_slot, own_slot) = unsafe { (liber_lifecycle_errno_slot(), liber_lifecycle_errno_location() as *const core::ffi::c_void) };
	print(if provider_slot == own_slot { b"lifecheck: errno is process-wide\n" } else { b"lifecheck: errno differs between images\n" });

	if mode == b"crash" {
		// A CRASH IS NOT AN EXIT, and the difference is the whole of a lifecycle contract. Nothing
		// below this line runs, and neither does any destructor: this thread does not return to the
		// runtime, so the finaliser walk never happens.
		print(b"lifecheck: crashing\n");
		// SAFETY: deliberately not safe. A write through a null pointer is the shortest fault this
		// system will not let a process survive, which is exactly what is being observed.
		unsafe { core::ptr::write_volatile(core::ptr::null_mut::<u64>(), 0) };
	}
	print(b"lifecheck: exiting\n");
	exit();
}

// The argument bytes outlive this call because nothing frees them before the process ends; a mode
// word is read once and compared, and copying it into a fixed buffer would add a bound to a value
// whose length is already bounded by the launch protocol.
fn leaked(bytes: alloc::vec::Vec<u8>) -> &'static [u8] {
	alloc::vec::Vec::leak(bytes)
}
