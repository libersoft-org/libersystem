//! Reading a file through the firmware's `FileProtocol`, and the UTF-16 name encoding that names
//! it.
//!
//! Moved out of the loader so a mock `FileProtocol` can drive it. The two defects recorded in these
//! functions - a short read returned as a full-length slice, and a name that did not fit opening a
//! DIFFERENT, shorter path - are both invisible to a boot on firmware that behaves, which is every
//! boot this tree has ever done.

use alloc::vec::Vec;
use core::ffi::c_void;

use crate::memory::{PAGE_SIZE, alloc_pages};
use crate::{self as uefi, BootServices};

// Read an entire file from the boot volume into fresh LOADER_DATA pages and return it
// as a 'static slice (the memory is retained across the hand-off).
// Give back the pages a `read_file` result occupies. The slice must be exactly what `read_file`
// returned and must not have been used since.
//
// `read_file` allocates fresh LOADER_DATA pages and returns an UNOWNED static slice, so a caller that
// copies the bytes elsewhere - which is what every bootstrap-list and pairing-file reader does - left
// the whole page allocation behind. Loader data becomes `MEM_BOOTLOADER`, which the kernel never
// seeds as usable, so each such read permanently removed its own size from the machine's RAM.
//
// # Safety
// `bytes` must be a slice returned by `read_file` from the same `bs`, and nothing may reference it
// afterwards.
pub unsafe fn free_file(bs: *mut BootServices, bytes: &[u8]) {
	let pages = bytes.len().div_ceil(PAGE_SIZE as usize).max(1);
	unsafe { ((*bs).free_pages)(bytes.as_ptr() as u64, pages) };
}

// The longest path this can open, in UTF-16 code units INCLUDING the terminator - which is the unit
// the firmware's `Open` takes and therefore the only one that describes the real boundary.
//
// It is stated here because two paths disagreed about it. The block-I/O bootstrap reader accepts any
// UTF-8 path shorter than 128 BYTES and this encodes into 64 units, so the same bootstrap list could
// work through Block I/O and fail through Simple File System - on a machine that has both, silently,
// with the file simply absent. And a non-ASCII path makes bytes and units differ, so neither number
// described the boundary even for its own reader. `MAX_PATH_UNITS` is the one both check against.
pub const MAX_PATH_UNITS: usize = 64;

// What the firmware said when it was asked for a file.
//
// THE STATUS, AND NOT MERELY ITS ABSENCE. `read_file` answers `Option`, so "this volume does not
// hold that path" and "this volume could not be read" reached the caller as one `None` - and the
// loader reads `None` as "the firmware did not mount the medium" and starts a block-level scan of
// every disk in the machine looking for the file itself. On a medium that simply does not carry the
// file, that scan cannot succeed and is paid in full: the medium is a CD, the scan mounts every FAT
// volume it meets, and a mount walks the volume's whole ownership map.
//
// The two are different facts about a medium and only one of them is what the fallback is for.
pub enum FirmwareRead {
	Bytes(&'static [u8]),
	// The volume was open and it does not hold this path. There is nothing another reader of the
	// SAME medium would find.
	Absent,
	// The firmware could not be asked - a name this reader cannot encode - or was asked and named a
	// failure. Another reader of the same medium may well do better, which is what the block-level
	// FAT backend is for.
	Failed,
}

// # Safety
// `bs` and `root` must be live firmware objects, before `ExitBootServices`. The returned slice is
// `'static` BY ASSERTION and not by fact: it borrows pages this function allocated, and the caller
// gives them back with `free_file`. Holding it past that read is a use-after-free (UEFI-005).
pub unsafe fn read_file(bs: *mut BootServices, root: *mut uefi::FileProtocol, name: &str) -> Option<&'static [u8]> {
	match unsafe { read_file_reported(bs, root, name) } {
		FirmwareRead::Bytes(bytes) => Some(bytes),
		_ => None,
	}
}

