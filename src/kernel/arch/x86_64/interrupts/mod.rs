// Hardware-interrupt dispatch and handler registration.
//
// Device interrupts land on vectors IRQ_BASE..IRQ_BASE+IRQ_COUNT. Each vector
// has a small stub in the IDT that funnels into a common dispatcher, which looks
// up a registered handler and signals end-of-interrupt to the LAPIC.
//
// The handler table is lock-free (an array of atomics): registration only stores
// a function pointer, and dispatch only loads one, so it is safe to call from
// interrupt context without risking a deadlock against a held lock.

use core::sync::atomic::{AtomicUsize, Ordering};

use alloc::sync::{Arc, Weak};

use super::apic;
use super::idt::{self, InterruptStackFrame};
use crate::arch::common::msi::MsiRegistry;
use crate::object::interrupt::Interrupt;
use crate::sync::SpinLock;

// Device-interrupt vector window (mirrors the legacy 16-IRQ layout).
pub const IRQ_BASE: u8 = 32;
pub const IRQ_COUNT: usize = 16;

// Well-known vectors.
pub const TIMER_VECTOR: u8 = IRQ_BASE; // IRQ 0
pub const SPURIOUS_VECTOR: u8 = 0xff;

// MSI-X vector window: per-device edge-triggered vectors delivered straight to a
// LAPIC, with no INTx sharing. Sits just above the legacy INTx window (32..48) and
// spans everything up to 240, leaving 240..255 for future IPIs and the spurious
// vector (0xff) - 192 device vectors. This is one GLOBAL window (a vector number
// identifies its device system-wide); Linux goes further with a per-CPU vector
// space (~200 per core), which is the future model if multi-queue devices ever
// need more than this.
pub const MSI_BASE: u8 = IRQ_BASE + IRQ_COUNT as u8; // 48
pub const MSI_COUNT: usize = 192;

// The cross-core wake IPI vector: sent by the scheduler when it enqueues work for a
// core that may be halted in its idle loop. The interrupt itself is the message -
// the handler only signals EOI; taking it bounces the target out of HLT, and its
// idle loop then finds the queued thread.
pub const WAKE_VECTOR: u8 = 0xf0;

pub type HandlerFn = fn(u32);

static HANDLERS: [AtomicUsize; IRQ_COUNT] = [const { AtomicUsize::new(0) }; IRQ_COUNT];

// Userspace-driver bindings: the Interrupt object to wake when each device vector
// fires. Held weakly, so closing the driver's handle (its Interrupt's Drop) clears
// the binding and the kernel stops delivering to a gone driver.
static BOUND: [SpinLock<Option<Weak<Interrupt>>>; IRQ_COUNT] = [const { SpinLock::new(None) }; IRQ_COUNT];

// MSI-X driver bindings (reserve / bind / dispatch / free bookkeeping, shared with
// aarch64 via arch::common::msi): slot index i maps to vector MSI_BASE + i.
static REGISTRY: MsiRegistry<MSI_COUNT> = MsiRegistry::new();

// Kernel virtual base for mapping device MSI-X tables (uncacheable), clear of the
// LAPIC (0xffff_f100) / IOAPIC (0xffff_f200) MMIO windows. TWO pages per MSI slot (see
// `msix_pages_for_entry`); the page-table chain is materialised at init (kernel PML4 active,
// before any process address space exists) so runtime per-device mappings under it propagate to
// every address space's shared kernel half.
const MSIX_VIRT_BASE: u64 = 0xffff_f300_0000_0000;

// An MSI-X table entry is sixteen bytes.
const MSIX_ENTRY_BYTES: u64 = 16;

// TWO PAGES OF ADDRESS SPACE PER SLOT, because one entry can need two (KERN-ARCH-016).
const MSIX_SLOT_STRIDE: u64 = 0x2000;

// Where this slot's table mapping starts.
fn msix_virt(slot: usize) -> u64 {
	MSIX_VIRT_BASE + slot as u64 * MSIX_SLOT_STRIDE
}

