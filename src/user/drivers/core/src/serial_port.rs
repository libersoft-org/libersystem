// ONE VIRTIO-SERIAL PORT, SERVED AS A BYTE STREAM - the half two drivers need.
//
// WHY IT IS HERE AND NOT IN A BINARY. The development channel drives ONE port of its own device and
// the console driver drives every port of a MULTIPORT one, and both owe a consumer the same thing:
// `liber:device@1`'s `console-stream`, whose `attach` settles a version before a byte moves, whose
// `write` ANSWERS - `again` is a host that stopped reading - and whose `receive` hands back a stream
// endpoint. The dev-channel driver said so where it was written: "a later multiport driver can carry
// the same protocol on a named port without anything above it changing". This is that, moved rather
// than copied, because two copies of a byte-stream contract is the one duplication whose divergence
// is invisible - mismatched bytes are refused nowhere, they arrive somewhere else in the reader.
//
// WHAT STAYS IN EACH BINARY is what is not about a port: which device it binds, how many ports it
// has, what it publishes them as, and what it does when the manager asks it to stop.

use alloc::vec::Vec;
use rt::*;

use crate::common;
use crate::virtio::{Queue, Virtio};
use proto::system::{ConsoleAttachment, ConsoleChunk, Error, console_stream};
use wire::Handles;

// The receive pool: enough slots that a burst of host writes is absorbed without the device
// running out of buffers between two interrupts, each large enough that an ordinary control
// frame arrives in one.
pub const RX_SLOTS: u16 = 8;
pub const RX_SLOT: u64 = 4096;

// How many chunks a consumer may fall behind by before the port stops taking new ones for it. The
// receive pool is the real bound on what can be in flight at once; twice it leaves room for the
// consumer to be between two reads without the ring stalling.
pub const STREAM_DEPTH: u64 = RX_SLOTS as u64 * 2;

// The largest transmit buffer one port needs. The protocol above defines a larger frame than one
// `write` may carry, and that is deliberate: a byte stream has no frame boundaries, so a consumer
// with more to say than the contract's bound simply says it in two writes.
pub const MAX_FRAME: usize = 65536;

// THE CONTRACT'S OWN BOUND AND VERSION, READ FROM THE ONE PLACE BOTH ENDS READ THEM FROM. A
// constant this driver defined for itself and another the consumer defined for itself are two
// constants, and the day they differ is the day a byte stream starts arriving somewhere else in the
// reader with nothing refused anywhere. `driver_protocol::console` is the shared crate, and the
// decisions made from these numbers are host-tested there rather than only in a booted guest.
pub const MAX_WRITE: usize = driver_protocol::console::MAX_WRITE as usize;
pub const CONTRACT_VERSION: u32 = driver_protocol::console::WIRE_VERSION;

// How long a write may wait for the transmit buffer to come back, in scheduler ticks
// (100 Hz). The host end can stop reading at any moment, and QEMU then stops consuming the
// transmit queue rather than discarding what it cannot deliver, so the buffer stays with the
// device.
//
// This matches the session's own idle deadline rather than being an independent, shorter
// guess. A shorter one drops replies from a host that is merely slow to read - a host
// pipelining up to the advertised outstanding bound can easily have more reply bytes in
// flight than a socket buffer holds, and losing its answers for reading a moment late is not
// a bound, it is data loss. A host that has genuinely gone is caught by the same window the
// session uses to decide the same thing, which is the only question this deadline is really
// asking.
pub const TX_DRAIN_TICKS: u64 = 3000;

pub struct Port<'a> {
	device: &'a Virtio,
	irq: u64,
	// OWNED RATHER THAN BORROWED, because a multiport device has one of these per port and a
	// borrowed queue would make the set of them a set of borrows of one device's queues that no
	// single scope can hand out. The queue is plain data - a ring's addresses and two indices - so
	// moving it into the port that is the only thing allowed to drive it costs nothing.
	tx: Queue,
	virt: u64,
	phys: u64,
	// The device owns the transmit buffer and has not returned it yet.
	busy: bool,
}

