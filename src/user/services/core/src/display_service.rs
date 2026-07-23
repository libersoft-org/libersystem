// DisplayService - capability-scoped application surfaces over the system scanout.
//
// The service is the only userspace process that maps the physical display backing.
// Clients receive independent MemoryObject surfaces and present damage rectangles;
// the foreground surface is copied or nearest-neighbor scaled into the scanout. The
// first native-size client is the console. A later client temporarily becomes the
// foreground, and release, peer-close, or process death restores the console surface.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use pix::{Image, Rect, Target};
use proto::codec::Buffer;
use proto::system::display::{self, Service};
use proto::system::display_admin::{self, Service as AdminService};
use proto::system::{DisplayEvent, Error, PixelFormat, PresentationStats, SurfaceInfo};
use rt::*;

const MAX_DIM: u32 = 8192;
const REQUEST_MAX: usize = 128;
const REPLY_MAX: usize = 128;

struct Scanout {
	gpu: u64,
	handle: u64,
	addr: u64,
	fb: Framebuffer,
	width: u32,
	height: u32,
}

impl Scanout {
	fn available(&self) -> bool {
		self.addr != 0 && self.width != 0 && self.height != 0
	}
}

struct Surface {
	chan: u64,
	handle: u64,
	addr: u64,
	width: u32,
	height: u32,
	pitch: u32,
	focus_proof: u64,
	initialized: bool,
}

struct EventStream {
	chan: u64,
	producer: u64,
	seq: u32,
}

struct Client {
	chan: u64,
	task: u64,
}

#[derive(Default)]
struct PerfStats {
	presents: u64,
	direct_presents: u64,
	scaled_presents: u64,
	source_pixels: u64,
	output_pixels: u64,
	blit_ns: u64,
	flush_ns: u64,
	max_present_ns: u64,
}

impl PerfStats {
	fn snapshot(&self) -> PresentationStats {
		PresentationStats { presents: self.presents, direct_presents: self.direct_presents, scaled_presents: self.scaled_presents, source_pixels: self.source_pixels, output_pixels: self.output_pixels, blit_ns: self.blit_ns, flush_ns: self.flush_ns, max_present_ns: self.max_present_ns }
	}
}

struct DisplayState {
	scanout: Scanout,
	surfaces: Vec<Surface>,
	events: Vec<EventStream>,
	focus_control: u64,
	kill_control: u64,
	console: u64,
	active: u64,
	stats: PerfStats,
}

impl DisplayState {
	fn new(scanout: Scanout, focus_control: u64, kill_control: u64) -> DisplayState {
		DisplayState { scanout, surfaces: Vec::new(), events: Vec::new(), focus_control, kill_control, console: 0, active: 0, stats: PerfStats::default() }
	}

	fn surface_index(&self, chan: u64) -> Option<usize> {
		self.surfaces.iter().position(|surface: &Surface| surface.chan == chan)
	}

	fn acquire(&mut self, chan: u64, requested_width: u32, requested_height: u32) -> Result<SurfaceInfo, Error> {
		if (requested_width == 0) != (requested_height == 0) {
			return Err(Error::Invalid);
		}
		if !self.scanout.available() {
			return Err(Error::NotFound);
		}
		let native: bool = requested_width == 0;
		let width: u32 = if native { self.scanout.width } else { requested_width };
		let height: u32 = if native { self.scanout.height } else { requested_height };
		if width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM {
			return Err(Error::Invalid);
		}
		let pitch: u32 = width.checked_mul(4).ok_or(Error::Invalid)?;
		let len: u64 = (pitch as u64).checked_mul(height as u64).ok_or(Error::Invalid)?;
		let handle: i64 = unsafe { memory_object_create(len) };
		if handle < 0 {
			return Err(Error::Again);
		}
		let handle: u64 = handle as u64;
		let addr: u64 = match unsafe { map_object(handle) } {
			Some(addr) => addr,
			None => {
				unsafe { close(handle) };
				return Err(Error::Again);
			}
		};
		unsafe { core::ptr::write_bytes(addr as *mut u8, 0, len as usize) };
		let granted: i64 = unsafe { duplicate(handle, RIGHT_WRITE | RIGHT_MAP | RIGHT_TRANSFER) };
		if granted < 0 {
			unsafe {
				unmap_object(handle);
				close(handle);
			}
			return Err(Error::Again);
		}
		self.remove_surface(chan, false);
		self.surfaces.push(Surface { chan, handle, addr, width, height, pitch, focus_proof: 0, initialized: false });
		if self.console == 0 && native {
			self.console = chan;
		}
		if self.active == 0 || chan != self.console {
			self.set_active(chan);
		}
		Ok(SurfaceInfo { pixels: Buffer { handle: granted as u64, len }, width, height, pitch, format: PixelFormat::B8g8r8x8 })
	}

