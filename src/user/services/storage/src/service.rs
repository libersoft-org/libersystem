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
use libermemfs::{LiberMemFs, Policy as MemPolicy};
use proto::codec::Buffer;
use proto::codec::{Sink, SliceWriter};
use proto::system::{Error, FileInfo, FileType, FsckReport, OpenOpts, OpenResult, SnapshotInfo, VolumeStatus, volume, volume_admin};
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
const RAM_VOLUME: &[u8] = b"ram";
const TMP_VOLUME: &[u8] = b"tmp";
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

// LiberFS layout on the disk: the filesystem starts at LBA 0 of its container.
//
// It used to start 32 MiB in, to clear a factory archive laid at LBA 0 that the service formatted
// a fresh volume from. That archive is gone (M0138): the system volume is now built as a real
// filesystem, so the disk carries a volume rather than a package to make one out of, and there is
// nothing in front of it to skip. The container is a GPT partition with the LiberFS type GUID
// when the disk has one, and otherwise the whole device - which is what the loader also assumes,
// since firmware exposes a partition as its own block handle whose LBA 0 is the partition start.
//
// The pool SIZE is derived from the disk's real capacity at mount time; FS_BLOCKS is only the
// fallback for a disk that cannot report one.
const SECTORS_PER_BLOCK: u64 = (liberfs::BLOCK_SIZE / SECTOR_SIZE) as u64;
const FS_START_SECTOR: u64 = 0;
// How long a streamed write may go without a chunk before the service gives up on it, in the LAPIC
// ticks `wait` takes. Long enough that a slow but real sender is not punished, short enough that a
// silent one does not hold the service.
//
// Deliberately generous. This is not a latency budget - it is the point at which a sender is
// treated as gone, and punishing a slow-but-real client would be a worse failure than waiting a
// while for a hostile one. Against a client that never sends anything, any finite bound is the
// whole of the improvement.
// The LAPIC timer runs at 100 Hz, so a tick is 10 ms and this is thirty seconds.
const STREAM_IDLE_TICKS: u64 = 3_000;

// The most a whole stream may take, counted from the request rather than from the last chunk.
//
// The idle deadline alone bounds SILENCE, not slowness: it is rebuilt after every chunk, so a
// sender that emits one byte just before each window expires renews it forever and holds the
// serve loop for as long as it likes. Two deadlines are needed because they answer different
// questions - "has this sender gone away?" and "has this operation run long enough?" - and
// neither implies the other.
//
// Five minutes: long enough that a slow but genuine transfer of the largest stream this service
// accepts is not cut off, short enough that a client cannot own the service for an afternoon.
const STREAM_TOTAL_TICKS: u64 = 30_000;

// A stream that has sent this many chunks must be averaging at least `STREAM_MIN_CHUNK` bytes in
// each, or it is refused.
//
// The time-based deadlines bound how LONG a sender may hold the service. This bounds how much
// WORK it may cost per byte delivered, and that is what a slowloris actually is: a sender that is
// never idle and never finished, drip-feeding a byte at a time. Bounding the pattern rather than
// the clock also makes it checkable without one - the deadlines depend on real elapsed time, which
// no test here can fast-forward.
//
// The allowance is deliberately loose: 256 chunks before the rule applies at all, and 64 bytes
// average after that. A small file sent in small pieces never reaches the first, and a client
// sending 4 KiB chunks is 64x clear of the second.
const STREAM_CHUNK_GRACE: usize = 256;
const STREAM_MIN_CHUNK: usize = 64;
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
		// The two memory volumes. They carry no handle - there is no block device to hand over,
		// which is the whole point - and the capacity follows the tag as a decimal byte count.
		// A reserved volume takes its memory here, so a mount that cannot get it fails HERE
		// rather than at the first write.
		// A LIVE system volume: a LiberFS image handed over in memory, copied into a writable
		// memory volume. The medium it came from is read-only - an optical disc, or a stick nobody
		// wants written to - so the running system needs its own copy, and that copy disappears at
		// power off, which is what makes it a live session.
		//
		// This is seeding, which M0138 retired for disks, and the distinction is the point: on a
		// disk the archive was a SECOND copy of what the volume already held, and removing that
		// duplication is what the milestone is for. On read-only media there is no first copy.
		Received::Message { len, handle } if handle != 0 && len >= 7 && &buf[..7] == b"LIVEVOL" => match unsafe { live_volume(handle) } {
			Some(fs) => Volume { fs: alloc::boxed::Box::new(MemFs { fs, name: SYSTEM_VOLUME }) },
			None => exit(),
		},
		Received::Message { len, .. } if len >= 6 && &buf[..6] == b"RAMVOL" => match LiberMemFs::mount(MemPolicy::Reserved, mem_capacity(&buf[6..len])) {
			Ok(fs) => Volume { fs: alloc::boxed::Box::new(MemFs { fs, name: RAM_VOLUME }) },
			Err(_) => exit(),
		},
		Received::Message { len, .. } if len >= 6 && &buf[..6] == b"TMPVOL" => match LiberMemFs::mount(MemPolicy::Capped, mem_capacity(&buf[6..len])) {
			Ok(fs) => Volume { fs: alloc::boxed::Box::new(MemFs { fs, name: TMP_VOLUME }) },
			Err(_) => exit(),
		},
		_ => exit(),
	};
	// 2. an optional privileged admin endpoint precedes the public service endpoint.
	// Ordinary boots and focused scenarios that do not need scoped clients still send
	// SERVE directly and retain the existing bootstrap contract.
	let (admin, service): (u64, u64) = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle: admin_handle } if admin_handle != 0 && len >= 5 && &buf[..5] == b"ADMIN" => match unsafe { recv_blocking(bootstrap, &mut buf) } {
			Received::Message { len, handle } if handle != 0 && len >= 5 && &buf[..5] == b"SERVE" => (admin_handle, handle),
			_ => exit(),
		},
		Received::Message { len, handle } if handle != 0 && len >= 5 && &buf[..5] == b"SERVE" => (0, handle),
		_ => exit(),
	};
	// 3. report in over the bootstrap channel (the supervisor that started us is
	//    listening there), then serve generated volume requests until the client side
	//    closes.
	unsafe {
		send_blocking(bootstrap, b"StorageService: online", 0);
	}
	serve_volume(&mut vol, service, admin);
}

