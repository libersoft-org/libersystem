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

// WHAT THIS DRIVER TELLS THE PEER ITS RECEIVE BUFFER IS. The peer is entitled to send this much
// without hearing anything back, so it must be a number this driver can actually hold: it is the
// staging buffer below and not the ring, because bytes sit in the staging buffer until a consumer
// asks for them and the ring slot is re-posted immediately.
const STAGING_BYTES: usize = 16384;

// How long a connect waits for the host to answer, in the 100 Hz ticks READY is measured in. The
// host either answers or resets almost immediately; this is the budget for it having gone away.
const CONNECT_TICKS: u64 = 40;
// How long a receive waits for bytes before answering "nothing yet". Short on purpose: a consumer
// that gets an empty answer asks again, and a driver that blocked here could not answer a stop.
const RECEIVE_TICKS: u64 = 2;

// The port this driver calls from. A single connection at a time, so a single local port.
const LOCAL_PORT: u32 = 1024;

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
	port: u32,
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
	fn new(port: u32) -> Connection {
		Connection { state: vsock::State::Connecting, port, peer_buf_alloc: 0, peer_fwd_cnt: 0, tx_cnt: 0, fwd_cnt: 0, told: 0 }
	}
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
		// THE EVENT QUEUE IS SET UP AND NOT USED, which is deliberate rather than an omission: the
		// device expects three queues to exist and a device whose third queue was never configured
		// is a device this driver has misinitialised. Nothing is posted to it, so nothing arrives.
		let _event = device.setup_queue(2);
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
		serve(bootstrap, &bind, &device, &mut rx, &tx, rx_virt, &rx_phys, tx_virt, tx_phys, guest_cid, server)
	}
}

