// Exact production methods are inserted below. L2CAP allocation/close bookkeeping is real.
// Controller, SDP response, HID decoding, codec and IPC boundaries are explicit spies.
// This proves service retirement/reconnection decisions, not radio negotiation or HID parsing.
extern crate alloc;
/* EXACT_MODULES */
mod classic {
	pub(crate) use super::fixture::Purpose;
}
mod fixture {
	use super::l2cap_bredr::{self, Channels, Command, Ertm, Mode, Signal, psm};
	use std::cell::RefCell;
	use std::collections::VecDeque;

	type Peer = [u8; 7];
	const PEER: Peer = [0, 1, 2, 3, 4, 5, 6];
	const HANDLE: u16 = 11;
	const DEFAULT_SINK_DELAY_US: u32 = 150_000;
	const HIDP_SET_PROTOCOL_BOOT: u8 = 0x70;
	#[derive(Clone, Copy, Debug, PartialEq, Eq)]
	enum Error {
		NotFound,
		Unsupported,
		Closed,
	}
	#[derive(Clone, Copy, Debug, PartialEq, Eq)]
	pub(crate) enum Profile {
		Input,
		Audio,
		Voice,
		Pan,
		Spp,
		Gatt,
	}
	mod bt_policy {
		pub(crate) use super::Profile;
		#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
		pub struct Trust(u8);
		impl Trust {
			pub fn with(self, profile: Profile, connected: bool) -> Self {
				let bit = 1 << (profile as u8);
				Self(if connected { self.0 | bit } else { self.0 & !bit })
			}
			pub fn has(self, profile: Profile) -> bool {
				self.0 & (1 << profile as u8) != 0
			}
		}
	}
	fn profile_to_logic(profile: Profile) -> Profile {
		profile
	}
	fn peer_is_classic(peer: &Peer) -> bool {
		peer[0] == 0
	}
	fn peer_to_wire(peer: &Peer) -> [u8; 6] {
		peer[1..].try_into().unwrap()
	}
	fn print(_: &[u8]) {}

