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
	//
	// THE CLAIM IS THE ORDER AND NOT ONE VALUE. This read "four times diffuse white comes back below
	// one", and the profile's curve sends exactly `WHITE` to exactly one - so that assertion would
	// pass or fail on the dither offset at one pixel. What "not a white rectangle" means is that a
	// window at two, three and four times diffuse white is three DIFFERENT outputs.
	let over = [2.0f32, 3.0, 4.0].map(|value| encoder.encode(Rgba::new(value, value, value, 1.0), 0, 0).red);
	assert!(over[0] < over[1] && over[1] < over[2], "a bright window keeps its gradations: {over:?}");
	assert!(over[0] < 1.0, "two times diffuse white is inside the range: {over:?}");
	assert!(over[2] <= 1.0 + 1.0 / 255.0, "and four times lands at the top of it rather than past: {over:?}");
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

#[test]
// EVERY MANDATORY FORMAT BEING RGB IS WHY A VIDEO PLAYER CONVERTS EVERY FRAME BEFORE ANYTHING WILL
// DRAW IT. The planar model is what removes that conversion, and its arithmetic is the part a recipe
// written for RGB gets wrong: the matrix and the range come FIRST, on code values, and produce
// ENCODED RGB - the transfer function has not been touched yet.
fn the_planar_model_reconstructs_reads_and_refuses_by_the_frozen_rules() {
	use planar::{MultiPlaneLayout, MultiPlaneView, PlanarFormat, YuvMatrix, YuvRange};

	// THE REGISTRY IS THE LIST AND THIS IS THE CODE. Three layouts and three matrices, by name.
	assert_eq!(planar::ALL_PLANAR_FORMATS.len(), graphics_profile::image::YUV_LAYOUTS.len());
	for (format, described) in planar::ALL_PLANAR_FORMATS.iter().zip(graphics_profile::image::YUV_LAYOUTS.iter()) {
		assert_eq!(format.name(), described.name);
		assert_eq!(format.planes(), described.planes as usize);
		assert_eq!(format.bits(), described.bits);
	}
	assert_eq!(planar::ALL_YUV_MATRICES.len(), graphics_profile::image::YUV_MATRICES.len());

	// AN INTERLEAVED CHROMA ROW IS TWICE ITS SAMPLE COUNT AND A PLANAR ONE IS ONCE, which is the
	// arithmetic that puts a decoder half a row out.
	let extent = Extent2D::new(4, 4);
	assert_eq!(PlanarFormat::Nv12.minimum_row_bytes(1, extent), Some(4), "two chroma samples for each of two columns");
	assert_eq!(PlanarFormat::I420.minimum_row_bytes(1, extent), Some(2), "one sample for each of two columns");
	assert_eq!(PlanarFormat::P010.minimum_row_bytes(0, extent), Some(8), "ten bits live in a sixteen-bit word");
	// AN ODD EXTENT'S CHROMA PLANE IS THE CEILING OF HALF THE LUMA EXTENT.
	assert_eq!(PlanarFormat::Nv12.plane_extent(1, Extent2D::new(5, 5)), Some(Extent2D::new(3, 3)));

	// A MALFORMED PLANE IS A REFUSAL AND NOT A CLAMP: a chroma pitch half a row short is a decoder's
	// arithmetic error, and accepting it draws a picture that shears.
	assert_eq!(MultiPlaneLayout::new(extent, PlanarFormat::Nv12, YuvMatrix::Bt709, YuvRange::Limited, ColorSpace::Srgb, [4, 2, 0]).err(), Some(Error::PitchTooSmall));

	// A LIMITED-RANGE WHITE IS 235, NOT 255, and a full-range one is 255 - which is the single
	// commonest cause of washed-out or crushed video and is not detectable from the samples.
	let white = |range: YuvRange, luma: u8| {
		let layout = MultiPlaneLayout::new(extent, PlanarFormat::Nv12, YuvMatrix::Bt709, range, ColorSpace::Srgb, [4, 4, 0]).expect("a layout");
		let luma_plane = [luma; 16];
		let chroma_plane = [128u8; 16];
		let view = MultiPlaneView::new(layout, [&luma_plane, &chroma_plane, &[]]).expect("a view");
		view.encoded_rgb(1, 1)
	};
	let limited = white(YuvRange::Limited, 235);
	assert!((limited.red - 1.0).abs() < 0.01 && (limited.green - 1.0).abs() < 0.01 && (limited.blue - 1.0).abs() < 0.01, "limited-range white: {limited:?}");
	let full = white(YuvRange::Full, 255);
	assert!((full.red - 1.0).abs() < 0.01, "full-range white: {full:?}");
	// AND THE SAME BYTES READ AS THE OTHER RANGE ARE A DIFFERENT COLOUR, which is what makes the flag
	// part of the image rather than a hint.
	let misread = white(YuvRange::Full, 235);
	assert!(misread.red < 0.95, "235 read as full range is not white: {misread:?}");

	// THE MATRIX IS DERIVED FROM ITS TWO COEFFICIENTS, so BT.601 and BT.709 disagree about the same
	// bytes - which they must, or one of them is not implemented.
	let coloured = |matrix: YuvMatrix| {
		let layout = MultiPlaneLayout::new(extent, PlanarFormat::Nv12, matrix, YuvRange::Limited, ColorSpace::Srgb, [4, 4, 0]).expect("a layout");
		let luma_plane = [120u8; 16];
		let chroma_plane = [200u8, 90, 200, 90, 200, 90, 200, 90, 200, 90, 200, 90, 200, 90, 200, 90];
		let view = MultiPlaneView::new(layout, [&luma_plane, &chroma_plane, &[]]).expect("a view");
		view.encoded_rgb(1, 1)
	};
	assert!(coloured(YuvMatrix::Bt601) != coloured(YuvMatrix::Bt709));
	assert!(coloured(YuvMatrix::Bt709) != coloured(YuvMatrix::Bt2020));

	// `P010` IS THE HIGH TEN BITS OF A LITTLE-ENDIAN WORD, and the low six are ignored on read: the
	// same ten-bit value with noise in its low bits is the same colour.
	let ten_bit = |low: u8| {
		let layout = MultiPlaneLayout::new(extent, PlanarFormat::P010, YuvMatrix::Bt709, YuvRange::Limited, ColorSpace::Srgb, [8, 8, 0]).expect("a layout");
		let word = (940u16 << 6) | low as u16;
		let mut luma_plane = [0u8; 32];
		for pair in luma_plane.chunks_exact_mut(2) {
			pair.copy_from_slice(&word.to_le_bytes());
		}
		let mut chroma_plane = [0u8; 32];
		for pair in chroma_plane.chunks_exact_mut(2) {
			pair.copy_from_slice(&((512u16 << 6).to_le_bytes()));
		}
		let view = MultiPlaneView::new(layout, [&luma_plane, &chroma_plane, &[]]).expect("a view");
		view.encoded_rgb(1, 1)
	};
	assert_eq!(ten_bit(0), ten_bit(63), "the low six bits are ignored on read");
	assert!((ten_bit(0).red - 1.0).abs() < 0.01, "940 of 64..940 is white at ten bits: {:?}", ten_bit(0));

	// I420's THREE PLANES SAY THE SAME THING AS NV12's TWO, which is what makes them one model.
	let planar_white = {
		let layout = MultiPlaneLayout::new(extent, PlanarFormat::I420, YuvMatrix::Bt709, YuvRange::Limited, ColorSpace::Srgb, [4, 2, 2]).expect("a layout");
		let luma_plane = [235u8; 16];
		let cb = [128u8; 4];
		let cr = [128u8; 4];
		let view = MultiPlaneView::new(layout, [&luma_plane, &cb, &cr]).expect("a view");
		view.encoded_rgb(1, 1)
	};
	let interleaved_white = white(YuvRange::Limited, 235);
	assert!((planar_white.red - interleaved_white.red).abs() < 0.01 && (planar_white.blue - interleaved_white.blue).abs() < 0.01, "{planar_white:?} against {interleaved_white:?}");

	// A CROP MUST LAND ON A CHROMA SAMPLE, or the crop's own chroma is between two of them.
	assert!(planar::crop_is_aligned(PlanarFormat::Nv12, (2, 4), Extent2D::new(4, 2)));
	assert!(!planar::crop_is_aligned(PlanarFormat::Nv12, (1, 4), Extent2D::new(4, 2)));
	assert!(!planar::crop_is_aligned(PlanarFormat::Nv12, (2, 4), Extent2D::new(3, 2)));

	// AND A SHORT PLANE IS REFUSED RATHER THAN READ PAST.
	let layout = MultiPlaneLayout::new(extent, PlanarFormat::Nv12, YuvMatrix::Bt709, YuvRange::Limited, ColorSpace::Srgb, [4, 4, 0]).expect("a layout");
	let short = [0u8; 8];
	let chroma = [128u8; 16];
	assert_eq!(MultiPlaneView::new(layout, [&short, &chroma, &[]]).err(), Some(Error::BufferTooShort));
}

