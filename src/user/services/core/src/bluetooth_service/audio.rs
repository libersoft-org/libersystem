// BLUETOOTH'S AUDIO, FOR AUDIOSERVICE: the `bluetooth-audio` root.
//
// THE ENDPOINTS ARE THE PROFILES', THE CONTRACT IS HERE. A2DP, HFP and LE Audio each offer what their links can carry
// - headphones and speakers as outputs, a headset's call audio as a voice endpoint, a phone playing to this system as
// the A2DP-sink route - through `offer`, and take it back through `withdraw`; AudioService, the one subscriber,
// hears each as it comes and goes on its endpoints stream, opens each as a PCM channel on the device-side contract a
// sound card speaks, sends levels down and hears a device's own back, and relays a voice session's call state out and
// a headset's commands in.
//
// ONE SUBSCRIBER, AND IT IS NEVER A ROLE. AudioService resolves this root by name through the broker, so stopping the
// radio's stack stops no audio and no audio waits for the radio; a second subscriber is refused, because two holders
// of one device's PCM channel would each be told the other's periods were played. When the subscriber's stream ends,
// every endpoint is closed with it, and a new subscriber is offered what is still there.
//
// THE CALL RELAY'S DEFAULT IS NO CALL. Until a session declares one, the Audio Gateway reports no call and answers a
// headset's call commands with ERROR - which is the profiles' to do, from `call`.

use super::*;
use driver_protocol::audio::{CMD_CAPTURE, CMD_CAPTURE_STOP, CMD_FORMAT, DeviceFormat, PcmFormat};
use proto::system::{AudioEndpoint, AudioEndpointKind, AudioEvent, BtCallCommand, BtCallState, EndpointVolume, bluetooth_audio};
use service_logic::audio_routing::TimerPacer;

// A Bluetooth endpoint's period: ten milliseconds of its own format, as AudioService drives it.
pub(crate) fn period_bytes(rate: u32, channels: u8) -> usize {
	(rate as usize / 100) * usize::from(channels) * 2
}

// ONE OPENED ENDPOINT'S PCM CHANNEL, on the device-side contract a sound card speaks: which link's stream it is, its
// format, and the request it holds - an output period whose acknowledgment waits for the timer, or a capture waiting
// for the phone's next packet.
pub(crate) struct Pcm {
	pub chan: u64,
	pub endpoint: u32,
	pub at: usize,
	pub handle: u16,
	pub kind: AudioEndpointKind,
	pub rate: u32,
	pub channels: u8,
	pub latency_us: u32,
	pub hardware_volume: bool,
	pub volume: u8,
	pub pacer: Option<TimerPacer>,
	// The clock tick an output period's acknowledgment is due at.
	pub ack_at: Option<u64>,
	pub capture_waiting: bool,
}

impl Pcm {
	// Whether the channel holds a request it has not answered: it is not read again until it has.
	pub fn holding(&self) -> bool {
		self.ack_at.is_some() || self.capture_waiting
	}
}

// The endpoints offered at once: a headset's output and voice, earbuds', a phone's route - bounded like everything a
// link carries.
const MAX_ENDPOINTS: usize = 16;

pub(crate) struct AudioRoot {
	// The producer end of the one subscriber's endpoints stream, zero with none.
	pub subscriber: u64,
	pub endpoints: Vec<AudioEndpoint>,
	// Which link each endpoint is a stream on: its id, the controller and the link's handle.
	pub owners: Vec<(u32, usize, u16)>,
	// The endpoints AudioService has opened.
	pub pcm: Vec<Pcm>,
	pub next_id: u32,
	// The call state AudioService last relayed.
	pub call: BtCallState,
}

impl AudioRoot {
	pub const fn new() -> AudioRoot {
		AudioRoot { subscriber: 0, endpoints: Vec::new(), owners: Vec::new(), pcm: Vec::new(), next_id: 1, call: BtCallState::None }
	}

