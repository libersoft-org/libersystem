// AcpiService - the firmware's interpreter, run where a defect in firmware code costs a restart and never the machine.
//
// WHAT IT HOLDS. The `FirmwareInterpreter` privilege, which ServiceManager gives to it alone at every start, and
// through it every authority the kernel mints under its policy: the ACPI tables, SystemMemory regions mapped as the
// memory they are, SystemIO `PortRange`s, the kernel-mediated accesses (the SMI command port, CMOS NVRAM, the PM
// timer, the global lock's release bit), PCI configuration accesses, general-purpose event control, and the reports
// that publish what the namespace describes. The interpreter is `aml`; every decision about a node is `acpi_model`'s.
//
// WHAT IT DOES AT EACH START. Loads the DSDT and every SSDT into one namespace; runs `_REG` for the spaces it serves;
// asks `\_SB._OSC`; walks the namespace with `_STA` and `_INI`; asks each processor's `_OSC` (or `_PDC`) and each
// PCI host bridge's `_OSC`, reporting what the bridge granted; reports every reservation, then every device and
// companion; reports "namespace loaded" - at which the kernel withdraws what this walk did not report again and tells
// DeviceManager; and enables every general-purpose event `\_GPE` answers.
//
// WHAT IT SERVES. DeviceManager's `acpi-admin` root - node channels for claims and live bindings, and the GPIO and
// serial-bus connections the service holds - and each node channel: evaluate, `_DSD`, `_DSM` and `Notify`. A GPE is
// answered by `_Lxx` or `_Exx` (the embedded controller's by its query methods), a GPIO-signalled event by `_Exx`,
// `_Lxx` or `_EVT` in the controller's scope; a device-check or eject `Notify` re-walks the subtree and withdraws what
// left.
//
// A CRASH COSTS A RESTART: the kernel disables every event it enabled and the regions go with its handles; what it
// published stays, and the next instance's walk is reconciled with it by identity.
//
// THE SLEEP STATES. At each start it evaluates `\_S3`, `\_S4` and `\_S5` and registers each sleep-type pair with the
// kernel, which writes them into PM1 control with SLP_EN - so power-off and S3 use what this machine's firmware
// describes. A registered pair outlives this instance: it is firmware data, and a restarted instance registers the
// same. THE PLATFORM'S STEP of the suspend transaction, `platform-sleep`, is served on the control channel ServiceManager
// holds for this service, beside the sleep notice: `prepare` evaluates `_PRW` and `_DSW` (or `_PSW`) on each wake node
// and arms its wake GPE, then runs `_PTS` and `_SST`; `wake` runs `_WAK` and `_SST` and disarms what `prepare` armed.
// The node channels still refuse `_PTS` and `_WAK`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use acpi_model::events::{self, Trigger};
use acpi_model::node::{self, Role, Scope};
use acpi_model::{admission, handshake, properties};
use aml::host::{Access, Host, HostError, TableBytes};
use aml::{Aml, Limits, NodeId, Object, Path, Seg, Space};
use ipc_client::ChannelTransport;
use platform::report::{self, Function, List};
use proto::system::{AcpiConnectionKind, AcpiNotification, Error, SleepState, acpi_admin, acpi_node, gpio_device, i2c_device, platform_sleep, sleep_notice};
use rt::*;
use wire::Handles;

include!(concat!(env!("OUT_DIR"), "/roles_acpi_service.rs"));

// DeviceManager's connections to the admin root at once: one, and room for a relaunched DeviceManager's.
const MAX_ADMINS: usize = 4;
// Node channels served at once - a claim's or a live binding's each.
const MAX_NODE_CHANNELS: usize = 64;
// Notifications queued on one node channel's stream before the oldest is dropped by the channel's own bound.
const NOTIFY_DEPTH: usize = 32;
// Notify values that re-walk the subtree: bus check, device check, eject request.
const NOTIFY_BUS_CHECK: u64 = 0;
const NOTIFY_DEVICE_CHECK: u64 = 1;
const NOTIFY_EJECT: u64 = 3;
// How long a GPIO or serial-bus controller has to answer one transaction.
const CONTROLLER_TICKS: u64 = TICKS_PER_SECOND / 2;

// ONE LINE, WRITTEN WHOLE, so another process's output never lands inside it.
fn say(text: &str) {
	let line = format!("AcpiService: {text}\n");
	print(line.as_bytes());
}

fn now_ns() -> u64 {
	clock_ns()
}

// ---------------------------------------------------------------------------------------------- the calls

fn table(privilege: u64, signature: &[u8; 4], instance: u32) -> Option<Vec<u8>> {
	let selector = u32::from_le_bytes(*signature) as u64 | (instance as u64) << 32;
	let size = unsafe { syscall(SYS_FIRMWARE_TABLE, privilege, selector, 0, 0) } as i64;
	if size <= 0 {
		return None;
	}
	let mut out = vec![0u8; size as usize];
	let got = unsafe { syscall(SYS_FIRMWARE_TABLE, privilege, selector, out.as_mut_ptr() as u64, out.len() as u64) } as i64;
	(got == size).then_some(out)
}

fn mediated(privilege: u64, operation: u64, a: u64, b: u64) -> i64 {
	unsafe { syscall(SYS_FIRMWARE_MEDIATED, privilege, operation, a, b) as i64 }
}

fn pci_address(function: Function) -> u64 {
	(function.segment as u64) << 32 | (function.bus as u64) << 16 | (function.device as u64) << 8 | function.function as u64
}

fn pci_read(privilege: u64, function: Function, offset: u16, width: u8) -> Option<u32> {
	let answer = unsafe { syscall(SYS_FIRMWARE_PCI, privilege, pci_address(function), offset as u64 | (width as u64) << 16, 0) } as i64;
	(answer >= 0).then_some(answer as u32)
}

fn send_report(privilege: u64, bytes: &[u8]) -> i64 {
	unsafe { syscall(SYS_FIRMWARE_REPORT, privilege, bytes.as_ptr() as u64, bytes.len() as u64, 0) as i64 }
}

fn gpe(privilege: u64, operation: u64, number: u16) -> i64 {
	unsafe { syscall(SYS_FIRMWARE_GPE, privilege, operation, number as u64, 0) as i64 }
}

fn errno_text(errno: i64) -> &'static str {
	match errno {
		ERR_ACCESS_DENIED => "the kernel's policy refuses it",
		ERR_INVALID => "the kernel calls it invalid",
		ERR_UNSUPPORTED => "this machine does not support it",
		ERR_NO_MEMORY => "the kernel has no memory for it",
		ERR_RESOURCE_EXHAUSTED => "a table is full",
		_ => "the kernel refused it",
	}
}

// ---------------------------------------------------------------------------------------------- the host

// One SystemMemory region the kernel mapped for this instance, and the node whose region it is: a mapping is reached
// only by the node it was minted for - the kernel's policy decides by node, and a second node's region over the same
// memory is asked for in its own right.
struct Mapped {
	node: String,
	base: u64,
	len: u64,
	virt: u64,
}

// One SystemIO range this instance was granted.
struct Ports {
	base: u16,
	len: u16,
}

// A connection DeviceManager handed over: to the controller `controller` names, for `value` - a line or an address.
struct Held {
	controller: String,
	kind: AcpiConnectionKind,
	value: u32,
	chan: u64,
	// An event line's stream, and the method its events run.
	events: u64,
}

// The embedded controller's two ports, as the transport reads them.
struct EcPorts {
	data: u16,
	command: u16,
}

impl aml::ec::Ports for EcPorts {
	fn status(&mut self) -> u8 {
		port::inb(self.command)
	}
	fn data(&mut self) -> u8 {
		port::inb(self.data)
	}
	fn command(&mut self, value: u8) {
		port::outb(self.command, value);
	}
	fn write_data(&mut self, value: u8) {
		port::outb(self.data, value);
	}
	fn now_us(&mut self) -> u64 {
		now_ns() / 1000
	}
	fn pause(&mut self) {
		core::hint::spin_loop();
	}
}

// What the FADT says the service needs to route accesses: the SMI command port and the PM timer's.
#[derive(Default, Clone, Copy)]
struct Fixed {
	smi_command: Option<u16>,
	pm_timer: Option<u16>,
	facs: Option<u64>,
}

// THE HOST THE INTERPRETER RUNS AGAINST: every access through the kernel, under its policy.
struct Firmware {
	privilege: u64,
	fixed: Fixed,
	// The region the next access is in, as the interpreter announced it: the declaring node's identity, space, base
	// and length.
	current: Option<(String, Space, u64, u64)>,
	regions: Vec<Mapped>,
	ports: Vec<Ports>,
	// Companion nodes by identity, so a region a companion declares names its function.
	companions: Vec<(String, Function)>,
	ec: Option<aml::ec::Ec<EcPorts>>,
	// The embedded controller's `_GLK` said 1: every access to it holds the firmware's global lock.
	ec_glk: bool,
	held: Vec<Held>,
	notifications: Vec<(Path, u64)>,
	facs_lock: Option<u64>,
	// Accesses refused, said once each.
	refused: Vec<String>,
}

fn width_ok(width: u8) -> bool {
	matches!(width, 8 | 16 | 32 | 64)
}

impl Firmware {
	fn refuse(&mut self, why: String) -> HostError {
		if !self.refused.contains(&why) {
			say(&format!("a firmware access is refused - {why}"));
			self.refused.push(why.clone());
		}
		HostError::Refused(why)
	}