#[test]
// A VIDEO FRAME IS SAMPLED BY THE SAME SAMPLER AS EVERYTHING ELSE. The plane reconstruction and the
// matrix are the only things that differ, and they happen before the shared pipeline starts: the
// transfer function, the primaries, the premultiply and the filter are one implementation.
fn a_planar_source_samples_through_the_shared_pipeline() {
	use planar::{MultiPlaneLayout, MultiPlaneView, PlanarFormat, YuvMatrix, YuvRange};
	let extent = Extent2D::new(4, 4);
	let layout = MultiPlaneLayout::new(extent, PlanarFormat::Nv12, YuvMatrix::Bt709, YuvRange::Limited, ColorSpace::Srgb, [4, 4, 0]).expect("a layout");
	let luma_plane = [235u8; 16];
	let chroma_plane = [128u8; 16];
	let view = MultiPlaneView::new(layout, [&luma_plane, &chroma_plane, &[]]).expect("a view");
	let working = pixel::Working::linear(ColorSpace::Srgb);
	let sampler = sample::Sampler::planar(view, working, sample::Spread::Clamp, None).expect("a sampler");
	// WHITE IN, WHITE OUT - in LIGHT, which means the sRGB transfer function was applied after the
	// matrix and not before it.
	let texel = sampler.texel(1, 1);
	assert!((texel.red - 1.0).abs() < 0.02 && (texel.alpha - 1.0).abs() < 0.01, "{texel:?}");
	// A mid-grey luma decodes to LESS than half the light, which is the transfer function having been
	// applied at all.
	let dark = {
		let luma_plane = [126u8; 16];
		let view = MultiPlaneView::new(layout, [&luma_plane, &chroma_plane, &[]]).expect("a view");
		let sampler = sample::Sampler::planar(view, working, sample::Spread::Clamp, None).expect("a sampler");
		sampler.texel(1, 1)
	};
	assert!(dark.red < 0.3 && dark.red > 0.1, "a mid-grey code value is a fifth of the light: {dark:?}");

	// AND A PYRAMID IS BUILT FROM THE SAME SAMPLER, so a scaled video frame is filtered in the same
	// light as a scaled image - not through a second reconstruction of its own.
	let view = MultiPlaneView::new(layout, [&luma_plane, &chroma_plane, &[]]).expect("a view");
	let sampler = sample::Sampler::planar(view, working, sample::Spread::Clamp, None).expect("a sampler");
	let pyramid = sample::Pyramid::from_sampler(&sampler, working).expect("a pyramid");
	assert_eq!(pyramid.levels(), 3, "four by four halves twice");
	let top = pyramid.level(2).expect("the top");
	let averaged = pixel::read(&top, 0, 0).expect("its one texel");
	assert!((averaged.red - 1.0).abs() < 0.02, "an all-white frame averages to white: {averaged:?}");
}

