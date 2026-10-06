// AN APPLICATION'S SERIAL PORT: a grant PermissionManager mints per launch from the admin root, for one component and
// one bonded BR/EDR peer the operator gave an alias and trusted for the serial port profile.
//
// A BYTE STREAM OVER RFCOMM AND NOTHING ABOVE IT. `connect` is the operator's SPP connect - the page, the stored key,
// the SDP search, the DLC - answered when the DLC opens or with why it did not; `write` hands the DLC what its queue
// holds past the peer's credits and says how much; `read` is one stream of what the peer sends.
//
// NO BYTE IS DROPPED FOR A SLOW READER. What arrived and is not in the reader's stream yet is held, and while anything
// is held the DLC gives the peer no credits back - so the peer stops, and starts again when the reader catches up.
//
// A GRANT ENDS WITH ITS LAUNCH: the owner task's end closes it, its stream with it, and the DLC it opened.

use super::*;
use proto::system::{SerialBytes, bluetooth_serial};

// Serial grants alive at once, across every controller.
const MAX_SERIALS: usize = bt_bounds::MINTED_GRANTS;
// How long a connect may take - a page, the stored key, an SDP search and RFCOMM - before it is answered timed out.
const CONNECT_TICKS: u64 = 15 * TICKS_PER_SECOND;
// The largest piece one stream message carries.
const PIECE: usize = 1024;
// How soon a reader whose stream was full is tried again.
const RETRY_TICKS: u64 = 2;

pub(crate) struct Serial {
	pub chan: u64,
	pub owner: u64,
	at: usize,
	peer: Peer,
	// The read stream's producer, zero until it is asked for.
	reader: u64,
	// A connect waiting for the DLC: its request, and when it is answered timed out.
	connecting: Option<(Vec<u8>, u64)>,
	// This grant opened the DLC, so its end closes it.
	opened: bool,
}

// A DEFERRED CONNECT'S ANSWER, encoded by running the generated dispatch again over the result.
struct Answered(Option<Result<(), Error>>);

impl bluetooth_serial::Service for Answered {
	fn connect(&mut self) -> Result<(), Error> {
		self.0.take().unwrap_or(Err(Error::Invalid))
	}

	fn write(&mut self, _data: Vec<u8>) -> Result<u32, Error> {
		Err(Error::Invalid)
	}

	fn read(&mut self) -> Result<Vec<SerialBytes>, Error> {
		Err(Error::Invalid)
	}

	fn close(&mut self) -> Result<(), Error> {
		Err(Error::Invalid)
	}
}

fn answer(chan: u64, request: &[u8], result: Result<(), Error>) {
	let mut out = [0u8; 64];
	let mut handles = wire::Handles::new();
	let mut reply_handles = wire::Handles::new();
	if let Some(len) = bluetooth_serial::dispatch(&mut Answered(Some(result)), request, &mut handles, &mut out, &mut reply_handles) {
		send_blocking(chan, &out[..len], 0);
	}
}

// THE GRANT'S OWN CALLS, answered at once.
struct SerialView<'a> {
	stack: &'a mut Stack,
	index: usize,
}

impl bluetooth_serial::Service for SerialView<'_> {
	// Deferred: `serve_serial` answers it.
	fn connect(&mut self) -> Result<(), Error> {
		Err(Error::Invalid)
	}

	fn write(&mut self, data: Vec<u8>) -> Result<u32, Error> {
		let (at, peer) = (self.stack.serials[self.index].at, self.stack.serials[self.index].peer);
		let (handle, channel) = self.stack.spp_of(at, &peer).ok_or(Error::Closed)?;
		let rfcomm = self.stack.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()).and_then(|classic| classic.rfcomm.as_mut()).ok_or(Error::Closed)?;
		let room = rfcomm.session.room(channel);
		if room == 0 {
			return Err(Error::Again);
		}
		let take = data.len().min(room);
		let outs = rfcomm.session.write(channel, &data[..take]).ok_or(Error::Closed)?;
		self.stack.run_rfcomm(at, handle, outs);
		Ok(take as u32)
	}

	// Validated here; the stream itself is made by `serve_serial`, which owns the channel.
	fn read(&mut self) -> Result<Vec<SerialBytes>, Error> {
		if self.stack.serials[self.index].reader != 0 {
			return Err(Error::Again);
		}
		Ok(Vec::new())
	}

	fn close(&mut self) -> Result<(), Error> {
		let (at, peer) = (self.stack.serials[self.index].at, self.stack.serials[self.index].peer);
		self.stack.serials[self.index].opened = false;
		match self.stack.disconnect_profile(at, &peer, Profile::Spp) {
			Ok(()) | Err(Error::NotFound) => Ok(()),
			Err(error) => Err(error),
		}
	}
}

