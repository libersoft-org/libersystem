//! A firmware that behaves however a test needs it to.
//!
//! THE FIRMWARE IS A SET OF FUNCTION POINTERS, so a mock is a set of Rust functions with the same
//! signatures. There is no emulation here and no second implementation of anything: the code under
//! test is the code that ships, driven through the same `BootServices` and protocol structs the
//! firmware would hand it.
//!
//! One global `State` because the entries are `extern "efiapi"` functions with no context argument -
//! the firmware passes a protocol pointer, not a closure - and the tests are single-threaded and
//! serialised by `guard()`. That is the same constraint the loader itself lives under: the firmware
//! calls it on one processor and nothing starts another.
//!
//! What the mock can be told to do is exactly the list of things real firmware does that OVMF does
//! not: demand buffer alignment, hand back fewer bytes than asked for, describe six hundred memory
//! regions, invalidate the map key between two calls, and report a pixel format nobody here has.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::{AllocateType, BlockIo, BlockIoMedia, BootServices, FileInfo, FileProtocol, GraphicsOutput, GraphicsOutputMode, GraphicsOutputModeInfo, Guid, Handle, MemoryDescriptor, MemoryType, PhysicalAddress, PixelBitmask, Status, TableHeader};

// One disk the mock presents.
pub struct Disk {
	pub media: BlockIoMedia,
	pub proto: BlockIo,
	// The bytes behind it, indexed by device block.
	pub contents: Vec<u8>,
	// Every read this disk was asked for: (lba, byte length, buffer address). The alignment case is
	// about the ADDRESS the driver is handed, which is not observable any other way.
	pub reads: Vec<(u64, usize, usize)>,
	// Set when a read arrived on a buffer that did not satisfy `io_align`, which is the thing real
	// NVMe/SCSI/USB stacks answer `EFI_INVALID_PARAMETER` for and OVMF ignores.
	pub misaligned: bool,
}

// What the mock firmware is currently configured to be.
pub struct State {
	pub disks: Vec<Box<Disk>>,
	// The memory map, as descriptors. `get_memory_map` reports these.
	pub descriptors: Vec<MemoryDescriptor>,
	// The map key it reports, and whether it changes on the next call - which is how a firmware
	// whose map moved between the sizing call and `ExitBootServices` presents.
	pub map_key: usize,
	pub key_changes: usize,
	// The status `exit_boot_services` answers with, and how many times to answer it before
	// succeeding. `attempts` counts the calls made.
	pub exit_status: Status,
	pub exit_refusals: usize,
	pub exit_attempts: usize,
	// Pages handed out by `allocate_pages`, so a test can see what was allocated and freed.
	pub allocations: Vec<(u64, usize)>,
	// (pointer, len, capacity) of every buffer `locate_handle_buffer` handed out, so the Guard can
	// give them back: `free_pool` is a no-op here and nothing else ever did.
	pub handle_buffers: Vec<(usize, usize, usize)>,
	pub frees: Vec<(u64, usize)>,
	// Addresses `allocate_pages` must hand back, in order, before it allocates for real.
	//
	// A firmware places pages where it likes, and the case that matters is the one where it likes
	// the address the kernel has to end up at - which on QEMU `virt` is where U-Boot itself is
	// running. Nothing in a real run can be asked to do that on demand; here it is a list.
	//
	// The memory behind a forced address is NOT real, so only a caller that decides on the address
	// without writing to it may be tested this way.
	pub forced_pages: Vec<u64>,
	// A memory type `allocate_pages` refuses, standing in for firmware that will not take one.
	pub refuse_memory_type: Option<MemoryType>,
	// The file the `FileProtocol` mock serves: its declared size, its bytes, and how many bytes each
	// `read` call may return (0 = as many as asked for).
	pub file_bytes: Vec<u8>,
	pub file_declared_size: u64,
	pub file_read_chunk: usize,
	// After this many reads the file answers with an error, which is the short-read case: the
	// declared size says one thing and the reads stop before it.
	pub file_reads_before_failure: usize,
	pub file_reads: usize,
	pub file_opened: Vec<Vec<u16>>,
	// The status `Open` answers with. `EFI_NOT_FOUND` is the firmware saying it read the directory
	// and the path is not in it, which is a different fact from any other failure - and the one the
	// loader must not answer with a scan of every disk in the machine.
	pub file_open_status: Status,
	// The GOP mode the firmware reports.
	pub gop: Option<GopConfig>,
}

