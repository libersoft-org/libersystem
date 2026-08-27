// System call dispatch and the minimal syscall set.
//
// The architecture entry stub (arch::syscall) calls syscall_dispatch with the
// syscall number and up to four arguments; this module decodes the number and
// runs the matching handler. Handlers that touch per-process state (handles,
// objects, mappings) operate on the calling thread's handle table.
//
// Return convention: a successful call returns its result value
// (a handle, an address, a count, ...). An error returns a small negative value
// in the range [-4095, -1]; sys_is_err() tests for it. This lets a syscall return
// a higher-half kernel address - whose top bit is set - without it being mistaken
// for an error.

use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::memlayout::{KERNEL_MMAP_BASE, USER_MMAP_BASE, USER_VA_END};

use crate::arch;
use crate::device;
use crate::fault::FaultInfo;
use crate::loader::{self, LoadError};
use crate::mem::frame::PAGE_SIZE;
use crate::object::channel::{Channel, ChannelError, Message, RecvRefusal};
use crate::object::claim::Claim;
use crate::object::device_memory::DeviceMemory;
use crate::object::dma_buffer::DmaBuffer;
use crate::object::domain::Domain;
use crate::object::event::Event;
use crate::object::handle::{Capability, Handle, HandleError};
use crate::object::interrupt::Interrupt;
use crate::object::memory_object::{MemoryError, MemoryObject};
use crate::object::privilege::{Privilege, PrivilegeKind};
use crate::object::process::Process;
use crate::object::rights::Rights;
use crate::object::thread::Thread;
use crate::object::timer::Timer;
use crate::object::wait_set::{WaitSet, WaitSetError};
use crate::object::{KernelObject, ObjectType};
use crate::sched;

// The syscall numbers and error codes are the shared kernel/userspace ABI:
// defined once in the abi crate (the single source of truth) and re-exported
// here so the rest of the kernel keeps referring to them as `syscall::SYS_*` /
// `syscall::ERR_*`.
pub use abi::{ABI_VERSION, ERR_ABI_MISMATCH, ERR_ACCESS_DENIED, ERR_BAD_HANDLE, ERR_BAD_SYSCALL, ERR_INTERRUPTED, ERR_INVALID, ERR_NO_MEMORY, ERR_NO_THREAD, ERR_NOT_MAPPED, ERR_PEER_CLOSED, ERR_RESOURCE_EXHAUSTED, ERR_TIMED_OUT, ERR_UNSUPPORTED, ERR_WOULD_BLOCK, PROC_STATE_FAILED, PROC_STATE_RUNNING, PROC_STATE_STOPPED, PROP_DMA_LIMIT, PROP_HANDLE_LIMIT, PROP_IPC_QUEUE_LIMIT, PROP_MEMORY_LIMIT, PROP_NAME, PROP_STACK_LIMIT, PROP_THREAD_LIMIT, SIG_CONT, SIG_INT, SIG_KILL, SIG_STOP, SIG_TERM, SYS_ABI_CHECK, SYS_BOOT_ID, SYS_BOOT_PROFILE, SYS_CHANNEL_CREATE, SYS_CHANNEL_PEEK, SYS_CHANNEL_RECV, SYS_CHANNEL_RECV_CAPS, SYS_CHANNEL_SEND, SYS_CHANNEL_SEND_ATTENUATED, SYS_CHANNEL_SEND_CAPS, SYS_CLOCK_GET, SYS_CLOCK_MONO_NS, SYS_CLOCK_RTC, SYS_CONSOLE_ATTACH, SYS_CONSOLE_FEED, SYS_CONSOLE_READLOG, SYS_CPU_INFO, SYS_CPU_NAME, SYS_DEBUG_NOOP, SYS_DEBUG_WRITE, SYS_DEVICE_CLAIM, SYS_DEVICE_CLAIM_INFO, SYS_DEVICE_COUNT, SYS_DEVICE_INFO, SYS_DEVICE_MEMORY_MAP, SYS_DEVICE_MSIX_ACQUIRE, SYS_DEVICE_QUIESCED, SYS_DEVICE_RELEASE, SYS_DMA_BUFFER_CREATE, SYS_DMA_BUFFER_MAP, SYS_DMA_BUFFER_PHYS, SYS_DMA_BUFFER_UNMAP, SYS_DOMAIN_CREATE, SYS_DOMAIN_KILL, SYS_DOMAIN_STATS_GET, SYS_EVENT_CREATE, SYS_EVENT_POLL, SYS_EVENT_SIGNAL, SYS_FAULT_INFO_GET, SYS_FRAMEBUFFER_MAP, SYS_HANDLE_CLOSE, SYS_HANDLE_DUPLICATE, SYS_INTERRUPT_ACK, SYS_INTERRUPT_BIND, SYS_IRQ_INFO, SYS_MEMMAP_GET, SYS_MEMORY_MAP, SYS_MEMORY_OBJECT_CREATE, SYS_MEMORY_STATS, SYS_MEMORY_UNMAP, SYS_OBJECT_INFO_GET, SYS_OBJECT_PROPERTY_SET, SYS_PCI_INFO, SYS_PROCESS_CREATE, SYS_PROCESS_GROUP_CREATE, SYS_PROCESS_GROUP_SIGNAL, SYS_PROCESS_GROUP_STATS, SYS_PROCESS_LOAD, SYS_PROCESS_LOAD_MODULE, SYS_PROCESS_SIGNAL, SYS_PROCESS_STATS_GET, SYS_RANDOM_GET, SYS_RANDOM_INSECURE, SYS_SIGNAL_CATCH, SYS_SIGNAL_TAKE, SYS_SYSTEM_POWER, SYS_THREAD_CREATE, SYS_THREAD_START, SYS_TIMER_CREATE, SYS_TIMER_POLL, SYS_TIMER_SET, SYS_USER_EXIT, SYS_WAIT, SYS_WAIT_ANY, SYS_WAITSET_ADD, SYS_WAITSET_CREATE, SYS_WAITSET_REMOVE, SYS_WAITSET_WAIT, SYS_YIELD};

// The sys_is_err helper is only consumed by the in-kernel test harness.
#[cfg(test)]
pub use abi::sys_is_err;

#[cfg(test)]
mod tests;

// Introspection record filled by object_info_get: the identity and type of the
// object behind a handle, and the access the handle confers. Defined in `abi` (the
// SSOT shared with userspace) and re-exported here next to its syscall.
pub use abi::ObjectInfo;

// Live per-process counters and state filled by process_stats_get. Defined in `abi`
// (the SSOT shared with userspace) and re-exported here next to its syscall.
pub use abi::ProcessStats;

// Live per-Domain resource counters filled by domain_stats_get. Defined in `abi`
// (the SSOT shared with userspace) and re-exported here next to its syscall.
pub use abi::DomainStats;

// The hardware-inventory records filled by cpu_info / memory_stats / memmap_get /
// irq_info / pci_info. Defined in `abi` (the SSOT shared with userspace) and
// re-exported here next to their syscalls.
pub use abi::{IrqInfo, MemmapRegion, MemoryStats, PciInfo};

// Does any value appear twice?
//
// A handle may be named ONCE in a transfer array. Naming it twice used to mint two
// capabilities from one - no race required, just a repeated number - because each was
// cloned independently and the close afterwards ran twice with the second failure
// discarded. Quadratic, over an array bounded by `MAX_MESSAGE_CAPS`.
pub(crate) fn has_repeat(values: &[u64]) -> bool {
	values.iter().enumerate().any(|(i, v)| values[..i].contains(v))
}

// A zeroed byte buffer of `len`, or None if the allocator will not give it.
//
// Every allocation sized from a userspace number goes through this. They were plain `vec!`
// macros, which abort the process on exhaustion instead of returning - so a syscall naming a
// large length answered with an allocation-error handler rather than an error code. The
// explicit ceilings in `abi` bound what may be asked for; this bounds what happens when the
// machine cannot supply even that.
fn try_zeroed_bytes(len: usize) -> Option<Vec<u8>> {
	let mut v: Vec<u8> = Vec::new();
	v.try_reserve_exact(len).ok()?;
	v.resize(len, 0);
	Some(v)
}

// The same for a u64 array.
fn try_zeroed_u64(len: usize) -> Option<Vec<u64>> {
	let mut v: Vec<u64> = Vec::new();
	v.try_reserve_exact(len).ok()?;
	v.resize(len, 0);
	Some(v)
}

// Validate a caller-supplied buffer. Always accepts kernel self-calls; for a
// ring-3 caller it requires the whole [ptr, ptr+len) range to lie in user space
// and every page it touches to be mapped in the active address space.
//
// WHAT THIS IS FOR HAS CHANGED, and saying so is the point of this note. It used to be the safety
// mechanism: a ring-3 caller could pass an in-bounds pointer to an unmapped page, the kernel read
// it, and the resulting ring-0 fault was fatal - on the SYS_DEBUG_WRITE path it struck while the
// serial TX lock was held, so the fault handler's own logging deadlocked on that lock and the
// machine hung. Every copy therefore had to be preceded by a check that could not be wrong.
//
// It could always be wrong, and that is the defect this milestone fixes rather than the one it
// describes: the check and the copy are two moments, and another thread in the same process can
// unmap the page in between. No amount of checking closes that.
//
// What makes a copy safe now is the copy: `arch::usercopy` marks every instruction that touches a
// user address in the exception table, so a fault resumes at a fixup and reports how far it got.
// This is an EARLY, CHEAP REFUSAL of obvious nonsense - a null pointer, a range that wraps, an
// address past the user half, a page mapped read-only where a write is coming. Refusing here costs
// a walk and answers the caller with a clean error instead of a short copy nobody asked how to
// interpret.
//
// Both are worth having. What was not worth having is two mechanisms whose comments disagreed about
// which one was load-bearing.
fn user_buf_ok(ptr: u64, len: u64) -> bool {
	if !arch::percpu::in_user_syscall() {
		return true;
	}
	if len == 0 {
		return true;
	}
	if ptr == 0 {
		return false;
	}
	let end = match ptr.checked_add(len) {
		Some(end) => end,
		None => return false,
	};
	if end > USER_VA_END {
		return false;
	}
	user_pages_ok(ptr, end, false)
}

// The same, for a buffer the kernel is about to WRITE. A page a ring-3 caller can only read is not
// a destination; before the exception table, accepting one meant the copy faulted in ring 0 and the
// kernel stopped. Now it means a short write, and this refuses it up front so the caller gets an
// error rather than a partial one - the same early-refusal argument as `user_buf_ok`.
fn user_buf_writable(ptr: u64, len: u64) -> bool {
	if !arch::percpu::in_user_syscall() {
		return true;
	}
	if len == 0 {
		return true;
	}
	if ptr == 0 {
		return false;
	}
	let Some(end) = ptr.checked_add(len) else {
		return false;
	};
	if end > USER_VA_END {
		return false;
	}
	user_pages_ok(ptr, end, true)
}

// Every page of `[ptr, end)` mapped, reachable by ring 3, and writable if `write`.
//
// The permission bits are the point. This used to ask only whether an address
// TRANSLATED, so a caller could hand the kernel a pointer into a page it cannot itself
// reach - a kernel-only page in its own address space - and the kernel would read or
// write it on the caller's behalf. Present-ness is not permission.
fn user_pages_ok(ptr: u64, end: u64, write: bool) -> bool {
	let mut page = ptr & !0xfff;
	let last = (end - 1) & !0xfff;
	loop {
		let Some(flags) = arch::paging::translate_flags(page) else {
			return false;
		};
		if flags & arch::paging::USER == 0 {
			return false;
		}
		if write && flags & arch::paging::WRITABLE == 0 {
			return false;
		}
		if page == last {
			return true;
		}
		page += 0x1000;
	}
}

// Upper bound on a single bulk SYS_DEBUG_WRITE, sized to the serial TX ring so one
// call never has to outrun the UART synchronously. A caller with more bytes chunks.
const DEBUG_WRITE_MAX: u64 = 16384;

// Write debug output to the serial port (and the kernel framebuffer console while it
// still owns the display). Two forms keyed on `len`: a single byte when `len` is 0
// (`arg` is the byte), or a bulk write when `len` > 0 (`arg` is a userspace pointer to
// `len` bytes). The bulk form flushes a buffer in one syscall instead of one per byte:
// the console service mirrors a screenful of output to serial, and the old per-byte
// path (one char format per byte, in a debug build) stalled that thread - and the gpu
// present queued behind it - for ~500 ms on a `help` listing.
fn sys_debug_write(arg: u64, len: u64) -> i64 {
	if len == 0 {
		crate::_print_byte(arg as u8);
		return 0;
	}
	if len > DEBUG_WRITE_MAX || !user_buf_ok(arg, len) {
		return ERR_INVALID;
	}
	// Copy the caller's bytes into a kernel buffer through the sanctioned SMAP
	// window, so the serial path below never touches user memory directly (it
	// holds the TX lock while it writes, so a fault there would deadlock).
	let bytes = match read_bytes(arg, len as usize) {
		Ok(bytes) => bytes,
		Err(error) => return error,
	};
	// Report how many bytes the transmit ring accepted: a caller pacing a mirror
	// backlog resumes from there on its next pass instead of losing the tail.
	crate::_print_bytes(&bytes) as i64
}

// Kernel virtual-address window for syscall-mapped MemoryObjects. It is global because the
// window itself is global: one kernel half, shared by every address space, so a range handed
// out here has to be unique across the whole system.
//
// The ring-3 counterpart is NOT here. Each user address space carries its own `VaPool` over
// `USER_MMAP_BASE..USER_VA_END` (see `AddressSpace::alloc_vrange`), because two address spaces
// handing out the same user virtual address are not sharing anything.
static KERNEL_VMAP: crate::sync::SpinLock<crate::mem::vapool::VaPool> = crate::sync::SpinLock::new(crate::mem::vapool::VaPool::new(KERNEL_MMAP_BASE, KERNEL_MMAP_BASE + MMAP_WINDOW));

// How far the kernel mmap window extends (nothing else is laid out above it, but
// an explicit bound turns a leak into a clean allocation failure, not a walk into
// unrelated address space).
// The kernel mmap window size. On riscv64 the Sv39 high half is only 256 GiB, so the
// 16 TiB x86/aarch64 window does not fit; use 64 GiB (KERNEL_MMAP_BASE sits at +128 GiB
// above the kernel base, so the window runs up to +192 GiB, within the high half).
#[cfg(not(target_arch = "riscv64"))]
const MMAP_WINDOW: u64 = 0x0000_1000_0000_0000;
#[cfg(target_arch = "riscv64")]
const MMAP_WINDOW: u64 = 0x0000_0010_0000_0000;

pub(crate) fn alloc_kernel_vrange(len: u64) -> u64 {
	KERNEL_VMAP.lock().alloc(len)
}

// Does the caller hold the named authority? The three console/display syscalls had nothing to
// check at all - knowing the syscall number was the whole qualification - so any process could
// take the display before DisplayService did, redirect the console's input sink, or type into a
// privileged console. Each now takes a handle to a `Privilege` of the matching kind, minted once
// at boot and delegated down the boot chain to the one component that should hold it.
//
// There is no ring-0 exemption. Nothing in the kernel calls these three, so exempting ring 0
// would buy nothing and would leave the check untested - the suite drives syscalls from kernel
// threads, and a gate every test walks around is a gate nobody has walked through.
fn holds_privilege(handle: u64, kind: PrivilegeKind) -> Result<(), i64> {
	let privilege = current_typed::<Privilege>(handle, ObjectType::Privilege, Rights::NONE)?;
	if privilege.kind() != kind { Err(ERR_ACCESS_DENIED) } else { Ok(()) }
}

// The address space a mapping made by this syscall belongs to: the caller's own for a
// ring-3 caller, the shared kernel space for a ring-0 one. Either way it wraps the page
// tables that are active right now, which is why the map paths can go on using
// `map_page`. What the space adds is the pool a user range came from and must go back to.
fn caller_address_space(user: bool, thread: &Arc<Thread>) -> Arc<crate::object::address_space::AddressSpace> {
	if user { thread.process().address_space().clone() } else { crate::object::address_space::AddressSpace::kernel() }
}

// Take a range out of the window the caller maps into, in one place so the choice of
// window and the choice of space cannot disagree.
fn alloc_vrange_in(space: &crate::object::address_space::AddressSpace, user: bool, len: u64) -> u64 {
	if user { space.alloc_vrange(len) } else { alloc_kernel_vrange(len) }
}

// Give the whole kernel mmap window its top-level page-table entries, so no later mapping into
// it creates one. Called at boot, before any address space exists to be copied from the kernel's.
//
// This window is what caught the problem: an address space made before the first object was
// mapped here lacked the entry, and the scheduler's guard refused to switch to it rather than
// let the machine triple-fault. The window is bounded (MMAP_WINDOW) precisely so it can be
// enumerated and reserved.
pub(crate) fn reserve_kernel_vmap() {
	crate::arch::paging::reserve_kernel_top_level(KERNEL_MMAP_BASE, MMAP_WINDOW);
}

// Return a previously handed-out range to its window's pool, picked by address.
// The unmap paths call this - the explicit unmap syscall and the object Drops
// that tear down a leftover mapping.
//
// `space` is the address space the range was mapped in, and is what makes a user range
// findable again now that the user pool is per-address-space. It is an Option because
// `DeviceMemory` can hold a mapping made before it learned to record one; a user range with
// no space to return it to is dropped rather than guessed at, which loses window space in
// that one address space and cannot corrupt another's.
pub(crate) fn free_vrange(space: Option<&crate::object::address_space::AddressSpace>, base: u64, len: u64) {
	if base >= KERNEL_MMAP_BASE {
		KERNEL_VMAP.lock().free(base, len);
	} else if base >= USER_MMAP_BASE {
		if let Some(space) = space {
			space.free_vrange(base, len);
		}
	}
}

// Map `count` consecutive pages into the active address space, rolling back every
// page mapped so far if an intermediate page table cannot be allocated. `phys_of`
// yields the physical frame for page `i`. Returns false only on out-of-frames,
// leaving nothing mapped - so a mid-map OOM leaves the object un-mapped and the
// caller can return ERR_NO_MEMORY and release its reserved virtual range.
fn map_pages_or_rollback(base: u64, count: usize, flags: u64, mut phys_of: impl FnMut(usize) -> u64) -> bool {
	for i in 0..count {
		if arch::paging::try_map_page(base + i as u64 * PAGE_SIZE, phys_of(i), flags).is_err() {
			for j in 0..i {
				arch::paging::unmap_page(base + j as u64 * PAGE_SIZE);
			}
			return false;
		}
	}
	true
}

// Fetch the current thread and look up a typed object handle on its table,
// releasing the table lock before returning. Collapses the boilerplate shared by
// the handlers that only need the looked-up object: a missing thread maps to
// ERR_NO_THREAD, denied rights to ERR_ACCESS_DENIED, and a bad handle or wrong
// type to ERR_BAD_HANDLE.
// The object behind a handle without asking what type it is, for callers that do not care.
fn untyped_object(handle: u64, rights: Rights) -> Result<Arc<dyn KernelObject>, i64> {
	let thread = sched::current_thread().ok_or(ERR_NO_THREAD)?;
	match thread.handles().lock().lookup(Handle::from_raw(handle), rights) {
		Ok(object) => Ok(object),
		Err(HandleError::AccessDenied) => Err(ERR_ACCESS_DENIED),
		Err(_) => Err(ERR_BAD_HANDLE),
	}
}

fn current_object(handle: u64, ty: ObjectType, rights: Rights) -> Result<Arc<dyn KernelObject>, i64> {
	let thread = sched::current_thread().ok_or(ERR_NO_THREAD)?;
	match thread.handles().lock().lookup_typed(Handle::from_raw(handle), ty, rights) {
		Ok(object) => Ok(object),
		Err(HandleError::AccessDenied) => Err(ERR_ACCESS_DENIED),
		Err(_) => Err(ERR_BAD_HANDLE),
	}
}

// Does the handle carry these rights, without resolving the object again?
//
// The mapping syscalls need a SECOND question about the same handle: `current_typed` has already
// established that it names the right kind of object and carries `MAP`, and what is left to ask is
// whether it also carries `WRITE` - because that, and not `MAP`, decides whether the pages may be
// written through. Answering false for an unknown handle is safe by construction: the caller has
// already failed if the handle does not resolve, so the only way to reach a `false` here is a
// handle that genuinely lacks the right.
fn handle_rights_allow(handle: u64, rights: Rights) -> bool {
	let Some(thread) = sched::current_thread() else { return false };
	let table = thread.handles().lock();
	table.rights_of(Handle::from_raw(handle)).is_ok_and(|held| held.contains(rights))
}

// Bind the calling thread or return ERR_NO_THREAD from the enclosing handler. The
// handlers that touch per-thread state (the handle table, the address space) open
// with this; a macro, not a function, because the early return must leave the
// handler itself, which returns the raw i64 syscall result.
macro_rules! current_thread {
	() => {
		match sched::current_thread() {
			Some(t) => t,
			None => return ERR_NO_THREAD,
		}
	};
}

// Install `object` into `thread`'s handle table with `rights`,
// returning the new handle's raw value, or ERR_RESOURCE_EXHAUSTED if the table (or
// the Domain's handle quota) is full. The shared tail of the create handlers.
fn install_object(thread: &Thread, object: Arc<dyn KernelObject>, rights: Rights) -> i64 {
	match thread.handles().lock().try_insert_object(object, rights) {
		Some(handle) => handle.raw() as i64,
		None => ERR_RESOURCE_EXHAUSTED,
	}
}

