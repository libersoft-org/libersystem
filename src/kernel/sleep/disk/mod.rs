// HIBERNATION'S KERNEL HALF: the snapshot of every page in use, read out by the image component; and the restore - the
// image's pages written into frames outside the image, then the whole-memory replacement. The kernel has no storage
// path, so everything between the two - sealing, encrypting, writing, reading, checking - is the image component's, and
// this holds what only the kernel can: which pages are in use, their copies, and the frames a restore may not touch.
// The entry and the jump are the architecture's (`arch::sleep::snapshot`, `arch::sleep::replace_memory`).
//
// WHAT A SNAPSHOT COPIES: every page of the memory map's RAM that the kernel or a process may hold - the pool's frames
// that are not free, and the whole of what the loader handed over (the kernel, its tables, the packages), the
// firmware's ACPI tables and its non-volatile storage, which the next boot's firmware fills again and the image's
// kernel must find as it left it. Not the framebuffer, and never reserved or device memory. The copies, the lists
// that say which page each is, and the bitmap of frames not to copy are allocated BEFORE the copy; the copies and the
// bitmap are the frames it skips, and the lists are copied with the rest, because the restored machine reads its copies
// back from them; the copy itself allocates nothing and takes no lock, so no lock is ever captured held.
//
// A LIST PAGE holds `PAIRS` pairs: for a snapshot, (the page, its copy); for a restore, (the frame it goes to, the
// frame holding it now).

use alloc::vec::Vec;
#[cfg(any(test, target_arch = "x86_64"))]
use core::sync::atomic::{AtomicU64, Ordering};

use abi::{ERR_INVALID, ERR_NO_MEMORY, ERR_RESOURCE_EXHAUSTED, SNAPSHOT_BATCH, SNAPSHOT_CONTEXT, SnapshotInfo, SystemFingerprint};

use crate::mem::frame;
use crate::sync::SpinLock;

const PAGE: u64 = 4096;
pub const PAIRS: usize = (PAGE as usize) / 16;
// Frames one bitmap page covers.
const BITS: u64 = PAGE * 8;

// ------------------------------------------------------------------ which pages

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
	// The pool's: in the image where not free.
	Pool,
	// Held whole: the loader's, the kernel's, the firmware's tables and storage.
	Whole,
	// Never: reserved, device, framebuffer, bad.
	Never,
}

fn class(kind: u32) -> Class {
	match kind {
		bootproto::MEM_USABLE | bootproto::MEM_BOOTLOADER_RECLAIMABLE => Class::Pool,
		bootproto::MEM_BOOTLOADER | bootproto::MEM_KERNEL | bootproto::MEM_ACPI_RECLAIMABLE | bootproto::MEM_ACPI_NVS => Class::Whole,
		_ => Class::Never,
	}
}

// Whether a frame is RAM of a class an image may hold - what a restore's target must be.
fn image_ram(phys: u64) -> bool {
	(0..crate::mem::memmap_len()).filter_map(crate::mem::memmap_get).any(|region| class(region.kind) != Class::Never && phys >= region.base && phys + PAGE <= region.base + region.length)
}

// The end of RAM: the highest address any region of the memory map reaches.
pub fn ram_top() -> u64 {
	(0..crate::mem::memmap_len()).filter_map(crate::mem::memmap_get).filter(|region| class(region.kind) != Class::Never).map(|region| region.base + region.length).max().unwrap_or(0)
}

// EVERY PAGE THE IMAGE HOLDS, in address order, until `visit` answers false: `skip` names the frames the snapshot set
// aside for itself. Takes the allocator's lock per page and holds none between pages.
#[cfg(any(test, target_arch = "x86_64"))]
fn each_image_page(skip: &[u64], mut visit: impl FnMut(u64) -> bool) {
	for index in 0..crate::mem::memmap_len() {
		let Some(region) = crate::mem::memmap_get(index) else { continue };
		let kind = class(region.kind);
		if kind == Class::Never {
			continue;
		}
		let first = region.base.div_ceil(PAGE) * PAGE;
		let end = (region.base + region.length) / PAGE * PAGE;
		let mut page = first.max(PAGE);
		while page < end {
			let wanted = !bit_has(skip, page) && (kind == Class::Whole || !frame::is_free(page));
			if wanted && !visit(page) {
				return;
			}
			page += PAGE;
		}
	}
}

