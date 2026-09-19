// EACH OF THESE WATCHES ONE WAY AN HDA DRIVER IS WRONG, and the first one is the reason this module
// exists at all: a verb built with the wrong payload width is not refused by anything. The codec
// answers it, a different node gets a different command, and every register afterwards reads
// plausibly.

use super::*;

#[test]
fn a_four_bit_verb_carries_a_wide_payload_and_a_twelve_bit_one_does_not() {
	// `SET STREAM FORMAT` is verb 2 with a sixteen-bit payload; `SET PIN CONTROL` is verb 0x707 with
	// an eight-bit one. Building the first as though it were the second shifts the command into the
	// payload and the payload into the node.
	let wide = verb(0, 3, 2, 0x4011);
	assert_eq!(wide >> 28, 0, "codec 0");
	assert_eq!((wide >> 20) & 0xFF, 3, "node 3");
	assert_eq!((wide >> 16) & 0x0F, 2, "the four-bit command");
	assert_eq!(wide & 0xFFFF, 0x4011, "and the whole sixteen-bit payload");

	let narrow = verb(0, 3, 0x707, 0x40);
	assert_eq!((narrow >> 8) & 0xFFF, 0x707);
	assert_eq!(narrow & 0xFF, 0x40);
	assert_eq!((narrow >> 20) & 0xFF, 3, "the node is where it was");
}

#[test]
fn the_written_form_of_a_wide_verb_is_shifted_back_before_it_is_packed() {
	// The specification writes `SET STREAM FORMAT` as 0x200, which is verb 2 in a twelve-bit field.
	// Handing that to `verb` directly builds a twelve-bit verb with an eight-bit payload - the exact
	// mistake above, spelled differently.
	assert_eq!(verb_wide(0, 3, VERB_SET_STREAM_FORMAT, 0x4011), verb(0, 3, 2, 0x4011));
	assert_ne!(verb_wide(0, 3, VERB_SET_STREAM_FORMAT, 0x4011), verb(0, 3, VERB_SET_STREAM_FORMAT, 0x4011));
}

#[test]
fn the_codec_address_and_node_survive_a_full_width_payload() {
	// The fields are adjacent, so a payload that overflows its width writes into the node.
	let packed = verb(15, 255, 2, 0xFFFF_FFFF);
	assert_eq!(packed >> 28, 15);
	assert_eq!((packed >> 20) & 0xFF, 255);
	assert_eq!(packed & 0xFFFF, 0xFFFF, "the payload is masked to its width, not allowed to spill");
}

#[test]
fn only_the_codec_addresses_that_answered_the_reset_are_walked() {
	let present: alloc::vec::Vec<u8> = codecs_present(0b0000_0000_0000_1001).collect();
	assert_eq!(present, alloc::vec![0, 3]);
	assert_eq!(codecs_present(0).count(), 0, "a machine with no codec has none to walk");
}

#[test]
fn a_node_range_reads_its_start_from_the_top_half_and_its_count_from_the_bottom() {
	// Reading them the other way round walks from node 1 for as many nodes as the first node
	// happens to be numbered.
	assert_eq!(node_range((0x02 << 16) | 0x0A), (2, 10));
	assert_eq!(node_range(0), (0, 0));
}

#[test]
fn an_address_nothing_answers_is_not_a_vendor() {
	// A verb sent where no codec is reads back as all ones or as zero, and a driver that took either
	// for an identity builds a route through a codec that is not there.
	assert!(!codec_answered(0));
	assert!(!codec_answered(0xFFFF_FFFF));
	assert!(codec_answered(0x8384_7680), "a real vendor and device id");
}

#[test]
fn a_widget_is_classified_by_the_four_bits_that_say_what_it_is() {
	assert_eq!(widget_kind(0 << 20), Widget::AudioOutput);
	assert_eq!(widget_kind(1 << 20), Widget::AudioInput);
	assert_eq!(widget_kind(2 << 20), Widget::AudioMixer);
	assert_eq!(widget_kind(3 << 20), Widget::AudioSelector);
	assert_eq!(widget_kind(4 << 20), Widget::PinComplex);
	assert_eq!(widget_kind(7 << 20), Widget::Other(7));
	// The rest of the capability word does not change what the widget is.
	assert_eq!(widget_kind((4 << 20) | 0x000F_FFFF), Widget::PinComplex);
}

