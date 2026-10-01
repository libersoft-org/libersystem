//! The trampoline that makes the generated input client reachable by name: one symbol for the one operation `gamepad`
//! uses, a tail jump to the `liber_channel_impl_*` the generated crate exports - the shape of every client provider.

#![no_std]

use core::arch::global_asm;

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_input_input_observe_gamepads,\"ax\",@progbits\n.globl liber_channel_liber_input_input_observe_gamepads\n.type liber_channel_liber_input_input_observe_gamepads,@function\nliber_channel_liber_input_input_observe_gamepads:\njmp liber_channel_impl_liber_input_input_observe_gamepads\n.size liber_channel_liber_input_input_observe_gamepads, . - liber_channel_liber_input_input_observe_gamepads\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_input_input_observe_gamepads,\"ax\",@progbits\n.globl liber_channel_liber_input_input_observe_gamepads\n.type liber_channel_liber_input_input_observe_gamepads,%function\nliber_channel_liber_input_input_observe_gamepads:\nb liber_channel_impl_liber_input_input_observe_gamepads\n.size liber_channel_liber_input_input_observe_gamepads, . - liber_channel_liber_input_input_observe_gamepads\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_input_input_observe_gamepads,\"ax\",@progbits\n.globl liber_channel_liber_input_input_observe_gamepads\n.type liber_channel_liber_input_input_observe_gamepads,%function\nliber_channel_liber_input_input_observe_gamepads:\ntail liber_channel_impl_liber_input_input_observe_gamepads\n.size liber_channel_liber_input_input_observe_gamepads, . - liber_channel_liber_input_input_observe_gamepads\n");
