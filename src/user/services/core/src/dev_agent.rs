// DevAgent - the development agent and the volatile artifact registry behind it.
//
// DeviceManager starts this program when it binds the development channel device, hands it a
// bootstrap channel the way it hands one to a driver, and transfers the driver's byte channel
// over it. Raw port bytes then arrive as messages on that byte channel and complete protocol
// frames go back the same way. The agent owns everything above the wire: the session, the
// deadlines, the verification pipeline and the registry.
//
// The two channels are separate on purpose. The byte channel is the wire and carries nothing
// but wire bytes, so anything the agent said on it - even its own report - would be written
// straight out of the port as unframed noise. Reporting in belongs on the bootstrap, where
// the supervisor that started it is listening.
//
// It is a process of its own rather than code inside the driver, and that separation is the
// point rather than tidiness. The driver holds a device capability and an MMIO mapping; the
// agent holds megabytes of unverified bytes a host streamed in and the logic that decides
// what to do with them. Keeping the second out of the first means a fault in the artifact
// path cannot take the device down, the driver stays a transport with nothing to know about
// artifacts, and the registry can be given resource limits that have nothing to do with what
// a driver needs. The protocol was written behind a byte stream and a `Sink` from the start
// precisely so this move would be a rewiring rather than a rewrite.
//
// The registry itself is gated on the development boot profile - see `dev_protocol` - so an
// ordinary boot that somehow reached this program would serve the protocol and refuse every
// registry operation.

#![no_std]
#![no_main]

extern crate alloc;

mod dev_protocol;

use alloc::vec::Vec;
use rt::*;

use crate::dev_protocol::{HEADER_LEN, MAGIC, MAX_PAYLOAD, PARTIAL_FRAME_TICKS, SESSION_IDLE_TICKS, Session, Sink, VERSION};

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		let mut buf: [u8; 16] = [0u8; 16];
		let bytes: u64 = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } if handle != 0 && len >= 5 && &buf[..5] == b"BYTES" => handle,
			_ => exit(),
		};
		send_blocking(bootstrap, b"agent.dev: online (registry)", 0);
		serve(bytes)
	}
}

// The transport, from the agent's side: one channel to the driver that carries raw port
// bytes in both directions. The agent builds whole frames and hands them over as one message
// each, so the driver never has to know where a frame begins or ends.
struct ChannelSink {
	channel: u64,
	frame: Vec<u8>,
}

impl Sink for ChannelSink {
	fn send(&mut self, opcode: u8, request: u32, generation: u32, status: u16, payload: &[u8]) -> bool {
		if payload.len() > MAX_PAYLOAD {
			return false;
		}
		self.frame.clear();
		self.frame.extend_from_slice(&MAGIC.to_le_bytes());
		self.frame.push(VERSION);
		self.frame.push(opcode);
		self.frame.extend_from_slice(&request.to_le_bytes());
		self.frame.extend_from_slice(&generation.to_le_bytes());
		self.frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
		self.frame.extend_from_slice(&status.to_le_bytes());
		self.frame.extend_from_slice(payload);
		unsafe { send_blocking(self.channel, &self.frame, 0) }
	}
}

// Serve the session: take whatever the driver forwarded, hand it to the protocol, and block
// until either more arrives or one of the session's deadlines comes due.
unsafe fn serve(channel: u64) -> ! {
	unsafe {
		let mut session: Session = Session::new();
		let mut sink: ChannelSink = ChannelSink { channel, frame: Vec::with_capacity(HEADER_LEN + MAX_PAYLOAD) };
		let mut pending: Vec<u8> = Vec::with_capacity(HEADER_LEN + MAX_PAYLOAD);
		// Two of the three deadlines; the third, the open publication's, belongs to the
		// session and is read back from it. Zero means the deadline does not apply.
		let mut fragment_at: u64 = 0;
		let mut session_at: u64 = 0;
		loop {
			// Drain everything queued before waiting, so the loop never blocks with messages
			// already sitting on the channel.
			let mut arrived: bool = false;
			while channel_peek(channel) >= 0 {
				match recv_vec_blocking(channel) {
					ReceivedVec::Message { bytes, .. } => {
						pending.extend_from_slice(&bytes);
						arrived = true;
					}
					// The driver is gone, so there is no channel left to serve or answer on.
					ReceivedVec::Closed => exit(),
				}
			}
			if arrived {
				// A driver that stopped accepting replies is as good as gone for this session.
				// Drop what it left buffered rather than answer into a channel nobody drains.
				if !session.consume(&mut pending, &mut sink) {
					session.close();
					pending.clear();
				}
				// Rearm the deadlines against what the parse left behind. The fragment deadline
				// dates from when the fragment first appeared, not from the last byte of it, so
				// a host trickling one byte at a time cannot hold a frame open indefinitely.
				// The session deadline is refreshed by any traffic at all, because a host that
				// is still writing has not gone away whatever it is writing.
				let now: u64 = clock();
				if pending.is_empty() {
					fragment_at = 0;
				} else if fragment_at == 0 {
					fragment_at = now + PARTIAL_FRAME_TICKS;
				}
				session_at = if session.is_open() { now + SESSION_IDLE_TICKS } else { 0 };
				continue;
			}
			// Wait on whichever deadline comes first, and on none at all when the channel is
			// idle with no session open: nothing is then waiting to expire, so an idle
			// development instance costs no wakeups.
			let deadline: u64 = soonest(&[fragment_at, session_at, session.publication_deadline()]);
			if wait(channel, deadline) == ERR_TIMED_OUT {
				let now: u64 = clock();
				if fragment_at != 0 && now >= fragment_at {
					session.fail_partial(&mut pending, &mut sink);
					fragment_at = 0;
				}
				let publication_at: u64 = session.publication_deadline();
				if publication_at != 0 && now >= publication_at {
					session.expire_publication();
				}
				if session_at != 0 && now >= session_at {
					session.close();
					session_at = 0;
				}
			}
		}
	}
}

// The earliest deadline that applies, or zero when none does.
fn soonest(deadlines: &[u64]) -> u64 {
	let mut earliest: u64 = 0;
	for &at in deadlines {
		if at != 0 && (earliest == 0 || at < earliest) {
			earliest = at;
		}
	}
	earliest
}
