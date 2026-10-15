// Exact production code is inserted below. Existing HID/audio consumers are real.
// Memory, time and rt output are modeled; the service-drain cases additionally
// model an already-selected request, absent devices and class-triggered refill.
#![allow(dead_code, unused_imports, unused_variables, unused_mut)]
extern crate alloc;
extern crate self as driver_protocol;
extern crate self as drivers;

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use keys::Mods;
use std::sync::Mutex;
use usb_class::ClassKind;

/* EXACT_MODULES */

static NOW: AtomicU64 = AtomicU64::new(100);
static KEY_SINK: AtomicU64 = AtomicU64::new(0);
static TRUSTED_KEY_SINK: AtomicU64 = AtomicU64::new(0);
static PTR_SINK: AtomicU64 = AtomicU64::new(77);
static TOUCH_SINK: AtomicU64 = AtomicU64::new(0);
static SENT: Mutex<Vec<(u64, Vec<u8>)>> = Mutex::new(Vec::new());
static PRINTED: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());
static CLOSED: Mutex<Vec<u64>> = Mutex::new(Vec::new());

fn clock() -> u64 {
	NOW.load(Ordering::SeqCst)
}
fn print(bytes: &[u8]) {
	PRINTED.lock().unwrap().push(bytes.to_vec());
}
fn close(handle: u64) {
	CLOSED.lock().unwrap().push(handle);
}
fn try_send(sink: u64, bytes: &[u8], handle: u64) -> bool {
	assert_eq!(handle, 0, "proof output cannot transfer capabilities");
	SENT.lock().unwrap().push((sink, bytes.to_vec()));
	true
}
fn send_blocking(sink: u64, bytes: &[u8], handle: u64) -> bool {
	try_send(sink, bytes, handle)
}
// A single service thread owns these modeled IPC heads. The production selector is
// outside this proof: it supplies the already-selected consumer to the extracted block.
static PCM_HEAD: Mutex<Option<(u64, Vec<u8>, u64)>> = Mutex::new(None);
static PCM_CLOSED: AtomicU64 = AtomicU64::new(0);
enum Polled {
	Message { len: usize, handle: u64 },
	Empty,
	Closed,
}
fn channel_peek(channel: u64) -> i64 {
	match PCM_HEAD.lock().unwrap().as_ref() {
		Some((owner, bytes, _)) if *owner == channel => bytes.len() as i64,
		_ if PCM_CLOSED.load(Ordering::SeqCst) == channel => -2,
		_ => -1,
	}
}
fn try_recv(channel: u64, output: &mut [u8]) -> Polled {
	let mut head = PCM_HEAD.lock().unwrap();
	if let Some((owner, bytes, _)) = head.as_ref() {
		assert_eq!(*owner, channel);
		assert!(bytes.len() <= output.len());
		let (_, bytes, handle) = head.take().unwrap();
		output[..bytes.len()].copy_from_slice(&bytes);
		Polled::Message { len: bytes.len(), handle }
	} else if PCM_CLOSED.load(Ordering::SeqCst) == channel {
		Polled::Closed
	} else {
		Polled::Empty
	}
}
mod common {
	pub enum ProviderReady {
		Consumer(usize),
		Device(usize),
	}
	pub struct Serving(pub Vec<(u32, u64)>);
	impl Serving {
		pub fn token_at(&self, at: usize) -> u32 {
			self.0[at].0
		}
		pub fn at(&self, at: usize) -> u64 {
			self.0[at].1
		}
		pub fn first_for(&self, token: u32) -> u64 {
			self.0.iter().find(|row| row.0 == token).map_or(0, |row| row.1)
		}
	}
}
enum SendOutcome {
	Delivered,
}
fn try_send_outcome(sink: u64, bytes: &[u8], handle: u64) -> SendOutcome {
	assert!(try_send(sink, bytes, handle));
	SendOutcome::Delivered
}
fn yield_now() {
	panic!("fixture omitted a required controller event");
}

// This proof sends pointer reports through the actual HID parser and consumer.
// Cooked keyboard delivery is outside its scope and must fail loudly if reached.
mod keys {
	#[derive(Default)]
	pub struct Mods;
	pub fn usage_keycode(_: u32) -> u16 {
		panic!("unexpected cooked keyboard boundary");
	}
	pub fn system_key_frame(_: u16, _: bool) -> Option<[u8; 5]> {
		panic!("unexpected system key boundary");
	}
	pub fn feed_key(_: u16, _: u32, _: &mut Mods) {
		panic!("unexpected cooked key output");
	}
}

// A field-compatible controller fixture, including empty candidate-only storage
// in the original build. Original production methods never write those fields.
// Every event/ring/dispatch method operating on this storage is extracted below.
struct Xhci {
	op: u64,
	ir0: u64,
	db: u64,
	cmd: Ring,
	evt_virt: u64,
	evt_phys: u64,
	evt_index: u64,
	evt_cycle: u32,
	dcbaa_virt: u64,
	ports_changed: port::PortSignal,
	budget: usb_class::Budget,
	net_rx: Option<(u32, u32)>,
	net_pending: Option<(u32, u32)>,
	net_notify_rx: Option<(u32, u32)>,
	net_notify_pending: Option<(u32, u32)>,
	hid_rx: Vec<(u32, u32)>,
	hid_pending: Vec<(u32, u32)>,
	serial_rx: [Option<(u32, u32)>; usb_serial::MAX_PORTS],
	serial_pending: [Option<(u32, u32)>; usb_serial::MAX_PORTS],
	class_rx: Vec<(u32, u32)>,
	class_pending: Vec<(u64, u32, u32)>,
	audio_rx: Vec<(u32, u32)>,
	audio_pending: Vec<(u64, u32, u32)>,
}

/* EXACT_PRODUCTION */

