use super::*;

// One Type I format descriptor with a discrete rate list.
fn format_bytes(channels: u8, subframe: u8, bits: u8, rates: &[u32]) -> alloc::vec::Vec<u8> {
	let mut out = alloc::vec::Vec::new();
	out.push((8 + rates.len() * 3) as u8);
	out.push(DT_CS_INTERFACE);
	out.push(AS_FORMAT_TYPE);
	out.push(FORMAT_TYPE_I);
	out.push(channels);
	out.push(subframe);
	out.push(bits);
	out.push(rates.len() as u8);
	for rate in rates {
		out.push(*rate as u8);
		out.push((*rate >> 8) as u8);
		out.push((*rate >> 16) as u8);
	}
	out
}

fn read_format(bytes: &[u8]) -> Result<FormatOne, NotBindable> {
	let record = descriptor::Walk::new(bytes).next().expect("one record");
	format_one(&record)
}

#[test]
fn a_sample_rate_is_twenty_four_bits_and_not_thirty_two() {
	// 48000 IS `80 BB 00`. A reader taking four bytes swallows the next rate's low byte and answers
	// a frequency no device has - and with one rate listed it would read past the record.
	let one = read_format(&format_bytes(2, 2, 16, &[48_000])).unwrap();
	assert_eq!(one.rate_count, 1);
	assert_eq!(one.rates[0], 48_000);

	let two = read_format(&format_bytes(2, 2, 16, &[44_100, 48_000])).unwrap();
	assert_eq!(&two.rates[..two.rate_count], &[44_100, 48_000], "two rates are two rates, not one wide one");
}

#[test]
fn a_zero_rate_count_is_a_range_and_not_an_empty_list() {
	let mut bytes = format_bytes(2, 2, 16, &[32_000, 96_000]);
	bytes[7] = 0;
	let format = read_format(&bytes).unwrap();
	assert!(format.continuous);
	assert_eq!(format.rate_count, 2, "a range is its two ends");
	// A driver reading a range as a one-entry list takes the LOWER bound as the only rate.
	assert!(format.suits(48_000, 2, 16), "48 kHz is inside 32..96");
	assert!(!format.suits(96_001, 2, 16), "and past the upper end is not");
	assert!(!format.suits(31_999, 2, 16));
}

#[test]
fn a_format_must_match_exactly_and_the_subframe_size_is_part_of_it() {
	let plain = read_format(&format_bytes(2, 2, 16, &[48_000])).unwrap();
	assert!(plain.suits(48_000, 2, 16));
	assert!(!plain.suits(44_100, 2, 16), "a rate the device does not list is a refusal, not a resample");
	assert!(!plain.suits(48_000, 1, 16), "and mono is not stereo");

	// SIXTEEN BITS IN A FOUR-BYTE SUBFRAME carries the same samples in twice the bytes, so a driver
	// matching on the resolution alone hands the device half a period and calls it whole.
	let padded = read_format(&format_bytes(2, 4, 16, &[48_000])).unwrap();
	assert!(!padded.suits(48_000, 2, 16));
	assert_eq!(padded.period_bytes(512), 512 * 2 * 4, "and its period is the subframe's size, not the resolution's");
	assert_eq!(plain.period_bytes(512), driver_protocol::audio::PERIOD_BYTES, "the wire's period is this format's period");
}

#[test]
fn a_format_type_this_driver_does_not_speak_is_refused_rather_than_read() {
	let mut bytes = format_bytes(2, 2, 16, &[48_000]);
	bytes[3] = 0x02; // Type II, a compressed family
	assert_eq!(read_format(&bytes), Err(NotBindable::NoUsableFormat));
	// And a record that ends before the fields this parse needs is malformed, not a zero format.
	let short = &bytes[..6];
	let record = descriptor::Walk::new(short).next();
	assert!(record.is_none() || format_one(&record.unwrap()) == Err(NotBindable::Malformed));
}

#[test]
fn only_an_isochronous_out_endpoint_is_a_playback_pipe() {
	// BITS 1:0 ARE THE TRANSFER TYPE. The bits above them are synchronisation and usage, which a
	// driver comparing the whole byte reads as a different transfer type on every device that sets
	// them - and QEMU's does.
	assert!(isochronous_out(0x01, 0x01));
	assert!(isochronous_out(0x01, 0x0D), "adaptive, data: still isochronous");
	assert!(!isochronous_out(0x81, 0x01), "an IN endpoint is not a playback pipe");
	assert!(!isochronous_out(0x01, 0x02), "and bulk is not isochronous");
	assert!(!isochronous_out(0x01, 0x03));
}

