// Loads a userspace program from an in-memory ELF image into a fresh Process and
// schedules it. This is the bridge from the init package (raw ELF bytes) to a
// running ring-3 process: it builds a private address space, maps the program and
// a stack into it, endows the process with a bootstrap capability, and queues a
// thread that drops to ring 3 at the program's entry point.

#![allow(dead_code)]

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::arch;
use crate::elf::{self, ElfError};
use crate::mem::frame::{self, PAGE_SIZE};
use crate::mem::hhdm_offset;
use crate::memlayout::{USER_STACK_PAGES, USER_STACK_TOP};
use crate::object::KernelObject;
use crate::object::address_space::AddressSpace;
use crate::object::domain::Domain;
use crate::object::process::Process;
use crate::object::rights::Rights;
use crate::sched;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LoadError {
	OutOfMemory,
	BadImage,
}

impl From<ElfError> for LoadError {
	fn from(err: ElfError) -> Self {
		match err {
			ElfError::OutOfMemory => LoadError::OutOfMemory,
			_ => LoadError::BadImage,
		}
	}
}

// The values the ring-3 entry trampoline needs, boxed and passed through the
// thread's single u64 argument.
struct UserEntry {
	entry: u64,
	stack_top: u64,
	bootstrap: u64,
}

// Thread body for a userspace process: unbox the entry context and drop to ring 3.
// enter() returns once the program exits or faults, after which this body returns
// and the thread is reaped - tearing the Process (and its address space and
// frames) down.
extern "C" fn user_process_trampoline(ctx: u64) {
	let boxed = unsafe { Box::from_raw(ctx as *mut UserEntry) };
	let UserEntry { entry, stack_top, bootstrap } = *boxed;
	unsafe {
		arch::usermode::enter(entry, stack_top, bootstrap);
	}
}

// Load `elf_image` into a new Process accounted to `domain`, seed it with a
// bootstrap capability to `bootstrap`, and schedule it. Returns the Process.
pub fn spawn_elf_process(domain: Arc<Domain>, elf_image: &[u8], bootstrap: Arc<dyn KernelObject>, rights: Rights, badge: u64) -> Result<Arc<Process>, LoadError> {
	let address_space = AddressSpace::create().ok_or(LoadError::OutOfMemory)?;
	let mut frames: Vec<u64> = Vec::new();

	let entry = match elf::load_into(elf_image, &address_space, &mut frames) {
		Ok(entry) => entry,
		Err(err) => {
			free_frames(frames);
			return Err(err.into());
		}
	};

	if let Err(err) = map_stack(&address_space, &mut frames) {
		free_frames(frames);
		return Err(err);
	}

	// From here on the Process owns the frames and frees them when it is dropped.
	let process = Process::new(address_space, domain);
	process.adopt_frames(frames);
	let handle = process.install(bootstrap, rights, badge);

	let ctx = Box::new(UserEntry { entry, stack_top: USER_STACK_TOP, bootstrap: handle });
	sched::thread_create(process.clone(), user_process_trampoline, Box::into_raw(ctx) as u64);
	Ok(process)
}

// Map the ring-3 stack (zeroed, writable) just below USER_STACK_TOP.
fn map_stack(address_space: &AddressSpace, frames: &mut Vec<u64>) -> Result<(), LoadError> {
	let flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER;
	let hhdm = hhdm_offset();
	let base = USER_STACK_TOP - USER_STACK_PAGES * PAGE_SIZE;
	for page in 0..USER_STACK_PAGES {
		let frame = frame::allocate().ok_or(LoadError::OutOfMemory)?;
		frames.push(frame);
		unsafe {
			core::ptr::write_bytes((hhdm + frame) as *mut u8, 0, PAGE_SIZE as usize);
		}
		address_space.map(base + page * PAGE_SIZE, frame, flags);
	}
	Ok(())
}

// Free frames accumulated on an error path, before any Process exists to adopt
// them. The half-built address space frees its own page tables when it is dropped.
fn free_frames(frames: Vec<u64>) {
	for frame in frames {
		frame::deallocate(frame);
	}
}
