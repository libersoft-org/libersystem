use super::*;

use graphics_profile::image::Operation;

fn layout_of(format: PixelFormat, width: u32, height: u32, pitch: u32, semantics: ImageSemantics) -> Result<ImageLayout, Error> {
	ImageLayout::new(Extent2D::new(width, height), pitch, PixelStorage::Known(format), RowOrigin::TopLeft, semantics)
}

fn colour(format: PixelFormat) -> ImageSemantics {
	let alpha = if format.admits(AlphaMode::Premultiplied) { AlphaMode::Premultiplied } else { AlphaMode::Opaque };
	ImageSemantics::Color { color_space: ColorSpace::SrgbLinear, alpha_mode: alpha }
}

#[test]
// THE PROFILE IS THE LIST AND THIS IS THE CODE, and neither restates the other. A fixture that only
// checked the code would let the two part; this holds the enumeration and the frozen registry to
// describing the same twelve formats, with the same widths and the same alpha rules.
fn the_format_enumeration_and_the_frozen_registry_describe_the_same_formats() {
	use graphics_profile::image::FORMATS;
	assert_eq!(format::ALL_FORMATS.len(), FORMATS.len(), "the code and the registry carry different numbers of formats");
	for (format, described) in format::ALL_FORMATS.iter().zip(FORMATS.iter()) {
		assert_eq!(format.name(), described.name, "the enumeration is out of order with the registry");
		assert_eq!(format.bytes_per_pixel(), described.bytes_per_pixel as u32);
		assert_eq!(format.carries(), described.carries);
	}
	// AND THE ALPHA RULES COME FROM THE REGISTRY rather than from a second table here.
	assert!(PixelFormat::B8G8R8A8Unorm.admits(AlphaMode::Premultiplied));
	assert!(!PixelFormat::B8G8R8X8Unorm.admits(AlphaMode::Premultiplied), "an X8 format is opaque only");
	assert!(!PixelFormat::A8Unorm.admits(AlphaMode::Premultiplied), "an alpha-only format has no colour to premultiply");
	assert!(PixelFormat::A8Unorm.admits(AlphaMode::Straight));
}

#[test]
// THE THREE SPANS ARE THREE DIFFERENT QUESTIONS. A validator demanding `pitch * height` refuses a
// buffer that is exactly big enough for the image it holds; one demanding only the visible bytes lets
// a scanout engine read past the allocation.
fn the_three_byte_spans_are_three_different_numbers() {
	// Four pixels wide at four bytes is sixteen; a pitch of twenty leaves four bytes of padding.
	let layout = layout_of(PixelFormat::B8G8R8A8Unorm, 4, 3, 20, colour(PixelFormat::B8G8R8A8Unorm)).expect("a layout this fixture built");
	assert_eq!(layout.minimum_row_bytes(), Some(16));
	// TWO FULL ROWS AND THE LAST ROW'S PIXELS: 20 + 20 + 16, and NOT 60.
	assert_eq!(layout.minimum_visible_bytes(), Some(56));
	// What a backend that reads whole rows may touch IS sixty.
	assert_eq!(layout.backend_access_span(true), Some(60));
	// And one that does not is back to the visible span.
	assert_eq!(layout.backend_access_span(false), Some(56));

	// A BUFFER OF EXACTLY THE VISIBLE BYTES IS ACCEPTED, which is the case a `pitch * height` check
	// rejects - and it is the ordinary case for the last row of a tightly packed image.
	let bytes = std::vec![0u8; 56];
	assert!(ImageView::new(layout, &bytes).is_ok());
	let bytes = std::vec![0u8; 55];
	assert_eq!(ImageView::new(layout, &bytes).err(), Some(Error::BufferTooShort));
}

#[test]
// THE CONSTRUCTOR IS THE POINT. What it replaces is an application computing a length and building an
// aliasing mutable slice out of a raw pointer, which is unsound whenever the length is wrong and is
// exactly as easy to write when it is.
fn a_layout_refuses_every_way_an_image_has_actually_been_wrong() {
	let semantics = colour(PixelFormat::B8G8R8A8Unorm);
	// A zero extent: a refusal rather than an empty image, because every consumer that divides by an
	// extent would have to check and one of them will not.
	assert_eq!(layout_of(PixelFormat::B8G8R8A8Unorm, 0, 4, 16, semantics).err(), Some(Error::ZeroExtent));
	assert_eq!(layout_of(PixelFormat::B8G8R8A8Unorm, 4, 0, 16, semantics).err(), Some(Error::ZeroExtent));
	// A pitch below the minimum row: a layout that cannot hold its own rows.
	assert_eq!(layout_of(PixelFormat::B8G8R8A8Unorm, 4, 4, 15, semantics).err(), Some(Error::PitchTooSmall));
	// AN ALPHA MODE THE FORMAT DOES NOT ADMIT is a TYPE ERROR, not an unsupported case.
	let straight = ImageSemantics::Color { color_space: ColorSpace::SrgbLinear, alpha_mode: AlphaMode::Straight };
	assert_eq!(layout_of(PixelFormat::B8G8R8X8Unorm, 4, 4, 16, straight).err(), Some(Error::AlphaModeNotAdmitted));
	// And a width whose row bytes overflow.
	assert_eq!(layout_of(PixelFormat::R32G32B32A32Float, u32::MAX, 2, u32::MAX, semantics).err(), Some(Error::Overflow));
}

