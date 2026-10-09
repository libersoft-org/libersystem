use super::*;

#[test]
fn bounded_transfer_indices_preserve_the_original_interpolation_bits() {
	fn reference(table: &TransferTable, value: f32, encode: bool) -> f32 {
		if !(value.is_finite() && (0.0..=1.0).contains(&value)) {
			return if encode { color::encode(table.transfer, value as f64) as f32 } else { color::decode(table.transfer, value as f64) as f32 };
		}
		let (entries, values, position) = if encode { (ENCODE_ENTRIES, table.encode.as_slice(), crate::composite::sqrt_inline(value) * ENCODE_ENTRIES as f32) } else { (DECODE_ENTRIES, table.decode.as_slice(), value * DECODE_ENTRIES as f32) };
		let index = position as usize;
		let fraction = position - index as f32;
		let low = values[index.min(entries)];
		let high = values[(index + 1).min(entries)];
		low + (high - low) * fraction
	}
	for transfer in [
		graphics_profile::image::Transfer::Linear,
		graphics_profile::image::Transfer::Srgb,
		graphics_profile::image::Transfer::Pq,
		graphics_profile::image::Transfer::Hlg,
	] {
		let table = TransferTable::new(transfer);
		let check = |value: f32| {
			assert_eq!(table.decode(value).to_bits(), reference(&table, value, false).to_bits(), "decode {transfer:?} {value:?}");
			assert_eq!(table.encode(value).to_bits(), reference(&table, value, true).to_bits(), "encode {transfer:?} {value:?}");
		};
		// Every half input includes signed zero, subnormals, extended values and NaNs;
		// adjacent f32 values around all table boundaries cover truncation endpoints.
		for bits in 0..=u16::MAX {
			check(half_to_f32(bits));
		}
		for index in 0..=ENCODE_ENTRIES {
			let linear = index as f32 / ENCODE_ENTRIES as f32;
			for value in [linear, linear * linear] {
				check(value.next_down());
				check(value);
				check(value.next_up());
			}
		}
	}
}
