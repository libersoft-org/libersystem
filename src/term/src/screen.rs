// The grid model (L2): the cell grid (primary + alternate screen), the cursor and its
// saved copy, the scroll region, the current SGR state and the logical colour model
// (palette + default fg/bg as RGB), the output escape-parser state, and the scrollback
// ring. It holds no pixels and no framebuffer address - a renderer packs its logical
// colours and draws its cells onto a surface, and a non-graphical consumer (`TextSink`)
// reads the same model to serialize it to text.

use alloc::vec::Vec;

// Per-VT scrollback: rows that scroll off the top of the primary screen are kept in a
// fixed ring so the user can page back through them (Shift+PageUp / PageDown). 100 rows
// ~= two screenfuls; the ring is allocated once per VT (deterministic memory: at the
// 4-VT cap this plus the cell grids stays within the rt 1 MB heap).
const SCROLLBACK_ROWS: usize = 1000;

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

// One screen cell: a glyph (a Unicode codepoint the renderer resolves to a font bitmap)
// plus its resolved foreground/background colours and an
// underline flag. The screen is a grid of these (`primary`, plus `alt` for the
// alternate screen); rendering reads the grid, so escape sequences and scrolling are
// pure grid edits and the renderer repaints only the cells that changed (damage tracking
// + double buffering).
#[derive(Clone, Copy, PartialEq)]
pub struct Cell {
	pub glyph: u32,
	pub fg: Color,
	pub bg: Color,
	pub bold: bool,
	pub underline: bool,
	pub reverse: bool,
}

// An SGR colour: the terminal default, a palette index (0-15 the ANSI base, 16-255 the
// xterm 256-colour cube + grayscale), or a 24-bit truecolour RGB. The renderer resolves
// it to a packed framebuffer pixel.
#[derive(Clone, Copy, PartialEq)]
pub enum Color {
	Default,
	Idx(u8),
	Rgb(u8, u8, u8),
}

// The caret shape selected by DECSCUSR (CSI Ps SP q): a steady underline by default, a
// block, or a vertical bar. The blink flag is recorded but the caret is drawn solid (a
// self-driven blink timer would keep the cooperative boot driver from settling - the
// same reason the M35c blink was dropped).
#[derive(Clone, Copy, PartialEq)]
pub enum CursorShape {
	Block,
	Underline,
	Bar,
}

// A scroll the parser performed on the grid this frame: rows [top, bot] moved by n cells,
// up (the default) or down. The renderer replays it as one bulk framebuffer pixel copy
// instead of re-blitting every glyph, then its dirty walk repaints only the vacated rows.
#[derive(Clone, Copy)]
pub struct ScrollOp {
	pub top: usize,
	pub bot: usize,
	pub n: usize,
	pub down: bool,
}

// The grid model (L2): the cell grid (primary + alternate screen), the cursor and its
// saved copy, the scroll region, the current SGR state and the logical colour model
// (palette + default fg/bg as RGB), the output escape-parser state, and the scrollback
// ring. It holds no pixels and no framebuffer address - the renderer packs its logical
// colours and draws its cells onto the surface. So a non-graphical consumer (a "screen as
// text" snapshot, ssh/telnet) can read this model without any rendering.
pub struct Screen {
	cols: usize,
	rows: usize,
	col: usize,
	row: usize,
	saved_col: usize,
	saved_row: usize,
	scroll_top: usize,
	scroll_bottom: usize,
	default_fg: (u8, u8, u8),
	default_bg: (u8, u8, u8),
	palette: [(u8, u8, u8); 16],
	fg_color: Color,
	bg_color: Color,
	bold: bool,
	underline: bool,
	reverse: bool,
	saved_fg_color: Color,
	saved_bg_color: Color,
	saved_bold: bool,
	saved_underline: bool,
	saved_reverse: bool,
	cursor_visible: bool,
	cursor_shape: CursorShape,
	cursor_blink: bool,
	bell: bool,
	osc: [u8; 256],
	osc_len: usize,
	// Pending tty mode changes requested by the program via ESC[?9001h/l (raw) and
	// ESC[?9002h/l (echo); drained by the service into this VT's line discipline.
	tty_raw_req: Option<bool>,
	tty_echo_req: Option<bool>,
	// Mouse tracking the foreground program enabled (DEC private modes): 0 off, 1 normal
	// (?1000, button press/release), 2 button-event (?1002, + drag), 3 any-event (?1003, +
	// motion). The console reads this to decide whether to deliver pointer events to the
	// program as mouse reports or drive its own selection / scrollback.
	mouse_mode: u8,
	// Whether the program asked for SGR-encoded mouse reports (?1006: ESC[<b;x;yM/m).
	mouse_sgr: bool,
	// Bracketed paste (?2004): the console wraps a paste in ESC[200~ .. ESC[201~.
	bracketed_paste: bool,
	// A clipboard write the program requested via OSC 52 (decoded to plain text); drained
	// by the console into the clipboard it holds.
	clipboard_set: Option<Vec<u8>>,
	// The current mouse selection as inclusive (anchor row, anchor col, end row, end col)
	// in global-row coordinates (scrollback rows first, then the live screen), or None.
	// The renderer reverses the selected cells; `selection_text` extracts their glyphs.
	selection: Option<(usize, usize, usize, usize)>,
	esc_state: u8,
	csi_private: u8,
	params: [u16; 16],
	nparams: usize,
	utf8_acc: u32,
	utf8_rem: u8,
	primary: Vec<Cell>,
	alt: Vec<Cell>,
	alt_active: bool,
	dirty: Vec<bool>,
	// Per-row soft-wrap flag (parallel to the live grid rows): wrap[r] is true when row r
	// ended by auto-wrapping into row r+1 (a glyph overflowed its last column), so the two
	// rows are one logical line. A text consumer joins soft-wrapped rows and breaks only on
	// hard newlines. It shifts with the grid on a scroll and is captured into `sb_wrap` when
	// a row scrolls into the scrollback ring.
	wrap: Vec<bool>,
	scrollback: Vec<Cell>,
	// Per-row soft-wrap flag for the scrollback ring (parallel to `scrollback` rows).
	sb_wrap: Vec<bool>,
	sb_cap: usize,
	sb_head: usize,
	sb_len: usize,
	view_offset: usize,
	// Grid scrolls performed by the parser since the last flush; the renderer replays them
	// as bulk framebuffer pixel copies, then drains this. (L2 records geometry only - no
	// pixels.)
	scrolls: Vec<ScrollOp>,
}

