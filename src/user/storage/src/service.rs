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
use liberfs::{BlockDevice, FormatOpts, FsError, LiberFs};
use proto::codec::Buffer;
use proto::system::{Error, FileInfo, FileType, FsckReport, OpenOpts, OpenResult, SnapshotInfo, VolumeStatus, volume};
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
// [status u32]. The capacity reply carries the most sectors the driver moves per
// request, so this service sizes its requests to the driver it talks to -
// MAX_SECTORS_FALLBACK (one DMA page) stands in for an old driver whose capacity
// reply lacks the field.
const SECTOR_SIZE: usize = 512;
const MAX_SECTORS_FALLBACK: u32 = 8;
const OP_READ: u32 = 0;
const OP_WRITE: u32 = 1;
const OP_CAPACITY: u32 = 2;
const OP_FLUSH: u32 = 3;

// LiberFS layout on the disk: the writable filesystem starts at FS_START_SECTOR - well
// past the factory archive at LBA 0, which the boot runner re-lays every boot and the
// filesystem never overwrites, so created files persist across reboots. The archive
// carries the staged program binaries (M61 box 7), so the FS starts 16 MB in. The pool
// SIZE is derived from the disk's real capacity at mount/format time (the capacity
// query, M63); FS_BLOCKS is only the fallback pool for a disk that cannot report one.
const SECTORS_PER_BLOCK: u64 = (liberfs::BLOCK_SIZE / SECTOR_SIZE) as u64;
const FS_START_SECTOR: u64 = 32768;
const FS_BLOCKS: u64 = 8192;

// An ISO9660 logical block (2048 bytes) is this many 512-byte disk sectors; one
// logical block is one read.
const ISO_SECTORS: u64 = (iso9660::SECTOR_SIZE / SECTOR_SIZE) as u64;

// A UDF logical block (2048 bytes) is this many 512-byte disk sectors; one logical
// block is one read.
const UDF_SECTORS: u64 = (udf::SECTOR_SIZE / SECTOR_SIZE) as u64;

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
			Volume { fs: alloc::boxed::Box::new(ArchiveFs { base, len: length }) }
		}
		Received::Message { len, handle } if handle != 0 && len >= 5 && &buf[..5] == b"BLOCK" => match unsafe { mount_or_format(handle) } {
			Some(fs) => Volume { fs: alloc::boxed::Box::new(DiskFs { fs }) },
			None => exit(),
		},
		Received::Message { len, handle } if handle != 0 && len >= 8 && &buf[..8] == b"FATBLOCK" => Volume { fs: alloc::boxed::Box::new(FatBacking { chan: handle, name: MEDIA_VOLUME, fs: None }) },
		Received::Message { len, handle } if handle != 0 && len >= 8 && &buf[..8] == b"ISOBLOCK" => match Iso9660::mount(IsoBlockDevice { chan: handle }) {
			Some(fs) => Volume { fs: alloc::boxed::Box::new(IsoFs { fs }) },
			None => exit(),
		},
		Received::Message { len, handle } if handle != 0 && len >= 8 && &buf[..8] == b"UDFBLOCK" => match Udf::mount(UdfBlockDevice { chan: handle }) {
			Some(fs) => Volume { fs: alloc::boxed::Box::new(UdfFs { fs }) },
			None => exit(),
		},
		Received::Message { len, handle } if handle != 0 && len >= 8 && &buf[..8] == b"USBBLOCK" => Volume { fs: alloc::boxed::Box::new(FatBacking { chan: handle, name: USB_VOLUME, fs: None }) },
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
	let mut reply: [u8; 4096] = [0u8; 4096];
	unsafe {
		serve_multi(service, &mut buf, &mut reply, |chan: u64, req: &[u8], handle: u64, out: &mut [u8], reply_handle: &mut u64| -> Option<usize> {
			// stamp the wall clock onto the filesystem before each request, so a
			// mutation's inode timestamps carry real time (the RTC's Unix seconds; the
			// NTP-disciplined policy clock lives in TimeService). A no-op on the backends
			// that do not track it.
			vol.fs.set_clock(clock_rtc());
			// OP_LIST opens a stream (the log-tail model): the entries are framed one
			// by one onto a fresh sub-channel, so a big directory never has to fit one
			// reply. Everything else dispatches to a single reply.
			let op: u16 = if req.len() >= 2 { u16::from_le_bytes([req[0], req[1]]) } else { 0 };
			if op == volume::OP_LIST {
				stream_list(&mut vol, chan, req);
				return None;
			}
			volume::dispatch(&mut vol, req, handle, out, reply_handle)
		});
	}
	exit();
}

// Serve one OP_LIST request: decode it, gather the listing, then stream the entries
// to the client over a fresh sub-channel (the reply carries the correlation id and
// the consumer endpoint out-of-band; closing the producer marks end-of-stream). A
// bad path replies the correlation id with NO consumer handle - the generated
// client reads that as "no stream" - so an error stays distinguishable from an
// empty directory (`cd` validates paths this way).
fn stream_list(vol: &mut Volume, service: u64, request: &[u8]) {
	let mut reader = proto::codec::Reader::new(request);
	let r = &mut reader;
	let (corr, path): (u32, String) = match (|| Some((r.u16()?, r.u32()?, r.string_lp()?)))() {
		Some((_op, corr, path)) => (corr, path),
		None => return,
	};
	let corr_bytes: [u8; 4] = corr.to_le_bytes();
	let items: Vec<FileInfo> = match vol.list_entries(&path) {
		Ok(items) => items,
		Err(_) => {
			unsafe {
				send_blocking(service, &corr_bytes, 0);
			}
			return;
		}
	};
	let (producer, consumer): (u64, u64) = match unsafe { channel() } {
		Some(pair) => pair,
		None => return,
	};
	unsafe {
		send_blocking(service, &corr_bytes, consumer);
	}
	let mut frame: [u8; 1024] = [0u8; 1024];
	for (seq, item) in items.iter().enumerate() {
		if let Some(n) = volume::list_frame(seq as u32, item, &mut frame) {
			unsafe {
				send_blocking(producer, &frame[..n], 0);
			}
		}
	}
	unsafe {
		close(producer);
	}
}