	// THE MAPPING an address is in, mapping the announced region first when it covers the address.
	fn memory(&mut self, address: u64, bytes: u64) -> Result<u64, HostError> {
		let Some((node, Space::SystemMemory, base, len)) = self.current.clone() else {
			return Err(self.refuse(format!("a SystemMemory access at {address:#x} outside any region")));
		};
		if let Some(mapped) = self.regions.iter().find(|mapped| mapped.node == node && address >= mapped.base && address + bytes <= mapped.base + mapped.len) {
			return Ok(mapped.virt + (address - mapped.base));
		}
		if address < base || address + bytes > base + len {
			return Err(self.refuse(format!("a SystemMemory access at {address:#x} outside {node}'s region")));
		}
		let mut request = FirmwareMapRequest { base, len, ..FirmwareMapRequest::default() };
		let identity = node.as_bytes();
		let take = identity.len().min(PLATFORM_NAME_LEN);
		request.node[..take].copy_from_slice(&identity[..take]);
		request.node_len = take as u32;
		if let Some((_, function)) = self.companions.iter().find(|(companion, _)| *companion == node) {
			request.companion = pci_address(*function);
		}
		let handle = unsafe { syscall(SYS_FIRMWARE_MAP, self.privilege, &request as *const FirmwareMapRequest as u64, 0, 0) } as i64;
		if handle <= 0 {
			return Err(self.refuse(format!("{node}'s SystemMemory region {base:#x}+{len:#x}: {}", errno_text(handle))));
		}
		let virt = unsafe { syscall(SYS_DEVICE_MEMORY_MAP, handle as u64, 0, 0, 0) } as i64;
		if virt <= 0 {
			close(handle as u64);
			return Err(HostError::Failed(format!("{node}'s region could not be mapped")));
		}
		// THE HANDLE STAYS OPEN: the mapping lives while this instance does, and goes with it.
		self.regions.push(Mapped { node, base, len, virt: virt as u64 });
		Ok(virt as u64 + (address - base))
	}

	// THE PORTS an access reaches, minting the announced region's range first.
	fn io(&mut self, port: u16, bytes: u16) -> Result<(), HostError> {
		if self.ports.iter().any(|held| port >= held.base && port as u32 + bytes as u32 <= held.base as u32 + held.len as u32) {
			return Ok(());
		}
		let Some((node, Space::SystemIo, base, len)) = self.current.clone() else {
			return Err(self.refuse(format!("a SystemIO access at {port:#06x} outside any region")));
		};
		let (Ok(base), Ok(len)) = (u16::try_from(base), u16::try_from(len)) else {
			return Err(self.refuse(format!("{node}'s SystemIO region is not in the port space")));
		};
		if port < base || port as u32 + bytes as u32 > base as u32 + len as u32 {
			return Err(self.refuse(format!("a SystemIO access at {port:#06x} outside {node}'s region")));
		}
		let range = unsafe { syscall(SYS_PORT_RANGE_FIRMWARE, self.privilege, base as u64, len as u64, 0) } as i64;
		if range <= 0 {
			return Err(self.refuse(format!("{node}'s SystemIO region {base:#06x}+{len:#x}: {}", errno_text(range))));
		}
		if port_range_map(range as u64) < 0 {
			close(range as u64);
			return Err(HostError::Failed(format!("{node}'s ports could not be granted")));
		}
		self.ports.push(Ports { base, len });
		Ok(())
	}

	fn io_read(&mut self, port: u16, width: u8) -> Result<u64, HostError> {
		// THE PM TIMER through the kernel: its port is in the reserved set.
		if Some(port) == self.fixed.pm_timer && width == 32 {
			let count = mediated(self.privilege, FIRMWARE_PM_TIMER, 0, 0);
			return if count >= 0 { Ok(count as u64) } else { Err(HostError::Failed(String::from("the PM timer"))) };
		}
		self.io(port, width as u16 / 8)?;
		Ok(match width {
			8 => port::inb(port) as u64,
			16 => port::inw(port) as u64,
			32 => port::inl(port) as u64,
			_ => return Err(HostError::Refused(String::from("a 64-bit port access"))),
		})
	}

	fn io_write(&mut self, port: u16, width: u8, value: u64) -> Result<(), HostError> {
		// THE SMI COMMAND PORT through the kernel, which refuses the value that would leave ACPI mode.
		if Some(port) == self.fixed.smi_command && width == 8 {
			let answer = mediated(self.privilege, FIRMWARE_SMI_COMMAND, value & 0xFF, 0);
			return if answer >= 0 { Ok(()) } else { Err(self.refuse(format!("an SMI command {value:#04x}: {}", errno_text(answer)))) };
		}
		self.io(port, width as u16 / 8)?;
		match width {
			8 => port::outb(port, value as u8),
			16 => port::outw(port, value as u16),
			32 => port::outl(port, value as u32),
			_ => return Err(HostError::Refused(String::from("a 64-bit port access"))),
		}
		Ok(())
	}

	// A serial-bus connection this instance holds, for the controller and address a field's descriptor names.
	fn serial(&self, connection: &[u8]) -> Option<(u64, u16)> {
		let aml::resource::Resource::I2c { address, controller, .. } = aml::resource::decode_one(connection)? else { return None };
		let wanted = absolute_identity(&controller);
		self.held.iter().find(|held| held.kind == AcpiConnectionKind::FieldAddress && held.value == address as u32 && wanted.as_ref().is_none_or(|wanted| *wanted == held.controller)).map(|held| (held.chan, address))
	}

	// A GPIO line this instance holds for reads: a field line, or an event line the service already holds for `_AEI`
	// - a controller grants each line once.
	fn line(&self, connection: &[u8], pin: u16) -> Option<u64> {
		let aml::resource::Resource::Gpio { controller, .. } = aml::resource::decode_one(connection)? else { return None };
		let wanted = absolute_identity(&controller);
		self.held.iter().find(|held| matches!(held.kind, AcpiConnectionKind::FieldLine | AcpiConnectionKind::EventLine) && held.value & 0xFFFF == pin as u32 && wanted.as_ref().is_none_or(|wanted| *wanted == held.controller)).map(|held| held.chan)
	}
}

impl Firmware {
	// ONE EMBEDDED CONTROLLER TRANSACTION, under the global lock when the controller's `_GLK` asks for it - taken with a
	// bound, since the firmware holds it for SMM work of its own.
	fn ec_access<R>(&mut self, access: impl FnOnce(&mut aml::ec::Ec<EcPorts>) -> Result<R, aml::ec::EcError>) -> Result<R, HostError> {
		if self.ec.is_none() {
			return Err(HostError::Unavailable(String::from("no embedded controller transport")));
		}
		let locked = self.ec_glk;
		if locked {
			let until = now_ns() + 100_000_000;
			while !self.global_lock(true) {
				if now_ns() > until {
					return Err(HostError::Failed(String::from("the global lock the embedded controller needs was not released")));
				}
				core::hint::spin_loop();
			}
		}
		let answer = match self.ec.as_mut() {
			Some(ec) => access(ec).map_err(|error| HostError::Failed(format!("the embedded controller: {error:?}"))),
			None => Err(HostError::Unavailable(String::from("no embedded controller transport"))),
		};
		if locked {
			self.global_lock(false);
		}
		answer
	}
}

// A resource source's path as a row identity, when it is absolute.
fn absolute_identity(text: &str) -> Option<String> {
	let name = aml::NameString::parse(text)?;
	if !name.root {
		return None;
	}
	let path = Path(name.segs);
	Some(node::identity(&path))
}

fn i2c_status(reply: &proto::system::I2cReply) -> Result<(), HostError> {
	match reply.status {
		proto::system::I2cStatus::Ok => Ok(()),
		other => Err(HostError::Failed(format!("the controller answered {other:?}"))),
	}
}

impl Host for Firmware {
	fn region(&mut self, node: &Path, space: Space, base: u64, length: u64) {
		self.current = Some((node::identity(node), space, base, length));
	}

	fn read(&mut self, access: Access) -> Result<u64, HostError> {
		if !width_ok(access.width) {
			return Err(HostError::Refused(format!("a {}-bit access", access.width)));
		}
		match access.space {
			Space::SystemMemory => {
				let at = self.memory(access.address, access.width as u64 / 8)?;
				// SAFETY: inside a mapping the kernel made for this region, at the access's own width.
				Ok(unsafe {
					match access.width {
						8 => core::ptr::read_volatile(at as *const u8) as u64,
						16 => core::ptr::read_volatile(at as *const u16) as u64,
						32 => core::ptr::read_volatile(at as *const u32) as u64,
						_ => core::ptr::read_volatile(at as *const u64),
					}
				})
			}
			Space::SystemIo => match u16::try_from(access.address) {
				Ok(port) => self.io_read(port, access.width),
				Err(_) => Err(self.refuse(format!("a SystemIO address {:#x}", access.address))),
			},
			Space::PciConfig => {
				let Some(address) = access.pci else { return Err(HostError::Refused(String::from("a PCI access with no function"))) };
				let function = Function { segment: address.segment, bus: address.bus, device: address.device, function: address.function };
				let (Ok(offset), true) = (u16::try_from(access.address), access.width <= 32) else { return Err(HostError::Refused(String::from("a PCI access past configuration space"))) };
				pci_read(self.privilege, function, offset, access.width / 8).map(|value| value as u64).ok_or_else(|| HostError::Failed(format!("a PCI configuration read at {offset:#x}")))
			}
			Space::SystemCmos => {
				let mut value = 0u64;
				for at in 0..access.width as u64 / 8 {
					let byte = mediated(self.privilege, FIRMWARE_CMOS_READ, access.address + at, 0);
					if byte < 0 {
						return Err(self.refuse(format!("CMOS byte {:#x}: {}", access.address + at, errno_text(byte))));
					}
					value |= (byte as u64) << (8 * at);
				}
				Ok(value)
			}
			Space::EmbeddedControl => {
				let (address, bytes) = (access.address, access.width as u64 / 8);
				self.ec_access(|ec| {
					let mut value = 0u64;
					for at in 0..bytes {
						value |= (ec.read((address + at) as u8)? as u64) << (8 * at);
					}
					Ok(value)
				})
			}
			other => Err(self.refuse(format!("a {} access", other.name()))),
		}
	}

