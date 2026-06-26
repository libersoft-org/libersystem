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

use proto::system::network;

extern crate alloc;
use alloc::boxed::Box;
use alloc::vec::Vec;

// The graphics-free terminal model (L2): the cell grid, the escape parser, the logical
// colours, the cursor, and the scrollback. This service is its renderer (L3) - it reads the
// model's snapshot/diff interface and draws it onto the framebuffer surface.
use term::{Cell, Color, CursorShape, Screen, ScrollOp};

// The 8x8 bitmap font: 256 glyphs indexed by Unicode codepoint 0x00-0xFF - the kernel
// boot-log font (basic latin U+0000-007F) extended with the Latin-1 supplement
// (U+00A0-00FF, U+0080-009F are the blank C1 controls), so non-ASCII Western text
// renders. Public domain (dhepper/font8x8); the binary is built from its headers.
static FONT: &[u8; 2048] = include_bytes!("font8x8_latin.bin");

const FONT_W: usize = 8;
const FONT_H: usize = 8;
const SCALE: usize = 2;
const CELL_W: usize = FONT_W * SCALE;
const CELL_H: usize = FONT_H * SCALE;

// The raw pixel target: the mapped framebuffer, its geometry, and its pixel format. The
// only place that touches pixels and the framebuffer address - the renderer (`Term`) draws
// cells through it, and a display-backend swap (boot-fb <-> virtio-gpu) replaces just this.
// It holds no grid and no terminal state.
struct Surface {
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
}

impl Surface {
	fn new(addr: u64, fb: &Framebuffer) -> Surface {
		Surface { addr, width: fb.width as usize, height: fb.height as usize, pitch: fb.pitch as usize, bytes_per_pixel: fb.bytes_per_pixel as usize, red_shift: fb.red_shift, red_size: fb.red_size, green_shift: fb.green_shift, green_size: fb.green_size, blue_shift: fb.blue_shift, blue_size: fb.blue_size }
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

	// Fill the whole framebuffer with one colour.
	fn fill(&self, color: u32) {
		for y in 0..self.height {
			for x in 0..self.width {
				self.put_pixel(x, y, color);
			}
		}
	}

	// Shift the framebuffer pixels for grid rows [top, bot] up by n cell-heights. A scroll
	// then moves the existing pixels in one bulk copy instead of re-blitting every glyph
	// (the full-frame glyph re-render is the dominant cost of scrolling). The vacated bottom
	// rows are repainted from the (blanked) grid by the renderer's dirty walk.
	fn scroll_pixels_up(&self, top: usize, bot: usize, n: usize) {
		let dy = n * CELL_H;
		let y_first = top * CELL_H;
		let y_end = ((bot + 1) * CELL_H).min(self.height);
		if dy >= y_end.saturating_sub(y_first) {
			return;
		}
		let row_bytes = (self.width * self.bytes_per_pixel).min(self.pitch);
		unsafe {
			let base = self.addr as *mut u8;
			let mut y = y_first;
			while y + dy < y_end {
				let dst = base.add(y * self.pitch);
				let src = base.add((y + dy) * self.pitch);
				core::ptr::copy_nonoverlapping(src, dst, row_bytes);
				y += 1;
			}
		}
	}

	// Shift the framebuffer pixels for grid rows [top, bot] down by n cell-heights - the
	// downward counterpart of scroll_pixels_up (reverse index / insert line).
	fn scroll_pixels_down(&self, top: usize, bot: usize, n: usize) {
		let dy = n * CELL_H;
		let y_first = top * CELL_H;
		let y_end = ((bot + 1) * CELL_H).min(self.height);
		if dy >= y_end.saturating_sub(y_first) {
			return;
		}
		let row_bytes = (self.width * self.bytes_per_pixel).min(self.pitch);
		unsafe {
			let base = self.addr as *mut u8;
			let mut y = y_end;
			while y > y_first + dy {
				y -= 1;
				let dst = base.add(y * self.pitch);
				let src = base.add((y - dy) * self.pitch);
				core::ptr::copy_nonoverlapping(src, dst, row_bytes);
			}
		}
	}
}

// The renderer (L3): the pixel surface it draws onto plus its own `last_caret` (where the
// caret was last painted). It is a pure consumer of the grid model - it reads the model's
// changed cells through `Screen`'s snapshot/diff interface (`cell`, `view_cell`,
// `dirty_take`, `take_scrolls`), resolves their logical colours to packed pixels, blits
// them, and replays the model's recorded scrolls as bulk pixel copies. It never mutates the
// grid and holds no terminal state.
struct FramebufferRenderer {
	surface: Surface,
	last_caret: Option<(usize, usize)>,
}

// The terminal (L2 + L3): the grid model and the renderer that draws it. A thin façade that
// owns both and wires the model's output to the renderer's framebuffer.
struct Term {
	screen: Screen,
	renderer: FramebufferRenderer,
}

impl Term {
	fn new(addr: u64, fb: &Framebuffer) -> Term {
		let cols = fb.width as usize / CELL_W;
		let rows = fb.height as usize / CELL_H;
		Term { screen: Screen::new(cols, rows), renderer: FramebufferRenderer::new(addr, fb) }
	}

	// Reflow the model to fit new_cols x new_rows, clamped to what the physical framebuffer
	// can show, then clear the now-unused area (only when the grid actually changed). The
	// local stand-in for a virtio-gpu mode-set (M44).
	fn resize(&mut self, new_cols: usize, new_rows: usize) {
		let max_cols = (self.renderer.surface.width / CELL_W).max(1);
		let max_rows = (self.renderer.surface.height / CELL_H).max(1);
		if self.screen.resize(new_cols, new_rows, max_cols, max_rows) {
			self.renderer.last_caret = None;
			self.renderer.clear_screen(&self.screen);
		}
	}

	// Paint the model's pending output to the framebuffer (the model's scroll + dirty diff;
	// see FramebufferRenderer::flush).
	fn flush(&mut self) {
		self.renderer.flush(&mut self.screen);
	}

	// Flash the screen with inverted colours (the visual bell) without touching the grid.
	fn draw_inverted(&self) {
		self.renderer.draw_inverted(&self.screen);
	}
}

// Follow a caret cell through this frame's grid scrolls so the renderer can erase the
// pixels the bulk copy carried with it. Returns where the caret's pixels ended up, or None
// if the scroll pushed them out of their band (the copy overwrote them, nothing to erase).
fn track_caret(caret: Option<(usize, usize)>, scrolls: &[ScrollOp]) -> Option<(usize, usize)> {
	let (c, mut r) = caret?;
	for op in scrolls {
		if r >= op.top && r <= op.bot {
			if op.down {
				if r + op.n <= op.bot {
					r += op.n;
				} else {
					return None;
				}
			} else if r >= op.top + op.n {
				r -= op.n;
			} else {
				return None;
			}
		}
	}
	Some((c, r))
}

impl FramebufferRenderer {
	fn new(addr: u64, fb: &Framebuffer) -> FramebufferRenderer {
		FramebufferRenderer { surface: Surface::new(addr, fb), last_caret: None }
	}

	// Fill the whole physical framebuffer with the model's default background - used when
	// the grid shrinks so the area now outside it is not left with stale pixels.
	fn clear_screen(&self, screen: &Screen) {
		let bg = screen.default_bg();
		self.surface.fill(self.surface.pack(bg.0, bg.1, bg.2));
	}

	// Paint one cell from the grid to the framebuffer.
	fn draw_cell(&self, screen: &Screen, col: usize, row: usize) {
		self.draw_cell_at(screen, col, row, screen.cell(col, row));
	}

	// Paint a given cell value at (col, row) - used by the live flush (reading the grid)
	// and the scrollback view flush (reading the scrollback ring).
	fn draw_cell_at(&self, screen: &Screen, col: usize, row: usize, cell: Cell) {
		let (fg, bg) = self.cell_colors(screen, &cell);
		self.blit_cell(col, row, cell.glyph, fg, bg, cell.underline);
	}

	// Blit one glyph cell to the framebuffer in already-resolved colours.
	fn blit_cell(&self, col: usize, row: usize, glyph: u8, fg: u32, bg: u32, underline: bool) {
		let base = (glyph as usize) * FONT_H;
		let x0 = col * CELL_W;
		let y0 = row * CELL_H;
		for gy in 0..FONT_H {
			let bits = FONT[base + gy];
			for gx in 0..FONT_W {
				let color = if bits & (1 << gx) != 0 { fg } else { bg };
				for sy in 0..SCALE {
					for sx in 0..SCALE {
						self.surface.put_pixel(x0 + gx * SCALE + sx, y0 + gy * SCALE + sy, color);
					}
				}
			}
		}
		if underline {
			for y in (y0 + CELL_H - SCALE)..(y0 + CELL_H) {
				for x in x0..(x0 + CELL_W) {
					self.surface.put_pixel(x, y, fg);
				}
			}
		}
	}

