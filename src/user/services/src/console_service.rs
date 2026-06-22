// ConsoleService - the userspace terminal: it owns the framebuffer and renders text.
//
// The kernel prints the boot log to the framebuffer and then hands the display over:
// `framebuffer_map` maps the pixel buffer into this service and stops the kernel
// console drawing. From then on ConsoleService owns rendering. It serves one client
// (the shell) over a bidirectional console channel: the client writes output bytes
// (which ConsoleService renders to the framebuffer, interpreting the ANSI escapes,
// and mirrors to the serial port) and reads input bytes (the keystrokes the kernel's
// console input delivers, which ConsoleService forwards to the client). So the
// terminal logic - the cell grid, the escape parser, the colours, the cursor - lives
// in userspace; the kernel keeps only the boot log and the serial mirror path.
//
// This is the M35c extraction of the M15/M35 kernel framebuffer console.

#![no_std]
#![no_main]

use rt::*;

// The 8x8 bitmap font, shared with the (now boot-log-only) kernel console.
static FONT: &[u8; 1024] = include_bytes!("../../../kernel/font8x8.bin");

const FONT_W: usize = 8;
const FONT_H: usize = 8;
const SCALE: usize = 2;
const CELL_W: usize = FONT_W * SCALE;
const CELL_H: usize = FONT_H * SCALE;

// Light-grey on near-black, matching the kernel console's boot-log colours.
const FG: (u8, u8, u8) = (0xc8, 0xc8, 0xc8);
const BG: (u8, u8, u8) = (0x0a, 0x0a, 0x12);

// The standard 16-colour ANSI palette (classic xterm/VGA RGB): 0-7 normal, 8-15 bright.
#[rustfmt::skip]
const ANSI_PALETTE: [(u8, u8, u8); 16] = [
	(0x00, 0x00, 0x00), (0xaa, 0x00, 0x00), (0x00, 0xaa, 0x00), (0xaa, 0x55, 0x00),
	(0x00, 0x00, 0xaa), (0xaa, 0x00, 0xaa), (0x00, 0xaa, 0xaa), (0xaa, 0xaa, 0xaa),
	(0x55, 0x55, 0x55), (0xff, 0x55, 0x55), (0x55, 0xff, 0x55), (0xff, 0xff, 0x55),
	(0x55, 0x55, 0xff), (0xff, 0x55, 0xff), (0x55, 0xff, 0xff), (0xff, 0xff, 0xff),
];

// The terminal: the mapped framebuffer + geometry, the text cursor, the current SGR
// colours, and the output escape-parser state.
struct Term {
	addr: u64,
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
	cols: usize,
	rows: usize,
	col: usize,
	row: usize,
	fg: u32,
	bg: u32,
	palette: [u32; 16],
	cur_fg: u32,
	cur_bg: u32,
	fg_idx: Option<u8>,
	bg_idx: Option<u8>,
	bold: bool,
	esc_state: u8,
	params: [u16; 8],
	nparams: usize,
	caret_shown: bool,
}

impl Term {
	fn new(addr: u64, fb: &Framebuffer) -> Term {
		let mut t = Term { addr, width: fb.width as usize, height: fb.height as usize, pitch: fb.pitch as usize, bytes_per_pixel: fb.bytes_per_pixel as usize, red_shift: fb.red_shift, red_size: fb.red_size, green_shift: fb.green_shift, green_size: fb.green_size, blue_shift: fb.blue_shift, blue_size: fb.blue_size, cols: fb.width as usize / CELL_W, rows: fb.height as usize / CELL_H, col: 0, row: 0, fg: 0, bg: 0, palette: [0; 16], cur_fg: 0, cur_bg: 0, fg_idx: None, bg_idx: None, bold: false, esc_state: 0, params: [0; 8], nparams: 0, caret_shown: false };
		t.fg = t.pack(FG.0, FG.1, FG.2);
		t.bg = t.pack(BG.0, BG.1, BG.2);
		for (i, &(r, g, b)) in ANSI_PALETTE.iter().enumerate() {
			t.palette[i] = t.pack(r, g, b);
		}
		t.cur_fg = t.fg;
		t.cur_bg = t.bg;
		t
	}

