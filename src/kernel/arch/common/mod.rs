// Portable, architecture-independent building blocks shared by every arch backend.
//
// Code under `arch/` is normally per-architecture, selected by the compile target.
// This module is the exception: it holds logic that is genuinely the same on every
// architecture (bus standards, table walks) but still belongs inside the HAL because
// it drives the machine. Each backend wires its tiny arch-specific primitives into
// these generic routines, so a new architecture reuses them instead of copying.
//
// `pci`: PCI / PCIe enumeration - only the config-space access mechanism is
// arch-specific (x86 I/O ports vs ECAM MMIO); the device tables, capability walk,
// BAR decoding and MSI-X resolution are shared.
// `paging`: the portable page-table permission flags the `arch::paging` contract
// exposes (each backend maps them onto its real hardware encoding).
// `msi`: the per-device MSI-X slot registry (bind / acquire / dispatch bookkeeping),
// shared by every interrupt-controller backend.
// `context`: the portable thread bootstrap each backend's context-switch trampoline
// lands in.
// `dtb`: the flattened-device-tree (FDT) parser shared by the device-tree-booted
// backends (aarch64 now, riscv64 next); only phys_to_virt + the fallback scan window
// are arch-specific.

pub mod context;
pub mod dtb;
pub mod msi;
pub mod paging;
pub mod pci;
