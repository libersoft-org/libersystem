// driver.virtio-vsock - the userspace virtio-vsock driver.
//
// vsock is a stream transport between this guest and its HOST, addressed by a context id and a port
// instead of by an IP address. It exists here for provisioning, diagnostics and agents: a machine
// with no network configured, or one whose network is deliberately closed, still has a way for its
// operator to reach a program inside it.
//
// IT IS PUBLISHED AS `local-stream` AND DELIBERATELY NOT AS `net`. A consumer of `net` gets a link
// it can route and a stack above it; a consumer of this gets connections to ONE peer, by port, with
// no addressing and no routing at all. If this had been published as `net` it would have become an
// ambient path around NetworkService reachable by everything that already asks for a link - which is
// exactly what the item that owns this driver says it must not be. The driver grants nothing,
// decides no policy and holds no credential: it moves bytes for whoever DeviceManager connected to
// it, and PermissionManager decides who that is.
//
// THE DECISIONS ARE IN `drivers::vsock` AND ARE HOST-TESTED: the packet header, the credit window
// and the connection state machine. Credit is where a vsock driver is wrong, silently and in both
// directions, and it is modular arithmetic on two counters the PEER moves - so it is tested against
// a peer that lies rather than against a device that does not.

#![no_std]
#![no_main]

use driver_protocol::stream;
use drivers::{common, virtio, vsock};
use rt::*;

// The queues virtio-vsock defines.
const QUEUE_RX: u16 = 0;
const QUEUE_TX: u16 = 1;

// Device configuration: the guest's own context id, as a little-endian u64 at offset zero. It is
// the HOST that assigns it, which is why it is read rather than chosen.
const CFG_GUEST_CID: u64 = 0;

// The receive pool. Each slot holds one packet header and the largest payload this wire carries.
const RX_SLOTS: u16 = 8;
const SLOT_BYTES: u64 = vsock::HEADER_LEN as u64 + stream::MAX_PAYLOAD as u64;

// WHAT THIS DRIVER TELLS THE PEER ITS RECEIVE BUFFER IS, PER STREAM. The peer is entitled to send
// this much without hearing anything back, so it must be a number this driver can actually hold: it
// is the staging buffer below and not the ring, because bytes sit in the staging buffer until a
// consumer asks for them and the ring slot is re-posted immediately.
//
// AND IT IS PER STREAM NOW, so the number is the table's budget divided by the table. Eight
// kilobytes holds two whole maximum payloads; a window the size of ONE would stall the peer after
// every packet until a consumer read, which is a correct driver that moves bytes in lock-step.
const STAGING_BYTES: usize = 8192;

// How long a connect waits for the host to answer, in the 100 Hz ticks READY is measured in. The
// host either answers or resets almost immediately; this is the budget for it having gone away.
const CONNECT_TICKS: u64 = 40;
// How long a receive waits for bytes before answering "nothing yet". Short on purpose: a consumer
// that gets an empty answer asks again, and a driver that blocked here could not answer a stop.
const RECEIVE_TICKS: u64 = 2;

// HOW MANY STREAMS THIS DRIVER SERVES AT ONCE.
//
// ONE CONSUMER IS ONE STREAM. The registry declares that many publications each admitting ONE
// consumer, and the reason is the one the entry has always given: two consumers sharing a stream
// would interleave their bytes with nothing to separate them again. What changes here is that there
// are several streams rather than one, which is what "connection limits" means - a bound kept by a
// table rather than a bound of one.
//
// FOUR RATHER THAN EIGHT. A driver serves at most eight provider connections, and each stream
// carries a staging buffer: four is the number whose buffers stay inside this driver's stack while
// leaving the window per stream large enough not to move bytes in lock-step.
const MOST_STREAMS: usize = 4;

// The first of this driver's local ports. A stream's port is this plus its slot, so a packet coming
// back from the host names the stream it belongs to by arithmetic rather than by a search - and one
// naming a port outside the range belongs to nobody. See `vsock::slot_of`.
const LOCAL_PORT_BASE: u32 = 1024;

// THE EVENT QUEUE. Four slots of one `u32`: what arrives here is a statement about the TRANSPORT and
// not about any connection, so the queue is never deep.
const QUEUE_EVENT: u16 = 2;
const EVENT_SLOTS: u16 = 4;
const EVENT_BYTES: u64 = vsock::EVENT_LEN as u64;

