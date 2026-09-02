// DeviceManager - the userspace device supervisor.
//
// ServiceManager starts this program from the init package, hands it a bootstrap
// channel, and over it a view of the init package (so it can spawn drivers from
// it). DeviceManager enumerates the devices the kernel discovered on the PCI bus
// (over the device syscalls) and launches the matching userspace driver for each,
// handing that driver only its own device's MMIO capability. It then reports in
// and stands; ServiceManager exercises the stop path on it (sending "STOP", to
// which it replies "DeviceManager: stopped" and exits). Device-state tracking and
// reacting to a driver crash grow here in later steps.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{OpenOpts, volume};
use rt::*;

include!(concat!(env!("OUT_DIR"), "/program_path.rs"));

// The state DeviceManager tracks per discovered device.
// PRESENCE IS NOT ACTIVATION, and the states say which is which.
//
// It was three values and two of them answered different questions with the same silence: a device
// no driver binds and a device whose driver could not be loaded were both simply absent from the
// summary, so "this system does not support that card" and "the image is missing a file" looked
// identical to anyone reading it. The milestone's second item is that distinction, and these are
// the names it asks for.
const STATE_UNKNOWN: u8 = 0;
const STATE_ONLINE: u8 = 1;
const STATE_FAILED: u8 = 2;
// Present, and no registry entry matches it. The system does not support this device; nothing is
// missing and nothing is wrong. It costs no process, no mapped ELF and no capability.
const STATE_UNBOUND: u8 = 3;
// Present, matched, and the artifact the registry names could not be read off the volume. The image
// is inconsistent with its own registry - a different fault from an unsupported device, and the one
// an operator can act on.
const STATE_DRIVER_MISSING: u8 = 4;

// How many times DeviceManager restarts a driver that crashes during bring-up
// before giving up on its device.
// AT MOST THREE AUTOMATIC ATTEMPTS PER NODE PER BOOT, which is two backoffs between them.
//
// Named for what it bounds. As `MAX_DRIVER_RESTARTS` it was compared against `node.attempt`, which is
// ZERO on the first attempt - so 0, 1 and 2 all passed and the node ran a FOURTH attempt before the
// budget was spent, with a third backoff that re-used the 200ms entry because `BACKOFF_TICKS` is two
// long and the index was clamped. M5 states the numbers: three attempts, two backoffs, 100ms then
// 200ms.
const MAX_AUTOMATIC_ATTEMPTS: u32 = 3;

// Say which budget this launch is working to.
//
// WITH THE NUMBERS IN IT. This said `boot window and this boot's deadline` - a line that names two
// quantities and prints neither, so the one thing it exists to answer, what the budget IS, was left
// for the reader to go and find. Ticks, because that is the unit every deadline in this file is in.
unsafe fn report_boot_window() {
	unsafe {
		let window: u64 = BOOT_WINDOW.load(core::sync::atomic::Ordering::Relaxed);
		if window == 0 {
			print(b"DeviceManager: no boot window was published; a bind is bounded by its own deadline alone\n");
			return;
		}
		let mut number = [0u8; 20];
		let deadline: u64 = BOOT_DEADLINE.load(core::sync::atomic::Ordering::Relaxed);
		print(b"DeviceManager: boot window ");
		let n = decimal(window, &mut number);
		print(&number[..n]);
		if deadline == 0 {
			print(b" tick(s), carried over without the boot's deadline\n");
		} else {
			print(b" tick(s), this boot's deadline at tick ");
			let n = decimal(deadline, &mut number);
			print(&number[..n]);
			print(b"\n");
		}
	}
}

// THE BLOCK TAGS THE BOOT HAND-OFF CARRIES, in the order ServiceManager reads them: the system
// volume, then the read-only FAT media, ISO9660 and UDF volumes. A property of that wire, named
// here so the number is one list rather than four variables and a `send` each.
const BOOT_BLOCK_TAGS: [&[u8]; 4] = [b"BLOCK", b"BLOCK2", b"BLOCK3", b"BLOCK4"];

// EVERY NUMBER THAT BOUNDS A BIND, IN ONE PLACE.
//
// They were three constants and a `recv_blocking`, which is to say two of them did not exist. A
// budget spread across the call sites that spend it is one nobody can add up, and the addition is
// the whole question here: the kernel's recovery ladder reboots the machine when the chain does not
// settle, so what matters is not what one device may cost but what every device may cost together.
//
// A tick is a hundredth of a second, which is what `clock()` counts and what `wait` takes.
const TICKS_PER_SECOND: u64 = 100;

// How long one attempt may wait for `READY` after its `BIND`. Two seconds, for the reason the
// development agent's deadline is two seconds: ServiceManager is blocked on this program's phase-2
// answer, and around all of it the kernel reboots the machine when its window runs out. A deadline
// longer than that window never fires.
const READY_DEADLINE_TICKS: u64 = 2 * TICKS_PER_SECOND;

// The delays between automatic attempts: 100 ms, then 200 ms. Two of them, because three attempts
// have two gaps. A backoff that would end after the incident deadline is not entered at all.
const BACKOFF_TICKS: [u64; 2] = [TICKS_PER_SECOND / 10, TICKS_PER_SECOND / 5];

// The share of the boot window ONE device's bring-up may spend, and the share of THAT reserved for
// giving the device back.
//
// A third, and a third of a third. Both are choices and neither is a measurement; what makes them
// defensible is the shape. Two thirds of the boot are left for everything that is not one device's
// bind - and a bind that exhausts its own budget still has a positive slice to release the claim
// with. "Whatever is left" was the first rule and it hands the rollback ZERO in exactly the case a
// rollback exists for: a bind that failed BY running out of time.
const BIND_SHARE_OF_WINDOW: u64 = 3;
const TEARDOWN_SHARE_OF_BIND: u64 = 3;

// The boot window this program was handed, in monotonic ticks. See ServiceManager, which keeps the
// length across restarts of this program and spends the deadline on the first launch only.
//
// Zero for either means "not published", and each is answered separately: no deadline means only
// the length bounds an incident, and no length at all means this program falls back to its own
// per-attempt deadline with no absolute bound - which is what it did before the window existed.
static BOOT_DEADLINE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static BOOT_WINDOW: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

// When THIS bring-up incident must be finished, and how much of it is kept back for the teardown.
//
// AN INCIDENT, NOT A NODE AND NOT A BOOT. Measuring from a node's first ever `BIND` would mean a
// driver that ran happily for an hour and then crashed has no budget left to be rebound with; its
// recovery would be `Failed` on arithmetic about a boot that finished long ago. Each bind-or-recover
// attempt-chain starts its own window, and the boot's own deadline clamps the first one because
// that is the only one competing with the boot.
// The teardown slice a machine that published no boot window gets. A teardown is a kill and a
// device release, both of which are fast when they work at all; this is how long a manager waits
// before deciding one did not.
const TEARDOWN_FALLBACK_TICKS: u64 = 200;

struct Incident {
	// Absolute tick by which everything - every attempt, every backoff, and the final teardown -
	// must be over. Zero when no window was published, meaning only the per-attempt deadline binds.
	deadline: u64,
	// Ticks kept back from `deadline` so a rollback has something to spend.
	teardown_reserve: u64,
}

impl Incident {
	// Open a window from now, clamped by the boot's own deadline while that is still in hand.
	unsafe fn open() -> Incident {
		unsafe {
			let window: u64 = BOOT_WINDOW.load(core::sync::atomic::Ordering::Relaxed);
			if window == 0 {
				return Incident { deadline: 0, teardown_reserve: 0 };
			}
			// THE ARITHMETIC IS `driver_binding`'s, where it can be DRIVEN. Two clamps and an
			// off-by-one between them decide whether a machine recovers, and that is not something
			// to reason about inside a binary nobody can run on a host - which is how the clamp
			// came to be unconditional and every recovery an hour after boot born already expired.
			let deadline: u64 = driver_binding::IncidentWindow::deadline(window, BIND_SHARE_OF_WINDOW, BOOT_DEADLINE.load(core::sync::atomic::Ordering::Relaxed), clock());
			Incident { deadline, teardown_reserve: (window / BIND_SHARE_OF_WINDOW) / TEARDOWN_SHARE_OF_BIND }
		}
	}

	// The deadline ONE attempt's `READY` wait gets: the shorter of the per-attempt allowance and
	// what is left of the incident once the teardown's share is set aside.
	unsafe fn attempt_deadline(&self) -> u64 {
		unsafe {
			let now: u64 = clock();
			let by_attempt: u64 = now.saturating_add(READY_DEADLINE_TICKS);
			if self.deadline == 0 {
				return by_attempt;
			}
			let spendable: u64 = self.deadline.saturating_sub(self.teardown_reserve);
			// PAST IT ALREADY IS NOT "NO DEADLINE". Returning zero here would mean "wait forever",
			// which is the opposite of what an exhausted budget asks for, so it returns an instant
			// that has already passed and the receive reports a timeout on its first look.
			if spendable <= now {
				return now.max(1);
			}
			by_attempt.min(spendable)
		}
	}

	// THE TICK A TEARDOWN'S TWO CONFIRMATIONS MUST BOTH BE IN BY.
	//
	// The reserve M5 sets aside, applied - which is what it was for and what nothing did with it: the
	// rollback answered synchronously, so a child that ignored `SIG_KILL` and a claim that never
	// reached `Free` were indistinguishable from a clean teardown and the reserve was a number the
	// window arithmetic subtracted and nobody spent. A window that was never published leaves this a
	// fixed slice rather than "forever", because "wait forever for a dead child" is the failure.
	unsafe fn teardown_deadline(&self) -> u64 {
		unsafe { clock().saturating_add(if self.teardown_reserve == 0 { TEARDOWN_FALLBACK_TICKS } else { self.teardown_reserve }) }
	}

	// Whether there is time for a backoff of `delay` and an attempt after it. A backoff that would
	// end after the deadline is not entered at all: the node goes straight to its verdict rather
	// than sleeping through the last of its budget and waking up to be told it is out.
	unsafe fn allows_backoff(&self, delay: u64) -> bool {
		unsafe { self.deadline == 0 || clock().saturating_add(delay) < self.deadline.saturating_sub(self.teardown_reserve) }
	}
}

// WHERE A BINDING IS AND WHY, from the crate that owns the answer. Not a `u8` per device with a
// meaning each reader remembers: `state[idx]` is a small integer set at a few points, and the
// transitions between its values are wherever somebody happened to write them.
use driver_binding::{BindingEvent, BindingId, BindingQueue, BindingRecord, BindingState, FailureCause, ProviderId};

// How long the development agent has to report in before DeviceManager stops waiting for it, in
// clock ticks (a hundred to the second, so this is two seconds).
//
// THE WAIT IS ON THE BOOT'S CRITICAL PATH, AND THAT PATH IS ALREADY BEING TIMED. ServiceManager is
// blocked on DeviceManager's phase-2 answer, which is blocked on this; around all of it the
// kernel's recovery ladder gives the chain a bounded number of settle rounds and REBOOTS THE
// MACHINE when they run out. A deadline longer than that window can never fire - the guest is
// already restarting - so this one has to be much shorter than it, not merely finite. Ten seconds
// was tried and the reboot beat it every time.
//
// Two seconds is the other side of the same measurement: a working agent reports in before the
// first service does, so this is orders of magnitude more time than a spawn and three receives
// need, and it still leaves most of the settle window for the rest of the chain to come up in.
#[cfg(feature = "development")]
const DEV_AGENT_REPORT_TICKS: u64 = 200;

// Everything this program holds on behalf of the development agent, so supervising it is one
// value rather than five more out-parameters threaded through driver launching.
//
// DeviceManager supervises the agent because DeviceManager started it: it exists exactly when
// the development channel device does, and nothing else in the system knows that. The agent
// dying is survivable, and this is what makes it so - without a supervisor its death would
// take the port with it, since the driver has nobody to hand bytes to and no way to acquire
// another.
//
// In a shipping build none of this is reachable: the device is not bound, no agent is
// started, and the capabilities below are never delivered.
#[cfg(feature = "development")]
#[derive(Default)]
struct DevAgent {
	// The agent's bootstrap. It is both how capabilities reach the agent and how this program
	// learns the agent is gone: it closes when the process ends, however it ended.
	bootstrap: u64,
	// The transport driver's bootstrap, over which a replacement agent's wire is handed down.
	// The driver keeps its device and its port across the gap and waits for that message.
	driver: u64,
	// A volume client of this program's own, kept past driver bring-up. The connection the
	// drivers were read through belongs to ServiceManager's message and is closed with it,
	// and a replacement has to be read off the volume long after that.
	storage: u64,
	// The capabilities delivered once, after the first agent was already running. They are
	// retained rather than forwarded and forgotten, because a replacement that could neither
	// launch a program nor answer a resolution query would be an agent in name only - and
	// nothing would deliver them a second time.
	launcher: u64,
	registry: u64,
	// The console this program hands every keyboard driver, kept so a REPLACEMENT agent gets one
	// too. Retained rather than re-derived: `restart` runs long after phase 2, where the value came
	// from, and an agent that could not type would be a quietly reduced agent rather than a failed
	// one - which is the harder kind of fault to notice.
	console_input: u64,
	// The value every handshake reports so a tool can tell which boot answered it. It is drawn
	// here, once, and handed to each agent: it identifies the boot, and this program is what
	// lives as long as the boot does. An agent drawing its own would announce a reboot that did
	// not happen every time it was replaced.
	nonce: [u8; 8],
}

#[cfg(feature = "development")]
impl DevAgent {
	// Retain a capability and pass the live agent a copy of it. A copy rather than the handle
	// itself: this program has to be able to give the same capability to the next agent, and
	// only one agent is ever alive to use it.
	unsafe fn deliver(&self, tag: &[u8], held: u64) {
		unsafe {
			if self.bootstrap == 0 || held == 0 {
				return;
			}
			let Some(info) = object_info(held) else { return };
			let copy: i64 = duplicate(held, info.rights);
			if copy < 0 || !send_blocking(self.bootstrap, tag, copy as u64) {
				if copy >= 0 {
					close(copy as u64);
				}
				print(b"DeviceManager: the development agent did not take ");
				print(tag);
				print(b"\n");
			}
		}
	}

	unsafe fn hold_launcher(&mut self, handle: u64) {
		unsafe {
			if handle == 0 {
				return;
			}
			if self.bootstrap == 0 {
				close(handle);
				return;
			}
			self.launcher = handle;
			self.deliver(b"PERM", handle);
		}
	}

	unsafe fn hold_registry(&mut self, handle: u64) {
		unsafe {
			if handle == 0 {
				return;
			}
			if self.bootstrap == 0 {
				close(handle);
				return;
			}
			self.registry = handle;
			self.deliver(b"REG", handle);
		}
	}

	// The agent's bootstrap became readable. Anything it says is passed through to the console;
	// its closing means the process ended, and a fresh one takes its place.
	#[cfg(feature = "development")]
	unsafe fn supervise(&mut self, buf: &mut [u8]) {
		unsafe {
			match recv_blocking(self.bootstrap, buf) {
				Received::Message { len, .. } => {
					print(&buf[..len]);
					print(b"\n");
				}
				Received::Closed => self.restart(),
			}
		}
	}

	// Start a replacement and give it everything the one before it had, except what it held:
	// the registry was in the dead process's memory and is gone, which is the whole meaning of
	// a restart. A failure at any step leaves the port transport-only rather than half-wired,
	// and says so.
	#[cfg(feature = "development")]
	unsafe fn restart(&mut self) {
		unsafe {
			close(self.bootstrap);
			self.bootstrap = 0;
			if self.driver == 0 || self.storage == 0 {
				return;
			}
			let Some((driver_side, agent_side)) = channel() else { return };
			// The agent is started before the driver is told, so a start that fails leaves the
			// driver waiting for a channel that will be offered again rather than holding one
			// whose other end never appears.
			self.bootstrap = start_dev_agent(self.storage, agent_side, self.console_input, &self.nonce);
			if self.bootstrap == 0 || !send_blocking(self.driver, b"BYTES", driver_side) {
				close(driver_side);
				print(b"DeviceManager: the development agent did not restart; the control channel is transport-only\n");
				return;
			}
			self.deliver(b"PERM", self.launcher);
			self.deliver(b"REG", self.registry);
		}
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. receive the init package shared buffer (to spawn drivers from) and map it.
		let (_pkg_handle, archive): (u64, &[u8]) = recv_package(bootstrap, &mut buf).unwrap_or_else(|| fail_bootstrap(bootstrap, b"package", b"init package not delivered"));
		let package: Package = Package::parse(archive).unwrap_or_else(|| fail_bootstrap(bootstrap, b"package", b"init package malformed"));
		// 1b. receive the SystemPower connection - the narrow door to stopping the machine, which
		//     this service holds only to mint one per keyboard driver. It used to be the
		//     root-Domain handle itself. `SYS_SYSTEM_POWER`
		//     checks it, and the Power key is the one path to stopping the machine that must
		//     survive a wedged supervisor, so it cannot be routed through ServiceManager.
		let power: u64 = recv_tagged(bootstrap, &mut buf, b"SYSPOWER").unwrap_or_else(|| fail_bootstrap(bootstrap, b"power", b"missing SystemPower connection"));
		// 1b2. and the ConsoleInputSource capability, held only to delegate to the same two
		//      keyboard drivers. `SYS_CONSOLE_FEED` requires it: a keyboard without one types
		//      nothing rather than typing on an authority it does not hold. Optional, like the
		//      drivers' own handling of it, so a boot that granted none still starts.
		let console_input: u64 = recv_tagged(bootstrap, &mut buf, b"CONSOLE").unwrap_or(0);
		// 1b3. and the DeviceManager privilege, which is what `device_claim`,
		//      `device_msix_acquire` and `interrupt_bind` now require. Without it this program takes
		//      no device out of the kernel and no driver is launched - which is the right failure:
		//      ungated, those syscalls handed any process the BAR of any PCI device, and on a
		//      machine with no IOMMU a DMA-capable one reaches memory the page tables were meant to
		//      isolate.
		let device_privilege: u64 = recv_tagged(bootstrap, &mut buf, b"DEVPRIV").unwrap_or(0);
		// 1b4. and the boot window: how long this boot is allowed to take, and when THIS boot's
		//      window closes. Carries no capability. Zero for either is "not published", and this
		//      program then bounds a bind by its per-attempt deadline alone - which is what it did
		//      before a window existed, so an old supervisor still starts a new DeviceManager.
		match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } if len >= 7 + 16 && &buf[..7] == b"BOOTWIN" => {
				BOOT_DEADLINE.store(u64::from_le_bytes(buf[7..15].try_into().unwrap_or([0; 8])), core::sync::atomic::Ordering::Relaxed);
				BOOT_WINDOW.store(u64::from_le_bytes(buf[15..23].try_into().unwrap_or([0; 8])), core::sync::atomic::Ordering::Relaxed);
			}
			_ => {}
		}
		// WHAT THIS LAUNCH WAS GIVEN, SAID OUT LOUD, because it is the only way to tell a first
		// launch from a restart from outside. The first DeviceManager of a boot is handed the
		// boot's own deadline; every one after it is handed the LENGTH and no deadline, since a
		// deadline in the past is not a budget. Two lines that differ is what makes "ServiceManager
		// kept the length" an observation rather than a claim about a variable nobody can see.
		report_boot_window();
		// 1b5. and the channel clients reach the provider catalogue on. Optional: a boot that
		//      granted none simply has nobody asking, and refusing to start over it would trade a
		//      missing query interface for a machine with no drivers.
		let catalogue_service: u64 = recv_tagged(bootstrap, &mut buf, b"SERVE").unwrap_or(0);

		// 2. phase 1: launch the bootstrap block driver (virtio_blk) for each disk it backs.
		//    It hands back a block-read service channel, which we route up to ServiceManager
		//    (it forwards it to StorageService). The non-bootstrap drivers cannot load yet -
		//    they live on the system volume, which is only mountable once virtio_blk and
		//    StorageService are up - so they wait for phase 2 below.
		// ONE CATALOGUE FOR THE LIFE OF THIS PROCESS. It was a local inside each bring-up phase,
		// which meant it died with the function that filled it - so what a machine has published
		// could be reported at the end of a phase and asked about never again. It outlives both
		// phases now, because it is what the provider-catalogue interface is served from.
		let mut catalogue = Catalogue::new();
		// One node per device, for the life of this program - see `launch_boot_drivers`. Both
		// bring-up phases append to this, and what supervises a driver afterwards reads it.
		let mut nodes: Vec<Node> = Vec::new();
		// The connections minted from the catalogue's root. Bounded: a server that mints a channel
		// per request without a bound is a server a client can exhaust.
		let mut catalogue_clients = CatalogueClients::new();
		// The operator endpoint and the ConfigService connection its writes go through. Both arrive
		// after bring-up; 0 until they do, and a boot that grants neither simply has no operator
		// path rather than a half-built one.
		let mut policy_service: u64 = 0;
		// The connections minted from the policy endpoint's root. One table per endpoint, because a
		// request arriving on one must not be answered by the other's view.
		let mut policy_clients = CatalogueClients::new();
		let mut policy_config: u64 = 0;
		// THE BOOT WIRE'S SHAPE, NOT THIS PROGRAM'S LIMIT, and the difference is the whole of M7.
		//
		// These were four named locals - `block_client`, `block2_client`, `block3_client`,
		// `block4_client` - each routed by hand, so a second disk had somewhere to go and a fifth
		// did not, and which volume was which depended on which driver finished first. What is left
		// is the number of BLOCK tags the hand-off to ServiceManager carries, which is a fact about
		// that wire: `BLOCK`, `BLOCK2`, `BLOCK3`, `BLOCK4`, feeding the system, media, ISO and UDF
		// volumes. The CATALOGUE has no such number - it holds `MAX_PROVIDERS` of any kind - so a
		// fifth disk is published, counted and REPORTED rather than silently dropped into a variable
		// that does not exist.
		let mut boot_blocks: [u64; BOOT_BLOCK_TAGS.len()] = [0; BOOT_BLOCK_TAGS.len()];
		// A connection to each block provider for whoever has to CHOOSE among them - see
		// `mint_connection` and the `PROBE` tags below.
		let mut probe_blocks: [u64; BOOT_BLOCK_TAGS.len()] = [0; BOOT_BLOCK_TAGS.len()];
		let mut net_client: u64 = 0;
		let mut input_client: u64 = 0;
		let mut usb_client: u64 = 0;
		let mut usbq_client: u64 = 0;
		let mut usb_pointer: u64 = 0;
		let mut raw_keys: u64 = 0;
		// What a post-`Online` restart needs, filled in by phase two. See `Recovery`.
		let mut recovery: Recovery = Recovery::none();
		// What this program holds on behalf of the development agent: its bootstrap, so the
		// launcher can be handed to it once PermissionManager exists - which is after this
		// program has finished starting drivers - and so its death is noticed and answered.
		#[cfg(feature = "development")]
		let mut dev: DevAgent = DevAgent::default();
		launch_boot_drivers(&package, &mut catalogue, &mut nodes, power, console_input, device_privilege, &mut buf, &mut boot_blocks, &mut probe_blocks);

		// 3. report in once the disks are bound, transferring the block service channel up
		//    the boot chain, then the second/third/fourth block disks' service channels (the
		//    report itself carries one handle; each `BLOCK2`/`BLOCK3`/`BLOCK4` handle is 0
		//    when that disk is absent). The net / gpu / snd / input driver channels follow in
		//    phase 2, once the volume they load from is mounted.
		send_blocking(bootstrap, b"DeviceManager: online", boot_blocks[0]);
		for (at, tag) in BOOT_BLOCK_TAGS.iter().enumerate().skip(1) {
			send_blocking(bootstrap, tag, boot_blocks[at]);
		}
		// AND THE PROBE CONNECTIONS, in the same order and under their own tags. A zero handle is a
		// disk this machine does not have, and the tag travels anyway so the reader's positions do
		// not shift - the same rule every other hand-off in this chain follows.
		for probe in probe_blocks {
			send_blocking(bootstrap, b"PROBE", probe);
		}

		// 4. stand until ServiceManager drives phase 2 (a "DRIVERS" message carrying a
		//    StorageService client, once the volume is up: we load the non-bootstrap drivers
		//    from vol://system/drivers/ and hand their channels up) or asks us to stop (which
		//    also drops the driver channels, so the drivers shut down with us).
		loop {
			// The development agent's bootstrap is watched alongside ServiceManager's. Its
			// closing is how this program learns the agent died, and answering that is this
			// program's job because this program is what started it.
			#[cfg(feature = "development")]
			if dev.bootstrap != 0 && wait_any(&[bootstrap, dev.bootstrap], 0) == 1 {
				dev.supervise(&mut buf);
				continue;
			}
			// THE WATCHDOG RUNS ON EVERY PASS OF THIS LOOP, and the wait comes back for it.
			//
			// A driver that stopped answering is torn down through the same machinery a crashed one
			// is: `advance` reads the event and the transaction rolls back. Anything it leaves
			// terminal stays terminal - there is no volume to reload a driver from at this point,
			// so a rebind is P02M0165 M6's subject and not this loop's.
			let mut soonest: u64 = tick_heartbeats(&mut nodes, &mut buf);
			// AND A NODE WAITING OUT ITS BACKOFF IS SOMETHING TO WAKE FOR. The loop skips such a node
			// rather than sleeping on it, so without this the wait could park past the moment it
			// became due and the retry would land whenever something unrelated next arrived.
			for node in nodes.iter() {
				if node.retry_at != 0 && (soonest == 0 || node.retry_at < soonest) {
					soonest = node.retry_at;
				}
				// AND A TEARDOWN RUNNING OUT OF ITS SLICE. The confirmations arrive on the handles
				// added to the wait below; this is what wakes the loop when they do NOT.
				if let Some(teardown) = &node.teardown {
					if soonest == 0 || teardown.deadline < soonest {
						soonest = teardown.deadline;
					}
				}
			}
			// A NODE WAITING FOR A PROVIDER THAT HAS ARRIVED, AND ONE WHOSE PROVIDER HAS GONE. The
			// standing loop is where a driver bound in phase two publishes what a later one waits
			// for, and where a withdrawal reaches an online node.
			settle_dependencies(&mut nodes, &mut catalogue);
			for at in 0..nodes.len() {
				let name: &[u8] = nodes[at].driver_name();
				// A NODE WHOSE BACKOFF HAS COME DUE. `advance` is event-driven and a node parked in
				// `Backoff` raises none, so without this a deferred attempt - a retryable failure, or
				// a device that was still being released when this manager looked - would wait for
				// some unrelated message to arrive before anything tried again.
				if nodes[at].record.state == BindingState::Backoff && nodes[at].retry_at != 0 && clock() >= nodes[at].retry_at && recovery.armed() {
					nodes[at].retry_at = 0;
					start_candidate(&mut nodes[at], recovery.storage, recovery.key_producer, power, console_input, device_privilege, &catalogue, &mut recovery.state);
				}
				// AN OPERATOR'S RETRY, PERFORMED. One attempt, from wherever the node was left -
				// but never on a node that still HAS a binding.
				//
				// `Retry` legitimately asks for one from `Failed` and from `Backoff`, so the seam
				// cannot require `Unbound`; what it must refuse is a node whose driver is still
				// running. `Online -> Binding` is not an edge, so every attempt failed and
				// `start_candidate` advanced the cursor for it, walking the list to exhaustion on a
				// device that was working the whole time.
				if nodes[at].restart_requested && recovery.armed() {
					nodes[at].restart_requested = false;
					if nodes[at].binding.is_none() && nodes[at].teardown.is_none() {
						start_candidate(&mut nodes[at], recovery.storage, recovery.key_producer, power, console_input, device_privilege, &catalogue, &mut recovery.state);
					}
				}
				// THE ANSWER IS ACTED ON. This discarded the `Step`, so a driver that crashed after
				// coming online was moved to `Backoff` and left there for the rest of the boot -
				// the bring-up loops that handle `Again` had returned long before. Recovery is only
				// possible once phase two has handed over what a rebind needs.
				match advance(&mut nodes[at], name, &mut catalogue) {
					// THE STANDING LOOP DOES NOT WAIT AT ALL. It comes round on its own bounded wait,
					// so a node whose backoff has not passed is simply skipped this time - which is
					// what "one node's delay is not every node's" means in the loop that has others.
					Step::Again if recovery.armed() && clock() >= nodes[at].retry_at => {
						start_candidate(&mut nodes[at], recovery.storage, recovery.key_producer, power, console_input, device_privilege, &catalogue, &mut recovery.state);
					}
					Step::NextCandidate if recovery.armed() => {
						spend_candidate(&mut nodes[at]);
						// AN OPERATOR'S ONE ATTEMPT IS SPENT HERE - see `Node::retry_once`. The
						// cursor still advances, so a later request tries the entry after this one
						// rather than the same one again; what does not happen is this program
						// starting it unasked.
						if core::mem::take(&mut nodes[at].retry_once) {
							print(b"DeviceManager: the attempt an operator asked for is spent; the next candidate is not started automatically\n");
						} else if nodes[at].candidate < nodes[at].candidates.len() {
							start_candidate(&mut nodes[at], recovery.storage, recovery.key_producer, power, console_input, device_privilege, &catalogue, &mut recovery.state);
						}
					}
					_ => {}
				}
			}
			// AND ANY NEW INCIDENT IS WRITTEN DOWN, so it outlives this program - see `persist_incidents`.
			persist_incidents(&mut nodes, policy_config);
			// ONE WAIT, AND ONLY A READY BOOTSTRAP FALLS THROUGH TO THE RECEIVE.
			//
			// Anything else goes round: a catalogue query is answered and a DEADLINE means the
			// watchdog has something to do. Falling through on the deadline was the first version
			// and it disarmed the whole mechanism - `recv_blocking` parks until the supervisor
			// speaks, so a driver could go quiet and nothing would look again until something
			// unrelated happened to arrive.
			// The wait set: the supervisor's channel, the catalogue's root, and every connection
			// minted from it. One wait, so a catalogue query cannot delay a supervisor message and
			// a supervisor message cannot delay a query.
			// PLUS TWO PER OUTSTANDING TEARDOWN. A node stopped after it came online waits for its
			// child's exit and its claim's settling exactly as one stopped during bring-up does, and
			// this is the loop those nodes live in - so the handles go into the same one wait rather
			// than being polled beside it.
			const WAITING_MAX: usize = MAX_CATALOGUE_CLIENTS * 2 + 3 + MAX_NODES_IN_FLIGHT * 2;
			let mut waiting: [u64; WAITING_MAX] = [0; WAITING_MAX];
			let mut waiting_count: usize = 1;
			waiting[0] = bootstrap;
			if catalogue_service != 0 {
				waiting[waiting_count] = catalogue_service;
				waiting_count += 1;
			}
			let policy_at: usize = waiting_count;
			if policy_service != 0 {
				waiting[waiting_count] = policy_service;
				waiting_count += 1;
			}
			let policy_clients_at: usize = waiting_count;
			for &client in policy_clients.live() {
				waiting[waiting_count] = client;
				waiting_count += 1;
			}
			let catalogue_clients_at: usize = waiting_count;
			for &client in catalogue_clients.live() {
				waiting[waiting_count] = client;
				waiting_count += 1;
			}
			// The teardown handles go LAST, so everything before them keeps the index arithmetic the
			// serving branches below rely on.
			let teardowns_at: usize = waiting_count;
			let mut teardown_owner: [usize; MAX_NODES_IN_FLIGHT * 2] = [0; MAX_NODES_IN_FLIGHT * 2];
			let mut teardown_claim: [bool; MAX_NODES_IN_FLIGHT * 2] = [false; MAX_NODES_IN_FLIGHT * 2];
			let mut teardown_count: usize = 0;
			for (at, node) in nodes.iter().enumerate() {
				let Some(teardown) = &node.teardown else { continue };
				for (handle, is_claim) in [(teardown.pending.process, false), (teardown.pending.claim, true)] {
					if handle == 0 || waiting_count >= WAITING_MAX {
						continue;
					}
					waiting[waiting_count] = handle;
					teardown_owner[teardown_count] = at;
					teardown_claim[teardown_count] = is_claim;
					teardown_count += 1;
					waiting_count += 1;
				}
			}
			let ready: i64 = wait_any(&waiting[..waiting_count], soonest);
			if ready != 0 {
				if ready > 0 {
					let at: usize = ready as usize;
					if at >= teardowns_at {
						// A TEARDOWN CONFIRMATION. Queued on its node like every other event, and
						// `advance` above is what reads it on the next pass.
						let which: usize = teardown_owner[at - teardowns_at];
						let generation: u64 = nodes[which].id.generation;
						if teardown_claim[at - teardowns_at] {
							if let Ok(info) = device_claim_info(waiting[at]) {
								if info.settled != 0 {
									nodes[which].push(BindingEvent::ClaimSettled { generation, state: info.state });
								}
							}
						} else {
							nodes[which].push(BindingEvent::Exited { generation });
						}
						continue;
					}
					// AND A CONNECTION WHOSE PEER HAS GONE IS GIVEN BACK RATHER THAN WAITED ON AGAIN.
					//
					// A closed channel is readable for ever - that is how `recv` gets to report the
					// closure instead of blocking - so a consumer that exited woke this wait on every
					// pass and the loop spun answering nothing, with the teardown handles behind it
					// starved by a dead handle in front. The serve functions answer whether the peer
					// is still there, and a `false` from a MINTED connection retires it. The two
					// roots are not retired: they are this program's service registrations, not
					// per-client connections, and there is no next client without them.
					if policy_service != 0 && (at == policy_at || (at >= policy_clients_at && at < policy_clients_at + policy_clients.live().len())) {
						let is_root: bool = at == policy_at;
						if !serve_policy_once(waiting[at], is_root, &mut policy_clients, &mut nodes, &mut catalogue, policy_config, &mut buf) && !is_root {
							print(b"DeviceManager: a device-policy client closed its connection; the slot is given back\n");
							policy_clients.retire(at - policy_clients_at);
						}
					} else {
						let is_root: bool = catalogue_service != 0 && at == 1;
						if !serve_catalogue_once(waiting[at], is_root, &mut catalogue_clients, &mut catalogue, &nodes, &mut buf) && !is_root {
							print(b"DeviceManager: a provider-catalogue client closed its connection; the slot is given back\n");
							catalogue_clients.retire(at - catalogue_clients_at);
						}
					}
				}
				continue;
			}
			match recv_blocking(bootstrap, &mut buf) {
				Received::Message { len, handle } if len >= 7 && &buf[..7] == b"DRIVERS" => {
					#[cfg(feature = "development")]
					launch_volume_drivers(handle, &mut catalogue, &mut nodes, power, console_input, device_privilege, &mut buf, &mut net_client, &mut input_client, &mut usb_client, &mut usbq_client, &mut usb_pointer, &mut raw_keys, &mut recovery, &mut dev);
					#[cfg(not(feature = "development"))]
					launch_volume_drivers(handle, &mut catalogue, &mut nodes, power, console_input, device_privilege, &mut buf, &mut net_client, &mut input_client, &mut usb_client, &mut usbq_client, &mut usb_pointer, &mut raw_keys, &mut recovery);
					// NOT CLOSED: `Recovery` holds it, because rebinding a crashed driver means
					// reading its artifact off the volume again. See `Recovery`.
					send_blocking(bootstrap, b"NET", net_client);
					// THE TAG CARRIES A FACT, NOT A CHANNEL - the display half, for the same reason
					// as the audio one below and with the same shape (2026-09-02). DisplayService
					// subscribes to the catalogue for its device now, so this program routes no
					// display channel and holds no `gpu_client` slot. What travels behind the tag is
					// whether this machine has a display driver bound, which the supervisor's driver
					// status view reports and which only this program knows.
					let display: [u8; 4] = [b'G', b'P', b'U', u8::from(catalogue.count_of(driver_protocol::provider::DISPLAY) > 0)];
					send_blocking(bootstrap, &display, 0);
					// AudioService subscribes for its provider too - and the boot hand-off is read
					// POSITIONALLY at every hop, so dropping the message would shift every read
					// after it. What travels behind the tag is the one thing the supervisor did with
					// that handle: whether this machine has a sound driver bound at all, which its
					// driver status view reports and which only this program knows.
					let audio: [u8; 4] = [b'S', b'N', b'D', u8::from(catalogue.count_of(driver_protocol::provider::AUDIO) > 0)];
					send_blocking(bootstrap, &audio, 0);
					send_blocking(bootstrap, b"INPUT", input_client);
					send_blocking(bootstrap, b"USB", usb_client);
					send_blocking(bootstrap, b"USBBUS", usbq_client);
					// the xhci driver's pointer-event channel (a USB pointing device;
					// InputService folds it alongside the virtio pointer's).
					send_blocking(bootstrap, b"INPUT2", usb_pointer);
					send_blocking(bootstrap, b"KEYS", raw_keys);
				}
				// The development agent's launcher, delivered once PermissionManager is up.
				// Forwarded rather than held: this program has no use for it, and the agent
				// could not have been given one when it started.
				#[cfg(feature = "development")]
				Received::Message { len, handle } if len >= 7 && &buf[..7] == b"DEVPERM" => dev.hold_launcher(handle),
				// A SHIPPING BOOT STILL RECEIVES BOTH. ServiceManager sends them whenever
				// PermissionManager comes up and its own comment says a boot with no agent ignores
				// them - so they are consumed and closed here. Without this arm they fall through
				// to the catch-all below, which reads any other message as "stop" and takes
				// DeviceManager down with the first one.
				#[cfg(not(feature = "development"))]
				Received::Message { len, handle } if (len >= 7 && &buf[..7] == b"DEVPERM") || (len >= 6 && &buf[..6] == b"DEVREG") => {
					if handle != 0 {
						close(handle);
					}
				}
				// The other end of the channel ProcessService already holds, so a launch can
				// ask the registry whether it has a generation of the artifact it is about to
				// read off the volume.
				#[cfg(feature = "development")]
				Received::Message { len, handle } if len >= 6 && &buf[..6] == b"DEVREG" => dev.hold_registry(handle),
				// The operator endpoint and this program's own ConfigService connection, both
				// arriving after ConfigService exists - which cannot be at this program's own
				// bootstrap, because ConfigService depends on the block driver this program binds.
				Received::Message { len, handle } if len >= 6 && &buf[..6] == b"POLICY" && len < 9 => {
					policy_service = handle;
				}
				Received::Message { len, handle } if len >= 9 && &buf[..9] == b"POLICYCFG" => {
					policy_config = handle;
					// READ BACK WHAT WAS STORED, now that there is somewhere to read it from.
					load_stored_policy(&mut nodes, policy_config);
					// AND THE RECORDS THAT NO LONGER DESCRIBE ANYTHING GO. The one moment this
					// program has both the inventory and somewhere to write - see the function.
					forget_absent_incidents(&nodes, policy_config);
				}
				Received::Message { .. } => {
					// THE DRIVERS GO DOWN BEFORE THIS PROGRAM DOES, and in reverse dependency
					// order. Answering the supervisor first and exiting would drop every driver
					// channel at once, which is a forced revocation dressed up as a shutdown.
					stop_all(&mut nodes, &mut catalogue, driver_binding::StopIntent::Shutdown, &mut buf);
					send_blocking(bootstrap, b"DeviceManager: stopped", 0);
					break;
				}
				Received::Closed => {
					// The supervisor is gone and there is nobody left to answer, but the devices are
					// still live and still this program's to quieten.
					stop_all(&mut nodes, &mut catalogue, driver_binding::StopIntent::Shutdown, &mut buf);
					break;
				}
			}
		}
	}
	exit();
}

