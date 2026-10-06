// LE AUDIO, BROADCAST: this host as a Broadcast Sink - an Auracast broadcast found, chosen by the operator, joined and
// played by AudioService to the default output, as a phone's A2DP stream is.
//
// FOUND BY ITS ANNOUNCEMENT. `broadcast-scan` runs extended scanning; every advertisement carrying a Broadcast Audio
// Announcement is a broadcast heard, by its Broadcast ID, its name and its advertiser's address and set.
//
// JOINED IN THREE STEPS. `broadcast-play` syncs to the advertiser's periodic train; the train's Basic Audio Announcement
// is its BASE - the streams, their LC3 configurations and locations - from which this host takes a left and a right
// stream where there are both, or one; the train's BIGInfo says whether the BIG is encrypted, and the BIG is synced
// with the operator's Broadcast Code where it is. Each BIS's data path is set up, and the broadcast is offered to
// AudioService as a route at the streams' rate and channels; each SDU decoded is the route's audio, a lost one
// concealed.
//
// ONE BROADCAST AT A TIME, ENDED BY THE OPERATOR OR LOST: either way the route leaves AudioService.

use super::*;
use proto::system::{AudioEndpoint, AudioEndpointKind, BroadcastSource};
use service_logic::lc3;
use service_logic::le_audio as lea;
use service_logic::le_iso;

// The longest broadcast scan an operator may ask for.
const MAX_SCAN_SECONDS: u32 = 30;
// The broadcasts one scan keeps.
const MAX_SOURCES: usize = 16;
// The BIG handle this host uses.
const BIG: u8 = 0;
// What the route holds for AudioService, at most: two hundred milliseconds.
const ROUTE_HOLD_MS: usize = 200;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
	Syncing,
	Reading,
	Joining,
	Playing,
}

pub(crate) struct Playing {
	pub at: usize,
	broadcast_id: u32,
	code: Option<[u8; 16]>,
	stage: Stage,
	sync: Option<u16>,
	// The BISes taken - their index and configuration - and once joined their handles and decoders, in that order.
	bises: Vec<lea::Bis>,
	handles: Vec<u16>,
	decoders: Vec<Box<lc3::Decoder>>,
	// A frame of each BIS's decoded samples, waiting for the others' to interleave.
	frames: Vec<Option<Vec<i16>>>,
	pub route: Option<u32>,
	pub queue: VecDeque<i16>,
	rate: u32,
	channels: u8,
}

#[derive(Default)]
pub(crate) struct Sink {
	pub scanning: Option<(usize, u64)>,
	pub sources: Vec<(BroadcastSource, u8)>,
	pub playing: Option<Playing>,
}

impl Stack {
	// ------------------------------------------------------------------ the operator's calls

	pub(crate) fn broadcast_scan(&mut self, at: usize, seconds: u32) -> Result<(), Error> {
		let controller = self.controllers.get_mut(at).ok_or(Error::NotFound)?;
		if !controller.powered {
			return Err(Error::Closed);
		}
		if !controller.extended {
			return Err(Error::Unsupported);
		}
		if controller.scan.as_ref().is_some_and(Scan::active) || self.broadcast.scanning.is_some_and(|(held, _)| held != at) {
			return Err(Error::Again);
		}
		let ticks = u64::from(seconds.clamp(1, MAX_SCAN_SECONDS)) * TICKS_PER_SECOND;
		// A SCAN ALREADY RUNNING here - one a play started, or the last one - runs on for the time asked, keeping what
		// it heard.
		if let Some((_, due)) = self.broadcast.scanning.as_mut() {
			*due = (*due).max(clock().saturating_add(ticks));
			return Ok(());
		}
		let own = controller.le.own_type();
		if !controller.command(le_iso::opcode::LE_SET_EXTENDED_SCAN_PARAMETERS, &le_iso::extended_scan_parameters(own)) || !controller.command(le_iso::opcode::LE_SET_EXTENDED_SCAN_ENABLE, &le_iso::extended_scan_enable(true)) {
			return Err(Error::Exhausted);
		}
		self.broadcast.scanning = Some((at, clock().saturating_add(ticks)));
		self.broadcast.sources.clear();
		Ok(())
	}

	pub(crate) fn broadcasts(&self, at: usize) -> Result<Vec<BroadcastSource>, Error> {
		self.controllers.get(at).ok_or(Error::NotFound)?;
		Ok(self.broadcast.sources.iter().map(|(source, _)| source.clone()).collect())
	}

