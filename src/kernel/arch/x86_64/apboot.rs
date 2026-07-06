// Application-processor bring-up: the real-mode trampoline.
//
// An application processor leaves reset in 16-bit real mode at the vector a
// STARTUP IPI names - a page-aligned physical address below 1 MiB. The loader
// reserved that page and handed it to the kernel; smp::init copies this trampoline
// into it, fills a small mailbox (the shared page tables' CR3, the 64-bit Rust
// entry, and this AP's stack top), then sends INIT + STARTUP + STARTUP. The
// trampoline switches 16 -> 32 -> 64-bit, loads the shared page tables, and calls
// the entry on the given stack.
//
// The blob is position independent: it derives its own physical base from CS
// (base = CS << 4) and patches its GDT descriptor and the two far-jump pointers at
// run time, so it works wherever the loader placed the page. It links into
// .rodata (never executed in place - only the copy in the low page runs).

use core::arch::global_asm;

unsafe extern "C" {
	static ap_tramp_start: u8;
	static ap_tramp_end: u8;
	static ap_mailbox: u8;
}

// The trampoline blob length in bytes (start .. mailbox end).
pub fn trampoline_len() -> usize {
	(&raw const ap_tramp_end as usize) - (&raw const ap_tramp_start as usize)
}

// Byte offset of the mailbox within the trampoline page.
fn mailbox_offset() -> usize {
	(&raw const ap_mailbox as usize) - (&raw const ap_tramp_start as usize)
}

// Copy the trampoline blob to `dst` (the HHDM virtual address of the reserved low
// page) and fill the constant mailbox fields (CR3 and the 64-bit entry). The
// per-AP stack is written separately before each wake.
pub unsafe fn install(dst: *mut u8, cr3: u64, entry: u64) {
	let src = &raw const ap_tramp_start as *const u8;
	unsafe {
		core::ptr::copy_nonoverlapping(src, dst, trampoline_len());
		let mb = dst.add(mailbox_offset()) as *mut u64;
		mb.add(0).write_volatile(cr3);
		mb.add(1).write_volatile(entry);
	}
}

// Write the AP stack top into the mailbox of the trampoline at `dst` (HHDM virtual
// address of the low page), before sending its STARTUP IPI.
pub unsafe fn set_stack(dst: *mut u8, stack_top: u64) {
	unsafe {
		let mb = dst.add(mailbox_offset()) as *mut u64;
		mb.add(2).write_volatile(stack_top);
	}
}

global_asm!(
	r#"
.section .rodata.aptramp, "a"
.code16
.globl ap_tramp_start
ap_tramp_start:
	cli
	cld
	movw %cs, %ax
	movw %ax, %ds
	# ebp = page linear base (CS << 4)
	xorl %eax, %eax
	movw %cs, %ax
	shll $4, %eax
	movl %eax, %ebp

	# GDT descriptor base = base + (gdt - start)
	movl %ebp, %eax
	addl $(ap_gdt - ap_tramp_start), %eax
	movl %eax, (ap_gdtr_base - ap_tramp_start)

	# 32-bit far-pointer offset = base + (prot32 - start)
	movl %ebp, %eax
	addl $(ap_prot32 - ap_tramp_start), %eax
	movl %eax, (ap_fptr32 - ap_tramp_start)

	# 64-bit far-pointer offset = base + (long64 - start)
	movl %ebp, %eax
	addl $(ap_long64 - ap_tramp_start), %eax
	movl %eax, (ap_fptr64 - ap_tramp_start)

	lgdtl (ap_gdtr - ap_tramp_start)

	movl %cr0, %eax
	orl $1, %eax
	movl %eax, %cr0

	ljmpl *(ap_fptr32 - ap_tramp_start)

.code32
ap_prot32:
	movw $0x10, %ax
	movw %ax, %ds
	movw %ax, %es
	movw %ax, %ss
	movw %ax, %fs
	movw %ax, %gs

	# enable PAE
	movl %cr4, %eax
	orl $(1 << 5), %eax
	movl %eax, %cr4

	# load CR3 (kernel PML4 phys, < 4 GiB) from the mailbox
	movl (ap_mailbox - ap_tramp_start)(%ebp), %eax
	movl %eax, %cr3

	# EFER: long-mode enable + no-execute enable
	movl $0xC0000080, %ecx
	rdmsr
	orl $(1 << 8), %eax
	orl $(1 << 11), %eax
	wrmsr

	# enable paging -> long mode active
	movl %cr0, %eax
	orl $(1 << 31), %eax
	movl %eax, %cr0

	ljmpl *(ap_fptr64 - ap_tramp_start)(%ebp)

.code64
ap_long64:
	movw $0x20, %ax
	movw %ax, %ds
	movw %ax, %es
	movw %ax, %ss
	# stack top + entry from the mailbox (rbp = ebp, zero-extended)
	movq (ap_mailbox - ap_tramp_start + 16)(%rbp), %rsp
	movq (ap_mailbox - ap_tramp_start + 8)(%rbp), %rax
	# call (not jmp) so the stack is ABI-aligned at the Rust entry; it never returns
	call *%rax
1:
	hlt
	jmp 1b

.align 16
ap_gdt:
	.quad 0x0000000000000000
	.quad 0x00CF9A000000FFFF
	.quad 0x00CF92000000FFFF
	.quad 0x00209A0000000000
	.quad 0x0000920000000000
ap_gdt_end:

.align 4
ap_gdtr:
	.word ap_gdt_end - ap_gdt - 1
ap_gdtr_base:
	.long 0

ap_fptr32:
	.long 0
	.word 0x08

ap_fptr64:
	.long 0
	.word 0x18

.align 8
.globl ap_mailbox
ap_mailbox:
	.quad 0
	.quad 0
	.quad 0
.globl ap_tramp_end
ap_tramp_end:
"#,
	options(att_syntax)
);
