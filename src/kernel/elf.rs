// Loads an ELF userspace image into a target address space. Fixed ET_EXEC images retain
// their link-time addresses; ET_DYN images receive a deterministic base and may use
// architecture-relative RELA relocations. Symbol relocations remain fail-closed until
// the M123 module graph supplies an export registry. Page tables are edited through the
// address space; segment contents and relocations are written through the HHDM, since
// the target address space is not active.

#![allow(dead_code)]

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

use crate::arch;
use crate::mem::frame::{self, PAGE_SIZE};
use crate::mem::hhdm_offset;
use crate::object::address_space::AddressSpace;
use crate::sync::SpinLock;

const DYNAMIC_MAIN_BASE: u64 = 0x1000_0000;
const DYNAMIC_MAIN_SIZE: u64 = 0x1000_0000;
const DYNAMIC_MODULE_BASE: u64 = 0x2000_0000;
const DYNAMIC_MODULE_SLOT_SIZE: u64 = 0x0100_0000;
const MAX_DYNAMIC_MODULES: u64 = 64;
const MAX_SHARED_CACHE_KEYS: usize = 16_384;
const MAX_HASH_COLLISIONS: usize = 8;

pub struct SharedPage {
	frame: u64,
}

impl SharedPage {
	pub fn frame(&self) -> u64 {
		self.frame
	}
}

impl Drop for SharedPage {
	fn drop(&mut self) {
		frame::deallocate(self.frame);
	}
}

static SHARED_PAGES: SpinLock<BTreeMap<u64, Vec<Weak<SharedPage>>>> = SpinLock::new(BTreeMap::new());

