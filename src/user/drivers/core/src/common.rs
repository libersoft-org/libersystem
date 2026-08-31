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
// `token` is this driver's OWN name for the publication, unique only within this driver. It is what
// a later withdrawal names; the identity the rest of the system uses is the manager's and is never
// something a driver chooses. A driver that publishes one provider of each kind may use the kind as
// its token and lose nothing.
pub unsafe fn offer(bootstrap: u64, bind: &Bind, provider_kind: u16, token: u16, handle: u64) -> bool {
	let mut payload = [0u8; proto::OFFER_PAYLOAD_LEN];
	proto::encode_offer(provider_kind, token, &mut payload);
	unsafe { send_frame_with(bootstrap, proto::Opcode::Offer, bind.generation, &payload, handle) }
}

// "The provider I published under this token is going away."
//
// AFTER the handshake, and not terminal: this driver stays bound and its other publications stay
// published. `token` is the one this driver chose when it offered - `online` uses the offer's
// position in its own list, so the first offer is token 0.
pub unsafe fn withdraw(bootstrap: u64, bind: &Bind, token: u16) -> bool {
	let mut payload = [0u8; proto::U16_PAYLOAD_LEN];
	proto::encode_u16(token, &mut payload);
	unsafe { send_frame(bootstrap, proto::Opcode::Withdraw, bind.generation, &payload) }
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
			Some(device) => {
				// REMEMBERED SO THE STOP PATH CAN REACH IT.
				//
				// A planned stop must reset the device before it certifies that the device is quiet,
				// and the loops that read the stop - `serve_blocks`, `event_loop`, `serve` - are
				// several calls below the one place the `Virtio` exists. Threading it through four
				// signatures would make each driver responsible for remembering; recording it HERE,
				// where every virtio driver already passes, makes it impossible to forget.
				remember_virtio(&device);
				device
			}
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
	describe_state(out, name, device, b"online", detail)
}

