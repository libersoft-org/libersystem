// driver.virtio-snd - the userspace virtio-sound PCM playback and capture driver.
//
// virtio-sound plays audio by configuring a PCM output stream over the control
// queue (set-params -> prepare -> start) and then handing the device PCM periods on
// the transmit queue, each a descriptor chain of an xfer header (the stream id), the
// PCM data, and a status word the device writes back. This driver brings the device
// up over the shared virtio transport, finds an output stream, and then serves a
// single client (AudioService): each message it receives is one PCM period (signed
// 16-bit, 2 channels, 48 kHz - the fixed format AudioService synthesizes), which it
// plays on the transmit queue; an empty message ends the stream (stop + release).
// Capture is the same shape with the direction reversed - the driver hands the
// device SPACE on the receive queue and waits for it to be filled - and reaches
// AudioService as the one-byte commands documented beside `CMD_CAPTURE`.
//
// Like driver.virtio-input it is interrupt-driven (MSI-X): DeviceManager hands it,
// after the usual "DEVICE" message, a second "IRQ" message carrying an Interrupt
// capability for the device's MSI-X vector (DeviceManager acquired it with
// device_msix_acquire, so the kernel has programmed the table and enabled MSI-X). The
// driver points the device at table entry 0 (`set_msix_vector`), enables interrupts
// on the transmit queue, and then for each period submits the chain and blocks on the
// interrupt until the device has consumed it (`submit_async` + `wait` + `take_used`)
// rather than busy-polling the used ring. The control queue stays poll-driven (its
// few set-up commands are synchronous and infrequent).

#![no_std]
#![no_main]

use rt::*;

use crate::virtio::{Queue, Virtio};
use drivers::{common, virtio};

// virtio-sound control requests (the PCM subset) and the success status.
const R_PCM_INFO: u32 = 0x0100;
const R_PCM_SET_PARAMS: u32 = 0x0101;
const R_PCM_PREPARE: u32 = 0x0102;
const R_PCM_RELEASE: u32 = 0x0103;
const R_PCM_START: u32 = 0x0104;
const R_PCM_STOP: u32 = 0x0105;
const S_OK: u32 = 0x8000;

// PCM stream direction, format, and rate codes (the values AudioService produces).
const D_OUTPUT: u8 = 0;
const D_INPUT: u8 = 1;
const FMT_S16: u8 = 5;
const RATE_48000: u8 = 7;
const CHANNELS: u8 = 2;

// The virtqueues: control (0), event (1), transmit (2) and receive (3). The event queue is set
// up (the device expects it) but its notifications are ignored.
const CONTROLQ: u16 = 0;
const EVENTQ: u16 = 1;
const TXQ: u16 = 2;
const RXQ: u16 = 3;

// THE SERVICE PROTOCOL, IN FULL, because it is three message shapes on one channel and a reader
// should not have to infer the third from the code:
//
//   - a message of PERIOD_BYTES is one PCM period to PLAY, received straight into the transmit DMA
//     page. The reply is "OK";
//   - an EMPTY message ends the playback stream (stop + release). The reply is "OK";
//   - a ONE-BYTE message is a command. `CMD_CAPTURE` asks for one captured period and is answered
//     with the period itself; `CMD_CAPTURE_STOP` ends the capture stream and is answered with "OK".
//
// One byte is unambiguous because AudioService pads every playback period to exactly PERIOD_BYTES -
// the driver's own SET_PARAMS negotiated that size, so a period of any other length was never a
// shape this protocol had.
const CMD_CAPTURE: u8 = 1;
const CMD_CAPTURE_STOP: u8 = 2;

// One PCM period: 512 stereo signed-16-bit frames = 2048 bytes (~10.6 ms at 48 kHz).
// AudioService always sends exactly this many bytes per period (padding the last
// with silence), so a submitted period always matches the negotiated period size.
const PERIOD_BYTES: u32 = 2048;
// The device-side ring holds several periods, so playback does not underrun while
// we synthesize and submit the next one.
const BUFFER_BYTES: u32 = PERIOD_BYTES * 8;

