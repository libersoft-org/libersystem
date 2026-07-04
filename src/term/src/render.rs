//! Framebuffer renderer (L3) for LiberSystem, shared by the kernel boot console and the
//! userspace ConsoleService.
//!
//! A `FramebufferRenderer` is a pure consumer of the grid model (`Screen`): it reads the
//! model's changed cells through the snapshot/diff interface (`cell`, `view_cell`,
//! `dirty_take`, `take_scrolls`), resolves their logical colours to packed pixels, and
//! blits them onto the current display `Surface`. The surface is abstract: a backend states
//! its pixel backing (`Raster`) and how to make writes visible (`present`), so the same
//! renderer drives the boot framebuffer, a virtio-gpu shared backing, or any future display.
//! `Term` ties a `Screen` to a renderer; both the kernel and ConsoleService own one.

use alloc::boxed::Box;

use crate::screen::{Cell, Color, CursorShape, Screen, ScrollOp};

// The 8x16 bitmap font: Unscii 2.1 (unscii-16 by Viznut, public domain), 2,997 glyphs
// covering ASCII, Latin-1 + Latin Extended-A (full Czech and Western/Central European
// coverage), Greek, Cyrillic, box drawing, block elements and the legacy-computing
// graphics. The asset is a sorted codepoint table plus one 16-byte bitmap per glyph
// (row-major, bit 7 = leftmost pixel): [count u32 LE][count x codepoint u32 LE]
// [count x 16 glyph bytes], generated from unscii-16.hex. A codepoint the font lacks
// renders as '?'.
static FONT: &[u8] = include_bytes!("unscii16.bin");

const FONT_W: usize = 8;
const FONT_H: usize = 16;
const SCALE: usize = 1;
// One text cell in pixels (a font glyph drawn at SCALE). Public so a consumer can size its
// grid to a framebuffer's pixel geometry.
pub const CELL_W: usize = FONT_W * SCALE;
pub const CELL_H: usize = FONT_H * SCALE;

// The 16-byte bitmap for a Unicode codepoint: a binary search over the asset's sorted
// codepoint table, falling back to '?' for a codepoint the font does not cover ('?' is
// guaranteed present). Called per changed cell, so the log2(n) probe is cheap.
fn glyph_bitmap(cp: u32) -> &'static [u8] {
	let count = u32::from_le_bytes([FONT[0], FONT[1], FONT[2], FONT[3]]) as usize;
	let table = &FONT[4..4 + count * 4];
	let glyphs_base = 4 + count * 4;
	let mut lo: usize = 0;
	let mut hi: usize = count;
	while lo < hi {
		let mid = (lo + hi) / 2;
		let entry = u32::from_le_bytes([table[mid * 4], table[mid * 4 + 1], table[mid * 4 + 2], table[mid * 4 + 3]]);
		if entry == cp {
			let at = glyphs_base + mid * FONT_H;
			return &FONT[at..at + FONT_H];
		}
		if entry < cp {
			lo = mid + 1;
		} else {
			hi = mid;
		}
	}
	if cp == b'?' as u32 {
		// unreachable as long as the asset carries '?': a fixed blank stops the recursion.
		static BLANK: [u8; FONT_H] = [0u8; FONT_H];
		return &BLANK;
	}
	glyph_bitmap(b'?' as u32)
}

// A linear framebuffer's geometry and pixel format, handed to a `Raster`. Decouples the
// renderer from any particular framebuffer description (the kernel's Limine response, the
// userspace ABI `Framebuffer`): each caller fills this from its own source.
pub struct Geometry {
	pub width: usize,
	pub height: usize,
	pub pitch: usize,
	pub bytes_per_pixel: usize,
	pub red_shift: u8,
	pub red_size: u8,
	pub green_shift: u8,
	pub green_size: u8,
	pub blue_shift: u8,
	pub blue_size: u8,
}

