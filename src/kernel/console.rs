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

use core::sync::atomic::{AtomicBool, Ordering};

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

// The standard 16-colour ANSI palette (the classic xterm/VGA RGB values): 0-7 the
// normal colours, 8-15 their bright variants. SGR `30-37`/`40-47` select 0-7,
// `90-97`/`100-107` and bold select 8-15.
#[rustfmt::skip]
const ANSI_PALETTE: [(u8, u8, u8); 16] = [
	(0x00, 0x00, 0x00), (0xaa, 0x00, 0x00), (0x00, 0xaa, 0x00), (0xaa, 0x55, 0x00),
	(0x00, 0x00, 0xaa), (0xaa, 0x00, 0xaa), (0x00, 0xaa, 0xaa), (0xaa, 0xaa, 0xaa),
	(0x55, 0x55, 0x55), (0xff, 0x55, 0x55), (0x55, 0xff, 0x55), (0xff, 0xff, 0x55),
	(0x55, 0x55, 0xff), (0xff, 0x55, 0xff), (0x55, 0xff, 0xff), (0xff, 0xff, 0xff),
];

// The caret blinks every this many monotonic ticks (100 Hz -> ~0.5 s) while idle.
const BLINK_TICKS: u64 = 50;

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
	// The 16-colour ANSI palette (packed) and the current SGR colours / attributes
	// that `draw_glyph` uses; an index of None means the default fg/bg.
	palette: [u32; 16],
	cur_fg: u32,
	cur_bg: u32,
	fg_idx: Option<u8>,
	bg_idx: Option<u8>,
	bold: bool,
	// Output escape-sequence parser state (0 = normal, 1 = after ESC, 2 = in CSI) and
	// the accumulated CSI numeric parameters.
	esc_state: u8,
	params: [u16; 8],
	nparams: usize,
	// Whether an underline caret is currently drawn at (col, row). The caret is hidden
	// before each character is processed and redrawn after, so it follows the cursor -
	// making cursor moves (the shell's line editor) visible on the framebuffer.
	caret_shown: bool,
	// The monotonic tick the caret was last toggled / output happened, for blinking.
	last_blink: u64,
}

// The framebuffer pointer is only ever dereferenced under the console lock.
unsafe impl Send for Console {}

static CONSOLE: SpinLock<Option<Console>> = SpinLock::new(None);

// Set once a userspace ConsoleService maps the framebuffer and takes over the
// display: the kernel console then stops drawing (boot-log output still reaches the
// serial port, but the framebuffer belongs to ConsoleService).
static DISABLED: AtomicBool = AtomicBool::new(false);

impl Console {
	fn new(info: FbInfo) -> Self {
		let mut console = Self { addr: info.addr, width: info.width, height: info.height, pitch: info.pitch, bytes_per_pixel: info.bytes_per_pixel, red_shift: info.red_shift, red_size: info.red_size, green_shift: info.green_shift, green_size: info.green_size, blue_shift: info.blue_shift, blue_size: info.blue_size, cols: info.width / CELL_W, rows: info.height / CELL_H, col: 0, row: 0, fg: 0, bg: 0, palette: [0; 16], cur_fg: 0, cur_bg: 0, fg_idx: None, bg_idx: None, bold: false, esc_state: 0, params: [0; 8], nparams: 0, caret_shown: false, last_blink: 0 };
		console.fg = console.pack(FG.0, FG.1, FG.2);
		console.bg = console.pack(BG.0, BG.1, BG.2);
		for (i, &(r, g, b)) in ANSI_PALETTE.iter().enumerate() {
			console.palette[i] = console.pack(r, g, b);
		}
		console.cur_fg = console.fg;
		console.cur_bg = console.bg;
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
				let color = if bits & (1 << gx) != 0 { self.cur_fg } else { self.cur_bg };
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
		// Hide the caret before drawing (so it leaves no artifact and scroll copies a
		// clean image), process the byte, then redraw the caret at the new cursor.
		if self.caret_shown {
			self.invert_caret();
			self.caret_shown = false;
		}
		self.put_char_raw(byte);
		self.invert_caret();
		self.caret_shown = true;
		// Keep the caret solid while output / typing is happening; it blinks only after
		// it has been idle for BLINK_TICKS (the blink timer counts from here).
		self.last_blink = crate::arch::apic::ticks();
	}

