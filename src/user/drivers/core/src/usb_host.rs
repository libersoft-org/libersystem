// THE USB CORE ABOVE ONE HOST-CONTROLLER INTERFACE: what EHCI, OHCI and UHCI share, and the interface each of them
// implements.
//
// A HOST CONTROLLER MOVES BYTES and does nothing else here: it resets and reports its root ports, keeps a schedule
// entry per pipe, runs one control or bulk transfer to its end, keeps an interrupt transfer standing and says when
// it ended. Everything that is USB rather than one controller's register file is above it, once: a device is
// addressed with SET_ADDRESS and read, a hub is configured and its ports powered, reset and watched, a device that
// leaves takes everything behind it with it, and a device is bound by the same class policy, descriptor walks and
// class state the xHCI driver binds it with - `usb_class` for which module may take it, `usb_input` for a HID
// interface's reports, `usb_bot` for a Bulk-Only disk. Nothing in a class module is forked for a controller.
//
// xHCI IS NOT A `Controller`, AND DELIBERATELY. It addresses a device itself (Address Device), routes by a route
// string instead of an address and owns its endpoints through contexts - so what it shares is the layer above the
// transfer: the same class policy and the same class state, which it reaches through its own transfer rings.
//
// WHAT THIS CORE BINDS: hubs, HID (keyboards, pointers, touch surfaces, gamepads) and Bulk-Only storage - the
// classes the earlier controllers carry. A device of any other class is addressed, said, and left in the
// inventory, and the line names the class.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use proto::system::usb;
use proto::system::{Error as UsbError, UsbDevice as UsbEntry};
use rt::*;

use crate::common;
use crate::descriptor;
use crate::usb_bot::{self, At, Moved};
use crate::usb_class::{Budget, ClassKind};
use crate::usb_input::{self, Input, Pads};

/// How fast a device talks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speed {
	Low,
	Full,
	High,
}

impl Speed {
	/// The inventory's word for it - the one `lsusb` prints for an xHCI device of the same speed.
	pub fn name(self) -> &'static str {
		match self {
			Speed::Low => "low",
			Speed::Full => "full",
			Speed::High => "high",
		}
	}

	// Endpoint zero's packet size until the device descriptor names the real one: 64 at high speed, 8 below it.
	fn initial_packet(self) -> u16 {
		if self == Speed::High { 64 } else { 8 }
	}
}

/// What kind of endpoint a pipe is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
	Control,
	Bulk,
	Interrupt,
}

/// THE TRANSACTION TRANSLATOR a full- or low-speed device behind a high-speed hub is reached through: that hub's
/// address and the port the device's branch hangs off. Only EHCI has a use for it, as split transactions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tt {
	pub hub: u8,
	pub port: u8,
}

/// One endpoint, as a controller is asked to keep a pipe for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Endpoint {
	pub address: u8,
	pub number: u8,
	pub dir_in: bool,
	pub kind: Kind,
	pub max_packet: u16,
	pub speed: Speed,
	// `bInterval` as the endpoint descriptor has it, which each controller turns into its own schedule.
	pub interval: u8,
	pub tt: Option<Tt>,
}

/// How a transfer ended: every byte asked for or fewer (`Ok`, with how many moved), a stall after some, or a failure
/// that is no stall - a timeout, a device gone, an error the bus reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Done {
	Ok(u32),
	Stall(u32),
	Failed,
}

/// A root port as the controller reports it: whether a device is connected, whether the port is enabled, and whether
/// its connection changed since the change was last acknowledged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Port {
	pub connected: bool,
	pub enabled: bool,
	pub changed: bool,
}

/// DMA memory a controller can name - every one of the three puts 32 address bits on the bus.
#[derive(Clone, Copy, Debug)]
pub struct Dma {
	pub handle: u64,
	pub virt: u64,
	pub phys: u32,
	pub bytes: u32,
}

impl Dma {
	/// The first `len` bytes, as a slice.
	///
	/// # Safety
	/// Nothing may be writing the buffer - no transfer in flight into it - for as long as the slice lives.
	pub unsafe fn bytes(&self, len: usize) -> &[u8] {
		unsafe { core::slice::from_raw_parts(self.virt as *const u8, len.min(self.bytes as usize)) }
	}

	/// # Safety
	/// As `bytes`, and no other reference into the buffer may be live.
	#[allow(clippy::mut_from_ref)]
	pub unsafe fn bytes_mut(&self, len: usize) -> &mut [u8] {
		unsafe { core::slice::from_raw_parts_mut(self.virt as *mut u8, len.min(self.bytes as usize)) }
	}

	pub fn release(&mut self) {
		if self.handle != 0 {
			close(self.handle);
			self.handle = 0;
		}
	}
}

/// DMA MEMORY BELOW 4 GiB for `device`, the controller's capability - see `rt::dma_buffer_below_4g`. EHCI, OHCI and
/// UHCI all put 32 address bits on the bus, so every page any of them is given is one of these.
pub fn dma_below_4g(device: u64, bytes: u32) -> Option<Dma> {
	// SAFETY: a DMA buffer for this process's controller, mapped for this process.
	let (handle, virt, phys) = unsafe { dma_buffer_below_4g(device, bytes as u64) }?;
	if phys >> 32 != 0 {
		close(handle);
		return None;
	}
	Some(Dma { handle, virt, phys: phys as u32, bytes })
}

/// THE CONTROLLER'S DESCRIPTORS, in one DMA buffer cut into equal slots - a UHCI's queue heads and transfer
/// descriptors, an OHCI's endpoint and transfer descriptors, an EHCI's queue heads and qTDs. A slot is the largest
/// descriptor's size, aligned to it, so any descriptor fits any slot. FIXED AT BRING-UP, which is the bound on
/// what the schedule can hold: a transfer that cannot have its descriptors waits for none, it is refused.
pub struct Pool {
	memory: Dma,
	slot: u32,
	used: Vec<bool>,
}

impl Pool {
	/// `count` slots of `slot` bytes, a power of two.
	pub fn new(device: u64, slot: u32, count: u32) -> Option<Pool> {
		let memory = dma_below_4g(device, (slot * count).next_multiple_of(4096))?;
		// SAFETY: the buffer was just made and nothing else references it.
		unsafe { memory.bytes_mut(memory.bytes as usize) }.fill(0);
		Some(Pool { memory, slot, used: alloc::vec![false; count as usize] })
	}

	/// A free slot, zeroed.
	pub fn take(&mut self) -> Option<u32> {
		let at = self.used.iter().position(|used| !used)?;
		self.used[at] = true;
		for word in 0..self.slot / 4 {
			self.write(at as u32, word * 4, 0);
		}
		Some(at as u32)
	}

	/// Give a slot back. The caller has made sure the controller no longer reaches it.
	pub fn give(&mut self, at: u32) {
		self.used[at as usize] = false;
	}

	/// How many slots are free.
	pub fn free(&self) -> usize {
		self.used.iter().filter(|used| !**used).count()
	}

	/// The address the controller names slot `at` by.
	pub fn phys(&self, at: u32) -> u32 {
		self.memory.phys + at * self.slot
	}

	/// The slot whose controller address is `phys`, if one is.
	pub fn slot_of(&self, phys: u32) -> Option<u32> {
		let offset = phys.checked_sub(self.memory.phys)?;
		(offset % self.slot == 0 && offset / self.slot < self.used.len() as u32).then_some(offset / self.slot)
	}