#[test]
fn a_pin_that_cannot_output_is_not_a_route() {
	assert!(pin_can_output(1 << 4));
	assert!(!pin_can_output(!(1u32 << 4)), "every other capability set and not that one");
}

#[test]
fn the_format_word_is_indices_and_not_the_numbers_it_names() {
	// A driver that wrote 48000 into this register configures nothing that resembles 48 kHz.
	let f = format(48000, 16, 2).expect("a supported format");
	assert_eq!(f & 0x0F, 1, "two channels is the index one");
	assert_eq!((f >> 4) & 0x07, 1, "sixteen bits is the code one");
	assert_eq!(f & (1 << 14), 0, "and the 48 kHz family clears the base bit");

	let cd = format(44100, 16, 2).expect("a supported format");
	assert_ne!(cd & (1 << 14), 0, "the 44.1 kHz family sets it");
	assert_eq!(format(48000, 24, 1).expect("mono"), 3 << 4, "one channel is the index zero");
}

#[test]
fn a_rate_or_a_depth_this_driver_does_not_configure_is_refused() {
	// Refused rather than approximated: a stream configured at a rate the caller did not ask for
	// plays at the wrong speed and reports success.
	assert_eq!(format(32000, 16, 2), None);
	assert_eq!(format(48000, 12, 2), None);
	assert_eq!(format(48000, 16, 0), None, "no channels is not a stream");
	assert_eq!(format(48000, 16, 17), None);
}

#[test]
fn a_seconds_worth_of_bytes_is_what_a_buffer_length_is_chosen_from() {
	assert_eq!(bytes_per_second(48000, 16, 2), 48000 * 2 * 2);
	assert_eq!(bytes_per_second(44100, 24, 1), 44100 * 3);
}

#[test]
fn the_write_pointer_is_the_last_entry_written_and_not_the_next_slot() {
	// This is the opposite of every other ring in this tree. Treating it as the next slot leaves one
	// entry unwritten and one read twice, on every command.
	assert_eq!(ring_next(0, 256), 1);
	assert_eq!(ring_next(255, 256), 0, "and it wraps");
	assert_eq!(ring_next(5, 0), 0, "a ring of no entries has nowhere to go");
}

#[test]
fn a_response_ring_has_something_when_its_pointer_has_moved() {
	assert!(!ring_has(4, 4));
	assert!(ring_has(5, 4));
	assert!(ring_has(0, 255), "including across the wrap");
}

#[test]
fn a_ring_size_no_controller_expresses_is_refused() {
	assert_eq!(ring_size_code(256), Some(2));
	assert_eq!(ring_size_code(16), Some(1));
	assert_eq!(ring_size_code(2), Some(0));
	assert_eq!(ring_size_code(64), None, "a plausible size that the register cannot say");
	assert_eq!(ring_size_code(0), None);
}

// ------------------------------------------------------- a fake controller's command and response
//
// THE PAIR WHERE THIS DRIVER ACTUALLY FAILED, and where the failure was invisible for four rounds.
// A command ring and a response ring are not one ring twice: the driver owns the command ring's WRITE
// pointer and reads the controller's, and the controller owns the response ring's write pointer and
// the driver keeps its own read cursor. Every one of those four numbers can be off by one on its own,
// and each produces a different plausible-looking stall.
//
// The model writes what a controller writes: it consumes commands up to the driver's write pointer,
// posts one response each, and - the part that mattered - STOPS after `rintcnt` responses until the
// status is acknowledged, which is exactly the behaviour that made a polling driver read its first
// answer and then nothing at all.
struct FakeCodecLink {
	entries: u16,
	// The command ring: what the driver has written, and what the controller has fetched.
	corb_write: u16,
	corb_read: u16,
	// The response ring's write pointer, which the controller owns.
	rirb_write: u16,
	// How many responses it may post before it stalls, and how many it has posted since.
	rintcnt: u16,
	posted: u16,
	stalled: bool,
}

impl FakeCodecLink {
	fn new(entries: u16, rintcnt: u16) -> FakeCodecLink {
		FakeCodecLink { entries, corb_write: 0, corb_read: 0, rirb_write: 0, rintcnt, posted: 0, stalled: false }
	}