// The raw pixel buffer: a mapped linear framebuffer, its geometry, and its pixel format.
// The only place that touches pixels and the framebuffer address. A display backend (the
// boot framebuffer, the virtio-gpu shared backing) is a `Raster` plus how to make its writes
// visible; it holds no grid and no terminal state.
pub struct Raster {
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

impl Raster {
	pub fn new(addr: u64, g: &Geometry) -> Raster {
		Raster { addr, width: g.width, height: g.height, pitch: g.pitch, bytes_per_pixel: g.bytes_per_pixel, red_shift: g.red_shift, red_size: g.red_size, green_shift: g.green_shift, green_size: g.green_size, blue_shift: g.blue_shift, blue_size: g.blue_size }
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

	// Read one packed pixel back from the framebuffer - used by a backend handoff to copy
	// the existing on-screen pixels into the new backing.
	#[inline]
	fn read_pixel(&self, x: usize, y: usize) -> u32 {
		if x >= self.width || y >= self.height {
			return 0;
		}
		let offset = y * self.pitch + x * self.bytes_per_pixel;
		let mut bytes = [0u8; 4];
		unsafe {
			let base = (self.addr as *const u8).add(offset);
			for i in 0..self.bytes_per_pixel {
				bytes[i] = core::ptr::read_volatile(base.add(i));
			}
		}
		u32::from_le_bytes(bytes)
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

// The swappable display backend: a `Raster` the renderer draws into, plus how to make those
// writes reach the screen (`present`). The boot framebuffer's writes are visible immediately
// (present is a no-op); a virtio-gpu shared backing is copied to the host scanout on a FLUSH
// the present queues. The renderer targets "the current surface" through this trait; a
// backend handoff copies the existing pixels into the new backing (see
// FramebufferRenderer::handoff). The geometry/pixel methods delegate to the raster so a
// backend only states its backing and its present.
pub trait Surface {
	fn raster(&self) -> &Raster;
	// Make the rectangle `x, y, w, h` (pixels) of this frame visible - the renderer
	// passes the bounding box of what it painted since the last present, so a backend
	// that copies to a host scanout (virtio-gpu) moves only the changed pixels.
	fn present(&self, x: u32, y: u32, w: u32, h: u32);

	fn width(&self) -> usize {
		self.raster().width
	}
	fn height(&self) -> usize {
		self.raster().height
	}
	fn pack(&self, r: u8, g: u8, b: u8) -> u32 {
		self.raster().pack(r, g, b)
	}
	fn put_pixel(&self, x: usize, y: usize, color: u32) {
		self.raster().put_pixel(x, y, color);
	}
	fn read_pixel(&self, x: usize, y: usize) -> u32 {
		self.raster().read_pixel(x, y)
	}
	fn fill(&self, color: u32) {
		self.raster().fill(color);
	}
	fn scroll_pixels_up(&self, top: usize, bot: usize, n: usize) {
		self.raster().scroll_pixels_up(top, bot, n);
	}
	fn scroll_pixels_down(&self, top: usize, bot: usize, n: usize) {
		self.raster().scroll_pixels_down(top, bot, n);
	}
}

// The renderer (L3): the current display surface it draws onto plus its own `last_caret`
// (where the caret was last painted). It is a pure consumer of the grid model - it reads the
// model's changed cells through `Screen`'s snapshot/diff interface (`cell`, `view_cell`,
// `dirty_take`, `take_scrolls`), resolves their logical colours to packed pixels, blits
// them, and replays the model's recorded scrolls as bulk pixel copies. The surface is
// swappable (`handoff`) so the display backend can change under it. It never mutates the
// grid and holds no terminal state. `dirty` accumulates the pixel bounding box of
// everything painted since the last present, so the present moves only that rectangle.
struct FramebufferRenderer {
	surface: Box<dyn Surface>,
	last_caret: Option<(usize, usize)>,
	dirty: Option<(usize, usize, usize, usize)>,
}

// The terminal (L2 + L3): the grid model and the renderer that draws it. A thin façade that
// owns both and wires the model's output to the renderer's framebuffer.
pub struct Term {
	pub screen: Screen,
	renderer: FramebufferRenderer,
}

impl Term {
	pub fn new(surface: Box<dyn Surface>) -> Term {
		let cols = surface.width() / CELL_W;
		let rows = surface.height() / CELL_H;
		Term { screen: Screen::new(cols, rows), renderer: FramebufferRenderer::new(surface) }
	}

	// Reflow the model to fit new_cols x new_rows, clamped to what the physical framebuffer
	// can show, then clear the now-unused area (only when the grid actually changed). The
	// local stand-in for a virtio-gpu mode-set (M44).
	pub fn resize(&mut self, new_cols: usize, new_rows: usize) {
		let max_cols = (self.renderer.surface.width() / CELL_W).max(1);
		let max_rows = (self.renderer.surface.height() / CELL_H).max(1);
		if self.screen.resize(new_cols, new_rows, max_cols, max_rows) {
			self.renderer.last_caret = None;
			self.renderer.clear_screen(&self.screen);
		}
	}

	// Paint the model's pending output to the framebuffer (the model's scroll + dirty diff;
	// see FramebufferRenderer::flush).
	pub fn flush(&mut self) {
		self.renderer.flush(&mut self.screen);
	}

	// Make the rendered frame visible: a no-op on the boot framebuffer, a FLUSH to the gpu
	// driver on the virtio-gpu backing. Only the bounding box of what was painted since the
	// last present is pushed; with nothing painted, nothing is sent at all.
	pub fn present(&mut self) {
		if let Some((x0, y0, x1, y1)) = self.renderer.dirty.take() {
			let x1 = x1.min(self.renderer.surface.width());
			let y1 = y1.min(self.renderer.surface.height());
			if x0 < x1 && y0 < y1 {
				self.renderer.surface.present(x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32);
			}
		}
	}

	// Hand the display over to a new backend, preserving the on-screen pixels, then present
	// the whole frame (everything on the new backing is new to its scanout).
	pub fn handoff(&mut self, next: Box<dyn Surface>) {
		self.renderer.handoff(next);
		self.renderer.dirty = None;
		let (w, h) = (self.renderer.surface.width() as u32, self.renderer.surface.height() as u32);
		self.renderer.surface.present(0, 0, w, h);
	}

	// Toggle the caret's blink phase: erase the caret when it is shown, redraw it when it
	// is hidden. Returns true when a pixel changed, so the caller knows to present. Every
	// `flush` repaints the caret (each output batch resets the phase to visible), so a
	// periodic caller gets the classic solid-while-active, blinking-while-idle caret.
	pub fn blink_caret(&mut self) -> bool {
		self.renderer.blink_caret(&self.screen)
	}

	// Flash the screen with inverted colours (the visual bell) without touching the grid.
	pub fn draw_inverted(&mut self) {
		self.renderer.draw_inverted(&self.screen);
		let (w, h) = (self.renderer.surface.width(), self.renderer.surface.height());
		self.renderer.mark(0, 0, w, h);
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
	fn new(surface: Box<dyn Surface>) -> FramebufferRenderer {
		FramebufferRenderer { surface, last_caret: None, dirty: None }
	}

	// Grow the painted bounding box by the pixel rectangle [x0, x1) x [y0, y1).
	fn mark(&mut self, x0: usize, y0: usize, x1: usize, y1: usize) {
		self.dirty = Some(match self.dirty {
			Some((dx0, dy0, dx1, dy1)) => (dx0.min(x0), dy0.min(y0), dx1.max(x1), dy1.max(y1)),
			None => (x0, y0, x1, y1),
		});
	}

	// Grow the painted bounding box by one cell.
	fn mark_cell(&mut self, col: usize, row: usize) {
		self.mark(col * CELL_W, row * CELL_H, (col + 1) * CELL_W, (row + 1) * CELL_H);
	}

	// Swap in a new display backend, copying the existing pixels into its backing (clamped to
	// the overlapping area) so the takeover never clears the screen. The resolution may change;
	// the renderer then targets the new surface (the caller re-sizes the model and presents).
	fn handoff(&mut self, next: Box<dyn Surface>) {
		let w = self.surface.width().min(next.width());
		let h = self.surface.height().min(next.height());
		for y in 0..h {
			for x in 0..w {
				next.put_pixel(x, y, self.surface.read_pixel(x, y));
			}
		}
		self.surface = next;
	}

	// Fill the whole physical framebuffer with the model's default background - used when
	// the grid shrinks so the area now outside it is not left with stale pixels.
	fn clear_screen(&mut self, screen: &Screen) {
		let bg = screen.default_bg();
		self.surface.fill(self.surface.pack(bg.0, bg.1, bg.2));
		let (w, h) = (self.surface.width(), self.surface.height());
		self.mark(0, 0, w, h);
	}

	// Paint one cell from the grid to the framebuffer.
	fn draw_cell(&self, screen: &Screen, col: usize, row: usize) {
		self.draw_cell_at(screen, col, row, screen.display_cell(col, row));
	}

	// Paint a given cell value at (col, row) - used by the live flush (reading the grid)
	// and the scrollback view flush (reading the scrollback ring).
	fn draw_cell_at(&self, screen: &Screen, col: usize, row: usize, cell: Cell) {
		let (fg, bg) = self.cell_colors(screen, &cell);
		self.blit_cell(col, row, cell.glyph, fg, bg, cell.underline);
	}

	// Blit one glyph cell to the framebuffer in already-resolved colours.
	fn blit_cell(&self, col: usize, row: usize, glyph: u32, fg: u32, bg: u32, underline: bool) {
		let bitmap = glyph_bitmap(glyph);
		let x0 = col * CELL_W;
		let y0 = row * CELL_H;
		for gy in 0..FONT_H {
			let bits = bitmap[gy];
			for gx in 0..FONT_W {
				let color = if bits & (0x80 >> gx) != 0 { fg } else { bg };
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
		let cols = screen.cols();
		for op in &scrolls {
			if op.down {
				self.surface.scroll_pixels_down(op.top, op.bot, op.n);
			} else {
				self.surface.scroll_pixels_up(op.top, op.bot, op.n);
			}
			// The whole band's pixels moved, so the present must carry all of it.
			self.mark(0, op.top * CELL_H, cols * CELL_W, (op.bot + 1) * CELL_H);
		}
		if let Some((c, r)) = ghost {
			screen.set_dirty(c, r);
		}
		for row in 0..screen.rows() {
			for col in 0..screen.cols() {
				if screen.dirty_take(col, row) {
					self.draw_cell(screen, col, row);
					self.mark_cell(col, row);
				}
			}
		}
		if screen.cursor_visible() && screen.cursor_col() < screen.cols() && screen.cursor_row() < screen.rows() {
			self.draw_caret(screen, screen.cursor_col(), screen.cursor_row());
			self.mark_cell(screen.cursor_col(), screen.cursor_row());
			self.last_caret = Some((screen.cursor_col(), screen.cursor_row()));
		} else {
			if let Some((c, r)) = self.last_caret {
				self.mark_cell(c, r);
			}
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
		self.mark(0, 0, screen.cols() * CELL_W, screen.rows() * CELL_H);
		self.last_caret = None;
	}

	// Toggle the caret's blink phase. `last_caret` doubles as the phase: Some = the caret
	// is painted (erase it by repainting its cell), None = it is not (draw it at the
	// cursor). Inert while scrolled back or while the cursor is hidden (?25l).
	fn blink_caret(&mut self, screen: &Screen) -> bool {
		if screen.view_offset() > 0 {
			return false;
		}
		if let Some((c, r)) = self.last_caret.take() {
			self.draw_cell(screen, c, r);
			self.mark_cell(c, r);
			return true;
		}
		if screen.cursor_visible() && screen.cursor_col() < screen.cols() && screen.cursor_row() < screen.rows() {
			self.draw_caret(screen, screen.cursor_col(), screen.cursor_row());
			self.mark_cell(screen.cursor_col(), screen.cursor_row());
			self.last_caret = Some((screen.cursor_col(), screen.cursor_row()));
			return true;
		}
		false
	}

	// Resolve a cell's logical colours to packed (fg, bg) framebuffer pixels: bold brightens
	// the ANSI base (0-7 -> 8-15), then reverse swaps fg and bg. This is the L2->L3 colour
	// fold done at draw time (it used to be baked into the cell by `recompute_colors`).
	fn cell_colors(&self, screen: &Screen, c: &Cell) -> (u32, u32) {
		let fg = self.resolve(screen, c.fg, screen.default_fg(), c.bold);
		let bg = self.resolve(screen, c.bg, screen.default_bg(), c.bold);
		if c.reverse { (bg, fg) } else { (fg, bg) }
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
				let step = |c: u8| -> u8 { if c == 0 { 0 } else { 55 + c * 40 } };
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
