// The CDC-ACM side of driver.xhci: a serial adapter on the bus, served over the SAME byte-stream
// contract the virtio console port publishes.
//
// THIS MODULE DEFINES NO CONTRACT AND IS NOT ALLOWED TO. What it publishes is `console-stream`, which
// the virtio multiport driver settled and which `drivers::serial_port` implements; what is different
// here is only the TRANSPORT - a bulk pair on a controller's transfer ring instead of a pair of
// virtio queues. `serial_port::Wire` is the seam, and `serial_port::Session` is everything above it,
// so there is one definition of what `again` means and one serve loop rather than two.
//
// THE DESCRIPTOR DECISIONS ARE IN `drivers::cdc` AND ARE HOST-TESTED, including the three that are
// silent when wrong: which data interface the UNION names rather than "the next one", which alternate
// setting actually carries endpoints - zero never does - and the line-coding structure, whose seven
// bytes put a COUNT and an ENUMERATION side by side.
//
// AND WHAT THE DEVICE SUPPORTS IS ASKED BEFORE IT IS TOLD. An MCU link has no UART behind the USB and
// STALLS `SET_LINE_CODING`; a driver that reads that stall as fatal throws away a byte stream that
// works. The ACM functional descriptor's capability bitmap says so in advance, and this module sends
// those requests only where it does.

use alloc::vec::Vec;
use proto::system::Error;
use rt::*;

use crate::usb_hid::Hids;
use crate::{CC_SHORT_PACKET, CC_STALL, CC_SUCCESS, DESC_CONFIG, FEATURE_ENDPOINT_HALT, REQ_CLEAR_FEATURE, REQ_GET_DESCRIPTOR, REQ_SET_CONFIGURATION, RT_ENDPOINT, TRB_CONFIGURE_ENDPOINT, TRB_EV_TRANSFER, TRB_IOC, TRB_NORMAL};
use crate::{Ring, UsbDevice, Xhci};
use crate::{command_and_wait, control_in_req, control_nodata, control_out_req, dma_page, r8, reset_endpoint, w32, wait_transfer};
use drivers::cdc;
use drivers::descriptor;
use drivers::serial_port;

// The standard request that selects an alternate setting, and the recipient it goes to.
const REQ_SET_INTERFACE: u8 = 0x0b;
const RT_INTERFACE_OUT: u8 = 0x01;
// A class request to the communications interface, which is where both ACM requests go.
const RT_CLASS_INTERFACE_OUT: u8 = 0x21;

/// HOW MANY BYTES ONE POSTED RECEIVE MAY TAKE, and why it is not the page.
///
/// A byte stream has no frame boundary, so the only bound on a single transfer is the one this
/// driver chooses - and the chunk that comes out of it is framed into the session's scratch buffer,
/// which `serial_port` sizes from its own receive slot. Two kilobytes is inside that and large
/// enough that an ordinary burst is one transfer rather than four.
pub const RX_BYTES: usize = 2048;

/// One bound CDC-ACM adapter: its two bulk pipes and what the device said it can do.
pub struct Serial {
	// THE SLOT THIS ADAPTER IS ON, kept HERE rather than taken from a `UsbDevice` beside it.
	//
	// A COMPOSITE DEVICE CARRIES SEVERAL ADAPTERS ON ONE SLOT, so "the device this port belongs to"
	// stopped being a one-to-one pairing the moment a second function was bindable - and every
	// operation on a bound port needs exactly one thing from the device, which is this number. The
	// device itself is still held, once, because it owns the pages its control transfers use.
	slot: u32,
	dci_in: u32,
	dci_out: u32,
	ring_in: Ring,
	ring_out: Ring,
	// THE PAGES ARE OWNED AND NOT MERELY MAPPED. A handle dropped on the floor keeps its mapping
	// alive for the life of the process, which looks like it works and is a page this driver can
	// never give back when the adapter is unplugged.
	rx_handle: u64,
	rx_virt: u64,
	rx_phys: u64,
	tx_handle: u64,
	tx_virt: u64,
	tx_phys: u64,
	// WHETHER A RECEIVE IS OUTSTANDING. Posting a second while the first is live would hand the
	// controller two transfers into one buffer, and the second would overwrite bytes the first
	// delivered.
	posted: bool,
	// What the ACM functional descriptor's capability bitmap said. Held because the answer decides
	// whether a later line-coding request is sent at all rather than sent and stalled.
	line_coding: bool,
}

