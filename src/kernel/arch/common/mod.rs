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
// The flattened-device-tree parser used to live here and is now the `fdt` CRATE: the
// loader needs the same reader (it takes the post-ExitBootServices console out of the
// tree rather than guessing an address), and nothing inside the kernel can be run on a
// host. Each backend's `dtb` shim still supplies phys_to_virt and the fallback scan
// window, which are the only arch-specific parts.
// `time`: the shared scheduler-tick rate (TICK_HZ) + the cycles->ns conversion each
// backend's cycle clock reports through.
// `rng`: the SplitMix64 mixer the arch random fallbacks share (no arch guarantees a
// hardware RNG on the bring-up core).

pub mod bootmem;
pub mod context;
pub mod fwcfg;
pub mod msi;
pub mod paging;
pub mod pci;
pub mod rng;
pub mod time;
