// architecture selection based on the compile target
//
// This module is the kernel's HARDWARE ABSTRACTION BOUNDARY (the HAL). The
// portable kernel reaches the machine ONLY through `arch::*` - there is no
// `cfg(target_arch)` outside this directory. Each backend below (`x86_64`,
// `aarch64`, `riscv64`) is one implementer of the same contract, selected by the
// compile target; the portable code never names an architecture.
//
// THE CONTRACT each backend must provide (the surface the portable kernel calls):
//   top-level:  init, init_interrupts, init_syscalls, init_tsc, init_bsp_percpu,
//               init_ap, enable_interrupts, disable_interrupts, interrupts_enabled,
//               idle_halt, halt_loop, reset, poweroff, cpu_brand, boot_profile,
//               exit_qemu (cfg(test))
//   paging:     PRESENT / WRITABLE / USER / NO_CACHE / NO_EXECUTE, map_page,
//               map_page_in, try_map_page, try_map_page_in, unmap_page,
//               unmap_page_in, translate,
//               new_address_space, free_address_space, user_access,
//               copy_to_user_page, enable_nx, enable_smap_smep, nx_enabled,
//               smap_enabled, smep_enabled, clac_on_entry, remove_bootstrap_identity
//   context:    switch_context, init_thread_stack, read_cr3, write_cr3
//               (read_cr3/write_cr3 name the active address-space token - CR3 on
//               x86, TTBR0 on aarch64, SATP on riscv64)
//   percpu:     PerCpu (cpu_id, lapic_id), allocate, init, this_cpu,
//               set_kernel_rsp, set_tss_rsp0_slot, set_rsp0, in_user_syscall
//   interrupts: IRQ_BASE, HandlerFn, register, bind, unbind, is_bound, is_bindable,
//               acquire_msi, bind_msi, eoi, irq_info, irq_info_len, init
//   apic:       local_id, eoi, send_wake_ipi, send_init, send_startup, ticks,
//               init, init_ap  (the interrupt controller + timer; GIC on aarch64,
//               PLIC/CLINT on riscv64 - keeps the `apic` name for now)
//   tsc:        now, init, hz, cycles_to_ns  (the fine cycle clock)
//   ioapic:     route, init, mask
//   serial:     SerialWriter, init, enable_rx_irq, enable_async, drain_tx,
//               flush_sync, write_bytes, read_byte
//   pci:        PciDevice / VirtioDevice / XhciDevice, scan,
//               scan_virtio, scan_xhci, set_intx_disabled, msix_enable
//   syscall:    init, invoke        usermode: enter, exit_to_kernel,
//               FAULT_PROBE_ADDR, program_*_bytes
//   apboot:     trampoline_len, install, set_stack   (SMP secondary bring-up)
//   rtc:        read_unix           random: fill
//
// THE CONTRACT IS THE x86_64 BOOT PATH, AND SAYING SO IS THE POINT OF THIS PARAGRAPH.
//
// The six `init_*` entries above are the hooks the bootloader-handoff `main::kmain` calls, and only
// x86_64 arrives through it. aarch64 enters at `aarch64::boot::aarch64_main` and riscv64 at
// `riscv64::boot::riscv64_main`; each brings up its own console, page tables, per-CPU register,
// interrupt controller, timer, syscall vector and secondary cores INLINE, and never calls
// `arch::init`, `arch::init_interrupts` or `arch::init_syscalls`. Those remain `todo!()` on both -
// seventeen such stubs in aarch64's `mod.rs`, twelve in riscv64's - and both targets run the whole
// test suite regardless.
//
// So a list of required symbols that two of three backends satisfy with `todo!()` is not yet a
// contract for boot; it is a contract for everything AFTER boot, which the same two backends do
// implement in full. That is a deliberate position rather than an unfinished one: bringing a
// machine up is the least portable thing a kernel does, and the shape of the hand-off differs
// (UEFI + a loader on x86_64, a device tree and firmware already in supervisor mode on the other
// two). What a fourth architecture would copy is therefore `boot.rs`, not `kmain`.
//
// What that costs, written down because it is the reason to revisit this: nothing type-checks the
// per-arch boot sequences against each other, so a step added to one - `remove_bootstrap_identity`,
// a percpu field, a reserved kernel window - is silently absent from the others until a test
// notices. The two candidates are a `BootSequence` trait each backend implements, or moving the
// portable half of the three bring-ups back into `kmain` and leaving only the machine-specific
// prologue per arch. Neither is scheduled; the position is recorded so the next person reads a
// decision instead of a gap.

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

// The human-readable name of the compile-target architecture, for the boot log.
#[cfg(target_arch = "x86_64")]
pub const NAME: &str = "x86_64";
#[cfg(target_arch = "aarch64")]
pub const NAME: &str = "aarch64";
#[cfg(target_arch = "riscv64")]
pub const NAME: &str = "riscv64";

#[cfg(test)]
mod tests;
