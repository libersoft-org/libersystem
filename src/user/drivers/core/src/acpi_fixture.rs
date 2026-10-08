// THE ACPI GATE'S FIXTURE DRIVER, for the development image alone: bound to the fixture SSDT's `LSFX0001` node, the
// firmware's method and `Notify` source.
//
// WHAT IT DOES WITH ITS NODE. It asks DeviceManager for the node channel once online, subscribes to the node's
// `Notify` values, and on every `Notify(LSF1, 0x80)` - which the GPIO controller's `_E02` raises when the harness
// raises line 2 - runs its probes and says what each answered: `RDVL` reads the dword the harness wrote; `WRVL` writes
// the device's own region inside its own `_CRS` range and the claim's window reads the same dword; `OTHR` reaches
// another node's region over that claimed range and is refused; `GPRD` reads line 5 through a `GeneralPurposeIo`
// field and `GPWR` is refused by name; `GSBW` and `GSBR` write and read register 0x10 of the bus fixture's device at
// 0x50; `_DSD` and `_DSM` answer; `_INI` is refused as a platform method. When its node channel closes - the ACPI
// service restarted - it asks again, and subscribes again to what it is answered with.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use drivers::common;
use ipc_client::ChannelTransport;
use proto::system::acpi_node;
use rt::*;
use wire::Handles;

#[path = "acpi_fixture/tree.rs"]
mod tree;

const TICKS: u64 = TICKS_PER_SECOND * 5;
const DSM_UUID: [u8; 16] = uuid(*b"5c3c6b2e8d7a4f5b9a412e1d7f0a6b93");

// `ToUUID`'s layout: the first three fields little-endian.
const fn uuid(hex: [u8; 32]) -> [u8; 16] {
	const fn nibble(byte: u8) -> u8 {
		if byte >= b'a' { byte - b'a' + 10 } else { byte - b'0' }
	}
	let mut raw = [0u8; 16];
	let mut at = 0;
	while at < 16 {
		raw[at] = nibble(hex[2 * at]) << 4 | nibble(hex[2 * at + 1]);
		at += 1;
	}
	[raw[3], raw[2], raw[1], raw[0], raw[5], raw[4], raw[7], raw[6], raw[8], raw[9], raw[10], raw[11], raw[12], raw[13], raw[14], raw[15]]
}

// ONE LINE, WRITTEN WHOLE, so another process's output never lands inside it.
fn say(text: &str) {
	let line = format!("acpi-fixture: {text}\n");
	print(line.as_bytes());
}

// Arguments in the node channel's value encoding: a package of integers.
fn integers(values: &[u64]) -> Vec<u8> {
	let mut out = alloc::vec![0x04];
	out.extend_from_slice(&(values.len() as u32).to_le_bytes());
	for value in values {
		out.push(0x01);
		out.extend_from_slice(&value.to_le_bytes());
	}
	out
}

// An answer in the value encoding, as an integer where it is one.
fn integer(bytes: &[u8]) -> Option<u64> {
	(bytes.len() == 9 && bytes[0] == 0x01).then(|| u64::from_le_bytes(bytes[1..9].try_into().unwrap_or([0; 8])))
}

struct Probe {
	node: u64,
	window: u64,
}

impl Probe {
	fn client(&self) -> acpi_node::Client<ChannelTransport> {
		acpi_node::Client::with_deadline(ChannelTransport { chan: self.node }, clock() + TICKS)
	}

	fn evaluate(&self, name: &str, args: &[u64]) -> Result<Vec<u8>, String> {
		let arguments = if args.is_empty() { Vec::new() } else { integers(args) };
		match self.client().evaluate(name, &arguments) {
			Some(Ok(bytes)) => Ok(bytes),
			Some(Err(error)) => Err(format!("{error:?}")),
			None => Err(String::from("no answer")),
		}
	}