#[test]
// A LITERAL 32 ONCE GAVE A DIAGONAL SMEAR RATHER THAN A PICTURE, which is why firmware's own masks are
// a storage arm at all. Two channels sharing a bit means one changes when the other is written - a
// mode nobody tested, and one whose colours shift as the content changes.
fn a_firmware_layout_with_overlapping_channels_is_refused() {
	let channel = |shift: u8, bits: u8| PackedChannel { shift, bits };
	let good = PackedRgbLayout { bytes_per_pixel: 2, red: channel(11, 5), green: channel(5, 6), blue: channel(0, 5), reserved: channel(0, 0) };
	assert!(good.validate().is_ok(), "a sixteen-bit 5:6:5 mode is a mode this system must accept");

	let overlapping = PackedRgbLayout { bytes_per_pixel: 2, red: channel(10, 6), green: channel(5, 6), blue: channel(0, 5), reserved: channel(0, 0) };
	assert_eq!(overlapping.validate().err(), Some(Error::OverlappingChannels));

	// A channel that runs past the element is not a channel.
	let past = PackedRgbLayout { bytes_per_pixel: 2, red: channel(12, 6), green: channel(5, 6), blue: channel(0, 5), reserved: channel(0, 0) };
	assert_eq!(past.validate().err(), Some(Error::UnknownFormat));

	// AND THE PACKED ARM IS NOT SAMPLEABLE, which is the profile's own rule and is stated here rather
	// than at every place that would otherwise have to remember it.
	assert!(!PixelStorage::PackedRgbUnorm(good).is_sampleable());
	assert!(PixelStorage::Known(PixelFormat::B8G8R8A8Unorm).is_sampleable());
}

#[test]
// A NORMAL MAP, A ROUGHNESS MAP, A COVERAGE MASK AND A DEPTH BUFFER ARE ALL JUST BYTES. The type
// carries what they mean, and the operation table is the profile's rather than a second copy here.
fn what_an_image_means_decides_what_may_be_done_to_it() {
	let mask = ImageSemantics::Mask { interpretation: MaskInterpretation::Coverage };
	let normal = ImageSemantics::NormalMap { convention: NormalMapConvention::GreenUp };
	let picking = ImageSemantics::Data;
	let colour = ImageSemantics::Color { color_space: ColorSpace::Srgb, alpha_mode: AlphaMode::Premultiplied };

	assert!(colour.admits(Operation::Transfer));
	assert!(!mask.admits(Operation::Transfer), "an sRGB decode over a coverage mask lightens every edge");
	assert!(!normal.admits(Operation::Transfer), "a normal map through a transfer function lights every bump as a dent");
	assert!(!picking.admits(Operation::Transfer));
	// A non-colour image cannot carry an ignored colour space or alpha mode, because a field that is
	// ignored is a field somebody eventually sets and expects to matter.
	assert_eq!(mask.color_space(), None);
	assert_eq!(mask.alpha_mode(), None);
	assert_eq!(colour.color_space(), Some(ColorSpace::Srgb));
}