impl Screen {
	pub fn new(cols: usize, rows: usize) -> Screen {
		let blank = Cell { glyph: b' ' as u32, fg: Color::Default, bg: Color::Default, bold: false, underline: false, reverse: false };
		Screen { cols, rows, col: 0, row: 0, saved_col: 0, saved_row: 0, scroll_top: 0, scroll_bottom: rows.saturating_sub(1), default_fg: FG, default_bg: BG, palette: ANSI_PALETTE, fg_color: Color::Default, bg_color: Color::Default, bold: false, underline: false, reverse: false, saved_fg_color: Color::Default, saved_bg_color: Color::Default, saved_bold: false, saved_underline: false, saved_reverse: false, cursor_visible: true, cursor_shape: CursorShape::Underline, cursor_blink: false, bell: false, osc: [0; 256], osc_len: 0, tty_raw_req: None, tty_echo_req: None, mouse_mode: 0, mouse_sgr: false, bracketed_paste: false, clipboard_set: None, selection: None, esc_state: 0, csi_private: 0, params: [0; 16], nparams: 0, utf8_acc: 0, utf8_rem: 0, primary: alloc::vec![blank; cols * rows], alt: alloc::vec![blank; cols * rows], alt_active: false, dirty: alloc::vec![true; cols * rows], wrap: alloc::vec![false; rows], scrollback: alloc::vec![blank; SCROLLBACK_ROWS * cols], sb_wrap: alloc::vec![false; SCROLLBACK_ROWS], sb_cap: SCROLLBACK_ROWS, sb_head: 0, sb_len: 0, view_offset: 0, scrolls: Vec::new() }
	}

	// The active cell buffer: the alternate screen while it is up, else the primary.
	fn cells(&self) -> &[Cell] {
		if self.alt_active { &self.alt } else { &self.primary }
	}

	// A snapshot of the live cell at (col, row): the renderer's read of the grid model.
	pub fn cell(&self, col: usize, row: usize) -> Cell {
		self.cells()[row * self.cols + col]
	}

	// The live cell at (col, row) with the mouse selection highlight applied (its colours
	// reversed when it falls in the selection) - the renderer's read for the live screen
	// (view offset 0); `view_cell` does the same for the scrollback view.
	pub fn display_cell(&self, col: usize, row: usize) -> Cell {
		let mut c = self.cell(col, row);
		if self.is_selected(self.sb_len + row, col) {
			c.reverse = !c.reverse;
		}
		c
	}

	// A blank cell in the current background (so erase/scroll paint the SGR bg).
	pub fn blank(&self) -> Cell {
		Cell { glyph: b' ' as u32, fg: self.fg_color, bg: self.bg_color, bold: self.bold, underline: false, reverse: self.reverse }
	}

	// The logical grid geometry: a renderer reads it to walk the cells.
	pub fn cols(&self) -> usize {
		self.cols
	}

	pub fn rows(&self) -> usize {
		self.rows
	}

	// The cursor's live position and how it is drawn - the renderer reads these to paint
	// the caret (the model never draws it).
	pub fn cursor_col(&self) -> usize {
		self.col
	}

	pub fn cursor_row(&self) -> usize {
		self.row
	}

	pub fn cursor_visible(&self) -> bool {
		self.cursor_visible
	}

	pub fn cursor_shape(&self) -> CursorShape {
		self.cursor_shape
	}

	// The current scrollback view offset (0 == live screen): the renderer switches to a
	// scrollback repaint while it is non-zero.
	pub fn view_offset(&self) -> usize {
		self.view_offset
	}