// The volume backing, behind the generated Storage.Volume contract: either a
// read-only PKGARCH1 archive mapped in memory (the ramdisk path), a writable LiberFS
// on the virtio-blk disk (the boot path), or a writable FAT12/16/32/exFAT volume on
// a second virtio-blk disk (foreign media).
// A lazily mounted FAT backing over a block-service channel, serving removable
// media (the virtio media disk and the USB stick - both unpluggable). The
// filesystem mounts on first use and remounts after the media went away: an I/O
// failure drops the mount, so the next request probes the media afresh - the
// hot-plug behaviour a removable volume needs. An instance therefore reports
// online at boot whether or not media is present.
struct FatBacking {
	chan: u64,
	name: &'static [u8],
	fs: Option<FatFs<FatBlockDevice>>,
}

impl FatBacking {
	// Run `op` on the mounted filesystem (mounting on first use), dropping the mount
	// on an I/O failure - the media was unplugged - so the next request remounts.
	fn run<R>(&mut self, op: impl FnOnce(&mut FatFs<FatBlockDevice>) -> Result<R, FsError>) -> Result<R, Error> {
		if self.fs.is_none() {
			self.fs = FatFs::mount(FatBlockDevice { chan: self.chan });
		}
		let fs: &mut FatFs<FatBlockDevice> = self.fs.as_mut().ok_or(Error::NotFound)?;
		// stamp the wall clock so entries we write carry real timestamps (the same
		// RTC source the LiberFS volume is stamped with).
		fs.set_clock(unsafe { clock_rtc() });
		match op(fs) {
			Ok(r) => Ok(r),
			Err(FsError::Io) => {
				self.fs = None;
				Err(Error::Again)
			}
			Err(e) => Err(map_fs_err(e)),
		}
	}
}

// One mounted filesystem behind the volume service. Every backend - LiberFS, FAT,
// ISO9660, UDF and the boot archive - implements this, so the service dispatches each
// request through one trait call instead of a per-operation match over the backends, and
// adding a backend is one `impl` plus one mount arm. Read, list, capacity and status are
// the universal operations; the mutation and snapshot operations default to the read-only
// answer (a foreign or optical medium refuses them), so a read-only backend implements
// only the four read operations.
trait FileSystem {
	// The vol:// volume name this backend answers to.
	fn volume_name(&self) -> &'static [u8];
	// Stamp the wall clock before a mutation, so a written inode's timestamps carry real
	// time. Only the writable native filesystem tracks it; the default is a no-op.
	fn set_clock(&mut self, _unix_secs: u64) {}
	// Read a whole file by its in-volume path.
	fn read_file(&mut self, name: &[u8]) -> Result<Vec<u8>, Error>;
	// List a directory (an empty name is the volume root) as name + length + kind + times.
	fn list_entries(&mut self, dir: &[u8]) -> Result<Vec<FileInfo>, Error>;
	// The byte size of the backing block device (for the `lsblk` inventory).
	fn capacity(&mut self) -> Result<u64, Error>;
	// The filesystem's own identity and health numbers (for `lsvol` / `status`).
	fn status(&mut self) -> Result<VolumeStatus, Error>;

	// Mutations. A read-only medium refuses with `invalid` (it has no write path); the
	// boot archive overrides these to `denied` (a policy refusal, not a missing feature).
	fn write_file(&mut self, _name: &[u8], _data: &[u8]) -> Result<(), Error> {
		Err(Error::Invalid)
	}
	fn remove(&mut self, _name: &[u8]) -> Result<(), Error> {
		Err(Error::Invalid)
	}
	fn mkdir(&mut self, _name: &[u8]) -> Result<(), Error> {
		Err(Error::Invalid)
	}
	fn rmdir(&mut self, _name: &[u8]) -> Result<(), Error> {
		Err(Error::Invalid)
	}
	fn set_compression(&mut self, _enabled: bool) -> Result<(), Error> {
		Err(Error::Invalid)
	}
	fn fsck(&mut self) -> Result<FsckReport, Error> {
		Err(Error::Invalid)
	}
	fn restore(&mut self, _name: &[u8], _snapshot: &[u8]) -> Result<(), Error> {
		Err(Error::Invalid)
	}

	// Snapshots. Only the native filesystem pins generations; every other backend has
	// none, so create / delete / open refuse with `denied` and the list is empty.
	fn snap_create(&mut self, _name: &[u8]) -> Result<(), Error> {
		Err(Error::Denied)
	}
	fn snap_list(&mut self) -> Result<Vec<SnapshotInfo>, Error> {
		Ok(Vec::new())
	}
	fn snap_delete(&mut self, _name: &[u8]) -> Result<(), Error> {
		Err(Error::Denied)
	}
	fn snap_read_file(&mut self, _snapshot: &[u8], _name: &[u8]) -> Result<Vec<u8>, Error> {
		Err(Error::Denied)
	}
}

// The volume the service serves: one boxed filesystem backend behind the trait above.
struct Volume {
	fs: alloc::boxed::Box<dyn FileSystem>,
}

