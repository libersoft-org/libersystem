// permission_manager - the userspace permission-policy manager (PermissionManager).
//
// PermissionManager is the policy over the kernel's capability mechanism. ServiceManager
// starts it from the init package and hands it the clients it is allowed to grant onward
// (a StorageService, a LogService, a NetworkService, and a TimeService client), a
// ProcessService client (the loading mechanism it drives to start the components it
// governs), and a "SERVE" channel its clients reach it on. It never loads a program itself
// - it reaches the kernel loader only through ProcessService, so mechanism (loading) and
// policy (granting) live in separate services and no one service can both load a program
// and reach every capability.
//
// Its policy is a typed permission manifest per component - a `Manifest` of `Capability`
// grants, the typed source of truth for what a component may be given (never a text or
// JSON file). When it launches a component it asks ProcessService to start it with a fresh
// bootstrap channel, then grants that component exactly its manifest's capabilities over
// that channel and nothing else - the strict app sandbox - and records every decision
// (grant or denial) in an audit trail. A component may also request a capability its
// manifest does not declare at runtime; the manager decides it with a non-interactive
// (headless) policy default - least privilege, so an undeclared request is refused - and
// records that request in the same audit trail as a dynamic decision (the dynamic path for
// later untrusted apps). Over the SERVE channel callers speak the generated `liber:system`
// Permission bindings: `lookup` returns a component's manifest, `audit` returns the trail,
// and `run` launches a named system tool on demand - the launcher / granter primitive: it
// starts the tool under its manifest, grants it exactly its declared capabilities, forwards
// the caller's stdout console and argument string, and returns the live process handle for
// job control (so the shell reaches the OS tools only through the manager, never the raw
// kernel loader).
//
// This milestone it governs four components. Two are report-back probes that prove the grant
// paths: sandbox_probe, whose manifest grants storage and log but not network, reads its one
// granted file and reports the bytes back; and request_probe, whose manifest grants only log,
// asks for an undeclared capability (storage) at runtime, which the headless policy refuses
// and records as a dynamic denial. The other two are real system tools the manager launches
// on demand through the `run` op - the launcher / granter path - each printing to a captured
// stdout: `date` (granted only time) renders the wall clock, and `cat` (granted only storage)
// prints a file. Each reaches exactly its manifest's capabilities and nothing else. The
// manager relays each component's proof and decisions summary, and each tool's printed
// output, to the supervisor, then serves the Permission contract until the supervisor drops
// its bootstrap channel.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::system::permission::{self, Service};
use proto::system::{process, AuditEntry, Capability, Error, Manifest, StartResult};
use rt::*;

// The governed component this milestone launches, and the rights a granted client is
// duplicated with before it is transferred (send + receive + wait + transfer onward - the
// set a service client needs, never more than the manager itself holds).
const PROBE_NAME: &[u8] = b"sandbox_probe";
// One of the system tools the manager launches on demand through the `run` op (the launcher
// / granter path): the `date` command run as its own sandboxed ELF, which renders the wall
// clock to a captured stdout; its manifest grants it exactly one capability (time).
const DATE_NAME: &[u8] = b"date";
// The governed component that exercises the dynamic permission-request path: its manifest
// grants only log, and at runtime it asks for an undeclared capability (storage) to prove
// the headless policy refuses any escalation beyond the manifest.
const REQUEST_NAME: &[u8] = b"request_probe";
// Another system tool launched on demand through the `run` op: the `cat` command run as its
// own sandboxed ELF, which prints a file to a captured stdout; its manifest grants it exactly
// one capability (storage).
const CAT_NAME: &[u8] = b"cat";
const GRANT_RIGHTS: u32 = RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER;

// A system tool launched through `run` receives, before its manifest grants, the caller's
// stdout console (so its `print` output renders on the launching terminal) under this tag,
// then its argument string.
const STDOUT_TAG: &[u8] = b"STDOUT";

// A runtime permission request rides a launched component's bootstrap channel as this tag
// followed by the requested capability's ordinal byte; the manager replies with the granted
// client tagged under the capability, or a bare `DENY` (no handle) if the policy refuses.
const REQUEST_TAG: &[u8] = b"REQUEST";
const DENY_REPLY: &[u8] = b"DENY";