	// Draw the caret in the current DECSCUSR shape over its cell (the glyph was already
	// repainted by the dirty flush): a block inverts the cell, an underline paints the
	// bottom rows, a bar the left columns.
	fn draw_caret(&self, screen: &Screen, col: usize, row: usize) {
		let x0 = col * CELL_W;
		let y0 = row * CELL_H;
		match screen.cursor_shape() {
			CursorShape::Block => {
				let cell = screen.cell(col, row);
				let (fg, bg) = self.cell_colors(screen, &cell);
				self.blit_cell(col, row, cell.glyph, bg, fg, cell.underline);
			}
			CursorShape::Underline => {
				let fg = self.cell_colors(screen, &screen.blank()).0;
				for y in (y0 + CELL_H - SCALE)..(y0 + CELL_H) {
					for x in x0..(x0 + CELL_W) {
						self.surface.put_pixel(x, y, fg);
					}
				}
			}
			CursorShape::Bar => {
				let fg = self.cell_colors(screen, &screen.blank()).0;
				for y in y0..(y0 + CELL_H) {
					for x in x0..(x0 + SCALE) {
						self.surface.put_pixel(x, y, fg);
					}
				}
			}
		}
	}

	// Push the changed cells to the framebuffer, then draw the caret. Called once per output
	// batch: many bytes edit the grid, one flush paints it (double buffering). The model's
	// recorded scrolls are replayed as bulk framebuffer pixel copies first, so a scroll moves
	// the existing pixels in one go instead of re-blitting every glyph; the dirty walk then
	// repaints only the vacated rows (and any cells edited this batch).
	fn flush(&mut self, screen: &mut Screen) {
		let scrolls = screen.take_scrolls();
		if screen.view_offset() > 0 {
			self.flush_view(screen);
			return;
		}
		// Move the framebuffer pixels for each grid scroll, following the old caret cell
		// through the same shifts so its smear lands on a cell the dirty walk repaints.
		let ghost = track_caret(self.last_caret, &scrolls);
		for op in &scrolls {
			if op.down {
				self.surface.scroll_pixels_down(op.top, op.bot, op.n);
			} else {
				self.surface.scroll_pixels_up(op.top, op.bot, op.n);
			}
		}
		if let Some((c, r)) = ghost {
			screen.set_dirty(c, r);
		}
		for row in 0..screen.rows() {
			for col in 0..screen.cols() {
				if screen.dirty_take(col, row) {
					self.draw_cell(screen, col, row);
				}
			}
		}
		if screen.cursor_visible() && screen.cursor_col() < screen.cols() && screen.cursor_row() < screen.rows() {
			self.draw_caret(screen, screen.cursor_col(), screen.cursor_row());
			self.last_caret = Some((screen.cursor_col(), screen.cursor_row()));
		} else {
			self.last_caret = None;
		}
	}

	// Repaint the whole screen from the scrollback view (no caret while scrolled back).
	fn flush_view(&mut self, screen: &Screen) {
		for row in 0..screen.rows() {
			for col in 0..screen.cols() {
				let cell = screen.view_cell(col, row);
				self.draw_cell_at(screen, col, row, cell);
			}
		}
		self.last_caret = None;
	}

	// Resolve a cell's logical colours to packed (fg, bg) framebuffer pixels: bold brightens
	// the ANSI base (0-7 -> 8-15), then reverse swaps fg and bg. This is the L2->L3 colour
	// fold done at draw time (it used to be baked into the cell by `recompute_colors`).
	fn cell_colors(&self, screen: &Screen, c: &Cell) -> (u32, u32) {
		let fg = self.resolve(screen, c.fg, screen.default_fg(), c.bold);
		let bg = self.resolve(screen, c.bg, screen.default_bg(), c.bold);
		if c.reverse {
			(bg, fg)
		} else {
			(fg, bg)
		}
	}

	// Resolve an SGR colour to a packed framebuffer pixel, using `default` for the
	// terminal default colour; `bold` brightens the ANSI base palette.
	fn resolve(&self, screen: &Screen, c: Color, default: (u8, u8, u8), bold: bool) -> u32 {
		match c {
			Color::Default => self.surface.pack(default.0, default.1, default.2),
			Color::Idx(i) => self.indexed(screen, i, bold),
			Color::Rgb(r, g, b) => self.surface.pack(r, g, b),
		}
	}

	// The xterm 256-colour palette: 0-15 the ANSI base (bold brightens 0-7), 16-231 a
	// 6x6x6 RGB cube, and 232-255 a 24-step grayscale ramp.
	fn indexed(&self, screen: &Screen, i: u8, bold: bool) -> u32 {
		match i {
			0..=15 => {
				let idx = if bold && i < 8 { i + 8 } else { i };
				let (r, g, b) = screen.palette_color(idx as usize);
				self.surface.pack(r, g, b)
			}
			16..=231 => {
				let n = i - 16;
				let step = |c: u8| -> u8 {
					if c == 0 {
						0
					} else {
						55 + c * 40
					}
				};
				self.surface.pack(step(n / 36), step((n / 6) % 6), step(n % 6))
			}
			_ => {
				let v = 8 + (i - 232) * 10;
				self.surface.pack(v, v, v)
			}
		}
	}

	// Paint the whole screen with every cell's colours swapped - the visual bell flash -
	// without touching the grid, so a following mark_all_dirty + flush restores it.
	fn draw_inverted(&self, screen: &Screen) {
		for row in 0..screen.rows() {
			for col in 0..screen.cols() {
				let c = screen.cell(col, row);
				let (fg, bg) = self.cell_colors(screen, &c);
				self.blit_cell(col, row, c.glyph, bg, fg, c.underline);
			}
		}
	}
}

// The number of virtual terminals the console multiplexes. Each VT is an independent
// shell over its own per-VT service connections; the foreground VT owns the display.
const NVT: usize = 4;

// The number of program-hosted PTYs open at once (the `script` tool, a future `ssh`). A
// PTY occupies three wait-set slots (its slave data + control channels and the master
// channel), so the whole wait set - keyboard + gpu + NVT display VTs + PTY_MAX PTYs - is
// `2 + 2*NVT + 3*PTY_MAX` = 16 <= the kernel's MAX_WAIT_ANY.
const PTY_MAX: usize = 2;

// Control-byte chords intercepted by the console (never forwarded to a shell): the
// virtio-input driver maps Ctrl+N to 0x0e (open a new VT) and Ctrl+] to 0x1d (cycle the
// foreground). F-keys are not mapped by the driver and Alt+key collides with escape
// sequences, so single control bytes are the unambiguous, unobtrusive switch keys.
const CHORD_NEW: u8 = 0x0e;
const CHORD_NEXT: u8 = 0x1d;

// Shift+PageUp / Shift+PageDown: the virtio-input driver collapses each to a single
// private byte (0x1e / 0x1f, which its layout never produces otherwise), so the console
// pages the foreground VT's scrollback view without a multi-byte input parser.
const CHORD_SCROLL_UP: u8 = 0x1e;
const CHORD_SCROLL_DOWN: u8 = 0x1f;

// The visual bell holds the inverted screen for this many monotonic ticks (100 Hz, so
// ~100 ms) before restoring it.
const BELL_FLASH_TICKS: u64 = 10;

// The tty line discipline limits (per VT).
const LD_LINE_MAX: usize = 128;
const LD_HIST_MAX: usize = 32;

// A small fixed buffer the line discipline accumulates echo bytes in, mirrored to the
// serial port after a keystroke is processed (the framebuffer is echoed live).
struct EchoBuf {
	buf: [u8; 512],
	len: usize,
}

impl EchoBuf {
	fn new() -> EchoBuf {
		EchoBuf { buf: [0u8; 512], len: 0 }
	}
	fn push(&mut self, bytes: &[u8]) {
		for &b in bytes {
			if self.len < self.buf.len() {
				self.buf[self.len] = b;
				self.len += 1;
			}
		}
	}
	fn as_slice(&self) -> &[u8] {
		&self.buf[..self.len]
	}
}

// The echo sink: line-edit feedback renders live to the VT's cell grid (if any) and is
// collected for the serial mirror.
struct Echo<'a> {
	term: Option<&'a mut Term>,
	ser: EchoBuf,
}

