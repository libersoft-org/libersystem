// The CDC network side of driver.xhci: an Ethernet adapter on the bus, served over the SAME frame
// transport NetworkService already consumes from `virtio-net`.
//
// THE DRIVER CARRIES NO NETWORK STACK, exactly as the virtio one does not. It brings the adapter up,
// moves frames in both directions and publishes its MAC and the link's segment size; every decision
// about what those frames MEAN belongs to NetworkService. That is what "feed the same
// capability-scoped frame transport" asks for, and it is also what keeps a second network stack from
// appearing inside a USB driver.
//
// THE DESCRIPTOR DECISIONS ARE IN `drivers::cdc` AND ARE HOST-TESTED, including the two that are
// silent when wrong: which alternate setting of the data interface actually has endpoints - zero
// never does, and a driver that configures zero waits on pipes that do not exist - and the NCM
// transfer block's datagram table, which is a list of offsets the DEVICE chose into a buffer the
// DEVICE sized.
//
// WHAT IS HERE AND WHAT IS NOT. ECM is implemented and runs against a device. NCM's framing is
// implemented in `drivers::cdc` and host-tested, and is NOT reachable from here yet: there is no NCM
// device model in this harness to bind against, and a binding path nothing executes is a claim
// rather than a driver. The module refuses an NCM device by name rather than pretending to drive it.

use alloc::vec::Vec;
use rt::*;

use crate::usb_hid::Hids;
use crate::{CC_SHORT_PACKET, CC_STALL, CC_SUCCESS, DESC_CONFIG, FEATURE_ENDPOINT_HALT, REQ_CLEAR_FEATURE, REQ_GET_DESCRIPTOR, REQ_SET_CONFIGURATION, RT_ENDPOINT, TRB_CONFIGURE_ENDPOINT, TRB_EV_TRANSFER, TRB_IOC, TRB_NORMAL};
use crate::{Ring, UsbDevice, Xhci};
use crate::{command_and_wait, control_in_req, control_nodata, dma_page, r8, reset_endpoint, w32, wait_transfer};
use drivers::cdc;
use drivers::descriptor;

// The standard request that selects an alternate setting, and the recipient it goes to.
const REQ_SET_INTERFACE: u8 = 0x0b;
const RT_INTERFACE_OUT: u8 = 0x01;
// A class request to the communications interface.
const RT_CLASS_INTERFACE_OUT: u8 = 0x21;
// A string descriptor request, and the language id every device that has strings supports.
const DESC_STRING: u16 = 0x0300;
const LANG_EN_US: u16 = 0x0409;

// The largest frame this module moves. An Ethernet frame is 1514 bytes plus the four-byte CRC a
// device may or may not append; a page holds that with room to spare and is the unit everything
// else here allocates in.
const FRAME_BYTES: usize = 2048;

/// A bound CDC network adapter.
// The notification endpoint's ring and the page it delivers into.
struct Notify {
	dci: u32,
	ring: Ring,
	virt: u64,
	phys: u64,
	posted: bool,
	// What the adapter last said its link was, so a repeat is not reported as a transition.
	up: Option<bool>,
}

pub struct Net {
	dci_in: u32,
	dci_out: u32,
	ring_in: Ring,
	ring_out: Ring,
	// THE NOTIFICATION ENDPOINT, WHEN THE ADAPTER HAS ONE. It is the only place a link transition
	// is announced: from the data endpoints, a cable nobody plugged in and a quiet network are the
	// same thing - a receive transfer that never completes.
	note: Option<Notify>,
	// The receive page, which a standing IN transfer fills, and the transmit page.
	rx_virt: u64,
	rx_phys: u64,
	rx_handle: u64,
	tx_virt: u64,
	tx_phys: u64,
	tx_handle: u64,
	/// Whether a receive transfer is outstanding.
	posted: bool,
	pub mac: [u8; 6],
	pub mtu: u16,
}

impl Net {
	/// Give this adapter's pages and rings back.
	pub fn release(&mut self) {
		self.ring_in.release();
		self.ring_out.release();
		if let Some(note) = self.note.as_mut() {
			note.ring.release();
		}
		for handle in [&mut self.rx_handle, &mut self.tx_handle] {
			if *handle != 0 {
				close(*handle);
				*handle = 0;
			}
		}
	}

	/// The device-context index of the receive endpoint, so the controller can recognise a
	/// completion for it during a synchronous wait and keep it rather than discarding it.
	/// The notification endpoint's device-context index, for an event that arrived on it.
	pub fn notify_endpoint(&self) -> Option<u32> {
		self.note.as_ref().map(|note| note.dci)
	}

