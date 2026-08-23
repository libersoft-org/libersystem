//! A `fscore::BlockDevice` over the firmware's Block I/O protocol, and the enumeration built on it.
//!
//! This is the whole trick P02M0108 rests on: the split between loader and kernel is not which
//! filesystem code runs, it is who supplies the block device. Before `ExitBootServices` the
//! firmware does; afterwards `virtio_blk` does. LiberFS above that line is the same crate,
//! unchanged, reading the same volume.
//!
//! Read-only by construction: `write_block` and `flush` are left to the trait's refusing defaults,
//! and the protocol's write and reset entries are never called.

use core::ffi::c_void;

use fscore::BlockDevice;

use crate::{self as uefi, BlockIo, BootServices, Handle};

// One firmware block device, already checked for media.
// The bounce buffer for a driver that demands an alignment the caller's buffer does not have: one
// filesystem block plus the slack to line it up. 8 KiB covers every block size this tree reads.
const BOUNCE_BYTES: usize = 8 * 1024;

#[derive(Clone, Copy)]
pub struct FirmwareDisk {
	proto: *mut BlockIo,
	media_id: u32,
	block_size: u32,
	last_block: u64,
	// The buffer alignment this driver requires. `EFI_BLOCK_IO_MEDIA` carries it and this did not
	// keep it, so `read_blocks` was handed whatever address the caller's slice happened to have -
	// which the specification makes a REQUIREMENT, and which NVMe, SCSI and USB stacks on real
	// firmware may answer with `EFI_INVALID_PARAMETER`. OVMF does not care, which is why nothing
	// here has ever shown it.
	io_align: u32,
}

impl FirmwareDisk {
	// The geometry the firmware reported, for a caller deciding whether this device can hold the
	// volume it is looking for - and for a test asserting the enumeration reported what the firmware
	// said, in the order it said it.
	pub fn block_size(&self) -> u32 {
		self.block_size
	}

	pub fn last_block(&self) -> u64 {
		self.last_block
	}

	pub fn io_align(&self) -> u32 {
		self.io_align
	}
}

impl BlockDevice for FirmwareDisk {
	fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool {
		// `buf` is a filesystem block, which may span several device blocks. Refuse rather
		// than read a partial block: a short read that reports success would be handed to a
		// superblock parser as if it were data.
		let device_block = self.block_size as u64;
		if device_block == 0 || buf.len() as u64 % device_block != 0 {
			return false;
		}
		// CHECKED, like `ImageDisk` beside it. `index` comes from a filesystem parser reading an
		// image this loader is explicitly not trusting, and `index * span` then `first + span` are
		// two multiplications and an addition that can wrap. This path only reads, so a wrap cannot
		// corrupt memory - it hands the parser a block from the START of the device while reporting
		// success, which is worse for a parser than an error is.
		let span = buf.len() as u64 / device_block;
		let Some(first) = index.checked_mul(span) else {
			return false;
		};
		let Some(last) = first.checked_add(span) else {
			return false;
		};
		if last > self.last_block.saturating_add(1) {
			return false;
		}
		// BOUNCE THROUGH AN ALIGNED BUFFER when the driver demands one and the caller's is not.
		// `IoAlign` of 0 or 1 means no requirement, which is every case this tree has met.
		if self.io_align > 1 && (buf.as_ptr() as usize) % self.io_align as usize != 0 {
			let mut bounce = [0u8; BOUNCE_BYTES];
			if buf.len() > bounce.len() {
				return false;
			}
			// A stack array is aligned to its element, so line it up by hand inside a larger one.
			let base = bounce.as_mut_ptr() as usize;
			let offset = (self.io_align as usize - base % self.io_align as usize) % self.io_align as usize;
			if offset + buf.len() > bounce.len() {
				return false;
			}
			let aligned = unsafe { bounce.as_mut_ptr().add(offset) };
			let status = unsafe { ((*self.proto).read_blocks)(self.proto, self.media_id, first, buf.len(), aligned as *mut c_void) };
			if uefi::is_error(status) {
				return false;
			}
			unsafe { core::ptr::copy_nonoverlapping(aligned, buf.as_mut_ptr(), buf.len()) };
			return true;
		}
		let status = unsafe { ((*self.proto).read_blocks)(self.proto, self.media_id, first, buf.len(), buf.as_mut_ptr() as *mut c_void) };
		!uefi::is_error(status)
	}
}

