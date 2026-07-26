// QEMU firmware configuration (fw_cfg): the host's read-only key/value channel.
//
// The host names a boot profile here rather than in the boot protocol or the boot
// image, so selecting one changes no bytes the guest is built from: the same kernel,
// loader and system image boot with or without it. That is what lets a development
// instance be started without a rebuild, and it keeps the profile out of every
// artifact that a production boot would carry.
//
// Only the port interface is implemented. The selector is written little-endian, while
// every multi-byte field the device returns is big-endian. Nothing here is trusted
// before the signature matches: on a machine without fw_cfg these ports read back as
// floating bus, so an absent or foreign device simply reports no file.

use super::port;

// Port interface registers on x86.
const SELECTOR_PORT: u16 = 0x510;
const DATA_PORT: u16 = 0x511;

// Well-known selector keys.
const KEY_SIGNATURE: u16 = 0x0000;
const KEY_FILE_DIRECTORY: u16 = 0x0019;

// One directory entry: big-endian size and selector, two reserved bytes, then a
// NUL-padded name.
const ENTRY_BYTES: usize = 64;
const NAME_OFFSET: usize = 8;

// A directory this large means the signature matched by accident; stop rather than
// walk a length the device never wrote.
const MAX_ENTRIES: u32 = 256;

unsafe fn select(key: u16) {
	unsafe { port::outw(SELECTOR_PORT, key) };
}

// Reads advance one shared cursor, so callers must consume a selection in order.
unsafe fn read_bytes(out: &mut [u8]) {
	for byte in out.iter_mut() {
		*byte = unsafe { port::inb(DATA_PORT) };
	}
}

fn present() -> bool {
	let mut signature = [0u8; 4];
	unsafe {
		select(KEY_SIGNATURE);
		read_bytes(&mut signature);
	}
	&signature == b"QEMU"
}

// Copy the named file into `out`, returning how many bytes were written. A file longer
// than `out` is truncated: every caller here reads a short bounded name.
pub(crate) fn read_file(name: &[u8], out: &mut [u8]) -> Option<usize> {
	if !present() {
		return None;
	}
	let mut count = [0u8; 4];
	unsafe {
		select(KEY_FILE_DIRECTORY);
		read_bytes(&mut count);
	}
	let count = u32::from_be_bytes(count).min(MAX_ENTRIES);
	for _ in 0..count {
		let mut entry = [0u8; ENTRY_BYTES];
		unsafe { read_bytes(&mut entry) };
		let size = u32::from_be_bytes([entry[0], entry[1], entry[2], entry[3]]) as usize;
		let key = u16::from_be_bytes([entry[4], entry[5]]);
		let raw = &entry[NAME_OFFSET..];
		let end = raw.iter().position(|byte| *byte == 0).unwrap_or(raw.len());
		if &raw[..end] != name {
			continue;
		}
		let len = size.min(out.len());
		unsafe {
			select(key);
			read_bytes(&mut out[..len]);
		}
		return Some(len);
	}
	None
}