	// The logical colour model the renderer folds to pixels: the terminal default fg/bg and
	// one entry of the (program-settable) 16-colour palette.
	pub fn default_fg(&self) -> (u8, u8, u8) {
		self.default_fg
	}

	pub fn default_bg(&self) -> (u8, u8, u8) {
		self.default_bg
	}

	pub fn palette_color(&self, i: usize) -> (u8, u8, u8) {
		self.palette[i]
	}

	pub fn mark_all_dirty(&mut self) {
		for d in self.dirty.iter_mut() {
			*d = true;
		}
	}

	// Read and clear one cell's dirty mark - the renderer consuming the diff as it paints.
	pub fn dirty_take(&mut self, col: usize, row: usize) -> bool {
		let idx = row * self.cols + col;
		let was = self.dirty[idx];
		self.dirty[idx] = false;
		was
	}

	// Mark one cell dirty: the renderer flags a cell it must repaint (e.g. to erase a caret).
	pub fn set_dirty(&mut self, col: usize, row: usize) {
		if col < self.cols && row < self.rows {
			self.dirty[row * self.cols + col] = true;
		}
	}

	// Drain the grid scrolls recorded since the last flush (this frame's scroll diff).
	pub fn take_scrolls(&mut self) -> Vec<ScrollOp> {
		core::mem::take(&mut self.scrolls)
	}

	// Drain a pending tty raw / echo mode change requested by the program (ESC[?9001/9002
	// h/l); the service applies it to this VT's line discipline.
	pub fn take_tty_raw_req(&mut self) -> Option<bool> {
		self.tty_raw_req.take()
	}

	pub fn take_tty_echo_req(&mut self) -> Option<bool> {
		self.tty_echo_req.take()
	}

	// Whether the foreground program enabled mouse tracking (DEC ?1000/?1002/?1003), and
	// at what granularity - the console reads these to decide whether to deliver pointer
	// events to the program as mouse reports or drive its own selection / scrollback.
	pub fn mouse_tracking(&self) -> bool {
		self.mouse_mode != 0
	}

	// Whether the program asked to be told about drag motion (?1002 button-event or ?1003
	// any-event), and whether it wants motion with no button held too (?1003).
	pub fn mouse_report_motion(&self) -> bool {
		self.mouse_mode >= 2
	}

	pub fn mouse_any_motion(&self) -> bool {
		self.mouse_mode == 3
	}

	// Whether the program asked for SGR-encoded reports (?1006).
	pub fn mouse_sgr(&self) -> bool {
		self.mouse_sgr
	}

	// Whether bracketed paste (?2004) is on, so the console wraps a paste in ESC[200~..201~.
	pub fn bracketed_paste(&self) -> bool {
		self.bracketed_paste
	}

	// Drain a clipboard write the program requested via OSC 52 (decoded plain text); the
	// console stores it in the clipboard it holds.
	pub fn take_clipboard_set(&mut self) -> Option<Vec<u8>> {
		self.clipboard_set.take()
	}

	// Whether a mouse selection is active (so the console copies it on release).
	pub fn has_selection(&self) -> bool {
		self.selection.is_some()
	}

	// Begin a mouse selection at viewport (col, row) for the current scroll offset: anchor
	// and end both start on the global cell the viewport position maps to.
	pub fn selection_begin(&mut self, col: usize, row: usize) {
		let old = self.selection;
		let g = self.view_global_row(row);
		let c = col.min(self.cols.saturating_sub(1));
		self.selection = Some((g, c, g, c));
		self.dirty_selection_rows(old);
	}

	// Extend the active selection's end to viewport (col, row) (a drag); a no-op with no
	// selection in progress.
	pub fn selection_extend(&mut self, col: usize, row: usize) {
		if let Some((ag, ac, _, _)) = self.selection {
			let old = self.selection;
			let g = self.view_global_row(row);
			let c = col.min(self.cols.saturating_sub(1));
			self.selection = Some((ag, ac, g, c));
			self.dirty_selection_rows(old);
		}
	}

	// Clear the selection highlight; a no-op (no repaint) when nothing was selected.
	pub fn selection_clear(&mut self) {
		if self.selection.is_some() {
			let old = self.selection.take();
			self.dirty_selection_rows(old);
		}
	}

	// Mark dirty every viewport row the old or the current selection touches, so a
	// drag repaints only the rows whose highlight can change - not the whole grid
	// (a full-grid repaint per pointer event is what made selection feel laggy).
	fn dirty_selection_rows(&mut self, old: Option<(usize, usize, usize, usize)>) {
		let base = self.view_global_row(0);
		for sel in [old, self.selection].into_iter().flatten() {
			let (ag, _, eg, _) = sel;
			let (lo, hi) = (ag.min(eg), ag.max(eg));
			for row in 0..self.rows {
				let g = base + row;
				if g >= lo && g <= hi {
					for col in 0..self.cols {
						self.dirty[row * self.cols + col] = true;
					}
				}
			}
		}
	}

