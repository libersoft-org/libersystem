// driver.virtio-gpu - the userspace virtio-gpu 2D display driver.
//
// virtio-gpu draws by attaching a guest framebuffer as the backing store of a host
// "resource", binding that resource to scanout 0, and presenting dirty rectangles
// with TRANSFER_TO_HOST_2D + RESOURCE_FLUSH. This driver brings the device up over
// the shared virtio transport, queries the display size (GET_DISPLAY_INFO), creates
// a B8G8R8X8 resource the size of the display, attaches a DMA framebuffer as its
// backing, and sets scanout 0. It then serves a single client (ConsoleService),
// which maps the shared backing, renders into it, and asks the driver to present
// (FLUSH) each frame. Only the control queue (queue 0) is used; the cursor queue
// and 3D are not.
//
// Host-window resizes are detected by the device's configuration-change interrupt:
// DeviceManager hands us the device's MSI-X Interrupt capability (a per-device
// edge-triggered vector, M46 - no INTx sharing to hijack), the config vector is
// routed to it, and a resize wakes the serve loop, which re-reads GET_DISPLAY_INFO,
// rebinds the scanout, and tells ConsoleService (RESIZE) to reflow. Should the
// interrupt be unavailable, the loop falls back to the old periodic size poll.

#![no_std]
#![no_main]

mod common;
mod virtio;

use rt::*;

use crate::virtio::{Queue, Virtio};

// virtio-gpu control commands (the 2D subset) and the two responses we check.
const CMD_GET_DISPLAY_INFO: u32 = 0x0100;
const CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
const CMD_SET_SCANOUT: u32 = 0x0103;
const CMD_RESOURCE_FLUSH: u32 = 0x0104;
const CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
const CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
const RESP_OK_NODATA: u32 = 0x1100;
const RESP_OK_DISPLAY_INFO: u32 = 0x1101;

// VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM: in memory B, G, R, X - i.e. a little-endian u32
// 0xXXRRGGBB, so red at bits 16-23, green 8-15, blue 0-7 (the layout ConsoleService
// packs pixels with).
const FORMAT_B8G8R8X8: u32 = 2;

// The control-queue header (virtio_gpu_ctrl_hdr) is 24 bytes; a rect 16 bytes.
const HDR_LEN: u64 = 24;
const PAGE: u64 = 4096;

// The single resource + scanout this driver drives.
const RESOURCE_ID: u32 = 1;
const SCANOUT_ID: u32 = 0;

// A sane fallback if the device reports no display size yet.
const FALLBACK_W: u32 = 1024;
const FALLBACK_H: u32 = 768;

// The resource (and its backing framebuffer) is allocated at this maximum size so the
// host window can grow up to it without reallocating the backing (a DmaBuffer maps
// once, so re-handing a new one to ConsoleService is avoided): on a resize the scanout
// is just rebound to the new sub-rectangle. A larger initial display raises the cap to
// fit it. A window grown past the cap is clamped (the console stops growing).
const MAX_W: u32 = 1920;
const MAX_H: u32 = 1080;

// How often (in 100 Hz monotonic ticks) the serve loop wakes to poll the host display
// size while idle - the FALLBACK when no config-change interrupt was granted: ~200 ms,
// snappy enough for window drags yet a trivial one-command-per-tick load.
const POLL_TICKS: u64 = 20;

// The virtio-gpu device config: events_read (le32 at 0) accumulates event bits the
// driver acknowledges by writing them to events_clear (le32 at 4).
// VIRTIO_GPU_EVENT_DISPLAY (bit 0) signals a display change.
const CONFIG_EVENTS_READ: u64 = 0;
const CONFIG_EVENTS_CLEAR: u64 = 4;
const EVENT_DISPLAY: u8 = 1;

unsafe fn wr32(addr: u64, v: u32) {
	unsafe { (addr as *mut u32).write_unaligned(v) }
}
unsafe fn wr64(addr: u64, v: u64) {
	unsafe { (addr as *mut u64).write_unaligned(v) }
}
unsafe fn rd32(addr: u64) -> u32 {
	unsafe { (addr as *const u32).read_unaligned() }
}

