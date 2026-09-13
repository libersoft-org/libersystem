// DevAgent - the development agent and the volatile artifact registry behind it.
//
// DeviceManager starts this program in a development image, hands it a bootstrap channel the way it
// hands one to a driver, and transfers a PROVIDER-CATALOGUE connection over it. This program finds
// its own wire through that catalogue: it subscribes to the `console-bytes` kind, opens a connection
// to a provider that appears, settles a version with it, and asks for the receive stream. Bytes then
// arrive as `console-chunk` frames and whole protocol frames go back as `write` calls. The agent owns
// everything above the wire: the session, the deadlines, the verification pipeline and the registry.
//
// IT USED TO BE HANDED THE WIRE, AND THAT IS WHAT CHANGED (2026-09-13). DeviceManager took the byte
// channel out of the catalogue when the binding reported, recognised its consumer by the DRIVER'S
// NAME, and transferred it under a `BYTES` tag; a replacement agent was re-wired by the manager
// sending the same tag down to the driver. So the one provider this machine publishes was the only
// one whose consumer was chosen by a string, the catalogue could show it to nobody else, and a
// provider that arrived late or came back after a rebind had no path to this program at all. What
// replaces it is what every other consumer in this tree already does: subscribe, hold the identity
// the manager minted, open, and reattach when a publication says the one being used has gone.
//
// The two channels are separate on purpose. The wire carries nothing but wire bytes, so anything
// the agent said on it - even its own report - would be written straight out of the port as
// unframed noise. Reporting in belongs on the bootstrap, where the supervisor that started it is
// listening.
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
use ipc_client::ChannelTransport;
use proto::codec::Handles;
use proto::system::{Error, ProviderInfo, ProviderKind, console_stream, provider_catalogue};
use rt::*;

use crate::dev_protocol::{HEADER_LEN, MAGIC, MAX_PAYLOAD, PARTIAL_FRAME_TICKS, SESSION_IDLE_TICKS, Session, Sink, VERSION};

// The version of `liber:device@1`'s `console-stream` this agent speaks. A provider that answers
// `attach` with a refusal is not this agent's wire, whatever kind it publishes under: the whole
// point of asking first is that a byte stream cannot report a framing disagreement later.
const WIRE_VERSION: u32 = 1;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 32] = [0u8; 32];
	// The catalogue, and with it the boot's identity. The identity travels in the same message
	// because it is not this program's to draw: this program can be replaced without the guest
	// restarting, and a value drawn here would tell every tool the guest had rebooted each time
	// its agent did.
	let (catalogue, nonce): (u64, [u8; 8]) = match recv_blocking(bootstrap, &mut buf) {
		Received::Message { len, handle } if handle != 0 && len >= 11 && &buf[..3] == b"CAT" => (handle, buf[3..11].try_into().unwrap_or([0u8; 8])),
		_ => exit(),
	};
	// The volume client the installed artifacts are read through, so a published
	// generation can be judged against the image it would shadow rather than against
	// whatever was published before it. Optional: without it every verdict is unknown,
	// which is honest, where assuming compatibility would not be.
	let storage: u64 = recv_tagged(bootstrap, &mut buf, b"STORAGE").unwrap_or(0);
	// The ConsoleInputSource capability, last in the sequence. Without it this agent can
	// still drive everything else and simply cannot type into the console.
	crate::dev_protocol::CONSOLE_INPUT.store(recv_tagged(bootstrap, &mut buf, b"CONSOLE").unwrap_or(0), core::sync::atomic::Ordering::Relaxed);
	send_blocking(bootstrap, b"agent.dev: online (registry)", 0);
	serve(catalogue, bootstrap, storage, nonce)
}

// THE WIRE, FOUND RATHER THAN GIVEN: one console-bytes provider, reached through the catalogue and
// held under the identity the manager minted for it.
//
// Every field here is part of one attachment and they go together. A provider that is withdrawn
// takes its control connection, its stream and its bounds with it, and the next one brings its own:
// keeping any of them across the gap is how a consumer comes to write into a channel whose device
// is no longer there.
struct Wire {
	// The catalogue connection. Connections to providers are minted from it, so it outlives every
	// attachment made through it.
	catalogue: u64,
	// The subscription: the snapshot and the live stream of `console-bytes` publications, in one
	// channel. Zero when the catalogue refused one, which is a machine whose development channel
	// cannot be followed - said once, rather than retried into a loop.
	subscription: u64,
	// WHICH PUBLICATION THIS AGENT IS USING. `slot`, `provider-generation` and the binding's own
	// generation together, because a withdrawal is described by identity after its handle is gone -
	// and a reused slot must not be mistaken for the provider that left it.
	provider: Option<ProviderInfo>,
	// The `console-stream` connection, and the receive endpoint it granted.
	control: u64,
	stream: u64,
	// The largest single `write` this provider accepts, as it answered. A frame longer than this is
	// written in several calls: a byte stream has no frame boundaries, which is the one freedom it
	// does have.
	max_frame: usize,
}

