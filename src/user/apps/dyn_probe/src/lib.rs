#![no_std]

use core::arch::global_asm;

#[cfg(target_arch = "x86_64")]
global_asm!(
	r#"
.section .text.start,"ax"
.global _start
.type _start,@function
_start:
	mov r12, rdi
	call liber_pix_probe
	mov rcx, 0x4c49425049584f4b
	cmp rax, rcx
	jne 1f
	mov rdi, r12
	lea rsi, [rip + .Lsuccess]
	mov rdx, 15
	xor r10, r10
	mov rax, 9
	syscall
1:
	mov rax, 17
	syscall
	ud2
.section .rodata,"a"
.Lsuccess:
	.ascii "dynamic link ok"
"#,
);

#[cfg(target_arch = "aarch64")]
global_asm!(
	r#"
.section .text.start,"ax"
.global _start
.type _start,%function
_start:
	mov x19, x0
	bl liber_pix_probe
	movz x1, #0x4f4b
	movk x1, #0x4958, lsl #16
	movk x1, #0x4250, lsl #32
	movk x1, #0x4c49, lsl #48
	cmp x0, x1
	b.ne 1f
	mov x0, x19
	adr x1, .Lsuccess
	mov x2, #15
	mov x3, #0
	mov x8, #9
	svc #0
1:
	mov x8, #17
	svc #0
	brk #0
.section .rodata,"a"
.Lsuccess:
	.ascii "dynamic link ok"
"#,
);

#[cfg(target_arch = "riscv64")]
global_asm!(
	r#"
.section .text.start,"ax"
.global _start
.type _start,@function
_start:
	mv s0, a0
	call liber_pix_probe
	li t0, 0x4c49425049584f4b
	bne a0, t0, 1f
	mv a0, s0
	lla a1, .Lsuccess
	li a2, 15
	li a3, 0
	li a7, 9
	ecall
1:
	li a7, 17
	ecall
	ebreak
.section .rodata,"a"
.Lsuccess:
	.ascii "dynamic link ok"
"#,
);
