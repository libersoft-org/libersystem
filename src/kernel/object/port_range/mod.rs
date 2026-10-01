// PortRange: the authority to a range of x86 I/O ports.
//
// A device may be reached through the separate 64 KiB port space as well as through memory, and ring 3
// may execute `in` and `out` only for the ports the running core's task-state segment allows. "May this
// process do port I/O" is the IOPL answer, and it hands out the PIC, the PIT, the keyboard controller,
// the CMOS and the PCI configuration mechanism at once. What a driver needs is ONE range, so the authority
// is an object naming a range: minted by the kernel only for a range a row records (or, for the firmware
// interpreter, a range its privilege admits), handed to a driver with its device's claim, and revoked
// with it.
//
// NO DIRECTIONAL RIGHTS: the permission bitmap has one bit per port and it governs `in` and `out` alike,
// so a range is all or nothing. The handle carries the ordinary rights - transfer, and the right to map.
// HOLDING IT GRANTS NOTHING until its holder maps it, so a DeviceManager that mints a range and passes it
// on never gains port access itself; and it is mapped into AT MOST ONE PROCESS at a time.
//
// THE GRANT LIVES FROM THE MINT UNTIL THE OBJECT IS REVOKED OR DROPPED - `grants` holds its ports out of
// every other grant for exactly that long. Unmapping keeps the grant; a revocation, a drop, or an unmap
// whose cross-core round did not confirm ends it, the last of those retiring its ports for the boot.
//
// ARM and RISC-V have no port space: every syscall reaching this answers `ERR_UNSUPPORTED` there and no
// row carries a port resource, so nothing is ever minted.

pub mod grants;

#[cfg(test)]
mod tests;

use alloc::boxed::Box;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, Ordering, fence};

use super::process::Process;
use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};
use crate::sync::SpinLock;

// The port space, in bitmap words.
pub const BITMAP_WORDS: usize = (grants::PORT_SPACE as usize / 8) / 8;

// ONE PROCESS'S PERMISSION BITMAP, in the processor's polarity: a set bit REFUSES its port. Words, so a
// copy reads it in eight-byte steps and a change writes it without tearing a word.
pub struct IoBitmap {
	words: [AtomicU64; BITMAP_WORDS],
}

// A PROCESS'S PORT STATE: its bitmap, the ranges mapped into it, and the generation every core's copy is
// checked against.
//
// CHANGES ARE SERIALISED; COPIES TAKE NO LOCK. A change runs under `change` with interrupts masked, makes
// the generation odd before it touches a word and even again after, and allocates nothing in between -
// the bitmap is allocated before, on the first map. A copy reads the generation before and after and
// tries again while it was odd or moved; see `begin_read` and `end_read`, and the cores' side in
// `arch::ioports`. So a copy on the changing core cannot run inside a change, and a copy on another core
// waits at most one.
pub struct IoPorts {
	// The process's object id, never reused - what a core's record names as loaded (x86_64's TSS copy alone).
	#[cfg(target_arch = "x86_64")]
	owner: u64,
	change: SpinLock<()>,
	generation: AtomicU64,
	// How many leading words of the bitmap a copy needs: one past the last word holding an allowed port,
	// 0 while no port is allowed.
	words: AtomicU32,
	// Null until the first map.
	bits: AtomicPtr<IoBitmap>,
	// What is mapped here, so a terminating process can take every range back.
	mapped: SpinLock<Vec<Arc<PortRange>>>,
}

impl IoPorts {
	pub const fn new(owner: u64) -> Self {
		#[cfg(not(target_arch = "x86_64"))]
		let _ = owner;
		Self {
			#[cfg(target_arch = "x86_64")]
			owner,
			change: SpinLock::new(()),
			generation: AtomicU64::new(0),
			words: AtomicU32::new(0),
			bits: AtomicPtr::new(core::ptr::null_mut()),
			mapped: SpinLock::new(Vec::new()),
		}
	}

	// The process these ports belong to.
	#[inline(always)]
	#[cfg(target_arch = "x86_64")]
	pub fn owner(&self) -> u64 {
		self.owner
	}

	// The generation a copy is compared against. Odd while a change is running.
	#[inline(always)]
	#[cfg(target_arch = "x86_64")]
	pub fn generation(&self) -> u64 {
		self.generation.load(Ordering::Acquire)
	}

	// THE START OF ONE COPY: the generation it is consistent with and how many words it needs, or `None`
	// while a change is running.
	#[inline(always)]
	#[cfg(target_arch = "x86_64")]
	pub fn begin_read(&self) -> Option<(u64, usize)> {
		let generation = self.generation.load(Ordering::Acquire);
		if generation & 1 != 0 {
			return None;
		}
		Some((generation, self.words.load(Ordering::Relaxed) as usize))
	}

