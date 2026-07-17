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
use ipc_client::ChannelTransport;
use keys::usage;
use pix::{Image, Rect, Target};
use proto::path;
use proto::system::{OpenOpts, input, volume};
use rt::*;

const USAGE: &[u8] = b"Usage: imgview <image>\nDisplays a still image or composited animation frame 0; animation playback is not supported.\n";

struct DecodedImage {
	width: u32,
	height: u32,
	pitch: u32,
	pixels: Vec<u8>,
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
		let display_channel = recv_tagged(bootstrap, &mut buf, b"DISPLAY").unwrap_or(0);
		let input_channel = recv_tagged(bootstrap, &mut buf, b"INPUT_KEYS").unwrap_or(0);
		let cwd: Vec<u8> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => Vec::new(),
		};
		let cwd = core::str::from_utf8(&cwd).unwrap_or("");
		let arg = tools::trim(&arg);
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
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
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
		let target = core::slice::from_raw_parts_mut(surface.addr() as *mut u8, target_len);
		let presented = render_fit(&image, framebuffer, target);
		let Some(blit) = presented else {
			let _ = surface::release(&display);
			return;
		};
		if !matches!(surface::present(&display, blit.rect), Some(Ok(()))) {
			let _ = surface::release(&display);
			return;
		}
		let Some(focus) = surface::input_focus(&display).and_then(Result::ok) else {
			let _ = surface::release(&display);
			return;
		};
		let mut input_client = input::Client::new(ChannelTransport { chan: input_channel });
		let Some(key_stream) = input_client.subscribe_keys(&focus) else {
			let _ = surface::release(&display);
			return;
		};
		let mut frame: [u8; 32] = [0; 32];
		let pannable = image.width > framebuffer.width || image.height > framebuffer.height;
		let mut native = false;
		let mut pan_x = 0u32;
		let mut pan_y = 0u32;
		while let Received::Message { len, handle } = recv_blocking(key_stream, &mut frame) {
			let mut frame_handle = handle;
			let event = input::subscribe_keys_read(&frame[..len], &mut frame_handle);
			if frame_handle != 0 {
				close(frame_handle);
			}
			if matches!(event, Some(ref event) if event.pressed && matches!(event.code, usage::ESCAPE | usage::Q)) {
				break;
			}
			let Some(event) = event.filter(|event| event.pressed && pannable && matches!(event.code, usage::LEFT | usage::RIGHT | usage::UP | usage::DOWN)) else {
				continue;
			};
			if !native {
				native = true;
				pan_x = image.width.saturating_sub(framebuffer.width) / 2;
				pan_y = image.height.saturating_sub(framebuffer.height) / 2;
			}
			let step_x = (framebuffer.width / 8).max(1);
			let step_y = (framebuffer.height / 8).max(1);
			match event.code {
				usage::LEFT => pan_x = pan_x.saturating_sub(step_x),
				usage::RIGHT => pan_x = pan_x.saturating_add(step_x).min(image.width.saturating_sub(framebuffer.width)),
				usage::UP => pan_y = pan_y.saturating_sub(step_y),
				usage::DOWN => pan_y = pan_y.saturating_add(step_y).min(image.height.saturating_sub(framebuffer.height)),
				_ => {}
			}
			let target = core::slice::from_raw_parts_mut(surface.addr() as *mut u8, target_len);
			if let Some(blit) = render_crop(&image, framebuffer, target, pan_x, pan_y) {
				let _ = surface::present(&display, blit.rect);
			}
		}
		close(key_stream);
		let _ = surface::release(&display);
	}
}

fn target(data: &mut [u8], framebuffer: Framebuffer) -> Target<'_> {
	Target { data, width: framebuffer.width, height: framebuffer.height, pitch: framebuffer.pitch, bytes_per_pixel: framebuffer.bytes_per_pixel, red_shift: framebuffer.red_shift, red_size: framebuffer.red_size, green_shift: framebuffer.green_shift, green_size: framebuffer.green_size, blue_shift: framebuffer.blue_shift, blue_size: framebuffer.blue_size }
}

fn render_fit(image: &DecodedImage, framebuffer: Framebuffer, output: &mut [u8]) -> Option<pix::BlitResult> {
	pix::blit(Image { data: &image.pixels, width: image.width, height: image.height, pitch: image.pitch }, target(output, framebuffer), Rect { x: 0, y: 0, width: image.width, height: image.height }, true)
}

fn render_crop(image: &DecodedImage, framebuffer: Framebuffer, output: &mut [u8], pan_x: u32, pan_y: u32) -> Option<pix::BlitResult> {
	pix::blit_crop(Image { data: &image.pixels, width: image.width, height: image.height, pitch: image.pitch }, target(output, framebuffer), pan_x, pan_y)
}