	// Position one 8-bit colour channel into the framebuffer pixel value.
	fn channel(&self, value: u8, size: u8, shift: u8) -> u32 {
		let size = (size as u32).min(8);
		((value as u32) >> (8 - size)) << (shift as u32)
	}

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
			let base = (self.addr as *mut u8).add(offset);
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

	fn scroll(&mut self) {
		let shift = CELL_H * self.pitch;
		let total = self.height * self.pitch;
		unsafe {
			core::ptr::copy((self.addr as *const u8).add(shift), self.addr as *mut u8, total - shift);
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

	// Render one output byte: hide the caret, process it (escapes, control chars, or a
	// glyph), then redraw the caret at the new cursor.
	fn put(&mut self, byte: u8) {
		if self.caret_shown {
			self.invert_caret();
			self.caret_shown = false;
		}
		self.put_raw(byte);
		self.invert_caret();
		self.caret_shown = true;
	}

	fn put_raw(&mut self, byte: u8) {
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
					self.put_raw(b' ');
				}
			}
			_ => {
				if self.col >= self.cols {
					self.newline();
				}
				let glyph = if (0x20..0x7f).contains(&byte) { byte } else { b'?' };
				self.draw_glyph(glyph, self.col, self.row);
				self.col += 1;
			}
		}
	}

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

	// Toggle an underline caret at the cursor cell (XOR of its bottom rows).
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
			let base = (self.addr as *mut u8).add(offset);
			for i in 0..self.bytes_per_pixel {
				let v = core::ptr::read_volatile(base.add(i));
				core::ptr::write_volatile(base.add(i), !v);
			}
		}
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. receive the client (shell) end of the console channel from the supervisor.
		let client: u64 = recv_tagged(bootstrap, &mut buf, b"CLIENT").unwrap_or_else(|| exit());

		// 2. map the framebuffer (this hands us the display; the kernel console stops).
		//    A headless boot has no framebuffer; we still serve so input/serial work.
		let mut fb: Framebuffer = Framebuffer::default();
		let addr: i64 = framebuffer_map(&mut fb);
		let mut term: Option<Term> = if sys_is_err(addr as u64) {
			None
		} else {
			let mut t = Term::new(addr as u64, &fb);
			t.clear();
			for &b in b"ConsoleService: online\n" {
				t.put(b);
			}
			Some(t)
		};

		// 3. report in to the supervisor.
		send_blocking(bootstrap, b"ConsoleService: online", 0);

		// 4. run the terminal loop.
		run(&mut term, client);
	}
}

// The terminal loop: attach to the kernel's console input, then multiplex the
// keystroke channel and the client's output channel. Keystrokes are forwarded to the
// client; output bytes are rendered to the framebuffer (if any) and mirrored to the
// serial port. A blink timeout toggles the caret while idle.
unsafe fn run(term: &mut Option<Term>, client: u64) -> ! {
	unsafe {
		// attach a channel the kernel feeds keystrokes on.
		let (feed, input): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => exit(),
		};
		if sys_is_err(syscall(SYS_CONSOLE_ATTACH, feed, 0, 0, 0)) {
			exit();
		}
		let waits: [u64; 2] = [input, client];
		let mut keys: [u8; 64] = [0u8; 64];
		let mut out: [u8; 1024] = [0u8; 1024];
		loop {
			// Block (no deadline) until a keystroke or output arrives. A self-driven
			// blink timer is avoided here: a thread that re-blocks on a deadline keeps
			// the cooperative `run_until_idle` (the boot driver) from ever settling, so
			// the caret stays solid for now (blinking is a later post-boot refinement).
			let ready: i64 = wait_any(&waits, 0);
			if ready < 0 {
				continue;
			}
			match ready as usize {
				0 => {
					// keystrokes from the kernel console input -> forward to the client.
					match recv_blocking(input, &mut keys) {
						Received::Message { len, .. } => {
							send_blocking(client, &keys[..len], 0);
						}
						Received::Closed => {}
					}
				}
				1 => {
					// output bytes from the client -> render + mirror to the serial port.
					match recv_blocking(client, &mut out) {
						Received::Message { len, .. } => {
							if let Some(t) = term.as_mut() {
								for &b in &out[..len] {
									t.put(b);
								}
							}
							print(&out[..len]);
						}
						Received::Closed => exit(),
					}
				}
				_ => {}
			}
		}
	}
}