#[repr(align(4096))]
struct Memory([u8; 4096]);
struct Arena(Vec<Box<Memory>>);
impl Arena {
	fn page(&mut self) -> u64 {
		let mut memory = Box::new(Memory([0; 4096]));
		let address = memory.0.as_mut_ptr() as u64;
		self.0.push(memory);
		address
	}
	fn ring(&mut self) -> Ring {
		let address = self.page();
		unsafe {
			(address as *mut u64).add(2 * (RING_TRBS as usize - 1)).write(address);
			w32(address + (RING_TRBS - 1) * 16 + 12, TRB_LINK << 10 | TRB_TOGGLE_CYCLE);
		}
		Ring { virt: address, phys: address, index: 0, cycle: 1, handle: 0 }
	}
}
struct Fixture {
	arena: Arena,
	hc: Xhci,
}
impl Fixture {
	fn new() -> Self {
		NOW.store(100, Ordering::SeqCst);
		*PCM_HEAD.lock().unwrap() = None;
		PCM_CLOSED.store(0, Ordering::SeqCst);
		SENT.lock().unwrap().clear();
		PRINTED.lock().unwrap().clear();
		CLOSED.lock().unwrap().clear();
		let mut arena = Arena(Vec::new());
		let evt = arena.page();
		let hc = Xhci { op: arena.page(), ir0: arena.page(), db: arena.page(), cmd: arena.ring(), evt_virt: evt, evt_phys: evt, evt_index: 0, evt_cycle: 1, dcbaa_virt: arena.page(), ports_changed: port::PortSignal::new(), budget: usb_class::Budget::new(), net_rx: None, net_pending: None, net_notify_rx: None, net_notify_pending: None, hid_rx: Vec::new(), hid_pending: Vec::new(), serial_rx: [None; usb_serial::MAX_PORTS], serial_pending: [None; usb_serial::MAX_PORTS], class_rx: Vec::new(), class_pending: Vec::new(), audio_rx: Vec::new(), audio_pending: Vec::new() };
		Self { arena, hc }
	}
	fn dev(&mut self, slot: u32, port: u32) -> UsbDevice {
		UsbDevice { slot, port, route: 0, speed: 1, ep0: self.arena.ring(), in_virt: 0, in_phys: 0, data_virt: 0, data_phys: 0, ctx_handle: 0, in_handle: 0, data_handle: 0, vendor: 0, product: 0, class: 0, remote_wakeup: false }
	}
	fn net(&mut self, slot: u32, port: u32) -> (UsbDevice, Net) {
		let dev = self.dev(slot, port);
		let page = self.arena.page();
		unsafe {
			core::ptr::copy_nonoverlapping([0xa1, 0, 1, 0, 0, 0, 0, 0].as_ptr(), page as *mut u8, 8);
		}
		let net = Net { dci_in: 5, dci_out: 4, ring_in: self.arena.ring(), ring_out: self.arena.ring(), note: Some(Notify { dci: 3, ring: self.arena.ring(), handle: 0, virt: page, phys: page, posted: false, up: None, every: 10, due: 0 }), rx_virt: 0, rx_phys: 0, rx_handle: 0, tx_virt: 0, tx_phys: 0, tx_handle: 0, posted: false, ncm: None, mac: [0; 6], mtu: 1500 };
		register_network(&mut self.hc, &dev, &net);
		(dev, net)
	}
	fn hid(&mut self, dci: u32) -> Hid {
		let page = self.arena.page();
		let descriptor = [
			0x05,
			0x01,
			0x09,
			0x02,
			0xa1,
			0x01,
			0x09,
			0x01,
			0xa1,
			0x00,
			0x05,
			0x09,
			0x19,
			0x01,
			0x29,
			0x03,
			0x15,
			0x00,
			0x25,
			0x01,
			0x95,
			0x03,
			0x75,
			0x01,
			0x81,
			0x02,
			0x95,
			0x01,
			0x75,
			0x05,
			0x81,
			0x03,
			0x05,
			0x01,
			0x09,
			0x30,
			0x09,
			0x31,
			0x15,
			0x81,
			0x25,
			0x7f,
			0x75,
			0x08,
			0x95,
			0x02,
			0x81,
			0x06,
			0xc0,
			0xc0,
		];
		unsafe {
			core::ptr::copy_nonoverlapping([1, 4, 5].as_ptr(), page as *mut u8, 3);
		}
		let layout = hid::parse(&descriptor);
		assert_eq!(layout.report_bytes(), 3);
		Hid { iface: 0, dci, ring: self.arena.ring(), page: Page { handle: page, virt: page, phys: page }, layout, posted: false, prevs: Vec::new(), mods: Mods::default(), x: 0, y: 0, buttons: 0, pads: Vec::new() }
	}
	fn hids(&mut self, slot: u32, port: u32) -> Hids {
		let dev = self.dev(slot, port);
		let report = self.hid(7);
		let mut hids = Hids::new();
		hids.entries.push((dev, vec![report]));
		post_reports(&mut self.hc, &mut hids);
		hids
	}
	fn events(&mut self, values: &[(u64, u32, u32)]) {
		unsafe {
			core::ptr::write_bytes(self.hc.evt_virt as *mut u8, 0, 4096);
		}
		self.hc.evt_index = 0;
		self.hc.evt_cycle = 1;
		for (index, &(pointer, status, control)) in values.iter().enumerate() {
			self.event_at(index, pointer, status, control | TRB_CYCLE);
		}
	}
	fn event_at(&mut self, index: usize, pointer: u64, status: u32, control: u32) {
		let at = self.hc.evt_virt + index as u64 * 16;
		unsafe {
			(at as *mut u64).write(pointer);
			w32(at + 8, status);
			w32(at + 12, control);
		}
	}
}
fn transfer(slot: u32, dci: u32) -> u32 {
	slot << 24 | dci << 16 | TRB_EV_TRANSFER << 10
}
fn completed(slot: u32) -> (u64, u32, u32) {
	(0x1230, CC_SUCCESS << 24, slot << 24 | TRB_EV_CMD_COMPLETE << 10)
}
fn read32(address: u64) -> u32 {
	unsafe { (address as *const u32).read_volatile() }
}
fn read64(address: u64) -> u64 {
	unsafe { (address as *const u64).read_volatile() }
}
fn assert_ring_consumed(f: &Fixture, count: u64) {
	assert_eq!(f.hc.evt_index, count);
	assert_eq!(read64(f.hc.ir0 + IR_ERDP), f.hc.evt_phys + count * 16 | ERDP_EHB);
}
fn assert_pointer_once(hids: &Hids, message: &str) {
	assert_eq!(SENT.lock().unwrap().as_slice(), &[(77, vec![4, 0, 5, 0, 1, 0])], "{message}");
	let h = &hids.entries[0].1[0];
	assert_eq!((h.x, h.y, h.buttons, h.ring.index), (4, 5, 1, 2));
	assert_eq!(h.prevs[0].1[..3], [1, 4, 5]);
}

fn command_cdc() {
	let mut f = Fixture::new();
	let mut network = Some(f.net(2, 2));
	let (dev, net) = network.as_mut().unwrap();
	post_notification(&f.hc, dev, net);
	let control = transfer(2, 3) | TRB_CYCLE;
	let status = CC_SHORT_PACKET << 24 | (NOTIFY_WINDOW as u32 - 8);
	f.events(&[(0xabc0, status, control), completed(9), completed(10)]);
	unsafe {
		w32(f.hc.db, 0xdead);
	}
	let param = 0x1234_5678_90ab_cde0;
	assert_eq!(command_and_wait(&mut f.hc, param, 0x2233, TRB_DISABLE_SLOT << 10 | 9 << 24), Some(9), "CDC completion must not fail the command");
	assert_eq!(read64(f.hc.cmd.virt), param);
	assert_eq!(read32(f.hc.cmd.virt + 8), 0x2233);
	assert_eq!(read32(f.hc.cmd.virt + 12), TRB_DISABLE_SLOT << 10 | 9 << 24 | TRB_CYCLE);
	assert_eq!(read32(f.hc.db), 0);
	assert_ring_consumed(&f, 2);
	assert_eq!(f.hc.net_notify_pending, Some((status, control)), "CDC completion payload must be retained");
	#[cfg(candidate)]
	{
		assert!(pre_sleep(&mut f.hc, &mut Hids::new(), &mut network));
		let (dev, net) = network.as_mut().unwrap();
		let note = net.note.as_ref().unwrap();
		assert_eq!((note.posted, note.due, note.up), (false, 110, Some(true)), "CDC consumer must clear posted and set its original interval");
		assert_eq!(notification_due(net), Some(110));
		assert_eq!(PRINTED.lock().unwrap().as_slice(), &[b"driver.xhci: the CDC adapter reports its link is up\n".to_vec()]);
		post_notification(&f.hc, dev, net);
		assert_eq!(net.note.as_ref().unwrap().ring.index, 1, "CDC notification must not be reposted before due");
		NOW.store(110, Ordering::SeqCst);
		post_notification(&f.hc, dev, net);
		assert!(net.note.as_ref().unwrap().posted);
		assert_eq!(net.note.as_ref().unwrap().ring.index, 2, "CDC notification must rearm at its original due time");
		assert_eq!(read32(f.hc.db + 2 * 4), 3);
		assert!(handle_net_notification(&f.hc, dev, net, status, control));
		assert_eq!(net.note.as_ref().unwrap().due, 120);
		assert_eq!(PRINTED.lock().unwrap().len(), 1, "a repeated link notification is not a new transition");
		assert_eq!(command_and_wait(&mut f.hc, 0x5670, 0, TRB_DISABLE_SLOT << 10 | 10 << 24), Some(10), "a later command must not receive the earlier command's stale completion");
		assert_ring_consumed(&f, 3);
		assert_eq!(f.hc.cmd.index, 2);
		assert_eq!(read64(f.hc.cmd.virt + 16), 0x5670);
	}
}