impl Echo<'_> {
	fn put(&mut self, bytes: &[u8]) {
		if let Some(t) = &mut self.term {
			for &b in bytes {
				t.screen.put_byte(b);
			}
		}
		self.ser.push(bytes);
	}
}

// The tty line discipline for one VT: in cooked mode it line-edits + echoes keystrokes
// (a movable cursor, mid-line insert/delete, command history, the editing control keys)
// on the program's behalf and delivers a complete line on Enter; in raw mode keystrokes
// pass straight through. This is the M35 line editor moved out of the shell into the
// terminal, so every program reading this console gets the editor for free.
struct Ld {
	line: [u8; LD_LINE_MAX],
	len: usize,
	cursor: usize,
	history: Vec<Vec<u8>>,
	hist_pos: usize,
	esc: u8,
	csi_param: u8,
	// false = raw mode (keystrokes pass through), true = cooked (line-edited). The
	// program toggles it with ESC[?9001h/l in its output stream.
	cooked: bool,
	// whether keystrokes are echoed (ESC[?9002h/l).
	echo: bool,
	// set when Ctrl+D ends input on an empty line: feed_key delivers a zero-byte read
	// (EOF) to the program instead of a line.
	eof: bool,
}

impl Ld {
	fn new() -> Ld {
		Ld { line: [0u8; LD_LINE_MAX], len: 0, cursor: 0, history: Vec::new(), hist_pos: 0, esc: 0, csi_param: 0, cooked: true, echo: true, eof: false }
	}

	// Feed one cooked-mode keystroke. Returns true when the line was submitted (Enter, the
	// Ctrl+C cancel, or Ctrl+D); on a Ctrl+D EOF `self.eof` is set and the line is empty.
	fn feed(&mut self, b: u8, e: &mut Echo) -> bool {
		match self.esc {
			1 => {
				self.esc = if b == b'[' { 2 } else { 0 };
				return false;
			}
			2 => {
				self.csi(b, e);
				return false;
			}
			_ => {}
		}
		match b {
			0x1b => self.esc = 1,
			b'\n' | b'\r' => {
				if self.echo {
					e.put(b"\n");
				}
				return true;
			}
			0x08 | 0x7f => self.backspace(e),
			0x01 => self.home(e),      // Ctrl+A
			0x05 => self.end(e),       // Ctrl+E
			0x15 => self.kill_line(e), // Ctrl+U
			0x17 => self.kill_word(e), // Ctrl+W
			0x04 => {
				// Ctrl+D: EOF on an empty line (feed_key delivers a zero-byte read so the
				// shell logs out), otherwise submit the buffered line like Enter.
				if self.len == 0 {
					self.eof = true;
				} else if self.echo {
					e.put(b"\n");
				}
				return true;
			}
			0x03 => {
				// Ctrl+C at the prompt: cancel the line and reprompt (deliver an empty
				// line). A foreground job is interrupted in raw mode, not here.
				if self.echo {
					e.put(b"^C\n");
				}
				self.len = 0;
				self.cursor = 0;
				return true;
			}
			0x20..=0x7e => self.insert(b, e),
			_ => {}
		}
		false
	}

	fn csi(&mut self, b: u8, e: &mut Echo) {
		match b {
			b'A' => self.history_prev(e),
			b'B' => self.history_next(e),
			b'C' => self.right(e),
			b'D' => self.left(e),
			b'H' => self.home(e),
			b'F' => self.end(e),
			b'0'..=b'9' => {
				self.csi_param = self.csi_param.wrapping_mul(10).wrapping_add(b - b'0');
				return;
			}
			b'~' => match self.csi_param {
				1 | 7 => self.home(e),
				4 | 8 => self.end(e),
				3 => self.delete(e),
				_ => {}
			},
			_ => {}
		}
		self.esc = 0;
		self.csi_param = 0;
	}

	fn insert(&mut self, c: u8, e: &mut Echo) {
		if self.len >= LD_LINE_MAX {
			return;
		}
		let mut i = self.len;
		while i > self.cursor {
			self.line[i] = self.line[i - 1];
			i -= 1;
		}
		self.line[self.cursor] = c;
		self.len += 1;
		if self.echo {
			e.put(&self.line[self.cursor..self.len]);
		}
		self.cursor += 1;
		if self.echo {
			self.move_left(self.len - self.cursor, e);
		}
	}

	fn backspace(&mut self, e: &mut Echo) {
		if self.cursor == 0 {
			return;
		}
		let mut i = self.cursor;
		while i < self.len {
			self.line[i - 1] = self.line[i];
			i += 1;
		}
		self.cursor -= 1;
		self.len -= 1;
		if self.echo {
			e.put(b"\x08");
			e.put(&self.line[self.cursor..self.len]);
			e.put(b" ");
			self.move_left(self.len - self.cursor + 1, e);
		}
	}

	fn delete(&mut self, e: &mut Echo) {
		if self.cursor >= self.len {
			return;
		}
		let mut i = self.cursor + 1;
		while i < self.len {
			self.line[i - 1] = self.line[i];
			i += 1;
		}
		self.len -= 1;
		if self.echo {
			e.put(&self.line[self.cursor..self.len]);
			e.put(b" ");
			self.move_left(self.len - self.cursor + 1, e);
		}
	}

	fn left(&mut self, e: &mut Echo) {
		if self.cursor > 0 {
			if self.echo {
				e.put(b"\x08");
			}
			self.cursor -= 1;
		}
	}

	fn right(&mut self, e: &mut Echo) {
		if self.cursor < self.len {
			if self.echo {
				e.put(&self.line[self.cursor..self.cursor + 1]);
			}
			self.cursor += 1;
		}
	}

	fn home(&mut self, e: &mut Echo) {
		if self.echo {
			self.move_left(self.cursor, e);
		}
		self.cursor = 0;
	}

	fn end(&mut self, e: &mut Echo) {
		if self.echo {
			e.put(&self.line[self.cursor..self.len]);
		}
		self.cursor = self.len;
	}

	fn move_left(&self, n: usize, e: &mut Echo) {
		for _ in 0..n {
			e.put(b"\x08");
		}
	}

	// Ctrl+U: erase the whole line.
	fn kill_line(&mut self, e: &mut Echo) {
		self.replace_line(b"", e);
	}

	// Ctrl+W: erase the word before the cursor (trailing spaces, then the word).
	fn kill_word(&mut self, e: &mut Echo) {
		while self.cursor > 0 && self.line[self.cursor - 1] == b' ' {
			self.backspace(e);
		}
		while self.cursor > 0 && self.line[self.cursor - 1] != b' ' {
			self.backspace(e);
		}
	}

	fn replace_line(&mut self, new: &[u8], e: &mut Echo) {
		if self.echo {
			e.put(&self.line[self.cursor..self.len]);
			for _ in 0..self.len {
				e.put(b"\x08 \x08");
			}
		}
		let n = new.len().min(LD_LINE_MAX);
		self.line[..n].copy_from_slice(&new[..n]);
		self.len = n;
		self.cursor = n;
		if self.echo {
			e.put(&self.line[..n]);
		}
	}

	fn history_prev(&mut self, e: &mut Echo) {
		if self.hist_pos == 0 {
			return;
		}
		self.hist_pos -= 1;
		let mut tmp = [0u8; LD_LINE_MAX];
		let h = &self.history[self.hist_pos];
		let n = h.len().min(LD_LINE_MAX);
		tmp[..n].copy_from_slice(&h[..n]);
		self.replace_line(&tmp[..n], e);
	}

	fn history_next(&mut self, e: &mut Echo) {
		if self.hist_pos >= self.history.len() {
			return;
		}
		self.hist_pos += 1;
		if self.hist_pos == self.history.len() {
			self.replace_line(b"", e);
		} else {
			let mut tmp = [0u8; LD_LINE_MAX];
			let h = &self.history[self.hist_pos];
			let n = h.len().min(LD_LINE_MAX);
			tmp[..n].copy_from_slice(&h[..n]);
			self.replace_line(&tmp[..n], e);
		}
	}

	// Record the submitted line in history (skipping empty / duplicate), then reset.
	fn commit(&mut self) {
		let trimmed = ld_trim(&self.line[..self.len]);
		if !trimmed.is_empty() && self.history.last().map(|h: &Vec<u8>| h.as_slice()) != Some(trimmed) {
			if self.history.len() >= LD_HIST_MAX {
				self.history.remove(0);
			}
			self.history.push(trimmed.to_vec());
		}
		self.len = 0;
		self.cursor = 0;
		self.hist_pos = self.history.len();
		self.esc = 0;
		self.csi_param = 0;
		self.eof = false;
	}
}