	#[derive(Clone, Debug, PartialEq, Eq)]
	struct MouseReport {
		dx: i32,
		dy: i32,
		wheel: i32,
		buttons: u8,
	}
	#[derive(Clone, Debug, PartialEq, Eq)]
	enum InputReport {
		KeyReleased,
		GamepadDeparted,
		Arrived(u32),
		Pointer(MouseReport),
	}
	#[derive(Clone, Debug, PartialEq, Eq)]
	enum Trace {
		Report(u64, InputReport),
		Close(u64),
		Signal(u16, Signal),
		Audio(AudioEvent),
		Page(Peer),
		Push(u16),
		Rfcomm(u16, Vec<RfOut>),
		PanDisconnect(Peer),
	}
	thread_local! {
		static TRACE: RefCell<Vec<Trace>> = RefCell::new(Vec::new());
		static DECODE_ID: RefCell<u32> = const { RefCell::new(10) };
	}
	fn trace(event: Trace) {
		TRACE.with_borrow_mut(|t| t.push(event));
	}
	fn events() -> Vec<Trace> {
		TRACE.with_borrow(Clone::clone)
	}
	fn clear() {
		TRACE.with_borrow_mut(Vec::clear);
	}
	fn close(chan: u64) {
		trace(Trace::Close(chan));
	}
	fn write_input(stream: u64, report: &InputReport) -> bool {
		trace(Trace::Report(stream, report.clone()));
		true
	}
	mod input {
		use super::*;
		// The decoder spy supplies known held reports, making the service's exact
		// release_streams ordering observable without claiming a HID parser test.
		pub struct Decoder {
			pub held: bool,
			pub generation: u32,
		}
		impl Decoder {
			pub fn new(bytes: &[u8]) -> Option<Self> {
				if bytes.is_empty() {
					return None;
				}
				let generation = DECODE_ID.with_borrow_mut(|id| {
					*id += 1;
					*id
				});
				Some(Self { held: true, generation })
			}
			pub fn boot_keyboard() -> Self {
				Self::new(&[1]).unwrap()
			}
			pub fn boot_mouse() -> Self {
				Self::new(&[2]).unwrap()
			}
			pub fn releases(&mut self) -> Vec<InputReport> {
				let held = self.held;
				self.held = false;
				if held { vec![InputReport::KeyReleased, InputReport::GamepadDeparted] } else { vec![] }
			}
			pub fn arrivals(&self) -> Vec<InputReport> {
				vec![InputReport::Arrived(self.generation)]
			}
		}
	}
	#[derive(Clone, Copy)]
	enum Operation {
		Play,
	}
	mod sbc {
		#[derive(Clone, Copy)]
		pub struct Config {
			pub frequency: u32,
			pub mode: Mode,
		}
		#[derive(Clone, Copy)]
		pub struct Mode;
		impl Config {
			pub fn samples(&self) -> usize {
				128
			}
		}
		impl Mode {
			pub fn channels(&self) -> usize {
				2
			}
		}
		pub struct Encoder;
		pub struct Decoder;
	}
	#[derive(Clone, Copy, Debug, PartialEq, Eq)]
	enum AudioEndpointKind {
		Output,
		Route,
		Voice,
	}
	#[derive(Clone, Debug, PartialEq, Eq)]
	struct AudioEndpoint {
		id: u32,
		peer: [u8; 6],
		name: String,
		kind: AudioEndpointKind,
		rate: u32,
		channels: u8,
		latency_us: u32,
		hardware_volume: bool,
		volume: u8,
	}
	#[derive(Clone, Debug, PartialEq, Eq)]
	enum AudioEvent {
		Arrived(AudioEndpoint),
		Departed(u32),
	}
	struct Pcm {
		endpoint: u32,
		chan: u64,
	}
	struct AudioRoot {
		endpoints: Vec<AudioEndpoint>,
		owners: Vec<(u32, usize, u16)>,
		pcm: Vec<Pcm>,
		next_id: u32,
	}
	impl AudioRoot {
		fn new() -> Self {
			Self { endpoints: vec![], owners: vec![], pcm: vec![], next_id: 1 }
		}
		fn send(&mut self, event: &AudioEvent) {
			trace(Trace::Audio(event.clone()));
		}
	}
	mod sdp {
		pub mod uuid {
			pub const SERIAL_PORT: u16 = 0x1101;
			pub const HUMAN_INTERFACE_DEVICE: u16 = 0x1124;
			pub const AUDIO_SINK: u16 = 0x110b;
			pub const NAP: u16 = 0x1116;
		}
		pub enum Uuid {
			U16(u16),
		}
		pub struct Record {
			pub descriptor: Vec<u8>,
		}
		impl Record {
			pub fn rfcomm_channel(&self) -> Option<u8> {
				Some(7)
			}
		}
		pub struct Search;
		pub enum Answer {
			More(Vec<u8>),
			Records(Vec<Record>),
			Failed(()),
		}
		impl Search {
			pub fn new(_: u16, _: Uuid) -> (Self, Vec<u8>) {
				(Self, vec![1])
			}
			pub fn answer(&mut self, _: &[u8]) -> Answer {
				Answer::Records(vec![Record { descriptor: vec![1] }])
			}
		}
	}
	use sdp::{Answer, Search, Uuid};
	fn hid_descriptor(record: &sdp::Record) -> Option<Vec<u8>> {
		Some(record.descriptor.clone())
	}
	struct SdpClient {
		cid: u16,
		search: Option<Search>,
		uuid: u16,
		purpose: Purpose,
	}
	#[derive(Clone, Debug, PartialEq, Eq)]
	enum RfOut {
		Closed(u8),
	}
	struct Session;
	impl Session {
		fn new(_: bool, _: usize) -> Self {
			Self
		}
		fn connect(&mut self, channel: u8) -> Option<Vec<RfOut>> {
			Some(vec![RfOut::Closed(channel)])
		}
		fn disconnect(&mut self, channel: u8) -> Vec<RfOut> {
			vec![RfOut::Closed(channel)]
		}
	}
	struct Rfcomm {
		cid: u16,
		session: Session,
		opening: Vec<(u8, Option<Profile>)>,
		open: Vec<(u8, Profile)>,
		held: Vec<(u8, VecDeque<u8>)>,
	}
	struct ClassicLink {
		channels: Channels,
		ertm: Vec<(u16, Ertm)>,
		sdp_client: Option<SdpClient>,
		rfcomm: Option<Rfcomm>,
		hid: Hid,
		profiles: bt_policy::Trust,
		a2dp: A2dp,
	}
	impl ClassicLink {
		fn new() -> Self {
			Self { channels: Channels::new(), ertm: vec![], sdp_client: None, rfcomm: None, hid: Hid::default(), profiles: bt_policy::Trust::default(), a2dp: A2dp::default() }
		}
	}
	struct Link {
		handle: u16,
		peer: Peer,
		encrypted: bool,
		classic: Option<ClassicLink>,
		decoder: Option<input::Decoder>,
		report: Option<()>,
		streams: Vec<u64>,
		buttons: u8,
	}
	impl Link {
		fn new() -> Self {
			Self { handle: HANDLE, peer: PEER, encrypted: true, classic: Some(ClassicLink::new()), decoder: None, report: None, streams: vec![], buttons: 0 }
		}
	}
	struct ControllerState {
		wants: Vec<(Peer, Want)>,
		transaction: u16,
	}
	impl ControllerState {
		fn is_pointer(&self, _: &Peer) -> bool {
			false
		}
		fn name_of(&self, _: &Peer) -> Option<String> {
			Some("fixture-peer".into())
		}
	}
	struct Controller {
		links: Vec<Link>,
		powered: bool,
		classic: bool,
		bredr_state: ControllerState,
	}
	impl Controller {
		fn link(&self, handle: u16) -> Option<&Link> {
			self.links.iter().find(|l| l.handle == handle)
		}
		fn link_mut(&mut self, handle: u16) -> Option<&mut Link> {
			self.links.iter_mut().find(|l| l.handle == handle)
		}
		fn link_to(&self, peer: &Peer) -> Option<&Link> {
			self.links.iter().find(|l| &l.peer == peer)
		}
		fn link_to_mut(&mut self, peer: &Peer) -> Option<&mut Link> {
			self.links.iter_mut().find(|l| &l.peer == peer)
		}
		fn l2cap(&mut self, _: u16, _: u16, _: &[u8]) {}
	}
	struct Stack {
		controllers: Vec<Controller>,
		audio: AudioRoot,
	}
	impl Stack {
		fn new(link: bool) -> Self {
			Self { controllers: vec![Controller { links: if link { vec![Link::new()] } else { vec![] }, powered: true, classic: true, bredr_state: ControllerState { wants: vec![], transaction: 1 } }], audio: AudioRoot::new() }
		}
		fn record(&self, _: usize, peer: &Peer) -> Option<()> {
			(*peer == PEER).then_some(())
		}
		fn page(&mut self, _: usize, peer: Peer) -> Result<(), Error> {
			trace(Trace::Page(peer));
			Ok(())
		}
		fn send_signal(&mut self, _: usize, handle: u16, signal: &Signal) {
			trace(Trace::Signal(handle, signal.clone()));
		}
		fn send_on(&mut self, _: usize, _: u16, _: u16, _: &[u8]) {}
		fn start_push_search(&mut self, _: usize, handle: u16) {
			trace(Trace::Push(handle));
		}
		fn push_found(&mut self, _: usize, _: u16, _: &[sdp::Record]) {}
		fn serial_closed(&mut self, _: usize, _: &Peer, _: Error) {}
		fn rfcomm_connect(&mut self, _: usize, _: u16, _: u8, _: Option<Profile>) {}
		fn run_rfcomm(&mut self, _: usize, handle: u16, outs: Vec<RfOut>) {
			trace(Trace::Rfcomm(handle, outs));
		}
		fn disconnect_pan(&mut self, _: usize, peer: &Peer) -> Result<(), Error> {
			trace(Trace::PanDisconnect(*peer));
			Ok(())
		}
		fn pan_found(&mut self, _: usize, _: u16, _: &[sdp::Record]) {}
		fn pan_opened(&mut self, _: usize, _: u16, _: u16) {}
		fn pan_closed(&mut self, _: usize, _: u16, _: u16) {}
		fn opp_l2cap_opened(&mut self, _: usize, _: u16, _: u16, _: usize) {}
		fn opp_l2cap_closed(&mut self, _: usize, _: u16, _: u16) {}
		// Untested AVDTP negotiation is a boundary; retirement/offer methods are exact.
		fn a2dp_channel_opened(&mut self, _: usize, _: u16, _: u16, _: u16) {}
	}