impl Port<'_> {
	// Reap every transmit completion the device has posted, which is what releases the
	// buffer for the next write.
	pub fn reclaim(&mut self) {
		while self.tx.take_used().is_some() {
			self.busy = false;
		}
	}

	// Put bytes on the wire WITHOUT waiting for the previous write, and say whether they went.
	//
	// The asynchronous path is the only one this port has, and deliberately: the synchronous
	// `Queue::submit` busy-polls the used ring without touching the indices `take_used` accounts
	// against, so one of each on a single queue leaves the reaper looking at a completion it was
	// never told to expect. A driver that wants to say something before it has a consumer - a
	// stamp on a freshly opened port - says it here, and the pump's first `reclaim` takes the
	// completion like any other.
	pub unsafe fn send_now(&mut self, payload: &[u8]) -> bool {
		unsafe {
			if self.busy || payload.is_empty() || payload.len() > MAX_WRITE {
				return false;
			}
			core::ptr::copy_nonoverlapping(payload.as_ptr(), self.virt as *mut u8, payload.len());
			self.busy = self.tx.submit_async(&[(self.phys, payload.len() as u32, false)]);
			self.busy
		}
	}

	// Write bytes to the port, waiting - bounded - for the previous write to be taken first.
	//
	// THE REFUSAL IS TYPED NOW. This answered `false`, and the loop above it turned that into an
	// EMPTY message to the consumer: a convention that only worked because every frame the
	// consumer sent was at least a header long. `again` says the same thing in the contract's own
	// words - nothing was written and these bytes may be offered again - and it is the answer to
	// the very call that made them, rather than a message arriving out of order with the reply.
	unsafe fn write(&mut self, payload: &[u8], bind: &common::Bind, bootstrap: u64) -> Result<u32, Error> {
		unsafe {
			if payload.is_empty() || payload.len() > MAX_WRITE {
				return Err(Error::Invalid);
			}
			let limit: u64 = clock() + TX_DRAIN_TICKS;
			loop {
				// A host that is slow to consume bytes has not stopped this control path.
				if !common::answer_ping(bootstrap, bind) {
					return Err(Error::Closed);
				}
				self.reclaim();
				if !self.busy {
					break;
				}
				if clock() >= limit {
					return Err(Error::Again);
				}
				if wait_any(&[self.irq, bootstrap], limit) == 0 {
					let _ = self.device.read_isr();
					interrupt_ack(self.irq);
				}
			}
			if self.send_now(payload) { Ok(payload.len() as u32) } else { Err(Error::Io) }
		}
	}
}

// WHAT ONE ATTACHED CONSUMER IS, and all of it: whether it settled a version, the stream endpoint it
// asked for, and how many chunks have gone down that stream.
//
// A session and nothing more. It is reset rather than carried when a consumer is replaced, because
// the bytes a dead consumer's session held belong to that session: handing a fragment of one to its
// replacement would be read as the beginning of a frame.
#[derive(Default)]
pub struct Attachment {
	attached: bool,
	// The producer end of the receive stream, kept here for the life of the connection. Closing it
	// is what tells the consumer the stream ended.
	stream: u64,
	// The next chunk's sequence number on that stream, which the frame carries so a consumer can
	// see a gap rather than read past one.
	seq: u32,
	// The endpoint `receive` owes its caller, held between the service call that minted it and the
	// reply that carries it away.
	granted: u64,
}

impl Attachment {
	// This consumer is over. The stream goes with it - a consumer that is gone cannot be the one
	// still being written to - and anything a half-finished `receive` minted is closed rather than
	// leaked into the next session.
	pub fn reset(&mut self) {
		if self.stream != 0 {
			close(self.stream);
		}
		if self.granted != 0 {
			close(self.granted);
		}
		*self = Attachment::default();
	}
}

/// PUTTING BYTES ON A WIRE, which is the ONE thing the contract above needs and the only thing that
/// differs between the transports that serve it.
///
/// WHY THERE IS A TRAIT HERE AT ALL. Everything else in this file - the attachment, the session, the
/// stream grant, the backpressure, the frame the chunks go out in - is about the CONTRACT and is the
/// same whatever moves the bytes. What is not the same is the transport: a virtio port owns a queue
/// and a descriptor, and a USB CDC-ACM adapter owns a bulk pair on a controller's ring. Two copies
/// of a serve loop is how the two come to disagree about what `again` means, and `console-stream`
/// has exactly one definition of that.
///
/// AND THE BOUND IS THE IMPLEMENTOR'S. Each transport decides how long it waits for room and what it
/// does while waiting; what the contract fixes is that the answer is one of these, and that `Again`
/// means NOTHING was written and the bytes may be offered again.
pub trait Wire {
	/// Write bytes to the port and say what happened.
	///
	/// # Safety
	/// The implementor's buffers and rings are the driver's for the life of the process; a caller
	/// that no longer owns them must not call this.
	unsafe fn write(&mut self, payload: &[u8], bind: &common::Bind, bootstrap: u64) -> Result<u32, Error>;
}

