use crate::codec::{Handles, Reader};
use crate::generated::liber::bluetooth::v1::{AudioEndpoint, AudioEvent, BtCallCommand, EndpointVolume, bluetooth_audio};

// Frozen pre-microphone v1 stream reader: the same four tags and unknown-tag
// refusal as the generated reader at c5811b19. AudioService consumes one IPC
// message before decoding and continues on None, so an unknown event cannot
// retain a message or corrupt the next frame's reader.
fn old_endpoints_read(bytes: &[u8]) -> Option<AudioEvent> {
	let mut reader = Reader::new(bytes);
	let _seq = reader.u32()?;
	let event = match reader.u8()? {
		0 => AudioEvent::Arrived(AudioEndpoint::read(&mut reader)?),
		1 => AudioEvent::Departed(reader.u32()?),
		2 => AudioEvent::Volume(EndpointVolume::read(&mut reader)?),
		3 => AudioEvent::Command(BtCallCommand::read(&mut reader)?),
		_ => return None,
	};
	reader.finish()?;
	Some(event)
}

#[test]
fn an_old_subscriber_skips_the_microphone_event_and_reads_the_next_speaker_event() {
	let events = [
		AudioEvent::MicrophoneVolume(EndpointVolume { id: 7, volume: 40 }),
		AudioEvent::Volume(EndpointVolume { id: 7, volume: 60 }),
		AudioEvent::Command(BtCallCommand::Answer),
		AudioEvent::Departed(7),
	];
	let mut received = alloc::vec::Vec::new();
	for (sequence, event) in events.iter().enumerate() {
		let mut bytes = [0; 64];
		let mut handles = Handles::new();
		let len = bluetooth_audio::endpoints_frame(sequence as u32, event, &mut bytes, &mut handles).unwrap();
		assert!(handles.as_slice().is_empty());
		// This is the old consumer's existing None => continue branch.
		let Some(event) = old_endpoints_read(&bytes[..len]) else { continue };
		received.push(event);
	}
	assert_eq!(received, events[1..]);
	assert_eq!(events[1].encode_vec().unwrap(), [2, 7, 0, 0, 0, 60]);
	assert_eq!(events[2].encode_vec().unwrap(), [3, 1]);
	assert_eq!(events[3].encode_vec().unwrap(), [1, 7, 0, 0, 0]);
	assert_eq!(bluetooth_audio::OP_ENDPOINTS, 1);
	assert_eq!(bluetooth_audio::OP_OPEN, 2);
	assert_eq!(bluetooth_audio::OP_SET_VOLUME, 3);
	assert_eq!(bluetooth_audio::OP_SET_CALL, 4);
}
