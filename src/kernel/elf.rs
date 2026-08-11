// Loads an ELF userspace image into a target address space. Fixed ET_EXEC images retain
// their link-time addresses; ET_DYN images receive a deterministic base and may use
// architecture-relative RELA relocations. Symbol relocations remain fail-closed until
// the module graph supplies an export registry. Page tables are edited through the
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
use crate::memlayout::{USER_MMAP_BASE, USER_STACK_PAGES, USER_STACK_TOP, USER_VA_END};
use crate::object::address_space::AddressSpace;
use crate::sync::SpinLock;

const DYNAMIC_MAIN_BASE: u64 = 0x1000_0000;
const DYNAMIC_MAIN_SIZE: u64 = 0x1000_0000;
const DYNAMIC_MODULE_BASE: u64 = 0x2000_0000;
const DYNAMIC_MODULE_SLOT_SIZE: u64 = 0x0100_0000;
const MAX_DYNAMIC_MODULES: u64 = 64;
const MAX_SHARED_CACHE_KEYS: usize = 16_384;
const MAX_HASH_COLLISIONS: usize = 8;
pub const MAX_DYNAMIC_SYMBOL_NAME: usize = 512;

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
		// RETIRE, like every other frame that was ever in a page table.
		//
		// This one path used to `deallocate`, handing the frame straight back to the allocator with
		// nothing between - while another core could still hold a translation for it. It is
		// reachable through `load_module_into`, which maps into a RUNNING process whose other
		// threads are on other cores, and x86's `unmap_page_in` does a local `invlpg` only. So a
		// relocation failure in a module loaded into a live process could free a frame a running
		// thread still reached.
		//
		// It was tried once before, on 2026-08-10, and reverted the same day: `retire` queued into a
		// HEAP-allocated quarantine, the heap grows by taking frames, and the failure this drop runs
		// on is precisely "there are no frames" - so the queue could not take it, the fallback
		// needed a shootdown, and when that did not complete the page was counted lost. The
		// out-of-frames rollback test caught it exactly: `a load refused at allocation 85 kept 1
		// frame(s) it took (0 still quarantined)`.
		//
		// The quarantine is now a fixed 512-entry array that allocates nothing, so the reason is
		// gone: a push refuses only when it is FULL, and the caller then pays for its own shootdown
		// - the path that already existed. The two findings landed together, which is what the
		// milestone said they had to do.
		//
		// SAFETY: this type owns the frame from creation to here, and the map that hands
		// out `Weak` references to it is the only place it is reachable from.
		unsafe { frame::retire(&[self.frame]) };
	}
}

static SHARED_PAGES: SpinLock<BTreeMap<u64, Vec<Weak<SharedPage>>>> = SpinLock::new(BTreeMap::new());

// One shared page, owning a fresh frame and registered nowhere, so a test can drop it and watch
// what its `Drop` does with the frame. Not `shared_page()`: that one hashes real image bytes and
// may hand back an existing page, which is the opposite of what a lifetime test needs.
#[cfg(test)]
pub fn shared_page_for_test() -> Arc<SharedPage> {
	let frame = frame::allocate().expect("a frame for the test's shared page");
	Arc::new(SharedPage { frame })
}

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

