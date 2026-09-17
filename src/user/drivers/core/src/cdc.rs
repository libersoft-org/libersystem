// THE COMMUNICATIONS-CLASS DECISIONS, WITH NO DEVICE BEHIND THEM.
//
// Two network classes share this module and DO NOT share a frame format, which is the whole reason
// it is one module and not two: ECM and NCM are the same descriptors, the same pair of interfaces
// and the same control requests, and then ECM puts ONE Ethernet frame in a transfer while NCM packs
// SEVERAL into a block with a table of offsets. The plumbing is common and the framing is not, and a
// module that ran them together would be one that had to be told which it was in every function.
//
// WHERE SUCH A DRIVER IS WRONG IS THE NCM TABLE, and it is wrong by reading past a buffer rather
// than by getting a number slightly off. A block arrives with a header naming where its datagram
// table is, and the table names where each datagram is and how long. Every one of those is a number
// THE DEVICE CHOSE, they index into the block the device also sent, and they can point backwards,
// past the end, at each other, or at themselves. This module is the reason those are refusals.
//
// This module decides. It never transfers: the controller owns the bus.

use crate::descriptor;

/// The communications interface class, and the data interface class that pairs with it.
pub const CLASS_COMMUNICATIONS: u8 = 0x02;
pub const CLASS_CDC_DATA: u8 = 0x0A;

/// The two network models this module speaks, as interface subclasses.
pub const SUBCLASS_ECM: u8 = 0x06;
pub const SUBCLASS_NCM: u8 = 0x0D;

/// Class-specific descriptor types and the functional subtypes inside them.
pub const DT_CS_INTERFACE: u8 = 0x24;
pub const FN_HEADER: u8 = 0x00;
pub const FN_UNION: u8 = 0x06;
pub const FN_ETHERNET: u8 = 0x0F;
pub const FN_NCM: u8 = 0x1A;

/// Class requests this driver issues.
///
/// SET_ETHERNET_PACKET_FILTER IS NOT OPTIONAL IN PRACTICE. An ECM device comes up with its filter
/// empty and forwards nothing; a driver that configures the alternate setting and waits is a driver
/// whose link is up and silent, which looks exactly like a cable that is not plugged in.
pub const REQ_SET_ETHERNET_PACKET_FILTER: u8 = 0x43;
pub const REQ_GET_NTB_PARAMETERS: u8 = 0x80;
pub const REQ_SET_NTB_INPUT_SIZE: u8 = 0x86;

/// Packet-filter bits, as `SET_ETHERNET_PACKET_FILTER`'s value.
pub const FILTER_PROMISCUOUS: u16 = 1 << 0;
pub const FILTER_ALL_MULTICAST: u16 = 1 << 1;
pub const FILTER_DIRECTED: u16 = 1 << 2;
pub const FILTER_BROADCAST: u16 = 1 << 3;
pub const FILTER_MULTICAST: u16 = 1 << 4;

/// Which network model a configuration describes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Model {
	/// One Ethernet frame per transfer.
	Ecm,
	/// Several frames per block, with a table of offsets.
	Ncm,
}

/// What the configuration descriptor says this device is, and where its parts are.
///
/// THE DATA INTERFACE'S ALTERNATE SETTING IS PART OF THE ANSWER AND NOT AN AFTERTHOUGHT. A CDC data
/// interface has alternate setting zero with NO ENDPOINTS - that is the specification's way of
/// saying "not carrying traffic" - and the endpoints appear only in a higher setting. A driver that
/// configures the device and starts waiting on alternate zero waits on endpoints that do not exist.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Binding {
	pub model: Model,
	pub config_value: u8,
	/// The communications interface, which carries the class descriptors and the notification
	/// endpoint.
	pub control_interface: u8,
	/// The data interface named by the union descriptor, and the setting its endpoints are in.
	pub data_interface: u8,
	pub data_alternate: u8,
	/// The bulk pair, as endpoint addresses.
	pub bulk_in: u8,
	pub bulk_out: u8,
	pub bulk_in_packet: u16,
	pub bulk_out_packet: u16,
	/// The string index the MAC address is published under. Zero means the device named none, which
	/// is a device that cannot say what address it answers to.
	pub mac_string: u8,
	/// The largest Ethernet frame the device will carry, from the Ethernet functional descriptor.
	pub max_segment: u16,
}