// Phase 1: enumerate the kernel device table and spawn the bootstrap block
// driver (virtio_blk) for each disk it backs, from the init package, handing it only that
// device's MMIO capability and info. Each disk's block-read service channel is routed up
// (system / media / iso / udf, in discovery order). The non-bootstrap drivers are skipped
// here - they load from the volume in phase 2, once it is mounted.
unsafe fn launch_boot_drivers(package: &Package, catalogue: &mut Catalogue, nodes: &mut Vec<Node>, power: u64, console_input: u64, device_privilege: u64, buf: &mut [u8], boot_blocks: &mut [u64; BOOT_BLOCK_TAGS.len()], probe_blocks: &mut [u64; BOOT_BLOCK_TAGS.len()]) {
	unsafe {
		let count: u64 = device_count();
		// ONE NODE PER BOOT-CRITICAL DEVICE, and they come up TOGETHER. Four disks used to be bound
		// one after another, each waiting out its own handshake before the next was started; they
		// are independent devices and there was never a reason for the fourth to wait on the first.
		// NODES OUTLIVE BRING-UP. They were locals in each phase, so a device could be supervised
		// only while the function that bound it was still running - which is until the boot chain
		// moves on. A node is per-DEVICE and lives for the life of this program, which is what M2 of
		// the transactional-bind milestone says it is and what a heartbeat needs it to be.
		let first_node: usize = nodes.len();
		let mut i: u64 = 0;
		while i < count {
			let mut info: DeviceInfo = DeviceInfo::default();
			if !device_info(i, &mut info) {
				i += 1;
				continue;
			}
			// PHASE ONE IS THE BOOT-CRITICAL LIFECYCLE, declared in the manifest, rather than the
			// name `virtio_blk` written here. That is the milestone's bootstrap exception stated as
			// a rule: only a driver needed to mount the system volume lives in `init.pkg`, and
			// `system-manifest` refuses a boot-critical driver staged anywhere else.
			let Some(entry) = registry_entry(&info) else {
				i += 1;
				continue;
			};
			if !entry.boot_critical || nodes.len() >= MAX_NODES_IN_FLIGHT {
				i += 1;
				continue;
			}
			let Some(elf) = package.lookup(entry.artifact) else {
				i += 1;
				continue;
			};
			let mut node = Node::new(i, &info, alloc::vec![entry]);
			// A BOOT-CRITICAL DRIVER DECLARING A REQUIREMENT WOULD BE A CYCLE, and `system-manifest`
			// refuses one - but the gate is asked here anyway rather than assumed, because "no
			// boot driver has requirements today" is a fact about the manifest and not a property
			// of this code.
			// A NODE WHOSE BIND FAILED AND WHOSE TEARDOWN HAS NOT CONFIRMED IS STILL KEPT.
			//
			// The node owns the Process and Claim handles of a teardown in flight, so dropping it
			// here would drop them: the child would never be reaped, the claim would be released by
			// the last close instead of by a confirmation nobody read, and the device would go back
			// into circulation with no record of whether it was quiet. Kept, pumped, and resolved
			// like any other node in `Stopping`.
			if gate_on_requirements(&mut node, entry, catalogue) {
				// AND A NODE WAITING FOR ITS CLAIM IS KEPT, WHICH IT WAS NOT (2026-09-01). This read
				// the answer as a bool, so a device whose claim was still `Releasing` - parked in
				// `Backoff` with `retry_at` set, waiting for the standing loop to look again - was
				// dropped here and never bound at all. `begin_bind`'s own comment already said the
				// boot path no longer did that; what had changed was the state it left behind, not
				// what this did with it.
				let started = begin_bind(&mut node, &info, elf, entry.name, 0, power, console_input, device_privilege);
				if matches!(started, BindStart::Opened | BindStart::WaitingForTheClaim) || node.teardown.is_some() {
					nodes.push(node);
				}
			}
			i += 1;
		}
		// THE CENTRAL WAIT, and every disk answers into it. A driver that never reports in costs
		// its own share of the boot window and nothing else's.
		while pump(nodes, first_node, catalogue, buf) {
			// WHAT ARRIVED AND WHAT WENT AWAY, once per pass - see `settle_dependencies`.
			settle_dependencies(nodes, catalogue);
			// AND A NODE PARKED ON SOMEBODY ELSE'S RELEASE LOOKS AT THE CLAIM AGAIN.
			//
			// This is the re-read `BindStart::WaitingForTheClaim` promises, and nothing performed it
			// on this phase: the node has no binding and no teardown, so it produces no event and no
			// `Step`, and the only retry arm below consumes a `Step::Again` a passive node cannot
			// answer with. A boot device briefly held by somebody else's teardown was therefore
			// kept, waited for, and never bound. Bounded by the kernel's latched release deadline -
			// `observe_claim` answers `Terminal` once it passes, which ends the node rather than
			// parking it again.
			for at in first_node..nodes.len() {
				if !nodes[at].waiting_for_claim || clock() < nodes[at].retry_at {
					continue;
				}
				let Some(entry) = nodes[at].candidates.get(nodes[at].candidate).copied() else { continue };
				let Some(elf) = package.lookup(entry.artifact) else { continue };
				let info = nodes[at].info;
				begin_bind(&mut nodes[at], &info, elf, entry.name, 0, power, console_input, device_privilege);
			}
			for at in first_node..nodes.len() {
				let name: &[u8] = nodes[at].driver_name();
				match advance(&mut nodes[at], name, catalogue) {
					Step::Waiting => {}
					Step::Online => {
						// PUBLISHED, NOT ROUTED. Everything a committed binding offered goes into
						// the catalogue with an identity this service minted; who gets which is a
						// question asked once below, over everything that came up, rather than at
						// the moment each driver happened to answer.
						nodes[at].offers.close_all();
					}
					// A BOOT DRIVER HAS ONE CANDIDATE, so the two rollback answers differ only in
					// whether another attempt follows. `Again` re-opens the same entry; the rest is
					// terminal for this device and the boot goes on without that disk.
					Step::Again => {
						// NOT BEFORE ITS BACKOFF HAS PASSED. The delay used to be a sleep inside
						// `advance`; it is now a deadline, and honouring it is the caller's job.
						wait_out_backoff(&nodes[at]);
						let entry = nodes[at].candidates[nodes[at].candidate];
						let Some(elf) = package.lookup(entry.artifact) else { continue };
						let info = nodes[at].info;
						begin_bind(&mut nodes[at], &info, elf, entry.name, 0, power, console_input, device_privilege);
					}
					// RESTING IS NOT A VERDICT THIS PHASE ACTS ON either: a boot driver an operator
					// disabled, or one parked on a provider that went away, keeps its cursor and is
					// revived by whatever asked for the stop. See `Step::Resting`.
					Step::NextCandidate | Step::Done | Step::Resting => {}
				}
			}
		}
		// WHO GETS WHICH DISK, asked once over everything that came up rather than as each driver
		// answered. The first virtio-blk disk is the writable system volume; a second is routed up
		// as the read-only FAT media volume, a third as the ISO9660 volume, a fourth as the UDF
		// volume.
		//
		// STILL BY ARRIVAL ORDER, AND THAT IS THE NEXT ITEM'S SUBJECT, not this one's. What has
		// changed is that the order is now a decision made in one place over a catalogue, instead
		// of four named variables filled by whichever driver finished first - which is what has to
		// be true before a ROLE can replace it.
		report_catalogue(catalogue, b"after the boot devices");
		// ONE LOOP OVER THE TAGS THE WIRE HAS, taking by lowest bus address. The count comes from
		// the wire's own list; nothing here decides how many disks a machine may have.
		// ONE PROBE CONNECTION PER BLOCK PROVIDER, MINTED BEFORE THE ROLES TAKE THEIRS.
		//
		// The system volume was whichever block provider came first by bus address: a paired volume
		// at a later address was never considered, and a machine whose first disk is not the system
		// one had no system volume at all. The PROBE belongs to StorageService - it is what parses a
		// LiberFS superblock - and what it was missing is anything to probe.
		//
		// MINTED, NOT DUPLICATED. A duplicate of a role's channel shares that role's reply queue,
		// which is the failure the whole per-consumer factory exists to prevent; these are fresh
		// connections through `CONNECT`, so the instance that probes them competes with nobody.
		// Taken first because `take` moves the offered channel and this asks the same binding.
		for (at, slot) in probe_blocks.iter_mut().enumerate() {
			let Some(found) = catalogue.entries.iter().enumerate().filter(|(_, entry)| entry.as_ref().is_some_and(|held| held.kind == driver_protocol::provider::BLOCK)).map(|(index, _)| index).nth(at) else {
				break;
			};
			*slot = mint_connection(catalogue, nodes, found);
		}
		// GLOBAL ON PURPOSE, unlike the takes in `route_offers`: this caller is choosing AMONG every
		// block provider the boot found, by bus address, which is what `take` is for. See `take_from`
		// for the other case, where one driver's own offers are being routed.
		for slot in boot_blocks.iter_mut() {
			*slot = catalogue.take(driver_protocol::provider::BLOCK);
		}
		// A DISK THE WIRE HAS NO TAG FOR IS SAID, not dropped. It is published in the catalogue and
		// answerable through the provider interface; what it does not have is a mount, because the
		// boot hand-off names four volumes.
		let left = catalogue.count_of(driver_protocol::provider::BLOCK).saturating_sub(BOOT_BLOCK_TAGS.len());
		if left > 0 {
			print(b"DeviceManager: this machine has more block providers than the boot hand-off has volumes for; the extra ones are published and unmounted\n");
		}
	}
}

// Phase 2: now that the system volume is mounted, load each non-bootstrap
// driver from vol://system/drivers/ through the StorageService client `storage` and spawn
// it with its device's MMIO capability. Their control / event channels are handed back for
// NetworkService, ConsoleService, AudioService, InputService and the USB StorageService
// instance, plus the xHCI driver's USB bus query channel (for the `lsusb` inventory) and
// its pointer-event channel (a USB pointing device, folded by InputService), and the
// merged raw-key consumer fed by every keyboard driver.
// Tracks each device's state and prints a summary.
#[allow(clippy::too_many_arguments)]
unsafe fn launch_volume_drivers(storage: u64, catalogue: &mut Catalogue, nodes: &mut Vec<Node>, power: u64, console_input: u64, device_privilege: u64, buf: &mut [u8], net_client: &mut u64, input_client: &mut u64, usb_client: &mut u64, usbq_client: &mut u64, usb_pointer: &mut u64, raw_keys: &mut u64, recovery: &mut Recovery, #[cfg(feature = "development")] dev: &mut DevAgent) {
	unsafe {
		let (key_producer, key_consumer): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return,
		};
		*raw_keys = key_consumer;
		let count: u64 = device_count();
		// per-device state, sized by what the kernel actually discovered - the bus is
		// the only bound, never an artificial cap that would silently skip devices.
		let mut state: Vec<u8> = alloc::vec![STATE_UNKNOWN; count as usize];
		// ONE NODE PER DEVICE WITH SOMETHING TO TRY, all in flight together.
		//
		// This used to be a loop that bound one device at a time, each waiting out its own
		// handshake. The devices are independent - a sound card has nothing to say about a network
		// card - and the only thing that made them sequential was where the wait was written.
		let first_node: usize = nodes.len();
		let mut i: u64 = 0;
		while i < count {
			let idx: usize = i as usize;
			let mut info: DeviceInfo = DeviceInfo::default();
			if !device_info(i, &mut info) {
				i += 1;
				continue;
			}
			let candidates = registry_candidates(&info);
			if candidates.is_empty() {
				// Present and unsupported. Recorded rather than skipped: an unbound device is a
				// fact about this system, and leaving it out of the summary is what made it
				// indistinguishable from a driver that failed to load.
				state[idx] = STATE_UNBOUND;
				i += 1;
				continue;
			}
			if candidates[0].boot_critical {
				// bound in phase 1, before there was a volume to load from; count them as online.
				state[idx] = STATE_ONLINE;
				i += 1;
				continue;
			}
			if nodes.len() >= MAX_NODES_IN_FLIGHT {
				i += 1;
				continue;
			}
			state[idx] = STATE_FAILED;
			nodes.push(Node::new(i, &info, candidates));
			i += 1;
		}
		// Open the first candidate on every node. A device whose first candidate cannot even be
		// opened falls through to the next one in the loop below, exactly as it did when this was
		// sequential.
		for at in first_node..nodes.len() {
			start_candidate(&mut nodes[at], storage, key_producer, power, console_input, device_privilege, catalogue, &mut state);
		}
		while pump(nodes, first_node, catalogue, buf) {
			// WHAT ARRIVED AND WHAT WENT AWAY - see `settle_dependencies`. A node it wakes asks for
			// one attempt the same way an operator's retry does, and this is where that is spent:
			// the node is `Unbound` and the bind that starts it is this phase's.
			settle_dependencies(nodes, catalogue);
			for at in first_node..nodes.len() {
				if nodes[at].restart_requested && nodes[at].record.state == BindingState::Unbound {
					nodes[at].restart_requested = false;
					start_candidate(&mut nodes[at], storage, key_producer, power, console_input, device_privilege, catalogue, &mut state);
				}
				// AND THE OTHER REASON A NODE IS WAITING TO BE LOOKED AT AGAIN - see
				// `Node::waiting_for_claim`. The same re-read phase one performs, reached through
				// the ordinary candidate path because this phase has the volume to read from.
				if nodes[at].waiting_for_claim && clock() >= nodes[at].retry_at {
					start_candidate(&mut nodes[at], storage, key_producer, power, console_input, device_privilege, catalogue, &mut state);
				}
			}
			for at in first_node..nodes.len() {
				let name: &[u8] = nodes[at].driver_name();
				match advance(&mut nodes[at], name, catalogue) {
					Step::Waiting => {}
					Step::Online => {
						state[nodes[at].index as usize] = STATE_ONLINE;
						#[cfg(feature = "development")]
						route_offers(&mut nodes[at], catalogue, name, storage, console_input, net_client, input_client, usb_client, usbq_client, usb_pointer, dev);
						#[cfg(not(feature = "development"))]
						route_offers(&mut nodes[at], catalogue, name, net_client, input_client, usb_client, usbq_client, usb_pointer);
					}
					// The same candidate, once more. The window and the attempt budget have already
					// said there is room for it.
					Step::Again => {
						wait_out_backoff(&nodes[at]);
						start_candidate(&mut nodes[at], storage, key_producer, power, console_input, device_privilege, catalogue, &mut state);
					}
					// EACH CANDIDATE IN TURN, most specific first, and the next one only after the
					// last is gone - which the rollback has just guaranteed. Every rejection is
					// said: which driver, and why. A fallback that quietly succeeds hides the fact
					// that the preferred driver did not, which is how a machine comes to run on its
					// second choice for months with nobody aware of it.
					Step::NextCandidate => {
						print(b"DeviceManager: ");
						print(name);
						print(b" did not bind; its resources are released and the next candidate follows\n");
						spend_candidate(&mut nodes[at]);
						// THE SAME RULE IN THE OTHER HANDLER - see `Node::retry_once`. Both advance
						// the cursor and both start the next entry, so a one-shot honoured in only
						// one of them is a one-shot that depends on which loop the node was in.
						if core::mem::take(&mut nodes[at].retry_once) {
							print(b"DeviceManager: the attempt an operator asked for is spent; the next candidate is not started automatically\n");
						} else if nodes[at].candidate < nodes[at].candidates.len() {
							start_candidate(&mut nodes[at], storage, key_producer, power, console_input, device_privilege, catalogue, &mut state);
						}
					}
					Step::Done | Step::Resting => {}
				}
			}
		}
		// KEPT, NOT CLOSED - see `Recovery`. A driver that crashes after coming online is rebound by
		// the standing loop, and an input driver's key sink is minted from this end.
		*recovery = Recovery { storage, key_producer, state: core::mem::take(&mut state) };
		report_state(&state);
		report_catalogue(catalogue, b"after every device");
	}
}

// A DRIVER'S NAME AS THE REPORT NAMES IT, not as the manifest keys it.
//
// The registry identifier is `virtio_blk`; every driver's own line, the kernel's device inventory and
// `lsdev` all say `virtio-blk`. DeviceManager printed the raw identifier in its failure, restart and
// give-up lines, so one boot could name one driver two ways - a driver that fails once and later
// binds appeared under both spellings, and a reader matching them up has to know that the two are one
// thing. One report, one name.
unsafe fn print_driver_name(name: &[u8]) {
	unsafe {
		let mut out = [0u8; 64];
		let n = name.len().min(out.len());
		for (at, byte) in name[..n].iter().enumerate() {
			out[at] = if *byte == b'_' { b'-' } else { *byte };
		}
		print(&out[..n]);
	}
}

// Read this node's current candidate off the volume and open a bind with it.
//
// THE ELF IS UNMAPPED THE MOMENT THE SPAWN IS DONE WITH IT. `sys_process_load` copies the whole
// image into a kernel buffer before the loader touches it, so the mapping is needed for the
// duration of one syscall - not, as it was, for the whole handshake. That is what makes a dozen
// devices coming up at once cost one mapping at a time rather than a dozen.
#[allow(clippy::too_many_arguments)]
unsafe fn start_candidate(node: &mut Node, storage: u64, key_producer: u64, power: u64, console_input: u64, device_privilege: u64, catalogue: &Catalogue, state: &mut [u8]) {
	unsafe {
		// A STORED DISABLE PARKS THE NODE; IT DOES NOT SPEND A CANDIDATE.
		//
		// The refusal used to be made inside `begin_bind`, which answers `false` for it - the same
		// answer a claim that was refused and a spawn that failed give. The caller reads that as
		// "this candidate did not work" and moves to the next one, so one policy-disabled recovery
		// walked the whole candidate list and left `node.candidate` past its end. A later enable then
		// reached `Unbound` with nothing left to launch: the persistence fix would have made the
		// device unrecoverable.
		//
		// A policy disable is a property of the NODE, not of any one candidate, so it is answered
		// here - before the list is consulted at all - and the cursor is untouched.
		if node.disabled_by_policy {
			node.record.move_to(BindingState::Disabled, None);
			return;
		}
		// WHETHER THE LAST THING THAT WENT WRONG WAS A MISSING ARTIFACT, so exhaustion can say which
		// of the two failures it was. See the block at the top of the loop.
		let mut missing = false;
		loop {
			if node.candidate >= node.candidates.len() {
				// CANDIDATES EXHAUSTED IS A TERMINAL STATE, AND IT WAS NOT BEING RECORDED.
				//
				// A `read_driver` failure updated only the old byte-state array and moved on, so a
				// node whose every candidate was absent from the volume stayed `Unbound` with no
				// cause on the externally served record - and `driver-missing` is a cause M1 names
				// precisely so an operator can tell a packaging fault from a driver that ran and
				// failed. Recorded here rather than at each miss, because "this one is not there" is
				// only a failure of the NODE once there is nothing else to try.
				if missing && node.record.state == BindingState::Unbound {
					node.record.move_to(BindingState::Failed, Some(FailureCause::DriverMissing));
				}
				return;
			}
			let entry = node.candidates[node.candidate];
			let driver_name: &[u8] = entry.name;
			// PARKED RATHER THAN LAUNCHED AND FAILED. Asked at every attempt, including a backoff
			// expiry, because a requirement can go away during a teardown and the event that said
			// so is spent by the time the delay ends.
			if !gate_on_requirements(node, entry, catalogue) {
				return;
			}
			let Some((file, mapped, size)) = read_driver(storage, driver_name) else {
				// The registry names an artifact the volume does not have. That is the image
				// disagreeing with itself, not a driver that ran and failed, and the two are worth
				// telling apart: one is a packaging fault and the other is a bug.
				state[node.index as usize] = STATE_DRIVER_MISSING;
				missing = true;
				print(b"DeviceManager: ");
				print_driver_name(driver_name);
				print(b" is named by the registry and not on the volume; trying the next candidate\n");
				node.candidate += 1;
				// AN OPERATOR'S BUDGET SURVIVES A CANDIDATE THAT NEVER STARTED - see
				// `Node::retry_once` (2026-09-01). Resetting here unconditionally handed the NEXT
				// candidate a full automatic budget, so one `retry` on a device whose first artifact
				// is missing opened three attempts on the second. Nothing ran, so the operator's one
				// attempt is not spent; what must not happen is it turning back into the automatic
				// budget it was set one below.
				if !node.retry_once {
					node.attempt = 0;
				}
				continue;
			};
			let elf: &[u8] = core::slice::from_raw_parts(mapped as *const u8, size);
			let info = node.info;
			let started = begin_bind(node, &info, elf, driver_name, key_producer, power, console_input, device_privilege);
			unmap_object(file);
			close(file);
			// A TEARDOWN IN FLIGHT IS NOT A CANDIDATE THAT FAILED YET. Moving to the next entry here
			// would start a second bind on a device whose first one has not been given back - the
			// claim not confirmed `Free`, the child not confirmed dead. The central wait resolves it
			// and answers `NextCandidate` when it has, which is where the candidate advances.
			//
			// AND NEITHER IS A CLAIM SOMEBODY ELSE IS STILL RELEASING (2026-09-01). That answer used
			// to be the same `false` as a refusal, so every pass over a `Releasing` device spent one
			// more candidate; once the cursor ran off the end the standing loop's retry started
			// nothing, because there was nothing left to start. A device briefly held by a teardown
			// could therefore exhaust its whole candidate list and never bind - a transient state
			// made permanent. The cursor stays where it is and the node waits in `Backoff`.
			if matches!(started, BindStart::Opened | BindStart::WaitingForTheClaim) || node.teardown.is_some() {
				return;
			}
			// It could not even be opened - the claim refused, the spawn refused. `begin_bind` has
			// already put the node where the table says that belongs, so the only question left is
			// whether there is another candidate to try.
			node.candidate += 1;
			// The same rule as the missing-artifact advance above: a candidate that could not be
			// STARTED does not spend an operator's one attempt, and must not restore the automatic
			// budget either. The flag itself is consumed where an attempt actually ends - the two
			// `Step::NextCandidate` handlers - and when a bind comes online.
			if !node.retry_once {
				node.attempt = 0;
			}
		}
	}
}

// ROUTED BY WHAT THE PROVIDER IS, not by which driver sent it and in what order.
//
// Every one of these used to be "the handle that came with the report", with the xHCI driver's
// extra two told apart by the literal bytes `USBBUS` and `POINTER` in the messages that followed -
// so what a capability was for was decided by parsing a string the driver chose.
#[allow(clippy::too_many_arguments)]
unsafe fn route_offers(node: &mut Node, catalogue: &mut Catalogue, driver_name: &[u8], #[cfg(feature = "development")] storage: u64, #[cfg(feature = "development")] console_input: u64, net_client: &mut u64, input_client: &mut u64, usb_client: &mut u64, usbq_client: &mut u64, usb_pointer: &mut u64, #[cfg(feature = "development")] dev: &mut DevAgent) {
	unsafe {
		let _ = driver_name;
		// PUBLISHED FIRST, ROUTED SECOND. Everything this binding offered enters the catalogue with
		// an identity this service minted; what follows takes from the catalogue rather than from
		// the driver's own message, so a provider that nothing routes is still a provider the
		// machine has - and is withdrawn with its binding rather than leaked.
		if *net_client == 0 {
			*net_client = catalogue.take_from(node.id, driver_protocol::provider::NET);
		}
		// AND THE DISPLAY IS NOT ROUTED ANY MORE EITHER (2026-09-02). It was taken into a slot of
		// this program's and handed down the boot chain to DisplayService - the same per-kind
		// injection as the audio one below, and the reason a rebound GPU driver could not restore a
		// picture: the slot was filled once and the replacement provider had nowhere to go.
		// DisplayService now SUBSCRIBES, so the publication stays in the catalogue with its offered
		// channel intact and `open` hands that channel to whichever consumer asks - at boot and
		// after a rebind, down one path.
		//
		// AND AUDIO IS NOT ROUTED AT ALL ANY MORE, which is the point (2026-08-31).
		//
		// It was taken into a slot of this program's and handed down the boot chain to
		// AudioService - the per-kind injection this milestone exists to replace. AudioService now
		// SUBSCRIBES: it holds a `provider-catalogue` connection, asks for the audio kind, and opens
		// a connection to whichever provider that answers with or arrives later. So the publication
		// stays in the catalogue with its offered channel intact, and `open` hands that channel to
		// the first consumer - which is also what makes the late-publication case work, since a
		// sound card bound after the service started reaches it down the same subscription.
		//
		// The other kinds still route; each is its own seam and moves on its own.
		// The development channel driver hands up a raw byte channel, and the agent that speaks the
		// protocol over it is started here rather than by ServiceManager. It exists exactly when the
		// device does, it has no other client, and its whole reason to be a separate process is to
		// keep the artifact registry out of the address space that holds a device capability - so it
		// is started where that device is bound, and nowhere else.
		#[cfg(feature = "development")]
		{
			let dev_bytes: u64 = catalogue.take_from(node.id, driver_protocol::provider::CONSOLE_BYTES);
			if driver_name == b"dev_channel" && dev_bytes != 0 {
				// The driver's bootstrap is kept rather than left to leak, because a replacement
				// agent's wire is handed down over it; and a volume connection of this program's own
				// is opened, because the one these drivers were read through is closed as soon as
				// the caller returns.
				if let Some(binding) = &node.binding {
					dev.driver = binding.channel;
				}
				dev.storage = service_connect(storage).unwrap_or(0);
				// INSECURE by name, because that is what this is: an identifier that tells one boot
				// from another, not a secret. Asking for the secure one would refuse on every
				// machine with no hardware random source - which is two of the three architectures -
				// for a number that never needed to be unguessable.
				random_insecure(&mut dev.nonce);
				dev.console_input = console_input;
				dev.bootstrap = start_dev_agent(dev.storage, dev_bytes, console_input, &dev.nonce);
				if dev.bootstrap == 0 {
					print(b"DeviceManager: development agent did not start; the control channel is transport-only\n");
				}
			} else if dev_bytes != 0 {
				close(dev_bytes);
			}
		}
		// The pointer flavour of virtio_input offers an INPUT provider; the keyboard flavour offers
		// none, so an absent one is a state rather than a failure.
		if *input_client == 0 {
			*input_client = catalogue.take_from(node.id, driver_protocol::provider::INPUT);
		}
		// The xHCI driver offers up to three: the USB stick's block service (absent when no
		// mass-storage device is attached), its bus query channel for the `lsusb` inventory, and a
		// pointer-event channel for a USB pointing device. All three arrived in ONE handshake and
		// were held unpublished until its `READY`, so a controller that died between them published
		// nothing.
		if *usb_client == 0 {
			*usb_client = catalogue.take_from(node.id, driver_protocol::provider::BLOCK);
		}
		if *usbq_client == 0 {
			*usbq_client = catalogue.take_from(node.id, driver_protocol::provider::USB_BUS);
		}
		if *usb_pointer == 0 {
			*usb_pointer = catalogue.take_from(node.id, driver_protocol::provider::POINTER);
		}
		// ANYTHING STILL HELD IS A PROVIDER NOBODY ROUTES, and it is closed rather than leaked: a
		// handle this service keeps forever is a channel the driver waits on forever.
		node.offers.close_all();
	}
}

// Start the development agent on the byte channel the development channel driver handed up.
// The channel becomes the agent's bootstrap: raw port bytes arrive on it and whole protocol
// frames go back, so the driver never learns what a frame is. The agent reports in once, and
// a failure to start is reported rather than retried - a development instance without its
// agent is still a usable guest, just one whose control channel carries nothing.
#[cfg(feature = "development")]
unsafe fn start_dev_agent(storage: u64, bytes: u64, console_input: u64, nonce: &[u8; 8]) -> u64 {
	unsafe {
		let loaded: Option<(u64, u64, usize)> = read_driver(storage, b"dev_agent");
		let (file, mapped, size): (u64, u64, usize) = match loaded {
			Some(t) => t,
			None => return 0,
		};
		let elf: &[u8] = core::slice::from_raw_parts(mapped as *const u8, size);
		// The agent gets a bootstrap of its own and the byte channel transferred over it. The
		// byte channel is the wire: anything sent on it goes out of the port, so it is not a
		// place to report in, and DeviceManager must never read from it either - every byte on
		// it belongs to the agent.
		let (dm_side, agent_side): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => {
				unmap_object(file);
				close(file);
				return 0;
			}
		};
		let started: bool = spawn(elf, agent_side) >= 0;
		unmap_object(file);
		close(file);
		// The wire and the boot's identity in one message: the identity is not the agent's to
		// draw, since it outlives any one agent, and one message leaves no order to get wrong.
		let mut opening: [u8; 13] = [0u8; 13];
		opening[..5].copy_from_slice(b"BYTES");
		opening[5..].copy_from_slice(nonce);
		if !started || !send_blocking(dm_side, &opening, bytes) {
			return 0;
		}
		// A volume connection of its own, so the agent can read the installed artifact a
		// publication would shadow. A fresh connection rather than a duplicate of
		// DeviceManager's handle: a volume client is a request and reply channel, and two
		// readers on one endpoint would take each other's replies.
		let connection: u64 = match service_connect(storage) {
			Some(connection) => connection,
			None => return 0,
		};
		if !send_blocking(dm_side, b"STORAGE", connection) {
			return 0;
		}
		// THE CONSOLE, WHICH THE AGENT WAITS FOR AND NOBODY WAS SENDING. `dev_agent` reads three
		// things from its bootstrap - the wire, a volume client, and this - and `recv_tagged`
		// BLOCKS. Sending two of the three left the agent waiting for a message nobody would send,
		// which left this function waiting for a report the agent would never make, which left
		// ServiceManager waiting for a phase-2 answer: one missing handoff stopped the whole boot,
		// silently. It is the ordered-bootstrap hazard this tree has written down three times.
		//
		// SENT WHETHER OR NOT THERE IS ONE. A zero handle is how "there is none" is said, and the
		// agent already reads it that way - it works without a console and cannot type into it.
		// What must never vary is the NUMBER of messages, because their order is the protocol.
		let feed: u64 = if console_input != 0 {
			let copy: i64 = duplicate(console_input, RIGHT_TRANSFER);
			if copy < 0 { 0 } else { copy as u64 }
		} else {
			0
		};
		if !send_blocking(dm_side, b"CONSOLE", feed) {
			return 0;
		}
		// The agent reports in before it serves, so a start that loaded but never ran is not
		// mistaken for a working one. The bootstrap stays open afterwards: dropping it is how
		// the agent would learn to shut down.
		//
		// AND THE WAIT HAS AN END. It used to be `recv_blocking`, which is a wait with no end, on
		// the boot's critical path, for the one program in this tree that nothing builds. When that
		// program was missing a single bootstrap handoff, the machine did not boot and no service
		// reported a failure - because none had failed, they were all waiting here. A start that
		// does not finish now costs the agent and nothing else.
		//
		// Not a retry and not a watchdog: an agent that dies LATER is `DevAgent::restart`'s job and
		// is unchanged. This is one bounded wait with a number on it.
		let mut buf: [u8; 64] = [0u8; 64];
		let deadline: u64 = clock() + DEV_AGENT_REPORT_TICKS;
		loop {
			match try_recv(dm_side, &mut buf) {
				Polled::Message { len, .. } if len >= 5 && &buf[..5] == b"agent" => {
					print(&buf[..len]);
					print(b"\n");
					// Kept, not dropped: this is how the launcher reaches the agent later, and how
					// the agent would learn to shut down.
					return dm_side;
				}
				Polled::Empty if clock() < deadline => {
					// PERIODIC deliberately. A plain timed wait counts as pending progress, and the
					// kernel then halts until the deadline whenever the run queue empties - which
					// would park the very process this is waiting for. This wait is a guard, not
					// progress; the kernel still wakes it when due.
					let waited: i64 = wait_periodic(dm_side, deadline);
					// A wait that cannot be done at all must not become a spin until the deadline:
					// burning a boot CPU until it expires is worse than the failure being survived.
					if waited < 0 && waited != ERR_TIMED_OUT {
						break;
					}
				}
				// Out of time, or a first message that is not a report. Both say the agent did not
				// start, and abandoning it means CLOSING its bootstrap: an agent whose bootstrap is
				// gone stops, where one left holding a wire nobody reads would hold the port for as
				// long as the boot lasts.
				_ => break,
			}
		}
		// The caller says what the loss COSTS ("the control channel is transport-only") for every
		// way this can fail; this line says which way it was.
		print(b"DeviceManager: the development agent did not report in; it is abandoned\n");
		close(dm_side);
		0
	}
}

