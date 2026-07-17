use std::env;

fn main() {
	let target = env::args().nth(1).expect("usage: exe-start <target>");
	let source = match target.as_str() {
		"x86_64-unknown-none" => r#".intel_syntax noprefix
.section .text.start,"ax",@progbits
.global _start
.type _start,@function
_start:
	and rsp, -16
	lea rsi, [rip + __user_main]
	call liber_rt_start
	ud2
.size _start, .-_start
"#,
		"aarch64-unknown-none" => r#".section .text.start,"ax",@progbits
.global _start
.type _start,%function
_start:
	mov x29, xzr
	adrp x1, __user_main
	add x1, x1, :lo12:__user_main
	bl liber_rt_start
	brk #0
.size _start, .-_start
"#,
		"riscv64gc-unknown-none-elf" => r#".option pic
.section .text.start,"ax",@progbits
.global _start
.type _start,@function
_start:
	andi sp, sp, -16
	mv s0, zero
	lla a1, __user_main
	call liber_rt_start
	ebreak
.size _start, .-_start
"#,
		_ => panic!("unsupported executable target: {target}"),
	};
	print!("{source}");
}