	/// One 32-bit word of a slot, at a byte offset inside it.
	pub fn read(&self, at: u32, offset: u32) -> u32 {
		// SAFETY: inside the pool, aligned, and the controller writes these words only as whole words.
		unsafe { ((self.memory.virt + (at * self.slot + offset) as u64) as *const u32).read_volatile() }
	}

	pub fn write(&self, at: u32, offset: u32, value: u32) {
		// SAFETY: as `read`.
		unsafe { ((self.memory.virt + (at * self.slot + offset) as u64) as *mut u32).write_volatile(value) }
	}
}

/// WAIT, BOUNDED, UNTIL `done` - on the controller's interrupt. Each wake has the controller's status read and
/// cleared (`service`) before the line is acknowledged, which is what a level line shared with other functions needs,
/// and the wait is a tick at most, so a transfer whose ending raised nothing is looked at again within one. A few
/// quick looks come first: an emulated controller ends most transfers within a frame. False when `ticks` passed first.
pub fn settle<C: Controller>(hc: &mut C, irq: u64, ticks: u64, mut done: impl FnMut(&mut C) -> bool) -> bool {
	for _ in 0..8 {
		if done(hc) {
			return true;
		}
		yield_now();
	}
	let deadline = clock().saturating_add(ticks);
	loop {
		if done(hc) {
			return true;
		}
		let now = clock();
		if now >= deadline {
			return false;
		}
		let _ = wait(irq, now.saturating_add(1).min(deadline));
		hc.service();
		interrupt_ack(irq);
	}
}

/// A controller's handle on one of its pipes.
pub type Pipe = u16;

/// THE HOST-CONTROLLER INTERFACE. Every transfer is bounded: one that does not end in its time answers `Failed`, and
/// the pipe is the controller's to clean up so the next transfer on it starts from a known state.
pub trait Controller {
	/// The name this controller's lines are said by - `driver.uhci`.
	fn who(&self) -> &'static [u8];
	/// How many root ports it has.
	fn ports(&self) -> u32;
	/// A root port's state now.
	fn port(&mut self, port: u32) -> Port;
	/// Reset a root port and enable it, answering the speed of what came up; None when nothing did - or when the
	/// port was handed to a companion controller, which says so.
	fn reset_port(&mut self, port: u32) -> Option<Speed>;
	/// Acknowledge a root port's change.
	fn acknowledge_port(&mut self, port: u32);
	/// Keep a pipe for an endpoint; None when the controller's pool of them is spent.
	fn open(&mut self, endpoint: &Endpoint) -> Option<Pipe>;
	/// Take a pipe out of the schedule, its transfer abandoned, and give its schedule entry back.
	fn close(&mut self, pipe: Pipe);
	/// Endpoint zero's pipe after SET_ADDRESS: its new address, and the packet size the device descriptor named.
	fn retarget(&mut self, pipe: Pipe, address: u8, max_packet: u16);
	/// One control transfer on a control pipe: the setup packet, a data stage of `len` bytes at `phys` in the
	/// direction the setup's first byte says, and the status stage.
	fn control(&mut self, pipe: Pipe, setup: &[u8; 8], data: Option<(u32, u16)>) -> Done;
	/// One bulk transfer of `len` bytes at `phys`, in the pipe's direction.
	fn bulk(&mut self, pipe: Pipe, phys: u32, len: u32) -> Done;
	/// Start an interrupt IN transfer of `len` bytes at `phys`, which ends on its own.
	fn submit(&mut self, pipe: Pipe, phys: u32, len: u32) -> bool;
	/// A started interrupt transfer's end, once it has one - and the pipe free for the next.
	fn reap(&mut self, pipe: Pipe) -> Option<Done>;
	/// The pipe's data toggle back to DATA0, after the device's halt was cleared.
	fn reset_toggle(&mut self, pipe: Pipe);
	/// THE INTERRUPT: the controller's status read and cleared. A root port's change is remembered for
	/// `take_port_change`, since a synchronous wait inside a transfer may be the one that saw it.
	fn service(&mut self);
	/// Whether a root port changed since the last call.
	fn take_port_change(&mut self) -> bool;
	/// For a controller that raises nothing when a root port changes - UHCI - how often its ports are looked at.
	fn port_poll(&self) -> Option<u64>;
	/// DMA memory it can name.
	fn dma(&mut self, bytes: u32) -> Option<Dma>;
	/// The longest bulk transfer it carries in one call.
	fn max_transfer(&self) -> u32;
	/// Whether it carries split transactions, which a full- or low-speed device behind a high-speed hub needs.
	fn splits(&self) -> bool;
	/// Stop the controller: no schedule runs and nothing masters the bus. Answers whether it confirmed.
	fn halt(&mut self) -> bool;
	/// Run it again after a sleep that kept its power.
	fn run(&mut self) -> bool;
}

// Standard requests and the hub class's.
const REQ_GET_STATUS: u8 = 0;
const REQ_CLEAR_FEATURE: u8 = 1;
const REQ_SET_FEATURE: u8 = 3;
const REQ_SET_ADDRESS: u8 = 5;
const REQ_GET_DESCRIPTOR: u8 = 6;
const REQ_SET_CONFIGURATION: u8 = 9;
const DESC_DEVICE: u16 = 1;
const DESC_CONFIG: u16 = 2;
const DESC_HUB: u16 = 0x29;
const FEATURE_ENDPOINT_HALT: u16 = 0;
const FEATURE_DEVICE_REMOTE_WAKEUP: u16 = 1;
const RT_ENDPOINT: u8 = 0x02;
const RT_CLASS_DEVICE_IN: u8 = 0xa0;
const RT_CLASS_PORT: u8 = 0x23;
const RT_CLASS_PORT_IN: u8 = 0xa3;
const CLASS_HUB: u8 = 9;
const HUB_FEAT_PORT_RESET: u16 = 4;
const HUB_FEAT_PORT_POWER: u16 = 8;
const HUB_FEAT_C_CONNECTION: u16 = 16;
const HUB_FEAT_C_ENABLE: u16 = 17;
const HUB_FEAT_C_SUSPEND: u16 = 18;
const HUB_FEAT_C_OVER_CURRENT: u16 = 19;
const HUB_FEAT_C_RESET: u16 = 20;
const PORT_STATUS_CONNECTION: u16 = 1 << 0;
const PORT_STATUS_ENABLE: u16 = 1 << 1;
const PORT_STATUS_LOW_SPEED: u16 = 1 << 9;
const PORT_STATUS_HIGH_SPEED: u16 = 1 << 10;
const PORT_CHANGE_CONNECTION: u16 = 1 << 0;
const PORT_CHANGE_RESET: u16 = 1 << 4;

/// The most devices one controller holds addressed, hubs included.
pub const MAX_DEVICES: usize = 16;
// The most ports of one hub this core watches - QEMU's hub has eight.
const MAX_HUB_PORTS: u8 = 15;
// THE SCRATCH PAGE: control data stages in its first half, the Bulk-Only wrapper in the second.
const CONTROL_BYTES: u16 = 2048;
const WRAPPER_AT: u32 = 2048;
// A report page is a page; a hub's status-change bitmap is a byte per eight ports.
const PAGE: u32 = 4096;
// How many times in a row a standing report may fail before it is given up on and said.
const REPORT_RETRIES: u8 = 8;

/// A device's bound role, as the inventory names it - the words xHCI's inventory uses for the same roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
	Device,
	Hub,
	Keyboard,
	Pointer,
	Touch,
	Gamepad,
	Storage,
}

impl Role {
	pub fn name(self) -> &'static str {
		match self {
			Role::Device => "device",
			Role::Hub => "hub",
			Role::Keyboard => "keyboard",
			Role::Pointer => "pointer",
			Role::Touch => "touch",
			Role::Gamepad => "gamepad",
			Role::Storage => "storage",
		}
	}
}

