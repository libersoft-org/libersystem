// A `fscore::BlockDevice` over the firmware's Block I/O protocol, and the volume discovery
// built on it.
//
// This is the whole trick P02M0108 rests on: the split between loader and kernel is not which
// filesystem code runs, it is who supplies the block device. Before `ExitBootServices` the
// firmware does; afterwards `virtio_blk` does. LiberFS above that line is the same crate,
// unchanged, reading the same volume.
//
// Read-only by construction: `write_block` and `flush` are left to the trait's refusing
// defaults, and the protocol's write and reset entries are never called.

extern crate alloc;

use core::ffi::c_void;

use fscore::BlockDevice;

use crate::uefi::{self, BlockIo, BootServices, Handle};

// One firmware block device, already checked for media.
// The bounce buffer for a driver that demands an alignment the caller's buffer does not have: one
// filesystem block plus the slack to line it up. 8 KiB covers every block size this tree reads.
const BOUNCE_BYTES: usize = 8 * 1024;

#[derive(Clone, Copy)]
pub(crate) struct FirmwareDisk {
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

impl BlockDevice for FirmwareDisk {
	fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool {
		// `buf` is a filesystem block, which may span several device blocks. Refuse rather
		// than read a partial block: a short read that reports success would be handed to a
		// superblock parser as if it were data.
		let device_block = self.block_size as u64;
		if device_block == 0 || buf.len() as u64 % device_block != 0 {
			return false;
		}
		let span = buf.len() as u64 / device_block;
		let first = index * span;
		if first + span > self.last_block + 1 {
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

// A block device over an image ALREADY IN MEMORY.
//
// A live medium carries its system volume as a FILE on the boot filesystem, not as a partition, so
// no amount of block-device enumeration finds it - which is why a live boot fell back to the ESP's
// `init.pkg` for its bootstrap set while holding the volume that names it. The bytes are addressable
// either way; only the path to them differs.
pub(crate) struct ImageDisk {
	pub(crate) bytes: &'static [u8],
}

impl BlockDevice for ImageDisk {
	fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool {
		// Checked throughout: `index` comes from a filesystem parsing an image this loader did not
		// produce, so an offset past the end must refuse rather than wrap into the middle of it.
		let Some(offset) = index.checked_mul(buf.len() as u64) else { return false };
		let Ok(offset) = usize::try_from(offset) else { return false };
		let Some(end) = offset.checked_add(buf.len()) else { return false };
		if end > self.bytes.len() {
			return false;
		}
		buf.copy_from_slice(&self.bytes[offset..end]);
		true
	}
}

// Every block device the firmware knows about, in the order it reports them.
//
// `logical_partition` devices are included deliberately: on a GPT disk the firmware exposes both
// the whole disk and each partition, and the system volume is a partition. Skipping them would
// find nothing on exactly the layout `just img` produces.
pub(crate) fn each_disk(bs: *mut BootServices, mut visit: impl FnMut(FirmwareDisk) -> bool) {
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

// Assemble the bootstrap archive in memory from files on the system volume.
//
// This is what "retire init.pkg" means in practice. The archive stops being a build artifact and
// becomes a hand-off structure: the loader reads `etc/bootstrap.list` from the volume, reads each
// program it names, and packs them into exactly the format the kernel already unpacks. The kernel
// and SystemManager are untouched - they receive the same named blob they receive today - and the
// only thing that changed is that every one of those programs now also exists as a file the user
// can see, which is the whole point of the milestone.
//
// Each line of the list is `<archive entry name> <path on the volume>`. Both are needed: the
// kernel looks entries up by the name they have always had, which is not the path they now live
// at.
// Generic over the FILESYSTEM, not just the device. The same list is read three ways now: off a
// LiberFS partition on an installed system, out of a LiberFS image in memory on a live medium, and
// off the FAT boot filesystem when the system volume cannot be read at all. One mechanism, three
// places - which is the point. The recovery path used to be a packaged archive instead, so the
// same job was done twice by two different means, and only one of them put the programs somewhere
// a user could replace them.
pub(crate) trait ReadsFiles {
	fn read(&mut self, path: &[u8]) -> Option<alloc::vec::Vec<u8>>;
}

impl<D: BlockDevice> ReadsFiles for liberfs::LiberFs<D> {
	fn read(&mut self, path: &[u8]) -> Option<alloc::vec::Vec<u8>> {
		self.read_file(path).ok()
	}
}

impl<D: BlockDevice> ReadsFiles for fat::FatFs<D> {
	fn read(&mut self, path: &[u8]) -> Option<alloc::vec::Vec<u8>> {
		self.read_file(path).ok()
	}
}

pub(crate) fn assemble_bootstrap<F: ReadsFiles>(fs: &mut F) -> Option<alloc::vec::Vec<u8>> {
	use alloc::vec::Vec;

	// The list parser and the archive builder live in `abi`, beside the format's reader: the loader
	// is a UEFI binary, so nothing here can be tested on the host, and these two are exactly the
	// parts that need a test. What stays here is the reading.
	let list = fs.read(b"etc/bootstrap.list")?;
	let rows = abi::bootstrap::parse_list(&list)?;
	let mut blobs: Vec<Vec<u8>> = Vec::new();
	blobs.try_reserve_exact(rows.len()).ok()?;
	for row in &rows {
		// A named program that is not on the volume is fatal rather than skipped. The bootstrap
		// set is exactly the programs the system needs before its volume is readable, so a
		// missing one produces a machine that dies later and further away, with nothing to say
		// which program it was.
		blobs.push(fs.read(row.path)?);
	}
	let entries: Vec<(&[u8], &[u8])> = rows.iter().zip(&blobs).map(|(row, blob)| (row.name, blob.as_slice())).collect();
	abi::bootstrap::build_package(&entries)
}