// ------------------------------------------------------------------ frames, bitmaps and lists

// A FRAME'S BYTES, through the direct map.
fn bytes(phys: u64) -> *mut u8 {
	(crate::mem::hhdm_offset() + phys) as *mut u8
}

// A bitmap's word for `phys`, over the bitmap's frames.
fn bit_word(frames: &[u64], phys: u64) -> Option<*mut u64> {
	let frame = phys / PAGE;
	let page = *frames.get((frame / BITS) as usize)?;
	// SAFETY: a word inside a bitmap frame, through the direct map.
	Some(unsafe { (bytes(page) as *mut u64).add(((frame % BITS) / 64) as usize) })
}

fn bit_has(frames: &[u64], phys: u64) -> bool {
	// SAFETY: a word of a bitmap frame, read.
	bit_word(frames, phys).is_some_and(|word| unsafe { word.read() } & (1 << ((phys / PAGE) % 64)) != 0)
}

// A pair of a list, over the list's pages.
fn pair_at(pages: &[u64], index: u64) -> *mut [u64; 2] {
	let page = pages[(index / PAIRS as u64) as usize];
	// SAFETY: the pair's slot inside a list page, through the direct map.
	unsafe { (bytes(page) as *mut [u64; 2]).add((index % PAIRS as u64) as usize) }
}

// A SET OF FRAMES, one bit each, in frames of its own.
struct Bitmap {
	frames: Vec<u64>,
}

impl Bitmap {
	fn new(top: u64) -> Result<Bitmap, i64> {
		let count = (top / PAGE).div_ceil(BITS) as usize;
		let mut frames = Vec::new();
		frames.try_reserve_exact(count).map_err(|_| ERR_NO_MEMORY)?;
		for _ in 0..count {
			let Some(phys) = frame::allocate() else {
				let bitmap = Bitmap { frames };
				bitmap.free();
				return Err(ERR_NO_MEMORY);
			};
			// SAFETY: a frame just allocated, reached through the direct map.
			unsafe { core::ptr::write_bytes(bytes(phys), 0, PAGE as usize) };
			frames.push(phys);
		}
		Ok(Bitmap { frames })
	}

	fn has(&self, phys: u64) -> bool {
		bit_has(&self.frames, phys)
	}

	// Set `phys`, answering whether it was set before; None for one past the bitmap.
	fn set(&self, phys: u64) -> Option<bool> {
		let word = bit_word(&self.frames, phys)?;
		let bit = 1u64 << ((phys / PAGE) % 64);
		// SAFETY: as above.
		unsafe {
			let before = word.read();
			word.write(before | bit);
			Some(before & bit != 0)
		}
	}

	fn free(self) {
		// SAFETY: every frame here was allocated by `new` and is referenced by nothing else.
		unsafe { frame::free_pages(&self.frames) };
	}
}

// THE PAIRS, `PAIRS` per list page.
struct Lists {
	pages: Vec<u64>,
}

impl Lists {
	fn get(&self, index: u64) -> [u64; 2] {
		// SAFETY: a list page this set owns; the index is inside it.
		unsafe { pair_at(&self.pages, index).read() }
	}

	fn put(&self, index: u64, value: [u64; 2]) {
		// SAFETY: as above.
		unsafe { pair_at(&self.pages, index).write(value) }
	}

	fn free(self) {
		// SAFETY: as `Bitmap::free`.
		unsafe { frame::free_pages(&self.pages) };
	}
}

// ------------------------------------------------------------------ the snapshot

