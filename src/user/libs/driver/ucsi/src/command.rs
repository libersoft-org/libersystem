//! THE COMMANDS, as CONTROL carries them: the command code in bits 7-0, the length of MESSAGE_OUT in 15-8, and the
//! command's own fields from bit 16. Each is built with the largest answer it may carry, which `execute` holds CCI's
//! length to.

pub const PPM_RESET: u8 = 0x01;
pub const ACK_CC_CI: u8 = 0x04;
pub const SET_NOTIFICATION_ENABLE: u8 = 0x05;
pub const GET_CAPABILITY: u8 = 0x06;
pub const GET_CONNECTOR_CAPABILITY: u8 = 0x07;
/// `SET_CCOM` from 2.0; `SET_UOM` before it, the same code.
pub const SET_CCOM: u8 = 0x08;
pub const SET_UOR: u8 = 0x09;
pub const SET_PDM: u8 = 0x0A;
pub const SET_PDR: u8 = 0x0B;
pub const GET_ALTERNATE_MODES: u8 = 0x0C;
pub const GET_CAM_SUPPORTED: u8 = 0x0D;
pub const GET_CURRENT_CAM: u8 = 0x0E;
pub const SET_NEW_CAM: u8 = 0x0F;
pub const GET_PDOS: u8 = 0x10;
pub const GET_CABLE_PROPERTY: u8 = 0x11;
pub const GET_CONNECTOR_STATUS: u8 = 0x12;
pub const GET_ERROR_STATUS: u8 = 0x13;
pub const GET_ATTENTION_VDO: u8 = 0x16;

/// One command: CONTROL's value, and the most MESSAGE_IN bytes its answer may fill.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Command {
	pub control: u64,
	pub answer: usize,
}

impl Command {
	pub fn code(&self) -> u8 {
		self.control as u8
	}
}

const fn command(code: u8, fields: u64, answer: usize) -> Command {
	Command { control: code as u64 | fields << 16, answer }
}

fn connector(number: u8) -> u64 {
	u64::from(number & 0x7F)
}

pub fn ppm_reset() -> Command {
	command(PPM_RESET, 0, 0)
}

/// The acknowledgement: the command's completion, and - only beside a `GET_CONNECTOR_STATUS`'s - a connector change.
pub fn ack(command_complete: bool, connector_change: bool) -> Command {
	command(ACK_CC_CI, u64::from(connector_change) | u64::from(command_complete) << 1, 0)
}

/// Every notification this OPM takes: command completion, external supply, operation mode, supported capabilities,
/// negotiated power level, PD reset, supported alternate modes, battery status, partner, power direction, connect and
/// error.
pub const NOTIFY_ALL: u16 = 0b1101_1011_1110_0111;

pub fn set_notification_enable(mask: u16) -> Command {
	command(SET_NOTIFICATION_ENABLE, u64::from(mask), 0)
}

pub fn get_capability() -> Command {
	command(GET_CAPABILITY, 0, 16)
}

pub fn get_connector_capability(number: u8) -> Command {
	command(GET_CONNECTOR_CAPABILITY, connector(number), 16)
}

pub fn get_connector_status(number: u8) -> Command {
	command(GET_CONNECTOR_STATUS, connector(number), 19)
}

pub fn get_error_status() -> Command {
	command(GET_ERROR_STATUS, 0, 16)
}

/// The USB operation role: bit 0 DFP (host), bit 1 UFP (device), bit 2 accept a swap the partner asks for.
pub fn set_uor(number: u8, host: bool, device: bool, accept: bool) -> Command {
	command(SET_UOR, connector(number) | (u64::from(host) | u64::from(device) << 1 | u64::from(accept) << 2) << 7, 0)
}

/// The power direction role: bit 0 provider (source), bit 1 consumer (sink), bit 2 accept a swap the partner asks for.
pub fn set_pdr(number: u8, source: bool, sink: bool, accept: bool) -> Command {
	command(SET_PDR, connector(number) | (u64::from(source) | u64::from(sink) << 1 | u64::from(accept) << 2) << 7, 0)
}

/// The connector's operation mode (`SET_CCOM`, or `SET_UOM` before 2.0).
pub fn set_ccom(number: u8, mode: u16) -> Command {
	command(SET_CCOM, connector(number) | u64::from(mode) << 7, 0)
}

/// The power direction mode.
pub fn set_pdm(number: u8, mode: u8) -> Command {
	command(SET_PDM, connector(number) | u64::from(mode & 0x7) << 7, 0)
}

/// Up to four power data objects from `offset`: the partner's when `partner`, the source capabilities when `source`.
pub fn get_pdos(number: u8, partner: bool, offset: u8, count: u8, source: bool) -> Command {
	let count = count.clamp(1, 4);
	command(GET_PDOS, connector(number) | u64::from(partner) << 7 | u64::from(offset) << 8 | u64::from(count - 1) << 16 | u64::from(source) << 18, usize::from(count) * 4)
}

pub fn get_cable_property(number: u8) -> Command {
	command(GET_CABLE_PROPERTY, connector(number), 5)
}

/// Whose alternate modes: the connector's own - which `SET_NEW_CAM`'s offset indexes - or the partner's (SOP).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Recipient {
	Connector = 0,
	Partner = 1,
}

/// One or two alternate modes from `offset` of the recipient's list.
pub fn get_alternate_modes(recipient: Recipient, number: u8, offset: u8, count: u8) -> Command {
	let count = count.clamp(1, 2);
	command(GET_ALTERNATE_MODES, recipient as u64 | connector(number) << 8 | u64::from(offset) << 16 | u64::from(count - 1) << 24, usize::from(count) * 6)
}

pub fn get_cam_supported(number: u8) -> Command {
	command(GET_CAM_SUPPORTED, connector(number), 16)
}

pub fn get_current_cam(number: u8) -> Command {
	command(GET_CURRENT_CAM, connector(number), 16)
}

pub fn get_attention_vdo(number: u8) -> Command {
	command(GET_ATTENTION_VDO, connector(number), 4)
}

/// Enter (or leave) the alternate mode at `offset` of the connector's list, with the mode's own configuration.
pub fn set_new_cam(number: u8, enter: bool, offset: u8, configuration: u32) -> Command {
	command(SET_NEW_CAM, connector(number) | u64::from(enter) << 7 | u64::from(offset) << 8 | u64::from(configuration) << 16, 0)
}