// Trim ASCII whitespace from both ends (the line discipline's history dedup).
fn ld_trim(mut s: &[u8]) -> &[u8] {
	while let [first, rest @ ..] = s {
		if first.is_ascii_whitespace() {
			s = rest;
		} else {
			break;
		}
	}
	while let [rest @ .., last] = s {
		if last.is_ascii_whitespace() {
			s = rest;
		} else {
			break;
		}
	}
	s
}

// One virtual terminal: its render state (a cell grid; None when headless), the service
// end of the console channel its shell writes output to and reads keys from, and the
// tty line discipline that cooks its keyboard input.
struct Vt {
	term: Option<Term>,
	client: u64,
	// The per-VT control channel to this VT's shell: the shell sends SET_FG (with the
	// foreground job's Process handle) / CLEAR_FG so the tty knows who owns it, and the
	// console sends JOB_STOPPED back when the user suspends the job with Ctrl+Z.
	control: u64,
	// The foreground job's Process handle while one runs (set by SET_FG, cleared by
	// CLEAR_FG / Ctrl+Z). When Some, the line discipline turns the signal keys into
	// signals to this process instead of editing the line.
	fg_proc: Option<u64>,
	// Boxed: the line-discipline buffer (a 128-byte line + history) is large, and a Vt is
	// returned by value through the deep spawn_vt call chain on a small (16 KiB) user
	// stack; keeping it inline overflowed the stack when opening a new VT.
	ld: Box<Ld>,
	// 0 for a display VT (its master is the hardware display + keyboard: it renders into
	// `term` and the foreground one owns the screen). For a program-hosted PTY it is the
	// console's end of the host's data channel: the line discipline cooks bytes the host
	// writes here (the typed-keys side) and the slave program's output is forwarded back
	// out it. A VT is thus a PTY whose master is the display; a PTY hosted by a program
	// (a future `ssh`, the `script` tool) is the same terminal with `term` None and the
	// master a channel instead of the framebuffer.
	master: u64,
}

// The capabilities ConsoleService holds to spawn a shell for any additional VT: a
// factory connection to each multi-client service (it mints a fresh per-VT client from
// each with `service_connect` / `network.open`) and the init package handle (it dups a
// read-only view per shell, and looks up the shell ELF in it).
struct Factories {
	storage: u64,
	log: u64,
	device: u64,
	process: u64,
	config: u64,
	net: u64,
	time: u64,
	audio: u64,
	pkg_handle: u64,
}

// The whole console session: the framebuffer it owns, the kernel keystroke channel, the
// live VTs (vts[fg] is foreground and owns the display), and the spawn capabilities.
struct Console {
	addr: u64,
	fb: Framebuffer,
	has_fb: bool,
	// The virtio-gpu driver's display channel, or 0 for the boot framebuffer (which is
	// visible directly, no present step). `present` FLUSHes the foreground over it.
	gpu: u64,
	// The current display size in pixels (the visible sub-rectangle of the max `fb`
	// geometry). New VTs are sized to it, and the gpu driver grows it toward the max on a
	// host-window resize. Equals the full `fb` geometry for the boot framebuffer.
	cur_w: u32,
	cur_h: u32,
	input: u64,
	// Foreground VT output accumulated during one wake for the serial debug mirror, written
	// out AFTER the display present: the emulated serial port is baud-throttled, so mirroring
	// it after presenting keeps a slow serial console from delaying the SPICE/VNC display.
	// Cleared after each drain.
	serial: Vec<u8>,
	vts: Vec<Vt>,
	fg: usize,
	// Program-hosted PTYs: terminals whose master is another program (the `script` tool,
	// a future `ssh`) instead of the display. Each runs a slave program (a shell) over its
	// own console + control channels with the same line discipline / signals / winsize as a
	// VT; the host drives it over the `master` channel. None is ever the foreground - they
	// are not on the screen - so they are kept apart from `vts` to leave the display path
	// (foreground, scrollback, switch, gpu-resize) untouched.
	ptys: Vec<Vt>,
	facs: Factories,
	package: Package<'static>,
	pkg_len: usize,
}

// Receive the "GPU" bootstrap message, returning the gpu driver's display channel, or 0
// when there is no virtio-gpu device. A 0 handle is valid here (unlike the tagged
// service factories, which require a capability), so this does not use recv_tagged.
unsafe fn recv_gpu(bootstrap: u64, buf: &mut [u8]) -> u64 {
	unsafe {
		match recv_blocking(bootstrap, buf) {
			Received::Message { len, handle } if len >= 3 && &buf[..3] == b"GPU" => handle,
			_ => 0,
		}
	}
}

// Acquire the display: prefer the virtio-gpu driver's shared framebuffer (which it
// presents on FLUSH and resizes on a host-window change), falling back to the boot
// framebuffer the kernel maps directly when there is no virtio-gpu device or the connect
// fails. Returns (pixel base, max geometry, whether a framebuffer was acquired, the gpu
// channel to FLUSH - 0 for the boot framebuffer, which needs no present, and the current
// display width/height the terminal is sized to within that max geometry).
unsafe fn acquire_display(gpu: u64, buf: &mut [u8]) -> (u64, Framebuffer, bool, u64, u32, u32) {
	unsafe {
		if gpu != 0 {
			if let Some((addr, fb, cur_w, cur_h)) = gpu_framebuffer(gpu, buf) {
				return (addr, fb, true, gpu, cur_w, cur_h);
			}
		}
		let mut fb: Framebuffer = Framebuffer::default();
		let addr_raw: i64 = framebuffer_map(&mut fb);
		let has_fb: bool = !sys_is_err(addr_raw as u64);
		// the boot framebuffer does not resize: its current size is its full geometry.
		(addr_raw as u64, fb, has_fb, 0, fb.width, fb.height)
	}
}

// Connect to the virtio-gpu driver: ask for the framebuffer (FB), receive its max
// geometry (the resource extent and pitch), its current display size, and a handle to
// the shared backing it renders into, and map it. Returns (pixel base, max geometry,
// current width, current height), or None on any failure (the caller then uses the boot
// framebuffer). The terminal is sized to the current display but may grow to the max.
unsafe fn gpu_framebuffer(gpu: u64, buf: &mut [u8]) -> Option<(u64, Framebuffer, u32, u32)> {
	unsafe {
		send_blocking(gpu, b"FB", 0);
		let (handle, len): (u64, usize) = match recv_blocking(gpu, buf) {
			Received::Message { len, handle } if handle != 0 => (handle, len),
			_ => return None,
		};
		let fb_len: usize = core::mem::size_of::<Framebuffer>();
		if len < fb_len + 8 {
			return None;
		}
		let fb: Framebuffer = (buf.as_ptr() as *const Framebuffer).read_unaligned();
		let cur_w: u32 = u32::from_le_bytes([buf[fb_len], buf[fb_len + 1], buf[fb_len + 2], buf[fb_len + 3]]);
		let cur_h: u32 = u32::from_le_bytes([buf[fb_len + 4], buf[fb_len + 5], buf[fb_len + 6], buf[fb_len + 7]]);
		let addr: i64 = dma_buffer_map(handle);
		if sys_is_err(addr as u64) {
			return None;
		}
		Some((addr as u64, fb, cur_w, cur_h))
	}
}

