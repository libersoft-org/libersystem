// Hardware-interrupt dispatch and handler registration.
//
// Device interrupts land on vectors IRQ_BASE..IRQ_BASE+IRQ_COUNT. Each vector
// has a small stub in the IDT that funnels into a common dispatcher, which looks
// up a registered handler and signals end-of-interrupt to the LAPIC.
//
// The handler table is lock-free (an array of atomics): registration only stores
// a function pointer, and dispatch only loads one, so it is safe to call from
// interrupt context without risking a deadlock against a held lock.

#![allow(dead_code)]

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use alloc::sync::{Arc, Weak};

use super::apic;
use super::idt::{self, InterruptStackFrame};
use crate::object::interrupt::Interrupt;
use crate::sync::SpinLock;

// Device-interrupt vector window (mirrors the legacy 16-IRQ layout).
pub const IRQ_BASE: u8 = 32;
pub const IRQ_COUNT: usize = 16;

// Well-known vectors.
pub const TIMER_VECTOR: u8 = IRQ_BASE; // IRQ 0
pub const SPURIOUS_VECTOR: u8 = 0xff;

// MSI-X vector window: per-device edge-triggered vectors delivered straight to a
// LAPIC, with no INTx sharing. Sits just above the legacy INTx window (32..48).
// 64 vectors leave headroom for many devices (and future multi-queue ones); the
// window still ends well below the spurious vector at 0xff.
pub const MSI_BASE: u8 = IRQ_BASE + IRQ_COUNT as u8; // 48
pub const MSI_COUNT: usize = 64;

pub type HandlerFn = fn(u8);

static HANDLERS: [AtomicUsize; IRQ_COUNT] = [const { AtomicUsize::new(0) }; IRQ_COUNT];

// Userspace-driver bindings: the Interrupt object to wake when each device vector
// fires. Held weakly, so closing the driver's handle (its Interrupt's Drop) clears
// the binding and the kernel stops delivering to a gone driver.
static BOUND: [SpinLock<Option<Weak<Interrupt>>>; IRQ_COUNT] = [const { SpinLock::new(None) }; IRQ_COUNT];

// MSI-X driver bindings (the MSI sibling of BOUND): the Interrupt to wake when each
// MSI vector fires, held weakly so a gone driver clears its own binding.
static MSI_BOUND: [SpinLock<Option<Weak<Interrupt>>>; MSI_COUNT] = [const { SpinLock::new(None) }; MSI_COUNT];

// Reservation flag per MSI slot, set when acquire_msi hands the vector out and
// cleared on unbind so the slot can be re-used.
static MSI_USED: [AtomicBool; MSI_COUNT] = [const { AtomicBool::new(false) }; MSI_COUNT];

// The discovered-device index each MSI slot was acquired for (u32::MAX = none),
// retained so the vector-to-device mapping stays inspectable - SYS_IRQ_INFO reads
// it for `lsirq`.
static MSI_OWNER: [AtomicU32; MSI_COUNT] = [const { AtomicU32::new(u32::MAX) }; MSI_COUNT];

// Kernel virtual base for mapping device MSI-X tables (uncacheable), clear of the
// LAPIC (0xffff_f100) / IOAPIC (0xffff_f200) MMIO windows. One page per MSI slot; the
// page-table chain is materialised at init (kernel PML4 active, before any process
// address space exists) so runtime per-device mappings under it propagate to every
// address space's shared kernel half.
const MSIX_VIRT_BASE: u64 = 0xffff_f300_0000_0000;

// Whether `vector` is a kernel MSI-X vector.
fn is_msi(vector: u8) -> bool {
	vector >= MSI_BASE && (vector as usize) < MSI_BASE as usize + MSI_COUNT
}

// Whether `vector` is a device-IRQ vector a driver may bind. The timer vector
// (IRQ_BASE) is the kernel's own and is never handed out.
pub fn is_bindable(vector: u8) -> bool {
	vector > IRQ_BASE && vector < IRQ_BASE + IRQ_COUNT as u8
}

// Bind `intr` to `vector` so the dispatch path wakes it when the vector fires.
// Returns false if the vector is already bound to a live Interrupt.
pub fn bind(vector: u8, intr: &Arc<Interrupt>) -> bool {
	let index = (vector - IRQ_BASE) as usize;
	let mut slot = BOUND[index].lock();
	if slot.as_ref().and_then(Weak::upgrade).is_some() {
		return false;
	}
	*slot = Some(Arc::downgrade(intr));
	intr.mark_bound();
	true
}

// Remove any binding for `vector` (called from an Interrupt's Drop).
pub fn unbind(vector: u8) {
	if is_msi(vector) {
		// MSI is edge-triggered and unshared: just drop the binding and free the slot
		// (there is no source to mask). A gone driver's device simply stops being drained.
		let index = (vector - MSI_BASE) as usize;
		*MSI_BOUND[index].lock() = None;
		MSI_OWNER[index].store(u32::MAX, Ordering::Release);
		MSI_USED[index].store(false, Ordering::Release);
		return;
	}
	let index = vector.wrapping_sub(IRQ_BASE) as usize;
	if index < IRQ_COUNT {
		*BOUND[index].lock() = None;
	}
}