impl Volume {
	// The vol:// name this backing answers to (its backend's).
	fn name(&self) -> &'static [u8] {
		self.fs.volume_name()
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
		let file: Vec<u8> = self.fs.read_file(target.path.as_bytes())?;
		let handle: u64 = unsafe { make_file_buffer(&file) }.ok_or(Error::Again)?;
		Ok(OpenResult { file: handle, size: file.len() as u64 })
	}

	// List the directory named by a vol:// path (each entry as name + byte length +
	// kind), for `ls`. An empty subdirectory names the volume root. Streamed entry by
	// entry (the serve loop frames the vector onto a sub-channel), so a big directory
	// never has to fit one reply; a bad path is an empty stream.
	fn list(&mut self, path: String) -> Vec<FileInfo> {
		self.list_entries(&path).unwrap_or_default()
	}

	// Create or overwrite a file from the zero-copy `data` buffer. The transferred
	// buffer handle is always consumed. A read-only volume refuses with `denied`.
	fn write(&mut self, path: String, data: Buffer) -> Result<(), Error> {
		// always release the transferred buffer handle, copying its bytes out first.
		let bytes: Option<Vec<u8>> = unsafe { read_buffer(&data) };
		let name: &[u8] = self.writable_name(&path)?;
		let bytes: Vec<u8> = bytes.ok_or(Error::Invalid)?;
		self.fs.write_file(name, &bytes)
	}

	// The streaming write form: the file's bytes arrive as plain messages on the
	// transferred `data` channel (an empty message or the peer closing marks the
	// end), so a file's size is bounded by the filesystem, never by one transfer.
	// The channel handle is always consumed; the reply goes out once the whole
	// file is written.
	fn write_stream(&mut self, path: String, data: u64) -> Result<(), Error> {
		if data == 0 {
			return Err(Error::Invalid);
		}
		let mut bytes: Vec<u8> = Vec::new();
		loop {
			match unsafe { recv_vec_blocking(data) } {
				ReceivedVec::Message { bytes: chunk, .. } => {
					if chunk.is_empty() {
						break;
					}
					bytes.extend_from_slice(&chunk);
				}
				ReceivedVec::Closed => break,
			}
		}
		unsafe { close(data) };
		let name: &[u8] = self.writable_name(&path)?;
		self.fs.write_file(name, &bytes)
	}

	// Delete a file. A read-only volume refuses with `denied`.
	fn remove(&mut self, path: String) -> Result<(), Error> {
		let name: &[u8] = self.writable_name(&path)?;
		self.fs.remove(name)
	}

	// Create a named read-only snapshot of the volume, pinning the current generation
	// so its blocks survive later writes. A read-only volume refuses with `denied`.
	fn snap_create(&mut self, name: String) -> Result<(), Error> {
		self.fs.snap_create(name.as_bytes())
	}

	// List the volume's named snapshots (name + pinned generation), oldest first. A
	// read-only archive volume has none.
	fn snap_list(&mut self) -> Result<Vec<SnapshotInfo>, Error> {
		self.fs.snap_list()
	}

	// Delete a named snapshot, releasing the blocks only it pinned. A read-only volume
	// refuses with `denied`.
	fn snap_delete(&mut self, name: String) -> Result<(), Error> {
		self.fs.snap_delete(name.as_bytes())
	}

	// Resolve a vol:// path inside a named snapshot and hand back the file's bytes as a
	// read-only shared buffer (out-of-band handle<file>) plus its length - reading an
	// earlier state. A read-only archive volume has no snapshots.
	fn snap_open(&mut self, snapshot: String, path: String) -> Result<OpenResult, Error> {
		let name: &[u8] = self.writable_name(&path)?;
		let file: Vec<u8> = self.fs.snap_read_file(snapshot.as_bytes(), name)?;
		let handle: u64 = unsafe { make_file_buffer(&file) }.ok_or(Error::Again)?;
		Ok(OpenResult { file: handle, size: file.len() as u64 })
	}

	// Create the directory at a vol:// path, plus any missing parents (mkdir -p). Only
	// the writable LiberFS volume supports it; the read-only archive refuses with
	// `denied`, the other backends with `invalid` (no directory writes implemented).
	fn mkdir(&mut self, path: String) -> Result<(), Error> {
		let name: &[u8] = self.writable_name(&path)?;
		self.fs.mkdir(name)
	}

	// Remove the empty directory at a vol:// path. Only the writable LiberFS volume
	// supports it; the read-only archive refuses with `denied`, the other backends with
	// `invalid`.
	fn rmdir(&mut self, path: String) -> Result<(), Error> {
		let name: &[u8] = self.writable_name(&path)?;
		self.fs.rmdir(name)
	}

	// The size in bytes of the block device backing this volume - asked of the disk
	// over the block channel (op 2), not of the filesystem, so it answers even for a
	// lazily mounted removable volume. The memory-archive backing reports its own
	// length. For the `lsblk` inventory.
	fn capacity(&mut self) -> Result<u64, Error> {
		self.fs.capacity()
	}

	// The filesystem's own identity and health numbers: label, pool and free bytes,
	// the compression switch, whether the mount is read-only, and the filesystem's
	// name. Only the LiberFS volume tracks pool numbers; the foreign backends report
	// their filesystem name with zero bytes.
	fn status(&mut self) -> Result<VolumeStatus, Error> {
		self.fs.status()
	}

	// Switch transparent compression on or off for new writes on the LiberFS volume.
	fn set_compression(&mut self, enabled: bool) -> Result<(), Error> {
		self.fs.set_compression(enabled)
	}

	// Verify every live block of the LiberFS volume against its checksum and name the
	// damaged files.
	fn fsck(&mut self) -> Result<FsckReport, Error> {
		self.fs.fsck()
	}

	// Copy a file out of a named snapshot (or, with an empty name, the previous
	// generation) over the live file: the recovery verb for what `fsck` named.
	fn restore(&mut self, path: String, snapshot: String) -> Result<(), Error> {
		let name: &[u8] = self.writable_name(&path)?;
		self.fs.restore(name, snapshot.as_bytes())
	}
}