pub struct GopConfig {
	pub width: u32,
	pub height: u32,
	pub stride: u32,
	pub format: u32,
	pub mask: PixelBitmask,
	pub base: u64,
	pub size: usize,
}

impl State {
	const fn new() -> State {
		State { disks: Vec::new(), descriptors: Vec::new(), refuse_memory_type: None, map_key: 1, key_changes: 0, exit_status: crate::STATUS_SUCCESS, exit_refusals: 0, exit_attempts: 0, allocations: Vec::new(), handle_buffers: Vec::new(), frees: Vec::new(), forced_pages: Vec::new(), file_bytes: Vec::new(), file_declared_size: 0, file_read_chunk: 0, file_reads_before_failure: usize::MAX, file_reads: 0, file_opened: Vec::new(), file_open_status: crate::STATUS_SUCCESS, gop: None }
	}
}

static mut STATE: State = State::new();
static BUSY: AtomicBool = AtomicBool::new(false);

// Serialise the tests against the one global state. Rust runs tests in threads, and a global the
// `efiapi` entries reach without a context argument cannot be per-thread; taking this at the top of
// every test makes the sharing explicit rather than lucky.
pub struct Guard;

pub fn guard() -> Guard {
	while BUSY.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
		core::hint::spin_loop();
	}
	// A fresh firmware for every test: state left behind by the previous one is how a mock starts
	// answering questions nobody asked.
	unsafe { core::ptr::write(&raw mut STATE, State::new()) };
	Guard
}

impl Drop for Guard {
	fn drop(&mut self) {
		unsafe {
			// Give back every page the test's firmware handed out, so the process's own allocator
			// does not carry a leak from one test into the next.
			for (addr, pages) in core::mem::take(&mut (*&raw mut STATE).allocations) {
				let layout = alloc::alloc::Layout::from_size_align(pages * 4096, 4096).expect("page layout");
				alloc::alloc::dealloc(addr as *mut u8, layout);
			}
			// And every handle buffer `locate_handle_buffer` handed out. `free_pool` is a no-op, so
			// nothing else ever gave these back.
			for (ptr, len, capacity) in core::mem::take(&mut (*&raw mut STATE).handle_buffers) {
				drop(Vec::from_raw_parts(ptr as *mut Handle, len, capacity));
			}
			core::ptr::write(&raw mut STATE, State::new());
		}
		BUSY.store(false, Ordering::Release);
	}
}

// The state, for a test that is holding the guard.
#[allow(clippy::mut_from_ref)]
pub fn state() -> &'static mut State {
	unsafe { &mut *&raw mut STATE }
}

unsafe extern "efiapi" fn allocate_pages(_ty: AllocateType, _mt: MemoryType, pages: usize, out: *mut PhysicalAddress) -> Status {
	if pages == 0 {
		return crate::STATUS_INVALID_PARAMETER;
	}
	// A FIRMWARE THAT REFUSES A MEMORY TYPE, which is not hypothetical: one refused the loader's own
	// scratch type and ended the boot in a panic (LDR-012). A test can stand that firmware up here.
	if state().refuse_memory_type == Some(_mt) {
		return crate::STATUS_INVALID_PARAMETER;
	}
	if !state().forced_pages.is_empty() {
		let addr = state().forced_pages.remove(0);
		unsafe { *out = addr };
		return crate::STATUS_SUCCESS;
	}
	let layout = alloc::alloc::Layout::from_size_align(pages * 4096, 4096).expect("page layout");
	// Zeroed, because firmware pages are not - and a test that depends on them being zero would be
	// depending on something the specification does not promise. What this DOES promise is that the
	// bytes are not the file's, which is what the short-read case is about.
	let addr = unsafe { alloc::alloc::alloc(layout) };
	if addr.is_null() {
		return crate::STATUS_NOT_FOUND;
	}
	unsafe { core::ptr::write_bytes(addr, 0xa5, pages * 4096) };
	state().allocations.push((addr as u64, pages));
	unsafe { *out = addr as u64 };
	crate::STATUS_SUCCESS
}

