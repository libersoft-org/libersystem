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

// The number of virtual terminals the console multiplexes. Each VT is an independent
// shell over its own per-VT service connections; the foreground VT owns the display.
const NVT: usize = 4;

// Control-byte chords intercepted by the console (never forwarded to a shell): the
// virtio-input driver maps Ctrl+N to 0x0e (open a new VT) and Ctrl+] to 0x1d (cycle the
// foreground). F-keys are not mapped by the driver and Alt+key collides with escape
// sequences, so single control bytes are the unambiguous, unobtrusive switch keys.
const CHORD_NEW: u8 = 0x0e;
const CHORD_NEXT: u8 = 0x1d;

// One virtual terminal: its render state (a cell grid; None when headless) and the
// service end of the console channel its shell writes output to and reads keys from.
struct Vt {
	term: Option<Term>,
	client: u64,
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
	pkg_handle: u64,
}

// The whole console session: the framebuffer it owns, the kernel keystroke channel, the
// live VTs (vts[fg] is foreground and owns the display), and the spawn capabilities.
struct Console {
	addr: u64,
	fb: Framebuffer,
	has_fb: bool,
	input: u64,
	vts: Vec<Vt>,
	fg: usize,
	facs: Factories,
	package: Package<'static>,
	pkg_len: usize,
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. receive VT 1's console channel (its shell, spawned by ServiceManager, holds
		//    the other end), then a factory connection per multi-client service and a
		//    read-only view of the init package: the capabilities to spawn additional VTs.
		let client: u64 = recv_tagged(bootstrap, &mut buf, b"CLIENT").unwrap_or_else(|| exit());
		let storage: u64 = recv_tagged(bootstrap, &mut buf, b"FSTORAGE").unwrap_or_else(|| exit());
		let log: u64 = recv_tagged(bootstrap, &mut buf, b"FLOG").unwrap_or_else(|| exit());
		let device: u64 = recv_tagged(bootstrap, &mut buf, b"FDEVICE").unwrap_or_else(|| exit());
		let process: u64 = recv_tagged(bootstrap, &mut buf, b"FPROCESS").unwrap_or_else(|| exit());
		let config: u64 = recv_tagged(bootstrap, &mut buf, b"FCONFIG").unwrap_or_else(|| exit());
		let time: u64 = recv_tagged(bootstrap, &mut buf, b"FTIME").unwrap_or_else(|| exit());
		let net: u64 = recv_tagged(bootstrap, &mut buf, b"FNET").unwrap_or_else(|| exit());
		let (pkg_handle, archive): (u64, &'static [u8]) = recv_package(bootstrap, &mut buf).unwrap_or_else(|| exit());
		let package: Package = Package::parse(archive).unwrap_or_else(|| exit());
		let pkg_len: usize = archive.len();

		// 2. map the framebuffer (this hands us the display; the kernel console stops).
		//    A headless boot has no framebuffer; we still serve so input/serial work.
		let mut fb: Framebuffer = Framebuffer::default();
		let addr_raw: i64 = framebuffer_map(&mut fb);
		let has_fb: bool = !sys_is_err(addr_raw as u64);
		let addr: u64 = addr_raw as u64;
		let term: Option<Term> = if has_fb {
			let mut t = Term::new(addr, &fb);
			t.clear();
			for &b in b"ConsoleService: online\n" {
				t.put_byte(b);
			}
			t.flush();
			Some(t)
		} else {
			None
		};

		// 3. report in to the supervisor.
		send_blocking(bootstrap, b"ConsoleService: online", 0);

		// 4. run the multiplexing terminal loop, starting with VT 1.
		let facs: Factories = Factories { storage, log, device, process, config, net, time, pkg_handle };
		let mut console: Console = Console { addr, fb, has_fb, input: 0, vts: alloc::vec![Vt { term, client }], fg: 0, facs, package, pkg_len };
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
		let mut waits: [u64; 1 + NVT] = [0u64; 1 + NVT];
		loop {
			// wait set: the keyboard channel (index 0) then each VT's console channel.
			waits[0] = console.input;
			let n: usize = console.vts.len();
			for i in 0..n {
				waits[1 + i] = console.vts[i].client;
			}
			let ready: i64 = wait_any(&waits[..1 + n], 0);
			if ready < 0 {
				continue;
			}
			let r: usize = ready as usize;
			if r == 0 {
				// keystrokes from the kernel console input.
				if let Received::Message { len, .. } = recv_blocking(console.input, &mut keys) {
					handle_keys(console, &keys[..len]);
				}
			} else {
				// output bytes from VT (r - 1)'s shell.
				let vi: usize = r - 1;
				match recv_blocking(console.vts[vi].client, &mut out) {
					Received::Message { len, .. } => render_output(console, vi, &out[..len]),
					Received::Closed => close_vt(console, vi),
				}
			}
		}
	}
}

