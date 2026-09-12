//! `IMAGE_COLOR_PROFILE_1.md`, generated from the registry it describes.
//!
//! A NORMATIVE DOCUMENT WRITTEN BY HAND DRIFTS FROM THE CODE, and this one is the worst case to let
//! drift: it carries the matrices, the transfer constants and the rounding rules that decide whether
//! two implementations produce the same pixels. Written from the registry, a change to a constant is
//! a change to the document in the same commit, and the hash over the canonical form makes it a line
//! in a diff.
//!
//! IT GOES TO `docs/graphics/` RATHER THAN `docs/gen/`, because it is a NORMATIVE specification that
//! other documents cite by name, and a citation into a generated directory reads as a build artefact.
//! Its generated banner says where it comes from.

use std::fmt::Write as _;

use graphics_profile::image::{self, AlphaMode, FORMATS, PRIMARIES, alpha_modes};

/// The canonical machine-readable form the hash is taken over.
///
/// ONE LINE PER ENTRY, IN PROFILE ORDER. Order is part of it: a reordering is a different document,
/// and a hash that ignored order would call two different registries the same profile.
pub fn canonical() -> String {
	let mut out = String::new();
	let _ = writeln!(out, "profile=image-color version={}", image::IMAGE_COLOR_PROFILE_VERSION);
	for format in FORMATS {
		let bits: Vec<String> = format.bits.iter().map(|bits| bits.to_string()).collect();
		let modes: Vec<&str> = alpha_modes(format).iter().map(|mode| mode.name()).collect();
		let _ = writeln!(out, "format={} channels={} bits={} encoding={} bytes={} carries={} alpha={}", format.name, format.channels, bits.join(","), format.encoding.name(), format.bytes_per_pixel, format.carries.name(), modes.join(","));
	}
	let _ = writeln!(out, "intermediate={}", image::CANONICAL_INTERMEDIATE);
	for semantics in [image::Semantics::Color, image::Semantics::Mask, image::Semantics::Normal, image::Semantics::Data, image::Semantics::Depth, image::Semantics::Identity] {
		let _ = writeln!(out, "semantics={} transfer-applies={}", semantics.name(), semantics.is_colour());
	}
	for semantics in [image::Semantics::Color, image::Semantics::Mask, image::Semantics::Normal, image::Semantics::Data, image::Semantics::Depth, image::Semantics::Identity] {
		let allowed: Vec<&str> = image::operations(semantics).iter().map(|operation| operation.name()).collect();
		let _ = writeln!(out, "operations={} allowed={}", semantics.name(), allowed.join(","));
	}
	for (name, value) in [("known", image::storage::KNOWN), ("packed-rgb-unorm", image::storage::PACKED_RGB_UNORM)] {
		let _ = writeln!(out, "storage={name} use={value}");
	}
	let _ = writeln!(out, "allocation row-alignment={} exported-padding={} short={} zero-extent={}", image::allocation::ROW_ALIGNMENT_BYTES, image::allocation::EXPORTED_PADDING, image::allocation::SHORT_ALLOCATION, image::allocation::ZERO_EXTENT);
	for (name, value) in [("pq", image::hdr_metadata::PQ), ("hlg", image::hdr_metadata::HLG), ("sdr", image::hdr_metadata::SDR)] {
		let _ = writeln!(out, "hdr-metadata={name} value={value}");
	}
	for primaries in PRIMARIES {
		let _ = writeln!(out, "primaries={} r={},{} g={},{} b={},{} w={},{}", primaries.name, primaries.red.x, primaries.red.y, primaries.green.x, primaries.green.y, primaries.blue.x, primaries.blue.y, primaries.white.x, primaries.white.y);
	}
	for space in image::COLOR_SPACES {
		let _ = writeln!(out, "space={} primaries={} transfer={}", space.name, space.primaries, space.transfer.name());
	}
	let _ = writeln!(out, "srgb threshold={} linear-threshold={} slope={} offset={} exponent={}", image::srgb::ENCODED_THRESHOLD, image::srgb::LINEAR_THRESHOLD, image::srgb::SLOPE, image::srgb::OFFSET, image::srgb::EXPONENT);
	let _ = writeln!(out, "pq m1={} m2={} c1={} c2={} c3={} peak={}", image::pq::M1, image::pq::M2, image::pq::C1, image::pq::C2, image::pq::C3, image::pq::PEAK_NITS);
	let _ = writeln!(out, "hlg a={} b={} c={} split={}", image::hlg::A, image::hlg::B, image::hlg::C, image::hlg::SPLIT);
	for (which, matrix) in [("forward", image::bradford::FORWARD), ("inverse", image::bradford::INVERSE)] {
		let row: Vec<String> = matrix.iter().flat_map(|row| row.iter().map(|value| value.to_string())).collect();
		let _ = writeln!(out, "bradford={which} {}", row.join(","));
	}
	let _ = writeln!(out, "reference diffuse-white-nits={} pq-absolute={}", image::reference::DIFFUSE_WHITE_NITS, image::reference::PQ_IS_ABSOLUTE);
	let _ = writeln!(out, "tone-map={} white={}", image::tone_map::NAME, image::tone_map::WHITE);
	let _ = writeln!(out, "gamut-map={} steps={} tolerance={}", image::gamut_map::NAME, image::gamut_map::STEPS, image::gamut_map::TOLERANCE);
	let rows: Vec<String> = image::dither::MATRIX.iter().map(|row| row.iter().map(|value| value.to_string()).collect::<Vec<String>>().join(",")).collect();
	let _ = writeln!(out, "dither={} phase={} matrix={}", image::dither::NAME, image::dither::PHASE, rows.join(";"));
	for (name, value) in [
		("float-to-integer", image::rounding::FLOAT_TO_INTEGER),
		("nan-to-integer", image::rounding::NAN_TO_INTEGER),
		("infinity-to-integer", image::rounding::INFINITY_TO_INTEGER),
		("float-to-half", image::rounding::FLOAT_TO_HALF),
		("half-subnormals", image::rounding::HALF_SUBNORMALS),
		("half-nan", image::rounding::HALF_NAN),
		("reserved-bits", image::rounding::RESERVED_BITS),
	] {
		let _ = writeln!(out, "rounding={name} rule={value}");
	}
	for layout in image::YUV_LAYOUTS {
		let _ = writeln!(out, "yuv={} planes={} order={} subsampling={}x{} bits={} placement={} pitch={}", layout.name, layout.planes, layout.order, layout.subsampling.0, layout.subsampling.1, layout.bits, layout.bit_placement, layout.pitch_rule);
	}
	for matrix in image::YUV_MATRICES {
		let _ = writeln!(out, "yuv-matrix={} kr={} kb={}", matrix.name, matrix.kr, matrix.kb);
	}
	let _ = writeln!(out, "yuv-range limited-luma-8={},{} limited-chroma-8={},{} full-8={},{}", image::yuv::LIMITED_LUMA_8.0, image::yuv::LIMITED_LUMA_8.1, image::yuv::LIMITED_CHROMA_8.0, image::yuv::LIMITED_CHROMA_8.1, image::yuv::FULL_8.0, image::yuv::FULL_8.1);
	let _ = writeln!(out, "yuv-range limited-luma-10={},{} limited-chroma-10={},{} full-10={},{}", image::yuv::LIMITED_LUMA_10.0, image::yuv::LIMITED_LUMA_10.1, image::yuv::LIMITED_CHROMA_10.0, image::yuv::LIMITED_CHROMA_10.1, image::yuv::FULL_10.0, image::yuv::FULL_10.1);
	for (name, value) in [
		("siting", image::yuv::SITING),
		("reconstruction", image::yuv::RECONSTRUCTION),
		("order", image::yuv::ORDER),
		("odd-extent", image::yuv::ODD_EXTENT),
		("crop-alignment", image::yuv::CROP_ALIGNMENT),
	] {
		let _ = writeln!(out, "yuv-rule={name} value={value}");
	}
	let _ = writeln!(out, "minima image-extent={} pitch-bytes={} planes={} gradient-stops={}", image::minima::IMAGE_EXTENT, image::minima::PITCH_BYTES, image::minima::PLANES, image::minima::GRADIENT_STOPS);
	for (name, value) in [
		("transfer-round-trip", image::tolerance::TRANSFER_ROUND_TRIP),
		("primary-round-trip", image::tolerance::PRIMARY_ROUND_TRIP),
		("tone-map", image::tolerance::TONE_MAP),
		("yuv-code-values", image::tolerance::YUV_CODE_VALUES),
		("dither", image::tolerance::DITHER),
		("filtered-sample", image::tolerance::FILTERED_SAMPLE),
	] {
		let _ = writeln!(out, "tolerance={name} value={value}");
	}
	for (name, value) in [
		("new-image", image::initialisation::NEW_IMAGE),
		("before-first-present", image::initialisation::BEFORE_FIRST_PRESENT),
		("padding", image::initialisation::PADDING),
	] {
		let _ = writeln!(out, "initialisation={name} value={value}");
	}
	for (name, value) in [("minimum-row-bytes", image::rows::MINIMUM_ROW_BYTES), ("pitch", image::rows::PITCH)] {
		let _ = writeln!(out, "row={name} value={value}");
	}
	for (name, value) in [
		("minimum-visible-bytes", image::spans::MINIMUM_VISIBLE_BYTES),
		("backend-access-span", image::spans::BACKEND_ACCESS_SPAN),
		("allocation-len", image::spans::ALLOCATION_LEN),
	] {
		let _ = writeln!(out, "span={name} value={value}");
	}
	out
}