impl Wire {
	// Subscribe, and take whatever the snapshot already holds.
	//
	// Zero is not a failure. A boot that granted no catalogue connection, or a catalogue that
	// refuses the subscription, is a development instance whose channel cannot be followed - which
	// is reported once and then served as a machine that has no development device at all.
	fn new(catalogue: u64) -> Wire {
		let subscription: u64 = if catalogue == 0 {
			print(b"agent.dev: no provider catalogue - this instance has no wire to find\n");
			0
		} else {
			let opened: u64 = provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::ConsoleBytes).unwrap_or(0);
			if opened == 0 {
				print(b"agent.dev: the catalogue refused a console-bytes subscription\n");
			}
			opened
		};
		Wire { catalogue, subscription, provider: None, control: 0, stream: 0, max_frame: 0 }
	}

	// Whether a publication names the provider this agent is attached to. A withdrawal arrives
	// after its handle is gone, so identity is the only thing left to recognise it by - and the
	// manager's own three numbers are what make a reused slot unmistakable for its predecessor.
	fn is_current(&self, info: &ProviderInfo) -> bool {
		self.provider.as_ref().is_some_and(|held| held.slot == info.slot && held.provider_generation == info.provider_generation && held.binding_generation == info.binding_generation)
	}

	// Let go of the provider being used. Everything the attachment held goes with it, because the
	// next provider brings its own bounds and its own stream.
	fn detach(&mut self) {
		if self.stream != 0 {
			close(self.stream);
			self.stream = 0;
		}
		if self.control != 0 {
			close(self.control);
			self.control = 0;
		}
		self.provider = None;
		self.max_frame = 0;
	}

	// Open one published provider, settle a version with it, and ask for its receive stream.
	//
	// THE HANDSHAKE IS WHAT SELECTS, not the publication. Any driver may publish this kind - a
	// virtio-serial multiport port is the candidate second one - and a provider that will not speak
	// this contract at this version is not this agent's wire, however it was published. Refusing
	// here costs one connection; guessing costs a stream neither end can parse.
	fn attach_to(&mut self, info: &ProviderInfo) -> bool {
		if self.catalogue == 0 || self.control != 0 || !info.live {
			return false;
		}
		let control: u64 = match provider_catalogue::Client::new(ChannelTransport { chan: self.catalogue }).open(info) {
			Some(Ok(handle)) => handle,
			// A REFUSAL IS SAID. This kind admits one consumer, so a second ask is refused - and an
			// agent that cannot tell that from "no device" cannot report either.
			Some(Err(_)) => {
				print(b"agent.dev: the catalogue refused a connection to the console-bytes provider it published\n");
				return false;
			}
			None => {
				print(b"agent.dev: the catalogue did not answer the connection it published\n");
				return false;
			}
		};
		let max_frame: usize = match console_stream::Client::new(ChannelTransport { chan: control }).attach(&WIRE_VERSION) {
			Some(Ok(attachment)) if attachment.version == WIRE_VERSION && attachment.max_frame > 0 => attachment.max_frame as usize,
			_ => {
				print(b"agent.dev: a console-bytes provider would not speak this wire version; it is not this agent's channel\n");
				close(control);
				return false;
			}
		};
		let stream: u64 = match console_stream::Client::new(ChannelTransport { chan: control }).receive() {
			Some(Ok(handle)) => handle,
			_ => {
				print(b"agent.dev: a console-bytes provider granted no receive stream\n");
				close(control);
				return false;
			}
		};
		self.control = control;
		self.stream = stream;
		self.max_frame = max_frame;
		self.provider = Some(info.clone());
		true
	}

	// Read every publication the subscription has queued, without blocking.
	//
	// This is attach, detach and failover in one place, which is the point: a provider that appears
	// while none is held is attached to, and a withdrawal that names the one being held drops it -
	// so a driver that rebinds is followed rather than mourned, and a session that was open when it
	// went is ended rather than left writing into a closed channel.
	fn poll_catalogue(&mut self, buf: &mut [u8]) -> bool {
		let mut lost: bool = false;
		while self.subscription != 0 {
			let PolledCaps::Message { len, handles } = try_recv_caps(self.subscription, buf) else {
				break;
			};
			// A publication frame carries no capability. Anything that arrives with one is closed
			// rather than dropped, so it does not stay charged to whoever sent it.
			for &handle in handles.as_slice() {
				close(handle);
			}
			let mut frame_handles: Handles = Handles::new();
			let Some(info) = provider_catalogue::subscribe_read(&buf[..len], &mut frame_handles) else {
				print(b"agent.dev: a provider frame did not decode\n");
				continue;
			};
			if info.kind != ProviderKind::ConsoleBytes {
				continue;
			}
			if !info.live {
				if self.is_current(&info) {
					self.detach();
					lost = true;
				}
				continue;
			}
			if self.control == 0 {
				self.attach_to(&info);
			}
		}
		lost
	}

	// Write a whole frame to the port, in as many calls as its bound needs.
	//
	// A BYTE STREAM HAS NO FRAME BOUNDARIES, which is the one freedom it has: a protocol frame
	// longer than what this provider accepts in one call is written in several, and the host
	// reassembles it the same way it reassembles everything else.
	//
	// `again` is the host having stopped reading, and it ends the SESSION rather than the
	// attachment: the port is fine, the tool at the other end is not. Any other refusal is the
	// provider itself, so the attachment goes and the subscription finds the next one.
	fn write_all(&mut self, bytes: &[u8]) -> bool {
		let mut at: usize = 0;
		while at < bytes.len() {
			if self.control == 0 || self.max_frame == 0 {
				return false;
			}
			let end: usize = (at + self.max_frame).min(bytes.len());
			match console_stream::Client::new(ChannelTransport { chan: self.control }).write(&bytes[at..end]) {
				Some(Ok(taken)) if taken as usize == end - at => at = end,
				// A PARTIAL WRITE IS NOT A WRITE. The contract answers with what the port took, and
				// a provider that took some of a frame has left the stream in a state no reader can
				// recover: the remainder would be read as the beginning of the next frame.
				Some(Ok(_)) => {
					self.detach();
					return false;
				}
				Some(Err(Error::Again)) => return false,
				_ => {
					self.detach();
					return false;
				}
			}
		}
		true
	}
}