// Open a manifest-declared driver path through the StorageService client and map its bytes,
// returning (file handle, mapped address, size) so the caller can spawn from the image and
// then release the mapping. None if the driver cannot be read.
unsafe fn read_driver(storage: u64, name: &[u8]) -> Option<(u64, u64, usize)> {
	unsafe {
		let name = core::str::from_utf8(name).ok()?;
		let opts: OpenOpts = OpenOpts { path: alloc::string::String::from(program_path(name)?), write: false, create: false };
		let result = match volume::Client::new(ChannelTransport { chan: storage }).open(&opts) {
			Some(Ok(r)) => r,
			_ => return None,
		};
		if result.file == 0 || result.size == 0 {
			if result.file != 0 {
				close(result.file);
			}
			return None;
		}
		let mapped: u64 = match map_object(result.file) {
			Some(base) => base,
			None => {
				close(result.file);
				return None;
			}
		};
		Some((result.file, mapped, result.size as usize))
	}
}

// ------------------------------------------------- the node, and its ordered queue
//
// A BINDING IS NOT THE THING THAT OUTLIVES ITS OWN EVENTS.
//
// The exit of a driver arrives while its binding is being torn down, and the next binding's `READY`
// arrives on the same device afterwards - so a queue owned by a binding has nowhere to put the
// events on either side of it, and two consecutive bindings would each hold a queue with the
// interesting moment falling between them. The queue belongs to a DEVICE NODE, which exists from
// the boot scan onward and holds `Option<Binding>`.
//
// The record used to be a local inside the bring-up loop and died with the iteration that made it.
// Where a device's binding is, and why, is a fact about the device - so it lives here, for the life
// of this program.

// The event kinds and the queue itself are `driver_binding`'s: a ring buffer with a generation
// filter inside this binary is one nobody can drive on a host, and "an exit racing a restart" is
// exactly the case that has to be driven rather than reasoned about. Same argument as
// `BindingState`, same crate.

// What is bound to a node right now: everything the manager holds on the driver's behalf.
//
// THE BINDING OWNS THE PROCESS HANDLE. `spawn` used to return it and nothing kept it, so nothing
// could end a process a failed bind had started - which is what made "a failed bind leaves nothing
// behind" untrue in the one case it is written for.
struct Binding {
	// The child Domain this driver runs in, and everything it allocates is charged to. Zero only
	// where creating one failed - see `begin_bind`.
	domain: u64,
	process: u64,
	channel: u64,
	claim: u64,
	key: ClaimKey,
}

impl Binding {
	// The same holdings, as the ledger a rollback undoes.
	//
	// ONE ROLLBACK IMPLEMENTATION, NOT TWO. A bind that fails before the binding is installed and
	// one that fails after it hold exactly the same four things, and the order they are given back
	// in is the property that matters - so a second implementation would be a second order, and the
	// two would agree until the day somebody changed one of them.
	// An INSTALLED binding holds no untransferred resources: everything the bind assembled either
	// reached the driver or the attempt was rolled back before this existed. So the two fields
	// `Attempt` grew for the pre-commit paths are empty here, and that is a statement about when this
	// conversion happens rather than an omission.
	fn into_attempt(self) -> Attempt {
		Attempt { held: driver_binding::Holdings::installed(self.domain, self.process, self.channel, self.claim), key: self.key }
	}
}

// WHAT A RESTART AFTER `Online` NEEDS, kept for the life of this program.
//
// The standing loop calls `advance` and used to DISCARD its `Step`. So a driver that crashed after
// coming online had its binding removed, its record moved to `Backoff` - and nothing ever called
// `start_candidate` again, because the bring-up loops that handle `Step::Again` have long returned by
// then. The node sat in `Backoff` for the rest of the boot. Acting on that answer needs the same
// three things phase two had and then dropped.
//
// THE KEY PRODUCER IS DELIBERATELY NOT CLOSED any more. Phase two closed it after handing each input
// driver a duplicate; a restarted `virtio_input` or `xhci` needs another, and minting one requires an
// end this program still holds. The cost is that InputService's consumer no longer sees the channel
// close when every input driver has died - which was never the signal it acts on, because a dead
// driver reaches it through this manager rather than through an EOF on the raw stream.
struct Recovery {
	storage: u64,
	key_producer: u64,
	state: Vec<u8>,
}

impl Recovery {
	fn none() -> Self {
		Self { storage: 0, key_producer: 0, state: Vec::new() }
	}

	fn armed(&self) -> bool {
		self.storage != 0
	}
}

// One device, for the life of this program.
struct Node {
	// THE FUNCTION THIS NODE IS, which is its BDF and nothing else. Its generation moves with each
	// binding; `id.generation` is 0 for a node that has never been claimed.
	id: BindingId,
	// The kernel's row number for the same function, which is a LOOKUP and not part of the
	// identity - `ClaimKey` addresses a binding by it, and one name that grows a field for every
	// table that wants to find it is not a name.
	index: u64,
	// What the kernel reported about this function. Held on the node because a rebind needs it and
	// a parallel array indexed the same way is one that can fall out of step.
	info: DeviceInfo,
	// Where this node's binding is and why it got there. Survives every binding on this node.
	record: BindingRecord,
	// What is bound now, if anything.
	binding: Option<Binding>,
	// The providers the current handshake has offered, held unpublished until it says `READY`.
	offers: Offers,
	// The window this bring-up incident may spend, opened when the first attempt begins.
	incident: Incident,
	// How many automatic attempts this incident has spent.
	attempt: u32,
	// The registry candidates left to try, most specific first, and which one is being tried.
	candidates: Vec<&'static Entry>,
	candidate: usize,
	// WHICH CANDIDATE THE LIVE BINDING IS ACTUALLY RUNNING - latched when the bind commits, cleared
	// when it ends (added 2026-08-31).
	//
	// `candidate` is a CURSOR: where the next bind starts. It was also being read as "which driver
	// this node is running", and those are the same value only until an operator moves the cursor.
	// `select` moves it deliberately - that is the whole verb - and its own comment promised it
	// "touches neither the record nor the live driver", which three readers made untrue: the served
	// binding record's `artifact`, the dependency rules `settle_dependencies` applies, and the
	// `provides` declaration a late `OFFER` from the RUNNING driver is published against. So
	// selecting a future driver renamed the running one, applied the wrong driver's `requires` to it
	// - which can STOP a driver whose own requirements are met - and published its providers against
	// a declaration it never made.
	//
	// Two facts, so two fields. Nothing reads this when there is no binding; `entry()` is where the
	// choice between them is made once.
	running: Option<usize>,
	// WHICH ENTRY THE BINDING THAT JUST ENDED WAS, latched when the binding is taken out of the node.
	//
	// `running` is cleared the moment the rollback starts, and the verdict that spends a candidate
	// arrives later - after the child's exit and the claim have both confirmed. So the advance had
	// only the cursor to work from, and the cursor is not a description of what ran: `select` moves
	// it to the entry the NEXT bind must start from, and can be asked while a different entry is
	// online. Advancing from there stepped over the operator's choice before anything had tried it.
	// See `spend_candidate`.
	spent: Option<usize>,
	// AN OPERATOR'S SELECTION HAS BEEN MADE AND NOT YET STARTED FROM. See `spend_candidate`, which
	// leaves the cursor alone while this is set, and `begin_bind`, which clears it when an attempt
	// starts. One bind, which is what "applies at the next bind" means.
	selection_pending: bool,
	// THE OPERATOR'S CHOICE, IF THERE IS ONE - an index into `candidates`.
	//
	// The cursor above is where the next bind STARTS; this is where it starts BY PREFERENCE. They are
	// the same value until something moves the cursor, and the difference only matters on the paths
	// that rewind it: a retry after exhaustion used to rewind to zero and hand an operator the
	// registry order instead of the driver they selected, because the preference existed only as a
	// stored string nothing reread until the next boot. Set by the live `select` verb and by
	// `load_stored_policy`, which are the two places a preference can come from.
	preferred: Option<usize>,
	// One ordered queue. Two nodes are independent; one node never handles two events at once.
	queue: BindingQueue,
	// The heartbeat, armed when this node comes `Online` and only for an entry that declared a
	// deadline.
	beat: Heartbeat,
	// The last frame this node's driver sent, and when. Two fields, kept because a capture taken
	// after the process is gone cannot ask the process anything.
	last_opcode: u16,
	last_frame_at: u64,
	// WHICH of the chosen entry's rules matched this device. See `BindingRecord.rule`.
	matched_rule: u32,
	// How many resources the current bind granted: the MMIO window, an interrupt where the entry
	// takes one, and whatever else it asked for.
	granted_resources: u32,
	// Why this node was asked to stop, if it was. `Fault` for everything that was not asked.
	stop_intent: driver_binding::StopIntent,
	// What the last teardown found, if there has been one. Rendered by P02M0166; kept here because
	// the node outlives the binding it is about, which is the whole reason the node exists.
	// AN OPERATOR'S `retry` IS ONE ATTEMPT, AND THE ATTEMPT OUTLIVES THE CANDIDATE.
	//
	// `PolicyVerb::Retry` grants it by setting `attempt` one below the automatic bound, which stops
	// `may_try_again` opening a second one - but ONLY for the candidate it was granted against.
	// `Step::NextCandidate` advances the cursor and resets `attempt` to zero, so a device with more
	// than one candidate answered a request for one attempt with one attempt on this entry and a
	// FULL automatic budget on every entry after it. The counter cannot carry the rule because the
	// counter is per candidate; this flag is per operator request, and it is what the two
	// `NextCandidate` handlers read to stop rather than walk on.
	retry_once: bool,
	incident_report: Option<Diagnostic>,
	// WHETHER THAT REPORT HAS BEEN WRITTEN SOMEWHERE THAT OUTLIVES THIS PROGRAM. See
	// `persist_incidents`: held only here, the snapshot died with the manager that took it.
	incident_stored: bool,
	// AN OPERATOR ASKED FOR ONE MORE ATTEMPT. Consumed by the standing loop - see `PolicyVerb::Retry`.
	restart_requested: bool,
	// THE TICK THIS NODE MAY TRY AGAIN AT, 0 for "now". Set by `back_off_until` instead of sleeping,
	// so one node's delay is not every node's - see the comment there.
	retry_at: u64,
	// THIS NODE IS PARKED ON SOMEBODY ELSE'S RELEASE, and `retry_at` is when to look again.
	//
	// Different from every other reason a node sits in `Backoff`, and the difference is who ends it.
	// An ordinary backoff ends with an attempt this node makes; this one ends when the device's
	// claim reaches `Free` - or when the kernel's own latched release deadline passes and
	// `observe_claim` answers `Terminal` instead, which is what bounds the wait. Nothing else marks
	// it, so the bring-up loops had no way to tell a node that is waiting for a claim from one that
	// has nothing left to do, and `pump` reported "no work in flight" and returned for the last time
	// with the node still parked. See `BindStart::WaitingForTheClaim`.
	waiting_for_claim: bool,
	// A TEARDOWN WAITING FOR ITS CONFIRMATIONS. See `Teardown`.
	teardown: Option<Teardown>,
	// AN OPERATOR DISABLED THIS DEVICE, AND THAT IS A DESIRE RATHER THAN A STATE.
	//
	// The stored record used to be applied by attempting `move_to(Disabled)` and ignoring the
	// refusal - and the table deliberately has no `Online -> Disabled` edge, so for every eligible
	// driver already bound by the time ConfigService became available the record was read and
	// silently forgotten. The device stayed online and a later crash bound it again, against a
	// policy that was still on disk.
	//
	// Kept apart from `record.state` because the two answer different questions: the state is where
	// this binding IS, and this is what the operator asked for. An already-online device stays live
	// until its next bind - taking it down under a running system is not what a stored record asks -
	// and that next bind is refused.
	disabled_by_policy: bool,
}

// A TEARDOWN THAT HAS BEEN STARTED AND NOT YET CONFIRMED.
//
// M4 says the exit and the claim reaching `Free` ARRIVE, as events, while the node is `Stopping`,
// and that the node leaves that state when both have. The rollback was one call instead: it killed
// the process, closed the Process handle in the same breath, released the device and answered where
// the node had landed - so nothing ever observed the child actually exit, `Stopping` was a label the
// record passed through rather than a state the node was in, and the teardown deadline M5 reserves
// had nothing to apply to. A child that ignores its death and a claim that never reaches `Free` were
// indistinguishable from a clean teardown.
//
// THE HANDLES STAY OPEN UNTIL THEIR EVENT ARRIVES, which is the whole mechanism: the Process handle
// is what the exit is waited on, and the Claim handle is what a release that answered `Releasing`
// settles through. The Domain is killed LAST, after both, for the reason the old rollback gave -
// killing it first takes the process out from under a teardown still reading its handles.
struct Teardown {
	// The handles still open and what has arrived, from the crate that also owns the ordering.
	pending: driver_binding::Pending,
	// The tick both confirmations must arrive by. A teardown that misses it ends at `Quarantined`
	// with the device out of circulation, which is P02M0098 M8's rule rather than a second one.
	deadline: u64,
	// Where the record goes once BOTH have arrived and the device is confirmed `Free`. `None` is
	// `StopIntent::Shutdown`'s answer: the manager is going away and there is no next binding.
	landed: Option<BindingState>,
	cause: FailureCause,
	// Whether this teardown was a retry that intends to bind again, which is what the caller reads
	// out of `resolve_teardown` to decide between another attempt and the next candidate.
	retrying: bool,
	// Whether a PLANNED stop is what led here, so the line describing it is printed once the
	// teardown has answered rather than before it has run.
	planned_stop: bool,
	intent: driver_binding::StopIntent,
}

// WHAT WAS TRUE WHEN A BINDING WENT WRONG, taken BEFORE the process is gone.
//
// A FIXED LIST, written down so it cannot grow into a diagnostic subsystem: the binding and its
// generation, the state it was in, the cause, the last opcode received and how long ago, the
// restart count, and the Domain's counters at that moment. No device payload, no memory dump, no
// log excerpt - each of those is unbounded in a different way, and a capture whose size depends on
// what the driver was doing is one that fails on the driver that most needs capturing.
#[derive(Clone, Copy)]
struct Diagnostic {
	binding: BindingId,
	state: BindingState,
	cause: FailureCause,
	last_opcode: u16,
	// Ticks between the last frame and the capture. `u64::MAX` for a binding that never sent one,
	// which is a different fact from "a long time ago".
	silent_for: u64,
	attempts: u32,
	// The child Domain's counters. `None` where the Domain was already gone or never made, which is
	// itself worth recording rather than reporting zeros.
	domain: Option<DomainStats>,
}

// WHETHER THIS DRIVER'S CONTROL PATH IS MAKING PROGRESS.
//
// A different question from whether its device is busy, and a driver may not pet its watchdog
// through an unrelated child - which is why the answer travels on the control channel and echoes a
// number the manager chose.
// See `driver_binding::Heartbeat`: the state and its three decisions live where they can be driven,
// because the refusal tests M7 names could only compare enum variants while they lived here.
type Heartbeat = driver_binding::Heartbeat;

impl Node {
	fn new(index: u64, info: &DeviceInfo, candidates: Vec<&'static Entry>) -> Node {
		Node { id: BindingId::new(info.bus, info.dev, info.func, 0), index, info: *info, record: BindingRecord::new(), restart_requested: false, retry_at: 0, binding: None, offers: Offers::new(), incident: Incident { deadline: 0, teardown_reserve: 0 }, attempt: 0, candidates, candidate: 0, running: None, spent: None, selection_pending: false, preferred: None, queue: BindingQueue::new(), beat: Heartbeat::default(), matched_rule: 0, granted_resources: 0, stop_intent: driver_binding::StopIntent::default(), last_opcode: 0, last_frame_at: 0, retry_once: false, incident_report: None, incident_stored: false, teardown: None, waiting_for_claim: false, disabled_by_policy: false }
	}

	// Queue one event for this node.
	fn push(&mut self, event: BindingEvent) -> bool {
		self.queue.push(event)
	}

	// Take the oldest event about the binding this node is holding now. A node holding neither a
	// binding nor a teardown has nothing an event could be about, which the queue answers by
	// draining.
	//
	// THE IDENTITY IS WHAT SAYS WHICH GENERATION, not a copy of the number inside the binding. Two
	// places holding one value is two places that agree until somebody updates one of them, and the
	// one that would have been missed is the one a stale event is filtered against.
	//
	// A TEARDOWN IS STILL HOLDING A BINDING'S WORTH OF STATE, AND THIS ASKED FOR GENERATION ZERO
	// (fixed 2026-09-02). The rollback TAKES `binding` out of the node before it runs, so from the
	// moment a teardown exists this answered 0 - and `BindingQueue::pop(0)` drains the queue and
	// returns None. Its two confirmations are pushed by `pump` as ordinary events carrying
	// `id.generation`, so they were dropped on the way in and `Pending::note` never saw them: every
	// teardown that had to WAIT ran to its deadline and settled `Unconfirmed`, which lands
	// `Quarantined`. That is every planned stop in the system - an operator's disable, a dependency
	// loss, a crash with attempts left - resolved as an unconfirmed teardown, and the reason nothing
	// caught it is that the suites send `STOP` at shutdown and exit before any teardown confirms.
	//
	// The generation is right for the teardown too: `node.id` is rebound only when the NEXT bind
	// takes a claim, so while a teardown is outstanding it still names the binding that ended, which
	// is exactly what its confirmations carry.
	fn pop(&mut self) -> Option<BindingEvent> {
		let holds_a_binding: bool = self.binding.is_some() || self.teardown.is_some();
		let generation: u64 = if holds_a_binding { self.id.generation } else { 0 };
		self.queue.pop(generation)
	}

	// The driver this node is currently trying, or an empty name for a node with none left.
	// THE ARTIFACT THIS NODE IS ON, or the LAST one it tried.
	//
	// Both exhaustion paths increment `candidate` past the final entry, so a record that has failed
	// every candidate read its artifact out of bounds and served an EMPTY name - on exactly the
	// failure an operator is trying to diagnose, and on a node where registry entries had matched and
	// been attempted. The last candidate is the one the failure is about.
	fn driver_name(&self) -> &'static [u8] {
		match self.entry().or_else(|| self.candidates.last().copied()) {
			Some(entry) => entry.name,
			None => b"",
		}
	}

	// THE REGISTRY ENTRY THIS NODE IS DESCRIBED BY RIGHT NOW - the one it is RUNNING if it is running
	// anything, and otherwise the one the cursor is on.
	//
	// One place makes this choice, because the three readers that need it were each making it
	// differently by reading the cursor directly, and a cursor an operator can move is not a
	// description of a live driver. See `Node::running`.
	fn entry(&self) -> Option<&'static Entry> {
		self.candidates.get(self.running.unwrap_or(self.candidate)).copied()
	}

	// Whether this node still has work in flight - a driver that has been sent `BIND` and has not
	// reached a terminal state.
	// A NODE THE CENTRAL WAIT HAS TO KEEP LOOKING AT. A binding being brought up or stopped, or a
	// teardown whose confirmations have not arrived - the second is why this is not just the first:
	// the binding is TAKEN out of the node before the rollback runs, so a node waiting for its
	// child to exit has no binding and is very much still in flight.
	fn in_flight(&self) -> bool {
		(self.binding.is_some() && matches!(self.record.state, BindingState::Binding | BindingState::Stopping)) || self.teardown.is_some()
	}
}

// ------------------------------------------------------- one bind, one transaction
//
// BIND USED TO BE A SEQUENCE OF STEPS THAT EACH SUCCEEDED SEPARATELY: map the ELF, spawn the
// process, hand over the device, wait for a report. A failure part-way left whatever the earlier
// steps did - the claim taken, the process running, the capability handed over - and the process
// handle was DROPPED after `spawn`, so nothing could end the process a failed bind had started.
//
// This is what the attempt has taken so far, and it is filled in as each thing is taken rather than
// reconstructed afterwards. A rollback that has to work out what happened is a rollback that gets it
// wrong on the path nobody exercises.
struct Attempt {
	// EVERYTHING THIS TRANSACTION HOLDS, AND THE ORDER IT GIVES IT BACK IN, from the crate where
	// both can be driven by a test. It lived here, in a binary nothing can run on a host, so the one
	// property this milestone rests on - a bind either completes or leaves nothing behind - was
	// asserted by reading the code, and two leaked handles survived that reading.
	held: driver_binding::Holdings,
	key: ClaimKey,
}

impl Attempt {
	fn new() -> Self {
		Self { held: driver_binding::Holdings::new(), key: ClaimKey::default() }
	}

	// Record a resource this attempt has acquired and not yet handed over.
	fn holds(&mut self, kind: u16, handle: u64) {
		let _ = self.held.hold(kind, handle);
	}

	// That resource has been transferred: it is the driver's now, and a rollback must not close it.
	fn handed_over(&mut self, handle: u64) {
		self.held.handed_over(handle);
	}

	// THE TRANSACTION COMMITS: what it took stays taken, and passes to whoever asked for the bind.
	//
	// CONSUMES the value rather than zeroing its fields, which is the difference between "these
	// handles are somebody else's now" and "somebody remembered to blank three variables". There is
	// no `Drop` here to disarm - a rollback that ran on every drop would have to be disarmed on this
	// path, and a disarm that is forgotten leaks a device silently.
	fn commit(self) {}

	// M4'S STEPS 1 TO 3, over the real syscalls. The order, the ledger and what step 4 is left
	// waiting for are `driver_binding::Holdings`'s, so this is the same teardown the fault cases in
	// that crate drive.
	unsafe fn begin_teardown(&mut self, offers: &mut Offers, deadline: u64) -> Teardown {
		unsafe {
			let mut closes = Syscalls;
			// The offers go with the transaction: everything a driver announced and did not get to
			// commit is closed here, after the kill and with the rest of what the manager holds.
			let pending = {
				let mut held = core::mem::replace(&mut self.held, driver_binding::Holdings::new());
				let out = held.begin_teardown(&mut closes);
				offers.close_all();
				out
			};
			Teardown { pending, deadline, landed: None, cause: FailureCause::TeardownUnconfirmed, retrying: false, planned_stop: false, intent: driver_binding::StopIntent::Fault }
		}
	}
}

// THE ROLLBACK'S EFFECTS, AS SYSCALLS. The other implementation of this trait is the fault suite's
// recorder, which is what makes the order below something a test asserts rather than reads.
struct Syscalls;

impl driver_binding::Closes for Syscalls {
	fn kill(&mut self, process: u64) {
		// NOT A REQUEST IT CAN DECLINE, and the handle is KEPT: the exit is what confirms this.
		unsafe { signal(process, SIG_KILL) };
	}

	fn close(&mut self, handle: u64) {
		unsafe { close(handle) };
	}

	fn release(&mut self, claim: u64) -> Option<u32> {
		let outcome = unsafe { device_release(claim) };
		if outcome < 0 || outcome as u32 == CLAIM_STATE_RELEASING { None } else { Some(outcome as u32) }
	}

	fn kill_domain(&mut self, domain: u64) {
		unsafe { domain_kill(domain) };
	}
}

// ------------------------------------------------- the manager half of the wire

// The providers one driver offered during its handshake, HELD UNPUBLISHED until it says `READY`.
//
// A driver that dies half way through announcing itself announces nothing: everything collected
// here is closed on a `FAILED` or a peer-close, and only a `READY` hands it on. Held by KIND rather
// than by arrival order, which is what the manager used to route on - it read the literal bytes
// `USBBUS` and `POINTER` out of two messages that followed the report, so what a capability was for
// was decided by parsing a string a driver chose.
struct Offers {
	kinds: [u16; driver_protocol::MAX_INITIAL_OFFERS],
	// The publisher-local token each offer carried. Unique within the driver that sent it, and the
	// only name that driver has for its own publication.
	tokens: [u16; driver_protocol::MAX_INITIAL_OFFERS],
	handles: [u64; driver_protocol::MAX_INITIAL_OFFERS],
	count: usize,
}

impl Offers {
	fn new() -> Self {
		Self { kinds: [0; driver_protocol::MAX_INITIAL_OFFERS], tokens: [0; driver_protocol::MAX_INITIAL_OFFERS], handles: [0; driver_protocol::MAX_INITIAL_OFFERS], count: 0 }
	}

	// Answers false when the bound is reached, so the caller can refuse the frame and close its
	// handle rather than accumulate. "Any number" is not a bound, and a driver is a separate process
	// that may be wrong or malicious.
	fn push(&mut self, kind: u16, token: u16, handle: u64) -> bool {
		if self.count >= driver_protocol::MAX_INITIAL_OFFERS {
			return false;
		}
		self.kinds[self.count] = kind;
		self.tokens[self.count] = token;
		self.handles[self.count] = handle;
		self.count += 1;
		true
	}

	// THERE IS NO `take` HERE ANY MORE. Taking a provider straight off the driver's message is what
	// routing by arrival order looked like; everything now goes into the catalogue first, where it
	// has an identity and can be withdrawn. What is left on this struct is what a handshake that
	// never reached `READY` leaves behind.

	// Everything still held goes, which is what a handshake that did not reach `READY` leaves.
	unsafe fn close_all(&mut self) {
		for index in 0..self.count {
			if self.handles[index] != 0 {
				unsafe { close(self.handles[index]) };
				self.handles[index] = 0;
			}
		}
		self.count = 0;
	}
}

// ------------------------------------------------------- the provider catalogue
//
// FOUR NAMED LOCALS, FILLED IN ARRIVAL ORDER, WAS THE DEFECT.
//
// `block_client`, `block2_client`, `block3_client` and `block4_client` were four variables, each
// routed by hand to the service that owns that kind. So a second disk DID have somewhere to go and a
// fifth did not, and which volume was which depended on which driver finished first. The defect is
// the fixed count and the hand-written routing, not the existence of a limit - the registry bounds
// what each entry may publish, on purpose.

// How many providers the catalogue holds at once - GENERATED, from the sum of every `provides` bound
// this image's registry declares. See `build.rs`.
//
// It was `32`, written here: a number with no relation to the registry, so an image whose drivers
// declared more than that had a valid publication CLOSED and one that declared far fewer carried a
// table it could never fill. The definition of done says the count is bounded by what drivers
// declare and by nothing compiled into this program, and one global fixed table is the same defect
// the four named locals were, with a larger constant. Nothing in the image can publish past the sum
// of its own declarations, so the sum is the only number that is neither arbitrary nor a limit of
// its own - and the `provides` bound is already refused per driver by `publish_all`.

// One published provider.
struct Provider {
	id: ProviderId,
	kind: u16,
	// The publisher's own name for it, which is what a withdrawal names.
	token: u16,
	// The channel the driver serves it on. Zero for a free slot.
	handle: u64,
	// HOW MANY CONSUMERS HAVE BEEN GIVEN A CONNECTION TO IT - the offered channel included, from the
	// moment it is handed to somebody rather than from publication. Bounded by what the driver's
	// registry entry declares for the kind - see `Entry::provides` - so a kind that admits one
	// refuses the second ask instead of answering with an endpoint nobody serves.
	//
	// EVERY PATH THAT HANDS ONE OUT COUNTS, and every consumer that goes gives its place back: the
	// bound is on CONCURRENT consumers, and a number that only rose would spend a declaration of one
	// on the first client that ever connected. See `Catalogue::take`, `mint_connection`,
	// `Devices::open` and `Catalogue::disconnected`.
	consumers: u16,
}

// ONE MORE CONNECTION TO THE PROVIDER IN `slot`, minted through the same `CONNECT` the served
// `open` uses. Zero when there is no live binding to ask, or when the ask failed.
//
// NOT A DUPLICATE OF THE CHANNEL THE PROVIDER WAS OFFERED ON. A duplicate shares the reply queue
// with whoever holds the original, which is the "two consumers competing over one reply queue" the
// whole per-consumer factory exists to prevent - and a change meant to honour that rule committed
// exactly it once, so it is written here where the next caller will read it.
unsafe fn mint_connection(catalogue: &mut Catalogue, nodes: &[Node], slot: usize) -> u64 {
	unsafe {
		let Some((binding, token, kind, taken)) = catalogue.entries[slot].as_ref().map(|provider| (provider.id.binding, provider.token, provider.kind, outstanding(provider))) else {
			return 0;
		};
		let Some(node) = nodes.iter().find(|node| node.id.same_function(binding) && node.id.generation == binding.generation) else {
			return 0;
		};
		// AND THE BOOT'S OWN CONNECTIONS ARE CHECKED AGAINST THE DECLARATION TOO.
		//
		// This minted without consulting or incrementing anything, so a declared bound was already
		// understated before the first public `open`: the block probe takes one connection per block
		// provider and the role that mounts it takes another, and neither appeared against the
		// number the entry declares. A driver whose entry says it serves one consumer and is asked
		// for two by the boot itself is a manifest that is wrong about the driver, and finding that
		// out here is the point of declaring it.
		// FROM THE ENTRY THE PUBLISHER IS RUNNING, not the cursor - see `Node::entry`. A provider
		// belongs to a live binding, so its declared consumer bound is that driver's declaration and
		// not whichever candidate an operator has since selected for the next bind.
		let admits = node.entry().and_then(|entry| entry.provides.iter().find(|&&(declared, _, _)| declared == kind)).map_or(1, |&(_, _, consumers)| consumers);
		if taken >= admits {
			print(b"DeviceManager: a provider was asked for one more connection than its driver declares it admits; refused\n");
			return 0;
		}
		let Some((control, generation)) = node.binding.as_ref().map(|live| (live.channel, node.id.generation)) else {
			return 0;
		};
		let Some((server, client)) = channel() else { return 0 };
		let mut payload = [0u8; driver_protocol::OFFER_PAYLOAD_LEN];
		payload[..2].copy_from_slice(&token.to_le_bytes());
		if !send_frame(control, driver_protocol::Opcode::Connect, generation, &payload[..2], server, u32::MAX) {
			close(server);
			close(client);
			return 0;
		}
		if let Some(held) = catalogue.entries[slot].as_mut() {
			held.consumers = held.consumers.saturating_add(1);
		}
		client
	}
}

// One provider, as the wire describes it. `live` is what tells a publication from a withdrawal.
fn provider_info_wire(provider: &Provider, live: bool) -> proto::system::ProviderInfo {
	proto::system::ProviderInfo { kind: provider_kind_from_wire(provider.kind), bus: provider.id.binding.bus as u32, dev: provider.id.binding.dev as u32, func: provider.id.binding.func as u32, binding_generation: provider.id.binding.generation, slot: provider.id.slot as u32, provider_generation: provider.id.generation, live }
}

// One frame on one subscription. Answers false when the endpoint would not take it, which is a
// consumer that has gone.
unsafe fn send_provider_frame(subscriber: &mut Subscriber, info: &proto::system::ProviderInfo) -> bool {
	unsafe {
		let mut frame = [0u8; 128];
		let mut frame_handles = wire::Handles::new();
		let Some(len) = proto::system::provider_catalogue::subscribe_frame(subscriber.seq, info, &mut frame, &mut frame_handles) else {
			for handle in frame_handles.as_slice() {
				close(*handle);
			}
			return false;
		};
		if !try_send_caps(subscriber.producer, &frame[..len], frame_handles.as_slice()) {
			for handle in frame_handles.as_slice() {
				close(*handle);
			}
			return false;
		}
		subscriber.seq = subscriber.seq.wrapping_add(1);
		true
	}
}

// WHAT IS PUBLISHED, BY KIND, WITH THE MANAGER OWNING EVERY IDENTITY IN IT.
struct Catalogue {
	entries: [Option<Provider>; MAX_PROVIDERS],
	// Bumped every time a slot is filled, so a reused slot is never mistaken for the provider that
	// left it.
	generation: u32,
	// WHO IS LISTENING. See `Subscriber`.
	subscribers: [Option<Subscriber>; MAX_SUBSCRIBERS],
}

// A CONSUMER WATCHING ONE KIND, and the endpoint its frames go out on.
//
// `subscribe` returned a snapshot and the dispatcher never reached it: `OP_SUBSCRIBE` is a stream
// operation, so the generated `dispatch` answers `None` for it and the separate `subscribe_open` /
// `subscribe_frame` pair is the path - which nothing called. So the catalogue could be READ by
// nobody and could tell nobody anything, and the "late subscriber sees what is already there, and
// everything after it" contract was a sentence in the IDL.
//
// The snapshot and the live stream are ONE operation for the reason the IDL says: a provider
// published between a read and a registration would be in neither, which is the same race as a
// service started later seeing nothing.
struct Subscriber {
	// The producer end. The consumer end went to the subscriber in the reply.
	producer: u64,
	// The kind it asked about. A subscription is per kind, so a disk arriving does not wake the
	// service that watches network interfaces.
	kind: u16,
	seq: u32,
}

// How many subscriptions the catalogue serves at once. One per consumer that watches a kind, and a
// consumer that has gone is reaped at its first unsendable frame - so this bounds the LIVE ones.
const MAX_SUBSCRIBERS: usize = MAX_CATALOGUE_CLIENTS;

// THE TWO EFFECTS AN EMPTIED PUBLICATION OWES, as the library names them. See
// `driver_binding::Withdrawn`: the loop and its order are the library's and are under test there;
// what is here is the syscall and the send.
impl driver_binding::Withdrawn<Provider> for Catalogue {
	fn close_channel(&mut self, provider: &Provider) {
		if provider.handle != 0 {
			// SAFETY: the handle belongs to this catalogue and the slot it came from is empty.
			unsafe { close(provider.handle) };
		}
	}

	fn announce_gone(&mut self, provider: &Provider) {
		// SAFETY: sends on subscriber channels this catalogue owns.
		unsafe { self.announce(provider, false) };
	}
}

impl Catalogue {
	const fn new() -> Self {
		Self { entries: [const { None }; MAX_PROVIDERS], generation: 0, subscribers: [const { None }; MAX_SUBSCRIBERS] }
	}