	fn present(&mut self, chan: u64, x: u32, y: u32, width: u32, height: u32) -> Result<(), Error> {
		let index: usize = self.surface_index(chan).ok_or(Error::Invalid)?;
		let surface: &Surface = &self.surfaces[index];
		let x1: u32 = x.checked_add(width).ok_or(Error::Invalid)?;
		let y1: u32 = y.checked_add(height).ok_or(Error::Invalid)?;
		if width == 0 || height == 0 || x1 > surface.width || y1 > surface.height {
			return Err(Error::Invalid);
		}
		if chan != self.active {
			return Ok(());
		}
		let source_pixels: u64 = width as u64 * height as u64;
		let start_ns: u64 = unsafe { clock_ns() };
		let blit: pix::BlitResult = self.blit(index, Rect { x, y, width, height });
		let blit_done_ns: u64 = unsafe { clock_ns() };
		let result: Result<(), Error> = self.flush((blit.rect.x, blit.rect.y, blit.rect.width, blit.rect.height));
		let done_ns: u64 = unsafe { clock_ns() };
		let blit_ns: u64 = blit_done_ns.saturating_sub(start_ns);
		let flush_ns: u64 = done_ns.saturating_sub(blit_done_ns);
		let total_ns: u64 = done_ns.saturating_sub(start_ns);
		self.stats.presents = self.stats.presents.saturating_add(1);
		if blit.direct {
			self.stats.direct_presents = self.stats.direct_presents.saturating_add(1);
		} else {
			self.stats.scaled_presents = self.stats.scaled_presents.saturating_add(1);
		}
		self.stats.source_pixels = self.stats.source_pixels.saturating_add(source_pixels);
		self.stats.output_pixels = self.stats.output_pixels.saturating_add(blit.pixels);
		self.stats.blit_ns = self.stats.blit_ns.saturating_add(blit_ns);
		self.stats.flush_ns = self.stats.flush_ns.saturating_add(flush_ns);
		self.stats.max_present_ns = self.stats.max_present_ns.max(total_ns);
		result
	}

	fn release(&mut self, chan: u64) -> Result<(), Error> {
		if self.surface_index(chan).is_none() {
			return Err(Error::Invalid);
		}
		self.remove_surface(chan, true);
		Ok(())
	}

	fn input_focus(&mut self, chan: u64) -> Result<u64, Error> {
		if chan != self.active {
			return Err(Error::Denied);
		}
		let index: usize = self.surface_index(chan).ok_or(Error::Invalid)?;
		let proof: u64 = core::mem::take(&mut self.surfaces[index].focus_proof);
		if proof == 0 { Err(Error::Again) } else { Ok(proof) }
	}

	fn revoke_focus(&mut self) {
		for surface in &mut self.surfaces {
			if surface.focus_proof != 0 {
				unsafe { close(surface.focus_proof) };
				surface.focus_proof = 0;
			}
		}
	}

	fn focus_command(&self, command: &[u8], handle: u64) -> bool {
		if self.focus_control == 0 || !unsafe { send_blocking(self.focus_control, command, handle) } {
			return false;
		}
		let mut reply: [u8; 8] = [0; 8];
		match unsafe { recv_blocking(self.focus_control, &mut reply) } {
			Received::Message { len, handle } => {
				if handle != 0 {
					unsafe { close(handle) };
				}
				len >= 2 && &reply[..2] == b"OK"
			}
			Received::Closed => false,
		}
	}

	fn set_active(&mut self, chan: u64) {
		self.revoke_focus();
		self.active = chan;
		if self.focus_control == 0 {
			return;
		}
		if chan == 0 {
			self.focus_command(b"CLEAR", 0);
			return;
		}
		if chan == self.console {
			self.focus_command(b"CONSOLE", 0);
			return;
		}
		let Some(index) = self.surface_index(chan) else { return };
		let (proof, registered): (u64, u64) = match unsafe { channel() } {
			Some(pair) => pair,
			None => return,
		};
		if self.focus_command(b"SET", registered) {
			self.surfaces[index].focus_proof = proof;
		} else {
			unsafe {
				close(proof);
				close(registered);
			}
		}
	}