// Look up a typed object handle on the calling thread's table and recover the
// concrete `Arc<T>`, collapsing the `current_object` + downcast the handlers
// repeat. The downcast cannot fail because lookup_typed already checked the type.
fn current_typed<T: KernelObject>(handle: u64, ty: ObjectType, rights: Rights) -> Result<Arc<T>, i64> {
	Ok(current_object(handle, ty, rights)?.into_any_arc().downcast::<T>().ok().expect("type checked by lookup_typed"))
}

// Write `value` to a caller-supplied buffer through the sanctioned SMAP window
// (arch::paging::user_access): under SMAP a plain kernel store to a user page
// faults, so every copy-out goes through here. The caller has already validated
// the pointer with user_buf_ok.
fn read_user<T>(ptr: u64) -> T {
	let mut value = core::mem::MaybeUninit::<T>::uninit();
	// Through the faultable copy, so a page that goes away between `user_buf_ok` and here is a
	// short read rather than a kernel fault. A short read leaves the tail of `value` as whatever
	// the caller's buffer held, which is why the zero-fill below exists: a partially-read `T` must
	// not carry stack residue into a decision.
	let read = arch::paging::user_access(|| unsafe { arch::usercopy::copy_from_user(value.as_mut_ptr() as *mut u8, ptr, core::mem::size_of::<T>()) });
	if read < core::mem::size_of::<T>() {
		unsafe { core::ptr::write_bytes(value.as_mut_ptr() as *mut u8, 0, core::mem::size_of::<T>()) };
	}
	unsafe { value.assume_init() }
}

// Copy `len` bytes to userspace, ALL of them, or say which syscall error it is.
//
// The faultable copies report how far they got, and for most callers "how far" is not a useful
// answer - a syscall that filled 800 bytes of a 4096-byte result has not half-succeeded, it has
// failed. Every call site used to discard the count, so a short copy was indistinguishable from a
// complete one and the syscall returned success with the length it was ASKED for. This is the form
// those callers should have had.
//
// `ERR_NOT_MAPPED` rather than `ERR_INVALID`: the buffer was valid when it was checked, and what
// happened since is that the caller's own address space changed under it.
fn copy_to_user_exact(dst: u64, src: *const u8, len: usize) -> Result<(), i64> {
	if len == 0 {
		return Ok(());
	}
	let copied = arch::paging::user_access(|| unsafe { arch::usercopy::copy_to_user(dst, src, len) });
	if copied == len { Ok(()) } else { Err(ERR_NOT_MAPPED) }
}

// The same in the other direction: every byte, or an error.
fn copy_from_user_exact(dst: *mut u8, src: u64, len: usize) -> Result<(), i64> {
	if len == 0 {
		return Ok(());
	}
	let read = arch::paging::user_access(|| unsafe { arch::usercopy::copy_from_user(dst, src, len) });
	if read == len { Ok(()) } else { Err(ERR_NOT_MAPPED) }
}

// Write `value` at a userspace address, or say why not.
//
// There is deliberately NO infallible form. This used to be one - it returned `()`, and a page that
// went away between `user_buf_writable` and the store simply did not get written while the syscall
// reported success. `SYS_CHANNEL_CREATE` is the sharpest example: it installs two handles, writes
// their numbers out, and returned 0 whether or not the numbers arrived, so a caller could own two
// handles it had never been told the names of - unclosable, and a success it could not act on.
//
// `user_buf_writable` stays and its job has changed: it is no longer the safety mechanism but an
// early, cheap refusal of obvious nonsense - a null pointer, a range that wraps, an address in the
// kernel half. What makes the write SAFE is the faultable copy underneath it.
#[must_use]
fn write_user<T>(ptr: u64, value: T) -> Result<(), i64> {
	if !user_buf_writable(ptr, core::mem::size_of::<T>() as u64) {
		return Err(ERR_NOT_MAPPED);
	}
	let value = core::mem::ManuallyDrop::new(value);
	copy_to_user_exact(ptr, &value as *const core::mem::ManuallyDrop<T> as *const u8, core::mem::size_of::<T>())
}

