// THE HID POWER DEVICE CLASS OVER THE COMMON HID MACHINERY (USB HID Power Devices 1.0): which fields of a UPS's
// reports carry which of the values `power_model::hid::Ups` holds, what the bits of the latest reports say, and
// how a control is written back.
//
// NO UPS-SPECIFIC PARSER. The report descriptor is read by `hid::fields`, the table every HID class reads; what
// is here is the Power Device and Battery System usages that table is searched for, and the preference between
// two fields that both carry one - a UPS reports a voltage for its input, its output and its battery, and the one
// the canonical record means is the battery's, or the power summary's that stands for it. A usage this module does
// not name is left in the table exactly as the device declared it and interpreted nowhere.
//
// A CONTROL IS A FIELD THE HOST MAY WRITE. DelayBeforeShutdown in a Feature or Output report schedules a turn-off
// and, set to -1, cancels one; SwitchOn/Off in an Outlet collection switches that outlet. Each is advertised only
// when the descriptor has it, writable, and a command for one it does not have is refused before anything is sent.

use crate::hid::{FieldInfo, FieldTable, ReportKind, read_field, write_field};
use alloc::vec::Vec;
use power_model::convert::Field;
use power_model::hid::{Status, Ups, Value};

pub const PAGE_POWER_DEVICE: u32 = 0x84;
pub const PAGE_BATTERY_SYSTEM: u32 = 0x85;

const fn power(usage: u32) -> u32 {
	PAGE_POWER_DEVICE << 16 | usage
}

const fn battery(usage: u32) -> u32 {
	PAGE_BATTERY_SYSTEM << 16 | usage
}

// The collections a value is preferred from, in order.
const POWER_SUMMARY: u32 = power(0x24);
const BATTERY: u32 = power(0x12);
const OUTPUT: u32 = power(0x1c);
const OUTLET: u32 = power(0x20);

// The values.
const VOLTAGE: u32 = power(0x30);
const CURRENT: u32 = power(0x31);
const ACTIVE_POWER: u32 = power(0x34);
const PERCENT_LOAD: u32 = power(0x35);
const TEMPERATURE: u32 = power(0x36);
const DELAY_BEFORE_SHUTDOWN: u32 = power(0x57);
const INTERNAL_FAILURE: u32 = power(0x62);
const OVERLOAD: u32 = power(0x65);
const OVER_TEMPERATURE: u32 = power(0x67);
const SWITCH_ON_OFF: u32 = power(0x6b);
const REMAINING_CAPACITY_LIMIT: u32 = battery(0x29);
const CAPACITY_MODE: u32 = battery(0x2c);
const BELOW_REMAINING_CAPACITY_LIMIT: u32 = battery(0x42);
const CHARGING: u32 = battery(0x44);
const DISCHARGING: u32 = battery(0x45);
const NEED_REPLACEMENT: u32 = battery(0x4b);
const RELATIVE_STATE_OF_CHARGE: u32 = battery(0x64);
const REMAINING_CAPACITY: u32 = battery(0x66);
const FULL_CHARGE_CAPACITY: u32 = battery(0x67);
const RUN_TIME_TO_EMPTY: u32 = battery(0x68);
const DESIGN_CAPACITY: u32 = battery(0x83);
const AC_PRESENT: u32 = battery(0xd0);

/// The most outlets a device's controls are counted for.
pub const MAX_OUTLETS: usize = 8;

/// Where each value the model reads lives, when the device has it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Map {
	pub capacity_mode: Option<FieldInfo>,
	pub remaining_capacity: Option<FieldInfo>,
	pub full_charge_capacity: Option<FieldInfo>,
	pub design_capacity: Option<FieldInfo>,
	pub remaining_capacity_limit: Option<FieldInfo>,
	pub relative_state_of_charge: Option<FieldInfo>,
	pub run_time_to_empty: Option<FieldInfo>,
	pub voltage: Option<FieldInfo>,
	pub current: Option<FieldInfo>,
	pub active_power: Option<FieldInfo>,
	pub percent_load: Option<FieldInfo>,
	pub temperature: Option<FieldInfo>,
	pub ac_present: Option<FieldInfo>,
	pub charging: Option<FieldInfo>,
	pub discharging: Option<FieldInfo>,
	pub below_remaining_capacity_limit: Option<FieldInfo>,
	pub need_replacement: Option<FieldInfo>,
	pub overload: Option<FieldInfo>,
	pub internal_failure: Option<FieldInfo>,
	pub over_temperature: Option<FieldInfo>,
	/// The writable DelayBeforeShutdown, when there is one.
	pub delay_before_shutdown: Option<FieldInfo>,
	/// Each switchable outlet's writable SwitchOn/Off, in the order the outlets are declared.
	pub outlets: Vec<FieldInfo>,
}