impl Wire for Port<'_> {
	unsafe fn write(&mut self, payload: &[u8], bind: &common::Bind, bootstrap: u64) -> Result<u32, Error> {
		unsafe { Port::write(self, payload, bind, bootstrap) }
	}
}

// The service, which is the port seen through the contract. It borrows rather than owns: the port is
// the driver's and outlives every consumer that ever attaches to it.
pub struct Console<'a, W: Wire> {
	port: &'a mut W,
	bind: &'a common::Bind,
	bootstrap: u64,
	state: &'a mut Attachment,
}

impl<W: Wire> console_stream::Service for Console<'_, W> {
	fn attach(&mut self, version: u32) -> Result<ConsoleAttachment, Error> {
		match driver_protocol::console::attach(version, CONTRACT_VERSION, MAX_WRITE as u32) {
			driver_protocol::console::Attach::Speak { version, max_frame } => {
				self.state.attached = true;
				Ok(ConsoleAttachment { version, max_frame })
			}
			// A version this driver does not serve, or a bound it could not honour. Either way the
			// connection stays unattached, and every operation on it goes on being refused.
			_ => Err(Error::Unsupported),
		}
	}

	fn write(&mut self, bytes: Vec<u8>) -> Result<u32, Error> {
		// NOT ATTACHED IS NOT A SMALL WRITE, and neither is a length this contract cannot carry.
		// Both are refusals rather than adjustments: a consumer that has not settled a version does
		// not know how what comes back is framed, and a write silently cut to fit is a frame the
		// reader will never reassemble.
		match driver_protocol::console::admit_write(self.state.attached, bytes.len(), MAX_WRITE as u32) {
			driver_protocol::console::Admit::Write => {}
			driver_protocol::console::Admit::NotAttached | driver_protocol::console::Admit::BadLength => return Err(Error::Invalid),
		}
		// SAFETY: the port's transmit buffer and its queue are this driver's for the life of the
		// process; `write` is unsafe because it copies into that mapping and submits a descriptor.
		unsafe { Wire::write(self.port, &bytes, self.bind, self.bootstrap) }
	}

	fn receive(&mut self) -> Result<Vec<ConsoleChunk>, Error> {
		match driver_protocol::console::grant_stream(self.state.attached, self.state.stream != 0) {
			driver_protocol::console::Grant::Mint => {}
			driver_protocol::console::Grant::NotAttached => return Err(Error::Invalid),
			// ONE SUBSCRIPTION PER CONNECTION. A second endpoint would be a second reader of one
			// port, and the bytes would be divided between them by whichever was drained first.
			driver_protocol::console::Grant::AlreadyGranted => return Err(Error::Again),
		}
		let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else {
			return Err(Error::Exhausted);
		};
		self.state.stream = producer;
		self.state.granted = consumer;
		// The snapshot is EMPTY and always will be: a byte stream has no state to describe, only
		// bytes that have not arrived yet.
		Ok(Vec::new())
	}
}

// Move bytes between the port and whichever consumer the catalogue gave this provider to.
//
// The receive ring is drained before every wait rather than only after an interrupt. Both queues
// share the device's single MSI-X vector, so the wait inside a transmit drain can consume the very
// interrupt that announced newly arrived bytes; draining first means the loop never blocks while
// work is already queued, whichever wait observed it.

