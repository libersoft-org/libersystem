// THE USB MONITOR CONTROL CLASS AND THE HID AMBIENT-LIGHT SENSOR OVER THE COMMON HID MACHINERY: which field of a
// monitor's reports is its brightness and which bytes are its EDID, which field of a sensor's is the illuminance, and
// what their bits say.
//
// NO MONITOR-SPECIFIC PARSER. The report descriptor is read by `hid::fields`, the table every HID class reads; what is
// here is the usages that table is searched for - the VESA Virtual Controls page's Brightness, the Monitor page's EDID
// Information, and the Sensors page's Illuminance under an Ambient Light collection - and what a value means. A usage
// this module does not name is left in the table exactly as the device declared it and interpreted nowhere.
//
// BRIGHTNESS IS A FEATURE REPORT, read by GET_REPORT and written by SET_REPORT, in the field's own logical range - which
// is the backlight's range: a monitor declaring 0..100 has a hundred and one levels. THE ILLUMINANCE IS AN INPUT REPORT,
// sent on the interrupt pipe as the light changes, in lux scaled by the field's unit exponent.

use crate::hid::{FieldInfo, FieldTable, ReportKind, read_field, write_field};
use alloc::vec::Vec;

pub const PAGE_SENSOR: u32 = 0x20;
pub const PAGE_MONITOR: u32 = 0x80;
pub const PAGE_VESA_VIRTUAL_CONTROLS: u32 = 0x82;

pub const EDID_INFORMATION: u32 = PAGE_MONITOR << 16 | 0x02;
pub const BRIGHTNESS: u32 = PAGE_VESA_VIRTUAL_CONTROLS << 16 | 0x10;
pub const AMBIENT_LIGHT: u32 = PAGE_SENSOR << 16 | 0x41;
pub const ILLUMINANCE: u32 = PAGE_SENSOR << 16 | 0x04d1;
// A SENSOR DATA FIELD MAY CARRY A MODIFIER in the top nibble of its usage id - the same field's maximum, its
// accuracy, its change sensitivity. Only the unmodified field is the reading.
const SENSOR_MODIFIER: u32 = 0xf000;
/// A SENSOR'S TWO PROPERTIES a host sets so it reports: its reporting state - All Events - and its power state - D0.
/// Each is a logical collection of selectors, the property's value the index of the one chosen.
pub const REPORTING_STATE: u32 = PAGE_SENSOR << 16 | 0x0316;
pub const POWER_STATE: u32 = PAGE_SENSOR << 16 | 0x0319;
pub const ALL_EVENTS: u32 = PAGE_SENSOR << 16 | 0x0841;
pub const FULL_POWER: u32 = PAGE_SENSOR << 16 | 0x0851;

/// The base block's first eight bytes, which every EDID starts with.
pub const EDID_HEADER: [u8; 8] = [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];

/// Where a monitor's brightness and EDID and a sensor's illuminance live, where the device has them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Map {
	/// The writable Brightness, preferring a Feature report, which GET_REPORT can also read back.
	pub brightness: Option<FieldInfo>,
	/// The EDID Information bytes of one Feature report, in order.
	pub edid: Vec<FieldInfo>,
	/// The illuminance reading, preferring an Input report, which the device sends as the light changes.
	pub illuminance: Option<FieldInfo>,
	/// The sensor's reporting-state and power-state properties, where it declares them.
	pub reporting_state: Option<FieldInfo>,
	pub power_state: Option<FieldInfo>,
}

impl Map {
	pub fn is_monitor(&self) -> bool {
		self.brightness.is_some()
	}

	pub fn is_light_sensor(&self) -> bool {
		self.illuminance.is_some()
	}
}

/// The map of a descriptor, or `None` for one that is neither a monitor with a brightness nor an ambient-light sensor.
pub fn map(table: &FieldTable) -> Option<Map> {
	let brightness = [ReportKind::Feature, ReportKind::Output].into_iter().find_map(|kind| table.fields.iter().find(|info| info.usage == BRIGHTNESS && info.kind == kind).copied());
	let mut edid: Vec<FieldInfo> = Vec::new();
	if let Some(first) = table.fields.iter().find(|info| info.usage == EDID_INFORMATION && info.kind == ReportKind::Feature && info.size == 8) {
		edid = table.fields.iter().filter(|info| info.usage == EDID_INFORMATION && info.kind == ReportKind::Feature && info.size == 8 && info.report_id == first.report_id).copied().collect();
		edid.sort_by_key(|info| info.bit_offset);
	}
	let reading = |info: &&FieldInfo| info.usage & !SENSOR_MODIFIER == ILLUMINANCE && info.usage & SENSOR_MODIFIER == 0 && info.application >> 16 == PAGE_SENSOR;
	let illuminance = [ReportKind::Input, ReportKind::Feature].into_iter().find_map(|kind| table.fields.iter().filter(reading).find(|info| info.kind == kind).copied());
	if brightness.is_none() && illuminance.is_none() {
		return None;
	}
	let property = |usage: u32| table.fields.iter().find(|info| info.kind == ReportKind::Feature && info.array && info.collection == usage).copied();
	Some(Map { brightness, edid, illuminance, reporting_state: property(REPORTING_STATE), power_state: property(POWER_STATE) })
}

