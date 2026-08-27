// Shared logic for the userspace virtio drivers.
//
// DeviceManager launches one driver process per device and hands it, over its
// bootstrap channel, a "DEVICE" message carrying the device's DeviceInfo (its MMIO
// struct offsets) and a transferred DeviceMemory capability to its MMIO BAR. The
// driver maps the BAR, brings the device up through the shared virtio transport
// (negotiation + a ready virtqueue), does its device-specific I/O over the queue,
// reports in, and then stands holding its device. This is the isolated,
// capability-scoped shell each driver runs inside.

use driver_protocol as proto;
use rt::*;

use crate::virtio::{self, Virtio};

// ------------------------------------------------------- the bring-up handshake
//
// ONE HANDSHAKE FOR EVERY DRIVER, versioned, with the binding's generation on everything either
// side says. What stood here was a sequence of named byte strings - `"DEVICE"`, `"IRQ"`, `"KEYS"`,
// `"SYSPOWER"`, `"CONSOLE"` - each matched by a `&buf[..6] == b"DEVICE"` at its own call site, in an
// order every driver had to know without being told, with no version, no generation, and no way to
// say "this went wrong" other than exiting.

// What `BIND` said: the device, which binding of it this is, and how many resources follow.
pub struct Bind {
	pub info: DeviceInfo,
	// P02M0098's claim generation. It goes on every frame this driver sends back, so a message from
	// a process the manager has already replaced cannot be mistaken for the replacement's.
	pub generation: u64,
	pub resource_count: u16,
}

// The capabilities a bring-up received, by KIND rather than by arrival order.
//
// Which of these a given driver gets depends on the driver, so the sequence has no length a driver
// could infer - which is exactly why `BIND` states the count and each `RESOURCE` states its kind.
// Without both, a driver either waits forever for a resource it will never be sent or starts before
// one it needs has arrived.
#[derive(Default)]
pub struct Resources {
	pub device: u64,
	pub irq: u64,
	pub keys: u64,
	pub syspower: u64,
	pub console: u64,
}

// Read one frame. Answers with the header, the payload length, and every capability it carried.
//
// THROUGH THE CAPABILITY-AWARE RECEIVE. The ordinary receive takes the first and drops the rest, so
// a reader that expected one and used it would silently destroy whatever was attached beyond it -
// capabilities gone, nobody told, on the path this protocol calls hostile input.
unsafe fn read_frame(channel: u64, buf: &mut [u8]) -> Option<(proto::Header, wire::Handles)> {
	unsafe {
		let ReceivedCaps::Message { len, handles } = recv_caps_blocking(channel, buf) else {
			return None;
		};
		let Ok(header) = proto::Header::decode(&buf[..len]) else {
			// A REFUSED FRAME LEAVES NO CAPABILITY BEHIND, whichever way it was malformed - and
			// "behind" includes the ones the reader never looked at.
			close_all(&handles);
			return None;
		};
		if header.check_handles(handles.as_slice().len()).is_err() {
			close_all(&handles);
			return None;
		}
		Some((header, handles))
	}
}

unsafe fn close_all(handles: &wire::Handles) {
	unsafe {
		for &handle in handles.as_slice() {
			close(handle);
		}
	}
}

// Send one frame carrying no capability.
unsafe fn send_frame(channel: u64, opcode: proto::Opcode, generation: u64, payload: &[u8]) -> bool {
	unsafe {
		let mut frame = [0u8; proto::HEADER_LEN + proto::MAX_PAYLOAD];
		let header = proto::Header { version: proto::VERSION, opcode, generation, payload_len: payload.len() as u32 };
		frame[..proto::HEADER_LEN].copy_from_slice(&header.encode());
		frame[proto::HEADER_LEN..proto::HEADER_LEN + payload.len()].copy_from_slice(payload);
		send_blocking(channel, &frame[..proto::HEADER_LEN + payload.len()], 0)
	}
}

// The same, moving one capability with it.
unsafe fn send_frame_with(channel: u64, opcode: proto::Opcode, generation: u64, payload: &[u8], handle: u64) -> bool {
	unsafe {
		let mut frame = [0u8; proto::HEADER_LEN + proto::MAX_PAYLOAD];
		let header = proto::Header { version: proto::VERSION, opcode, generation, payload_len: payload.len() as u32 };
		frame[..proto::HEADER_LEN].copy_from_slice(&header.encode());
		frame[proto::HEADER_LEN..proto::HEADER_LEN + payload.len()].copy_from_slice(payload);
		send_blocking(channel, &frame[..proto::HEADER_LEN + payload.len()], handle)
	}
}