impl Serial {
	/// Whether this adapter implements the line-coding and control-line requests, for a report that
	/// says what was bound rather than only that something was.
	/// The endpoint this adapter's standing receive is posted on.
	///
	/// ASKED FOR BY THE CONTROLLER so a completion for it can be KEPT when it lands during a
	/// synchronous wait, on the same terms as the network adapter's - see `Xhci::serial_pending`.
	pub fn receive_endpoint(&self) -> u32 {
		self.dci_in
	}

	pub fn speaks_line_coding(&self) -> bool {
		self.line_coding
	}

	/// The slot this port's device is on, which is how a port and its device find each other.
	pub fn slot(&self) -> u32 {
		self.slot
	}

	/// Give this adapter's pages and rings back.
	pub fn release(&mut self) {
		self.ring_in.release();
		self.ring_out.release();
		for handle in [&mut self.rx_handle, &mut self.tx_handle] {
			if *handle != 0 {
				close(*handle);
				*handle = 0;
			}
		}
	}
}

/// Walk this device's configurations, bind EVERY CDC-ACM adapter it carries and bring their pipes
/// up. Answers how many are in `out`.
///
/// SEVERAL, BECAUSE ONE DEVICE CAN BE SEVERAL SERIAL PORTS. A two-port USB serial adapter is one
/// composite device with two ACM functions in one configuration, and a driver that stopped at the
/// first one left the second port on the bus with nothing driving it - which looks from outside
/// exactly like a port that does not work.
///
/// ONE WALK AND ONE COMMAND, whatever the count. The configuration descriptors are read once, every
/// function in the chosen configuration is decided from the same bytes before any other control
/// transfer disturbs them, and all their endpoints go into ONE input context and ONE
/// `CONFIGURE_ENDPOINT` - which is also the only way to build it, since a device has one slot
/// context whose Context Entries field has to cover the highest endpoint of all of them.
///
/// QUIET WHEN IT IS NOT AN ACM DEVICE AT ALL, and loud about every other refusal. This is offered
/// every communications-class device before the network models are, and most of them are adapters:
/// a module that announced "not a serial port" for every Ethernet adapter on the bus would be noise
/// in front of the message that matters. A device that IS an ACM adapter and could not be brought up
/// says exactly which step refused it.
///
/// # Safety
/// `dev` is an addressed device this controller owns, and its input context and data page are this
/// driver's mappings.
pub unsafe fn configure_serial(hc: &mut Xhci, dev: &mut UsbDevice, limit: usize, out: &mut Vec<Serial>) -> usize {
	unsafe {
		let mut hids: Hids = Hids::new();
		// A DEVICE MAY OFFER SEVERAL CONFIGURATIONS and the host picks one; a composite adapter that
		// puts its serial function in the second is a device a driver reading index zero decides is
		// not a serial port. The network model already learned this from RNDIS-then-ECM devices.
		let configurations = match control_in_req(hc, &mut hids, dev, 0x80, REQ_GET_DESCRIPTOR, (descriptor::DT_DEVICE as u16) << 8, 0, 18) {
			Some(received) if received >= 18 => r8(dev.data_virt + 17).max(1),
			_ => 1,
		};
		// EVERY FUNCTION OF THE CHOSEN CONFIGURATION, DECIDED BEFORE ANYTHING ELSE IS ASKED OF THE
		// DEVICE. `bind_acm_nth` reads the descriptor where the last control transfer left it, and
		// the next transfer overwrites that page - so the bindings are taken out of it here, while
		// the bytes are still the ones they were read from.
		let mut bound: [Option<cdc::AcmBinding>; cdc::MAX_ACM_FUNCTIONS] = [const { None }; cdc::MAX_ACM_FUNCTIONS];
		let mut count: usize = 0;
		let mut refusal: Option<cdc::NotBindable> = None;
		for index in 0..configurations.min(8) {
			let Some(head) = control_in_req(hc, &mut hids, dev, 0x80, REQ_GET_DESCRIPTOR, DESC_CONFIG << 8 | index as u16, 0, 9) else {
				continue;
			};
			let head_bytes = core::slice::from_raw_parts(dev.data_virt as *const u8, head.min(9) as usize);
			let Some(head_record) = descriptor::Walk::new(head_bytes).next() else { continue };
			if descriptor::check_type(descriptor::DT_CONFIG, head_record.kind).is_err() {
				continue;
			}
			let Ok(total) = head_record.field16(2) else { continue };
			let total = total.min(1024);
			let Some(received) = control_in_req(hc, &mut hids, dev, 0x80, REQ_GET_DESCRIPTOR, DESC_CONFIG << 8 | index as u16, 0, total) else {
				continue;
			};
			let Ok(total) = descriptor::check_transfer(total, received) else { continue };
			let config_bytes = core::slice::from_raw_parts(dev.data_virt as *const u8, total as usize);
			for nth in 0..limit.min(cdc::MAX_ACM_FUNCTIONS) {
				match cdc::bind_acm_nth(config_bytes, nth) {
					Ok(one) => {
						bound[count] = Some(one);
						count += 1;
					}
					// NO `nth` FUNCTION IS THE END OF THIS CONFIGURATION AND NOT A REFUSAL, unless
					// nothing was found at all - in which case it is the quiet answer below.
					Err(cdc::NotBindable::NoNetworkInterface) => break,
					Err(why) => {
						refusal = Some(why);
						break;
					}
				}
			}
			if count > 0 {
				break;
			}
		}
		if count == 0 {
			// THE QUIET REFUSAL, and the only one. Every other device offered here is a
			// communications-class device that is not a serial port, and the module after this one
			// says what it is or is not.
			if matches!(refusal, Some(cdc::NotBindable::NoNetworkInterface) | None) {
				return 0;
			}
			print(b"driver.xhci: a CDC-ACM interface is on the bus and could not be bound - ");
			print(match refusal {
				Some(cdc::NotBindable::NoUnion) => b"no union functional descriptor".as_slice(),
				Some(cdc::NotBindable::NoDataInterface) => b"its union names a data interface that is not in the configuration",
				Some(cdc::NotBindable::NoBulkPair) => b"no alternate setting carries a bulk pair",
				_ => b"its configuration descriptor is malformed",
			});
			print(b"\n");
			return 0;
		}

		// THE RINGS FIRST, because the input context has to carry their addresses and a ring that
		// could not be allocated is a port that is not brought up rather than one whose endpoint
		// context points at nothing.
		let mut rings: [Option<(Ring, Ring)>; cdc::MAX_ACM_FUNCTIONS] = [const { None }; cdc::MAX_ACM_FUNCTIONS];
		for slot in rings.iter_mut().take(count) {
			let Some(ring_in) = Ring::new() else { break };
			let Some(ring_out) = Ring::new() else {
				let mut ring_in = ring_in;
				ring_in.release();
				break;
			};
			*slot = Some((ring_in, ring_out));
		}
		let ready = rings.iter().take(count).take_while(|slot| slot.is_some()).count();
		if ready == 0 {
			return 0;
		}

		core::ptr::write_bytes(dev.in_virt as *mut u8, 0, 4096);
		let mut add: u32 = 1;
		let mut entries: u32 = 0;
		for index in 0..ready {
			let one = bound[index].as_ref().expect("a binding that was counted");
			let dci_in: u32 = (one.bulk_in & 0x0f) as u32 * 2 + 1;
			let dci_out: u32 = (one.bulk_out & 0x0f) as u32 * 2;
			add |= 1 << dci_in | 1 << dci_out;
			entries = entries.max(dci_in).max(dci_out);
		}
		((dev.in_virt + 4) as *mut u32).write_volatile(add);
		let slot_ctx: u64 = dev.in_virt + hc.ctx_size;
		(slot_ctx as *mut u32).write_volatile(entries << 27 | dev.speed << 20 | dev.route);
		((slot_ctx + 4) as *mut u32).write_volatile(dev.port << 16);
		// ENDPOINT TYPE SIX IS BULK IN AND TWO IS BULK OUT. THE NOTIFICATION ENDPOINT IS NOT
		// CONFIGURED, and that is a decision rather than an omission: what it carries is serial
		// state - carrier, ring, break, framing errors - and there is nothing above this driver that
		// takes them. An interrupt pipe configured for a consumer that does not exist is bandwidth
		// the bus reserves for nobody and an endpoint whose transfers have to be reaped anyway.
		for index in 0..ready {
			let one = bound[index].as_ref().expect("a binding that was counted");
			let (ring_in, ring_out) = rings[index].as_ref().expect("a ring pair that was made");
			let dci_in: u32 = (one.bulk_in & 0x0f) as u32 * 2 + 1;
			let dci_out: u32 = (one.bulk_out & 0x0f) as u32 * 2;
			let contexts: [(u32, u32, u32, &Ring); 2] = [(dci_in, one.bulk_in_packet as u32, 6u32, ring_in), (dci_out, one.bulk_out_packet as u32, 2u32, ring_out)];
			for &(dci, mps, ep_type, ring) in &contexts {
				let ep_ctx: u64 = dev.in_virt + (1 + dci as u64) * hc.ctx_size;
				((ep_ctx + 4) as *mut u32).write_volatile(mps << 16 | ep_type << 3 | 3 << 1);
				((ep_ctx + 8) as *mut u32).write_volatile((ring.phys | ring.cycle as u64) as u32);
				((ep_ctx + 12) as *mut u32).write_volatile((ring.phys >> 32) as u32);
				((ep_ctx + 16) as *mut u32).write_volatile(mps);
			}
		}
		if command_and_wait(hc, dev.in_phys, 0, TRB_CONFIGURE_ENDPOINT << 10 | dev.slot << 24).is_none() {
			print(b"driver.xhci: the CDC-ACM adapter's bulk endpoints were refused by the controller\n");
			return 0;
		}
		// ONE SET_CONFIGURATION FOR THE DEVICE AND NOT ONE PER PORT: the request selects a
		// configuration, and issuing it again would reset every interface in it - including the one
		// a port brought up a moment ago.
		let config_value = bound[0].as_ref().expect("a binding that was counted").config_value;
		if control_nodata(hc, &mut hids, dev, 0x00, REQ_SET_CONFIGURATION, config_value as u16, 0).is_none() {
			print(b"driver.xhci: the CDC-ACM adapter refused SET_CONFIGURATION\n");
			return 0;
		}

		let mut brought_up: usize = 0;
		for index in 0..ready {
			let one = bound[index].as_ref().expect("a binding that was counted");
			// THE ALTERNATE SETTING THAT HAS THE ENDPOINTS, which is the request a CDC driver
			// forgets: setting zero carries none by definition, so everything above is correct and
			// nothing ever arrives.
			if control_nodata(hc, &mut hids, dev, RT_INTERFACE_OUT, REQ_SET_INTERFACE, one.data_alternate as u16, one.data_interface as u16).is_none() {
				print(b"driver.xhci: the CDC-ACM adapter refused the alternate setting that carries its endpoints\n");
				continue;
			}
			// AND THE LINE, ONLY IF THE DEVICE SAID IT HAS ONE. A stall here is recovered from
			// rather than fatal for the same reason the capability is read at all: the byte stream
			// is the product, and a link with no UART behind it is a working one that cannot be
			// configured.
			if one.supports_line_coding() {
				let coding = cdc::LineCoding::default_8n1().encode();
				core::ptr::copy_nonoverlapping(coding.as_ptr(), dev.data_virt as *mut u8, coding.len());
				if control_out_req(hc, &mut hids, dev, RT_CLASS_INTERFACE_OUT, cdc::REQ_SET_LINE_CODING, 0, one.control_interface as u16, coding.len() as u16).is_none() {
					print(b"driver.xhci: the CDC-ACM adapter stalled SET_LINE_CODING although it declared the capability - the byte stream is kept and the line is left as it was\n");
				}
				// DTR AND RTS ARE A BITMAP IN wValue AND THE REQUEST HAS NO DATA STAGE. Raising both
				// is what tells the other end somebody is here; a device with no modem lines
				// ignores it.
				if control_nodata(hc, &mut hids, dev, RT_CLASS_INTERFACE_OUT, cdc::REQ_SET_CONTROL_LINE_STATE, cdc::control_lines(true, true), one.control_interface as u16).is_none() {
					print(b"driver.xhci: the CDC-ACM adapter stalled SET_CONTROL_LINE_STATE - the byte stream is kept\n");
				}
			}
			let (Some((rx_handle, rx_virt, rx_phys)), Some((tx_handle, tx_virt, tx_phys))) = (dma_page(), dma_page()) else {
				break;
			};
			let (ring_in, ring_out) = rings[index].take().expect("a ring pair that was made");
			out.push(Serial { slot: dev.slot, dci_in: (one.bulk_in & 0x0f) as u32 * 2 + 1, dci_out: (one.bulk_out & 0x0f) as u32 * 2, ring_in, ring_out, rx_handle, rx_virt, rx_phys, tx_handle, tx_virt, tx_phys, posted: false, line_coding: one.supports_line_coding() });
			brought_up += 1;
		}
		// A RING PAIR WHOSE PORT DID NOT COME UP IS GIVEN BACK rather than left to the process's
		// lifetime - the same rule `Serial::release` keeps for the ones that did.
		for slot in rings.iter_mut() {
			if let Some((ring_in, ring_out)) = slot.as_mut() {
				ring_in.release();
				ring_out.release();
			}
			*slot = None;
		}
		brought_up
	}
}

