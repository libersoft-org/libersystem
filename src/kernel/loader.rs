// Loads a userspace program from an in-memory ELF image into a fresh Process and
// schedules it. This is the bridge from the init package (raw ELF bytes) to a
// running ring-3 process: it builds a private address space, maps the program and
// a stack into it, endows the process with a bootstrap capability, and queues a
// thread that drops to ring 3 at the program's entry point.

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
	// The target is terminating, so it may not be extended - see the guard in `load_image_into`.
	// Separate from `BadImage` because the image is fine and the caller's mistake is a race, not a
	// malformed file; both refuse with `ERR_INVALID` at the syscall boundary.
	Terminating,
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
	// THE LAST GATE BEFORE RING 3. A thread can be enqueued and then have its process killed before
	// it is ever scheduled, and every other kill point is a SCHEDULING point - which this thread has
	// not reached yet. Entering here would run user code in an address space whose mappings and
	// handles have already been torn down. Returning instead reaps the thread the ordinary way.
	if let Some(thread) = crate::sched::current_thread() {
		let process = thread.process();
		if process.is_terminating() || process.is_killed() {
			return;
		}
	}
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
	// A short heap here refuses the load, which the caller already handles - the frames go back
	// with the address space rather than the kernel going down mid-spawn.
	let Some(process) = Process::new(address_space, domain) else {
		return Err(LoadError::OutOfMemory);
	};
	if let Some(name) = image_artifact_name(elf_image) {
		process.header().set_name(name);
	}
	// BOOKED BEFORE IT IS TAKEN. `adopt_frames` extends a vector, and extending an EMPTY vector
	// allocates too - the marker on it said "moved in whole at spawn", which describes where the
	// list came from and not whether the destination can hold it. Nothing is mapped into a process
	// anyone else can see yet, so the failure just returns the frames.
	if !process.reserve_adopt(frames.len(), shared.len()) {
		free_frames(frames);
		return Err(LoadError::OutOfMemory);
	}
	process.adopt_frames(frames);
	process.adopt_shared_pages(shared);
	process.charge_stack(USER_STACK_PAGES * PAGE_SIZE);
	// REFUSED RATHER THAN STARTED WITHOUT IT. A spawn whose bootstrap capability cannot be installed
	// is not a spawn: the child would run with nothing to talk to and the parent would be told it
	// succeeded.
	let Some(handle) = process.install(bootstrap, rights, badge) else {
		process.terminate();
		return Err(LoadError::OutOfMemory);
	};

	// FALLIBLY: this is the last allocation of a spawn and `Box::new` made a short heap a halt.
	let Some(ctx) = crate::mem::heap::try_box(UserEntry { entry, stack_top: USER_STACK_TOP, bootstrap: handle }) else {
		process.terminate();
		return Err(LoadError::OutOfMemory);
	};
	let raw_ctx = Box::into_raw(ctx);
	if sched::thread_create(process.clone(), user_process_trampoline, raw_ctx as u64).is_none() {
		// The last allocation of the load, and it used to be the one that panicked. Take
		// the context box back (the trampoline that would have consumed it never runs) and
		// let the process drop, which unmaps its segments and returns its frames.
		drop(unsafe { Box::from_raw(raw_ctx) });
		process.terminate();
		return Err(LoadError::OutOfMemory);
	}
	Ok(process)
}