	// PLAY ONE: the periodic train first.
	pub(crate) fn broadcast_play(&mut self, at: usize, broadcast_id: u32, code: &[u8]) -> Result<(), Error> {
		let code: Option<[u8; 16]> = match code.len() {
			0 => None,
			16 => code.try_into().ok(),
			_ => return Err(Error::Invalid),
		};
		let (source, address_type) = self.broadcast.sources.iter().find(|(source, _)| source.broadcast_id == broadcast_id).cloned().ok_or(Error::NotFound)?;
		// ONE SCAN AT A TIME on a controller: the train is found by the broadcast's own, never beside an operator's.
		if self.controllers.get(at).ok_or(Error::NotFound)?.scan.as_ref().is_some_and(Scan::active) {
			return Err(Error::Again);
		}
		self.broadcast_end(true);
		let controller = self.controllers.get_mut(at).ok_or(Error::NotFound)?;
		let mut wire = [0u8; 6];
		wire.copy_from_slice(&source.address.bytes);
		let wire = hci_codec::address_to_wire(&wire);
		// THE TRAIN IS FOUND WHILE SCANNING: the scan runs on until the sync is established.
		if self.broadcast.scanning.is_none() {
			let own = controller.le.own_type();
			controller.command(le_iso::opcode::LE_SET_EXTENDED_SCAN_PARAMETERS, &le_iso::extended_scan_parameters(own));
			controller.command(le_iso::opcode::LE_SET_EXTENDED_SCAN_ENABLE, &le_iso::extended_scan_enable(true));
			self.broadcast.scanning = Some((at, clock().saturating_add(u64::from(MAX_SCAN_SECONDS) * TICKS_PER_SECOND)));
		}
		if !controller.command(le_iso::opcode::LE_PERIODIC_ADVERTISING_CREATE_SYNC, &le_iso::periodic_create_sync(source.sid, address_type, &wire)) {
			return Err(Error::Exhausted);
		}
		self.broadcast.playing = Some(Playing { at, broadcast_id, code, stage: Stage::Syncing, sync: None, bises: Vec::new(), handles: Vec::new(), decoders: Vec::new(), frames: Vec::new(), route: None, queue: VecDeque::new(), rate: 0, channels: 0 });
		print(b"BluetoothService: joining a broadcast's periodic train\n");
		Ok(())
	}

	pub(crate) fn broadcast_stop(&mut self, at: usize) -> Result<(), Error> {
		if self.broadcast.playing.as_ref().is_none_or(|playing| playing.at != at) {
			return Err(Error::NotFound);
		}
		self.broadcast_end(true);
		Ok(())
	}

	// THE BROADCAST ENDS: its BIG and its sync let go where `tell` says the controller holds them, and its route withdrawn.
	pub(crate) fn broadcast_end(&mut self, tell: bool) {
		let Some(mut playing) = self.broadcast.playing.take() else { return };
		if let Some(code) = playing.code.as_mut() {
			scrub(code);
		}
		if tell && let Some(controller) = self.controllers.get_mut(playing.at) {
			if !playing.handles.is_empty() {
				controller.command(le_iso::opcode::LE_BIG_TERMINATE_SYNC, &[BIG]);
			}
			if let Some(sync) = playing.sync {
				controller.command(le_iso::opcode::LE_PERIODIC_ADVERTISING_TERMINATE_SYNC, &sync.to_le_bytes());
			}
		}
		if let Some(id) = playing.route {
			self.audio.withdraw(id);
		}
		print(b"BluetoothService: the broadcast ended\n");
	}

	fn broadcast_scan_off(&mut self) {
		if let Some((at, _)) = self.broadcast.scanning.take()
			&& let Some(controller) = self.controllers.get_mut(at)
		{
			controller.command(le_iso::opcode::LE_SET_EXTENDED_SCAN_ENABLE, &le_iso::extended_scan_enable(false));
		}
	}

	pub(crate) fn broadcast_deadline(&self) -> Option<u64> {
		self.broadcast.scanning.map(|(_, due)| due)
	}

	pub(crate) fn broadcast_timers(&mut self) {
		if self.broadcast.scanning.is_some_and(|(_, due)| clock() >= due) {
			self.broadcast_scan_off();
			if self.broadcast.playing.as_ref().is_some_and(|playing| playing.stage == Stage::Syncing) {
				print(b"BluetoothService: the broadcast's periodic train was not found\n");
				self.broadcast_end(true);
			}
		}
	}

	// ------------------------------------------------------------------ the controller's account

