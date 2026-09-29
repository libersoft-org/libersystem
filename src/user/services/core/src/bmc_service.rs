// BmcService - the one consumer of every `ipmi` provider: each baseboard management controller the system's IPMI
// interfaces reach, by its name, and the `bmc` tool's view of them.
//
// WHAT IT HOLDS AND WHAT IT DOES NOT. It holds no claim and publishes nothing. It consumes `ipmi` through a catalogue
// connection minted for that kind alone, groups the bindings by the BMC they reach - ONE BMC reached twice, through two
// interfaces answering one GUID, is reported as a pair an operator may reduce with DeviceManager's `disable`, and
// nothing is de-duplicated automatically - and serves the tool's reads through one binding of each BMC. It carries no
// raw command and stops nothing: erasing the log and the chassis operations are the drivers' executors', which only
// AdminService reaches.
//
// THE SYSTEM'S OWN EVENTS INTO EVERY BMC'S LOG, once per BMC and a pair's through one of its interfaces:
//   BOOT COMPLETED, when ServiceManager's status - read on the `supervisor` channel the plan delivers - shows every row
//   with a wanted state `running` or `stopped`: the test behind ServiceManager's own "online". The canary's and the
//   drivers' rows carry no wanted state and are not read. At most once per boot: an instance whose own row shows a
//   restart does not write it again.
//   AN ORDERLY SHUTDOWN, on the orderly-shutdown notice - a power-off and a reboot alike - answered once each event is
//   sent or the notice's bound is spent. The immediate power paths carry no notice, and log nothing.
//
// A RESTART IS TRANSPARENT: the replacement reads every provider again from the catalogue's snapshot.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::generated::liber::bmc::v1 as bmc;
use proto::system::{Error, ProviderInfo, ProviderKind, ShutdownAction, provider_catalogue, shutdown_notice, supervisor};
use rt::*;
use wire::Handles;

include!(concat!(env!("OUT_DIR"), "/roles_bmc_service.rs"));

const MAX_CLIENTS: usize = 16;
const MAX_BINDINGS: usize = 8;
const MAX_FOLLOWERS: usize = 8;
// How long one question to a binding may take: a read of every sensor, or of a FRU device, is many transactions.
const PROVIDER_TICKS: u64 = 60 * TICKS_PER_SECOND;
const QUICK_TICKS: u64 = 10 * TICKS_PER_SECOND;
// ServiceManager's status is read this often until the boot has settled.
const STATUS_TICKS: u64 = 2 * TICKS_PER_SECOND;
// A followed log is polled this often.
const FOLLOW_TICKS: u64 = 5 * TICKS_PER_SECOND;
// The IDs a follower remembers having sent.
const FOLLOW_MEMORY: usize = 1024;
// Pages read at most in one follow poll.
const FOLLOW_PAGES: usize = 16;
// Frames queued on a follower's stream before one is dropped.
const STREAM_DEPTH: u64 = 64;
// This service's own row in ServiceManager's status.
const OWN_ROW: &str = "bmc_service";

fn say(text: &str) {
	let line = format!("BmcService: {text}\n");
	print(line.as_bytes());
}

// ONE BINDING OF ONE BMC: its publication, its request channel, its change stream and what it last described.
struct Binding {
	info: ProviderInfo,
	chan: u64,
	changes: u64,
	described: bmc::BindingInfo,
}

// A FOLLOWED LOG: the stream the tool reads, the BMC's name, and the entries already sent.
struct Follower {
	stream: u64,
	name: String,
	seq: u32,
	last_addition: u32,
	sent: Vec<u16>,
	next: u64,
}

struct Service {
	catalogue: u64,
	bindings: Vec<Binding>,
	followers: Vec<Follower>,
	status: u64,
	// The boot's event: whether this instance may write it at all, whether the boot has settled, and the BMCs it was
	// written to.
	may_write_boot: bool,
	settled: bool,
	// Whether a status that answered an error was said.
	status_refused: bool,
	booted: Vec<String>,
	next_status: u64,
	// The name a follow request named, as the generated decoder handed it over.
	follow_name: Option<String>,
}