	fn remove_surface(&mut self, chan: u64, restore: bool) {
		if let Some(index) = self.surface_index(chan) {
			let surface: Surface = self.surfaces.swap_remove(index);
			unsafe {
				if surface.focus_proof != 0 {
					close(surface.focus_proof);
				}
				unmap_object(surface.handle);
				close(surface.handle);
			}
		}
		if !restore {
			return;
		}
		if self.console == chan {
			self.console = 0;
		}
		if self.active == chan {
			let next: u64 = if self.console != 0 && self.surface_index(self.console).is_some() { self.console } else { 0 };
			self.set_active(next);
			if restore && self.active != 0 {
				self.present_active_full();
			}
		}
	}

	fn drop_client(&mut self, chan: u64) {
		self.remove_surface(chan, true);
		if let Some(index) = self.events.iter().position(|stream: &EventStream| stream.chan == chan) {
			let stream: EventStream = self.events.swap_remove(index);
			unsafe { close(stream.producer) };
		}
	}

	fn set_event_stream(&mut self, chan: u64, producer: u64) {
		if let Some(index) = self.events.iter().position(|stream: &EventStream| stream.chan == chan) {
			let old: EventStream = self.events.swap_remove(index);
			unsafe { close(old.producer) };
		}
		self.events.push(EventStream { chan, producer, seq: 0 });
	}

	fn notify_resize(&mut self) {
		let event = DisplayEvent { width: self.scanout.width, height: self.scanout.height };
		let mut frame: [u8; 32] = [0; 32];
		let mut i: usize = 0;
		while i < self.events.len() {
			let mut frame_handle: u64 = 0;
			let sent: bool = match display::events_frame(self.events[i].seq, &event, &mut frame, &mut frame_handle) {
				Some(n) => unsafe { send_blocking(self.events[i].producer, &frame[..n], frame_handle) },
				None => false,
			};
			if sent {
				self.events[i].seq = self.events[i].seq.wrapping_add(1);
				i += 1;
			} else {
				if frame_handle != 0 {
					unsafe { close(frame_handle) };
				}
				let dead: EventStream = self.events.swap_remove(i);
				unsafe { close(dead.producer) };
			}
		}
	}

	fn present_active_full(&mut self) {
		let Some(index) = self.surface_index(self.active) else { return };
		let width: u32 = self.surfaces[index].width;
		let height: u32 = self.surfaces[index].height;
		let blit: pix::BlitResult = self.blit(index, Rect { x: 0, y: 0, width, height });
		let _ = self.flush((blit.rect.x, blit.rect.y, blit.rect.width, blit.rect.height));
	}

	fn blit(&mut self, index: usize, damage: Rect) -> pix::BlitResult {
		let first: bool = !self.surfaces[index].initialized;
		self.surfaces[index].initialized = true;
		let surface: &Surface = &self.surfaces[index];
		let source_len: usize = surface.pitch as usize * surface.height as usize;
		let target_len: usize = self.scanout.fb.pitch as usize * self.scanout.height as usize;
		let source: &[u8] = unsafe { core::slice::from_raw_parts(surface.addr as *const u8, source_len) };
		let target: &mut [u8] = unsafe { core::slice::from_raw_parts_mut(self.scanout.addr as *mut u8, target_len) };
		pix::blit(Image { data: source, width: surface.width, height: surface.height, pitch: surface.pitch }, Target { data: target, width: self.scanout.width, height: self.scanout.height, pitch: self.scanout.fb.pitch, bytes_per_pixel: self.scanout.fb.bytes_per_pixel, red_shift: self.scanout.fb.red_shift, red_size: self.scanout.fb.red_size, green_shift: self.scanout.fb.green_shift, green_size: self.scanout.fb.green_size, blue_shift: self.scanout.fb.blue_shift, blue_size: self.scanout.fb.blue_size }, damage, first).expect("DisplayService validates surface and scanout bounds before blitting")
	}