#[test]
// A BUFFER RECYCLED BETWEEN DOMAINS HOLDS WHATEVER THE LAST ONE PUT IN IT, and an image whose bytes
// were never written shows that - a disclosure that reads as a flicker and is therefore never
// reported.
fn an_owned_image_is_zeroed_and_owns_what_a_backend_may_touch() {
	let layout = layout_of(PixelFormat::B8G8R8A8Unorm, 4, 3, 20, colour(PixelFormat::B8G8R8A8Unorm)).expect("a layout");
	let mut image = OwnedImage::new(layout).expect("an allocation this small");
	assert!(image.view().bytes().iter().all(|byte| *byte == 0), "a newly allocated image must be zeroed");
	// IT OWNS THE FINAL ROW'S PADDING, because a scanout engine fetching whole rows reads it - and an
	// allocation one row-padding short is an out-of-bounds read by HARDWARE, which no bounds check in
	// this process can catch.
	assert_eq!(image.allocation_len(), 60);

	// A row accessor hands back the PIXELS and not the pitch: padding is not content, and a readback
	// that returned it would return uninitialised bytes.
	assert_eq!(image.view().row(0).map(|row| row.len()), Some(16));
	assert_eq!(image.view().row(3), None, "a row past the extent is not a row");

	// EXPORT ZEROES THE PADDING rather than trusting it to have stayed zero: a drawing routine that
	// wrote a whole pitch is not a bug, and its padding is still not content. Two exports of one image
	// must be the same bytes, which is what makes it hashable.
	image.view_mut().bytes_mut().fill(0xAB);
	let exported = image.export().to_vec();
	for y in 0..3usize {
		assert_eq!(&exported[y * 20 + 16..y * 20 + 20], &[0, 0, 0, 0], "row {y}'s padding was exported as content");
		assert!(exported[y * 20..y * 20 + 16].iter().all(|byte| *byte == 0xAB), "row {y}'s pixels were cleared");
	}

	// AND A COLOUR-ONLY ENTRY POINT VALIDATES ITS SEMANTICS rather than documenting them.
	let mask = layout_of(PixelFormat::A8Unorm, 4, 4, 4, ImageSemantics::Mask { interpretation: MaskInterpretation::Coverage }).expect("a mask layout");
	assert_eq!(OwnedImage::new_color(mask).err(), Some(Error::NotColour));
	assert!(OwnedImage::new(mask).is_ok(), "a mask still has an owner; it is only the colour entry point that refuses it");
}

#[test]
// BOTH RECTANGLES ARE HALF-OPEN, so adjacent ones tile without overlapping. Closed rectangles are why
// two adjacent damage regions redraw a shared column twice, which is invisible until it flickers.
fn both_coordinate_spaces_are_half_open_and_neither_is_the_other() {
	let rect = RectF::new(1.0, 2.0, 3.0, 4.0);
	assert!(rect.contains(PointF { x: 1.0, y: 2.0 }), "the top-left corner is inside");
	assert!(!rect.contains(PointF { x: 4.0, y: 2.0 }), "the right edge is OUTSIDE, which is what makes adjacent rectangles tile");
	assert!(!rect.contains(PointF { x: 1.0, y: 6.0 }));
	// A NEGATIVE ORIGIN IS REPRESENTABLE, which is the whole reason the drawing space is signed: an
	// unsigned parameter makes "half a pixel to the left of the origin" unrepresentable.
	let off_screen = RectF::new(-0.5, -0.5, 2.0, 2.0);
	assert!(off_screen.contains(PointF { x: -0.5, y: -0.5 }));
	// An empty rectangle has a zero extent, and an intersection that does not meet is empty rather
	// than negative.
	assert!(RectF::new(0.0, 0.0, 0.0, 5.0).is_empty());
	assert!(rect.intersection(&RectF::new(100.0, 100.0, 1.0, 1.0)).is_empty());

	// The pixel space is where damage lives, and a rectangle that does not fit its surface is refused
	// at the boundary rather than clamped inside it.
	let extent = Extent2D::new(64, 32);
	assert!(PixelRect::new(0, 0, 64, 32).fits(extent));
	assert!(!PixelRect::new(1, 0, 64, 32).fits(extent));
	assert!(!PixelRect::new(0, 0, u32::MAX, 1).fits(extent), "a rectangle whose right edge overflows must be refused rather than wrapped");
}

