// Channel: the kernel's basic IPC object.
//
// A Channel is one end of a connected pair. A message written to one end is
// delivered to the peer's inbox and read from there. Both operations are
// non-blocking, which is the kernel's async IPC core: send() and recv() never
// wait. Waiting (blocking until an endpoint is readable) is layered on top by the
// scheduler later; for now a caller that gets WouldBlock cooperatively yields and
// retries.
//
// A message carries a small byte payload plus zero or more transferred
// capabilities (moved out of the sender's handle table and into the receiver's),
// and the badge of the endpoint handle it was sent through, so a server sharing
// one endpoint among several clients can tell them apart.

#![allow(dead_code)]

use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};

use super::domain::Domain;
use super::handle::Capability;
use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};
use crate::sched;
use crate::sync::SpinLock;

// Default bounded queue depth per endpoint. A full queue makes send report Full,
// the backpressure signal - a message is never silently dropped. A creator that
// knows its traffic picks its own depth (create_with_depth); this is the default.
const CHANNEL_QUEUE_DEFAULT: usize = 64;
// The deepest queue a creator may ask for: bounds the kernel memory one channel
// can pin through queued messages.
const CHANNEL_QUEUE_MAX: usize = 4096;

// Outcome of a non-blocking channel operation that did not complete.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChannelError {
	// The peer endpoint is gone; no further progress is possible.
	PeerClosed,
	// The peer's queue is at its limit (send): back off and retry.
	Full,
	// No message is waiting (recv) while the peer is still open: retry later.
	Empty,
}

// The next message identity. Monotonic and global rather than per-channel: a receiver looks at the
// head of one queue and then takes from that same queue, so per-channel would be enough - but an id
// that is unique across the system cannot be confused with one from anywhere else, and the counter
// costs one relaxed increment per send. It starts at 1 so that 0 is never a real message.
static NEXT_MESSAGE_ID: AtomicU64 = AtomicU64::new(1);

// A unit of IPC: a byte payload, transferred capabilities, and a sender badge.
pub struct Message {
	// What makes "take THIS message" expressible.
	//
	// A receiver has to know a message's shape before it can take it - how many bytes it must have
	// room for, how many handle slots it must reserve - and it cannot learn that without looking
	// first. Between the looking and the taking, a second receiver on the same endpoint may take
	// the message that was looked at, so what the first receiver then gets is a DIFFERENT message
	// it never inspected. `recv_identified` closes that by naming the message it agreed to.
	pub id: u64,
	pub bytes: Vec<u8>,
	pub caps: Vec<Capability>,
	pub badge: u64,
	// The sender's Domain charged for this message's queued bytes (and the amount),
	// refunded when the message is taken (recv) or dropped (channel close). None when
	// the send was not accounted (internal kernel IPC).
	queue_charge: Option<(Arc<Domain>, u64)>,
}

impl Message {
	pub fn new(bytes: Vec<u8>, caps: Vec<Capability>, badge: u64) -> Self {
		Self { id: NEXT_MESSAGE_ID.fetch_add(1, Ordering::Relaxed), bytes, caps, badge, queue_charge: None }
	}

	// Charge this message's byte length to `domain`'s in-transit IPC quota, to be
	// held until the message is taken or dropped. Returns false (charging nothing)
	// if `domain` is at its queue cap - the backpressure signal.
	fn charge_queue(&mut self, domain: &Arc<Domain>) -> bool {
		let bytes = self.bytes.len() as u64;
		if !domain.try_charge_ipc_queue(bytes) {
			return false;
		}
		self.queue_charge = Some((domain.clone(), bytes));
		true
	}

	// Release the queued-bytes charge: delivery is committed, or the message is being destroyed.
	//
	// Public because the commit point is the SYSCALL's, not this module's: the message is off the
	// queue and in hand well before the caller knows whether it can take it, and a message that goes
	// back to the head must go back still accounted for.
	pub fn release_queue_charge(&mut self) {
		self.take_queue_charge();
	}

	// Refund and clear any queued-bytes charge. Called when the message leaves the
	// queue: by recv on the way out, or when a closing endpoint drops its inbox.
	fn take_queue_charge(&mut self) {
		if let Some((domain, bytes)) = self.queue_charge.take() {
			domain.uncharge_ipc_queue(bytes);
		}
	}
}