const PAGE: u64 = 4096;

unsafe fn wr32(addr: u64, v: u32) {
	unsafe { (addr as *mut u32).write_unaligned(v) }
}
unsafe fn rd32(addr: u64) -> u32 {
	unsafe { (addr as *const u32).read_unaligned() }
}
unsafe fn wr8(addr: u64, v: u8) {
	unsafe { (addr as *mut u8).write_volatile(v) }
}

// The control queue plus the command / response DMA buffers reused for every
// control request.
struct Ctl {
	q: Queue,
	cmd_virt: u64,
	cmd_phys: u64,
	resp_virt: u64,
	resp_phys: u64,
}

impl Ctl {
	// Submit a control command (cmd_len bytes, device-readable) plus a resp_len-byte
	// device-writable response, returning the response status word, or None on a queue
	// error.
	unsafe fn submit(&self, cmd_len: u32, resp_len: u32) -> Option<u32> {
		unsafe {
			core::ptr::write_bytes(self.resp_virt as *mut u8, 0, resp_len as usize);
			self.q.submit(&[(self.cmd_phys, cmd_len, false), (self.resp_phys, resp_len, true)])?;
			Some(rd32(self.resp_virt))
		}
	}

	// PCM_INFO over `count` streams starting at 0: return the id of the first stream in
	// `direction`, or None when the query fails or the device has none.
	//
	// CAPTURE IS NOT ASSUMED TO EXIST. A device with no input stream is a machine with no
	// microphone, which is an ordinary machine - the caller reports "not found" rather than the
	// driver refusing to start. Playback keeps its historical fallback of stream 0 at the call site,
	// because that is the one QEMU has always had and changing it is not this item.
	unsafe fn find_stream(&self, count: u32, direction: u8) -> Option<u32> {
		unsafe {
			if count == 0 || count > 32 {
				return None;
			}
			// request: virtio_snd_query_info { code, start_id, count, size(=32) }.
			core::ptr::write_bytes(self.cmd_virt as *mut u8, 0, 16);
			wr32(self.cmd_virt, R_PCM_INFO);
			wr32(self.cmd_virt + 4, 0);
			wr32(self.cmd_virt + 8, count);
			wr32(self.cmd_virt + 12, 32);
			// response: status(4) + count * virtio_snd_pcm_info(32); direction @ +24 of each.
			let resp_len = 4 + count * 32;
			if self.submit(16, resp_len) != Some(S_OK) {
				return None;
			}
			for i in 0..count {
				let info = self.resp_virt + 4 + i as u64 * 32;
				if ((info + 24) as *const u8).read_volatile() == direction {
					return Some(i);
				}
			}
			None
		}
	}

	// SET_PARAMS for `stream`: signed-16-bit, 2-channel, 48 kHz, our period/buffer sizes.
	unsafe fn set_params(&self, stream: u32) -> bool {
		unsafe {
			core::ptr::write_bytes(self.cmd_virt as *mut u8, 0, 24);
			wr32(self.cmd_virt, R_PCM_SET_PARAMS);
			wr32(self.cmd_virt + 4, stream);
			wr32(self.cmd_virt + 8, BUFFER_BYTES);
			wr32(self.cmd_virt + 12, PERIOD_BYTES);
			wr32(self.cmd_virt + 16, 0); // features
			wr8(self.cmd_virt + 20, CHANNELS);
			wr8(self.cmd_virt + 21, FMT_S16);
			wr8(self.cmd_virt + 22, RATE_48000);
			wr8(self.cmd_virt + 23, 0); // padding
			self.submit(24, 4) == Some(S_OK)
		}
	}

	// A simple virtio_snd_pcm_hdr { code, stream } command (prepare/start/stop/release).
	unsafe fn stream_cmd(&self, code: u32, stream: u32) -> bool {
		unsafe {
			wr32(self.cmd_virt, code);
			wr32(self.cmd_virt + 4, stream);
			self.submit(8, 4) == Some(S_OK)
		}
	}
}

