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
//               idle_halt, halt_loop, reset, poweroff, exit_qemu (cfg(test))
//   paging:     PRESENT / WRITABLE / USER / NO_CACHE / NO_EXECUTE, map_page,
//               map_page_in, unmap_page, unmap_pages, unmap_page_in, translate,
//               new_address_space, free_address_space, user_access,
//               copy_to_user_page, enable_nx, enable_smap_smep, nx_enabled,
//               smap_enabled, smep_enabled, clac_on_entry, remove_bootstrap_identity
//   context:    switch_context, init_thread_stack, read_cr3, write_cr3
//               (read_cr3/write_cr3 name the active address-space token - CR3 on
//               x86, TTBR0 on aarch64, SATP on riscv64)
//   percpu:     PerCpu (cpu_id, lapic_id), allocate, init, this_cpu,
//               set_kernel_rsp, set_tss_rsp0_slot, set_rsp0, in_user_syscall
//   interrupts: IRQ_BASE, HandlerFn, register, bind, unbind, is_bound, is_bindable,
//               acquire_msi, bind_msi, irq_info, irq_info_len, init
//   apic:       local_id, eoi, send_wake_ipi, send_init, send_startup, ticks,
//               init, init_ap  (the interrupt controller + timer; GIC on aarch64,
//               PLIC/CLINT on riscv64 - keeps the `apic` name for now)
//   tsc:        now, init, hz, cycles_to_ns  (the fine cycle clock)
//   ioapic:     route, init, mask
//   serial:     SerialWriter, init, enable_rx_irq, enable_async, drain_tx,
//               flush_sync, write_bytes, read_byte
//   pci:        PciDevice / VirtioDevice / XhciDevice / VirtioCap, scan,
//               scan_virtio, scan_xhci, set_intx_disabled, msix_enable
//   syscall:    init, invoke        usermode: enter, exit_to_kernel,
//               FAULT_PROBE_ADDR, program_*_bytes
//   apboot:     trampoline_len, install, set_stack   (SMP secondary bring-up)
//   rtc:        read_unix           random: fill
//
// x86_64 is the reference implementation; aarch64 / riscv64 are compiling stubs
// (M115) that M116 / M117 fill in.

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
