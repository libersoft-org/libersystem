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
		let mut net_client: u64 = 0;
		let mut gpu_client: u64 = 0;
		let mut snd_client: u64 = 0;
		let mut input_client: u64 = 0;
		let mut usb_client: u64 = 0;
		let mut usbq_client: u64 = 0;
		let mut usb_pointer: u64 = 0;
		let mut raw_keys: u64 = 0;
		// What this program holds on behalf of the development agent: its bootstrap, so the
		// launcher can be handed to it once PermissionManager exists - which is after this
		// program has finished starting drivers - and so its death is noticed and answered.
		#[cfg(feature = "development")]
		let mut dev: DevAgent = DevAgent::default();
		launch_boot_drivers(&package, &mut catalogue, &mut nodes, power, console_input, device_privilege, &mut buf, &mut boot_blocks);

		// 3. report in once the disks are bound, transferring the block service channel up
		//    the boot chain, then the second/third/fourth block disks' service channels (the
		//    report itself carries one handle; each `BLOCK2`/`BLOCK3`/`BLOCK4` handle is 0
		//    when that disk is absent). The net / gpu / snd / input driver channels follow in
		//    phase 2, once the volume they load from is mounted.
		send_blocking(bootstrap, b"DeviceManager: online", boot_blocks[0]);
		for (at, tag) in BOOT_BLOCK_TAGS.iter().enumerate().skip(1) {
			send_blocking(bootstrap, tag, boot_blocks[at]);
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
			let soonest: u64 = tick_heartbeats(&mut nodes, &mut buf);
			for at in 0..nodes.len() {
				let name: &[u8] = nodes[at].driver_name();
				let _ = advance(&mut nodes[at], name, &mut catalogue);
			}
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
			let mut waiting: [u64; MAX_CATALOGUE_CLIENTS * 2 + 3] = [0; MAX_CATALOGUE_CLIENTS * 2 + 3];
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
			for &client in catalogue_clients.live() {
				waiting[waiting_count] = client;
				waiting_count += 1;
			}
			let ready: i64 = wait_any(&waiting[..waiting_count], soonest);
			if ready != 0 {
				if ready > 0 {
					let at: usize = ready as usize;
					if policy_service != 0 && (at == policy_at || (at >= policy_clients_at && at < policy_clients_at + policy_clients.live().len())) {
						serve_policy_once(waiting[at], at == policy_at, &mut policy_clients, &mut nodes, policy_config, &mut buf);
					} else {
						let is_root: bool = catalogue_service != 0 && at == 1;
						serve_catalogue_once(waiting[at], is_root, &mut catalogue_clients, &catalogue, &nodes, &mut buf);
					}
				}
				continue;
			}
			match recv_blocking(bootstrap, &mut buf) {
				Received::Message { len, handle } if len >= 7 && &buf[..7] == b"DRIVERS" => {
					#[cfg(feature = "development")]
					launch_volume_drivers(handle, &mut catalogue, &mut nodes, power, console_input, device_privilege, &mut buf, &mut net_client, &mut gpu_client, &mut snd_client, &mut input_client, &mut usb_client, &mut usbq_client, &mut usb_pointer, &mut raw_keys, &mut dev);
					#[cfg(not(feature = "development"))]
					launch_volume_drivers(handle, &mut catalogue, &mut nodes, power, console_input, device_privilege, &mut buf, &mut net_client, &mut gpu_client, &mut snd_client, &mut input_client, &mut usb_client, &mut usbq_client, &mut usb_pointer, &mut raw_keys);
					if handle != 0 {
						close(handle);
					}
					send_blocking(bootstrap, b"NET", net_client);
					send_blocking(bootstrap, b"GPU", gpu_client);
					send_blocking(bootstrap, b"SND", snd_client);
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
unsafe fn launch_boot_drivers(package: &Package, catalogue: &mut Catalogue, nodes: &mut Vec<Node>, power: u64, console_input: u64, device_privilege: u64, buf: &mut [u8], boot_blocks: &mut [u64; BOOT_BLOCK_TAGS.len()]) {
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
			if gate_on_requirements(&mut node, entry, catalogue) && begin_bind(&mut node, &info, elf, entry.name, 0, power, console_input, device_privilege) {
				nodes.push(node);
			}
			i += 1;
		}
		// THE CENTRAL WAIT, and every disk answers into it. A driver that never reports in costs
		// its own share of the boot window and nothing else's.
		while pump(nodes, first_node, catalogue, buf) {
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
						let entry = nodes[at].candidates[nodes[at].candidate];
						let Some(elf) = package.lookup(entry.artifact) else { continue };
						let info = nodes[at].info;
						begin_bind(&mut nodes[at], &info, elf, entry.name, 0, power, console_input, device_privilege);
					}
					Step::NextCandidate | Step::Done => {}
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
unsafe fn launch_volume_drivers(storage: u64, catalogue: &mut Catalogue, nodes: &mut Vec<Node>, power: u64, console_input: u64, device_privilege: u64, buf: &mut [u8], net_client: &mut u64, gpu_client: &mut u64, snd_client: &mut u64, input_client: &mut u64, usb_client: &mut u64, usbq_client: &mut u64, usb_pointer: &mut u64, raw_keys: &mut u64, #[cfg(feature = "development")] dev: &mut DevAgent) {
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
			for at in first_node..nodes.len() {
				let name: &[u8] = nodes[at].driver_name();
				match advance(&mut nodes[at], name, catalogue) {
					Step::Waiting => {}
					Step::Online => {
						state[nodes[at].index as usize] = STATE_ONLINE;
						#[cfg(feature = "development")]
						route_offers(&mut nodes[at], catalogue, name, storage, console_input, net_client, gpu_client, snd_client, input_client, usb_client, usbq_client, usb_pointer, dev);
						#[cfg(not(feature = "development"))]
						route_offers(&mut nodes[at], catalogue, name, net_client, gpu_client, snd_client, input_client, usb_client, usbq_client, usb_pointer);
					}
					// The same candidate, once more. The window and the attempt budget have already
					// said there is room for it.
					Step::Again => {
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
						nodes[at].candidate += 1;
						nodes[at].attempt = 0;
						if nodes[at].candidate < nodes[at].candidates.len() {
							start_candidate(&mut nodes[at], storage, key_producer, power, console_input, device_privilege, catalogue, &mut state);
						}
					}
					Step::Done => {}
				}
			}
		}
		close(key_producer);
		report_state(&state);
		report_catalogue(catalogue, b"after every device");
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
		loop {
			if node.candidate >= node.candidates.len() {
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
				print(b"DeviceManager: ");
				print(driver_name);
				print(b" is named by the registry and not on the volume; trying the next candidate\n");
				node.candidate += 1;
				node.attempt = 0;
				continue;
			};
			let elf: &[u8] = core::slice::from_raw_parts(mapped as *const u8, size);
			let info = node.info;
			let opened: bool = begin_bind(node, &info, elf, driver_name, key_producer, power, console_input, device_privilege);
			unmap_object(file);
			close(file);
			if opened {
				return;
			}
			// It could not even be opened - the claim refused, the spawn refused. `begin_bind` has
			// already put the node where the table says that belongs, so the only question left is
			// whether there is another candidate to try.
			node.candidate += 1;
			node.attempt = 0;
		}
	}
}

// ROUTED BY WHAT THE PROVIDER IS, not by which driver sent it and in what order.
//
// Every one of these used to be "the handle that came with the report", with the xHCI driver's
// extra two told apart by the literal bytes `USBBUS` and `POINTER` in the messages that followed -
// so what a capability was for was decided by parsing a string the driver chose.
#[allow(clippy::too_many_arguments)]
unsafe fn route_offers(node: &mut Node, catalogue: &mut Catalogue, driver_name: &[u8], #[cfg(feature = "development")] storage: u64, #[cfg(feature = "development")] console_input: u64, net_client: &mut u64, gpu_client: &mut u64, snd_client: &mut u64, input_client: &mut u64, usb_client: &mut u64, usbq_client: &mut u64, usb_pointer: &mut u64, #[cfg(feature = "development")] dev: &mut DevAgent) {
	unsafe {
		let _ = driver_name;
		// PUBLISHED FIRST, ROUTED SECOND. Everything this binding offered enters the catalogue with
		// an identity this service minted; what follows takes from the catalogue rather than from
		// the driver's own message, so a provider that nothing routes is still a provider the
		// machine has - and is withdrawn with its binding rather than leaked.
		if *net_client == 0 {
			*net_client = catalogue.take(driver_protocol::provider::NET);
		}
		if *gpu_client == 0 {
			*gpu_client = catalogue.take(driver_protocol::provider::DISPLAY);
		}
		if *snd_client == 0 {
			*snd_client = catalogue.take(driver_protocol::provider::AUDIO);
		}
		// The development channel driver hands up a raw byte channel, and the agent that speaks the
		// protocol over it is started here rather than by ServiceManager. It exists exactly when the
		// device does, it has no other client, and its whole reason to be a separate process is to
		// keep the artifact registry out of the address space that holds a device capability - so it
		// is started where that device is bound, and nowhere else.
		#[cfg(feature = "development")]
		{
			let dev_bytes: u64 = catalogue.take(driver_protocol::provider::CONSOLE_BYTES);
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
			*input_client = catalogue.take(driver_protocol::provider::INPUT);
		}
		// The xHCI driver offers up to three: the USB stick's block service (absent when no
		// mass-storage device is attached), its bus query channel for the `lsusb` inventory, and a
		// pointer-event channel for a USB pointing device. All three arrived in ONE handshake and
		// were held unpublished until its `READY`, so a controller that died between them published
		// nothing.
		if *usb_client == 0 {
			*usb_client = catalogue.take(driver_protocol::provider::BLOCK);
		}
		if *usbq_client == 0 {
			*usbq_client = catalogue.take(driver_protocol::provider::USB_BUS);
		}
		if *usb_pointer == 0 {
			*usb_pointer = catalogue.take(driver_protocol::provider::POINTER);
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
	fn into_attempt(self) -> Attempt {
		Attempt { domain: self.domain, process: self.process, channel: self.channel, claim: self.claim, key: self.key }
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
	incident_report: Option<Diagnostic>,
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
#[derive(Clone, Copy, Default)]
struct Heartbeat {
	// The entry's declared deadline in ticks, or 0 for a driver that is not supervised this way.
	deadline: u32,
	// The sequence the outstanding `PING` was sent with, and whether one is outstanding.
	sequence: u32,
	awaiting: bool,
	// When the next `PING` is due, and when an outstanding one stops being answerable.
	due: u64,
	expires: u64,
}

impl Heartbeat {
	// Arm from the entry's declared deadline. The cadence is `heartbeat_period` and not a second
	// number: a driver always gets one whole period to answer inside the deadline it declared.
	unsafe fn arm(&mut self, deadline: Option<u32>) {
		unsafe {
			let Some(deadline) = deadline else {
				*self = Heartbeat::default();
				return;
			};
			*self = Heartbeat { deadline, sequence: 0, awaiting: false, due: clock().saturating_add(driver_protocol::heartbeat_period(deadline) as u64), expires: 0 };
		}
	}

	fn supervised(&self) -> bool {
		self.deadline != 0
	}

	// The soonest tick this node needs the wait to come back at, or 0 for "nothing to wake for".
	fn wake_at(&self) -> u64 {
		if !self.supervised() {
			return 0;
		}
		if self.awaiting { self.expires } else { self.due }
	}
}

impl Node {
	fn new(index: u64, info: &DeviceInfo, candidates: Vec<&'static Entry>) -> Node {
		Node { id: BindingId::new(info.bus, info.dev, info.func, 0), index, info: *info, record: BindingRecord::new(), binding: None, offers: Offers::new(), incident: Incident { deadline: 0, teardown_reserve: 0 }, attempt: 0, candidates, candidate: 0, queue: BindingQueue::new(), beat: Heartbeat::default(), matched_rule: 0, granted_resources: 0, stop_intent: driver_binding::StopIntent::default(), last_opcode: 0, last_frame_at: 0, incident_report: None }
	}

	// Queue one event for this node.
	fn push(&mut self, event: BindingEvent) -> bool {
		self.queue.push(event)
	}

	// Take the oldest event about the binding this node is holding now. A node with no binding has
	// nothing an event could be about, which the queue answers by draining.
	//
	// THE IDENTITY IS WHAT SAYS WHICH GENERATION, not a copy of the number inside the binding. Two
	// places holding one value is two places that agree until somebody updates one of them, and the
	// one that would have been missed is the one a stale event is filtered against.
	fn pop(&mut self) -> Option<BindingEvent> {
		let generation: u64 = if self.binding.is_some() { self.id.generation } else { 0 };
		self.queue.pop(generation)
	}

	// The driver this node is currently trying, or an empty name for a node with none left.
	fn driver_name(&self) -> &'static [u8] {
		match self.candidates.get(self.candidate) {
			Some(entry) => entry.name,
			None => b"",
		}
	}

	// Whether this node still has work in flight - a driver that has been sent `BIND` and has not
	// reached a terminal state.
	fn in_flight(&self) -> bool {
		self.binding.is_some() && matches!(self.record.state, BindingState::Binding | BindingState::Stopping)
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
	// The child Domain the driver runs in. Rolled back LAST, after the process is gone and the
	// claim is given back: killing it first would take the process out from under a teardown that
	// is still reading its handles.
	domain: u64,
	// The driver's process, with MANAGE - what `spawn` returns and nothing used to keep.
	process: u64,
	// This service's end of the bootstrap channel.
	channel: u64,
	// The claim handle, and the key it names. Closing the handle would release the device on its
	// own; releasing it explicitly is how the terminal state is LEARNED rather than assumed.
	claim: u64,
	key: ClaimKey,
}

impl Attempt {
	fn new() -> Self {
		Self { domain: 0, process: 0, channel: 0, claim: 0, key: ClaimKey::default() }
	}

	// THE TRANSACTION COMMITS: what it took stays taken, and passes to whoever asked for the bind.
	//
	// CONSUMES the value rather than zeroing its fields, which is the difference between "these
	// handles are somebody else's now" and "somebody remembered to blank three variables". There is
	// no `Drop` here to disarm - a rollback that ran on every drop would have to be disarmed on this
	// path, and a disarm that is forgotten leaks a device silently.
	fn commit(self) {}

	// UNDO WHAT WAS TAKEN, IN REVERSE, and answer with where the node lands.
	//
	// The order is the property:
	//
	//   1. `SIG_KILL` the process - not a request it can decline, because a driver that failed its
	//      handshake is not a driver whose cooperation is available;
	//   2. close this service's own handles to it, including every provider it offered and nobody
	//      routed;
	//   3. release the claim by its key, which is what performs the device teardown - bus mastering
	//      off, the mapping revoked, interrupts masked, the IOMMU unmap confirmed. The teardown
	//      cannot be confirmed BEFORE this step, because the release is what starts it.
	//
	// IDEMPOTENT, because an interrupted rollback is the case that leaves the mess: every field is
	// zeroed as it is given back, so a second call finds nothing to do.
	//
	// The answer is `Quarantined` when the release did not confirm - and that is the one stated
	// exception to "either completes or leaves nothing": its frames, vectors and grants stay charged
	// and out of circulation, because the alternative is handing back memory a device may still be
	// writing to.
	unsafe fn roll_back(&mut self, offers: &mut Offers) -> BindingState {
		unsafe {
			if self.process != 0 {
				signal(self.process, SIG_KILL);
				close(self.process);
				self.process = 0;
			}
			offers.close_all();
			if self.channel != 0 {
				close(self.channel);
				self.channel = 0;
			}
			if self.claim == 0 {
				// Nothing was taken, so there is nothing to confirm and nothing to quarantine - but
				// a Domain may still have been created, and a Domain nobody kills is a subtree
				// charged to this manager for ever.
				if self.domain != 0 {
					domain_kill(self.domain);
					close(self.domain);
					self.domain = 0;
				}
				return BindingState::Backoff;
			}
			let outcome = device_release(self.claim);
			close(self.claim);
			self.claim = 0;
			// AND THE DOMAIN LAST, after the process is gone and the device is given back. Killing
			// it first would take the process out from under a teardown still reading its handles,
			// and the release above is what performs that teardown.
			if self.domain != 0 {
				domain_kill(self.domain);
				close(self.domain);
				self.domain = 0;
			}
			if outcome == CLAIM_STATE_QUARANTINED as i64 { BindingState::Quarantined } else { BindingState::Backoff }
		}
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

// How many providers the catalogue holds at once. A bound, and a stated one: every driver in the
// image may publish at most `MAX_INITIAL_OFFERS`, and `MAX_NODES_IN_FLIGHT` bounds the drivers - so
// this is generous rather than arbitrary, and a machine that exceeded it would be refused loudly by
// `publish` rather than silently losing a disk.
const MAX_PROVIDERS: usize = 32;

// One published provider.
struct Provider {
	id: ProviderId,
	kind: u16,
	// The publisher's own name for it, which is what a withdrawal names.
	token: u16,
	// The channel the driver serves it on. Zero for a free slot.
	handle: u64,
}

// WHAT IS PUBLISHED, BY KIND, WITH THE MANAGER OWNING EVERY IDENTITY IN IT.
struct Catalogue {
	entries: [Option<Provider>; MAX_PROVIDERS],
	// Bumped every time a slot is filled, so a reused slot is never mistaken for the provider that
	// left it.
	generation: u32,
}

impl Catalogue {
	const fn new() -> Self {
		Self { entries: [const { None }; MAX_PROVIDERS], generation: 0 }
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
				let Some(&(_, most)) = entry.provides.iter().find(|&&(declared, _)| declared == kind) else {
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
				self.entries[slot] = Some(Provider { id: ProviderId::new(binding, slot as u16, self.generation), kind: offers.kinds[index], token: offers.tokens[index], handle });
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
			Some(provider.id)
		}
	}

	// Withdraw everything a binding published, which is what the end of that binding means.
	unsafe fn withdraw_binding(&mut self, binding: BindingId) -> usize {
		unsafe {
			let mut gone: usize = 0;
			for slot in 0..MAX_PROVIDERS {
				if !self.entries[slot].as_ref().is_some_and(|provider| provider.binding_is(binding)) {
					continue;
				}
				if let Some(provider) = self.entries[slot].take()
					&& provider.handle != 0
				{
					close(provider.handle);
				}
				gone += 1;
			}
			gone
		}
	}

	// How many providers of `kind` THIS BINDING has published, which is what a per-entry bound is
	// about: two controllers of one kind each publishing one is not one controller publishing two.
	fn count_for(&self, binding: BindingId, kind: u16) -> usize {
		self.entries.iter().filter(|entry| entry.as_ref().is_some_and(|provider| provider.binding_is(binding) && provider.kind == kind)).count()
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

// One comparable number for a function's place on the bus, so "lowest address first" is one
// comparison rather than three.
fn address_of(provider: &Provider) -> u32 {
	((provider.id.binding.bus as u32) << 16) | ((provider.id.binding.dev as u32) << 8) | provider.id.binding.func as u32
}

impl Provider {
	// Whether this provider belongs to that binding - the same function AND the same generation,
	// because a provider published by a binding that is over is not this binding's.
	fn binding_is(&self, binding: BindingId) -> bool {
		self.id.binding == binding
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
unsafe fn give_up(record: &mut BindingRecord, txn: &mut Attempt, offers: &mut Offers, cause: FailureCause, driver_name: &[u8]) -> bool {
	unsafe { give_up_with(record, txn, offers, cause, driver_binding::StopIntent::Fault, driver_name) }
}

// The same, for a node that was asked to stop rather than one that failed.
//
// THE INTENT IS WHAT `Stopping` RESOLVES AGAINST. P02M0162's table sends a confirmed teardown on to
// `Backoff` and then back to `Binding`, which is right for a driver that died and exactly wrong for
// one that was asked to stop: the operator stops it and it starts again. An UNCONFIRMED teardown
// ignores the intent entirely and ends at `Quarantined`, because what is unknown is whether the
// device is still live and no intent changes that.
unsafe fn give_up_with(record: &mut BindingRecord, txn: &mut Attempt, offers: &mut Offers, cause: FailureCause, intent: driver_binding::StopIntent, driver_name: &[u8]) -> bool {
	unsafe {
		// THE PATH THROUGH THE TABLE DEPENDS ON WHETHER A DEVICE WAS TAKEN, and flattening that was
		// wrong. `Binding -> Failed` is for a transaction that failed BEFORE the claim; once there
		// is a device to quieten, the table's only way out is `Binding -> Stopping -> Failed`,
		// because `Stopping` is what "there is a teardown to run" means. Going straight to `Failed`
		// records a node that never had a device, which is a different story about the same boot.
		let took_the_device = txn.claim != 0;
		if took_the_device {
			record.move_to(BindingState::Stopping, Some(cause));
		}
		let after = txn.roll_back(offers);
		// The rollback says whether the device is quiet. `give_up` is only called once the decision
		// to stop trying has been made, so the confirmed outcome is `Failed` rather than `Backoff`.
		let landed = if after == BindingState::Quarantined {
			Some(BindingState::Quarantined)
		} else {
			// `give_up` is only reached once the decision to stop trying has been made, so there are
			// no attempts left to spend - which for a fault is `Failed`, and for the other intents is
			// whatever they say. `Shutdown` answers None: the manager is going away, so there is no
			// next binding to describe and entering a state nobody will read is a state nobody
			// wrote down for a reason.
			intent.confirmed_lands_at(false)
		};
		let state = match landed {
			Some(landed) if record.move_to(landed, Some(cause)) => landed,
			_ => record.state,
		};
		print(b"DeviceManager: ");
		print(driver_name);
		if intent == driver_binding::StopIntent::Fault {
			print(b" did not bind (");
			print(cause.name());
			print(b") - the node is ");
		} else {
			print(b" was stopped (");
			print(intent.name());
			print(b") - the node is ");
		}
		print(state.name());
		print(b"\n");
		false
	}
}

// GIVE THE ATTEMPT BACK AND TRY AGAIN. Answers false when the teardown did not confirm, which ends
// the node rather than rebinding over a device that may still be writing to memory.
unsafe fn retry_or_quarantine(record: &mut BindingRecord, txn: &mut Attempt, offers: &mut Offers, cause: FailureCause, driver_name: &[u8]) -> bool {
	unsafe {
		// Through `Stopping` for the same reason `give_up` does it: there is a device to quieten, and
		// the table has no edge from `Binding` to `Backoff` once one has been taken.
		if txn.claim != 0 {
			record.move_to(BindingState::Stopping, Some(cause));
		}
		let after = txn.roll_back(offers);
		if after == BindingState::Quarantined {
			record.move_to(BindingState::Quarantined, Some(FailureCause::TeardownUnconfirmed));
			print(b"DeviceManager: ");
			print(driver_name);
			print(b" - the teardown did not confirm, so this device is quarantined for the boot\n");
			return false;
		}
		record.move_to(BindingState::Backoff, Some(cause));
		print(b"DeviceManager: restarting ");
		print(driver_name);
		print(b"\n");
		// The next pass reopens the transaction from `Backoff`, which the table allows.
		true
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
			let Some(binding) = &node.binding else { continue };
			handles[set] = binding.channel;
			owner[set] = at;
			is_process[set] = false;
			set += 1;
			if binding.process != 0 {
				handles[set] = binding.process;
				owner[set] = at;
				is_process[set] = true;
				set += 1;
			}
			let deadline: u64 = node.incident.attempt_deadline();
			if soonest == 0 || deadline < soonest {
				soonest = deadline;
			}
		}
		if set == 0 {
			return false;
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
			if !timed_out {
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
						if node.beat.awaiting && sequence == node.beat.sequence {
							node.beat.awaiting = false;
							node.beat.due = clock().saturating_add(driver_protocol::heartbeat_period(node.beat.deadline) as u64);
						} else {
							node.push(BindingEvent::Ponged { generation, sequence });
						}
					}
				}
				driver_protocol::Opcode::Stopped => {
					node.push(BindingEvent::Stopped { generation });
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
		print(driver_name);
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
// `may_try_again` has already established there is room for it, so this only has to perform it.
// A parked wait rather than a spin: the scheduler is cooperative and a busy loop here would starve
// every other driver coming up beside this one.
unsafe fn back_off(incident: &Incident, attempt: u32) {
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
		sleep_until(until);
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
unsafe fn begin_bind(node: &mut Node, info: &DeviceInfo, elf: &[u8], driver_name: &[u8], key_producer: u64, power: u64, console_input: u64, device_privilege: u64) -> bool {
	unsafe {
		if node.attempt == 0 {
			// ONE WINDOW FOR THE WHOLE CHAIN OF ATTEMPTS, opened on the first and not per attempt.
			// Three attempts of two seconds plus their backoffs is 6.3 seconds for ONE device,
			// which is already past the window the kernel's settle ladder gives the whole boot -
			// and a machine with several unbindable devices multiplies it.
			node.incident = Incident::open();
		}
		let mut txn = Attempt::new();
		if !node.record.move_to(BindingState::Binding, None) {
			print(b"DeviceManager: refusing an illegal transition into binding for ");
			print(driver_name);
			print(b"\n");
			return false;
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
			ClaimReadiness::WaitAndSeeAgain => return false,
			ClaimReadiness::Terminal(cause) => return give_up(&mut node.record, &mut txn, &mut node.offers, cause, driver_name),
		}
		let grant: ClaimGrant = match device_claim(node.index, device_privilege) {
			Ok(grant) => grant,
			// NOT RETRYABLE, and named as such: the device is held by somebody else and waiting
			// changes nothing this service controls.
			Err(_) => return give_up(&mut node.record, &mut txn, &mut node.offers, FailureCause::ClaimRefused, driver_name),
		};
		txn.claim = grant.claim;
		txn.key = grant.key;
		node.record.generation = grant.key.generation;
		// THE SAME FUNCTION, ONE BINDING LATER. The BDF is what survives a rebind and the
		// generation is what makes the last binding's messages refusable.
		node.id = node.id.rebound(grant.key.generation);
		let (dm_side, driver_side): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return give_up(&mut node.record, &mut txn, &mut node.offers, FailureCause::ResourceExhausted, driver_name),
		};
		txn.channel = dm_side;
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
			return give_up(&mut node.record, &mut txn, &mut node.offers, FailureCause::ResourceExhausted, driver_name);
		}
		txn.domain = domain as u64;
		let process: i64 = spawn_in(elf, driver_side, domain as u64);
		if process < 0 {
			return give_up(&mut node.record, &mut txn, &mut node.offers, FailureCause::SpawnFailed, driver_name);
		}
		txn.process = process as u64;
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
		let mut resources: [(u16, u64); 5] = [(0u16, 0u64); 5];
		let mut resource_count: usize = 0;
		// The device's own MMIO capability is always first and always present.
		resources[resource_count] = (driver_protocol::ResourceKind::Device as u16, grant.memory);
		resource_count += 1;
		let use_msix: bool = driver_name == b"virtio_input" || driver_name == b"virtio_net" || driver_name == b"virtio_snd" || driver_name == b"xhci" || driver_name == b"virtio_gpu" || driver_name == b"dev_channel";
		if use_msix {
			let irq: i64 = device_msix_acquire(grant.claim);
			if irq < 0 {
				return give_up(&mut node.record, &mut txn, &mut node.offers, FailureCause::ResourceExhausted, driver_name);
			}
			resources[resource_count] = (driver_protocol::ResourceKind::Irq as u16, irq as u64);
			resource_count += 1;
		}
		if driver_name == b"virtio_input" || driver_name == b"xhci" {
			let sink: i64 = duplicate(key_producer, RIGHT_SEND | RIGHT_TRANSFER);
			if sink < 0 {
				return give_up(&mut node.record, &mut txn, &mut node.offers, FailureCause::ResourceExhausted, driver_name);
			}
			resources[resource_count] = (driver_protocol::ResourceKind::Keys as u16, sink as u64);
			resource_count += 1;
			// A CONNECTION OF ITS OWN, not a copy of an authority. These two used to be handed a
			// duplicate of the root-Domain handle - which can kill every process on the machine -
			// so that the Power key would work. What they get now can ask for a reboot and
			// nothing else, on a channel nobody else answers on.
			let Some(connection) = service_connect(power) else { return give_up(&mut node.record, &mut txn, &mut node.offers, FailureCause::ResourceExhausted, driver_name) };
			resources[resource_count] = (driver_protocol::ResourceKind::SysPower as u16, connection);
			resource_count += 1;
			// The capability that lets those keystrokes reach the console at all. A duplicate
			// per driver, for the same reason as the power connection.
			if console_input != 0 {
				let feed: i64 = duplicate(console_input, RIGHT_TRANSFER);
				if feed < 0 {
					return give_up(&mut node.record, &mut txn, &mut node.offers, FailureCause::ResourceExhausted, driver_name);
				}
				resources[resource_count] = (driver_protocol::ResourceKind::Console as u16, feed as u64);
				resource_count += 1;
			}
		}
		node.granted_resources = resource_count as u32;
		// WHICH RULE CHOSE THIS DRIVER, recorded where the choice is still in hand. An entry may
		// declare several and "virtio_console bound it" does not say which applied - the pinned
		// development console and the ordinary one are the same artifact under two rules.
		node.matched_rule = node.candidates.get(node.candidate).and_then(|entry| entry.rules.iter().position(|rule| rule.matches(info))).unwrap_or(0) as u32;
		// `BIND` - the device, and the count of what follows. No capability travels with it.
		let mut payload = [0u8; driver_protocol::MAX_PAYLOAD];
		let payload_len = driver_protocol::encode_bind(info, resource_count as u16, &mut payload);
		if !send_frame(dm_side, driver_protocol::Opcode::Bind, grant.key.generation, &payload[..payload_len], 0, 0) {
			return give_up(&mut node.record, &mut txn, &mut node.offers, FailureCause::DriverExited, driver_name);
		}
		for &(kind, handle) in resources[..resource_count].iter() {
			let mut kind_payload = [0u8; driver_protocol::U16_PAYLOAD_LEN];
			driver_protocol::encode_u16(kind, &mut kind_payload);
			// THE DEVICE CAPABILITY ARRIVES WITHOUT RIGHT_TRANSFER, through one attenuating
			// move. It is minted here WITH it, because this process is the one that hands it
			// over and cannot do that with a capability it may not move - minting it without
			// TRANSFER outright would break the boot on the first try, right here. The rule is
			// about the HOLDER: a driver cannot pass its device on.
			let mask: u32 = if kind == driver_protocol::ResourceKind::Device as u16 { RIGHT_READ | RIGHT_WRITE | RIGHT_MAP } else { RIGHTS_ALL };
			if !send_frame(dm_side, driver_protocol::Opcode::Resource, grant.key.generation, &kind_payload, handle, mask) {
				return give_up(&mut node.record, &mut txn, &mut node.offers, FailureCause::DriverExited, driver_name);
			}
		}
		// IN FLIGHT. The transaction's holdings become the node's binding, which is what the
		// central wait watches and what a rollback gives back.
		node.binding = Some(Binding { domain: txn.domain, process: txn.process, channel: txn.channel, claim: txn.claim, key: txn.key });
		txn.commit();
		true
	}
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
					if node.record.state == BindingState::Online {
						let entry = node.candidates[node.candidate];
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
					print(driver_name);
					print(b" answered a ping nobody asked; the watchdog is not reset by it\n");
					continue;
				}
				// A WEDGED DRIVER IS TORN DOWN LIKE A CRASHED ONE. The teardown is the same
				// transaction and the same retry-and-quarantine counter; what differs is the reason
				// for starting it, which is what the record carries.
				BindingEvent::Wedged { .. } => {
					print(b"DeviceManager: ");
					print(driver_name);
					print(b" stopped answering its control path inside the deadline its registry entry declares\n");
					// `hung`, NOT `handshake-timeout`. A driver that came up and then went quiet is
					// a different fact from one that never answered at all, and a reader cannot act
					// on "it did not answer" without knowing which.
					FailureCause::Hung
				}
				BindingEvent::Withdrawn { token, .. } => {
					let withdrawn = catalogue.withdraw(node.id, token);
					print(b"DeviceManager: ");
					print(driver_name);
					print(if withdrawn.is_some() { b" withdrew a provider it had published\n" } else { b" withdrew a provider that was not published under that token\n" });
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
					if !node.record.move_to(BindingState::Online, None) {
						print(b"DeviceManager: a second terminal frame on a binding that is already past its handshake - refused\n");
						return Step::Again;
					}
					let entry = node.candidates[node.candidate];
					catalogue.publish_all(node.id, entry, &mut node.offers);
					// SUPERVISION STARTS WHERE THE DRIVER SAYS IT IS UP, not at the bind: before
					// `READY` the bind budget is what bounds it, and two deadlines over one
					// interval is two authorities that disagree the first time one is slower.
					node.beat.arm(entry.heartbeat_deadline);
					return Step::Online;
				}
				// A `FAILED` FRAME IS ABOUT THE DRIVER, NEVER ABOUT ONE OF ITS CHILDREN.
				//
				// A controller whose child fails says so by WITHDRAWING that child's provider - the
				// binding stays `Online` and its siblings stay published. There is no child-failure
				// frame and there should not be one: the two are different facts and the protocol
				// already has a word for each.
				BindingEvent::Failed { code, .. } => {
					// A DRIVER THAT SAID WHY. Retryability is read off the code rather than decided
					// again here: `device-not-responding` and `out-of-memory` are the two a second
					// attempt can change, and the other three describe a driver that has read its
					// device and will not drive it however many times it is asked.
					print(b"DeviceManager: ");
					print(driver_name);
					print(if code.retryable() { b" reported a retryable failure\n" } else { b" reported a permanent failure\n" });
					FailureCause::DriverReported(code)
				}
				// A PLANNED STOP COMPLETING, which is not a failure and must not be recorded as one.
				// The node carries the intent it was stopped WITH, and that is what decides where a
				// confirmed teardown lands - a driver that died goes back round to `Backoff` and
				// then `Binding`, and an operator's stop that did the same would be a stop that
				// starts the driver again.
				BindingEvent::Stopped { .. } => {
					print(b"DeviceManager: ");
					print(driver_name);
					print(b" stopped cleanly: ");
					print(node.stop_intent.name());
					print(b"\n");
					FailureCause::DriverExited
				}
				// The process ended, or its channel closed with nothing terminal on it. Both are a
				// driver that is gone without having said anything.
				BindingEvent::Exited { .. } | BindingEvent::Closed { .. } => FailureCause::DriverExited,
				BindingEvent::TimedOut { .. } => {
					// A DRIVER THAT IS STILL THERE AND HAS NOT ANSWERED, which is the case the
					// budget exists for: before it, this wait had no end and one silent driver held
					// the manager - and therefore the boot - for as long as it liked.
					print(b"DeviceManager: ");
					print(driver_name);
					print(b" did not report in inside its share of the boot window\n");
					FailureCause::HandshakeTimeout
				}
			};
			// THE CAPTURE COMES FIRST, BEFORE ANYTHING IS GIVEN BACK. The Domain's counters cannot
			// be read once the Domain is killed, and the process cannot be asked once it is signalled
			// - so a capture taken after the rollback is a capture of the rollback.
			let report = capture(node, cause);
			report_incident(driver_name, &report);
			node.incident_report = Some(report);
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
				print(driver_name);
				print(b" went away holding published providers; they are withdrawn and whatever was in flight on them is NOT confirmed\n");
			}
			// ROLL BACK WHAT THIS BINDING HELD, through the one order there is. The binding is
			// TAKEN out of the node first, so an interrupted rollback cannot be re-entered against
			// handles it has already given back.
			let Some(binding) = node.binding.take() else { return Step::Done };
			let mut txn = binding.into_attempt();
			// A PLANNED STOP IS NEVER RETRIED, whatever the cause reads as: the whole point of
			// asking a driver to stop is that it stays stopped.
			let planned: bool = node.stop_intent != driver_binding::StopIntent::Fault;
			let retryable: bool = !planned && cause.retryable() && may_try_again(&node.incident, node.attempt);
			let alive: bool = if retryable { retry_or_quarantine(&mut node.record, &mut txn, &mut node.offers, cause, driver_name) } else { give_up_with(&mut node.record, &mut txn, &mut node.offers, cause, node.stop_intent, driver_name) };
			if !alive {
				return Step::Done;
			}
			if !retryable {
				return Step::NextCandidate;
			}
			node.attempt += 1;
			back_off(&node.incident, node.attempt);
			return Step::Again;
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
	catalogue: &'a Catalogue,
	nodes: &'a [Node],
}

// The operator's endpoint, served by this program and reached only through the one connection
// PermissionManager mints for the operator path.
struct PolicyView<'a> {
	nodes: &'a mut [Node],
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
		unsafe { apply_policy(node, verb) };
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

// What a verb does to the node itself. The write has already happened; this is the half no other
// component can perform.
unsafe fn apply_policy(node: &mut Node, verb: proto::system::PolicyVerb) {
	unsafe {
		use proto::system::PolicyVerb;
		match verb {
			// A DISABLE ON A RUNNING BINDING GOES THROUGH THE TEARDOWN, carrying the intent so it
			// does not rebind; one on a binding that is not running has nothing to give back.
			PolicyVerb::Disable => {
				node.stop_intent = driver_binding::StopIntent::OperatorDisable;
				if !node.record.move_to(BindingState::Disabled, None) {
					print(b"DeviceManager: the operator's disable is queued behind this binding's teardown\n");
				}
			}
			// ENABLE GOES TO `Unbound`, which is what INVITES the bind an enable is asking for.
			PolicyVerb::Enable => {
				node.stop_intent = driver_binding::StopIntent::Fault;
				node.record.move_to(BindingState::Unbound, None);
			}
			// SELECT CHANGES NOTHING NOW. It is stored, and it applies at the next bind.
			PolicyVerb::Select => {}
			// A RETRY GRANTS EXACTLY ONE FURTHER ATTEMPT and does not reset the automatic budget:
			// without that rule the two mechanisms meet in the table with nothing said, and whoever
			// implements it decides for themselves whether an operator can spend the budget again.
			PolicyVerb::Retry => {
				node.attempt = node.attempt.saturating_sub(1);
				node.incident = Incident::open();
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

	fn subscribe(&mut self, kind: proto::system::ProviderKind) -> Vec<proto::system::ProviderInfo> {
		let wire: u16 = provider_kind_wire(kind);
		self.catalogue.entries.iter().filter_map(|entry| entry.as_ref()).filter(|provider| provider.kind == wire).map(|provider| proto::system::ProviderInfo { kind, bus: provider.id.binding.bus as u32, dev: provider.id.binding.dev as u32, func: provider.id.binding.func as u32, binding_generation: provider.id.binding.generation, slot: provider.id.slot as u32, provider_generation: provider.id.generation }).collect()
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

// Decide what a verb does to this node, WITHOUT applying it.
//
// Separated so the decision can be read on its own: what a verb means is a question about the
// binding's state, and what it costs is a question about ConfigService.
fn decide_policy(node: &Node, verb: proto::system::PolicyVerb, artifact: &str) -> PolicyDecision {
	use proto::system::{PolicyOutcome, PolicyVerb};
	let none = |outcome: PolicyOutcome| PolicyDecision { outcome, store: None, remove: false };
	// BOOT-CRITICAL BINDINGS ARE OUT. Their policy would live on a volume that is not mounted when
	// those bindings are made, which is a dependency the wrong way round.
	if node.candidates.first().is_some_and(|entry| entry.boot_critical) {
		return none(PolicyOutcome::Refused);
	}
	match verb {
		PolicyVerb::Retry => {
			// QUARANTINE IS OUT OF REACH OF THIS ONE, for this boot. Its resources are charged and
			// out of circulation precisely because nothing confirmed the device was quiet, and an
			// operator saying so does not make it so.
			if node.record.state == BindingState::Quarantined {
				return none(PolicyOutcome::Quarantined);
			}
			// AN ACTION, NOT A STORED PREFERENCE. Nothing is written.
			none(PolicyOutcome::Accepted)
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
			if !node.candidates.iter().any(|entry| entry.name == artifact.as_bytes()) {
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
	}
}

// The wire number the driver protocol gives a typed kind. The two sets are the same set spelled
// twice - once in the IDL for clients and once in `driver_protocol` for the wire - and this is the
// one place they meet, so a divergence is a compile error here rather than a provider nobody finds.
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
unsafe fn serve_policy_once(service: u64, is_root: bool, clients: &mut CatalogueClients, nodes: &mut [Node], config: u64, buf: &mut [u8]) {
	unsafe {
		let ReceivedCaps::Message { len, handles } = recv_caps_blocking(service, buf) else { return };
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
				return;
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
				return;
			}
		}
		let mut view = PolicyView { nodes, config };
		let mut reply = [0u8; 1024];
		let mut request_handles = wire::Handles::new();
		let mut reply_handles = wire::Handles::new();
		let request: Vec<u8> = buf[..len].to_vec();
		if let Some(written) = proto::system::device_policy_admin::dispatch(&mut view, &request, &mut request_handles, &mut reply, &mut reply_handles) {
			send_blocking(service, &reply[..written], 0);
		}
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
}

// Answer one catalogue request on `channel`.
//
// THE ROOT MINTS CONNECTIONS AND THE CONNECTIONS CARRY REQUESTS, which is the shape every other
// multi-client server in this tree has. It was a single typed dispatch, and `service_connect` -
// which is how every consumer reaches a service here - sends the reserved CONNECT opcode and waits:
// so the first thing that ever asked for the catalogue hung the boot, because nothing answered it.
unsafe fn serve_catalogue_once(channel: u64, is_root: bool, clients: &mut CatalogueClients, catalogue: &Catalogue, nodes: &[Node], buf: &mut [u8]) -> bool {
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
		let mut view = CatalogueView { catalogue, nodes };
		let mut reply = [0u8; 4096];
		let mut request_handles = wire::Handles::new();
		let mut reply_handles = wire::Handles::new();
		let request: Vec<u8> = buf[..len].to_vec();
		if let Some(written) = proto::system::provider_catalogue::dispatch(&mut view, &request, &mut request_handles, &mut reply, &mut reply_handles) {
			send_blocking(channel, &reply[..written], 0);
		}
		true
	}
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
		match snapshot.state {
			CLAIM_STATE_FREE => ClaimReadiness::Bindable,
			CLAIM_STATE_RELEASING => {
				// The kernel latches the deadline itself, inside the same read - so a `Releasing`
				// that comes back here still has time left, and one that ran out came back
				// `Quarantined` instead. There is no arithmetic to repeat and no second authority.
				print(b"DeviceManager: ");
				print(driver_name);
				print(b"'s device is still being torn down by whoever held it last; not acquiring it yet\n");
				ClaimReadiness::WaitAndSeeAgain
			}
			CLAIM_STATE_QUARANTINED => {
				print(b"DeviceManager: ");
				print(driver_name);
				print(b"'s device is quarantined - nothing observed it go quiet, and it is not claimed again this boot\n");
				node.record.move_to(BindingState::Quarantined, Some(FailureCause::TeardownUnconfirmed));
				ClaimReadiness::Terminal(FailureCause::TeardownUnconfirmed)
			}
			// A CORRECT `domain_kill` CANNOT LEAVE IT HERE. Reported as the invariant violation it
			// is rather than bound over: a manager that quietly rebound would be handing out a
			// device somebody still holds.
			_ => {
				print(b"DeviceManager: ");
				print(driver_name);
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
unsafe fn stop_all(nodes: &mut [Node], catalogue: &mut Catalogue, intent: driver_binding::StopIntent, buf: &mut [u8]) {
	unsafe {
		// The order, by how many OTHER nodes' published kinds a node requires. A node requiring
		// something is a dependent and goes first; one requiring nothing is a leaf provider and
		// goes last. A cycle cannot appear here - `system-manifest` refuses one at registry-build
		// time - so a single ordering pass is enough and there is no fixed point to chase.
		let mut order: Vec<usize> = (0..nodes.len()).collect();
		order.sort_by_key(|&at| {
			let entry = nodes[at].candidates.get(nodes[at].candidate);
			core::cmp::Reverse(entry.map_or(0, |entry| entry.requires.len()))
		});
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
			while clock() < deadline {
				drain_channel(&mut nodes[at], buf);
				if nodes[at].queue.is_empty() {
					wait(channel, deadline);
					continue;
				}
				let _ = advance(&mut nodes[at], name, catalogue);
				if nodes[at].record.state != BindingState::Online {
					break;
				}
			}
			if nodes[at].record.state == BindingState::Online {
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
			if node.beat.awaiting {
				if now >= node.beat.expires {
					// NOT ANSWERED INSIDE THE DEADLINE ITS ENTRY DECLARED. Queued as an event so it
					// runs through the same state machine as a crash - the teardown is the same
					// transaction and the same counter, and only the reason differs.
					node.push(BindingEvent::Wedged { generation });
					node.beat.awaiting = false;
				}
			} else if now >= node.beat.due {
				node.beat.sequence = node.beat.sequence.wrapping_add(1);
				let mut payload = [0u8; driver_protocol::SEQUENCE_PAYLOAD_LEN];
				driver_protocol::encode_sequence(node.beat.sequence, &mut payload);
				if send_frame(channel, driver_protocol::Opcode::Ping, generation, &payload, 0, 0) {
					node.beat.awaiting = true;
					node.beat.expires = now.saturating_add(node.beat.deadline as u64);
				} else {
					// The channel is gone, which is a driver that ended rather than one that is
					// slow. The exit event will arrive on its own; this only stops asking.
					node.beat.due = now.saturating_add(node.beat.deadline as u64);
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
	fn matches(self, info: &DeviceInfo) -> bool {
		if self.pci_class.is_some_and(|class| info.class != class) {
			return false;
		}
		if self.pci_subclass.is_some_and(|subclass| info.subclass != subclass) {
			return false;
		}
		if self.pci_interface.is_some_and(|interface| info.prog_if != interface) {
			return false;
		}
		// THE TRANSPORT IS ASKED BEFORE THE TYPE, and that ordering is the whole point of the pair:
		// `virtio_type` is only a virtio number on a function whose transport says so. Without it a
		// rule for virtio type 2 would match anything this system happens to number 2 next.
		if self.transport.is_some_and(|transport| info.transport != transport) {
			return false;
		}
		if self.virtio_type.is_some_and(|kind| info.device_type != kind) {
			return false;
		}
		if self.pci_vendor.is_some_and(|vendor| info.vendor != vendor) {
			return false;
		}
		if self.pci_product.is_some_and(|product| info.product != product) {
			return false;
		}
		if let Some(address) = self.pci_address
			&& (info.bus != address.bus || info.dev != address.dev || info.func != address.func)
		{
			return false;
		}
		true
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
	provides: &'static [(u16, u16)],
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
