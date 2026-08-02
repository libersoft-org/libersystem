// A `fscore::BlockDevice` over the firmware's Block I/O protocol, and the volume discovery
// built on it.
//
// This is the whole trick M0138 rests on: the split between loader and kernel is not which
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
pub(crate) struct FirmwareDisk {
	proto: *mut BlockIo,
	media_id: u32,
	block_size: u32,
	last_block: u64,
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
		let status = unsafe { ((*self.proto).read_blocks)(self.proto, self.media_id, first, buf.len(), buf.as_mut_ptr() as *mut c_void) };
		!uefi::is_error(status)
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
		let (present, media_id, block_size, last_block) = unsafe { ((*media).media_present, (*media).media_id, (*media).block_size, (*media).last_block) };
		if !present || block_size == 0 {
			continue;
		}
		if visit(FirmwareDisk { proto, media_id, block_size, last_block }) {
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
pub(crate) fn assemble_bootstrap(fs: &mut liberfs::LiberFs<FirmwareDisk>) -> Option<alloc::vec::Vec<u8>> {
	use abi::{PKG_ENTRY_LEN as ENTRY_LEN, PKG_HEADER_LEN as HEADER_LEN, PKG_NAME_LEN as NAME_LEN};
	use alloc::vec::Vec;

	let list = fs.read_file(b"etc/bootstrap.list").ok()?;
	let mut entries: Vec<(&[u8], Vec<u8>)> = Vec::new();
	for line in list.split(|&b| b == b'\n') {
		if line.is_empty() {
			continue;
		}
		let mut parts = line.splitn(2, |&b| b == b' ');
		let name = parts.next()?;
		let path = parts.next()?;
		if name.len() > NAME_LEN {
			return None;
		}
		// A named program that is not on the volume is fatal rather than skipped. The bootstrap
		// set is exactly the programs the system needs before its volume is readable, so a
		// missing one produces a machine that dies later and further away, with nothing to say
		// which program it was.
		let bytes = fs.read_file(path).ok()?;
		entries.push((name, bytes));
	}
	if entries.is_empty() {
		return None;
	}

	let table = HEADER_LEN + ENTRY_LEN * entries.len();
	let mut out: Vec<u8> = Vec::new();
	out.extend_from_slice(abi::PKG_MAGIC);
	out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
	out.extend_from_slice(&0u32.to_le_bytes());
	let mut offset = table;
	for (name, bytes) in &entries {
		let mut field = [0u8; NAME_LEN];
		field[..name.len()].copy_from_slice(name);
		out.extend_from_slice(&field);
		out.extend_from_slice(&(offset as u32).to_le_bytes());
		out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
		offset += bytes.len();
	}
	for (_, bytes) in &entries {
		out.extend_from_slice(bytes);
	}
	Some(out)
}