fn align_up(x: u64, a: u64) -> u64 {
	(x + a - 1) & !(a - 1)
}

// The control queue plus the command / response DMA buffers reused for every
// control-queue request.
struct Gpu {
	q: Queue,
	cmd_virt: u64,
	cmd_phys: u64,
	resp_virt: u64,
	resp_phys: u64,
}

impl Gpu {
	// Write the 24-byte control header (type, then zeroed flags/fence/ctx) at the start
	// of the command buffer.
	unsafe fn hdr(&self, ty: u32) {
		unsafe {
			core::ptr::write_bytes(self.cmd_virt as *mut u8, 0, HDR_LEN as usize);
			wr32(self.cmd_virt, ty);
		}
	}

	// Submit the command (cmd_len bytes, device-readable) plus a resp_len-byte
	// device-writable response, returning the response type, or None on a queue error.
	unsafe fn submit(&self, cmd_len: u32, resp_len: u32) -> Option<u32> {
		unsafe {
			core::ptr::write_bytes(self.resp_virt as *mut u8, 0, resp_len as usize);
			self.q.submit(&[(self.cmd_phys, cmd_len, false), (self.resp_phys, resp_len, true)])?;
			Some(rd32(self.resp_virt))
		}
	}

	// GET_DISPLAY_INFO -> (width, height) of scanout 0, falling back to a default if the
	// device reports nothing enabled yet.
	unsafe fn display_size(&self) -> (u32, u32) {
		unsafe {
			self.hdr(CMD_GET_DISPLAY_INFO);
			// response: hdr(24) + 16 * virtio_gpu_display_one{ rect(16), enabled(4), flags(4) }.
			if self.submit(HDR_LEN as u32, 24 + 16 * 24) != Some(RESP_OK_DISPLAY_INFO) {
				return (FALLBACK_W, FALLBACK_H);
			}
			// pmodes[0].r.width @ hdr + 8, .height @ hdr + 12.
			let w = rd32(self.resp_virt + HDR_LEN + 8);
			let h = rd32(self.resp_virt + HDR_LEN + 12);
			if w == 0 || h == 0 {
				(FALLBACK_W, FALLBACK_H)
			} else {
				(w, h)
			}
		}
	}

	// RESOURCE_CREATE_2D: create the host-side B8G8R8X8 resource of the given size.
	unsafe fn create_2d(&self, w: u32, h: u32) -> bool {
		unsafe {
			self.hdr(CMD_RESOURCE_CREATE_2D);
			wr32(self.cmd_virt + 24, RESOURCE_ID);
			wr32(self.cmd_virt + 28, FORMAT_B8G8R8X8);
			wr32(self.cmd_virt + 32, w);
			wr32(self.cmd_virt + 36, h);
			self.submit(40, HDR_LEN as u32) == Some(RESP_OK_NODATA)
		}
	}

	// RESOURCE_ATTACH_BACKING: hand the device the guest framebuffer pages as the
	// resource's backing store. The framebuffer DMA buffer is mapped contiguously but
	// its physical frames are scattered, so one mem-entry per page (its true physical
	// address). The entry list lives in `entries` (its own DMA buffer); the request is
	// submitted as a descriptor chain - the 32-byte fixed head, then the entry pages,
	// then the response - so a multi-page entry list need not be physically contiguous.
	unsafe fn attach_backing(&self, fb_handle: u64, entries: &Dma, pages: u64) -> bool {
		unsafe {
			// fill the entry list: addr(u64), length(u32 = one page), padding(u32).
			for i in 0..pages {
				let e = entries.virt + i * 16;
				wr64(e, dma_buffer_phys_at(fb_handle, i * PAGE));
				wr32(e + 8, PAGE as u32);
				wr32(e + 12, 0);
			}
			// fixed head: hdr + resource_id + nr_entries.
			self.hdr(CMD_RESOURCE_ATTACH_BACKING);
			wr32(self.cmd_virt + 24, RESOURCE_ID);
			wr32(self.cmd_virt + 28, pages as u32);
			// descriptor chain: [head 32B][entry page 0..N][response].
			let entry_bytes = pages * 16;
			let entry_pages = align_up(entry_bytes, PAGE) / PAGE;
			let mut descs: [(u64, u32, bool); 16] = [(0, 0, false); 16];
			let mut n = 0;
			descs[n] = (self.cmd_phys, 32, false);
			n += 1;
			for p in 0..entry_pages {
				if n >= 15 {
					return false; // would not leave room for the response descriptor
				}
				let off = p * PAGE;
				let len = (entry_bytes - off).min(PAGE) as u32;
				descs[n] = (dma_buffer_phys_at(entries.handle, off), len, false);
				n += 1;
			}
			descs[n] = (self.resp_phys, HDR_LEN as u32, true);
			n += 1;
			core::ptr::write_bytes(self.resp_virt as *mut u8, 0, HDR_LEN as usize);
			if self.q.submit(&descs[..n]).is_none() {
				return false;
			}
			rd32(self.resp_virt) == RESP_OK_NODATA
		}
	}

