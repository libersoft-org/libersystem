// THE CORE OVER A MODEL CONTROLLER: a bus the test builds out of hubs, keyboards and disks, played by a
// `Controller` that answers the requests a real device answers - its descriptors at address zero and then at the
// address SET_ADDRESS gave it, a hub's class requests, a keyboard's reports on its interrupt pipe and a disk's
// Bulk-Only commands on its two bulk pipes. What the tests read is what the core did with it: who was addressed and
// bound to what, what was refused and said, what a departure took away, and that nothing it took was left open.

use super::*;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec;

// A DEVICE OF THE MODEL BUS.
#[derive(Clone)]
enum Function {
	// A hub with this many ports, at this speed.
	Hub { ports: u8, power: Vec<bool>, reset_change: Vec<bool>, connect_change: Vec<bool> },
	// A boot keyboard: its reports queued, and whether its next report stalls.
	Keyboard { reports: Vec<[u8; 8]>, stall_next: bool },
	// A Bulk-Only disk of this many sectors.
	Disk { sectors: u32, pending: Option<(u32, Vec<u8>, u32)> },
	// A device that answers four bytes of its descriptor and nothing else.
	Mute,
}

struct Plugged {
	speed: Speed,
	// Where it hangs: a root port, or a model device's index and its port.
	at: (Option<usize>, u32),
	address: u8,
	configured: bool,
	function: Function,
	// What the core cleared a halt on, as (endpoint) values.
	cleared: Vec<u16>,
	wakeup: bool,
}

#[derive(Clone, Copy)]
struct PipeState {
	endpoint: Endpoint,
	// A standing interrupt transfer's page and length.
	standing: Option<(u32, u32)>,
	toggle_resets: u32,
}

struct Model {
	devices: Vec<Plugged>,
	ports: u32,
	// Root port changes not acknowledged yet.
	changed: Vec<bool>,
	pipes: BTreeMap<Pipe, PipeState>,
	next_pipe: Pipe,
	// Every DMA buffer handed out, by the address the "device" sees.
	memory: BTreeMap<u32, Box<[u8]>>,
	next_phys: u32,
	// The device reset on a port and not yet given an address.
	default: Option<usize>,
	splits: bool,
	opened: u32,
	closed: u32,
}

impl Model {
	fn new(ports: u32) -> Model {
		Model { devices: Vec::new(), ports, changed: vec![false; ports as usize + 1], pipes: BTreeMap::new(), next_pipe: 1, memory: BTreeMap::new(), next_phys: 0x1000, default: None, splits: false, opened: 0, closed: 0 }
	}

	fn plug(&mut self, at: (Option<usize>, u32), speed: Speed, function: Function) -> usize {
		self.devices.push(Plugged { speed, at, address: 0, configured: false, function, cleared: Vec::new(), wakeup: false });
		self.devices.len() - 1
	}

	fn hub(ports: u8) -> Function {
		let n = ports as usize + 1;
		Function::Hub { ports, power: vec![false; n], reset_change: vec![false; n], connect_change: vec![true; n] }
	}

	fn keyboard() -> Function {
		Function::Keyboard { reports: Vec::new(), stall_next: false }
	}

	// The device plugged at `at`, if any.
	fn on(&self, at: (Option<usize>, u32)) -> Option<usize> {
		self.devices.iter().position(|device| device.at == at)
	}

	// The device a pipe's address names: zero is the one in its default state.
	fn by_address(&self, address: u8) -> Option<usize> {
		if address == 0 {
			return self.default;
		}
		self.devices.iter().position(|device| device.address == address)
	}

	fn bytes(&mut self, phys: u32, len: usize) -> &mut [u8] {
		let (&base, buffer) = self.memory.range_mut(..=phys).next_back().expect("an address the core was given");
		let at = (phys - base) as usize;
		&mut buffer[at..at + len]
	}

