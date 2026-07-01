// StorageService - a userspace service that resolves vol:// paths on a volume.
//
// The kernel loads this program from the init package into a ring-3 process and
// hands it a bootstrap channel. Over that channel it receives, in order:
//   1. the volume backing, one of:
//        "RAMDISK" + length, with a MemoryObject capability holding the volume's
//          PKGARCH1 archive - a read-only volume (the kernel's direct-client test
//          path); or
//        "BLOCK", with a channel capability to the virtio-blk driver's block
//          service, on which a writable on-disk filesystem (LiberFS) is mounted - the
//          boot path. A fresh or stale disk is formatted and seeded from the factory
//          archive laid at LBA 0, so the volume always starts with its seed files; or
//        "FATBLOCK", with a channel capability to a second virtio-blk driver's block
//          service, on which a writable FAT12/16/32 or exFAT volume is
//          mounted as vol://media - a flash-drive / SD-card image through the same
//          Volume contract;
//        "ISOBLOCK", with a channel capability to a third virtio-blk driver's block
//          service, on which a read-only ISO9660 volume is mounted as vol://iso - an
//          optical / install image through the same Volume contract;
//        "UDFBLOCK", with a channel capability to a fourth virtio-blk driver's block
//          service, on which a read-only UDF volume is mounted as vol://udf - a DVD /
//          Blu-ray image through the same Volume contract;
//        "USBBLOCK", with a channel capability to the xhci driver's block service
//          (a USB mass-storage stick over the Bulk-Only Transport), on which a
//          writable FAT volume is mounted as vol://usb - removable USB media
//          through the same Volume contract;
//   2. "SERVE", with a channel capability on which clients send requests.
// The service then serves the generated Storage.Volume contract: `open` resolves a
// vol:// path and replies with the file's length plus a MemoryObject capability to
// its bytes (handle<file>, a zero-copy read); `list` enumerates the volume; and on a
// writable LiberFS and FAT volume `write` creates-or-truncates a file from a
// zero-copy `buffer` and `remove` deletes one, both persisting to the disk so they
// survive a reboot. A read-only archive volume rejects writes with `denied`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use fat::FatFs;
use iso9660::Iso9660;
use liberfs::{BlockDevice, FsError, LiberFs};
use proto::codec::Buffer;
use proto::system::{volume, Error, FileInfo, FileKind, OpenOpts, OpenResult, SnapshotInfo};
use rt::*;
use udf::Udf;

// the volume names this service answers to; the URI's volume component must match
// one of these. "system" is the writable LiberFS disk; "media" is a writable FAT
// disk mounted off a second virtio-blk device; "iso" is a read-only ISO9660 disk
// mounted off a third virtio-blk device; "udf" is a read-only UDF disk mounted off a
// fourth virtio-blk device; "usb" is a writable FAT disk mounted off the xhci
// driver's USB mass-storage block service.
const SYSTEM_VOLUME: &[u8] = b"system";
const MEDIA_VOLUME: &[u8] = b"media";
const ISO_VOLUME: &[u8] = b"iso";
const UDF_VOLUME: &[u8] = b"udf";
const USB_VOLUME: &[u8] = b"usb";
// block-service protocol with driver.virtio-blk: request [op u32][lba u64][count u32]
// where op 0 = read, 1 = write. A read replies [status u32] carrying a MemoryObject
// of count*512 bytes; a write transfers a MemoryObject of count*512 bytes and replies
// [status u32]. A single request moves at most one DMA page (8 sectors).
const SECTOR_SIZE: usize = 512;
const MAX_SECTORS_PER_READ: usize = 8;
const OP_READ: u32 = 0;
const OP_WRITE: u32 = 1;

// LiberFS layout on the disk: the writable filesystem spans FS_BLOCKS filesystem blocks
// (one block = SECTORS_PER_BLOCK disk sectors) starting at FS_START_SECTOR - well past
// the factory archive at LBA 0, which the boot runner re-lays every boot and the
// filesystem never overwrites, so created files persist across reboots. The archive now
// carries the staged program binaries (M61 box 7), so it is a few megabytes: the FS
// starts 16 MiB in (past the archive) and the pool is 32 MiB, ample for the seeded ELFs.
const SECTORS_PER_BLOCK: u64 = (liberfs::BLOCK_SIZE / SECTOR_SIZE) as u64;
const FS_START_SECTOR: u64 = 32768;
const FS_BLOCKS: u64 = 8192;