struct Snapshot {
	lists: Lists,
	// The frames set aside: the copies, the lists and this bitmap.
	skip: Bitmap,
	// How many copy frames the lists hold, and how many pages the copy filled.
	capacity: u64,
	pages: u64,
	context: [u8; SNAPSHOT_CONTEXT],
	fingerprint: SystemFingerprint,
}

impl Snapshot {
	fn release(self) {
		for index in 0..self.capacity {
			let [_, copy] = self.lists.get(index);
			// SAFETY: every copy frame was allocated by `prepare` and is referenced by nothing else.
			unsafe { frame::deallocate(copy) };
		}
		self.lists.free();
		self.skip.free();
	}
}

static SNAPSHOT: SpinLock<Option<Snapshot>> = SpinLock::new(None);

// THE COPY'S PLAN, READ BY THE ARCHITECTURE'S ENTRY WITH NO LOCK: the list pages' array and its length, the capacity,
// the bitmap's array and its length; and the pages the copy filled, written after it. THE SNAPSHOT IS x86_64'S ENTRY's
// alone today - the other ports have no resume path for the machine an image restores - so its half is compiled there
// and for the suite.
#[cfg(any(test, target_arch = "x86_64"))]
static PLAN_LISTS: AtomicU64 = AtomicU64::new(0);
#[cfg(any(test, target_arch = "x86_64"))]
static PLAN_LIST_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(any(test, target_arch = "x86_64"))]
static PLAN_CAPACITY: AtomicU64 = AtomicU64::new(0);
#[cfg(any(test, target_arch = "x86_64"))]
static PLAN_SKIP: AtomicU64 = AtomicU64::new(0);
#[cfg(any(test, target_arch = "x86_64"))]
static PLAN_SKIP_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(any(test, target_arch = "x86_64"))]
static COPIED: AtomicU64 = AtomicU64::new(0);

// The slack the copies are allocated with beyond the pages counted in use: what the kernel may still allocate between
// the count and the copy.
#[cfg(any(test, target_arch = "x86_64"))]
const SLACK: u64 = 2048;

// PREPARE A SNAPSHOT: the pages in use counted, and the copies, the lists and the bitmap of frames the copy skips
// allocated. `ERR_RESOURCE_EXHAUSTED` - said by the caller - when free memory cannot hold the copy.
#[cfg(any(test, target_arch = "x86_64"))]
pub fn prepare(context: [u8; SNAPSHOT_CONTEXT]) -> Result<u64, i64> {
	if let Some(old) = SNAPSHOT.lock().take() {
		old.release();
	}
	let fingerprint = fingerprint();
	let skip = Bitmap::new(ram_top())?;
	let mut used = 0u64;
	each_image_page(&skip.frames, |_| {
		used += 1;
		true
	});
	let capacity = used + SLACK;
	let list_count = capacity.div_ceil(PAIRS as u64) as usize;
	// THE COPIES, THE LISTS AND THE BITMAP ARE THE FREE MEMORY'S: refused before a frame is taken when they cannot fit.
	if (frame::free_count() as u64) < capacity + list_count as u64 + 64 {
		skip.free();
		return Err(ERR_RESOURCE_EXHAUSTED);
	}
	let mut pages = Vec::new();
	if pages.try_reserve_exact(list_count).is_err() {
		skip.free();
		return Err(ERR_NO_MEMORY);
	}
	for _ in 0..list_count {
		match frame::allocate() {
			Some(phys) => pages.push(phys),
			None => {
				unsafe { frame::free_pages(&pages) };
				skip.free();
				return Err(ERR_RESOURCE_EXHAUSTED);
			}
		}
	}
	let lists = Lists { pages };
	for index in 0..capacity {
		match frame::allocate() {
			Some(copy) => lists.put(index, [0, copy]),
			None => {
				for given in 0..index {
					unsafe { frame::deallocate(lists.get(given)[1]) };
				}
				lists.free();
				skip.free();
				return Err(ERR_RESOURCE_EXHAUSTED);
			}
		}
	}
	// THE FRAMES THE COPY SKIPS: every copy and the bitmap's own. NOT THE LIST PAGES: the restored machine gives the
	// copies back by reading which frames they are from those pages - its allocator holds them as the snapshot's, and
	// no other record of them survives the replacement - so the pages go into the image like any other, each copied
	// after the column of copies it holds was written here.
	for index in 0..capacity {
		skip.set(lists.get(index)[1]);
	}
	for &page in skip.frames.clone().iter() {
		skip.set(page);
	}
	PLAN_LISTS.store(lists.pages.as_ptr() as u64, Ordering::Release);
	PLAN_LIST_COUNT.store(lists.pages.len() as u64, Ordering::Release);
	PLAN_CAPACITY.store(capacity, Ordering::Release);
	PLAN_SKIP.store(skip.frames.as_ptr() as u64, Ordering::Release);
	PLAN_SKIP_COUNT.store(skip.frames.len() as u64, Ordering::Release);
	COPIED.store(0, Ordering::Release);
	*SNAPSHOT.lock() = Some(Snapshot { lists, skip, capacity, pages: 0, context, fingerprint });
	Ok(used)
}