fn command_hid() {
	let mut f = Fixture::new();
	let mut hids = f.hids(3, 3);
	let control = transfer(3, 7) | TRB_CYCLE;
	f.events(&[(0xabc0, CC_SUCCESS << 24, control), completed(9)]);
	assert_eq!(command_and_wait(&mut f.hc, 0x1000, 0, TRB_DISABLE_SLOT << 10 | 9 << 24), Some(9), "HID completion must not fail the command");
	assert_ring_consumed(&f, 2);
	assert_eq!(f.hc.hid_pending, vec![(CC_SUCCESS << 24, control)], "HID completion payload must be retained");
	assert!(SENT.lock().unwrap().is_empty(), "HID wait without owners must defer the real report");
	#[cfg(candidate)]
	{
		let sleep = pre_sleep(&mut f.hc, &mut hids, &mut None);
		assert_pointer_once(&hids, "HID report must be consumed before sleep");
		assert!(sleep && f.hc.hid_pending.is_empty());
		assert_eq!(read32(f.hc.db + 3 * 4), 7);
		assert_eq!(read64(hids.entries[0].1[0].ring.virt + 16), hids.entries[0].1[0].page.phys);
	}
}

#[cfg(candidate)]
fn waiters() {
	for empty in [false, true] {
		for command_wait in [false, true] {
			let mut f = Fixture::new();
			let mut hids = f.hids(3, 3);
			let report = (0xabc0, CC_SUCCESS << 24, transfer(3, 7));
			let wanted = if command_wait { completed(9) } else { (0xdef0, CC_SHORT_PACKET << 24 | 3, transfer(9, 1)) };
			f.events(&[report, wanted]);
			let mut none = Hids::new();
			let active = if empty { &mut none } else { &mut hids };
			if command_wait {
				assert_eq!(wait_command(&mut f.hc, active), Some(CC_SUCCESS));
			} else {
				assert_eq!(wait_transfer_at(&mut f.hc, active, 9, 1), Some((0xdef0, CC_SHORT_PACKET, 3)));
			}
			if empty {
				assert!(SENT.lock().unwrap().is_empty());
				assert_eq!(f.hc.hid_pending.len(), 1, "empty Hids must preserve another live owner's report");
				assert!(pre_sleep(&mut f.hc, &mut hids, &mut None));
			} else {
				assert!(f.hc.hid_pending.is_empty(), "matching HID must retain inline progress");
			}
			assert_pointer_once(&hids, "synchronous waits must preserve HID delivery and rearm");
		}
	}
}

#[cfg(candidate)]
fn ownership() {
	let mut f = Fixture::new();
	let mut hids = f.hids(3, 3);
	for control in [transfer(4, 7), transfer(3, 9), 3 << 24 | 7 << 16 | TRB_EV_CMD_COMPLETE << 10] {
		assert!(!keep_hid(&mut f.hc, CC_SUCCESS << 24, control));
		handle_hid_event(&mut f.hc, &mut hids, CC_SUCCESS << 24, control);
	}
	assert!(f.hc.hid_pending.is_empty() && SENT.lock().unwrap().is_empty());
	let (dev, mut net) = f.net(2, 2);
	post_notification(&f.hc, &dev, &mut net);
	assert!(!handle_net_notification(&f.hc, &dev, &mut net, CC_SUCCESS << 24, transfer(3, 3)));
	assert!(!handle_net_notification(&f.hc, &dev, &mut net, CC_SUCCESS << 24, transfer(2, 5)));
	assert!(net.note.as_ref().unwrap().posted);
	f.events(&[(0, CC_SUCCESS << 24, 39 << 10), completed(9)]);
	assert_eq!(wait_event(&mut f.hc, TRB_EV_CMD_COMPLETE), None, "unrelated unsupported event policy must not be weakened");
	assert_eq!(f.hc.evt_index, 1);
}

#[cfg(candidate)]
fn retention() {
	let mut f = Fixture::new();
	f.hc.net_rx = Some((2, 5));
	f.hc.serial_rx[0] = Some((4, 3));
	f.hc.audio_rx.push((5, 3));
	f.hc.class_rx.push((6, 3));
	let events = [
		(0, 0, TRB_EV_PORT_STATUS << 10),
		(0x20, CC_SUCCESS << 24, transfer(2, 5)),
		(0x30, CC_SHORT_PACKET << 24 | 2, transfer(4, 3)),
		(0x40, CC_SUCCESS << 24, transfer(5, 3)),
		(0x50, CC_SUCCESS << 24, transfer(6, 3)),
		completed(9),
	];
	f.events(&events);
	assert_eq!(wait_event(&mut f.hc, TRB_EV_CMD_COMPLETE).map(|event| event.2 >> 24), Some(9));
	assert!(f.hc.ports_changed.take());
	assert_eq!(f.hc.net_pending, Some((events[1].1, events[1].2 | TRB_CYCLE)));
	assert_eq!(f.hc.serial_pending[0], Some((events[2].1, events[2].2 | TRB_CYCLE)));
	assert_eq!(f.hc.audio_pending, vec![(events[3].0, events[3].1, events[3].2 | TRB_CYCLE)]);
	assert_eq!(f.hc.class_pending, vec![(events[4].0, events[4].1, events[4].2 | TRB_CYCLE)]);
	assert_ring_consumed(&f, 6);
}

#[cfg(candidate)]
fn capacity() {
	let mut f = Fixture::new();
	let mut hids = Hids::new();
	for slot in 1..=usb_class::HID_LIMITS.devices {
		let dev = f.dev(slot, slot);
		let interfaces = (0..usb_class::HID_INTERFACES).map(|i| f.hid(3 + 2 * i)).collect();
		hids.entries.push((dev, interfaces));
	}
	post_reports(&mut f.hc, &mut hids);
	assert_eq!(f.hc.hid_rx.len(), 32);
	for &(slot, dci) in &f.hc.hid_rx.clone() {
		assert!(keep_hid(&mut f.hc, CC_SUCCESS << 24, transfer(slot, dci)));
		assert!(keep_hid(&mut f.hc, CC_SUCCESS << 24, transfer(slot, dci)));
	}
	assert_eq!(f.hc.hid_pending.len(), 32, "at most one completion per standing HID report");
	assert!(pre_sleep(&mut f.hc, &mut hids, &mut None));
	assert_eq!(SENT.lock().unwrap().len(), 32, "every admitted HID interface must make progress");
	assert!(hids.entries.iter().all(|(_, interfaces)| interfaces.iter().all(|h| h.ring.index == 2)));
}