// Build a packet in the transmit buffer and hand it to the device.
#[allow(clippy::too_many_arguments)]
unsafe fn send(tx: &virtio::Queue, tx_virt: u64, tx_phys: u64, guest_cid: u64, connection: &Connection, op: u16, flags: u32, payload: &[u8]) -> bool {
	unsafe {
		let header = vsock::Header { src_cid: guest_cid, dst_cid: vsock::CID_HOST, src_port: LOCAL_PORT, dst_port: connection.port, len: payload.len() as u32, kind: vsock::TYPE_STREAM, op, flags, buf_alloc: STAGING_BYTES as u32, fwd_cnt: connection.fwd_cnt };
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

// Take every packet the device has filled, apply it to the connection, and re-post the slot.
//
// THE PACKET IS ADMITTED BEFORE IT IS BELIEVED and the credit it carries is applied even when this
// driver is not waiting for anything: a credit update arrives unasked, and a driver that only read
// the ring while waiting for a response would stall its own writes behind a window it never saw
// open.
#[allow(clippy::too_many_arguments)]
// Answers whether the peer sent past the window this driver advertised.
unsafe fn drain(rx: &mut virtio::Queue, rx_virt: u64, rx_phys: &[u64; RX_SLOTS as usize], guest_cid: u64, connection: &mut Connection, staging: &mut Staging) -> bool {
	unsafe {
		let mut overrun = false;
		while let Some((id, len)) = rx.take_used() {
			if (id as usize) < rx_phys.len() {
				let slot = rx_virt + id as u64 * SLOT_BYTES;
				let mut bytes = [0u8; vsock::HEADER_LEN];
				if len as usize >= vsock::HEADER_LEN {
					for (i, byte) in bytes.iter_mut().enumerate() {
						*byte = r8(slot + i as u64);
					}
				}
				if let Some(header) = vsock::Header::decode(&bytes) {
					if vsock::admit(&header, len as usize, guest_cid, LOCAL_PORT, stream::MAX_PAYLOAD).is_ok() {
						// THE CREDIT IS TAKEN FROM EVERY PACKET, not only from a credit update: the
						// specification puts these two fields in every header precisely so that a
						// data packet carries the window with it.
						connection.peer_buf_alloc = header.buf_alloc;
						connection.peer_fwd_cnt = header.fwd_cnt;
						if header.op == vsock::OP_RW && header.len > 0 {
							let mut payload = [0u8; stream::MAX_PAYLOAD as usize];
							let take = (header.len as usize).min(payload.len());
							for (i, byte) in payload[..take].iter_mut().enumerate() {
								*byte = r8(slot + vsock::HEADER_LEN as u64 + i as u64);
							}
							if !staging.put(&payload[..take]) {
								// The peer sent past the window this driver advertised. That is the
								// hostile-credit case, and the answer is a reset rather than a
								// silent drop: dropping leaves both ends believing different things
								// about the byte stream for ever.
								overrun = true;
							}
						}
						connection.state = vsock::advance(connection.state, header.op, header.flags);
					}
				}
				rx.post_recv(id, rx_phys[id as usize], SLOT_BYTES as u32);
			}
		}
		rx.notify();
		overrun
	}
}

#[allow(clippy::too_many_arguments)]
unsafe fn serve(bootstrap: u64, bind: &common::Bind, device: &virtio::Virtio, rx: &mut virtio::Queue, tx: &virtio::Queue, rx_virt: u64, rx_phys: &[u64; RX_SLOTS as usize], tx_virt: u64, tx_phys: u64, guest_cid: u64, server: u64) -> ! {
	unsafe {
		let mut serving = common::Serving::new(server, 0);
		let mut connection = Connection { state: vsock::State::Closed, port: 0, peer_buf_alloc: 0, peer_fwd_cnt: 0, tx_cnt: 0, fwd_cnt: 0, told: 0 };
		let mut staging = Staging::new();
		let mut request = [0u8; stream::REQUEST_LEN + stream::MAX_PAYLOAD as usize];
		loop {
			let Some(at) = common::serve_any_or_answer(bootstrap, bind, &mut serving) else {
				if connection.state != vsock::State::Closed {
					send(tx, tx_virt, tx_phys, guest_cid, &connection, vsock::OP_RST, 0, &[]);
				}
				let quiet = common::quiesce_virtio();
				common::finish_stop(bootstrap, bind, device.capability, quiet);
				exit();
			};
			let endpoint = serving.at(at);
			let Received::Message { len, handle } = recv_blocking(endpoint, &mut request) else {
				continue;
			};
			if handle != 0 {
				close(handle);
			}
			let Some((stream::Request { op, arg }, payload)) = stream::Request::decode(&request[..len]) else {
				answer(endpoint, stream::STATUS_INVALID, &[]);
				continue;
			};
			match op {
				stream::OP_CONNECT => {
					if connection.state != vsock::State::Closed {
						answer(endpoint, stream::STATUS_INVALID, &[]);
						continue;
					}
					connection = Connection::new(arg);
					staging = Staging::new();
					if !send(tx, tx_virt, tx_phys, guest_cid, &connection, vsock::OP_REQUEST, 0, &[]) {
						connection.state = vsock::State::Closed;
						answer(endpoint, stream::STATUS_ERR, &[]);
						continue;
					}
					// WAIT FOR AN ANSWER, WITH A DEADLINE IN TICKS. A host that never answers is a
					// host that has gone away, which the item names as a case: a wait with no
					// deadline would make that the driver's last act.
					let mut deadline = common::Deadline::ticks(CONNECT_TICKS);
					while connection.state == vsock::State::Connecting && deadline.waiting() {
						drain(rx, rx_virt, rx_phys, guest_cid, &mut connection, &mut staging);
					}
					match connection.state {
						vsock::State::Open => answer(endpoint, stream::STATUS_OK, &[]),
						// A RESET IS A REFUSAL AND A TIMEOUT IS NOT THE SAME ANSWER. Closed here
						// means the host answered with a reset - nothing is listening on that port.
						// Still Connecting means it said nothing at all.
						vsock::State::Closed => {
							answer(endpoint, stream::STATUS_CLOSED, &[]);
						}
						_ => {
							connection.state = vsock::State::Closed;
							answer(endpoint, stream::STATUS_ERR, &[]);
						}
					}
				}
				stream::OP_SEND => {
					drain(rx, rx_virt, rx_phys, guest_cid, &mut connection, &mut staging);
					if !vsock::may_write(connection.state) {
						answer(endpoint, stream::STATUS_CLOSED, &[]);
						continue;
					}
					// THE WINDOW IS ASKED FOR RATHER THAN ASSUMED, and a peer whose numbers do not
					// admit a window at all is refused rather than written to: `may_send` answers
					// None for a peer claiming to have taken more than it was sent, which is a
					// window wider than its own buffer.
					let Some(window) = vsock::may_send(connection.peer_buf_alloc, connection.peer_fwd_cnt, connection.tx_cnt) else {
						send(tx, tx_virt, tx_phys, guest_cid, &connection, vsock::OP_RST, 0, &[]);
						connection.state = vsock::State::Closed;
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
					if !send(tx, tx_virt, tx_phys, guest_cid, &connection, vsock::OP_RW, 0, &payload[..moving]) {
						answer(endpoint, stream::STATUS_ERR, &[]);
						continue;
					}
					connection.tx_cnt = connection.tx_cnt.wrapping_add(moving as u32);
					answer_len(endpoint, stream::STATUS_OK, moving as u32);
				}
				stream::OP_RECEIVE => {
					let mut deadline = common::Deadline::ticks(RECEIVE_TICKS);
					let mut reset = false;
					loop {
						if drain(rx, rx_virt, rx_phys, guest_cid, &mut connection, &mut staging) {
							reset = true;
							break;
						}
						if staging.held() > 0 || !vsock::may_read(connection.state) || !deadline.waiting() {
							break;
						}
					}
					if reset {
						send(tx, tx_virt, tx_phys, guest_cid, &connection, vsock::OP_RST, 0, &[]);
						connection.state = vsock::State::Closed;
						print(b"driver.virtio-vsock: the peer sent past the window this driver advertised - the connection is reset\n");
						answer(endpoint, stream::STATUS_CLOSED, &[]);
						continue;
					}
					if staging.held() == 0 && !vsock::may_read(connection.state) {
						answer(endpoint, stream::STATUS_CLOSED, &[]);
						continue;
					}
					let mut out = [0u8; stream::MAX_PAYLOAD as usize];
					let most = (arg as usize).min(out.len());
					let moved = staging.take(most, &mut out);
					connection.fwd_cnt = connection.fwd_cnt.wrapping_add(moved as u32);
					// TELL THE PEER ITS WINDOW HAS REOPENED once half the buffer has been consumed.
					// A driver that never sends this stalls after one buffer's worth of bytes, and
					// one that sends it per read spends a packet on every byte.
					if connection.fwd_cnt.wrapping_sub(connection.told) >= (STAGING_BYTES as u32) / 2 {
						if send(tx, tx_virt, tx_phys, guest_cid, &connection, vsock::OP_CREDIT_UPDATE, 0, &[]) {
							connection.told = connection.fwd_cnt;
						}
					}
					answer(endpoint, stream::STATUS_OK, &out[..moved]);
				}
				stream::OP_SHUTDOWN => {
					if connection.state == vsock::State::Closed {
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
					send(tx, tx_virt, tx_phys, guest_cid, &connection, vsock::OP_SHUTDOWN, flags, &[]);
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
					connection.state = vsock::advance(connection.state, vsock::OP_SHUTDOWN, ours);
					if connection.state == vsock::State::Closed {
						send(tx, tx_virt, tx_phys, guest_cid, &connection, vsock::OP_RST, 0, &[]);
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