// The full grantable vocabulary, in the fixed order the manager evaluates a manifest: for
// each, it grants the held client if the manifest lists the capability, or records a denial
// if not. This is also the order a launched component receives its grants in. The store
// names every system service a component may be declared to reach; the manager holds a live
// client only for the ones the supervisor wired it (the rest stay 0 - declared in the
// vocabulary, not yet grantable - so a manifest naming them records the decision but hands
// over nothing).
const VOCABULARY: [Capability; 14] = [Capability::Storage, Capability::Log, Capability::Network, Capability::Device, Capability::Config, Capability::Time, Capability::Audio, Capability::Input, Capability::Graph, Capability::Resource, Capability::Process, Capability::Permission, Capability::Supervisor, Capability::Volumes];

// The manager's policy: the permission manifest declared for each component it governs -
// the typed source of truth for what that component may be granted.
fn manifest_for(component: &[u8]) -> Option<Manifest> {
	match component {
		b"sandbox_probe" => Some(Manifest { component: String::from("sandbox_probe"), grants: alloc::vec![Capability::Storage, Capability::Log] }),
		b"date" => Some(Manifest { component: String::from("date"), grants: alloc::vec![Capability::Time] }),
		b"request_probe" => Some(Manifest { component: String::from("request_probe"), grants: alloc::vec![Capability::Log] }),
		b"cat" => Some(Manifest { component: String::from("cat"), grants: alloc::vec![Capability::Storage] }),
		b"write" => Some(Manifest { component: String::from("write"), grants: alloc::vec![Capability::Storage] }),
		b"rm" => Some(Manifest { component: String::from("rm"), grants: alloc::vec![Capability::Storage] }),
		b"ls" => Some(Manifest { component: String::from("ls"), grants: alloc::vec![Capability::Storage] }),
		b"mkdir" => Some(Manifest { component: String::from("mkdir"), grants: alloc::vec![Capability::Storage] }),
		b"rmdir" => Some(Manifest { component: String::from("rmdir"), grants: alloc::vec![Capability::Storage] }),
		b"log" => Some(Manifest { component: String::from("log"), grants: alloc::vec![Capability::Log, Capability::Time] }),
		b"snap" => Some(Manifest { component: String::from("snap"), grants: alloc::vec![Capability::Storage] }),
		b"dev" => Some(Manifest { component: String::from("dev"), grants: alloc::vec![Capability::Device] }),
		b"config" => Some(Manifest { component: String::from("config"), grants: alloc::vec![Capability::Config] }),
		b"set" => Some(Manifest { component: String::from("set"), grants: alloc::vec![Capability::Config] }),
		b"beep" => Some(Manifest { component: String::from("beep"), grants: alloc::vec![Capability::Audio] }),
		b"usage" => Some(Manifest { component: String::from("usage"), grants: alloc::vec![Capability::Resource] }),
		b"ps" => Some(Manifest { component: String::from("ps"), grants: alloc::vec![Capability::Process] }),
		b"run" => Some(Manifest { component: String::from("run"), grants: alloc::vec![Capability::Process] }),
		b"perm" => Some(Manifest { component: String::from("perm"), grants: alloc::vec![Capability::Permission] }),
		b"stop" => Some(Manifest { component: String::from("stop"), grants: alloc::vec![Capability::Supervisor] }),
		b"lsvol" => Some(Manifest { component: String::from("lsvol"), grants: alloc::vec![Capability::Volumes] }),
		_ => None,
	}
}

// The non-interactive (headless) policy default for a runtime permission request: a
// capability a component did not pre-declare in its manifest. An appliance has no human to
// approve such a request, so least privilege applies and it is refused - a component can
// never gain authority its manifest did not declare. (The interactive approval path for
// later untrusted apps replaces this one hook; the request is recorded either way.)
fn dynamic_policy(_component: &[u8], _cap: Capability) -> bool {
	false
}