unsafe extern "efiapi" fn free_pages(addr: PhysicalAddress, pages: usize) -> Status {
	let st = state();
	st.frees.push((addr, pages));
	if let Some(index) = st.allocations.iter().position(|&(a, p)| a == addr && p == pages) {
		st.allocations.remove(index);
		let layout = alloc::alloc::Layout::from_size_align(pages * 4096, 4096).expect("page layout");
		unsafe { alloc::alloc::dealloc(addr as *mut u8, layout) };
		return crate::STATUS_SUCCESS;
	}
	crate::STATUS_INVALID_PARAMETER
}

unsafe extern "efiapi" fn get_memory_map(size: *mut usize, buf: *mut MemoryDescriptor, key: *mut usize, desc_size: *mut usize, desc_ver: *mut u32) -> Status {
	let st = state();
	let stride = core::mem::size_of::<MemoryDescriptor>();
	let needed = st.descriptors.len() * stride;
	unsafe {
		*desc_size = stride;
		*desc_ver = 1;
	}
	if buf.is_null() || unsafe { *size } < needed {
		unsafe { *size = needed };
		return crate::STATUS_BUFFER_TOO_SMALL;
	}
	for (i, d) in st.descriptors.iter().enumerate() {
		unsafe { core::ptr::write((buf as *mut u8).add(i * stride) as *mut MemoryDescriptor, *d) };
	}
	unsafe {
		*size = needed;
		*key = st.map_key;
	}
	// A firmware whose map moves between calls: every fetch reports a new key, so the key the caller
	// carries into `ExitBootServices` is already stale.
	if st.key_changes > 0 {
		st.key_changes -= 1;
		st.map_key += 1;
	}
	crate::STATUS_SUCCESS
}

unsafe extern "efiapi" fn exit_boot_services(_image: Handle, _key: usize) -> Status {
	let st = state();
	st.exit_attempts += 1;
	if st.exit_refusals > 0 {
		st.exit_refusals -= 1;
		return st.exit_status;
	}
	crate::STATUS_SUCCESS
}

unsafe extern "efiapi" fn allocate_pool(_mt: MemoryType, size: usize, out: *mut *mut c_void) -> Status {
	let layout = alloc::alloc::Layout::from_size_align(size.max(1), 8).expect("pool layout");
	let addr = unsafe { alloc::alloc::alloc(layout) };
	if addr.is_null() {
		return crate::STATUS_NOT_FOUND;
	}
	unsafe { *out = addr as *mut c_void };
	crate::STATUS_SUCCESS
}

unsafe extern "efiapi" fn free_pool(_addr: *mut c_void) -> Status {
	// The handle buffer the disk enumeration frees is owned by the test, which drops it with the
	// state. Leaking it here rather than freeing a pointer the mock did not allocate is the safe
	// direction for a test firmware.
	crate::STATUS_SUCCESS
}