/// HOW MANY SERIAL PORTS THIS CONTROLLER PUBLISHES.
///
/// TWO, AND THE NUMBER IS A PUBLICATION AND NOT A CAPACITY. Each bound port is a `console-bytes`
/// provider of its own, and the provider list a driver offers is POSITIONAL - a token is a place in
/// it - so the count has to be fixed when the binding is made rather than when a device arrives.
/// Two is what the fixture this was proved against carries and what a two-port adapter is; a third
/// port is one more entry in that list and one more name, decided the day a machine has one.
pub const MAX_PORTS: usize = 2;

/// The serial adapters this controller has bound, and the devices they are on.
///
/// TWO LISTS AND NOT A LIST OF PAIRS, because the relation is not one to one: a composite adapter
/// is ONE device carrying TWO ports, and pairing each port with a device would either duplicate a
/// `UsbDevice` - which owns pages, so a copy is a double free waiting to happen - or force the two
/// ports of one device to be modelled as something other than two ports.
pub struct Serials {
	pub devices: Vec<UsbDevice>,
	pub ports: Vec<Serial>,
}

impl Serials {
	pub const fn new() -> Serials {
		Serials { devices: Vec::new(), ports: Vec::new() }
	}

	pub fn len(&self) -> usize {
		self.ports.len()
	}