	// AN EXTENDED REPORT: a broadcast's announcement kept; any other advertisement is the ordinary scan's.
	pub(crate) fn broadcast_report(&mut self, at: usize, report: &le_iso::ExtendedReport) -> bool {
		let Some((broadcast_id, name)) = lea::broadcast_announcement(&report.data) else { return false };
		if self.broadcast.scanning.is_none_or(|(held, _)| held != at) {
			return true;
		}
		if self.broadcast.sources.iter().any(|(source, _)| source.broadcast_id == broadcast_id) || self.broadcast.sources.len() >= MAX_SOURCES {
			return true;
		}
		let kind = if report.address_type & 1 == 0 { PeerKind::Public } else { PeerKind::RandomStatic };
		let address = PeerAddress { kind, bytes: hci_codec::address_from_wire(&report.wire_address).to_vec() };
		self.broadcast.sources.push((BroadcastSource { broadcast_id, name, address, sid: report.sid }, report.address_type & 1));
		true
	}

	pub(crate) fn broadcast_event(&mut self, at: usize, event: le_iso::Event) {
		match event {
			le_iso::Event::ExtendedReports(reports) => {
				for report in reports {
					if !self.broadcast_report(at, &report) {
						self.extended_advertising(at, &report);
					}
				}
			}
			le_iso::Event::SyncEstablished(established) => {
				let Some(playing) = self.broadcast.playing.as_mut().filter(|playing| playing.at == at && playing.stage == Stage::Syncing) else { return };
				if established.status != 0 {
					print(b"BluetoothService: the broadcast's periodic train could not be joined\n");
					self.broadcast_end(false);
					return;
				}
				playing.sync = Some(established.sync);
				playing.stage = Stage::Reading;
			}
			le_iso::Event::PeriodicReport { sync, data, complete } => {
				let Some(playing) = self.broadcast.playing.as_mut().filter(|playing| playing.sync == Some(sync) && playing.stage == Stage::Reading) else { return };
				if !complete {
					return;
				}
				let Some(base) = lea::service_data(&data, lea::uuid::BASIC_AUDIO_ANNOUNCEMENT).and_then(lea::parse_base) else { return };
				// LEFT AND RIGHT where a subgroup has both, otherwise its first stream.
				let mut chosen: Vec<lea::Bis> = Vec::new();
				for subgroup in &base.subgroups {
					let left = subgroup.bises.iter().find(|bis| bis.config.allocation & lea::location::FRONT_LEFT != 0);
					let right = subgroup.bises.iter().find(|bis| bis.config.allocation & lea::location::FRONT_RIGHT != 0);
					match (left, right) {
						(Some(left), Some(right)) if left.index != right.index => chosen = alloc::vec![left.clone(), right.clone()],
						_ => chosen = subgroup.bises.first().cloned().into_iter().collect(),
					}
					if !chosen.is_empty() {
						break;
					}
				}
				if chosen.is_empty() {
					print(b"BluetoothService: the broadcast carries no LC3 stream this host decodes\n");
					self.broadcast_end(true);
					return;
				}
				playing.bises = chosen;
				playing.stage = Stage::Joining;
			}
			le_iso::Event::BigInfo(info) => {
				let Some(playing) = self.broadcast.playing.as_mut().filter(|playing| playing.sync == Some(info.sync) && playing.stage == Stage::Joining && playing.handles.is_empty()) else { return };
				if info.encrypted && playing.code.is_none() {
					print(b"BluetoothService: the broadcast is encrypted and no Broadcast Code was given\n");
					self.broadcast_end(true);
					return;
				}
				let indexes: Vec<u8> = playing.bises.iter().map(|bis| bis.index).collect();
				// THE CODE IS HELD UNTIL THE CONTROLLER HAS IT, and no longer: the queue zeroes its copy once sent.
				let mut code = playing.code.take();
				let mut parameters = le_iso::big_create_sync(BIG, info.sync, if info.encrypted { code.as_ref() } else { None }, &indexes);
				// THE BIG IS ASKED FOR ONCE: a second BIGInfo is not a second request.
				playing.handles = alloc::vec![0; indexes.len()];
				self.controllers[at].command(le_iso::opcode::LE_BIG_CREATE_SYNC, &parameters);
				scrub(&mut parameters);
				if let Some(code) = code.as_mut() {
					scrub(code);
				}
			}
			le_iso::Event::BigEstablished(established) => {
				let Some(playing) = self.broadcast.playing.as_mut().filter(|playing| playing.at == at && established.big == BIG) else { return };
				if established.status != 0 || established.handles.len() != playing.bises.len() {
					print(b"BluetoothService: the broadcast's BIG could not be joined - a wrong Broadcast Code, or the source left\n");
					playing.handles.clear();
					self.broadcast_end(true);
					return;
				}
				playing.handles = established.handles.clone();
				playing.decoders = playing.bises.iter().filter_map(|bis| lc3::Config::new(bis.config.sample_rate, bis.config.frame_us)).map(|config| Box::new(lc3::Decoder::new(config))).collect();
				playing.frames = alloc::vec![None; playing.bises.len()];
				playing.rate = playing.bises[0].config.sample_rate;
				playing.channels = playing.bises.len() as u8;
				playing.stage = Stage::Playing;
				for handle in established.handles {
					self.controllers[at].command(le_iso::opcode::LE_SETUP_ISO_DATA_PATH, &le_iso::setup_iso_data_path(handle, le_iso::Direction::Output));
				}
				self.broadcast_scan_off();
				self.broadcast_offer();
			}
			le_iso::Event::SyncLost { sync } => {
				if self.broadcast.playing.as_ref().is_some_and(|playing| playing.sync == Some(sync)) {
					print(b"BluetoothService: the broadcast's periodic train was lost\n");
					self.broadcast_end(false);
				}
			}
			le_iso::Event::BigLost { big, .. } => {
				if big == BIG && self.broadcast.playing.is_some() {
					print(b"BluetoothService: the broadcast's BIG was lost\n");
					if let Some(playing) = self.broadcast.playing.as_mut() {
						playing.handles.clear();
					}
					self.broadcast_end(true);
				}
			}
			le_iso::Event::CisEstablished(_) => {}
		}
	}

