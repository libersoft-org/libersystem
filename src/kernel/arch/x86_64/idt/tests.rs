crate::tagged_test!(breakpoint_exception_returns, [Idt, Kernel, ArchX86_64], id = "kernel.arch.x86_64.idt.breakpoint_exception_returns", covers = ["kernel"]);
fn breakpoint_exception_returns() {
	// Reaching the next line proves the IDT breakpoint handler returned cleanly.
	unsafe { core::arch::asm!("int3") };
}

crate::tagged_test!(every_vector_is_filed_by_whether_it_pushes_a_code, [Idt, Kernel, ArchX86_64], id = "kernel.arch.x86_64.idt.every_vector_is_filed_by_whether_it_pushes_a_code", covers = ["kernel"]);
fn every_vector_is_filed_by_whether_it_pushes_a_code() {
	// KERN-ARCH-004 gave every architectural vector a handler that knows its own number, and the
	// handlers come in two shapes because the CPU pushes an error code for some vectors and not
	// others. A vector filed on the wrong side reads the interrupt frame at the wrong offset: the
	// instruction pointer it reports is garbage, and the `iretq` at the end returns to it.
	//
	// So the two tables are checked against `has_error_code`, which is the architecture's own
	// answer, and every vector below 32 is required to appear exactly once - in a table or among
	// the six with dedicated handlers. A vector that appears in neither is one nothing populates.
	use super::{GENERIC_NO_CODE, GENERIC_WITH_CODE, has_error_code};
	const DEDICATED: [usize; 6] = [0, 3, 6, 8, 13, 14];

	for (vector, _) in GENERIC_NO_CODE {
		assert!(!has_error_code(vector), "vector {vector} pushes an error code and is filed as one that does not");
	}
	for (vector, _) in GENERIC_WITH_CODE {
		assert!(has_error_code(vector), "vector {vector} pushes no error code and is filed as one that does");
	}
	for vector in 0..32usize {
		let mut seen = 0;
		if GENERIC_NO_CODE.iter().any(|(v, _)| *v == vector) {
			seen += 1;
		}
		if GENERIC_WITH_CODE.iter().any(|(v, _)| *v == vector) {
			seen += 1;
		}
		if DEDICATED.contains(&vector) {
			seen += 1;
		}
		assert_eq!(seen, 1, "vector {vector} is populated {seen} time(s); every architectural vector needs exactly one handler");
	}
}
