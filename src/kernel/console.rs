// Framebuffer boot console.
//
// Mirrors the kernel log to a linear RGB framebuffer (provided by the loader as a
// boot-time video mode) using the shared `term` terminal stack: a `term::Term` (the
// grid model plus the framebuffer renderer) drawing onto a `KernelSurface` (the boot
// framebuffer; its writes are visible immediately, so `present` is a no-op).
// It is a mirror, not a replacement: serial output is unchanged and always happens;
// the console is best-effort - skipped entirely if no framebuffer was provided, and
// skipped for a single print if its lock is already held (e.g. a panic that
// interrupted a print), so it can never deadlock the logger.
//
// The boot console and the userspace ConsoleService share one renderer (the `term`
// crate): at display takeover the kernel hands its boot-log text across as logical
// text (SYS_CONSOLE_READLOG) and ConsoleService replays it into VT 1's model, so the
// boot log stays on screen with no second renderer.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};

use term::{Geometry, Raster, Surface, Term, TextSink};

use crate::sync::SpinLock;

// A framebuffer description handed in from the loader's BootInfo.
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

// The kernel's display backend: the boot framebuffer the renderer draws into. Its
// writes land directly in scanout memory and are visible immediately, so `present`
// is a no-op (there is no host compositor to flush to, unlike the virtio-gpu
// backing).
struct KernelSurface {
	raster: Raster,
}

impl Surface for KernelSurface {
	fn raster(&self) -> &Raster {
		&self.raster
	}
	fn present(&self, _x: u32, _y: u32, _w: u32, _h: u32) {}
}

// The boot console: the shared terminal (`term::Term` - the grid model plus the
// framebuffer renderer) drawing onto the boot framebuffer.
struct Console {
	term: Term,
}

// The framebuffer pointer (held inside the Term's Raster) is only ever dereferenced
// under the console lock.
unsafe impl Send for Console {}

static CONSOLE: SpinLock<Option<Console>> = SpinLock::new(None);

// Set once a userspace ConsoleService maps the framebuffer and takes over the
// display: the kernel console then stops drawing (boot-log output still reaches the
// serial port, but the framebuffer belongs to ConsoleService). The grid model is
// kept so its text can still be handed across (boot_log_text / SYS_CONSOLE_READLOG).
static DISABLED: AtomicBool = AtomicBool::new(false);

impl Write for Console {
	fn write_str(&mut self, s: &str) -> fmt::Result {
		for &byte in s.as_bytes() {
			self.term.screen.put_byte(byte);
		}
		Ok(())
	}
}

// Initialize the console from a framebuffer description. A degenerate mode (no
// pixels) leaves the console uninitialized so logging falls back to serial only.
// Allocates the grid model, so it must run after the heap is up.
pub fn init(info: FbInfo) {
	if info.width == 0 || info.height == 0 || info.bytes_per_pixel == 0 {
		return;
	}
	let geometry = Geometry { width: info.width, height: info.height, pitch: info.pitch, bytes_per_pixel: info.bytes_per_pixel, red_shift: info.red_shift, red_size: info.red_size, green_shift: info.green_shift, green_size: info.green_size, blue_shift: info.blue_shift, blue_size: info.blue_size };
	let surface: Box<dyn Surface> = Box::new(KernelSurface { raster: Raster::new(info.addr as u64, &geometry) });
	let term = Term::new(surface, term::SCROLLBACK_ROWS);
	if term.screen.cols() == 0 || term.screen.rows() == 0 {
		return;
	}
	*CONSOLE.lock() = Some(Console { term });
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
			console.term.flush();
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
				console.term.screen.put_byte(byte);
			}
			console.term.flush();
		}
	}
}

// The boot console's log as logical text, for handing the boot log to the userspace
// ConsoleService at display takeover (SYS_CONSOLE_READLOG). Serializes the grid
// model's scrollback and screen to UTF-8 lines (soft-wraps joined, trailing blanks
// trimmed). None when there is no boot console or its lock is held (mid-print).
pub fn boot_log_text() -> Option<Vec<u8>> {
	let mut guard = CONSOLE.try_lock()?;
	let console = guard.as_mut()?;
	let mut sink = TextSink::new();
	sink.capture(&console.term.screen);
	Some(sink.as_bytes().to_vec())
}

// Hand the framebuffer to a userspace ConsoleService: the kernel console stops
// drawing (its boot-log job is done; serial output continues). The grid model is
// kept so boot_log_text can still read it. Called by the framebuffer_map syscall
// when ConsoleService maps the display.
pub fn disable() {
	DISABLED.store(true, Ordering::Relaxed);
}

// Whether the framebuffer has been handed to userspace (so a second framebuffer_map
// is refused - the first mapper owns the display).
pub fn is_disabled() -> bool {
	DISABLED.load(Ordering::Relaxed)
}