	// THE BROADCAST OFFERED to AudioService as a route.
	fn broadcast_offer(&mut self) {
		let Some(playing) = self.broadcast.playing.as_ref() else { return };
		let at = playing.at;
		let name = self.broadcast.sources.iter().find(|(source, _)| source.broadcast_id == playing.broadcast_id).map(|(source, _)| source.name.clone()).unwrap_or_default();
		let peer = self.broadcast.sources.iter().find(|(source, _)| source.broadcast_id == playing.broadcast_id).map(|(source, _)| source.address.clone()).unwrap_or(PeerAddress { kind: PeerKind::Public, bytes: alloc::vec![0; 6] });
		let endpoint = AudioEndpoint { id: 0, peer, name, kind: AudioEndpointKind::Route, rate: playing.rate, channels: playing.channels, latency_us: 40_000, hardware_volume: false, volume: 100 };
		let route = self.audio.offer(endpoint, at, 0);
		if let Some(playing) = self.broadcast.playing.as_mut() {
			playing.route = route;
		}
		print(b"BluetoothService: a broadcast is offered to AudioService as a route\n");
	}

	pub(crate) fn broadcast_handle(&self, at: usize, handle: u16) -> bool {
		self.broadcast.playing.as_ref().is_some_and(|playing| playing.at == at && playing.handles.contains(&handle))
	}

	pub(crate) fn broadcast_owns(&self, id: u32) -> bool {
		self.broadcast.playing.as_ref().is_some_and(|playing| playing.route == Some(id))
	}

	// AN SDU FROM A BIS: decoded, and once every stream's frame is in, interleaved into the route.
	pub(crate) fn broadcast_sdu(&mut self, sdu: &le_iso::Sdu) {
		let Some(playing) = self.broadcast.playing.as_mut().filter(|playing| playing.stage == Stage::Playing) else { return };
		let Some(slot) = playing.handles.iter().position(|handle| *handle == sdu.handle) else { return };
		let config = playing.bises[slot].config;
		let frame = config.sample_rate as usize * config.frame_us as usize / 1_000_000;
		let mut pcm = alloc::vec![0i16; frame];
		let data = (sdu.valid && sdu.data.len() >= usize::from(config.octets)).then(|| &sdu.data[..usize::from(config.octets)]);
		if playing.decoders.get_mut(slot).is_none_or(|decoder| decoder.decode(data, &mut pcm).is_err()) {
			pcm.fill(0);
		}
		playing.frames[slot] = Some(pcm);
		if playing.frames.iter().all(Option::is_some) {
			let frames: Vec<Vec<i16>> = playing.frames.iter_mut().map(|frame| frame.take().unwrap_or_default()).collect();
			for index in 0..frame {
				for channel in &frames {
					playing.queue.push_back(channel.get(index).copied().unwrap_or(0));
				}
			}
			let hold = playing.rate as usize * usize::from(playing.channels) * ROUTE_HOLD_MS / 1000;
			while playing.queue.len() > hold {
				playing.queue.pop_front();
			}
			if let Some(id) = playing.route {
				self.audio_capture_ready(id);
			}
		}
	}

	// A period of the route, where one is held.
	pub(crate) fn broadcast_take_period(&mut self, samples: usize) -> Option<Vec<u8>> {
		let playing = self.broadcast.playing.as_mut()?;
		if playing.queue.len() < samples {
			return None;
		}
		let mut out = Vec::with_capacity(samples * 2);
		for sample in playing.queue.drain(..samples) {
			out.extend_from_slice(&sample.to_le_bytes());
		}
		Some(out)
	}
}