// Render a VT's output: append it to that VT's grid, and if it is the foreground VT flush
// the grid to the framebuffer and mirror the bytes to the serial port.
unsafe fn render_output(console: &mut Console, vi: usize, bytes: &[u8]) {
	unsafe {
		let fg: bool = vi == console.fg;
		if let Some(t) = console.vts[vi].term.as_mut() {
			for &b in bytes {
				t.put_byte(b);
			}
			if fg {
				t.flush();
			}
		}
		if fg {
			print(bytes);
		}
	}
}

// Dispatch keystrokes: a switch chord opens or cycles VTs (intercepted, never seen by a
// shell); any other byte is forwarded to the foreground VT's shell.
unsafe fn handle_keys(console: &mut Console, keys: &[u8]) {
	unsafe {
		for &b in keys {
			if b == CHORD_NEW {
				create_vt(console);
			} else if b == CHORD_NEXT {
				switch_next(console);
			} else {
				send_blocking(console.vts[console.fg].client, &[b], 0);
			}
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
		if let Some(vt) = spawn_vt(&console.facs, &console.package, console.pkg_len, console.addr, &console.fb) {
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

// A VT's shell exited (its console channel closed): drop the VT and its connection. If it
// was the last VT, the session is over and ConsoleService exits with it.
unsafe fn close_vt(console: &mut Console, vi: usize) {
	unsafe {
		if console.vts.len() <= 1 {
			exit();
		}
		close(console.vts[vi].client);
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
		t.mark_all_dirty();
		t.flush();
	}
}

// Spawn one VT's shell: mint a fresh per-VT client from each service factory, create the
// VT's console channel, spawn the shell ELF from the package, and hand it the full
// capability set in the order it expects (STORAGE, LOG, DEVICE, PROCESS, CONFIG, NET,
// TIME, CONSOLE, then PACKAGE). Wait for the shell's "online" report (it self-checks
// storage over its own connection), then nudge it to print its first prompt. Returns the
// VT (its cleared grid + the service end of its console channel) or None on any failure.
unsafe fn spawn_vt(facs: &Factories, package: &Package, pkg_len: usize, addr: u64, fb: &Framebuffer) -> Option<Vt> {
	unsafe {
		let shell_elf: &[u8] = package.lookup(b"shell")?;
		let storage: u64 = service_connect(facs.storage)?;
		let log: u64 = service_connect(facs.log)?;
		let device: u64 = service_connect(facs.device)?;
		let process: u64 = service_connect(facs.process)?;
		let config: u64 = service_connect(facs.config)?;
		let time: u64 = service_connect(facs.time)?;
		let mut net = network::Client::new(ChannelTransport { chan: facs.net });
		let net_client: u64 = match net.open() {
			Some(Ok(h)) => h,
			_ => return None,
		};
		let (vt_service, vt_client): (u64, u64) = channel()?;
		let (boot_parent, boot_child): (u64, u64) = channel()?;
		if spawn(shell_elf, boot_child) < 0 {
			return None;
		}
		send_blocking(boot_parent, b"STORAGE", storage);
		send_blocking(boot_parent, b"LOG", log);
		send_blocking(boot_parent, b"DEVICE", device);
		send_blocking(boot_parent, b"PROCESS", process);
		send_blocking(boot_parent, b"CONFIG", config);
		send_blocking(boot_parent, b"NET", net_client);
		send_blocking(boot_parent, b"TIME", time);
		send_blocking(boot_parent, b"CONSOLE", vt_client);
		let pkg_dup: i64 = duplicate(facs.pkg_handle, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
		if pkg_dup < 0 {
			return None;
		}
		let mut pbuf: [u8; 16] = [0u8; 16];
		pbuf[..7].copy_from_slice(b"PACKAGE");
		pbuf[7..15].copy_from_slice(&(pkg_len as u64).to_le_bytes());
		send_blocking(boot_parent, &pbuf[..15], pkg_dup as u64);
		// wait for the shell to self-check storage and report in, then drop its bootstrap.
		let mut rbuf: [u8; 32] = [0u8; 32];
		if let Received::Closed = recv_blocking(boot_parent, &mut rbuf) {
			close(boot_parent);
			return None;
		}
		close(boot_parent);
		// nudge the new shell to print its first prompt: an empty line dispatches to a
		// silent reprompt, the same first prompt VT 1 shows at boot.
		send_blocking(vt_service, b"\n", 0);
		let mut term: Term = Term::new(addr, fb);
		term.clear();
		Some(Vt { term: Some(term), client: vt_service })
	}
}