// THE WHOLE OF THE MANAGER-TO-DRIVER HALF: one `BIND`, then exactly the resources it promised.
//
// Exits the process on anything that is not that. A driver with no working device has nothing to do,
// and a driver that has been sent something it cannot parse has no way to ask again.
pub unsafe fn handshake(bootstrap: u64) -> (Bind, Resources) {
	unsafe {
		// THE NOTE THIS BINARY CARRIES, READ BACK OUT OF ITSELF.
		//
		// Two things at once, and both matter. It REFERENCES the note's static, which is what keeps
		// its object file linked in at all - a note nothing mentions is a note the linker never
		// pulls out of the rlib, and it would then be missing from every driver with no error
		// anywhere. And it checks what was actually emitted against what this build speaks: a
		// binary whose note disagrees with what it puts on the wire is the one case that would make
		// the manager's pre-claim version check a check of nothing.
		if proto::declared_version() != proto::VERSION {
			exit();
		}
		let mut buf = [0u8; proto::HEADER_LEN + proto::MAX_PAYLOAD];
		let Some((header, handles)) = read_frame(bootstrap, &mut buf) else { exit() };
		if header.opcode != proto::Opcode::Bind {
			close_all(&handles);
			exit();
		}
		let Ok((info, resource_count)) = proto::decode_bind(header.payload(&buf)) else { exit() };
		let generation = header.generation;
		let mut resources = Resources::default();
		// EXACTLY THE PROMISED NUMBER. A count the manager states and does not keep would leave this
		// loop waiting for a frame that is not coming, which is the deadlock the count exists to
		// remove - so the loop is bounded by what was promised and by nothing else.
		for _ in 0..resource_count {
			let Some((header, handles)) = read_frame(bootstrap, &mut buf) else { exit() };
			if header.opcode != proto::Opcode::Resource || header.generation != generation {
				close_all(&handles);
				exit();
			}
			let Ok(kind) = proto::decode_resource(header.payload(&buf)) else {
				close_all(&handles);
				exit();
			};
			let handle = handles.as_slice()[0];
			let slot = match kind {
				proto::ResourceKind::Device => &mut resources.device,
				proto::ResourceKind::Irq => &mut resources.irq,
				proto::ResourceKind::Keys => &mut resources.keys,
				proto::ResourceKind::SysPower => &mut resources.syspower,
				proto::ResourceKind::Console => &mut resources.console,
			};
			// A SECOND RESOURCE OF ONE KIND IS NOT A SPARE. Overwriting the slot would leak the
			// first capability silently; this keeps the first and closes the second, which is the
			// same rule a refused frame follows.
			if *slot != 0 {
				close(handle);
			} else {
				*slot = handle;
			}
		}
		(Bind { info, generation, resource_count }, resources)
	}
}

// Offer a provider this driver serves. HELD UNPUBLISHED by the manager until `ready`, and closed on
// `failed` - so a driver that dies half way through announcing itself announces nothing.
pub unsafe fn offer(bootstrap: u64, bind: &Bind, provider_kind: u16, handle: u64) -> bool {
	let mut payload = [0u8; proto::U16_PAYLOAD_LEN];
	proto::encode_u16(provider_kind, &mut payload);
	unsafe { send_frame_with(bootstrap, proto::Opcode::Offer, bind.generation, &payload, handle) }
}

// "I am up." Terminal: nothing this driver sends afterwards is part of the handshake.
pub unsafe fn ready(bootstrap: u64, bind: &Bind) -> bool {
	unsafe { send_frame(bootstrap, proto::Opcode::Ready, bind.generation, &[]) }
}

// "I am not up, and here is what I know about why."
//
// A DRIVER'S OWN VOCABULARY, not the manager's. A driver is hostile input by this protocol's rule,
// and letting it name the manager's causes would let it declare things only the manager can
// determine - which would then be recorded as fact.
pub unsafe fn failed(bootstrap: u64, bind: &Bind, code: proto::DriverFailureCode) -> ! {
	let mut payload = [0u8; proto::U16_PAYLOAD_LEN];
	proto::encode_u16(code as u16, &mut payload);
	unsafe {
		send_frame(bootstrap, proto::Opcode::Failed, bind.generation, &payload);
		exit()
	}
}

// Receive the device from DeviceManager, map its MMIO BAR, and negotiate it up to
// FEATURES_OK through the virtio transport. Returns the negotiated device; the
// caller sets up its queues and calls `driver_ok`. Exits the process on any failure
// (a driver with no working device has nothing to do).
pub unsafe fn bringup(bootstrap: u64) -> (Bind, Virtio) {
	unsafe { bringup_features(bootstrap, 0) }
}

// `bringup`, additionally asking the negotiation for the word-0 (device-specific)
// feature bits `want_word0` names; the accepted set is readable off the returned
// device (`features_word0`).
// ANSWERS WITH THE BINDING TOO, because everything this driver says afterwards has to carry the
// generation. A bring-up that returned only the device left the driver with no way to stamp its own
// report, which is what let a message from a replaced process be taken for its replacement's.
pub unsafe fn bringup_features(bootstrap: u64, want_word0: u32) -> (Bind, Virtio) {
	unsafe {
		let (bind, resources) = handshake(bootstrap);
		let device = bringup_bound(bootstrap, &bind, &resources, want_word0);
		(bind, device)
	}
}