#[test]
// NONE OF THE COLOUR NUMBERS ARE IN THIS CRATE. The matrices are DERIVED from the frozen
// chromaticities, so a tabulated matrix cannot drift from them - and the derivation is checked by the
// property every correct one has: the primaries must reproduce their own white point.
fn the_colour_matrices_are_derived_from_the_frozen_primaries() {
	for space in color::ALL_SPACES {
		let matrix = color::rgb_to_xyz(space).expect("every profile space has primaries");
		// WHITE IN, WHITE OUT. RGB (1,1,1) through the matrix must be the space's own white point in
		// XYZ, which is the identity the per-primary scaling exists to produce.
		let white = color::multiply_vector(&matrix, [1.0, 1.0, 1.0]);
		let chromaticity = space.primaries().white;
		let expected = [chromaticity.x / chromaticity.y, 1.0, (1.0 - chromaticity.x - chromaticity.y) / chromaticity.y];
		for (index, (found, want)) in white.iter().zip(expected.iter()).enumerate() {
			assert!((found - want).abs() < 1e-9, "{}'s primaries do not reproduce its white point at component {index}: {found} against {want}", space.name());
		}
	}
	// A CONVERSION AND ITS REVERSE MUST BE THE IDENTITY, which is what catches a transposed matrix -
	// a transposition is still invertible, so only the round trip finds it.
	let there = color::convert(ColorSpace::DisplayP3Linear, ColorSpace::Rec2020Linear).expect("both are profile spaces");
	let back = color::convert(ColorSpace::Rec2020Linear, ColorSpace::DisplayP3Linear).expect("both are profile spaces");
	let round = color::multiply(&back, &there);
	for (row, values) in round.iter().enumerate() {
		for (column, value) in values.iter().enumerate() {
			let expected = if row == column { 1.0 } else { 0.0 };
			assert!((value - expected).abs() < 1e-9, "the round trip is not the identity at {row},{column}: {value}");
		}
	}
	// AND A WIDE-GAMUT COLOUR IN A NARROWER SPACE IS NEGATIVE, which the profile says is allowed
	// INSIDE the pipeline - a conversion that clamped here would destroy it before anything could
	// tone-map or gamut-map it. The direction matters: Display P3 is INSIDE Rec. 2020, so it is
	// Rec. 2020's green in Display P3 that goes negative and not the other way round.
	let green = color::multiply_vector(&back, [0.0, 1.0, 0.0]);
	assert!(green.iter().any(|component| *component < 0.0), "Rec. 2020's green is outside Display P3 in at least one component: {green:?}");
	let inside = color::multiply_vector(&there, [0.0, 1.0, 0.0]);
	assert!(inside.iter().all(|component| *component > -1e-9), "Display P3 is a subset of Rec. 2020, so nothing of it is outside: {inside:?}");
}

#[test]
// THE LINEAR SEGMENT OF sRGB IS THE PART THAT GETS DROPPED, and PQ is the one transfer function whose
// 1.0 is a quantity of light rather than a fraction of one. Both directions are checked by their
// round trip, which is the test that catches a constant transcribed into the wrong branch.
fn every_transfer_function_round_trips_through_its_own_inverse() {
	use graphics_profile::image::Transfer;
	for transfer in [Transfer::Linear, Transfer::Srgb, Transfer::Pq, Transfer::Hlg] {
		for step in 0..=64u32 {
			let value = step as f64 / 64.0;
			let there = color::decode(transfer, value);
			let back = color::encode(transfer, there);
			assert!((back - value).abs() < 1e-6, "{:?} does not round trip at {value}: {back}", transfer);
		}
	}
	// THE ENDPOINTS, which is where the linear segment and the power law have to meet.
	assert!((color::decode(Transfer::Srgb, 0.0) - 0.0).abs() < 1e-12);
	assert!((color::decode(Transfer::Srgb, 1.0) - 1.0).abs() < 1e-12);
	// And the two segments meet at the threshold: a discontinuity there is banding in the darks.
	let threshold = graphics_profile::image::srgb::ENCODED_THRESHOLD;
	let below = color::decode(Transfer::Srgb, threshold - 1e-9);
	let above = color::decode(Transfer::Srgb, threshold + 1e-9);
	assert!((above - below).abs() < 1e-5, "the sRGB segments do not meet at their threshold: {below} against {above}");
}

#[test]
// A HALF IS THE CANONICAL INTERMEDIATE'S STORAGE, so the trip through it has to keep what it claims
// to keep: the subnormals at the bottom of a gradient, the infinities at the top, and a NaN as a NaN
// rather than as an infinity with its payload truncated away.
fn the_half_float_conversion_survives_every_class_of_value() {
	for value in [0.0f32, 1.0, -1.0, 0.5, 65504.0, -65504.0, 6.103_515_6e-5] {
		let back = pixel::half_to_f32(pixel::f32_to_half(value));
		assert_eq!(back, value, "a value a half can hold must come back unchanged: {value}");
	}
	// A SUBNORMAL HALF IS A NORMAL SINGLE, and rounding it to zero is where a gradient's darkest band
	// disappears.
	let subnormal = pixel::half_to_f32(1);
	assert!(subnormal > 0.0 && subnormal < 1e-7, "the smallest half is a small positive number: {subnormal}");
	assert_eq!(pixel::f32_to_half(subnormal), 1);
	// Too large clamps to an infinity rather than wrapping to a small number.
	assert!(pixel::half_to_f32(pixel::f32_to_half(1e30)).is_infinite());
	assert!(pixel::half_to_f32(pixel::f32_to_half(f32::NAN)).is_nan(), "a NaN whose mantissa is truncated becomes an infinity");
	// ROUNDING IS TO NEAREST EVEN and not truncation, which would bias every accumulated colour down.
	let exact = pixel::half_to_f32(pixel::f32_to_half(0.1));
	assert!((exact - 0.1).abs() < 0.001, "{exact}");
}

