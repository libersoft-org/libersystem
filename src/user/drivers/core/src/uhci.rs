// driver.uhci - an Intel UHCI USB 1.1 host controller, the controller engine under the shared USB core.
//
// WHAT IS UHCI'S AND NOTHING ELSE: a frame list the controller walks every millisecond, queue heads on it - one per
// pipe - with chains of transfer descriptors hung under them, data toggles the driver keeps, and two root ports
// that raise nothing when a device arrives, so they are looked at every quarter of a second. Everything USB is the
// shared core's - `drivers::usb_host` addresses, configures, binds and serves - and the class modules under it are
// the ones xHCI's driver runs.
//
// THE SCHEDULE IS ONE LIST. Every frame-list entry points at one anchor queue head, and every pipe's queue head is
// linked after it, so every pipe is visited every frame: an interrupt endpoint is polled each millisecond, which is
// never slower than the `bInterval` it asked for, and the control and bulk pipes share what is left of the frame.
// The anchor is never removed, so a pipe is inserted after it and taken out from behind whatever precedes it.
//
// ITS REGISTERS ARE I/O PORTS - BAR 4, which reaches this driver as a port range - and its descriptors and data are
// below 4 GiB, which is all the controller can name. Its interrupt is the function's INTx, which the firmware puts on
// a line other functions share; every wake reads and clears the status before the line is acknowledged.
//
// DEVELOPMENT-ONLY, AND THAT IS A DECISION: this is the final compatibility wave, and nothing has shown a target that
// needs it, so it is not enabled by default - see the manifest.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use core::sync::atomic::{Ordering, fence};
use drivers::usb_host::{self, Controller, Dma, Done, Endpoint, Pipe, Pool, Port, Speed};
use drivers::{common, keys, usb_input};
#[cfg(target_arch = "x86_64")]
use rt::port::{inw, outl, outw};

// THE PORTS HAVE NO I/O PORT SPACE a driver reaches, so a UHCI there is never brought up: mapping its port range is
// refused before any register is touched. These stand in for the instructions so the driver builds for every target.
#[cfg(not(target_arch = "x86_64"))]
fn inw(_port: u16) -> u16 {
	0xffff
}
#[cfg(not(target_arch = "x86_64"))]
fn outw(_port: u16, _value: u16) {}
#[cfg(not(target_arch = "x86_64"))]
fn outl(_port: u16, _value: u32) {}
use rt::*;

// The registers, as offsets from the I/O base.
const USBCMD: u16 = 0x00;
const USBSTS: u16 = 0x02;
const USBINTR: u16 = 0x04;
const FRNUM: u16 = 0x06;
const FRBASEADD: u16 = 0x08;
const PORTSC: u16 = 0x10;

const CMD_RS: u16 = 1 << 0;
const CMD_HCRESET: u16 = 1 << 1;
const CMD_CF: u16 = 1 << 6;
const CMD_MAXP: u16 = 1 << 7;
const STS_HALTED: u16 = 1 << 5;
const STS_HSE: u16 = 1 << 3;
const STS_HCPE: u16 = 1 << 4;
// Timeout/CRC, resume, completion and short packet.
const INTR_ALL: u16 = 0x0f;

const PORT_CCS: u16 = 1 << 0;
const PORT_CSC: u16 = 1 << 1;
const PORT_PE: u16 = 1 << 2;
const PORT_PEC: u16 = 1 << 3;
const PORT_ALWAYS: u16 = 1 << 7;
const PORT_LSDA: u16 = 1 << 8;
const PORT_PR: u16 = 1 << 9;
// The bits a port write keeps as they read: the enable, resume, reset and suspend. The two change bits are written as
// zero unless they are being cleared.
const PORT_RW: u16 = PORT_PE | 1 << 6 | PORT_PR | 1 << 12;

// A link: terminate, a queue head rather than a TD, depth first.
const LINK_T: u32 = 1 << 0;
const LINK_Q: u32 = 1 << 1;
const LINK_VF: u32 = 1 << 2;

