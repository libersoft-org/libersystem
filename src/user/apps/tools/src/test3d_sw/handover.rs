//! The resolved attachment crossing into the shared image model. Each band exclusively borrows
//! its destination rows; workers only read attachments. The fixed array books all dispatch storage.

use graphics_core::layout::RowOrigin;
use graphics_core::pixel::{Rgba, write};
use graphics_core::{ImageViewMut, OwnedImage};
use render_math::Vec4;
use soft3d::pass::Colour;

pub(super) const MAX_BANDS: usize = 32;

pub(super) struct Band<'a> {
	first_y: u32,
	view: ImageViewMut<'a>,
}

pub(super) fn bands(image: &mut OwnedImage) -> [Option<Band<'_>>; MAX_BANDS] {
	let layout = *image.layout();
	let rows = layout.extent.height.div_ceil(MAX_BANDS as u32);
	let mut chunks = image.bytes_mut().chunks_mut(rows as usize * layout.pitch as usize);
	core::array::from_fn(|index| {
		let bytes = chunks.next()?;
		let mut part = layout;
		part.extent.height = (bytes.len() / layout.pitch as usize) as u32;
		let first_y = match layout.origin {
			RowOrigin::TopLeft => index as u32 * rows,
			RowOrigin::BottomLeft => layout.extent.height - index as u32 * rows - part.extent.height,
		};
		// OwnedImage allocates every pitched row, and each disjoint chunk contains whole rows.
		Some(Band { first_y, view: ImageViewMut::new(part, bytes).expect("a band's complete image rows") })
	})
}

pub(super) fn paint(band: &mut Band<'_>, colour: &Colour, identities: &Colour, overlay: bool) {
	let extent = band.view.layout().extent;
	for y in 0..extent.height {
		for x in 0..extent.width {
			let source_y = band.first_y + y;
			let texel = if overlay { identity_colour(identities.identity_at(x, source_y, 0).unwrap_or(0)) } else { colour.at(x, source_y, 0) };
			write(&mut band.view, x, y, Rgba::new(texel.x, texel.y, texel.z, 1.0));
		}
	}
}

/// Flat IDs retain the exact original demo colours; no filtering or float conversion of IDs.
fn identity_colour(ident: u32) -> Vec4 {
	match ident {
		1 => Vec4::new(0.95, 0.25, 0.20, 1.0),
		2 => Vec4::new(0.20, 0.45, 0.95, 1.0),
		3 => Vec4::new(0.95, 0.85, 0.20, 1.0),
		_ => Vec4::new(0.02, 0.02, 0.03, 1.0),
	}
}

#[cfg(test)]
#[path = "../../../../../tools/soft3d-bench/src/handover_tests.rs"]
mod tests;