/// Why a configuration is not one this driver can bind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NotBindable {
	/// No communications interface of a network subclass this driver speaks.
	NoNetworkInterface,
	/// No union functional descriptor, so nothing says which data interface belongs to it.
	NoUnion,
	/// The union names a data interface that is not in this configuration.
	NoDataInterface,
	/// The data interface has no alternate setting carrying a bulk pair.
	NoBulkPair,
	/// No Ethernet functional descriptor, so the device published no MAC and no segment size.
	NoEthernet,
	/// A descriptor record is malformed or runs past the transfer.
	Malformed,
}

/// Read one configuration descriptor and decide what to bind.
///
/// ONE PASS AND NO ASSUMED ORDER. The class descriptors, the data interface and its endpoints may
/// appear in any order a device chooses, and several devices put the union descriptor after the
/// interface it names. So this collects and then decides, rather than deciding as it walks.
pub fn bind(config: &[u8]) -> Result<Binding, NotBindable> {
	let mut walk = descriptor::Walk::new(config);
	let mut config_value: Option<u8> = None;
	let mut model: Option<Model> = None;
	let mut control_interface: Option<u8> = None;
	let mut union_data: Option<u8> = None;
	let mut mac_string: Option<u8> = None;
	let mut max_segment: u16 = 0;
	// The data interface's candidate settings, as (interface, alternate, in, out, in mps, out mps).
	let mut candidate: Option<(u8, u8, Option<(u8, u16)>, Option<(u8, u16)>)> = None;
	let mut best: Option<(u8, u8, u8, u8, u16, u16)> = None;
	// Which interface the class descriptors that follow belong to.
	let mut current_is_control = false;

	let finish = |candidate: &mut Option<(u8, u8, Option<(u8, u16)>, Option<(u8, u16)>)>, best: &mut Option<(u8, u8, u8, u8, u16, u16)>| {
		if let Some((iface, alt, Some((in_addr, in_mps)), Some((out_addr, out_mps)))) = candidate.take() {
			// THE FIRST SETTING THAT CARRIES A PAIR WINS, and lower alternates are preferred because
			// a device that offers several is offering larger buffers, not different function.
			if best.is_none_or(|(_, have, _, _, _, _)| alt < have) {
				*best = Some((iface, alt, in_addr, out_addr, in_mps, out_mps));
			}
		}
	};

	for record in walk.by_ref() {
		match record.kind {
			descriptor::DT_CONFIG => {
				config_value = record.field(5).ok();
			}
			descriptor::DT_INTERFACE => {
				finish(&mut candidate, &mut best);
				let (Ok(number), Ok(alternate), Ok(class), Ok(subclass)) = (record.field(2), record.field(3), record.field(5), record.field(6)) else {
					return Err(NotBindable::Malformed);
				};
				current_is_control = false;
				if class == CLASS_COMMUNICATIONS && model.is_none() {
					match subclass {
						SUBCLASS_ECM => {
							model = Some(Model::Ecm);
							control_interface = Some(number);
							current_is_control = true;
						}
						SUBCLASS_NCM => {
							model = Some(Model::Ncm);
							control_interface = Some(number);
							current_is_control = true;
						}
						_ => {}
					}
				} else if class == CLASS_COMMUNICATIONS && Some(number) == control_interface {
					current_is_control = true;
				} else if class == CLASS_CDC_DATA {
					candidate = Some((number, alternate, None, None));
				}
			}
			DT_CS_INTERFACE if current_is_control => {
				let Ok(subtype) = record.field(2) else { return Err(NotBindable::Malformed) };
				match subtype {
					FN_UNION => {
						// The subordinate interface at offset four is the data interface. A union
						// with no subordinate is a union that names nothing.
						union_data = record.field(4).ok();
					}
					FN_ETHERNET => {
						mac_string = record.field(3).ok();
						max_segment = record.field16(8).unwrap_or(0);
					}
					_ => {}
				}
			}
			descriptor::DT_ENDPOINT => {
				if let Some((_, _, ref mut ep_in, ref mut ep_out)) = candidate {
					let (Ok(address), Ok(attributes), Ok(packet)) = (record.field(2), record.field(3), record.field16(4)) else {
						return Err(NotBindable::Malformed);
					};
					// Bulk only. An interrupt endpoint on a data interface is not a frame pipe.
					if attributes & 0x03 == 0x02 {
						if address & 0x80 != 0 {
							ep_in.get_or_insert((address, packet));
						} else {
							ep_out.get_or_insert((address, packet));
						}
					}
				}
			}
			_ => {}
		}
	}
	finish(&mut candidate, &mut best);
	if walk.fault().is_some() {
		return Err(NotBindable::Malformed);
	}

	let model = model.ok_or(NotBindable::NoNetworkInterface)?;
	let control_interface = control_interface.ok_or(NotBindable::NoNetworkInterface)?;
	let mac_string = mac_string.ok_or(NotBindable::NoEthernet)?;
	let union_data = union_data.ok_or(NotBindable::NoUnion)?;
	let (data_interface, data_alternate, bulk_in, bulk_out, bulk_in_packet, bulk_out_packet) = best.ok_or(NotBindable::NoBulkPair)?;
	// THE UNION IS BELIEVED AND THE WALK IS CHECKED AGAINST IT. A device whose union names interface
	// three while its only data interface is one is describing a configuration this driver would
	// otherwise bind to the wrong half of.
	if union_data != data_interface {
		return Err(NotBindable::NoDataInterface);
	}
	Ok(Binding { model, config_value: config_value.ok_or(NotBindable::Malformed)?, control_interface, data_interface, data_alternate, bulk_in, bulk_out, bulk_in_packet, bulk_out_packet, mac_string, max_segment })
}