	// REGISTER A SUBSCRIBER AND HAND IT WHAT IS ALREADY THERE, in one step, because the two cannot
	// be separated without a window. Answers false when it could not be registered, in which case
	// the caller closes the endpoint rather than serving a stream nothing will ever write to.
	unsafe fn subscribe_stream(&mut self, kind: u16, producer: u64) -> bool {
		unsafe {
			// DEAD SUBSCRIPTIONS ARE REAPED BEFORE ONE IS REFUSED (added 2026-09-02).
			//
			// A subscriber was forgotten only when `announce` next FAILED to send it something, and
			// that needs a publication or a withdrawal of its own kind. On a machine whose provider
			// set is stable - which is every machine after bring-up - nothing ever announces again,
			// so a consumer that exits leaves its slot occupied for the life of the boot. Eight
			// consumer restarts and the ninth subscription is refused with every provider still
			// there, which is the late-subscriber contract failing for a reason that has nothing to
			// do with how many consumers there are.
			//
			// Reaped HERE rather than every pass: the count only matters when a slot is wanted, and
			// probing eight endpoints on every turn of the standing loop would pay for it on every
			// heartbeat instead. Putting the producer ends in the wait set would notice sooner and
			// costs eight wait entries permanently, which is the more expensive answer to a question
			// nobody asks until the array is full.
			if self.subscribers.iter().all(Option::is_some) {
				self.reap_dead_subscribers();
			}
			let Some(slot) = self.subscribers.iter().position(Option::is_none) else {
				print(b"DeviceManager: the provider catalogue has as many subscribers as it will hold; refusing another\n");
				// The endpoint goes back rather than being held by a subscription that does not
				// exist: closing it is what tells the consumer the stream ended before it began.
				close(producer);
				return false;
			};
			let mut subscriber = Subscriber { producer, kind, seq: 0 };
			// EVERYTHING OF THAT KIND PUBLISHED RIGHT NOW, before the registration is visible to a
			// publication - this function holds `&mut self`, so nothing can publish in between.
			for index in 0..MAX_PROVIDERS {
				let Some(provider) = self.entries[index].as_ref() else { continue };
				if provider.kind != kind {
					continue;
				}
				let info = provider_info_wire(provider, true);
				if !send_provider_frame(&mut subscriber, &info) {
					close(producer);
					return false;
				}
			}
			self.subscribers[slot] = Some(subscriber);
			true
		}
	}

	// EVERY SUBSCRIPTION WHOSE CONSUMER HAS GONE, CLOSED AND FORGOTTEN.
	//
	// The probe is a non-blocking RECEIVE on the producer end, which is not a channel this service
	// ever reads for content: nothing is on the other side that sends, so a live consumer answers
	// `Empty` and a gone one answers `Closed`. That is the same fact `announce` learns from a failed
	// send, without needing something to announce.
	unsafe fn reap_dead_subscribers(&mut self) {
		unsafe {
			let mut buf: [u8; 1] = [0; 1];
			for slot in 0..MAX_SUBSCRIBERS {
				let Some(subscriber) = self.subscribers[slot].as_ref() else { continue };
				if matches!(try_recv_caps(subscriber.producer, &mut buf), PolledCaps::Closed)
					&& let Some(dead) = self.subscribers[slot].take()
				{
					close(dead.producer);
					print(b"DeviceManager: a provider subscription whose consumer has gone is closed and its slot given back\n");
				}
			}
		}
	}

	// A PUBLICATION OR A WITHDRAWAL, TO EVERYONE WATCHING THAT KIND. A subscriber whose endpoint
	// will not take the frame has gone: it is closed and forgotten rather than retried, because a
	// consumer that cannot be told is not a consumer.
	unsafe fn announce(&mut self, provider: &Provider, live: bool) {
		unsafe {
			let info = provider_info_wire(provider, live);
			for slot in 0..MAX_SUBSCRIBERS {
				let Some(subscriber) = self.subscribers[slot].as_mut() else { continue };
				if subscriber.kind != provider.kind {
					continue;
				}
				if !send_provider_frame(subscriber, &info) {
					if let Some(dead) = self.subscribers[slot].take() {
						close(dead.producer);
					}
				}
			}
		}
	}

	// Close every subscription. The manager is going away, and a stream whose producer stays open is
	// a consumer waiting for frames nobody will send.
	unsafe fn close_subscriptions(&mut self) {
		unsafe {
			for slot in 0..MAX_SUBSCRIBERS {
				if let Some(dead) = self.subscribers[slot].take() {
					close(dead.producer);
				}
			}
		}
	}

	// Take everything a committed binding offered into the catalogue, minting an id for each.
	//
	// Answers how many were published. A handle the catalogue has no room for is CLOSED rather than
	// dropped: a handle this service keeps and never serves is a channel the driver waits on
	// forever, and one it silently discards is a provider the machine has and cannot see.
	unsafe fn publish_all(&mut self, binding: BindingId, entry: &'static Entry, offers: &mut Offers) -> usize {
		unsafe {
			let mut published: usize = 0;
			for index in 0..offers.count {
				let handle = offers.handles[index];
				if handle == 0 {
					continue;
				}
				offers.handles[index] = 0;
				// A KIND THIS ENTRY NEVER DECLARED, OR ONE PAST WHAT IT DECLARED. `system-manifest`
				// checks the declaration is coherent; this checks the driver honoured it, which is
				// the half no build-time check can do. A compromised driver advertising itself as a
				// disk is refused here, with its handle closed rather than kept.
				let kind = offers.kinds[index];
				let Some(&(_, most, _)) = entry.provides.iter().find(|&&(declared, _, _)| declared == kind) else {
					print(b"DeviceManager: ");
					print(entry.name);
					print(b" offered a provider kind it does not declare in `provides`; refused\n");
					close(handle);
					continue;
				};
				if self.count_for(binding, kind) >= most as usize {
					print(b"DeviceManager: ");
					print(entry.name);
					print(b" offered more providers of one kind than it declares in `provides`; refused\n");
					close(handle);
					continue;
				}
				let Some(slot) = self.entries.iter().position(Option::is_none) else {
					print(b"DeviceManager: the provider catalogue is full; a published provider is being closed rather than lost quietly\n");
					close(handle);
					continue;
				};
				self.generation = self.generation.wrapping_add(1);
				// ZERO, BECAUSE NOBODY HAS BEEN GIVEN A CONNECTION YET. This was `1` on the argument
				// that the offer a publication carries IS a connection - and the offer is not handed
				// to anybody at publication: it sits in this entry until `take` or `open` moves it.
				// Counting it here made the DEFAULT provider unusable through the public factory:
				// `open` refuses at the declared limit, a kind declaring one consumer was already at
				// it, and the retained handle was reachable only through the private `take` this
				// milestone exists to replace.
				//
				// The count is now what it says it is - how many consumers have been GIVEN a
				// connection - and every path that hands one out is the path that increments it:
				// `take`, `mint_connection` and `open`.
				let provider = Provider { id: ProviderId::new(binding, slot as u16, self.generation), kind: offers.kinds[index], token: offers.tokens[index], handle, consumers: 0 };
				// AND EVERYONE WATCHING THAT KIND IS TOLD, which is the live half of a subscription:
				// a consumer that subscribed before this driver bound sees it appear.
				self.announce(&provider, true);
				self.entries[slot] = Some(provider);
				published += 1;
			}
			offers.count = 0;
			published
		}
	}

	// The channel of the first published provider of `kind` that has not been handed out yet.
	//
	// THE ENTRY STAYS. Handing the channel to a consumer does not unpublish the provider - it is
	// still there, still belongs to its binding, and still has to be withdrawn when that binding
	// ends. Removing the entry was the first version and it made the catalogue a staging area that
	// reported nothing: everything routed, so everything read as absent.
	//
	// The HANDLE is what moves, and it moves once. A second take of one kind answers 0 rather than
	// handing one capability to two consumers, which is two consumers competing over one reply queue
	// and not two connections.
	// THE SAME MOVE, BUT ONLY FROM ONE PUBLISHER (added 2026-09-01).
	//
	// `take` below chooses among EVERY unclaimed provider of a kind, by bus address. That is right
	// where the caller is choosing among all of them - the boot's four block volumes - and wrong
	// where the caller is routing ONE driver's offers, which is what `route_offers` does: it was
	// calling the global form, so "the USB stick's block service" was whichever block provider
	// happened to be unclaimed and lowest-addressed at that moment, not the one the xHCI driver
	// published. An extra unclaimed non-xHCI disk could therefore be handed over as `USBBLOCK`.
	//
	// The previous round rejected an audit finding about this on the grounds that the take sits
	// inside `route_offers` and is therefore origin-scoped by construction. It is not: where a call
	// SITS says nothing about what it SELECTS, and this one selected from the whole catalogue. The
	// binding is what makes origin a rule rather than a likelihood, so it is passed in.
	fn take_from(&mut self, binding: BindingId, kind: u16) -> u64 {
		let Some(slot) = self.entries.iter().position(|entry| entry.as_ref().is_some_and(|held| held.kind == kind && held.handle != 0 && held.binding_is(binding))) else {
			return 0;
		};
		match self.entries[slot].as_mut() {
			Some(provider) => {
				let handle = provider.handle;
				provider.handle = 0;
				// One consumer, counted - the same rule `take` states below.
				provider.consumers = provider.consumers.saturating_add(1);
				handle
			}
			None => 0,
		}
	}

	fn take(&mut self, kind: u16) -> u64 {
		// BY THE PUBLISHER'S ADDRESS ON THE BUS, ASCENDING - never by which driver finished first.
		//
		// THIS IS THE ORIGIN THE MILESTONE ASKS FOR, and for four identical virtio-blk disks it is
		// the only thing that separates them: they run the same driver, their formats do not differ,
		// and a FAT BPB cannot tell a removable medium from a USB stick. What differs is where each
		// one is plugged in, and the boot scan enumerates the bus ONCE, in bus order - so bus:dev:fn
		// is stable across boots in a way "whichever answered first" never was.
		let mut best: Option<usize> = None;
		for slot in 0..MAX_PROVIDERS {
			let Some(provider) = self.entries[slot].as_ref() else { continue };
			if provider.kind != kind || provider.handle == 0 {
				continue;
			}
			let better = match best {
				None => true,
				Some(previous) => self.entries[previous].as_ref().is_some_and(|held| address_of(provider) < address_of(held)),
			};
			if better {
				best = Some(slot);
			}
		}
		let slot = match best {
			Some(slot) => slot,
			None => return 0,
		};
		match self.entries[slot].as_mut() {
			Some(provider) => {
				let handle = provider.handle;
				provider.handle = 0;
				// AND IT IS ONE CONSUMER, COUNTED. This moved a connection to a consumer without
				// touching the count, so the declared bound described the connections `open` had
				// minted and not the ones this provider is actually serving.
				provider.consumers = provider.consumers.saturating_add(1);
				handle
			}
			None => 0,
		}
	}

	// Withdraw one publication of `binding`, named by the token its publisher chose.
	//
	// Answers the id it had, so a caller can say WHICH provider went away rather than that one did.
	unsafe fn withdraw(&mut self, binding: BindingId, token: u16) -> Option<ProviderId> {
		unsafe {
			let slot = self.entries.iter().position(|entry| entry.as_ref().is_some_and(|provider| provider.binding_is(binding) && provider.token == token))?;
			let provider = self.entries[slot].take()?;
			if provider.handle != 0 {
				close(provider.handle);
			}
			// AND EVERYONE WATCHING THAT KIND IS TOLD. A withdrawal that only removed the local
			// entry left a consumer holding a channel whose server is gone, which looks exactly like
			// one that is idle.
			self.announce(&provider, false);
			Some(provider.id)
		}
	}

	// Withdraw everything a binding published, which is what the end of that binding means.
	unsafe fn withdraw_binding(&mut self, binding: BindingId) -> usize {
		unsafe {
			// THE LOOP IS THE LIBRARY'S, and that is the whole of this change.
			//
			// This had its own loop, and `Publications::withdraw_binding` - the host-testable model
			// the publish/crash/subscribe race test drives - had another. They shared the leaf
			// predicate and nothing else, so the named test would have passed unchanged if THIS loop
			// had stopped selecting correctly, stopped emptying a slot, or counted twice. One
			// implementation of which slots go and how many that was; the side effect per slot -
			// closing the channel and announcing the withdrawal - stays here, because the model has
			// no handles and no subscribers to have them for.
			//
			// The announcement cannot run inside the closure: it borrows `self`, and the slots being
			// withdrawn are `self`'s. The withdrawn providers are collected and announced after.
			//
			// ON THE STACK, AND THEREFORE UNCONDITIONALLY (2026-08-30). This collected into a `Vec`
			// with a `try_reserve` whose failure was handled by printing a line and carrying on - and
			// on that path `capacity()` is zero, so the closure pushed NOTHING, every provider was
			// removed and closed, and not one withdrawal was announced. Every subscriber then kept
			// metadata for providers that no longer exist, for the rest of the boot, which is exactly
			// the stale-provider state the withdrawal exists to prevent. The comment claimed the
			// announcement was merely "short"; it was absent.
			//
			// `MAX_PROVIDERS` is the sum of every `provides` bound this image's registry declares - a
			// compile-time constant and a small one - so the array that holds them needs no allocator
			// at all and the failure mode goes with it.
			// AND THE TRANSFER IS THE LIBRARY'S TOO (2026-08-31). The closure used to close the
			// channel AND copy the provider into the array the announcement loop reads, and that
			// second half is a decision a host test could not see: the version before this one
			// collected into a `Vec` whose short-allocation path silently copied nothing, so every
			// provider was removed and closed and no withdrawal was announced. `withdraw_slots_into`
			// is driven by `driver-binding`'s own test and answers with exactly what it emptied; what
			// is left here is a `close` and a `send`, neither of which is a choice.
			let mut taken: [Option<Provider>; MAX_PROVIDERS] = [const { None }; MAX_PROVIDERS];
			let Some(gone) = driver_binding::withdraw_slots_into(&mut self.entries, binding, |provider| provider.id, &mut taken) else {
				print(b"DeviceManager: the withdrawal buffer is smaller than the catalogue, which cannot happen - nothing was withdrawn\n");
				return 0;
			};
			// AND THE TWO SIDE EFFECTS ARE COUNTED AGAINST WHAT WAS EMPTIED (added 2026-08-31).
			//
			// The selection and the transfer are the library's and have their own test; the `close`
			// and the `announce` are here, where no host test can reach them - so a loop that stopped
			// visiting a provider would leave a subscriber holding metadata for a publication that no
			// longer exists, which is the stale-provider state the withdrawal exists to prevent, and
			// nothing would have said so. The count the library returns is what this can be checked
			// against, and checking it costs one comparison on a per-binding path.
			// AND THE EFFECTS LOOP IS THE LIBRARY'S TOO (2026-09-02). The count comparison below
			// catches a loop that stops VISITING a slot and nothing else: deleting the `close` or the
			// `announce` from the body left every test in this tree green while a consumer held a
			// channel whose server was gone, or a subscriber kept metadata for a publication that no
			// longer exists - which is precisely M7's no-stale-provider and no-handle-leak rule. The
			// order and the completeness are now `driver-binding`'s `apply_withdrawal`, driven by its
			// own test; what is left here is the syscall and the send, neither of which is a choice.
			let announced: usize = driver_binding::apply_withdrawal(&taken, self);
			if announced != gone {
				print(b"DeviceManager: a withdrawal emptied more slots than it announced; a subscriber is now holding a provider that is gone\n");
			}
			gone
		}
	}

	// How many providers of `kind` THIS BINDING has published, which is what a per-entry bound is
	// about: two controllers of one kind each publishing one is not one controller publishing two.
	fn count_for(&self, binding: BindingId, kind: u16) -> usize {
		self.entries.iter().filter(|entry| entry.as_ref().is_some_and(|provider| provider.binding_is(binding) && provider.kind == kind)).count()
	}

	// A CONSUMER OF ONE PUBLICATION HAS GONE, so its place against the declared bound comes back.
	//
	// Named by the publisher's own token, like a withdrawal: a driver never sees a `ProviderId`.
	// Saturating, because a driver reporting more departures than it was given connections is a
	// driver to disbelieve rather than a count to wrap into a bound nobody declared.
	fn disconnected(&mut self, binding: BindingId, token: u16) {
		if let Some(provider) = self.entries.iter_mut().filter_map(Option::as_mut).find(|provider| provider.binding_is(binding) && provider.token == token) {
			provider.consumers = provider.consumers.saturating_sub(1);
		}
	}

	// How many providers this binding has published, of any kind.
	fn count_for_binding(&self, binding: BindingId) -> usize {
		self.entries.iter().filter(|entry| entry.as_ref().is_some_and(|provider| provider.binding_is(binding))).count()
	}

	// How many providers of `kind` are published. The answer a subscriber wants, and the one the
	// four fixed locals could only give up to four.
	fn count_of(&self, kind: u16) -> usize {
		self.entries.iter().filter(|entry| entry.as_ref().is_some_and(|provider| provider.kind == kind)).count()
	}
}

// HOW MANY CONNECTIONS TO THIS PROVIDER EXIST OR ARE PROMISED.
//
// The connections handed out, PLUS the offered channel while it is still sitting in the entry. The
// offered one is not a consumer yet - nobody holds it - but it is a connection this driver made and
// is serving, and it will go to whoever asks next. So a path that would MINT a new one has to count
// it, or a provider declaring one consumer would mint a second endpoint for a driver already serving
// the first, and the two would compete over one reply queue - the exact failure the per-consumer
// factory exists to prevent.
//
// A path that hands out the OFFERED channel does not add to this: it moves the same connection from
// promised to held.
fn outstanding(provider: &Provider) -> u16 {
	provider.consumers.saturating_add(u16::from(provider.handle != 0))
}

// One comparable number for a function's place on the bus, so "lowest address first" is one
// comparison rather than three.
fn address_of(provider: &Provider) -> u32 {
	((provider.id.binding.bus as u32) << 16) | ((provider.id.binding.dev as u32) << 8) | provider.id.binding.func as u32
}

impl Provider {
	// Whether this provider belongs to that binding - the same function AND the same generation,
	// because a provider published by a binding that is over is not this binding's.
	fn binding_is(&self, binding: BindingId) -> bool {
		// THE SHARED RULE, not a second copy of it. `driver_binding::ProviderId::belongs_to` is what
		// the host-testable `Publications` asks as well, so the withdrawal this catalogue performs
		// and the one the race test drives are the same decision.
		self.id.belongs_to(binding)
	}
}

// Send one frame, optionally moving one capability with it under `mask`.
unsafe fn send_frame(channel: u64, opcode: driver_protocol::Opcode, generation: u64, payload: &[u8], handle: u64, mask: u32) -> bool {
	unsafe {
		let mut frame = [0u8; driver_protocol::HEADER_LEN + driver_protocol::MAX_PAYLOAD];
		let header = driver_protocol::Header { version: driver_protocol::VERSION, opcode, generation, payload_len: payload.len() as u32 };
		frame[..driver_protocol::HEADER_LEN].copy_from_slice(&header.encode());
		frame[driver_protocol::HEADER_LEN..driver_protocol::HEADER_LEN + payload.len()].copy_from_slice(payload);
		let bytes = &frame[..driver_protocol::HEADER_LEN + payload.len()];
		if handle == 0 { send_blocking(channel, bytes, 0) } else { send_blocking_attenuated(channel, bytes, handle, mask) }
	}
}

// GIVE THE ATTEMPT BACK AND STOP TRYING.
//
// The node lands where the table allows a failed bind to land, and never in `Unbound`: that is where
// a node STARTS and what invites a bind, so a failed bind landing there is a bind that immediately
// happens again with nothing recorded about why the last one did not work.
//
// `Backoff` when the teardown confirmed and there is something a later attempt could change,
// `Failed` when there is not, `Quarantined` when the release could not be confirmed - which outranks
// both, because a device that may still be live is not a device to try again on.
// The same, for a failure that happened BEFORE the binding was installed and may still have attempts.
//
// `give_up` hardcoded "no attempts left", on the reasoning that it "is only called once the decision
// to stop trying has been made". That is true of the post-`Online` paths and false of `begin_bind`,
// which calls it for `SpawnFailed` and for a `DriverExited` while sending the initial frames - both of
// which the crate's own `retryable()` classifies as worth another try. So a transient spawn shortage
// ended the node permanently, and M3's `Stopping -> Backoff` edge was unreachable from the one place
// that needed it.
unsafe fn give_up_retryable(record: &mut BindingRecord, txn: &mut Attempt, offers: &mut Offers, pending: &mut Option<Teardown>, deadline: u64, cause: FailureCause, driver_name: &[u8], attempts_left: bool) -> bool {
	unsafe { give_up_with_budget(record, txn, offers, pending, deadline, cause, driver_binding::StopIntent::Fault, driver_name, attempts_left && cause.retryable()) }
}

// The same, for a node that was asked to stop rather than one that failed.
//
// THE INTENT IS WHAT `Stopping` RESOLVES AGAINST. P02M0162's table sends a confirmed teardown on to
// `Backoff` and then back to `Binding`, which is right for a driver that died and exactly wrong for
// one that was asked to stop: the operator stops it and it starts again. An UNCONFIRMED teardown
// ignores the intent entirely and ends at `Quarantined`, because what is unknown is whether the
// device is still live and no intent changes that.
unsafe fn give_up_with(record: &mut BindingRecord, txn: &mut Attempt, offers: &mut Offers, pending: &mut Option<Teardown>, deadline: u64, cause: FailureCause, intent: driver_binding::StopIntent, driver_name: &[u8]) -> bool {
	unsafe { give_up_with_budget(record, txn, offers, pending, deadline, cause, intent, driver_name, false) }
}

// `attempts_left` is what the table's confirmed outcome branches on - see `give_up_retryable`.
#[allow(clippy::too_many_arguments)]
unsafe fn give_up_with_budget(record: &mut BindingRecord, txn: &mut Attempt, offers: &mut Offers, pending: &mut Option<Teardown>, deadline: u64, cause: FailureCause, intent: driver_binding::StopIntent, driver_name: &[u8], attempts_left: bool) -> bool {
	unsafe {
		// THE PATH THROUGH THE TABLE DEPENDS ON WHETHER A DEVICE WAS TAKEN, and flattening that was
		// wrong. `Binding -> Failed` is for a transaction that failed BEFORE the claim; once there
		// is a device to quieten, the table's only way out is `Binding -> Stopping -> Failed`,
		// because `Stopping` is what "there is a teardown to run" means. Going straight to `Failed`
		// records a node that never had a device, which is a different story about the same boot.
		let took_the_device = txn.held.claim != 0;
		if took_the_device {
			record.move_to(BindingState::Stopping, Some(cause));
		}
		// STEPS 1 TO 3. Where the node lands is decided when the confirmations arrive, so it is
		// recorded on the teardown rather than applied now - see `resolve_teardown`.
		let mut teardown = txn.begin_teardown(offers, deadline);
		teardown.cause = cause;
		// `give_up` is only reached once the decision to stop trying has been made, so there are no
		// attempts left to spend - which for a fault is `Failed`, and for the other intents is
		// whatever they say. `Shutdown` answers None: the manager is going away, so there is no next
		// binding to describe and entering a state nobody will read is a state nobody wrote down for
		// a reason.
		teardown.landed = intent.confirmed_lands_at(attempts_left);
		teardown.retrying = false;
		teardown.intent = intent;
		print(b"DeviceManager: ");
		print_driver_name(driver_name);
		if intent == driver_binding::StopIntent::Fault {
			print(b" did not bind (");
			print(cause.name());
			print(b") - stopping it");
		} else {
			print(b" was stopped (");
			print(intent.name());
			print(b") - stopping it");
		}
		print(b"\n");
		*pending = Some(teardown);
		false
	}
}

// STEP 4, AND THE ONLY PLACE A TEARDOWN ENDS. Answers `Some` once both confirmations have arrived or
// the teardown deadline has passed, and `None` while the node is still `Stopping` and waiting.
unsafe fn resolve_teardown(node: &mut Node, driver_name: &[u8], now: u64) -> Option<BindingState> {
	unsafe {
		let Some(teardown) = node.teardown.as_mut() else { return None };
		let mut closes = Syscalls;
		let deadline = teardown.deadline;
		let confirmed = match teardown.pending.settle(&mut closes, now, deadline)? {
			driver_binding::Settled::Free => BindingState::Backoff,
			driver_binding::Settled::Unconfirmed => BindingState::Quarantined,
		};
		let (cause, landed, retrying) = (teardown.cause, teardown.landed, teardown.retrying);
		let (planned_stop, intent) = (teardown.planned_stop, teardown.intent);
		node.teardown = None;
		if confirmed == BindingState::Quarantined {
			node.record.move_to(BindingState::Quarantined, Some(FailureCause::TeardownUnconfirmed));
			print(b"DeviceManager: ");
			print_driver_name(driver_name);
			print(b" - the teardown did not confirm, so this device is quarantined for the boot\n");
			if planned_stop {
				print(b"DeviceManager: ");
				print_driver_name(driver_name);
				print(b" answered the stop, and its teardown did NOT confirm - nothing here says its work was flushed: ");
				print(intent.name());
				print(b"\n");
				// AND THIS ONE IS AN INCIDENT, which is why `advance` no longer captures one for an
				// answered stop. A planned stop that ends in `Quarantined` is a device that may still
				// be live, and that is exactly the thing an operator has to be able to read
				// afterwards - so the capture happens HERE, where the failure became known, and
				// carries the cause that describes it rather than the one the stop was labelled with.
				let report = capture(node, FailureCause::TeardownUnconfirmed);
				report_incident(driver_name, &report);
				node.incident_report = Some(report);
				node.incident_stored = false;
			}
			return Some(BindingState::Quarantined);
		}
		if planned_stop {
			print(b"DeviceManager: ");
			print_driver_name(driver_name);
			print(b" stopped cleanly: ");
			print(intent.name());
			print(b"\n");
		}
		if retrying {
			node.record.move_to(BindingState::Backoff, Some(cause));
			print(b"DeviceManager: restarting ");
			print_driver_name(driver_name);
			print(b"\n");
			return Some(BindingState::Backoff);
		}
		let state = match landed {
			Some(landed) if node.record.move_to(landed, Some(cause)) => landed,
			_ => node.record.state,
		};
		print(b"DeviceManager: ");
		print_driver_name(driver_name);
		print(b" - the node is ");
		print(state.name());
		print(b"\n");
		Some(state)
	}
}

// GIVE THE ATTEMPT BACK AND TRY AGAIN. Answers false when the teardown did not confirm, which ends
// the node rather than rebinding over a device that may still be writing to memory.
unsafe fn retry_or_quarantine(record: &mut BindingRecord, txn: &mut Attempt, offers: &mut Offers, pending: &mut Option<Teardown>, deadline: u64, cause: FailureCause) {
	unsafe {
		// Through `Stopping` for the same reason `give_up` does it: there is a device to quieten, and
		// the table has no edge from `Binding` to `Backoff` once one has been taken.
		if txn.held.claim != 0 {
			record.move_to(BindingState::Stopping, Some(cause));
		}
		let mut teardown = txn.begin_teardown(offers, deadline);
		teardown.cause = cause;
		teardown.retrying = true;
		*pending = Some(teardown);
	}
}

// Launch (and, on a crash during bring-up, restart) the driver for device `i`,
// handing it only that device's MMIO capability and info. Returns true once the
// driver reports in. If a started driver crashes before reporting, the kernel tears
// it down and its bootstrap channel peer-closes (recv returns Closed); DeviceManager
// then re-acquires a fresh capability and respawns it, up to a few times - the
// driver crash/restart cycle. Drivers do not crash in normal operation, so the
// restart path is dormant on a healthy boot.
// ------------------------------------------------------- the one wait
//
// SHORT NON-BLOCKING STEPS DRIVEN FROM ONE CENTRAL WAIT.
//
// Bring-up used to be a blocking `launch_one` per device: claim, spawn, and then park on the
// handshake until it answered. One driver that never answered held the manager, and every other
// device waited behind the bring-up of a device nobody was using. An actor framework is not needed
// for this and is not wanted - what is needed is that no step blocks.
//
// The wait set is two handles per node in flight: its channel, and its process. The second is what
// makes an exit an EVENT rather than something discovered by a read failing.

// The ceiling on nodes in flight at once, from the kernel's wait-set limit and two handles each.
const MAX_NODES_IN_FLIGHT: usize = abi::MAX_WAIT_HANDLES / 2;

// What a handle in the wait set is: a driver's channel, its process, or the claim of a teardown
// waiting to settle.
const WAIT_CHANNEL: u8 = 0;
const WAIT_EXIT: u8 = 1;
const WAIT_CLAIM: u8 = 2;

// Wait for one thing to happen anywhere, and queue it on the node it belongs to.
//
// Returns false when there is nothing in flight to wait for, which is the loop's exit condition.
unsafe fn pump(nodes: &mut [Node], in_flight_from: usize, catalogue: &mut Catalogue, buf: &mut [u8]) -> bool {
	unsafe {
		// The wait set, and which node each entry belongs to. Rebuilt every pass because what is in
		// flight changes every pass.
		let mut handles: [u64; abi::MAX_WAIT_HANDLES] = [0; abi::MAX_WAIT_HANDLES];
		let mut owner: [usize; abi::MAX_WAIT_HANDLES] = [0; abi::MAX_WAIT_HANDLES];
		let mut is_process: [bool; abi::MAX_WAIT_HANDLES] = [false; abi::MAX_WAIT_HANDLES];
		// WHAT EACH READY HANDLE MEANS. `is_process` answered two questions with one bool while
		// there were only two kinds of handle in the set; a claim is a third.
		let mut kind: [u8; abi::MAX_WAIT_HANDLES] = [WAIT_CHANNEL; abi::MAX_WAIT_HANDLES];
		let mut set: usize = 0;
		// The earliest deadline anywhere, so the wait ends when the FIRST node runs out rather than
		// when the last one does.
		let mut soonest: u64 = 0;
		// THE WATCHDOG RUNS DURING BRING-UP TOO, over EVERY node and not only the ones this phase is
		// binding. Phase two takes seconds, and a driver bound in phase one that nothing pinged for
		// the length of it was declared wedged for being unattended rather than for being wedged -
		// which is a supervisor reporting its own inattention as a driver fault.
		let beats: u64 = tick_heartbeats(nodes, buf);
		if beats != 0 {
			soonest = beats;
		}
		// AND A NODE OUTSIDE THIS PHASE'S RANGE CONSUMES ITS OWN EVENTS. Its verdict is not this
		// loop's to act on - there is no candidate list to advance for a driver another phase bound
		// - but leaving a wedge queued would mean noticing it and never answering it.
		for at in 0..in_flight_from {
			if nodes[at].record.state != BindingState::Online {
				continue;
			}
			let name: &[u8] = nodes[at].driver_name();
			let _ = advance(&mut nodes[at], name, catalogue);
		}
		for (at, node) in nodes.iter().enumerate() {
			if at < in_flight_from || !node.in_flight() || set + 2 > abi::MAX_WAIT_HANDLES {
				continue;
			}
			// A TEARDOWN'S TWO HANDLES, WHICH IS WHAT MAKES ITS CONFIRMATIONS EVENTS. The Process
			// handle is still open because the exit has not arrived, and the Claim handle is open
			// when the release answered `Releasing` rather than terminally. The node has no binding
			// at this point - it was taken out before the rollback - which is why this is checked
			// first and not inside the binding arm.
			if let Some(teardown) = &node.teardown {
				if teardown.pending.process != 0 {
					handles[set] = teardown.pending.process;
					owner[set] = at;
					is_process[set] = true;
					kind[set] = WAIT_EXIT;
					set += 1;
				}
				if teardown.pending.claim != 0 && set < abi::MAX_WAIT_HANDLES {
					handles[set] = teardown.pending.claim;
					owner[set] = at;
					is_process[set] = false;
					kind[set] = WAIT_CLAIM;
					set += 1;
				}
				if soonest == 0 || teardown.deadline < soonest {
					soonest = teardown.deadline;
				}
				continue;
			}
			let Some(binding) = &node.binding else { continue };
			handles[set] = binding.channel;
			owner[set] = at;
			is_process[set] = false;
			kind[set] = WAIT_CHANNEL;
			set += 1;
			if binding.process != 0 {
				handles[set] = binding.process;
				owner[set] = at;
				is_process[set] = true;
				kind[set] = WAIT_EXIT;
				set += 1;
			}
			let deadline: u64 = node.incident.attempt_deadline();
			if soonest == 0 || deadline < soonest {
				soonest = deadline;
			}
		}
		// A NODE PARKED ON SOMEBODY ELSE'S RELEASE IS WORK IN FLIGHT WITH NO HANDLE TO WAIT ON.
		//
		// `in_flight` is a question about HANDLES - a binding being brought up, or a teardown whose
		// confirmations have not arrived - and a node waiting for a device's claim to reach `Free`
		// has neither. So it contributed nothing to the set below, `set == 0` read as "nothing left
		// to do", and the loop returned for the last time with the node still parked: the re-read
		// `BindStart::WaitingForTheClaim` promises was never performed by anything. The wait such a
		// node needs is a DEADLINE rather than a handle, so it is one here.
		let parked: Option<u64> = nodes.iter().skip(in_flight_from).filter(|node| node.waiting_for_claim).map(|node| node.retry_at).min();
		if let Some(due) = parked
			&& (soonest == 0 || due < soonest)
		{
			soonest = due;
		}
		if set == 0 {
			// Nothing to wake ON, so the deadline IS the wait. Bounded by the kernel's own latched
			// release deadline rather than by anything here: once it passes, `observe_claim` answers
			// `Terminal` and the next attempt ends the node instead of parking it again.
			let Some(due) = parked else { return false };
			if due > clock() {
				sleep_until(due);
			}
			return true;
		}
		let ready: i64 = wait_any(&handles[..set], soonest);
		if ready < 0 {
			// THE DEADLINE, OR A WAIT THAT COULD NOT BE DONE AT ALL. Either way the answer is the
			// same: give every node whose own deadline has passed its timeout, and let the state
			// machine decide what that means. Treating an un-performable wait as a spin would burn
			// the rest of the window on a cooperative scheduler.
			let now: u64 = clock();
			let mut timed_out: bool = false;
			for (at, node) in nodes.iter_mut().enumerate() {
				if at < in_flight_from || !node.in_flight() {
					continue;
				}
				if node.incident.attempt_deadline() > now {
					continue;
				}
				let generation: u64 = node.id.generation;
				node.push(BindingEvent::TimedOut { generation });
				timed_out = true;
			}
			// NOBODY TIMED OUT AND THE WAIT STILL FAILED, which is a wait this program cannot
			// perform - a handle without the WAIT right, or a set the kernel refused. Reporting no
			// progress here would spin; the honest answer is to time out everything in flight,
			// because a manager that cannot wait cannot supervise.
			//
			// UNLESS A PARKED NODE'S RE-READ CAME DUE, WHICH IS A LEGITIMATE WAKE (fixed 2026-09-02).
			// The claim re-read added its `retry_at` to `soonest` so the loop would come round for
			// it - correctly - and `wait_any` reports a deadline the same way it reports a refusal:
			// a negative result. So the shortest backoff in the system started waking this branch,
			// finding no node whose OWN attempt deadline had passed, concluding the wait could not
			// be performed, and timing out every healthy handshake in flight. A supervisor turning
			// its own poll into a driver's `HandshakeTimeout` is worse than the wait it was added to
			// perform.
			let parked_due: bool = parked.is_some_and(|due| due <= now);
			if !timed_out && !parked_due {
				for (at, node) in nodes.iter_mut().enumerate() {
					if at < in_flight_from || !node.in_flight() {
						continue;
					}
					let generation: u64 = node.id.generation;
					node.push(BindingEvent::TimedOut { generation });
				}
			}
			return true;
		}
		let at: usize = ready as usize;
		if at >= set {
			return true;
		}
		let which: usize = owner[at];
		if kind[at] == WAIT_CLAIM {
			// THE OTHER HALF OF THE TEARDOWN. Read once the wait has woken, which is what the claim
			// handle is waitable for - a manager on one `wait_any` loop cannot spin on a status.
			let generation: u64 = nodes[which].id.generation;
			let state: u32 = match device_claim_info(handles[at]) {
				Ok(info) if info.settled != 0 => info.state,
				// A read that failed, or one that says the release has not settled after the handle
				// signalled: neither is a terminal state, and a teardown that cannot learn one ends
				// at its deadline rather than being called confirmed.
				_ => return true,
			};
			nodes[which].push(BindingEvent::ClaimSettled { generation, state });
			return true;
		}
		if is_process[at] {
			let generation: u64 = nodes[which].id.generation;
			nodes[which].push(BindingEvent::Exited { generation });
			return true;
		}
		// A READABLE CHANNEL IS DRAINED, not read once. Several frames can be waiting - a driver
		// sends its offers and its `READY` back to back - and taking one per wait would cost a
		// syscall per frame and reorder nothing.
		drain_channel(&mut nodes[which], buf);
		true
	}
}

