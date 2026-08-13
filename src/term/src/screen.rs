// The grid model (L2): the cell grid (primary + alternate screen), the cursor and its
// saved copy, the scroll region, the current SGR state and the logical colour model
// (palette + default fg/bg as RGB), the output escape-parser state, and the scrollback
// ring. It holds no pixels and no framebuffer address - a renderer packs its logical
// colours and draws its cells onto a surface, and a non-graphical consumer (`TextSink`)
// reads the same model to serialize it to text.

use alloc::vec::Vec;

// Per-VT scrollback: rows that scroll off the top of the primary screen are kept in a
// fixed ring so the user can page back through them (Shift+PageUp / PageDown). The
// depth is the operator's policy (the `console.scrollback` config key, read at VT
// creation); this is the default when no configuration answers. The ring is allocated
// once per VT (deterministic memory: at the 4-VT cap this plus the cell grids stays
// within the rt 1 MB heap).
pub const SCROLLBACK_ROWS: usize = 1000;

// The ceilings `Screen::new` clamps to. 1024 columns is 8192 pixels at this cell size - past 8K -
// and 512 rows is past 8192 pixels tall, so no real display reaches them; they exist so an
// arithmetic overflow or a configuration typo cannot ask for an allocation the machine cannot make.
pub const MAX_COLS: usize = 1024;
pub const MAX_ROWS: usize = 512;

// WHAT ONE VT MAY COST IN SCROLLBACK, in bytes, and the number that makes the per-VT figure
// answerable rather than emergent.
//
// `SCROLLBACK_ROWS` is 1000 and a `Cell` is 16 bytes, so at 1024x768 (128 columns) the ring alone
// was 1.95 MB and a VT cost 2.15 MB; at 1920x1080, 4.17 MB. VTs are created on demand with no cap,
// so console memory grew with how many the operator opened, in a service whose heap starts at 1 MB.
// The rows an operator configures are now clamped to what fits this budget at the current width -
// so the cost per VT has a ceiling that does not depend on the display's resolution.
pub const MAX_SCROLLBACK_BYTES: usize = 2 * 1024 * 1024;

// How many bytes of unanswered query replies may queue before they are dropped - see `push_reply`.
// What the terminal may owe a program at once.
//
// SIZED FOR ITS LARGEST PRODUCER, which for a while it was not. 256 was chosen for DSR and DA, which
// answer in tens of bytes; `answer_clipboard` then base64-encodes the whole clipboard into the same
// buffer, and `push_reply` refuses the WHOLE addition on overflow - silently, returning nothing to
// the caller. About 185 bytes of plaintext fitted, so a selected paragraph produced no answer at all,
// indistinguishable from the query being ignored.
//
// 8 KiB holds roughly 6 KiB of selection, which is a paragraph rather than a document; past that the
// answer is an empty OSC 52 payload, which is a valid answer meaning "nothing you can have". Silence
// is the one option that is not acceptable, because it is the behaviour the query item was reopened
// to remove.
pub(crate) const MAX_REPLY_BYTES: usize = 8 * 1024;

// How many scroll operations may queue for the renderer before the queue stops being cheaper than
// a full repaint. A frame's honest diff is a handful; anything past a screenful is a VT nobody has
// flushed. See `record_scroll`.
const MAX_PENDING_SCROLLS: usize = 64;

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
// block, or a vertical bar. The blink flag is honoured by `blink_caret`, which the console calls
// on its own timer - the renderer never drives one itself, because a self-driven timer would keep
// the cooperative boot driver from settling.
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
// One screen: its cells and the per-row soft-wrap flags that describe them.
//
// `wrap[r]` is true when row r ended by auto-wrapping into row r+1 - a glyph overflowed its last
// column - so the two rows are one logical line. A text consumer joins soft-wrapped rows and breaks
// only on hard newlines. The flags shift with the grid on a scroll and are captured into `sb_wrap`
// when a row scrolls into the scrollback ring.
//
// The pairing is the point: cells and their line structure describe the same screen, so they belong
// to the same value and cannot be read from different ones.
#[derive(Clone)]
struct ScreenBuffer {
	cells: Vec<Cell>,
	wrap: Vec<bool>,
	// THIS BUFFER'S SAVED CURSOR - DECSC's, and `?1049`'s.
	//
	// Per buffer, the way xterm's `sc[]` is indexed by which screen is showing, and it was one
	// field shared by both. Two consequences. `?1049h` saves the shell's cursor and the full-screen
	// program then issues its own DECSC, which overwrote that save, so `?1049l` returned the shell
	// to wherever the program had been. And a resize reflowed the primary buffer without touching
	// the save at all, so after a width change the shell came back to a cell that was no longer
	// where its prompt was.
	//
	// The LIVE cursor stays shared, which is also xterm: `?47` leaves the cursor where the program
	// left it - the test above this file's `?1049` case pins that deliberately, because a ported
	// program using `?47` saves and restores the cursor itself. Giving each buffer its own live
	// position would have quietly changed that.
	saved: SavedCursor,
}

// What DECSC stores: a position and the SGR state that was in force with it.
#[derive(Clone, Copy)]
struct SavedCursor {
	col: usize,
	row: usize,
	fg_color: Color,
	bg_color: Color,
	bold: bool,
	underline: bool,
	reverse: bool,
}

impl Default for SavedCursor {
	fn default() -> SavedCursor {
		SavedCursor { col: 0, row: 0, fg_color: Color::Default, bg_color: Color::Default, bold: false, underline: false, reverse: false }
	}
}

impl ScreenBuffer {
	fn new(cols: usize, rows: usize, blank: Cell) -> ScreenBuffer {
		ScreenBuffer { cells: alloc::vec![blank; cols * rows], wrap: alloc::vec![false; rows], saved: SavedCursor::default() }
	}
}