// ---------------------------------------------------------------------------------------------
// The wire boundary: `liber:graphics@1`'s descriptors becoming this library's validated types.
//
// WHAT THESE HOLD IS THAT THERE IS NO OTHER WAY IN. Each case is a descriptor a peer could send and
// a consumer would otherwise trust: a pitch that does not cover a row, an extent of zero, an alpha
// mode the format has no channel for. The refusals are `ImageLayout::new`'s and are tested there
// too; what is tested HERE is that crossing the boundary reaches them.
// ---------------------------------------------------------------------------------------------

use graphics_proto::generated::liber::graphics::v1 as gw;

fn wire_layout(width: u32, height: u32, pitch: u32, format: gw::PixelFormat, alpha: gw::AlphaMode) -> gw::ImageLayout {
	gw::ImageLayout { size: gw::Extent2d { width, height }, pitch, format, alpha, color_space: gw::ColorSpace::Srgb, origin: gw::RowOrigin::TopLeft }
}

#[test]
fn a_wire_descriptor_whose_pitch_does_not_cover_a_row_is_refused_at_the_boundary() {
	// 4 bytes per pixel over 16 pixels is 64, and 63 is the descriptor that makes every row after
	// the first read into the one before it.
	let short = wire_layout(16, 8, 63, gw::PixelFormat::B8g8r8a8Unorm, gw::AlphaMode::Premultiplied);
	assert_eq!(ImageLayout::try_from(&short), Err(Error::PitchTooSmall));
	// And the pitch that exactly covers it is admitted, so the bound is the row and not a margin.
	let exact = wire_layout(16, 8, 64, gw::PixelFormat::B8g8r8a8Unorm, gw::AlphaMode::Premultiplied);
	assert!(ImageLayout::try_from(&exact).is_ok());
}

