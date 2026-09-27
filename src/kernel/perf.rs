// THE FRAME ACCOUNT'S RECORD BUFFER, in the kernel.
//
// ONE BUFFER, ATTACHED ONLY ON A `development-trace` BOOT. Every process on the measured frame path
// appends here - the application, DisplayService, the display driver - and the scheduler adds a
// record at every context switch and every wake, so a term's work and its wait follow from the same
// clock as its sites. On every other boot nothing is allocated, both syscalls answer
// `ERR_UNSUPPORTED`, and each scheduler hook is one test of the armed flag.
//
// THE LAYOUT, THE BOUND, THE REFUSAL COUNT AND THE LINE FORM ARE `perfbuf`'s, which is host-tested;
// what is here is only what needs a kernel: the storage, the thread and core a record is stamped
// with, the names of the processes, and the serial wire the drain goes out on.

use crate::arch;
use crate::object::KernelObject;
use crate::object::thread::Thread;
use crate::serial_println;
use abi::{ERR_INVALID, ERR_UNSUPPORTED, PERF_CONTROL_ARM, PERF_CONTROL_DISARM, PERF_CONTROL_DRAIN, PERF_RECORD_REFUSED, PERF_RECORD_UNARMED};
use perfbuf::{Buffer, Push, Record};

static BUFFER: Buffer = Buffer::new();

// Give the buffer its 8 MiB, on the boot that asked to be measured and on no other.
//
// CONTIGUOUS AND THROUGH THE DIRECT MAP, because an append runs with interrupts masked inside the
// scheduler and inside a syscall, and a slot address must be one addition away. A boot that cannot
// spare the memory says so and runs unmeasured: the instrument is never a reason to fail a boot.
pub fn init() {
	if arch::boot_profile() != Some("development-trace") {
		return;
	}
	let pages = crate::mem::frame::pages_for(perfbuf::BUFFER_BYTES);
	let Some(phys) = crate::mem::frame::allocate_contiguous(pages) else {
		serial_println!("perf: the frame-account buffer ({} bytes) could not be allocated - this boot is not measurable", perfbuf::BUFFER_BYTES);
		return;
	};
	if !crate::mem::within_direct_map(phys, perfbuf::BUFFER_BYTES as u64) {
		serial_println!("perf: the frame-account buffer landed outside the direct map - this boot is not measurable");
		return;
	}
	let base = (crate::mem::hhdm_offset() + phys) as *mut Record;
	// SAFETY: the frames were just allocated for this and nothing else holds them, and the direct
	// map covers the whole range (checked above) for the life of the kernel.
	unsafe {
		core::ptr::write_bytes(base as *mut u8, 0, perfbuf::BUFFER_BYTES);
		BUFFER.attach(base, perfbuf::CAPACITY);
	}
	serial_println!("perf: frame-account buffer attached - {} records of {} bytes", perfbuf::CAPACITY, perfbuf::RECORD_BYTES);
}

// The one test every scheduler hook makes.
#[inline(always)]
pub fn armed() -> bool {
	BUFFER.armed()
}

fn core_id() -> u16 {
	crate::sched::current_cpu_id() as u16
}

fn koid32(koid: u64) -> u32 {
	koid as u32
}

// Name a thread's process in this window, once: the first record a thread appears in writes the
// four-chunk group the drain's table is built from.
fn note(thread: &Thread) {
	let generation = BUFFER.generation();
	if thread.perf_window().swap(generation, core::sync::atomic::Ordering::Relaxed) == generation {
		return;
	}
	let process = thread.process();
	let mut name = [0u8; perfbuf::NAME_CHUNKS * 8];
	process.header().with_name(|label| {
		if let Some(label) = label {
			let bytes = label.as_bytes();
			let len = bytes.len().min(name.len());
			name[..len].copy_from_slice(&bytes[..len]);
		}
	});
	let group = perfbuf::name_records(koid32(thread.header().koid()), process.header().koid(), &name, arch::tsc::now(), core_id());
	BUFFER.push_group(&group);
}