	fn write(&mut self, access: Access, value: u64) -> Result<(), HostError> {
		if !width_ok(access.width) {
			return Err(HostError::Refused(format!("a {}-bit access", access.width)));
		}
		match access.space {
			Space::SystemMemory => {
				let at = self.memory(access.address, access.width as u64 / 8)?;
				// SAFETY: as the read.
				unsafe {
					match access.width {
						8 => core::ptr::write_volatile(at as *mut u8, value as u8),
						16 => core::ptr::write_volatile(at as *mut u16, value as u16),
						32 => core::ptr::write_volatile(at as *mut u32, value as u32),
						_ => core::ptr::write_volatile(at as *mut u64, value),
					}
				}
				Ok(())
			}
			Space::SystemIo => match u16::try_from(access.address) {
				Ok(port) => self.io_write(port, access.width, value),
				Err(_) => Err(self.refuse(format!("a SystemIO address {:#x}", access.address))),
			},
			Space::PciConfig => {
				let Some(address) = access.pci else { return Err(HostError::Refused(String::from("a PCI access with no function"))) };
				let function = Function { segment: address.segment, bus: address.bus, device: address.device, function: address.function };
				let (Ok(offset), true) = (u16::try_from(access.address), access.width <= 32) else { return Err(HostError::Refused(String::from("a PCI access past configuration space"))) };
				let answer = unsafe { syscall(SYS_FIRMWARE_PCI, self.privilege, pci_address(function), offset as u64 | (access.width as u64 / 8) << 16 | FIRMWARE_PCI_WRITE, value) } as i64;
				if answer < 0 { Err(self.refuse(format!("a PCI configuration write to {:02x}:{:02x}.{} at {offset:#x}: {}", function.bus, function.device, function.function, errno_text(answer)))) } else { Ok(()) }
			}
			Space::SystemCmos => {
				for at in 0..access.width as u64 / 8 {
					let answer = mediated(self.privilege, FIRMWARE_CMOS_WRITE, access.address + at, (value >> (8 * at)) & 0xFF);
					if answer < 0 {
						return Err(self.refuse(format!("CMOS byte {:#x}: {}", access.address + at, errno_text(answer))));
					}
				}
				Ok(())
			}
			Space::EmbeddedControl => {
				let (address, bytes) = (access.address, access.width as u64 / 8);
				self.ec_access(|ec| {
					for at in 0..bytes {
						ec.write((address + at) as u8, (value >> (8 * at)) as u8)?;
					}
					Ok(())
				})
			}
			// A GENERALPURPOSEIO WRITE IS REFUSED BY NAME: output lines are not supported.
			Space::GeneralPurposeIo => Err(self.refuse(String::from("a GeneralPurposeIo write - output lines are not supported"))),
			other => Err(self.refuse(format!("a {} access", other.name()))),
		}
	}

	fn serial_bus(&mut self, connection: &[u8], protocol: u8, length: u8, command: u64, write: Option<&[u8]>) -> Result<Vec<u8>, HostError> {
		let Some((chan, address)) = self.serial(connection) else {
			return Err(self.refuse(String::from("a GenericSerialBus field names a connection this instance was not granted")));
		};
		let mut client = i2c_device::Client::with_deadline(ChannelTransport { chan }, clock() + CONTROLLER_TICKS);
		let command = command as u8;
		let answered = match (protocol, write) {
			(aml::host::protocol::QUICK, write) => client.quick(&write.is_none()),
			(aml::host::protocol::SEND_RECEIVE, Some(bytes)) => client.send_byte(&bytes.first().copied().unwrap_or(0), &false),
			(aml::host::protocol::SEND_RECEIVE, None) => client.receive_byte(&false),
			(aml::host::protocol::BYTE, Some(bytes)) => client.write_byte_data(&command, &bytes.first().copied().unwrap_or(0), &false),
			(aml::host::protocol::BYTE, None) => client.read_byte_data(&command, &false),
			(aml::host::protocol::WORD, Some(bytes)) => client.write_word_data(&command, &u16::from_le_bytes([bytes.first().copied().unwrap_or(0), bytes.get(1).copied().unwrap_or(0)]), &false),
			(aml::host::protocol::WORD, None) => client.read_word_data(&command, &false),
			(aml::host::protocol::BLOCK, Some(bytes)) => client.block_write(&command, bytes, &false),
			(aml::host::protocol::BLOCK, None) => client.block_read(&command, &false),
			(aml::host::protocol::BYTES, Some(bytes)) => {
				let mut data = vec![command];
				data.extend_from_slice(&bytes[..bytes.len().min(length as usize)]);
				client.write(&data)
			}
			(aml::host::protocol::BYTES, None) => client.i2c_block_read(&command, &length, &false),
			_ => return Err(self.refuse(format!("serial-bus protocol {protocol:#04x} at {address:#04x}"))),
		};
		match answered {
			Some(Ok(reply)) => {
				i2c_status(&reply)?;
				Ok(reply.bytes)
			}
			Some(Err(error)) => Err(HostError::Failed(format!("the controller refused: {error:?}"))),
			None => Err(HostError::Failed(String::from("the controller did not answer"))),
		}
	}

	fn gpio_read(&mut self, connection: &[u8], pin: u16) -> Result<bool, HostError> {
		let Some(chan) = self.line(connection, pin) else {
			return Err(self.refuse(format!("a GeneralPurposeIo field reads line {pin}, which this instance was not granted")));
		};
		match gpio_device::Client::with_deadline(ChannelTransport { chan }, clock() + CONTROLLER_TICKS).level() {
			Some(Ok(level)) => Ok(level),
			Some(Err(error)) => Err(HostError::Failed(format!("line {pin}: {error:?}"))),
			None => Err(HostError::Failed(format!("line {pin}'s controller did not answer"))),
		}
	}

	fn table(&mut self, signature: [u8; 4], oem_id: &[u8], oem_table_id: &[u8]) -> Option<TableBytes> {
		for instance in 0..64 {
			let bytes = table(self.privilege, &signature, instance)?;
			if bytes.len() >= 24 && bytes[10..16].trim_ascii_end() == oem_id.trim_ascii_end() && bytes[16..24].trim_ascii_end() == oem_table_id.trim_ascii_end() {
				return Some(TableBytes { bytes });
			}
		}
		None
	}

	fn table_address(&mut self, _signature: [u8; 4], _oem_id: &[u8], _oem_table_id: &[u8]) -> Option<(u64, u64)> {
		// A `DataTableRegion` is a table's bytes in memory the kernel does not map for firmware code.
		None
	}

	fn sleep(&mut self, ms: u64) {
		sleep_until(clock() + (ms * TICKS_PER_SECOND).div_ceil(1000));
	}

	fn stall(&mut self, us: u64) {
		let until = now_ns() + us.min(100) * 1000;
		while now_ns() < until {
			core::hint::spin_loop();
		}
	}

	fn timer(&mut self) -> u64 {
		now_ns() / 100
	}

	fn now_ms(&mut self) -> u64 {
		now_ns() / 1_000_000
	}

	fn notify(&mut self, node: &Path, value: u64) {
		self.notifications.push((node.clone(), value));
	}

	fn debug(&mut self, text: &str) {
		say(&format!("firmware debug: {text}"));
	}

	fn fatal(&mut self, kind: u8, code: u32, argument: u64) {
		say(&format!("the firmware declares a fatal error: type {kind:#x}, code {code:#x}, argument {argument:#x}"));
	}

	// THE GLOBAL LOCK, in the FACS the firmware shares: bit 1 owned, bit 0 pending. Taken when free; marked pending
	// when the firmware holds it, and the firmware's release raises the event this service is told of. Given up with
	// `GBL_RLS` when the firmware asked for it meanwhile.
	fn global_lock(&mut self, acquire: bool) -> bool {
		let Some(at) = self.facs_lock else { return true };
		// SAFETY: the FACS's global lock dword, mapped write-back for this instance; the firmware updates it with the
		// same atomic protocol.
		let lock = unsafe { &*(at as *const core::sync::atomic::AtomicU32) };
		if acquire {
			let mut current = lock.load(core::sync::atomic::Ordering::Acquire);
			loop {
				let owned = current & 2 != 0;
				let next = if owned { current | 1 } else { (current & !3) | 2 };
				match lock.compare_exchange(current, next, core::sync::atomic::Ordering::AcqRel, core::sync::atomic::Ordering::Acquire) {
					Ok(_) => return !owned,
					Err(seen) => current = seen,
				}
			}
		}
		let previous = lock.fetch_and(!3, core::sync::atomic::Ordering::AcqRel);
		if previous & 1 != 0 {
			let _ = mediated(self.privilege, FIRMWARE_GLOBAL_LOCK_RELEASE, 0, 0);
		}
		true
	}
}

// ---------------------------------------------------------------------------------------------- the service

// One node channel: the node it is scoped to, its class (for the parent methods a class row admits), and its
// notification stream.
struct NodeChannel {
	chan: u64,
	node: NodeId,
	identity: String,
	class: Option<&'static str>,
	stream: u64,
	sequence: u32,
}

// What this instance published, by node: the row's identity, its role and its class.
struct Published {
	node: NodeId,
	identity: String,
	role: Role,
}

struct Service {
	aml: Aml,
	host: Firmware,
	instance: u64,
	events: u64,
	admin_root: u64,
	admins: Vec<u64>,
	channels: Vec<NodeChannel>,
	published: Vec<Published>,
	// The event this instance's embedded controller raises, and its node.
	ec_gpe: Option<u16>,
	ec_node: Option<NodeId>,
	// The wake GPEs `prepare` armed for the sleep in progress, which `wake` disarms.
	wake_armed: Vec<u16>,
}

impl Service {
	fn node_of_identity(&self, identity: &str) -> Option<NodeId> {
		if let Some(published) = self.published.iter().find(|published| published.identity == identity) {
			return Some(published.node);
		}
		let path = identity.strip_prefix(node::IDENTITY_PREFIX)?;
		self.aml.lookup(path)
	}

	fn evaluate(&mut self, node: NodeId, args: &[Object], what: &str) -> Option<Object> {
		let answer = self.aml.evaluate(node, args, &mut self.host);
		let result = match answer {
			Ok(value) => value,
			Err(error) => {
				say(&format!("{what} {} failed - {error:?}", self.aml.ns.path(node)));
				None
			}
		};
		self.deliver_notifications();
		result
	}

	fn run(&mut self, scope: NodeId, name: &[u8; 4], args: &[Object]) -> bool {
		match self.aml.ns.child(scope, Seg(*name)) {
			Some(method) => {
				let _ = self.evaluate(method, args, "the event method");
				true
			}
			None => false,
		}
	}

	fn report(&self, bytes: &[u8], what: &str) -> i64 {
		let answer = send_report(self.host.privilege, bytes);
		if answer < 0 {
			say(&format!("{what} was not published - {}", errno_text(answer)));
		}
		answer
	}

	// ------------------------------------------------------------------ the walk

