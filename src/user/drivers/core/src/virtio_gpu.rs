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

extern crate alloc;

use rt::*;

use crate::virtio::{Queue, Virtio};
use drivers::{common, gpu, virtio};

// THE TYPED WIRE THIS DRIVER SERVES. It was seven byte strings and a framebuffer record memcpy'd out
// of a message at an offset; `liber:display-device@1` is the same conversation with a generated
// reader and writer at each end, and its module documentation is where the lifecycle this driver
// implements is written down.
use alloc::vec::Vec;
use display_device_proto::codec::Handles;
use display_device_proto::generated::liber::display_device::v1 as wire;
use display_device_proto::generated::liber::graphics::v1 as graphics;

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
	fn display_size(&self) -> (u32, u32) {
		unsafe {
			self.hdr(CMD_GET_DISPLAY_INFO);
			// response: hdr(24) + 16 * virtio_gpu_display_one{ rect(16), enabled(4), flags(4) }.
			if self.submit(HDR_LEN as u32, 24 + 16 * 24) != Some(RESP_OK_DISPLAY_INFO) {
				return (FALLBACK_W, FALLBACK_H);
			}
			// pmodes[0].r.width @ hdr + 8, .height @ hdr + 12.
			let w = rd32(self.resp_virt + HDR_LEN + 8);
			let h = rd32(self.resp_virt + HDR_LEN + 12);
			// THE DEVICE CHOOSES THIS NUMBER. A zero was always refused; everything else it could say
			// - four billion, or anything whose pitch has already wrapped - was believed, and the
			// framebuffer described from it is the one ConsoleService maps and draws into.
			gpu::display_geometry((w, h), (FALLBACK_W, FALLBACK_H))
		}
	}

	// RESOURCE_CREATE_2D: create the host-side B8G8R8X8 resource `id` of the given size.
	fn create_2d(&self, id: u32, w: u32, h: u32) -> bool {
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
	fn detach_backing(&self, id: u32) -> bool {
		unsafe {
			self.hdr(CMD_RESOURCE_DETACH_BACKING);
			wr32(self.cmd_virt + 24, id);
			wr32(self.cmd_virt + 28, 0);
			self.submit(32, HDR_LEN as u32) == Some(RESP_OK_NODATA)
		}
	}

	// RESOURCE_UNREF: destroy the host-side resource `id`.
	fn unref(&self, id: u32) -> bool {
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
	fn set_scanout(&self, id: u32, w: u32, h: u32) -> bool {
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
	fn present(&self, id: u32, x: u32, y: u32, w: u32, h: u32, stride: u32) -> bool {
		unsafe {
			// THE OFFSET IS THE LAST ARITHMETIC BEFORE THE DEVICE IS TOLD WHERE TO READ, and an
			// unchecked one addresses the resource outside itself.
			let Some(offset) = gpu::transfer_offset(x, y, stride) else {
				return false;
			};
			// TRANSFER_TO_HOST_2D: rect, offset(u64), resource_id, padding.
			self.hdr(CMD_TRANSFER_TO_HOST_2D);
			self.rect(24, x, y, w, h);
			wr64(self.cmd_virt + 40, offset);
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
	fn rect(&self, at: u64, x: u32, y: u32, w: u32, h: u32) {
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

// `device` is the DeviceMemory capability this memory is for, so the kernel can keep the frames out
// of circulation if this driver dies with the GPU still pointed at them.
unsafe fn dma(device: u64, size: u64) -> Option<Dma> {
	unsafe {
		let (handle, virt, _phys) = dma_buffer_for(device, size)?;
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
fn create_backing(gpu: &Gpu, id: u32, w: u32, h: u32) -> Option<Backing> {
	unsafe {
		// A PRODUCT THAT DOES NOT FIT IS NOT AN ALLOCATION THAT FAILS - it is a smaller one that
		// succeeds, which is a backing shorter than the picture it is said to hold.
		let fb_size = align_up(drivers::gpu::backing_bytes(w, h)?, PAGE);
		let pages = fb_size / PAGE;
		let handle: i64 = dma_buffer_create_for(gpu.q.capability, fb_size);
		if handle < 0 {
			return None;
		}
		let handle = handle as u64;
		let entries = match dma(gpu.q.capability, align_up(pages * 16, PAGE)) {
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
fn release_backing(gpu: &Gpu, old: Backing) {
	gpu.detach_backing(old.id);
	gpu.unref(old.id);
	close(old.entries.handle);
	close(old.handle);
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		// 1. bring the device up (recv "DEVICE" + MMIO cap, map, negotiate to FEATURES_OK),
		//    then receive the config-change Interrupt capability ("IRQ") DeviceManager
		//    acquired for us.
		let (bind, resources) = common::handshake(bootstrap);
		let mut device: Virtio = common::bringup_bound(bootstrap, &bind, &resources, 0);
		let irq: u64 = resources.irq;
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
		let cmd = match dma(q.capability, PAGE) {
			Some(d) => d,
			None => exit(),
		};
		let resp = match dma(q.capability, PAGE) {
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
		let mut line = [0u8; 64];
		let n = common::describe(&mut line, b"virtio-gpu", &device, b"");
		common::online(bootstrap, &bind, &line[..n], &[(driver_protocol::provider::DISPLAY, far)]);
		serve(bootstrap, &bind, &device, &gpu, backing, service, irq)
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
// THE DEVICE AS THE TYPED INTERFACE SEES IT: the backing it owns, how much of it is visible, and
// which generation those pixels belong to.
//
// THE GENERATION IS THE POINT OF THIS STRUCT. It moves when the BACKING moves and not when the
// visible size does, which is the difference between "the same pixels, differently framed" and "your
// pixels are gone" - and a present naming an old generation is refused rather than drawn into memory
// the driver has given back.
struct Scanout<'a> {
	gpu: &'a Gpu,
	backing: Backing,
	visible: (u32, u32),
	generation: u32,
	/// The event stream's producer end, or zero before the service opens one.
	events: u64,
}

impl Scanout<'_> {
	// The current scanout as the wire describes it, with a mappable duplicate of the backing.
	//
	// `RIGHT_WRITE` EXPLICITLY. The receiver maps this framebuffer and DRAWS into it, and it used to
	// be handed over with `MAP` alone - which worked only because every mapping was made writable
	// whether or not the capability said so. Now that `WRITE` is what decides, the intent has to be
	// stated: this is a surface to render into, so the grant says so.
	fn describe(&self) -> Option<wire::Scanout> {
		let dup: i64 = duplicate(self.backing.handle, RIGHT_READ | RIGHT_WRITE | RIGHT_MAP | RIGHT_TRANSFER);
		if dup < 0 {
			return None;
		}
		let pitch = drivers::gpu::pitch_bytes(self.backing.w)?;
		let len = (pitch as u64).checked_mul(self.backing.h as u64)?;
		Some(wire::Scanout {
			backing: display_device_proto::codec::Buffer { handle: dup as u64, len },
			// THE ALLOCATED EXTENT AND NOT THE VISIBLE ONE. A driver allocates at least what the
			// display asked for and may allocate more, so a later resize needs no new backing; the
			// layout describes the memory and `visible` describes the picture.
			layout: graphics::ImageLayout { size: graphics::Extent2d { width: self.backing.w, height: self.backing.h }, pitch, format: graphics::PixelFormat::B8g8r8x8Unorm, alpha: graphics::AlphaMode::Opaque, color_space: graphics::ColorSpace::Srgb, origin: graphics::RowOrigin::TopLeft },
			visible: wire::Extent2d { width: self.visible.0, height: self.visible.1 },
			generation: self.generation,
		})
	}

	// Push one event to the service, dropping it when there is no stream or the peer is gone.
	//
	// BOUNDED AND IDEMPOTENT, which is what makes dropping safe here: both events are snapshots of
	// the current state rather than deltas, so the newest one is the true one and a lost older one
	// costs nothing.
	fn emit(&mut self, event: &wire::DeviceEvent, seq: &mut u32) {
		if self.events == 0 {
			return;
		}
		let mut frame: [u8; 96] = [0u8; 96];
		let mut handles = Handles::new();
		let sent = match wire::display_device::events_frame(*seq, event, &mut frame, &mut handles) {
			Some(n) => send_caps_blocking(self.events, &frame[..n], handles.as_slice()),
			None => false,
		};
		if sent {
			*seq = seq.wrapping_add(1);
			return;
		}
		for handle in handles.as_slice() {
			close(*handle);
		}
		close(self.events);
		self.events = 0;
	}
}

impl wire::display_device::Service for Scanout<'_> {
	fn scanout(&mut self) -> Result<wire::Scanout, wire::Error> {
		// A DRIVER THAT CANNOT HAND ITS BACKING OVER IS OUT OF WHAT IT NEEDS TO DO SO - a handle, or
		// an arithmetic that fits - which is `exhausted` and not `invalid`: nothing was wrong with
		// the request.
		self.describe().ok_or(wire::Error::Exhausted)
	}

	fn present(&mut self, generation: u32, damage: wire::DamageRegion) -> Result<(), wire::Error> {
		// A BACKING FROM AN OLD GENERATION CAN NEVER BE PRESENTED INTO A NEW ONE. The frame was drawn
		// against pixels this driver has given back, and transferring it would show whatever is in
		// that memory now.
		if generation != self.generation {
			return Err(wire::Error::Stale);
		}
		let mut set: drivers::gpu::DamageSet = drivers::gpu::DamageSet::new();
		if damage.whole {
			set.add((0, 0, self.visible.0, self.visible.1));
		} else {
			for rect in &damage.rects {
				let (Ok(x), Ok(y)) = (u32::try_from(rect.origin.x), u32::try_from(rect.origin.y)) else {
					return Err(wire::Error::Invalid);
				};
				set.add((x, y, rect.size.width, rect.size.height));
			}
		}
		// AN EMPTY LIST IS "NOTHING CHANGED" and completes without touching the device, which is the
		// same answer the service gives its own clients.
		for rect in set.rects() {
			// CLIPPED TO THE VISIBLE SCANOUT AND NOT CLAMPED TO IT: pixels past it need no transfer,
			// a rectangle entirely past it presents nothing, and the transfer must stay inside the
			// resource.
			if let Some((x, y, w, h)) = drivers::gpu::visible_rect(*rect, self.visible)
				&& !self.gpu.present(self.backing.id, x, y, w, h, self.backing.w)
			{
				return Err(wire::Error::Io);
			}
		}
		Ok(())
	}

	fn events(&mut self) -> Vec<wire::DeviceEvent> {
		Vec::new()
	}
}

// SAFE NOW, AND SAYING SO IS THE POINT. It was `unsafe fn` with an `unsafe` body because it decoded a
// framebuffer record out of a message with `read_unaligned` and wrote one out with
// `copy_nonoverlapping`; the typed wire has a generated reader and writer, so nothing in this loop
// dereferences anything the compiler cannot see.
fn serve(bootstrap: u64, bind: &common::Bind, device: &Virtio, gpu: &Gpu, backing: Backing, service: u64, irq: u64) -> ! {
	{
		let mut scanout: Scanout<'_> = Scanout { gpu, visible: (backing.w, backing.h), backing, generation: 1, events: 0 };
		let mut seq: u32 = 0;
		let mut req: [u8; 128] = [0u8; 128];
		loop {
			// wake on a service request or on a display change. The interrupt path blocks
			// with no deadline; the poll fallback is a housekeeping wake (WAIT_PERIODIC), so
			// it never counts as pending progress for the scheduler's boot driver (or the
			// kernel tests).
			// The manager's channel joins the set this loop already waits on. The polled branch
			// keeps its period: a display that must re-read its size on a timer still has to
			// answer, so the ping rides the same wait rather than a second one.
			let ready: i64 = if irq != 0 {
				match common::wait_or_answer(bootstrap, bind, &[service, irq]) {
					Some(at) => at as i64,
					None => {
						common::finish_stop(bootstrap, bind, gpu.q.capability, common::quiesce_virtio());
						exit()
					}
				}
			} else {
				wait_any_periodic(&[service, bootstrap], clock() + POLL_TICKS)
			};
			if irq == 0 && ready == 1 {
				if !common::answer_ping(bootstrap, bind) {
					common::finish_stop(bootstrap, bind, gpu.q.capability, common::quiesce_virtio());
					exit();
				}
				continue;
			}
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
				if nw > 0 && nh > 0 && (nw, nh) != scanout.visible {
					if nw > scanout.backing.w || nh > scanout.backing.h {
						// the display outgrew the allocation: reallocate at the new geometry
						// (each axis at least what the old backing held, so a wider-but-
						// shorter window never shrinks an axis mid-swap), rebind the scanout,
						// release the old resource, and hand the new backing to ConsoleService.
						match create_backing(gpu, scanout.backing.id + 1, nw.max(scanout.backing.w), nh.max(scanout.backing.h)) {
							Some(replacement) => {
								if !gpu.set_scanout(replacement.id, nw, nh) {
									release_backing(gpu, replacement);
									continue;
								}
								let old: Backing = core::mem::replace(&mut scanout.backing, replacement);
								release_backing(gpu, old);
								scanout.visible = (nw, nh);
								// THE BACKING MOVED, SO THE GENERATION MOVES. Everything the service
								// drew against the old one is gone, and a present naming that
								// generation is refused rather than transferred into memory this
								// driver has already given back.
								scanout.generation = scanout.generation.wrapping_add(1);
								// The replacement, as an event: the service remaps, swaps its
								// surfaces and closes its old handle, which frees the old buffer.
								match scanout.describe() {
									Some(described) => {
										let event = wire::DeviceEvent::Replaced(described);
										scanout.emit(&event, &mut seq);
									}
									// A duplicate this driver cannot make is a driver that cannot
									// hand its backing over at all; the service asks again through
									// `scanout` when it next adopts.
									None => (),
								}
								continue;
							}
							None => {
								// the reallocation failed (memory pressure): clamp to the standing
								// allocation rather than blanking the screen, and fall through to
								// the in-allocation rebind below.
								nw = nw.min(scanout.backing.w);
								nh = nh.min(scanout.backing.h);
								if (nw, nh) == scanout.visible {
									continue;
								}
							}
						}
					}
					scanout.visible = (nw, nh);
					gpu.set_scanout(scanout.backing.id, nw, nh);
					// A RESIZE INSIDE THE EXISTING BACKING KEEPS ITS GENERATION: the same pixels are
					// still there and what changed is how much of them is shown. The service reflows
					// and presents the new frame against the backing it already holds.
					let event = wire::DeviceEvent::Resized(wire::Extent2d { width: nw, height: nh });
					scanout.emit(&event, &mut seq);
				}
				continue;
			}
			// A message woke us: drain every queued request and ANSWER EACH ONE, through the
			// generated dispatch. The coalescing this loop used to do across messages is gone with
			// the byte protocol that needed it: a present is one call with one answer, and the
			// merging that is still worth doing happens inside one present's damage list.
			loop {
				match try_recv_caps(service, &mut req) {
					PolledCaps::Message { len, mut handles } => {
						let op: u16 = if len >= 2 { u16::from_le_bytes([req[0], req[1]]) } else { 0 };
						if op == wire::display_device::OP_EVENTS {
							open_event_stream(service, &req[..len], &mut handles, &mut scanout);
						} else {
							let mut reply: [u8; 128] = [0u8; 128];
							let mut reply_handles = Handles::new();
							match wire::display_device::dispatch(&mut scanout, &req[..len], &mut handles, &mut reply, &mut reply_handles) {
								Some(n) => {
									if !send_caps_blocking(service, &reply[..n], reply_handles.as_slice()) {
										for handle in reply_handles.as_slice() {
											close(*handle);
										}
									}
								}
								None => {
									for handle in reply_handles.as_slice() {
										close(*handle);
									}
								}
							}
						}
						// EVERY CAPABILITY THE REQUEST CARRIED IS CLOSED, claimed or not: a dispatch
						// that refused the frame leaves whatever it held, and a driver that dropped
						// the list would leak one handle per malformed request.
						for handle in handles.as_slice() {
							close(*handle);
						}
					}
					PolledCaps::Empty => break,
					PolledCaps::Closed => exit(),
				}
			}
		}
	}
}

// Open the event stream: a channel pair, the consumer end to the service, the producer kept here.
//
// ONE STREAM AT A TIME, and a second `events` call replaces the first. There is one consumer of a
// display device in this system - the service that adopted it - and a driver holding two producers
// would be a driver deciding which of them is the real one.
fn open_event_stream(service: u64, request: &[u8], request_handles: &mut Handles, scanout: &mut Scanout<'_>) {
	if request.len() != 6 || !request_handles.is_empty() {
		return;
	}
	let corr: u32 = u32::from_le_bytes([request[2], request[3], request[4], request[5]]);
	let Some((producer, consumer)) = channel() else { return };
	if !send_blocking(service, &corr.to_le_bytes(), consumer) {
		close(producer);
		close(consumer);
		return;
	}
	if scanout.events != 0 {
		close(scanout.events);
	}
	scanout.events = producer;
}
