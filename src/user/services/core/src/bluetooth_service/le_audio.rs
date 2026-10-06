// LE AUDIO, UNICAST: earbuds - a coordinated set, or one device alone - offered to AudioService as an output and, where
// they record, a voice device; streamed over CISes in LC3.
//
// A DEVICE IS ITS SET. A bonded LE peer trusted for audio has its table walked (`service_logic::bap::Member`); one that
// is a member of a coordinated set brings its set's key, and the other members are found by the RSIs they advertise
// and bonded without a second word from the operator - the pairing they started was the set's. The device is offered
// once: its output at the best LC3 configuration every member's sink supports, two channels where the members carry
// left and right (or one device carries both), and its voice at 16 kHz where a member records.
//
// THE STREAM FOLLOWS THE CHANNEL. AudioService opening the output, or the voice device for a session, starts the
// group (`service_logic::cap::Group`): codec, CIG, QoS, Enable, the CISes together, the data paths; each period it
// plays is cut by channel, coded in LC3 a frame per channel, and sent as one SDU per CIS; what a member records
// arrives the same way and is decoded into the capture queue. Closing the channel releases the ASEs and the CIG.
//
// ITS LEVEL IS ITS OWN: the Volume Control Service on every member, and a member's own change taken back.

use super::*;
use proto::system::{AudioEndpoint, AudioEndpointKind};
use proto::system::{BtCallCommand, BtCallState};
use service_logic::bap::{self, Found, Member, Out as BapOut};
use service_logic::cap::{self, Action, Group, Step, Stream};
use service_logic::gtbs;
use service_logic::lc3;
use service_logic::le_audio::{self as lea, Purpose};
use service_logic::le_iso;

// What a member's recording holds for AudioService, at most: a hundred milliseconds.
const CAPTURE_HOLD_MS: usize = 100;
// How long the set's other members are looked for.
const SEARCH_MS: u32 = 10_000;

pub(crate) struct LeLink {
	pub member: Box<Member>,
	pub found: Option<Found>,
}

// One CIS's coding: its encoders - one a channel it carries - and, where it records, its decoder.
pub(crate) struct Coding {
	pub stream: Stream,
	pub encoders: Vec<Box<lc3::Encoder>>,
	pub decoder: Option<Box<lc3::Decoder>>,
	pub sequence: u16,
}

pub(crate) struct LeDevice {
	pub at: usize,
	// The set's key, size and the identities of its bonded members; a lone device has no key.
	pub sirk: Option<[u8; 16]>,
	pub size: u8,
	pub members: Vec<Peer>,
	pub output: Option<u32>,
	pub voice: Option<u32>,
	// The output's format: the configuration chosen for music, and its channels.
	pub media: Option<lea::Config>,
	pub channels: u8,
	pub group: Option<Group>,
	pub purpose: Option<Purpose>,
	// The stream to start once the group winding down has stopped: a call taking the device from music, or music
	// coming back after it.
	pub pending: Option<Purpose>,
	// While the set's other members are looked for: when the device is offered with the members it has; and the scan the
	// search runs on - its id, and the deadline it had before the search lengthened it, none where the search began it.
	pub search_until: Option<u64>,
	pub search_scan: Option<(u32, Option<u64>)>,
	pub coding: Vec<Coding>,
	pub capture: VecDeque<i16>,
	pub dropped: u64,
}

impl LeDevice {
	fn holds(&self, peer: &Peer) -> bool {
		self.members.contains(peer)
	}
}

// THIS HOST'S OWN GATT SERVER, for an LE peer: GAP and GATT, then LE Audio's two - the telephone bearer a voice
// session's call is relayed through (TMAP's call gateway), and the capabilities this host takes as a broadcast sink.
pub(crate) fn host_server(call: BtCallState) -> gatt_server::Server {
	let mut server = gatt_server::Server::new(b"LiberSystem");
	server.add_service(lea::uuid::GTBS, &gtbs::characteristics(call_of(call)));
	server.add_service(lea::uuid::PACS, &lea::sink_pacs());
	server
}

fn call_of(state: BtCallState) -> gtbs::Call {
	match state {
		BtCallState::None => gtbs::Call::None,
		BtCallState::Incoming => gtbs::Call::Incoming,
		BtCallState::Outgoing => gtbs::Call::Outgoing,
		BtCallState::Active => gtbs::Call::Active,
		BtCallState::Held => gtbs::Call::Held,
	}
}

impl Stack {
	// ------------------------------------------------------------------ the call gateway

