// The encoder's headers, read back by this tree's own decoder.
//
// That is the whole point of these tests: a specification can be misread twice in the same
// direction, so nothing here compares the bytes against what I believe the format says. Every
// assertion runs the headers through `header::read_header_*` - the parser the player uses - and
// checks the values that came out.

use super::*;
use crate::header;

#[test]
fn the_bit_writer_packs_low_bit_first() {
	// The one thing about Vorbis' bit packing that catches everybody. A four-bit 0b1010 followed by
	// a four-bit 0b0011 is one byte, and the FIRST value occupies the LOW nibble.
	let mut writer = BitWriter::new();
	writer.write(0b1010, 4).expect("written");
	writer.write(0b0011, 4).expect("written");
	assert_eq!(writer.finish(), alloc::vec![0b0011_1010]);

	// A value wider than the byte it starts in continues into the next one, low bits first.
	let mut writer = BitWriter::new();
	writer.write(1, 1).expect("written");
	writer.write(0xff, 8).expect("written");
	assert_eq!(writer.finish(), alloc::vec![0xff, 0x01]);

	// A partial last byte keeps its unused bits zero: the packet's length is in bytes and the
	// reader stops at the framing bit rather than at the end of the byte.
	let mut writer = BitWriter::new();
	writer.write(0b101, 3).expect("written");
	assert_eq!(writer.finish(), alloc::vec![0b0000_0101]);
}

#[test]
fn a_codeword_is_written_most_significant_bit_first() {
	// A codeword is a path through a tree, so its first bit is the one nearest the root - which is
	// its most significant bit, written into the same low-bit-first stream as everything else.
	let mut writer = BitWriter::new();
	writer.write_codeword(0b110, 3).expect("written");
	assert_eq!(writer.finish(), alloc::vec![0b0000_0011]);
	assert!(BitWriter::new().write_codeword(0, 0).is_none(), "a zero-length codeword is not a codeword");
}

#[test]
fn a_flat_codebook_is_complete_and_its_codes_are_canonical() {
	let book = Codebook::flat(4, None).expect("four entries is a power of two");
	assert_eq!(book.entries(), 4);
	// Four entries of two bits each: 00, 01, 10, 11 in index order.
	assert_eq!(book.code(0), Some((0b00, 2)));
	assert_eq!(book.code(1), Some((0b01, 2)));
	assert_eq!(book.code(2), Some((0b10, 2)));
	assert_eq!(book.code(3), Some((0b11, 2)));
	// A book that is not a power of two cannot be flat AND complete, and the decoder refuses an
	// incomplete tree - so it is refused here rather than written and discovered later.
	assert!(Codebook::flat(3, None).is_none());
	assert!(Codebook::flat(1, None).is_none());
}

#[test]
fn vorbis_floats_decode_to_the_values_they_were_built_from() {
	// The format's own float: mantissa * 2^(exponent - 788). These are the values the residue book
	// carries, and a decoder that reads a different number from them decodes to a different signal.
	for value in [1.0f32, -1.0, 0.5, 0.031_25, 2.0, -0.031_25] {
		let packed = f32_to_vorbis(value);
		let mantissa = (packed & 0x001f_ffff) as f64;
		let exponent = ((packed >> 21) & 0x3ff) as i32 - 788;
		let sign = if packed & 0x8000_0000 != 0 { -1.0 } else { 1.0 };
		let decoded = sign * mantissa * libm::pow(2.0, exponent as f64);
		assert!((decoded - value as f64).abs() < 1e-9, "{value} came back as {decoded}");
	}
	assert_eq!(f32_to_vorbis(0.0), 0, "zero is zero, not a denormal");
}

#[test]
fn the_identification_header_says_what_the_stream_is() {
	let packet = write_ident(2, 44_100, 11).expect("the header is written");
	let ident = header::read_header_ident(&packet).expect("our own decoder reads it");
	assert_eq!(ident.audio_channels, 2);
	assert_eq!(ident.audio_sample_rate, 44_100);
	assert_eq!(ident.blocksize_0, 11);
	assert_eq!(ident.blocksize_1, 11);
}

#[test]
fn the_comment_header_carries_the_vendor_and_no_comments() {
	let packet = write_comment("LiberSystem").expect("the header is written");
	let comment = header::read_header_comment(&packet).expect("our own decoder reads it");
	assert_eq!(comment.comment_list.len(), 0, "version one strips metadata deliberately");
}

