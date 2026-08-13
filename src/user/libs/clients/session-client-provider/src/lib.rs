// The ELF trampolines that make the session channel client a shared library rather than a copy in
// every program that links it: each `liber_channel_liber_session_*` symbol jumps to the generated
// `..._impl_...` implementation, so a tool imports the name and the encoder lives here once.

#![no_std]

use core::arch::global_asm;

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_session_session_job_list,\"ax\",@progbits\n.globl liber_channel_liber_session_session_job_list\n.type liber_channel_liber_session_session_job_list,@function\nliber_channel_liber_session_session_job_list:\njmp liber_channel_impl_liber_session_session_job_list\n.size liber_channel_liber_session_session_job_list, . - liber_channel_liber_session_session_job_list\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_session_session_job_list,\"ax\",@progbits\n.globl liber_channel_liber_session_session_job_list\n.type liber_channel_liber_session_session_job_list,%function\nliber_channel_liber_session_session_job_list:\nb liber_channel_impl_liber_session_session_job_list\n.size liber_channel_liber_session_session_job_list, . - liber_channel_liber_session_session_job_list\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_session_session_job_list,\"ax\",@progbits\n.globl liber_channel_liber_session_session_job_list\n.type liber_channel_liber_session_session_job_list,%function\nliber_channel_liber_session_session_job_list:\ntail liber_channel_impl_liber_session_session_job_list\n.size liber_channel_liber_session_session_job_list, . - liber_channel_liber_session_session_job_list\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_session_session_job_signal,\"ax\",@progbits\n.globl liber_channel_liber_session_session_job_signal\n.type liber_channel_liber_session_session_job_signal,@function\nliber_channel_liber_session_session_job_signal:\njmp liber_channel_impl_liber_session_session_job_signal\n.size liber_channel_liber_session_session_job_signal, . - liber_channel_liber_session_session_job_signal\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_session_session_job_signal,\"ax\",@progbits\n.globl liber_channel_liber_session_session_job_signal\n.type liber_channel_liber_session_session_job_signal,%function\nliber_channel_liber_session_session_job_signal:\nb liber_channel_impl_liber_session_session_job_signal\n.size liber_channel_liber_session_session_job_signal, . - liber_channel_liber_session_session_job_signal\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_session_session_job_signal,\"ax\",@progbits\n.globl liber_channel_liber_session_session_job_signal\n.type liber_channel_liber_session_session_job_signal,%function\nliber_channel_liber_session_session_job_signal:\ntail liber_channel_impl_liber_session_session_job_signal\n.size liber_channel_liber_session_session_job_signal, . - liber_channel_liber_session_session_job_signal\n");