#[test]
fn a_period_is_several_isochronous_packets_and_the_last_one_is_short() {
	// QEMU's full-speed audio endpoint carries 192 bytes a frame - 48 kHz stereo 16-bit is exactly
	// that in one millisecond - and the wire's period is 2048, which does not divide by it.
	const PERIOD: u32 = driver_protocol::audio::PERIOD_BYTES;
	assert_eq!(packets_for(PERIOD, 192), 11, "ten whole packets and a remainder");
	let total: u32 = (0..packets_for(PERIOD, 192)).map(|index| packet_span(PERIOD, 192, index)).sum();
	assert_eq!(total, PERIOD, "the packets are the period and nothing is dropped at the split");
	assert_eq!(packet_span(PERIOD, 192, 0), 192);
	assert_eq!(packet_span(PERIOD, 192, 10), PERIOD - 10 * 192, "the last one is short, which is not an error");
	assert_eq!(packet_span(PERIOD, 192, 11), 0, "past the end is nothing, not a wrap");

	// AND AN EVEN DIVISION IS NOT A SPECIAL CASE.
	assert_eq!(packets_for(1024, 512), 2);
	assert_eq!(packet_span(1024, 512, 1), 512);
	assert_eq!(packets_for(0, 192), 0, "an empty period is no packets");
	// A packet size of zero is a device that says it carries nothing; asking how many packets that
	// is must not divide by it.
	assert_eq!(packets_for(PERIOD, 0), 0);
}

// A WHOLE AUDIO CONFIGURATION: an audio-control interface, then one streaming interface per endpoint given -
// alternate 0 with nothing on it and alternate 1 carrying the endpoint, PCM, in `format`.
fn audio_config(endpoints: &[(u8, u8)], format: &[u8]) -> alloc::vec::Vec<u8> {
	let mut body: alloc::vec::Vec<u8> = alloc::vec![9, 4, 0, 0, 0, CLASS_AUDIO, SUBCLASS_AUDIOCONTROL, 0, 0];
	body.extend_from_slice(&[9, DT_CS_INTERFACE, 0x01, 0x00, 0x01, 9, 0, 1, 1]);
	for (at, &(address, attributes)) in endpoints.iter().enumerate() {
		let number = at as u8 + 1;
		body.extend_from_slice(&[9, 4, number, 0, 0, CLASS_AUDIO, SUBCLASS_AUDIOSTREAMING, 0, 0]);
		body.extend_from_slice(&[9, 4, number, 1, 1, CLASS_AUDIO, SUBCLASS_AUDIOSTREAMING, 0, 0]);
		body.extend_from_slice(&[7, DT_CS_INTERFACE, AS_GENERAL, 2, 1, 0x01, 0x00]);
		body.extend_from_slice(format);
		body.extend_from_slice(&[9, 5, address, attributes, 192, 0, 1, 0, 0]);
	}
	let total = (9 + body.len()) as u16;
	let mut config = alloc::vec![9, 2, total as u8, (total >> 8) as u8, 1 + endpoints.len() as u8, 1, 0, 0x80, 50];
	config.extend_from_slice(&body);
	config
}

#[test]
fn a_microphone_binds_as_a_source_and_not_as_a_sink() {
	// THE HARNESS'S MICROPHONE: isochronous IN, asynchronous, data.
	let microphone = audio_config(&[(0x81, 0x05)], &format_bytes(2, 2, 16, &[48_000]));
	let source = bind_for(&microphone, Direction::Source).expect("a microphone is a source");
	assert_eq!((source.streaming_interface, source.alternate, source.endpoint, source.max_packet, source.config_value), (1, 1, 0x81, 192, 1));
	assert_eq!(bind(&microphone), Err(NotBindable::NoUsableFormat), "and nothing in it plays");
}

