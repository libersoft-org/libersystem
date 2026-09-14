use super::*;

use graphics_core::ColorSpace;
use graphics_core::geom::{PointF, RectF};
use graphics_core::pixel::OutputLuminance;

fn rect_path(rect: RectF) -> Path {
	let mut builder = PathBuilder::new();
	builder.add_rect(rect).expect("a rectangle is four lines");
	builder.finish()
}

fn red() -> Paint {
	Paint::Solid(Color::new(1.0, 0.0, 0.0, 1.0, ColorSpace::SrgbLinear))
}

#[test]
// WHAT A CANVAS PRODUCES IS A LIST AND NOT PIXELS, and that is what lets this whole layer be tested
// with no backend at all - which is the property the indirection was chosen for, before the GPU
// backend it was also chosen for exists.
fn a_canvas_records_a_list_rather_than_drawing() {
	let mut canvas = Canvas::new();
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 10.0, 10.0)), red(), FillRule::NonZero).expect("a fill");
	canvas.set_opacity(0.5);
	canvas.fill_path(rect_path(RectF::new(5.0, 5.0, 10.0, 10.0)), red(), FillRule::EvenOdd).expect("a fill");
	let list = canvas.finish().expect("a balanced recording");

	assert_eq!(list.version(), DRAW_LIST_VERSION);
	assert_eq!(list.commands().len(), 2);
	// THE STATE AT THE TIME OF THE CALL IS RECORDED WITH IT, not looked up at replay: a list whose
	// commands referred to a mutable state object would draw differently depending on when it was
	// replayed.
	match &list.commands()[1] {
		Command::FillPath { opacity, rule, .. } => {
			assert_eq!(*opacity, 0.5);
			assert_eq!(*rule, FillRule::EvenOdd);
		}
		other => panic!("expected a fill, got {other:?}"),
	}
}

#[test]
// A COMPONENT THAT DRAWS THE SAME ROUNDED RECTANGLE FORTY TIMES RECORDS IT ONCE. Deduplication is not
// an optimisation here: a list that stored forty copies would exceed the resource ceiling for a
// drawing that has one shape in it.
fn identical_resources_are_recorded_once() {
	let mut canvas = Canvas::new();
	let rect = RectF::new(0.0, 0.0, 4.0, 4.0);
	for _ in 0..8 {
		canvas.fill_path(rect_path(rect), red(), FillRule::NonZero).expect("a fill");
	}
	let list = canvas.finish().expect("a balanced recording");
	assert_eq!(list.commands().len(), 8);
	assert_eq!(list.resources().paths.len(), 1, "eight draws of one shape are one path");
	// And a DIFFERENT shape is a different entry, or deduplication would be losing drawings.
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 5.0, 4.0)), red(), FillRule::NonZero).expect("a fill");
	assert_eq!(canvas.finish().expect("balanced").resources().paths.len(), 2);
}

#[test]
// `restore` WITHOUT A `save` IS AN ERROR AND NOT A NO-OP. Treating it as one is how a component that
// restores one time too many silently inherits its parent's clip - and the drawing that results is
// wrong somewhere else, in a component that did nothing.
fn saved_state_is_a_stack_and_an_unbalanced_one_is_refused() {
	let mut canvas = Canvas::new();
	assert_eq!(canvas.restore().err(), Some(Error::UnbalancedSave));

	canvas.save().expect("a save");
	canvas.concat_transform(&Transform::translate(10.0, 0.0));
	canvas.set_clip(rect_path(RectF::new(0.0, 0.0, 4.0, 4.0)), FillRule::NonZero).expect("a clip");
	canvas.restore().expect("a restore");
	// THE CLIP PUSHED SINCE THE SAVE IS POPPED BY THE RESTORE, and the transform is back.
	assert_eq!(canvas.transform(), Transform::IDENTITY);
	let list = canvas.finish().expect("a balanced recording");
	assert!(matches!(list.commands().last(), Some(Command::PopClip)));
	list.validate().expect("the clips balance");

	// AND A RECORDING WITH A SAVE STILL OPEN IS REFUSED rather than producing a list that replays
	// with a clip nobody meant.
	let mut canvas = Canvas::new();
	canvas.save().expect("a save");
	assert_eq!(canvas.finish().err(), Some(Error::UnbalancedSave));
}

#[test]
// AN UNCLOSED LAYER WOULD SILENTLY LOSE EVERYTHING INSIDE IT: its contents were recorded into an
// offscreen nothing composites. That is a drawing that is simply missing, with nothing to say why.
fn an_unclosed_layer_is_refused_rather_than_losing_what_is_inside_it() {
	let mut canvas = Canvas::new();
	canvas.begin_layer(None, 0.5, BlendMode::Multiply, None).expect("a layer");
	canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 4.0, 4.0)), red(), FillRule::NonZero).expect("a fill");
	assert_eq!(canvas.finish().err(), Some(Error::UnbalancedLayer));
	canvas.end_layer().expect("closing it");
	let list = canvas.finish().expect("now balanced");
	assert_eq!(list.commands().len(), 3);
	// And an end without a begin is the other direction.
	assert_eq!(canvas.end_layer().err(), Some(Error::UnbalancedLayer));
}

#[test]
// THE VALIDATION BOUNDARY IS THE LIST, once, so a backend may then assume. A handle naming an entry
// the table does not have is a defect in whatever built the list, and it is named as such rather than
// discovered as an index panic inside a rasteriser.
fn a_handle_the_table_does_not_have_is_refused_by_name() {
	let mut builder = DrawListBuilder::new();
	let path = builder.add_path(rect_path(RectF::new(0.0, 0.0, 4.0, 4.0))).expect("a path");
	assert_eq!(path, PathHandle(0));
	builder.push(Command::FillPath { path: PathHandle(7), paint: red(), rule: FillRule::NonZero, transform: Transform::IDENTITY, antialias: Antialias::On, blend: BlendMode::Normal, operator: Operator::SrcOver, opacity: 1.0 }).expect("recording it is allowed; validating is where it fails");
	assert_eq!(builder.finish().err(), Some(Error::UnknownResource { kind: ResourceKind::Path, index: 7 }));
}

#[test]
// A PROFILE CEILING MET IS A DRAWING THAT IS LEGAL AND TOO LARGE. It has to be split or simplified,
// and the ceiling is NAMED so the caller knows which way - "invalid" would leave them reading their
// own drawing code.
fn a_profile_ceiling_is_refused_by_name_while_building() {
	let limits = graphics_profile::RENDER2D_PROFILE_1_MIN_LIMITS;
	let mut builder = PathBuilder::new();
	builder.move_to(PointF { x: 0.0, y: 0.0 }).expect("a start");
	let mut error = None;
	for index in 0..limits.max_path_verbs as usize + 2 {
		if let Err(found) = builder.line_to(PointF { x: index as f32, y: 0.0 }) {
			error = Some(found);
			break;
		}
	}
	assert_eq!(error, Some(Error::LimitExceeded { limit: "path verbs", ceiling: limits.max_path_verbs as u64 }));

	// AND A DRAWING CALL BEFORE A `move_to` IS THE CALLER'S MISTAKE AND NOT AN IMPLICIT ORIGIN:
	// starting at (0,0) for them draws a line from the corner of the surface, which is a visible
	// artefact whose cause is three functions away.
	let mut fresh = PathBuilder::new();
	assert_eq!(fresh.line_to(PointF { x: 1.0, y: 1.0 }).err(), Some(Error::PathNotStarted));
}