/// ONE ADDRESSED DEVICE: its address, the root port its branch hangs off and - behind a hub - that hub's address and
/// port, its speed and the translator it is reached through, endpoint zero's pipe, what its descriptor says it is,
/// and the role it was bound to.
pub struct Device {
	pub address: u8,
	pub port: u32,
	pub parent: Option<(u8, u8)>,
	pub speed: Speed,
	pub tt: Option<Tt>,
	pub ep0: Pipe,
	pub vendor: u16,
	pub product: u16,
	pub class: u8,
	pub role: Role,
	pub remote_wakeup: bool,
}

// ONE STANDING INPUT REPORT: the device and interface it is for, its interrupt pipe and report page, the shared
// binding's input state, and how many times in a row it failed.
struct Report {
	address: u8,
	endpoint: u8,
	pipe: Pipe,
	page: Dma,
	input: Input,
	failures: u8,
	standing: bool,
}

// ONE CONFIGURED HUB: its address, its port count and power-on time, and its status-change pipe and page.
struct Hub {
	address: u8,
	ports: u8,
	pipe: Option<Pipe>,
	page: Dma,
	standing: bool,
}

// THE BULK-ONLY DISK: its device, its two bulk pipes and their endpoint addresses, and the data buffer the sectors
// move through. The shared transport's unit is held beside it.
struct Disk {
	address: u8,
	pipe_in: Pipe,
	pipe_out: Pipe,
	ep_in: u8,
	ep_out: u8,
	data: Dma,
}

/// A CONTROLLER AND EVERYTHING ON ITS BUS.
pub struct Host<C: Controller> {
	pub hc: C,
	pub devices: Vec<Device>,
	reports: Vec<Report>,
	hubs: Vec<Hub>,
	disk: Option<(Disk, usb_bot::Unit)>,
	pub pads: Pads,
	budget: Budget,
	scratch: Dma,
	used: [bool; 128],
}

impl<C: Controller> Host<C> {
	/// A host over a controller that is running with its schedules empty. None when its scratch page cannot be had.
	pub fn new(mut hc: C) -> Option<Host<C>> {
		let scratch = hc.dma(PAGE)?;
		let mut used = [false; 128];
		used[0] = true;
		Some(Host { hc, devices: Vec::new(), reports: Vec::new(), hubs: Vec::new(), disk: None, pads: Pads::new(), budget: Budget::new(), scratch, used })
	}

	fn say(&self, line: &[u8]) {
		print(self.hc.who());
		print(b": ");
		print(line);
		print(b"\n");
	}

	/// Every root port's device, enumerated - at bring-up.
	pub fn scan(&mut self) {
		for port in 1..=self.hc.ports() {
			let state = self.hc.port(port);
			if state.changed {
				self.hc.acknowledge_port(port);
			}
			if state.connected {
				self.attach_root(port);
			}
		}
	}

	/// Whether any bound device is a keyboard, a pointer, a touch surface, a gamepad or a disk.
	pub fn any(&self, role: Role) -> bool {
		self.devices.iter().any(|device| device.role == role)
	}

	/// Whether any bound HID interface carries what `which` asks of its input state - a pointer, contacts, a gamepad.
	pub fn carries(&self, which: impl Fn(&Input) -> bool) -> bool {
		self.reports.iter().any(|report| which(&report.input))
	}

	/// Whether a bound keyboard's remote wakeup is enabled.
	pub fn remote_wakeup(&self) -> bool {
		self.devices.iter().any(|device| device.remote_wakeup)
	}

	fn attach_root(&mut self, port: u32) {
		let Some(speed) = self.hc.reset_port(port) else { return };
		let _ = self.attach(port, None, None, speed);
	}

	// ------------------------------------------------------------------- the default pipe

	// One control transfer on endpoint zero of `ep0`, its data stage through the scratch page. Answers how many bytes
	// moved; a stall is answered too, as None, and endpoint zero needs no recovery - the next setup packet clears it.
	fn control(&mut self, ep0: Pipe, request_type: u8, request: u8, value: u16, index: u16, len: u16) -> Option<u32> {
		let setup: [u8; 8] = [request_type, request, value as u8, (value >> 8) as u8, index as u8, (index >> 8) as u8, len as u8, (len >> 8) as u8];
		let data = (len != 0).then_some((self.scratch.phys, len.min(CONTROL_BYTES)));
		match self.hc.control(ep0, &setup, data) {
			Done::Ok(moved) => Some(moved),
			Done::Stall(_) | Done::Failed => None,
		}
	}

	// The first `len` bytes the last control transfer brought in, copied out before the next can overwrite them.
	fn scratch(&self, len: u32) -> Vec<u8> {
		// SAFETY: the scratch page; no transfer is in flight into it between control transfers.
		unsafe { self.scratch.bytes(len.min(CONTROL_BYTES as u32) as usize) }.to_vec()
	}

	// ------------------------------------------------------------------- enumeration

	// ADDRESS ONE DEVICE THAT CAME UP ON A PORT and bind it: endpoint zero at address zero, the descriptor's head for
	// its packet size, SET_ADDRESS, the whole descriptor at the new address. Behind a high-speed hub a full- or
	// low-speed device is reached through that hub's translator, and a controller that carries no split
	// transactions refuses it by name. Answers the device's index; None, with everything it took given back, when a
	// step failed or the table is full.
	fn attach(&mut self, port: u32, parent: Option<(u8, u8)>, tt: Option<Tt>, speed: Speed) -> Option<usize> {
		if tt.is_some() && !self.hc.splits() {
			self.say(b"a full- or low-speed device behind a high-speed hub needs split transactions, which this driver does not carry - it is left unbound");
			return None;
		}
		if self.devices.len() >= MAX_DEVICES {
			self.say(b"a device arrived and this controller already holds as many as it addresses - it is left unbound");
			return None;
		}
		let max_packet = speed.initial_packet();
		let ep0 = self.hc.open(&Endpoint { address: 0, number: 0, dir_in: false, kind: Kind::Control, max_packet, speed, interval: 0, tt })?;
		let Some(address) = (1..128u8).find(|&address| !self.used[address as usize]) else {
			self.hc.close(ep0);
			return None;
		};
		let identified = (|| {
			// EIGHT BYTES FIRST, AND THE PACKET SIZE IS THE EIGHTH. A device that returned four has not sent it.
			if self.control(ep0, 0x80, REQ_GET_DESCRIPTOR, DESC_DEVICE << 8, 0, 8)? < 8 {
				return None;
			}
			let mps0 = self.scratch(8)[7] as u16;
			if !matches!(mps0, 8 | 16 | 32 | 64) {
				return None;
			}
			self.control(ep0, 0x00, REQ_SET_ADDRESS, address as u16, 0, 0)?;
			// THE SET_ADDRESS RECOVERY INTERVAL: two milliseconds the device may take before it answers at its new
			// address, rounded up to a tick.
			sleep_until(clock().saturating_add(1));
			self.hc.retarget(ep0, address, mps0);
			// THE IDENTITY RIDES AT OFFSETS FOUR TO ELEVEN, so a short answer is not an identity.
			if self.control(ep0, 0x80, REQ_GET_DESCRIPTOR, DESC_DEVICE << 8, 0, 18)? < 12 {
				return None;
			}
			let bytes = self.scratch(18);
			Some((bytes[4], u16::from_le_bytes([bytes[8], bytes[9]]), u16::from_le_bytes([bytes[10], bytes[11]])))
		})();
		let Some((class, vendor, product)) = identified else {
			self.hc.close(ep0);
			self.say(b"a device did not answer its enumeration - it is left unbound");
			return None;
		};
		self.used[address as usize] = true;
		self.devices.push(Device { address, port, parent, speed, tt, ep0, vendor, product, class, role: Role::Device, remote_wakeup: false });
		let index = self.devices.len() - 1;
		self.report(index);
		self.bind(index);
		Some(index)
	}

