// driver.virtio-fs - a directory of the host (virtio device 26, FUSE over virtqueues), served as the `share-device`
// contract on a DEVELOPMENT machine.
//
// DEVELOPMENT ONLY, AND OPT-IN TWICE. This program is in no shipping image, and the device exists only on a machine
// started with a share - `VIRTIOFS_DIR` to the runner, which starts the host's `virtiofsd` over that directory, read-
// only. Its one consumer is StorageService, which serves the share as `vol://host` behind its own grants; nothing
// else may connect, and the system volume, packages and staging never come from here.
//
// READ-ONLY, BY NODE, AND IN THIS SYSTEM'S TERMS (see `drivers::fuse`): a node is known by a handle each connection's
// table hands out, never by the host's node id; an attribute crosses as kind, size, permission bits and modification
// time; a name is held to a volume's rules in both directions; a host errno becomes a meaning, not a number. Symbolic
// links, devices, sockets and pipes are listed as `other` and never opened, followed or walked through. No extended
// attribute is asked for, and the host's daemon is started without them.
//
// SYNCHRONOUS, ONE REQUEST AT A TIME: every FUSE request is submitted and waited for before the next consumer message
// is read, so nothing is in flight between two of them.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use drivers::common;
use drivers::fuse::{self, Attr, Kind, Nodes};
use drivers::virtio::{Queue, Virtio};
use proto::generated::liber::share_device::v1::{ShareData, ShareEntry, ShareInfo, ShareKind, ShareListing, ShareNode, share_device};
use proto::system::Error;
use rt::*;

// The queues: FORGET goes on the high-priority one, every other request on the first request queue.
const HIPRIO_QUEUE: u16 = 0;
const REQUEST_QUEUE: u16 = 1;

// The configuration: the tag, NUL-padded, then how many request queues the device has.
const CONFIG_TAG: u64 = 0;
const TAG_LEN: usize = 36;
const CONFIG_REQUEST_QUEUES: u64 = 36;

// THE ANSWER'S LAYOUT: its header in a page of its own and its body in DATA_PAGES pages after it, each page one
// descriptor - the way the FUSE wire is laid out by the drivers its servers are written against, and what makes a
// body larger than a page reach the guest at all: one descriptor spanning several pages was answered EIO by the host's
// daemon for every read past the first page. A chain is at most eight descriptors, so four pages of body.
const PAGE: u64 = 4096;
const DATA_PAGES: usize = 4;
const REPLY_LEN: u64 = (1 + DATA_PAGES as u64) * PAGE;

// THE BOUNDS THIS DRIVER SERVES: one read, one page of a listing, and the bytes asked of one READDIR - each the body
// the answer's pages hold.
const MAX_READ: usize = DATA_PAGES * PAGE as usize;
const MAX_ENTRIES: usize = 64;
const LISTING_BYTES: u32 = MAX_READ as u32;

// One DMA buffer: its address here and the device's.
#[derive(Clone, Copy)]
struct Buffer {
	virt: u64,
	phys: u64,
	len: u64,
}

impl Buffer {
	fn write(&self, at: usize, bytes: &[u8]) {
		// SAFETY: this driver's own DMA buffer; every caller writes inside it.
		unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), (self.virt + at as u64) as *mut u8, bytes.len()) };
	}

	fn bytes(&self, at: usize, len: usize) -> &[u8] {
		let at = at.min(self.len as usize);
		// SAFETY: as above; the device has given the chain back before this is read.
		unsafe { core::slice::from_raw_parts((self.virt + at as u64) as *const u8, len.min(self.len as usize - at)) }
	}

	fn clear(&self, len: usize) {
		// SAFETY: as above.
		unsafe { core::ptr::write_bytes(self.virt as *mut u8, 0, len.min(self.len as usize)) };
	}
}

// THE SHARE: the two queues, a request buffer and a reply buffer, the tag and the next request's number.
struct Share {
	hiprio: Queue,
	requests: Queue,
	request: Buffer,
	reply: Buffer,
	tag: String,
	unique: u64,
}

