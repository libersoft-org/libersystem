// driver.virtio-snd - the userspace virtio-sound PCM playback driver.
//
// virtio-sound plays audio by configuring a PCM output stream over the control
// queue (set-params -> prepare -> start) and then handing the device PCM periods on
// the transmit queue, each a descriptor chain of an xfer header (the stream id), the
// PCM data, and a status word the device writes back. This driver brings the device
// up over the shared virtio transport, finds an output stream, and then serves a
// single client (AudioService): each message it receives is one PCM period (signed
// 16-bit, 2 channels, 48 kHz - the fixed format AudioService synthesizes), which it
// plays on the transmit queue; an empty message ends the stream (stop + release).
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

mod common;
mod virtio;

use rt::*;

use crate::virtio::{Queue, Virtio};

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
const FMT_S16: u8 = 5;
const RATE_48000: u8 = 7;
const CHANNELS: u8 = 2;

// The virtqueues: control (0), event (1), and transmit (2). The event queue is set
// up (the device expects it) but its notifications are ignored; the receive queue
// (capture) is not used.
const CONTROLQ: u16 = 0;
const EVENTQ: u16 = 1;
const TXQ: u16 = 2;

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

	// PCM_INFO over `count` streams starting at 0: return the id of the first output
	// stream, or 0 if the query fails (stream 0 is the output stream on QEMU).
	unsafe fn find_output_stream(&self, count: u32) -> u32 {
		unsafe {
			if count == 0 || count > 32 {
				return 0;
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
				return 0;
			}
			for i in 0..count {
				let info = self.resp_virt + 4 + i as u64 * 32;
				if ((info + 24) as *const u8).read_volatile() == D_OUTPUT {
					return i;
				}
			}
			0
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

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		// 1. bring the device up (recv "DEVICE" + MMIO cap, map, negotiate to FEATURES_OK).
		let mut device: Virtio = common::bringup(bootstrap);
		// 2. receive our device's MSI-X Interrupt capability ("IRQ" + handle) and route
		//    this device's interrupts to MSI-X table entry 0 (DeviceManager acquired it via
		//    device_msix_acquire, so the kernel has programmed the table and enabled MSI-X).
		let irq: u64 = recv_irq(bootstrap);
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
		txq.enable_interrupts();
		device.driver_ok();

		// 4. allocate the control command/response buffers and the transmit DMA page.
		let (_cmd_h, cmd_virt, cmd_phys) = dma_buffer(PAGE).unwrap_or_else(|| exit());
		let (_resp_h, resp_virt, resp_phys) = dma_buffer(PAGE).unwrap_or_else(|| exit());
		let ctl = Ctl { q: ctlq, cmd_virt, cmd_phys, resp_virt, resp_phys };
		// one page: xfer header @0 (4B), status @8 (8B), PCM period @64 (PERIOD_BYTES).
		let (_tx_h, tx_virt, tx_phys) = dma_buffer(PAGE).unwrap_or_else(|| exit());
		wr32(tx_virt, 0); // xfer header = stream id (filled below once known)
		let mut tx = Tx { q: txq, xfer_phys: tx_phys, period_virt: tx_virt + 64, period_phys: tx_phys + 64, status_phys: tx_phys + 8 };

		// 5. read the PCM stream count from the device config (virtio_snd_config: jacks
		//    @0, streams @4, chmaps @8), find the output stream, and write its id into the
		//    xfer header.
		let streams: u32 = config_u32(&device, 4);
		let stream: u32 = ctl.find_output_stream(streams);
		wr32(tx_virt, stream);

		// 6. report in, transferring the client end of our service channel up the chain
		//    (DeviceManager -> ServiceManager -> AudioService), then serve it. We stand on
		//    the service channel, not the bootstrap channel, so DeviceManager being stopped
		//    after boot does not tear us down.
		let (service, far): (u64, u64) = channel().unwrap_or_else(|| exit());
		send_blocking(bootstrap, b"driver.virtio-snd: online", far);
		serve(&ctl, &mut tx, irq, stream, service)
	}
}

// Receive the "IRQ" message carrying this device's Interrupt capability, which
// DeviceManager acquired (device_msix_acquire) and transferred to us. Exits if it
// does not arrive.
unsafe fn recv_irq(bootstrap: u64) -> u64 {
	unsafe {
		let mut buf: [u8; 16] = [0u8; 16];
		match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } if handle != 0 && len >= 3 && &buf[..3] == b"IRQ" => handle,
			_ => exit(),
		}
	}
}

// Read a little-endian u32 from the device-specific config at `offset`.
unsafe fn config_u32(device: &Virtio, offset: u64) -> u32 {
	unsafe { device.config_read(offset) as u32 | (device.config_read(offset + 1) as u32) << 8 | (device.config_read(offset + 2) as u32) << 16 | (device.config_read(offset + 3) as u32) << 24 }
}

// Serve AudioService: each non-empty message is one PCM period (received straight
// into the transmit DMA page) to play; the first period of a session lazily
// configures and starts the stream; an empty message ends the session (stop +
// release). Exits when the client closes.
unsafe fn serve(ctl: &Ctl, tx: &mut Tx, irq: u64, stream: u32, service: u64) -> ! {
	unsafe {
		let mut started: bool = false;
		loop {
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
					exit();
				}
			}
		}
	}
}
