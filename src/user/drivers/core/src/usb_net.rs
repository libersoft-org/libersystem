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
// THE TWO MODELS SHARE EVERYTHING BUT THE FRAMING (2026-10-15). ECM puts one Ethernet frame in a
// transfer. NCM packs several into a transfer block with a table of offsets, and first asks the host
// to agree how: `GET_NTB_PARAMETERS` says how large a block the device sends and how it wants the
// host's laid out, and where the device offers a choice the host makes it - 16-bit blocks, and no
// larger than the page they land in. The blocks themselves are `drivers::cdc`'s: one built for each
// frame that goes out, and one that came in split into exactly the frames its table names, all of
// them or none. What NetworkService sees is the same in both: the MAC and the MTU, then one Ethernet
// frame per message. NCM's oracle is the host kernel's own `usb_f_ncm` as a gadget, with a process on
// the gadget's network interface answering at the Ethernet level.
//
// AND NCM TAKES NO MEMORY ECM DOES NOT. The receive page holds a whole block because the device is
// told a page is the most it may send, and the transmit page holds the one block a frame goes out in -
// the same three pages and three rings the class budget charges either model for.

use alloc::vec::Vec;
use rt::*;

use crate::usb_hid::Hids;
use crate::{CC_SHORT_PACKET, CC_STALL, CC_SUCCESS, DESC_CONFIG, FEATURE_ENDPOINT_HALT, REQ_CLEAR_FEATURE, REQ_GET_DESCRIPTOR, REQ_SET_CONFIGURATION, RT_ENDPOINT, TRB_CONFIGURE_ENDPOINT, TRB_EV_TRANSFER, TRB_IOC, TRB_NORMAL};
use crate::{Ring, UsbDevice, Xhci};
use crate::{command_and_wait, control_in_req, control_nodata, control_out_req, dma_page, r8, reset_endpoint, w32, wait_transfer};
use drivers::cdc;
use drivers::common;
use drivers::descriptor;

// The standard request that selects an alternate setting, and the recipient it goes to.
const REQ_SET_INTERFACE: u8 = 0x0b;
const RT_INTERFACE_OUT: u8 = 0x01;
// A class request to the communications interface, and one it answers.
const RT_CLASS_INTERFACE_OUT: u8 = 0x21;
const RT_CLASS_INTERFACE_IN: u8 = 0xa1;
// A string descriptor request, and the language id every device that has strings supports.
const DESC_STRING: u16 = 0x0300;
const LANG_EN_US: u16 = 0x0409;

// The largest frame this module moves. An Ethernet frame is 1514 bytes plus the four-byte CRC a
// device may or may not append; a page holds that with room to spare and is the unit everything
// else here allocates in.
const FRAME_BYTES: usize = 2048;

// THE PAGE EVERY BUFFER HERE IS, and so the largest transfer block either direction may carry: an NCM
// device is told it may send no more, and the host builds no more.
const PAGE_BYTES: usize = 4096;

/// A bound CDC network adapter.
// The notification endpoint's ring and the page it delivers into.
struct Notify {
	dci: u32,
	ring: Ring,
	// The page's handle, kept so detach can give the page back.
	handle: u64,
	virt: u64,
	phys: u64,
	posted: bool,
	// What the adapter last said its link was, so a repeat is not reported as a transition.
	up: Option<bool>,
	// THE ENDPOINT'S SERVICE INTERVAL IN CLOCK TICKS, and the tick the next read may be posted at - see
	// `post_notification`.
	every: u64,
	due: u64,
}

// WHAT AN NCM ADAPTER ADDS TO THE PLUMBING ECM SHARES WITH IT, all of it agreed at bind: how the blocks
// this host sends are laid out, and how long a block the device may send - which is the length the
// standing receive is posted for. The rest is counting: the sequence number the next block carries,
// the blocks refused whole, and whether a block has carried more than one datagram yet, so the first
// one that does is said once rather than every time.
struct Ncm {
	out: cdc::NtbOut,
	in_size: u32,
	sequence: u16,
	refused: u64,
	packed: bool,
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
	// AN NCM ADAPTER'S FRAMING, and `None` for ECM: whether a transfer is one frame or a block of them.
	ncm: Option<Ncm>,
	pub mac: [u8; 6],
	pub mtu: u16,
}