#[test]
fn a_wire_descriptor_with_no_pixels_in_it_is_refused() {
	// A ZERO EXTENT IS A REFUSAL AND NOT AN EMPTY IMAGE: every consumer that divides by an extent
	// would otherwise have to check, and one of them will not.
	for (width, height) in [(0u32, 8u32), (16, 0), (0, 0)] {
		let empty = wire_layout(width, height, 64, gw::PixelFormat::B8g8r8a8Unorm, gw::AlphaMode::Premultiplied);
		assert_eq!(ImageLayout::try_from(&empty), Err(Error::ZeroExtent), "{width}x{height} was admitted");
	}
}

#[test]
fn a_wire_descriptor_claiming_alpha_a_format_does_not_have_is_refused() {
	// `B8G8R8X8` has four bytes and three channels: the fourth is unused, so `Straight` describes a
	// channel that is not there. Refusing it at the boundary keeps the combination out of every
	// backend's match arms.
	let impossible = wire_layout(16, 8, 64, gw::PixelFormat::B8g8r8x8Unorm, gw::AlphaMode::Straight);
	assert!(ImageLayout::try_from(&impossible).is_err());
}

#[test]
fn a_validated_layout_round_trips_through_the_wire_unchanged() {
	// THE DIRECTION THAT CANNOT FAIL. A validated colour layout is expressible by construction, so
	// `to_wire` answers `Some` - and the value that comes back through `try_from` is the same one.
	for format in [gw::PixelFormat::B8g8r8a8Unorm, gw::PixelFormat::R8g8b8a8Unorm, gw::PixelFormat::R16g16b16a16Float] {
		for origin in [gw::RowOrigin::TopLeft, gw::RowOrigin::BottomLeft] {
			let mut sent = wire_layout(32, 4, 256, format, gw::AlphaMode::Premultiplied);
			sent.origin = origin;
			sent.color_space = gw::ColorSpace::Rec2020Pq;
			let validated = ImageLayout::try_from(&sent).expect("a descriptor this test built is admissible");
			let back = validated.to_wire().expect("a colour layout is expressible on the wire");
			assert_eq!(back, sent);
		}
	}
}

#[test]
fn a_layout_the_wire_cannot_describe_answers_none_rather_than_inventing_one() {
	// The wire record holds a colour space and an alpha mode and has nothing that could say the
	// image is depth or a mask. Answering `None` is the honest form; inventing a colour space for a
	// depth buffer is the quiet nonsense a boundary exists to stop.
	let depth = ImageLayout::new(Extent2D { width: 8, height: 8 }, 32, PixelStorage::Known(PixelFormat::R32Uint), RowOrigin::TopLeft, ImageSemantics::Depth).expect("a depth layout");
	assert!(depth.to_wire().is_none());
}