// How many pages must be mapped for an entry beginning `offset` bytes into its page.
//
// The MSI-X table's offset within its BAR is 8-BYTE aligned - the low three bits of the capability
// field hold the BIR - so a sixteen-byte entry may legally begin at 0xff8 and end in the next page.
// The backend mapped exactly one page and wrote four dwords at the offset, so for such a device the
// last two - message data and vector control - went to an unmapped address: a kernel page fault
// while programming an interrupt, and the same one page assumed again at teardown when masking.
fn msix_pages_for_entry(offset: u64) -> u64 {
	if offset + MSIX_ENTRY_BYTES > 0x1000 { 2 } else { 1 }
}

// Where each slot's MSI-X entry sits inside its mapped page, recorded when the entry is programmed
// so the teardown masks the same words rather than guessing offset 0.
#[allow(clippy::declare_interior_mutable_const)]
const NO_OFFSET: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static MSIX_ENTRY_OFFSET: [core::sync::atomic::AtomicU32; MSI_COUNT] = [NO_OFFSET; MSI_COUNT];

// Whether `vector` is a kernel MSI-X vector.
fn is_msi(vector: u32) -> bool {
	vector >= MSI_BASE as u32 && (vector as usize) < MSI_BASE as usize + MSI_COUNT
}

// Whether `vector` is a device-IRQ vector a driver may bind. The timer vector
// (IRQ_BASE) is the kernel's own and is never handed out.
pub fn is_bindable(vector: u32) -> bool {
	vector > IRQ_BASE as u32 && vector < IRQ_BASE as u32 + IRQ_COUNT as u32
}

// Bind `intr` to `vector` so the dispatch path wakes it when the vector fires.
// Returns false if the vector is already bound to a live Interrupt.
pub fn bind(vector: u32, intr: &Arc<Interrupt>) -> bool {
	if !is_bindable(vector) {
		return false;
	}
	let index = (vector - IRQ_BASE as u32) as usize;
	let mut slot = BOUND[index].lock();
	if slot.as_ref().and_then(Weak::upgrade).is_some() {
		return false;
	}
	*slot = Some(Arc::downgrade(intr));
	intr.mark_bound();
	true
}

// Remove any binding for `vector` (called from an Interrupt's Drop).
// Returns whether the teardown CONFIRMED, which on this port it always does: masking an MSI-X table
// entry and freeing the slot are local writes with no remote agreement to wait for. riscv64 is the
// port where it can fail - see its `unbind`, which asks the owning hart to disable the EID - and the
// signature is shared so a caller can fold the answer into a claim's terminal state without asking
// which architecture it is on.
pub fn unbind(vector: u32) -> bool {
	if is_msi(vector) {
		// MASK the device's table entry, then unmap its page, and only then free the vector.
		//
		// "There is no source to mask" was true of the LAPIC and not of the DEVICE. The slot's
		// MSI-X table page stayed mapped at a fixed kernel address, so re-acquiring the slot called
		// `map_page` over a live leaf - which the paging layer deliberately refuses and the
		// infallible entry point turns into a panic. And nothing stopped the departing device from
		// raising the vector, so the next driver to be given it could be woken by the last one's
		// hardware: ownership confusion with no way for either side to notice.
		let slot = (vector - MSI_BASE as u32) as usize;
		mask_and_unmap_msix_entry(slot);
		// RETIRED, NOT FREED. The mask stops the next message; it says nothing about one the device
		// has already sent, so a vector reused now can wake its next owner with the last owner's
		// hardware. It waits for `SYS_DEVICE_QUIESCED` - the device's own capability holder saying
		// the device stopped - exactly as that driver's DMA frames do.
		REGISTRY.retire(slot);
		return true;
	}
	let index = vector.wrapping_sub(IRQ_BASE as u32) as usize;
	if index < IRQ_COUNT {
		*BOUND[index].lock() = None;
	}
	true
}

// Give back a vector whose Interrupt NEVER REACHED ITS OWNER: mask the entry, unmap the table page
// and free the slot outright rather than retiring it as pending.
//
// `unbind` retires, because a device that has been running may already have sent a message the mask
// cannot recall - so the slot waits for `SYS_DEVICE_QUIESCED`. That reasoning does not apply to a
// vector acquired by a syscall that then failed before the driver existed: MSI-X was never enabled
// on the device, nothing can have been sent, and there is no owner to quiesce it. Retiring those
// left the slot waiting for a message from a driver that was never created - repeatable, one slot
// of a fixed table per failed acquire.
pub fn release_unused_msi(vector: u32) {
	if !is_msi(vector) {
		return;
	}
	let slot = (vector - MSI_BASE as u32) as usize;
	mask_and_unmap_msix_entry(slot);
	REGISTRY.free(slot);
}

