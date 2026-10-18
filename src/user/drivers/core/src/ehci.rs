// driver.ehci - an EHCI USB 2.0 host controller, the controller engine under the shared USB core.
//
// WHAT IS EHCI'S AND NOTHING ELSE: two schedules - the asynchronous one, a ring of queue heads for control and bulk
// pipes behind one head of reclamation, and the periodic one, a frame list whose every entry leads to the interrupt
// pipes' queue heads - with qTDs hung under each queue head, root ports that raise a change interrupt, and the
// companion controllers a full- or low-speed device on a root port is handed to. Everything USB is the shared core's
// - `drivers::usb_host` - and the class modules under it are the ones xHCI's driver runs.
//
// A FULL- OR LOW-SPEED DEVICE ON A ROOT PORT IS NOT THIS CONTROLLER'S. Its reset leaves the port disabled - or its
// line state says low speed before any reset - and the port is handed to its companion through its owner bit, where
// the companion's own driver finds it arriving. A controller with no companion says so instead. One BEHIND A
// HIGH-SPEED HUB would be reached through that hub's transaction translator by split transactions, which this
// driver does not carry: QEMU has no high-speed hub, so there is nothing to run them against, and the shared core
// refuses such a device by name.
//
// ONE qTD CARRIES A STAGE of at most five pages, and a bulk transfer runs stage by stage, each to its end, so a short
// packet ends the transfer where it happened. A control transfer is one queue, whose data stage names the status
// stage as its alternate next, which is where a short data stage goes.
//
// ITS REGISTERS ARE BAR 0, its schedules and data below 4 GiB - the segment register stays zero - and its interrupt
// the function's INTx on a line other functions share: every wake reads and clears the status before the line is
// acknowledged.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use core::sync::atomic::{Ordering, fence};
use drivers::usb_host::{self, Controller, Dma, Done, Endpoint, Kind, Pipe, Pool, Port, Speed};
use drivers::{common, keys, usb_input};
use rt::*;

// The capability registers.
const CAP_LENGTH: u64 = 0x00;
const CAP_HCSPARAMS: u64 = 0x04;
const CAP_HCCPARAMS: u64 = 0x08;
// The operational registers, from the capability length on.
const USBCMD: u64 = 0x00;
const USBSTS: u64 = 0x04;
const USBINTR: u64 = 0x08;
const FRINDEX: u64 = 0x0c;
const CTRLDSSEGMENT: u64 = 0x10;
const PERIODICLISTBASE: u64 = 0x14;
const ASYNCLISTADDR: u64 = 0x18;
const CONFIGFLAG: u64 = 0x40;
const PORTSC: u64 = 0x44;

const CMD_RS: u32 = 1 << 0;
const CMD_HCRESET: u32 = 1 << 1;
const CMD_PSE: u32 = 1 << 4;
const CMD_ASE: u32 = 1 << 5;
const CMD_IAAD: u32 = 1 << 6;
// An interrupt threshold of one microframe.
const CMD_ITC1: u32 = 1 << 16;
const STS_PCD: u32 = 1 << 2;
const STS_HSE: u32 = 1 << 4;
const STS_IAA: u32 = 1 << 5;
const STS_HALTED: u32 = 1 << 12;
const STS_CHANGES: u32 = 0x3f;
// Completion, error, port change, host system error, async advance.
const INTR_ALL: u32 = 0x37;

const PORT_CCS: u32 = 1 << 0;
const PORT_CSC: u32 = 1 << 1;
const PORT_PE: u32 = 1 << 2;
const PORT_PEC: u32 = 1 << 3;
const PORT_OCC: u32 = 1 << 5;
const PORT_PR: u32 = 1 << 8;
const PORT_LINE: u32 = 3 << 10;
const PORT_LINE_K: u32 = 1 << 10;
const PORT_PP: u32 = 1 << 12;
const PORT_OWNER: u32 = 1 << 13;
const PORT_WC: u32 = PORT_CSC | PORT_PEC | PORT_OCC;