	// Word `at` of the bitmap, within a copy that `begin_read` started with a non-zero word count - which
	// only a mapped range produces, so the bitmap exists.
	#[inline(always)]
	#[cfg(target_arch = "x86_64")]
	pub fn word(&self, at: usize) -> u64 {
		let bits = self.bits.load(Ordering::Acquire);
		if bits.is_null() {
			return u64::MAX;
		}
		// SAFETY: the bitmap is never freed while its process lives, and a copy reads the bitmap of a
		// process one of whose threads is current on the copying core.
		unsafe { (*bits).words[at].load(Ordering::Relaxed) }
	}

	// THE END OF THAT COPY: whether it is consistent with `generation`.
	#[inline(always)]
	#[cfg(target_arch = "x86_64")]
	pub fn end_read(&self, generation: u64) -> bool {
		fence(Ordering::Acquire);
		self.generation.load(Ordering::Relaxed) == generation
	}

	// Allocate the bitmap, refusing everything, if this is the first map. Outside every lock, and
	// FALLIBLY: a map that cannot allocate is refused.
	fn ensure_bitmap(&self) -> bool {
		if !self.bits.load(Ordering::Acquire).is_null() {
			return true;
		}
		// In place, rather than through `try_box`, which would build the 8 KiB value on the kernel
		// stack first.
		let Ok(mut fresh) = Box::<IoBitmap>::try_new_uninit() else { return false };
		let words = fresh.as_mut_ptr() as *mut AtomicU64;
		for at in 0..BITMAP_WORDS {
			// SAFETY: `at` is inside the one allocation `try_new_uninit` returned, which nothing else can
			// see yet.
			unsafe { words.add(at).write(AtomicU64::new(u64::MAX)) };
		}
		// SAFETY: every word was written above.
		let raw = Box::into_raw(unsafe { fresh.assume_init() });
		if self.bits.compare_exchange(core::ptr::null_mut(), raw, Ordering::AcqRel, Ordering::Acquire).is_err() {
			// Another thread of this process mapped first; its bitmap stands.
			// SAFETY: `raw` came from `Box::into_raw` above and was never published.
			drop(unsafe { Box::from_raw(raw) });
		}
		true
	}

	// ONE CHANGE: allow or refuse `len` ports from `base`, under the change lock with interrupts masked.
	// The bitmap exists - a change is only ever made for a range that was mapped.
	fn change(&self, base: u16, len: u16, allow: bool) {
		let bits = self.bits.load(Ordering::Acquire);
		debug_assert!(!bits.is_null(), "a port change before the first map");
		if bits.is_null() {
			return;
		}
		// SAFETY: as in `word`, and this process is alive: its caller holds it.
		let bits = unsafe { &*bits };
		let _serial = self.change.lock();
		let generation = self.generation.load(Ordering::Relaxed);
		self.generation.store(generation.wrapping_add(1), Ordering::Relaxed);
		fence(Ordering::Release);
		let end = base as usize + len as usize;
		let mut port = base as usize;
		while port < end {
			let word = port / 64;
			let first = port % 64;
			let count = (64 - first).min(end - port);
			let mask = if count == 64 { u64::MAX } else { ((1u64 << count) - 1) << first };
			if allow {
				bits.words[word].fetch_and(!mask, Ordering::Relaxed);
			} else {
				bits.words[word].fetch_or(mask, Ordering::Relaxed);
			}
			port += count;
		}
		// How many words a copy needs now: one past the last word with an allowed port.
		let mut words = self.words.load(Ordering::Relaxed) as usize;
		if allow {
			words = words.max((end - 1) / 64 + 1);
		} else {
			while words > 0 && bits.words[words - 1].load(Ordering::Relaxed) == u64::MAX {
				words -= 1;
			}
		}
		self.words.store(words as u32, Ordering::Relaxed);
		self.generation.store(generation.wrapping_add(2), Ordering::Release);
	}

	// Free the bitmap with its process.
	pub fn release(&mut self) {
		let bits = core::mem::replace(self.bits.get_mut(), core::ptr::null_mut());
		if !bits.is_null() {
			// SAFETY: `bits` came from `Box::into_raw` in `ensure_bitmap`, and `&mut self` means no copy
			// can be reading it.
			drop(unsafe { Box::from_raw(bits) });
		}
	}
}

