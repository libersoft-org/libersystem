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
// edge-triggered vector - no INTx sharing to hijack), the config vector is
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
const CMD_RESOURCE_UNREF: u32 = 0x0102;
const CMD_SET_SCANOUT: u32 = 0x0103;
const CMD_RESOURCE_FLUSH: u32 = 0x0104;
const CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
const CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
const CMD_RESOURCE_DETACH_BACKING: u32 = 0x0107;
const RESP_OK_NODATA: u32 = 0x1100;
const RESP_OK_DISPLAY_INFO: u32 = 0x1101;

// VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM: in memory B, G, R, X - i.e. a little-endian u32
// 0xXXRRGGBB, so red at bits 16-23, green 8-15, blue 0-7 (the layout ConsoleService
// packs pixels with).
const FORMAT_B8G8R8X8: u32 = 2;

// The control-queue header (virtio_gpu_ctrl_hdr) is 24 bytes; a rect 16 bytes.
const HDR_LEN: u64 = 24;
const PAGE: u64 = 4096;

// The first host resource id; each reallocation binds the next id, so the old
// resource and its replacement never alias while both exist.
const FIRST_RESOURCE_ID: u32 = 1;
const SCANOUT_ID: u32 = 0;

// A sane fallback if the device reports no display size yet.
const FALLBACK_W: u32 = 1024;
const FALLBACK_H: u32 = 768;

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
			if w == 0 || h == 0 { (FALLBACK_W, FALLBACK_H) } else { (w, h) }
		}
	}

	// RESOURCE_CREATE_2D: create the host-side B8G8R8X8 resource `id` of the given size.
	unsafe fn create_2d(&self, id: u32, w: u32, h: u32) -> bool {
		unsafe {
			self.hdr(CMD_RESOURCE_CREATE_2D);
			wr32(self.cmd_virt + 24, id);
			wr32(self.cmd_virt + 28, FORMAT_B8G8R8X8);
			wr32(self.cmd_virt + 32, w);
			wr32(self.cmd_virt + 36, h);
			self.submit(40, HDR_LEN as u32) == Some(RESP_OK_NODATA)
		}
	}

	// RESOURCE_DETACH_BACKING: release resource `id`'s guest backing store.
	unsafe fn detach_backing(&self, id: u32) -> bool {
		unsafe {
			self.hdr(CMD_RESOURCE_DETACH_BACKING);
			wr32(self.cmd_virt + 24, id);
			wr32(self.cmd_virt + 28, 0);
			self.submit(32, HDR_LEN as u32) == Some(RESP_OK_NODATA)
		}
	}

	// RESOURCE_UNREF: destroy the host-side resource `id`.
	unsafe fn unref(&self, id: u32) -> bool {
		unsafe {
			self.hdr(CMD_RESOURCE_UNREF);
			wr32(self.cmd_virt + 24, id);
			wr32(self.cmd_virt + 28, 0);
			self.submit(32, HDR_LEN as u32) == Some(RESP_OK_NODATA)
		}
	}

	// RESOURCE_ATTACH_BACKING: hand the device the guest framebuffer pages as the
	// resource's backing store. The framebuffer DMA buffer is mapped contiguously but
	// its physical frames are scattered, so the entry list is built by coalescing
	// physically contiguous runs - one mem-entry per run, not per page - which keeps
	// even a large (4K+) framebuffer's request inside the control queue's descriptor
	// budget (contiguous DMA would collapse it to one entry). The entry list
	// lives in `entries` (its own DMA buffer); the request is submitted as a
	// descriptor chain - the 32-byte fixed head, then the entry pages, then the
	// response - so a multi-page entry list need not be physically contiguous.
	unsafe fn attach_backing(&self, id: u32, fb_handle: u64, entries: &Dma, pages: u64) -> bool {
		unsafe {
			// fill the entry list with coalesced runs: addr(u64), length(u32), padding.
			let mut nr: u64 = 0;
			let mut run_base: u64 = 0;
			let mut run_len: u64 = 0;
			for i in 0..pages {
				let phys = dma_buffer_phys_at(fb_handle, i * PAGE);
				if run_len != 0 && phys == run_base + run_len {
					run_len += PAGE;
					continue;
				}
				if run_len != 0 {
					let e = entries.virt + nr * 16;
					wr64(e, run_base);
					wr32(e + 8, run_len as u32);
					wr32(e + 12, 0);
					nr += 1;
				}
				run_base = phys;
				run_len = PAGE;
			}
			if run_len != 0 {
				let e = entries.virt + nr * 16;
				wr64(e, run_base);
				wr32(e + 8, run_len as u32);
				wr32(e + 12, 0);
				nr += 1;
			}
			// fixed head: hdr + resource_id + nr_entries.
			self.hdr(CMD_RESOURCE_ATTACH_BACKING);
			wr32(self.cmd_virt + 24, id);
			wr32(self.cmd_virt + 28, nr as u32);
			// descriptor chain: [head 32B][entry page 0..N][response].
			let entry_bytes = nr * 16;
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

	// SET_SCANOUT: bind resource `id` to scanout 0 covering the whole display.
	unsafe fn set_scanout(&self, id: u32, w: u32, h: u32) -> bool {
		unsafe {
			self.hdr(CMD_SET_SCANOUT);
			self.rect(24, 0, 0, w, h);
			wr32(self.cmd_virt + 40, SCANOUT_ID);
			wr32(self.cmd_virt + 44, id);
			self.submit(48, HDR_LEN as u32) == Some(RESP_OK_NODATA)
		}
	}

	// Present a rectangle of the framebuffer: copy those guest-backing pixels to the
	// host resource `id`, then flush that rectangle to the display. `stride` is the
	// resource's pixel width (the backing's allocated geometry), which fixes the byte
	// offset of the rectangle's first pixel in the backing.
	unsafe fn present(&self, id: u32, x: u32, y: u32, w: u32, h: u32, stride: u32) -> bool {
		unsafe {
			// TRANSFER_TO_HOST_2D: rect, offset(u64), resource_id, padding.
			self.hdr(CMD_TRANSFER_TO_HOST_2D);
			self.rect(24, x, y, w, h);
			wr64(self.cmd_virt + 40, (y as u64 * stride as u64 + x as u64) * 4);
			wr32(self.cmd_virt + 48, id);
			wr32(self.cmd_virt + 52, 0);
			if self.submit(56, HDR_LEN as u32) != Some(RESP_OK_NODATA) {
				return false;
			}
			// RESOURCE_FLUSH: rect, resource_id, padding.
			self.hdr(CMD_RESOURCE_FLUSH);
			self.rect(24, x, y, w, h);
			wr32(self.cmd_virt + 40, id);
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

// The guest framebuffer and its host resource: the DmaBuffer backing (unmapped here -
// ConsoleService is the one that renders into it, and a DmaBuffer maps only once),
// the mem-entry list, the resource id bound to the backing, and the allocated
// geometry (the pitch every consumer renders against). Reallocated whenever the host
// display outgrows it - displays outgrow any constant, so no fixed ceiling stands
// here; the display's own reported size is the only bound.
struct Backing {
	handle: u64,
	entries: Dma,
	id: u32,
	w: u32,
	h: u32,
}

// Allocate a framebuffer + host resource for `w x h` under resource id `id`: the
// DmaBuffer backing, its mem-entry list, RESOURCE_CREATE_2D and ATTACH_BACKING.
// None on any failure, with everything allocated so far released (the caller keeps
// its old backing).
unsafe fn create_backing(gpu: &Gpu, id: u32, w: u32, h: u32) -> Option<Backing> {
	unsafe {
		let fb_size = align_up(w as u64 * h as u64 * 4, PAGE);
		let pages = fb_size / PAGE;
		let handle: i64 = dma_buffer_create(fb_size);
		if handle < 0 {
			return None;
		}
		let handle = handle as u64;
		let entries = match dma(align_up(pages * 16, PAGE)) {
			Some(d) => d,
			None => {
				close(handle);
				return None;
			}
		};
		if !gpu.create_2d(id, w, h) || !gpu.attach_backing(id, handle, &entries, pages) {
			close(entries.handle);
			close(handle);
			return None;
		}
		Some(Backing { handle, entries, id, w, h })
	}
}

// Release a replaced backing: unbind and destroy its host resource, then close our
// guest handles (ConsoleService's dup keeps the old buffer alive until it swaps to
// the replacement, so its mapping never dangles).
unsafe fn release_backing(gpu: &Gpu, old: Backing) {
	unsafe {
		gpu.detach_backing(old.id);
		gpu.unref(old.id);
		close(old.entries.handle);
		close(old.handle);
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

		// 3. query the current display size, then create the resource + framebuffer at
		//    exactly that geometry - the backing grows on demand when the host display
		//    outgrows it (see serve), so no ceiling constant stands here. The framebuffer
		//    is created but NOT mapped here: a DmaBuffer may be mapped only once, and
		//    ConsoleService is the one that renders into it, so it maps it. We only need
		//    the backing's physical frames (dma_buffer_phys_at works unmapped).
		let (init_w, init_h) = gpu.display_size();
		let backing: Backing = match create_backing(&gpu, FIRST_RESOURCE_ID, init_w, init_h) {
			Some(b) => b,
			None => exit(),
		};
		// bind scanout 0 to the whole allocation (the current display).
		if !gpu.set_scanout(backing.id, init_w, init_h) {
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
		serve(&device, &gpu, backing, service, irq)
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
// timeout) wakes it to re-read the display size. A resize within the allocation
// rebinds the scanout to the new sub-rectangle and tells ConsoleService (RESIZE) so
// it reflows its terminal; a resize BEYOND the allocation reallocates - a new DMA
// framebuffer and host resource at the new geometry, the scanout re-attached, the
// old backing released - and hands the new backing over (FBNEW), so any
// host-supported resolution renders in full. "FB" hands back the framebuffer
// (allocated geometry + current size + a MAP|TRANSFER dup of the backing); "FLUSH"
// presents the current rectangle (backing -> host resource -> display).
unsafe fn serve(device: &Virtio, gpu: &Gpu, mut backing: Backing, service: u64, irq: u64) -> ! {
	unsafe {
		let mut cur_w: u32 = backing.w;
		let mut cur_h: u32 = backing.h;
		let mut req: [u8; 32] = [0u8; 32];
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
				let (mut nw, mut nh) = gpu.display_size();
				if nw > 0 && nh > 0 && (nw, nh) != (cur_w, cur_h) {
					if nw > backing.w || nh > backing.h {
						// the display outgrew the allocation: reallocate at the new geometry
						// (each axis at least what the old backing held, so a wider-but-
						// shorter window never shrinks an axis mid-swap), rebind the scanout,
						// release the old resource, and hand the new backing to ConsoleService.
						match create_backing(gpu, backing.id + 1, nw.max(backing.w), nh.max(backing.h)) {
							Some(replacement) => {
								if !gpu.set_scanout(replacement.id, nw, nh) {
									release_backing(gpu, replacement);
									continue;
								}
								let old: Backing = core::mem::replace(&mut backing, replacement);
								release_backing(gpu, old);
								cur_w = nw;
								cur_h = nh;
								// FBNEW: the new backing's geometry, the display size, and a
								// mappable dup - ConsoleService remaps, swaps its surfaces, and
								// closes its old handle (which frees the old buffer).
								let dup: i64 = duplicate(backing.handle, RIGHT_MAP | RIGHT_TRANSFER);
								if dup < 0 {
									exit();
								}
								let mut msg: [u8; 45] = [0u8; 45];
								msg[..5].copy_from_slice(b"FBNEW");
								let info = framebuffer_info(backing.w, backing.h);
								let fb_len: usize = core::mem::size_of::<Framebuffer>();
								core::ptr::copy_nonoverlapping(&info as *const Framebuffer as *const u8, msg[5..].as_mut_ptr(), fb_len);
								msg[5 + fb_len..5 + fb_len + 4].copy_from_slice(&cur_w.to_le_bytes());
								msg[5 + fb_len + 4..5 + fb_len + 8].copy_from_slice(&cur_h.to_le_bytes());
								send_blocking(service, &msg[..5 + fb_len + 8], dup as u64);
								continue;
							}
							None => {
								// the reallocation failed (memory pressure): clamp to the standing
								// allocation rather than blanking the screen, and fall through to
								// the in-allocation rebind below.
								nw = nw.min(backing.w);
								nh = nh.min(backing.h);
								if (nw, nh) == (cur_w, cur_h) {
									continue;
								}
							}
						}
					}
					cur_w = nw;
					cur_h = nh;
					gpu.set_scanout(backing.id, cur_w, cur_h);
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
			// of deferred presents collapses into a single present. Each FLUSH carries the
			// rectangle the console repainted; the queued rectangles are united into one
			// bounding box, so a backlog still moves only the changed region of the newest
			// frame (the console always renders into the shared backing). A bare FLUSH (no
			// rectangle) presents the whole display.
			let mut flush_rect: Option<(u32, u32, u32, u32)> = None;
			loop {
				match try_recv(service, &mut req) {
					Polled::Message { len, .. } => {
						let m: &[u8] = &req[..len];
						if m.starts_with(b"FB") {
							// hand back the allocated framebuffer geometry (pitch and extent),
							// the current display size, and a mappable, transferable dup of the
							// backing handle (we keep our own handle to stay pinned).
							let dup: i64 = duplicate(backing.handle, RIGHT_MAP | RIGHT_TRANSFER);
							if dup < 0 {
								exit();
							}
							let info = framebuffer_info(backing.w, backing.h);
							let fb_len: usize = core::mem::size_of::<Framebuffer>();
							let mut reply: [u8; 32] = [0u8; 32];
							core::ptr::copy_nonoverlapping(&info as *const Framebuffer as *const u8, reply.as_mut_ptr(), fb_len);
							reply[fb_len..fb_len + 4].copy_from_slice(&cur_w.to_le_bytes());
							reply[fb_len + 4..fb_len + 8].copy_from_slice(&cur_h.to_le_bytes());
							send_blocking(service, &reply[..fb_len + 8], dup as u64);
						} else if m.starts_with(b"FLUSH") {
							let r = if m.len() >= 21 { (rd32_le(m, 5), rd32_le(m, 9), rd32_le(m, 13), rd32_le(m, 17)) } else { (0, 0, cur_w, cur_h) };
							flush_rect = Some(match flush_rect {
								Some(u) => union_rect(u, r),
								None => r,
							});
						}
					}
					Polled::Empty => break,
					Polled::Closed => exit(),
				}
			}
			if let Some((x, y, w, h)) = flush_rect {
				// Clamp to the visible scanout: pixels past it need no transfer, and the
				// transfer must stay inside the resource.
				let x = x.min(cur_w);
				let y = y.min(cur_h);
				let w = w.min(cur_w - x);
				let h = h.min(cur_h - y);
				if w > 0 && h > 0 {
					gpu.present(backing.id, x, y, w, h, backing.w);
				}
			}
		}
	}
}

// Read a little-endian u32 at `at` in `m`.
fn rd32_le(m: &[u8], at: usize) -> u32 {
	u32::from_le_bytes([m[at], m[at + 1], m[at + 2], m[at + 3]])
}

// The ABI Framebuffer describing a backing of the given allocated geometry (the
// B8G8R8X8 pixel layout every consumer renders with).
fn framebuffer_info(w: u32, h: u32) -> Framebuffer {
	Framebuffer { width: w, height: h, pitch: w * 4, bytes_per_pixel: 4, red_shift: 16, red_size: 8, green_shift: 8, green_size: 8, blue_shift: 0, blue_size: 8, _pad: [0; 2] }
}

// The bounding box of two rectangles (x, y, w, h).
fn union_rect(a: (u32, u32, u32, u32), b: (u32, u32, u32, u32)) -> (u32, u32, u32, u32) {
	let x0 = a.0.min(b.0);
	let y0 = a.1.min(b.1);
	let x1 = (a.0 + a.2).max(b.0 + b.2);
	let y1 = (a.1 + a.3).max(b.1 + b.3);
	(x0, y0, x1 - x0, y1 - y0)
}