fn serve_once<W: Wire>(serving: &mut common::Serving, index: usize, state: &mut Attachment, port: &mut W, bind: &common::Bind, bootstrap: u64, request: &mut [u8], reply: &mut [u8]) -> bool {
	let end: u64 = serving.at(index);
	let (len, handle): (usize, u64) = match recv_blocking(end, request) {
		Received::Message { len, handle } => (len, handle),
		Received::Closed => return false,
	};
	// A request carrying a capability is a request this contract does not define. Closed rather
	// than dropped, so whatever was sent does not stay charged to the sender for ever.
	if handle != 0 {
		close(handle);
	}
	// THE OPCODE IS READ BEFORE THE DISPATCHER SEES IT, because `receive` answers with an ENDPOINT
	// and the generated dispatcher writes bytes. Reading two bytes here is not a second parser: the
	// dispatcher re-reads them and refuses anything it does not know.
	if len >= 2 && u16::from_le_bytes([request[0], request[1]]) == console_stream::OP_RECEIVE {
		return serve_receive(end, state, port, bind, bootstrap, &request[..len], reply);
	}
	let mut request_handles: Handles = Handles::new();
	let mut reply_handles: Handles = Handles::new();
	let mut console: Console<W> = Console { port, bind, bootstrap, state };
	let Some(written) = console_stream::dispatch(&mut console, &request[..len], &mut request_handles, reply, &mut reply_handles) else {
		// A request this contract does not define, or one whose reply would not fit. Neither is
		// answerable, and answering the NEXT request on this connection with this one's reply is
		// the failure a correlation id exists to make visible - so the connection ends here.
		return false;
	};
	send_blocking(end, &reply[..written], reply_handles.first())
}

// `receive`, whose reply carries the stream endpoint. The service mints the pair - it is the one
// that knows whether this consumer may have one - and this hands it over.
fn serve_receive<W: Wire>(end: u64, state: &mut Attachment, port: &mut W, bind: &common::Bind, bootstrap: u64, request: &[u8], reply: &mut [u8]) -> bool {
	let mut request_handles: Handles = Handles::new();
	let granted: u64;
	let outcome: Option<(u32, Result<Vec<ConsoleChunk>, Error>)> = {
		let mut console: Console<W> = Console { port, bind, bootstrap, state };
		console_stream::receive_open(&mut console, request, &mut request_handles)
	};
	let Some((corr, result)) = outcome else { return false };
	match result {
		Ok(_) => {
			granted = core::mem::take(&mut state.granted);
			let Some(written) = console_stream::receive_reply_ok(corr, reply) else {
				close(granted);
				return false;
			};
			if !send_blocking(end, &reply[..written], granted) {
				// The transfer did not happen, so the endpoint is still this driver's - and a
				// producer nobody will ever read is a stream that has to end rather than linger.
				close(granted);
				state.reset();
				return false;
			}
			true
		}
		Err(error) => {
			let Some(written) = console_stream::receive_reply_err(corr, &error, reply) else { return false };
			send_blocking(end, &reply[..written], 0)
		}
	}
}

// Hand the device's completed receive buffers to the attached consumer and put them back on the
// ring. True when anything was taken, which is what tells the caller to look again before parking.
//
// A consumer that has not asked for a stream, or none at all, is not an error: the bytes are
// discarded and the buffer is recycled. Both halves matter. Discarding is right because the bytes
// belong to a session nobody is holding; recycling is right because eight buffers is all the device
// has, and stopping here would leave it with nowhere to put what a host is still writing.
fn drain_receive(bind: &common::Bind, bootstrap: u64, rx: &mut Queue, rx_virt: u64, rx_phys: &[u64], state: &mut Attachment, frame: &mut [u8]) -> bool {
	let mut worked: bool = false;
	for _ in 0..RX_SLOTS {
		let Some((id, len)) = rx.take_used() else { break };
		if id >= RX_SLOTS {
			continue;
		}
		if len > 0 && state.stream != 0 {
			let n: usize = if len as u64 > RX_SLOT { RX_SLOT as usize } else { len as usize };
			// SAFETY: the slot is this driver's DMA mapping, `n` is bounded by the slot size, and
			// the device has given the buffer back - which is what `take_used` means.
			let chunk: &[u8] = unsafe { core::slice::from_raw_parts((rx_virt + id as u64 * RX_SLOT) as *const u8, n) };
			send_chunk(bind, bootstrap, state, chunk, frame);
		}
		rx.post_recv(id, rx_phys[id as usize], RX_SLOT as u32);
		worked = true;
	}
	if worked {
		rx.notify();
	}
	worked
}