	// `driver.uhci: port 1 (hub 1.2) VVVV:PPPP class 0x09 full speed, address 2`.
	fn report(&self, index: usize) {
		let device = &self.devices[index];
		let mut line: common::Bounded<128> = common::Bounded::new();
		line.push(self.hc.who());
		line.push(b": port ");
		line.decimal(device.port as u64);
		if let Some((hub, port)) = device.parent {
			line.push(b" (hub ");
			line.decimal(hub as u64);
			line.push(b".");
			line.decimal(port as u64);
			line.push(b")");
		}
		line.push(b" ");
		line.push(&hex4(device.vendor));
		line.push(b":");
		line.push(&hex4(device.product));
		line.push(b" class 0x");
		line.push(&common::hex2(device.class));
		line.push(b" ");
		line.push(device.speed.name().as_bytes());
		line.push(b" speed, address ");
		line.decimal(device.address as u64);
		line.push(b"\n");
		print(line.as_bytes());
	}

	// ------------------------------------------------------------------- binding

	// BIND AN ADDRESSED DEVICE BY THE SHARED POLICY: a hub is expanded; otherwise its configuration is read and its
	// HID interfaces are offered to the HID module, then a Bulk-Only interface to the storage module - each only
	// while the class budget admits one more - and a device neither takes is left addressed and said.
	fn bind(&mut self, index: usize) {
		let (ep0, class) = (self.devices[index].ep0, self.devices[index].class);
		let Some((head, config)) = self.configuration(ep0) else {
			self.say(b"a device's configuration could not be read - it is left addressed");
			return;
		};
		if class == CLASS_HUB {
			self.expand(index, head.value, &config);
			return;
		}
		let declared = usb_input::declare(&config);
		if !declared.is_empty() && self.budget.admits(ClassKind::Hid, class, self.hc.who()) && self.bind_hid(index, &head, &declared) {
			let _ = self.budget.admit(ClassKind::Hid);
			return;
		}
		if self.disk.is_none()
			&& let Some(found) = usb_bot::find(&config)
			&& self.budget.admits(ClassKind::Storage, class, self.hc.who())
			&& self.bind_storage(index, head.value, &found)
		{
			let _ = self.budget.admit(ClassKind::Storage);
			return;
		}
		let mut line: common::Bounded<96> = common::Bounded::new();
		line.push(b"a device of class 0x");
		line.push(&common::hex2(class));
		line.push(b" carries nothing this controller's driver binds - it is left addressed");
		self.say(line.as_bytes());
	}

	// THE FIRST CONFIGURATION, its head and its whole descriptor, read through the scratch page - the head as a
	// record like any other, and the whole of it arrived before it is walked: the page is reused, so the bytes past a
	// short answer are the previous transfer's.
	fn configuration(&mut self, ep0: Pipe) -> Option<(descriptor::ConfigHead, Vec<u8>)> {
		let head = self.control(ep0, 0x80, REQ_GET_DESCRIPTOR, DESC_CONFIG << 8, 0, 9)?;
		let head = descriptor::config_head(&self.scratch(head)).ok()?;
		let total = head.total.min(CONTROL_BYTES);
		let received = self.control(ep0, 0x80, REQ_GET_DESCRIPTOR, DESC_CONFIG << 8, 0, total)?;
		let total = descriptor::check_transfer(total, received).ok()?;
		Some((head, self.scratch(total as u32)))
	}

	// THE HID MODULE: the configuration selected, then per declared interface an interrupt pipe, a report page and its
	// report descriptor - the layout its reports decode against, chosen by the shared binding - and its first report
	// posted. An interface that carries nothing the system consumes gives its pipe and page back. A keyboard that can
	// wake the machine is told it may, now, while no report is in flight on the device's other interfaces' account.
	fn bind_hid(&mut self, index: usize, head: &descriptor::ConfigHead, declared: &[usb_input::Declared]) -> bool {
		let device = &self.devices[index];
		let (ep0, address, speed, tt, vendor, product, port) = (device.ep0, device.address, device.speed, device.tt, device.vendor, device.product, device.port);
		if self.control(ep0, 0x00, REQ_SET_CONFIGURATION, head.value as u16, 0, 0).is_none() {
			return false;
		}
		let first = self.reports.len();
		for found in declared {
			let Some(endpoint) = found.endpoint else { continue };
			let report_descriptor = match found.desc_len {
				0 => None,
				desc_len => self.control(ep0, usb_input::RT_INTERFACE_IN, REQ_GET_DESCRIPTOR, usb_input::DESC_REPORT << 8, found.iface as u16, desc_len.min(CONTROL_BYTES)).map(|received| self.scratch(received.min(desc_len as u32))),
			};
			let chosen = usb_input::layout_for(report_descriptor.as_deref(), found.boot_keyboard, || self.control(ep0, 0x21, usb_input::HID_REQ_SET_PROTOCOL, 0, found.iface as u16, 0).is_some());
			let Some(layout) = chosen else { continue };
			let pipe = self.hc.open(&Endpoint { address, number: endpoint.address & 0xf, dir_in: true, kind: Kind::Interrupt, max_packet: endpoint.max_packet, speed, interval: endpoint.interval, tt });
			let page = self.hc.dma(PAGE);
			let (Some(pipe), Some(page)) = (pipe, page) else {
				if let Some(pipe) = pipe {
					self.hc.close(pipe);
				}
				if let Some(mut page) = page {
					page.release();
				}
				continue;
			};
			let mut input = Input::new(found.iface, layout, self.hc.who());
			input.attach_pads(vendor, product, port, &mut self.pads);
			self.reports.push(Report { address, endpoint: endpoint.address, pipe, page, input, failures: 0, standing: false });
		}
		if self.reports.len() == first {
			return false;
		}
		let bound = &self.reports[first..];
		let role = if bound.iter().any(|report| report.input.layout.has_keyboard()) {
			Role::Keyboard
		} else if bound.iter().any(|report| report.input.layout.has_digitizer()) {
			Role::Touch
		} else if bound.iter().any(|report| report.input.has_gamepad()) {
			Role::Gamepad
		} else {
			Role::Pointer
		};
		if head.remote_wakeup && role == Role::Keyboard {
			self.devices[index].remote_wakeup = self.control(ep0, 0x00, REQ_SET_FEATURE, FEATURE_DEVICE_REMOTE_WAKEUP, 0, 0).is_some();
		}
		self.devices[index].role = role;
		for at in first..self.reports.len() {
			self.post(at);
		}
		true
	}

	// Post a report's next transfer, sized by its layout.
	fn post(&mut self, at: usize) {
		let report = &mut self.reports[at];
		let len = report.input.layout.report_bytes().clamp(1, usb_input::MAX_REPORT as u32);
		report.standing = self.hc.submit(report.pipe, report.page.phys, len);
	}

