// QEMU fw-cfg (MMIO) + the ramfb early framebuffer - shared by the device-tree-booted
// backends (aarch64, riscv64).
//
// QEMU's `virt` machine has no VGA, so the kernel gets no linear framebuffer from
// firmware the way x86 does from `virtio-vga`. `-device virtio-gpu-pci,ramfb=on`
// closes that gap: the SAME device exposes a simple "ramfb" framebuffer the guest
// programs over fw-cfg (used by the kernel for the early boot log) AND the virtio-gpu
// the userspace driver later drives - one display head, a clean handoff, exactly the
// x86 virtio-vga story.
//
// This module speaks the fw-cfg MMIO DMA interface: it walks the file directory to
// find `etc/ramfb`, allocates a framebuffer from the physical frame pool, and programs
// ramfb to scan it out. Everything - the fw-cfg MMIO registers (below RAM, but the
// kernel's high direct map reaches them) and the guest-RAM DMA buffers - is touched
// through the backend's `phys_to_virt`. QEMU-only: the MMIO is reached through a
// normal-memory direct-map mapping, which is fine under QEMU's IO dispatch.

use core::ptr::{read_volatile, write_volatile};

// fw-cfg MMIO register offsets (QEMU "qemu,fw-cfg-mmio").
const REG_SELECTOR: u64 = 8; // u16, big-endian
const REG_DATA: u64 = 0; // the selected entry, read a byte at a time
const REG_DMA: u64 = 16; // u64, big-endian: writing it triggers a DMA

// fw-cfg well-known selectors.
const SEL_SIGNATURE: u16 = 0x0000; // reads "QEMU"
const SEL_FILE_DIR: u16 = 0x0019; // the file directory

// One directory entry: big-endian size and selector, two reserved bytes, then a NUL-padded name.
const DIR_ENTRY_BYTES: usize = 64;
const DIR_NAME_OFFSET: usize = 8;
const MAX_DIR_ENTRIES: u32 = 256;

// FWCfgDmaAccess.control bits (the whole struct is big-endian in guest memory).
const DMA_ERROR: u32 = 0x01;
const DMA_READ: u32 = 0x02;
const DMA_SELECT: u32 = 0x08; // control >> 16 = selector to (re)select
const DMA_WRITE: u32 = 0x10;

// DRM_FORMAT_XRGB8888 - a 32-bit pixel `0x00RRGGBB` (byte order B,G,R,X in memory).
const FOURCC_XRGB8888: u32 = 0x3432_5258; // 'X','R','2','4'

// The ramfb framebuffer this module set up, for the caller to wire into the console.
#[derive(Clone, Copy)]
pub struct RamFb {
	pub phys: u64,   // physical base (draw through phys_to_virt)
	pub width: u32,  // pixels
	pub height: u32, // pixels
	pub stride: u32, // bytes per row
}

// One fw-cfg session: the MMIO base plus the backend's direct-map accessor.
struct FwCfg {
	base: u64,
	p2v: fn(u64) -> u64,
}

impl FwCfg {
	// Select an entry. The selector register is big-endian on the MMIO interface, unlike the
	// x86 port interface where it is little-endian.
	unsafe fn select(&self, key: u16) {
		unsafe { write_volatile(((self.p2v)(self.base + REG_SELECTOR)) as *mut u16, key.to_be()) };
	}

	// Read the selected entry a byte at a time. The data register is one shared cursor, so the
	// caller reads a selection through in order.
	unsafe fn read_bytes(&self, out: &mut [u8]) {
		for byte in out.iter_mut() {
			*byte = unsafe { read_volatile(((self.p2v)(self.base + REG_DATA)) as *const u8) };
		}
	}

	// Read a big-endian u32 from guest physical `pa` (a DMA buffer field).
	unsafe fn be32_at(&self, pa: u64) -> u32 {
		unsafe { u32::from_be(read_volatile((self.p2v)(pa) as *const u32)) }
	}

	// Store `v` big-endian at guest physical `pa`.
	unsafe fn put_be32(&self, pa: u64, v: u32) {
		unsafe { write_volatile((self.p2v)(pa) as *mut u32, v.to_be()) }
	}
	unsafe fn put_be64(&self, pa: u64, v: u64) {
		unsafe { write_volatile((self.p2v)(pa) as *mut u64, v.to_be()) }
	}

