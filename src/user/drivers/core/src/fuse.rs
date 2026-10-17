// THE FUSE WIRE AS VIRTIO-FS CARRIES IT (protocol 7.31): the requests a guest sends a host's file server, the
// answers it reads back, and the checks that stand between those answers and anything inside this system - the parts
// of the virtio-fs driver a host can test.
//
// THE HOST IS NOT TRUSTED TO BE TIDY. Every answer is decoded against its own header and its declared lengths, a
// directory listing's records against the buffer they arrived in, and a name against the rules a name in a volume
// keeps: no separator, no NUL, not `.` or `..`, at most 255 bytes. A name that breaks one is never handed on - it is
// how a share would reach outside itself.
//
// AND THE HOST'S IDENTITIES STAY THE HOST'S. The node ids the server answers with, its inode and device numbers, its
// owners and groups, are none of a client's business: a node is known inside this system by a handle `Nodes` hands out,
// and an attribute crosses as its kind, its size, its permission bits and its modification time.

// The protocol version this driver speaks.
pub const MAJOR: u32 = 7;
pub const MINOR: u32 = 31;

// The operations it sends.
pub const LOOKUP: u32 = 1;
pub const FORGET: u32 = 2;
pub const GETATTR: u32 = 3;
pub const OPEN: u32 = 14;
pub const READ: u32 = 15;
pub const STATFS: u32 = 17;
pub const RELEASE: u32 = 18;
pub const INIT: u32 = 26;
pub const OPENDIR: u32 = 27;
pub const READDIR: u32 = 28;
pub const RELEASEDIR: u32 = 29;

// The root's node id, which the protocol fixes.
pub const ROOT: u64 = 1;

// The fixed parts' lengths.
pub const IN_HEADER: usize = 40;
pub const OUT_HEADER: usize = 16;
pub const INIT_IN: usize = 16;
pub const INIT_OUT_MIN: usize = 24;
pub const ATTR: usize = 88;
pub const ENTRY_OUT: usize = 40 + ATTR;
pub const ATTR_OUT: usize = 16 + ATTR;
pub const GETATTR_IN: usize = 16;
pub const OPEN_IN: usize = 8;
pub const OPEN_OUT: usize = 16;
pub const READ_IN: usize = 40;
pub const RELEASE_IN: usize = 24;
pub const FORGET_IN: usize = 8;
pub const STATFS_OUT: usize = 80;
pub const DIRENT: usize = 24;

// THE BOUNDS: a name, one read or one listing, and how many nodes a share may have looked up at once.
pub const NAME_MAX: usize = 255;
pub const MAX_IO: usize = 64 * 1024;
pub const MAX_NODES: usize = 256;

// The open flags this driver asks for: read-only.
pub const O_RDONLY: u32 = 0;
// A directory opened for listing.
pub const O_DIRECTORY: u32 = 0o200000;