// Where a range is in its life.
enum State {
	// Granted, and mapped nowhere.
	Idle,
	// Granted, and mapped into this process.
	Mapped(Weak<Process>),
	// The grant has ended - revoked, dropped, or an unmap that could not be confirmed.
	Ended,
}

// Why a map or unmap was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MapError {
	// Mapped into a process already (a map), or not mapped into the caller's (an unmap).
	Busy,
	// The grant has ended.
	Ended,
	// The caller's process is going away.
	Terminating,
	// The bitmap or the process's list could not be allocated.
	NoMemory,
}

pub struct PortRange {
	header: ObjectHeader,
	base: u16,
	len: u16,
	// The claim this range was minted under, so its mappings are counted on it and its release waits for
	// them; `None` for a firmware region.
	claim: Option<abi::ClaimKey>,
	state: SpinLock<State>,
}

impl PortRange {
	// MINT: grant `len` ports from `base` and wrap the grant in an object, or refuse - against the reserved
	// set, every live grant and every retired span, in one step.
	// The ports it grants, for the suites outside this module.
	#[cfg(all(test, target_arch = "x86_64"))]
	pub fn span(&self) -> (u16, u16) {
		(self.base, self.len)
	}

	pub fn mint(base: u16, len: u16, claim: Option<abi::ClaimKey>) -> Result<Arc<Self>, grants::Refusal> {
		// THE OBJECT FIRST, so the grant is taken with nothing left to allocate - and its id is the grant's
		// owner.
		let range = crate::mem::heap::try_arc(Self { header: ObjectHeader::new(), base, len, claim, state: SpinLock::new(State::Ended) }).ok_or(grants::Refusal::NoMemory)?;
		grants::grant(base, len, range.header.koid())?;
		*range.state.lock() = State::Idle;
		Ok(range)
	}

	// THE CONSOLE HANDOFF'S MINT: the ports kernel item `item` installed - the console UART a claim has just
	// taken from the kernel - moved into this range's grant in one step, and back into the reserved set when
	// the grant ends. See `grants::grant_from_install`.
	pub fn mint_from_install(item: u32, base: u16, len: u16, claim: abi::ClaimKey) -> Result<Arc<Self>, grants::Refusal> {
		let range = crate::mem::heap::try_arc(Self { header: ObjectHeader::new(), base, len, claim: Some(claim), state: SpinLock::new(State::Ended) }).ok_or(grants::Refusal::NoMemory)?;
		grants::grant_from_install(item, base, len, range.header.koid())?;
		*range.state.lock() = State::Idle;
		Ok(range)
	}

	// The process this range is mapped into, for the development request that kills the console UART's
	// driver.
	#[cfg(liber_development)]
	pub fn holder(&self) -> Option<Arc<Process>> {
		match &*self.state.lock() {
			State::Mapped(holder) => holder.upgrade(),
			State::Idle | State::Ended => None,
		}
	}

	// MAP INTO `process`: its threads may use these ports from their next access on any core - a grant
	// needs no cross-core round, because a core that has not loaded it yet faults once and loads it then.
	pub fn map_into(self: &Arc<Self>, process: &Arc<Process>) -> Result<(), MapError> {
		let io = process.io_ports();
		if !io.ensure_bitmap() {
			return Err(MapError::NoMemory);
		}
		if io.mapped.lock().try_reserve(1).is_err() {
			return Err(MapError::NoMemory);
		}
		// Refused once the process is going away, and held against its teardown for the whole operation.
		let Some(_extend) = process.begin_extend() else { return Err(MapError::Terminating) };
		let mut state = self.state.lock();
		match *state {
			State::Idle => {}
			State::Mapped(_) => return Err(MapError::Busy),
			State::Ended => return Err(MapError::Ended),
		}
		*state = State::Mapped(Arc::downgrade(process));
		if let Some(key) = self.claim {
			crate::device::mmio_mapping_installed(key);
		}
		io.change(self.base, self.len, true);
		// ALLOC-OK: reserved above, and nothing else pushes to this list outside a map of the same
		// process, which holds `begin_extend` too - two maps racing each reserved their own slot.
		io.mapped.lock().push(self.clone());
		Ok(())
	}

	// UNMAP FROM `process`, the caller's own: the ports are taken back, the local core copies again, and
	// ONE cross-core round confirms no core still lets the process use them. Answers whether it confirmed;
	// an unconfirmed round ends the grant and retires the ports for the boot.
	pub fn unmap_from(&self, process: &Arc<Process>) -> Result<bool, MapError> {
		let mut state = self.state.lock();
		match &*state {
			State::Mapped(holder) if core::ptr::eq(holder.as_ptr(), Arc::as_ptr(process)) => {}
			State::Ended => return Err(MapError::Ended),
			_ => return Err(MapError::Busy),
		}
		let confirmed = self.take_back(process);
		*state = if confirmed { State::Idle } else { State::Ended };
		drop(state);
		if !confirmed {
			grants::end(self.header.koid(), false);
		}
		forget(process, self);
		Ok(confirmed)
	}