// Present the foreground framebuffer through the virtio-gpu driver - a no-op for the boot
// framebuffer, whose pixel writes are visible immediately. The driver copies the shared
// backing to its host resource and flushes it to the display.
unsafe fn present(gpu: u64) {
	unsafe {
		if gpu != 0 {
			send_blocking(gpu, b"FLUSH", 0);
		}
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. receive VT 1's console channel (its shell, spawned by ServiceManager, holds
		//    the other end), then a factory connection per multi-client service and a
		//    read-only view of the init package: the capabilities to spawn additional VTs.
		let client: u64 = recv_tagged(bootstrap, &mut buf, b"CLIENT").unwrap_or_else(|| exit());
		// VT 1's control channel (ServiceManager brokered it: the shell holds the other end).
		let control: u64 = recv_tagged(bootstrap, &mut buf, b"CONTROL").unwrap_or_else(|| exit());
		let storage: u64 = recv_tagged(bootstrap, &mut buf, b"FSTORAGE").unwrap_or_else(|| exit());
		let log: u64 = recv_tagged(bootstrap, &mut buf, b"FLOG").unwrap_or_else(|| exit());
		let device: u64 = recv_tagged(bootstrap, &mut buf, b"FDEVICE").unwrap_or_else(|| exit());
		let process: u64 = recv_tagged(bootstrap, &mut buf, b"FPROCESS").unwrap_or_else(|| exit());
		let config: u64 = recv_tagged(bootstrap, &mut buf, b"FCONFIG").unwrap_or_else(|| exit());
		let time: u64 = recv_tagged(bootstrap, &mut buf, b"FTIME").unwrap_or_else(|| exit());
		let audio: u64 = recv_tagged(bootstrap, &mut buf, b"FAUDIO").unwrap_or_else(|| exit());
		let net: u64 = recv_tagged(bootstrap, &mut buf, b"FNET").unwrap_or_else(|| exit());
		// The gpu driver's display channel (0 = no virtio-gpu device; a 0 handle is valid
		// here, unlike the tagged factories above, so we do not use recv_tagged).
		let gpu: u64 = recv_gpu(bootstrap, &mut buf);
		let (pkg_handle, archive): (u64, &'static [u8]) = recv_package(bootstrap, &mut buf).unwrap_or_else(|| exit());
		let package: Package = Package::parse(archive).unwrap_or_else(|| exit());
		let pkg_len: usize = archive.len();

		// 2. acquire the display: the virtio-gpu driver's resizable shared framebuffer if
		//    present (it presents on FLUSH), else the boot framebuffer the kernel maps
		//    directly (the test path). A headless boot has neither; we still serve input.
		//    The framebuffer is the maximum (resource) geometry; the terminal is sized to
		//    the current display, which the gpu driver grows toward the max on a resize.
		let (addr, fb, has_fb, gpu, cur_w, cur_h): (u64, Framebuffer, bool, u64, u32, u32) = acquire_display(gpu, &mut buf);
		let term: Option<Term> = if has_fb {
			let mut t = Term::new(addr, &fb);
			t.resize(cur_w as usize / CELL_W, cur_h as usize / CELL_H);
			t.screen.clear();
			for &b in b"ConsoleService: online\n" {
				t.screen.put_byte(b);
			}
			t.flush();
			Some(t)
		} else {
			None
		};

		// 3. report in to the supervisor.
		send_blocking(bootstrap, b"ConsoleService: online", 0);

		// 4. run the multiplexing terminal loop, starting with VT 1.
		let facs: Factories = Factories { storage, log, device, process, config, net, time, audio, pkg_handle };
		let mut console: Console = Console { addr, fb, has_fb, gpu, cur_w, cur_h, input: 0, serial: Vec::new(), vts: alloc::vec![Vt { term, client, control, fg_proc: None, ld: Box::new(Ld::new()), master: 0 }], fg: 0, ptys: Vec::new(), facs, package, pkg_len };
		run(&mut console);
	}
}

// The session loop: attach to the kernel's console input, then multiplex the keystroke
// channel and every live VT's output channel. Keystrokes go to the foreground VT's shell
// unless they are a switch chord (intercepted here); a VT's output is rendered into its
// own grid, and the foreground VT flushes to the framebuffer and mirrors to the serial
// port. A self-driven blink timer is avoided: a thread that re-blocks on a deadline keeps
// the cooperative `run_until_idle` (the boot driver) from ever settling.
unsafe fn run(console: &mut Console) -> ! {
	unsafe {
		// attach a channel the kernel feeds keystrokes on.
		let (feed, input): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => exit(),
		};
		if sys_is_err(syscall(SYS_CONSOLE_ATTACH, feed, 0, 0, 0)) {
			exit();
		}
		console.input = input;
		let mut keys: [u8; 64] = [0u8; 64];
		let mut out: [u8; 1024] = [0u8; 1024];
		let mut waits: [u64; 2 + 2 * NVT + 3 * PTY_MAX] = [0u64; 2 + 2 * NVT + 3 * PTY_MAX];
		// present the initial banner (the foreground term was rendered in __user_main).
		present(console.gpu);
		loop {
			// wait set: the keyboard channel (index 0), then each display VT's data channel
			// and its control channel interleaved (data at 1 + 2*i, control at 2 + 2*i),
			// then the gpu driver's display channel (when present, it sends RESIZE on a
			// host-window change), then each program-hosted PTY's slave-data, slave-control,
			// and master channels interleaved (data / control / master at pty_base + 3*j).
			waits[0] = console.input;
			let nv: usize = console.vts.len();
			for i in 0..nv {
				waits[1 + 2 * i] = console.vts[i].client;
				waits[2 + 2 * i] = console.vts[i].control;
			}
			let gpu_idx: usize = 1 + 2 * nv;
			let have_gpu: bool = console.gpu != 0;
			if have_gpu {
				waits[gpu_idx] = console.gpu;
			}
			let pty_base: usize = gpu_idx + have_gpu as usize;
			let np: usize = console.ptys.len();
			for j in 0..np {
				waits[pty_base + 3 * j] = console.ptys[j].client;
				waits[pty_base + 3 * j + 1] = console.ptys[j].control;
				waits[pty_base + 3 * j + 2] = console.ptys[j].master;
			}
			let total: usize = pty_base + 3 * np;
			// Block (~0% CPU) until a channel is ready: a keystroke, VT output, a gpu RESIZE, or
			// a program-hosted PTY's traffic.
			let ready: i64 = wait_any(&waits[..total], 0);
			if ready >= 0 {
				let r: usize = ready as usize;
				if r == 0 {
					// keystrokes from the kernel console input.
					if let Received::Message { len, .. } = recv_blocking(console.input, &mut keys) {
						handle_keys(console, &keys[..len]);
					}
				} else if have_gpu && r == gpu_idx {
					// the gpu driver reports a host-window resize: refit every VT.
					handle_gpu_resize(console);
				} else if r < gpu_idx {
					let vi: usize = (r - 1) / 2;
					if (r - 1) % 2 == 0 {
						// Output bytes from VT vi's shell: drain the whole burst into the grid
						// before the single present below, so a multi-line command (e.g. `help`)
						// paints in one GPU flush instead of one full-frame flush per printed line.
						loop {
							match try_recv(console.vts[vi].client, &mut out) {
								Polled::Message { len, .. } => render_output(console, vi, &out[..len]),
								Polled::Empty => break,
								Polled::Closed => {
									close_vt(console, vi);
									break;
								}
							}
						}
					} else {
						// a control message from VT vi's shell (SET_FG / CLEAR_FG / winsize / PTY_OPEN).
						handle_control(console, vi);
					}
				} else {
					// a program-hosted PTY: forward the slave program's output to the host, serve
					// its control channel, or feed the host's input through the line discipline.
					let pj: usize = (r - pty_base) / 3;
					match (r - pty_base) % 3 {
						0 => loop {
							// Drain the slave program's output burst before the single present below.
							match try_recv(console.ptys[pj].client, &mut out) {
								Polled::Message { len, .. } => pty_output(console, pj, &out[..len]),
								Polled::Empty => break,
								Polled::Closed => {
									close_pty(console, pj);
									break;
								}
							}
						},
						1 => handle_pty_control(console, pj),
						_ => match recv_blocking(console.ptys[pj].master, &mut out) {
							Received::Message { len, .. } => pty_master_input(console, pj, &out[..len]),
							Received::Closed => close_pty(console, pj),
						},
					}
				}
				// Present the freshly rendered foreground to the display (a no-op on the boot
				// framebuffer), THEN drain the serial debug mirror. present() only queues a
				// FLUSH to the gpu driver; the frame reaches the screen once that driver runs,
				// as soon as this thread next blocks. The mirror is best-effort and
				// non-blocking (the kernel drops it under backpressure rather than throttling
				// this thread on the baud-paced UART), so it never stalls the framebuffer the
				// SPICE/VNC user sees.
				present(console.gpu);
				if !console.serial.is_empty() {
					print(&console.serial);
					console.serial.clear();
				}
			}
		}
	}
}

