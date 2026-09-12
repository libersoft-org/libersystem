use super::*;

fn a_face(index: u32) -> FaceIdentity {
	FaceIdentity { file: FileIdentity([7; 32]), index }
}

fn a_key() -> GlyphCacheKey {
	GlyphCacheKey { face: a_face(0), generation: Generation(1), glyph: 42, size: Fixed266::from_pixels(16), variation: VariationCoordinates::NONE, transform: TransformKey::IDENTITY, phase: SubpixelPhase { x: 0, y: 0 }, kind: GlyphKind::Outline, selection: KindSelection::default(), mode: RasterisationMode::Grayscale }
}

#[test]
// THE FIELD-BY-FIELD NEGATIVE CASE, which is the whole reason this key is written down. Each of
// these is a pair of entries that MUST NOT share a cache slot, and every one of them did under the
// key this contract replaced - face, size, transform and subpixel phase, with the glyph itself
// missing.
fn two_entries_differing_in_one_field_are_two_entries() {
	let base = a_key();
	let differing: [(&str, GlyphCacheKey); 11] = [
		("the file", GlyphCacheKey { face: FaceIdentity { file: FileIdentity([9; 32]), index: 0 }, ..base }),
		("the face index", GlyphCacheKey { face: a_face(1), ..base }),
		("the generation", GlyphCacheKey { generation: Generation(2), ..base }),
		("the glyph", GlyphCacheKey { glyph: 43, ..base }),
		("the size", GlyphCacheKey { size: Fixed266::from_pixels(17), ..base }),
		("the variation coordinates", GlyphCacheKey { variation: VariationCoordinates::new(&[4096]).expect("one axis"), ..base }),
		("the transform", GlyphCacheKey { transform: TransformKey::new([2.0, 0.0, 0.0, 2.0, 0.0, 0.0]).expect("a finite matrix"), ..base }),
		("the subpixel phase", GlyphCacheKey { phase: SubpixelPhase { x: 2, y: 0 }, ..base }),
		("the glyph kind", GlyphCacheKey { kind: GlyphKind::GrayscaleMask, ..base }),
		("the strike", GlyphCacheKey { selection: KindSelection { strike: Some(0), palette: None }, ..base }),
		("the palette", GlyphCacheKey { selection: KindSelection { strike: None, palette: Some(1) }, ..base }),
	];
	for (what, other) in differing {
		assert_ne!(base, other, "two entries differing in {what} must not collide");
	}
	// AND THE RASTERISATION MODE, which is four distinct values and not a boolean: an RGB-horizontal
	// mask and a BGR-vertical mask of one glyph are different pixels.
	let modes = [
		RasterisationMode::Grayscale,
		RasterisationMode::Subpixel(SubpixelLayout::RgbHorizontal),
		RasterisationMode::Subpixel(SubpixelLayout::BgrHorizontal),
		RasterisationMode::Subpixel(SubpixelLayout::RgbVertical),
		RasterisationMode::Subpixel(SubpixelLayout::BgrVertical),
	];
	for (index, mode) in modes.iter().enumerate() {
		for other in &modes[index + 1..] {
			assert_ne!(GlyphCacheKey { mode: *mode, ..base }, GlyphCacheKey { mode: *other, ..base }, "{mode:?} and {other:?} must not share an entry");
		}
	}
	// AND AN IDENTICAL KEY IS THE SAME ENTRY, which is the half a key that never matches would also
	// satisfy: everything above passes for a key built from a random number.
	assert_eq!(base, a_key());
}

#[test]
// ROUND-HALF-TO-EVEN AT THE ONE CONVERSION SITE. Two implementations of the same shaping have to
// produce the same bytes, and half-away-from-zero makes the answer depend on the sign.
fn a_scaled_font_unit_rounds_half_to_even_in_both_directions() {
	// 1000 units per em at 64/64 of a pixel: one unit is 1/1000 px, so these are exact ties.
	let size = Fixed266::from_pixels(1);
	let em = 128u16;
	// 1 unit at em 128 and size 64/64 is 0.5 of a 26.6 step: a tie.
	assert_eq!(Fixed266::from_font_units(1, em, size).expect("in range").raw(), 0, "0.5 rounds to even, which is 0");
	assert_eq!(Fixed266::from_font_units(3, em, size).expect("in range").raw(), 2, "1.5 rounds to even, which is 2");
	assert_eq!(Fixed266::from_font_units(-1, em, size).expect("in range").raw(), 0, "-0.5 rounds to even, which is 0");
	assert_eq!(Fixed266::from_font_units(-3, em, size).expect("in range").raw(), -2, "-1.5 rounds to even, which is -2");
	assert_eq!(Fixed266::from_font_units(5, em, size).expect("in range").raw(), 2, "2.5 rounds to even, which is 2");
	assert_eq!(Fixed266::from_font_units(7, em, size).expect("in range").raw(), 4, "3.5 rounds to even, which is 4");
	// AND WHAT IS NOT A TIE ROUNDS THE ORDINARY WAY, in both directions.
	assert_eq!(Fixed266::from_font_units(2, em, size).expect("in range").raw(), 1, "exactly 1 is 1");
	assert_eq!(Fixed266::from_font_units(-5, em, size).expect("in range").raw(), -2, "-2.5 rounds to even, which is -2");
	assert_eq!(Fixed266::from_font_units(9, em, size).expect("in range").raw(), 4, "4.5 rounds to even, which is 4 - the direction half-away-from-zero would get wrong");
	// A face with no design grid is a refusal rather than a division by zero.
	assert_eq!(Fixed266::from_font_units(1, 0, size), Err(Overflow::Value));
}