// Allocate a free MSI vector and program a device's MSI-X table ENTRY 0 so the device
// delivers it to LAPIC `dest` (edge-triggered, fixed delivery, unmasked). `table_phys`
// is the physical address of the device's MSI-X table.
//
// ENTRY 0 IS THE ONLY ENTRY THIS KERNEL PROGRAMS, and that is now a stated limit rather than an
// assumption. A device may hold ONE live vector, because two slots for one device would both be
// programmed into this one entry - the second overwriting the first, and the first's `unbind` later
// masking the entry the second is live on. Wanting more than one vector per device means programming
// the entry index the slot owns (the table's size is in the MSI-X capability and the backends already
// map the table page), and the refusal is what makes that a change rather than a bug fix.
//
// `MsiRegistry::acquire_unique_live` is what refuses the second, and `acquire_msi_unique` below is
// how the syscall reaches it. This comment said `acquire` did, and it did not - the check was a
// separate `has_live` at the syscall, which two CPUs could both pass before either claimed a slot.
//
// Returns the vector; None if every MSI slot is taken OR this device already holds one. The caller
// enables MSI-X on the device and binds an Interrupt to the returned vector with bind_msi. `owner`
// is the discovered-device index the vector is acquired for, retained for the `lsirq` inventory.
#[cfg(test)]
pub fn acquire_msi(table_phys: u64, dest: u8, owner: u32) -> Option<u32> {
	program_acquired(REGISTRY.acquire(owner, MSI_COUNT)?, table_phys, dest)
}

// The same, and ONLY IF the device holds no live vector already - one operation rather than a
// `device_has_live_msi` at the syscall followed by an `acquire` here, which two CPUs could both pass.
// See `MsiRegistry::acquire_unique_live`. This is what `sys_device_msix_acquire` calls; the form
// above stays for the kernel's own bring-up test, which stands in for DeviceManager on a device the
// booted system has already claimed.
pub fn acquire_msi_unique(table_phys: u64, dest: u8, owner: u32) -> Option<u32> {
	program_acquired(REGISTRY.acquire_unique_live(owner, MSI_COUNT)?, table_phys, dest)
}

fn program_acquired(slot: usize, table_phys: u64, dest: u8) -> Option<u32> {
	// The IDT vector stays a byte - that is what an x86 gate index IS - and widens on the way out
	// to the portable identifier every backend now speaks (KERN-ARCH-017).
	let vector = MSI_BASE + slot as u8;
	program_msix_entry(slot, table_phys, vector, dest);
	Some(vector as u32)
}

// The state of the device-interrupt vector at `index` over both windows: the fixed
// INTx window (0..IRQ_COUNT) first, then the MSI-X window. `bound` reports whether
// the vector is in use - a registered kernel handler or a live driver binding -
// and `device` the MSI owner's device index (IRQ_NO_DEVICE otherwise).
pub fn irq_info(index: usize) -> Option<abi::IrqInfo> {
	if index < IRQ_COUNT {
		let vector = IRQ_BASE + index as u8;
		// The timer vector has a dedicated IDT gate (not a HANDLERS entry), so it is
		// reported in use explicitly - it is always the kernel's own.
		let handled = vector == TIMER_VECTOR || HANDLERS[index].load(Ordering::SeqCst) != 0;
		let bound = handled || is_bound(vector as u32);
		return Some(abi::IrqInfo { vector: vector as u32, kind: abi::IRQ_KIND_FIXED, bound: bound as u32, device: abi::IRQ_NO_DEVICE });
	}
	let slot = index - IRQ_COUNT;
	if slot >= MSI_COUNT {
		return None;
	}
	let vector = MSI_BASE as u32 + slot as u32;
	Some(abi::IrqInfo { vector, kind: abi::IRQ_KIND_MSI, bound: is_bound(vector) as u32, device: REGISTRY.owner(slot) })
}