// The same line, for a driver that reached its report WITHOUT what it exists to offer.
//
// `describe` hardcoded the word `online`, and the two virtio-blk exits that have no request queue and
// no service channel went through it - so a boot said `driver.virtio-blk: online (00:02.0, no
// channel)`, a success claim contradicted by its own parenthetical four characters later. The state
// is what the line is FOR; it cannot be the one part of it that is always the same.
pub fn describe_state(out: &mut [u8; 64], name: &[u8], device: &Virtio, state: &[u8], detail: &[u8]) -> usize {
	let (bus, dev, func) = device.address();
	let mut n = 0usize;
	push(out, &mut n, b"driver.");
	push(out, &mut n, name);
	push(out, &mut n, b": ");
	push(out, &mut n, state);
	push(out, &mut n, b" (");
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

// ONE WRITE, NOT TWO. `print(report); print(b"\n")` is two syscalls with a gap between them, and the
// kernel writes to the same serial port from its own cores - so a boot report carried
// `driver.virtio-blk: online (00:02.0)iommu: 00:08.0 attached to domain 4` and then a bare newline,
// which is two lines a reader cannot read and neither of them the line either component wrote. The
// gap is what has to go; the terminator belongs to the line.
unsafe fn print_line(report: &[u8]) {
	unsafe {
		let mut out = [0u8; 96];
		let n = report.len().min(out.len() - 1);
		out[..n].copy_from_slice(&report[..n]);
		out[n] = b'\n';
		print(&out[..n + 1]);
	}
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

pub fn hex2(byte: u8) -> [u8; 2] {
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
		print_line(report);
		// THE TOKEN IS THE POSITION IN THIS DRIVER'S OWN OFFER LIST, which is unique within this
		// driver by construction and costs a driver author no thought at all. The kind would do for
		// every driver in the tree today, because none publishes two of one kind - and that is
		// exactly the assumption a token exists to stop being load-bearing.
		for (token, &(kind, handle)) in offers.iter().enumerate() {
			if handle == 0 {
				continue;
			}
			if !offer(bootstrap, bind, kind, token as u16, handle) {
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
pub unsafe fn online_and_stand(bootstrap: u64, bind: &Bind, report: &[u8], service: u64, provider_kind: u16, device: u64) -> ! {
	unsafe {
		print_line(report);
		if service != 0 && !offer(bootstrap, bind, provider_kind, 0, service) {
			exit();
		}
		if !ready(bootstrap, bind) {
			exit();
		}
		stand(bootstrap, bind, device);
	}
}

// Stand holding the device, answering the manager's `PING` until it drops the channel.
//
// A DRIVER THAT ONLY BLOCKS IS INDISTINGUISHABLE FROM A WEDGED ONE. It used to be a single
// `recv_blocking` whose result was discarded: any message ended the driver and no message was ever
// answered, so "is this driver's control path making progress" had no way to be asked. The answer
// echoes the sequence it was asked with, on the same channel and through the same frame codec as
// every other event.
pub unsafe fn stand(bootstrap: u64, bind: &Bind, device: u64) -> ! {
	unsafe {
		let mut buf: [u8; proto::HEADER_LEN + proto::MAX_PAYLOAD] = [0u8; proto::HEADER_LEN + proto::MAX_PAYLOAD];
		loop {
			let Received::Message { len, handle } = recv_blocking(bootstrap, &mut buf) else { exit() };
			let Ok(header) = proto::Header::decode(&buf[..len]) else {
				if handle != 0 {
					close(handle);
				}
				continue;
			};
			// A FRAME FROM A BINDING THAT IS OVER IS NOT THIS BINDING'S. Dropped rather than
			// answered: answering it would tell the manager a generation it has moved on from is
			// alive.
			if header.generation != bind.generation {
				if handle != 0 {
					close(handle);
				}
				continue;
			}
			match header.opcode {
				// A DRIVER THAT STANDS SERVES NOTHING, so there is nowhere to put a second consumer
				// and the endpoint goes back. Closed rather than dropped: a consumer whose endpoint
				// is closed learns its connection ended, where one whose endpoint is merely never
				// read waits for ever.
				proto::Opcode::Connect => {
					if handle != 0 {
						close(handle);
					}
				}
				proto::Opcode::Ping => {
					let Ok(sequence) = proto::decode_sequence(header.payload(&buf)) else { continue };
					if !pong(bootstrap, bind, sequence) {
						exit();
					}
				}
				// A STOP IS ANSWERED, AFTER THE DEVICE IS QUIET. `stand` treated every opcode other
				// than `PING` as terminal and exited, so a driver standing on its channel -
				// `virtio_console` is one - never sent `STOPPED` at all and the manager waited out
				// its forced-teardown deadline for a driver that had done exactly what it was asked.
				//
				// AND THEN IT ANSWERED TOO EARLY. The first correction called `stopped` directly from
				// here, which certifies a clean stop - the kernel gives back DMA frames and masked
				// vectors on the strength of it and cannot check the claim - while the device was
				// still live with its queues programmed. Having no work to DRAIN is not the same as
				// having no hardware to STOP. This goes through `finish_stop` like every other
				// planned-stop path, so the reset happens first and a device that does not confirm
				// gets no certificate.
				proto::Opcode::Stop => {
					STOP_PENDING.store(true, core::sync::atomic::Ordering::Release);
					finish_stop(bootstrap, bind, device, quiesce_virtio());
					exit();
				}
				// Anything else on this channel ends the stand, which is what dropping the channel
				// has always meant.
				_ => exit(),
			}
		}
	}
}

// The same combined wait for a driver that already waits on SEVERAL handles - an interrupt and a
// service channel, typically. Answers the index into `handles` that is ready, or None when the
// manager dropped the channel.
//
// A driver calls this instead of `wait_any` and changes nothing else: the manager's channel joins
// the set it was already waiting on, so the ping is answered by the loop being supervised rather
// than by a second one that would keep answering after the first had stopped working.
pub unsafe fn wait_or_answer(bootstrap: u64, bind: &Bind, handles: &[u64]) -> Option<usize> {
	unsafe {
		let mut set: [u64; 8] = [0; 8];
		let count: usize = handles.len().min(set.len() - 1);
		set[..count].copy_from_slice(&handles[..count]);
		set[count] = bootstrap;
		loop {
			// DRAINED FIRST, for the reason `serve_or_answer` gives: `wait_any` answers with the
			// first ready index, so a handle that is always ready starves everything after it - and
			// what comes after it here is the channel a watchdog is asking on.
			match drain_control(bootstrap, bind) {
				Control::Continue => {}
				Control::Stop => {
					// LATCHED, NOT ANSWERED - see `finish_stop`. The caller unwinds and answers once
					// its own work is finished or abandoned, which is what `STOPPED` certifies.
					STOP_PENDING.store(true, core::sync::atomic::Ordering::Release);
					return None;
				}
				Control::Ended => return None,
			}
			for (at, &handle) in handles[..count].iter().enumerate() {
				if poll_ready(handle) {
					return Some(at);
				}
			}
			if wait_any(&set[..count + 1], 0) < 0 {
				return None;
			}
		}
	}
}

// WAIT FOR WORK OR FOR A PING, AND ANSWER THE PING HERE.
//
// A driver with a service loop of its own cannot stand on `stand`, and a driver parked in
// `recv_blocking(server)` cannot answer anything - so an IDLE driver would look exactly like a
// wedged one, which is the distinction this whole mechanism exists to make. This is the combined
// wait, and it belongs in the driver's own loop: a second loop answering pings would be a driver
// whose watchdog is petted by something other than the path being supervised.
//
// Answers true when `server` has a request to read. False means the manager dropped the channel,
// which ends the driver the way it always has.
pub unsafe fn serve_or_answer(bootstrap: u64, bind: &Bind, server: u64) -> bool {
	unsafe {
		// THE SET IS EPHEMERAL, SO IT MAY NOT ACCEPT - and it used to (corrected 2026-08-30).
		//
		// `Serving::new(server)` lives for this call. `drain_control_into` was handed it and would
		// `accept` a `CONNECT`'s server end into it; this function then returned and the set was
		// dropped, taking the endpoint with it. A consumer that asked the catalogue for a connection
		// held a client end whose server half nobody was ever going to read, and waited for ever -
		// which is worse than being refused, because nothing tells it.
		//
		// So this shape REFUSES a second consumer: `drain_control_into(.., None)` closes an endpoint
		// it cannot place, and a consumer whose endpoint closes learns its connection ended. A driver
		// that means to serve several holds its own `Serving` across calls and uses
		// `serve_any_or_answer`; `virtio_blk` is the one that does.
		let mut one = Serving::new(server);
		serve_any_or_answer_inner(bootstrap, bind, &mut one, false).is_some()
	}
}

// THE SAME, OVER EVERY CONSUMER THIS PROVIDER HAS. Answers WHICH endpoint has work, or `None` when
// the driver is finished.
//
// `serve_or_answer` is this with a set of one, kept for the loops that serve something which is not
// a provider - a driver's own control path - and so a caller that will never see a `CONNECT` does
// not have to hold a set to say so.
pub unsafe fn serve_any_or_answer(bootstrap: u64, bind: &Bind, serving: &mut Serving) -> Option<usize> {
	unsafe { serve_any_or_answer_inner(bootstrap, bind, serving, true) }
}

// The two shapes above, with the one thing that differs between them named: whether a `CONNECT` may
// be ACCEPTED into `serving`. A caller whose set outlives the call may; one whose set is a local may
// not, because accepting into a set that is about to be dropped loses the endpoint.
unsafe fn serve_any_or_answer_inner(bootstrap: u64, bind: &Bind, serving: &mut Serving, accepts: bool) -> Option<usize> {
	unsafe {
		loop {
			// THE MANAGER'S CHANNEL IS DRAINED FIRST, EVERY PASS, and that is not a nicety.
			//
			// `wait_any` answers with the FIRST ready index, so a server with work always waiting on
			// it starves everything after it in the set - and a busy driver would never see a ping,
			// which is precisely the driver a watchdog must not kill. Measured: every virtio-blk in
			// the machine was declared wedged while serving StorageService as fast as it could.
			let placed = if accepts { Some(&mut *serving) } else { None };
			match drain_control_into(bootstrap, bind, placed) {
				Control::Continue => {}
				Control::Stop => {
					STOP_PENDING.store(true, core::sync::atomic::Ordering::Release);
					return None;
				}
				Control::Ended => return None,
			}
			// THE FIRST WITH WORK, and a set that grew while this was parked is waited on next pass.
			for index in 0..serving.as_slice().len() {
				if poll_ready(serving.at(index)) {
					return Some(index);
				}
			}
			// Nothing on any of them: park until one speaks. The manager's channel goes LAST so a
			// consumer with work waiting does not starve it - see the note above about the reverse.
			let mut set: [u64; MAX_PROVIDER_CLIENTS + 1] = [0; MAX_PROVIDER_CLIENTS + 1];
			let live = serving.as_slice();
			set[..live.len()].copy_from_slice(live);
			set[live.len()] = bootstrap;
			if wait_any(&set[..live.len() + 1], 0) < 0 {
				return None;
			}
		}
	}
}

// THE ENDPOINTS THIS DRIVER SERVES ONE PROVIDER ON.
//
// A provider used to be one channel: the driver made a pair, offered the client end, and served the
// server end until that client closed. So the SECOND consumer of a disk or a NIC had nowhere to go -
// the catalogue could show it the provider and had nothing to give it, and handing over the same
// channel would be two consumers competing over one reply queue rather than two connections.
//
// The manager mints each new pair and sends the server end in a `CONNECT` frame; this is where they
// accumulate. A bound, because a driver cannot grow an unbounded set of clients from frames a
// manager sends - and one that is full refuses the endpoint by closing it, which the consumer reads
// as a connection that ended rather than one that never answers.
pub const MAX_PROVIDER_CLIENTS: usize = 8;

pub struct Serving {
	ends: [u64; MAX_PROVIDER_CLIENTS],
	count: usize,
}

impl Serving {
	// The first one, which the driver made itself and offered to the manager.
	pub fn new(first: u64) -> Self {
		let mut ends = [0u64; MAX_PROVIDER_CLIENTS];
		ends[0] = first;
		Self { ends, count: 1 }
	}

	pub fn as_slice(&self) -> &[u64] {
		&self.ends[..self.count]
	}

	pub fn at(&self, index: usize) -> u64 {
		self.ends[index]
	}

	// A consumer's endpoint has closed: drop it and keep the rest. The order of the others does not
	// matter, so the last one fills the hole rather than everything shifting.
	pub fn close_at(&mut self, index: usize) {
		unsafe { close(self.ends[index]) };
		self.count -= 1;
		self.ends[index] = self.ends[self.count];
		self.ends[self.count] = 0;
	}

	// One more, from a `CONNECT`. False when this driver is already serving as many as it will.
	fn accept(&mut self, end: u64) -> bool {
		if self.count >= MAX_PROVIDER_CLIENTS {
			return false;
		}
		self.ends[self.count] = end;
		self.count += 1;
		true
	}
}

// What draining the manager's channel decided.
enum Control {
	// Nothing terminal: pings were answered, if any.
	Continue,
	// The manager asked this driver to stop and mean all of it.
	Stop,
	// The manager dropped the channel or sent something that ends this driver.
	Ended,
}

// Answer every `PING` waiting on `bootstrap` right now, without blocking.
unsafe fn drain_control(bootstrap: u64, bind: &Bind) -> Control {
	unsafe { drain_control_into(bootstrap, bind, None) }
}

// The same, for a loop that serves a provider: a `CONNECT` carries an endpoint and there has to be
// somewhere to put it. `None` is a caller that serves none, and one arriving there is refused with
// its handle closed rather than silently dropped.
unsafe fn drain_control_into(bootstrap: u64, bind: &Bind, mut serving: Option<&mut Serving>) -> Control {
	unsafe {
		let mut buf: [u8; proto::HEADER_LEN + proto::MAX_PAYLOAD] = [0u8; proto::HEADER_LEN + proto::MAX_PAYLOAD];
		loop {
			let (len, handle) = match try_recv(bootstrap, &mut buf) {
				Polled::Message { len, handle } => (len, handle),
				Polled::Empty => return Control::Continue,
				Polled::Closed => return Control::Ended,
			};
			let Ok(header) = proto::Header::decode(&buf[..len]) else {
				if handle != 0 {
					close(handle);
				}
				continue;
			};
			// A frame from a binding that is over is not this binding's: dropped rather than
			// answered, because answering it would tell the manager a generation it has moved on
			// from is alive.
			if header.generation != bind.generation {
				continue;
			}
			match header.opcode {
				// ONE MORE CONSUMER. The manager made the pair and kept the client end; this is the
				// server end, and serving it is the whole of what a driver has to do about it.
				proto::Opcode::Connect => {
					let accepted = handle != 0 && serving.as_deref_mut().is_some_and(|serving| serving.accept(handle));
					if !accepted && handle != 0 {
						// Full, or a loop that serves no provider. Closed rather than kept, so the
						// consumer learns its connection ended instead of waiting on a server that
						// will never read it.
						close(handle);
					}
				}
				proto::Opcode::Ping => {
					let Ok(sequence) = proto::decode_sequence(header.payload(&buf)) else { continue };
					if !pong(bootstrap, bind, sequence) {
						return Control::Ended;
					}
				}
				// ASKED TO STOP, AND IT MEANS ALL OF IT. A driver reaching here has nothing in
				// flight it can finish - the loops that call this are between units of work - so
				// what it owes is the answer and then its own exit. A driver with something to
				// drain overrides `Control::Stop` rather than letting this decide for it.
				proto::Opcode::Stop => return Control::Stop,
				_ => {
					if handle != 0 {
						close(handle);
					}
					return Control::Ended;
				}
			}
		}
	}
}

// Answer one `PING` that is already waiting on `bootstrap`, for a loop that does its own waiting.
//
// False when the manager dropped the channel or sent anything else, which ends the driver.
pub unsafe fn answer_ping(bootstrap: u64, bind: &Bind) -> bool {
	unsafe {
		match drain_control(bootstrap, bind) {
			Control::Continue => true,
			Control::Stop => {
				STOP_PENDING.store(true, core::sync::atomic::Ordering::Release);
				false
			}
			Control::Ended => false,
		}
	}
}

// "Everything I accepted is finished or abandoned and my device is quiet."
//
// Terminal for the binding. A driver sends this and exits; the manager reads it as the confirmation
// that a PLANNED stop completed, which is what makes it different from a channel that simply closed.
pub unsafe fn stopped(bootstrap: u64, bind: &Bind) -> bool {
	unsafe { send_frame(bootstrap, proto::Opcode::Stopped, bind.generation, &[]) }
}

// A STOP WAS READ AND NOT YET ANSWERED.
//
// `STOPPED` is defined as "everything I accepted is finished or abandoned and my device is quiet",
// and the wait helpers used to send it the instant they READ the stop - before the driver had done
// any of that, and before the caller had even been told. `virtio_blk` has `flush_request` and never
// called it on this path; `virtio_snd` exits with a stream still playing; nothing calls
// `device_quiesced`, which is what lets the kernel release orphaned DMA frames and pending vectors.
//
// So the helpers now only LATCH the stop, and `finish_stop` is what answers it - after the driver's
// own cleanup, which is the only code that knows what "finished or abandoned" means for this device.
static STOP_PENDING: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

// THE DEVICE THIS DRIVER BROUGHT UP, so the stop path can stop it.
//
// One per process, because a driver drives one device - which is what `Bind` is about. Zero until
// `bringup_bound` records one, and a driver that never bound has nothing to quieten.
static VIRTIO_COMMON: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn remember_virtio(device: &Virtio) {
	VIRTIO_COMMON.store(device.common_base(), core::sync::atomic::Ordering::Release);
}

// STOP THE DEVICE AND WAIT FOR IT TO SAY SO - the virtio transport's own reset.
//
// This is what `STOPPED` certifies, and no driver was doing it: the reset happened at BRING-UP and
// a planned stop left the queues live. Returns whether the device confirmed; a driver whose device
// does not must not report a clean stop.
pub unsafe fn quiesce_virtio() -> bool {
	let common = VIRTIO_COMMON.load(core::sync::atomic::Ordering::Acquire);
	if common == 0 {
		return false;
	}
	unsafe { virtio::quiesce_at(common) }
}

// Whether the manager has asked this driver to stop. `true` means the exit is a PLANNED one and the
// driver owes a `STOPPED` once it has finished up.
pub fn stop_requested() -> bool {
	STOP_PENDING.load(core::sync::atomic::Ordering::Acquire)
}

// The cleanup is done: answer the stop, and quiesce the device so the kernel may reclaim what it was
// holding on this driver's behalf. Called on the exit path of every driver the manager can stop.
//
// `device_quiesced` is the claim the kernel needs before it will give back a dead driver's DMA frames
// and masked vectors; it was called only during the INITIAL reset, so a stopped driver left both
// held. A driver with no device capability passes 0 and only answers the frame.
// `quiet` is the DRIVER'S OWN ANSWER about its hardware, and it is what this will not invent.
//
// `STOPPED` is a certificate that the device is quiet, and `device_quiesced` is the claim on which
// the kernel gives back orphaned DMA frames and masked vectors. The kernel explicitly cannot check
// either - it relies on the caller having just stopped the device - so a driver that could not
// confirm the hardware stopped must not make the claim. It says so and answers nothing, and the
// manager's deadline then takes the forced path: the claim is quarantined and what it held stays out
// of circulation, which is the correct outcome for a device that may still be mastering the bus.
//
// This used to take no such argument, and every caller was therefore certifying quiescence it had
// not established.
pub unsafe fn finish_stop(bootstrap: u64, bind: &Bind, device: u64, quiet: bool) {
	unsafe {
		if !quiet {
			print(b"driver: the device did not confirm it stopped - no clean stop is acknowledged for it\n");
			STOP_PENDING.store(false, core::sync::atomic::Ordering::Release);
			return;
		}
		if device != 0 {
			device_quiesced(device);
		}
		if STOP_PENDING.swap(false, core::sync::atomic::Ordering::AcqRel) {
			stopped(bootstrap, bind);
		}
	}
}

// "I am here, and this is the number you asked me with."
pub unsafe fn pong(bootstrap: u64, bind: &Bind, sequence: u32) -> bool {
	let mut payload = [0u8; proto::SEQUENCE_PAYLOAD_LEN];
	proto::encode_sequence(sequence, &mut payload);
	unsafe { send_frame(bootstrap, proto::Opcode::Pong, bind.generation, &payload) }
}