impl Share {
	// ONE REQUEST AND ITS ANSWER: the header and the body parts out, the answer back, decoded against this request's
	// number. The body of a successful answer, copied out; a host error as this system's meaning.
	fn call(&mut self, opcode: u32, nodeid: u64, parts: &[&[u8]]) -> Result<Vec<u8>, Error> {
		let body: usize = parts.iter().map(|part| part.len()).sum();
		let len = fuse::IN_HEADER + body;
		if len as u64 > PAGE {
			return Err(Error::Invalid);
		}
		self.unique = self.unique.wrapping_add(1).max(1);
		let unique = self.unique;
		self.request.write(0, &fuse::in_header(len as u32, opcode, unique, nodeid));
		let mut at = fuse::IN_HEADER;
		for part in parts {
			self.request.write(at, part);
			at += part.len();
		}
		self.reply.clear(fuse::OUT_HEADER);
		let mut chain = [(0u64, 0u32, false); 2 + DATA_PAGES];
		chain[0] = (self.request.phys, len as u32, false);
		chain[1] = (self.reply.phys, fuse::OUT_HEADER as u32, true);
		for page in 0..DATA_PAGES {
			chain[2 + page] = (self.reply.phys + (1 + page as u64) * PAGE, PAGE as u32, true);
		}
		let mut used = [0u32; 1];
		// EVERY FAILURE IS SAID, with the operation and the cause: what a consumer sees is a meaning, and the line
		// is where the host's own answer stays visible to a developer.
		if let Err(fault) = self.requests.submit_chains(&[&chain], None, &mut used) {
			print(format!("driver.virtio-fs: FUSE operation {opcode} was not completed by the device ({fault:?})\n").as_bytes());
			return Err(Error::Io);
		}
		// THE ANSWER REASSEMBLED: its header out of the first page, its body out of the pages after it.
		let written = used[0] as usize;
		let mut answered: Vec<u8> = Vec::new();
		answered.try_reserve_exact(written).map_err(|_| Error::Exhausted)?;
		answered.extend_from_slice(self.reply.bytes(0, written.min(fuse::OUT_HEADER)));
		answered.extend_from_slice(self.reply.bytes(PAGE as usize, written.saturating_sub(fuse::OUT_HEADER)));
		let (errno, body) = match fuse::out_header(&answered, unique) {
			Ok(answer) => answer,
			Err(fault) => {
				print(format!("driver.virtio-fs: FUSE operation {opcode} was answered malformed ({fault:?}, {} byte(s) used)\n", used[0]).as_bytes());
				return Err(Error::Io);
			}
		};
		if errno != 0 {
			if opcode != fuse::LOOKUP || errno != 2 {
				print(format!("driver.virtio-fs: FUSE operation {opcode} answered errno {errno} (answer pages at {:#x})\n", self.reply.phys).as_bytes());
			}
			return Err(fuse::errno_error(errno));
		}
		let mut out = Vec::new();
		out.try_reserve_exact(body.len()).map_err(|_| Error::Exhausted)?;
		out.extend_from_slice(body);
		Ok(out)
	}

	// THE LOOKUPS OF A NODE GIVEN BACK, on the high-priority queue; FORGET has no answer.
	fn forget(&mut self, nodeid: u64, count: u64) {
		if count == 0 {
			return;
		}
		self.unique = self.unique.wrapping_add(1).max(1);
		let len = fuse::IN_HEADER + fuse::FORGET_IN;
		self.request.write(0, &fuse::in_header(len as u32, fuse::FORGET, self.unique, nodeid));
		self.request.write(fuse::IN_HEADER, &fuse::forget_in(count));
		let chain: [(u64, u32, bool); 1] = [(self.request.phys, len as u32, false)];
		let mut used = [0u32; 1];
		let _ = self.hiprio.submit_chains(&[&chain], None, &mut used);
	}

	fn init(&mut self) -> Result<(), Error> {
		let answer = self.call(fuse::INIT, 0, &[&fuse::init_in()])?;
		fuse::init_out(&answer).map(|_| ()).map_err(|_| Error::Unsupported)
	}

	fn getattr(&mut self, nodeid: u64) -> Result<Attr, Error> {
		let answer = self.call(fuse::GETATTR, nodeid, &[&fuse::getattr_in()])?;
		fuse::attr_out(&answer).map_err(|_| Error::Io)
	}

	fn lookup(&mut self, dir: u64, name: &[u8]) -> Result<(u64, Attr), Error> {
		let mut body = [0u8; fuse::NAME_MAX + 1];
		let len = fuse::lookup_in(name, &mut body).map_err(|_| Error::Invalid)?;
		let answer = self.call(fuse::LOOKUP, dir, &[&body[..len]])?;
		match fuse::entry_out(&answer) {
			Ok(found) => Ok(found),
			// AN ENTRY THAT NAMES NO NODE is a negative answer some servers give for a name that is not there.
			Err(_) => Err(Error::NotFound),
		}
	}

