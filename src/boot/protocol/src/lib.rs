// LiberSystem boot protocol: the typed hand-off from the UEFI loader to the kernel.
//
// The loader builds one `BootInfo` in memory it has reserved (LoaderData), maps
// all of physical RAM at a fixed higher-half offset (the HHDM), loads the kernel
// ELF and its packages, then enters the kernel with a pointer to this struct.
// Every pointer in `BootInfo` is an HHDM virtual address, so the kernel can read
// them straight away on the loader's page tables.
//
// This crate is `no_std` and dependency-free so both the loader (an
// `x86_64-unknown-uefi` PE binary) and the kernel can share the exact same
// layout. The structs are `#[repr(C)]` and the layout is frozen by MAGIC +
// VERSION; bump VERSION on any incompatible change and both sides will refuse to
// boot on a mismatch rather than read a stale layout.

#![no_std]

#[cfg(test)]
extern crate std;

// The shared read-only ELF64 reader, used by both the loader (to load the kernel)
// and the kernel (to load userspace programs).
pub mod elf;

pub mod boot_manifest;
// The signed boot manifest, version 2: the same question the text one answers, plus who says so.
pub mod manifest;
pub mod sha256;

// The written rule for whether a candidate library may replace an installed provider in a
// running system. It reads only the two images, so the guest, the build system and any
// audit tool reach the same verdict from the same bytes.
pub mod compat;

// Identifies a valid `BootInfo`. The loader writes it; the kernel checks it.
// Spells "LBSPROT2" (LiberSystem boot protocol, revision 2 - the UEFI loader).
pub const MAGIC: u64 = 0x4c42_5350_524f_5432;

// Layout revision. Bump on any incompatible change to the structs below.
pub const VERSION: u32 = 1;

// Region kinds reported in `MemRegion::kind`. These mirror the kernel ABI's
// stable MEMMAP_* codes (abi::MEMMAP_*) so the loader hands the kernel values it
// can retain verbatim for `lsmem` without a second translation table.
pub const MEM_USABLE: u32 = 0;
pub const MEM_RESERVED: u32 = 1;
pub const MEM_ACPI_RECLAIMABLE: u32 = 2;
pub const MEM_ACPI_NVS: u32 = 3;
pub const MEM_BAD: u32 = 4;
pub const MEM_BOOTLOADER: u32 = 5;
pub const MEM_KERNEL: u32 = 6;
pub const MEM_FRAMEBUFFER: u32 = 7;
// Loader memory whose life ended at the handoff: the final memory-map buffer and anything else the
// loader could not free before `ExitBootServices` and nothing owns after it (LDR-012). Seeded as
// usable, unlike `MEM_BOOTLOADER`, which holds the kernel image, the packages, the page tables,
// `BootInfo`, the boot stack and the AP trampoline.
pub const MEM_BOOTLOADER_RECLAIMABLE: u32 = 8;

// One physical memory-map region: its physical base, byte length, and kind (a
// MEM_* code above). The loader sorts these ascending by base and coalesces
// adjacent runs of the same kind, so the kernel's frame allocator can seed its
// free list straight from the MEM_USABLE runs.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MemRegion {
	pub base: u64,
	pub length: u64,
	pub kind: u32,
	pub _pad: u32,
}

// The linear framebuffer the loader obtained from the UEFI Graphics Output
// Protocol. `addr` is the framebuffer's HHDM virtual address (phys + hhdm_offset,
// mapped uncacheable); the channel shifts/sizes describe the pixel format. Present
// only when `BootInfo::fb_present` is non-zero (headless boots have no GOP).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Framebuffer {
	pub addr: u64,
	pub width: u32,
	pub height: u32,
	pub pitch: u32,
	pub bpp: u32,
	pub red_shift: u8,
	pub red_size: u8,
	pub green_shift: u8,
	pub green_size: u8,
	pub blue_shift: u8,
	pub blue_size: u8,
	pub _pad: [u8; 2],
}