// The transmit queue and the single DMA page that holds the xfer header, the PCM
// period, and the status word (all small enough to share one physically contiguous
// page).
struct Tx {
	q: Queue,
	xfer_phys: u64,
	period_virt: u64,
	period_phys: u64,
	status_phys: u64,
}

impl Tx {
	// Play one period: the PCM is already in `period_virt` (received straight into it).
	// Submit [xfer][pcm][status], then block on the device's MSI-X interrupt until it
	// has consumed the chain, reap the completion, and re-arm the interrupt.
	unsafe fn play(&mut self, irq: u64) -> bool {
		unsafe {
			if !self.q.submit_async(&[(self.xfer_phys, 4, false), (self.period_phys, PERIOD_BYTES, false), (self.status_phys, 8, true)]) {
				return false;
			}
			// block until the device raises its MSI-X interrupt for the consumed period.
			wait(irq, 0);
			self.q.take_used();
			// clear the pending flag so the next period wakes us (edge-triggered MSI-X).
			interrupt_ack(irq);
			true
		}
	}
}

// The receive queue and its own DMA page, laid out exactly like the transmit one.
//
// THE DIRECTION IS THE WHOLE DIFFERENCE. Playback hands the device a period it may READ and waits
// for it to be consumed; capture hands it SPACE it may WRITE and waits for it to be filled. The
// descriptor chain says which: the PCM segment is device-writable here and device-readable there,
// and the xfer header stays device-readable in both because it is the driver naming the stream.
struct Rx {
	q: Queue,
	xfer_phys: u64,
	period_virt: u64,
	period_phys: u64,
	status_phys: u64,
}