	// REVOKE, for the claim's release: take the range back from whoever holds it through the same round,
	// and end the grant - its ports back in circulation when the round confirmed, retired when it did
	// not. Answers whether it confirmed.
	pub fn revoke(&self) -> bool {
		let mut state = self.state.lock();
		let holder = match core::mem::replace(&mut *state, State::Ended) {
			State::Ended => return true,
			State::Idle => None,
			State::Mapped(holder) => Some(holder),
		};
		let mut confirmed = true;
		let mut held_by = None;
		if let Some(process) = holder.and_then(|holder| holder.upgrade()) {
			confirmed = self.take_back(&process);
			held_by = Some(process);
		}
		drop(state);
		grants::end(self.header.koid(), confirmed);
		if let Some(process) = held_by {
			forget(&process, self);
		}
		confirmed
	}

	// A TERMINATING PROCESS GIVES THE RANGE BACK: its threads may still be running on other cores until
	// their next switch, so the same round runs. Confirmed, the range is idle again and its holder may pass
	// it on; not confirmed, the grant ends and the ports are retired.
	fn give_back(&self, process: &Process) {
		let mut state = self.state.lock();
		match &*state {
			State::Mapped(holder) if core::ptr::eq(holder.as_ptr(), process) => {}
			_ => return,
		}
		let confirmed = self.take_back(process);
		*state = if confirmed { State::Idle } else { State::Ended };
		drop(state);
		if !confirmed {
			grants::end(self.header.koid(), false);
		}
	}

	// The revocation itself: clear the range's bits as one change, RELEASE the process's lock, copy again
	// on this core if it is running the process, and only then run one round of the TLB shootdown's
	// request and acknowledgement - whose per-core service step copies again on every core where the
	// running process's generation moved. The range's own lock is held throughout, so a revocation and an
	// unmap of one range cannot interleave.
	fn take_back(&self, process: &Process) -> bool {
		process.io_ports().change(self.base, self.len, false);
		crate::arch::ioports::reload_if_running(process);
		let confirmed = crate::mem::tlb::shootdown();
		if let Some(key) = self.claim {
			crate::device::mmio_mapping_torn_down(key, confirmed);
		}
		confirmed
	}
}

// THE FIRMWARE'S PART OF THE RESERVED SET, recorded before the boot scan records any row - in the test
// build as well, where a derivation row is checked against it during that scan.
pub fn reserve_firmware_ports() {
	if !crate::arch::ioports::supported() {
		return;
	}
	// COM1 FIRST: the kernel console has driven it since the kernel's first line, and it is a run-time
	// install like any other port the kernel starts driving - which is what lets the COM1 handoff move it.
	if grants::install(grants::KERNEL_CONSOLE, grants::COM1.0, grants::COM1.1).is_err() {
		crate::serial_println!("ports: COM1 could not be reserved to the kernel console - no memory for the table");
	}
	let mut blocks = [(0u16, 0u16); 16];
	let count = crate::arch::ioports::firmware_blocks(&mut blocks);
	for &(base, len) in &blocks[..count] {
		if !grants::reserve_firmware(base, len) {
			crate::serial_println!("ports: the firmware's block at {base:#06x} ({len} ports) could not be reserved - a port past the space or no memory; it stays mintable");
		}
	}
}

// Take `range` off `process`'s list of what is mapped into it.
fn forget(process: &Process, range: &PortRange) {
	let mut mapped = process.io_ports().mapped.lock();
	if let Some(at) = mapped.iter().position(|held| core::ptr::eq(Arc::as_ptr(held), range)) {
		mapped.swap_remove(at);
	}
}

// EVERY RANGE MAPPED INTO A TERMINATING PROCESS, given back. Called by the teardown, which already
// refuses every new map.
pub fn give_back_all(process: &Process) {
	let ranges = core::mem::take(&mut *process.io_ports().mapped.lock());
	for range in ranges {
		range.give_back(process);
	}
}

impl Drop for PortRange {
	fn drop(&mut self) {
		// A dropped range cannot be mapped: the process's list held a reference for as long as it was.
		// So the grant ends with nothing to take back.
		if !matches!(*self.state.lock(), State::Ended) {
			grants::end(self.header.koid(), true);
		}
	}
}

impl_kernel_object!(PortRange, PortRange);
