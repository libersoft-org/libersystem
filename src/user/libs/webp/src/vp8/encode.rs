use alloc::{vec, vec::Vec};

use super::entropy::BoolWriter;
use super::tables::{AC_QUANT, CATEGORY_BASE, CATEGORY_PROBS, COEFFICIENT_BANDS, COEFFICIENT_PROBS, COEFFICIENT_UPDATES, DC_QUANT, TOKEN_TREE, ZIGZAG};
use super::transform;
use crate::Error;

const Y_MODE_TREE: [i8; 8] = [-4, 2, 4, 6, 0, -1, -2, -3];
const Y_MODE_PROBS: [u8; 4] = [145, 156, 163, 128];
const BLOCK_MODE_TREE: [i8; 18] = [0, 2, -1, 4, -2, 6, 8, 12, -3, 10, -5, -6, -4, 14, -7, 16, -8, -9];
const BLOCK_DC_PROBS: [u8; 9] = [231, 120, 48, 89, 115, 113, 120, 152, 112];
const UV_MODE_TREE: [i8; 6] = [0, 2, -1, 4, -2, -3];
const UV_MODE_PROBS: [u8; 3] = [142, 114, 183];

struct SourcePlanes {
	y: Vec<u8>,
	u: Vec<u8>,
	v: Vec<u8>,
	y_stride: usize,
	uv_stride: usize,
	macroblock_columns: usize,
	macroblock_rows: usize,
}

pub(crate) fn encode_keyframe(image: &pix::RgbaImage, quality: u8, effort: u8) -> Result<Vec<u8>, Error> {
	if quality > 100 || effort > 100 || image.width == 0 || image.height == 0 || image.width > 0x3fff || image.height > 0x3fff {
		return Err(Error::Unsupported);
	}
	let source = source_planes(image)?;
	let quantizer_index = (127 - u16::from(quality) * 127 / 100) as u8;
	let dc_dequant = i32::from(DC_QUANT[usize::from(quantizer_index)]);
	let ac_dequant = i32::from(AC_QUANT[usize::from(quantizer_index)]);

	let mut modes = BoolWriter::new();
	write_frame_header(&mut modes, quantizer_index);
	let mut tokens = BoolWriter::new();
	let mut reconstructed_y = zeroed(source.y.len())?;
	let mut reconstructed_u = zeroed(source.u.len())?;
	let mut reconstructed_v = zeroed(source.v.len())?;
	let mut above_y = vec![[0u8; 4]; source.macroblock_columns];
	let mut above_u = vec![[0u8; 2]; source.macroblock_columns];
	let mut above_v = vec![[0u8; 2]; source.macroblock_columns];

	for macroblock_y in 0..source.macroblock_rows {
		let mut left_y = [0u8; 4];
		let mut left_u = [0u8; 2];
		let mut left_v = [0u8; 2];
		for macroblock_x in 0..source.macroblock_columns {
			let chroma_mode = choose_chroma_mode(&source, &reconstructed_u, &reconstructed_v, macroblock_x, macroblock_y, effort);
			write_prediction_modes(&mut modes, chroma_mode);
			encode_luma_macroblock(&mut tokens, &source.y, &mut reconstructed_y, source.y_stride, macroblock_x, macroblock_y, dc_dequant, ac_dequant, &mut above_y[macroblock_x], &mut left_y);
			encode_chroma_macroblock(&mut tokens, &source.u, &mut reconstructed_u, source.uv_stride, macroblock_x, macroblock_y, chroma_mode, dc_dequant.min(132), ac_dequant, &mut above_u[macroblock_x], &mut left_u);
			encode_chroma_macroblock(&mut tokens, &source.v, &mut reconstructed_v, source.uv_stride, macroblock_x, macroblock_y, chroma_mode, dc_dequant.min(132), ac_dequant, &mut above_v[macroblock_x], &mut left_v);
		}
	}

	let mode_partition = modes.finish();
	let token_partition = tokens.finish();
	if mode_partition.len() > 0x7ffff {
		return Err(Error::TooLarge);
	}
	let total = 10usize.checked_add(mode_partition.len()).and_then(|size| size.checked_add(token_partition.len())).ok_or(Error::TooLarge)?;
	let mut frame = Vec::new();
	frame.try_reserve_exact(total).map_err(|_| Error::TooLarge)?;
	let tag = (u32::try_from(mode_partition.len()).map_err(|_| Error::TooLarge)? << 5) | (1 << 4);
	frame.extend_from_slice(&tag.to_le_bytes()[..3]);
	frame.extend_from_slice(&[0x9d, 0x01, 0x2a]);
	frame.extend_from_slice(&(image.width as u16).to_le_bytes());
	frame.extend_from_slice(&(image.height as u16).to_le_bytes());
	frame.extend_from_slice(&mode_partition);
	frame.extend_from_slice(&token_partition);
	Ok(frame)
}