#[test]
fn a_speaker_binds_as_a_sink_and_its_feedback_endpoint_is_not_a_microphone() {
	let speaker = audio_config(&[(0x01, 0x09)], &format_bytes(2, 2, 16, &[48_000]));
	assert_eq!(bind(&speaker).map(|sink| sink.endpoint), Ok(0x01));
	assert_eq!(bind_for(&speaker, Direction::Source), Err(NotBindable::NoUsableFormat));
	// AN ASYNCHRONOUS SINK'S FEEDBACK ENDPOINT is isochronous IN with usage type feedback: the rate the sink
	// wants, not samples.
	assert!(!isochronous_in(0x82, 0x11), "feedback usage");
	assert!(isochronous_in(0x81, 0x05) && isochronous_in(0x81, 0x0D), "asynchronous or synchronous data");
	assert!(!isochronous_in(0x01, 0x05), "an OUT endpoint records nothing");
	let fed_back = audio_config(&[(0x82, 0x11)], &format_bytes(2, 2, 16, &[48_000]));
	assert_eq!(bind_for(&fed_back, Direction::Source), Err(NotBindable::NoUsableFormat));
}

#[test]
fn a_headset_is_a_sink_and_a_source_on_its_own_interfaces() {
	let headset = audio_config(&[(0x01, 0x09), (0x82, 0x05)], &format_bytes(2, 2, 16, &[48_000]));
	let sink = bind(&headset).expect("its speaker");
	let source = bind_for(&headset, Direction::Source).expect("its microphone");
	assert_eq!((sink.streaming_interface, sink.endpoint), (1, 0x01));
	assert_eq!((source.streaming_interface, source.endpoint), (2, 0x82));
}

#[test]
fn a_source_at_a_rate_the_wire_is_not_is_refused_as_a_sink_is() {
	let slow = audio_config(&[(0x81, 0x05)], &format_bytes(2, 2, 16, &[44_100]));
	assert_eq!(bind_for(&slow, Direction::Source), Err(NotBindable::NoUsableFormat));
	let mono = audio_config(&[(0x81, 0x05)], &format_bytes(1, 2, 16, &[48_000]));
	assert_eq!(bind_for(&mono, Direction::Source), Err(NotBindable::NoUsableFormat));
}

// AN ASYNCHRONOUS SPEAKER: its data endpoint asynchronous and naming its feedback endpoint, which follows it in the same
// alternate setting - as the harness's speaker has them.
fn async_speaker(synch: u8, feedback: Option<u8>) -> alloc::vec::Vec<u8> {
	let mut body: alloc::vec::Vec<u8> = alloc::vec![9, 4, 0, 0, 0, CLASS_AUDIO, SUBCLASS_AUDIOCONTROL, 0, 0];
	body.extend_from_slice(&[9, DT_CS_INTERFACE, 0x01, 0x00, 0x01, 9, 0, 1, 1]);
	body.extend_from_slice(&[9, 4, 1, 0, 0, CLASS_AUDIO, SUBCLASS_AUDIOSTREAMING, 0, 0]);
	body.extend_from_slice(&[9, 4, 1, 1, 1 + u8::from(feedback.is_some()), CLASS_AUDIO, SUBCLASS_AUDIOSTREAMING, 0, 0]);
	body.extend_from_slice(&[7, DT_CS_INTERFACE, AS_GENERAL, 2, 1, 0x01, 0x00]);
	body.extend_from_slice(&format_bytes(2, 2, 16, &[48_000]));
	body.extend_from_slice(&[9, 5, 0x01, 0x05, 200, 0, 1, 0, synch]);
	if let Some(address) = feedback {
		body.extend_from_slice(&[9, 5, address, 0x11, 3, 0, 1, 3, 0]);
	}
	let total = (9 + body.len()) as u16;
	let mut config = alloc::vec![9, 2, total as u8, (total >> 8) as u8, 2, 1, 0, 0x80, 50];
	config.extend_from_slice(&body);
	config
}