// A TD's control and status word.
const TD_ACTIVE: u32 = 1 << 23;
const TD_STALLED: u32 = 1 << 22;
const TD_ERRORS: u32 = 0x7e << 16; // stalled, buffer, babble, NAK, CRC/timeout, bitstuff
const TD_NAK: u32 = 1 << 19;
const TD_IOC: u32 = 1 << 24;
const TD_LS: u32 = 1 << 26;
const TD_CERR3: u32 = 3 << 27;
const TD_SPD: u32 = 1 << 29;
const PID_SETUP: u32 = 0x2d;
const PID_IN: u32 = 0x69;
const PID_OUT: u32 = 0xe1;

// THE POOL: queue heads and TDs in 32-byte slots - the setup packet the first of them - and how many TDs one stage of
// a bulk transfer is cut into at most.
const SLOTS: u32 = 512;
const SLOT: u32 = 32;
const SETUP_SLOT: u32 = 0;
const CHUNK: usize = 64;
// The most pipes one controller keeps.
const MAX_PIPES: usize = 64;
// How long a transfer may take: a control transfer a second, a bulk stage five.
const CONTROL_TICKS: u64 = 100;
const BULK_TICKS: u64 = 500;

// One pipe: its endpoint, its queue head's slot, the toggle its next packet carries, and a standing interrupt
// transfer's TDs and length.
struct PipeState {
	endpoint: Endpoint,
	qh: u32,
	toggle: bool,
	standing: Vec<u32>,
	standing_len: u32,
}

struct Uhci {
	base: u16,
	irq: u64,
	device: u64,
	ports: u32,
	pool: Pool,
	frames: Dma,
	anchor: u32,
	pipes: Vec<Option<PipeState>>,
	port_changed: bool,
	said_error: bool,
}

impl Uhci {
	fn reg(&self, offset: u16) -> u16 {
		inw(self.base + offset)
	}

	fn set(&self, offset: u16, value: u16) {
		outw(self.base + offset, value);
	}

	fn portsc(&self, port: u32) -> u16 {
		self.reg(PORTSC + 2 * (port as u16 - 1))
	}

	fn set_portsc(&self, port: u32, value: u16) {
		self.set(PORTSC + 2 * (port as u16 - 1), value);
	}

	// RESET THE CONTROLLER AND BUILD ITS SCHEDULE: the global reset, the pool and the frame list - every entry the
	// anchor queue head - and the controller running with every interrupt it raises enabled.
	fn bring_up(base: u16, irq: u64, device: u64) -> Option<Uhci> {
		outw(base + USBCMD, 0);
		outw(base + USBCMD, CMD_HCRESET);
		let deadline = clock() + 10;
		while inw(base + USBCMD) & CMD_HCRESET != 0 {
			if clock() >= deadline {
				return None;
			}
			yield_now();
		}
		// THE CONTROLLER IS STOPPED AND RESET: nothing a previous instance pointed it at is in flight, which releases
		// the frames the kernel held for it.
		device_quiesced(device);
		let pool = Pool::new(device, SLOT, SLOTS)?;
		let frames = usb_host::dma_below_4g(device, 4096)?;
		let mut uhci = Uhci { base, irq, device, ports: 0, pool, frames, anchor: 0, pipes: (0..MAX_PIPES).map(|_| None).collect(), port_changed: false, said_error: false };
		let setup = uhci.pool.take()?;
		debug_assert_eq!(setup, SETUP_SLOT);
		uhci.anchor = uhci.pool.take()?;
		uhci.pool.write(uhci.anchor, 0, LINK_T);
		uhci.pool.write(uhci.anchor, 4, LINK_T);
		let entry = uhci.pool.phys(uhci.anchor) | LINK_Q;
		for at in 0..1024u64 {
			// SAFETY: the frame list page, which the controller does not walk until it runs below.
			unsafe { ((uhci.frames.virt + at * 4) as *mut u32).write_volatile(entry) };
		}
		// THE PORTS: a register whose always-one bit reads one is a port, up to the eight the window holds.
		while uhci.ports < 7 && {
			let value = uhci.portsc(uhci.ports + 1);
			value != 0xffff && value & PORT_ALWAYS != 0
		} {
			uhci.ports += 1;
		}
		fence(Ordering::SeqCst);
		outl(base + FRBASEADD, uhci.frames.phys);
		outw(base + FRNUM, 0);
		outw(base + USBSTS, 0x3f);
		outw(base + USBINTR, INTR_ALL);
		outw(base + USBCMD, CMD_RS | CMD_CF | CMD_MAXP);
		let deadline = clock() + 10;
		while inw(base + USBSTS) & STS_HALTED != 0 {
			if clock() >= deadline {
				return None;
			}
			yield_now();
		}
		Some(uhci)
	}

