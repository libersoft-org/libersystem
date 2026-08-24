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
// THE READING IS THE ONLY PART THAT NEEDS FIRMWARE. The policy above it - what a source that names
// programs and cannot be checked MEANS - lives in `abi::bootstrap`, beside the list parser and the
// archive writer, where a host can drive it. This crate keeps its own trait because the two
// filesystems are types it does not own, and the orphan rule does not let it implement somebody
// else's trait for them.
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

// Read a source's bootstrap set and say WHAT THE SOURCE IS.
//
// This used to answer `Option<Vec<u8>>`, and `None` meant two different things: "there is no
// bootstrap list here, try the next disk" and "this source was selected and its manifest refused
// it". A caller cannot tell those apart, so every caller did the same thing with both - it went on
// to the next source. That turns a detected tampering into a silent fallback, which is the opposite
// of what the check is for.
//
// The state machine is `abi::bootstrap::assemble`, beside the list parser it uses and the archive
// writer it ends with, because this is a UEFI binary and nothing inside one can be tested. What
// stays here is the reading and the reporting.
pub(crate) fn assemble_bootstrap<F: ReadsFiles>(fs: &mut F) -> abi::bootstrap::Selection {
	let verdict = abi::bootstrap::assemble(|path| fs.read(path), digests_ok);
	if matches!(verdict, abi::bootstrap::Selection::Verified(_)) {
		// SAY THAT IT CHECKED. A check that is silent when it passes cannot be told apart in a boot
		// log from one that was never wired up, and this one's whole value is that it ran.
		crate::arch::serial::write_str("loader: bootstrap set verified against etc/boot.manifest\n");
	}
	verdict
}

// Whether `bytes` is what the manifest says the file at `path` should be, with the line that says
// which way it failed. The checking itself is `bootproto::boot_manifest`, which is host-tested; what
// is here is the reporting, because this is a UEFI binary and nothing in one can be tested.
pub(crate) fn digests_ok(manifest: &[u8], path: &[u8], bytes: &[u8]) -> bool {
	match bootproto::boot_manifest::verify(manifest, path, bytes) {
		bootproto::boot_manifest::Verdict::Ok => true,
		bootproto::boot_manifest::Verdict::NotAManifest => {
			crate::arch::serial::write_str("loader: etc/boot.manifest is not a manifest this loader understands - refusing to boot\n");
			false
		}
		bootproto::boot_manifest::Verdict::NotNamed => {
			crate::arch::serial::write_str("loader: a boot file is not named in etc/boot.manifest - refusing to boot\n");
			false
		}
		bootproto::boot_manifest::Verdict::Mismatch => {
			crate::arch::serial::write_str("loader: a boot file does not match its digest in etc/boot.manifest - refusing to boot\n");
			false
		}
	}
}