// An ISO9660 logical block (2048 bytes) is this many 512-byte disk sectors; one read
// stays within a single DMA page (8 sectors).
const ISO_SECTORS: u64 = (iso9660::SECTOR_SIZE / SECTOR_SIZE) as u64;

// A UDF logical block (2048 bytes) is this many 512-byte disk sectors; one read stays
// within a single DMA page (8 sectors).
const UDF_SECTORS: u64 = (udf::SECTOR_SIZE / SECTOR_SIZE) as u64;

// An upper bound on a single write, so a bogus buffer length cannot make us allocate
// without limit; the filesystem enforces the real per-file maximum.
const MAX_WRITE: usize = 256 * 1024;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	// 1. volume backing: the legacy ramdisk archive (read-only, kernel test) or the
	//    virtio-blk disk mounted as a writable LiberFS (real boot).
	let mut vol: Volume = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if handle != 0 && len >= 7 + 8 && &buf[..7] == b"RAMDISK" => {
			let length: usize = u64::from_le_bytes([buf[7], buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14]]) as usize;
			let base: u64 = unsafe { syscall(SYS_MEMORY_MAP, handle, 0, 0, 0) };
			if sys_is_err(base) {
				exit();
			}
			Volume::Archive { base, len: length }
		}
		Received::Message { len, handle } if handle != 0 && len >= 5 && &buf[..5] == b"BLOCK" => match unsafe { mount_or_format(handle) } {
			Some(fs) => Volume::Disk(fs),
			None => exit(),
		},
		Received::Message { len, handle } if handle != 0 && len >= 8 && &buf[..8] == b"FATBLOCK" => match FatFs::mount(FatBlockDevice { chan: handle }) {
			Some(fs) => Volume::Fat(fs, MEDIA_VOLUME),
			None => exit(),
		},
		Received::Message { len, handle } if handle != 0 && len >= 8 && &buf[..8] == b"ISOBLOCK" => match Iso9660::mount(IsoBlockDevice { chan: handle }) {
			Some(fs) => Volume::Iso(fs),
			None => exit(),
		},
		Received::Message { len, handle } if handle != 0 && len >= 8 && &buf[..8] == b"UDFBLOCK" => match Udf::mount(UdfBlockDevice { chan: handle }) {
			Some(fs) => Volume::Udf(fs),
			None => exit(),
		},
		Received::Message { len, handle } if handle != 0 && len >= 8 && &buf[..8] == b"USBBLOCK" => match FatFs::mount(FatBlockDevice { chan: handle }) {
			Some(fs) => Volume::Fat(fs, USB_VOLUME),
			None => exit(),
		},
		_ => exit(),
	};
	// 2. service endpoint: clients reach the service here.
	let service: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if handle != 0 && len >= 5 && &buf[..5] == b"SERVE" => handle,
		_ => exit(),
	};
	// 3. report in over the bootstrap channel (the supervisor that started us is
	//    listening there), then serve generated volume requests until the client side
	//    closes.
	unsafe {
		send_blocking(bootstrap, b"StorageService: online", 0);
	}
	let mut reply: [u8; 2048] = [0u8; 2048];
	unsafe {
		serve_multi(service, &mut buf, &mut reply, |_chan: u64, req: &[u8], handle: u64, out: &mut [u8], reply_handle: &mut u64| -> Option<usize> { volume::dispatch(&mut vol, req, handle, out, reply_handle) });
	}
	exit();
}

// The volume backing, behind the generated Storage.Volume contract: either a
// read-only PKGARCH1 archive mapped in memory (the ramdisk path), a writable LiberFS
// on the virtio-blk disk (the boot path), or a writable FAT12/16/32/exFAT volume on
// a second virtio-blk disk (foreign media).
enum Volume {
	Archive { base: u64, len: usize },
	Disk(LiberFs<ChannelBlockDevice>),
	// The FAT backing serves two volumes - the virtio media disk and the USB stick -
	// so it carries the vol:// name it answers to.
	Fat(FatFs<FatBlockDevice>, &'static [u8]),
	Iso(Iso9660<IsoBlockDevice>),
	Udf(Udf<UdfBlockDevice>),
}

impl Volume {
	// The vol:// name this backing answers to: writable LiberFS (and the test archive)
	// is "system"; the writable FAT carries its name ("media" or "usb"); the read-only
	// ISO9660 is "iso"; the read-only UDF is "udf".
	fn name(&self) -> &'static [u8] {
		match self {
			Volume::Fat(_, name) => name,
			Volume::Iso(_) => ISO_VOLUME,
			Volume::Udf(_) => UDF_VOLUME,
			_ => SYSTEM_VOLUME,
		}
	}
}