	/// EVERY STANDING REPORT THAT ENDED, decoded through the shared binding and posted again. A stalled one is
	/// unhalted - CLEAR_FEATURE(ENDPOINT_HALT) and the toggle back to DATA0 - and posted again; one that failed is
	/// posted again until it has failed `REPORT_RETRIES` times in a row, and then said and left.
	pub fn reap_reports(&mut self) {
		for at in 0..self.reports.len() {
			if !self.reports[at].standing {
				continue;
			}
			let Some(done) = self.hc.reap(self.reports[at].pipe) else { continue };
			self.reports[at].standing = false;
			match done {
				Done::Ok(moved) => {
					let report = &mut self.reports[at];
					report.failures = 0;
					// SAFETY: the transfer into the page has ended and the next is not posted until after this copy.
					let bytes = unsafe { report.page.bytes(moved.min(usb_input::MAX_REPORT as u32) as usize) }.to_vec();
					usb_input::feed(&mut report.input, &mut self.pads, &bytes);
					self.post(at);
				}
				Done::Stall(_) => {
					let (address, endpoint, pipe) = (self.reports[at].address, self.reports[at].endpoint, self.reports[at].pipe);
					if let Some(ep0) = self.devices.iter().find(|device| device.address == address).map(|device| device.ep0) {
						let _ = self.control(ep0, RT_ENDPOINT, REQ_CLEAR_FEATURE, FEATURE_ENDPOINT_HALT, endpoint as u16, 0);
					}
					self.hc.reset_toggle(pipe);
					self.post(at);
				}
				Done::Failed => {
					self.reports[at].failures += 1;
					if self.reports[at].failures < REPORT_RETRIES {
						self.post(at);
					} else {
						self.say(b"an input device's reports failed eight times in a row - it is no longer read");
					}
				}
			}
		}
	}

	// THE STORAGE MODULE: the configuration selected, both bulk pipes and the data buffer taken, and the unit started
	// through the shared transport. Everything is given back when the unit does not start.
	fn bind_storage(&mut self, index: usize, value: u8, found: &usb_bot::Interface) -> bool {
		let device = &self.devices[index];
		let (ep0, address, speed, tt) = (device.ep0, device.address, device.speed, device.tt);
		if self.control(ep0, 0x00, REQ_SET_CONFIGURATION, value as u16, 0, 0).is_none() {
			return false;
		}
		let bulk = |ep: usb_bot::Bulk| Endpoint { address, number: ep.address & 0xf, dir_in: ep.address & 0x80 != 0, kind: Kind::Bulk, max_packet: ep.max_packet, speed, interval: 0, tt };
		let pipe_in = self.hc.open(&bulk(found.ep_in));
		let pipe_out = self.hc.open(&bulk(found.ep_out));
		let data = self.hc.dma(PAGE);
		let (Some(pipe_in), Some(pipe_out), Some(data)) = (pipe_in, pipe_out, data) else {
			for pipe in [pipe_in, pipe_out].into_iter().flatten() {
				self.hc.close(pipe);
			}
			if let Some(mut data) = data {
				data.release();
			}
			return false;
		};
		let mut disk = Disk { address, pipe_in, pipe_out, ep_in: found.ep_in.address, ep_out: found.ep_out.address, data };
		let mut unit = usb_bot::Unit::new(found.iface);
		let started = usb_bot::start(&mut Bot { hc: &mut self.hc, ep0, disk: &mut disk, scratch: &self.scratch }, &mut unit);
		if !started {
			self.hc.close(disk.pipe_in);
			self.hc.close(disk.pipe_out);
			disk.data.release();
			self.say(b"a disk did not come ready, or its sector is not 512 bytes - it is left addressed");
			return false;
		}
		self.devices[index].role = Role::Storage;
		self.disk = Some((disk, unit));
		true
	}

	/// SERVE ONE BLOCK REQUEST through the shared transport - or, with no disk bound, answer it with an error status,
	/// which is what an empty publication costs its consumer.
	pub fn serve_block(&mut self, blk_server: u64, req: &[u8; 16], handle: u64) {
		let ep0 = self.disk.as_ref().and_then(|(disk, _)| self.devices.iter().find(|device| device.address == disk.address)).map(|device| device.ep0);
		match (self.disk.as_mut(), ep0) {
			(Some((disk, unit)), Some(ep0)) => usb_bot::serve(&mut Bot { hc: &mut self.hc, ep0, disk, scratch: &self.scratch }, unit, blk_server, req, handle),
			_ => {
				if handle != 0 {
					close(handle);
				}
				usb_bot::reply_block(blk_server, driver_protocol::block::STATUS_ERR, 0);
			}
		}
	}

	// ------------------------------------------------------------------- hubs

	// CONFIGURE A HUB AND ENUMERATE WHAT IS ON ITS PORTS: its configuration selected, its class descriptor read for
	// the port count and the power-on time, its status-change endpoint standing, every port powered, and each that
	// carries a device reset and that device addressed - through `attach`, so a hub found behind it expands too.
	fn expand(&mut self, index: usize, value: u8, config: &[u8]) {
		let device = &self.devices[index];
		let (ep0, address, speed, tt) = (device.ep0, device.address, device.speed, device.tt);
		self.devices[index].role = Role::Hub;
		if self.control(ep0, 0x00, REQ_SET_CONFIGURATION, value as u16, 0, 0).is_none() {
			self.say(b"a hub refused its configuration - its ports are not enumerated");
			return;
		}
		// bNbrPorts rides at offset two and bPwrOn2PwrGood at five, so anything shorter than six bytes is no hub
		// descriptor this core can power a port by.
		let Some(received) = self.control(ep0, RT_CLASS_DEVICE_IN, REQ_GET_DESCRIPTOR, DESC_HUB << 8, 0, 9) else { return };
		if received < 6 {
			return;
		}
		let bytes = self.scratch(received);
		let ports = bytes[2].min(MAX_HUB_PORTS);
		let power_ms = bytes[5] as u64 * 2;
		// THE STATUS-CHANGE ENDPOINT: the hub's one interrupt IN endpoint, a bit per port that changed.
		let change = descriptor::Walk::new(config).find(|record| record.kind == descriptor::DT_ENDPOINT && record.field(2).is_ok_and(|ep| ep & 0x80 != 0) && record.field(3).is_ok_and(|attrs| attrs & 3 == 3));
		let pipe = change.and_then(|record| {
			let (Ok(ep), Ok(mps), Ok(interval)) = (record.field(2), record.field16(4), record.field(6)) else { return None };
			self.hc.open(&Endpoint { address, number: ep & 0xf, dir_in: true, kind: Kind::Interrupt, max_packet: mps, speed, interval, tt })
		});
		let Some(page) = self.hc.dma(PAGE) else {
			if let Some(pipe) = pipe {
				self.hc.close(pipe);
			}
			return;
		};
		for port in 1..=ports {
			let _ = self.control(ep0, RT_CLASS_PORT, REQ_SET_FEATURE, HUB_FEAT_PORT_POWER, port as u16, 0);
		}
		// bPwrOn2PwrGood is in two-millisecond units; a tick past it, at least.
		sleep_until(clock().saturating_add(1 + power_ms / 10));
		self.hubs.push(Hub { address, ports, pipe, page, standing: false });
		let hub_at = self.hubs.len() - 1;
		for port in 1..=ports {
			self.attach_hub_port(address, port);
		}
		self.post_hub(hub_at);
	}

	fn post_hub(&mut self, at: usize) {
		let hub = &mut self.hubs[at];
		if let Some(pipe) = hub.pipe {
			hub.standing = self.hc.submit(pipe, hub.page.phys, (hub.ports as u32 + 8) / 8);
		}
	}

