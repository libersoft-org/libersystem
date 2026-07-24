#![no_std]

use core::arch::global_asm;

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_time_time_now,\"ax\",@progbits\n.globl liber_channel_liber_time_time_now\n.type liber_channel_liber_time_time_now,@function\nliber_channel_liber_time_time_now:\njmp liber_channel_impl_liber_time_time_now\n.size liber_channel_liber_time_time_now, . - liber_channel_liber_time_time_now\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_time_time_now,\"ax\",@progbits\n.globl liber_channel_liber_time_time_now\n.type liber_channel_liber_time_time_now,%function\nliber_channel_liber_time_time_now:\nb liber_channel_impl_liber_time_time_now\n.size liber_channel_liber_time_time_now, . - liber_channel_liber_time_time_now\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_time_time_now,\"ax\",@progbits\n.globl liber_channel_liber_time_time_now\n.type liber_channel_liber_time_time_now,%function\nliber_channel_liber_time_time_now:\ntail liber_channel_impl_liber_time_time_now\n.size liber_channel_liber_time_time_now, . - liber_channel_liber_time_time_now\n");