// The virtual ranges a successful load left mapped, so a caller whose own later step
// fails can take them down before it frees the frames underneath them.
//
// Without this the loader freed the ELF frames when the STACK mapping failed and unmapped
// nothing: the process's page tables went on naming frames that were back in the
// allocator, and the next thing to be handed one of them shared physical memory with a
// live address space. `load_parsed` unmaps its own segments when IT fails; this is for the
// failure that happens after it has succeeded.
pub fn loaded_ranges(elf: &[u8]) -> Vec<(u64, u64)> {
	let Some(image) = bootproto::elf::Elf::parse(elf) else {
		return Vec::new();
	};
	let bias = if image.image_type == bootproto::elf::ET_DYN { DYNAMIC_MAIN_BASE } else { 0 };
	let mut out = Vec::new();
	for i in 0..image.segment_count() {
		let Some(ph) = image.segment(i) else { continue };
		if ph.p_type != bootproto::elf::PT_LOAD {
			continue;
		}
		let Some(start) = ph.p_vaddr.checked_add(bias).map(align_down) else { continue };
		let Some(end) = ph.p_vaddr.checked_add(bias).and_then(|v| v.checked_add(ph.p_memsz)).and_then(align_up) else { continue };
		out.push((start, end));
	}
	out
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

// The bottom of the ring-3 stack the loader maps eagerly. Below it the stack is demand-grown
// up to the Domain's own ceiling, which is not a compile-time range and is not bounded here: a
// segment down there collides only if the stack actually grows into it, and the fault handler's
// `try_map` refuses that and kills the process, which is the image's own doing.
const STACK_BASE: u64 = USER_STACK_TOP - USER_STACK_PAGES * PAGE_SIZE;

// Do [a_start, a_end) and [b_start, b_end) share a byte?
fn overlaps(a_start: u64, a_end: u64, b_start: u64, b_end: u64) -> bool {
	a_start < b_end && b_start < a_end
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
	// Userspace, ALWAYS, whatever the image type. `window` is the tighter bound a
	// dynamic image is confined to; it was the only bound there was, and it is `None`
	// for ET_EXEC - so an executable could name any address at all, including the
	// kernel half, and the mapper would map it there with the USER bit set.
	//
	// On x86_64 that was the first link of a complete escalation: a user page in the
	// higher half, executed, issuing `syscall` - and the entry stub used to read the
	// resulting negative return address as a kernel self-call. The stub no longer
	// decides that way, and this refuses the premise as well. Two independent defences
	// for one hole, deliberately.
	if start >= USER_VA_END || end > USER_VA_END || end <= start {
		return Err(ElfError::BadImage);
	}
	// And clear of the two ranges the kernel maps into this address space whether the image
	// mentions them or not: the ring-3 stack, mapped right after the segments are, and the
	// mmap window, whose pool hands out addresses in the belief that they are free.
	//
	// Nothing unsafe happened without this - `try_map` refuses to remap a live page, so the
	// stack mapping failed and the load failed with it. What it failed WITH was
	// `OutOfMemory`, for an image that is not large but malformed, and the image chose which
	// of its own segments got mapped before that happened. Refusing here names the real
	// problem and refuses it before a single frame is allocated for the segment.

	if overlaps(start, end, STACK_BASE, USER_STACK_TOP) || overlaps(start, end, USER_MMAP_BASE, USER_VA_END) {
		return Err(ElfError::BadImage);
	}
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
		let kind = bootproto::elf::dynamic_relocation_kind(bootproto::elf::expected_machine(), relocation.relocation_type()).ok_or(ElfError::BadImage)?;
		if !kind.accepts_symbol(relocation.symbol()) {
			return Err(ElfError::BadImage);
		}
		let value = match kind {
			bootproto::elf::DynamicRelocationKind::Relative => bias.checked_add_signed(relocation.addend).ok_or(ElfError::BadImage)?,
			bootproto::elf::DynamicRelocationKind::Symbol => {
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
			}
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
		if name.len() > MAX_DYNAMIC_SYMBOL_NAME || exports.len() >= 65_536 {
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

const fn align_down(value: u64) -> u64 {
	value & !(PAGE_SIZE - 1)
}

fn align_up(value: u64) -> Option<u64> {
	value.checked_add(PAGE_SIZE - 1).map(align_down)
}

#[cfg(test)]
mod tests {
	use super::{ElfError, LoadedSegment, validate_segment};
	use crate::memlayout::USER_VA_END;
	use bootproto::elf::{PF_R, PF_W, PF_X, PT_LOAD, ProgramHeader};

	// A page-aligned, otherwise well-formed loadable segment at `vaddr`.
	fn segment(vaddr: u64, memsz: u64, flags: u32) -> ProgramHeader {
		ProgramHeader { p_type: PT_LOAD, p_flags: flags, p_offset: 0, p_vaddr: vaddr, p_paddr: 0, p_filesz: 0, p_memsz: memsz, p_align: 0x1000 }
	}

	crate::tagged_test!(an_et_exec_segment_may_not_name_the_kernel_half, [Memory], id = "kernel.elf.an_et_exec_segment_may_not_name_the_kernel_half", covers = ["kernel"]);
	fn an_et_exec_segment_may_not_name_the_kernel_half() {
		// `window` is None for ET_EXEC - that is what the loader passes - and it used to be the
		// ONLY bound, so an executable naming a higher-half address was mapped there with the
		// USER bit set. Every one of these is a segment a hostile image can simply declare.
		let none: &[LoadedSegment] = &[];
		for (vaddr, what) in [
			(USER_VA_END, "the first address above the user half"),
			(USER_VA_END + 0x1000, "one page above the user half"),
			(0xffff_8000_0000_0000, "the bottom of the x86_64 kernel half"),
			(0xffff_ffff_8000_0000, "the kernel's own image range"),
		] {
			let header = segment(vaddr, 0x1000, PF_R | PF_X);
			assert!(matches!(validate_segment(&header, 0, None, none), Err(ElfError::BadImage)), "an ET_EXEC segment at {what} ({vaddr:#x}) must be refused");
		}
		// A segment ENDING above the half is refused too: the start being legal is not enough,
		// because the pages that follow it are what get mapped.
		let straddling = segment(USER_VA_END - 0x1000, 0x4000, PF_R | PF_W);
		assert!(matches!(validate_segment(&straddling, 0, None, none), Err(ElfError::BadImage)), "a segment starting inside the half and ending above it must be refused");
		// And a legal one still loads, so the bound is a boundary rather than a blanket refusal.
		let ok = segment(0x40_0000, 0x2000, PF_R | PF_X);
		assert!(validate_segment(&ok, 0, None, none).is_ok(), "an ordinary low segment must still validate");
	}

	crate::tagged_test!(a_bias_may_not_carry_a_segment_out_of_the_user_half, [Memory], id = "kernel.elf.a_bias_may_not_carry_a_segment_out_of_the_user_half", covers = ["kernel"]);
	fn a_bias_may_not_carry_a_segment_out_of_the_user_half() {
		// A dynamic image's addresses are its own plus a bias the loader picks, so the check has
		// to be on the sum. An overflowing sum is refused rather than wrapped to something low.
		let none: &[LoadedSegment] = &[];
		let header = segment(0x1000, 0x1000, PF_R | PF_X);
		assert!(matches!(validate_segment(&header, USER_VA_END, None, none), Err(ElfError::BadImage)), "a bias that lands the segment above the half must be refused");
		assert!(matches!(validate_segment(&header, u64::MAX, None, none), Err(ElfError::BadImage)), "a bias that overflows the address must be refused, not wrapped");
	}

	crate::tagged_test!(a_segment_overlapping_one_already_loaded_is_refused, [Memory], id = "kernel.elf.a_segment_overlapping_one_already_loaded_is_refused", covers = ["kernel"]);
	fn a_segment_overlapping_one_already_loaded_is_refused() {
		// Two segments over the same pages means the second's frames replace the first's in the
		// page tables while the first's stay in the process's owned list - and the image decides
		// the addresses, so this is a claim to check, not a property to assume.
		let loaded = [LoadedSegment { start: 0x40_0000, end: 0x44_0000, writable: false, executable: true, first_frame: 0 }];
		for (vaddr, memsz, what) in [
			(0x40_0000u64, 0x1000u64, "starting exactly where a loaded segment starts"),
			(0x43_f000, 0x2000, "straddling the end of a loaded segment"),
			(0x3f_f000, 0x2000, "straddling the start of a loaded segment"),
			(0x40_0000, 0x40_0000, "covering a loaded segment exactly"),
		] {
			let header = segment(vaddr, memsz, PF_R | PF_W);
			assert!(matches!(validate_segment(&header, 0, None, &loaded), Err(ElfError::BadImage)), "a segment {what} must be refused");
		}
		// Abutting is not overlapping: a segment that begins where the last one ended is legal.
		let abutting = segment(0x44_0000, 0x1000, PF_R | PF_W);
		assert!(validate_segment(&abutting, 0, None, &loaded).is_ok(), "a segment starting where the previous one ended must validate");
	}

	crate::tagged_test!(a_segment_may_not_claim_the_stack_or_the_mmap_window, [Memory], id = "kernel.elf.a_segment_may_not_claim_the_stack_or_the_mmap_window", covers = ["kernel"]);
	fn a_segment_may_not_claim_the_stack_or_the_mmap_window() {
		// Two ranges the kernel maps into every process whether the image mentions them or
		// not. An image claiming either used to be caught only by `try_map` refusing to
		// remap - after its earlier segments were already mapped, and reported as
		// out-of-memory.
		use crate::memlayout::{USER_MMAP_BASE, USER_STACK_TOP};
		let none: &[LoadedSegment] = &[];
		for (vaddr, memsz, what) in [
			(USER_STACK_TOP - 0x1000, 0x1000u64, "the top page of the ring-3 stack"),
			(super::STACK_BASE, 0x1000, "the bottom page of the ring-3 stack"),
			(super::STACK_BASE - 0x1000, 0x4000, "straddling the bottom of the ring-3 stack"),
			(USER_MMAP_BASE, 0x1000, "the base of the mmap window"),
			(USER_MMAP_BASE + 0x10_0000, 0x1000, "inside the mmap window"),
		] {
			let header = segment(vaddr, memsz, PF_R | PF_W);
			assert!(matches!(validate_segment(&header, 0, None, none), Err(ElfError::BadImage)), "a segment claiming {what} ({vaddr:#x}) must be refused");
		}
		// The page below the eagerly-mapped stack is NOT refused: that is the demand-grown
		// region, whose extent is a Domain setting rather than a constant, and a collision
		// there is the image's own problem when its stack reaches that far.
		let below = segment(super::STACK_BASE - 0x1000, 0x1000, PF_R | PF_W);
		assert!(validate_segment(&below, 0, None, none).is_ok(), "the page below the eager stack is not a range this refuses");
	}

	crate::tagged_test!(a_write_execute_segment_is_refused, [Memory], id = "kernel.elf.a_write_execute_segment_is_refused", covers = ["kernel"]);
	fn a_write_execute_segment_is_refused() {
		// W^X, declared by the image rather than derived from it, so it is the image's claim
		// that has to be refused.
		let none: &[LoadedSegment] = &[];
		let wx = segment(0x40_0000, 0x1000, PF_R | PF_W | PF_X);
		assert!(matches!(validate_segment(&wx, 0, None, none), Err(ElfError::BadImage)), "a segment claiming both write and execute must be refused");
	}
}
