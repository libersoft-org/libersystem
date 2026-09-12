//! `opendir` / `readdir` / `closedir`, which enumerate a record rather than a directory.
//!
//! WHAT THE LOADER DOES WITH THESE. It walks each directory on its search path looking for `*.json`
//! manifests, and treats whatever it finds as a candidate driver. That is the ambient scan this
//! milestone exists to replace: the set of candidates is a property of the filesystem, and anything
//! that can write a file into one of those directories can add a driver.
//!
//! WHAT HAPPENS INSTEAD. A directory "contains" exactly the manifests the installed record names
//! under it, and nothing else. The loader's own walk is unchanged and finds exactly the set the
//! selection slot already admitted - so the code path is exercised for real rather than patched out,
//! and the answer it gets cannot be widened by anything outside the launch.

use crate::record;
use core::ffi::{c_char, c_void};
use core::ptr;

/// THE MOST ENTRIES ONE WALK MAY RETURN, which is the record's own bound: a directory cannot hold
/// more manifests than the record holds ICDs.
const MAX_ENTRIES: usize = record::MAX_ICDS;

/// A walk in progress. One per `opendir`, and there is a fixed pool of them.
#[derive(Clone, Copy)]
struct Walk {
	in_use: bool,
	/// The manifest paths this directory holds, as offsets into the record's own ICD list.
	entries: [usize; MAX_ENTRIES],
	count: usize,
	next: usize,
	/// The `struct dirent` the last `readdir` filled in, kept here because `readdir` returns a
	/// pointer into storage the caller does not own.
	entry: Dirent,
}

/// The `struct dirent` the sysroot declares. Its size and the offset of `name` are asserted by the
/// profile sysroot's per-target header in every C translation unit, so the two halves cannot drift
/// apart without a compile stopping on the file that disagrees.
#[repr(C)]
#[derive(Clone, Copy)]
struct Dirent {
	inode: u64,
	kind: u8,
	name: [c_char; 256],
}

/// THE POOL IS FIXED AND SMALL because the loader opens one directory at a time and closes it before
/// the next. A pool that grew would be an allocation on a path whose whole point is that it does not
/// reach for resources.
const MAX_WALKS: usize = 4;

static mut WALKS: [Walk; MAX_WALKS] = [Walk { in_use: false, entries: [0; MAX_ENTRIES], count: 0, next: 0, entry: Dirent { inode: 0, kind: 0, name: [0; 256] } }; MAX_WALKS];

unsafe fn as_bytes(text: *const c_char) -> &'static [u8] {
	if text.is_null() {
		return b"";
	}
	let mut length = 0;
	while unsafe { *text.add(length) } != 0 {
		length += 1;
	}
	unsafe { core::slice::from_raw_parts(text as *const u8, length) }
}

/// The directory part of a path, and the file part after it.
fn split(path: &[u8]) -> (&[u8], &[u8]) {
	match path.iter().rposition(|byte| *byte == b'/') {
		Some(at) => (&path[..at], &path[at + 1..]),
		None => (b"", path),
	}
}

/// Which of the record's manifests lie directly under `directory`.
fn entries_under(directory: &[u8]) -> ([usize; MAX_ENTRIES], usize) {
	let mut entries = [0usize; MAX_ENTRIES];
	let mut count = 0;
	let Some(record) = record::installed() else {
		return (entries, 0);
	};
	for (index, icd) in record.icds().enumerate() {
		let (parent, _) = split(icd.manifest_path);
		if parent == directory && count < MAX_ENTRIES {
			entries[count] = index;
			count += 1;
		}
	}
	(entries, count)
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn opendir(path: *const c_char) -> *mut c_void {
	let directory = unsafe { as_bytes(path) };
	let (entries, count) = entries_under(directory);
	if count == 0 {
		// AN EMPTY DIRECTORY IS NOT AN OPEN ONE. The loader treats a NULL from `opendir` as "this
		// search-path element has nothing", which is exactly what a directory outside the record
		// means here - and it is the branch upstream already handles.
		//
		// AND IT SAYS WHICH DIRECTORY, because the quietest way a discovery model can be wrong is
		// for the loader's search path and the record's paths never to meet: every call returns
		// "nothing here", the enumeration succeeds, and no driver is reached. That is exactly what
		// happened the first time this ran, and nothing said so.
		crate::report(b"no manifest under ", directory);
		return ptr::null_mut();
	}
	let walks = &raw mut WALKS;
	for index in 0..MAX_WALKS {
		let walk = unsafe { &mut (*walks)[index] };
		if walk.in_use {
			continue;
		}
		walk.in_use = true;
		walk.entries = entries;
		walk.count = count;
		walk.next = 0;
		return walk as *mut Walk as *mut c_void;
	}
	// THE POOL IS EXHAUSTED, which means the loader is holding four walks open at once. Refusing is
	// right: growing the pool here would hide a leak in the caller.
	ptr::null_mut()
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn readdir(handle: *mut c_void) -> *mut c_void {
	if handle.is_null() {
		return ptr::null_mut();
	}
	let walk = unsafe { &mut *(handle as *mut Walk) };
	if walk.next >= walk.count {
		return ptr::null_mut();
	}
	let Some(record) = record::installed() else {
		return ptr::null_mut();
	};
	let Some(icd) = record.icds().nth(walk.entries[walk.next]) else {
		return ptr::null_mut();
	};
	walk.next += 1;
	let (_, name) = split(icd.manifest_path);
	walk.entry.name = [0; 256];
	let length = name.len().min(walk.entry.name.len() - 1);
	for index in 0..length {
		walk.entry.name[index] = name[index] as c_char;
	}
	// `DT_REG`, because every entry this enumerates is a manifest and there are no directories to
	// descend into. The loader checks this field before trying to read one.
	walk.entry.kind = 8;
	walk.entry.inode = walk.next as u64;
	&raw mut walk.entry as *mut c_void
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn closedir(handle: *mut c_void) -> i32 {
	if handle.is_null() {
		return -1;
	}
	let walk = unsafe { &mut *(handle as *mut Walk) };
	walk.in_use = false;
	0
}

/// Release every walk. Test support only: the pool is process-wide, and a fixture that left one open
/// would exhaust it for the next.
#[cfg(test)]
pub fn reset() {
	let walks = &raw mut WALKS;
	for index in 0..MAX_WALKS {
		unsafe { (*walks)[index].in_use = false };
	}
}