// A file the loader read from the boot medium into memory for the kernel: its
// HHDM virtual address, byte length, and a short NUL-padded name (e.g. "init.pkg"
// or "volume.pkg"). The kernel matches on the name to find each package.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Module {
	pub addr: u64,
	pub size: u64,
	pub name: [u8; 32],
}

// PSCI conduits - see `BootInfo::psci_conduit`.
//
// NOTHING ANSWERS. Either the architecture has no PSCI at all (x86), or whoever would have answered
// is gone - which is what dropping from EL2 to EL1 does when the firmware's EL2 was the only thing
// below.
pub const PSCI_NONE: u32 = 0;
// `hvc #0`: the conduit for a kernel running at EL1 under something at EL2 that serves it - QEMU's
// `virt` without `virtualization=on`, and any hypervisor.
pub const PSCI_HVC: u32 = 1;
// `smc #0`: the conduit for a machine with secure firmware at EL3. Carried so the field can express
// it; no path in this tree sets it yet, and the kernel refuses it by name rather than guessing.
pub const PSCI_SMC: u32 = 2;

// The root hand-off structure. The loader fills one of these and passes its
// address to the kernel entry point in `rdi` (SysV C ABI first argument).
#[repr(C)]
pub struct BootInfo {
	// MAGIC / VERSION guard: the kernel refuses to boot on a mismatch.
	pub magic: u64,
	pub version: u32,
	pub _pad0: u32,

	// virt = phys + hhdm_offset for all physical memory (the higher-half direct map).
	pub hhdm_offset: u64,

	// Physical memory map: `memmap_len` `MemRegion`s at HHDM virtual `memmap`.
	pub memmap: u64,
	pub memmap_len: u64,

	// Loaded packages: `modules_len` `Module`s at HHDM virtual `modules`.
	pub modules: u64,
	pub modules_len: u64,

	// The boot framebuffer; valid only when `fb_present` is non-zero.
	pub framebuffer: Framebuffer,
	pub fb_present: u32,

	// WHICH PSCI CONDUIT, IF ANY, ANSWERS THIS KERNEL - `PSCI_NONE`, `PSCI_HVC` or `PSCI_SMC`.
	//
	// This was `_pad1`, and the field it became exists because the loader's aarch64 EL2 branch was
	// run for the first time. PSCI is not a property of the architecture: it is a service provided
	// by whatever runs BELOW the kernel, and dropping an exception level changes who that is. QEMU's
	// `virt` implements PSCI at HVC for a guest that starts at EL1; with `virtualization=on` the
	// guest owns EL2, so an `hvc` from EL1 lands in the guest's own EL2 vectors - the firmware's,
	// which outlive `ExitBootServices` - and nothing below answers at all. The kernel hardcoded HVC
	// and faulted bringing up its secondaries, fifteen lines into an otherwise working boot.
	//
	// So the entity that decided the exception level says what it left behind. `PSCI_NONE` is the
	// honest answer for a kernel that must then run on one core, and it is x86's answer too.
	//
	// The struct's LAYOUT is unchanged: this is the padding word that was already here, named.
	pub psci_conduit: u32,

	// ACPI RSDP physical address (0 if the firmware exposed none). The kernel
	// parses the MADT from here to enumerate LAPICs and wake the APs itself.
	pub rsdp: u64,

	// A reserved page of physical memory below 1 MiB for the AP real-mode
	// bring-up trampoline (INIT-SIPI-SIPI targets a page-aligned vector < 1 MiB).
	pub smp_trampoline: u64,

	// Physical address of the flattened device tree (0 on x86, which uses ACPI). The
	// device-tree architectures (aarch64, riscv64) enter their kernel's boot stub with
	// the DTB pointer where QEMU's `-kernel` load would put it (x0 / a1); when the UEFI
	// loader hands a BootInfo there instead, it carries the DTB pointer here so the
	// kernel still finds its RAM / CPU / device inventory. On those arches
	// `framebuffer.addr` is the framebuffer's PHYSICAL base (the loader builds no page
	// tables, so the kernel maps it through its own direct map), unlike x86 where
	// `framebuffer.addr` is an HHDM virtual address the loader already mapped.
	pub dtb: u64,
}