/// Decode the MAC address a device publishes as a string descriptor.
///
/// IT IS TWELVE UPPER-CASE HEX CHARACTERS IN UTF-16, which is a specification requirement and not a
/// convention - so a string of any other length, or one with a character that is not hex, is a
/// device that did not answer the question rather than one to guess at. Lower case is accepted
/// because devices ship it and nothing is ambiguous about it.
pub fn mac_from_string(bytes: &[u8]) -> Option<[u8; 6]> {
	// A string descriptor is `bLength`, `bDescriptorType`, then UTF-16LE.
	if bytes.len() < 2 || bytes[1] != 0x03 {
		return None;
	}
	let declared = bytes[0] as usize;
	if declared > bytes.len() || declared != 2 + 24 {
		return None;
	}
	let mut mac = [0u8; 6];
	for (i, byte) in mac.iter_mut().enumerate() {
		let hi = hex(bytes[2 + i * 4], bytes[3 + i * 4])?;
		let lo = hex(bytes[4 + i * 4], bytes[5 + i * 4])?;
		*byte = hi << 4 | lo;
	}
	Some(mac)
}

// One UTF-16LE code unit as a hex digit. The high byte must be zero: a character outside ASCII is
// not a hex digit however it renders.
fn hex(low: u8, high: u8) -> Option<u8> {
	if high != 0 {
		return None;
	}
	match low {
		b'0'..=b'9' => Some(low - b'0'),
		b'a'..=b'f' => Some(low - b'a' + 10),
		b'A'..=b'F' => Some(low - b'A' + 10),
		_ => None,
	}
}

// ---------------------------------------------------------------------------------------------
// NCM: the transfer block, which is the part a driver reads past a buffer for.
// ---------------------------------------------------------------------------------------------

/// The 16-bit transfer header's signature, "NCMH".
pub const NTH16_SIGNATURE: u32 = 0x484D_434E;
/// The 16-bit datagram pointer's signature, "NCM0" - and the CRC variant "NCM1", which this driver
/// refuses rather than mis-reads: the two have the same table and different datagram contents.
pub const NDP16_SIGNATURE: u32 = 0x304D_434E;
pub const NDP16_CRC_SIGNATURE: u32 = 0x314D_434E;