#[test]
// WHAT HAPPENS AT EVERY NUMERIC BOUNDARY IS STATED rather than left to a language's defaults: a cast
// of a NaN to an integer is unspecified, and of an infinity is a trap on some architectures.
fn quantisation_follows_the_frozen_rounding_rule() {
	assert_eq!(pixel::quantise(0.5, 255.0), 128.0, "half rounds AWAY from zero");
	assert_eq!(pixel::quantise(1.0, 255.0), 255.0);
	assert_eq!(pixel::quantise(2.0, 255.0), 255.0, "an over-range value clamps");
	assert_eq!(pixel::quantise(-1.0, 255.0), 0.0);
	assert_eq!(pixel::quantise(f32::NAN, 255.0), 0.0, "a NaN reaching an integer channel is zero");
	assert_eq!(pixel::quantise(f32::INFINITY, 255.0), 255.0, "an infinity clamps to the extreme");
	assert_eq!(pixel::quantise(f32::NEG_INFINITY, 255.0), 0.0);

	// THE DITHER IS A PERMUTATION OF ITS OWN RANGE, so its offsets span just under one step and
	// average to nothing - which is what makes it a dither rather than a brightening.
	let mut sum = 0.0f32;
	for y in 0..8 {
		for x in 0..8 {
			let offset = pixel::dither_offset(x, y);
			assert!((-0.5..0.5).contains(&offset), "{offset}");
			sum += offset;
		}
	}
	assert!(sum.abs() < 1e-3, "the offsets must average to zero: {sum}");
	assert_eq!(pixel::quantisation_steps(PixelStorage::Known(PixelFormat::R8G8B8A8Unorm)), Some(255.0));
	assert_eq!(pixel::quantisation_steps(PixelStorage::Known(PixelFormat::R16G16B16A16Float)), None, "a float target has no step to spread an error over");
}

#[test]
// FILTERING PREMULTIPLIES BEFORE INTERPOLATING. Interpolating straight alpha pulls the colour of
// fully transparent texels into the result, which rings a dark halo around every transparent edge -
// and it looks correct on every opaque test image, which is how it ships.
fn sampling_interpolates_premultiplied_and_does_not_ring_a_halo() {
	// Two texels: transparent (whose stored colour is a misleading black) and opaque white.
	let semantics = ImageSemantics::Color { color_space: ColorSpace::SrgbLinear, alpha_mode: AlphaMode::Straight };
	let layout = layout_of(PixelFormat::R8G8B8A8Unorm, 2, 1, 8, semantics).expect("a layout");
	let bytes = [0, 0, 0, 0, 255, 255, 255, 255];
	let view = ImageView::new(layout, &bytes).expect("a view");
	let sampler = sample::Sampler::new(view, pixel::Working::linear(ColorSpace::Srgb), sample::Spread::Clamp).expect("a sampler");
	// Halfway between the two texel centres: half the alpha, and the colour must be half of WHITE -
	// which in premultiplied terms means red equals alpha. A straight-alpha interpolation gives a
	// colour of 0.5 and an alpha of 0.5, which unpremultiplies to a grey edge.
	let middle = sampler.sample(1.0, 0.5, sample::Quality::Bilinear);
	assert!((middle.alpha - 0.5).abs() < 0.01, "{middle:?}");
	assert!((middle.red - 0.5).abs() < 0.01, "premultiplied, the colour must track the alpha: {middle:?}");
	let unpremultiplied = middle.red / middle.alpha;
	assert!((unpremultiplied - 1.0).abs() < 0.02, "the visible colour stays white rather than darkening to grey: {unpremultiplied}");

	// AND THE SPREAD MODES ARE THE THREE THE PROFILE NAMES.
	assert_eq!(sample::Spread::Clamp.wrap(-3, 4), Some(0));
	assert_eq!(sample::Spread::Clamp.wrap(9, 4), Some(3));
	assert_eq!(sample::Spread::Repeat.wrap(-1, 4), Some(3));
	assert_eq!(sample::Spread::Repeat.wrap(5, 4), Some(1));
	assert_eq!(sample::Spread::Mirror.wrap(4, 4), Some(3), "a mirror turns back at the edge rather than wrapping");
	assert_eq!(sample::Spread::Mirror.wrap(-1, 4), Some(0));
}

