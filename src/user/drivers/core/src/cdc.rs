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
pub const SUBCLASS_ACM: u8 = 0x02;
/// A VENDOR PROTOCOL UNDER THE ACM SUBCLASS, WHICH IS WHAT RNDIS IS.
///
/// Microsoft's Remote NDIS puts an ETHERNET ADAPTER behind class 2, subclass 2 and protocol 0xFF -
/// the same class and subclass a serial port declares - so the two bytes that used to decide this
/// name a network card as well as a modem. Measured on QEMU's own `usb-net`, whose default mode is
/// RNDIS: the serial binding accepted it, took the device, and the boot reported no network provider
/// at all with nothing anywhere saying why.
///
/// SO THE PROTOCOL IS PART OF THE ANSWER. An ACM serial port declares no protocol, or one of the
/// command sets the subclass defines; a vendor protocol under this subclass is a device speaking
/// something else entirely through an ACM-shaped control interface.
pub const PROTOCOL_VENDOR: u8 = 0xff;
pub const SUBCLASS_ECM: u8 = 0x06;
pub const SUBCLASS_NCM: u8 = 0x0D;

/// Class-specific descriptor types and the functional subtypes inside them.
pub const DT_CS_INTERFACE: u8 = 0x24;
pub const FN_HEADER: u8 = 0x00;
pub const FN_CALL_MANAGEMENT: u8 = 0x01;
pub const FN_ACM: u8 = 0x02;
pub const FN_UNION: u8 = 0x06;
pub const FN_ETHERNET: u8 = 0x0F;
pub const FN_NCM: u8 = 0x1A;

/// Class requests this driver issues.
///
/// SET_ETHERNET_PACKET_FILTER IS NOT OPTIONAL IN PRACTICE. An ECM device comes up with its filter
/// empty and forwards nothing; a driver that configures the alternate setting and waits is a driver
/// whose link is up and silent, which looks exactly like a cable that is not plugged in.
pub const REQ_SET_LINE_CODING: u8 = 0x20;
pub const REQ_GET_LINE_CODING: u8 = 0x21;
pub const REQ_SET_CONTROL_LINE_STATE: u8 = 0x22;
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
	/// The notification endpoint's address, when the control interface publishes one. An adapter
	/// that does not is one whose link state cannot be asked for.
	pub notification: Option<u8>,
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

/// Keep the LOWEST alternate setting that carried a whole bulk pair.
///
/// ONE COPY OF THE RULE, because there are two bindings that need it - the network models and ACM -
/// and the rule is not obvious enough to be written twice: alternate zero of a CDC data interface is
/// the specification's way of saying "not carrying traffic", so the endpoints appear only in a
/// higher setting, and a device that offers several is offering larger buffers rather than different
/// function. A driver that took the first setting it walked past would take whichever the device
/// happened to list first.
type Pair = (u8, u8, u8, u8, u16, u16);
type Candidate = Option<(u8, u8, Option<(u8, u16)>, Option<(u8, u16)>)>;