fn put32(out: &mut [u8], at: usize, value: u32) {
	out[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn put64(out: &mut [u8], at: usize, value: u64) {
	out[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
	u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
	let mut word = [0u8; 8];
	word.copy_from_slice(&bytes[at..at + 8]);
	u64::from_le_bytes(word)
}

// A request's header: its whole length, the operation, the request's number and the node it is about. The caller
// is the guest's root, uid and gid zero, which the host maps however it was told to.
pub fn in_header(len: u32, opcode: u32, unique: u64, nodeid: u64) -> [u8; IN_HEADER] {
	let mut out = [0u8; IN_HEADER];
	put32(&mut out, 0, len);
	put32(&mut out, 4, opcode);
	put64(&mut out, 8, unique);
	put64(&mut out, 16, nodeid);
	out
}

// The INIT request's body: the version, no read-ahead and no flags - nothing this driver does not use.
pub fn init_in() -> [u8; INIT_IN] {
	let mut out = [0u8; INIT_IN];
	put32(&mut out, 0, MAJOR);
	put32(&mut out, 4, MINOR);
	out
}

// A GETATTR body: no file handle.
pub fn getattr_in() -> [u8; GETATTR_IN] {
	[0u8; GETATTR_IN]
}

// An OPEN or OPENDIR body.
pub fn open_in(flags: u32) -> [u8; OPEN_IN] {
	let mut out = [0u8; OPEN_IN];
	put32(&mut out, 0, flags);
	out
}

// A READ or READDIR body: the handle, the offset and the most bytes wanted.
pub fn read_in(fh: u64, offset: u64, size: u32, flags: u32) -> [u8; READ_IN] {
	let mut out = [0u8; READ_IN];
	put64(&mut out, 0, fh);
	put64(&mut out, 8, offset);
	put32(&mut out, 16, size);
	put32(&mut out, 32, flags);
	out
}

// A RELEASE or RELEASEDIR body.
pub fn release_in(fh: u64, flags: u32) -> [u8; RELEASE_IN] {
	let mut out = [0u8; RELEASE_IN];
	put64(&mut out, 0, fh);
	put32(&mut out, 8, flags);
	out
}

// A FORGET body: how many lookups of the node are given back.
pub fn forget_in(count: u64) -> [u8; FORGET_IN] {
	count.to_le_bytes()
}

// WHY AN ANSWER IS NOT BELIEVED.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fault {
	// Shorter than its header, or than the header says.
	Short,
	// Answering another request than the one asked.
	Unique,
	// A body shorter than the operation's fixed answer.
	Body,
	// A version this driver does not speak.
	Version,
	// A name that breaks a rule a volume's name keeps.
	Name,
	// A record of a listing that runs past what arrived.
	Record,
}

// A decoded answer's header: the errno the host answered (zero for success, a positive number for an error) and the
// body's bytes.
pub fn out_header(bytes: &[u8], unique: u64) -> Result<(u32, &[u8]), Fault> {
	if bytes.len() < OUT_HEADER {
		return Err(Fault::Short);
	}
	let len = u32_at(bytes, 0) as usize;
	if len < OUT_HEADER || len > bytes.len() {
		return Err(Fault::Short);
	}
	if u64_at(bytes, 8) != unique {
		return Err(Fault::Unique);
	}
	let error = i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
	// AN ERROR IS NEGATIVE ON THE WIRE, and nothing else is: a positive one is a server that is not speaking FUSE.
	if error > 0 || error == i32::MIN {
		return Err(Fault::Body);
	}
	Ok((error.unsigned_abs(), &bytes[OUT_HEADER..len]))
}

// What INIT answered: the version, and the largest write the server takes.
pub fn init_out(body: &[u8]) -> Result<(u32, u32), Fault> {
	if body.len() < INIT_OUT_MIN {
		return Err(Fault::Body);
	}
	let (major, minor) = (u32_at(body, 0), u32_at(body, 4));
	if major != MAJOR || minor < 27 {
		return Err(Fault::Version);
	}
	Ok((minor, u32_at(body, 20)))
}

// What kind of node.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
	File,
	Directory,
	// A symbolic link, a device, a socket or a pipe: never followed, never opened.
	Other,
}

// AN ATTRIBUTE AS IT CROSSES INTO THIS SYSTEM: what the node is, how long, its permission bits and when it was last
// written. The host's inode number, device, link count, owner and group stay behind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Attr {
	pub kind: Kind,
	pub size: u64,
	pub permissions: u16,
	pub mtime: u64,
}

const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;

pub fn kind_of(mode: u32) -> Kind {
	match mode & S_IFMT {
		S_IFDIR => Kind::Directory,
		S_IFREG => Kind::File,
		_ => Kind::Other,
	}
}

// One `fuse_attr`, at the start of `bytes`.
pub fn attr(bytes: &[u8]) -> Result<Attr, Fault> {
	if bytes.len() < ATTR {
		return Err(Fault::Body);
	}
	let mode = u32_at(bytes, 60);
	Ok(Attr { kind: kind_of(mode), size: u64_at(bytes, 8), permissions: (mode & 0o7777) as u16, mtime: u64_at(bytes, 32) })
}

// What LOOKUP answered: the server's node id - which stays inside the driver - and the node's attribute.
pub fn entry_out(body: &[u8]) -> Result<(u64, Attr), Fault> {
	if body.len() < ENTRY_OUT {
		return Err(Fault::Body);
	}
	let nodeid = u64_at(body, 0);
	if nodeid == 0 {
		return Err(Fault::Body);
	}
	Ok((nodeid, attr(&body[40..])?))
}

