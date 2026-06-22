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

extern crate alloc;
use alloc::vec::Vec;

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

// One screen cell: a glyph plus its resolved foreground/background colours and an
// underline flag. The screen is a grid of these (`primary`, plus `alt` for the
// alternate screen); rendering reads the grid, so escape sequences and scrolling are
// pure grid edits and `flush` repaints only the cells that changed (damage tracking
// + double buffering).
#[derive(Clone, Copy, PartialEq)]
struct Cell {
	glyph: u8,
	fg: u32,
	bg: u32,
	underline: bool,
}

// The terminal: the mapped framebuffer + geometry, the cell grid (primary +
// alternate screen), the cursor and its saved copy, the scroll region, the current
// SGR state, and the output escape-parser state.
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
	saved_col: usize,
	saved_row: usize,
	scroll_top: usize,
	scroll_bottom: usize,
	fg: u32,
	bg: u32,
	palette: [u32; 16],
	cur_fg: u32,
	cur_bg: u32,
	cur_underline: bool,
	fg_idx: Option<u8>,
	bg_idx: Option<u8>,
	bold: bool,
	underline: bool,
	reverse: bool,
	saved_fg_idx: Option<u8>,
	saved_bg_idx: Option<u8>,
	saved_bold: bool,
	saved_underline: bool,
	saved_reverse: bool,
	cursor_visible: bool,
	esc_state: u8,
	csi_private: u8,
	params: [u16; 16],
	nparams: usize,
	primary: Vec<Cell>,
	alt: Vec<Cell>,
	alt_active: bool,
	dirty: Vec<bool>,
	last_caret: Option<(usize, usize)>,
}

