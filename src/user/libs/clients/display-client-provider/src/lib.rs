//! The trampolines that make the generated display clients reachable by name: one symbol for each operation the
//! `brightness` tool uses, a tail jump to the `liber_channel_impl_*` the generated crate exports - the shape of every
//! client provider.

#![no_std]

use core::arch::global_asm;

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_display_display_brightness_outputs,\"ax\",@progbits\n.globl liber_channel_liber_display_display_brightness_outputs\n.type liber_channel_liber_display_display_brightness_outputs,@function\nliber_channel_liber_display_display_brightness_outputs:\njmp liber_channel_impl_liber_display_display_brightness_outputs\n.size liber_channel_liber_display_display_brightness_outputs, . - liber_channel_liber_display_display_brightness_outputs\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_display_display_brightness_backlights,\"ax\",@progbits\n.globl liber_channel_liber_display_display_brightness_backlights\n.type liber_channel_liber_display_display_brightness_backlights,@function\nliber_channel_liber_display_display_brightness_backlights:\njmp liber_channel_impl_liber_display_display_brightness_backlights\n.size liber_channel_liber_display_display_brightness_backlights, . - liber_channel_liber_display_display_brightness_backlights\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_display_brightness_policy_set,\"ax\",@progbits\n.globl liber_channel_liber_display_brightness_policy_set\n.type liber_channel_liber_display_brightness_policy_set,@function\nliber_channel_liber_display_brightness_policy_set:\njmp liber_channel_impl_liber_display_brightness_policy_set\n.size liber_channel_liber_display_brightness_policy_set, . - liber_channel_liber_display_brightness_policy_set\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_display_brightness_policy_settings,\"ax\",@progbits\n.globl liber_channel_liber_display_brightness_policy_settings\n.type liber_channel_liber_display_brightness_policy_settings,@function\nliber_channel_liber_display_brightness_policy_settings:\njmp liber_channel_impl_liber_display_brightness_policy_settings\n.size liber_channel_liber_display_brightness_policy_settings, . - liber_channel_liber_display_brightness_policy_settings\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_display_brightness_policy_set_automatic,\"ax\",@progbits\n.globl liber_channel_liber_display_brightness_policy_set_automatic\n.type liber_channel_liber_display_brightness_policy_set_automatic,@function\nliber_channel_liber_display_brightness_policy_set_automatic:\njmp liber_channel_impl_liber_display_brightness_policy_set_automatic\n.size liber_channel_liber_display_brightness_policy_set_automatic, . - liber_channel_liber_display_brightness_policy_set_automatic\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_display_brightness_policy_set_idle,\"ax\",@progbits\n.globl liber_channel_liber_display_brightness_policy_set_idle\n.type liber_channel_liber_display_brightness_policy_set_idle,@function\nliber_channel_liber_display_brightness_policy_set_idle:\njmp liber_channel_impl_liber_display_brightness_policy_set_idle\n.size liber_channel_liber_display_brightness_policy_set_idle, . - liber_channel_liber_display_brightness_policy_set_idle\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_display_display_brightness_outputs,\"ax\",@progbits\n.globl liber_channel_liber_display_display_brightness_outputs\n.type liber_channel_liber_display_display_brightness_outputs,%function\nliber_channel_liber_display_display_brightness_outputs:\nb liber_channel_impl_liber_display_display_brightness_outputs\n.size liber_channel_liber_display_display_brightness_outputs, . - liber_channel_liber_display_display_brightness_outputs\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_display_display_brightness_backlights,\"ax\",@progbits\n.globl liber_channel_liber_display_display_brightness_backlights\n.type liber_channel_liber_display_display_brightness_backlights,%function\nliber_channel_liber_display_display_brightness_backlights:\nb liber_channel_impl_liber_display_display_brightness_backlights\n.size liber_channel_liber_display_display_brightness_backlights, . - liber_channel_liber_display_display_brightness_backlights\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_display_brightness_policy_set,\"ax\",@progbits\n.globl liber_channel_liber_display_brightness_policy_set\n.type liber_channel_liber_display_brightness_policy_set,%function\nliber_channel_liber_display_brightness_policy_set:\nb liber_channel_impl_liber_display_brightness_policy_set\n.size liber_channel_liber_display_brightness_policy_set, . - liber_channel_liber_display_brightness_policy_set\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_display_brightness_policy_settings,\"ax\",@progbits\n.globl liber_channel_liber_display_brightness_policy_settings\n.type liber_channel_liber_display_brightness_policy_settings,%function\nliber_channel_liber_display_brightness_policy_settings:\nb liber_channel_impl_liber_display_brightness_policy_settings\n.size liber_channel_liber_display_brightness_policy_settings, . - liber_channel_liber_display_brightness_policy_settings\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_display_brightness_policy_set_automatic,\"ax\",@progbits\n.globl liber_channel_liber_display_brightness_policy_set_automatic\n.type liber_channel_liber_display_brightness_policy_set_automatic,%function\nliber_channel_liber_display_brightness_policy_set_automatic:\nb liber_channel_impl_liber_display_brightness_policy_set_automatic\n.size liber_channel_liber_display_brightness_policy_set_automatic, . - liber_channel_liber_display_brightness_policy_set_automatic\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_display_brightness_policy_set_idle,\"ax\",@progbits\n.globl liber_channel_liber_display_brightness_policy_set_idle\n.type liber_channel_liber_display_brightness_policy_set_idle,%function\nliber_channel_liber_display_brightness_policy_set_idle:\nb liber_channel_impl_liber_display_brightness_policy_set_idle\n.size liber_channel_liber_display_brightness_policy_set_idle, . - liber_channel_liber_display_brightness_policy_set_idle\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_display_display_brightness_outputs,\"ax\",@progbits\n.globl liber_channel_liber_display_display_brightness_outputs\n.type liber_channel_liber_display_display_brightness_outputs,%function\nliber_channel_liber_display_display_brightness_outputs:\ntail liber_channel_impl_liber_display_display_brightness_outputs\n.size liber_channel_liber_display_display_brightness_outputs, . - liber_channel_liber_display_display_brightness_outputs\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_display_display_brightness_backlights,\"ax\",@progbits\n.globl liber_channel_liber_display_display_brightness_backlights\n.type liber_channel_liber_display_display_brightness_backlights,%function\nliber_channel_liber_display_display_brightness_backlights:\ntail liber_channel_impl_liber_display_display_brightness_backlights\n.size liber_channel_liber_display_display_brightness_backlights, . - liber_channel_liber_display_display_brightness_backlights\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_display_brightness_policy_set,\"ax\",@progbits\n.globl liber_channel_liber_display_brightness_policy_set\n.type liber_channel_liber_display_brightness_policy_set,%function\nliber_channel_liber_display_brightness_policy_set:\ntail liber_channel_impl_liber_display_brightness_policy_set\n.size liber_channel_liber_display_brightness_policy_set, . - liber_channel_liber_display_brightness_policy_set\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_display_brightness_policy_settings,\"ax\",@progbits\n.globl liber_channel_liber_display_brightness_policy_settings\n.type liber_channel_liber_display_brightness_policy_settings,%function\nliber_channel_liber_display_brightness_policy_settings:\ntail liber_channel_impl_liber_display_brightness_policy_settings\n.size liber_channel_liber_display_brightness_policy_settings, . - liber_channel_liber_display_brightness_policy_settings\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_display_brightness_policy_set_automatic,\"ax\",@progbits\n.globl liber_channel_liber_display_brightness_policy_set_automatic\n.type liber_channel_liber_display_brightness_policy_set_automatic,%function\nliber_channel_liber_display_brightness_policy_set_automatic:\ntail liber_channel_impl_liber_display_brightness_policy_set_automatic\n.size liber_channel_liber_display_brightness_policy_set_automatic, . - liber_channel_liber_display_brightness_policy_set_automatic\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_display_brightness_policy_set_idle,\"ax\",@progbits\n.globl liber_channel_liber_display_brightness_policy_set_idle\n.type liber_channel_liber_display_brightness_policy_set_idle,%function\nliber_channel_liber_display_brightness_policy_set_idle:\ntail liber_channel_impl_liber_display_brightness_policy_set_idle\n.size liber_channel_liber_display_brightness_policy_set_idle, . - liber_channel_liber_display_brightness_policy_set_idle\n");