	pub fn receive_endpoint(&self) -> u32 {
		self.dci_in
	}

	/// The MAC and MTU message this driver leads every connection with - the same eleven bytes
	/// `virtio-net` sends, because it is the same contract and a second spelling of it is a second
	/// contract.
	pub fn hello(&self) -> [u8; 11] {
		let mut out = [0u8; 11];
		out[..3].copy_from_slice(b"MAC");
		out[3..9].copy_from_slice(&self.mac);
		out[9..11].copy_from_slice(&self.mtu.to_le_bytes());
		out
	}
}

/// Read one string descriptor into the device's control page and answer its bytes.
unsafe fn string_descriptor(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, index: u8, out: &mut [u8]) -> Option<usize> {
	unsafe {
		if index == 0 {
			return None;
		}
		let received = control_in_req(hc, hids, dev, 0x80, REQ_GET_DESCRIPTOR, DESC_STRING | index as u16, LANG_EN_US, out.len() as u16)?;
		let received = (received as usize).min(out.len());
		// THE DECLARED LENGTH IS CHECKED AGAINST WHAT ARRIVED, for the same reason every other
		// descriptor read here is: the page is reused, so the bytes past a short answer are the
		// previous transfer's.
		if received < 2 {
			return None;
		}
		for (i, byte) in out[..received].iter_mut().enumerate() {
			*byte = r8(dev.data_virt + i as u64);
		}
		let declared = out[0] as usize;
		if declared > received {
			return None;
		}
		Some(declared)
	}
}