#[test]
// A NAMED SCANOUT FORMAT AND A MODE LINE ARE TWO WAYS OF SAYING ONE THING, and the tree used to say
// it in three places: a boot console, a blitter's destination and a display client each kept its own
// `16/8/0`. One of those is right for `B8G8R8X8` and wrong for `R8G8B8X8`, and nothing would have
// caught the difference but a screen full of swapped colours.
fn a_named_packed_format_describes_itself_as_masks() {
	let bgr = PixelFormat::B8G8R8X8Unorm.packed_masks().expect("a scanout format has masks");
	// The NAME is memory order and the SHIFTS are not: blue is the first byte, so on a little-endian
	// element blue sits at 0 and red at 16.
	assert_eq!((bgr.red.shift, bgr.green.shift, bgr.blue.shift), (16, 8, 0));
	assert_eq!(PixelFormat::B8G8R8A8Unorm.packed_masks(), Some(bgr), "the two BGR formats differ in their fourth lane, which these masks do not describe");

	let rgb = PixelFormat::R8G8B8X8Unorm.packed_masks().expect("the firmware RGB hand-off has masks");
	assert_eq!((rgb.red.shift, rgb.green.shift, rgb.blue.shift), (0, 8, 16), "the mirror of the BGR pair, which a hard-coded table got wrong");
	assert_eq!(PixelFormat::R8G8B8A8Unorm.packed_masks(), Some(rgb));

	// THE FOURTH LANE IS NOT DESCRIBED. The shared packer writes a declared reserved span with all
	// its bits set, so describing one here would turn a blitter's `0x00rrggbb` into `0xffrrggbb` and
	// write over a destination's alpha. (Measured: three blit tests failed on exactly that byte.)
	assert_eq!(bgr.reserved.bits, 0, "the fourth lane belongs to whoever owns the format");
	assert!(bgr.validate().is_ok(), "and what it describes is a layout the packer accepts");

	// EVERYTHING ELSE IS HONEST ABOUT NOT BEING FOUR 8-BIT SPANS, rather than being handed BGRA's.
	for format in [
		PixelFormat::A8Unorm,
		PixelFormat::R8Unorm,
		PixelFormat::R8G8Unorm,
		PixelFormat::R10G10B10A2Unorm,
		PixelFormat::R16G16B16A16Unorm,
		PixelFormat::R16G16B16A16Float,
		PixelFormat::R32Uint,
		PixelFormat::R32G32B32A32Float,
	] {
		assert_eq!(format.packed_masks(), None, "{} is not four 8-bit channels", format.name());
	}

	// AND THE STORAGE ASKS THE SAME QUESTION FOR BOTH ARMS, so a writer that packs never matches on
	// which arm described its destination.
	assert_eq!(PixelStorage::Known(PixelFormat::B8G8R8X8Unorm).packed_masks(), Some(bgr));
	assert_eq!(PixelStorage::PackedRgbUnorm(bgr).packed_masks(), Some(bgr));
	assert_eq!(PixelStorage::Known(PixelFormat::R32Uint).packed_masks(), None);
}