	fn descriptor(&self, index: usize, kind: u16, len: u16) -> Vec<u8> {
		let device = &self.devices[index];
		let class = if matches!(device.function, Function::Hub { .. }) { 9 } else { 0 };
		let mut out = match kind >> 8 {
			1 => {
				let mps0 = if device.speed == Speed::High { 64 } else { 8 };
				let mut d = vec![18u8, 1, 0x00, 0x02, class, 0, 0, mps0, 0x34, 0x12, index as u8, 0x56, 0, 1, 0, 0, 0, 1];
				if matches!(device.function, Function::Mute) {
					d.truncate(4);
				}
				d
			}
			2 => match &device.function {
				Function::Hub { .. } => vec![9, 2, 25, 0, 1, 1, 0, 0xe0, 0, 9, 4, 0, 0, 1, 9, 0, 0, 0, 7, 5, 0x81, 3, 1, 0, 12],
				Function::Keyboard { .. } => vec![9, 2, 34, 0, 1, 1, 0, 0xa0, 50, 9, 4, 0, 0, 1, 3, 1, 1, 0, 9, 0x21, 0x11, 1, 0, 1, 0x22, KEYBOARD.len() as u8, 0, 7, 5, 0x81, 3, 8, 0, 10],
				Function::Disk { .. } => vec![9, 2, 32, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 2, 8, 6, 0x50, 0, 7, 5, 0x81, 2, 64, 0, 0, 7, 5, 0x02, 2, 64, 0, 0],
				Function::Mute => vec![],
			},
			0x22 => KEYBOARD.to_vec(),
			0x29 => match &device.function {
				Function::Hub { ports, .. } => vec![9, 0x29, *ports, 0, 0, 5, 0, 0, 0xff],
				_ => vec![],
			},
			_ => vec![],
		};
		out.truncate(len as usize);
		out
	}

	fn hub_request(&mut self, index: usize, request_type: u8, request: u8, value: u16, port: usize) -> Option<Vec<u8>> {
		let connected = self.on((Some(index), port as u32)).map(|child| self.devices[child].speed);
		let Function::Hub { power, reset_change, connect_change, .. } = &mut self.devices[index].function else { return None };
		match (request_type, request) {
			(0xa3, 0) => {
				let mut status: u16 = if power[port] { 1 << 8 } else { 0 };
				if let Some(speed) = connected
					&& power[port]
				{
					status |= 1 | 2;
					status |= match speed {
						Speed::Low => 1 << 9,
						Speed::High => 1 << 10,
						Speed::Full => 0,
					};
				}
				let change: u16 = (connect_change[port] as u16) | (reset_change[port] as u16) << 4;
				let mut out = status.to_le_bytes().to_vec();
				out.extend_from_slice(&change.to_le_bytes());
				Some(out)
			}
			(0x23, 3) if value == 8 => {
				power[port] = true;
				Some(vec![])
			}
			(0x23, 3) if value == 4 => {
				reset_change[port] = true;
				// THE DEVICE ON THE PORT IS IN ITS DEFAULT STATE NOW.
				self.default = self.on((Some(index), port as u32));
				if let Some(child) = self.default {
					self.devices[child].address = 0;
				}
				Some(vec![])
			}
			(0x23, 1) if value == 20 => {
				reset_change[port] = false;
				Some(vec![])
			}
			(0x23, 1) if value == 16 => {
				connect_change[port] = false;
				Some(vec![])
			}
			(0x23, 1) => Some(vec![]),
			_ => None,
		}
	}
}

// A boot keyboard's report descriptor: eight modifier bits and six key array bytes.
const KEYBOARD: &[u8] = &[
	0x05,
	0x01,
	0x09,
	0x06,
	0xa1,
	0x01,
	0x05,
	0x07,
	0x19,
	0xe0,
	0x29,
	0xe7,
	0x15,
	0x00,
	0x25,
	0x01,
	0x75,
	0x01,
	0x95,
	0x08,
	0x81,
	0x02,
	0x95,
	0x06,
	0x75,
	0x08,
	0x15,
	0x00,
	0x26,
	0xff,
	0x00,
	0x05,
	0x07,
	0x19,
	0x00,
	0x2a,
	0xff,
	0x00,
	0x81,
	0x00,
	0xc0,
];

