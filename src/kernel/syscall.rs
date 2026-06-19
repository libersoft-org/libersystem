// System call dispatch and the minimal syscall set.
//
// The architecture entry stub (arch::syscall) calls syscall_dispatch with the
// syscall number and up to four arguments; this module decodes the number and
// runs the matching handler. Handlers that touch per-process state (handles,
// objects, mappings) operate on the calling thread's handle table.
//
// Return convention (Linux-style): a successful call returns its result value
// (a handle, an address, a count, ...). An error returns a small negative value
// in the range [-4095, -1]; sys_is_err() tests for it. This lets a syscall return
// a higher-half kernel address - whose top bit is set - without it being mistaken
// for an error.

#![allow(dead_code)]

use core::sync::atomic::{AtomicU64, Ordering};

use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::memlayout::{KERNEL_MMAP_BASE, USER_MMAP_BASE, USER_VA_END};

use crate::arch;
use crate::fault::FaultInfo;
use crate::mem::frame::PAGE_SIZE;
use crate::object::channel::{Channel, ChannelError, Message};
use crate::object::domain::Domain;
use crate::object::event::Event;
use crate::object::handle::{Capability, Handle, HandleError};
use crate::object::memory_object::{MemoryError, MemoryObject};
use crate::object::rights::Rights;
use crate::object::timer::Timer;
use crate::object::{KernelObject, ObjectType};
use crate::sched;

// The syscall numbers, error codes, and the sys_is_err helper are the shared
// kernel/userspace ABI: defined once in the abi crate (the single source of
// truth) and re-exported here so the rest of the kernel keeps referring to them
// as `syscall::SYS_*` / `syscall::ERR_*` / `syscall::sys_is_err`.
pub use abi::{ERR_ACCESS_DENIED, ERR_BAD_HANDLE, ERR_BAD_SYSCALL, ERR_INVALID, ERR_NO_MEMORY, ERR_NO_THREAD, ERR_NOT_MAPPED, ERR_PEER_CLOSED, ERR_RESOURCE_EXHAUSTED, ERR_TIMED_OUT, ERR_WOULD_BLOCK, SYS_CHANNEL_CREATE, SYS_CHANNEL_RECV, SYS_CHANNEL_SEND, SYS_CLOCK_GET, SYS_DEBUG_NOOP, SYS_DEBUG_WRITE, SYS_DOMAIN_CREATE, SYS_DOMAIN_KILL, SYS_EVENT_CREATE, SYS_EVENT_POLL, SYS_EVENT_SIGNAL, SYS_FAULT_INFO_GET, SYS_HANDLE_CLOSE, SYS_HANDLE_DUPLICATE, SYS_MEMORY_MAP, SYS_MEMORY_OBJECT_CREATE, SYS_MEMORY_UNMAP, SYS_OBJECT_INFO_GET, SYS_TIMER_CREATE, SYS_TIMER_POLL, SYS_TIMER_SET, SYS_USER_EXIT, SYS_WAIT, SYS_YIELD, sys_is_err};

// Introspection record filled by object_info_get: the identity and type of the
// object behind a handle, and the access the handle confers. repr(C) with only
// fixed-width fields so it marshals cleanly across the syscall boundary.
#[repr(C)]
pub struct ObjectInfo {
	pub koid: u64,
	pub object_type: u64,
	pub rights: u32,
	pub generation: u32,
}

// Validate a caller-supplied buffer. Always accepts kernel self-calls; for a
// ring-3 caller it requires the whole [ptr, ptr+len) range to lie in user space.
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
	match ptr.checked_add(len) {
		Some(end) => end <= USER_VA_END,
		None => false,
	}
}

// Kernel virtual-address window for syscall-mapped MemoryObjects. A bump pointer
// hands out non-overlapping ranges; M6 does not reclaim this address space.
static MMAP_NEXT: AtomicU64 = AtomicU64::new(KERNEL_MMAP_BASE);

fn alloc_kernel_vrange(len: u64) -> u64 {
	MMAP_NEXT.fetch_add(len, Ordering::Relaxed)
}

// User virtual-address window for ring-3 syscall-mapped MemoryObjects. Like the
// kernel window a bump pointer hands out non-overlapping ranges and does not
// reclaim them. Each process maps into its own page tables, so the same range is
// private per address space even though the bump is global.
static USER_MMAP_NEXT: AtomicU64 = AtomicU64::new(USER_MMAP_BASE);

