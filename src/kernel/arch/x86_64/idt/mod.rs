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
//
// The two tables at the bottom of this file split the generic vectors by exactly this, and a vector
// filed on the wrong side reads the frame at the wrong offset - a fault whose handler then reports
// nonsense or faults itself. So this stays as the architecture's own answer and
// `every_vector_is_filed_by_whether_it_pushes_a_code` compares the tables against it rather than
// against a second reading of the manual.
#[cfg(test)]
pub(super) const fn has_error_code(vector: usize) -> bool {
	matches!(vector, 8 | 10 | 11 | 12 | 13 | 14 | 17 | 21 | 29 | 30)
}

pub fn init() {
	unsafe {
		let idt = &mut *addr_of_mut!(IDT);

		// Every architectural vector gets a handler that knows its own number; the six below then
		// override the ones with a dedicated one. Nothing is left on a shared handler that cannot
		// say what it was.
		let mut i = 0;
		while i < GENERIC_NO_CODE.len() {
			let (vector, handler) = GENERIC_NO_CODE[i];
			idt[vector].set(handler, 0);
			i += 1;
		}
		let mut i = 0;
		while i < GENERIC_WITH_CODE.len() {
			let (vector, handler) = GENERIC_WITH_CODE[i];
			idt[vector].set_with_code(handler, 0);
			i += 1;
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

// WHERE THE FAULT CAME FROM, ASKED ONCE (KERN-ARCH-004).
//
// Only the page-fault and #GP handlers looked at the privilege level; every other vector called the
// fatal halt path whatever raised it. So `ud2`, a divide by zero, a bound-range check or an
// alignment fault from ring 3 stopped every CPU rather than the one process that executed it -
// which is the property the exception table established for user copies and had never held for user faults.
//
// The low two bits of the saved code selector are the CPL, so this is one comparison, and it
// belongs in front of every vector rather than in the two that happened to have it.
//
// `clac_on_entry` first, for the same reason `general_protection_fault` does it: a gate does not
// clear `EFLAGS.AC`, and the terminate path longjmps into a kernel thread that must not inherit a
// user-set alignment-check flag.
fn from_ring_three(frame: &InterruptStackFrame) -> bool {
	super::paging::clac_on_entry();
	frame.code_segment & 3 == 3
}

// Terminate the faulting process, or halt if the fault was the kernel's own.
fn user_fault_or_halt(frame: &InterruptStackFrame, kind: u64, error_code: u64, address: u64, name: &str) {
	if from_ring_three(frame) {
		crate::fault::terminate_user(crate::fault::FaultInfo { kind, error_code, address, instruction_pointer: frame.instruction_pointer });
	}
	crate::serial_println!("EXCEPTION: {name} (code {:#x}) at {:#x}", error_code, frame.instruction_pointer);
	super::halt_loop();
}

extern "x86-interrupt" fn divide_error(frame: InterruptStackFrame) {
	user_fault_or_halt(&frame, crate::fault::FAULT_DIVIDE, 0, 0, "divide error");
}

// A ring-0 `int3` is recoverable and stays that way: report and return so execution continues past
// it, which is what a kernel breakpoint is for. A ring-3 one is not a debugger request - nothing in
// this system attaches one - so it ends the process that executed it rather than printing a line
// per iteration for a program that loops over `int3`.
extern "x86-interrupt" fn breakpoint(frame: InterruptStackFrame) {
	if from_ring_three(&frame) {
		crate::fault::terminate_user(crate::fault::FaultInfo { kind: crate::fault::FAULT_BREAKPOINT, error_code: 0, address: 0, instruction_pointer: frame.instruction_pointer });
	}
	crate::serial_println!("EXCEPTION: breakpoint at {:#x} (continuing)", frame.instruction_pointer);
}

extern "x86-interrupt" fn invalid_opcode(frame: InterruptStackFrame) {
	user_fault_or_halt(&frame, crate::fault::FAULT_INVALID_OPCODE, 0, 0, "invalid opcode");
}

extern "x86-interrupt" fn double_fault(frame: InterruptStackFrame, error_code: u64) -> ! {
	crate::serial_println!("EXCEPTION: DOUBLE FAULT (code {:#x}) at {:#x}", error_code, frame.instruction_pointer);
	super::halt_loop();
}

extern "x86-interrupt" fn general_protection_fault(frame: InterruptStackFrame, error_code: u64) {
	// A gate does not clear EFLAGS.AC; the fault path may longjmp into a kernel
	// thread, so drop any user-set AC before kernel execution continues there.
	super::paging::clac_on_entry();
	// A #GP taken in ring 3 is a userspace bug: terminate that process and return
	// to the kernel. The low two bits of the saved code selector are the CPL.
	if frame.code_segment & 3 == 3 {
		crate::fault::terminate_user(crate::fault::FaultInfo { kind: crate::fault::FAULT_GENERAL_PROTECTION, error_code, address: 0, instruction_pointer: frame.instruction_pointer });
	}
	// In ring 0 it is a kernel bug; halt loudly.
	crate::serial_println!("EXCEPTION: general protection fault (code {:#x}) at {:#x}", error_code, frame.instruction_pointer);
	super::halt_loop();
}

extern "x86-interrupt" fn page_fault(mut frame: InterruptStackFrame, error_code: u64) {
	// See general_protection_fault: the terminate path longjmps into a kernel
	// thread, so any user-set EFLAGS.AC must not travel there.
	super::paging::clac_on_entry();
	let cr2: u64;
	unsafe {
		asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack, preserves_flags));
	}
	if frame.code_segment & 3 == 3 {
		// A ring-3 not-present fault just below the mapped stack is growth, not a
		// bug: map the missing page and return, and the CPU retries the instruction.
		if crate::fault::grow_user_stack(cr2, error_code) {
			return;
		}
		// Anything else taken in ring 3 is a userspace bug (a bad dereference, a
		// write to a read-only page, recursion past the stack floor): terminate that
		// process and return to the kernel. The low two bits of the saved code
		// selector are the CPL.
		crate::fault::terminate_user(crate::fault::FaultInfo { kind: crate::fault::FAULT_PAGE, error_code, address: cr2, instruction_pointer: frame.instruction_pointer });
	}
	// In ring 0, one class of fault is NOT a kernel bug: an instruction the kernel declared may
	// fault, because it is copying to or from a userspace address that another thread can unmap
	// underneath it. Those resume at their fixup with the copy reporting how far it got.
	//
	// Matched on the faulting instruction EXACTLY, never by range, so this can only ever rescue an
	// address some copy routine put in the table itself. Anything else still halts, which is the
	// property that keeps a real kernel bug loud.
	//
	// `frame` is the CPU's own frame on the stack rather than a copy: rustc's `x86-interrupt` ABI
	// materialises the by-value parameter in place, so writing `instruction_pointer` here is what
	// `iretq` will resume at. If that ever stopped being true the test would not merely fail, it
	// would triple-fault - which is the loudest possible way to find out.
	if let Some(fixup) = crate::extable::fixup_for(frame.instruction_pointer, cr2) {
		// VOLATILE, and the compiler is what proved it has to be. A plain
		// `frame.instruction_pointer = fixup` drew "value assigned to `frame` is never read" -
		// which is true of the local and false of the machine: nothing in this function reads it
		// again, so the store is dead by every rule the optimiser knows, and it would have been
		// deleted. The fixup would then never happen and the fault would fall through to the halt
		// below, which is the failure this whole milestone is about, arriving through the fix for
		// it. A volatile write is the statement that the store is the point.
		unsafe { (&raw mut frame.instruction_pointer).write_volatile(fixup) };
		return;
	}
	// The one other exception: the test suite arms a probe address to prove SMAP/SMEP refuse a
	// kernel access to user memory - that expected fault retires the probing thread instead.
	#[cfg(test)]
	if crate::fault::smap_probe_trip(cr2, error_code) {
		crate::sched::exit();
	}
	crate::serial_println!("EXCEPTION: page fault (code {:#x}) at {:#x}, CR2 = {:#x}", error_code, frame.instruction_pointer, cr2);
	super::halt_loop();
}

// ONE HANDLER PER VECTOR, so a crash record can say WHICH exception (KERN-ARCH-004).
//
// The vectors without a handler of their own shared two functions - one with a hardware error code
// and one without - and neither could name itself, because the `x86-interrupt` ABI has no room for
// an extra argument. A generated trampoline per vector has: each is three lines and closes over its
// own number, and the number reaches userspace in `FaultInfo::address`.
macro_rules! generic_vectors {
	($($name:ident = $vector:expr),* $(,)?) => {
		$(
			extern "x86-interrupt" fn $name(frame: InterruptStackFrame) {
				user_fault_or_halt(&frame, crate::fault::FAULT_EXCEPTION, 0, $vector, "unhandled fault");
			}
		)*
	};
}

macro_rules! generic_vectors_with_code {
	($($name:ident = $vector:expr),* $(,)?) => {
		$(
			extern "x86-interrupt" fn $name(frame: InterruptStackFrame, error_code: u64) {
				user_fault_or_halt(&frame, crate::fault::FAULT_EXCEPTION, error_code, $vector, "unhandled fault");
			}
		)*
	};
}

generic_vectors!(generic_v1 = 1, generic_v2 = 2, generic_v4 = 4, generic_v5 = 5, generic_v7 = 7, generic_v9 = 9, generic_v15 = 15, generic_v16 = 16, generic_v18 = 18, generic_v19 = 19, generic_v20 = 20, generic_v22 = 22, generic_v23 = 23, generic_v24 = 24, generic_v25 = 25, generic_v26 = 26, generic_v27 = 27, generic_v28 = 28, generic_v31 = 31,);

generic_vectors_with_code!(generic_v10 = 10, generic_v11 = 11, generic_v12 = 12, generic_v17 = 17, generic_v21 = 21, generic_v29 = 29, generic_v30 = 30,);

// The table the initialiser walks, so adding a vector is one line in one place.
pub(super) const GENERIC_NO_CODE: [(usize, extern "x86-interrupt" fn(InterruptStackFrame)); 19] = [
	(1, generic_v1),
	(2, generic_v2),
	(4, generic_v4),
	(5, generic_v5),
	(7, generic_v7),
	(9, generic_v9),
	(15, generic_v15),
	(16, generic_v16),
	(18, generic_v18),
	(19, generic_v19),
	(20, generic_v20),
	(22, generic_v22),
	(23, generic_v23),
	(24, generic_v24),
	(25, generic_v25),
	(26, generic_v26),
	(27, generic_v27),
	(28, generic_v28),
	(31, generic_v31),
];

pub(super) const GENERIC_WITH_CODE: [(usize, extern "x86-interrupt" fn(InterruptStackFrame, u64)); 7] = [(10, generic_v10), (11, generic_v11), (12, generic_v12), (17, generic_v17), (21, generic_v21), (29, generic_v29), (30, generic_v30)];

#[cfg(test)]
mod tests;