// Render a VT's output: append it to that VT's grid, and if it is the foreground VT flush
// the grid to the framebuffer, ring the visual bell, and mirror the bytes to the serial
// port.
unsafe fn render_output(console: &mut Console, vi: usize, bytes: &[u8]) {
	unsafe {
		let fg: bool = vi == console.fg;
		let input: u64 = console.input;
		let gpu: u64 = console.gpu;
		let mut raw_req: Option<bool> = None;
		let mut echo_req: Option<bool> = None;
		if let Some(t) = console.vts[vi].term.as_mut() {
			for &b in bytes {
				t.screen.put_byte(b);
			}
			// Pick up any tty mode change the program asked for in this output.
			raw_req = t.screen.take_tty_raw_req();
			echo_req = t.screen.take_tty_echo_req();
			let bell: bool = t.screen.take_bell();
			if fg {
				t.flush();
				// BEL: invert the foreground screen briefly, then restore. A one-off timed
				// wait (woken early by a keystroke), not a perpetual re-arm, so it never
				// stalls the cooperative boot driver.
				if bell {
					t.draw_inverted();
					present(gpu);
					let _ = wait(input, clock() + BELL_FLASH_TICKS);
					t.screen.mark_all_dirty();
					t.flush();
				}
			}
		}
		// Apply the program's tty mode request to this VT's line discipline.
		if let Some(raw) = raw_req {
			console.vts[vi].ld.cooked = !raw;
		}
		if let Some(echo) = echo_req {
			console.vts[vi].ld.echo = echo;
		}
		if fg {
			// Buffer for the serial mirror; the session loop writes it after the present so the
			// baud-throttled serial port never delays the display (see `run`).
			console.serial.extend_from_slice(bytes);
		}
	}
}

// Dispatch keystrokes: a switch chord opens or cycles VTs (intercepted, never seen by a
// shell); otherwise the foreground VT's line discipline handles the byte - cooking it
// into the line editor and delivering a whole line on Enter, or (in raw mode) passing it
// straight through to the shell.
unsafe fn handle_keys(console: &mut Console, keys: &[u8]) {
	unsafe {
		for &b in keys {
			if b == CHORD_NEW {
				create_vt(console);
			} else if b == CHORD_NEXT {
				switch_next(console);
			} else if b == CHORD_SCROLL_UP {
				scroll_fg(console, true);
			} else if b == CHORD_SCROLL_DOWN {
				scroll_fg(console, false);
			} else {
				// any other keystroke returns the foreground VT to its live screen first.
				snap_fg_live(console);
				feed_key(console, b);
			}
		}
	}
}

// Feed one keystroke to the foreground VT. In cooked mode the line discipline edits +
// echoes it and, on Enter, ships the whole line (plus newline) to the shell; in raw mode
// the byte passes straight through.
unsafe fn feed_key(console: &mut Console, b: u8) {
	unsafe {
		let fg: usize = console.fg;
		feed_tty(&mut console.vts[fg], b);
	}
}

// Feed one input byte to a terminal's line discipline - shared by the foreground display
// VT (the keyboard) and a program-hosted PTY (the host's master channel). In cooked mode
// the discipline edits + echoes the byte and, on Enter, ships the whole line (plus a
// newline) to the slave program; in raw mode the byte passes straight through. The echo
// goes wherever the terminal's master is: a display VT mirrors it to the serial port (and
// renders live into its grid), a PTY sends it back out its master channel so the host
// (e.g. a remote terminal over ssh) sees what was typed.
unsafe fn feed_tty(vt: &mut Vt, b: u8) {
	unsafe {
		let client: u64 = vt.client;
		// A foreground job owns the tty: the signal keys become signals to it (the tty's
		// ISIG behaviour). Other input is swallowed - foreground programs do not read stdin,
		// so type-ahead is dropped the way a Linux tty drains its queue for a non-reader.
		if let Some(proc) = vt.fg_proc {
			match b {
				0x03 => {
					// Ctrl+C: interrupt. The job terminates, its completion channel closes,
					// and the shell's run_foreground returns to the prompt.
					signal(proc, SIG_INT);
					tty_echo(vt, b"^C\n");
				}
				0x1a => {
					// Ctrl+Z: suspend the job and tell the shell to background it. Clear
					// fg_proc so a second Ctrl+Z is not double-reported before CLEAR_FG.
					signal(proc, SIG_STOP);
					send_blocking(vt.control, b"JOB_STOPPED", 0);
					tty_echo(vt, b"^Z\n");
					if let Some(p) = vt.fg_proc.take() {
						close(p);
					}
				}
				0x1c => {
					// Ctrl+\: terminate.
					signal(proc, SIG_TERM);
					tty_echo(vt, b"^\\\n");
				}
				_ => {} // swallowed: a foreground job does not read stdin here
			}
			return;
		}
		if !vt.ld.cooked {
			send_blocking(client, &[b], 0);
			return;
		}
		let submitted: bool;
		let ser: EchoBuf;
		{
			let mut echo: Echo = Echo { term: vt.term.as_mut(), ser: EchoBuf::new() };
			submitted = vt.ld.feed(b, &mut echo);
			if let Some(t) = echo.term {
				t.flush();
			}
			ser = echo.ser;
		}
		// Deliver the echoed bytes: to the serial mirror for a display VT, to the master
		// channel for a PTY (its term, if any, was already rendered above).
		if vt.master == 0 {
			print(ser.as_slice());
		} else {
			send_blocking(vt.master, ser.as_slice(), 0);
		}
		if submitted {
			if vt.ld.eof {
				// Ctrl+D on an empty line: deliver a zero-byte read (EOF) so the shell
				// logs out, the way a tty signals end-of-input.
				vt.ld.commit();
				send_blocking(client, &[], 0);
			} else {
				let n: usize = vt.ld.len;
				let mut out: [u8; LD_LINE_MAX + 1] = [0u8; LD_LINE_MAX + 1];
				out[..n].copy_from_slice(&vt.ld.line[..n]);
				out[n] = b'\n';
				vt.ld.commit();
				send_blocking(client, &out[..n + 1], 0);
			}
		}
	}
}

// Echo a control-key acknowledgement (e.g. "^C") on a terminal: render it into the VT's
// grid and flush (a display VT), then send it on to the master - the serial port for a
// display VT, the host's master channel for a PTY - the way the line discipline echoes an
// edit. Only called for the foreground display VT or an active PTY.
unsafe fn tty_echo(vt: &mut Vt, msg: &[u8]) {
	unsafe {
		if let Some(t) = vt.term.as_mut() {
			for &c in msg {
				t.screen.put_byte(c);
			}
			t.flush();
		}
		if vt.master == 0 {
			print(msg);
		} else {
			send_blocking(vt.master, msg, 0);
		}
	}
}

// Handle a control message from VT vi's shell. SET_FG hands over the foreground job's
// Process handle, so the tty signals it on Ctrl+C / Ctrl+Z / Ctrl+\; CLEAR_FG takes it
// back when the job is done; GET / SET_WINSIZE report / change the terminal size; PTY_OPEN
// asks the tty to host a program on a new pseudo-terminal (for the `script` tool, a future
// ssh) and replies the master channel. The shell's end closing is driven by the data
// channel, so here a close just tears the VT down too.
unsafe fn handle_control(console: &mut Console, vi: usize) {
	unsafe {
		let mut cbuf: [u8; 64] = [0u8; 64];
		match recv_blocking(console.vts[vi].control, &mut cbuf) {
			Received::Message { len, handle } => {
				let msg: &[u8] = &cbuf[..len];
				if tty_fg_winsize(&mut console.vts[vi], msg, handle) {
					// SET_FG / CLEAR_FG / GET_WINSIZE handled identically for VTs and PTYs.
				} else if msg.starts_with(b"SET_WINSIZE") && len >= 15 {
					// Resize this VT's terminal to the requested cols x rows.
					let cols = u16::from_le_bytes([msg[11], msg[12]]) as usize;
					let rows = u16::from_le_bytes([msg[13], msg[14]]) as usize;
					resize_vt(console, vi, cols, rows);
				} else if msg.starts_with(b"PTY_OPEN") {
					// `PTY_OPEN` + a program name: open a pty hosting that program and reply
					// the master channel (the host's data side) to the shell.
					let mut nbuf: [u8; 32] = [0u8; 32];
					let name: &[u8] = if len > 8 { &cbuf[8..len] } else { b"shell" };
					let nn: usize = name.len().min(nbuf.len());
					nbuf[..nn].copy_from_slice(&name[..nn]);
					let control: u64 = console.vts[vi].control;
					match open_pty(console, &nbuf[..nn]) {
						Some(master) => {
							send_blocking(control, b"PTY", master);
						}
						None => {
							send_blocking(control, b"PTY_FAIL", 0);
						}
					}
				} else if handle != 0 {
					// an unexpected transferred handle would otherwise leak.
					close(handle);
				}
			}
			Received::Closed => close_vt(console, vi),
		}
	}
}