	fn flush(&mut self, rect: (u32, u32, u32, u32)) -> Result<(), Error> {
		if self.scanout.gpu == 0 {
			return Ok(());
		}
		let mut msg: [u8; 23] = [0; 23];
		msg[..7].copy_from_slice(b"PRESENT");
		msg[7..11].copy_from_slice(&rect.0.to_le_bytes());
		msg[11..15].copy_from_slice(&rect.1.to_le_bytes());
		msg[15..19].copy_from_slice(&rect.2.to_le_bytes());
		msg[19..23].copy_from_slice(&rect.3.to_le_bytes());
		if !unsafe { send_blocking(self.scanout.gpu, &msg, 0) } {
			return Err(Error::Closed);
		}
		let mut reply: [u8; 64] = [0; 64];
		loop {
			match unsafe { recv_blocking(self.scanout.gpu, &mut reply) } {
				Received::Message { len, handle } if len >= 2 && &reply[..2] == b"OK" => {
					if handle != 0 {
						unsafe { close(handle) };
					}
					return Ok(());
				}
				Received::Message { len, handle } if len >= 3 && &reply[..3] == b"ERR" => {
					if handle != 0 {
						unsafe { close(handle) };
					}
					return Err(Error::Again);
				}
				Received::Message { len, handle } => {
					if self.handle_gpu_message(&reply[..len], handle) {
						self.notify_resize();
					}
				}
				Received::Closed => return Err(Error::Closed),
			}
		}
	}

	fn handle_gpu_message(&mut self, msg: &[u8], handle: u64) -> bool {
		if msg.len() >= 5 && &msg[..5] == b"FBNEW" && handle != 0 {
			let fb_len: usize = core::mem::size_of::<Framebuffer>();
			if msg.len() < 5 + fb_len + 8 {
				unsafe { close(handle) };
				return false;
			}
			let fb: Framebuffer = unsafe { (msg[5..].as_ptr() as *const Framebuffer).read_unaligned() };
			let width: u32 = read_u32(msg, 5 + fb_len);
			let height: u32 = read_u32(msg, 5 + fb_len + 4);
			let addr: i64 = unsafe { dma_buffer_map(handle) };
			if sys_is_err(addr as u64) || !valid_scanout(&fb, width, height) {
				unsafe {
					if !sys_is_err(addr as u64) {
						dma_buffer_unmap(handle);
					}
					close(handle);
				}
				return false;
			}
			let old: u64 = self.scanout.handle;
			self.scanout.handle = handle;
			self.scanout.addr = addr as u64;
			self.scanout.fb = fb;
			self.scanout.width = width;
			self.scanout.height = height;
			for surface in &mut self.surfaces {
				surface.initialized = false;
			}
			if old != 0 {
				unsafe {
					dma_buffer_unmap(old);
					close(old);
				}
			}
			return true;
		}
		if msg.len() >= 14 && &msg[..6] == b"RESIZE" {
			if handle != 0 {
				unsafe { close(handle) };
			}
			let width: u32 = read_u32(msg, 6);
			let height: u32 = read_u32(msg, 10);
			if width != 0 && height != 0 && width <= self.scanout.fb.width && height <= self.scanout.fb.height {
				self.scanout.width = width;
				self.scanout.height = height;
				for surface in &mut self.surfaces {
					surface.initialized = false;
				}
				return true;
			}
			return false;
		}
		if handle != 0 {
			unsafe { close(handle) };
		}
		false
	}
}

struct DisplayCall<'a> {
	state: &'a mut DisplayState,
	chan: u64,
}

impl Service for DisplayCall<'_> {
	fn acquire(&mut self, width: u32, height: u32) -> Result<SurfaceInfo, Error> {
		self.state.acquire(self.chan, width, height)
	}

	fn present(&mut self, x: u32, y: u32, width: u32, height: u32) -> Result<(), Error> {
		self.state.present(self.chan, x, y, width, height)
	}

	fn release(&mut self) -> Result<(), Error> {
		self.state.release(self.chan)
	}

	fn events(&mut self) -> Vec<DisplayEvent> {
		Vec::new()
	}

	fn input_focus(&mut self) -> Result<u64, Error> {
		self.state.input_focus(self.chan)
	}
}

struct AdminCall<'a> {
	clients: &'a mut Vec<Client>,
	stats: &'a PerfStats,
}