/// Configure an addressed device as a CDC Ethernet adapter, or answer None when it is not one.
pub unsafe fn configure_network(hc: &mut Xhci, dev: &mut UsbDevice) -> Option<Net> {
	unsafe {
		let mut hids: Hids = Hids::new();
		// HOW MANY CONFIGURATIONS THE DEVICE HAS, and the reason this is read at all.
		//
		// A USB device may offer SEVERAL CONFIGURATIONS and the host picks one; almost every device
		// in this tree offers exactly one, so every other walk here reads index zero and stops. An
		// Ethernet adapter is the exception, and it is not an unusual one: the common ones offer
		// RNDIS as configuration one and CDC-ECM as configuration two, so a driver that reads index
		// zero reads the configuration it does not speak and concludes the device is not an adapter.
		// That is exactly what this did, and the message it printed - "no ECM or NCM interface" -
		// was true of the descriptor it had and false of the device.
		let configurations = match control_in_req(hc, &mut hids, dev, 0x80, REQ_GET_DESCRIPTOR, (descriptor::DT_DEVICE as u16) << 8, 0, 18) {
			Some(received) if received >= 18 => r8(dev.data_virt + 17).max(1),
			_ => 1,
		};
		let mut chosen: Option<(cdc::Binding, u16)> = None;
		let mut refusal: Option<cdc::NotBindable> = None;
		for index in 0..configurations.min(8) {
			// The head first, for its total length, then the whole descriptor - and the whole of it
			// has to have arrived before it is walked.
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
			match cdc::bind(config_bytes) {
				Ok(bound) => {
					chosen = Some((bound, index as u16));
					break;
				}
				Err(why) => refusal = Some(why),
			}
		}
		let Some((bound, _index)) = chosen else {
			// SAID, AND WITH THE REASON. A class module that answers `None` for every device it is
			// offered is indistinguishable from one that is broken, and this is the branch a
			// communications-class device that is not an adapter this driver can bind lands in.
			print(b"driver.xhci: a class-2 device is not a CDC adapter this driver binds - ");
			print(match refusal {
				Some(cdc::NotBindable::NoNetworkInterface) | None => b"no configuration of it carries an ECM or NCM interface".as_slice(),
				Some(cdc::NotBindable::NoUnion) => b"no union functional descriptor",
				Some(cdc::NotBindable::NoDataInterface) => b"its union names a data interface that is not in the configuration",
				Some(cdc::NotBindable::NoBulkPair) => b"no alternate setting carries a bulk pair",
				Some(cdc::NotBindable::NoEthernet) => b"no Ethernet functional descriptor",
				Some(cdc::NotBindable::Malformed) => b"its configuration descriptor is malformed",
			});
			print(b"\n");
			return None;
		};
		if bound.model != cdc::Model::Ecm {
			// SAID RATHER THAN SILENTLY SKIPPED. An NCM adapter is a device this tree can decode and
			// cannot yet drive, and a driver that leaves it addressed without a word is one nobody
			// can tell from a driver that did not recognise it.
			print(b"driver.xhci: a CDC-NCM adapter is on the bus and this controller drives ECM only - it is left unbound\n");
			return None;
		}

		// The MAC the device publishes, read before anything is configured: an adapter that cannot
		// say what address it answers to is one NetworkService cannot build a stack on.
		let mut string = [0u8; 64];
		let Some(len) = string_descriptor(hc, &mut hids, dev, bound.mac_string, &mut string) else {
			print(b"driver.xhci: the CDC adapter did not answer the string descriptor its Ethernet descriptor names\n");
			return None;
		};
		let Some(mac) = cdc::mac_from_string(&string[..len]) else {
			print(b"driver.xhci: the CDC adapter's MAC string is not twelve hex characters\n");
			return None;
		};

		let dci_in: u32 = (bound.bulk_in & 0x0f) as u32 * 2 + 1;
		let dci_out: u32 = (bound.bulk_out & 0x0f) as u32 * 2;
		let ring_in: Ring = Ring::new()?;
		let ring_out: Ring = Ring::new()?;
		// AN ADAPTER WITHOUT ONE IS BOUND EXACTLY AS BEFORE, which is why every part of this is an
		// option rather than a requirement: a device that publishes no notification endpoint is one
		// whose link state cannot be asked for, not one this driver refuses.
		let note_dci: Option<u32> = bound.notification.map(|address| (address & 0x0f) as u32 * 2 + 1);
		let note_ring: Option<Ring> = match note_dci {
			Some(_) => Some(Ring::new()?),
			None => None,
		};
		let note_page = match note_dci {
			Some(_) => dma_page(),
			None => None,
		};
		core::ptr::write_bytes(dev.in_virt as *mut u8, 0, 4096);
		let note_bit = note_dci.map_or(0, |dci| 1u32 << dci);
		((dev.in_virt + 4) as *mut u32).write_volatile(1 | 1 << dci_in | 1 << dci_out | note_bit);
		let entries: u32 = dci_in.max(dci_out).max(note_dci.unwrap_or(0));
		let slot_ctx: u64 = dev.in_virt + hc.ctx_size;
		(slot_ctx as *mut u32).write_volatile(entries << 27 | dev.speed << 20 | dev.route);
		((slot_ctx + 4) as *mut u32).write_volatile(dev.port << 16);
		// ENDPOINT TYPE SEVEN IS INTERRUPT IN, where six is bulk IN and two is bulk OUT. A driver
		// that configured the notification endpoint as bulk asks the controller for a pipe the
		// device does not have, and the configure command is refused for the whole adapter.
		let mut contexts: [(u32, u32, u32, &Ring); 3] = [(dci_in, bound.bulk_in_packet as u32, 6u32, &ring_in), (dci_out, bound.bulk_out_packet as u32, 2u32, &ring_out), (dci_in, 0, 6, &ring_in)];
		let mut context_count = 2usize;
		if let (Some(dci), Some(ring)) = (note_dci, note_ring.as_ref()) {
			contexts[2] = (dci, NOTIFY_BYTES as u32, 7u32, ring);
			context_count = 3;
		}
		for &(dci, mps, ep_type, ring) in &contexts[..context_count] {
			let ep_ctx: u64 = dev.in_virt + (1 + dci as u64) * hc.ctx_size;
			((ep_ctx + 4) as *mut u32).write_volatile(mps << 16 | ep_type << 3 | 3 << 1);
			((ep_ctx + 8) as *mut u32).write_volatile((ring.phys | ring.cycle as u64) as u32);
			((ep_ctx + 12) as *mut u32).write_volatile((ring.phys >> 32) as u32);
			((ep_ctx + 16) as *mut u32).write_volatile(mps);
		}
		if command_and_wait(hc, dev.in_phys, 0, TRB_CONFIGURE_ENDPOINT << 10 | dev.slot << 24).is_none() {
			print(b"driver.xhci: the CDC adapter's bulk endpoints were refused by the controller\n");
			return None;
		}
		if control_nodata(hc, &mut hids, dev, 0x00, REQ_SET_CONFIGURATION, bound.config_value as u16, 0).is_none() {
			print(b"driver.xhci: the CDC adapter refused SET_CONFIGURATION\n");
			return None;
		}
		// AND THE ALTERNATE SETTING THAT HAS THE ENDPOINTS. This is the request a CDC driver forgets:
		// the data interface starts on setting zero, which the specification defines as carrying no
		// endpoints at all, so everything above is correct and nothing ever arrives.
		if control_nodata(hc, &mut hids, dev, RT_INTERFACE_OUT, REQ_SET_INTERFACE, bound.data_alternate as u16, bound.data_interface as u16).is_none() {
			print(b"driver.xhci: the CDC adapter refused the alternate setting that carries its endpoints\n");
			return None;
		}
		// AND THE PACKET FILTER, which is the other one. An ECM device comes up forwarding nothing;
		// a link that is up and silent looks exactly like a cable that is not plugged in.
		if control_nodata(hc, &mut hids, dev, RT_CLASS_INTERFACE_OUT, cdc::REQ_SET_ETHERNET_PACKET_FILTER, cdc::FILTER_DIRECTED | cdc::FILTER_BROADCAST | cdc::FILTER_ALL_MULTICAST, bound.control_interface as u16).is_none() {
			print(b"driver.xhci: the CDC adapter refused the packet filter, so it would forward nothing\n");
			return None;
		}

		// WHETHER THIS ADAPTER CAN BE ASKED ABOUT ITS LINK AT ALL, said once. An adapter with no
		// notification endpoint is one whose link state nothing can report; one with it is where a
		// link transition would come from.
		match bound.notification {
			Some(address) => {
				let mut line = *b"driver.xhci: the CDC adapter publishes a notification endpoint at 00\n";
				let digits = b"0123456789abcdef";
				let at = line.len() - 3;
				line[at] = digits[(address >> 4) as usize];
				line[at + 1] = digits[(address & 0x0F) as usize];
				print(&line);
			}
			None => print(b"driver.xhci: the CDC adapter publishes no notification endpoint - its link state cannot be asked for\n"),
		}

		let (rx_handle, rx_virt, rx_phys) = dma_page()?;
		let (tx_handle, tx_virt, tx_phys) = dma_page()?;
		// THE DEVICE'S OWN SEGMENT SIZE, LESS THE HEADER, IS THE MTU - and a device that published
		// nothing usable gets the ordinary Ethernet number rather than a zero NetworkService would
		// size its buffers by.
		let mtu = match bound.max_segment {
			0 | 1..=14 => 1500,
			segment => (segment - 14).min(FRAME_BYTES as u16 - 14),
		};
		let note = match (note_dci, note_ring, note_page) {
			(Some(dci), Some(ring), Some((_, virt, phys))) => Some(Notify { dci, ring, virt, phys, posted: false, up: None }),
			_ => None,
		};
		Some(Net { dci_in, dci_out, ring_in, ring_out, note, rx_virt, rx_phys, rx_handle, tx_virt, tx_phys, tx_handle, posted: false, mac, mtu })
	}
}

