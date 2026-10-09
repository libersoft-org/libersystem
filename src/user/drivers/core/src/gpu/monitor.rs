//! Monitor discovery from the bound virtio-gpu's bounded GET_EDID response.
//! The EDID reader owns parsing; this adapter supplies the existing typed display-device wire.

use alloc::vec::Vec;
use display_device_proto::generated::liber::display_device::v1 as wire;
use edid::{Descriptor, Edid, Mode, PhysicalSize};

pub const RESPONSE_BYTES: usize = 24 + 8 + 1024;
const RESP_OK_EDID: u32 = 0x1104;

/// Only completed bytes may be read. A device's declared size cannot make untouched DMA bytes
/// into a valid block, even when those bytes still contain a previous response.
pub fn response(bytes: &[u8], used: u32) -> Option<wire::MonitorDescription> {
	let completed = bytes.get(..usize::try_from(used).ok()?)?;
	if completed.len() < 32 || completed.len() > RESPONSE_BYTES || u32::from_le_bytes(completed[..4].try_into().ok()?) != RESP_OK_EDID {
		return None;
	}
	let size = u32::from_le_bytes(completed[24..28].try_into().ok()?) as usize;
	if !(edid::BLOCK_LEN..=1024).contains(&size) || !size.is_multiple_of(edid::BLOCK_LEN) {
		return None;
	}
	describe(completed.get(32..32 + size)?)
}

fn describe(bytes: &[u8]) -> Option<wire::MonitorDescription> {
	let parsed = Edid::parse(bytes).ok()?;
	for index in 0..parsed.extension_count() {
		parsed.extension(index)?;
	}
	let mut description = wire::MonitorDescription {
		identity: wire::MonitorIdentity { manufacturer: u16::from_be_bytes([bytes[8], bytes[9]]), product: parsed.product_code(), serial: parsed.serial_number() },
		physical_size_mm: match parsed.physical_size() {
			PhysicalSize::Centimetres { width, height } => Some(wire::Extent2d { width: u32::from(width) * 10, height: u32::from(height) * 10 }),
			_ => None,
		},
		modes: Vec::new(),
		preferred: None,
	};
	for index in 0..4 {
		if let Some(Descriptor::Timing(timing)) = parsed.descriptor(index) {
			let Some(refresh_millihertz) = timing.refresh_millihertz() else { continue };
			let mode = wire::MonitorMode { size: wire::Extent2d { width: timing.horizontal_active, height: timing.vertical_active }, refresh_millihertz, interlaced: timing.interlaced };
			let position = add_mode(&mut description.modes, mode);
			if index == 0 && parsed.preferred_timing().is_some() {
				description.preferred = Some(position);
				// The preferred timing's image size has millimetre precision; the base block is cm.
				if let Some((width, height)) = timing.image_size_mm {
					description.physical_size_mm = Some(wire::Extent2d { width, height });
				}
			}
		}
	}
	let mut modes = [Mode { width: 0, height: 0, refresh_hz: 0 }; 17];
	let established = parsed.established_timings(&mut modes);
	for mode in &modes[..established] {
		add_mode(&mut description.modes, wire_mode(*mode, mode.width == 1024 && mode.height == 768 && mode.refresh_hz == 87));
	}
	let standard = parsed.standard_timings(&mut modes);
	for mode in &modes[..standard] {
		add_mode(&mut description.modes, wire_mode(*mode, false));
	}
	Some(description)
}

fn wire_mode(mode: Mode, interlaced: bool) -> wire::MonitorMode {
	wire::MonitorMode { size: wire::Extent2d { width: mode.width, height: mode.height }, refresh_millihertz: mode.refresh_hz * 1000, interlaced }
}

fn add_mode(modes: &mut Vec<wire::MonitorMode>, mode: wire::MonitorMode) -> u32 {
	if let Some(index) = modes.iter().position(|existing| *existing == mode) {
		return index as u32;
	}
	let index = modes.len() as u32;
	modes.push(mode);
	index
}

#[cfg(test)]
mod tests {
	use super::*;