impl Controller for Model {
	fn who(&self) -> &'static [u8] {
		b"driver.model"
	}

	fn ports(&self) -> u32 {
		self.ports
	}

	fn port(&mut self, port: u32) -> Port {
		let connected = self.on((None, port)).is_some();
		Port { connected, enabled: connected, changed: self.changed[port as usize] }
	}

	fn reset_port(&mut self, port: u32) -> Option<Speed> {
		let device = self.on((None, port))?;
		self.devices[device].address = 0;
		self.default = Some(device);
		Some(self.devices[device].speed)
	}

	fn acknowledge_port(&mut self, port: u32) {
		self.changed[port as usize] = false;
	}

	fn open(&mut self, endpoint: &Endpoint) -> Option<Pipe> {
		let pipe = self.next_pipe;
		self.next_pipe += 1;
		self.pipes.insert(pipe, PipeState { endpoint: *endpoint, standing: None, toggle_resets: 0 });
		self.opened += 1;
		Some(pipe)
	}

	fn close(&mut self, pipe: Pipe) {
		assert!(self.pipes.remove(&pipe).is_some(), "a pipe is closed once");
		self.closed += 1;
	}

	fn retarget(&mut self, pipe: Pipe, address: u8, max_packet: u16) {
		let state = self.pipes.get_mut(&pipe).expect("an open pipe");
		state.endpoint.address = address;
		state.endpoint.max_packet = max_packet;
	}

	fn control(&mut self, pipe: Pipe, setup: &[u8; 8], data: Option<(u32, u16)>) -> Done {
		let address = self.pipes[&pipe].endpoint.address;
		let Some(index) = self.by_address(address) else { return Done::Failed };
		let (request_type, request) = (setup[0], setup[1]);
		let value = u16::from_le_bytes([setup[2], setup[3]]);
		let windex = u16::from_le_bytes([setup[4], setup[5]]);
		let len = u16::from_le_bytes([setup[6], setup[7]]);
		let answer: Option<Vec<u8>> = match (request_type, request) {
			(0x80 | 0x81 | 0xa0, 6) => Some(self.descriptor(index, value, len)),
			(0x00, 5) => {
				self.devices[index].address = value as u8;
				self.default = None;
				Some(vec![])
			}
			(0x00, 9) => {
				self.devices[index].configured = true;
				Some(vec![])
			}
			(0x00, 3) if value == 1 => {
				self.devices[index].wakeup = true;
				Some(vec![])
			}
			(0x02, 1) => {
				self.devices[index].cleared.push(windex);
				Some(vec![])
			}
			(0x21, 0x0b) | (0x21, 0xff) => Some(vec![]),
			(0xa3 | 0x23, _) => self.hub_request(index, request_type, request, value, windex as usize),
			_ => None,
		};
		let Some(answer) = answer else { return Done::Stall(0) };
		if let Some((phys, room)) = data {
			let take = answer.len().min(room as usize);
			self.bytes(phys, take).copy_from_slice(&answer[..take]);
			return Done::Ok(take as u32);
		}
		Done::Ok(0)
	}

	fn bulk(&mut self, pipe: Pipe, phys: u32, len: u32) -> Done {
		let endpoint = self.pipes[&pipe].endpoint;
		let Some(index) = self.by_address(endpoint.address) else { return Done::Failed };
		let Function::Disk { sectors, pending } = &mut self.devices[index].function else { return Done::Failed };
		let sectors = *sectors;
		if !endpoint.dir_in && len == 31 && pending.is_none() {
			let cbw = self.bytes(phys, 31).to_vec();
			let tag = u32::from_le_bytes(cbw[4..8].try_into().unwrap());
			let data_len = u32::from_le_bytes(cbw[8..12].try_into().unwrap());
			let cb = cbw[15..15 + cbw[14] as usize].to_vec();
			let Function::Disk { pending, .. } = &mut self.devices[index].function else { unreachable!() };
			*pending = Some((tag, cb, data_len));
			return Done::Ok(31);
		}
		let (tag, cb, data_len) = pending.clone().expect("a stage after a CBW");
		if endpoint.dir_in && len == 13 {
			let Function::Disk { pending, .. } = &mut self.devices[index].function else { unreachable!() };
			*pending = None;
			let mut csw = b"USBS".to_vec();
			csw.extend_from_slice(&tag.to_le_bytes());
			csw.extend_from_slice(&0u32.to_le_bytes());
			csw.push(0);
			self.bytes(phys, 13).copy_from_slice(&csw);
			return Done::Ok(13);
		}
		assert_eq!(len, data_len, "the data stage is as long as the CBW said");
		if cb[0] == 0x25 {
			let mut answer = (sectors - 1).to_be_bytes().to_vec();
			answer.extend_from_slice(&512u32.to_be_bytes());
			self.bytes(phys, 8).copy_from_slice(&answer);
			return Done::Ok(8);
		}
		Done::Ok(len)
	}

	fn submit(&mut self, pipe: Pipe, phys: u32, len: u32) -> bool {
		self.pipes.get_mut(&pipe).expect("an open pipe").standing = Some((phys, len));
		true
	}

	fn reap(&mut self, pipe: Pipe) -> Option<Done> {
		let state = *self.pipes.get(&pipe)?;
		let (phys, len) = state.standing?;
		let index = self.by_address(state.endpoint.address)?;
		let answer: Option<Result<Vec<u8>, ()>> = match &mut self.devices[index].function {
			Function::Keyboard { stall_next, .. } if *stall_next => {
				*stall_next = false;
				Some(Err(()))
			}
			Function::Keyboard { reports, .. } if !reports.is_empty() => Some(Ok(reports.remove(0).to_vec())),
			Function::Hub { connect_change, .. } => {
				let mut bitmap = vec![0u8; len as usize];
				for (port, changed) in connect_change.iter().enumerate().skip(1) {
					if *changed {
						bitmap[port / 8] |= 1 << (port % 8);
					}
				}
				bitmap.iter().any(|byte| *byte != 0).then_some(Ok(bitmap))
			}
			_ => None,
		};
		let answer = answer?;
		self.pipes.get_mut(&pipe).unwrap().standing = None;
		match answer {
			Ok(bytes) => {
				let take = bytes.len().min(len as usize);
				self.bytes(phys, take).copy_from_slice(&bytes[..take]);
				Some(Done::Ok(take as u32))
			}
			Err(()) => Some(Done::Stall(0)),
		}
	}

	fn reset_toggle(&mut self, pipe: Pipe) {
		self.pipes.get_mut(&pipe).expect("an open pipe").toggle_resets += 1;
	}

	fn service(&mut self) {}

	fn take_port_change(&mut self) -> bool {
		self.changed.iter().any(|changed| *changed)
	}

	fn port_poll(&self) -> Option<u64> {
		None
	}

	fn dma(&mut self, bytes: u32) -> Option<Dma> {
		let phys = self.next_phys;
		self.next_phys += bytes.next_multiple_of(4096);
		let buffer = vec![0u8; bytes as usize].into_boxed_slice();
		let virt = buffer.as_ptr() as u64;
		self.memory.insert(phys, buffer);
		Some(Dma { handle: 0, virt, phys, bytes })
	}

	fn max_transfer(&self) -> u32 {
		64 * 1024
	}

	fn splits(&self) -> bool {
		self.splits
	}

	fn halt(&mut self) -> bool {
		true
	}

	fn run(&mut self) -> bool {
		true
	}
}

