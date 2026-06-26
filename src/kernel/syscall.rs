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
use crate::device;
use crate::fault::FaultInfo;
use crate::loader::{self, LoadError};
use crate::mem::frame::PAGE_SIZE;
use crate::object::channel::{Channel, ChannelError, Message};
use crate::object::device_memory::DeviceMemory;
use crate::object::dma_buffer::DmaBuffer;
use crate::object::domain::Domain;
use crate::object::event::Event;
use crate::object::handle::{Capability, Handle, HandleError};
use crate::object::interrupt::Interrupt;
use crate::object::memory_object::{MemoryError, MemoryObject};
use crate::object::process::Process;
use crate::object::rights::Rights;
use crate::object::thread::Thread;
use crate::object::timer::Timer;
use crate::object::{KernelObject, ObjectType};
use crate::sched;

// The syscall numbers and error codes are the shared kernel/userspace ABI:
// defined once in the abi crate (the single source of truth) and re-exported
// here so the rest of the kernel keeps referring to them as `syscall::SYS_*` /
// `syscall::ERR_*`.
pub use abi::{ERR_ACCESS_DENIED, ERR_BAD_HANDLE, ERR_BAD_SYSCALL, ERR_INVALID, ERR_NO_MEMORY, ERR_NO_THREAD, ERR_NOT_MAPPED, ERR_PEER_CLOSED, ERR_RESOURCE_EXHAUSTED, ERR_TIMED_OUT, ERR_WOULD_BLOCK, PROP_DMA_LIMIT, PROP_HANDLE_LIMIT, PROP_IPC_QUEUE_LIMIT, PROP_MEMORY_LIMIT, PROP_NAME, PROP_THREAD_LIMIT, SIG_CONT, SIG_INT, SIG_KILL, SIG_STOP, SIG_TERM, SYS_CHANNEL_CREATE, SYS_CHANNEL_RECV, SYS_CHANNEL_SEND, SYS_CLOCK_GET, SYS_CLOCK_RTC, SYS_CONSOLE_ATTACH, SYS_CONSOLE_FEED, SYS_DEBUG_NOOP, SYS_DEBUG_WRITE, SYS_DEVICE_ACQUIRE, SYS_DEVICE_COUNT, SYS_DEVICE_INFO, SYS_DEVICE_MEMORY_MAP, SYS_DEVICE_MSIX_ACQUIRE, SYS_DMA_BUFFER_CREATE, SYS_DMA_BUFFER_MAP, SYS_DMA_BUFFER_PHYS, SYS_DOMAIN_CREATE, SYS_DOMAIN_KILL, SYS_EVENT_CREATE, SYS_EVENT_POLL, SYS_EVENT_SIGNAL, SYS_FAULT_INFO_GET, SYS_FRAMEBUFFER_MAP, SYS_HANDLE_CLOSE, SYS_HANDLE_DUPLICATE, SYS_INTERRUPT_ACK, SYS_INTERRUPT_BIND, SYS_MEMORY_MAP, SYS_MEMORY_OBJECT_CREATE, SYS_MEMORY_UNMAP, SYS_OBJECT_INFO_GET, SYS_OBJECT_PROPERTY_SET, SYS_PROCESS_CREATE, SYS_PROCESS_LOAD, SYS_PROCESS_SIGNAL, SYS_RANDOM_GET, SYS_SYSTEM_POWER, SYS_THREAD_CREATE, SYS_THREAD_START, SYS_TIMER_CREATE, SYS_TIMER_POLL, SYS_TIMER_SET, SYS_USER_EXIT, SYS_WAIT, SYS_WAIT_ANY, SYS_YIELD};

// The sys_is_err helper is only consumed by the in-kernel test harness.
#[cfg(test)]
pub use abi::sys_is_err;

// Introspection record filled by object_info_get: the identity and type of the
// object behind a handle, and the access the handle confers. Defined in `abi` (the
// SSOT shared with userspace) and re-exported here next to its syscall.
pub use abi::ObjectInfo;

