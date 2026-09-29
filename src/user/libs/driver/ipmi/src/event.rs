//! THE SYSTEM'S OWN EVENTS, as the BMC's log records them: a Platform Event Message (Sensor/Event 0x02) the driver
//! builds and sends through the system interface, generator 0x41 - system management software - so every one reads
//! the same in the log whatever the interface.

use crate::Request;

pub const PLATFORM_EVENT: u8 = 0x02;
/// System management software, software ID 0x20.
pub const GENERATOR: u8 = 0x41;
/// Event message format revision 0x04: IPMI 2.0.
pub const EVM_REV: u8 = 0x04;
/// The event/reading type of a sensor-specific event.
pub const SENSOR_SPECIFIC: u8 = 0x6F;
/// The sensor number the system's own events carry.
pub const SENSOR: u8 = 0x00;

/// OS Boot (0x1F), offset 0x06: boot completed, device not specified.
pub const OS_BOOT: u8 = 0x1F;
pub const BOOT_COMPLETED: u8 = 0x06;
/// OS Stop/Shutdown (0x20), offset 0x03: an orderly shutdown.
pub const OS_STOP: u8 = 0x20;
pub const GRACEFUL_SHUTDOWN: u8 = 0x03;

/// A sensor-specific event with only its offset: data 2 and 3 unspecified.
pub fn message(sensor_type: u8, offset: u8) -> Request {
	Request::new(crate::netfn::SENSOR_EVENT, PLATFORM_EVENT, &[GENERATOR, EVM_REV, sensor_type, SENSOR, SENSOR_SPECIFIC, offset & 0x0F, 0xFF, 0xFF])
}

pub fn boot_completed() -> Request {
	message(OS_BOOT, BOOT_COMPLETED)
}

pub fn graceful_shutdown() -> Request {
	message(OS_STOP, GRACEFUL_SHUTDOWN)
}

/// Whether a SEL system event record is one of these, and which.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ours {
	Boot,
	Shutdown,
}

pub fn ours(record: &crate::sel::Record) -> Option<Ours> {
	let crate::sel::Record::System { generator, sensor_type, event_type, data, .. } = record else { return None };
	if *generator & 0xFF != u16::from(GENERATOR) || *event_type != SENSOR_SPECIFIC {
		return None;
	}
	match (*sensor_type, data[0] & 0x0F) {
		(OS_BOOT, BOOT_COMPLETED) => Some(Ours::Boot),
		(OS_STOP, GRACEFUL_SHUTDOWN) => Some(Ours::Shutdown),
		_ => None,
	}
}