unsafe extern "efiapi" fn locate_handle_buffer(_ty: crate::LocateSearchType, guid: *const Guid, _key: *mut c_void, count: *mut usize, handles: *mut *mut Handle) -> Status {
	if unsafe { *guid } != crate::BLOCK_IO_PROTOCOL_GUID {
		return crate::STATUS_NOT_FOUND;
	}
	let st = state();
	if st.disks.is_empty() {
		return crate::STATUS_NOT_FOUND;
	}
	// The handle IS the disk's address, so `handle_protocol` can find it again. Real firmware
	// handles are opaque pointers into its own database; this is the same shape.
	let mut list: Vec<Handle> = Vec::new();
	for disk in st.disks.iter_mut() {
		let ptr: *mut Disk = &raw mut **disk;
		list.push(ptr as Handle);
	}
	unsafe {
		*count = list.len();
		*handles = list.as_mut_ptr();
	}
	// Handed to the caller, which frees it through `free_pool` - a no-op above - so the allocation
	// has to stay alive for the length of the test and be reclaimed with the rest of the state.
	// `forget` alone leaked one buffer per call, and a test looping over `locate_handle_buffer` grew
	// the process's heap for as long as it ran. Recorded here, freed by the Guard.
	let (ptr, len, capacity) = (list.as_mut_ptr(), list.len(), list.capacity());
	core::mem::forget(list);
	unsafe {
		(*&raw mut STATE).handle_buffers.push((ptr as usize, len, capacity));
	}
	crate::STATUS_SUCCESS
}

unsafe extern "efiapi" fn handle_protocol(handle: Handle, guid: *const Guid, out: *mut *mut c_void) -> Status {
	if unsafe { *guid } != crate::BLOCK_IO_PROTOCOL_GUID {
		return crate::STATUS_NOT_FOUND;
	}
	let disk = handle as *mut Disk;
	unsafe { *out = (&raw mut (*disk).proto) as *mut c_void };
	crate::STATUS_SUCCESS
}

unsafe extern "efiapi" fn read_blocks(proto: *mut BlockIo, media_id: u32, lba: u64, len: usize, buf: *mut c_void) -> Status {
	// The protocol is the first field of a `Disk`'s `proto`, so the disk is at a known offset back
	// from it - the same trick a firmware driver plays with its own context.
	let disk = unsafe { &mut *((proto as usize - core::mem::offset_of!(Disk, proto)) as *mut Disk) };
	if media_id != disk.media.media_id {
		return crate::STATUS_INVALID_PARAMETER;
	}
	disk.reads.push((lba, len, buf as usize));
	let align = disk.media.io_align as usize;
	if align > 1 && (buf as usize) % align != 0 {
		// WHAT REAL FIRMWARE DOES. The specification makes `IoAlign` a requirement on the caller's
		// buffer, and NVMe, SCSI and USB stacks answer this rather than reading into it. OVMF does
		// not care, which is why nothing in this tree has ever shown it.
		disk.misaligned = true;
		return crate::STATUS_INVALID_PARAMETER;
	}
	let block = disk.media.block_size as usize;
	let Some(offset) = (lba as usize).checked_mul(block) else {
		return crate::STATUS_INVALID_PARAMETER;
	};
	let Some(end) = offset.checked_add(len) else {
		return crate::STATUS_INVALID_PARAMETER;
	};
	if end > disk.contents.len() {
		return crate::STATUS_INVALID_PARAMETER;
	}
	unsafe { core::ptr::copy_nonoverlapping(disk.contents[offset..end].as_ptr(), buf as *mut u8, len) };
	crate::STATUS_SUCCESS
}

unsafe extern "efiapi" fn file_open(_this: *mut FileProtocol, out: *mut *mut FileProtocol, name: *const u16, _mode: u64, _attr: u64) -> Status {
	let mut wide: Vec<u16> = Vec::new();
	let mut i = 0usize;
	loop {
		let unit = unsafe { *name.add(i) };
		if unit == 0 {
			break;
		}
		wide.push(unit);
		i += 1;
		if i > 1024 {
			break;
		}
	}
	state().file_opened.push(wide);
	let status = state().file_open_status;
	if crate::is_error(status) {
		return status;
	}
	unsafe { *out = file_protocol() };
	crate::STATUS_SUCCESS
}

unsafe extern "efiapi" fn file_close(_this: *mut FileProtocol) -> Status {
	crate::STATUS_SUCCESS
}