// Links: terminate, and the type field naming a queue head.
const LINK_T: u32 = 1 << 0;
const LINK_QH: u32 = 1 << 1;

// A queue head's characteristics: the speed codes, toggle from the qTD, head of reclamation, the control-endpoint
// flag a full- or low-speed control endpoint needs, and one transaction per microframe.
const EPS_FULL: u32 = 0;
const EPS_LOW: u32 = 1;
const EPS_HIGH: u32 = 2;
const QH_DTC: u32 = 1 << 14;
const QH_HEAD: u32 = 1 << 15;
const QH_CONTROL: u32 = 1 << 27;
const QH_MULT1: u32 = 1 << 30;

// A qTD's token.
const TOKEN_ACTIVE: u32 = 1 << 7;
const TOKEN_HALTED: u32 = 1 << 6;
const TOKEN_ERRORS: u32 = 0x3c; // buffer, babble, transaction, missed microframe
const PID_OUT: u32 = 0 << 8;
const PID_IN: u32 = 1 << 8;
const PID_SETUP: u32 = 2 << 8;
const TOKEN_CERR3: u32 = 3 << 10;
const TOKEN_IOC: u32 = 1 << 15;
const TOKEN_DT: u32 = 1 << 31;

// The slot offsets of a queue head: its horizontal link, characteristics, capabilities, current qTD and the overlay -
// next, alternate next, token and the five buffer pointers.
const QH_LINK: u32 = 0;
const QH_CHARS: u32 = 4;
const QH_CAPS: u32 = 8;
const QH_CURRENT: u32 = 12;
const QH_NEXT: u32 = 16;
const QH_ALT: u32 = 20;
const QH_TOKEN: u32 = 24;

const SLOTS: u32 = 256;
const SLOT: u32 = 64;
const SETUP_SLOT: u32 = 0;
const MAX_PIPES: usize = 64;
// Five pages a qTD carries, less where its buffer starts in the first.
const QTD_PAGES: u32 = 5;
const CONTROL_TICKS: u64 = 100;
const BULK_TICKS: u64 = 500;

// ONE PIPE: its endpoint, its queue head, whether it is on the periodic schedule, and a standing interrupt transfer's
// qTD with its length.
struct PipeState {
	endpoint: Endpoint,
	qh: u32,
	periodic: bool,
	standing: Option<(u32, u32)>,
}

struct Ehci {
	op: u64,
	irq: u64,
	device: u64,
	ports: u32,
	companions: u32,
	pool: Pool,
	frames: Dma,
	async_anchor: u32,
	periodic_anchor: u32,
	pipes: Vec<Option<PipeState>>,
	port_changed: bool,
	advanced: bool,
	said_error: bool,
}

// A qTD as this driver queued it: its slot and its length.
#[derive(Clone, Copy)]
struct Queued {
	qtd: u32,
	len: u32,
}

impl Ehci {
	fn reg(&self, offset: u64) -> u32 {
		// SAFETY: the operational registers, inside the window mapped for this process.
		unsafe { ((self.op + offset) as *const u32).read_volatile() }
	}

	fn set(&self, offset: u64, value: u32) {
		// SAFETY: as `reg`.
		unsafe { ((self.op + offset) as *mut u32).write_volatile(value) }
	}

	fn portsc(&self, port: u32) -> u32 {
		self.reg(PORTSC + (port as u64 - 1) * 4)
	}

	fn set_portsc(&self, port: u32, value: u32) {
		self.set(PORTSC + (port as u64 - 1) * 4, value);
	}

