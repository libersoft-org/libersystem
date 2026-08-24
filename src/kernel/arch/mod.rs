// architecture selection based on the compile target
//
// This module is the kernel's HARDWARE ABSTRACTION BOUNDARY (the HAL). The
// portable kernel reaches the machine ONLY through `arch::*` - there is no
// `cfg(target_arch)` outside this directory. Each backend below (`x86_64`,
// `aarch64`, `riscv64`) is one implementer of the same contract, selected by the
// compile target; the portable code never names an architecture.
//
// THE CONTRACT, IN THREE PARTS, because one list of required symbols was not true of any backend.
//
// (1) WHAT EVERY BACKEND PROVIDES, and what a fourth architecture owes in full. This is the surface
//     the portable kernel calls after the machine is up, and all three implement it:
//       top-level:  enable_interrupts, disable_interrupts, interrupts_enabled, idle_halt,
//                   halt_loop, reset, poweroff, cpu_brand, boot_profile, exit_qemu (cfg(test))
//       paging:     PRESENT / WRITABLE / USER / NO_CACHE / NO_EXECUTE, map_page, map_page_in,
//                   try_map_page, try_map_page_in, unmap_page, unmap_page_in, translate,
//                   new_address_space, free_address_space, user_access, copy_to_user_page,
//                   remove_bootstrap_identity
//       context:    switch_context, init_thread_stack, read_cr3, write_cr3 (read_cr3/write_cr3
//                   name the active address-space token - CR3 on x86, TTBR0 on aarch64, SATP on
//                   riscv64)
//       percpu:     PerCpu (cpu_id, lapic_id), allocate, init, this_cpu, set_kernel_rsp,
//                   set_stack_bounds, in_user_syscall
//       interrupts: IRQ_BASE, HandlerFn, register, bind, unbind, is_bound, is_bindable,
//                   acquire_msi_unique, bind_msi, eoi, irq_info, irq_info_len
//       apic:       local_id, send_wake_ipi, ticks   (the interrupt controller + timer; GIC on
//                   aarch64, PLIC/CLINT + IMSIC on riscv64 - keeps the `apic` name for now)
//       tsc:        now, init, hz, cycles_to_ns   (the fine cycle clock)
//       serial:     SerialWriter, init, enable_rx_irq, enable_async, drain_tx, flush_sync,
//                   write_bytes, read_byte
//       pci:        PciDevice / VirtioDevice / XhciDevice, scan, scan_virtio, scan_xhci,
//                   set_intx_disabled, msix_enable
//       syscall:    invoke (cfg(test))            usermode: enter, exit_to_kernel
//       rtc:        read_unix                     random:   fill
//
// (2) WHAT ONLY THE x86_64 BOOTLOADER HAND-OFF CALLS, AND ONLY x86_64 COMPILES.
//     `main::kmain` is the UEFI-loader entry and only x86_64 arrives through it; aarch64 enters at
//     `aarch64::boot::aarch64_main` and riscv64 at `riscv64::boot::riscv64_main`, and each brings up
//     its console, page tables, per-CPU register, interrupt controller, timer, syscall vector and
//     secondary cores INLINE. A fourth architecture writes its own `boot.rs` and owes NONE of these:
//       top-level:  init, init_interrupts, init_syscalls, init_tsc, init_bsp_percpu, init_ap
//       apic:       send_init, send_startup       apboot: cr3_is_reachable, install, set_stack
//       ioapic:     route, init, mask             syscall: init
//       interrupts: IRQ_BASE, HandlerFn, register (the x86 INTx registry; the other two are MSI)
//     THESE ARE NOT ENTRIES ON THE OTHER TWO BACKENDS AT ALL. They used to be, answered by twenty
//     `todo!()` bodies nothing could reach - and a body like that is indistinguishable, to a reader
//     and to a static scan, from an unfinished port. P02M0151 removed the requirement rather than the
//     symptom: `kmain`, the ACPI MADT walk, the INIT-SIPI-SIPI wake, the real-mode trampoline and
//     everything reached only from them are `#[cfg(target_arch = "x86_64")]`, so a port that does not
//     arrive that way defines none of it. `tools/check-arch-surface.sh` keeps it that way.
//
// (3) WHAT ONLY x86_64 HAS AT ALL, because the protection exists there and not elsewhere:
//       paging:     enable_nx, enable_smap_smep, nx_enabled, smap_enabled, smep_enabled
//       percpu:     set_tss_rsp0_slot
//     `clac_on_entry` is x86_64 and riscv64: SMAP's `clac` and SSTATUS.SUM answer the same question,
//     and aarch64's cortex-a72 is ARMv8.0 with no PAN, so `user_access` there is a passthrough that
//     says so at the call site. THAT ASYMMETRY IS A FACT ABOUT THE MACHINES, not an unfinished port:
//     a stub answering `false` for a protection the CPU does not have would be a contract entry that
//     lies, which is what these five used to be on the other two backends before they were removed.
//
// WHAT THIS COSTS, and it is smaller than it was. Nothing type-checks the three boot PROLOGUES
// against each other, so a machine-level step added to one is silently absent from the others until
// a test notices. The boot TAIL is no longer in that class: P02M0142 moved the part that carries
// policy - the recovery ladder, the crash-notify channel, the readiness wait, the idle hook that
// watches for a lost SystemManager, the shell hand-off - into `main::boot_userspace`, which all
// three entries call. What remains per-arch is the machine, which is the least portable thing a
// kernel does and the part a fourth architecture would genuinely have to write.

// Architecture-independent HAL helpers shared by every backend (compiled for all
// targets): the portable PCI enumeration each arch's `pci` shim builds on.
pub mod common;

#[cfg(target_arch = "x86_64")]
pub mod x86_64;
#[cfg(target_arch = "x86_64")]
pub use self::x86_64::*;

#[cfg(target_arch = "aarch64")]
pub mod aarch64;
#[cfg(target_arch = "aarch64")]
pub use self::aarch64::*;

#[cfg(target_arch = "riscv64")]
pub mod riscv64;
#[cfg(target_arch = "riscv64")]
pub use self::riscv64::*;

// The human-readable name of the compile-target architecture, for the boot log. Only the boot log
// asks, and the test build has no boot.
#[cfg(all(not(test), target_arch = "x86_64"))]
pub const NAME: &str = "x86_64";
#[cfg(all(not(test), target_arch = "aarch64"))]
pub const NAME: &str = "aarch64";
#[cfg(all(not(test), target_arch = "riscv64"))]
pub const NAME: &str = "riscv64";

#[cfg(test)]
mod tests;