	// A hub port's status word and change word.
	fn hub_port_status(&mut self, ep0: Pipe, port: u8) -> Option<(u16, u16)> {
		if self.control(ep0, RT_CLASS_PORT_IN, REQ_GET_STATUS, 0, port as u16, 4)? < 4 {
			return None;
		}
		let bytes = self.scratch(4);
		Some((u16::from_le_bytes([bytes[0], bytes[1]]), u16::from_le_bytes([bytes[2], bytes[3]])))
	}

	// BRING UP THE DEVICE ON ONE HUB PORT: connected, reset through the hub (waiting on the reset change), the change
	// acknowledged, its speed read off the port status, and the device addressed - reached, below a high-speed hub,
	// through that hub's translator when it is not high speed itself.
	fn attach_hub_port(&mut self, hub: u8, port: u8) {
		let Some(at) = self.devices.iter().position(|device| device.address == hub) else { return };
		let (ep0, root, hub_speed, hub_tt) = (self.devices[at].ep0, self.devices[at].port, self.devices[at].speed, self.devices[at].tt);
		let Some((status, _)) = self.hub_port_status(ep0, port) else { return };
		if status & PORT_STATUS_CONNECTION == 0 {
			return;
		}
		if self.control(ep0, RT_CLASS_PORT, REQ_SET_FEATURE, HUB_FEAT_PORT_RESET, port as u16, 0).is_none() {
			return;
		}
		// THE RESET IS WAITED FOR ON ITS CHANGE BIT, bounded: half a second.
		let deadline = clock().saturating_add(50);
		let status = loop {
			sleep_until(clock().saturating_add(1));
			match self.hub_port_status(ep0, port) {
				Some((status, change)) if change & PORT_CHANGE_RESET != 0 => break Some(status),
				Some(_) if clock() < deadline => continue,
				_ => break None,
			}
		};
		let _ = self.control(ep0, RT_CLASS_PORT, REQ_CLEAR_FEATURE, HUB_FEAT_C_RESET, port as u16, 0);
		let _ = self.control(ep0, RT_CLASS_PORT, REQ_CLEAR_FEATURE, HUB_FEAT_C_CONNECTION, port as u16, 0);
		let Some(status) = status else { return };
		if status & PORT_STATUS_ENABLE == 0 {
			return;
		}
		let speed = if status & PORT_STATUS_LOW_SPEED != 0 {
			Speed::Low
		} else if status & PORT_STATUS_HIGH_SPEED != 0 {
			Speed::High
		} else {
			Speed::Full
		};
		// THE RESET RECOVERY: ten milliseconds before the device need answer, a tick.
		sleep_until(clock().saturating_add(1));
		let tt = match hub_tt {
			Some(tt) => Some(tt),
			None if hub_speed == Speed::High && speed != Speed::High => Some(Tt { hub, port }),
			None => None,
		};
		let _ = self.attach(root, Some((hub, port)), tt, speed);
	}

	/// EVERY HUB WHOSE STATUS-CHANGE TRANSFER ENDED: each port whose bit is set is read and its changes acknowledged,
	/// a connection change takes what was on the port away and brings up what is on it now, and the transfer is
	/// posted again.
	pub fn reap_hubs(&mut self) {
		for at in 0..self.hubs.len() {
			let Some(hub) = self.hubs.get(at) else { break };
			let (Some(pipe), true) = (hub.pipe, hub.standing) else { continue };
			let Some(done) = self.hc.reap(pipe) else { continue };
			self.hubs[at].standing = false;
			let (address, ports) = (self.hubs[at].address, self.hubs[at].ports);
			if let Done::Ok(moved) = done {
				// SAFETY: the transfer into the page ended and the next is posted after this copy.
				let bitmap = unsafe { self.hubs[at].page.bytes(moved as usize) }.to_vec();
				for port in 1..=ports {
					if bitmap.get(port as usize / 8).is_some_and(|byte| byte & (1 << (port % 8)) != 0) {
						self.hub_port_changed(address, port);
					}
				}
			}
			// The hub may have gone with a port of its own parent; post again only if it is still here.
			if let Some(again) = self.hubs.iter().position(|hub| hub.address == address) {
				self.post_hub(again);
			}
		}
	}

	fn hub_port_changed(&mut self, hub: u8, port: u8) {
		let Some(ep0) = self.devices.iter().find(|device| device.address == hub).map(|device| device.ep0) else { return };
		let Some((status, change)) = self.hub_port_status(ep0, port) else { return };
		for (bit, feature) in [(1 << 1, HUB_FEAT_C_ENABLE), (1 << 2, HUB_FEAT_C_SUSPEND), (1 << 3, HUB_FEAT_C_OVER_CURRENT), (PORT_CHANGE_RESET, HUB_FEAT_C_RESET)] {
			if change & bit != 0 {
				let _ = self.control(ep0, RT_CLASS_PORT, REQ_CLEAR_FEATURE, feature, port as u16, 0);
			}
		}
		if change & PORT_CHANGE_CONNECTION == 0 {
			return;
		}
		let _ = self.control(ep0, RT_CLASS_PORT, REQ_CLEAR_FEATURE, HUB_FEAT_C_CONNECTION, port as u16, 0);
		self.detach_where(|device| device.parent == Some((hub, port)));
		if status & PORT_STATUS_CONNECTION != 0 {
			self.attach_hub_port(hub, port);
		}
	}

	// ------------------------------------------------------------------- departure

	/// EVERY ROOT PORT THAT CHANGED: what was on it taken away, and what is on it now brought up.
	pub fn reconcile(&mut self) {
		for port in 1..=self.hc.ports() {
			let state = self.hc.port(port);
			if !state.changed {
				continue;
			}
			self.hc.acknowledge_port(port);
			self.detach_where(|device| device.port == port);
			if state.connected {
				self.attach_root(port);
			}
		}
	}

	// TAKE AWAY EVERY DEVICE `which` NAMES and everything behind it: a hub's devices first, then each device's
	// reports - its gamepads departing before anything is released - its disk and its hub state, its pipes and pages,
	// its class budget and its address.
	fn detach_where(&mut self, which: impl Fn(&Device) -> bool) {
		let mut gone: Vec<u8> = self.devices.iter().filter(|device| which(device)).map(|device| device.address).collect();
		// BEHIND A HUB THAT LEFT, everything on its ports - found by walking parents until nothing new joins.
		loop {
			let more: Vec<u8> = self.devices.iter().filter(|device| device.parent.is_some_and(|(hub, _)| gone.contains(&hub)) && !gone.contains(&device.address)).map(|device| device.address).collect();
			if more.is_empty() {
				break;
			}
			gone.extend(more);
		}
		for address in gone {
			self.detach(address);
		}
	}

	fn detach(&mut self, address: u8) {
		let Some(at) = self.devices.iter().position(|device| device.address == address) else { return };
		let reports: Vec<usize> = (0..self.reports.len()).filter(|&at| self.reports[at].address == address).collect();
		if !reports.is_empty() {
			usb_input::depart(&mut self.pads, reports.iter().map(|&at| &self.reports[at].input));
			self.budget.release(ClassKind::Hid);
		}
		for at in reports.into_iter().rev() {
			let mut report = self.reports.remove(at);
			self.hc.close(report.pipe);
			report.page.release();
		}
		if self.disk.as_ref().is_some_and(|(disk, _)| disk.address == address)
			&& let Some((mut disk, _)) = self.disk.take()
		{
			self.hc.close(disk.pipe_in);
			self.hc.close(disk.pipe_out);
			disk.data.release();
			self.budget.release(ClassKind::Storage);
			self.say(b"the disk left - its block requests are answered with an error until one arrives");
		}
		if let Some(hub) = self.hubs.iter().position(|hub| hub.address == address) {
			let mut hub = self.hubs.remove(hub);
			if let Some(pipe) = hub.pipe {
				self.hc.close(pipe);
			}
			hub.page.release();
		}
		let device = self.devices.remove(at);
		self.hc.close(device.ep0);
		self.used[address as usize] = false;
	}
}

