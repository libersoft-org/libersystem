//! A `fscore::BlockDevice` over the firmware's Block I/O protocol, and the enumeration built on it.
//!
//! This is the whole trick the volume format rests on: the split between loader and kernel is not which
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
	// The firmware handle this device was found on.
	//
	// Kept so a caller can ask WHICH device this is rather than inferring it from what was read off
	// it. The loader needs exactly that: the medium it booted from is the one whose handle the
	// firmware named in `EFI_LOADED_IMAGE_PROTOCOL`, and identifying it by "the first FAT volume
	// that answered" is the same guess as taking the first LiberFS volume found - the guess this
	// tree's volume pairing exists to retire.
	handle: Handle,
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
	// The firmware handle this device was enumerated on.
	pub fn handle(&self) -> Handle {
		self.handle
	}

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

	// A CONTIGUOUS SPAN IS ONE FIRMWARE REQUEST.
	//
	// The trait's default loops `read_block`, and this protocol's entry point already takes a byte
	// count - so a filesystem asking for a whole cluster run was turned into one `ReadBlocks` call
	// per 512-byte sector on the way down. On an emulated ATAPI CD that is one command round-trip
	// per sector, and reading an 18 MB file off the boot medium meant thirty-five thousand of them.
	//
	// The bounce path stays per block on purpose: the aligned scratch buffer is one filesystem block
	// long, so a span that needs it cannot be moved in one piece, and refusing the read instead
	// would break exactly the firmware the bounce exists for.
	fn read_blocks(&mut self, index: u64, count: u64, buf: &mut [u8]) -> bool {
		if count == 0 {
			return true;
		}
		let device_block = self.block_size as u64;
		if device_block == 0 || buf.len() as u64 % count != 0 {
			return false;
		}
		let block = buf.len() as u64 / count;
		if block % device_block != 0 {
			return false;
		}
		if self.io_align > 1 && (buf.as_ptr() as usize) % self.io_align as usize != 0 {
			let block = block as usize;
			for i in 0..count as usize {
				if !self.read_block(index + i as u64, &mut buf[i * block..(i + 1) * block]) {
					return false;
				}
			}
			return true;
		}
		// The same arithmetic the single-block path checks, over the whole span: every number here
		// came from a filesystem parsing a medium this loader does not trust.
		let span = block / device_block;
		let Some(first) = index.checked_mul(span) else {
			return false;
		};
		let Some(total) = span.checked_mul(count) else {
			return false;
		};
		let Some(last) = first.checked_add(total) else {
			return false;
		};
		if last > self.last_block.saturating_add(1) {
			return false;
		}
		let status = unsafe { ((*self.proto).read_blocks)(self.proto, self.media_id, first, buf.len(), buf.as_mut_ptr() as *mut c_void) };
		!uefi::is_error(status)
	}
}

// The device the firmware loaded this image FROM, when it names one.
//
// `EFI_LOADED_IMAGE_PROTOCOL` carries it, so the boot medium is a fact the firmware states rather
// than something a loader has to deduce from what it manages to read. Firmware that mounts the ESP
// without exposing it as a block device names a handle with no Block I/O on it, and a caller that
// matches against the enumeration simply finds nothing - which is the honest answer there.
//
// # Safety
// `bs` must be the live `BootServices` table and `image_handle` the handle this image was entered
// with, before `ExitBootServices`.
pub unsafe fn loaded_image_device(bs: *mut BootServices, image_handle: Handle) -> Option<Handle> {
	let mut li: *mut c_void = core::ptr::null_mut();
	let status = unsafe { ((*bs).handle_protocol)(image_handle, &uefi::LOADED_IMAGE_PROTOCOL_GUID, &mut li) };
	if uefi::is_error(status) || li.is_null() {
		return None;
	}
	let device = unsafe { (*(li as *mut uefi::LoadedImage)).device_handle };
	if device.is_null() { None } else { Some(device) }
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
		if visit(FirmwareDisk { handle, proto, media_id, block_size, last_block, io_align }) {
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
// WHY NO VOLUME WAS CHOSEN, WHICH IS TWO DIFFERENT FACTS.
//
// This answered `Option`, so a disk whose superblock is CORRUPT, whose read FAILED, or whose UUID was
// altered was skipped exactly like a disk that is not a LiberFS volume at all - and exhaustion then
// read as "the paired volume is not in this machine", which is the one answer that lets policy fall
// back to a signed medium. So corrupting one superblock reached the fallback as though the disk were
// absent. LiberFS distinguishes six mount errors and all of them were erased here.
pub enum VolumeChoice<T> {
	Chosen(T),
	// Every disk was looked at and none carried the volume wanted. Absence.
	NotHere,
	// A disk answered as a LiberFS volume and FAILED - a corrupt superblock, an I/O error, or a
	// volume whose identity did not match after it had been opened. Present and broken.
	Failed,
}

// `open` answers `Err(())` for a disk that is a LiberFS volume and could not be used, `Ok(None)` for
// a disk that is not one, and `Ok(Some(..))` for a volume it opened.
pub unsafe fn choose_volume<T>(bs: *mut BootServices, want: Option<[u8; 16]>, mut open: impl FnMut(FirmwareDisk) -> Result<Option<([u8; 16], T)>, ()>) -> VolumeChoice<T> {
	let mut chosen: Option<T> = None;
	let mut failed = false;
	// WHETHER ANY LIBERFS VOLUME WAS HERE AT ALL, which decides what exhaustion MEANS.
	//
	// A volume that opens cleanly under a different identity used to be dismissed as somebody else's
	// disk and forgotten - so a walk that found LiberFS volumes and no match answered `NotHere`, the
	// same answer as a machine with no LiberFS volume on it, and the loader may fall back to the
	// signed boot medium from there. That turns "the selected volume's identity is wrong" into "the
	// selected volume is absent", which is exactly the substitution the exact-pairing rule exists to
	// refuse: change a superblock UUID, recompute the unauthenticated filesystem checksum, and a
	// present-invalid source becomes an absence with a fallback behind it.
	let mut saw_liberfs = false;
	unsafe {
		each_disk(bs, |device| {
			match open(device) {
				Err(()) => {
					// A VOLUME THAT IS HERE AND BROKEN IS REMEMBERED. The walk goes on - another disk
					// may carry the one wanted - and if none does, the answer is `Failed` rather than
					// `NotHere`, because something WAS there.
					failed = true;
					false
				}
				Ok(None) => false,
				Ok(Some((uuid, value))) => {
					saw_liberfs = true;
					if want.is_some_and(|want| want != uuid) {
						// A volume that opened cleanly and is not the one the pairing names is not a
						// failure ON ITS OWN: it is somebody else's disk, which is an ordinary thing
						// to find, and a later disk may still carry the one wanted. The walk goes on;
						// what changes is what happens when it ends without a match - see
						// `saw_liberfs` above.
						return false;
					}
					chosen = Some(value);
					true
				}
			}
		})
	};
	match chosen {
		Some(value) => VolumeChoice::Chosen(value),
		None if failed => VolumeChoice::Failed,
		// EXHAUSTION WITH A CANDIDATE PRESENT IS A FAILURE, NOT AN ABSENCE. Every disk was walked,
		// LiberFS volumes were found, and none of them is the one the pairing names. That is a
		// present source whose identity is wrong, which this milestone requires to end terminally
		// rather than in a fallback.
		None if saw_liberfs && want.is_some() => VolumeChoice::Failed,
		None => VolumeChoice::NotHere,
	}
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