#[test]
// A FILTER NODE MAY ONLY READ NODES BEFORE IT, which makes the graph acyclic BY CONSTRUCTION rather
// than by a check a later edit can defeat. A cycle is a filter that never finishes.
fn a_filter_graph_is_acyclic_by_construction_and_its_bounds_run_backwards() {
	let mut graph = FilterGraph::default();
	let source = graph.push(FilterNode::Source).expect("a source");
	let blurred = graph.push(FilterNode::Blur { input: source, x: 4.0, y: 4.0 }).expect("a blur");
	let offset = graph.push(FilterNode::Offset { input: blurred, dx: 2.0, dy: 3.0 }).expect("an offset");
	assert_eq!((source, blurred, offset), (0, 1, 2));
	// A node reading itself, or reading one that comes later, is refused.
	assert_eq!(graph.push(FilterNode::Blur { input: 3, x: 1.0, y: 1.0 }).err(), Some(Error::FilterCycle));

	// THE BOUNDS MAP RUNS BACKWARDS FROM THE OUTPUT, which is the direction the question runs: a
	// caller knows what it wants drawn and needs to know what to read. A blur needs three standard
	// deviations more than it produces on each side, and an offset shifts what it needs.
	let needed = graph.required_input(RectF::new(0.0, 0.0, 10.0, 10.0));
	assert_eq!(needed.x, -14.0, "offset by two, then twelve for the blur's three sigma");
	assert_eq!(needed.y, -15.0);
	assert_eq!(needed.width, 34.0);

	// AND A RADIUS PAST THE PROFILE'S CEILING IS REFUSED, because the scratch a blur reserves is the
	// scratch its radius decides.
	let limits = graphics_profile::RENDER2D_PROFILE_1_MIN_LIMITS;
	let mut graph = FilterGraph::default();
	graph.push(FilterNode::Source).expect("a source");
	assert_eq!(graph.push(FilterNode::Blur { input: 0, x: limits.max_filter_radius as f32, y: 0.0 }).err(), Some(Error::LimitExceeded { limit: "filter radius", ceiling: limits.max_filter_radius as u64 }));
}

#[test]
// A PERSPECTIVE TRANSFORM IS WHAT A CARD FLIP, A PAGE TURN AND A MAP TILT ARE, and a point at or
// beyond the horizon has NO image - which is `None` rather than a very large number, because dividing
// by a `w` near zero produces a vertex at ten million pixels and a rasteriser that spends a second on
// one triangle.
fn a_transform_is_projective_and_the_horizon_has_no_image() {
	let identity = Transform::IDENTITY;
	assert!(identity.is_affine());
	assert_eq!(identity.map_point(PointF { x: 3.0, y: 4.0 }), Some(PointF { x: 3.0, y: 4.0 }));

	// Concatenation applies the new transform INSIDE the one already set.
	let moved = Transform::translate(10.0, 0.0).concat(&Transform::scale(2.0, 2.0));
	assert_eq!(moved.map_point(PointF { x: 1.0, y: 0.0 }), Some(PointF { x: 12.0, y: 0.0 }));

	// A projective transform whose `w` vanishes at a point.
	let projective = Transform { m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]] };
	assert!(!projective.is_affine());
	assert_eq!(projective.map_point(PointF { x: 2.0, y: 0.0 }), Some(PointF { x: 1.0, y: 0.0 }));
	assert_eq!(projective.map_point(PointF { x: 0.0, y: 5.0 }), None, "a point on the horizon has no image");
	assert_eq!(projective.map_point(PointF { x: -1.0, y: 0.0 }), None, "and neither has one beyond it");

	// A ROTATED RECTANGLE'S BOUNDS NEED ALL FOUR CORNERS: taking two gives a bound smaller than the
	// drawing, which is how a damage rectangle comes to clip the thing it was computed for.
	let rotated = Transform { m: [[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]] };
	let bounds = rotated.map_rect(RectF::new(0.0, 0.0, 4.0, 2.0)).expect("all four corners have images");
	assert_eq!((bounds.x, bounds.y, bounds.width, bounds.height), (-2.0, 0.0, 2.0, 4.0));

	// COMPOSITION STAYS PROJECTIVE, and the composed transform is the one that maps in one step what
	// the two map in two - which is the property a nested component's transform depends on.
	let perspective = Transform { m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, -0.001, 1.0]] };
	let composed = perspective.concat(&Transform::scale(2.0, 2.0));
	assert!(!composed.is_affine());
	let point = PointF { x: 3.0, y: 5.0 };
	let two_steps = perspective.map_point(Transform::scale(2.0, 2.0).map_point(point).expect("an image")).expect("an image");
	let one_step = composed.map_point(point).expect("an image");
	assert!((one_step.x - two_steps.x).abs() < 1e-3 && (one_step.y - two_steps.y).abs() < 1e-3, "{one_step:?} against {two_steps:?}");

	// AND THE INVERSE TAKES A DEVICE POINT BACK, which is what a hit test on a nested component needs.
	let back = composed.inverse().expect("a transform that collapses nothing is invertible");
	let returned = back.map_point(one_step).expect("an image");
	assert!((returned.x - point.x).abs() < 1e-2 && (returned.y - point.y).abs() < 1e-2, "{returned:?} against {point:?}");
	// A transform that collapses the plane onto a line has no inverse, and says so rather than
	// returning one of the many points that map to each image.
	assert_eq!(Transform { m: [[1.0, 1.0, 0.0], [2.0, 2.0, 0.0], [0.0, 0.0, 1.0]] }.inverse(), None);
}

#[test]
// A PATH THAT ONLY SUPPORTS CONSTRUCTION IS HALF A PATH: an application that cannot ask whether a
// path contains a point implements its own geometry, which is the private-rasteriser failure one
// level up. And the two fill rules must actually differ, or one of them is not implemented.
fn a_path_answers_a_hit_test_under_both_fill_rules() {
	// A square with a square hole, wound the SAME way: non-zero fills the hole and even-odd does not.
	let mut builder = PathBuilder::new();
	builder.add_rect(RectF::new(0.0, 0.0, 10.0, 10.0)).expect("the outside");
	builder.add_rect(RectF::new(3.0, 3.0, 4.0, 4.0)).expect("the inside");
	let path = builder.finish();
	assert_eq!(path.subpaths(), 2);

	let middle = PointF { x: 5.0, y: 5.0 };
	assert!(path.contains(middle, FillRule::NonZero), "two same-wound contours make the middle doubly wound, which is inside");
	assert!(!path.contains(middle, FillRule::EvenOdd), "two crossings is even, which is outside");
	// Outside both, and inside both.
	assert!(!path.contains(PointF { x: 20.0, y: 5.0 }, FillRule::NonZero));
	assert!(path.contains(PointF { x: 1.0, y: 5.0 }, FillRule::EvenOdd));

	// A CONTROL POINT IS OFTEN WELL OUTSIDE THE CURVE, so the loose bound is bigger than the tight
	// one - and a layer sized by the loose one allocates an offscreen bigger than it needs, every
	// frame.
	let mut curved = PathBuilder::new();
	curved.move_to(PointF { x: 0.0, y: 0.0 }).expect("a start");
	curved.quad_to(PointF { x: 5.0, y: 100.0 }, PointF { x: 10.0, y: 0.0 }).expect("a curve");
	let curved = curved.finish();
	let loose = curved.bounds().expect("points");
	let tight = curved.tight_bounds().expect("extrema");
	assert_eq!(loose.height, 100.0, "the control point is a hundred up");
	assert!(tight.height < 51.0 && tight.height > 49.0, "the curve only reaches half way: {}", tight.height);
}

#[test]
// A CACHE KEY IS A HASH OF BYTES, so the list has a byte form whether or not anything sends it
// anywhere - and a list whose encoding LOSES a field silently produces a cache HIT on a drawing that
// is not the one recorded.
fn the_canonical_encoding_distinguishes_drawings_that_differ() {
	let draw = |opacity: f32, rule: FillRule| {
		let mut canvas = Canvas::new();
		canvas.set_opacity(opacity);
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 4.0, 4.0)), red(), rule).expect("a fill");
		canvas.finish().expect("balanced")
	};
	let base = draw(1.0, FillRule::NonZero);
	assert_eq!(encode::digest(&base), encode::digest(&draw(1.0, FillRule::NonZero)), "the same drawing must key the same");
	// EVERY FIELD THE ENCODING CARRIES MUST CHANGE IT, or that field is the one a cache hit loses.
	assert_ne!(encode::digest(&base), encode::digest(&draw(0.5, FillRule::NonZero)), "opacity");
	assert_ne!(encode::digest(&base), encode::digest(&draw(1.0, FillRule::EvenOdd)), "fill rule");
	// AND THE VERSION IS THE FIRST FIELD, so two lists recorded under different schemas cannot collide
	// however similar their commands are.
	assert_eq!(&encode::encode(&base)[..4], &DRAW_LIST_VERSION.to_le_bytes());
}