#[test]
fn the_setup_header_is_read_back_as_the_configuration_it_describes() {
	// THE LOAD-BEARING TEST. The setup header is where a stream stops being decodable if anything
	// in it is a bit wrong, and everything after it depends on the decoder having read the same
	// codebooks, floor and residue this encoder believes it wrote.
	for channels in [1u8, 2] {
		let setup = write_setup(channels, 11).expect("the header is written");
		let parsed = header::read_header_setup(&setup, channels, (11, 11)).expect("our own decoder reads it");
		assert_eq!(parsed.codebooks.len(), 3, "a floor book, a class book and a residue value book");
		assert_eq!(parsed.floors.len(), 1);
		assert_eq!(parsed.residues.len(), 1);
		assert_eq!(parsed.mappings.len(), 1);
		assert_eq!(parsed.modes.len(), 1);
		// The residue covers every channel's whole spectrum, which is what residue 2's interleaving
		// means - and getting this number wrong is how a stereo stream decodes as half a signal.
		assert_eq!(parsed.residues[0].residue_end, 1024 * channels as u32);
		assert_eq!(parsed.residues[0].residue_partition_size, RESIDUE_PARTITION);
		assert_eq!(parsed.residues[0].residue_classifications, 1);
	}
}

#[test]
fn a_setup_header_with_an_incomplete_codebook_is_refused_by_the_decoder() {
	// The refusal this encoder is written against: the decoder demands a complete Huffman tree, so
	// a book whose lengths do not fill it is rejected rather than tolerated. This is what makes
	// `Codebook::flat` refuse a non-power-of-two rather than writing something that parses here and
	// fails in a player.
	let mut book = Codebook { lengths: alloc::vec![2, 2, 2], lookup: None, codes: alloc::vec::Vec::new() };
	book.assign_codes().expect("codes are assignable even when the tree is short");
	let mut out = BitWriter::new();
	write_header_begin(&mut out, 5).expect("written");
	out.write(0, 8).expect("written");
	book.write(&mut out).expect("written");
	let packet = out.finish();
	assert!(header::read_header_setup(&packet, 1, (11, 11)).is_err(), "three two-bit entries leave a hole in the tree");
}