// Handle a control message from a program-hosted PTY's slave program (its shell): the
// same SET_FG / CLEAR_FG / GET_WINSIZE / SET_WINSIZE link as a VT (so signals and winsize
// work over a pty exactly as over the display), but a PTY has no display to repaint and a
// close tears the pty down rather than the session.
unsafe fn handle_pty_control(console: &mut Console, pj: usize) {
	unsafe {
		let mut cbuf: [u8; 64] = [0u8; 64];
		match recv_blocking(console.ptys[pj].control, &mut cbuf) {
			Received::Message { len, handle } => {
				let msg: &[u8] = &cbuf[..len];
				if tty_fg_winsize(&mut console.ptys[pj], msg, handle) {
					// handled
				} else if msg.starts_with(b"SET_WINSIZE") && len >= 15 {
					tty_resize_pty(&mut console.ptys[pj]);
				} else if handle != 0 {
					close(handle);
				}
			}
			Received::Closed => close_pty(console, pj),
		}
	}
}

// SET_FG / CLEAR_FG / GET_WINSIZE: the control messages handled identically for a display
// VT and a program PTY (they touch only the terminal's own foreground job and size).
// Returns true if `msg` was one of them; false otherwise, so the caller handles the rest
// (SET_WINSIZE, which repaints differently between a VT and a PTY, plus a VT's PTY_OPEN).
unsafe fn tty_fg_winsize(vt: &mut Vt, msg: &[u8], handle: u64) -> bool {
	unsafe {
		if msg.starts_with(b"SET_FG") && handle != 0 {
			if let Some(old) = vt.fg_proc.replace(handle) {
				close(old);
			}
		} else if msg.starts_with(b"CLEAR_FG") {
			if let Some(p) = vt.fg_proc.take() {
				close(p);
			}
		} else if msg.starts_with(b"GET_WINSIZE") {
			let (rows, cols) = tty_dims(vt);
			send_winsize(vt.control, b"WINSIZE", rows, cols);
		} else {
			return false;
		}
		true
	}
}

// A fixed default size for a program-hosted PTY (the host owns a pty's size; the slave only
// reads it, and resizing a pty from the host is a later ssh refinement).
const PTY_COLS: u16 = 80;
const PTY_ROWS: u16 = 24;

// A terminal's size as (rows, cols): a display VT's from its cell grid, a headless display
// VT 0 x 0, a program PTY the fixed PTY default.
fn tty_dims(vt: &Vt) -> (u16, u16) {
	match vt.term.as_ref() {
		Some(t) => (t.screen.rows() as u16, t.screen.cols() as u16),
		None if vt.master != 0 => (PTY_ROWS, PTY_COLS),
		None => (0, 0),
	}
}

// Send a winsize-bearing control reply: [tag][rows u16 LE][cols u16 LE].
unsafe fn send_winsize(control: u64, tag: &[u8], rows: u16, cols: u16) {
	unsafe {
		let mut r: [u8; 16] = [0u8; 16];
		let n = tag.len();
		r[..n].copy_from_slice(tag);
		r[n..n + 2].copy_from_slice(&rows.to_le_bytes());
		r[n + 2..n + 4].copy_from_slice(&cols.to_le_bytes());
		send_blocking(control, &r[..n + 4], 0);
	}
}

// Resize VT vi's terminal to cols x rows, repainting it if it is foreground, then send a
// RESIZE event (the SIGWINCH equivalent) back to its program with the actual (clamped)
// size so it can re-query and redraw.
unsafe fn resize_vt(console: &mut Console, vi: usize, cols: usize, rows: usize) {
	unsafe {
		let fg: bool = vi == console.fg;
		if let Some(t) = console.vts[vi].term.as_mut() {
			t.resize(cols, rows);
			if fg {
				t.flush();
			}
		}
		let (rows, cols) = tty_dims(&console.vts[vi]);
		send_winsize(console.vts[vi].control, b"RESIZE", rows, cols);
	}
}

// Acknowledge a slave program's SET_WINSIZE on a program-hosted PTY: a pty has no display
// to mode-set and its size is host-owned (fixed), so just reply RESIZE with the current
// size so the slave can re-query and redraw.
unsafe fn tty_resize_pty(vt: &mut Vt) {
	unsafe {
		let (rows, cols) = tty_dims(vt);
		send_winsize(vt.control, b"RESIZE", rows, cols);
	}
}

// Forward a PTY slave program's output bytes straight out to the host over the master
// channel. A pty has no framebuffer; the host (the `script` tool, a future ssh) renders or
// relays the bytes itself.
unsafe fn pty_output(console: &mut Console, pj: usize, bytes: &[u8]) {
	unsafe {
		send_blocking(console.ptys[pj].master, bytes, 0);
	}
}

// Feed bytes the host wrote on a PTY's master channel through that PTY's line discipline
// (the typed-keys side): cooked editing + echo back out the master, delivering whole lines
// to the slave program - exactly as the keyboard drives a display VT.
unsafe fn pty_master_input(console: &mut Console, pj: usize, bytes: &[u8]) {
	unsafe {
		for &b in bytes {
			feed_tty(&mut console.ptys[pj], b);
		}
	}
}

// A program-hosted PTY ended: its slave program exited (its console channel closed) or the
// host dropped the master. Close all its channels and remove it from the pool.
unsafe fn close_pty(console: &mut Console, pj: usize) {
	unsafe {
		close(console.ptys[pj].client);
		close(console.ptys[pj].control);
		close(console.ptys[pj].master);
		if let Some(p) = console.ptys[pj].fg_proc.take() {
			close(p);
		}
		console.ptys.remove(pj);
	}
}

// Handle a display-change event from the gpu driver: on a host-window resize it rebinds
// the scanout to the new pixel size and sends RESIZE + the new width/height. Refit every
// VT's terminal to the new size (each shell is notified, the SIGWINCH equivalent); the
// run loop re-presents the foreground afterwards. If the driver's channel has closed,
// stop polling it (the display freezes on the last frame - the driver is gone).
unsafe fn handle_gpu_resize(console: &mut Console) {
	unsafe {
		let mut buf: [u8; 32] = [0u8; 32];
		let len: usize = match recv_blocking(console.gpu, &mut buf) {
			Received::Message { len, .. } => len,
			Received::Closed => {
				console.gpu = 0;
				return;
			}
		};
		if len < 14 || &buf[..6] != b"RESIZE" {
			return;
		}
		let new_w: u32 = u32::from_le_bytes([buf[6], buf[7], buf[8], buf[9]]);
		let new_h: u32 = u32::from_le_bytes([buf[10], buf[11], buf[12], buf[13]]);
		if new_w == 0 || new_h == 0 {
			return;
		}
		console.cur_w = new_w;
		console.cur_h = new_h;
		let cols: usize = new_w as usize / CELL_W;
		let rows: usize = new_h as usize / CELL_H;
		let n: usize = console.vts.len();
		for vi in 0..n {
			resize_vt(console, vi, cols, rows);
		}
	}
}

// Open a new virtual terminal: spawn a fully-capable shell over its own per-VT service
// connections, make it foreground, and repaint. A no-op when headless or at the VT cap.
unsafe fn create_vt(console: &mut Console) {
	unsafe {
		if !console.has_fb || console.vts.len() >= NVT {
			return;
		}
		if let Some(vt) = spawn_vt(&console.facs, &console.package, console.pkg_len, console.addr, &console.fb, console.cur_w, console.cur_h) {
			console.vts.push(vt);
			console.fg = console.vts.len() - 1;
			repaint(console);
		}
	}
}

// Cycle the foreground to the next VT (round-robin) and repaint it. A no-op with one VT.
unsafe fn switch_next(console: &mut Console) {
	if console.vts.len() <= 1 {
		return;
	}
	console.fg = (console.fg + 1) % console.vts.len();
	repaint(console);
}

// Page the foreground VT's scrollback view up (older) or down (newer) and repaint it.
fn scroll_fg(console: &mut Console, up: bool) {
	if let Some(t) = console.vts[console.fg].term.as_mut() {
		if up {
			t.screen.scroll_view_up();
		} else {
			t.screen.scroll_view_down();
		}
		t.flush();
	}
}

// Return the foreground VT to its live screen if it was scrolled back, so typing always
// brings the cursor row back into view.
fn snap_fg_live(console: &mut Console) {
	if let Some(t) = console.vts[console.fg].term.as_mut() {
		if t.screen.snap_live() {
			t.flush();
		}
	}
}