#[test]
fn an_asynchronous_speaker_binds_with_the_feedback_endpoint_it_names() {
	let sink = bind(&async_speaker(0x82, Some(0x82))).expect("an asynchronous speaker is a sink");
	assert_eq!(sink.sync, Sync::Asynchronous);
	assert_eq!(sink.feedback, Some(Feedback { endpoint: 0x82, max_packet: 3, interval: 1, refresh: 3 }));
	assert_eq!(bind(&async_speaker(0x83, Some(0x82))).map(|sink| sink.feedback), Ok(None), "an address it does not name is not its feedback");
	assert_eq!(bind(&async_speaker(0, None)).map(|sink| (sink.sync, sink.feedback)), Ok((Sync::Asynchronous, None)), "implicit feedback: played at nominal");
	let adaptive = audio_config(&[(0x01, 0x09)], &format_bytes(2, 2, 16, &[48_000]));
	assert_eq!(bind(&adaptive).map(|sink| (sink.sync, sink.feedback)), Ok((Sync::Adaptive, None)));
	assert!(feedback_in(0x82, 0x11) && !feedback_in(0x81, 0x05) && !feedback_in(0x02, 0x11));
}

#[test]
fn the_pacer_averages_exactly_the_rate_over_a_second_at_both_speeds() {
	// FULL SPEED: 48 frames a millisecond nominal; a clock 0.2 % fast asks 48.096 - 10.14 on the wire.
	let nominal = Pacer::nominal_for(48_000, interval_us(1, false));
	assert_eq!(nominal, 48 << 16);
	let mut pacer = Pacer::new(nominal, 50);
	assert_eq!((0..1000).map(|_| pacer.next()).sum::<u32>(), 48_000, "nominal is 48 a millisecond, exactly");
	let raw: u32 = (48_096u32 << 14) / 1000;
	let wire = raw.to_le_bytes();
	let value = feedback_value(&wire[..3], false).expect("three bytes carry it");
	assert_eq!(value, raw << 2);
	assert!(pacer.feedback(value));
	let second: u32 = (0..1000).map(|_| pacer.next()).sum();
	assert!(second.abs_diff(48_096) <= 1, "a second at the device's rate is its frames, to the frame: {second}");
	// A PEEK TAKES NOTHING: what it says is what the next packet is, and the carry stays where it was.
	assert!((0..1000).all(|_| {
		let said = pacer.peek();
		said == pacer.peek() && said == pacer.next()
	}));
	// HIGH SPEED: six frames a microframe nominal, 16.16 on the wire.
	let nominal = Pacer::nominal_for(48_000, interval_us(1, true));
	assert_eq!(nominal, 6 << 16);
	let mut pacer = Pacer::new(nominal, 8);
	let value = feedback_value(&((6 << 16) + 786u32).to_le_bytes(), true).expect("four bytes");
	assert!(pacer.feedback(value));
	let second: u32 = (0..8000).map(|_| pacer.next()).sum();
	assert!(second.abs_diff((((6u64 << 16) + 786) * 8000 >> 16) as u32) <= 1, "{second}");
	assert_eq!(feedback_value(&[1, 2], false), None, "a packet too short carries no value");
}

#[test]
fn a_feedback_value_far_from_nominal_is_ignored_and_counted() {
	let mut pacer = Pacer::new(48 << 16, 50);
	assert!(!pacer.feedback(50 << 16), "two frames a millisecond off is a device misreporting, not a clock");
	assert_eq!((pacer.rate(), pacer.ignored), (48 << 16, 1));
	assert!(pacer.feedback((48 << 16) + (1 << 16)), "exactly one frame off is obeyed");
	// AND NEVER PAST THE PACKET: a rate the packet cannot carry is clamped, never overrun.
	let mut small = Pacer::new(48 << 16, 40);
	assert!((0..10).all(|_| small.next() <= 40));
}