fn provider(chan: u64, ticks: u64) -> bmc::ipmi_provider::Client<ChannelTransport> {
	bmc::ipmi_provider::Client::with_deadline(ChannelTransport { chan }, clock() + ticks)
}

impl Service {
	// THE BMCS, by name, each with its bindings in the order they were published.
	fn bmcs(&self) -> Vec<(String, Vec<usize>)> {
		let mut out: Vec<(String, Vec<usize>)> = Vec::new();
		for (at, binding) in self.bindings.iter().enumerate() {
			match out.iter_mut().find(|(name, _)| *name == binding.described.name) {
				Some((_, members)) => members.push(at),
				None => out.push((binding.described.name.clone(), alloc::vec![at])),
			}
		}
		out
	}

	// The binding a BMC's reads go through: its first available one, or its first.
	fn primary(&self, name: &str) -> Option<usize> {
		let members: Vec<usize> = self.bindings.iter().enumerate().filter(|(_, binding)| binding.described.name == name).map(|(at, _)| at).collect();
		members.iter().copied().find(|&at| self.bindings[at].described.available).or_else(|| members.first().copied())
	}

	fn adopt(&mut self, info: ProviderInfo) {
		if self.bindings.len() >= MAX_BINDINGS {
			say("an ipmi provider was refused: this service holds eight");
			return;
		}
		if self.bindings.iter().any(|binding| binding.info.slot == info.slot && binding.info.provider_generation == info.provider_generation) {
			return;
		}
		let Some(Ok(chan)) = provider_catalogue::Client::new(ChannelTransport { chan: self.catalogue }).open(&info) else {
			say("a published ipmi provider could not be opened");
			return;
		};
		let Some(Ok(described)) = provider(chan, QUICK_TICKS).describe() else {
			say("an ipmi provider did not describe itself");
			close(chan);
			return;
		};
		let changes = provider(chan, QUICK_TICKS).changes().unwrap_or(0);
		say(&format!("{} is reached through {:?} at {}{}", described.name, described.interface, described.binding, if described.available { "" } else { " - unavailable" }));
		// A PAIR: two interfaces to one BMC, each answering its GUID. Reported, never de-duplicated.
		if described.guid
			&& let Some(other) = self.bindings.iter().find(|binding| binding.described.name == described.name)
		{
			say(&format!("{} is reached through two interfaces - {} and {}; DeviceManager's `disable` removes one, and its watchdog is taken through the first only", described.name, other.described.binding, described.binding));
		}
		let name = described.name.clone();
		self.bindings.push(Binding { info, chan, changes, described });
		// A BMC THAT ARRIVES AFTER THE BOOT SETTLED is written its boot event too, once.
		if self.settled {
			self.write_boot(&name);
		}
	}

	fn withdraw(&mut self, info: &ProviderInfo) {
		if let Some(at) = self.bindings.iter().position(|binding| binding.info.slot == info.slot && binding.info.provider_generation == info.provider_generation && binding.info.binding_generation == info.binding_generation) {
			let binding = self.bindings.remove(at);
			close(binding.chan);
			if binding.changes != 0 {
				close(binding.changes);
			}
			say(&format!("{} is no longer reached through {}", binding.described.name, binding.described.binding));
		}
	}

	// A change a binding reported: its description read again.
	fn changed(&mut self, at: usize, change: &bmc::Change) {
		let chan = self.bindings[at].chan;
		if let Some(Ok(described)) = provider(chan, QUICK_TICKS).describe() {
			self.bindings[at].described = described;
		}
		say(&format!("{} through {}: {:?}", change.name, self.bindings[at].described.binding, change.kind));
	}

	// ------------------------------------------------------------ the system's own events

