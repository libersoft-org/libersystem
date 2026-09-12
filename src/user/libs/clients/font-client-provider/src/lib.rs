//! The trampolines that make the generated font clients reachable by name.
//!
//! One symbol per operation, each a tail jump to the `liber_channel_impl_*` the generated crate
//! exports. Written as assembly for the reason every other provider is: a Rust wrapper would be a
//! function the linker may inline or reorder, and what a consumer links against has to be an
//! address with a stable name.

#![no_std]

use core::arch::global_asm;

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_font_font_catalogue_list,\"ax\",@progbits\n.globl liber_channel_liber_font_font_catalogue_list\n.type liber_channel_liber_font_font_catalogue_list,@function\nliber_channel_liber_font_font_catalogue_list:\njmp liber_channel_impl_liber_font_font_catalogue_list\n.size liber_channel_liber_font_font_catalogue_list, . - liber_channel_liber_font_font_catalogue_list\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_font_font_catalogue_list,\"ax\",@progbits\n.globl liber_channel_liber_font_font_catalogue_list\n.type liber_channel_liber_font_font_catalogue_list,%function\nliber_channel_liber_font_font_catalogue_list:\nb liber_channel_impl_liber_font_font_catalogue_list\n.size liber_channel_liber_font_font_catalogue_list, . - liber_channel_liber_font_font_catalogue_list\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_font_font_catalogue_list,\"ax\",@progbits\n.globl liber_channel_liber_font_font_catalogue_list\n.type liber_channel_liber_font_font_catalogue_list,%function\nliber_channel_liber_font_font_catalogue_list:\ntail liber_channel_impl_liber_font_font_catalogue_list\n.size liber_channel_liber_font_font_catalogue_list, . - liber_channel_liber_font_font_catalogue_list\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_font_font_catalogue_resolve_info,\"ax\",@progbits\n.globl liber_channel_liber_font_font_catalogue_resolve_info\n.type liber_channel_liber_font_font_catalogue_resolve_info,@function\nliber_channel_liber_font_font_catalogue_resolve_info:\njmp liber_channel_impl_liber_font_font_catalogue_resolve_info\n.size liber_channel_liber_font_font_catalogue_resolve_info, . - liber_channel_liber_font_font_catalogue_resolve_info\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_font_font_catalogue_resolve_info,\"ax\",@progbits\n.globl liber_channel_liber_font_font_catalogue_resolve_info\n.type liber_channel_liber_font_font_catalogue_resolve_info,%function\nliber_channel_liber_font_font_catalogue_resolve_info:\nb liber_channel_impl_liber_font_font_catalogue_resolve_info\n.size liber_channel_liber_font_font_catalogue_resolve_info, . - liber_channel_liber_font_font_catalogue_resolve_info\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_font_font_catalogue_resolve_info,\"ax\",@progbits\n.globl liber_channel_liber_font_font_catalogue_resolve_info\n.type liber_channel_liber_font_font_catalogue_resolve_info,%function\nliber_channel_liber_font_font_catalogue_resolve_info:\ntail liber_channel_impl_liber_font_font_catalogue_resolve_info\n.size liber_channel_liber_font_font_catalogue_resolve_info, . - liber_channel_liber_font_font_catalogue_resolve_info\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_font_font_catalogue_resolve_into,\"ax\",@progbits\n.globl liber_channel_liber_font_font_catalogue_resolve_into\n.type liber_channel_liber_font_font_catalogue_resolve_into,@function\nliber_channel_liber_font_font_catalogue_resolve_into:\njmp liber_channel_impl_liber_font_font_catalogue_resolve_into\n.size liber_channel_liber_font_font_catalogue_resolve_into, . - liber_channel_liber_font_font_catalogue_resolve_into\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_font_font_catalogue_resolve_into,\"ax\",@progbits\n.globl liber_channel_liber_font_font_catalogue_resolve_into\n.type liber_channel_liber_font_font_catalogue_resolve_into,%function\nliber_channel_liber_font_font_catalogue_resolve_into:\nb liber_channel_impl_liber_font_font_catalogue_resolve_into\n.size liber_channel_liber_font_font_catalogue_resolve_into, . - liber_channel_liber_font_font_catalogue_resolve_into\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_font_font_catalogue_resolve_into,\"ax\",@progbits\n.globl liber_channel_liber_font_font_catalogue_resolve_into\n.type liber_channel_liber_font_font_catalogue_resolve_into,%function\nliber_channel_liber_font_font_catalogue_resolve_into:\ntail liber_channel_impl_liber_font_font_catalogue_resolve_into\n.size liber_channel_liber_font_font_catalogue_resolve_into, . - liber_channel_liber_font_font_catalogue_resolve_into\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_font_font_catalogue_subscribe,\"ax\",@progbits\n.globl liber_channel_liber_font_font_catalogue_subscribe\n.type liber_channel_liber_font_font_catalogue_subscribe,@function\nliber_channel_liber_font_font_catalogue_subscribe:\njmp liber_channel_impl_liber_font_font_catalogue_subscribe\n.size liber_channel_liber_font_font_catalogue_subscribe, . - liber_channel_liber_font_font_catalogue_subscribe\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_font_font_catalogue_subscribe,\"ax\",@progbits\n.globl liber_channel_liber_font_font_catalogue_subscribe\n.type liber_channel_liber_font_font_catalogue_subscribe,%function\nliber_channel_liber_font_font_catalogue_subscribe:\nb liber_channel_impl_liber_font_font_catalogue_subscribe\n.size liber_channel_liber_font_font_catalogue_subscribe, . - liber_channel_liber_font_font_catalogue_subscribe\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_font_font_catalogue_subscribe,\"ax\",@progbits\n.globl liber_channel_liber_font_font_catalogue_subscribe\n.type liber_channel_liber_font_font_catalogue_subscribe,%function\nliber_channel_liber_font_font_catalogue_subscribe:\ntail liber_channel_impl_liber_font_font_catalogue_subscribe\n.size liber_channel_liber_font_font_catalogue_subscribe, . - liber_channel_liber_font_font_catalogue_subscribe\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_font_font_catalogue_admin_rescan,\"ax\",@progbits\n.globl liber_channel_liber_font_font_catalogue_admin_rescan\n.type liber_channel_liber_font_font_catalogue_admin_rescan,@function\nliber_channel_liber_font_font_catalogue_admin_rescan:\njmp liber_channel_impl_liber_font_font_catalogue_admin_rescan\n.size liber_channel_liber_font_font_catalogue_admin_rescan, . - liber_channel_liber_font_font_catalogue_admin_rescan\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_font_font_catalogue_admin_rescan,\"ax\",@progbits\n.globl liber_channel_liber_font_font_catalogue_admin_rescan\n.type liber_channel_liber_font_font_catalogue_admin_rescan,%function\nliber_channel_liber_font_font_catalogue_admin_rescan:\nb liber_channel_impl_liber_font_font_catalogue_admin_rescan\n.size liber_channel_liber_font_font_catalogue_admin_rescan, . - liber_channel_liber_font_font_catalogue_admin_rescan\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_font_font_catalogue_admin_rescan,\"ax\",@progbits\n.globl liber_channel_liber_font_font_catalogue_admin_rescan\n.type liber_channel_liber_font_font_catalogue_admin_rescan,%function\nliber_channel_liber_font_font_catalogue_admin_rescan:\ntail liber_channel_impl_liber_font_font_catalogue_admin_rescan\n.size liber_channel_liber_font_font_catalogue_admin_rescan, . - liber_channel_liber_font_font_catalogue_admin_rescan\n");