// A UAC2 FUNCTION: an audio-control interface of protocol 0x20 with a clock entity of `clock_kind` as `0x10`, a
// USB-streaming input terminal 1 and a speaker output terminal 3 on clock `terminal_clock`, and a streaming interface
// of protocol `streaming_protocol` whose alternate 1 carries `channels` x `subslot`/`bits` PCM on isochronous OUT 0x01
// (asynchronous) with its feedback endpoint 0x81 - every endpoint the seven bytes UAC2 uses.
fn uac2_config(clock_kind: u8, terminal_clock: u8, streaming_protocol: u8, channels: u8, subslot: u8, bits: u8) -> alloc::vec::Vec<u8> {
	let mut body: alloc::vec::Vec<u8> = alloc::vec![9, 4, 0, 0, 0, CLASS_AUDIO, SUBCLASS_AUDIOCONTROL, PROTOCOL_UAC2, 0];
	body.extend_from_slice(&[9, DT_CS_INTERFACE, 0x01, 0x00, 0x02, 0x01, 46, 0, 0]);
	body.extend_from_slice(&[8, DT_CS_INTERFACE, clock_kind, 0x10, 0x03, 0x07, 0, 0]);
	body.extend_from_slice(&[17, DT_CS_INTERFACE, AC_INPUT_TERMINAL, 1, 0x01, 0x01, 0, terminal_clock, 2, 3, 0, 0, 0, 0, 0, 0, 0]);
	body.extend_from_slice(&[12, DT_CS_INTERFACE, AC_OUTPUT_TERMINAL, 3, 0x01, 0x03, 0, 1, terminal_clock, 0, 0, 0]);
	body.extend_from_slice(&[9, 4, 1, 0, 0, CLASS_AUDIO, SUBCLASS_AUDIOSTREAMING, streaming_protocol, 0]);
	body.extend_from_slice(&[9, 4, 1, 1, 2, CLASS_AUDIO, SUBCLASS_AUDIOSTREAMING, streaming_protocol, 0]);
	body.extend_from_slice(&[16, DT_CS_INTERFACE, AS_GENERAL, 1, 0, FORMAT_TYPE_I, 1, 0, 0, 0, channels, 3, 0, 0, 0, 0]);
	body.extend_from_slice(&[6, DT_CS_INTERFACE, AS_FORMAT_TYPE, FORMAT_TYPE_I, subslot, bits]);
	body.extend_from_slice(&[7, 5, 0x01, 0x05, 28, 0, 1]);
	body.extend_from_slice(&[8, 0x25, 0x01, 0, 0, 0, 0, 0]);
	body.extend_from_slice(&[7, 5, 0x81, 0x11, 4, 0, 4]);
	let total = (9 + body.len()) as u16;
	let mut config = alloc::vec![9, 2, total as u8, (total >> 8) as u8, 2, 1, 0, 0x80, 50];
	config.extend_from_slice(&body);
	config
}

#[test]
fn a_uac2_speaker_binds_with_the_clock_its_terminal_runs_on_and_its_feedback_endpoint() {
	let speaker = bind(&uac2_config(AC_CLOCK_SOURCE, 0x10, PROTOCOL_UAC2, 2, 2, 16)).expect("a UAC2 speaker of the wire's shape binds");
	assert_eq!((speaker.streaming_interface, speaker.alternate, speaker.endpoint, speaker.max_packet, speaker.interval), (1, 1, 0x01, 28, 1));
	assert_eq!(speaker.clock, Some(Clock { control_interface: 0, id: 0x10 }), "the rate is the clock's, set through the audio-control interface");
	assert!(speaker.format.clocked && speaker.format.rate_count == 0, "a UAC2 format carries no rates");
	assert_eq!((speaker.sync, speaker.feedback.map(|feedback| (feedback.endpoint, feedback.max_packet, feedback.interval))), (Sync::Asynchronous, Some((0x81, 4, 4))), "no bSynchAddress in UAC2: the feedback endpoint of the same setting");
	// AND A UAC1 SPEAKER HAS NO CLOCK: its rate is the endpoint's.
	let older = bind(&audio_config(&[(0x01, 0x09)], &format_bytes(2, 2, 16, &[48_000]))).expect("a UAC1 speaker");
	assert_eq!(older.clock, None);
}

#[test]
fn a_uac2_clock_that_must_be_chosen_is_refused_by_name() {
	assert_eq!(bind(&uac2_config(AC_CLOCK_SELECTOR, 0x10, PROTOCOL_UAC2, 2, 2, 16)), Err(NotBindable::ClockTopology), "a selector is a choice of clock");
	assert_eq!(bind(&uac2_config(AC_CLOCK_MULTIPLIER, 0x10, PROTOCOL_UAC2, 2, 2, 16)), Err(NotBindable::ClockTopology), "and so is a multiplier");
	assert_eq!(bind(&uac2_config(AC_CLOCK_SOURCE, 0x22, PROTOCOL_UAC2, 2, 2, 16)), Err(NotBindable::ClockTopology), "a terminal naming a clock that is not there");
}

