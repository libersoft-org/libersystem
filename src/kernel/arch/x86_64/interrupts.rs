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

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

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

pub type HandlerFn = fn(u8);

static HANDLERS: [AtomicUsize; IRQ_COUNT] = [const { AtomicUsize::new(0) }; IRQ_COUNT];

// Userspace-driver bindings: the Interrupt object to wake when each device vector
// fires. Held weakly, so closing the driver's handle (its Interrupt's Drop) clears
// the binding and the kernel stops delivering to a gone driver.
static BOUND: [SpinLock<Option<Weak<Interrupt>>>; IRQ_COUNT] = [const { SpinLock::new(None) }; IRQ_COUNT];

// The GSI routed to each device-IRQ vector (NO_GSI = none). Tracked so the kernel
// can mask the source on delivery (until the driver acks, so a level-triggered
// device cannot re-storm) and on release (a gone driver's device falls silent).
const NO_GSI: u32 = u32::MAX;
static ROUTED_GSI: [AtomicU32; IRQ_COUNT] = [const { AtomicU32::new(NO_GSI) }; IRQ_COUNT];

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
	let index = vector.wrapping_sub(IRQ_BASE) as usize;
	if index < IRQ_COUNT {
		*BOUND[index].lock() = None;
		// If this vector routed a device GSI, mask the source so the gone driver's
		// device cannot keep interrupting, and free the slot.
		let gsi = ROUTED_GSI[index].swap(NO_GSI, Ordering::AcqRel);
		if gsi != NO_GSI {
			super::ioapic::mask(gsi);
		}
	}
}

// Route a device's `gsi` to a free device-IRQ vector delivered to LAPIC `dest`, and
// return that vector (None if every slot is taken). The vector lands in the
// IRQ_BASE.. window the IDT stubs cover; PCI INTx is level-triggered active-low.
pub fn acquire(gsi: u32, dest: u8) -> Option<u8> {
	for index in 1..IRQ_COUNT {
		if BOUND[index].lock().as_ref().and_then(Weak::upgrade).is_some() {
			continue;
		}
		// Reserve this slot's GSI atomically; skip it if another acquire took it first.
		if ROUTED_GSI[index].compare_exchange(NO_GSI, gsi, Ordering::AcqRel, Ordering::Acquire).is_err() {
			continue;
		}
		let vector = IRQ_BASE + index as u8;
		super::ioapic::route(gsi, vector, dest, true);
		return Some(vector);
	}
	None
}

// Re-arm a device vector after its driver has serviced the interrupt: unmask its
// GSI so the device can interrupt again (a no-op for a vector with no routed GSI).
pub fn ack(vector: u8) {
	let index = vector.wrapping_sub(IRQ_BASE) as usize;
	if index >= IRQ_COUNT {
		return;
	}
	let gsi = ROUTED_GSI[index].load(Ordering::Acquire);
	if gsi != NO_GSI {
		super::ioapic::unmask(gsi);
	}
}

// Whether `vector` currently has a live driver binding. Used to confirm that a
// crashed driver's IRQ was detached during cleanup.
pub fn is_bound(vector: u8) -> bool {
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
		// Mask a routed device source until the driver acks, so a level-triggered
		// device cannot re-storm before its driver has serviced it.
		let gsi = ROUTED_GSI[index].load(Ordering::Acquire);
		if gsi != NO_GSI {
			super::ioapic::mask(gsi);
		}
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

// Install the IRQ stubs and the spurious handler into the IDT.
pub fn init() {
	for (i, stub) in STUBS.iter().enumerate() {
		idt::set_gate(IRQ_BASE as usize + i, *stub);
	}
	// The timer vector preempts, so it gets a dedicated stub instead of the generic
	// count-and-dispatch path.
	idt::set_gate(TIMER_VECTOR as usize, timer);
	idt::set_gate(SPURIOUS_VECTOR as usize, spurious);
}
