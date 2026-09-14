//! THE CANONICAL ENCODING, which is an INTERNAL BINARY FORM and not a wire ABI.
//!
//! WHY IT EXISTS AT ALL. Profile 1 requires the list to be IMMUTABLE, VERSIONED and CACHEABLE, and a
//! cache key is a hash of bytes - so the list has a byte form whether or not anything sends it
//! anywhere.
//!
//! WHAT IT IS NOT: stable across releases, endian-defined, rights-bearing, or safe to accept from
//! another process. It carries no capability, and nothing validates it as untrusted input, because
//! its only producer and only consumer are the same process's own `render2d`. Cross-process transport
//! is a named EXTENSION designed against a real consumer, and it adds byte order, rights and snapshot
//! semantics ON TOP of this schema rather than instead of it.
//!
//! AND THE ROUND TRIP IS WORTH TESTING FOR THE SAME REASON THE HASH IS: a list whose encoding loses a
//! field silently produces a cache HIT on a drawing that is not the one recorded.

use alloc::vec::Vec;

use crate::list::{Command, DrawList};

/// A digest over the list's canonical form, for use as a cache key.
///
/// FNV-1a AND NOT A CRYPTOGRAPHIC HASH, deliberately: this keys a process-local cache whose inputs
/// this process produced. A cryptographic digest here would cost every frame and defend against an
/// adversary that, by this form's own definition, cannot reach it.
pub fn digest(list: &DrawList) -> u64 {
	let mut hash = 0xcbf2_9ce4_8422_2325u64;
	for byte in encode(list) {
		hash ^= byte as u64;
		hash = hash.wrapping_mul(0x100_0000_01b3);
	}
	hash
}

/// The canonical bytes.
///
/// THE VERSION IS THE FIRST FIELD, so two lists recorded under different schemas cannot collide in a
/// cache however similar their commands are.
pub fn encode(list: &DrawList) -> Vec<u8> {
	let mut out = Vec::new();
	out.extend_from_slice(&list.version().to_le_bytes());
	out.extend_from_slice(&(list.commands().len() as u32).to_le_bytes());
	for command in list.commands() {
		encode_command(command, &mut out);
	}
	let resources = list.resources();
	out.extend_from_slice(&(resources.paths.len() as u32).to_le_bytes());
	for path in &resources.paths {
		out.extend_from_slice(&(path.verbs().len() as u32).to_le_bytes());
		for verb in path.verbs() {
			out.push(*verb as u8);
		}
		for point in path.points() {
			out.extend_from_slice(&point.x.to_bits().to_le_bytes());
			out.extend_from_slice(&point.y.to_bits().to_le_bytes());
		}
	}
	out.extend_from_slice(&(resources.images.len() as u32).to_le_bytes());
	for image in &resources.images {
		out.extend_from_slice(&image.identity.to_le_bytes());
		out.extend_from_slice(&image.layout_generation.to_le_bytes());
		// THE CONTENT GENERATION IS IN THE ENCODING AND NOT IN THE PREPARED KEY, and the difference is
		// the whole content-versus-structure rule: two frames of a video are different DRAWINGS, so
		// they must not share a cached raster - and they are the same STRUCTURE, so the prepared list
		// stays valid.
		out.extend_from_slice(&image.content_generation.to_le_bytes());
	}
	out.extend_from_slice(&(resources.stops.len() as u32).to_le_bytes());
	for stops in &resources.stops {
		out.extend_from_slice(&(stops.len() as u32).to_le_bytes());
		for stop in stops {
			out.extend_from_slice(&stop.offset.to_bits().to_le_bytes());
			for channel in [stop.color.red, stop.color.green, stop.color.blue, stop.color.alpha] {
				out.extend_from_slice(&channel.to_bits().to_le_bytes());
			}
		}
	}
	out.extend_from_slice(&(resources.dashes.len() as u32).to_le_bytes());
	for pattern in &resources.dashes {
		out.extend_from_slice(&(pattern.len() as u32).to_le_bytes());
		for length in pattern {
			out.extend_from_slice(&length.to_bits().to_le_bytes());
		}
	}
	out.extend_from_slice(&(resources.glyph_runs.len() as u32).to_le_bytes());
	for run in &resources.glyph_runs {
		out.extend_from_slice(&run.face.face.file.0);
		out.extend_from_slice(&run.face.generation.0.to_le_bytes());
		out.extend_from_slice(&run.size.raw().to_le_bytes());
		out.extend_from_slice(&(run.glyphs.len() as u32).to_le_bytes());
		for glyph in &run.glyphs {
			out.extend_from_slice(&glyph.glyph.to_le_bytes());
			out.extend_from_slice(&glyph.x_offset.raw().to_le_bytes());
			out.extend_from_slice(&glyph.y_offset.raw().to_le_bytes());
			out.extend_from_slice(&glyph.x_advance.raw().to_le_bytes());
			out.extend_from_slice(&glyph.y_advance.raw().to_le_bytes());
		}
	}
	out.extend_from_slice(&(resources.filters.len() as u32).to_le_bytes());
	for graph in &resources.filters {
		out.extend_from_slice(&(graph.nodes().len() as u32).to_le_bytes());
		for node in graph.nodes() {
			encode_filter(node, &mut out);
		}
	}
	out
}