impl AdminService for AdminCall<'_> {
	fn bind(&mut self, task: u64) -> Result<u64, Error> {
		if task == 0 {
			return Err(Error::Invalid);
		}
		let (server, client): (u64, u64) = match unsafe { channel() } {
			Some(pair) => pair,
			None => {
				unsafe { close(task) };
				return Err(Error::Again);
			}
		};
		self.clients.push(Client { chan: server, task });
		Ok(client)
	}

	fn stats(&mut self) -> PresentationStats {
		self.stats.snapshot()
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 128] = [0; 128];
	unsafe {
		let gpu: u64 = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } if len >= 3 && &buf[..3] == b"GPU" => handle,
			_ => fail_bootstrap(bootstrap, b"gpu", b"driver channel not delivered"),
		};
		let focus_control: u64 = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } if len >= 5 && &buf[..5] == b"FOCUS" => handle,
			_ => fail_bootstrap(bootstrap, b"focus", b"input focus channel not delivered"),
		};
		let kill_control: u64 = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } if len >= 4 && &buf[..4] == b"KILL" => handle,
			_ => fail_bootstrap(bootstrap, b"kill", b"emergency input channel not delivered"),
		};
		let admin: u64 = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } if len >= 5 && &buf[..5] == b"ADMIN" => handle,
			_ => fail_bootstrap(bootstrap, b"admin", b"display admin channel not delivered"),
		};
		let service: u64 = recv_tagged(bootstrap, &mut buf, b"SERVE").unwrap_or_else(|| fail_bootstrap(bootstrap, b"serve", b"missing serve channel"));
		let scanout: Scanout = init_scanout(gpu, &mut buf);
		if !scanout.available() {
			fail_bootstrap(bootstrap, b"display", b"no framebuffer available");
		}
		send_blocking(bootstrap, b"DisplayService: online", 0);
		serve_display(service, admin, DisplayState::new(scanout, focus_control, kill_control));
	}
}

unsafe fn init_scanout(gpu: u64, buf: &mut [u8]) -> Scanout {
	unsafe {
		if gpu != 0 {
			send_blocking(gpu, b"FB", 0);
			if let Received::Message { len, handle } = recv_blocking(gpu, buf) {
				let fb_len: usize = core::mem::size_of::<Framebuffer>();
				if handle != 0 && len >= fb_len + 8 {
					let fb: Framebuffer = (buf.as_ptr() as *const Framebuffer).read_unaligned();
					let width: u32 = read_u32(buf, fb_len);
					let height: u32 = read_u32(buf, fb_len + 4);
					let addr: i64 = dma_buffer_map(handle);
					if !sys_is_err(addr as u64) && valid_scanout(&fb, width, height) {
						return Scanout { gpu, handle, addr: addr as u64, fb, width, height };
					}
					if !sys_is_err(addr as u64) {
						dma_buffer_unmap(handle);
					}
					close(handle);
				}
			}
		}
		let mut fb: Framebuffer = Framebuffer::default();
		let addr: i64 = framebuffer_map(&mut fb);
		if !sys_is_err(addr as u64) && valid_scanout(&fb, fb.width, fb.height) { Scanout { gpu: 0, handle: 0, addr: addr as u64, width: fb.width, height: fb.height, fb } } else { Scanout { gpu: 0, handle: 0, addr: 0, fb: Framebuffer::default(), width: 0, height: 0 } }
	}
}

