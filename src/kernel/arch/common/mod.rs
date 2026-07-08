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

pub mod pci;
