#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use rt::{RIGHT_MAP, RIGHT_READ, RIGHT_TRANSFER, ReceivedVecCaps, close, duplicate, map_object, memory_object_create, recv_vec_caps_deadline, resolve, send_caps_blocking, unmap_object};
use wire::{Buffer, Handles, Transport, TransportError};

pub unsafe fn make_buffer(bytes: &[u8]) -> Option<Buffer> {
	unsafe {
		let object = memory_object_create(bytes.len().max(1) as u64);
		if object < 0 {
			return None;
		}
		let object = object as u64;
		let mapped = match map_object(object) {
			Some(base) => base,
			None => {
				close(object);
				return None;
			}
		};
		core::ptr::copy_nonoverlapping(bytes.as_ptr(), mapped as *mut u8, bytes.len());
		unmap_object(object);
		let granted = duplicate(object, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
		close(object);
		if granted < 0 {
			return None;
		}
		Some(Buffer { handle: granted as u64, len: bytes.len() as u64 })
	}
}

pub struct ChannelTransport {
	pub chan: u64,
}

impl Transport for ChannelTransport {
	fn call(&mut self, request: &[u8], request_handles: &[u64], reply_handles: &mut Handles, deadline: u64) -> Result<Vec<u8>, TransportError> {
		unsafe {
			if !send_caps_blocking(self.chan, request, request_handles) {
				return Err(TransportError::SendRefused);
			}
			// EACH ENDING KEPT DISTINCT. These were one `None`, so a caller could not tell a
			// departed peer (retry impossible) from a refused receive (peer still there) from a
			// deadline (the request may already have been acted on).
			match recv_vec_caps_deadline(self.chan, reply_handles, deadline) {
				ReceivedVecCaps::Message { bytes } => Ok(bytes),
				ReceivedVecCaps::Closed => Err(TransportError::PeerClosed),
				ReceivedVecCaps::Failed => Err(TransportError::ReceiveFailed),
				ReceivedVecCaps::TimedOut => Err(TransportError::TimedOut),
			}
		}
	}

	fn discard_handles(&mut self, handles: &[u64]) {
		for &handle in handles {
			if handle != 0 {
				unsafe { close(handle) };
			}
		}
	}
}

pub struct SvcTransport {
	broker: u64,
	name: &'static [u8],
	chan: u64,
}

impl SvcTransport {
	pub const fn new(broker: u64, name: &'static [u8], chan: u64) -> SvcTransport {
		SvcTransport { broker, name, chan }
	}

	pub unsafe fn channel(&mut self) -> u64 {
		if self.chan == 0 {
			self.chan = unsafe { resolve(self.broker, self.name) }.unwrap_or(0);
		}
		self.chan
	}

	pub unsafe fn reconnect(&mut self) -> bool {
		unsafe {
			if self.chan != 0 {
				close(self.chan);
				self.chan = 0;
			}
			self.channel() != 0
		}
	}
}

impl Transport for SvcTransport {
	fn call(&mut self, request: &[u8], request_handles: &[u64], reply_handles: &mut Handles, deadline: u64) -> Result<Vec<u8>, TransportError> {
		unsafe {
			let chan = self.channel();
			if chan == 0 {
				return Err(TransportError::NoRoute);
			}
			if !send_caps_blocking(chan, request, request_handles) {
				if !self.reconnect() || !send_caps_blocking(self.chan, request, request_handles) {
					return Err(TransportError::SendRefused);
				}
			}
			match recv_vec_caps_deadline(self.chan, reply_handles, deadline) {
				ReceivedVecCaps::Message { bytes } => Ok(bytes),
				ReceivedVecCaps::Failed => Err(TransportError::ReceiveFailed),
				ReceivedVecCaps::TimedOut => Err(TransportError::TimedOut),
				ReceivedVecCaps::Closed => {
					let _ = self.reconnect();
					Err(TransportError::PeerClosed)
				}
			}
		}
	}

	fn discard_handles(&mut self, handles: &[u64]) {
		for &handle in handles {
			if handle != 0 {
				unsafe { close(handle) };
			}
		}
	}
}

impl Transport for &mut SvcTransport {
	fn call(&mut self, request: &[u8], request_handles: &[u64], reply_handles: &mut Handles, deadline: u64) -> Result<Vec<u8>, TransportError> {
		(**self).call(request, request_handles, reply_handles, deadline)
	}

	fn discard_handles(&mut self, handles: &[u64]) {
		(**self).discard_handles(handles)
	}
}
