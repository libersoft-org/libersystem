//! THE CHASSIS (IPMI 2.0, 28): its status, the identify light, and the control operations - which reach the machine
//! only as administrative actions, never from here. POWER UP and the diagnostic interrupt are refused at preparation:
//! a machine that is running needs no power up, and an interrupt is not an operation a person approves.

use crate::Request;

pub const GET_CHASSIS_STATUS: u8 = 0x01;
pub const CHASSIS_CONTROL: u8 = 0x02;
pub const CHASSIS_IDENTIFY: u8 = 0x04;

/// A chassis control operation, by the byte Chassis Control takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Control {
	PowerDown,
	PowerCycle,
	HardReset,
	SoftShutdown,
}

impl Control {
	/// The operation an administrative request's parameter byte names: 0, 2, 3 or 5. Power up (1) and the diagnostic
	/// interrupt (4) are refused.
	pub const fn from_parameter(byte: u8) -> Option<Control> {
		match byte {
			0 => Some(Control::PowerDown),
			2 => Some(Control::PowerCycle),
			3 => Some(Control::HardReset),
			5 => Some(Control::SoftShutdown),
			_ => None,
		}
	}

	pub const fn byte(self) -> u8 {
		match self {
			Control::PowerDown => 0,
			Control::PowerCycle => 2,
			Control::HardReset => 3,
			Control::SoftShutdown => 5,
		}
	}

	/// Whether it stops the machine at once, with nothing stopped or flushed.
	pub const fn hard(self) -> bool {
		!matches!(self, Control::SoftShutdown)
	}
}

pub fn control(operation: Control) -> Request {
	Request::new(crate::netfn::CHASSIS, CHASSIS_CONTROL, &[operation.byte()])
}

pub fn status_request() -> Request {
	Request::new(crate::netfn::CHASSIS, GET_CHASSIS_STATUS, &[])
}

/// On for `seconds` (1 to 255), or off with 0.
pub fn identify(seconds: u8) -> Request {
	Request::new(crate::netfn::CHASSIS, CHASSIS_IDENTIFY, &[seconds])
}

/// Get Chassis Status's answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Status {
	pub power_on: bool,
	pub overload: bool,
	pub interlock: bool,
	pub power_fault: bool,
	pub control_fault: bool,
	/// 0 stays off, 1 restores the last state, 2 powers up, 3 unknown.
	pub restore_policy: u8,
	pub last_event: u8,
	pub intrusion: bool,
	pub drive_fault: bool,
	pub cooling_fault: bool,
	/// The identify light: 0 off, 1 timed on, 2 on indefinitely - when the BMC reports it.
	pub identify: Option<u8>,
}

pub fn status(data: &[u8]) -> Option<Status> {
	if data.len() < 3 {
		return None;
	}
	let misc = data[2];
	Some(Status { power_on: data[0] & 1 != 0, overload: data[0] & 2 != 0, interlock: data[0] & 4 != 0, power_fault: data[0] & 8 != 0, control_fault: data[0] & 0x10 != 0, restore_policy: (data[0] >> 5) & 3, last_event: data[1], intrusion: misc & 1 != 0, drive_fault: misc & 4 != 0, cooling_fault: misc & 8 != 0, identify: (misc & 0x40 != 0).then_some((misc >> 4) & 3) })
}