#[test]
// OVERFLOW IS A TYPED REFUSAL AND NEVER A SATURATION. A saturated advance is a position that is
// silently wrong, which is the failure that cannot be found by looking at the picture.
fn a_quantity_outside_the_representation_is_refused_rather_than_saturated() {
	assert_eq!(Fixed266::from_font_units(i32::MAX, 1, Fixed266::from_pixels(1000)), Err(Overflow::Value));
	let huge = Fixed266::from_raw(i32::MAX);
	assert_eq!(huge.checked_add(Fixed266::from_raw(1)), Err(Overflow::Sum));
	let glyphs = [
		PositionedGlyph { glyph: 1, x_offset: Fixed266::ZERO, y_offset: Fixed266::ZERO, x_advance: huge, y_advance: Fixed266::ZERO, kind: GlyphKind::Outline, selection: KindSelection::default() },
		PositionedGlyph { glyph: 2, x_offset: Fixed266::ZERO, y_offset: Fixed266::ZERO, x_advance: huge, y_advance: Fixed266::ZERO, kind: GlyphKind::Outline, selection: KindSelection::default() },
	];
	let run = GlyphRun { face: FaceRef { face: a_face(0), generation: Generation(1) }, size: Fixed266::from_pixels(16), variation: VariationCoordinates::NONE, script: ScriptTag::from_bytes(*b"Latn"), direction: Direction::LeftToRight, mode: RasterisationMode::Grayscale, origin_x: Fixed266::ZERO, origin_y: Fixed266::ZERO, glyphs: &glyphs };
	assert_eq!(run.total_advance(), Err(Overflow::Sum), "a line whose advance does not fit is one this layer will not draw");
}

