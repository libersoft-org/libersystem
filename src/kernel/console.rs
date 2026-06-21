// Framebuffer text console.
//
// Mirrors the kernel log to a linear RGB framebuffer (provided by Limine as a
// boot-time video mode) using an embedded 8x8 bitmap font, so the boot log is
// visible on screen as well as on the serial port. The console owns a cursor,
// scrolls when it reaches the bottom, and packs pixels per the framebuffer's
// colour masks. It is a mirror, not a replacement: serial output is unchanged and
// always happens; the console is best-effort - skipped entirely if no framebuffer
// was provided, and skipped for a single print if its lock is already held (e.g.
// a panic that interrupted a print), so it can never deadlock the logger.

#![allow(dead_code)]

use core::fmt::{self, Write};

use crate::sync::SpinLock;

// Public-domain 8x8 bitmap font (dhepper/font8x8, basic latin U+0000..U+007F):
// 128 glyphs, 8 bytes each, one byte per row with bit 0 = leftmost pixel.
static FONT: &[u8; 1024] = include_bytes!("font8x8.bin");

const FONT_W: usize = 8;
const FONT_H: usize = 8;
// Each font pixel is drawn as a SCALE x SCALE block so glyphs are legible on a
// high-resolution framebuffer.
const SCALE: usize = 2;
const CELL_W: usize = FONT_W * SCALE;
const CELL_H: usize = FONT_H * SCALE;

// Light-grey text on a near-black background.
const FG: (u8, u8, u8) = (0xc8, 0xc8, 0xc8);
const BG: (u8, u8, u8) = (0x0a, 0x0a, 0x12);

// A framebuffer description handed in from the Limine response.
pub struct FbInfo {
	pub addr: *mut u8,
	pub width: usize,
	pub height: usize,
	// Bytes between the start of consecutive rows (may exceed width * bytes/pixel).
	pub pitch: usize,
	pub bytes_per_pixel: usize,
	pub red_shift: u8,
	pub red_size: u8,
	pub green_shift: u8,
	pub green_size: u8,
	pub blue_shift: u8,
	pub blue_size: u8,
}

struct Console {
	addr: *mut u8,
	width: usize,
	height: usize,
	pitch: usize,
	bytes_per_pixel: usize,
	red_shift: u8,
	red_size: u8,
	green_shift: u8,
	green_size: u8,
	blue_shift: u8,
	blue_size: u8,
	// Text grid dimensions and the cursor position within it.
	cols: usize,
	rows: usize,
	col: usize,
	row: usize,
	// Foreground/background packed into the framebuffer pixel format.
	fg: u32,
	bg: u32,
}

// The framebuffer pointer is only ever dereferenced under the console lock.
unsafe impl Send for Console {}

static CONSOLE: SpinLock<Option<Console>> = SpinLock::new(None);

impl Console {
	fn new(info: FbInfo) -> Self {
		let mut console = Self { addr: info.addr, width: info.width, height: info.height, pitch: info.pitch, bytes_per_pixel: info.bytes_per_pixel, red_shift: info.red_shift, red_size: info.red_size, green_shift: info.green_shift, green_size: info.green_size, blue_shift: info.blue_shift, blue_size: info.blue_size, cols: info.width / CELL_W, rows: info.height / CELL_H, col: 0, row: 0, fg: 0, bg: 0 };
		console.fg = console.pack(FG.0, FG.1, FG.2);
		console.bg = console.pack(BG.0, BG.1, BG.2);
		console
	}

	// Position one 8-bit colour channel into the framebuffer pixel value: keep the
	// top `size` bits, then shift left by `shift`. Clamped to 8 so a degenerate
	// mask can never underflow the shift.
	fn channel(&self, value: u8, size: u8, shift: u8) -> u32 {
		let size = (size as u32).min(8);
		((value as u32) >> (8 - size)) << (shift as u32)
	}

	// Pack an (r, g, b) triple into a framebuffer pixel using the mode's masks.
	fn pack(&self, r: u8, g: u8, b: u8) -> u32 {
		self.channel(r, self.red_size, self.red_shift) | self.channel(g, self.green_size, self.green_shift) | self.channel(b, self.blue_size, self.blue_shift)
	}