// The staging buffer a connection's received bytes wait in.
struct Staging {
	bytes: [u8; STAGING_BYTES],
	// The window of bytes held: `at .. end`.
	at: usize,
	end: usize,
}

impl Staging {
	fn new() -> Staging {
		Staging { bytes: [0u8; STAGING_BYTES], at: 0, end: 0 }
	}

	fn held(&self) -> usize {
		self.end - self.at
	}

	fn room(&self) -> usize {
		STAGING_BYTES - self.end + self.at
	}

	// Take up to `most` bytes out.
	fn take(&mut self, most: usize, out: &mut [u8]) -> usize {
		let moved = self.held().min(most).min(out.len());
		out[..moved].copy_from_slice(&self.bytes[self.at..self.at + moved]);
		self.at += moved;
		if self.at == self.end {
			self.at = 0;
			self.end = 0;
		}
		moved
	}

	// Put bytes in, compacting first so the window is contiguous. False when they do not fit, which
	// is the peer having exceeded the credit this driver advertised.
	fn put(&mut self, data: &[u8]) -> bool {
		if data.len() > self.room() {
			return false;
		}
		if self.end + data.len() > STAGING_BYTES {
			self.bytes.copy_within(self.at..self.end, 0);
			self.end -= self.at;
			self.at = 0;
		}
		self.bytes[self.end..self.end + data.len()].copy_from_slice(data);
		self.end += data.len();
		true
	}
}

// One connection, and everything credit accounting needs.
struct Connection {
	state: vsock::State,
	// The peer's port - the one a consumer asked to reach.
	port: u32,
	// THIS SIDE'S PORT, which is what tells two streams apart. Both ends of every stream this driver
	// holds share a context id and a host, so the local port is the only field an arriving packet
	// carries that says which stream it is for.
	local: u32,
	// What the peer says about its own receive buffer, updated from every packet it sends.
	peer_buf_alloc: u32,
	peer_fwd_cnt: u32,
	// Everything this driver has written to the peer, free-running and wrapping.
	tx_cnt: u32,
	// Everything this driver has taken out of its own receive buffer and handed to its consumer,
	// free-running and wrapping. THE PEER'S WINDOW IS COMPUTED FROM THIS, so it counts bytes
	// DELIVERED and not bytes that arrived: a driver reporting arrivals tells the peer there is room
	// while its staging buffer is full.
	fwd_cnt: u32,
	// The last `fwd_cnt` the peer was told about, so a credit update is sent when it has moved
	// enough to matter rather than on every read.
	told: u32,
}

impl Connection {
	fn new(port: u32, local: u32) -> Connection {
		Connection { state: vsock::State::Connecting, port, local, peer_buf_alloc: 0, peer_fwd_cnt: 0, tx_cnt: 0, fwd_cnt: 0, told: 0 }
	}

	fn closed(local: u32) -> Connection {
		Connection { state: vsock::State::Closed, port: 0, local, peer_buf_alloc: 0, peer_fwd_cnt: 0, tx_cnt: 0, fwd_cnt: 0, told: 0 }
	}
}

// ONE CONSUMER'S STREAM: its connection, the bytes waiting for it, and which consumer it is for.
struct Stream {
	// The consumer endpoint this stream belongs to, or zero for a free slot.
	//
	// THE ENDPOINT AND NOT ITS INDEX IN `Serving`. `close_at` fills the hole it makes with the LAST
	// entry, so an index names a different consumer the moment any other one leaves - and a stream
	// found by index would then be somebody else's bytes under the right name.
	owner: u64,
	connection: Connection,
	staging: Staging,
}

impl Stream {
	fn free(slot: usize) -> Stream {
		Stream { owner: 0, connection: Connection::closed(LOCAL_PORT_BASE + slot as u32), staging: Staging::new() }
	}
}

// Which slot serves this consumer, if any.
fn slot_owned_by(streams: &[Stream; MOST_STREAMS], endpoint: u64) -> Option<usize> {
	streams.iter().position(|stream| stream.owner == endpoint)
}

unsafe fn w8(addr: u64, value: u8) {
	unsafe { (addr as *mut u8).write_volatile(value) }
}