#[test]
// THE SCANOUT LAYOUT IS BUILT IN FOUR PLACES AND DECIDED IN ONE. A boot console, a display client's
// mapping, a terminal renderer and a blitter's destination each restated "top-left, opaque, sRGB" -
// three constants that are not a choice at this layer, and three chances to differ.
fn a_scanout_layout_carries_the_constants_a_scanout_does_not_choose() {
	let masks = PackedRgbLayout::from_masks(4, (16, 8), (8, 8), (0, 8)).expect("a four-byte mode line");
	let layout = ImageLayout::scanout(Extent2D::new(64, 32), 64 * 4, PixelStorage::PackedRgbUnorm(masks)).expect("the ordinary firmware hand-off");
	assert_eq!(layout.origin, RowOrigin::TopLeft);
	assert_eq!(layout.semantics, ImageSemantics::Color { color_space: ColorSpace::Srgb, alpha_mode: AlphaMode::Opaque });
	assert_eq!(layout.storage, PixelStorage::PackedRgbUnorm(masks));
	assert_eq!(layout.minimum_visible_bytes(), Some(64 * 4 * 32));

	// IT IS THE CHECKED CONSTRUCTOR AND NOT A SHORTHAND AROUND IT: a pitch that does not hold a row
	// is refused here exactly as it is there.
	assert_eq!(ImageLayout::scanout(Extent2D::new(64, 32), 64 * 4 - 1, PixelStorage::PackedRgbUnorm(masks)).err(), Some(Error::PitchTooSmall));
	// An element size a mode line cannot have is refused before a layout exists at all.
	assert_eq!(PackedRgbLayout::from_masks(256, (16, 8), (8, 8), (0, 8)), None);
	// And the six numbers never describe a reserved span, for the reason stated above.
	assert_eq!(masks.reserved.bits, 0);
}

#[test]
// A COLOUR SPACE NAME IS NOT ENOUGH TO TONE MAP WITH, which is this test's whole point: the same
// bright pixel encodes differently for a dim panel and for a bright one, and identically for two
// displays that said nothing.
fn the_tone_map_asks_the_destination_what_it_can_show() {
	use crate::pixel::{Encoder, OutputLuminance, Working};
	let working = Working::linear(ColorSpace::Srgb);
	let semantics = ImageSemantics::Color { color_space: ColorSpace::Srgb, alpha_mode: AlphaMode::Premultiplied };
	let storage = PixelStorage::Known(PixelFormat::R8G8B8A8Unorm);

	// A destination that said nothing gets the profile's own stated white point, and that is the
	// ASSUMPTION WRITTEN DOWN ONCE rather than one made per conversion.
	let unknown = Encoder::new(&semantics, storage, working).expect("an encoder");
	assert_eq!(unknown.tone_map_white(), graphics_profile::image::tone_map::WHITE);
	assert_eq!(OutputLuminance::UNKNOWN.tone_map_white(), graphics_profile::image::tone_map::WHITE);
	assert_eq!(OutputLuminance::default(), OutputLuminance::UNKNOWN);

	// A thousand-nit display whose diffuse white is the profile's 203 maps its peak at about five
	// times white, which is a different curve from the default four.
	let bright = OutputLuminance { sdr_white_nits: Some(203.0), max_nits: Some(1015.0), min_nits: Some(0.1), max_frame_average_nits: Some(600.0) };
	assert!((bright.tone_map_white() - 5.0).abs() < 1e-6);
	let encoder = Encoder::new_for_output(&semantics, storage, working, bright).expect("an encoder");
	assert_eq!(encoder.tone_map_white(), bright.tone_map_white());

	// AND THE TWO CURVES PRODUCE DIFFERENT PIXELS, which is what makes the metadata worth carrying: a
	// highlight at three times diffuse white is darker on the panel that cannot show it.
	let highlight = crate::pixel::Rgba::new(3.0, 3.0, 3.0, 1.0);
	assert_ne!(unknown.encode(highlight, 0, 0), encoder.encode(highlight, 0, 0), "the destination's luminance changed the pixel");

	// A DISPLAY DESCRIPTION THAT CANNOT BE TRUE FALLS BACK rather than computing a curve from
	// nonsense: a zero white, a peak below diffuse white, and a non-finite number are each refused.
	for impossible in [
		OutputLuminance { sdr_white_nits: Some(0.0), max_nits: Some(1000.0), ..OutputLuminance::UNKNOWN },
		OutputLuminance { sdr_white_nits: Some(203.0), max_nits: Some(100.0), ..OutputLuminance::UNKNOWN },
		OutputLuminance { sdr_white_nits: Some(f32::NAN), max_nits: Some(1000.0), ..OutputLuminance::UNKNOWN },
		OutputLuminance { sdr_white_nits: Some(203.0), max_nits: Some(f32::INFINITY), ..OutputLuminance::UNKNOWN },
		// Half a description is not a description.
		OutputLuminance { max_nits: Some(1000.0), ..OutputLuminance::UNKNOWN },
		OutputLuminance { sdr_white_nits: Some(203.0), ..OutputLuminance::UNKNOWN },
	] {
		assert_eq!(impossible.tone_map_white(), graphics_profile::image::tone_map::WHITE, "{impossible:?} is not a display");
	}
}