pub struct Channel {
	header: ObjectHeader,
	// Messages waiting to be read at this endpoint (the peer pushes here).
	inbox: SpinLock<VecDeque<Message>>,
	// The bounded depth of this endpoint's inbox: a send to it reports Full at
	// this many queued messages.
	limit: usize,
	// The peer endpoint, held weakly so the two ends do not form a reference
	// cycle. Upgrading fails once the peer has been dropped (its handles closed).
	peer: SpinLock<Option<Weak<Channel>>>,
}

impl Channel {
	// Create a connected pair of endpoints with the default queue depth.
	pub fn create() -> (Arc<Channel>, Arc<Channel>) {
		Self::create_with_depth(0)
	}

	// Create a connected pair whose endpoints queue up to `depth` messages each
	// (0 = the default depth; anything else is clamped to the sane band).
	pub fn create_with_depth(depth: usize) -> (Arc<Channel>, Arc<Channel>) {
		let limit = if depth == 0 { CHANNEL_QUEUE_DEFAULT } else { depth.clamp(1, CHANNEL_QUEUE_MAX) };
		let a = Arc::new(Channel { header: ObjectHeader::new(), inbox: SpinLock::new(VecDeque::new()), limit, peer: SpinLock::new(None) });
		let b = Arc::new(Channel { header: ObjectHeader::new(), inbox: SpinLock::new(VecDeque::new()), limit, peer: SpinLock::new(None) });
		*a.peer.lock() = Some(Arc::downgrade(&b));
		*b.peer.lock() = Some(Arc::downgrade(&a));
		(a, b)
	}

	fn peer(&self) -> Option<Arc<Channel>> {
		self.peer.lock().as_ref().and_then(|w| w.upgrade())
	}

	// True once the peer endpoint has been closed.
	pub fn is_peer_closed(&self) -> bool {
		self.peer().is_none()
	}

	// True if a recv on this endpoint would not block: a message is queued, or the
	// peer has closed (recv then reports PeerClosed). The readiness `wait` tests.
	pub fn is_readable(&self) -> bool {
		!self.inbox.lock().is_empty() || self.is_peer_closed()
	}

	// True if a send through this endpoint would not report Full: the peer's queue
	// has room. A gone peer also counts as ready, so a sender blocked for room
	// wakes and observes PeerClosed from the send instead of waiting forever. The
	// readiness a WAIT_WRITABLE `wait` tests - the sender's half of backpressure.
	pub fn is_writable(&self) -> bool {
		match self.peer() {
			Some(peer) => {
				let len = peer.inbox.lock().len();
				len < peer.limit
			}
			None => true,
		}
	}

	// Deliver a message to the peer's inbox. Non-blocking: Full if the peer's
	// queue is at its limit, PeerClosed if the peer is gone. Internal kernel IPC;
	// the queued bytes are not charged to any Domain.
	pub fn send(&self, msg: Message) -> Result<(), ChannelError> {
		self.send_inner(msg, None).map_err(|(err, _)| err)
	}

	// Like `send`, but charge the queued bytes to `sender`'s in-transit IPC quota
	// (refunded when the message is received or the channel closes). Returns Full -
	// the backpressure signal - if `sender` is at its queue cap.
	pub fn send_charged(&self, msg: Message, sender: &Arc<Domain>) -> Result<(), ChannelError> {
		self.send_inner(msg, Some(sender)).map_err(|(err, _)| err)
	}

	// The same, returning the capabilities the message carried when it could not be sent.
	//
	// A send that fails takes the message with it, and with the message the capabilities
	// it was carrying - which the sender had already given up. That is a capability
	// LOST: not duplicated, not leaked, gone, with the sender told only "would block".
	// A caller doing a real transfer needs them back to put them where they were.
	pub fn send_charged_or_return(&self, msg: Message, sender: &Arc<Domain>) -> Result<(), (ChannelError, Vec<Capability>)> {
		self.send_inner(msg, Some(sender))
	}

	fn send_inner(&self, mut msg: Message, sender: Option<&Arc<Domain>>) -> Result<(), (ChannelError, Vec<Capability>)> {
		let Some(peer) = self.peer() else {
			return Err((ChannelError::PeerClosed, msg.caps));
		};
		{
			let mut inbox = peer.inbox.lock();
			if inbox.len() >= peer.limit {
				return Err((ChannelError::Full, msg.caps));
			}
			// Charge only once space is assured, so a refused message charges nothing.
			if let Some(domain) = sender {
				if !msg.charge_queue(domain) {
					return Err((ChannelError::Full, msg.caps));
				}
			}
			inbox.push_back(msg);
		}
		// The peer endpoint is now readable: wake any thread blocked waiting on it.
		sched::wake_object(peer.header.koid());
		Ok(())
	}