#[test]
// A PREPARED LIST IS A CACHE, and `is_compatible` is a comparison rather than a judgement. Each
// dependency is changed ALONE here, which is the test the enumeration exists to make possible.
fn every_prepared_dependency_invalidates_alone_and_is_named() {
	use graphics_core::{Extent2D, PixelFormat};
	let mut canvas = Canvas::new();
	let image = ImageRecord { identity: 7, layout_generation: 1, content_generation: 1 };
	canvas.draw_image(image, RectF::new(0.0, 0.0, 4.0, 4.0), RectF::new(0.0, 0.0, 4.0, 4.0), ImageQuality::Bilinear).expect("an image");
	let list = canvas.finish().expect("balanced");
	let target = TargetDescription { extent: Extent2D::new(64, 64), format: PixelFormat::B8G8R8A8Unorm, color_space: ColorSpace::SrgbLinear, scale: 1.0, luminance: OutputLuminance::UNKNOWN };
	let key = PreparedKey::of(&list, &target, ("soft2d", 1), 5);
	assert!(key.compatible_with(&key).is_ok());

	// Each one alone.
	let mut other = TargetDescription { format: PixelFormat::R8G8B8A8Unorm, ..target };
	assert_eq!(PreparedKey::of(&list, &other, ("soft2d", 1), 5).compatible_with(&key).err(), Some(RePrepare::Format));
	other = TargetDescription { extent: Extent2D::new(65, 64), ..target };
	assert_eq!(PreparedKey::of(&list, &other, ("soft2d", 1), 5).compatible_with(&key).err(), Some(RePrepare::Extent));
	other = TargetDescription { scale: 2.0, ..target };
	assert_eq!(PreparedKey::of(&list, &other, ("soft2d", 1), 5).compatible_with(&key).err(), Some(RePrepare::Scale));
	assert_eq!(PreparedKey::of(&list, &target, ("gpu2d", 1), 5).compatible_with(&key).err(), Some(RePrepare::Backend));
	assert_eq!(PreparedKey::of(&list, &target, ("soft2d", 2), 5).compatible_with(&key).err(), Some(RePrepare::Backend));
	assert_eq!(PreparedKey::of(&list, &target, ("soft2d", 1), 6).compatible_with(&key).err(), Some(RePrepare::GlyphCache));

	// A REPLACED IMAGE IS STRUCTURAL.
	let mut canvas = Canvas::new();
	let replaced = ImageRecord { identity: 7, layout_generation: 2, content_generation: 1 };
	canvas.draw_image(replaced, RectF::new(0.0, 0.0, 4.0, 4.0), RectF::new(0.0, 0.0, 4.0, 4.0), ImageQuality::Bilinear).expect("an image");
	let structural = canvas.finish().expect("balanced");
	assert_eq!(PreparedKey::of(&structural, &target, ("soft2d", 1), 5).compatible_with(&key).err(), Some(RePrepare::ImageLayout));

	// AND A NEW VIDEO FRAME IS NOT. The prepared list stays valid, and what has to happen is that the
	// named image's upload cache is refreshed - re-flattening every path for it would be re-preparing
	// sixty times a second for no reason.
	let mut canvas = Canvas::new();
	let refreshed = ImageRecord { identity: 7, layout_generation: 1, content_generation: 2 };
	canvas.draw_image(refreshed, RectF::new(0.0, 0.0, 4.0, 4.0), RectF::new(0.0, 0.0, 4.0, 4.0), ImageQuality::Bilinear).expect("an image");
	let content = canvas.finish().expect("balanced");
	assert!(PreparedKey::of(&content, &target, ("soft2d", 1), 5).compatible_with(&key).is_ok(), "a content change must NOT invalidate the preparation");
	assert_eq!(prepared::content_refreshed(&list, &content), std::vec![7], "and it must be reported as a content refresh");
	// The drawings still differ, so they must not share a cached raster.
	assert_ne!(encode::digest(&list), encode::digest(&content));
}

#[test]
// THE ENUMERATIONS ARE THE PROFILE'S AND A FIXTURE HOLDS THEM TO IT. An enumeration that drifted from
// the published list would be an API offering a mode the specification does not define, or missing one
// it requires - either discovered by a conformance run rather than by a compiler.
fn the_operator_and_blend_enumerations_match_the_frozen_lists() {
	use graphics_profile::compositing::{BLENDS, NON_SEPARABLE_BLENDS, OPERATORS};
	assert_eq!(blend::ALL_OPERATORS.len(), OPERATORS.len());
	for (operator, published) in blend::ALL_OPERATORS.iter().zip(OPERATORS.iter()) {
		assert_eq!(operator.name(), published.name, "the operator enumeration is out of order with the registry");
		assert_eq!(operator.factors(), (published.source_factor, published.backdrop_factor));
	}
	assert_eq!(blend::ALL_BLEND_MODES.len(), BLENDS.len() + NON_SEPARABLE_BLENDS.len());
	for (mode, published) in blend::ALL_BLEND_MODES.iter().zip(BLENDS.iter().map(|blend| blend.name).chain(NON_SEPARABLE_BLENDS.iter().map(|blend| blend.name))) {
		assert_eq!(mode.name(), published);
	}
	// And the four that mix channels are the four that say they do.
	let non_separable: std::vec::Vec<&str> = blend::ALL_BLEND_MODES.iter().filter(|mode| !mode.is_separable()).map(|mode| mode.name()).collect();
	assert_eq!(non_separable, std::vec!["Hue", "Saturation", "Color", "Luminosity"]);
}

#[test]
// IF A BOUNDS QUERY FLATTENS DIFFERENTLY FROM THE RASTERISER, AN APPLICATION ASKS WHERE SOMETHING IS
// AND DRAWS IT SOMEWHERE ELSE. One flattening, at the profile's tolerance, in DEVICE space - so a
// curve under a two-times transform is subdivided twice as finely.
fn one_flattening_serves_every_query_and_is_done_in_device_space() {
	let mut builder = PathBuilder::new();
	builder.move_to(PointF { x: 0.0, y: 0.0 }).expect("a start");
	builder.quad_to(PointF { x: 50.0, y: 100.0 }, PointF { x: 100.0, y: 0.0 }).expect("a curve");
	let path = builder.finish();

	let plain = flatten::flatten(&path, None);
	let scaled = flatten::flatten(&path, Some(&Transform::scale(4.0, 4.0)));
	assert_eq!(plain.len(), 1);
	assert!(scaled[0].points.len() > plain[0].points.len(), "a curve drawn larger must be flattened more finely: {} against {}", scaled[0].points.len(), plain[0].points.len());

	// THE SAME SEGMENTS ANSWER THE LENGTH, so a dash pattern laid out along it ends where the drawing
	// does. A straight line's length is exact, which is the case that catches a scale applied twice.
	let mut line = PathBuilder::new();
	line.move_to(PointF { x: 0.0, y: 0.0 }).expect("a start");
	line.line_to(PointF { x: 3.0, y: 4.0 }).expect("a line");
	let line = line.finish();
	assert!((query::length(&line, None) - 5.0).abs() < 1e-3);
	assert!((query::length(&line, Some(&Transform::scale(2.0, 2.0))) - 10.0).abs() < 1e-3);
}