impl volume::Service for Volume {
	// Resolve a vol:// path and hand back the file's bytes as a read-only shared
	// buffer (out-of-band handle<file>) plus its length - a zero-copy read.
	fn open(&mut self, o: OpenOpts) -> Result<OpenResult, Error> {
		// `open` is the read path; writes go through `write` / `remove`.
		if o.write || o.create {
			return Err(Error::Denied);
		}
		let target: VolumePath = VolumePath::parse(o.path.as_bytes()).ok_or(Error::NotFound)?;
		if target.volume != self.name() {
			return Err(Error::NotFound);
		}
		let name: &[u8] = target.path.as_bytes();
		match self {
			Volume::Archive { base, len } => {
				let archive: &[u8] = unsafe { core::slice::from_raw_parts(*base as *const u8, *len) };
				let file: &[u8] = Package::parse(archive).and_then(|p| p.lookup(name)).ok_or(Error::NotFound)?;
				let handle: u64 = unsafe { make_file_buffer(file) }.ok_or(Error::Again)?;
				Ok(OpenResult { file: handle, size: file.len() as u64 })
			}
			Volume::Disk(fs) => {
				let file: Vec<u8> = fs.read_file(name).map_err(map_fs_err)?;
				let handle: u64 = unsafe { make_file_buffer(&file) }.ok_or(Error::Again)?;
				Ok(OpenResult { file: handle, size: file.len() as u64 })
			}
			Volume::Fat(fs, _) => {
				let file: Vec<u8> = fs.read_file(name).map_err(map_fat_err)?;
				let handle: u64 = unsafe { make_file_buffer(&file) }.ok_or(Error::Again)?;
				Ok(OpenResult { file: handle, size: file.len() as u64 })
			}
			Volume::Iso(fs) => {
				let file: Vec<u8> = fs.read_file(name).map_err(map_iso_err)?;
				let handle: u64 = unsafe { make_file_buffer(&file) }.ok_or(Error::Again)?;
				Ok(OpenResult { file: handle, size: file.len() as u64 })
			}
			Volume::Udf(fs) => {
				let file: Vec<u8> = fs.read_file(name).map_err(map_udf_err)?;
				let handle: u64 = unsafe { make_file_buffer(&file) }.ok_or(Error::Again)?;
				Ok(OpenResult { file: handle, size: file.len() as u64 })
			}
		}
	}

	// List the directory named by a vol:// path (each entry as name + byte length +
	// kind), for `ls`. An empty subdirectory names the volume root.
	fn list(&mut self, path: String) -> Result<Vec<FileInfo>, Error> {
		let dir: &[u8] = self.list_dir_name(&path)?;
		match self {
			Volume::Archive { base, len } => {
				// the test archive is a flat package - it has no subdirectories.
				if !dir.is_empty() {
					return Err(Error::NotFound);
				}
				let archive: &[u8] = unsafe { core::slice::from_raw_parts(*base as *const u8, *len) };
				let package = Package::parse(archive).ok_or(Error::NotFound)?;
				let mut files: Vec<FileInfo> = Vec::new();
				for index in 0..package.len() {
					if let Some(name) = package.name(index) {
						let size: u64 = package.lookup(name).map(|b| b.len()).unwrap_or(0) as u64;
						files.push(file_info(name, size, false));
					}
				}
				Ok(files)
			}
			Volume::Disk(fs) => {
				let entries = if dir.is_empty() { fs.list() } else { fs.read_dir(dir) }.map_err(map_fs_err)?;
				Ok(entries.into_iter().map(|(name, size, is_dir)| file_info(&name, size, is_dir)).collect())
			}
			Volume::Fat(fs, _) => {
				let entries = if dir.is_empty() { fs.list() } else { fs.list_dir(dir) }.map_err(map_fat_err)?;
				Ok(entries.into_iter().map(|e| file_info(e.name.as_bytes(), e.size, e.is_dir)).collect())
			}
			Volume::Iso(fs) => {
				let entries = if dir.is_empty() { fs.list() } else { fs.list_dir(dir) }.map_err(map_iso_err)?;
				Ok(entries.into_iter().map(|e| file_info(e.name.as_bytes(), e.size, e.is_dir)).collect())
			}
			Volume::Udf(fs) => {
				let entries = if dir.is_empty() { fs.list() } else { fs.list_dir(dir) }.map_err(map_udf_err)?;
				Ok(entries.into_iter().map(|e| file_info(e.name.as_bytes(), e.size, e.is_dir)).collect())
			}
		}
	}

