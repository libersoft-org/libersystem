// The actual AudioService consumes a broker-minted Bluetooth root. The kernel plays
// the publisher, using real bounded channels, without restarting AudioService or
// claiming to execute BluetoothService's own decision to retire a full stream.
use super::*;
use alloc::sync::Arc;
use audio_proto::generated::liber::audio::v1::{AudioDevice, AudioTransport, audio_control};
use bluetooth_proto::codec::Handles;
use bluetooth_proto::generated::liber::bluetooth::v1::{AudioEndpoint, AudioEndpointKind, AudioEvent, EndpointVolume, PeerAddress, PeerKind, bluetooth_audio};
use object::channel::{Channel, ChannelError, Message};
use object::rights::Rights;

fn receive(channel: &Channel, why: &str) -> Message {
	let deadline = arch::apic::ticks() + 1_000;
	loop {
		if let Ok(message) = channel.recv() {
			return message;
		}
		assert!(arch::apic::ticks() < deadline, "{why}");
		sched::run_until_idle();
	}
}

fn control(channel: &Channel) -> audio_control::Client<hardware::KernelTransport<'_>> {
	audio_control::Client::new(hardware::KernelTransport::new(channel, 1_000))
}

fn event(stream: &Channel, item: AudioEvent) -> Result<(), ChannelError> {
	let mut bytes = [0; 256];
	let mut handles = Handles::new();
	let n = bluetooth_audio::endpoints_frame(0, &item, &mut bytes, &mut handles).expect("the generated Bluetooth event encodes");
	assert!(handles.as_slice().is_empty());
	stream.send(Message::new(bytes[..n].to_vec(), Vec::new()))
}

fn endpoint(id: u32, kind: AudioEndpointKind, volume: u8) -> AudioEvent {
	AudioEvent::Arrived(AudioEndpoint { id, peer: PeerAddress { kind: PeerKind::Bredr, bytes: alloc::vec![1, 2, 3, 4, 5, 6] }, name: if id == 7 { "fixture output".into() } else { "fixture voice".into() }, kind, rate: 16_000, channels: 1, latency_us: 10_000, hardware_volume: true, volume })
}

fn snapshot(stream: &Channel, speaker: u8, microphone: u8) {
	event(stream, endpoint(7, AudioEndpointKind::Output, 100)).expect("output snapshot");
	event(stream, endpoint(8, AudioEndpointKind::Voice, speaker)).expect("voice snapshot");
	event(stream, AudioEvent::MicrophoneVolume(EndpointVolume { id: 8, volume: microphone })).expect("current microphone snapshot");
}

// Answer a real fresh resolve and subscription. The supplied stream is populated
// before its consumer capability is delivered, so capacity tests do not depend on
// whether another CPU happens to drain the AudioService event queue first.
fn subscribe(broker: &Channel, stream: Arc<Channel>) -> Arc<Channel> {
	let request = receive(broker, "AudioService must re-resolve BTAUDIO after the event stream closes");
	let mut expected = abi::RESOLVE_OP.to_le_bytes().to_vec();
	expected.extend_from_slice(b"BTAUDIO");
	assert_eq!(request.bytes, expected, "the actual service asks the broker for the correct authority");
	assert!(request.caps.is_empty());
	let (server, client) = Channel::create();
	send_cap(broker, b"OK", client, Rights::SEND | Rights::RECEIVE | Rights::WAIT | Rights::TRANSFER).expect("broker grants the new Bluetooth connection");
	let request = receive(&server, "the newly resolved connection must subscribe");
	assert_eq!(request.bytes.len(), 6);
	assert_eq!(le_u16(&request.bytes, 0), bluetooth_audio::OP_ENDPOINTS);
	let mut reply = [0; 16];
	let n = bluetooth_audio::endpoints_reply_ok(le_u32(&request.bytes, 2), &mut reply).expect("generated stream reply");
	send_cap(&server, &reply[..n], stream, Rights::SEND | Rights::RECEIVE | Rights::WAIT | Rights::TRANSFER).expect("the real event-stream capability is transferred");
	server
}

fn answer_output_open(root: &Channel) -> Arc<Channel> {
	let request = receive(root, "the offered output must be reopened on the current Bluetooth connection");
	assert_eq!(request.bytes.len(), 10);
	assert_eq!(le_u16(&request.bytes, 0), bluetooth_audio::OP_OPEN);
	assert_eq!(le_u32(&request.bytes, 6), 7);
	let (server, client) = Channel::create();
	let mut reply = le_u32(&request.bytes, 2).to_le_bytes().to_vec();
	reply.push(1);
	reply.extend_from_slice(&0u32.to_le_bytes());
	send_cap(root, &reply, client, Rights::SEND | Rights::RECEIVE | Rights::WAIT | Rights::TRANSFER).expect("the output's PCM capability is transferred");
	server
}

fn inventory(root: &Channel) -> Vec<AudioDevice> {
	control(root).devices().expect("the real AudioService control interface remains responsive")
}