	// Take the next message from this endpoint's inbox. Non-blocking: Empty if
	// nothing is queued (peer still open), PeerClosed once the peer is gone and
	// the inbox has drained. Queued messages are always delivered first.
	pub fn recv(&self) -> Result<Message, ChannelError> {
		let popped = {
			let mut inbox = self.inbox.lock();
			let was_full = inbox.len() >= self.limit;
			inbox.pop_front().map(|msg| (msg, was_full))
		};
		if let Some((msg, was_full)) = popped {
			// The charge STAYS with the message until delivery commits.
			//
			// It used to be refunded here, on the way out of the queue, and a receive that then
			// failed its copy to userspace put the message back through `return_to_head` - uncharged,
			// and past the limit. One receiver at a time that is "one over for an instant"; nothing
			// serialises these, so N receivers failing their copies concurrently push N messages past
			// the limit with none of them accounted. The refund belongs at the point of no return,
			// which is where the caller now performs it.
			// The queue just left its full state: the peer endpoint is writable again,
			// so wake any sender blocked (WAIT_WRITABLE) waiting for room.
			if was_full {
				if let Some(peer) = self.peer() {
					sched::wake_object(peer.header.koid());
				}
			}
			return Ok(msg);
		}
		if self.is_peer_closed() { Err(ChannelError::PeerClosed) } else { Err(ChannelError::Empty) }
	}

	// Take the head of the queue if it is still the message `id` names AND it fits.
	//
	// `peek_shape` then `recv` is two operations under two separate locks, and a second receiver on
	// the same endpoint can take the peeked message in between. What arrives is then a DIFFERENT
	// message, and the caller has already decided what it can hold: the copy afterwards uses the
	// received length, so a receiver that declared a hundred bytes could be handed a megabyte and
	// the kernel would write all of it into a buffer it validated for a hundred. That is a
	// kernel-to-userspace overrun reachable from ring 3 with two threads and no special timing.
	//
	// The reservation had the same shape: capabilities were counted from the peeked message and
	// installed from the received one, so a receive could install more handles than the quota it
	// paid for - and when the recv then failed, the reservation was never given back.
	//
	// One lock, one decision, one dequeue. A message that does not fit is left where it is and the
	// caller is told what it would have needed, which is the same answer `peek_shape` was giving
	// and now cannot go stale between the asking and the taking.
	//
	// `recv_if_fits` - what this replaces - closed the race that mattered and left one open. It
	// refused a message that did not fit and took whatever was at the head otherwise, so a caller
	// that had reserved room for the message it PEEKED could still be handed a different one, as
	// long as that one also fitted. For the byte length that was harmless (the receiver's buffer
	// was big enough either way); for the capability count it was not, because the reservation is
	// exact and a message carrying fewer caps than reserved leaves quota held for nothing, while
	// one carrying more cannot be installed at all.
	//
	// Naming the message removes the class rather than another instance of it: the caller inspects
	// a message, decides about THAT message, and either gets it or is told it is gone.
	// Put a taken message back at the head of the queue, because it could not be DELIVERED.
	//
	// P02M0121 made a receive transactional in its handle resources: peek, reserve, take the same
	// message or none. P02M0119 then opened a second boundary behind it - the queue against the
	// caller's memory - and that one was not transactional at all: the message came off the queue,
	// the copy into userspace was allowed to be short, and the syscall reported the length it had
	// been asked for. The message was gone and the caller had part of it and no way to know.
	//
	// A short copy there means the caller unmapped its own buffer, which is the caller's doing and
	// not a reason to destroy a message somebody else sent. So it goes back where it was: at the
	// FRONT, because it was the front, which restores the order exactly unless another receiver has
	// taken the new head in between - and that is a receiver making progress, not a reordering this
	// endpoint promised not to have.
	//
	// The message goes back UNCHARGED. `recv_identified` refunded the sender's queued-bytes quota on
	// the way out and the domain that paid it is no longer reachable from here, so re-taking the
	// charge would mean threading the sender through a path that exists only for a caller that broke
	// its own buffer. The cost is an accounting entry one message light until this one is read; the
	// alternative is destroying a message the sender was told was delivered.
	//
	// It goes back CHARGED, which is what makes the count honest: the queued-bytes charge is no
	// longer refunded on the way out of the queue, so a message returned here was never unaccounted
	// and the domain's `ipc_queue` figure never understated what is outstanding.
	pub fn return_to_head(&self, message: Message) {
		self.inbox.lock().push_front(message);
		sched::wake_object(self.header.koid());
	}