// Free every MSI vector that is masked and waiting for `device` to be confirmed stopped, and answer
// how many. Reached from `SYS_DEVICE_QUIESCED`.
// How many MSI slots this device still holds. See `MsiRegistry::held_by_device`.
// WHETHER THIS DEVICE STILL HOLDS A SLOT WHOSE TEARDOWN HAS NOT HAPPENED.
//
// `unbind` RETIRES a slot (pending) and a disable the controller refused QUARANTINES it, so a slot
// that is neither is one still bound - and a claim release that publishes `Free` over one of those
// gives a vector back while an unbind is still on its way to it. See `device::settled_vectors` and
// `MsiRegistry::has_unbound`, which states why a quarantined slot is settled rather than outstanding.
pub fn msi_live_for_device(device: u32) -> bool {
	REGISTRY.has_unbound(device)
}

// How many of this device's slots are QUARANTINED - a vector stranded by a teardown the controller
// refused. Sampled either side of a release so the ones stranded by THAT release can be told from
// the ones it inherited. See `MsiRegistry::quarantined_for` and `device::release_claim`.
pub fn msi_quarantined_for_device(device: u32) -> usize {
	REGISTRY.quarantined_for(device)
}

pub fn msi_held_by_device(device: u32) -> usize {
	REGISTRY.held_by_device(device)
}

pub fn release_msi_for_device(device: u32) -> usize {
	REGISTRY.release_for_device(device)
}

// The number of vectors irq_info reports over.
pub fn irq_info_len() -> usize {
	IRQ_COUNT + MSI_COUNT
}

// Map a device's MSI-X table page uncacheable and write entry 0: message address
// 0xFEE00000 | dest<<12 (physical destination, fixed delivery), message data = the
// allocated vector (edge-triggered), vector control = 0 (unmasked). A driver must
// never write its own MSI-X table; only the kernel programs it here.
fn program_msix_entry(slot: usize, table_phys: u64, vector: u8, dest: u8) {
	let virt = msix_virt(slot);
	let offset = table_phys & 0xfff;
	// Where in the page this device's entry sits, kept so the teardown can mask the SAME words -
	// and so it knows whether the entry crossed into a second page. The table is page-mapped but
	// the entry need not be at offset 0, and a mask written at a guessed offset would corrupt
	// whatever the device keeps there instead.
	MSIX_ENTRY_OFFSET[slot].store(offset as u32, Ordering::Release);
	let flags = super::paging::WRITABLE | super::paging::NO_CACHE | super::paging::NO_EXECUTE;
	for page in 0..msix_pages_for_entry(offset) {
		super::paging::map_page(virt + page * 0x1000, (table_phys & !0xfff) + page * 0x1000, flags);
	}
	let entry = (virt + offset) as *mut u32;
	let msg_addr: u32 = 0xFEE0_0000 | ((dest as u32) << 12);
	unsafe {
		entry.add(0).write_volatile(msg_addr); // message address low
		entry.add(1).write_volatile(0); // message address high
		entry.add(2).write_volatile(vector as u32); // message data
		entry.add(3).write_volatile(0); // vector control (unmasked)
	}
}

// Mask the device's MSI-X entry and take its table page back out of the kernel window.
//
// The order is the point: the entry is masked (vector control bit 0) while the page is still
// mapped, so the device stops raising the vector BEFORE the mapping that could stop it goes away.
// Then the page is unmapped, which is what makes the slot reusable - `program_msix_entry` maps at a
// fixed per-slot address and mapping over a live leaf is refused.
fn mask_and_unmap_msix_entry(slot: usize) {
	let virt = msix_virt(slot);
	// Only if this slot actually has a page mapped: `free` can be reached for a slot that was
	// acquired and never programmed.
	if super::paging::translate(virt).is_some() {
		let offset = MSIX_ENTRY_OFFSET[slot].load(Ordering::Acquire) as u64;
		let entry = (virt + offset) as *mut u32;
		unsafe {
			// Vector control bit 0 = masked. The device may still have a message in flight; masking
			// stops the next one, which is all this can promise without the driver's cooperation.
			entry.add(3).write_volatile(1);
		}
		// EVERY PAGE THIS SLOT MAPPED, in reverse - the mask above is written through the second
		// one when the entry straddles, so it has to go last.
		for page in (0..msix_pages_for_entry(offset)).rev() {
			super::paging::unmap_page(virt + page * 0x1000);
		}
	}
}