	// THE BYTES OF A FILE: opened, read once, released - no handle outlives the request.
	fn read(&mut self, nodeid: u64, offset: u64, length: u32) -> Result<Vec<u8>, Error> {
		let opened = self.call(fuse::OPEN, nodeid, &[&fuse::open_in(fuse::O_RDONLY)])?;
		let fh = fuse::open_out(&opened).map_err(|_| Error::Io)?;
		let read = self.call(fuse::READ, nodeid, &[&fuse::read_in(fh, offset, length, fuse::O_RDONLY)]);
		let _ = self.call(fuse::RELEASE, nodeid, &[&fuse::release_in(fh, fuse::O_RDONLY)]);
		let mut data = read?;
		data.truncate(length as usize);
		Ok(data)
	}

	// ONE PAGE OF A DIRECTORY: opened, listed from `cursor`, released.
	fn list(&mut self, nodeid: u64, cursor: u64) -> Result<ShareListing, Error> {
		let opened = self.call(fuse::OPENDIR, nodeid, &[&fuse::open_in(fuse::O_RDONLY)])?;
		let fh = fuse::open_out(&opened).map_err(|_| Error::Io)?;
		let listed = self.call(fuse::READDIR, nodeid, &[&fuse::read_in(fh, cursor, LISTING_BYTES, fuse::O_RDONLY)]);
		let _ = self.call(fuse::RELEASEDIR, nodeid, &[&fuse::release_in(fh, fuse::O_RDONLY)]);
		let body = listed?;
		let mut entries: Vec<ShareEntry> = Vec::new();
		let mut next = cursor;
		let mut full = false;
		let mut failed = false;
		fuse::dirents(&body, |entry, after| {
			if full || failed {
				return;
			}
			if let Some(entry) = entry {
				if entries.len() == MAX_ENTRIES {
					full = true;
					return;
				}
				// The name passed the rules, so it is UTF-8.
				let Ok(name) = core::str::from_utf8(entry.name) else { return };
				let mut owned = String::new();
				if owned.try_reserve_exact(name.len()).is_err() || entries.try_reserve(1).is_err() {
					failed = true;
					return;
				}
				owned.push_str(name);
				entries.push(ShareEntry { name: owned, kind: share_kind(entry.kind) });
			}
			next = after;
		})
		.map_err(|_| Error::Io)?;
		if failed {
			return Err(Error::Exhausted);
		}
		// AN EMPTY ANSWER IS THE END: a directory continued past its last record answers nothing.
		Ok(ShareListing { entries, next, done: body.is_empty() })
	}
}

fn share_kind(kind: Kind) -> ShareKind {
	match kind {
		Kind::File => ShareKind::File,
		Kind::Directory => ShareKind::Directory,
		Kind::Other => ShareKind::Other,
	}
}

fn node(handle: u32, attr: &Attr) -> ShareNode {
	ShareNode { handle, kind: share_kind(attr.kind), size: attr.size, permissions: attr.permissions, modified: attr.mtime }
}

// ONE CONNECTION'S VIEW: the share, and the nodes this connection holds.
struct Connection<'a> {
	share: &'a mut Share,
	nodes: &'a mut Nodes,
}

impl Connection<'_> {
	// A held node, of the kind the request needs.
	fn held(&self, handle: u32, wanted: Kind) -> Result<u64, Error> {
		let (nodeid, kind) = self.nodes.get(handle).ok_or(Error::NotFound)?;
		if kind != wanted {
			return Err(Error::Invalid);
		}
		Ok(nodeid)
	}
}

