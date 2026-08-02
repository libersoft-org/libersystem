// imgview - governed fullscreen image viewer.
//
// The tool reads an image through its bundled volume grants, decodes it into a
// bounded B8G8R8X8 buffer, presents through its process-bound DisplayService
// connection, and consumes only the focus-gated raw-key stream. It never reaches
// a framebuffer, input device, or storage device directly.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use keys::usage;
use pix::{Image, Target};
use proto::system::{OpenOpts, input};
use rt::*;
use storage_proto::path;
use volume_client::VolumeClient;

const USAGE: &[u8] = b"Usage: imgview <image>\nDisplays a still image or composited animation frame 0; animation playback is not supported.\nControls: +/= zoom in, - zoom out, hold arrows to pan, Esc/q quit.\n";
const TTY_RAW_ON: &[u8] = b"\x1b[?9001h";
const TTY_RAW_OFF: &[u8] = b"\x1b[?9001l";
const ZOOM_MIN: u32 = 100;
const ZOOM_MAX: u32 = 800;
const ZOOM_STEP: u32 = 5;
const PAN_REPEAT_TICKS: u64 = 2;
const SERIAL_ESCAPE_TICKS: u64 = 2;
const PAN_STEP_DIVISOR: u32 = 64;
const HELD_LEFT: u8 = 1 << 0;
const HELD_RIGHT: u8 = 1 << 1;
const HELD_UP: u8 = 1 << 2;
const HELD_DOWN: u8 = 1 << 3;
const HELD_ZOOM_IN: u8 = 1 << 4;
const HELD_ZOOM_OUT: u8 = 1 << 5;
const HELD_PAN: u8 = HELD_LEFT | HELD_RIGHT | HELD_UP | HELD_DOWN;
const HELD_ZOOM: u8 = HELD_ZOOM_IN | HELD_ZOOM_OUT;

struct DecodedImage {
	width: u32,
	height: u32,
	pitch: u32,
	pixels: Vec<u8>,
}

#[derive(Clone, Copy)]
struct Viewport {
	base_width: u32,
	base_height: u32,
	zoom: u32,
	width: u32,
	height: u32,
	pan_x: u32,
	pan_y: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewAction {
	None,
	Redraw,
	Exit,
}

#[derive(Clone, Copy)]
enum SerialInput {
	Ground,
	Escape,
	Csi,
}

impl Viewport {
	fn new(image: &DecodedImage, framebuffer: Framebuffer) -> Option<Self> {
		let (base_width, base_height) = fit_dimensions(image.width, image.height, framebuffer)?;
		Some(Viewport { base_width, base_height, zoom: ZOOM_MIN, width: base_width, height: base_height, pan_x: 0, pan_y: 0 })
	}

	fn set_zoom(&mut self, zoom: u32, framebuffer: Framebuffer) -> bool {
		let zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
		if zoom == self.zoom {
			return false;
		}
		let old_width = self.width.max(1);
		let old_height = self.height.max(1);
		let old_center_x = visible_center(self.width, self.pan_x, framebuffer.width);
		let old_center_y = visible_center(self.height, self.pan_y, framebuffer.height);
		let Some(width) = scaled_dimension(self.base_width, zoom) else {
			return false;
		};
		let Some(height) = scaled_dimension(self.base_height, zoom) else {
			return false;
		};
		self.zoom = zoom;
		self.width = width;
		self.height = height;
		let center_x = old_center_x * width as u64 / old_width as u64;
		let center_y = old_center_y * height as u64 / old_height as u64;
		self.pan_x = pan_for_center(center_x, width, framebuffer.width);
		self.pan_y = pan_for_center(center_y, height, framebuffer.height);
		true
	}

	fn zoom_in(&mut self, framebuffer: Framebuffer) -> bool {
		self.set_zoom(self.zoom.saturating_add(ZOOM_STEP), framebuffer)
	}

	fn zoom_out(&mut self, framebuffer: Framebuffer) -> bool {
		self.set_zoom(self.zoom.saturating_sub(ZOOM_STEP), framebuffer)
	}

	fn can_pan(&self, framebuffer: Framebuffer) -> bool {
		self.width > framebuffer.width || self.height > framebuffer.height
	}