// Bind `intr` to an MSI `vector` so dispatch wakes it when the vector fires (the MSI
// sibling of bind()). Returns false if the vector is already bound to a live Interrupt.
pub fn bind_msi(vector: u32, intr: &Arc<Interrupt>) -> bool {
	if !is_msi(vector) {
		return false;
	}
	REGISTRY.bind((vector - MSI_BASE as u32) as usize, intr)
}

// End-of-interrupt for a serviced vector. x86 MSI is edge-triggered and the LAPIC EOI
// is issued from the ISR stub, so there is no source to complete here: a no-op, kept for
// the portable SYS_INTERRUPT_ACK path (the riscv PLIC completes its level source here).
pub fn eoi(_vector: u32) {}

// Whether `vector` currently has a live driver binding. Used to confirm that a
// crashed driver's IRQ was detached during cleanup.
pub fn is_bound(vector: u32) -> bool {
	if is_msi(vector) {
		return REGISTRY.is_bound((vector - MSI_BASE as u32) as usize);
	}
	let index = vector.wrapping_sub(IRQ_BASE as u32) as usize;
	if index >= IRQ_COUNT {
		return false;
	}
	BOUND[index].lock().as_ref().and_then(Weak::upgrade).is_some()
}

// Register `handler` for a device-interrupt `vector` (IRQ_BASE..IRQ_BASE+IRQ_COUNT).
pub fn register(vector: u32, handler: HandlerFn) {
	let index = (vector - IRQ_BASE as u32) as usize;
	HANDLERS[index].store(handler as usize, Ordering::SeqCst);
}

// Common interrupt path: invoke the registered handler (if any), then EOI.
fn dispatch(vector: u8) {
	// A gate does not clear EFLAGS.AC: drop any user-set (or interrupted-window)
	// AC so the handler runs with SMAP enforced; iretq restores the interrupted
	// context's own AC.
	super::paging::clac_on_entry();
	let index = (vector - IRQ_BASE) as usize;
	let raw = HANDLERS[index].load(Ordering::SeqCst);
	if raw != 0 {
		let handler: HandlerFn = unsafe { core::mem::transmute::<usize, HandlerFn>(raw) };
		handler(vector as u32);
	}
	// Deliver to a userspace driver bound to this vector, if any.
	if let Some(intr) = BOUND[index].lock().as_ref().and_then(Weak::upgrade) {
		intr.signal();
	}
	apic::eoi();
}

// MSI dispatch: edge-triggered, so just wake the bound driver and EOI - no
// mask/unmask dance (there is no shared level line to gate, unlike the INTx path).
fn dispatch_msi(vector: u8) {
	super::paging::clac_on_entry();
	REGISTRY.dispatch((vector - MSI_BASE) as usize);
	apic::eoi();
}

macro_rules! irq_stub {
	($name:ident, $vector:expr_2021) => {
		extern "x86-interrupt" fn $name(_frame: InterruptStackFrame) {
			dispatch($vector);
		}
	};
}

irq_stub!(irq0, 32);
irq_stub!(irq1, 33);
irq_stub!(irq2, 34);
irq_stub!(irq3, 35);
irq_stub!(irq4, 36);
irq_stub!(irq5, 37);
irq_stub!(irq6, 38);
irq_stub!(irq7, 39);
irq_stub!(irq8, 40);
irq_stub!(irq9, 41);
irq_stub!(irq10, 42);
irq_stub!(irq11, 43);
irq_stub!(irq12, 44);
irq_stub!(irq13, 45);
irq_stub!(irq14, 46);
irq_stub!(irq15, 47);

// Build the MSI stub table: the x86-interrupt ABI passes no vector number, so
// every vector needs its own tiny entry point - the macro mints one anonymous
// stub per listed vector and collects their function pointers.
macro_rules! msi_stubs {
	($($v:literal),* $(,)?) => {
		[$({
			extern "x86-interrupt" fn stub(_frame: InterruptStackFrame) {
				dispatch_msi($v);
			}
			stub as extern "x86-interrupt" fn(InterruptStackFrame)
		}),*]
	};
}