fn alloc_user_vrange(len: u64) -> u64 {
	USER_MMAP_NEXT.fetch_add(len, Ordering::Relaxed)
}

// Entry point called by the architecture syscall stub. `num` selects the call;
// the meaning of the arguments and the return value is per-syscall.
#[unsafe(no_mangle)]
pub extern "C" fn syscall_dispatch(num: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> u64 {
	let result: i64 = match num {
		SYS_DEBUG_NOOP => a0 as i64,
		SYS_CLOCK_GET => arch::apic::ticks() as i64,
		SYS_DEBUG_WRITE => {
			crate::serial_print!("{}", a0 as u8 as char);
			0
		}
		SYS_MEMORY_OBJECT_CREATE => sys_memory_object_create(a0),
		SYS_MEMORY_MAP => sys_memory_map(a0),
		SYS_MEMORY_UNMAP => sys_memory_unmap(a0),
		SYS_HANDLE_DUPLICATE => sys_handle_duplicate(a0, a1),
		SYS_HANDLE_CLOSE => sys_handle_close(a0),
		SYS_CHANNEL_CREATE => sys_channel_create(a0, a1),
		SYS_CHANNEL_SEND => sys_channel_send(a0, a1, a2, a3),
		SYS_CHANNEL_RECV => sys_channel_recv(a0, a1, a2, a3),
		SYS_EVENT_CREATE => sys_event_create(),
		SYS_EVENT_SIGNAL => sys_event_signal(a0),
		SYS_EVENT_POLL => sys_event_poll(a0),
		SYS_TIMER_CREATE => sys_timer_create(),
		SYS_TIMER_SET => sys_timer_set(a0, a1),
		SYS_TIMER_POLL => sys_timer_poll(a0),
		SYS_USER_EXIT => arch::usermode::exit_to_kernel(),
		SYS_FAULT_INFO_GET => sys_fault_info_get(a0, a1),
		SYS_DOMAIN_CREATE => sys_domain_create(a0, a1, a2),
		SYS_DOMAIN_KILL => sys_domain_kill(a0),
		SYS_YIELD => {
			sched::yield_now();
			0
		}
		SYS_OBJECT_INFO_GET => sys_object_info_get(a0, a1, a2),
		SYS_WAIT => sys_wait(a0, a1),
		_ => ERR_BAD_SYSCALL,
	};
	result as u64
}

// Create a MemoryObject and install a handle to it in the caller's table.
fn sys_memory_object_create(size: u64) -> i64 {
	let thread = match sched::current_thread() {
		Some(t) => t,
		None => return ERR_NO_THREAD,
	};
	// Charge the physical memory to the caller's Domain at the create boundary.
	let object = match MemoryObject::create_in(thread.domain(), size as usize) {
		Ok(o) => o,
		Err(MemoryError::QuotaExceeded) => return ERR_RESOURCE_EXHAUSTED,
		Err(MemoryError::OutOfMemory) => return ERR_NO_MEMORY,
	};
	let installed = thread.handles().lock().try_insert_object(object, Rights::ALL, 0);
	match installed {
		Some(handle) => handle.raw() as i64,
		None => ERR_RESOURCE_EXHAUSTED,
	}
}

// Map a MemoryObject into the kernel address space, returning its virtual base.
fn sys_memory_map(handle: u64) -> i64 {
	let thread = match sched::current_thread() {
		Some(t) => t,
		None => return ERR_NO_THREAD,
	};
	let object = {
		let table = thread.handles().lock();
		match table.lookup_typed(Handle::from_raw(handle), ObjectType::MemoryObject, Rights::MAP) {
			Ok(o) => o,
			Err(_) => return ERR_BAD_HANDLE,
		}
	};
	let memory = object.as_any().downcast_ref::<MemoryObject>().expect("type checked by lookup_typed");
	if memory.mapped_at() != 0 {
		return ERR_INVALID;
	}
	// A ring-3 caller maps into its own (lower-half) user space with the USER bit
	// so the program can reach the pages; a ring-0 caller maps into the shared
	// kernel window. Either way the active page tables are the caller's, so a
	// plain map_page lands in the right address space.
	let user = arch::percpu::in_user_syscall();
	let base = if user { alloc_user_vrange(memory.size() as u64) } else { alloc_kernel_vrange(memory.size() as u64) };
	let flags = if user { arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER } else { arch::paging::PRESENT | arch::paging::WRITABLE };
	for (i, &phys) in memory.frames().iter().enumerate() {
		arch::paging::map_page(base + i as u64 * PAGE_SIZE, phys, flags);
	}
	memory.set_mapped_at(base);
	base as i64
}

// Remove a MemoryObject's mapping from the kernel address space.
fn sys_memory_unmap(handle: u64) -> i64 {
	let thread = match sched::current_thread() {
		Some(t) => t,
		None => return ERR_NO_THREAD,
	};
	let object = {
		let table = thread.handles().lock();
		match table.lookup_typed(Handle::from_raw(handle), ObjectType::MemoryObject, Rights::MAP) {
			Ok(o) => o,
			Err(_) => return ERR_BAD_HANDLE,
		}
	};
	let memory = object.as_any().downcast_ref::<MemoryObject>().expect("type checked by lookup_typed");
	let base = memory.mapped_at();
	if base == 0 {
		return ERR_NOT_MAPPED;
	}
	for i in 0..memory.frames().len() {
		arch::paging::unmap_page(base + i as u64 * PAGE_SIZE);
	}
	memory.set_mapped_at(0);
	0
}

// Derive a weaker handle to the same object (attenuation only).
fn sys_handle_duplicate(handle: u64, rights_bits: u64) -> i64 {
	let thread = match sched::current_thread() {
		Some(t) => t,
		None => return ERR_NO_THREAD,
	};
	let new_rights = Rights::from_bits(rights_bits as u32);
	let mut table = thread.handles().lock();
	match table.duplicate(Handle::from_raw(handle), new_rights) {
		Ok(h) => h.raw() as i64,
		Err(HandleError::AccessDenied) => ERR_ACCESS_DENIED,
		Err(_) => ERR_BAD_HANDLE,
	}
}

// Close a handle in the caller's table.
fn sys_handle_close(handle: u64) -> i64 {
	let thread = match sched::current_thread() {
		Some(t) => t,
		None => return ERR_NO_THREAD,
	};
	let mut table = thread.handles().lock();
	match table.close(Handle::from_raw(handle)) {
		Ok(()) => 0,
		Err(_) => ERR_BAD_HANDLE,
	}
}

// Copy a byte payload out of a caller-supplied buffer. M7 runs in ring 0, so the
// pointer is a kernel pointer used directly; userspace copy-in with validation is
// added with ring 3.
fn read_bytes(ptr: u64, len: usize) -> Vec<u8> {
	if ptr == 0 || len == 0 {
		return Vec::new();
	}
	let slice = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };
	slice.to_vec()
}