fn encode_command(command: &Command, out: &mut Vec<u8>) {
	// A DISCRIMINANT PER COMMAND, written first, so that two commands with the same payload bytes
	// cannot encode identically.
	let tag: u8 = match command {
		Command::FillPath { .. } => 1,
		Command::StrokePath { .. } => 2,
		Command::DrawImage { .. } => 3,
		Command::DrawGlyphRun { .. } => 4,
		Command::PushClip { .. } => 5,
		Command::PushClipMask { .. } => 9,
		Command::PopClip => 6,
		Command::BeginLayer { .. } => 7,
		Command::EndLayer => 8,
	};
	out.push(tag);
	match command {
		Command::FillPath { path, rule, transform, antialias, blend, operator, opacity, .. } => {
			out.extend_from_slice(&path.0.to_le_bytes());
			out.push(*rule as u8);
			encode_transform(transform, out);
			out.push(*antialias as u8);
			out.push(*blend as u8);
			out.push(*operator as u8);
			out.extend_from_slice(&opacity.to_bits().to_le_bytes());
		}
		Command::StrokePath { path, style, transform, antialias, blend, operator, opacity, .. } => {
			out.extend_from_slice(&path.0.to_le_bytes());
			out.extend_from_slice(&style.width.to_bits().to_le_bytes());
			out.push(style.cap as u8);
			out.push(style.join as u8);
			out.extend_from_slice(&style.miter_limit.to_bits().to_le_bytes());
			out.push(style.scaling as u8);
			// THE DASH IS PART OF THE DRAWING, so it is part of the digest: a solid stroke and a
			// dashed one are different pictures, and a cache that shared a raster between them would
			// be returning the wrong one for whichever was drawn second.
			match style.dash {
				Some(dash) => {
					out.push(1);
					out.extend_from_slice(&dash.pattern.0.to_le_bytes());
					out.extend_from_slice(&dash.phase.to_bits().to_le_bytes());
				}
				None => out.push(0),
			}
			encode_transform(transform, out);
			out.push(*antialias as u8);
			out.push(*blend as u8);
			out.push(*operator as u8);
			out.extend_from_slice(&opacity.to_bits().to_le_bytes());
		}
		Command::DrawImage { image, source, destination, quality, transform, blend, operator, opacity } => {
			out.extend_from_slice(&image.0.to_le_bytes());
			encode_rect(source, out);
			encode_rect(destination, out);
			out.push(*quality as u8);
			encode_transform(transform, out);
			out.push(*blend as u8);
			out.push(*operator as u8);
			out.extend_from_slice(&opacity.to_bits().to_le_bytes());
		}
		Command::DrawGlyphRun { run, transform, blend, operator, opacity, .. } => {
			out.extend_from_slice(&run.0.to_le_bytes());
			encode_transform(transform, out);
			out.push(*blend as u8);
			out.push(*operator as u8);
			out.extend_from_slice(&opacity.to_bits().to_le_bytes());
		}
		Command::PushClipMask { image, transform, inverse } => {
			out.extend_from_slice(&image.0.to_le_bytes());
			encode_transform(transform, out);
			out.push(u8::from(*inverse));
		}
		Command::PushClip { path, rule, transform, antialias, inverse } => {
			out.push(u8::from(*inverse));
			out.extend_from_slice(&path.0.to_le_bytes());
			out.push(*rule as u8);
			encode_transform(transform, out);
			out.push(*antialias as u8);
		}
		Command::BeginLayer { bounds, opacity, blend, operator, filter } => {
			match bounds {
				Some(rect) => {
					out.push(1);
					encode_rect(rect, out);
				}
				None => out.push(0),
			}
			out.extend_from_slice(&opacity.to_bits().to_le_bytes());
			out.push(*blend as u8);
			out.push(*operator as u8);
			match filter {
				Some(handle) => {
					out.push(1);
					out.extend_from_slice(&handle.0.to_le_bytes());
				}
				None => out.push(0),
			}
		}
		Command::PopClip | Command::EndLayer => {}
	}
}

