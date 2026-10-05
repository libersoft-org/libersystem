use super::*;

#[test]
fn a_length_is_the_tag_and_every_other_length_is_refused() {
	assert_eq!(message(&[0u8; PERIOD_BYTES as usize]), Message::Play);
	assert_eq!(message(&[]), Message::EndPlayback);
	assert_eq!(message(&[CMD_CAPTURE]), Message::Capture);
	assert_eq!(message(&[CMD_CAPTURE_STOP]), Message::EndCapture);

	// A SHORT PERIOD IS NOT A SMALL ONE. A server that played it would play part of a period as
	// though it were whole, which is a click rather than an error - and AudioService pads rather
	// than sending one, so this length was never a shape of this wire.
	assert_eq!(message(&[0u8; PERIOD_BYTES as usize - 1]), Message::Unknown);
	assert_eq!(message(&[0u8; PERIOD_BYTES as usize + 1]), Message::Unknown);
	// AND AN UNDEFINED COMMAND BYTE IS NOT CAPTURE. A server that treated anything one byte long as
	// a capture request would answer a question nobody asked with a buffer of samples.
	assert_eq!(message(&[0]), Message::Unknown);
	assert_eq!(message(&[6]), Message::Unknown);
	assert_eq!(message(&[255]), Message::Unknown);
	assert_eq!(message(&[CMD_STATS]), Message::Stats);
}

#[test]
fn the_counters_round_trip_and_no_other_answer_reads_as_them() {
	let stats = PlaybackStats { underruns: 1, silent_frames: 9_600, feedback_q16: 48 << 16 | 0x1999, feedback_ignored: 2 };
	assert_eq!(PlaybackStats::decode(&stats.encode()), Some(stats));
	assert_eq!(PlaybackStats::decode(REFUSED), None, "a provider that keeps none refuses");
	assert_eq!(PlaybackStats::decode(OK), None);
	assert_eq!(PlaybackStats::decode(&[0; PERIOD_BYTES as usize]), None);
	assert!(STATS_BYTES != OK.len() && STATS_BYTES != PERIOD_BYTES as usize && STATS_BYTES != 0);
}

#[test]
fn a_refusal_cannot_be_mistaken_for_samples() {
	// The whole reason a refusal is empty: a period is PERIOD_BYTES, so a consumer tells "here are
	// your samples" from "no" by the length alone.
	assert!(REFUSED.is_empty());
	assert_ne!(REFUSED.len(), PERIOD_BYTES as usize);
	assert_ne!(OK.len(), PERIOD_BYTES as usize);
	assert!(!OK.is_empty(), "and an acknowledgement is not a refusal either");
}

#[test]
fn the_format_is_the_one_both_servers_negotiate() {
	// 512 stereo 16-bit frames.
	assert_eq!(PERIOD_BYTES, 512 * CHANNELS as u32 * (BITS as u32 / 8));
	assert_eq!(RATE_HZ, 48_000);
}

#[test]
// THE TWO NEW COMMANDS: the format question, and the level - two bytes, the level at most 100 - and a period of the
// provider's own length.
fn the_format_and_volume_commands_and_a_providers_own_period() {
	assert_eq!(message(&[CMD_FORMAT]), Message::Format);
	assert_eq!(message(&[CMD_VOLUME, 40]), Message::Volume(40));
	assert_eq!(message(&[CMD_VOLUME, 101]), Message::Unknown, "a level past 100 is no level");
	assert_eq!(message(&[CMD_VOLUME]), Message::Unknown);
	assert_eq!(message_of(&[0u8; 640], 640), Message::Play, "ten milliseconds of 16 kHz stereo");
	assert_eq!(message_of(&[0u8; 640], 2048), Message::Unknown);
	assert_eq!(message_of(&[0u8; 2], 2), Message::Unknown, "a period is never as short as a command");
	assert!(COMMAND_MAX < MIN_PERIOD_BYTES as usize);
}

#[test]
// THE FORMAT ROUND-TRIPS, a refusal reads as nothing, and what no consumer could drive is refused rather than read.
fn the_format_round_trips_and_an_undrivable_one_is_refused() {
	let voice = DeviceFormat { output: Some(PcmFormat { rate: 16_000, channels: 1 }), input: Some(PcmFormat { rate: 16_000, channels: 1 }), period_bytes: 320, latency_us: 7_500, hardware_volume: true, volume: 60 };
	assert_eq!(DeviceFormat::decode(&voice.encode()), Some(voice));
	let speaker = DeviceFormat { output: Some(PcmFormat { rate: 44_100, channels: 2 }), input: None, period_bytes: 1_764, latency_us: 150_000, hardware_volume: false, volume: 100 };
	assert_eq!(DeviceFormat::decode(&speaker.encode()), Some(speaker));
	assert_eq!(DeviceFormat::decode(REFUSED), None);
	assert_eq!(DeviceFormat::decode(OK), None);
	assert!(FORMAT_BYTES != STATS_BYTES && FORMAT_BYTES != OK.len() && FORMAT_BYTES < MIN_PERIOD_BYTES as usize);
	let mut odd = speaker;
	odd.period_bytes = 1_765;
	assert_eq!(DeviceFormat::decode(&odd.encode()), None, "a period that is not whole frames");
	let mut fast = speaker;
	fast.output = Some(PcmFormat { rate: 96_000, channels: 2 });
	assert_eq!(DeviceFormat::decode(&fast.encode()), None, "a rate no consumer converts");
	let legacy = DeviceFormat::LEGACY;
	assert_eq!(legacy.period_bytes, PERIOD_BYTES);
	assert_eq!(legacy.latency_us, 21_333, "two periods of 512 frames at 48 kHz");
}