// Create a connected channel pair, install a handle to each endpoint in the
// caller's table, and write the two raw handles to *out0_ptr and *out1_ptr.
fn sys_channel_create(out0_ptr: u64, out1_ptr: u64) -> i64 {
	let thread = match sched::current_thread() {
		Some(t) => t,
		None => return ERR_NO_THREAD,
	};
	if out0_ptr == 0 || out1_ptr == 0 {
		return ERR_INVALID;
	}
	if !user_buf_ok(out0_ptr, 8) || !user_buf_ok(out1_ptr, 8) {
		return ERR_INVALID;
	}
	let (ep0, ep1) = Channel::create();
	let (h0, h1) = {
		let mut table = thread.handles().lock();
		// Enforce the Domain's handle quota for both endpoints; if the second
		// does not fit, roll the first back so neither is left half-created.
		let h0 = match table.try_insert_object(ep0, Rights::ALL, 0) {
			Some(h) => h,
			None => return ERR_RESOURCE_EXHAUSTED,
		};
		let h1 = match table.try_insert_object(ep1, Rights::ALL, 0) {
			Some(h) => h,
			None => {
				let _ = table.close(h0);
				return ERR_RESOURCE_EXHAUSTED;
			}
		};
		(h0, h1)
	};
	unsafe {
		(out0_ptr as *mut u64).write(h0.raw());
		(out1_ptr as *mut u64).write(h1.raw());
	}
	0
}