	// Create or overwrite a file from the zero-copy `data` buffer. The transferred
	// buffer handle is always consumed. A read-only volume refuses with `denied`.
	fn write(&mut self, path: String, data: Buffer) -> Result<(), Error> {
		// always release the transferred buffer handle, copying its bytes out first.
		let bytes: Option<Vec<u8>> = unsafe { read_buffer(&data) };
		let name: &[u8] = self.writable_name(&path)?;
		let bytes: Vec<u8> = bytes.ok_or(Error::Invalid)?;
		match self {
			Volume::Archive { .. } => Err(Error::Denied),
			Volume::Disk(fs) => fs.write_file(name, &bytes).map_err(map_fs_err),
			Volume::Fat(fs, _) => fs.write_file(name, &bytes).map_err(map_fat_err),
			Volume::Iso(_) => Err(Error::Invalid),
			Volume::Udf(_) => Err(Error::Invalid),
		}
	}

	// Delete a file. A read-only volume refuses with `denied`.
	fn remove(&mut self, path: String) -> Result<(), Error> {
		let name: &[u8] = self.writable_name(&path)?;
		match self {
			Volume::Archive { .. } => Err(Error::Denied),
			Volume::Disk(fs) => fs.remove(name).map_err(map_fs_err),
			Volume::Fat(fs, _) => fs.remove(name).map_err(map_fat_err),
			Volume::Iso(_) => Err(Error::Invalid),
			Volume::Udf(_) => Err(Error::Invalid),
		}
	}

	// Create a named read-only snapshot of the volume, pinning the current generation
	// so its blocks survive later writes. A read-only volume refuses with `denied`.
	fn snap_create(&mut self, name: String) -> Result<(), Error> {
		match self {
			Volume::Archive { .. } => Err(Error::Denied),
			Volume::Disk(fs) => fs.create_snapshot(name.as_bytes()).map_err(map_fs_err),
			Volume::Fat(..) => Err(Error::Denied),
			Volume::Iso(_) => Err(Error::Denied),
			Volume::Udf(_) => Err(Error::Denied),
		}
	}

	// List the volume's named snapshots (name + pinned generation), oldest first. A
	// read-only archive volume has none.
	fn snap_list(&mut self) -> Result<Vec<SnapshotInfo>, Error> {
		match self {
			Volume::Archive { .. } => Ok(Vec::new()),
			Volume::Disk(fs) => {
				let snaps = fs.list_snapshots().map_err(map_fs_err)?;
				Ok(snaps.into_iter().map(|(name, generation)| SnapshotInfo { name: String::from_utf8_lossy(&name).into_owned(), generation }).collect())
			}
			Volume::Fat(..) => Ok(Vec::new()),
			Volume::Iso(_) => Ok(Vec::new()),
			Volume::Udf(_) => Ok(Vec::new()),
		}
	}

	// Delete a named snapshot, releasing the blocks only it pinned. A read-only volume
	// refuses with `denied`.
	fn snap_delete(&mut self, name: String) -> Result<(), Error> {
		match self {
			Volume::Archive { .. } => Err(Error::Denied),
			Volume::Disk(fs) => fs.delete_snapshot(name.as_bytes()).map_err(map_fs_err),
			Volume::Fat(..) => Err(Error::Denied),
			Volume::Iso(_) => Err(Error::Denied),
			Volume::Udf(_) => Err(Error::Denied),
		}
	}

	// Resolve a vol:// path inside a named snapshot and hand back the file's bytes as a
	// read-only shared buffer (out-of-band handle<file>) plus its length - reading an
	// earlier state. A read-only archive volume has no snapshots.
	fn snap_open(&mut self, snapshot: String, path: String) -> Result<OpenResult, Error> {
		let name: &[u8] = self.writable_name(&path)?;
		match self {
			Volume::Archive { .. } => Err(Error::Denied),
			Volume::Disk(fs) => {
				// open a second, read-only view re-rooted at the snapshot over the same
				// block backing (the channel handle is shared, not consumed).
				let chan: u64 = fs.device().chan;
				let mut snap: LiberFs<ChannelBlockDevice> = LiberFs::mount_named_snapshot(ChannelBlockDevice { chan }, snapshot.as_bytes()).ok_or(Error::NotFound)?;
				let file: Vec<u8> = snap.read_file(name).map_err(map_fs_err)?;
				let handle: u64 = unsafe { make_file_buffer(&file) }.ok_or(Error::Again)?;
				Ok(OpenResult { file: handle, size: file.len() as u64 })
			}
			Volume::Fat(..) => Err(Error::Denied),
			Volume::Iso(_) => Err(Error::Denied),
			Volume::Udf(_) => Err(Error::Denied),
		}
	}