impl Volume {
	// The directory listing behind the `list` stream: each entry as name + byte
	// length + kind + timestamps. An empty subdirectory names the volume root.
	fn list_entries(&mut self, path: &str) -> Result<Vec<FileInfo>, Error> {
		let dir: &[u8] = self.list_dir_name(path)?;
		self.fs.list_entries(dir)
	}

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
fn file_info(name: &[u8], size: u64, is_dir: bool, mtime: u64, ctime: u64) -> FileInfo {
	FileInfo { name: String::from_utf8_lossy(name).into_owned(), size, r#type: if is_dir { FileType::Dir } else { FileType::File }, mtime, ctime }
}

// The native LiberFS backend: the full read-write filesystem with snapshots, compression
// and fsck. The one backend that implements every operation.
struct DiskFs {
	fs: LiberFs<ChannelBlockDevice>,
}

impl FileSystem for DiskFs {
	fn volume_name(&self) -> &'static [u8] {
		SYSTEM_VOLUME
	}
	fn set_clock(&mut self, unix_secs: u64) {
		self.fs.set_clock(unix_secs);
	}
	fn read_file(&mut self, name: &[u8]) -> Result<Vec<u8>, Error> {
		self.fs.read_file(name).map_err(map_fs_err)
	}
	fn list_entries(&mut self, dir: &[u8]) -> Result<Vec<FileInfo>, Error> {
		let entries = if dir.is_empty() { self.fs.list() } else { self.fs.read_dir(dir) }.map_err(map_fs_err)?;
		Ok(entries.into_iter().map(|(name, size, is_dir, mtime, ctime)| file_info(&name, size, is_dir, mtime, ctime)).collect())
	}
	fn capacity(&mut self) -> Result<u64, Error> {
		unsafe { block_capacity(self.fs.device().chan) }
	}
	fn status(&mut self) -> Result<VolumeStatus, Error> {
		let block: u64 = liberfs::BLOCK_SIZE as u64;
		Ok(VolumeStatus { label: String::from_utf8_lossy(self.fs.label()).into_owned(), total_bytes: self.fs.num_blocks() * block, free_bytes: self.fs.free_blocks() * block, compression: self.fs.compression(), read_only: self.fs.is_read_only(), filesystem: String::from("liberfs") })
	}
	fn write_file(&mut self, name: &[u8], data: &[u8]) -> Result<(), Error> {
		self.fs.write_file(name, data).map_err(map_fs_err)
	}
	fn remove(&mut self, name: &[u8]) -> Result<(), Error> {
		self.fs.remove(name).map_err(map_fs_err)
	}
	fn mkdir(&mut self, name: &[u8]) -> Result<(), Error> {
		self.fs.mkdir(name).map_err(map_fs_err)
	}
	fn rmdir(&mut self, name: &[u8]) -> Result<(), Error> {
		self.fs.rmdir(name).map_err(map_fs_err)
	}
	fn set_compression(&mut self, enabled: bool) -> Result<(), Error> {
		self.fs.set_compression(enabled).map_err(map_fs_err)
	}
	fn fsck(&mut self) -> Result<FsckReport, Error> {
		let report = self.fs.fsck().map_err(map_fs_err)?;
		Ok(FsckReport { checksum_failures: report.checksum_failures, damaged: report.damaged.iter().map(|p| String::from_utf8_lossy(p).into_owned()).collect() })
	}
	fn restore(&mut self, name: &[u8], snapshot: &[u8]) -> Result<(), Error> {
		self.fs.restore_file(name, snapshot).map_err(map_fs_err)
	}
	fn snap_create(&mut self, name: &[u8]) -> Result<(), Error> {
		self.fs.create_snapshot(name).map_err(map_fs_err)
	}
	fn snap_list(&mut self) -> Result<Vec<SnapshotInfo>, Error> {
		let snaps = self.fs.list_snapshots().map_err(map_fs_err)?;
		Ok(snaps.into_iter().map(|(name, generation)| SnapshotInfo { name: String::from_utf8_lossy(&name).into_owned(), generation }).collect())
	}
	fn snap_delete(&mut self, name: &[u8]) -> Result<(), Error> {
		self.fs.delete_snapshot(name).map_err(map_fs_err)
	}
	fn snap_read_file(&mut self, snapshot: &[u8], name: &[u8]) -> Result<Vec<u8>, Error> {
		// a cheap re-rooted read on the live mount - one table lookup plus the file's own
		// blocks, never a second mount or a volume walk.
		self.fs.read_file_from_snapshot(snapshot, name).map_err(map_fs_err)
	}
}

// The FAT / exFAT backend for foreign removable media: read-write (create, overwrite,
// delete files), but no directory writes, snapshots, compression or fsck - so it uses the
// trait defaults for those. Mounting is lazy and self-healing (see `FatBacking::run`).
impl FileSystem for FatBacking {
	fn volume_name(&self) -> &'static [u8] {
		self.name
	}
	fn read_file(&mut self, name: &[u8]) -> Result<Vec<u8>, Error> {
		self.run(|fs| fs.read_file(name))
	}
	fn list_entries(&mut self, dir: &[u8]) -> Result<Vec<FileInfo>, Error> {
		// the foreign backends do not surface timestamps yet: 0 renders as "-".
		let entries = self.run(|fs| if dir.is_empty() { fs.list() } else { fs.list_dir(dir) })?;
		Ok(entries.into_iter().map(|e| file_info(e.name.as_bytes(), e.size, e.is_dir, 0, 0)).collect())
	}
	fn capacity(&mut self) -> Result<u64, Error> {
		unsafe { block_capacity(self.chan) }
	}
	fn status(&mut self) -> Result<VolumeStatus, Error> {
		let (kind, total, free): (&'static str, u64, u64) = self.run(|fs| Ok((fs.kind_name(), fs.total_bytes(), fs.free_bytes()?)))?;
		Ok(VolumeStatus { label: String::new(), total_bytes: total, free_bytes: free, compression: false, read_only: false, filesystem: String::from(kind) })
	}
	fn write_file(&mut self, name: &[u8], data: &[u8]) -> Result<(), Error> {
		self.run(|fs| fs.write_file(name, data))
	}
	fn remove(&mut self, name: &[u8]) -> Result<(), Error> {
		self.run(|fs| fs.remove(name))
	}
}

// The read-only ISO9660 backend for optical and install media: read and list only, so it
// uses the trait defaults (which refuse writes and report no snapshots) for the rest.
struct IsoFs {
	fs: Iso9660<IsoBlockDevice>,
}

impl FileSystem for IsoFs {
	fn volume_name(&self) -> &'static [u8] {
		ISO_VOLUME
	}
	fn read_file(&mut self, name: &[u8]) -> Result<Vec<u8>, Error> {
		self.fs.read_file(name).map_err(map_fs_err)
	}
	fn list_entries(&mut self, dir: &[u8]) -> Result<Vec<FileInfo>, Error> {
		let entries = if dir.is_empty() { self.fs.list() } else { self.fs.list_dir(dir) }.map_err(map_fs_err)?;
		Ok(entries.into_iter().map(|e| file_info(e.name.as_bytes(), e.size, e.is_dir, 0, 0)).collect())
	}
	fn capacity(&mut self) -> Result<u64, Error> {
		unsafe { block_capacity(self.fs.device().chan) }
	}
	fn status(&mut self) -> Result<VolumeStatus, Error> {
		Ok(VolumeStatus { label: String::new(), total_bytes: self.fs.total_bytes(), free_bytes: 0, compression: false, read_only: true, filesystem: String::from("iso9660") })
	}
}

