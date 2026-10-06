//! THE COMMON AUDIO PROFILE'S INITIATOR, FOR ONE DEVICE OF ONE OR MORE EARBUDS: a coordinated set - a pair found and
//! driven as one through CSIP - or a single device, streamed as one.
//!
//! ONE CONFIGURATION FOR THE SET. The best LC3 configuration every member's sink supports (and, for a call, the voice
//! configuration a member's source supports) is chosen once; each member's sink stream carries the audio location it
//! declared - the left earbud the left channel - and a call records from the first member with a source.
//!
//! THE STEPS, ALL MEMBERS TOGETHER: Config Codec to each; once every ASE is Codec Configured, the CIG with one CIS per
//! member; QoS on each member's CIS; Enable; once every ASE is Enabling, the CISes created together; each CIS up, its
//! ISO data paths set up - host to controller for what this host plays, controller to host for what it records - and
//! Receiver Start Ready for a source; once every ASE streams, `Streaming` says which CIS carries what.
//!
//! THE GROUP WINDS DOWN IN ORDER. `stop` - or a CIS lost, a CIS refused, a member's link gone - releases every ASE; once
//! every ASE is released the CISes still up are disconnected, and only once none is up is the CIG removed, which a
//! controller refuses while any of its CISes stands. `Stopped` comes last, with why.

use crate::ascs::State;
use crate::bap::{Direction, Found};
use crate::le_audio::{self, Config, Purpose, location};
use crate::le_iso::{self, CisParameters};
use alloc::vec::Vec;

/// The CIG this host uses: one device streams at a time.
pub const CIG: u8 = 1;

// An ASE sent a Release, its next state not yet notified: not released until it is.
const RELEASE_ASKED: u8 = 0xff;
// `Disconnect`, and the reason a host ending a stream gives.
const DISCONNECT: u16 = 0x0406;
const REMOTE_USER_TERMINATED: u8 = 0x13;

/// What one member streams, once the group streams: its link, its CIS's handle, and each direction's configuration.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Stream {
	pub link: u16,
	pub cis: u16,
	pub sink: Option<Config>,
	pub source: Option<Config>,
}

