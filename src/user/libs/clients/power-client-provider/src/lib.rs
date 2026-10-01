//! The trampolines that make the generated power clients reachable by name: one symbol for each operation `powerctl`
//! uses, a tail jump to the `liber_channel_impl_*` the generated crate exports - the shape of every client provider.

#![no_std]

use core::arch::global_asm;

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_power_power_sources,\"ax\",@progbits\n.globl liber_channel_liber_power_power_sources\n.type liber_channel_liber_power_power_sources,@function\nliber_channel_liber_power_power_sources:\njmp liber_channel_impl_liber_power_power_sources\n.size liber_channel_liber_power_power_sources, . - liber_channel_liber_power_power_sources\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_power_processor_power_admin_status,\"ax\",@progbits\n.globl liber_channel_liber_power_processor_power_admin_status\n.type liber_channel_liber_power_processor_power_admin_status,@function\nliber_channel_liber_power_processor_power_admin_status:\njmp liber_channel_impl_liber_power_processor_power_admin_status\n.size liber_channel_liber_power_processor_power_admin_status, . - liber_channel_liber_power_processor_power_admin_status\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_power_processor_power_admin_set_profile,\"ax\",@progbits\n.globl liber_channel_liber_power_processor_power_admin_set_profile\n.type liber_channel_liber_power_processor_power_admin_set_profile,@function\nliber_channel_liber_power_processor_power_admin_set_profile:\njmp liber_channel_impl_liber_power_processor_power_admin_set_profile\n.size liber_channel_liber_power_processor_power_admin_set_profile, . - liber_channel_liber_power_processor_power_admin_set_profile\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_power_processor_power_admin_set_fan_curve,\"ax\",@progbits\n.globl liber_channel_liber_power_processor_power_admin_set_fan_curve\n.type liber_channel_liber_power_processor_power_admin_set_fan_curve,@function\nliber_channel_liber_power_processor_power_admin_set_fan_curve:\njmp liber_channel_impl_liber_power_processor_power_admin_set_fan_curve\n.size liber_channel_liber_power_processor_power_admin_set_fan_curve, . - liber_channel_liber_power_processor_power_admin_set_fan_curve\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_power_power_sources,\"ax\",@progbits\n.globl liber_channel_liber_power_power_sources\n.type liber_channel_liber_power_power_sources,%function\nliber_channel_liber_power_power_sources:\nb liber_channel_impl_liber_power_power_sources\n.size liber_channel_liber_power_power_sources, . - liber_channel_liber_power_power_sources\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_power_processor_power_admin_status,\"ax\",@progbits\n.globl liber_channel_liber_power_processor_power_admin_status\n.type liber_channel_liber_power_processor_power_admin_status,%function\nliber_channel_liber_power_processor_power_admin_status:\nb liber_channel_impl_liber_power_processor_power_admin_status\n.size liber_channel_liber_power_processor_power_admin_status, . - liber_channel_liber_power_processor_power_admin_status\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_power_processor_power_admin_set_profile,\"ax\",@progbits\n.globl liber_channel_liber_power_processor_power_admin_set_profile\n.type liber_channel_liber_power_processor_power_admin_set_profile,%function\nliber_channel_liber_power_processor_power_admin_set_profile:\nb liber_channel_impl_liber_power_processor_power_admin_set_profile\n.size liber_channel_liber_power_processor_power_admin_set_profile, . - liber_channel_liber_power_processor_power_admin_set_profile\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_power_processor_power_admin_set_fan_curve,\"ax\",@progbits\n.globl liber_channel_liber_power_processor_power_admin_set_fan_curve\n.type liber_channel_liber_power_processor_power_admin_set_fan_curve,%function\nliber_channel_liber_power_processor_power_admin_set_fan_curve:\nb liber_channel_impl_liber_power_processor_power_admin_set_fan_curve\n.size liber_channel_liber_power_processor_power_admin_set_fan_curve, . - liber_channel_liber_power_processor_power_admin_set_fan_curve\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_power_power_sources,\"ax\",@progbits\n.globl liber_channel_liber_power_power_sources\n.type liber_channel_liber_power_power_sources,%function\nliber_channel_liber_power_power_sources:\ntail liber_channel_impl_liber_power_power_sources\n.size liber_channel_liber_power_power_sources, . - liber_channel_liber_power_power_sources\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_power_processor_power_admin_status,\"ax\",@progbits\n.globl liber_channel_liber_power_processor_power_admin_status\n.type liber_channel_liber_power_processor_power_admin_status,%function\nliber_channel_liber_power_processor_power_admin_status:\ntail liber_channel_impl_liber_power_processor_power_admin_status\n.size liber_channel_liber_power_processor_power_admin_status, . - liber_channel_liber_power_processor_power_admin_status\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_power_processor_power_admin_set_profile,\"ax\",@progbits\n.globl liber_channel_liber_power_processor_power_admin_set_profile\n.type liber_channel_liber_power_processor_power_admin_set_profile,%function\nliber_channel_liber_power_processor_power_admin_set_profile:\ntail liber_channel_impl_liber_power_processor_power_admin_set_profile\n.size liber_channel_liber_power_processor_power_admin_set_profile, . - liber_channel_liber_power_processor_power_admin_set_profile\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_power_processor_power_admin_set_fan_curve,\"ax\",@progbits\n.globl liber_channel_liber_power_processor_power_admin_set_fan_curve\n.type liber_channel_liber_power_processor_power_admin_set_fan_curve,%function\nliber_channel_liber_power_processor_power_admin_set_fan_curve:\ntail liber_channel_impl_liber_power_processor_power_admin_set_fan_curve\n.size liber_channel_liber_power_processor_power_admin_set_fan_curve, . - liber_channel_liber_power_processor_power_admin_set_fan_curve\n");