unsafe fn r8(addr: u64) -> u8 {
	unsafe { (addr as *const u8).read_volatile() }
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		let (bind, device) = common::bringup(bootstrap);
		let mut guest_cid: u64 = 0;
		for i in 0..8u64 {
			guest_cid |= (device.config_read(CFG_GUEST_CID + i) as u64) << (i * 8);
		}

		let rx = device.setup_queue(QUEUE_RX);
		let tx = device.setup_queue(QUEUE_TX);
		// THE EVENT QUEUE IS SET UP AND READ. It carries one thing - the transport being reset under
		// this driver, after a migration or a reconfiguration of the host's device - and that is a
		// statement no connection's own state machine can make for itself: every connection is gone
		// and none of them was told. A queue configured and never posted to, which is what this was,
		// is a queue that cannot deliver it.
		let mut events = device.setup_queue(QUEUE_EVENT);
		let (Some(mut rx), Some(tx)) = (rx, tx) else {
			device.driver_ok();
			let mut line = [0u8; 64];
			let n = common::describe_state(&mut line, b"virtio-vsock", &device, b"DEGRADED", b"no rx/tx queue");
			common::online_and_stand(bootstrap, &bind, &line[..n], 0, 0, device.capability);
		};

		// One pool for the receive slots and one page for the transmit packet.
		let Some((rxpool, rx_virt, _)) = dma_buffer_for(device.capability, RX_SLOTS as u64 * SLOT_BYTES) else {
			device.driver_ok();
			let mut line = [0u8; 64];
			let n = common::describe_state(&mut line, b"virtio-vsock", &device, b"DEGRADED", b"no receive pool");
			common::online_and_stand(bootstrap, &bind, &line[..n], 0, 0, device.capability);
		};
		let Some((_txbuf, tx_virt, tx_phys)) = dma_buffer_for(device.capability, SLOT_BYTES) else {
			device.driver_ok();
			let mut line = [0u8; 64];
			let n = common::describe_state(&mut line, b"virtio-vsock", &device, b"DEGRADED", b"no transmit buffer");
			common::online_and_stand(bootstrap, &bind, &line[..n], 0, 0, device.capability);
		};
		let mut rx_phys = [0u64; RX_SLOTS as usize];
		for id in 0..RX_SLOTS {
			rx_phys[id as usize] = dma_buffer_phys_at(rxpool, id as u64 * SLOT_BYTES);
			rx.post_recv(id, rx_phys[id as usize], SLOT_BYTES as u32);
		}
		rx.notify();
		// THE EVENT POOL, AND A DRIVER THAT CANNOT HAVE ONE STILL RUNS. An event is news about the
		// transport, not a way to move bytes: a machine whose event queue could not be set up or
		// given a buffer serves its streams exactly as before and is told so, rather than refusing
		// to bind over a queue almost nothing ever writes to.
		let mut event_virt = 0u64;
		let mut event_phys = [0u64; EVENT_SLOTS as usize];
		if events.is_some() {
			match dma_buffer_for(device.capability, EVENT_SLOTS as u64 * EVENT_BYTES) {
				Some((pool, virt, _)) => {
					event_virt = virt;
					if let Some(queue) = events.as_mut() {
						for id in 0..EVENT_SLOTS {
							event_phys[id as usize] = dma_buffer_phys_at(pool, id as u64 * EVENT_BYTES);
							queue.post_recv(id, event_phys[id as usize], EVENT_BYTES as u32);
						}
						queue.notify();
					}
				}
				None => {
					events = None;
					print(b"driver.virtio-vsock: no buffer for the event queue - a transport reset will not be noticed\n");
				}
			}
		} else {
			print(b"driver.virtio-vsock: no event queue on this device - a transport reset will not be noticed\n");
		}
		device.driver_ok();

		let Some((server, client)) = channel() else {
			let mut line = [0u8; 64];
			let n = common::describe_state(&mut line, b"virtio-vsock", &device, b"DEGRADED", b"no channel");
			common::online_and_stand(bootstrap, &bind, &line[..n], 0, 0, device.capability);
		};
		let mut report: common::Bounded<96> = common::Bounded::new();
		report.push(b"driver.virtio-vsock: online (");
		report.push(&common::hex2(bind.info.bus));
		report.push(b":");
		report.push(&common::hex2(bind.info.dev));
		report.push(b".");
		report.push(&[b'0' + (bind.info.func % 10)]);
		report.push(b", guest cid ");
		report.decimal(guest_cid);
		report.push(b")");
		common::online(bootstrap, &bind, report.as_bytes(), &[(driver_protocol::provider::LOCAL_STREAM, client)]);
		serve(bootstrap, &bind, &device, &mut rx, &tx, &mut events, rx_virt, &rx_phys, tx_virt, tx_phys, event_virt, &event_phys, guest_cid, server)
	}
}