// THE COPY ITSELF, from the architecture's entry with every other core held and interrupts off: every page in use to
// its copy frame, in address order. Takes no lock of its own, allocates nothing, prints nothing. Answers false when the
// pages outgrew the copies prepared.
#[cfg(any(test, target_arch = "x86_64"))]
pub fn copy_now() -> bool {
	let lists_ptr = PLAN_LISTS.load(Ordering::Acquire) as *const u64;
	let list_count = PLAN_LIST_COUNT.load(Ordering::Acquire) as usize;
	let capacity = PLAN_CAPACITY.load(Ordering::Acquire);
	// SAFETY: the arrays `prepare` published, owned by the held snapshot, which nothing frees while the copy runs.
	let lists = unsafe { core::slice::from_raw_parts(lists_ptr, list_count) };
	let skip = unsafe { core::slice::from_raw_parts(PLAN_SKIP.load(Ordering::Acquire) as *const u64, PLAN_SKIP_COUNT.load(Ordering::Acquire) as usize) };
	let mut index = 0u64;
	let mut fits = true;
	each_image_page(skip, |page| {
		if index == capacity {
			fits = false;
			return false;
		}
		// SAFETY: the pair's slot in a list page the snapshot owns; the copy frame is skipped, so never the page itself.
		unsafe {
			let slot = pair_at(lists, index);
			let [_, copy] = slot.read();
			core::ptr::copy_nonoverlapping(bytes(page), bytes(copy), PAGE as usize);
			slot.write([page, copy]);
		}
		index += 1;
		true
	});
	COPIED.store(index, Ordering::Release);
	fits
}

// THE FIRST RETURN: the copy's page count recorded in the snapshot the image component reads.
#[cfg(any(test, target_arch = "x86_64"))]
pub fn taken() -> u64 {
	let pages = COPIED.load(Ordering::Acquire);
	if let Some(snapshot) = SNAPSHOT.lock().as_mut() {
		snapshot.pages = pages;
	}
	pages
}

// THE SNAPSHOT GIVEN BACK - after its image is written, when it is not, and in the restored machine, whose snapshot
// state is the one from before the copy and whose copies are nobody's now.
pub fn release() -> bool {
	match SNAPSHOT.lock().take() {
		Some(snapshot) => {
			snapshot.release();
			true
		}
		None => false,
	}
}

pub fn info() -> Result<SnapshotInfo, i64> {
	let held = SNAPSHOT.lock();
	let Some(snapshot) = held.as_ref() else { return Err(ERR_INVALID) };
	if snapshot.pages == 0 {
		return Err(ERR_INVALID);
	}
	Ok(SnapshotInfo { pages: snapshot.pages, context: snapshot.context, system: snapshot.fingerprint.system, hardware: snapshot.fingerprint.hardware })
}