// THE SHARED TRANSPORT'S CONTROLLER SIDE: the controller, endpoint zero for the device-side recovery and the class
// reset, the disk's pipes and buffer, and the scratch page whose second half the wrappers ride in.
struct Bot<'a, C: Controller> {
	hc: &'a mut C,
	ep0: Pipe,
	disk: &'a mut Disk,
	scratch: &'a Dma,
}

impl<C: Controller> Bot<'_, C> {
	fn control(&mut self, request_type: u8, request: u8, value: u16, index: u16) {
		let setup: [u8; 8] = [request_type, request, value as u8, (value >> 8) as u8, index as u8, (index >> 8) as u8, 0, 0];
		let _ = self.hc.control(self.ep0, &setup, None);
	}
}

impl<C: Controller> usb_bot::Transport for Bot<'_, C> {
	fn bulk(&mut self, dir_in: bool, at: At, len: u32) -> Moved {
		let phys = match at {
			At::Wrapper(offset) => self.scratch.phys + WRAPPER_AT + offset,
			At::Data => self.disk.data.phys,
		};
		let pipe = if dir_in { self.disk.pipe_in } else { self.disk.pipe_out };
		match self.hc.bulk(pipe, phys, len) {
			Done::Ok(moved) => Moved::Done(moved),
			Done::Stall(moved) => Moved::Stall(moved),
			Done::Failed => Moved::Failed,
		}
	}

	// The device's halt cleared - which resets its data toggle - and the pipe's toggle with it.
	fn recover(&mut self, dir_in: bool) {
		let (pipe, endpoint) = if dir_in { (self.disk.pipe_in, self.disk.ep_in) } else { (self.disk.pipe_out, self.disk.ep_out) };
		self.control(RT_ENDPOINT, REQ_CLEAR_FEATURE, FEATURE_ENDPOINT_HALT, endpoint as u16);
		self.hc.reset_toggle(pipe);
	}

	fn class_reset(&mut self, iface: u16) {
		self.control(usb_bot::RT_CLASS_INTERFACE, usb_bot::BOT_REQ_RESET, 0, iface);
	}

	fn wrapper(&mut self) -> &mut [u8] {
		// SAFETY: the scratch page's second half, which only the transport uses and only between its transfers.
		unsafe { core::slice::from_raw_parts_mut((self.scratch.virt + WRAPPER_AT as u64) as *mut u8, 64) }
	}

	// THE DATA BUFFER GROWS TO THE REQUEST and is bounded by what the admission reserved: it never shrinks, inside the
	// controller's Domain, for the life of the binding.
	fn data(&mut self, bytes: u64) -> Option<&mut [u8]> {
		if bytes > self.disk.data.bytes as u64 {
			if !Budget::buffer_within(ClassKind::Storage, bytes) {
				return None;
			}
			let grown = self.hc.dma(bytes.div_ceil(PAGE as u64) as u32 * PAGE)?;
			self.disk.data.release();
			self.disk.data = grown;
		}
		// SAFETY: the data buffer, `bytes` long or more, which no transfer is writing between the transport's calls.
		Some(unsafe { self.disk.data.bytes_mut(self.disk.data.bytes as usize) })
	}

	fn max_transfer(&self) -> u32 {
		self.hc.max_transfer()
	}
}

fn hex4(value: u16) -> [u8; 4] {
	let digits = b"0123456789abcdef";
	[digits[(value >> 12) as usize & 0xf], digits[(value >> 8) as usize & 0xf], digits[(value >> 4) as usize & 0xf], digits[value as usize & 0xf]]
}

// ----------------------------------------------------------------------- the driver around a host

// THE TOKENS: the bus inventory's, its position in the offer list - and, above it, the rest: a fresh token for every
// publication made.
const BUS_TOKEN: u16 = 0;
const FIRST_TOKEN: u16 = 16;

// A PUBLICATION THAT FOLLOWS WHAT THE BUS HOLDS: a disk's block wire, the pointer, the touch surface, the gamepads.
//
// xHCI publishes the pointer and the gamepads whether or not anything behind them is bound, and that is right for one
// controller. THESE COME SEVERAL TO A MACHINE - an EHCI and its three companions are four - and InputService attaches
// four pointer providers, four gamepad providers and one touch surface at a time, so a controller with nothing behind a
// publication would hold a place a controller with a device needs. The USB volume's instance takes the first block
// provider whose binding also publishes a bus, and an empty one would hold that place too. So each is offered when its
// kind of device binds, under a fresh token, and withdrawn when the last of them leaves.
struct Publication {
	kind: u16,
	token: Option<u16>,
}

impl Publication {
	// Offer or withdraw, so the publication says what the bus holds now.
	fn follow(&mut self, bound: bool, next: &mut u16, bootstrap: u64, bind: &common::Bind, serving: &mut common::Serving, who: &[u8]) {
		match (bound, self.token) {
			(true, None) => {
				let token = *next;
				let Some((near, far)) = channel() else { return };
				if !serving.publish(token, near) {
					close(near);
					close(far);
					print(who);
					print(b": no room for another publication - a device stays unpublished\n");
					return;
				}
				if !common::offer(bootstrap, bind, self.kind, token, far) {
					serving.retire(token);
					return;
				}
				*next = next.saturating_add(1);
				self.token = Some(token);
			}
			(false, Some(token)) => {
				serving.retire(token);
				common::withdraw(bootstrap, bind, token);
				self.token = None;
			}
			_ => {}
		}
	}

	// The connection a sink sends through: the publication's first, or none.
	fn first(&self, serving: &common::Serving) -> u64 {
		self.token.map_or(0, |token| serving.first_for(token))
	}
}

// THE FOUR FOLLOWING PUBLICATIONS, and the token the next one takes.
struct Publications {
	disk: Publication,
	pointer: Publication,
	touch: Publication,
	gamepad: Publication,
	next: u16,
}

impl Publications {
	fn new() -> Publications {
		let publication = |kind| Publication { kind, token: None };
		Publications { disk: publication(driver_protocol::provider::BLOCK), pointer: publication(driver_protocol::provider::POINTER), touch: publication(driver_protocol::provider::TOUCH), gamepad: publication(driver_protocol::provider::GAMEPAD), next: FIRST_TOKEN }
	}

	// Every publication brought to what the bus holds, and the sinks pointed at what is published.
	fn follow<C: Controller>(&mut self, host: &mut Host<C>, bootstrap: u64, bind: &common::Bind, serving: &mut common::Serving) {
		let who = host.hc.who();
		let Publications { disk, pointer, touch, gamepad, next } = self;
		disk.follow(host.disk.is_some(), next, bootstrap, bind, serving, who);
		pointer.follow(host.carries(|input| input.layout.has_pointer()), next, bootstrap, bind, serving, who);
		touch.follow(host.carries(|input| input.layout.has_digitizer()), next, bootstrap, bind, serving, who);
		gamepad.follow(host.carries(Input::has_gamepad), next, bootstrap, bind, serving, who);
		self.sinks(host, serving);
	}

	fn sinks<C: Controller>(&self, host: &mut Host<C>, serving: &common::Serving) {
		usb_input::PTR_SINK.store(self.pointer.first(serving), Ordering::Relaxed);
		usb_input::TOUCH_SINK.store(self.touch.first(serving), Ordering::Relaxed);
		host.pads.follow(self.gamepad.first(serving));
	}
}