	pub fn owner(&self, id: u32) -> Option<(usize, u16)> {
		self.owners.iter().find(|(held, _, _)| *held == id).map(|(_, at, handle)| (*at, *handle))
	}

	// One event to the subscriber; a subscriber that has gone is let go, and with it every endpoint's channel.
	fn send(&mut self, event: &AudioEvent) {
		if self.subscriber == 0 {
			return;
		}
		let mut frame = [0u8; 256];
		let mut handles = wire::Handles::new();
		let Some(len) = bluetooth_audio::endpoints_frame(0, event, &mut frame, &mut handles) else { return };
		if matches!(try_send_outcome(self.subscriber, &frame[..len], 0), SendOutcome::Failed) {
			close(self.subscriber);
			self.subscriber = 0;
			self.call = BtCallState::None;
		}
	}

	// A PROFILE OFFERS AN ENDPOINT, a stream on the link at `handle`: numbered here, never reused while this instance
	// runs, and told to the subscriber. `None` when the bound is reached.
	pub fn offer(&mut self, mut endpoint: AudioEndpoint, at: usize, handle: u16) -> Option<u32> {
		if self.endpoints.len() >= MAX_ENDPOINTS {
			return None;
		}
		endpoint.id = self.next_id;
		self.next_id = self.next_id.wrapping_add(1).max(1);
		let id = endpoint.id;
		self.endpoints.push(endpoint.clone());
		self.owners.push((id, at, handle));
		self.send(&AudioEvent::Arrived(endpoint));
		Some(id)
	}

	// THE ENDPOINT IS GONE: a link that dropped, a profile that disconnected - a device leaving, which AudioService's
	// routing rule answers. Its PCM channel closes with it.
	pub fn withdraw(&mut self, id: u32) {
		let before = self.endpoints.len();
		self.endpoints.retain(|endpoint| endpoint.id != id);
		self.owners.retain(|(held, _, _)| *held != id);
		self.pcm.retain(|pcm| {
			let mine = pcm.endpoint == id;
			if mine {
				close(pcm.chan);
			}
			!mine
		});
		if self.endpoints.len() != before {
			self.send(&AudioEvent::Departed(id));
		}
	}

	// A DEVICE CHANGED ITS OWN LEVEL - AVRCP absolute volume, HFP's gains, LE Volume Control.
	pub fn volume_changed(&mut self, id: u32, volume: u8) {
		if let Some(endpoint) = self.endpoints.iter_mut().find(|endpoint| endpoint.id == id) {
			endpoint.volume = volume.min(100);
			self.send(&AudioEvent::Volume(EndpointVolume { id, volume: volume.min(100) }));
		}
	}

	// A HEADSET'S CALL COMMAND, relayed when a session declares a call; refused - false, which the profile answers with
	// ERROR - when none does.
	#[allow(dead_code)]
	pub fn command(&mut self, command: BtCallCommand) -> bool {
		if self.call == BtCallState::None || self.subscriber == 0 {
			return false;
		}
		self.send(&AudioEvent::Command(command));
		true
	}
}

// THE SUBSCRIBER'S CALLS.
pub(crate) struct AudioView<'a> {
	pub stack: &'a mut Stack,
}

