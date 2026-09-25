//! The trampolines that make the generated Bluetooth clients reachable by name.
//!
//! One symbol per operation `btctl` uses, each a tail jump to the `liber_channel_impl_*` the generated
//! crate exports - the same shape as every other client provider, for the same reason: a consumer links
//! against an address with a stable name, not against a function the linker may inline or reorder.

#![no_std]

use core::arch::global_asm;

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_controllers,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_controllers\n.type liber_channel_liber_bluetooth_bluetooth_controllers,@function\nliber_channel_liber_bluetooth_bluetooth_controllers:\njmp liber_channel_impl_liber_bluetooth_bluetooth_controllers\n.size liber_channel_liber_bluetooth_bluetooth_controllers, . - liber_channel_liber_bluetooth_bluetooth_controllers\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_controllers,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_controllers\n.type liber_channel_liber_bluetooth_bluetooth_controllers,%function\nliber_channel_liber_bluetooth_bluetooth_controllers:\nb liber_channel_impl_liber_bluetooth_bluetooth_controllers\n.size liber_channel_liber_bluetooth_bluetooth_controllers, . - liber_channel_liber_bluetooth_bluetooth_controllers\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_controllers,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_controllers\n.type liber_channel_liber_bluetooth_bluetooth_controllers,%function\nliber_channel_liber_bluetooth_bluetooth_controllers:\ntail liber_channel_impl_liber_bluetooth_bluetooth_controllers\n.size liber_channel_liber_bluetooth_bluetooth_controllers, . - liber_channel_liber_bluetooth_bluetooth_controllers\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_scan,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_scan\n.type liber_channel_liber_bluetooth_bluetooth_scan,@function\nliber_channel_liber_bluetooth_bluetooth_scan:\njmp liber_channel_impl_liber_bluetooth_bluetooth_scan\n.size liber_channel_liber_bluetooth_bluetooth_scan, . - liber_channel_liber_bluetooth_bluetooth_scan\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_scan,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_scan\n.type liber_channel_liber_bluetooth_bluetooth_scan,%function\nliber_channel_liber_bluetooth_bluetooth_scan:\nb liber_channel_impl_liber_bluetooth_bluetooth_scan\n.size liber_channel_liber_bluetooth_bluetooth_scan, . - liber_channel_liber_bluetooth_bluetooth_scan\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_scan,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_scan\n.type liber_channel_liber_bluetooth_bluetooth_scan,%function\nliber_channel_liber_bluetooth_bluetooth_scan:\ntail liber_channel_impl_liber_bluetooth_bluetooth_scan\n.size liber_channel_liber_bluetooth_bluetooth_scan, . - liber_channel_liber_bluetooth_bluetooth_scan\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_results,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_results\n.type liber_channel_liber_bluetooth_bluetooth_results,@function\nliber_channel_liber_bluetooth_bluetooth_results:\njmp liber_channel_impl_liber_bluetooth_bluetooth_results\n.size liber_channel_liber_bluetooth_bluetooth_results, . - liber_channel_liber_bluetooth_bluetooth_results\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_results,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_results\n.type liber_channel_liber_bluetooth_bluetooth_results,%function\nliber_channel_liber_bluetooth_bluetooth_results:\nb liber_channel_impl_liber_bluetooth_bluetooth_results\n.size liber_channel_liber_bluetooth_bluetooth_results, . - liber_channel_liber_bluetooth_bluetooth_results\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_results,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_results\n.type liber_channel_liber_bluetooth_bluetooth_results,%function\nliber_channel_liber_bluetooth_bluetooth_results:\ntail liber_channel_impl_liber_bluetooth_bluetooth_results\n.size liber_channel_liber_bluetooth_bluetooth_results, . - liber_channel_liber_bluetooth_bluetooth_results\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_scanning,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_scanning\n.type liber_channel_liber_bluetooth_bluetooth_scanning,@function\nliber_channel_liber_bluetooth_bluetooth_scanning:\njmp liber_channel_impl_liber_bluetooth_bluetooth_scanning\n.size liber_channel_liber_bluetooth_bluetooth_scanning, . - liber_channel_liber_bluetooth_bluetooth_scanning\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_scanning,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_scanning\n.type liber_channel_liber_bluetooth_bluetooth_scanning,%function\nliber_channel_liber_bluetooth_bluetooth_scanning:\nb liber_channel_impl_liber_bluetooth_bluetooth_scanning\n.size liber_channel_liber_bluetooth_bluetooth_scanning, . - liber_channel_liber_bluetooth_bluetooth_scanning\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_scanning,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_scanning\n.type liber_channel_liber_bluetooth_bluetooth_scanning,%function\nliber_channel_liber_bluetooth_bluetooth_scanning:\ntail liber_channel_impl_liber_bluetooth_bluetooth_scanning\n.size liber_channel_liber_bluetooth_bluetooth_scanning, . - liber_channel_liber_bluetooth_bluetooth_scanning\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_power,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_power\n.type liber_channel_liber_bluetooth_bluetooth_operator_power,@function\nliber_channel_liber_bluetooth_bluetooth_operator_power:\njmp liber_channel_impl_liber_bluetooth_bluetooth_operator_power\n.size liber_channel_liber_bluetooth_bluetooth_operator_power, . - liber_channel_liber_bluetooth_bluetooth_operator_power\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_power,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_power\n.type liber_channel_liber_bluetooth_bluetooth_operator_power,%function\nliber_channel_liber_bluetooth_bluetooth_operator_power:\nb liber_channel_impl_liber_bluetooth_bluetooth_operator_power\n.size liber_channel_liber_bluetooth_bluetooth_operator_power, . - liber_channel_liber_bluetooth_bluetooth_operator_power\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_power,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_power\n.type liber_channel_liber_bluetooth_bluetooth_operator_power,%function\nliber_channel_liber_bluetooth_bluetooth_operator_power:\ntail liber_channel_impl_liber_bluetooth_bluetooth_operator_power\n.size liber_channel_liber_bluetooth_bluetooth_operator_power, . - liber_channel_liber_bluetooth_bluetooth_operator_power\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_pair,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_pair\n.type liber_channel_liber_bluetooth_bluetooth_operator_pair,@function\nliber_channel_liber_bluetooth_bluetooth_operator_pair:\njmp liber_channel_impl_liber_bluetooth_bluetooth_operator_pair\n.size liber_channel_liber_bluetooth_bluetooth_operator_pair, . - liber_channel_liber_bluetooth_bluetooth_operator_pair\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_pair,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_pair\n.type liber_channel_liber_bluetooth_bluetooth_operator_pair,%function\nliber_channel_liber_bluetooth_bluetooth_operator_pair:\nb liber_channel_impl_liber_bluetooth_bluetooth_operator_pair\n.size liber_channel_liber_bluetooth_bluetooth_operator_pair, . - liber_channel_liber_bluetooth_bluetooth_operator_pair\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_pair,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_pair\n.type liber_channel_liber_bluetooth_bluetooth_operator_pair,%function\nliber_channel_liber_bluetooth_bluetooth_operator_pair:\ntail liber_channel_impl_liber_bluetooth_bluetooth_operator_pair\n.size liber_channel_liber_bluetooth_bluetooth_operator_pair, . - liber_channel_liber_bluetooth_bluetooth_operator_pair\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_progress,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_progress\n.type liber_channel_liber_bluetooth_bluetooth_operator_progress,@function\nliber_channel_liber_bluetooth_bluetooth_operator_progress:\njmp liber_channel_impl_liber_bluetooth_bluetooth_operator_progress\n.size liber_channel_liber_bluetooth_bluetooth_operator_progress, . - liber_channel_liber_bluetooth_bluetooth_operator_progress\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_progress,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_progress\n.type liber_channel_liber_bluetooth_bluetooth_operator_progress,%function\nliber_channel_liber_bluetooth_bluetooth_operator_progress:\nb liber_channel_impl_liber_bluetooth_bluetooth_operator_progress\n.size liber_channel_liber_bluetooth_bluetooth_operator_progress, . - liber_channel_liber_bluetooth_bluetooth_operator_progress\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_progress,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_progress\n.type liber_channel_liber_bluetooth_bluetooth_operator_progress,%function\nliber_channel_liber_bluetooth_bluetooth_operator_progress:\ntail liber_channel_impl_liber_bluetooth_bluetooth_operator_progress\n.size liber_channel_liber_bluetooth_bluetooth_operator_progress, . - liber_channel_liber_bluetooth_bluetooth_operator_progress\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_bonded,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_bonded\n.type liber_channel_liber_bluetooth_bluetooth_operator_bonded,@function\nliber_channel_liber_bluetooth_bluetooth_operator_bonded:\njmp liber_channel_impl_liber_bluetooth_bluetooth_operator_bonded\n.size liber_channel_liber_bluetooth_bluetooth_operator_bonded, . - liber_channel_liber_bluetooth_bluetooth_operator_bonded\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_bonded,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_bonded\n.type liber_channel_liber_bluetooth_bluetooth_operator_bonded,%function\nliber_channel_liber_bluetooth_bluetooth_operator_bonded:\nb liber_channel_impl_liber_bluetooth_bluetooth_operator_bonded\n.size liber_channel_liber_bluetooth_bluetooth_operator_bonded, . - liber_channel_liber_bluetooth_bluetooth_operator_bonded\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_bonded,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_bonded\n.type liber_channel_liber_bluetooth_bluetooth_operator_bonded,%function\nliber_channel_liber_bluetooth_bluetooth_operator_bonded:\ntail liber_channel_impl_liber_bluetooth_bluetooth_operator_bonded\n.size liber_channel_liber_bluetooth_bluetooth_operator_bonded, . - liber_channel_liber_bluetooth_bluetooth_operator_bonded\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_forget,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_forget\n.type liber_channel_liber_bluetooth_bluetooth_operator_forget,@function\nliber_channel_liber_bluetooth_bluetooth_operator_forget:\njmp liber_channel_impl_liber_bluetooth_bluetooth_operator_forget\n.size liber_channel_liber_bluetooth_bluetooth_operator_forget, . - liber_channel_liber_bluetooth_bluetooth_operator_forget\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_forget,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_forget\n.type liber_channel_liber_bluetooth_bluetooth_operator_forget,%function\nliber_channel_liber_bluetooth_bluetooth_operator_forget:\nb liber_channel_impl_liber_bluetooth_bluetooth_operator_forget\n.size liber_channel_liber_bluetooth_bluetooth_operator_forget, . - liber_channel_liber_bluetooth_bluetooth_operator_forget\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_forget,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_forget\n.type liber_channel_liber_bluetooth_bluetooth_operator_forget,%function\nliber_channel_liber_bluetooth_bluetooth_operator_forget:\ntail liber_channel_impl_liber_bluetooth_bluetooth_operator_forget\n.size liber_channel_liber_bluetooth_bluetooth_operator_forget, . - liber_channel_liber_bluetooth_bluetooth_operator_forget\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_enable,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_enable\n.type liber_channel_liber_bluetooth_bluetooth_operator_enable,@function\nliber_channel_liber_bluetooth_bluetooth_operator_enable:\njmp liber_channel_impl_liber_bluetooth_bluetooth_operator_enable\n.size liber_channel_liber_bluetooth_bluetooth_operator_enable, . - liber_channel_liber_bluetooth_bluetooth_operator_enable\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_enable,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_enable\n.type liber_channel_liber_bluetooth_bluetooth_operator_enable,%function\nliber_channel_liber_bluetooth_bluetooth_operator_enable:\nb liber_channel_impl_liber_bluetooth_bluetooth_operator_enable\n.size liber_channel_liber_bluetooth_bluetooth_operator_enable, . - liber_channel_liber_bluetooth_bluetooth_operator_enable\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_enable,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_enable\n.type liber_channel_liber_bluetooth_bluetooth_operator_enable,%function\nliber_channel_liber_bluetooth_bluetooth_operator_enable:\ntail liber_channel_impl_liber_bluetooth_bluetooth_operator_enable\n.size liber_channel_liber_bluetooth_bluetooth_operator_enable, . - liber_channel_liber_bluetooth_bluetooth_operator_enable\n");