#[test]
// A PYRAMID IS WHAT STOPS A THUMBNAIL SHIMMERING. Its levels are averaged in PREMULTIPLIED LINEAR
// light: averaging encoded values makes a downscaled image darker than the original by exactly the
// amount the transfer function bends, which is the defect that makes photographs look muddy.
fn the_pyramid_averages_in_linear_light_and_is_sampled_between_levels() {
	let semantics = ImageSemantics::Color { color_space: ColorSpace::Srgb, alpha_mode: AlphaMode::Opaque };
	let layout = layout_of(PixelFormat::R8G8B8X8Unorm, 2, 2, 8, semantics).expect("a layout");
	// A checkerboard of black and white, which averages to the LIGHT halfway between them - which is
	// not the encoded value halfway between them.
	let bytes = [255, 255, 255, 255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255, 255];
	let view = ImageView::new(layout, &bytes).expect("a view");
	let working = pixel::Working::linear(ColorSpace::Srgb);
	let pyramid = sample::Pyramid::build(&view, working).expect("a pyramid");
	assert_eq!(pyramid.levels(), 2, "a two-by-two image has a base and a one-by-one top");
	let top = pyramid.level(1).expect("the top");
	let averaged = pixel::read(&top, 0, 0).expect("its one texel");
	assert!((averaged.red - 0.5).abs() < 0.01, "half black and half white is half the LIGHT: {averaged:?}");
	// THE SAMPLE BETWEEN LEVELS IS INTERPOLATED rather than snapped to the nearer level, because the
	// point at which a nearest-level sampler switches is visible as a band across a gradient of scale.
	let between = pyramid.sample(0.5, 0.5, 0.5, sample::Spread::Clamp);
	assert!(between.alpha > 0.99, "{between:?}");
	assert!(between.red > 0.0 && between.red <= 1.0, "{between:?}");
	// And the anisotropic tap answers rather than dividing by a zero-length minor axis.
	let stretched = pyramid.sample_anisotropic(0.5, 0.5, (8.0, 0.0), (0.0, 0.0), sample::Spread::Clamp, 16);
	assert!(stretched.alpha > 0.99, "{stretched:?}");
}