impl bluetooth_audio::Service for AudioView<'_> {
	// Validated here; the stream itself is made by `serve_endpoints`, which owns the channel.
	fn endpoints(&mut self) -> Result<Vec<AudioEvent>, Error> {
		if self.stack.audio.subscriber != 0 {
			return Err(Error::Again);
		}
		Ok(self.stack.audio.endpoints.iter().cloned().map(AudioEvent::Arrived).collect())
	}

	// THE PCM CHANNEL OF AN ENDPOINT: one holder, served here on the device-side contract. An endpoint not offered is not
	// found, and one already open is busy.
	fn open(&mut self, id: u32) -> Result<u64, Error> {
		let audio = &mut self.stack.audio;
		let endpoint = audio.endpoints.iter().find(|endpoint| endpoint.id == id).cloned().ok_or(Error::NotFound)?;
		let (at, handle) = audio.owner(id).ok_or(Error::NotFound)?;
		if audio.pcm.iter().any(|pcm| pcm.endpoint == id) {
			return Err(Error::Again);
		}
		let (server, client) = channel().ok_or(Error::Exhausted)?;
		audio.pcm.push(Pcm { chan: server, endpoint: id, at, handle, kind: endpoint.kind, rate: endpoint.rate, channels: endpoint.channels, latency_us: endpoint.latency_us, hardware_volume: endpoint.hardware_volume, volume: endpoint.volume, pacer: None, ack_at: None, capture_waiting: false });
		Ok(client)
	}

	// A LEVEL FOR A DEVICE THAT LEVELS ITSELF: sent to it by the profile that drives it.
	fn set_volume(&mut self, id: u32, volume: u8) -> Result<(), Error> {
		if volume > 100 {
			return Err(Error::Invalid);
		}
		let endpoint = self.stack.audio.endpoints.iter().find(|endpoint| endpoint.id == id).ok_or(Error::NotFound)?;
		if !endpoint.hardware_volume {
			return Err(Error::Unsupported);
		}
		let (at, handle) = self.stack.audio.owner(id).ok_or(Error::NotFound)?;
		self.stack.a2dp_set_volume(at, handle, volume)?;
		if let Some(endpoint) = self.stack.audio.endpoints.iter_mut().find(|endpoint| endpoint.id == id) {
			endpoint.volume = volume;
		}
		Ok(())
	}

	fn set_call(&mut self, state: BtCallState) -> Result<(), Error> {
		self.stack.audio.call = state;
		Ok(())
	}
}