// Send a message (byte payload + optionally one transferred handle) to the peer.
// The message is stamped with the badge of the channel handle used. The
// transferred handle is consumed only on a successful send (left intact on
// failure, so the caller can retry on WOULD_BLOCK).
fn sys_channel_send(ch: u64, bytes_ptr: u64, bytes_len: u64, xfer: u64) -> i64 {
	let thread = match sched::current_thread() {
		Some(t) => t,
		None => return ERR_NO_THREAD,
	};
	let object = {
		let table = thread.handles().lock();
		match table.lookup_typed(Handle::from_raw(ch), ObjectType::Channel, Rights::SEND) {
			Ok(o) => o,
			Err(HandleError::AccessDenied) => return ERR_ACCESS_DENIED,
			Err(_) => return ERR_BAD_HANDLE,
		}
	};
	let channel = object.as_any().downcast_ref::<Channel>().expect("type checked by lookup_typed");
	let badge = thread.handles().lock().badge_of(Handle::from_raw(ch)).unwrap_or(0);
	if !user_buf_ok(bytes_ptr, bytes_len) {
		return ERR_INVALID;
	}
	let bytes = read_bytes(bytes_ptr, bytes_len as usize);
	// Build the capability to transfer, if any, without yet removing the handle.
	let caps = if xfer != 0 {
		let table = thread.handles().lock();
		let xobject = match table.lookup(Handle::from_raw(xfer), Rights::TRANSFER) {
			Ok(o) => o,
			Err(HandleError::AccessDenied) => return ERR_ACCESS_DENIED,
			Err(_) => return ERR_BAD_HANDLE,
		};
		let rights = table.rights_of(Handle::from_raw(xfer)).unwrap_or(Rights::NONE);
		let xbadge = table.badge_of(Handle::from_raw(xfer)).unwrap_or(0);
		alloc::vec![Capability::new(xobject, rights, xbadge)]
	} else {
		Vec::new()
	};
	match channel.send(Message::new(bytes, caps, badge)) {
		Ok(()) => {
			// Delivered: now consume the transferred handle.
			if xfer != 0 {
				let _ = thread.handles().lock().close(Handle::from_raw(xfer));
			}
			0
		}
		Err(ChannelError::Full) => ERR_WOULD_BLOCK,
		Err(ChannelError::PeerClosed) => ERR_PEER_CLOSED,
		Err(_) => ERR_INVALID,
	}
}

// Receive a message: copy up to `bytes_cap` payload bytes to `bytes_ptr` and, if
// the message carried a transferred capability, install it and write the new
// handle to *out_handle_ptr (0 if none). Returns the payload byte count.
fn sys_channel_recv(ch: u64, bytes_ptr: u64, bytes_cap: u64, out_handle_ptr: u64) -> i64 {
	let thread = match sched::current_thread() {
		Some(t) => t,
		None => return ERR_NO_THREAD,
	};
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
	let message = match channel.recv() {
		Ok(m) => m,
		Err(ChannelError::Empty) => return ERR_WOULD_BLOCK,
		Err(ChannelError::PeerClosed) => return ERR_PEER_CLOSED,
		Err(_) => return ERR_INVALID,
	};
	let n = core::cmp::min(message.bytes.len(), bytes_cap as usize);
	if n > 0 && bytes_ptr != 0 {
		unsafe {
			core::ptr::copy_nonoverlapping(message.bytes.as_ptr(), bytes_ptr as *mut u8, n);
		}
	}
	// Install the transferred capability (if any) and report its new handle.
	if out_handle_ptr != 0 {
		let handle_value = match message.caps.into_iter().next() {
			Some(cap) => thread.handles().lock().insert(cap).raw(),
			None => 0,
		};
		unsafe {
			(out_handle_ptr as *mut u64).write(handle_value);
		}
	}
	n as i64
}

