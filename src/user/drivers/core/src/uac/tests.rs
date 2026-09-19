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