impl share_device::Service for Connection<'_> {
	fn describe(&mut self) -> Result<ShareInfo, Error> {
		let mut tag = String::new();
		tag.try_reserve_exact(self.share.tag.len()).map_err(|_| Error::Exhausted)?;
		tag.push_str(&self.share.tag);
		Ok(ShareInfo { tag, max_read: MAX_READ as u32, max_nodes: fuse::MAX_NODES as u32 })
	}

	fn root(&mut self) -> Result<ShareNode, Error> {
		let attr = self.share.getattr(fuse::ROOT)?;
		if attr.kind != Kind::Directory {
			return Err(Error::Io);
		}
		Ok(node(fuse::ROOT_HANDLE, &attr))
	}

	fn lookup(&mut self, dir: u32, name: String) -> Result<ShareNode, Error> {
		// NOTHING IS WALKED THROUGH BUT A DIRECTORY: a symbolic link is a node of its own, never the place it points.
		let parent = self.held(dir, Kind::Directory)?;
		fuse::check_name(name.as_bytes()).map_err(|_| Error::Invalid)?;
		let (nodeid, attr) = self.share.lookup(parent, name.as_bytes())?;
		match self.nodes.looked_up(nodeid, attr.kind) {
			Some(handle) => Ok(node(handle, &attr)),
			None => {
				self.share.forget(nodeid, 1);
				Err(Error::Exhausted)
			}
		}
	}

	fn attributes(&mut self, handle: u32) -> Result<ShareNode, Error> {
		let (nodeid, _) = self.nodes.get(handle).ok_or(Error::NotFound)?;
		let attr = self.share.getattr(nodeid)?;
		Ok(node(handle, &attr))
	}

	fn list(&mut self, dir: u32, cursor: u64) -> Result<ShareListing, Error> {
		let nodeid = self.held(dir, Kind::Directory)?;
		self.share.list(nodeid, cursor)
	}

	fn read(&mut self, handle: u32, offset: u64, length: u32) -> Result<ShareData, Error> {
		if length as usize > MAX_READ {
			return Err(Error::Invalid);
		}
		// A FILE AND NOTHING ELSE IS OPENED: not a link, a device, a socket or a pipe.
		let nodeid = self.held(handle, Kind::File)?;
		if length == 0 {
			return Ok(ShareData { bytes: Vec::new() });
		}
		self.share.read(nodeid, offset, length).map(|bytes| ShareData { bytes })
	}

	fn forget(&mut self, handle: u32) -> Result<(), Error> {
		if handle == fuse::ROOT_HANDLE {
			return Err(Error::Invalid);
		}
		let (nodeid, count) = self.nodes.forget(handle).ok_or(Error::NotFound)?;
		self.share.forget(nodeid, count);
		Ok(())
	}
}

// THE NODES OF EVERY CONNECTION, by the connection's channel.
struct Clients {
	entries: Vec<(u64, Nodes)>,
}

impl Clients {
	fn of(&mut self, channel: u64) -> Option<&mut Nodes> {
		if let Some(at) = self.entries.iter().position(|(held, _)| *held == channel) {
			return Some(&mut self.entries[at].1);
		}
		if self.entries.len() >= common::MAX_PROVIDER_CLIENTS || self.entries.try_reserve(1).is_err() {
			return None;
		}
		self.entries.push((channel, Nodes::new()));
		self.entries.last_mut().map(|(_, nodes)| nodes)
	}

	fn take(&mut self, channel: u64) -> Option<Nodes> {
		let at = self.entries.iter().position(|(held, _)| *held == channel)?;
		Some(self.entries.swap_remove(at).1)
	}
}

fn serve(share: &mut Share, clients: &mut Clients, serving: &common::Serving, index: usize, buf: &mut [u8], reply: &mut [u8]) -> bool {
	let channel = serving.at(index);
	let (len, mut handles) = match try_recv_caps(channel, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return true,
		PolledCaps::Closed => return false,
	};
	let Some(nodes) = clients.of(channel) else { return false };
	let mut reply_handles = wire::Handles::new();
	let mut view = Connection { share, nodes };
	if let Some(written) = share_device::dispatch(&mut view, &buf[..len], &mut handles, reply, &mut reply_handles) {
		send_caps_blocking(channel, &reply[..written], reply_handles.as_slice());
	}
	true
}

// A consumer has gone: every node it held given back to the host, and the manager told.
fn part(share: &mut Share, clients: &mut Clients, serving: &mut common::Serving, bootstrap: u64, bind: &common::Bind, index: usize) -> bool {
	let channel = serving.at(index);
	if let Some(mut nodes) = clients.take(channel) {
		for (nodeid, count) in nodes.drain() {
			share.forget(nodeid, count);
		}
	}
	let token = serving.close_at(index);
	common::disconnected(bootstrap, bind, token)
}

struct Sleep<'a> {
	device: &'a Virtio,
	share: &'a mut Share,
	clients: &'a mut Clients,
	serving: &'a mut common::Serving,
	stopped: bool,
}

impl common::SleepStep for Sleep<'_> {
	fn suspend(&mut self, request: &driver_protocol::SuspendRequest) -> driver_protocol::Suspended {
		match self.device.sleep(request) {
			Some(stopped) => {
				self.stopped = stopped;
				driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Done, awake_by_ms: 0 }
			}
			None => driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Refused(driver_protocol::DriverFailureCode::DeviceNotResponding), awake_by_ms: 0 },
		}
	}

	// A DEVICE THAT LOST ITS POWER LOST ITS FUSE SESSION: it is opened again, and every handle a connection held names
	// nothing now - a consumer looks its nodes up again.
	fn resume(&mut self, lost_power: bool) -> bool {
		let woke = self.device.wake(&mut [&mut self.share.hiprio, &mut self.share.requests], lost_power, core::mem::take(&mut self.stopped));
		if woke && lost_power {
			self.clients.entries.clear();
			return self.share.init().is_ok();
		}
		woke
	}

	fn serving(&mut self) -> Option<&mut common::Serving> {
		Some(self.serving)
	}
}

