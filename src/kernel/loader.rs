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
use crate::object::thread::Thread;
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
// The artifact name an image carries in its own identity note, which is what lets a fault
// message say which program faulted. Nothing else names a process: `set_name` had exactly one
// caller, the property syscall, so every process in a booted system was anonymous and a fault
// could only be attributed by guessing which image an address fell in and disassembling it -
// a guess that sent one investigation down the wrong path for hours.
//
// Taking it from the image rather than from a parameter is what makes it uniform: every spawn
// path already has the bytes, and an image that carries no note (a hand-laid test fixture)
// simply stays unnamed rather than forcing every caller to invent one.
fn image_artifact_name(elf_image: &[u8]) -> Option<&str> {
	let note = bootproto::elf::Elf::parse(elf_image)?.liber_identity_note()?;
	let value = note.split(|byte| *byte == b'\n').find_map(|line| line.strip_prefix(b"artifact=".as_slice()))?;
	core::str::from_utf8(value).ok().filter(|name| !name.is_empty())
}

pub fn spawn_elf_process(domain: Arc<Domain>, elf_image: &[u8], bootstrap: Arc<dyn KernelObject>, rights: Rights, badge: u64) -> Result<Arc<Process>, LoadError> {
	let address_space = AddressSpace::create().ok_or(LoadError::OutOfMemory)?;
	let mut frames: Vec<u64> = Vec::new();
	let mut shared = Vec::new();

	let entry = match elf::load_into(elf_image, &address_space, &mut frames, &mut shared) {
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
	if let Some(name) = image_artifact_name(elf_image) {
		process.header().set_name(name);
	}
	process.adopt_frames(frames);
	process.adopt_shared_pages(shared);
	process.charge_stack(USER_STACK_PAGES * PAGE_SIZE);
	let handle = process.install(bootstrap, rights, badge);

	let ctx = Box::new(UserEntry { entry, stack_top: USER_STACK_TOP, bootstrap: handle });
	sched::thread_create(process.clone(), user_process_trampoline, Box::into_raw(ctx) as u64);
	Ok(process)
}

// Load `elf_image` into an already-created `process`: map its PT_LOAD segments and
// a ring-3 stack into the process's address space and hand the leaf frames to the
// process to own (freed on its drop, like spawn_elf_process). Returns the program
// entry point. Unlike spawn_elf_process this neither creates the process nor
// starts a thread: the userspace spawn path (process_create / process_load /
// thread_create / thread_start) drives those as separate, capability-gated steps.
pub fn load_image_into(process: &Process, elf_image: &[u8]) -> Result<u64, LoadError> {
	if process.has_dynamic_modules() && bootproto::elf::Elf::parse(elf_image).is_none_or(|image| image.image_type != bootproto::elf::ET_DYN) {
		return Err(LoadError::BadImage);
	}
	let mut frames: Vec<u64> = Vec::new();
	let mut shared = Vec::new();
	let entry = match elf::load_resolved_into(elf_image, process.address_space(), &mut frames, &mut shared, &|name| process.resolve_dynamic_symbol(name)) {
		Ok(entry) => entry,
		Err(err) => {
			free_frames(frames);
			return Err(err.into());
		}
	};
	if let Err(err) = map_stack(process.address_space(), &mut frames) {
		// UNMAP before freeing. The segments above are mapped into the process's live page
		// tables, and `free_frames` hands their frames back to the allocator - so without
		// this the tables went on naming memory that had been given away, and the next
		// owner of those frames shared them with a live address space. A use-after-free of
		// physical memory, reachable by nothing more than running out of memory at the
		// wrong moment.
		for (start, end) in elf::loaded_ranges(elf_image) {
			let mut address = start;
			while address < end {
				let _ = process.address_space().unmap(address);
				address += PAGE_SIZE;
			}
		}
		free_frames(frames);
		return Err(err);
	}
	// From here on the Process owns the frames and frees them when it is dropped.
	// The name comes from the executable rather than from whichever module loaded last, so a
	// dynamic program is reported under its own artifact and not under a provider of its.
	if let Some(name) = image_artifact_name(elf_image) {
		process.header().set_name(name);
	}
	process.adopt_frames(frames);
	process.adopt_shared_pages(shared);
	process.charge_stack(USER_STACK_PAGES * PAGE_SIZE);
	Ok(entry)
}

// Map one ET_DYN dependency at the bias chosen by ProcessService. Unlike the main
// image load this does not map a stack or create a thread; providers are loaded in
// dependency order and the main SYS_PROCESS_LOAD remains the transaction's final step.
pub fn load_module_into(process: &Process, elf_image: &[u8], bias: u64) -> Result<(), LoadError> {
	if !process.reserve_dynamic_module_at(bias) {
		return Err(LoadError::BadImage);
	}
	let mut frames: Vec<u64> = Vec::new();
	let mut shared = Vec::new();
	let exports = match elf::load_module_into(elf_image, process.address_space(), &mut frames, &mut shared, bias, &|name| process.resolve_dynamic_symbol(name)) {
		Ok(exports) => exports,
		Err(err) => {
			process.release_dynamic_module_at(bias);
			free_frames(frames);
			return Err(err.into());
		}
	};
	if !process.register_dynamic_symbols(&exports) {
		process.release_dynamic_module_at(bias);
		elf::unmap_module(elf_image, process.address_space(), bias);
		free_frames(frames);
		return Err(LoadError::BadImage);
	}
	process.adopt_frames(frames);
	process.adopt_shared_pages(shared);
	Ok(())
}

// Build a process's ring-3 entry thread, suspended (off every run queue). The
// thread drops to ring 3 at `entry` on the stack topped at `stack_top`, with
// `bootstrap` delivered in rdi. Returns None if the process Domain is at its
// thread cap, reclaiming the boxed entry context. thread_start later enqueues it.
pub fn create_user_thread(process: &Arc<Process>, entry: u64, stack_top: u64, bootstrap: u64) -> Option<Arc<Thread>> {
	let ctx = Box::new(UserEntry { entry, stack_top, bootstrap });
	let raw = Box::into_raw(ctx) as u64;
	match sched::thread_create_suspended(process.clone(), user_process_trampoline, raw) {
		Some(thread) => Some(thread),
		None => {
			// The thread was not created, so reclaim the leaked entry context.
			drop(unsafe { Box::from_raw(raw as *mut UserEntry) });
			None
		}
	}
}

// Map the ring-3 stack (zeroed, writable, never executable) just below
// USER_STACK_TOP.
fn map_stack(address_space: &AddressSpace, frames: &mut Vec<u64>) -> Result<(), LoadError> {
	let flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER | arch::paging::NO_EXECUTE;
	let hhdm = hhdm_offset();
	let base = USER_STACK_TOP - USER_STACK_PAGES * PAGE_SIZE;
	for page in 0..USER_STACK_PAGES {
		let Some(frame) = frame::allocate() else {
			unmap_stack(address_space, page);
			return Err(LoadError::OutOfMemory);
		};
		frames.push(frame);
		unsafe {
			core::ptr::write_bytes((hhdm + frame) as *mut u8, 0, PAGE_SIZE as usize);
		}
		if address_space.try_map(base + page * PAGE_SIZE, frame, flags).is_err() {
			unmap_stack(address_space, page);
			return Err(LoadError::OutOfMemory);
		}
	}
	Ok(())
}

// Take down the stack pages mapped so far, for a stack mapping that failed partway. The
// frames themselves are in the caller's `frames` list and are freed with the rest; what
// this removes is the page-table entries that would otherwise outlive them.
fn unmap_stack(address_space: &AddressSpace, mapped_pages: u64) {
	let base = USER_STACK_TOP - USER_STACK_PAGES * PAGE_SIZE;
	for page in 0..mapped_pages {
		let _ = address_space.unmap(base + page * PAGE_SIZE);
	}
}

// Free frames accumulated on an error path, before any Process exists to adopt
// them. The half-built address space frees its own page tables when it is dropped.
fn free_frames(frames: Vec<u64>) {
	if frames.is_empty() {
		return;
	}
	// These frames were mapped into a live address space a moment ago. Every other core
	// has to have dropped its translations before the allocator may hand them to anyone
	// else - see `mem::tlb`.
	crate::mem::tlb::shootdown();
	for frame in frames {
		frame::deallocate(frame);
	}
}