	fn run(&self, round: u32) {
		match self.evaluate("RDVL", &[]).map(|bytes| integer(&bytes)) {
			Ok(Some(value)) => say(&format!("round {round}: RDVL answered {value:#x}")),
			other => say(&format!("round {round}: RDVL did not answer an integer - {other:?}")),
		}
		let written = 0x5A5A_0000 | round as u64;
		match self.evaluate("WRVL", &[written]).map(|bytes| integer(&bytes)) {
			Ok(Some(value)) => {
				// SAFETY: the claim's own window, mapped by this driver; its first dword is the region `WRVL` wrote.
				let read = if self.window != 0 { (unsafe { core::ptr::read_volatile(self.window as *const u32) }) as u64 } else { 0 };
				say(&format!("round {round}: WRVL wrote {value:#x} and the claim's window reads {read:#x}"));
			}
			other => say(&format!("round {round}: WRVL failed - {other:?}")),
		}
		match self.evaluate("OTHR", &[]) {
			Err(why) => say(&format!("round {round}: another node's region over the claimed range was refused ({why})")),
			Ok(bytes) => say(&format!("round {round}: another node's region over the claimed range was NOT refused - {bytes:x?}")),
		}
		match self.evaluate("GPRD", &[]).map(|bytes| integer(&bytes)) {
			Ok(Some(level)) => say(&format!("round {round}: GPRD read line 5 as {level}")),
			other => say(&format!("round {round}: GPRD failed - {other:?}")),
		}
		match self.evaluate("GPWR", &[]) {
			Err(why) => say(&format!("round {round}: a GeneralPurposeIo write was refused ({why})")),
			Ok(_) => say(&format!("round {round}: a GeneralPurposeIo write was NOT refused")),
		}
		let byte = 0x60 + (round as u64 & 0x0F);
		let wrote = self.evaluate("GSBW", &[byte]);
		match self.evaluate("GSBR", &[]).map(|bytes| integer(&bytes)) {
			Ok(Some(value)) if wrote.is_ok() => say(&format!("round {round}: GSBW wrote {byte:#04x} and GSBR read {value:#04x}")),
			other => say(&format!("round {round}: the serial-bus field failed - {wrote:?} {other:?}")),
		}
		match self.client().properties() {
			Some(Ok(block)) => say(&format!("round {round}: _DSD answered a {}-byte property block", block.len())),
			other => say(&format!("round {round}: _DSD failed - {other:?}")),
		}
		match self.client().dsm(&DSM_UUID, &1, &1, &integers(&[41])).map(|answer| answer.map(|bytes| integer(&bytes))) {
			Some(Ok(Some(value))) => say(&format!("round {round}: _DSM function 1 answered {value}")),
			other => say(&format!("round {round}: _DSM failed - {other:?}")),
		}
		match self.evaluate("_INI", &[]) {
			Err(why) => say(&format!("round {round}: _INI was refused ({why})")),
			Ok(_) => say(&format!("round {round}: _INI was NOT refused")),
		}
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, resources) = common::handshake(bootstrap);
	if bind.info.platform.source == PLATFORM_SOURCE_TREE {
		tree::serve(bootstrap, &bind, &resources);
	}
	let window = if resources.device != 0 { unsafe { syscall(SYS_DEVICE_MEMORY_MAP, resources.device, 0, 0, 0) } } else { 0 };
	if !common::online(bootstrap, &bind, b"driver.acpi-fixture: online", &[]) {
		exit();
	}
	let _ = common::request_node(bootstrap, &bind);
	let mut probe = Probe { node: 0, window: if (window as i64) > 0 { window } else { 0 } };
	let mut stream: u64 = 0;
	let mut round: u32 = 0;
	let mut buf = [0u8; 256];
	loop {
		let handles: &[u64] = if stream != 0 { core::slice::from_ref(&stream) } else { &[] };
		match common::wait_node_or_answer(bootstrap, &bind, handles) {
			None => {
				if common::stop_requested() {
					common::finish_stop(bootstrap, &bind, resources.device, true);
				}
				exit();
			}
			// THE NODE ANSWERED: a channel to subscribe on, or none.
			Some(at) if at == handles.len() => {
				if stream != 0 {
					close(stream);
					stream = 0;
				}
				probe.node = common::node().unwrap_or(0);
				if probe.node == 0 {
					say("the firmware describes no node for this device");
					continue;
				}
				match probe.client().path() {
					Some(Ok(path)) => say(&format!("its node is {path}")),
					other => say(&format!("its node did not say its path - {other:?}")),
				}
				stream = probe.client().notifications().unwrap_or(0);
				if stream == 0 {
					say("its node gave no notification stream");
				}
			}
			Some(_) => loop {
				match try_recv_caps(stream, &mut buf) {
					PolledCaps::Message { len, handles } => {
						for &leftover in handles.as_slice() {
							close(leftover);
						}
						let mut frame = Handles::new();
						let Some(notification) = acpi_node::notifications_read(&buf[..len], &mut frame) else { continue };
						say(&format!("Notify {:#x} arrived (#{})", notification.value, notification.sequence));
						if notification.value == 0x80 {
							round += 1;
							probe.run(round);
						}
					}
					PolledCaps::Empty => break,
					PolledCaps::Closed => {
						// THE SERVICE ENDED: its channels with it. The next answer to the request - asked again by the
						// control path when the node channel closes - is the new instance's.
						close(stream);
						stream = 0;
						say("its node's stream closed - the ACPI service ended; asking again");
						break;
					}
				}
			},
		}
	}
}