	// The selected text as the console copies it to the clipboard: the selected glyphs of
	// each global row in reading order, trailing spaces trimmed per row, rows joined by a
	// newline. Empty when nothing is selected.
	pub fn selection_text(&self) -> Vec<u8> {
		let (lo, hi) = match self.sel_bounds() {
			Some(b) => b,
			None => return Vec::new(),
		};
		let (lg, lc) = lo;
		let (hg, hc) = hi;
		let last_col = self.cols.saturating_sub(1);
		let mut out: Vec<u8> = Vec::new();
		let mut g = lg;
		while g <= hg && g < self.total_logical_rows() {
			let start_col = if g == lg { lc } else { 0 };
			let end_col = if g == hg { hc.min(last_col) } else { last_col };
			let mut line: Vec<u32> = Vec::new();
			let mut c = start_col;
			while c <= end_col {
				line.push(self.global_glyph(c, g));
				c += 1;
			}
			while line.last() == Some(&(b' ' as u32)) {
				line.pop();
			}
			for &cp in &line {
				push_utf8(&mut out, cp);
			}
			if g != hg {
				out.push(b'\n');
			}
			g += 1;
		}
		out
	}

	// The global row (scrollback rows first, then the live screen) a viewport row maps to
	// at the current scroll offset - mirrors `view_cell`'s mapping.
	fn view_global_row(&self, row: usize) -> usize {
		(self.sb_len - self.view_offset) + row
	}

	// The selection's ordered ((row, col) low, high) endpoints in reading order, or None.
	fn sel_bounds(&self) -> Option<((usize, usize), (usize, usize))> {
		let (ag, ac, eg, ec) = self.selection?;
		let a = (ag, ac);
		let e = (eg, ec);
		Some(if a <= e { (a, e) } else { (e, a) })
	}