/// A field's raw bits as its logical value - sign-extended where its logical range goes below zero.
pub fn logical(info: &FieldInfo, raw: u32) -> i64 {
	if info.logical_min < 0 && info.size > 0 && info.size < 32 && raw >> (info.size - 1) & 1 != 0 {
		return i64::from(raw) - (1i64 << info.size);
	}
	if info.logical_min < 0 && info.size == 32 {
		return i64::from(raw as i32);
	}
	i64::from(raw)
}

/// The brightness's range: its logical minimum and maximum, neither below zero.
pub fn range(info: &FieldInfo) -> (u32, u32) {
	(info.logical_min.max(0) as u32, info.logical_max.max(0) as u32)
}

/// The brightness a report body holds, or `None` when it is outside the field's range - which a field with a null
/// state means as "not known".
pub fn level(info: &FieldInfo, body: &[u8]) -> Option<u32> {
	if (info.bit_offset + info.size).div_ceil(8) as usize > body.len() {
		return None;
	}
	let value = logical(info, read_field(body, info));
	(value >= i64::from(info.logical_min) && value <= i64::from(info.logical_max) && value >= 0).then_some(value as u32)
}

/// Write a brightness into a report body read a moment before. False when the level is outside the field's range.
pub fn write_level(body: &mut [u8], info: &FieldInfo, level: u32) -> bool {
	let level = i64::from(level);
	if level < i64::from(info.logical_min) || level > i64::from(info.logical_max) {
		return false;
	}
	let mask: u64 = if info.size >= 32 { u64::from(u32::MAX) } else { (1u64 << info.size) - 1 };
	write_field(body, info, (level as u64 & mask) as u32)
}

/// The illuminance a report body holds, in thousandths of a lux: the logical value scaled by the field's unit
/// exponent. `None` when the field is not in the body or holds its null state; a negative reading is no light.
pub fn milli_lux(info: &FieldInfo, body: &[u8]) -> Option<u64> {
	if (info.bit_offset + info.size).div_ceil(8) as usize > body.len() {
		return None;
	}
	let value = logical(info, read_field(body, info));
	if info.null_state && (value < i64::from(info.logical_min) || value > i64::from(info.logical_max)) {
		return None;
	}
	let mut milli = i128::from(value.max(0)) * 1000;
	let exponent = i32::from(info.unit_exponent);
	if exponent >= 0 {
		milli = milli.saturating_mul(10i128.saturating_pow(exponent as u32));
	} else {
		milli /= 10i128.saturating_pow((-exponent) as u32);
	}
	Some(milli.clamp(0, i128::from(u64::MAX)) as u64)
}

/// THE VALUE THAT CHOOSES `wanted` among a property's selectors: the field's logical minimum plus the selector's place
/// after the first, where that is inside the field's range.
pub fn selector_value(info: &FieldInfo, wanted: u32) -> Option<u32> {
	let place = i64::from(wanted.checked_sub(info.usage)?);
	let value = i64::from(info.logical_min) + place;
	(value >= i64::from(info.logical_min) && value <= i64::from(info.logical_max) && value >= 0).then_some(value as u32)
}

/// The EDID bytes a report body holds, as many of them as it has.
pub fn edid_bytes(map: &Map, body: &[u8]) -> Vec<u8> {
	map.edid.iter().take_while(|info| (info.bit_offset + info.size).div_ceil(8) as usize <= body.len()).map(|info| read_field(body, info) as u8).collect()
}

/// A MONITOR'S EDID IDENTITY: the base block's manufacturer id, product code and serial number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
	pub manufacturer: u16,
	pub product: u16,
	pub serial: u32,
}

/// The identity of an EDID base block, or `None` when the bytes are not one: too short, or without the header.
pub fn edid_identity(bytes: &[u8]) -> Option<Identity> {
	if bytes.len() < 16 || bytes[..8] != EDID_HEADER {
		return None;
	}
	Some(Identity { manufacturer: u16::from_be_bytes([bytes[8], bytes[9]]), product: u16::from_le_bytes([bytes[10], bytes[11]]), serial: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) })
}

#[cfg(test)]
mod tests;