fn encode_filter(node: &crate::filter::FilterNode, out: &mut Vec<u8>) {
	use crate::filter::FilterNode;
	match node {
		FilterNode::Source => out.push(1),
		FilterNode::Backdrop => out.push(10),
		FilterNode::Image(handle) => {
			out.push(2);
			out.extend_from_slice(&handle.0.to_le_bytes());
		}
		FilterNode::Blur { input, x, y } => {
			out.push(3);
			out.extend_from_slice(&input.to_le_bytes());
			out.extend_from_slice(&x.to_bits().to_le_bytes());
			out.extend_from_slice(&y.to_bits().to_le_bytes());
		}
		FilterNode::Offset { input, dx, dy } => {
			out.push(4);
			out.extend_from_slice(&input.to_le_bytes());
			out.extend_from_slice(&dx.to_bits().to_le_bytes());
			out.extend_from_slice(&dy.to_bits().to_le_bytes());
		}
		FilterNode::ColorMatrix { input, matrix } => {
			out.push(5);
			out.extend_from_slice(&input.to_le_bytes());
			for row in matrix {
				for value in row {
					out.extend_from_slice(&value.to_bits().to_le_bytes());
				}
			}
		}
		FilterNode::Flood { color } => {
			out.push(6);
			for channel in [color.red, color.green, color.blue, color.alpha] {
				out.extend_from_slice(&channel.to_bits().to_le_bytes());
			}
		}
		FilterNode::Composite { source, backdrop, operator } => {
			out.push(7);
			out.extend_from_slice(&source.to_le_bytes());
			out.extend_from_slice(&backdrop.to_le_bytes());
			out.push(*operator as u8);
		}
		FilterNode::Blend { source, backdrop, mode } => {
			out.push(8);
			out.extend_from_slice(&source.to_le_bytes());
			out.extend_from_slice(&backdrop.to_le_bytes());
			out.push(*mode as u8);
		}
		FilterNode::In { input, mask } => {
			out.push(9);
			out.extend_from_slice(&input.to_le_bytes());
			out.extend_from_slice(&mask.to_le_bytes());
		}
		FilterNode::Convolution { input, weights, divisor, bias } => {
			out.push(11);
			out.extend_from_slice(&input.to_le_bytes());
			for row in weights {
				for value in row {
					out.extend_from_slice(&value.to_bits().to_le_bytes());
				}
			}
			out.extend_from_slice(&divisor.to_bits().to_le_bytes());
			out.extend_from_slice(&bias.to_bits().to_le_bytes());
		}
		// DILATE AND ERODE ARE TWO TAGS AND NOT ONE WITH A FLAG: the encoding is what a cache is keyed
		// by, and a flag inside a tag is a byte a reader can miss while still parsing the node.
		FilterNode::MorphologyDilate { input, x, y } => encode_morphology(12, *input, *x, *y, out),
		FilterNode::MorphologyErode { input, x, y } => encode_morphology(13, *input, *x, *y, out),
		FilterNode::DisplacementMap { input, map, scale, x_channel, y_channel } => {
			out.push(14);
			out.extend_from_slice(&input.to_le_bytes());
			out.extend_from_slice(&map.to_le_bytes());
			out.extend_from_slice(&scale.to_bits().to_le_bytes());
			out.push(*x_channel as u8);
			out.push(*y_channel as u8);
		}
		FilterNode::Crop { input, rect } => {
			out.push(15);
			out.extend_from_slice(&input.to_le_bytes());
			encode_rect(rect, out);
		}
		FilterNode::Tile { input, rect } => {
			out.push(16);
			out.extend_from_slice(&input.to_le_bytes());
			encode_rect(rect, out);
		}
	}
}

fn encode_morphology(tag: u8, input: u16, x: f32, y: f32, out: &mut Vec<u8>) {
	out.push(tag);
	out.extend_from_slice(&input.to_le_bytes());
	out.extend_from_slice(&x.to_bits().to_le_bytes());
	out.extend_from_slice(&y.to_bits().to_le_bytes());
}

fn encode_transform(transform: &crate::transform::Transform, out: &mut Vec<u8>) {
	for row in transform.m {
		for value in row {
			out.extend_from_slice(&value.to_bits().to_le_bytes());
		}
	}
}

fn encode_rect(rect: &graphics_core::geom::RectF, out: &mut Vec<u8>) {
	for value in [rect.x, rect.y, rect.width, rect.height] {
		out.extend_from_slice(&value.to_bits().to_le_bytes());
	}
}