#[cfg(candidate)]
fn detach() {
	let mut f = Fixture::new();
	let mut hids = f.hids(3, 3);
	let other = f.hids(4, 4);
	hids.entries.extend(other.entries);
	keep_hid(&mut f.hc, CC_SUCCESS << 24, transfer(3, 7));
	keep_hid(&mut f.hc, CC_SUCCESS << 24, transfer(4, 7));
	f.events(&[(0x10, CC_SUCCESS << 24, transfer(3, 7)), completed(3)]);
	unsafe {
		detach_registered(&mut f.hc, &mut hids, &mut None, 3);
	}
	assert!(!f.hc.hid_pending.iter().any(|&(_, control)| control >> 24 == 3), "detached HID completion must not survive slot reuse");
	assert_eq!(f.hc.hid_rx, vec![(4, 7)]);
	assert_eq!(f.hc.hid_pending.len(), 1);
	let replacement = f.hids(3, 3);
	hids.entries.extend(replacement.entries);
	assert!(pre_sleep(&mut f.hc, &mut hids, &mut None));
	assert_eq!(SENT.lock().unwrap().len(), 1);
	assert_eq!(hids.entries.iter().find(|(dev, _)| dev.slot == 3).unwrap().1[0].ring.index, 1);

	let mut network = Some(f.net(2, 2));
	f.hc.net_notify_pending = Some((CC_SUCCESS << 24, transfer(2, 3)));
	f.events(&[(0x20, CC_SUCCESS << 24, transfer(2, 3)), completed(2)]);
	unsafe {
		detach_registered(&mut f.hc, &mut hids, &mut network, 2);
	}
	assert!(network.is_none() && f.hc.net_notify_rx.is_none() && f.hc.net_notify_pending.is_none(), "notification arriving during disable must not recreate departed ownership");
}

#[cfg(candidate)]
fn nested_stall() {
	let mut f = Fixture::new();
	let mut hids = f.hids(3, 3);
	let other = f.hids(4, 4);
	hids.entries.extend(other.entries);
	let mut network = Some(f.net(2, 2));
	let (dev, net) = network.as_mut().unwrap();
	post_notification(&f.hc, dev, net);
	f.hc.serial_rx[0] = Some((5, 3));
	f.hc.audio_rx.push((6, 3));
	f.hc.class_rx.push((7, 3));
	keep_hid(&mut f.hc, CC_STALL << 24, transfer(3, 7));
	let nested = [
		(0x20, CC_SUCCESS << 24, transfer(2, 5)),
		(0x30, CC_SUCCESS << 24, transfer(5, 3)),
		(0x40, CC_SUCCESS << 24, transfer(6, 3)),
		(0x50, CC_SUCCESS << 24, transfer(7, 3)),
		(0x60, CC_SHORT_PACKET << 24 | 8, transfer(2, 3)),
		(0x70, CC_SUCCESS << 24, transfer(4, 7)),
		completed(3),
		completed(3),
		(0x80, CC_SUCCESS << 24, transfer(3, 1)),
	];
	f.events(&nested);
	assert!(!pre_sleep(&mut f.hc, &mut hids, &mut network), "nested recovery completions must prevent sleep");
	assert_eq!(f.hc.cmd.index, 2, "actual HID stall recovery must issue both controller commands");
	assert_eq!(hids.entries.iter().find(|(dev, _)| dev.slot == 3).unwrap().0.ep0.index, 2, "actual HID stall recovery must clear the device endpoint halt");
	assert!(hids.entries.iter().all(|(_, interfaces)| interfaces[0].ring.index == 2));
	assert_eq!(SENT.lock().unwrap().len(), 1, "other HID must progress inline during real recovery");
	assert_eq!(network.as_ref().unwrap().1.note.as_ref().unwrap().up, Some(true));
	assert!(f.hc.net_notify_pending.is_none() && f.hc.hid_pending.is_empty());
	assert_eq!(f.hc.net_pending, Some((nested[0].1, nested[0].2 | TRB_CYCLE)));
	assert_eq!(f.hc.serial_pending[0], Some((nested[1].1, nested[1].2 | TRB_CYCLE)));
	assert_eq!(f.hc.audio_pending, vec![(nested[2].0, nested[2].1, nested[2].2 | TRB_CYCLE)]);
	assert_eq!(f.hc.class_pending, vec![(nested[3].0, nested[3].1, nested[3].2 | TRB_CYCLE)]);
	assert_ring_consumed(&f, 9);
	// Their unchanged full service consumers are outside this focused proof.
	// The exact production continue decision preserves them for the next pass.
}

#[cfg(candidate)]
fn ring_wrap() {
	let mut f = Fixture::new();
	f.hc.evt_index = RING_TRBS - 1;
	f.event_at((RING_TRBS - 1) as usize, 0x100, CC_SUCCESS << 24, transfer(3, 7) | TRB_CYCLE);
	f.event_at(0, 0x200, CC_SUCCESS << 24, 9 << 24 | TRB_EV_CMD_COMPLETE << 10);
	assert_eq!(unsafe { take_event(&mut f.hc) }.unwrap().0, 0x100);
	assert_eq!((f.hc.evt_index, f.hc.evt_cycle), (0, 0));
	assert_eq!(read64(f.hc.ir0 + IR_ERDP), f.hc.evt_phys | ERDP_EHB);
	assert_eq!(unsafe { take_event(&mut f.hc) }.unwrap().0, 0x200);
	assert_ring_consumed(&f, 1);
}

fn main() {
	let selected = std::env::args().nth(1).expect("named proof selection");
	match selected.as_str() {
		"audio-ready" => usb_audio::ready_period(),
		"audio-boundaries" => usb_audio::command_boundaries(),
		"audio-tail" => usb_audio::ending_period(),
		"audio-late" => usb_audio::late_period(),
		"audio-late-boundaries" => usb_audio::late_boundaries(),
		"command-cdc" => command_cdc(),
		"command-hid" => command_hid(),
		#[cfg(candidate)]
		"service-drain" => service_drain::check(),
		#[cfg(candidate)]
		"detach" => detach(),
		#[cfg(candidate)]
		"nested-stall" => nested_stall(),
		#[cfg(candidate)]
		"all" => {
			command_cdc();
			command_hid();
			waiters();
			ownership();
			retention();
			capacity();
			detach();
			nested_stall();
			ring_wrap();
			println!("xhci-event-retention: PASS 9 groups");
		}
		_ => panic!("unknown proof selection"),
	}
}

/* AUDIO_CASES_BEGIN */
// These cases are inserted inside the actual playback module to inspect its
// private queue/rings. Production methods are unchanged literal source spans.
fn empty_audio(f: &mut Fixture, high_speed: bool, feedback: bool) -> Devices {
	let (max_packet, interval) = if high_speed { (28u16, 125u32) } else { (200, 1000) };
	let (slots, group) = uac::posting(interval, 4096 / u32::from(max_packet));
	let format = uac::FormatOne { channels: 2, subframe_bytes: 2, bits: 16, continuous: false, rates: [48_000; uac::MAX_RATES], rate_count: 1, clocked: high_speed };
	let binding = uac::Binding { config_value: 1, streaming_interface: 1, alternate: 1, endpoint: 1, max_packet, interval: 1, format, sync: uac::Sync::Asynchronous, feedback: None, clock: None };
	let mut pacer = uac::Pacer::new(uac::Pacer::nominal_for(48_000, interval), u32::from(max_packet) / 4);
	let actual_rate = if high_speed { (48_096u64 * 65_536).div_ceil(8_000) as u32 } else { (((48_096u64 * 16_384 + 500) / 1000) as u32) << 2 };
	if feedback {
		assert!(pacer.feedback(actual_rate));
	}
	let page = f.arena.page();
	let sink = Sink { dci: 2, ring: f.arena.ring(), page, virt: page, phys: page, binding, feedback: None, queue: VecDeque::new(), pacer, slots, group, posted: 0, next: 0, inflight: VecDeque::new(), running: false, ending: false, dry: false, underruns: 0, silent_frames: 0, waiting: 0 };
	let mut dev = f.dev(3, 3);
	dev.speed = if high_speed { SPEED_HIGH } else { 1 };
	Devices { list: vec![(dev, Audio { sink: Some(sink), source: None })] }
}