#[test]
// A KEY IS HASHED AND COMPARED, so a transform in it may not be a float that compares unequal to
// itself - and two transforms that ARE equal may not hash apart.
fn a_transform_key_refuses_the_non_finite_and_normalises_negative_zero() {
	assert!(TransformKey::new([f32::NAN, 0.0, 0.0, 1.0, 0.0, 0.0]).is_none());
	assert!(TransformKey::new([1.0, 0.0, 0.0, f32::INFINITY, 0.0, 0.0]).is_none());
	let positive = TransformKey::new([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]).expect("finite");
	let negative = TransformKey::new([1.0, -0.0, 0.0, 1.0, 0.0, 0.0]).expect("finite");
	assert_eq!(positive, negative, "-0.0 and 0.0 are one transform and must be one entry");
	assert_eq!(positive, TransformKey::IDENTITY);
	assert_eq!(TransformKey::IDENTITY.matrix(), [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
}

#[test]
// THE INSTANCE IS THE COORDINATES, so a named instance and an arbitrary one that resolve to the same
// coordinates are one entry - and a face with more axes than the contract carries is a refusal
// rather than a truncated instance that would silently share another instance's cache.
fn variation_coordinates_are_bounded_and_compare_by_value() {
	assert_eq!(VariationCoordinates::new(&[]).expect("none"), VariationCoordinates::NONE);
	let named = VariationCoordinates::new(&[8192, -4096]).expect("two axes");
	let arbitrary = VariationCoordinates::new(&[8192, -4096]).expect("two axes");
	assert_eq!(named, arbitrary);
	assert_eq!(named.as_slice(), &[8192, -4096]);
	assert_ne!(named, VariationCoordinates::new(&[8192, -4097]).expect("two axes"));
	let too_many = [0i16; MAX_VARIATION_AXES + 1];
	assert!(VariationCoordinates::new(&too_many).is_none(), "a face beyond the bound is refused, not truncated");
}

#[test]
// THE PHASE IS QUANTISED, because a cache keyed on a continuous position caches nothing - and it is
// taken from the ORIGIN PLUS THE OFFSET, which is where the glyph actually lands.
fn the_subpixel_phase_is_the_quarter_pixel_the_glyph_lands_in() {
	assert_eq!(SubpixelPhase::of(Fixed266::from_raw(0), Fixed266::from_raw(0)), SubpixelPhase { x: 0, y: 0 });
	assert_eq!(SubpixelPhase::of(Fixed266::from_raw(16), Fixed266::from_raw(48)), SubpixelPhase { x: 1, y: 3 });
	assert_eq!(SubpixelPhase::of(Fixed266::from_raw(64 + 32), Fixed266::from_raw(-16)), SubpixelPhase { x: 2, y: 3 }, "a negative position has a phase inside its pixel too");
	let glyphs = [PositionedGlyph { glyph: 5, x_offset: Fixed266::from_raw(16), y_offset: Fixed266::ZERO, x_advance: Fixed266::from_pixels(8), y_advance: Fixed266::ZERO, kind: GlyphKind::SubpixelMask, selection: KindSelection::default() }];
	let run = GlyphRun { face: FaceRef { face: a_face(2), generation: Generation(9) }, size: Fixed266::from_pixels(16), variation: VariationCoordinates::NONE, script: ScriptTag::from_bytes(*b"Arab"), direction: Direction::RightToLeft, mode: RasterisationMode::Subpixel(SubpixelLayout::BgrVertical), origin_x: Fixed266::from_raw(32), origin_y: Fixed266::ZERO, glyphs: &glyphs };
	let key = run.cache_key(0).expect("the glyph is in the run");
	assert_eq!(key.phase, SubpixelPhase { x: 3, y: 0 }, "the phase is the origin plus the offset");
	assert_eq!(key.face, a_face(2));
	assert_eq!(key.generation, Generation(9));
	assert_eq!(key.kind, GlyphKind::SubpixelMask);
	assert_eq!(key.mode, RasterisationMode::Subpixel(SubpixelLayout::BgrVertical), "the mode is the run's, because it is the surface's and not the glyph's");
	assert!(run.cache_key(1).is_none());
	let scaled = TransformKey::new([2.0, 0.0, 0.0, 2.0, 0.0, 0.0]).expect("finite");
	assert_ne!(run.cache_key_under(0, scaled), run.cache_key(0));
	assert_eq!(ScriptTag::from_bytes(*b"Arab").to_bytes(), *b"Arab");
}

#[test]
// THE OWNERSHIP RULE, WHICH IS THE ONLY IMMUTABILITY TRANSITION AVAILABLE. The caller's transport
// buffer stays writable - the kernel has no seal and the caller created the object - so the library
// copies before it parses, and what it holds does not change when the caller writes.
fn taking_the_bytes_is_what_makes_them_immutable() {
	let face = FaceRef { face: a_face(0), generation: Generation(3) };
	let mut transport = alloc::vec![1u8, 2, 3, 4];
	let held = FaceBytes::copy_from(face, &transport);
	transport[0] = 0xff;
	assert_eq!(held.bytes(), &[1, 2, 3, 4], "what the library holds is its own copy");
	assert_eq!(held.face(), face);
	assert!(held.is_current(Generation(3)));
	assert!(!held.is_current(Generation(4)), "a replaced face invalidates what was derived from it");
}

fn cluster(start: u32, end: u32, first_glyph: u16, glyph_count: u16) -> Cluster {
	Cluster { source: SourceRange { start, end }, first_glyph, glyph_count, first_caret: 0, caret_count: 0 }
}

#[test]
// THE MAPPING CHECKS ITS OWN INVARIANTS, because every one of these has been a real bug in a text
// stack and none of them is visible at the call that consumes the mapping.
fn a_cluster_mapping_is_refused_when_it_is_not_one() {
	let good = [cluster(0, 1, 0, 1), cluster(1, 3, 1, 1)];
	assert!(ClusterMap::new(&good, &[0, 1], &[], 3).is_some());
	// OVERLAPPING CLUSTERS: one byte in two clusters, so a hit test has two answers.
	let overlapping = [cluster(0, 2, 0, 1), cluster(1, 3, 1, 1)];
	assert!(ClusterMap::new(&overlapping, &[0, 1], &[], 3).is_none());
	// OUT OF THE SOURCE: a cluster naming bytes the string does not have.
	let outside = [cluster(0, 1, 0, 1), cluster(1, 9, 1, 1)];
	assert!(ClusterMap::new(&outside, &[0, 1], &[], 3).is_none());
	// NOT A PERMUTATION: two clusters drawn at one visual position.
	assert!(ClusterMap::new(&good, &[0, 0], &[], 3).is_none());
	// A CARET SLICE OUTSIDE THE STORAGE.
	let ligature = [Cluster { source: SourceRange { start: 0, end: 3 }, first_glyph: 0, glyph_count: 1, first_caret: 0, caret_count: 2 }];
	assert!(ClusterMap::new(&ligature, &[0], &[Fixed266::from_pixels(3)], 3).is_none());
	assert!(ClusterMap::new(&ligature, &[0], &[Fixed266::from_pixels(3), Fixed266::from_pixels(6)], 3).is_some());
}

#[test]
// A LIGATURE HAS CARET STOPS INSIDE IT, which one cluster-start integer cannot express: `ffi` is one
// glyph for three characters and a caret has to be able to stand between them.
fn a_ligature_carries_the_caret_positions_inside_it() {
	let clusters = [Cluster { source: SourceRange { start: 0, end: 3 }, first_glyph: 0, glyph_count: 1, first_caret: 0, caret_count: 2 }];
	let carets = [Fixed266::from_pixels(3), Fixed266::from_pixels(6)];
	let map = ClusterMap::new(&clusters, &[0], &carets, 3).expect("a valid mapping");
	assert_eq!(map.intra_ligature_carets(0), &carets);
	assert_eq!(map.intra_ligature_carets(1), &[] as &[Fixed266]);
	assert_eq!(map.cluster_at(1), Some(0), "every byte of the ligature is in its cluster");
	assert_eq!(map.cluster_at(3), None);
	assert_eq!(map.source_len(), 3);
	assert_eq!(map.clusters().len(), 1);
	// AFFINITY IS A VALUE AND NOT AN ASSUMPTION: one offset, two places.
	assert_ne!(Caret { offset: 3, affinity: CaretAffinity::Trailing }, Caret { offset: 3, affinity: CaretAffinity::Leading }, "a boundary between two directions has two caret positions at one offset");
}

#[test]
// A CONTIGUOUS RANGE OF TEXT IS NOT A CONTIGUOUS RUN OF PIXELS in mixed-direction content, and a
// consumer that assumed it was draws a highlight over text nobody selected.
fn a_bidi_selection_can_be_discontiguous_and_the_mapping_says_so() {
	// Three clusters written in logical order, the middle one drawn last: `A [c b] ` reordered.
	let clusters = [cluster(0, 1, 0, 1), cluster(1, 2, 1, 1), cluster(2, 3, 2, 1)];
	let map = ClusterMap::new(&clusters, &[0, 2, 1], &[], 3).expect("a valid mapping");
	assert_eq!(map.visual_of_logical(1), Some(2));
	assert_eq!(map.logical_of_visual(2), Some(1));
	assert_eq!(map.logical_of_visual(9), None);
	// Logical 0 and 1 are drawn at visual 0 and 2, with another cluster between them.
	assert!(!map.selection_is_contiguous(SourceRange { start: 0, end: 2 }), "the selection is two pieces on screen");
	assert!(map.selection_is_contiguous(SourceRange { start: 1, end: 3 }), "visual 1 and 2 are adjacent");
	assert!(map.selection_is_contiguous(SourceRange { start: 0, end: 0 }), "an empty selection is contiguous");
}

#[test]
// A RUN IS HOMOGENEOUS BY CONSTRUCTION: the face, script, direction and mode are carried once, so a
// run of two faces is not a thing this type can hold. What varies per glyph is the KIND, because one
// shaped run can mix an outline and a colour bitmap from the same face.
fn a_run_carries_one_face_and_reports_which_kinds_are_in_it() {
	let glyphs = [
		PositionedGlyph { glyph: 1, x_offset: Fixed266::ZERO, y_offset: Fixed266::ZERO, x_advance: Fixed266::from_pixels(6), y_advance: Fixed266::ZERO, kind: GlyphKind::Outline, selection: KindSelection::default() },
		PositionedGlyph { glyph: 2, x_offset: Fixed266::ZERO, y_offset: Fixed266::ZERO, x_advance: Fixed266::from_pixels(6), y_advance: Fixed266::ZERO, kind: GlyphKind::ColrPaintGraph, selection: KindSelection { strike: None, palette: Some(0) } },
	];
	let run = GlyphRun { face: FaceRef { face: a_face(0), generation: Generation(1) }, size: Fixed266::from_pixels(16), variation: VariationCoordinates::NONE, script: ScriptTag::from_bytes(*b"Latn"), direction: Direction::LeftToRight, mode: RasterisationMode::Grayscale, origin_x: Fixed266::ZERO, origin_y: Fixed266::ZERO, glyphs: &glyphs };
	assert_eq!(run.total_advance().expect("in range"), (Fixed266::from_pixels(12), Fixed266::ZERO));
	assert_eq!(run::kinds_in(&run), [true, false, false, false, false, true]);
	assert_eq!(run.cache_key(0).expect("in the run").selection, KindSelection::default());
	assert_eq!(run.cache_key(1).expect("in the run").selection.palette, Some(0));
}