fn roles(host: &Host<Model>) -> Vec<Role> {
	host.devices.iter().map(|device| device.role).collect()
}

#[test]
// A HUB WITH A KEYBOARD AND A DISK BEHIND IT, AND A KEYBOARD ON THE OTHER ROOT PORT: four devices addressed, each at
// an address of its own; the hub configured and its ports powered and reset; both keyboards bound with a report
// standing; the disk started and sized; nobody reached through a translator on a full-speed bus.
fn a_hub_its_devices_and_a_root_device_are_addressed_and_bound() {
	let mut model = Model::new(2);
	let hub = model.plug((None, 1), Speed::Full, Model::hub(4));
	model.plug((Some(hub), 1), Speed::Full, Model::keyboard());
	model.plug((Some(hub), 3), Speed::Full, Function::Disk { sectors: 4096, pending: None });
	model.plug((None, 2), Speed::Low, Model::keyboard());
	let mut host = Host::new(model).expect("a scratch page");
	host.scan();
	assert_eq!(roles(&host), [Role::Hub, Role::Keyboard, Role::Storage, Role::Keyboard]);
	let mut addresses: Vec<u8> = host.devices.iter().map(|device| device.address).collect();
	addresses.sort();
	addresses.dedup();
	assert_eq!(addresses.len(), 4, "four addresses, none shared");
	assert!(host.hc.devices.iter().all(|device| device.configured), "every device's configuration was selected");
	assert_eq!(host.devices[1].parent, Some((host.devices[0].address, 1)), "the keyboard hangs off the hub's first port");
	assert_eq!(host.devices[3].speed, Speed::Low);
	assert!(host.devices.iter().all(|device| device.tt.is_none()), "no translator on a full-speed bus");
	assert_eq!(host.disk.as_ref().map(|(_, unit)| unit.capacity), Some(4096 * 512), "the disk was started and sized");
	assert!(host.reports.iter().all(|report| report.standing), "every report is standing");
	assert!(host.devices[1].remote_wakeup && host.hc.devices[1].wakeup, "a keyboard that may wake the machine was told it may");
}