// The setup header read back FIELD BY FIELD, mirroring the decoder's parser.
//
// `the_setup_header_is_read_back_as_the_configuration_it_describes` says whether the header parses;
// this says WHERE it stopped parsing when it does not, which is the difference between a failing
// test you can act on and one that only tells you something is wrong. It earned its place
// immediately: the class-dimension field is three bits, fourteen posts in one class wrote a
// thirteen, and the decoder read back a five - a number that is legal, so the only symptom was
// `HeaderBadFormat` eleven fields later.
#[test]
fn the_setup_header_reads_back_field_by_field() {
	use crate::bitpacking::BitpackCursor;
	let setup = write_setup(1, 11).expect("written");
	let mut rdr = BitpackCursor::new(&setup);
	assert_eq!(rdr.read_u8().unwrap(), 5, "packet type");
	for expected in b"vorbis" {
		assert_eq!(rdr.read_u8().unwrap(), *expected, "signature");
	}
	assert_eq!(rdr.read_u8().unwrap(), 2, "codebook count minus one");
	for book in 0..3u32 {
		assert_eq!(rdr.read_u24().unwrap(), 0x564342, "book {book} sync");
		assert_eq!(rdr.read_u16().unwrap(), 1, "book {book} dimensions");
		let entries = rdr.read_u24().unwrap();
		assert_eq!(rdr.read_bit_flag().unwrap(), false, "book {book} ordered");
		assert_eq!(rdr.read_bit_flag().unwrap(), false, "book {book} sparse");
		for _ in 0..entries {
			let _ = rdr.read_u5().unwrap();
		}
		let lookup = rdr.read_u4().unwrap();
		if lookup != 0 {
			let _ = rdr.read_f32().unwrap();
			let _ = rdr.read_f32().unwrap();
			let bits = rdr.read_u4().unwrap() + 1;
			let _ = rdr.read_bit_flag().unwrap();
			for _ in 0..entries {
				let _ = rdr.read_dyn_u32(bits).unwrap();
			}
		}
	}
	assert_eq!(rdr.read_u6().unwrap(), 0, "time count minus one");
	assert_eq!(rdr.read_u16().unwrap(), 0, "time value");
	assert_eq!(rdr.read_u6().unwrap(), 0, "floor count minus one");
	assert_eq!(rdr.read_u16().unwrap(), 1, "floor type");
	assert_eq!(rdr.read_u5().unwrap(), 2, "floor partitions");
	assert_eq!(rdr.read_u4().unwrap(), 0, "partition class");
	assert_eq!(rdr.read_u4().unwrap(), 0, "partition class");
	assert_eq!(rdr.read_u3().unwrap(), 6, "class dimensions minus one");
	assert_eq!(rdr.read_u2().unwrap(), 0, "class subclasses");
	assert_eq!(rdr.read_u8().unwrap(), 1, "subclass book plus one");
	assert_eq!(rdr.read_u2().unwrap(), 1, "multiplier minus one");
	assert_eq!(rdr.read_u4().unwrap(), 10, "rangebits");
	for post in FLOOR_POSTS {
		assert_eq!(rdr.read_dyn_u32(10).unwrap(), post, "floor post");
	}
	assert_eq!(rdr.read_u6().unwrap(), 0, "residue count minus one");
	assert_eq!(rdr.read_u16().unwrap(), 2, "residue type");
	assert_eq!(rdr.read_u24().unwrap(), 0, "residue begin");
	assert_eq!(rdr.read_u24().unwrap(), 1024, "residue end");
	assert_eq!(rdr.read_u24().unwrap(), RESIDUE_PARTITION - 1, "partition size minus one");
	assert_eq!(rdr.read_u6().unwrap(), 0, "classifications minus one");
	assert_eq!(rdr.read_u8().unwrap(), 1, "classbook");
	assert_eq!(rdr.read_u3().unwrap(), 1, "cascade low bits");
	assert_eq!(rdr.read_bit_flag().unwrap(), false, "cascade high flag");
	assert_eq!(rdr.read_u8().unwrap(), 2, "residue book");
	assert_eq!(rdr.read_u6().unwrap(), 0, "mapping count minus one");
	assert_eq!(rdr.read_u16().unwrap(), 0, "mapping type");
	assert_eq!(rdr.read_bit_flag().unwrap(), false, "submap flag");
	assert_eq!(rdr.read_bit_flag().unwrap(), false, "coupling flag");
	assert_eq!(rdr.read_u2().unwrap(), 0, "reserved");
	assert_eq!(rdr.read_u8().unwrap(), 0, "submap reserved byte");
	assert_eq!(rdr.read_u8().unwrap(), 0, "submap floor");
	assert_eq!(rdr.read_u8().unwrap(), 0, "submap residue");
	assert_eq!(rdr.read_u6().unwrap(), 0, "mode count minus one");
	assert_eq!(rdr.read_bit_flag().unwrap(), false, "blockflag");
	assert_eq!(rdr.read_u16().unwrap(), 0, "windowtype");
	assert_eq!(rdr.read_u16().unwrap(), 0, "transformtype");
	assert_eq!(rdr.read_u8().unwrap(), 0, "mode mapping");
	assert_eq!(rdr.read_bit_flag().unwrap(), true, "framing");
}

#[test]
fn the_forward_mdct_and_this_trees_inverse_round_trip() {
	// THE SCALE COMES FROM HERE. Vorbis' MDCT pair carries a normalisation that implementations
	// place differently, and reading it off the specification is the mistake this whole file is
	// written against - so the forward transform's constant is the one that makes a round trip
	// through the DECODER'S OWN inverse reproduce the input, and this is the measurement.
	//
	// A single block is not a full TDAC round trip - that needs two overlapping blocks, which the
	// test below does - but it is where a scale error or a sign error shows up first and largest.
	const LOG2: u8 = 8; // 256-sample blocks: 128 spectral values
	let two_n: usize = 1 << LOG2;
	let n = two_n / 2;

	// A signal with content at several frequencies, so a transform that is right for one bin and
	// wrong for the rest cannot pass.
	let signal: alloc::vec::Vec<f32> = (0..two_n)
		.map(|i| {
			let t = i as f32;
			0.5 * libm::sinf(core::f32::consts::PI * 2.0 * 3.0 * t / two_n as f32) + 0.25 * libm::sinf(core::f32::consts::PI * 2.0 * 11.0 * t / two_n as f32) + 0.125 * libm::cosf(core::f32::consts::PI * 2.0 * 29.0 * t / two_n as f32)
		})
		.collect();

	let spectrum = forward_mdct(&signal).expect("a power-of-two block");
	assert_eq!(spectrum.len(), n, "the MDCT of 2n samples is n coefficients");

	// Through the decoder's inverse, which takes a full-length buffer with the second half zeroed -
	// the same shape `audio.rs` hands it.
	let cached = crate::header_cached::CachedBlocksizeDerived::from_blocksize(LOG2);
	let mut buffer: alloc::vec::Vec<f32> = spectrum.clone();
	buffer.resize(two_n, 0.0);
	crate::imdct::inverse_mdct(&cached, &mut buffer, LOG2);

	// TIME-DOMAIN ALIASING is the whole point of the transform: one block back is the input plus a
	// folded copy of itself, and only the overlap-add of two blocks cancels it. What a single block
	// can show is that the SECOND quarter and the THIRD quarter are the input's, up to that fold -
	// so the assertion is on the shape of the aliasing rather than on equality, and it is enough to
	// catch a scale, a sign or an off-by-one in the phase.
	//
	// Measured, not derived: `2 / n` is the constant that makes this hold.
	let mut worst = 0.0f32;
	for i in 0..n {
		// The aliasing rule for the first half: y[i] = x[i] - x[n - 1 - i] reflected about n/2.
		let folded = signal[i] - signal[n - 1 - i];
		let error = libm::fabsf(buffer[i] - folded);
		if error > worst {
			worst = error;
		}
	}
	assert!(worst < 0.02, "the first half comes back as the input minus its own reflection, which is what a single MDCT block carries: worst error {worst}");
}