	// WALK FROM `scope` - the whole namespace at the start, a subtree after a device-check - and report what it holds:
	// reservations first, then every other row and companion. Answers the identities reported.
	fn publish(&mut self, found: Vec<aml::Found>, whole: bool) -> Vec<String> {
		let mut scopes: Vec<(NodeId, Option<Scope>)> = Vec::new();
		let mut classified: Vec<(aml::Found, aml::Identity, Role)> = Vec::new();
		for item in found {
			let identity = match self.aml.identity(item.node, &mut self.host) {
				Ok(identity) => identity,
				Err(error) => {
					say(&format!("{} is not accounted for - its identity did not evaluate: {error:?}", item.path));
					continue;
				}
			};
			let inherited = {
				let mut at = self.aml.ns.parent(item.node);
				let mut scope = None;
				while let Some(parent) = at {
					if let Some((_, known)) = scopes.iter().find(|(node, _)| *node == parent) {
						scope = *known;
						break;
					}
					at = self.aml.ns.parent(parent);
				}
				scope
			};
			let parent = self.aml.ns.parent(item.node).unwrap_or(aml::ROOT);
			let has = |aml: &Aml, node: NodeId, name: &[u8; 4]| aml.ns.child(node, Seg(*name)).is_some();
			let objects = node::Objects { bcl: has(&self.aml, item.node, b"_BCL"), bcm: has(&self.aml, item.node, b"_BCM"), parent_dod: has(&self.aml, parent, b"_DOD"), parent_dos: has(&self.aml, parent, b"_DOS") };
			let role = node::role(item.kind, &identity, inherited, &objects);
			let scope = self.role_scope(item.node, &role, inherited);
			scopes.push((item.node, scope));
			classified.push((item, identity, role));
		}
		let mut reported = Vec::new();
		// RESERVATIONS FIRST, so every new row of the walk is kept out of their ranges; THEN THE COMPANIONS, so a device's
		// connection that names a controller's companion joins that controller's row even where the walk reaches the
		// controller after the device - a touchpad below the I2C controller's node names a GPIO controller whose node
		// comes later in the namespace; then everything else.
		let pass_of = |role: &Role| match role {
			Role::Reservation => 0,
			Role::Companion { .. } => 1,
			_ => 2,
		};
		for pass in 0..3 {
			for (item, identity, role) in classified.iter() {
				if pass_of(role) != pass {
					continue;
				}
				if let Some(identity_text) = self.account(item, identity, role) {
					reported.push(identity_text);
				}
			}
		}
		if whole {
			say(&format!("the walk found {} node(s)", classified.len()));
		}
		reported
	}

	// THE SCOPE A ROLE MAKES FOR WHAT IS BELOW IT: a host bridge's bus from its `_SEG` and `_BBN`; a bridge's companion
	// roots the functions behind it at its secondary bus, read from the bridge itself.
	fn role_scope(&mut self, node: NodeId, role: &Role, inherited: Option<Scope>) -> Option<Scope> {
		let (segment, bbn) = match role {
			Role::HostBridge => (self.integer(node, b"_SEG").unwrap_or(0) as u16, self.integer(node, b"_BBN").unwrap_or(0) as u8),
			_ => (0, 0),
		};
		let secondary = match *role {
			Role::Companion { segment, bus, device, function } => {
				let function = Function { segment, bus, device, function };
				let header = pci_read(self.host.privilege, function, 0x0E, 1).unwrap_or(0xFF);
				if header & 0x7F == 1 { pci_read(self.host.privilege, function, 0x19, 1).map(|bus| bus as u8) } else { None }
			}
			_ => None,
		};
		node::scope_for(role, segment, bbn, secondary, inherited)
	}

	// THE COMPANIONS, KNOWN BEFORE THE WALK RUNS ANY `_STA`: a region a companion node declares names its function when
	// the kernel is asked to map it - the firmware-held rule - and the walk's own `_STA` methods may reach one.
	fn pre_companions(&mut self) {
		let mut scopes: Vec<(NodeId, Option<Scope>)> = Vec::new();
		for node in self.aml.ns.descendants(aml::ROOT) {
			if self.aml.kind(node) != aml::Kind::Device {
				continue;
			}
			let Ok(identity) = self.aml.identity(node, &mut self.host) else { continue };
			let inherited = {
				let mut at = self.aml.ns.parent(node);
				let mut scope = None;
				while let Some(parent) = at {
					if let Some((_, known)) = scopes.iter().find(|(seen, _)| *seen == parent) {
						scope = *known;
						break;
					}
					at = self.aml.ns.parent(parent);
				}
				scope
			};
			let role = node::role(aml::Kind::Device, &identity, inherited, &node::Objects::default());
			let scope = self.role_scope(node, &role, inherited);
			scopes.push((node, scope));
			if let Role::Companion { segment, bus, device, function } = role {
				let name = node::identity(&self.aml.ns.path(node));
				if !self.host.companions.iter().any(|(held, _)| *held == name) {
					self.host.companions.push((name, Function { segment, bus, device, function }));
				}
			}
		}
		self.host.notifications.clear();
	}

	fn integer(&mut self, node: NodeId, name: &[u8; 4]) -> Option<u64> {
		match self.aml.child_value(node, name, &mut self.host) {
			Ok(Some(Object::Integer(value))) => Some(value),
			_ => None,
		}
	}

	// ONE NODE, ACCOUNTED FOR: reported as what it is, or said to be what it is not. Answers the identity reported.
	fn account(&mut self, item: &aml::Found, identity: &aml::Identity, role: &Role) -> Option<String> {
		let name = node::identity(&item.path);
		match role {
			Role::HostBridge => {
				self.bridge(item.node, &name);
				None
			}
			Role::Processor => {
				self.processor(item.node, &name);
				None
			}
			Role::NotADevice(why) => {
				say(&format!("{name} is not a device - {why}"));
				None
			}
			Role::Companion { segment, bus, device, function } => {
				let function = Function { segment: *segment, bus: *bus, device: *device, function: *function };
				self.companion(item.node, &name, function).then_some(name)
			}
			Role::Reservation | Role::Device { .. } | Role::EmbeddedController | Role::ThermalZone => self.device(item.node, &name, identity, role).then_some(name),
		}
	}

	fn bridge(&mut self, node: NodeId, name: &str) {
		let segment = self.integer(node, b"_SEG").unwrap_or(0) as u16;
		let bbn = self.integer(node, b"_BBN").unwrap_or(0) as u8;
		let end = self.aml.resources(node, &mut self.host).ok().and_then(|resources| {
			resources.iter().find_map(|resource| match resource {
				aml::resource::Resource::BusNumbers { base, length } if *length > 0 => Some((base + length - 1).min(0xFF) as u8),
				_ => None,
			})
		});
		let request = handshake::pci_request();
		let answer = self.aml.osc(node, handshake::PCI_HOST_BRIDGE, 1, &request, &mut self.host);
		self.deliver_notifications();
		let answered = match answer {
			Ok(answer) => answer,
			Err(error) => {
				say(&format!("{name}'s _OSC failed - {error:?}; it grants nothing"));
				None
			}
		};
		let granted = handshake::pci_granted(answered.as_deref());
		say(&format!("{name} is a PCI host bridge (segment {segment}, buses {bbn:#04x}..{:#04x}) - its _OSC grants native {}", end.unwrap_or(0xFF), handshake::pci_control_text(granted)));
		let mut out = [0u8; 16];
		if let Some(len) = report::encode_osc(segment, bbn, end.unwrap_or(0xFF), granted, &mut out) {
			self.report(&out[..len], "the host bridge's _OSC answer");
		}
	}

	fn processor(&mut self, node: NodeId, name: &str) {
		let answered = if self.aml.ns.child(node, Seg(*b"_OSC")).is_some() {
			let answer = self.aml.osc(node, handshake::PROCESSOR, 1, &handshake::processor_request(), &mut self.host);
			matches!(answer, Ok(Some(_)))
		} else if let Some(pdc) = self.aml.ns.child(node, Seg(*b"_PDC")) {
			self.evaluate(pdc, &[Object::Buffer(handshake::pdc_buffer().to_vec())], "_PDC");
			true
		} else {
			false
		};
		self.deliver_notifications();
		say(&format!("{name} is a processor - the service's own{}", if answered { ", told the forms the kernel executes" } else { "" }));
	}

	fn companion(&mut self, node: NodeId, name: &str, function: Function) -> bool {
		// NOT ON THE BUS, OR A BRIDGE: no row to join.
		let vendor = pci_read(self.host.privilege, function, 0, 2).unwrap_or(0xFFFF);
		if vendor == 0xFFFF {
			say(&format!("{name} names {:02x}:{:02x}.{}, which is not on the bus", function.bus, function.device, function.function));
			return false;
		}
		let (aei, field_lines, field_addresses) = self.lists_for(node, name);
		let path = name.as_bytes();
		let companion = report::CompanionReport { path, function, aei_lines: aei, field_lines, field_addresses };
		let mut out = [0u8; 1024];
		let Some(len) = report::encode_companion(&companion, &mut out) else { return false };
		let row = self.report(&out[..len], name);
		if row < 0 {
			return false;
		}
		if !self.host.companions.iter().any(|(held, _)| held == name) {
			self.host.companions.push((String::from(name), function));
		}
		self.remember(node, name, Role::Companion { segment: function.segment, bus: function.bus, device: function.device, function: function.function });
		true
	}

	// THE LINES AND ADDRESSES THE SERVICE HOLDS THROUGH A CONTROLLER: its `_AEI` lines - each with its trigger in the top
	// byte, as DeviceManager scopes the connection - and those its own fields name.
	fn lists_for(&mut self, node: NodeId, name: &str) -> (List, List, List) {
		let mut aei = List::default();
		if let Ok(Some(Object::Buffer(bytes))) = self.aml.child_value(node, b"_AEI", &mut self.host)
			&& let Ok(resources) = aml::resource::decode(&bytes)
		{
			for resource in resources {
				if let aml::resource::Resource::Gpio { interrupt: true, pins, level, active_low, .. } = resource {
					let trigger = match (level, active_low) {
						(true, false) => 4u32,
						(true, true) => 5,
						(false, false) => 1,
						(false, true) => 2,
					};
					for pin in pins {
						aei.push(pin as u32 | trigger << 24);
					}
				}
			}
		}
		let (field_lines, field_addresses) = self.field_connections(name);
		(aei, field_lines, field_addresses)
	}