	// AN EARBUD WROTE THE CALL CONTROL POINT: answered by its notification, and what it asked relayed - the gateway's
	// own refusal where no session declares a call, and where the writer is not an encrypted peer trusted for audio:
	// a call is answered or ended only by a device the operator let carry it.
	pub(crate) fn le_server_writes(&mut self, at: usize, handle: u16) {
		let Some((peer, encrypted)) = self.controllers[at].link(handle).map(|link| (link.peer, link.encrypted)) else { return };
		let trusted = encrypted && self.record(at, &peer).is_some_and(|record| record.trusted.contains(&Profile::Audio));
		let call = if trusted { call_of(self.audio.call) } else { gtbs::Call::None };
		let Some(server) = self.controllers[at].link_mut(handle).and_then(|link| link.server.as_mut()) else { return };
		let writes = server.take_writes();
		let Some(control) = server.handle_of(lea::uuid::CALL_CONTROL_POINT) else { return };
		let mut notifications = Vec::new();
		let mut commands = Vec::new();
		for (attribute, value) in writes {
			if attribute != control {
				continue;
			}
			let (answer, command) = gtbs::control_point(&value, call);
			server.set_value(control, answer);
			notifications.extend(server.notification(control));
			commands.extend(command);
		}
		for notification in notifications {
			self.controllers[at].l2cap(handle, ATT_CID, &notification);
		}
		for command in commands {
			self.audio.command(match command {
				gtbs::Command::Answer => BtCallCommand::Answer,
				gtbs::Command::HangUp => BtCallCommand::HangUp,
				gtbs::Command::Reject => BtCallCommand::Reject,
			});
		}
	}

	// THE CALL A SESSION DECLARED, to every LE peer's bearer: its state and the list, notified where the peer asked.
	pub(crate) fn le_call(&mut self, state: BtCallState) {
		let call = call_of(state);
		for controller in self.controllers.iter_mut() {
			let mut out = Vec::new();
			for link in controller.links.iter_mut().filter(|link| !link.is_classic()) {
				let Some(server) = link.server.as_mut() else { continue };
				for (kind, value) in [(lea::uuid::CALL_STATE, gtbs::call_state(call)), (lea::uuid::BEARER_LIST_CURRENT_CALLS, gtbs::current_calls(call))] {
					let Some(attribute) = server.handle_of(kind) else { continue };
					server.set_value(attribute, value);
					if let Some(notification) = server.notification(attribute) {
						out.push((link.handle, notification));
					}
				}
			}
			for (handle, notification) in out {
				controller.l2cap(handle, ATT_CID, &notification);
			}
		}
	}

	// ------------------------------------------------------------------ the walk

	// A BONDED LE PEER TRUSTED FOR AUDIO, its link encrypted: its table walked - once.
	pub(crate) fn le_audio_start(&mut self, at: usize, handle: u16) {
		// A CONTROLLER WITHOUT THE CIS CENTRAL ROLE carries no unicast stream: its earbuds are not offered at all.
		if !self.controllers[at].le_audio_capable {
			return;
		}
		let Some(link) = self.controllers[at].link(handle) else { return };
		if link.le_audio.is_some() || !link.encrypted || link.is_classic() {
			return;
		}
		let peer = link.peer;
		let Some(record) = self.record(at, &peer) else { return };
		let pending_member = self.le_devices.iter().any(|device| device.holds(&peer));
		if !record.trusted.contains(&Profile::Audio) && !pending_member {
			return;
		}
		// THE BOND'S KEY - looked up with its keys, as the list's records carry none - as the wire carries it, least
		// significant first, which is how the walk takes it to decrypt a set's SIRK; the bond keeps it most significant
		// first. The copies are zeroed once the walk holds its own.
		let Some(mut bond) = self.bond(at, &peer) else { return };
		let mut ltk = [0u8; 16];
		if bond.key.len() == 16 {
			ltk.copy_from_slice(&bond.key);
			ltk.reverse();
		}
		scrub_record(&mut bond);
		let (member, outs) = Member::new(ltk);
		scrub(&mut ltk);
		if let Some(link) = self.controllers[at].link_mut(handle) {
			link.le_audio = Some(LeLink { member: Box::new(member), found: None });
		}
		self.le_audio_outs(at, handle, outs);
	}

	fn le_audio_outs(&mut self, at: usize, handle: u16, outs: Vec<BapOut>) {
		for out in outs {
			match out {
				BapOut::Request(pdu) => self.audio_att(at, handle, pdu),
				BapOut::Event(event) => self.le_audio_event(at, handle, event),
			}
		}
	}

	// A member's answer, routed from the ATT bearer's queue.
	pub(crate) fn le_audio_response(&mut self, at: usize, handle: u16, pdu: &[u8]) {
		let Some(le) = self.controllers[at].link_mut(handle).and_then(|link| link.le_audio.as_mut()) else { return };
		let outs = le.member.on_response(pdu);
		self.le_audio_outs(at, handle, outs);
	}