	// THE BOOT'S EVENT to one BMC, through one of its bindings, once.
	fn write_boot(&mut self, name: &str) {
		if !self.may_write_boot || self.booted.iter().any(|written| written == name) {
			return;
		}
		let Some(at) = self.primary(name) else { return };
		let status = provider(self.bindings[at].chan, QUICK_TICKS).system_event(&bmc::SystemEventKind::BootCompleted);
		match status {
			Some(Ok(status)) if status.outcome == bmc::Outcome::Answered => {
				say(&format!("the boot completed - its event is in the log of {name}"));
				self.booted.push(String::from(name));
			}
			other => say(&format!("the boot's event could not be written to the log of {name} - {other:?}")),
		}
	}

	// WHETHER THE BOOT HAS SETTLED: every row with a wanted state running or stopped - and whether this instance is a
	// restarted one, which writes nothing.
	fn read_status(&mut self) {
		if self.status == 0 || self.settled {
			return;
		}
		// UNANSWERED is the boot still bringing the standing loop up; an ERROR is said, once, since the boot's event
		// waits on it.
		let rows = match supervisor::Client::with_deadline(ChannelTransport { chan: self.status }, clock() + QUICK_TICKS).status() {
			Some(Ok(rows)) => rows,
			Some(Err(error)) => {
				if !self.status_refused {
					self.status_refused = true;
					say(&format!("ServiceManager's status answered {error:?} - the boot's event waits for a status it can read"));
				}
				return;
			}
			None => return,
		};
		if rows.iter().any(|row| row.name == OWN_ROW && row.restarts > 0) && self.may_write_boot {
			self.may_write_boot = false;
			say("this instance is a restarted one - the boot's event was written by the first, and is not written again");
		}
		let settled = rows.iter().filter(|row| !row.desired.is_empty()).all(|row| row.state == "running" || row.state == "stopped");
		if !settled {
			return;
		}
		self.settled = true;
		for (name, _) in self.bmcs() {
			self.write_boot(&name);
		}
	}

	// THE ORDERLY SHUTDOWN'S EVENT to every BMC, through one binding each, within the notice's bound.
	fn shutdown(&mut self, action: ShutdownAction) {
		for (name, _) in self.bmcs() {
			let Some(at) = self.primary(&name) else { continue };
			let status = provider(self.bindings[at].chan, TICKS_PER_SECOND / 2).system_event(&bmc::SystemEventKind::GracefulShutdown);
			say(&format!(
				"an orderly {} - its event {} the log of {name}",
				match action {
					ShutdownAction::PowerOff => "power-off",
					ShutdownAction::Reboot => "reboot",
				},
				if matches!(status, Some(Ok(ref status)) if status.outcome == bmc::Outcome::Answered) { "is in" } else { "could not be written to" }
			));
		}
	}

	// ------------------------------------------------------------ a followed log

	fn follow_poll(&mut self, at: usize) {
		let name = self.followers[at].name.clone();
		let Some(binding) = self.primary(&name) else { return };
		let chan = self.bindings[binding].chan;
		let Some(Ok(info)) = provider(chan, QUICK_TICKS).sel_info() else { return };
		if info.status.outcome != bmc::Outcome::Answered || info.last_addition == self.followers[at].last_addition {
			return;
		}
		self.followers[at].last_addition = info.last_addition;
		// THE ENTRIES NOT YET SENT, read from the log's start page by page.
		let mut first = 0u16;
		for _ in 0..FOLLOW_PAGES {
			let Some(Ok(page)) = provider(chan, PROVIDER_TICKS).sel_page(&first) else { return };
			for entry in page.entries.iter() {
				let follower = &mut self.followers[at];
				if follower.sent.contains(&entry.id) {
					continue;
				}
				if follower.sent.len() >= FOLLOW_MEMORY {
					follower.sent.remove(0);
				}
				follower.sent.push(entry.id);
				let mut frame = [0u8; 512];
				let mut handles = Handles::new();
				if let Some(len) = bmc::bmc::sel_follow_frame(follower.seq, entry, &mut frame, &mut handles) {
					follower.seq = follower.seq.wrapping_add(1);
					let _ = try_send(follower.stream, &frame[..len], 0);
				}
			}
			if page.next == 0xFFFF {
				break;
			}
			first = page.next;
		}
	}
}

