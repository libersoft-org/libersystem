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

use super::handle::Capability;
use super::{KernelObject, ObjectHeader, ObjectType};
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
}

impl Message {
	pub fn new(bytes: Vec<u8>, caps: Vec<Capability>, badge: u64) -> Self {
		Self { bytes, caps, badge }
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
	// queue is at its limit, PeerClosed if the peer is gone.
	pub fn send(&self, msg: Message) -> Result<(), ChannelError> {
		let peer = self.peer().ok_or(ChannelError::PeerClosed)?;
		{
			let mut inbox = peer.inbox.lock();
			if inbox.len() >= CHANNEL_QUEUE_LIMIT {
				return Err(ChannelError::Full);
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
		if let Some(msg) = self.inbox.lock().pop_front() {
			return Ok(msg);
		}
		if self.is_peer_closed() {
			Err(ChannelError::PeerClosed)
		} else {
			Err(ChannelError::Empty)
		}
	}
}

impl KernelObject for Channel {
	fn header(&self) -> &ObjectHeader {
		&self.header
	}

	fn object_type(&self) -> ObjectType {
		ObjectType::Channel
	}

	fn as_any(&self) -> &dyn Any {
		self
	}
}

impl Drop for Channel {
	fn drop(&mut self) {
		// This endpoint is closing; wake any thread blocked waiting on the peer so
		// its recv/wait observes the now-closed channel.
		if let Some(peer) = self.peer() {
			sched::wake_object(peer.header.koid());
		}
	}
}