	fn put_char_raw(&mut self, byte: u8) {
		// Output escape-sequence parser: ESC [ ... <final>. We interpret SGR (colour) and
		// consume any other CSI sequence, so a control sequence never renders as glyphs.
		match self.esc_state {
			1 => {
				if byte == b'[' {
					self.esc_state = 2;
					self.params = [0; 8];
					self.nparams = 0;
				} else {
					self.esc_state = 0;
				}
				return;
			}
			2 => {
				self.csi_byte(byte);
				return;
			}
			_ => {}
		}
		match byte {
			0x1b => self.esc_state = 1,
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
					self.put_char_raw(b' ');
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

	// Consume one byte of a CSI sequence (after `ESC [`): accumulate the numeric
	// parameters, apply SGR on `m`, and end (ignore) any other final byte.
	fn csi_byte(&mut self, byte: u8) {
		match byte {
			b'0'..=b'9' => {
				let p = &mut self.params[self.nparams];
				*p = p.saturating_mul(10).saturating_add((byte - b'0') as u16);
			}
			b';' => {
				if self.nparams + 1 < self.params.len() {
					self.nparams += 1;
				}
			}
			b'm' => {
				self.apply_sgr();
				self.esc_state = 0;
			}
			0x40..=0x7e => self.esc_state = 0,
			_ => {}
		}
	}

	// Apply the accumulated SGR parameters to the current colours / bold attribute.
	fn apply_sgr(&mut self) {
		for i in 0..=self.nparams {
			match self.params[i] {
				0 => {
					self.fg_idx = None;
					self.bg_idx = None;
					self.bold = false;
				}
				1 => self.bold = true,
				22 => self.bold = false,
				30..=37 => self.fg_idx = Some((self.params[i] - 30) as u8),
				39 => self.fg_idx = None,
				40..=47 => self.bg_idx = Some((self.params[i] - 40) as u8),
				49 => self.bg_idx = None,
				90..=97 => self.fg_idx = Some((self.params[i] - 90 + 8) as u8),
				100..=107 => self.bg_idx = Some((self.params[i] - 100 + 8) as u8),
				_ => {}
			}
		}
		self.recompute_colors();
	}

	// Recompute the packed current colours from the indices + bold (bold brightens a
	// normal foreground); a None index means the default fg/bg.
	fn recompute_colors(&mut self) {
		self.cur_fg = match self.fg_idx {
			Some(i) => self.palette[if self.bold && i < 8 { (i + 8) as usize } else { i as usize }],
			None => self.fg,
		};
		self.cur_bg = match self.bg_idx {
			Some(i) => self.palette[i as usize],
			None => self.bg,
		};
	}

	// Toggle an underline caret (the bottom SCALE pixel rows of the cursor cell) by
	// inverting those pixels - reversible without knowing the glyph beneath it.
	fn invert_caret(&self) {
		if self.col >= self.cols || self.row >= self.rows {
			return;
		}
		let x0 = self.col * CELL_W;
		let y0 = self.row * CELL_H;
		for y in (y0 + CELL_H - SCALE)..(y0 + CELL_H) {
			for x in x0..(x0 + CELL_W) {
				self.invert_pixel(x, y);
			}
		}
	}

	#[inline]
	fn invert_pixel(&self, x: usize, y: usize) {
		if x >= self.width || y >= self.height {
			return;
		}
		let offset = y * self.pitch + x * self.bytes_per_pixel;
		unsafe {
			let base = self.addr.add(offset);
			for i in 0..self.bytes_per_pixel {
				let v = core::ptr::read_volatile(base.add(i));
				core::ptr::write_volatile(base.add(i), !v);
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
	if DISABLED.load(Ordering::Relaxed) {
		return;
	}
	if let Some(mut guard) = CONSOLE.try_lock() {
		if let Some(console) = guard.as_mut() {
			let _ = console.write_fmt(args);
		}
	}
}

// Mirror raw bytes to the framebuffer console, if one is initialized. The bulk twin
// of write_fmt for the bulk SYS_DEBUG_WRITE path, so a buffer renders without a
// per-char format_args. A no-op once the display is handed to a userspace console.
pub fn write_bytes(bytes: &[u8]) {
	if DISABLED.load(Ordering::Relaxed) {
		return;
	}
	if let Some(mut guard) = CONSOLE.try_lock() {
		if let Some(console) = guard.as_mut() {
			for &byte in bytes {
				console.put_char(byte);
			}
		}
	}
}

// Drive the blinking caret: toggle it if at least BLINK_TICKS have passed since the
// last toggle or output. The interactive loop calls this every round with the
// monotonic tick; output resets the timer so the caret stays solid while typing.
pub fn blink_tick(now: u64) {
	if DISABLED.load(Ordering::Relaxed) {
		return;
	}
	if let Some(mut guard) = CONSOLE.try_lock() {
		if let Some(console) = guard.as_mut() {
			if now.wrapping_sub(console.last_blink) >= BLINK_TICKS {
				console.last_blink = now;
				console.invert_caret();
				console.caret_shown = !console.caret_shown;
			}
		}
	}
}

// Hand the framebuffer to a userspace ConsoleService: the kernel console stops
// drawing (its boot-log job is done; serial output continues). Called by the
// framebuffer_map syscall when ConsoleService maps the display.
pub fn disable() {
	DISABLED.store(true, Ordering::Relaxed);
}

// Whether the framebuffer has been handed to userspace (so a second framebuffer_map
// is refused - the first mapper owns the display).
pub fn is_disabled() -> bool {
	DISABLED.load(Ordering::Relaxed)
}