// Build a packet in the transmit buffer and hand it to the device.
#[allow(clippy::too_many_arguments)]
unsafe fn send(tx: &virtio::Queue, tx_virt: u64, tx_phys: u64, guest_cid: u64, connection: &Connection, op: u16, flags: u32, payload: &[u8]) -> bool {
	unsafe {
		let header = vsock::Header { src_cid: guest_cid, dst_cid: vsock::CID_HOST, src_port: connection.local, dst_port: connection.port, len: payload.len() as u32, kind: vsock::TYPE_STREAM, op, flags, buf_alloc: STAGING_BYTES as u32, fwd_cnt: connection.fwd_cnt };
		let bytes = header.encode();
		for (i, byte) in bytes.iter().enumerate() {
			w8(tx_virt + i as u64, *byte);
		}
		for (i, byte) in payload.iter().enumerate() {
			w8(tx_virt + vsock::HEADER_LEN as u64 + i as u64, *byte);
		}
		// READ-ONLY, SO THE DEVICE MAY REPORT NOTHING WRITTEN. `submit_checked` bounds the used
		// length by what the chain offered for writing, which for a transmit is zero - and this
		// device does write zero, unlike virtio-scsi, which is why the ordinary bound is right here.
		let bufs = [(tx_phys, vsock::HEADER_LEN as u32 + payload.len() as u32, false)];
		match tx.submit_checked(&bufs) {
			Ok(_) => true,
			Err(fault) => {
				let mut line: common::Bounded<128> = common::Bounded::new();
				line.push(b"driver.virtio-vsock: the queue refused a packet - ");
				line.push(match fault {
					virtio::UsedFault::Id => b"the used element named a descriptor this chain did not post",
					virtio::UsedFault::Length => b"the device reported more written than the chain offered",
					virtio::UsedFault::NoCompletion => b"the device never completed it",
					_ => b"the chain was refused",
				});
				line.push(b"\n");
				print(line.as_bytes());
				false
			}
		}
	}
}

// Take every packet the device has filled, apply it to the stream it names, and re-post the slot.
//
// THE PACKET IS ADMITTED BEFORE IT IS BELIEVED and the credit it carries is applied even when this
// driver is not waiting for anything: a credit update arrives unasked, and a driver that only read
// the ring while waiting for a response would stall its own writes behind a window it never saw
// open.
//
// AND THE STREAM IS CHOSEN BY THE PORT THE PACKET NAMES, not by whichever one the driver was last
// working on. One ring carries every stream's packets, interleaved in whatever order the host sent
// them, and the local port is the only field that tells them apart - see `vsock::slot_of`.
//
// Answers a bit per stream whose peer sent past the window this driver advertised.
unsafe fn drain(rx: &mut virtio::Queue, rx_virt: u64, rx_phys: &[u64; RX_SLOTS as usize], guest_cid: u64, streams: &mut [Stream; MOST_STREAMS]) -> u32 {
	unsafe {
		let mut overrun = 0u32;
		while let Some((id, len)) = rx.take_used() {
			if (id as usize) < rx_phys.len() {
				let at = rx_virt + id as u64 * SLOT_BYTES;
				let mut bytes = [0u8; vsock::HEADER_LEN];
				if len as usize >= vsock::HEADER_LEN {
					for (i, byte) in bytes.iter_mut().enumerate() {
						*byte = r8(at + i as u64);
					}
				}
				if let Some(header) = vsock::Header::decode(&bytes) {
					if let Some(slot) = vsock::slot_of(header.dst_port, LOCAL_PORT_BASE, MOST_STREAMS as u32) {
						let stream_at = &mut streams[slot];
						if vsock::admit(&header, len as usize, guest_cid, stream_at.connection.local, stream::MAX_PAYLOAD).is_ok() {
							// THE CREDIT IS TAKEN FROM EVERY PACKET, not only from a credit update:
							// the specification puts these two fields in every header precisely so
							// that a data packet carries the window with it.
							stream_at.connection.peer_buf_alloc = header.buf_alloc;
							stream_at.connection.peer_fwd_cnt = header.fwd_cnt;
							if header.op == vsock::OP_RW && header.len > 0 {
								let mut payload = [0u8; stream::MAX_PAYLOAD as usize];
								let take = (header.len as usize).min(payload.len());
								for (i, byte) in payload[..take].iter_mut().enumerate() {
									*byte = r8(at + vsock::HEADER_LEN as u64 + i as u64);
								}
								if !stream_at.staging.put(&payload[..take]) {
									// The peer sent past the window this driver advertised. That is
									// the hostile-credit case, and the answer is a reset rather than
									// a silent drop: dropping leaves both ends believing different
									// things about the byte stream for ever.
									overrun |= 1 << slot;
								}
							}
							stream_at.connection.state = vsock::advance(stream_at.connection.state, header.op, header.flags);
						}
					}
				}
				rx.post_recv(id, rx_phys[id as usize], SLOT_BYTES as u32);
			}
		}
		rx.notify();
		overrun
	}
}