/// The largest notification this driver reads: the eight-byte header and a speed change's rates.
const NOTIFY_BYTES: usize = cdc::NOTIFICATION_HEADER_LEN + 8;

/// The same number, for the caller that turns a transfer event's RESIDUAL length into how many
/// bytes arrived. A residual is what was NOT transferred.
pub const NOTIFY_WINDOW: usize = NOTIFY_BYTES;

/// Post the standing notification transfer, if none is outstanding.
///
/// A QUEUE NOTHING DRAINS IS A QUEUE THAT FILLS. An interrupt endpoint the driver configured and
/// never read from is one the device eventually stops being able to write to, so this is posted
/// beside the receive transfer rather than only when somebody asks about the link.
pub fn post_notification(hc: &Xhci, dev: &UsbDevice, net: &mut Net) {
	unsafe {
		let Some(note) = net.note.as_mut() else { return };
		if note.posted {
			return;
		}
		note.ring.push(note.phys, NOTIFY_BYTES as u32, TRB_NORMAL << 10 | TRB_IOC);
		w32(hc.db + dev.slot as u64 * 4, note.dci);
		note.posted = true;
	}
}

/// Read one notification the adapter delivered, and say when its link CHANGED.
///
/// A REPEAT IS NOT A TRANSITION. An adapter that says "connected" twice has not reconnected, and a
/// driver reporting both would have a log that reads like a flapping cable.
pub fn handle_notification(net: &mut Net, arrived: usize) -> Option<cdc::Notification> {
	unsafe {
		let note = net.note.as_mut()?;
		note.posted = false;
		let mut bytes = [0u8; NOTIFY_BYTES];
		let take = arrived.min(NOTIFY_BYTES);
		for (index, byte) in bytes[..take].iter_mut().enumerate() {
			*byte = ((note.virt + index as u64) as *const u8).read_volatile();
		}
		match cdc::notification(&bytes[..take]) {
			cdc::Notification::Link { up } => {
				if note.up == Some(up) {
					return None;
				}
				note.up = Some(up);
				Some(cdc::Notification::Link { up })
			}
			other => Some(other),
		}
	}
}