	// THE FIELDS' CONNECTIONS to the controller `controller` names: GPIO lines and serial-bus addresses.
	fn field_connections(&self, controller: &str) -> (List, List) {
		let mut lines = List::default();
		let mut addresses = List::default();
		for node in self.aml.ns.descendants(aml::ROOT) {
			let Some(object) = self.aml.ns.object(node) else { continue };
			let Object::Field(field) = &*object.borrow() else { continue };
			let Some(connection) = field.connection.as_ref() else { continue };
			match aml::resource::decode_one(&connection.0) {
				Some(aml::resource::Resource::Gpio { pins, controller: named, .. }) if absolute_identity(&named).as_deref() == Some(controller) => {
					for pin in pins {
						if !lines.as_slice().contains(&(pin as u32)) {
							lines.push(pin as u32);
						}
					}
				}
				Some(aml::resource::Resource::I2c { address, controller: named, .. }) if absolute_identity(&named).as_deref() == Some(controller) => {
					if !addresses.as_slice().contains(&(address as u32)) {
						addresses.push(address as u32);
					}
				}
				_ => {}
			}
		}
		(lines, addresses)
	}

	fn device(&mut self, node: NodeId, name: &str, identity: &aml::Identity, role: &Role) -> bool {
		let resources = match self.aml.resources(node, &mut self.host) {
			Ok(resources) => resources,
			Err(error) => {
				say(&format!("{name} is not published - its _CRS: {error:?}"));
				return false;
			}
		};
		let mut dsd = self.aml.properties(node, &mut self.host).unwrap_or_default();
		// AN IPMI NODE'S INTERFACE TYPE, for the kernel's check against SMBIOS's type-38 records.
		if identity.hid.as_deref() == Some("IPI0001")
			&& let Some(interface) = self.integer(node, b"_IFT")
		{
			dsd.values.push((String::from("_IFT"), aml::dsd::Value::Integer(interface)));
		}
		let block = properties::block(identity.uid.as_deref(), &dsd);
		if block.cut {
			say(&format!("{name}'s properties are past the block's bound - cut at a record"));
		}
		self.deliver_notifications();
		let aml = &self.aml;
		let mut resolve = |text: &str| aml.lookup_from(node, text).map(|target| node::identity(&aml.ns.path(target)));
		let described = match node::describe(&self.aml.ns.path(node), role, identity, &resources, block.bytes, &mut resolve) {
			Ok(described) => described,
			Err(why) => {
				say(&format!("{name} is not published - {why}"));
				return false;
			}
		};
		for refusal in &described.refused {
			say(&format!("{name}: {refusal}"));
		}
		let mut targets: [(u8, &[u8]); MAX_PLATFORM_CONNECTIONS] = [(0, &[]); MAX_PLATFORM_CONNECTIONS];
		for (at, (connection, identity)) in described.targets.iter().enumerate().take(MAX_PLATFORM_CONNECTIONS) {
			targets[at] = (*connection, identity.as_bytes());
		}
		let device = report::DeviceReport { description: described.description, targets, target_count: described.targets.len().min(MAX_PLATFORM_CONNECTIONS), parent: described.parent, properties: &described.properties };
		let mut out = vec![0u8; report::MAX_REPORT];
		let Some(len) = report::encode_device(&device, &mut out) else {
			say(&format!("{name} is not published - its report is past the bound"));
			return false;
		};
		let row = self.report(&out[..len], name);
		if row < 0 {
			return false;
		}
		let what = match role {
			Role::Reservation => "a reservation",
			Role::EmbeddedController => "the embedded controller, firmware-held",
			Role::ThermalZone => "a thermal zone",
			_ if described.method_only() => "a method-only device",
			_ => "a platform device",
		};
		say(&format!("{name} is {what} (row {row})"));
		self.remember(node, name, role.clone());
		// A CONTROLLER ROW'S LINES AND ADDRESSES, as a companion's are.
		let (aei, field_lines, field_addresses) = self.lists_for(node, name);
		if aei.count + field_lines.count + field_addresses.count != 0 {
			let lists = report::ListsReport { identity: name.as_bytes(), aei_lines: aei, field_lines, field_addresses };
			let mut out = [0u8; 1024];
			if let Some(len) = report::encode_lists(&lists, &mut out) {
				self.report(&out[..len], name);
			}
		}
		true
	}

	fn remember(&mut self, node: NodeId, identity: &str, role: Role) {
		self.published.retain(|published| published.identity != identity);
		self.published.push(Published { node, identity: String::from(identity), role });
	}