	// STOP AND RESET THE CONTROLLER AND BUILD ITS SCHEDULES: the asynchronous ring's head of reclamation, the frame
	// list - every entry leading to the periodic anchor - both schedules enabled with the controller running, every
	// root port routed to this controller, and the ports powered where it switches their power.
	fn bring_up(base: u64, irq: u64, device: u64) -> Option<Ehci> {
		// SAFETY: the capability registers, at the head of the window.
		let (caplength, hcs, hcc) = unsafe { (((base + CAP_LENGTH) as *const u8).read_volatile() as u64, ((base + CAP_HCSPARAMS) as *const u32).read_volatile(), ((base + CAP_HCCPARAMS) as *const u32).read_volatile()) };
		let op = base + caplength;
		let reg = |offset: u64| unsafe { ((op + offset) as *const u32).read_volatile() };
		let set = |offset: u64, value: u32| unsafe { ((op + offset) as *mut u32).write_volatile(value) };
		// A LEGACY-SUPPORT CAPABILITY is in the function's configuration space, which this driver does not reach: a
		// firmware that holds the controller through it is not asked to let go. Said, since the next line may be why.
		if (hcc >> 8) & 0xff >= 0x40 {
			print(b"driver.ehci: the controller has a legacy-support capability this driver cannot reach - a firmware holding it is not asked to let go\n");
		}
		set(USBINTR, 0);
		set(USBCMD, reg(USBCMD) & !CMD_RS);
		let deadline = clock() + 10;
		while reg(USBSTS) & STS_HALTED == 0 {
			if clock() >= deadline {
				return None;
			}
			yield_now();
		}
		set(USBCMD, CMD_HCRESET);
		let deadline = clock() + 50;
		while reg(USBCMD) & CMD_HCRESET != 0 {
			if clock() >= deadline {
				return None;
			}
			yield_now();
		}
		// HALTED AND RESET: nothing a previous instance pointed it at is in flight.
		device_quiesced(device);
		let pool = Pool::new(device, SLOT, SLOTS)?;
		let frames = usb_host::dma_below_4g(device, 4096)?;
		let mut ehci = Ehci { op, irq, device, ports: hcs & 0xf, companions: (hcs >> 12) & 0xf, pool, frames, async_anchor: 0, periodic_anchor: 0, pipes: (0..MAX_PIPES).map(|_| None).collect(), port_changed: false, advanced: false, said_error: false };
		let setup = ehci.pool.take()?;
		debug_assert_eq!(setup, SETUP_SLOT);
		// THE HEAD OF RECLAMATION, linked to itself and doing nothing: its overlay halted.
		let anchor = ehci.pool.take()?;
		ehci.pool.write(anchor, QH_LINK, ehci.pool.phys(anchor) | LINK_QH);
		ehci.pool.write(anchor, QH_CHARS, EPS_HIGH << 12 | QH_HEAD | 64 << 16);
		ehci.pool.write(anchor, QH_CAPS, QH_MULT1);
		ehci.pool.write(anchor, QH_NEXT, LINK_T);
		ehci.pool.write(anchor, QH_ALT, LINK_T);
		ehci.pool.write(anchor, QH_TOKEN, TOKEN_HALTED);
		ehci.async_anchor = anchor;
		// THE PERIODIC ANCHOR: every frame leads to it, it is never due, and the interrupt pipes hang behind it.
		let periodic = ehci.pool.take()?;
		ehci.pool.write(periodic, QH_LINK, LINK_T);
		ehci.pool.write(periodic, QH_CHARS, EPS_HIGH << 12 | 64 << 16);
		ehci.pool.write(periodic, QH_CAPS, QH_MULT1);
		ehci.pool.write(periodic, QH_NEXT, LINK_T);
		ehci.pool.write(periodic, QH_ALT, LINK_T);
		ehci.pool.write(periodic, QH_TOKEN, TOKEN_HALTED);
		ehci.periodic_anchor = periodic;
		let entry = ehci.pool.phys(periodic) | LINK_QH;
		for at in 0..1024u64 {
			// SAFETY: the frame list page, which the controller does not walk until it runs below.
			unsafe { ((ehci.frames.virt + at * 4) as *mut u32).write_volatile(entry) };
		}
		fence(Ordering::SeqCst);
		set(CTRLDSSEGMENT, 0);
		set(PERIODICLISTBASE, ehci.frames.phys);
		set(ASYNCLISTADDR, ehci.pool.phys(anchor));
		set(USBSTS, STS_CHANGES);
		set(USBCMD, CMD_ITC1 | CMD_PSE | CMD_ASE | CMD_RS);
		let deadline = clock() + 10;
		while reg(USBSTS) & STS_HALTED != 0 {
			if clock() >= deadline {
				return None;
			}
			yield_now();
		}
		// EVERY ROOT PORT IS THIS CONTROLLER'S until a reset says a device on one is not high speed.
		set(CONFIGFLAG, 1);
		sleep_until(clock() + 1);
		if hcs & (1 << 4) != 0 {
			for port in 1..=ehci.ports {
				let value = ehci.portsc(port);
				ehci.set_portsc(port, (value & !PORT_WC) | PORT_PP);
			}
			sleep_until(clock() + 2);
		}
		set(USBINTR, INTR_ALL);
		Some(ehci)
	}