pub struct Screen {
	cols: usize,
	rows: usize,
	col: usize,
	row: usize,
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
	cursor_visible: bool,
	cursor_shape: CursorShape,
	cursor_blink: bool,
	bell: bool,
	osc: [u8; 256],
	osc_len: usize,
	// Whether the control string in progress ran past the buffer - see `osc_byte`.
	osc_overflow: bool,
	// Mouse tracking the foreground program enabled (DEC private modes): 0 off, 1 normal
	// (?1000, button press/release), 2 button-event (?1002, + drag), 3 any-event (?1003, +
	// motion). The console reads this to decide whether to deliver pointer events to the
	// program as mouse reports or drive its own selection / scrollback.
	mouse_mode: u8,
	// The three DEC tracking modes a program may enable independently; `mouse_mode` is the highest
	// of them - see `refresh_mouse_mode`.
	mouse_press: bool,
	mouse_button: bool,
	mouse_any: bool,
	// Whether the program asked for SGR-encoded mouse reports (?1006: ESC[<b;x;yM/m).
	mouse_sgr: bool,
	// Bracketed paste (?2004): the console wraps a paste in ESC[200~ .. ESC[201~.
	bracketed_paste: bool,
	// A clipboard write the program requested via OSC 52 (decoded to plain text); drained
	// by the console into the clipboard it holds.
	clipboard_set: Option<Vec<u8>>,
	// A clipboard READ the program requested via `OSC 52 ; Pc ; ?`, holding the selection byte it
	// named. Drained by the console, which owns the clipboard and answers with `answer_clipboard`.
	clipboard_query: Option<u8>,
	// Bytes this terminal owes the program: the answer to a query sequence, drained by the console
	// into the program's input. See `take_reply`.
	reply: Vec<u8>,
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
	// The smallest codepoint the sequence in progress may legally encode - see `begin_utf8`.
	utf8_min: u32,
	// THE TWO SCREENS ARE TWO OBJECTS, and `alt_active` says which is live.
	//
	// They used to be four parallel fields - `primary`, `alt`, `wrap`, `alt_wrap` - with the wrap
	// vectors SWAPPED by hand on entry and exit, and that produced one defect with four symptoms,
	// all of them the same missing question: which screen is this?
	//
	//  - `global_glyph` read `primary` and `global_wrap` read `wrap`, so during alt they described
	//    different screens. A selection over a full-screen program highlighted the alternate cells
	//    and copied the shell text underneath them.
	//  - `resize` reflowed the primary buffer using the ALTERNATE screen's line structure, then
	//    assigned both vectors unconditionally, leaving them swapped afterwards.
	//  - RIS set `alt_active = false` directly rather than through `leave_alt_buffer`, which is the
	//    only place that swapped back - so a reset during alt left the primary live with the
	//    alternate's wrap flags.
	//  - `?47` preserved the alternate CELLS across leave and re-enter while clearing its wrap
	//    flags on every entry, so a program got its characters back without their line structure.
	//
	// One type answers the question once: a buffer owns its cells AND its wrap flags, `screen()`
	// picks the live one, and nothing swaps anything.
	primary: ScreenBuffer,
	alt: ScreenBuffer,
	alt_active: bool,
	dirty: Vec<bool>,
	// Whether the next glyph wraps before it lands: set when one fills the last column, cleared by
	// anything that moves the cursor. See `put_glyph`.
	pending_wrap: bool,
	scrollback: Vec<Cell>,
	// Per-row soft-wrap flag for the scrollback ring (parallel to `scrollback` rows).
	sb_wrap: Vec<bool>,
	sb_cap: usize,
	// WHAT WAS ASKED FOR, beside `sb_cap`, which is what fits.
	//
	// `resize` passed `self.sb_cap` back into `geometry` as the request, and `geometry` clamps the
	// request to `MAX_SCROLLBACK_BYTES / row_bytes` - which shrinks as the width grows. That made
	// the clamp MONOTONE across a terminal's life: widen once and the depth was reduced, narrow
	// again and the reduced value was what got re-clamped, so `console.scrollback` in the
	// configuration stopped describing the terminal after the first widen and never came back.
	//
	// Nothing was unsafe - the byte budget is the point and it held either way. The configured
	// number simply stopped meaning anything.
	requested_scrollback: usize,
	sb_head: usize,
	sb_len: usize,
	view_offset: usize,
	// Grid scrolls performed by the parser since the last flush; the renderer replays them
	// as bulk framebuffer pixel copies, then drains this. (L2 records geometry only - no
	// pixels.)
	scrolls: Vec<ScrollOp>,
	// The viewport cell the text mouse cursor sits on (an inverted block that tracks the
	// pointer, like the Linux console's gpm cursor), or None when hidden. A pure overlay:
	// `display_cell` / `view_cell` reverse this cell's colours so it rides on top of whatever
	// text or selection is there, and moving it dirties only the old and new cells.
	mouse: Option<(usize, usize)>,
}

// The one place a terminal's geometry and its scrollback budget are decided.
//
// At least one cell, so there is always somewhere for the cursor to be; the module's own maximums
// on top of whatever the caller asks for; and a scrollback bounded by BYTES rather than rows, so a
// wider display does not silently cost proportionally more - see `MAX_SCROLLBACK_BYTES`.
//
// Shared by `Screen::new` and `Screen::resize`, which is the point: the constructor enforced all
// three and the resize enforced only the caller's two, so every rule here stopped applying the
// moment a window changed size.
fn geometry(cols: usize, rows: usize, scrollback: usize, max_cols: usize, max_rows: usize) -> (usize, usize, usize) {
	let cols = cols.clamp(1, max_cols.max(1));
	let rows = rows.clamp(1, max_rows.max(1));
	let row_bytes = cols * core::mem::size_of::<Cell>();
	let scrollback = scrollback.min(MAX_SCROLLBACK_BYTES / row_bytes.max(1));
	(cols, rows, scrollback)
}

impl Screen {
	// Build a grid.
	//
	// BOUNDED AND CLAMPED, because the callers' numbers come from a framebuffer geometry and a
	// configuration tree. `cols * rows` and `scrollback * cols` were computed with no `checked_mul`,
	// and `Term::new` divides the surface by the cell size - so a surface smaller than 8x16 produced
	// a 0x0 `Screen` whose first `put_glyph` indexed an empty `wrap` vector. The kernel checks for
	// 0x0 after constructing one; a public API should not require its callers to know that.
	pub fn new(cols: usize, rows: usize, scrollback: usize) -> Screen {
		// The request is kept as it arrived; `scrollback` below is what fits at this width.
		let requested: usize = scrollback;
		let (cols, rows, scrollback) = geometry(cols, rows, scrollback, MAX_COLS, MAX_ROWS);
		let blank = Cell { glyph: b' ' as u32, fg: Color::Default, bg: Color::Default, bold: false, underline: false, reverse: false };
		Screen { cols, rows, col: 0, row: 0, scroll_top: 0, scroll_bottom: rows.saturating_sub(1), default_fg: FG, default_bg: BG, palette: ANSI_PALETTE, fg_color: Color::Default, bg_color: Color::Default, bold: false, underline: false, reverse: false, cursor_visible: true, cursor_shape: CursorShape::Underline, cursor_blink: false, bell: false, osc: [0; 256], osc_len: 0, osc_overflow: false, mouse_mode: 0, mouse_press: false, mouse_button: false, mouse_any: false, mouse_sgr: false, bracketed_paste: false, clipboard_set: None, clipboard_query: None, reply: Vec::new(), selection: None, esc_state: 0, csi_private: 0, params: [0; 16], nparams: 0, utf8_acc: 0, utf8_rem: 0, utf8_min: 0, primary: ScreenBuffer::new(cols, rows, blank), alt: ScreenBuffer::new(cols, rows, blank), alt_active: false, dirty: alloc::vec![true; cols * rows], pending_wrap: false, scrollback: alloc::vec![blank; scrollback * cols], sb_wrap: alloc::vec![false; scrollback], sb_cap: scrollback, requested_scrollback: requested, sb_head: 0, sb_len: 0, view_offset: 0, scrolls: Vec::new(), mouse: None }
	}

	// How many rows of scrollback this screen can hold at its current width. Public so a test can
	// assert the configured depth comes back after a widen and a narrow; the byte budget is what
	// bounds it, and the CONFIGURED number is what it is derived from at every resize.
	pub fn scrollback_capacity(&self) -> usize {
		self.sb_cap
	}

	// The LIVE screen: the alternate while it is up, else the primary. Every read and write of a
	// live cell or a live wrap flag goes through one of these two, which is what makes "which
	// screen is this?" a question with one answer.
	fn screen(&self) -> &ScreenBuffer {
		if self.alt_active { &self.alt } else { &self.primary }
	}

	fn screen_mut(&mut self) -> &mut ScreenBuffer {
		if self.alt_active { &mut self.alt } else { &mut self.primary }
	}

	// The live buffer's saved cursor. Whichever buffer is showing owns the DECSC slot, so a program
	// on the alternate screen cannot overwrite the shell's.
	fn saved(&self) -> SavedCursor {
		if self.alt_active { self.alt.saved } else { self.primary.saved }
	}

	fn set_saved(&mut self, saved: SavedCursor) {
		if self.alt_active {
			self.alt.saved = saved;
		} else {
			self.primary.saved = saved;
		}
	}

	// The active cell buffer.
	fn cells(&self) -> &[Cell] {
		&self.screen().cells
	}