fn keep_lowest(candidate: &mut Candidate, best: &mut Option<Pair>) {
	if let Some((iface, alt, Some((in_addr, in_mps)), Some((out_addr, out_mps)))) = candidate.take()
		&& best.is_none_or(|(_, have, _, _, _, _)| alt < have)
	{
		*best = Some((iface, alt, in_addr, out_addr, in_mps, out_mps));
	}
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
	let mut notification: Option<u8> = None;
	let mut union_data: Option<u8> = None;
	let mut mac_string: Option<u8> = None;
	let mut max_segment: u16 = 0;
	// The data interface's candidate settings, as (interface, alternate, in, out, in mps, out mps).
	let mut candidate: Candidate = None;
	let mut best: Option<Pair> = None;
	// Which interface the class descriptors that follow belong to.
	let mut current_is_control = false;

	for record in walk.by_ref() {
		match record.kind {
			descriptor::DT_CONFIG => {
				config_value = record.field(5).ok();
			}
			descriptor::DT_INTERFACE => {
				keep_lowest(&mut candidate, &mut best);
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
			// THE NOTIFICATION ENDPOINT, WHICH IS ON THE CONTROL INTERFACE AND NOT THE DATA ONE.
			// It is how an adapter says its link came up or went down, and a driver that never
			// looked for it cannot tell a cable nobody plugged in from a quiet network.
			descriptor::DT_ENDPOINT if current_is_control && notification.is_none() => {
				let (Ok(address), Ok(attributes)) = (record.field(2), record.field(3)) else {
					return Err(NotBindable::Malformed);
				};
				// INTERRUPT AND IN. A control interface with a bulk endpoint on it is not this, and
				// an OUT one is not something an adapter reports through.
				if attributes & 0x03 == 0x03 && address & 0x80 != 0 {
					notification = Some(address);
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
	keep_lowest(&mut candidate, &mut best);
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
	Ok(Binding { model, config_value: config_value.ok_or(NotBindable::Malformed)?, control_interface, notification, data_interface, data_alternate, bulk_in, bulk_out, bulk_in_packet, bulk_out_packet, mac_string, max_segment })
}

/// Decode the MAC address a device publishes as a string descriptor.
///
/// IT IS TWELVE UPPER-CASE HEX CHARACTERS IN UTF-16, which is a specification requirement and not a
/// convention - so a string of any other length, or one with a character that is not hex, is a
/// device that did not answer the question rather than one to guess at. Lower case is accepted
/// because devices ship it and nothing is ambiguous about it.
/// What an ACM adapter's configuration says, and where its parts are.
///
/// THE SAME THREE QUESTIONS THE NETWORK MODELS ASK, of a different subclass: which data interface
/// the union names, which alternate setting its endpoints are in, and where the bulk pair is. What
/// is new is the CAPABILITY BITMAP, because a serial adapter with no UART behind it is a perfectly
/// good byte stream that refuses every line-coding request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AcmBinding {
	pub config_value: u8,
	/// The communications interface, which carries the class descriptors and the notification
	/// endpoint.
	pub control_interface: u8,
	/// The data interface named by the union descriptor, and the setting its endpoints are in.
	pub data_interface: u8,
	pub data_alternate: u8,
	pub bulk_in: u8,
	pub bulk_out: u8,
	pub bulk_in_packet: u16,
	pub bulk_out_packet: u16,
	/// The interrupt endpoint the device reports serial state on, or zero when it published none.
	/// A device without one is still a byte stream; what it cannot do is say the carrier dropped.
	pub notify_in: u8,
	pub notify_packet: u16,
	/// The ACM functional descriptor's capability bitmap, or zero when the device published no such
	/// descriptor - which is itself an answer: it supports none of them.
	pub capabilities: u8,
}

impl AcmBinding {
	/// Whether this device implements `SET_LINE_CODING` / `GET_LINE_CODING` /
	/// `SET_CONTROL_LINE_STATE`, which is bit 1 of the capability bitmap.
	///
	/// ASKED BEFORE THE REQUEST IS SENT, because the alternative is finding out from a STALL - and a
	/// driver that reads a stall here as "this device is broken" discards a working byte stream. An
	/// MCU link has no UART behind the USB and says so exactly this way.
	pub fn supports_line_coding(self) -> bool {
		self.capabilities & 0x02 != 0
	}
}

/// The seven-byte line-coding structure, in the units the wire uses.
///
/// TWO CONVENTIONS SIDE BY SIDE, AND THAT IS THE TRAP. `data_bits` is a COUNT - five, six, seven,
/// eight or sixteen - and `stop_bits` is an ENUMERATION in which 0 means ONE stop bit, 1 means one
/// and a half and 2 means two. A driver that writes the number it means into both asks for one and a
/// half stop bits when it means one: some devices refuse it and others accept it and frame every
/// byte differently, which is a link that works until the first byte of the second frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LineCoding {
	pub rate: u32,
	pub stop_bits: StopBits,
	pub parity: Parity,
	pub data_bits: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StopBits {
	One,
	OneAndAHalf,
	Two,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Parity {
	None,
	Odd,
	Even,
	Mark,
	Space,
}

/// The length of the line-coding structure on the wire.
pub const LINE_CODING_LEN: usize = 7;

impl LineCoding {
	/// 115200 8N1, which is what every one of these adapters is set to when nobody has said
	/// otherwise, and what a device with no UART behind it ignores.
	pub fn default_8n1() -> LineCoding {
		LineCoding { rate: 115_200, stop_bits: StopBits::One, parity: Parity::None, data_bits: 8 }
	}

	/// Encode for `SET_LINE_CODING`. The rate is LITTLE-endian, unlike every SCSI field in this tree
	/// and like every other USB one.
	pub fn encode(self) -> [u8; LINE_CODING_LEN] {
		let rate = self.rate.to_le_bytes();
		let stop = match self.stop_bits {
			StopBits::One => 0,
			StopBits::OneAndAHalf => 1,
			StopBits::Two => 2,
		};
		let parity = match self.parity {
			Parity::None => 0,
			Parity::Odd => 1,
			Parity::Even => 2,
			Parity::Mark => 3,
			Parity::Space => 4,
		};
		[rate[0], rate[1], rate[2], rate[3], stop, parity, self.data_bits]
	}

	/// Decode a `GET_LINE_CODING` answer, or `None` when it is not one: too short, or a field
	/// carrying a value the structure does not define.
	///
	/// A VALUE OUTSIDE THE ENUMERATION IS REFUSED RATHER THAN ROUNDED. Three stop bits is not a
	/// setting, and reading it as "two, probably" is a driver deciding what a device meant.
	pub fn decode(bytes: &[u8]) -> Option<LineCoding> {
		if bytes.len() < LINE_CODING_LEN {
			return None;
		}
		let rate = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
		let stop_bits = match bytes[4] {
			0 => StopBits::One,
			1 => StopBits::OneAndAHalf,
			2 => StopBits::Two,
			_ => return None,
		};
		let parity = match bytes[5] {
			0 => Parity::None,
			1 => Parity::Odd,
			2 => Parity::Even,
			3 => Parity::Mark,
			4 => Parity::Space,
			_ => return None,
		};
		if !matches!(bytes[6], 5 | 6 | 7 | 8 | 16) {
			return None;
		}
		Some(LineCoding { rate, stop_bits, parity, data_bits: bytes[6] })
	}
}

/// The `wValue` of `SET_CONTROL_LINE_STATE`: DTR is bit 0 and RTS is bit 1.
///
/// A BITMAP IN THE SETUP PACKET AND NOT A PAYLOAD. The request carries no data stage at all, so a
/// driver that sends the two lines as bytes sends a transfer the device did not ask for and gets a
/// stall for a request it could have made.
pub fn control_lines(dtr: bool, rts: bool) -> u16 {
	(dtr as u16) | ((rts as u16) << 1)
}

/// How many ACM functions of one configuration this binder will look at.
///
/// A COMPOSITE DEVICE CARRIES SEVERAL, which is what a two-port USB serial adapter is and what the
/// item this binder belongs to asks to be exercised against. Four is a bound and not a guess: the
/// walk is over a descriptor a device wrote, so the number of functions it declares is an input.
pub const MAX_ACM_FUNCTIONS: usize = 4;

/// How many CDC-data interfaces one configuration may declare, as far as this binder looks.
const MAX_DATA_INTERFACES: usize = 8;

/// One ACM function's control half, collected while the walk runs.
#[derive(Clone, Copy)]
struct AcmFunction {
	control: u8,
	union_data: Option<u8>,
	capabilities: u8,
	notify: Option<(u8, u16)>,
}

/// Put a finished data-interface candidate away under ITS OWN INTERFACE NUMBER.
///
/// PER INTERFACE AND NOT ONE BEST IN THE CONFIGURATION, which is the whole difference a composite
/// device makes. `keep_lowest` keeps a single lowest-alternate pair for the whole descriptor, which
/// is right when there is one data interface and wrong the moment there are two: the second
/// function's union names interface three, and the pair kept was interface one's - so either the
/// binder refuses a device that is perfectly good, or it hands one function's control interface the
/// other function's endpoints and the two streams cross with nothing reporting it.
fn keep_lowest_per_interface(candidate: &mut Candidate, data: &mut [Option<Pair>; MAX_DATA_INTERFACES], count: &mut usize) {
	let Some((iface, alt, Some((in_addr, in_mps)), Some((out_addr, out_mps)))) = candidate.take() else {
		return;
	};
	let pair: Pair = (iface, alt, in_addr, out_addr, in_mps, out_mps);
	if let Some(slot) = data.iter_mut().flatten().find(|(have, ..)| *have == iface) {
		if alt < slot.1 {
			*slot = pair;
		}
		return;
	}
	if *count < data.len() {
		data[*count] = Some(pair);
		*count += 1;
	}
}

/// Read one configuration descriptor and decide what to bind as an ACM adapter.
///
/// The FIRST ACM function in the configuration. `bind_acm_nth` is the same walk asking for a later
/// one, which is what a device carrying several serial ports needs.
pub fn bind_acm(config: &[u8]) -> Result<AcmBinding, NotBindable> {
	bind_acm_nth(config, 0)
}

/// The `nth` ACM function of one configuration, counted in the order the descriptor declares them.
///
/// ONE PASS AND NO ASSUMED ORDER, for the reason `bind` above states: several devices put the union
/// descriptor after the interface it names.
///
/// A CONFIGURATION WITH NO `nth` FUNCTION ANSWERS `NoNetworkInterface`, which is the same answer a
/// configuration carrying no ACM function at all gives - and is what lets a caller walk `0, 1, 2...`
/// until it is told there are no more, without a second way of saying the same thing.
pub fn bind_acm_nth(config: &[u8], nth: usize) -> Result<AcmBinding, NotBindable> {
	let mut walk = descriptor::Walk::new(config);
	let mut config_value: Option<u8> = None;
	let mut functions: [Option<AcmFunction>; MAX_ACM_FUNCTIONS] = [None; MAX_ACM_FUNCTIONS];
	let mut found: usize = 0;
	// Which function the class descriptors and the interrupt endpoint that follow belong to.
	let mut current: Option<usize> = None;
	let mut data: [Option<Pair>; MAX_DATA_INTERFACES] = [None; MAX_DATA_INTERFACES];
	let mut data_count: usize = 0;
	let mut candidate: Candidate = None;

	for record in walk.by_ref() {
		match record.kind {
			descriptor::DT_CONFIG => config_value = record.field(5).ok(),
			descriptor::DT_INTERFACE => {
				keep_lowest_per_interface(&mut candidate, &mut data, &mut data_count);
				let (Ok(number), Ok(alternate), Ok(class), Ok(subclass), Ok(protocol)) = (record.field(2), record.field(3), record.field(5), record.field(6), record.field(7)) else {
					return Err(NotBindable::Malformed);
				};
				current = None;
				// THE PROTOCOL IS PART OF THE ANSWER - see `PROTOCOL_VENDOR`. Class and subclass
				// alone accept RNDIS, which is an Ethernet adapter wearing a serial port's
				// identity.
				if class == CLASS_COMMUNICATIONS && subclass == SUBCLASS_ACM && protocol != PROTOCOL_VENDOR {
					current = match functions.iter().position(|slot| matches!(slot, Some(function) if function.control == number)) {
						// AN ALTERNATE SETTING OF A CONTROL INTERFACE ALREADY SEEN is the same
						// function and not a second one.
						Some(index) => Some(index),
						None if found < MAX_ACM_FUNCTIONS => {
							functions[found] = Some(AcmFunction { control: number, union_data: None, capabilities: 0, notify: None });
							found += 1;
							Some(found - 1)
						}
						None => None,
					};
				} else if class == CLASS_COMMUNICATIONS {
					current = functions.iter().position(|slot| matches!(slot, Some(function) if function.control == number));
				} else if class == CLASS_CDC_DATA {
					candidate = Some((number, alternate, None, None));
				}
			}
			DT_CS_INTERFACE => {
				let Some(index) = current else { continue };
				let Ok(subtype) = record.field(2) else { return Err(NotBindable::Malformed) };
				let Some(function) = functions[index].as_mut() else { continue };
				match subtype {
					// The subordinate interface at offset four is the data interface.
					FN_UNION => function.union_data = record.field(4).ok(),
					// The capability bitmap at offset three. A device that published no ACM
					// descriptor supports none of them, which is what zero already says.
					FN_ACM => function.capabilities = record.field(3).unwrap_or(0),
					_ => {}
				}
			}
			descriptor::DT_ENDPOINT => {
				let (Ok(address), Ok(attributes), Ok(packet)) = (record.field(2), record.field(3), record.field16(4)) else {
					return Err(NotBindable::Malformed);
				};
				// THE NOTIFICATION ENDPOINT BELONGS TO THE COMMUNICATIONS INTERFACE, and taking an
				// interrupt endpoint from whichever interface carried one would take a HID
				// function's on a composite adapter.
				if let Some(index) = current
					&& attributes & 0x03 == 0x03
					&& address & 0x80 != 0
					&& let Some(function) = functions[index].as_mut()
				{
					function.notify.get_or_insert((address, packet));
					continue;
				}
				if let Some((_, _, ref mut ep_in, ref mut ep_out)) = candidate
					&& attributes & 0x03 == 0x02
				{
					if address & 0x80 != 0 {
						ep_in.get_or_insert((address, packet));
					} else {
						ep_out.get_or_insert((address, packet));
					}
				}
			}
			_ => {}
		}
	}
	keep_lowest_per_interface(&mut candidate, &mut data, &mut data_count);
	if walk.fault().is_some() {
		return Err(NotBindable::Malformed);
	}

	let function = functions.get(nth).copied().flatten().ok_or(NotBindable::NoNetworkInterface)?;
	// THE UNION NAMES THE DATA INTERFACE, and "the next interface" is what binds the wrong function
	// on a composite device.
	let union_data = function.union_data.ok_or(NotBindable::NoUnion)?;
	if data_count == 0 {
		return Err(NotBindable::NoBulkPair);
	}
	let (data_interface, data_alternate, bulk_in, bulk_out, bulk_in_packet, bulk_out_packet) = data.iter().flatten().find(|(iface, ..)| *iface == union_data).copied().ok_or(NotBindable::NoDataInterface)?;
	let (notify_in, notify_packet) = function.notify.unwrap_or((0, 0));
	Ok(AcmBinding { config_value: config_value.ok_or(NotBindable::Malformed)?, control_interface: function.control, data_interface, data_alternate, bulk_in, bulk_out, bulk_in_packet, bulk_out_packet, notify_in, notify_packet, capabilities: function.capabilities })
}

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

/// What an adapter said on its notification endpoint.
///
/// THE LINK IS THE ONE THING A FRAME PIPE CANNOT SAY. A cable nobody plugged in and a quiet network
/// look identical from the data endpoints - both are a receive transfer that never completes - and
/// this is the only place an adapter distinguishes them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Notification {
	/// The link came up or went down.
	Link { up: bool },
	/// The link's rates changed, in bits per second, downlink then uplink.
	Speed { down: u32, up: u32 },
	/// A notification this driver does not act on. ANSWERED AS ITSELF rather than as a link change:
	/// a driver that read every notification as a link transition reports the link going down when
	/// the adapter said something else entirely.
	Other(u8),
	/// Too short to be a notification at all.
	Malformed,
}

/// The eight-byte header every notification carries.
pub const NOTIFICATION_HEADER_LEN: usize = 8;
pub const NOTIFY_NETWORK_CONNECTION: u8 = 0x00;
pub const NOTIFY_CONNECTION_SPEED_CHANGE: u8 = 0x2A;

/// Decode one notification.
///
/// THE LENGTH FIELD IS THE DEVICE'S CLAIM AND `bytes` IS WHAT ARRIVED. A speed change whose header
/// promises eight bytes of rates in a transfer that carried two is not a slow link, it is a lie -
/// and reading the rates out of it reads past what the controller wrote.
pub fn notification(bytes: &[u8]) -> Notification {
	if bytes.len() < NOTIFICATION_HEADER_LEN {
		return Notification::Malformed;
	}
	let code = bytes[1];
	let value = u16::from_le_bytes([bytes[2], bytes[3]]);
	let length = u16::from_le_bytes([bytes[6], bytes[7]]) as usize;
	match code {
		NOTIFY_NETWORK_CONNECTION => Notification::Link { up: value != 0 },
		NOTIFY_CONNECTION_SPEED_CHANGE => {
			if length < 8 || bytes.len() < NOTIFICATION_HEADER_LEN + 8 {
				return Notification::Malformed;
			}
			let down = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
			let up = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
			Notification::Speed { down, up }
		}
		other => Notification::Other(other),
	}
}