pub const NTH16_LEN: usize = 12;
pub const NDP16_MIN_LEN: usize = 16;

/// Why a transfer block is refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlockFault {
	/// Too short to hold a header, or a header shorter than the layout.
	Short,
	/// Not "NCMH", or a datagram pointer that is not "NCM0".
	Signature,
	/// The header's own block length disagrees with what arrived.
	Length,
	/// A pointer lands outside the block, or inside the header it follows.
	Pointer,
	/// A datagram runs past the block, or its table runs past the pointer's own length.
	Datagram,
	/// The pointer chain does not end.
	Chain,
}

/// One transfer block's header.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Nth16 {
	pub sequence: u16,
	pub block_length: u16,
	pub ndp_index: u16,
}

/// Read the block header and check it against what actually arrived.
pub fn nth16(block: &[u8]) -> Result<Nth16, BlockFault> {
	if block.len() < NTH16_LEN {
		return Err(BlockFault::Short);
	}
	let signature = u32::from_le_bytes([block[0], block[1], block[2], block[3]]);
	if signature != NTH16_SIGNATURE {
		return Err(BlockFault::Signature);
	}
	let header_length = u16::from_le_bytes([block[4], block[5]]) as usize;
	if header_length < NTH16_LEN || header_length > block.len() {
		return Err(BlockFault::Short);
	}
	let sequence = u16::from_le_bytes([block[6], block[7]]);
	let block_length = u16::from_le_bytes([block[8], block[9]]);
	let ndp_index = u16::from_le_bytes([block[10], block[11]]);
	// THE DEVICE'S OWN LENGTH AGAINST THE TRANSPORT'S OBSERVATION. A block claiming more than
	// arrived is the claim that makes every offset below reach past the buffer.
	if block_length as usize > block.len() {
		return Err(BlockFault::Length);
	}
	// AND THE TABLE MUST BE INSIDE THE BLOCK AND AFTER THE HEADER. Zero is the one value that means
	// "no datagrams", and anything between one and the header's end points into the header itself.
	if ndp_index != 0 && ((ndp_index as usize) < header_length || (ndp_index as usize) + NDP16_MIN_LEN > block_length as usize) {
		return Err(BlockFault::Pointer);
	}
	Ok(Nth16 { sequence, block_length, ndp_index })
}

/// One datagram inside a block, as an offset and a length into it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Datagram {
	pub at: u16,
	pub len: u16,
}

/// Walk the datagrams of one transfer block.
///
/// THE CHAIN IS BOUNDED BY CONSTRUCTION. Each datagram pointer names the next one, and a device can
/// point a pointer at itself, at one before it, or around a ring - so this walks at most as many
/// pointers as could fit in the block and refuses rather than looping. A bound written as "a
/// reasonable number" is the same defect with a bigger constant.
pub struct Datagrams<'a> {
	block: &'a [u8],
	block_length: usize,
	next_ndp: usize,
	// Within the current pointer: the byte offset of the next entry, and where its table ends.
	entry: usize,
	entry_end: usize,
	visited: usize,
	most: usize,
	fault: Option<BlockFault>,
}