	/// The device a port is on, and the port, borrowed together.
	///
	/// BY SLOT, because that is what a port knows about its device and what an event carries.
	pub fn pair(&mut self, index: usize) -> Option<(&mut UsbDevice, &mut Serial)> {
		let slot = self.ports.get(index)?.slot();
		let port = self.ports.get_mut(index)?;
		let device = self.devices.iter_mut().find(|device| device.slot == slot)?;
		Some((device, port))
	}
}

impl Default for Serials {
	fn default() -> Serials {
		Serials::new()
	}
}

/// Post the standing receive, if one is not already outstanding.
pub fn post_receive(hc: &Xhci, serial: &mut Serial) {
	unsafe {
		if serial.posted {
			return;
		}
		serial.ring_in.push(serial.rx_phys, RX_BYTES as u32, TRB_NORMAL << 10 | TRB_IOC);
		w32(hc.db + serial.slot as u64 * 4, serial.dci_in);
		serial.posted = true;
	}
}

/// Handle one event against a bound adapter, answering the bytes it delivered.
///
/// THE BYTES ARE COPIED OUT BEFORE THE NEXT TRANSFER IS POSTED, which is not a nicety: the receive
/// page is the one the controller writes into, so a caller holding a slice of it while the next
/// transfer runs is reading a buffer the device is filling.
///
/// A ZERO-LENGTH TRANSFER IS A PACKET BOUNDARY AND NOT A FAILURE. A device sends one when what it
/// had to say happened to be a multiple of the endpoint's packet size; there are no bytes in it and
/// the receive is simply posted again.
pub fn handle_serial_event(hc: &mut Xhci, dev: &mut UsbDevice, serial: &mut Serial, status: u32, control: u32, out: &mut Vec<u8>) -> bool {
	unsafe {
		if control >> 10 & 0x3f != TRB_EV_TRANSFER {
			return false;
		}
		if control >> 24 != serial.slot || (control >> 16 & 0x1f) != serial.dci_in {
			return false;
		}
		serial.posted = false;
		let code: u32 = status >> 24;
		if code == CC_STALL {
			let dequeue: u64 = serial.ring_in.phys + serial.ring_in.index * 16 | serial.ring_in.cycle as u64;
			let mut none: Hids = Hids::new();
			reset_endpoint(hc, &mut none, serial.slot, serial.dci_in, dequeue);
			let address: u16 = 0x80 | (serial.dci_in >> 1) as u16;
			let _ = control_nodata(hc, &mut none, dev, RT_ENDPOINT, REQ_CLEAR_FEATURE, FEATURE_ENDPOINT_HALT, address);
			post_receive(hc, serial);
			return false;
		}
		if code != CC_SUCCESS && code != CC_SHORT_PACKET {
			post_receive(hc, serial);
			return false;
		}
		// The event's residual is what was NOT transferred, so what arrived is the difference.
		let moved: usize = RX_BYTES.saturating_sub((status & 0x00ff_ffff) as usize);
		if moved == 0 {
			post_receive(hc, serial);
			return false;
		}
		out.clear();
		out.reserve(moved);
		for i in 0..moved {
			out.push(r8(serial.rx_virt + i as u64));
		}
		post_receive(hc, serial);
		true
	}
}