// Entry point called by the architecture syscall stub. `num` selects the call;
// the meaning of the arguments and the return value is per-syscall.
#[unsafe(no_mangle)]
pub extern "C" fn syscall_dispatch(num: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> u64 {
	let result: i64 = match num {
		SYS_DEBUG_NOOP => a0 as i64,
		SYS_CLOCK_GET => arch::apic::ticks() as i64,
		SYS_CLOCK_RTC => arch::rtc::read_unix() as i64,
		SYS_CLOCK_MONO_NS => arch::tsc::cycles_to_ns(arch::tsc::now()) as i64,
		SYS_DEBUG_WRITE => sys_debug_write(a0, a1),
		SYS_MEMORY_OBJECT_CREATE => sys_memory_object_create(a0),
		SYS_DMA_BUFFER_CREATE => sys_dma_buffer_create(a0, a1),
		SYS_DEVICE_QUIESCED => sys_device_quiesced(a0),
		SYS_DMA_BUFFER_MAP => sys_dma_buffer_map(a0),
		SYS_DMA_BUFFER_UNMAP => sys_dma_buffer_unmap(a0),
		SYS_DMA_BUFFER_PHYS => sys_dma_buffer_phys(a0, a1),
		SYS_DEVICE_MEMORY_MAP => sys_device_memory_map(a0),
		SYS_RANDOM_GET => sys_random_get(a0, a1),
		SYS_RANDOM_INSECURE => sys_random_insecure(a0, a1),
		SYS_INTERRUPT_BIND => sys_interrupt_bind(a0, a1),
		SYS_DEVICE_MSIX_ACQUIRE => sys_device_msix_acquire(a0),
		SYS_INTERRUPT_ACK => sys_interrupt_ack(a0),
		SYS_SYSTEM_POWER => sys_system_power(a0, a1),
		SYS_CONSOLE_FEED => sys_console_feed(a0, a1, a2),
		SYS_FRAMEBUFFER_MAP => sys_framebuffer_map(a0, a1, a2),
		SYS_CONSOLE_READLOG => sys_console_readlog(a0, a1),
		SYS_OBJECT_PROPERTY_SET => sys_object_property_set(a0, a1, a2, a3),
		SYS_PROCESS_CREATE => sys_process_create(a0),
		SYS_PROCESS_LOAD => sys_process_load(a0, a1, a2),
		SYS_PROCESS_LOAD_MODULE => sys_process_load_module(a0, a1, a2, a3),
		SYS_BOOT_PROFILE => sys_boot_profile(a0, a1),
		SYS_BOOT_ID => sys_boot_id(),
		SYS_PROCESS_SIGNAL => sys_process_signal(a0, a1),
		SYS_PROCESS_GROUP_CREATE => sys_process_group_create(a0, a1),
		SYS_PROCESS_GROUP_SIGNAL => sys_process_group_signal(a0, a1),
		SYS_PROCESS_GROUP_STATS => sys_process_group_stats(a0, a1, a2),
		SYS_SIGNAL_CATCH => sys_signal_catch(a0),
		SYS_SIGNAL_TAKE => sys_signal_take(a0),
		SYS_THREAD_CREATE => sys_thread_create(a0, a1, a2, a3),
		SYS_THREAD_START => sys_thread_start(a0),
		SYS_CONSOLE_ATTACH => sys_console_attach(a0, a1),
		SYS_DEVICE_COUNT => device::count() as i64,
		SYS_DEVICE_INFO => sys_device_info(a0, a1, a2),
		// `SYS_DEVICE_ACQUIRE` IS RETIRED AND ITS NUMBER IS NOT REUSED. It minted a `DeviceMemory`
		// for anyone with the DeviceManager privilege who named an index, counted owners instead of
		// having one, and answered with nothing the caller could later release the device by.
		// `SYS_DEVICE_CLAIM` is what replaced it; a caller still issuing the old number gets
		// `ERR_BAD_SYSCALL`, which is what a call that no longer exists should say.
		SYS_DEVICE_CLAIM => sys_device_claim(a0, a1, a2),
		SYS_DEVICE_RELEASE => sys_device_release(a0),
		SYS_DEVICE_CLAIM_INFO => sys_device_claim_info(a0, a1, a2),
		SYS_MEMORY_MAP => sys_memory_map(a0),
		SYS_MEMORY_UNMAP => sys_memory_unmap(a0),
		SYS_HANDLE_DUPLICATE => sys_handle_duplicate(a0, a1),
		SYS_HANDLE_CLOSE => sys_handle_close(a0),
		SYS_CHANNEL_CREATE => sys_channel_create(a0, a1, a2),
		SYS_CHANNEL_SEND => sys_channel_send(a0, a1, a2, a3),
		SYS_CHANNEL_SEND_ATTENUATED => sys_channel_send_attenuated(a0, a1, a2, a3),
		SYS_CHANNEL_RECV => sys_channel_recv(a0, a1, a2, a3),
		SYS_CHANNEL_SEND_CAPS => sys_channel_send_caps(a0, a1, a2, a3),
		SYS_CHANNEL_RECV_CAPS => sys_channel_recv_caps(a0, a1, a2, a3),
		SYS_WAITSET_CREATE => sys_waitset_create(),
		SYS_WAITSET_ADD => sys_waitset_add(a0, a1),
		SYS_WAITSET_REMOVE => sys_waitset_remove(a0, a1),
		SYS_WAITSET_WAIT => sys_waitset_wait(a0, a1, a2),
		SYS_EVENT_CREATE => sys_event_create(),
		SYS_EVENT_SIGNAL => sys_event_signal(a0),
		SYS_EVENT_POLL => sys_event_poll(a0),
		SYS_TIMER_CREATE => sys_timer_create(),
		SYS_TIMER_SET => sys_timer_set(a0, a1),
		SYS_TIMER_POLL => sys_timer_poll(a0),
		SYS_USER_EXIT => {
			// The status the program is reporting, latched on its Process before the thread
			// leaves. This syscall took no argument at all and discarded a0, so a waiter could
			// see that a program had finished but never whether it succeeded.
			if let Some(thread) = sched::current_thread() {
				thread.process().set_exit_status(a0);
			}
			arch::usermode::exit_to_kernel()
		}
		SYS_FAULT_INFO_GET => sys_fault_info_get(a0, a1),
		SYS_DOMAIN_CREATE => sys_domain_create(a0, a1, a2),
		SYS_DOMAIN_KILL => sys_domain_kill(a0),
		SYS_DOMAIN_STATS_GET => sys_domain_stats_get(a0, a1, a2),
		SYS_CPU_INFO => sys_cpu_info(a0, a1),
		SYS_CPU_NAME => sys_cpu_name(a0, a1),
		SYS_MEMORY_STATS => sys_memory_stats(a0, a1),
		SYS_MEMMAP_GET => sys_memmap_get(a0, a1, a2),
		SYS_IRQ_INFO => sys_irq_info(a0, a1, a2),
		SYS_PCI_INFO => sys_pci_info(a0, a1, a2),
		SYS_CHANNEL_PEEK => sys_channel_peek(a0),
		SYS_ABI_CHECK => sys_abi_check(a0),
		SYS_YIELD => {
			sched::yield_now();
			0
		}
		SYS_OBJECT_INFO_GET => sys_object_info_get(a0, a1, a2),
		SYS_PROCESS_STATS_GET => sys_process_stats_get(a0, a1, a2),
		SYS_WAIT => sys_wait(a0, a1, a2),
		SYS_WAIT_ANY => sys_wait_any(a0, a1, a2, a3),
		_ => ERR_BAD_SYSCALL,
	};
	result as u64
}

// SYS_ABI_CHECK: a starting process reports the ABI revision it was built against
// (a0 = its abi::ABI_VERSION). The kernel accepts a match and refuses a mismatch, so a
// binary built against a different revision - a renumbered call, a grown struct - is
// stopped at startup with a typed error instead of running against a mismatched ABI and
// misbehaving. New syscalls only append and old ones never renumber, so this call and
// its number stay valid across every revision.
fn sys_abi_check(claimed: u64) -> i64 {
	if claimed == ABI_VERSION as u64 {
		return 0;
	}
	// SAY BOTH NUMBERS.
	//
	// The refusal is correct and, on its own, almost useless to whoever has to act on it: `rt` prints
	// "built against a different kernel ABI revision" and the caller is left to work out WHICH binary
	// and which two revisions. That cost hours - the answer turned out to be a stale artifact in the
	// image, and nothing in the message pointed at a build.
	crate::serial_println!("abi: refusing a caller built against revision {claimed}; this kernel is {ABI_VERSION}");
	ERR_ABI_MISMATCH
}

// Create a MemoryObject and install a handle to it in the caller's table.
fn sys_memory_object_create(size: u64) -> i64 {
	let thread = current_thread!();
	// Charge the physical memory to the caller's Domain at the create boundary.
	let object = match MemoryObject::create_in(thread.domain(), size as usize) {
		Ok(o) => o,
		Err(MemoryError::QuotaExceeded) => return ERR_RESOURCE_EXHAUSTED,
		Err(MemoryError::OutOfMemory) => return ERR_NO_MEMORY,
	};
	install_object(&thread, object, Rights::ALL)
}

// Create a DmaBuffer - pinned DMA memory charged to the caller's Domain DMA quota
// - and install a handle to it in the caller's table. A driver maps the buffer
// and hands its physical address to its device.
// Allocate pinned, physically contiguous DMA memory.
//
// `device_handle` names the DeviceMemory capability for the device this buffer's physical address
// is about to be handed to, or 0 for a buffer that is not going to a device (which is what every
// caller passed before this argument existed, and what they still get). Naming the device is what
// lets the kernel hold the frames back if this process dies still holding the buffer: see
// `DmaBuffer::mark_orphaned` and `SYS_DEVICE_QUIESCED`.
//
// A capability rather than an index, because the two ends of the rule have to be the same
// authority: the driver that names a device here is the one that will later claim to have stopped
// it, and both are things only the holder of that device's capability may say.
fn sys_dma_buffer_create(size: u64, device_handle: u64) -> i64 {
	let thread = current_thread!();
	// THE CLAIM COMES WITH THE DEVICE CAPABILITY, which is the point of putting it there: the caller
	// names a `DeviceMemory` it holds, and the kernel reads BOTH which device this buffer is for and
	// which binding of it - so ending that binding reaches this buffer without anything having to
	// remember it was created.
	let (device, claim): (Option<u32>, Option<abi::ClaimKey>) = if device_handle == 0 {
		(None, None)
	} else {
		match current_typed::<DeviceMemory>(device_handle, ObjectType::DeviceMemory, Rights::WRITE) {
			Ok(memory) => (memory.device_index(), memory.claim()),
			Err(e) => return e,
		}
	};
	// Under the lifecycle guard, so the buffer joins the creating process's DMA registry before a
	// teardown can take its snapshot. A buffer created after the orphan pass would never be marked,
	// and an unmarked buffer's frames go back into circulation while a device may still name them.
	let Some(guard) = thread.process().begin_extend() else {
		return ERR_INVALID;
	};
	let object = match DmaBuffer::create_for(thread.domain(), size as usize, device) {
		Ok(o) => o,
		Err(MemoryError::QuotaExceeded) => return ERR_RESOURCE_EXHAUSTED,
		Err(MemoryError::OutOfMemory) => return ERR_NO_MEMORY,
	};
	if !guard.record_dma_buffer(&object) {
		return ERR_NO_MEMORY;
	}
	// RECORDED AS DERIVED BEFORE THE HANDLE EXISTS, for the reason the MMIO capability is: a
	// capability the revocation cannot reach outlives the claim that justified it, so a table that
	// cannot grow is a refusal rather than a buffer nothing can revoke.
	if let Some(key) = claim {
		if !device::register_derived(key, alloc::sync::Arc::downgrade(&(object.clone() as alloc::sync::Arc<dyn KernelObject>))) {
			return ERR_RESOURCE_EXHAUSTED;
		}
	}
	install_object(&thread, object, Rights::ALL)
}

// Map a DmaBuffer into the caller's address space. One mapping per address space;
// the driver and a display server may map the same backing concurrently.
fn sys_dma_buffer_map(handle: u64) -> i64 {
	// Nothing new once the process has begun going away: its cleanup has already taken
	// its snapshot of what to unmap, and a mapping registered after that is one nothing
	// will ever collect.
	//
	// HELD ACROSS THE WHOLE OPERATION, not read at the top. Between the check and the record below
	// there is a reservation, a virtual-range allocation and a page-table walk, and the teardown
	// takes its snapshot in the middle of its own two steps - so a flag read here said nothing
	// about the record down there. The guard is refused once teardown has begun and teardown waits
	// for it to be released.
	let thread = current_thread!();
	let Some(guard) = thread.process().begin_extend() else {
		return ERR_INVALID;
	};
	let dma = match current_typed::<DmaBuffer>(handle, ObjectType::DmaBuffer, Rights::MAP) {
		Ok(o) => o,
		Err(e) => return e,
	};
	let user = arch::percpu::in_user_syscall();
	let space = caller_address_space(user, &thread);
	let cr3 = space.cr3();
	if !dma.reserve_mapping(cr3) {
		return ERR_INVALID;
	}
	let base = alloc_vrange_in(&space, user, dma.size() as u64);
	if base == 0 {
		dma.abandon_reservation(cr3);
		return ERR_NO_MEMORY;
	}
	// WRITABLE ONLY IF THE CAPABILITY CARRIES `WRITE`.
	//
	// Every mapping syscall checked `Rights::MAP` and then built a writable PTE unconditionally, so
	// a capability deliberately attenuated to `READ | MAP` produced a writable mapping - which is
	// the whole of what attenuation is for. The kernel hands out exactly such capabilities: the
	// bootstrap package and the ramdisk are passed read-only, and a holder could write to both.
	//
	// `MAP` says WHETHER the object may be mapped and `WRITE` says what may be done through it;
	// collapsing the two made `READ`, `WRITE` and `MAP` mean one thing for a direct operation and
	// another for a mapping.
	let writable = handle_rights_allow(handle, Rights::WRITE);
	let flags = arch::paging::PRESENT | arch::paging::NO_EXECUTE | if writable { arch::paging::WRITABLE } else { 0 } | if user { arch::paging::USER } else { 0 };
	let frames = dma.frames();
	if !map_pages_or_rollback(base, frames.len(), flags, |i| frames[i]) {
		free_vrange(Some(&space), base, dma.size() as u64);
		dma.abandon_reservation(cr3);
		return ERR_NO_MEMORY;
	}
	// A COMMIT THAT FINDS NO RESERVATION MUST UNDO THE MAP. Since `remove_mapping` no longer matches
	// a reservation this cannot happen any more; if it ever does, the page tables would hold a
	// mapping nothing records, which is precisely the state that lets teardown retire frames while
	// translations are live.
	if !dma.commit_mapping(cr3, base) {
		for i in 0..frames.len() {
			arch::paging::unmap_page(base + i as u64 * PAGE_SIZE);
		}
		free_vrange(Some(&space), base, dma.size() as u64);
		return ERR_NO_MEMORY;
	}
	// Rolled back the same way `sys_memory_map` does, and for the same reason.
	if !guard.record_dma_mapping(dma.clone()) {
		dma.remove_mapping(&space);
		return ERR_NO_MEMORY;
	}
	base as i64
}

fn sys_dma_buffer_unmap(handle: u64) -> i64 {
	let thread = current_thread!();
	let dma = match current_typed::<DmaBuffer>(handle, ObjectType::DmaBuffer, Rights::MAP) {
		Ok(dma) => dma,
		Err(error) => return error,
	};
	let space = caller_address_space(arch::percpu::in_user_syscall(), &thread);
	if !dma.remove_mapping(&space) {
		return ERR_NOT_MAPPED;
	}
	thread.process().forget_dma_mapping(&dma);
	0
}

// Return the physical address backing byte `offset` of a DmaBuffer - the address a
// driver programs into its device for DMA. A multi-page buffer is not physically
// contiguous (it is mapped contiguously in virtual space but its frames are
// scattered), so a driver that splits the buffer into device buffers spanning more
// than the first page must query each one's true physical address by its offset.
// Offset 0 returns the physical base.
fn sys_dma_buffer_phys(handle: u64, offset: u64) -> i64 {
	let dma = match current_typed::<DmaBuffer>(handle, ObjectType::DmaBuffer, Rights::READ) {
		Ok(o) => o,
		Err(e) => return e,
	};
	// A PHYSICAL ADDRESS IS FOR A DEVICE, so a buffer bound to none cannot have one.
	//
	// `sys_dma_buffer_create` accepts a zero device handle, and this did not look - so any process
	// that could make a DMA buffer could learn the physical address of its own pages, on a machine
	// with no IOMMU, for no device. Binding is what makes the address answerable.
	//
	// NOT "ONLY WHILE MAPPED", which is the narrowing that suggests itself and would break the
	// display: virtio-gpu deliberately does not map its framebuffer backing - ConsoleService renders
	// into it and a `DmaBuffer` maps only once - while the GPU still needs the addresses. Mapping is
	// not what makes an address legitimate; being for a device is.
	//
	// WHAT THIS DOES NOT DO, said plainly: an address is an integer and nothing takes one back.
	// Without an IOMMU a driver that has one keeps it after the buffer is gone. This closes who may
	// ask, not how long an answer lives.
	if dma.device().is_none() {
		return ERR_INVALID;
	}
	// A TRANSLATED BUFFER ANSWERS WITH ITS IOVA, and a physical address never leaves the kernel for
	// it. The two cases are deliberately the same syscall: a driver asks for the address its device
	// uses, and which kind of address that is depends on whether the device is behind an IOMMU -
	// which is the kernel's business rather than the driver's. Under an enforcing profile the answer
	// is revocable; under `trusted-untranslated` it is the raw integer it always was.
	if dma.is_translated() {
		let base = dma.device_address();
		if offset >= dma.size() as u64 {
			return ERR_INVALID;
		}
		return (base + offset) as i64;
	}
	let frames = dma.frames();
	let page = (offset / PAGE_SIZE) as usize;
	if page >= frames.len() {
		return ERR_INVALID;
	}
	(frames[page] + offset % PAGE_SIZE) as i64
}

// Map the boot framebuffer into the caller's address space and write its geometry
// into the caller's buffer, returning the mapped virtual base; the kernel console
// then stops drawing (the display belongs to the caller). Once-only: a second call
// (after the display is handed out) returns ERR_INVALID. Intended for a single
// userspace ConsoleService; capability-gating it is a later hardening.
fn sys_framebuffer_map(buf_ptr: u64, buf_len: u64, privilege: u64) -> i64 {
	// `privilege` names a DisplayController. Without it, the display went to whoever asked
	// first - and asking first is a race any process at boot could try to win, after which the
	// kernel console is off and the screen belongs to the winner.
	if let Err(error) = holds_privilege(privilege, PrivilegeKind::DisplayController) {
		return error;
	}
	let size = core::mem::size_of::<abi::Framebuffer>() as u64;
	if buf_len < size || !user_buf_ok(buf_ptr, size) {
		return ERR_INVALID;
	}
	// CLAIMED HERE, not checked here. `is_disabled()` followed by `disable()` at the very end left
	// the whole mapping between the check and the act, so two privileged callers could both pass and
	// both be handed the display. Every failure below releases the claim, because a caller that took
	// it and then could not finish must not leave the console dark and the display unowned.
	if !crate::console::try_claim() {
		return ERR_INVALID;
	}
	let (addr, geom) = match crate::framebuffer_geometry() {
		Some(t) => t,
		None => {
			crate::console::release_claim();
			return ERR_INVALID;
		}
	};
	let base_phys = match arch::paging::translate(addr) {
		Some(p) => p,
		None => {
			crate::console::release_claim();
			return ERR_INVALID;
		}
	};
	let total = geom.height as u64 * geom.pitch as u64;
	let pages = total.div_ceil(PAGE_SIZE);
	let user = arch::percpu::in_user_syscall();
	let thread = current_thread!();
	let space = caller_address_space(user, &thread);
	let base = alloc_vrange_in(&space, user, total);
	if base == 0 {
		crate::console::release_claim();
		return ERR_NO_MEMORY;
	}
	let mut flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::NO_EXECUTE;
	if user {
		flags |= arch::paging::USER;
	}
	if !map_pages_or_rollback(base, pages as usize, flags, |i| base_phys + i as u64 * PAGE_SIZE) {
		free_vrange(Some(&space), base, total);
		crate::console::release_claim();
		return ERR_NO_MEMORY;
	}
	if let Err(e) = write_user(buf_ptr, geom) {
		// The reply did not reach the caller, so the mapping must not stay: the caller is told the
		// call failed and would have no reason - or any way - to unmap a framebuffer it was never
		// given the address of. Everything this call built comes back out, in reverse.
		for i in 0..pages {
			arch::paging::unmap_page(base + i * PAGE_SIZE);
		}
		free_vrange(Some(&space), base, total);
		crate::console::release_claim();
		return e;
	}
	// The display is already claimed - `try_claim` above took it - so the console has stopped
	// drawing and there is nothing left to do but answer.
	base as i64
}

// Copy the kernel boot console's log text into the caller's buffer, returning the
// number of bytes written (0 when there is no boot console). The kernel and the
// userspace ConsoleService share the same `term` stack: at display takeover the
// ConsoleService reads the boot log as logical text and replays it into VT 1's
// model, so the boot log stays on screen with no second renderer. The grid model
// survives sys_framebuffer_map's disable(), so this is valid after takeover.
fn sys_console_readlog(buf_ptr: u64, buf_len: u64) -> i64 {
	if buf_len == 0 || !user_buf_ok(buf_ptr, buf_len) {
		return ERR_INVALID;
	}
	let text = match crate::console::boot_log_text() {
		Some(t) => t,
		None => return 0,
	};
	let n = (text.len() as u64).min(buf_len) as usize;
	if let Err(error) = copy_to_user_exact(buf_ptr, text.as_ptr(), n) {
		return error;
	}
	n as i64
}

// Copy the boot profile's name into the caller's buffer, returning the bytes written, or 0
// when this boot named none. A free syscall: the profile is public identity, the same fact
// the kernel prints at boot, and a component that must behave differently under it needs to
// be able to ask rather than infer.
// This boot's identity, drawn once and then constant for the life of the kernel.
//
// `OnceLock` rather than a boot-time initialiser: the id is wanted by the first userspace process
// and by anything that outlives it, and threading an init call through the boot path for a value
// that can be produced on demand is one more ordering to get wrong. Zero is reserved for "not this
// boot", so a draw that lands on zero is drawn again rather than handed out.
fn boot_id() -> u64 {
	static BOOT_ID: crate::sync::SpinLock<u64> = crate::sync::SpinLock::new(0);
	let mut id = BOOT_ID.lock();
	while *id == 0 {
		let mut bytes = [0u8; 8];
		arch::random::insecure(&mut bytes);
		*id = u64::from_le_bytes(bytes);
	}
	*id
}

// Read this boot's identity. Returns it directly rather than through a buffer: it is one word, and
// a syscall that cannot fail is one fewer error path at every call site.
fn sys_boot_id() -> i64 {
	// The top bit is cleared because the syscall ABI returns a signed value and a negative one is
	// an error code everywhere else in this table. An id is still 63 bits of distinctness, which is
	// more than a machine that reboots will ever exhaust.
	(boot_id() & 0x7fff_ffff_ffff_ffff) as i64
}

fn sys_boot_profile(buf_ptr: u64, buf_len: u64) -> i64 {
	let profile = match arch::boot_profile() {
		Some(profile) => profile,
		None => return 0,
	};
	if buf_len == 0 || !user_buf_ok(buf_ptr, buf_len) {
		return ERR_INVALID;
	}
	let n = profile.len().min(buf_len as usize);
	if let Err(error) = copy_to_user_exact(buf_ptr, profile.as_ptr(), n) {
		return error;
	}
	n as i64
}

// Copy the online CPU set's LAPIC ids (one u32 per core, as many as fit) into the
// caller's buffer, returning the core count. The topology is retained by smp at
// report-in; a free syscall - the CPU inventory is public identity, not a capability.
fn sys_cpu_info(buf_ptr: u64, buf_len: u64) -> i64 {
	let count = crate::smp::cpu_count();
	// EIGHT BYTES PER CORE, because that is the width of the id being reported: an SBI hart id is
	// an `unsigned long` and an MPIDR affinity is 40 bits. A four-byte element would truncate the
	// two architectures whose ids do not fit and hand the caller two cores with one id.
	let n = count.min((buf_len / 8) as usize);
	if n > 0 && !user_buf_ok(buf_ptr, n as u64 * 8) {
		return ERR_INVALID;
	}
	for cpu in 0..n {
		// A buffer that went away partway leaves the caller a list it cannot tell is short, so the
		// loop stops and says so rather than reporting the full core count over a half-written array.
		if let Err(e) = write_user((buf_ptr as *mut u64).wrapping_add(cpu) as u64, crate::smp::lapic_id(cpu)) {
			return e;
		}
	}
	count as i64
}

// Write the CPU's model / brand string into the caller's buffer (as many bytes as
// fit), returning the byte length. A free syscall - the CPU model is public
// identity - feeding the `lscpu` model field. The per-arch source is arch::cpu_brand
// (x86 CPUID brand string, aarch64 MIDR decode, riscv64 SBI vendor id).
fn sys_cpu_name(buf_ptr: u64, buf_len: u64) -> i64 {
	let mut brand: [u8; 64] = [0u8; 64];
	let len: usize = arch::cpu_brand(&mut brand);
	let n: usize = len.min(buf_len as usize);
	if n > 0 {
		if !user_buf_ok(buf_ptr, n as u64) {
			return ERR_INVALID;
		}
		for (i, &b) in brand[..n].iter().enumerate() {
			if let Err(e) = write_user((buf_ptr as *mut u8).wrapping_add(i) as u64, b) {
				return e;
			}
		}
	}
	n as i64
}

// Copy the physical-memory and kernel-heap totals into the caller's buffer (a
// MemoryStats): the frame pool's total (fixed at init) and free frame counts, and
// the heap's total and free bytes. A free syscall feeding the `free` command.
fn sys_memory_stats(buf_ptr: u64, buf_len: u64) -> i64 {
	let size = core::mem::size_of::<MemoryStats>() as u64;
	if buf_len < size || !user_buf_ok(buf_ptr, size) {
		return ERR_INVALID;
	}
	let (total_frames, free_frames) = crate::mem::frame::totals();
	let (heap_total, heap_free) = crate::mem::heap::stats();
	let out = MemoryStats { total_frames: total_frames as u64, free_frames: free_frames as u64, heap_total, heap_free };
	if let Err(e) = write_user(buf_ptr, out) {
		return e;
	}
	1
}

// Copy the boot memory-map region at `index` into the caller's buffer (a
// MemmapRegion), returning the region count - ERR_INVALID past the end, so a caller
// walks the retained map without knowing its size up front. A free syscall feeding
// the `lsmem` command.
fn sys_memmap_get(index: u64, buf_ptr: u64, buf_len: u64) -> i64 {
	let region = match crate::mem::memmap_get(index as usize) {
		Some(r) => r,
		None => return ERR_INVALID,
	};
	let size = core::mem::size_of::<MemmapRegion>() as u64;
	if buf_len < size || !user_buf_ok(buf_ptr, size) {
		return ERR_INVALID;
	}
	if let Err(e) = write_user(buf_ptr, region) {
		return e;
	}
	crate::mem::memmap_len() as i64
}

// Copy the device-interrupt vector state at `index` into the caller's buffer (an
// IrqInfo), returning the vector count - the fixed INTx window first, then the MSI-X
// window with each owned vector's device index. ERR_INVALID past the end. A free
// syscall feeding the `lsirq` command.
fn sys_irq_info(index: u64, buf_ptr: u64, buf_len: u64) -> i64 {
	let info = match arch::interrupts::irq_info(index as usize) {
		Some(i) => i,
		None => return ERR_INVALID,
	};
	let size = core::mem::size_of::<IrqInfo>() as u64;
	if buf_len < size || !user_buf_ok(buf_ptr, size) {
		return ERR_INVALID;
	}
	if let Err(e) = write_user(buf_ptr, info) {
		return e;
	}
	arch::interrupts::irq_info_len() as i64
}

// Copy the retained PCI function at `index` into the caller's buffer (a PciInfo),
// returning the function count - ERR_INVALID past the end. The kernel keeps the
// full boot bus scan, so every present function is reported, not just the ones
// drivers bind. A free syscall feeding the `lspci` command.
fn sys_pci_info(index: u64, buf_ptr: u64, buf_len: u64) -> i64 {
	let info = match device::pci_get(index as usize) {
		Some(p) => p,
		None => return ERR_INVALID,
	};
	let size = core::mem::size_of::<PciInfo>() as u64;
	if buf_len < size || !user_buf_ok(buf_ptr, size) {
		return ERR_INVALID;
	}
	if let Err(e) = write_user(buf_ptr, info) {
		return e;
	}
	device::pci_count() as i64
}

// Map a DeviceMemory's physical MMIO region into the caller's address space,
// uncacheable, and return its virtual base. A ring-3 caller maps into its own user
// space (USER bit); a ring-0 caller into the shared kernel window. One mapping per
// object: a second call returns ERR_INVALID.
fn sys_device_memory_map(handle: u64) -> i64 {
	let device = match current_typed::<DeviceMemory>(handle, ObjectType::DeviceMemory, Rights::MAP) {
		Ok(o) => o,
		Err(e) => return e,
	};
	// CLAIMED, not tested. Two threads both finding it unmapped and both mapping it is how a
	// mapping outlived the capability that authorised it.
	if !device.claim_mapping() {
		return ERR_INVALID;
	}
	let user = arch::percpu::in_user_syscall();
	let thread = current_thread!();
	let space = caller_address_space(user, &thread);
	let pages = device.pages();
	let len = pages as u64 * PAGE_SIZE;
	let base = alloc_vrange_in(&space, user, len);
	if base == 0 {
		device.release_claim();
		return ERR_NO_MEMORY;
	}
	let mut flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::NO_CACHE | arch::paging::NO_EXECUTE;
	if user {
		flags |= arch::paging::USER;
	}
	// From the ALIGNED physical base, and the caller gets its offset back. A BAR that does
	// not begin on a page boundary cannot be expressed in a PTE, so mapping from
	// `phys_base` silently dropped its low bits and returned an address pointing at the
	// wrong register.
	let phys_base = device.aligned_phys_base();
	if !map_pages_or_rollback(base, pages, flags, |i| phys_base + i as u64 * PAGE_SIZE) {
		free_vrange(Some(&space), base, len);
		device.release_claim();
		return ERR_NO_MEMORY;
	}
	device.set_mapped_in(base, space);
	(base + device.page_offset()) as i64
}

// Write the DeviceInfo for the virtio device at `index` (its type + MMIO struct
// offsets) into the caller's buffer. Returns 0 on success, ERR_INVALID for an
// out-of-range index or an undersized/bad buffer. The driver pairs this with a
// device_acquire'd DeviceMemory capability to reach the device.
fn sys_device_info(index: u64, buf_ptr: u64, buf_len: u64) -> i64 {
	let size = core::mem::size_of::<abi::DeviceInfo>() as u64;
	if buf_len < size || !user_buf_ok(buf_ptr, size) {
		return ERR_INVALID;
	}
	let info = device::with(index as usize, |d| abi::DeviceInfo { device_type: d.device_type as u32, bar_len: d.bar_len, common_offset: d.common_offset, notify_offset: d.notify_offset, notify_multiplier: d.notify_multiplier, isr_offset: d.isr_offset, device_offset: d.device_offset, device_len: d.device_len, bus: d.bus, dev: d.dev, func: d.func, class: d.class, subclass: d.subclass, prog_if: d.prog_if, _pad0: 0, _pad1: [0; 2] });
	match info {
		Some(info) => {
			if let Err(e) = write_user(buf_ptr, info) {
				return e;
			}
			0
		}
		None => ERR_INVALID,
	}
}

// TAKE THE DEVICE AT `index`, and answer with everything one binding needs.
//
// This replaces `SYS_DEVICE_ACQUIRE`, which minted a `DeviceMemory` and nothing else. Two callers
// naming one index both got one, because the kernel counted owners instead of having one; and the
// single handle it answered with was precisely the handle that gets sent on to the driver, so after
// a successful bind the manager held nothing about the claim at all - it could not learn which
// binding this was, read what state the device was in, or take the device back.
//
// The answer is an `abi::ClaimGrant` copied into `grant_ptr`: the key, the MMIO capability that
// travels to the driver, and the claim handle that stays here.
//
// ONE OPERATION OR NONE OF IT. Everything that can fail is listed in the order it is attempted, and
// every refusal past the claim itself releases the claim before returning - a partial success would
// leave a device nothing can release and nothing can rebind.
fn sys_device_claim(index: u64, privilege: u64, grant_ptr: u64) -> i64 {
	// `privilege` names a DeviceManager. Without it this minted a capability to any device's BAR for
	// any caller that named an index - see `PrivilegeKind::DeviceManager` for why that is worse than
	// it sounds on a machine with no IOMMU.
	if let Err(error) = holds_privilege(privilege, PrivilegeKind::DeviceManager) {
		return error;
	}
	let thread = current_thread!();
	let size = core::mem::size_of::<abi::ClaimGrant>() as u64;
	// THE CHEAP REFUSALS FIRST, before anything is taken: an unusable buffer is not a reason to
	// claim a device and then give it back.
	if !user_buf_ok(grant_ptr, size) {
		return ERR_INVALID;
	}
	// BOTH HANDLES ARE BOOKED BEFORE EITHER OBJECT EXISTS. `insert_reserved` cannot fail, which is
	// what makes "the second install fails after the first succeeded" - the case most likely to be
	// got half right - not a state this code can reach. What CAN fail is the booking, and it fails
	// before anything has moved.
	if !thread.handles().lock().reserve(2) {
		return ERR_RESOURCE_EXHAUSTED;
	}
	let Some((bar_phys, bar_len)) = device::with(index as usize, |d| (d.bar_phys, d.bar_len)) else {
		thread.handles().lock().release_reservation(2);
		return ERR_INVALID;
	};
	let key = match device::claim(index as usize) {
		Ok(key) => key,
		Err(error) => {
			thread.handles().lock().release_reservation(2);
			return claim_errno(error);
		}
	};
	// FROM HERE THE DEVICE IS TAKEN, so every refusal below gives it back.
	let Some(memory) = DeviceMemory::for_claim(key, bar_phys, bar_len as usize) else {
		return abandon_claim(&thread, key, ERR_RESOURCE_EXHAUSTED);
	};
	// RECORDED AS DERIVED BEFORE IT IS HANDED OUT. A capability the revocation cannot reach is
	// exactly what this milestone exists to make impossible, so a table that cannot grow is a failed
	// mint rather than a capability that outlives its claim.
	if !device::register_derived(key, alloc::sync::Arc::downgrade(&(memory.clone() as alloc::sync::Arc<dyn KernelObject>))) {
		return abandon_claim(&thread, key, ERR_RESOURCE_EXHAUSTED);
	}
	let Some(claim) = Claim::create(key) else {
		return abandon_claim(&thread, key, ERR_RESOURCE_EXHAUSTED);
	};
	// The MMIO capability keeps TRANSFER because DeviceManager is the one that hands it over, and it
	// arrives at the driver WITHOUT it through the attenuating send. Minting it without TRANSFER
	// outright would break the boot on the first try - the broker cannot move a capability it may
	// not move.
	let memory_handle = thread.handles().lock().insert_reserved(Capability::new(memory, Rights::READ | Rights::WRITE | Rights::MAP | Rights::TRANSFER));
	// AND THE CLAIM HANDLE CARRIES NEITHER TRANSFER NOR DUPLICATE. That is what makes it stay: a
	// claim handle that can be moved leaves this Domain, survives its killing, and holds the forced
	// release off exactly when the machine most needs it. WAIT, because the terminal result of a
	// release arrives on it; MANAGE, because ending the claim is what it is for.
	let claim_handle = thread.handles().lock().insert_reserved(Capability::new(claim, Rights::READ | Rights::WAIT | Rights::MANAGE));
	let grant = abi::ClaimGrant { key, memory: memory_handle.raw(), claim: claim_handle.raw() };
	if let Err(error) = write_user(grant_ptr, grant) {
		// THE CALLER NEVER LEARNED THE NAME OF ANY OF THIS, so none of it may survive. Closing the
		// claim handle is what releases the device - its `Drop` is the forced release - and the
		// order is the MMIO capability first so the claim's teardown finds nothing still holding the
		// device's registers.
		let mut table = thread.handles().lock();
		let _ = table.close(memory_handle);
		let _ = table.close(claim_handle);
		return error;
	}
	0
}

// Give a claim back and answer with `error`, for a refusal that happened after the device was taken.
//
// The reservation goes too: the two handles were booked and neither was installed.
fn abandon_claim(thread: &alloc::sync::Arc<Thread>, key: abi::ClaimKey, error: i64) -> i64 {
	let _ = device::release_claim(key);
	thread.handles().lock().release_reservation(2);
	error
}

// One errno per refusal, and they are distinct on purpose: a caller that cannot tell "somebody else
// has it" from "you passed nonsense" cannot retry correctly.
fn claim_errno(error: device::ClaimError) -> i64 {
	match error {
		// Nothing is there. Retrying will not make one appear.
		device::ClaimError::NoSuchDevice => ERR_INVALID,
		// Somebody has it, or is giving it back. WORTH WAITING ON, which is the whole reason this
		// is not the ERR_INVALID everything used to collapse into.
		device::ClaimError::AlreadyClaimed => abi::ERR_ALREADY_CLAIMED,
		// Not worth waiting on: both of these last the rest of the boot.
		device::ClaimError::Quarantined | device::ClaimError::Retired => ERR_UNSUPPORTED,
		// The DMA policy would not admit it, or the IOMMU would not confirm the attach. A policy
		// refusal rather than a malformed request, and the caller cannot fix it by asking again or
		// by asking for less.
		device::ClaimError::Refused => ERR_ACCESS_DENIED,
		// The key belongs to a previous binding of this device.
		device::ClaimError::Stale => ERR_INVALID,
	}
}

// END THE CLAIM THIS HANDLE NAMES, and answer with the state the device reached.
//
// THE KEY TRAVELS INSIDE THE OBJECT rather than through userspace, which is the same guarantee
// stated more strongly: a release "takes the whole key", and a key that cannot be supplied by the
// caller cannot be forged by it either. A claim whose device has since been released and re-claimed
// carries a generation that is no longer current, and `release_claim` refuses it rather than
// applying it to whoever holds the device now.
//
// Requires MANAGE on the claim handle. Answers with the `abi::CLAIM_STATE_*` code the device
// reached - `Free`, or `Quarantined` where the teardown could not be confirmed.
fn sys_device_release(claim_handle: u64) -> i64 {
	let claim = match current_typed::<Claim>(claim_handle, ObjectType::Claim, Rights::MANAGE) {
		Ok(c) => c,
		Err(e) => return e,
	};
	claim.release() as i64
}

// Read a claim handle: which binding it names, what state that device is in, and whether the
// release has settled.
//
// Reading is not enough on its own and is not meant to be - a manager built on one `wait_any` loop
// cannot spin on a status, which is why the claim handle is waitable. This is what it reads once the
// wait has woken it.
fn sys_device_claim_info(claim_handle: u64, buf_ptr: u64, buf_len: u64) -> i64 {
	let claim = match current_typed::<Claim>(claim_handle, ObjectType::Claim, Rights::READ) {
		Ok(c) => c,
		Err(e) => return e,
	};
	let size = core::mem::size_of::<abi::ClaimInfo>() as u64;
	if buf_len < size || !user_buf_ok(buf_ptr, size) {
		return ERR_INVALID;
	}
	let key = claim.key();
	// THE CLAIM'S OWN OUTCOME WINS OVER THE TABLE'S CURRENT STATE. Once this claim has settled, the
	// device may already have been claimed by somebody else - and answering `Claimed` for a binding
	// that ended would be this handle reporting on a binding that is not its own.
	let state = match claim.outcome() {
		Some(code) => code,
		None => device::claim_state(key.device_index as usize).map_or(abi::CLAIM_STATE_FREE, |state| state.code()),
	};
	let info = abi::ClaimInfo { key, state, settled: u32::from(claim.is_settled()) };
	if let Err(error) = write_user(buf_ptr, info) {
		return error;
	}
	0
}

// "I have stopped this device": release the DMA frames held for it.
//
// The caller must hold the device's own DeviceMemory capability with WRITE - the same authority the
// reset itself needed, because that is what this is a claim about. Returns the number of frames
// released, which is 0 in the ordinary case where no driver of this device ever died holding one.
//
// The kernel cannot check the claim, and could not on any machine without an IOMMU: it has no
// per-class knowledge of what "reset" means for a device it does not drive. What it can do is
// require the claim to come from the holder of the capability and to be made explicitly, so a
// driver that never resets its device never releases anything - a leak rather than a use after
// free, which is the direction this should lean.
fn sys_device_quiesced(device_handle: u64) -> i64 {
	let memory = match current_typed::<DeviceMemory>(device_handle, ObjectType::DeviceMemory, Rights::WRITE) {
		Ok(m) => m,
		Err(e) => return e,
	};
	// A CLAIM THAT IS NO LONGER CURRENT PROVES NOTHING ABOUT THIS DEVICE.
	//
	// The whole authority of this call is "the holder of the capability has just reset the hardware",
	// and a capability from a PREVIOUS binding is held by somebody who did no such thing to the
	// device as it is now. Without this, a `DeviceMemory` that outlived its claim - sitting in a
	// message queue, or in a process being torn down - could release the frames and vectors the
	// CURRENT driver is still using. Nothing forged it; it simply became a statement about a
	// different machine, and the generation is what tells the two apart.
	if let Some(key) = memory.claim() {
		if !device::claim_is_current(key) {
			return ERR_ACCESS_DENIED;
		}
	}
	match memory.device_index() {
		Some(index) => {
			// The DMA frames AND the MSI vectors. Both were held for the same reason - a request to
			// stop is not proof of stopping - and this is the one claim that answers it, so a
			// quiesce that released only half of them would leave the other half waiting forever.
			let vectors = crate::arch::interrupts::release_msi_for_device(index);
			if vectors != 0 {
				crate::serial_println!("irq: device {index} confirmed stopped - {vectors} masked MSI vector(s) released for re-use");
			}
			// The answer stays the FRAME count, which is what the syscall has always meant.
			crate::object::dma_buffer::release_for(index) as i64
		}
		// A bare MMIO window is not a device-table entry, so nothing is keyed on it.
		None => ERR_INVALID,
	}
}

// Fill a caller buffer with `len` random bytes from the kernel CSPRNG (RDRAND when
// available). Returns the number of bytes written, or ERR_INVALID for an
// out-of-range buffer.
fn sys_random_get(buf_ptr: u64, len: u64) -> i64 {
	random_into(buf_ptr, len, true)
}

// Random bytes that are NOT cryptographic, asked for by that name.
//
// The split is the fix. One syscall answered from a hardware source where there was one and from a
// clock-seeded formula where there was not, and userspace saw one answer either way - so anything
// deriving a key or a token from it was guessable on any machine without the hardware, with nothing
// to say so. And that is not a corner: two of this system's three architectures have no hardware
// source at all, so the formula was the ANSWER there rather than the fallback.
//
// What was wrong was never the formula. A boot identifier, a jitter, a hash seed all want exactly
// this and none of them wants an error instead. It was that the formula arrived under a name that
// promised otherwise, and a caller had no way to ask for one and be sure it had not got the other.
fn sys_random_insecure(buf_ptr: u64, len: u64) -> i64 {
	random_into(buf_ptr, len, false)
}

fn random_into(buf_ptr: u64, len: u64, must_be_secure: bool) -> i64 {
	if len == 0 {
		return 0;
	}
	if !user_buf_ok(buf_ptr, len) {
		return ERR_INVALID;
	}
	if must_be_secure && !arch::random::secure_available() {
		// No retry and no smaller request changes this, which is what `ERR_UNSUPPORTED` says and
		// `ERR_RESOURCE_EXHAUSTED` would not.
		return ERR_UNSUPPORTED;
	}
	// Generate into a kernel buffer in bounded chunks, then copy out to the caller.
	const CHUNK: usize = 256;
	let mut scratch = [0u8; CHUNK];
	let mut filled: u64 = 0;
	while filled < len {
		let n = ((len - filled) as usize).min(CHUNK);
		if must_be_secure {
			if !arch::random::secure(&mut scratch[..n]) {
				// The source stopped answering part-way. Refuse rather than finish the buffer from
				// somewhere else: a half-hardware key is a key nobody can reason about.
				return ERR_UNSUPPORTED;
			}
		} else {
			arch::random::insecure(&mut scratch[..n]);
		}
		// `filled` counted the chunk whether or not it landed, so a buffer that went away partway
		// left the caller told it had `len` bytes of randomness over memory holding whatever was
		// there before - the worst possible thing to be wrong about quietly.
		if let Err(error) = copy_to_user_exact(buf_ptr + filled, scratch.as_ptr(), n) {
			return error;
		}
		filled += n as u64;
	}
	filled as i64
}

// Bind a device IRQ vector to a new Interrupt object and install a handle to it in
// the caller's table. A driver waits on the handle; the kernel marks it pending
// and wakes the driver when the vector fires. ERR_INVALID for a non-bindable
// vector, ERR_RESOURCE_EXHAUSTED if the vector is already bound.
fn sys_interrupt_bind(vector: u64, privilege: u64) -> i64 {
	// And a legacy interrupt line, for the same reason.
	if let Err(error) = holds_privilege(privilege, PrivilegeKind::DeviceManager) {
		return error;
	}
	let thread = current_thread!();
	// The bound is the identifier's width, and the backend decides what is bindable inside it -
	// a byte was the x86 IDT's limit standing in for every architecture's (KERN-ARCH-017).
	if vector > u32::MAX as u64 || !arch::interrupts::is_bindable(vector as u32) {
		return ERR_INVALID;
	}
	let v = vector as u32;
	let Some(interrupt) = Interrupt::new(v) else { return ERR_RESOURCE_EXHAUSTED };
	if !arch::interrupts::bind(v, &interrupt) {
		return ERR_RESOURCE_EXHAUSTED;
	}
	// On a failed install the Interrupt is dropped here, and its Drop unbinds the
	// vector, so no explicit rollback is needed.
	install_object(&thread, interrupt, Rights::ALL)
}

// Acquire an MSI-X Interrupt for the discovered device at `index`: allocate a
// per-device LAPIC vector, program the device's MSI-X table entry 0 to deliver it to
// this CPU, enable MSI-X on the device, and mint an Interrupt bound to that vector.
// Unlike the INTx path the device's legacy pin stays disabled (MSI-X replaces it), so
// the driver gets its own edge-triggered vector with no INTx sharing. ERR_INVALID for
// an out-of-range index or a device with no MSI-X capability.
fn sys_device_msix_acquire(claim_handle: u64) -> i64 {
	// THE CLAIM IS THE AUTHORITY, AND IT NAMES THE DEVICE.
	//
	// This took an index plus an ambient DeviceManager privilege, so nothing anywhere said WHICH
	// BINDING the vector belonged to - and a revocation had no way to ask. Holding the claim answers
	// both questions at once and is strictly stronger than the privilege was: the privilege said
	// "you are a device manager", and this says "you are the one who took THIS device".
	let thread = current_thread!();
	let claim = match current_typed::<Claim>(claim_handle, ObjectType::Claim, Rights::MANAGE) {
		Ok(c) => c,
		Err(e) => return e,
	};
	// A BINDING THAT HAS ENDED DERIVES NOTHING. The claim handle outlives the claim - it settles
	// rather than disappearing - and minting a vector from a settled one would attach live interrupt
	// authority to a device somebody else may already hold.
	if claim.is_settled() {
		return ERR_ACCESS_DENIED;
	}
	let key = claim.key();
	let index = key.device_index as u64;
	let (cap, table_phys, bus, dev, func) = match device::with(index as usize, |d| (d.msix_cap, d.msix_table_phys, d.bus, d.dev, d.func)) {
		Some((cap, table_phys, bus, dev, func)) if cap != 0 => (cap, table_phys, bus, dev, func),
		_ => return ERR_INVALID,
	};
	// Our MSI message address encodes an 8-bit xAPIC destination. If the running
	// core's LAPIC id does not fit (a >255-core machine), steer the vector to the
	// first core with an addressable id instead of truncating silently; x2APIC
	// delivery, which would lift the limit, is not implemented yet.
	let lapic = arch::percpu::this_cpu().lapic_id();
	let dest = if lapic <= u8::MAX as u64 {
		lapic as u8
	} else {
		match (0..crate::smp::cpu_count()).map(crate::smp::lapic_id).find(|&id| id <= u8::MAX as u64) {
			Some(id) => id as u8,
			None => return ERR_RESOURCE_EXHAUSTED,
		}
	};
	// ONE LIVE VECTOR PER DEVICE, refused rather than discovered by aliasing.
	//
	// Every backend programs the device's MSI-X table ENTRY 0 for whatever slot it was given, so two
	// live acquisitions for one device produce two vectors, two registry slots and two `Interrupt`
	// objects all pointing at one hardware entry - which then carries the second vector. The first
	// `Interrupt` is bound to a vector the device will never raise, and when it drops, `unbind` masks
	// and unmaps entry 0, which is the entry the SECOND interrupt is live on. An old handle silently
	// disabling a new one.
	//
	// ASKED AND ANSWERED IN ONE OPERATION, by `acquire_msi_unique` below.
	//
	// This was a `device_has_live_msi` here followed by an `acquire` further down, with the reasoning
	// that the policy belongs at the syscall and the mechanism should stay callable by the kernel's
	// own xHCI bring-up test - which stands in for DeviceManager on a device the booted system has
	// already claimed. The split of RESPONSIBILITY was right and the split into two operations was
	// not: two CPUs could both read `false` and then both claim a slot, and the device's entry 0
	// would end up holding the second while the first `Interrupt` is bound to a vector that will
	// never fire - and masks entry 0 when it drops. So the mechanism gained a form that does both at
	// once, `acquire_msi` stays beside it for the bring-up test, and this syscall still owns what to
	// do when the answer is no.
	//
	// A PENDING slot does not count: its `Interrupt` is gone, and a restarting driver reprogramming
	// its own device's entry 0 is exactly what should happen. `ERR_RESOURCE_EXHAUSTED` because that
	// is what the caller can act on - the device's vector is spoken for.
	// THE HANDLE SLOT IS BOOKED BEFORE THE HARDWARE IS TOUCHED.
	//
	// This programmed the device's table, bound the `Interrupt`, enabled MSI-X and only then tried
	// to install the handle - which fails if the caller's table is full or its Domain is at quota.
	// The `Interrupt` then dropped, `unbind` masked the entry and the slot went PENDING, waiting for
	// a `SYS_DEVICE_QUIESCED` that will never come for a driver that was never created. MSI slots
	// are a fixed table and each failed acquire cost one, repeatably.
	//
	// Reserving first is what `HandleTable::reserve`/`insert_reserved` exist for, and the receive
	// path already uses them this way: book the room, touch the hardware, install against the
	// booking - which cannot fail - and give the booking back on any earlier refusal.
	if !thread.handles().lock().reserve(1) {
		return ERR_RESOURCE_EXHAUSTED;
	}
	let vector = match arch::interrupts::acquire_msi_unique(table_phys, dest, index as u32) {
		Some(v) => v,
		None => {
			thread.handles().lock().release_reservation(1);
			return ERR_RESOURCE_EXHAUSTED;
		}
	};
	let Some(interrupt) = Interrupt::new(vector) else {
		// The vector is ours and the heap is not. Given back the same way a failed bind gives it
		// back: MSI-X is not enabled yet, so nothing can have been sent.
		arch::interrupts::release_unused_msi(vector);
		thread.handles().lock().release_reservation(1);
		return ERR_RESOURCE_EXHAUSTED;
	};
	if !arch::interrupts::bind_msi(vector, &interrupt) {
		// The vector raced to another binder. FREED, not retired: MSI-X has not been enabled on the
		// device yet, so nothing can have been sent and there is no owner to quiesce it.
		arch::interrupts::release_unused_msi(vector);
		thread.handles().lock().release_reservation(1);
		return ERR_RESOURCE_EXHAUSTED;
	}
	// RECORDED AS DERIVED BEFORE THE DEVICE IS ALLOWED TO RAISE IT. A vector the revocation cannot
	// reach keeps delivering to a driver whose binding has ended, which is the interrupt half of the
	// property this milestone is about; a table that cannot grow gives the vector back rather than
	// arming one nothing can take away.
	if !device::register_derived(key, alloc::sync::Arc::downgrade(&(interrupt.clone() as alloc::sync::Arc<dyn KernelObject>))) {
		arch::interrupts::release_unused_msi(vector);
		thread.handles().lock().release_reservation(1);
		return ERR_RESOURCE_EXHAUSTED;
	}
	// Turn on MSI-X now that its table entry is programmed; the device's INTx pin stays
	// disabled (MSI-X is its interrupt source from here on).
	arch::pci::msix_enable(bus, dev, func, cap);
	thread.handles().lock().insert_reserved(Capability::new(interrupt, Rights::ALL)).raw() as i64
}

// Acknowledge a serviced interrupt: clear the Interrupt's pending flag so the driver's
// next `wait` blocks until the device interrupts again, then run the arch end-of-
// interrupt. MSI-X (x86/aarch64) is edge-triggered, so eoi is a no-op there; on riscv a
// wired INTx routed through the PLIC is level-triggered, so eoi completes the PLIC
// source (the driver has already deasserted its device line). Requires the WRITE right.
fn sys_interrupt_ack(handle: u64) -> i64 {
	let interrupt = match current_typed::<Interrupt>(handle, ObjectType::Interrupt, Rights::WRITE) {
		Ok(i) => i,
		Err(e) => return e,
	};
	interrupt.clear();
	arch::interrupts::eoi(interrupt.vector());
	0
}

// Reboot or power the machine off (action = POWER_REBOOT | POWER_OFF), for a caller holding
// MANAGE on the ROOT Domain. Diverges on a valid action; ERR_ACCESS_DENIED without the
// capability, ERR_INVALID on an unknown action.
//
// This was ungated - any ring-3 process could halt the machine with one syscall, a sandboxed
// component launched through PermissionManager included, in a kernel where reading one
// Domain's counters needs a handle carrying READ. The comment here used to say that
// restricting it was "a PermissionManager concern, deferred", which is a decision that
// stopped being one the moment it lived only in a comment.
//
// The root Domain is the right key and not an arbitrary one: whoever holds MANAGE on it can
// already `sys_domain_kill` the whole system, so being able to power it off as well is no new
// authority. Any other Domain would be an escalation - killing the apps Domain is not the
// same as stopping the machine - which is why the handle is compared against the root rather
// than merely required to be some Domain.
fn sys_system_power(handle: u64, action: u64) -> i64 {
	let domain = match current_typed::<Domain>(handle, ObjectType::Domain, Rights::MANAGE) {
		Ok(d) => d,
		Err(e) => return e,
	};
	if !Arc::ptr_eq(&domain, &sched::root_domain()) {
		return ERR_ACCESS_DENIED;
	}
	match action {
		abi::POWER_REBOOT => arch::reset(),
		abi::POWER_OFF => arch::poweroff(),
		_ => ERR_INVALID,
	}
}

// Inject one byte into the kernel console input - the path a userspace input driver
// (the virtio-input keyboard) uses to feed the interactive shell. (Gating this to the
// input driver is a PermissionManager concern, deferred.)
//
// `serial` chooses which of the two arrival paths the byte imitates. Zero is a
// keystroke, which the console service drops when its display is not focused. Non-zero
// is serial input, which it accepts regardless - the path a driven guest needs, because
// a scenario runner has no display to focus and must still be able to type.
//
// Returns 0 when the console took the byte and ERR_WOULD_BLOCK when it did not, which is
// either a full input queue or no console service attached at all. The answer was always
// computed here and always discarded; a caller driving the guest has to know whether what
// it sent arrived.
fn sys_console_feed(byte: u64, serial: u64, privilege: u64) -> i64 {
	// `privilege` names a ConsoleInputSource. Without it any process could type into a
	// privileged console - shell commands, a password, a confirmation - as if a person had.
	if let Err(error) = holds_privilege(privilege, PrivilegeKind::ConsoleInputSource) {
		return error;
	}
	let accepted = if serial != 0 { crate::console_input::feed_serial(byte as u8) } else { crate::console_input::feed(byte as u8) };
	if accepted { 0 } else { ERR_WOULD_BLOCK }
}

// Set a property on an object: a human-readable name (PROP_NAME; a2 = name
// pointer, a3 = name length, max 64 bytes UTF-8), or a Domain resource-counter
// limit (PROP_*_LIMIT; a2 = the new limit). Both require the MANAGE right on the
// handle; limit properties require the handle to name a Domain.
fn sys_object_property_set(handle: u64, prop: u64, a2: u64, a3: u64) -> i64 {
	let thread = current_thread!();
	if prop == PROP_NAME {
		const MAX_NAME: u64 = 64;
		let (ptr, len) = (a2, a3);
		if len == 0 || len > MAX_NAME || !user_buf_ok(ptr, len) {
			return ERR_INVALID;
		}
		let object = {
			let table = thread.handles().lock();
			match table.lookup(Handle::from_raw(handle), Rights::MANAGE) {
				Ok(o) => o,
				Err(HandleError::AccessDenied) => return ERR_ACCESS_DENIED,
				Err(_) => return ERR_BAD_HANDLE,
			}
		};
		let mut buf = [0u8; MAX_NAME as usize];
		// A short read would leave zeros in the tail and the name would silently be a different
		// name - one the caller never asked to set.
		if let Err(error) = copy_from_user_exact(buf.as_mut_ptr(), ptr, len as usize) {
			return error;
		}
		let name = match core::str::from_utf8(&buf[..len as usize]) {
			Ok(s) => s,
			Err(_) => return ERR_INVALID,
		};
		object.header().set_name(name);
		return 0;
	}
	// The remaining properties set a Domain resource limit.
	let domain = match current_typed::<Domain>(handle, ObjectType::Domain, Rights::MANAGE) {
		Ok(o) => o,
		Err(e) => return e,
	};
	let counter = match prop {
		PROP_MEMORY_LIMIT => domain.account().memory(),
		PROP_HANDLE_LIMIT => domain.account().handles(),
		PROP_THREAD_LIMIT => domain.account().threads(),
		PROP_DMA_LIMIT => domain.account().dma(),
		PROP_IPC_QUEUE_LIMIT => domain.account().ipc_queue(),
		PROP_STACK_LIMIT => domain.account().stack(),
		_ => return ERR_INVALID,
	};
	counter.set_limit(a2);
	0
}

// Create an empty process with its own address space and install a handle to it in
// the caller's table. The process is accounted to a Domain: `domain_handle` of 0
// means the caller's own Domain, otherwise it names a Domain the caller may spawn
// into (it must carry the MANAGE right), so a manager can launch a governed
// component under a bounded sub-Domain it controls. The process has no threads until
// process_load gives it an image and thread_create / thread_start give it a running
// thread.
fn sys_process_create(domain_handle: u64) -> i64 {
	let thread = current_thread!();
	let domain = if domain_handle == 0 {
		thread.domain().clone()
	} else {
		match current_typed::<Domain>(domain_handle, ObjectType::Domain, Rights::MANAGE) {
			Ok(d) => d,
			Err(e) => return e,
		}
	};
	let process = match sched::process_create(domain) {
		Some(p) => p,
		None => return ERR_NO_MEMORY,
	};
	install_object(&thread, process, Rights::ALL)
}

// Load an ELF image into a process created by process_create and return its entry
// point. The image bytes are read from the caller's address space at [elf_ptr,
// elf_ptr + elf_len) - a userspace spawner first brings them in via
// memory_object_create + memory_map. The kernel maps the program and a ring-3
// stack into the target process. Requires the MANAGE right on the process handle.
fn sys_process_load(process_handle: u64, elf_ptr: u64, elf_len: u64) -> i64 {
	if elf_len == 0 || elf_len as usize > abi::MAX_ELF_BYTES || !user_buf_ok(elf_ptr, elf_len) {
		return ERR_INVALID;
	}
	let process = match current_typed::<Process>(process_handle, ObjectType::Process, Rights::MANAGE) {
		Ok(o) => o,
		Err(e) => return e,
	};
	// BUFFERED, and this is the milestone's own defect closed.
	//
	// It used to read the image in place: `from_raw_parts(elf_ptr, elf_len)` and the whole loader
	// running inside a `user_access` window. The reasoning was that the loader copies only the
	// PT_LOAD segments, so buffering the whole ELF was a cost with no return - and that is true
	// right up until another thread in the caller's process unmaps the buffer mid-load. Then the
	// kernel takes a page fault in ring 0 at an instruction inside the ELF parser, which is not in
	// `.extable` and cannot be, because it is ORDINARY CODE reading a slice. The machine halts.
	//
	// The exception table was built for exactly this class and this path bypassed it. It needs no
	// privilege: a process creates a child in its own Domain, holds MANAGE on it, calls load on a
	// large image and unmaps the buffer from a second thread.
	//
	// One copy, bounded by `MAX_ELF_BYTES` and fallible, is what makes the loader read memory
	// userspace cannot take away. It costs a copy of the file per spawn - about a megabyte for the
	// programs this system actually has - against a spawn that already allocates and fills a frame
	// per loaded page. If images ever reach the tens of megabytes the ceiling allows, the answer is
	// to take a MemoryObject handle instead of a pointer, so the kernel holds a reference to the
	// backing and reads its frames directly; that is an ABI change, and it is not worth making for
	// a size this system does not have.
	let Some(mut image) = try_zeroed_bytes(elf_len as usize) else {
		return ERR_NO_MEMORY;
	};
	if let Err(error) = copy_from_user_exact(image.as_mut_ptr(), elf_ptr, elf_len as usize) {
		return error;
	}
	match loader::load_image_into(&process, &image) {
		Ok(entry) => entry as i64,
		Err(LoadError::OutOfMemory) => ERR_NO_MEMORY,
		Err(LoadError::BadImage | LoadError::Terminating) => ERR_INVALID,
	}
}

fn sys_process_load_module(process_handle: u64, elf_ptr: u64, elf_len: u64, bias: u64) -> i64 {
	if elf_len == 0 || elf_len as usize > abi::MAX_ELF_BYTES || !user_buf_ok(elf_ptr, elf_len) {
		return ERR_INVALID;
	}
	let process = match current_typed::<Process>(process_handle, ObjectType::Process, Rights::MANAGE) {
		Ok(process) => process,
		Err(error) => return error,
	};
	// Buffered for the reason `sys_process_load` is, and it is the same defect: a module image read
	// in place is read by ordinary loader code that no exception-table entry can rescue.
	let Some(mut image) = try_zeroed_bytes(elf_len as usize) else {
		return ERR_NO_MEMORY;
	};
	if let Err(error) = copy_from_user_exact(image.as_mut_ptr(), elf_ptr, elf_len as usize) {
		return error;
	}
	match loader::load_module_into(&process, &image, bias) {
		Ok(()) => 0,
		Err(LoadError::OutOfMemory) => ERR_NO_MEMORY,
		Err(LoadError::BadImage | LoadError::Terminating) => ERR_INVALID,
	}
}

// Create a ring-3 entry thread in `process_handle`, suspended (not yet running),
// at `entry` on the stack topped at `stack_top`, and install a handle to it in the
// caller's table. If `bootstrap_handle` is non-zero, the capability it names is
// moved out of the caller's table into the child's and delivered to the child's
// thread in rdi - the way a process is endowed with its initial capability.
// Requires the MANAGE right on the process handle (and TRANSFER on the bootstrap).
fn sys_thread_create(process_handle: u64, entry: u64, stack_top: u64, bootstrap_handle: u64) -> i64 {
	// WHERE THE THREAD WILL START, CHECKED BEFORE IT EXISTS (KERN-ARCH-005).
	//
	// `entry` and `stack_top` were stored as given and reached the ring transition unexamined. The
	// kernel builds an IRET frame from them and executes `iretq` at CPL0: a NONCANONICAL RIP or RSP
	// raises #GP or #SS *before* the privilege transition completes, so the handler sees a
	// kernel-origin fault and halts the machine. That is a global failure any process could cause
	// by asking for a thread at a bad address.
	//
	// `USER_VA_END` is the exclusive top of the low canonical half on every architecture here, so
	// one comparison answers both halves of it: an address below it is canonical by construction
	// and is not in the kernel's half. A stack TOP may equal it - a stack grows down from its top
	// and the top itself is never dereferenced - which is why the two bounds differ by one
	// comparison.
	//
	// The rest of what an address could be wrong about - unmapped, no-execute, supervisor-only,
	// read-only - stays a RECOVERABLE ring-3 page fault on the first fetch or push, which kills the
	// child and nothing else. Those are already right; this is the class that was not.
	if entry >= crate::memlayout::USER_VA_END || stack_top > crate::memlayout::USER_VA_END {
		return ERR_INVALID;
	}
	// AND ALIGNED, so a bad start fails here rather than somewhere inside the child. Sixteen is the
	// SysV stack alignment every ABI in this tree uses; `USER_STACK_TOP` and every page-aligned
	// stack a test hands over already satisfy it.
	if stack_top % 16 != 0 {
		return ERR_INVALID;
	}
	let thread = current_thread!();
	let process = match current_typed::<Process>(process_handle, ObjectType::Process, Rights::MANAGE) {
		Ok(o) => o,
		Err(e) => return e,
	};
	// THE TARGET'S GUARD, NOT A READ OF ITS FLAG, and held to the end of the transaction.
	//
	// This was `if process.is_terminating() { return ERR_INVALID; }`. A flag read answers a question
	// about a moment that has passed by the time the answer is used, and what follows is a
	// four-step transaction on the target: take from the caller, insert into the child, build the
	// thread, install the handle, commit the take. The guard that makes the answer hold was taken
	// three calls later inside `Thread::build`, by which time the capability is already in the
	// child.
	//
	// The window was exact and what it left behind was permanent. The child's bootstrap capability
	// is inserted ORDINARILY, so a `terminate()` racing in here reached `close_all`, which skips
	// reserved slots and took this one. `create_user_thread` then failed - `begin_extend` refuses on
	// a terminating process - and the rollback's `take` out of the child returned `Err`, so
	// `restore_taken` never ran. The caller was left with a slot that is `reserved: true` and
	// `cap: None`: not on the free list, never committed, skipped by `close_all` forever, and one
	// unit of handle quota short. A supervisor racing spawns against kills loses one of each per
	// attempt until it cannot spawn at all.
	//
	// One guard over the whole transaction closes it: the target cannot begin terminating while it
	// is held, so every rollback below can still reach the capability it needs to put back.
	// `Thread::build` takes its own - guards nest by counting - so this one does not have to be
	// threaded down.
	let Some(extend) = process.begin_extend() else {
		return ERR_INVALID;
	};
	// Move the bootstrap capability (if any) into the child, recording the handle
	// value the child will see, so the kernel can wire it into the thread's rdi.
	// TAKE the bootstrap - a transfer moves the capability, and the caller's handle dies
	// with the take rather than being closed afterwards by a call whose failure was
	// discarded.
	// TAKEN FOR TRANSFER, so the caller's handle value survives every way this can fail.
	//
	// It used `take`, which kills the value as it takes the capability, and `put_back`, which
	// reissues it under a handle the caller is never told - so a rollback left the capability alive
	// in the caller's table and unreachable by it. And when the child's `try_insert` refused on
	// quota, the capability was not put back at all: it was dropped where it stood, and neither
	// party had it.
	//
	// The batch send learned this and this call did not. Same primitive, same three outcomes.
	let bootstrap = Handle::from_raw(bootstrap_handle);
	let child_bootstrap = if bootstrap_handle != 0 {
		let cap = match thread.handles().lock().take_for_transfer(bootstrap, Rights::TRANSFER) {
			Ok(cap) => cap,
			Err(HandleError::AccessDenied) => return ERR_ACCESS_DENIED,
			Err(_) => return ERR_BAD_HANDLE,
		};
		let inserted = process.handles().lock().try_insert_or_return(cap);
		match inserted {
			Ok(handle) => handle.raw(),
			Err(cap) => {
				// The child would not take it, so it goes back where it came from - at the same
				// handle value, which is what makes the refusal cost the caller nothing.
				thread.handles().lock().restore_taken(bootstrap, cap);
				return ERR_RESOURCE_EXHAUSTED;
			}
		}
		// Every path from here on either commits the take or restores it, and the two rollbacks
		// below can no longer fail to find the capability: the guard above stops the target
		// terminating, so nothing else can take it out of the child. `abandon_taken` is what answers
		// for the case that is still theoretically reachable - see the rollbacks.
	} else {
		0
	};
	// And if the THREAD cannot be made, the capability goes back where it came from.
	//
	// It used to be left in the child: moved in, closed in the caller, and only then was
	// the thread created - so a thread quota or a stack allocation that failed left the
	// capability sitting in a process with no thread to receive its handle, and gone from
	// the caller that owned it. Neither party had it and nothing said so.
	let new_thread = match loader::create_user_thread(&process, entry, stack_top, child_bootstrap) {
		Some(t) => t,
		None => {
			// The thread could not be made: take the capability back out of the child and return it
			// to the handle the caller named it by. `put_back` reissued it under a new value the
			// caller was never told, which is the same unreachable-capability defect the batch send
			// had.
			if child_bootstrap != 0 {
				match process.handles().lock().take(Handle::from_raw(child_bootstrap), Rights::NONE) {
					Ok(cap) => thread.handles().lock().restore_taken(bootstrap, cap),
					// The capability is not in the child any more and it is not here: it cannot be
					// restored, so the reservation is ABANDONED rather than left standing. That
					// costs the caller the capability and the handle value, which is the truth
					// about what happened; leaving the slot reserved cost it those AND the slot
					// AND a unit of quota, permanently.
					Err(_) => thread.handles().lock().abandon_taken(bootstrap),
				}
			}
			return ERR_RESOURCE_EXHAUSTED;
		}
	};
	// The thread's own handle, and only THEN the transfer is committed.
	//
	// `install_object` is the last thing that can fail, and it fails against the CALLER's table - so
	// committing before it would leave the bootstrap capability gone from the caller and the
	// caller told the call failed. Doing it after means a failure here still has somewhere to put
	// everything back.
	let installed = install_object(&thread, new_thread, Rights::ALL);
	if installed < 0 {
		if child_bootstrap != 0 {
			match process.handles().lock().take(Handle::from_raw(child_bootstrap), Rights::NONE) {
				Ok(cap) => thread.handles().lock().restore_taken(bootstrap, cap),
				Err(_) => thread.handles().lock().abandon_taken(bootstrap),
			}
		}
		return installed;
	}
	// `take_for_transfer` states the contract in as many words - exactly one of `commit_taken` or
	// `restore_taken` must follow - and the success path here followed neither.
	//
	// The cost was not abstract. `commit_taken` does two things: `retire_or_recycle`, which returns
	// the slot to the free list under the generation rules, and `uncharge_handles(1)`. Neither
	// happened, so every successful spawn that passed a bootstrap capability leaked one handle slot
	// AND one unit of the caller's handle quota - on the ordinary success path of the ordinary spawn
	// syscall. A supervisor spawning in a loop walks its own quota down until it cannot spawn.
	if child_bootstrap != 0 {
		thread.handles().lock().commit_taken(bootstrap);
	}
	// The transaction is over, so the target may terminate again.
	drop(extend);
	installed
}

// Start a suspended thread created by thread_create, enqueueing it to run. Exactly
// once: a repeated start returns ERR_INVALID rather than double-enqueueing it.
// Requires the MANAGE right on the thread handle.
fn sys_thread_start(thread_handle: u64) -> i64 {
	let target = match current_typed::<Thread>(thread_handle, ObjectType::Thread, Rights::MANAGE) {
		Ok(o) => o,
		Err(e) => return e,
	};
	// The PROCESS, not only the thread. `try_start` answers "has this thread been started before",
	// which says nothing about whether the process it belongs to is still alive - so a thread built
	// through the race this milestone closes could be enqueued into a process that had already been
	// killed, with its handles closed and its mappings gone.
	if target.process().is_terminating() {
		return ERR_INVALID;
	}
	if sched::thread_start(target) { 0 } else { ERR_INVALID }
}

// Deliver a signal to a process: the holder of its MANAGE capability requests a
// default disposition. INT / TERM / KILL terminate the target; STOP suspends it; CONT
// resumes a suspended one. Each case wakes the target's threads so a blocked thread
// observes the change at once (a kill exits it, a stop parks it, a continue releases
// it) rather than waiting on whatever it was blocked on. INT is catchable: a process
// that armed itself with SYS_SIGNAL_CATCH gets a pending flag set (and its threads
// woken so a blocked poll loop notices) instead of being terminated, so it can stop
// cleanly. There are no other user-installed handlers yet - only the
// default dispositions.
fn sys_process_signal(process_handle: u64, signal: u64) -> i64 {
	// A GROUP HANDLE IS ACCEPTED HERE TOO, and it has to be.
	//
	// A pipeline's job-control handle is a ProcessGroup, and everything that signals a job was
	// written against a Process: the tty turns Ctrl+C into `signal(fg, SIG_INT)`, `fg` resumes a
	// stopped job with `signal(job, SIG_CONT)`, and the session's job table holds whatever it was
	// given. Each of those refused a group with `bad handle` - so a foreground pipeline could not
	// be interrupted and a stopped one could not be resumed, silently, because the return value of
	// a signal is not something a shell checks.
	//
	// The alternative was a second control message and a second code path at each of those three
	// sites, all to answer the same question - "signal this job" - differently depending on how
	// many processes happen to be in it. That is the kind of distinction a caller should not have
	// to carry: the object knows what it is.
	//
	// `sys_process_group_signal` stays, for a caller that means to require a group, and both go
	// through `deliver_signal` so the disposition cannot drift.
	match current_typed::<Process>(process_handle, ObjectType::Process, Rights::MANAGE) {
		Ok(process) => deliver_signal(&process, signal),
		Err(ERR_BAD_HANDLE) => sys_process_group_signal(process_handle, signal),
		Err(e) => e,
	}
}

// Create a ProcessGroup over the Process handles in a user array, so a pipeline can be
// signalled and waited on as the one job it is. Each handle is looked up with MANAGE - the
// same right signalling one of them needs - so a group cannot be assembled out of processes
// the caller could not already signal individually. Membership is sealed here: there is no
// join, which is how "which processes does this reach" stays answerable from the handle.
fn sys_process_group_create(handles_ptr: u64, count: u64) -> i64 {
	use crate::object::process_group::{MAX_GROUP_MEMBERS, ProcessGroup};
	if count == 0 || count as usize > MAX_GROUP_MEMBERS {
		return ERR_INVALID;
	}
	let bytes = count * core::mem::size_of::<u64>() as u64;
	if !user_buf_ok(handles_ptr, bytes) {
		return ERR_INVALID;
	}
	let Some(mut raw) = try_zeroed_u64(count as usize) else {
		return ERR_NO_MEMORY;
	};
	// A short read leaves zeros, and handle 0 is a handle: the group would be built from members
	// the caller did not name.
	if let Err(error) = copy_from_user_exact(raw.as_mut_ptr() as *mut u8, handles_ptr, (count as usize) * 8) {
		return error;
	}
	// FALLIBLE, like the `try_zeroed_u64` three lines above it: the odd one out in its own function,
	// exactly as `sys_wait_any`'s was. The count is bounded by the ABI, which makes this not a
	// denial of service and still not a reason to abort the kernel on a short heap.
	let mut members: alloc::vec::Vec<Arc<Process>> = alloc::vec::Vec::new();
	if members.try_reserve(raw.len()).is_err() {
		return ERR_NO_MEMORY;
	}
	for handle in raw {
		match current_typed::<Process>(handle, ObjectType::Process, Rights::MANAGE) {
			Ok(process) => members.push(process),
			Err(e) => return e,
		}
	}
	let Some(group) = ProcessGroup::create(&members) else {
		return ERR_INVALID;
	};
	let thread = current_thread!();
	// WAIT IS IN THE SET, and its absence is what made a group unwaitable in practice.
	//
	// The object has been waitable since the arm was added to `wait`, and the test that pins that
	// installs its own handle with every right - so the gap was invisible from there. Every handle
	// a real caller gets comes from HERE, and without WAIT the lookup refuses before readiness is
	// ever consulted: `wait_any([group, control])` answered ACCESS_DENIED, which the shell read as
	// "the control channel is ready" and blocked on a channel only the terminal writes to. A
	// foreground pipeline never returned a prompt.
	//
	// A job you may signal and may not wait for is not a job control handle.
	install_object(&thread, group, Rights::MANAGE | Rights::READ | Rights::WAIT | Rights::TRANSFER | Rights::DUPLICATE)
}

// Deliver `signal` to every live member of a group. Authority is the group handle carrying
// MANAGE; membership grants nothing, so a stage cannot signal its siblings - which is the
// confused-deputy shape Unix process groups have, where any member may signal the rest.
//
// Every live member is signalled even if one fails, because a partially interrupted pipeline
// is worse than either outcome: the stages that did receive it exit and the rest keep running
// with their peers gone.
fn sys_process_group_signal(group_handle: u64, signal: u64) -> i64 {
	use crate::object::process_group::ProcessGroup;
	let group = match current_typed::<ProcessGroup>(group_handle, ObjectType::ProcessGroup, Rights::MANAGE) {
		Ok(g) => g,
		Err(e) => return e,
	};
	let mut result = 0;
	// Into a fixed array: a group is bounded at `MAX_GROUP_MEMBERS`, so one pass covers it and the
	// walk asks the heap for nothing. `live()` collects into a `Vec`, which is an infallible
	// allocation on a syscall path.
	let mut members: [Option<alloc::sync::Arc<Process>>; crate::object::process_group::MAX_GROUP_MEMBERS] = [const { None }; _];
	let live = group.live_into(&mut members);
	for slot in members.iter_mut().take(live) {
		if let Some(process) = slot.take() {
			let one = deliver_signal(&process, signal);
			if one != 0 {
				result = one;
			}
		}
	}
	result
}

// PER-MEMBER STATS FOR A GROUP: one `ProcessStats` per stage, in creation order, into the caller's
// array. Returns how many were written, which is the group's size when the buffer is big enough.
//
// THE ORDER IS THE POINT. A pipeline's group is created in the order of the line, so slot `i` is
// stage `i` - and "which stage failed" is a question a caller can only answer if the answer keeps
// its position. A set with no order would say that something failed, which the caller already knew
// from the last stage's status being wrong.
//
// A FINISHED STAGE COMES FROM THE GROUP'S RECORD and a running one from the process. The group
// holds its members weakly, so a finished pipeline's processes may already be gone; the record was
// taken when each reached a terminal state, which is the only moment it is certainly available.
// The live counters are meaningless for a process that no longer exists and are reported as zero
// rather than as a number nobody can act on.
fn sys_process_group_stats(group_handle: u64, buf_ptr: u64, count: u64) -> i64 {
	use crate::object::process_group::ProcessGroup;
	// READ, not MANAGE: asking what a job did is not the authority to end it. The shell holds both,
	// but a monitor handed a group to observe should not thereby be able to kill it - and a right
	// is only withheld where it is checked.
	let group = match current_typed::<ProcessGroup>(group_handle, ObjectType::ProcessGroup, Rights::READ) {
		Ok(g) => g,
		Err(e) => return e,
	};
	// ONE PASS, ONE LOCK, into a fixed array a group's cap covers whole. This read the records and
	// the live members as two independent snapshots and joined them BY INDEX - which is the pairing
	// that came apart the moment the member list was compacted, and would still be two reads of a
	// group that can change between them even now that it is not.
	let mut stats: [ProcessStats; crate::object::process_group::MAX_GROUP_MEMBERS] = [ProcessStats { state: PROC_STATE_RUNNING, ..Default::default() }; _];
	let written = group.snapshot_into(&mut stats);
	let size = core::mem::size_of::<ProcessStats>() as u64;
	let writable = core::cmp::min(count, written as u64);
	let bytes = match writable.checked_mul(size) {
		Some(bytes) => bytes,
		None => return ERR_INVALID,
	};
	if bytes == 0 {
		return 0;
	}
	if !user_buf_ok(buf_ptr, bytes) {
		return ERR_INVALID;
	}
	for index in 0..writable as usize {
		if let Err(e) = write_user(buf_ptr + index as u64 * size, stats[index]) {
			return e;
		}
	}
	writable as i64
}

// Wake every thread of `process`, WITHOUT allocating.
//
// This was `for thread in process.live_threads()`, and `live_threads` collects into a `Vec` - so
// delivering a signal to a process asked the heap for memory, from a syscall, on behalf of ring 3.
// A short heap turned `SYS_PROCESS_SIGNAL` into a kernel abort, and the allocation gate never saw it
// because it does not look for `collect`.
//
// A fixed batch, refilled until the list is walked. Eight is one cache line of pointers and covers
// every process in this tree in one pass; a larger one costs another pass and no correctness. A
// thread that appears twice across two batches is woken twice, which `try_claim_wake` already makes
// a no-op - and a thread that exits between batches is one that no longer needs waking.
fn wake_every_thread(process: &alloc::sync::Arc<Process>) {
	let mut at = 0;
	loop {
		let mut batch: [Option<alloc::sync::Arc<crate::object::thread::Thread>>; 8] = [const { None }; _];
		let (written, next) = process.live_threads_from(at, &mut batch);
		for slot in batch.iter_mut().take(written) {
			// Outside the process's thread lock, which `live_threads_from` has already released:
			// waking takes the scheduler's lock and dropping the last reference runs `Thread::drop`.
			if let Some(thread) = slot.take() {
				sched::wake_thread(&thread);
			}
		}
		if written == 0 && next == at {
			break;
		}
		at = next;
	}
}

// The disposition one signal has on one process, shared by the per-process and per-group
// syscalls so a group cannot drift from what signalling a member directly does.
fn deliver_signal(process: &alloc::sync::Arc<Process>, signal: u64) -> i64 {
	match signal {
		SIG_INT if process.is_int_caught() => {
			process.set_int_pending();
			wake_every_thread(process);
		}
		SIG_INT | SIG_TERM | SIG_KILL => {
			process.terminate();
			wake_every_thread(process);
		}
		SIG_STOP => {
			process.set_stopped(true);
			wake_every_thread(process);
		}
		SIG_CONT => {
			process.set_stopped(false);
			sched::wake_object(process.header().koid());
		}
		_ => return ERR_INVALID,
	}
	0
}

// Arm the calling process to catch `signal` (SIG_INT only for now): a
// later delivery of it sets a pending flag instead of terminating the process. A
// self-service disposition - a process arms only itself - so it needs no capability.
fn sys_signal_catch(signal: u64) -> i64 {
	if signal != SIG_INT {
		return ERR_INVALID;
	}
	current_thread!().process().catch_int();
	0
}

// Poll and clear a pending caught `signal` on the calling process: returns 1 if it
// was delivered since the last take (clearing it), else 0. SIG_INT only.
fn sys_signal_take(signal: u64) -> i64 {
	if signal != SIG_INT {
		return ERR_INVALID;
	}
	if current_thread!().process().take_int_pending() { 1 } else { 0 }
}

// Register the calling thread's channel as the kernel's console input sink: the
// kernel reads serial bytes and sends them on it, and the userspace shell receives
// them on the peer endpoint. The handle must name a Channel the caller can send on.
fn sys_console_attach(handle: u64, privilege: u64) -> i64 {
	// `privilege` names a ConsoleSink. Without it any process could take over the channel the
	// kernel feeds console input to - reading every keystroke meant for the shell - and silence
	// the kernel's own console on the way past.
	if let Err(error) = holds_privilege(privilege, PrivilegeKind::ConsoleSink) {
		return error;
	}
	let channel = match current_typed::<Channel>(handle, ObjectType::Channel, Rights::SEND) {
		Ok(o) => o,
		Err(e) => return e,
	};
	crate::console_input::attach(channel);
	// A userspace console service is taking over: stop the kernel framebuffer console.
	// framebuffer_map already does this when the service maps the boot framebuffer, but a
	// service driving a virtio-gpu display never maps it (it presents through the gpu
	// driver), so the kernel console would otherwise keep rendering every SYS_DEBUG_WRITE
	// byte - the console service's serial mirror among them - as a glyph into the now
	// invisible boot framebuffer, costing ~400 ms of wasted blitting per screenful.
	crate::console::disable();
	0
}

// Map a MemoryObject into the kernel address space, returning its virtual base.
fn sys_memory_map(handle: u64) -> i64 {
	// Nothing new once the process has begun going away: its cleanup has already taken
	// its snapshot of what to unmap, and a mapping registered after that is one nothing
	// will ever collect. Held for the whole operation - see `sys_dma_buffer_map` for why the flag
	// read that used to be here was not the same thing.
	let thread = current_thread!();
	let Some(guard) = thread.process().begin_extend() else {
		return ERR_INVALID;
	};
	let memory = match current_typed::<MemoryObject>(handle, ObjectType::MemoryObject, Rights::MAP) {
		Ok(memory) => memory,
		Err(error) => return error,
	};
	// Reject a duplicate map within the SAME address space; mapping into a different
	// address space is allowed, so an object can be shared (e.g. the init package
	// mapped by both ServiceManager and DeviceManager).
	// A ring-3 caller maps into its own (lower-half) user space with the USER bit
	// so the program can reach the pages; a ring-0 caller maps into the shared
	// kernel window. Either way the active page tables are the caller's, so a
	// plain map_page lands in the right address space.
	let user = arch::percpu::in_user_syscall();
	let space = caller_address_space(user, &thread);
	let cr3 = space.cr3();
	// Claim it under one lock. Asking and then acting let two threads of one process both
	// find the object unmapped and both map it - and the second mapping then left the
	// process's cleanup list while staying in the page tables.
	if !memory.reserve_mapping(cr3) {
		return ERR_INVALID;
	}
	let base = alloc_vrange_in(&space, user, memory.size() as u64);
	if base == 0 {
		memory.abandon_reservation(cr3);
		return ERR_NO_MEMORY;
	}
	// WRITABLE ONLY IF THE CAPABILITY CARRIES `WRITE`.
	//
	// Every mapping syscall checked `Rights::MAP` and then built a writable PTE unconditionally, so
	// a capability deliberately attenuated to `READ | MAP` produced a writable mapping - which is
	// the whole of what attenuation is for. The kernel hands out exactly such capabilities: the
	// bootstrap package and the ramdisk are passed read-only, and a holder could write to both.
	//
	// `MAP` says WHETHER the object may be mapped and `WRITE` says what may be done through it;
	// collapsing the two made `READ`, `WRITE` and `MAP` mean one thing for a direct operation and
	// another for a mapping.
	let writable = handle_rights_allow(handle, Rights::WRITE);
	let flags = arch::paging::PRESENT | arch::paging::NO_EXECUTE | if writable { arch::paging::WRITABLE } else { 0 } | if user { arch::paging::USER } else { 0 };
	let frames = memory.frames();
	if !map_pages_or_rollback(base, frames.len(), flags, |i| frames[i]) {
		free_vrange(Some(&space), base, memory.size() as u64);
		memory.abandon_reservation(cr3);
		return ERR_NO_MEMORY;
	}
	// Same rule as the DMA path: a mapping in the page tables that no registry describes is the
	// state teardown cannot clean up, so a commit that finds nothing undoes the map instead.
	if !memory.commit_mapping(cr3, base) {
		for i in 0..frames.len() {
			arch::paging::unmap_page(base + i as u64 * PAGE_SIZE);
		}
		free_vrange(Some(&space), base, memory.size() as u64);
		return ERR_NO_MEMORY;
	}
	// `remove_mapping` unmaps the pages AND returns the virtual range, which is the whole rollback:
	// a mapping the process's cleanup list does not know about is one nothing will ever collect, so
	// a record that cannot be made has to undo the map rather than leave it.
	if !guard.record_memory_mapping(memory.clone()) {
		memory.remove_mapping(&space);
		return ERR_NO_MEMORY;
	}
	base as i64
}

// Remove a MemoryObject's mapping from the kernel address space.
fn sys_memory_unmap(handle: u64) -> i64 {
	let thread = current_thread!();
	let memory = match current_typed::<MemoryObject>(handle, ObjectType::MemoryObject, Rights::MAP) {
		Ok(memory) => memory,
		Err(error) => return error,
	};
	let space = caller_address_space(arch::percpu::in_user_syscall(), &thread);
	if !memory.remove_mapping(&space) {
		return ERR_NOT_MAPPED;
	}
	thread.process().forget_memory_mapping(&memory);
	0
}

// Derive a weaker handle to the same object (attenuation only).
fn sys_handle_duplicate(handle: u64, rights_bits: u64) -> i64 {
	let thread = current_thread!();
	let new_rights = Rights::from_bits(rights_bits as u32);
	let mut table = thread.handles().lock();
	match table.duplicate(Handle::from_raw(handle), new_rights) {
		Ok(h) => h.raw() as i64,
		Err(HandleError::AccessDenied) => ERR_ACCESS_DENIED,
		// A full quota is not a bad handle, and saying so is the difference between a
		// caller that can back off and one that thinks it was given a broken handle.
		Err(HandleError::LimitReached) => ERR_RESOURCE_EXHAUSTED,
		Err(_) => ERR_BAD_HANDLE,
	}
}

// Close a handle in the caller's table.
fn sys_handle_close(handle: u64) -> i64 {
	let thread = current_thread!();
	let mut table = thread.handles().lock();
	match table.close(Handle::from_raw(handle)) {
		Ok(()) => 0,
		Err(_) => ERR_BAD_HANDLE,
	}
}

// Copy a byte payload out of a caller-supplied buffer through the sanctioned SMAP
// window. Ring-0 self-calls pass kernel pointers, which the window does not
// affect; a ring-3 caller's pointer has been validated by user_buf_ok.
fn read_bytes(ptr: u64, len: usize) -> Result<Vec<u8>, i64> {
	if ptr == 0 || len == 0 {
		return Ok(Vec::new());
	}
	// An allocation this kernel could not make is NOT an empty message.
	//
	// It used to answer one: the buffer failed, `Vec::new()` went out, and the send delivered a
	// zero-byte message and reported success. For a protocol where an empty message means something
	// - and this system has at least one, where it marks the end of a write stream - memory pressure
	// silently changed what was said. The batch path already answered `ERR_NO_MEMORY` here; this one
	// had its own helper and its own answer.
	// The two failures are told apart because they send an operator to different components: a
	// refused allocation is this machine, a short read is the caller's own address space.
	let Some(mut bytes) = try_zeroed_bytes(len) else {
		return Err(ERR_NO_MEMORY);
	};
	// EVERY byte, or nothing. The buffer is zero-filled, so a short read used to produce the prefix
	// the caller meant followed by zeros it did not - and `sys_channel_send` sent that and reported
	// success. Zero-filling is right for a scalar (`read_user`), where a partly-read value must not
	// carry stack residue into a decision; for a payload it turns a failed copy into a plausible
	// message.
	copy_from_user_exact(bytes.as_mut_ptr(), ptr, len)?;
	Ok(bytes)
}

// Create a connected channel pair, install a handle to each endpoint in the
// caller's table, and write the two raw handles to *out0_ptr and *out1_ptr.
// `depth` bounds each endpoint's queue in messages (0 = the default), so a
// creator that knows its traffic picks its own backpressure point.
fn sys_channel_create(out0_ptr: u64, out1_ptr: u64, depth: u64) -> i64 {
	let thread = current_thread!();
	if out0_ptr == 0 || out1_ptr == 0 {
		return ERR_INVALID;
	}
	if !user_buf_ok(out0_ptr, 8) || !user_buf_ok(out1_ptr, 8) {
		return ERR_INVALID;
	}
	let Some((ep0, ep1)) = Channel::try_create_with_depth(depth as usize) else { return ERR_RESOURCE_EXHAUSTED };
	let (h0, h1) = {
		let mut table = thread.handles().lock();
		// Enforce the Domain's handle quota for both endpoints; if the second
		// does not fit, roll the first back so neither is left half-created.
		let h0 = match table.try_insert_object(ep0, Rights::ALL) {
			Some(h) => h,
			None => return ERR_RESOURCE_EXHAUSTED,
		};
		let h1 = match table.try_insert_object(ep1, Rights::ALL) {
			Some(h) => h,
			None => {
				let _ = table.close(h0);
				return ERR_RESOURCE_EXHAUSTED;
			}
		};
		(h0, h1)
	};
	// Both numbers out, or NEITHER handle stays.
	//
	// This returned 0 whatever happened to the writes. If the output page went away after
	// `user_buf_ok` - the caller's own doing, from another thread - the caller then owned two
	// endpoints whose numbers it had never been told: unclosable, uncloseable by anyone, and held
	// against its Domain's handle quota until the process died. A success it could not act on and a
	// leak it could not find.
	//
	// Rolling both back is the only answer that leaves the caller where it started. Rolling back
	// just the second would leave one endpoint of a pair, which is not a thing to hand anybody.
	if let Err(error) = write_user(out0_ptr, h0.raw()).and_then(|()| write_user(out1_ptr, h1.raw())) {
		let mut table = thread.handles().lock();
		let _ = table.close(h0);
		let _ = table.close(h1);
		return error;
	}
	0
}

// Send a message (byte payload + optionally one transferred handle) to the peer.
// transferred handle is consumed only on a successful send (left intact on
// failure, so the caller can retry on WOULD_BLOCK).
fn sys_channel_send(ch: u64, bytes_ptr: u64, bytes_len: u64, xfer: u64) -> i64 {
	// The length is refused before anything is looked up or locked: a number this large is
	// not a message whatever handle it names, and answering it costs nothing.
	if bytes_len as usize > abi::MAX_MESSAGE_BYTES {
		return ERR_INVALID;
	}
	let thread = current_thread!();
	let object = {
		let table = thread.handles().lock();
		match table.lookup_typed(Handle::from_raw(ch), ObjectType::Channel, Rights::SEND) {
			Ok(object) => object,
			Err(HandleError::AccessDenied) => return ERR_ACCESS_DENIED,
			Err(_) => return ERR_BAD_HANDLE,
		}
	};
	let channel = object.as_any().downcast_ref::<Channel>().expect("type checked by lookup_typed");
	if !user_buf_ok(bytes_ptr, bytes_len) {
		return ERR_INVALID;
	}
	let bytes = match read_bytes(bytes_ptr, bytes_len as usize) {
		Ok(bytes) => bytes,
		Err(error) => return error,
	};
	// THE SAME transfer the batch path uses, and for the reason the batch path was given it.
	//
	// This built the capability with `Capability::new` from a LOOKUP - a clone of the authority -
	// sent it, and then closed the caller's handle with the result discarded. Two threads of one
	// process naming the same handle could both look it up, both clone, and both send: one handle
	// became two capabilities without `DUPLICATE`, which is the one thing a capability system may
	// not do. The close of the loser then failed and nobody was told.
	//
	// `take_for_transfer` empties the slot and reserves it, so a second thread finds nothing to
	// take; `commit_taken` kills the handle value once delivery succeeds, and `restore_taken` gives
	// the capability back at the SAME handle when it does not - so a refused send costs the caller
	// nothing, not even the name it used.
	let handle = Handle::from_raw(xfer);
	// THE ROOM FOR THE CAPABILITY IS BOOKED BEFORE THE CAPABILITY MOVES.
	//
	// This was `alloc::vec![cap]` immediately after `take_for_transfer` had emptied and reserved the
	// caller's slot - the worst available ordering, with the allocation failure landing between the
	// two halves of a transaction, where the only answers left are to abort or to leave a
	// reservation nobody can resolve. Reserving first means a short heap refuses before anything has
	// moved.
	let mut caps: Vec<Capability> = Vec::new();
	if xfer != 0 {
		if caps.try_reserve(1).is_err() {
			return ERR_NO_MEMORY;
		}
		let mut table = thread.handles().lock();
		match table.take_for_transfer(handle, Rights::TRANSFER) {
			Ok(cap) => caps.push(cap),
			Err(HandleError::AccessDenied) => return ERR_ACCESS_DENIED,
			Err(_) => return ERR_BAD_HANDLE,
		}
	}
	match channel.send_charged_or_return(Message::new(bytes, caps), thread.domain()) {
		Ok(()) => {
			// Delivered: the handle value dies now, and its quota is refunded.
			if xfer != 0 {
				thread.handles().lock().commit_taken(handle);
			}
			thread.process().record_send();
			0
		}
		Err(err) => {
			// Undelivered: the capability goes back to the handle it was named by, still live and
			// still the same value.
			if xfer != 0 {
				let mut table = thread.handles().lock();
				for cap in err.1 {
					table.restore_taken(handle, cap);
				}
			}
			match err.0 {
				ChannelError::Full => ERR_WOULD_BLOCK,
				ChannelError::PeerClosed => ERR_PEER_CLOSED,
				_ => ERR_INVALID,
			}
		}
	}
}

// MOVE ONE CAPABILITY WITH LESS AUTHORITY THAN THE SENDER HOLDS.
//
// `sys_channel_send` moves a handle with the rights it already has, so "arrives without TRANSFER"
// was not a property of the existing primitive and had to become one. There is no room for a fifth
// argument - the ABI carries exactly four on all three ports and the ordinary send spends all of
// them - so the last one is a POINTER to an `abi::CapTransfer` in the caller's memory, read through
// the same `user_buf_ok`/`read_user` path every other pointer argument uses.
//
// INSIDE, THIS IS THE SAME TRANSACTIONAL MOVE THE ORDINARY SEND PERFORMS. A mask applied to an
// existing transfer, not a second way of moving a capability - which matters because the ordinary
// send was itself fixed for this: it used to look up, clone and send, so two threads naming one
// handle could both look up, both clone and both send, and one handle became two capabilities
// without DUPLICATE. A new call that reimplemented the move would have reintroduced exactly that.
//
// The receiver gets the INTERSECTION of the capability's rights and the mask. A mask naming a right
// the capability does not hold is not an error, because an intersection cannot widen.
fn sys_channel_send_attenuated(ch: u64, bytes_ptr: u64, bytes_len: u64, transfer_ptr: u64) -> i64 {
	if bytes_len as usize > abi::MAX_MESSAGE_BYTES {
		return ERR_INVALID;
	}
	let size = core::mem::size_of::<abi::CapTransfer>() as u64;
	if !user_buf_ok(transfer_ptr, size) {
		return ERR_INVALID;
	}
	let transfer: abi::CapTransfer = read_user(transfer_ptr);
	// A SEND THAT MOVES NOTHING IS NOT THIS CALL. The ordinary send is what carries a bare message,
	// and accepting handle 0 here would make the attenuating path a second way of doing the same
	// thing with a mask nobody reads.
	if transfer.handle == 0 {
		return ERR_INVALID;
	}
	let thread = current_thread!();
	let object = {
		let table = thread.handles().lock();
		match table.lookup_typed(Handle::from_raw(ch), ObjectType::Channel, Rights::SEND) {
			Ok(object) => object,
			Err(HandleError::AccessDenied) => return ERR_ACCESS_DENIED,
			Err(_) => return ERR_BAD_HANDLE,
		}
	};
	let channel = object.as_any().downcast_ref::<Channel>().expect("type checked by lookup_typed");
	if !user_buf_ok(bytes_ptr, bytes_len) {
		return ERR_INVALID;
	}
	let bytes = match read_bytes(bytes_ptr, bytes_len as usize) {
		Ok(bytes) => bytes,
		Err(error) => return error,
	};
	// Bits outside the defined set are dropped rather than refused: `from_bits` is the boundary
	// hygiene every rights value arriving from ring 3 goes through, and a mask cannot widen anyway.
	let mask = Rights::from_bits(transfer.rights);
	let handle = Handle::from_raw(transfer.handle);
	let mut caps: Vec<Capability> = Vec::new();
	if caps.try_reserve(1).is_err() {
		return ERR_NO_MEMORY;
	}
	// THE SLOT IS EMPTIED AND RESERVED BEFORE ANYTHING IS COPIED, so a second thread naming the same
	// handle finds nothing to take. This is the transaction; the mask below is applied inside it.
	let taken = {
		let mut table = thread.handles().lock();
		match table.take_for_transfer(handle, Rights::TRANSFER) {
			Ok(cap) => cap,
			Err(HandleError::AccessDenied) => return ERR_ACCESS_DENIED,
			Err(_) => return ERR_BAD_HANDLE,
		}
	};
	// THE ORIGINAL IS KEPT AND THE ATTENUATED COPY IS WHAT TRAVELS. A refused send has to leave the
	// sender EXACTLY where it was - the handle still open, at the same value, with its rights
	// unchanged - and narrowing the capability in place would have made the restore a widening
	// operation, which is a primitive this kernel should not have at all.
	caps.push(taken.attenuated(mask));
	match channel.send_charged_or_return(Message::new(bytes, caps), thread.domain()) {
		Ok(()) => {
			// Delivered: the handle value dies now, and its quota is refunded. `taken` is dropped
			// here having never been installed anywhere.
			thread.handles().lock().commit_taken(handle);
			thread.process().record_send();
			0
		}
		Err(err) => {
			// Undelivered. The attenuated copy the message carried is dropped, and what goes back
			// into the slot is the capability that came out of it.
			drop(err.1);
			thread.handles().lock().restore_taken(handle, taken);
			match err.0 {
				ChannelError::Full => ERR_WOULD_BLOCK,
				ChannelError::PeerClosed => ERR_PEER_CLOSED,
				_ => ERR_INVALID,
			}
		}
	}
}

// Report the byte length of the next pending message on the channel WITHOUT
// dequeuing it (ERR_WOULD_BLOCK when nothing is queued, ERR_PEER_CLOSED once the
// queue is empty and the peer is gone), so a receiver sizes its buffer exactly
// for the recv that follows instead of guessing a ceiling.
fn sys_channel_peek(ch: u64) -> i64 {
	let thread = current_thread!();
	let object = {
		let table = thread.handles().lock();
		match table.lookup_typed(Handle::from_raw(ch), ObjectType::Channel, Rights::RECEIVE) {
			Ok(o) => o,
			Err(HandleError::AccessDenied) => return ERR_ACCESS_DENIED,
			Err(_) => return ERR_BAD_HANDLE,
		}
	};
	let channel = object.as_any().downcast_ref::<Channel>().expect("type checked by lookup_typed");
	match channel.peek_len() {
		Ok(len) => len as i64,
		Err(ChannelError::Empty) => ERR_WOULD_BLOCK,
		Err(ChannelError::PeerClosed) => ERR_PEER_CLOSED,
		Err(_) => ERR_INVALID,
	}
}

// Receive a message: copy up to `bytes_cap` payload bytes to `bytes_ptr` and, if
// the message carried a transferred capability, install it and write the new
// handle to *out_handle_ptr (0 if none). Returns the payload byte count.
// Send a message transferring SEVERAL capabilities. `caps_ptr` points at
// `[count, handle0, ...]`; the count travels with the list because the four argument registers
// are already spent on the channel, the buffer and its length.
//
// Every handle is looked up and every one must carry TRANSFER before anything is sent, so a
// list with one bad entry moves nothing - a partially transferred set would leave the sender
// holding some of what it meant to give away and the receiver wired to half a graph.
fn sys_channel_send_caps(ch: u64, bytes_ptr: u64, bytes_len: u64, caps_ptr: u64) -> i64 {
	if bytes_len as usize > abi::MAX_MESSAGE_BYTES {
		return ERR_INVALID;
	}
	let thread = current_thread!();
	if !user_buf_ok(bytes_ptr, bytes_len) || !user_buf_ok(caps_ptr, 8) {
		return ERR_INVALID;
	}
	let count = read_user::<u64>(caps_ptr) as usize;
	if count == 0 || count > abi::MAX_MESSAGE_CAPS {
		return ERR_INVALID;
	}
	if !user_buf_ok(caps_ptr, ((count + 1) * 8) as u64) {
		return ERR_INVALID;
	}
	let mut raw = [0u64; abi::MAX_MESSAGE_CAPS];
	for (index, slot) in raw.iter_mut().take(count).enumerate() {
		*slot = read_user::<u64>(caps_ptr + ((index + 1) * 8) as u64);
	}

	let object = {
		let table = thread.handles().lock();
		match table.lookup_typed(Handle::from_raw(ch), ObjectType::Channel, Rights::SEND) {
			Ok(object) => object,
			Err(HandleError::AccessDenied) => return ERR_ACCESS_DENIED,
			Err(_) => return ERR_BAD_HANDLE,
		}
	};
	let channel: Arc<Channel> = object.into_any_arc().downcast::<Channel>().ok().expect("type checked by lookup_typed");
	let Some(mut bytes) = try_zeroed_bytes(bytes_len as usize) else {
		return ERR_NO_MEMORY;
	};
	// The payload, all of it - see `read_bytes` for why a short read must not become a message.
	if let Err(error) = copy_from_user_exact(bytes.as_mut_ptr(), bytes_ptr, bytes_len as usize) {
		return error;
	}

	if has_repeat(&raw[..count]) {
		return ERR_INVALID;
	}

	// TAKE every capability, under one lock, or take none. A transfer moves a capability, and a
	// move that leaves the source in place is a duplication however the bookkeeping is written.
	//
	// The handle VALUES stay alive until the message is actually delivered. They used to die at
	// the take, so an undelivered send put the capabilities back under fresh handles the caller
	// had no way to learn - and a caller doing the only sensible thing with a failed send, closing
	// what it could not hand over, closed a value that was already dead and left the capability
	// unreachable and still charged. One leaked capability per failed transfer, in userspace code
	// that was correct. Nothing in the ABI could have told it otherwise.
	//
	// So the slots are emptied and RESERVED across the send: the handle names nothing while the
	// message is in flight, which is the truth about it, and the outcome decides whether the value
	// dies or comes back.
	// Both reserved before the first take, for the reason the single send gives: an allocation that
	// fails between the two halves of a transaction has no good answer, and one that fails before it
	// is an ordinary refusal. `count` is bounded by the ABI, so this is not a denial of service -
	// it is the abort-on-short-heap that a bounded size does not excuse.
	let mut caps: Vec<Capability> = Vec::new();
	let mut taken: Vec<Handle> = Vec::new();
	if caps.try_reserve(count).is_err() || taken.try_reserve(count).is_err() {
		return ERR_NO_MEMORY;
	}
	{
		let mut table = thread.handles().lock();
		for &raw_handle in raw.iter().take(count) {
			let handle = Handle::from_raw(raw_handle);
			match table.take_for_transfer(handle, Rights::TRANSFER) {
				Ok(cap) => {
					caps.push(cap);
					taken.push(handle);
				}
				Err(err) => {
					// put back whatever was taken before the refusal, each to the handle it came
					// from: a rejected send costs the caller nothing at all.
					for (handle, cap) in taken.into_iter().zip(caps) {
						table.restore_taken(handle, cap);
					}
					return match err {
						HandleError::AccessDenied => ERR_ACCESS_DENIED,
						_ => ERR_BAD_HANDLE,
					};
				}
			}
		}
	}
	match channel.send_charged_or_return(Message::new(bytes, caps), thread.domain()) {
		Ok(()) => {
			// Delivered: the handle values die now, and their quota is refunded.
			let mut table = thread.handles().lock();
			for handle in taken {
				table.commit_taken(handle);
			}
			thread.process().record_send();
			0
		}
		Err(err) => {
			// Undelivered: the capabilities go back to the handles they were named by, still live
			// and still the same values, so the caller can close them or try again.
			let mut table = thread.handles().lock();
			for (handle, cap) in taken.into_iter().zip(err.1) {
				table.restore_taken(handle, cap);
			}
			match err.0 {
				ChannelError::PeerClosed => ERR_PEER_CLOSED,
				_ => ERR_WOULD_BLOCK,
			}
		}
	}
}

// Take one message, having first made room for everything it carries. BOTH receives go through
// this; that they did not is what this is here to fix.
//
// `sys_channel_recv_caps` was made transactional and `sys_channel_recv` was left as it was, and the
// one left behind is the one nearly everything uses - all four of the runtime's receive wrappers
// issue `SYS_CHANNEL_RECV`, and `SYS_CHANNEL_RECV_CAPS` is the exception for a receiver expecting
// more than one capability. So the rare path got the treatment and the common path kept doing this:
//
//     let message = channel.recv()?;                       // dequeued. gone from the queue.
//     ...
//     thread.handles().lock().insert(cap)                  // charge_handle: accounting, no limit
//
// `HandleTable::insert` is the unbounded install, and its comment says why - it is used by paths
// that must not fail. The install could not be allowed to fail BECAUSE the message was already
// destroyed if it did, so the quota was dropped rather than the ordering fixed. The result was a
// direct hole in resource-domain isolation: a Domain at its handle limit receives a transferred
// capability and is over it, receives again and is further over. Every other way of acquiring a
// handle is bounded - `try_insert`, `try_insert_or_return`, `duplicate` (bounded for
// exactly this reason) - and asking a peer to send one was not.
//
// Three steps, in this order, and the order is the whole thing:
//
//   1. LOOK at the head and learn its identity and its shape.
//   2. RESERVE precisely what that message needs - quota and slot memory both.
//   3. TAKE that message BY IDENTITY, or nothing.
//
// Reserving before looking is what the first version of the multi-cap fix did: it booked
// `MAX_MESSAGE_CAPS` up front because it had no way to name the message it had inspected. That
// refuses a Domain with one free handle slot a message carrying one capability, which is a false
// refusal in the safe direction but a false refusal all the same - and for the plain receive it
// would be worse than that, because reserving one handle for every message would stop a Domain at
// its limit from receiving PLAIN BYTES.
//
// `install_caps_max` is how many capabilities the caller will actually install, which is where the
// two receives differ: the plain one takes the first and drops the rest, so it reserves at most
// one however many arrived. `refuse_above_bytes` is likewise the caller's contract - the plain
// receive truncates to the buffer it was given and passes `usize::MAX` here, the multi-cap one
// refuses and leaves the message where it is.
fn receive_transactionally(thread: &crate::object::thread::Thread, channel: &Channel, refuse_above_bytes: usize, install_caps_max: usize) -> Result<crate::object::channel::Message, i64> {
	// Bounded, because the only way to go round again is for another receiver on this endpoint to
	// have taken the message in between - which is somebody making progress, not a stall. Bounded
	// anyway: a caller told to try again can, and a loop with no ceiling inside a syscall is a
	// place a contended endpoint could hold a core.
	const ATTEMPTS: usize = 8;
	for _ in 0..ATTEMPTS {
		let (id, bytes, caps) = match channel.peek_identified() {
			Ok(shape) => shape,
			Err(ChannelError::PeerClosed) => return Err(ERR_PEER_CLOSED),
			Err(_) => return Err(ERR_WOULD_BLOCK),
		};
		if bytes > refuse_above_bytes {
			return Err(ERR_INVALID);
		}
		let reserved = caps.min(install_caps_max);
		if !thread.handles().lock().reserve(reserved) {
			return Err(ERR_RESOURCE_EXHAUSTED);
		}
		match channel.recv_identified(id, refuse_above_bytes, abi::MAX_MESSAGE_CAPS) {
			Ok(message) => return Ok(message),
			Err(refusal) => {
				thread.handles().lock().release_reservation(reserved);
				match refusal {
					// The message went to another receiver between the look and the take. Nothing
					// was destroyed and nothing is wrong: look at whatever is there now.
					RecvRefusal::Superseded => continue,
					RecvRefusal::TooLarge | RecvRefusal::TooManyCaps => return Err(ERR_INVALID),
					RecvRefusal::Gone(ChannelError::PeerClosed) => return Err(ERR_PEER_CLOSED),
					RecvRefusal::Gone(_) => return Err(ERR_WOULD_BLOCK),
				}
			}
		}
	}
	// Eight receivers took eight messages out from under this one. Not an error about the channel,
	// so the answer is the one that means "nothing for you right now, come back".
	Err(ERR_WOULD_BLOCK)
}

// Receive a message and take EVERY capability it carried. `caps_ptr` is written as
// `[count, handle0, ...]`. The ordinary `sys_channel_recv` takes the first and drops the rest,
// which is right for a receiver expecting one and silent loss for a receiver expecting more.
fn sys_channel_recv_caps(ch: u64, bytes_ptr: u64, bytes_cap: u64, caps_ptr: u64) -> i64 {
	let thread = current_thread!();
	if !user_buf_ok(bytes_ptr, bytes_cap) || !user_buf_ok(caps_ptr, ((abi::MAX_MESSAGE_CAPS + 1) * 8) as u64) {
		return ERR_INVALID;
	}
	let channel = match current_typed::<Channel>(ch, ObjectType::Channel, Rights::RECEIVE) {
		Ok(c) => c,
		Err(e) => return e,
	};
	// Look, reserve exactly, take by identity. A message that does not fit is left in the queue -
	// the caller can come back with a bigger buffer or after closing some handles, and nothing is
	// destroyed that nobody can retry.
	//
	// This used to be three steps - `peek_shape`, `reserve`, `recv` - each taking the queue lock on
	// its own, with nothing tying the three to one message. A second receiver on the same endpoint
	// could take the peeked message in between, and what arrived was then a different message while
	// the caller had already decided what it could hold. The copy below uses the RECEIVED length,
	// so a receiver that declared a hundred bytes could be handed a megabyte and the kernel would
	// write all of it into a buffer validated for a hundred: a kernel-to-userspace overrun
	// reachable from ring 3 with two threads and no timing tricks. The capability half had the same
	// shape, installing handles counted from one message against a reservation paid for another -
	// and the reservation was never returned when the recv then failed, so the race leaked handle
	// quota even when it delivered nothing.
	let mut message = match receive_transactionally(&thread, &channel, bytes_cap as usize, abi::MAX_MESSAGE_CAPS) {
		Ok(message) => message,
		Err(error) => return error,
	};
	// The payload BEFORE anything is installed, and the message goes back if it will not fit.
	//
	// A short copy here used to be invisible: the count was discarded, the capabilities were
	// installed anyway, and the syscall returned the message's full length. The message was off the
	// queue, the caller had part of it, and nothing said so - which is precisely the delivery invariant,
	// broken at the boundary the exception table opened behind it.
	//
	// The message is put back at the head rather than destroyed. A short copy means the caller
	// unmapped its own buffer, and that is not a reason to lose what somebody else sent.
	if let Err(error) = copy_to_user_exact(bytes_ptr, message.bytes.as_ptr(), message.bytes.len()) {
		thread.handles().lock().release_reservation(message.caps.len().min(abi::MAX_MESSAGE_CAPS));
		channel.return_to_head(message);
		return error;
	}
	// Delivery is committed here: the payload is in the caller's buffer and the message cannot go
	// back to the queue, so this is where the sender's queued-bytes charge is released. Everything
	// before this point can still `return_to_head`, and does so with the charge intact.
	channel.commit_delivery(&mut message);
	let delivered = message.bytes.len();
	let mut raws = [0u64; abi::MAX_MESSAGE_CAPS];
	let mut installed = 0usize;
	{
		let mut table = thread.handles().lock();
		for cap in message.caps.into_iter().take(abi::MAX_MESSAGE_CAPS) {
			// Against the reservation taken above. `insert` would charge the quota a second time
			// for a handle already paid for, and could refuse a handle the caller was promised.
			raws[installed] = table.insert_reserved(cap).raw();
			installed += 1;
		}
	}
	// The array, then its count - and if any of it will not land, NONE of the handles stay.
	//
	// A caller that is not told a handle's number cannot close it, so a half-written array is a
	// quota leak it can neither see nor repair. The writes used to be discarded outright, which made
	// that the ordinary outcome of a caller unmapping its own buffer at the wrong moment.
	//
	// The message itself cannot be returned here the way the payload path returns it: the
	// capabilities have left it and are in the handle table. What is recoverable is recovered - the
	// handles are closed, so nothing leaks - and the caller gets an error instead of a length.
	let out = (0..installed).try_for_each(|i| write_user(caps_ptr + ((i + 1) * 8) as u64, raws[i])).and_then(|()| write_user(caps_ptr, installed as u64));
	if let Err(error) = out {
		let mut table = thread.handles().lock();
		for raw in raws.iter().take(installed) {
			let _ = table.close(Handle::from_raw(*raw));
		}
		return error;
	}
	thread.process().record_recv();
	delivered as i64
}

fn sys_channel_recv(ch: u64, bytes_ptr: u64, bytes_cap: u64, out_handle_ptr: u64) -> i64 {
	let thread = current_thread!();
	if !user_buf_ok(bytes_ptr, bytes_cap) || (out_handle_ptr != 0 && !user_buf_ok(out_handle_ptr, 8)) {
		return ERR_INVALID;
	}
	let object = {
		let table = thread.handles().lock();
		match table.lookup_typed(Handle::from_raw(ch), ObjectType::Channel, Rights::RECEIVE) {
			Ok(o) => o,
			Err(HandleError::AccessDenied) => return ERR_ACCESS_DENIED,
			Err(_) => return ERR_BAD_HANDLE,
		}
	};
	let channel = object.as_any().downcast_ref::<Channel>().expect("type checked by lookup_typed");
	// At most ONE capability is installed however many the message carried, so at most one is
	// reserved. `usize::MAX` for the byte ceiling keeps this receive's contract exactly as it was:
	// it truncates to the buffer it was given rather than refusing, which is what its callers are
	// written against - the storage service's stream loop peeks the length, asks the filesystem
	// for a buffer of that size, and treats a short read as a short read. Turning that into a
	// refusal would convert a handled shortfall into an aborted stream. The multi-cap receive
	// refuses instead, because its callers know the shape they are expecting and a message that
	// does not fit is one they can come back for.
	//
	// ONLY WHEN THE CALLER WANTS ONE. This reserved a handle unconditionally, including when
	// `out_handle_ptr` is null - which is the caller saying "I am not taking the capability". A
	// process at its exact handle quota was then refused BEFORE it could dequeue, so it could not
	// drain its own channel of capability-bearing messages despite needing no new handle. The
	// reservation was released afterwards, which is too late to help: the refusal happens first.
	let install_max: usize = if out_handle_ptr != 0 { 1 } else { 0 };
	let mut message = match receive_transactionally(&thread, channel, usize::MAX, install_max) {
		Ok(message) => message,
		Err(error) => return error,
	};
	// The payload first, all of it, and the message goes back to the head if it will not land.
	//
	// `n` is a TRUNCATION to the caller's buffer and that is this receive's contract - see above. A
	// short copy is a different thing: the caller's buffer stopped existing partway through, and
	// discarding that count meant reporting `n` bytes delivered when fewer arrived. The message was
	// already off the queue, so nothing could be retried and nothing said anything was wrong.
	let n = core::cmp::min(message.bytes.len(), bytes_cap as usize);
	if n > 0 && bytes_ptr != 0 {
		if let Err(error) = copy_to_user_exact(bytes_ptr, message.bytes.as_ptr(), n) {
			thread.handles().lock().release_reservation(message.caps.len().min(install_max));
			channel.return_to_head(message);
			return error;
		}
	}
	// Committed, for the same reason as the batch path: past here the message is the caller's.
	channel.commit_delivery(&mut message);
	thread.process().record_recv();
	// Install the transferred capability (if any) and report its new handle. Against the
	// reservation taken above: `insert` is the UNBOUNDED install, and using it here is how a
	// Domain at its handle limit went past it by receiving. The capabilities past the first are
	// dropped, as they always were - they were never installed, so they cost no quota.
	if out_handle_ptr != 0 {
		let handle_value = match message.caps.into_iter().next() {
			Some(cap) => thread.handles().lock().insert_reserved(cap).raw(),
			None => 0,
		};
		// A handle whose number never reached the caller is a handle nobody can close. Closed here
		// instead, and the error goes back in place of the length - the payload has already been
		// delivered, so this is not a receive that can be retried, but it is one that does not leak.
		if let Err(error) = write_user(out_handle_ptr, handle_value) {
			if handle_value != 0 {
				let _ = thread.handles().lock().close(Handle::from_raw(handle_value));
			}
			return error;
		}
	}
	// No `else`: with a null out pointer nothing was reserved in the first place, so there is
	// nothing to give back. The message's capabilities are dropped with it, which is what the
	// caller asked for by not offering somewhere to put them.
	n as i64
}

// Block the calling thread until the object behind `handle` becomes ready (a
// Channel readable, an Event signaled, a Timer expired) or `deadline` (an
// absolute LAPIC tick value; 0 = no timeout) passes. Returns 0 when the object
// became ready, ERR_TIMED_OUT on timeout. This is the kernel's one blocking
// primitive; the non-blocking send/recv/poll calls layer the synchronous-looking
// `call()` on top of it. The handle must carry the WAIT right. `flags` may carry
// WAIT_PERIODIC: the deadline is a recurring housekeeping wake, still honored but
// never holding the scheduler's settling point open. It may also carry
// WAIT_WRITABLE: readiness for a Channel then means the peer's queue has room (or
// the peer is gone), so a sender that got WOULD_BLOCK blocks here until the
// receiver drains - backpressure without spinning.
fn sys_wait(handle: u64, deadline: u64, flags: u64) -> i64 {
	let periodic = flags & abi::WAIT_PERIODIC != 0;
	let writable = flags & abi::WAIT_WRITABLE != 0;
	let thread = current_thread!();
	let object = {
		let table = thread.handles().lock();
		match table.lookup(Handle::from_raw(handle), Rights::WAIT) {
			Ok(o) => o,
			Err(HandleError::AccessDenied) => return ERR_ACCESS_DENIED,
			Err(_) => return ERR_BAD_HANDLE,
		}
	};
	let koid = object.header().koid();
	// A ProcessGroup is ready when its MEMBERS terminate, and a terminating process wakes
	// its own koid - not the group's. Nothing connected the two, so a waiter on a group
	// could stay parked while the group reported itself finished. Registering on the
	// members as well as the group is the fix that needs no back-link from a process to
	// the groups it belongs to: the wake it already sends is the one being listened for.
	// FALLIBLY. `collect()` here is an infallible allocation sized by a process group's live
	// membership, on a path a ring-3 caller decides to take - and an infallible allocation on a
	// short heap ABORTS the kernel.
	let mut group_koids: Vec<u64> = Vec::new();
	if let Some(group) = object.as_any().downcast_ref::<crate::object::process_group::ProcessGroup>() {
		let mut live: [Option<alloc::sync::Arc<Process>>; crate::object::process_group::MAX_GROUP_MEMBERS] = [const { None }; _];
		let count = group.live_into(&mut live);
		if group_koids.try_reserve(count + 1).is_err() {
			return ERR_NO_MEMORY;
		}
		group_koids.push(koid);
		group_koids.extend(live.iter().take(count).filter_map(|member| member.as_ref()).map(|member| member.header().koid()));
	}
	// Condition-variable loop: re-check readiness after each wake, so an early or
	// spurious wake just re-blocks and a deadline is honored on re-check. A signal that
	// arrives while blocked is honoured first: a kill retires the thread, a stop parks it.
	loop {
		if thread.process().is_killed() {
			drop(object);
			sched::exit();
		}
		if thread.process().is_stopped() {
			sched::block_on(thread.process().header().koid(), sched::NO_DEADLINE);
			continue;
		}
		// A CAUGHT INTERRUPT ENDS THE WAIT, without consuming it - see ERR_INTERRUPTED. A process
		// that armed itself to handle Ctrl+C polls a flag, and one parked here polls nothing: the
		// wake this signal sends found no object ready and this loop put it straight back to sleep.
		if thread.process().take_int_report() {
			return ERR_INTERRUPTED;
		}
		if object_ready_for(&object, writable) {
			return 0;
		}
		let block_deadline = wait_block_deadline(&object, deadline);
		if block_deadline != sched::NO_DEADLINE && arch::apic::ticks() >= block_deadline {
			return ERR_TIMED_OUT;
		}
		if group_koids.is_empty() {
			sched::block_on_flagged(koid, block_deadline, periodic, || object_ready_for(&object, writable));
		} else {
			sched::block_on_any(&group_koids, block_deadline, periodic, || object_ready_for(&object, writable));
		}
	}
}

// Block until ANY handle in the caller's array `[handles_ptr; count]` is ready,
// returning that handle's index, or ERR_TIMED_OUT at `deadline` (absolute ticks,
// 0 = none). Like `wait` but over a set: a driver waits on its device interrupt and
// a control channel at once, waking on whichever is ready first. Each handle needs
// the WAIT right. `flags` may carry WAIT_PERIODIC (see sys_wait).
fn sys_wait_any(handles_ptr: u64, count: u64, deadline: u64, flags: u64) -> i64 {
	// A fixed ceiling first, before the caller's holdings are consulted: the handle limit
	// is itself reachable past its bound through `SYS_HANDLE_DUPLICATE`, so "as many as
	// you hold" is not a bound.
	if count as usize > abi::MAX_WAIT_HANDLES {
		return ERR_INVALID;
	}
	let periodic = flags & abi::WAIT_PERIODIC != 0;
	let thread = current_thread!();
	let n = count as usize;
	// A wait set cannot name more handles than the caller's domain actually holds:
	// every entry must resolve in the handle table below, and the domain's live
	// handle count bounds the table. This is the real bound on the caller's array
	// (the scratch tables live on the kernel heap, so it is not a memory cap).
	let held = thread.process().domain().account().handles().used();
	// A fixed ceiling as well as the caller's holding: the handle limit is reachable past
	// its own bound through `SYS_HANDLE_DUPLICATE`, so "as many as you hold" is not a bound.
	if n == 0 || n as u64 > held || !user_buf_ok(handles_ptr, count * 8) {
		return ERR_INVALID;
	}
	// Copy the caller's handle array into a kernel buffer through the sanctioned
	// SMAP window before resolving it.
	let Some(mut raw) = try_zeroed_u64(n) else {
		return ERR_NO_MEMORY;
	};
	// All of it: a short read leaves zeros, and waiting on handle 0 is not what the caller asked
	// for - it is waiting on something else entirely, or on nothing.
	if let Err(error) = copy_from_user_exact(raw.as_mut_ptr() as *mut u8, handles_ptr, n * 8) {
		return error;
	}
	// Resolve every handle once up front, recording each object and its koid. The
	// scratch tables are heap-allocated, sized by the actual set - `wait_any` is a
	// blocking call, so the allocation is never on a hot path.
	// FALLIBLY, like every other buffer in this function - `try_zeroed_u64` is used twice below and
	// this was the odd one out. `resize_with` aborts on a short heap, sized by `n` up to
	// `MAX_WAIT_HANDLES`.
	let mut objects: alloc::vec::Vec<Option<Arc<dyn KernelObject>>> = alloc::vec::Vec::new();
	if objects.try_reserve_exact(n).is_err() {
		return ERR_NO_MEMORY;
	}
	objects.resize_with(n, || None);
	let mut koids: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
	if koids.try_reserve(n).is_err() {
		return ERR_NO_MEMORY;
	}
	{
		let table = thread.handles().lock();
		for i in 0..n {
			let object = match table.lookup(Handle::from_raw(raw[i]), Rights::WAIT) {
				Ok(o) => o,
				Err(HandleError::AccessDenied) => return ERR_ACCESS_DENIED,
				Err(_) => return ERR_BAD_HANDLE,
			};
			koids.push(object.header().koid());
			// A PROCESSGROUP IS WOKEN BY ITS MEMBERS, and they wake their own koids.
			//
			// `SYS_WAIT` learned this and this call did not, which was invisible for as long as
			// nothing waited on a group here - and then the shell started waiting on `[group,
			// control]` for a foreground pipeline and never woke: the pipeline finished, the group
			// reported itself ready on the next re-check, and no re-check ever came. The prompt
			// never returned.
			//
			// Registering on the members as well needs no back-link from a process to its groups:
			// the wake it already sends is the one being listened for. FALLIBLY, because the
			// membership is a length a ring-3 caller chose.
			if let Some(group) = object.as_any().downcast_ref::<crate::object::process_group::ProcessGroup>() {
				let mut live: [Option<alloc::sync::Arc<Process>>; crate::object::process_group::MAX_GROUP_MEMBERS] = [const { None }; _];
				let count = group.live_into(&mut live);
				if koids.try_reserve(count).is_err() {
					return ERR_NO_MEMORY;
				}
				koids.extend(live.iter().take(count).filter_map(|member| member.as_ref()).map(|member| member.header().koid()));
			}
			objects[i] = Some(object);
		}
	}
	// Condition-variable loop: re-check every object after each wake, blocking on the
	// whole set until one is ready or the deadline passes.
	loop {
		if thread.process().is_killed() {
			for slot in objects.iter_mut() {
				slot.take();
			}
			sched::exit();
		}
		if thread.process().is_stopped() {
			sched::block_on(thread.process().header().koid(), sched::NO_DEADLINE);
			continue;
		}
		// As in `sys_wait`: a caught interrupt ends the wait rather than being slept through. An
		// ordinary return, so `objects` is dropped on the way out - the manual clearing above
		// belongs to the kill path, which never returns at all.
		//
		// AHEAD OF THE READINESS SCAN, deliberately: a message that is already queued stays queued,
		// so a caller that decides to carry on rather than exit takes it on the next pass. Nothing
		// is lost by answering the interrupt first, and a program that never gets to hear about it
		// while its channel keeps producing is the shape of "Ctrl+C does nothing" all over again.
		if thread.process().take_int_report() {
			return ERR_INTERRUPTED;
		}
		for (i, slot) in objects.iter().enumerate().take(n) {
			if let Some(object) = slot {
				if object_ready(object) {
					return i as i64;
				}
			}
		}
		// The EARLIEST of the caller's deadline and every armed timer in the set.
		//
		// Single `SYS_WAIT` has done this since timers learned to wake their waiters; this took the
		// caller's deadline alone. Reaching a timer's deadline generates no wake of its own - the
		// timer becomes ready, and nobody is told - so a wait on an armed timer with no deadline of
		// its own slept past the expiry with nothing to bring it back. A driver waiting on its
		// interrupt and a watchdog together is exactly this set.
		let mut block_deadline = if deadline == 0 { sched::NO_DEADLINE } else { deadline };
		for slot in objects.iter().take(n) {
			if let Some(object) = slot {
				block_deadline = core::cmp::min(block_deadline, wait_block_deadline(object, deadline));
			}
		}
		if block_deadline != sched::NO_DEADLINE && arch::apic::ticks() >= block_deadline {
			return ERR_TIMED_OUT;
		}
		sched::block_on_any(&koids, block_deadline, periodic, || objects.iter().take(n).any(|slot| slot.as_ref().is_some_and(|o| object_ready(o))));
	}
}

// Create a wait set: a set of objects the kernel KEEPS, registered once and waited on many times.
//
// `SYS_WAIT_ANY` takes a fresh array on every call and registers a waiter on every object in it, so
// one pass costs the caller a lock and a list insertion per object it listens to - every pass, for
// as long as it runs. A set pays that once per member, when the member joins.
fn sys_waitset_create() -> i64 {
	let thread = current_thread!();
	let Some(set) = WaitSet::create_in(thread.domain().clone()) else { return ERR_RESOURCE_EXHAUSTED };
	install_object(&thread, set, Rights::ALL)
}

// Add the object behind `object_handle` to the set behind `set_handle`.
//
// Needs WAIT on the object - the same right waiting on it directly needs, because that is what this
// is - and MANAGE on the set.
fn sys_waitset_add(set_handle: u64, object_handle: u64) -> i64 {
	let set = match current_typed::<WaitSet>(set_handle, ObjectType::WaitSet, Rights::MANAGE) {
		Ok(o) => o,
		Err(e) => return e,
	};
	// Untyped: a set watches whatever can be waited on, so the type check is `object_ready`'s job
	// rather than a list of types kept here and drifting from it.
	let object = match untyped_object(object_handle, Rights::WAIT) {
		Ok(o) => o,
		Err(e) => return e,
	};
	// The koid the member joined under, so a caller learns it here rather than paying
	// `SYS_OBJECT_INFO_GET` per member to find out what `SYS_WAITSET_WAIT` will answer with.
	// Userspace CAN ask - `ObjectInfo` carries `koid` - and should not have to.
	let koid = object.header().koid();
	match set.add(object) {
		Ok(()) => koid as i64,
		Err(WaitSetError::Full | WaitSetError::TooManySets) => ERR_RESOURCE_EXHAUSTED,
		Err(_) => ERR_INVALID,
	}
}

// Take an object out of the set. Named by its handle, like everything else, and matched by koid -
// so a caller may remove a member through any handle it holds to the same object.
fn sys_waitset_remove(set_handle: u64, koid: u64) -> i64 {
	let set = match current_typed::<WaitSet>(set_handle, ObjectType::WaitSet, Rights::MANAGE) {
		Ok(o) => o,
		Err(e) => return e,
	};
	// BY KOID, which is what `add` returned and what `wait` answers with.
	//
	// It used to take the object's HANDLE and resolve it only to read the koid off it - so removing
	// a member required still holding the handle it joined under, and the one thing a caller most
	// wants to do with a dead peer is close it. Closing first left the member in the set, and a
	// closed peer is permanently readable, so the set woke on it forever: 12,917 wakes in three
	// minutes, which is how this was found. The answer was to order the two operations at four call
	// sites, and ordering is a rule people follow rather than one the interface keeps.
	//
	// Naming a koid is not more authority than naming a handle. The set handle carries MANAGE, the
	// koid can only match a member of THAT set, and joining it required a handle with WAIT in the
	// first place. What is removed is a membership, not an object.
	match set.remove(koid) {
		Ok(()) => 0,
		Err(_) => ERR_INVALID,
	}
}

// Block until any member of the set is ready, returning its INDEX in the set, or ERR_TIMED_OUT at
// `deadline` (absolute ticks, 0 = none).
//
// The index is into the set's current membership, which the caller decides - so it is stable as long
// as the caller does not add or remove, and a caller that does knows it did. The alternative, a
// koid, would make every wake a lookup.
fn sys_waitset_wait(set_handle: u64, deadline: u64, flags: u64) -> i64 {
	let thread = current_thread!();
	let set = match current_typed::<WaitSet>(set_handle, ObjectType::WaitSet, Rights::WAIT) {
		Ok(o) => o,
		Err(e) => return e,
	};
	let periodic = flags & abi::WAIT_PERIODIC != 0;
	let set_koid = set.header().koid();
	loop {
		if thread.process().is_killed() {
			sched::exit();
		}
		if thread.process().is_stopped() {
			sched::block_on(thread.process().header().koid(), sched::NO_DEADLINE);
			continue;
		}
		// One pass over the membership, under one lock, allocating nothing: the ready member if
		// there is one, and the earliest deadline in the set either way.
		//
		// The earliest deadline is the same rule `sys_wait_any` follows - a timer becoming ready
		// generates no wake of its own, so a wait that did not account for one could sleep past it
		// with nothing to bring it back.
		let (ready, block_deadline) = set.with_members(|members| {
			let mut earliest = if deadline == 0 { sched::NO_DEADLINE } else { deadline };
			let mut ready = None;
			for object in members.iter() {
				if ready.is_none() && object_ready(object) {
					// The member's KOID, not its index in the set.
					//
					// An index only means something to a caller keeping a list in exactly the
					// kernel's order - and the kernel removes a member by retaining the others,
					// while a service's client table uses `swap_remove`, which permutes. That
					// mismatch is what forced the first migration attempt to RECONCILE membership
					// every pass instead of editing it where it changes, and the reconcile was
					// measured as the whole cost: 433,645 ns at sixty-two clients against a
					// 188,821 ns baseline, with the set populated but not waited on still costing
					// 425,281. The kernel-side theory was fine; the userspace bookkeeping the index
					// forced was quadratic.
					//
					// A koid needs no mirror at all: the caller maps it to a client however it
					// already indexes them. Koids come from a counter that starts at 1 and only
					// increases, so one always fits in the `i64` a syscall returns with negatives
					// reserved for errors - worth saying because the index it replaces could not
					// have collided with an error code either, and that property should not be
					// assumed a second time.
					ready = Some(object.header().koid() as i64);
				}
				earliest = core::cmp::min(earliest, wait_block_deadline(object, deadline));
			}
			(ready, earliest)
		});
		if let Some(koid) = ready {
			return koid;
		}
		if block_deadline != sched::NO_DEADLINE && arch::apic::ticks() >= block_deadline {
			return ERR_TIMED_OUT;
		}
		// ONE registration, on the set. What makes this the point of the whole object: a member's
		// wake reaches the set through the observer registered when it joined, and the set's wake
		// reaches whoever is parked here.
		sched::block_on_flagged(set_koid, block_deadline, periodic, || set.with_members(|members| members.iter().any(object_ready)));
	}
}

// Whether the waitable object behind a handle is currently ready. A non-waitable
// object type is never ready (the wait would block until its deadline).
fn object_ready(object: &Arc<dyn KernelObject>) -> bool {
	object_ready_for(object, false)
}

// object_ready with the WAIT_WRITABLE sense: for a Channel, `writable` asks
// whether a send would find room (the sender's half of backpressure) instead of
// whether a recv would find a message. Other object types ignore the flag.
fn object_ready_for(object: &Arc<dyn KernelObject>, writable: bool) -> bool {
	let any = object.as_any();
	if let Some(channel) = any.downcast_ref::<Channel>() {
		return if writable { channel.is_writable() } else { channel.is_readable() };
	}
	if let Some(event) = any.downcast_ref::<Event>() {
		return event.is_signaled();
	}
	if let Some(timer) = any.downcast_ref::<Timer>() {
		return timer.is_expired();
	}
	if let Some(interrupt) = any.downcast_ref::<Interrupt>() {
		return interrupt.is_pending();
	}
	if let Some(claim) = any.downcast_ref::<Claim>() {
		// A claim handle becomes ready once its release has SETTLED, so a manager parked in
		// `wait_any` learns the device is back - `Free`, or `Quarantined` where the teardown could
		// not be confirmed - without polling for it. It sits in the wait set beside the driver's
		// process handle, which is the other half of the same question.
		return claim.is_settled();
	}
	if let Some(process) = any.downcast_ref::<Process>() {
		// A Process handle becomes ready once the process has terminated (exited or
		// been killed), so a holder can wait for a child to finish - the kernel's
		// process-terminated signal.
		return process.is_terminated();
	}
	if let Some(group) = any.downcast_ref::<crate::object::process_group::ProcessGroup>() {
		// A group is ready once EVERY member has terminated, which is what makes a pipeline
		// waitable as one thing: a job stays a job until its last stage is gone, so a shell
		// cannot announce a pipeline finished while a stage is still running.
		//
		// Without this arm a group handle was never ready, so a caller polling one waited
		// forever - which is how a pipeline job would have failed to reap.
		return group.finished();
	}
	false
}

// The tick deadline to block until: the caller's timeout (0 = none) capped by an
// armed Timer's own deadline, so a wait on a timer wakes in time to observe it
// expire.
fn wait_block_deadline(object: &Arc<dyn KernelObject>, deadline: u64) -> u64 {
	let caller = if deadline == 0 { sched::NO_DEADLINE } else { deadline };
	if let Some(timer) = object.as_any().downcast_ref::<Timer>() {
		if let Some(timer_deadline) = timer.deadline() {
			return core::cmp::min(caller, timer_deadline);
		}
	}
	caller
}

// Copy the current process's recorded fault into the caller's buffer. Returns 1
// if a fault was recorded and copied, 0 if none was recorded, or an error. Lets a
// supervisor inspect why a process was terminated.
fn sys_fault_info_get(buf_ptr: u64, buf_len: u64) -> i64 {
	let thread = current_thread!();
	let info = match thread.process().fault_info() {
		Some(i) => i,
		None => return 0,
	};
	let size = core::mem::size_of::<FaultInfo>() as u64;
	if buf_len < size || !user_buf_ok(buf_ptr, size) {
		return ERR_INVALID;
	}
	if let Err(e) = write_user(buf_ptr, info) {
		return e;
	}
	1
}

// Introspect a handle in the caller's table: write an ObjectInfo describing the
// object behind it (koid, type, rights, generation, byte size for memory-backed
// objects) into the caller's buffer. Returns 1 on success, ERR_BAD_HANDLE for an
// unknown/stale handle, or ERR_INVALID if the buffer is too small or out of range.
fn sys_object_info_get(handle: u64, buf_ptr: u64, buf_len: u64) -> i64 {
	let thread = current_thread!();
	let (info, object) = {
		let table = thread.handles().lock();
		let info = match table.info(Handle::from_raw(handle)) {
			Some(i) => i,
			None => return ERR_BAD_HANDLE,
		};
		let object = match table.lookup(Handle::from_raw(handle), Rights::NONE) {
			Ok(o) => o,
			Err(_) => return ERR_BAD_HANDLE,
		};
		(info, object)
	};
	let size = core::mem::size_of::<ObjectInfo>() as u64;
	if buf_len < size || !user_buf_ok(buf_ptr, size) {
		return ERR_INVALID;
	}
	// The real byte size of memory-backed objects, so a service can validate a
	// claimed transfer length against the object itself; 0 for other types.
	let obj_size = match info.object_type {
		ObjectType::MemoryObject => object.as_any().downcast_ref::<MemoryObject>().map_or(0, |m| m.size() as u64),
		ObjectType::DmaBuffer => object.as_any().downcast_ref::<DmaBuffer>().map_or(0, |d| d.size() as u64),
		_ => 0,
	};
	let out = ObjectInfo { koid: info.koid, object_type: info.object_type.code(), rights: info.rights.bits(), generation: info.generation, size: obj_size };
	if let Err(e) = write_user(buf_ptr, out) {
		return e;
	}
	1
}

// Read live per-process counters and state for a Process handle: write a ProcessStats
// (IPC volume, handle and memory usage, liveness) into the caller's buffer. Requires
// the READ right on the Process handle. The liveness state is derived from the live
// process - a fault or kill is FAILED, a process whose threads have all exited is
// STOPPED, an otherwise-running process is RUNNING - so a SystemGraphService holding a
// component's process handle sees its crash / stop at the next snapshot. Returns 1 on
// success, the usual handle / argument errors otherwise.
fn sys_process_stats_get(handle: u64, buf_ptr: u64, buf_len: u64) -> i64 {
	let process = match current_typed::<Process>(handle, ObjectType::Process, Rights::READ) {
		Ok(p) => p,
		Err(e) => return e,
	};
	let size = core::mem::size_of::<ProcessStats>() as u64;
	if buf_len < size || !user_buf_ok(buf_ptr, size) {
		return ERR_INVALID;
	}
	let state = if process.is_killed() {
		PROC_STATE_FAILED
	} else if process.live_thread_count() == 0 {
		PROC_STATE_STOPPED
	} else {
		PROC_STATE_RUNNING
	};
	let (completion, completion_valid) = match process.exit_status() {
		Some(status) => (status, 1),
		None => (0, 0),
	};
	let out = ProcessStats { messages_sent: process.messages_sent(), messages_received: process.messages_received(), handle_count: process.handle_count(), memory_bytes: process.memory_bytes(), state, completion, completion_valid };
	if let Err(e) = write_user(buf_ptr, out) {
		return e;
	}
	1
}

// Create a child Domain of the caller's Domain with the given resource caps and
// install a handle to it in the caller's table. The child's limits bind in
// addition to every ancestor's, so a subdomain can only subdivide its parent's
// budget, never exceed it. a0/a1/a2 are the memory/handle/thread caps.
fn sys_domain_create(memory_limit: u64, handle_limit: u64, thread_limit: u64) -> i64 {
	let thread = current_thread!();
	// A parent that is being killed does not get new children: the kill walks a snapshot, so one
	// created after it was taken would outlive the domain it belongs to.
	let Some(child) = Domain::new_child(thread.domain(), memory_limit, handle_limit, thread_limit) else {
		return ERR_INVALID;
	};
	install_object(&thread, child, Rights::ALL)
}

// Read the live resource counters of the Domain named by `handle` into the caller's
// buffer (a DomainStats): the used and limit of memory, handles, threads, IPC queue
// bytes and DMA. Requires the READ right, so a ResourceManager that holds a Domain
// can observe its usage against the budgets it set without the governed component
// reporting them.
fn sys_domain_stats_get(handle: u64, buf_ptr: u64, buf_len: u64) -> i64 {
	let domain = match current_typed::<Domain>(handle, ObjectType::Domain, Rights::READ) {
		Ok(o) => o,
		Err(e) => return e,
	};
	let size = core::mem::size_of::<DomainStats>() as u64;
	if buf_len < size || !user_buf_ok(buf_ptr, size) {
		return ERR_INVALID;
	}
	let out = domain_stats_snapshot(&domain);
	if let Err(e) = write_user(buf_ptr, out) {
		return e;
	}
	1
}

pub(crate) fn domain_stats_snapshot(domain: &Domain) -> DomainStats {
	let account = domain.account();
	DomainStats { memory_used: account.memory().used(), memory_peak: account.memory().peak(), memory_limit: account.memory().limit(), handles_used: account.handles().used(), handles_limit: account.handles().limit(), threads_used: account.threads().used(), threads_limit: account.threads().limit(), ipc_used: account.ipc_queue().used(), ipc_limit: account.ipc_queue().limit(), dma_used: account.dma().used(), dma_limit: account.dma().limit(), stack_used: account.stack().used(), stack_limit: account.stack().limit() }
}

// Kill the Domain named by `handle` and its whole subtree: every descendant
// process is terminated and its resources freed. Requires the MANAGE right.
fn sys_domain_kill(handle: u64) -> i64 {
	let domain = match current_typed::<Domain>(handle, ObjectType::Domain, Rights::MANAGE) {
		Ok(o) => o,
		Err(e) => return e,
	};
	domain.kill();
	0
}

// Create an Event and install a handle to it in the caller's table.
fn sys_event_create() -> i64 {
	let thread = current_thread!();
	let Some(event) = Event::create() else { return ERR_RESOURCE_EXHAUSTED };
	install_object(&thread, event, Rights::ALL)
}

// Raise an event's signal.
fn sys_event_signal(handle: u64) -> i64 {
	let event = match current_typed::<Event>(handle, ObjectType::Event, Rights::WRITE) {
		Ok(o) => o,
		Err(e) => return e,
	};
	event.signal();
	0
}

// Observe an event's signal: 1 if signaled, 0 if not.
fn sys_event_poll(handle: u64) -> i64 {
	let event = match current_typed::<Event>(handle, ObjectType::Event, Rights::READ) {
		Ok(o) => o,
		Err(e) => return e,
	};
	i64::from(event.is_signaled())
}

// Create a Timer and install a handle to it in the caller's table.
fn sys_timer_create() -> i64 {
	let thread = current_thread!();
	let Some(timer) = Timer::create() else { return ERR_RESOURCE_EXHAUSTED };
	install_object(&thread, timer, Rights::ALL)
}

// Arm a timer to fire at an absolute tick deadline.
fn sys_timer_set(handle: u64, deadline_ticks: u64) -> i64 {
	let timer = match current_typed::<Timer>(handle, ObjectType::Timer, Rights::WRITE) {
		Ok(o) => o,
		Err(e) => return e,
	};
	timer.set(deadline_ticks);
	0
}

// Observe a timer: 1 if armed and expired, 0 otherwise.
fn sys_timer_poll(handle: u64) -> i64 {
	let timer = match current_typed::<Timer>(handle, ObjectType::Timer, Rights::READ) {
		Ok(o) => o,
		Err(e) => return e,
	};
	i64::from(timer.is_expired())
}