	// The frame the controller is in - what a descriptor taken out of the schedule waits one of before it is reused.
	fn frame(&self) -> u16 {
		self.reg(FRNUM) & 0x7ff
	}

	// WAIT FOR THE NEXT FRAME, so nothing the controller read in this one is still in its hands.
	fn next_frame(&self) {
		let now = self.frame();
		let deadline = clock() + 2;
		while self.frame() == now && clock() < deadline {
			yield_now();
		}
	}

	fn td_write(&self, td: u32, link: u32, status: u32, token: u32, buffer: u32) {
		self.pool.write(td, 0, link);
		self.pool.write(td, 4, status);
		self.pool.write(td, 8, token);
		self.pool.write(td, 12, buffer);
	}

	// BUILD A CHAIN OF TDs for `packets` - each its PID, toggle, buffer and length - under a pipe's endpoint, linked
	// depth first, the last raising the completion interrupt. Every IN packet detects a short packet, which is what
	// ends an IN transfer the device had less for. None, with nothing taken, when the pool cannot hold the chain.
	fn chain(&mut self, endpoint: &Endpoint, packets: &[(u32, bool, u32, u32)]) -> Option<Vec<u32>> {
		if self.pool.free() < packets.len() {
			return None;
		}
		let tds: Vec<u32> = packets.iter().map(|_| self.pool.take()).collect::<Option<Vec<u32>>>()?;
		for (at, &(pid, toggle, buffer, len)) in packets.iter().enumerate() {
			let last = at + 1 == packets.len();
			let link = if last { LINK_T } else { self.pool.phys(tds[at + 1]) | LINK_VF };
			let mut status = TD_ACTIVE | TD_CERR3;
			if endpoint.speed == Speed::Low {
				status |= TD_LS;
			}
			if pid == PID_IN {
				status |= TD_SPD;
			}
			if last {
				status |= TD_IOC;
			}
			let max_len = if len == 0 { 0x7ff } else { len - 1 };
			let token = max_len << 21 | (toggle as u32) << 19 | (endpoint.number as u32 & 0xf) << 15 | (endpoint.address as u32 & 0x7f) << 8 | pid;
			self.td_write(tds[at], link, status, token, buffer);
		}
		Some(tds)
	}

	// Hang a chain under a pipe's queue head - after every word of it was written.
	fn hang(&self, qh: u32, first: u32) {
		fence(Ordering::SeqCst);
		self.pool.write(qh, 4, self.pool.phys(first));
	}

	// WHERE A CHAIN STANDS: None while it runs; once it has ended - its last TD done, a TD failed, or an IN TD came
	// back short - how many bytes its data TDs moved, how many TDs completed, and whether it stalled or failed.
	fn outcome(&self, tds: &[u32]) -> Option<(u32, usize, Option<bool>)> {
		let mut moved = 0;
		for (at, &td) in tds.iter().enumerate() {
			let status = self.pool.read(td, 4);
			if status & TD_ACTIVE != 0 {
				return None;
			}
			// A NAK IS NOT A FAILURE - the controller retries it while the TD stays active - so only the other error
			// bits end a chain.
			if status & (TD_ERRORS & !TD_NAK) != 0 {
				return Some((moved, at, Some(status & TD_STALLED != 0)));
			}
			let actual = (status + 1) & 0x7ff;
			moved += actual;
			let token = self.pool.read(td, 8);
			let asked = ((token >> 21) + 1) & 0x7ff;
			if token & 0xff == PID_IN && actual < asked {
				return Some((moved, at + 1, None));
			}
		}
		Some((moved, tds.len(), None))
	}

	// RUN ONE CHAIN ON A PIPE TO ITS END, bounded: hung, waited for on the interrupt, and taken off the queue head -
	// one frame passing before its TDs go back, so nothing the controller read is still in its hands. Answers what
	// `outcome` says, or None when it ran out of time.
	fn run_chain(&mut self, pipe: usize, packets: &[(u32, bool, u32, u32)], ticks: u64) -> Option<(u32, usize, Option<bool>)> {
		let (endpoint, qh) = {
			let state = self.pipes[pipe].as_ref()?;
			(state.endpoint, state.qh)
		};
		let tds = self.chain(&endpoint, packets)?;
		self.hang(qh, tds[0]);
		let irq = self.irq;
		let ended = usb_host::settle(self, irq, ticks, |uhci| uhci.outcome(&tds).is_some());
		let result = if ended { self.outcome(&tds) } else { None };
		self.pool.write(qh, 4, LINK_T);
		self.next_frame();
		for td in tds {
			self.pool.give(td);
		}
		result
	}