fn assert_snapshot(root: &Channel, speaker: u8, microphone: u8) -> (u32, u32) {
	let deadline = arch::apic::ticks() + 1_000;
	loop {
		// Event and control channels are independent. A readiness scan may pass the
		// event slot just before an update arrives, then answer this control request
		// first. Wait for the required values, not a particular scheduling order.
		let rows = inventory(root);
		assert!(rows.len() <= 2, "no stale duplicate endpoint generations");
		if let (Some(output), Some(voice)) = (rows.iter().find(|device| device.label == "fixture output"), rows.iter().find(|device| device.label == "fixture voice")) {
			assert!(rows.iter().all(|device| device.transport == AudioTransport::Bluetooth));
			assert!(output.default_output && voice.default_voice && voice.hardware_volume);
			if voice.volume == speaker && control(root).microphone_volume(&voice.id) == Some(Ok(microphone)) {
				return (output.id, voice.id);
			}
		}
		assert!(arch::apic::ticks() < deadline, "the current independent speaker/microphone snapshot never settled: {rows:?}");
		sched::run_until_idle();
	}
}

fn wait_withdrawn(control_root: &Channel, old_root: &Channel, old_pcm: &Channel) {
	let deadline = arch::apic::ticks() + 1_000;
	loop {
		if inventory(control_root).is_empty() && old_root.is_peer_closed() && old_pcm.is_peer_closed() {
			return;
		}
		assert!(arch::apic::ticks() < deadline, "closed subscription must remove old devices, PCM and root before replacement");
		sched::run_until_idle();
	}
}

tagged_test!(audio_service_rebuilds_bluetooth_inventory_after_retired_event_streams, [Service, Audio, AudioService], id = "kernel.services.audio_service_rebuilds_bluetooth_inventory_after_retired_event_streams", covers = ["kernel", "bin.audio_service", "audio-proto", "bluetooth-proto"]);
fn audio_service_rebuilds_bluetooth_inventory_after_retired_event_streams() {
	let (volume, package) = scenario_packages().expect("the current staged service packages");
	let elf = program_elf(&package, volume, b"audio_service").expect("AudioService in the current package");
	let (broker, bootstrap) = Channel::create();
	let process = spawn_dynamic_test_process(sched::root_domain(), elf, bootstrap);
	let (_admin, admin_child) = Channel::create();
	let (_serve, serve_child) = Channel::create();
	let (control_root, control_child) = Channel::create();
	let rights = Rights::SEND | Rights::RECEIVE | Rights::WAIT | Rights::TRANSFER;
	send_cap(&broker, b"ADMIN", admin_child, rights).expect("audio admin role");
	send_cap(&broker, b"SERVE", serve_child, rights).expect("audio service role");
	for role in [b"CATALOGUE".as_slice(), b"LATENCY", b"STATS"] {
		broker.send(Message::new(role.to_vec(), Vec::new())).expect("explicitly absent optional role");
	}
	send_cap(&broker, b"CONTROL", control_child, rights).expect("audio control role");
	assert_eq!(receive(&broker, "AudioService online").bytes, b"AudioService: online");

	let (events, consumer) = Channel::try_create_with_depth(64).expect("bounded event channel");
	snapshot(&events, 75, 53);
	let root = subscribe(&broker, consumer);
	let pcm = answer_output_open(&root);
	let original = assert_snapshot(&control_root, 75, 53);
	assert!(!process.is_terminated());

	// Ordinary publisher loss: this is a live consumer reconnect, not a request to
	// restart the escalation-only AudioService through the supervisor.
	drop(events);
	wait_withdrawn(&control_root, &root, &pcm);

	// A replacement stream is already full when it arrives. The kernel channel
	// really refuses update65; closing the publisher models the production stack's
	// retirement decision, which is tested separately against its exact methods.
	let (events, consumer) = Channel::try_create_with_depth(64).expect("bounded replacement channel");
	snapshot(&events, 20, 47);
	for _ in 3..64 {
		event(&events, AudioEvent::MicrophoneVolume(EndpointVolume { id: 8, volume: 60 })).expect("queued old microphone update");
	}
	assert_eq!(events.peer_unread(), Some(64));
	assert_eq!(event(&events, AudioEvent::MicrophoneVolume(EndpointVolume { id: 8, volume: 73 })), Err(ChannelError::Full), "the actual bounded kernel queue refuses the latest update");
	let root2 = subscribe(&broker, consumer);
	drop(events);
	// Old queued Arrived is still decoded before PeerClosed. Its open is accepted
	// by this stand-in only to observe that the consumer closes it on stream loss.
	let pcm2 = answer_output_open(&root2);
	wait_withdrawn(&control_root, &root2, &pcm2);

	let (events, consumer) = Channel::try_create_with_depth(64).expect("fresh recovered event channel");
	snapshot(&events, 20, 73);
	let root3 = subscribe(&broker, consumer);
	let pcm3 = answer_output_open(&root3);
	let recovered = assert_snapshot(&control_root, 20, 73);
	assert!(recovered.0 > original.1 && recovered.1 > recovered.0, "the old AudioService device generations were retired and the same endpoint IDs remapped");
	assert_eq!(control(&control_root).microphone_volume(&original.1), Some(Err(audio_proto::generated::liber::audio::v1::Error::NotFound)));
	event(&events, AudioEvent::Volume(EndpointVolume { id: 8, volume: 32 })).expect("new speaker event");
	event(&events, AudioEvent::MicrophoneVolume(EndpointVolume { id: 8, volume: 80 })).expect("new microphone event");
	assert_eq!(assert_snapshot(&control_root, 32, 80), recovered, "later events update the replacement devices in place");
	assert!(!root3.is_peer_closed() && !pcm3.is_peer_closed());
	assert!(!process.is_terminated(), "the original AudioService process survives all three subscriptions");
	process.terminate();
	sched::run_until_idle();
}