// Load `elf_image` into an already-created `process`: map its PT_LOAD segments and
// a ring-3 stack into the process's address space and hand the leaf frames to the
// process to own (freed on its drop, like spawn_elf_process). Returns the program
// entry point. Unlike spawn_elf_process this neither creates the process nor
// starts a thread: the userspace spawn path (process_create / process_load /
// thread_create / thread_start) drives those as separate, capability-gated steps.
pub fn load_image_into(process: &Process, elf_image: &[u8]) -> Result<u64, LoadError> {
	// THE GUARD LIVES WITH THE OPERATION, not with one of its callers.
	//
	// These are the largest resource-extending operations the kernel has - a frame per page of
	// every PT_LOAD segment, a ring-3 stack, then `adopt_frames`, `adopt_shared_pages` and
	// `charge_stack` - and neither took `begin_extend`. `SYS_PROCESS_LOAD` did not even read
	// `is_terminating`: it looked the process up with MANAGE and loaded into it. So a load that
	// began before a `terminate()` and adopted after it handed a dead process a page table full of
	// live mappings and a frame list the teardown snapshot never saw.
	//
	// Say what that is and is not. `Drop for Process` unmaps and frees adopted frames when the last
	// reference goes, so it is not the physical use-after-free the first audit of this area found -
	// it is a resource set that grows after the barrier that exists to close it, and holds until the
	// last handle to a dead process is dropped. The invariant "after `begin_teardown` a process's
	// resource set cannot grow" was simply not true.
	//
	// Here rather than in the syscall because the boot path loads too, and a rule that travels with
	// the operation cannot be forgotten by a new caller. Boot takes it without noticing: a process
	// it has just created is not terminating. The guard is held to the end, which is after the
	// adopts and after every rollback.
	let Some(_extend) = process.begin_extend() else {
		return Err(LoadError::Terminating);
	};
	// AND THE ADOPTION BOOKING, which `begin_extend` does not serialise - see `Process::image_load`.
	// Two loads into one process would otherwise both pass `reserve_adopt` and the second would
	// extend infallibly.
	let Some(_load) = process.begin_image_load() else {
		return Err(LoadError::Terminating);
	};
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
		// Walked rather than collected: this is the cleanup for an allocation failure, and it used
		// to ask the heap for a `Vec` of ranges before it could give any memory back.
		elf::for_each_loaded_range(elf_image, |start, end| {
			let mut address = start;
			while address < end {
				let _ = process.address_space().unmap(address);
				address += PAGE_SIZE;
			}
		});
		free_frames(frames);
		return Err(err);
	}
	// From here on the Process owns the frames and frees them when it is dropped.
	// The name comes from the executable rather than from whichever module loaded last, so a
	// dynamic program is reported under its own artifact and not under a provider of its.
	if let Some(name) = image_artifact_name(elf_image) {
		process.header().set_name(name);
	}
	// The same booking, and here the unwind has more to do: the segments and the stack are mapped
	// into a live address space, so they come out of the page tables before the frames go back.
	if !process.reserve_adopt(frames.len(), shared.len()) {
		elf::for_each_loaded_range(elf_image, |start, end| {
			let mut address = start;
			while address < end {
				let _ = process.address_space().unmap(address);
				address += PAGE_SIZE;
			}
		});
		unmap_stack(process.address_space(), USER_STACK_PAGES);
		free_frames(frames);
		return Err(LoadError::OutOfMemory);
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
	// The same guard, and this one is by definition an operation on a process that already exists
	// and may already be dying. It also claims a module slot and registers dynamic symbols, both of
	// which are records the teardown snapshot has to see.
	let Some(_extend) = process.begin_extend() else {
		return Err(LoadError::Terminating);
	};
	// The same booking as `load_image_into`: modules adopt into the same two vectors, and this is
	// the path the audit named, since `SYS_PROCESS_LOAD_MODULE` runs against a process that already
	// owns its main image.
	let Some(_load) = process.begin_image_load() else {
		return Err(LoadError::Terminating);
	};
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
	// BOOKED BEFORE THE POINT OF NO RETURN. This is the caller that made the old marker false: the
	// process already owns every frame of its main image, so the adopt at the end extends a vector
	// with contents in it, on the `SYS_PROCESS_LOAD_MODULE` path. Reserving here rather than at the
	// adopt puts the failure inside the transaction that already knows how to unwind - before the
	// symbols are registered, which is the step this function cannot take back.
	if !process.reserve_adopt(frames.len(), shared.len()) {
		process.release_dynamic_module_at(bias);
		elf::unmap_module(elf_image, process.address_space(), bias);
		free_frames(frames);
		return Err(LoadError::OutOfMemory);
	}
	if !process.register_dynamic_symbols(&exports) {
		process.release_dynamic_module_at(bias);
		elf::unmap_module(elf_image, process.address_space(), bias);
		// The Domain booking `reserve_adopt` took above: this is the one rollback that happens
		// AFTER a successful reservation, so it is the one that has to give it back.
		process.release_adopt_charge(frames.len());
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
	// FALLIBLY: `SYS_THREAD_CREATE` reaches here, so a short heap must be the `None` this function
	// already returns rather than a kernel abort.
	let ctx = crate::mem::heap::try_box(UserEntry { entry, stack_top, bootstrap })?;
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
	// The whole stack's worth of records booked once, before the first frame is taken: the same
	// rule as the segment loop, and cheaper here because the count is known.
	if frames.try_reserve(USER_STACK_PAGES as usize).is_err() {
		return Err(LoadError::OutOfMemory);
	}
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
	// These frames were mapped into a live address space a moment ago. Every other core has to
	// have dropped its translations before the allocator may hand them to anyone else, and
	// `frame::retire` is the one place that decides so - it does the shootdown, frees on success
	// and quarantines when a core did not answer. This used to do the shootdown here and free
	// regardless of the outcome.
	//
	// SAFETY: every frame here was allocated by this load and never adopted by a Process, so this
	// call is its only owner.
	unsafe {
		frame::retire(&frames);
		// A rollback frees a whole image at once, so it is worth the shootdown now rather than
		// leaving it to whoever next crosses the drain threshold.
		frame::drain_quarantine();
	}
}
