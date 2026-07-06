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
	pub _pad1: u32,

	// ACPI RSDP physical address (0 if the firmware exposed none). The kernel
	// parses the MADT from here to enumerate LAPICs and wake the APs itself.
	pub rsdp: u64,

	// A reserved page of physical memory below 1 MiB for the AP real-mode
	// bring-up trampoline (INIT-SIPI-SIPI targets a page-aligned vector < 1 MiB).
	pub smp_trampoline: u64,
}