unsafe extern "efiapi" fn file_read(_this: *mut FileProtocol, size: *mut usize, buf: *mut c_void) -> Status {
	let st = state();
	if st.file_reads >= st.file_reads_before_failure {
		return crate::STATUS_NOT_FOUND;
	}
	let asked = unsafe { *size };
	let chunk = if st.file_read_chunk == 0 { asked } else { st.file_read_chunk.min(asked) };
	let already: usize = st.file_reads * if st.file_read_chunk == 0 { asked } else { st.file_read_chunk };
	let available = st.file_bytes.len().saturating_sub(already);
	let give = chunk.min(available);
	if give > 0 {
		unsafe { core::ptr::copy_nonoverlapping(st.file_bytes[already..already + give].as_ptr(), buf as *mut u8, give) };
	}
	st.file_reads += 1;
	unsafe { *size = give };
	crate::STATUS_SUCCESS
}

unsafe extern "efiapi" fn file_set_position(_this: *mut FileProtocol, _pos: u64) -> Status {
	crate::STATUS_SUCCESS
}

unsafe extern "efiapi" fn file_get_info(_this: *mut FileProtocol, guid: *const Guid, size: *mut usize, buf: *mut c_void) -> Status {
	if unsafe { *guid } != crate::FILE_INFO_GUID {
		return crate::STATUS_NOT_FOUND;
	}
	let needed = core::mem::size_of::<FileInfo>();
	if unsafe { *size } < needed {
		unsafe { *size = needed };
		return crate::STATUS_BUFFER_TOO_SMALL;
	}
	let info = FileInfo { size: needed as u64, file_size: state().file_declared_size, physical_size: state().file_declared_size, create_time: [0; 16], last_access_time: [0; 16], modification_time: [0; 16], attribute: 0 };
	unsafe {
		core::ptr::write(buf as *mut FileInfo, info);
		*size = needed;
	}
	crate::STATUS_SUCCESS
}

// One `FileProtocol` for the whole mock: every `open` answers with it, and the state says what it
// contains.
pub fn file_protocol() -> *mut FileProtocol {
	static mut FILE: Option<FileProtocol> = None;
	unsafe {
		let slot = &mut *&raw mut FILE;
		if slot.is_none() {
			*slot = Some(FileProtocol { revision: 0x0001_0000, open: file_open, close: file_close, delete: core::ptr::null(), read: file_read, write: core::ptr::null(), get_position: core::ptr::null(), set_position: file_set_position, get_info: file_get_info, set_info: core::ptr::null(), flush: core::ptr::null() });
		}
		slot.as_mut().expect("file protocol") as *mut FileProtocol
	}
}

unsafe extern "efiapi" fn locate_protocol(guid: *const Guid, _registration: *mut c_void, out: *mut *mut c_void) -> Status {
	if unsafe { *guid } != crate::GRAPHICS_OUTPUT_PROTOCOL_GUID {
		return crate::STATUS_NOT_FOUND;
	}
	let Some(config) = state().gop.as_ref() else {
		return crate::STATUS_NOT_FOUND;
	};
	static mut INFO: Option<GraphicsOutputModeInfo> = None;
	static mut MODE: Option<GraphicsOutputMode> = None;
	static mut GOP: Option<GraphicsOutput> = None;
	unsafe {
		let info = &mut *&raw mut INFO;
		*info = Some(GraphicsOutputModeInfo { version: 0, horizontal_resolution: config.width, vertical_resolution: config.height, pixel_format: config.format, pixel_information: config.mask, pixels_per_scan_line: config.stride });
		let mode = &mut *&raw mut MODE;
		*mode = Some(GraphicsOutputMode { max_mode: 1, mode: 0, info: info.as_mut().expect("mode info") as *mut GraphicsOutputModeInfo, size_of_info: core::mem::size_of::<GraphicsOutputModeInfo>(), frame_buffer_base: config.base, frame_buffer_size: config.size });
		let gop = &mut *&raw mut GOP;
		*gop = Some(GraphicsOutput { query_mode: core::ptr::null(), set_mode: core::ptr::null(), blt: core::ptr::null(), mode: mode.as_mut().expect("mode") as *mut GraphicsOutputMode });
		*out = gop.as_mut().expect("gop") as *mut GraphicsOutput as *mut c_void;
	}
	crate::STATUS_SUCCESS
}