	// Create the directory at a vol:// path, plus any missing parents (mkdir -p). Only
	// the writable LiberFS volume supports it; the read-only archive refuses with
	// `denied`, the other backends with `invalid` (no directory writes implemented).
	fn mkdir(&mut self, path: String) -> Result<(), Error> {
		let name: &[u8] = self.writable_name(&path)?;
		match self {
			Volume::Archive { .. } => Err(Error::Denied),
			Volume::Disk(fs) => fs.mkdir(name).map_err(map_fs_err),
			Volume::Fat(..) => Err(Error::Invalid),
			Volume::Iso(_) => Err(Error::Invalid),
			Volume::Udf(_) => Err(Error::Invalid),
		}
	}

	// Remove the empty directory at a vol:// path. Only the writable LiberFS volume
	// supports it; the read-only archive refuses with `denied`, the other backends with
	// `invalid`.
	fn rmdir(&mut self, path: String) -> Result<(), Error> {
		let name: &[u8] = self.writable_name(&path)?;
		match self {
			Volume::Archive { .. } => Err(Error::Denied),
			Volume::Disk(fs) => fs.rmdir(name).map_err(map_fs_err),
			Volume::Fat(..) => Err(Error::Invalid),
			Volume::Iso(_) => Err(Error::Invalid),
			Volume::Udf(_) => Err(Error::Invalid),
		}
	}
}

impl Volume {
	// Validate a vol:// path for a mutating op and return the file name within the
	// volume. The name borrows `path`, so it outlives the call.
	fn writable_name<'a>(&self, path: &'a str) -> Result<&'a [u8], Error> {
		let target: VolumePath<'a> = VolumePath::parse(path.as_bytes()).ok_or(Error::NotFound)?;
		if target.volume != self.name() {
			return Err(Error::NotFound);
		}
		Ok(target.path.as_bytes())
	}

	// Validate a vol:// listing path and return the directory within the volume (empty
	// names the volume root, which `VolumePath::parse` rejects). A trailing slash is
	// tolerated so `vol://system/bin/` and `vol://system/bin` both name the same
	// directory.
	fn list_dir_name<'a>(&self, path: &'a str) -> Result<&'a [u8], Error> {
		const SCHEME: &[u8] = b"vol://";
		let rest: &[u8] = path.as_bytes().strip_prefix(SCHEME).ok_or(Error::NotFound)?;
		let (volume, sub): (&[u8], &[u8]) = match rest.iter().position(|&b: &u8| b == b'/') {
			Some(i) => (&rest[..i], &rest[i + 1..]),
			None => (rest, &[]),
		};
		if volume != self.name() {
			return Err(Error::NotFound);
		}
		Ok(sub.strip_suffix(b"/").unwrap_or(sub))
	}
}

// Build a listing entry from a raw name, byte length, and whether it is a directory.
fn file_info(name: &[u8], size: u64, is_dir: bool) -> FileInfo {
	FileInfo { name: String::from_utf8_lossy(name).into_owned(), size, kind: if is_dir { FileKind::Dir } else { FileKind::File } }
}

// Map an LiberFS error onto the Storage.Volume `error` enum.
fn map_fs_err(e: FsError) -> Error {
	match e {
		FsError::NotFound => Error::NotFound,
		FsError::NoSpace => Error::Again,
		FsError::TooLong | FsError::Invalid => Error::Invalid,
		// on-disk corruption caught by a block checksum: the data cannot be trusted.
		FsError::Corrupt => Error::Invalid,
		FsError::Io => Error::Again,
	}
}

// Map a FAT error onto the Storage.Volume `error` enum.
fn map_fat_err(e: fat::FsError) -> Error {
	match e {
		fat::FsError::NotFound => Error::NotFound,
		fat::FsError::NoSpace => Error::Again,
		fat::FsError::TooLong | fat::FsError::Invalid => Error::Invalid,
		fat::FsError::Io => Error::Again,
	}
}