	// WAIT FOR TWO FRAMES TO PASS, so a queue head taken out of the periodic schedule is in no frame the controller is
	// still walking.
	fn frames_pass(&self) {
		let start = self.reg(FRINDEX) >> 3;
		let deadline = clock() + 2;
		while (self.reg(FRINDEX) >> 3).wrapping_sub(start) & 0x3ff < 2 && clock() < deadline {
			yield_now();
		}
	}

	// THE ASYNCHRONOUS SCHEDULE HAS LET GO: the doorbell rung and its answer waited for, so a queue head unlinked from
	// the ring - or one whose overlay was rewritten - is in no cache the controller still reads from.
	fn async_advance(&mut self) {
		self.advanced = false;
		self.set(USBCMD, self.reg(USBCMD) | CMD_IAAD);
		let irq = self.irq;
		if !usb_host::settle(self, irq, 10, |ehci| ehci.advanced) {
			print(b"driver.ehci: the asynchronous schedule did not answer its doorbell\n");
		}
	}

	fn qtd_write(&self, qtd: u32, next: u32, alt: u32, token: u32, buffer: u32) {
		self.pool.write(qtd, 0, next);
		self.pool.write(qtd, 4, alt);
		self.pool.write(qtd, 8, token);
		for page in 0..QTD_PAGES {
			let pointer = if page == 0 { buffer } else { (buffer & !0xfff) + page * 4096 };
			self.pool.write(qtd, 12 + page * 4, pointer);
		}
	}

	// QUEUE qTDs UNDER A PIPE'S QUEUE HEAD: each its token and buffer, linked in order, the alternate next of each
	// pointing at the last - the status stage of a control transfer, the stage after which nothing runs for any other
	// - and the overlay given the first, which is what an idle queue head fetches. None when the pool cannot hold them.
	fn queue(&mut self, at: usize, stages: &[(u32, u32, u32)]) -> Option<Vec<Queued>> {
		if self.pool.free() < stages.len() {
			return None;
		}
		let qh = self.pipes[at].as_ref()?.qh;
		let slots: Vec<u32> = stages.iter().map(|_| self.pool.take()).collect::<Option<Vec<u32>>>()?;
		let last = *slots.last()?;
		let mut queued = Vec::new();
		for (index, &(token, buffer, len)) in stages.iter().enumerate() {
			let next = if index + 1 < slots.len() { self.pool.phys(slots[index + 1]) } else { LINK_T };
			let alt = if slots[index] == last { LINK_T } else { self.pool.phys(last) };
			self.qtd_write(slots[index], next, alt, token | len << 16 | TOKEN_ACTIVE | TOKEN_CERR3, buffer);
			queued.push(Queued { qtd: slots[index], len });
		}
		fence(Ordering::SeqCst);
		// THE OVERLAY KEEPS ITS TOGGLE: for a bulk or interrupt pipe it is where the toggle lives.
		let toggle = self.pool.read(qh, QH_TOKEN) & TOKEN_DT;
		self.pool.write(qh, QH_ALT, LINK_T);
		self.pool.write(qh, QH_NEXT, self.pool.phys(slots[0]));
		self.pool.write(qh, QH_TOKEN, toggle);
		Some(queued)
	}