// Validate a caller-supplied buffer. Always accepts kernel self-calls; for a
// ring-3 caller it requires the whole [ptr, ptr+len) range to lie in user space
// and every page it touches to be mapped in the active address space. The bounds
// check alone is not enough: a ring-3 caller can pass an in-bounds pointer to an
// unmapped page, and a kernel read or write of it then takes a ring-0 page fault.
// On the SYS_DEBUG_WRITE path that fault strikes while the serial TX lock is held,
// so the fault handler's own logging deadlocks on that lock and the machine hangs.
// There is no demand paging - any ring-3 fault terminates the process - so a valid
// buffer is always fully mapped; reject anything that is not.
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
	let mut page = ptr & !0xfff;
	let last = (end - 1) & !0xfff;
	loop {
		if arch::paging::translate(page).is_none() {
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
	let bytes = unsafe { core::slice::from_raw_parts(arg as *const u8, len as usize) };
	crate::_print_bytes(bytes);
	0
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

// Fetch the current thread and look up a typed object handle on its table,
// releasing the table lock before returning. Collapses the boilerplate shared by
// the handlers that only need the looked-up object: a missing thread maps to
// ERR_NO_THREAD, denied rights to ERR_ACCESS_DENIED, and a bad handle or wrong
// type to ERR_BAD_HANDLE.
fn current_object(handle: u64, ty: ObjectType, rights: Rights) -> Result<Arc<dyn KernelObject>, i64> {
	let thread = sched::current_thread().ok_or(ERR_NO_THREAD)?;
	match thread.handles().lock().lookup_typed(Handle::from_raw(handle), ty, rights) {
		Ok(object) => Ok(object),
		Err(HandleError::AccessDenied) => Err(ERR_ACCESS_DENIED),
		Err(_) => Err(ERR_BAD_HANDLE),
	}
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

// Install `object` into `thread`'s handle table with `rights` and `badge`,
// returning the new handle's raw value, or ERR_RESOURCE_EXHAUSTED if the table (or
// the Domain's handle quota) is full. The shared tail of the create handlers.
fn install_object(thread: &Thread, object: Arc<dyn KernelObject>, rights: Rights, badge: u64) -> i64 {
	match thread.handles().lock().try_insert_object(object, rights, badge) {
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

// Entry point called by the architecture syscall stub. `num` selects the call;
// the meaning of the arguments and the return value is per-syscall.
#[unsafe(no_mangle)]
pub extern "C" fn syscall_dispatch(num: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> u64 {
	let result: i64 = match num {
		SYS_DEBUG_NOOP => a0 as i64,
		SYS_CLOCK_GET => arch::apic::ticks() as i64,
		SYS_CLOCK_RTC => arch::rtc::read_unix() as i64,
		SYS_DEBUG_WRITE => sys_debug_write(a0, a1),
		SYS_MEMORY_OBJECT_CREATE => sys_memory_object_create(a0),
		SYS_DMA_BUFFER_CREATE => sys_dma_buffer_create(a0),
		SYS_DMA_BUFFER_MAP => sys_dma_buffer_map(a0),
		SYS_DMA_BUFFER_PHYS => sys_dma_buffer_phys(a0, a1),
		SYS_DEVICE_MEMORY_MAP => sys_device_memory_map(a0),
		SYS_RANDOM_GET => sys_random_get(a0, a1),
		SYS_INTERRUPT_BIND => sys_interrupt_bind(a0),
		SYS_DEVICE_MSIX_ACQUIRE => sys_device_msix_acquire(a0),
		SYS_INTERRUPT_ACK => sys_interrupt_ack(a0),
		SYS_SYSTEM_POWER => sys_system_power(a0),
		SYS_CONSOLE_FEED => sys_console_feed(a0),
		SYS_FRAMEBUFFER_MAP => sys_framebuffer_map(a0, a1),
		SYS_OBJECT_PROPERTY_SET => sys_object_property_set(a0, a1, a2, a3),
		SYS_PROCESS_CREATE => sys_process_create(),
		SYS_PROCESS_LOAD => sys_process_load(a0, a1, a2),
		SYS_PROCESS_SIGNAL => sys_process_signal(a0, a1),
		SYS_THREAD_CREATE => sys_thread_create(a0, a1, a2, a3),
		SYS_THREAD_START => sys_thread_start(a0),
		SYS_CONSOLE_ATTACH => sys_console_attach(a0),
		SYS_DEVICE_COUNT => device::count() as i64,
		SYS_DEVICE_INFO => sys_device_info(a0, a1, a2),
		SYS_DEVICE_ACQUIRE => sys_device_acquire(a0),
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
		SYS_WAIT_ANY => sys_wait_any(a0, a1, a2),
		_ => ERR_BAD_SYSCALL,
	};
	result as u64
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
	install_object(&thread, object, Rights::ALL, 0)
}

// Create a DmaBuffer - pinned DMA memory charged to the caller's Domain DMA quota
// - and install a handle to it in the caller's table. A driver maps the buffer
// and hands its physical address to its device.
fn sys_dma_buffer_create(size: u64) -> i64 {
	let thread = current_thread!();
	let object = match DmaBuffer::create_in(thread.domain(), size as usize) {
		Ok(o) => o,
		Err(MemoryError::QuotaExceeded) => return ERR_RESOURCE_EXHAUSTED,
		Err(MemoryError::OutOfMemory) => return ERR_NO_MEMORY,
	};
	install_object(&thread, object, Rights::ALL, 0)
}

// Map a DmaBuffer into the caller's address space (cacheable RAM, unlike device
// MMIO) and return its virtual base, so a driver can fill the virtqueue rings it
// then points its device at. One mapping per buffer: a second call returns
// ERR_INVALID.
fn sys_dma_buffer_map(handle: u64) -> i64 {
	let dma = match current_typed::<DmaBuffer>(handle, ObjectType::DmaBuffer, Rights::MAP) {
		Ok(o) => o,
		Err(e) => return e,
	};
	if dma.mapped_at() != 0 {
		return ERR_INVALID;
	}
	let user = arch::percpu::in_user_syscall();
	let base = if user { alloc_user_vrange(dma.size() as u64) } else { alloc_kernel_vrange(dma.size() as u64) };
	let flags = if user { arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER } else { arch::paging::PRESENT | arch::paging::WRITABLE };
	for (i, &phys) in dma.frames().iter().enumerate() {
		arch::paging::map_page(base + i as u64 * PAGE_SIZE, phys, flags);
	}
	dma.set_mapped_at(base);
	base as i64
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
// userspace ConsoleService; capability-gating it is a later (M38) hardening.
fn sys_framebuffer_map(buf_ptr: u64, buf_len: u64) -> i64 {
	let size = core::mem::size_of::<abi::Framebuffer>() as u64;
	if buf_len < size || !user_buf_ok(buf_ptr, size) {
		return ERR_INVALID;
	}
	if crate::console::is_disabled() {
		return ERR_INVALID;
	}
	let (addr, geom) = match crate::framebuffer_geometry() {
		Some(t) => t,
		None => return ERR_INVALID,
	};
	let base_phys = match arch::paging::translate(addr) {
		Some(p) => p,
		None => return ERR_INVALID,
	};
	let total = geom.height as u64 * geom.pitch as u64;
	let pages = total.div_ceil(PAGE_SIZE);
	let user = arch::percpu::in_user_syscall();
	let base = if user { alloc_user_vrange(total) } else { alloc_kernel_vrange(total) };
	let mut flags = arch::paging::PRESENT | arch::paging::WRITABLE;
	if user {
		flags |= arch::paging::USER;
	}
	for i in 0..pages {
		arch::paging::map_page(base + i * PAGE_SIZE, base_phys + i * PAGE_SIZE, flags);
	}
	unsafe {
		(buf_ptr as *mut abi::Framebuffer).write_unaligned(geom);
	}
	// Hand the display to the caller: the kernel console stops drawing to it.
	crate::console::disable();
	base as i64
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
	if device.mapped_at() != 0 {
		return ERR_INVALID;
	}
	let user = arch::percpu::in_user_syscall();
	let pages = device.pages();
	let len = pages as u64 * PAGE_SIZE;
	let base = if user { alloc_user_vrange(len) } else { alloc_kernel_vrange(len) };
	let mut flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::NO_CACHE;
	if user {
		flags |= arch::paging::USER;
	}
	for i in 0..pages {
		arch::paging::map_page(base + i as u64 * PAGE_SIZE, device.phys_base() + i as u64 * PAGE_SIZE, flags);
	}
	device.set_mapped_at(base);
	base as i64
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
	let info = device::with(index as usize, |d| abi::DeviceInfo { virtio_type: d.virtio_type as u32, bar_len: d.bar_len, common_offset: d.common_offset, notify_offset: d.notify_offset, notify_multiplier: d.notify_multiplier, isr_offset: d.isr_offset, device_offset: d.device_offset });
	match info {
		Some(info) => {
			unsafe {
				(buf_ptr as *mut abi::DeviceInfo).write_unaligned(info);
			}
			0
		}
		None => ERR_INVALID,
	}
}

// Mint a DeviceMemory capability for the MMIO BAR of the virtio device at `index`
// and install it in the caller's handle table, returning the handle. The caller
// (DeviceManager) hands it to the matching driver, which maps it with
// device_memory_map. Returns ERR_INVALID for an out-of-range index. (Gating this
// to DeviceManager is a PermissionManager policy concern, deferred.)
fn sys_device_acquire(index: u64) -> i64 {
	let thread = current_thread!();
	let memory = match device::with(index as usize, |d| DeviceMemory::new(d.bar_phys, d.bar_len as usize)) {
		Some(m) => m,
		None => return ERR_INVALID,
	};
	install_object(&thread, memory, Rights::READ | Rights::WRITE | Rights::MAP | Rights::TRANSFER, 0)
}

// Fill a caller buffer with `len` random bytes from the kernel CSPRNG (RDRAND when
// available). Returns the number of bytes written, or ERR_INVALID for an
// out-of-range buffer.
fn sys_random_get(buf_ptr: u64, len: u64) -> i64 {
	if len == 0 {
		return 0;
	}
	if !user_buf_ok(buf_ptr, len) {
		return ERR_INVALID;
	}
	// Generate into a kernel buffer in bounded chunks, then copy out to the caller.
	const CHUNK: usize = 256;
	let mut scratch = [0u8; CHUNK];
	let mut filled: u64 = 0;
	while filled < len {
		let n = ((len - filled) as usize).min(CHUNK);
		arch::random::fill(&mut scratch[..n]);
		unsafe {
			core::ptr::copy_nonoverlapping(scratch.as_ptr(), (buf_ptr + filled) as *mut u8, n);
		}
		filled += n as u64;
	}
	filled as i64
}

// Bind a device IRQ vector to a new Interrupt object and install a handle to it in
// the caller's table. A driver waits on the handle; the kernel marks it pending
// and wakes the driver when the vector fires. ERR_INVALID for a non-bindable
// vector, ERR_RESOURCE_EXHAUSTED if the vector is already bound.
fn sys_interrupt_bind(vector: u64) -> i64 {
	let thread = current_thread!();
	if vector > u8::MAX as u64 || !arch::interrupts::is_bindable(vector as u8) {
		return ERR_INVALID;
	}
	let v = vector as u8;
	let interrupt = Interrupt::new(v);
	if !arch::interrupts::bind(v, &interrupt) {
		return ERR_RESOURCE_EXHAUSTED;
	}
	// On a failed install the Interrupt is dropped here, and its Drop unbinds the
	// vector, so no explicit rollback is needed.
	install_object(&thread, interrupt, Rights::ALL, 0)
}

// Acquire an MSI-X Interrupt for the discovered device at `index`: allocate a
// per-device LAPIC vector, program the device's MSI-X table entry 0 to deliver it to
// this CPU, enable MSI-X on the device, and mint an Interrupt bound to that vector.
// Unlike the INTx path the device's legacy pin stays disabled (MSI-X replaces it), so
// the driver gets its own edge-triggered vector with no INTx sharing. ERR_INVALID for
// an out-of-range index or a device with no MSI-X capability.
fn sys_device_msix_acquire(index: u64) -> i64 {
	let thread = current_thread!();
	let (cap, table_phys, bus, dev, func) = match device::with(index as usize, |d| (d.msix_cap, d.msix_table_phys, d.bus, d.dev, d.func)) {
		Some((cap, table_phys, bus, dev, func)) if cap != 0 => (cap, table_phys, bus, dev, func),
		_ => return ERR_INVALID,
	};
	let dest = arch::percpu::this_cpu().lapic_id() as u8;
	let vector = match arch::interrupts::acquire_msi(table_phys, dest) {
		Some(v) => v,
		None => return ERR_RESOURCE_EXHAUSTED,
	};
	let interrupt = Interrupt::new(vector);
	if !arch::interrupts::bind_msi(vector, &interrupt) {
		// The vector raced to another binder; release the slot we reserved.
		arch::interrupts::unbind(vector);
		return ERR_RESOURCE_EXHAUSTED;
	}
	// Turn on MSI-X now that its table entry is programmed; the device's INTx pin stays
	// disabled (MSI-X is its interrupt source from here on).
	arch::pci::msix_enable(bus, dev, func, cap);
	install_object(&thread, interrupt, Rights::ALL, 0)
}

// Acknowledge a serviced interrupt: clear the Interrupt's pending flag so the driver's
// next `wait` blocks until the device interrupts again. MSI-X is edge-triggered, so
// there is no device source to unmask. Requires the WRITE right on the Interrupt handle.
fn sys_interrupt_ack(handle: u64) -> i64 {
	let interrupt = match current_typed::<Interrupt>(handle, ObjectType::Interrupt, Rights::WRITE) {
		Ok(i) => i,
		Err(e) => return e,
	};
	interrupt.clear();
	0
}

// Reboot or power the machine off (action = POWER_REBOOT | POWER_OFF). Diverges on a
// valid action; ERR_INVALID otherwise. Restricting this to an authorized component is
// a PermissionManager concern, deferred.
fn sys_system_power(action: u64) -> i64 {
	match action {
		abi::POWER_REBOOT => arch::reset(),
		abi::POWER_OFF => arch::poweroff(),
		_ => ERR_INVALID,
	}
}

// Inject one byte into the kernel console input, as if it had arrived on the serial
// line - the path a userspace input driver (the virtio-input keyboard) uses to feed
// the interactive shell. (Gating this to the input driver is a PermissionManager
// concern, deferred.) Returns 0.
fn sys_console_feed(byte: u64) -> i64 {
	crate::console_input::feed(byte as u8);
	0
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
		unsafe {
			core::ptr::copy_nonoverlapping(ptr as *const u8, buf.as_mut_ptr(), len as usize);
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
		_ => return ERR_INVALID,
	};
	counter.set_limit(a2);
	0
}

// Create an empty process with its own address space, accounted to the caller's
// Domain, and install a handle to it in the caller's table. The process has no
// threads until process_load gives it an image and thread_create / thread_start
// give it a running thread. (Spawning into a different sub-Domain is deferred.)
fn sys_process_create() -> i64 {
	let thread = current_thread!();
	let process = match sched::process_create(thread.domain().clone()) {
		Some(p) => p,
		None => return ERR_NO_MEMORY,
	};
	install_object(&thread, process, Rights::ALL, 0)
}

// Load an ELF image into a process created by process_create and return its entry
// point. The image bytes are read from the caller's address space at [elf_ptr,
// elf_ptr + elf_len) - a userspace spawner first brings them in via
// memory_object_create + memory_map. The kernel maps the program and a ring-3
// stack into the target process. Requires the MANAGE right on the process handle.
fn sys_process_load(process_handle: u64, elf_ptr: u64, elf_len: u64) -> i64 {
	if elf_len == 0 || !user_buf_ok(elf_ptr, elf_len) {
		return ERR_INVALID;
	}
	let process = match current_typed::<Process>(process_handle, ObjectType::Process, Rights::MANAGE) {
		Ok(o) => o,
		Err(e) => return e,
	};
	// Read the image in place from the caller's (active) address space: the loader
	// copies only the PT_LOAD segments into the child's fresh frames, so there is no
	// need to buffer the whole ELF on the kernel heap. The caller's tables stay
	// active across the load (the child is mapped through its own CR3, not switched
	// to), so the slice remains valid.
	let image = unsafe { core::slice::from_raw_parts(elf_ptr as *const u8, elf_len as usize) };
	match loader::load_image_into(&process, image) {
		Ok(entry) => entry as i64,
		Err(LoadError::OutOfMemory) => ERR_NO_MEMORY,
		Err(LoadError::BadImage) => ERR_INVALID,
	}
}

// Create a ring-3 entry thread in `process_handle`, suspended (not yet running),
// at `entry` on the stack topped at `stack_top`, and install a handle to it in the
// caller's table. If `bootstrap_handle` is non-zero, the capability it names is
// moved out of the caller's table into the child's and delivered to the child's
// thread in rdi - the way a process is endowed with its initial capability.
// Requires the MANAGE right on the process handle (and TRANSFER on the bootstrap).
fn sys_thread_create(process_handle: u64, entry: u64, stack_top: u64, bootstrap_handle: u64) -> i64 {
	let thread = current_thread!();
	let process = match current_typed::<Process>(process_handle, ObjectType::Process, Rights::MANAGE) {
		Ok(o) => o,
		Err(e) => return e,
	};
	// Move the bootstrap capability (if any) into the child, recording the handle
	// value the child will see, so the kernel can wire it into the thread's rdi.
	let child_bootstrap = if bootstrap_handle != 0 {
		let cap = {
			let table = thread.handles().lock();
			let xobject = match table.lookup(Handle::from_raw(bootstrap_handle), Rights::TRANSFER) {
				Ok(o) => o,
				Err(HandleError::AccessDenied) => return ERR_ACCESS_DENIED,
				Err(_) => return ERR_BAD_HANDLE,
			};
			let rights = table.rights_of(Handle::from_raw(bootstrap_handle)).unwrap_or(Rights::NONE);
			let badge = table.badge_of(Handle::from_raw(bootstrap_handle)).unwrap_or(0);
			Capability::new(xobject, rights, badge)
		};
		let child_handle = process.handles().lock().insert(cap).raw();
		// The capability now lives in the child: consume the caller's handle.
		let _ = thread.handles().lock().close(Handle::from_raw(bootstrap_handle));
		child_handle
	} else {
		0
	};
	let new_thread = match loader::create_user_thread(&process, entry, stack_top, child_bootstrap) {
		Some(t) => t,
		None => return ERR_RESOURCE_EXHAUSTED,
	};
	install_object(&thread, new_thread, Rights::ALL, 0)
}

// Start a suspended thread created by thread_create, enqueueing it to run. Exactly
// once: a repeated start returns ERR_INVALID rather than double-enqueueing it.
// Requires the MANAGE right on the thread handle.
fn sys_thread_start(thread_handle: u64) -> i64 {
	let target = match current_typed::<Thread>(thread_handle, ObjectType::Thread, Rights::MANAGE) {
		Ok(o) => o,
		Err(e) => return e,
	};
	if sched::thread_start(target) { 0 } else { ERR_INVALID }
}

// Deliver a signal to a process: the holder of its MANAGE capability requests a
// default disposition. INT / TERM / KILL terminate the target; STOP suspends it; CONT
// resumes a suspended one. Each case wakes the target's threads so a blocked thread
// observes the change at once (a kill exits it, a stop parks it, a continue releases
// it) rather than waiting on whatever it was blocked on. There are no user-installed
// handlers in this milestone - only the default dispositions.
fn sys_process_signal(process_handle: u64, signal: u64) -> i64 {
	let process = match current_typed::<Process>(process_handle, ObjectType::Process, Rights::MANAGE) {
		Ok(p) => p,
		Err(e) => return e,
	};
	match signal {
		SIG_INT | SIG_TERM | SIG_KILL => {
			process.terminate();
			for thread in process.live_threads() {
				sched::wake_thread(&thread);
			}
		}
		SIG_STOP => {
			process.set_stopped(true);
			for thread in process.live_threads() {
				sched::wake_thread(&thread);
			}
		}
		SIG_CONT => {
			process.set_stopped(false);
			sched::wake_object(process.header().koid());
		}
		_ => return ERR_INVALID,
	}
	0
}

// Register the calling thread's channel as the kernel's console input sink: the
// kernel reads serial bytes and sends them on it, and the userspace shell receives
// them on the peer endpoint. The handle must name a Channel the caller can send on.
fn sys_console_attach(handle: u64) -> i64 {
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
	let thread = current_thread!();
	let object = {
		let table = thread.handles().lock();
		match table.lookup_typed(Handle::from_raw(handle), ObjectType::MemoryObject, Rights::MAP) {
			Ok(o) => o,
			Err(_) => return ERR_BAD_HANDLE,
		}
	};
	let memory = object.as_any().downcast_ref::<MemoryObject>().expect("type checked by lookup_typed");
	// Reject a duplicate map within the SAME address space; mapping into a different
	// address space is allowed, so an object can be shared (e.g. the init package
	// mapped by both ServiceManager and DeviceManager).
	let cr3 = arch::context::read_cr3();
	if memory.mapped_at() != 0 && memory.mapped_cr3() == cr3 {
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
	memory.set_mapped_cr3(cr3);
	base as i64
}

// Remove a MemoryObject's mapping from the kernel address space.
fn sys_memory_unmap(handle: u64) -> i64 {
	let thread = current_thread!();
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
	arch::paging::unmap_pages(base, memory.frames().len());
	memory.set_mapped_at(0);
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
	let thread = current_thread!();
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
	let thread = current_thread!();
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
	match channel.send_charged(Message::new(bytes, caps, badge), thread.domain()) {
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

// The most handles `wait_any` accepts at once (enough for a driver's IRQ plus a
// service's client/listener/socket channels - NetworkService multiplexes its frame
// channel, several clients, a listener, and a pool of sockets at once; ConsoleService
// multiplexes its keyboard, every VT's data+control channel, the gpu channel, and a
// pool of program-hosted PTYs).
const MAX_WAIT_ANY: usize = 20;

// Block until ANY handle in the caller's array `[handles_ptr; count]` is ready,
// returning that handle's index, or ERR_TIMED_OUT at `deadline` (absolute ticks,
// 0 = none). Like `wait` but over a set: a driver waits on its device interrupt and
// a control channel at once, waking on whichever is ready first. Each handle needs
// the WAIT right.
fn sys_wait_any(handles_ptr: u64, count: u64, deadline: u64) -> i64 {
	let thread = current_thread!();
	let n = count as usize;
	if n == 0 || n > MAX_WAIT_ANY || !user_buf_ok(handles_ptr, count * 8) {
		return ERR_INVALID;
	}
	let raw = unsafe { core::slice::from_raw_parts(handles_ptr as *const u64, n) };
	// Resolve every handle once up front, recording each object and its koid.
	let mut objects: [Option<Arc<dyn KernelObject>>; MAX_WAIT_ANY] = core::array::from_fn(|_| None);
	let mut koids: [u64; MAX_WAIT_ANY] = [0; MAX_WAIT_ANY];
	{
		let table = thread.handles().lock();
		for i in 0..n {
			let object = match table.lookup(Handle::from_raw(raw[i]), Rights::WAIT) {
				Ok(o) => o,
				Err(HandleError::AccessDenied) => return ERR_ACCESS_DENIED,
				Err(_) => return ERR_BAD_HANDLE,
			};
			koids[i] = object.header().koid();
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
		for (i, slot) in objects.iter().enumerate().take(n) {
			if let Some(object) = slot {
				if object_ready(object) {
					return i as i64;
				}
			}
		}
		let block_deadline = if deadline == 0 { sched::NO_DEADLINE } else { deadline };
		if block_deadline != sched::NO_DEADLINE && arch::apic::ticks() >= block_deadline {
			return ERR_TIMED_OUT;
		}
		sched::block_on_any(&koids[..n], block_deadline);
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
	if let Some(interrupt) = any.downcast_ref::<Interrupt>() {
		return interrupt.is_pending();
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
	let thread = current_thread!();
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
	let thread = current_thread!();
	let child = Domain::new_child(thread.domain(), memory_limit, handle_limit, thread_limit);
	install_object(&thread, child, Rights::ALL, 0)
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
	install_object(&thread, Event::create(), Rights::ALL, 0)
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
	install_object(&thread, Timer::create(), Rights::ALL, 0)
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
