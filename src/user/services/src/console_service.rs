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
// This is the extraction of the kernel framebuffer console into userspace.

#![no_std]
#![no_main]

use rt::*;

use proto::system::{config, network, process};

// The shell's command vocabulary, shared with the shell itself: the line discipline
// completes the command word on Tab, and the shell prints the matches on a double Tab.
mod commands;

extern crate alloc;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

// The shared terminal stack from the `term` crate: the graphics-free grid model (L2,
// `Screen` inside `Term`) and the framebuffer renderer (L3, `Term`) that draws it onto a
// display `Surface`. This service supplies the userspace display backends - the boot
// framebuffer and the virtio-gpu shared backing - and drives `Term`; the kernel boot
// console shares the same `Term`.
use term::{CELL_H, CELL_W, Echo, EchoBuf, Geometry, LD_HIST_MAX, Ld, Raster, RawSink, SCROLLBACK_ROWS, Surface, Term};

// The boot framebuffer the kernel maps directly: its pixel writes are visible immediately,
// so present is a no-op. The fallback display (and the deterministic test path).
struct BootSurface {
	raster: Raster,
}

impl Surface for BootSurface {
	fn raster(&self) -> &Raster {
		&self.raster
	}
	fn present(&self, _x: u32, _y: u32, _w: u32, _h: u32) {}
}

// The virtio-gpu driver's shared backing: pixel writes land in a DMA buffer the driver
// copies to its host scanout on FLUSH, so present queues that FLUSH over the driver's
// display channel.
struct GpuSurface {
	raster: Raster,
	gpu: u64,
}

impl Surface for GpuSurface {
	fn raster(&self) -> &Raster {
		&self.raster
	}
	// Queue a FLUSH carrying the changed rectangle, so the driver transfers only those
	// pixels to the host scanout instead of the whole frame.
	fn present(&self, x: u32, y: u32, w: u32, h: u32) {
		unsafe {
			let mut msg: [u8; 21] = [0u8; 21];
			msg[..5].copy_from_slice(b"FLUSH");
			msg[5..9].copy_from_slice(&x.to_le_bytes());
			msg[9..13].copy_from_slice(&y.to_le_bytes());
			msg[13..17].copy_from_slice(&w.to_le_bytes());
			msg[17..21].copy_from_slice(&h.to_le_bytes());
			send_blocking(self.gpu, &msg, 0);
		}
	}
}

// Build the display backend for a mapped framebuffer: the virtio-gpu backing when a gpu
// channel is given (it presents on FLUSH), else the boot framebuffer (present is a no-op).
fn make_surface(addr: u64, fb: &Framebuffer, gpu: u64) -> Box<dyn Surface> {
	let raster = Raster::new(addr, &geometry(fb));
	if gpu != 0 { Box::new(GpuSurface { raster, gpu }) } else { Box::new(BootSurface { raster }) }
}

// The renderer's `Geometry` for a mapped ABI `Framebuffer`: the pixel format the display
// backends hand to a `Raster`.
fn geometry(fb: &Framebuffer) -> Geometry {
	Geometry { width: fb.width as usize, height: fb.height as usize, pitch: fb.pitch as usize, bytes_per_pixel: fb.bytes_per_pixel as usize, red_shift: fb.red_shift, red_size: fb.red_size, green_shift: fb.green_shift, green_size: fb.green_size, blue_shift: fb.blue_shift, blue_size: fb.blue_size }
}

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

// Clipboard chords the driver collapses to a single private byte (0xC0 / 0xC1, never valid
// UTF-8 nor produced by the layout): Copy (Ctrl+Shift+C / Ctrl+Insert) saves the current
// selection to the clipboard; Paste (Ctrl+Shift+V / Shift+Insert) injects it into the shell.
const CHORD_COPY: u8 = 0xc0;
const CHORD_PASTE: u8 = 0xc1;

// The visual bell holds the inverted screen for this many monotonic ticks (100 Hz, so
// ~100 ms) before restoring it.
const BELL_FLASH_TICKS: u64 = 10;

// The caret blink phase (in 100 Hz ticks): the run loop's periodic wake toggles the
// foreground caret every ~400 ms while the console idles - the classic terminal rate.
const BLINK_PHASE_TICKS: u64 = 40;

// The most serial-mirror backlog the console keeps queued (the kernel's transmit ring
// accepts what it can per wake; the rest waits here). A burst beyond this is hopelessly
// ahead of the baud-paced wire: the oldest pending output is dropped and the gap is
// marked in the mirror.
const SERIAL_PENDING_MAX: usize = 32768;

// What the serial mirror prints in place of output it had to drop (see above).
const SERIAL_GAP_MARKER: &[u8] = b"\n[serial mirror: behind - older output dropped]\n";

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
	// returned by value through the deep spawn_vt call chain on a small (16 kB) user
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
	// This VT's shell working directory, mirrored here from the shell over the control
	// channel (SET_CWD, on startup and after each `cd`). The shell owns the cwd; the copy
	// lets argument Tab completion resolve a relative path against it and list the target
	// directory. Defaults to the system volume until the first SET_CWD arrives.
	cwd: String,
}

// The capabilities ConsoleService holds to spawn a shell for any additional VT: a
// factory connection to each multi-client service, from which it mints a fresh per-VT
// client with `service_connect` / `network.open`. The shell (and any PTY program) is
// loaded through the ProcessService `process` factory, so no init package is held.
struct Factories {
	storage: u64,
	log: u64,
	device: u64,
	process: u64,
	config: u64,
	net: u64,
	time: u64,
	audio: u64,
	session: u64,
	perm: u64,
}

// How long a config read may wait for its reply. A live ConfigService answers in
// one synchronous round-trip, far inside this; the bound exists so VT creation
// never hangs when the supervisor wired the config factory to a mute endpoint (a
// test scenario) - a missed deadline reads as "no answer, use the default".
const CONFIG_WAIT_TICKS: u64 = 100;

// A proto transport with a bounded reply wait (see CONFIG_WAIT_TICKS): send the
// request, wait for the reply until the deadline, and treat a miss as no answer
// instead of blocking forever.
struct DeadlineTransport {
	chan: u64,
	ticks: u64,
}

impl proto::codec::Transport for DeadlineTransport {
	fn call(&mut self, request: &[u8], request_handle: u64) -> Option<(Vec<u8>, u64)> {
		unsafe {
			if !send_blocking(self.chan, request, request_handle) {
				return None;
			}
			if wait(self.chan, clock() + self.ticks) != 0 {
				return None;
			}
			let mut reply: [u8; 4096] = [0u8; 4096];
			match try_recv(self.chan, &mut reply) {
				Polled::Message { len, handle } => Some((reply[..len].to_vec(), handle)),
				_ => None,
			}
		}
	}
}