	// The pipe a handle names.
	fn index(pipe: Pipe) -> usize {
		pipe as usize - 1
	}

	// A data stage cut into packets of the endpoint's size, toggles alternating from `toggle`.
	fn packets(endpoint: &Endpoint, pid: u32, mut toggle: bool, phys: u32, len: u32) -> Vec<(u32, bool, u32, u32)> {
		let mps = endpoint.max_packet.max(8) as u32;
		let mut out = Vec::new();
		let mut done = 0;
		while done < len || (len == 0 && out.is_empty()) {
			let take = (len - done).min(mps);
			out.push((pid, toggle, phys + done, take));
			toggle = !toggle;
			done += take;
			if len == 0 {
				break;
			}
		}
		out
	}
}

impl Controller for Uhci {
	fn who(&self) -> &'static [u8] {
		b"driver.uhci"
	}

	fn ports(&self) -> u32 {
		self.ports
	}

	fn port(&mut self, port: u32) -> Port {
		let value = self.portsc(port);
		Port { connected: value & PORT_CCS != 0, enabled: value & PORT_PE != 0, changed: value & (PORT_CSC | PORT_PEC) != 0 }
	}

	// THE ROOT PORT RESET: fifty milliseconds of reset, then the port enabled - written again until it reads back
	// enabled, a few times, which some parts need - and the device's speed read off its low-speed bit.
	fn reset_port(&mut self, port: u32) -> Option<Speed> {
		let keep = self.portsc(port) & PORT_RW;
		self.set_portsc(port, keep | PORT_PR);
		sleep_until(clock() + 5);
		self.set_portsc(port, keep & !PORT_PR);
		sleep_until(clock() + 1);
		for _ in 0..10 {
			let value = self.portsc(port);
			if value & PORT_CCS == 0 {
				return None;
			}
			if value & PORT_PE != 0 {
				self.set_portsc(port, (value & PORT_RW) | PORT_CSC | PORT_PEC);
				return Some(if value & PORT_LSDA != 0 { Speed::Low } else { Speed::Full });
			}
			self.set_portsc(port, (value & PORT_RW) | PORT_PE | PORT_CSC | PORT_PEC);
			sleep_until(clock() + 1);
		}
		None
	}

	fn acknowledge_port(&mut self, port: u32) {
		let value = self.portsc(port);
		self.set_portsc(port, (value & PORT_RW) | (value & (PORT_CSC | PORT_PEC)));
	}

	// A PIPE IS A QUEUE HEAD, linked in right after the anchor with its element empty.
	fn open(&mut self, endpoint: &Endpoint) -> Option<Pipe> {
		let at = self.pipes.iter().position(Option::is_none)?;
		let qh = self.pool.take()?;
		self.pool.write(qh, 4, LINK_T);
		self.pool.write(qh, 0, self.pool.read(self.anchor, 0));
		fence(Ordering::SeqCst);
		self.pool.write(self.anchor, 0, self.pool.phys(qh) | LINK_Q);
		self.pipes[at] = Some(PipeState { endpoint: *endpoint, qh, toggle: false, standing: Vec::new(), standing_len: 0 });
		Some(at as Pipe + 1)
	}

	// TAKEN OUT FROM BEHIND WHATEVER PRECEDES IT, a frame waited, and its TDs and its queue head given back.
	fn close(&mut self, pipe: Pipe) {
		let Some(state) = self.pipes.get_mut(Self::index(pipe)).and_then(Option::take) else { return };
		let target = self.pool.phys(state.qh) | LINK_Q;
		let mut at = self.anchor;
		loop {
			let link = self.pool.read(at, 0);
			if link == target {
				self.pool.write(at, 0, self.pool.read(state.qh, 0));
				break;
			}
			if link & LINK_T != 0 {
				break;
			}
			let Some(next) = self.pool.slot_of(link & !0xf) else { break };
			at = next;
		}
		self.pool.write(state.qh, 4, LINK_T);
		self.next_frame();
		for td in state.standing {
			self.pool.give(td);
		}
		self.pool.give(state.qh);
	}

	fn retarget(&mut self, pipe: Pipe, address: u8, max_packet: u16) {
		if let Some(state) = self.pipes.get_mut(Self::index(pipe)).and_then(Option::as_mut) {
			state.endpoint.address = address;
			state.endpoint.max_packet = max_packet;
		}
	}

	// A CONTROL TRANSFER IN TWO CHAINS: the setup and data stages, then the status stage - separately, because a
	// short data stage stops its queue at the short packet and the status stage still has to run.
	fn control(&mut self, pipe: Pipe, setup: &[u8; 8], data: Option<(u32, u16)>) -> Done {
		let at = Self::index(pipe);
		let Some(endpoint) = self.pipes.get(at).and_then(Option::as_ref).map(|state| state.endpoint) else { return Done::Failed };
		for (word, bytes) in setup.chunks(4).enumerate() {
			self.pool.write(SETUP_SLOT, word as u32 * 4, u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
		}
		let data_in = setup[0] & 0x80 != 0;
		let mut packets = alloc::vec![(PID_SETUP, false, self.pool.phys(SETUP_SLOT), 8)];
		if let Some((phys, len)) = data
			&& len > 0
		{
			packets.extend(Self::packets(&endpoint, if data_in { PID_IN } else { PID_OUT }, true, phys, len as u32));
		}
		let moved = match self.run_chain(at, &packets, CONTROL_TICKS) {
			Some((moved, _, None)) => moved.saturating_sub(8),
			Some((_, _, Some(true))) => return Done::Stall(0),
			_ => return Done::Failed,
		};
		let status_pid = if data_in && data.is_some_and(|(_, len)| len > 0) { PID_OUT } else { PID_IN };
		match self.run_chain(at, &[(status_pid, true, 0, 0)], CONTROL_TICKS) {
			Some((_, _, None)) => Done::Ok(moved),
			Some((_, _, Some(true))) => Done::Stall(moved),
			_ => Done::Failed,
		}
	}

	// A BULK TRANSFER IN STAGES of at most `CHUNK` packets, each run to its end before the next is built, the toggle
	// advanced by the packets that completed - until all of it moved, a packet came back short, or one failed.
	fn bulk(&mut self, pipe: Pipe, phys: u32, len: u32) -> Done {
		let at = Self::index(pipe);
		let Some((endpoint, mut toggle)) = self.pipes.get(at).and_then(Option::as_ref).map(|state| (state.endpoint, state.toggle)) else { return Done::Failed };
		let pid = if endpoint.dir_in { PID_IN } else { PID_OUT };
		let mps = endpoint.max_packet.max(8) as u32;
		let mut moved = 0;
		let outcome = loop {
			let stage = (len - moved).min(mps * CHUNK as u32);
			let packets = Self::packets(&endpoint, pid, toggle, phys + moved, stage);
			let Some((part, completed, failed)) = self.run_chain(at, &packets, BULK_TICKS) else { break Done::Failed };
			if completed % 2 == 1 {
				toggle = !toggle;
			}
			moved += part;
			match failed {
				Some(true) => break Done::Stall(moved),
				Some(false) => break Done::Failed,
				None if part < stage || moved >= len => break Done::Ok(moved),
				None => continue,
			}
		};
		if let Some(state) = self.pipes.get_mut(at).and_then(Option::as_mut) {
			state.toggle = toggle;
		}
		outcome
	}

	fn submit(&mut self, pipe: Pipe, phys: u32, len: u32) -> bool {
		let at = Self::index(pipe);
		let Some((endpoint, toggle, standing)) = self.pipes.get(at).and_then(Option::as_ref).map(|state| (state.endpoint, state.toggle, !state.standing.is_empty())) else { return false };
		if standing {
			return false;
		}
		let packets = Self::packets(&endpoint, PID_IN, toggle, phys, len);
		let Some(tds) = self.chain(&endpoint, &packets) else { return false };
		let qh = self.pipes[at].as_ref().map_or(0, |state| state.qh);
		self.hang(qh, tds[0]);
		if let Some(state) = self.pipes[at].as_mut() {
			state.standing = tds;
			state.standing_len = len;
		}
		true
	}

	fn reap(&mut self, pipe: Pipe) -> Option<Done> {
		let at = Self::index(pipe);
		let (moved, completed, failed) = {
			let state = self.pipes.get(at)?.as_ref()?;
			if state.standing.is_empty() {
				return None;
			}
			self.outcome(&state.standing)?
		};
		let state = self.pipes[at].take()?;
		self.pool.write(state.qh, 4, LINK_T);
		self.next_frame();
		for &td in &state.standing {
			self.pool.give(td);
		}
		let toggle = if completed % 2 == 1 { !state.toggle } else { state.toggle };
		self.pipes[at] = Some(PipeState { standing: Vec::new(), standing_len: 0, toggle, ..state });
		Some(match failed {
			Some(true) => Done::Stall(moved),
			Some(false) => Done::Failed,
			None => Done::Ok(moved),
		})
	}

	fn reset_toggle(&mut self, pipe: Pipe) {
		if let Some(state) = self.pipes.get_mut(Self::index(pipe)).and_then(Option::as_mut) {
			state.toggle = false;
		}
	}

	// THE STATUS, READ AND CLEARED. A host-system or process error is the controller saying its schedule or the bus
	// is broken: said once, since nothing here can repair it - a rebind resets it.
	fn service(&mut self) {
		let status = self.reg(USBSTS);
		self.set(USBSTS, status & 0x3f);
		if status & (STS_HSE | STS_HCPE) != 0 && !self.said_error {
			self.said_error = true;
			print(b"driver.uhci: the controller reported a host system or process error and stopped\n");
		}
	}

	// THE PORTS RAISE NOTHING, so a change is found by looking - see `port_poll`.
	fn take_port_change(&mut self) -> bool {
		let changed = core::mem::take(&mut self.port_changed);
		changed || (1..=self.ports).any(|port| self.portsc(port) & PORT_CSC != 0)
	}

	fn port_poll(&self) -> Option<u64> {
		Some(25)
	}

	fn dma(&mut self, bytes: u32) -> Option<Dma> {
		usb_host::dma_below_4g(self.device, bytes)
	}

	fn max_transfer(&self) -> u32 {
		64 * 1024
	}

	fn splits(&self) -> bool {
		false
	}

	fn halt(&mut self) -> bool {
		self.set(USBCMD, self.reg(USBCMD) & !CMD_RS);
		let deadline = clock() + 5;
		while self.reg(USBSTS) & STS_HALTED == 0 {
			if clock() >= deadline {
				return false;
			}
			yield_now();
		}
		true
	}

	fn run(&mut self) -> bool {
		self.set(USBCMD, self.reg(USBCMD) | CMD_RS);
		let deadline = clock() + 5;
		while self.reg(USBSTS) & STS_HALTED != 0 {
			if clock() >= deadline {
				return false;
			}
			yield_now();
		}
		true
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, resources) = common::handshake(bootstrap);
	let device = resources.device;
	// THE INTERRUPT IS THE FUNCTION'S INTx, its row's line - there is no other.
	if device == 0 || resources.line_count == 0 {
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
	}
	let irq = resources.lines[0];
	keys::set_power(resources.syspower);
	keys::set_console_input(resources.console);
	usb_input::KEY_SINK.store(resources.keys, Ordering::Relaxed);
	usb_input::TRUSTED_KEY_SINK.store(resources.trusted_keys, Ordering::Relaxed);
	// THE REGISTERS: the function's I/O BAR 4, mapped into this process.
	let Some(at) = (0..bind.info.port_count as usize).find(|&at| bind.info.ports[at].source == PORT_SOURCE_IO_BAR && bind.info.ports[at].index == 4 && bind.info.ports[at].len >= 0x14) else {
		print(b"driver.uhci: the function carries no I/O window for its registers - it is not driven\n");
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
	};
	if at >= resources.port_range_count || port_range_map(resources.port_ranges[at]) < 0 {
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
	}
	let Some(uhci) = Uhci::bring_up(bind.info.ports[at].base, irq, device) else {
		print(b"driver.uhci: the controller did not reset or would not run\n");
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::DeviceNotResponding);
	};
	let Some(host) = usb_host::Host::new(uhci) else { common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::OutOfMemory) };
	usb_host::run(bootstrap, &bind, device, irq, host)
}
