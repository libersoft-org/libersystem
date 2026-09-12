//! `fopen` / `fread` / `fclose` / `fileno` / `fstat`, reading a manifest out of the record.
//!
//! THE LOADER READS ITS MANIFESTS WITH STDIO, and there is no reason to patch that out: what it
//! reads is bytes, and where those bytes come from is this module's answer. A manifest here is a
//! slice the record already holds - it was staged and verified before the launch - so "opening" one
//! is finding it and "reading" it is copying from memory.
//!
//! NOTHING ELSE CAN BE OPENED. A path the record does not name returns NULL, which is the branch the
//! loader already handles for a manifest it cannot read.

use crate::record;
use core::ffi::{c_char, c_void};
use core::ptr;

/// An open manifest: which bytes, and how far through them the caller is.
#[derive(Clone, Copy)]
struct Open {
	in_use: bool,
	bytes: &'static [u8],
	at: usize,
}

/// The loader opens one manifest at a time and closes it before the next.
const MAX_OPEN: usize = 4;

static mut OPEN: [Open; MAX_OPEN] = [Open { in_use: false, bytes: &[], at: 0 }; MAX_OPEN];

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

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fopen(path: *const c_char, mode: *const c_char) -> *mut c_void {
	// READING ONLY, AND A WRITE MODE IS REFUSED RATHER THAN IGNORED. A loader that believed it had
	// opened a manifest for writing would report success for a write that went nowhere.
	let mode = unsafe { as_bytes(mode) };
	if !mode.starts_with(b"r") {
		return ptr::null_mut();
	}
	let Some(record) = record::installed() else {
		return ptr::null_mut();
	};
	let Some(icd) = record.icd_by_manifest(unsafe { as_bytes(path) }) else {
		// SAYS WHICH PATH, for the reason `opendir` does: a loader whose search path and a record's
		// paths never meet gets "nothing here" from every call, succeeds, and reaches no driver.
		crate::report(b"no manifest at ", unsafe { as_bytes(path) });
		return ptr::null_mut();
	};
	let opens = &raw mut OPEN;
	for index in 0..MAX_OPEN {
		let open = unsafe { &mut (*opens)[index] };
		if open.in_use {
			continue;
		}
		open.in_use = true;
		open.bytes = icd.manifest;
		open.at = 0;
		return open as *mut Open as *mut c_void;
	}
	ptr::null_mut()
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fread(buffer: *mut c_void, size: usize, count: usize, stream: *mut c_void) -> usize {
	if stream.is_null() || buffer.is_null() {
		return 0;
	}
	let Some(total) = size.checked_mul(count) else {
		return 0;
	};
	let open = unsafe { &mut *(stream as *mut Open) };
	let remaining = open.bytes.len() - open.at;
	let taken = total.min(remaining);
	unsafe { ptr::copy_nonoverlapping(open.bytes.as_ptr().add(open.at), buffer as *mut u8, taken) };
	open.at += taken;
	// THE RETURN IS A COUNT OF ITEMS, NOT OF BYTES, which is `fread`'s contract and the usual place
	// a reimplementation goes wrong.
	match size {
		0 => 0,
		_ => taken / size,
	}
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fclose(stream: *mut c_void) -> i32 {
	if stream.is_null() {
		return -1;
	}
	let open = unsafe { &mut *(stream as *mut Open) };
	open.in_use = false;
	open.bytes = &[];
	open.at = 0;
	0
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fileno(stream: *mut c_void) -> i32 {
	if stream.is_null() {
		return -1;
	}
	// AN INDEX, NOT A DESCRIPTOR, and nothing outside this crate may treat it as one. The loader
	// uses it only to pass to `fstat` below, which is why a number that means something here is
	// enough - and why no other call in this substrate accepts one.
	let opens = &raw const OPEN;
	for index in 0..MAX_OPEN {
		if core::ptr::eq(unsafe { &(*opens)[index] } as *const Open, stream as *const Open) {
			return index as i32;
		}
	}
	-1
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fstat(descriptor: i32, out: *mut c_void) -> i32 {
	if out.is_null() || descriptor < 0 || descriptor as usize >= MAX_OPEN {
		return -1;
	}
	let opens = &raw const OPEN;
	let open = unsafe { &(*opens)[descriptor as usize] };
	if !open.in_use {
		return -1;
	}
	// THE ONE FIELD THE LOADER READS IS THE SIZE - it uses it to allocate the buffer it then reads
	// the manifest into. The rest of `struct stat` is zeroed rather than invented: a mode or a
	// timestamp made up here is a fact somebody later relies on.
	// The layout the profile sysroot asserts in every C translation unit: fifty-six bytes, with the
	// mode at sixteen and the size at forty.
	#[repr(C)]
	struct Stat {
		device: u64,
		inode: u64,
		mode: u32,
		links: u64,
		user: u32,
		group: u32,
		size: i64,
		modified: i64,
	}
	// WRITTEN UNALIGNED, because the alignment of the caller's buffer is the caller's business and C
	// gives this function no way to check it. A real caller passes a `struct stat` and is aligned;
	// assuming that is how a substrate acquires an undefined behaviour it will never see fail.
	unsafe {
		core::ptr::write_unaligned(
			out as *mut Stat,
			Stat {
				device: 0,
				inode: descriptor as u64 + 1,
				// `S_IFREG`, because a manifest is a regular file as far as every check the loader makes
				// is concerned.
				mode: 0o100_000,
				links: 1,
				user: 0,
				group: 0,
				size: open.bytes.len() as i64,
				modified: 0,
			},
		);
	}
	0
}

/// Close every open manifest. Test support only.
#[cfg(test)]
pub fn reset() {
	let opens = &raw mut OPEN;
	for index in 0..MAX_OPEN {
		unsafe {
			(*opens)[index].in_use = false;
			(*opens)[index].bytes = &[];
			(*opens)[index].at = 0;
		}
	}
}