// `SYS_PERF_RECORD(site, cycles, value)`: one site's record, stamped with the caller's thread and
// core.
pub fn sys_record(site: u64, cycles: u64, value: u64) -> i64 {
	if !BUFFER.attached() {
		return ERR_UNSUPPORTED;
	}
	if !BUFFER.armed() {
		return PERF_RECORD_UNARMED;
	}
	let thread = crate::sched::current_thread();
	let koid = thread.as_ref().map_or(0, |thread| koid32(thread.header().koid()));
	if let Some(thread) = thread.as_deref() {
		note(thread);
	}
	match BUFFER.push(Record { site: perfbuf::site_tag(site), cycles, value, thread: koid, core: core_id(), kind: perfbuf::KIND_SITE, detail: 0 }) {
		Push::Accepted => 0,
		Push::Unarmed => PERF_RECORD_UNARMED,
		Push::Refused => PERF_RECORD_REFUSED,
	}
}

// `SYS_PERF_CONTROL(op)`: arm, disarm, or drain to the debug serial.
pub fn sys_control(op: u64) -> i64 {
	if !BUFFER.attached() {
		return ERR_UNSUPPORTED;
	}
	match op {
		PERF_CONTROL_ARM => {
			BUFFER.arm(arch::tsc::now());
			0
		}
		PERF_CONTROL_DISARM => {
			BUFFER.disarm();
			0
		}
		PERF_CONTROL_DRAIN => drain() as i64,
		_ => ERR_INVALID,
	}
}

// THE DRAIN GOES OUT ON THE DEBUG SERIAL AND NOT A BYTE OF IT MAY BE LOST. The ordinary debug write
// is best-effort - it drops what the transmit ring cannot hold, which is right for a console mirror
// and fatal for a record stream whose last line is the count a collector checks - so each line is
// pushed until the ring has taken all of it, draining the UART synchronously whenever it is full.
// The console mirror is left out: megabytes of record lines are addressed to a program.
fn drain() -> u64 {
	let drained = perfbuf::drain_lines(&BUFFER, &mut |line: &[u8]| {
		let _guard = crate::print_lock();
		let mut rest = line;
		while !rest.is_empty() {
			let taken = arch::serial::write_bytes(rest);
			rest = &rest[taken..];
			if !rest.is_empty() {
				arch::serial::flush_sync();
			}
		}
	});
	arch::serial::flush_sync();
	drained.records
}

// A context switch, while armed: `prev` leaves for `why`, `next` runs. `None` is the core's idle
// context.
pub fn switch(prev: Option<&Thread>, next: Option<&Thread>, why: u8) {
	let cycles = arch::tsc::now();
	for thread in [prev, next].into_iter().flatten() {
		note(thread);
	}
	let outgoing = prev.map_or(0, |thread| koid32(thread.header().koid()) as u64);
	let incoming = next.map_or(0, |thread| koid32(thread.header().koid()));
	BUFFER.push(Record { site: [0; 8], cycles, value: outgoing, thread: incoming, core: core_id(), kind: perfbuf::KIND_SWITCH, detail: why });
}

// A thread made runnable, while armed, on the core it is queued on. Its waker is the thread running
// here - the sender of a message, say - except for a deadline, which the tick passed and nobody sent.
pub fn wake(woken: &Thread, cause: u8) {
	let cycles = arch::tsc::now();
	let waker = if cause == perfbuf::WOKEN_BY_DEADLINE {
		0
	} else {
		let current = crate::sched::current_thread();
		if let Some(current) = current.as_deref() {
			note(current);
		}
		current.map_or(0, |thread| koid32(thread.header().koid()) as u64)
	};
	note(woken);
	BUFFER.push(Record { site: [0; 8], cycles, value: waker, thread: koid32(woken.header().koid()), core: core_id(), kind: perfbuf::KIND_WAKE, detail: cause });
}
