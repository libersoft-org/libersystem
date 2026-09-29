//! The trampolines that make the generated BMC client reachable by name.
//!
//! One symbol per operation `bmc` uses, each a tail jump to the `liber_channel_impl_*` the generated crate exports - the
//! same shape as every other client provider, for the same reason: a consumer links against an address with a stable
//! name, not against a function the linker may inline or reorder.

#![no_std]

use core::arch::global_asm;

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bmc_bmc_list,\"ax\",@progbits\n.globl liber_channel_liber_bmc_bmc_list\n.type liber_channel_liber_bmc_bmc_list,@function\nliber_channel_liber_bmc_bmc_list:\njmp liber_channel_impl_liber_bmc_bmc_list\n.size liber_channel_liber_bmc_bmc_list, . - liber_channel_liber_bmc_bmc_list\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bmc_bmc_list,\"ax\",@progbits\n.globl liber_channel_liber_bmc_bmc_list\n.type liber_channel_liber_bmc_bmc_list,%function\nliber_channel_liber_bmc_bmc_list:\nb liber_channel_impl_liber_bmc_bmc_list\n.size liber_channel_liber_bmc_bmc_list, . - liber_channel_liber_bmc_bmc_list\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bmc_bmc_list,\"ax\",@progbits\n.globl liber_channel_liber_bmc_bmc_list\n.type liber_channel_liber_bmc_bmc_list,%function\nliber_channel_liber_bmc_bmc_list:\ntail liber_channel_impl_liber_bmc_bmc_list\n.size liber_channel_liber_bmc_bmc_list, . - liber_channel_liber_bmc_bmc_list\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bmc_bmc_sensors,\"ax\",@progbits\n.globl liber_channel_liber_bmc_bmc_sensors\n.type liber_channel_liber_bmc_bmc_sensors,@function\nliber_channel_liber_bmc_bmc_sensors:\njmp liber_channel_impl_liber_bmc_bmc_sensors\n.size liber_channel_liber_bmc_bmc_sensors, . - liber_channel_liber_bmc_bmc_sensors\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bmc_bmc_sensors,\"ax\",@progbits\n.globl liber_channel_liber_bmc_bmc_sensors\n.type liber_channel_liber_bmc_bmc_sensors,%function\nliber_channel_liber_bmc_bmc_sensors:\nb liber_channel_impl_liber_bmc_bmc_sensors\n.size liber_channel_liber_bmc_bmc_sensors, . - liber_channel_liber_bmc_bmc_sensors\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bmc_bmc_sensors,\"ax\",@progbits\n.globl liber_channel_liber_bmc_bmc_sensors\n.type liber_channel_liber_bmc_bmc_sensors,%function\nliber_channel_liber_bmc_bmc_sensors:\ntail liber_channel_impl_liber_bmc_bmc_sensors\n.size liber_channel_liber_bmc_bmc_sensors, . - liber_channel_liber_bmc_bmc_sensors\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bmc_bmc_sel_info,\"ax\",@progbits\n.globl liber_channel_liber_bmc_bmc_sel_info\n.type liber_channel_liber_bmc_bmc_sel_info,@function\nliber_channel_liber_bmc_bmc_sel_info:\njmp liber_channel_impl_liber_bmc_bmc_sel_info\n.size liber_channel_liber_bmc_bmc_sel_info, . - liber_channel_liber_bmc_bmc_sel_info\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bmc_bmc_sel_info,\"ax\",@progbits\n.globl liber_channel_liber_bmc_bmc_sel_info\n.type liber_channel_liber_bmc_bmc_sel_info,%function\nliber_channel_liber_bmc_bmc_sel_info:\nb liber_channel_impl_liber_bmc_bmc_sel_info\n.size liber_channel_liber_bmc_bmc_sel_info, . - liber_channel_liber_bmc_bmc_sel_info\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bmc_bmc_sel_info,\"ax\",@progbits\n.globl liber_channel_liber_bmc_bmc_sel_info\n.type liber_channel_liber_bmc_bmc_sel_info,%function\nliber_channel_liber_bmc_bmc_sel_info:\ntail liber_channel_impl_liber_bmc_bmc_sel_info\n.size liber_channel_liber_bmc_bmc_sel_info, . - liber_channel_liber_bmc_bmc_sel_info\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bmc_bmc_sel_page,\"ax\",@progbits\n.globl liber_channel_liber_bmc_bmc_sel_page\n.type liber_channel_liber_bmc_bmc_sel_page,@function\nliber_channel_liber_bmc_bmc_sel_page:\njmp liber_channel_impl_liber_bmc_bmc_sel_page\n.size liber_channel_liber_bmc_bmc_sel_page, . - liber_channel_liber_bmc_bmc_sel_page\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bmc_bmc_sel_page,\"ax\",@progbits\n.globl liber_channel_liber_bmc_bmc_sel_page\n.type liber_channel_liber_bmc_bmc_sel_page,%function\nliber_channel_liber_bmc_bmc_sel_page:\nb liber_channel_impl_liber_bmc_bmc_sel_page\n.size liber_channel_liber_bmc_bmc_sel_page, . - liber_channel_liber_bmc_bmc_sel_page\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bmc_bmc_sel_page,\"ax\",@progbits\n.globl liber_channel_liber_bmc_bmc_sel_page\n.type liber_channel_liber_bmc_bmc_sel_page,%function\nliber_channel_liber_bmc_bmc_sel_page:\ntail liber_channel_impl_liber_bmc_bmc_sel_page\n.size liber_channel_liber_bmc_bmc_sel_page, . - liber_channel_liber_bmc_bmc_sel_page\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bmc_bmc_fru,\"ax\",@progbits\n.globl liber_channel_liber_bmc_bmc_fru\n.type liber_channel_liber_bmc_bmc_fru,@function\nliber_channel_liber_bmc_bmc_fru:\njmp liber_channel_impl_liber_bmc_bmc_fru\n.size liber_channel_liber_bmc_bmc_fru, . - liber_channel_liber_bmc_bmc_fru\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bmc_bmc_fru,\"ax\",@progbits\n.globl liber_channel_liber_bmc_bmc_fru\n.type liber_channel_liber_bmc_bmc_fru,%function\nliber_channel_liber_bmc_bmc_fru:\nb liber_channel_impl_liber_bmc_bmc_fru\n.size liber_channel_liber_bmc_bmc_fru, . - liber_channel_liber_bmc_bmc_fru\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bmc_bmc_fru,\"ax\",@progbits\n.globl liber_channel_liber_bmc_bmc_fru\n.type liber_channel_liber_bmc_bmc_fru,%function\nliber_channel_liber_bmc_bmc_fru:\ntail liber_channel_impl_liber_bmc_bmc_fru\n.size liber_channel_liber_bmc_bmc_fru, . - liber_channel_liber_bmc_bmc_fru\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bmc_bmc_chassis,\"ax\",@progbits\n.globl liber_channel_liber_bmc_bmc_chassis\n.type liber_channel_liber_bmc_bmc_chassis,@function\nliber_channel_liber_bmc_bmc_chassis:\njmp liber_channel_impl_liber_bmc_bmc_chassis\n.size liber_channel_liber_bmc_bmc_chassis, . - liber_channel_liber_bmc_bmc_chassis\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bmc_bmc_chassis,\"ax\",@progbits\n.globl liber_channel_liber_bmc_bmc_chassis\n.type liber_channel_liber_bmc_bmc_chassis,%function\nliber_channel_liber_bmc_bmc_chassis:\nb liber_channel_impl_liber_bmc_bmc_chassis\n.size liber_channel_liber_bmc_bmc_chassis, . - liber_channel_liber_bmc_bmc_chassis\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bmc_bmc_chassis,\"ax\",@progbits\n.globl liber_channel_liber_bmc_bmc_chassis\n.type liber_channel_liber_bmc_bmc_chassis,%function\nliber_channel_liber_bmc_bmc_chassis:\ntail liber_channel_impl_liber_bmc_bmc_chassis\n.size liber_channel_liber_bmc_bmc_chassis, . - liber_channel_liber_bmc_bmc_chassis\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bmc_bmc_identify,\"ax\",@progbits\n.globl liber_channel_liber_bmc_bmc_identify\n.type liber_channel_liber_bmc_bmc_identify,@function\nliber_channel_liber_bmc_bmc_identify:\njmp liber_channel_impl_liber_bmc_bmc_identify\n.size liber_channel_liber_bmc_bmc_identify, . - liber_channel_liber_bmc_bmc_identify\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bmc_bmc_identify,\"ax\",@progbits\n.globl liber_channel_liber_bmc_bmc_identify\n.type liber_channel_liber_bmc_bmc_identify,%function\nliber_channel_liber_bmc_bmc_identify:\nb liber_channel_impl_liber_bmc_bmc_identify\n.size liber_channel_liber_bmc_bmc_identify, . - liber_channel_liber_bmc_bmc_identify\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bmc_bmc_identify,\"ax\",@progbits\n.globl liber_channel_liber_bmc_bmc_identify\n.type liber_channel_liber_bmc_bmc_identify,%function\nliber_channel_liber_bmc_bmc_identify:\ntail liber_channel_impl_liber_bmc_bmc_identify\n.size liber_channel_liber_bmc_bmc_identify, . - liber_channel_liber_bmc_bmc_identify\n");