/// What a member does next.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Step {
	Configure(Option<Config>, Option<Config>),
	Qos { cis: u8, sink: Option<Config>, source: Option<Config> },
	Enable(u16),
	ReceiverReady,
	Release,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
	/// A member's next operation, on its link.
	Member(u16, Step),
	/// An HCI command to send.
	Hci(u16, Vec<u8>),
	/// The group streams.
	Streaming(Vec<Stream>),
	/// The group stopped, or never started: why.
	Stopped(&'static str),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
	Configuring,
	Grouping,
	Qos,
	Enabling,
	Connecting,
	Streaming,
	Stopping,
	Stopped,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Member {
	link: u16,
	cis_id: u8,
	sink: Option<Config>,
	source: Option<Config>,
	sink_state: Option<u8>,
	source_state: Option<u8>,
	cis: Option<u16>,
	up: bool,
	// Its CIS's disconnection is asked for.
	ending: bool,
}

impl Member {
	// Every ASE it has is released - idle, configured with no stream, or releasing.
	fn released(&self) -> bool {
		matches!(self.sink_state, None | Some(0) | Some(1) | Some(6)) && matches!(self.source_state, None | Some(0) | Some(1) | Some(6))
	}

	// An ASE of it holds a configuration a Release would end.
	fn holds(&self) -> bool {
		!matches!(self.sink_state, None | Some(0) | Some(6)) || !matches!(self.source_state, None | Some(0) | Some(6))
	}
}

pub struct Group {
	members: Vec<Member>,
	contexts: u16,
	stage: Stage,
	// The CIG is set in the controller; and why the group winds down.
	cig: bool,
	reason: &'static str,
}

fn state_code(state: &State) -> u8 {
	match state {
		State::Idle => 0,
		State::CodecConfigured { .. } => 1,
		State::QosConfigured { .. } => 2,
		State::Enabling { .. } => 3,
		State::Streaming { .. } => 4,
		State::Disabling { .. } => 5,
		State::Releasing => 6,
	}
}

impl Group {
	/// THE CONFIGURATION THE SET TAKES, and the group that streams it: Config Codec to each member. `None` - with why -
	/// where no member has a sink, or no configuration suits every member.
	pub fn start(members: &[(u16, Found)], purpose: Purpose) -> Result<(Group, Vec<Action>), &'static str> {
		let sinks: Vec<&Found> = members.iter().map(|(_, found)| found).filter(|found| found.sink.is_some()).collect();
		if sinks.is_empty() {
			return Err("no member of the device has a sink");
		}
		let candidates: &[Config] = match purpose {
			Purpose::Media => &le_audio::MEDIA_CONFIGURATIONS,
			Purpose::Voice => &le_audio::VOICE_CONFIGURATIONS,
		};
		// THE BEST CANDIDATE EVERY SINK SUPPORTS.
		let common = candidates.iter().find(|config| sinks.iter().all(|found| found.sink.as_ref().is_some_and(|capabilities| supports(capabilities, config)))).copied();
		let Some(common) = common else { return Err("no LC3 configuration suits every member") };
		// A CALL RECORDS from the first member with a source that takes the voice configuration.
		let recorder = if purpose == Purpose::Voice { members.iter().position(|(_, found)| found.source.as_ref().is_some_and(|capabilities| supports(capabilities, &common))) } else { None };
		let lone = members.len() == 1;
		let mut group = Group { members: Vec::new(), contexts: if purpose == Purpose::Voice { le_audio::context::CONVERSATIONAL } else { le_audio::context::MEDIA }, stage: Stage::Configuring, cig: false, reason: "stopped" };
		let mut actions = Vec::new();
		for (index, (link, found)) in members.iter().enumerate() {
			// THE MEMBER'S CHANNEL: a set member's one declared location; a lone device's music on every front location it
			// declares - stereo in one stream where it declares both - and its call audio mono.
			let declared = found.sink_locations & (location::FRONT_LEFT | location::FRONT_RIGHT);
			let allocation = match (lone, purpose) {
				(true, Purpose::Media) => declared,
				(true, Purpose::Voice) => 0,
				(false, _) => declared & declared.wrapping_neg(),
			};
			let sink = found.sink.is_some().then_some(Config { allocation, ..common });
			let source = (recorder == Some(index)).then_some(common);
			group.members.push(Member { link: *link, cis_id: index as u8, sink, source, sink_state: None, source_state: None, cis: None, up: false, ending: false });
			actions.push(Action::Member(*link, Step::Configure(sink, source)));
		}
		Ok((group, actions))
	}

	pub fn links(&self) -> impl Iterator<Item = u16> + '_ {
		self.members.iter().map(|member| member.link)
	}

	/// The CIS handles the group's members stream on.
	pub fn cis_handles(&self) -> impl Iterator<Item = u16> + '_ {
		self.members.iter().filter_map(|member| member.cis)
	}

	/// Whether the group is winding down, or done.
	pub fn stopping(&self) -> bool {
		matches!(self.stage, Stage::Stopping | Stage::Stopped)
	}

	fn all(&self, state: u8) -> bool {
		self.members.iter().all(|member| (member.sink.is_none() || member.sink_state == Some(state)) && (member.source.is_none() || member.source_state == Some(state)))
	}

	/// A MEMBER'S ASE MOVED.
	pub fn on_ase(&mut self, link: u16, direction: Direction, state: &State) -> Vec<Action> {
		let code = state_code(state);
		let Some(member) = self.members.iter_mut().find(|member| member.link == link) else { return Vec::new() };
		match direction {
			Direction::Sink if member.sink.is_some() => member.sink_state = Some(code),
			Direction::Source if member.source.is_some() => member.source_state = Some(code),
			_ => return Vec::new(),
		}
		match self.stage {
			// EVERY ASE CONFIGURED: the CIG, one CIS a member, each the size of its own streams.
			Stage::Configuring if self.all(1) => {
				self.stage = Stage::Grouping;
				let interval = self.members.iter().find_map(|member| member.sink.or(member.source)).map_or(10_000, |config| config.sdu_interval_us());
				let cises: Vec<CisParameters> = self.members.iter().map(|member| CisParameters { id: member.cis_id, max_sdu_to_peripheral: member.sink.map_or(0, |config| config.max_sdu()), max_sdu_to_central: member.source.map_or(0, |config| config.max_sdu()), retransmissions: 2 }).collect();
				alloc::vec![Action::Hci(le_iso::opcode::LE_SET_CIG_PARAMETERS, le_iso::set_cig_parameters(CIG, interval, 20, &cises))]
			}
			Stage::Qos if self.all(2) => {
				self.stage = Stage::Enabling;
				let contexts = self.contexts;
				self.members.iter().map(|member| Action::Member(member.link, Step::Enable(contexts))).collect()
			}
			// EVERY ASE ENABLING: the CISes, together.
			Stage::Enabling if self.all(3) => {
				self.stage = Stage::Connecting;
				let pairs: Vec<(u16, u16)> = self.members.iter().filter_map(|member| Some((member.cis?, member.link))).collect();
				alloc::vec![Action::Hci(le_iso::opcode::LE_CREATE_CIS, le_iso::create_cis(&pairs))]
			}
			Stage::Connecting | Stage::Enabling if self.all(4) => {
				self.stage = Stage::Streaming;
				alloc::vec![Action::Streaming(
					self.members.iter().filter_map(|member| Some(Stream { link: member.link, cis: member.cis?, sink: member.sink, source: member.source })).collect()
				)]
			}
			Stage::Stopping => self.wind_down(),
			_ => Vec::new(),
		}
	}

	// THE NEXT STEP DOWN: once every ASE is released, each CIS still up disconnected; once none is up, the CIG removed -
	// where it was set - and the group stopped.
	fn wind_down(&mut self) -> Vec<Action> {
		if self.stage != Stage::Stopping || !self.members.iter().all(Member::released) {
			return Vec::new();
		}
		let mut actions = Vec::new();
		for member in self.members.iter_mut().filter(|member| member.up && !member.ending) {
			member.ending = true;
			if let Some(cis) = member.cis {
				let handle = cis.to_le_bytes();
				actions.push(Action::Hci(DISCONNECT, alloc::vec![handle[0], handle[1], REMOTE_USER_TERMINATED]));
			}
		}
		if self.members.iter().any(|member| member.up) {
			return actions;
		}
		if self.cig {
			self.cig = false;
			actions.push(Action::Hci(le_iso::opcode::LE_REMOVE_CIG, le_iso::remove_cig(CIG).to_vec()));
		}
		self.stage = Stage::Stopped;
		actions.push(Action::Stopped(self.reason));
		actions
	}

	// WIND DOWN for `reason`: every ASE that holds a configuration released, and the next step taken.
	fn end(&mut self, reason: &'static str) -> Vec<Action> {
		if self.stopping() {
			return self.wind_down();
		}
		self.stage = Stage::Stopping;
		self.reason = reason;
		let mut actions = Vec::new();
		for member in self.members.iter_mut().filter(|member| member.holds()) {
			for state in [&mut member.sink_state, &mut member.source_state] {
				if !matches!(state, None | Some(0) | Some(6)) {
					*state = Some(RELEASE_ASKED);
				}
			}
			actions.push(Action::Member(member.link, Step::Release));
		}
		actions.extend(self.wind_down());
		actions
	}

	/// THE CIG IS SET: its CIS handles, in the members' order - QoS on each.
	pub fn on_cig(&mut self, handles: &[u16]) -> Vec<Action> {
		if self.stage != Stage::Grouping {
			return Vec::new();
		}
		if handles.len() != self.members.len() {
			return self.end("the controller refused the CIG");
		}
		self.cig = true;
		self.stage = Stage::Qos;
		let mut actions = Vec::new();
		for (member, handle) in self.members.iter_mut().zip(handles) {
			member.cis = Some(*handle);
			actions.push(Action::Member(member.link, Step::Qos { cis: member.cis_id, sink: member.sink, source: member.source }));
		}
		actions
	}

	/// A CIS UP: its data paths, and the source's readiness - or the group failed.
	pub fn on_cis(&mut self, handle: u16, status: u8) -> Vec<Action> {
		let stopping = self.stopping();
		let Some(member) = self.members.iter_mut().find(|member| member.cis == Some(handle)) else { return Vec::new() };
		if status != 0 {
			return self.end("a CIS was not established");
		}
		member.up = true;
		if stopping {
			// UP WHILE THE GROUP WINDS DOWN: taken down again.
			return self.wind_down();
		}
		let mut actions = Vec::new();
		if member.sink.is_some() {
			actions.push(Action::Hci(le_iso::opcode::LE_SETUP_ISO_DATA_PATH, le_iso::setup_iso_data_path(handle, le_iso::Direction::Input).to_vec()));
		}
		if member.source.is_some() {
			actions.push(Action::Hci(le_iso::opcode::LE_SETUP_ISO_DATA_PATH, le_iso::setup_iso_data_path(handle, le_iso::Direction::Output).to_vec()));
			actions.push(Action::Member(member.link, Step::ReceiverReady));
		}
		actions
	}

	/// STOP: every ASE released, the CISes disconnected, the CIG removed - each once the step before it is done.
	pub fn stop(&mut self) -> Vec<Action> {
		self.end("stopped")
	}

	/// A CIS WENT DOWN: asked for while winding down, or lost - which winds the group down.
	pub fn on_cis_down(&mut self, handle: u16) -> Vec<Action> {
		let Some(member) = self.members.iter_mut().find(|member| member.cis == Some(handle)) else { return Vec::new() };
		member.up = false;
		self.end("a CIS was lost")
	}

	/// A MEMBER'S LINK WENT, and its CIS with it: the group cannot stream as one.
	pub fn on_link_lost(&mut self, link: u16) -> Vec<Action> {
		if !self.members.iter().any(|member| member.link == link) {
			return Vec::new();
		}
		self.members.retain(|member| member.link != link);
		self.end("a member's link was lost")
	}
}

fn supports(capabilities: &le_audio::Capabilities, config: &Config) -> bool {
	let rate_bit = match config.sample_rate {
		8_000 => 1 << 0,
		16_000 => 1 << 2,
		24_000 => 1 << 4,
		32_000 => 1 << 5,
		44_100 => 1 << 6,
		48_000 => 1 << 7,
		_ => 0,
	};
	let duration_bit = if config.frame_us == 7_500 { 0b01 } else { 0b10 };
	capabilities.frequencies & rate_bit != 0 && capabilities.durations & duration_bit != 0 && (capabilities.min_octets..=capabilities.max_octets).contains(&config.octets)
}

#[cfg(test)]
mod tests;