#[rustfmt::skip]
const MSI_STUBS: [extern "x86-interrupt" fn(InterruptStackFrame); MSI_COUNT] = msi_stubs![
	48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63,
	64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79,
	80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95,
	96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111,
	112, 113, 114, 115, 116, 117, 118, 119, 120, 121, 122, 123, 124, 125, 126, 127,
	128, 129, 130, 131, 132, 133, 134, 135, 136, 137, 138, 139, 140, 141, 142, 143,
	144, 145, 146, 147, 148, 149, 150, 151, 152, 153, 154, 155, 156, 157, 158, 159,
	160, 161, 162, 163, 164, 165, 166, 167, 168, 169, 170, 171, 172, 173, 174, 175,
	176, 177, 178, 179, 180, 181, 182, 183, 184, 185, 186, 187, 188, 189, 190, 191,
	192, 193, 194, 195, 196, 197, 198, 199, 200, 201, 202, 203, 204, 205, 206, 207,
	208, 209, 210, 211, 212, 213, 214, 215, 216, 217, 218, 219, 220, 221, 222, 223,
	224, 225, 226, 227, 228, 229, 230, 231, 232, 233, 234, 235, 236, 237, 238, 239,
];

// Spurious LAPIC interrupts must not signal EOI, so they bypass the dispatcher.
extern "x86-interrupt" fn spurious(_frame: InterruptStackFrame) {}

// The cross-core wake IPI: the delivery itself is the whole message (it bounces a
// halted core out of HLT so its idle loop re-checks the run queue), so the handler
// only acknowledges it.
extern "x86-interrupt" fn wake_ipi(_frame: InterruptStackFrame) {
	// The wake IPI carries two errands now: bounce a halted core into its run queue, and
	// answer a TLB shootdown. Both are "look at something you were told about", and one
	// interrupt is cheaper than two vectors.
	crate::mem::tlb::service_pending();
	apic::eoi();
}

// The LAPIC timer vector. Unlike the generic IRQ stubs it preempts: after counting
// the tick and signalling EOI, it rotates to the next ready thread. EOI is sent
// BEFORE the switch so the LAPIC keeps delivering ticks while this thread is
// descheduled. Ring 3 is preempted too: its interrupt frame lands on the thread's
// own kernel stack (per-thread TSS.RSP0, retargeted by the scheduler and by
// usermode::enter), so the switch travels with the thread; the CPL is passed down
// so a killed process spinning in ring 3 can be retired at its next tick.
extern "x86-interrupt" fn timer(frame: InterruptStackFrame) {
	// The timer is the one entry that context-switches: without this, an AC set by
	// the interrupted context (ring-3 code may set it freely) would leak into the
	// next thread's kernel execution, suspending SMAP there.
	super::paging::clac_on_entry();
	apic::on_timer_tick();
	apic::eoi();
	crate::sched::on_timer_preempt(frame.code_segment & 3 == 3);
}

const STUBS: [extern "x86-interrupt" fn(InterruptStackFrame); IRQ_COUNT] = [irq0, irq1, irq2, irq3, irq4, irq5, irq6, irq7, irq8, irq9, irq10, irq11, irq12, irq13, irq14, irq15];

// Install the IRQ stubs and the spurious handler into the IDT.
pub fn init() {
	for (i, stub) in STUBS.iter().enumerate() {
		idt::set_gate(IRQ_BASE as usize + i, *stub);
	}
	// MSI-X vectors get their own edge-triggered stubs in the band above the INTx window.
	for (i, stub) in MSI_STUBS.iter().enumerate() {
		idt::set_gate(MSI_BASE as usize + i, *stub);
	}
	// The cross-core wake IPI: delivery is the message, the handler only EOIs.
	idt::set_gate(WAKE_VECTOR as usize, wake_ipi);
	// Materialise the MSI-X table mapping region's page tables now, while the kernel
	// PML4 is active and before any process address space is created, so later per-device
	// mappings under it land in the shared kernel half and are visible everywhere.
	super::paging::map_page(MSIX_VIRT_BASE, 0, super::paging::WRITABLE | super::paging::NO_EXECUTE);
	super::paging::unmap_page(MSIX_VIRT_BASE);
	// The timer vector preempts, so it gets a dedicated stub instead of the generic
	// count-and-dispatch path.
	idt::set_gate(TIMER_VECTOR as usize, timer);
	idt::set_gate(SPURIOUS_VECTOR as usize, spurious);
}

#[cfg(test)]
mod tests;