#[derive(Clone)]
enum Scope {
	Full,
	Directory(String),
}

struct Client {
	chan: u64,
	scope: Scope,
}

struct AdminCall<'a> {
	volume: &'a Volume,
	clients: &'a mut Vec<Client>,
}

impl volume_admin::Service for AdminCall<'_> {
	fn open_directory(&mut self, path: String) -> Result<u64, Error> {
		let scope: Scope = Scope::directory(self.volume, &path)?;
		let (server, client): (u64, u64) = unsafe { channel() }.ok_or(Error::Again)?;
		self.clients.push(Client { chan: server, scope });
		Ok(client)
	}
}

impl Scope {
	fn directory(volume: &Volume, path: &str) -> Result<Scope, Error> {
		if volume.name() != SYSTEM_VOLUME {
			return Err(Error::Denied);
		}
		let target: VolumePath = VolumePath::parse(path.as_bytes()).ok_or(Error::Invalid)?;
		if target.volume != volume.name() {
			return Err(Error::NotFound);
		}
		let directory: &str = core::str::from_utf8(target.path.as_bytes()).map_err(|_| Error::Invalid)?;
		Ok(Scope::Directory(String::from(directory)))
	}

	fn allows_path(&self, volume: &[u8], path: &str) -> bool {
		match self {
			Self::Full => true,
			Self::Directory(directory) => {
				let Some(target) = VolumePath::parse(path.as_bytes()) else { return false };
				target.volume == volume && (target.path.as_bytes() == directory.as_bytes() || target.path.as_bytes().strip_prefix(directory.as_bytes()).is_some_and(|rest| rest.starts_with(b"/")))
			}
		}
	}

	fn allows_request(&self, volume: &[u8], request: &[u8]) -> bool {
		if matches!(self, Self::Full) {
			return true;
		}
		let op: u16 = if request.len() >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
		match op {
			volume::OP_OPEN | volume::OP_LIST | volume::OP_WRITE | volume::OP_REMOVE | volume::OP_MKDIR | volume::OP_RMDIR | volume::OP_WRITE_STREAM => request_path(request).is_some_and(|path| self.allows_path(volume, path)),
			_ => false,
		}
	}
}

fn request_path(request: &[u8]) -> Option<&str> {
	if request.len() < 8 {
		return None;
	}
	let len: usize = u16::from_le_bytes([request[6], request[7]]) as usize;
	let end: usize = 8usize.checked_add(len)?;
	core::str::from_utf8(request.get(8..end)?).ok()
}

fn denied_reply(request: &[u8], reply: &mut [u8]) -> Option<usize> {
	if request.len() < 6 {
		return None;
	}
	let corr: u32 = u32::from_le_bytes([request[2], request[3], request[4], request[5]]);
	let mut writer = SliceWriter::new(reply);
	writer.u32(corr)?;
	writer.u8(0)?;
	Error::Denied.write(&mut writer)?;
	Some(writer.pos())
}

