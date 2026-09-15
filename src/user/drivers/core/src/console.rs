// VIRTIO-SERIAL MULTIPORT, as pure decisions: what a control message means, which queues a port
// owns, and what the driver is allowed to do about either.
//
// WHY THIS IS A MODULE AND NOT PART OF THE BINARY. Every one of these answers is a function of bytes
// the DEVICE chose - a control message arrives on a queue the host fills - and a driver that decided
// them inline could only be tested by booting one. Here the hostile cases are fixtures: a device
// announcing four thousand ports, a name longer than the buffer, a message for a port that was never
// added, a message shorter than its own header.
//
// THE QUEUE INDEX RULE IS THE SPECIFICATION'S AND IS NOT A CHOICE. Port 0 keeps queues 0 and 1, the
// CONTROL pair is 2 and 3, and port `n > 0` takes `2n + 2` and `2n + 3`. A driver that numbered them
// any other way negotiates a device that answers nothing, and the failure presents as a dead port
// rather than as a wrong index - which is the kind of mistake that costs a day to see.

/// `VIRTIO_CONSOLE_F_MULTIPORT`, which is what turns one console into a device with a control queue.
pub const FEATURE_MULTIPORT: u32 = 1 << 1;

/// The control queue pair, which exists only once MULTIPORT is negotiated.
pub const CONTROL_RECEIVE_QUEUE: u16 = 2;
pub const CONTROL_TRANSMIT_QUEUE: u16 = 3;

/// THE MOST PORTS THIS DRIVER WILL EVER TRACK, whatever the device announces.
///
/// A CONTROL QUEUE IS AN INPUT. `max_nr_ports` is a number the device writes into its own
/// configuration, and a driver that sized anything from it would let a device - or anything acting
/// as one - choose how much memory this driver allocates and how many queues it sets up. Sixteen is
/// four more than any machine this system runs on offers, and a port past it is REFUSED by name
/// rather than silently ignored.
pub const MAX_PORTS: u32 = 16;

/// The longest port name kept, in bytes. A name is a selector - "the one called
/// `org.libersystem.dev`" - and a selector nobody can type is not one.
pub const MAX_NAME: usize = 48;

/// The control message, which is four fields and eight bytes on the wire.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Control {
	pub id: u32,
	pub event: u16,
	pub value: u16,
}

/// The events, as the specification numbers them.
pub mod event {
	pub const DEVICE_READY: u16 = 0;
	pub const PORT_ADD: u16 = 1;
	pub const PORT_REMOVE: u16 = 2;
	pub const PORT_READY: u16 = 3;
	pub const CONSOLE_PORT: u16 = 4;
	pub const RESIZE: u16 = 5;
	pub const PORT_OPEN: u16 = 6;
	pub const PORT_NAME: u16 = 7;
}

impl Control {
	pub const LEN: usize = 8;

	/// One control message from the bytes a receive buffer came back with.
	///
	/// A MESSAGE SHORTER THAN ITS HEADER IS NOT A MESSAGE. The device chooses the used length, so a
	/// driver that read eight bytes out of a four-byte answer would be reading whatever the buffer
	/// held before - which on a recycled DMA slot is the previous message.
	pub fn decode(bytes: &[u8]) -> Option<Control> {
		if bytes.len() < Self::LEN {
			return None;
		}
		Some(Control { id: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]), event: u16::from_le_bytes([bytes[4], bytes[5]]), value: u16::from_le_bytes([bytes[6], bytes[7]]) })
	}

	pub fn encode(&self, out: &mut [u8]) -> Option<usize> {
		let slot = out.get_mut(..Self::LEN)?;
		slot[..4].copy_from_slice(&self.id.to_le_bytes());
		slot[4..6].copy_from_slice(&self.event.to_le_bytes());
		slot[6..8].copy_from_slice(&self.value.to_le_bytes());
		Some(Self::LEN)
	}
}

/// What the driver does about one control message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
	/// Set up this port's queues and answer `PORT_READY`.
	Add(u32),
	/// The port is open at the host end: its byte stream may be published.
	Open(u32),
	/// The host closed it: the stream stops and the publication is withdrawn.
	Close(u32),
	/// Tear the port down - its queues and its publication both.
	Remove(u32),
	/// The port's name is now recorded, and this is what a consumer selects on.
	Named(u32),
	/// This port is the console. Kept because it decides which port an emergency write goes to.
	Console(u32),
	/// Understood and requiring nothing: a resize, or a repeat of something already true.
	Settled,
	/// REFUSED BY NAME rather than ignored: a port past the cap, a message for a port that was
	/// never added, or a message that is not a message.
	Refused(Refusal),
}