// The boot services table, wired to the entries above. Everything the loader does not call is left
// null: a mock that pretends to implement more than it does is a mock that hides a call.
pub fn boot_services() -> *mut BootServices {
	static mut BS: Option<BootServices> = None;
	unsafe {
		let slot = &mut *&raw mut BS;
		if slot.is_none() {
			*slot = Some(BootServices { header: TableHeader { signature: 0x5652_4553_544f_4f42, revision: 0x0002_0046, header_size: 0, crc32: 0, reserved: 0 }, raise_tpl: core::ptr::null(), restore_tpl: core::ptr::null(), allocate_pages, free_pages, get_memory_map, allocate_pool, free_pool, create_event: core::ptr::null(), set_timer: core::ptr::null(), wait_for_event: core::ptr::null(), signal_event: core::ptr::null(), close_event: core::ptr::null(), check_event: core::ptr::null(), install_protocol_interface: core::ptr::null(), reinstall_protocol_interface: core::ptr::null(), uninstall_protocol_interface: core::ptr::null(), handle_protocol, reserved: core::ptr::null(), register_protocol_notify: core::ptr::null(), locate_handle: core::ptr::null(), locate_device_path: core::ptr::null(), install_configuration_table: core::ptr::null(), load_image: core::ptr::null(), start_image: core::ptr::null(), exit: core::ptr::null(), unload_image: core::ptr::null(), exit_boot_services, get_next_monotonic_count: core::ptr::null(), stall: core::ptr::null(), set_watchdog_timer: core::ptr::null(), connect_controller: core::ptr::null(), disconnect_controller: core::ptr::null(), open_protocol: core::ptr::null(), close_protocol: core::ptr::null(), open_protocol_information: core::ptr::null(), protocols_per_handle: core::ptr::null(), locate_handle_buffer, locate_protocol, install_multiple_protocol_interfaces: core::ptr::null(), uninstall_multiple_protocol_interfaces: core::ptr::null(), calculate_crc32: core::ptr::null(), copy_mem: core::ptr::null(), set_mem: core::ptr::null(), create_event_ex: core::ptr::null() });
		}
		slot.as_mut().expect("boot services") as *mut BootServices
	}
}

// Add a disk with `block_size`-byte blocks, `io_align`-byte buffer alignment and `contents` behind
// it. Returns nothing: the disks are read back through the enumeration, which is the point.
pub fn add_disk(block_size: u32, io_align: u32, contents: Vec<u8>) {
	let blocks = contents.len() as u64 / block_size.max(1) as u64;
	let media_id = state().disks.len() as u32 + 1;
	let mut disk = Box::new(Disk { media: BlockIoMedia { media_id, removable_media: false, media_present: true, logical_partition: true, read_only: true, write_caching: false, block_size, io_align, last_block: blocks.saturating_sub(1), lowest_aligned_lba: 0, logical_blocks_per_physical_block: 1, optimal_transfer_length_granularity: 0 }, proto: BlockIo { revision: 1, media: core::ptr::null_mut(), reset: core::ptr::null(), read_blocks, write_blocks: core::ptr::null(), flush_blocks: core::ptr::null() }, contents, reads: Vec::new(), misaligned: false });
	disk.proto.media = &raw mut disk.media;
	state().disks.push(disk);
}

// A memory descriptor, for building maps by hand.
pub fn descriptor(ty: u32, base: u64, pages: u64) -> MemoryDescriptor {
	MemoryDescriptor { ty, _pad: 0, phys_start: base, virt_start: 0, page_count: pages, attribute: 0 }
}