	// A NOTIFICATION, where the link runs LE Audio: true when it was one of the member's characteristics.
	pub(crate) fn le_audio_notification(&mut self, at: usize, handle: u16, attribute: u16, value: &[u8]) -> bool {
		let Some(le) = self.controllers[at].link_mut(handle).and_then(|link| link.le_audio.as_mut()) else { return false };
		let outs = le.member.on_notification(attribute, value);
		let ours = !outs.is_empty();
		self.le_audio_outs(at, handle, outs);
		ours
	}

	fn le_audio_event(&mut self, at: usize, handle: u16, event: bap::Event) {
		let Some(peer) = self.controllers[at].link(handle).map(|link| link.peer) else { return };
		match event {
			bap::Event::Ready(found) => {
				if let Some(le) = self.controllers[at].link_mut(handle).and_then(|link| link.le_audio.as_mut()) {
					le.found = Some(found.clone());
				}
				print(b"BluetoothService: an LE Audio device's stream endpoints were found\n");
				self.le_device_member(at, peer, &found);
			}
			bap::Event::Ase { direction, state, .. } => {
				let Some(index) = self.le_devices.iter().position(|device| device.at == at && device.holds(&peer)) else { return };
				let actions = match self.le_devices[index].group.as_mut() {
					Some(group) => group.on_ase(handle, direction, &state),
					None => return,
				};
				self.le_group_actions(index, actions);
			}
			bap::Event::Refused { opcode, code, .. } => {
				print(alloc::format!("BluetoothService: an earbud refused stream control operation {opcode} with code {code}\n").as_bytes());
				if let Some(index) = self.le_devices.iter().position(|device| device.at == at && device.holds(&peer)) {
					let actions = self.le_devices[index].group.as_mut().map(Group::stop).unwrap_or_default();
					self.le_group_actions(index, actions);
				}
			}
			bap::Event::Volume(state) => {
				if let Some(id) = self.le_devices.iter().find(|device| device.at == at && device.holds(&peer)).and_then(|device| device.output) {
					self.audio.volume_changed(id, lea::level_of(state.setting));
				}
			}
			bap::Event::Failed(reason) => {
				print(b"BluetoothService: an LE device's table is not one a unicast client drives: ");
				print(reason.as_bytes());
				print(b"\n");
			}
		}
	}

	// ------------------------------------------------------------------ the device

	// A MEMBER FOUND READY: the device it belongs to - its set's, or its own - made or joined, the other members looked
	// for, and the device offered once every member it knows of is ready.
	fn le_device_member(&mut self, at: usize, peer: Peer, found: &Found) {
		let sirk = found.set.map(|(sirk, _, _)| sirk);
		let size = found.set.map_or(1, |(_, size, _)| size.max(1));
		let index = match self.le_devices.iter().position(|device| device.at == at && (device.holds(&peer) || sirk.is_some() && device.sirk == sirk)) {
			Some(index) => index,
			None => {
				self.le_devices.push(LeDevice { at, sirk, size, members: Vec::new(), output: None, voice: None, media: None, channels: 1, group: None, purpose: None, pending: None, search_until: None, search_scan: None, coding: Vec::new(), capture: VecDeque::new(), dropped: 0 });
				self.le_devices.len() - 1
			}
		};
		if !self.le_devices[index].holds(&peer) {
			self.le_devices[index].members.push(peer);
		}
		// THE SET'S OTHER MEMBERS, looked for by their RSIs.
		if (self.le_devices[index].members.len() as u8) < self.le_devices[index].size {
			print(b"BluetoothService: an earbud is a member of a coordinated set; its other members are looked for\n");
			let scan = self.scan_le(at, SEARCH_MS);
			self.le_devices[index].search_scan = scan;
			self.le_devices[index].search_until = Some(clock().saturating_add(u64::from(SEARCH_MS) * TICKS_PER_SECOND / 1000));
			return;
		}
		// EVERY MEMBER IT KNOWS OF READY: offered now, or - with a member still pairing - once that one is.
		if self.le_ready_members(index).len() == self.le_devices[index].members.len() {
			self.le_offer(index);
		}
	}

	pub(crate) fn le_audio_deadline(&self) -> Option<u64> {
		self.le_devices.iter().filter_map(|device| device.search_until).min()
	}