// PAGES [first, first + count) READ OUT: `write(offset, bytes)` called with the frames' addresses and then the pages.
pub fn read(first: u64, count: u64, mut write: impl FnMut(usize, &[u8]) -> Result<(), i64>) -> Result<(), i64> {
	let held = SNAPSHOT.lock();
	let Some(snapshot) = held.as_ref() else { return Err(ERR_INVALID) };
	if count == 0 || count > SNAPSHOT_BATCH || first.checked_add(count).is_none_or(|end| end > snapshot.pages) {
		return Err(ERR_INVALID);
	}
	for at in 0..count {
		let [page, _] = snapshot.lists.get(first + at);
		write(at as usize * 8, &page.to_le_bytes())?;
	}
	let data = count as usize * 8;
	for at in 0..count {
		let [_, copy] = snapshot.lists.get(first + at);
		// SAFETY: a copy frame the held snapshot owns, read through the direct map.
		let page = unsafe { core::slice::from_raw_parts(bytes(copy), PAGE as usize) };
		write(data + at as usize * PAGE as usize, page)?;
	}
	Ok(())
}

// ------------------------------------------------------------------ the fingerprint

// THE SYSTEM IMAGE'S DIGEST, computed once: the kernel's own code and read-only data as they are loaded, then every
// module the loader handed over - name and bytes, in the order handed over - and, on a development profile, the variant
// the development switch names (see `arch::sleep::development_variant`). The modules are the kernel's for its whole
// life, so the digest reads the same bytes whenever it is asked.
static SYSTEM_DIGEST: SpinLock<Option<[u8; 32]>> = SpinLock::new(None);

fn system_digest() -> [u8; 32] {
	if let Some(digest) = *SYSTEM_DIGEST.lock() {
		return digest;
	}
	let (code, code_len) = crate::arch::sleep::kernel_image();
	// SAFETY: the kernel's own loaded code and read-only data, mapped for its whole life.
	let image = unsafe { core::slice::from_raw_parts(code as *const u8, code_len) };
	let info = crate::boot_info();
	// SAFETY: the loader's module array, in the direct map for the kernel's whole life.
	let modules = unsafe { core::slice::from_raw_parts(info.modules as *const bootproto::Module, info.modules_len as usize) };
	let mut variant = [0u8; 64];
	let variant_len = crate::arch::sleep::development_variant(&mut variant);
	// SAFETY: each module's bytes, as `module_bytes` reads them.
	let digest = digest_of(image, modules.iter().map(|module| (&module.name[..], unsafe { core::slice::from_raw_parts(module.addr as *const u8, module.size as usize) })), &variant[..variant_len]);
	*SYSTEM_DIGEST.lock() = Some(digest);
	digest
}

// The digest itself: the kernel's bytes, each module's name and bytes in order, and the variant - each part hashed on its
// own first, so no two different lists of parts run together into the same bytes. The parts' digests are fed to one
// hasher in turn, so nothing is allocated to hold them.
fn digest_of<'a>(kernel: &[u8], modules: impl Iterator<Item = (&'a [u8], &'a [u8])>, variant: &[u8]) -> [u8; 32] {
	let mut parts = bootproto::sha256::Sha256::new();
	parts.update(&bootproto::sha256::digest(kernel));
	for (name, bytes) in modules {
		parts.update(&bootproto::sha256::digest(name));
		parts.update(&bootproto::sha256::digest(bytes));
	}
	if !variant.is_empty() {
		parts.update(b"variant");
		parts.update(&bootproto::sha256::digest(variant));
	}
	parts.finish()
}