// Take every frame waiting on this node's channel and queue what each one means.
unsafe fn drain_channel(node: &mut Node, buf: &mut [u8]) {
	unsafe {
		let Some(binding) = &node.binding else { return };
		let (channel, generation): (u64, u64) = (binding.channel, node.id.generation);
		loop {
			let frame = try_recv_caps(channel, buf);
			let (len, handles) = match frame {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return,
				PolledCaps::Closed => {
					node.push(BindingEvent::Closed { generation });
					return;
				}
			};
			let refuse = |handles: &wire::Handles| {
				for &handle in handles.as_slice() {
					close(handle);
				}
			};
			let Ok(header) = driver_protocol::Header::decode(&buf[..len]) else {
				refuse(&handles);
				continue;
			};
			// A FRAME CARRYING A STALE GENERATION IS DROPPED RATHER THAN ACTED ON, and its handles
			// with it: a capability from a binding that is over is not a capability to publish.
			if header.check_handles(handles.as_slice().len()).is_err() || header.generation != generation {
				refuse(&handles);
				continue;
			}
			// WHEN THIS DRIVER LAST SAID ANYTHING, for the capture. Recorded on the way past rather
			// than reconstructed later: after the process is gone there is nobody to ask.
			node.last_opcode = header.opcode as u16;
			node.last_frame_at = clock();
			match header.opcode {
				// A DRIVER SENDING `CONNECT` IS SENDING THE MANAGER'S OWN FRAME BACK. This is
				// manager-to-driver and nothing else; a frame arriving here under it is refused with
				// its handle closed, like every other opcode this direction does not carry.
				driver_protocol::Opcode::Connect => {
					refuse(&handles);
					continue;
				}
				driver_protocol::Opcode::Offer => {
					let Ok((kind, token)) = driver_protocol::decode_offer(header.payload(buf)) else {
						refuse(&handles);
						continue;
					};
					if node.offers.push(kind, token, handles.as_slice()[0]) {
						node.push(BindingEvent::Offered { generation });
					} else {
						// PAST THE BOUND IS A REFUSAL WITH THE HANDLE CLOSED, not an accumulation.
						refuse(&handles);
					}
				}
				driver_protocol::Opcode::Ready => {
					if driver_protocol::decode_ready(header.payload(buf)).is_ok() {
						node.push(BindingEvent::Ready { generation });
					}
				}
				driver_protocol::Opcode::Failed => {
					// A `FAILED` WHOSE PAYLOAD IS NOT ONE IS REFUSED, not rounded to a code.
					//
					// This mapped every decode error to `InternalError` and queued an ordinary
					// `Failed`, so a malformed frame became the recorded FACT `DriverReported(
					// InternalError)` - a driver-owned vocabulary entry manufactured from input that
					// did not contain one, carrying that code's non-retryable policy with it. The
					// vocabulary is closed precisely so a driver cannot hand the manager a fact it
					// did not state. A refused frame is dropped like every other malformed one; what
					// the driver does next - a valid terminal frame, an exit, or nothing until the
					// deadline - is what the manager concludes from.
					let Ok(code) = driver_protocol::decode_failed(header.payload(buf)) else {
						refuse(&handles);
						continue;
					};
					node.push(BindingEvent::Failed { generation, code });
				}
				driver_protocol::Opcode::Pong => {
					// THE WATCHDOG IS SETTLED HERE, NOT THROUGH THE QUEUE, and the order is why.
					//
					// The manager wakes exactly at the deadline it set, drains the channel and then
					// asks whether the driver answered. With the answer sitting UNREAD in the node's
					// queue, that question said no - so a driver that replied on time was declared
					// wedged and its own pong then arrived to a manager that had stopped waiting.
					// The heartbeat is control-path state, not a lifecycle event, and it is settled
					// where it is read.
					//
					// A MISMATCH STILL BECOMES AN EVENT, because that is the case worth reporting: a
					// duplicate, one from an earlier round, or a number nobody asked with does NOT
					// reset the watchdog, and saying so is the whole difference from `rt::heartbeat`.
					if let Ok(sequence) = driver_protocol::decode_sequence(header.payload(buf)) {
						if node.beat.answered(sequence, clock(), driver_protocol::heartbeat_period(node.beat.deadline())) {
						} else {
							node.push(BindingEvent::Ponged { generation, sequence });
						}
					}
				}
				driver_protocol::Opcode::Stopped => {
					// ONLY WHERE A STOP WAS ACTUALLY ASKED FOR. This queued any generation-matching
					// STOPPED, so a driver could announce a clean stop nobody requested and the
					// manager would print that it stopped cleanly and tear the binding down. A
					// planned stop is a state this manager put the node INTO; an unsolicited frame
					// claiming one is a driver describing a conversation that did not happen.
					if node.record.state == BindingState::Stopping && node.stop_intent != driver_binding::StopIntent::Fault {
						node.push(BindingEvent::Stopped { generation });
					} else {
						print(b"DeviceManager: ");
						print_driver_name(node.driver_name());
						print(b" said it had stopped and nothing had asked it to; the frame is refused\n");
					}
				}
				// Manager-to-driver opcodes coming the wrong way: refused rather than ignored,
				// because a driver asking the manager whether IT is alive, or telling it to stop,
				// is a driver that has misunderstood which end of this channel it is on.
				driver_protocol::Opcode::Ping | driver_protocol::Opcode::Stop => refuse(&handles),
				driver_protocol::Opcode::Withdraw => {
					if let Ok(token) = driver_protocol::decode_withdraw(header.payload(buf)) {
						node.push(BindingEvent::Withdrawn { generation, token });
					}
				}
				driver_protocol::Opcode::Disconnect => {
					if let Ok(token) = driver_protocol::decode_disconnect(header.payload(buf)) {
						node.push(BindingEvent::Disconnected { generation, token });
					}
				}
				// Manager-to-driver opcodes, coming the wrong way. Refused, not ignored.
				driver_protocol::Opcode::Bind | driver_protocol::Opcode::Resource => refuse(&handles),
			}
		}
	}
}

// One decimal number into `out`, answering how many bytes it wrote.
//
// A capture that cannot be READ is a capture nobody takes seriously, and this program has no
// formatter - every other line it prints is a fixed byte string. Twenty digits covers `u64::MAX`.
fn decimal(value: u64, out: &mut [u8; 20]) -> usize {
	if value == 0 {
		out[0] = b'0';
		return 1;
	}
	let mut digits = [0u8; 20];
	let mut at: usize = 0;
	let mut rest = value;
	while rest > 0 && at < digits.len() {
		digits[at] = b'0' + (rest % 10) as u8;
		rest /= 10;
		at += 1;
	}
	for index in 0..at {
		out[index] = digits[at - 1 - index];
	}
	at
}

// Print the capture. One line, in the order `Diagnostic` declares its fields, so the log and the
// struct cannot drift into two different stories about the same incident.
unsafe fn report_incident(driver_name: &[u8], report: &Diagnostic) {
	unsafe {
		let mut number = [0u8; 20];
		print(b"DeviceManager: incident ");
		print_driver_name(driver_name);
		print(b" at ");
		let n = decimal(report.binding.bus as u64, &mut number);
		print(&number[..n]);
		print(b":");
		let n = decimal(report.binding.dev as u64, &mut number);
		print(&number[..n]);
		print(b".");
		let n = decimal(report.binding.func as u64, &mut number);
		print(&number[..n]);
		print(b" generation ");
		let n = decimal(report.binding.generation, &mut number);
		print(&number[..n]);
		print(b", ");
		print(report.state.name());
		print(b", last opcode ");
		let n = decimal(report.last_opcode as u64, &mut number);
		print(&number[..n]);
		if report.silent_for == u64::MAX {
			print(b" (it never sent one)");
		} else {
			print(b" ");
			let n = decimal(report.silent_for, &mut number);
			print(&number[..n]);
			print(b" tick(s) ago");
		}
		print(b", attempt ");
		let n = decimal(report.attempts as u64, &mut number);
		print(&number[..n]);
		match report.domain {
			Some(stats) => {
				print(b", domain memory ");
				let n = decimal(stats.memory_used, &mut number);
				print(&number[..n]);
				print(b" peak ");
				let n = decimal(stats.memory_peak, &mut number);
				print(&number[..n]);
				print(b", handles ");
				let n = decimal(stats.handles_used, &mut number);
				print(&number[..n]);
				print(b", threads ");
				let n = decimal(stats.threads_used, &mut number);
				print(&number[..n]);
				print(b", dma ");
				let n = decimal(stats.dma_used, &mut number);
				print(&number[..n]);
			}
			// A DIFFERENT FACT FROM ZEROS. A Domain that was never made or is already gone has no
			// counters, and printing zeros would say the driver used nothing.
			None => print(b", no domain counters were readable"),
		}
		print(b"\n");
		// THE CAUSE IS LAST AND IT IS NOT A NUMBER. It is the one field a reader acts on, and a
		// discriminant would make them count variants in a source file to find out what happened.
		print(b"DeviceManager:   cause: ");
		print(cause_name(report.cause));
		print(b"\n");
	}
}

// A name for each cause. `driver-reported` carries the driver's own code, which is the half that
// explains anything - "the driver said it failed" without saying what it said explains nothing.
fn cause_name(cause: FailureCause) -> &'static [u8] {
	match cause {
		FailureCause::DriverMissing => b"the registry names a driver this image does not contain",
		FailureCause::ProtocolMismatch => b"its note declares a protocol version this build does not implement",
		FailureCause::ClaimRefused => b"somebody else holds the device",
		FailureCause::IommuRequired => b"its DMA policy demands translation on a machine that is not enforcing it",
		FailureCause::ResourceExhausted => b"a quota or an allocation refused",
		FailureCause::SpawnFailed => b"the process could not be started",
		FailureCause::HandshakeTimeout => b"it never answered its bind at all",
		FailureCause::Hung => b"it came up and then stopped answering its control path",
		FailureCause::DriverExited => b"it exited without saying anything",
		FailureCause::Stopped => b"it was asked to stop and it did",
		FailureCause::DriverReported(code) => code.name(),
		FailureCause::TeardownUnconfirmed => b"the release did not confirm, so the device may still be live",
	}
}

// Take the bounded capture. See `Diagnostic` for why the list is fixed.
unsafe fn capture(node: &Node, cause: FailureCause) -> Diagnostic {
	unsafe {
		let now: u64 = clock();
		Diagnostic {
			binding: node.id,
			state: node.record.state,
			cause,
			last_opcode: node.last_opcode,
			// NEVER SPOKE IS NOT "A LONG TIME AGO". A binding whose driver sent nothing at all is a
			// different failure from one that went quiet, and collapsing them would lose the
			// difference between a driver that never started and one that stopped.
			silent_for: if node.last_frame_at == 0 { u64::MAX } else { now.saturating_sub(node.last_frame_at) },
			attempts: node.record.attempts,
			domain: node.binding.as_ref().filter(|binding| binding.domain != 0).and_then(|binding| domain_stats(binding.domain)),
		}
	}
}

// WHETHER EVERY KIND THIS ENTRY DECLARED IS PUBLISHED.
//
// EVERY, not the first - starting a bind on the first published requirement is a bind that fails on
// the second, which spends an attempt and reports a driver failure for a condition that was never
// the driver's.
fn requirements_met(entry: &'static Entry, catalogue: &Catalogue) -> bool {
	entry.requires.iter().all(|&kind| catalogue.count_of(kind) > 0)
}

// Start this node's current candidate, or park it waiting for what it declared.
//
// THE QUESTION IS ASKED AT EVERY ATTEMPT AND NOT ONLY THE FIRST. Reacting to a withdrawal EVENT
// while a node sits in `Backoff` is not enough: a node can arrive in `Backoff` from `Stopping` after
// a crash, and if the requirement went away DURING that teardown the event is spent - so the expiry
// would proceed into a bind gated on a condition that no longer holds. One rule, asked every time,
// instead of an edge per way of getting there.
unsafe fn gate_on_requirements(node: &mut Node, entry: &'static Entry, catalogue: &Catalogue) -> bool {
	unsafe {
		if requirements_met(entry, catalogue) {
			return true;
		}
		if node.record.move_to(BindingState::DependencyPending, None) {
			print(b"DeviceManager: ");
			print(entry.name);
			print(b" is waiting for a provider it declares in `requires`\n");
		}
		false
	}
}

// Whether another automatic attempt is allowed: the attempt budget AND the time budget, which are
// two different budgets and are spent in different places.
//
// The table has an edge for each - `Binding -> Failed` when the attempts are spent, `Backoff ->
// Failed` when the time is - and reading only the first is how a node sits in a backoff whose
// deadline passed while it slept.
unsafe fn may_try_again(incident: &Incident, attempt: u32) -> bool {
	// `attempt` is zero-based, so the number of attempts already made is `attempt + 1` - which is
	// also what `begin_bind` prints. Another one is allowed only while that count is below the bound.
	unsafe { attempt + 1 < MAX_AUTOMATIC_ATTEMPTS && incident.allows_backoff(BACKOFF_TICKS[(attempt as usize).min(BACKOFF_TICKS.len() - 1)]) }
}

// Sleep the backoff before attempt number `attempt` (1-based: the delay before the second attempt
// is the first of `BACKOFF_TICKS`).
//
// `may_try_again` has already established there is room for it, so this only has to say WHEN.
//
// A DEADLINE, NOT A SLEEP. This called `sleep_until`, which parks the ONE DeviceManager thread - so a
// node's 100 ms or 200 ms backoff was 100 ms or 200 ms during which no other node's handshake, exit,
// heartbeat or catalogue query was looked at. M2's node independence is the whole point of the queue
// and the multiplexed wait above it, and a sleep in the middle of the loop undoes both. The node
// records when it may try again and the loop's own wait is bounded by the soonest of them.
// A BRING-UP LOOP HAS NOTHING ELSE TO DO, so it waits the node's backoff out where the standing loop
// skips and comes back. Both honour the same deadline; they differ in what else they could be doing.
// SPEND THE CANDIDATE THAT ENDED, which is not always the one the cursor is on.
//
// Both `Step::NextCandidate` handlers used to do `candidate += 1` directly, and that is only right
// while the cursor still describes what ran. It does not after a `select`: that verb moves the cursor
// to the entry the NEXT bind must start from, and an operator may ask for it while a DIFFERENT entry
// is online. When that online entry then failed, the advance stepped past the selection and started
// the entry after it - so the one verb whose whole contract is "this applies at the next bind" was
// skipped by the next bind.
//
// The entry that ended is `Node::spent`, latched when the binding was taken out of the node; the
// cursor is the answer only when nothing was running, which is the pre-claim failures
// `start_candidate` advances for itself.
fn spend_candidate(node: &mut Node) {
	let ran: usize = node.spent.take().unwrap_or(node.candidate);
	// AN UNSTARTED SELECTION IS NEVER SPENT BY SOMETHING ELSE ENDING (fixed 2026-09-02).
	//
	// This compared the cursor with the entry that ran and kept it only when it was numerically
	// LATER, on the reasoning that a cursor past what ran must have been moved on purpose. That is
	// true of a later selection and false of every other one: an operator selecting the FIRST
	// candidate, or the one that is running, leaves a cursor at or before `ran`, which looked
	// exactly like a cursor that had not advanced yet - so `select` worked only when it happened to
	// name a higher index, which is not a contract anybody can use.
	//
	// A flag says what a comparison cannot. `PolicyVerb::Select` sets it, `begin_bind` clears it
	// when an attempt actually starts from that entry, and while it is set the cursor is left where
	// the operator put it. That is exactly "applies at the NEXT bind": one bind starts there and the
	// selection is spent, so a selected candidate that then fails advances like any other and cannot
	// hold the node in a loop on one entry.
	if node.selection_pending {
		node.attempt = 0;
		return;
	}
	if node.candidate <= ran {
		node.candidate = ran + 1;
	}
	node.attempt = 0;
}

unsafe fn wait_out_backoff(node: &Node) {
	unsafe {
		if node.retry_at > clock() {
			sleep_until(node.retry_at);
		}
	}
}

unsafe fn back_off_until(incident: &Incident, attempt: u32) -> u64 {
	unsafe {
		let index: usize = (attempt as usize).saturating_sub(1).min(BACKOFF_TICKS.len() - 1);
		let mut until: u64 = clock().saturating_add(BACKOFF_TICKS[index]);
		// NEVER PAST THE INCIDENT. A backoff is not a reason to overrun the window; if the delay
		// would end after what is spendable, what is spendable is where it ends.
		if incident.deadline != 0 {
			let spendable: u64 = incident.deadline.saturating_sub(incident.teardown_reserve);
			if until > spendable {
				until = spendable;
			}
		}
		until
	}
}

// OPEN THE TRANSACTION AND SEND `BIND`, AND DO NOT WAIT FOR THE ANSWER.
//
// Everything this attempt takes is recorded in the transaction as it is taken, so there is no path
// out that leaves a claim, a process, a handle or a provider behind. What is different from the
// blocking bring-up this replaces is only where it ENDS: with the frames sent and the node in
// flight, for the central wait to hear about.
//
// Returns false when the attempt could not be opened at all, having already put the node wherever
// the table says a bind that failed this early belongs.
#[allow(clippy::too_many_arguments)]
// WHAT STARTING A BIND ANSWERED, and why one bit could not say it (2026-09-01).
//
// `begin_bind` returned a bool, and every caller read `false` as "this candidate is spent". One of
// its `false` paths does not mean that at all: a device whose claim is still `Releasing` is one
// somebody else is giving back, and the node is parked in `Backoff` with `retry_at` set so the
// standing loop looks again. Read as a failure, that path consumed a candidate per pass until the
// cursor ran off the end of the list - after which the retry starts nothing, because there is
// nothing left to start - and on the boot path it dropped the node entirely rather than keeping it.
// A transient state became a permanent one by way of a return type that could not express it.
//
// The bind's own comment already said this was fixed. What had been fixed was the STATE TRANSITION
// inside `begin_bind`; the callers still could not tell the two `false`s apart, so the defect stayed.
enum BindStart {
	// A claim was taken and a child started. The central wait owns it from here.
	Opened,
	// Nothing was started and this candidate IS spent - the claim was refused, the spawn failed.
	CandidateFailed,
	// Nothing was started and this candidate is NOT spent. The device's claim is still being
	// released by whoever held it; the node waits in `Backoff` and the standing loop re-reads the
	// claim when `retry_at` comes due. Bounded by the kernel's own latched release deadline, after
	// which `observe_claim` answers `Terminal` instead and this stops being reachable.
	WaitingForTheClaim,
}

unsafe fn begin_bind(node: &mut Node, info: &DeviceInfo, elf: &[u8], driver_name: &[u8], key_producer: u64, power: u64, console_input: u64, device_privilege: u64) -> BindStart {
	unsafe {
		// A STORED DISABLE IS CONSULTED BEFORE EVERY BIND, not applied once when it was read.
		//
		// This is the other half of `load_stored_policy`: the record is a desire that outlives any
		// one binding, so the question "may this device be bound" has to be asked HERE, where a bind
		// starts, rather than answered once against whatever state the node happened to be in when
		// ConfigService first answered. Without it a driver that was online when the policy arrived
		// stayed online - correctly - and then bound again on its next crash, against a record that
		// was still on disk.
		if node.disabled_by_policy {
			node.record.move_to(BindingState::Disabled, None);
			print(b"DeviceManager: ");
			print_driver_name(driver_name);
			print(b" is disabled in stored policy; this bind does not start\n");
			return BindStart::CandidateFailed;
		}
		if node.attempt == 0 {
			// ONE WINDOW FOR THE WHOLE CHAIN OF ATTEMPTS, opened on the first and not per attempt.
			// Three attempts of two seconds plus their backoffs is 6.3 seconds for ONE device,
			// which is already past the window the kernel's settle ladder gives the whole boot -
			// and a machine with several unbindable devices multiplies it.
			node.incident = Incident::open();
		}
		// THE TEARDOWN SLICE THIS ATTEMPT'S ROLLBACK GETS, computed once from the window rather than
		// at each failure point - so every exit from this function gives its rollback the same
		// budget, which is what "reserved" means.
		let teardown_deadline: u64 = node.incident.teardown_deadline();
		// WHAT THIS ARTIFACT SAYS IT SPEAKS, BEFORE THE DEVICE IS GIVEN TO IT.
		//
		// The note exists for exactly this moment - its own definition says so: "refusing a driver
		// AFTER the claim would mean taking a device back from something that should never have held
		// it." Nothing read it. `driver_protocol::declared_version` answers for the RUNNING binary
		// and `common::handshake` calls it once the process is already spawned and the device already
		// claimed, so an artifact declaring a version this build does not implement was claimed,
		// started, and only then failed on the frame exchange.
		//
		// NO NOTE IS NOT A MISMATCH. An artifact without one is not an artifact this build produced,
		// and that is a packaging fault rather than a version disagreement - but it is equally not
		// something to hand a device to, and `protocol-mismatch` is the cause for both. It is the
		// variant M0161 defined for this and the reason it had no producer.
		// THE ONE PREDICATE, in the crate that owns the note - see `speaks_this_version`. A caller
		// comparing the number itself has to remember that a missing note and a stale one lead to
		// the same refusal, and remembering is what a shared predicate is for.
		match driver_protocol::speaks_this_version(elf) {
			true => {}
			false => {
				print(b"DeviceManager: ");
				print_driver_name(driver_name);
				print(b" does not declare this build's driver protocol; refusing before the claim\n");
				node.record.move_to(BindingState::Failed, Some(FailureCause::ProtocolMismatch));
				return BindStart::CandidateFailed;
			}
		}
		// WHETHER THIS NODE STILL HAS AN ATTEMPT, computed once and handed to every failure exit below.
		// A failure here is a failure BEFORE the binding is installed, and `SpawnFailed` and a
		// `DriverExited` while sending the initial frames are both classified retryable by the crate -
		// so ending the node permanently on the first transient shortage was the table's
		// `Stopping -> Backoff` edge being unreachable from the one place that needed it.
		let attempts_left: bool = may_try_again(&node.incident, node.attempt);
		// The backoff this attempt was waiting out is spent; nothing should wake for it again - and
		// this attempt IS the re-read a parked node was waiting to make, so the park is over
		// whatever this attempt turns out to be. The arm below sets it again if the claim is still
		// being released.
		node.retry_at = 0;
		node.waiting_for_claim = false;
		// AND AN OPERATOR'S SELECTION IS SPENT BY THE BIND IT ASKED FOR. From here the entry this
		// attempt is on is an ordinary candidate: if it fails, the cursor advances like any other.
		node.selection_pending = false;
		let mut txn = Attempt::new();
		if !node.record.move_to(BindingState::Binding, None) {
			print(b"DeviceManager: refusing an illegal transition into binding for ");
			print_driver_name(driver_name);
			print(b"\n");
			return BindStart::CandidateFailed;
		}
		node.record.attempts = node.attempt + 1;
		// WHAT THIS DEVICE IS DOING, READ BEFORE IT IS ASSUMED FREE.
		//
		// A NEW MANAGER CAN LEGITIMATELY ARRIVE AT A DEVICE THAT IS `Releasing`. `Domain::kill` marks
		// the subtree and `Process::terminate` closes each handle table synchronously, so the claim
		// handle's last close - and with it the forced teardown - starts promptly; what is
		// asynchronous is the rest, and the teardown still has to confirm the device is quiet.
		//
		// Treating that as a refusal would be a permanent `Failed` for a state that was about to
		// clear on its own, because `claim-refused` is classified as NOT retryable: a transient
		// condition promoted to a terminal one by two correct rules meeting.
		//
		// AND A BIND IS ATTEMPTED ONLY ON AN OBSERVED `Free`, never on a deadline having merely
		// passed. "Waits and then binds" is not one of the branches.
		match observe_claim(node, device_privilege, driver_name) {
			ClaimReadiness::Bindable => {}
			ClaimReadiness::WaitAndSeeAgain => {
				// COME BACK TO IT, rather than losing it.
				//
				// This returned `false`, and both callers read that as "this candidate failed": the
				// non-boot path moved to the next candidate while the record was still `Binding` - so
				// no later candidate could enter `Binding` either - and the boot path dropped the
				// device entirely. A `Releasing` claim is a device somebody else is still giving
				// back, which is a WAIT and not a failure, and the kernel has already latched the
				// deadline, so the only thing missing was something to make this manager look again.
				// `Backoff` is the state for an attempt that has not started, and `retry_at` is what
				// the standing loop's wait is bounded by.
				node.record.move_to(BindingState::Backoff, None);
				node.retry_at = clock().saturating_add(BACKOFF_TICKS[0]);
				// AND THE NODE IS MARKED AS PARKED ON THAT RELEASE, which is the half that was
				// missing (2026-09-01). `retry_at` alone said WHEN to look again and nothing said
				// that anything should: the node has no binding and no teardown, so `Node::in_flight`
				// excludes it, `pump` found an empty wait set and answered "nothing in flight", and
				// the bring-up loop returned with the re-read never performed. On a boot-critical
				// device that is a manager reporting itself online with a zero system-block handle.
				// See `Node::waiting_for_claim`.
				node.waiting_for_claim = true;
				return BindStart::WaitingForTheClaim;
			}
			ClaimReadiness::Terminal(cause) => return bind_start_of(give_up_retryable(&mut node.record, &mut txn, &mut node.offers, &mut node.teardown, teardown_deadline, cause, driver_name, attempts_left)),
		}
		let grant: ClaimGrant = match device_claim(node.index, device_privilege) {
			Ok(grant) => grant,
			// WHICH REFUSAL IT WAS. The kernel keeps two apart and this collapsed them into one:
			// `ERR_ACCESS_DENIED` is the DMA policy declining to admit the device on a machine that is
			// not enforcing translation, which is `iommu-required` - a cause M3 kept in the vocabulary
			// on the explicit condition that the distinguishable kernel path produce it, and nothing
			// did. Everything else here is "somebody else holds it", which is `claim-refused` and is
			// not worth waiting on either.
			Err(errno) if errno == abi::ERR_ACCESS_DENIED => return bind_start_of(give_up_retryable(&mut node.record, &mut txn, &mut node.offers, &mut node.teardown, teardown_deadline, FailureCause::IommuRequired, driver_name, attempts_left)),
			Err(_) => return bind_start_of(give_up_retryable(&mut node.record, &mut txn, &mut node.offers, &mut node.teardown, teardown_deadline, FailureCause::ClaimRefused, driver_name, attempts_left)),
		};
		txn.held.claim = grant.claim;
		txn.key = grant.key;
		node.record.generation = grant.key.generation;
		// THE SAME FUNCTION, ONE BINDING LATER. The BDF is what survives a rebind and the
		// generation is what makes the last binding's messages refusable.
		node.id = node.id.rebound(grant.key.generation);
		let (dm_side, driver_side): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return bind_start_of(give_up_retryable(&mut node.record, &mut txn, &mut node.offers, &mut node.teardown, teardown_deadline, FailureCause::ResourceExhausted, driver_name, attempts_left)),
		};
		txn.held.channel = dm_side;
		// HELD BY THE TRANSACTION until the spawn consumes it - see `Attempt::driver_side`.
		txn.held.driver_side = driver_side;
		// THE PROCESS HANDLE IS KEPT. It used to be dropped the moment `spawn` returned, so
		// nothing could end the process a failed bind had started - which is what made "leaves
		// nothing behind" untrue in the one case it is written for. It is also the handle the
		// central wait watches, which is what makes an exit an event rather than a read failing.
		// ONE CHILD DOMAIN PER BINDING, and this is the change M5 is actually about: every driver
		// used to be launched with `spawn`, which is `spawn_in(.., 0)`, and domain 0 means the
		// SPAWNER'S OWN Domain. So every driver in the system was charged to DeviceManager, and a
		// driver that exhausted memory exhausted the manager's budget rather than its own.
		//
		// NOT RESOURCE-BOUNDED HERE. Limits are ResourceManager's subject; a number invented at this
		// line would be a policy nobody declared. What the Domain buys without one is ATTRIBUTION -
		// memory, handles, threads, IPC, DMA and stack, per binding, which `DomainStats` already
		// reports - and a subtree that can be killed as a unit.
		let domain: i64 = domain_create(u64::MAX, u64::MAX, u64::MAX);
		if domain < 0 {
			return bind_start_of(give_up_retryable(&mut node.record, &mut txn, &mut node.offers, &mut node.teardown, teardown_deadline, FailureCause::ResourceExhausted, driver_name, attempts_left));
		}
		txn.held.domain = domain as u64;
		let process: i64 = spawn_in(elf, driver_side, domain as u64);
		if process < 0 {
			// The spawn did NOT take the bootstrap handle - `spawn_prepared_in` says so - so it is
			// still the transaction's and the rollback closes it.
			return bind_start_of(give_up_retryable(&mut node.record, &mut txn, &mut node.offers, &mut node.teardown, teardown_deadline, FailureCause::SpawnFailed, driver_name, attempts_left));
		}
		// Taken by the spawn: no longer this transaction's to close.
		txn.held.driver_side = 0;
		txn.held.process = process as u64;
		// ASSEMBLE THE RESOURCE LIST BEFORE ANNOUNCING ITS LENGTH.
		//
		// `BIND` states how many `RESOURCE` frames follow, and that number is this service's own
		// count of the list it is ABOUT TO SEND - not a number read out of the registry entry,
		// which has no resource list to read one from. A promise about what is already in hand
		// is the only kind that can be kept, and the driver's receive loop is bounded by it:
		// state one too many and the driver waits forever for a frame that is not coming.
		//
		// The interrupt-driven drivers (virtio-input, virtio-net, virtio-snd, xhci, virtio-gpu,
		// dev-channel) each take their own per-device MSI-X vector, edge-triggered with no INTx
		// sharing. The gpu routes only its CONFIG vector to it and keeps its control queue
		// polled; the dev channel is idle almost always and must block on its interrupt rather
		// than poll, because a spinning driver starves the cooperative scheduler for the guest's
		// whole life. The remaining polling drivers get none, so their device IRQs stay silent.
		// RECORDED ON THE TRANSACTION AS THEY ARE TAKEN. The list used to be a local array, so a
		// failure while acquiring a later entry - or while sending - reached `give_up` with the
		// earlier ones held and nothing able to close them.
		txn.holds(driver_protocol::ResourceKind::Device as u16, grant.memory);
		let use_msix: bool = driver_name == b"virtio_input" || driver_name == b"virtio_net" || driver_name == b"virtio_snd" || driver_name == b"xhci" || driver_name == b"virtio_gpu" || driver_name == b"dev_channel";
		if use_msix {
			let irq: i64 = device_msix_acquire(grant.claim);
			if irq < 0 {
				return bind_start_of(give_up_retryable(&mut node.record, &mut txn, &mut node.offers, &mut node.teardown, teardown_deadline, FailureCause::ResourceExhausted, driver_name, attempts_left));
			}
			txn.holds(driver_protocol::ResourceKind::Irq as u16, irq as u64);
		}
		if driver_name == b"virtio_input" || driver_name == b"xhci" {
			let sink: i64 = duplicate(key_producer, RIGHT_SEND | RIGHT_TRANSFER);
			if sink < 0 {
				return bind_start_of(give_up_retryable(&mut node.record, &mut txn, &mut node.offers, &mut node.teardown, teardown_deadline, FailureCause::ResourceExhausted, driver_name, attempts_left));
			}
			txn.holds(driver_protocol::ResourceKind::Keys as u16, sink as u64);
			// A CONNECTION OF ITS OWN, not a copy of an authority. These two used to be handed a
			// duplicate of the root-Domain handle - which can kill every process on the machine -
			// so that the Power key would work. What they get now can ask for a reboot and
			// nothing else, on a channel nobody else answers on.
			let Some(connection) = service_connect(power) else { return bind_start_of(give_up_retryable(&mut node.record, &mut txn, &mut node.offers, &mut node.teardown, teardown_deadline, FailureCause::ResourceExhausted, driver_name, attempts_left)) };
			txn.holds(driver_protocol::ResourceKind::SysPower as u16, connection);
			// The capability that lets those keystrokes reach the console at all. A duplicate
			// per driver, for the same reason as the power connection.
			if console_input != 0 {
				let feed: i64 = duplicate(console_input, RIGHT_TRANSFER);
				if feed < 0 {
					return bind_start_of(give_up_retryable(&mut node.record, &mut txn, &mut node.offers, &mut node.teardown, teardown_deadline, FailureCause::ResourceExhausted, driver_name, attempts_left));
				}
				txn.holds(driver_protocol::ResourceKind::Console as u16, feed as u64);
			}
		}
		let resource_count: usize = txn.held.resources().len();
		node.granted_resources = resource_count as u32;
		// WHICH RULE CHOSE THIS DRIVER, recorded where the choice is still in hand. An entry may
		// declare several and "virtio_console bound it" does not say which applied - the pinned
		// development console and the ordinary one are the same artifact under two rules.
		node.matched_rule = node.candidates.get(node.candidate).and_then(|entry| entry.rules.iter().position(|rule| rule.matches(info))).unwrap_or(0) as u32;
		// `BIND` - the device, and the count of what follows. No capability travels with it.
		let mut payload = [0u8; driver_protocol::MAX_PAYLOAD];
		let payload_len = driver_protocol::encode_bind(info, resource_count as u16, &mut payload);
		if !send_frame(dm_side, driver_protocol::Opcode::Bind, grant.key.generation, &payload[..payload_len], 0, 0) {
			return bind_start_of(give_up_retryable(&mut node.record, &mut txn, &mut node.offers, &mut node.teardown, teardown_deadline, FailureCause::DriverExited, driver_name, attempts_left));
		}
		for index in 0..resource_count {
			let (kind, handle) = txn.held.resources()[index];
			let mut kind_payload = [0u8; driver_protocol::U16_PAYLOAD_LEN];
			driver_protocol::encode_u16(kind, &mut kind_payload);
			// THE DEVICE CAPABILITY ARRIVES WITHOUT RIGHT_TRANSFER, through one attenuating
			// move. It is minted here WITH it, because this process is the one that hands it
			// over and cannot do that with a capability it may not move - minting it without
			// TRANSFER outright would break the boot on the first try, right here. The rule is
			// about the HOLDER: a driver cannot pass its device on.
			let mask: u32 = if kind == driver_protocol::ResourceKind::Device as u16 { RIGHT_READ | RIGHT_WRITE | RIGHT_MAP } else { RIGHTS_ALL };
			if !send_frame(dm_side, driver_protocol::Opcode::Resource, grant.key.generation, &kind_payload, handle, mask) {
				// The send did not transfer it, so it is still the transaction's - and so is every
				// entry after it, which is exactly what the rollback now closes.
				return bind_start_of(give_up_retryable(&mut node.record, &mut txn, &mut node.offers, &mut node.teardown, teardown_deadline, FailureCause::DriverExited, driver_name, attempts_left));
			}
			txn.handed_over(handle);
		}
		// IN FLIGHT. The transaction's holdings become the node's binding, which is what the
		// central wait watches and what a rollback gives back.
		node.binding = Some(Binding { domain: txn.held.domain, process: txn.held.process, channel: txn.held.channel, claim: txn.held.claim, key: txn.key });
		// AND WHICH CANDIDATE IS RUNNING IT. Latched with the binding, from the cursor as it stands
		// now, so a later `select` cannot change the answer for a driver already started. See
		// `Node::running`.
		node.running = Some(node.candidate);
		txn.commit();
		BindStart::Opened
	}
}