impl gatt::AdminView<'_> {
	pub(crate) fn mint_serial_grant(&mut self, alias: String, owner: u64) -> Result<u64, Error> {
		if owner == 0 {
			return Err(Error::Invalid);
		}
		if alias.is_empty() || self.stack.serials.len() >= MAX_SERIALS {
			close(owner);
			return Err(if alias.is_empty() { Error::Invalid } else { Error::Exhausted });
		}
		// THE PEER THE ALIAS NAMES, bonded on BR/EDR and trusted for the serial port - or nothing.
		let found = (0..self.stack.controllers.len()).find_map(|at| self.stack.records(at).into_iter().find(|record| record.alias == alias && record.radio == Radio::Classic).map(|record| (at, record)));
		let Some((at, record)) = found else {
			close(owner);
			return Err(Error::NotFound);
		};
		if !record.trusted.contains(&Profile::Spp) {
			close(owner);
			return Err(Error::Denied);
		}
		let Some(peer) = peer_from_wire(&record.peer) else {
			close(owner);
			return Err(Error::Invalid);
		};
		let Some((mine, theirs)) = channel() else {
			close(owner);
			return Err(Error::Exhausted);
		};
		self.stack.serials.push(Serial { chan: mine, owner, at, peer, reader: 0, connecting: None, opened: false });
		Ok(theirs)
	}
}

impl Stack {
	// The link and the server channel of a peer's open serial port DLC.
	pub(crate) fn spp_of(&self, at: usize, peer: &Peer) -> Option<(u16, u8)> {
		let link = self.controllers.get(at)?.link_to(peer)?;
		let rfcomm = link.classic.as_ref()?.rfcomm.as_ref()?;
		rfcomm.open.iter().find(|(_, profile)| *profile == Profile::Spp).map(|(channel, _)| (link.handle, *channel))
	}