	pub fn recv_identified(&self, id: u64, bytes_cap: usize, cap_slots: usize) -> Result<Message, RecvRefusal> {
		let popped = {
			let mut inbox = self.inbox.lock();
			match inbox.front() {
				// Somebody else took it, or it was never the head. Not an error: the caller peeks
				// again and decides about whatever is there now.
				Some(msg) if msg.id != id => return Err(RecvRefusal::Superseded),
				Some(msg) if msg.bytes.len() > bytes_cap => return Err(RecvRefusal::TooLarge(msg.bytes.len())),
				Some(msg) if msg.caps.len() > cap_slots => return Err(RecvRefusal::TooManyCaps(msg.caps.len())),
				Some(_) => {
					let was_full = inbox.len() >= self.limit;
					inbox.pop_front().map(|msg| (msg, was_full))
				}
				None => None,
			}
		};
		if let Some((msg, was_full)) = popped {
			// As above: the charge travels with the message and is released when delivery commits.
			if was_full {
				if let Some(peer) = self.peer() {
					sched::wake_object(peer.header.koid());
				}
			}
			return Ok(msg);
		}
		if self.is_peer_closed() { Err(RecvRefusal::Gone(ChannelError::PeerClosed)) } else { Err(RecvRefusal::Gone(ChannelError::Empty)) }
	}

	// The byte length of the next pending message without dequeuing it, so a
	// receiver can size its buffer exactly before the recv.
	pub fn peek_len(&self) -> Result<usize, ChannelError> {
		self.peek_shape().map(|(bytes, _)| bytes)
	}

	// The next message's payload size AND capability count, without taking it.
	//
	// A receive that dequeues first and only then discovers it cannot deliver has already
	// destroyed the message: a buffer one byte too small, or a handle table with no room
	// for the capabilities, and the message is gone with an error returned. A caller
	// cannot retry what no longer exists. Both numbers have to be knowable BEFORE the
	// message leaves the queue.
	pub fn peek_shape(&self) -> Result<(usize, usize), ChannelError> {
		self.peek_identified().map(|(_, bytes, caps)| (bytes, caps))
	}

	// The same, plus the identity that lets a caller act on the message it just looked at.
	pub fn peek_identified(&self) -> Result<(u64, usize, usize), ChannelError> {
		if let Some(msg) = self.inbox.lock().front() {
			return Ok((msg.id, msg.bytes.len(), msg.caps.len()));
		}
		if self.is_peer_closed() { Err(ChannelError::PeerClosed) } else { Err(ChannelError::Empty) }
	}
}

// Why `recv_identified` did not take the message. `TooLarge` and `TooManyCaps` carry what the head
// of the queue actually needs, so a caller can size a second attempt - and the message is still
// there to be taken, which is what makes the answer worth carrying.
pub enum RecvRefusal {
	TooLarge(usize),
	TooManyCaps(usize),
	// The message the caller inspected is no longer at the head: another receiver on this endpoint
	// took it. Nothing is wrong and nothing was destroyed - look again.
	Superseded,
	Gone(ChannelError),
}

impl_kernel_object!(Channel, Channel);

impl Drop for Channel {
	fn drop(&mut self) {
		// Refund the sender's queued-bytes charge for every message left undelivered
		// in this endpoint's inbox. Drain under the lock, then refund (the refund
		// touches Domain counters, not this inbox).
		let leftover: Vec<Message> = self.inbox.lock().drain(..).collect();
		for mut msg in leftover {
			msg.take_queue_charge();
		}
		// This endpoint is closing; wake any thread blocked waiting on the peer so
		// its recv/wait observes the now-closed channel.
		if let Some(peer) = self.peer() {
			sched::wake_object(peer.header.koid());
		}
	}
}

#[cfg(test)]
mod tests;