	// Run one fw-cfg DMA: place a FWCfgDmaAccess { control, length, address } at
	// `dma_pa`, kick the DMA register, and spin until QEMU clears control. Returns
	// false on the ERROR bit. `dma_pa` and `buf_pa` are guest physical addresses.
	unsafe fn dma(&self, dma_pa: u64, control: u32, length: u32, buf_pa: u64) -> bool {
		unsafe {
			self.put_be32(dma_pa, control);
			self.put_be32(dma_pa + 4, length);
			self.put_be64(dma_pa + 8, buf_pa);
			// Kick: the DMA register is big-endian; a single 64-bit BE write of the
			// struct's physical address triggers the (synchronous, under QEMU) transfer.
			write_volatile((self.p2v)(self.base + REG_DMA) as *mut u64, dma_pa.to_be());
			// QEMU processes the DMA in the MMIO write handler, so control is already
			// updated; poll a bounded number of times regardless.
			for _ in 0..1_000_000 {
				let ctl = self.be32_at(dma_pa);
				if ctl & DMA_ERROR != 0 {
					return false;
				}
				if ctl == 0 {
					return true;
				}
				core::hint::spin_loop();
			}
			false
		}
	}
}

// Bring up the ramfb framebuffer at the requested geometry over fw-cfg at `fwcfg_base`,
// reading/writing MMIO and guest RAM through `p2v` (the backend's phys_to_virt). Returns
// the framebuffer on success, or None when there is no fw-cfg / no ramfb file / the
// allocation fails - in which case the boot stays serial-only.
// Copy the fw-cfg file `name` into `out`, returning how many bytes were written, or None when
// the device is absent, foreign, or has no such file.
//
// This is the selector-and-data path rather than the DMA path the ramfb setup above uses, and
// deliberately so: it runs before the kernel has memory to hand out, so it cannot allocate the
// DMA descriptor or the destination buffer that path needs. Reads advance one shared cursor, so
// a selection has to be consumed in order.
//
// Nothing is trusted before the signature matches. An absent device reads back whatever the bus
// floats, which is exactly the case that must report no file rather than a plausible one.
pub fn read_file(fwcfg_base: u64, name: &[u8], out: &mut [u8], p2v: fn(u64) -> u64) -> Option<usize> {
	if fwcfg_base == 0 {
		return None;
	}
	let fw = FwCfg { base: fwcfg_base, p2v };
	let mut signature = [0u8; 4];
	unsafe {
		fw.select(SEL_SIGNATURE);
		fw.read_bytes(&mut signature);
	}
	if &signature != b"QEMU" {
		return None;
	}
	let mut count = [0u8; 4];
	unsafe {
		fw.select(SEL_FILE_DIR);
		fw.read_bytes(&mut count);
	}
	// A directory this long means the signature matched by accident; stop rather than walk a
	// length the device never wrote.
	let count = u32::from_be_bytes(count).min(MAX_DIR_ENTRIES);
	for _ in 0..count {
		let mut entry = [0u8; DIR_ENTRY_BYTES];
		unsafe { fw.read_bytes(&mut entry) };
		let size = u32::from_be_bytes([entry[0], entry[1], entry[2], entry[3]]) as usize;
		let selector = u16::from_be_bytes([entry[4], entry[5]]);
		let raw = &entry[DIR_NAME_OFFSET..];
		let end = raw.iter().position(|byte| *byte == 0).unwrap_or(raw.len());
		if &raw[..end] != name {
			continue;
		}
		let len = size.min(out.len());
		unsafe {
			fw.select(selector);
			fw.read_bytes(&mut out[..len]);
		}
		return Some(len);
	}
	None
}

pub fn setup_ramfb(fwcfg_base: u64, width: u32, height: u32, p2v: fn(u64) -> u64) -> Option<RamFb> {
	if fwcfg_base == 0 {
		return None;
	}
	let fw = FwCfg { base: fwcfg_base, p2v };

	// A scratch region for the DMA descriptor + the directory / config buffers, taken
	// from the frame pool so its physical address is known (a heap buffer's is not).
	const SCRATCH_PAGES: usize = 8; // 32 KiB - the DMA struct + a generous file dir
	let scratch = crate::mem::frame::allocate_contiguous(SCRATCH_PAGES)?;
	let dma_pa = scratch; // FWCfgDmaAccess (16 bytes)
	let buf_pa = scratch + 64; // data buffer
	let buf_cap = (SCRATCH_PAGES as u64) * 4096 - 64;

	let result = unsafe { probe_and_program(&fw, dma_pa, buf_pa, buf_cap, width, height) };

	// The scratch pages are only needed during setup.
	// SAFETY: the contiguous scratch span allocated for this setup and used by nothing else once
	// `probe_and_program` has returned.
	unsafe { release_span(scratch, SCRATCH_PAGES) };
	result
}