// THE HARDWARE'S DIGEST: the memory map by class - every kind of RAM the loader or the kernel may take one class, each
// other kind its own, adjacent ranges of one class merged, so what a boot's loader allocated does not move it - the
// cores, and every PCI function the scan found with its identity.
fn hardware_digest() -> [u8; 32] {
	let mut parts = bootproto::sha256::Sha256::new();
	let mut last: Option<(u32, u64, u64)> = None;
	let push = |parts: &mut bootproto::sha256::Sha256, range: (u32, u64, u64)| {
		parts.update(&range.0.to_le_bytes());
		parts.update(&range.1.to_le_bytes());
		parts.update(&range.2.to_le_bytes());
	};
	for index in 0..crate::mem::memmap_len() {
		let Some(region) = crate::mem::memmap_get(index) else { continue };
		let kind = match region.kind {
			bootproto::MEM_USABLE | bootproto::MEM_BOOTLOADER_RECLAIMABLE | bootproto::MEM_BOOTLOADER | bootproto::MEM_KERNEL => 0,
			other => other + 1,
		};
		match last {
			Some((class, base, length)) if class == kind && base + length == region.base => last = Some((class, base, length + region.length)),
			Some(range) => {
				push(&mut parts, range);
				last = Some((kind, region.base, region.length));
			}
			None => last = Some((kind, region.base, region.length)),
		}
	}
	if let Some(range) = last {
		push(&mut parts, range);
	}
	parts.update(&(crate::smp::cpu_count() as u64).to_le_bytes());
	for index in 0..crate::device::count() {
		let _ = crate::device::with(index, |entry| {
			if entry.on_bus && entry.platform.is_none() {
				parts.update(&[entry.bus, entry.dev, entry.func, entry.class, entry.subclass, entry.prog_if]);
				parts.update(&entry.vendor.to_le_bytes());
				parts.update(&entry.product.to_le_bytes());
			}
		});
	}
	parts.finish()
}

pub fn fingerprint() -> SystemFingerprint {
	SystemFingerprint { system: system_digest(), hardware: hardware_digest() }
}

// ------------------------------------------------------------------ the restore

pub struct Restore {
	lists: Lists,
	// The frames the image's pages go to.
	targets: Bitmap,
	pages: u64,
	written: u64,
	// Frames the allocator answered that are targets: held, never used, given back with the restore.
	aside: Vec<u64>,
	// The image's resume context, which x86_64's replacement jumps with.
	#[cfg(target_arch = "x86_64")]
	pub context: [u8; SNAPSHOT_CONTEXT],
}

static RESTORE: SpinLock<Option<Restore>> = SpinLock::new(None);

impl Restore {
	// A FRAME NO PAGE OF THE IMAGE GOES TO.
	fn safe_frame(targets: &Bitmap, aside: &mut Vec<u64>) -> Option<u64> {
		loop {
			let phys = frame::allocate()?;
			if !targets.has(phys) {
				return Some(phys);
			}
			if aside.try_reserve(1).is_err() {
				unsafe { frame::deallocate(phys) };
				return None;
			}
			aside.push(phys);
		}
	}

	fn release(self) {
		for index in 0..self.written {
			let [_, held] = self.lists.get(index);
			// SAFETY: a frame `write` allocated for this restore.
			unsafe { frame::deallocate(held) };
		}
		unsafe { frame::free_pages(&self.aside) };
		self.lists.free();
		self.targets.free();
	}

	// The pairs, for the architecture's copy: (the frame a page goes to, the frame holding it), `pages` of them.
	#[cfg(target_arch = "x86_64")]
	pub fn pairs(&self) -> (&[u64], u64) {
		(&self.lists.pages, self.pages)
	}

	// Another frame outside the image, for the architecture's trampoline and its tables.
	#[cfg(target_arch = "x86_64")]
	pub fn frame(&mut self) -> Option<u64> {
		Self::safe_frame(&self.targets, &mut self.aside)
	}
}