/// Put bytes on the adapter's bulk OUT pipe and wait, bounded, for the controller to take them.
fn transmit(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, serial: &mut Serial, bytes: &[u8]) -> bool {
	unsafe {
		if bytes.is_empty() || bytes.len() > RX_BYTES {
			return false;
		}
		core::ptr::copy_nonoverlapping(bytes.as_ptr(), serial.tx_virt as *mut u8, bytes.len());
		serial.ring_out.push(serial.tx_phys, bytes.len() as u32, TRB_NORMAL << 10 | TRB_IOC);
		w32(hc.db + serial.slot as u64 * 4, serial.dci_out);
		match wait_transfer(hc, hids, serial.slot, serial.dci_out) {
			Some(CC_SUCCESS) | Some(CC_SHORT_PACKET) => true,
			Some(CC_STALL) => {
				let dequeue: u64 = serial.ring_out.phys + serial.ring_out.index * 16 | serial.ring_out.cycle as u64;
				reset_endpoint(hc, hids, serial.slot, serial.dci_out, dequeue);
				let address: u16 = (serial.dci_out >> 1) as u16;
				let _ = control_nodata(hc, hids, dev, RT_ENDPOINT, REQ_CLEAR_FEATURE, FEATURE_ENDPOINT_HALT, address);
				false
			}
			_ => false,
		}
	}
}