#[test]
// "SUPPORTS THE STANDARD BLEND MODES" IS NOT A CONTRACT: two implementations satisfy that sentence and
// disagree on ColorBurn at zero, on SoftLight below a quarter, and on all four of the non-separable
// modes. Each equation is checked against the value the frozen registry states.
fn every_operator_and_blend_mode_matches_its_frozen_equation() {
	use composite::{BlendMode, Operator, blend, composite as apply};
	// THE ENUMERATIONS ARE THE REGISTRY'S, in its order and with its names.
	assert_eq!(composite::ALL_OPERATORS.len(), graphics_profile::compositing::OPERATORS.len());
	for (operator, entry) in composite::ALL_OPERATORS.iter().zip(graphics_profile::compositing::OPERATORS.iter()) {
		assert_eq!(operator.name(), entry.name);
		assert_eq!(operator.factors(), (entry.source_factor, entry.backdrop_factor));
	}
	assert_eq!(composite::ALL_BLEND_MODES.len(), graphics_profile::compositing::BLENDS.len() + graphics_profile::compositing::NON_SEPARABLE_BLENDS.len());

	let white = Rgba::new(1.0, 1.0, 1.0, 1.0);
	let half_red = Rgba::new(0.5, 0.0, 0.0, 0.5);
	// THE OPERATORS, against the equation `co = as*Fa*Cs + ab*Fb*Cb`.
	assert_eq!(apply(Operator::Clear, BlendMode::Normal, half_red, white), Rgba::TRANSPARENT);
	assert_eq!(apply(Operator::Src, BlendMode::Normal, half_red, white), half_red);
	assert_eq!(apply(Operator::Dst, BlendMode::Normal, half_red, white), white);
	let over = apply(Operator::SrcOver, BlendMode::Normal, half_red, white);
	assert!((over.red - 1.0).abs() < 1e-5 && (over.green - 0.5).abs() < 1e-5 && (over.alpha - 1.0).abs() < 1e-5, "{over:?}");
	let plus = apply(Operator::Plus, BlendMode::Normal, half_red, half_red);
	assert!((plus.red - 1.0).abs() < 1e-5 && (plus.alpha - 1.0).abs() < 1e-5, "additive light adds: {plus:?}");
	// `SrcIn` keeps the source only where the backdrop is, so over nothing it is nothing.
	assert_eq!(apply(Operator::SrcIn, BlendMode::Normal, half_red, Rgba::TRANSPARENT), Rgba::TRANSPARENT);

	// THE TWO WITH A DIVISION, at the endpoints where implementations disagree.
	assert_eq!(blend(BlendMode::ColorDodge, [0.0, 0.0, 0.0], [0.5, 0.5, 0.5])[0], 0.0, "a black backdrop dodges to black");
	assert_eq!(blend(BlendMode::ColorDodge, [0.5, 0.5, 0.5], [1.0, 1.0, 1.0])[0], 1.0);
	assert_eq!(blend(BlendMode::ColorBurn, [1.0, 1.0, 1.0], [0.5, 0.5, 0.5])[0], 1.0, "a white backdrop burns to white");
	assert_eq!(blend(BlendMode::ColorBurn, [0.5, 0.5, 0.5], [0.0, 0.0, 0.0])[0], 0.0);
	// SOFTLIGHT'S `D(Cb)` IS A FUNCTION OF THE BACKDROP, and its quarter threshold is on the backdrop:
	// at a backdrop of exactly a quarter the two branches of D must meet.
	let lower = blend(BlendMode::SoftLight, [0.25, 0.25, 0.25], [0.75, 0.75, 0.75])[0];
	let expected = 0.25 + (2.0 * 0.75 - 1.0) * ((((16.0 * 0.25 - 12.0) * 0.25 + 4.0) * 0.25) - 0.25);
	assert!((lower - expected).abs() < 1e-5, "{lower} against {expected}");
	// OVERLAY IS HARDLIGHT WITH THE OPERANDS SWAPPED, which is the definition rather than a coincidence.
	assert_eq!(blend(BlendMode::Overlay, [0.3, 0.3, 0.3], [0.7, 0.7, 0.7]), blend(BlendMode::HardLight, [0.7, 0.7, 0.7], [0.3, 0.3, 0.3]));

	// THE FOUR NON-SEPARABLE MODES need a luminance model, a saturation model and a gamut clip, and a
	// mode whose name does not imply the clip is the one that gets it wrong on saturated colours.
	let luminosity = blend(BlendMode::Luminosity, [0.2, 0.4, 0.6], [0.9, 0.9, 0.9]);
	let (red, green, blue) = graphics_profile::compositing::model::LUMINANCE_COEFFICIENTS;
	let light = luminosity[0] as f64 * red + luminosity[1] as f64 * green + luminosity[2] as f64 * blue;
	assert!((light - 0.9).abs() < 1e-3, "Luminosity takes the source's luminance: {light}");
	let saturated = blend(BlendMode::Saturation, [0.2, 0.4, 0.6], [0.0, 0.0, 1.0]);
	for channel in saturated {
		assert!((0.0..=1.0).contains(&channel), "the gamut clip keeps every channel representable: {saturated:?}");
	}
	// A BLEND WHERE THERE IS NO BACKDROP IS THE SOURCE. A blend mode is defined on colours, and where
	// the backdrop is transparent there is no colour to blend with.
	let over_nothing = apply(Operator::SrcOver, BlendMode::Multiply, half_red, Rgba::TRANSPARENT);
	assert!((over_nothing.red - half_red.red).abs() < 1e-5, "{over_nothing:?}");
}

#[test]
// THE STAGE ORDER IS FIXED ONCE, HERE, because each private copy of it disagreed with the others -
// and the disagreement is invisible until two implementations are compared side by side.
fn the_pipeline_round_trips_a_pixel_through_every_stage() {
	let source_semantics = ImageSemantics::Color { color_space: ColorSpace::Srgb, alpha_mode: AlphaMode::Straight };
	let target_semantics = ImageSemantics::Color { color_space: ColorSpace::Srgb, alpha_mode: AlphaMode::Straight };
	let working = pixel::Working::linear(ColorSpace::Srgb);
	let decoder = pixel::Decoder::new(&source_semantics, working).expect("a decoder");
	let encoder = pixel::Encoder::new(&target_semantics, PixelStorage::Known(PixelFormat::R8G8B8A8Unorm), working).expect("an encoder");
	// A mid-grey at full alpha: decoded to light, encoded back, it must land on itself.
	let raw = Rgba::new(0.5, 0.25, 0.75, 1.0);
	let light = decoder.decode(raw);
	assert!(light.red < raw.red, "an encoded value decodes to LESS light, which is what the transfer function does: {light:?}");
	let back = encoder.encode(light, 0, 0);
	// The dither offset is under half a step at eight bits, so the round trip lands within one step.
	for (returned, original) in [(back.red, raw.red), (back.green, raw.green), (back.blue, raw.blue)] {
		assert!((returned - original).abs() < 2.0 / 255.0, "{returned} against {original}");
	}

	// AND A LINEAR WORKING SPACE CARRYING AN ENCODED SPACE IS REFUSED, because it would be found as a
	// picture that is too dark rather than as an error.
	assert_eq!(pixel::Working::LinearPremultiplied(ColorSpace::Srgb).validate(), Err(Error::UnknownColorSpace));
	assert_eq!(pixel::Working::linear(ColorSpace::Rec2020Pq).space(), ColorSpace::Rec2020Linear, "the three Rec. 2020 transfers share one linear space");

	// TONE MAPPING WHERE THE TARGET IS NARROWER, and not clipping: the difference between a bright
	// window and a white rectangle.
	let bright = encoder.encode(Rgba::new(4.0, 4.0, 4.0, 1.0), 0, 0);
	assert!(bright.red < 1.0, "a value four times diffuse white must come back inside the target's range: {bright:?}");
	let float_target = pixel::Encoder::new(&target_semantics, PixelStorage::Known(PixelFormat::R16G16B16A16Float), working).expect("an encoder");
	let kept = float_target.encode(Rgba::new(4.0, 4.0, 4.0, 1.0), 0, 0);
	assert!(kept.red > 1.0, "a float target holds what it is given rather than tone-mapping it away: {kept:?}");
}