	// THE SEARCH OVER: a member that never bonded is let go, and the device is offered with the members it has.
	pub(crate) fn le_audio_timers(&mut self) {
		let now = clock();
		for index in 0..self.le_devices.len() {
			if self.le_devices[index].search_until.is_none_or(|due| now < due) {
				continue;
			}
			self.le_devices[index].search_until = None;
			self.le_search_done(index);
			let at = self.le_devices[index].at;
			let members = self.le_devices[index].members.clone();
			let bonded: Vec<Peer> = members.into_iter().filter(|peer| self.record(at, peer).is_some()).collect();
			self.le_devices[index].members = bonded;
			print(b"BluetoothService: the coordinated set's search is over; the device is offered with the members found\n");
			self.le_offer(index);
		}
	}

	// The members whose walks are done, with what each found.
	fn le_ready_members(&self, index: usize) -> Vec<(u16, Found)> {
		let device = &self.le_devices[index];
		device
			.members
			.iter()
			.filter_map(|peer| {
				let link = self.controllers.get(device.at)?.link_to(peer)?;
				Some((link.handle, link.le_audio.as_ref()?.found.clone()?))
			})
			.collect()
	}

	// THE DEVICE OFFERED: its output and, where a member records, its voice.
	fn le_offer(&mut self, index: usize) {
		let members = self.le_ready_members(index);
		if members.is_empty() || self.le_devices[index].output.is_some() {
			return;
		}
		let Ok((_, actions)) = Group::start(&members, Purpose::Media) else {
			print(b"BluetoothService: no LC3 configuration suits every member of the LE device\n");
			return;
		};
		let Some(media) = actions.iter().find_map(|action| match action {
			Action::Member(_, Step::Configure(sink, _)) => *sink,
			_ => None,
		}) else {
			return;
		};
		// TWO CHANNELS where the members carry left and right between them, or one carries both.
		let locations = members.iter().fold(0u32, |all, (_, found)| all | found.sink_locations) & (lea::location::FRONT_LEFT | lea::location::FRONT_RIGHT);
		let channels = if locations.count_ones() == 2 { 2 } else { 1 };
		let at = self.le_devices[index].at;
		let first = self.le_devices[index].members[0];
		let handle = self.controllers[at].link_to(&first).map_or(0, |link| link.handle);
		let name = self.controllers[at].bredr_state.name_of(&first).or_else(|| self.record(at, &first).map(|record| record.name)).unwrap_or_default();
		let level = members.iter().find_map(|(_, found)| found.volume).map_or(100, |state| lea::level_of(state.setting));
		let frame_us = media.frame_us;
		let output = AudioEndpoint { id: 0, peer: peer_to_wire(&first), name: name.clone(), kind: AudioEndpointKind::Output, rate: media.sample_rate, channels, latency_us: frame_us * 2 + 20_000, hardware_volume: true, volume: level };
		let output = self.audio.offer(output, at, handle);
		// A CALL AT THE VOICE CONFIGURATION THE SET TAKES, where a member records.
		let records = members.iter().any(|(_, found)| found.source.is_some());
		let voice_config = if records {
			Group::start(&members, Purpose::Voice).ok().and_then(|(_, actions)| {
				actions.iter().find_map(|action| match action {
					Action::Member(_, Step::Configure(_, Some(source))) => Some(*source),
					_ => None,
				})
			})
		} else {
			None
		};
		let voice = voice_config.and_then(|config| self.audio.offer(AudioEndpoint { id: 0, peer: peer_to_wire(&first), name, kind: AudioEndpointKind::Voice, rate: config.sample_rate, channels: 1, latency_us: config.frame_us * 2 + 20_000, hardware_volume: true, volume: level }, at, handle));
		self.le_search_done(index);
		let device = &mut self.le_devices[index];
		device.search_until = None;
		device.output = output;
		device.voice = voice;
		device.media = Some(media);
		device.channels = channels;
		print(b"BluetoothService: an LE Audio device is offered to AudioService\n");
	}

	// THE SET'S OTHER MEMBER ADVERTISING: its RSI resolves with a device's key - paired, as the set's pairing was.
	pub(crate) fn le_audio_advertised(&mut self, at: usize, address: &PeerAddress, data: &[u8]) {
		let Some(rsi) = lea::rsi(data) else { return };
		let searching = self.le_devices.iter().any(|device| device.at == at && device.search_until.is_some());
		let Some(index) = self.le_devices.iter().position(|device| device.at == at && device.sirk.is_some_and(|sirk| lea::rsi_resolves(&sirk, &rsi)) && (device.members.len() as u8) < device.size) else {
			if searching {
				print(b"BluetoothService: an advertiser's RSI names no set being looked for\n");
			}
			return;
		};
		let Some(peer) = peer_from_wire(address) else { return };
		if self.le_devices[index].holds(&peer) || self.record(at, &peer).is_some() {
			return;
		}
		// PAIRED AS THE SET'S - the operator's pairing of the first member was the consent - and a member only once the
		// attempt is taken; one refused for now (another pairing running) is tried again at its next advertisement.
		match self.pair_le(at, peer, false) {
			Ok(()) => {
				print(b"BluetoothService: the coordinated set's other member was found; it is paired as the set's\n");
				self.le_devices[index].members.push(peer);
			}
			Err(error) => print(alloc::format!("BluetoothService: the coordinated set's other member was found, but its pairing was refused for now ({error:?}); it is tried again when it is heard again\n").as_bytes()),
		}
	}