	/* EXACT_PRODUCTION */

	fn link(stack: &Stack) -> &Link {
		stack.controllers[0].link(HANDLE).unwrap()
	}
	fn link_mut(stack: &mut Stack) -> &mut Link {
		stack.controllers[0].link_mut(HANDLE).unwrap()
	}
	fn classic(stack: &Stack) -> &ClassicLink {
		link(stack).classic.as_ref().unwrap()
	}
	fn classic_mut(stack: &mut Stack) -> &mut ClassicLink {
		link_mut(stack).classic.as_mut().unwrap()
	}
	fn channel(stack: &mut Stack, psm: u16) -> u16 {
		classic_mut(stack).channels.open(psm, false).unwrap().0
	}
	fn closed() -> Vec<u16> {
		events()
			.iter()
			.filter_map(|e| match e {
				Trace::Signal(HANDLE, Signal { command: Command::DisconnectionRequest { scid, .. }, .. }) => Some(*scid),
				_ => None,
			})
			.collect()
	}
	fn opened(psm: u16) -> Vec<u16> {
		events()
			.iter()
			.filter_map(|e| match e {
				Trace::Signal(HANDLE, Signal { command: Command::ConnectionRequest { psm: held, scid }, .. }) if *held == psm => Some(*scid),
				_ => None,
			})
			.collect()
	}
	fn ack_closes(stack: &mut Stack) {
		let sent = events();
		for event in sent {
			if let Trace::Signal(HANDLE, Signal { identifier, command: Command::DisconnectionRequest { dcid, scid } }) = event {
				let response = Signal { identifier, command: Command::DisconnectionResponse { dcid, scid } };
				let _ = classic_mut(stack).channels.on_signal(response, &mut |_| l2cap_bredr::Admission::NotSupported);
				stack.channel_closed(0, HANDLE, scid);
			}
		}
	}
	fn seed_siblings(stack: &mut Stack) {
		let mut bits = bt_policy::Trust::default();
		for p in [Profile::Input, Profile::Voice, Profile::Pan, Profile::Spp, Profile::Gatt] {
			bits = bits.with(p, true);
		}
		classic_mut(stack).profiles = bits;
		// Existing unrelated RFCOMM DLC and its held input must survive target retirement.
		classic_mut(stack).rfcomm = Some(Rfcomm { cid: 500, session: Session, opening: vec![], open: vec![(7, Profile::Spp), (8, Profile::Voice)], held: vec![(7, VecDeque::from([9]))] });
	}
	fn assert_siblings(stack: &Stack, retired: Profile) {
		assert_eq!(stack.controllers[0].links.len(), 1, "profile teardown must retain the ACL");
		assert!(link(stack).encrypted);
		for p in [Profile::Input, Profile::Audio, Profile::Voice, Profile::Pan, Profile::Spp, Profile::Gatt] {
			if p != retired {
				assert!(classic(stack).profiles.has(p), "sibling profile must remain connected: {p:?}");
			}
		}
		assert_eq!(classic(stack).rfcomm.as_ref().unwrap().open, vec![(7, Profile::Spp), (8, Profile::Voice)]);
		assert_eq!(classic(stack).rfcomm.as_ref().unwrap().held[0].1, VecDeque::from([9]));
	}
	fn seed_audio(stack: &mut Stack, outgoing: bool) -> (u16, u16, u16, u32) {
		let signalling = channel(stack, psm::AVDTP);
		let media = channel(stack, psm::AVDTP);
		let avctp = channel(stack, psm::AVCTP);
		classic_mut(stack).a2dp = A2dp { signalling: Some(signalling), media: Some(media), avctp: Some(avctp), outgoing, config: Some(sbc::Config { frequency: 48_000, mode: sbc::Mode }), state: State::Streaming, volume: Volume::Device(43), frames: vec![1, 2], route: VecDeque::from([7]), encoder: Some(Box::new(sbc::Encoder)), decoder: Some(Box::new(sbc::Decoder)), waiting_command: Some(Operation::Play), ..A2dp::default() };
		stack.a2dp_offer(0, HANDLE);
		let endpoint = classic(stack).a2dp.endpoint.unwrap();
		stack.audio.pcm.push(Pcm { endpoint, chan: 700 });
		(signalling, media, avctp, endpoint)
	}
	fn seed_input(stack: &mut Stack) -> (u16, u16, u32) {
		let control = channel(stack, psm::HID_CONTROL);
		let interrupt = channel(stack, psm::HID_INTERRUPT);
		let decoder = input::Decoder::new(&[1]).unwrap();
		let generation = decoder.generation;
		let held = link_mut(stack);
		held.decoder = Some(decoder);
		held.streams = vec![800, 801];
		held.buttons = 3;
		let classic = held.classic.as_mut().unwrap();
		classic.hid = Hid { control: Some(control), interrupt: Some(interrupt), searched: true, outgoing: false };
		classic.profiles = classic.profiles.with(Profile::Input, true);
		(control, interrupt, generation)
	}
	fn seed_search(stack: &mut Stack, profile: Profile) -> u16 {
		let cid = channel(stack, psm::SDP);
		classic_mut(stack).sdp_client = Some(SdpClient { cid, search: Some(Search), uuid: 1, purpose: Purpose::Profile(profile) });
		cid
	}
	fn operator_audio() {
		clear();
		let mut stack = Stack::new(true);
		seed_siblings(&mut stack);
		let (signalling, media, avctp, endpoint) = seed_audio(&mut stack, true);
		let extra = stack.audio.offer(AudioEndpoint { id: 0, peer: [9; 6], name: "voice".into(), kind: AudioEndpointKind::Voice, rate: 16_000, channels: 1, latency_us: 20_000, hardware_volume: true, volume: 37 }, 0, 99).unwrap();
		stack.audio.pcm.push(Pcm { endpoint: extra, chan: 701 });
		clear();
		assert_eq!(stack.disconnect_profile(0, &PEER, Profile::Audio), Ok(()), "operator Audio disconnect must succeed");
		assert_eq!(closed(), vec![signalling, media, avctp], "Audio disconnect must close signalling, media and AVRCP");
		assert_eq!(stack.audio.endpoints.iter().map(|e| e.id).collect::<Vec<_>>(), vec![extra]);
		assert_eq!(stack.audio.pcm.iter().map(|p| p.chan).collect::<Vec<_>>(), vec![701]);
		assert_eq!(events().iter().filter(|e| **e == Trace::Close(700)).count(), 1);
		assert_eq!(events().iter().filter(|e| **e == Trace::Audio(AudioEvent::Departed(endpoint))).count(), 1);
		assert!(!classic(&stack).profiles.has(Profile::Audio));
		assert_siblings(&stack, Profile::Audio);
		let state = &classic(&stack).a2dp;
		assert_eq!(state.state, State::Idle);
		assert!(state.frames.is_empty() && state.route.is_empty());
		assert!(state.signalling.is_none() && state.media.is_none() && state.avctp.is_none());
		let previous = events();
		for cid in [signalling, media, avctp] {
			stack.channel_closed(0, HANDLE, cid);
		}
		assert_eq!(events(), previous, "late channel-close callbacks must not repeat retirement");
	}
	fn audio_inventory() {
		for outgoing in [true, false] {
			clear();
			let mut stack = Stack::new(true);
			seed_siblings(&mut stack);
			let (signalling, media, avctp, _) = seed_audio(&mut stack, outgoing);
			assert!(connected_profiles(link(&stack)).contains(&Profile::Audio), "offered Audio endpoint must appear in connected inventory");
			clear();
			stack.channel_closed(0, HANDLE, media);
			assert!(!connected_profiles(link(&stack)).contains(&Profile::Audio), "withdrawn Audio endpoint must leave connected inventory");
			assert_eq!(closed(), vec![signalling]);
			assert_eq!(classic(&stack).a2dp.avctp, Some(avctp), "remote-control-only channel survives a media loss");
			assert_siblings(&stack, Profile::Audio);
			clear();
			assert_eq!(stack.disconnect_profile(0, &PEER, Profile::Audio), Ok(()), "remaining AVRCP-only Audio disconnect must succeed");
			assert_eq!(closed(), vec![avctp]);
			assert!(classic(&stack).a2dp.avctp.is_none());
			assert_siblings(&stack, Profile::Audio);
		}
		clear();
		let mut stack = Stack::new(true);
		seed_siblings(&mut stack);
		for handle in 0..MAX_ENDPOINTS {
			stack.audio.offer(AudioEndpoint { id: 0, peer: [9; 6], name: "occupied".into(), kind: AudioEndpointKind::Voice, rate: 16_000, channels: 1, latency_us: 20_000, hardware_volume: false, volume: 100 }, 0, handle as u16).unwrap();
		}
		let signalling = channel(&mut stack, psm::AVDTP);
		let media = channel(&mut stack, psm::AVDTP);
		classic_mut(&mut stack).a2dp = A2dp { signalling: Some(signalling), media: Some(media), outgoing: true, config: Some(sbc::Config { frequency: 48_000, mode: sbc::Mode }), ..A2dp::default() };
		clear();
		stack.a2dp_offer(0, HANDLE);
		assert!(classic(&stack).a2dp.endpoint.is_none() && !connected_profiles(link(&stack)).contains(&Profile::Audio), "refused bounded endpoint offer must not publish Audio ownership or inventory");
		assert_eq!(stack.audio.endpoints.len(), MAX_ENDPOINTS);
		assert!(events().is_empty());
	}
	fn assert_input_retired(stack: &Stack) {
		let expected = vec![
			Trace::Report(800, InputReport::KeyReleased),
			Trace::Report(801, InputReport::KeyReleased),
			Trace::Report(800, InputReport::GamepadDeparted),
			Trace::Report(801, InputReport::GamepadDeparted),
			Trace::Report(800, InputReport::Pointer(MouseReport { dx: 0, dy: 0, wheel: 0, buttons: 0 })),
			Trace::Report(801, InputReport::Pointer(MouseReport { dx: 0, dy: 0, wheel: 0, buttons: 0 })),
			Trace::Close(800),
			Trace::Close(801),
		];
		let actual: Vec<_> = events().into_iter().filter(|e| matches!(e, Trace::Report(..) | Trace::Close(800 | 801))).collect();
		assert_eq!(actual, expected, "held input reports must precede closing each stream");
		assert!(link(stack).streams.is_empty());
		assert_eq!(link(stack).buttons, 0);
		assert!(link(stack).decoder.is_none(), "retired HID decoder must not be visible to a new input consumer");
		assert!(classic(stack).hid.control.is_none() && classic(stack).hid.interrupt.is_none());
		assert!(!connected_profiles(link(stack)).contains(&Profile::Input));
	}
	fn operator_input() {
		clear();
		let mut stack = Stack::new(true);
		seed_siblings(&mut stack);
		seed_audio(&mut stack, true);
		let (control, interrupt, _) = seed_input(&mut stack);
		let search = seed_search(&mut stack, Profile::Input);
		clear();
		assert_eq!(stack.disconnect_profile(0, &PEER, Profile::Input), Ok(()));
		assert_eq!(closed(), vec![control, interrupt, search]);
		assert_input_retired(&stack);
		assert_siblings(&stack, Profile::Input);
		let previous = events();
		for cid in [control, interrupt, search] {
			stack.channel_closed(0, HANDLE, cid);
		}
		assert_eq!(events(), previous, "replayed HID close callbacks cannot release streams twice");
	}
	fn peer_input_close() {
		for first_control in [true, false] {
			clear();
			let mut stack = Stack::new(true);
			seed_siblings(&mut stack);
			seed_audio(&mut stack, true);
			let (control, interrupt, _) = seed_input(&mut stack);
			let search = seed_search(&mut stack, Profile::Input);
			clear();
			stack.channel_closed(0, HANDLE, if first_control { control } else { interrupt });
			assert_eq!(closed(), vec![if first_control { interrupt } else { control }, search]);
			assert_input_retired(&stack);
			assert_siblings(&stack, Profile::Input);
		}
	}
	fn late_sdp() {
		clear();
		let mut stack = Stack::new(true);
		seed_input(&mut stack);
		let cid = seed_search(&mut stack, Profile::Input);
		stack.disconnect_profile(0, &PEER, Profile::Input).unwrap();
		clear();
		// A new consumer observes the same ACL after retirement, while the old SDP reply arrives.
		link_mut(&mut stack).streams.push(802);
		stack.sdp_answer(0, HANDLE, cid, b"late descriptor");
		assert!(link(&stack).decoder.is_none() && events().is_empty(), "late SDP must not resurrect retired input");
	}
	fn answer_descriptor(stack: &mut Stack) {
		let cid = classic(stack).sdp_client.as_ref().unwrap().cid;
		stack.channel_opened(0, HANDLE, cid);
		stack.sdp_answer(0, HANDLE, cid, b"descriptor");
	}
	fn outbound_reconnect() {
		clear();
		let mut stack = Stack::new(true);
		let (_, _, old) = seed_input(&mut stack);
		stack.disconnect_profile(0, &PEER, Profile::Input).unwrap();
		ack_closes(&mut stack);
		clear();
		link_mut(&mut stack).streams.push(802);
		stack.connect_profile(0, &PEER, Profile::Input).unwrap();
		answer_descriptor(&mut stack);
		let control = *opened(psm::HID_CONTROL).last().expect("same-ACL reconnect must open fresh control");
		stack.channel_opened(0, HANDLE, control);
		let interrupt = *opened(psm::HID_INTERRUPT).last().expect("same-ACL reconnect must open fresh interrupt");
		stack.channel_opened(0, HANDLE, interrupt);
		assert!(connected_profiles(link(&stack)).contains(&Profile::Input));
		assert!(link(&stack).decoder.as_ref().unwrap().generation > old);
		assert!(events().iter().any(|e| matches!(e, Trace::Report(802, InputReport::Arrived(id)) if *id > old)));
		assert_eq!(stack.controllers[0].links.len(), 1);
	}
	fn inbound_reconnect() {
		clear();
		let mut stack = Stack::new(true);
		seed_input(&mut stack);
		stack.disconnect_profile(0, &PEER, Profile::Input).unwrap();
		ack_closes(&mut stack);
		clear();
		let control = channel(&mut stack, psm::HID_CONTROL);
		let interrupt = channel(&mut stack, psm::HID_INTERRUPT);
		stack.channel_opened(0, HANDLE, control);
		stack.channel_opened(0, HANDLE, interrupt);
		assert!(classic(&stack).sdp_client.is_some() && opened(psm::SDP).len() == 1, "fresh inbound HID must request its descriptor on the same ACL");
		assert!(!classic(&stack).hid.outgoing, "incoming HID must not be converted into an operator connection");
		answer_descriptor(&mut stack);
		assert!(connected_profiles(link(&stack)).contains(&Profile::Input));
	}
	fn cold_input() {
		clear();
		let mut stack = Stack::new(false);
		stack.connect_profile(0, &PEER, Profile::Input).unwrap();
		assert!(events().contains(&Trace::Page(PEER)));
		assert_eq!(stack.controllers[0].bredr_state.wants.len(), 1);
		let pending = std::mem::take(&mut stack.controllers[0].bredr_state.wants).into_iter().map(|(_, want)| want).collect();
		// Link creation/encryption is the controller boundary; resume executes the exact production dispatch block.
		stack.controllers[0].links.push(Link::new());
		stack.resume_wanted(0, HANDLE, pending);
		answer_descriptor(&mut stack);
		assert_eq!(opened(psm::HID_CONTROL).len(), 1, "queued cold Input connect must open HID control after SDP");
		stack.resume_wanted(0, HANDLE, vec![Want::Push]);
		assert!(events().contains(&Trace::Push(HANDLE)));
	}
	fn unrelated_search() {
		for profile in [Profile::Input, Profile::Audio] {
			clear();
			let mut stack = Stack::new(true);
			seed_siblings(&mut stack);
			seed_audio(&mut stack, true);
			seed_input(&mut stack);
			let cid = seed_search(&mut stack, Profile::Spp);
			clear();
			stack.disconnect_profile(0, &PEER, profile).unwrap();
			assert!(classic(&stack).sdp_client.as_ref().is_some_and(|s| s.cid == cid && s.purpose == Purpose::Profile(Profile::Spp)));
			assert!(!closed().contains(&cid), "retiring one profile cannot cancel another profile's lookup");
			assert_siblings(&stack, profile);
		}
	}
	fn audio_late_sdp() {
		clear();
		let mut stack = Stack::new(true);
		seed_audio(&mut stack, true);
		let cid = seed_search(&mut stack, Profile::Audio);
		stack.disconnect_profile(0, &PEER, Profile::Audio).unwrap();
		let search_closed = closed().contains(&cid);
		clear();
		stack.sdp_answer(0, HANDLE, cid, b"late sink");
		assert!(search_closed && opened(psm::AVDTP).is_empty() && classic(&stack).sdp_client.is_none(), "late SDP must not resurrect retired Audio");
	}
	fn errors_and_existing_profiles() {
		clear();
		let mut stack = Stack::new(true);
		assert_eq!(stack.disconnect_profile(0, &[0; 7], Profile::Audio), Err(Error::NotFound));
		assert_eq!(stack.disconnect_profile(0, &PEER, Profile::Audio), Err(Error::NotFound));
		assert_eq!(stack.disconnect_profile(0, &PEER, Profile::Input), Err(Error::NotFound));
		seed_siblings(&mut stack);
		clear();
		assert_eq!(stack.disconnect_profile(0, &PEER, Profile::Spp), Ok(()));
		assert_eq!(events(), vec![Trace::Rfcomm(HANDLE, vec![RfOut::Closed(7)])]);
		clear();
		assert_eq!(stack.disconnect_profile(0, &PEER, Profile::Pan), Ok(()));
		assert_eq!(events(), vec![Trace::PanDisconnect(PEER)]);
		link_mut(&mut stack).classic = None;
		assert_eq!(stack.disconnect_profile(0, &PEER, Profile::Audio), Err(Error::Unsupported));
	}
	pub fn main() {
		let selected = std::env::args().nth(1).unwrap_or_else(|| "all".into());
		let cases: &[(&str, fn())] = &[
			("operator-audio", operator_audio),
			("audio-inventory", audio_inventory),
			("operator-input", operator_input),
			("peer-input-close", peer_input_close),
			("late-sdp", late_sdp),
			("outbound-reconnect", outbound_reconnect),
			("inbound-reconnect", inbound_reconnect),
			("cold-input", cold_input),
			("unrelated-search", unrelated_search),
			("audio-late-sdp", audio_late_sdp),
			("errors-existing-profiles", errors_and_existing_profiles),
		];
		let mut count = 0;
		for &(name, run) in cases {
			if selected == "all" || selected == name {
				run();
				count += 1;
				println!("profile lifecycle {name}: PASS");
			}
		}
		assert!(count > 0, "unknown fixture case");
	}
}
fn main() {
	fixture::main();
}