#[test]
// EVERY CALLER THAT WANTS THE POINT WANTS THE DIRECTION: placing a label along a curve needs where AND
// which way, and two passes can land on two different segments at a boundary.
fn a_path_answers_where_and_which_way_at_a_distance() {
	let mut builder = PathBuilder::new();
	builder.move_to(PointF { x: 0.0, y: 0.0 }).expect("a start");
	builder.line_to(PointF { x: 10.0, y: 0.0 }).expect("a line");
	builder.line_to(PointF { x: 10.0, y: 10.0 }).expect("a line");
	let path = builder.finish();

	let (point, tangent) = query::at_distance(&path, 5.0, None).expect("half way along the first segment");
	assert!((point.x - 5.0).abs() < 1e-3 && point.y.abs() < 1e-3);
	assert!((tangent.x - 1.0).abs() < 1e-3 && tangent.y.abs() < 1e-3);

	let (point, tangent) = query::at_distance(&path, 15.0, None).expect("half way along the second");
	assert!((point.x - 10.0).abs() < 1e-3 && (point.y - 5.0).abs() < 1e-3);
	assert!(tangent.x.abs() < 1e-3 && (tangent.y - 1.0).abs() < 1e-3);

	// PAST THE END IS THE END, with the last direction. Answering `None` would make a dash or a label
	// that ran one rounding unit past the path disappear instead of finishing at its tip.
	let (point, _) = query::at_distance(&path, 1000.0, None).expect("past the end is the end");
	assert!((point.x - 10.0).abs() < 1e-3);
}

#[test]
// A LINE HAS NO INTERIOR, so a hit test that only asked about the fill would say nothing in a diagram
// is clickable. And the width is interpreted the way the STYLE says: using the wrong rule makes a
// zoomed-in diagram's lines unclickable at exactly the zoom where they are easiest to see.
fn a_stroke_is_hit_tested_and_bounded_by_its_own_rules() {
	let mut builder = PathBuilder::new();
	builder.move_to(PointF { x: 0.0, y: 0.0 }).expect("a start");
	builder.line_to(PointF { x: 100.0, y: 0.0 }).expect("a line");
	let path = builder.finish();
	let style = StrokeStyle { width: 4.0, miter_limit: 1.0, ..StrokeStyle::default() };

	assert!(query::stroke_contains(&path, &style, PointF { x: 50.0, y: 1.5 }, None), "inside the pen");
	assert!(!query::stroke_contains(&path, &style, PointF { x: 50.0, y: 3.0 }, None), "outside it");
	// A BUTT CAP DOES NOT EXTEND THE LINE AND A SQUARE ONE DOES, which differ by exactly the
	// half-width at both ends of every open path in a drawing.
	assert!(!query::stroke_contains(&path, &style, PointF { x: 101.0, y: 0.0 }, None));
	let square = StrokeStyle { cap: Cap::Square, ..style };
	assert!(query::stroke_contains(&path, &square, PointF { x: 101.0, y: 0.0 }, None));

	// UNDER A TRANSFORM THE TWO SCALING RULES DIFFER, and that is the whole reason both exist.
	let zoom = Transform::scale(4.0, 4.0);
	assert!(query::stroke_contains(&path, &style, PointF { x: 200.0, y: 6.0 }, Some(&zoom)), "a scaled pen is four times wider");
	let hairline = StrokeStyle { scaling: StrokeScaling::NonScaling, ..style };
	assert!(!query::stroke_contains(&path, &hairline, PointF { x: 200.0, y: 6.0 }, Some(&zoom)), "a hairline stays one pen wide at any zoom");

	// AND THE MITER IS THE PART THAT IS FORGOTTEN: a nearly parallel join reaches out by the
	// half-width times the limit, so a layer sized without it clips the sharp corners somebody drew
	// on purpose.
	let plain = query::stroke_bounds(&path, &style, None).expect("bounds");
	let mitred = query::stroke_bounds(&path, &StrokeStyle { miter_limit: 4.0, ..style }, None).expect("bounds");
	assert!(mitred.width > plain.width, "a larger miter limit reaches further: {} against {}", mitred.width, plain.width);
}

#[test]
// TWO BACKENDS PRODUCE DIFFERENT UNIONS IF ANY OF THE EIGHT QUESTIONS IS LEFT OPEN. These are the
// answers, checked against results that can be stated by hand.
fn the_boolean_operations_produce_the_stated_results() {
	let square = |x: f32, y: f32, size: f32| {
		let mut builder = PathBuilder::new();
		builder.add_rect(RectF::new(x, y, size, size)).expect("a square");
		builder.finish()
	};
	let left = square(0.0, 0.0, 10.0);
	let right = square(5.0, 0.0, 10.0);

	// A UNION OF TWO OVERLAPPING SQUARES IS ONE CONTOUR, and its bounds are both of them.
	let union = boolean::combine(&left, FillRule::NonZero, &right, FillRule::NonZero, boolean::Operation::Union).expect("a union");
	assert_eq!(union.subpaths(), 1, "two overlapping squares make one shape");
	let bounds = union.bounds().expect("points");
	assert!((bounds.x - 0.0).abs() < 0.01 && (bounds.width - 15.0).abs() < 0.01, "{bounds:?}");
	assert!(query::fill_contains(&union, FillRule::NonZero, PointF { x: 12.0, y: 5.0 }, None), "the right square's own half is in the union");

	// AN INTERSECTION IS THE OVERLAP ALONE.
	let overlap = boolean::combine(&left, FillRule::NonZero, &right, FillRule::NonZero, boolean::Operation::Intersection).expect("an intersection");
	let bounds = overlap.bounds().expect("points");
	assert!((bounds.x - 5.0).abs() < 0.01 && (bounds.width - 5.0).abs() < 0.01, "{bounds:?}");
	assert!(!query::fill_contains(&overlap, FillRule::NonZero, PointF { x: 2.0, y: 5.0 }, None), "what only the left square covered is not in the intersection");

	// A DIFFERENCE KEEPS WHAT ONLY THE FIRST COVERED.
	let cut = boolean::combine(&left, FillRule::NonZero, &right, FillRule::NonZero, boolean::Operation::Difference).expect("a difference");
	assert!(query::fill_contains(&cut, FillRule::NonZero, PointF { x: 2.0, y: 5.0 }, None));
	assert!(!query::fill_contains(&cut, FillRule::NonZero, PointF { x: 7.0, y: 5.0 }, None), "the overlap is removed");

	// AN INTERSECTION OF SHAPES THAT DO NOT TOUCH IS EMPTY, and that is a correct answer rather than
	// an error.
	let apart = boolean::combine(&left, FillRule::NonZero, &square(100.0, 0.0, 5.0), FillRule::NonZero, boolean::Operation::Intersection).expect("an answer");
	assert!(apart.is_empty(), "an empty result is a path with nothing in it");

	// AND THE RESULT IS IN THE CANONICAL WINDING: an outer contour's signed area is POSITIVE in the
	// device's y-down space, which is what makes two implementations produce the same BYTES and not
	// only the same shape.
	for contour in flatten::flatten(&union, None) {
		assert!(contour.double_area() > 0.0, "an outer contour must be wound positive");
	}
}

#[test]
// A PATH DRAWN WITH A NON-ZERO RULE AND ONE WITH AN EVEN-ODD RULE ARE DIFFERENT SHAPES. Simplifying
// makes the shape explicit, so that everything after it works on regions rather than on a rule - which
// is what lets two operands with different rules combine at all.
fn simplify_resolves_a_rule_into_a_shape() {
	let mut builder = PathBuilder::new();
	builder.add_rect(RectF::new(0.0, 0.0, 10.0, 10.0)).expect("the outside");
	builder.add_rect(RectF::new(3.0, 3.0, 4.0, 4.0)).expect("the inside, wound the same way");
	let path = builder.finish();

	let non_zero = boolean::simplify(&path, FillRule::NonZero).expect("a shape");
	let even_odd = boolean::simplify(&path, FillRule::EvenOdd).expect("a shape");
	// Under non-zero the middle is filled; under even-odd it is a hole. The simplified shapes must
	// therefore differ, and each must answer the same way its rule did.
	assert!(query::fill_contains(&non_zero, FillRule::NonZero, PointF { x: 5.0, y: 5.0 }, None));
	assert!(!query::fill_contains(&even_odd, FillRule::NonZero, PointF { x: 5.0, y: 5.0 }, None), "the hole survives simplification");
	assert!(query::fill_contains(&even_odd, FillRule::NonZero, PointF { x: 1.0, y: 5.0 }, None), "and the ring around it does");
}