// What GETATTR answered.
pub fn attr_out(body: &[u8]) -> Result<Attr, Fault> {
	if body.len() < ATTR_OUT {
		return Err(Fault::Body);
	}
	attr(&body[16..])
}

// What OPEN or OPENDIR answered: the server's handle.
pub fn open_out(body: &[u8]) -> Result<u64, Fault> {
	if body.len() < OPEN_OUT {
		return Err(Fault::Body);
	}
	Ok(u64_at(body, 0))
}

// What STATFS answered: the blocks, the free ones and the block size.
pub fn statfs_out(body: &[u8]) -> Result<(u64, u64, u32), Fault> {
	if body.len() < STATFS_OUT {
		return Err(Fault::Body);
	}
	Ok((u64_at(body, 0), u64_at(body, 16), u32_at(body, 40)))
}

// WHAT AN ERRNO THE HOST ANSWERED BECOMES INSIDE THIS SYSTEM: a handful of meanings a client can act on, never the
// host's number itself - which would say what kind of system the share is on.
pub fn errno_error(errno: u32) -> proto::system::Error {
	use proto::system::Error;
	match errno {
		// ENOENT
		2 => Error::NotFound,
		// EPERM, EACCES, EROFS
		1 | 13 | 30 => Error::Denied,
		// ENOTDIR, EISDIR, EINVAL, ENAMETOOLONG, ELOOP
		20 | 21 | 22 | 36 | 40 => Error::Invalid,
		// EAGAIN, ENOMEM, EINTR
		11 | 12 | 4 => Error::Again,
		// ENOSYS, EOPNOTSUPP
		38 | 95 => Error::Unsupported,
		// ENFILE, EMFILE
		23 | 24 => Error::Exhausted,
		_ => Error::Io,
	}
}

// WHY A NAME IS NOT ONE A VOLUME HOLDS.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NameRefusal {
	Empty,
	TooLong,
	// `.` or `..`: the node itself or the one above it, which is how a path walks out of a share.
	Dots,
	// A separator or a NUL inside it.
	Separator,
	// Bytes that are not UTF-8.
	Encoding,
}

pub fn check_name(name: &[u8]) -> Result<(), NameRefusal> {
	if name.is_empty() {
		return Err(NameRefusal::Empty);
	}
	if name.len() > NAME_MAX {
		return Err(NameRefusal::TooLong);
	}
	if name == b"." || name == b".." {
		return Err(NameRefusal::Dots);
	}
	if name.iter().any(|byte| *byte == b'/' || *byte == 0) {
		return Err(NameRefusal::Separator);
	}
	if core::str::from_utf8(name).is_err() {
		return Err(NameRefusal::Encoding);
	}
	Ok(())
}

// A LOOKUP body: the name and its terminating NUL, into `out`; the length written, or the refusal.
pub fn lookup_in(name: &[u8], out: &mut [u8]) -> Result<usize, NameRefusal> {
	check_name(name)?;
	if out.len() < name.len() + 1 {
		return Err(NameRefusal::TooLong);
	}
	out[..name.len()].copy_from_slice(name);
	out[name.len()] = 0;
	Ok(name.len() + 1)
}

// ONE RECORD OF A LISTING: the name, its kind as the listing says it, and the offset to continue from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Dirent<'a> {
	pub name: &'a [u8],
	pub kind: Kind,
	pub next: u64,
}

// The kinds a listing's `type` field names: the `DT_*` values.
fn dirent_kind(value: u32) -> Kind {
	match value {
		4 => Kind::Directory,
		8 => Kind::File,
		_ => Kind::Other,
	}
}

