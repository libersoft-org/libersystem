// The USB Audio Class side of driver.xhci: an isochronous sink and the walk that chose it.
//
// ISOCHRONOUS IS A TRANSFER TYPE NOTHING ELSE HERE USES, and that is most of this module. The bus
// RESERVES BANDWIDTH for one and delivers late rather than not at all: there is no retry, no stall
// to clear and no completion code that means "try again". A driver that treated a short or missing
// completion the way it treats a bulk one would be waiting for an answer the transport is defined
// not to give.
//
// WHAT IT PLAYS IS DECIDED IN `drivers::uac` AND HELD BY FIXTURES: the format descriptor, the
// alternate setting that carries an endpoint at all, and the arithmetic that turns one period of
// this system's wire into the packets an endpoint's size allows.

use rt::*;

use crate::usb_hid::Hids;
use crate::{CC_SHORT_PACKET, CC_SUCCESS, DESC_CONFIG, REQ_GET_DESCRIPTOR, REQ_SET_CONFIGURATION, TRB_CONFIGURE_ENDPOINT, TRB_IOC};
use crate::{Ring, UsbDevice, Xhci};
use crate::{command_and_wait, control_in_req, control_nodata, dma_page, r8, w32, wait_transfer};
use drivers::descriptor;
use drivers::uac;

const REQ_SET_INTERFACE: u8 = 0x0b;
const RT_INTERFACE_OUT: u8 = 0x01;

/// The isochronous TRB type, and the endpoint type for an isochronous OUT pipe.
///
/// ONE AND FIVE ARE OUT AND IN, and six and two are the bulk pair every other module here uses. A
/// driver that configured an isochronous endpoint as bulk asks the controller for a pipe the device
/// does not have; one that posted a Normal TRB on an isochronous ring posts a transfer the
/// controller has no schedule for.
const TRB_ISOCH: u32 = 5;
const EP_TYPE_ISOCH_OUT: u32 = 1;

/// Start this transfer as soon as the schedule allows, rather than naming a frame.
///
/// A FRAME ID IS A PROMISE ABOUT WHEN, and a driver that names one has to know what frame the
/// controller is on and how far ahead it may schedule. `SIA` is the transport saying "you choose",
/// which for a sink whose consumer paces it is the right answer: the period arrives when it arrives.
const TRB_START_ISOCH_ASAP: u32 = 1 << 31;

/// A bound audio sink.
pub struct Audio {
	dci: u32,
	ring: Ring,
	/// The page one period is staged in before the controller reads it.
	virt: u64,
	phys: u64,
	binding: uac::Binding,
}

impl Audio {
	pub fn release(&mut self) {
		self.ring.release();
	}

	/// What this sink is, for the line the driver reports itself with.
	pub fn describes(&self) -> (u8, u8, u16) {
		(self.binding.format.channels, self.binding.format.bits, self.binding.max_packet)
	}
}

