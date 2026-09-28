//! The trampolines that make the generated TPM client reachable by name.
//!
//! One symbol per operation of `liber:tpm@1/tpm`, each a tail jump to the `liber_channel_impl_*` the generated
//! crate exports - the same shape as every other client provider, for the same reason: a consumer links against an
//! address with a stable name, not against a function the linker may inline or reorder.

#![no_std]

use core::arch::global_asm;

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_tpm_tpm_info,\"ax\",@progbits\n.globl liber_channel_liber_tpm_tpm_info\n.type liber_channel_liber_tpm_tpm_info,@function\nliber_channel_liber_tpm_tpm_info:\njmp liber_channel_impl_liber_tpm_tpm_info\n.size liber_channel_liber_tpm_tpm_info, . - liber_channel_liber_tpm_tpm_info\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_tpm_tpm_info,\"ax\",@progbits\n.globl liber_channel_liber_tpm_tpm_info\n.type liber_channel_liber_tpm_tpm_info,%function\nliber_channel_liber_tpm_tpm_info:\nb liber_channel_impl_liber_tpm_tpm_info\n.size liber_channel_liber_tpm_tpm_info, . - liber_channel_liber_tpm_tpm_info\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_tpm_tpm_info,\"ax\",@progbits\n.globl liber_channel_liber_tpm_tpm_info\n.type liber_channel_liber_tpm_tpm_info,%function\nliber_channel_liber_tpm_tpm_info:\ntail liber_channel_impl_liber_tpm_tpm_info\n.size liber_channel_liber_tpm_tpm_info, . - liber_channel_liber_tpm_tpm_info\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_tpm_tpm_random,\"ax\",@progbits\n.globl liber_channel_liber_tpm_tpm_random\n.type liber_channel_liber_tpm_tpm_random,@function\nliber_channel_liber_tpm_tpm_random:\njmp liber_channel_impl_liber_tpm_tpm_random\n.size liber_channel_liber_tpm_tpm_random, . - liber_channel_liber_tpm_tpm_random\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_tpm_tpm_random,\"ax\",@progbits\n.globl liber_channel_liber_tpm_tpm_random\n.type liber_channel_liber_tpm_tpm_random,%function\nliber_channel_liber_tpm_tpm_random:\nb liber_channel_impl_liber_tpm_tpm_random\n.size liber_channel_liber_tpm_tpm_random, . - liber_channel_liber_tpm_tpm_random\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_tpm_tpm_random,\"ax\",@progbits\n.globl liber_channel_liber_tpm_tpm_random\n.type liber_channel_liber_tpm_tpm_random,%function\nliber_channel_liber_tpm_tpm_random:\ntail liber_channel_impl_liber_tpm_tpm_random\n.size liber_channel_liber_tpm_tpm_random, . - liber_channel_liber_tpm_tpm_random\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_tpm_tpm_pcr_read,\"ax\",@progbits\n.globl liber_channel_liber_tpm_tpm_pcr_read\n.type liber_channel_liber_tpm_tpm_pcr_read,@function\nliber_channel_liber_tpm_tpm_pcr_read:\njmp liber_channel_impl_liber_tpm_tpm_pcr_read\n.size liber_channel_liber_tpm_tpm_pcr_read, . - liber_channel_liber_tpm_tpm_pcr_read\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_tpm_tpm_pcr_read,\"ax\",@progbits\n.globl liber_channel_liber_tpm_tpm_pcr_read\n.type liber_channel_liber_tpm_tpm_pcr_read,%function\nliber_channel_liber_tpm_tpm_pcr_read:\nb liber_channel_impl_liber_tpm_tpm_pcr_read\n.size liber_channel_liber_tpm_tpm_pcr_read, . - liber_channel_liber_tpm_tpm_pcr_read\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_tpm_tpm_pcr_read,\"ax\",@progbits\n.globl liber_channel_liber_tpm_tpm_pcr_read\n.type liber_channel_liber_tpm_tpm_pcr_read,%function\nliber_channel_liber_tpm_tpm_pcr_read:\ntail liber_channel_impl_liber_tpm_tpm_pcr_read\n.size liber_channel_liber_tpm_tpm_pcr_read, . - liber_channel_liber_tpm_tpm_pcr_read\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_tpm_tpm_pcr_extend,\"ax\",@progbits\n.globl liber_channel_liber_tpm_tpm_pcr_extend\n.type liber_channel_liber_tpm_tpm_pcr_extend,@function\nliber_channel_liber_tpm_tpm_pcr_extend:\njmp liber_channel_impl_liber_tpm_tpm_pcr_extend\n.size liber_channel_liber_tpm_tpm_pcr_extend, . - liber_channel_liber_tpm_tpm_pcr_extend\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_tpm_tpm_pcr_extend,\"ax\",@progbits\n.globl liber_channel_liber_tpm_tpm_pcr_extend\n.type liber_channel_liber_tpm_tpm_pcr_extend,%function\nliber_channel_liber_tpm_tpm_pcr_extend:\nb liber_channel_impl_liber_tpm_tpm_pcr_extend\n.size liber_channel_liber_tpm_tpm_pcr_extend, . - liber_channel_liber_tpm_tpm_pcr_extend\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_tpm_tpm_pcr_extend,\"ax\",@progbits\n.globl liber_channel_liber_tpm_tpm_pcr_extend\n.type liber_channel_liber_tpm_tpm_pcr_extend,%function\nliber_channel_liber_tpm_tpm_pcr_extend:\ntail liber_channel_impl_liber_tpm_tpm_pcr_extend\n.size liber_channel_liber_tpm_tpm_pcr_extend, . - liber_channel_liber_tpm_tpm_pcr_extend\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_tpm_tpm_seal,\"ax\",@progbits\n.globl liber_channel_liber_tpm_tpm_seal\n.type liber_channel_liber_tpm_tpm_seal,@function\nliber_channel_liber_tpm_tpm_seal:\njmp liber_channel_impl_liber_tpm_tpm_seal\n.size liber_channel_liber_tpm_tpm_seal, . - liber_channel_liber_tpm_tpm_seal\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_tpm_tpm_seal,\"ax\",@progbits\n.globl liber_channel_liber_tpm_tpm_seal\n.type liber_channel_liber_tpm_tpm_seal,%function\nliber_channel_liber_tpm_tpm_seal:\nb liber_channel_impl_liber_tpm_tpm_seal\n.size liber_channel_liber_tpm_tpm_seal, . - liber_channel_liber_tpm_tpm_seal\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_tpm_tpm_seal,\"ax\",@progbits\n.globl liber_channel_liber_tpm_tpm_seal\n.type liber_channel_liber_tpm_tpm_seal,%function\nliber_channel_liber_tpm_tpm_seal:\ntail liber_channel_impl_liber_tpm_tpm_seal\n.size liber_channel_liber_tpm_tpm_seal, . - liber_channel_liber_tpm_tpm_seal\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_tpm_tpm_unseal,\"ax\",@progbits\n.globl liber_channel_liber_tpm_tpm_unseal\n.type liber_channel_liber_tpm_tpm_unseal,@function\nliber_channel_liber_tpm_tpm_unseal:\njmp liber_channel_impl_liber_tpm_tpm_unseal\n.size liber_channel_liber_tpm_tpm_unseal, . - liber_channel_liber_tpm_tpm_unseal\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_tpm_tpm_unseal,\"ax\",@progbits\n.globl liber_channel_liber_tpm_tpm_unseal\n.type liber_channel_liber_tpm_tpm_unseal,%function\nliber_channel_liber_tpm_tpm_unseal:\nb liber_channel_impl_liber_tpm_tpm_unseal\n.size liber_channel_liber_tpm_tpm_unseal, . - liber_channel_liber_tpm_tpm_unseal\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_tpm_tpm_unseal,\"ax\",@progbits\n.globl liber_channel_liber_tpm_tpm_unseal\n.type liber_channel_liber_tpm_tpm_unseal,%function\nliber_channel_liber_tpm_tpm_unseal:\ntail liber_channel_impl_liber_tpm_tpm_unseal\n.size liber_channel_liber_tpm_tpm_unseal, . - liber_channel_liber_tpm_tpm_unseal\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_tpm_tpm_quote,\"ax\",@progbits\n.globl liber_channel_liber_tpm_tpm_quote\n.type liber_channel_liber_tpm_tpm_quote,@function\nliber_channel_liber_tpm_tpm_quote:\njmp liber_channel_impl_liber_tpm_tpm_quote\n.size liber_channel_liber_tpm_tpm_quote, . - liber_channel_liber_tpm_tpm_quote\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_tpm_tpm_quote,\"ax\",@progbits\n.globl liber_channel_liber_tpm_tpm_quote\n.type liber_channel_liber_tpm_tpm_quote,%function\nliber_channel_liber_tpm_tpm_quote:\nb liber_channel_impl_liber_tpm_tpm_quote\n.size liber_channel_liber_tpm_tpm_quote, . - liber_channel_liber_tpm_tpm_quote\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_tpm_tpm_quote,\"ax\",@progbits\n.globl liber_channel_liber_tpm_tpm_quote\n.type liber_channel_liber_tpm_tpm_quote,%function\nliber_channel_liber_tpm_tpm_quote:\ntail liber_channel_impl_liber_tpm_tpm_quote\n.size liber_channel_liber_tpm_tpm_quote, . - liber_channel_liber_tpm_tpm_quote\n");