fn setup_audio(f: &mut Fixture, high_speed: bool) -> Devices {
	let mut devices = empty_audio(f, high_speed, true);
	// Reach the low-queue state through legal actual wire calls, not by inventing
	// partial-period queue contents: two accepted periods start the stream.
	for _ in 0..2 {
		unsafe {
			play(&mut f.hc, &mut devices, 88, &vec![0x31; audio::PERIOD_BYTES as usize]);
		}
	}
	assert_eq!(*SENT.lock().unwrap(), vec![(88, audio::OK.to_vec()), (88, audio::OK.to_vec())]);
	SENT.lock().unwrap().clear();
	let (dev, sink) = devices.sink().unwrap();
	assert_eq!(sink.posted, sink.slots);
	assert_eq!(sink.waiting, 0);
	// The first group completes before the next already-credited period arrives.
	// The original partial-refill floor still avoids silence at this point.
	let completed = sink.inflight[sink.group as usize - 1];
	assert!(unsafe { absorb_sink(&mut f.hc, dev, sink, completed, CC_SUCCESS << 24, transfer(3, 2)) });
	assert_eq!((sink.underruns, sink.silent_frames), (0, 0));
	assert!(sink.posted < sink.slots && sink.posted >= sink.group);
	assert!(sink.queue.len() < sink.pacer.peek() as usize * 4);
	assert_eq!(sink.waiting, 0, "the next full period already has credit");
	devices
}

pub fn ready_period() {
	for high_speed in [false, true] {
		let mut f = Fixture::new();
		let mut devices = setup_audio(&mut f, high_speed);
		let period: Vec<u8> = (0..audio::PERIOD_BYTES as usize).map(|i| (i % 251) as u8 | 1).collect();
		*PCM_HEAD.lock().unwrap() = Some((88, period.clone(), 77));
		let serving = common::Serving(vec![(5, 88)]);
		let (completed, retained_prefix, mut expected_pcm) = {
			let (_, sink) = devices.sink().unwrap();
			(sink.inflight[sink.group as usize - 1], sink.inflight.iter().skip(sink.group as usize).copied().collect::<Vec<_>>(), sink.queue.iter().copied().collect::<Vec<_>>())
		};
		expected_pcm.extend_from_slice(&period);
		let taken = unsafe { ready_audio(&mut f.hc, &mut devices, &serving, common::ProviderReady::Consumer(0)) };
		let (dev, sink) = devices.sink().unwrap();
		assert!(unsafe { absorb_sink(&mut f.hc, dev, sink, completed, CC_SUCCESS << 24, transfer(3, 2)) });
		// Preserve the measured original/mutant counters before the causal assertion.
		println!("xhci-audio-order: high_speed={high_speed} took={taken} underruns={} silent_frames={}", sink.underruns, sink.silent_frames);
		assert_eq!((sink.underruns, sink.silent_frames), (0, 0), "a queued period must precede silent refills");
		assert!(taken && PCM_HEAD.lock().unwrap().is_none());
		assert_eq!(*CLOSED.lock().unwrap(), vec![77], "received capability must close exactly once");
		let retained = retained_prefix.len();
		assert_eq!(sink.inflight.iter().take(retained).copied().collect::<Vec<_>>(), retained_prefix, "completion keeps the outstanding original prefix unchanged");
		// The high-speed case wraps the real TRB ring. Its production inflight
		// pointers identify new packets without interpreting LINK as a packet.
		let mut emitted = Vec::new();
		for &trb in sink.inflight.iter().skip(retained) {
			let address = read64(trb);
			let length = unsafe { ((trb + 8) as *const u32).read_volatile() as usize };
			emitted.extend_from_slice(unsafe { core::slice::from_raw_parts(address as *const u8, length) });
		}
		assert!(!emitted.is_empty());
		assert_eq!(emitted, expected_pcm[..emitted.len()], "refilled DMA packets preserve the waiting PCM prefix");
		assert_eq!(sink.queue.iter().copied().collect::<Vec<_>>(), expected_pcm[emitted.len()..]);
		assert_eq!(*SENT.lock().unwrap(), vec![(88, audio::OK.to_vec())], "one request receives exactly one response");
	}
	println!("xhci-audio-order: PASS ready period");
}

pub fn command_boundaries() {
	let mut f = Fixture::new();
	let mut devices = setup_audio(&mut f, false);
	let serving = common::Serving(vec![(5, 88)]);
	// The fast path must leave every non-period request queued for its ORIGINAL
	// post-event dispatch, including a zero-byte EndPlayback and a transferred cap.
	for bytes in [
		vec![],
		vec![audio::CMD_STATS],
		vec![audio::CMD_CAPTURE],
		vec![audio::CMD_CAPTURE_STOP],
		vec![audio::CMD_FORMAT],
		vec![255],
		vec![0; audio::PERIOD_BYTES as usize - 1],
	] {
		*PCM_HEAD.lock().unwrap() = Some((88, bytes.clone(), 77));
		assert!(!unsafe { ready_audio(&mut f.hc, &mut devices, &serving, common::ProviderReady::Consumer(0)) });
		assert_eq!(*PCM_HEAD.lock().unwrap(), Some((88, bytes, 77)));
		assert!(CLOSED.lock().unwrap().is_empty() && SENT.lock().unwrap().is_empty());
	}
	*PCM_HEAD.lock().unwrap() = None;
	PCM_CLOSED.store(88, Ordering::SeqCst);
	assert!(!unsafe { ready_audio(&mut f.hc, &mut devices, &serving, common::ProviderReady::Consumer(0)) });
	PCM_CLOSED.store(0, Ordering::SeqCst);
	assert!(!unsafe { ready_audio(&mut f.hc, &mut devices, &serving, common::ProviderReady::Consumer(0)) });
	*PCM_HEAD.lock().unwrap() = Some((88, vec![1; audio::PERIOD_BYTES as usize], 0));
	assert!(!unsafe { ready_audio(&mut f.hc, &mut devices, &serving, common::ProviderReady::Device(0)) });
	let other = common::Serving(vec![(4, 88)]);
	assert!(!unsafe { ready_audio(&mut f.hc, &mut devices, &other, common::ProviderReady::Consumer(0)) });
	assert!(PCM_HEAD.lock().unwrap().is_some());

	// Existing refusal/credit policy is untouched: a second request while the
	// first is still waiting must not enter the playback queue or replace its owner.
	devices.sink().unwrap().1.waiting = 91;
	let queue_before = devices.sink().unwrap().1.queue.clone();
	assert!(unsafe { ready_audio(&mut f.hc, &mut devices, &serving, common::ProviderReady::Consumer(0)) });
	assert_eq!(devices.sink().unwrap().1.waiting, 91);
	assert_eq!(devices.sink().unwrap().1.queue, queue_before);
	assert_eq!(*SENT.lock().unwrap(), vec![(88, audio::REFUSED.to_vec())]);
	devices.sink().unwrap().1.waiting = 0;
	SENT.lock().unwrap().clear();
	devices.sink().unwrap().1.queue = vec![0x45; PLAYBACK_QUEUE].into();
	*PCM_HEAD.lock().unwrap() = Some((88, vec![1; audio::PERIOD_BYTES as usize], 0));
	assert!(unsafe { ready_audio(&mut f.hc, &mut devices, &serving, common::ProviderReady::Consumer(0)) });
	assert_eq!(devices.sink().unwrap().1.queue.len(), PLAYBACK_QUEUE);
	assert_eq!(*SENT.lock().unwrap(), vec![(88, audio::REFUSED.to_vec())]);
	devices.sink().unwrap().1.queue.clear();
	SENT.lock().unwrap().clear();
	// Once the selected credited period is queued, normal detach still releases
	// its live binding. The wire promises queued PCM, not completion at the speaker.
	*PCM_HEAD.lock().unwrap() = Some((88, vec![1; audio::PERIOD_BYTES as usize], 0));
	assert!(unsafe { ready_audio(&mut f.hc, &mut devices, &serving, common::ProviderReady::Consumer(0)) });
	devices.list[0].1.release();
	devices.list.clear();
	SENT.lock().unwrap().clear();
	*PCM_HEAD.lock().unwrap() = Some((88, vec![1; audio::PERIOD_BYTES as usize], 0));
	assert!(unsafe { ready_audio(&mut f.hc, &mut devices, &serving, common::ProviderReady::Consumer(0)) });
	assert_eq!(*SENT.lock().unwrap(), vec![(88, audio::REFUSED.to_vec())], "a departed sink still refuses playback");
	println!("xhci-audio-order: PASS boundaries");
}