// The read-only UDF backend for optical media: read and list only, like ISO9660.
struct UdfFs {
	fs: Udf<UdfBlockDevice>,
}

impl FileSystem for UdfFs {
	fn volume_name(&self) -> &'static [u8] {
		UDF_VOLUME
	}
	fn read_file(&mut self, name: &[u8]) -> Result<Vec<u8>, Error> {
		self.fs.read_file(name).map_err(map_fs_err)
	}
	fn list_entries(&mut self, dir: &[u8]) -> Result<Vec<FileInfo>, Error> {
		let entries = if dir.is_empty() { self.fs.list() } else { self.fs.list_dir(dir) }.map_err(map_fs_err)?;
		Ok(entries.into_iter().map(|e| file_info(e.name.as_bytes(), e.size, e.is_dir, 0, 0)).collect())
	}
	fn capacity(&mut self) -> Result<u64, Error> {
		unsafe { block_capacity(self.fs.device().chan) }
	}
	fn status(&mut self) -> Result<VolumeStatus, Error> {
		Ok(VolumeStatus { label: String::new(), total_bytes: self.fs.total_bytes(), free_bytes: 0, compression: false, read_only: true, filesystem: String::from("udf") })
	}
}

// The boot archive backend: a read-only PKGARCH1 archive mapped in memory (the kernel
// test's ramdisk path), answering the "system" volume. It refuses every mutation with
// `denied` (a policy refusal - the archive is deliberately immutable), not `invalid`.
struct ArchiveFs {
	base: u64,
	len: usize,
}

impl ArchiveFs {
	// The mapped archive bytes.
	fn archive(&self) -> &[u8] {
		unsafe { core::slice::from_raw_parts(self.base as *const u8, self.len) }
	}
}

impl FileSystem for ArchiveFs {
	fn volume_name(&self) -> &'static [u8] {
		SYSTEM_VOLUME
	}
	fn read_file(&mut self, name: &[u8]) -> Result<Vec<u8>, Error> {
		let file: &[u8] = Package::parse(self.archive()).and_then(|p| p.lookup(name)).ok_or(Error::NotFound)?;
		Ok(file.to_vec())
	}
	fn list_entries(&mut self, dir: &[u8]) -> Result<Vec<FileInfo>, Error> {
		// the test archive is a flat package - it has no subdirectories.
		if !dir.is_empty() {
			return Err(Error::NotFound);
		}
		let package = Package::parse(self.archive()).ok_or(Error::NotFound)?;
		let mut files: Vec<FileInfo> = Vec::new();
		for index in 0..package.len() {
			if let Some(name) = package.name(index) {
				let size: u64 = package.lookup(name).map(|b| b.len()).unwrap_or(0) as u64;
				// the archive format carries no timestamps.
				files.push(file_info(name, size, false, 0, 0));
			}
		}
		Ok(files)
	}
	fn capacity(&mut self) -> Result<u64, Error> {
		Ok(self.len as u64)
	}
	fn status(&mut self) -> Result<VolumeStatus, Error> {
		Ok(VolumeStatus { label: String::new(), total_bytes: self.len as u64, free_bytes: 0, compression: false, read_only: true, filesystem: String::from("archive") })
	}
	fn write_file(&mut self, _name: &[u8], _data: &[u8]) -> Result<(), Error> {
		Err(Error::Denied)
	}
	fn remove(&mut self, _name: &[u8]) -> Result<(), Error> {
		Err(Error::Denied)
	}
	fn mkdir(&mut self, _name: &[u8]) -> Result<(), Error> {
		Err(Error::Denied)
	}
	fn rmdir(&mut self, _name: &[u8]) -> Result<(), Error> {
		Err(Error::Denied)
	}
	fn set_compression(&mut self, _enabled: bool) -> Result<(), Error> {
		Err(Error::Denied)
	}
	fn restore(&mut self, _name: &[u8], _snapshot: &[u8]) -> Result<(), Error> {
		Err(Error::Denied)
	}
}