// Parse a runtime permission request off a component's bootstrap channel: `REQUEST` + the
// requested capability's ordinal byte. Returns the capability if the message is a request,
// or None if it is the component's final report (any other message).
fn parse_request(msg: &[u8]) -> Option<Capability> {
	if msg.len() == REQUEST_TAG.len() + 1 && &msg[..REQUEST_TAG.len()] == REQUEST_TAG {
		return Capability::decode(&msg[REQUEST_TAG.len()..]);
	}
	None
}

// The bootstrap tag a granted capability's client is transferred under - matched by the
// launched component's receive order.
fn tag_for(cap: Capability) -> &'static [u8] {
	match cap {
		Capability::Log => b"LOG",
		Capability::Storage => b"STORAGE",
		Capability::Network => b"NETWORK",
		Capability::Device => b"DEVICE",
		Capability::Config => b"CONFIG",
		Capability::Time => b"TIME",
		Capability::Audio => b"AUDIO",
		Capability::Input => b"INPUT",
		Capability::Graph => b"GRAPH",
		Capability::Resource => b"RESOURCE",
		Capability::Process => b"PROCESS",
		Capability::Permission => b"PERMISSION",
		Capability::Supervisor => b"SUPERVISOR",
		// The `volumes` capability bundles four channels; the grant hands them over under their
		// own per-volume tags (see `grant_volumes`), so this single tag is never sent - it only
		// keeps the match total for the bundling capability.
		Capability::Volumes => b"VOLUMES",
	}
}

// The grantable clients the manager holds and may hand onward (0 = not granted to it).
struct Clients {
	log: u64,
	storage: u64,
	network: u64,
	device: u64,
	config: u64,
	time: u64,
	audio: u64,
	input: u64,
	graph: u64,
	resource: u64,
	process: u64,
	permission: u64,
	supervisor: u64,
	// The three non-system volume StorageService clients, bundled with `storage` (the system
	// volume) under the `volumes` capability for the `lsvol` overview.
	storage_media: u64,
	storage_iso: u64,
	storage_udf: u64,
}

impl Clients {
	// The held client for a grantable capability.
	fn for_capability(&self, cap: Capability) -> u64 {
		match cap {
			Capability::Log => self.log,
			Capability::Storage => self.storage,
			Capability::Network => self.network,
			Capability::Device => self.device,
			Capability::Config => self.config,
			Capability::Time => self.time,
			Capability::Audio => self.audio,
			Capability::Input => self.input,
			Capability::Graph => self.graph,
			Capability::Resource => self.resource,
			Capability::Process => self.process,
			Capability::Permission => self.permission,
			Capability::Supervisor => self.supervisor,
			// The `volumes` capability has no single representative client - it is granted as a
			// bundle of four channels by `grant_volumes`, never through this single-channel path.
			// The system volume stands in here for the (headless-denied) dynamic-request path.
			Capability::Volumes => self.storage,
		}
	}
}

// The manager's serve state. The manifest table is fixed policy (served read-only by
// `lookup`); the audit trail is the mutable record of every grant decision made. It also
// holds the ProcessService client it drives to load tools and the grantable clients it may
// hand on, so the `run` op can launch a named tool under its manifest on demand.
struct Manager {
	audit: Vec<AuditEntry>,
	procsvc: u64,
	clients: Clients,
}

impl Service for Manager {
	fn lookup(&mut self, component: String) -> Result<Manifest, Error> {
		manifest_for(component.as_bytes()).ok_or(Error::NotFound)
	}
	fn audit(&mut self) -> Result<Vec<AuditEntry>, Error> {
		Ok(self.audit.clone())
	}
	fn run(&mut self, name: String, args: String, cwd: String, stdout: u64) -> Result<StartResult, Error> {
		match unsafe { run_tool_under_manifest(self.procsvc, name.as_bytes(), args.as_bytes(), cwd.as_bytes(), stdout, &self.clients, &mut self.audit) } {
			Some(started) => Ok(started),
			None => Err(Error::NotFound),
		}
	}
}

