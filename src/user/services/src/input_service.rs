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
// hands back a wait-drained event stream of that snapshot (the bounded-snapshot form).
//
// When the supervisor that started it drops the bootstrap channel (no clients this
// boot), the service exits.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use proto::system::input;
use proto::system::{KeyEvent, PointerEvent};
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
	held: Vec<u16>,
	focus_peer: u64,
	kill_control: u64,
	key_stream: Option<KeyStream>,
	proof_nonce: u64,
}

struct KeyStream {
	owner: u64,
	producer: u64,
	seq: u32,
}

impl Input {
	fn new(kill_control: u64) -> Input {
		Input { recent: Vec::new(), held: Vec::new(), focus_peer: 0, kill_control, key_stream: None, proof_nonce: 0 }
	}

	// Record one mapped event, dropping the oldest once the ring is full.
	fn record(&mut self, event: PointerEvent) {
		self.recent.push(event);
		if self.recent.len() > RING_CAP {
			self.recent.remove(0);
		}
	}

	fn record_key(&mut self, raw: &[u8]) {
		if raw.len() != 3 || raw[2] > 1 {
			return;
		}
		let code: u16 = u16::from_le_bytes([raw[0], raw[1]]);
		let pressed: bool = raw[2] != 0;
		let held: Option<usize> = self.held.iter().position(|held| *held == code);
		if pressed {
			if held.is_some() {
				return;
			}
			self.held.push(code);
		} else if let Some(index) = held {
			self.held.swap_remove(index);
		} else {
			return;
		}
		self.send_key(KeyEvent { code, pressed });
		if pressed && code == 0x29 && self.held.iter().any(|code| *code == 0xe0 || *code == 0xe4) && self.held.iter().any(|code| *code == 0xe2 || *code == 0xe6) {
			if self.kill_control != 0 {
				unsafe {
					let _ = send_blocking(self.kill_control, b"KILL", 0);
				}
			}
			self.set_focus(0);
		}
	}

	fn send_key(&mut self, event: KeyEvent) -> bool {
		let Some(stream) = self.key_stream.as_mut() else { return false };
		let mut frame: [u8; 32] = [0; 32];
		let mut frame_handle: u64 = 0;
		let sent: bool = match input::subscribe_keys_frame(stream.seq, &event, &mut frame, &mut frame_handle) {
			Some(len) => unsafe { try_send(stream.producer, &frame[..len], frame_handle) },
			None => false,
		};
		if sent {
			stream.seq = stream.seq.wrapping_add(1);
			true
		} else {
			if frame_handle != 0 {
				unsafe { close(frame_handle) };
			}
			let dead: KeyStream = self.key_stream.take().unwrap();
			unsafe { close(dead.producer) };
			false
		}
	}

	fn close_key_stream(&mut self, release_held: bool) {
		if release_held {
			for code in self.held.clone() {
				if !self.send_key(KeyEvent { code, pressed: false }) {
					break;
				}
			}
		}
		if let Some(stream) = self.key_stream.take() {
			unsafe { close(stream.producer) };
		}
	}

	fn set_focus(&mut self, peer: u64) {
		self.close_key_stream(true);
		if self.focus_peer != 0 {
			unsafe { close(self.focus_peer) };
		}
		self.focus_peer = peer;
	}

	fn notify_console(&self, focused: bool, forward: u64) {
		if forward != 0 {
			let mut message: [u8; 9] = [0; 9];
			message[..8].copy_from_slice(b"KEYFOCUS");
			message[8] = focused as u8;
			unsafe {
				let _ = send_blocking(forward, &message, 0);
			}
		}
	}

	fn validate_focus(&mut self, proof: u64) -> bool {
		if proof == 0 || self.focus_peer == 0 {
			return false;
		}
		self.proof_nonce = self.proof_nonce.wrapping_add(1);
		let challenge: [u8; 8] = self.proof_nonce.to_le_bytes();
		if !unsafe { try_send(proof, &challenge, 0) } {
			return false;
		}
		let mut received: [u8; 8] = [0; 8];
		matches!(unsafe { try_recv(self.focus_peer, &mut received) }, Polled::Message { len: 8, handle: 0 } if received == challenge)
	}
}