// Read the event queue and say whether the transport was reset under this driver.
//
// AN EVENT IS ABOUT THE TRANSPORT AND NOT ABOUT A CONNECTION, which is why one answer covers every
// stream: a reset means the other end of all of them has been taken away - after a migration, or
// after the host's vsock device was reconfigured - and a driver that went on writing into them would
// be writing to nobody while telling its consumers the connection is open.
//
// AND AN EVENT THIS DRIVER DOES NOT KNOW IS RE-POSTED AND NOT ACTED ON. Treating an unrecognised
// number as a reset would close every connection on a message the specification may give a meaning
// to later.
unsafe fn drain_events(events: &mut virtio::Queue, virt: u64, phys: &[u64; EVENT_SLOTS as usize]) -> bool {
	unsafe {
		let mut reset = false;
		while let Some((id, len)) = events.take_used() {
			if (id as usize) < phys.len() {
				let at = virt + id as u64 * EVENT_BYTES;
				let mut bytes = [0u8; vsock::EVENT_LEN];
				if len as usize >= vsock::EVENT_LEN {
					for (i, byte) in bytes.iter_mut().enumerate() {
						*byte = r8(at + i as u64);
					}
					if vsock::event(&bytes) == Some(vsock::EVENT_TRANSPORT_RESET) {
						reset = true;
					}
				}
				events.post_recv(id, phys[id as usize], EVENT_BYTES as u32);
			}
		}
		events.notify();
		reset
	}
}