// Every block device the firmware knows about, in the order it reports them.
//
// `logical_partition` devices are included deliberately: on a GPT disk the firmware exposes both
// the whole disk and each partition, and the system volume is a partition. Skipping them would
// find nothing on exactly the layout `./image.sh` produces.
//
// # Safety
// `bs` must be the live `BootServices` table, before `ExitBootServices`. Each `FirmwareDisk` the
// visitor is handed borrows firmware protocol pointers valid only for that call.
pub unsafe fn each_disk(bs: *mut BootServices, mut visit: impl FnMut(FirmwareDisk) -> bool) {
	let mut count: usize = 0;
	let mut handles: *mut Handle = core::ptr::null_mut();
	let status = unsafe { ((*bs).locate_handle_buffer)(uefi::BY_PROTOCOL, &uefi::BLOCK_IO_PROTOCOL_GUID, core::ptr::null_mut(), &mut count, &mut handles) };
	if uefi::is_error(status) || handles.is_null() {
		return;
	}
	for i in 0..count {
		let handle = unsafe { *handles.add(i) };
		let mut proto: *mut c_void = core::ptr::null_mut();
		let status = unsafe { ((*bs).handle_protocol)(handle, &uefi::BLOCK_IO_PROTOCOL_GUID, &mut proto) };
		if uefi::is_error(status) || proto.is_null() {
			continue;
		}
		let proto = proto as *mut BlockIo;
		let media = unsafe { (*proto).media };
		if media.is_null() {
			continue;
		}
		// A drive with no medium - an empty optical bay - answers every read with an error.
		// Asking it is not harmful, but skipping it keeps the diagnostics honest about what
		// was actually tried.
		let (present, media_id, block_size, last_block, io_align) = unsafe { ((*media).media_present, (*media).media_id, (*media).block_size, (*media).last_block, (*media).io_align) };
		if !present || block_size == 0 {
			continue;
		}
		if visit(FirmwareDisk { proto, media_id, block_size, last_block, io_align }) {
			break;
		}
	}
	// The buffer came from the firmware's pool and is ours to release.
	unsafe { ((*bs).free_pool)(handles as *mut c_void) };
}

// Walk the firmware's disks and hand back the FIRST ONE THE PAIRING ACCEPTS.
//
// THE ORDER IS THE FIRMWARE'S AND THE CHOICE IS NOT. Which handle comes first depends on
// enumeration, on which controller answered, on the machine - nothing about it is stable, so a
// loader that takes the first volume it recognises boots a different disk on the same hardware from
// one power cycle to the next. That is the finding this loader's milestone is named for, and the
// answer is that the boot medium NAMES the volume it belongs to: `want` is that name, and a volume
// that is not it is somebody else's system.
//
// `open` is given each disk in turn and answers with the volume's identity and whatever the caller
// wants to keep - a mounted filesystem, in the loader. It is called for every disk until one is
// accepted, so the work it does is the cheap half; the expensive half belongs after the choice.
//
// With no pairing (`want` is None) the first volume that opens wins, which is the single-disk case
// and the only one where "first" is a safe answer.
//
// # Safety
// `bs` must be the live `BootServices` table, before `ExitBootServices` - the same contract
// `each_disk` below it carries, and it was safe here while being unsafe there.
pub unsafe fn choose_volume<T>(bs: *mut BootServices, want: Option<[u8; 16]>, mut open: impl FnMut(FirmwareDisk) -> Option<([u8; 16], T)>) -> Option<T> {
	let mut chosen: Option<T> = None;
	unsafe {
		each_disk(bs, |device| {
			let Some((uuid, value)) = open(device) else { return false };
			if want.is_some_and(|want| want != uuid) {
				return false;
			}
			chosen = Some(value);
			true
		})
	};
	chosen
}

// The identity a boot medium's sidecar names, parsed from its hex text.
//
// THE OTHER HALF OF THE PAIRING. `choose_volume` decides which volume matches; this decides what
// "matches" means, and it is the half a malformed sidecar reaches first. Thirty-two hex digits, with
// dashes and whitespace ignored the way every UUID is written; anything else is NOT a pairing, and
// answering None is what makes the loader say so on the serial port and fall back to the first
// volume rather than pairing against a number it half read.
pub fn parse_pairing(bytes: &[u8]) -> Option<[u8; 16]> {
	let mut out = [0u8; 16];
	let mut nibbles = 0usize;
	for &b in bytes {
		if b == b'-' || b.is_ascii_whitespace() {
			continue;
		}
		let v = match b {
			b'0'..=b'9' => b - b'0',
			b'a'..=b'f' => b - b'a' + 10,
			b'A'..=b'F' => b - b'A' + 10,
			_ => return None,
		};
		if nibbles >= 32 {
			return None;
		}
		out[nibbles / 2] |= if nibbles % 2 == 0 { v << 4 } else { v };
		nibbles += 1;
	}
	if nibbles == 32 { Some(out) } else { None }
}