/// THE ADAPTER SEEN AS A WIRE, which is the whole of what `serial_port`'s contract needs from a
/// transport.
///
/// IT BORROWS THE CONTROLLER AND THE DEVICE because a USB write is not a property of the endpoint: it
/// is a transfer on a ring the controller owns, rung through a doorbell the controller owns, whose
/// completion arrives on the controller's event ring. The virtio port can be a `Port` on its own
/// because its queue IS the device; this one cannot.
pub struct Adapter<'a> {
	pub hc: &'a mut Xhci,
	pub hids: &'a mut Hids,
	// THE DEVICE, STILL, ALTHOUGH THE PORT CARRIES ITS OWN SLOT. Clearing a halted endpoint is a
	// CONTROL transfer, which runs on the device's control ring and through its data page - so a
	// port that can recover from a stall cannot be addressed by its slot number alone. Everything
	// else here is the port's.
	pub dev: &'a mut UsbDevice,
	pub serial: &'a mut Serial,
}

impl serial_port::Wire for Adapter<'_> {
	unsafe fn write(&mut self, payload: &[u8], bind: &drivers::common::Bind, bootstrap: u64) -> Result<u32, Error> {
		// NOT ATTACHED AND A LENGTH THE CONTRACT REFUSES ARE THE SERVICE'S ANSWERS AND NOT THIS
		// ONE. What is refused here is an EMPTY write, which is a transfer with nothing in it.
		if payload.is_empty() {
			return Err(Error::Invalid);
		}
		// CHUNKED, BECAUSE THE CONTRACT'S BOUND IS NOT THIS TRANSPORT'S. `console-stream` admits a
		// write of `MAX_WRITE` and says so in the `attach` reply; one bulk transfer here carries a
		// page at most. A byte stream has no frame boundaries, so several transfers ARE the write -
		// and refusing the difference instead would be a driver advertising a bound it does not
		// honour, which is worse than a slow write.
		let mut sent: usize = 0;
		while sent < payload.len() {
			// A HOST THAT IS SLOW TO READ HAS NOT STOPPED THIS CONTROL PATH, and it is asked before
			// EVERY chunk rather than once: a large write is exactly the case where a driver that
			// answers its supervisor only at the start goes quiet for long enough to be killed for
			// somebody else's slowness.
			if !drivers::common::answer_ping(bootstrap, bind) {
				return Err(Error::Closed);
			}
			let end: usize = (sent + RX_BYTES).min(payload.len());
			if !transmit(self.hc, self.hids, self.dev, self.serial, &payload[sent..end]) {
				// WHAT WENT IS REPORTED, which is what a byte stream's answer means: the caller
				// knows how much to offer again. Answering `Io` after a partial write would make the
				// consumer send the whole thing twice, and the device would have both.
				return if sent == 0 { Err(Error::Io) } else { Ok(sent as u32) };
			}
			sent = end;
		}
		Ok(sent as u32)
	}
}
