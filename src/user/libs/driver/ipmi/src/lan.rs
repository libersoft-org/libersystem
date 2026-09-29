//! LAN CONFIGURATION AND USERS, READ-ONLY (IPMI 2.0, 22 and 23): which channels are LAN channels, each one's address
//! source, address, mask, gateway, MAC and VLAN, and each user's name, enabled state and privilege limit on it.
//! Changing either is excluded: a user change is a password.

use crate::Request;
use alloc::string::String;
use alloc::vec::Vec;

pub const GET_CHANNEL_INFO: u8 = 0x42;
pub const GET_USER_ACCESS: u8 = 0x44;
pub const GET_USER_NAME: u8 = 0x46;
/// On the transport network function.
pub const GET_LAN_CONFIGURATION: u8 = 0x02;

/// The channel medium of an 802.3 LAN.
pub const MEDIUM_LAN: u8 = 0x04;
/// Channels 1 to 11 are the ones a LAN channel may be.
pub const CHANNELS: core::ops::RangeInclusive<u8> = 1..=11;
/// The most users one channel is read for.
pub const MAX_USERS: u8 = 16;

pub mod parameter {
	pub const IP_ADDRESS: u8 = 3;
	pub const IP_SOURCE: u8 = 4;
	pub const MAC: u8 = 5;
	pub const SUBNET_MASK: u8 = 6;
	pub const GATEWAY: u8 = 12;
	pub const VLAN: u8 = 20;
}

pub fn channel_info_request(channel: u8) -> Request {
	Request::new(crate::netfn::APP, GET_CHANNEL_INFO, &[channel & 0x0F])
}

/// The channel's medium type, from Get Channel Info.
pub fn medium(data: &[u8]) -> Option<u8> {
	Some(data.get(1)? & 0x7F)
}

pub fn parameter_request(channel: u8, parameter: u8) -> Request {
	Request::new(crate::netfn::TRANSPORT, GET_LAN_CONFIGURATION, &[channel & 0x0F, parameter, 0, 0])
}

/// A parameter's data, after its revision byte, when it is `length` bytes.
pub fn parameter(data: &[u8], length: usize) -> Option<&[u8]> {
	data.get(1..1 + length)
}

/// One LAN channel's configuration; each part absent when the BMC did not answer it.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Lan {
	pub channel: u8,
	/// 0 unspecified, 1 static, 2 DHCP, 3 the BIOS, 4 other.
	pub source: Option<u8>,
	pub address: Option<[u8; 4]>,
	pub mask: Option<[u8; 4]>,
	pub gateway: Option<[u8; 4]>,
	pub mac: Option<[u8; 6]>,
	/// The VLAN ID, when one is enabled.
	pub vlan: Option<Option<u16>>,
}

pub fn vlan(data: &[u8]) -> Option<Option<u16>> {
	let raw = u16::from_le_bytes(parameter(data, 2)?.try_into().ok()?);
	Some((raw & 0x8000 != 0).then_some(raw & 0x0FFF))
}

pub fn user_access_request(channel: u8, user: u8) -> Request {
	Request::new(crate::netfn::APP, GET_USER_ACCESS, &[channel & 0x0F, user & 0x3F])
}

pub fn user_name_request(user: u8) -> Request {
	Request::new(crate::netfn::APP, GET_USER_NAME, &[user & 0x3F])
}

/// Get User Access's answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Access {
	pub max_users: u8,
	pub enabled_users: u8,
	/// The user's privilege limit on the channel: 1 callback, 2 user, 3 operator, 4 administrator, 5 OEM, 0xF none.
	pub privilege: u8,
	pub messaging: bool,
}

pub fn access(data: &[u8]) -> Option<Access> {
	if data.len() < 4 {
		return None;
	}
	Some(Access { max_users: data[0] & 0x3F, enabled_users: data[1] & 0x3F, privilege: data[3] & 0x0F, messaging: data[3] & 0x10 != 0 })
}

/// A user name: sixteen bytes, padded with NULs.
pub fn user_name(data: &[u8]) -> Option<String> {
	let bytes = data.get(..16)?;
	Some(bytes.iter().take_while(|byte| **byte != 0).map(|byte| char::from(*byte)).collect())
}

/// One user as read.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct User {
	pub id: u8,
	pub name: String,
	pub enabled: bool,
	pub privilege: u8,
}

/// Users of one channel, as read.
pub type Users = Vec<User>;