impl Map {
	/// Every report this map reads a value from, each once.
	pub fn reports(&self) -> Vec<(ReportKind, u8)> {
		let mut out: Vec<(ReportKind, u8)> = Vec::new();
		let all = [
			&self.capacity_mode,
			&self.remaining_capacity,
			&self.full_charge_capacity,
			&self.design_capacity,
			&self.remaining_capacity_limit,
			&self.relative_state_of_charge,
			&self.run_time_to_empty,
			&self.voltage,
			&self.current,
			&self.active_power,
			&self.percent_load,
			&self.temperature,
			&self.ac_present,
			&self.charging,
			&self.discharging,
			&self.below_remaining_capacity_limit,
			&self.need_replacement,
			&self.overload,
			&self.internal_failure,
			&self.over_temperature,
		];
		for info in all.into_iter().flatten() {
			if !out.contains(&(info.kind, info.report_id)) {
				out.push((info.kind, info.report_id));
			}
		}
		out
	}
}

fn writable(info: &FieldInfo) -> bool {
	info.kind != ReportKind::Input
}

// The field carrying `usage`, preferring the collections in `from` in their order, and a readable report (Input
// first, because it is the one the device sends when it changes) over a Feature.
fn pick(table: &FieldTable, usage: u32, from: &[u32]) -> Option<FieldInfo> {
	let candidates = || table.fields.iter().filter(move |info| info.usage == usage && info.kind != ReportKind::Output);
	for &collection in from {
		for kind in [ReportKind::Input, ReportKind::Feature] {
			if let Some(found) = candidates().find(|info| info.collection == collection && info.kind == kind) {
				return Some(*found);
			}
		}
	}
	for kind in [ReportKind::Input, ReportKind::Feature] {
		if let Some(found) = candidates().find(|info| info.kind == kind) {
			return Some(*found);
		}
	}
	None
}

/// Whether a descriptor describes a power device: a field in an application collection on the Power Device or
/// Battery System page.
pub fn is_power_device(table: &FieldTable) -> bool {
	table.fields.iter().any(|info| matches!(info.application >> 16, PAGE_POWER_DEVICE | PAGE_BATTERY_SYSTEM))
}

/// The map of a power device's descriptor, or `None` for one that is not a power device or carries none of the
/// values the model reads.
pub fn map(table: &FieldTable) -> Option<Map> {
	if !is_power_device(table) {
		return None;
	}
	let summary = [POWER_SUMMARY, BATTERY];
	let output = [OUTPUT, POWER_SUMMARY];
	let mut map = Map { capacity_mode: pick(table, CAPACITY_MODE, &summary), remaining_capacity: pick(table, REMAINING_CAPACITY, &summary), full_charge_capacity: pick(table, FULL_CHARGE_CAPACITY, &summary), design_capacity: pick(table, DESIGN_CAPACITY, &summary), remaining_capacity_limit: pick(table, REMAINING_CAPACITY_LIMIT, &summary), relative_state_of_charge: pick(table, RELATIVE_STATE_OF_CHARGE, &summary), run_time_to_empty: pick(table, RUN_TIME_TO_EMPTY, &summary), voltage: pick(table, VOLTAGE, &summary), current: pick(table, CURRENT, &summary), active_power: pick(table, ACTIVE_POWER, &output), percent_load: pick(table, PERCENT_LOAD, &output), temperature: pick(table, TEMPERATURE, &summary), ac_present: pick(table, AC_PRESENT, &summary), charging: pick(table, CHARGING, &summary), discharging: pick(table, DISCHARGING, &summary), below_remaining_capacity_limit: pick(table, BELOW_REMAINING_CAPACITY_LIMIT, &summary), need_replacement: pick(table, NEED_REPLACEMENT, &summary), overload: pick(table, OVERLOAD, &output), internal_failure: pick(table, INTERNAL_FAILURE, &summary), over_temperature: pick(table, OVER_TEMPERATURE, &summary), delay_before_shutdown: table.fields.iter().find(|info| info.usage == DELAY_BEFORE_SHUTDOWN && writable(info)).copied(), outlets: Vec::new() };
	for info in table.fields.iter().filter(|info| info.usage == SWITCH_ON_OFF && info.collection == OUTLET && writable(info)) {
		if map.outlets.len() < MAX_OUTLETS && !map.outlets.iter().any(|known| known.occurrence == info.occurrence) {
			map.outlets.push(*info);
		}
	}
	// A DEVICE WITH ONLY CONTROLS is still one: an outlet strip reports nothing and switches.
	if map.reports().is_empty() && map.outlets.is_empty() && map.delay_before_shutdown.is_none() {
		return None;
	}
	Some(map)
}

/// The latest body of one report, as the transport last read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
	pub kind: ReportKind,
	pub report_id: u8,
	pub body: Vec<u8>,
}