	// A snapshot of the live cell at (col, row): the renderer's read of the grid model.
	// A blank cell outside the grid rather than a panic: this is public, `set_cell` already returns
	// quietly for the same coordinates, and a renderer that asks for a cell that is not there is
	// describing a stale geometry rather than a reason to end the process.
	pub fn cell(&self, col: usize, row: usize) -> Cell {
		if col >= self.cols || row >= self.rows {
			return self.blank();
		}
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
		if self.mouse == Some((col, row)) {
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

	// WHERE THE CARET LOGICALLY IS, as one linear cell index - `row * cols + col`, plus one when a
	// wrap is deferred.
	//
	// The line editor moves the caret by emitting backspaces and counting them, which assumes every
	// backspace moves: it does not, once the start of a long line has scrolled off the top and the
	// reverse-wrap has no row above it to step onto. Given this, the editor can compute where it
	// wants to be and address that cell directly - and can tell when the cell it wants is above the
	// screen, which is the case it used to walk into and lose the caret in.
	//
	// The deferred wrap is included because it is a real column the editor counted: a glyph that
	// filled the last column leaves the caret parked on it with the wrap owed, and the editor has
	// already counted that column as printed.
	pub fn caret_index(&self) -> usize {
		self.row * self.cols + self.col + usize::from(self.pending_wrap)
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

	// Whether the caret should BLINK. DECSCUSR separates blinking from steady and this flag records
	// which was asked for; there was no getter, so the renderer's blink timer toggled whenever the
	// cursor was visible - `CSI 4 SP q` (steady underline) blinked, and so did the default cursor,
	// whose flag is `false`.
	pub fn cursor_blink(&self) -> bool {
		self.cursor_blink
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

	// Record one scroll for the renderer to replay, BOUNDED.
	//
	// The only drain is `take_scrolls` inside the renderer's flush, and ConsoleService flushes the
	// foreground VT only - so a program on VT 2 while the user watches VT 1 accumulated one
	// `ScrollOp` per scrolled line, forever, and switching to it replayed every one of them as a
	// bulk pixel copy before the full repaint that overwrote them all.
	//
	// Past the cap the queue is DROPPED and the whole grid marked dirty. That is the same answer
	// `repaint` gives and a strictly cheaper one: a scroll diff longer than the screen has no
	// information a full repaint does not, and keeping it costs memory that grows with how long
	// nobody looked.
	fn record_scroll(&mut self, op: ScrollOp) {
		if self.scrolls.len() >= MAX_PENDING_SCROLLS {
			self.scrolls.clear();
			self.mark_all_dirty();
			return;
		}
		self.scrolls.push(op);
	}

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

	// Drain an OSC 52 clipboard QUERY, returning the selection byte the program named.
	pub fn take_clipboard_query(&mut self) -> Option<u8> {
		self.clipboard_query.take()
	}

	// Answer a clipboard query: `OSC 52 ; Pc ; <base64> ST`, into the same reply buffer DSR and DA
	// use, so the console does not need a second write-back path to the program.
	pub fn answer_clipboard(&mut self, selection: u8, text: &[u8]) {
		let mut out: Vec<u8> = alloc::vec![0x1b, b']', b'5', b'2', b';', selection, b';'];
		// AN OVERSIZED SELECTION IS ANSWERED EMPTY, not answered with nothing.
		//
		// base64 is four bytes per three, plus the `OSC 52 ; Pc ;` prefix and the `ESC \` terminator,
		// so what fits is `(MAX_REPLY_BYTES - 9) * 3 / 4`. Past that the program gets an empty
		// payload and learns the outcome; `push_reply` refusing the whole thing left it waiting for
		// an answer that was never going to come.
		let room = (MAX_REPLY_BYTES - out.len() - 2) * 3 / 4;
		if text.len() <= room {
			base64_encode(text, &mut out);
		}
		// ST rather than BEL: both terminate an OSC and `ESC \` is what a program that sent a
		// query is most likely to be scanning for.
		out.push(0x1b);
		out.push(b'\\');
		if !self.push_reply(&out) {
			// The full answer did not fit beside what is already queued. Answer empty rather than
			// not at all - the empty payload is a dozen bytes and says "nothing you can have",
			// which is the outcome the program can act on.
			let empty: [u8; 9] = [0x1b, b']', b'5', b'2', b';', selection, b';', 0x1b, b'\\'];
			let _ = self.push_reply(&empty);
		}
	}

	// Drain what the terminal owes the program in reply to a query - the same shape
	// `clipboard_set` and the tty mode requests already use: the model records what it owes, the
	// console delivers it into the program's input.
	pub fn take_reply(&mut self) -> Vec<u8> {
		core::mem::take(&mut self.reply)
	}

	// Whether a mouse selection is active (so the console copies it on release).
	pub fn has_selection(&self) -> bool {
		self.selection.is_some()
	}

	// The viewport cell the text mouse cursor sits on, if shown (the renderer follows it
	// through a scroll so the bulk pixel copy does not leave the block smeared).
	pub fn mouse(&self) -> Option<(usize, usize)> {
		self.mouse
	}

	// Set the text mouse-cursor cell (the inverted block that tracks the pointer), or None to
	// hide it. Dirties the old and new cells so the next flush repaints just those two.
	// Returns whether the position actually changed.
	pub fn set_mouse(&mut self, m: Option<(usize, usize)>) -> bool {
		if self.mouse == m {
			return false;
		}
		for cell in [self.mouse, m].into_iter().flatten() {
			let (c, r) = cell;
			if c < self.cols && r < self.rows {
				self.dirty[r * self.cols + c] = true;
			}
		}
		self.mouse = m;
		true
	}

	// Begin a mouse selection at viewport (col, row) for the current scroll offset: anchor
	// and end both start on the global cell the viewport position maps to.
	pub fn selection_begin(&mut self, col: usize, row: usize) {
		let old = self.selection;
		let g = self.view_global_row(row);
		let c = col.min(self.cols.saturating_sub(1));
		self.selection = Some((g, c, g, c));
		// The whole previous selection is gone, so repaint all of its rows; the fresh one is a
		// single cell.
		if let Some((ag, _, eg, _)) = old {
			self.dirty_global_span(ag.min(eg), ag.max(eg));
		}
		self.dirty_global_span(g, g);
	}

	// Extend the active selection's end to viewport (col, row) (a drag); a no-op with no
	// selection in progress.
	pub fn selection_extend(&mut self, col: usize, row: usize) {
		if let Some((ag, ac, oeg, _)) = self.selection {
			let g = self.view_global_row(row);
			let c = col.min(self.cols.saturating_sub(1));
			self.selection = Some((ag, ac, g, c));
			// Only the rows between the OLD and the NEW end change their highlight (the anchor
			// side is unchanged), so repaint just that band - not the whole selection. Dirtying
			// the entire span on every drag event is O(span) per event = O(span^2) over a drag,
			// which made a large selection lag badly.
			self.dirty_global_span(oeg.min(g), oeg.max(g));
		}
	}

	// Clear the selection highlight; a no-op (no repaint) when nothing was selected.
	pub fn selection_clear(&mut self) {
		if let Some((ag, _, eg, _)) = self.selection.take() {
			self.dirty_global_span(ag.min(eg), ag.max(eg));
		}
	}

	// Mark dirty every viewport row overlapping the global-row band [lo_g, hi_g], so a
	// selection change repaints only the rows whose highlight can actually differ - not the
	// whole grid (a full-grid, or even full-selection, repaint per pointer event is what made
	// selection feel laggy).
	fn dirty_global_span(&mut self, lo_g: usize, hi_g: usize) {
		let base = self.view_global_row(0);
		for row in 0..self.rows {
			let g = base + row;
			if g >= lo_g && g <= hi_g {
				for col in 0..self.cols {
					self.dirty[row * self.cols + col] = true;
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
			let buf = &mut self.screen_mut().cells;
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
			let screen = self.screen_mut();
			for c in screen.cells.iter_mut() {
				*c = blank;
			}
			for w in screen.wrap.iter_mut() {
				*w = false;
			}
		}
		self.mark_all_dirty();
		self.col = 0;
		self.row = 0;
	}

	// Resize the logical grid to new_cols x new_rows (clamped to what the physical
	// framebuffer can show), reflowing the screen: the overlapping rectangle of cells is
	// preserved (bottom-anchored so the cursor line stays on screen), the alternate screen
	// and scrollback are reset, and the now-unused area is cleared. This is the local
	// stand-in for a virtio-gpu mode-set; the same path runs on a real resolution
	// change once that driver lands.
	pub fn resize(&mut self, new_cols: usize, new_rows: usize, max_cols: usize, max_rows: usize) -> bool {
		// THE SAME FUNCTION THE CONSTRUCTOR USES, so the declared maximums mean the same thing at
		// both ends of a terminal's life.
		//
		// This clamped only to the caller's `max_cols`/`max_rows` - which `Term::resize` derives
		// from the surface geometry - so after a resize the ceiling was whatever the framebuffer
		// was rather than what the module declares. And `sb_cap` was carried over unchanged while
		// the scrollback was reallocated as `sb_cap * new_cols`, so the BYTE budget computed once
		// at construction stopped holding the moment the width changed: a terminal created narrow
		// kept its generous row count and multiplied it by a much larger width.
		// FROM THE REQUEST, not from the current capacity: re-clamping what the last clamp produced
		// is what made the depth ratchet down and stay down.
		let (new_cols, new_rows, new_sb_cap) = geometry(new_cols, new_rows, self.requested_scrollback, max_cols.min(MAX_COLS), max_rows.min(MAX_ROWS));
		if new_cols == self.cols && new_rows == self.rows && new_sb_cap == self.sb_cap {
			return false;
		}
		// REFLOW, which is what the comment always said and the code never did.
		//
		// It copied the overlapping `min(cols) x min(rows)` rectangle bottom-anchored and threw the
		// rest away, so narrowing the window DESTROYED the text right of the new width instead of
		// flowing it onto the next line - and in the same call emptied the scrollback, dropped the
		// wrap metadata, reallocated the alternate screen and forced `alt_active` to false, so a
		// full-screen program was thrown back into the primary buffer by a host window resize.
		//
		// The information needed is already here: the soft-wrap flags say which rows are
		// continuations, so the scrollback and the screen together are a sequence of LOGICAL lines.
		// Rebuild those, lay them out at the new width, and the screen is the tail.
		let blank = Cell { glyph: b' ' as u32, fg: Color::Default, bg: Color::Default, bold: false, underline: false, reverse: false };
		let mut lines: Vec<Vec<Cell>> = Vec::new();
		// Where the cursor is in logical terms, so it can be found again afterwards.
		// TWO POSITIONS FOLLOW THE REFLOW, both of them the primary buffer's: the live cursor when
		// the primary is the live buffer, and the primary's saved (DECSC / `?1049`) slot always.
		// During alt the live cursor belongs to the full-screen program and is clamped instead -
		// `usize::MAX` is a row no reflow can match, which is how it is excluded here.
		let live_track: usize = if self.alt_active { usize::MAX } else { self.row };
		let track: [(usize, usize); 2] = [(live_track, self.col), (self.primary.saved.row, self.primary.saved.col)];
		let mut tracked_at: [Option<(usize, usize)>; 2] = [None; 2];
		{
			let mut current: Vec<Cell> = Vec::new();
			let mut started = false;
			let total = self.sb_len + self.rows;
			for g in 0..total {
				let (row_cells, wrapped): (&[Cell], bool) = if g < self.sb_len {
					let ring = (self.sb_head + g) % self.sb_cap;
					(&self.scrollback[ring * self.cols..ring * self.cols + self.cols], self.sb_wrap[ring])
				} else {
					let r = g - self.sb_len;
					// THE PRIMARY BUFFER WITH THE PRIMARY FLAGS, whichever screen is showing. This
					// read `self.wrap`, which during alt holds the ALTERNATE screen's line
					// structure - so a resize while a full-screen program was up reflowed the
					// shell's scrollback according to the program's line breaks.
					(&self.primary.cells[r * self.cols..r * self.cols + self.cols], self.primary.wrap.get(r).copied().unwrap_or(false))
				};
				// TWO PRIMARY POSITIONS ARE TRACKED, the live one and the DECSC save, and both are
				// read from the primary buffer whichever screen is showing: the live pair belongs
				// to the primary only when the primary IS live, and is parked with it otherwise.
				//
				// Before, the mapping was skipped entirely during alt and nothing took the other
				// branch, so `cursor_row_out` stayed `None`, the "end of the content" fallback
				// fired, and a window resize teleported a full-screen program's cursor to a
				// position computed from the SHELL's scrollback. And the save was never remapped at
				// all, so `?1049l` after a width change returned the shell to a cell that was no
				// longer where its prompt was.
				for (slot, (trow, tcol)) in track.iter().enumerate() {
					if g >= self.sb_len && g - self.sb_len == *trow {
						tracked_at[slot] = Some((lines.len(), current.len() + tcol));
					}
				}
				current.extend_from_slice(row_cells);
				started = true;
				if !wrapped {
					// A hard line end: trim the trailing blanks a fixed-width grid pads with, so a
					// narrower screen does not re-wrap padding into rows of its own.
					while current.last().is_some_and(|c| c.glyph == b' ' as u32 && c.bg == Color::Default && !c.reverse) {
						current.pop();
					}
					lines.push(core::mem::take(&mut current));
					started = false;
				}
			}
			if started {
				lines.push(current);
			}
		}
		// Lay the logical lines out at the new width. A line longer than the screen becomes several
		// rows, all but the last marked as soft-wrapped, which is the same encoding this screen
		// already uses - so the next resize can undo it.
		let mut rows_out: Vec<(Vec<Cell>, bool)> = Vec::new();
		let mut tracked_out: [Option<(usize, usize)>; 2] = [None; 2];
		for (index, line) in lines.iter().enumerate() {
			let chunks = line.len().div_ceil(new_cols).max(1);
			for chunk in 0..chunks {
				let from = chunk * new_cols;
				let to = (from + new_cols).min(line.len());
				let mut row: Vec<Cell> = alloc::vec![blank; new_cols];
				for (at, cell) in line[from..to].iter().enumerate() {
					row[at] = *cell;
				}
				for (slot, at) in tracked_at.iter().enumerate() {
					if let Some((line_index, column)) = *at
						&& line_index == index
						&& (column >= from && column < from + new_cols)
					{
						tracked_out[slot] = Some((rows_out.len(), column - from));
					}
				}
				rows_out.push((row, chunk + 1 < chunks));
			}
		}
		if rows_out.is_empty() {
			rows_out.push((alloc::vec![blank; new_cols], false));
		}
		// The last `new_rows` rows are the screen; everything before them is scrollback.
		let screen_from = rows_out.len().saturating_sub(new_rows);
		let mut new_primary = alloc::vec![blank; new_cols * new_rows];
		let mut new_wrap = alloc::vec![false; new_rows];
		for (r, (row, wrapped)) in rows_out[screen_from..].iter().enumerate() {
			new_primary[r * new_cols..r * new_cols + new_cols].copy_from_slice(row);
			new_wrap[r] = *wrapped;
		}
		let mut new_scrollback = alloc::vec![blank; new_sb_cap * new_cols];
		let mut new_sb_wrap = alloc::vec![false; new_sb_cap];
		let keep_from = screen_from.saturating_sub(new_sb_cap);
		let mut sb_len = 0usize;
		for (row, wrapped) in rows_out[keep_from..screen_from].iter() {
			new_scrollback[sb_len * new_cols..sb_len * new_cols + new_cols].copy_from_slice(row);
			new_sb_wrap[sb_len] = *wrapped;
			sb_len += 1;
		}
		// A tracked position follows its own cell. When the reflow lost it - an empty screen, or a
		// position past the last written cell - it lands at the end of the content, which is where
		// a shell prompt is.
		let end_of_content: usize = rows_out.len().saturating_sub(1).saturating_sub(screen_from);
		let resolve = |slot: Option<(usize, usize)>| -> (usize, usize) {
			let (r, c) = match slot {
				Some((r, c)) if r >= screen_from => (r - screen_from, c),
				_ => (end_of_content, 0),
			};
			(r.min(new_rows - 1), c.min(new_cols - 1))
		};
		let (sav_row, sav_col) = resolve(tracked_out[1]);
		let mut primary_saved: SavedCursor = self.primary.saved;
		primary_saved.row = sav_row;
		primary_saved.col = sav_col;

		// THE ALTERNATE SCREEN'S POSITIONS ARE CLAMPED, NOT REFLOWED. Its buffer is a rectangle
		// copied as a rectangle - a full-screen program redraws on a resize - so there are no
		// logical lines to follow, and clamping is what such a program expects. It is also strictly
		// better than what it replaces: the live cursor during alt fell through to the "end of the
		// content" fallback, which is a position computed from the SHELL's scrollback, in a buffer
		// the program cannot see.
		let mut alt_saved: SavedCursor = self.alt.saved;
		alt_saved.col = alt_saved.col.min(new_cols - 1);
		alt_saved.row = alt_saved.row.min(new_rows - 1);
		let (cur_row, cur_col) = if self.alt_active { (self.row.min(new_rows - 1), self.col.min(new_cols - 1)) } else { resolve(tracked_out[0]) };

		// THE ALTERNATE SCREEN SURVIVES. It is a rectangle by definition - a full-screen program
		// redraws it on a resize - so it is copied as one rather than reflowed, and `alt_active`
		// stays what it was instead of being forced back to the primary buffer.
		let mut new_alt = alloc::vec![blank; new_cols * new_rows];
		let copy_rows = self.rows.min(new_rows);
		let copy_cols = self.cols.min(new_cols);
		for r in 0..copy_rows {
			for c in 0..copy_cols {
				new_alt[r * new_cols + c] = self.alt.cells[r * self.cols + c];
			}
		}

		self.primary = ScreenBuffer { cells: new_primary, wrap: new_wrap, saved: primary_saved };
		// The alternate screen's flags start clear because a full-screen program redraws on a
		// resize; its CELLS are copied so it does not go blank underneath one that has not yet.
		self.alt = ScreenBuffer { cells: new_alt, wrap: alloc::vec![false; new_rows], saved: alt_saved };
		self.dirty = alloc::vec![true; new_cols * new_rows];
		self.scrollback = new_scrollback;
		self.sb_wrap = new_sb_wrap;
		self.sb_cap = new_sb_cap;
		self.sb_head = 0;
		self.sb_len = sb_len;
		self.view_offset = 0;
		self.selection = None;
		self.mouse = None;
		self.cols = new_cols;
		self.rows = new_rows;
		self.col = cur_col;
		self.row = cur_row;
		self.pending_wrap = false;
		self.scroll_top = 0;
		self.scroll_bottom = new_rows - 1;
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
			self.scrollback[dst + col] = self.primary.cells[src + col];
		}
		self.sb_wrap[ring] = self.primary.wrap[screen_row];
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
			self.screen().cells[(g - self.sb_len) * self.cols + col]
		};
		if self.is_selected(g, col) {
			cell.reverse = !cell.reverse;
		}
		if self.mouse == Some((col, row)) {
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
			let buf = &mut self.screen_mut().cells;
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
			self.screen_mut().wrap[row] = if src <= bot { self.screen().wrap[src] } else { false };
		}
		self.record_scroll(ScrollOp { top, bot, n, down: false });
	}

	// Scroll the rows [top, bot] down by n, filling the freed top rows with blanks - the
	// downward counterpart of region_up (reverse index / insert line).
	fn region_down(&mut self, top: usize, bot: usize, n: usize) {
		let n = n.max(1);
		let cols = self.cols;
		let blank = self.blank();
		{
			let buf = &mut self.screen_mut().cells;
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
			self.screen_mut().wrap[row] = if row >= top + n { self.screen().wrap[row - n] } else { false };
		}
		self.record_scroll(ScrollOp { top, bot, n, down: true });
	}

	fn scroll_up(&mut self, n: usize) {
		// Lines that scroll off the top of the full primary screen go to scrollback (not
		// when a program set a scroll region, nor on the alternate screen). A held scroll
		// view is nudged up by the same amount so its content stays anchored.
		// THE WHOLE SCREEN, not merely a region that starts at the top. This checked `scroll_top ==
		// 0` and never `scroll_bottom == rows - 1`, though the comment above says a region scroll
		// must not reach history - so `CSI 1;10r` filled the scrollback with the top ten rows of a
		// status pane. And the count is clamped to the REGION, not to the screen: `CSI 999S` inside
		// a small region used to push rows that were never in it.
		let whole_screen = self.scroll_top == 0 && self.scroll_bottom + 1 == self.rows;
		if !self.alt_active && whole_screen {
			let n = n.min(self.scroll_bottom - self.scroll_top + 1);
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
	//
	// THIS MOVES THE CURSOR, so it ends a deferred wrap - which it did not, and that was the other
	// half of the `pending_wrap` defect. A full line followed by `ESC D` followed by a glyph fed
	// twice: once for the ESC and once for the wrap the ESC had not cleared, so the character
	// landed two rows down. `ESC M` and `ESC E` had the same shape.
	//
	// `put_glyph`'s own wrap path clears the flag BEFORE calling this, so the clear here is not
	// double work - it is the same rule reaching the paths that were not going through it.
	fn line_feed(&mut self) {
		if self.row == self.scroll_bottom {
			self.scroll_up(1);
		} else if self.row + 1 < self.rows {
			self.row += 1;
		}
		self.cursor_moved();
	}

	// Reverse line feed (RI): move up one line, scrolling down if at the top.
	fn reverse_line_feed(&mut self) {
		if self.row == self.scroll_top {
			self.scroll_down(1);
		} else if self.row > 0 {
			self.row -= 1;
		}
		self.cursor_moved();
	}

	fn put_glyph(&mut self, glyph: u32) {
		if self.pending_wrap {
			// The previous glyph filled the last column: this row soft-wraps into the next.
			let row = self.row;
			self.screen_mut().wrap[row] = true;
			self.col = 0;
			self.pending_wrap = false;
			self.line_feed();
		}
		let cell = Cell { glyph, fg: self.fg_color, bg: self.bg_color, bold: self.bold, underline: self.underline, reverse: self.reverse };
		self.set_cell(self.col, self.row, cell);
		// PENDING WRAP IS A FLAG, not a cursor one past the end.
		//
		// This did `self.col += 1` unconditionally, so a glyph in the last column left `col ==
		// cols` and only the NEXT glyph read that as a wrap. Two things followed. The renderer
		// draws the caret only where `cursor_col() < cols`, so filling a line exactly made the
		// caret vanish. And a CSI that moves the row alone - `CSI B` - left `col == cols` intact,
		// so the next glyph both wrapped and line-fed and landed a row below where it belonged.
		//
		// The cursor now always names a real cell, and the deferred wrap is the state it actually
		// is. Every absolute or relative column move clears it, because a cursor that has been
		// moved is not waiting to wrap.
		if self.col + 1 >= self.cols {
			self.pending_wrap = true;
		} else {
			self.col += 1;
		}
	}

	// Render a decoded Unicode codepoint: the cell records the codepoint itself, and the
	// renderer resolves it to a font glyph (one the font lacks draws as '?').
	fn put_codepoint(&mut self, cp: u32) {
		self.put_glyph(cp);
	}

	// Begin a UTF-8 multi-byte sequence from its lead byte, recording how many
	// continuation bytes follow. A stray continuation or invalid lead renders U+FFFD.
	fn begin_utf8(&mut self, byte: u8) {
		// `utf8_min` is the smallest codepoint this LENGTH is allowed to encode, which is what makes
		// an overlong form detectable once the sequence completes: `\xc0\x80` is a two-byte
		// encoding of U+0000, shaped perfectly and forbidden, and it is how a filter that looks for
		// a byte sequence gets walked past.
		if byte & 0xe0 == 0xc0 {
			self.utf8_acc = (byte & 0x1f) as u32;
			self.utf8_rem = 1;
			self.utf8_min = 0x80;
		} else if byte & 0xf0 == 0xe0 {
			self.utf8_acc = (byte & 0x0f) as u32;
			self.utf8_rem = 2;
			self.utf8_min = 0x800;
		} else if byte & 0xf8 == 0xf0 {
			self.utf8_acc = (byte & 0x07) as u32;
			self.utf8_rem = 3;
			self.utf8_min = 0x1_0000;
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
					// The ENCODING, not just the shape. The bit patterns were all that was checked,
					// so an overlong form, a surrogate and anything above U+10FFFF all reached a
					// cell - and then rendered as `?`, because the font lookup rejected what the
					// decoder had accepted. Every malformed sequence is one U+FFFD, which is what
					// this crate's own comments already promised.
					let cp = self.utf8_acc;
					let valid = cp <= 0x10FFFF && !(0xD800..=0xDFFF).contains(&cp) && cp >= self.utf8_min;
					self.put_codepoint(if valid { cp } else { 0xfffd });
				}
				return;
			}
			// A TRUNCATED sequence is one replacement character, and then this byte is reinterpreted
			// on its own. It used to vanish: the partial was dropped silently, so `\xe2\x82A`
			// printed `A` and nothing else, and a stream that lost a byte lost a character with no
			// sign that it had.
			self.utf8_rem = 0;
			self.put_codepoint(0xfffd);
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
		// NO BLANKET CLEAR HERE. There was one - every byte under 0x20 that was not ESC, plus DEL,
		// cleared `pending_wrap` before dispatch - hoisted "so a new one cannot forget". It made the
		// flag wrong for every control byte that moves NOTHING: BEL is such a byte, so `12345678`,
		// BEL, `X` on an eight-column screen put the X over the 8 instead of at the start of the
		// next row. A shell that rings the bell on a completion miss at the end of a full line hits
		// it. The CSI path abandoned the same shortcut for the same reason.
		//
		// The blanket was load-bearing: `\r`, `\t` and the reverse-wrap backspace assign the cursor
		// and relied on it. They call `cursor_moved` themselves now, which is what the rule at
		// `cursor_moved` says every assigning path does. `\n` is covered by `line_feed`.
		match byte {
			0x1b => self.esc_state = 1,
			// There is no tty/line discipline yet, so NL still implies a carriage return.
			b'\n' => {
				self.col = 0;
				self.line_feed();
			}
			b'\r' => {
				self.col = 0;
				self.cursor_moved();
			}
			0x08 => {
				if self.col > 0 {
					self.col -= 1;
				} else if self.row > 0 && self.screen().wrap.get(self.row - 1).copied().unwrap_or(false) {
					// REVERSE WRAP over a soft-wrapped row. Backspace stopped dead at column 0, so
					// the line editor - which moves the cursor by repeating `\x08` - could not step
					// back onto the previous row. On an 80-column terminal a command long enough to
					// wrap left Home, Backspace and mid-line editing moving `Ld.cursor` while the
					// caret stayed stuck on the last physical row: the buffer and the screen
					// diverged and every later edit was drawn in the wrong place.
					//
					// Only over a row this screen itself marked as continuing into this one, so a
					// backspace at the start of a REAL line still stops where the line starts.
					self.row -= 1;
					self.col = self.cols - 1;
				}
				self.cursor_moved();
			}
			b'\t' => {
				let next = (self.col / 8 + 1) * 8;
				self.col = next.min(self.cols.saturating_sub(1));
				self.cursor_moved();
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
				self.cursor_moved();
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
		} else {
			// TRUNCATION IS A REFUSAL. Bytes past the buffer were dropped and whatever had been
			// kept was dispatched anyway - so an OSC 52 payload longer than 256 bytes set the
			// clipboard to a PREFIX of the intended text. A control string that did not arrive
			// whole was not received, and the flag is what `osc_dispatch` reads to say so.
			self.osc_overflow = true;
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

	// THE RULE, IN ONE PLACE: moving the cursor ends a deferred wrap; not moving it does not.
	//
	// `csi_dispatch` used to clear `pending_wrap` unconditionally at its entry, under a comment
	// saying "a sequence that moves or repositions the cursor ends any deferred wrap" - and most
	// CSI sequences do not move the cursor. SGR, DSR, DA, the cursor style and every mode set went
	// through it, so on an eight-column screen `12345678` `ESC[31m` `X` put the X over the 8
	// instead of at the start of the next row. A colour change between a full line and the next
	// character is what a prompt does.
	//
	// So every path that actually assigns `row` or `col` calls this, and nothing else does.
	fn cursor_moved(&mut self) {
		self.pending_wrap = false;
	}

	fn csi_dispatch(&mut self, byte: u8) {
		match byte {
			b'A' => {
				self.row = self.row.saturating_sub(self.param(0, 1));
				self.cursor_moved();
			}
			b'B' => {
				self.row = (self.row + self.param(0, 1)).min(self.rows.saturating_sub(1));
				self.cursor_moved();
			}
			b'C' => {
				self.col = (self.col + self.param(0, 1)).min(self.cols.saturating_sub(1));
				self.cursor_moved();
			}
			b'D' => {
				self.col = self.col.saturating_sub(self.param(0, 1));
				self.cursor_moved();
			}
			b'E' => {
				self.col = 0;
				self.row = (self.row + self.param(0, 1)).min(self.rows.saturating_sub(1));
				self.cursor_moved();
			}
			b'F' => {
				self.col = 0;
				self.row = self.row.saturating_sub(self.param(0, 1));
				self.cursor_moved();
			}
			b'G' => {
				self.col = (self.param(0, 1) - 1).min(self.cols.saturating_sub(1));
				self.cursor_moved();
			}
			b'd' => {
				self.row = (self.param(0, 1) - 1).min(self.rows.saturating_sub(1));
				self.cursor_moved();
			}
			b'H' | b'f' => {
				let r = self.param(0, 1);
				let c = self.param(1, 1);
				self.row = (r - 1).min(self.rows.saturating_sub(1));
				self.col = (c - 1).min(self.cols.saturating_sub(1));
				self.cursor_moved();
			}
			// DSR - device status report. `CSI 5n` is "are you there" and `CSI 6n` asks for the
			// cursor position, one-based; the reply goes back as though the user had typed it.
			// There was no reply path at all, so a program asking where the cursor is waited
			// forever.
			b'n' if self.csi_private == 0 => match self.param(0, 0) {
				// A refused DSR is a lost cursor report, not a hung program: the answer is
				// small and the buffer is drained every output pass, so it is refused only
				// when a program is querying faster than it reads.
				5 => {
					let _ = self.push_reply(b"\x1b[0n");
				}
				6 => {
					let mut out: Vec<u8> = alloc::vec![0x1b, b'['];
					push_dec(&mut out, self.row + 1);
					out.push(b';');
					push_dec(&mut out, self.col + 1);
					out.push(b'R');
					let _ = self.push_reply(&out);
				}
				_ => {}
			},
			// DA - device attributes. The answer says VT102, which is what this feature set is
			// closest to and what a program reads as "no extensions to negotiate".
			b'c' if self.csi_private == 0 => {
				let _ = self.push_reply(b"\x1b[?6c");
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
			b'u' => {
				self.restore_cursor();
				self.cursor_moved();
			}
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
			2 => (0, total),
			// `CSI 3 J` clears the SAVED lines - the scrollback - and leaves the display alone.
			// This fell into the catch-all and wiped the screen instead, so the one sequence a
			// program uses to drop history destroyed what the user was reading.
			3 => {
				self.sb_len = 0;
				self.sb_head = 0;
				// AND REPAINT, through the same helper `snap_live` uses. This assigned
				// `view_offset = 0` and returned with no `mark_all_dirty`, so a user reading
				// history when a program sent `CSI 3J` got the model switched to the live screen
				// while the renderer still believed the visible cells were clean - and the
				// framebuffer went on showing pieces of the scrollback view.
				//
				// Leaving history is what requires the repaint, wherever it happens, which is the
				// argument for one helper rather than three assignments open-coded twice.
				self.snap_live();
				return;
			}
			// An erase mode this terminal does not implement erases NOTHING. It used to erase
			// everything, so `CSI 99J` - a typo, a mangled sequence, a program written for another
			// terminal - cleared the screen.
			_ => return,
		};
		self.fill_cells(start, end);
	}

	// EL - erase in line: 0 cursor..eol, 1 bol..cursor, 2 the whole line.
	fn erase_line(&mut self, mode: usize) {
		let row_start = self.row * self.cols;
		let (start, end) = match mode {
			0 => (row_start + self.col, row_start + self.cols),
			1 => (row_start, row_start + self.col + 1),
			2 => (row_start, row_start + self.cols),
			// Same rule as `erase_display`: an unimplemented mode does nothing.
			_ => return,
		};
		self.fill_cells(start, end);
	}

	// Blank the cell range [start, end) and mark it dirty.
	fn fill_cells(&mut self, start: usize, end: usize) {
		let blank = self.blank();
		{
			let buf = &mut self.screen_mut().cells;
			let end = end.min(buf.len());
			for cell in &mut buf[start.min(end)..end] {
				*cell = blank;
			}
		}
		// A row whose LAST cell was erased is not soft-wrapped into the next one any more.
		//
		// `CSI J` and `CSI K` both come through here, and they blanked the cells and left `wrap`
		// alone - so a flag survived the complete erasure of the row it described, and `TextSink`
		// went on joining an empty row to its successor. `clear()` does clear them, which is why
		// this was easy to miss.
		if self.cols > 0 {
			let first = start / self.cols;
			let last = end.saturating_sub(1) / self.cols;
			for row in first..=last.min(self.rows.saturating_sub(1)) {
				let row_end = (row + 1) * self.cols;
				if start <= row_end.saturating_sub(1) && end >= row_end {
					if let Some(flag) = self.screen_mut().wrap.get_mut(row) {
						*flag = false;
					}
				}
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
			let buf = &mut self.screen_mut().cells;
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
			let buf = &mut self.screen_mut().cells;
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
		// DECSTBM homes the cursor, so it assigns it, so it says so. `csi_dispatch` no longer clears
		// the flag on entry - that shortcut was removed for the non-moving sequences - and without
		// this line DECSTBM left a wrap deferred against a column the cursor had just left.
		self.cursor_moved();
	}

	fn save_cursor(&mut self) {
		let saved = SavedCursor { col: self.col, row: self.row, fg_color: self.fg_color, bg_color: self.bg_color, bold: self.bold, underline: self.underline, reverse: self.reverse };
		self.set_saved(saved);
	}

	fn restore_cursor(&mut self) {
		let saved: SavedCursor = self.saved();
		self.col = saved.col.min(self.cols.saturating_sub(1));
		self.row = saved.row.min(self.rows.saturating_sub(1));
		self.fg_color = saved.fg_color;
		self.bg_color = saved.bg_color;
		self.bold = saved.bold;
		self.underline = saved.underline;
		self.reverse = saved.reverse;
	}

	// DEC private mode set/reset (CSI ? ... h/l): cursor visibility + alternate screen.
	fn set_mode(&mut self, enable: bool) {
		if self.csi_private != b'?' {
			return;
		}
		for i in 0..=self.nparams {
			match self.params[i] {
				25 => self.cursor_visible = enable,
				// THE THREE ALTERNATE-SCREEN MODES ARE NOT ONE MODE, and ported full-screen
				// programs depend on the difference.
				//
				//   ?47   switches buffers and nothing else - the program saves its own cursor.
				//   ?1047 switches, and CLEARS the alternate screen when leaving, so the next
				//         program that enters it does not inherit the last one's picture.
				//   ?1049 saves the cursor on entry and restores it on leaving, as well as
				//         clearing - the sequence a modern program uses precisely so it does not
				//         have to do either by hand.
				47 => {
					if enable {
						self.enter_alt_buffer();
					} else {
						self.leave_alt_buffer();
					}
				}
				1047 => {
					if enable {
						self.enter_alt_buffer();
					} else {
						self.leave_alt_buffer();
						self.clear_alt();
					}
				}
				1049 => {
					if enable {
						self.save_cursor();
						self.enter_alt_buffer();
						// ...and blank it, which is the part that makes `?1049h` a clean slate.
						self.clear_alt();
					} else {
						self.leave_alt_buffer();
						self.clear_alt();
						self.restore_cursor();
					}
				}
				// 9001 / 9002 ARE GONE. They set the tty's raw and echo modes from the OUTPUT
				// stream, so `cat` on a file containing them reconfigured the terminal - a
				// program's data and a program's request were the same bytes. Mode control is a
				// request on the terminal's control channel now (`rt::tty_set_mode`), which only
				// an interactive foreground job is given, and there is deliberately no second path
				// to the same state.
				// Disabling a HIGHER tracking mode falls back to whatever lower one is still on
				// rather than turning tracking off: `?1002l` cancelled a `?1000` that the program
				// never disabled and still believes it has.
				// THREE MODES, not one number. Disabling a higher one used to set `mouse_mode = 0`
				// outright, cancelling a `?1000` the program never disabled and still believes it
				// has. They are tracked separately and the effective mode is the highest enabled.
				1000 => {
					self.mouse_press = enable;
					self.refresh_mouse_mode();
				}
				1002 => {
					self.mouse_button = enable;
					self.refresh_mouse_mode();
				}
				1003 => {
					self.mouse_any = enable;
					self.refresh_mouse_mode();
				}
				1006 => self.mouse_sgr = enable,
				2004 => self.bracketed_paste = enable,
				_ => {}
			}
		}
	}

	// Switch to the alternate buffer. The CURSOR POLICY is the caller's - see `set_mode`, where the
	// three DEC modes differ in exactly that and used to share one path.
	fn enter_alt_buffer(&mut self) {
		if self.alt_active {
			return;
		}
		self.alt_active = true;
		self.view_offset = 0;
		// NOTHING IS SWAPPED AND NOTHING IS CLEARED. Each buffer owns its own flags, so the
		// primary's are simply not the live ones any more - and the alternate's are whatever it
		// had, which is the fix for `?47`: the cells were deliberately preserved across leave and
		// re-enter while their wrap flags were wiped on every entry, so a program got its
		// characters back without their line structure. Entry-clearing belongs to `?1049`, and it
		// clears the whole buffer through `clear_alt`.
		self.mark_all_dirty();
		self.col = 0;
		self.row = 0;
		self.cursor_moved();
	}

	fn leave_alt_buffer(&mut self) {
		if !self.alt_active {
			return;
		}
		self.alt_active = false;
		self.mark_all_dirty();
		// A DEFERRED WRAP DOES NOT SURVIVE THE SWITCH, in either direction. The flag describes "the
		// glyph in the last column of the live buffer's current row is waiting to wrap", and the
		// live buffer is being changed underneath it: after `?47l` it would describe the primary's
		// cursor, which is parked wherever the shell left it and has no such glyph. Entering gets
		// this for free by homing; leaving has to say it.
		self.cursor_moved();
	}

	// Blank the alternate buffer, so the next program to enter it does not inherit the last one's
	// picture. `?1047` and `?1049` do this on the way out; `?47` deliberately does not.
	//
	// Called AFTER `leave_alt_buffer`, because while the alternate screen is active `wrap` holds
	// ITS flags and `alt_wrap` holds the primary's parked ones - clearing before the swap wiped the
	// primary's, which is the thing the swap exists to protect.
	// The effective tracking mode: the highest of the three the program has enabled.
	fn refresh_mouse_mode(&mut self) {
		self.mouse_mode = if self.mouse_any {
			3
		} else if self.mouse_button {
			2
		} else if self.mouse_press {
			1
		} else {
			0
		};
	}

	// Blank the alternate buffer. ONE FUNCTION NOW, because there is no longer a "while it is
	// active" case: the flags live in the buffer they describe, so clearing it does not depend on
	// which screen is showing. The two that existed - `clear_alt_active` and `clear_alt` - differed
	// only in which vector they reached through the swap, and that swap is gone.
	fn clear_alt(&mut self) {
		let blank = self.blank();
		for cell in self.alt.cells.iter_mut() {
			*cell = blank;
		}
		for w in self.alt.wrap.iter_mut() {
			*w = false;
		}
		// AND ITS SAVED CURSOR, which is the alternate's own DECSC slot: a fresh alternate screen has
		// nothing saved on it, and a DECRC from the next program must not restore a position the
		// previous one stored.
		self.alt.saved = SavedCursor::default();
	}

	// RIS - reset to the initial state.
	// RIS - reset to the initial state, which means INDISTINGUISHABLE FROM A FRESH `Screen`.
	//
	// It restored SGR, the cursor, the margins, the mouse modes and bracketed paste, and left the
	// OSC-modified palette, the default colours, the DECSCUSR shape and blink and the saved-cursor
	// state exactly as the previous program had them. That is the whole purpose of the sequence and
	// the thing a shell relies on after a program crashes: a terminal it did not configure.
	fn reset(&mut self) {
		self.fg_color = Color::Default;
		self.bg_color = Color::Default;
		self.bold = false;
		self.underline = false;
		self.reverse = false;
		self.cursor_visible = true;
		self.cursor_shape = CursorShape::Underline;
		self.cursor_blink = false;
		self.scroll_top = 0;
		self.scroll_bottom = self.rows.saturating_sub(1);
		// THROUGH `leave_alt_buffer`, and the alternate screen is blanked as well as the primary.
		//
		// This assigned `alt_active = false` directly, which was the only correct thing to do while
		// the two screens shared a swapped wrap vector - and it was not correct: it skipped the
		// swap, so a RIS during alt left the primary live with the alternate's line structure, and
		// the `clear()` below then ran against it. With each buffer owning its own flags the direct
		// assignment is merely inconsistent rather than wrong, and going through the one function
		// that leaves the alternate screen is what keeps it that way.
		self.leave_alt_buffer();
		self.clear_alt();
		self.view_offset = 0;
		self.sb_head = 0;
		self.sb_len = 0;
		self.mouse_mode = 0;
		self.mouse_press = false;
		self.mouse_button = false;
		self.mouse_any = false;
		self.mouse_sgr = false;
		self.bracketed_paste = false;
		self.selection = None;
		self.mouse = None;
		self.pending_wrap = false;
		// The palette and the default colours an OSC changed, which outlived the program that
		// changed them.
		self.palette = ANSI_PALETTE;
		self.default_fg = FG;
		self.default_bg = BG;
		// The saved cursor, so a later DECRC cannot restore a position from before the reset. Both
		// buffers' - RIS means indistinguishable from a fresh `Screen`, and the alternate's slot is
		// not reachable from the primary once `leave_alt_buffer` above has run.
		self.primary.saved = SavedCursor::default();
		self.alt.saved = SavedCursor::default();
		// Any half-parsed sequence: a reset arriving mid-escape leaves no state behind it.
		self.esc_state = 0;
		self.csi_private = 0;
		self.nparams = 0;
		self.params = [0; 16];
		self.osc_len = 0;
		self.osc_overflow = false;
		self.utf8_rem = 0;
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
	// bar. The blink flag says which of the pair was asked for and `blink_caret` reads it.
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
	// Queue bytes for the program, bounded: a reply nobody drains must not grow without limit, and a
	// program that queries faster than the console delivers is asking for the same answer twice.
	// True when the reply was taken. A caller that has produced an answer and cannot deliver it needs
	// to know: `answer_clipboard` used to discard one silently, which is the same silence the query
	// item exists to remove.
	fn push_reply(&mut self, bytes: &[u8]) -> bool {
		if self.reply.len() + bytes.len() > MAX_REPLY_BYTES {
			return false;
		}
		self.reply.extend_from_slice(bytes);
		true
	}

	fn osc_dispatch(&mut self) {
		// A control string that ran past the buffer did not arrive, so nothing it might have said
		// is acted on. See `osc_byte`.
		if self.osc_overflow {
			self.osc_overflow = false;
			self.osc_len = 0;
			return;
		}
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
						// EVERY CELL CHANGES. Cells hold `Color::Idx` and resolve to RGB at draw
						// time - the right design - so repainting the palette repaints the screen,
						// and nothing here dirtied it: the old colours stayed until something else
						// happened to.
						self.mark_all_dirty();
					}
				}
			}
			Some(10) => {
				if let Some((r, g, b)) = parse_osc_color(&self.osc[rest_start..len]) {
					self.default_fg = (r, g, b);
					// Same reason as OSC 4: `Color::Default` is resolved at draw time.
					self.mark_all_dirty();
				}
			}
			Some(11) => {
				if let Some((r, g, b)) = parse_osc_color(&self.osc[rest_start..len]) {
					self.default_bg = (r, g, b);
					self.mark_all_dirty();
				}
			}
			Some(52) => {
				// OSC 52 ; Pc ; Pd - set the clipboard, or QUERY it when Pd is "?".
				//
				// The query used to do nothing, under a comment saying it "needs a write-back path
				// the console owns, not this model". That path is `reply`, three hundred lines
				// above, and it is what DSR and DA already answer through. What the model does not
				// own is the clipboard CONTENT, so it records the request the same way it records
				// `clipboard_set` - the console drains it and calls `answer_clipboard`, which puts
				// the reply where every other reply goes.
				let rest = &self.osc[rest_start..len];
				if let Some(semi2) = rest.iter().position(|&b| b == b';') {
					let selection: u8 = rest.first().copied().filter(|&b| b != b';').unwrap_or(b'c');
					let data = &rest[semi2 + 1..];
					if data == b"?" {
						self.clipboard_query = Some(selection);
					} else if let Some(text) = base64_decode(data) {
						self.clipboard_set = Some(text);
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
			self.screen().cells[(g - self.sb_len) * self.cols + col].glyph
		}
	}

	// Whether global row `g` soft-wraps into the next row (so the two are one logical line).
	pub(crate) fn global_wrap(&self, g: usize) -> bool {
		if g < self.sb_len {
			let ring = (self.sb_head + g) % self.sb_cap;
			self.sb_wrap[ring]
		} else {
			self.screen().wrap[g - self.sb_len]
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
// Append `v` as ASCII decimal - the reply sequences carry one-based coordinates.
fn push_dec(out: &mut Vec<u8>, v: usize) {
	if v >= 10 {
		push_dec(out, v / 10);
	}
	out.push(b'0' + (v % 10) as u8);
}

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
// Base64 into `out`, for the OSC 52 query answer. The decoder below is its opposite.
fn base64_encode(data: &[u8], out: &mut Vec<u8>) {
	const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
	for chunk in data.chunks(3) {
		let b0 = chunk[0] as u32;
		let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
		let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
		let triple = (b0 << 16) | (b1 << 8) | b2;
		out.push(ALPHABET[(triple >> 18) as usize & 0x3f]);
		out.push(ALPHABET[(triple >> 12) as usize & 0x3f]);
		out.push(if chunk.len() > 1 { ALPHABET[(triple >> 6) as usize & 0x3f] } else { b'=' });
		out.push(if chunk.len() > 2 { ALPHABET[triple as usize & 0x3f] } else { b'=' });
	}
}

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
