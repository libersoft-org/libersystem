// InputService - the userspace pointer-input service.
//
// ServiceManager starts this program from the init package and hands it a bootstrap
// channel, then over it a "SERVE" channel (the one clients reach it on), an "INPUT"
// and an "INPUT2" channel (the raw pointer-event streams the virtio_input pointer
// driver and the xhci driver feed it; a handle is 0 when that pointer source is
// absent this boot), and a "FORWARD" channel
// (ConsoleService's pointer sink, which drives selection / scrollback / mouse reports).
// The drivers send normalized [x u16][y u16][buttons u8][wheel i8] events; InputService
// maps each to the text-cell grid and keeps a bounded ring of the recent ones for the
// typed `subscribe` API, and forwards the raw bytes to ConsoleService verbatim. Over the
// serve channel clients speak the generated `liber:system` Input bindings: `subscribe`
// hands back a wait-drained event stream of that snapshot (the M30 bounded-snapshot form).
//
// When the supervisor that started it drops the bootstrap channel (no clients this
// boot), the service exits.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use proto::system::input;
use proto::system::PointerEvent;
use rt::*;

// The default text-cell grid the normalized pointer position maps onto: the boot
// framebuffer (1280x800) with 16x16 cells. A resize-aware grid (consulting
// ConsoleService) is a later refinement; this is the plumbing.
const COLS: u32 = 80;
const ROWS: u32 = 50;
// The pointer driver scales each axis into 0..=NORM_MAX; one past that is the span we
// divide the grid across.
const NORM_SPAN: u32 = 0x1_0000;
// The bounded ring of recent mapped pointer events a `subscribe` snapshot returns.
const RING_CAP: usize = 32;
// A raw pointer event from the driver: [x u16 LE][y u16 LE][buttons u8][wheel i8]. The
// first five bytes are the minimum to map a cell position; the wheel byte is forwarded
// to ConsoleService but not part of the typed snapshot.
const RAW_LEN: usize = 5;

// The recent pointer events, mapped to the text-cell grid - the bounded source a
// `subscribe` stream snapshots.
struct Input {
	recent: Vec<PointerEvent>,
}

impl Input {
	fn new() -> Input {
		Input { recent: Vec::new() }
	}

	// Record one mapped event, dropping the oldest once the ring is full.
	fn record(&mut self, event: PointerEvent) {
		self.recent.push(event);
		if self.recent.len() > RING_CAP {
			self.recent.remove(0);
		}
	}
}

// The generated Input service contract: `subscribe` returns the recent pointer
// events, which the serve loop streams frame by frame over a fresh sub-channel.
impl input::Service for Input {
	fn subscribe(&mut self) -> Vec<PointerEvent> {
		self.recent.clone()
	}
}

// Map a raw normalized pointer event to a text-cell PointerEvent, or None if it is
// too short. The x/y axes (0..=NORM_MAX) scale onto the COLS x ROWS grid.
fn map_event(raw: &[u8]) -> Option<PointerEvent> {
	if raw.len() < RAW_LEN {
		return None;
	}
	let x: u32 = u16::from_le_bytes([raw[0], raw[1]]) as u32;
	let y: u32 = u16::from_le_bytes([raw[2], raw[3]]) as u32;
	let buttons: u8 = raw[4];
	let col: u16 = ((x * COLS) / NORM_SPAN) as u16;
	let row: u16 = ((y * ROWS) / NORM_SPAN) as u16;
	Some(PointerEvent { col, row, buttons })
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 64] = [0u8; 64];

	// 1. the serve channel clients reach us on, and the raw pointer-event channels the
	//    pointer drivers feed us ("INPUT" = the virtio pointer, "INPUT2" = the xhci
	//    driver's USB pointer; a handle is 0 when that source is absent).
	let service: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"SERVE") }.unwrap_or_else(|| exit());
	let raw: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if len >= 5 && &buf[..5] == b"INPUT" => handle,
		_ => exit(),
	};
	let raw2: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if len >= 6 && &buf[..6] == b"INPUT2" => handle,
		_ => exit(),
	};
	// ConsoleService's pointer sink: we forward every raw event to it so it can drive
	// selection, scrollback, and mouse reports (handle 0 = no console this boot).
	let forward: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if len >= 7 && &buf[..7] == b"FORWARD" => handle,
		_ => 0,
	};

	// 2. report in to the supervisor that started us.
	unsafe {
		send_blocking(bootstrap, b"InputService: online", 0);
	}

	// 3. serve until the client side closes.
	let mut state: Input = Input::new();
	unsafe {
		serve(service, [raw, raw2], forward, &mut state);
	}
	exit();
}