	fn class_of(&self, identity: &str) -> Option<&'static str> {
		self.published.iter().find(|published| published.identity == identity).and_then(|published| match published.role {
			Role::Device { class, .. } => class,
			_ => None,
		})
	}

	// ------------------------------------------------------------------ events

	// EVERY `Notify` the last evaluation raised: to each channel scoped to the node, and a device check, a bus check or
	// an eject re-walks the subtree and withdraws what left.
	fn deliver_notifications(&mut self) {
		let pending = core::mem::take(&mut self.host.notifications);
		for (path, value) in pending {
			let identity = node::identity(&path);
			say(&format!("Notify({}, {value:#x})", path));
			let mut frame = [0u8; 64];
			for channel in self.channels.iter_mut().filter(|channel| channel.identity == identity && channel.stream != 0) {
				channel.sequence = channel.sequence.wrapping_add(1);
				let mut handles = Handles::new();
				if let Some(len) = acpi_node::notifications_frame(channel.sequence, &AcpiNotification { value: value as u32, sequence: channel.sequence }, &mut frame, &mut handles)
					&& !try_send(channel.stream, &frame[..len], 0)
				{
					say(&format!("a Notify for {identity} was not taken by its driver's stream"));
				}
			}
			if matches!(value, NOTIFY_BUS_CHECK | NOTIFY_DEVICE_CHECK | NOTIFY_EJECT)
				&& let Some(node) = self.aml.ns.lookup_path(&path)
			{
				self.rewalk(node);
			}
		}
	}

	// A SUBTREE AGAIN: what it still holds is reported (reconciled by identity - the same rows, no events), and what
	// it held and does not any more is withdrawn.
	fn rewalk(&mut self, scope: NodeId) {
		let before: Vec<String> = {
			let prefix = node::identity(&self.aml.ns.path(scope));
			self.published.iter().filter(|published| published.identity == prefix || published.identity.starts_with(&format!("{prefix}."))).map(|published| published.identity.clone()).collect()
		};
		let walk = self.aml.rewalk(scope, &mut self.host);
		let reported = self.publish(walk.found, false);
		for gone in before.into_iter().filter(|identity| !reported.contains(identity)) {
			let mut out = [0u8; 128];
			if let Some(len) = report::encode_withdraw(gone.as_bytes(), &mut out) {
				self.report(&out[..len], &gone);
			}
			say(&format!("{gone} left - withdrawn"));
			self.published.retain(|published| published.identity != gone);
		}
	}

	// ONE LATCHED GENERAL-PURPOSE EVENT: the embedded controller's queries, or `_Exx` (acknowledged first) or `_Lxx`
	// (acknowledged after), then enabled again.
	fn general_purpose_event(&mut self, number: u16) {
		let privilege = self.host.privilege;
		if Some(number) == self.ec_gpe {
			gpe(privilege, GPE_ACKNOWLEDGE, number);
			self.ec_queries();
			gpe(privilege, GPE_REARM, number);
			return;
		}
		let scope = self.aml.lookup("\\_GPE").unwrap_or(aml::ROOT);
		let edge = events::name(number, Trigger::Edge).filter(|name| self.aml.ns.child(scope, Seg(*name)).is_some());
		let level = events::name(number, Trigger::Level).filter(|name| self.aml.ns.child(scope, Seg(*name)).is_some());
		match (edge, level) {
			(Some(name), _) => {
				gpe(privilege, GPE_ACKNOWLEDGE, number);
				say(&format!("general-purpose event {number:#04x} - running {}", core::str::from_utf8(&name).unwrap_or("?")));
				self.run(scope, &name, &[]);
			}
			(None, Some(name)) => {
				say(&format!("general-purpose event {number:#04x} - running {}", core::str::from_utf8(&name).unwrap_or("?")));
				self.run(scope, &name, &[]);
				gpe(privilege, GPE_ACKNOWLEDGE, number);
			}
			(None, None) => {
				say(&format!("general-purpose event {number:#04x} has no method - left disabled"));
				gpe(privilege, GPE_ACKNOWLEDGE, number);
				gpe(privilege, GPE_DISABLE, number);
				return;
			}
		}
		let answer = gpe(privilege, GPE_REARM, number);
		if answer < 0 {
			say(&format!("general-purpose event {number:#04x} could not be enabled again - {}", errno_text(answer)));
		} else {
			say(&format!("general-purpose event {number:#04x} is enabled again"));
		}
	}

	// THE EMBEDDED CONTROLLER'S EVENTS: every query drained, bounded, and its `_Qxx` run in the controller's scope.
	fn ec_queries(&mut self) {
		let Some(ec) = self.host.ec.as_mut() else { return };
		let drained = ec.drain();
		let Some(scope) = self.ec_node else { return };
		match drained {
			Ok((queries, flooded)) => {
				if flooded {
					say("the embedded controller raised more queries than one event answers - the rest wait for the next");
				}
				for query in queries {
					let name = aml::ec::query_method(query);
					if !self.run(scope, &name, &[]) {
						say(&format!("the embedded controller's query {query:#04x} has no method"));
					}
				}
			}
			Err(error) => say(&format!("the embedded controller's queries could not be read - {error:?}")),
		}
	}

	// A GPIO-SIGNALLED EVENT on a line held for `_AEI`: `_Exx`, `_Lxx` or `_EVT` in the controller's scope, then the
	// line acknowledged so it may fire again.
	fn gpio_event(&mut self, at: usize) {
		let (controller, value, chan, stream) = {
			let held = &self.host.held[at];
			(held.controller.clone(), held.value, held.chan, held.events)
		};
		let mut buf = [0u8; 256];
		loop {
			let (len, handles) = match try_recv_caps(stream, &mut buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return,
				PolledCaps::Closed => {
					close(stream);
					self.host.held[at].events = 0;
					say(&format!("the event stream of {controller} line {} closed", value & 0xFFFF));
					return;
				}
			};
			for &leftover in handles.as_slice() {
				close(leftover);
			}
			let mut frame_handles = Handles::new();
			if gpio_device::events_read(&buf[..len], &mut frame_handles).is_none() {
				continue;
			}
			let pin = value & 0xFFFF;
			let Some(scope) = self.node_of_identity(&controller) else { continue };
			let names: Vec<[u8; 4]> = self.aml.ns.children(scope).into_iter().filter_map(|child| self.aml.ns.path(child).last().map(|seg| seg.0)).collect();
			let has_evt = names.contains(b"_EVT");
			match events::gpio_answer(pin, &names, has_evt) {
				Some(events::Answer::Named(name, _)) => {
					say(&format!("{controller} line {pin} - running {}", core::str::from_utf8(&name).unwrap_or("?")));
					self.run(scope, &name, &[]);
				}
				Some(events::Answer::Evt) => {
					say(&format!("{controller} line {pin} - running _EVT"));
					self.run(scope, b"_EVT", &[Object::Integer(pin as u64)]);
				}
				None => say(&format!("{controller} line {pin} fired and no method answers it")),
			}
			let _ = gpio_device::Client::with_deadline(ChannelTransport { chan }, clock() + CONTROLLER_TICKS).acknowledge();
		}
	}

	// ------------------------------------------------------------------ what it serves

	fn open_node(&mut self, identity: &str) -> Result<u64, Error> {
		if self.channels.len() >= MAX_NODE_CHANNELS {
			return Err(Error::Exhausted);
		}
		let node = self.node_of_identity(identity).ok_or(Error::NotFound)?;
		let (mine, theirs) = channel().ok_or(Error::Again)?;
		let class = self.class_of(identity);
		self.channels.push(NodeChannel { chan: mine, node, identity: String::from(identity), class, stream: 0, sequence: 0 });
		say(&format!("a node channel for {identity} is open"));
		Ok(theirs)
	}

	fn connection(&mut self, controller: String, kind: AcpiConnectionKind, value: u32, chan: u64) -> Result<(), Error> {
		// THE SAME GRANT AGAIN - a DeviceManager that asks twice - replaces the first.
		if let Some(at) = self.host.held.iter().position(|held| held.controller == controller && held.kind == kind && held.value == value) {
			let old = self.host.held.remove(at);
			close(old.chan);
			if old.events != 0 {
				close(old.events);
			}
		}
		let mut events = 0;
		if kind == AcpiConnectionKind::EventLine {
			events = gpio_device::Client::with_deadline(ChannelTransport { chan }, clock() + CONTROLLER_TICKS).events().unwrap_or(0);
			if events == 0 {
				say(&format!("{controller} line {} gave no event stream", value & 0xFFFF));
			}
		}
		let what = match kind {
			AcpiConnectionKind::EventLine => "_AEI line",
			AcpiConnectionKind::FieldLine => "field line",
			AcpiConnectionKind::FieldAddress => "field address",
		};
		say(&format!("{controller} {what} {:#x} is granted", value & 0xFFFF));
		self.host.held.push(Held { controller, kind, value, chan, events });
		Ok(())
	}

	fn node_evaluate(&mut self, at: usize, name: &str, arguments: &[u8]) -> Result<Vec<u8>, Error> {
		let (node, class) = (self.channels[at].node, self.channels[at].class);
		let target = match admission::admit(name, class) {
			Ok(target) => target,
			Err(refusal) => {
				say(&format!("{} asked for {name} - refused ({refusal:?})", self.channels[at].identity));
				return Err(Error::Denied);
			}
		};
		let target = match target {
			admission::Target::Node => node,
			admission::Target::Own(seg) => self.aml.ns.child(node, Seg(seg)).ok_or(Error::NotFound)?,
			admission::Target::Parent(seg) => {
				let parent = self.aml.ns.parent(node).ok_or(Error::NotFound)?;
				self.aml.ns.child(parent, Seg(seg)).ok_or(Error::NotFound)?
			}
		};
		let args: Vec<Object> = if arguments.is_empty() {
			Vec::new()
		} else {
			match aml::wire::decode(arguments).map_err(|_| Error::Invalid)? {
				aml::wire::Value::Package(elements) if elements.len() <= 7 => elements.iter().map(aml::wire::to_object).collect(),
				_ => return Err(Error::Invalid),
			}
		};
		let answered = self.aml.evaluate(target, &args, &mut self.host);
		self.deliver_notifications();
		let value = answered.map_err(|error| {
			say(&format!("{} evaluated {name} - {error:?}", self.channels[at].identity));
			Error::Io
		})?;
		let aml = &self.aml;
		let wire = match value {
			Some(object) => aml::wire::from_object(&object, &mut |element| aml.element_path(element).map(|path| path.text())),
			None => aml::wire::Value::None,
		};
		aml::wire::encode(&wire).map_err(|_| Error::Exhausted)
	}

	fn node_properties(&mut self, at: usize) -> Result<Vec<u8>, Error> {
		let node = self.channels[at].node;
		let dsd = self.aml.properties(node, &mut self.host).map_err(|_| Error::Io)?;
		let uid = self.aml.identity(node, &mut self.host).ok().and_then(|identity| identity.uid);
		self.deliver_notifications();
		Ok(properties::block(uid.as_deref(), &dsd).bytes)
	}

	fn node_dsm(&mut self, at: usize, uuid: &[u8], revision: u64, function: u64, arguments: &[u8]) -> Result<Vec<u8>, Error> {
		let node = self.channels[at].node;
		let uuid: [u8; 16] = uuid.try_into().map_err(|_| Error::Invalid)?;
		let args = if arguments.is_empty() { Object::Package(Vec::new()) } else { aml::wire::to_object(&aml::wire::decode(arguments).map_err(|_| Error::Invalid)?) };
		let answered = self.aml.dsm_bytes(node, &uuid, revision, function, args, &mut self.host);
		self.deliver_notifications();
		let value = answered.map_err(|_| Error::Io)?.ok_or(Error::NotFound)?;
		let aml = &self.aml;
		aml::wire::encode(&aml::wire::from_object(&value, &mut |element| aml.element_path(element).map(|path| path.text()))).map_err(|_| Error::Exhausted)
	}
}

struct AdminView<'a> {
	service: &'a mut Service,
}

impl acpi_admin::Service for AdminView<'_> {
	fn open_node(&mut self, identity: String) -> Result<u64, Error> {
		self.service.open_node(&identity)
	}

	fn connection(&mut self, controller: String, kind: AcpiConnectionKind, value: u32, connection: u64) -> Result<(), Error> {
		self.service.connection(controller, kind, value, connection)
	}
}

struct NodeView<'a> {
	service: &'a mut Service,
	at: usize,
}

impl acpi_node::Service for NodeView<'_> {
	fn path(&mut self) -> Result<String, Error> {
		Ok(self.service.channels[self.at].identity.clone())
	}

	fn evaluate(&mut self, name: String, arguments: Vec<u8>) -> Result<Vec<u8>, Error> {
		self.service.node_evaluate(self.at, &name, &arguments)
	}

	fn properties(&mut self) -> Result<Vec<u8>, Error> {
		self.service.node_properties(self.at)
	}

	fn dsm(&mut self, uuid: Vec<u8>, revision: u64, function: u64, arguments: Vec<u8>) -> Result<Vec<u8>, Error> {
		self.service.node_dsm(self.at, &uuid, revision, function, &arguments)
	}

	fn notifications(&mut self) -> Vec<AcpiNotification> {
		Vec::new()
	}
}

// ---------------------------------------------------------------------------------------------- the start

// THE FADT'S FACTS this host routes by, and the tables to load: the DSDT, then every SSDT in order.
fn fixed(privilege: u64) -> Fixed {
	let Some(bytes) = table(privilege, b"FACP", 0) else { return Fixed::default() };
	let Ok(fadt) = acpi::Fadt::new(&bytes) else { return Fixed::default() };
	Fixed { smi_command: fadt.smi_command().and_then(|port| u16::try_from(port).ok()), pm_timer: fadt.timer().and_then(|timer| timer.block.io_port()), facs: fadt.facs() }
}

fn load_tables(aml: &mut Aml, host: &mut Firmware) -> usize {
	let mut loaded = 0;
	match table(host.privilege, b"DSDT", 0) {
		Some(dsdt) => match aml.load(&dsdt, host) {
			Ok(_) => loaded += 1,
			Err(error) => say(&format!("the DSDT did not load - {error:?}")),
		},
		None => say("the machine hands over no DSDT"),
	}
	for instance in 0..64 {
		let Some(ssdt) = table(host.privilege, b"SSDT", instance) else { break };
		match aml.load(&ssdt, host) {
			Ok(_) => loaded += 1,
			Err(error) => say(&format!("SSDT {instance} did not load - {error:?}")),
		}
	}
	for warning in core::mem::take(&mut aml.warnings) {
		say(&format!("loading: {warning}"));
	}
	loaded
}

// THE EMBEDDED CONTROLLER FROM THE ECDT, for accesses before the namespace is walked: its two ports as `PortRange`s,
// its GPE bit and its node.
fn ec_from_ecdt(service: &mut Service) {
	let Some(ecdt) = table(service.host.privilege, b"ECDT", 0) else { return };
	if ecdt.len() < 66 {
		return;
	}
	let port_of = |at: usize| -> Option<u16> { (ecdt[at] == 1).then(|| u64::from_le_bytes(ecdt[at + 4..at + 12].try_into().unwrap_or([0; 8]))).and_then(|port| u16::try_from(port).ok()) };
	let (Some(command), Some(data)) = (port_of(36), port_of(48)) else { return };
	let id: String = ecdt[65..].iter().take_while(|byte| **byte != 0).map(|byte| *byte as char).collect();
	start_ec(service, command, data, Some(ecdt[64] as u16), service.aml.lookup(&id));
}