// Allocate a free MSI vector and program a device's MSI-X table entry 0 so the device
// delivers it to LAPIC `dest` (edge-triggered, fixed delivery, unmasked). `table_phys`
// is the physical address of the device's MSI-X table. Returns the vector (None if
// every MSI slot is taken); the caller enables MSI-X on the device and binds an
// Interrupt to the returned vector with bind_msi. `owner` is the discovered-device
// index the vector is acquired for, retained for the `lsirq` inventory.
pub fn acquire_msi(table_phys: u64, dest: u8, owner: u32) -> Option<u8> {
	for index in 0..MSI_COUNT {
		if MSI_USED[index].compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
			continue;
		}
		MSI_OWNER[index].store(owner, Ordering::Release);
		let vector = MSI_BASE + index as u8;
		program_msix_entry(index, table_phys, vector, dest);
		return Some(vector);
	}
	None
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
		let bound = handled || is_bound(vector);
		return Some(abi::IrqInfo { vector: vector as u32, kind: abi::IRQ_KIND_FIXED, bound: bound as u32, device: abi::IRQ_NO_DEVICE });
	}
	let slot = index - IRQ_COUNT;
	if slot >= MSI_COUNT {
		return None;
	}
	let vector = MSI_BASE + slot as u8;
	Some(abi::IrqInfo { vector: vector as u32, kind: abi::IRQ_KIND_MSI, bound: is_bound(vector) as u32, device: MSI_OWNER[slot].load(Ordering::Acquire) })
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
	let virt = MSIX_VIRT_BASE + slot as u64 * 0x1000;
	super::paging::map_page(virt, table_phys & !0xfff, super::paging::WRITABLE | super::paging::NO_CACHE);
	let entry = (virt + (table_phys & 0xfff)) as *mut u32;
	let msg_addr: u32 = 0xFEE0_0000 | ((dest as u32) << 12);
	unsafe {
		entry.add(0).write_volatile(msg_addr); // message address low
		entry.add(1).write_volatile(0); // message address high
		entry.add(2).write_volatile(vector as u32); // message data
		entry.add(3).write_volatile(0); // vector control (unmasked)
	}
}

// Bind `intr` to an MSI `vector` so dispatch wakes it when the vector fires (the MSI
// sibling of bind()). Returns false if the vector is already bound to a live Interrupt.
pub fn bind_msi(vector: u8, intr: &Arc<Interrupt>) -> bool {
	let index = (vector - MSI_BASE) as usize;
	let mut slot = MSI_BOUND[index].lock();
	if slot.as_ref().and_then(Weak::upgrade).is_some() {
		return false;
	}
	*slot = Some(Arc::downgrade(intr));
	intr.mark_bound();
	true
}

// Whether `vector` currently has a live driver binding. Used to confirm that a
// crashed driver's IRQ was detached during cleanup.
pub fn is_bound(vector: u8) -> bool {
	if is_msi(vector) {
		let index = (vector - MSI_BASE) as usize;
		return MSI_BOUND[index].lock().as_ref().and_then(Weak::upgrade).is_some();
	}
	let index = vector.wrapping_sub(IRQ_BASE) as usize;
	if index >= IRQ_COUNT {
		return false;
	}
	BOUND[index].lock().as_ref().and_then(Weak::upgrade).is_some()
}

// Register `handler` for a device-interrupt `vector` (IRQ_BASE..IRQ_BASE+IRQ_COUNT).
pub fn register(vector: u8, handler: HandlerFn) {
	let index = (vector - IRQ_BASE) as usize;
	HANDLERS[index].store(handler as usize, Ordering::SeqCst);
}

