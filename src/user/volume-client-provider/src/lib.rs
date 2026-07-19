#![no_std]

use core::arch::global_asm;

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_storage_volume_open,\"ax\",@progbits\n.globl liber_channel_liber_storage_volume_open\n.type liber_channel_liber_storage_volume_open,@function\nliber_channel_liber_storage_volume_open:\njmp liber_channel_impl_liber_storage_volume_open\n.size liber_channel_liber_storage_volume_open, . - liber_channel_liber_storage_volume_open\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_storage_volume_open,\"ax\",@progbits\n.globl liber_channel_liber_storage_volume_open\n.type liber_channel_liber_storage_volume_open,%function\nliber_channel_liber_storage_volume_open:\nb liber_channel_impl_liber_storage_volume_open\n.size liber_channel_liber_storage_volume_open, . - liber_channel_liber_storage_volume_open\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_storage_volume_open,\"ax\",@progbits\n.globl liber_channel_liber_storage_volume_open\n.type liber_channel_liber_storage_volume_open,%function\nliber_channel_liber_storage_volume_open:\ntail liber_channel_impl_liber_storage_volume_open\n.size liber_channel_liber_storage_volume_open, . - liber_channel_liber_storage_volume_open\n");