pub fn late_period() {
	for high_speed in [false, true] {
		let mut f = Fixture::new();
		let mut devices = setup_audio(&mut f, high_speed);
		let serving = common::Serving(vec![(5, 88)]);
		// The selector saw no consumer head and chose an IRQ. The period arrives
		// AFTER the existing initial-ready branch, before a later sink completion.
		assert!(!unsafe { ready_audio(&mut f.hc, &mut devices, &serving, common::ProviderReady::Device(0)) });
		assert!(PCM_HEAD.lock().unwrap().is_none());
		let period: Vec<u8> = (0..audio::PERIOD_BYTES as usize).map(|i| ((i + 43) % 251) as u8 | 1).collect();
		let (completed, retained_prefix, mut expected_pcm) = {
			let (_, sink) = devices.sink().unwrap();
			(sink.inflight[sink.group as usize - 1], sink.inflight.iter().skip(sink.group as usize).copied().collect::<Vec<_>>(), sink.queue.iter().copied().collect::<Vec<_>>())
		};
		expected_pcm.extend_from_slice(&period);
		*PCM_HEAD.lock().unwrap() = Some((88, period, 77));
		// Execute the literal raw-ring audio branch and real sink completion.
		let skipped_consumer = unsafe { audio_completion(&mut f.hc, &mut devices, &serving, common::ProviderReady::Device(0), completed, CC_SUCCESS << 24, transfer(3, 2)) };
		let (_, sink) = devices.sink().unwrap();
		println!("xhci-audio-late: high_speed={high_speed} underruns={} silent_frames={} head_pending={}", sink.underruns, sink.silent_frames, PCM_HEAD.lock().unwrap().is_some());
		assert_eq!((sink.underruns, sink.silent_frames), (0, 0), "late credited PCM must precede the next silent refill");
		assert!(!skipped_consumer, "an IRQ selection must not become a consumed consumer selection");
		assert!(PCM_HEAD.lock().unwrap().is_none());
		assert_eq!(*CLOSED.lock().unwrap(), vec![77]);
		let retained = retained_prefix.len();
		assert_eq!(sink.inflight.iter().take(retained).copied().collect::<Vec<_>>(), retained_prefix);
		let mut emitted = Vec::new();
		for &trb in sink.inflight.iter().skip(retained) {
			let address = read64(trb);
			let length = unsafe { ((trb + 8) as *const u32).read_volatile() as usize };
			emitted.extend_from_slice(unsafe { core::slice::from_raw_parts(address as *const u8, length) });
		}
		assert!(!emitted.is_empty());
		assert_eq!(emitted, expected_pcm[..emitted.len()], "late PCM preserves the exact DMA prefix across ring wrap");
		assert_eq!(sink.queue.iter().copied().collect::<Vec<_>>(), expected_pcm[emitted.len()..]);
		assert_eq!(*SENT.lock().unwrap(), vec![(88, audio::OK.to_vec())]);
	}
	println!("xhci-audio-late: PASS late arrival, FS/HS and exact DMA");
}

pub fn late_boundaries() {
	let mut f = Fixture::new();
	let mut devices = setup_audio(&mut f, false);
	// A non-PCM head is never taken, even though a sink event can refill audio.
	for bytes in [
		vec![],
		vec![audio::CMD_STATS],
		vec![audio::CMD_CAPTURE],
		vec![audio::CMD_CAPTURE_STOP],
		vec![audio::CMD_FORMAT],
		vec![255],
		vec![1; audio::PERIOD_BYTES as usize - 1],
	] {
		*PCM_HEAD.lock().unwrap() = Some((88, bytes.clone(), 77));
		assert!(!unsafe { play_before_refill(&mut f.hc, &mut devices, 88, transfer(3, 2)) });
		assert_eq!(*PCM_HEAD.lock().unwrap(), Some((88, bytes, 77)));
	}
	*PCM_HEAD.lock().unwrap() = Some((88, vec![1; audio::PERIOD_BYTES as usize], 77));
	let queue = devices.sink().unwrap().1.queue.clone();
	devices.sink().unwrap().1.waiting = 91;
	assert!(!unsafe { play_before_refill(&mut f.hc, &mut devices, 88, transfer(3, 2)) });
	assert_eq!(devices.sink().unwrap().1.waiting, 91);
	assert_eq!(devices.sink().unwrap().1.queue, queue);
	devices.sink().unwrap().1.waiting = 0;
	devices.sink().unwrap().1.queue = vec![0x45; PLAYBACK_QUEUE].into();
	assert!(!unsafe { play_before_refill(&mut f.hc, &mut devices, 88, transfer(3, 2)) });
	assert_eq!(devices.sink().unwrap().1.queue.len(), PLAYBACK_QUEUE);
	devices.sink().unwrap().1.queue = queue;
	for (server, control) in [(0, transfer(3, 2)), (99, transfer(3, 2)), (88, transfer(4, 2)), (88, transfer(3, 7))] {
		assert!(!unsafe { play_before_refill(&mut f.hc, &mut devices, server, control) });
	}
	assert_eq!(*PCM_HEAD.lock().unwrap(), Some((88, vec![1; audio::PERIOD_BYTES as usize], 77)));
	assert!(CLOSED.lock().unwrap().is_empty() && SENT.lock().unwrap().is_empty());
	*PCM_HEAD.lock().unwrap() = None;
	PCM_CLOSED.store(88, Ordering::SeqCst);
	assert!(!unsafe { play_before_refill(&mut f.hc, &mut devices, 88, transfer(3, 2)) });
	PCM_CLOSED.store(0, Ordering::SeqCst);
	assert!(!unsafe { play_before_refill(&mut f.hc, &mut devices, 88, transfer(3, 2)) });
	// Feedback is the other real refill path. Its endpoint is explicitly bound;
	// credit and the exact head still apply. The normal play operation supplies
	// the one response, including any feedback transfers it posts.
	let page = f.arena.page();
	devices.sink().unwrap().1.feedback = Some(FeedbackPipe { dci: 3, ring: f.arena.ring(), page, phys: page, virt: page, max_packet: 3, group: 1, inflight: VecDeque::new() });
	*PCM_HEAD.lock().unwrap() = Some((88, vec![1; audio::PERIOD_BYTES as usize], 77));
	assert!(unsafe { play_before_refill(&mut f.hc, &mut devices, 88, transfer(3, 3)) });
	assert!(PCM_HEAD.lock().unwrap().is_none());
	assert_eq!(*CLOSED.lock().unwrap(), vec![77]);
	assert_eq!(*SENT.lock().unwrap(), vec![(88, audio::OK.to_vec())]);
	// A second read after the same head was consumed is a no-op.
	assert!(!unsafe { play_before_refill(&mut f.hc, &mut devices, 88, transfer(3, 3)) });
	devices.list[0].1.release();
	devices.list.clear();
	*PCM_HEAD.lock().unwrap() = Some((88, vec![1; audio::PERIOD_BYTES as usize], 78));
	assert!(!unsafe { play_before_refill(&mut f.hc, &mut devices, 88, transfer(3, 2)) });
	assert!(PCM_HEAD.lock().unwrap().is_some());
	// The raw dispatch keeps a non-transfer event out of the fast path and
	// marks an actually consumed selected audio request to avoid another receive.
	let mut f = Fixture::new();
	let mut devices = setup_audio(&mut f, false);
	let serving = common::Serving(vec![(5, 88)]);
	*PCM_HEAD.lock().unwrap() = Some((88, vec![1; audio::PERIOD_BYTES as usize], 77));
	assert!(!unsafe { audio_completion(&mut f.hc, &mut devices, &serving, common::ProviderReady::Consumer(0), 0, CC_SUCCESS << 24, 3 << 24 | 2 << 16 | TRB_EV_CMD_COMPLETE << 10) });
	assert!(PCM_HEAD.lock().unwrap().is_some());
	let (_, sink) = devices.sink().unwrap();
	let completed = sink.inflight[sink.group as usize - 1];
	assert!(unsafe { audio_completion(&mut f.hc, &mut devices, &serving, common::ProviderReady::Consumer(0), completed, CC_SUCCESS << 24, transfer(3, 2)) });
	assert!(PCM_HEAD.lock().unwrap().is_none());
	assert_eq!(*SENT.lock().unwrap(), vec![(88, audio::OK.to_vec())]);
	println!("xhci-audio-late: PASS credit, control, endpoint and closure boundaries");
}