// ------------------------------------------------------------------ the tool's interface

struct ToolView<'a> {
	service: &'a mut Service,
}

impl ToolView<'_> {
	fn binding(&self, name: &str) -> Result<u64, Error> {
		self.service.primary(name).map(|at| self.service.bindings[at].chan).ok_or(Error::NotFound)
	}

	fn forward<T>(&self, name: &str, ticks: u64, call: impl FnOnce(&mut bmc::ipmi_provider::Client<ChannelTransport>) -> Option<Result<T, Error>>) -> Result<T, Error> {
		let chan = self.binding(name)?;
		call(&mut provider(chan, ticks)).unwrap_or(Err(Error::TimedOut))
	}
}

impl bmc::bmc::Service for ToolView<'_> {
	fn list(&mut self) -> Result<Vec<bmc::BmcSummary>, Error> {
		Ok(self
			.service
			.bmcs()
			.into_iter()
			.map(|(name, members)| {
				let first = &self.service.bindings[members[0]].described;
				bmc::BmcSummary { name: name.clone(), administrable: first.administrable, available: members.iter().any(|&at| self.service.bindings[at].described.available), device: first.device.clone(), bindings: members.iter().take(4).map(|&at| self.service.bindings[at].described.clone()).collect::<Vec<_>>().into(), paired: members.len() > 1 }
			})
			.collect())
	}

	fn sensors(&mut self, name: String) -> Result<bmc::Sensors, Error> {
		self.forward(&name, PROVIDER_TICKS, |client| client.sensors())
	}

	fn sel_info(&mut self, name: String) -> Result<bmc::SelInfo, Error> {
		self.forward(&name, QUICK_TICKS, |client| client.sel_info())
	}

	fn sel_page(&mut self, name: String, first: u16) -> Result<bmc::SelPage, Error> {
		self.forward(&name, PROVIDER_TICKS, |client| client.sel_page(&first))
	}

	// THE FOLLOW'S FIRST FRAMES: none - what the log already holds is `sel-page`'s; a follow is what arrives after it.
	fn sel_follow(&mut self, name: String) -> Vec<bmc::SelEntry> {
		self.service.follow_name = Some(name);
		Vec::new()
	}

	fn fru(&mut self, name: String) -> Result<bmc::Fru, Error> {
		self.forward(&name, PROVIDER_TICKS, |client| client.fru())
	}

	fn chassis(&mut self, name: String) -> Result<bmc::Chassis, Error> {
		self.forward(&name, QUICK_TICKS, |client| client.chassis())
	}

	fn identify(&mut self, name: String, seconds: u8) -> Result<bmc::Status, Error> {
		self.forward(&name, QUICK_TICKS, |client| client.identify(&seconds))
	}

	fn lan(&mut self, name: String) -> Result<bmc::Lan, Error> {
		self.forward(&name, PROVIDER_TICKS, |client| client.lan())
	}
}