#[test]
// A COMPOSITOR THAT REDRAWS EVERYTHING EVERY FRAME BURNS A BATTERY ON A BLINKING CURSOR. Damage is
// computed from the recorded list, and it is a BOUNDED list with an explicit whole-surface fallback -
// a list that silently merged everything when it got long would look like damage tracking while doing
// none.
fn damage_is_a_bounded_list_with_an_explicit_fallback() {
	use graphics_core::Extent2D;
	let extent = Extent2D::new(200, 200);
	let mut canvas = Canvas::new();
	canvas.fill_path(rect_path(RectF::new(10.25, 10.25, 20.0, 20.0)), red(), FillRule::NonZero).expect("a fill");
	let list = canvas.finish().expect("balanced");
	match damage::of(&list, extent) {
		damage::Damage::Regions(regions) => {
			assert_eq!(regions.len(), 1);
			// ROUNDED OUTWARDS ON EVERY SIDE: a rectangle rounded inwards leaves the antialiased edge
			// of the thing that moved on the screen, which is a one-pixel ghost.
			assert_eq!(regions[0], graphics_core::PixelRect::new(10, 10, 21, 21));
		}
		other => panic!("expected regions, got {other:?}"),
	}

	// A LAYER WITH NO STATED BOUNDS COULD HAVE TOUCHED ANYTHING, and guessing smaller is how a stale
	// region is left on the screen.
	let mut canvas = Canvas::new();
	canvas.begin_layer(None, 1.0, BlendMode::Normal, None).expect("a layer");
	canvas.end_layer().expect("closing it");
	assert_eq!(damage::of(&canvas.finish().expect("balanced"), extent), damage::Damage::Whole);

	// AND PAST THE BOUND THE WHOLE SURFACE IS CHEAPER, which is a value a consumer can see rather
	// than a merge it cannot.
	let mut canvas = Canvas::new();
	for index in 0..damage::MAX_REGIONS + 2 {
		canvas.fill_path(rect_path(RectF::new(index as f32 * 2.0, 0.0, 1.0, 1.0)), red(), FillRule::NonZero).expect("a fill");
	}
	assert_eq!(damage::of(&canvas.finish().expect("balanced"), extent), damage::Damage::Whole);
}

#[test]
// HOSTILE TRANSFORMS, DEGENERATE CURVES AND DEEP NESTING, under the profile's own limits. What is
// asserted is that every query ANSWERS: no panic, no unbounded allocation, no walk that does not come
// back. Whether a degenerate path has a meaningful length is not the question.
fn hostile_geometry_is_answered_rather_than_crashed_on() {
	let hostile = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, -0.0, f32::MAX, f32::MIN_POSITIVE, -1e30];
	for value in hostile {
		let mut builder = PathBuilder::new();
		if builder.move_to(PointF { x: value, y: value }).is_err() {
			continue;
		}
		let _ = builder.line_to(PointF { x: -value, y: value });
		let _ = builder.quad_to(PointF { x: value, y: -value }, PointF { x: 0.0, y: 0.0 });
		let _ = builder.cubic_to(PointF { x: value, y: 0.0 }, PointF { x: 0.0, y: value }, PointF { x: 1.0, y: 1.0 });
		let path = builder.finish();

		let transforms = [
			Transform::IDENTITY,
			Transform::scale(value, value),
			Transform { m: [[value, 0.0, 0.0], [0.0, value, 0.0], [value, value, 0.0]] },
			Transform { m: [[0.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]] },
		];
		for transform in transforms {
			let _ = flatten::flatten(&path, Some(&transform));
			let _ = query::length(&path, Some(&transform));
			let _ = query::at_distance(&path, value, Some(&transform));
			let _ = query::fill_contains(&path, FillRule::NonZero, PointF { x: value, y: 0.0 }, Some(&transform));
			let _ = query::stroke_contains(&path, &StrokeStyle::default(), PointF { x: 0.0, y: value }, Some(&transform));
			let _ = query::stroke_bounds(&path, &StrokeStyle { width: value, ..StrokeStyle::default() }, Some(&transform));
			let _ = transform.map_rect(RectF::new(value, value, value, value));
			let _ = transform.approximate_scale();
		}
		let _ = path.tight_bounds();
	}

	// THE BOOLEAN OPERATIONS ARE QUADRATIC IN THE EDGE COUNT, so they are fed hostile SHAPES rather
	// than hostile shapes under four hostile transforms each: the property being checked is that a
	// degenerate operand answers, and a thousand of them check it no better than a few.
	let degenerate = |a: f32, b: f32| {
		let mut builder = PathBuilder::new();
		let _ = builder.move_to(PointF { x: a, y: b });
		let _ = builder.line_to(PointF { x: b, y: a });
		let _ = builder.line_to(PointF { x: a, y: b });
		let _ = builder.close();
		builder.finish()
	};
	for (a, b) in [(0.0, 0.0), (f32::NAN, 1.0), (f32::INFINITY, 0.0), (1e30, -1e30)] {
		let path = degenerate(a, b);
		let _ = boolean::simplify(&path, FillRule::NonZero);
		let _ = boolean::combine(&path, FillRule::EvenOdd, &path, FillRule::NonZero, boolean::Operation::Union);
		let _ = boolean::combine(&path, FillRule::NonZero, &rect_path(RectF::new(0.0, 0.0, 4.0, 4.0)), FillRule::NonZero, boolean::Operation::Difference);
	}

	// DEEP NESTING, to the profile's own ceiling and one past it - and the refusal is by name.
	let limits = graphics_profile::RENDER2D_PROFILE_1_MIN_LIMITS;
	let mut canvas = Canvas::new();
	for _ in 0..limits.max_layer_depth {
		canvas.begin_layer(None, 1.0, BlendMode::Normal, None).expect("within the ceiling");
	}
	assert_eq!(canvas.begin_layer(None, 1.0, BlendMode::Normal, None).err(), Some(Error::LimitExceeded { limit: "layer depth", ceiling: limits.max_layer_depth as u64 }));
	for _ in 0..limits.max_layer_depth {
		canvas.end_layer().expect("closing them");
	}
	canvas.finish().expect("balanced again");
}