// The same read, saying WHICH of the two failures happened.
//
// # Safety
// The same contract as `read_file`, which is a thin wrapper over this.
pub unsafe fn read_file_reported(bs: *mut BootServices, root: *mut uefi::FileProtocol, name: &str) -> FirmwareRead {
	let mut wname = [0u16; MAX_PATH_UNITS];
	// A name that does not fit is not this name: opening a truncated path would open a different
	// file, which is worse than not opening one. `Failed` rather than `Absent`, because this says
	// nothing about the medium - the block-level backend takes longer paths than this encoder does.
	if !to_utf16(name, &mut wname) {
		return FirmwareRead::Failed;
	}

	let mut file: *mut uefi::FileProtocol = core::ptr::null_mut();
	let status = unsafe { ((*root).open)(root, &mut file, wname.as_ptr(), uefi::FILE_MODE_READ, 0) };
	if uefi::is_error(status) || file.is_null() {
		// THE ONE STATUS THAT IS AN ANSWER ABOUT THE MEDIUM. `EFI_NOT_FOUND` from `Open` is the
		// firmware saying it read the directory and this path is not in it. Everything else - a
		// device error, an unsupported volume, a null handle returned with a success status - is
		// this firmware failing to answer, and another reader of the same medium may still succeed.
		if status == uefi::STATUS_NOT_FOUND {
			return FirmwareRead::Absent;
		}
		return FirmwareRead::Failed;
	}
	// EVERY PATH BELOW CLOSES THE FILE. Two of them used to return without doing so, and a handle
	// left open is a firmware object that can still move the memory map - the one thing that must
	// hold still between the sizing `GetMemoryMap` and `ExitBootServices`.
	let guard = OpenFile { file };
	let file = guard.file;

	// File size via GetInfo. The structure ends in a variable-length name, so the specified
	// pattern is: call, and when the firmware answers BUFFER_TOO_SMALL with a required size, call
	// again with that size. The 512-byte stack buffer holds every name this loader opens today;
	// the heap path is what makes the helper correct for a name that is longer.
	let mut info_buf = [0u8; 512];
	let mut info_size = info_buf.len();
	let mut heap_buf: Vec<u8> = Vec::new();
	let mut info = info_buf.as_mut_ptr();
	let status = unsafe { ((*file).get_info)(file, &uefi::FILE_INFO_GUID, &mut info_size, info as *mut c_void) };
	let status = if status == uefi::STATUS_BUFFER_TOO_SMALL {
		if info_size > 64 * 1024 {
			return FirmwareRead::Failed;
		}
		if heap_buf.try_reserve_exact(info_size).is_err() {
			return FirmwareRead::Failed;
		}
		heap_buf.resize(info_size, 0);
		info = heap_buf.as_mut_ptr();
		unsafe { ((*file).get_info)(file, &uefi::FILE_INFO_GUID, &mut info_size, info as *mut c_void) }
	} else {
		status
	};
	if uefi::is_error(status) || info_size < core::mem::size_of::<uefi::FileInfo>() {
		return FirmwareRead::Failed;
	}
	let file_size = unsafe { (*(info as *const uefi::FileInfo)).file_size } as usize;

	let pages = file_size.div_ceil(PAGE_SIZE as usize).max(1);
	let Some(phys) = (unsafe { alloc_pages(bs, pages) }) else {
		return FirmwareRead::Failed;
	};

	// Read the whole file (loop until the firmware stops handing back bytes).
	let mut read_total = 0usize;
	while read_total < file_size {
		let mut chunk = file_size - read_total;
		let status = unsafe { ((*file).read)(file, &mut chunk, (phys as *mut u8).add(read_total) as *mut c_void) };
		if uefi::is_error(status) || chunk == 0 {
			break;
		}
		read_total += chunk;
	}
	// A SHORT READ IS NOT A FILE. This returned `from_raw_parts(phys, file_size)` however much had
	// arrived, so a file whose `FileInfo` said one megabyte and whose second read failed became a
	// one-megabyte slice whose tail was whatever those freshly allocated pages held - handed on as
	// a kernel image, a bootstrap package or a volume. The pages go back, and the answer is that
	// something IS there and this reader could not get it - not that the medium lacks the file.
	if read_total != file_size {
		unsafe { ((*bs).free_pages)(phys, pages) };
		return FirmwareRead::Failed;
	}
	FirmwareRead::Bytes(unsafe { core::slice::from_raw_parts(phys as *const u8, file_size) })
}

// Closes its handle however the scope ends.
struct OpenFile {
	file: *mut uefi::FileProtocol,
}

impl Drop for OpenFile {
	fn drop(&mut self) {
		unsafe { ((*self.file).close)(self.file) };
	}
}

// Encode `s` as NUL-terminated UTF-16 into `out`. False when it does not fit.
//
// This widened BYTES: `out[i] = b as u16` turned the two UTF-8 bytes of `č` into two separate
// UTF-16 units, so a name with any non-ASCII character named something else. And it `break`s when
// full, so a path longer than the buffer opened a DIFFERENT, SHORTER path rather than failing -
// with `FirmwareVolume` accepting up to 128 bytes before handing them here.
pub fn to_utf16(s: &str, out: &mut [u16]) -> bool {
	let mut i = 0;
	for unit in s.encode_utf16() {
		if i + 1 >= out.len() {
			return false;
		}
		out[i] = unit;
		i += 1;
	}
	out[i] = 0;
	true
}