#[test]
// A KEYBOARD'S REPORTS ARE REAPED AND POSTED AGAIN, and one that stalls is unhalted on the device - its endpoint's
// halt cleared - and in the pipe, whose toggle goes back to DATA0, before it stands again.
fn a_report_is_reaped_and_a_stalled_one_is_unhalted_and_posted_again() {
	let mut model = Model::new(1);
	let keyboard = model.plug((None, 1), Speed::Full, Model::keyboard());
	let mut host = Host::new(model).expect("a scratch page");
	host.scan();
	let Function::Keyboard { reports, .. } = &mut host.hc.devices[keyboard].function else { unreachable!() };
	reports.push([0, 0, 0, 0, 0, 0, 0, 0]);
	host.reap_reports();
	assert!(host.reports[0].standing, "posted again after the report");
	let Function::Keyboard { stall_next, .. } = &mut host.hc.devices[keyboard].function else { unreachable!() };
	*stall_next = true;
	host.reap_reports();
	assert_eq!(host.hc.devices[keyboard].cleared, [0x81], "the halt of endpoint 0x81 was cleared on the device");
	assert_eq!(host.hc.pipes[&host.reports[0].pipe].toggle_resets, 1, "and the pipe's toggle reset");
	assert!(host.reports[0].standing, "and the report stands again");
}