	fn pan(&mut self, code: u16, framebuffer: Framebuffer) -> bool {
		let old_x = self.pan_x;
		let old_y = self.pan_y;
		let step_x = (framebuffer.width / PAN_STEP_DIVISOR).max(1);
		let step_y = (framebuffer.height / PAN_STEP_DIVISOR).max(1);
		match code {
			usage::LEFT => self.pan_x = self.pan_x.saturating_sub(step_x),
			usage::RIGHT => self.pan_x = self.pan_x.saturating_add(step_x).min(self.width.saturating_sub(framebuffer.width)),
			usage::UP => self.pan_y = self.pan_y.saturating_sub(step_y),
			usage::DOWN => self.pan_y = self.pan_y.saturating_add(step_y).min(self.height.saturating_sub(framebuffer.height)),
			_ => {}
		}
		self.pan_x != old_x || self.pan_y != old_y
	}
}

fn fit_dimensions(width: u32, height: u32, framebuffer: Framebuffer) -> Option<(u32, u32)> {
	if width == 0 || height == 0 || framebuffer.width == 0 || framebuffer.height == 0 {
		return None;
	}
	if framebuffer.width as u64 * height as u64 <= framebuffer.height as u64 * width as u64 { Some((framebuffer.width, ((height as u64 * framebuffer.width as u64) / width as u64).max(1) as u32)) } else { Some((((width as u64 * framebuffer.height as u64) / height as u64).max(1) as u32, framebuffer.height)) }
}

fn scaled_dimension(base: u32, zoom: u32) -> Option<u32> {
	let scaled = (base as u64 * zoom as u64).div_ceil(100).max(if zoom > ZOOM_MIN { base as u64 + 1 } else { base as u64 });
	u32::try_from(scaled).ok()
}

fn visible_center(width: u32, pan: u32, viewport: u32) -> u64 {
	if width > viewport { pan as u64 + viewport as u64 / 2 } else { width as u64 / 2 }
}

fn pan_for_center(center: u64, width: u32, viewport: u32) -> u32 {
	if width <= viewport { 0 } else { center.saturating_sub(viewport as u64 / 2).min(width.saturating_sub(viewport) as u64) as u32 }
}

fn arrow_mask(code: u16) -> u8 {
	match code {
		usage::LEFT => HELD_LEFT,
		usage::RIGHT => HELD_RIGHT,
		usage::UP => HELD_UP,
		usage::DOWN => HELD_DOWN,
		_ => 0,
	}
}

fn handle_code(code: u16, pressed: bool, viewport: &mut Viewport, framebuffer: Framebuffer, held: &mut u8) -> ViewAction {
	if pressed && matches!(code, usage::ESCAPE | usage::Q) {
		return ViewAction::Exit;
	}
	if matches!(code, usage::PLUS | usage::KEYPAD_PLUS) {
		if pressed {
			*held = (*held & !HELD_ZOOM_OUT) | HELD_ZOOM_IN;
			return if viewport.zoom_in(framebuffer) { ViewAction::Redraw } else { ViewAction::None };
		}
		*held &= !HELD_ZOOM_IN;
		return ViewAction::None;
	}
	if matches!(code, usage::MINUS | usage::KEYPAD_MINUS) {
		if pressed {
			*held = (*held & !HELD_ZOOM_IN) | HELD_ZOOM_OUT;
			return if viewport.zoom_out(framebuffer) { ViewAction::Redraw } else { ViewAction::None };
		}
		*held &= !HELD_ZOOM_OUT;
		return ViewAction::None;
	}
	let mask = arrow_mask(code);
	if mask == 0 {
		return ViewAction::None;
	}
	if pressed {
		if !viewport.can_pan(framebuffer) {
			return ViewAction::None;
		}
		*held |= mask;
		if viewport.pan(code, framebuffer) { ViewAction::Redraw } else { ViewAction::None }
	} else {
		*held &= !mask;
		ViewAction::None
	}
}

fn handle_serial_byte(state: &mut SerialInput, escape_deadline: &mut u64, byte: u8, viewport: &mut Viewport, framebuffer: Framebuffer) -> ViewAction {
	match *state {
		SerialInput::Ground => match byte {
			0x1b => {
				*state = SerialInput::Escape;
				*escape_deadline = unsafe { clock() }.saturating_add(SERIAL_ESCAPE_TICKS);
				ViewAction::None
			}
			b'q' => ViewAction::Exit,
			b'+' | b'=' => {
				if viewport.zoom_in(framebuffer) {
					ViewAction::Redraw
				} else {
					ViewAction::None
				}
			}
			b'-' => {
				if viewport.zoom_out(framebuffer) {
					ViewAction::Redraw
				} else {
					ViewAction::None
				}
			}
			_ => ViewAction::None,
		},
		SerialInput::Escape => {
			*escape_deadline = 0;
			*state = if byte == b'[' { SerialInput::Csi } else { SerialInput::Ground };
			if byte == b'[' { ViewAction::None } else { ViewAction::Exit }
		}
		SerialInput::Csi => {
			*escape_deadline = 0;
			*state = SerialInput::Ground;
			let code = match byte {
				b'A' => usage::UP,
				b'B' => usage::DOWN,
				b'C' => usage::RIGHT,
				b'D' => usage::LEFT,
				_ => return ViewAction::None,
			};
			if viewport.can_pan(framebuffer) && viewport.pan(code, framebuffer) { ViewAction::Redraw } else { ViewAction::None }
		}
	}
}

fn zoom_held(viewport: &mut Viewport, framebuffer: Framebuffer, held: u8) -> bool {
	let mut changed = false;
	if held & HELD_ZOOM_IN != 0 {
		changed |= viewport.zoom_in(framebuffer);
	}
	if held & HELD_ZOOM_OUT != 0 {
		changed |= viewport.zoom_out(framebuffer);
	}
	changed
}

fn pan_held(viewport: &mut Viewport, framebuffer: Framebuffer, held: u8) -> bool {
	let mut changed = false;
	if held & HELD_LEFT != 0 {
		changed |= viewport.pan(usage::LEFT, framebuffer);
	}
	if held & HELD_RIGHT != 0 {
		changed |= viewport.pan(usage::RIGHT, framebuffer);
	}
	if held & HELD_UP != 0 {
		changed |= viewport.pan(usage::UP, framebuffer);
	}
	if held & HELD_DOWN != 0 {
		changed |= viewport.pan(usage::DOWN, framebuffer);
	}
	changed
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0; 256];
	unsafe {
		inherit_stdout(bootstrap);
		let arg: Vec<u8> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => exit(),
		};
		let system = recv_tagged(bootstrap, &mut buf, b"SYSTEM").unwrap_or(0);
		let media = recv_tagged(bootstrap, &mut buf, b"MEDIA").unwrap_or(0);
		let iso = recv_tagged(bootstrap, &mut buf, b"ISO").unwrap_or(0);
		let udf = recv_tagged(bootstrap, &mut buf, b"UDF").unwrap_or(0);
		let usb = recv_tagged(bootstrap, &mut buf, b"USB").unwrap_or(0);
		// Two more volumes follow USB in the bundle; drained so nothing is left to be read as
		// the next message.
		let _ = recv_tagged(bootstrap, &mut buf, b"RAM");
		let _ = recv_tagged(bootstrap, &mut buf, b"TMP");
		let display_channel = recv_tagged(bootstrap, &mut buf, b"DISPLAY").unwrap_or(0);
		let input_channel = recv_tagged(bootstrap, &mut buf, b"INPUT_KEYS").unwrap_or(0);
		let cwd: Vec<u8> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => Vec::new(),
		};
		let cwd = core::str::from_utf8(&cwd).unwrap_or("");
		let arg = trim(&arg);
		if arg == b"--help" {
			print(USAGE);
			close_if_present(display_channel);
			close_if_present(input_channel);
			exit();
		}
		if arg.is_empty() || arg.iter().any(u8::is_ascii_whitespace) {
			print(USAGE);
			close_if_present(display_channel);
			close_if_present(input_channel);
			exit();
		}
		let Some(uri) = path::resolve(cwd, arg) else {
			print(b"imgview: invalid path\n");
			exit();
		};
		let storage = path::volume_client(cwd, arg, system, media, iso, udf, usb);
		let Some(image) = load_image(storage, &uri) else {
			close_if_present(display_channel);
			close_if_present(input_channel);
			exit();
		};
		if display_channel == 0 || input_channel == 0 {
			print(b"imgview: graphical capabilities unavailable\n");
			close_if_present(display_channel);
			close_if_present(input_channel);
			exit();
		}
		show(display_channel, input_channel, image);
		close(input_channel);
		close(display_channel);
	}
	exit();
}