fn start_ec(service: &mut Service, command: u16, data: u16, gpe_bit: Option<u16>, node: Option<NodeId>) {
	for port in [data, command] {
		let range = unsafe { syscall(SYS_PORT_RANGE_FIRMWARE, service.host.privilege, port as u64, 1, 0) } as i64;
		if range <= 0 || port_range_map(range as u64) < 0 {
			say(&format!("the embedded controller's port {port:#06x} was not granted - {}", errno_text(range)));
			return;
		}
		service.host.ports.push(Ports { base: port, len: 1 });
	}
	service.host.ec = Some(aml::ec::Ec::new(EcPorts { data, command }));
	service.ec_gpe = gpe_bit;
	service.ec_node = node;
	// `_GLK`: the firmware shares the controller with SMM code, and every access holds the global lock.
	service.host.ec_glk = node.and_then(|node| service.integer(node, b"_GLK")).is_some_and(|glk| glk != 0);
	for (path, error) in service.aml.run_reg(Space::EmbeddedControl, true, &mut service.host) {
		say(&format!("{path}'s _REG for the embedded controller failed - {error:?}"));
	}
	say(&format!("the embedded controller answers at data {data:#06x}, command {command:#06x}{}", gpe_bit.map(|bit| format!(", event {bit:#04x}")).unwrap_or_default()));
}

// THE EMBEDDED CONTROLLER FROM ITS `PNP0C09` NODE, when no ECDT named it: its `_CRS`'s two ports and its `_GPE`.
fn ec_from_namespace(service: &mut Service) {
	if service.host.ec.is_some() {
		return;
	}
	let Some(ec) = service.published.iter().find(|published| published.role == Role::EmbeddedController).map(|published| published.node) else { return };
	let ports: Vec<u16> = service
		.aml
		.resources(ec, &mut service.host)
		.unwrap_or_default()
		.iter()
		.filter_map(|resource| match resource {
			aml::resource::Resource::Io { base, .. } => u16::try_from(*base).ok(),
			_ => None,
		})
		.collect();
	if ports.len() < 2 {
		say("the embedded controller's _CRS names fewer than two ports - it is not driven");
		return;
	}
	let gpe_bit = service.integer(ec, b"_GPE").map(|bit| bit as u16);
	start_ec(service, ports[1], ports[0], gpe_bit, Some(ec));
}

// EVERY GENERAL-PURPOSE EVENT `\_GPE` ANSWERS, enabled; and the embedded controller's.
fn enable_events(service: &mut Service) {
	let privilege = service.host.privilege;
	let count = gpe(privilege, GPE_COUNT, 0);
	if count <= 0 {
		say("this machine has no general-purpose events");
		return;
	}
	let names: Vec<[u8; 4]> = match service.aml.lookup("\\_GPE") {
		Some(scope) => service.aml.ns.children(scope).into_iter().filter_map(|child| service.aml.ns.path(child).last().map(|seg| seg.0)).collect(),
		None => Vec::new(),
	};
	let mut enabled = Vec::new();
	for (number, _) in events::handled(&names).into_iter().chain(service.ec_gpe.map(|bit| (bit, Trigger::Edge))) {
		if gpe(privilege, GPE_ENABLE, number) == 0 {
			enabled.push(format!("{number:#04x}"));
		} else {
			say(&format!("general-purpose event {number:#04x} could not be enabled"));
		}
	}
	say(&format!("{count} general-purpose event(s); enabled: {}", if enabled.is_empty() { String::from("none") } else { enabled.join(" ") }));
}

// THE SLEEP TYPES THIS FIRMWARE DESCRIBES, registered with the kernel: `\_S3`, `\_S4` and `\_S5`, each a package whose
// first two integers are SLP_TYPa and SLP_TYPb. A state the namespace does not describe is not registered, and the
// kernel refuses to enter it.
fn register_sleep_types(service: &mut Service) {
	let mut registered: Vec<&str> = Vec::new();
	for (path, state, name) in [("\\_S3_", SLEEP_STATE_RAM, "S3"), ("\\_S4_", SLEEP_STATE_DISK, "S4"), ("\\_S5_", SLEEP_STATE_SOFT_OFF, "S5")] {
		let Some(node) = service.aml.lookup(path) else { continue };
		let Some(Object::Package(elements)) = service.evaluate(node, &[], "the sleep type") else {
			say(&format!("{path} is not a package - {name} is not registered"));
			continue;
		};
		let integer = |at: usize| elements.get(at).and_then(|element| if let Object::Integer(value) = &*element.borrow() { Some(*value) } else { None });
		let (Some(a), b) = (integer(0), integer(1)) else {
			say(&format!("{path} names no sleep type - {name} is not registered"));
			continue;
		};
		let answer = unsafe { syscall(SYS_FIRMWARE_SLEEP_TYPE, service.host.privilege, state, a & 7, b.unwrap_or(a) & 7) } as i64;
		if answer == 0 {
			registered.push(name);
		} else {
			say(&format!("the kernel refused {name}'s sleep type - {}", errno_text(answer)));
		}
	}
	say(&format!("sleep types registered: {}", if registered.is_empty() { String::from("none") } else { registered.join(" ") }));
}

// `_SST`'S VALUES: working, waking, sleeping, sleeping with its context saved.
const SST_WORKING: u64 = 1;
const SST_WAKING: u64 = 2;
const SST_SLEEPING: u64 = 3;
const SST_HIBERNATING: u64 = 4;

impl Service {
	// A METHOD BY ITS ABSOLUTE PATH, where the namespace has it: `_PTS`, `_WAK`, `\_SI._SST`. One it does not have is
	// no failure - most machines have no `\_SI`.
	fn root_method(&mut self, path: &str, argument: u64) {
		if let Some(node) = self.aml.lookup(path) {
			let _ = self.evaluate(node, &[Object::Integer(argument)], "the sleep method");
		}
	}

	// ONE WAKE NODE ARMED: `_PRW`'s event - an index into the FADT's GPE blocks; an event on a GPE block device is not
	// one this service can arm - then `_DSW` (or `_PSW`, where there is no `_DSW`), then the event set for wake.
	fn arm_wake(&mut self, identity: &str, target: u64) {
		let Some(node) = self.node_of_identity(identity) else {
			say(&format!("{identity} is not in the namespace - its wake is not armed"));
			return;
		};
		let prw = match self.aml.child_value(node, b"_PRW", &mut self.host) {
			Ok(Some(Object::Package(elements))) => elements,
			_ => {
				say(&format!("{identity} has no _PRW - it cannot wake the machine"));
				return;
			}
		};
		let number = prw.first().and_then(|element| if let Object::Integer(value) = &*element.borrow() { Some(*value) } else { None });
		let Some(number) = number else {
			say(&format!("{identity}'s _PRW names an event on a GPE block device - its wake is not armed"));
			return;
		};
		let deepest = prw.get(1).and_then(|element| if let Object::Integer(value) = &*element.borrow() { Some(*value) } else { None }).unwrap_or(0);
		if deepest < target {
			say(&format!("{identity} wakes from S{deepest} at the deepest - not from S{target}; its wake is not armed"));
			return;
		}
		if !self.run(node, b"_DSW", &[Object::Integer(1), Object::Integer(target), Object::Integer(0)]) {
			self.run(node, b"_PSW", &[Object::Integer(1)]);
		}
		let number = number as u16;
		if gpe(self.host.privilege, GPE_WAKE_SET, number) == 0 {
			self.wake_armed.push(number);
			say(&format!("{identity} armed to wake the machine on general-purpose event {number:#04x}"));
		} else {
			say(&format!("general-purpose event {number:#04x} could not be set for {identity}'s wake"));
		}
	}
}

// THE PLATFORM'S STEP - see the head of this file. `_PTS` for a sleep the firmware enters; suspend to idle is no
// firmware transition, so it runs `_SST` alone.
impl platform_sleep::Service for Service {
	fn prepare(&mut self, state: SleepState, wake_nodes: Vec<String>) -> Result<(), Error> {
		let target: u64 = match state {
			SleepState::Idle => 0,
			SleepState::Ram => 3,
			SleepState::Disk => 4,
		};
		self.wake_armed.clear();
		for identity in &wake_nodes {
			self.arm_wake(identity, target);
		}
		if target != 0 {
			self.root_method("\\_PTS", target);
		}
		self.root_method("\\_SI_._SST", if state == SleepState::Disk { SST_HIBERNATING } else { SST_SLEEPING });
		say(&format!(
			"the platform is prepared for {}",
			match state {
				SleepState::Idle => "suspend to idle",
				SleepState::Ram => "S3",
				SleepState::Disk => "S4",
			}
		));
		Ok(())
	}

	fn wake(&mut self, state: SleepState) -> Result<(), Error> {
		self.root_method("\\_SI_._SST", SST_WAKING);
		match state {
			SleepState::Idle => {}
			SleepState::Ram => self.root_method("\\_WAK", 3),
			SleepState::Disk => self.root_method("\\_WAK", 4),
		}
		for number in core::mem::take(&mut self.wake_armed) {
			let _ = gpe(self.host.privilege, GPE_WAKE_CLEAR, number);
		}
		self.root_method("\\_SI_._SST", SST_WORKING);
		say("the platform is awake again");
		Ok(())
	}
}

// THE SLEEP NOTICE: nothing is asked of this service at the announcement or the resume; its step is `platform-sleep`.
impl sleep_notice::Service for Service {
	fn announce(&mut self, _state: SleepState) -> Result<(), Error> {
		Ok(())
	}

	fn hold_writes(&mut self) -> Result<(), Error> {
		Ok(())
	}

	fn release_writes(&mut self) -> Result<(), Error> {
		Ok(())
	}

	fn resumed(&mut self, _state: SleepState) -> Result<(), Error> {
		Ok(())
	}
}