/// The document a person reads.
pub fn document(hash: &str) -> String {
	let mut out = String::from("<!-- @generated by profile-doc from the image and colour registry. Do not edit; run `./gen.sh`. -->\n");
	let _ = writeln!(out, "# Image and Colour Profile {}\n", image::IMAGE_COLOR_PROFILE_VERSION);
	let _ = writeln!(out, "Profile hash: `{hash}`\n");
	let _ = writeln!(out, "What a pixel IS, what it MEANS, and the exact numbers every conversion between two of them");
	let _ = writeln!(out, "uses. \"Supports colour management\" is not a contract: two implementations can satisfy that");
	let _ = writeln!(out, "sentence and produce visibly different images, which is what this document exists to prevent.");
	let _ = writeln!(out, "Every value below is frozen. A change to one is a change to the profile version in the same");
	let _ = writeln!(out, "edit, and the hash above is what makes that a line in a diff.\n");

	let _ = writeln!(out, "## Storage formats\n");
	let _ = writeln!(out, "Eight-bit RGB is what a display takes; it is not what a coverage mask, a glyph cache, a filter");
	let _ = writeln!(out, "intermediate, a wide-gamut composite or an HDR target is made of. Channel order is defined by the");
	let _ = writeln!(out, "format NAME and never by host endianness.\n");
	let _ = writeln!(out, "| format | channels | bits | encoding | bytes | alpha modes | for |");
	let _ = writeln!(out, "| --- | --- | --- | --- | ---: | --- | --- |");
	for format in FORMATS {
		let bits: Vec<String> = format.bits.iter().map(|bits| bits.to_string()).collect();
		let modes: Vec<&str> = alpha_modes(format).iter().map(|mode| mode.name()).collect();
		let _ = writeln!(out, "| `{}` | {} | {} | {} | {} | {} | {} |", format.name, format.channels, bits.join(":"), format.encoding.name(), format.bytes_per_pixel, modes.join(", "), format.purpose);
	}
	let _ = writeln!(out, "\nThe canonical intermediate is `{}`, premultiplied and LINEAR: every layer, filter", image::CANONICAL_INTERMEDIATE);
	let _ = writeln!(out, "intermediate and offscreen composite. Repeated compositing through eight-bit sRGB bands and loses");
	let _ = writeln!(out, "precision, and a blur over an eight-bit intermediate is where it shows first.\n");
	let _ = writeln!(out, "`{}` on a format with no alpha channel is a TYPE ERROR and not an unsupported case.", AlphaMode::Straight.name());
	let _ = writeln!(out, "Under `{}` an alpha channel reads and writes its maximum rather than being carried through", AlphaMode::Opaque.name());
	let _ = writeln!(out, "unexamined. And an ALPHA-ONLY format admits `{}` alone: premultiplication is a relation", AlphaMode::Straight.name());
	let _ = writeln!(out, "between colour and alpha, and with no colour there is nothing to have been multiplied.\n");

	let _ = writeln!(out, "## What an image means\n");
	let _ = writeln!(out, "A normal map, a roughness map, a coverage mask and a depth buffer are all just bytes, and running");
	let _ = writeln!(out, "any of them through an sRGB decode corrupts them silently. A transfer function applies to colour");
	let _ = writeln!(out, "and to nothing else.\n");
	let _ = writeln!(out, "| semantics | a transfer function applies |");
	let _ = writeln!(out, "| --- | --- |");
	for semantics in [image::Semantics::Color, image::Semantics::Mask, image::Semantics::Normal, image::Semantics::Data, image::Semantics::Depth, image::Semantics::Identity] {
		let _ = writeln!(out, "| `{}` | {} |", semantics.name(), if semantics.is_colour() { "yes" } else { "NO" });
	}

	let _ = writeln!(out, "\n## What may be done to an image\n");
	let _ = writeln!(out, "Filtering an identity image averages two object ids into a third that names a different object;");
	let _ = writeln!(out, "dithering a mask adds noise to coverage; a transfer function on anything but colour is the classic");
	let _ = writeln!(out, "one. Each refusal below is a defect somebody has shipped.\n");
	let _ = writeln!(out, "| semantics | operations |");
	let _ = writeln!(out, "| --- | --- |");
	for semantics in [image::Semantics::Color, image::Semantics::Mask, image::Semantics::Normal, image::Semantics::Data, image::Semantics::Depth, image::Semantics::Identity] {
		let allowed: Vec<String> = image::operations(semantics).iter().map(|operation| format!("`{}`", operation.name())).collect();
		let _ = writeln!(out, "| `{}` | {} |", semantics.name(), allowed.join(", "));
	}
	let _ = writeln!(out, "\n| storage | may be used for |");
	let _ = writeln!(out, "| --- | --- |");
	for (name, value) in [("`Known(PixelFormat)`", image::storage::KNOWN), ("`PackedRgbUnorm`", image::storage::PACKED_RGB_UNORM)] {
		let _ = writeln!(out, "| {name} | {value} |");
	}

	let _ = writeln!(out, "\n## Allocation and padding\n");
	let _ = writeln!(out, "| rule | value |");
	let _ = writeln!(out, "| --- | --- |");
	let _ = writeln!(out, "| row alignment | {} bytes |", image::allocation::ROW_ALIGNMENT_BYTES);
	let _ = writeln!(out, "| padding on export | {} |", image::allocation::EXPORTED_PADDING);
	let _ = writeln!(out, "| an allocation below the minimum visible bytes | {} |", image::allocation::SHORT_ALLOCATION);
	let _ = writeln!(out, "| a zero extent | {} |", image::allocation::ZERO_EXTENT);
	let _ = writeln!(out, "\nPadding is part of the image's BYTES and not part of the image: never read as pixel content, and");
	let _ = writeln!(out, "written as zero on export, so two exports of one image are the same bytes - which is what makes an");
	let _ = writeln!(out, "image hashable and a golden comparison meaningful.\n");

	let _ = writeln!(out, "## HDR metadata\n");
	let _ = writeln!(out, "Without it a PQ image is untone-mappable: PQ is ABSOLUTE, so a display dimmer than the content has");
	let _ = writeln!(out, "to know what the content's peak actually was, and \"assume ten thousand\" tone-maps every image as");
	let _ = writeln!(out, "though it were the brightest ever made.\n");
	let _ = writeln!(out, "| transfer | metadata |");
	let _ = writeln!(out, "| --- | --- |");
	for (name, value) in [("PQ", image::hdr_metadata::PQ), ("HLG", image::hdr_metadata::HLG), ("SDR", image::hdr_metadata::SDR)] {
		let _ = writeln!(out, "| {name} | {value} |");
	}

	let _ = writeln!(out, "\n## Primaries and white point\n");
	let _ = writeln!(out, "| set | red | green | blue | white |");
	let _ = writeln!(out, "| --- | --- | --- | --- | --- |");
	for semantics in [image::Semantics::Color, image::Semantics::Mask, image::Semantics::Normal, image::Semantics::Data, image::Semantics::Depth, image::Semantics::Identity] {
		let allowed: Vec<&str> = image::operations(semantics).iter().map(|operation| operation.name()).collect();
		let _ = writeln!(out, "operations={} allowed={}", semantics.name(), allowed.join(","));
	}
	for (name, value) in [("known", image::storage::KNOWN), ("packed-rgb-unorm", image::storage::PACKED_RGB_UNORM)] {
		let _ = writeln!(out, "storage={name} use={value}");
	}
	let _ = writeln!(out, "allocation row-alignment={} exported-padding={} short={} zero-extent={}", image::allocation::ROW_ALIGNMENT_BYTES, image::allocation::EXPORTED_PADDING, image::allocation::SHORT_ALLOCATION, image::allocation::ZERO_EXTENT);
	for (name, value) in [("pq", image::hdr_metadata::PQ), ("hlg", image::hdr_metadata::HLG), ("sdr", image::hdr_metadata::SDR)] {
		let _ = writeln!(out, "hdr-metadata={name} value={value}");
	}
	for primaries in PRIMARIES {
		let _ = writeln!(out, "| {} | {}, {} | {}, {} | {}, {} | {}, {} |", primaries.name, primaries.red.x, primaries.red.y, primaries.green.x, primaries.green.y, primaries.blue.x, primaries.blue.y, primaries.white.x, primaries.white.y);
	}
	let _ = writeln!(out, "\n| colour space | primaries | transfer |");
	let _ = writeln!(out, "| --- | --- | --- |");
	for space in image::COLOR_SPACES {
		let _ = writeln!(out, "| `{}` | {} | `{}` |", space.name, space.primaries, space.transfer.name());
	}

	let _ = writeln!(out, "\n## Transfer functions\n");
	let _ = writeln!(out, "### sRGB\n");
	let _ = writeln!(out, "NOT a pure power law, and the linear segment is the part that gets dropped. A 2.2 power law is");
	let _ = writeln!(out, "close enough to look right and wrong enough that two implementations disagree in the darks, which");
	let _ = writeln!(out, "is where banding lives.\n");
	let _ = writeln!(out, "```text");
	let _ = writeln!(out, "encoded -> linear:  c <= {}      ->  c / {}", image::srgb::ENCODED_THRESHOLD, image::srgb::SLOPE);
	let _ = writeln!(out, "                    otherwise       ->  ((c + {}) / {}) ^ {}", image::srgb::OFFSET, 1.0 + image::srgb::OFFSET, image::srgb::EXPONENT);
	let _ = writeln!(out, "linear -> encoded:  c <= {}    ->  c * {}", image::srgb::LINEAR_THRESHOLD, image::srgb::SLOPE);
	let _ = writeln!(out, "                    otherwise       ->  {} * c ^ (1 / {}) - {}", 1.0 + image::srgb::OFFSET, image::srgb::EXPONENT, image::srgb::OFFSET);
	let _ = writeln!(out, "```\n");
	let _ = writeln!(out, "### PQ, SMPTE ST 2084\n");
	let _ = writeln!(out, "Its constants are twelve-bit FRACTIONS and are written as such: rounding them to decimals loses");
	let _ = writeln!(out, "the identity that makes two implementations agree at the endpoints, and the endpoints are where a");
	let _ = writeln!(out, "display's black level and peak white are decided. PQ is ABSOLUTE: an encoded 1.0 is");
	let _ = writeln!(out, "{} cd/m².\n", image::pq::PEAK_NITS);
	let _ = writeln!(out, "```text");
	let _ = writeln!(out, "m1 = 2610 / 16384        = {}", image::pq::M1);
	let _ = writeln!(out, "m2 = 2523 / 4096 * 128   = {}", image::pq::M2);
	let _ = writeln!(out, "c1 = 3424 / 4096         = {}", image::pq::C1);
	let _ = writeln!(out, "c2 = 2413 / 4096 * 32    = {}", image::pq::C2);
	let _ = writeln!(out, "c3 = 2392 / 4096 * 32    = {}", image::pq::C3);
	let _ = writeln!(out);
	let _ = writeln!(out, "encoded -> linear:  Y = ((max(N^(1/m2) - c1, 0)) / (c2 - c3 * N^(1/m2))) ^ (1/m1)");
	let _ = writeln!(out, "linear -> encoded:  N = ((c1 + c2 * Y^m1) / (1 + c3 * Y^m1)) ^ m2");
	let _ = writeln!(out, "```\n");
	let _ = writeln!(out, "### HLG, ITU-R BT.2100\n");
	let _ = writeln!(out, "`b` and `c` are derived from `a` - `b = 1 - 4a` and `c = 0.5 - a ln(4a)` - and are written out");
	let _ = writeln!(out, "anyway, because a reader checking one implementation against another needs the value rather than");
	let _ = writeln!(out, "the derivation.\n");
	let _ = writeln!(out, "```text");
	let _ = writeln!(out, "a = {}    b = {}    c = {}", image::hlg::A, image::hlg::B, image::hlg::C);
	let _ = writeln!(out);
	let _ = writeln!(out, "linear -> encoded:  E <= 1/12   ->  sqrt(3E)");
	let _ = writeln!(out, "                    otherwise   ->  a * ln(12E - b) + c");
	let _ = writeln!(out, "```\n");

	let _ = writeln!(out, "## Chromatic adaptation\n");
	let _ = writeln!(out, "BRADFORD, named and written down, because \"adapts between white points\" is satisfied by three");
	let _ = writeln!(out, "different matrices in common use and they do not agree. Adaptation happens in this cone space and");
	let _ = writeln!(out, "in no other.\n");
	let _ = writeln!(out, "```text");
	for (which, matrix) in [("forward", image::bradford::FORWARD), ("inverse", image::bradford::INVERSE)] {
		let _ = writeln!(out, "{which}:");
		for row in matrix {
			let _ = writeln!(out, "  [{:>12}, {:>12}, {:>12}]", row[0], row[1], row[2]);
		}
	}
	let _ = writeln!(out, "```\n");

	let _ = writeln!(out, "## Reference white, tone mapping and gamut mapping\n");
	let _ = writeln!(out, "Diffuse white is {} cd/m². A relative 1.0 in an SDR space is that much light, which is", image::reference::DIFFUSE_WHITE_NITS);
	let _ = writeln!(out, "what makes an SDR image and an HDR image composable at all. Inside the pipeline a linear value may");
	let _ = writeln!(out, "be negative or above one - a wide-gamut colour in a narrower space is negative, and a highlight is");
	let _ = writeln!(out, "above one - and clamping happens at OUTPUT and nowhere else.\n");
	let _ = writeln!(out, "TONE MAPPING is {}, with white at {} times diffuse white:\n", image::tone_map::NAME, image::tone_map::WHITE);
	let _ = writeln!(out, "```text");
	let _ = writeln!(out, "L_out = L * (1 + L / white^2) / (1 + L)");
	let _ = writeln!(out, "```\n");
	let _ = writeln!(out, "applied to LUMINANCE, with the colour scaled by `L_out / L`, and with luminance taken from the");
	let _ = writeln!(out, "DESTINATION space's own coefficients: a Rec. 2020 colour's luminance is not its sRGB luminance. One");
	let _ = writeln!(out, "parameter and exact reproducibility were chosen over a filmic curve, which is prettier and is five");
	let _ = writeln!(out, "constants two implementations will copy from different sources.\n");
	let _ = writeln!(out, "GAMUT MAPPING is {}, in {} steps to a tolerance of", image::gamut_map::NAME, image::gamut_map::STEPS);
	let _ = writeln!(out, "{}. Clipping each channel shifts hue, and it shifts it most on exactly the", image::gamut_map::TOLERANCE);
	let _ = writeln!(out, "saturated colours a wide-gamut image was made for. A fixed step count is what makes two");
	let _ = writeln!(out, "implementations produce the same pixels rather than \"until it converges\".\n");

	let _ = writeln!(out, "## Dithering\n");
	let _ = writeln!(out, "{}, with the phase anchored to {}. Error diffusion carries state ACROSS", image::dither::NAME, image::dither::PHASE);
	let _ = writeln!(out, "pixels, which makes a tile-parallel renderer's output depend on how it decomposed the image - so it");
	let _ = writeln!(out, "is not merely vague here, it is incompatible with the architecture. A tile-relative phase makes the");
	let _ = writeln!(out, "pattern visibly restart at every tile boundary, which is the artefact that looks like a seam.\n");
	let _ = writeln!(out, "The offset added to a channel before quantisation is `(matrix[y % 8][x % 8] + 0.5) / 64 - 0.5` of");
	let _ = writeln!(out, "one quantisation step, with `x` and `y` the TARGET's coordinates.\n");
	let _ = writeln!(out, "```text");
	for row in image::dither::MATRIX {
		let cells: Vec<String> = row.iter().map(|value| format!("{value:>2}")).collect();
		let _ = writeln!(out, "  {}", cells.join(" "));
	}
	let _ = writeln!(out, "```\n");

	let _ = writeln!(out, "## Rounding, at every boundary\n");
	let _ = writeln!(out, "| boundary | rule |");
	let _ = writeln!(out, "| --- | --- |");
	for (name, value) in [
		("float to an integer channel", image::rounding::FLOAT_TO_INTEGER),
		("NaN to an integer channel", image::rounding::NAN_TO_INTEGER),
		("infinity to an integer channel", image::rounding::INFINITY_TO_INTEGER),
		("float into `R16G16B16A16_FLOAT`", image::rounding::FLOAT_TO_HALF),
		("half-float subnormals", image::rounding::HALF_SUBNORMALS),
		("NaN into a half", image::rounding::HALF_NAN),
		("an `X8` byte and reserved bits", image::rounding::RESERVED_BITS),
	] {
		let _ = writeln!(out, "| {name} | {value} |");
	}

	let _ = writeln!(out, "\n## YUV\n");
	let _ = writeln!(out, "| layout | planes | order | subsampling | bits | bit placement | pitch |");
	let _ = writeln!(out, "| --- | ---: | --- | --- | ---: | --- | --- |");
	for layout in image::YUV_LAYOUTS {
		let _ = writeln!(out, "| {} | {} | {} | {}x{} | {} | {} | {} |", layout.name, layout.planes, layout.order, layout.subsampling.0, layout.subsampling.1, layout.bits, layout.bit_placement, layout.pitch_rule);
	}
	let _ = writeln!(out, "\n| matrix | Kr | Kb |");
	let _ = writeln!(out, "| --- | ---: | ---: |");
	for matrix in image::YUV_MATRICES {
		let _ = writeln!(out, "| {} | {} | {} |", matrix.name, matrix.kr, matrix.kb);
	}
	let _ = writeln!(out, "\nLimited range at eight bits is luma {}..{} and chroma {}..{}; full range is", image::yuv::LIMITED_LUMA_8.0, image::yuv::LIMITED_LUMA_8.1, image::yuv::LIMITED_CHROMA_8.0, image::yuv::LIMITED_CHROMA_8.1);
	let _ = writeln!(out, "{}..{}. At ten bits: luma {}..{}, chroma {}..{}, full {}..{}. Getting this wrong is", image::yuv::FULL_8.0, image::yuv::FULL_8.1, image::yuv::LIMITED_LUMA_10.0, image::yuv::LIMITED_LUMA_10.1, image::yuv::LIMITED_CHROMA_10.0, image::yuv::LIMITED_CHROMA_10.1, image::yuv::FULL_10.0, image::yuv::FULL_10.1);
	let _ = writeln!(out, "the single commonest cause of washed-out or crushed video.\n");
	let _ = writeln!(out, "| rule | value |");
	let _ = writeln!(out, "| --- | --- |");
	for (name, value) in [
		("chroma siting", image::yuv::SITING),
		("reconstruction", image::yuv::RECONSTRUCTION),
		("ORDER OF OPERATIONS", image::yuv::ORDER),
		("odd extents", image::yuv::ODD_EXTENT),
		("crop alignment", image::yuv::CROP_ALIGNMENT),
	] {
		let _ = writeln!(out, "| {name} | {value} |");
	}
	let _ = writeln!(out, "\nMissing or short planes and invalid layouts are typed refusals, and every per-plane size uses");
	let _ = writeln!(out, "checked arithmetic.\n");

	let _ = writeln!(out, "## Guaranteed minima\n");
	let _ = writeln!(out, "A profile with no minima promises nothing: \"supports large images\" is satisfied by an");
	let _ = writeln!(out, "implementation that refuses at 513 pixels, so an application written against it discovers the real");
	let _ = writeln!(out, "limit by being refused, in front of a user. These are the numbers an application may assume without");
	let _ = writeln!(out, "asking.\n");
	let _ = writeln!(out, "| minimum | value |");
	let _ = writeln!(out, "| --- | ---: |");
	let _ = writeln!(out, "| image extent, either direction | {} |", image::minima::IMAGE_EXTENT);
	let _ = writeln!(out, "| pitch, in bytes | {} |", image::minima::PITCH_BYTES);
	let _ = writeln!(out, "| planes | {} |", image::minima::PLANES);
	let _ = writeln!(out, "| gradient colour stops | {} |", image::minima::GRADIENT_STOPS);

	let _ = writeln!(out, "\n## Conformance tolerances\n");
	let _ = writeln!(out, "So that \"passes conformance\" has a boundary rather than a judgement. An exact comparison is the");
	let _ = writeln!(out, "wrong test for a transcendental, and a judgement is not a test at all.\n");
	let _ = writeln!(out, "| comparison | tolerance | in |");
	let _ = writeln!(out, "| --- | ---: | --- |");
	let _ = writeln!(out, "| a transfer function and its inverse | {} | a fraction of full range |", image::tolerance::TRANSFER_ROUND_TRIP);
	let _ = writeln!(out, "| a colour converted between spaces and back | {} | linear light |", image::tolerance::PRIMARY_ROUND_TRIP);
	let _ = writeln!(out, "| a tone-mapped value | {} | a fraction of full range |", image::tolerance::TONE_MAP);
	let _ = writeln!(out, "| a YUV image converted to RGB | {} | CODE VALUES at the source depth |", image::tolerance::YUV_CODE_VALUES);
	let _ = writeln!(out, "| the dither | {} | EXACT: the matrix and the phase are both stated |", image::tolerance::DITHER);
	let _ = writeln!(out, "| a filtered sample | {} | a fraction of full range |", image::tolerance::FILTERED_SAMPLE);

	let _ = writeln!(out, "\n## Initialisation\n");
	let _ = writeln!(out, "The uninitialised case is a DISCLOSURE rather than an aesthetic problem: a presentable image whose");
	let _ = writeln!(out, "bytes were never written shows whatever the allocator handed over, which in a system with a shared");
	let _ = writeln!(out, "page pool is somebody else's pixels - and the bug reads as a flicker rather than as a leak, so it is");
	let _ = writeln!(out, "not reported.\n");
	let _ = writeln!(out, "| when | rule |");
	let _ = writeln!(out, "| --- | --- |");
	for (name, value) in [
		("a newly allocated image", image::initialisation::NEW_IMAGE),
		("before a first present", image::initialisation::BEFORE_FIRST_PRESENT),
		("padding", image::initialisation::PADDING),
	] {
		let _ = writeln!(out, "| {name} | {value} |");
	}

	let _ = writeln!(out, "\n## The three byte spans\n");
	let _ = writeln!(out, "Three different questions about one image, and not \"the size\". An implementation that stores one");
	let _ = writeln!(out, "number answers all three with it and is wrong about two: it either accepts a buffer that cannot hold");
	let _ = writeln!(out, "the image, or refuses one that is exactly big enough.\n");
	let _ = writeln!(out, "First the two ROW quantities the spans are computed from:\n");
	let _ = writeln!(out, "| quantity | value |");
	let _ = writeln!(out, "| --- | --- |");
	for (name, value) in [("minimum row bytes", image::rows::MINIMUM_ROW_BYTES), ("pitch", image::rows::PITCH)] {
		let _ = writeln!(out, "| {name} | `{value}` |");
	}
	let _ = writeln!(out, "\nAnd the three spans:\n");
	let _ = writeln!(out, "| span | what it is |");
	let _ = writeln!(out, "| --- | --- |");
	for (name, value) in [
		("`minimum_visible_bytes()`", image::spans::MINIMUM_VISIBLE_BYTES),
		("`backend_access_span()`", image::spans::BACKEND_ACCESS_SPAN),
		("`allocation_len()`", image::spans::ALLOCATION_LEN),
	] {
		let _ = writeln!(out, "| {name} | {value} |");
	}
	out
}