fn write_frame_header(output: &mut BoolWriter, quantizer_index: u8) {
	output.push(false, 128);
	output.push(false, 128);
	output.push(false, 128);
	output.push(false, 128);
	output.literal(0, 6);
	output.literal(0, 3);
	output.push(false, 128);
	output.literal(0, 2);
	output.literal(u32::from(quantizer_index), 7);
	for _ in 0..5 {
		output.push(false, 128);
	}
	output.push(false, 128);
	for block_type in COEFFICIENT_UPDATES {
		for band in block_type {
			for context in band {
				for probability in context {
					output.push(false, probability);
				}
			}
		}
	}
	output.push(false, 128);
}

fn write_prediction_modes(output: &mut BoolWriter, chroma_mode: i8) {
	output.symbol(&Y_MODE_TREE, &Y_MODE_PROBS, 4);
	for _ in 0..16 {
		output.symbol(&BLOCK_MODE_TREE, &BLOCK_DC_PROBS, 0);
	}
	output.symbol(&UV_MODE_TREE, &UV_MODE_PROBS, chroma_mode);
}

#[allow(clippy::too_many_arguments)]
fn encode_luma_macroblock(output: &mut BoolWriter, source: &[u8], reconstructed: &mut [u8], stride: usize, macroblock_x: usize, macroblock_y: usize, dc_dequant: i32, ac_dequant: i32, above_context: &mut [u8; 4], left_context: &mut [u8; 4]) {
	for block_y in 0..4usize {
		let mut left = left_context[block_y];
		for block_x in 0..4usize {
			let x = macroblock_x * 16 + block_x * 4;
			let y = macroblock_y * 16 + block_y * 4;
			let predictor = [luma_predictor(reconstructed, stride, x, y); 16];
			let nonzero = encode_block(output, 0, usize::from(above_context[block_x] + left), source, reconstructed, stride, x, y, predictor, dc_dequant, ac_dequant);
			let state = u8::from(nonzero);
			above_context[block_x] = state;
			left = state;
		}
		left_context[block_y] = left;
	}
}

#[allow(clippy::too_many_arguments)]
fn encode_chroma_macroblock(output: &mut BoolWriter, source: &[u8], reconstructed: &mut [u8], stride: usize, macroblock_x: usize, macroblock_y: usize, chroma_mode: i8, dc_dequant: i32, ac_dequant: i32, above_context: &mut [u8; 2], left_context: &mut [u8; 2]) {
	for block_y in 0..2usize {
		let mut left = left_context[block_y];
		for block_x in 0..2usize {
			let x = macroblock_x * 8 + block_x * 4;
			let y = macroblock_y * 8 + block_y * 4;
			let predictor = chroma_predictor(reconstructed, stride, macroblock_x, macroblock_y, block_x, block_y, chroma_mode);
			let nonzero = encode_block(output, 1, usize::from(above_context[block_x] + left), source, reconstructed, stride, x, y, predictor, dc_dequant, ac_dequant);
			let state = u8::from(nonzero);
			above_context[block_x] = state;
			left = state;
		}
		left_context[block_y] = left;
	}
}

#[allow(clippy::too_many_arguments)]
fn encode_block(output: &mut BoolWriter, plane: usize, context: usize, source: &[u8], reconstructed: &mut [u8], stride: usize, x: usize, y: usize, predictor: [i32; 16], dc_dequant: i32, ac_dequant: i32) -> bool {
	let mut coefficients = [0i32; 16];
	for row in 0..4usize {
		for column in 0..4usize {
			coefficients[row * 4 + column] = i32::from(source[(y + row) * stride + x + column]) - predictor[row * 4 + column];
		}
	}
	transform::forward(&mut coefficients);
	for (index, coefficient) in coefficients.iter_mut().enumerate() {
		*coefficient = round_div(*coefficient, if index == 0 { dc_dequant } else { ac_dequant }).clamp(-2048, 2048);
	}
	let nonzero = write_coefficients(output, plane, context, &coefficients);
	for (index, coefficient) in coefficients.iter_mut().enumerate() {
		*coefficient *= if index == 0 { dc_dequant } else { ac_dequant };
	}
	transform::inverse(&mut coefficients);
	for row in 0..4usize {
		for column in 0..4usize {
			reconstructed[(y + row) * stride + x + column] = (predictor[row * 4 + column] + coefficients[row * 4 + column]).clamp(0, 255) as u8;
		}
	}
	nonzero
}