unsafe fn serve_display(root: u64, admin: u64, mut state: DisplayState) -> ! {
	unsafe {
		let mut clients: Vec<Client> = alloc::vec![Client { chan: root, task: 0 }];
		let mut request: [u8; REQUEST_MAX] = [0; REQUEST_MAX];
		let mut reply: [u8; REPLY_MAX] = [0; REPLY_MAX];
		loop {
			let mut waits: Vec<u64> = Vec::with_capacity(clients.len() + 3);
			if state.scanout.gpu != 0 {
				waits.push(state.scanout.gpu);
			}
			if state.kill_control != 0 {
				waits.push(state.kill_control);
			}
			waits.push(admin);
			waits.extend(clients.iter().map(|client| client.chan));
			let ready: i64 = wait_any(&waits, 0);
			if ready < 0 {
				continue;
			}
			let gpu_first: bool = state.scanout.gpu != 0;
			if gpu_first && ready == 0 {
				match recv_blocking(state.scanout.gpu, &mut request) {
					Received::Message { len, handle } => {
						if state.handle_gpu_message(&request[..len], handle) {
							state.notify_resize();
							state.present_active_full();
						}
					}
					Received::Closed => state.scanout.gpu = 0,
				}
				continue;
			}
			let kill_index: usize = gpu_first as usize;
			let kill_present: bool = state.kill_control != 0;
			if kill_present && ready as usize == kill_index {
				match recv_blocking(state.kill_control, &mut request) {
					Received::Message { len, handle } => {
						if handle != 0 {
							close(handle);
						}
						if len >= 4
							&& &request[..4] == b"KILL"
							&& state.active != 0 && state.active != state.console
							&& let Some(victim) = clients.iter().position(|client| client.chan == state.active)
						{
							let chan: u64 = clients[victim].chan;
							if clients[victim].task != 0 {
								let _ = signal(clients[victim].task, SIG_KILL);
							}
							state.drop_client(chan);
							close(chan);
							let victim: Client = clients.swap_remove(victim);
							if victim.task != 0 {
								close(victim.task);
							}
						}
					}
					Received::Closed => state.kill_control = 0,
				}
				continue;
			}
			let admin_index: usize = gpu_first as usize + kill_present as usize;
			if ready as usize == admin_index {
				match recv_blocking(admin, &mut request) {
					Received::Message { len, mut handle } => {
						let mut reply_handle: u64 = 0;
						let mut call = AdminCall { clients: &mut clients, stats: &state.stats };
						if let Some(n) = display_admin::dispatch(&mut call, &request[..len], &mut handle, &mut reply, &mut reply_handle) {
							if !send_blocking(admin, &reply[..n], reply_handle) && reply_handle != 0 {
								close(reply_handle);
							}
						} else if reply_handle != 0 {
							close(reply_handle);
						}
						if handle != 0 {
							close(handle);
						}
					}
					Received::Closed => exit(),
				}
				continue;
			}
			let client_index: usize = ready as usize - admin_index - 1;
			let chan: u64 = clients[client_index].chan;
			match recv_blocking(chan, &mut request) {
				Received::Message { len, handle } if len == 0 => {
					if handle != 0 {
						close(handle);
					}
					if client_index == 0 {
						exit();
					}
					state.drop_client(chan);
					close(chan);
					clients.swap_remove(client_index);
				}
				Received::Message { len, mut handle } => {
					let op: u16 = if len >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
					if op == HEARTBEAT_OP {
						send_blocking(chan, b"PONG", 0);
					} else if op == CONNECT_OP && clients[client_index].task == 0 {
						match channel() {
							Some((mine, theirs)) => {
								clients.push(Client { chan: mine, task: 0 });
								send_blocking(chan, &[], theirs);
							}
							None => {
								send_blocking(chan, &[], 0);
							}
						}
					} else if op == display::OP_EVENTS {
						open_events(chan, &request[..len], &mut handle, &mut state);
					} else {
						let mut reply_handle: u64 = 0;
						let mut call = DisplayCall { state: &mut state, chan };
						if let Some(n) = display::dispatch(&mut call, &request[..len], &mut handle, &mut reply, &mut reply_handle) {
							if !send_blocking(chan, &reply[..n], reply_handle) && reply_handle != 0 {
								close(reply_handle);
							}
						} else if reply_handle != 0 {
							close(reply_handle);
						}
					}
					if handle != 0 {
						close(handle);
					}
				}
				Received::Closed => {
					if client_index == 0 {
						exit();
					}
					state.drop_client(chan);
					close(chan);
					let client: Client = clients.swap_remove(client_index);
					if client.task != 0 {
						close(client.task);
					}
				}
			}
		}
	}
}

fn open_events(chan: u64, request: &[u8], request_handle: &mut u64, state: &mut DisplayState) {
	if request.len() != 6 || *request_handle != 0 {
		return;
	}
	let corr: u32 = read_u32(request, 2);
	*request_handle = 0;
	let (producer, consumer): (u64, u64) = match unsafe { channel() } {
		Some(pair) => pair,
		None => return,
	};
	if unsafe { send_blocking(chan, &corr.to_le_bytes(), consumer) } {
		state.set_event_stream(chan, producer);
	} else {
		unsafe {
			close(producer);
			close(consumer);
		}
	}
}

fn valid_scanout(fb: &Framebuffer, width: u32, height: u32) -> bool {
	width != 0 && height != 0 && width <= fb.width && height <= fb.height && fb.bytes_per_pixel != 0 && fb.bytes_per_pixel <= 4 && fb.pitch >= fb.width.saturating_mul(fb.bytes_per_pixel)
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
	u32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
}
