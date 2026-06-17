// Interrupt Descriptor Table and basic CPU exception handlers (x86_64).
//
// All 32 architectural exception vectors are populated so nothing escalates to a
// triple fault silently. A handful of vectors get specific, informative
// handlers; the rest share a generic handler (split by whether the CPU pushes a
// hardware error code, since that changes the stack-frame ABI).

use core::arch::asm;
use core::mem::size_of;
use core::ptr::{addr_of, addr_of_mut};

use super::gdt::{DOUBLE_FAULT_IST_INDEX, KERNEL_CODE_SELECTOR};

#[derive(Clone, Copy)]
#[repr(C)]
struct IdtEntry {
	offset_low: u16,
	selector: u16,
	ist: u8,
	type_attr: u8,
	offset_mid: u16,
	offset_high: u32,
	reserved: u32,
}

impl IdtEntry {
	const fn missing() -> Self {
		Self { offset_low: 0, selector: 0, ist: 0, type_attr: 0, offset_mid: 0, offset_high: 0, reserved: 0 }
	}

	fn set_addr(&mut self, handler: u64, ist: u8) {
		self.offset_low = handler as u16;
		self.offset_mid = (handler >> 16) as u16;
		self.offset_high = (handler >> 32) as u32;
		self.selector = KERNEL_CODE_SELECTOR;
		self.ist = ist & 0x7;
		self.type_attr = 0x8E; // present, DPL=0, 64-bit interrupt gate
		self.reserved = 0;
	}

	fn set(&mut self, handler: extern "x86-interrupt" fn(InterruptStackFrame), ist: u8) {
		self.set_addr(handler as usize as u64, ist);
	}

	fn set_with_code(&mut self, handler: extern "x86-interrupt" fn(InterruptStackFrame, u64), ist: u8) {
		self.set_addr(handler as usize as u64, ist);
	}

	fn set_diverging(&mut self, handler: extern "x86-interrupt" fn(InterruptStackFrame, u64) -> !, ist: u8) {
		self.set_addr(handler as usize as u64, ist);
	}
}

// The stack frame the CPU pushes when entering an interrupt gate.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct InterruptStackFrame {
	pub instruction_pointer: u64,
	pub code_segment: u64,
	pub cpu_flags: u64,
	pub stack_pointer: u64,
	pub stack_segment: u64,
}

static mut IDT: [IdtEntry; 256] = [IdtEntry::missing(); 256];

#[repr(C, packed)]
struct DescriptorPointer {
	limit: u16,
	base: u64,
}

// Architectural exception vectors that push a hardware error code.
const fn has_error_code(vector: usize) -> bool {
	matches!(vector, 8 | 10 | 11 | 12 | 13 | 14 | 17 | 21 | 29 | 30)
}

pub fn init() {
	unsafe {
		let idt = &mut *addr_of_mut!(IDT);

		// Default every architectural exception first, then override the ones we
		// give dedicated handlers.
		let mut v = 0;
		while v < 32 {
			if has_error_code(v) {
				idt[v].set_with_code(generic_with_code, 0);
			} else {
				idt[v].set(generic, 0);
			}
			v += 1;
		}

		idt[0].set(divide_error, 0);
		idt[3].set(breakpoint, 0);
		idt[6].set(invalid_opcode, 0);
		idt[8].set_diverging(double_fault, DOUBLE_FAULT_IST_INDEX);
		idt[13].set_with_code(general_protection_fault, 0);
		idt[14].set_with_code(page_fault, 0);

		load();
	}
}

// Load the IDT register on the running core. The IDT array is shared and built
// once on the BSP; each application processor calls this to point its IDTR at it.
pub fn load() {
	unsafe {
		let ptr = DescriptorPointer { limit: (size_of::<[IdtEntry; 256]>() - 1) as u16, base: addr_of!(IDT) as u64 };
		asm!("lidt [{}]", in(reg) &ptr, options(readonly, nostack, preserves_flags));
	}
}