	// A SET MEMBER BONDED: known by its identity from now on, and trusted for audio as the set is.
	pub(crate) fn le_audio_bonded(&mut self, at: usize, advertised: &Peer, identity: &Peer) {
		for device in self.le_devices.iter_mut().filter(|device| device.at == at) {
			for member in device.members.iter_mut().filter(|member| *member == advertised) {
				*member = *identity;
			}
		}
		let peer = identity;
		if self.le_devices.iter().any(|device| device.at == at && device.holds(peer)) && !self.record(at, peer).is_some_and(|record| record.trusted.contains(&Profile::Audio)) {
			let _ = self.rewrite_bond(at, peer, |record| {
				record.trusted.retain(|held| *held != Profile::Audio);
				record.trusted.push(Profile::Audio);
			});
		}
	}

	// ------------------------------------------------------------------ AudioService's channels

	pub(crate) fn le_device_of(&self, id: u32) -> Option<usize> {
		self.le_devices.iter().position(|device| device.output == Some(id) || device.voice == Some(id))
	}

	// THE CHANNEL OPENED: the group starts, for music or a call.
	pub(crate) fn le_audio_open(&mut self, id: u32) {
		let Some(index) = self.le_device_of(id) else { return };
		let purpose = if self.le_devices[index].voice == Some(id) { Purpose::Voice } else { Purpose::Media };
		let device = &mut self.le_devices[index];
		if let Some(group) = device.group.as_mut() {
			if device.purpose == Some(purpose) && !group.stopping() {
				return;
			}
			// ONE STREAM AT A TIME: a call takes the device from music - once the music's group has wound down, since
			// its ASEs and its CIG are what the call's will be.
			device.pending = Some(purpose);
			let actions = group.stop();
			self.le_group_actions(index, actions);
			return;
		}
		self.le_start(index, purpose);
	}

	fn le_start(&mut self, index: usize, purpose: Purpose) {
		let members = self.le_ready_members(index);
		match Group::start(&members, purpose) {
			Ok((group, actions)) => {
				self.le_devices[index].group = Some(group);
				self.le_devices[index].purpose = Some(purpose);
				self.le_group_actions(index, actions);
			}
			Err(reason) => {
				print(b"BluetoothService: the LE device cannot stream: ");
				print(reason.as_bytes());
				print(b"\n");
			}
		}
	}

	// THE CHANNEL CLOSED: the ASEs released and the CIG removed - and music back where its channel is still open.
	pub(crate) fn le_audio_close(&mut self, id: u32) {
		let Some(index) = self.le_device_of(id) else { return };
		let purpose = if self.le_devices[index].voice == Some(id) { Purpose::Voice } else { Purpose::Media };
		let output_open = self.le_devices[index].output.is_some_and(|output| self.audio.pcm.iter().any(|pcm| pcm.endpoint == output));
		let device = &mut self.le_devices[index];
		if device.pending == Some(purpose) {
			device.pending = None;
		}
		if device.purpose != Some(purpose) {
			return;
		}
		if purpose == Purpose::Voice && output_open {
			device.pending = Some(Purpose::Media);
		}
		device.capture.clear();
		let actions = device.group.as_mut().map(Group::stop).unwrap_or_default();
		self.le_group_actions(index, actions);
	}

	// A LEVEL, to every member's volume control.
	pub(crate) fn le_audio_volume(&mut self, id: u32, level: u8) -> Result<(), Error> {
		let index = self.le_device_of(id).ok_or(Error::NotFound)?;
		let at = self.le_devices[index].at;
		for peer in self.le_devices[index].members.clone() {
			let Some(handle) = self.controllers[at].link_to(&peer).map(|link| link.handle) else { continue };
			let outs = match self.controllers[at].link_mut(handle).and_then(|link| link.le_audio.as_mut()) {
				Some(le) => le.member.set_volume(level),
				None => continue,
			};
			self.le_audio_outs(at, handle, outs);
		}
		Ok(())
	}

