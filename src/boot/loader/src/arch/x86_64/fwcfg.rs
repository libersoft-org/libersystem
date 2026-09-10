// QEMU firmware configuration (fw_cfg), the port interface, read by the loader for ONE file: the
// harness's DMA-mode record.
//
// The kernel has a reader of its own for the development profile scalar; this is the loader's, and
// it exists because the x86_64 UEFI path's harness input is `fw_cfg` and the loader is what relays
// it - validated, with `harness` provenance - into `BootInfo`. Reading a file does not consume it,
// so the kernel can revalidate the same bytes against what was relayed.
//
// Only the port interface. The selector is written little-endian, every multi-byte field the device
// returns is big-endian, and nothing here is trusted before the signature matches: on a machine
// without fw_cfg these ports read back as floating bus, so an absent device reports no file.

use core::arch::asm;

const SELECTOR_PORT: u16 = 0x510;
const DATA_PORT: u16 = 0x511;

const KEY_SIGNATURE: u16 = 0x0000;
const KEY_FILE_DIRECTORY: u16 = 0x0019;

// One directory entry: big-endian size and selector, two reserved bytes, then a NUL-padded name.
const ENTRY_BYTES: usize = 64;
const NAME_OFFSET: usize = 8;

// A directory this large means the signature matched by accident; stop rather than walk a length
// the device never wrote.
const MAX_ENTRIES: u32 = 256;

unsafe fn outw(port: u16, value: u16) {
	unsafe { asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags)) };
}

unsafe fn inb(port: u16) -> u8 {
	let value: u8;
	unsafe { asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags)) };
	value
}

unsafe fn select(key: u16) {
	unsafe { outw(SELECTOR_PORT, key) };
}

// Reads advance one shared cursor, so a selection is consumed in order.
unsafe fn read_bytes(out: &mut [u8]) {
	for byte in out.iter_mut() {
		*byte = unsafe { inb(DATA_PORT) };
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

// Copy the named file into `out`, answering how many bytes it holds - the file's OWN length, so a
// caller can tell a longer file from one that fits. At most `out.len()` bytes are copied.
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
		return Some(size);
	}
	None
}