// Mint a client connection from a serve_multi `factory` under the same bounded
// wait; 0 when the factory does not answer in time.
unsafe fn connect_deadline(factory: u64, ticks: u64) -> u64 {
	unsafe {
		let req: [u8; 2] = CONNECT_OP.to_le_bytes();
		if !send_blocking(factory, &req, 0) {
			return 0;
		}
		if wait(factory, clock() + ticks) != 0 {
			return 0;
		}
		let mut buf: [u8; 16] = [0u8; 16];
		match try_recv(factory, &mut buf) {
			Polled::Message { handle, .. } if handle != 0 => handle,
			_ => 0,
		}
	}
}

// The per-VT terminal policy from the config tree - the scrollback depth and the
// line-editor history depth (`console.scrollback`, `console.history`) - read at
// every VT creation, so a `set` applies to the next VT. The term crate's defaults
// stand when no config client is held or a key does not answer / parse.
fn term_policy(config: u64) -> (usize, usize) {
	if config == 0 {
		return (SCROLLBACK_ROWS, LD_HIST_MAX);
	}
	let mut client = config::Client::new(DeadlineTransport { chan: config, ticks: CONFIG_WAIT_TICKS });
	let mut read = |key: &str, default: usize| -> usize {
		match client.get(key) {
			Some(Ok(value)) => value.parse::<usize>().ok().filter(|&n| n > 0).unwrap_or(default),
			_ => default,
		}
	};
	let scrollback: usize = read("console.scrollback", SCROLLBACK_ROWS);
	let history: usize = read("console.history", LD_HIST_MAX);
	(scrollback, history)
}

// The whole console session: the framebuffer it owns, the kernel keystroke channel, the
// live VTs (vts[fg] is foreground and owns the display), and the spawn capabilities.
struct Console {
	addr: u64,
	fb: Framebuffer,
	// The gpu backing's DmaBuffer handle (0 on the boot framebuffer): kept so a FBNEW
	// reallocation can release the old buffer (close unmaps and frees it) after the
	// VTs swap onto the replacement.
	fb_handle: u64,
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
	// The foreground VT's raw output stream (L1), tapped during one wake for the serial debug
	// mirror and written out AFTER the display present: the emulated serial port is
	// baud-throttled, so mirroring it after presenting keeps a slow serial console from
	// delaying the SPICE/VNC display. A `RawSink` - the same L1 tap a future ssh/`script` would
	// fork the stream into - drained and cleared after each wake.
	serial: RawSink,
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
	// The broker (bootstrap) channel the restartable factories (config, device) are
	// re-resolved over when their service crashed and was restarted: ServiceManager
	// answers a RESOLVE with a connection to the live instance, so a VT opened after
	// the restart still gets live per-VT connections.
	broker: u64,
	// The console's own ConfigService client (0 when the factory never answered),
	// consulted at every VT creation for the terminal policy (`term_policy`).
	config_client: u64,
	// The pointer-forward channel from InputService (0 = no pointer device this boot): raw
	// 6-byte pointer events [x u16 LE][y u16 LE][buttons u8][wheel i8] arrive on it, which
	// `handle_pointer` turns into SGR mouse reports (for a program that enabled tracking)
	// or native selection / scrollback / paste (when no program is tracking the mouse).
	pointer: u64,
	// The console-held clipboard. A mouse selection copies
	// into it (select-to-copy), middle-click pastes it, and a program's OSC 52 sets it.
	clipboard: Vec<u8>,
	// The pointer button bits from the previous event, to detect press / release edges.
	ptr_buttons: u8,
	// Set when the serial mirror had to drop old backlog (SERIAL_PENDING_MAX): the next
	// drain writes the gap marker where the dropped output would have been.
	serial_gap: bool,
	// The Tab-completion vocabulary: the shell builtins plus the system volume's bin/
	// listing (bash's builtins + $PATH), fetched lazily on the first Tab through a fresh
	// storage connection and cached for the session (None until then; an unreachable
	// volume caches the builtins alone).
	vocab: Option<Vec<Vec<u8>>>,
}

// Map the boot framebuffer the kernel hands over (`framebuffer_map`): the display the
// kernel drew the boot log to, whose pixel writes are visible immediately. Returns (pixel
// base, geometry), or None when headless or the display was already handed over. This is
// the only display on the test path, and the surface a gpu takeover hands off from.
unsafe fn map_boot_framebuffer() -> Option<(u64, Framebuffer)> {
	unsafe {
		let mut fb: Framebuffer = Framebuffer::default();
		let addr: i64 = framebuffer_map(&mut fb);
		if sys_is_err(addr as u64) {
			return None;
		}
		Some((addr as u64, fb))
	}
}

// Connect to the virtio-gpu driver: ask for the framebuffer (FB), receive its max
// geometry (the resource extent and pitch), its current display size, and a handle to
// the shared backing it renders into, and map it. Returns (pixel base, max geometry,
// current width, current height), or None on any failure (the caller then uses the boot
// framebuffer). The terminal is sized to the current display but may grow to the max.
unsafe fn gpu_framebuffer(gpu: u64, buf: &mut [u8]) -> Option<(u64, u64, Framebuffer, u32, u32)> {
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
		Some((handle, addr as u64, fb, cur_w, cur_h))
	}
}