	fn le_group_actions(&mut self, index: usize, actions: Vec<Action>) {
		let at = self.le_devices[index].at;
		for action in actions {
			match action {
				Action::Member(handle, step) => {
					let outs = match self.controllers[at].link_mut(handle).and_then(|link| link.le_audio.as_mut()) {
						Some(le) => match step {
							Step::Configure(sink, source) => le.member.configure(sink, source),
							Step::Qos { cis, sink, source } => le.member.qos(cap::CIG, cis, sink, source),
							Step::Enable(contexts) => le.member.enable(contexts),
							Step::ReceiverReady => le.member.receiver_ready(),
							Step::Release => le.member.release(),
						},
						None => continue,
					};
					self.le_audio_outs(at, handle, outs);
				}
				Action::Hci(op, params) => {
					self.controllers[at].command(op, &params);
				}
				Action::Streaming(streams) => self.le_streaming(index, streams),
				Action::Stopped(reason) => {
					let device = &mut self.le_devices[index];
					device.group = None;
					device.purpose = None;
					device.coding.clear();
					print(b"BluetoothService: the LE device's stream ended: ");
					print(reason.as_bytes());
					print(b"\n");
					if let Some(purpose) = self.le_devices[index].pending.take() {
						self.le_start(index, purpose);
					}
				}
			}
		}
	}

	// THE GROUP STREAMS: a coder for each CIS - an encoder a channel it carries, a decoder where it records.
	fn le_streaming(&mut self, index: usize, streams: Vec<Stream>) {
		let mut coding = Vec::new();
		for stream in streams {
			let encoders = stream.sink.map_or(0, |config| config.channels());
			let mut encoder_list = Vec::new();
			if let Some(config) = stream.sink.and_then(|config| lc3::Config::new(config.sample_rate, config.frame_us)) {
				for _ in 0..encoders {
					encoder_list.push(Box::new(lc3::Encoder::new(config)));
				}
			}
			let decoder = stream.source.and_then(|config| lc3::Config::new(config.sample_rate, config.frame_us)).map(|config| Box::new(lc3::Decoder::new(config)));
			coding.push(Coding { stream, encoders: encoder_list, decoder, sequence: 0 });
		}
		self.le_devices[index].coding = coding;
		print(b"BluetoothService: an LE Audio device streams over its CISes\n");
	}

	// The CIG's handles, from its command's completion.
	pub(crate) fn le_audio_cig(&mut self, at: usize, params: &[u8]) {
		let Some(index) = self.le_devices.iter().position(|device| device.at == at && device.group.is_some()) else { return };
		let handles = le_iso::cig_handles(params).map(|(_, handles)| handles).unwrap_or_default();
		let actions = self.le_devices[index].group.as_mut().map(|group| group.on_cig(&handles)).unwrap_or_default();
		self.le_group_actions(index, actions);
	}

	// THE LE AUDIO SUBEVENTS: a CIS up; the broadcast's own go to `broadcast`.
	pub(crate) fn on_le_audio_event(&mut self, at: usize, event: le_iso::Event) {
		match event {
			le_iso::Event::CisEstablished(cis) => {
				let Some(index) = self.le_devices.iter().position(|device| device.at == at && device.group.as_ref().is_some_and(|group| group.cis_handles().any(|handle| handle == cis.handle))) else { return };
				let actions = self.le_devices[index].group.as_mut().map(|group| group.on_cis(cis.handle, cis.status)).unwrap_or_default();
				self.le_group_actions(index, actions);
			}
			other => self.broadcast_event(at, other),
		}
	}

	// Whether a handle is one of this controller's isochronous streams'.
	pub(crate) fn iso_handle(&self, at: usize, handle: u16) -> bool {
		self.le_devices.iter().any(|device| device.at == at && device.group.as_ref().is_some_and(|group| group.cis_handles().any(|held| held == handle))) || self.broadcast_handle(at, handle)
	}

	// A CIS WENT DOWN: asked for by the group winding down, or lost. True when it was one.
	pub(crate) fn le_audio_disconnected(&mut self, at: usize, handle: u16) -> bool {
		let Some(index) = self.le_devices.iter().position(|device| device.at == at && device.group.as_ref().is_some_and(|group| group.cis_handles().any(|held| held == handle))) else { return false };
		let actions = self.le_devices[index].group.as_mut().map(|group| group.on_cis_down(handle)).unwrap_or_default();
		self.le_group_actions(index, actions);
		true
	}

	// A MEMBER'S LINK WENT.
	pub(crate) fn le_audio_link_gone(&mut self, at: usize, link: &mut Link) {
		if link.le_audio.take().is_none() {
			return;
		}
		let Some(index) = self.le_devices.iter().position(|device| device.at == at && device.holds(&link.peer)) else { return };
		let actions = self.le_devices[index].group.as_mut().map(|group| group.on_link_lost(link.handle)).unwrap_or_default();
		self.le_group_actions(index, actions);
		// NO MEMBER CONNECTED: the device leaves AudioService.
		let device = &self.le_devices[index];
		let connected = device.members.iter().any(|peer| *peer != link.peer && self.controllers[at].link_to(peer).is_some());
		if !connected {
			let (output, voice) = (device.output, device.voice);
			for id in [output, voice].into_iter().flatten() {
				self.audio.withdraw(id);
			}
			let device = &mut self.le_devices[index];
			device.output = None;
			device.voice = None;
		}
	}