	// SET_SCANOUT: bind the resource to scanout 0 covering the whole display.
	unsafe fn set_scanout(&self, w: u32, h: u32) -> bool {
		unsafe {
			self.hdr(CMD_SET_SCANOUT);
			self.rect(24, 0, 0, w, h);
			wr32(self.cmd_virt + 40, SCANOUT_ID);
			wr32(self.cmd_virt + 44, RESOURCE_ID);
			self.submit(48, HDR_LEN as u32) == Some(RESP_OK_NODATA)
		}
	}

	// Present the whole framebuffer: copy the guest backing to the host resource, then
	// flush that rectangle to the display.
	unsafe fn present(&self, w: u32, h: u32) -> bool {
		unsafe {
			// TRANSFER_TO_HOST_2D: rect, offset(u64), resource_id, padding.
			self.hdr(CMD_TRANSFER_TO_HOST_2D);
			self.rect(24, 0, 0, w, h);
			wr64(self.cmd_virt + 40, 0);
			wr32(self.cmd_virt + 48, RESOURCE_ID);
			wr32(self.cmd_virt + 52, 0);
			if self.submit(56, HDR_LEN as u32) != Some(RESP_OK_NODATA) {
				return false;
			}
			// RESOURCE_FLUSH: rect, resource_id, padding.
			self.hdr(CMD_RESOURCE_FLUSH);
			self.rect(24, 0, 0, w, h);
			wr32(self.cmd_virt + 40, RESOURCE_ID);
			wr32(self.cmd_virt + 44, 0);
			self.submit(48, HDR_LEN as u32) == Some(RESP_OK_NODATA)
		}
	}

	// Write a virtio_gpu_rect (x, y, width, height) at offset `at` in the command buffer.
	unsafe fn rect(&self, at: u64, x: u32, y: u32, w: u32, h: u32) {
		unsafe {
			wr32(self.cmd_virt + at, x);
			wr32(self.cmd_virt + at + 4, y);
			wr32(self.cmd_virt + at + 8, w);
			wr32(self.cmd_virt + at + 12, h);
		}
	}
}

// A mapped DMA buffer (handle kept open to keep it pinned).
struct Dma {
	handle: u64,
	virt: u64,
}