	// WHERE A QUEUE STANDS: None while a qTD is active; once it ended, the bytes moved and whether it stalled or failed.
	// A short IN stage ends it - its remaining length is not zero - except a control transfer's data stage, whose
	// alternate next is the status stage that still runs.
	fn outcome(&self, queued: &[Queued]) -> Option<(u32, Option<bool>)> {
		let mut moved = 0;
		let last = queued.len().saturating_sub(1);
		for (index, entry) in queued.iter().enumerate() {
			let token = self.pool.read(entry.qtd, 8);
			if token & TOKEN_ACTIVE != 0 {
				return None;
			}
			let left = (token >> 16) & 0x7fff;
			moved += entry.len.saturating_sub(left);
			if token & TOKEN_HALTED != 0 {
				return Some((moved, Some(token & TOKEN_ERRORS == 0)));
			}
			if left != 0 && index < last && token & (3 << 8) == PID_IN {
				// A SHORT STAGE: the controller went to its alternate next - the last qTD - and only that one is left to
				// look at. A queue with no stage after it ends here.
				let tail = self.pool.read(queued[last].qtd, 8);
				if queued.len() >= 2 && self.pool.read(entry.qtd, 4) & LINK_T == 0 {
					if tail & TOKEN_ACTIVE != 0 {
						return None;
					}
					if tail & TOKEN_HALTED != 0 {
						return Some((moved, Some(tail & TOKEN_ERRORS == 0)));
					}
				}
				return Some((moved, None));
			}
		}
		Some((moved, None))
	}

	// AFTER A QUEUE ENDED: the overlay emptied and any halt cleared - keeping its toggle - and the qTDs given back once
	// the schedule has let go of them.
	fn settle_queue(&mut self, at: usize, queued: &[Queued]) {
		let Some((qh, periodic)) = self.pipes[at].as_ref().map(|state| (state.qh, state.periodic)) else { return };
		let toggle = self.pool.read(qh, QH_TOKEN) & TOKEN_DT;
		self.pool.write(qh, QH_NEXT, LINK_T);
		self.pool.write(qh, QH_ALT, LINK_T);
		self.pool.write(qh, QH_TOKEN, toggle);
		if periodic {
			self.frames_pass();
		} else {
			self.async_advance();
		}
		for entry in queued {
			self.pool.give(entry.qtd);
		}
	}

	fn run_queue(&mut self, at: usize, stages: &[(u32, u32, u32)], ticks: u64) -> Option<(u32, Option<bool>)> {
		let queued = self.queue(at, stages)?;
		let irq = self.irq;
		let ended = usb_host::settle(self, irq, ticks, |ehci| ehci.outcome(&queued).is_some());
		let result = if ended { self.outcome(&queued) } else { None };
		self.settle_queue(at, &queued);
		result
	}

	fn index(pipe: Pipe) -> usize {
		pipe as usize - 1
	}

	// HAND A ROOT PORT TO ITS COMPANION, which is where a full- or low-speed device on it is driven - said, since the
	// device arrives on another controller's bus.
	fn release(&self, port: u32, what: &[u8]) {
		let mut line: common::Bounded<128> = common::Bounded::new();
		line.push(b"driver.ehci: port ");
		line.decimal(port as u64);
		line.push(b" carries a ");
		line.push(what);
		if self.companions == 0 {
			line.push(b" device and this controller has no companion to hand it to - it is left unbound\n");
			print(line.as_bytes());
			return;
		}
		line.push(b" device - handed to its companion controller\n");
		print(line.as_bytes());
		let value = self.portsc(port);
		self.set_portsc(port, (value & !PORT_WC) | PORT_OWNER);
	}

	// The schedule mask of an interrupt queue head: every microframe the endpoint's interval asks for, at most one frame
	// apart - an interval longer than a frame is polled every frame, which is never later than it asked.
	fn smask(interval: u8) -> u32 {
		match interval {
			0 | 1 => 0xff,
			2 => 0x55,
			3 => 0x11,
			_ => 0x01,
		}
	}
}