// A RESTORE BEGUN: its `count` frames, read by `frame_at` twice, checked - RAM of an image's classes, page-aligned, each
// named once - and the list pages allocated outside them.
pub fn begin(count: u64, context: [u8; SNAPSHOT_CONTEXT], mut frame_at: impl FnMut(u64) -> Result<u64, i64>) -> Result<(), i64> {
	if count == 0 || !crate::arch::sleep::context_is_ours(&context) {
		return Err(ERR_INVALID);
	}
	if let Some(old) = RESTORE.lock().take() {
		old.release();
	}
	let top = ram_top();
	let targets = Bitmap::new(top)?;
	for index in 0..count {
		let phys = match frame_at(index) {
			Ok(phys) => phys,
			Err(error) => {
				targets.free();
				return Err(error);
			}
		};
		if phys % PAGE != 0 || phys == 0 || phys >= top || !image_ram(phys) || targets.set(phys) != Some(false) {
			targets.free();
			return Err(ERR_INVALID);
		}
	}
	let pages = count;
	let mut aside = Vec::new();
	let mut list_pages = Vec::new();
	let list_count = pages.div_ceil(PAIRS as u64) as usize;
	if list_pages.try_reserve_exact(list_count).is_err() {
		targets.free();
		return Err(ERR_NO_MEMORY);
	}
	for _ in 0..list_count {
		match Restore::safe_frame(&targets, &mut aside) {
			Some(phys) => list_pages.push(phys),
			None => {
				unsafe { frame::free_pages(&list_pages) };
				unsafe { frame::free_pages(&aside) };
				targets.free();
				return Err(ERR_RESOURCE_EXHAUSTED);
			}
		}
	}
	let lists = Lists { pages: list_pages };
	for index in 0..count {
		// READ AGAIN, AND HELD TO WHAT THE FIRST PASS CHECKED: a list that changed between the two is refused.
		let phys = match frame_at(index) {
			Ok(phys) if targets.has(phys) => phys,
			other => {
				let _ = other;
				lists.free();
				unsafe { frame::free_pages(&aside) };
				targets.free();
				return Err(ERR_INVALID);
			}
		};
		lists.put(index, [phys, 0]);
	}
	*RESTORE.lock() = Some(Restore {
		lists,
		targets,
		pages,
		written: 0,
		aside,
		#[cfg(target_arch = "x86_64")]
		context,
	});
	Ok(())
}

// PAGES [first, first + count) WRITTEN, in order: each into a frame outside the image.
pub fn write(first: u64, count: u64, mut read: impl FnMut(usize, *mut u8) -> Result<(), i64>) -> Result<(), i64> {
	let mut held = RESTORE.lock();
	let Some(restore) = held.as_mut() else { return Err(ERR_INVALID) };
	if count == 0 || count > SNAPSHOT_BATCH || first != restore.written || first + count > restore.pages {
		return Err(ERR_INVALID);
	}
	for at in 0..count {
		let Some(phys) = Restore::safe_frame(&restore.targets, &mut restore.aside) else { return Err(ERR_RESOURCE_EXHAUSTED) };
		if let Err(error) = read(at as usize * PAGE as usize, bytes(phys)) {
			unsafe { frame::deallocate(phys) };
			return Err(error);
		}
		let [target, _] = restore.lists.get(first + at);
		restore.lists.put(first + at, [target, phys]);
		restore.written += 1;
	}
	Ok(())
}

// THE RESTORE DROPPED, its frames given back.
pub fn abandon() -> bool {
	match RESTORE.lock().take() {
		Some(restore) => {
			restore.release();
			true
		}
		None => false,
	}
}

// THE WHOLE-MEMORY REPLACEMENT, once every page is written - handed to the boot core, where the image's resume path
// continues. Answers only when it cannot happen.
pub fn commit() -> i64 {
	match RESTORE.lock().as_ref() {
		Some(restore) if restore.written == restore.pages => {}
		_ => return ERR_INVALID,
	}
	crate::arch::sleep::replace()
}

// ON THE BOOT CORE: the replacement itself, with the restore taken out of its lock for it - nothing may hold that lock,
// or any other, while memory is overwritten - and put back only when it did not happen.
#[cfg(target_arch = "x86_64")]
pub fn replace_now() -> i64 {
	let Some(mut restore) = RESTORE.lock().take() else { return ERR_INVALID };
	let error = crate::arch::sleep::replace_memory(&mut restore);
	*RESTORE.lock() = Some(restore);
	error
}

#[cfg(test)]
mod tests;