fn write_coefficients(output: &mut BoolWriter, plane: usize, mut context: usize, coefficients: &[i32; 16]) -> bool {
	let last = ZIGZAG.iter().rposition(|index| coefficients[*index] != 0);
	let Some(last) = last else {
		output.symbol(&TOKEN_TREE, &COEFFICIENT_PROBS[plane][0][context], 11);
		return false;
	};
	let mut previous_was_zero = false;
	for position in 0..=last {
		let coefficient = coefficients[ZIGZAG[position]];
		let probabilities = &COEFFICIENT_PROBS[plane][COEFFICIENT_BANDS[position]][context];
		let token = coefficient_token(coefficient);
		output.symbol_from(&TOKEN_TREE, probabilities, token, if previous_was_zero { 2 } else { 0 });
		if coefficient == 0 {
			context = 0;
			previous_was_zero = true;
			continue;
		}
		write_coefficient_value(output, coefficient, token);
		context = if coefficient.unsigned_abs() == 1 { 1 } else { 2 };
		previous_was_zero = false;
	}
	if last < 15 {
		let position = last + 1;
		output.symbol(&TOKEN_TREE, &COEFFICIENT_PROBS[plane][COEFFICIENT_BANDS[position]][context], 11);
	}
	true
}

fn coefficient_token(coefficient: i32) -> i8 {
	match coefficient.unsigned_abs().min(2048) {
		0..=4 => coefficient.unsigned_abs() as i8,
		5..=6 => 5,
		7..=10 => 6,
		11..=18 => 7,
		19..=34 => 8,
		35..=66 => 9,
		_ => 10,
	}
}

fn write_coefficient_value(output: &mut BoolWriter, coefficient: i32, token: i8) {
	let magnitude = coefficient.unsigned_abs().min(2048) as u16;
	if token >= 5 {
		let category = usize::try_from(token - 5).unwrap();
		let extra = magnitude - CATEGORY_BASE[category];
		let active = CATEGORY_PROBS[category].iter().take_while(|probability| **probability != 0).count();
		for (index, probability) in CATEGORY_PROBS[category][..active].iter().enumerate() {
			output.push(extra & (1u16 << (active - index - 1)) != 0, *probability);
		}
	}
	output.push(coefficient < 0, 128);
}

fn luma_predictor(reconstructed: &[u8], stride: usize, x: usize, y: usize) -> i32 {
	let above = if y == 0 { 127 * 4 } else { reconstructed[(y - 1) * stride + x..][..4].iter().map(|value| i32::from(*value)).sum() };
	let left = if x == 0 { 129 * 4 } else { (0..4usize).map(|row| i32::from(reconstructed[(y + row) * stride + x - 1])).sum() };
	(above + left + 4) >> 3
}

fn choose_chroma_mode(source: &SourcePlanes, reconstructed_u: &[u8], reconstructed_v: &[u8], macroblock_x: usize, macroblock_y: usize, effort: u8) -> i8 {
	let candidate_count = match effort {
		0..=24 => 1,
		25..=49 => 2,
		50..=74 => 3,
		_ => 4,
	};
	let mut best = (u64::MAX, 0i8);
	for mode in 0..candidate_count {
		let mode = mode as i8;
		if mode == 1 && macroblock_y == 0 || mode == 2 && macroblock_x == 0 || mode == 3 && (macroblock_x == 0 || macroblock_y == 0) {
			continue;
		}
		let score = chroma_mode_error(&source.u, reconstructed_u, source.uv_stride, macroblock_x, macroblock_y, mode) + chroma_mode_error(&source.v, reconstructed_v, source.uv_stride, macroblock_x, macroblock_y, mode);
		if score < best.0 {
			best = (score, mode);
		}
	}
	best.1
}

fn chroma_mode_error(source: &[u8], reconstructed: &[u8], stride: usize, macroblock_x: usize, macroblock_y: usize, mode: i8) -> u64 {
	let mut error = 0u64;
	for block_y in 0..2usize {
		for block_x in 0..2usize {
			let predictor = chroma_predictor(reconstructed, stride, macroblock_x, macroblock_y, block_x, block_y, mode);
			let x = macroblock_x * 8 + block_x * 4;
			let y = macroblock_y * 8 + block_y * 4;
			for row in 0..4usize {
				for column in 0..4usize {
					let difference = i64::from(source[(y + row) * stride + x + column]) - i64::from(predictor[row * 4 + column]);
					error += difference.unsigned_abs().pow(2);
				}
			}
		}
	}
	error
}