fn trim(mut bytes: &[u8]) -> &[u8] {
	while bytes.first().is_some_and(|byte| byte.is_ascii_whitespace()) {
		bytes = &bytes[1..];
	}
	while bytes.last().is_some_and(|byte| byte.is_ascii_whitespace()) {
		bytes = &bytes[..bytes.len() - 1];
	}
	bytes
}

unsafe fn close_if_present(handle: u64) {
	if handle != 0 {
		unsafe { close(handle) };
	}
}

unsafe fn load_image(storage: u64, uri: &str) -> Option<DecodedImage> {
	unsafe {
		if storage == 0 {
			print(b"imgview: volume unavailable\n");
			return None;
		}
		let opts = OpenOpts { path: String::from(uri), write: false, create: false };
		let mut client = VolumeClient::new(storage);
		let opened = match client.open(&opts) {
			Some(Ok(opened)) if opened.file != 0 => opened,
			_ => {
				print(b"imgview: cannot open image\n");
				return None;
			}
		};
		let len = match usize::try_from(opened.size) {
			Ok(len) if len != 0 => len,
			_ => {
				close(opened.file);
				print(b"imgview: invalid image size\n");
				return None;
			}
		};
		let mapped = match map_object(opened.file) {
			Some(mapped) => mapped,
			None => {
				close(opened.file);
				print(b"imgview: cannot map image\n");
				return None;
			}
		};
		let bytes = core::slice::from_raw_parts(mapped as *const u8, len);
		let decoded = imgconv::decode_frame(bytes, 0).ok().and_then(|(_, image)| {
			let pixels = image.to_bgrx().ok()?;
			Some(DecodedImage { width: image.width, height: image.height, pitch: image.pitch, pixels })
		});
		unmap_object(opened.file);
		close(opened.file);
		match decoded {
			Some(image) => Some(image),
			None => {
				print(b"imgview: unsupported or invalid image\n");
				None
			}
		}
	}
}