impl Rx {
	// Capture one period into `period_virt`: submit [xfer][space][status], block on the device's
	// MSI-X interrupt until it has filled the chain, reap the completion and re-arm.
	unsafe fn capture(&mut self, irq: u64) -> bool {
		unsafe {
			if !self.q.submit_async(&[(self.xfer_phys, 4, false), (self.period_phys, PERIOD_BYTES, true), (self.status_phys, 8, true)]) {
				return false;
			}
			wait(irq, 0);
			self.q.take_used();
			interrupt_ack(irq);
			true
		}
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		// 1. bring the device up (recv "DEVICE" + MMIO cap, map, negotiate to FEATURES_OK).
		let (bind, resources) = common::handshake(bootstrap);
		let mut device: Virtio = common::bringup_bound(bootstrap, &bind, &resources, 0);
		// 2. receive our device's MSI-X Interrupt capability ("IRQ" + handle) and route
		//    this device's interrupts to MSI-X table entry 0 (DeviceManager acquired it via
		//    device_msix_acquire, so the kernel has programmed the table and enabled MSI-X).
		let irq: u64 = resources.irq;
		device.set_msix_vector(0);
		// 3. set up control (0), event (1, drained-never), and transmit (2) queues, then
		//    go live. The receive (capture) queue is not used. The transmit queue is
		//    interrupt-driven; the control and event queues stay quiet (poll / unused).
		let ctlq: Queue = match device.setup_queue(CONTROLQ) {
			Some(q) => q,
			None => exit(),
		};
		let _eventq: Queue = match device.setup_queue(EVENTQ) {
			Some(q) => q,
			None => exit(),
		};
		let txq: Queue = match device.setup_queue(TXQ) {
			Some(q) => q,
			None => exit(),
		};
		// The receive queue is set up whether or not this device has an input stream: the queue
		// exists in the transport either way, and a device with no capture stream simply never has
		// a chain submitted to it.
		let rxq: Queue = match device.setup_queue(RXQ) {
			Some(q) => q,
			None => exit(),
		};
		txq.enable_interrupts();
		rxq.enable_interrupts();
		device.driver_ok();

		// 4. allocate the control command/response buffers and the transmit DMA page.
		let (_cmd_h, cmd_virt, cmd_phys) = dma_buffer_for(device.capability, PAGE).unwrap_or_else(|| exit());
		let (_resp_h, resp_virt, resp_phys) = dma_buffer_for(device.capability, PAGE).unwrap_or_else(|| exit());
		let ctl = Ctl { q: ctlq, cmd_virt, cmd_phys, resp_virt, resp_phys };
		// one page: xfer header @0 (4B), status @8 (8B), PCM period @64 (PERIOD_BYTES).
		let (_tx_h, tx_virt, tx_phys) = dma_buffer_for(device.capability, PAGE).unwrap_or_else(|| exit());
		wr32(tx_virt, 0); // xfer header = stream id (filled below once known)
		let mut tx = Tx { q: txq, xfer_phys: tx_phys, period_virt: tx_virt + 64, period_phys: tx_phys + 64, status_phys: tx_phys + 8 };
		// The capture page is its own, not a second use of the transmit one: a captured period is
		// device-written while a played one is device-read, and one page serving both would be a
		// buffer the device may write while the driver is filling it for playback.
		let (_rx_h, rx_virt, rx_phys) = dma_buffer_for(device.capability, PAGE).unwrap_or_else(|| exit());
		let mut rx = Rx { q: rxq, xfer_phys: rx_phys, period_virt: rx_virt + 64, period_phys: rx_phys + 64, status_phys: rx_phys + 8 };

		// 5. read the PCM stream count from the device config (virtio_snd_config: jacks
		//    @0, streams @4, chmaps @8), find the output stream, and write its id into the
		//    xfer header.
		let streams: u32 = config_u32(&device, 4);
		let stream: u32 = ctl.find_stream(streams, D_OUTPUT).unwrap_or(0);
		wr32(tx_virt, stream);
		// The capture stream, when the device has one. `NO_STREAM` is the answer to "record" on a
		// machine with no input stream, and AudioService turns it into a not-found for its client.
		let capture: Option<u32> = ctl.find_stream(streams, D_INPUT);
		wr32(rx_virt, capture.unwrap_or(0));

		// 6. report in, transferring the client end of our service channel up the chain
		//    (DeviceManager -> ServiceManager -> AudioService), then serve it. We stand on
		//    the service channel, not the bootstrap channel, so DeviceManager being stopped
		//    after boot does not tear us down.
		let (service, far): (u64, u64) = channel().unwrap_or_else(|| exit());
		let mut line = [0u8; 64];
		let n = common::describe(&mut line, b"virtio-snd", &device, b"");
		common::online(bootstrap, &bind, &line[..n], &[(driver_protocol::provider::AUDIO, far)]);
		serve(bootstrap, &bind, &ctl, &mut tx, &mut rx, irq, stream, capture, service)
	}
}

// Read a little-endian u32 from the device-specific config at `offset`.
unsafe fn config_u32(device: &Virtio, offset: u64) -> u32 {
	unsafe { device.config_read(offset) as u32 | (device.config_read(offset + 1) as u32) << 8 | (device.config_read(offset + 2) as u32) << 16 | (device.config_read(offset + 3) as u32) << 24 }
}