pub fn ending_period() {
	for high_speed in [false, true] {
		let mut f = Fixture::new();
		let mut devices = empty_audio(&mut f, high_speed, false);
		let packet = if high_speed { 24usize } else { 192 };
		let period: Vec<u8> = (0..audio::PERIOD_BYTES as usize).map(|i| ((i + 99 * 7) % 251) as u8 | 1).collect();
		// DMA memory need not contain silence before the writer fills its tail.
		let page = devices.sink().unwrap().1.virt;
		unsafe {
			core::ptr::write_bytes(page as *mut u8, 0xa5, 4096);
		}
		unsafe {
			play(&mut f.hc, &mut devices, 88, &period);
		}
		assert_eq!(*SENT.lock().unwrap(), vec![(88, audio::OK.to_vec())]);
		assert!(!devices.sink().unwrap().1.running);
		assert_eq!(devices.sink().unwrap().1.posted, 0, "one period remains below the ordinary start threshold");
		unsafe {
			end_play(&mut f.hc, &mut devices);
		}
		let (_, sink) = devices.sink().unwrap();
		let count = period.len().div_ceil(packet);
		assert!(sink.running && sink.ending && sink.queue.is_empty());
		assert_eq!(sink.inflight.len(), count);
		assert_eq!(sink.posted as usize, count);
		let mut emitted = Vec::new();
		for &trb in &sink.inflight {
			let address = read64(trb);
			let length = unsafe { ((trb + 8) as *const u32).read_volatile() as usize };
			assert_eq!(length, packet, "an ending packet keeps its paced length");
			emitted.extend_from_slice(unsafe { core::slice::from_raw_parts(address as *const u8, length) });
		}
		let mut expected = period.clone();
		expected.resize(count * packet, 0);
		assert_eq!(emitted, expected, "ending DMA preserves every PCM byte and zero-fills only final tail");
		assert_eq!((sink.underruns, sink.silent_frames), (0, 0), "intentional end padding is not an underrun");
		let final_trb = *sink.inflight.back().unwrap();
		let ring_at = sink.ring.index;
		unsafe {
			end_play(&mut f.hc, &mut devices);
		}
		assert_eq!(devices.sink().unwrap().1.ring.index, ring_at, "an empty ending queue posts no extra packet");
		let (dev, sink) = devices.sink().unwrap();
		assert!(unsafe { absorb_sink(&mut f.hc, dev, sink, final_trb, CC_SUCCESS << 24, transfer(3, 2)) });
		assert!(!sink.running && !sink.ending && sink.inflight.is_empty() && sink.posted == 0);
		assert_eq!(sink.ring.index, ring_at, "the final completion goes idle without another packet");
		assert_eq!((sink.underruns, sink.silent_frames), (0, 0));
		unsafe {
			end_play(&mut f.hc, &mut devices);
		}
		assert_eq!(devices.sink().unwrap().1.ring.index, ring_at);

		// Intentional end padding must not suppress an actual live dry spell.
		let mut f = Fixture::new();
		let mut devices = setup_audio(&mut f, high_speed);
		let (dev, sink) = devices.sink().unwrap();
		let completed = *sink.inflight.back().unwrap();
		assert!(unsafe { absorb_sink(&mut f.hc, dev, sink, completed, CC_SUCCESS << 24, transfer(3, 2)) });
		assert_eq!(sink.underruns, 1);
		assert!(sink.silent_frames > 0 && sink.running && !sink.ending);
	}
	println!("xhci-audio-tail: PASS one period, zero tail, idle and live underrun");
}
/* AUDIO_CASES_END */