// Launch a component under its permission manifest: ask ProcessService (the loading
// mechanism) to start it with a fresh bootstrap channel, then for every capability in the
// vocabulary grant the held client if the manifest lists it (recording the grant) or
// withhold it (recording the denial). The component receives exactly its manifest's
// capabilities, in vocabulary order, and can reach nothing else - the sandbox. After the
// static grants it may make runtime permission requests for undeclared capabilities; each
// is decided by the headless policy default and recorded in the same audit trail as a
// dynamic decision. Returns the bytes the component reported back (its proof the granted
// capabilities are live), or None if the launch failed.
unsafe fn launch_under_manifest(procsvc: u64, component: &[u8], clients: &Clients, audit: &mut Vec<AuditEntry>, buf: &mut [u8]) -> Option<Vec<u8>> {
	unsafe {
		let manifest: Manifest = manifest_for(component)?;
		let (manager_side, child_side): (u64, u64) = channel()?;
		// Hand the child end to ProcessService, which loads the component and starts it with
		// that end as its bootstrap; the manager keeps `manager_side` to grant over. The
		// returned process handle is the manager's job-control handle on the component.
		let name: String = String::from_utf8_lossy(component).into_owned();
		let mut process_client = process::Client::new(ChannelTransport { chan: procsvc });
		let task: u64 = match process_client.launch(&name, &child_side) {
			Some(Ok(started)) => started.task,
			_ => {
				close(manager_side);
				return None;
			}
		};
		// Grant exactly the manifest's capabilities, auditing every decision. A granted
		// client is duplicated (the manager keeps its own) with only the rights a client
		// needs, then transferred under its tag; a withheld capability is recorded denied
		// and simply never handed over - so the component cannot reach it.
		for &cap in VOCABULARY.iter() {
			let granted: bool = manifest.grants.contains(&cap);
			if granted {
				let dup: i64 = duplicate(clients.for_capability(cap), GRANT_RIGHTS);
				if dup < 0 || !send_blocking(manager_side, tag_for(cap), dup as u64) {
					close(manager_side);
					close(task);
					return None;
				}
			}
			audit.push(AuditEntry { component: String::from_utf8_lossy(component).into_owned(), capability: cap, granted, dynamic: false });
		}
		// Handle any runtime permission requests, then capture the component's final report. A
		// request is `REQUEST` + a capability ordinal for a capability outside the manifest;
		// the headless policy default decides it (recorded as a dynamic audit entry), and the
		// manager replies with the granted client or a bare DENY. Any other message is the
		// component's final report - the bytes it produced through its grants - and ends the
		// launch.
		let result: Option<Vec<u8>> = loop {
			match recv_blocking(manager_side, buf) {
				Received::Message { len, .. } => {
					if let Some(cap) = parse_request(&buf[..len]) {
						let granted: bool = grant_dynamic(component, cap, clients, manager_side);
						audit.push(AuditEntry { component: String::from_utf8_lossy(component).into_owned(), capability: cap, granted, dynamic: true });
						continue;
					}
					break Some(buf[..len].to_vec());
				}
				Received::Closed => break None,
			}
		};
		close(manager_side);
		close(task);
		result
	}
}

// Decide and act on one runtime permission request: apply the headless policy default and,
// if it allows the request and the manager actually holds the capability, duplicate that
// client (with only the rights a client needs) and transfer it under its tag; otherwise
// reply with a bare DENY (no handle). Returns whether the capability was handed over.
unsafe fn grant_dynamic(component: &[u8], cap: Capability, clients: &Clients, manager_side: u64) -> bool {
	unsafe {
		if dynamic_policy(component, cap) {
			let dup: i64 = duplicate(clients.for_capability(cap), GRANT_RIGHTS);
			if dup >= 0 && send_blocking(manager_side, tag_for(cap), dup as u64) {
				return true;
			}
		}
		send_blocking(manager_side, DENY_REPLY, 0);
		false
	}
}