// `endpoints` IS A STREAM: the snapshot of what is offered now, then every change, on the consumer end of a fresh pair
// whose producer this service keeps.
pub(crate) fn serve_endpoints(stack: &mut Stack, channel: u64, request: &[u8], handles: &mut wire::Handles, reply: &mut [u8]) {
	let mut view = AudioView { stack };
	let Some((corr, result)) = bluetooth_audio::endpoints_open(&mut view, request, handles) else { return };
	let answer = match result {
		Ok(snapshot) => match channel_with_depth(64) {
			Some((producer, consumer)) => {
				if let Some(len) = bluetooth_audio::endpoints_reply_ok(corr, reply)
					&& send_caps_blocking(channel, &reply[..len], &[consumer])
				{
					stack.audio.subscriber = producer;
					stack.audio.call = BtCallState::None;
					for event in &snapshot {
						stack.audio.send(event);
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
	if let Some(len) = bluetooth_audio::endpoints_reply_err(corr, &answer, reply) {
		send_blocking(channel, &reply[..len], 0);
	}
}

// ------------------------------------------------------------------ the PCM channels

impl Stack {
	// ONE REQUEST ON AN OPENED ENDPOINT'S CHANNEL: its format, a period to play - acknowledged on the timer - the end of
	// playback, or a period of a phone's audio to capture.
	pub(crate) fn serve_pcm(&mut self, index: usize, buf: &mut [u8]) {
		let chan = self.audio.pcm[index].chan;
		let (len, handles) = match try_recv_caps(chan, buf) {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => return,
			PolledCaps::Closed => {
				let pcm = self.audio.pcm.remove(index);
				close(pcm.chan);
				if pcm.kind == AudioEndpointKind::Output {
					self.a2dp_stop(pcm.at, pcm.handle);
				}
				return;
			}
		};
		for &handle in handles.as_slice() {
			close(handle);
		}
		let pcm = &self.audio.pcm[index];
		let (at, handle, kind) = (pcm.at, pcm.handle, pcm.kind);
		let period = period_bytes(pcm.rate, pcm.channels);
		let reply: Option<Vec<u8>> = match (len, buf.first().copied()) {
			// THE END OF PLAYBACK: the stream is suspended and the clock starts again with the next period.
			(0, _) => {
				self.audio.pcm[index].pacer = None;
				if kind == AudioEndpointKind::Output {
					self.a2dp_stop(at, handle);
				}
				Some(driver_protocol::audio::OK.to_vec())
			}
			(1, Some(CMD_FORMAT)) => {
				let pcm = &self.audio.pcm[index];
				let format = PcmFormat { rate: pcm.rate, channels: pcm.channels };
				let (output, input) = match kind {
					AudioEndpointKind::Output => (Some(format), None),
					AudioEndpointKind::Input | AudioEndpointKind::Route => (None, Some(format)),
					AudioEndpointKind::Voice => (Some(format), Some(format)),
				};
				let answer = DeviceFormat { output, input, period_bytes: period as u32, latency_us: pcm.latency_us, hardware_volume: pcm.hardware_volume, volume: pcm.volume };
				Some(answer.encode().to_vec())
			}
			(1, Some(CMD_CAPTURE)) if matches!(kind, AudioEndpointKind::Route | AudioEndpointKind::Input) => {
				let samples = period / 2;
				match self.a2dp_take_period(at, handle, samples) {
					Some(bytes) => Some(bytes),
					None => {
						self.audio.pcm[index].capture_waiting = true;
						None
					}
				}
			}
			(1, Some(CMD_CAPTURE_STOP)) => Some(driver_protocol::audio::OK.to_vec()),
			// A PERIOD TO PLAY, encoded and sent now; its acknowledgment waits for the timer, a little ahead of it.
			(len, _) if len == period && matches!(kind, AudioEndpointKind::Output | AudioEndpointKind::Voice) => {
				let samples: Vec<i16> = buf[..len].chunks_exact(2).map(|pair| i16::from_le_bytes([pair[0], pair[1]])).collect();
				self.a2dp_play(at, handle, &samples);
				let pcm = &mut self.audio.pcm[index];
				let frames = (period / (2 * usize::from(pcm.channels))) as u64;
				let rate = pcm.rate;
				let pacer = pcm.pacer.get_or_insert_with(|| a2dp::pacer(rate));
				pacer.take(frames);
				let due = a2dp::ack_tick(pacer);
				if due <= clock() {
					Some(driver_protocol::audio::OK.to_vec())
				} else {
					pcm.ack_at = Some(due);
					None
				}
			}
			// Anything else - the counters, a level, a length this endpoint has no shape for - is refused.
			_ => Some(Vec::new()),
		};
		if let Some(reply) = reply {
			send_blocking(chan, &reply, 0);
		}
	}

	// THE TIMER'S ACKNOWLEDGMENTS that have come due.
	pub(crate) fn pcm_timers(&mut self) {
		let now = clock();
		for pcm in self.audio.pcm.iter_mut() {
			if pcm.ack_at.is_some_and(|due| due <= now) {
				pcm.ack_at = None;
				send_blocking(pcm.chan, driver_protocol::audio::OK, 0);
			}
		}
	}

	pub(crate) fn pcm_deadline(&self) -> Option<u64> {
		self.audio.pcm.iter().filter_map(|pcm| pcm.ack_at).min()
	}

	// A PHONE'S AUDIO ARRIVED: a capture waiting on the route answered with a period, if a whole one is held.
	pub(crate) fn audio_capture_ready(&mut self, id: u32) {
		let Some(index) = self.audio.pcm.iter().position(|pcm| pcm.endpoint == id && pcm.capture_waiting) else { return };
		let pcm = &self.audio.pcm[index];
		let (at, handle, samples, chan) = (pcm.at, pcm.handle, period_bytes(pcm.rate, pcm.channels) / 2, pcm.chan);
		if let Some(bytes) = self.a2dp_take_period(at, handle, samples) {
			self.audio.pcm[index].capture_waiting = false;
			send_blocking(chan, &bytes, 0);
		}
	}
}