// Install a handler for a (typically hardware-interrupt) vector. Safe to call
// after the IDT is loaded: the CPU reads the table live on each interrupt, so
// adding gates before interrupts are enabled needs no reload.
pub fn set_gate(vector: usize, handler: extern "x86-interrupt" fn(InterruptStackFrame)) {
	unsafe {
		let idt = &mut *addr_of_mut!(IDT);
		idt[vector].set(handler, 0);
	}
}

extern "x86-interrupt" fn divide_error(frame: InterruptStackFrame) {
	crate::serial_println!("EXCEPTION: divide error at {:#x}", frame.instruction_pointer);
	super::halt_loop();
}

// Breakpoint is recoverable: report and return so execution continues past int3.
extern "x86-interrupt" fn breakpoint(frame: InterruptStackFrame) {
	crate::serial_println!("EXCEPTION: breakpoint at {:#x} (continuing)", frame.instruction_pointer);
}

extern "x86-interrupt" fn invalid_opcode(frame: InterruptStackFrame) {
	crate::serial_println!("EXCEPTION: invalid opcode at {:#x}", frame.instruction_pointer);
	super::halt_loop();
}

extern "x86-interrupt" fn double_fault(frame: InterruptStackFrame, error_code: u64) -> ! {
	crate::serial_println!("EXCEPTION: DOUBLE FAULT (code {:#x}) at {:#x}", error_code, frame.instruction_pointer);
	super::halt_loop();
}

extern "x86-interrupt" fn general_protection_fault(frame: InterruptStackFrame, error_code: u64) {
	// A #GP taken in ring 3 is a userspace bug: terminate that process and return
	// to the kernel. The low two bits of the saved code selector are the CPL.
	if frame.code_segment & 3 == 3 {
		crate::serial_println!("fault: ring-3 #GP (code {:#x}) at {:#x} - terminating process", error_code, frame.instruction_pointer);
		crate::fault::terminate_user(crate::fault::FaultInfo { kind: crate::fault::FAULT_GENERAL_PROTECTION, error_code, address: 0, instruction_pointer: frame.instruction_pointer });
	}
	// In ring 0 it is a kernel bug; halt loudly.
	crate::serial_println!("EXCEPTION: general protection fault (code {:#x}) at {:#x}", error_code, frame.instruction_pointer);
	super::halt_loop();
}

extern "x86-interrupt" fn page_fault(frame: InterruptStackFrame, error_code: u64) {
	let cr2: u64;
	unsafe {
		asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack, preserves_flags));
	}
	// A page fault taken in ring 3 is a userspace bug (a bad dereference, a write
	// to a read-only page, and so on): terminate that process and return to the
	// kernel. The low two bits of the saved code selector are the CPL.
	if frame.code_segment & 3 == 3 {
		crate::serial_println!("fault: ring-3 page fault (code {:#x}) at {:#x}, CR2 = {:#x} - terminating process", error_code, frame.instruction_pointer, cr2);
		crate::fault::terminate_user(crate::fault::FaultInfo { kind: crate::fault::FAULT_PAGE, error_code, address: cr2, instruction_pointer: frame.instruction_pointer });
	}
	// In ring 0 it is a kernel bug; halt loudly.
	crate::serial_println!("EXCEPTION: page fault (code {:#x}) at {:#x}, CR2 = {:#x}", error_code, frame.instruction_pointer, cr2);
	super::halt_loop();
}

extern "x86-interrupt" fn generic(frame: InterruptStackFrame) {
	crate::serial_println!("EXCEPTION: unhandled fault at {:#x}", frame.instruction_pointer);
	super::halt_loop();
}

extern "x86-interrupt" fn generic_with_code(frame: InterruptStackFrame, error_code: u64) {
	crate::serial_println!("EXCEPTION: unhandled fault (code {:#x}) at {:#x}", error_code, frame.instruction_pointer);
	super::halt_loop();
}