// The transport, from the agent's side. The agent builds whole frames and hands them over as one
// call each, so nothing below it has to know where a frame begins or ends.
struct WireSink<'a> {
	wire: &'a mut Wire,
	frame: &'a mut Vec<u8>,
}

impl Sink for WireSink<'_> {
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
		let frame: Vec<u8> = core::mem::take(self.frame);
		let sent: bool = self.wire.write_all(&frame);
		*self.frame = frame;
		sent
	}
}

// Serve the session: find a wire, take whatever it delivers, hand it to the protocol, and block
// until either more arrives or one of the session's deadlines comes due.
fn serve(catalogue: u64, bootstrap: u64, storage: u64, nonce: [u8; 8]) -> ! {
	let mut wire: Wire = Wire::new(catalogue);
	let mut session: Session = Session::new(storage, nonce);
	let mut frame: Vec<u8> = Vec::with_capacity(HEADER_LEN + MAX_PAYLOAD);
	let mut pending: Vec<u8> = Vec::with_capacity(HEADER_LEN + MAX_PAYLOAD);
	// Big enough for one publication frame, which is a fixed-size record and nothing more.
	let mut published: Vec<u8> = alloc::vec![0u8; 256];
	// Two of the three deadlines; the third, the open publication's, belongs to the
	// session and is read back from it. Zero means the deadline does not apply.
	let mut fragment_at: u64 = 0;
	let mut session_at: u64 = 0;
	loop {
		// ATTACH AND DETACH BEFORE READING, every pass. A provider that arrived while this program
		// was parked is the wire for this pass, and one that was withdrawn must not be read from at
		// all - both are decided here rather than discovered as a failed receive.
		if wire.poll_catalogue(&mut published) {
			// The provider being used has gone. The bytes in flight belonged to the session it
			// carried, and half a frame handed to the next attachment would be read as the
			// beginning of one.
			session.close();
			pending.clear();
			fragment_at = 0;
			session_at = 0;
		}
		// Take what is queued, but parse after every message rather than after draining
		// the queue. Parsing last would let a host that pipelines requests grow the
		// accumulator by the whole queue - the protocol's bound is on one frame, not on
		// how many a host may send before this loop runs, and the channel queue is not
		// itself capped. Parsing between messages keeps the accumulator at one incomplete
		// frame plus the message that is being folded into it, whatever a host does.
		let mut arrived: bool = false;
		while wire.stream != 0 && channel_peek(wire.stream) >= 0 {
			match recv_vec_blocking(wire.stream) {
				ReceivedVec::Message { bytes, .. } => {
					let mut frame_handles: Handles = Handles::new();
					match console_stream::receive_read(&bytes, &mut frame_handles) {
						Some(chunk) => pending.extend_from_slice(&chunk.bytes),
						// A frame on this stream that does not decode is not a short read: the
						// endpoint is not carrying what it said it would, and nothing further on
						// it can be trusted to be a chunk boundary.
						None => {
							print(b"agent.dev: a console chunk did not decode; the attachment ends\n");
							wire.detach();
							session.close();
							pending.clear();
							break;
						}
					}
				}
				// The provider is gone. The session goes with it and the subscription finds the
				// next one; this used to end the whole process, because there was no next one to
				// find.
				ReceivedVec::Closed | ReceivedVec::Failed => {
					wire.detach();
					session.close();
					pending.clear();
					break;
				}
			}
			arrived = true;
			// A provider that stopped accepting writes is as good as gone for this session.
			// Drop what it left buffered rather than answer into a channel nobody drains.
			{
				let mut sink: WireSink = WireSink { wire: &mut wire, frame: &mut frame };
				if !session.consume(&mut pending, &mut sink) {
					session.close();
					pending.clear();
				}
			}
			// A host asked for a fresh agent. The acknowledgement is already queued in the
			// provider's channel, and a queued message outlives the sender, so leaving now is
			// what the host asked for rather than a reply it never gets. DeviceManager holds
			// this bootstrap and starts the replacement when it closes.
			if session.restart_requested() {
				exit();
			}
		}
		if arrived {
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
		// A launched program writes at its own pace, so its output channel is waited on
		// alongside the wire rather than polled; draining before the wait keeps the loop
		// from blocking with output already queued.
		session.drain_launch();
		// Wait on whichever deadline comes first, and on none at all when the wire is
		// idle with no session open: nothing is then waiting to expire, so an idle
		// development instance costs no wakeups.
		let deadline: u64 = soonest(&[fragment_at, session_at, session.publication_deadline()]);
		// Five things can wake this: the wire, the catalogue - which is how a provider that
		// appears while nothing is attached reaches this loop at all - a launched program's
		// output, the resolution channel, and the bootstrap, which carries the launcher,
		// delivered long after this program started because PermissionManager comes up after
		// the drivers do.
		let launch: u64 = session.launch_channel();
		let registry: u64 = session.registry_channel();
		let mut watched: Vec<u64> = alloc::vec![bootstrap];
		if wire.subscription != 0 {
			watched.push(wire.subscription);
		}
		if wire.stream != 0 {
			watched.push(wire.stream);
		}
		if launch != 0 {
			watched.push(launch);
		}
		if registry != 0 {
			watched.push(registry);
		}
		let ready: i64 = wait_any(&watched, deadline);
		if ready == 0 {
			let mut buf: [u8; 16] = [0u8; 16];
			match recv_blocking(bootstrap, &mut buf) {
				Received::Message { len, handle } if handle != 0 && len >= 4 && &buf[..4] == b"PERM" => session.set_launcher(handle),
				// ProcessService's end of the resolution channel: a launch asks whether the
				// registry has a generation of an artifact before it reads the volume.
				Received::Message { len, handle } if handle != 0 && len >= 3 && &buf[..3] == b"REG" => session.set_registry(handle),
				// The supervisor dropped this program's bootstrap, which is how it is told
				// to shut down.
				Received::Closed => exit(),
				_ => {}
			}
			continue;
		}
		if ready > 0 && registry != 0 && watched.get(ready as usize) == Some(&registry) {
			session.answer_resolution();
			continue;
		}
		if ready == ERR_TIMED_OUT {
			let now: u64 = clock();
			if fragment_at != 0 && now >= fragment_at {
				let mut sink: WireSink = WireSink { wire: &mut wire, frame: &mut frame };
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