fn buffer(device: &Virtio, len: u64) -> Buffer {
	// SAFETY: the bound device's capability; a buffer the device may read and write.
	match unsafe { dma_buffer_for(device.capability, len) } {
		Some((_, virt, phys)) => Buffer { virt, phys, len },
		None => exit(),
	}
}

// The tag as the configuration holds it: NUL-padded, printable, or no tag at all.
fn read_tag(device: &Virtio) -> String {
	let mut bytes = [0u8; TAG_LEN];
	for (at, byte) in bytes.iter_mut().enumerate() {
		*byte = device.config_read(CONFIG_TAG + at as u64);
	}
	let len = bytes.iter().position(|byte| *byte == 0).unwrap_or(TAG_LEN);
	let mut tag = String::new();
	if bytes[..len].iter().all(|byte| byte.is_ascii_graphic()) && tag.try_reserve_exact(len).is_ok() {
		tag.extend(bytes[..len].iter().map(|byte| *byte as char));
	}
	tag
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		let (bind, resources) = common::handshake(bootstrap);
		let device: Virtio = common::bringup_bound(bootstrap, &bind, &resources, 0);
		let queues = u32::from_le_bytes([
			device.config_read(CONFIG_REQUEST_QUEUES),
			device.config_read(CONFIG_REQUEST_QUEUES + 1),
			device.config_read(CONFIG_REQUEST_QUEUES + 2),
			device.config_read(CONFIG_REQUEST_QUEUES + 3),
		]);
		if queues == 0 {
			print(b"driver.virtio-fs: the device has no request queue - it is not driven\n");
			common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::UnsupportedDevice);
		}
		let Some(hiprio) = device.setup_queue(HIPRIO_QUEUE) else { common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable) };
		let Some(requests) = device.setup_queue(REQUEST_QUEUE) else { common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable) };
		let tag = read_tag(&device);
		let mut share = Share { hiprio, requests, request: buffer(&device, PAGE), reply: buffer(&device, REPLY_LEN), tag, unique: 0 };
		device.driver_ok();
		if share.init().is_err() {
			print(b"driver.virtio-fs: the host's file server did not open a FUSE session this driver speaks - it is not driven\n");
			common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::DeviceNotResponding);
		}
		let Some((mine, far)) = channel() else { common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::OutOfMemory) };
		let mut line = [0u8; 64];
		let mut detail: common::Bounded<32> = common::Bounded::new();
		detail.push(b"tag ");
		detail.push(share.tag.as_bytes());
		detail.push(b", read-only");
		let n = common::describe(&mut line, b"virtio-fs", &device, detail.as_bytes());
		if !common::online(bootstrap, &bind, &line[..n], &[(driver_protocol::provider::HOST_SHARE, far)]) {
			exit();
		}
		let mut serving = common::Serving::from_offers(&[(0, mine)]);
		let mut clients = Clients { entries: Vec::new() };
		let mut buf = alloc::vec![0u8; 8192];
		let mut reply = alloc::vec![0u8; MAX_READ + 1024];
		common::takes_sleep();
		loop {
			match common::wait_providers_until(bootstrap, &bind, &mut serving, &[], 0) {
				None => {
					if common::stop_requested() {
						common::finish_stop(bootstrap, &bind, device.capability, common::quiesce_virtio());
					}
					exit();
				}
				Some(None) => {
					if !common::take_sleep_step(bootstrap, &bind, &mut Sleep { device: &device, share: &mut share, clients: &mut clients, serving: &mut serving, stopped: false }) {
						if common::stop_requested() {
							common::finish_stop(bootstrap, &bind, device.capability, common::quiesce_virtio());
						}
						exit();
					}
				}
				Some(Some(common::ProviderReady::Connected(_))) | Some(Some(common::ProviderReady::Device(_))) => {}
				Some(Some(common::ProviderReady::Consumer(index))) => {
					if !serve(&mut share, &mut clients, &serving, index, &mut buf, &mut reply) && !part(&mut share, &mut clients, &mut serving, bootstrap, &bind, index) {
						exit();
					}
				}
			}
		}
	}
}