	// ONE REQUEST ON A SERIAL GRANT.
	pub(crate) fn serve_serial(&mut self, index: usize, buf: &mut [u8]) {
		let chan = self.serials[index].chan;
		let (len, handles) = match try_recv_caps(chan, buf) {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => return,
			PolledCaps::Closed => {
				self.end_serial(index);
				return;
			}
		};
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		let request = buf[..len].to_vec();
		let Some(op_code) = request.get(..2).map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]])) else { return };
		let (at, peer) = (self.serials[index].at, self.serials[index].peer);
		match op_code {
			bluetooth_serial::OP_CONNECT => {
				if self.spp_of(at, &peer).is_some() {
					answer(chan, &request, Ok(()));
					return;
				}
				if self.serials[index].connecting.is_some() {
					answer(chan, &request, Err(Error::Again));
					return;
				}
				match self.connect_profile(at, &peer, Profile::Spp) {
					Ok(()) => {
						let grant = &mut self.serials[index];
						grant.connecting = Some((request, clock().saturating_add(CONNECT_TICKS)));
						grant.opened = true;
					}
					Err(error) => answer(chan, &request, Err(error)),
				}
			}
			bluetooth_serial::OP_READ => {
				let mut view = SerialView { stack: self, index };
				let mut handles = wire::Handles::new();
				let Some((corr, result)) = bluetooth_serial::read_open(&mut view, &request, &mut handles) else { return };
				let mut out = [0u8; 64];
				let error = match result {
					Ok(_) => match channel_with_depth(16) {
						Some((producer, consumer)) => {
							if let Some(len) = bluetooth_serial::read_reply_ok(corr, &mut out)
								&& send_caps_blocking(chan, &out[..len], &[consumer])
							{
								self.serials[index].reader = producer;
								if let Some((handle, _)) = self.spp_of(at, &peer) {
									self.serial_deliver(at, handle);
								}
								return;
							}
							close(producer);
							close(consumer);
							return;
						}
						None => Error::Exhausted,
					},
					Err(error) => error,
				};
				if let Some(len) = bluetooth_serial::read_reply_err(corr, &error, &mut out) {
					send_blocking(chan, &out[..len], 0);
				}
			}
			_ => {
				let mut out = [0u8; 64];
				let mut handles = wire::Handles::new();
				let mut reply_handles = wire::Handles::new();
				let replied = bluetooth_serial::dispatch(&mut SerialView { stack: self, index }, &request, &mut handles, &mut out, &mut reply_handles);
				if let Some(len) = replied {
					send_blocking(chan, &out[..len], 0);
				}
			}
		}
	}

	// THE PEER'S BYTES, to the reader of every grant on it: as much as its stream takes now, the rest held - and while
	// anything is held the peer gets no credits back.
	pub(crate) fn serial_deliver(&mut self, at: usize, handle: u16) {
		let Some(peer) = self.controllers[at].link(handle).map(|link| link.peer) else { return };
		let Some((_, channel)) = self.spp_of(at, &peer) else { return };
		let Some(index) = self.serials.iter().position(|grant| grant.at == at && grant.peer == peer) else { return };
		let reader = self.serials[index].reader;
		let Some(rfcomm) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()).and_then(|classic| classic.rfcomm.as_mut()) else { return };
		let Some(held) = rfcomm.held.iter_mut().find(|(held, _)| *held == channel).map(|(_, held)| held) else { return };
		let mut gone = false;
		while reader != 0 && !held.is_empty() {
			let piece: Vec<u8> = held.iter().take(PIECE).copied().collect();
			let mut frame = [0u8; PIECE + 64];
			let mut handles = wire::Handles::new();
			let Some(len) = bluetooth_serial::read_frame(0, &SerialBytes { bytes: piece.clone() }, &mut frame, &mut handles) else { break };
			match try_send_outcome(reader, &frame[..len], 0) {
				SendOutcome::Delivered => {
					held.drain(..piece.len());
				}
				SendOutcome::Failed => {
					gone = true;
					break;
				}
				_ => break,
			}
		}
		let waiting = !held.is_empty();
		let outs = rfcomm.session.pause(channel, waiting);
		self.run_rfcomm(at, handle, outs);
		if gone {
			close(reader);
			self.serials[index].reader = 0;
		}
	}

	// Whether a reader's stream was full: it is tried again soon.
	pub(crate) fn serial_deadline(&self) -> Option<u64> {
		let waiting = self.serials.iter().any(|grant| grant.reader != 0 && self.spp_of(grant.at, &grant.peer).is_some_and(|(handle, channel)| self.controllers[grant.at].link(handle).and_then(|link| link.classic.as_ref()?.rfcomm.as_ref()).is_some_and(|rfcomm| rfcomm.held.iter().any(|(held, bytes)| *held == channel && !bytes.is_empty()))));
		let connecting = self.serials.iter().filter_map(|grant| grant.connecting.as_ref().map(|(_, due)| *due)).min();
		match (waiting, connecting) {
			(true, Some(due)) => Some(due.min(clock() + RETRY_TICKS)),
			(true, None) => Some(clock() + RETRY_TICKS),
			(false, due) => due,
		}
	}

	// THE TIMERS: held bytes tried again, and connects that never finished answered timed out.
	pub(crate) fn serial_timers(&mut self) {
		let now = clock();
		for index in 0..self.serials.len() {
			let (at, peer) = (self.serials[index].at, self.serials[index].peer);
			if let Some((handle, _)) = self.spp_of(at, &peer) {
				self.serial_deliver(at, handle);
			}
			if self.serials[index].connecting.as_ref().is_some_and(|(_, due)| now >= *due)
				&& let Some((request, _)) = self.serials[index].connecting.take()
			{
				answer(self.serials[index].chan, &request, Err(Error::TimedOut));
			}
		}
	}

	// THE DLC OPENED: a connect waiting for it is answered.
	pub(crate) fn serial_opened(&mut self, at: usize, handle: u16) {
		let Some(peer) = self.controllers[at].link(handle).map(|link| link.peer) else { return };
		for grant in self.serials.iter_mut().filter(|grant| grant.at == at && grant.peer == peer) {
			if let Some((request, _)) = grant.connecting.take() {
				answer(grant.chan, &request, Ok(()));
			}
		}
	}

	// THE DLC CLOSED, OR NEVER OPENED: a connect waiting is answered with why, and the reader's stream ends.
	pub(crate) fn serial_closed(&mut self, at: usize, peer: &Peer, why: Error) {
		for grant in self.serials.iter_mut().filter(|grant| grant.at == at && grant.peer == *peer) {
			if let Some((request, _)) = grant.connecting.take() {
				answer(grant.chan, &request, Err(why.clone()));
			}
			if grant.reader != 0 {
				close(grant.reader);
				grant.reader = 0;
			}
			grant.opened = false;
		}
	}

	// THE GRANT ENDS: its launch is over, or its holder closed it. The DLC it opened closes with it.
	pub(crate) fn end_serial(&mut self, index: usize) {
		let grant = self.serials.remove(index);
		close(grant.chan);
		close(grant.owner);
		if grant.reader != 0 {
			close(grant.reader);
		}
		if grant.opened {
			let _ = self.disconnect_profile(grant.at, &grant.peer, Profile::Spp);
		}
	}
}