	#[inline]
	fn put_pixel(&self, x: usize, y: usize, color: u32) {
		if x >= self.width || y >= self.height {
			return;
		}
		let offset = y * self.pitch + x * self.bytes_per_pixel;
		let bytes = color.to_le_bytes();
		unsafe {
			let base = self.addr.add(offset);
			for i in 0..self.bytes_per_pixel {
				core::ptr::write_volatile(base.add(i), bytes[i]);
			}
		}
	}

	fn fill(&self, y0: usize, y1: usize, color: u32) {
		for y in y0..y1 {
			for x in 0..self.width {
				self.put_pixel(x, y, color);
			}
		}
	}

	fn clear(&mut self) {
		self.fill(0, self.height, self.bg);
		self.col = 0;
		self.row = 0;
	}

	fn draw_glyph(&self, glyph: u8, cell_col: usize, cell_row: usize) {
		let base = (glyph as usize) * FONT_H;
		let x0 = cell_col * CELL_W;
		let y0 = cell_row * CELL_H;
		for gy in 0..FONT_H {
			let bits = FONT[base + gy];
			for gx in 0..FONT_W {
				let color = if bits & (1 << gx) != 0 { self.fg } else { self.bg };
				for sy in 0..SCALE {
					for sx in 0..SCALE {
						self.put_pixel(x0 + gx * SCALE + sx, y0 + gy * SCALE + sy, color);
					}
				}
			}
		}
	}

	// Move the whole image up by one text row and clear the freed bottom row.
	fn scroll(&mut self) {
		let shift = CELL_H * self.pitch;
		let total = self.height * self.pitch;
		unsafe {
			core::ptr::copy(self.addr.add(shift), self.addr, total - shift);
		}
		self.fill((self.rows - 1) * CELL_H, self.height, self.bg);
	}

	fn newline(&mut self) {
		self.col = 0;
		if self.row + 1 < self.rows {
			self.row += 1;
		} else {
			self.scroll();
		}
	}

	fn put_char(&mut self, byte: u8) {
		match byte {
			b'\n' => self.newline(),
			b'\r' => self.col = 0,
			0x08 => {
				if self.col > 0 {
					self.col -= 1;
				}
			}
			b'\t' => {
				let next = (self.col / 4 + 1) * 4;
				while self.col < next && self.col < self.cols {
					self.put_char(b' ');
				}
			}
			_ => {
				if self.col >= self.cols {
					self.newline();
				}
				// Render printable ASCII; substitute '?' for anything else (the font
				// only covers U+0000..U+007F and control codes are not glyphs).
				let glyph = if (0x20..0x7f).contains(&byte) { byte } else { b'?' };
				self.draw_glyph(glyph, self.col, self.row);
				self.col += 1;
			}
		}
	}
}

impl Write for Console {
	fn write_str(&mut self, s: &str) -> fmt::Result {
		for byte in s.bytes() {
			self.put_char(byte);
		}
		Ok(())
	}
}

// Initialize the console from a framebuffer description and clear the screen.
// A degenerate mode (no pixels) leaves the console uninitialized so logging falls
// back to serial only.
pub fn init(info: FbInfo) {
	if info.width == 0 || info.height == 0 || info.bytes_per_pixel == 0 {
		return;
	}
	let mut console = Console::new(info);
	if console.cols == 0 || console.rows == 0 {
		return;
	}
	console.clear();
	*CONSOLE.lock() = Some(console);
}

// Mirror formatted output to the framebuffer console, if one is initialized.
// Best-effort: try_lock means a print that interrupts a held lock (a panic
// mid-print) skips the console rather than deadlocking - serial still shows it.
pub fn write_fmt(args: fmt::Arguments<'_>) {
	if let Some(mut guard) = CONSOLE.try_lock() {
		if let Some(console) = guard.as_mut() {
			let _ = console.write_fmt(args);
		}
	}
}