impl Term {
	fn new(addr: u64, fb: &Framebuffer) -> Term {
		let cols = fb.width as usize / CELL_W;
		let rows = fb.height as usize / CELL_H;
		let mut t = Term { addr, width: fb.width as usize, height: fb.height as usize, pitch: fb.pitch as usize, bytes_per_pixel: fb.bytes_per_pixel as usize, red_shift: fb.red_shift, red_size: fb.red_size, green_shift: fb.green_shift, green_size: fb.green_size, blue_shift: fb.blue_shift, blue_size: fb.blue_size, cols, rows, col: 0, row: 0, saved_col: 0, saved_row: 0, scroll_top: 0, scroll_bottom: rows.saturating_sub(1), fg: 0, bg: 0, palette: [0; 16], cur_fg: 0, cur_bg: 0, cur_underline: false, fg_idx: None, bg_idx: None, bold: false, underline: false, reverse: false, saved_fg_idx: None, saved_bg_idx: None, saved_bold: false, saved_underline: false, saved_reverse: false, cursor_visible: true, esc_state: 0, csi_private: 0, params: [0; 16], nparams: 0, primary: Vec::new(), alt: Vec::new(), alt_active: false, dirty: Vec::new(), last_caret: None };
		t.fg = t.pack(FG.0, FG.1, FG.2);
		t.bg = t.pack(BG.0, BG.1, BG.2);
		for (i, &(r, g, b)) in ANSI_PALETTE.iter().enumerate() {
			t.palette[i] = t.pack(r, g, b);
		}
		t.cur_fg = t.fg;
		t.cur_bg = t.bg;
		let blank = Cell { glyph: b' ', fg: t.fg, bg: t.bg, underline: false };
		t.primary = alloc::vec![blank; cols * rows];
		t.alt = alloc::vec![blank; cols * rows];
		t.dirty = alloc::vec![true; cols * rows];
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

	// The active cell buffer: the alternate screen while it is up, else the primary.
	fn cells(&self) -> &[Cell] {
		if self.alt_active {
			&self.alt
		} else {
			&self.primary
		}
	}

	// A blank cell in the current background (so erase/scroll paint the SGR bg).
	fn blank(&self) -> Cell {
		Cell { glyph: b' ', fg: self.cur_fg, bg: self.cur_bg, underline: false }
	}

	fn mark_all_dirty(&mut self) {
		for d in self.dirty.iter_mut() {
			*d = true;
		}
	}

	// Write a cell into the active buffer, marking it dirty only when it changes.
	fn set_cell(&mut self, col: usize, row: usize, cell: Cell) {
		if col >= self.cols || row >= self.rows {
			return;
		}
		let idx = row * self.cols + col;
		let changed = {
			let buf = if self.alt_active { &mut self.alt } else { &mut self.primary };
			if buf[idx] != cell {
				buf[idx] = cell;
				true
			} else {
				false
			}
		};
		if changed {
			self.dirty[idx] = true;
		}
	}

	fn clear(&mut self) {
		let blank = self.blank();
		{
			let buf = if self.alt_active { &mut self.alt } else { &mut self.primary };
			for c in buf.iter_mut() {
				*c = blank;
			}
		}
		self.mark_all_dirty();
		self.col = 0;
		self.row = 0;
	}

	// Paint one cell from the grid to the framebuffer.
	fn draw_cell(&self, col: usize, row: usize) {
		let cell = self.cells()[row * self.cols + col];
		let base = (cell.glyph as usize) * FONT_H;
		let x0 = col * CELL_W;
		let y0 = row * CELL_H;
		for gy in 0..FONT_H {
			let bits = FONT[base + gy];
			for gx in 0..FONT_W {
				let color = if bits & (1 << gx) != 0 { cell.fg } else { cell.bg };
				for sy in 0..SCALE {
					for sx in 0..SCALE {
						self.put_pixel(x0 + gx * SCALE + sx, y0 + gy * SCALE + sy, color);
					}
				}
			}
		}
		if cell.underline {
			for y in (y0 + CELL_H - SCALE)..(y0 + CELL_H) {
				for x in x0..(x0 + CELL_W) {
					self.put_pixel(x, y, cell.fg);
				}
			}
		}
	}

	// A solid underline caret. The cell glyph is repainted first (by the dirty
	// flush), so this just paints over the bottom rows.
	fn draw_caret(&self, col: usize, row: usize) {
		let x0 = col * CELL_W;
		let y0 = row * CELL_H;
		for y in (y0 + CELL_H - SCALE)..(y0 + CELL_H) {
			for x in x0..(x0 + CELL_W) {
				self.put_pixel(x, y, self.cur_fg);
			}
		}
	}

	// Push the changed cells to the framebuffer, then draw the caret. Called once per
	// output batch: many bytes edit the grid, one flush paints it (double buffering).
	fn flush(&mut self) {
		if let Some((c, r)) = self.last_caret {
			let idx = r * self.cols + c;
			if idx < self.dirty.len() {
				self.dirty[idx] = true;
			}
		}
		for row in 0..self.rows {
			for col in 0..self.cols {
				let idx = row * self.cols + col;
				if self.dirty[idx] {
					self.draw_cell(col, row);
					self.dirty[idx] = false;
				}
			}
		}
		if self.cursor_visible && self.col < self.cols && self.row < self.rows {
			self.draw_caret(self.col, self.row);
			self.last_caret = Some((self.col, self.row));
		} else {
			self.last_caret = None;
		}
	}

	// Scroll the rows [top, bot] up by n, filling the freed bottom rows with blanks.
	fn region_up(&mut self, top: usize, bot: usize, n: usize) {
		let n = n.max(1);
		let cols = self.cols;
		let blank = self.blank();
		{
			let buf = if self.alt_active { &mut self.alt } else { &mut self.primary };
			for row in top..=bot {
				let src = row + n;
				for col in 0..cols {
					buf[row * cols + col] = if src <= bot { buf[src * cols + col] } else { blank };
				}
			}
		}
		for row in top..=bot {
			for col in 0..cols {
				self.dirty[row * cols + col] = true;
			}
		}
	}

	// Scroll the rows [top, bot] down by n, filling the freed top rows with blanks.
	fn region_down(&mut self, top: usize, bot: usize, n: usize) {
		let n = n.max(1);
		let cols = self.cols;
		let blank = self.blank();
		{
			let buf = if self.alt_active { &mut self.alt } else { &mut self.primary };
			for row in (top..=bot).rev() {
				for col in 0..cols {
					buf[row * cols + col] = if row >= top + n { buf[(row - n) * cols + col] } else { blank };
				}
			}
		}
		for row in top..=bot {
			for col in 0..cols {
				self.dirty[row * cols + col] = true;
			}
		}
	}

	fn scroll_up(&mut self, n: usize) {
		self.region_up(self.scroll_top, self.scroll_bottom, n);
	}

	fn scroll_down(&mut self, n: usize) {
		self.region_down(self.scroll_top, self.scroll_bottom, n);
	}

	// Line feed (IND): move down one line, scrolling the region if at the bottom.
	fn line_feed(&mut self) {
		if self.row == self.scroll_bottom {
			self.scroll_up(1);
		} else if self.row + 1 < self.rows {
			self.row += 1;
		}
	}

	// Reverse line feed (RI): move up one line, scrolling down if at the top.
	fn reverse_line_feed(&mut self) {
		if self.row == self.scroll_top {
			self.scroll_down(1);
		} else if self.row > 0 {
			self.row -= 1;
		}
	}

	fn put_char(&mut self, byte: u8) {
		if self.col >= self.cols {
			self.col = 0;
			self.line_feed();
		}
		let glyph = if (0x20..0x7f).contains(&byte) { byte } else { b'?' };
		let cell = Cell { glyph, fg: self.cur_fg, bg: self.cur_bg, underline: self.cur_underline };
		self.set_cell(self.col, self.row, cell);
		self.col += 1;
	}

	// The output parser entry point: feed one byte from the client's output stream.
	fn put_byte(&mut self, byte: u8) {
		match self.esc_state {
			1 => {
				self.esc_intermediate(byte);
				return;
			}
			2 => {
				self.csi_byte(byte);
				return;
			}
			3 => {
				self.osc_byte(byte);
				return;
			}
			_ => {}
		}
		match byte {
			0x1b => self.esc_state = 1,
			// No tty/line-discipline yet (M35i), so NL still implies a carriage return.
			b'\n' => {
				self.col = 0;
				self.line_feed();
			}
			b'\r' => self.col = 0,
			0x08 => {
				if self.col > 0 {
					self.col -= 1;
				}
			}
			b'\t' => {
				let next = (self.col / 8 + 1) * 8;
				self.col = next.min(self.cols.saturating_sub(1));
			}
			0x07 => {} // bell (a visible/audible bell is M35h)
			_ => {
				if byte >= 0x20 {
					self.put_char(byte);
				}
			}
		}
	}

	// After ESC: a CSI introducer (`[`), an OSC introducer (`]`), or a short two-byte
	// escape (DECSC/DECRC, IND/RI/NEL, RIS).
	fn esc_intermediate(&mut self, byte: u8) {
		match byte {
			b'[' => {
				self.esc_state = 2;
				self.params = [0; 16];
				self.nparams = 0;
				self.csi_private = 0;
			}
			b']' => self.esc_state = 3,
			b'7' => {
				self.save_cursor();
				self.esc_state = 0;
			}
			b'8' => {
				self.restore_cursor();
				self.esc_state = 0;
			}
			b'D' => {
				self.line_feed();
				self.esc_state = 0;
			}
			b'M' => {
				self.reverse_line_feed();
				self.esc_state = 0;
			}
			b'E' => {
				self.col = 0;
				self.line_feed();
				self.esc_state = 0;
			}
			b'c' => {
				self.reset();
				self.esc_state = 0;
			}
			_ => self.esc_state = 0,
		}
	}

	// Swallow an OSC string until BEL (0x07) or the start of an ST (ESC \).
	fn osc_byte(&mut self, byte: u8) {
		if byte == 0x07 {
			self.esc_state = 0;
		} else if byte == 0x1b {
			self.esc_state = 1;
		}
	}

	fn csi_byte(&mut self, byte: u8) {
		match byte {
			b'?' | b'>' | b'!' => self.csi_private = byte,
			b'0'..=b'9' => {
				let p = &mut self.params[self.nparams];
				*p = p.saturating_mul(10).saturating_add((byte - b'0') as u16);
			}
			b';' => {
				if self.nparams + 1 < self.params.len() {
					self.nparams += 1;
				}
			}
			0x20..=0x2f => {} // intermediate bytes - ignore
			0x40..=0x7e => {
				self.csi_dispatch(byte);
				self.esc_state = 0;
			}
			_ => self.esc_state = 0,
		}
	}

	// Read CSI parameter `i`, mapping an absent or zero parameter to `default`.
	fn param(&self, i: usize, default: usize) -> usize {
		if i <= self.nparams {
			let v = self.params[i] as usize;
			if v == 0 {
				default
			} else {
				v
			}
		} else {
			default
		}
	}

	fn csi_dispatch(&mut self, byte: u8) {
		match byte {
			b'A' => self.row = self.row.saturating_sub(self.param(0, 1)),
			b'B' => self.row = (self.row + self.param(0, 1)).min(self.rows.saturating_sub(1)),
			b'C' => self.col = (self.col + self.param(0, 1)).min(self.cols.saturating_sub(1)),
			b'D' => self.col = self.col.saturating_sub(self.param(0, 1)),
			b'E' => {
				self.col = 0;
				self.row = (self.row + self.param(0, 1)).min(self.rows.saturating_sub(1));
			}
			b'F' => {
				self.col = 0;
				self.row = self.row.saturating_sub(self.param(0, 1));
			}
			b'G' => self.col = (self.param(0, 1) - 1).min(self.cols.saturating_sub(1)),
			b'd' => self.row = (self.param(0, 1) - 1).min(self.rows.saturating_sub(1)),
			b'H' | b'f' => {
				let r = self.param(0, 1);
				let c = self.param(1, 1);
				self.row = (r - 1).min(self.rows.saturating_sub(1));
				self.col = (c - 1).min(self.cols.saturating_sub(1));
			}
			b'J' => self.erase_display(self.param(0, 0)),
			b'K' => self.erase_line(self.param(0, 0)),
			b'L' => {
				let n = self.param(0, 1);
				self.insert_lines(n);
			}
			b'M' => {
				let n = self.param(0, 1);
				self.delete_lines(n);
			}
			b'@' => {
				let n = self.param(0, 1);
				self.insert_chars(n);
			}
			b'P' => {
				let n = self.param(0, 1);
				self.delete_chars(n);
			}
			b'X' => {
				let n = self.param(0, 1);
				self.erase_chars(n);
			}
			b'S' => {
				let n = self.param(0, 1);
				self.scroll_up(n);
			}
			b'T' => {
				let n = self.param(0, 1);
				self.scroll_down(n);
			}
			b'r' => self.set_scroll_region(),
			b's' => self.save_cursor(),
			b'u' => self.restore_cursor(),
			b'h' => self.set_mode(true),
			b'l' => self.set_mode(false),
			b'm' => self.apply_sgr(),
			_ => {}
		}
	}

	// ED - erase in display: 0 cursor..end, 1 start..cursor, 2/3 the whole screen.
	fn erase_display(&mut self, mode: usize) {
		let cur = self.row * self.cols + self.col;
		let total = self.cols * self.rows;
		let (start, end) = match mode {
			0 => (cur, total),
			1 => (0, (cur + 1).min(total)),
			_ => (0, total),
		};
		self.fill_cells(start, end);
	}

	// EL - erase in line: 0 cursor..eol, 1 bol..cursor, 2 the whole line.
	fn erase_line(&mut self, mode: usize) {
		let row_start = self.row * self.cols;
		let (start, end) = match mode {
			0 => (row_start + self.col, row_start + self.cols),
			1 => (row_start, row_start + self.col + 1),
			_ => (row_start, row_start + self.cols),
		};
		self.fill_cells(start, end);
	}

	// Blank the cell range [start, end) and mark it dirty.
	fn fill_cells(&mut self, start: usize, end: usize) {
		let blank = self.blank();
		{
			let buf = if self.alt_active { &mut self.alt } else { &mut self.primary };
			let end = end.min(buf.len());
			for cell in &mut buf[start.min(end)..end] {
				*cell = blank;
			}
		}
		let end = end.min(self.dirty.len());
		for d in &mut self.dirty[start.min(end)..end] {
			*d = true;
		}
	}

	fn insert_lines(&mut self, n: usize) {
		if self.row < self.scroll_top || self.row > self.scroll_bottom {
			return;
		}
		self.region_down(self.row, self.scroll_bottom, n);
	}

	fn delete_lines(&mut self, n: usize) {
		if self.row < self.scroll_top || self.row > self.scroll_bottom {
			return;
		}
		self.region_up(self.row, self.scroll_bottom, n);
	}

	fn insert_chars(&mut self, n: usize) {
		let cols = self.cols;
		let row = self.row;
		let col = self.col;
		if col >= cols {
			return;
		}
		let n = n.min(cols - col);
		let blank = self.blank();
		let row_start = row * cols;
		{
			let buf = if self.alt_active { &mut self.alt } else { &mut self.primary };
			for c in (col..cols).rev() {
				buf[row_start + c] = if c >= col + n { buf[row_start + c - n] } else { blank };
			}
		}
		for c in col..cols {
			self.dirty[row_start + c] = true;
		}
	}

	fn delete_chars(&mut self, n: usize) {
		let cols = self.cols;
		let row = self.row;
		let col = self.col;
		if col >= cols {
			return;
		}
		let n = n.min(cols - col);
		let blank = self.blank();
		let row_start = row * cols;
		{
			let buf = if self.alt_active { &mut self.alt } else { &mut self.primary };
			for c in col..cols {
				buf[row_start + c] = if c + n < cols { buf[row_start + c + n] } else { blank };
			}
		}
		for c in col..cols {
			self.dirty[row_start + c] = true;
		}
	}

	fn erase_chars(&mut self, n: usize) {
		let row_start = self.row * self.cols;
		let end = (self.col + n).min(self.cols);
		self.fill_cells(row_start + self.col, row_start + end);
	}

	// DECSTBM - set the scroll region; resets to the whole screen on bad params, and
	// homes the cursor.
	fn set_scroll_region(&mut self) {
		let top = self.param(0, 1).saturating_sub(1);
		let bottom = self.param(1, self.rows).saturating_sub(1).min(self.rows.saturating_sub(1));
		if top < bottom {
			self.scroll_top = top;
			self.scroll_bottom = bottom;
		} else {
			self.scroll_top = 0;
			self.scroll_bottom = self.rows.saturating_sub(1);
		}
		self.col = 0;
		self.row = 0;
	}

	fn save_cursor(&mut self) {
		self.saved_col = self.col;
		self.saved_row = self.row;
		self.saved_fg_idx = self.fg_idx;
		self.saved_bg_idx = self.bg_idx;
		self.saved_bold = self.bold;
		self.saved_underline = self.underline;
		self.saved_reverse = self.reverse;
	}

	fn restore_cursor(&mut self) {
		self.col = self.saved_col.min(self.cols.saturating_sub(1));
		self.row = self.saved_row.min(self.rows.saturating_sub(1));
		self.fg_idx = self.saved_fg_idx;
		self.bg_idx = self.saved_bg_idx;
		self.bold = self.saved_bold;
		self.underline = self.saved_underline;
		self.reverse = self.saved_reverse;
		self.recompute_colors();
	}

	// DEC private mode set/reset (CSI ? ... h/l): cursor visibility + alternate screen.
	fn set_mode(&mut self, enable: bool) {
		if self.csi_private != b'?' {
			return;
		}
		for i in 0..=self.nparams {
			match self.params[i] {
				25 => self.cursor_visible = enable,
				47 | 1047 | 1049 => {
					if enable {
						self.enter_alt();
					} else {
						self.leave_alt();
					}
				}
				_ => {}
			}
		}
	}

	fn enter_alt(&mut self) {
		if self.alt_active {
			return;
		}
		self.save_cursor();
		self.alt_active = true;
		let blank = self.blank();
		for c in self.alt.iter_mut() {
			*c = blank;
		}
		self.mark_all_dirty();
		self.col = 0;
		self.row = 0;
	}

	fn leave_alt(&mut self) {
		if !self.alt_active {
			return;
		}
		self.alt_active = false;
		self.mark_all_dirty();
		self.restore_cursor();
	}

	// RIS - reset to the initial state.
	fn reset(&mut self) {
		self.fg_idx = None;
		self.bg_idx = None;
		self.bold = false;
		self.underline = false;
		self.reverse = false;
		self.cursor_visible = true;
		self.scroll_top = 0;
		self.scroll_bottom = self.rows.saturating_sub(1);
		self.alt_active = false;
		self.recompute_colors();
		self.clear();
	}

	fn apply_sgr(&mut self) {
		for i in 0..=self.nparams {
			match self.params[i] {
				0 => {
					self.fg_idx = None;
					self.bg_idx = None;
					self.bold = false;
					self.underline = false;
					self.reverse = false;
				}
				1 => self.bold = true,
				22 => self.bold = false,
				3 | 23 => {} // italic - the 8x8 font cannot render it
				4 => self.underline = true,
				24 => self.underline = false,
				7 => self.reverse = true,
				27 => self.reverse = false,
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
		let fg = match self.fg_idx {
			Some(i) => self.palette[if self.bold && i < 8 { (i + 8) as usize } else { i as usize }],
			None => self.fg,
		};
		let bg = match self.bg_idx {
			Some(i) => self.palette[i as usize],
			None => self.bg,
		};
		if self.reverse {
			self.cur_fg = bg;
			self.cur_bg = fg;
		} else {
			self.cur_fg = fg;
			self.cur_bg = bg;
		}
		self.cur_underline = self.underline;
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
				t.put_byte(b);
			}
			t.flush();
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
									t.put_byte(b);
								}
								t.flush();
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