impl<'a> Datagrams<'a> {
	pub fn new(block: &'a [u8], header: Nth16) -> Datagrams<'a> {
		Datagrams {
			block,
			block_length: header.block_length as usize,
			next_ndp: header.ndp_index as usize,
			entry: 0,
			entry_end: 0,
			visited: 0,
			// Every pointer is at least sixteen bytes, so a block cannot hold more than this many of
			// them however they are chained.
			most: header.block_length as usize / NDP16_MIN_LEN + 1,
			fault: None,
		}
	}

	/// The refusal that ended the walk, if one did.
	pub fn fault(&self) -> Option<BlockFault> {
		self.fault
	}

	// Move to the pointer at `next_ndp`, or answer false when the chain is over or refused.
	fn open_pointer(&mut self) -> bool {
		loop {
			if self.next_ndp == 0 {
				return false;
			}
			self.visited += 1;
			if self.visited > self.most {
				self.fault = Some(BlockFault::Chain);
				return false;
			}
			let at = self.next_ndp;
			if at + NDP16_MIN_LEN > self.block_length {
				self.fault = Some(BlockFault::Pointer);
				return false;
			}
			let signature = u32::from_le_bytes([self.block[at], self.block[at + 1], self.block[at + 2], self.block[at + 3]]);
			if signature != NDP16_SIGNATURE {
				// THE CRC VARIANT IS A DIFFERENT FORMAT AND NOT A DIALECT OF THIS ONE, so it is
				// refused by name rather than walked as if its datagrams were plain.
				self.fault = Some(BlockFault::Signature);
				return false;
			}
			let length = u16::from_le_bytes([self.block[at + 4], self.block[at + 5]]) as usize;
			let next = u16::from_le_bytes([self.block[at + 6], self.block[at + 7]]) as usize;
			if length < NDP16_MIN_LEN || at + length > self.block_length {
				self.fault = Some(BlockFault::Datagram);
				return false;
			}
			self.entry = at + 8;
			self.entry_end = at + length;
			self.next_ndp = next;
			return true;
		}
	}
}

impl Iterator for Datagrams<'_> {
	type Item = Datagram;

	fn next(&mut self) -> Option<Datagram> {
		loop {
			if self.fault.is_some() {
				return None;
			}
			if self.entry + 4 > self.entry_end {
				if !self.open_pointer() {
					return None;
				}
				continue;
			}
			let at = u16::from_le_bytes([self.block[self.entry], self.block[self.entry + 1]]);
			let len = u16::from_le_bytes([self.block[self.entry + 2], self.block[self.entry + 3]]);
			self.entry += 4;
			// A ZERO PAIR ENDS THIS POINTER'S TABLE, which is the specification's terminator and not
			// an empty datagram.
			if at == 0 && len == 0 {
				self.entry = self.entry_end;
				continue;
			}
			if len == 0 || at as usize + len as usize > self.block_length {
				self.fault = Some(BlockFault::Datagram);
				return None;
			}
			return Some(Datagram { at, len });
		}
	}
}

/// Build one 16-bit transfer block holding a single datagram, for the transmit side.
///
/// ONE DATAGRAM PER BLOCK, DELIBERATELY. Packing several is what NCM is for and it is a latency
/// trade this driver does not have the evidence to make: a frame held back waiting for a second one
/// is a frame delayed by whatever the next one takes to arrive, and nothing here measures that yet.
/// The receive side reads whatever the device packs, which is where the packing matters.
pub fn ncm_block(sequence: u16, frame: &[u8], out: &mut [u8]) -> Option<usize> {
	let total = NTH16_LEN + NDP16_MIN_LEN + frame.len();
	if total > out.len() || total > u16::MAX as usize {
		return None;
	}
	out[..total].fill(0);
	out[0..4].copy_from_slice(&NTH16_SIGNATURE.to_le_bytes());
	out[4..6].copy_from_slice(&(NTH16_LEN as u16).to_le_bytes());
	out[6..8].copy_from_slice(&sequence.to_le_bytes());
	out[8..10].copy_from_slice(&(total as u16).to_le_bytes());
	out[10..12].copy_from_slice(&(NTH16_LEN as u16).to_le_bytes());
	let ndp = NTH16_LEN;
	out[ndp..ndp + 4].copy_from_slice(&NDP16_SIGNATURE.to_le_bytes());
	out[ndp + 4..ndp + 6].copy_from_slice(&(NDP16_MIN_LEN as u16).to_le_bytes());
	// No next pointer.
	out[ndp + 6..ndp + 8].copy_from_slice(&0u16.to_le_bytes());
	let data_at = (NTH16_LEN + NDP16_MIN_LEN) as u16;
	out[ndp + 8..ndp + 10].copy_from_slice(&data_at.to_le_bytes());
	out[ndp + 10..ndp + 12].copy_from_slice(&(frame.len() as u16).to_le_bytes());
	// The terminating zero pair is already there from the fill.
	out[data_at as usize..total].copy_from_slice(frame);
	Some(total)
}

#[cfg(test)]
mod tests;