// Serve AudioService. The protocol is the three shapes documented beside `CMD_CAPTURE`: a period
// to play, an empty message that ends playback, or a one-byte command. A playback period is
// received straight into the transmit DMA page; the first period of a session lazily configures and
// starts the output stream. Exits when the client closes.
unsafe fn serve(bootstrap: u64, bind: &common::Bind, ctl: &Ctl, tx: &mut Tx, rx: &mut Rx, irq: u64, stream: u32, capture: Option<u32>, service: u64) -> ! {
	unsafe {
		let mut started: bool = false;
		let mut capturing: bool = false;
		loop {
			// The manager's ping is answered by THIS loop. A driver parked waiting for its next
			// period is idle, not wedged, and only a combined wait can tell the two apart.
			if !common::serve_or_answer(bootstrap, bind, service) {
				// THE STREAMS ARE STOPPED AND RELEASED BEFORE THE STOP IS ANSWERED. This exited with
				// playback or capture still running - the same cleanup its service-channel-close
				// branch performs, skipped on the path that promises the work is finished.
				if common::stop_requested() {
					if started {
						ctl.stream_cmd(R_PCM_STOP, stream);
						ctl.stream_cmd(R_PCM_RELEASE, stream);
					}
					if capturing {
						ctl.stream_cmd(R_PCM_STOP, capture.unwrap_or(0));
						ctl.stream_cmd(R_PCM_RELEASE, capture.unwrap_or(0));
					}
				}
				common::finish_stop(bootstrap, bind, ctl.q.capability, common::quiesce_virtio());
				exit();
			}
			// receive straight into the period region of the transmit DMA page.
			let period: &mut [u8] = core::slice::from_raw_parts_mut(tx.period_virt as *mut u8, PERIOD_BYTES as usize);
			match recv_blocking(service, period) {
				Received::Message { len, .. } => {
					if len == 0 {
						// end of stream: stop and release if we started.
						if started {
							ctl.stream_cmd(R_PCM_STOP, stream);
							ctl.stream_cmd(R_PCM_RELEASE, stream);
							started = false;
						}
						send_blocking(service, b"OK", 0);
						continue;
					}
					if len == 1 {
						let command: u8 = period[0];
						serve_command(ctl, rx, irq, capture, service, command, &mut capturing);
						continue;
					}
					// first period of a session: configure and start the stream.
					if !started {
						started = ctl.set_params(stream) && ctl.stream_cmd(R_PCM_PREPARE, stream) && ctl.stream_cmd(R_PCM_START, stream);
					}
					if started {
						tx.play(irq);
					}
					send_blocking(service, b"OK", 0);
				}
				Received::Closed => {
					if started {
						ctl.stream_cmd(R_PCM_STOP, stream);
						ctl.stream_cmd(R_PCM_RELEASE, stream);
					}
					if capturing {
						ctl.stream_cmd(R_PCM_STOP, capture.unwrap_or(0));
						ctl.stream_cmd(R_PCM_RELEASE, capture.unwrap_or(0));
					}
					exit();
				}
			}
		}
	}
}

// The one-byte commands. Split out because the capture side has its own lifecycle - prepare and
// start on the first period, stop and release on request - and threading it through the playback
// match arm would put two stream state machines in one block.
//
// A REFUSAL IS AN EMPTY REPLY, and an empty reply is never a period: a period is PERIOD_BYTES. That
// is how "this machine has no input stream" and "the device would not start one" reach AudioService
// without a second message shape for errors.
unsafe fn serve_command(ctl: &Ctl, rx: &mut Rx, irq: u64, capture: Option<u32>, service: u64, command: u8, capturing: &mut bool) {
	unsafe {
		let Some(stream) = capture else {
			// No input stream on this device. Answer every capture command the same way, including
			// the stop, so a client that gives up does not block on a reply that never comes.
			send_blocking(service, &[], 0);
			return;
		};
		match command {
			CMD_CAPTURE => {
				if !*capturing {
					*capturing = ctl.set_params(stream) && ctl.stream_cmd(R_PCM_PREPARE, stream) && ctl.stream_cmd(R_PCM_START, stream);
				}
				if !*capturing || !rx.capture(irq) {
					send_blocking(service, &[], 0);
					return;
				}
				let filled: &[u8] = core::slice::from_raw_parts(rx.period_virt as *const u8, PERIOD_BYTES as usize);
				send_blocking(service, filled, 0);
			}
			CMD_CAPTURE_STOP => {
				if *capturing {
					ctl.stream_cmd(R_PCM_STOP, stream);
					ctl.stream_cmd(R_PCM_RELEASE, stream);
					*capturing = false;
				}
				send_blocking(service, b"OK", 0);
			}
			// A command this driver does not know. Answered rather than ignored, because the client
			// is blocked on the reply either way.
			_ => {
				send_blocking(service, &[], 0);
			}
		}
	}
}