	// THE CONTROLLER RESET OR FAULTED: its links, CISes and CIG are gone with the session, and so are its devices -
	// walked again when their members reconnect.
	pub(crate) fn le_audio_reset(&mut self, at: usize) {
		let mut gone = Vec::new();
		self.le_devices.retain(|device| {
			if device.at != at {
				return true;
			}
			gone.extend([device.output, device.voice].into_iter().flatten());
			false
		});
		for id in gone {
			self.audio.withdraw(id);
		}
		if self.broadcast.playing.as_ref().is_some_and(|playing| playing.at == at) {
			self.broadcast_end(false);
		}
		if self.broadcast.scanning.is_some_and(|(held, _)| held == at) {
			self.broadcast.scanning = None;
		}
	}

	// A SCAN OF THE HOST'S OWN, for a set's other members: their advertisements reach `le_audio_advertised` whoever
	// scans. A SCAN ALREADY RUNNING - an operator's, or the broadcast sink's - IS RESTARTED, and runs at least as long as
	// the search: a controller reports each advertiser once per enable, and the other member was reported before this
	// host held the set's key to know it.
	//
	// Answers the scan the search runs on: its id and the deadline it had before, where it lengthened one that ran.
	pub(crate) fn scan_le(&mut self, at: usize, ms: u32) -> Option<(u32, Option<u64>)> {
		let ticks = u64::from(ms).saturating_mul(TICKS_PER_SECOND) / 1000;
		let due = clock().saturating_add(ticks.max(1));
		if let Some((held, until)) = self.broadcast.scanning.as_mut()
			&& *held == at
		{
			*until = (*until).max(due);
			if let Some(controller) = self.controllers.get_mut(at) {
				controller.command(le_iso::opcode::LE_SET_EXTENDED_SCAN_ENABLE, &le_iso::extended_scan_enable(false));
				controller.command(le_iso::opcode::LE_SET_EXTENDED_SCAN_ENABLE, &le_iso::extended_scan_enable(true));
			}
			return None;
		}
		let id = self.next_scan;
		let controller = self.controllers.get_mut(at)?;
		if !controller.powered {
			return None;
		}
		if let Some(scan) = controller.scan.as_mut().filter(|scan| scan.active()) {
			if !scan.running {
				return None;
			}
			let held = (scan.id, Some(scan.deadline));
			scan.deadline = scan.deadline.max(due);
			controller.le_scan_enable(false);
			controller.le_scan_enable(true);
			print(b"BluetoothService: the scan running is restarted to hear the set's other members\n");
			return Some(held);
		}
		if !controller.le_scan_parameters() || !controller.le_scan_enable(true) {
			return None;
		}
		controller.scan = Some(Scan { id, deadline: due, running: true, inquiring: false, results: Vec::new(), paging: Vec::new() });
		self.next_scan = self.next_scan.wrapping_add(1);
		Some((id, None))
	}

	// THE SEARCH IS OVER - every member found, or its time up: the scan it ran on gets its own deadline back, and one it
	// began ends, so a scan the operator asks for next is not refused for it.
	fn le_search_done(&mut self, index: usize) {
		let Some((id, before)) = self.le_devices[index].search_scan.take() else { return };
		let at = self.le_devices[index].at;
		let Some(controller) = self.controllers.get_mut(at) else { return };
		let now = clock();
		let stop = match controller.scan.as_mut().filter(|scan| scan.id == id && scan.running) {
			Some(scan) => match before {
				Some(deadline) if deadline > now => {
					scan.deadline = deadline;
					false
				}
				_ => {
					scan.running = false;
					true
				}
			},
			None => false,
		};
		if stop {
			controller.le_scan_enable(false);
		}
	}

	// ------------------------------------------------------------------ the samples