// Map a filesystem error onto the Storage.Volume `error` enum. Every backend now
// reports through the one shared fs-core `FsError`, so this single mapping covers them
// all - LiberFS, FAT, ISO9660 and UDF alike.
fn map_fs_err(e: FsError) -> Error {
	match e {
		FsError::NotFound => Error::NotFound,
		FsError::NoSpace => Error::Again,
		// the malformed-request family: bad or overlong names, wrong kinds, a non-empty
		// directory, a duplicate snapshot name, an impossible operation.
		FsError::TooLong | FsError::BadName | FsError::IsDir | FsError::NotDir | FsError::NotEmpty | FsError::Exists | FsError::Invalid => Error::Invalid,
		// on-disk corruption caught by a block checksum: the data cannot be trusted.
		FsError::Corrupt => Error::Invalid,
		FsError::Io => Error::Again,
		// a read-only mount (a snapshot, or a volume degraded by a corrupt snapshot
		// table) refuses mutations, like any other read-only volume.
		FsError::ReadOnly => Error::Denied,
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
// SECTORS_PER_BLOCK consecutive disk sectors, offset to the volume's container -
// a GPT partition carrying the LiberFS type GUID when the disk has one, else the
// fixed filesystem region at FS_START_SECTOR. Access is bounded to the container:
// a block index at or past `limit` fails rather than reaching whatever lies beyond
// (another partition, or past the disk) - a hostile superblock claiming a bigger
// pool than the container is refused by the filesystem's own mount probe against
// this bound. Reads and writes go through the driver's block service on `chan`,
// which stays open for the life of the service.
struct ChannelBlockDevice {
	chan: u64,
	// The container's first 512-byte LBA: filesystem block 0 begins here.
	base: u64,
	// The container's size in filesystem blocks: the first index out of bounds.
	limit: u64,
	// The most sectors the driver moves per request (from its capacity reply);
	// `read_blocks` chunks a longer span by it.
	max_sectors: u32,
}

impl BlockDevice for ChannelBlockDevice {
	fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool {
		if index >= self.limit {
			return false;
		}
		let lba: u64 = self.base + index * SECTORS_PER_BLOCK;
		unsafe { block_read(self.chan, lba, SECTORS_PER_BLOCK as u32, buf.as_mut_ptr()) }
	}

	fn read_blocks(&mut self, index: u64, count: u64, buf: &mut [u8]) -> bool {
		if index + count > self.limit {
			return false;
		}
		// a contiguous extent run in as few requests as the driver's cap allows.
		let per: u64 = (self.max_sectors as u64 / SECTORS_PER_BLOCK).max(1);
		let mut done: u64 = 0;
		while done < count {
			let n: u64 = (count - done).min(per);
			let lba: u64 = self.base + (index + done) * SECTORS_PER_BLOCK;
			let dst: &mut [u8] = &mut buf[done as usize * liberfs::BLOCK_SIZE..];
			if !unsafe { block_read(self.chan, lba, (n * SECTORS_PER_BLOCK) as u32, dst.as_mut_ptr()) } {
				return false;
			}
			done += n;
		}
		true
	}

	fn write_block(&mut self, index: u64, buf: &[u8]) -> bool {
		if index >= self.limit {
			return false;
		}
		let lba: u64 = self.base + index * SECTORS_PER_BLOCK;
		unsafe { block_write(self.chan, lba, SECTORS_PER_BLOCK as u32, buf.as_ptr()) }
	}

	fn flush(&mut self) -> bool {
		unsafe { block_flush(self.chan) }
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
	fn read_block(&mut self, lba: u64, buf: &mut [u8]) -> bool {
		unsafe { block_read(self.chan, lba, 1, buf.as_mut_ptr()) }
	}

	fn write_block(&mut self, lba: u64, buf: &[u8]) -> bool {
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
// starts with its seed files. The volume's container is a GPT partition carrying the
// LiberFS type GUID when the disk has one (so a disk partitioned by another system
// mounts the same volume), else the fixed region past the factory archive; a fresh
// format spans the whole container. The block channel stays open for the serve loop.
unsafe fn mount_or_format(block_client: u64) -> Option<LiberFs<ChannelBlockDevice>> {
	let (base, pool): (u64, u64) = match unsafe { gpt_liberfs_partition(block_client) } {
		Some((first, last)) => (first, (last - first + 1) / SECTORS_PER_BLOCK),
		None => (FS_START_SECTOR, unsafe { disk_pool_blocks(block_client) }),
	};
	let max_sectors: u32 = unsafe { block_request_sectors(block_client) };
	// an existing filesystem (files persisted from a previous boot) mounts as-is, at
	// the size recorded in its superblock - never silently grown, the free map would
	// not match. A volume smaller than its container allows is reported.
	if let Some(fs) = LiberFs::mount(ChannelBlockDevice { chan: block_client, base, limit: pool, max_sectors }) {
		if fs.num_blocks() != pool {
			unsafe {
				print(b"storage: vol://system spans less than the disk allows (formatted earlier; online resize is future work)\n");
			}
		}
		if fs.is_read_only() {
			unsafe {
				print(b"storage: vol://system mounted READ-ONLY (damaged metadata or snapshot table; copy data off / restore, or reformat to write)\n");
			}
		}
		return Some(fs);
	}
	// otherwise lay down a fresh filesystem and copy in the factory seed files. The
	// device is rebuilt from the (Copy) channel handle - the failed mount consumed the
	// previous device value but left the channel open. The volume gets a uuid stirred
	// from the clocks (unique enough to tell volumes apart; no RNG exists yet) and the
	// "system" label; compression starts off, togglable later via `set-compression`.
	let uuid: [u8; 16] = unsafe { stir_uuid() };
	let opts: FormatOpts = FormatOpts { uuid, label: b"system".to_vec(), compress: false };
	let mut fs: LiberFs<ChannelBlockDevice> = LiberFs::format_opts(ChannelBlockDevice { chan: block_client, base, limit: pool, max_sectors }, pool, opts).ok()?;
	// stamp real time before seeding, so the factory files carry a real ctime/mtime.
	fs.set_clock(unsafe { clock_rtc() });
	if let Some(archive) = unsafe { read_seed_archive(block_client, max_sectors) } {
		seed_from_archive(&mut fs, &archive);
	}
	Some(fs)
}

// Sixteen uuid bytes stirred from the wall clock, the boot-relative nanosecond clock,
// and a fixed tag, mixed through a splitmix64 round each - distinct across formats,
// which is all the volume id needs (no RNG syscall exists yet).
unsafe fn stir_uuid() -> [u8; 16] {
	fn mix(mut x: u64) -> u64 {
		x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
		x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
		x ^ (x >> 31)
	}
	let a: u64 = mix(unsafe { clock_rtc() } ^ 0x4C69_6265_7246_5321);
	let b: u64 = mix(unsafe { clock_ns() } ^ a);
	let mut uuid = [0u8; 16];
	uuid[..8].copy_from_slice(&a.to_le_bytes());
	uuid[8..].copy_from_slice(&b.to_le_bytes());
	uuid
}

// The filesystem pool the disk's real capacity allows: everything past the factory
// archive region, in filesystem blocks - asked of the disk over the capacity query.
// Falls back to the fixed FS_BLOCKS pool when the disk cannot answer (or is too
// small for the layout), so an old driver still mounts something.
unsafe fn disk_pool_blocks(block_client: u64) -> u64 {
	let fs_start_bytes: u64 = FS_START_SECTOR * SECTOR_SIZE as u64;
	match unsafe { block_capacity(block_client) } {
		Ok(bytes) if bytes > fs_start_bytes + liberfs::BLOCK_SIZE as u64 => (bytes - fs_start_bytes) / liberfs::BLOCK_SIZE as u64,
		_ => FS_BLOCKS,
	}
}

// The LiberFS GPT partition type GUID, 4C424653-0001-4000-8000-4C6962657246
// ("LBFS" / "LiberF"), in its on-disk byte order (the first three groups
// little-endian, the rest as written). A disk partitioned by any other system marks
// a LiberFS volume with this GUID and the volume is found by it.
const LIBERFS_GUID_ON_DISK: [u8; 16] = [0x53, 0x46, 0x42, 0x4C, 0x01, 0x00, 0x00, 0x40, 0x80, 0x00, 0x4C, 0x69, 0x62, 0x65, 0x72, 0x46];

// The smallest partition worth mounting, in 512-byte sectors: 16 filesystem blocks
// (two superblock slots, the root leaf, and room to breathe). A GPT entry below this
// is ignored - the disk's content must never be able to kill the storage service by
// making the format fail.
const MIN_PARTITION_SECTORS: u64 = 16 * SECTORS_PER_BLOCK;

// Probe the disk for a GPT and return the first usable partition carrying the
// LiberFS type GUID as its (first LBA, last LBA), or None (no GPT, or no usable
// LiberFS partition - the fixed factory layout applies then). Reads the header at
// LBA 1 and walks the entry array it points at, one 8-sector page at a time. A
// malformed header (the GPT spec requires a power-of-two entry size >= 128) or a
// degenerate entry (an impossible or too-small span) is skipped, never trusted.
unsafe fn gpt_liberfs_partition(block_client: u64) -> Option<(u64, u64)> {
	unsafe {
		let mut header = [0u8; SECTOR_SIZE];
		if !block_read(block_client, 1, 1, header.as_mut_ptr()) {
			return None;
		}
		if &header[0..8] != b"EFI PART" {
			return None;
		}
		let entries_lba = u64::from_le_bytes(header[72..80].try_into().unwrap());
		let num_entries = u32::from_le_bytes(header[80..84].try_into().unwrap()) as usize;
		let entry_size = u32::from_le_bytes(header[84..88].try_into().unwrap()) as usize;
		if entry_size < 128 || entry_size > SECTOR_SIZE || !entry_size.is_power_of_two() || num_entries == 0 {
			return None;
		}
		// walk the entry array a page (8 sectors) at a time; a standard 128-entry,
		// 128-byte-entry array is 4 pages.
		const PAGE_SECTORS: usize = 8;
		let per_page: usize = PAGE_SECTORS * SECTOR_SIZE / entry_size;
		let mut page = [0u8; PAGE_SECTORS * SECTOR_SIZE];
		let mut index: usize = 0;
		while index < num_entries.min(512) {
			let lba = entries_lba + (index / per_page * PAGE_SECTORS) as u64;
			if !block_read(block_client, lba, PAGE_SECTORS as u32, page.as_mut_ptr()) {
				return None;
			}
			for slot in 0..per_page {
				if index >= num_entries {
					break;
				}
				let e = &page[slot * entry_size..slot * entry_size + entry_size];
				if e[0..16] == LIBERFS_GUID_ON_DISK {
					let first = u64::from_le_bytes(e[32..40].try_into().unwrap());
					let last = u64::from_le_bytes(e[40..48].try_into().unwrap());
					// a degenerate span is skipped, not fatal: keep scanning, another
					// entry may be the real volume.
					if first != 0 && last > first && last - first + 1 >= MIN_PARTITION_SECTORS {
						return Some((first, last));
					}
				}
				index += 1;
			}
		}
		None
	}
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
// several sectors now that the program binaries are staged, so the whole archive is
// read in chunks of the driver's own per-request cap (`max_sectors`). Returns
// None if the disk holds no archive. The header's claims (the entry count, each blob's
// end) are DISK CONTENT sizing an in-memory buffer, and this path runs exactly on a
// disk without a valid filesystem - the least trustworthy disk there is: every claim
// is bounded by the seed region's fixed size (the filesystem starts right past it),
// and a claim beyond it means "no archive", never an allocation.
unsafe fn read_seed_archive(block_client: u64, max_sectors: u32) -> Option<Vec<u8>> {
	unsafe {
		// the driver's per-request cap bounds each chunk, and 1 MB bounds the chunk
		// (a driver reporting no practical limit must not size this buffer).
		let chunk: usize = SECTOR_SIZE * max_sectors.min(2048) as usize;
		const SEED_REGION_BYTES: usize = FS_START_SECTOR as usize * SECTOR_SIZE;
		// `total` starts large enough to reach the entry count, grows to cover the whole
		// entry table, then becomes the end of the last blob.
		let mut archive: Vec<u8> = Vec::new();
		let mut total: usize = PKG_HEADER_LEN;
		let mut table_end: usize = 0;
		let mut filled: usize = 0;
		while filled < total {
			let want: usize = ((total + chunk - 1) / chunk) * chunk;
			if archive.len() < want {
				archive.resize(want, 0);
			}
			let lba: u64 = (filled / SECTOR_SIZE) as u64;
			if !block_read(block_client, lba, (chunk / SECTOR_SIZE) as u32, archive.as_mut_ptr().add(filled)) {
				return None;
			}
			filled += chunk;
			if table_end == 0 {
				// the first chunk carries the magic and the entry count.
				if &archive[..PKG_MAGIC.len()] != PKG_MAGIC {
					return None;
				}
				let count: usize = u32::from_le_bytes([archive[8], archive[9], archive[10], archive[11]]) as usize;
				let claimed: usize = PKG_HEADER_LEN + PKG_ENTRY_LEN * count;
				if claimed > SEED_REGION_BYTES {
					return None;
				}
				table_end = claimed;
				total = table_end;
			}
			if total == table_end && filled >= table_end {
				// the whole entry table is in memory: the archive's total size is the end of
				// its last blob - bounded by the seed region like every other claim.
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
				if end > SEED_REGION_BYTES {
					return None;
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
// if the claimed length exceeds the transferred object's real size.
unsafe fn read_buffer(data: &Buffer) -> Option<Vec<u8>> {
	unsafe {
		if data.handle == 0 {
			return None;
		}
		let len: usize = data.len as usize;
		// Bind the claimed length to the object the client actually transferred: the
		// kernel reports the memory object's real byte size, so a bogus length can
		// never make us allocate or copy beyond what the client backed with memory.
		let real: usize = match object_info(data.handle) {
			Some(info) => info.size as usize,
			None => 0,
		};
		if len > real {
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

// Send one capacity query [op=2][0 u64][0 u32] to the driver and return the disk's
// size in bytes. The reply is [status u32][capacity bytes u64][max sectors u32] (the
// trailing per-request cap is read by `block_request_sectors`). `again`
// when the driver (or its disk) cannot answer.
unsafe fn block_capacity(block_client: u64) -> Result<u64, Error> {
	unsafe {
		let mut req: [u8; 16] = [0u8; 16];
		req[..4].copy_from_slice(&OP_CAPACITY.to_le_bytes());
		if !send_blocking(block_client, &req, 0) {
			return Err(Error::Again);
		}
		let mut rep: [u8; 16] = [0u8; 16];
		match recv_blocking(block_client, &mut rep) {
			Received::Message { len, handle } if len >= 12 && handle == 0 && u32::from_le_bytes([rep[0], rep[1], rep[2], rep[3]]) == 0 => Ok(u64::from_le_bytes([rep[4], rep[5], rep[6], rep[7], rep[8], rep[9], rep[10], rep[11]])),
			_ => Err(Error::Again),
		}
	}
}

// Ask the driver how many sectors one request may move: the capacity reply's
// trailing [max sectors u32] field. MAX_SECTORS_FALLBACK (one DMA page) for a
// driver whose reply lacks the field, so an old driver still serves.
unsafe fn block_request_sectors(block_client: u64) -> u32 {
	unsafe {
		let mut req: [u8; 16] = [0u8; 16];
		req[..4].copy_from_slice(&OP_CAPACITY.to_le_bytes());
		if !send_blocking(block_client, &req, 0) {
			return MAX_SECTORS_FALLBACK;
		}
		let mut rep: [u8; 16] = [0u8; 16];
		match recv_blocking(block_client, &mut rep) {
			Received::Message { len, handle } if len >= 16 && handle == 0 && u32::from_le_bytes([rep[0], rep[1], rep[2], rep[3]]) == 0 => {
				let max: u32 = u32::from_le_bytes([rep[12], rep[13], rep[14], rep[15]]);
				if max == 0 { MAX_SECTORS_FALLBACK } else { max }
			}
			_ => MAX_SECTORS_FALLBACK,
		}
	}
}

// Send one flush request [op=3][0 u64][0 u32] to the driver: every write issued so
// far must reach the medium before any later one. The reply is [status u32]. LiberFS
// brackets its superblock commit with this barrier, so crash atomicity holds on a
// disk with a volatile write cache.
unsafe fn block_flush(block_client: u64) -> bool {
	unsafe {
		let mut req: [u8; 16] = [0u8; 16];
		req[..4].copy_from_slice(&OP_FLUSH.to_le_bytes());
		if !send_blocking(block_client, &req, 0) {
			return false;
		}
		let mut rep: [u8; 16] = [0u8; 16];
		match recv_blocking(block_client, &mut rep) {
			Received::Message { len, handle } if len >= 4 && handle == 0 => u32::from_le_bytes([rep[0], rep[1], rep[2], rep[3]]) == 0,
			_ => false,
		}
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