// Map an ISO9660 error onto the Storage.Volume `error` enum.
fn map_iso_err(e: iso9660::FsError) -> Error {
	match e {
		iso9660::FsError::NotFound => Error::NotFound,
		iso9660::FsError::TooLong | iso9660::FsError::Invalid => Error::Invalid,
		iso9660::FsError::Io => Error::Again,
	}
}

// Map a UDF error onto the Storage.Volume `error` enum.
fn map_udf_err(e: udf::FsError) -> Error {
	match e {
		udf::FsError::NotFound => Error::NotFound,
		udf::FsError::TooLong | udf::FsError::Invalid => Error::Invalid,
		udf::FsError::Io => Error::Again,
	}
}

// Create a read-only shared buffer holding `file`'s bytes and return a transferable
// capability to it (read + map + transfer), or None on failure.
unsafe fn make_file_buffer(file: &[u8]) -> Option<u64> {
	unsafe {
		let buffer: u64 = syscall(SYS_MEMORY_OBJECT_CREATE, file.len() as u64, 0, 0, 0);
		if sys_is_err(buffer) {
			return None;
		}
		let mapped: u64 = match map_object(buffer) {
			Some(base) => base,
			None => {
				close(buffer);
				return None;
			}
		};
		core::ptr::copy_nonoverlapping(file.as_ptr(), mapped as *mut u8, file.len());
		unmap_object(buffer);
		// attenuate to read + map plus the transfer right, then drop the full handle.
		let granted: i64 = duplicate(buffer, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
		close(buffer);
		if granted < 0 {
			return None;
		}
		Some(granted as u64)
	}
}

// The virtio-blk disk as a block device for LiberFS: each LiberFS block maps to
// SECTORS_PER_BLOCK consecutive disk sectors, offset to the filesystem region at
// FS_START_SECTOR. Reads and writes go through the driver's block service on `chan`,
// which stays open for the life of the service.
struct ChannelBlockDevice {
	chan: u64,
}

impl BlockDevice for ChannelBlockDevice {
	fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool {
		let lba: u64 = FS_START_SECTOR + index * SECTORS_PER_BLOCK;
		unsafe { block_read(self.chan, lba, SECTORS_PER_BLOCK as u32, buf.as_mut_ptr()) }
	}

	fn write_block(&mut self, index: u64, buf: &[u8]) -> bool {
		let lba: u64 = FS_START_SECTOR + index * SECTORS_PER_BLOCK;
		unsafe { block_write(self.chan, lba, SECTORS_PER_BLOCK as u32, buf.as_ptr()) }
	}
}

// A second virtio-blk disk as a block device for the FAT backend: foreign media is
// addressed by absolute 512-byte LBA, so each FAT sector maps straight to one disk
// sector with no filesystem-region offset. Reads and writes go through the driver's
// block service on `chan`, which stays open for the life of the service.
struct FatBlockDevice {
	chan: u64,
}

impl fat::BlockDevice for FatBlockDevice {
	fn read_sector(&mut self, lba: u64, buf: &mut [u8]) -> bool {
		unsafe { block_read(self.chan, lba, 1, buf.as_mut_ptr()) }
	}

	fn write_sector(&mut self, lba: u64, buf: &[u8]) -> bool {
		unsafe { block_write(self.chan, lba, 1, buf.as_ptr()) }
	}
}

// A third virtio-blk disk as a block device for the ISO9660 backend: optical media is
// addressed by absolute 2048-byte logical block, so each block maps to ISO_SECTORS
// consecutive 512-byte disk sectors. Read-only, through the driver's block service on
// `chan`, which stays open for the life of the service.
struct IsoBlockDevice {
	chan: u64,
}

impl iso9660::BlockDevice for IsoBlockDevice {
	fn read_block(&mut self, lba: u64, buf: &mut [u8]) -> bool {
		let sector: u64 = lba * ISO_SECTORS;
		unsafe { block_read(self.chan, sector, ISO_SECTORS as u32, buf.as_mut_ptr()) }
	}
}

// A fourth virtio-blk disk as a block device for the UDF backend: DVD / Blu-ray media is
// addressed by absolute 2048-byte logical block, so each block maps to UDF_SECTORS
// consecutive 512-byte disk sectors. Read-only, through the driver's block service on
// `chan`, which stays open for the life of the service.
struct UdfBlockDevice {
	chan: u64,
}

impl udf::BlockDevice for UdfBlockDevice {
	fn read_block(&mut self, lba: u64, buf: &mut [u8]) -> bool {
		let sector: u64 = lba * UDF_SECTORS;
		unsafe { block_read(self.chan, sector, UDF_SECTORS as u32, buf.as_mut_ptr()) }
	}
}

// Mount the LiberFS on the virtio-blk disk, or, on a fresh or stale disk, format a new
// filesystem and seed it from the factory archive laid at LBA 0 so the volume always
// starts with its seed files. The block channel stays open for the serve loop.
unsafe fn mount_or_format(block_client: u64) -> Option<LiberFs<ChannelBlockDevice>> {
	// an existing filesystem (files persisted from a previous boot) mounts as-is.
	if let Some(fs) = LiberFs::mount(ChannelBlockDevice { chan: block_client }) {
		return Some(fs);
	}
	// otherwise lay down a fresh filesystem and copy in the factory seed files. The
	// device is rebuilt from the (Copy) channel handle - the failed mount consumed the
	// previous device value but left the channel open.
	let mut fs: LiberFs<ChannelBlockDevice> = LiberFs::format(ChannelBlockDevice { chan: block_client }, FS_BLOCKS).ok()?;
	if let Some(archive) = unsafe { read_seed_archive(block_client) } {
		seed_from_archive(&mut fs, &archive);
	}
	Some(fs)
}

// Copy every file from the factory PKGARCH1 archive into the filesystem (best effort;
// a file that does not fit is skipped).
fn seed_from_archive(fs: &mut LiberFs<ChannelBlockDevice>, archive: &[u8]) {
	let Some(package) = Package::parse(archive) else {
		return;
	};
	for index in 0..package.len() {
		if let Some(name) = package.name(index) {
			if let Some(bytes) = package.lookup(name) {
				let _ = fs.write_file(name, bytes);
			}
		}
	}
}

// Read the factory PKGARCH1 archive off the virtio-blk disk at LBA 0 into a Vec,
// leaving `block_client` open for the filesystem. The header + entry table may span
// several sectors now that the program binaries are staged, so the whole archive is read
// in page-sized (8-sector) chunks - each request moves at most one DMA page. Returns
// None if the disk holds no archive.
unsafe fn read_seed_archive(block_client: u64) -> Option<Vec<u8>> {
	unsafe {
		const PAGE: usize = SECTOR_SIZE * MAX_SECTORS_PER_READ;
		// `total` starts large enough to reach the entry count, grows to cover the whole
		// entry table, then becomes the end of the last blob.
		let mut archive: Vec<u8> = Vec::new();
		let mut total: usize = PKG_HEADER_LEN;
		let mut table_end: usize = 0;
		let mut filled: usize = 0;
		while filled < total {
			let want: usize = ((total + PAGE - 1) / PAGE) * PAGE;
			if archive.len() < want {
				archive.resize(want, 0);
			}
			let lba: u64 = (filled / SECTOR_SIZE) as u64;
			if !block_read(block_client, lba, MAX_SECTORS_PER_READ as u32, archive.as_mut_ptr().add(filled)) {
				return None;
			}
			filled += PAGE;
			if table_end == 0 {
				// the first page carries the magic and the entry count.
				if &archive[..PKG_MAGIC.len()] != PKG_MAGIC {
					return None;
				}
				let count: usize = u32::from_le_bytes([archive[8], archive[9], archive[10], archive[11]]) as usize;
				table_end = PKG_HEADER_LEN + PKG_ENTRY_LEN * count;
				total = table_end;
			}
			if total == table_end && filled >= table_end {
				// the whole entry table is in memory: the archive's total size is the end of
				// its last blob.
				let count: usize = (table_end - PKG_HEADER_LEN) / PKG_ENTRY_LEN;
				let mut end: usize = table_end;
				let mut i: usize = 0;
				while i < count {
					let e: usize = PKG_HEADER_LEN + PKG_ENTRY_LEN * i;
					let off: usize = u32::from_le_bytes([archive[e + PKG_NAME_LEN], archive[e + PKG_NAME_LEN + 1], archive[e + PKG_NAME_LEN + 2], archive[e + PKG_NAME_LEN + 3]]) as usize;
					let size: usize = u32::from_le_bytes([archive[e + PKG_NAME_LEN + 4], archive[e + PKG_NAME_LEN + 5], archive[e + PKG_NAME_LEN + 6], archive[e + PKG_NAME_LEN + 7]]) as usize;
					if off + size > end {
						end = off + size;
					}
					i += 1;
				}
				total = end;
			}
		}
		archive.truncate(total);
		Some(archive)
	}
}

// Copy the bytes behind a zero-copy `data` buffer out into a Vec and release the
// transferred buffer handle. Always consumes the handle. Returns None on failure or
// if the length exceeds MAX_WRITE.
unsafe fn read_buffer(data: &Buffer) -> Option<Vec<u8>> {
	unsafe {
		if data.handle == 0 {
			return None;
		}
		let len: usize = data.len as usize;
		if len > MAX_WRITE {
			close(data.handle);
			return None;
		}
		if len == 0 {
			close(data.handle);
			return Some(Vec::new());
		}
		let base: u64 = match map_object(data.handle) {
			Some(base) => base,
			None => {
				close(data.handle);
				return None;
			}
		};
		let mut bytes: Vec<u8> = alloc::vec![0u8; len];
		core::ptr::copy_nonoverlapping(base as *const u8, bytes.as_mut_ptr(), len);
		unmap_object(data.handle);
		close(data.handle);
		Some(bytes)
	}
}

// Send one block-read request [op=0][lba u64][count u32] to the driver and copy the
// returned sectors into `dst`. The reply is [status u32] carrying, on success, a
// MemoryObject of count*512 bytes which we map, copy out, and release. Returns true
// on success. `dst` must have room for count*512 bytes.
unsafe fn block_read(block_client: u64, lba: u64, count: u32, dst: *mut u8) -> bool {
	unsafe {
		let mut req: [u8; 16] = [0u8; 16];
		req[..4].copy_from_slice(&OP_READ.to_le_bytes());
		req[4..12].copy_from_slice(&lba.to_le_bytes());
		req[12..16].copy_from_slice(&count.to_le_bytes());
		if !send_blocking(block_client, &req, 0) {
			return false;
		}
		let mut rep: [u8; 16] = [0u8; 16];
		let (status, handle): (u32, u64) = match recv_blocking(block_client, &mut rep) {
			Received::Message { len, handle } if len >= 4 => (u32::from_le_bytes([rep[0], rep[1], rep[2], rep[3]]), handle),
			_ => return false,
		};
		if status != 0 || handle == 0 {
			if handle != 0 {
				close(handle);
			}
			return false;
		}
		let src: u64 = match map_object(handle) {
			Some(base) => base,
			None => {
				close(handle);
				return false;
			}
		};
		core::ptr::copy_nonoverlapping(src as *const u8, dst, count as usize * SECTOR_SIZE);
		unmap_object(handle);
		close(handle);
		true
	}
}

// Send one block-write request [op=1][lba u64][count u32] to the driver, transferring
// a freshly staged MemoryObject of count*512 bytes filled from `src`. The driver maps
// it, writes it to the disk, and closes it; the reply is [status u32]. Returns true on
// success. `src` must hold count*512 bytes.
unsafe fn block_write(block_client: u64, lba: u64, count: u32, src: *const u8) -> bool {
	unsafe {
		let bytes: usize = count as usize * SECTOR_SIZE;
		// stage the sectors in a fresh MemoryObject, then attenuate to a transferable
		// read+map handle (the driver only reads it).
		let obj: u64 = syscall(SYS_MEMORY_OBJECT_CREATE, bytes as u64, 0, 0, 0);
		if sys_is_err(obj) {
			return false;
		}
		let mapped: u64 = match map_object(obj) {
			Some(base) => base,
			None => {
				close(obj);
				return false;
			}
		};
		core::ptr::copy_nonoverlapping(src, mapped as *mut u8, bytes);
		unmap_object(obj);
		let granted: i64 = duplicate(obj, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
		close(obj);
		if granted < 0 {
			return false;
		}
		let mut req: [u8; 16] = [0u8; 16];
		req[..4].copy_from_slice(&OP_WRITE.to_le_bytes());
		req[4..12].copy_from_slice(&lba.to_le_bytes());
		req[12..16].copy_from_slice(&count.to_le_bytes());
		// send consumes the granted handle (transferred to the driver).
		if !send_blocking(block_client, &req, granted as u64) {
			return false;
		}
		let mut rep: [u8; 16] = [0u8; 16];
		match recv_blocking(block_client, &mut rep) {
			Received::Message { len, .. } if len >= 4 => u32::from_le_bytes([rep[0], rep[1], rep[2], rep[3]]) == 0,
			_ => false,
		}
	}
}