fn value(info: &Option<FieldInfo>, reports: &[Report]) -> Option<Value> {
	let info = info.as_ref()?;
	let report = reports.iter().find(|report| report.kind == info.kind && report.report_id == info.report_id)?;
	if (info.bit_offset + info.size).div_ceil(8) as usize > report.body.len() {
		return None;
	}
	Some(Value { raw: read_field(&report.body, info), field: Field { bits: info.size as u8, logical_min: info.logical_min, logical_max: info.logical_max, null_state: info.null_state, unit: info.unit, exponent: info.unit_exponent } })
}

// A flag: a one-bit field (or any field) that is set.
fn flag(info: &Option<FieldInfo>, reports: &[Report]) -> Option<bool> {
	value(info, reports).map(|value| value.raw != 0)
}

/// What the latest reports say, as the model's decoded UPS. A value whose report has not been read yet is absent,
/// exactly as a usage the device does not have is.
pub fn decode(map: &Map, reports: &[Report]) -> Ups {
	Ups { capacity_mode: value(&map.capacity_mode, reports).map(|value| value.raw), remaining_capacity: value(&map.remaining_capacity, reports), full_charge_capacity: value(&map.full_charge_capacity, reports), design_capacity: value(&map.design_capacity, reports), remaining_capacity_limit: value(&map.remaining_capacity_limit, reports), relative_state_of_charge: value(&map.relative_state_of_charge, reports), run_time_to_empty: value(&map.run_time_to_empty, reports), voltage: value(&map.voltage, reports), current: value(&map.current, reports), active_power: value(&map.active_power, reports), percent_load: value(&map.percent_load, reports), temperature: value(&map.temperature, reports), status: Status { ac_present: flag(&map.ac_present, reports), charging: flag(&map.charging, reports), discharging: flag(&map.discharging, reports), below_remaining_capacity_limit: flag(&map.below_remaining_capacity_limit, reports), need_replacement: flag(&map.need_replacement, reports), overload: flag(&map.overload, reports), internal_failure: flag(&map.internal_failure, reports), over_temperature: flag(&map.over_temperature, reports) }, switchable_outlets: map.outlets.len() as u8, delay_before_shutdown: map.delay_before_shutdown.is_some(), source_time: None }
}

/// A logical value as the raw bits of a field, or `None` when it is outside the field's logical range - a
/// control is never written as a value its field cannot hold.
pub fn raw_for(info: &FieldInfo, logical: i64) -> Option<u32> {
	if logical < info.logical_min as i64 || logical > info.logical_max as i64 {
		return None;
	}
	let mask: u64 = if info.size >= 32 { u32::MAX as u64 } else { (1u64 << info.size) - 1 };
	Some((logical as u64 & mask) as u32)
}

/// Write one control's logical value into a report body read a moment before, leaving every other field as the
/// device last said it was.
pub fn write_control(body: &mut [u8], info: &FieldInfo, logical: i64) -> bool {
	match raw_for(info, logical) {
		Some(raw) => write_field(body, info, raw),
		None => false,
	}
}

/// A HID interface a power device may be: its setting, its interrupt IN endpoint, and how long its report
/// descriptor says it is. Whether it IS a power device is the descriptor's to say, which is read over the bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interface {
	pub config_value: u8,
	pub interface: u8,
	pub alternate: u8,
	pub interrupt_in: crate::usb_function::Endpoint,
	pub report_descriptor_length: u16,
}

/// The HID class, and the HID descriptor that names the report descriptor's length.
pub const CLASS_HID: u8 = 0x03;
pub const DT_HID: u8 = 0x21;
pub const DT_REPORT: u8 = 0x22;
/// GET_REPORT and SET_REPORT, class requests to the interface.
pub const REQ_GET_REPORT: u8 = 0x01;
pub const REQ_SET_REPORT: u8 = 0x09;

/// Every HID interface of one configuration with an interrupt IN endpoint and a report descriptor.
pub fn interfaces(config: &[u8]) -> Vec<Interface> {
	let Ok(parsed) = crate::usb_function::Configuration::parse(config) else { return Vec::new() };
	let mut out = Vec::new();
	for setting in parsed.settings.iter().filter(|setting| setting.class == CLASS_HID) {
		let Some(interrupt_in) = setting.first(crate::usb_function::Endpoint::is_interrupt_in) else { continue };
		let Some(length) = parsed.functional(setting).find(|record| record.kind == DT_HID).and_then(|record| record.field16(7).ok()) else { continue };
		if length == 0 {
			continue;
		}
		out.push(Interface { config_value: parsed.value, interface: setting.interface, alternate: setting.alternate, interrupt_in, report_descriptor_length: length });
	}
	out
}

#[cfg(test)]
mod tests;