// WHAT A CHECKED LAYOUT HELPER OWES AN ADVERSARIAL DESCRIPTION, WHICH IS AN ANSWER AND NEVER A WRAP.
//
// EVERY EXTENT AND PITCH IN THIS TREE COMES FROM SOMEWHERE ELSE: firmware's mode line, a driver's
// scanout record, a client's surface request. Each of those is a number this process did not choose,
// and each is multiplied by another one to decide how many bytes something may touch - so the one
// thing these helpers must never do is answer a SMALL number for a description whose real span does
// not fit. A wrap here is an out-of-bounds read with a length check in front of it that passed.
//
// SWEPT RATHER THAN FUZZED, for the reason the codec sweep beside it gives: a deterministic sweep is
// what a build gate can run. What is swept is the boundary set - zero, one, the powers of two either
// side of a `u32`, and the maxima - in both axes and in the pitch, across every format the registry
// carries.
#[test]
fn a_checked_layout_answers_or_refuses_and_never_wraps() {
	const EDGES: [u32; 10] = [0, 1, 2, 3, 4, 0xffff, 0x1_0000, 0x7fff_ffff, 0xffff_fffe, u32::MAX];
	for &format in format::ALL_FORMATS.iter() {
		for &width in EDGES.iter() {
			for &height in EDGES.iter() {
				for &pitch in EDGES.iter() {
					let Ok(layout) = ImageLayout::scanout(Extent2D::new(width, height), pitch, PixelStorage::Known(format)) else {
						continue;
					};
					// A LAYOUT THAT VALIDATED CARRIES ITS OWN INVARIANT: the pitch holds a row.
					let row = layout.minimum_row_bytes().expect("a validated layout has a minimum row");
					assert!(pitch >= row, "{format:?} {width}x{height} pitch {pitch} validated below its own minimum row {row}");
					assert!(width > 0 && height > 0, "a zero extent is a refusal and not an empty image");
					// AND THE TWO SPANS ARE ORDERED. The backend that reads the final row's padding
					// may touch at least as much as the one that does not, and a helper that
					// answered otherwise would be describing a driver reading behind itself.
					match (layout.backend_access_span(false), layout.backend_access_span(true)) {
						(Some(visible), Some(whole)) => {
							assert!(whole >= visible, "{format:?} {width}x{height} pitch {pitch}: the padded span {whole} is smaller than the visible one {visible}");
							// AND NEITHER WRAPPED. Recomputed in `u128`, where nothing this tree
							// can express overflows, so a `u64` answer that is smaller than the
							// truth is a failure here rather than a read past a buffer somewhere.
							let exact = u128::from(height) * u128::from(pitch);
							assert_eq!(u128::from(whole), exact, "{format:?} {width}x{height} pitch {pitch}: the padded span is not height by pitch");
							let exact_visible = (u128::from(height) - 1) * u128::from(pitch) + u128::from(row);
							assert_eq!(u128::from(visible), exact_visible, "{format:?} {width}x{height} pitch {pitch}: the visible span is not what whole rows plus a last row is");
						}
						// A SPAN THAT DOES NOT FIT IS `None` AND NOT A WRAPPED NUMBER, which is the
						// whole point of the checked arithmetic underneath.
						(visible, whole) => {
							let exact = u128::from(height) * u128::from(pitch);
							assert!(exact > u128::from(u64::MAX) || whole.is_none() || visible.is_none(), "{format:?} {width}x{height} pitch {pitch}: a span that fits was refused");
						}
					}
					// The pixel count is the same question in the other unit, and has the same duty.
					if let Some(pixels) = layout.pixels() {
						assert_eq!(u128::from(pixels), u128::from(width) * u128::from(height), "{format:?} {width}x{height}: the pixel count wrapped");
					}
				}
			}
		}
	}
}