// A follow's request: the stream opened, its IDs so far recorded as sent, and the service polling it from now.
fn open_follow(service: &mut Service, chan: u64, request: &[u8], handles: &mut Handles) {
	let mut view = ToolView { service };
	let Some((corr, _)) = bmc::bmc::sel_follow_open(&mut view, request, handles) else { return };
	let Some(name) = service.follow_name.take() else { return };
	if service.primary(&name).is_none() || service.followers.len() >= MAX_FOLLOWERS {
		send_blocking(chan, &corr.to_le_bytes(), 0);
		return;
	}
	let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return };
	// WHAT THE LOG HOLDS NOW is not followed: the entries already there are recorded as sent.
	let mut sent = Vec::new();
	let mut last_addition = 0;
	if let Some(at) = service.primary(&name) {
		let chan = service.bindings[at].chan;
		if let Some(Ok(info)) = provider(chan, QUICK_TICKS).sel_info() {
			last_addition = info.last_addition;
		}
		let mut first = 0u16;
		for _ in 0..FOLLOW_PAGES {
			let Some(Ok(page)) = provider(chan, PROVIDER_TICKS).sel_page(&first) else { break };
			sent.extend(page.entries.iter().map(|entry| entry.id));
			if page.next == 0xFFFF {
				break;
			}
			first = page.next;
		}
	}
	service.followers.push(Follower { stream: producer, name, seq: 0, last_addition, sent, next: clock() + FOLLOW_TICKS });
	send_caps_blocking(chan, &corr.to_le_bytes(), &[consumer]);
}

// ------------------------------------------------------------------ the notice

struct Notice<'a> {
	service: &'a mut Service,
}