fn serve_volume(vol: &mut Volume, root: u64, mut admin: u64) -> ! {
	let mut clients: Vec<Client> = alloc::vec![Client { chan: root, scope: Scope::Full }];
	let mut request: [u8; 1024] = [0u8; 1024];
	let mut reply: [u8; 4096] = [0u8; 4096];
	loop {
		let mut waits: Vec<u64> = Vec::with_capacity(clients.len() + usize::from(admin != 0));
		if admin != 0 {
			waits.push(admin);
		}
		waits.extend(clients.iter().map(|client| client.chan));
		let ready: i64 = unsafe { wait_any(&waits, 0) };
		if ready < 0 {
			continue;
		}
		let chan: u64 = waits[ready as usize];
		if chan == admin {
			match unsafe { recv_blocking(admin, &mut request) } {
				Received::Message { len, handle } => {
					let mut reply_handle = proto::codec::Handles::new();
					let mut handle = if handle == 0 { proto::codec::Handles::new() } else { proto::codec::Handles::from_slice(&[handle]) };
					let mut call = AdminCall { volume: vol, clients: &mut clients };
					if let Some(reply_len) = volume_admin::dispatch(&mut call, &request[..len], &mut handle, &mut reply, &mut reply_handle) {
						if !unsafe { send_caps_blocking(admin, &reply[..reply_len], reply_handle.as_slice()) } {
							for &leftover in reply_handle.as_slice() {
								unsafe { close(leftover) };
							}
						}
					} else {
						for &leftover in reply_handle.as_slice() {
							unsafe { close(leftover) };
						}
					}
					for &unclaimed in handle.as_slice() {
						unsafe { close(unclaimed) };
					}
				}
				Received::Closed => admin = 0,
			}
			continue;
		}
		let Some(index) = clients.iter().position(|client| client.chan == chan) else { continue };
		let scope: Scope = clients[index].scope.clone();
		match unsafe { recv_blocking(chan, &mut request) } {
			Received::Message { len, .. } if len == 0 => {
				if index == 0 {
					exit();
				}
				unsafe { close(chan) };
				clients.swap_remove(index);
			}
			Received::Message { len, handle } => {
				let mut handle = if handle == 0 { proto::codec::Handles::new() } else { proto::codec::Handles::from_slice(&[handle]) };
				let op: u16 = if len >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
				if op == HEARTBEAT_OP {
					unsafe { send_blocking(chan, b"PONG", 0) };
				} else if op == CONNECT_OP {
					if let Some((server, client)) = unsafe { channel() } {
						clients.push(Client { chan: server, scope });
						unsafe { send_blocking(chan, &[], client) };
					} else {
						unsafe { send_blocking(chan, &[], 0) };
					}
				} else {
					// Stamp mutations before authorization and dispatch. The clock is a no-op on
					// read-only backends, while denied requests never reach their filesystem.
					vol.fs.set_clock(unsafe { clock_rtc() });
					if op == volume::OP_LIST {
						stream_list(vol, chan, &scope, &request[..len], &mut handle);
					} else {
						let mut reply_handle = proto::codec::Handles::new();
						let reply_len: Option<usize> = if scope.allows_request(vol.name(), &request[..len]) { volume::dispatch(vol, &request[..len], &mut handle, &mut reply, &mut reply_handle) } else { denied_reply(&request[..len], &mut reply) };
						if let Some(reply_len) = reply_len {
							if !unsafe { send_caps_blocking(chan, &reply[..reply_len], reply_handle.as_slice()) } {
								for &leftover in reply_handle.as_slice() {
									unsafe { close(leftover) };
								}
							}
						} else {
							for &leftover in reply_handle.as_slice() {
								unsafe { close(leftover) };
							}
						}
					}
				}
				for &unclaimed in handle.as_slice() {
					unsafe { close(unclaimed) };
				}
			}
			Received::Closed => {
				if index == 0 {
					exit();
				}
				unsafe { close(chan) };
				clients.swap_remove(index);
			}
		}
	}
}