#[derive(Clone, Copy)]
struct LoadedSegment {
	start: u64,
	end: u64,
	writable: bool,
	executable: bool,
	first_frame: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ElfError {
	// The bytes are not a valid ELF64 image for this architecture (bad magic / class
	// / type / machine, or truncated). Every granular parse failure the shared reader
	// can report collapses to this - callers only distinguish it from out-of-memory.
	BadImage,
	OutOfMemory,
}

// Load `elf` into `addr_space`, recording every physical frame it allocates into
// `frames` so the caller can free them on teardown (frames pushed before an error are
// left in `frames` for the caller's cleanup). Returns the entry-point virtual address.
pub fn load_into(elf: &[u8], addr_space: &AddressSpace, frames: &mut Vec<u64>, shared: &mut Vec<Arc<SharedPage>>) -> Result<u64, ElfError> {
	let image = bootproto::elf::Elf::parse(elf).ok_or(ElfError::BadImage)?;
	let bias = if image.image_type == bootproto::elf::ET_DYN { DYNAMIC_MAIN_BASE } else { 0 };
	let window = (image.image_type == bootproto::elf::ET_DYN).then_some((DYNAMIC_MAIN_BASE, DYNAMIC_MAIN_BASE + DYNAMIC_MAIN_SIZE));
	load_parsed(&image, addr_space, frames, shared, bias, window, true, &|_| None).map(|loaded| loaded.0)
}

pub fn load_resolved_into(elf: &[u8], addr_space: &AddressSpace, frames: &mut Vec<u64>, shared: &mut Vec<Arc<SharedPage>>, resolve: &impl Fn(&str) -> Option<u64>) -> Result<u64, ElfError> {
	let image = bootproto::elf::Elf::parse(elf).ok_or(ElfError::BadImage)?;
	let bias = if image.image_type == bootproto::elf::ET_DYN { DYNAMIC_MAIN_BASE } else { 0 };
	let window = (image.image_type == bootproto::elf::ET_DYN).then_some((DYNAMIC_MAIN_BASE, DYNAMIC_MAIN_BASE + DYNAMIC_MAIN_SIZE));
	load_parsed(&image, addr_space, frames, shared, bias, window, true, resolve).map(|loaded| loaded.0)
}

pub fn load_module_into(elf: &[u8], addr_space: &AddressSpace, frames: &mut Vec<u64>, shared: &mut Vec<Arc<SharedPage>>, bias: u64, resolve: &impl Fn(&str) -> Option<u64>) -> Result<Vec<(String, u64)>, ElfError> {
	let image = bootproto::elf::Elf::parse(elf).ok_or(ElfError::BadImage)?;
	let module_end = DYNAMIC_MODULE_BASE + MAX_DYNAMIC_MODULES * DYNAMIC_MODULE_SLOT_SIZE;
	if image.image_type != bootproto::elf::ET_DYN || bias < DYNAMIC_MODULE_BASE || bias >= module_end || (bias - DYNAMIC_MODULE_BASE) % DYNAMIC_MODULE_SLOT_SIZE != 0 {
		return Err(ElfError::BadImage);
	}
	load_parsed(&image, addr_space, frames, shared, bias, Some((bias, bias + DYNAMIC_MODULE_SLOT_SIZE)), false, resolve).map(|loaded| loaded.1)
}

pub fn unmap_module(elf: &[u8], addr_space: &AddressSpace, bias: u64) {
	let Some(image) = bootproto::elf::Elf::parse(elf) else { return };
	for index in 0..image.segment_count() {
		let Some(segment) = image.segment(index) else { return };
		if segment.p_type != bootproto::elf::PT_LOAD || segment.p_memsz == 0 {
			continue;
		}
		let Some(start) = segment.p_vaddr.checked_add(bias).map(align_down) else { return };
		let Some(end) = segment.p_vaddr.checked_add(bias).and_then(|value| value.checked_add(segment.p_memsz)).and_then(align_up) else { return };
		let mut address = start;
		while address < end {
			let _ = addr_space.unmap(address);
			address += PAGE_SIZE;
		}
	}
}

fn load_parsed(image: &bootproto::elf::Elf<'_>, addr_space: &AddressSpace, frames: &mut Vec<u64>, shared: &mut Vec<Arc<SharedPage>>, bias: u64, window: Option<(u64, u64)>, require_entry: bool, resolve: &impl Fn(&str) -> Option<u64>) -> Result<(u64, Vec<(String, u64)>), ElfError> {
	let mut loaded = Vec::new();
	let shared_start = shared.len();
	let result = (|| {
		for i in 0..image.segment_count() {
			let ph = image.segment(i).ok_or(ElfError::BadImage)?;
			if ph.p_type != bootproto::elf::PT_LOAD {
				continue;
			}
			validate_segment(&ph, bias, window, &loaded)?;
			let data = image.segment_data(&ph).ok_or(ElfError::BadImage)?;
			loaded.push(map_segment(data, addr_space, frames, shared, &ph, bias)?);
		}
		if loaded.is_empty() {
			return Err(ElfError::BadImage);
		}
		if image.image_type == bootproto::elf::ET_DYN {
			apply_relocations(image, &loaded, frames, bias, resolve)?;
		}
		let entry = image.entry.checked_add(bias).ok_or(ElfError::BadImage)?;
		if require_entry && !loaded.iter().any(|segment| segment.executable && entry >= segment.start && entry < segment.end) {
			return Err(ElfError::BadImage);
		}
		Ok((entry, collect_exports(image, &loaded, bias)?))
	})();
	if result.is_err() {
		unmap_segments(addr_space, &loaded);
		shared.truncate(shared_start);
	}
	result
}

fn validate_segment(ph: &bootproto::elf::ProgramHeader, bias: u64, window: Option<(u64, u64)>, loaded: &[LoadedSegment]) -> Result<(), ElfError> {
	if ph.p_memsz == 0 || ph.p_filesz > ph.p_memsz || ph.p_flags & bootproto::elf::PF_W != 0 && ph.p_flags & bootproto::elf::PF_X != 0 {
		return Err(ElfError::BadImage);
	}
	if ph.p_align > 1 && (!ph.p_align.is_power_of_two() || ph.p_vaddr % ph.p_align != ph.p_offset % ph.p_align) {
		return Err(ElfError::BadImage);
	}
	let start = ph.p_vaddr.checked_add(bias).map(align_down).ok_or(ElfError::BadImage)?;
	let end = align_up(ph.p_vaddr.checked_add(bias).and_then(|value| value.checked_add(ph.p_memsz)).ok_or(ElfError::BadImage)?).ok_or(ElfError::BadImage)?;
	if window.is_some_and(|(window_start, window_end)| start < window_start || end > window_end) {
		return Err(ElfError::BadImage);
	}
	if loaded.iter().any(|segment| start < segment.end && end > segment.start) {
		return Err(ElfError::BadImage);
	}
	Ok(())
}

// Map one PT_LOAD segment page by page: allocate a zeroed frame for each page, copy
// the file-backed bytes (`data`, the segment's p_filesz portion) that fall in it, and
// map it at the segment's virtual address. Bytes past `data.len()` (the .bss tail) stay
// zero. Assumes page-aligned, non-overlapping segments (the userspace linker script
// enforces this). W^X: a segment is writable or executable per its flags, never both
// implicitly - only PF_X segments are fetchable, everything else maps no-execute.
fn map_segment(data: &[u8], addr_space: &AddressSpace, frames: &mut Vec<u64>, shared: &mut Vec<Arc<SharedPage>>, ph: &bootproto::elf::ProgramHeader, bias: u64) -> Result<LoadedSegment, ElfError> {
	let mut flags = arch::paging::PRESENT | arch::paging::USER;
	if ph.p_flags & bootproto::elf::PF_W != 0 {
		flags |= arch::paging::WRITABLE;
	}
	if ph.p_flags & bootproto::elf::PF_X == 0 {
		flags |= arch::paging::NO_EXECUTE;
	}
	let load_start = ph.p_vaddr.checked_add(bias).ok_or(ElfError::BadImage)?;
	let data_end = load_start.checked_add(data.len() as u64).ok_or(ElfError::BadImage)?;
	let start = align_down(load_start);
	let end = align_up(load_start.checked_add(ph.p_memsz).ok_or(ElfError::BadImage)?).ok_or(ElfError::BadImage)?;
	let pages = (end - start) / PAGE_SIZE;
	let first_frame = frames.len();
	let first_shared = shared.len();
	for page in 0..pages {
		let page_start = start + page * PAGE_SIZE;
		let copy_start = page_start.max(load_start);
		let copy_end = page_start.checked_add(PAGE_SIZE).ok_or(ElfError::BadImage)?.min(data_end);
		let destination_offset = usize::try_from(copy_start - page_start).map_err(|_| ElfError::BadImage)?;
		let copy = usize::try_from(copy_end.saturating_sub(copy_start)).map_err(|_| ElfError::BadImage)?;
		let source_offset = if copy == 0 { 0 } else { usize::try_from(copy_start.saturating_sub(load_start)).map_err(|_| ElfError::BadImage)? };
		let immutable = ph.p_flags & bootproto::elf::PF_W == 0;
		let (frame, shared_page) = if immutable {
			let page = shared_page(data, source_offset, destination_offset, copy)?;
			(page.frame(), Some(page))
		} else {
			let frame = frame::allocate().ok_or(ElfError::OutOfMemory)?;
			initialize_page(frame, data, source_offset, destination_offset, copy);
			frames.push(frame);
			(frame, None)
		};
		if addr_space.try_map(page_start, frame, flags).is_err() {
			for mapped in 0..page {
				let _ = addr_space.unmap(start + mapped * PAGE_SIZE);
			}
			shared.truncate(first_shared);
			return Err(ElfError::OutOfMemory);
		}
		if let Some(page) = shared_page {
			shared.push(page);
		}
	}
	Ok(LoadedSegment { start, end, writable: ph.p_flags & bootproto::elf::PF_W != 0, executable: ph.p_flags & bootproto::elf::PF_X != 0, first_frame })
}

fn initialize_page(frame: u64, data: &[u8], source_offset: usize, destination_offset: usize, copy: usize) {
	let dst = (hhdm_offset() + frame) as *mut u8;
	unsafe {
		core::ptr::write_bytes(dst, 0, PAGE_SIZE as usize);
		if copy != 0 {
			core::ptr::copy_nonoverlapping(data.as_ptr().add(source_offset), dst.add(destination_offset), copy);
		}
	}
}

fn shared_page(data: &[u8], source_offset: usize, destination_offset: usize, copy: usize) -> Result<Arc<SharedPage>, ElfError> {
	let hash = page_hash(data, source_offset, destination_offset, copy);
	let mut cache = SHARED_PAGES.lock();
	if !cache.contains_key(&hash) && cache.len() >= MAX_SHARED_CACHE_KEYS {
		let frame = frame::allocate().ok_or(ElfError::OutOfMemory)?;
		initialize_page(frame, data, source_offset, destination_offset, copy);
		return Ok(Arc::new(SharedPage { frame }));
	}
	let candidates = cache.entry(hash).or_default();
	candidates.retain(|candidate| candidate.strong_count() != 0);
	for candidate in candidates.iter().filter_map(Weak::upgrade) {
		if page_matches(candidate.frame(), data, source_offset, destination_offset, copy) {
			return Ok(candidate);
		}
	}
	let frame = frame::allocate().ok_or(ElfError::OutOfMemory)?;
	initialize_page(frame, data, source_offset, destination_offset, copy);
	let page = Arc::new(SharedPage { frame });
	if candidates.len() < MAX_HASH_COLLISIONS {
		candidates.push(Arc::downgrade(&page));
	}
	Ok(page)
}

fn page_hash(data: &[u8], source_offset: usize, destination_offset: usize, copy: usize) -> u64 {
	let mut hash = 0xcbf2_9ce4_8422_2325u64;
	for _ in 0..destination_offset {
		hash = (hash ^ 0).wrapping_mul(0x1000_0000_01b3);
	}
	for &byte in &data[source_offset..source_offset + copy] {
		hash = (hash ^ byte as u64).wrapping_mul(0x1000_0000_01b3);
	}
	for _ in destination_offset + copy..PAGE_SIZE as usize {
		hash = (hash ^ 0).wrapping_mul(0x1000_0000_01b3);
	}
	hash
}

fn page_matches(frame: u64, data: &[u8], source_offset: usize, destination_offset: usize, copy: usize) -> bool {
	let bytes = unsafe { core::slice::from_raw_parts((hhdm_offset() + frame) as *const u8, PAGE_SIZE as usize) };
	bytes[..destination_offset].iter().all(|byte| *byte == 0) && bytes[destination_offset..destination_offset + copy] == data[source_offset..source_offset + copy] && bytes[destination_offset + copy..].iter().all(|byte| *byte == 0)
}

fn unmap_segments(addr_space: &AddressSpace, loaded: &[LoadedSegment]) {
	for segment in loaded {
		let mut address = segment.start;
		while address < segment.end {
			let _ = addr_space.unmap(address);
			address += PAGE_SIZE;
		}
	}
}

fn apply_relocations(image: &bootproto::elf::Elf<'_>, loaded: &[LoadedSegment], frames: &[u64], bias: u64, resolve: &impl Fn(&str) -> Option<u64>) -> Result<(), ElfError> {
	let dynamic = image.dynamic_info().ok_or(ElfError::BadImage)?;
	let Some(info) = dynamic else { return Ok(()) };
	for relocation in image.rela_entries(&info).ok_or(ElfError::BadImage)?.chain(image.plt_rela_entries(&info).ok_or(ElfError::BadImage)?) {
		let target = relocation.offset.checked_add(bias).ok_or(ElfError::BadImage)?;
		let value = if relocation.symbol() == 0 && relocation.relocation_type() == relative_relocation_type() {
			bias.checked_add_signed(relocation.addend).ok_or(ElfError::BadImage)?
		} else {
			if !symbol_relocation_type(relocation.relocation_type()) {
				return Err(ElfError::BadImage);
			}
			let (symbol, name) = image.symbol(&info, relocation.symbol()).ok_or(ElfError::BadImage)?;
			if !matches!(symbol.symbol_type(), 0..=2) {
				return Err(ElfError::BadImage);
			}
			let base = if symbol.is_defined() {
				bias.checked_add(symbol.value).ok_or(ElfError::BadImage)?
			} else if let Some(address) = resolve(name) {
				address
			} else if symbol.binding() == 2 {
				0
			} else {
				return Err(ElfError::BadImage);
			};
			base.checked_add_signed(relocation.addend).ok_or(ElfError::BadImage)?
		};
		write_loaded_u64(loaded, frames, target, value)?;
	}
	Ok(())
}

fn collect_exports(image: &bootproto::elf::Elf<'_>, loaded: &[LoadedSegment], bias: u64) -> Result<Vec<(String, u64)>, ElfError> {
	let Some(info) = image.dynamic_info().ok_or(ElfError::BadImage)? else { return Ok(Vec::new()) };
	let Some(symbols) = image.symbols(&info) else { return Ok(Vec::new()) };
	let mut exports = Vec::new();
	for (symbol, name) in symbols {
		if !symbol.is_defined() || !matches!(symbol.binding(), 1 | 2) || !matches!(symbol.symbol_type(), 0..=2) || !matches!(symbol.visibility(), 0 | 3) || name.is_empty() {
			continue;
		}
		if name.len() > 255 || exports.len() >= 65_536 {
			return Err(ElfError::BadImage);
		}
		let address = bias.checked_add(symbol.value).ok_or(ElfError::BadImage)?;
		if !loaded.iter().any(|segment| address >= segment.start && address < segment.end) {
			return Err(ElfError::BadImage);
		}
		if exports.iter().any(|(existing, _): &(String, u64)| existing == name) {
			return Err(ElfError::BadImage);
		}
		exports.push((String::from(name), address));
	}
	Ok(exports)
}

fn write_loaded_u64(loaded: &[LoadedSegment], frames: &[u64], address: u64, value: u64) -> Result<(), ElfError> {
	let segment = loaded.iter().find(|segment| segment.writable && address >= segment.start && address.checked_add(8).is_some_and(|end| end <= segment.end)).ok_or(ElfError::BadImage)?;
	let offset = address - segment.start;
	let page = usize::try_from(offset / PAGE_SIZE).map_err(|_| ElfError::BadImage)?;
	let within = usize::try_from(offset % PAGE_SIZE).map_err(|_| ElfError::BadImage)?;
	if within + 8 > PAGE_SIZE as usize {
		return Err(ElfError::BadImage);
	}
	let frame = *frames.get(segment.first_frame + page).ok_or(ElfError::BadImage)?;
	unsafe {
		((hhdm_offset() + frame) as *mut u8).add(within).cast::<u64>().write_unaligned(value);
	}
	Ok(())
}

#[cfg(target_arch = "x86_64")]
const RELATIVE_RELOCATION_TYPE: u32 = 8;
#[cfg(target_arch = "aarch64")]
const RELATIVE_RELOCATION_TYPE: u32 = 1027;
#[cfg(target_arch = "riscv64")]
const RELATIVE_RELOCATION_TYPE: u32 = 3;

fn relative_relocation_type() -> u32 {
	RELATIVE_RELOCATION_TYPE
}

#[cfg(target_arch = "x86_64")]
const SYMBOL_RELOCATION_TYPES: &[u32] = &[1, 6, 7];
#[cfg(target_arch = "aarch64")]
const SYMBOL_RELOCATION_TYPES: &[u32] = &[257, 1025, 1026];
#[cfg(target_arch = "riscv64")]
const SYMBOL_RELOCATION_TYPES: &[u32] = &[2, 5];

fn symbol_relocation_type(relocation: u32) -> bool {
	SYMBOL_RELOCATION_TYPES.contains(&relocation)
}

const fn align_down(value: u64) -> u64 {
	value & !(PAGE_SIZE - 1)
}

fn align_up(value: u64) -> Option<u64> {
	value.checked_add(PAGE_SIZE - 1).map(align_down)
}