	fn checksum(block: &mut [u8]) {
		block[127] = 0u8.wrapping_sub(block[..127].iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)));
	}

	fn fixture() -> [u8; RESPONSE_BYTES] {
		let mut response = [0u8; RESPONSE_BYTES];
		response[..4].copy_from_slice(&RESP_OK_EDID.to_le_bytes());
		response[24..28].copy_from_slice(&128u32.to_le_bytes());
		let b = &mut response[32..160];
		b[..8].copy_from_slice(&[0, 255, 255, 255, 255, 255, 255, 0]);
		b[8..10].copy_from_slice(&0x4a14u16.to_be_bytes());
		b[10..12].copy_from_slice(&0x1234u16.to_le_bytes());
		b[12..16].copy_from_slice(&123456u32.to_le_bytes());
		b[18] = 1;
		b[19] = 4;
		b[21] = 52;
		b[22] = 29;
		b[24] = 2;
		b[35] = 0x21; // 640x480@60 and 800x600@60.
		b[36] = 0x10; // 1024x768@87 interlaced.
		b[38..54].fill(1);
		b[38] = 69; // 800x600@60, already in established timings.
		b[39] = 0x40;
		// 1920x1080@60, 148.5 MHz, 280x45 blanking, 88/44 and 4/5 sync; 509x286 mm.
		b[54..72].copy_from_slice(&[0x02, 0x3a, 0x80, 0x18, 0x71, 0x38, 0x2d, 0x40, 88, 44, 0x45, 0, 0xfd, 0x1e, 0x11, 0, 0, 0x1e]);
		checksum(b);
		response
	}

	#[test]
	fn publishes_identity_preferred_mode_mm_size_and_deduplicated_timings() {
		let description = response(&fixture(), RESPONSE_BYTES as u32).unwrap();
		assert_eq!(description.identity, wire::MonitorIdentity { manufacturer: 0x4a14, product: 0x1234, serial: 123456 });
		assert_eq!(description.physical_size_mm, Some(wire::Extent2d { width: 509, height: 286 }));
		assert_eq!(description.preferred, Some(0));
		assert_eq!(description.modes.len(), 4);
		assert_eq!(description.modes[0], wire::MonitorMode { size: wire::Extent2d { width: 1920, height: 1080 }, refresh_millihertz: 60_000, interlaced: false });
		assert!(description.modes[3].interlaced);
	}

	#[test]
	fn refuses_short_completed_payload_even_when_old_dma_bytes_look_valid() {
		let bytes = fixture();
		for used in [0, 23, 24, 31, 32, 159, RESPONSE_BYTES as u32 + 1, u32::MAX] {
			assert!(response(&bytes, used).is_none(), "used={used}");
		}
		assert!(response(&bytes, 160).is_some());
	}

	#[test]
	fn refuses_device_error_and_invalid_declared_size() {
		let mut bytes = fixture();
		bytes[0] ^= 1;
		assert!(response(&bytes, RESPONSE_BYTES as u32).is_none());
		bytes[0] ^= 1;
		for size in [0u32, 127, 129, 1025, u32::MAX] {
			bytes[24..28].copy_from_slice(&size.to_le_bytes());
			assert!(response(&bytes, RESPONSE_BYTES as u32).is_none());
		}
	}

	#[test]
	fn refuses_corrupt_missing_or_excessive_extensions() {
		let mut bytes = fixture();
		bytes[32 + 126] = 1;
		checksum(&mut bytes[32..160]);
		assert!(response(&bytes, RESPONSE_BYTES as u32).is_none());
		bytes[24..28].copy_from_slice(&256u32.to_le_bytes());
		bytes[160] = 2;
		assert!(response(&bytes, RESPONSE_BYTES as u32).is_none());
		checksum(&mut bytes[160..288]);
		assert!(response(&bytes, RESPONSE_BYTES as u32).is_some());
		bytes[32 + 126] = 5;
		checksum(&mut bytes[32..160]);
		assert!(response(&bytes, RESPONSE_BYTES as u32).is_none());
	}

	#[test]
	fn malformed_timing_and_aspect_ratio_never_become_mode_or_physical_size() {
		let mut bytes = fixture();
		let b = &mut bytes[32..160];
		b[58] &= 0x0f; // Active horizontal width becomes 128, sync no longer fits after next edit.
		b[57] = 1;
		b[58] &= 0xf0; // Horizontal blanking is one pixel; 88+44 cannot fit.
		b[22] = 0;
		checksum(b);
		let description = response(&bytes, RESPONSE_BYTES as u32).unwrap();
		assert_eq!(description.physical_size_mm, None);
		assert_eq!(description.preferred, None);
		assert_eq!(description.modes.len(), 3);
	}

	#[test]
	fn absent_preference_uses_base_cm_and_base_checksum_is_required() {
		let mut bytes = fixture();
		bytes[32 + 24] = 0;
		checksum(&mut bytes[32..160]);
		let description = response(&bytes, RESPONSE_BYTES as u32).unwrap();
		assert_eq!(description.preferred, None);
		assert_eq!(description.physical_size_mm, Some(wire::Extent2d { width: 520, height: 290 }));
		bytes[40] ^= 1;
		assert!(response(&bytes, RESPONSE_BYTES as u32).is_none());
	}

	#[test]
	fn metadata_wire_carries_the_maximum_snapshot_and_refuses_an_extra_mode() {
		let mut description = response(&fixture(), RESPONSE_BYTES as u32).unwrap();
		let mode = description.modes[0].clone();
		description.modes.resize(29, mode.clone());
		let mut frame = [0u8; 1024];
		let used = description.encode(&mut frame).unwrap();
		assert_eq!(wire::MonitorDescription::decode(&frame[..used]), Some(description.clone()));
		description.modes.push(mode);
		// LSIDL bounds apply at the receiving boundary. The generic writer lets a hostile fixture
		// encode this over-limit frame; a DisplayService reader must refuse it before allocating.
		let used = description.encode(&mut frame).unwrap();
		assert!(wire::MonitorDescription::decode(&frame[..used]).is_none());
	}
}
