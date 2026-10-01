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

forward!("liber_channel_liber_process_process_start", "liber_channel_impl_liber_process_process_start");
forward!("liber_channel_liber_process_process_list", "liber_channel_impl_liber_process_process_list");
forward!("liber_channel_liber_process_process_launch", "liber_channel_impl_liber_process_process_launch");
forward!("liber_channel_liber_process_process_launch_bounded", "liber_channel_impl_liber_process_process_launch_bounded");
forward!("liber_channel_liber_process_system_sleep_suspend", "liber_channel_impl_liber_process_system_sleep_suspend");
forward!("liber_channel_liber_process_system_sleep_hibernate", "liber_channel_impl_liber_process_system_sleep_hibernate");
forward!("liber_channel_liber_process_system_sleep_inhibit", "liber_channel_impl_liber_process_system_sleep_inhibit");
forward!("liber_channel_liber_process_system_sleep_release", "liber_channel_impl_liber_process_system_sleep_release");
forward!("liber_channel_liber_process_system_sleep_last_sleep", "liber_channel_impl_liber_process_system_sleep_last_sleep");
forward!("liber_channel_liber_process_system_sleep_status", "liber_channel_impl_liber_process_system_sleep_status");
forward!("liber_channel_liber_process_system_sleep_schedule_wake", "liber_channel_impl_liber_process_system_sleep_schedule_wake");
