#![no_std]

extern crate alloc;

use alloc::rc::Rc;
use base_proto::generated::liber::base::v1::Error;
use core::cell::RefCell;
use display_proto::generated::liber::display::v1::{DisplayEvent, PixelFormat, SurfaceInfo, display};
use input_proto::generated::liber::input::v1::input;
use ipc_client::ChannelTransport;
use rt::{Framebuffer, close, map_object, unmap_object};

pub use pix::{BlitResult, Image, Rect, Target};

pub type Client = Rc<RefCell<display::Client<ChannelTransport>>>;

pub fn connect(channel: u64) -> Client {
	Rc::new(RefCell::new(display::Client::new(ChannelTransport { chan: channel })))
}

pub struct Mapping {
	handle: u64,
	addr: u64,
	framebuffer: Framebuffer,
}

impl Mapping {
	pub fn from_info(info: SurfaceInfo) -> Option<Mapping> {
		let handle = info.pixels.handle;
		let expected = (info.pitch as u64).checked_mul(info.height as u64)?;
		if handle == 0 || info.format != PixelFormat::B8g8r8x8 || info.width == 0 || info.height == 0 || info.pitch < info.width.checked_mul(4)? || info.pixels.len < expected {
			if handle != 0 {
				unsafe { close(handle) };
			}
			return None;
		}
		let addr = match unsafe { map_object(handle) } {
			Some(addr) => addr,
			None => {
				unsafe { close(handle) };
				return None;
			}
		};
		let framebuffer = Framebuffer { width: info.width, height: info.height, pitch: info.pitch, bytes_per_pixel: 4, red_shift: 16, red_size: 8, green_shift: 8, green_size: 8, blue_shift: 0, blue_size: 8, _pad: [0; 2] };
		Some(Mapping { handle, addr, framebuffer })
	}

	pub const fn addr(&self) -> u64 {
		self.addr
	}

	pub const fn framebuffer(&self) -> Framebuffer {
		self.framebuffer
	}
}

impl Drop for Mapping {
	fn drop(&mut self) {
		unsafe {
			unmap_object(self.handle);
			close(self.handle);
		}
	}
}

pub fn acquire(client: &Client, width: u32, height: u32) -> Option<Result<Mapping, Error>> {
	match client.borrow_mut().acquire(&width, &height)? {
		Ok(info) => Some(Mapping::from_info(info).ok_or(Error::Invalid)),
		Err(error) => Some(Err(error)),
	}
}

pub fn present(client: &Client, rect: Rect) -> Option<Result<(), Error>> {
	client.borrow_mut().present(&rect.x, &rect.y, &rect.width, &rect.height)
}

pub fn release(client: &Client) -> Option<Result<(), Error>> {
	client.borrow_mut().release()
}

pub fn events(client: &Client) -> Option<u64> {
	client.borrow_mut().events()
}

pub fn read_event(message: &[u8], handle: &mut u64) -> Option<DisplayEvent> {
	display::events_read(message, handle)
}

pub fn input_focus(client: &Client) -> Option<Result<u64, Error>> {
	client.borrow_mut().input_focus()
}

pub fn subscribe_keys(channel: u64, focus: u64) -> Option<u64> {
	input::Client::new(ChannelTransport { chan: channel }).subscribe_keys(&focus)
}
