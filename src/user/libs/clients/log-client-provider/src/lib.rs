#![no_std]

use core::arch::global_asm;

#[cfg(target_arch = "x86_64")]
macro_rules! forward {
	($symbol:literal, $implementation:literal) => {
		global_asm!(concat!(".section .text.", $symbol, ",\"ax\",@progbits\n", ".globl ", $symbol, "\n", ".type ", $symbol, ",@function\n", $symbol, ":\n", "jmp ", $implementation, "\n", ".size ", $symbol, ", . - ", $symbol, "\n",));
	};
}

#[cfg(target_arch = "aarch64")]
macro_rules! forward {
	($symbol:literal, $implementation:literal) => {
		global_asm!(concat!(".section .text.", $symbol, ",\"ax\",@progbits\n", ".globl ", $symbol, "\n", ".type ", $symbol, ",%function\n", $symbol, ":\n", "b ", $implementation, "\n", ".size ", $symbol, ", . - ", $symbol, "\n",));
	};
}

#[cfg(target_arch = "riscv64")]
macro_rules! forward {
	($symbol:literal, $implementation:literal) => {
		global_asm!(concat!(".section .text.", $symbol, ",\"ax\",@progbits\n", ".globl ", $symbol, "\n", ".type ", $symbol, ",%function\n", $symbol, ":\n", "tail ", $implementation, "\n", ".size ", $symbol, ", . - ", $symbol, "\n",));
	};
}

forward!("liber_channel_liber_log_log_emit", "liber_channel_impl_liber_log_log_emit");
forward!("liber_channel_liber_log_log_query", "liber_channel_impl_liber_log_log_query");
forward!("liber_channel_liber_log_log_tail", "liber_channel_impl_liber_log_log_tail");