// THE GLOBAL LOCK'S DWORD, in the FACS mapped for this instance.
fn map_facs(service: &mut Service) {
	let Some(facs) = service.host.fixed.facs else { return };
	service.host.current = Some((String::from("acpi:\\_GL_"), Space::SystemMemory, facs, 64));
	match service.host.memory(facs + 16, 4) {
		Ok(at) => service.host.facs_lock = Some(at),
		Err(_) => say("the FACS could not be mapped - the global lock is taken without the firmware"),
	}
	service.host.current = None;
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut roles: [u64; BOOTSTRAP_ROLES.len()] = [0; BOOTSTRAP_ROLES.len()];
	if let Err(error) = receive_roles(bootstrap, &BOOTSTRAP_ROLES, &mut roles) {
		fail_bootstrap(bootstrap, error.tag(), error.reason());
	}
	let (privilege, admin_root) = (roles[0], roles[1]);
	let mut service = Service { aml: Aml::new(Limits::default()), host: Firmware { privilege, fixed: Fixed::default(), current: None, regions: Vec::new(), ports: Vec::new(), companions: Vec::new(), ec: None, ec_glk: false, held: Vec::new(), notifications: Vec::new(), facs_lock: None, refused: Vec::new() }, instance: 0, events: 0, admin_root, admins: Vec::new(), channels: Vec::new(), published: Vec::new(), ec_gpe: None, ec_node: None, wake_armed: Vec::new() };
	if privilege == 0 {
		say("no FirmwareInterpreter privilege was handed over - the namespace is not loaded");
		send_blocking(bootstrap, b"AcpiService: online - no firmware privilege", 0);
		serve(&mut service, bootstrap);
	}
	// THE INSTANCE ATTACHES FIRST: every report comes from the running instance, and its events arrive on this channel.
	let Some((events, theirs)) = channel() else { fail_bootstrap(bootstrap, b"FIRMWARE", b"no channel for the firmware's events") };
	let instance = unsafe { syscall(SYS_FIRMWARE_EVENTS, privilege, theirs, 0, 0) } as i64;
	close(theirs);
	if instance <= 0 {
		fail_bootstrap(bootstrap, b"FIRMWARE", b"the kernel refused to attach this instance");
	}
	service.instance = instance as u64;
	service.events = events;
	if table(privilege, b"FACP", 0).is_none() {
		say("this machine describes itself with no ACPI - nothing to interpret");
		let mut out = [0u8; 16];
		if let Some(len) = report::encode_loaded(service.instance, &mut out) {
			service.report(&out[..len], "the namespace");
		}
		send_blocking(bootstrap, b"AcpiService: online - no ACPI on this machine", 0);
		serve(&mut service, bootstrap);
	}
	service.host.fixed = fixed(privilege);
	let loaded = load_tables(&mut service.aml, &mut service.host);
	// `_REG` FOR THE SPACES THIS SERVICE SERVES from the start; the embedded controller's once its transport exists.
	for space in [Space::SystemMemory, Space::SystemIo, Space::PciConfig, Space::SystemCmos] {
		for (path, error) in service.aml.run_reg(space, true, &mut service.host) {
			say(&format!("{path}'s _REG for {} failed - {error:?}", space.name()));
		}
	}
	map_facs(&mut service);
	ec_from_ecdt(&mut service);
	// THE PLATFORM-WIDE HANDSHAKE, before the walk runs `_INI`.
	if let Some(sb) = service.aml.lookup("\\_SB_") {
		match service.aml.osc(sb, handshake::PLATFORM_WIDE, 1, &handshake::platform_request(), &mut service.host) {
			Ok(answer) => say(&format!("\\_SB._OSC agrees to {:#x} of {:#x}", handshake::platform_granted(answer.as_deref()), handshake::PLATFORM_SUPPORT)),
			Err(error) => say(&format!("\\_SB._OSC failed - {error:?}")),
		}
	}
	service.pre_companions();
	let walk = service.aml.initialize(&mut service.host);
	for (path, error) in &walk.failures {
		say(&format!("{path} did not initialise - {error:?}"));
	}
	service.deliver_notifications();
	let reported = service.publish(walk.found, true);
	ec_from_namespace(&mut service);
	// THE WALK IS PUBLISHED: the kernel withdraws what it did not report again, then tells DeviceManager.
	let mut out = [0u8; 16];
	if let Some(len) = report::encode_loaded(service.instance, &mut out) {
		service.report(&out[..len], "the namespace");
	}
	enable_events(&mut service);
	register_sleep_types(&mut service);
	let online = format!("AcpiService: online - instance {}, {loaded} table(s), {} node(s) reported", service.instance, reported.len());
	print(online.as_bytes());
	print(b"\n");
	send_blocking(bootstrap, online.as_bytes(), 0);
	serve(&mut service, bootstrap)
}

// THE LOOP: the kernel's events, the admin root and its connections, every node channel, every event line, and the
// control channel.
fn serve(service: &mut Service, bootstrap: u64) -> ! {
	let mut buf = vec![0u8; 8192];
	let mut reply = vec![0u8; 8192];
	let mut control = bootstrap;
	loop {
		let mut waitset: Vec<u64> = Vec::new();
		for handle in [control, service.events, service.admin_root] {
			if handle != 0 {
				waitset.push(handle);
			}
		}
		waitset.extend(service.admins.iter().copied());
		waitset.extend(service.channels.iter().map(|channel| channel.chan));
		waitset.extend(service.host.held.iter().filter(|held| held.events != 0).map(|held| held.events));
		waitset.truncate(MAX_WAIT_HANDLES);
		if waitset.is_empty() {
			exit();
		}
		let ready = wait_any(&waitset, 0);
		if ready < 0 {
			continue;
		}
		let handle = waitset[ready as usize];
		if handle == service.events {
			loop {
				match try_recv_caps(service.events, &mut buf) {
					PolledCaps::Message { len, handles } => {
						for &leftover in handles.as_slice() {
							close(leftover);
						}
						if len >= 3 {
							let number = u16::from_le_bytes([buf[1], buf[2]]);
							match buf[0] {
								FIRMWARE_EVENT_GPE => service.general_purpose_event(number),
								FIRMWARE_EVENT_STORM => say(&format!("general-purpose event {number:#04x} stormed and stays disabled")),
								FIRMWARE_EVENT_GLOBAL_LOCK => {}
								_ => {}
							}
						}
					}
					PolledCaps::Empty => break,
					PolledCaps::Closed => {
						close(service.events);
						service.events = 0;
						break;
					}
				}
			}
			continue;
		}
		if let Some(at) = service.host.held.iter().position(|held| held.events == handle) {
			service.gpio_event(at);
			continue;
		}
		// THE CONTROL CHANNEL: ServiceManager's sleep notice and the platform's step.
		if handle == control {
			match try_recv_caps(control, &mut buf) {
				PolledCaps::Message { len, mut handles } => {
					let op = if len >= 2 { u16::from_le_bytes([buf[0], buf[1]]) } else { 0 };
					let mut reply_handles = Handles::new();
					let written = if matches!(op, platform_sleep::OP_PREPARE | platform_sleep::OP_WAKE) { platform_sleep::dispatch(service, &buf[..len], &mut handles, &mut reply, &mut reply_handles) } else { sleep_notice::dispatch(service, &buf[..len], &mut handles, &mut reply, &mut reply_handles) };
					for &leftover in handles.as_slice().iter().chain(reply_handles.as_slice()) {
						close(leftover);
					}
					if let Some(written) = written {
						let _ = try_send(control, &reply[..written], 0);
					}
				}
				PolledCaps::Empty => {}
				PolledCaps::Closed => control = 0,
			}
			continue;
		}
		if let Some(at) = service.channels.iter().position(|channel| channel.chan == handle) {
			serve_node(service, at, &mut buf, &mut reply);
			continue;
		}
		serve_admin(service, handle, &mut buf, &mut reply);
	}
}

fn serve_node(service: &mut Service, at: usize, buf: &mut [u8], reply: &mut [u8]) {
	let chan = service.channels[at].chan;
	let (len, mut handles) = match try_recv_caps(chan, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return,
		PolledCaps::Closed => {
			let gone = service.channels.remove(at);
			close(gone.chan);
			if gone.stream != 0 {
				close(gone.stream);
			}
			return;
		}
	};
	if len >= 2 && u16::from_le_bytes([buf[0], buf[1]]) == acpi_node::OP_NOTIFICATIONS {
		let request = buf[..len].to_vec();
		let mut view = NodeView { service, at };
		let Some((corr, _)) = acpi_node::notifications_open(&mut view, &request, &mut handles) else { return };
		let Some((producer, consumer)) = channel_with_depth(NOTIFY_DEPTH as u64) else { return };
		let channel = &mut service.channels[at];
		if channel.stream != 0 {
			close(channel.stream);
		}
		channel.stream = producer;
		send_caps_blocking(chan, &corr.to_le_bytes(), &[consumer]);
		return;
	}
	let mut reply_handles = Handles::new();
	let written = acpi_node::dispatch(&mut NodeView { service, at }, &buf[..len], &mut handles, reply, &mut reply_handles);
	for &leftover in handles.as_slice() {
		close(leftover);
	}
	match written {
		Some(written) => {
			send_caps_blocking(chan, &reply[..written], reply_handles.as_slice());
		}
		None => {
			let gone = service.channels.remove(at);
			close(gone.chan);
			if gone.stream != 0 {
				close(gone.stream);
			}
		}
	}
}

fn serve_admin(service: &mut Service, handle: u64, buf: &mut [u8], reply: &mut [u8]) {
	let is_root = handle == service.admin_root;
	if !is_root && !service.admins.contains(&handle) {
		return;
	}
	let (len, mut handles) = match try_recv_caps(handle, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return,
		PolledCaps::Closed => {
			close(handle);
			if is_root {
				service.admin_root = 0;
			} else {
				service.admins.retain(|&admin| admin != handle);
			}
			return;
		}
	};
	if len >= 2 {
		let op = u16::from_le_bytes([buf[0], buf[1]]);
		if op == HEARTBEAT_OP {
			send_blocking(handle, b"PONG", 0);
			return;
		}
		if op == CONNECT_OP {
			if service.admins.len() >= MAX_ADMINS {
				send_blocking(handle, &[], 0);
				return;
			}
			match channel() {
				Some((mine, theirs)) => {
					service.admins.push(mine);
					send_blocking(handle, &[], theirs);
				}
				None => {
					send_blocking(handle, &[], 0);
				}
			}
			return;
		}
	}
	if is_root {
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		return;
	}
	let mut reply_handles = Handles::new();
	let written = acpi_admin::dispatch(&mut AdminView { service }, &buf[..len], &mut handles, reply, &mut reply_handles);
	for &leftover in handles.as_slice() {
		close(leftover);
	}
	match written {
		Some(written) => {
			if !send_caps_blocking(handle, &reply[..written], reply_handles.as_slice()) {
				for &leftover in reply_handles.as_slice() {
					close(leftover);
				}
			}
		}
		None => {
			service.admins.retain(|&admin| admin != handle);
			close(handle);
		}
	}
}
