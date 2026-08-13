// An in-memory block device, and the bootstrap archive assembled off a filesystem above it.
//
// The FIRMWARE-backed block device and the enumeration of firmware disks moved to the `uefi` crate,
// where a mock firmware can drive them: everything they do is function pointers the loader cannot
// exercise from inside a UEFI binary. What stays here needs no firmware at all.

extern crate alloc;

use fscore::BlockDevice;

pub(crate) use uefi::disk::{FirmwareDisk, each_disk};

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