// The device half of a bring-up whose handshake has already been read, for the drivers that need
// the other resources too and therefore read the handshake themselves.
//
// A FAILURE HERE IS REPORTED, not merely exited on. A driver that walks away tells the manager only
// that its process is gone, which is indistinguishable from a crash; a `FAILED` carrying a code the
// manager can read retryability off is the difference between a rebind and a permanent refusal.
pub unsafe fn bringup_bound(bootstrap: u64, bind: &Bind, resources: &Resources, want_word0: u32) -> Virtio {
	unsafe {
		if resources.device == 0 {
			failed(bootstrap, bind, proto::DriverFailureCode::ResourceUnusable);
		}
		// map the device's MMIO BAR into our address space.
		let base: u64 = syscall(SYS_DEVICE_MEMORY_MAP, resources.device, 0, 0, 0);
		if sys_is_err(base) {
			failed(bootstrap, bind, proto::DriverFailureCode::ResourceUnusable);
		}
		// reset -> negotiate -> features-ok, and the reset is also what says the frames a previous
		// driver of this device left behind are safe to recycle (see `virtio::negotiate_for`).
		match virtio::negotiate_for(resources.device, base, &bind.info, want_word0) {
			Some(device) => device,
			// The device did not negotiate. RETRYABLE, and said so: the part may yet come up, and
			// this is the case a rebind exists for.
			None => failed(bootstrap, bind, proto::DriverFailureCode::DeviceNotResponding),
		}
	}
}

// Report in over the bootstrap channel, then stand holding the device until
// DeviceManager drops the channel.
// A driver's report, with the device it is about.
//
// FOUR IDENTICAL LINES ARE ONE LINE THE READER CANNOT USE. A machine with four `virtio-blk`
// functions printed `driver.virtio-blk: online` four times, and the information that would have told
// them apart was six lines further down in the kernel's DMA audit, which lists the same four devices
// by address. So the address comes with the report: `driver.virtio-blk: online (00:01.0)`.
//
// `detail` is whatever else the driver has to say about itself - a role, a self-test result - and is
// empty for most. The whole line is built in a fixed buffer because a driver has no formatter.
pub fn describe(out: &mut [u8; 64], name: &[u8], device: &Virtio, detail: &[u8]) -> usize {
	let (bus, dev, func) = device.address();
	let mut n = 0usize;
	push(out, &mut n, b"driver.");
	push(out, &mut n, name);
	push(out, &mut n, b": online (");
	push(out, &mut n, &hex2(bus));
	push(out, &mut n, b":");
	push(out, &mut n, &hex2(dev));
	push(out, &mut n, b".");
	push(out, &mut n, &[b'0' + (func % 10)]);
	if !detail.is_empty() {
		push(out, &mut n, b", ");
		push(out, &mut n, detail);
	}
	push(out, &mut n, b")");
	n
}

// Append what fits and drop what does not: a report that runs off the end of its buffer is a report,
// and a driver that panicked while writing one is a device that never came up.
fn push(out: &mut [u8; 64], at: &mut usize, bytes: &[u8]) {
	for byte in bytes {
		if *at < out.len() {
			out[*at] = *byte;
			*at += 1;
		}
	}
}

fn hex2(byte: u8) -> [u8; 2] {
	const HEX: &[u8; 16] = b"0123456789abcdef";
	[HEX[(byte >> 4) as usize], HEX[(byte & 0xf) as usize]]
}

// Announce this driver is up, and go on serving.
//
// The same two steps `online_and_stand` takes - the offers, then the terminal `READY` - for the
// drivers that have work to do afterwards rather than standing on the channel. The order is the
// property: offers are held UNPUBLISHED by the manager until the terminal frame, so a driver that
// dies between them announces nothing.
pub unsafe fn online(bootstrap: u64, bind: &Bind, report: &[u8], offers: &[(u16, u64)]) -> bool {
	unsafe {
		print(report);
		print(b"\n");
		for &(kind, handle) in offers {
			if handle == 0 {
				continue;
			}
			if !offer(bootstrap, bind, kind, handle) {
				return false;
			}
		}
		ready(bootstrap, bind)
	}
}

// Announce this driver is up and stand holding its device until DeviceManager drops the channel.
//
// THE HUMAN LINE STOPS BEING LOAD-BEARING. It used to BE the report: the manager stored whatever
// handle arrived with it, printed the bytes and called that a driver coming up, so changing a boot
// line's wording could break binding and the boot-report milestone had to chase every assertion
// that parsed one. The line is still printed - by the driver, to the log, where a human reads it -
// and what the manager acts on is the `READY` frame.
//
// `service` is the provider this driver serves, or 0 for one that serves none. It travels in an
// `OFFER` BEFORE the `READY`, because the manager holds offers unpublished until the terminal frame:
// a driver that dies between the two announces nothing.
pub unsafe fn online_and_stand(bootstrap: u64, bind: &Bind, report: &[u8], service: u64, provider_kind: u16) -> ! {
	unsafe {
		print(report);
		print(b"\n");
		if service != 0 && !offer(bootstrap, bind, provider_kind, service) {
			exit();
		}
		if !ready(bootstrap, bind) {
			exit();
		}
		let mut buf: [u8; 16] = [0u8; 16];
		let _ = recv_blocking(bootstrap, &mut buf);
	}
	exit();
}
