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

use core::sync::atomic::{AtomicUsize, Ordering};

use super::apic;
use super::idt::{self, InterruptStackFrame};

// Device-interrupt vector window (mirrors the legacy 16-IRQ layout).
pub const IRQ_BASE: u8 = 32;
pub const IRQ_COUNT: usize = 16;

// Well-known vectors.
pub const TIMER_VECTOR: u8 = IRQ_BASE; // IRQ 0
pub const SPURIOUS_VECTOR: u8 = 0xff;

pub type HandlerFn = fn(u8);

static HANDLERS: [AtomicUsize; IRQ_COUNT] = [const { AtomicUsize::new(0) }; IRQ_COUNT];

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
	apic::eoi();
}

macro_rules! irq_stub {
	($name:ident, $vector:expr) => {
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

const STUBS: [extern "x86-interrupt" fn(InterruptStackFrame); IRQ_COUNT] = [irq0, irq1, irq2, irq3, irq4, irq5, irq6, irq7, irq8, irq9, irq10, irq11, irq12, irq13, irq14, irq15];

// Install the IRQ stubs and the spurious handler into the IDT.
pub fn init() {
	for (i, stub) in STUBS.iter().enumerate() {
		idt::set_gate(IRQ_BASE as usize + i, *stub);
	}
	idt::set_gate(SPURIOUS_VECTOR as usize, spurious);
}