	// Whether the cell at column `col` of global row `g` falls within the selection.
	fn is_selected(&self, g: usize, col: usize) -> bool {
		match self.sel_bounds() {
			Some((lo, hi)) => (g, col) >= lo && (g, col) <= hi,
			None => false,
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

	pub fn clear(&mut self) {
		let blank = self.blank();
		{
			let buf = if self.alt_active { &mut self.alt } else { &mut self.primary };
			for c in buf.iter_mut() {
				*c = blank;
			}
		}
		for w in self.wrap.iter_mut() {
			*w = false;
		}
		self.mark_all_dirty();
		self.col = 0;
		self.row = 0;
	}

	// Resize the logical grid to new_cols x new_rows (clamped to what the physical
	// framebuffer can show), reflowing the screen: the overlapping rectangle of cells is
	// preserved (bottom-anchored so the cursor line stays on screen), the alternate screen
	// and scrollback are reset, and the now-unused area is cleared. This is the local
	// stand-in for a virtio-gpu mode-set (M44); the same path runs on a real resolution
	// change once that driver lands.
	pub fn resize(&mut self, new_cols: usize, new_rows: usize, max_cols: usize, max_rows: usize) -> bool {
		let new_cols = new_cols.clamp(1, max_cols);
		let new_rows = new_rows.clamp(1, max_rows);
		if new_cols == self.cols && new_rows == self.rows {
			return false;
		}
		let blank = Cell { glyph: b' ' as u32, fg: Color::Default, bg: Color::Default, bold: false, underline: false, reverse: false };
		let mut new_primary = alloc::vec![blank; new_cols * new_rows];
		let copy_rows = self.rows.min(new_rows);
		let copy_cols = self.cols.min(new_cols);
		let src_row0 = self.rows - copy_rows; // keep the bottom rows
		let dst_row0 = new_rows - copy_rows;
		for r in 0..copy_rows {
			for c in 0..copy_cols {
				new_primary[(dst_row0 + r) * new_cols + c] = self.primary[(src_row0 + r) * self.cols + c];
			}
		}
		// Track the cursor with the content (bottom-anchored), clamped into the new grid.
		let new_row = if self.row >= src_row0 { dst_row0 + (self.row - src_row0) } else { 0 };
		self.col = self.col.min(new_cols - 1);
		self.row = new_row.min(new_rows - 1);
		self.primary = new_primary;
		self.alt = alloc::vec![blank; new_cols * new_rows];
		self.dirty = alloc::vec![true; new_cols * new_rows];
		self.wrap = alloc::vec![false; new_rows];
		// Scrollback is reset on a resize (its fixed width changed)
		self.scrollback = alloc::vec![blank; SCROLLBACK_ROWS * new_cols];
		self.sb_wrap = alloc::vec![false; SCROLLBACK_ROWS];
		self.sb_cap = SCROLLBACK_ROWS;
		self.sb_head = 0;
		self.sb_len = 0;
		self.view_offset = 0;
		self.selection = None;
		self.cols = new_cols;
		self.rows = new_rows;
		self.scroll_top = 0;
		self.scroll_bottom = new_rows - 1;
		self.alt_active = false;
		true
	}

	// Copy primary screen row `screen_row` into the scrollback ring (oldest first); the
	// oldest row is dropped once the ring is full. The row's soft-wrap flag travels with it.
	fn push_scrollback(&mut self, screen_row: usize) {
		if self.sb_cap == 0 {
			return;
		}
		let cols = self.cols;
		let ring = (self.sb_head + self.sb_len) % self.sb_cap;
		let dst = ring * cols;
		let src = screen_row * cols;
		for col in 0..cols {
			self.scrollback[dst + col] = self.primary[src + col];
		}
		self.sb_wrap[ring] = self.wrap[screen_row];
		if self.sb_len < self.sb_cap {
			self.sb_len += 1;
		} else {
			self.sb_head = (self.sb_head + 1) % self.sb_cap;
		}
	}

	// The cell shown at viewport (col, row) for the current scrollback view offset: a
	// scrollback row while the viewport reaches above the live screen, else a live cell.
	// The mouse selection highlight is applied (reversed colours) over both.
	pub fn view_cell(&self, col: usize, row: usize) -> Cell {
		let g = (self.sb_len - self.view_offset) + row;
		let mut cell = if g < self.sb_len {
			let ring = (self.sb_head + g) % self.sb_cap;
			self.scrollback[ring * self.cols + col]
		} else {
			self.primary[(g - self.sb_len) * self.cols + col]
		};
		if self.is_selected(g, col) {
			cell.reverse = !cell.reverse;
		}
		cell
	}

	// Page the scrollback view up (toward older lines) by one screen. A no-op on the
	// alternate screen or with no history.
	pub fn scroll_view_up(&mut self) {
		let page = self.rows.saturating_sub(1).max(1);
		self.scroll_view_up_by(page);
	}

	// Page the scrollback view down (toward the live screen) by one screen; on reaching the
	// live screen the whole grid is marked dirty so the next flush repaints it.
	pub fn scroll_view_down(&mut self) {
		let page = self.rows.saturating_sub(1).max(1);
		self.scroll_view_down_by(page);
	}

	// Move the scrollback view up (toward older lines) by `lines` rows - the wheel's
	// finer-grained scroll. A no-op on the alternate screen or with no history.
	pub fn scroll_view_up_by(&mut self, lines: usize) {
		if self.alt_active || self.sb_len == 0 {
			return;
		}
		self.view_offset = (self.view_offset + lines).min(self.sb_len);
	}

	// Move the scrollback view down (toward the live screen) by `lines` rows; on reaching
	// the live screen the whole grid is marked dirty so the next flush repaints it.
	pub fn scroll_view_down_by(&mut self, lines: usize) {
		let new = self.view_offset.saturating_sub(lines);
		if new == 0 && self.view_offset > 0 {
			self.mark_all_dirty();
		}
		self.view_offset = new;
	}

	// Snap back to the live screen; returns whether the view actually moved.
	pub fn snap_live(&mut self) -> bool {
		if self.view_offset > 0 {
			self.view_offset = 0;
			self.mark_all_dirty();
			true
		} else {
			false
		}
	}

	// Scroll the rows [top, bot] up by n, filling the freed bottom rows with blanks. A pure
	// grid edit: the cells, their dirty marks and their soft-wrap flags shift together, the
	// vacated bottom rows are marked dirty (and not soft-wrapped), and the scroll is recorded
	// so the renderer can move the framebuffer pixels in one bulk copy (the fast path)
	// instead of re-blitting the whole band.
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
		// Shift the dirty marks with the grid so a cell edited earlier this batch but not yet
		// painted stays dirty at its new row (the bulk pixel copy carries stale pixels there
		// that the dirty walk then overpaints); the vacated bottom rows become dirty.
		for row in top..=bot {
			let src = row + n;
			for col in 0..cols {
				self.dirty[row * cols + col] = if src <= bot { self.dirty[src * cols + col] } else { true };
			}
		}
		// Soft-wrap flags follow the same shift; the vacated rows are no longer continued.
		for row in top..=bot {
			let src = row + n;
			self.wrap[row] = if src <= bot { self.wrap[src] } else { false };
		}
		self.scrolls.push(ScrollOp { top, bot, n, down: false });
	}

	// Scroll the rows [top, bot] down by n, filling the freed top rows with blanks - the
	// downward counterpart of region_up (reverse index / insert line).
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
		for row in (top..=bot).rev() {
			for col in 0..cols {
				self.dirty[row * cols + col] = if row >= top + n { self.dirty[(row - n) * cols + col] } else { true };
			}
		}
		for row in (top..=bot).rev() {
			self.wrap[row] = if row >= top + n { self.wrap[row - n] } else { false };
		}
		self.scrolls.push(ScrollOp { top, bot, n, down: true });
	}