unsafe fn dma(size: u64) -> Option<Dma> {
	unsafe {
		let (handle, virt, _phys) = dma_buffer(size)?;
		Some(Dma { handle, virt })
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		// 1. bring the device up (recv "DEVICE" + MMIO cap, map, negotiate to FEATURES_OK),
		//    then receive the config-change Interrupt capability ("IRQ") DeviceManager
		//    acquired for us.
		let mut device: Virtio = common::bringup(bootstrap);
		let irq: u64 = recv_irq(bootstrap);
		// 2. set up the control queue (queue 0) and go live. The queue stays polled
		//    (NO_VECTOR - set_msix_vector runs after setup_queue on purpose); only the
		//    device's CONFIG vector is routed to our interrupt, so it fires exactly for
		//    display changes.
		let q: Queue = match device.setup_queue(0) {
			Some(q) => q,
			None => exit(),
		};
		if irq != 0 {
			device.set_msix_vector(0);
		}
		device.driver_ok();
		// command + response buffers reused for every control request.
		let cmd = match dma(PAGE) {
			Some(d) => d,
			None => exit(),
		};
		let resp = match dma(PAGE) {
			Some(d) => d,
			None => exit(),
		};
		let cmd_phys = dma_buffer_phys(cmd.handle);
		let resp_phys = dma_buffer_phys(resp.handle);
		let gpu = Gpu { q, cmd_virt: cmd.virt, cmd_phys, resp_virt: resp.virt, resp_phys };

		// 3. query the current display size, then create the resource + framebuffer at the
		//    maximum size (so the window can grow without reallocating the backing). The
		//    framebuffer is created but NOT mapped here: a DmaBuffer may be mapped only
		//    once, and ConsoleService is the one that renders into it, so it maps it. We
		//    only need the backing's physical frames (dma_buffer_phys_at works unmapped).
		let (init_w, init_h) = gpu.display_size();
		let max_w = init_w.max(MAX_W);
		let max_h = init_h.max(MAX_H);
		let fb_size = align_up(max_w as u64 * max_h as u64 * 4, PAGE);
		let pages = fb_size / PAGE;
		let fb_handle: u64 = {
			let h: i64 = dma_buffer_create(fb_size);
			if h < 0 {
				exit();
			}
			h as u64
		};
		let entries = match dma(align_up(pages * 16, PAGE)) {
			Some(d) => d,
			None => exit(),
		};
		// create the resource at the max size, attach the backing, then bind scanout 0 to
		// the current display sub-rectangle of it.
		if !gpu.create_2d(max_w, max_h) || !gpu.attach_backing(fb_handle, &entries, pages) || !gpu.set_scanout(init_w, init_h) {
			exit();
		}

		// 4. the host resource starts blank (QEMU zeroes it) and the guest backing is only
		//    copied to it on TRANSFER_TO_HOST_2D, so we present nothing here: the first
		//    visible frame is ConsoleService's cleared, rendered banner (its first FLUSH).
		//    This keeps any stale guest-frame content off the screen.

		// 5. report in, transferring the client end of our service channel up the chain
		//    (DeviceManager -> ServiceManager -> ConsoleService), then serve it. We stand
		//    on this service channel, not the bootstrap channel, so DeviceManager being
		//    stopped after boot does not tear us down (the backing must stay pinned).
		let (service, far): (u64, u64) = match channel() {
			Some(p) => p,
			None => exit(),
		};
		send_blocking(bootstrap, b"driver.virtio-gpu: online", far);
		serve(&device, &gpu, fb_handle, max_w, max_h, init_w, init_h, service, irq)
	}
}

// Receive the "IRQ" message carrying the device's Interrupt capability (the
// config-change vector), which DeviceManager acquired and transferred to us.
// Returns 0 when it does not arrive - the serve loop then falls back to polling.
unsafe fn recv_irq(bootstrap: u64) -> u64 {
	unsafe {
		let mut buf: [u8; 16] = [0u8; 16];
		match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } if len >= 3 && &buf[..3] == b"IRQ" => handle,
			_ => 0,
		}
	}
}