#[cfg(target_arch = "x86_64")]
global_asm!(".section .text.liber_channel_liber_bmc_bmc_lan,\"ax\",@progbits\n.globl liber_channel_liber_bmc_bmc_lan\n.type liber_channel_liber_bmc_bmc_lan,@function\nliber_channel_liber_bmc_bmc_lan:\njmp liber_channel_impl_liber_bmc_bmc_lan\n.size liber_channel_liber_bmc_bmc_lan, . - liber_channel_liber_bmc_bmc_lan\n");

#[cfg(target_arch = "aarch64")]
global_asm!(".section .text.liber_channel_liber_bmc_bmc_lan,\"ax\",@progbits\n.globl liber_channel_liber_bmc_bmc_lan\n.type liber_channel_liber_bmc_bmc_lan,%function\nliber_channel_liber_bmc_bmc_lan:\nb liber_channel_impl_liber_bmc_bmc_lan\n.size liber_channel_liber_bmc_bmc_lan, . - liber_channel_liber_bmc_bmc_lan\n");

#[cfg(target_arch = "riscv64")]
global_asm!(".section .text.liber_channel_liber_bmc_bmc_lan,\"ax\",@progbits\n.globl liber_channel_liber_bmc_bmc_lan\n.type liber_channel_liber_bmc_bmc_lan,%function\nliber_channel_liber_bmc_bmc_lan:\ntail liber_channel_impl_liber_bmc_bmc_lan\n.size liber_channel_liber_bmc_bmc_lan, . - liber_channel_liber_bmc_bmc_lan\n");
