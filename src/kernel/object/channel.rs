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

use super::domain::Domain;
use super::handle::Capability;
use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};
use crate::sched;
use crate::sync::SpinLock;

// Bounded queue depth per endpoint. A full queue makes send report Full, the
// backpressure signal - a message is never silently dropped.
const CHANNEL_QUEUE_LIMIT: usize = 64;

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

// A unit of IPC: a byte payload, transferred capabilities, and a sender badge.
pub struct Message {
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
		Self { bytes, caps, badge, queue_charge: None }
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
	// The peer endpoint, held weakly so the two ends do not form a reference
	// cycle. Upgrading fails once the peer has been dropped (its handles closed).
	peer: SpinLock<Option<Weak<Channel>>>,
}

impl Channel {
	// Create a connected pair of endpoints.
	pub fn create() -> (Arc<Channel>, Arc<Channel>) {
		let a = Arc::new(Channel { header: ObjectHeader::new(), inbox: SpinLock::new(VecDeque::new()), peer: SpinLock::new(None) });
		let b = Arc::new(Channel { header: ObjectHeader::new(), inbox: SpinLock::new(VecDeque::new()), peer: SpinLock::new(None) });
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

	// Deliver a message to the peer's inbox. Non-blocking: Full if the peer's
	// queue is at its limit, PeerClosed if the peer is gone. Internal kernel IPC;
	// the queued bytes are not charged to any Domain.
	pub fn send(&self, msg: Message) -> Result<(), ChannelError> {
		self.send_inner(msg, None)
	}

	// Like `send`, but charge the queued bytes to `sender`'s in-transit IPC quota
	// (refunded when the message is received or the channel closes). Returns Full -
	// the backpressure signal - if `sender` is at its queue cap.
	pub fn send_charged(&self, msg: Message, sender: &Arc<Domain>) -> Result<(), ChannelError> {
		self.send_inner(msg, Some(sender))
	}

	fn send_inner(&self, mut msg: Message, sender: Option<&Arc<Domain>>) -> Result<(), ChannelError> {
		let peer = self.peer().ok_or(ChannelError::PeerClosed)?;
		{
			let mut inbox = peer.inbox.lock();
			if inbox.len() >= CHANNEL_QUEUE_LIMIT {
				return Err(ChannelError::Full);
			}
			// Charge only once space is assured, so a refused message charges nothing.
			if let Some(domain) = sender {
				if !msg.charge_queue(domain) {
					return Err(ChannelError::Full);
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
		if let Some(mut msg) = self.inbox.lock().pop_front() {
			// The message has left the queue: refund the sender's queued-bytes charge.
			msg.take_queue_charge();
			return Ok(msg);
		}
		if self.is_peer_closed() { Err(ChannelError::PeerClosed) } else { Err(ChannelError::Empty) }
	}

	// The byte length of the next pending message without dequeuing it, so a
	// receiver can size its buffer exactly before the recv.
	pub fn peek_len(&self) -> Result<usize, ChannelError> {
		if let Some(msg) = self.inbox.lock().front() {
			return Ok(msg.bytes.len());
		}
		if self.is_peer_closed() { Err(ChannelError::PeerClosed) } else { Err(ChannelError::Empty) }
	}
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