// `give_up_retryable` answers a bool and that bool is always `false` - see `give_up_with_budget`,
// whose every path ends there. It is a "this did not open" marker rather than an outcome, so the
// argument is taken and discarded: what it means at every use inside `begin_bind` is the same thing,
// that nothing started and this candidate is spent. Where the node was LEFT - `Backoff`, `Failed`,
// `Stopping` - is the record's business and the caller does not branch on it.
fn bind_start_of(_did_not_open: bool) -> BindStart {
	BindStart::CandidateFailed
}

// What one node's state machine decided, for the caller that knows what to do about it.
enum Step {
	// Still in flight: nothing terminal has arrived.
	Waiting,
	// Committed. Its offers are on the node, and routing them is the caller's - which provider
	// goes where is a policy this machinery has no business knowing.
	Online,
	// Rolled back, and this candidate may be tried again.
	Again,
	// Rolled back, and this candidate is spent. The next one, if there is one, follows.
	NextCandidate,
	// RESTING, WHICH IS NOT THE SAME AS SPENT. A teardown that was ASKED for landed where its intent
	// says it lands - `Disabled` for an operator's stop, `DependencyPending` for a provider that went
	// away - and neither of those is a candidate that failed. This exists because the two used to
	// share `NextCandidate`, and the cursor advance that answer carries turned both landings into a
	// spent entry: a one-candidate device disabled by an operator could never be enabled again, and a
	// node parked on a lost dependency lost the entry `settle_dependencies` restarts it from. The
	// node keeps its cursor and waits for whatever revives it - `PolicyVerb::Enable`, or the
	// provider being published again.
	Resting,
	// Terminal: `Failed` or `Quarantined`. Nothing further happens on this node this boot.
	Done,
}

// Drain one node's queue and act on what is in it.
//
// ONE EVENT AT A TIME AND IN ORDER, which is the whole point of the queue: an exit that arrives
// during a teardown and a `READY` that arrives on the next binding are two events about two
// bindings, and the generation on each is what keeps them apart.
unsafe fn advance(node: &mut Node, driver_name: &[u8], catalogue: &mut Catalogue) -> Step {
	unsafe {
		while let Some(event) = node.pop() {
			// AN EVENT ARRIVING DURING A TEARDOWN BELONGS TO THE TEARDOWN. The node is `Stopping`
			// and the only two facts it is waiting for are the child's exit and the claim settling;
			// anything else a dying binding emits is about a binding that is already over.
			if let Some(teardown) = node.teardown.as_mut() {
				teardown.pending.note(event);
				continue;
			}
			// WHETHER THIS WAS A STOP THAT COMPLETED, so the outcome can be stated once the
			// teardown has answered rather than before it has run - see the `Stopped` arm.
			let mut planned_stop = false;
			let cause: FailureCause = match event {
				// AN OFFER BEFORE `READY` IS HELD; AN OFFER AFTER IT IS PUBLISHED AT ONCE.
				//
				// Driver readiness and provider readiness are different facts. `READY` means the
				// process is initialised and supervised; a provider appears when its OWN probe is
				// complete, and for a controller with children that is later - the xHCI driver
				// reports in and then enumerates its bus. Holding every offer until the terminal
				// frame is right for the handshake, where a driver that dies half way through
				// announcing itself must announce nothing, and wrong afterwards, where there is no
				// handshake left to be half way through.
				BindingEvent::Offered { .. } => {
					// AGAINST THE DECLARATION THE RUNNING DRIVER ACTUALLY MADE - see `Node::entry`.
					// This read the cursor, so a late offer from a live driver was published against
					// whichever entry an operator had since selected: the wrong `provides` set, and
					// the wrong consumer bound for a kind the running driver never declared.
					if node.record.state == BindingState::Online
						&& let Some(entry) = node.entry()
					{
						catalogue.publish_all(node.id, entry, &mut node.offers);
					}
					continue;
				}
				// A LIVE DRIVER RETIRING ONE OF ITS OWN PUBLICATIONS. Not an outcome either: the
				// driver stays bound and its other providers stay published. Named by the token it
				// chose, because a driver never sees the identity this service minted.
				// AN ANSWER THAT ECHOES THE NUMBER IT WAS ASKED WITH, and nothing else counts.
				//
				// This is the whole of what `rt::heartbeat` gets wrong: it returns true for ANY
				// message, so a driver emitting unrelated traffic reads as responsive and a busy
				// driver and a wedged one look the same. A pong with a sequence nobody is waiting
				// for - a duplicate, one from an earlier round, one invented - does NOT reset the
				// watchdog. Stale GENERATIONS never reach here at all: `pop` drops them.
				// ONLY A MISMATCH REACHES HERE - `drain_channel` settles the answer that was asked
				// for. A duplicate, one from an earlier round, or a number nobody asked with does
				// NOT reset the watchdog, and this is where that is said out loud.
				BindingEvent::Ponged { .. } => {
					print(b"DeviceManager: ");
					print_driver_name(driver_name);
					print(b" answered a ping nobody asked; the watchdog is not reset by it\n");
					continue;
				}
				// A WEDGED DRIVER IS TORN DOWN LIKE A CRASHED ONE. The teardown is the same
				// transaction and the same retry-and-quarantine counter; what differs is the reason
				// for starting it, which is what the record carries.
				BindingEvent::Wedged { .. } => {
					print(b"DeviceManager: ");
					print_driver_name(driver_name);
					print(b" stopped answering its control path inside the deadline its registry entry declares\n");
					// `hung`, NOT `handshake-timeout`. A driver that came up and then went quiet is
					// a different fact from one that never answered at all, and a reader cannot act
					// on "it did not answer" without knowing which.
					FailureCause::Hung
				}
				// A CLAIM SETTLING WHEN NO TEARDOWN IS OUTSTANDING. The teardown arm above consumes
				// these; one arriving here belongs to a teardown that has already been resolved -
				// its deadline passed and the late confirmation came anyway - and a node that has
				// been quarantined for it is not un-quarantined by the answer turning up.
				BindingEvent::ClaimSettled { .. } => continue,
				BindingEvent::Withdrawn { token, .. } => {
					let withdrawn = catalogue.withdraw(node.id, token);
					print(b"DeviceManager: ");
					print_driver_name(driver_name);
					print(if withdrawn.is_some() { b" withdrew a provider it had published\n" } else { b" withdrew a provider that was not published under that token\n" });
					continue;
				}
				// ONE PLACE GIVEN BACK against what the entry declares. The bound is on CONCURRENT
				// consumers, and with nothing coming this way the count only rose - so a kind
				// admitting one was refused for the rest of the boot the moment its first consumer
				// closed. Saturating, because a driver reporting more departures than connections is
				// a driver to disbelieve, not a count to wrap.
				BindingEvent::Disconnected { token, .. } => {
					catalogue.disconnected(node.id, token);
					continue;
				}
				BindingEvent::Ready { .. } => {
					// THE TRANSACTION COMMITS. What it took stays taken, and everything held
					// unpublished through the handshake enters the catalogue here - in one place,
					// so a provider offered before `READY` and one offered after it are published
					// by the same code under the same bound.
					// EXACTLY ONE TERMINAL FRAME, AND THE STATE TABLE IS WHAT SAYS SO.
					//
					// The result of `move_to` was discarded, so a SECOND `READY` on an already-online
					// binding was acted on in full: the table refused `Online -> Online` silently, and
					// this went on to publish the offers again, re-arm supervision and report
					// `Step::Online` a second time. The handshake ends in one `READY` or one `FAILED`;
					// a driver that sends another is not making a transition, and the refusal the
					// table already computes is the answer - it just had to be read.
					if !node.record.state.accepts_terminal_frame() || !node.record.move_to(BindingState::Online, None) {
						print(b"DeviceManager: a second terminal frame on a binding that is already past its handshake - refused\n");
						return Step::Again;
					}
					// THE BRING-UP INCIDENT IS SPENT AND A NEW ONE STARTS AT `Online`.
					//
					// Neither counter was reset here, so a crash an hour later was judged against the
					// incident opened for the ORIGINAL bring-up: expired, so recovery was declared
					// spent before it began - or, if it had not expired, charged with whatever
					// attempts the bring-up had already used. A driver that came up is a driver whose
					// bring-up succeeded, and what follows is a different incident.
					node.attempt = 0;
					node.incident = Incident::open();
					// AND THE OPERATOR'S ONE ATTEMPT SUCCEEDED, so there is nothing left to spend.
					// A flag left set here would stop the FIRST later crash from trying the next
					// candidate, long after the request that set it was answered.
					node.retry_once = false;
					// The entry this binding is RUNNING - see `Node::entry`. Latched at the bind
					// commit, so a `select` between the commit and this `READY` cannot publish the
					// live driver's providers against another candidate's declaration.
					let Some(entry) = node.entry() else { continue };
					catalogue.publish_all(node.id, entry, &mut node.offers);
					// SUPERVISION STARTS WHERE THE DRIVER SAYS IT IS UP, not at the bind: before
					// `READY` the bind budget is what bounds it, and two deadlines over one
					// interval is two authorities that disagree the first time one is slower.
					node.beat.arm(entry.heartbeat_deadline, clock(), entry.heartbeat_deadline.map_or(0, driver_protocol::heartbeat_period));
					return Step::Online;
				}
				// A `FAILED` FRAME IS ABOUT THE DRIVER, NEVER ABOUT ONE OF ITS CHILDREN.
				//
				// A controller whose child fails says so by WITHDRAWING that child's provider - the
				// binding stays `Online` and its siblings stay published. There is no child-failure
				// frame and there should not be one: the two are different facts and the protocol
				// already has a word for each.
				BindingEvent::Failed { code, .. } => {
					// AND THE SAME RULE ON THIS SIDE OF THE CHOICE, which is where it was missing.
					//
					// `READY` was refused a second time because the state table has no `Online ->
					// Online` edge; `FAILED` was not, because it does not move to a fixed state - it
					// computes a cause and goes through the teardown, and the teardown is reachable
					// from `Online` for the good reason that a driver can crash after coming up. So
					// `READY` then `FAILED` on one generation took an online binding apart on the
					// strength of a frame the handshake had already ended. It is a second terminal
					// frame and it is refused like the other one.
					if !node.record.state.accepts_terminal_frame() {
						print(b"DeviceManager: a second terminal frame on a binding that is already past its handshake - refused\n");
						return Step::Again;
					}
					// A DRIVER THAT SAID WHY. Retryability is read off the code rather than decided
					// again here: `device-not-responding` and `out-of-memory` are the two a second
					// attempt can change, and the other three describe a driver that has read its
					// device and will not drive it however many times it is asked.
					print(b"DeviceManager: ");
					print_driver_name(driver_name);
					print(if code.retryable() { b" reported a retryable failure\n" } else { b" reported a permanent failure\n" });
					FailureCause::DriverReported(code)
				}
				// A PLANNED STOP COMPLETING, which is not a failure and must not be recorded as one.
				// The node carries the intent it was stopped WITH, and that is what decides where a
				// confirmed teardown lands - a driver that died goes back round to `Backoff` and
				// then `Binding`, and an operator's stop that did the same would be a stop that
				// starts the driver again.
				BindingEvent::Stopped { .. } => {
					// SAID AFTER THE TEARDOWN, NOT BEFORE IT. This printed "stopped cleanly" HERE -
					// before `roll_back` had called `device_release` and learned whether the device
					// went quiet - so an unconfirmed teardown could announce a clean flush and then
					// land in `Quarantined`. The answer is recorded and the line is printed below,
					// once there is something to base it on.
					planned_stop = true;
					// AND IT IS NOT `driver-exited` - WHICH IT WAS, AND THE COMMENT KNEW (fixed
					// 2026-09-01). The previous version returned `DriverExited` and said in as many
					// words that the cause "renders as 'it exited without saying anything', which is
					// the opposite of what a STOPPED frame is" - and then returned it anyway,
					// arguing the cause "only travels so the shared teardown path has one to carry".
					// It travels further: the shared path captures an incident, prints it and
					// PERSISTS it, so a clean shutdown left a stored row telling the operator the
					// driver had crashed. M3 requires a planned stop not to be classified as a crash,
					// and a comment describing the lie is not the same as not telling it.
					FailureCause::Stopped
				}
				// The process ended, or its channel closed with nothing terminal on it. Both are a
				// driver that is gone without having said anything.
				BindingEvent::Exited { .. } | BindingEvent::Closed { .. } => FailureCause::DriverExited,
				BindingEvent::TimedOut { .. } => {
					// A DRIVER THAT IS STILL THERE AND HAS NOT ANSWERED, which is the case the
					// budget exists for: before it, this wait had no end and one silent driver held
					// the manager - and therefore the boot - for as long as it liked.
					print(b"DeviceManager: ");
					print_driver_name(driver_name);
					print(b" did not report in inside its share of the boot window\n");
					FailureCause::HandshakeTimeout
				}
			};
			// THE CAPTURE COMES FIRST, BEFORE ANYTHING IS GIVEN BACK. The Domain's counters cannot
			// be read once the Domain is killed, and the process cannot be asked once it is signalled
			// - so a capture taken after the rollback is a capture of the rollback.
			//
			// AND A DRIVER THAT ANSWERED A STOP IS NOT AN INCIDENT AT ALL (corrected 2026-09-01).
			//
			// This ran unconditionally. Naming the cause `Stopped` rather than `DriverExited` made
			// the label honest and left every SURFACE saying the same wrong thing: `incident()`
			// answers `present: true` off `incident_report` being set, `lsdev --incident` renders
			// "nothing has gone wrong here" only for `present: false`, and `persist_incidents` writes
			// a `device.policy.incident.` row that outlives this program. So an operator who disabled
			// a device, or a machine that shut one down cleanly, found a stored report of it for the
			// rest of the boot and the next one. The arm above says in as many words that a planned
			// stop "is not a failure and must not be recorded as one"; this is where that stops being
			// a comment.
			//
			// `planned_stop` and not `node.stop_intent`: the intent says what was ASKED FOR, and a
			// driver that was asked to stop and instead died without answering is an incident - the
			// operator wanted a clean stop and did not get one. The `STOPPED` frame is what makes it
			// clean, and it is what this reads. An answered stop whose teardown then fails to confirm
			// is captured in `resolve_teardown`, where that failure becomes known.
			if !planned_stop {
				let report = capture(node, cause);
				report_incident(driver_name, &report);
				node.incident_report = Some(report);
				node.incident_stored = false;
			}
			// EVERYTHING THIS BINDING PUBLISHED GOES WITH IT, and the count is SAID.
			//
			// A provider outliving the binding that published it is a channel whose server is gone:
			// a consumer holding it waits on a driver that no longer exists, which is a failure
			// nobody can attribute. And the number is on the line because a forced teardown does not
			// know whether the work in flight on those channels completed - reporting silently
			// would let a reader assume it did.
			let published = catalogue.withdraw_binding(node.id);
			if published > 0 {
				print(b"DeviceManager: ");
				print_driver_name(driver_name);
				print(b" went away holding published providers; they are withdrawn and whatever was in flight on them is NOT confirmed\n");
			}
			// ROLL BACK WHAT THIS BINDING HELD, through the one order there is. The binding is
			// TAKEN out of the node first, so an interrupted rollback cannot be re-entered against
			// handles it has already given back.
			let Some(binding) = node.binding.take() else { return Step::Done };
			// The binding is over, so there is no running candidate any more and the cursor is the
			// only answer again. See `Node::running`. WHICH ENTRY IT WAS is kept, because the
			// answer arrives later than this: the teardown resolves on its own confirmations, and
			// `spend_candidate` needs to know what ended rather than where the cursor has since
			// been moved to. See `Node::spent`.
			node.spent = node.running.take();
			let mut txn = binding.into_attempt();
			// A PLANNED STOP IS NEVER RETRIED, whatever the cause reads as: the whole point of
			// asking a driver to stop is that it stays stopped.
			let planned: bool = node.stop_intent != driver_binding::StopIntent::Fault;
			let retryable: bool = !planned && cause.retryable() && may_try_again(&node.incident, node.attempt);
			let deadline: u64 = node.incident.teardown_deadline();
			if retryable {
				retry_or_quarantine(&mut node.record, &mut txn, &mut node.offers, &mut node.teardown, deadline, cause);
			} else {
				give_up_with(&mut node.record, &mut txn, &mut node.offers, &mut node.teardown, deadline, cause, node.stop_intent, driver_name);
			}
			// THE STOP IS DESCRIBED WHEN THE TEARDOWN ANSWERS, not now. "Stopped cleanly" is a claim
			// about a device being quiet, and at this point nothing has observed it go quiet: the
			// kill has been sent and the release started, and both confirmations are still to come.
			if let Some(teardown) = node.teardown.as_mut() {
				teardown.planned_stop = planned_stop;
				if planned_stop {
					teardown.intent = node.stop_intent;
				}
			}
			// THE NODE IS `Stopping` AND STAYS THERE until its exit and its claim have both arrived.
			// This used to be the point where the whole rollback had already run, so what came back
			// was a verdict; now it is a wait, and the verdict is `resolve_teardown`'s.
			return Step::Waiting;
		}
		// THE TEARDOWN'S OWN CONFIRMATIONS, which are events like any other. A node whose teardown
		// is still outstanding is `Stopping` and has nothing else to do.
		if node.teardown.is_some() {
			let now: u64 = clock();
			let Some(landed) = resolve_teardown(node, driver_name, now) else { return Step::Waiting };
			return match landed {
				// A confirmed teardown that intends to try again re-opens the transaction from
				// `Backoff`, which the table allows.
				BindingState::Backoff if node.record.state == BindingState::Backoff => {
					node.attempt += 1;
					node.retry_at = back_off_until(&node.incident, node.attempt);
					Step::Again
				}
				BindingState::Quarantined => Step::Done,
				// A TEARDOWN THAT WAS ASKED FOR DID NOT SPEND A CANDIDATE - see `Step::Resting`.
				// These two landings are where a PLANNED stop ends, and the entry that ran is the
				// entry the node comes back on: an enable rebinds it and a returning provider
				// restarts it. Reading them as a failed candidate advanced the cursor past the only
				// entry either revival could use.
				BindingState::Disabled | BindingState::DependencyPending => Step::Resting,
				// Everything else is this candidate finished. `Failed` ends the node; the phases
				// read `NextCandidate` as "there may be another entry to try".
				_ => Step::NextCandidate,
			};
		}
		Step::Waiting
	}
}

// THE PROVIDER CATALOGUE, SERVED.
//
// `subscribe` answers with everything of that kind published RIGHT NOW. One operation, because two
// would have a window between them: a provider added after the read and before the registration
// would be in neither, which is the same race as a service started later seeing nothing. The
// generated stream opens FROM the snapshot, so there is no second step to lose anything in.
struct CatalogueView<'a> {
	catalogue: &'a mut Catalogue,
	nodes: &'a [Node],
}

// The operator's endpoint, served by this program and reached only through the one connection
// PermissionManager mints for the operator path.
struct PolicyView<'a> {
	nodes: &'a mut [Node],
	// A DISABLE ON A RUNNING BINDING HAS TO WITHDRAW WHAT IT PUBLISHED before it asks the driver to
	// finish, so the verb needs the catalogue - see `apply_policy`.
	catalogue: &'a mut Catalogue,
	// This program's OWN ConfigService connection. The bytes live there; the decision lives here.
	config: u64,
}

impl proto::system::device_policy_admin::Service for PolicyView<'_> {
	fn apply(&mut self, index: u32, verb: proto::system::PolicyVerb, artifact: alloc::string::String) -> Result<proto::system::PolicyOutcome, proto::system::Error> {
		use proto::system::PolicyOutcome;
		let Some(node) = self.nodes.iter_mut().find(|node| node.index == index as u64) else {
			return Ok(PolicyOutcome::NoSuchDevice);
		};
		let decision = decide_policy(node, verb, &artifact);
		if decision.outcome != PolicyOutcome::Accepted {
			return Ok(decision.outcome);
		}
		// THE WRITE IS PART OF ACCEPTING IT. A verb that persists and could not be written down has
		// not been applied, and reporting otherwise would be a preference an operator believes is
		// stored and is not.
		if self.config != 0 {
			let key = policy_key(node.id);
			let mut client = proto::system::config::Client::new(ChannelTransport { chan: self.config });
			let written = if decision.remove {
				client.remove(&key).is_some()
			} else if let Some(value) = decision.store.clone() {
				client.set(&proto::system::ConfigEntry { key: key.clone(), value }).is_some()
			} else {
				true
			};
			if !written {
				return Ok(PolicyOutcome::NotStored);
			}
		} else if decision.store.is_some() || decision.remove {
			return Ok(PolicyOutcome::NotStored);
		}
		// AND NOW THE EFFECT ON THE NODE, which is the half only this program can perform.
		unsafe { apply_policy(node, verb, artifact.as_str(), self.catalogue) };
		Ok(PolicyOutcome::Accepted)
	}

	fn incident(&mut self, index: u32) -> Result<proto::system::IncidentReport, proto::system::Error> {
		let Some(node) = self.nodes.iter().find(|node| node.index == index as u64) else {
			return Err(proto::system::Error::NotFound);
		};
		// A BINDING THAT HAS NEVER FAILED ANSWERS `present: false`, not an error. "Nothing has gone
		// wrong here" is a fact an operator asked for; an error would read as "the question could
		// not be answered", which is a different thing.
		let Some(report) = node.incident_report else {
			return Ok(proto::system::IncidentReport { present: false, bus: 0, dev: 0, func: 0, generation: 0, state: proto::system::BindingState::Unbound, cause: proto::system::FailureCause::None, last_opcode: 0, silent_for: 0, attempts: 0, domain_known: false, memory_used: 0, memory_peak: 0, handles_used: 0, threads_used: 0, dma_used: 0 });
		};
		let domain = report.domain.unwrap_or_default();
		Ok(proto::system::IncidentReport { present: true, bus: report.binding.bus as u32, dev: report.binding.dev as u32, func: report.binding.func as u32, generation: report.binding.generation, state: binding_state_wire(report.state), cause: failure_cause_wire(Some(report.cause)), last_opcode: report.last_opcode as u32, silent_for: report.silent_for, attempts: report.attempts, domain_known: report.domain.is_some(), memory_used: domain.memory_used, memory_peak: domain.memory_peak, handles_used: domain.handles_used, threads_used: domain.threads_used, dma_used: domain.dma_used })
	}

	fn stored(&mut self, index: u32) -> Result<alloc::string::String, proto::system::Error> {
		let Some(node) = self.nodes.iter().find(|node| node.index == index as u64) else {
			return Err(proto::system::Error::NotFound);
		};
		if self.config == 0 {
			return Ok(alloc::string::String::new());
		}
		let key = policy_key(node.id);
		match proto::system::config::Client::new(ChannelTransport { chan: self.config }).get(&key) {
			// A DEVICE WITH NO RECORD ANSWERS EMPTY, not an error: "nothing is stored" is a fact an
			// operator asked for, and an error would read as "the question could not be answered".
			Some(Ok(value)) => Ok(value),
			_ => Ok(alloc::string::String::new()),
		}
	}
}

// START THE TEARDOWN AN OPERATOR ASKED FOR, on a binding that is running.
//
// The providers go first and the driver is asked second, which is the order `stop_all` uses and for
// the reason written there: a driver asked to finish while work keeps arriving is a driver that
// cannot finish. What this does NOT do is wait - the standing loop's own wait already watches this
// node's channel and its process, and `advance` resolves the `Stopping` against the intent recorded
// here. A verb that blocked would be the teardown-blocks-every-other-node defect with a nicer name.
// A REQUIREMENT THAT ARRIVED, AND ONE THAT WENT AWAY.
//
// `gate_on_requirements` was asked only where `start_candidate` was called, so a node parked in
// `DependencyPending` stayed there for the rest of the boot however many providers arrived
// afterwards - nothing revisited it - and an online driver whose declared requirement was withdrawn
// went on running against a provider that had gone. M6's wait-then-bind and dependency-lost
// behaviour were both a state name with no transition into or out of it.
//
// ASKED ONCE PER PASS OVER THE CATALOGUE AS IT IS, rather than wired to a publish and a withdraw
// event. There are four ways the catalogue changes - a `READY` publishing a driver's offers, an
// `OFFER` after it, a `WITHDRAW` frame, and a binding ending - and an edge per way is four places
// for the rule to differ. The condition is a property of the catalogue, so it is read from the
// catalogue; the loops that own the node array already come round for the wait.
//
// Answers how many nodes it moved, so a caller can say whether anything changed.
unsafe fn settle_dependencies(nodes: &mut [Node], catalogue: &mut Catalogue) -> usize {
	unsafe {
		let mut moved: usize = 0;
		// EVERY NODE THAT WILL LOSE ITS DEPENDENCY, WORKED OUT BEFORE ANY OF THEM IS STOPPED.
		//
		// A stop withdraws what the node published, and that withdrawal is what takes the requirement
		// away from ITS dependents - so the set is a closure and not a single pass. Computing it
		// first is what makes the order below possible: acting node by node stops each one as it is
		// discovered, which is the provider before its dependent, exactly backwards.
		moved += stop_nodes_that_lost_a_dependency(nodes, catalogue);
		for node in nodes.iter_mut() {
			// THE ENTRY THE NODE IS ACTUALLY DESCRIBED BY, not the cursor - see `Node::entry`. Read
			// from the cursor, an operator selecting a future driver applied THAT driver's
			// `requires` to the running one, which stops a binding whose own requirements are met.
			let Some(entry) = node.entry() else { continue };
			if entry.requires.is_empty() {
				continue;
			}
			let met: bool = requirements_met(entry, catalogue);
			match node.record.state {
				// WAITING, AND WHAT IT WAS WAITING FOR IS HERE. Asked for exactly one attempt - the
				// same mechanism an operator's retry uses, so a node woken by a publication and one
				// woken by a person take the same path.
				//
				// AND IT DOES NOT GO THROUGH `Unbound`, WHICH IS WHY IT NEVER WOKE (fixed
				// 2026-09-01). This asked for `DependencyPending -> Unbound` first and did the rest
				// of its work only if that transition succeeded. It cannot succeed: the record's
				// table has no such edge, deliberately, and `driver-binding` has a test named
				// `a_node_waiting_for_a_dependency_has_no_way_back_to_where_a_bind_begins` asserting
				// the refusal with the reason - "a node waiting for a provider that then goes away is
				// waiting harder, not waiting less". So `move_to` answered false, the flag was never
				// set, nothing counted the node as moved, and a driver whose declared requirement
				// arrived sat in `DependencyPending` for the rest of the boot. The requires-edge that
				// M6 asks to WAKE a node only ever put it to sleep.
				//
				// The state this node needs is `Binding`, and `DependencyPending -> Binding` IS a
				// legal edge - it is the one the table leaves open on purpose. `begin_bind` performs
				// it, so the flag alone is the whole of the work here: the standing loop consumes it,
				// calls `start_candidate`, and the ordinary bind path makes the transition. That is
				// also exactly what the operator's retry does, which is what the comment above always
				// claimed this shared with it.
				BindingState::DependencyPending if met => {
					node.attempt = 0;
					node.retry_at = 0;
					node.restart_requested = true;
					print(b"DeviceManager: ");
					print_driver_name(entry.name);
					print(
						b" was waiting for a provider it declares in `requires`, and it is here now
",
					);
					moved += 1;
				}
				// ONLINE, AND WHAT IT DECLARED IS GONE - handled by
				// `stop_nodes_that_lost_a_dependency` above, which needs the whole set before it can
				// order it. It is named here because this is the match a reader looks at to find out
				// what happens to an online node whose requirement went away.
				_ => {}
			}
		}
		moved
	}
}

// EVERY ONLINE NODE WHOSE DECLARED REQUIREMENT HAS GONE, STOPPED DEPENDENTS FIRST.
//
// Two rules meet here and the previous version kept neither.
//
// THE SET IS A CLOSURE. Stopping a node withdraws what it published, and that withdrawal is what
// takes the requirement away from whatever depended on IT. Asking `requirements_met` once per node
// against the live catalogue therefore sees only the first level: in A requires B, B requires C, the
// loss of C stopped B, and A learned about it only after B's teardown had completed and withdrawn
// B's providers - a pass later, and after the driver A was talking to had already gone. So the set
// is closed here first, discounting the providers of nodes already known to be going, and nothing is
// stopped while it is being computed.
//
// AND THE ORDER IS DEPENDENTS FIRST, by the same depth `stop_all` uses - the milestone states the
// rule once and it is not a rule about shutdown, it is a rule about taking a provider away from
// something that is using it.
//
// AND EACH ONE IS WITHDRAWN BEFORE IT IS ASKED. `begin_dependency_stop` deliberately did not, on the
// reasoning that the teardown would withdraw them anyway and withdrawing here would re-enter this
// function's own condition for the node's dependents. It would - and that re-entry IS the closure
// above, which is the thing that was missing rather than a hazard to avoid. What the omission cost
// is the rule M3 states without qualification and the operator's stop and the shutdown both keep:
// while a driver drains, its providers stayed in the catalogue, so a consumer could open a fresh
// connection to a driver that had already been told to finish.
// WHICH NODES A WITHDRAWAL CAN STOP - the ones that are USING what went away.
//
// `Online` was the whole answer, and M6's table names two: a requirement withdrawn while a node is
// still HANDSHAKING is the same loss, and the node holds the same claim. Left out, a provider that
// went away during a bind ran to completion and the dependent came online against a requirement that
// no longer held - and then stayed there, because nothing asks again after `Online` except this
// function, which had already made its pass. `Binding -> Stopping` is an edge the table has for
// exactly this, and the intent carries it on to `DependencyPending` when the teardown confirms.
//
// The table's other pre-claim edge, `Binding -> DependencyPending`, has no trigger here on purpose:
// a node is only observable in `Binding` once `begin_bind` has taken the claim and installed the
// binding - everything before that happens inside one synchronous call, and `gate_on_requirements`
// is asked at the start of it. So the pre-claim withdrawal cannot be observed from outside, and a
// branch for it would be a branch nothing can reach.
fn stoppable_on_a_lost_dependency(node: &Node) -> bool {
	matches!(node.record.state, BindingState::Online | BindingState::Binding) && node.binding.is_some()
}

unsafe fn stop_nodes_that_lost_a_dependency(nodes: &mut [Node], catalogue: &mut Catalogue) -> usize {
	unsafe {
		// AND THE ORDINARY PASS ALLOCATES NOTHING. This runs on every turn of the standing loop -
		// every catalogue query, every teardown confirmation, every deadline - and on all but a
		// handful of them no node has lost anything. The closure below needs two vectors; asking the
		// cheap question first means they are allocated on the passes that are going to use them.
		if !nodes.iter().any(|node| stoppable_on_a_lost_dependency(node) && node.entry().is_some_and(|entry| !entry.requires.is_empty() && !requirements_met(entry, catalogue))) {
			return 0;
		}
		// THE CLOSURE. A node is doomed when a kind it requires is provided by nothing that is
		// staying: `count_of` over the whole catalogue, less what the already-doomed nodes publish of
		// that kind. Every pass can only add to the set, so `nodes.len()` passes is the worst case
		// and the fixed point is reached however the nodes happen to be enumerated.
		let mut doomed: Vec<bool> = alloc::vec![false; nodes.len()];
		let mut any = false;
		for _ in 0..nodes.len() {
			let mut grew = false;
			for at in 0..nodes.len() {
				if doomed[at] || !stoppable_on_a_lost_dependency(&nodes[at]) {
					continue;
				}
				let Some(entry) = nodes[at].entry() else { continue };
				if entry.requires.is_empty() {
					continue;
				}
				let lost = entry.requires.iter().any(|&kind| {
					let leaving: usize = (0..nodes.len()).filter(|&other| doomed[other]).map(|other| catalogue.count_for(nodes[other].id, kind)).sum();
					catalogue.count_of(kind) <= leaving
				});
				if lost {
					doomed[at] = true;
					grew = true;
					any = true;
				}
			}
			if !grew {
				break;
			}
		}
		if !any {
			return 0;
		}
		let depth: Vec<usize> = dependency_depths(nodes);
		let mut order: Vec<usize> = (0..nodes.len()).filter(|&at| doomed[at]).collect();
		// Deepest first, the index breaking ties so two unrelated nodes at one level stop in a stable
		// order rather than in whatever order the sort happened to leave them.
		order.sort_by_key(|&at| (core::cmp::Reverse(depth[at]), at));
		let mut moved: usize = 0;
		for at in order {
			print(b"DeviceManager: ");
			print_driver_name(nodes[at].driver_name());
			print(b" declares a provider in `requires` that has been withdrawn; stopping it\n");
			nodes[at].stop_intent = driver_binding::StopIntent::DependencyLost;
			begin_dependency_stop(&mut nodes[at], catalogue);
			moved += 1;
		}
		moved
	}
}

// Ask an online driver to stop because what it requires has gone. The same shape as the operator's
// disable, including the withdrawal that comes first - see `stop_nodes_that_lost_a_dependency`,
// which is the only caller and which owns the ORDER the withdrawals happen in.
unsafe fn begin_dependency_stop(node: &mut Node, catalogue: &mut Catalogue) {
	unsafe {
		let Some(binding) = &node.binding else { return };
		let (channel, generation): (u64, u64) = (binding.channel, node.id.generation);
		// THE PROVIDER IS WITHDRAWN AND NEW CONNECTIONS REFUSED FIRST, so nothing arrives during the
		// drain. A driver asked to finish while work keeps being handed to it is a driver that
		// cannot finish - which is why the operator's disable and the shutdown both do this, and why
		// a planned stop that skipped it was the one intent of the four that let a consumer connect
		// to a driver already on its way out.
		catalogue.withdraw_binding(node.id);
		if !node.record.move_to(BindingState::Stopping, None) {
			print(b"DeviceManager: a lost dependency could not enter the teardown\n");
			return;
		}
		if !send_frame(channel, driver_protocol::Opcode::Stop, generation, &[], 0, 0) {
			// Its channel is already gone, so there is nobody to ask: the exit will arrive on its
			// own and `advance` resolves it against the intent just recorded.
			print(b"DeviceManager: ");
			print_driver_name(node.driver_name());
			print(b" could not be asked to stop; its exit is what will end the binding\n");
		}
	}
}

unsafe fn begin_operator_stop(node: &mut Node, catalogue: &mut Catalogue) {
	unsafe {
		let Some(binding) = &node.binding else { return };
		let (channel, generation): (u64, u64) = (binding.channel, node.id.generation);
		catalogue.withdraw_binding(node.id);
		if !node.record.move_to(BindingState::Stopping, None) {
			print(b"DeviceManager: the operator's disable could not enter the teardown\n");
			return;
		}
		if !send_frame(channel, driver_protocol::Opcode::Stop, generation, &[], 0, 0) {
			// Its channel is already gone, so there is nobody to ask: the exit will arrive on its
			// own and `advance` resolves it against the intent just recorded.
			print(b"DeviceManager: ");
			print_driver_name(node.driver_name());
			print(b" could not be asked to stop; its exit is what will end the binding\n");
		}
	}
}