// The unsafe core of setup_ramfb: verify the signature, walk the file directory for
// `etc/ramfb`, allocate + zero the framebuffer, and write the ramfb config.
unsafe fn probe_and_program(fw: &FwCfg, dma_pa: u64, buf_pa: u64, buf_cap: u64, width: u32, height: u32) -> Option<RamFb> {
	unsafe {
		// Signature: select 0x0000 reads the ASCII "QEMU". Guards against programming a
		// machine with no fw-cfg (or a stale base).
		if !fw.dma(dma_pa, (SEL_SIGNATURE as u32) << 16 | DMA_SELECT | DMA_READ, 4, buf_pa) {
			return None;
		}
		let sig = [
			read_volatile((fw.p2v)(buf_pa) as *const u8),
			read_volatile((fw.p2v)(buf_pa + 1) as *const u8),
			read_volatile((fw.p2v)(buf_pa + 2) as *const u8),
			read_volatile((fw.p2v)(buf_pa + 3) as *const u8),
		];
		if &sig != b"QEMU" {
			return None;
		}

		// File directory: select it and read the entry count (a big-endian u32), then
		// stream the entries. Each entry is { size: be32, select: be16, reserved: be16,
		// name: [u8; 56] } = 64 bytes.
		if !fw.dma(dma_pa, (SEL_FILE_DIR as u32) << 16 | DMA_SELECT | DMA_READ, 4, buf_pa) {
			return None;
		}
		let count = fw.be32_at(buf_pa);
		let entries = (count as u64).min(buf_cap / 64);
		if entries == 0 || !fw.dma(dma_pa, DMA_READ, (entries * 64) as u32, buf_pa) {
			return None;
		}

		// Find `etc/ramfb` and take its selector.
		let mut ramfb_sel: Option<u16> = None;
		for i in 0..entries {
			let e = buf_pa + i * 64;
			let sel = ((read_volatile((fw.p2v)(e + 4) as *const u8) as u16) << 8) | read_volatile((fw.p2v)(e + 5) as *const u8) as u16;
			let name = e + 8;
			if name_eq(fw, name, b"etc/ramfb") {
				ramfb_sel = Some(sel);
				break;
			}
		}
		let ramfb_sel = ramfb_sel?;

		// Allocate the framebuffer (physically contiguous - QEMU scans it out as one
		// linear region) and zero it so the screen starts blank.
		let stride = width * 4;
		let fb_bytes = stride as u64 * height as u64;
		let fb_pages = crate::mem::frame::pages_for(fb_bytes as usize);
		let fb_phys = crate::mem::frame::allocate_contiguous(fb_pages)?;
		core::ptr::write_bytes((fw.p2v)(fb_phys) as *mut u8, 0, (fb_pages * 4096) as usize);

		// Program ramfb: the RAMFBCfg { addr: be64, fourcc: be32, flags: be32, width:
		// be32, height: be32, stride: be32 } (28 bytes, big-endian) written to the file.
		fw.put_be64(buf_pa, fb_phys);
		fw.put_be32(buf_pa + 8, FOURCC_XRGB8888);
		fw.put_be32(buf_pa + 12, 0); // flags
		fw.put_be32(buf_pa + 16, width);
		fw.put_be32(buf_pa + 20, height);
		fw.put_be32(buf_pa + 24, stride);
		if !fw.dma(dma_pa, (ramfb_sel as u32) << 16 | DMA_SELECT | DMA_WRITE, 28, buf_pa) {
			// THE WHOLE SPAN, NOT ITS FIRST PAGE (KERN-ARCH-022).
			//
			// `deallocate` releases exactly one frame - the comment beside it said "the run stays
			// contiguous", which is true of the memory and says nothing about how much of it comes
			// back. A 1920x1080 framebuffer is 2025 pages, so this failure path leaked 2024 of them
			// and the boot carried on with that memory simply gone.
			release_span(fb_phys, fb_pages);
			return None;
		}

		Some(RamFb { phys: fb_phys, width, height, stride })
	}
}

// Give back a contiguous span this module allocated and never published.
//
// # Safety
//
// `phys` must be the base of a `pages`-page `allocate_contiguous` span still owned by the caller,
// freed exactly once, and not mapped anywhere.
pub(crate) unsafe fn release_span(phys: u64, pages: usize) {
	for i in 0..pages as u64 {
		// SAFETY: a page of the caller's own contiguous span, per this function's contract.
		// NEVER-MAPPED: these spans are firmware buffers reached through the physical map; none of
		// them is ever entered into a page table.
		unsafe { crate::mem::frame::deallocate(phys + i * 4096) };
	}
}

// Compare a fw-cfg file-directory name (null-padded to 56 bytes) against `want`.
unsafe fn name_eq(fw: &FwCfg, name_pa: u64, want: &[u8]) -> bool {
	unsafe {
		for (i, &c) in want.iter().enumerate() {
			if read_volatile((fw.p2v)(name_pa + i as u64) as *const u8) != c {
				return false;
			}
		}
		// The next byte must terminate the name (NUL) so `etc/ramfb` does not match a
		// longer name sharing the prefix.
		read_volatile((fw.p2v)(name_pa + want.len() as u64) as *const u8) == 0
	}
}

#[cfg(test)]
mod tests;