// One chunk down the stream, with backpressure retained rather than dropped.
//
// A consumer that is merely slow has not gone, so a stalled send is waited on - and the manager's
// channel is answered while waiting, because a driver that stops answering pings while a consumer
// is behind is a driver a watchdog kills for someone else's slowness. A stream whose consumer has
// closed ends the attachment: there is nobody left to read what the port produces.
fn send_chunk(bind: &common::Bind, bootstrap: u64, state: &mut Attachment, chunk: &[u8], frame: &mut [u8]) {
	let mut handles: Handles = Handles::new();
	let item: ConsoleChunk = ConsoleChunk { bytes: chunk.to_vec() };
	let Some(written) = console_stream::receive_frame(state.seq, &item, frame, &mut handles) else { return };
	state.seq = state.seq.wrapping_add(1);
	loop {
		if !common::answer_ping(bootstrap, bind) {
			return;
		}
		match try_send_outcome(state.stream, &frame[..written], 0) {
			SendOutcome::Delivered => return,
			SendOutcome::Failed => {
				close(state.stream);
				state.stream = 0;
				return;
			}
			SendOutcome::Stalled => {
				// SYS_WAIT_ANY observes readability, not writability, so there is nothing to wait
				// ON here: the wait is on the manager's channel with a one-tick deadline, which
				// keeps the control path live and retries capacity on the next clock tick.
				let ready: i64 = wait_any_periodic(&[bootstrap], clock().saturating_add(1));
				if ready < 0 && ready != ERR_TIMED_OUT {
					return;
				}
			}
		}
	}
}

// ONE SERVED PORT, WHOLE: both queues, the receive pool behind one of them, the transmit buffer
// behind the other, and the single consumer's session.
//
// WHY THE BUNDLE IS THE UNIT. A driver with one port could keep these as six locals and did; a
// driver with N of them cannot, and the pieces are not independently useful - a receive pool with no
// attachment to hand its bytes to is bytes discarded, and a transmit buffer with no queue is a
// mapping. Bundling them is also what makes "one port" the thing a loop iterates over, which is the
// only shape in which the two drivers can share this code at all.
pub struct Stream<'a> {
	port: Port<'a>,
	rx: Queue,
	rx_virt: u64,
	rx_phys: [u64; RX_SLOTS as usize],
	state: Attachment,
}

impl<'a> Stream<'a> {
	// Set up one port's queue pair, post its receive pool and hand back the port ready to serve.
	//
	// THE QUEUES ARE THE CALLER'S TO NAME, because which pair a port owns is not this module's
	// decision: a single-port device's port is 0 and 1, and on a multiport one the pair is
	// `drivers::console`'s rule. None means the device has no such queue, which is a port that
	// cannot be served rather than a driver that cannot start.
	//
	// SAFETY: `device` must be a negotiated device this process owns, and `irq` its interrupt.
	pub unsafe fn open(device: &'a Virtio, irq: u64, receive: u16, transmit: u16) -> Option<Stream<'a>> {
		unsafe {
			let mut rx: Queue = device.setup_queue(receive)?;
			let tx: Queue = device.setup_queue(transmit)?;
			// Both queues, because the receive side is how bytes arrive and the transmit side's
			// completions are the only proof the buffer came back.
			rx.enable_interrupts();
			tx.enable_interrupts();
			let (pool, rx_virt, _): (u64, u64, u64) = dma_buffer_for(device.capability, RX_SLOTS as u64 * RX_SLOT)?;
			let (_txbuf, virt, phys): (u64, u64, u64) = dma_buffer_for(device.capability, MAX_FRAME as u64)?;
			// Each slot at its own physical address, with the contiguous virtual mapping read back
			// as `rx_virt + id * RX_SLOT`.
			let mut rx_phys: [u64; RX_SLOTS as usize] = [0u64; RX_SLOTS as usize];
			let mut id: u16 = 0;
			while id < RX_SLOTS {
				rx_phys[id as usize] = dma_buffer_phys_at(pool, id as u64 * RX_SLOT);
				rx.post_recv(id, rx_phys[id as usize], RX_SLOT as u32);
				id += 1;
			}
			rx.notify();
			Some(Stream { port: Port { device, irq, tx, virt, phys, busy: false }, rx, rx_virt, rx_phys, state: Attachment::default() })
		}
	}

	// The device capability behind this port's queues, which is what `finish_stop` hands to the
	// kernel so the frames and masked vectors this binding held can be reclaimed.
	pub fn capability(&self) -> u64 {
		self.rx.capability
	}

	// The device an interrupt has to be acknowledged on.
	pub fn device(&self) -> &Virtio {
		self.port.device
	}

	// Reap the transmit completions an interrupt announced.
	pub fn reclaim(&mut self) {
		self.port.reclaim();
	}

	// This consumer is over - see `Attachment::reset`.
	pub fn reset(&mut self) {
		self.state.reset();
	}

	// One line on this port before anybody is attached to it, which is what a driver stamps a
	// freshly opened port with. False when the port would not take it.
	//
	// SAFETY: as `Port::send_now`.
	pub unsafe fn stamp(&mut self, line: &[u8]) -> bool {
		unsafe { self.port.send_now(line) }
	}

	// Hand whatever the device has delivered to the attached consumer and re-post the buffers. True
	// when anything was taken, which is what tells the caller to look again before parking.
	pub fn drain(&mut self, bind: &common::Bind, bootstrap: u64, buffers: &mut Buffers) -> bool {
		drain_receive(bind, bootstrap, &mut self.rx, self.rx_virt, &self.rx_phys, &mut self.state, &mut buffers.frame)
	}

	// Serve one request from the consumer on `index`. False means that consumer is gone.
	pub fn serve(&mut self, serving: &mut common::Serving, index: usize, bind: &common::Bind, bootstrap: u64, buffers: &mut Buffers) -> bool {
		serve_once(serving, index, &mut self.state, &mut self.port, bind, bootstrap, &mut buffers.request, &mut buffers.reply)
	}
}

/// ONE ATTACHED CONSUMER AND THE CONTRACT PLUMBING TO SERVE IT, for a port whose transport is not a
/// virtio queue.
///
/// `Stream` IS THE VIRTIO SHAPE and owns more than this: two queues, a receive pool and the
/// descriptor bookkeeping behind them. A controller that owns its own rings - a USB CDC-ACM adapter
/// on an xHCI transfer ring - already has all of that and needs only the half that is about
/// `console-stream`: the attachment, the grant, the serve loop and the backpressure. Splitting it
/// here rather than copying it is what keeps the two transports from growing two answers to "what
/// does `again` mean".
#[derive(Default)]
pub struct Session {
	state: Attachment,
}

impl Session {
	pub fn new() -> Session {
		Session::default()
	}

