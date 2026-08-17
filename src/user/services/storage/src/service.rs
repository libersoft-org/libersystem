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
//          boot path. The disk is MOUNTED, never formatted: the system volume is built as
//          a filesystem by `mkpackages` and written to the medium, so a disk that carries
//          no volume is a disk this service refuses and says why; or
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
use liberfs::{BlockDevice, FsError, LiberFs, MountError};
use libermemfs::{LiberMemFs, Policy as MemPolicy};
use proto::codec::Buffer;
use proto::codec::{Handles, Sink, SliceWriter};
use proto::system::{Error, FileEvent, FileEventKind, FileInfo, FileType, FsckReport, OpenOpts, OpenResult, SnapshotInfo, VolumeStatus, WriterMode, volume, volume_admin, writer};
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
// a fresh volume from. That archive is gone (P02M0108): the system volume is now built as a real
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
// WHAT THIS BOUNDS NOW. It used to be the defence against one client stopping the service, because
// the receive ran inside the serve loop. It is not that any more - a stream is a pending operation
// and the loop returns after every chunk - so what remains is the lifetime of a table ENTRY: a
// slot, a path, a channel handle and whatever the volume is holding for it.
//
// That is a different question with different stakes. Cutting a slow sender off early costs a real
// client its transfer; letting a gone one linger costs one entry. So the bound stays generous.
//
// The timer runs at 100 Hz on every architecture, so a tick is 10 ms and this is thirty seconds.
const STREAM_IDLE_TICKS: u64 = 3_000;

// How long a reply waits for a client that is not reading it.
//
// The last place one client could stop this service. Streams stopped blocking the loop while they
// transfer, but the ANSWER still went out through an unbounded send, so a client that filled its
// reply queue and stopped reading held the loop on it - every other client, every volume. A client
// that will not take its reply for ten seconds is treated as gone and its channel is dropped;
// keeping it would mean keeping everyone else waiting for it.
const REPLY_TICKS: u64 = 1_000;

// The most a whole stream may take, counted from the request rather than from the last chunk.
//
// The idle deadline alone bounds SILENCE, not slowness: it is rebuilt after every chunk, so a
// sender that emits one byte just before each window expires renews it forever. Two deadlines are
// needed because they answer different questions - "has this sender gone away?" and "has this
// operation run long enough?" - and neither implies the other.
//
// What it protects has changed with the rest: a renewing sender no longer holds the service, it
// holds the one slot a pending write occupies, and with one slot per volume that still means the
// NEXT stream to that volume is refused for as long as it lasts. That is what this bounds - not
// availability, but how long one client may keep the queue to itself.
//
// Five minutes: long enough that a slow but genuine transfer of the largest stream this service
// accepts is not cut off, short enough that a client cannot own the slot for an afternoon.
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
			Volume::new(alloc::boxed::Box::new(ArchiveFs { base, len: length }))
		}
		Received::Message { len, handle } if handle != 0 && len >= 5 && &buf[..5] == b"BLOCK" => match unsafe { mount_system_volume(handle) } {
			Some(fs) => Volume::new(alloc::boxed::Box::new(DiskFs { fs })),
			None => exit(),
		},
		Received::Message { len, handle } if handle != 0 && len >= 8 && &buf[..8] == b"FATBLOCK" => Volume::new(alloc::boxed::Box::new(FatBacking { chan: handle, name: MEDIA_VOLUME, fs: None })),
		// `mount_checked` here too, for the reason written against UDF below - which was written
		// while this line went on calling `mount`. The reader gained `MountError` and this boundary
		// went on collapsing it, so the distinction existed everywhere except where somebody would
		// read it.
		Received::Message { len, handle } if handle != 0 && len >= 8 && &buf[..8] == b"ISOBLOCK" => match Iso9660::mount_checked(IsoBlockDevice { chan: handle }) {
			Ok(fs) => Volume::new(alloc::boxed::Box::new(IsoFs { fs })),
			Err(why) => {
				unsafe { print(alloc::format!("storage: the ISO volume was refused: {why:?}\n").as_bytes()) };
				exit()
			}
		},
		// `mount_checked`, so a refusal says WHICH refusal. `mount` collapses "this is not UDF",
		// "this UDF is damaged", "this UDF uses something the reader does not implement" and "the
		// device would not answer" into one `None`, and a probe that tries backends in turn cannot
		// tell "try the next one" from "this IS the format and it is broken, do not pretend
		// otherwise". The distinction existed in the reader and stopped at this line.
		Received::Message { len, handle } if handle != 0 && len >= 8 && &buf[..8] == b"UDFBLOCK" => match Udf::mount_checked(UdfBlockDevice { chan: handle }) {
			Ok(fs) => Volume::new(alloc::boxed::Box::new(UdfFs { fs })),
			Err(why) => {
				unsafe { print(alloc::format!("storage: the UDF volume was refused: {why:?}\n").as_bytes()) };
				exit()
			}
		},
		Received::Message { len, handle } if handle != 0 && len >= 8 && &buf[..8] == b"USBBLOCK" => Volume::new(alloc::boxed::Box::new(FatBacking { chan: handle, name: USB_VOLUME, fs: None })),
		// The two memory volumes. They carry no handle - there is no block device to hand over,
		// which is the whole point - and the capacity follows the tag as a decimal byte count.
		// A reserved volume takes its memory here, so a mount that cannot get it fails HERE
		// rather than at the first write.
		// A LIVE system volume: a LiberFS image handed over in memory, copied into a writable
		// memory volume. The medium it came from is read-only - an optical disc, or a stick nobody
		// wants written to - so the running system needs its own copy, and that copy disappears at
		// power off, which is what makes it a live session.
		//
		// This is seeding, which P02M0108 retired for disks, and the distinction is the point: on a
		// disk the archive was a SECOND copy of what the volume already held, and removing that
		// duplication is what the milestone is for. On read-only media there is no first copy.
		Received::Message { len, handle } if handle != 0 && len >= 7 && &buf[..7] == b"LIVEVOL" => match unsafe { live_volume(handle) } {
			Some(fs) => Volume::new(alloc::boxed::Box::new(MemFs { fs, name: SYSTEM_VOLUME })),
			None => exit(),
		},
		Received::Message { len, .. } if len >= 6 && &buf[..6] == b"RAMVOL" => match LiberMemFs::mount(MemPolicy::Reserved, mem_capacity(&buf[6..len])) {
			Ok(fs) => Volume::new(alloc::boxed::Box::new(MemFs { fs, name: RAM_VOLUME })),
			Err(_) => exit(),
		},
		Received::Message { len, .. } if len >= 6 && &buf[..6] == b"TMPVOL" => match LiberMemFs::mount(MemPolicy::Capped, mem_capacity(&buf[6..len])) {
			Ok(fs) => Volume::new(alloc::boxed::Box::new(MemFs { fs, name: TMP_VOLUME })),
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
	/// EXACTLY ONE PATH, and nothing beside it - not the directory it sits in, not a sibling.
	///
	/// This is what a selected-file grant is made of: a program handed one file to open must not be
	/// able to reopen the file next to it, which a directory-scoped client can. The narrowest thing
	/// a volume client can be.
	///
	/// `writable` is carried HERE rather than being a property of the handle, because it has to
	/// survive `connect`: a client minted from a read-only file grant is read-only too, and a scope
	/// that forgot the flag would let a viewer mint itself a writer.
	File {
		path: String,
		writable: bool,
	},
}

struct Client {
	chan: u64,
	// The koid the wait set answers with when this client becomes ready.
	//
	// `waitset_wait` names the READY MEMBER, not a position: a caller that had to keep a list in
	// the kernel's order was the whole reason the first two migration attempts failed, because this
	// table uses `swap_remove` and permutes. A koid needs no mirror.
	koid: u64,
	scope: Scope,
	// Set when this client would not take an answer within the bound, cleared the moment it takes
	// one. While it is set, answers to this client are attempted ONCE and abandoned rather than
	// waited on.
	//
	// Bounding each send stopped the permanent stop and left a starvation window behind it: a
	// client that has stopped reading still has a queue full of REQUESTS, and every one of them
	// cost the whole service another reply deadline. For a subclient that hardly showed - it is
	// dropped on its first stall and its backlog goes with it - and the root client is never
	// dropped, because closing it ends the service. So a root that stopped listening held everyone
	// else up, one deadline per queued request, which is the same defect as the unbounded send with
	// a slower clock.
	//
	// Waiting is the part that costs other people. Attempting is not.
	quiet: bool,
	// Set when this client is a transactional writer session rather than a volume client, in which
	// case it speaks the `writer` interface and nothing else.
	//
	// The session lives HERE, in the client entry, because that is what makes closing the channel
	// an abort: a client that goes away takes its staged bytes with it, and there is no other table
	// to forget to clean up. `scope` is what admitted it and is not consulted again - the path was
	// checked once, when the session opened, and the session can only ever write to that path.
	writer: Option<WriterSession>,
}

// A transactional writer session: the bytes staged for one path, published only by `commit`.
struct WriterSession {
	// The in-volume name, resolved and checked when the session opened. Keeping the RESOLVED name
	// is what makes a commit land where the open was allowed to: re-resolving it at commit time
	// would ask the scope question again, later, against a volume that may have changed.
	name: String,
	// The vol:// path, for the event a commit publishes.
	path: String,
	staged: Vec<u8>,
	cursor: u64,
	// The most this session may stage, and whether that number came from this service's policy or
	// from the filesystem - the same distinction `write-stream` draws, and for the same reason: a
	// filesystem's ceiling is about this moment (`again`), a policy's is not (`invalid`).
	limit: usize,
	limit_is_policy: bool,
	// Set by `commit` or `abort`. The channel stays open afterwards and every op on it fails,
	// because answering as though a new session had begun would publish a second time.
	closed: bool,
}

impl WriterSession {
	// Grow the staged file to `len` with zeros, refusing rather than growing past the ceiling.
	fn reserve_to(&mut self, len: usize) -> Result<(), Error> {
		if len > self.limit {
			return Err(if self.limit_is_policy { Error::Invalid } else { Error::Again });
		}
		if len > self.staged.len() {
			self.staged.try_reserve(len - self.staged.len()).map_err(|_| Error::Again)?;
			self.staged.resize(len, 0);
		}
		Ok(())
	}
}

// A writer session AS THE SERVE LOOP CALLS IT: the session's staged bytes plus the volume they
// will be published to. The two are separate because a session is one client's and the volume is
// everyone's, and only `commit` needs both.
struct WriterCall<'a> {
	vol: &'a mut Volume,
	session: &'a mut WriterSession,
}

impl writer::Service for WriterCall<'_> {
	fn write(&mut self, data: Vec<u8>) -> Result<u64, Error> {
		if self.session.closed {
			return Err(Error::Invalid);
		}
		let at: usize = self.session.cursor as usize;
		let end: usize = at.checked_add(data.len()).ok_or(Error::Invalid)?;
		self.session.reserve_to(end)?;
		self.session.staged[at..end].copy_from_slice(&data);
		self.session.cursor = end as u64;
		Ok(self.session.staged.len() as u64)
	}

	fn write_at(&mut self, offset: u64, data: Vec<u8>) -> Result<(), Error> {
		if self.session.closed {
			return Err(Error::Invalid);
		}
		let at: usize = usize::try_from(offset).map_err(|_| Error::Invalid)?;
		let end: usize = at.checked_add(data.len()).ok_or(Error::Invalid)?;
		// The zero-extension is here rather than in the backend: `reserve_to` grows with zeros, so
		// a write past the end leaves a gap of zeros instead of whatever the allocation held.
		self.session.reserve_to(end)?;
		self.session.staged[at..end].copy_from_slice(&data);
		self.session.cursor = end as u64;
		Ok(())
	}

	fn truncate(&mut self, length: u64) -> Result<(), Error> {
		if self.session.closed {
			return Err(Error::Invalid);
		}
		let len: usize = usize::try_from(length).map_err(|_| Error::Invalid)?;
		self.session.reserve_to(len)?;
		self.session.staged.truncate(len);
		self.session.cursor = core::cmp::min(self.session.cursor, length);
		Ok(())
	}

	fn flush(&mut self) -> Result<u64, Error> {
		if self.session.closed {
			return Err(Error::Invalid);
		}
		Ok(self.session.staged.len() as u64)
	}

	fn commit(&mut self) -> Result<u64, Error> {
		if self.session.closed {
			return Err(Error::Invalid);
		}
		let published: u64 = self.session.staged.len() as u64;
		let existed: bool = self.vol.exists(self.session.name.as_bytes());
		// CLOSED FIRST, before the publish can fail. A commit that failed has still ended the
		// transaction: the staged bytes are gone from the caller's point of view, and letting it
		// retry on the same session would publish whatever the failed attempt left behind.
		self.session.closed = true;
		let staged: Vec<u8> = core::mem::take(&mut self.session.staged);
		self.vol.fs.write_file_owned(self.session.name.as_bytes(), staged)?;
		let kind = if existed { FileEventKind::Modified } else { FileEventKind::Created };
		let path = core::mem::take(&mut self.session.path);
		self.vol.note(kind, &path, published);
		Ok(published)
	}

	fn abort(&mut self) -> Result<(), Error> {
		if self.session.closed {
			return Err(Error::Invalid);
		}
		self.session.closed = true;
		self.session.staged = Vec::new();
		Ok(())
	}
}

struct AdminCall<'a> {
	volume: &'a Volume,
	clients: &'a mut Vec<Client>,
	// The wait set the clients belong to. Handed in rather than reached for, because admitting a
	// client is joining it - see `admit_client`.
	set: u64,
}

impl volume_admin::Service for AdminCall<'_> {
	fn open_file(&mut self, path: String, writable: bool) -> Result<u64, Error> {
		let scope: Scope = Scope::file(self.volume, &path, writable)?;
		self.mint(scope)
	}

	fn open_directory(&mut self, path: String) -> Result<u64, Error> {
		let scope: Scope = Scope::directory(self.volume, &path)?;
		self.mint(scope)
	}
}

impl AdminCall<'_> {
	// One place a narrowed client is made, so a file grant and a directory grant cannot come to
	// differ in how they are admitted or in what a refusal leaves behind.
	fn mint(&mut self, scope: Scope) -> Result<u64, Error> {
		let (server, client): (u64, u64) = unsafe { channel() }.ok_or(Error::Again)?;
		// A refused admission closes BOTH ends: the grant did not happen, so neither handle has an
		// owner. `Again` is what it is - the table is full or the machine is short, and both are
		// conditions a caller can retry rather than a fault in the request.
		if !admit_client(self.set, self.clients, Client { chan: server, koid: 0, scope, quiet: false, writer: None }) {
			unsafe {
				close(server);
				close(client);
			}
			return Err(Error::Again);
		}
		Ok(client)
	}
}

// The volume interface AS THE SERVE LOOP CALLS IT: the volume, plus the client table.
//
// `open-writer` is the one volume op whose answer is a new CLIENT rather than a value, and the
// generated dispatch is handed exactly one service object - so either every op can reach the table
// or none can. This is the shape `AdminCall` already has for `open-directory`, for the same reason.
// Every other op forwards to the volume unchanged; the forwarding is dull on purpose, because the
// alternative - two dispatch sites, one with the table and one without - is how an op ends up
// meaning different things depending on which client asked.
struct VolumeCall<'a> {
	vol: &'a mut Volume,
	clients: &'a mut Vec<Client>,
	set: u64,
	// The scope of the client that asked. `open-writer` is the only op that consults it here (the
	// loop checks every other one before dispatching), because the session it creates outlives the
	// request and has to carry the same restriction the request was under.
	scope: Scope,
}