/// Why a control message was refused, because "it did nothing" is not something a reader can act on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// A port index at or past `MAX_PORTS`.
	PortOutOfRange,
	/// A message about a port this driver was never told to add.
	UnknownPort,
	/// Fewer bytes than a control message has.
	Truncated,
	/// An event number the specification does not define.
	UnknownEvent,
}

/// One port's state, which is what an `Action` is computed against.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Port {
	pub added: bool,
	pub open: bool,
	pub console: bool,
	name: [u8; MAX_NAME],
	name_len: u8,
}

// WRITTEN OUT BECAUSE AN ARRAY OF FORTY-EIGHT HAS NO `Default`, and the alternative - a name that is
// a heap allocation - would put an allocation on the path a control message takes.
impl Default for Port {
	fn default() -> Self {
		Port { added: false, open: false, console: false, name: [0; MAX_NAME], name_len: 0 }
	}
}

impl Port {
	/// The name as it was given, or an empty slice for a port the host has not named.
	pub fn name(&self) -> &[u8] {
		&self.name[..self.name_len as usize]
	}
}

/// Every port this driver tracks, and the rule that decides what a message does to it.
#[derive(Clone, Copy, Debug)]
pub struct Ports {
	ports: [Port; MAX_PORTS as usize],
	/// What the DEVICE said it has, clamped to what this driver will track.
	announced: u32,
}

impl Default for Ports {
	fn default() -> Self {
		Self::new(MAX_PORTS)
	}
}

impl Ports {
	/// Track a device announcing `max_nr_ports`, clamped.
	pub fn new(announced: u32) -> Ports {
		Ports { ports: [Port::default(); MAX_PORTS as usize], announced: announced.min(MAX_PORTS) }
	}

	pub fn announced(&self) -> u32 {
		self.announced
	}

	pub fn get(&self, port: u32) -> Option<&Port> {
		self.ports.get(port as usize)
	}

	/// How many ports are open, which is how many byte streams this driver is publishing.
	pub fn open_count(&self) -> usize {
		self.ports.iter().filter(|port| port.open).count()
	}

	/// THE QUEUE PAIR A PORT OWNS. Port 0 is 0 and 1; the control pair is 2 and 3; port `n` is
	/// `2n + 2` and `2n + 3`.
	pub fn queue_pair(port: u32) -> Option<(u16, u16)> {
		if port >= MAX_PORTS {
			return None;
		}
		if port == 0 {
			return Some((0, 1));
		}
		let receive = u16::try_from(port * 2 + 2).ok()?;
		Some((receive, receive + 1))
	}

	/// What one control message means, with the state updated to match.
	///
	/// `payload` is whatever followed the header, which only `PORT_NAME` uses.
	pub fn apply(&mut self, bytes: &[u8]) -> Action {
		let Some(control) = Control::decode(bytes) else {
			return Action::Refused(Refusal::Truncated);
		};
		let payload = &bytes[Control::LEN..];
		if control.id >= self.announced || control.id >= MAX_PORTS {
			return Action::Refused(Refusal::PortOutOfRange);
		}
		let index = control.id as usize;
		match control.event {
			event::PORT_ADD => {
				if self.ports[index].added {
					return Action::Settled;
				}
				self.ports[index].added = true;
				Action::Add(control.id)
			}
			event::PORT_REMOVE => {
				if !self.ports[index].added {
					return Action::Refused(Refusal::UnknownPort);
				}
				self.ports[index] = Port::default();
				Action::Remove(control.id)
			}
			event::PORT_OPEN => {
				if !self.ports[index].added {
					return Action::Refused(Refusal::UnknownPort);
				}
				let open = control.value != 0;
				if self.ports[index].open == open {
					return Action::Settled;
				}
				self.ports[index].open = open;
				if open { Action::Open(control.id) } else { Action::Close(control.id) }
			}
			event::CONSOLE_PORT => {
				if !self.ports[index].added {
					return Action::Refused(Refusal::UnknownPort);
				}
				self.ports[index].console = true;
				Action::Console(control.id)
			}
			event::PORT_NAME => {
				if !self.ports[index].added {
					return Action::Refused(Refusal::UnknownPort);
				}
				// THE NAME IS TRUNCATED AND KEPT, not refused: a device that names a port with two
				// hundred bytes has given a long name rather than a hostile one, and the first
				// forty-eight are what a consumer selects on. A trailing NUL is not part of it.
				let end = payload.iter().position(|byte| *byte == 0).unwrap_or(payload.len());
				let keep = end.min(MAX_NAME);
				self.ports[index].name[..keep].copy_from_slice(&payload[..keep]);
				self.ports[index].name_len = keep as u8;
				Action::Named(control.id)
			}
			event::RESIZE | event::DEVICE_READY | event::PORT_READY => Action::Settled,
			_ => Action::Refused(Refusal::UnknownEvent),
		}
	}
}

#[cfg(test)]
#[path = "console/tests.rs"]
mod tests;