#[test]
// THE ONE CONVERSION PATH. A display's copy-and-scale, a screenshot's readback and a decoder's
// hand-over are the same operation with different arguments, and each private copy of it disagreed
// with the others about the stage order.
fn one_image_converts_into_another_format_and_space() {
	let source_semantics = ImageSemantics::Color { color_space: ColorSpace::Srgb, alpha_mode: AlphaMode::Straight };
	let source_layout = layout_of(PixelFormat::R8G8B8A8Unorm, 2, 1, 8, source_semantics).expect("a layout");
	let bytes = [255, 0, 0, 255, 0, 255, 0, 255];
	let source = ImageView::new(source_layout, &bytes).expect("a view");

	let target_semantics = ImageSemantics::Color { color_space: ColorSpace::SrgbLinear, alpha_mode: AlphaMode::Premultiplied };
	let target_layout = layout_of(PixelFormat::R16G16B16A16Float, 2, 1, 16, target_semantics).expect("a layout");
	let mut target = OwnedImage::new(target_layout).expect("an image");
	{
		let mut view = target.view_mut();
		pixel::convert_image(&source, &mut view, pixel::Working::linear(ColorSpace::Srgb)).expect("a conversion");
	}
	let converted = pixel::read(&target.view(), 0, 0).expect("a pixel");
	assert!((converted.red - 1.0).abs() < 0.01 && converted.green < 0.01, "{converted:?}");
	assert!((converted.alpha - 1.0).abs() < 0.01);
}

#[test]
// A TABLE IS ONLY ALLOWED TO BE FAST IF IT IS ALSO RIGHT. The exact transfer functions are the
// definition; these tables are built from them, and the agreement is held to the profile's own
// round-trip tolerance rather than to "close enough" - a tenth of a level of error is a visible band
// in a dark gradient, which is exactly where an evenly spaced table is worst.
fn the_transfer_tables_agree_with_the_exact_functions() {
	use graphics_profile::image::Transfer;
	let tolerance = graphics_profile::image::tolerance::TRANSFER_ROUND_TRIP as f32;
	for transfer in [Transfer::Srgb, Transfer::Linear, Transfer::Pq, Transfer::Hlg] {
		let table = pixel::TransferTable::new(transfer);
		let mut worst_decode = 0.0f32;
		let mut worst_encode = 0.0f32;
		for step in 0..=4096 {
			let value = step as f32 / 4096.0;
			let decoded = (table.decode(value) - color::decode(transfer, value as f64) as f32).abs();
			let encoded = (table.encode(value) - color::encode(transfer, value as f64) as f32).abs();
			worst_decode = worst_decode.max(decoded);
			worst_encode = worst_encode.max(encoded);
		}
		assert!(worst_decode <= tolerance, "{transfer:?}: decode is off by {worst_decode}");
		assert!(worst_encode <= tolerance, "{transfer:?}: encode is off by {worst_encode}");
	}
	// AND OUTSIDE `0..=1` THE EXACT FUNCTION ANSWERS, because an extended-range value is not what the
	// table covers and must not be silently clamped into it.
	let table = pixel::TransferTable::new(Transfer::Srgb);
	assert_eq!(table.encode(4.0), color::encode(Transfer::Srgb, 4.0) as f32);
	assert_eq!(table.decode(-1.0), color::decode(Transfer::Srgb, -1.0) as f32);
}