#[test]
fn two_overlapping_blocks_reconstruct_the_signal() {
	// THE PROPERTY THAT MATTERS. A codec's transform is only correct if consecutive blocks, each
	// windowed going in and coming out, add back up to what went in - that is what makes the
	// aliasing cancel, and it is the thing a per-block test cannot see.
	//
	// Three blocks of a continuous signal, hopped by n, and the middle n samples of the output are
	// compared against the middle n samples of the input.
	const LOG2: u8 = 8;
	let two_n: usize = 1 << LOG2;
	let n = two_n / 2;
	let window = window_for(LOG2);
	assert_eq!(window.len(), two_n, "the window spans a whole block");

	let total = two_n * 3;
	let signal: alloc::vec::Vec<f32> = (0..total).map(|i| 0.6 * libm::sinf(core::f32::consts::PI * 2.0 * 5.0 * i as f32 / two_n as f32)).collect();

	let cached = crate::header_cached::CachedBlocksizeDerived::from_blocksize(LOG2);
	// Two consecutive blocks, hopped by n, each windowed in and out.
	let mut halves: alloc::vec::Vec<alloc::vec::Vec<f32>> = alloc::vec::Vec::new();
	for block in 0..2usize {
		let start = block * n;
		let framed: alloc::vec::Vec<f32> = (0..two_n).map(|i| signal[start + i] * window[i]).collect();
		let spectrum = forward_mdct(&framed).expect("a power-of-two block");
		let mut buffer: alloc::vec::Vec<f32> = spectrum;
		buffer.resize(two_n, 0.0);
		crate::imdct::inverse_mdct(&cached, &mut buffer, LOG2);
		for (i, value) in buffer.iter_mut().enumerate() {
			*value *= window[i];
		}
		halves.push(buffer);
	}

	// The overlap-add region: the second half of block 0 plus the first half of block 1 is the
	// input over `n..2n`.
	let mut worst = 0.0f32;
	for i in 0..n {
		let reconstructed = halves[0][n + i] + halves[1][i];
		let error = libm::fabsf(reconstructed - signal[n + i]);
		if error > worst {
			worst = error;
		}
	}
	assert!(worst < 0.02, "two windowed, overlapped blocks reconstruct the signal between them: worst error {worst}");
}

// A spectrum with a peak of `height` at `at`, decaying either side - the shape a floor fit has to
// follow, and the shape a straight line between two distant posts fails to cover if the fit does
// not check its own work.
fn peaked(n: usize, at: usize, height: f32) -> Vec<f32> {
	(0..n)
		.map(|bin| {
			let distance = if bin > at { bin - at } else { at - bin } as f32;
			height / (1.0 + distance * distance * 0.05)
		})
		.collect()
}