/// Post the standing receive transfer, if none is outstanding.
pub fn post_receive(hc: &Xhci, dev: &UsbDevice, net: &mut Net) {
	unsafe {
		if net.posted {
			return;
		}
		net.ring_in.push(net.rx_phys, FRAME_BYTES as u32, TRB_NORMAL << 10 | TRB_IOC);
		w32(hc.db + dev.slot as u64 * 4, net.dci_in);
		net.posted = true;
	}
}

/// Handle one event against a bound adapter, answering the frame it delivered.
///
/// A FRAME IS COPIED OUT BEFORE THE NEXT TRANSFER IS POSTED, which is not a nicety: the receive page
/// is the one the controller writes into, so a caller holding a slice of it while the next transfer
/// runs is reading a buffer the device is filling.
pub fn handle_net_event(hc: &mut Xhci, dev: &mut UsbDevice, net: &mut Net, status: u32, control: u32, out: &mut Vec<u8>) -> bool {
	unsafe {
		if control >> 10 & 0x3f != TRB_EV_TRANSFER {
			return false;
		}
		if control >> 24 != dev.slot || (control >> 16 & 0x1f) != net.dci_in {
			return false;
		}
		net.posted = false;
		let code: u32 = status >> 24;
		if code == CC_STALL {
			let dequeue: u64 = net.ring_in.phys + net.ring_in.index * 16 | net.ring_in.cycle as u64;
			let mut none: Hids = Hids::new();
			reset_endpoint(hc, &mut none, dev.slot, net.dci_in, dequeue);
			let address: u16 = 0x80 | (net.dci_in >> 1) as u16;
			let _ = control_nodata(hc, &mut none, dev, RT_ENDPOINT, REQ_CLEAR_FEATURE, FEATURE_ENDPOINT_HALT, address);
			post_receive(hc, dev, net);
			return false;
		}
		if code != CC_SUCCESS && code != CC_SHORT_PACKET {
			post_receive(hc, dev, net);
			return false;
		}
		// The event's residual is what was NOT transferred, so the frame is the difference.
		let moved: usize = FRAME_BYTES.saturating_sub((status & 0x00ff_ffff) as usize);
		// A ZERO-LENGTH TRANSFER IS A PACKET BOUNDARY AND NOT A FRAME. An ECM device sends one when
		// a frame happens to be a multiple of the endpoint's packet size, and forwarding it upward
		// would hand NetworkService an empty Ethernet frame to parse.
		if moved < 14 {
			post_receive(hc, dev, net);
			return false;
		}
		out.clear();
		out.reserve(moved);
		for i in 0..moved {
			out.push(r8(net.rx_virt + i as u64));
		}
		post_receive(hc, dev, net);
		true
	}
}

/// Send one frame. Synchronous, like every other bulk transfer this controller makes.
pub fn transmit(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, net: &mut Net, frame: &[u8]) -> bool {
	unsafe {
		if frame.is_empty() || frame.len() > FRAME_BYTES {
			return false;
		}
		core::ptr::copy_nonoverlapping(frame.as_ptr(), net.tx_virt as *mut u8, frame.len());
		net.ring_out.push(net.tx_phys, frame.len() as u32, TRB_NORMAL << 10 | TRB_IOC);
		w32(hc.db + dev.slot as u64 * 4, net.dci_out);
		match wait_transfer(hc, hids, dev.slot, net.dci_out) {
			Some(CC_SUCCESS) | Some(CC_SHORT_PACKET) => true,
			Some(CC_STALL) => {
				let dequeue: u64 = net.ring_out.phys + net.ring_out.index * 16 | net.ring_out.cycle as u64;
				reset_endpoint(hc, hids, dev.slot, net.dci_out, dequeue);
				let address: u16 = (net.dci_out >> 1) as u16;
				let _ = control_nodata(hc, hids, dev, RT_ENDPOINT, REQ_CLEAR_FEATURE, FEATURE_ENDPOINT_HALT, address);
				false
			}
			_ => false,
		}
	}
}