/* SERVICE_DRAIN_MODELS_BEGIN */
// The service loop is literal production code. This fixture supplies an event
// producer that replaces each consumed transfer with the next cycle's event;
// three rings is a finite fallback so the original fails instead of hanging.
// Class protocols, absent devices and interrupt notification are explicit models.
static ACKS: AtomicU64 = AtomicU64::new(0);
static INTERRUPTER: AtomicU64 = AtomicU64::new(0);
fn interrupt_ack(irq: u64) {
	assert_eq!(irq, 99);
	assert_eq!(read32(INTERRUPTER.load(Ordering::SeqCst) + IR_IMAN), IMAN_IE | IMAN_IP, "IP must be cleared before interrupt acknowledgment");
	ACKS.fetch_add(1, Ordering::SeqCst);
}
struct Never;
impl Never {
	fn deliver(&mut self, _: &(), _: u64, _: &[u8], _: &mut Vec<u8>) {
		panic!("no serial device is present in this service-drain fixture");
	}
}
struct EmptySerials;
impl EmptySerials {
	fn len(&self) -> usize {
		0
	}
	fn pair(&mut self, _: usize) -> Option<(&mut UsbDevice, &mut Never)> {
		None
	}
}
struct Serving;
impl Serving {
	fn as_slice(&self) -> &[u64] {
		&[]
	}
	fn first_for(&self, _: u16) -> u64 {
		0
	}
	fn token_at(&self, _: usize) -> u16 {
		panic!("no network consumer")
	}
	fn at(&self, _: usize) -> u64 {
		panic!("no network consumer")
	}
	fn close_at(&mut self, _: usize) -> u16 {
		panic!("no network consumer")
	}
}
mod common {
	pub enum ProviderReady {
		Consumer(usize),
		Device(usize),
	}
	pub fn disconnected(_: u64, _: &(), _: u16) {
		panic!("no network consumer")
	}
}
mod usb_uas {
	use super::*;
	pub fn absorb_event(_: &mut Never, _: u32, _: u64, _: u32, _: u32) -> bool {
		panic!("no UAS device")
	}
}
mod usb_serial {
	use super::*;
	pub fn handle_serial_event(_: &mut Xhci, _: &mut UsbDevice, _: &mut Never, _: u32, _: u32, _: &mut Vec<u8>) -> bool {
		panic!("no serial device");
	}
}
mod usb_audio {
	use super::*;
	pub fn absorb(_: &mut Xhci, _: &mut (), _: u64, _: u32, _: u32) -> bool {
		false
	}
	pub fn play_before_refill(_: &mut Xhci, _: &mut (), _: u64, _: u32) -> bool {
		false
	}
	// No audio source is present in the service-drain model, where production answers false at
	// `devices.source()` before it looks at the consumer's channel.
	pub fn request_before_fill(_: &mut Xhci, _: &mut Hids, _: &mut (), _: u64, _: u32) -> bool {
		false
	}
}
fn handle_net_notification(_: &mut Xhci, _: &mut UsbDevice, _: &mut Net, _: u32, _: u32) -> bool {
	panic!("no network device");
}
// The network module's receive output, the frames one transfer carried. No adapter is present here.
#[derive(Default)]
struct Received;
impl Received {
	fn frames(&self) -> core::iter::Empty<&[u8]> {
		core::iter::empty()
	}
}
fn handle_net_event(_: &mut Xhci, _: &mut UsbDevice, _: &mut Net, _: u32, _: u32, _: &mut Received) -> bool {
	panic!("no network device");
}
fn handle_hid_event(_: &mut Xhci, _: &mut Hids, _: u32, _: u32) {
	panic!("only the explicitly owned class and port events may appear");
}
struct RefillingClass {
	limit: u64,
	seen: Vec<u64>,
}
impl RefillingClass {
	fn absorb(&mut self, hc: &mut Xhci, _: &mut Hids, pointer: u64, status: u32, control: u32) -> bool {
		assert_eq!(ACKS.load(Ordering::SeqCst), 1, "event drain must follow IRQ acknowledgment");
		assert_eq!(status, CC_SUCCESS << 24 | (pointer as u32 & 0xffff), "event status remains paired with its pointer");
		assert_eq!(control & !TRB_CYCLE, transfer(7, 3), "event endpoint remains paired with its pointer");
		assert_eq!(control & TRB_CYCLE, 1 ^ ((pointer / RING_TRBS) as u32 & 1));
		assert_eq!(read64(hc.ir0 + IR_ERDP), hc.evt_phys + hc.evt_index * 16 | ERDP_EHB, "the actual dequeue must be published before the class consumes the event");
		self.seen.push(pointer);
		let next = pointer + RING_TRBS;
		if next < self.limit {
			let just_consumed = (hc.evt_index + RING_TRBS - 1) % RING_TRBS;
			let address = hc.evt_virt + just_consumed * 16;
			// Modeled controller writes data first and commits its next cycle bit last.
			unsafe {
				(address as *mut u64).write(next);
				w32(address + 8, CC_SUCCESS << 24 | (next as u32 & 0xffff));
				w32(address + 12, transfer(7, 3) | ((control & TRB_CYCLE) ^ 1));
			}
		}
		true
	}
}
fn begin(f: &Fixture) {
	INTERRUPTER.store(f.hc.ir0, Ordering::SeqCst);
	ACKS.store(0, Ordering::SeqCst);
}
pub fn check() {
	// An empty/coalesced wake still publishes ERDP/EHB, independently of take_event.
	let mut f = Fixture::new();
	let mut class = RefillingClass { limit: 0, seen: Vec::new() };
	begin(&f);
	assert_eq!(unsafe { one_pass(&mut f.hc, &mut class) }, (0, false));
	assert_eq!(read64(f.hc.ir0 + IR_ERDP), f.hc.evt_phys | ERDP_EHB, "empty wake must still clear EHB");
	assert!(class.seen.is_empty());

	let mut f = Fixture::new();
	let initial: Vec<_> = (0..RING_TRBS).map(|n| (n, CC_SUCCESS << 24 | n as u32, transfer(7, 3))).collect();
	f.events(&initial);
	let mut class = RefillingClass { limit: 3 * RING_TRBS, seen: Vec::new() };
	begin(&f);
	let progressed = unsafe { one_pass(&mut f.hc, &mut class) };
	println!("xhci-service-drain: first batch consumed={} pending_consumer={}", class.seen.len(), progressed.0);
	assert_eq!(class.seen.len(), RING_TRBS as usize, "selected consumer must follow at most one ring");
	assert_eq!(progressed, (0, false), "the original selected consumer must be reached");
	assert_eq!(class.seen, (0..RING_TRBS).collect::<Vec<_>>(), "first batch is FIFO with no omissions");
	assert_eq!((f.hc.evt_index, f.hc.evt_cycle), (0, 0));
	assert_eq!(read64(f.hc.evt_virt), RING_TRBS, "the next event remains on the real ring");
	assert_eq!(read32(f.hc.evt_virt + 12) & TRB_CYCLE, f.hc.evt_cycle);
	// Stop replenishing after one further ring; all already-published events remain.
	class.limit = 2 * RING_TRBS;
	begin(&f);
	assert_eq!(unsafe { one_pass(&mut f.hc, &mut class) }, (0, false));
	assert_eq!(class.seen, (0..2 * RING_TRBS).collect::<Vec<_>>(), "later batch preserves all remaining events exactly once");
	assert_eq!((f.hc.evt_index, f.hc.evt_cycle), (0, 1));
	begin(&f);
	assert_eq!(unsafe { one_pass(&mut f.hc, &mut class) }, (0, false));
	assert_eq!(class.seen.len(), (2 * RING_TRBS) as usize);

	// A real port-status event still records its pending signal and asks for the
	// unchanged rescan before dispatch. The external controller also fills the
	// slot consumed by that port event before the next service pass.
	let mut f = Fixture::new();
	let mut port_initial = initial;
	port_initial[3] = (3, 0, TRB_EV_PORT_STATUS << 10);
	f.events(&port_initial);
	let mut class = RefillingClass { limit: 2 * RING_TRBS, seen: Vec::new() };
	begin(&f);
	assert_eq!(unsafe { one_pass(&mut f.hc, &mut class) }, (0, true));
	assert_eq!(class.seen, (0..RING_TRBS).filter(|n| *n != 3).collect::<Vec<_>>());
	assert!(f.hc.ports_changed.is_pending(), "the original short-circuit rescan flag remains intact");
	let next = RING_TRBS + 3;
	f.event_at(3, next, CC_SUCCESS << 24 | next as u32, transfer(7, 3));
	begin(&f);
	assert_eq!(unsafe { one_pass(&mut f.hc, &mut class) }, (0, true));
	assert!(!f.hc.ports_changed.is_pending(), "the retained pending flag is taken on the next original rescan check");
	assert_eq!(class.seen, (0..2 * RING_TRBS).filter(|n| *n != 3).collect::<Vec<_>>(), "port rescan must not drop or reorder remaining transfer events");
	println!("xhci-service-drain: PASS finite batch, FIFO, rescan and IRQ/EHB");
}
/* SERVICE_DRAIN_MODELS_END */