unsafe fn show(display_channel: u64, input_channel: u64, image: DecodedImage) {
	unsafe {
		let display = surface::connect(display_channel);
		let Some(surface) = surface::acquire(&display, 0, 0).and_then(Result::ok) else {
			print(b"imgview: cannot acquire display\n");
			return;
		};
		let framebuffer = surface.framebuffer();
		let target_len = match (framebuffer.pitch as usize).checked_mul(framebuffer.height as usize) {
			Some(len) => len,
			None => return,
		};
		let Some(mut viewport) = Viewport::new(&image, framebuffer) else {
			let _ = surface::release(&display);
			return;
		};
		if !present_view(&display, &surface, framebuffer, target_len, &image, &viewport) {
			let _ = surface::release(&display);
			return;
		}
		let Some(focus) = surface::input_focus(&display).and_then(Result::ok) else {
			let _ = surface::release(&display);
			return;
		};
		let Some(key_stream) = surface::subscribe_keys(input_channel, focus) else {
			let _ = surface::release(&display);
			return;
		};
		let stdin_channel = stdin();
		if stdin_channel != 0 {
			print(TTY_RAW_ON);
		}
		let mut key_frame: [u8; 32] = [0; 32];
		let mut stdin_frame: [u8; 32] = [0; 32];
		let mut held: u8 = 0;
		let mut serial_input = SerialInput::Ground;
		let mut serial_escape_deadline: u64 = 0;
		let mut next_repeat = clock().saturating_add(PAN_REPEAT_TICKS);
		let mut exit_requested = false;
		while !exit_requested {
			let repeat_pan = held & HELD_PAN != 0 && viewport.can_pan(framebuffer);
			let repeat_zoom = held & HELD_ZOOM != 0;
			let repeat_deadline = if repeat_pan || repeat_zoom { next_repeat } else { 0 };
			let deadline = match (repeat_deadline, serial_escape_deadline) {
				(0, 0) => 0,
				(0, deadline) | (deadline, 0) => deadline,
				(repeat, escape) => repeat.min(escape),
			};
			let wait_for_escape = serial_escape_deadline != 0 && (repeat_deadline == 0 || serial_escape_deadline <= repeat_deadline);
			let ready = if stdin_channel != 0 {
				let waits = [key_stream, stdin_channel];
				if deadline == 0 {
					wait_any(&waits, 0)
				} else if wait_for_escape {
					wait_any(&waits, deadline)
				} else {
					wait_any_periodic(&waits, deadline)
				}
			} else {
				let waits = [key_stream];
				if deadline == 0 {
					wait_any(&waits, 0)
				} else if wait_for_escape {
					wait_any(&waits, deadline)
				} else {
					wait_any_periodic(&waits, deadline)
				}
			};
			if ready == ERR_TIMED_OUT {
				let now = clock();
				if serial_escape_deadline != 0 && now >= serial_escape_deadline {
					exit_requested = true;
					continue;
				}
				if now >= next_repeat {
					if zoom_held(&mut viewport, framebuffer, held) || pan_held(&mut viewport, framebuffer, held) {
						let _ = present_view(&display, &surface, framebuffer, target_len, &image, &viewport);
					}
					next_repeat = now.saturating_add(PAN_REPEAT_TICKS);
				}
				continue;
			}
			if ready < 0 {
				break;
			}
			if ready == 0 {
				match recv_blocking(key_stream, &mut key_frame) {
					Received::Message { len, handle } => {
						let mut frame_handle = handle;
						if let Some(event) = input::subscribe_keys_read(&key_frame[..len], &mut frame_handle) {
							let action = handle_code(event.code, event.pressed, &mut viewport, framebuffer, &mut held);
							if action == ViewAction::Exit {
								exit_requested = true;
							} else if action == ViewAction::Redraw {
								next_repeat = clock().saturating_add(PAN_REPEAT_TICKS);
								let _ = present_view(&display, &surface, framebuffer, target_len, &image, &viewport);
							}
						}
						if frame_handle != 0 {
							close(frame_handle);
						}
					}
					Received::Closed => break,
				}
			} else if stdin_channel != 0 {
				match recv_blocking(stdin_channel, &mut stdin_frame) {
					Received::Message { len, handle } => {
						if handle != 0 {
							close(handle);
						}
						for &byte in &stdin_frame[..len] {
							let action = handle_serial_byte(&mut serial_input, &mut serial_escape_deadline, byte, &mut viewport, framebuffer);
							if action == ViewAction::Exit {
								exit_requested = true;
								break;
							}
							if action == ViewAction::Redraw {
								next_repeat = clock().saturating_add(PAN_REPEAT_TICKS);
								let _ = present_view(&display, &surface, framebuffer, target_len, &image, &viewport);
							}
						}
					}
					Received::Closed => break,
				}
			}
		}
		close(key_stream);
		if stdin_channel != 0 {
			print(TTY_RAW_OFF);
			set_stdin(0);
		}
		let _ = surface::release(&display);
	}
}

fn target(data: &mut [u8], framebuffer: Framebuffer) -> Target<'_> {
	Target { data, width: framebuffer.width, height: framebuffer.height, pitch: framebuffer.pitch, bytes_per_pixel: framebuffer.bytes_per_pixel, red_shift: framebuffer.red_shift, red_size: framebuffer.red_size, green_shift: framebuffer.green_shift, green_size: framebuffer.green_size, blue_shift: framebuffer.blue_shift, blue_size: framebuffer.blue_size }
}

unsafe fn present_view(display: &surface::Client, surface: &surface::Mapping, framebuffer: Framebuffer, target_len: usize, image: &DecodedImage, viewport: &Viewport) -> bool {
	let output = core::slice::from_raw_parts_mut(surface.addr() as *mut u8, target_len);
	let Some(blit) = pix::blit_view(Image { data: &image.pixels, width: image.width, height: image.height, pitch: image.pitch }, target(output, framebuffer), viewport.width, viewport.height, viewport.pan_x, viewport.pan_y) else {
		return false;
	};
	matches!(surface::present(display, blit.rect), Some(Ok(())))
}