// What a verb does to the node itself. The write has already happened; this is the half no other
// component can perform.
unsafe fn apply_policy(node: &mut Node, verb: proto::system::PolicyVerb, artifact: &str, catalogue: &mut Catalogue) {
	unsafe {
		use proto::system::PolicyVerb;
		match verb {
			// A DISABLE ON A RUNNING BINDING GOES THROUGH THE TEARDOWN, carrying the intent so it
			// does not rebind; one on a binding that is not running has nothing to give back.
			PolicyVerb::Disable => {
				// THE DESIRE, beside the stop. What follows takes the binding down; this is what
				// stops the next one starting, and it is what a stored record restores on the next
				// boot - see `load_stored_policy` and the check in `begin_bind`.
				node.disabled_by_policy = true;
				node.stop_intent = driver_binding::StopIntent::OperatorDisable;
				// A RUNNING BINDING IS STOPPED, NOT RELABELLED.
				//
				// This tried `Online -> Disabled` directly. The table refuses that - the path is
				// `Online -> Stopping -> Disabled`, because `Stopping` is what "there is a teardown
				// to run" means - and nothing sent `STOP`, withdrew the providers or enqueued
				// anything. So the refusal was silent, the loop went back to heartbeats, and the
				// driver an operator had just disabled stayed online. The stop the shutdown path
				// performs is the same stop this verb needs.
				if node.record.state == BindingState::Online {
					begin_operator_stop(node, catalogue);
					return;
				}
				if !node.record.move_to(BindingState::Disabled, None) {
					print(b"DeviceManager: the operator's disable is queued behind this binding's teardown\n");
				}
			}
			// ENABLE GOES TO `Unbound`, which is what INVITES the bind an enable is asking for.
			PolicyVerb::Enable => {
				// AND ENABLE IS WHAT LIFTS IT. An enable that moved the record and left the desire
				// would be a node that binds once and is refused for ever after.
				node.disabled_by_policy = false;
				node.stop_intent = driver_binding::StopIntent::Fault;
				// AND SOMETHING HAS TO PERFORM THE BIND IT INVITES - WHEN THERE IS ONE TO PERFORM.
				//
				// `Unbound` is the state that invites a bind; nothing in the standing loop starts a
				// candidate for a node just because it is in that state, so an accepted enable
				// brought nothing back. Same mechanism the operator retry uses.
				//
				// THE MOVE'S ANSWER IS READ, WHICH IT WAS NOT. Enabling a node that is still ONLINE
				// - the ordinary case for a disable that was stored while the driver was running -
				// has no `Online -> Unbound` edge, so the record stayed where it was and the restart
				// was requested anyway. There is nothing to restart on a device that is already
				// running, and asking for one spent its candidates against a state no bind can start
				// from. The desire is lifted either way; only a node that actually reached `Unbound`
				// gets an attempt.
				if node.record.move_to(BindingState::Unbound, None) {
					node.attempt = 0;
					node.incident = Incident::open();
					// AND THE CURSOR IS REWOUND WHEN THERE IS NOTHING LEFT TO TRY, exactly as
					// `PolicyVerb::Retry` does it and for the same reason: `start_candidate` returns
					// immediately at an exhausted cursor, so an enable that only lifted the desire
					// and asked for a restart would report `Accepted` and start nothing. To the
					// operator's own preference where there is one - a stored `select` is a choice
					// about which driver, and it outlives the disable that came after it.
					if node.candidate >= node.candidates.len() {
						node.candidate = node.preferred.unwrap_or(0);
					}
					node.restart_requested = true;
				} else {
					print(b"DeviceManager: the stored disable is lifted; this device is not stopped, so nothing is restarted\n");
				}
			}
			// SELECT MOVES THE CURSOR NOW, AND THAT IS WHAT "APPLIES AT THE NEXT BIND" MEANS.
			//
			// This did nothing at all, on the reasoning that the record is stored and the stored
			// record is read at startup - which makes a selection apply at the next BOOT, not the
			// next bind. `load_stored_policy` runs once, when the ConfigService connection arrives,
			// and nothing reruns it; an operator who selected a driver and then stopped and started
			// the device got the registry order again, and the contract this milestone states is
			// "the next bind".
			//
			// The cursor IS the preference - `load_stored_policy` expresses a stored `select=` by
			// setting exactly this field - so applying it here and applying it at startup are the
			// same operation on the same state, which is what keeps the two paths from disagreeing.
			// The candidate was already validated against this node's list by `decide_policy`, which
			// refuses an artifact the image never declared for this device.
			//
			// It still does NOT disturb a running binding: moving the cursor changes which candidate
			// the NEXT bind starts from and touches neither the record nor the live driver.
			PolicyVerb::Select => match candidate_position(node, artifact.as_bytes()) {
				Some(at) => {
					node.candidate = at;
					node.preferred = Some(at);
					// AND IT SURVIVES THE END OF WHATEVER IS RUNNING - see `spend_candidate`. The
					// cursor alone could not say it had been moved on purpose.
					node.selection_pending = true;
				}
				// Unreachable through the served verb, which validated it; a caller that reaches
				// here with an unknown artifact changes nothing rather than silently rewinding.
				None => print(b"DeviceManager: the selected artifact is not a candidate for this device; the cursor is unchanged\n"),
			},
			// A RETRY GRANTS EXACTLY ONE FURTHER ATTEMPT and does not reset the automatic budget:
			// without that rule the two mechanisms meet in the table with nothing said, and whoever
			// implements it decides for themselves whether an operator can spend the budget again.
			PolicyVerb::Retry => {
				// EXACTLY ONE FURTHER ATTEMPT, AND IT IS ACTUALLY OPENED.
				//
				// This subtracted from the counter and replaced the incident, and left the record in
				// `Failed` - so it granted zero attempts, not one: nothing performed the legal
				// `Failed -> Binding` transition and nothing called a bind path. The standing loop
				// starts a candidate for a node it finds in `Backoff`, which is where a retry with a
				// budget belongs; from there the ordinary machinery runs exactly one attempt, because
				// the incident is fresh and `Retry` does not reset the automatic budget.
				// EXACTLY ONE, COUNTED FROM THE BOUND RATHER THAN FROM WHATEVER THE COUNTER HOLDS
				// (corrected 2026-08-31).
				//
				// `attempt.saturating_sub(1)` assumed the counter was at the bound, and on the case
				// this verb exists for it is at ZERO: `Step::NextCandidate` resets `attempt` to 0
				// every time it advances the cursor, including the advance PAST the final candidate
				// that records exhaustion. So a retry after exhaustion subtracted one from zero,
				// saturated at zero, and handed the node the whole automatic budget again - three
				// further attempts where the operator asked for one, and the comment two lines up
				// promising it "does not reset the automatic budget" was describing the opposite of
				// what happened.
				//
				// Set rather than decremented: `may_try_again` allows another attempt while
				// `attempt + 1 < MAX_AUTOMATIC_ATTEMPTS`, so leaving exactly one means starting from
				// one below the bound whatever the counter happened to be.
				//
				// THE ARITHMETIC IS THE LIBRARY'S (2026-09-02) - see `driver_binding::one_more_attempt`.
				// Both of the corrections recorded above were arithmetic mistakes in code no host
				// test could reach; the rule is one place now, with a test that says "exactly one".
				let granted = driver_binding::one_more_attempt(node.candidate, node.candidates.len(), node.preferred, MAX_AUTOMATIC_ATTEMPTS);
				node.attempt = granted.attempt;
				node.incident = Incident::open();
				// AND THE CURSOR IS REWOUND WHEN THERE IS NOTHING LEFT TO TRY, which is the case this
				// granted zero attempts in.
				//
				// `Step::NextCandidate` advances `node.candidate` past the final entry, and that is
				// how a node records "every candidate has been tried". `start_candidate` returns
				// immediately for a cursor in that state - correctly, since it is the terminal
				// condition - so a retry that only opened an incident and asked the loop to start a
				// candidate asked it to start nothing. An operator saw `Accepted` and nothing
				// happened, which is the one outcome a policy verb may not produce.
				//
				// Rewound TO THE SELECTED CANDIDATE where there is one, and to the registry order
				// otherwise (corrected 2026-08-30).
				//
				// This rewound to zero unconditionally, and said the stored preference would be
				// re-applied by `load_stored_policy` "on the next start" - which is the next BOOT.
				// So an operator who selected a driver, watched it exhaust its candidates and asked
				// for a retry got the registry order instead of their choice, on the one verb whose
				// whole purpose is "try again". The preference lives in `node.preferred`, which both
				// the stored-policy load and the live `select` verb set, so a retry consults the same
				// field rather than inventing which entry was meant.
				node.candidate = granted.candidate;
				// AND THE ONE ATTEMPT IS THE WHOLE REQUEST, NOT ONE PER CANDIDATE (corrected
				// 2026-09-01).
				//
				// Setting `attempt` to one below the bound spends the automatic budget for THIS
				// candidate, and that is as far as a counter can reach: when the attempt fails,
				// `advance` answers `Step::NextCandidate`, both loop handlers advance the cursor,
				// reset `attempt` to zero and start the next entry with a full budget. A device with
				// three candidates therefore answered "try once" with one attempt plus six more. The
				// flag says the request is spent whatever the cursor does, and the handlers read it
				// where they would otherwise walk on.
				node.retry_once = true;
				// ASKED FOR, AND THE LOOP PERFORMS IT. A state change alone would not do: `advance`
				// is event-driven and a node sitting in `Failed` or `Backoff` raises no event, so
				// nothing would ever start the attempt. The flag is consumed once by the standing
				// loop, which calls `start_candidate` - and `Failed -> Binding` is a legal edge, so
				// the ordinary bind path performs the transition rather than this verb faking it.
				node.restart_requested = true;
			}
		}
	}
}

impl proto::system::provider_catalogue::Service for CatalogueView<'_> {
	// EVERY DEVICE NODE'S BINDING, IN ONE READ, so `lsdev` and the System Graph render the same
	// enums rather than each deriving a state. The graph's unconditional `Running` was not a bug in
	// the graph so much as the absence of anything else to say.
	fn bindings(&mut self) -> Vec<proto::system::BindingRecord> {
		self.nodes
			.iter()
			.map(|node| proto::system::BindingRecord {
				index: node.index as u32,
				bus: node.id.bus as u32,
				dev: node.id.dev as u32,
				func: node.id.func as u32,
				generation: node.id.generation,
				state: binding_state_wire(node.record.state),
				cause: failure_cause_wire(node.record.failure),
				attempts: node.record.attempts,
				// The driver chosen for it, or empty where nothing matched - which is a fact about
				// the machine and not an absence.
				artifact: alloc::string::String::from_utf8_lossy(node.driver_name()).into_owned(),
				rule: node.matched_rule,
				providers: self.catalogue.count_for_binding(node.id) as u32,
				resources: node.granted_resources,
			})
			.collect()
	}

	// A CONNECTION TO ONE PROVIDER, MINTED FOR THIS CONSUMER.
	//
	// The manager makes the pair, hands the SERVER end to the driver in a `CONNECT` frame and
	// answers with the client end. That direction is the whole reason this needs no reply from the
	// driver: capabilities already travel manager-to-driver for every resource a bind hands over, so
	// this is that mechanism rather than a new round trip with its own half-way failure.
	fn open(&mut self, provider: proto::system::ProviderInfo) -> Result<u64, proto::system::Error> {
		unsafe {
			let wire: u16 = provider_kind_wire(provider.kind);
			// THE PROVIDER THIS NAMES, by the identity the manager minted - not by kind and not by
			// position. A consumer holding a stale `provider-info` names a slot that has been reused
			// and its generation is what says so.
			let Some(slot) = self.catalogue.entries.iter().position(|entry| entry.as_ref().is_some_and(|held| held.kind == wire && held.id.slot as u32 == provider.slot && held.id.generation == provider.provider_generation && held.id.binding.generation == provider.binding_generation)) else {
				return Err(proto::system::Error::NotFound);
			};
			// WHAT ITS DRIVER DECLARED. A kind that admits one consumer is refused here, at the ask,
			// which is the difference between a consumer that knows and one that waits for ever.
			let (token, admits) = {
				let held = self.catalogue.entries[slot].as_ref().expect("the slot was just found");
				// The publisher's own declaration, from the entry it is RUNNING - see `Node::entry`
				// and the same read in `mint_connection`.
				let admits = self.nodes.iter().find(|node| node.id.same_function(held.id.binding) && node.id.generation == held.id.binding.generation).and_then(|node| node.entry()).and_then(|entry| entry.provides.iter().find(|&&(kind, _, _)| kind == held.kind)).map_or(1, |&(_, _, consumers)| consumers);
				(held.token, admits)
			};
			// EXISTING PLUS PROMISED, not just handed out - see `outstanding`. The branch below hands
			// out the offered channel, which is already inside this number and does not add to it.
			if self.catalogue.entries[slot].as_ref().is_some_and(|held| outstanding(held) > admits || (held.handle == 0 && held.consumers >= admits)) {
				print(b"DeviceManager: a provider was asked for one more consumer than its driver declares it admits; refused\n");
				return Err(proto::system::Error::Denied);
			}
			// THE OFFERED CHANNEL IS THE FIRST CONNECTION, AND THIS IS WHERE IT IS HANDED OUT.
			//
			// A publication carries the endpoint the driver made itself, and it sat in the entry
			// reachable only through the private `Catalogue::take` - the hand-written routing this
			// milestone exists to delete. So the PUBLIC factory could not deliver the first
			// connection for the default single-consumer provider at all: minting a second one for a
			// driver that declares it serves one is exactly what the bound above refuses, and the
			// one it may serve was being held back.
			//
			// Moved rather than duplicated, like `take`: a duplicate shares the driver's reply queue
			// with whoever holds the original, which is the failure the per-consumer factory exists
			// to prevent.
			if let Some(held) = self.catalogue.entries[slot].as_mut()
				&& held.handle != 0
			{
				let offered = held.handle;
				held.handle = 0;
				held.consumers += 1;
				return Ok(offered);
			}
			// THE BINDING THAT PUBLISHED IT has to still be the one that is bound: a `CONNECT` sent
			// on a channel whose generation has moved on is a frame the driver drops, and answering
			// with a client end nobody will ever serve is the failure this refuses instead.
			let binding = self.catalogue.entries[slot].as_ref().map(|held| held.id.binding).expect("the slot was just found");
			let Some((control, generation)) = self.nodes.iter().find(|node| node.id.same_function(binding) && node.id.generation == binding.generation).and_then(|node| node.binding.as_ref().map(|live| (live.channel, node.id.generation))) else {
				return Err(proto::system::Error::NotFound);
			};
			// NOT `channel` - that is the driver's control channel, shadowed just above. The pair.
			let Some((server, client)) = channel() else { return Err(proto::system::Error::Exhausted) };
			let mut payload = [0u8; driver_protocol::OFFER_PAYLOAD_LEN];
			payload[..2].copy_from_slice(&token.to_le_bytes());
			// THE WHOLE ENDPOINT, unattenuated: a server end with rights taken off it is one the
			// driver cannot answer on, which is the same as not sending it.
			if !send_frame(control, driver_protocol::Opcode::Connect, generation, &payload[..2], server, u32::MAX) {
				close(server);
				close(client);
				return Err(proto::system::Error::Closed);
			}
			if let Some(held) = self.catalogue.entries[slot].as_mut() {
				held.consumers += 1;
			}
			Ok(client)
		}
	}

	fn subscribe(&mut self, kind: proto::system::ProviderKind) -> Vec<proto::system::ProviderInfo> {
		let wire: u16 = provider_kind_wire(kind);
		self.catalogue.entries.iter().filter_map(|entry| entry.as_ref()).filter(|provider| provider.kind == wire).map(|provider| proto::system::ProviderInfo { kind, bus: provider.id.binding.bus as u32, dev: provider.id.binding.dev as u32, func: provider.id.binding.func as u32, binding_generation: provider.id.binding.generation, slot: provider.id.slot as u32, provider_generation: provider.id.generation, live: true }).collect()
	}
}

// ------------------------------------------------------- the operator's four verbs
//
// DEVICEMANAGER OWNS THE DECISION; CONFIGSERVICE HOLDS THE BYTES. That division is the whole of the
// authority question here, and it is not made true by saying it: `ConfigService::set` accepts any
// key from any client holding a connection, and `CAP_CONFIG` is granted to three services today.
// Any of them could store a WELL-FORMED disable and the load-time re-check would pass it, because
// that check catches STALENESS and never asks who wrote the record.
//
// So the four verbs live on one narrow endpoint of this program's, and the key prefix they persist
// under is writable by this program alone - `ConfigService::remove` refuses anything outside it.
// Holding `CAP_CONFIG`, or holding the read-only binding snapshot, gets a component no closer.

// The reserved prefix. One path, no ACL, no policy language.
const DEVICE_POLICY_PREFIX: &[u8] = b"device.policy.";

// What the operator asked, applied to one node.
struct PolicyDecision {
	outcome: proto::system::PolicyOutcome,
	// The record to write, or None. `enable` writes nothing - it REMOVES, which is a different
	// operation and the reason `ConfigService` grew one.
	store: Option<alloc::string::String>,
	remove: bool,
}

// WHICH CANDIDATE AN ARTIFACT NAMES ON THIS NODE, through the library's narrowing rule.
//
// The names are collected onto the stack because `driver_binding::selected_candidate` takes a slice
// of them and a node's candidates are a slice of entries. A node's list is a FILTERED SUBSET of
// `DRIVER_REGISTRY`, so the registry's own length bounds it - a compile-time constant, which is what
// makes this allocation-free and gives it no failure path.
fn candidate_position(node: &Node, artifact: &[u8]) -> Option<usize> {
	let mut names: [&[u8]; DRIVER_REGISTRY.len()] = [b""; DRIVER_REGISTRY.len()];
	let count = node.candidates.len().min(DRIVER_REGISTRY.len());
	for (at, entry) in node.candidates.iter().take(count).enumerate() {
		names[at] = entry.name;
	}
	driver_binding::selected_candidate(&names[..count], artifact)
}

// Decide what a verb does to this node, WITHOUT applying it.
//
// Separated so the decision can be read on its own: what a verb means is a question about the
// binding's state, and what it costs is a question about ConfigService.
fn decide_policy(node: &Node, verb: proto::system::PolicyVerb, artifact: &str) -> PolicyDecision {
	use proto::system::{PolicyOutcome, PolicyVerb};
	let none = |outcome: PolicyOutcome| PolicyDecision { outcome, store: None, remove: false };
	// BOOT-CRITICAL BINDINGS ARE OUT. Their policy would live on a volume that is not mounted when
	// those bindings are made, which is a dependency the wrong way round.
	let boot_critical: bool = node.candidates.first().is_some_and(|entry| entry.boot_critical);
	if boot_critical {
		return none(PolicyOutcome::Refused);
	}
	match verb {
		PolicyVerb::Retry => {
			// THE RULE IS THE LIBRARY'S (2026-09-02) - see `driver_binding::decide_retry`. Which
			// states admit a retry is arithmetic about a binding's lifecycle, and it lived here,
			// where no host test could reach it; what stays is the mapping onto this protocol's
			// outcomes, which is a table and not a decision.
			//
			// QUARANTINE IS OUT OF REACH OF THIS ONE, for this boot. Its resources are charged and
			// out of circulation precisely because nothing confirmed the device was quiet, and an
			// operator saying so does not make it so.
			//
			// AND THERE HAS TO BE SOMETHING TO RETRY (added 2026-08-31).
			//
			// Quarantine was the only state this refused, so `retry` was accepted on a node that is
			// ONLINE, still BINDING, in the middle of a teardown, waiting on a dependency or
			// operator-disabled - none of which is a binding that failed. On an online node it opened
			// a fresh incident, rewound the cursor and asked the loop to start a candidate for a
			// device that already has a driver running; on a disabled one it argued with the verb
			// that actually applies, which is `enable`.
			//
			// The three states where a retry means something are the ones where an attempt has ENDED
			// without a binding: `Failed`, `Backoff`, and `Unbound` with the candidates exhausted.
			// `busy` is the answer for the rest - the same "not now, and here is why" the enable path
			// gives - because the operator's next move is to look at the state rather than to try a
			// different verb.
			match driver_binding::decide_retry(node.record.state, boot_critical) {
				// AN ACTION, NOT A STORED PREFERENCE. Nothing is written.
				driver_binding::RetryVerdict::Grant => none(PolicyOutcome::Accepted),
				driver_binding::RetryVerdict::Quarantined => none(PolicyOutcome::Quarantined),
				driver_binding::RetryVerdict::Busy => none(PolicyOutcome::Busy),
				driver_binding::RetryVerdict::Refused => none(PolicyOutcome::Refused),
			}
		}
		PolicyVerb::Enable => {
			// AN ENABLE WHILE A DISABLE'S TEARDOWN IS STILL IN FLIGHT IS BUSY. Cancelling the intent
			// would mean a device that was never observed to be disabled at all, which is worse to
			// explain than a refusal that says why.
			if node.record.state == BindingState::Stopping && node.stop_intent == driver_binding::StopIntent::OperatorDisable {
				return none(PolicyOutcome::Busy);
			}
			// NOT A THIRD STORED STATE: the REMOVAL of the disable record, so a device that was
			// never disabled and one that was enabled again are the same device.
			PolicyDecision { outcome: PolicyOutcome::Accepted, store: None, remove: true }
		}
		PolicyVerb::Disable => PolicyDecision { outcome: PolicyOutcome::Accepted, store: Some(alloc::string::String::from("disabled")), remove: false },
		PolicyVerb::Select => {
			// POLICY NARROWS AND NEVER WIDENS. An artifact the registry did not declare for THIS
			// device is refused rather than obeyed - the whole point of bounding a preference by the
			// candidate list is that an operator cannot name a driver the image never offered.
			if candidate_position(node, artifact.as_bytes()).is_none() {
				return none(PolicyOutcome::NotACandidate);
			}
			// AND IT APPLIES AT THE NEXT BIND, never to the binding that is running: rebinding a
			// live device because a preference changed would take a working driver away at a moment
			// nobody chose. The operator who wants that has `disable` followed by `enable`.
			let mut record = alloc::string::String::from("select=");
			record.push_str(artifact);
			PolicyDecision { outcome: PolicyOutcome::Accepted, store: Some(record), remove: false }
		}
	}
}

// EVERY NODE'S STORED POLICY, READ BACK AND APPLIED. Called when the ConfigService connection arrives.
//
// The policy was WRITE-ONLY: `PolicyView::apply` wrote `device.policy.<bdf>` and nothing ever read it
// again - not at startup, not at a reconnect, not at candidate selection - so a disable an operator
// stored did not survive a reboot and a selected artifact was never preferred. The two stored shapes
// are the two `decide_policy` writes: `disabled`, and `select=<artifact>`.
//
// A STALE CHOICE IS SAID, NOT SILENTLY DROPPED. An artifact this image no longer declares for this
// device cannot be honoured - policy narrows and never widens - and an operator whose stored
// preference has stopped applying is exactly who needs to be told.
unsafe fn load_stored_policy(nodes: &mut [Node], config: u64) {
	unsafe {
		if config == 0 {
			return;
		}
		for node in nodes.iter_mut() {
			let key = policy_key(node.id);
			let mut client = proto::system::config::Client::new(ChannelTransport { chan: config });
			let Some(Ok(entry)) = client.get(&key) else { continue };
			let value = entry.as_bytes();
			if value == b"disabled" {
				// THE DESIRE IS RECORDED WHETHER OR NOT IT CAN BE APPLIED NOW. This was the move
				// alone, with its refusal ignored - and a node already `Online` cannot move to
				// `Disabled`, so every driver bound before ConfigService answered kept its record
				// read and forgotten.
				node.disabled_by_policy = true;
				// A device that has nothing bound is disabled immediately: `Unbound -> Disabled` is
				// in the table, which is the edge for a node with nothing to tear down.
				if node.record.move_to(BindingState::Disabled, None) {
					print(b"DeviceManager: ");
					print_driver_name(node.driver_name());
					print(b" stays unbound - an operator disabled it and that is stored\n");
				} else {
					print(b"DeviceManager: ");
					print_driver_name(node.driver_name());
					print(b" is disabled in stored policy and is already bound; it stays up and will not bind again\n");
				}
				continue;
			}
			let Some(wanted) = value.strip_prefix(b"select=") else { continue };
			match node.candidates.iter().position(|entry| entry.name == wanted) {
				Some(at) => {
					node.candidate = at;
					// AND IT SURVIVES A REWIND. `Retry` after exhaustion resets the cursor, and
					// without the preference recorded beside it that reset handed the operator the
					// registry order instead of their stored choice.
					node.preferred = Some(at);
					print(b"DeviceManager: ");
					print_driver_name(wanted);
					print(b" is the stored choice for this device and is tried first\n");
				}
				None => {
					print(b"DeviceManager: the stored choice ");
					print_driver_name(wanted);
					print(b" is not a candidate this image declares for that device any more; it is STALE and the registry order applies\n");
				}
			}
		}
	}
}

// WRITE EVERY NEW INCIDENT SOMEWHERE THAT OUTLIVES THIS PROGRAM.
//
// The report lived only in `Node::incident_report`, and the endpoint that serves it is this program's
// - so when DeviceManager died, the endpoint and the sole copy went together and the last thing that
// went wrong before it died was exactly what nobody could read. M5 asks for the snapshot to remain
// visible after both the driver AND the manager have died.
//
// ConfigService is where it goes, because it is the one component that already outlives this one and
// already holds this program's per-device records. The value is a compact text row rather than the
// wire struct: whoever reads it after this program is gone is reading it through an ordinary
// configuration `get`, not through a typed endpoint that no longer exists.
fn persist_incidents(nodes: &mut [Node], config: u64) {
	{
		if config == 0 {
			return;
		}
		for node in nodes.iter_mut() {
			if node.incident_stored {
				continue;
			}
			let Some(report) = node.incident_report else { continue };
			let mut value = alloc::string::String::new();
			let mut number = [0u8; 20];
			let mut push_number = |value: &mut alloc::string::String, n: u64| {
				let written = decimal(n, &mut number);
				value.push_str(core::str::from_utf8(&number[..written]).unwrap_or("0"));
			};
			value.push_str("cause=");
			value.push_str(core::str::from_utf8(report.cause.name()).unwrap_or("?"));
			value.push_str(" state=");
			value.push_str(core::str::from_utf8(report.state.name()).unwrap_or("?"));
			value.push_str(" gen=");
			push_number(&mut value, report.binding.generation);
			value.push_str(" attempts=");
			push_number(&mut value, report.attempts as u64);
			value.push_str(" last-opcode=");
			push_number(&mut value, report.last_opcode as u64);
			value.push_str(" silent-for=");
			push_number(&mut value, report.silent_for);
			// AND THE WHOLE TYPED RECORD, not the half of it that fitted a summary line.
			//
			// This wrote cause, state, generation, attempts, opcode and silence and dropped the
			// device's address and every Domain counter - so the persisted copy could not answer
			// what the live endpoint answers, and "the snapshot outlives the manager" was true of a
			// summary rather than of the report. A reader that finds fewer fields than the schema
			// has cannot tell a record written before this from one whose driver had no Domain.
			// THE ROW NUMBER IS NOT RECORDED, AND WAS (corrected 2026-08-30).
			//
			// It was added so `lsdev --incident N` could find one record rather than list them all,
			// and it made the record wrong in a way listing never was. The key is the device's
			// ADDRESS because that is what identifies it across boots; `node.index` is this boot's
			// position in an inventory whose order and membership can change. A record written when
			// device 3 was the NIC, read in a boot where row 3 is the audio controller, matched -
			// and several old records could match one row. A field that is only correct until the
			// next boot is worse in a record whose whole purpose is to outlive the manager.
			//
			// The address below is what a reader matches on now, and `lsdev` asks by address on the
			// path where the manager is gone. Where the manager is alive it answers by index itself,
			// from the live inventory that gives the number meaning.
			value.push_str(" at=");
			push_number(&mut value, report.binding.bus as u64);
			value.push(':');
			push_number(&mut value, report.binding.dev as u64);
			value.push('.');
			push_number(&mut value, report.binding.func as u64);
			// `domain=` is absent when the Domain was already gone or never made, which is itself
			// worth recording rather than reporting zeros - the same distinction the live report
			// carries as `domain_known`.
			if let Some(domain) = report.domain {
				value.push_str(" domain=1 memory=");
				push_number(&mut value, domain.memory_used);
				value.push_str(" peak=");
				push_number(&mut value, domain.memory_peak);
				value.push_str(" handles=");
				push_number(&mut value, domain.handles_used);
				value.push_str(" threads=");
				push_number(&mut value, domain.threads_used);
				value.push_str(" dma=");
				push_number(&mut value, domain.dma_used);
			} else {
				value.push_str(" domain=0");
			}
			let key = incident_key(node.id);
			let mut client = proto::system::config::Client::new(ChannelTransport { chan: config });
			if client.set(&proto::system::ConfigEntry { key, value }).is_none() {
				continue;
			}
			// AND NOTHING WRITES A ROW MAP ANY MORE (removed 2026-08-31).
			//
			// A second record used to be written here, mapping row number to
			// `<mmio-len>.<bus>.<dev>.<func>`, so `lsdev --incident N` could turn a row into an
			// address with this program gone. It was validated by comparing the stored MMIO length
			// against what the kernel reports for that row - and a window length is not an identity:
			// two devices of one model share it, so equal-sized devices reordering between
			// inventories resolved a row to another device's incident while passing the check.
			//
			// `device-entry` now carries the address from the kernel's own table, so the reader asks
			// the machine instead. This record's only purpose was to answer a question the kernel can
			// answer, and a persisted copy of a fact the authority still holds is one more thing that
			// can be wrong.
			node.incident_stored = true;
		}
	}
}

// EVERY STORED INCIDENT FOR A DEVICE THIS MACHINE NO LONGER HAS, REMOVED.
//
// The records outlive this program deliberately, and nothing ever took one away - so a machine that
// lost a card kept its last incident for ever, and `lsdev`'s fallback listed it beside the live ones
// with nothing to say which was which. M4 makes this program the owner of the reserved prefix, and
// owning a set of records includes deciding when one stops describing anything.
//
// Run when the ConfigService connection arrives, beside `load_stored_policy`, which is the one moment
// this program has both the inventory and somewhere to write. THE POLICY RECORDS ARE NOT SWEPT: a
// disable stored for a device that is unplugged today is a preference for when it comes back, which
// is exactly what persisting it is for. An incident is a description of something that happened to a
// device that was here.
unsafe fn forget_absent_incidents(nodes: &[Node], config: u64) {
	unsafe {
		if config == 0 {
			return;
		}
		let mut client = proto::system::config::Client::new(ChannelTransport { chan: config });
		let Some(Ok(entries)) = client.list() else { return };
		let mut stale: Vec<alloc::string::String> = Vec::new();
		for entry in entries.iter() {
			// A ROW MAP FROM A BOOT THAT STILL WROTE THEM. Nothing writes these any longer - the
			// address comes from the kernel's device table now, see `persist_incidents` - so every
			// one of them is a leftover and all of them go, rather than the ones whose row number
			// happens not to exist. Keeping a row map alive because its NUMBER still exists is what
			// let a stale one outlive the device it described.
			if entry.key.starts_with("device.policy.incident-at.") {
				stale.push(entry.key.clone());
				continue;
			}
			let Some(rest) = entry.key.strip_prefix("device.policy.incident.") else { continue };
			if !nodes.iter().any(|node| incident_key(node.id).ends_with(rest) && incident_key(node.id).len() == "device.policy.incident.".len() + rest.len()) {
				stale.push(entry.key.clone());
			}
		}
		for key in stale {
			print(b"DeviceManager: a stored incident names a device this machine no longer has; it is removed rather than listed beside the live ones\n");
			let _ = client.remove(&key);
		}
	}
}

// Where one device's last incident is kept, under the same reserved prefix as its policy so both are
// the records this program owns.
fn incident_key(id: BindingId) -> alloc::string::String {
	let mut key = alloc::string::String::from("device.policy.incident.");
	let mut number = [0u8; 20];
	for (at, part) in [id.bus as u64, id.dev as u64, id.func as u64].into_iter().enumerate() {
		if at > 0 {
			key.push('.');
		}
		let n = decimal(part, &mut number);
		key.push_str(core::str::from_utf8(&number[..n]).unwrap_or("0"));
	}
	key
}

// The config key one device's policy lives under. The BDF, because that is the device's identity -
// a row number would name whatever the table happened to hold this boot.
fn policy_key(id: BindingId) -> alloc::string::String {
	let mut key = alloc::string::String::from_utf8_lossy(DEVICE_POLICY_PREFIX).into_owned();
	let mut number = [0u8; 20];
	for (at, part) in [id.bus as u64, id.dev as u64, id.func as u64].into_iter().enumerate() {
		if at > 0 {
			key.push('.');
		}
		let n = decimal(part, &mut number);
		key.push_str(core::str::from_utf8(&number[..n]).unwrap_or("0"));
	}
	key
}

// The typed state a binding state is. One mapping, in the one place the two vocabularies meet, so a
// divergence is a compile error rather than a surface quietly reporting the wrong thing.
fn binding_state_wire(state: BindingState) -> proto::system::BindingState {
	match state {
		BindingState::Unbound => proto::system::BindingState::Unbound,
		BindingState::DependencyPending => proto::system::BindingState::DependencyPending,
		BindingState::Binding => proto::system::BindingState::Binding,
		BindingState::Online => proto::system::BindingState::Online,
		BindingState::Stopping => proto::system::BindingState::Stopping,
		BindingState::Backoff => proto::system::BindingState::Backoff,
		BindingState::Failed => proto::system::BindingState::Failed,
		BindingState::Quarantined => proto::system::BindingState::Quarantined,
		BindingState::Disabled => proto::system::BindingState::Disabled,
	}
}

// The same for the cause. `None` is what a binding that has not failed says - an absent field would
// be one every reader has to guess about.
fn failure_cause_wire(cause: Option<FailureCause>) -> proto::system::FailureCause {
	match cause {
		None => proto::system::FailureCause::None,
		Some(FailureCause::DriverMissing) => proto::system::FailureCause::DriverMissing,
		Some(FailureCause::ProtocolMismatch) => proto::system::FailureCause::ProtocolMismatch,
		Some(FailureCause::ClaimRefused) => proto::system::FailureCause::ClaimRefused,
		Some(FailureCause::IommuRequired) => proto::system::FailureCause::IommuRequired,
		Some(FailureCause::ResourceExhausted) => proto::system::FailureCause::ResourceExhausted,
		Some(FailureCause::SpawnFailed) => proto::system::FailureCause::SpawnFailed,
		Some(FailureCause::HandshakeTimeout) => proto::system::FailureCause::HandshakeTimeout,
		Some(FailureCause::DriverExited) => proto::system::FailureCause::DriverExited,
		Some(FailureCause::DriverReported(_)) => proto::system::FailureCause::DriverReportedFailure,
		Some(FailureCause::TeardownUnconfirmed) => proto::system::FailureCause::TeardownUnconfirmed,
		Some(FailureCause::Hung) => proto::system::FailureCause::Hung,
		Some(FailureCause::Stopped) => proto::system::FailureCause::Stopped,
	}
}

// The wire number the driver protocol gives a typed kind. The two sets are the same set spelled
// twice - once in the IDL for clients and once in `driver_protocol` for the wire - and this is the
// one place they meet, so a divergence is a compile error here rather than a provider nobody finds.
// The other direction, for a frame that describes a provider the catalogue holds by its wire kind.
// A kind the wire does not name cannot be in the catalogue - `publish_all` refuses one the entry
// does not declare, and the registry's kinds are these - so the fallback is unreachable and is
// `Block` rather than a panic in a supervisor.
fn provider_kind_from_wire(kind: u16) -> proto::system::ProviderKind {
	use driver_protocol::provider;
	match kind {
		provider::NET => proto::system::ProviderKind::Net,
		provider::DISPLAY => proto::system::ProviderKind::Display,
		provider::AUDIO => proto::system::ProviderKind::Audio,
		provider::INPUT => proto::system::ProviderKind::Input,
		provider::USB_BUS => proto::system::ProviderKind::UsbBus,
		provider::POINTER => proto::system::ProviderKind::Pointer,
		provider::CONSOLE_BYTES => proto::system::ProviderKind::ConsoleBytes,
		_ => proto::system::ProviderKind::Block,
	}
}