// Block the calling thread until the object behind `handle` becomes ready (a
// Channel readable, an Event signaled, a Timer expired) or `deadline` (an
// absolute LAPIC tick value; 0 = no timeout) passes. Returns 0 when the object
// became ready, ERR_TIMED_OUT on timeout. This is the kernel's one blocking
// primitive; the non-blocking send/recv/poll calls layer the synchronous-looking
// `call()` on top of it. The handle must carry the WAIT right.
fn sys_wait(handle: u64, deadline: u64) -> i64 {
	let thread = match sched::current_thread() {
		Some(t) => t,
		None => return ERR_NO_THREAD,
	};
	let object = {
		let table = thread.handles().lock();
		match table.lookup(Handle::from_raw(handle), Rights::WAIT) {
			Ok(o) => o,
			Err(HandleError::AccessDenied) => return ERR_ACCESS_DENIED,
			Err(_) => return ERR_BAD_HANDLE,
		}
	};
	let koid = object.header().koid();
	// Condition-variable loop: re-check readiness after each wake, so an early or
	// spurious wake just re-blocks and a deadline is honored on re-check.
	loop {
		if object_ready(&object) {
			return 0;
		}
		let block_deadline = wait_block_deadline(&object, deadline);
		if block_deadline != sched::NO_DEADLINE && arch::apic::ticks() >= block_deadline {
			return ERR_TIMED_OUT;
		}
		sched::block_on(koid, block_deadline);
	}
}