#[test]
fn a_uac2_format_is_matched_on_its_shape_and_a_mix_of_versions_is_malformed() {
	assert_eq!(bind(&uac2_config(AC_CLOCK_SOURCE, 0x10, PROTOCOL_UAC2, 1, 2, 16)), Err(NotBindable::NoUsableFormat), "mono is not stereo");
	assert_eq!(bind(&uac2_config(AC_CLOCK_SOURCE, 0x10, PROTOCOL_UAC2, 2, 4, 16)), Err(NotBindable::NoUsableFormat), "sixteen bits in four bytes is not the wire");
	assert_eq!(bind(&uac2_config(AC_CLOCK_SOURCE, 0x10, PROTOCOL_UAC2, 2, 3, 24)), Err(NotBindable::NoUsableFormat));
	// A UAC2 CONTROL INTERFACE OVER A STREAMING INTERFACE OF PROTOCOL ZERO reads that interface as UAC1, whose general
	// descriptor this is not - so nothing of it suits, and a configuration that did suit both ways is two things.
	assert!(bind(&uac2_config(AC_CLOCK_SOURCE, 0x10, 0, 2, 2, 16)).is_err());
	let mut mixed = audio_config(&[(0x01, 0x09)], &format_bytes(2, 2, 16, &[48_000]));
	mixed[9 + 7] = PROTOCOL_UAC2;
	assert_eq!(bind(&mixed), Err(NotBindable::Malformed), "a UAC2 control interface over a UAC1 setting");
}

// A `GET RANGE` answer of these (minimum, maximum, resolution) triples.
fn range(triples: &[(u32, u32, u32)]) -> alloc::vec::Vec<u8> {
	let mut out = (triples.len() as u16).to_le_bytes().to_vec();
	for &(low, high, step) in triples {
		out.extend_from_slice(&low.to_le_bytes());
		out.extend_from_slice(&high.to_le_bytes());
		out.extend_from_slice(&step.to_le_bytes());
	}
	out
}

#[test]
fn a_clocks_range_is_read_as_its_subranges_and_a_short_answer_is_refused() {
	assert_eq!(range_offers(&range(&[(44_100, 44_100, 0), (48_000, 48_000, 0)]), 48_000), Ok(true), "two discrete rates");
	assert_eq!(range_offers(&range(&[(44_100, 44_100, 0)]), 48_000), Ok(false), "a clock without it");
	assert_eq!(range_offers(&range(&[(32_000, 96_000, 0)]), 48_000), Ok(true), "a range with no step offers every rate in it");
	assert_eq!(range_offers(&range(&[(48_000, 192_000, 48_000)]), 48_000), Ok(true), "the first step");
	assert_eq!(range_offers(&range(&[(44_100, 192_000, 48_000)]), 48_000), Ok(false), "48 kHz is not a step from 44.1");
	assert_eq!(range_offers(&range(&[(8_000, 44_100, 0)]), 48_000), Ok(false), "and past the top is not offered");
	let mut short = range(&[(48_000, 48_000, 0), (96_000, 96_000, 0)]);
	short.truncate(short.len() - 1);
	assert_eq!(range_offers(&short, 48_000), Err(NotBindable::Malformed), "an answer shorter than its own count");
	assert_eq!(range_offers(&[1], 48_000), Err(NotBindable::Malformed), "or than the count itself");
	let many: alloc::vec::Vec<(u32, u32, u32)> = (0..MAX_SUBRANGES as u32 + 1).map(|n| (48_000 + n, 48_000 + n, 0)).collect();
	assert_eq!(range_offers(&range(&many), 48_000), Err(NotBindable::Malformed), "more subranges than are read");
	assert_eq!(range_offers(&range(&[]), 48_000), Ok(false), "a clock with no rates offers none");
}

#[test]
fn the_packets_ahead_and_the_completion_groups_are_counted_in_time() {
	assert_eq!(posting(interval_us(1, false), 21), (16, 8), "full speed: sixteen milliseconds ahead, an event every eight");
	assert_eq!(posting(interval_us(1, true), 146), (128, 64), "a microframe interval: the same time, eight times the packets");
	assert_eq!(posting(interval_us(4, true), 146), (16, 8), "a high-speed millisecond is a millisecond");
	assert_eq!(posting(interval_us(1, false), 10), (10, 8), "bounded by the page's buffers");
	assert_eq!(posting(interval_us(1, true), 1_000), (MAX_AHEAD, 64), "and by the most kept ahead");
	assert_eq!(posting(interval_us(6, false), 21), (1, 1), "an interval past both still keeps one, with its event");
}