// Serve ConsoleService while watching for host-window resizes. The serve loop waits
// on the service channel and the config-change interrupt: a message wakes it at once
// (FB / FLUSH); the interrupt (or, with no interrupt granted, a periodic poll
// timeout) wakes it to re-read the display size, and on a change it rebinds the
// scanout to the new sub-rectangle of the resource and tells ConsoleService (RESIZE)
// so it reflows its terminal. "FB" hands back the framebuffer (max geometry + current
// size + a MAP|TRANSFER dup of the backing); "FLUSH" presents the current rectangle
// (backing -> host resource -> display).
#[allow(clippy::too_many_arguments)]
unsafe fn serve(device: &Virtio, gpu: &Gpu, fb_handle: u64, max_w: u32, max_h: u32, init_w: u32, init_h: u32, service: u64, irq: u64) -> ! {
	unsafe {
		let mut cur_w: u32 = init_w;
		let mut cur_h: u32 = init_h;
		let mut req: [u8; 16] = [0u8; 16];
		loop {
			// wake on a service request or on a display change. The interrupt path blocks
			// with no deadline; the poll fallback is a housekeeping wake (WAIT_PERIODIC), so
			// it never counts as pending progress for the scheduler's boot driver (or the
			// kernel tests).
			let ready: i64 = if irq != 0 { wait_any(&[service, irq], 0) } else { wait_any_periodic(&[service], clock() + POLL_TICKS) };
			if ready != 0 {
				if irq != 0 {
					// acknowledge the display event (write the read bits back to events_clear)
					// and re-arm the interrupt BEFORE reading the new size, so a change racing
					// the read fires again rather than being lost.
					let events: u8 = device.config_read(CONFIG_EVENTS_READ);
					if events & EVENT_DISPLAY != 0 {
						device.config_write(CONFIG_EVENTS_CLEAR, EVENT_DISPLAY);
					}
					interrupt_ack(irq);
				}
				// a display change (or poll timeout): a resize shows up as a new
				// GET_DISPLAY_INFO size.
				let (nw, nh) = gpu.display_size();
				let (nw, nh) = (nw.min(max_w), nh.min(max_h));
				if nw > 0 && nh > 0 && (nw, nh) != (cur_w, cur_h) {
					cur_w = nw;
					cur_h = nh;
					gpu.set_scanout(cur_w, cur_h);
					// ask ConsoleService to reflow to the new display (it then renders and
					// FLUSHes, which presents the new frame).
					let mut msg: [u8; 14] = [0u8; 14];
					msg[..6].copy_from_slice(b"RESIZE");
					msg[6..10].copy_from_slice(&cur_w.to_le_bytes());
					msg[10..14].copy_from_slice(&cur_h.to_le_bytes());
					send_blocking(service, &msg, 0);
				}
				continue;
			}
			// A message woke us: drain every queued request, coalescing FLUSHes so a backlog
			// of deferred presents collapses into a single present of the latest backing.
			// The console always renders the newest frame into the shared backing, so older
			// queued FLUSHes are redundant; presenting once keeps the display from falling
			// behind by N stale frames when a slow display client makes each present costly.
			let mut need_present = false;
			loop {
				match try_recv(service, &mut req) {
					Polled::Message { len, .. } => {
						let m: &[u8] = &req[..len];
						if m.starts_with(b"FB") {
							// hand back the max framebuffer geometry (pitch and extent), the
							// current display size, and a mappable, transferable dup of the
							// backing handle (we keep our own handle to stay pinned).
							let dup: i64 = duplicate(fb_handle, RIGHT_MAP | RIGHT_TRANSFER);
							if dup < 0 {
								exit();
							}
							let info = Framebuffer { width: max_w, height: max_h, pitch: max_w * 4, bytes_per_pixel: 4, red_shift: 16, red_size: 8, green_shift: 8, green_size: 8, blue_shift: 0, blue_size: 8, _pad: [0; 2] };
							let fb_len: usize = core::mem::size_of::<Framebuffer>();
							let mut reply: [u8; 32] = [0u8; 32];
							core::ptr::copy_nonoverlapping(&info as *const Framebuffer as *const u8, reply.as_mut_ptr(), fb_len);
							reply[fb_len..fb_len + 4].copy_from_slice(&cur_w.to_le_bytes());
							reply[fb_len + 4..fb_len + 8].copy_from_slice(&cur_h.to_le_bytes());
							send_blocking(service, &reply[..fb_len + 8], dup as u64);
						} else if m.starts_with(b"FLUSH") {
							need_present = true;
						}
					}
					Polled::Empty => break,
					Polled::Closed => exit(),
				}
			}
			if need_present {
				gpu.present(cur_w, cur_h);
			}
		}
	}
}