// Whether the waitable object behind a handle is currently ready. A non-waitable
// object type is never ready (the wait would block until its deadline).
fn object_ready(object: &Arc<dyn KernelObject>) -> bool {
	let any = object.as_any();
	if let Some(channel) = any.downcast_ref::<Channel>() {
		return channel.is_readable();
	}
	if let Some(event) = any.downcast_ref::<Event>() {
		return event.is_signaled();
	}
	if let Some(timer) = any.downcast_ref::<Timer>() {
		return timer.is_expired();
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
	let thread = match sched::current_thread() {
		Some(t) => t,
		None => return ERR_NO_THREAD,
	};
	let info = match thread.process().fault_info() {
		Some(i) => i,
		None => return 0,
	};
	let size = core::mem::size_of::<FaultInfo>() as u64;
	if buf_len < size || !user_buf_ok(buf_ptr, size) {
		return ERR_INVALID;
	}
	unsafe {
		(buf_ptr as *mut FaultInfo).write_unaligned(info);
	}
	1
}

// Introspect a handle in the caller's table: write an ObjectInfo describing the
// object behind it (koid, type, rights, generation) into the caller's buffer.
// Returns 1 on success, ERR_BAD_HANDLE for an unknown/stale handle, or
// ERR_INVALID if the buffer is too small or out of range.
fn sys_object_info_get(handle: u64, buf_ptr: u64, buf_len: u64) -> i64 {
	let thread = match sched::current_thread() {
		Some(t) => t,
		None => return ERR_NO_THREAD,
	};
	let info = match thread.handles().lock().info(Handle::from_raw(handle)) {
		Some(i) => i,
		None => return ERR_BAD_HANDLE,
	};
	let size = core::mem::size_of::<ObjectInfo>() as u64;
	if buf_len < size || !user_buf_ok(buf_ptr, size) {
		return ERR_INVALID;
	}
	let out = ObjectInfo { koid: info.koid, object_type: info.object_type.code(), rights: info.rights.bits(), generation: info.generation };
	unsafe {
		(buf_ptr as *mut ObjectInfo).write_unaligned(out);
	}
	1
}

// Create a child Domain of the caller's Domain with the given resource caps and
// install a handle to it in the caller's table. The child's limits bind in
// addition to every ancestor's, so a subdomain can only subdivide its parent's
// budget, never exceed it. a0/a1/a2 are the memory/handle/thread caps.
fn sys_domain_create(memory_limit: u64, handle_limit: u64, thread_limit: u64) -> i64 {
	let thread = match sched::current_thread() {
		Some(t) => t,
		None => return ERR_NO_THREAD,
	};
	let child = Domain::new_child(thread.domain(), memory_limit, handle_limit, thread_limit);
	let installed = thread.handles().lock().try_insert_object(child, Rights::ALL, 0);
	match installed {
		Some(handle) => handle.raw() as i64,
		None => ERR_RESOURCE_EXHAUSTED,
	}
}

// Kill the Domain named by `handle` and its whole subtree: every descendant
// process is terminated and its resources freed. Requires the MANAGE right.
fn sys_domain_kill(handle: u64) -> i64 {
	let thread = match sched::current_thread() {
		Some(t) => t,
		None => return ERR_NO_THREAD,
	};
	let object = {
		let table = thread.handles().lock();
		match table.lookup_typed(Handle::from_raw(handle), ObjectType::Domain, Rights::MANAGE) {
			Ok(o) => o,
			Err(HandleError::AccessDenied) => return ERR_ACCESS_DENIED,
			Err(_) => return ERR_BAD_HANDLE,
		}
	};
	object.as_any().downcast_ref::<Domain>().expect("type checked by lookup_typed").kill();
	0
}

// Create an Event and install a handle to it in the caller's table.
fn sys_event_create() -> i64 {
	let thread = match sched::current_thread() {
		Some(t) => t,
		None => return ERR_NO_THREAD,
	};
	let installed = thread.handles().lock().try_insert_object(Event::create(), Rights::ALL, 0);
	match installed {
		Some(handle) => handle.raw() as i64,
		None => ERR_RESOURCE_EXHAUSTED,
	}
}

// Raise an event's signal.
fn sys_event_signal(handle: u64) -> i64 {
	let thread = match sched::current_thread() {
		Some(t) => t,
		None => return ERR_NO_THREAD,
	};
	let object = {
		let table = thread.handles().lock();
		match table.lookup_typed(Handle::from_raw(handle), ObjectType::Event, Rights::WRITE) {
			Ok(o) => o,
			Err(HandleError::AccessDenied) => return ERR_ACCESS_DENIED,
			Err(_) => return ERR_BAD_HANDLE,
		}
	};
	object.as_any().downcast_ref::<Event>().expect("type checked by lookup_typed").signal();
	0
}

// Observe an event's signal: 1 if signaled, 0 if not.
fn sys_event_poll(handle: u64) -> i64 {
	let thread = match sched::current_thread() {
		Some(t) => t,
		None => return ERR_NO_THREAD,
	};
	let object = {
		let table = thread.handles().lock();
		match table.lookup_typed(Handle::from_raw(handle), ObjectType::Event, Rights::READ) {
			Ok(o) => o,
			Err(HandleError::AccessDenied) => return ERR_ACCESS_DENIED,
			Err(_) => return ERR_BAD_HANDLE,
		}
	};
	i64::from(object.as_any().downcast_ref::<Event>().expect("type checked by lookup_typed").is_signaled())
}

// Create a Timer and install a handle to it in the caller's table.
fn sys_timer_create() -> i64 {
	let thread = match sched::current_thread() {
		Some(t) => t,
		None => return ERR_NO_THREAD,
	};
	let installed = thread.handles().lock().try_insert_object(Timer::create(), Rights::ALL, 0);
	match installed {
		Some(handle) => handle.raw() as i64,
		None => ERR_RESOURCE_EXHAUSTED,
	}
}

// Arm a timer to fire at an absolute tick deadline.
fn sys_timer_set(handle: u64, deadline_ticks: u64) -> i64 {
	let thread = match sched::current_thread() {
		Some(t) => t,
		None => return ERR_NO_THREAD,
	};
	let object = {
		let table = thread.handles().lock();
		match table.lookup_typed(Handle::from_raw(handle), ObjectType::Timer, Rights::WRITE) {
			Ok(o) => o,
			Err(HandleError::AccessDenied) => return ERR_ACCESS_DENIED,
			Err(_) => return ERR_BAD_HANDLE,
		}
	};
	object.as_any().downcast_ref::<Timer>().expect("type checked by lookup_typed").set(deadline_ticks);
	0
}

// Observe a timer: 1 if armed and expired, 0 otherwise.
fn sys_timer_poll(handle: u64) -> i64 {
	let thread = match sched::current_thread() {
		Some(t) => t,
		None => return ERR_NO_THREAD,
	};
	let object = {
		let table = thread.handles().lock();
		match table.lookup_typed(Handle::from_raw(handle), ObjectType::Timer, Rights::READ) {
			Ok(o) => o,
			Err(HandleError::AccessDenied) => return ERR_ACCESS_DENIED,
			Err(_) => return ERR_BAD_HANDLE,
		}
	};
	i64::from(object.as_any().downcast_ref::<Timer>().expect("type checked by lookup_typed").is_expired())
}
