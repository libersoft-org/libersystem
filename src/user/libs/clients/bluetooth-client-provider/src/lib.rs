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
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_cancel,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_cancel\n.type liber_channel_liber_bluetooth_bluetooth_cancel,@function\nliber_channel_liber_bluetooth_bluetooth_cancel:\njmp liber_channel_impl_liber_bluetooth_bluetooth_cancel\n.size liber_channel_liber_bluetooth_bluetooth_cancel, . - liber_channel_liber_bluetooth_bluetooth_cancel\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_cancel,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_cancel\n.type liber_channel_liber_bluetooth_bluetooth_cancel,%function\nliber_channel_liber_bluetooth_bluetooth_cancel:\nb liber_channel_impl_liber_bluetooth_bluetooth_cancel\n.size liber_channel_liber_bluetooth_bluetooth_cancel, . - liber_channel_liber_bluetooth_bluetooth_cancel\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_cancel,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_cancel\n.type liber_channel_liber_bluetooth_bluetooth_cancel,%function\nliber_channel_liber_bluetooth_bluetooth_cancel:\ntail liber_channel_impl_liber_bluetooth_bluetooth_cancel\n.size liber_channel_liber_bluetooth_bluetooth_cancel, . - liber_channel_liber_bluetooth_bluetooth_cancel\n");

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
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_cancel,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_cancel\n.type liber_channel_liber_bluetooth_bluetooth_operator_cancel,@function\nliber_channel_liber_bluetooth_bluetooth_operator_cancel:\njmp liber_channel_impl_liber_bluetooth_bluetooth_operator_cancel\n.size liber_channel_liber_bluetooth_bluetooth_operator_cancel, . - liber_channel_liber_bluetooth_bluetooth_operator_cancel\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_cancel,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_cancel\n.type liber_channel_liber_bluetooth_bluetooth_operator_cancel,%function\nliber_channel_liber_bluetooth_bluetooth_operator_cancel:\nb liber_channel_impl_liber_bluetooth_bluetooth_operator_cancel\n.size liber_channel_liber_bluetooth_bluetooth_operator_cancel, . - liber_channel_liber_bluetooth_bluetooth_operator_cancel\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_cancel,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_cancel\n.type liber_channel_liber_bluetooth_bluetooth_operator_cancel,%function\nliber_channel_liber_bluetooth_bluetooth_operator_cancel:\ntail liber_channel_impl_liber_bluetooth_bluetooth_operator_cancel\n.size liber_channel_liber_bluetooth_bluetooth_operator_cancel, . - liber_channel_liber_bluetooth_bluetooth_operator_cancel\n");

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

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_prompts,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_prompts\n.type liber_channel_liber_bluetooth_bluetooth_operator_prompts,@function\nliber_channel_liber_bluetooth_bluetooth_operator_prompts:\njmp liber_channel_impl_liber_bluetooth_bluetooth_operator_prompts\n.size liber_channel_liber_bluetooth_bluetooth_operator_prompts, . - liber_channel_liber_bluetooth_bluetooth_operator_prompts\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_prompts,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_prompts\n.type liber_channel_liber_bluetooth_bluetooth_operator_prompts,%function\nliber_channel_liber_bluetooth_bluetooth_operator_prompts:\nb liber_channel_impl_liber_bluetooth_bluetooth_operator_prompts\n.size liber_channel_liber_bluetooth_bluetooth_operator_prompts, . - liber_channel_liber_bluetooth_bluetooth_operator_prompts\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_prompts,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_prompts\n.type liber_channel_liber_bluetooth_bluetooth_operator_prompts,%function\nliber_channel_liber_bluetooth_bluetooth_operator_prompts:\ntail liber_channel_impl_liber_bluetooth_bluetooth_operator_prompts\n.size liber_channel_liber_bluetooth_bluetooth_operator_prompts, . - liber_channel_liber_bluetooth_bluetooth_operator_prompts\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_answer,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_answer\n.type liber_channel_liber_bluetooth_bluetooth_operator_answer,@function\nliber_channel_liber_bluetooth_bluetooth_operator_answer:\njmp liber_channel_impl_liber_bluetooth_bluetooth_operator_answer\n.size liber_channel_liber_bluetooth_bluetooth_operator_answer, . - liber_channel_liber_bluetooth_bluetooth_operator_answer\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_answer,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_answer\n.type liber_channel_liber_bluetooth_bluetooth_operator_answer,%function\nliber_channel_liber_bluetooth_bluetooth_operator_answer:\nb liber_channel_impl_liber_bluetooth_bluetooth_operator_answer\n.size liber_channel_liber_bluetooth_bluetooth_operator_answer, . - liber_channel_liber_bluetooth_bluetooth_operator_answer\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_answer,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_answer\n.type liber_channel_liber_bluetooth_bluetooth_operator_answer,%function\nliber_channel_liber_bluetooth_bluetooth_operator_answer:\ntail liber_channel_impl_liber_bluetooth_bluetooth_operator_answer\n.size liber_channel_liber_bluetooth_bluetooth_operator_answer, . - liber_channel_liber_bluetooth_bluetooth_operator_answer\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_discoverable,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_discoverable\n.type liber_channel_liber_bluetooth_bluetooth_operator_discoverable,@function\nliber_channel_liber_bluetooth_bluetooth_operator_discoverable:\njmp liber_channel_impl_liber_bluetooth_bluetooth_operator_discoverable\n.size liber_channel_liber_bluetooth_bluetooth_operator_discoverable, . - liber_channel_liber_bluetooth_bluetooth_operator_discoverable\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_discoverable,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_discoverable\n.type liber_channel_liber_bluetooth_bluetooth_operator_discoverable,%function\nliber_channel_liber_bluetooth_bluetooth_operator_discoverable:\nb liber_channel_impl_liber_bluetooth_bluetooth_operator_discoverable\n.size liber_channel_liber_bluetooth_bluetooth_operator_discoverable, . - liber_channel_liber_bluetooth_bluetooth_operator_discoverable\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_discoverable,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_discoverable\n.type liber_channel_liber_bluetooth_bluetooth_operator_discoverable,%function\nliber_channel_liber_bluetooth_bluetooth_operator_discoverable:\ntail liber_channel_impl_liber_bluetooth_bluetooth_operator_discoverable\n.size liber_channel_liber_bluetooth_bluetooth_operator_discoverable, . - liber_channel_liber_bluetooth_bluetooth_operator_discoverable\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_trust,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_trust\n.type liber_channel_liber_bluetooth_bluetooth_operator_trust,@function\nliber_channel_liber_bluetooth_bluetooth_operator_trust:\njmp liber_channel_impl_liber_bluetooth_bluetooth_operator_trust\n.size liber_channel_liber_bluetooth_bluetooth_operator_trust, . - liber_channel_liber_bluetooth_bluetooth_operator_trust\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_trust,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_trust\n.type liber_channel_liber_bluetooth_bluetooth_operator_trust,%function\nliber_channel_liber_bluetooth_bluetooth_operator_trust:\nb liber_channel_impl_liber_bluetooth_bluetooth_operator_trust\n.size liber_channel_liber_bluetooth_bluetooth_operator_trust, . - liber_channel_liber_bluetooth_bluetooth_operator_trust\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_trust,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_trust\n.type liber_channel_liber_bluetooth_bluetooth_operator_trust,%function\nliber_channel_liber_bluetooth_bluetooth_operator_trust:\ntail liber_channel_impl_liber_bluetooth_bluetooth_operator_trust\n.size liber_channel_liber_bluetooth_bluetooth_operator_trust, . - liber_channel_liber_bluetooth_bluetooth_operator_trust\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_alias,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_alias\n.type liber_channel_liber_bluetooth_bluetooth_operator_alias,@function\nliber_channel_liber_bluetooth_bluetooth_operator_alias:\njmp liber_channel_impl_liber_bluetooth_bluetooth_operator_alias\n.size liber_channel_liber_bluetooth_bluetooth_operator_alias, . - liber_channel_liber_bluetooth_bluetooth_operator_alias\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_alias,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_alias\n.type liber_channel_liber_bluetooth_bluetooth_operator_alias,%function\nliber_channel_liber_bluetooth_bluetooth_operator_alias:\nb liber_channel_impl_liber_bluetooth_bluetooth_operator_alias\n.size liber_channel_liber_bluetooth_bluetooth_operator_alias, . - liber_channel_liber_bluetooth_bluetooth_operator_alias\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_alias,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_alias\n.type liber_channel_liber_bluetooth_bluetooth_operator_alias,%function\nliber_channel_liber_bluetooth_bluetooth_operator_alias:\ntail liber_channel_impl_liber_bluetooth_bluetooth_operator_alias\n.size liber_channel_liber_bluetooth_bluetooth_operator_alias, . - liber_channel_liber_bluetooth_bluetooth_operator_alias\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_devices,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_devices\n.type liber_channel_liber_bluetooth_bluetooth_operator_devices,@function\nliber_channel_liber_bluetooth_bluetooth_operator_devices:\njmp liber_channel_impl_liber_bluetooth_bluetooth_operator_devices\n.size liber_channel_liber_bluetooth_bluetooth_operator_devices, . - liber_channel_liber_bluetooth_bluetooth_operator_devices\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_devices,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_devices\n.type liber_channel_liber_bluetooth_bluetooth_operator_devices,%function\nliber_channel_liber_bluetooth_bluetooth_operator_devices:\nb liber_channel_impl_liber_bluetooth_bluetooth_operator_devices\n.size liber_channel_liber_bluetooth_bluetooth_operator_devices, . - liber_channel_liber_bluetooth_bluetooth_operator_devices\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_devices,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_devices\n.type liber_channel_liber_bluetooth_bluetooth_operator_devices,%function\nliber_channel_liber_bluetooth_bluetooth_operator_devices:\ntail liber_channel_impl_liber_bluetooth_bluetooth_operator_devices\n.size liber_channel_liber_bluetooth_bluetooth_operator_devices, . - liber_channel_liber_bluetooth_bluetooth_operator_devices\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_connect,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_connect\n.type liber_channel_liber_bluetooth_bluetooth_operator_connect,@function\nliber_channel_liber_bluetooth_bluetooth_operator_connect:\njmp liber_channel_impl_liber_bluetooth_bluetooth_operator_connect\n.size liber_channel_liber_bluetooth_bluetooth_operator_connect, . - liber_channel_liber_bluetooth_bluetooth_operator_connect\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_connect,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_connect\n.type liber_channel_liber_bluetooth_bluetooth_operator_connect,%function\nliber_channel_liber_bluetooth_bluetooth_operator_connect:\nb liber_channel_impl_liber_bluetooth_bluetooth_operator_connect\n.size liber_channel_liber_bluetooth_bluetooth_operator_connect, . - liber_channel_liber_bluetooth_bluetooth_operator_connect\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_connect,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_connect\n.type liber_channel_liber_bluetooth_bluetooth_operator_connect,%function\nliber_channel_liber_bluetooth_bluetooth_operator_connect:\ntail liber_channel_impl_liber_bluetooth_bluetooth_operator_connect\n.size liber_channel_liber_bluetooth_bluetooth_operator_connect, . - liber_channel_liber_bluetooth_bluetooth_operator_connect\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_disconnect,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_disconnect\n.type liber_channel_liber_bluetooth_bluetooth_operator_disconnect,@function\nliber_channel_liber_bluetooth_bluetooth_operator_disconnect:\njmp liber_channel_impl_liber_bluetooth_bluetooth_operator_disconnect\n.size liber_channel_liber_bluetooth_bluetooth_operator_disconnect, . - liber_channel_liber_bluetooth_bluetooth_operator_disconnect\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_disconnect,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_disconnect\n.type liber_channel_liber_bluetooth_bluetooth_operator_disconnect,%function\nliber_channel_liber_bluetooth_bluetooth_operator_disconnect:\nb liber_channel_impl_liber_bluetooth_bluetooth_operator_disconnect\n.size liber_channel_liber_bluetooth_bluetooth_operator_disconnect, . - liber_channel_liber_bluetooth_bluetooth_operator_disconnect\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_disconnect,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_disconnect\n.type liber_channel_liber_bluetooth_bluetooth_operator_disconnect,%function\nliber_channel_liber_bluetooth_bluetooth_operator_disconnect:\ntail liber_channel_impl_liber_bluetooth_bluetooth_operator_disconnect\n.size liber_channel_liber_bluetooth_bluetooth_operator_disconnect, . - liber_channel_liber_bluetooth_bluetooth_operator_disconnect\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_pair_legacy,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_pair_legacy\n.type liber_channel_liber_bluetooth_bluetooth_operator_pair_legacy,@function\nliber_channel_liber_bluetooth_bluetooth_operator_pair_legacy:\njmp liber_channel_impl_liber_bluetooth_bluetooth_operator_pair_legacy\n.size liber_channel_liber_bluetooth_bluetooth_operator_pair_legacy, . - liber_channel_liber_bluetooth_bluetooth_operator_pair_legacy\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_pair_legacy,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_pair_legacy\n.type liber_channel_liber_bluetooth_bluetooth_operator_pair_legacy,%function\nliber_channel_liber_bluetooth_bluetooth_operator_pair_legacy:\nb liber_channel_impl_liber_bluetooth_bluetooth_operator_pair_legacy\n.size liber_channel_liber_bluetooth_bluetooth_operator_pair_legacy, . - liber_channel_liber_bluetooth_bluetooth_operator_pair_legacy\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_pair_legacy,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_pair_legacy\n.type liber_channel_liber_bluetooth_bluetooth_operator_pair_legacy,%function\nliber_channel_liber_bluetooth_bluetooth_operator_pair_legacy:\ntail liber_channel_impl_liber_bluetooth_bluetooth_operator_pair_legacy\n.size liber_channel_liber_bluetooth_bluetooth_operator_pair_legacy, . - liber_channel_liber_bluetooth_bluetooth_operator_pair_legacy\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_media,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_media\n.type liber_channel_liber_bluetooth_bluetooth_operator_media,@function\nliber_channel_liber_bluetooth_bluetooth_operator_media:\njmp liber_channel_impl_liber_bluetooth_bluetooth_operator_media\n.size liber_channel_liber_bluetooth_bluetooth_operator_media, . - liber_channel_liber_bluetooth_bluetooth_operator_media\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_media,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_media\n.type liber_channel_liber_bluetooth_bluetooth_operator_media,%function\nliber_channel_liber_bluetooth_bluetooth_operator_media:\nb liber_channel_impl_liber_bluetooth_bluetooth_operator_media\n.size liber_channel_liber_bluetooth_bluetooth_operator_media, . - liber_channel_liber_bluetooth_bluetooth_operator_media\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_media,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_media\n.type liber_channel_liber_bluetooth_bluetooth_operator_media,%function\nliber_channel_liber_bluetooth_bluetooth_operator_media:\ntail liber_channel_impl_liber_bluetooth_bluetooth_operator_media\n.size liber_channel_liber_bluetooth_bluetooth_operator_media, . - liber_channel_liber_bluetooth_bluetooth_operator_media\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_send,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_send\n.type liber_channel_liber_bluetooth_bluetooth_operator_send,@function\nliber_channel_liber_bluetooth_bluetooth_operator_send:\njmp liber_channel_impl_liber_bluetooth_bluetooth_operator_send\n.size liber_channel_liber_bluetooth_bluetooth_operator_send, . - liber_channel_liber_bluetooth_bluetooth_operator_send\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_send,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_send\n.type liber_channel_liber_bluetooth_bluetooth_operator_send,%function\nliber_channel_liber_bluetooth_bluetooth_operator_send:\nb liber_channel_impl_liber_bluetooth_bluetooth_operator_send\n.size liber_channel_liber_bluetooth_bluetooth_operator_send, . - liber_channel_liber_bluetooth_bluetooth_operator_send\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_send,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_send\n.type liber_channel_liber_bluetooth_bluetooth_operator_send,%function\nliber_channel_liber_bluetooth_bluetooth_operator_send:\ntail liber_channel_impl_liber_bluetooth_bluetooth_operator_send\n.size liber_channel_liber_bluetooth_bluetooth_operator_send, . - liber_channel_liber_bluetooth_bluetooth_operator_send\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_receive,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_receive\n.type liber_channel_liber_bluetooth_bluetooth_operator_receive,@function\nliber_channel_liber_bluetooth_bluetooth_operator_receive:\njmp liber_channel_impl_liber_bluetooth_bluetooth_operator_receive\n.size liber_channel_liber_bluetooth_bluetooth_operator_receive, . - liber_channel_liber_bluetooth_bluetooth_operator_receive\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_receive,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_receive\n.type liber_channel_liber_bluetooth_bluetooth_operator_receive,%function\nliber_channel_liber_bluetooth_bluetooth_operator_receive:\nb liber_channel_impl_liber_bluetooth_bluetooth_operator_receive\n.size liber_channel_liber_bluetooth_bluetooth_operator_receive, . - liber_channel_liber_bluetooth_bluetooth_operator_receive\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_receive,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_receive\n.type liber_channel_liber_bluetooth_bluetooth_operator_receive,%function\nliber_channel_liber_bluetooth_bluetooth_operator_receive:\ntail liber_channel_impl_liber_bluetooth_bluetooth_operator_receive\n.size liber_channel_liber_bluetooth_bluetooth_operator_receive, . - liber_channel_liber_bluetooth_bluetooth_operator_receive\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_object_push_write,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_object_push_write\n.type liber_channel_liber_bluetooth_object_push_write,@function\nliber_channel_liber_bluetooth_object_push_write:\njmp liber_channel_impl_liber_bluetooth_object_push_write\n.size liber_channel_liber_bluetooth_object_push_write, . - liber_channel_liber_bluetooth_object_push_write\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_object_push_write,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_object_push_write\n.type liber_channel_liber_bluetooth_object_push_write,%function\nliber_channel_liber_bluetooth_object_push_write:\nb liber_channel_impl_liber_bluetooth_object_push_write\n.size liber_channel_liber_bluetooth_object_push_write, . - liber_channel_liber_bluetooth_object_push_write\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_object_push_write,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_object_push_write\n.type liber_channel_liber_bluetooth_object_push_write,%function\nliber_channel_liber_bluetooth_object_push_write:\ntail liber_channel_impl_liber_bluetooth_object_push_write\n.size liber_channel_liber_bluetooth_object_push_write, . - liber_channel_liber_bluetooth_object_push_write\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_object_push_finish,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_object_push_finish\n.type liber_channel_liber_bluetooth_object_push_finish,@function\nliber_channel_liber_bluetooth_object_push_finish:\njmp liber_channel_impl_liber_bluetooth_object_push_finish\n.size liber_channel_liber_bluetooth_object_push_finish, . - liber_channel_liber_bluetooth_object_push_finish\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_object_push_finish,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_object_push_finish\n.type liber_channel_liber_bluetooth_object_push_finish,%function\nliber_channel_liber_bluetooth_object_push_finish:\nb liber_channel_impl_liber_bluetooth_object_push_finish\n.size liber_channel_liber_bluetooth_object_push_finish, . - liber_channel_liber_bluetooth_object_push_finish\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_object_push_finish,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_object_push_finish\n.type liber_channel_liber_bluetooth_object_push_finish,%function\nliber_channel_liber_bluetooth_object_push_finish:\ntail liber_channel_impl_liber_bluetooth_object_push_finish\n.size liber_channel_liber_bluetooth_object_push_finish, . - liber_channel_liber_bluetooth_object_push_finish\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_object_push_abort,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_object_push_abort\n.type liber_channel_liber_bluetooth_object_push_abort,@function\nliber_channel_liber_bluetooth_object_push_abort:\njmp liber_channel_impl_liber_bluetooth_object_push_abort\n.size liber_channel_liber_bluetooth_object_push_abort, . - liber_channel_liber_bluetooth_object_push_abort\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_object_push_abort,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_object_push_abort\n.type liber_channel_liber_bluetooth_object_push_abort,%function\nliber_channel_liber_bluetooth_object_push_abort:\nb liber_channel_impl_liber_bluetooth_object_push_abort\n.size liber_channel_liber_bluetooth_object_push_abort, . - liber_channel_liber_bluetooth_object_push_abort\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_object_push_abort,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_object_push_abort\n.type liber_channel_liber_bluetooth_object_push_abort,%function\nliber_channel_liber_bluetooth_object_push_abort:\ntail liber_channel_impl_liber_bluetooth_object_push_abort\n.size liber_channel_liber_bluetooth_object_push_abort, . - liber_channel_liber_bluetooth_object_push_abort\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_connect_pan,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_connect_pan\n.type liber_channel_liber_bluetooth_bluetooth_operator_connect_pan,@function\nliber_channel_liber_bluetooth_bluetooth_operator_connect_pan:\njmp liber_channel_impl_liber_bluetooth_bluetooth_operator_connect_pan\n.size liber_channel_liber_bluetooth_bluetooth_operator_connect_pan, . - liber_channel_liber_bluetooth_bluetooth_operator_connect_pan\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_connect_pan,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_connect_pan\n.type liber_channel_liber_bluetooth_bluetooth_operator_connect_pan,%function\nliber_channel_liber_bluetooth_bluetooth_operator_connect_pan:\nb liber_channel_impl_liber_bluetooth_bluetooth_operator_connect_pan\n.size liber_channel_liber_bluetooth_bluetooth_operator_connect_pan, . - liber_channel_liber_bluetooth_bluetooth_operator_connect_pan\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bluetooth_bluetooth_operator_connect_pan,\"ax\",@progbits\n.globl liber_channel_liber_bluetooth_bluetooth_operator_connect_pan\n.type liber_channel_liber_bluetooth_bluetooth_operator_connect_pan,%function\nliber_channel_liber_bluetooth_bluetooth_operator_connect_pan:\ntail liber_channel_impl_liber_bluetooth_bluetooth_operator_connect_pan\n.size liber_channel_liber_bluetooth_bluetooth_operator_connect_pan, . - liber_channel_liber_bluetooth_bluetooth_operator_connect_pan\n");