#[test]
// A CONTROL POINT AT `w == 0` IS ON THE PROJECTIVE HORIZON, and the profile decided what happens to it
// rather than leaving each implementation to guess: the segment is CLIPPED against the `w = epsilon`
// plane in homogeneous space BEFORE the divide, a segment with no part on the near side is dropped,
// and the primitive is refused only when NOTHING survives.
fn the_horizon_clips_a_segment_and_refuses_only_a_geometry_with_no_image() {
	// `w = 1 - y/100`, so the horizon is the line `y == 100` and everything past it is behind the eye.
	let projective = Transform { m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, -0.01, 1.0]] };
	assert!(!projective.is_affine());

	// A CROSSING SQUARE KEEPS THE HALF THAT HAS AN IMAGE. It comes back OPEN, because a fill that
	// closed it would close it across the gap the horizon made - a shape nobody drew.
	let crossing = rect_path(RectF::new(0.0, 50.0, 100.0, 100.0));
	let contours = flatten::flatten(&crossing, Some(&projective));
	assert!(!contours.is_empty(), "a square straddling the horizon has an image and must keep it");
	for contour in &contours {
		assert!(!contour.closed, "a contour the horizon cut is no longer the closed loop that was drawn");
		for point in &contour.points {
			assert!(point.x.is_finite() && point.y.is_finite(), "clipping happens BEFORE the divide, so no vertex is at infinity: {point:?}");
		}
	}
	flatten::flatten_checked(&crossing, Some(&projective)).expect("part of it survives, so it is not refused");

	// ENTIRELY BEYOND THE HORIZON IS A REFUSAL AND NOT AN EMPTY DRAWING. Returning no contours would
	// have the caller believe it drew something that was simply invisible.
	let beyond = rect_path(RectF::new(0.0, 200.0, 100.0, 100.0));
	assert!(flatten::flatten(&beyond, Some(&projective)).is_empty());
	assert_eq!(flatten::flatten_checked(&beyond, Some(&projective)).err(), Some(Error::BeyondHorizon));

	// AND THE REFUSAL ARRIVES AT THE CALL THAT MADE THE MISTAKE. Validation is eager, so an
	// application learns at `fill_path` rather than at replay, three frames away from the cause.
	let mut canvas = Canvas::new();
	canvas.set_transform(projective);
	assert_eq!(canvas.fill_path(beyond.clone(), Paint::Solid(Color::BLACK), FillRule::NonZero).err(), Some(Error::BeyondHorizon));
	assert_eq!(canvas.stroke_path(beyond.clone(), Paint::Solid(Color::BLACK), StrokeStyle::default()).err(), Some(Error::BeyondHorizon));
	assert_eq!(canvas.set_clip(beyond.clone(), FillRule::NonZero).err(), Some(Error::BeyondHorizon));
	canvas.fill_path(crossing, Paint::Solid(Color::BLACK), FillRule::NonZero).expect("the part with an image draws");

	// AN AFFINE TRANSFORM HAS NO HORIZON, so the same path is an ordinary drawing and pays nothing for
	// the check: every point of an affine map has an image.
	let mut affine = Canvas::new();
	affine.set_transform(Transform::translate(10.0, 10.0));
	affine.fill_path(beyond, Paint::Solid(Color::BLACK), FillRule::NonZero).expect("no horizon, no refusal");
}

#[test]
// THE FOUR ANSWERS THE OVERLAPPING-SQUARES FIXTURE DOES NOT REACH: an OPEN subpath as an operand,
// XOR, the canonical ORDER of the output contours, and DETERMINISM - which is the one a caching
// compositor depends on, because a union recomputed from the same bytes must be the same bytes.
fn the_remaining_boolean_answers_are_pinned() {
	// AN OPEN SUBPATH IS CLOSED WITH A STRAIGHT SEGMENT. A boolean operation is defined on REGIONS,
	// and refusing an open operand would fail the commonest use - the union of two stroke outlines.
	let mut open = PathBuilder::new();
	open.move_to(PointF { x: 0.0, y: 0.0 }).expect("a start");
	open.line_to(PointF { x: 10.0, y: 0.0 }).expect("across");
	open.line_to(PointF { x: 10.0, y: 10.0 }).expect("down");
	open.line_to(PointF { x: 0.0, y: 10.0 }).expect("back");
	let open = open.finish();
	let closed = boolean::simplify(&open, FillRule::NonZero).expect("an open operand is a region");
	assert!(query::fill_contains(&closed, FillRule::NonZero, PointF { x: 5.0, y: 5.0 }, None), "the missing side is supplied by a straight segment, so the square is a region");

	// XOR KEEPS WHAT EXACTLY ONE OPERAND COVERS.
	let square = |x: f32| {
		let mut builder = PathBuilder::new();
		builder.add_rect(RectF::new(x, 0.0, 10.0, 10.0)).expect("a square");
		builder.finish()
	};
	let (left, right) = (square(0.0), square(5.0));
	let either = boolean::combine(&left, FillRule::NonZero, &right, FillRule::NonZero, boolean::Operation::Xor).expect("an xor");
	assert!(query::fill_contains(&either, FillRule::NonZero, PointF { x: 2.0, y: 5.0 }, None), "only the left covers this");
	assert!(query::fill_contains(&either, FillRule::NonZero, PointF { x: 12.0, y: 5.0 }, None), "only the right covers this");
	assert!(!query::fill_contains(&either, FillRule::NonZero, PointF { x: 7.0, y: 5.0 }, None), "both cover this, so it is out");

	// THE CANONICAL ORDER: minimum y, then minimum x, then the first point. Two squares that do not
	// touch come out lowest-y first however they were given.
	let lower = |y: f32| {
		let mut builder = PathBuilder::new();
		builder.add_rect(RectF::new(0.0, y, 4.0, 4.0)).expect("a square");
		builder.finish()
	};
	let mut reversed = PathBuilder::new();
	reversed.add_rect(RectF::new(0.0, 40.0, 4.0, 4.0)).expect("the far one first");
	reversed.add_rect(RectF::new(0.0, 0.0, 4.0, 4.0)).expect("and the near one second");
	let ordered = boolean::simplify(&reversed.finish(), FillRule::NonZero).expect("two regions");
	let contours = flatten::flatten(&ordered, None);
	assert_eq!(contours.len(), 2);
	let first = contours[0].points.iter().fold(f32::INFINITY, |lowest, point| lowest.min(point.y));
	let second = contours[1].points.iter().fold(f32::INFINITY, |lowest, point| lowest.min(point.y));
	assert!(first < second, "the output is ordered by minimum y whatever order the input was in: {first} then {second}");

	// DETERMINISM: the output is a function of the input and the tolerance alone. The same operation
	// twice, and the same operation on a separately built but identical operand, are the same PATH.
	let again = boolean::combine(&square(0.0), FillRule::NonZero, &square(5.0), FillRule::NonZero, boolean::Operation::Xor).expect("an xor");
	assert_eq!(either.verbs(), again.verbs());
	assert_eq!(either.points(), again.points(), "the same input bytes produce the same output bytes");

	// AND A DEGENERATE OPERAND CONTRIBUTES NOTHING RATHER THAN REFUSING. A contour left with fewer
	// than three distinct points is dropped, and a result with no contours is an EMPTY path.
	let mut degenerate = PathBuilder::new();
	degenerate.move_to(PointF { x: 1.0, y: 1.0 }).expect("a start");
	degenerate.line_to(PointF { x: 1.0, y: 1.0 }).expect("a zero-length edge");
	degenerate.close().expect("closed");
	let nothing = boolean::simplify(&degenerate.finish(), FillRule::NonZero).expect("an answer, not a refusal");
	assert!(nothing.is_empty(), "a degenerate contour contributes nothing");
	assert!(lower(0.0).bounds().is_some());
}

// THE SHAPES, AND WHAT IS ACTUALLY CHECKED ABOUT THEM.
//
// NOT THE POINTS THEY PRODUCE. A fixture holding the twelve control points of a rounded rectangle
// passes for the shape it was written from and says nothing about what that shape IS - and it fails
// on every legitimate refactor. What is held here is the CONVENTIONS the profile freezes: where a
// shape starts, which way it goes, that it is on the curve it claims, and what it refuses.

/// The vertices a path flattens to, in order, as one list per subpath boundary is not needed here:
/// every shape this module builds is a single subpath.
fn flattened(path: &Path) -> Vec<PointF> {
	let mut points = Vec::new();
	path.for_each_line(|from, to| {
		if points.is_empty() {
			points.push(from);
		}
		points.push(to);
	});
	points
}

/// Twice the signed area, POSITIVE for a shape wound clockwise in the device's y-down space.
fn signed_area(points: &[PointF]) -> f32 {
	let mut sum = 0.0;
	for window in points.windows(2) {
		sum += window[0].x * window[1].y - window[1].x * window[0].y;
	}
	sum
}