// The driver's live device inventory, served over the generated `usb` contract - the one `lsusb` reads from xHCI.
struct Inventory<'a> {
	devices: &'a [Device],
}

impl usb::Service for Inventory<'_> {
	fn list(&mut self) -> Result<Vec<UsbEntry>, UsbError> {
		Ok(self.devices.iter().map(|device| UsbEntry { port: device.port, speed: String::from(device.speed.name()), vendor: device.vendor as u32, product: device.product as u32, class: device.class as u32, r#type: String::from(device.role.name()) }).collect())
	}
}

// THE SLEEP: the controller halted - no schedule runs, nothing masters the bus - and run again after a sleep that kept
// its power. A sleep that lost it lost every device's address and configuration with it, which only a bind sets up,
// so that resume answers it did not come back and the binding starts again.
struct Sleep<'a, C: Controller> {
	hc: &'a mut C,
	serving: &'a mut common::Serving,
}

impl<C: Controller> common::SleepStep for Sleep<'_, C> {
	fn suspend(&mut self, _request: &driver_protocol::SuspendRequest) -> driver_protocol::Suspended {
		if !self.hc.halt() {
			return driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Refused(driver_protocol::DriverFailureCode::DeviceNotResponding), awake_by_ms: 0 };
		}
		driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Done, awake_by_ms: 0 }
	}

	fn resume(&mut self, lost_power: bool) -> bool {
		!lost_power && self.hc.run()
	}

	fn serving(&mut self) -> Option<&mut common::Serving> {
		Some(self.serving)
	}
}

/// THE WHOLE DRIVER AROUND A HOST, from its bus enumerated to its stop: the online line, the bus inventory's
/// publication, and the ones that follow what the bus holds - a disk's block wire, the pointer, the touch surface, the
/// gamepads - and the loop that serves them: the controller's interrupt reaped into reports and hub changes, root ports
/// reconciled, block requests carried, the inventory answered, the sleep taken.
pub fn run<C: Controller>(bootstrap: u64, bind: &common::Bind, device: u64, irq: u64, mut host: Host<C>) -> ! {
	let who = host.hc.who();
	let (bus_server, bus_client) = channel().unwrap_or_else(|| exit());
	host.scan();
	let mut report: common::Bounded<256> = common::Bounded::new();
	report.push(who);
	report.push(b": online (");
	report.push(&common::hex2(bind.info.bus));
	report.push(b":");
	report.push(&common::hex2(bind.info.dev));
	report.push(b".");
	report.push(&[b'0' + (bind.info.func % 10)]);
	report.push(b", ");
	report.decimal(host.devices.len() as u64);
	report.push(b" device(s))");
	for (role, marker) in [
		(Role::Keyboard, b" (keyboard)".as_slice()),
		(Role::Pointer, b" (pointer)"),
		(Role::Storage, b" (storage)"),
		(Role::Touch, b" (touch)"),
		(Role::Gamepad, b" (gamepad)"),
	] {
		if host.any(role) {
			report.push(marker);
		}
	}
	common::online_named(bootstrap, bind, report.as_bytes(), &[(driver_protocol::provider::USB_BUS, bus_client, &[][..])]);
	let mut serving = common::Serving::from_offers(&[(BUS_TOKEN, bus_server)]);
	let mut publications = Publications::new();
	publications.follow(&mut host, bootstrap, bind, &mut serving);
	common::takes_sleep();
	// THE LOOP'S TIMER, on a housekeeping wait: a tick away while a gamepad frame is owed, and - for a controller that
	// raises nothing when a root port changes - at the next look at its ports.
	let retry: u64 = match timer_create() {
		timer if timer > 0 => timer as u64,
		_ => exit(),
	};
	let mut next_poll: u64 = host.hc.port_poll().map_or(u64::MAX, |every| clock().saturating_add(every));
	let mut req = [0u8; 16];
	loop {
		host.reap_reports();
		host.reap_hubs();
		publications.follow(&mut host, bootstrap, bind, &mut serving);
		host.pads.flush();
		let owed = host.pads.owes();
		timer_set(retry, core::cmp::min(if owed { clock().saturating_add(1) } else { u64::MAX }, next_poll));
		let waiting = [irq, retry];
		let ready = match common::wait_providers_or_sleep(bootstrap, bind, &mut serving, &waiting, owed || next_poll != u64::MAX) {
			Some(Some(ready)) => Some(ready),
			Some(None) => {
				if common::take_sleep_step(bootstrap, bind, &mut Sleep { hc: &mut host.hc, serving: &mut serving }) {
					continue;
				}
				None
			}
			None => None,
		};
		let Some(ready) = ready else {
			common::finish_stop(bootstrap, bind, device, host.hc.halt());
			exit();
		};
		publications.sinks(&mut host, &serving);
		// THE INTERRUPT IS ACKNOWLEDGED AFTER THE CONTROLLER'S STATUS IS CLEARED, which is what a level line needs: the
		// line stays masked until then, and an acknowledgement before the clear would be one more firing for nothing.
		if let common::ProviderReady::Device(0) = ready {
			host.hc.service();
			interrupt_ack(irq);
		}
		let mut look = host.hc.take_port_change();
		if clock() >= next_poll {
			next_poll = host.hc.port_poll().map_or(u64::MAX, |every| clock().saturating_add(every));
			look = true;
		}
		if look {
			host.reconcile();
			publications.follow(&mut host, bootstrap, bind, &mut serving);
		}
		let common::ProviderReady::Consumer(at) = ready else { continue };
		let server = serving.at(at);
		let token = serving.token_at(at);
		let closed = if publications.disk.token == Some(token) {
			match try_recv(server, &mut req) {
				Polled::Message { len, handle } if len >= 16 => {
					host.serve_block(server, &req, handle);
					false
				}
				Polled::Message { handle, .. } => {
					if handle != 0 {
						close(handle);
					}
					usb_bot::reply_block(server, driver_protocol::block::STATUS_INVALID, 0);
					false
				}
				Polled::Empty => false,
				Polled::Closed => true,
			}
		} else if token == BUS_TOKEN {
			let mut query = [0u8; 64];
			match try_recv_caps(server, &mut query) {
				PolledCaps::Message { len, handles } => {
					let mut api = Inventory { devices: &host.devices };
					let mut reply = [0u8; 4096];
					let mut reply_handles = proto::codec::Handles::new();
					let mut handles = handles;
					if let Some(n) = usb::dispatch(&mut api, &query[..len], &mut handles, &mut reply, &mut reply_handles)
						&& !send_caps_blocking(server, &reply[..n], reply_handles.as_slice())
					{
						for &leftover in reply_handles.as_slice() {
							close(leftover);
						}
					}
					for &unclaimed in handles.as_slice() {
						close(unclaimed);
					}
					false
				}
				PolledCaps::Empty => false,
				PolledCaps::Closed => true,
			}
		} else {
			// The pointer, touch and gamepad consumers receive and send nothing - nor does one still connected to a disk
			// that left: drain what they send, with its capabilities, and return the allowance when the peer closes.
			match try_recv_caps(server, &mut req) {
				PolledCaps::Message { handles, .. } => {
					for &handle in handles.as_slice() {
						close(handle);
					}
					false
				}
				PolledCaps::Empty => false,
				PolledCaps::Closed => true,
			}
		};
		if closed {
			let token = serving.close_at(at);
			publications.sinks(&mut host, &serving);
			common::disconnected(bootstrap, bind, token);
		}
	}
}

#[cfg(test)]
mod tests;
