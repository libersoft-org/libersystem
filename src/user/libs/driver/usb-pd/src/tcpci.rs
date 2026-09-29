//! A TYPE-C PORT CONTROLLER'S REGISTERS (TCPCI, revision 2.0), as the `tcpci` driver reaches them over its one I2C
//! address: the map, the alert bits, the commands, and the receive and transmit buffers' layouts.

use alloc::vec::Vec;

use crate::engine::{Cc, Rp};

pub const VENDOR_ID: u8 = 0x00;
pub const PRODUCT_ID: u8 = 0x02;
pub const BCD_DEVICE: u8 = 0x04;
pub const TC_REVISION: u8 = 0x06;
pub const PD_REVISION: u8 = 0x08;
pub const ALERT: u8 = 0x10;
pub const ALERT_MASK: u8 = 0x12;
pub const POWER_STATUS_MASK: u8 = 0x14;
pub const FAULT_STATUS_MASK: u8 = 0x15;
pub const TCPC_CONTROL: u8 = 0x19;
pub const ROLE_CONTROL: u8 = 0x1A;
pub const FAULT_CONTROL: u8 = 0x1B;
pub const POWER_CONTROL: u8 = 0x1C;
pub const CC_STATUS: u8 = 0x1D;
pub const POWER_STATUS: u8 = 0x1E;
pub const FAULT_STATUS: u8 = 0x1F;
pub const COMMAND: u8 = 0x23;
pub const DEVICE_CAPABILITIES_1: u8 = 0x24;
pub const MESSAGE_HEADER_INFO: u8 = 0x2E;
pub const RECEIVE_DETECT: u8 = 0x2F;
pub const RECEIVE_BUFFER: u8 = 0x30;
pub const TRANSMIT: u8 = 0x50;
pub const TRANSMIT_BUFFER: u8 = 0x51;
pub const VBUS_VOLTAGE: u8 = 0x70;
pub const VBUS_VOLTAGE_ALARM_HI: u8 = 0x76;
pub const VBUS_VOLTAGE_ALARM_LO: u8 = 0x78;

// ALERT.
pub const ALERT_CC_STATUS: u16 = 1 << 0;
pub const ALERT_POWER_STATUS: u16 = 1 << 1;
pub const ALERT_RX_STATUS: u16 = 1 << 2;
pub const ALERT_RX_HARD_RESET: u16 = 1 << 3;
pub const ALERT_TX_FAILED: u16 = 1 << 4;
pub const ALERT_TX_DISCARDED: u16 = 1 << 5;
pub const ALERT_TX_SUCCESS: u16 = 1 << 6;
pub const ALERT_VBUS_ALARM_HI: u16 = 1 << 7;
pub const ALERT_VBUS_ALARM_LO: u16 = 1 << 8;
pub const ALERT_FAULT: u16 = 1 << 9;
pub const ALERT_RX_OVERFLOW: u16 = 1 << 10;
/// Every alert the engine takes.
pub const ALERT_ALL: u16 = ALERT_CC_STATUS | ALERT_POWER_STATUS | ALERT_RX_STATUS | ALERT_RX_HARD_RESET | ALERT_TX_FAILED | ALERT_TX_DISCARDED | ALERT_TX_SUCCESS | ALERT_VBUS_ALARM_HI | ALERT_VBUS_ALARM_LO | ALERT_FAULT | ALERT_RX_OVERFLOW;

// POWER_STATUS.
pub const POWER_SINKING_VBUS: u8 = 1 << 0;
pub const POWER_VBUS_PRESENT: u8 = 1 << 2;
pub const POWER_UNINITIALIZED: u8 = 1 << 6;

// POWER_CONTROL.
pub const POWER_AUTO_DISCHARGE: u8 = 1 << 4;
pub const POWER_DISABLE_ALARMS: u8 = 1 << 5;
pub const POWER_DISABLE_MONITORING: u8 = 1 << 6;

// DEVICE_CAPABILITIES_1: the controller switches the sink path itself, and it measures VBUS and raises alarms on it.
pub const CAPABILITY_SINK_VBUS: u16 = 1 << 2;
pub const CAPABILITY_VBUS_MEASUREMENT: u16 = 1 << 10;

// TCPC_CONTROL: the plug's orientation - CC2 carries the messages.
pub const CONTROL_FLIPPED: u8 = 1 << 0;

// COMMAND.
pub const COMMAND_ENABLE_VBUS_DETECT: u8 = 0x33;
pub const COMMAND_DISABLE_SINK_VBUS: u8 = 0x44;
pub const COMMAND_SINK_VBUS: u8 = 0x55;
pub const COMMAND_LOOK4CONNECTION: u8 = 0x99;

// RECEIVE_DETECT: SOP messages and hard resets.
pub const DETECT_SOP_AND_HARD_RESET: u8 = 1 << 0 | 1 << 5;

// TRANSMIT: SOP with the controller's three retries, and a hard reset.
pub const TRANSMIT_SOP: u8 = 3 << 4;
pub const TRANSMIT_HARD_RESET: u8 = 5;

/// ROLE_CONTROL for a sink: Rd on both CC lines.
pub const ROLE_SINK: u8 = 0b10 << 2 | 0b10;
/// And for error recovery: both open.
pub const ROLE_OPEN: u8 = 0b11 << 2 | 0b11;

/// MESSAGE_HEADER_INFO for a sink that is the upstream-facing port, speaking `revision` (1 for 2.0, 2 for 3.0).
pub fn header_info(revision: u8) -> u8 {
	(revision & 0x3) << 1
}

/// CC_STATUS as a sink sees it: the stronger line's advertisement, open on both lines being open.
pub fn cc(status: u8) -> Cc {
	let level = (status & 0x3).max((status >> 2) & 0x3);
	match level {
		1 => Cc::Rp(Rp::Default),
		2 => Cc::Rp(Rp::Medium),
		3 => Cc::Rp(Rp::High),
		_ => Cc::Open,
	}
}

/// Which line the partner's Rp is on: `Some(true)` for CC2 (flipped), `Some(false)` for CC1, `None` on neither.
pub fn flipped(status: u8) -> Option<bool> {
	match (status & 0x3, (status >> 2) & 0x3) {
		(0, 0) => None,
		(cc1, cc2) => Some(cc2 > cc1),
	}
}

/// VBUS_VOLTAGE in millivolts: 25 mV units, scaled down by the factor in bits 11-10.
pub fn vbus_millivolts(raw: u16) -> u32 {
	let scale = match (raw >> 10) & 0x3 {
		0 => 1,
		1 => 2,
		2 => 4,
		_ => return 0,
	};
	u32::from(raw & 0x3FF) * 25 * scale
}

/// A VBUS alarm threshold in the register's 25 mV units.
pub fn alarm_threshold(millivolts: u32) -> u16 {
	(millivolts / 25).min(0x3FF) as u16
}

/// RECEIVE_BUFFER as read: the readable byte count, the frame type, then the message. The message, if the frame is
/// SOP and the count fits what was read.
pub fn received(buffer: &[u8]) -> Option<&[u8]> {
	let count = usize::from(*buffer.first()?);
	let frame = *buffer.get(1)?;
	if count < 1 || 1 + count > buffer.len() || frame != 0 {
		return None;
	}
	Some(&buffer[2..1 + count])
}

/// TRANSMIT_BUFFER as written: the byte count, then the message.
pub fn transmit_buffer(message: &[u8]) -> Vec<u8> {
	let mut out = Vec::with_capacity(1 + message.len());
	out.push(message.len() as u8);
	out.extend_from_slice(message);
	out
}