fn chroma_predictor(reconstructed: &[u8], stride: usize, macroblock_x: usize, macroblock_y: usize, block_x: usize, block_y: usize, mode: i8) -> [i32; 16] {
	let x = macroblock_x * 8;
	let y = macroblock_y * 8;
	let dc = match (macroblock_y != 0, macroblock_x != 0) {
		(false, false) => 128,
		(true, false) => (reconstructed[(y - 1) * stride + x..][..8].iter().map(|value| i32::from(*value)).sum::<i32>() + 4) >> 3,
		(false, true) => ((0..8usize).map(|row| i32::from(reconstructed[(y + row) * stride + x - 1])).sum::<i32>() + 4) >> 3,
		(true, true) => {
			let above: i32 = reconstructed[(y - 1) * stride + x..][..8].iter().map(|value| i32::from(*value)).sum();
			let left: i32 = (0..8usize).map(|row| i32::from(reconstructed[(y + row) * stride + x - 1])).sum();
			(above + left + 8) >> 4
		}
	};
	let block_x = x + block_x * 4;
	let block_y = y + block_y * 4;
	let mut predictor = [dc; 16];
	for row in 0..4usize {
		for column in 0..4usize {
			predictor[row * 4 + column] = match mode {
				0 => dc,
				1 => i32::from(reconstructed[(y - 1) * stride + block_x + column]),
				2 => i32::from(reconstructed[(block_y + row) * stride + x - 1]),
				3 => (i32::from(reconstructed[(y - 1) * stride + block_x + column]) + i32::from(reconstructed[(block_y + row) * stride + x - 1]) - i32::from(reconstructed[(y - 1) * stride + x - 1])).clamp(0, 255),
				_ => unreachable!(),
			};
		}
	}
	predictor
}

fn source_planes(image: &pix::RgbaImage) -> Result<SourcePlanes, Error> {
	let macroblock_columns = usize::try_from(image.width.div_ceil(16)).map_err(|_| Error::TooLarge)?;
	let macroblock_rows = usize::try_from(image.height.div_ceil(16)).map_err(|_| Error::TooLarge)?;
	let y_stride = macroblock_columns.checked_mul(16).ok_or(Error::TooLarge)?;
	let y_height = macroblock_rows.checked_mul(16).ok_or(Error::TooLarge)?;
	let uv_stride = macroblock_columns.checked_mul(8).ok_or(Error::TooLarge)?;
	let uv_height = macroblock_rows.checked_mul(8).ok_or(Error::TooLarge)?;
	let mut y_plane = zeroed(y_stride.checked_mul(y_height).ok_or(Error::TooLarge)?)?;
	let mut u_plane = zeroed(uv_stride.checked_mul(uv_height).ok_or(Error::TooLarge)?)?;
	let mut v_plane = zeroed(u_plane.len())?;
	let width = usize::try_from(image.width).map_err(|_| Error::TooLarge)?;
	let height = usize::try_from(image.height).map_err(|_| Error::TooLarge)?;

	for y in 0..y_height {
		let source_y = y.min(height - 1);
		for x in 0..y_stride {
			let source_x = x.min(width - 1);
			let pixel = &image.pixels[(source_y * width + source_x) * 4..][..4];
			y_plane[y * y_stride + x] = rgb_to_y(pixel);
		}
	}
	for y in 0..uv_height {
		for x in 0..uv_stride {
			let mut red = 0i32;
			let mut green = 0i32;
			let mut blue = 0i32;
			for offset_y in 0..2usize {
				for offset_x in 0..2usize {
					let source_x = (x * 2 + offset_x).min(width - 1);
					let source_y = (y * 2 + offset_y).min(height - 1);
					let pixel = &image.pixels[(source_y * width + source_x) * 4..][..4];
					red += i32::from(pixel[0]);
					green += i32::from(pixel[1]);
					blue += i32::from(pixel[2]);
				}
			}
			u_plane[y * uv_stride + x] = ((-9719 * red - 19081 * green + 28800 * blue + (128 << 18) + (1 << 17)) >> 18).clamp(0, 255) as u8;
			v_plane[y * uv_stride + x] = ((28800 * red - 24116 * green - 4684 * blue + (128 << 18) + (1 << 17)) >> 18).clamp(0, 255) as u8;
		}
	}

	Ok(SourcePlanes { y: y_plane, u: u_plane, v: v_plane, y_stride, uv_stride, macroblock_columns, macroblock_rows })
}

fn rgb_to_y(pixel: &[u8]) -> u8 {
	((16839 * i32::from(pixel[0]) + 33059 * i32::from(pixel[1]) + 6420 * i32::from(pixel[2]) + (16 << 16) + (1 << 15)) >> 16).clamp(0, 255) as u8
}

fn round_div(value: i32, divisor: i32) -> i32 {
	if value >= 0 { (value + divisor / 2) / divisor } else { -((-value + divisor / 2) / divisor) }
}

fn zeroed(length: usize) -> Result<Vec<u8>, Error> {
	let mut bytes = Vec::new();
	bytes.try_reserve_exact(length).map_err(|_| Error::TooLarge)?;
	bytes.resize(length, 0);
	Ok(bytes)
}