// Serve one OP_LIST request: decode it, gather the listing, then stream the entries
// to the client over a fresh sub-channel (the reply carries the correlation id and
// the consumer endpoint out-of-band; closing the producer marks end-of-stream). A
// bad path replies the correlation id with NO consumer handle - the generated
// client reads that as "no stream" - so an error stays distinguishable from an
// empty directory (`cd` validates paths this way).
fn stream_list(vol: &mut Volume, service: u64, scope: &Scope, request: &[u8], request_handle: &mut proto::codec::Handles) {
	let mut reader = proto::codec::Reader::with_handle_list(request, request_handle);
	let r = &mut reader;
	let (corr, path): (u32, String) = match (|| Some((r.u16()?, r.u32()?, r.string_lp()?)))() {
		Some((_op, corr, path)) => (corr, path),
		None => return,
	};
	if r.has_handle() {
		return;
	}
	request_handle.clear();
	let corr_bytes: [u8; 4] = corr.to_le_bytes();
	if !scope.allows_path(vol.name(), &path) {
		unsafe {
			send_blocking(service, &corr_bytes, 0);
		}
		return;
	}
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
	// CHECKED. If the client closed the main channel just after asking, the consumer handle is
	// never delivered - and it was previously never closed either, so it leaked AND left the
	// producer with a live peer nobody would ever read. The next send then blocked forever, which
	// is a permanent stop of the whole service caused by a client merely hanging up.
	unsafe {
		if !send_blocking(service, &corr_bytes, consumer) {
			close(consumer);
			close(producer);
			return;
		}
	}
	let mut frame: [u8; 1024] = [0u8; 1024];
	for (seq, item) in items.iter().enumerate() {
		let mut frame_handle: u64 = 0;
		if let Some(n) = volume::list_frame(seq as u32, item, &mut frame, &mut frame_handle) {
			// BOUNDED. A client that takes the consumer handle and then stops reading fills this
			// queue, and an unbounded send held the entire service on it - the same defect the
			// inbound stream had, in the direction that had no bound at all. Abandoning a stalled
			// listing truncates it for that client; blocking truncates it for every client.
			let outcome = unsafe { send_deadline(producer, &frame[..n], frame_handle, clock().saturating_add(STREAM_IDLE_TICKS)) };
			match outcome {
				SendOutcome::Delivered => {}
				SendOutcome::Failed | SendOutcome::Stalled => {
					if frame_handle != 0 {
						unsafe { close(frame_handle) };
					}
					break;
				}
			}
		} else if frame_handle != 0 {
			unsafe { close(frame_handle) };
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
// What a filesystem says about writing to one path, BEFORE a byte is accepted.
enum WritePlan {
	// The path may be written. `max_len` is a real ceiling from this filesystem, or `None` when it
	// has none to give before the write is attempted.
	Allowed { max_len: Option<usize> },
	// It may not, and this is why: a read-only medium, a policy refusal, a missing parent, a
	// directory in the way. Returned before the caller allocates anything for the payload.
	Refused(Error),
}

// The most a STREAM may accumulate when the filesystem gives no ceiling of its own.
//
// This is the receiver's policy and nothing else. It is not a filesystem limit and not a protocol
// limit - an ordinary write to LiberFS may legitimately exceed it, because the payload already
// exists and the filesystem decides. It exists because a stream must be bounded by SOMETHING
// before it accepts the first byte, and the sender chooses how much to send.
//
// It cannot currently be stated in the contract: LSIDL has no constant declarations, so putting
// this number in `idl/*.lsidl` needs generator work. Until then a client learns it by being
// refused, which is the honest description of where it lives.
const STREAM_ACCUMULATION: usize = 64 * 1024 * 1024;

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

	// Write a file from a buffer the caller hands over.
	//
	// A streamed write has already built the whole file in memory, so copying it again into the
	// backend doubles the peak for no reason - and on a reserved memory volume it is the
	// difference between accepting a file and refusing it. Backends that must copy anyway (a
	// disk) simply borrow the slice; the memory backend adopts the allocation.
	fn write_file_owned(&mut self, name: &[u8], data: Vec<u8>) -> Result<(), Error> {
		self.write_file(name, &data)
	}

	// Whether this path may be written at all, and how much a single write may carry.
	//
	// Asked of the path rather than the volume, because a bound taken from the volume is wrong in
	// both directions: it refuses a rewrite of a file that already holds the space it needs, and
	// it accepts a new file whose name then does not fit.
	//
	// The point of a PLAN rather than a length is that a refusal is an answer. The previous shape
	// returned `Option<Result<usize, Error>>`, where `None` meant three different things at once -
	// "no cheap limit", "read-only medium" and "cannot validate the path yet" - so a stream to an
	// ISO volume accepted up to the caller's ceiling before anything refused it, and the comment
	// claiming the destination was validated first held for one backend only.
	//
	// `max_len: None` means this filesystem has no ceiling to give before the write: it is bounded
	// by the medium at commit time. It is NOT permission to invent one - see `STREAM_ACCUMULATION`.
	//
	// The default is a refusal, matching the default mutations below: a backend that has not
	// implemented writing must not read as writable-with-unknown-limit.
	fn write_plan(&mut self, _name: &[u8]) -> WritePlan {
		WritePlan::Refused(Error::Invalid)
	}
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

impl Volume {
	// Collect a streamed write, bounded at every step.
	fn receive_stream(&mut self, data: u64, path: &str) -> Result<Vec<u8>, Error> {
		// Validated before a byte is accepted, and against what a single write may actually be -
		// not the volume's whole capacity, which on a disk backend is the size of the disk. A
		// stream to a path that cannot exist should cost a parse, not a heap.
		// Validates the destination as a side effect: a stream to a missing parent, to a file used
		// as a directory, or to a directory is refused HERE rather than after the whole file has
		// been held in memory.
		// Validates the destination as a side effect: a stream to a missing parent, to a file used
		// as a directory, or to a directory is refused HERE rather than after the whole file has
		// been held in memory.
		// A backend that answers for the path gives the real ceiling; one that does not gets the
		// per-file maximum, because a stream must be bounded by SOMETHING before it accepts a byte
		// - unlike an ordinary write, where the whole payload already exists and the backend can
		// simply refuse it.
		// The ceiling AND where it came from. Exceeding the two means different things to a client:
		// a filesystem's ceiling is the space it has, so a later attempt may fit, while
		// `STREAM_ACCUMULATION` is this service's own policy and no amount of waiting changes it.
		// Reporting both as `Again` told a client to retry something that can only fail.
		let (limit, limit_is_policy): (usize, bool) = {
			let name: &[u8] = self.writable_name(path)?;
			match self.fs.write_plan(name) {
				// A read-only volume, a missing parent, a directory in the way: refused HERE,
				// before a byte is accepted, which is what the plan exists for.
				WritePlan::Refused(e) => return Err(e),
				WritePlan::Allowed { max_len: Some(max) } => (max, false),
				WritePlan::Allowed { max_len: None } => (STREAM_ACCUMULATION, true),
			}
		};

		// Fixed once, before the first chunk: a total deadline that a sender cannot push back by
		// sending something.
		let expires = unsafe { clock() }.saturating_add(STREAM_TOTAL_TICKS);
		let mut bytes: Vec<u8> = Vec::new();
		let mut chunks: usize = 0;
		loop {
			// Bounded at RECEPTION. The accumulation check below cannot help if a single chunk
			// has already been allocated at whatever size the sender chose.
			// Bounded in TIME as well as in size. The serve loop runs this synchronously, so a
			// client that opens a stream and then says nothing would otherwise hold the whole
			// service - every other client, every other volume, the admin endpoint - for as long
			// as it liked. The previous round stopped a sender from drowning the service in data;
			// it did not stop one from stopping it with silence.
			//
			// A deadline bounds the harm rather than removing it: the service is still unavailable
			// while it waits. Streams belong in the event loop as pending operations, which is a
			// larger change and is recorded as such.
			//
			// The EARLIER of the two deadlines: idle (has the sender gone away) and total (has this
			// operation run long enough). Taking the idle one alone let a sender renew its window
			// with a single byte and hold the service indefinitely while never being idle.
			let idle = unsafe { clock() }.saturating_add(STREAM_IDLE_TICKS);
			let deadline = core::cmp::min(idle, expires);
			match unsafe { recv_vec_deadline(data, limit.saturating_sub(bytes.len()), deadline) } {
				BoundedVec::Message { bytes: chunk, handle } => {
					// A stream carries plain messages. A client that transfers a capability
					// anyway must not leak it into this service's table.
					if handle != 0 {
						unsafe { close(handle) };
					}
					if chunk.is_empty() {
						break;
					}
					if bytes.try_reserve_exact(chunk.len()).is_err() {
						return Err(Error::Again);
					}
					bytes.extend_from_slice(&chunk);
					chunks += 1;
					// Refused as malformed rather than retryable: a sender behaving this way will
					// behave the same way if it tries again.
					if chunks > STREAM_CHUNK_GRACE && bytes.len() / chunks < STREAM_MIN_CHUNK {
						return Err(Error::Invalid);
					}
				}
				// Over the limit, or refused before it was allocated. Stop reading rather than
				// draining politely: a sender that keeps writing would otherwise hold this
				// service in the method for as long as it liked, and blocking a hostile sender is
				// the smaller harm.
				// `Again` throughout, matching the ordinary write: the request was well formed and
				// the volume could not take it now. Reporting `Invalid` here made the same refusal
				// look permanent through one transport and retryable through the other.
				BoundedVec::TooLarge { .. } => return Err(if limit_is_policy { Error::Invalid } else { Error::Again }),
				// Both deadlines surface here; `Again` fits either, because the request was well
				// formed and a later attempt may succeed.
				BoundedVec::Idle => return Err(Error::Again),
				// NOT an end of file. Running out of memory, or a channel error, used to look
				// exactly like the sender finishing - so the prefix received so far was written
				// over the destination and the call reported success. A write that cannot be
				// completed must leave the file as it was, which is the property this filesystem
				// fixed in its backend and lost again here.
				BoundedVec::NoMemory { .. } => return Err(Error::Again),
				BoundedVec::ReceiveError => return Err(Error::Invalid),
				// (no other endings)
				// The sender is done. The only ending that means the file is whole.
				BoundedVec::PeerClosed => break,
			}
		}
		Ok(bytes)
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
		// Mapped and lent, never copied into this service's heap: the filesystem releases its
		// reservation before it makes its own copy, which is the whole point of having one. The
		// path is checked before the buffer is touched, so a write to a path that cannot exist
		// costs a parse rather than a mapping.
		// Owned before anything can refuse. Validating first was right and left the handle behind
		// on every early return, because the guard that closes it was created afterwards - so a
		// client repeating a bad path drained the service's handle table one call at a time.
		let owned = OwnedHandle::new(data.handle);
		let allowed: Option<usize> = {
			let name: &[u8] = self.writable_name(&path)?;
			match self.fs.write_plan(name) {
				// Refused before the payload is mapped rather than after: a write to a read-only
				// volume used to map the client's buffer first and refuse in `write_file`.
				WritePlan::Refused(e) => return Err(e),
				WritePlan::Allowed { max_len } => max_len,
			}
		};
		// A ceiling is only checked when the filesystem gave one. `STREAM_ACCUMULATION` is
		// deliberately NOT applied here: the payload already exists in the client's memory, so
		// there is nothing for this service to be protected from, and applying it was the
		// regression that refused a 20 MiB disk write for a limit invented on behalf of streaming.
		if allowed.is_some_and(|allowed| data.len as usize > allowed) {
			// The volume cannot take this now; the request itself was well formed.
			return Err(Error::Again);
		}
		let buffer = unsafe { map_buffer(&Buffer { handle: owned.release(), len: data.len }) }.ok_or(Error::Invalid)?;
		let name: &[u8] = self.writable_name(&path)?;
		self.fs.write_file(name, buffer.as_slice())
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
		// The stream's channel is owned from here on: the generated dispatch has handed it over,
		// so every path out of this method has to close it. A `?` before the close leaked it, and
		// a client repeating a bad path could exhaust the handle table that way.
		let outcome = self.receive_stream(data, &path);
		unsafe { close(data) };
		let bytes = outcome?;
		let name: &[u8] = self.writable_name(&path)?;
		self.fs.write_file_owned(name, bytes)
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
	// Writable unless the mount itself is read-only (a snapshot, or a volume degraded by a corrupt
	// snapshot table). No ceiling before the write: free blocks are a lower bound on what fits
	// because this filesystem compresses, so quoting them as a maximum would refuse writes that
	// would have succeeded.
	fn write_plan(&mut self, _name: &[u8]) -> WritePlan {
		if self.fs.is_read_only() {
			return WritePlan::Refused(Error::Denied);
		}
		WritePlan::Allowed { max_len: None }
	}
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
	// Writable, with no ceiling to give before the write: the FAT backend decides at commit time.
	fn write_plan(&mut self, _name: &[u8]) -> WritePlan {
		WritePlan::Allowed { max_len: None }
	}
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
// The two memory volumes behind the one shared trait. Every mutation is supported - this is the
// only writable backend besides LiberFS - and there is deliberately no `set_clock`: file times
// are reported as zero because nothing here outlives the boot that made it, so a timestamp would
// describe an age that has no meaning to a caller.
struct MemFs {
	fs: LiberMemFs,
	name: &'static [u8],
}

impl FileSystem for MemFs {
	fn volume_name(&self) -> &'static [u8] {
		self.name
	}
	fn read_file(&mut self, name: &[u8]) -> Result<Vec<u8>, Error> {
		self.fs.read_file(name).map_err(map_fs_err)
	}
	fn list_entries(&mut self, dir: &[u8]) -> Result<Vec<FileInfo>, Error> {
		let entries = self.fs.list_entries(dir).map_err(map_fs_err)?;
		// The backend already reserved fallibly and copied each name once; collecting infallibly
		// here and rebuilding every name would undo both. `Entry` owns its `String`, so it moves.
		let mut out: Vec<FileInfo> = Vec::new();
		out.try_reserve_exact(entries.len()).map_err(|_| Error::Again)?;
		for entry in entries {
			out.push(FileInfo { name: entry.name, size: entry.size, r#type: if entry.is_dir { FileType::Dir } else { FileType::File }, mtime: 0, ctime: 0 });
		}
		Ok(out)
	}
	fn capacity(&mut self) -> Result<u64, Error> {
		Ok(self.fs.capacity())
	}
	fn write_plan(&mut self, name: &[u8]) -> WritePlan {
		match self.fs.writable_len(name) {
			Ok(max) => WritePlan::Allowed { max_len: Some(max) },
			Err(e) => WritePlan::Refused(map_fs_err(e)),
		}
	}
	fn write_file_owned(&mut self, name: &[u8], data: Vec<u8>) -> Result<(), Error> {
		// Adopted, not copied: the streamed buffer becomes the file's own storage, so a reserved
		// volume needs the memory once instead of twice.
		self.fs.write_file_owned(name, data).map_err(map_fs_err)
	}
	fn status(&mut self) -> Result<VolumeStatus, Error> {
		Ok(VolumeStatus { label: String::new(), total_bytes: self.fs.capacity(), free_bytes: self.fs.free(), compression: false, read_only: false, filesystem: String::from("libermemfs") })
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
}

// A block device over bytes already in memory, so a filesystem image handed over as a buffer can
// be mounted without a disk behind it.
struct ImageDevice {
	bytes: Vec<u8>,
}

impl BlockDevice for ImageDevice {
	fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool {
		// Checked, because the index comes from the image being read: a damaged or crafted
		// superblock can name a block far past the end, and multiplying it out unchecked is a
		// panic in debug and a wrapped read of the wrong block in release. A block device should
		// refuse an impossible index itself rather than trust every parser above it.
		let Ok(index) = usize::try_from(index) else { return false };
		let Some(start) = index.checked_mul(buf.len()) else { return false };
		let Some(end) = start.checked_add(buf.len()) else { return false };
		match self.bytes.get(start..end) {
			Some(src) => {
				buf.copy_from_slice(src);
				true
			}
			None => false,
		}
	}
}

// Build the live system volume: mount the handed-over LiberFS image and copy every file it holds
// into a writable memory volume, which is then served as `vol://system`.
//
// Sized from the image rather than from the scratch sizes the other memory volumes carry: a live
// session's system volume holds what the medium shipped, plus room to work in.
unsafe fn live_volume(handle: u64) -> Option<LiberMemFs> {
	let image = unsafe { read_buffer(&Buffer { handle, len: unsafe { object_info(handle) }?.size }) }?;
	// Sized from what the image HOLDS, not from how big the image is: a compressed source expands,
	// names cost, and a buffer keeps the capacity it grew to. Guessing from the image size is how
	// a copy runs out of room half way through.
	let mut source = LiberFs::mount(ImageDevice { bytes: image })?;
	// Checked throughout, matching `measure` itself. Corrupt metadata that measures near the top
	// of the address space would otherwise panic in debug and WRAP in release - and a wrapped
	// total sizes the volume far too small, which is the worst of the three outcomes because it
	// looks like a successful mount.
	let wanted = match measure(&mut source, b"", 0) {
		Some(bytes) => bytes.checked_add(bytes / 4)?.checked_add(4 * 1024 * 1024)?,
		None => return None,
	};
	let mut live = LiberMemFs::mount(MemPolicy::Capped, wanted).ok()?;
	// Every failure stops the import. A live system that comes up missing executables because a
	// write was refused half way through is worse than one that refuses to come up: the first is
	// discovered by whoever needed the missing file.
	let copied = copy_tree(&mut source, &mut live, b"", 0)?;
	unsafe {
		print(b"storage: vol://system is a live copy in memory (the medium is never written)\n");
	}
	let _ = copied;
	Some(live)
}

// The bytes a source subtree holds, so the live volume can be sized before anything is copied.
// The deepest the import walks. A source image may nest deeper than the destination accepts, and
// the destination refuses the PATH at write time - after the walk has already recursed that far on
// a kernel stack. Bounded here, where the recursion is, rather than trusted to the far end.
const IMPORT_MAX_DEPTH: u32 = 16;

fn measure(source: &mut LiberFs<ImageDevice>, dir: &[u8], depth: u32) -> Option<usize> {
	if depth > IMPORT_MAX_DEPTH {
		return None;
	}
	let entries = (if dir.is_empty() { source.list() } else { source.read_dir(dir) }).ok()?;
	let mut total = 0usize;
	for (name, size, is_dir, _, _) in entries.into_iter() {
		let mut path: Vec<u8> = Vec::new();
		path.try_reserve_exact(dir.len() + 1 + name.len()).ok()?;
		if !dir.is_empty() {
			path.extend_from_slice(dir);
			path.push(b'/');
		}
		path.extend_from_slice(&name);
		total = total.checked_add(name.len())?;
		if is_dir {
			total = total.checked_add(measure(source, &path, depth + 1)?)?;
		} else {
			total = total.checked_add(size as usize)?;
		}
	}
	Some(total)
}

// Copy one directory and everything under it from the image into the live volume.
//
// Returns the number of files copied, or None at the FIRST failure. Ignoring failures let a
// directory that could not be created be recursed into anyway, so every file under it failed on a
// missing parent and was dropped too - silently, with the volume then reported as ready.
fn copy_tree(source: &mut LiberFs<ImageDevice>, live: &mut LiberMemFs, dir: &[u8], depth: u32) -> Option<usize> {
	if depth > IMPORT_MAX_DEPTH {
		return None;
	}
	let entries = (if dir.is_empty() { source.list() } else { source.read_dir(dir) }).ok()?;
	let mut copied = 0usize;
	for (name, _, is_dir, _, _) in entries.into_iter() {
		let mut path: Vec<u8> = Vec::new();
		path.try_reserve_exact(dir.len() + 1 + name.len()).ok()?;
		if !dir.is_empty() {
			path.extend_from_slice(dir);
			path.push(b'/');
		}
		path.extend_from_slice(&name);
		if is_dir {
			live.mkdir(&path).ok()?;
			copied += copy_tree(source, live, &path, depth + 1)?;
		} else {
			// Handed over by ownership: the bytes are already allocated, so the live volume adopts
			// them instead of making a third copy of every file on the medium.
			let bytes = source.read_file(&path).ok()?;
			live.write_file_owned(&path, bytes).ok()?;
			copied += 1;
		}
	}
	Some(copied)
}

// The capacity a memory volume was asked for, as decimal bytes after its tag. An unreadable or
// absent number is a refusal rather than a default: mounting a volume of a size nobody chose is
// how a system quietly runs out of memory later.
fn mem_capacity(bytes: &[u8]) -> usize {
	let text = core::str::from_utf8(bytes).unwrap_or("");
	text.trim().parse::<usize>().unwrap_or_else(|_| unsafe { exit() })
}

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
	// `denied` rather than `invalid`, matching its mutations: the boot archive is refused as a
	// matter of policy, not because writing was never implemented.
	fn write_plan(&mut self, _name: &[u8]) -> WritePlan {
		WritePlan::Refused(Error::Denied)
	}
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
	fs.set_clock(unsafe { clock_rtc() });
	// Empty, and said out loud. There used to be a factory archive at LBA 0 to seed from, and
	// with it gone (M0138) a disk carrying no filesystem carries no system volume either - the
	// volume is built as a filesystem now, not made on the spot from a package beside it.
	//
	// Formatting rather than refusing keeps a genuinely fresh disk usable, but silence here would
	// let a machine whose system volume never arrived look exactly like one that is merely new.
	unsafe {
		print(b"storage: vol://system had no filesystem; formatted an EMPTY volume (nothing was seeded - the disk carries no system volume)\n");
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

// The filesystem pool the disk's real capacity allows, in filesystem blocks - asked of the disk
// over the capacity query.
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

// The transferred buffer's bytes, borrowed in place.
//
// The caller sees the client's payload without it being copied into this service's heap first.
// That copy was the reason a memory volume's reservation could not be used: the payload was built
// beside the reservation, so a 63 MiB write into a 64 MiB reserved volume needed both at once and
// the volume's own guarantee was unreachable through the public API. Mapping and lending lets the
// filesystem release before it copies, which is what the reservation is for.
//
// The mapping and the handle are released when the guard drops, on every path.
struct MappedBuffer {
	handle: u64,
	base: u64,
	len: usize,
}

// A transferred handle owned from the moment it arrives.
//
// The generated dispatch takes the handle out of the request and hands ownership to the method,
// so anything that returns without closing it loses it for good - and a client repeating a
// refused call exhausts the service's table. Taking ownership on the FIRST line means no
// validation, however early, can leak it.
struct OwnedHandle {
	handle: u64,
}

impl OwnedHandle {
	fn new(handle: u64) -> Self {
		Self { handle }
	}

	// Give the handle up to something that will close it itself.
	fn release(mut self) -> u64 {
		let handle = self.handle;
		self.handle = 0;
		handle
	}
}

impl Drop for OwnedHandle {
	fn drop(&mut self) {
		if self.handle != 0 {
			unsafe { close(self.handle) };
		}
	}
}

impl MappedBuffer {
	fn as_slice(&self) -> &[u8] {
		if self.len == 0 {
			return &[];
		}
		unsafe { core::slice::from_raw_parts(self.base as *const u8, self.len) }
	}
}

impl Drop for MappedBuffer {
	fn drop(&mut self) {
		unsafe {
			if self.base != 0 {
				unmap_object(self.handle);
			}
			close(self.handle);
		}
	}
}

// Map a transferred buffer for borrowing. Always consumes the handle. None when the handle is
// unusable or the claimed length exceeds what the client actually backed with memory.
unsafe fn map_buffer(data: &Buffer) -> Option<MappedBuffer> {
	unsafe {
		if data.handle == 0 {
			return None;
		}
		let len: usize = data.len as usize;
		let real: usize = match object_info(data.handle) {
			Some(info) => info.size as usize,
			None => 0,
		};
		if len > real {
			close(data.handle);
			return None;
		}
		if len == 0 {
			return Some(MappedBuffer { handle: data.handle, base: 0, len: 0 });
		}
		match map_object(data.handle) {
			Some(base) => Some(MappedBuffer { handle: data.handle, base, len }),
			None => {
				close(data.handle);
				None
			}
		}
	}
}

// Copy the bytes behind a zero-copy `data` buffer out into a Vec and release the
// transferred buffer handle. Always consumes the handle. Returns None on failure or
// if the claimed length exceeds the transferred object's real size.
#[allow(dead_code)]
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
		// Fallible: this is the client's payload, sized by the client, and an infallible
		// allocation here aborts the whole service instead of answering `again`. It is also
		// allocated while a memory volume still holds its reservation, so on a tight heap this is
		// exactly where the process dies.
		let mut bytes: Vec<u8> = Vec::new();
		if bytes.try_reserve_exact(len).is_err() {
			unmap_object(data.handle);
			close(data.handle);
			return None;
		}
		bytes.resize(len, 0);
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