// Present the foreground VT's freshly rendered frame to the display: a no-op on the boot
// framebuffer (whose writes are visible immediately), a FLUSH to the gpu driver on the
// virtio-gpu backing carrying just the repainted rectangle. Driven by the surface backend
// the foreground VT renders onto.
unsafe fn present_fg(console: &mut Console) {
	let fg: usize = console.fg;
	if let Some(t) = console.vts[fg].term.as_mut() {
		t.present();
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. receive the whole bootstrap capability set (ended by READY) and take each by
		//    name: VT 1's console + control channels (its shell, spawned by ServiceManager,
		//    holds the other ends), a factory connection per multi-client service - among
		//    them the ProcessService `process` factory it loads and launches each shell
		//    through - and the optional display plumbing: the gpu driver's display channel
		//    and InputService's pointer forward (absent on a headless / pointerless boot,
		//    read as 0).
		let mut caps: CapSet = recv_caps(bootstrap);
		let required = |h: u64| -> u64 {
			if h == 0 {
				fail_bootstrap(bootstrap, b"capability", b"required console capability not granted")
			}
			h
		};
		let client: u64 = required(caps.take(CAP_CLIENT));
		let control: u64 = required(caps.take(CAP_CONTROL));
		let storage: u64 = required(caps.take(CAP_FSTORAGE));
		let log: u64 = required(caps.take(CAP_FLOG));
		let device: u64 = required(caps.take(CAP_FDEVICE));
		let process: u64 = required(caps.take(CAP_FPROCESS));
		let config: u64 = required(caps.take(CAP_FCONFIG));
		let time: u64 = required(caps.take(CAP_FTIME));
		let audio: u64 = required(caps.take(CAP_FAUDIO));
		let session: u64 = required(caps.take(CAP_FSESSION));
		let perm: u64 = required(caps.take(CAP_FPERM));
		let net: u64 = required(caps.take(CAP_FNET));
		let gpu: u64 = caps.take(CAP_GPU);
		let pointer: u64 = caps.take(CAP_POINTER);
		drop(caps);

		// 2. acquire the display backends. The boot framebuffer the kernel hands over holds
		//    the boot log; the virtio-gpu driver's resizable shared backing is the runtime
		//    display when present (it presents on FLUSH and resizes on a host-window change).
		//    New VTs render on the gpu backing when present, else the boot framebuffer; a
		//    headless boot has neither and we still serve input. The framebuffer is the
		//    allocated (resource) geometry; the terminal is sized to the current display,
		//    and the gpu driver reallocates the backing when the display outgrows it
		//    (arriving here as FBNEW).
		let boot: Option<(u64, Framebuffer)> = map_boot_framebuffer();
		let gpu_disp: Option<(u64, u64, Framebuffer, u32, u32)> = if gpu != 0 { gpu_framebuffer(gpu, &mut buf) } else { None };
		// 0 = no present (the boot framebuffer, or a gpu whose connect failed).
		let gpu: u64 = if gpu_disp.is_some() { gpu } else { 0 };
		let (fb_handle, addr, fb, cur_w, cur_h): (u64, u64, Framebuffer, u32, u32) = match (gpu_disp, boot) {
			(Some((gh, ga, gf, gw, ghh)), _) => (gh, ga, gf, gw, ghh),
			(None, Some((ba, bf))) => (0, ba, bf, bf.width, bf.height),
			(None, None) => (0, 0, Framebuffer::default(), 0, 0),
		};
		let has_fb: bool = gpu_disp.is_some() || boot.is_some();
		// The console's own ConfigService client (minted under the bounded wait) and
		// VT 1's terminal policy: the scrollback and history depths are the config
		// tree's, re-read at every later VT creation.
		let config_client: u64 = connect_deadline(config, CONFIG_WAIT_TICKS);
		let (vt_scrollback, vt_history): (usize, usize) = term_policy(config_client);
		// Build VT 1's terminal directly on the runtime display surface (the gpu backing when
		// present, else the boot framebuffer), then seed its grid model with the kernel boot
		// log. The kernel and this service share the same `term` stack, so the kernel hands
		// the boot log across as text (SYS_CONSOLE_READLOG) and we replay it into the model:
		// the boot log stays on screen - and in the scrollback - after the gpu and this
		// renderer take over, with no second renderer and no pixel-level handoff.
		let term: Option<Term> = if has_fb {
			let mut t = Term::new(make_surface(addr, &fb, gpu), vt_scrollback);
			t.resize(cur_w as usize / CELL_W, cur_h as usize / CELL_H);
			let mut log: Vec<u8> = alloc::vec![0u8; 16384];
			let n: i64 = console_readlog(&mut log);
			if n > 0 {
				for &b in &log[..n as usize] {
					t.screen.put_byte(b);
				}
				t.screen.put_byte(b'\n');
			}
			t.flush();
			Some(t)
		} else {
			None
		};

		// 3. report in to the supervisor.
		send_blocking(bootstrap, b"ConsoleService: online", 0);

		// 4. run the multiplexing terminal loop, starting with VT 1.
		let facs: Factories = Factories { storage, log, device, process, config, net, time, audio, session, perm };
		let mut console: Console = Console { addr, fb, fb_handle, has_fb, gpu, cur_w, cur_h, input: 0, serial: RawSink::new(), vts: alloc::vec![Vt { term, client, control, fg_proc: None, ld: Box::new(Ld::new(vt_history)), master: 0, cwd: String::from("vol://system") }], fg: 0, ptys: Vec::new(), facs, broker: bootstrap, config_client, pointer, clipboard: Vec::new(), ptr_buttons: 0, serial_gap: false, vocab: None };
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
		// The per-message output buffer: sized to the transport ceiling (4096, the same
		// bound every service reply and the shell's relay use), so a program's largest
		// single print renders whole - the kernel truncates a message to the receiver's
		// buffer silently.
		let mut out: [u8; 4096] = [0u8; 4096];
		// The wait set is built per iteration, sized by the live VT / PTY sets - PTYs
		// grow on demand, so the set has no fixed bound (the kernel's wait_any sanity
		// bound is far above any real console).
		let mut waits: Vec<u64> = Vec::new();
		// present the initial banner (the foreground term was rendered in __user_main).
		present_fg(console);
		loop {
			// wait set: the keyboard channel (index 0), then each display VT's data channel
			// and its control channel interleaved (data at 1 + 2*i, control at 2 + 2*i),
			// then the gpu driver's display channel (when present, it sends RESIZE on a
			// host-window change), then each program-hosted PTY's slave-data, slave-control,
			// and master channels interleaved (data / control / master at pty_base + 3*j),
			// then the pointer channel (when present) in the last slot.
			waits.clear();
			waits.push(console.input);
			let nv: usize = console.vts.len();
			for i in 0..nv {
				waits.push(console.vts[i].client);
				waits.push(console.vts[i].control);
			}
			let gpu_idx: usize = 1 + 2 * nv;
			let have_gpu: bool = console.gpu != 0;
			if have_gpu {
				waits.push(console.gpu);
			}
			let pty_base: usize = gpu_idx + have_gpu as usize;
			let np: usize = console.ptys.len();
			for j in 0..np {
				waits.push(console.ptys[j].client);
				waits.push(console.ptys[j].control);
				waits.push(console.ptys[j].master);
			}
			// The pointer channel (when present) is the last wait slot: raw pointer events
			// from InputService drive selection, scrollback, and SGR mouse reports.
			let ptr_idx: usize = pty_base + 3 * np;
			let have_pointer: bool = console.pointer != 0;
			if have_pointer {
				waits.push(console.pointer);
			}
			// Block (~0% CPU) until a channel is ready - a keystroke, VT output, a gpu
			// RESIZE, or a program-hosted PTY's traffic - or until the caret's blink phase
			// elapses. The blink deadline is a housekeeping wake (WAIT_PERIODIC): it recurs
			// forever, so it must never count as pending progress for the scheduler's boot
			// driver (or the kernel tests). Headless (no framebuffer) there is no caret to
			// blink, so the wait has no deadline at all.
			let ready: i64 = if console.has_fb { wait_any_periodic(&waits, clock() + BLINK_PHASE_TICKS) } else { wait_any(&waits, 0) };
			if ready == ERR_TIMED_OUT {
				// the idle blink tick: toggle the foreground caret's phase. Every flush
				// repaints the caret, so activity keeps it solid and the phase restarts
				// with each event (the deadline above is re-armed per iteration).
				blink_fg(console);
				continue;
			}
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
				} else if have_pointer && r == ptr_idx {
					// a raw pointer event from InputService: SGR report, selection, or scrollback.
					loop {
						match try_recv(console.pointer, &mut out) {
							Polled::Message { len, .. } => handle_pointer(console, &out[..len]),
							Polled::Empty => break,
							Polled::Closed => {
								console.pointer = 0;
								break;
							}
						}
					}
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
				present_fg(console);
				drain_serial(console);
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
		let mut raw_req: Option<bool> = None;
		let mut echo_req: Option<bool> = None;
		let mut clip_req: Option<Vec<u8>> = None;
		if let Some(t) = console.vts[vi].term.as_mut() {
			for &b in bytes {
				t.screen.put_byte(b);
			}
			// Pick up any tty mode change the program asked for in this output.
			raw_req = t.screen.take_tty_raw_req();
			echo_req = t.screen.take_tty_echo_req();
			// Pick up an OSC 52 clipboard-set the program emitted in this output.
			clip_req = t.screen.take_clipboard_set();
			let bell: bool = t.screen.take_bell();
			if fg {
				t.flush();
				// BEL: invert the foreground screen briefly, then restore. A one-off timed
				// wait (woken early by a keystroke), not a perpetual re-arm, so it never
				// stalls the cooperative boot driver.
				if bell {
					t.draw_inverted();
					t.present();
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
		// Adopt an OSC 52 clipboard-set into the console-held clipboard (a program sets
		// the selection, a later middle-click pastes it).
		if let Some(text) = clip_req {
			console.clipboard = text;
		}
		if fg {
			// Tap the raw output stream (L1) into the serial mirror, alongside the L2 model above;
			// the session loop drains it after the present, bounded by what the kernel's transmit
			// ring accepts, so the baud-throttled serial port never delays the display (see `run`).
			// A backlog past the cap drops its oldest bytes - the newest output is the valuable
			// part of a debug mirror - and the gap is marked on the next drain.
			console.serial.feed(bytes);
			let pending: usize = console.serial.as_bytes().len();
			if pending > SERIAL_PENDING_MAX {
				console.serial.consume(pending - SERIAL_PENDING_MAX);
				console.serial_gap = true;
			}
		}
	}
}

// Drain the serial mirror's backlog into the kernel's transmit ring, consuming exactly
// what the ring accepted and leaving the rest for a later wake (every event drains, and
// the caret-blink tick bounds the wait between drains) - so a burst is paced to the
// wire instead of truncated, and this thread never blocks on the baud-paced UART. A
// marked gap (dropped backlog) is announced ahead of the remaining stream, best-effort.
unsafe fn drain_serial(console: &mut Console) {
	unsafe {
		if console.serial.is_empty() {
			return;
		}
		if console.serial_gap {
			debug_write(SERIAL_GAP_MARKER);
			console.serial_gap = false;
		}
		loop {
			let accepted: usize = debug_write(console.serial.as_bytes());
			console.serial.consume(accepted);
			if accepted == 0 || console.serial.is_empty() {
				break;
			}
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
			} else if b == CHORD_COPY {
				// Ctrl+Shift+C / Ctrl+Insert: copy the current selection to the clipboard.
				copy_selection(console);
			} else if b == CHORD_PASTE {
				// Ctrl+Shift+V / Shift+Insert: paste the clipboard into the foreground shell.
				let fg: usize = console.fg;
				let bracketed: bool = console.vts[fg].term.as_ref().is_some_and(|t| t.screen.bracketed_paste());
				paste_clipboard(console, bracketed);
			} else {
				// any other keystroke returns the foreground VT to its live screen first.
				snap_fg_live(console);
				feed_key(console, b);
			}
		}
	}
}

// Copy the foreground VT's current mouse selection to the console clipboard (right-click
// or the Ctrl+Shift+C / Ctrl+Insert chord); a no-op when nothing is selected.
unsafe fn copy_selection(console: &mut Console) {
	let fg: usize = console.fg;
	let text: Vec<u8> = match console.vts[fg].term.as_ref() {
		Some(t) => t.screen.selection_text(),
		None => Vec::new(),
	};
	if !text.is_empty() {
		console.clipboard = text;
	}
}

// Feed one keystroke to the foreground VT. In cooked mode the line discipline edits +
// echoes it and, on Enter, ships the whole line (plus newline) to the shell; in raw mode
// the byte passes straight through.
unsafe fn feed_key(console: &mut Console, b: u8) {
	unsafe {
		let vocab: Vec<Vec<u8>> = completion_vocab(console, b);
		let fg: usize = console.fg;
		feed_tty(&mut console.vts[fg], b, &vocab);
	}
}

// The Tab-completion vocabulary for the segment under the cursor. On the command word (the
// first token) it is the shell builtins plus the system volume's bin/ listing (bash's
// builtins + $PATH), fetched lazily on the first Tab and cached for the session. On a later
// token it is the entries of the directory the partial path names, resolved against this
// VT's cwd and listed fresh each time (a directory's contents vary), with a trailing '/' on
// the sub-directories so the line discipline can keep a directory open for its sub-path. Any
// key other than Tab answers an empty list without any IPC.
unsafe fn completion_vocab(console: &mut Console, b: u8) -> Vec<Vec<u8>> {
	unsafe {
		if b != b'\t' {
			return Vec::new();
		}
		let fg: usize = console.fg;
		let vt: &Vt = &console.vts[fg];
		let line: &[u8] = &vt.ld.line[..vt.ld.len];
		// The token under the cursor is the run back to the last space; the command word
		// (no space before it) completes over the runnable programs, a later token over a
		// directory's entries.
		let tok_start: usize = line.iter().rposition(|&c: &u8| c == b' ').map_or(0, |p: usize| p + 1);
		if tok_start == 0 {
			return command_vocab(console);
		}
		path_vocab(console, fg, tok_start)
	}
}

// The command-word vocabulary: the shell builtins plus the system volume's bin/ listing,
// cached for the session (the bin/ set is seeded at format time and static per boot), so the
// input path pays this IPC round-trip once.
unsafe fn command_vocab(console: &mut Console) -> Vec<Vec<u8>> {
	unsafe {
		if console.vocab.is_none() {
			let mut names: Vec<Vec<u8>> = Vec::new();
			if let Some(storage) = service_connect(console.facs.storage) {
				let mut client = proto::system::volume::Client::new(ChannelTransport { chan: storage });
				if let Some(consumer) = client.list("vol://system/bin") {
					names = drain_stream(consumer, proto::system::volume::list_read).into_iter().map(|f| f.name.into_bytes()).collect();
				}
				close(storage);
			}
			for &builtin in commands::BUILTINS {
				names.push(builtin.as_bytes().to_vec());
			}
			names.sort();
			names.dedup();
			console.vocab = Some(names);
		}
		console.vocab.clone().unwrap_or_default()
	}
}

// The path-argument vocabulary for a later token: resolve the directory the partial path
// names (everything up to its last '/', or the cwd when it has none yet) against this VT's
// cwd, list that directory, and answer its entries - a trailing '/' on each sub-directory so
// the line discipline keeps it open for the sub-path. The line discipline filters these by
// the trailing path segment, exactly as it filters the command word. Only the system volume
// is offered (ConsoleService holds only its storage client); any other volume answers empty.
unsafe fn path_vocab(console: &mut Console, fg: usize, tok_start: usize) -> Vec<Vec<u8>> {
	unsafe {
		let vt: &Vt = &console.vts[fg];
		let line_len: usize = vt.ld.len;
		let token: Vec<u8> = vt.ld.line[tok_start..line_len].to_vec();
		let cwd: String = vt.cwd.clone();
		let dir_arg: &[u8] = match token.iter().rposition(|&c: &u8| c == b'/') {
			Some(s) => &token[..s],
			None => b"",
		};
		let target: String = match proto::path::resolve(&cwd, dir_arg) {
			Some(t) => t,
			None => return Vec::new(),
		};
		if target != "vol://system" && !target.starts_with("vol://system/") {
			return Vec::new();
		}
		let mut names: Vec<Vec<u8>> = Vec::new();
		if let Some(storage) = service_connect(console.facs.storage) {
			let mut client = proto::system::volume::Client::new(ChannelTransport { chan: storage });
			if let Some(consumer) = client.list(&target) {
				for f in drain_stream(consumer, proto::system::volume::list_read) {
					let mut name: Vec<u8> = f.name.into_bytes();
					if f.r#type == proto::system::FileType::Dir {
						name.push(b'/');
					}
					names.push(name);
				}
			}
			close(storage);
		}
		names
	}
}

// Feed one input byte to a terminal's line discipline - shared by the foreground display
// VT (the keyboard) and a program-hosted PTY (the host's master channel). In cooked mode
// the discipline edits + echoes the byte and, on Enter, ships the whole line (plus a
// newline) to the slave program; in raw mode the byte passes straight through. The echo
// goes wherever the terminal's master is: a display VT mirrors it to the serial port (and
// renders live into its grid), a PTY sends it back out its master channel so the host
// (e.g. a remote terminal over ssh) sees what was typed.
unsafe fn feed_tty(vt: &mut Vt, b: u8, vocab: &[Vec<u8>]) {
	unsafe {
		let client: u64 = vt.client;
		// A foreground job owns the tty: the signal keys become signals to it (the tty's
		// ISIG behaviour) and are consumed here. Every other byte flows on through the line
		// discipline below to the job's stdin, so an interactive foreground tool reads its
		// input edited + echoed exactly as the shell reads keystrokes at its prompt.
		if let Some(proc) = vt.fg_proc {
			match b {
				0x03 => {
					// Ctrl+C: interrupt. The job terminates, its completion channel closes,
					// and the shell's run_foreground returns to the prompt.
					signal(proc, SIG_INT);
					tty_echo(vt, b"^C\n");
					return;
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
					return;
				}
				0x1c => {
					// Ctrl+\: terminate.
					signal(proc, SIG_TERM);
					tty_echo(vt, b"^\\\n");
					return;
				}
				_ => {} // any other byte: fall through to the line discipline (the job's stdin)
			}
		}
		if !vt.ld.cooked {
			send_blocking(client, &[b], 0);
			return;
		}
		let submitted: bool;
		let ser: EchoBuf;
		{
			let mut echo: Echo = Echo { term: vt.term.as_mut(), ser: EchoBuf::new() };
			submitted = vt.ld.feed(b, vocab, &mut echo);
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
			if vt.ld.relist {
				// A double Tab found several completions: hand the unfinished line to the
				// program marked with a leading tab (a cooked line can never contain one),
				// so it prints the matches and re-draws the prompt. The buffer is NOT
				// committed - the operator keeps typing right where they were. Only the
				// prompt owner gets the marker; a foreground job's stdin never sees it.
				vt.ld.relist = false;
				if vt.fg_proc.is_none() {
					let n: usize = vt.ld.len;
					let mut out: Vec<u8> = Vec::with_capacity(n + 1);
					out.push(b'\t');
					out.extend_from_slice(&vt.ld.line[..n]);
					send_blocking(client, &out, 0);
				}
			} else if vt.ld.eof {
				// Ctrl+D on an empty line: deliver a zero-byte read (EOF) so the shell
				// logs out, the way a tty signals end-of-input.
				vt.ld.commit();
				send_blocking(client, &[], 0);
			} else {
				let n: usize = vt.ld.len;
				// build the line + newline on the heap: up to LD_LINE_MAX + 1 (4 kB),
				// too big for this service's stack.
				let mut out: Vec<u8> = Vec::with_capacity(n + 1);
				out.extend_from_slice(&vt.ld.line[..n]);
				out.push(b'\n');
				vt.ld.commit();
				send_blocking(client, &out, 0);
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
		let mut cbuf: [u8; 256] = [0u8; 256];
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
				} else if let Some(path) = msg.strip_prefix(b"SET_CWD") {
					// The shell reports its working directory (on startup and after `cd`), so
					// argument Tab completion can resolve a relative path against it.
					if let Ok(s) = core::str::from_utf8(path) {
						console.vts[vi].cwd.clear();
						console.vts[vi].cwd.push_str(s);
					}
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
			let vocab: Vec<Vec<u8>> = completion_vocab(console, b);
			feed_tty(&mut console.ptys[pj], b, &vocab);
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
		let mut buf: [u8; 64] = [0u8; 64];
		let (len, handle): (usize, u64) = match recv_blocking(console.gpu, &mut buf) {
			Received::Message { len, handle } => (len, handle),
			Received::Closed => {
				console.gpu = 0;
				return;
			}
		};
		// FBNEW: the driver reallocated the backing for a display that outgrew it -
		// swap every display VT onto the new buffer, then release the old one.
		if len >= 5 && &buf[..5] == b"FBNEW" && handle != 0 {
			handle_gpu_realloc(console, &buf[..len], handle);
			return;
		}
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

// Adopt the driver's reallocated backing (FBNEW: the new allocated geometry, the
// display size, and the new buffer's handle): map it, swap every display VT's
// renderer onto it (the grid models survive; each repaints in full on its next
// flush), reflow to the new display, and release the old mapping by closing our old
// handle (the driver already dropped its own, so the close frees the buffer).
unsafe fn handle_gpu_realloc(console: &mut Console, msg: &[u8], handle: u64) {
	unsafe {
		let fb_len: usize = core::mem::size_of::<Framebuffer>();
		if msg.len() < 5 + fb_len + 8 {
			close(handle);
			return;
		}
		let fb: Framebuffer = (msg[5..].as_ptr() as *const Framebuffer).read_unaligned();
		let new_w: u32 = u32::from_le_bytes([msg[5 + fb_len], msg[5 + fb_len + 1], msg[5 + fb_len + 2], msg[5 + fb_len + 3]]);
		let new_h: u32 = u32::from_le_bytes([msg[5 + fb_len + 4], msg[5 + fb_len + 5], msg[5 + fb_len + 6], msg[5 + fb_len + 7]]);
		let addr: i64 = dma_buffer_map(handle);
		if sys_is_err(addr as u64) {
			close(handle);
			return;
		}
		let old_handle: u64 = console.fb_handle;
		console.addr = addr as u64;
		console.fb = fb;
		console.fb_handle = handle;
		console.cur_w = new_w;
		console.cur_h = new_h;
		let cols: usize = new_w as usize / CELL_W;
		let rows: usize = new_h as usize / CELL_H;
		let n: usize = console.vts.len();
		for vi in 0..n {
			if let Some(t) = console.vts[vi].term.as_mut() {
				t.set_surface(make_surface(console.addr, &console.fb, console.gpu));
			}
			resize_vt(console, vi, cols, rows);
		}
		// the run loop presents the foreground right after this handler, pushing the
		// full repaint (the whole grid went dirty with the surface swap) onto the new
		// backing - the old buffer is only closed once nothing renders into it.
		if old_handle != 0 {
			close(old_handle);
		}
	}
}

// Toggle the foreground VT's caret blink phase, presenting only when a pixel actually
// changed (the terminal skips the toggle while scrolled back or with the cursor hidden).
// Every flush repaints the caret, so the caret is solid while typing or under output and
// blinks while the console is idle.
fn blink_fg(console: &mut Console) {
	let vi: usize = console.fg;
	if let Some(t) = console.vts[vi].term.as_mut() {
		if t.blink_caret() {
			t.present();
		}
	}
}

// Open a new virtual terminal: spawn a fully-capable shell over its own per-VT service
// connections, make it foreground, and repaint. A no-op when headless; the VT set grows
// on demand (a VT's cost is its grid, paid only when opened) - never a fixed cap.
unsafe fn create_vt(console: &mut Console) {
	unsafe {
		if !console.has_fb {
			return;
		}
		let broker: u64 = console.broker;
		if let Some(vt) = spawn_vt(&mut console.facs, broker, console.config_client, console.addr, &console.fb, console.gpu, console.cur_w, console.cur_h) {
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

// Handle one raw pointer event from InputService: [x u16 LE][y u16 LE][buttons u8][wheel i8].
// When the foreground program enabled mouse tracking (DECSET ?1000 / ?1002 / ?1003), the
// event is translated into SGR mouse reports and delivered to the program (best-effort: a
// program that is not reading drops them rather than stalling the console). Otherwise the
// console drives it natively: the wheel pages the scrollback, click-drag selects a range
// (copied to the clipboard on release), and middle-click pastes the clipboard (bracketed
// when the program asked for ?2004).
unsafe fn handle_pointer(console: &mut Console, msg: &[u8]) {
	unsafe {
		if msg.len() < 6 {
			return;
		}
		let fg: usize = console.fg;
		let x: u32 = u16::from_le_bytes([msg[0], msg[1]]) as u32;
		let y: u32 = u16::from_le_bytes([msg[2], msg[3]]) as u32;
		let buttons: u8 = msg[4];
		let wheel: i8 = msg[5] as i8;
		let prev: u8 = console.ptr_buttons;
		console.ptr_buttons = buttons;
		// The foreground VT's grid geometry and its mouse / paste modes.
		let (cols, rows, tracking, sgr, motion, anymotion, bracket): (usize, usize, bool, bool, bool, bool, bool) = match console.vts[fg].term.as_ref() {
			Some(t) => (t.screen.cols(), t.screen.rows(), t.screen.mouse_tracking(), t.screen.mouse_sgr(), t.screen.mouse_report_motion(), t.screen.mouse_any_motion(), t.screen.bracketed_paste()),
			None => return,
		};
		if cols == 0 || rows == 0 {
			return;
		}
		// Map the normalized 0..0x10000 position onto the 0-based viewport cell grid.
		let col: usize = ((x as usize * cols) / 0x1_0000).min(cols - 1);
		let row: usize = ((y as usize * rows) / 0x1_0000).min(rows - 1);
		if tracking {
			// A program owns the mouse: hide the console's own text cursor while it does.
			if let Some(t) = console.vts[fg].term.as_mut() {
				if t.screen.set_mouse(None) {
					t.flush();
				}
			}
			present_fg(console);
			pointer_report(console, fg, col, row, buttons, prev, wheel, sgr, motion, anymotion);
			return;
		}
		// Native console handling: no program is tracking the mouse. Track the text mouse cursor
		// (an inverted block on the cell under the pointer) and drive the wheel / selection.
		let left_now: bool = buttons & 1 != 0;
		let left_was: bool = prev & 1 != 0;
		let mid_now: bool = buttons & 4 != 0;
		let mid_was: bool = prev & 4 != 0;
		let right_now: bool = buttons & 2 != 0;
		let right_was: bool = prev & 2 != 0;
		if let Some(t) = console.vts[fg].term.as_mut() {
			t.screen.set_mouse(Some((col, row)));
			if wheel != 0 {
				// Route the wheel to the scrollback view (three lines per notch).
				if wheel > 0 {
					t.screen.scroll_view_up_by(3);
				} else {
					t.screen.scroll_view_down_by(3);
				}
			} else if left_now && !left_was {
				// Press: anchor a fresh selection at the cell under the pointer.
				t.screen.selection_begin(col, row);
			} else if left_now && left_was {
				// Drag: extend the selection to the cell under the pointer.
				t.screen.selection_extend(col, row);
			}
			t.flush();
		}
		present_fg(console);
		if !left_now && left_was {
			// Release: copy the selected text to the clipboard (select-to-copy). A bare click
			// (no drag, so nothing selected) clears the transient highlight instead.
			let text: Vec<u8> = match console.vts[fg].term.as_ref() {
				Some(t) => t.screen.selection_text(),
				None => Vec::new(),
			};
			if text.is_empty() {
				if let Some(t) = console.vts[fg].term.as_mut() {
					t.screen.selection_clear();
					t.flush();
				}
				present_fg(console);
			} else {
				console.clipboard = text;
			}
		}
		if mid_now && !mid_was {
			// Middle-click: paste the clipboard (bracketed when the program asked for ?2004).
			paste_clipboard(console, bracket);
		}
		if right_now && !right_was {
			// Right-click: copy the current selection to the clipboard (select-then-right-click).
			copy_selection(console);
		}
	}
}

// Translate a pointer event into SGR mouse reports for a tracking program. Only the SGR
// encoding (?1006) is produced; without it the report is dropped (the legacy X10 byte
// encoding is not emitted). The button press / release edges are always reported; a wheel
// tick reports as button 64 (up) / 65 (down); a drag (button held) reports under ?1002 and
// any bare motion under ?1003 (Cb + 32). Reports are best-effort (try_send), so a program
// that is not draining its input drops them rather than stalling the console loop.
unsafe fn pointer_report(console: &mut Console, fg: usize, col: usize, row: usize, buttons: u8, prev: u8, wheel: i8, sgr: bool, motion: bool, anymotion: bool) {
	unsafe {
		if !sgr {
			return;
		}
		let client: u64 = console.vts[fg].client;
		let cx: usize = col + 1;
		let cy: usize = row + 1;
		if wheel != 0 {
			let cb: usize = if wheel > 0 { 64 } else { 65 };
			send_sgr(client, cb, cx, cy, true);
			return;
		}
		// Button edges (bit 0 left -> Cb 0, bit 1 right -> Cb 2, bit 2 middle -> Cb 1).
		for &(bit, code) in &[(1u8, 0usize), (4u8, 1usize), (2u8, 2usize)] {
			let now: bool = buttons & bit != 0;
			let was: bool = prev & bit != 0;
			if now && !was {
				send_sgr(client, code, cx, cy, true);
			} else if !now && was {
				send_sgr(client, code, cx, cy, false);
			}
		}
		// Motion (no button change this event): a drag under ?1002, any motion under ?1003.
		if buttons == prev {
			let any_button: bool = buttons & 0b111 != 0;
			if (motion && any_button) || anymotion {
				let base: usize = if buttons & 1 != 0 {
					0
				} else if buttons & 4 != 0 {
					1
				} else if buttons & 2 != 0 {
					2
				} else {
					3
				};
				send_sgr(client, 32 + base, cx, cy, true);
			}
		}
	}
}

// Send one SGR mouse report to a tracking program: ESC [ < Cb ; Cx ; Cy followed by M for a
// press / motion or m for a release, with 1-based cell coordinates. Best-effort: a full or
// closed input channel drops the report rather than blocking the console.
unsafe fn send_sgr(client: u64, cb: usize, cx: usize, cy: usize, press: bool) {
	unsafe {
		let mut buf: [u8; 24] = [0u8; 24];
		let mut n: usize = 0;
		buf[n] = 0x1b;
		n += 1;
		buf[n] = b'[';
		n += 1;
		buf[n] = b'<';
		n += 1;
		n += write_dec(&mut buf[n..], cb);
		buf[n] = b';';
		n += 1;
		n += write_dec(&mut buf[n..], cx);
		buf[n] = b';';
		n += 1;
		n += write_dec(&mut buf[n..], cy);
		buf[n] = if press { b'M' } else { b'm' };
		n += 1;
		try_send(client, &buf[..n], 0);
	}
}

// Write `v` as ASCII decimal into `buf` and return the number of bytes written.
fn write_dec(buf: &mut [u8], v: usize) -> usize {
	let mut tmp: [u8; 20] = [0u8; 20];
	let mut i: usize = 0;
	let mut n: usize = v;
	loop {
		tmp[i] = b'0' + (n % 10) as u8;
		i += 1;
		n /= 10;
		if n == 0 {
			break;
		}
	}
	for j in 0..i {
		buf[j] = tmp[i - 1 - j];
	}
	i
}

// Paste the console-held clipboard into the foreground VT (middle-click).
// When the program asked for bracketed paste (?2004) the content is wrapped in
// ESC [ 200 ~ ... ESC [ 201 ~ and sent straight to the program, so it can tell a paste from
// typed input; otherwise the bytes are fed through the line discipline as if typed (so a
// paste at the prompt enters the line editor and echoes). A no-op with an empty clipboard.
unsafe fn paste_clipboard(console: &mut Console, bracketed: bool) {
	unsafe {
		if console.clipboard.is_empty() {
			return;
		}
		// A paste targets the live screen, so leave any scrollback view first.
		snap_fg_live(console);
		let fg: usize = console.fg;
		if bracketed {
			let client: u64 = console.vts[fg].client;
			send_blocking(client, b"\x1b[200~", 0);
			send_blocking(client, &console.clipboard, 0);
			send_blocking(client, b"\x1b[201~", 0);
		} else {
			let clip: Vec<u8> = console.clipboard.clone();
			for &b in &clip {
				feed_key(console, b);
			}
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
		close(console.vts[vi].client);
		close(console.vts[vi].control);
		if let Some(p) = console.vts[vi].fg_proc.take() {
			close(p);
		}
		// The last VT's shell exiting must NOT halt the machine: reload a fresh shell on
		// it (a logout returns a clean login prompt, like a real console). A secondary VT
		// is removed instead, returning the operator to another VT (its still-live shell).
		if console.vts.len() <= 1 {
			if reload_vt(console, vi) {
				repaint(console);
				return;
			}
			// Reloading failed (out of channels, or the shell binary is gone): there is
			// nothing left to serve, so the console genuinely exits.
			exit();
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

// Reload a fresh shell onto an existing VT (its grid stays; the old shell's channels are
// already closed by the caller). Mint a new console + control channel pair, spawn a
// core-capable shell over them, swap the VT onto them, clear the grid and nudge the first
// prompt. Returns false if the shell could not be spawned. This is what a logout on the
// primary VT does instead of tearing the session down - the reloaded shell is
// core-capable (ConsoleService mints per-VT service connections, minus the few
// single-client capabilities it cannot proxy), the same set every non-primary VT gets.
unsafe fn reload_vt(console: &mut Console, vi: usize) -> bool {
	unsafe {
		let (vt_service, vt_client): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return false,
		};
		let (control_console, control_shell): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => {
				close(vt_service);
				close(vt_client);
				return false;
			}
		};
		let broker: u64 = console.broker;
		if !spawn_shell(&mut console.facs, broker, vt_client, control_shell) {
			close(vt_service);
			close(vt_client);
			close(control_console);
			close(control_shell);
			return false;
		}
		console.vts[vi].client = vt_service;
		console.vts[vi].control = control_console;
		console.vts[vi].fg_proc = None;
		if let Some(t) = console.vts[vi].term.as_mut() {
			t.screen.clear();
		}
		// nudge the fresh shell to print its first prompt (an empty line reprompts).
		send_blocking(vt_service, b"\n", 0);
		true
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

// Spawn a core-capable shell over the given console + control channels (the shell's
// ends): mint a fresh per-session client from each service factory, spawn the shell ELF,
// hand it its capability set in the order it expects (STORAGE, MEDIA, ISO, UDF, LOG,
// DEVICE, PROCESS, CONFIG, NET, TIME, AUDIO, INPUT, GRAPH, PERM, RESOURCE, CONSOLE,
// CONTROL), wait for its "online" report, then release its bootstrap + Process handle. The extended capabilities
// (the media / iso / udf volumes, input, graph, perm, resource) are sent as 0 - a
// non-primary VT cannot mint them per session (input / graph are single-client, the rest
// are not proxied here) - so the shell boots core-capable and the dependent command
// reports the service unavailable. The tags are still sent (with handle 0) so the shell
// stays in positional sync with ServiceManager's primary-VT order. The terminal's
// liveness is tracked solely by its console channel closing on exit; holding the Process
// handle would pin the shell's handle table (and that channel) alive, so the terminal
// could never be reaped when the shell logs out or exits. Shared by spawn_vt (a display
// VT) and open_pty (a program-hosted PTY).
unsafe fn spawn_shell(facs: &mut Factories, broker: u64, shell_console: u64, shell_control: u64) -> bool {
	unsafe {
		let storage: u64 = match service_connect(facs.storage) {
			Some(h) => h,
			None => return false,
		};
		let log: u64 = match service_connect(facs.log) {
			Some(h) => h,
			None => return false,
		};
		// The config and device factories re-resolve through the broker when dead: their
		// services restart transparently, so a VT opened after such a restart still gets
		// live connections (the other factories' services do not restart yet).
		let device: u64 = match connect_or_resolve(&mut facs.device, broker, CAP_DEVICE) {
			Some(h) => h,
			None => return false,
		};
		let process: u64 = match service_connect(facs.process) {
			Some(h) => h,
			None => return false,
		};
		let config: u64 = match connect_or_resolve(&mut facs.config, broker, CAP_CONFIG) {
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
		// A fresh per-VT session: this VT's shell owns it and keeps its cwd for the VT's
		// lifetime (the VT is torn down on logout, so there is no shell restart to outlive).
		let session: u64 = match service_connect(facs.session) {
			Some(h) => h,
			None => return false,
		};
		// A fresh per-VT PermissionManager client: this VT's shell launches commands as
		// governed processes and drives its `perm` command through it, just like VT 1.
		let perm: u64 = match service_connect(facs.perm) {
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
		// Load and start the shell through ProcessService (the sole process-creation
		// mechanism), which reads its binary from the system volume's `bin/`. The child's
		// end of its bootstrap channel is handed over as the launch bootstrap; a dedicated
		// launcher connection to ProcessService is minted for this one call.
		let launcher: u64 = match service_connect(facs.process) {
			Some(h) => h,
			None => return false,
		};
		let started = match process::Client::new(ChannelTransport { chan: launcher }).launch("shell", &boot_child) {
			Some(Ok(s)) => s,
			_ => {
				close(launcher);
				return false;
			}
		};
		close(launcher);
		let shell_proc: u64 = started.task;
		// The named capability set, ended by READY: the shell takes each by name, so the
		// capabilities a non-primary VT does not get are simply not sent - no placeholder
		// messages keeping an order.
		send_blocking(boot_parent, CAP_STORAGE, storage);
		send_blocking(boot_parent, CAP_LOG, log);
		send_blocking(boot_parent, CAP_DEVICE, device);
		send_blocking(boot_parent, CAP_PROCESS, process);
		send_blocking(boot_parent, CAP_CONFIG, config);
		send_blocking(boot_parent, CAP_NET, net_client);
		send_blocking(boot_parent, CAP_TIME, time);
		send_blocking(boot_parent, CAP_AUDIO, audio);
		send_blocking(boot_parent, CAP_PERM, perm);
		// This VT's session.
		send_blocking(boot_parent, CAP_SESSION, session);
		send_blocking(boot_parent, CAP_CONSOLE, shell_console);
		send_blocking(boot_parent, CAP_CONTROL, shell_control);
		send_ready(boot_parent);
		// wait for the shell to report in, then drop its bootstrap.
		let mut rbuf: [u8; 32] = [0u8; 32];
		if let Received::Closed = recv_blocking(boot_parent, &mut rbuf) {
			close(boot_parent);
			close(shell_proc);
			return false;
		}
		close(boot_parent);
		close(shell_proc);
		true
	}
}

// Open one VT's shell: create the VT's console + control channels, spawn a fully-capable
// shell over them, nudge it to print its first prompt, and return the VT (its cleared grid
// + the service ends of those channels). None on any failure.
unsafe fn spawn_vt(facs: &mut Factories, broker: u64, config_client: u64, addr: u64, fb: &Framebuffer, gpu: u64, cur_w: u32, cur_h: u32) -> Option<Vt> {
	unsafe {
		let (vt_service, vt_client): (u64, u64) = channel()?;
		let (control_console, control_shell): (u64, u64) = channel()?;
		if !spawn_shell(facs, broker, vt_client, control_shell) {
			close(vt_service);
			close(vt_client);
			close(control_console);
			close(control_shell);
			return None;
		}
		// nudge the new shell to print its first prompt: an empty line dispatches to a
		// silent reprompt, the same first prompt VT 1 shows at boot.
		send_blocking(vt_service, b"\n", 0);
		// the new VT's terminal policy, re-read from the config tree so a `set`
		// applies here (the next VT) without restarting the console.
		let (vt_scrollback, vt_history): (usize, usize) = term_policy(config_client);
		let mut term: Term = Term::new(make_surface(addr, fb, gpu), vt_scrollback);
		term.resize(cur_w as usize / CELL_W, cur_h as usize / CELL_H);
		term.screen.clear();
		Some(Vt { term: Some(term), client: vt_service, control: control_console, fg_proc: None, ld: Box::new(Ld::new(vt_history)), master: 0, cwd: String::from("vol://system") })
	}
}

// Open a program-hosted PTY: a terminal whose master is another program (the `script`
// tool, a future ssh) instead of the hardware display. Spawn the named slave program over
// a fresh console + control channel pair - a shell gets the full capability set, any other
// program just its console + control - and return the master channel end the host drives it
// on. None on failure. The PTY set grows on demand - never a fixed cap.
unsafe fn open_pty(console: &mut Console, name: &[u8]) -> Option<u64> {
	unsafe {
		let (slave_service, slave_client): (u64, u64) = channel()?;
		let (control_console, control_slave): (u64, u64) = channel()?;
		let (master_console, master_host): (u64, u64) = channel()?;
		let is_shell: bool = name == b"shell";
		let broker: u64 = console.broker;
		let ok: bool = if is_shell { spawn_shell(&mut console.facs, broker, slave_client, control_slave) } else { spawn_pty_program(&console.facs, name, slave_client, control_slave) };
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
		// the slave's line-editor history depth follows the same config policy as a
		// display VT (a pty has no grid, so only the history applies).
		let (_, pty_history): (usize, usize) = term_policy(console.config_client);
		console.ptys.push(Vt { term: None, client: slave_service, control: control_console, fg_proc: None, ld: Box::new(Ld::new(pty_history)), master: master_console, cwd: String::from("vol://system") });
		Some(master_host)
	}
}

// Spawn a minimal (non-shell) program as a PTY slave: it gets only its console + control
// channels (no service factories, no online handshake), the bootstrap a bare terminal
// client needs. Used to host a simple program on a pty (the pty loopback test slave); a
// shell uses spawn_shell. Loaded through ProcessService from the system volume's `bin/`.
unsafe fn spawn_pty_program(facs: &Factories, name: &[u8], program_console: u64, program_control: u64) -> bool {
	unsafe {
		let name_str: &str = match core::str::from_utf8(name) {
			Ok(s) => s,
			Err(_) => return false,
		};
		let (boot_parent, boot_child): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return false,
		};
		let launcher: u64 = match service_connect(facs.process) {
			Some(h) => h,
			None => return false,
		};
		let started = match process::Client::new(ChannelTransport { chan: launcher }).launch(name_str, &boot_child) {
			Some(Ok(s)) => s,
			_ => {
				close(launcher);
				return false;
			}
		};
		close(launcher);
		send_blocking(boot_parent, CAP_CONSOLE, program_console);
		send_blocking(boot_parent, CAP_CONTROL, program_control);
		close(boot_parent);
		close(started.task);
		true
	}
}