	// A PERIOD AUDIOSERVICE PLAYED: each CIS's channels cut out of it, coded, and sent as one SDU.
	pub(crate) fn le_audio_play(&mut self, id: u32, samples: &[i16]) {
		let Some(index) = self.le_device_of(id) else { return };
		let at = self.le_devices[index].at;
		let channels = usize::from(if self.le_devices[index].voice == Some(id) { 1 } else { self.le_devices[index].channels.max(1) });
		let mut packets = Vec::new();
		let device = &mut self.le_devices[index];
		for coding in device.coding.iter_mut() {
			let Some(config) = coding.stream.sink else { continue };
			let frame = config.sample_rate as usize * config.frame_us as usize / 1_000_000;
			let octets = usize::from(config.octets);
			let mut sdu = alloc::vec![0u8; octets * coding.encoders.len()];
			let picks = channel_picks(config.allocation, channels);
			for (slot, encoder) in coding.encoders.iter_mut().enumerate() {
				let pick = picks.get(slot).copied().unwrap_or(Pick::Mix);
				let mut pcm = alloc::vec![0i16; frame];
				for (at_frame, sample) in pcm.iter_mut().enumerate() {
					let base = at_frame * channels;
					*sample = match pick {
						Pick::Channel(channel) => samples.get(base + channel.min(channels - 1)).copied().unwrap_or(0),
						Pick::Mix => (samples.get(base..base + channels).map_or(0, |all| all.iter().map(|value| i32::from(*value)).sum::<i32>() / channels as i32)) as i16,
					};
				}
				if encoder.encode(&pcm, &mut sdu[slot * octets..(slot + 1) * octets]).is_err() {
					device.dropped += 1;
				}
			}
			packets.push(le_iso::iso_packet(coding.stream.cis, coding.sequence, &sdu));
			coding.sequence = coding.sequence.wrapping_add(1);
		}
		for packet in packets {
			self.send_iso(at, &packet);
		}
	}

	// What a member recorded, as AudioService takes it: a period, where one is held.
	pub(crate) fn le_audio_take_period(&mut self, id: u32, samples: usize) -> Option<Vec<u8>> {
		let index = self.le_device_of(id)?;
		let capture = &mut self.le_devices[index].capture;
		if capture.len() < samples {
			return None;
		}
		let mut out = Vec::with_capacity(samples * 2);
		for sample in capture.drain(..samples) {
			out.extend_from_slice(&sample.to_le_bytes());
		}
		Some(out)
	}

	// AN SDU FROM A CIS: what a member recorded, decoded into the capture queue - a lost one concealed.
	pub(crate) fn le_audio_sdu(&mut self, at: usize, sdu: &le_iso::Sdu) {
		let Some(index) = self.le_devices.iter().position(|device| device.at == at && device.coding.iter().any(|coding| coding.stream.cis == sdu.handle && coding.decoder.is_some())) else { return };
		let device = &mut self.le_devices[index];
		let Some(coding) = device.coding.iter_mut().find(|coding| coding.stream.cis == sdu.handle) else { return };
		let (Some(decoder), Some(config)) = (coding.decoder.as_mut(), coding.stream.source) else { return };
		let frame = config.sample_rate as usize * config.frame_us as usize / 1_000_000;
		let mut pcm = alloc::vec![0i16; frame];
		let data = (sdu.valid && sdu.data.len() >= usize::from(config.octets)).then(|| &sdu.data[..usize::from(config.octets)]);
		if decoder.decode(data, &mut pcm).is_err() {
			pcm.fill(0);
			device.dropped += 1;
		}
		device.capture.extend(pcm.iter().copied());
		let hold = config.sample_rate as usize * CAPTURE_HOLD_MS / 1000;
		while device.capture.len() > hold {
			device.capture.pop_front();
		}
		if let Some(id) = device.voice {
			self.audio_capture_ready(id);
		}
	}

	// ONE ISO PACKET to the controller, where it has a buffer free; otherwise dropped and counted - a late frame is
	// worth nothing to an earbud.
	pub(crate) fn send_iso(&mut self, at: usize, packet: &[u8]) {
		let controller = &mut self.controllers[at];
		if controller.iso_free == 0 {
			controller.iso_dropped = controller.iso_dropped.saturating_add(1);
			return;
		}
		if send_packet(controller.transport, HciPacketKind::Iso, packet) {
			controller.iso_free -= 1;
		}
	}
}

// Which channel of AudioService's period a CIS's encoder takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pick {
	Channel(usize),
	Mix,
}

// THE CHANNELS A STREAM CARRIES, in its frames' order: left before right, by the locations its allocation names; a
// mono stream takes a mix of a stereo period.
fn channel_picks(allocation: u32, channels: usize) -> Vec<Pick> {
	let mut out = Vec::new();
	if channels == 1 {
		out.push(Pick::Channel(0));
		return out;
	}
	if allocation & lea::location::FRONT_LEFT != 0 {
		out.push(Pick::Channel(0));
	}
	if allocation & lea::location::FRONT_RIGHT != 0 {
		out.push(Pick::Channel(1));
	}
	if out.is_empty() {
		out.push(Pick::Mix);
	}
	out
}