// The generated Input service contract: `subscribe` returns the recent pointer
// events, which the serve loop streams frame by frame over a fresh sub-channel.
impl input::Service for Input {
	fn subscribe(&mut self) -> Vec<PointerEvent> {
		self.recent.clone()
	}

	fn subscribe_keys(&mut self, focus: u64) -> Vec<KeyEvent> {
		if focus != 0 {
			unsafe { close(focus) };
		}
		Vec::new()
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
	let service: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"SERVE") }.unwrap_or_else(|| unsafe { fail_bootstrap(bootstrap, b"serve", b"missing serve channel") });
	let raw: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if len >= 5 && &buf[..5] == b"INPUT" => handle,
		_ => unsafe { fail_bootstrap(bootstrap, b"input", b"pointer channel not delivered") },
	};
	let raw2: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if len >= 6 && &buf[..6] == b"INPUT2" => handle,
		_ => unsafe { fail_bootstrap(bootstrap, b"input2", b"usb pointer channel not delivered") },
	};
	// ConsoleService's pointer sink: we forward every raw event to it so it can drive
	// selection, scrollback, and mouse reports (handle 0 = no console this boot).
	let forward: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if len >= 7 && &buf[..7] == b"FORWARD" => handle,
		_ => 0,
	};
	let keys: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if len >= 4 && &buf[..4] == b"KEYS" => handle,
		_ => 0,
	};
	let focus: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if len >= 5 && &buf[..5] == b"FOCUS" => handle,
		_ => 0,
	};
	let kill: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if len >= 4 && &buf[..4] == b"KILL" => handle,
		_ => 0,
	};

	// 2. report in to the supervisor that started us.
	unsafe {
		send_blocking(bootstrap, b"InputService: online", 0);
	}

	// 3. serve until the client side closes.
	let mut state: Input = Input::new(kill);
	unsafe {
		serve(service, [raw, raw2], forward, keys, focus, &mut state);
	}
	exit();
}