#[test]
// UNPLUGGED, THE HUB TAKES EVERYTHING BEHIND IT: its devices leave the inventory, their pipes are closed, their
// addresses are free again and the disk is gone - and what is plugged in again is addressed afresh.
fn a_hub_that_leaves_takes_everything_behind_it_and_gives_it_all_back() {
	let mut model = Model::new(2);
	let hub = model.plug((None, 1), Speed::Full, Model::hub(4));
	model.plug((Some(hub), 2), Speed::Full, Model::keyboard());
	model.plug((Some(hub), 4), Speed::Full, Function::Disk { sectors: 64, pending: None });
	model.plug((None, 2), Speed::Full, Model::keyboard());
	let mut host = Host::new(model).expect("a scratch page");
	host.scan();
	assert_eq!(host.devices.len(), 4);
	let held = host.hc.pipes.len();
	// THE HUB AND WHAT IS ON IT, OUT.
	host.hc.devices.retain(|device| device.at.0.is_none() && device.at.1 == 2);
	host.hc.changed[1] = true;
	assert!(host.hc.take_port_change());
	host.reconcile();
	assert_eq!(roles(&host), [Role::Keyboard], "the root port's keyboard is all that is left");
	assert!(host.disk.is_none(), "the disk left with its hub");
	assert_eq!(host.reports.len(), 1, "and the hub's keyboard's report with it");
	assert_eq!(host.hc.pipes.len(), 2, "endpoint zero and the report pipe of the one keyboard left - {} were held", held);
	assert_eq!(host.used.iter().filter(|used| **used).count(), 2, "address zero and the keyboard's");
	// AND PLUGGED IN AGAIN.
	let hub = host.hc.plug((None, 1), Speed::Full, Model::hub(2));
	host.hc.plug((Some(hub), 1), Speed::Full, Function::Disk { sectors: 64, pending: None });
	host.hc.changed[1] = true;
	host.reconcile();
	assert_eq!(roles(&host), [Role::Keyboard, Role::Hub, Role::Storage]);
}

#[test]
// A FULL-SPEED DEVICE BEHIND A HIGH-SPEED HUB is reached through that hub's translator: a controller that carries
// no split transactions refuses it by name and addresses nothing for it, and one that carries them reaches it with
// the hub's address and port as its translator.
fn a_full_speed_device_behind_a_high_speed_hub_needs_the_hubs_translator() {
	for splits in [false, true] {
		let mut model = Model::new(1);
		model.splits = splits;
		let hub = model.plug((None, 1), Speed::High, Model::hub(2));
		model.plug((Some(hub), 2), Speed::Full, Model::keyboard());
		let mut host = Host::new(model).expect("a scratch page");
		host.scan();
		if splits {
			assert_eq!(roles(&host), [Role::Hub, Role::Keyboard]);
			assert_eq!(host.devices[1].tt, Some(Tt { hub: host.devices[0].address, port: 2 }));
		} else {
			assert_eq!(roles(&host), [Role::Hub], "the keyboard was refused, not addressed");
			assert_eq!(host.hc.opened - host.hc.closed, host.hc.pipes.len() as u32);
		}
	}
}

#[test]
// A DEVICE THAT DOES NOT FINISH ITS ENUMERATION LEAVES NOTHING BEHIND: its endpoint zero is closed and its address
// was never taken.
fn a_device_that_fails_its_enumeration_leaves_nothing_open() {
	let mut model = Model::new(1);
	model.plug((None, 1), Speed::Full, Function::Mute);
	let mut host = Host::new(model).expect("a scratch page");
	host.scan();
	assert!(host.devices.is_empty());
	assert!(host.hc.pipes.is_empty(), "endpoint zero was closed");
	assert_eq!(host.used.iter().filter(|used| **used).count(), 1, "and no address but zero is held");
}

#[test]
// THE CLASS BUDGET IS THE SHARED ONE: a hub of ten keyboards binds the eight the HID module admits, and the other two
// stay addressed and unbound - refused by the same ceiling xHCI's refusal names.
fn the_hid_module_takes_the_keyboards_its_budget_admits_and_no_more() {
	let mut model = Model::new(1);
	let hub = model.plug((None, 1), Speed::Full, Model::hub(10));
	for port in 1..=10 {
		model.plug((Some(hub), port), Speed::Full, Model::keyboard());
	}
	let mut host = Host::new(model).expect("a scratch page");
	host.scan();
	assert_eq!(host.devices.len(), 11);
	assert_eq!(host.devices.iter().filter(|device| device.role == Role::Keyboard).count(), 8);
	assert_eq!(host.devices.iter().filter(|device| device.role == Role::Device).count(), 2);
}