// Run a named system tool on demand under its permission manifest - the launcher / granter
// primitive behind the `run` op. Unlike a governed component (which reports back over its
// bootstrap), a tool prints to the caller's terminal and exits: ask ProcessService to start
// it with a fresh bootstrap channel, forward the caller's stdout console first (so the
// tool's `inherit_stdout` adopts it) then its argument string, and finally grant exactly the
// manifest's capabilities in vocabulary order (auditing each decision). Returns the live
// process handle (for the caller's job control) and the per-capability decisions, or None if
// the tool has no manifest, the argument is not a known program name, or the launch fails.
unsafe fn run_tool_under_manifest(procsvc: u64, name: &[u8], args: &[u8], cwd: &[u8], stdout: u64, clients: &Clients, audit: &mut Vec<AuditEntry>) -> Option<StartResult> {
	unsafe {
		let manifest: Manifest = manifest_for(name)?;
		let name_str: &str = core::str::from_utf8(name).ok()?;
		let (manager_side, child_side): (u64, u64) = channel()?;
		let mut process_client = process::Client::new(ChannelTransport { chan: procsvc });
		let started: StartResult = match process_client.launch(name_str, &child_side) {
			Some(Ok(s)) => s,
			_ => {
				close(manager_side);
				return None;
			}
		};
		// Forward the stdout console first (the tool's `inherit_stdout` reads the first
		// message), then the argument string, then the manifest grants.
		send_blocking(manager_side, STDOUT_TAG, stdout);
		send_blocking(manager_side, args, 0);
		for &cap in VOCABULARY.iter() {
			let granted: bool = manifest.grants.contains(&cap);
			if granted {
				// Most capabilities are a single channel: duplicate the held client (narrowed)
				// and transfer it under its tag. The `volumes` capability instead bundles the
				// four volume StorageService clients, handed over under their own per-volume
				// tags by `grant_volumes`.
				let ok: bool = if cap == Capability::Volumes {
					grant_volumes(manager_side, clients)
				} else {
					let dup: i64 = duplicate(clients.for_capability(cap), GRANT_RIGHTS);
					dup >= 0 && send_blocking(manager_side, tag_for(cap), dup as u64)
				};
				if !ok {
					close(manager_side);
					return None;
				}
			}
			audit.push(AuditEntry { component: String::from_utf8_lossy(name).into_owned(), capability: cap, granted, dynamic: false });
		}
		// Hand over the inherited working directory last, after the capability grants. It is
		// plain data (no handle), so a tool resolves a relative path argument against it; a
		// tool that takes no path simply never reads it, leaving it a harmless trailing
		// message - sending it before the tagged grants would instead be mis-consumed by the
		// tool's `recv_tagged` for its capabilities.
		send_blocking(manager_side, cwd, 0);
		close(manager_side);
		Some(started)
	}
}

// Grant the four volume StorageService clients the `volumes` capability bundles, in a fixed
// order under their own per-volume tags: the system (writable LiberFS), media (FAT/exFAT),
// iso (ISO9660), and udf (UDF) volumes. Each held client is duplicated (narrowed to a client's
// rights, the manager keeping its own) and transferred; a volume whose disk is absent is held
// as 0 and handed over as a tagged message with no handle, which `lsvol` reads as zero files -
// so the grant always sends exactly four messages and the receiver's order stays aligned.
// Returns false only if a transfer itself fails.
unsafe fn grant_volumes(manager_side: u64, clients: &Clients) -> bool {
	unsafe {
		let volumes: [(&[u8], u64); 4] = [(b"SYSTEM", clients.storage), (b"MEDIA", clients.storage_media), (b"ISO", clients.storage_iso), (b"UDF", clients.storage_udf)];
		for &(tag, client) in volumes.iter() {
			let dup: i64 = duplicate(client, GRANT_RIGHTS);
			let handle: u64 = if dup >= 0 { dup as u64 } else { 0 };
			if !send_blocking(manager_side, tag, handle) {
				return false;
			}
		}
		true
	}
}