impl Controller for Ehci {
	fn who(&self) -> &'static [u8] {
		b"driver.ehci"
	}

	fn ports(&self) -> u32 {
		self.ports
	}

	fn port(&mut self, port: u32) -> Port {
		let value = self.portsc(port);
		// A PORT ITS COMPANION OWNS is not this controller's to report: it shows nothing connected.
		if value & PORT_OWNER != 0 {
			return Port { connected: false, enabled: false, changed: value & PORT_CSC != 0 };
		}
		Port { connected: value & PORT_CCS != 0, enabled: value & PORT_PE != 0, changed: value & (PORT_CSC | PORT_PEC) != 0 }
	}

	// THE ROOT PORT RESET: a port whose line already says low speed is handed over without one; otherwise fifty
	// milliseconds of reset, its end waited for, and a port the reset left disabled carries a full-speed device - the
	// companion's too. One left enabled carries a high-speed device, this controller's.
	fn reset_port(&mut self, port: u32) -> Option<Speed> {
		let value = self.portsc(port);
		if value & PORT_CCS == 0 || value & PORT_OWNER != 0 {
			return None;
		}
		if value & PORT_LINE == PORT_LINE_K {
			self.release(port, b"low-speed");
			return None;
		}
		self.set_portsc(port, (value & !PORT_WC & !PORT_PE) | PORT_PR);
		sleep_until(clock() + 5);
		self.set_portsc(port, self.portsc(port) & !PORT_WC & !PORT_PR);
		let deadline = clock() + 2;
		while self.portsc(port) & PORT_PR != 0 {
			if clock() >= deadline {
				return None;
			}
			yield_now();
		}
		sleep_until(clock() + 1);
		let value = self.portsc(port);
		if value & PORT_CCS == 0 {
			return None;
		}
		if value & PORT_PE == 0 {
			self.release(port, b"full-speed");
			return None;
		}
		Some(Speed::High)
	}

	fn acknowledge_port(&mut self, port: u32) {
		let value = self.portsc(port);
		self.set_portsc(port, (value & !PORT_WC) | (value & PORT_WC));
	}

	// A PIPE IS A QUEUE HEAD: a control or bulk pipe's on the asynchronous ring, right after its head of reclamation;
	// an interrupt pipe's behind the periodic anchor, with the schedule mask its interval asks for.
	fn open(&mut self, endpoint: &Endpoint) -> Option<Pipe> {
		let at = self.pipes.iter().position(Option::is_none)?;
		let qh = self.pool.take()?;
		let eps = match endpoint.speed {
			Speed::High => EPS_HIGH,
			Speed::Full => EPS_FULL,
			Speed::Low => EPS_LOW,
		};
		let control = endpoint.kind == Kind::Control;
		let mut chars = endpoint.address as u32 & 0x7f | (endpoint.number as u32 & 0xf) << 8 | eps << 12 | (endpoint.max_packet as u32 & 0x7ff) << 16;
		if control {
			chars |= QH_DTC;
			if endpoint.speed != Speed::High {
				chars |= QH_CONTROL;
			}
		}
		let mut caps = QH_MULT1;
		let periodic = endpoint.kind == Kind::Interrupt;
		if periodic {
			caps |= Self::smask(endpoint.interval);
		}
		if let Some(tt) = endpoint.tt {
			caps |= (tt.hub as u32 & 0x7f) << 16 | (tt.port as u32 & 0x7f) << 23;
		}
		self.pool.write(qh, QH_CHARS, chars);
		self.pool.write(qh, QH_CAPS, caps);
		self.pool.write(qh, QH_CURRENT, 0);
		self.pool.write(qh, QH_NEXT, LINK_T);
		self.pool.write(qh, QH_ALT, LINK_T);
		self.pool.write(qh, QH_TOKEN, 0);
		let anchor = if periodic { self.periodic_anchor } else { self.async_anchor };
		self.pool.write(qh, QH_LINK, self.pool.read(anchor, QH_LINK));
		fence(Ordering::SeqCst);
		self.pool.write(anchor, QH_LINK, self.pool.phys(qh) | LINK_QH);
		self.pipes[at] = Some(PipeState { endpoint: *endpoint, qh, periodic, standing: None });
		Some(at as Pipe + 1)
	}

	// UNLINKED FROM BEHIND WHATEVER PRECEDES IT, the schedule's letting go waited for, and its descriptors given back.
	fn close(&mut self, pipe: Pipe) {
		let Some(state) = self.pipes.get_mut(Self::index(pipe)).and_then(Option::take) else { return };
		let anchor = if state.periodic { self.periodic_anchor } else { self.async_anchor };
		let target = self.pool.phys(state.qh) | LINK_QH;
		let mut at = anchor;
		loop {
			let link = self.pool.read(at, QH_LINK);
			if link == target {
				self.pool.write(at, QH_LINK, self.pool.read(state.qh, QH_LINK));
				break;
			}
			if link & LINK_T != 0 {
				break;
			}
			let Some(next) = self.pool.slot_of(link & !0x1f) else { break };
			if next == anchor {
				break;
			}
			at = next;
		}
		if state.periodic {
			self.frames_pass();
		} else {
			self.async_advance();
		}
		if let Some((qtd, _)) = state.standing {
			self.pool.give(qtd);
		}
		self.pool.give(state.qh);
	}

	fn retarget(&mut self, pipe: Pipe, address: u8, max_packet: u16) {
		let Some(state) = self.pipes.get_mut(Self::index(pipe)).and_then(Option::as_mut) else { return };
		state.endpoint.address = address;
		state.endpoint.max_packet = max_packet;
		let qh = state.qh;
		let chars = self.pool.read(qh, QH_CHARS) & !(0x7f | 0x7ff << 16);
		self.pool.write(qh, QH_CHARS, chars | address as u32 & 0x7f | (max_packet as u32 & 0x7ff) << 16);
		// THE CONTROLLER MAY HOLD THE OLD CHARACTERISTICS: the asynchronous schedule lets go before the next transfer.
		self.async_advance();
	}

	// ONE QUEUE: setup on DATA0, the data stage on DATA1 - its alternate next the status stage, which a short answer
	// goes to - and the status stage on DATA1 the other way, interrupting when it ends.
	fn control(&mut self, pipe: Pipe, setup: &[u8; 8], data: Option<(u32, u16)>) -> Done {
		let at = Self::index(pipe);
		if self.pipes.get(at).and_then(Option::as_ref).is_none() {
			return Done::Failed;
		}
		for (word, bytes) in setup.chunks(4).enumerate() {
			self.pool.write(SETUP_SLOT, word as u32 * 4, u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
		}
		let data_in = setup[0] & 0x80 != 0;
		let mut stages = alloc::vec![(PID_SETUP, self.pool.phys(SETUP_SLOT), 8)];
		let with_data = data.filter(|&(_, len)| len > 0);
		if let Some((phys, len)) = with_data {
			stages.push(((if data_in { PID_IN } else { PID_OUT }) | TOKEN_DT, phys, len as u32));
		}
		let status_in = !(data_in && with_data.is_some());
		stages.push(((if status_in { PID_IN } else { PID_OUT }) | TOKEN_DT | TOKEN_IOC, 0, 0));
		match self.run_queue(at, &stages, CONTROL_TICKS) {
			Some((moved, None)) => Done::Ok(moved.saturating_sub(8)),
			Some((moved, Some(true))) => Done::Stall(moved.saturating_sub(8)),
			_ => Done::Failed,
		}
	}

	// A BULK TRANSFER STAGE BY STAGE, each one qTD of at most five pages run to its end, the toggle kept in the queue
	// head's overlay - until all of it moved, a stage came back short, or one failed.
	fn bulk(&mut self, pipe: Pipe, phys: u32, len: u32) -> Done {
		let at = Self::index(pipe);
		let Some(endpoint) = self.pipes.get(at).and_then(Option::as_ref).map(|state| state.endpoint) else { return Done::Failed };
		let pid = if endpoint.dir_in { PID_IN } else { PID_OUT };
		let mut moved = 0;
		loop {
			let at_phys = phys + moved;
			let stage = (len - moved).min(QTD_PAGES * 4096 - (at_phys & 0xfff));
			let Some((part, failed)) = self.run_queue(at, &[(pid | TOKEN_IOC, at_phys, stage)], BULK_TICKS) else { return Done::Failed };
			moved += part;
			match failed {
				Some(true) => return Done::Stall(moved),
				Some(false) => return Done::Failed,
				None if part < stage || moved >= len => return Done::Ok(moved),
				None => continue,
			}
		}
	}

	fn submit(&mut self, pipe: Pipe, phys: u32, len: u32) -> bool {
		let at = Self::index(pipe);
		if self.pipes.get(at).and_then(Option::as_ref).is_none_or(|state| state.standing.is_some()) {
			return false;
		}
		let Some(queued) = self.queue(at, &[(PID_IN | TOKEN_IOC, phys, len)]) else { return false };
		if let Some(state) = self.pipes[at].as_mut() {
			state.standing = Some((queued[0].qtd, len));
		}
		true
	}

	fn reap(&mut self, pipe: Pipe) -> Option<Done> {
		let at = Self::index(pipe);
		let (qtd, len) = self.pipes.get(at)?.as_ref()?.standing?;
		let queued = [Queued { qtd, len }];
		let (moved, failed) = self.outcome(&queued)?;
		if let Some(state) = self.pipes[at].as_mut() {
			state.standing = None;
		}
		self.settle_queue(at, &queued);
		Some(match failed {
			Some(true) => Done::Stall(moved),
			Some(false) => Done::Failed,
			None => Done::Ok(moved),
		})
	}

	// THE TOGGLE LIVES IN THE OVERLAY of a bulk or interrupt queue head, and back to DATA0 is its bit cleared - on an
	// idle queue head, which is the only time a caller asks.
	fn reset_toggle(&mut self, pipe: Pipe) {
		let Some(qh) = self.pipes.get(Self::index(pipe)).and_then(Option::as_ref).map(|state| state.qh) else { return };
		let token = self.pool.read(qh, QH_TOKEN);
		self.pool.write(qh, QH_TOKEN, token & !TOKEN_DT & !TOKEN_HALTED & !TOKEN_ACTIVE);
	}

	// THE STATUS, READ AND CLEARED: a port change remembered, the doorbell's answer noted, and a host system error -
	// the controller saying it stopped - said once.
	fn service(&mut self) {
		let status = self.reg(USBSTS) & STS_CHANGES;
		self.set(USBSTS, status);
		if status & STS_PCD != 0 {
			self.port_changed = true;
		}
		if status & STS_IAA != 0 {
			self.advanced = true;
		}
		if status & STS_HSE != 0 && !self.said_error {
			self.said_error = true;
			print(b"driver.ehci: the controller reported a host system error and stopped\n");
		}
	}

	fn take_port_change(&mut self) -> bool {
		core::mem::take(&mut self.port_changed)
	}

	fn port_poll(&self) -> Option<u64> {
		None
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
	// SAFETY: the device's register window, which the claim minted for this process.
	let base: u64 = unsafe { syscall(SYS_DEVICE_MEMORY_MAP, device, 0, 0, 0) };
	if sys_is_err(base) {
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
	}
	let Some(ehci) = Ehci::bring_up(base, irq, device) else {
		print(b"driver.ehci: the controller did not reset or would not run\n");
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::DeviceNotResponding);
	};
	let Some(host) = usb_host::Host::new(ehci) else { common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::OutOfMemory) };
	usb_host::run(bootstrap, &bind, device, irq, host)
}