#[allow(clippy::too_many_arguments)]
unsafe fn serve(bootstrap: u64, bind: &common::Bind, device: &virtio::Virtio, rx: &mut virtio::Queue, tx: &virtio::Queue, events: &mut Option<virtio::Queue>, rx_virt: u64, rx_phys: &[u64; RX_SLOTS as usize], tx_virt: u64, tx_phys: u64, event_virt: u64, event_phys: &[u64; EVENT_SLOTS as usize], guest_cid: u64, server: u64) -> ! {
	unsafe {
		let mut serving = common::Serving::new(server, 0);
		let mut streams: [Stream; MOST_STREAMS] = core::array::from_fn(Stream::free);
		let mut request = [0u8; stream::REQUEST_LEN + stream::MAX_PAYLOAD as usize];
		loop {
			let Some(at) = common::serve_any_or_answer(bootstrap, bind, &mut serving) else {
				for stream_at in streams.iter_mut() {
					if stream_at.connection.state != vsock::State::Closed {
						send(tx, tx_virt, tx_phys, guest_cid, &stream_at.connection, vsock::OP_RST, 0, &[]);
					}
				}
				let quiet = common::quiesce_virtio();
				common::finish_stop(bootstrap, bind, device.capability, quiet);
				exit();
			};
			let endpoint = serving.at(at);
			// A CONSUMER THAT CLOSED IS ONE CLIENT LEAVING AND NOT THIS DRIVER'S END. The rule for
			// dropping it and telling the manager is in `recv_from_consumer`, which says why.
			//
			// AND ITS STREAM GOES WITH IT, which is this driver's own half: the consumer that left
			// is the one that stream was for, and leaving it open would hold a host-side connection
			// nobody can read from or close - the host going on believing a program in this guest is
			// listening. The slot is given back to the table in the same breath, or "connection
			// limits" would mean four connections per boot rather than four at a time.
			let Some((len, handle)) = common::recv_from_consumer(bootstrap, bind, &mut serving, at, &mut request) else {
				if let Some(slot) = slot_owned_by(&streams, endpoint) {
					if streams[slot].connection.state != vsock::State::Closed {
						send(tx, tx_virt, tx_phys, guest_cid, &streams[slot].connection, vsock::OP_RST, 0, &[]);
					}
					streams[slot] = Stream::free(slot);
				}
				continue;
			};
			if handle != 0 {
				close(handle);
			}
			// THE TRANSPORT'S OWN NEWS IS READ BEFORE THE REQUEST IS ANSWERED, which is the only
			// moment its answer differs: a reset means every connection is gone, and the operation
			// about to run is the one that would otherwise write into a dead one and report success.
			if let Some(queue) = events.as_mut() {
				if drain_events(queue, event_virt, event_phys) {
					print(b"driver.virtio-vsock: the transport was reset - every connection is closed\n");
					for (slot, stream_at) in streams.iter_mut().enumerate() {
						let owner = stream_at.owner;
						*stream_at = Stream::free(slot);
						stream_at.owner = owner;
					}
				}
			}
			let Some((stream::Request { op, arg }, payload)) = stream::Request::decode(&request[..len]) else {
				answer(endpoint, stream::STATUS_INVALID, &[]);
				continue;
			};
			match op {
				stream::OP_CONNECT => {
					// A CONSUMER GETS ONE STREAM AT A TIME. Asking again while its own is OPEN is a
					// mistake rather than a second connection - the wire has no way to say which of
					// the two a later SEND is for - but asking again after it has CLOSED is an
					// ordinary reconnect, and it reuses the slot it already holds.
					let held = slot_owned_by(&streams, endpoint);
					if let Some(slot) = held {
						if streams[slot].connection.state != vsock::State::Closed {
							answer(endpoint, stream::STATUS_INVALID, &[]);
							continue;
						}
					}
					// THE BOUND IS THE TABLE AND THE REFUSAL IS TYPED. A driver that served one more
					// consumer by evicting the first would take a live stream away from a program
					// that never asked; one that queued it would answer a connect whenever somebody
					// unrelated happened to finish.
					let Some(slot) = held.or_else(|| streams.iter().position(|stream_at| stream_at.owner == 0)) else {
						print(b"driver.virtio-vsock: every stream is in use - this connect is refused\n");
						answer(endpoint, stream::STATUS_ERR, &[]);
						continue;
					};
					// A RECONNECT STARTS EMPTY. Bytes left in the staging buffer from the connection
					// that closed belong to a stream that no longer exists, and handing them to the
					// next one would be the previous peer's data read out of the new one.
					streams[slot] = Stream::free(slot);
					streams[slot].owner = endpoint;
					streams[slot].connection = Connection::new(arg, LOCAL_PORT_BASE + slot as u32);
					if !send(tx, tx_virt, tx_phys, guest_cid, &streams[slot].connection, vsock::OP_REQUEST, 0, &[]) {
						streams[slot] = Stream::free(slot);
						answer(endpoint, stream::STATUS_ERR, &[]);
						continue;
					}
					// WAIT FOR AN ANSWER, WITH A DEADLINE IN TICKS. A host that never answers is a
					// host that has gone away, which the item names as a case: a wait with no
					// deadline would make that the driver's last act.
					//
					// AND THE WAIT DRAINS THE WHOLE RING, not this stream's packets. One ring
					// carries every stream, so a driver that read only what it was waiting for would
					// leave another stream's bytes behind the response it wanted.
					let mut deadline = common::Deadline::ticks(CONNECT_TICKS);
					while streams[slot].connection.state == vsock::State::Connecting && deadline.waiting() {
						drain(rx, rx_virt, rx_phys, guest_cid, &mut streams);
					}
					match streams[slot].connection.state {
						vsock::State::Open => answer(endpoint, stream::STATUS_OK, &[]),
						// A RESET IS A REFUSAL AND A TIMEOUT IS NOT THE SAME ANSWER. Closed here
						// means the host answered with a reset - nothing is listening on that port.
						// Still Connecting means it said nothing at all.
						vsock::State::Closed => {
							streams[slot] = Stream::free(slot);
							streams[slot].owner = endpoint;
							answer(endpoint, stream::STATUS_CLOSED, &[]);
						}
						_ => {
							streams[slot] = Stream::free(slot);
							streams[slot].owner = endpoint;
							answer(endpoint, stream::STATUS_ERR, &[]);
						}
					}
				}
				stream::OP_SEND => {
					let Some(slot) = slot_owned_by(&streams, endpoint) else {
						answer(endpoint, stream::STATUS_CLOSED, &[]);
						continue;
					};
					drain(rx, rx_virt, rx_phys, guest_cid, &mut streams);
					if !vsock::may_write(streams[slot].connection.state) {
						answer(endpoint, stream::STATUS_CLOSED, &[]);
						continue;
					}
					// THE WINDOW IS ASKED FOR RATHER THAN ASSUMED, and a peer whose numbers do not
					// admit a window at all is refused rather than written to: `may_send` answers
					// None for a peer claiming to have taken more than it was sent, which is a
					// window wider than its own buffer.
					let Some(window) = vsock::may_send(streams[slot].connection.peer_buf_alloc, streams[slot].connection.peer_fwd_cnt, streams[slot].connection.tx_cnt) else {
						send(tx, tx_virt, tx_phys, guest_cid, &streams[slot].connection, vsock::OP_RST, 0, &[]);
						streams[slot].connection.state = vsock::State::Closed;
						print(b"driver.virtio-vsock: the peer's credit is wider than its own buffer - the connection is reset\n");
						answer(endpoint, stream::STATUS_CLOSED, &[]);
						continue;
					};
					// A SHORT WRITE IS AN HONEST ANSWER AND A DROPPED TAIL IS NOT. The reply carries
					// how many bytes went, so a caller with a full window writes the rest next time.
					let moving = (window as usize).min(payload.len());
					if moving == 0 {
						answer(endpoint, stream::STATUS_OK, &[]);
						continue;
					}
					if !send(tx, tx_virt, tx_phys, guest_cid, &streams[slot].connection, vsock::OP_RW, 0, &payload[..moving]) {
						answer(endpoint, stream::STATUS_ERR, &[]);
						continue;
					}
					streams[slot].connection.tx_cnt = streams[slot].connection.tx_cnt.wrapping_add(moving as u32);
					answer_len(endpoint, stream::STATUS_OK, moving as u32);
				}
				stream::OP_RECEIVE => {
					let Some(slot) = slot_owned_by(&streams, endpoint) else {
						answer(endpoint, stream::STATUS_CLOSED, &[]);
						continue;
					};
					let mut deadline = common::Deadline::ticks(RECEIVE_TICKS);
					// AN OVERRUN ON ANOTHER STREAM IS THAT STREAM'S, and it is reset there rather
					// than being carried back here: one ring feeds every stream, so a read on one
					// consumer's stream routinely drains packets belonging to another's.
					let mut mine = false;
					loop {
						let overran = drain(rx, rx_virt, rx_phys, guest_cid, &mut streams);
						if overran != 0 {
							for (other, stream_at) in streams.iter_mut().enumerate() {
								if overran & (1 << other) == 0 {
									continue;
								}
								send(tx, tx_virt, tx_phys, guest_cid, &stream_at.connection, vsock::OP_RST, 0, &[]);
								stream_at.connection.state = vsock::State::Closed;
								print(b"driver.virtio-vsock: the peer sent past the window this driver advertised - the connection is reset\n");
							}
							if overran & (1 << slot) != 0 {
								mine = true;
								break;
							}
						}
						if streams[slot].staging.held() > 0 || !vsock::may_read(streams[slot].connection.state) || !deadline.waiting() {
							break;
						}
					}
					if mine {
						answer(endpoint, stream::STATUS_CLOSED, &[]);
						continue;
					}
					if streams[slot].staging.held() == 0 && !vsock::may_read(streams[slot].connection.state) {
						answer(endpoint, stream::STATUS_CLOSED, &[]);
						continue;
					}
					let mut out = [0u8; stream::MAX_PAYLOAD as usize];
					let most = (arg as usize).min(out.len());
					let moved = streams[slot].staging.take(most, &mut out);
					streams[slot].connection.fwd_cnt = streams[slot].connection.fwd_cnt.wrapping_add(moved as u32);
					// TELL THE PEER ITS WINDOW HAS REOPENED once half the buffer has been consumed.
					// A driver that never sends this stalls after one buffer's worth of bytes, and
					// one that sends it per read spends a packet on every byte.
					if streams[slot].connection.fwd_cnt.wrapping_sub(streams[slot].connection.told) >= (STAGING_BYTES as u32) / 2 {
						if send(tx, tx_virt, tx_phys, guest_cid, &streams[slot].connection, vsock::OP_CREDIT_UPDATE, 0, &[]) {
							streams[slot].connection.told = streams[slot].connection.fwd_cnt;
						}
					}
					answer(endpoint, stream::STATUS_OK, &out[..moved]);
				}
				stream::OP_SHUTDOWN => {
					let Some(slot) = slot_owned_by(&streams, endpoint) else {
						answer(endpoint, stream::STATUS_CLOSED, &[]);
						continue;
					};
					if streams[slot].connection.state == vsock::State::Closed {
						answer(endpoint, stream::STATUS_CLOSED, &[]);
						continue;
					}
					// THE CONSUMER'S FLAGS ARE TRANSLATED AND NOT PASSED THROUGH. A consumer says
					// which direction IT is done with; the packet says which direction the SENDER is
					// done with, and this side is the sender - so "I will read no more" goes out as
					// RECEIVE and "I will write no more" as SEND. They happen to be the same two
					// bits, which is exactly why this is written out rather than left implicit.
					let mut flags = 0u32;
					if arg & stream::SHUTDOWN_READ != 0 {
						flags |= vsock::SHUTDOWN_RECEIVE;
					}
					if arg & stream::SHUTDOWN_WRITE != 0 {
						flags |= vsock::SHUTDOWN_SEND;
					}
					send(tx, tx_virt, tx_phys, guest_cid, &streams[slot].connection, vsock::OP_SHUTDOWN, flags, &[]);
					// AND THE SAME FLAGS LAND ON THIS CONNECTION THE OTHER WAY ROUND. `advance` reads
					// a shutdown as an ARRIVING one - the peer saying what it will stop doing - so
					// applying our own to it without swapping closes the half we meant to keep.
					let mut ours = 0u32;
					if arg & stream::SHUTDOWN_READ != 0 {
						ours |= vsock::SHUTDOWN_SEND;
					}
					if arg & stream::SHUTDOWN_WRITE != 0 {
						ours |= vsock::SHUTDOWN_RECEIVE;
					}
					streams[slot].connection.state = vsock::advance(streams[slot].connection.state, vsock::OP_SHUTDOWN, ours);
					if streams[slot].connection.state == vsock::State::Closed {
						send(tx, tx_virt, tx_phys, guest_cid, &streams[slot].connection, vsock::OP_RST, 0, &[]);
					}
					answer(endpoint, stream::STATUS_OK, &[]);
				}
				stream::OP_IDENTITY => answer(endpoint, stream::STATUS_OK, &guest_cid.to_le_bytes()),
				_ => answer(endpoint, stream::STATUS_INVALID, &[]),
			}
		}
	}
}

// Answer with a status and a payload.
fn answer(endpoint: u64, status: u32, payload: &[u8]) {
	let mut frame = [0u8; stream::REPLY_LEN + stream::MAX_PAYLOAD as usize];
	frame[..stream::REPLY_LEN].copy_from_slice(&stream::reply(status, payload.len() as u32));
	frame[stream::REPLY_LEN..stream::REPLY_LEN + payload.len()].copy_from_slice(payload);
	send_blocking(endpoint, &frame[..stream::REPLY_LEN + payload.len()], 0);
}

// Answer a send with how many bytes of it went, which is not always all of them.
fn answer_len(endpoint: u64, status: u32, len: u32) {
	send_blocking(endpoint, &stream::reply(status, len), 0);
}