/// Walk a device's configurations for an audio sink this system's wire can drive, and configure it.
pub unsafe fn configure_audio(hc: &mut Xhci, dev: &mut UsbDevice) -> Option<Audio> {
	unsafe {
		let mut hids: Hids = Hids::new();
		let configurations = match control_in_req(hc, &mut hids, dev, 0x80, REQ_GET_DESCRIPTOR, (descriptor::DT_DEVICE as u16) << 8, 0, 18) {
			Some(received) if received >= 18 => r8(dev.data_virt + 17).max(1),
			_ => 1,
		};
		let mut chosen: Option<uac::Binding> = None;
		let mut refusal: Option<uac::NotBindable> = None;
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
			match uac::bind(config_bytes) {
				Ok(bound) => {
					chosen = Some(bound);
					break;
				}
				Err(why) => refusal = Some(why),
			}
		}
		let Some(bound) = chosen else {
			// SAID, AND WITH THE REASON, for the reason the CDC module says its own: a class module
			// that answers `None` for every device it is offered is indistinguishable from a broken
			// one.
			print(b"driver.xhci: this device is not an audio sink this driver binds - ");
			print(match refusal {
				Some(uac::NotBindable::NoAudioControl) | None => b"no configuration of it carries an audio-control interface".as_slice(),
				Some(uac::NotBindable::NoUsableFormat) => b"no alternate setting offers 48 kHz stereo 16-bit, and this driver refuses rather than resamples",
				Some(uac::NotBindable::Malformed) => b"its configuration descriptor is malformed",
				Some(uac::NotBindable::TooMany) => b"its configuration describes more than this bounded walk follows",
			});
			print(b"\n");
			return None;
		};

		if control_nodata(hc, &mut hids, dev, 0x00, REQ_SET_CONFIGURATION, bound.config_value as u16, 0).is_none() {
			print(b"driver.xhci: the audio device refused its configuration\n");
			return None;
		}
		// THE ALTERNATE SETTING IS THE WHOLE POINT. Setting zero is the ZERO-BANDWIDTH one by
		// specification - it carries no endpoint at all - so a driver that configured the device and
		// stopped has a streaming interface with nothing on it and a stream that never starts. The
		// CDC data interface has the same trap and it is written down there too.
		if control_nodata(hc, &mut hids, dev, RT_INTERFACE_OUT, REQ_SET_INTERFACE, bound.alternate as u16, bound.streaming_interface as u16).is_none() {
			print(b"driver.xhci: the audio device refused the alternate setting that carries its endpoint\n");
			return None;
		}

		let dci: u32 = (bound.endpoint & 0x0f) as u32 * 2;
		let ring: Ring = Ring::new()?;
		let (_handle, virt, phys) = dma_page()?;
		core::ptr::write_bytes(dev.in_virt as *mut u8, 0, 4096);
		((dev.in_virt + 4) as *mut u32).write_volatile(1 | 1 << dci);
		let slot_ctx: u64 = dev.in_virt + hc.ctx_size;
		(slot_ctx as *mut u32).write_volatile(dci << 27 | dev.speed << 20 | dev.route);
		((slot_ctx + 4) as *mut u32).write_volatile(dev.port << 16);
		let ep_ctx: u64 = dev.in_virt + (1 + dci as u64) * hc.ctx_size;
		// THE INTERVAL IS AN EXPONENT IN THE CONTEXT AND A COUNT IN THE DESCRIPTOR, and for a
		// full-speed isochronous endpoint the descriptor's `bInterval` is frames directly while the
		// context wants log2 of microframes. One frame is eight microframes, so an interval of one
		// frame is an exponent of three - and a driver that wrote the descriptor's number gets a
		// schedule eight times too fast, which the controller answers with underruns.
		let interval: u32 = 3 + bound.interval.saturating_sub(1) as u32;
		(ep_ctx as *mut u32).write_volatile(interval << 16);
		// ERROR COUNT ZERO, WHICH IS WHAT ISOCHRONOUS MEANS. The bulk endpoints here write three -
		// retry this many times - and an isochronous endpoint that retried would be delivering last
		// interval's audio in this one.
		((ep_ctx + 4) as *mut u32).write_volatile((bound.max_packet as u32) << 16 | EP_TYPE_ISOCH_OUT << 3);
		((ep_ctx + 8) as *mut u32).write_volatile((ring.phys | ring.cycle as u64) as u32);
		((ep_ctx + 12) as *mut u32).write_volatile((ring.phys >> 32) as u32);
		((ep_ctx + 16) as *mut u32).write_volatile(bound.max_packet as u32);
		if command_and_wait(hc, dev.in_phys, 0, TRB_CONFIGURE_ENDPOINT << 10 | dev.slot << 24).is_none() {
			print(b"driver.xhci: the audio device's isochronous endpoint was refused by the controller\n");
			return None;
		}
		Some(Audio { dci, ring, virt, phys, binding: bound })
	}
}

/// Play one period, answering whether the controller took it.
///
/// ONE TRANSFER DESCRIPTOR PER SERVICE INTERVAL. An isochronous endpoint carries `max_packet` bytes
/// each time the bus schedules it, so a period is several transfers - and the last one is short,
/// which is not an error: the wire's period and the device's packet size divide evenly only by
/// accident.
pub unsafe fn play(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, sink: &mut Audio, period: &[u8]) -> bool {
	unsafe {
		let bytes = period.len().min(4096);
		for (index, byte) in period[..bytes].iter().enumerate() {
			((sink.virt + index as u64) as *mut u8).write_volatile(*byte);
		}
		let packets = uac::packets_for(bytes as u32, sink.binding.max_packet);
		if packets == 0 {
			return false;
		}
		core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
		for index in 0..packets {
			let span = uac::packet_span(bytes as u32, sink.binding.max_packet, index);
			let at = sink.phys + index as u64 * sink.binding.max_packet as u64;
			// ONLY THE LAST ONE ASKS FOR A COMPLETION. An interrupt per packet is eleven events a
			// period at this size, and what the caller is waiting for is the period, not the packet.
			let last = index + 1 == packets;
			let control = TRB_ISOCH << 10 | TRB_START_ISOCH_ASAP | if last { TRB_IOC } else { 0 };
			sink.ring.push(at, span, control);
		}
		w32(hc.db + dev.slot as u64 * 4, sink.dci);
		// A SHORT PACKET IS A NORMAL COMPLETION HERE. The last transfer of a period is short
		// whenever the period is not a multiple of the packet size, which is every period at this
		// wire's size against this device's.
		match wait_transfer(hc, hids, dev.slot, sink.dci) {
			Some(code) => code == CC_SUCCESS || code == CC_SHORT_PACKET,
			None => false,
		}
	}
}