// A VT's shell exited (its console channel closed): drop the VT and its connection. A
// secondary VT is removed and the foreground moves to a neighbour. The primary VT is the
// session leader (it owns the system's core service connections, brokered to it at boot),
// so its shell exiting ends the session: ConsoleService exits with it, detaching from the
// kernel console and bringing the machine down - the `exit`/Ctrl+D-to-halt the boot banner
// promises. (A clean exit only reaches here now that the shell's Process handle is no
// longer pinned by the supervisor; otherwise its console channel never closed.)
unsafe fn close_vt(console: &mut Console, vi: usize) {
	unsafe {
		if console.vts.len() <= 1 {
			exit();
		}
		close(console.vts[vi].client);
		close(console.vts[vi].control);
		if let Some(p) = console.vts[vi].fg_proc.take() {
			close(p);
		}
		console.vts.remove(vi);
		if console.fg >= console.vts.len() {
			console.fg = console.vts.len() - 1;
		} else if console.fg > vi {
			console.fg -= 1;
		}
		repaint(console);
	}
}

// Repaint the foreground VT's whole screen from its grid (after a switch or a VT add /
// remove changed which grid owns the display).
fn repaint(console: &mut Console) {
	if let Some(t) = console.vts[console.fg].term.as_mut() {
		t.screen.mark_all_dirty();
		t.flush();
	}
}

// Spawn a fully-capable shell over the given console + control channels (the shell's
// ends): mint a fresh per-session client from each service factory, spawn the shell ELF,
// hand it the full capability set in the order it expects (STORAGE, LOG, DEVICE, PROCESS,
// CONFIG, NET, TIME, AUDIO, CONSOLE, CONTROL, then PACKAGE), wait for its "online" report (it
// self-checks storage over its own connection), then release its bootstrap + Process
// handle. The terminal's liveness is tracked solely by its console channel closing on
// exit; holding the Process handle would pin the shell's handle table (and that channel)
// alive, so the terminal could never be reaped when the shell logs out or exits. Shared by
// spawn_vt (a display VT) and open_pty (a program-hosted PTY).
unsafe fn spawn_shell(facs: &Factories, package: &Package, pkg_len: usize, shell_console: u64, shell_control: u64) -> bool {
	unsafe {
		let shell_elf: &[u8] = match package.lookup(b"shell") {
			Some(e) => e,
			None => return false,
		};
		let storage: u64 = match service_connect(facs.storage) {
			Some(h) => h,
			None => return false,
		};
		let log: u64 = match service_connect(facs.log) {
			Some(h) => h,
			None => return false,
		};
		let device: u64 = match service_connect(facs.device) {
			Some(h) => h,
			None => return false,
		};
		let process: u64 = match service_connect(facs.process) {
			Some(h) => h,
			None => return false,
		};
		let config: u64 = match service_connect(facs.config) {
			Some(h) => h,
			None => return false,
		};
		let time: u64 = match service_connect(facs.time) {
			Some(h) => h,
			None => return false,
		};
		let audio: u64 = match service_connect(facs.audio) {
			Some(h) => h,
			None => return false,
		};
		let mut net = network::Client::new(ChannelTransport { chan: facs.net });
		let net_client: u64 = match net.open() {
			Some(Ok(h)) => h,
			_ => return false,
		};
		let (boot_parent, boot_child): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return false,
		};
		let shell_proc: i64 = spawn(shell_elf, boot_child);
		if shell_proc < 0 {
			return false;
		}
		send_blocking(boot_parent, b"STORAGE", storage);
		send_blocking(boot_parent, b"LOG", log);
		send_blocking(boot_parent, b"DEVICE", device);
		send_blocking(boot_parent, b"PROCESS", process);
		send_blocking(boot_parent, b"CONFIG", config);
		send_blocking(boot_parent, b"NET", net_client);
		send_blocking(boot_parent, b"TIME", time);
		send_blocking(boot_parent, b"AUDIO", audio);
		send_blocking(boot_parent, b"CONSOLE", shell_console);
		send_blocking(boot_parent, b"CONTROL", shell_control);
		let pkg_dup: i64 = duplicate(facs.pkg_handle, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
		if pkg_dup < 0 {
			close(boot_parent);
			close(shell_proc as u64);
			return false;
		}
		let mut pbuf: [u8; 16] = [0u8; 16];
		pbuf[..7].copy_from_slice(b"PACKAGE");
		pbuf[7..15].copy_from_slice(&(pkg_len as u64).to_le_bytes());
		send_blocking(boot_parent, &pbuf[..15], pkg_dup as u64);
		// wait for the shell to self-check storage and report in, then drop its bootstrap.
		let mut rbuf: [u8; 32] = [0u8; 32];
		if let Received::Closed = recv_blocking(boot_parent, &mut rbuf) {
			close(boot_parent);
			close(shell_proc as u64);
			return false;
		}
		close(boot_parent);
		close(shell_proc as u64);
		true
	}
}

// Open one VT's shell: create the VT's console + control channels, spawn a fully-capable
// shell over them, nudge it to print its first prompt, and return the VT (its cleared grid
// + the service ends of those channels). None on any failure.
unsafe fn spawn_vt(facs: &Factories, package: &Package, pkg_len: usize, addr: u64, fb: &Framebuffer, cur_w: u32, cur_h: u32) -> Option<Vt> {
	unsafe {
		let (vt_service, vt_client): (u64, u64) = channel()?;
		let (control_console, control_shell): (u64, u64) = channel()?;
		if !spawn_shell(facs, package, pkg_len, vt_client, control_shell) {
			close(vt_service);
			close(vt_client);
			close(control_console);
			close(control_shell);
			return None;
		}
		// nudge the new shell to print its first prompt: an empty line dispatches to a
		// silent reprompt, the same first prompt VT 1 shows at boot.
		send_blocking(vt_service, b"\n", 0);
		let mut term: Term = Term::new(addr, fb);
		term.resize(cur_w as usize / CELL_W, cur_h as usize / CELL_H);
		term.screen.clear();
		Some(Vt { term: Some(term), client: vt_service, control: control_console, fg_proc: None, ld: Box::new(Ld::new()), master: 0 })
	}
}

// Open a program-hosted PTY: a terminal whose master is another program (the `script`
// tool, a future ssh) instead of the hardware display. Spawn the named slave program over
// a fresh console + control channel pair - a shell gets the full capability set, any other
// program just its console + control - and return the master channel end the host drives it
// on. None on failure or at the PTY cap.
unsafe fn open_pty(console: &mut Console, name: &[u8]) -> Option<u64> {
	unsafe {
		if console.ptys.len() >= PTY_MAX {
			return None;
		}
		let (slave_service, slave_client): (u64, u64) = channel()?;
		let (control_console, control_slave): (u64, u64) = channel()?;
		let (master_console, master_host): (u64, u64) = channel()?;
		let is_shell: bool = name == b"shell";
		let ok: bool = if is_shell { spawn_shell(&console.facs, &console.package, console.pkg_len, slave_client, control_slave) } else { spawn_pty_program(&console.package, name, slave_client, control_slave) };
		if !ok {
			close(slave_service);
			close(slave_client);
			close(control_console);
			close(control_slave);
			close(master_console);
			close(master_host);
			return None;
		}
		// nudge a hosted shell to print its first prompt (an empty line reprompts silently).
		if is_shell {
			send_blocking(slave_service, b"\n", 0);
		}
		console.ptys.push(Vt { term: None, client: slave_service, control: control_console, fg_proc: None, ld: Box::new(Ld::new()), master: master_console });
		Some(master_host)
	}
}

// Spawn a minimal (non-shell) program as a PTY slave: it gets only its console + control
// channels (no service factories, no online handshake), the bootstrap a bare terminal
// client needs. Used to host a simple program on a pty (the pty loopback test slave); a
// shell uses spawn_shell.
unsafe fn spawn_pty_program(package: &Package, name: &[u8], program_console: u64, program_control: u64) -> bool {
	unsafe {
		let elf: &[u8] = match package.lookup(name) {
			Some(e) => e,
			None => return false,
		};
		let (boot_parent, boot_child): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return false,
		};
		let proc: i64 = spawn(elf, boot_child);
		if proc < 0 {
			return false;
		}
		send_blocking(boot_parent, b"CONSOLE", program_console);
		send_blocking(boot_parent, b"CONTROL", program_control);
		close(boot_parent);
		close(proc as u64);
		true
	}
}
