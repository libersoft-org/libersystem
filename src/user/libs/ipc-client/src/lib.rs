#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use rt::{RIGHT_MAP, RIGHT_READ, RIGHT_TRANSFER, ReceivedVec, close, duplicate, map_object, memory_object_create, recv_vec_blocking, resolve, send_blocking, unmap_object};
use wire::{Buffer, Transport};

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
	fn call(&mut self, request: &[u8], request_handle: u64) -> Option<(Vec<u8>, u64)> {
		unsafe {
			if !send_blocking(self.chan, request, request_handle) {
				return None;
			}
			match recv_vec_blocking(self.chan) {
				ReceivedVec::Message { bytes, handle } => Some((bytes, handle)),
				ReceivedVec::Closed => None,
			}
		}
	}

	fn discard_handle(&mut self, handle: u64) {
		if handle != 0 {
			unsafe { close(handle) };
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
	fn call(&mut self, request: &[u8], request_handle: u64) -> Option<(Vec<u8>, u64)> {
		unsafe {
			let chan = self.channel();
			if chan == 0 {
				return None;
			}
			if !send_blocking(chan, request, request_handle) {
				if !self.reconnect() || !send_blocking(self.chan, request, request_handle) {
					return None;
				}
			}
			match recv_vec_blocking(self.chan) {
				ReceivedVec::Message { bytes, handle } => Some((bytes, handle)),
				ReceivedVec::Closed => {
					let _ = self.reconnect();
					None
				}
			}
		}
	}

	fn discard_handle(&mut self, handle: u64) {
		if handle != 0 {
			unsafe { close(handle) };
		}
	}
}

impl Transport for &mut SvcTransport {
	fn call(&mut self, request: &[u8], request_handle: u64) -> Option<(Vec<u8>, u64)> {
		(**self).call(request, request_handle)
	}
}