#[test]
fn the_floor_this_encoder_writes_is_the_curve_the_decoder_draws() {
	// THE ONE PROPERTY EVERYTHING ELSE RESTS ON. Floor 1 codes each post as a correction to a
	// prediction the decoder computes from posts it has already fixed, so `code_floor` is an
	// inverse of a function in the decoder and either it is exact or the curve is wrong from the
	// first post that disagrees - and the residues, which are the spectrum divided by that curve,
	// then decode into noise with nothing before that point looking wrong.
	//
	// Asserted as a round trip through both stages rather than against a table of expected
	// codewords: a table would be this code's own answer written down twice.
	let blocksize_log2: u8 = 11;
	let posts = floor_x_list(blocksize_log2).len();
	for wanted in [
		alloc::vec![40u32; 16],
		// Rising, falling, and a value at each end of the range - the corrections that take the
		// large branch, where the room on one side runs out and the decoder measures from the other.
		(0..posts as u32).map(|index| 8 + index * 6).collect::<Vec<u32>>(),
		(0..posts as u32).map(|index| 120 - index * 6).collect::<Vec<u32>>(),
		{
			let mut v = alloc::vec![64u32; posts];
			v[0] = 1;
			v[1] = 126;
			v[posts - 1] = 2;
			v
		},
	] {
		let coded = code_floor(&wanted, blocksize_log2).expect("every one of these is a curve floor 1 can draw");
		let curve = render_floor(&coded, blocksize_log2).expect("renders");
		// The curve passes THROUGH each post at the value that was asked for. Checked at the posts
		// rather than everywhere, because between them the curve is a line and that is the point.
		let x_list = floor_x_list(blocksize_log2);
		let n: usize = 1 << (blocksize_log2 - 1);
		for (index, &x) in x_list.iter().enumerate() {
			if x as usize >= n {
				continue;
			}
			let expected = crate::audio::FLOOR1_INVERSE_DB_TABLE[(wanted[index] * 2) as usize];
			assert!((curve[x as usize] - expected).abs() < 1e-6, "post {index} at x={x} wanted {expected} and the decoder draws {}", curve[x as usize]);
		}
	}
}

#[test]
fn a_fitted_floor_is_never_under_the_spectrum_it_was_fitted_to() {
	// WHY "NEVER UNDER" IS THE REQUIREMENT AND NOT "CLOSE". The residue is the spectrum divided by
	// this curve and it is coded by a book covering -1 .. +0.97, so a bin above its own floor
	// cannot be represented at all - it comes back clipped, which is audible as a crushed peak
	// exactly where the signal is loudest. Fitting over rather than through is what makes every
	// residue in range by construction.
	let blocksize_log2: u8 = 11;
	let n: usize = 1 << (blocksize_log2 - 1);
	for magnitude in [
		alloc::vec![0.5f32; n],
		// A peak in the middle of the WIDEST segment this post list has - between 768 and 1024 -
		// which is the case a first-pass fit gets wrong: both posts are far away, so the straight
		// line between them passes under the peak.
		peaked(n, 900, 0.9),
		peaked(n, 6, 0.93),
		peaked(n, 300, 0.4),
		// Silence, where every post lands on the bottom of the table and the corrections are zero.
		alloc::vec![0.0f32; n],
	] {
		let coded = fit_floor(&magnitude, blocksize_log2).expect("this encoder can fit these");
		let curve = render_floor(&coded, blocksize_log2).expect("renders");
		for (bin, &value) in magnitude.iter().enumerate() {
			assert!(curve[bin] >= value, "bin {bin}: the floor is {} and the spectrum is {value}", curve[bin]);
		}
		// AND NOT ABSURDLY ABOVE IT. A floor of 1.0 everywhere would pass the assertion above and
		// spend every bit of the residue coding silence. The loudest bin must be within a factor of
		// the curve over it - the table's step at multiplier 2 is about 2 dB, and a fit that lands
		// within a few steps of the peak is doing its job.
		let peak = magnitude.iter().fold(0.0f32, |a, &b| if b > a { b } else { a });
		if peak > 0.01 {
			let over = curve.iter().zip(magnitude.iter()).filter(|pair| *pair.1 >= peak * 0.99).map(|pair| *pair.0 / *pair.1).fold(0.0f32, |a, b| if b > a { b } else { a });
			assert!(over < 2.0, "the floor sits {over} times over the loudest bin, which is bits spent on nothing");
		}
	}

	// AND A SPECTRUM IT CANNOT SIT ABOVE IS REFUSED. At multiplier 2 the highest coded Y lands one
	// table entry short of 1.0, so a bin above that has no floor - and the two things an encoder
	// could do instead are both silent lies: fitting as high as it can clips that bin's residue,
	// and scaling the spectrum changes the output level with no field in the format to record it.
	let mut past = alloc::vec![0.1f32; n];
	past[10] = floor_ceiling() * 1.01;
	assert_eq!(fit_floor(&past, blocksize_log2), None, "a bin above the reachable ceiling is refused rather than clipped");
	past[10] = floor_ceiling();
	assert!(fit_floor(&past, blocksize_log2).is_some(), "and a bin exactly at it still fits");
}