// Common interrupt path: invoke the registered handler (if any), then EOI.
fn dispatch(vector: u8) {
	let index = (vector - IRQ_BASE) as usize;
	let raw = HANDLERS[index].load(Ordering::SeqCst);
	if raw != 0 {
		let handler: HandlerFn = unsafe { core::mem::transmute::<usize, HandlerFn>(raw) };
		handler(vector);
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
	let index = (vector - MSI_BASE) as usize;
	if let Some(intr) = MSI_BOUND[index].lock().as_ref().and_then(Weak::upgrade) {
		intr.signal();
	}
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

macro_rules! msi_stub {
	($name:ident, $vector:expr_2021) => {
		extern "x86-interrupt" fn $name(_frame: InterruptStackFrame) {
			dispatch_msi($vector);
		}
	};
}

msi_stub!(msi0, 48);
msi_stub!(msi1, 49);
msi_stub!(msi2, 50);
msi_stub!(msi3, 51);
msi_stub!(msi4, 52);
msi_stub!(msi5, 53);
msi_stub!(msi6, 54);
msi_stub!(msi7, 55);
msi_stub!(msi8, 56);
msi_stub!(msi9, 57);
msi_stub!(msi10, 58);
msi_stub!(msi11, 59);
msi_stub!(msi12, 60);
msi_stub!(msi13, 61);
msi_stub!(msi14, 62);
msi_stub!(msi15, 63);
msi_stub!(msi16, 64);
msi_stub!(msi17, 65);
msi_stub!(msi18, 66);
msi_stub!(msi19, 67);
msi_stub!(msi20, 68);
msi_stub!(msi21, 69);
msi_stub!(msi22, 70);
msi_stub!(msi23, 71);
msi_stub!(msi24, 72);
msi_stub!(msi25, 73);
msi_stub!(msi26, 74);
msi_stub!(msi27, 75);
msi_stub!(msi28, 76);
msi_stub!(msi29, 77);
msi_stub!(msi30, 78);
msi_stub!(msi31, 79);
msi_stub!(msi32, 80);
msi_stub!(msi33, 81);
msi_stub!(msi34, 82);
msi_stub!(msi35, 83);
msi_stub!(msi36, 84);
msi_stub!(msi37, 85);
msi_stub!(msi38, 86);
msi_stub!(msi39, 87);
msi_stub!(msi40, 88);
msi_stub!(msi41, 89);
msi_stub!(msi42, 90);
msi_stub!(msi43, 91);
msi_stub!(msi44, 92);
msi_stub!(msi45, 93);
msi_stub!(msi46, 94);
msi_stub!(msi47, 95);
msi_stub!(msi48, 96);
msi_stub!(msi49, 97);
msi_stub!(msi50, 98);
msi_stub!(msi51, 99);
msi_stub!(msi52, 100);
msi_stub!(msi53, 101);
msi_stub!(msi54, 102);
msi_stub!(msi55, 103);
msi_stub!(msi56, 104);
msi_stub!(msi57, 105);
msi_stub!(msi58, 106);
msi_stub!(msi59, 107);
msi_stub!(msi60, 108);
msi_stub!(msi61, 109);
msi_stub!(msi62, 110);
msi_stub!(msi63, 111);

// Spurious LAPIC interrupts must not signal EOI, so they bypass the dispatcher.
extern "x86-interrupt" fn spurious(_frame: InterruptStackFrame) {}

// The LAPIC timer vector. Unlike the generic IRQ stubs it preempts: after counting
// the tick and signalling EOI, it rotates to the next ready thread when it
// interrupted ring-0 thread code. EOI is sent BEFORE the switch so the LAPIC keeps
// delivering ticks while this thread is descheduled. Ring-3 is not preempted yet:
// its interrupt frame lands on the shared per-core RSP0 stack, so context-switching
// from here would not travel with the thread (that needs a per-thread RSP0).
extern "x86-interrupt" fn timer(frame: InterruptStackFrame) {
	apic::on_timer_tick();
	apic::eoi();
	if frame.code_segment & 3 == 0 {
		crate::sched::on_timer_preempt();
	}
}

const STUBS: [extern "x86-interrupt" fn(InterruptStackFrame); IRQ_COUNT] = [irq0, irq1, irq2, irq3, irq4, irq5, irq6, irq7, irq8, irq9, irq10, irq11, irq12, irq13, irq14, irq15];

const MSI_STUBS: [extern "x86-interrupt" fn(InterruptStackFrame); MSI_COUNT] = [msi0, msi1, msi2, msi3, msi4, msi5, msi6, msi7, msi8, msi9, msi10, msi11, msi12, msi13, msi14, msi15, msi16, msi17, msi18, msi19, msi20, msi21, msi22, msi23, msi24, msi25, msi26, msi27, msi28, msi29, msi30, msi31, msi32, msi33, msi34, msi35, msi36, msi37, msi38, msi39, msi40, msi41, msi42, msi43, msi44, msi45, msi46, msi47, msi48, msi49, msi50, msi51, msi52, msi53, msi54, msi55, msi56, msi57, msi58, msi59, msi60, msi61, msi62, msi63];

// Install the IRQ stubs and the spurious handler into the IDT.
pub fn init() {
	for (i, stub) in STUBS.iter().enumerate() {
		idt::set_gate(IRQ_BASE as usize + i, *stub);
	}
	// MSI-X vectors get their own edge-triggered stubs in the band above the INTx window.
	for (i, stub) in MSI_STUBS.iter().enumerate() {
		idt::set_gate(MSI_BASE as usize + i, *stub);
	}
	// Materialise the MSI-X table mapping region's page tables now, while the kernel
	// PML4 is active and before any process address space is created, so later per-device
	// mappings under it land in the shared kernel half and are visible everywhere.
	super::paging::map_page(MSIX_VIRT_BASE, 0, super::paging::WRITABLE);
	super::paging::unmap_page(MSIX_VIRT_BASE);
	// The timer vector preempts, so it gets a dedicated stub instead of the generic
	// count-and-dispatch path.
	idt::set_gate(TIMER_VECTOR as usize, timer);
	idt::set_gate(SPURIOUS_VECTOR as usize, spurious);
}