#[test]
// A CIRCLE IS ON THE CIRCLE, which is the property four cubics have to be measured against: the
// frozen control ratio is a CHOICE, and the thing the choice has to deliver is a curve whose
// greatest radial error is a fraction of a pixel rather than a shape that merely looks round.
fn a_circle_is_on_its_circle_and_is_wound_clockwise_on_screen() {
	let mut builder = PathBuilder::new();
	builder.add_circle(PointF { x: 100.0, y: 60.0 }, 40.0).expect("a circle");
	let path = builder.finish();

	// IT STARTS AT THE +X EXTREME, which is what a dash pattern and a stroke's first cap are placed
	// from - so it is a convention with a visible consequence rather than an internal detail.
	assert_eq!(path.points()[0], PointF { x: 140.0, y: 60.0 });

	let points = flattened(&path);
	let mut worst: f64 = 0.0;
	for point in &points {
		let (dx, dy) = (point.x as f64 - 100.0, point.y as f64 - 60.0);
		worst = worst.max(((dx * dx + dy * dy).sqrt() - 40.0).abs());
	}
	// A QUARTER CIRCLE HAS NO EXACT CUBIC FORM, and this is how close the frozen ratio comes on a
	// forty-pixel radius: the curve passes through the quadrant midpoints exactly and bulges by
	// about 2.7 parts in ten thousand of the radius between them.
	assert!(worst < 0.02, "the greatest radial error is {worst}, which is not a circle");

	// CLOCKWISE ON SCREEN, which is `add_rect`'s direction - so a circle inside a rectangle is a
	// solid under the non-zero rule and a ring only if one of them is reversed.
	assert!(signed_area(&points) > 0.0, "a circle is wound in the direction of increasing angle");
	let mut rectangle = PathBuilder::new();
	rectangle.add_rect(RectF::new(0.0, 0.0, 10.0, 10.0)).expect("a rectangle");
	assert!(signed_area(&flattened(&rectangle.finish())) > 0.0, "and a rectangle is wound the same way");
}

#[test]
// AN ELLIPSE IS NOT A SCALED CIRCLE HERE, it is built with both radii - so the check is that both
// extremes are where they belong and that the curve is on the ellipse between them.
fn an_ellipse_reaches_both_of_its_extremes() {
	let mut builder = PathBuilder::new();
	builder.add_ellipse(PointF { x: 0.0, y: 0.0 }, 30.0, 10.0).expect("an ellipse");
	let path = builder.finish();
	let bounds = path.tight_bounds().expect("an ellipse has bounds");
	assert!((bounds.x + 30.0).abs() < 0.01 && (bounds.y + 10.0).abs() < 0.01, "the tight bounds start at the extremes: {bounds:?}");
	assert!((bounds.width - 60.0).abs() < 0.02 && (bounds.height - 20.0).abs() < 0.02, "and are the full axes: {bounds:?}");

	let mut worst: f64 = 0.0;
	for point in flattened(&path) {
		let (x, y) = (point.x as f64 / 30.0, point.y as f64 / 10.0);
		worst = worst.max((x * x + y * y - 1.0).abs());
	}
	assert!(worst < 0.002, "the greatest deviation from the unit ellipse is {worst}");
}

#[test]
// A ROUNDED RECTANGLE WITH NO RADIUS IS THE RECTANGLE - the same verbs and the same points, not a
// shape that merely looks like one. A caller whose radius computed to zero gets what it would have
// drawn, and a backend's rectangle fast path still applies.
fn a_rounded_rectangle_with_no_radius_is_exactly_a_rectangle() {
	let rect = RectF::new(4.0, 6.0, 20.0, 12.0);
	let mut rounded = PathBuilder::new();
	rounded.add_rounded_rect(rect, 0.0, 0.0).expect("a square-cornered rounded rectangle");
	let mut plain = PathBuilder::new();
	plain.add_rect(rect).expect("a rectangle");
	let (rounded, plain) = (rounded.finish(), plain.finish());
	assert_eq!(rounded.verbs(), plain.verbs());
	assert_eq!(rounded.points(), plain.points());
}

#[test]
// RADII THAT DO NOT FIT ARE SCALED TOGETHER. Clamping each corner against its own side instead
// produces a rectangle whose corners have different curvatures - which is a shape nobody asked for,
// arrived at silently, and it is what makes the two policies distinguishable from the outside.
fn oversized_corner_radii_are_scaled_by_one_factor() {
	// A 100 by 40 rectangle with 30-pixel radii: the height allows 20 and the width allows 50, so
	// ONE factor of two thirds applies to both - and the corners stay circular.
	let mut builder = PathBuilder::new();
	builder.add_rounded_rect(RectF::new(0.0, 0.0, 100.0, 40.0), 30.0, 30.0).expect("a rounded rectangle");
	let path = builder.finish();
	assert_eq!(path.points()[0], PointF { x: 20.0, y: 0.0 }, "the top edge begins one scaled radius in");

	let bounds = path.tight_bounds().expect("bounds");
	assert!(bounds.width <= 100.01 && bounds.height <= 40.01, "the shape stays inside the rectangle: {bounds:?}");
	// THE CORNER IS A QUARTER OF A CIRCLE OF THE SCALED RADIUS, so the point at the corner's
	// midpoint is at the 45-degree position of a circle of radius 20 centred 20 in from each side.
	let centre = (20.0f64, 20.0f64);
	let midpoint = flattened(&path).into_iter().find(|point| point.x < 20.0 && point.y < 20.0).expect("the top-left corner is drawn");
	let (dx, dy) = (midpoint.x as f64 - centre.0, midpoint.y as f64 - centre.1);
	assert!(((dx * dx + dy * dy).sqrt() - 20.0).abs() < 0.05, "the corner is on a circle of the scaled radius");
}

#[test]
// AN ARC OF A QUARTER TURN IS THE ELLIPSE'S OWN FIRST SEGMENT. The two are built by different code -
// the ellipse from the frozen ratio with no trigonometry at all, the arc from `4/3 * tan(sweep/4)` -
// and they have to agree, or a rounded corner drawn as an arc and one drawn as part of a circle are
// different shapes.
fn a_quarter_turn_arc_agrees_with_the_ellipse_it_is_part_of() {
	let centre = PointF { x: 10.0, y: 20.0 };
	let mut arc = PathBuilder::new();
	arc.add_arc(centre, 40.0, 25.0, 0.0, core::f32::consts::FRAC_PI_2).expect("a quarter arc");
	let arc = arc.finish();
	let mut ellipse = PathBuilder::new();
	ellipse.add_ellipse(centre, 40.0, 25.0).expect("an ellipse");
	let ellipse = ellipse.finish();

	assert_eq!(arc.verbs(), &[Verb::MoveTo, Verb::CubicTo], "one cubic, and the subpath is left OPEN");
	for (from_arc, from_ellipse) in arc.points().iter().zip(ellipse.points().iter()) {
		assert!((from_arc.x - from_ellipse.x).abs() < 0.001 && (from_arc.y - from_ellipse.y).abs() < 0.001, "{from_arc:?} is not {from_ellipse:?}");
	}
}

#[test]
// A SWEEP BEYOND A FULL TURN IS ONE TURN. The second lap is invisible for a fill and doubles the
// winding number for the non-zero rule, so an animation that drives a sweep past `2*pi` would turn a
// ring into a disc at the moment it passed.
fn an_arc_sweeping_past_a_full_turn_draws_one() {
	let mut builder = PathBuilder::new();
	builder.add_arc(PointF { x: 0.0, y: 0.0 }, 10.0, 10.0, 0.0, 12.0).expect("an over-long arc");
	let path = builder.finish();
	let last = path.points().last().copied().expect("an arc has points");
	assert!((last.x - 10.0).abs() < 0.01 && last.y.abs() < 0.01, "a full turn ends where it started, not at {last:?}");
	assert_eq!(path.verbs().len(), 5, "a full turn is four cubics after the move, at a quarter turn each");
}