/// What one receive completion delivered: the bytes copied out of the receive page, and where each
/// frame is in them.
///
/// ONE FRAME FOR ECM AND AS MANY AS THE DEVICE PACKED FOR NCM, which is the whole difference between
/// the two models as the service loop sees it - each frame goes up as a message of its own. Held across
/// the loop and reused, as the frame buffer it replaced was: this runs once per arriving transfer.
#[derive(Default)]
pub struct Received {
	bytes: Vec<u8>,
	frames: Vec<cdc::Datagram>,
}

impl Received {
	/// The frames, in the order the device sent them.
	pub fn frames(&self) -> impl Iterator<Item = &[u8]> {
		self.frames.iter().filter_map(|frame| self.bytes.get(frame.at as usize..frame.at as usize + frame.len as usize))
	}
}

impl Net {
	/// Give this adapter's pages and rings back.
	pub fn release(&mut self) {
		self.ring_in.release();
		self.ring_out.release();
		if let Some(note) = self.note.as_mut() {
			note.ring.release();
			if note.handle != 0 {
				close(note.handle);
				note.handle = 0;
			}
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

	// HOW LONG THE STANDING RECEIVE IS POSTED FOR: a frame for ECM, and for NCM exactly the block size
	// the device was left at or told - never the whole page, because a block of exactly that size ends
	// with nothing after it and a longer receive would take the next block into this one.
	fn receive_bytes(&self) -> usize {
		self.ncm.as_ref().map_or(FRAME_BYTES, |ncm| ncm.in_size as usize)
	}

	// EVERY RING AND PAGE THIS ADAPTER NEEDS, or none of them: an allocation that fails gives back what
	// the ones before it took. `note` is the notification endpoint's DCI and service interval, for an
	// adapter that has one.
	unsafe fn take(dci_in: u32, dci_out: u32, note: Option<(u32, u64)>, mac: [u8; 6], mtu: u16) -> Option<Net> {
		unsafe {
			let ring_in = Ring::new();
			let ring_out = Ring::new();
			let rx = dma_page();
			let tx = dma_page();
			let note_ring = note.and_then(|_| Ring::new());
			let note_page = note.and_then(|_| dma_page());
			match (ring_in, ring_out, rx, tx, note, note_ring, note_page) {
				(Some(ring_in), Some(ring_out), Some((rx_handle, rx_virt, rx_phys)), Some((tx_handle, tx_virt, tx_phys)), note, note_ring, note_page) if note.is_none() || (note_ring.is_some() && note_page.is_some()) => {
					let note = match (note, note_ring, note_page) {
						(Some((dci, every)), Some(ring), Some((handle, virt, phys))) => Some(Notify { dci, ring, handle, virt, phys, posted: false, up: None, every, due: 0 }),
						_ => None,
					};
					Some(Net { dci_in, dci_out, ring_in, ring_out, note, rx_virt, rx_phys, rx_handle, tx_virt, tx_phys, tx_handle, posted: false, ncm: None, mac, mtu })
				}
				(ring_in, ring_out, rx, tx, _, note_ring, note_page) => {
					for mut ring in [ring_in, ring_out, note_ring].into_iter().flatten() {
						ring.release();
					}
					for (handle, _, _) in [rx, tx, note_page].into_iter().flatten() {
						close(handle);
					}
					None
				}
			}
		}
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
		// AN ADAPTER WITHOUT ONE IS BOUND EXACTLY AS BEFORE, which is why every part of this is an
		// option rather than a requirement: a device that publishes no notification endpoint is one
		// whose link state cannot be asked for, not one this driver refuses.
		let note_dci: Option<u32> = bound.notification.map(|address| (address & 0x0f) as u32 * 2 + 1);
		// AND ITS INTERVAL, which is the endpoint context's and not only the descriptor's: left at zero it asks the
		// controller to poll the endpoint every microframe.
		let note_interval = crate::classes::interrupt_interval(dev.speed, u32::from(bound.notification_interval));
		// THE DEVICE'S OWN SEGMENT SIZE, LESS THE HEADER, IS THE MTU - and a device that published
		// nothing usable gets the ordinary Ethernet number rather than a zero NetworkService would
		// size its buffers by.
		let mtu = match bound.max_segment {
			0 | 1..=14 => 1500,
			segment => (segment - 14).min(FRAME_BYTES as u16 - 14),
		};

		// EVERY RING AND PAGE BEFORE THE CONTROLLER IS TOLD ABOUT ANY OF THEM, and ALL OF THEM GIVEN BACK
		// on any failure from here on. The class budget is given back at detach on the assumption that
		// the memory went with it; an attach that failed part way and kept its pages - an adapter whose
		// endpoints were refused, one that refused its configuration - held them for the life of the
		// driver with nothing charged for them.
		let note = note_dci.map(|dci| (dci, interval_ticks(note_interval)));
		let mut net = Net::take(dci_in, dci_out, note, mac, mtu)?;
		let configured = 'configure: {
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
			let mut contexts: [(u32, u32, u32, u32, &Ring); 3] = [(dci_in, bound.bulk_in_packet as u32, 6u32, 0, &net.ring_in), (dci_out, bound.bulk_out_packet as u32, 2u32, 0, &net.ring_out), (dci_in, 0, 6, 0, &net.ring_in)];
			let mut context_count = 2usize;
			if let Some(note) = net.note.as_ref() {
				contexts[2] = (note.dci, NOTIFY_BYTES as u32, 7u32, note_interval, &note.ring);
				context_count = 3;
			}
			for &(dci, mps, ep_type, interval, ring) in &contexts[..context_count] {
				let ep_ctx: u64 = dev.in_virt + (1 + dci as u64) * hc.ctx_size;
				(ep_ctx as *mut u32).write_volatile(interval << 16);
				((ep_ctx + 4) as *mut u32).write_volatile(mps << 16 | ep_type << 3 | 3 << 1);
				((ep_ctx + 8) as *mut u32).write_volatile((ring.phys | ring.cycle as u64) as u32);
				((ep_ctx + 12) as *mut u32).write_volatile((ring.phys >> 32) as u32);
				((ep_ctx + 16) as *mut u32).write_volatile(mps);
			}
			if command_and_wait(hc, dev.in_phys, 0, TRB_CONFIGURE_ENDPOINT << 10 | dev.slot << 24).is_none() {
				print(b"driver.xhci: the CDC adapter's bulk endpoints were refused by the controller\n");
				break 'configure false;
			}
			if control_nodata(hc, &mut hids, dev, 0x00, REQ_SET_CONFIGURATION, bound.config_value as u16, 0).is_none() {
				print(b"driver.xhci: the CDC adapter refused SET_CONFIGURATION\n");
				break 'configure false;
			}
			// AN NCM ADAPTER AGREES ITS BLOCKS BEFORE ITS DATA INTERFACE CARRIES ANYTHING - see `negotiate_ncm`.
			if bound.model == cdc::Model::Ncm {
				let Some(ncm) = negotiate_ncm(hc, &mut hids, dev, &bound, mtu as usize + cdc::ETHERNET_HEADER) else {
					break 'configure false;
				};
				net.ncm = Some(ncm);
			}
			// AND THE ALTERNATE SETTING THAT HAS THE ENDPOINTS. This is the request a CDC driver forgets:
			// the data interface starts on setting zero, which the specification defines as carrying no
			// endpoints at all, so everything above is correct and nothing ever arrives.
			if control_nodata(hc, &mut hids, dev, RT_INTERFACE_OUT, REQ_SET_INTERFACE, bound.data_alternate as u16, bound.data_interface as u16).is_none() {
				print(b"driver.xhci: the CDC adapter refused the alternate setting that carries its endpoints\n");
				break 'configure false;
			}
			// AND THE PACKET FILTER, which is the other one. An ECM device comes up forwarding nothing;
			// a link that is up and silent looks exactly like a cable that is not plugged in. AN NCM
			// FUNCTION TAKES IT ONLY IF IT SAYS SO, and one that does not forwards by its own default -
			// sending it anyway is a stall on a device that is working.
			if bound.model == cdc::Model::Ncm && bound.ncm_capabilities & cdc::NCM_CAN_FILTER == 0 {
				print(b"driver.xhci: the CDC-NCM adapter takes no packet filter, so it forwards by its own default\n");
			} else if control_nodata(hc, &mut hids, dev, RT_CLASS_INTERFACE_OUT, cdc::REQ_SET_ETHERNET_PACKET_FILTER, cdc::FILTER_DIRECTED | cdc::FILTER_BROADCAST | cdc::FILTER_ALL_MULTICAST, bound.control_interface as u16).is_none() {
				print(b"driver.xhci: the CDC adapter refused the packet filter, so it would forward nothing\n");
				break 'configure false;
			}
			true
		};
		if !configured {
			net.release();
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
		Some(net)
	}
}

/// Agree an NCM adapter's transfer blocks, or say why it cannot be driven.
///
/// ALL OF IT WHILE THE DATA INTERFACE CARRIES NOTHING. The specification lets the block format change
/// only in the data interface's resting setting, and SELECTING that setting is how NCM resets a
/// function. So the resting setting is selected first, whatever a previous host left the function in -
/// a host driver the device was bound to before this one, a controller that was reset under it - what
/// the device then says is its default, and the input size is agreed before a block can be sent at any
/// other.
///
/// THE REQUESTS ARE THE ONES A HOST OWES A 16-BIT NCM FUNCTION AND NO MORE: `GET_NTB_PARAMETERS`, its
/// answer checked before anything is built on it; `SET_NTB_FORMAT` only to a device that offers 32-bit
/// blocks as well, because only then is the format a choice somebody could have made differently; and
/// `SET_NTB_INPUT_SIZE` only when the device would otherwise send blocks longer than the page they land
/// in. The CRC mode is left at its reset default - no CRC - and a device that appends one anyway sends
/// blocks whose tables say "NCM1", which the receive side refuses by name and counts.
///
/// `longest` is the longest frame the link carries: a device whose parameters admit no block that holds
/// one is refused here rather than at the first full-size frame NetworkService sends.
unsafe fn negotiate_ncm(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, bound: &cdc::Binding, longest: usize) -> Option<Ncm> {
	unsafe {
		let interface = bound.control_interface as u16;
		if control_nodata(hc, hids, dev, RT_INTERFACE_OUT, REQ_SET_INTERFACE, 0, bound.data_interface as u16).is_none() {
			print(b"driver.xhci: the CDC-NCM adapter refused its data interface's resting setting, which is how NCM is reset\n");
			return None;
		}
		let mut answer = [0u8; cdc::NTB_PARAMETERS_LEN];
		let Some(received) = control_in_req(hc, hids, dev, RT_CLASS_INTERFACE_IN, cdc::REQ_GET_NTB_PARAMETERS, 0, interface, answer.len() as u16) else {
			print(b"driver.xhci: the CDC-NCM adapter did not answer GET_NTB_PARAMETERS\n");
			return None;
		};
		// WHAT ARRIVED AND NOT WHAT WAS ASKED FOR: the page is reused, so the bytes past a short answer
		// are the previous transfer's - and a short answer is refused below as one.
		let received = (received as usize).min(answer.len());
		for (i, byte) in answer[..received].iter_mut().enumerate() {
			*byte = r8(dev.data_virt + i as u64);
		}
		let agreed = cdc::ntb_parameters(&answer[..received]).and_then(|parameters| cdc::NtbOut::new(&parameters, bound.bulk_out_packet, longest, PAGE_BYTES).map(|out| (parameters, out)));
		let (parameters, out) = match agreed {
			Ok(agreed) => agreed,
			Err(why) => {
				print(b"driver.xhci: the CDC-NCM adapter's transfer blocks are not ones this driver can frame - ");
				print(match why {
					cdc::NtbRefusal::Short => b"its GET_NTB_PARAMETERS answer is short".as_slice(),
					cdc::NtbRefusal::No16Bit => b"it offers no 16-bit blocks",
					cdc::NtbRefusal::InSize => b"its largest IN block is under the 2048 bytes the specification makes the floor",
					cdc::NtbRefusal::Divisor => b"its OUT datagram divisor is zero or its remainder is not less than it",
					cdc::NtbRefusal::Alignment => b"its OUT table alignment is not a power of two of at least four",
					cdc::NtbRefusal::OutSize => b"no OUT block it takes holds one frame of the link's size within a page",
				});
				print(b"\n");
				return None;
			}
		};
		if parameters.formats & cdc::NTB_FORMATS_32 != 0 && control_nodata(hc, hids, dev, RT_CLASS_INTERFACE_OUT, cdc::REQ_SET_NTB_FORMAT, cdc::NTB_FORMAT_16, interface).is_none() {
			print(b"driver.xhci: the CDC-NCM adapter offers 32-bit blocks and refused to send 16-bit ones\n");
			return None;
		}
		let (in_size, ask) = cdc::ntb_input_size(parameters.in_max, PAGE_BYTES as u32)?;
		if let Some(size) = ask {
			let size = size.to_le_bytes();
			core::ptr::copy_nonoverlapping(size.as_ptr(), dev.data_virt as *mut u8, size.len());
			if control_out_req(hc, hids, dev, RT_CLASS_INTERFACE_OUT, cdc::REQ_SET_NTB_INPUT_SIZE, 0, interface, size.len() as u16).is_none() {
				print(b"driver.xhci: the CDC-NCM adapter refused a page as the largest block it may send, and its own largest is more\n");
				return None;
			}
		}
		let mut line: common::Bounded<160> = common::Bounded::new();
		line.push(b"driver.xhci: CDC-NCM adapter - 16-bit transfer blocks of up to ");
		line.decimal(u64::from(in_size));
		line.push(b" bytes in (it offered ");
		line.decimal(u64::from(parameters.in_max));
		line.push(b") and ");
		line.decimal(out.most as u64);
		line.push(b" out\n");
		print(line.as_bytes());
		Some(Ncm { out, in_size, sequence: 0, refused: 0, packed: false })
	}
}

/// The largest notification this driver reads: the eight-byte header and a speed change's rates.
const NOTIFY_BYTES: usize = cdc::NOTIFICATION_HEADER_LEN + 8;

/// The same number, for the caller that turns a transfer event's RESIDUAL length into how many
/// bytes arrived. A residual is what was NOT transferred.
pub const NOTIFY_WINDOW: usize = NOTIFY_BYTES;

// An endpoint-context interval - the exponent of a period of 125 us microframes - in clock ticks, at least one.
fn interval_ticks(exponent: u32) -> u64 {
	let micros = 125u64 << exponent.min(15);
	micros.saturating_mul(TICKS_PER_SECOND).div_ceil(1_000_000).max(1)
}

/// Post the standing notification transfer, if none is outstanding - AND NOT BEFORE THE ENDPOINT'S
/// INTERVAL HAS PASSED SINCE THE LAST ONE CAME BACK.
///
/// A QUEUE NOTHING DRAINS IS A QUEUE THAT FILLS. An interrupt endpoint the driver configured and
/// never read from is one the device eventually stops being able to write to, so this is posted
/// beside the receive transfer rather than only when somebody asks about the link.
///
/// THE INTERVAL IS KEPT HERE TOO, and not left to the controller. QEMU's adapter answers every read
/// with its connection state and leaves the pacing to the host controller, and its xHCI does not pace
/// an interrupt endpoint at all: a read posted the moment the last one came back came back at once,
/// some six thousand times a second, and the controller's interrupt kept a core busy for as long as
/// the adapter was plugged in - found as a kernel test whose timed waits woke late by seconds while
/// an audio stream played. On a controller that keeps the interval this costs nothing: a read never
/// comes back sooner than one interval after the last.
pub fn post_notification(hc: &Xhci, dev: &UsbDevice, net: &mut Net) {
	unsafe {
		let Some(note) = net.note.as_mut() else { return };
		if note.posted || clock() < note.due {
			return;
		}
		note.ring.push(note.phys, NOTIFY_BYTES as u32, TRB_NORMAL << 10 | TRB_IOC);
		w32(hc.db + dev.slot as u64 * 4, note.dci);
		note.posted = true;
	}
}

/// The tick the notification endpoint's next read is due at, while none is posted - what the
/// service loop's retry timer is armed for.
pub fn notification_due(net: &Net) -> Option<u64> {
	net.note.as_ref().filter(|note| !note.posted).map(|note| note.due)
}

/// Read one notification the adapter delivered, and say when its link CHANGED.
///
/// A REPEAT IS NOT A TRANSITION. An adapter that says "connected" twice has not reconnected, and a
/// driver reporting both would have a log that reads like a flapping cable.
pub fn handle_notification(net: &mut Net, arrived: usize) -> Option<cdc::Notification> {
	unsafe {
		let note = net.note.as_mut()?;
		note.posted = false;
		note.due = clock() + note.every;
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
		net.ring_in.push(net.rx_phys, net.receive_bytes() as u32, TRB_NORMAL << 10 | TRB_IOC);
		w32(hc.db + dev.slot as u64 * 4, net.dci_in);
		net.posted = true;
	}
}

/// Handle one event against a bound adapter, answering whether it delivered frames into `out`.
///
/// THE TRANSFER IS COPIED OUT BEFORE THE NEXT ONE IS POSTED, which is not a nicety: the receive page
/// is the one the controller writes into, so a caller holding a slice of it while the next transfer
/// runs is reading a buffer the device is filling. And an NCM block is split from the COPY, so the
/// table is read from the same bytes its offsets index, not from a page that may have moved on.
pub fn handle_net_event(hc: &mut Xhci, dev: &mut UsbDevice, net: &mut Net, status: u32, control: u32, out: &mut Received) -> bool {
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
		// The event's residual is what was NOT transferred, so the transfer is the difference - from
		// the length THIS adapter's receive was posted with, which is a frame for ECM and a block for NCM.
		let moved: usize = net.receive_bytes().saturating_sub((status & 0x00ff_ffff) as usize);
		out.bytes.clear();
		out.frames.clear();
		// A ZERO-LENGTH TRANSFER IS A PACKET BOUNDARY AND NOT A FRAME. An ECM device sends one when
		// a frame happens to be a multiple of the endpoint's packet size, and forwarding it upward
		// would hand NetworkService an empty Ethernet frame to parse. For NCM it is not a block either,
		// and not a malformed one: there is nothing in it to have got wrong.
		if moved == 0 || (net.ncm.is_none() && moved < cdc::ETHERNET_HEADER) {
			post_receive(hc, dev, net);
			return false;
		}
		out.bytes.reserve(moved);
		for i in 0..moved {
			out.bytes.push(r8(net.rx_virt + i as u64));
		}
		post_receive(hc, dev, net);
		let Some(ncm) = net.ncm.as_mut() else {
			out.frames.push(cdc::Datagram { at: 0, len: moved as u16 });
			return true;
		};
		match cdc::ncm_frames(&out.bytes, FRAME_BYTES, &mut out.frames) {
			Ok(_) => {
				// THE FIRST BLOCK THAT CARRIED SEVERAL FRAMES, said once - which is the line that shows the
				// device is packing and this side is splitting, rather than a link that happens to carry
				// one frame per block.
				if out.frames.len() > 1 && !ncm.packed {
					ncm.packed = true;
					let mut line: common::Bounded<128> = common::Bounded::new();
					line.push(b"driver.xhci: a CDC-NCM transfer block carried ");
					line.decimal(out.frames.len() as u64);
					line.push(b" datagrams - each goes up as a frame of its own\n");
					print(line.as_bytes());
				}
				!out.frames.is_empty()
			}
			Err(fault) => {
				// DROPPED WHOLE AND COUNTED, and said at the first and at every doubling after it: a device
				// that sends one bad block is worth a line, and one that sends nothing else must not turn the
				// log into a copy of the link.
				ncm.refused += 1;
				if ncm.refused.is_power_of_two() {
					let mut line: common::Bounded<192> = common::Bounded::new();
					line.push(b"driver.xhci: the CDC-NCM adapter sent a malformed transfer block (");
					line.push(match fault {
						cdc::BlockFault::Short => b"too short for its header".as_slice(),
						cdc::BlockFault::Signature => b"not NCMH, or a table that is not NCM0",
						cdc::BlockFault::Length => b"it claims more than arrived",
						cdc::BlockFault::Pointer => b"its table is outside it",
						cdc::BlockFault::Datagram => b"a datagram runs past it",
						cdc::BlockFault::Chain => b"its tables do not end",
						cdc::BlockFault::Frame => b"a datagram that is not a frame",
					});
					line.push(b") - dropped whole, ");
					line.decimal(ncm.refused);
					line.push(b" so far\n");
					print(line.as_bytes());
				}
				false
			}
		}
	}
}

/// Send one frame. Synchronous, like every other bulk transfer this controller makes.
pub fn transmit(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, net: &mut Net, frame: &[u8]) -> bool {
	unsafe {
		if frame.is_empty() || frame.len() > FRAME_BYTES {
			return false;
		}
		let longest = net.mtu as usize + cdc::ETHERNET_HEADER;
		let len = match net.ncm.as_mut() {
			None => {
				core::ptr::copy_nonoverlapping(frame.as_ptr(), net.tx_virt as *mut u8, frame.len());
				frame.len()
			}
			// ONE BLOCK A FRAME, laid out as the device asked. A frame shorter than an Ethernet header or
			// longer than the link's segment is refused HERE: the device's parser refuses such a datagram by
			// dropping the whole block it is in, which from this side is a frame that silently never arrived.
			Some(ncm) => {
				if frame.len() < cdc::ETHERNET_HEADER || frame.len() > longest {
					return false;
				}
				let page = core::slice::from_raw_parts_mut(net.tx_virt as *mut u8, PAGE_BYTES);
				let Some(len) = cdc::ncm_block(ncm.sequence, frame, &ncm.out, page) else { return false };
				ncm.sequence = ncm.sequence.wrapping_add(1);
				len
			}
		};
		net.ring_out.push(net.tx_phys, len as u32, TRB_NORMAL << 10 | TRB_IOC);
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
