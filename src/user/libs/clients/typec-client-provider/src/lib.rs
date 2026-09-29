//! The trampoline that makes the generated Type-C client reachable by name: one symbol for the one operation `typec`
//! uses, a tail jump to the `liber_channel_impl_*` the generated crate exports - the shape of every client provider.

#![no_std]

use core::arch::global_asm;

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_typec_typec_connectors,\"ax\",@progbits\n.globl liber_channel_liber_typec_typec_connectors\n.type liber_channel_liber_typec_typec_connectors,@function\nliber_channel_liber_typec_typec_connectors:\njmp liber_channel_impl_liber_typec_typec_connectors\n.size liber_channel_liber_typec_typec_connectors, . - liber_channel_liber_typec_typec_connectors\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_typec_typec_connectors,\"ax\",@progbits\n.globl liber_channel_liber_typec_typec_connectors\n.type liber_channel_liber_typec_typec_connectors,%function\nliber_channel_liber_typec_typec_connectors:\nb liber_channel_impl_liber_typec_typec_connectors\n.size liber_channel_liber_typec_typec_connectors, . - liber_channel_liber_typec_typec_connectors\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_typec_typec_connectors,\"ax\",@progbits\n.globl liber_channel_liber_typec_typec_connectors\n.type liber_channel_liber_typec_typec_connectors,%function\nliber_channel_liber_typec_typec_connectors:\ntail liber_channel_impl_liber_typec_typec_connectors\n.size liber_channel_liber_typec_typec_connectors, . - liber_channel_liber_typec_typec_connectors\n");