// Serve loop: wait on the serve channel and the raw pointer-event channels at once.
// On each wake, drain every queued raw event into the cell-mapped ring, then handle
// one client request. Returns when the client side closes (no more clients). Once
// a raw channel closes (its pointer driver retired), it is dropped from the wait
// set so a peer-closed channel cannot spin the loop.
unsafe fn serve(service: u64, raws: [u64; 2], forward: u64, keys: u64, focus: u64, state: &mut Input) {
	unsafe {
		let mut req: [u8; 64] = [0u8; 64];
		let mut open: [bool; 2] = [raws[0] != 0, raws[1] != 0];
		let mut clients: Vec<u64> = alloc::vec![service];
		let mut keys_open: bool = keys != 0;
		let mut focus_open: bool = focus != 0;
		loop {
			let mut waitset: Vec<u64> = Vec::with_capacity(clients.len() + 4);
			if focus_open {
				waitset.push(focus);
			}
			if keys_open {
				waitset.push(keys);
			}
			for (i, &raw) in raws.iter().enumerate() {
				if open[i] {
					waitset.push(raw);
				}
			}
			waitset.extend_from_slice(&clients);
			let ready: i64 = wait_any(&waitset, 0);
			if ready < 0 {
				continue;
			}
			let ready_handle: u64 = waitset[ready as usize];
			if focus_open && ready_handle == focus {
				let acknowledged: bool = match recv_blocking(focus, &mut req) {
					Received::Message { len, handle } if len >= 3 && &req[..3] == b"SET" && handle != 0 => {
						state.notify_console(false, forward);
						state.set_focus(handle);
						true
					}
					Received::Message { len, handle } if len >= 7 && &req[..7] == b"CONSOLE" => {
						if handle != 0 {
							close(handle);
						}
						state.set_focus(0);
						state.notify_console(true, forward);
						true
					}
					Received::Message { handle, .. } => {
						if handle != 0 {
							close(handle);
						}
						state.set_focus(0);
						state.notify_console(false, forward);
						true
					}
					Received::Closed => {
						focus_open = false;
						state.set_focus(0);
						false
					}
				};
				if acknowledged {
					send_blocking(focus, b"OK", 0);
				}
				continue;
			}
			if keys_open && ready_handle == keys {
				loop {
					match try_recv(keys, &mut req) {
						Polled::Message { len, handle } => {
							if handle != 0 {
								close(handle);
							}
							state.record_key(&req[..len]);
						}
						Polled::Empty => break,
						Polled::Closed => {
							keys_open = false;
							break;
						}
					}
				}
				continue;
			}
			for (i, &raw) in raws.iter().enumerate() {
				if !open[i] || ready_handle != raw {
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
				continue;
			}
			let Some(client_index) = clients.iter().position(|client| *client == ready_handle) else { continue };
			let client: u64 = clients[client_index];
			match recv_blocking(client, &mut req) {
				Received::Message { len, mut handle } => {
					let op: u16 = if len >= 2 { u16::from_le_bytes([req[0], req[1]]) } else { 0 };
					if op == CONNECT_OP {
						if let Some((mine, theirs)) = channel() {
							clients.push(mine);
							send_blocking(client, &[], theirs);
						}
					} else if op == input::OP_SUBSCRIBE {
						stream_subscribe(client, &req[..len], state);
					} else if op == input::OP_SUBSCRIBE_KEYS {
						stream_subscribe_keys(client, &req[..len], &mut handle, state);
					}
					if handle != 0 {
						close(handle);
					}
				}
				Received::Closed => {
					if state.key_stream.as_ref().is_some_and(|stream| stream.owner == client) {
						state.close_key_stream(false);
					}
					if client_index == 0 {
						return;
					}
					close(client);
					clients.swap_remove(client_index);
				}
			}
		}
	}
}

fn stream_subscribe_keys(service: u64, request: &[u8], request_handle: &mut u64, state: &mut Input) {
	if request.len() != 10 || *request_handle == 0 {
		return;
	}
	let corr: u32 = u32::from_le_bytes([request[2], request[3], request[4], request[5]]);
	let proof: u64 = core::mem::take(request_handle);
	let valid: bool = state.validate_focus(proof);
	unsafe { close(proof) };
	if !valid {
		unsafe {
			send_blocking(service, &corr.to_le_bytes(), 0);
		}
		return;
	}
	state.close_key_stream(true);
	let (producer, consumer): (u64, u64) = match unsafe { channel() } {
		Some(pair) => pair,
		None => return,
	};
	if unsafe { send_blocking(service, &corr.to_le_bytes(), consumer) } {
		state.key_stream = Some(KeyStream { owner: service, producer, seq: 0 });
	} else {
		unsafe {
			close(producer);
			close(consumer);
		}
	}
}

// Serve one `subscribe` request: gather the bounded snapshot, then stream the mapped
// pointer events to the client over a fresh sub-channel. The reply on the service
// channel carries the correlation id and the consumer endpoint (out-of-band); each
// event then travels as its own framed message on the producer endpoint, and closing
// the producer marks end-of-stream.
fn stream_subscribe(service: u64, request: &[u8], state: &mut Input) {
	let mut request_handle: u64 = 0;
	let (corr, items): (u32, Vec<PointerEvent>) = match input::subscribe_open(state, request, &mut request_handle) {
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
		let mut frame_handle: u64 = 0;
		if let Some(n) = input::subscribe_frame(seq as u32, item, &mut frame, &mut frame_handle) {
			unsafe {
				if !send_blocking(producer, &frame[..n], frame_handle) && frame_handle != 0 {
					close(frame_handle);
				}
			}
		} else if frame_handle != 0 {
			unsafe { close(frame_handle) };
		}
	}
	unsafe {
		close(producer);
	}
}