// Serve loop: wait on the serve channel and the raw pointer-event channels at once.
// On each wake, drain every queued raw event into the cell-mapped ring, then handle
// one client request. Returns when the client side closes (no more clients). Once
// a raw channel closes (its pointer driver retired), it is dropped from the wait
// set so a peer-closed channel cannot spin the loop.
unsafe fn serve(service: u64, raws: [u64; 2], forward: u64, state: &mut Input) {
	unsafe {
		let mut req: [u8; 64] = [0u8; 64];
		let mut open: [bool; 2] = [raws[0] != 0, raws[1] != 0];
		loop {
			// block until the serve channel (or, while open, a raw channel) is ready.
			let mut waitset: [u64; 3] = [service, 0, 0];
			let mut n: usize = 1;
			for (i, &raw) in raws.iter().enumerate() {
				if open[i] {
					waitset[n] = raw;
					n += 1;
				}
			}
			wait_any(&waitset[..n], 0);
			// drain every pending raw pointer event: fold it into the ring for the typed
			// snapshot, and forward the raw bytes to ConsoleService for selection / reports.
			for (i, &raw) in raws.iter().enumerate() {
				if !open[i] {
					continue;
				}
				loop {
					match try_recv(raw, &mut req) {
						Polled::Message { len, .. } => {
							if let Some(event) = map_event(&req[..len]) {
								state.record(event);
							}
							if forward != 0 {
								send_blocking(forward, &req[..len], 0);
							}
						}
						Polled::Empty => break,
						Polled::Closed => {
							open[i] = false;
							break;
						}
					}
				}
			}
			// handle one client request: `subscribe` opens a stream, served out of band.
			match try_recv(service, &mut req) {
				Polled::Message { len, .. } => {
					let op: u16 = if len >= 2 { u16::from_le_bytes([req[0], req[1]]) } else { 0 };
					if op == input::OP_SUBSCRIBE {
						stream_subscribe(service, &req[..len], state);
					}
				}
				Polled::Empty => {}
				Polled::Closed => return,
			}
		}
	}
}

// Serve one `subscribe` request: gather the bounded snapshot, then stream the mapped
// pointer events to the client over a fresh sub-channel. The reply on the service
// channel carries the correlation id and the consumer endpoint (out-of-band); each
// event then travels as its own framed message on the producer endpoint, and closing
// the producer marks end-of-stream.
fn stream_subscribe(service: u64, request: &[u8], state: &mut Input) {
	let (corr, items): (u32, Vec<PointerEvent>) = match input::subscribe_open(state, request) {
		Some(v) => v,
		None => return,
	};
	let (producer, consumer): (u64, u64) = match unsafe { channel() } {
		Some(pair) => pair,
		None => return,
	};
	let corr_bytes: [u8; 4] = corr.to_le_bytes();
	unsafe {
		send_blocking(service, &corr_bytes, consumer);
	}
	let mut frame: [u8; 32] = [0u8; 32];
	for (seq, item) in items.iter().enumerate() {
		if let Some(n) = input::subscribe_frame(seq as u32, item, &mut frame) {
			unsafe {
				send_blocking(producer, &frame[..n], 0);
			}
		}
	}
	unsafe {
		close(producer);
	}
}