// THE RECORDS OF ONE READDIR ANSWER, each held to the buffer: a record whose name runs past what arrived ends the walk
// with `Fault::Record`. `.` and `..` are dropped - they are the listing's, not the directory's - and so is a name that
// breaks a rule, which is never a record: one host file with a name no volume can hold does not make its directory
// unlistable. The visitor sees every record kept and the offset after every record dropped, so a listing continued
// from the last offset neither repeats nor skips. Answers how many names were dropped for breaking a rule.
pub fn dirents<'a>(body: &'a [u8], mut visit: impl FnMut(Option<Dirent<'a>>, u64)) -> Result<usize, Fault> {
	let mut at = 0usize;
	let mut dropped = 0usize;
	while at < body.len() {
		if body.len() - at < DIRENT {
			return Err(Fault::Record);
		}
		let next = u64_at(body, at + 8);
		let namelen = u32_at(body, at + 16) as usize;
		let kind = dirent_kind(u32_at(body, at + 20));
		let end = at + DIRENT + namelen;
		if namelen == 0 || namelen > NAME_MAX || end > body.len() {
			return Err(Fault::Record);
		}
		let name = &body[at + DIRENT..end];
		if name == b"." || name == b".." {
			visit(None, next);
		} else if check_name(name).is_err() {
			dropped += 1;
			visit(None, next);
		} else {
			visit(Some(Dirent { name, kind, next }), next);
		}
		// RECORDS ARE PADDED TO EIGHT BYTES; the padding of the last may be cut off.
		at = (end + 7) & !7;
	}
	Ok(dropped)
}

// THE NODES A SHARE HAS LOOKED UP, by the handle this system knows each by: the server's node id, what the node is and
// how many lookups of it are outstanding - which is what FORGET gives back. Bounded; handle zero is never a node; the
// root is handle one and is never forgotten. A handle is not reused while the table lives, so a stale one reaches
// nothing.
#[derive(Clone, Copy)]
struct Held {
	handle: u32,
	nodeid: u64,
	kind: Kind,
	count: u64,
}

pub struct Nodes {
	slots: [Option<Held>; MAX_NODES],
	next: u32,
}

impl Default for Nodes {
	fn default() -> Self {
		Nodes::new()
	}
}

pub const ROOT_HANDLE: u32 = 1;

impl Nodes {
	pub const fn new() -> Nodes {
		let mut slots = [None; MAX_NODES];
		slots[0] = Some(Held { handle: ROOT_HANDLE, nodeid: ROOT, kind: Kind::Directory, count: 0 });
		Nodes { slots, next: ROOT_HANDLE + 1 }
	}

	// The server's node id behind a handle, and what the node is.
	pub fn get(&self, handle: u32) -> Option<(u64, Kind)> {
		self.slots.iter().flatten().find(|held| held.handle == handle).map(|held| (held.nodeid, held.kind))
	}

	// A lookup answered with `nodeid`: the handle it is known by - the one it already has, its count raised and its kind
	// as answered now, or a new one. None when the table is full or the handles are spent; the caller then gives the
	// lookup straight back.
	pub fn looked_up(&mut self, nodeid: u64, kind: Kind) -> Option<u32> {
		if let Some(held) = self.slots.iter_mut().flatten().find(|held| held.nodeid == nodeid) {
			held.count = held.count.saturating_add(1);
			held.kind = kind;
			return Some(held.handle);
		}
		let slot = self.slots.iter_mut().find(|slot| slot.is_none())?;
		let handle = self.next;
		self.next = self.next.checked_add(1)?;
		*slot = Some(Held { handle, nodeid, kind, count: 1 });
		Some(handle)
	}

	// The handle let go: the server's node id and the lookups to give back with FORGET, or None for the root or a
	// handle that names nothing.
	pub fn forget(&mut self, handle: u32) -> Option<(u64, u64)> {
		if handle == ROOT_HANDLE {
			return None;
		}
		let slot = self.slots.iter_mut().find(|slot| slot.is_some_and(|held| held.handle == handle))?;
		slot.take().map(|held| (held.nodeid, held.count))
	}

	// Every node but the root let go, for a connection that has gone: what FORGET gives back for each.
	pub fn drain(&mut self) -> impl Iterator<Item = (u64, u64)> + '_ {
		self.slots.iter_mut().filter(|slot| slot.is_some_and(|held| held.handle != ROOT_HANDLE)).filter_map(|slot| slot.take().map(|held| (held.nodeid, held.count)))
	}

	pub fn len(&self) -> usize {
		self.slots.iter().flatten().count()
	}

	pub fn is_empty(&self) -> bool {
		false
	}
}

#[cfg(test)]
mod tests;
