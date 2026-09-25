//! The trampolines that make the generated administrative-request clients reachable by name.
//!
//! One symbol per operation the `dfu` tool uses, each a tail jump to the `liber_channel_impl_*` the generated
//! crate exports - the same shape as every other client provider, for the same reason: a consumer links
//! against an address with a stable name, not against a function the linker may inline or reorder.

#![no_std]

use core::arch::global_asm;

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_admin_admin_request_request,\"ax\",@progbits\n.globl liber_channel_liber_admin_admin_request_request\n.type liber_channel_liber_admin_admin_request_request,@function\nliber_channel_liber_admin_admin_request_request:\njmp liber_channel_impl_liber_admin_admin_request_request\n.size liber_channel_liber_admin_admin_request_request, . - liber_channel_liber_admin_admin_request_request\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_admin_admin_request_request,\"ax\",@progbits\n.globl liber_channel_liber_admin_admin_request_request\n.type liber_channel_liber_admin_admin_request_request,%function\nliber_channel_liber_admin_admin_request_request:\nb liber_channel_impl_liber_admin_admin_request_request\n.size liber_channel_liber_admin_admin_request_request, . - liber_channel_liber_admin_admin_request_request\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_admin_admin_request_request,\"ax\",@progbits\n.globl liber_channel_liber_admin_admin_request_request\n.type liber_channel_liber_admin_admin_request_request,%function\nliber_channel_liber_admin_admin_request_request:\ntail liber_channel_impl_liber_admin_admin_request_request\n.size liber_channel_liber_admin_admin_request_request, . - liber_channel_liber_admin_admin_request_request\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_admin_admin_authority_execute,\"ax\",@progbits\n.globl liber_channel_liber_admin_admin_authority_execute\n.type liber_channel_liber_admin_admin_authority_execute,@function\nliber_channel_liber_admin_admin_authority_execute:\njmp liber_channel_impl_liber_admin_admin_authority_execute\n.size liber_channel_liber_admin_admin_authority_execute, . - liber_channel_liber_admin_admin_authority_execute\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_admin_admin_authority_execute,\"ax\",@progbits\n.globl liber_channel_liber_admin_admin_authority_execute\n.type liber_channel_liber_admin_admin_authority_execute,%function\nliber_channel_liber_admin_admin_authority_execute:\nb liber_channel_impl_liber_admin_admin_authority_execute\n.size liber_channel_liber_admin_admin_authority_execute, . - liber_channel_liber_admin_admin_authority_execute\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_admin_admin_authority_execute,\"ax\",@progbits\n.globl liber_channel_liber_admin_admin_authority_execute\n.type liber_channel_liber_admin_admin_authority_execute,%function\nliber_channel_liber_admin_admin_authority_execute:\ntail liber_channel_impl_liber_admin_admin_authority_execute\n.size liber_channel_liber_admin_admin_authority_execute, . - liber_channel_liber_admin_admin_authority_execute\n");