impl volume::Service for VolumeCall<'_> {
	fn open(&mut self, o: OpenOpts) -> Result<OpenResult, Error> {
		self.vol.open(o)
	}
	fn list(&mut self, path: String) -> Result<Vec<FileInfo>, Error> {
		self.vol.list(path)
	}
	fn write(&mut self, path: String, data: Buffer) -> Result<(), Error> {
		self.vol.write(path, data)
	}
	fn remove(&mut self, path: String) -> Result<(), Error> {
		self.vol.remove(path)
	}
	fn snap_create(&mut self, name: String) -> Result<(), Error> {
		self.vol.snap_create(name)
	}
	fn snap_list(&mut self) -> Result<Vec<SnapshotInfo>, Error> {
		self.vol.snap_list()
	}
	fn snap_delete(&mut self, name: String) -> Result<(), Error> {
		self.vol.snap_delete(name)
	}
	fn snap_open(&mut self, snapshot: String, path: String) -> Result<OpenResult, Error> {
		self.vol.snap_open(snapshot, path)
	}
	fn mkdir(&mut self, path: String) -> Result<(), Error> {
		self.vol.mkdir(path)
	}
	fn rmdir(&mut self, path: String) -> Result<(), Error> {
		self.vol.rmdir(path)
	}
	fn capacity(&mut self) -> Result<u64, Error> {
		self.vol.capacity()
	}
	fn status(&mut self) -> Result<VolumeStatus, Error> {
		self.vol.status()
	}
	fn set_compression(&mut self, enabled: bool) -> Result<(), Error> {
		self.vol.set_compression(enabled)
	}
	fn fsck(&mut self) -> Result<FsckReport, Error> {
		self.vol.fsck()
	}
	fn restore(&mut self, path: String, snapshot: String) -> Result<(), Error> {
		self.vol.restore(path, snapshot)
	}
	fn write_stream(&mut self, path: String, data: u64) -> Result<(), Error> {
		self.vol.write_stream(path, data)
	}
	fn stat(&mut self, path: String) -> Result<FileInfo, Error> {
		self.vol.stat(path)
	}
	fn rename(&mut self, from: String, to: String) -> Result<(), Error> {
		self.vol.rename(from, to)
	}
	fn truncate(&mut self, path: String, length: u64) -> Result<(), Error> {
		self.vol.truncate(path, length)
	}
	fn touch(&mut self, path: String, create: bool, at: u64) -> Result<(), Error> {
		self.vol.touch(path, create, at)
	}
	fn read(&mut self, path: String, offset: u64, length: u32) -> Result<Buffer, Error> {
		self.vol.read(path, offset, length)
	}
	fn watch(&mut self, path: String) -> Result<Vec<FileEvent>, Error> {
		self.vol.watch(path)
	}

	// Open a transactional writer over one path: a channel speaking `writer`, admitted as a client
	// of this service so its session is pumped by the same loop as everything else.
	fn open_writer(&mut self, path: String, mode: WriterMode) -> Result<u64, Error> {
		if !self.scope.allows_path(self.vol.name(), &path) {
			return Err(Error::Denied);
		}
		let name: &[u8] = self.vol.writable_name(&path)?;
		// REFUSED BEFORE A CHANNEL EXISTS, so a session over a read-only volume costs a parse
		// rather than a handle pair and a client slot.
		let (limit, limit_is_policy): (usize, bool) = match self.vol.fs.write_plan(name) {
			WritePlan::Refused(e) => return Err(e),
			WritePlan::Allowed { max_len: Some(max) } => (max, false),
			WritePlan::Allowed { max_len: None } => (STREAM_ACCUMULATION, true),
		};
		// ONE SESSION PER PATH. Two transactions over one file publish in an order neither chose,
		// and the loser's work disappears at the moment it reported success - so the second is told
		// `again`, which is true: the first one ends.
		if self.clients.iter().any(|client| client.writer.as_ref().is_some_and(|session| !session.closed && session.name.as_bytes() == name)) {
			return Err(Error::Again);
		}
		let mut staged: Vec<u8> = Vec::new();
		let mut cursor: u64 = 0;
		if matches!(mode, WriterMode::Append) {
			// A missing file appends to nothing, which is what an append to a file that is not
			// there means - the session creates it. Any other failure is the volume's answer and
			// is passed on rather than turned into an empty file.
			match self.vol.fs.read_file(name) {
				Ok(existing) => {
					cursor = existing.len() as u64;
					staged = existing;
				}
				Err(Error::NotFound) => {}
				Err(e) => return Err(e),
			}
			if staged.len() > limit {
				return Err(if limit_is_policy { Error::Invalid } else { Error::Again });
			}
		}
		let session = WriterSession { name: to_string(core::str::from_utf8(name).map_err(|_| Error::Invalid)?)?, path: to_string(&path)?, staged, cursor, limit, limit_is_policy, closed: false };
		let (server, client): (u64, u64) = unsafe { channel() }.ok_or(Error::Again)?;
		if !admit_client(self.set, self.clients, Client { chan: server, koid: 0, scope: self.scope.clone(), quiet: false, writer: Some(session) }) {
			unsafe {
				close(server);
				close(client);
			}
			return Err(Error::Again);
		}
		Ok(client)
	}

	// A SECOND CLIENT, not a second name for this one. See the op's own comment in `storage.lsidl`
	// for the failure this exists to stop; the short form is that a duplicated channel endpoint is
	// one queue with two holders, so two concurrent callers take each other's replies.
	//
	// It grants nothing new: the minted client carries this caller's scope, so a directory-scoped
	// client can only mint another directory-scoped client, and one that was refused a path is
	// refused it again on every connection it makes. That is what makes this safe to hand to a
	// governed tool - it is the authority the caller already has, arriving on its own wire.
	fn connect(&mut self) -> Result<u64, Error> {
		let (server, client): (u64, u64) = unsafe { channel() }.ok_or(Error::Again)?;
		// A refused admission closes BOTH ends, for `open-directory`'s reason: the grant did not
		// happen, so neither handle has an owner. `Again` is the honest answer - the client table
		// is full, which a caller can retry rather than a fault in what it asked.
		if !admit_client(self.set, self.clients, Client { chan: server, koid: 0, scope: self.scope.clone(), quiet: false, writer: None }) {
			unsafe {
				close(server);
				close(client);
			}
			return Err(Error::Again);
		}
		Ok(client)
	}
}