#[test]
// AN ARC JOINS WHAT IS ALREADY BEING DRAWN, and that is what makes it usable as one segment of a
// larger outline - a tab, a pie slice, a capsule - rather than only as a shape of its own.
fn an_arc_lines_to_its_start_inside_a_subpath_and_moves_to_it_outside_one() {
	let mut joined = PathBuilder::new();
	joined.move_to(PointF { x: 0.0, y: 0.0 }).expect("a start");
	joined.add_arc(PointF { x: 50.0, y: 0.0 }, 10.0, 10.0, 0.0, 1.0).expect("an arc");
	assert_eq!(joined.finish().verbs()[..2], [Verb::MoveTo, Verb::LineTo], "the arc reaches its start with a line");

	// AND A CLOSED SUBPATH IS NOT AN OPEN ONE: after a close the arc starts its own subpath rather
	// than drawing a segment back from wherever the closed shape ended.
	let mut after_a_shape = PathBuilder::new();
	after_a_shape.add_rect(RectF::new(0.0, 0.0, 4.0, 4.0)).expect("a rectangle");
	after_a_shape.add_arc(PointF { x: 50.0, y: 0.0 }, 10.0, 10.0, 0.0, 1.0).expect("an arc");
	let verbs = after_a_shape.finish();
	assert_eq!(verbs.verbs()[4..6], [Verb::Close, Verb::MoveTo], "the arc moves rather than lining across the gap");
}

#[test]
// A POLYGON IS CLOSED AND A POLYLINE IS NOT, which is the whole difference between them and the
// reason both exist: a stroked polyline has two end caps and a stroked polygon has none.
fn a_polyline_is_open_and_a_polygon_is_closed() {
	let corners = [PointF { x: 0.0, y: 0.0 }, PointF { x: 10.0, y: 0.0 }, PointF { x: 10.0, y: 10.0 }];
	let mut open = PathBuilder::new();
	open.add_polyline(&corners).expect("a polyline");
	assert_eq!(open.finish().verbs(), &[Verb::MoveTo, Verb::LineTo, Verb::LineTo]);
	let mut closed = PathBuilder::new();
	closed.add_polygon(&corners).expect("a polygon");
	assert_eq!(closed.finish().verbs(), &[Verb::MoveTo, Verb::LineTo, Verb::LineTo, Verb::Close]);
}

#[test]
// WHAT THE SHAPES REFUSE, AND WHY EACH REFUSAL IS BETTER THAN A DRAWING. A negative radius is a
// subtraction of two sizes that went wrong; a polygon of two points is a loop that produced nothing.
// Both have a shape that could be drawn for them, and drawing it hides the mistake inside a picture
// that is merely slightly wrong.
fn a_shape_whose_numbers_are_not_a_shape_is_refused() {
	let mut builder = PathBuilder::new();
	assert_eq!(builder.add_circle(PointF::default(), -1.0).err(), Some(Error::DegenerateShape { what: "an ellipse's x radius" }));
	assert!(matches!(builder.add_rounded_rect(RectF::new(0.0, 0.0, 4.0, 4.0), f32::NAN, 1.0), Err(Error::DegenerateShape { .. })));
	assert!(matches!(builder.add_arc(PointF::default(), 1.0, 1.0, f32::INFINITY, 1.0), Err(Error::DegenerateShape { .. })));
	assert_eq!(builder.add_polyline(&[PointF::default()]).err(), Some(Error::DegenerateShape { what: "a polyline of fewer than two points" }));
	assert_eq!(builder.add_polygon(&[PointF::default(), PointF::default()]).err(), Some(Error::DegenerateShape { what: "a polygon of fewer than three points" }));
	// AND NOTHING WAS RECORDED BY ANY OF THEM: a refusal that had already pushed a `move_to` would
	// leave a stray subpath in a path the caller went on to use.
	assert!(builder.finish().is_empty());
}

#[test]
// THE SINE AND COSINE AN ARC IS BUILT FROM, held against the host's own - at the quadrant
// boundaries, which is where a range reduction is wrong if it is wrong anywhere.
fn the_arc_sine_and_cosine_agree_with_the_host_at_every_quadrant() {
	for step in -16..=16 {
		let angle = step as f32 * core::f32::consts::FRAC_PI_2 * 0.5;
		let (sine, cosine) = crate::shape::sin_cos_f32(angle);
		let (expected_sine, expected_cosine) = ((angle as f64).sin(), (angle as f64).cos());
		assert!((sine as f64 - expected_sine).abs() < 1e-6, "sin({angle}) is {sine} and not {expected_sine}");
		assert!((cosine as f64 - expected_cosine).abs() < 1e-6, "cos({angle}) is {cosine} and not {expected_cosine}");
	}
	// A NON-FINITE ANGLE HAS NO SINE, and answering zero for one would place an arc's points at the
	// centre rather than refusing - which is why `add_arc` checks the angle instead of trusting this.
	assert!(crate::shape::sin_cos_f32(f32::NAN).0.is_nan());
}

#[test]
// THE BOUNDS MAP OF THE NODES THAT ARE NOT AN EFFECT WITH A NAME. A node whose map is wrong produces
// a picture that is correct in the middle and clipped at the edge, which is the defect that looks
// like a rasteriser bug and is not one.
fn the_general_filter_nodes_declare_what_they_read() {
	use crate::filter::{Channel, FilterNode};

	let output = RectF::new(10.0, 10.0, 20.0, 20.0);
	// ONE PIXEL ON EVERY SIDE is the three-by-three kernel's whole reach, which is why the size is
	// frozen: a bounds map for a kernel whose size is a parameter has to be computed rather than said.
	let convolution = FilterNode::Convolution { input: 0, weights: [[0.0; 3]; 3], divisor: 1.0, bias: 0.0 };
	assert_eq!(convolution.required_input(output), RectF::new(9.0, 9.0, 22.0, 22.0));
	// A MORPHOLOGY READS ITS RADIUS and no more - unlike the blur, which reads three standard
	// deviations because a Gaussian has no edge and a structuring element does.
	assert_eq!(FilterNode::MorphologyDilate { input: 0, x: 3.0, y: 1.0 }.required_input(output), RectF::new(7.0, 9.0, 26.0, 22.0));
	// A DISPLACEMENT READS AS FAR AS ITS SCALE CAN REACH. Nothing here knows the map's values, so the
	// bound is the largest offset the scale can produce.
	assert_eq!(FilterNode::DisplacementMap { input: 0, map: 1, scale: 4.0, x_channel: Channel::Red, y_channel: Channel::Green }.required_input(output), RectF::new(6.0, 6.0, 28.0, 28.0));
	// A CROP READS ONLY WHAT SURVIVES IT, which is what makes a crop the cheap way to bound an effect
	// rather than a mask applied after the cost has already been paid.
	assert_eq!(FilterNode::Crop { input: 0, rect: RectF::new(0.0, 0.0, 16.0, 16.0) }.required_input(output), RectF::new(10.0, 10.0, 6.0, 6.0));
	// AND A TILE READS ITS RECTANGLE WHATEVER IS ASKED FOR: every output pixel comes from inside it.
	assert_eq!(FilterNode::Tile { input: 0, rect: RectF::new(4.0, 4.0, 8.0, 8.0) }.required_input(output), RectF::new(4.0, 4.0, 8.0, 8.0));

	// A MORPHOLOGY AND A DISPLACEMENT ARE BOUNDED BY THE SAME CEILING AS A BLUR, because all three
	// are a per-pixel cost that scales with a number the caller chose.
	let mut graph = FilterGraph::default();
	let source = graph.push(FilterNode::Source).expect("a node");
	let ceiling = graphics_profile::RENDER2D_PROFILE_1_MIN_LIMITS.max_filter_radius as f32;
	assert!(matches!(graph.push(FilterNode::MorphologyDilate { input: source, x: ceiling + 1.0, y: 0.0 }), Err(Error::LimitExceeded { limit: "filter radius", .. })));
	assert!(matches!(graph.push(FilterNode::DisplacementMap { input: source, map: source, scale: ceiling + 1.0, x_channel: Channel::Red, y_channel: Channel::Green }), Err(Error::LimitExceeded { limit: "filter radius", .. })));
}