	/// Serve one request from the consumer on `index`. False means that consumer is gone.
	pub fn serve<W: Wire>(&mut self, serving: &mut common::Serving, index: usize, port: &mut W, bind: &common::Bind, bootstrap: u64, buffers: &mut Buffers) -> bool {
		serve_once(serving, index, &mut self.state, port, bind, bootstrap, &mut buffers.request, &mut buffers.reply)
	}

	/// Hand bytes the transport received to the attached consumer.
	///
	/// BYTES WITH NOBODY TO TAKE THEM ARE DISCARDED, for the reason `drain_receive` states: they
	/// belong to a session nobody is holding, and a transport that stopped taking them instead would
	/// leave the device with nowhere to put what a host is still writing.
	pub fn deliver(&mut self, bind: &common::Bind, bootstrap: u64, chunk: &[u8], buffers: &mut Buffers) {
		if chunk.is_empty() || self.state.stream == 0 {
			return;
		}
		send_chunk(bind, bootstrap, &mut self.state, chunk, &mut buffers.frame);
	}

	/// Whether a consumer is attached and holding a stream, for a transport deciding whether a
	/// receive is worth posting.
	pub fn listening(&self) -> bool {
		self.state.stream != 0
	}

	/// This consumer is over - see `Attachment::reset`.
	pub fn reset(&mut self) {
		self.state.reset();
	}
}

// THE SCRATCH ONE PUMP NEEDS, sized from the contract rather than guessed, and shared by every port
// a driver serves.
//
// One set and not one per port, because a pump serves one port at a time: a second set would be an
// allocation nothing is ever reading. The sizes are the two bounds either end of this module already
// has - the largest write the contract admits, and the receive slot a chunk is cut from - each with
// room for the frame header that wraps it.
pub struct Buffers {
	request: Vec<u8>,
	reply: Vec<u8>,
	frame: Vec<u8>,
}

impl Default for Buffers {
	fn default() -> Self {
		Buffers { request: alloc::vec![0u8; MAX_WRITE + 64], reply: alloc::vec![0u8; 128], frame: alloc::vec![0u8; RX_SLOT as usize + 32] }
	}
}