fn provider_kind_wire(kind: proto::system::ProviderKind) -> u16 {
	match kind {
		proto::system::ProviderKind::Block => driver_protocol::provider::BLOCK,
		proto::system::ProviderKind::Net => driver_protocol::provider::NET,
		proto::system::ProviderKind::Display => driver_protocol::provider::DISPLAY,
		proto::system::ProviderKind::Audio => driver_protocol::provider::AUDIO,
		proto::system::ProviderKind::Input => driver_protocol::provider::INPUT,
		proto::system::ProviderKind::UsbBus => driver_protocol::provider::USB_BUS,
		proto::system::ProviderKind::Pointer => driver_protocol::provider::POINTER,
		proto::system::ProviderKind::ConsoleBytes => driver_protocol::provider::CONSOLE_BYTES,
	}
}

// Answer one catalogue request. Non-blocking by construction: the caller only reaches this when the
// channel is ready, and one request is one reply.
// Answer one operator request. The endpoint is narrow by design and single-client: exactly one
// connection is minted for the operator path, so there is nothing here to multiplex.
// Answers false when the peer has gone, which is what lets the standing loop give the slot back -
// see `CatalogueClients::retire`. It returned nothing, so a policy client that exited was waited on
// for the rest of the boot exactly as a catalogue client was.
unsafe fn serve_policy_once(service: u64, is_root: bool, clients: &mut CatalogueClients, nodes: &mut [Node], catalogue: &mut Catalogue, config: u64, buf: &mut [u8]) -> bool {
	unsafe {
		let ReceivedCaps::Message { len, handles } = recv_caps_blocking(service, buf) else { return false };
		for &handle in handles.as_slice() {
			close(handle);
		}
		// THE ROOT MINTS CONNECTIONS HERE TOO. Same shape as the catalogue, and for the same reason:
		// `service_connect` is how every consumer in this tree reaches a service, and it sends the
		// reserved CONNECT opcode and WAITS. PermissionManager is the only consumer here, and it
		// asked before anything else could - so an endpoint that could not answer hung the boot.
		if len >= 2 {
			let op: u16 = u16::from_le_bytes([buf[0], buf[1]]);
			if op == HEARTBEAT_OP {
				send_blocking(service, b"PONG", 0);
				return true;
			}
			if op == CONNECT_OP && is_root {
				match channel_pair_for_catalogue(clients) {
					Some(theirs) => {
						send_blocking(service, &[], theirs);
					}
					None => {
						send_blocking(service, &[], 0);
					}
				}
				return true;
			}
		}
		let mut view = PolicyView { nodes, catalogue, config };
		let mut reply = [0u8; 1024];
		let mut request_handles = wire::Handles::new();
		let mut reply_handles = wire::Handles::new();
		let request: Vec<u8> = buf[..len].to_vec();
		if let Some(written) = proto::system::device_policy_admin::dispatch(&mut view, &request, &mut request_handles, &mut reply, &mut reply_handles) {
			send_blocking(service, &reply[..written], 0);
		}
		true
	}
}

// How many clients may hold a connection to the catalogue at once.
//
// A BOUND, because a server that mints a channel per request without one is a server a client can
// exhaust. `lsdev` and the System Graph are two; the rest is headroom.
const MAX_CATALOGUE_CLIENTS: usize = 8;

// The client connections minted from the catalogue's root, and the root itself at index 0.
struct CatalogueClients {
	channels: [u64; MAX_CATALOGUE_CLIENTS],
	count: usize,
}

impl CatalogueClients {
	const fn new() -> Self {
		Self { channels: [0; MAX_CATALOGUE_CLIENTS], count: 0 }
	}

	fn live(&self) -> &[u64] {
		&self.channels[..self.count]
	}

	// GIVE ONE BACK. The slot is capacity for a LIVE client, not a lifetime allocation.
	//
	// This structure was append-only, and a channel whose peer has closed is PERMANENTLY READABLE -
	// that is what `Channel::is_readable` answers for a gone peer, so `recv` reports the closure
	// rather than blocking. The standing loop puts every client in its one `wait_any`, so a consumer
	// that exited left a handle that woke the loop on every pass, forever: DeviceManager spun
	// answering nothing, and the teardown and catalogue handles behind it in the wait set were
	// starved by a dead one in front. The eight slots were also spent for the life of the boot, so
	// eight consumer restarts exhausted a bound meant to describe how many may hold a connection AT
	// ONCE.
	//
	// The last live entry moves into the gap: the array is a set, the standing loop rebuilds its
	// wait set from `live()` on every pass, and nothing indexes across a call to this.
	unsafe fn retire(&mut self, at: usize) {
		unsafe {
			if at >= self.count {
				return;
			}
			close(self.channels[at]);
			self.count -= 1;
			self.channels[at] = self.channels[self.count];
			self.channels[self.count] = 0;
		}
	}
}

// Answer one catalogue request on `channel`.
//
// THE ROOT MINTS CONNECTIONS AND THE CONNECTIONS CARRY REQUESTS, which is the shape every other
// multi-client server in this tree has. It was a single typed dispatch, and `service_connect` -
// which is how every consumer reaches a service here - sends the reserved CONNECT opcode and waits:
// so the first thing that ever asked for the catalogue hung the boot, because nothing answered it.
unsafe fn serve_catalogue_once(channel: u64, is_root: bool, clients: &mut CatalogueClients, catalogue: &mut Catalogue, nodes: &[Node], buf: &mut [u8]) -> bool {
	unsafe {
		let ReceivedCaps::Message { len, handles } = recv_caps_blocking(channel, buf) else { return false };
		for &handle in handles.as_slice() {
			close(handle);
		}
		if len >= 2 {
			let op: u16 = u16::from_le_bytes([buf[0], buf[1]]);
			if op == HEARTBEAT_OP {
				send_blocking(channel, b"PONG", 0);
				return true;
			}
			if op == CONNECT_OP && is_root {
				match channel_pair_for_catalogue(clients) {
					Some(theirs) => {
						send_blocking(channel, &[], theirs);
					}
					None => {
						send_blocking(channel, &[], 0);
					}
				}
				return true;
			}
		}
		let request: Vec<u8> = buf[..len].to_vec();
		let mut request_handles = wire::Handles::new();
		// A SUBSCRIPTION IS NOT A CALL, and the generated dispatcher says so by answering `None` for
		// it: a stream operation replies with an ENDPOINT and then writes frames on it. Nothing
		// called this path, so `subscribe` - the milestone's Goal - could not be reached through the
		// server at all, and the `None` fell through to a reply that was never sent.
		if len >= 2 && u16::from_le_bytes([buf[0], buf[1]]) == proto::system::provider_catalogue::OP_SUBSCRIBE {
			open_subscription(channel, catalogue, nodes, &request, &mut request_handles);
			return true;
		}
		let mut view = CatalogueView { catalogue, nodes };
		let mut reply = [0u8; 4096];
		let mut reply_handles = wire::Handles::new();
		if let Some(written) = proto::system::provider_catalogue::dispatch(&mut view, &request, &mut request_handles, &mut reply, &mut reply_handles) {
			// WITH WHATEVER THE REPLY CARRIES, and that is not a refinement (2026-08-31).
			//
			// `open` answers with a CONNECTION - a discriminant, the handle's index, and the handle
			// itself - and this sent the bytes with a zero transfer, so the index arrived and the
			// capability did not. The client decodes `take_handle` off a reply that has none and
			// answers `None`, which reads as "the catalogue did not answer" - and the operation this
			// milestone exists for could not work at all. Nothing had called it in production, so
			// nothing had found out: AudioService is the first consumer to `open`.
			send_caps_blocking(channel, &reply[..written], reply_handles.as_slice());
		}
		true
	}
}

// OPEN ONE SUBSCRIPTION: the snapshot and the live stream, in one operation.
//
// The reply on the service channel carries the correlation id and the CONSUMER end; every
// `provider-info` then travels as its own framed message on the producer end, which this program
// keeps. Closing the producer is what tells a consumer the stream has ended - so it is kept for the
// life of the subscription rather than closed after the snapshot, which is the difference between
// this and a one-shot `tail`.
unsafe fn open_subscription(service: u64, catalogue: &mut Catalogue, nodes: &[Node], request: &[u8], request_handles: &mut wire::Handles) {
	unsafe {
		let corr: u32 = {
			let mut view = CatalogueView { catalogue, nodes };
			let Some((corr, _)) = proto::system::provider_catalogue::subscribe_open(&mut view, request, request_handles) else { return };
			corr
		};
		// THE KIND, READ FROM THE REQUEST. `subscribe_open` hands back the snapshot rather than the
		// argument, and a live stream has to know which kind it is watching to know which frames are
		// its own - so the kind is decoded here, from the same bytes.
		let Some(kind) = subscribed_kind(request) else { return };
		let Some((producer, consumer)) = channel() else { return };
		send_blocking(service, &corr.to_le_bytes(), consumer);
		// EVERYTHING OF THAT KIND NOW, AND EVERYTHING AFTER IT, in one step - see `subscribe_stream`.
		if !catalogue.subscribe_stream(kind, producer) {
			return;
		}
	}
}

// The `provider-kind` a subscribe request names: opcode, correlation, then the kind - decoded by the
// generated reader rather than by this file knowing the encoding, which is the mistake that put four
// hand-parsed offsets in a supervisor once already.
fn subscribed_kind(request: &[u8]) -> Option<u16> {
	if request.len() < 2 + 4 {
		return None;
	}
	Some(provider_kind_wire(proto::system::ProviderKind::decode(&request[2 + 4..])?))
}

// Mint one client connection, keeping the server end. None once the bound is reached, which the
// caller answers with a zero handle rather than by growing.
unsafe fn channel_pair_for_catalogue(clients: &mut CatalogueClients) -> Option<u64> {
	unsafe {
		if clients.count >= MAX_CATALOGUE_CLIENTS {
			print(b"DeviceManager: the provider catalogue has as many clients as it will hold; refusing another\n");
			return None;
		}
		let (mine, theirs) = channel()?;
		clients.channels[clients.count] = mine;
		clients.count += 1;
		Some(theirs)
	}
}

// What the claim's state says a bind may do right now.
enum ClaimReadiness {
	// Observed `Free`. Bind.
	Bindable,
	// `Releasing`, and its deadline has not passed. Come back; do NOT acquire.
	WaitAndSeeAgain,
	// Nothing further will happen on this node.
	Terminal(FailureCause),
}

// Read the claim before binding, and answer what may be done about it.
//
// | state read | what this answers |
// | `Free` | bind normally |
// | `Releasing` | come back, bounded by the claim's OWN deadline - which the kernel minted at the release, because the manager that would have supplied one may be the one that died |
// | `Releasing` still, at that deadline | do NOT acquire: `Quarantined` with `teardown-unconfirmed`, which is the conservative answer and the true one - nothing observed the device go quiet |
// | `Quarantined` | adopt it; it is already terminal |
// | `Claimed` | an invariant violation, reported as one - a correct `domain_kill` cannot leave it here, and a manager that quietly rebound over it would be handing out a device somebody still holds |
unsafe fn observe_claim(node: &mut Node, device_privilege: u64, driver_name: &[u8]) -> ClaimReadiness {
	unsafe {
		// A snapshot this manager cannot take is not a licence to assume the device is free.
		let Some(snapshot) = device_claim_snapshot(node.index, device_privilege) else {
			return ClaimReadiness::Bindable;
		};
		// WHAT THE CLAIM ALREADY HOLDS, ADOPTED. `granted_resources` counts the RESOURCE frames this
		// manager sent during the CURRENT bind, so a node reconstructed by a NEW manager - which is
		// the case M6 is about - started at zero and reported a binding charged with nothing while
		// the kernel held its MMIO window, its vectors and its IOMMU grants. The kernel counts them
		// from its own records; this is the manager taking the count it did not make.
		let held = snapshot.mmio_windows + snapshot.irq_vectors + snapshot.iommu_grants;
		if held > node.granted_resources {
			node.granted_resources = held;
		}
		// AND A QUARANTINED GRANT IS SAID OUT LOUD, because it is the one holding a reconstructed
		// node cannot act on. `iommu_grants` counts live and quarantined mappings together - a
		// quarantined one is charged exactly like a live one - so a manager adopting a binding could
		// see a charge and not that part of it is out of circulation for the life of the boot.
		if snapshot.iommu_quarantined > 0 {
			print(b"DeviceManager: ");
			print_driver_name(driver_name);
			print(b" holds an IOMMU mapping the device never confirmed it stopped resolving; that address space stays out of circulation for this boot\n");
		}
		match snapshot.state {
			CLAIM_STATE_FREE => ClaimReadiness::Bindable,
			CLAIM_STATE_RELEASING => {
				// The kernel latches the deadline itself, inside the same read - so a `Releasing`
				// that comes back here still has time left, and one that ran out came back
				// `Quarantined` instead. There is no arithmetic to repeat and no second authority.
				print(b"DeviceManager: ");
				print_driver_name(driver_name);
				print(b"'s device is still being torn down by whoever held it last; not acquiring it yet\n");
				ClaimReadiness::WaitAndSeeAgain
			}
			CLAIM_STATE_QUARANTINED => {
				print(b"DeviceManager: ");
				print_driver_name(driver_name);
				print(b"'s device is quarantined - nothing observed it go quiet, and it is not claimed again this boot\n");
				// ADOPTED, AND THE MOVE IS CHECKED. This was `move_to(..)` with the result discarded,
				// and the edge did not exist - so the refusal was silent and `give_up` then recorded
				// `Failed` for an attempt that had taken no claim. `Binding -> Quarantined` is now in
				// the table, and a refusal here would be a bug worth seeing rather than a state
				// quietly replaced by a worse description of it.
				if !node.record.move_to(BindingState::Quarantined, Some(FailureCause::TeardownUnconfirmed)) {
					print(b"DeviceManager: the binding record refused to adopt the device's quarantine\n");
				}
				ClaimReadiness::Terminal(FailureCause::TeardownUnconfirmed)
			}
			// A CORRECT `domain_kill` CANNOT LEAVE IT HERE. Reported as the invariant violation it
			// is rather than bound over: a manager that quietly rebound would be handing out a
			// device somebody still holds.
			_ => {
				print(b"DeviceManager: ");
				print_driver_name(driver_name);
				print(b"'s device reads as CLAIMED and this manager holds no claim on it - that is an invariant violation, not a device to rebind over\n");
				ClaimReadiness::Terminal(FailureCause::ClaimRefused)
			}
		}
	}
}

// STOP EVERY BOUND DRIVER, DEPENDENTS FIRST.
//
// A CONTROLLER BEING STOPPED STOPS ITS CHILDREN FIRST, which here means: a node that REQUIRES a
// provider kind goes down before whatever publishes that kind. Stopping the provider first would
// leave its consumer holding a channel whose server has gone, mid-request, and then ask it politely
// to finish - which is the forced path with a courteous name.
//
// NOT BUS REMOVAL. The scan runs once and no removal event exists, so the only ways a controller
// goes away are a shutdown and a teardown, and both are asked for from inside.
// HOW FAR EACH NODE STANDS FROM A LEAF PROVIDER, following the kinds it requires to the nodes that
// publish them.
//
// REVERSE DEPENDENCY ORDER IS DEPTH AND NOT A COUNT. Ordering by how many direct requirements a node
// declares holds only for chains one deep: in A requires a kind B provides, and B requires a kind C
// provides, A and B BOTH declare one requirement, their tie falls to enumeration order, and B can be
// stopped before its own dependent A. The manifest validator accepts such chains - it refuses cycles
// and orphans - so this is an ordinary supported registry, not an exotic one.
//
// A cycle cannot appear (refused at registry-build time), so the relaxation terminates; `nodes.len()`
// passes is the worst case for a chain that happens to be enumerated backwards, and every pass is
// over a handful of entries.
//
// THE ENTRY EACH NODE IS RUNNING, not the cursor - see `Node::entry`. Reading the cursor builds the
// graph out of whichever candidate an operator had SELECTED for the next bind, so a `select` on a
// device that stays online could order a teardown by a driver that is not running.
//
// SHARED, because two callers order by this rule and the milestone states it once: the shutdown, and
// the dependency-lost stop that has to take dependents down before what they depend on.
fn dependency_depths(nodes: &[Node]) -> Vec<usize> {
	let mut depth: Vec<usize> = alloc::vec![0; nodes.len()];
	for _ in 0..nodes.len() {
		let mut moved = false;
		for at in 0..nodes.len() {
			let Some(entry) = nodes[at].entry() else { continue };
			let mut want = 0usize;
			for kind in entry.requires {
				// Whoever publishes that kind: this node stands one level above the deepest of them.
				// A requirement nothing in this image provides cannot occur - the manifest validator
				// refuses it - and if it somehow did, it contributes nothing, which leaves the node a
				// leaf rather than inventing a level for it.
				for other in 0..nodes.len() {
					if other == at {
						continue;
					}
					// The same rule on the PROVIDER side: who publishes a kind is a fact about the
					// driver that is running, not about the one selected next.
					let Some(provider) = nodes[other].entry() else { continue };
					if provider.provides.iter().any(|(declared, _, _)| declared == kind) {
						want = want.max(depth[other] + 1);
					}
				}
			}
			if want != depth[at] {
				depth[at] = want;
				moved = true;
			}
		}
		if !moved {
			break;
		}
	}
	depth
}

unsafe fn stop_all(nodes: &mut [Node], catalogue: &mut Catalogue, intent: driver_binding::StopIntent, buf: &mut [u8]) {
	unsafe {
		// EVERY SUBSCRIPTION IS CLOSED FIRST. A consumer learns a stream has ended by its producer
		// closing; one left open through a shutdown is a consumer waiting on frames that will never
		// come, and it would outlive the program that promised them.
		catalogue.close_subscriptions();
		// REVERSE DEPENDENCY ORDER, WHICH IS DEPTH AND NOT A COUNT.
		//
		// This sorted by how many direct requirements a node declares, on the reasoning that "a node
		// requiring something is a dependent and goes first". That holds only for chains one deep. In
		// A requires a kind B provides, and B requires a kind C provides, A and B BOTH declare one
		// requirement: their tie falls to enumeration order and B can be stopped before its own
		// dependent A. The manifest validator accepts such chains - it refuses cycles and orphans -
		// so this is an ordinary supported registry, not an exotic one.
		//
		// Depth is what the order needs: how far a node is from a leaf provider, following the kinds
		// it requires to the nodes that publish them. A cycle cannot appear (refused at registry-build
		// time), so the relaxation below terminates; `nodes.len()` passes is the worst case for a
		// chain that happens to be enumerated backwards, and every pass is over a handful of entries.
		let depth: Vec<usize> = dependency_depths(nodes);
		let mut order: Vec<usize> = (0..nodes.len()).collect();
		// Deepest first: a dependent is stopped before what it depends on. The index breaks ties so
		// two unrelated nodes at one level stop in a stable order rather than in whatever order the
		// sort happened to leave them.
		order.sort_by_key(|&at| (core::cmp::Reverse(depth[at]), at));
		for at in order {
			if nodes[at].record.state != BindingState::Online {
				continue;
			}
			let Some(binding) = &nodes[at].binding else { continue };
			let (channel, generation): (u64, u64) = (binding.channel, nodes[at].id.generation);
			nodes[at].stop_intent = intent;
			// THE PROVIDER IS WITHDRAWN AND NEW CONNECTIONS REFUSED FIRST, so nothing arrives
			// during the drain. A driver asked to finish while work keeps being handed to it is a
			// driver that cannot finish.
			catalogue.withdraw_binding(nodes[at].id);
			// AND THE NODE ENTERS `Stopping`, WHICH IS WHAT MAKES THE ANSWER ADMISSIBLE (corrected
			// 2026-08-31).
			//
			// This recorded the intent, withdrew the provider and sent `STOP` - and left the record
			// `Online`. `drain_channel` admits a `STOPPED` frame only from a node that is already
			// `Stopping`, deliberately: a planned stop is a state the manager put the node INTO, and
			// a driver announcing one nobody asked for is describing a conversation that did not
			// happen. So every driver that answered this shutdown correctly had its answer REFUSED
			// as unsolicited, the node stayed `Online` for the whole slice, and the loop below then
			// injected `Wedged` and reported a FORCED teardown. The clean path could not be taken by
			// any driver, however well behaved - the one outcome M3 exists to produce was
			// unreachable, and the milestone's own "never claims the work was flushed" line was being
			// printed on every shutdown.
			//
			// `begin_operator_stop` had this right and this did not; the shape is now the same in
			// both, and `Online -> Stopping` is a legal edge of the record's own table.
			if !nodes[at].record.move_to(BindingState::Stopping, None) {
				print(b"DeviceManager: a node could not enter the teardown for a planned stop; its exit is what will end the binding\n");
			}
			let name: &[u8] = nodes[at].driver_name();
			if !send_frame(channel, driver_protocol::Opcode::Stop, generation, &[], 0, 0) {
				// The channel is already gone: there is nothing to ask and nothing to wait for.
				let _ = advance(&mut nodes[at], name, catalogue);
				continue;
			}
			// A DEADLINE THAT EXPIRES FORCES THE REVOCATION AND SAYS IT WAS FORCED - never that a
			// clean flush happened. The slice is the teardown reserve this node's incident already
			// carries, so a stop is bounded by the same budget everything else on this node is.
			let deadline: u64 = clock().saturating_add(nodes[at].incident.teardown_reserve.max(driver_protocol::MAX_HEARTBEAT_DEADLINE as u64));
			// WHAT THE WAIT IS FOR IS "THE DRIVER REACTED", AND THAT IS NOT THE SAME AS "THE
			// TEARDOWN FINISHED" (2026-08-31, and this half was got wrong first).
			//
			// The original condition was `state != Online`, which worked because the node was left
			// `Online`: any reaction moved it out and ended the wait. Putting the node into
			// `Stopping` above breaks that reading, and the obvious replacement - wait while it is
			// `Stopping` - is a DIFFERENT wait: a node stays `Stopping` while its teardown runs, so
			// that version waited for the teardown to complete and reported a FORCED stop against
			// every driver whose teardown outlived the slice. Measured: it turned an ordinary
			// shutdown into `did not answer the stop inside its slice` for a driver that had
			// answered.
			//
			// The reaction is the binding ENDING. A driver that answers `STOPPED` and one that exits
			// both give their binding up, and `advance` takes it in both cases; a driver that is
			// still there and silent keeps it. So the wait ends when the node leaves `Stopping` OR
			// when its binding is gone, and the forced branch below fires only for the one case that
			// is actually a failure to answer - still `Stopping`, still holding its binding.
			while clock() < deadline {
				drain_channel(&mut nodes[at], buf);
				if nodes[at].queue.is_empty() {
					wait(channel, deadline);
					continue;
				}
				let _ = advance(&mut nodes[at], name, catalogue);
				if nodes[at].record.state != BindingState::Stopping || nodes[at].binding.is_none() {
					break;
				}
			}
			if nodes[at].record.state == BindingState::Stopping && nodes[at].binding.is_some() {
				print(b"DeviceManager: ");
				print(name);
				print(b" did not answer the stop inside its slice; the teardown is FORCED and nothing here says its work was flushed\n");
				nodes[at].push(BindingEvent::Wedged { generation });
				let _ = advance(&mut nodes[at], name, catalogue);
			}
		}
	}
}

// PING WHAT IS DUE, AND DECLARE WEDGED WHAT DID NOT ANSWER.
//
// Called from the stand loop's every pass, whether it woke on a channel or on the deadline: an
// expiry that only fires when nothing else happens is a watchdog a busy machine switches off.
//
// Answers the soonest tick any supervised node needs the wait back at, or 0 for "nothing to wake
// for" - which the wait reads as no timeout, correctly, because there is then nothing to time out.
unsafe fn tick_heartbeats(nodes: &mut [Node], buf: &mut [u8]) -> u64 {
	unsafe {
		let now: u64 = clock();
		let mut soonest: u64 = 0;
		for node in nodes.iter_mut() {
			if !node.beat.supervised() || node.record.state != BindingState::Online {
				continue;
			}
			// THE ANSWER IS READ WHERE THE QUESTION IS ASKED.
			//
			// A node that is already `Online` is not in the bring-up wait set - that set is for
			// bindings in flight - so nothing was reading its channel, and its `PONG` sat there
			// unread while the manager decided it had not answered. Every driver in the machine was
			// declared wedged at sequence 1, which is the shape of a supervisor listening on the
			// wrong end rather than of a driver that stopped.
			drain_channel(node, buf);
			let Some(binding) = &node.binding else { continue };
			let (channel, generation): (u64, u64) = (binding.channel, node.id.generation);
			match node.beat.tick(now) {
				driver_binding::Beat::Idle => {}
				// NOT ANSWERED INSIDE THE DEADLINE ITS ENTRY DECLARED. Queued as an event so it
				// runs through the same state machine as a crash - the teardown is the same
				// transaction and the same counter, and only the reason differs.
				driver_binding::Beat::Wedged => {
					node.push(BindingEvent::Wedged { generation });
				}
				driver_binding::Beat::Ask(sequence) => {
					let mut payload = [0u8; driver_protocol::SEQUENCE_PAYLOAD_LEN];
					driver_protocol::encode_sequence(sequence, &mut payload);
					if send_frame(channel, driver_protocol::Opcode::Ping, generation, &payload, 0, 0) {
						node.beat.asked(now);
					} else {
						// The channel is gone, which is a driver that ended rather than one that is
						// slow. The exit event will arrive on its own; this only stops asking.
						node.beat.unsendable(now);
					}
				}
			}
			let wake = node.beat.wake_at();
			if wake != 0 && (soonest == 0 || wake < soonest) {
				soonest = wake;
			}
		}
		soonest
	}
}

// WHAT IS PUBLISHED, COUNTED PER KIND.
//
// The four named locals could report up to four block providers and had no way to say there were
// five. This counts what the catalogue holds, so a machine with more disks than the old code had
// variables says so instead of quietly binding the ones that fit.
unsafe fn report_catalogue(catalogue: &Catalogue, phase: &[u8]) {
	unsafe {
		let mut line = [0u8; 96];
		let mut at: usize = 0;
		for (label, kind) in [
			(b"block".as_slice(), driver_protocol::provider::BLOCK),
			(b"net", driver_protocol::provider::NET),
			(b"display", driver_protocol::provider::DISPLAY),
			(b"audio", driver_protocol::provider::AUDIO),
			(b"input", driver_protocol::provider::INPUT),
			(b"usb-bus", driver_protocol::provider::USB_BUS),
		] {
			let count = catalogue.count_of(kind);
			if count == 0 || at + label.len() + 4 >= line.len() {
				continue;
			}
			if at > 0 {
				line[at] = b',';
				line[at + 1] = b' ';
				at += 2;
			}
			line[at] = b'0' + (count.min(9) as u8);
			line[at + 1] = b' ';
			at += 2;
			line[at..at + label.len()].copy_from_slice(label);
			at += label.len();
		}
		if at == 0 {
			return;
		}
		// WHICH BRING-UP THIS IS. Both phases report, and the line was the same string for both -
		// so a machine whose second phase publishes nothing new printed one fact twice with nothing
		// saying they were different moments.
		print(b"DeviceManager: providers published ");
		print(phase);
		print(b" - ");
		print(&line[..at]);
		print(b"\n");
	}
}

// Print a one-line summary of how many devices are online (their driver bound and
// reported in) out of those with a driver to bind - the device-state DeviceManager
// tracks. Devices with no userspace driver yet stay unknown and are not counted.
unsafe fn report_state(state: &[u8]) {
	unsafe {
		let mut online: u32 = 0;
		let mut tracked: u32 = 0;
		let mut unbound: u32 = 0;
		let mut missing: u32 = 0;
		for &s in state {
			match s {
				STATE_UNKNOWN => {}
				STATE_UNBOUND => unbound += 1,
				STATE_DRIVER_MISSING => {
					missing += 1;
					tracked += 1;
				}
				STATE_ONLINE => {
					online += 1;
					tracked += 1;
				}
				_ => tracked += 1,
			}
		}
		print(b"DeviceManager: ");
		print_count(online);
		print(b" of ");
		print_count(tracked);
		print(b" device(s) online");
		// SAID, not counted into the same number. An unsupported device is not a failure and a
		// missing artifact is not an unsupported device; folding either into "online out of
		// tracked" is what made both invisible.
		if unbound > 0 {
			print(b", ");
			print_count(unbound);
			print(b" unbound");
		}
		if missing > 0 {
			print(b", ");
			print_count(missing);
			print(b" driver-missing");
		}
		print(b"\n");
	}
}

// Print a small non-negative count in decimal (one or two digits suffice for the
// handful of devices QEMU exposes).
unsafe fn print_count(n: u32) {
	unsafe {
		if n >= 10 {
			print(&[b'0' + (n / 10) as u8]);
		}
		print(&[b'0' + (n % 10) as u8]);
	}
}

// THE DRIVER REGISTRY, generated from the manifest. See `build.rs`.
//
// It replaces `driver_for(&DeviceInfo)`, which was seven `match` arms of virtio type numbers plus
// one hardcoded PCI address for the development console - driver selection written in Rust rather
// than declared, so adding a driver meant editing this file, and nothing could check that the
// image's drivers and the code's list agreed. `system-manifest` refuses a duplicate identity, an
// ambiguous equal-priority match, an empty rule set, a rule naming something discovery cannot
// answer, and a boot-critical driver staged on the volume it exists to mount, all before this table
// is emitted.

// ONE RULE IS A CONJUNCTION: every predicate that is present must hold, and `None` means "do not
// ask" rather than "must be absent". A driver's rule list is the disjunction.
//
// Both halves are needed and the tree proved it: the development console is selected by its virtio
// type AND its pinned address, which a set of single-question rules could only OR together - and
// OR-ing them would bind any device at that address to the console driver.
#[derive(Clone, Copy)]
struct Address {
	bus: u8,
	dev: u8,
	func: u8,
}

#[derive(Clone, Copy)]
struct Rule {
	// THE TRANSPORT AND THE VIRTIO TYPE, which cite one specification between them. It was a single
	// `device_type`, and that conflated the virtio specification's own numbers with a LiberSystem
	// constant invented for the xHCI row - so a rule could not say "a virtio-pci function whose
	// virtio type is 1" and had to say "device type 1" instead.
	transport: Option<u8>,
	virtio_type: Option<u32>,
	// The standards identity, which discovery now carries. It was declarable and never satisfied
	// while `DeviceInfo` had no class byte; the kernel had resolved all three for every function
	// since the first PCI scan and kept them for `lspci` alone.
	pci_class: Option<u8>,
	pci_subclass: Option<u8>,
	pci_interface: Option<u8>,
	// The part. Never a rule on its own - `system-manifest` refuses one - only a narrowing on a rule
	// that already names what the device IS.
	pci_vendor: Option<u16>,
	pci_product: Option<u16>,
	pci_address: Option<Address>,
}

impl Rule {
	// THE DECISION ITSELF IS `driver_binding::Match`, which is host-tested. This is the conversion
	// from the generated registry's shape to it and nothing more: a second copy of the predicate
	// here would be a second thing to keep in step, and the one that is not tested is the one that
	// drifts. See the note beside `Match` for why the conjunction could not be checked before.
	fn matches(self, info: &DeviceInfo) -> bool {
		self.as_match().matches(&driver_binding::Discovered { transport: info.transport, virtio_type: info.device_type, class: info.class, subclass: info.subclass, prog_if: info.prog_if, vendor: info.vendor, product: info.product, bus: info.bus, dev: info.dev, func: info.func })
	}

	fn as_match(self) -> driver_binding::Match {
		driver_binding::Match { transport: self.transport, virtio_type: self.virtio_type, class: self.pci_class, subclass: self.pci_subclass, prog_if: self.pci_interface, vendor: self.pci_vendor, product: self.pci_product, address: self.pci_address.map(|address| (address.bus, address.dev, address.func)) }
	}
}

struct Entry {
	name: &'static [u8],
	// The staged file, which is what the loader asks for - not derived from the name, because the
	// two differ for a pinned driver and deriving it is how they drift.
	artifact: &'static [u8],
	// WHETHER THE VOLUME CANNOT BE MOUNTED WITHOUT IT, which is the only thing selection asks of
	// the manifest's four lifecycle classes: a boot-critical driver is staged in `init.pkg` and
	// bound in phase one, and everything else waits for a volume.
	boot_critical: bool,
	// How specific the match is, as a rank: generic 0, exact 1, quirk 2. Only compared, never
	// named - the names are the manifest's and `system-manifest` is what checks them.
	priority: u8,
	// The provider kinds this entry needs published before it can bind. A driver launched without
	// them is not one that failed - it is one that was tried too early.
	requires: &'static [u16],
	// Which kinds it may publish and at most how many. A BOUND, so a driver cannot publish a kind
	// it never declared: a compromised one advertising itself as a disk is what this closes.
	// `system-manifest` refuses a requirement nothing produces and a cycle, at registry-build time.
	// Kind, at most how many of them, and HOW MANY CONSUMERS ONE ADMITS. The third is what a
	// per-consumer connection needs: a kind that admits one says so here, and the second `open` is
	// refused rather than handed an endpoint nobody serves.
	provides: &'static [(u16, u16, u16)],
	// How long this driver may take to answer a `PING`, in ticks. `None` is "not heartbeat-
	// supervised", which is the honest state for a driver that stands on its channel and does
	// nothing else - and the registry refuses 0, because `wait_any` reads that as no timeout at all.
	heartbeat_deadline: Option<u32>,
	rules: &'static [Rule],
}

include!(concat!(env!("OUT_DIR"), "/driver_registry.rs"));

// The registry entry that binds `info`, or None when nothing in the image does.
//
// EVERY candidate is collected and the most specific wins, rather than the first that matches -
// "never choose by enumeration order" is the milestone's rule and a first-match loop is exactly
// that. A tie cannot happen: `system-manifest` refuses two entries whose rules overlap at one
// priority, which is decidable because the rule set is closed. The assertion here is what keeps
// that proof honest if the check is ever weakened.
fn registry_entry(info: &DeviceInfo) -> Option<&'static Entry> {
	registry_candidates(info).first().copied()
}

// Every entry that matches `info`, most specific first.
//
// The ORDER is the arbitration and the LIST is the fallback. A bind that fails may try the next
// compatible candidate - but only after the first attempt's resources are gone, which the caller
// enforces by unmapping and closing before it comes back here. Trying them in parallel, or trying
// the next while the first still holds the device, is how two drivers come to own one controller.
//
// Nothing is launched to find out whether it fits: the choice is made from metadata alone, which is
// the item's "probe metadata before process creation rather than launching every possible driver to
// see which one succeeds".
//
// A tie is refused rather than broken. `system-manifest` proves at registry-build time that two
// entries cannot overlap at one priority, so this cannot fire - and it stays here because a proof
// that nothing checks is a comment.
fn registry_candidates(info: &DeviceInfo) -> Vec<&'static Entry> {
	let mut candidates: Vec<&'static Entry> = DRIVER_REGISTRY.iter().filter(|entry| entry.rules.iter().any(|rule| rule.matches(info))).collect();
	candidates.sort_by(|left, right| right.priority.cmp(&left.priority));
	for pair in candidates.windows(2) {
		if pair[0].priority == pair[1].priority {
			unsafe { print(b"DeviceManager: two registry entries match one device at the same priority; leaving it unbound\n") };
			return Vec::new();
		}
	}
	candidates
}