	// The driver writes one command: it advances its own pointer first and rings afterwards.
	fn submit(&mut self) {
		self.corb_write = ring_next(self.corb_write, self.entries);
	}

	// The controller consumes what it can and answers each, stopping at its response bound.
	fn run(&mut self) {
		while self.corb_read != self.corb_write {
			if self.posted == self.rintcnt {
				self.stalled = true;
				return;
			}
			self.corb_read = ring_next(self.corb_read, self.entries);
			self.rirb_write = ring_next(self.rirb_write, self.entries);
			self.posted += 1;
		}
	}

	// The driver acknowledges the response status, which is what releases the stall.
	fn acknowledge(&mut self) {
		self.posted = 0;
		self.stalled = false;
	}
}

#[test]
fn a_polling_driver_that_never_acknowledges_gets_one_answer_and_then_silence() {
	// THE DEFECT THIS DRIVER HAD, as a property rather than an anecdote. `RINTCNT` at one is what an
	// interrupt-driven driver wants; a polling one that never clears the response status then gets
	// its first answer and nothing else, with the command ring's write pointer moving and the
	// controller's read pointer standing still - which is exactly what the machine reported.
	let mut link = FakeCodecLink::new(256, 1);
	let mut read = 0u16;

	link.submit();
	link.run();
	assert!(ring_has(link.rirb_write, read), "the first verb is answered");
	read = ring_next(read, link.entries);

	link.submit();
	link.run();
	assert!(link.stalled, "and the second is not even fetched");
	assert_eq!(link.corb_read, 1, "the controller stopped one command behind");
	assert_eq!(link.corb_write, 2, "while the driver had written two");
	assert!(!ring_has(link.rirb_write, read), "so there is no second answer to read");
}

#[test]
fn acknowledging_the_response_status_lets_the_command_ring_carry_on() {
	// The repair, as the same property with the acknowledgement put back.
	let mut link = FakeCodecLink::new(256, 1);
	let mut read = 0u16;
	for step in 0..8u16 {
		link.submit();
		link.run();
		assert!(ring_has(link.rirb_write, read), "verb {step} should be answered");
		read = ring_next(read, link.entries);
		link.acknowledge();
	}
	assert_eq!(link.corb_read, link.corb_write, "the controller kept up with every command");
}

#[test]
fn a_bound_high_enough_never_stalls_in_the_first_place() {
	// The other half of the repair: a polling driver asks for the largest count the field holds, so
	// the stall is not something it has to keep stepping out of.
	let mut link = FakeCodecLink::new(256, 0xFF);
	let mut read = 0u16;
	for _ in 0..64 {
		link.submit();
		link.run();
		assert!(!link.stalled);
		assert!(ring_has(link.rirb_write, read));
		read = ring_next(read, link.entries);
	}
}

#[test]
fn both_rings_wrap_together_over_a_full_pass_and_stay_in_step() {
	// A ring of sixteen taken twice round, so both pointers wrap and the reader has to follow. An
	// off-by-one in either direction shows up as a reader that is permanently one answer behind or
	// one ahead - the first looks like a slow codec, the second like a codec answering the wrong verb.
	const ENTRIES: u16 = 16;
	let mut link = FakeCodecLink::new(ENTRIES, 0xFF);
	let mut read = 0u16;
	for step in 0..(ENTRIES * 2) {
		link.submit();
		link.run();
		assert!(ring_has(link.rirb_write, read), "step {step} should have an answer waiting");
		read = ring_next(read, ENTRIES);
		assert!(!ring_has(link.rirb_write, read), "and exactly one, not two");
	}
	assert_eq!(read, link.rirb_write, "the reader ends where the controller does");
}

#[test]
fn input_and_output_capability_are_different_bits() {
	// BIT 4 IS OUTPUT AND BIT 5 IS INPUT, and a driver that confused them routes the speaker jack
	// into the capture converter - which runs, reports nothing wrong, and returns silence.
	assert!(pin_can_output(1 << 4));
	assert!(!pin_can_input(1 << 4));
	assert!(pin_can_input(1 << 5));
	assert!(!pin_can_output(1 << 5));
	// A pin that does both says so in both bits.
	assert!(pin_can_output((1 << 4) | (1 << 5)));
	assert!(pin_can_input((1 << 4) | (1 << 5)));
	assert!(!pin_can_output(0));
	assert!(!pin_can_input(0));
}