impl shutdown_notice::Service for Notice<'_> {
	fn prepare(&mut self, action: ShutdownAction) -> Result<(), Error> {
		self.service.shutdown(action);
		Ok(())
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	// THE ROLES: a catalogue connection minted for `ipmi` alone, ServiceManager's `supervisor` status channel, and the
	// root the tool's connections are minted on.
	let mut roles: [u64; BOOTSTRAP_ROLES.len()] = [0; BOOTSTRAP_ROLES.len()];
	if let Err(error) = receive_roles(bootstrap, &BOOTSTRAP_ROLES, &mut roles) {
		fail_bootstrap(bootstrap, error.tag(), error.reason());
	}
	let (catalogue, status, root) = (roles[0], roles[1], roles[2]);
	let subscription: u64 = if catalogue != 0 { provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::Ipmi).unwrap_or(0) } else { 0 };
	let mut service = Service { catalogue, bindings: Vec::new(), followers: Vec::new(), status, may_write_boot: true, settled: false, status_refused: false, booted: Vec::new(), next_status: clock(), follow_name: None };
	print(b"BmcService: online\n");
	send_blocking(bootstrap, b"BmcService: online", 0);

	let mut clients: Vec<u64> = Vec::new();
	let mut buf = alloc::vec![0u8; 8192];
	let mut reply = alloc::vec![0u8; 131072];
	let mut control = bootstrap;
	let mut subscribed = subscription != 0;
	let timer: u64 = match timer_create() {
		t if t > 0 => t as u64,
		_ => exit(),
	};
	loop {
		let now = clock();
		if !service.settled && now >= service.next_status {
			service.next_status = now + STATUS_TICKS;
			service.read_status();
		}
		for at in 0..service.followers.len() {
			if clock() >= service.followers[at].next {
				service.followers[at].next = clock() + FOLLOW_TICKS;
				service.follow_poll(at);
			}
		}
		let mut due = service.followers.iter().map(|follower| follower.next).min().unwrap_or(u64::MAX);
		if !service.settled && service.status != 0 {
			due = due.min(service.next_status);
		}
		timer_set(timer, due);
		let mut waitset: Vec<u64> = alloc::vec![timer];
		if control != 0 {
			waitset.push(control);
		}
		if root != 0 {
			waitset.push(root);
		}
		if subscribed {
			waitset.push(subscription);
		}
		waitset.extend(service.bindings.iter().filter(|binding| binding.changes != 0).map(|binding| binding.changes));
		waitset.extend(service.followers.iter().map(|follower| follower.stream));
		waitset.extend(clients.iter().copied());
		let ready = wait_any(&waitset, 0);
		if ready < 0 {
			continue;
		}
		let handle = waitset[ready as usize];
		if handle == timer {
			continue;
		}
		// THE CATALOGUE: a provider arriving or leaving.
		if subscribed && handle == subscription {
			loop {
				let (len, handles) = match try_recv_caps(subscription, &mut buf) {
					PolledCaps::Message { len, handles } => (len, handles),
					PolledCaps::Empty => break,
					PolledCaps::Closed => {
						subscribed = false;
						break;
					}
				};
				for &leftover in handles.as_slice() {
					close(leftover);
				}
				let mut frame_handles = Handles::new();
				let Some(info) = provider_catalogue::subscribe_read(&buf[..len], &mut frame_handles) else { continue };
				if info.live {
					service.adopt(info);
				} else {
					service.withdraw(&info);
				}
			}
			continue;
		}
		// A BINDING'S CHANGES.
		if let Some(at) = service.bindings.iter().position(|binding| binding.changes == handle) {
			loop {
				match try_recv_caps(handle, &mut buf) {
					PolledCaps::Message { len, mut handles } => {
						if let Some(change) = bmc::ipmi_provider::changes_read(&buf[..len], &mut handles) {
							service.changed(at, &change);
						}
						for &leftover in handles.as_slice() {
							close(leftover);
						}
					}
					PolledCaps::Empty => break,
					PolledCaps::Closed => {
						close(handle);
						service.bindings[at].changes = 0;
						break;
					}
				}
			}
			continue;
		}
		// A FOLLOWER'S STREAM: readable only when its reader went.
		if let Some(at) = service.followers.iter().position(|follower| follower.stream == handle) {
			if let PolledCaps::Closed = try_recv_caps(handle, &mut buf) {
				close(handle);
				service.followers.remove(at);
			}
			continue;
		}
		// THE CONTROL CHANNEL: ServiceManager's notice.
		if handle == control {
			let (len, mut handles) = match try_recv_caps(control, &mut buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => continue,
				PolledCaps::Closed => {
					control = 0;
					continue;
				}
			};
			let mut reply_handles = Handles::new();
			let written = shutdown_notice::dispatch(&mut Notice { service: &mut service }, &buf[..len], &mut handles, &mut reply, &mut reply_handles);
			for &leftover in handles.as_slice().iter().chain(reply_handles.as_slice()) {
				close(leftover);
			}
			if let Some(written) = written {
				let _ = try_send(control, &reply[..written], 0);
			}
			continue;
		}
		// THE ROOT mints a connection; a CLIENT speaks `bmc`.
		let is_root = handle == root;
		let (len, mut handles) = match try_recv_caps(handle, &mut buf) {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => continue,
			PolledCaps::Closed => {
				if !is_root {
					clients.retain(|&client| client != handle);
					close(handle);
				}
				continue;
			}
		};
		if len >= 2 {
			let op = u16::from_le_bytes([buf[0], buf[1]]);
			if op == HEARTBEAT_OP {
				send_blocking(handle, b"PONG", 0);
				continue;
			}
			if op == CONNECT_OP {
				match channel().filter(|_| clients.len() < MAX_CLIENTS) {
					Some((mine, theirs)) => {
						clients.push(mine);
						send_blocking(handle, &[], theirs);
					}
					None => {
						send_blocking(handle, &[], 0);
					}
				}
				continue;
			}
			if !is_root && op == bmc::bmc::OP_SEL_FOLLOW {
				let request = buf[..len].to_vec();
				open_follow(&mut service, handle, &request, &mut handles);
				continue;
			}
		}
		if is_root {
			for &leftover in handles.as_slice() {
				close(leftover);
			}
			continue;
		}
		let request = buf[..len].to_vec();
		let mut reply_handles = Handles::new();
		match bmc::bmc::dispatch(&mut ToolView { service: &mut service }, &request, &mut handles, &mut reply, &mut reply_handles) {
			Some(written) => {
				send_caps_blocking(handle, &reply[..written], reply_handles.as_slice());
			}
			// A REQUEST THE INTERFACE DOES NOT CARRY ends that connection.
			None => {
				clients.retain(|&client| client != handle);
				close(handle);
			}
		}
	}
}