	fn scroll_up(&mut self, n: usize) {
		// Lines that scroll off the top of the full primary screen go to scrollback (not
		// when a program set a scroll region, nor on the alternate screen). A held scroll
		// view is nudged up by the same amount so its content stays anchored.
		if !self.alt_active && self.scroll_top == 0 {
			let n = n.min(self.rows);
			for i in 0..n {
				self.push_scrollback(i);
			}
			if self.view_offset > 0 {
				self.view_offset = (self.view_offset + n).min(self.sb_len);
			}
		}
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

	fn put_glyph(&mut self, glyph: u32) {
		if self.col >= self.cols {
			// The previous glyph filled the last column: this row soft-wraps into the next.
			self.wrap[self.row] = true;
			self.col = 0;
			self.line_feed();
		}
		let cell = Cell { glyph, fg: self.fg_color, bg: self.bg_color, bold: self.bold, underline: self.underline, reverse: self.reverse };
		self.set_cell(self.col, self.row, cell);
		self.col += 1;
	}

	// Render a decoded Unicode codepoint: the cell records the codepoint itself, and the
	// renderer resolves it to a font glyph (one the font lacks draws as '?').
	fn put_codepoint(&mut self, cp: u32) {
		self.put_glyph(cp);
	}

	// Begin a UTF-8 multi-byte sequence from its lead byte, recording how many
	// continuation bytes follow. A stray continuation or invalid lead renders U+FFFD.
	fn begin_utf8(&mut self, byte: u8) {
		if byte & 0xe0 == 0xc0 {
			self.utf8_acc = (byte & 0x1f) as u32;
			self.utf8_rem = 1;
		} else if byte & 0xf0 == 0xe0 {
			self.utf8_acc = (byte & 0x0f) as u32;
			self.utf8_rem = 2;
		} else if byte & 0xf8 == 0xf0 {
			self.utf8_acc = (byte & 0x07) as u32;
			self.utf8_rem = 3;
		} else {
			self.put_codepoint(0xfffd);
		}
	}

	// The output parser entry point: feed one byte from the client's output stream.
	pub fn put_byte(&mut self, byte: u8) {
		// Mid UTF-8 sequence: fold in continuation bytes until the codepoint completes.
		if self.utf8_rem > 0 {
			if byte & 0xc0 == 0x80 {
				self.utf8_acc = (self.utf8_acc << 6) | (byte & 0x3f) as u32;
				self.utf8_rem -= 1;
				if self.utf8_rem == 0 {
					self.put_codepoint(self.utf8_acc);
				}
				return;
			}
			// A malformed sequence: drop it and reinterpret this byte below.
			self.utf8_rem = 0;
		}
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
			0x07 => self.bell = true, // BEL: a visual flash, rendered by the console
			0x20..=0x7e => self.put_codepoint(byte as u32),
			_ if byte >= 0x80 => self.begin_utf8(byte),
			_ => {} // other C0 control bytes: ignored
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
			b']' => {
				self.esc_state = 3;
				self.osc_len = 0;
			}
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

	// Accumulate an OSC string until BEL (0x07) or the start of an ST (ESC \), then act
	// on it. Bytes past the buffer are dropped (only short control strings - a palette
	// set - are acted on; a long title is ignored anyway).
	fn osc_byte(&mut self, byte: u8) {
		if byte == 0x07 {
			self.osc_dispatch();
			self.esc_state = 0;
		} else if byte == 0x1b {
			// ESC: the start of a String Terminator (ESC \); act now, then consume the
			// following byte as a normal escape (the trailing '\' is a harmless no-op).
			self.osc_dispatch();
			self.esc_state = 1;
		} else if self.osc_len < self.osc.len() {
			self.osc[self.osc_len] = byte;
			self.osc_len += 1;
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
			if v == 0 { default } else { v }
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
			b'q' => self.set_cursor_style(self.param(0, 1)),
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
		self.saved_fg_color = self.fg_color;
		self.saved_bg_color = self.bg_color;
		self.saved_bold = self.bold;
		self.saved_underline = self.underline;
		self.saved_reverse = self.reverse;
	}

	fn restore_cursor(&mut self) {
		self.col = self.saved_col.min(self.cols.saturating_sub(1));
		self.row = self.saved_row.min(self.rows.saturating_sub(1));
		self.fg_color = self.saved_fg_color;
		self.bg_color = self.saved_bg_color;
		self.bold = self.saved_bold;
		self.underline = self.saved_underline;
		self.reverse = self.saved_reverse;
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
				9001 => self.tty_raw_req = Some(enable),
				9002 => self.tty_echo_req = Some(enable),
				1000 => self.mouse_mode = if enable { 1 } else { 0 },
				1002 => self.mouse_mode = if enable { 2 } else { 0 },
				1003 => self.mouse_mode = if enable { 3 } else { 0 },
				1006 => self.mouse_sgr = enable,
				2004 => self.bracketed_paste = enable,
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
		self.view_offset = 0;
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
		self.fg_color = Color::Default;
		self.bg_color = Color::Default;
		self.bold = false;
		self.underline = false;
		self.reverse = false;
		self.cursor_visible = true;
		self.scroll_top = 0;
		self.scroll_bottom = self.rows.saturating_sub(1);
		self.alt_active = false;
		self.view_offset = 0;
		self.sb_head = 0;
		self.sb_len = 0;
		self.mouse_mode = 0;
		self.mouse_sgr = false;
		self.bracketed_paste = false;
		self.selection = None;
		self.clear();
	}

	fn apply_sgr(&mut self) {
		let mut i = 0;
		while i <= self.nparams {
			match self.params[i] {
				0 => {
					self.fg_color = Color::Default;
					self.bg_color = Color::Default;
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
				30..=37 => self.fg_color = Color::Idx((self.params[i] - 30) as u8),
				38 => {
					if let Some((c, adv)) = self.parse_ext_color(i) {
						self.fg_color = c;
						i += adv;
					}
				}
				39 => self.fg_color = Color::Default,
				40..=47 => self.bg_color = Color::Idx((self.params[i] - 40) as u8),
				48 => {
					if let Some((c, adv)) = self.parse_ext_color(i) {
						self.bg_color = c;
						i += adv;
					}
				}
				49 => self.bg_color = Color::Default,
				90..=97 => self.fg_color = Color::Idx((self.params[i] - 90 + 8) as u8),
				100..=107 => self.bg_color = Color::Idx((self.params[i] - 100 + 8) as u8),
				_ => {}
			}
			i += 1;
		}
	}

	// Parse an extended-colour selector starting at param `i` (the 38 or 48): the
	// `; 5 ; n` form selects 256-colour index n, the `; 2 ; r ; g ; b` form a 24-bit RGB.
	// Returns the colour and how many extra params it consumed.
	fn parse_ext_color(&self, i: usize) -> Option<(Color, usize)> {
		match self.params.get(i + 1).copied() {
			Some(5) if i + 2 <= self.nparams => Some((Color::Idx(self.params[i + 2] as u8), 2)),
			Some(2) if i + 4 <= self.nparams => {
				let r = self.params[i + 2] as u8;
				let g = self.params[i + 3] as u8;
				let b = self.params[i + 4] as u8;
				Some((Color::Rgb(r, g, b), 4))
			}
			_ => None,
		}
	}

	// DECSCUSR (CSI Ps SP q): select the cursor shape + blink. 0/1 blinking block, 2
	// steady block, 3 blinking underline, 4 steady underline, 5 blinking bar, 6 steady
	// bar. The blink flag is recorded but the caret is drawn solid.
	fn set_cursor_style(&mut self, n: usize) {
		let (shape, blink) = match n {
			0 | 1 => (CursorShape::Block, true),
			2 => (CursorShape::Block, false),
			3 => (CursorShape::Underline, true),
			4 => (CursorShape::Underline, false),
			5 => (CursorShape::Bar, true),
			6 => (CursorShape::Bar, false),
			_ => (self.cursor_shape, self.cursor_blink),
		};
		self.cursor_shape = shape;
		self.cursor_blink = blink;
	}

	// Whether a BEL arrived since the last check, clearing the flag.
	pub fn take_bell(&mut self) -> bool {
		let b = self.bell;
		self.bell = false;
		b
	}

	// Act on a completed OSC string: OSC 4;n;spec sets palette entry n (0-15), OSC
	// 10;spec / 11;spec the default fg / bg. OSC 0/1/2 (title) and 8 (hyperlink) are
	// accepted and ignored - a bare VT console has no title bar or clickable links.
	fn osc_dispatch(&mut self) {
		let len = self.osc_len;
		let semi = match self.osc[..len].iter().position(|&b| b == b';') {
			Some(i) => i,
			None => return,
		};
		let code = parse_dec(&self.osc[..semi]);
		let rest_start = semi + 1;
		match code {
			Some(4) => {
				let (n, color) = {
					let rest = &self.osc[rest_start..len];
					let semi2 = match rest.iter().position(|&b| b == b';') {
						Some(i) => i,
						None => return,
					};
					(parse_dec(&rest[..semi2]), parse_osc_color(&rest[semi2 + 1..]))
				};
				if let (Some(n), Some((r, g, b))) = (n, color) {
					if n < 16 {
						self.palette[n] = (r, g, b);
					}
				}
			}
			Some(10) => {
				if let Some((r, g, b)) = parse_osc_color(&self.osc[rest_start..len]) {
					self.default_fg = (r, g, b);
				}
			}
			Some(11) => {
				if let Some((r, g, b)) = parse_osc_color(&self.osc[rest_start..len]) {
					self.default_bg = (r, g, b);
				}
			}
			Some(52) => {
				// OSC 52 ; Pc ; Pd - set the clipboard. Pd is base64-encoded text (or "?"
				// to query, which needs a write-back path the console owns, not this model).
				let rest = &self.osc[rest_start..len];
				if let Some(semi2) = rest.iter().position(|&b| b == b';') {
					let data = &rest[semi2 + 1..];
					if data != b"?" {
						if let Some(text) = base64_decode(data) {
							self.clipboard_set = Some(text);
						}
					}
				}
			}
			_ => {}
		}
	}

	// The number of logical rows a text consumer walks: the scrollback history followed by
	// the live screen.
	pub(crate) fn total_logical_rows(&self) -> usize {
		self.sb_len + self.rows
	}

	// The glyph (Unicode codepoint) at column `col` of global row `g` (scrollback rows
	// first, then the live primary screen) - a text consumer's read of the grid, mirroring
	// `view_cell`.
	pub(crate) fn global_glyph(&self, col: usize, g: usize) -> u32 {
		if g < self.sb_len {
			let ring = (self.sb_head + g) % self.sb_cap;
			self.scrollback[ring * self.cols + col].glyph
		} else {
			self.primary[(g - self.sb_len) * self.cols + col].glyph
		}
	}

	// Whether global row `g` soft-wraps into the next row (so the two are one logical line).
	pub(crate) fn global_wrap(&self, g: usize) -> bool {
		if g < self.sb_len {
			let ring = (self.sb_head + g) % self.sb_cap;
			self.sb_wrap[ring]
		} else {
			self.wrap[g - self.sb_len]
		}
	}
}

// Append one Unicode codepoint to `out` as UTF-8 (an invalid codepoint encodes as '?').
// The text consumers (`TextSink`, `selection_text`) serialize grid glyphs through this,
// so what a program printed as UTF-8 reads back as the same bytes.
pub(crate) fn push_utf8(out: &mut Vec<u8>, cp: u32) {
	let c = char::from_u32(cp).unwrap_or('?');
	let mut buf = [0u8; 4];
	out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
}

// Parse a decimal byte string to usize, or None if empty / non-numeric.
fn parse_dec(s: &[u8]) -> Option<usize> {
	if s.is_empty() {
		return None;
	}
	let mut v: usize = 0;
	for &b in s {
		if !b.is_ascii_digit() {
			return None;
		}
		v = v.checked_mul(10)?.checked_add((b - b'0') as usize)?;
	}
	Some(v)
}

// Decode a standard-alphabet base64 byte string (the OSC 52 clipboard payload) to its
// bytes; `=` ends the data and any non-alphabet byte fails the decode.
fn base64_decode(s: &[u8]) -> Option<Vec<u8>> {
	fn sextet(b: u8) -> Option<u32> {
		match b {
			b'A'..=b'Z' => Some((b - b'A') as u32),
			b'a'..=b'z' => Some((b - b'a' + 26) as u32),
			b'0'..=b'9' => Some((b - b'0' + 52) as u32),
			b'+' => Some(62),
			b'/' => Some(63),
			_ => None,
		}
	}
	let mut out: Vec<u8> = Vec::new();
	let mut acc: u32 = 0;
	let mut bits: u32 = 0;
	for &b in s {
		if b == b'=' {
			break;
		}
		acc = (acc << 6) | sextet(b)?;
		bits += 6;
		if bits >= 8 {
			bits -= 8;
			out.push((acc >> bits) as u8);
		}
	}
	Some(out)
}

fn hex_digit(b: u8) -> Option<u8> {
	match b {
		b'0'..=b'9' => Some(b - b'0'),
		b'a'..=b'f' => Some(b - b'a' + 10),
		b'A'..=b'F' => Some(b - b'A' + 10),
		_ => None,
	}
}

fn hex2(s: &[u8]) -> Option<u8> {
	Some(hex_digit(s[0])? * 16 + hex_digit(s[1])?)
}

// Parse 1-4 hex digits and scale to 8 bits (xterm: "f" -> 0xff, "ff" -> 0xff, etc).
fn scale_hex(s: &[u8]) -> Option<u8> {
	if s.is_empty() || s.len() > 4 {
		return None;
	}
	let mut v: u32 = 0;
	for &b in s {
		v = (v << 4) | hex_digit(b)? as u32;
	}
	let scaled = match s.len() {
		1 => (v << 4) | v,
		2 => v,
		3 => v >> 4,
		_ => v >> 8,
	};
	Some(scaled as u8)
}

// Parse an X11 / xterm OSC colour spec to (r, g, b): "rgb:RR/GG/BB" (1-4 hex digits per
// component) or "#RGB" / "#RRGGBB".
fn parse_osc_color(s: &[u8]) -> Option<(u8, u8, u8)> {
	if let Some(rest) = s.strip_prefix(b"rgb:") {
		let mut it = rest.split(|&b| b == b'/');
		let r = scale_hex(it.next()?)?;
		let g = scale_hex(it.next()?)?;
		let b = scale_hex(it.next()?)?;
		if it.next().is_some() {
			return None;
		}
		Some((r, g, b))
	} else if let Some(rest) = s.strip_prefix(b"#") {
		match rest.len() {
			3 => Some((hex_digit(rest[0])? * 0x11, hex_digit(rest[1])? * 0x11, hex_digit(rest[2])? * 0x11)),
			6 => Some((hex2(&rest[0..2])?, hex2(&rest[2..4])?, hex2(&rest[4..6])?)),
			_ => None,
		}
	} else {
		None
	}
}