// A `String` that says it could not be allocated instead of aborting the service.
//
// `String::from` calls the infallible allocator, whose answer to a full heap is to end the
// process; a service that a client can end by asking for enough sessions is not bounded, whatever
// its other ceilings say.
fn to_string(from: &str) -> Result<String, Error> {
	let mut owned = String::new();
	owned.try_reserve_exact(from.len()).map_err(|_| Error::Again)?;
	owned.push_str(from);
	Ok(owned)
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

	fn file(volume: &Volume, path: &str, writable: bool) -> Result<Scope, Error> {
		let target: VolumePath = VolumePath::parse(path.as_bytes()).ok_or(Error::Invalid)?;
		if target.volume != volume.name() {
			return Err(Error::NotFound);
		}
		let file: &str = core::str::from_utf8(target.path.as_bytes()).map_err(|_| Error::Invalid)?;
		Ok(Scope::File { path: String::from(file), writable })
	}

	fn allows_path(&self, volume: &[u8], path: &str) -> bool {
		match self {
			Self::Full => true,
			Self::Directory(directory) => {
				let Some(target) = VolumePath::parse(path.as_bytes()) else { return false };
				target.volume == volume && (target.path.as_bytes() == directory.as_bytes() || target.path.as_bytes().strip_prefix(directory.as_bytes()).is_some_and(|rest| rest.starts_with(b"/")))
			}
			// EQUALITY, not a prefix. A file scope that admitted anything beginning with its path
			// would admit `notes.txt.bak` for a grant over `notes.txt`, and a directory's worth of
			// files for a grant over the directory's own name.
			Self::File { path: file, .. } => {
				let Some(target) = VolumePath::parse(path.as_bytes()) else { return false };
				target.volume == volume && target.path.as_bytes() == file.as_bytes()
			}
		}
	}

	fn allows_request(&self, volume: &[u8], request: &[u8]) -> bool {
		if matches!(self, Self::Full) {
			return true;
		}
		// WHAT A SELECTED-FILE GRANT CAN DO, in one place so the answer is readable rather than
		// assembled from the path check and the op table. It may not LIST - listing its own path
		// answers nothing and listing anything else is refused anyway - and a READ-ONLY one may not
		// reach any op that changes the file, which is the whole difference between handing a
		// program a file to show and handing it one to edit.
		if let Self::File { writable, .. } = self {
			let op: u16 = if request.len() >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
			if op == volume::OP_LIST {
				return false;
			}
			if !writable && matches!(op, volume::OP_WRITE | volume::OP_REMOVE | volume::OP_TRUNCATE | volume::OP_TOUCH | volume::OP_WRITE_STREAM | volume::OP_OPEN_WRITER) {
				return false;
			}
		}
		let op: u16 = if request.len() >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
		match op {
			// Every op whose FIRST field is the path it acts on, which is what `request_path`
			// reads. `rename` is deliberately absent: it carries two paths, this helper sees one,
			// and a scoped client allowed to rename by its source alone could move a file out of
			// the directory it was granted. It is refused until the check can read both.
			volume::OP_OPEN | volume::OP_LIST | volume::OP_WRITE | volume::OP_REMOVE | volume::OP_MKDIR | volume::OP_RMDIR | volume::OP_WRITE_STREAM | volume::OP_STAT | volume::OP_TRUNCATE | volume::OP_TOUCH | volume::OP_READ | volume::OP_WATCH | volume::OP_OPEN_WRITER => request_path(request).is_some_and(|path| self.allows_path(volume, path)),
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

// A write stream the service has accepted but not finished.
//
// The point of this type is that the serve loop RETURNS after every chunk. Receiving a stream
// synchronously meant one client held the service for the whole transfer - every other client,
// every volume, the admin endpoint - and three rounds of review answered that with a deadline,
// which bounds the harm rather than removing it.
struct PendingWrite {
	// The client's end of the stream, and the channel its reply goes back on.
	stream: u64,
	// The koid the stream joined the wait set under, kept beside the handle because leaving is by
	// koid: a member that has to be named by its handle cannot be retired after the handle is
	// closed, and closing first is the natural thing to do with a dead peer.
	stream_koid: u64,
	client: u64,
	corr: u32,
	path: String,
	// Absolute ticks. The idle one is renewed by every chunk; the total one is not, so a sender
	// that drip-feeds cannot hold the entry forever.
	idle: u64,
	expires: u64,
	// Whether the filesystem is accumulating this itself. When it is, the memory is charged to the
	// volume as it arrives instead of piling up here where its accounting cannot see it.
	incremental: bool,
	bytes: Vec<u8>,
	received: usize,
	chunks: usize,
	limit: usize,
	limit_is_policy: bool,
}

impl PendingWrite {
	fn deadline(&self) -> u64 {
		core::cmp::min(self.idle, self.expires)
	}
}

// `[corr][1]` for success, `[corr][0][error]` for failure - the shape the generated dispatch
// writes, produced by hand because this reply is sent long after the call that asked for it.
fn write_stream_reply(corr: u32, result: Result<(), Error>, out: &mut [u8]) -> Option<usize> {
	let mut writer = SliceWriter::new(out);
	let w = &mut writer;
	w.u32(corr)?;
	match result {
		Ok(()) => w.u8(1)?,
		Err(e) => {
			w.u8(0)?;
			e.write(w)?;
		}
	}
	Some(writer.pos())
}

// Accept a write stream, or refuse it before a byte is taken.
//
// The destination is validated here - a read-only volume, a missing parent, a directory in the way
// - so a refusal costs a parse rather than a transfer. `Err` carries the correlation id because the
// caller has to answer the client that asked.
fn begin_stream(vol: &mut Volume, client: u64, scope: &Scope, request: &[u8], request_handle: &mut proto::codec::Handles, busy: bool) -> Result<PendingWrite, (u32, Error)> {
	let mut reader = proto::codec::Reader::with_handle_list(request, request_handle);
	let r = &mut reader;
	let parsed: Option<(u32, String, u64)> = (|| {
		let _op = r.u16()?;
		let corr = r.u32()?;
		let path = r.string_lp()?;
		let _len = r.u32()?;
		let data = r.take_handle()?;
		Some((corr, path, data))
	})();
	let Some((corr, path, data)) = parsed else {
		// Left for the serve loop to close, which closes whatever is unclaimed after every request.
		// Closing here as well left the list populated, so the same handles were closed twice -
		// harmless while slots are not reused, and wrong regardless of that.
		return Err((0, Error::Invalid));
	};
	request_handle.clear();
	// Every refusal from here on owns the stream channel and has to close it.
	let refuse = |e: Error| {
		unsafe { close(data) };
		Err((corr, e))
	};
	// The handle has to BE a channel this service can read and wait on.
	//
	// The interface declares `handle<channel>` and nothing more, so a client may transfer anything
	// - a memory object, a channel without WAIT - and the loop would put it straight into the
	// shared `wait_any_periodic`. A wait that cannot be performed returns an error rather than
	// blocking, and the loop's error branch retries, so one bad handle spins the service until the
	// deadline. Checked here, where refusing costs a parse.
	const OBJECT_TYPE_CHANNEL: u64 = 5;
	match unsafe { object_info(data) } {
		Some(info) if info.object_type == OBJECT_TYPE_CHANNEL && info.rights & RIGHT_READ != 0 && info.rights & RIGHT_WAIT != 0 => {}
		_ => return refuse(Error::Invalid),
	}
	if !scope.allows_request(vol.name(), request) {
		return refuse(Error::Denied);
	}
	// One at a time: see the note where `pending` is declared.
	if busy {
		return refuse(Error::Again);
	}
	vol.fs.set_clock(unsafe { clock_rtc() });
	let name: &[u8] = match vol.writable_name(&path) {
		Ok(name) => name,
		Err(e) => return refuse(e),
	};
	let (limit, limit_is_policy): (usize, bool) = match vol.fs.stream_plan(name) {
		WritePlan::Refused(e) => return refuse(e),
		WritePlan::Allowed { max_len: Some(max) } => (max, false),
		WritePlan::Allowed { max_len: None } => (STREAM_ACCUMULATION, true),
	};
	let incremental: bool = match vol.fs.stream_begin(name) {
		Some(Ok(())) => true,
		Some(Err(e)) => return refuse(e),
		None => false,
	};
	let now = unsafe { clock() };
	Ok(PendingWrite { stream: data, stream_koid: 0, client, corr, path, idle: now.saturating_add(STREAM_IDLE_TICKS), expires: now.saturating_add(STREAM_TOTAL_TICKS), incremental, bytes: Vec::new(), received: 0, chunks: 0, limit, limit_is_policy })
}

enum StreamStep {
	More,
	Done(Result<(), Error>),
}

// Take ONE chunk and return to the loop. This is the whole difference from the synchronous
// version: the service is available again between every chunk, so a slow or silent sender costs
// other clients nothing.
fn take_chunk(vol: &mut Volume, p: &mut PendingWrite) -> StreamStep {
	let room = p.limit.saturating_sub(p.received);
	// Straight into the destination when it can hand out the space.
	//
	// The path below allocates the message as its own vector and the filesystem then copies it, so
	// every chunk exists twice for as long as the copy takes - beside the reservation, the pending
	// buffer, and the pending buffer's old allocation while it grows. That is why a reserved volume
	// could not promise to take one chunk near its capacity.
	if p.incremental {
		let waiting: i64 = unsafe { channel_peek(p.stream) };
		if waiting == ERR_PEER_CLOSED {
			return StreamStep::Done(Ok(()));
		}
		// Only "nothing yet" means try again. Any other failure ends the stream: retrying it says
		// the sender is merely slow, which is a claim the error does not support. The handle
		// validation in `begin_stream` removes most ways to reach this, which is a reason it has
		// not bitten rather than a reason to fold every error into patience.
		if waiting == ERR_WOULD_BLOCK {
			return StreamStep::More;
		}
		if waiting < 0 {
			return StreamStep::Done(Err(Error::Invalid));
		}
		let want = waiting as usize;
		if want == 0 {
			// An empty message is the sender saying it is finished; take it and stop.
			let mut nothing: [u8; 1] = [0u8; 1];
			let _ = unsafe { recv_into(p.stream, &mut nothing[..0]) };
			return StreamStep::Done(Ok(()));
		}
		if want > room {
			return StreamStep::Done(Err(if p.limit_is_policy { Error::Invalid } else { Error::Again }));
		}
		let offered = want;
		// Whether an offer was actually opened. A backend that does not implement `stream_spare`
		// returns `None`, and there is then nothing to close - calling `stream_advance` on it is
		// closing an offer that was never opened, which the protocol now refuses and which used to
		// be a silent no-op. Getting this wrong took every service that streams offline.
		let mut opened = false;
		let written = match vol.fs.stream_spare(want) {
			Some(Ok(spare)) => {
				opened = true;
				match unsafe { recv_into(p.stream, spare) } {
					RecvInto::Received(n) => n,
					RecvInto::PeerClosed => {
						if vol.fs.stream_advance(offered, 0).is_err() {
							return StreamStep::Done(Err(Error::Invalid));
						}
						return StreamStep::Done(Ok(()));
					}
					RecvInto::Empty => {
						if vol.fs.stream_advance(offered, 0).is_err() {
							return StreamStep::Done(Err(Error::Invalid));
						}
						return StreamStep::More;
					}
					RecvInto::Failed => {
						let _ = vol.fs.stream_advance(offered, 0);
						return StreamStep::Done(Err(Error::Invalid));
					}
				}
			}
			Some(Err(e)) => return StreamStep::Done(Err(e)),
			None => 0,
		};
		if opened && vol.fs.stream_advance(offered, written).is_err() {
			return StreamStep::Done(Err(Error::Invalid));
		}
		p.received = p.received.saturating_add(written);
		p.chunks += 1;
		if p.chunks > STREAM_CHUNK_GRACE && p.received / p.chunks < STREAM_MIN_CHUNK {
			return StreamStep::Done(Err(Error::Invalid));
		}
		p.idle = unsafe { clock() }.saturating_add(STREAM_IDLE_TICKS);
		return StreamStep::More;
	}
	match unsafe { recv_vec_bounded(p.stream, room) } {
		BoundedVec::Message { bytes: chunk, handle } => {
			// A stream carries plain messages; a capability sent anyway must not leak into this
			// service's table.
			if handle != 0 {
				unsafe { close(handle) };
			}
			if chunk.is_empty() {
				return StreamStep::Done(Ok(()));
			}
			if p.incremental {
				if let Err(e) = vol.fs.stream_push(&chunk) {
					return StreamStep::Done(Err(e));
				}
			} else if p.bytes.try_reserve_exact(chunk.len()).is_err() {
				return StreamStep::Done(Err(Error::Again));
			} else {
				p.bytes.extend_from_slice(&chunk);
			}
			p.received = p.received.saturating_add(chunk.len());
			p.chunks += 1;
			// A drip-feeder is refused for the pattern, not the clock: it is never idle, so no
			// deadline catches it.
			if p.chunks > STREAM_CHUNK_GRACE && p.received / p.chunks < STREAM_MIN_CHUNK {
				return StreamStep::Done(Err(Error::Invalid));
			}
			// Idle window renewed by arrival; the total deadline is not, which is what stops the
			// renewal being unbounded.
			p.idle = unsafe { clock() }.saturating_add(STREAM_IDLE_TICKS);
			StreamStep::More
		}
		// The sender is done. The only ending that means the file is whole.
		BoundedVec::PeerClosed => StreamStep::Done(Ok(())),
		BoundedVec::TooLarge { .. } => StreamStep::Done(Err(if p.limit_is_policy { Error::Invalid } else { Error::Again })),
		BoundedVec::NoMemory { .. } => StreamStep::Done(Err(Error::Again)),
		BoundedVec::ReceiveError => StreamStep::Done(Err(Error::Invalid)),
		// Nothing there after all - the wait said readable, so this is a race with another reader
		// rather than an ending.
		BoundedVec::Idle => StreamStep::More,
	}
}

// Store what arrived (or give it up), close the stream, and answer the client that asked.
// Give up a pending write whose client is gone.
//
// `PendingWrite` remembers the channel to answer on as a bare handle. When that client is removed -
// on a quit message, on a stalled reply, or on a closed peer - the write went on regardless: it
// kept the single pending slot and the volume's memory, committed a file for a caller that no
// longer existed, and finally answered through a handle that had been closed. Today that send only
// fails; the day handle numbers are reused it delivers one client's answer to another.
//
// Nothing is sent. There is nobody to tell, and the number that named them may already mean
// something else.
// Answer a client, bounded. Returns whether the client STALLED and should be dropped.
//
// Every reply goes through here. The typed dispatch was given a deadline and called the last place
// a client could stop the service, which was not true: the heartbeat, both `CONNECT` answers, the
// second-listing refusal and the immediate stream refusal all answered through an unbounded send.
// A client that sends heartbeats and never reads them fills its queue, and the next `PONG` holds
// the whole service with no deadline at all.
//
// Handles it could not hand over are closed here, so a refused answer never leaks the capability
// it was carrying.
fn reply_to(chan: u64, bytes: &[u8], handles: &[u64], wait: bool) -> bool {
	matches!(reply_outcome(chan, bytes, handles, wait), SendOutcome::Stalled)
}

// The same send, with WHICH ending it had.
//
// `reply_to` collapses the three outcomes into "should the caller drop this subclient", which is
// all most callers need. It is not enough for an answer that carries a capability: `Delivered` and
// `Failed` both come back as `false`, so a handover to a peer that had already gone read as
// success, and the other end of the channel was left open with nobody who could ever take it.
fn reply_outcome(chan: u64, bytes: &[u8], handles: &[u64], wait: bool) -> SendOutcome {
	// `wait` is false for a client already known not to be reading: the send is attempted and
	// abandoned at once, so its backlog costs the service nothing but the attempts. A deadline of
	// the current tick is what "try once" looks like to `send_caps_deadline`; zero would mean
	// forever, which is the opposite.
	let sent = unsafe {
		let deadline = if wait { clock().saturating_add(REPLY_TICKS) } else { clock().max(1) };
		send_caps_deadline(chan, bytes, handles, deadline)
	};
	if !matches!(sent, SendOutcome::Delivered) {
		for &leftover in handles {
			unsafe { close(leftover) };
		}
	}
	sent
}

fn abandon_pending(set: u64, vol: &mut Volume, pending: &mut Option<PendingWrite>, chan: u64) {
	if pending.as_ref().is_none_or(|p| p.client != chan) {
		return;
	}
	let p = pending.take().expect("checked");
	if p.incremental {
		vol.fs.stream_abort();
	}
	leave_stream(set, p.stream_koid, p.stream);
}

// Take the stream out of the wait set and close it, in that order, and nowhere else.
//
// The ORDER used to be the whole function, and the interface now keeps it instead. `waitset_remove`
// took the HANDLE a member joined under, so closing first handed it a dead handle: the member stayed
// in the set, its closed peer was permanently READABLE, and the loop woke for a koid no member owned
// - forever. A livelock, so it showed up as a suite that never finished rather than one that failed.
// A diagnostic counted 12,917 of those wakes in three minutes before it was found, at the same test
// that hung the FIRST migration attempt in P02M0117 - which was reverted without a diagnosis, and may
// well have been this.
//
// Removal is by KOID now, which is the number `waitset_add` returned, so a closed handle cannot make
// a member unnameable. This function stays because there is still an order (leave, then close) and
// one place to keep it is better than four.
//
// The per-pass reconcile cannot do this job, and trying to make it was the mistake: by the time it
// notices `pending` is gone, whoever took it has already closed the handle. Joining can be noticed
// late. Leaving cannot.
fn leave_stream(set: u64, koid: u64, stream: u64) {
	let _ = unsafe { waitset_remove(set, koid) };
	unsafe { close(stream) };
}

// Finish a write stream and answer the client that asked for it. Returns the client's channel when
// it would not take that answer, so the caller drops the subclient like any other stall.
//
// The send was made bounded and its outcome then discarded, which fixed the permanent hang and left
// a slower leak in its place: a subclient whose queue is full at exactly this moment stays in the
// table holding its channel, and nothing ever revisits it. `reply_to` already reports a stall for
// every other answer in the service; this one had its own.
fn finish_stream(set: u64, vol: &mut Volume, p: PendingWrite, outcome: Result<(), Error>, quiet: bool, reply: &mut [u8]) -> Option<u64> {
	let result = match outcome {
		Ok(()) => {
			// The length BEFORE the bytes are handed over, because `write_file_owned` takes them.
			let published: u64 = p.received as u64;
			let existed: bool = match vol.writable_name(&p.path) {
				Ok(name) => vol.exists(name),
				Err(_) => false,
			};
			let landed = if p.incremental {
				vol.fs.stream_commit()
			} else {
				match vol.writable_name(&p.path) {
					Ok(name) => vol.fs.write_file_owned(name, p.bytes),
					Err(e) => Err(e),
				}
			};
			if landed.is_ok() {
				vol.note(if existed { FileEventKind::Modified } else { FileEventKind::Created }, &p.path, published);
			}
			landed
		}
		Err(e) => {
			if p.incremental {
				vol.fs.stream_abort();
			}
			Err(e)
		}
	};
	leave_stream(set, p.stream_koid, p.stream);
	if let Some(len) = write_stream_reply(p.corr, result, reply) {
		// The SAME answer path as every other reply, `quiet` and all.
		//
		// This used to call `send_deadline` directly, so it always waited out the full deadline even
		// for a client already known not to be reading - and when that client was the root, which is
		// never dropped, the flag was never set either. A root that first stalled here went on
		// costing every other client one deadline per queued request, which is the starvation the
		// flag exists to remove, reached through a different door. One answer path is what stops
		// there being a next door.
		if matches!(reply_outcome(p.client, &reply[..len], &[], !quiet), SendOutcome::Stalled) {
			return Some(p.client);
		}
	}
	None
}

// Drop a subclient that would not take its answer, wherever the stall was noticed.
//
// The root client is never dropped: it is the boot chain's channel, closing it ends the service,
// and a root that has stopped reading is the boot chain's problem rather than something to resolve
// by exiting. Index 0 is the root by construction - `serve_volume` seeds the table with it.
fn drop_stalled(set: u64, vol: &mut Volume, clients: &mut Vec<Client>, pending: &mut Option<PendingWrite>, chan: u64) {
	let Some(index) = clients.iter().position(|c| c.chan == chan) else { return };
	if index == 0 {
		// The root is never dropped - closing it ends the service - so what it gets instead is the
		// quiet flag, exactly as the request path gives it. Returning without setting it left the
		// root paying full deadlines for the rest of its backlog and everybody else waiting behind
		// them.
		clients[0].quiet = true;
		return;
	}
	abandon_pending(set, vol, pending, chan);
	release_client(set, clients, index);
	unsafe { close(chan) };
}

fn serve_volume(vol: &mut Volume, root: u64, mut admin: u64) -> ! {
	// The admin's own `quiet`, for the reason the clients have one.
	//
	// The admin channel is never dropped: there is one of it, it is the operator's way in, and
	// closing it turns a slow console into a system that cannot be administered. That decision is
	// right and it makes the flag MORE necessary rather than less - being undroppable is exactly the
	// condition under which a backlog costs everyone else a deadline per request, which is what the
	// root client demonstrated.
	let mut admin_quiet = false;

	// ONE registration per member, made when it joins, instead of one per member per pass.
	//
	// `wait_any` takes a fresh array of handles on every call, so the kernel registers a waiter on
	// every channel in it and takes them all out again - once per pass, for as long as this service
	// runs. Answering one client therefore costs more the more OTHER clients are connected, and
	// none of them asked for anything. Measured on 2026-08-09: 56,974 ns per round trip at four
	// clients, 133,811 at sixty-two - about 1,325 ns of tax per additional connection. That slope
	// is why `MAX_CLIENTS` is 64.
	let set: i64 = unsafe { waitset_create() };
	if set < 0 {
		unsafe { print(b"storage: cannot create the wait set; the service cannot serve\n") };
		unsafe { exit() };
	}
	let set: u64 = set as u64;

	let mut clients: Vec<Client> = Vec::new();
	if !admit_client(set, &mut clients, Client { chan: root, koid: 0, scope: Scope::Full, quiet: false, writer: None }) {
		unsafe { print(b"storage: cannot admit the root client; the service cannot serve\n") };
		unsafe { exit() };
	}
	// The admin joins once too, and leaves when its peer closes. Its koid is kept beside the handle
	// because the wait answers with koids and this comparison happens every pass.
	let mut admin_koid: u64 = 0;
	if admin != 0 {
		let koid = unsafe { waitset_add(set, admin) };
		if koid <= 0 {
			unsafe { print(b"storage: cannot watch the admin channel; the service cannot serve\n") };
			unsafe { exit() };
		}
		admin_koid = koid as u64;
	}
	// The stream's handle and koid while one is in flight; 0 when none is.
	//
	// Reconciled once per pass against `pending`, which is the one thing here that is NOT edited
	// where it changes: it is taken in four places and set in two, and threading a set handle
	// through all six is exactly the "missed call site" P02M0117 warns about. Reconciling ONE member
	// is two comparisons - it is the per-CLIENT reconcile that was quadratic and got the second
	// migration attempt reverted, and this is not that.
	let mut stream_chan: u64 = 0;
	let mut stream_koid: u64 = 0;

	let mut request: [u8; 1024] = [0u8; 1024];
	let mut reply: [u8; 4096] = [0u8; 4096];
	// At most one at a time. The memory filesystem accumulates a stream in the volume itself and
	// has room for one such write; a second would have to fall back to accumulating in this heap,
	// which is the accounting hole the incremental path exists to close. Refusing the second with
	// `Again` says something true and retryable rather than silently changing how it is charged.
	let mut pending: Option<PendingWrite> = None;
	// One listing in flight, for the same reason as the write: the service produces it between
	// passes, and a second would need its own slot and its own bound for no gain today.
	let mut listing: Option<PendingList> = None;
	// The watchers, which unlike the listing and the stream are MANY and long-lived: a watch lasts
	// until one side closes it, so there is no single slot to hold one in. They are not members of
	// the wait set - this service only ever SENDS on them, and a watcher that closed its end is
	// discovered by the send that fails rather than by a wake nobody would answer.
	let mut watchers: Vec<Watcher> = Vec::new();
	loop {
		// Hand out what the last pass produced. Here rather than at each mutation, because a
		// mutation happens behind the generated dispatch and this is the first point afterwards
		// that can see both the volume's outbox and the watcher table.
		deliver_events(&mut watchers, &mut vol.events);
		// Push what the consumer will take right now. Nothing here blocks, so a consumer that has
		// stopped reading costs this pass and nothing else.
		if let Some(l) = listing.as_mut() {
			if pump_list(l) {
				unsafe { close(l.producer) };
				listing = None;
			}
		}
		// The stream JOINS here and leaves in `leave_stream`, which is also the only place its
		// handle is closed - the two have to happen together and in that order, so they live in one
		// function. Joining is safe to notice late; leaving is not.
		let want_stream: u64 = pending.as_ref().map(|p| p.stream).unwrap_or(0);
		if want_stream != stream_chan {
			stream_koid = 0;
			if want_stream != 0 {
				let koid = unsafe { waitset_add(set, want_stream) };
				// A stream the set will not watch is one whose chunks would never wake this loop.
				// The client is told by the deadline rather than left waiting on a promise.
				if koid > 0 {
					stream_koid = koid as u64;
					// On the entry as well as in the loop, so whoever retires the stream can name it
					// without the handle - see `leave_stream`.
					if let Some(p) = pending.as_mut() {
						p.stream_koid = stream_koid;
					}
				}
			}
			stream_chan = want_stream;
		}
		// Nothing else to assemble. The membership is the set's, maintained where it CHANGES - a client
		// joining or leaving, a stream beginning or ending - instead of restated on every pass.
		// The stream is a member like everything else, so a chunk arriving and a new client asking
		// for something are the same kind of event and neither excludes the other.
		//
		// Wake for the stream's deadline even if nothing arrives, so a silent sender is given up on
		// rather than waited out.
		// A stalled listing polls rather than sleeps: `wait_any` applies one readiness sense to the
		// whole set, so a producer waiting for ROOM cannot be waited on beside clients waiting to
		// be READ. A tick at a time keeps the service responsive and costs a wake per tick only
		// while a consumer is actually behind. Mixed senses in `wait_any` would remove the poll.
		let deadline: u64 = match (pending.as_ref(), listing.is_some()) {
			(Some(p), false) => p.deadline(),
			(Some(p), true) => core::cmp::min(p.deadline(), unsafe { clock() }.saturating_add(1)),
			(None, true) => unsafe { clock() }.saturating_add(1),
			(None, false) => 0,
		};
		// PERIODIC, for the reason `recv_vec_deadline` gives: a plain timed wait counts as pending
		// progress, and `run_until_idle` then halts until the deadline whenever the run queue
		// empties. With a stream's deadline in the set, that means the peer which would SEND the
		// next chunk cannot run - the service sleeps thirty seconds and gives up on a sender that
		// was never given the chance to speak. This wait is a guard, not progress.
		let ready: i64 = unsafe { waitset_wait(set, deadline, WAIT_PERIODIC) };
		if ready < 0 {
			// A wait that TIMED OUT is ordinary; one that could not be performed is not, and
			// retrying it spins the loop at full speed until the deadline serving nobody. The
			// same distinction was drawn in `recv_vec_deadline` and then lost when the wait moved
			// up here, so a pending operation is given up on rather than retried.
			if ready != ERR_TIMED_OUT {
				if let Some(p) = pending.take() {
					let quiet = clients.iter().any(|c| c.chan == p.client && c.quiet);
					if let Some(stalled) = finish_stream(set, vol, p, Err(Error::Invalid), quiet, &mut reply) {
						drop_stalled(set, vol, &mut clients, &mut pending, stalled);
					}
				}
				if let Some(l) = listing.take() {
					unsafe { close(l.producer) };
				}
				continue;
			}
			if let Some(p) = pending.as_ref() {
				if unsafe { clock() } >= p.deadline() {
					let p = pending.take().expect("checked");
					let quiet = clients.iter().any(|c| c.chan == p.client && c.quiet);
					if let Some(stalled) = finish_stream(set, vol, p, Err(Error::Again), quiet, &mut reply) {
						drop_stalled(set, vol, &mut clients, &mut pending, stalled);
					}
				}
			}
			continue;
			// A stalled listing needs no branch here: the push at the top of the loop retries it,
			// and gives it up once its own deadline passes.
		}
		// `ready` is the KOID of whatever became ready, and the three things it can be are checked
		// in the order they are cheapest to rule out.
		let ready: u64 = ready as u64;
		if let Some(p) = pending.as_ref() {
			if stream_koid != 0 && ready == stream_koid {
				let _ = p;
				let mut p = pending.take().expect("checked");
				match take_chunk(vol, &mut p) {
					StreamStep::More => pending = Some(p),
					StreamStep::Done(result) => {
						let quiet = clients.iter().any(|c| c.chan == p.client && c.quiet);
						if let Some(stalled) = finish_stream(set, vol, p, result, quiet, &mut reply) {
							drop_stalled(set, vol, &mut clients, &mut pending, stalled);
						}
					}
				}
				continue;
			}
		}
		if admin != 0 && ready == admin_koid {
			match unsafe { recv_caps_blocking(admin, &mut request) } {
				ReceivedCaps::Message { len, handles: mut caps } => {
					let mut reply_handle = proto::codec::Handles::new();
					// EVERY CAPABILITY THE MESSAGE CARRIED. This was `Handles::from_slice(&[handle])`
					// over the single-handle receive, which keeps the first and drops the rest - so a
					// client sending stdin, stdout and stderr had two destroyed before dispatch.
					let mut handle = caps;
					let mut call = AdminCall { volume: vol, clients: &mut clients, set };
					if let Some(reply_len) = volume_admin::dispatch(&mut call, &request[..len], &mut handle, &mut reply, &mut reply_handle) {
						// Bounded like every other answer. Admin is privileged, so this is
						// availability rather than exposure - but "no unbounded send to a client
						// remains" is a claim that is either true or it is not, and an admin that
						// stops reading its replies held the whole service exactly as a client
						// would. `reply_to` closes the handles it could not hand over.
						//
						// A stall does NOT drop the admin channel: it is the operator's way in,
						// there is only one of it, and closing it turns a slow console into a
						// service that can no longer be administered at all.
						admin_quiet = match reply_outcome(admin, &reply[..reply_len], reply_handle.as_slice(), !admin_quiet) {
							SendOutcome::Stalled => true,
							// It took its answer, or the channel is gone and the `ReceivedCaps::Closed`
							// branch will retire it. Either way it is not a channel to keep skipping
							// the wait for.
							_ => false,
						};
					} else {
						for &leftover in reply_handle.as_slice() {
							unsafe { close(leftover) };
						}
					}
					for &unclaimed in handle.as_slice() {
						unsafe { close(unclaimed) };
					}
				}
				ReceivedCaps::Closed => {
					// Out of the set with it: a member whose handle is closed is a wake that can
					// never be answered.
					let _ = unsafe { waitset_remove(set, admin_koid) };
					admin = 0;
					admin_koid = 0;
				}
			}
			continue;
		}
		// By koid, which is what the wait answered with. The scan is the same one the channel
		// lookup was, over the same table - what has gone is the kernel doing sixty-two
		// registrations to tell us which entry to scan for.
		// A wake for a koid no member owns is not expected and must not spin. It happened - 12,917
		// times in three minutes - while a closed stream stayed in the set because its handle had
		// been closed before `waitset_remove` could name it, and a closed peer is permanently
		// readable. The loop is written so that cannot recur (`leave_stream`), and this stays a
		// `continue` rather than an assertion because a service does not get to abort over one.
		let Some(index) = clients.iter().position(|client| client.koid == ready) else { continue };
		let chan: u64 = clients[index].chan;
		let scope: Scope = clients[index].scope.clone();
		let quiet: bool = clients[index].quiet;
		match unsafe { recv_caps_blocking(chan, &mut request) } {
			ReceivedCaps::Message { len, .. } if len == 0 => {
				if index == 0 {
					exit();
				}
				abandon_pending(set, vol, &mut pending, chan);
				// The set FIRST, the handle after - and `release_client` is the only place that knows it,
				// because removal is by KOID and the koid lives in the client table beside the handle.
				// When removal took the handle, closing first left the member in the set and a closed peer
				// is permanently READABLE: the set woke, no client matched the koid, the loop continued, and
				// it woke again at once. A livelock rather than a deadlock, which is why it presented as a
				// test that never finished instead of one that stopped.
				release_client(set, &mut clients, index);
				unsafe { close(chan) };
			}
			ReceivedCaps::Message { len, handles: mut caps } => {
				// EVERY CAPABILITY THE MESSAGE CARRIED. This was `Handles::from_slice(&[handle])`
				// over the single-handle receive, which keeps the first and drops the rest - so a
				// client sending stdin, stdout and stderr had two destroyed before dispatch.
				let mut handle = caps;
				// Set when a client would not take its reply within the bound. It is dropped below:
				// a client that has stopped reading is gone for every practical purpose, and the
				// alternative is holding the whole service for it.
				let mut stalled = false;
				let op: u16 = if len >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
				// A WRITER SESSION SPEAKS `writer` AND NOTHING ELSE, and it is asked first because
				// the two interfaces number their ops from one: `writer.write-at` is 2 and so is
				// `volume.list`, so a session's positioned write was being answered by the
				// directory streamer. Deciding by the CLIENT rather than by the op is the only
				// arrangement that cannot collide, and it is what the contract says anyway - the
				// channel `open-writer` returns speaks one interface.
				//
				// Its path was checked when the session opened, so `scope` is not consulted again:
				// there is one path this client can write to and it cannot name another.
				if clients[index].writer.is_some() {
					let mut reply_handle = proto::codec::Handles::new();
					let reply_len: Option<usize> = {
						let session = clients[index].writer.as_mut().expect("checked");
						let mut call = WriterCall { vol, session };
						writer::dispatch(&mut call, &request[..len], &mut handle, &mut reply, &mut reply_handle)
					};
					if let Some(reply_len) = reply_len {
						stalled = reply_to(chan, &reply[..reply_len], reply_handle.as_slice(), !quiet);
					} else {
						for &leftover in reply_handle.as_slice() {
							unsafe { close(leftover) };
						}
					}
				} else if op == HEARTBEAT_OP {
					stalled = reply_to(chan, b"PONG", &[], !quiet);
				} else if op == CONNECT_OP {
					// An empty reply with no handle is this call's refusal form, and a table that is
					// full uses it like a channel that could not be created.
					match unsafe { channel() } {
						Some((server, client)) if admit_client(set, &mut clients, Client { chan: server, koid: 0, scope, quiet: false, writer: None }) => {
							stalled = reply_to(chan, &[], &[client], !quiet);
						}
						Some((server, client)) => {
							unsafe {
								close(server);
								close(client);
							}
							stalled = reply_to(chan, &[], &[], !quiet);
						}
						None => stalled = reply_to(chan, &[], &[], !quiet),
					}
				} else {
					// Stamp mutations before authorization and dispatch. The clock is a no-op on
					// read-only backends, while denied requests never reach their filesystem.
					vol.fs.set_clock(unsafe { clock_rtc() });
					if op == volume::OP_LIST {
						// A second listing while one is in flight is refused, like a second stream.
						if listing.is_some() {
							let corr = if len >= 6 { u32::from_le_bytes([request[2], request[3], request[4], request[5]]) } else { 0 };
							// `again`, and it now SAYS `again`: one listing at a time is a statement
							// about this moment and the caller may retry. It used to be a bare
							// correlation id, which the client could not tell from a directory that
							// is not there.
							let mut body: [u8; 32] = [0u8; 32];
							stalled = match volume::list_reply_err(corr, &Error::Again, &mut body) {
								Some(n) => reply_to(chan, &body[..n], &[], !quiet),
								None => false,
							};
						} else {
							match stream_list(vol, chan, &scope, quiet, &request[..len], &mut handle) {
								ListStart::Started(started) => listing = Some(started),
								ListStart::Done => {}
								ListStart::ClientStalled => stalled = true,
							}
						}
					} else if op == volume::OP_WRITE_STREAM {
						// Registered rather than received. `begin_stream` answers the client only
						// when it refuses; otherwise the reply waits until the stream ends, and the
						// loop goes straight back to serving everyone else.
						match begin_stream(vol, chan, &scope, &request[..len], &mut handle, pending.is_some()) {
							Ok(entry) => pending = Some(entry),
							Err((corr, e)) => {
								if let Some(reply_len) = write_stream_reply(corr, Err(e), &mut reply) {
									stalled = reply_to(chan, &reply[..reply_len], &[], !quiet);
								}
							}
						}
					} else if op == volume::OP_WATCH {
						match start_watch(vol, chan, &scope, quiet, &request[..len], &mut handle, &mut watchers) {
							ListStart::ClientStalled => stalled = true,
							ListStart::Started(_) | ListStart::Done => {}
						}
					} else {
						let mut reply_handle = proto::codec::Handles::new();
						let reply_len: Option<usize> = if scope.allows_request(vol.name(), &request[..len]) {
							let mut call = VolumeCall { vol, clients: &mut clients, set, scope: scope.clone() };
							volume::dispatch(&mut call, &request[..len], &mut handle, &mut reply, &mut reply_handle)
						} else {
							denied_reply(&request[..len], &mut reply)
						};
						if let Some(reply_len) = reply_len {
							// Bounded, and a client that will not take its answer is dropped below
							// rather than waited for.
							stalled = reply_to(chan, &reply[..reply_len], reply_handle.as_slice(), !quiet);
						} else {
							for &leftover in reply_handle.as_slice() {
								unsafe { close(leftover) };
							}
						}
					}
				}
				if stalled {
					// Never the root client: dropping that ends the service, and a stalled root is
					// the boot chain's problem rather than something to resolve by exiting. What it
					// gets instead is the quiet flag, so the REST of its backlog is answered without
					// waiting and nobody else pays for it.
					if index != 0 {
						abandon_pending(set, vol, &mut pending, chan);
						// The set FIRST, the handle after - `release_client` keeps that order, and removal by
						// koid means a closed handle can no longer make a member unnameable. See the note at
						// the first of these.
						release_client(set, &mut clients, index);
						unsafe { close(chan) };
					} else {
						clients[0].quiet = true;
					}
				} else if let Some(client) = clients.get_mut(index) {
					// It took its answer, so it is reading again. Cleared on the way through rather
					// than probed for: the send is the probe.
					if client.chan == chan {
						client.quiet = false;
					}
				}
				for &unclaimed in handle.as_slice() {
					unsafe { close(unclaimed) };
				}
			}
			ReceivedCaps::Closed => {
				if index == 0 {
					exit();
				}
				abandon_pending(set, vol, &mut pending, chan);
				// The set FIRST, the handle after - and `release_client` is the only place that knows it,
				// because removal is by KOID and the koid lives in the client table beside the handle.
				// When removal took the handle, closing first left the member in the set and a closed peer
				// is permanently READABLE: the set woke, no client matched the koid, the loop continued, and
				// it woke again at once. A livelock rather than a deadlock, which is why it presented as a
				// test that never finished instead of one that stopped.
				release_client(set, &mut clients, index);
				unsafe { close(chan) };
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
// A listing the service has started and not finished.
//
// The entries are produced one at a time between passes of the serve loop, for the same reason a
// write stream takes one chunk at a time: a consumer that does not read must not hold the service.
// It used to send them with an unbounded blocking send, and a client that took the handle and
// stopped reading stopped StorageService permanently.
struct PendingList {
	producer: u64,
	items: Vec<FileInfo>,
	seq: usize,
	// Absolute ticks. A consumer that never drains is given up on rather than waited for.
	expires: u64,
}

// Push as many entries as the consumer will take right now, without blocking.
//
// Returns true when the listing is finished (or abandoned) and the caller should drop it. The
// service comes back here on the next pass, so a consumer that drains slowly still gets everything
// while everyone else keeps being served.
fn pump_list(p: &mut PendingList) -> bool {
	let mut frame: [u8; 1024] = [0u8; 1024];
	// One past the last entry is the TERMINAL frame: an empty message meaning "that was all".
	//
	// Without it a listing given up on looks exactly like one that finished - the producer closes
	// either way, and a closed channel is what "done" has always meant - so a client could take the
	// first 64 entries of a large directory for the whole of it. That is "a short listing looks
	// complete" for the third time, from a third cause, and it cannot be fixed in the producer
	// alone: closing carries no information, so completion has to be said rather than implied.
	while p.seq <= p.items.len() {
		if p.seq == p.items.len() {
			return match unsafe { try_send_outcome(p.producer, &[], 0) } {
				SendOutcome::Delivered | SendOutcome::Failed => true,
				SendOutcome::Stalled => (unsafe { clock() }) >= p.expires,
			};
		}
		let mut frame_handles = Handles::new();
		let Some(n) = volume::list_frame(p.seq as u32, &p.items[p.seq], &mut frame, &mut frame_handles) else {
			// An entry that will not encode ENDS the listing, without the terminal frame, so the
			// client is told the answer is incomplete. Skipping it produced a directory listing
			// that silently lacked a name - the same defect as a truncated one, one entry at a
			// time, and the reader cannot see the gap because it ignores the sequence number.
			for handle in frame_handles.as_slice() {
				unsafe { close(*handle) };
			}
			return true;
		};
		match unsafe { try_send_caps_outcome(p.producer, &frame[..n], frame_handles.as_slice()) } {
			SendOutcome::Delivered => {
				p.seq += 1;
				continue;
			}
			// Nobody is there. Waiting out the deadline for a consumer that has closed wakes the
			// loop every tick and refuses the next listing, for nothing.
			SendOutcome::Failed => {
				for handle in frame_handles.as_slice() {
					unsafe { close(*handle) };
				}
				return true;
			}
			// The queue is full. Give up only once the consumer has had long enough; otherwise
			// come back on the next pass.
			SendOutcome::Stalled => {
				for handle in frame_handles.as_slice() {
					unsafe { close(*handle) };
				}
				return unsafe { clock() } >= p.expires;
			}
		}
	}
	true
}

// A live watcher: the producer end of its event stream and the path it asked about.
struct Watcher {
	producer: u64,
	// The vol:// path. A watch on a file matches that file; a watch on a directory matches the
	// entries directly below it and the directory itself.
	path: String,
	seq: u32,
}

// The most watchers this service will hold at once.
//
// Small on purpose. A watcher costs a channel, a path and a send per event to every one of them,
// so the cost of a mutation grows with this number - and nothing in the system needs many. A
// client refused here is told `again`, which is true: watchers end.
const MAX_WATCHERS: usize = 16;

impl Watcher {
	// Whether this watcher asked about `path`: the path itself, or an entry directly below it.
	//
	// DIRECTLY below, not anywhere below. A subtree watch would report a change three directories
	// down to a watcher of the root, which is the shape that makes an event stream unbounded in a
	// way its consumer cannot predict - and `tail -f` and a directory listing both want exactly
	// this. A client that wants a subtree watches the directories it cares about.
	fn matches(&self, path: &str) -> bool {
		if self.path == path {
			return true;
		}
		match path.strip_prefix(self.path.as_str()).and_then(|rest| rest.strip_prefix('/')) {
			Some(entry) => !entry.contains('/'),
			None => false,
		}
	}
}

// Hand every pending event to the watchers that asked for it, dropping the ones that cannot take
// it. Returns nothing: there is no caller that could do anything about a watcher that has gone.
//
// A watcher that will not read is DROPPED rather than waited for or buffered. Waiting is the defect
// this service spent a milestone removing from every other send; buffering is unbounded growth
// driven by a client that stopped listening. Its end of the stream closing is how it learns, and
// the contract says what to do about it: re-read, rather than reconstruct from the events.
fn deliver_events(watchers: &mut Vec<Watcher>, events: &mut Vec<FileEvent>) {
	if watchers.is_empty() {
		events.clear();
		return;
	}
	let mut frame: [u8; 1024] = [0u8; 1024];
	for event in events.drain(..) {
		let mut at: usize = 0;
		while at < watchers.len() {
			if !watchers[at].matches(&event.path) {
				at += 1;
				continue;
			}
			let mut frame_handles = Handles::new();
			let delivered: bool = match volume::watch_frame(watchers[at].seq, &event, &mut frame, &mut frame_handles) {
				Some(n) => {
					for handle in frame_handles.as_slice() {
						unsafe { close(*handle) };
					}
					matches!(unsafe { try_send_outcome(watchers[at].producer, &frame[..n], 0) }, SendOutcome::Delivered)
				}
				// An event that will not encode ends the watch rather than being skipped: a
				// consumer that ignores the sequence number cannot see a gap, so a silently
				// dropped event is a watcher that believes it is up to date and is not.
				None => {
					for handle in frame_handles.as_slice() {
						unsafe { close(*handle) };
					}
					false
				}
			};
			if delivered {
				watchers[at].seq = watchers[at].seq.saturating_add(1);
				at += 1;
			} else {
				let gone = watchers.swap_remove(at);
				unsafe { close(gone.producer) };
			}
		}
	}
}

// Start a watch, or refuse it. Shaped like `stream_list`, which is the other op that answers with a
// sub-channel: the reply carries the consumer end and the producer stays here.
fn start_watch(vol: &mut Volume, service: u64, scope: &Scope, quiet: bool, request: &[u8], request_handle: &mut proto::codec::Handles, watchers: &mut Vec<Watcher>) -> ListStart {
	let mut reader = proto::codec::Reader::with_handle_list(request, request_handle);
	let r = &mut reader;
	let (corr, path): (u32, String) = match (|| Some((r.u16()?, r.u32()?, r.string_lp()?)))() {
		Some((_op, corr, path)) => (corr, path),
		None => return ListStart::Done,
	};
	if r.has_handle() {
		return ListStart::Done;
	}
	request_handle.clear();
	let refuse = |chan: u64, error: Error| -> ListStart {
		let mut body: [u8; 32] = [0u8; 32];
		match volume::watch_reply_err(corr, &error, &mut body) {
			None => ListStart::Done,
			Some(n) => {
				if reply_to(chan, &body[..n], &[], !quiet) {
					ListStart::ClientStalled
				} else {
					ListStart::Done
				}
			}
		}
	};
	if !scope.allows_path(vol.name(), &path) {
		return refuse(service, Error::Denied);
	}
	// THE PATH HAS TO EXIST. A watch on a name that is not there would otherwise be the way to
	// watch for a creation, and it is not: the events for a creation carry the parent's listing, so
	// the answer to "tell me when this appears" is to watch the directory. Accepting it here would
	// hold a slot for a typo forever.
	let target: VolumePath = match VolumePath::parse(path.as_bytes()) {
		Some(target) if target.volume == vol.name() => target,
		_ => return refuse(service, Error::NotFound),
	};
	let is_root: bool = target.path.as_bytes().is_empty();
	if !is_root && vol.fs.stat_entry(target.path.as_bytes()).is_err() {
		return refuse(service, Error::NotFound);
	}
	if watchers.len() >= MAX_WATCHERS || watchers.try_reserve(1).is_err() {
		return refuse(service, Error::Again);
	}
	let owned = match to_string(&path) {
		Ok(owned) => owned,
		Err(e) => return refuse(service, e),
	};
	let (producer, consumer): (u64, u64) = match unsafe { channel() } {
		Some(pair) => pair,
		None => return refuse(service, Error::Again),
	};
	let mut ok_body: [u8; 16] = [0u8; 16];
	let Some(ok_len) = volume::watch_reply_ok(corr, &mut ok_body) else {
		unsafe {
			close(producer);
			close(consumer);
		}
		return ListStart::Done;
	};
	match reply_outcome(service, &ok_body[..ok_len], &[consumer], !quiet) {
		SendOutcome::Delivered => {}
		SendOutcome::Stalled => {
			unsafe { close(producer) };
			return ListStart::ClientStalled;
		}
		SendOutcome::Failed => {
			unsafe { close(producer) };
			return ListStart::Done;
		}
	}
	watchers.push(Watcher { producer, path: owned, seq: 0 });
	ListStart::Done
}

// How a listing request ended. Three outcomes rather than `Option`, because bounding the answer
// introduces a third: the client that will not take it.
enum ListStart {
	// Under way; the serve loop pumps it between passes.
	Started(PendingList),
	// Finished here - refused, unreadable, or answered outright. Nothing for the loop to do.
	Done,
	// The client did not take its answer within the deadline. The loop drops that subclient,
	// exactly as it does for every other reply.
	ClientStalled,
}

// Begin a listing: answer the client with the consumer end of a fresh channel, and hand the
// producer back to the serve loop to push entries into.
//
// Every answer here is BOUNDED. This function was the counterexample to the claim recorded in
// P02M0109 that no unbounded send to a client remained in the service: the loop had been converted to
// `reply_to` and this had not, so it still answered through three `send_blocking` calls. The
// scenario that mattered is the one the deadline exists for - a client fills its reply queue, a
// previous `reply_to` times out, the client is NOT dropped (one stalled reply is not proof of
// death), and its next request is a listing. The service then blocked against the same full queue
// with no deadline at all, and stopped serving anybody.
fn stream_list(vol: &mut Volume, service: u64, scope: &Scope, quiet: bool, request: &[u8], request_handle: &mut proto::codec::Handles) -> ListStart {
	let mut reader = proto::codec::Reader::with_handle_list(request, request_handle);
	let r = &mut reader;
	let (corr, path): (u32, String) = match (|| Some((r.u16()?, r.u32()?, r.string_lp()?)))() {
		Some((_op, corr, path)) => (corr, path),
		// unreadable: nothing to correlate an answer with, so there is no answer to send.
		None => return ListStart::Done,
	};
	if r.has_handle() {
		return ListStart::Done;
	}
	request_handle.clear();
	// THE REFUSAL SAYS WHY, which is what the schema's error arm is for. It used to be a
	// correlation id and nothing else - a reply the client could only read as "no stream", with a
	// directory that is not there, a path outside the grant and a volume that could not be read all
	// looking identical. `list_reply_err` encodes the error the caller can act on.
	//
	// Every path below that cannot produce a stream goes through it, including the two that once
	// answered nothing at all and left the client waiting on a listing that was never coming.
	let refuse = |chan: u64, error: Error| -> ListStart {
		let mut body: [u8; 32] = [0u8; 32];
		match volume::list_reply_err(corr, &error, &mut body) {
			// An error that will not encode is not a reason to send a malformed reply; the client's
			// call fails, which is the same outcome by a coarser route.
			None => ListStart::Done,
			Some(n) => {
				if reply_to(chan, &body[..n], &[], !quiet) {
					ListStart::ClientStalled
				} else {
					ListStart::Done
				}
			}
		}
	};
	if !scope.allows_path(vol.name(), &path) {
		return refuse(service, Error::Denied);
	}
	let items: Vec<FileInfo> = match vol.list_entries(&path) {
		Ok(items) => items,
		Err(e) => return refuse(service, e),
	};
	let (producer, consumer): (u64, u64) = match unsafe { channel() } {
		Some(pair) => pair,
		// No channel to give: the host is out of resources, not the volume.
		None => return refuse(service, Error::Again),
	};
	// The Ok body: the correlation id, the tag, and the handle's placeholder - the consumer end
	// itself travels in the reply's handle list below.
	let mut ok_body: [u8; 16] = [0u8; 16];
	let Some(ok_len) = volume::list_reply_ok(corr, &mut ok_body) else {
		unsafe { close(producer) };
		unsafe { close(consumer) };
		return ListStart::Done;
	};
	// CHECKED, and it is the send that most needs checking, because it carries a capability. If the
	// client closed the main channel just after asking, or simply will not read, the consumer
	// handle is never delivered - `reply_to` closes what it could not hand over, and the producer
	// is this function's to close. Leaving them open leaked the capability AND left the producer
	// with a live peer nobody would ever read, so the next send blocked forever.
	match reply_outcome(service, &ok_body[..ok_len], &[consumer], !quiet) {
		SendOutcome::Delivered => {}
		// The consumer is already closed by `reply_outcome`; the producer is this function's, and
		// a producer whose peer is gone is a handle nobody will ever take back.
		SendOutcome::Stalled => {
			unsafe { close(producer) };
			return ListStart::ClientStalled;
		}
		SendOutcome::Failed => {
			unsafe { close(producer) };
			return ListStart::Done;
		}
	}
	// Handed back to the serve loop rather than produced here. Sending the entries in place is what
	// let one unreading consumer stop the service; the loop pushes what fits between passes.
	ListStart::Started(PendingList { producer, items, seq: 0, expires: unsafe { clock() }.saturating_add(STREAM_IDLE_TICKS) })
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
	// The mounted filesystem, mounting on first use.
	//
	// Extracted because `write_plan` did not do it and every other operation did. A volume whose
	// FIRST request was a write answered `NotFound` from an unmounted backing, and started working
	// only once a `list`, `read` or `status` had mounted it as a side effect - so `write
	// vol://media/file` failed after boot and succeeded after anything else. Both paths reach the
	// filesystem the same way now, and a third cannot forget.
	fn ensure_mounted(&mut self) -> Result<&mut FatFs<FatBlockDevice>, Error> {
		if self.fs.is_none() {
			// `mount_checked`, so the REASON survives. `mount` answers `Option`, and this mapped
			// every failure to `NotFound` - so a cable that stopped answering told the operator the
			// volume was not there, and a medium this build cannot read told them the same thing.
			// The three answers below send somebody somewhere different, which is the whole point
			// of the distinction existing.
			match FatFs::mount_checked(FatBlockDevice { chan: self.chan }) {
				Ok(fs) => self.fs = Some(fs),
				// The device did not answer, or the memory was not there: both are worth retrying
				// and neither says anything about the medium.
				Err(fat::MountError::Io | fat::MountError::NoMemory) => return Err(Error::Again),
				// A medium this build cannot read, or one whose own structures failed their own
				// checks. Retrying changes nothing.
				Err(fat::MountError::Unsupported | fat::MountError::Corrupt) => return Err(Error::Invalid),
				// Nothing here claims to be FAT, which is the one answer that really is "not found".
				Err(fat::MountError::NotFat) => return Err(Error::NotFound),
			}
		}
		self.fs.as_mut().ok_or(Error::NotFound)
	}

	fn run<R>(&mut self, op: impl FnOnce(&mut FatFs<FatBlockDevice>) -> Result<R, FsError>) -> Result<R, Error> {
		let fs: &mut FatFs<FatBlockDevice> = self.ensure_mounted()?;
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

// The most one `read` window may deliver.
//
// A ceiling and not a refusal: `volume.read` clamps rather than refusing, so this number is a
// property of the service that a client never has to know. It is one megabyte because the window
// is copied once into the service's heap and once into the buffer it hands back, and a client that
// wants more asks again - which is the loop it is already writing.
const READ_WINDOW_MAX: usize = 1024 * 1024;

// The most clients this service will hold at once, counting the root.
//
// There was no limit and no fallible allocation: every `CONNECT` did `clients.push(...)` and every
// pass through the event loop built a fresh wait set with `Vec::with_capacity`. Any holder of the
// service capability could grow both without bound - and the allocator's answer to running out is
// to abort the process, so the service that has been hardened against one client's stalls all the
// way through this milestone could still be killed by a client that simply asks a lot.
//
// DERIVED from the wait set's own limit, less the admin channel and the write stream.
//
// It was sixty-four, and that number was measured around a defect rather than chosen: `wait_any`
// took a fresh array of handles on every pass, so the kernel registered a waiter on every channel
// and removed them all again, and answering one client cost more the more OTHERS were connected.
// Sixty-four is where the service was still brisk. P02M0117 removed that cost - one registration per
// member, made when it joins - and the slope fell from 1,325 ns per additional client to 526.
//
// So the ceiling stops being a performance number and becomes a structural one: a client is a
// member of the set, and the set holds `MAX_WAIT_SET_MEMBERS`. Two are spoken for.
const MAX_CLIENTS: usize = rt::MAX_WAIT_SET_MEMBERS - 2;

// Admit a client into the table AND into the wait set, or say why not.
//
// The two are ONE operation and this is the only place either happens. A client in the table the
// set does not know about never gets served; a member of the set with no table entry is a wake
// nobody can answer. Membership drifting apart is the failure P02M0117 warned about by name - "a
// missed call site is a silently stale set that serves the WRONG client" - and the way to not miss
// a call site is to have one.
//
// Fallible on three counts a `push` is not: the table has a ceiling, the growth that carries it
// there can be refused, and the kernel can refuse the registration.
fn admit_client(set: u64, clients: &mut Vec<Client>, mut client: Client) -> bool {
	if clients.len() >= MAX_CLIENTS {
		return false;
	}
	if clients.try_reserve(1).is_err() {
		return false;
	}
	let koid = unsafe { waitset_add(set, client.chan) };
	if koid <= 0 {
		return false;
	}
	client.koid = koid as u64;
	clients.push(client);
	true
}

// Take a client out of both, in the order that cannot leave a wake for a member that is gone: the
// set first, the table second, the handle last.
fn release_client(set: u64, clients: &mut Vec<Client>, index: usize) -> Client {
	let _ = unsafe { waitset_remove(set, clients[index].koid) };
	clients.swap_remove(index)
}

// A path split into the directory that holds it and its final component - the two halves `stat`
// needs to answer from a listing. An empty directory is the volume root, which is the same
// convention `list` uses.
fn split_parent(name: &[u8]) -> (&[u8], &[u8]) {
	match name.iter().rposition(|&byte| byte == b'/') {
		Some(at) => (&name[..at], &name[at + 1..]),
		None => (&[], name),
	}
}

trait FileSystem {
	// The vol:// volume name this backend answers to.
	fn volume_name(&self) -> &'static [u8];
	// Stamp the wall clock before a mutation, so a written inode's timestamps carry real
	// time. Only the writable native filesystem tracks it; the default is a no-op.
	fn set_clock(&mut self, _unix_secs: u64) {}
	// Read a whole file by its in-volume path.
	fn read_file(&mut self, name: &[u8]) -> Result<Vec<u8>, Error>;
	// Read at most `len` bytes from `offset`, for a client reading a file in windows.
	//
	// The DEFAULT reads the whole file and slices it, which is correct on every backend and
	// bounded by nothing - so it is the default rather than the answer. A backend that can seek
	// overrides it, and the two that carry the large files (LiberFS and FAT) do; the archive and
	// memory backends hold their bytes in memory already, so slicing them costs a copy of the
	// window rather than a read of the file.
	//
	// A window past the end is EMPTY rather than an error: see the contract at `volume.read`.
	fn read_window(&mut self, name: &[u8], offset: u64, len: usize) -> Result<Vec<u8>, Error> {
		let file: Vec<u8> = self.read_file(name)?;
		let start: usize = core::cmp::min(offset, file.len() as u64) as usize;
		let end: usize = core::cmp::min(start.saturating_add(len), file.len());
		let mut window: Vec<u8> = Vec::new();
		window.try_reserve_exact(end - start).map_err(|_| Error::Again)?;
		window.extend_from_slice(&file[start..end]);
		Ok(window)
	}
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

	// Receive a stream INTO the filesystem, chunk by chunk, rather than accumulating it outside.
	//
	// The default is "not supported", and a caller that gets `None` falls back to accumulating -
	// which is right for a disk, where the bytes are going to a medium with its own space and the
	// service's heap is only a staging area. It matters for the memory filesystem, where the
	// accumulator and the destination compete for the SAME memory and the volume's accounting
	// could not see half of it: `free()` reported room the service had already spent.
	// What a STREAM to this path may carry. Defaults to the ordinary plan, which is right for every
	// backend whose bytes are bound for a medium: only the memory filesystem pays for holding the
	// old contents and the new ones at once.
	fn stream_plan(&mut self, name: &[u8]) -> WritePlan {
		self.write_plan(name)
	}
	fn stream_begin(&mut self, _name: &[u8]) -> Option<Result<(), Error>> {
		None
	}
	fn stream_push(&mut self, _chunk: &[u8]) -> Result<(), Error> {
		Err(Error::Invalid)
	}
	// Space to receive into, so a chunk is not allocated twice - once by the transport and once by
	// the destination.
	fn stream_spare(&mut self, _want: usize) -> Option<Result<&mut [u8], Error>> {
		None
	}
	// Closes the outstanding offer. `Err` means the protocol was broken - an offer closed that was
	// never opened, or closed with a length that does not match the one handed out - which is a bug
	// in this file rather than a condition the medium can produce, and is surfaced rather than
	// dropped so it cannot become a file full of zeros nobody wrote.
	// Closes the outstanding offer. `Err` means the protocol was broken - an offer closed with a
	// length that does not match the one handed out - which is a bug in this file rather than a
	// condition the medium can produce, and is surfaced rather than dropped so it cannot become a
	// file full of zeros nobody wrote.
	//
	// The default is `Ok`, because a backend that does not implement `stream_spare` never opens an
	// offer and so has none to close. Making the default an error took every service that streams
	// offline at boot, which is the loudest possible way to learn that "no offer" and "a broken
	// offer" are different answers.
	fn stream_advance(&mut self, _offered: usize, _written: usize) -> Result<(), Error> {
		Ok(())
	}
	fn stream_commit(&mut self) -> Result<(), Error> {
		Err(Error::Invalid)
	}
	fn stream_abort(&mut self) {}

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

	// ONE FILE'S FACTS, without reading the directory that holds it.
	//
	// The default answers from `list_entries`, which is what every caller had to do by hand: read
	// the parent and search it. That is correct on any backend and wrong only in cost, so it is the
	// DEFAULT rather than the answer - a backend that can look one entry up directly overrides it,
	// and the native filesystem does.
	fn stat_entry(&mut self, name: &[u8]) -> Result<FileInfo, Error> {
		let (dir, base) = split_parent(name);
		let entries = self.list_entries(dir)?;
		entries.into_iter().find(|entry| entry.name.as_bytes() == base).ok_or(Error::NotFound)
	}

	// The mutations a read-only or incapable backend refuses BY NAME rather than by silence.
	//
	// `Invalid` and not `Denied`: denied is "you may not", and these backends would refuse the same
	// operation from anybody. An ISO9660 volume cannot rename a file because the medium has no way
	// to express it, which is a statement about the backend rather than about the caller.
	fn rename(&mut self, _from: &[u8], _to: &[u8]) -> Result<(), Error> {
		Err(Error::Invalid)
	}
	fn truncate(&mut self, _name: &[u8], _length: u64) -> Result<(), Error> {
		Err(Error::Invalid)
	}
	fn touch(&mut self, _name: &[u8], _create: bool) -> Result<(), Error> {
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
	// Mutations this service has performed and not yet handed to the watchers.
	//
	// An OUTBOX rather than a push, because a mutation happens behind the generated `Service`
	// trait, which is handed the volume and nothing else - the watcher table belongs to the serve
	// loop. The loop drains this after every request, so an event's life here is one dispatch long.
	//
	// Bounded like everything else: one request produces at most two events, and one that would
	// pass the bound is DROPPED rather than grown. That is the honest failure for this contract -
	// a watcher is told to re-read rather than to reconstruct state from the sequence - and it is
	// the reason `watch` does not promise every event, only the ones it delivers.
	events: Vec<FileEvent>,
}

// The most events one request may leave behind. Two is what the largest mutation (a rename)
// produces; the rest is headroom for a request that mutates more than once, and the ceiling
// exists so a backend that started emitting per-block events could not grow this without bound.
const MAX_PENDING_EVENTS: usize = 8;

impl Volume {
	fn new(fs: alloc::boxed::Box<dyn FileSystem>) -> Volume {
		Volume { fs, events: Vec::new() }
	}

	// The vol:// name this backing answers to (its backend's).
	fn name(&self) -> &'static [u8] {
		self.fs.volume_name()
	}

	// Record a change for the watchers, or drop it if the outbox is full.
	//
	// Dropping is deliberate and is why this returns nothing: the alternative is failing a
	// mutation that has already happened because nobody could be told about it, which would make
	// watching a way to make writes fail.
	fn note(&mut self, kind: FileEventKind, path: &str, size: u64) {
		if self.events.len() >= MAX_PENDING_EVENTS || self.events.try_reserve(1).is_err() {
			return;
		}
		let mut owned = String::new();
		if owned.try_reserve_exact(path.len()).is_err() {
			return;
		}
		owned.push_str(path);
		self.events.push(FileEvent { path: owned, kind, size });
	}

	// Whether a path exists now, asked before a mutation so the event it produces can say whether
	// the file was created or changed. A backend that cannot answer is treated as "not there",
	// which reports a modification as a creation - the direction that tells a watcher to look.
	fn exists(&mut self, name: &[u8]) -> bool {
		self.fs.stat_entry(name).is_ok()
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
	// never has to fit one reply.
	//
	// A DIRECTORY THAT COULD NOT BE READ IS AN ERROR, not an empty listing. It was the latter for
	// as long as the schema said `stream<file-info>` with no error arm, and that was the one
	// "a failure looks like an empty answer" case this contract had left - kept alive by a
	// generator that emitted nothing for `result<stream<T>, error>` rather than by anything here.
	fn list(&mut self, path: String) -> Result<Vec<FileInfo>, Error> {
		self.list_entries(&path)
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
		// Asked BEFORE the write, because afterwards every path exists and the answer would always
		// be `modified` - a watcher waiting for a file to appear would never hear that it had.
		let existed: bool = self.exists(name);
		self.fs.write_file(name, buffer.as_slice())?;
		self.note(if existed { FileEventKind::Modified } else { FileEventKind::Created }, &path, data.len);
		Ok(())
	}

	// UNREACHABLE. The serve loop intercepts `OP_WRITE_STREAM` and registers a pending write, so
	// nothing dispatches here; the method exists because the generated trait requires it.
	//
	// It used to carry a SECOND, synchronous implementation of the same protocol - receive the
	// whole stream, then store it - kept "so the direct-dispatch path still behaves". Nothing takes
	// that path: the only direct dispatch in the tree is a stub in the protocol tests. Two
	// implementations of one protocol, with different orderings and different bounds, is a drift
	// waiting to happen, so the unreachable one is gone rather than maintained.
	fn write_stream(&mut self, _path: String, data: u64) -> Result<(), Error> {
		if data != 0 {
			unsafe { close(data) };
		}
		Err(Error::Invalid)
	}

	// Delete a file. A read-only volume refuses with `denied`.
	fn remove(&mut self, path: String) -> Result<(), Error> {
		let name: &[u8] = self.writable_name(&path)?;
		self.fs.remove(name)?;
		self.note(FileEventKind::Removed, &path, 0);
		Ok(())
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
		self.fs.mkdir(name)?;
		self.note(FileEventKind::Created, &path, 0);
		Ok(())
	}

	// Remove the empty directory at a vol:// path. Only the writable LiberFS volume
	// supports it; the read-only archive refuses with `denied`, the other backends with
	// `invalid`.
	fn rmdir(&mut self, path: String) -> Result<(), Error> {
		let name: &[u8] = self.writable_name(&path)?;
		self.fs.rmdir(name)?;
		self.note(FileEventKind::Removed, &path, 0);
		Ok(())
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

	// One file's facts. Read-only like `open` and `list`, so it takes the reading path rather than
	// `writable_name` - a client that may read a volume may ask how big a file on it is.
	fn stat(&mut self, path: String) -> Result<FileInfo, Error> {
		let target: VolumePath = VolumePath::parse(path.as_bytes()).ok_or(Error::NotFound)?;
		if target.volume != self.name() {
			return Err(Error::NotFound);
		}
		self.fs.stat_entry(target.path.as_bytes())
	}

	// BOTH PATHS ARE CHECKED, and both against the same volume. A rename whose destination was
	// checked less carefully than its source is a way out of a granted directory, which is what
	// `writable_name` exists to prevent - so it is asked twice rather than once.
	fn rename(&mut self, from: String, to: String) -> Result<(), Error> {
		let source: Vec<u8> = self.writable_name(&from)?.to_vec();
		let destination: &[u8] = self.writable_name(&to)?;
		let size: u64 = self.fs.stat_entry(&source).map(|entry| entry.size).unwrap_or(0);
		self.fs.rename(&source, destination)?;
		// TWO events, because a rename is two changes to two paths and a watcher holds one of them.
		// Reporting it once, on either path, is the shape that tells half the watchers nothing.
		self.note(FileEventKind::Removed, &from, 0);
		self.note(FileEventKind::Created, &to, size);
		Ok(())
	}

	fn truncate(&mut self, path: String, length: u64) -> Result<(), Error> {
		let name: &[u8] = self.writable_name(&path)?;
		self.fs.truncate(name, length)?;
		self.note(FileEventKind::Modified, &path, length);
		Ok(())
	}

	fn touch(&mut self, path: String, create: bool, at: u64) -> Result<(), Error> {
		let name: &[u8] = self.writable_name(&path)?;
		// The caller's time, when it gave one. The serve loop already stamped the service's own
		// clock before dispatching, so zero simply leaves that in place - there is no second
		// meaning to give it and no clock to fall back to.
		if at != 0 {
			self.fs.set_clock(at);
		}
		let existed: bool = self.exists(name);
		self.fs.touch(name, create)?;
		let size: u64 = self.fs.stat_entry(name).map(|entry| entry.size).unwrap_or(0);
		self.note(if existed { FileEventKind::Modified } else { FileEventKind::Created }, &path, size);
		Ok(())
	}

	// A WINDOW of a file, as a shared buffer of exactly the bytes delivered.
	//
	// The window is clamped to what one reply may carry rather than refused, so a client that
	// streams a file by repeating this call cannot pick a chunk size that stops it working. A
	// window past the end is not an error: it delivers nothing and says so by its length, which is
	// how a sequential reader learns it has reached the end without asking a second question.
	fn read(&mut self, path: String, offset: u64, length: u32) -> Result<Buffer, Error> {
		let want: usize = core::cmp::min(length as usize, READ_WINDOW_MAX);
		let target: VolumePath = VolumePath::parse(path.as_bytes()).ok_or(Error::NotFound)?;
		if target.volume != self.name() {
			return Err(Error::NotFound);
		}
		let window: Vec<u8> = self.fs.read_window(target.path.as_bytes(), offset, want)?;
		let handle: u64 = unsafe { make_file_buffer(&window) }.ok_or(Error::Again)?;
		Ok(Buffer { handle, len: window.len() as u64 })
	}

	// UNREACHABLE, for the reason `write_stream` is: the serve loop intercepts `OP_WATCH` and
	// registers a watcher, because a watch outlives the call that asked for it and this trait
	// answers within one. The method exists because the generated trait requires it.
	fn watch(&mut self, _path: String) -> Result<Vec<FileEvent>, Error> {
		Err(Error::Invalid)
	}

	// UNREACHABLE for a third reason: opening a writer creates a CLIENT, and the client table
	// belongs to the serve loop. `VolumeCall` is what the loop dispatches through, and its
	// implementation of this op is the real one.
	fn open_writer(&mut self, _path: String, _mode: WriterMode) -> Result<u64, Error> {
		Err(Error::Invalid)
	}
	// This impl answers ops that need only the volume; minting a client needs the client table,
	// which is why `VolumeCall` exists. The serve loop dispatches through that one.
	fn connect(&mut self) -> Result<u64, Error> {
		Err(Error::Invalid)
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
fn file_info(name: &[u8], size: u64, is_dir: bool, mtime: u64, ctime: u64) -> Result<FileInfo, Error> {
	Ok(FileInfo { name: try_string(name)?, size, r#type: if is_dir { FileType::Dir } else { FileType::File }, mtime, ctime })
}

// A `String` from bytes, reporting rather than aborting when the machine will not give the room.
//
// `String::from_utf8_lossy(..).into_owned()` allocates infallibly, so a directory listing under
// memory pressure took the service down through the allocator - on the DISK backends only, because
// the memory one had already been taught to reserve fallibly and answer `Again`. The same operation
// therefore reported on one volume and aborted on another.
//
// Lossy for the same reason it always was: a name off a foreign medium need not be UTF-8, and a
// listing that omits such a file is worse than one that renders it approximately.
fn try_string(bytes: &[u8]) -> Result<String, Error> {
	// The clean case borrows and costs one reservation.
	if let Ok(text) = core::str::from_utf8(bytes) {
		let mut out = String::new();
		out.try_reserve_exact(text.len()).map_err(|_| Error::Again)?;
		out.push_str(text);
		return Ok(out);
	}
	// And the case this helper exists for does NOT go through `String::from_utf8_lossy`.
	//
	// That call borrows when the input is clean and builds an owned `String` of replacement
	// characters when it is not - through the infallible path. So the helper written to make disk
	// listings fallible was infallible on the one input that makes it necessary, and a name that is
	// not UTF-8 is precisely what a foreign FAT, ISO9660 or UDF medium produces.
	//
	// Reserved first, at the worst case: every byte of a malformed sequence becomes one U+FFFD,
	// which is three bytes, and each replacement consumes at least one input byte. Nothing below can
	// grow the string past that, so nothing below can allocate.
	let mut out = String::new();
	let worst = bytes.len().checked_mul(3).ok_or(Error::Again)?;
	out.try_reserve_exact(worst).map_err(|_| Error::Again)?;
	let mut rest = bytes;
	loop {
		match core::str::from_utf8(rest) {
			Ok(text) => {
				out.push_str(text);
				return Ok(out);
			}
			Err(e) => {
				let (good, bad) = rest.split_at(e.valid_up_to());
				// good is valid by construction - `valid_up_to` is where the error starts.
				out.push_str(core::str::from_utf8(good).unwrap_or(""));
				out.push('\u{FFFD}');
				// `error_len` is None when the input simply ends mid-sequence, and then the rest of
				// it is one bad tail rather than a byte to step over.
				match e.error_len() {
					Some(skip) => rest = &bad[skip..],
					None => return Ok(out),
				}
			}
		}
	}
}

// Collect `items` into a vector whose room was reserved fallibly.
fn try_collect(items: impl ExactSizeIterator<Item = Result<FileInfo, Error>>) -> Result<Vec<FileInfo>, Error> {
	let mut out: Vec<FileInfo> = Vec::new();
	out.try_reserve_exact(items.len()).map_err(|_| Error::Again)?;
	for item in items {
		out.push(item?);
	}
	Ok(out)
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
	//
	// The DESTINATION is checked, which it was not: this backend ignored the name it was given and
	// answered `Allowed` for anything, so a stream to a missing parent accepted up to the whole
	// accumulation ceiling before failing at commit. The comment promising validation before the
	// first byte held for the memory filesystem alone.
	fn write_plan(&mut self, name: &[u8]) -> WritePlan {
		if self.fs.is_read_only() {
			return WritePlan::Refused(Error::Denied);
		}
		// A path that names an existing DIRECTORY cannot be written as a file.
		//
		// Only a successful read decides that. Treating any failure as "not a directory" let an
		// I/O error read as permission to proceed - the preflight would accept a write to a
		// filesystem it had just failed to inspect.
		match self.fs.read_dir(name) {
			Ok(_) => return WritePlan::Refused(Error::Invalid),
			Err(FsError::NotFound) | Err(FsError::NotDir) => {}
			Err(e) => return WritePlan::Refused(map_fs_err(e)),
		}
		// The parent has to exist. An empty parent is the volume root, which always does.
		//
		// The concrete reason is kept. Asking only whether the call failed merged a missing parent
		// with a parent that is a FILE, with an I/O error and with a corrupt filesystem, and
		// answered `NotFound` for all four - so `file/child` blamed the wrong thing and an
		// unreadable medium looked like an absent directory.
		if let Some(cut) = name.iter().rposition(|&b| b == b'/') {
			let parent = &name[..cut];
			if !parent.is_empty()
				&& let Err(e) = self.fs.read_dir(parent)
			{
				return WritePlan::Refused(map_fs_err(e));
			}
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
		try_collect(entries.into_iter().map(|(name, size, is_dir, mtime, ctime)| file_info(&name, size, is_dir, mtime, ctime)))
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
	// THE WHOLE TAXONOMY, not two fields of it.
	//
	// `LiberFs::FsckReport` has carried `structural_failures`, `stream_failures`, `io_failures` and
	// `faults` for several rounds, and this adapter forwarded `checksum_failures` and `damaged`
	// alone, because the wire record had nowhere to put the rest. So the entire distinction between
	// a failing disk, wrong metadata and an undecodable stream existed for unit tests and for a
	// direct Rust caller of the crate - not through `volume.fsck()`, which is the only way the
	// system exposes it, and the only way an operator ever sees any of this.
	fn fsck(&mut self) -> Result<FsckReport, Error> {
		let report = self.fs.fsck().map_err(map_fs_err)?;
		Ok(FsckReport { checksum_failures: report.checksum_failures, damaged: report.damaged.iter().map(|p| String::from_utf8_lossy(p).into_owned()).collect(), structural_failures: report.structural_failures, stream_failures: report.stream_failures, io_failures: report.io_failures, faults: report.faults.iter().map(|p| String::from_utf8_lossy(p).into_owned()).collect() })
	}
	// STRAIGHT OFF THE MEDIUM. The default reads the whole file to hand back a window of it, which
	// on the disk means reading a gigabyte to answer a question about sixty-four kilobytes of it -
	// so the one backend that carries files that size seeks instead.
	fn read_window(&mut self, name: &[u8], offset: u64, len: usize) -> Result<Vec<u8>, Error> {
		self.fs.read_at(name, offset, len).map_err(map_fs_err)
	}

	// THE NATIVE FILESYSTEM ANSWERS ALL FOUR, because LiberFS already implements them: `stat`
	// reads the inode, `rename` moves the directory entry without touching the contents, and
	// `truncate` drops or zero-extends. They were unreachable from a client only because the
	// contract did not name them.
	fn stat_entry(&mut self, name: &[u8]) -> Result<FileInfo, Error> {
		let stat = self.fs.stat(name).map_err(map_fs_err)?;
		let (_, base) = split_parent(name);
		Ok(FileInfo { name: String::from_utf8_lossy(base).into_owned(), size: stat.size, r#type: if stat.is_dir { FileType::Dir } else { FileType::File }, mtime: stat.mtime, ctime: stat.ctime })
	}

	fn rename(&mut self, from: &[u8], to: &[u8]) -> Result<(), Error> {
		self.fs.rename(from, to).map_err(map_fs_err)
	}

	fn truncate(&mut self, name: &[u8], length: u64) -> Result<(), Error> {
		self.fs.truncate(name, length).map_err(map_fs_err)
	}

	// `touch` is the one of the four LiberFS does not have as a verb, and it is two it does: a file
	// that exists is truncated to its own length, which is the mutation that stamps its mtime
	// without changing a byte; a missing one is created empty when the caller asked for it.
	fn touch(&mut self, name: &[u8], create: bool) -> Result<(), Error> {
		match self.fs.stat(name) {
			Ok(stat) => self.fs.truncate(name, stat.size).map_err(map_fs_err),
			Err(_) if create => self.fs.write_file(name, &[]).map_err(map_fs_err),
			Err(error) => Err(map_fs_err(error)),
		}
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
	// The destination is checked for the same reason as on the disk backend - a stream to a path
	// that cannot exist should cost a lookup, not a transfer.
	fn write_plan(&mut self, name: &[u8]) -> WritePlan {
		// Mount if this is the volume's first request. Reading `self.fs` directly refused every
		// write that arrived before some other operation had mounted the medium.
		let fs: &mut FatFs<FatBlockDevice> = match self.ensure_mounted() {
			Ok(fs) => fs,
			Err(e) => return WritePlan::Refused(e),
		};
		// As on LiberFS: only a successful read says "this is a directory"; a failure to look is
		// not permission to write.
		match fs.list_dir(name) {
			Ok(_) => return WritePlan::Refused(Error::Invalid),
			Err(FsError::NotFound) | Err(FsError::NotDir) => {}
			Err(e) => return WritePlan::Refused(map_fs_err(e)),
		}
		if let Some(cut) = name.iter().rposition(|&b| b == b'/') {
			let parent = &name[..cut];
			// The concrete reason, for the same reason as on LiberFS above.
			if !parent.is_empty()
				&& let Err(e) = fs.list_dir(parent)
			{
				return WritePlan::Refused(map_fs_err(e));
			}
		}
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
		try_collect(entries.into_iter().map(|e| file_info(e.name.as_bytes(), e.size, e.is_dir, 0, 0)))
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
	// A streamed rewrite keeps the old file until the commit, so it cannot spend what that file
	// holds. Answering with the ordinary figure admitted streams the first chunk then refused.
	fn stream_plan(&mut self, name: &[u8]) -> WritePlan {
		match self.fs.stream_len(name) {
			Ok(max) => WritePlan::Allowed { max_len: Some(max) },
			Err(e) => WritePlan::Refused(map_fs_err(e)),
		}
	}
	fn write_file_owned(&mut self, name: &[u8], data: Vec<u8>) -> Result<(), Error> {
		// Adopted, not copied: the streamed buffer becomes the file's own storage, so a reserved
		// volume needs the memory once instead of twice.
		self.fs.write_file_owned(name, data).map_err(map_fs_err)
	}
	fn stream_begin(&mut self, name: &[u8]) -> Option<Result<(), Error>> {
		Some(self.fs.stream_begin(name).map_err(map_fs_err))
	}
	fn stream_push(&mut self, chunk: &[u8]) -> Result<(), Error> {
		self.fs.stream_push(chunk).map_err(map_fs_err)
	}
	fn stream_spare(&mut self, want: usize) -> Option<Result<&mut [u8], Error>> {
		Some(self.fs.stream_spare(want).map_err(map_fs_err))
	}
	fn stream_advance(&mut self, offered: usize, written: usize) -> Result<(), Error> {
		self.fs.stream_advance(offered, written).map_err(map_fs_err)
	}
	fn stream_commit(&mut self) -> Result<(), Error> {
		self.fs.stream_commit().map_err(map_fs_err)
	}
	fn stream_abort(&mut self) {
		self.fs.stream_abort();
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
	// The memory volumes answer three of the four. `stat_entry` stays the default - a listing of
	// an in-memory directory is a walk of a sorted vector, so looking one name up directly would
	// save nothing worth a second implementation of the lookup.
	fn rename(&mut self, from: &[u8], to: &[u8]) -> Result<(), Error> {
		self.fs.rename(from, to).map_err(map_fs_err)
	}
	fn truncate(&mut self, name: &[u8], length: u64) -> Result<(), Error> {
		self.fs.truncate(name, length).map_err(map_fs_err)
	}
	fn touch(&mut self, name: &[u8], create: bool) -> Result<(), Error> {
		self.fs.touch(name, create).map_err(map_fs_err)
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
	// A truncated or foreign image is simply not a live volume here - this path builds one from a
	// staged image and has nothing to format, so the reason is not actionable.
	let mut source = LiberFs::mount(ImageDevice { bytes: image }).ok()?;
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
	// THE RANGED READ, CONNECTED. `Storage.Volume.read(path, offset, length)` is in the interface,
	// `read_window` is the backend hook and `DiskFs` overrides it; this took the default, which
	// reads the WHOLE file and slices it - so the 64 MiB ceiling still applied to every window read
	// from `vol://iso`, and `read_file_into`, added to this backend precisely to avoid that, was
	// unreachable from the service. The milestone recorded it as not done because whole-file staging
	// would have to be reworked first; `volume.read` and `read_window` are that rework.
	fn read_window(&mut self, name: &[u8], offset: u64, len: usize) -> Result<Vec<u8>, Error> {
		let mut window: Vec<u8> = Vec::new();
		window.try_reserve_exact(len).map_err(|_| Error::Again)?;
		window.resize(len, 0);
		let read = self.fs.read_file_into(name, offset, &mut window).map_err(map_fs_err)?;
		window.truncate(read);
		Ok(window)
	}
	fn list_entries(&mut self, dir: &[u8]) -> Result<Vec<FileInfo>, Error> {
		let entries = if dir.is_empty() { self.fs.list() } else { self.fs.list_dir(dir) }.map_err(map_fs_err)?;
		try_collect(entries.into_iter().map(|e| file_info(e.name.as_bytes(), e.size, e.is_dir, 0, 0)))
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
		try_collect(entries.into_iter().map(|e| file_info(e.name.as_bytes(), e.size, e.is_dir, 0, 0)))
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
	// ONE NAME, WITHOUT A LISTING - which is the only way to answer this on a flat archive.
	//
	// The default `stat_entry` reads the parent directory and searches it, and this backend has no
	// directories: a package key is a whole path, so `bin/cat.lsexe` is one entry and `bin` is not
	// an entry at all. `read_file` has always looked names up directly; before this, `stat` of a
	// staged artifact failed on the volume that holds every staged artifact, which is how `which`
	// found nothing on an archive-backed system volume.
	fn stat_entry(&mut self, name: &[u8]) -> Result<FileInfo, Error> {
		let package = Package::parse(self.archive()).ok_or(Error::NotFound)?;
		let size: u64 = package.lookup(name).ok_or(Error::NotFound)?.len() as u64;
		let (_, base) = split_parent(name);
		file_info(base, size, false, 0, 0)
	}
	fn list_entries(&mut self, dir: &[u8]) -> Result<Vec<FileInfo>, Error> {
		// the test archive is a flat package - it has no subdirectories.
		if !dir.is_empty() {
			return Err(Error::NotFound);
		}
		let package = Package::parse(self.archive()).ok_or(Error::NotFound)?;
		let mut files: Vec<FileInfo> = Vec::new();
		files.try_reserve_exact(package.len()).map_err(|_| Error::Again)?;
		for index in 0..package.len() {
			if let Some(name) = package.name(index) {
				let size: u64 = package.lookup(name).map(|b| b.len()).unwrap_or(0) as u64;
				// the archive format carries no timestamps.
				files.push(file_info(name, size, false, 0, 0)?);
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
		// The service is short of MEMORY, not the volume of space. Both are worth retrying and the
		// caller's next move differs: `NoSpace` says free something on the volume, `NoMemory` says
		// wait for this service. They arrive here as one `Again` because the Storage protocol has
		// no finer word yet - what matters is that the filesystems no longer conflate them, so the
		// distinction exists to be surfaced when the protocol grows one.
		FsError::NoMemory => Error::Again,
		// A file too large to return in one buffer, which is a ranged read away from working - not
		// a damaged volume, which is what it used to be reported as.
		FsError::TooLarge => Error::Invalid,
		// the malformed-request family: bad or overlong names, wrong kinds, a non-empty
		// directory, a duplicate snapshot name, an impossible operation.
		FsError::TooLong | FsError::BadName | FsError::IsDir | FsError::NotDir | FsError::NotEmpty | FsError::Exists | FsError::Invalid => Error::Invalid,
		// on-disk corruption caught by a block checksum: the data cannot be trusted.
		FsError::Corrupt => Error::Invalid,
		FsError::Io => Error::Again,
		// A commit that MAY have landed. NOT `Again`: retrying is exactly the wrong response - the
		// write the caller thinks it lost may already be on the medium, and the volume is read-only
		// from here on, so a retry can only fail or duplicate. `Denied` says "this mount will not
		// take another write", which is the true statement this protocol can make today.
		FsError::CommitUncertain => Error::Denied,
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

	fn read_blocks(&mut self, index: u64, count: u64, buf: &mut [u8]) -> bool {
		unsafe { read_blocks_chunked(self.chan, ISO_SECTORS, index, count, buf, ISO_SECTORS as usize * SECTOR_SIZE) }
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

	fn read_blocks(&mut self, index: u64, count: u64, buf: &mut [u8]) -> bool {
		unsafe { read_blocks_chunked(self.chan, UDF_SECTORS, index, count, buf, UDF_SECTORS as usize * SECTOR_SIZE) }
	}

	// HOW BIG THE DISC IS, which this did not implement and therefore answered `None`.
	//
	// `udf::mount` probes the Anchor Volume Descriptor Pointer at 256, at N-256 and at N when the
	// backing knows its size - the redundancy optical media carries precisely so a damaged anchor is
	// survivable. `MemDisc` in the UDF test suite implements this and the tests pass; this adapter
	// did not, so every real `vol://udf` mount took the default `None` and probed anchor 256 alone.
	// A feature that worked in the fixture and nowhere else.
	//
	// The capacity was already available: `block_capacity` asks the driver, in this file, and is
	// what the LiberFS and FAT paths use to size their volumes.
	fn block_count(&mut self) -> Option<u64> {
		let bytes = unsafe { block_capacity(self.chan) }.ok()?;
		// In UDF blocks, not disk sectors. A capacity that is not a whole number of 2 KiB blocks is
		// truncated rather than rounded up: the last partial block is not addressable as a UDF
		// block, and answering N+1 would send the anchor probe off the end of the medium.
		Some(bytes / (UDF_SECTORS * SECTOR_SIZE as u64))
	}
}
// How many 2048-byte logical blocks one request may carry.
//
// The reader asks for a whole contiguous extent and the DEFAULT `read_blocks` in `fs-core` is a
// loop over `read_block`, so an extent of a thousand blocks was a thousand messages to the driver -
// the batching the readers already ask for existed nowhere. Overriding it here is what makes the
// call mean something.
//
// Chunked rather than passed straight through: the driver's `Span::fit` GROWS its DMA buffer to
// whatever a request needs, so handing it a hundred-megabyte extent asks it to allocate one. 64
// blocks is 128 kB per request - sixty-four times fewer round trips than a block at a time, and a
// buffer size the driver was already sized for.
const READ_BLOCKS_PER_REQUEST: u64 = 64;

// One `read_blocks` for both optical backends: same shape, different sectors-per-block.
unsafe fn read_blocks_chunked(chan: u64, sectors_per_block: u64, index: u64, count: u64, buf: &mut [u8], block_bytes: usize) -> bool {
	if count == 0 {
		return true;
	}
	if buf.len() < count as usize * block_bytes {
		return false;
	}
	let mut done: u64 = 0;
	while done < count {
		let run: u64 = core::cmp::min(READ_BLOCKS_PER_REQUEST, count - done);
		let at: usize = done as usize * block_bytes;
		let bytes: usize = run as usize * block_bytes;
		let Ok(sectors) = u32::try_from(run * sectors_per_block) else { return false };
		// SAFETY: `buf` was checked above to hold `count` blocks, and this window is inside it.
		if !unsafe { block_read(chan, (index + done) * sectors_per_block, sectors, buf[at..at + bytes].as_mut_ptr()) } {
			return false;
		}
		done += run;
	}
	true
}

// Mount the LiberFS on the virtio-blk disk. The volume's container is a GPT partition carrying the
// LiberFS type GUID when the disk has one (so a disk partitioned by another system mounts the same
// volume), else the whole device. The block channel stays open for the serve loop.
//
// It does not format, and the name used to say `mount_or_format` because it did. Laying a
// filesystem over a disk is a decision a person makes with the disk in front of them; a service
// deciding it at boot, from a sample of sectors, is how somebody else's data gets destroyed by a
// machine that was only supposed to start up.
unsafe fn mount_system_volume(block_client: u64) -> Option<LiberFs<ChannelBlockDevice>> {
	// What the disk IS, before deciding what may be written to it. Exactly two answers lead
	// anywhere near a format, and the difference between them and the rest is the difference
	// between a blank disk and somebody else's.
	//
	// The probe used to answer that question with one bit - "LBA 1 begins with EFI PART, or
	// it does not" - and the negative half was read as licence to format the whole device
	// from sector ZERO. That put an ordinary MBR-partitioned disk, a hybrid MBR, a GPT with
	// a damaged signature and an intact backup, and a USB stick carrying FAT straight at
	// LBA 0 all in the same bucket as a disk fresh out of its bag.
	let mut sectors = DiskSectors { chan: block_client };
	let (base, pool): (u64, u64) = match partition::probe(&mut sectors) {
		partition::Disk::LiberFs { first, last } => (first, (last - first + 1) / SECTORS_PER_BLOCK),
		// the fixed whole-device layout: a disk with nothing on it, or one already carrying
		// this system's own volume at LBA 0 (which is what every system disk looks like on
		// its second boot). The MOUNT is what keeps the second one safe - it formats only
		// when the volume says `Unformatted`.
		// A disk already carrying this system's own volume at LBA 0 - which is what every system
		// disk looks like, because the volume is BUILT as a filesystem by `mkpackages` and written
		// to the medium. Mounting it is not formatting it.
		partition::Disk::LiberFsWholeDevice => (FS_START_SECTOR, unsafe { disk_pool_blocks(block_client) }),
		// A disk that looks empty. This used to be the one answer that licensed laying a filesystem
		// over the whole device, and it should never have been: formatting a disk is a decision
		// somebody makes, not one a service infers at boot from the bytes in front of it.
		//
		// It was not even a feature by the end. The format existed to SEED - there was a factory
		// archive at LBA 0 and a fresh disk was formatted and populated from it - and P02M0108 retired
		// the seeding. What was left formatted an EMPTY volume and printed that it had done so,
		// which is of no use to anybody: a machine whose system volume never arrived is not helped
		// by being given a blank one, and a disk of somebody else's data is actively harmed.
		//
		// Provisioning a disk is `mkpackages` writing a verified volume image to it, on a machine
		// where a person is standing. That is the manual step, it already exists, and it is the
		// only one.
		partition::Disk::Blank => {
			unsafe {
				print(b"storage: vol://system NOT mounted: the disk carries no filesystem. Nothing was changed - write a system volume image to it deliberately; this system does not format disks by itself\n");
			}
			return None;
		}
		// Everything below means the whole-device fallback is not available, because the
		// fallback starts at sector ZERO - it would lay a filesystem over the protective
		// MBR, the GPT header, the entry array and every partition the disk carries.
		// Refusing costs a boot; the alternative cost the disk.
		partition::Disk::GptWithoutLiberFs => {
			unsafe {
				print(b"storage: vol://system NOT mounted: the disk has a GPT with no LiberFS partition. Nothing was changed - create one, or attach the right disk\n");
			}
			return None;
		}
		// TWO CANDIDATES AND NOTHING TO CHOOSE BETWEEN THEM. Mounting either would be mounting
		// whichever the entry order happens to name first, which a partitioning tool or a clone can
		// change without touching either filesystem - and this mount is writable.
		partition::Disk::AmbiguousLiberFs => {
			unsafe {
				print(b"storage: vol://system NOT mounted: the disk names MORE THAN ONE LiberFS partition and nothing says which is the system volume. Nothing was changed - remove or retype the one that is not\n");
			}
			return None;
		}
		partition::Disk::MbrWithoutLiberFs => {
			unsafe {
				print(b"storage: vol://system NOT mounted: the disk carries an MBR partition table. Nothing was changed - its partitions are still there; repartition it deliberately if that is what you want\n");
			}
			return None;
		}
		partition::Disk::ForeignFilesystem { name } => {
			unsafe {
				print(b"storage: vol://system NOT mounted: the disk carries a filesystem written straight onto the medium (");
				print(name.as_bytes());
				print(b"), with no partition table. Nothing was changed - copy the data off before reusing this disk\n");
			}
			return None;
		}
		// The one that closes the hole this crate was written for and then left open by a
		// hair: no table, nothing recognised, and bytes that are not zero. There is no
		// complete list of filesystem signatures, so "I did not recognise it" was never
		// evidence of an empty disk - and a raw ext4 begins one sector past where the probe
		// used to stop looking.
		partition::Disk::UnknownData => {
			unsafe {
				print(b"storage: vol://system NOT mounted: the disk carries data this build does not recognise, and no partition table. Nothing was changed - erase it deliberately if it really is scrap\n");
			}
			return None;
		}
		partition::Disk::HybridMbrAndGpt => {
			unsafe {
				print(b"storage: vol://system NOT mounted: the disk carries BOTH an MBR partition table and a GPT. Nothing was changed - two tables describing one disk disagree by construction, and nothing here can say which you meant\n");
			}
			return None;
		}
		partition::Disk::NoMemory => {
			unsafe {
				print(b"storage: vol://system NOT mounted: this machine could not hold the disk's partition table while checking it. Nothing was changed - the disk may be perfectly fine\n");
			}
			return None;
		}
		partition::Disk::CorruptGpt => {
			unsafe {
				print(b"storage: vol://system NOT mounted: the disk's GPT does not verify, in neither the primary nor the backup copy. Nothing was changed - this is a damaged partition table, not a blank disk\n");
			}
			return None;
		}
		partition::Disk::Io => {
			unsafe {
				print(b"storage: vol://system NOT mounted: the disk did not answer while its partition table was read. Nothing was changed - check the device and reboot\n");
			}
			return None;
		}
	};
	let max_sectors: u32 = unsafe { block_request_sectors(block_client) };
	// an existing filesystem (files persisted from a previous boot) mounts as-is, at
	// the size recorded in its superblock - never silently grown, the free map would
	// not match. A volume smaller than its container allows is reported.
	// Format ONLY when the disk says there is nothing here.
	//
	// Every other answer leaves the volume untouched and says which it was. This read every failure
	// as a blank disk, so a device that hiccupped during boot, or a volume written by a build with
	// a different layout, was answered by laying a fresh filesystem over it - a transient fault
	// turned into permanent loss. The service already refused to do that for two corruption cases;
	// this is the same rule for the rest.
	let mounted = LiberFs::mount(ChannelBlockDevice { chan: block_client, base, limit: pool, max_sectors });
	match mounted {
		// The probe said this container holds a LiberFS and the mount disagrees. That is a
		// contradiction about a real volume, not an empty disk: something between the superblock
		// and the probe is damaged. It used to fall through to a format, which destroyed exactly
		// the volume the contradiction was about.
		Err(MountError::Unformatted) => {
			unsafe {
				print(b"storage: vol://system NOT mounted: the container looks like a LiberFS volume and does not mount as one. Nothing was changed - this is damage, not an empty disk\n");
			}
			return None;
		}
		Err(reason) => {
			unsafe {
				print(match reason {
					MountError::Io => b"storage: vol://system NOT mounted: the disk did not answer. Nothing was changed - check the device and reboot; this is not a blank disk
"
					.as_slice(),
					MountError::Unsupported => b"storage: vol://system NOT mounted: written by a newer or different LiberFS build. Nothing was changed - boot a build that reads it
"
					.as_slice(),
					MountError::DeviceTooSmall => b"storage: vol://system NOT mounted: the medium is smaller than the volume it claims. Nothing was changed
"
					.as_slice(),
					// the MACHINE, not the medium. This used to fall through to "its
					// superblocks are damaged", which sends an operator to look at a disk
					// that is perfectly fine.
					MountError::NoMemory => b"storage: vol://system NOT mounted: this machine could not hold the volume's free maps. Nothing was changed - the disk is fine; the memory was not there
"
					.as_slice(),
					_ => b"storage: vol://system NOT mounted: its superblocks are damaged. Nothing was changed - copy data off or restore before reformatting
"
					.as_slice(),
				});
			}
			return None;
		}
		Ok(_) => {}
	}
	if let Ok(fs) = mounted {
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
	// Every path above either mounted or returned. There is no fallback, and that is the point:
	// this function reads a disk and mounts what is on it. It does not create.
	None
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

// The disk as raw sectors, for the partition probe. Reads go through the driver's block
// service; the capacity is the driver's answer in sectors, and a driver that cannot answer
// leaves it None - which the probe treats as "nothing bounds this table", not as zero.
struct DiskSectors {
	chan: u64,
}

impl partition::Sectors for DiskSectors {
	fn read(&mut self, lba: u64, buf: &mut [u8]) -> bool {
		unsafe { block_read(self.chan, lba, 1, buf.as_mut_ptr()) }
	}

	fn capacity(&mut self) -> Option<u64> {
		unsafe { block_capacity(self.chan) }.ok().map(|bytes| bytes / SECTOR_SIZE as u64)
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