// Demonstrate the on-demand tool launcher (the `run` op's mechanism) at startup: stand in
// for the shell by handing the tool a captured stdout console, run it under its manifest,
// and read back what it printed - proof the tool reached its one granted capability and that
// its output was forwarded to the caller's terminal. The shell reaches this same path live
// over the `run` op; here the manager plays both launcher and terminal so the path is
// exercised end to end. Returns the bytes the tool printed, or empty if it could not start.
unsafe fn demonstrate_tool(procsvc: u64, name: &[u8], args: &[u8], clients: &Clients, audit: &mut Vec<AuditEntry>, buf: &mut [u8]) -> Vec<u8> {
	unsafe {
		let (output, console): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return Vec::new(),
		};
		let started: StartResult = match run_tool_under_manifest(procsvc, name, args, b"", console, clients, audit) {
			Some(s) => s,
			None => {
				close(output);
				return Vec::new();
			}
		};
		let printed: Vec<u8> = match recv_blocking(output, buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => Vec::new(),
		};
		close(output);
		close(started.task);
		printed
	}
}

// Build the human-readable decisions summary for one launched component from the audit
// trail - one `cap=grant` or `cap=deny` token per recorded decision for that component, in
// order; a runtime (dynamic) request is marked with a trailing `(dynamic)`. The supervisor
// relays this as the manager's proof of exactly which capabilities that component was and
// was not given; the typed trail itself is served verbatim over the Permission contract.
fn summarize_for(audit: &[AuditEntry], component: &[u8]) -> Vec<u8> {
	let mut out: String = String::new();
	for e in audit.iter().filter(|e: &&AuditEntry| e.component.as_bytes() == component) {
		if !out.is_empty() {
			out.push(' ');
		}
		out.push_str(&e.capability.to_text());
		out.push('=');
		out.push_str(if e.granted { "grant" } else { "deny" });
		if e.dynamic {
			out.push_str("(dynamic)");
		}
	}
	out.into_bytes()
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 512] = [0u8; 512];

	// 1. receive the grantable clients the manager may hand onward, then the ProcessService
	//    client it drives to load the components it governs. A client the supervisor does not
	//    grant arrives as 0 (the manager simply cannot grant what it does not hold). Storage,
	//    log, network, time, config, device, audio, resource, process, and supervisor are wired
	//    (time so the governed `date` command can read the wall clock, config/device/audio/resource
	//    so the governed `config` / `set`, `dev`, `beep`, and `usage` commands can reach their one
	//    service, process so the governed `ps` / `run` commands can list / start processes - a
	//    dedicated ProcessService connection, kept separate from the launch mechanism below -, and
	//    supervisor so the governed `stop` command can drive the supervisor's teardown path over a
	//    dedicated ServiceManager admin channel); the permission capability is not received but
	//    minted locally below (a self-connection to the manager's own serve channel); the remaining
	//    vocabulary capabilities (input, graph) are declared in the store but not wired - held 0, so
	//    a manifest naming one records the decision yet hands over nothing (input / graph are
	//    single-client and cannot be proxied at all).
	let storage: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"STORAGE") }.unwrap_or(0);
	let log: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"LOG") }.unwrap_or(0);
	let network: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"NETWORK") }.unwrap_or(0);
	let time: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"TIME") }.unwrap_or(0);
	let config: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"CONFIG") }.unwrap_or(0);
	let device: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"DEVICE") }.unwrap_or(0);
	let audio: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"AUDIO") }.unwrap_or(0);
	let resource: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"RESOURCE") }.unwrap_or(0);
	let process: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"PROCESS_GRANT") }.unwrap_or(0);
	// The admin channel the manager grants to the governed `stop` command (whose manifest
	// grants supervisor): a dedicated ServiceManager admin channel, separate from the shell's,
	// the manager holds but never drives itself - it only duplicates a narrowed copy onto the
	// sandboxed `stop` tool, which speaks the bare request/reply teardown protocol over it.
	let supervisor: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"SUPERVISOR") }.unwrap_or(0);
	// The three non-system volume StorageService clients the supervisor connects for the
	// manager, bundled with the system `storage` client under the `volumes` capability the
	// governed `lsvol` command is granted: media (FAT/exFAT), iso (ISO9660), udf (UDF). A
	// volume whose disk is absent arrives as 0 (the manager simply cannot grant what it does
	// not hold), and `lsvol` shows it as zero files.
	let storage_media: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"STORAGE_MEDIA") }.unwrap_or(0);
	let storage_iso: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"STORAGE_ISO") }.unwrap_or(0);
	let storage_udf: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"STORAGE_UDF") }.unwrap_or(0);
	// Mint the manager's self-connection: a dedicated channel pair whose server end is seeded
	// into the serve set below (so requests on it are dispatched like any other client's) and
	// whose client end the manager holds as the grantable `permission` capability. The governed
	// `perm` command thus reaches the very audit trail this manager serves over a connection of
	// its own - a capability the manager grants to a copy of itself, on a dedicated channel so a
	// granted tool's queries never race the supervisor's own connection.
	let (perm_self_server, perm_self_client): (u64, u64) = unsafe { channel() }.unwrap_or_else(|| exit());
	let clients: Clients = Clients { log, storage, network, time, config, device, audio, input: 0, graph: 0, resource, process, permission: perm_self_client, supervisor, storage_media, storage_iso, storage_udf };
	let procsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"PROCESS") }.unwrap_or_else(|| exit());

	// 2. wait for the serve channel clients reach us on.
	let service: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"SERVE") }.unwrap_or_else(|| exit());

	// 3. launch each governed component under its manifest, accumulating one shared audit
	//    trail: sandbox_probe (granted storage + log, denied the rest) reads its one file and
	//    reports the bytes back; `date` (granted only time) is launched on demand through the
	//    `run` op and renders the wall clock to a captured stdout; request_probe (granted only
	//    log) asks for an undeclared capability at runtime to exercise the dynamic-request
	//    path; and `cat` (granted only storage) is likewise launched through `run`, printing a
	//    file to a captured stdout.
	let mut audit: Vec<AuditEntry> = Vec::new();
	let probe_read: Vec<u8> = unsafe { launch_under_manifest(procsvc, PROBE_NAME, &clients, &mut audit, &mut buf) }.unwrap_or_default();
	let date_read: Vec<u8> = unsafe { demonstrate_tool(procsvc, DATE_NAME, b"", &clients, &mut audit, &mut buf) };
	let request_read: Vec<u8> = unsafe { launch_under_manifest(procsvc, REQUEST_NAME, &clients, &mut audit, &mut buf) }.unwrap_or_default();
	let cat_read: Vec<u8> = unsafe { demonstrate_tool(procsvc, CAT_NAME, b"vol://system/hello.txt", &clients, &mut audit, &mut buf) };

	// 4. report in to the supervisor, then relay each governed component's proof and its
	//    decisions summary (exactly which capabilities it was and was not given): the bytes
	//    sandbox_probe read through its storage grant, the instant `date` printed through its
	//    time grant to a captured stdout, request_probe's verdict on its runtime request for an
	//    undeclared capability (its summary marks that refused request as a dynamic decision),
	//    then the bytes the on-demand `cat` tool printed through its storage grant to the
	//    forwarded stdout.
	unsafe {
		send_blocking(bootstrap, b"PermissionManager: online", 0);
		send_blocking(bootstrap, &probe_read, 0);
		send_blocking(bootstrap, &summarize_for(&audit, PROBE_NAME), 0);
		send_blocking(bootstrap, &date_read, 0);
		send_blocking(bootstrap, &summarize_for(&audit, DATE_NAME), 0);
		send_blocking(bootstrap, &request_read, 0);
		send_blocking(bootstrap, &summarize_for(&audit, REQUEST_NAME), 0);
		send_blocking(bootstrap, &cat_read, 0);
	}

	// 5. serve generated lookup/audit/run requests until the supervisor drops the channel. The
	//    self-connection's server end is seeded into the client set alongside the root, so the
	//    governed `perm` command - granted the matching client end - is served like any other.
	let mut manager: Manager = Manager { audit, procsvc, clients };
	let mut request: [u8; 512] = [0u8; 512];
	let mut reply: [u8; 1024] = [0u8; 1024];
	unsafe {
		serve_multi_seeded(service, &[perm_self_server], &mut request, &mut reply, |_chan: u64, req: &[u8], handle: u64, out: &mut [u8], reply_handle: &mut u64| -> Option<usize> { permission::dispatch(&mut manager, req, handle, out, reply_handle) });
	}
	exit();
}
