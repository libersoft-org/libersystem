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
// Currently it governs four components. Two are report-back probes that prove the grant
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
use ipc_client::ChannelTransport;
use proto::codec::Handles;
use proto::system::audio_admin;
use proto::system::display_admin;
use proto::system::input_admin;
use proto::system::network;
use proto::system::permission::{self, Service};
use proto::system::{AuditEntry, Capability, EnvVar, Error, LaunchContext, Manifest, PipelineResult, PipelineStage, StartResult, process, volume};
use rt::*;
use services::executable;

// The governed component the manager launches, and the rights a granted client is
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
// The network-wave representative: it receives exactly NetworkService, queries the typed
// interface state and renders it to the caller's stdout.
const IP_NAME: &[u8] = b"ip";
const GRANT_RIGHTS: u32 = RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER;

// A system tool launched through `run` receives, before its manifest grants, its launch
// endpoints as a named run (the caller's console, so its `print` output renders on the launching
// terminal), then its launch context.

// What this is sized for, stated rather than left to be inferred from one number: converting
// the largest image the system ships - `wallpapers/logo.webp`, 3840x2160 - which needs the
// decoded input and the output RGBA live at the same time, about 33 MB each, plus codec
// working memory and the allocator's chunk granularity. Measured whole-Domain peak for that
// conversion: 109,830,144 bytes (`imgconv_governed_working_set_is_measured`).
//
// It was 96 MiB against a measured 84,475,904, and that measurement was a 4K conversion
// upscaled from a 2x2 BMP - an output with no input to decode, so it never showed the term
// that dominates a real conversion. The limit then refused the wallpaper the system installs,
// which is the failure mode of a budget sized from a single unrepresentative sample: it
// rejects whatever is bigger than that sample, for reasons nobody can predict from the file.
const IMGCONV_MEMORY_LIMIT: u64 = 128 * 1024 * 1024;

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
const VOCABULARY: [Capability; 19] = [
	Capability::Storage,
	Capability::Log,
	Capability::Network,
	Capability::Device,
	Capability::Config,
	Capability::Time,
	Capability::Audio,
	Capability::Input,
	Capability::Graph,
	Capability::Resource,
	Capability::Process,
	Capability::Permission,
	Capability::Supervisor,
	Capability::Volumes,
	Capability::Services,
	Capability::Usb,
	Capability::Display,
	Capability::InputKeys,
	Capability::AudioStream,
];

// A store row where the policy allows everything the component requests - the
// common case for the curated first-party tools, whose requests were written
// against exactly what they need.
fn granted(component: &str, caps: Vec<Capability>) -> Manifest {
	Manifest { component: String::from(component), requested: caps.clone(), grants: caps }
}

// A store row where the component requests more than the policy allows: the
// grants are the audited intersection, and the withheld remainder surfaces as a
// denial in the launch audit. This is the requested-vs-granted split the packaged
// form ships: the package declares `requested`, the manager decides.
fn intersected(component: &str, requested: Vec<Capability>, allowed: &[Capability]) -> Manifest {
	let grants: Vec<Capability> = requested.iter().copied().filter(|cap: &Capability| allowed.contains(cap)).collect();
	Manifest { component: String::from(component), requested, grants }
}

// The manager's policy: the permission manifest declared for each component it governs -
// the typed source of truth for what that component may be granted.
fn manifest_for(component: &[u8]) -> Option<Manifest> {
	match component {
		// sandbox_probe requests network on top of storage + log, and the policy
		// withholds it: the granted set is the intersection, the audit records the
		// denial, and the probe proves the sandbox holds exactly the grants.
		b"sandbox_probe" => Some(intersected("sandbox_probe", alloc::vec![Capability::Storage, Capability::Log, Capability::Network], &[Capability::Storage, Capability::Log])),
		b"date" => Some(granted("date", alloc::vec![Capability::Time])),
		b"request_probe" => Some(granted("request_probe", alloc::vec![Capability::Log])),
		// `echo` and `readln` need no capability at all: they only read and write the stdio
		// the launch itself hands them, which is not a manifest grant. They still need an
		// entry, because a tool with no manifest is refused - a governed launch never runs a
		// component whose permissions were never declared, and "declared as needing nothing"
		// has to be sayable.
		b"echo" => Some(granted("echo", alloc::vec![])),
		b"readln" => Some(granted("readln", alloc::vec![])),
		b"cat" => Some(granted("cat", alloc::vec![Capability::Volumes])),
		// The two halves of shell redirection, granted exactly what a redirection is: the volumes,
		// and nothing else. The command being redirected gets neither - it receives one stream
		// endpoint, which is the whole point of expanding `cmd < a > b` into a pipeline rather than
		// opening files inside the shell and handing the child a file capability.
		b"redirect_in" => Some(granted("redirect_in", alloc::vec![Capability::Volumes])),
		b"redirect_out" => Some(granted("redirect_out", alloc::vec![Capability::Volumes])),
		// `tee` writes files as well as passing bytes on, so it holds exactly what a redirection
		// holds and nothing more - it is a redirection that also has a stdout.
		b"tee" => Some(granted("tee", alloc::vec![Capability::Volumes])),
		b"write" => Some(granted("write", alloc::vec![Capability::Volumes])),
		b"rm" => Some(granted("rm", alloc::vec![Capability::Volumes])),
		b"pwd" => Some(granted("pwd", alloc::vec![])),
		b"kill" => Some(granted("kill", alloc::vec![Capability::Session])),
		b"sort" => Some(granted("sort", alloc::vec![Capability::Volumes])),
		b"cut" => Some(granted("cut", alloc::vec![Capability::Volumes])),
		b"tree" => Some(granted("tree", alloc::vec![Capability::Volumes])),
		b"find" => Some(granted("find", alloc::vec![Capability::Volumes])),
		b"grep" => Some(granted("grep", alloc::vec![Capability::Volumes])),
		b"cp" => Some(granted("cp", alloc::vec![Capability::Volumes])),
		b"mv" => Some(granted("mv", alloc::vec![Capability::Volumes])),
		b"clear" => Some(granted("clear", alloc::vec![])),
		b"which" => Some(granted("which", alloc::vec![Capability::Volumes])),
		b"wc" => Some(granted("wc", alloc::vec![Capability::Volumes])),
		b"head" => Some(granted("head", alloc::vec![Capability::Volumes])),
		b"tail" => Some(granted("tail", alloc::vec![Capability::Volumes])),
		b"hexdump" => Some(granted("hexdump", alloc::vec![Capability::Volumes])),
		b"truncate" => Some(granted("truncate", alloc::vec![Capability::Volumes])),
		b"touch" => Some(granted("touch", alloc::vec![Capability::Volumes, Capability::Time])),
		b"ls" => Some(granted("ls", alloc::vec![Capability::Volumes])),
		b"du" => Some(granted("du", alloc::vec![Capability::Volumes])),
		b"mkdir" => Some(granted("mkdir", alloc::vec![Capability::Volumes])),
		b"rmdir" => Some(granted("rmdir", alloc::vec![Capability::Volumes])),
		b"log" => Some(granted("log", alloc::vec![Capability::Log, Capability::Time])),
		b"snap" => Some(granted("snap", alloc::vec![Capability::Storage])),
		b"volume" => Some(granted("volume", alloc::vec![Capability::Storage])),
		b"lsdev" => Some(granted("lsdev", alloc::vec![Capability::Device])),
		b"config" => Some(granted("config", alloc::vec![Capability::Config])),
		b"set" => Some(granted("set", alloc::vec![Capability::Config])),
		b"beep" => Some(granted("beep", alloc::vec![Capability::Audio])),
		b"imgview" => Some(granted("imgview", alloc::vec![Capability::Volumes, Capability::Display, Capability::InputKeys])),
		b"licoview" => Some(granted("licoview", alloc::vec![Capability::Volumes])),
		b"licoedit" => Some(granted("licoedit", alloc::vec![Capability::Volumes])),
		b"lico" => Some(granted("lico", alloc::vec![Capability::Volumes])),
		b"imgconv" => Some(granted("imgconv", alloc::vec![Capability::Volumes])),
		b"play" => Some(granted("play", alloc::vec![Capability::Volumes, Capability::AudioStream])),
		b"graphics_probe" => Some(granted("graphics_probe", alloc::vec![Capability::Display, Capability::InputKeys, Capability::AudioStream])),
		b"usage" => Some(granted("usage", alloc::vec![Capability::Resource])),
		b"ps" => Some(granted("ps", alloc::vec![Capability::Resource, Capability::Process])),
		b"run" => Some(granted("run", alloc::vec![Capability::Process])),
		b"perm" => Some(granted("perm", alloc::vec![Capability::Permission])),
		// `watch` launches the command it watches through the manager, so it holds a manager client
		// and NOTHING ELSE. That is what makes "watching a command lends it no authority" true by
		// construction rather than by intention: the child is started under its own manifest, and
		// `watch` has nothing of its own to lend it.
		b"watch" => Some(granted("watch", alloc::vec![Capability::Permission])),
		b"stop" => Some(granted("stop", alloc::vec![Capability::Supervisor])),
		// The inverse of `stop`, and granted exactly what it is granted: one admin channel.
		b"start" => Some(granted("start", alloc::vec![Capability::Supervisor])),
		b"lsvol" => Some(granted("lsvol", alloc::vec![Capability::Volumes])),
		b"lssvc" => Some(granted("lssvc", alloc::vec![Capability::Services])),
		b"lsblk" => Some(granted("lsblk", alloc::vec![Capability::Volumes])),
		b"lsusb" => Some(granted("lsusb", alloc::vec![Capability::Usb])),
		b"ping" => Some(granted("ping", alloc::vec![Capability::Network])),
		b"ip" => Some(granted("ip", alloc::vec![Capability::Network])),
		b"nslookup" => Some(granted("nslookup", alloc::vec![Capability::Network])),
		b"tcp" => Some(granted("tcp", alloc::vec![Capability::Network])),
		b"nc" => Some(granted("nc", alloc::vec![Capability::Network])),
		b"arp" => Some(granted("arp", alloc::vec![Capability::Network])),
		b"httpd" => Some(granted("httpd", alloc::vec![Capability::Network])),
		b"ss" => Some(granted("ss", alloc::vec![Capability::Network])),
		// The inventory commands need no capability at all: the system identity and the
		// uptime are compile-time / free-syscall data, and the boot log, CPU set, memory
		// totals, memory map and vector table are read over their own free syscalls -
		// the emptiest manifests in the store.
		b"uname" => Some(granted("uname", alloc::vec![])),
		b"uptime" => Some(granted("uptime", alloc::vec![])),
		b"dmesg" => Some(granted("dmesg", alloc::vec![])),
		b"lscpu" => Some(granted("lscpu", alloc::vec![])),
		b"free" => Some(granted("free", alloc::vec![])),
		b"lsmem" => Some(granted("lsmem", alloc::vec![])),
		b"lsirq" => Some(granted("lsirq", alloc::vec![])),
		b"lspci" => Some(granted("lspci", alloc::vec![])),
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
		// The `volumes` capability bundles five channels; the grant hands them over under their
		// own per-volume tags (see `grant_volumes`), so this single tag is never sent - it only
		// keeps the match total for the bundling capability.
		Capability::Volumes => b"VOLUMES",
		Capability::Services => b"SERVICES",
		Capability::Usb => b"USB",
		Capability::Display => b"DISPLAY",
		Capability::InputKeys => b"INPUT_KEYS",
		Capability::AudioStream => b"AUDIO_STREAM",
		Capability::Session => b"SESSION",
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
	// The broker (bootstrap) channel the re-resolvable capabilities are re-resolved
	// over when their held client dies: config and device restart transparently
	// (ServiceManager relaunches them and answers a RESOLVE with a connection to the
	// live instance), so their grants must survive the crash of the instance the held
	// client points at.
	broker: u64,
	// The supervisor-status client bundled under the `services` capability for the `lssvc`
	// overview - a dedicated ServiceManager status channel, separate from the graph's.
	services: u64,
	// The xHCI driver's USB bus query client, granted under the `usb` capability for the
	// `lsusb` overview (0 when the driver never came up).
	usb: u64,
	// The four non-system volume StorageService clients, bundled with `storage` (the system
	// volume) under the `volumes` capability for the `lsvol` overview.
	storage_media: u64,
	storage_iso: u64,
	storage_udf: u64,
	storage_usb: u64,
	// The two memory volumes. Held like every other volume client; the bundle below sends them
	// under their own tags so the receiver's fixed order stays aligned.
	storage_ram: u64,
	storage_tmp: u64,
	display_admin: u64,
	input_admin: u64,
	audio_admin: u64,
	// VT 1's SessionService client, granted to the governed `kill` command so it can ask the
	// session to signal a job without ever holding the job's Process handle.
	session: u64,
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
			Capability::Session => self.session,
			Capability::Audio => self.audio,
			Capability::Input => self.input,
			Capability::Graph => self.graph,
			Capability::Resource => self.resource,
			Capability::Process => self.process,
			Capability::Permission => self.permission,
			Capability::Supervisor => self.supervisor,
			Capability::Services => self.services,
			Capability::Usb => self.usb,
			Capability::Display | Capability::InputKeys | Capability::AudioStream => 0,
			// The `volumes` capability has no single representative client - it is granted as a
			// bundle of five channels by `grant_volumes`, never through this single-channel path.
			// The system volume stands in here for the (headless-denied) dynamic-request path.
			Capability::Volumes => self.storage,
		}
	}
}

// Mint a launch-scoped capability. Display binding consumes a duplicate of the exact
// task ProcessService just returned and atomically returns its associated connection;
// input/audio admins mint connections narrowed to their advertised operation subset.
unsafe fn grant_for_task(clients: &mut Clients, cap: Capability, task: u64) -> u64 {
	unsafe {
		match cap {
			Capability::Display => {
				if clients.display_admin == 0 {
					return 0;
				}
				let bound_task: i64 = duplicate(task, RIGHT_MANAGE | RIGHT_TRANSFER);
				if bound_task < 0 {
					return 0;
				}
				match display_admin::Client::new(ChannelTransport { chan: clients.display_admin }).bind(&(bound_task as u64)) {
					Some(Ok(display)) => display,
					_ => {
						close(bound_task as u64);
						0
					}
				}
			}
			Capability::InputKeys => {
				if clients.input_admin == 0 {
					return 0;
				}
				match input_admin::Client::new(ChannelTransport { chan: clients.input_admin }).open_keys() {
					Some(Ok(input)) => input,
					_ => 0,
				}
			}
			Capability::AudioStream => {
				if clients.audio_admin == 0 {
					return 0;
				}
				match audio_admin::Client::new(ChannelTransport { chan: clients.audio_admin }).open_streams() {
					Some(Ok(audio)) => audio,
					_ => 0,
				}
			}
			_ => grant_handle(clients, cap),
		}
	}
}

// Mint the handle actually granted for `cap`. Network is always a fresh `open`
// sub-connection, so concurrent tools never share one reply queue. Config and device
// are likewise fresh sub-connections and additionally re-resolve a dead held client
// through the broker, making their service restarts transparent. Every other capability
// is granted as a narrowed duplicate of the held client.
// Returns 0 when no live client can be produced. (A re-resolving grant assumes the
// broker peer answers RESOLVE - ServiceManager does; a scenario that grants config or
// device must stand in for the broker or keep the service alive.)
unsafe fn grant_handle(clients: &mut Clients, cap: Capability) -> u64 {
	unsafe {
		if cap == Capability::Network {
			let mut client = network::Client::new(ChannelTransport { chan: clients.network });
			let minted = match client.open() {
				Some(Ok(minted)) => minted,
				_ => return 0,
			};
			let dup = duplicate(minted, GRANT_RIGHTS);
			close(minted);
			return if dup >= 0 { dup as u64 } else { 0 };
		}
		let (held, name): (&mut u64, &'static [u8]) = match cap {
			Capability::Config => (&mut clients.config, CAP_CONFIG),
			Capability::Device => (&mut clients.device, CAP_DEVICE),
			_ => {
				let dup: i64 = duplicate(clients.for_capability(cap), GRANT_RIGHTS);
				return if dup >= 0 { dup as u64 } else { 0 };
			}
		};
		// Mint a fresh sub-connection, re-resolving a dead held client through the
		// broker (answered once the restarted instance serves).
		let minted: u64 = match connect_or_resolve(held, clients.broker, name) {
			Some(m) => m,
			None => return 0,
		};
		// Narrow the minted connection to a client's rights, like every other grant.
		let dup: i64 = duplicate(minted, GRANT_RIGHTS);
		close(minted);
		if dup >= 0 { dup as u64 } else { 0 }
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
		let identity = executable::lookup_identity(&component).ok_or(Error::NotFound)?;
		manifest_for(identity.as_bytes()).ok_or(Error::NotFound)
	}
	// The audit trail, streamed entry by entry (the serve loop frames the vector
	// onto a sub-channel): the trail grows with every launch and never has to fit
	// one reply.
	fn audit(&mut self) -> Vec<AuditEntry> {
		self.audit.clone()
	}
	fn run(&mut self, name: String, args: String, cwd: String, environment: Vec<EnvVar>, stdout: u64) -> Result<StartResult, Error> {
		if !environment_is_acceptable(&environment) {
			unsafe { close(stdout) };
			return Err(Error::Invalid);
		}
		match unsafe { run_tool_under_manifest(self.procsvc, name.as_bytes(), args.as_bytes(), cwd.as_bytes(), &environment, stdout, &mut self.clients, &mut self.audit) } {
			Some(started) => Ok(started),
			None => Err(Error::NotFound),
		}
	}

	// Start a pipeline as one transaction. The broker allocates every edge itself: the caller
	// names only the tools and their arguments, so it can neither pick which endpoint a stage
	// receives nor hand one in from outside. `stdout` is the terminal end, and it belongs to
	// the LAST stage - every earlier stage writes into the edge made for it.
	fn run_pipeline(&mut self, stages: Vec<PipelineStage>, cwd: String, environment: Vec<EnvVar>, stdout: u64) -> Result<PipelineResult, Error> {
		if !environment_is_acceptable(&environment) {
			unsafe { close(stdout) };
			return Err(Error::Invalid);
		}
		if stages.is_empty() || stages.len() > MAX_PIPELINE_STAGES {
			// Bounded like every other resource here: a caller cannot ask for an unbounded
			// number of processes and endpoints in one request.
			unsafe { close(stdout) };
			return Err(Error::Invalid);
		}
		unsafe {
			// One edge per `A | B`. Allocated up front so a failure to make one costs nothing
			// but the endpoints already made - no stage exists yet.
			let mut edges: Vec<(u64, u64)> = Vec::new();
			for _ in 1..stages.len() {
				match channel() {
					Some(pair) => edges.push(pair),
					None => {
						for (read, write) in edges {
							close(read);
							close(write);
						}
						close(stdout);
						return Err(Error::Invalid);
					}
				}
			}
			// Stage i writes to edge i (or the terminal, if it is last) and reads from
			// edge i-1 (or nothing, if it is first). `channel()` returns (a, b) as a connected
			// pair; a stage writes into one end and its consumer reads the other.
			let mut requests: Vec<StageRequest> = Vec::new();
			for (index, stage) in stages.iter().enumerate() {
				let out: u64 = if index + 1 == stages.len() { stdout } else { edges[index].1 };
				let input: u64 = if index == 0 { 0 } else { edges[index - 1].0 };
				// A stage's diagnostics belong on the TERMINAL, not in the pipe.
				//
				// Every stage but the last writes into an edge, and until the error endpoint
				// existed a diagnostic had nowhere else to go: `eprint` falls back to stdout, so
				// `cat missing | readln` would hand "cat: invalid path" to `readln` as if it were
				// data. Each stage gets its own send-only duplicate of the terminal, so a message
				// reaches the person and the pipe carries only what the tool produced.
				//
				// Send-only deliberately: a stage has no business READING the terminal through the
				// channel it reports errors on.
				//
				// UNLESS THE STAGE ASKED FOR `2>&1`, which is the opposite request: fold the
				// diagnostics into the stream. Only the broker can serve it, because only the broker
				// knows which endpoint a stage's output actually IS - an edge for every stage but
				// the last, and the caller's terminal for that one. The shell cannot name it and
				// deliberately cannot: the record's own comment says a caller names no stdio at all.
				let error: u64 = {
					let source: u64 = if stage.merge_errors { out } else { stdout };
					let dup: i64 = duplicate(source, RIGHT_SEND | RIGHT_WAIT | RIGHT_TRANSFER);
					if dup > 0 { dup as u64 } else { 0 }
				};
				requests.push(StageRequest { name: stage.name.as_bytes(), args: stage.args.as_bytes(), stdout: out, stdin: input, stderr: error });
			}
			let started: Vec<StartResult> = match run_pipeline_under_manifest(self.procsvc, &requests, cwd.as_bytes(), &environment, &mut self.clients, &mut self.audit) {
				Some(started) => started,
				None => {
					// The transaction released nothing, so nothing ran. The endpoints are the
					// only thing to clean up.
					for (read, write) in edges {
						close(read);
						close(write);
					}
					close(stdout);
					return Err(Error::NotFound);
				}
			};
			// The broker's own copies of the edge endpoints are spent: each was transferred to
			// the stage that owns it, and holding a duplicate here would keep a pipe open after
			// its writer exits, so the reader would never see end-of-stream.
			let count: u32 = started.len() as u32;
			let tasks: Vec<u64> = started.iter().map(|s| s.task).collect();
			let group: i64 = process_group_create(&tasks);
			for task in tasks {
				close(task);
			}
			if group < 0 {
				return Err(Error::Invalid);
			}
			Ok(PipelineResult { group: group as u64, stages: count })
		}
	}
}

// A pipeline may not ask for more stages than the shell grammar can express, so the two
// bounds cannot disagree about what is a legal line.
const MAX_PIPELINE_STAGES: usize = 8;

// Launch a component under its permission manifest: ask ProcessService (the loading
// mechanism) to start it with a fresh bootstrap channel, then for every capability in the
// vocabulary grant the held client if the manifest lists it (recording the grant) or
// withhold it (recording the denial). The component receives exactly its manifest's
// capabilities, in vocabulary order, and can reach nothing else - the sandbox. After the
// static grants it may make runtime permission requests for undeclared capabilities; each
// is decided by the headless policy default and recorded in the same audit trail as a
// dynamic decision. Returns the bytes the component reported back (its proof the granted
// capabilities are live), or None if the launch failed.
unsafe fn launch_under_manifest(procsvc: u64, component: &[u8], clients: &mut Clients, audit: &mut Vec<AuditEntry>, buf: &mut [u8]) -> Option<Vec<u8>> {
	unsafe {
		let (manager_side, child_side): (u64, u64) = channel()?;
		// Hand the child end to ProcessService, which loads the component and starts it with
		// that end as its bootstrap; the manager keeps `manager_side` to grant over. The
		// returned process handle is the manager's job-control handle on the component.
		let name: String = String::from_utf8_lossy(component).into_owned();
		let mut process_client = process::Client::new(ChannelTransport { chan: procsvc });
		// PREPARED, not started: the component is built but does not run until every grant
		// below has been installed. A launch that fails partway is then a process that never
		// ran at all, rather than one that observed half its capabilities and started work on
		// the strength of them.
		let started: StartResult = match process_client.launch_prepared(&name, &child_side) {
			Some(Ok(started)) => started,
			_ => {
				close(manager_side);
				return None;
			}
		};
		let task: u64 = started.task;
		let koid: u64 = started.info.koid;
		let policy_name: String = match executable::logical_name(&started.info.name) {
			Some(name) => String::from(name),
			None => {
				// Abandoned rather than released: dropping a prepared launch is how a failed
				// transaction unwinds, and it leaves nothing that ever ran.
				close(manager_side);
				close(task);
				return None;
			}
		};
		let manifest: Manifest = match manifest_for(policy_name.as_bytes()) {
			Some(manifest) => manifest,
			None => {
				close(manager_side);
				close(task);
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
				let handle: u64 = grant_for_task(clients, cap, task);
				if handle == 0 || !send_blocking(manager_side, tag_for(cap), handle) {
					close(manager_side);
					close(task);
					return None;
				}
			}
			audit.push(AuditEntry { component: policy_name.clone(), capability: cap, granted, dynamic: false });
		}
		// Every static grant is installed, so the graph this component can see is complete:
		// release it. This MUST happen before the receive loop below - the manager waits on
		// the component there, and waiting on a process that was never started is a hang, not
		// an error. It is also the transaction's commit point: everything above can fail and
		// leave nothing running, and nothing below can.
		if !matches!(process_client.release(&koid), Some(Ok(true))) {
			close(manager_side);
			close(task);
			return None;
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
						let granted: bool = grant_dynamic(policy_name.as_bytes(), cap, clients, manager_side);
						audit.push(AuditEntry { component: policy_name.clone(), capability: cap, granted, dynamic: true });
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
unsafe fn grant_dynamic(component: &[u8], cap: Capability, clients: &mut Clients, manager_side: u64) -> bool {
	unsafe {
		if dynamic_policy(component, cap) {
			let handle: u64 = grant_handle(clients, cap);
			if handle != 0 && send_blocking(manager_side, tag_for(cap), handle) {
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
unsafe fn run_tool_under_manifest(procsvc: u64, name: &[u8], args: &[u8], cwd: &[u8], environment: &[EnvVar], stdout: u64, clients: &mut Clients, audit: &mut Vec<AuditEntry>) -> Option<StartResult> {
	unsafe {
		let name_str: &str = core::str::from_utf8(name).ok()?;
		let (manager_side, child_side): (u64, u64) = channel()?;
		let mut process_client = process::Client::new(ChannelTransport { chan: procsvc });
		// Prepared, never started here: every tool goes through the same gate a pipeline stage
		// does, so the single-stage and multi-stage paths cannot drift in how a process is
		// built. The release is at the bottom, once the whole graph this tool can see exists.
		let started: StartResult = match if name == b"imgconv" { process_client.launch_bounded(name_str, &IMGCONV_MEMORY_LIMIT, &child_side) } else { process_client.launch_prepared(name_str, &child_side) } {
			Some(Ok(s)) => s,
			_ => {
				close(manager_side);
				return None;
			}
		};
		let prepared: bool = name != b"imgconv";
		let policy_name: String = match executable::logical_name(&started.info.name) {
			Some(name) => String::from(name),
			None => {
				close(manager_side);
				close(started.task);
				return None;
			}
		};
		let manifest: Manifest = match manifest_for(policy_name.as_bytes()) {
			Some(manifest) => manifest,
			None => {
				close(manager_side);
				close(started.task);
				return None;
			}
		};
		// Forward the stdout console first (the tool's `inherit_stdout` reads the first
		// message), then the argument string, then the manifest grants.
		// The launch endpoints, named and ended by READY. A governed tool gets the caller's
		// console, which is full duplex, so it reads and writes the same channel.
		send_blocking(manager_side, CAP_STDOUT, stdout);
		send_ready(manager_side);
		if !send_launch_context(manager_side, args, cwd, environment) {
			close(manager_side);
			close(started.task);
			return None;
		}
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
					let handle: u64 = grant_for_task(clients, cap, started.task);
					handle != 0 && send_blocking(manager_side, tag_for(cap), handle)
				};
				if !ok {
					close(manager_side);
					return None;
				}
			}
			audit.push(AuditEntry { component: policy_name.clone(), capability: cap, granted, dynamic: false });
		}
		// Commit: the tool's stdout, arguments, grants and cwd are all queued, so what it will
		// observe is complete. Anything that failed above returned without releasing, which
		// leaves a process that never ran rather than one that started on half a grant set.
		if prepared && !matches!(process_client.release(&started.info.koid), Some(Ok(true))) {
			close(manager_side);
			return None;
		}
		close(manager_side);
		Some(started)
	}
}

// A pipeline stage as the broker needs it: the tool to run, its argument string, and the
// stdio it is to be given. `stdout` is always present (the terminal for the last stage, an
// edge's write end otherwise); `stdin` is present only for a stage that has a producer.
struct StageRequest<'a> {
	name: &'a [u8],
	args: &'a [u8],
	stdout: u64,
	stdin: u64,
	// The terminal, so a stage's diagnostics do not travel down the pipe as data.
	stderr: u64,
}

// Build a whole pipeline as one transaction: prepare every stage, install every stage's
// stdio and grants, and only then release them together.
//
// The ordering is the point. Every stage is created behind the start gate, so a failure at
// ANY stage - an unknown tool, a missing manifest, an endpoint that cannot be transferred -
// returns having released nothing, and no stage has run. A pipeline that half-exists is
// worse than one that does not: stage B reading from a producer that will never be started
// would block forever, and stage A writing into an endpoint nobody will read would too.
//
// Each stage's stdio goes over in ONE message carrying ordered capabilities - stdout first,
// stdin second when it has one - because a receiver cannot tell a second message that was
// never sent from the next handoff in its bootstrap sequence. That is what the P02M0090
// multi-capability work exists for, and it is why this needs no "does this stage have
// stdin?" agreement between the two sides.
unsafe fn run_pipeline_under_manifest(procsvc: u64, stages: &[StageRequest], cwd: &[u8], environment: &[EnvVar], clients: &mut Clients, audit: &mut Vec<AuditEntry>) -> Option<Vec<StartResult>> {
	unsafe {
		let mut process_client = process::Client::new(ChannelTransport { chan: procsvc });
		let mut prepared: Vec<(StartResult, u64)> = Vec::new();
		// Unwind that runs on every early return: abandoning a prepared launch is how this
		// transaction rolls back, because a stage that was never released never ran.
		macro_rules! abandon {
			($built:expr) => {{
				for (started, manager_side) in $built {
					close(manager_side);
					close(started.task);
				}
				return None;
			}};
		}
		for stage in stages {
			let name_str: &str = match core::str::from_utf8(stage.name) {
				Ok(s) => s,
				Err(_) => abandon!(prepared),
			};
			let (manager_side, child_side): (u64, u64) = match channel() {
				Some(pair) => pair,
				None => abandon!(prepared),
			};
			let started: StartResult = match process_client.launch_prepared(name_str, &child_side) {
				Some(Ok(s)) => s,
				_ => {
					close(manager_side);
					abandon!(prepared);
				}
			};
			let policy_name: String = match executable::logical_name(&started.info.name) {
				Some(name) => String::from(name),
				None => {
					close(manager_side);
					close(started.task);
					abandon!(prepared);
				}
			};
			let manifest: Manifest = match manifest_for(policy_name.as_bytes()) {
				Some(manifest) => manifest,
				None => {
					close(manager_side);
					close(started.task);
					abandon!(prepared);
				}
			};
			// stdout, then stdin when there is one, as ordered capabilities in one message.
			// A stage writes into one edge and reads from another, so the two endpoints are
			// named separately rather than told apart by how many arrived.
			let installed: bool = send_blocking(manager_side, CAP_STDOUT, stage.stdout) && (stage.stdin == 0 || send_blocking(manager_side, CAP_STDIN, stage.stdin)) && (stage.stderr == 0 || send_blocking(manager_side, CAP_STDERR, stage.stderr)) && send_ready(manager_side);
			if !installed {
				close(manager_side);
				close(started.task);
				abandon!(prepared);
			}
			if !send_launch_context(manager_side, stage.args, cwd, environment) {
				close(manager_side);
				close(started.task);
				abandon!(prepared);
			}
			for &cap in VOCABULARY.iter() {
				let granted: bool = manifest.grants.contains(&cap);
				if granted {
					let ok: bool = if cap == Capability::Volumes {
						grant_volumes(manager_side, clients)
					} else {
						let handle: u64 = grant_for_task(clients, cap, started.task);
						handle != 0 && send_blocking(manager_side, tag_for(cap), handle)
					};
					if !ok {
						close(manager_side);
						close(started.task);
						abandon!(prepared);
					}
				}
				audit.push(AuditEntry { component: policy_name.clone(), capability: cap, granted, dynamic: false });
			}
			prepared.push((started, manager_side));
		}
		// Commit. Released in pipeline order so a producer is running before its consumer can
		// find the pipe empty - not required for correctness, since a consumer blocks on an
		// empty channel either way, but it keeps the common case from a needless wait.
		let mut started: Vec<StartResult> = Vec::new();
		for (result, manager_side) in prepared {
			if !matches!(process_client.release(&result.info.koid), Some(Ok(true))) {
				// Past the first release the transaction can no longer be unwound cleanly:
				// stages already running are told to stop rather than left orphaned.
				close(manager_side);
				close(result.task);
				for earlier in &started {
					signal(earlier.task, abi::SIG_KILL);
				}
				return None;
			}
			close(manager_side);
			started.push(result);
		}
		Some(started)
	}
}

// What a launch context's environment may carry, checked by the LAUNCHER rather than trusted from
// the caller. The shell proposes the table; this manager decides what a child is born with, the
// same way it decides which capabilities it gets.
//
// Refused rather than trimmed. Silently dropping a variable that is too long, or the sixty-fifth
// of sixty-four, gives a program an environment its caller never described - and the difference
// surfaces as a tool behaving differently for reasons nothing reports. A caller that asks for more
// than this is told.
//
// The total is well under `rt::LAUNCH_CONTEXT_MAX`, which bounds the whole encoded record: the
// arguments and the working directory have to fit beside it.
const MAX_ENV_VARS: usize = 64;
const MAX_ENV_NAME: usize = 64;
const MAX_ENV_VALUE: usize = 4096;
const MAX_ENV_BYTES: usize = 32 * 1024;

// Whether an environment table may be handed to a child.
//
// A name is what a shell expands, so it is bounded, non-empty, and free of `=` (which would make
// the pair unparseable wherever it is rendered as `NAME=value`) and of NUL. Values are bounded but
// otherwise opaque - they are data.
fn environment_is_acceptable(environment: &[EnvVar]) -> bool {
	if environment.len() > MAX_ENV_VARS {
		return false;
	}
	let mut total: usize = 0;
	for variable in environment {
		if variable.name.is_empty() || variable.name.len() > MAX_ENV_NAME || variable.value.len() > MAX_ENV_VALUE {
			return false;
		}
		if variable.name.bytes().any(|byte| byte == b'=' || byte == 0) {
			return false;
		}
		total += variable.name.len() + variable.value.len();
		if total > MAX_ENV_BYTES {
			return false;
		}
	}
	true
}

// Send a launched program its context: the arguments it was invoked with, the working directory
// it resolves relative paths against, and the environment it inherits.
//
// One message, right after the stdout handoff and BEFORE any capability grant. It used to be two
// bare messages with the grants between them, and the second - the working directory - had to be
// last, because a tool reads its grants with a tagged receive and a bare message arriving early
// is consumed as whatever that tool reads next. Growing that sequence is what shifted the
// `volumes` bundle by two and surfaced, four steps later, as a tool reading a volume client as
// its working directory.
//
// The environment is a SNAPSHOT: the values as they stood when the launch was asked for, never a
// capability to the session that holds them. The caller proposes it and this manager decides it,
// which is why it is checked here rather than where it was read.
//
// Returns false if the context cannot be encoded or sent, which the callers treat like any other
// failed grant: the process is abandoned rather than started with half a context.
unsafe fn send_launch_context(manager_side: u64, args: &[u8], cwd: &[u8], environment: &[EnvVar]) -> bool {
	let context = LaunchContext { arguments: String::from_utf8_lossy(args).into_owned(), cwd: String::from_utf8_lossy(cwd).into_owned(), environment: environment.to_vec() };
	let Some(bytes) = context.encode_vec() else { return false };
	if bytes.len() > rt::LAUNCH_CONTEXT_MAX {
		return false;
	}
	unsafe { send_blocking(manager_side, &bytes, 0) }
}

// Grant the volume StorageService clients the `volumes` capability bundles, each under its own
// tag: system (writable LiberFS), media (FAT/exFAT), iso (ISO9660), udf (UDF), usb (FAT off the
// USB stick), and the two memory volumes. Each held client is duplicated (narrowed to a client's
// rights, the manager keeping its own) and transferred; a volume whose disk is absent is held as
// 0 and handed over as a tagged message with no handle. The run ends with READY, so a tool takes
// what it wants BY NAME and what it leaves is closed for it.
//
// The sentence that stood here said the grant "always sends exactly five messages and the
// receiver's order stays aligned", and by then it sent seven. That is the failure this shape
// removes rather than a comment that needed updating: keeping a count in prose, in one file, that
// twelve others depend on positionally. Returns false only if a transfer itself fails.
unsafe fn grant_volumes(manager_side: u64, clients: &Clients) -> bool {
	unsafe {
		let volumes: [(&[u8], u64); 7] = [
			(CAP_SYSTEM, clients.storage),
			(CAP_MEDIA, clients.storage_media),
			(CAP_ISO, clients.storage_iso),
			(CAP_UDF, clients.storage_udf),
			(CAP_USB, clients.storage_usb),
			(CAP_RAM, clients.storage_ram),
			(CAP_TMP, clients.storage_tmp),
		];
		for &(tag, client) in volumes.iter() {
			// A FRESH SUB-CONNECTION PER GRANT, not a duplicate of the manager's own.
			//
			// `grant_handle` states the rule for network - "Network is always a fresh `open`
			// sub-connection, so concurrent tools never share one reply queue" - and this path
			// duplicated instead, which was invisible while tools ran one at a time. A pipeline
			// runs them at once: `redirect_in f | tee g | wc` had two stages sending on two names
			// for ONE endpoint, and one of them took the other's reply and reported that the file
			// could not be opened. See `volume.connect` in `storage.lsidl`.
			//
			// A VOLUME THAT CANNOT MINT ONE IS GRANTED AS ZERO rather than failing the launch. The
			// manager holds seven volume clients and several are routinely absent (no USB, no ISO);
			// a stage that asks such a volume for anything already gets nothing, and turning a
			// missing volume into a failed pipeline would break every line that touches storage on
			// a machine with one disk.
			let minted: u64 = if client == 0 {
				0
			} else {
				let mut volume_client = volume::Client::new(ChannelTransport { chan: client });
				match volume_client.connect() {
					Some(Ok(fresh)) => {
						let dup: i64 = duplicate(fresh, GRANT_RIGHTS);
						close(fresh);
						if dup >= 0 { dup as u64 } else { 0 }
					}
					_ => 0,
				}
			};
			if !send_blocking(manager_side, tag, minted) {
				return false;
			}
		}
		send_ready(manager_side)
	}
}

// Demonstrate the on-demand tool launcher (the `run` op's mechanism) at startup: stand in
// for the shell by handing the tool a captured stdout console, run it under its manifest,
// and drain everything it prints until clean exit - proof the tool reached its one granted
// capability and that its complete output was forwarded to the caller's terminal. The shell reaches this same path live
// over the `run` op; here the manager plays both launcher and terminal so the path is
// exercised end to end. Returns the bytes the tool printed, or empty if it could not start.
unsafe fn demonstrate_tool(procsvc: u64, name: &[u8], args: &[u8], clients: &mut Clients, audit: &mut Vec<AuditEntry>, buf: &mut [u8]) -> Vec<u8> {
	unsafe {
		let (output, console): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return Vec::new(),
		};
		let started: StartResult = match run_tool_under_manifest(procsvc, name, args, b"", &[], console, clients, audit) {
			Some(s) => s,
			None => {
				close(output);
				return Vec::new();
			}
		};
		let mut printed: Vec<u8> = Vec::new();
		loop {
			match recv_blocking(output, buf) {
				Received::Message { len, .. } => printed.extend_from_slice(&buf[..len]),
				Received::Closed => break,
			}
		}
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
	//    so the governed `config` / `set`, `lsdev`, `beep`, and `usage` commands can reach their one
	//    service, process so the governed `ps` / `run` commands can list / start processes - a
	//    dedicated ProcessService connection, kept separate from the launch mechanism below -, and
	//    supervisor so the governed `stop` command can drive the supervisor's teardown path over a
	//    dedicated ServiceManager admin channel). Private display/input/audio admin clients mint
	//    process-bound display, key-only input and playback-only audio grants; applications never
	//    receive those admin interfaces. The permission capability is minted locally below as a
	//    self-connection. Legacy broad `input` and `graph` remain declared but unwired.
	//
	// Taken BY NAME out of one set, not read in order off the channel. This handshake used to be
	// twenty-three positional receives matched against twenty-three sends in another file, with
	// nothing marking where the run ended - and `unwrap_or(0)`, which is right for a capability
	// the supervisor genuinely cannot grant, made a drifted sequence indistinguishable from a
	// withheld one. Adding two receives here without matching sends once reported that
	// NetworkService had not asked for a client: a true statement about the wrong service, four
	// steps from the edit. `recv_caps` reads to the READY terminator and `take` matches on the
	// tag, so a capability that is absent reads as absent and an order that has changed does not
	// read as anything at all.
	let mut caps: CapSet = unsafe { recv_caps(bootstrap) };
	let storage: u64 = caps.take(CAP_STORAGE);
	let log: u64 = caps.take(CAP_LOG);
	let network: u64 = caps.take(CAP_NETWORK);
	let time: u64 = caps.take(CAP_TIME);
	// VT 1's SessionService client, for the governed `kill` command. Absent (0) in the harnesses
	// that run no session service, where `kill` is simply not grantable.
	let session: u64 = caps.take(CAP_SESSION);
	let config: u64 = caps.take(CAP_CONFIG);
	let device: u64 = caps.take(CAP_DEVICE);
	let audio: u64 = caps.take(CAP_AUDIO);
	let display_admin: u64 = caps.take(CAP_DISPLAY_ADMIN);
	let input_admin: u64 = caps.take(CAP_INPUT_ADMIN);
	let audio_admin: u64 = caps.take(CAP_AUDIO_ADMIN);
	let resource: u64 = caps.take(CAP_RESOURCE);
	let process: u64 = caps.take(CAP_PROCESS_GRANT);
	// The admin channel the manager grants to the governed `stop` command (whose manifest
	// grants supervisor): a dedicated ServiceManager admin channel, separate from the shell's,
	// the manager holds but never drives itself - it only duplicates a narrowed copy onto the
	// sandboxed `stop` tool, which speaks the bare request/reply teardown protocol over it.
	let supervisor: u64 = caps.take(CAP_SUPERVISOR);
	// The three non-system volume StorageService clients the supervisor connects for the
	// manager, bundled with the system `storage` client under the `volumes` capability the
	// governed `lsvol` command is granted: media (FAT/exFAT), iso (ISO9660), udf (UDF). A
	// volume whose disk is absent arrives as 0 (the manager simply cannot grant what it does
	// not hold), and `lsvol` shows it as zero files.
	let storage_media: u64 = caps.take(CAP_STORAGE_MEDIA);
	let storage_iso: u64 = caps.take(CAP_STORAGE_ISO);
	let storage_udf: u64 = caps.take(CAP_STORAGE_UDF);
	let storage_usb: u64 = caps.take(CAP_STORAGE_USB);
	let storage_ram: u64 = caps.take(CAP_STORAGE_RAM);
	let storage_tmp: u64 = caps.take(CAP_STORAGE_TMP);
	// The supervisor-status channel the manager grants to the governed `lssvc` command
	// (whose manifest grants services): a dedicated ServiceManager status channel, separate
	// from SystemGraphService's, the manager holds but never drives itself.
	let services: u64 = caps.take(CAP_SERVICES);
	// The xHCI driver's USB bus query channel the manager grants to the governed `lsusb`
	// command (whose manifest grants usb): the driver serves the typed `usb` inventory on
	// it; 0 when the driver never came up.
	let usb: u64 = caps.take(CAP_USBBUS);
	// Mint the manager's self-connection: a dedicated channel pair whose server end is seeded
	// into the serve set below (so requests on it are dispatched like any other client's) and
	// whose client end the manager holds as the grantable `permission` capability. The governed
	// `perm` command thus reaches the very audit trail this manager serves over a connection of
	// its own - a capability the manager grants to a copy of itself, on a dedicated channel so a
	// granted tool's queries never race the supervisor's own connection.
	let (perm_self_server, perm_self_client): (u64, u64) = unsafe { channel() }.unwrap_or_else(|| unsafe { fail_bootstrap(bootstrap, b"channel", b"could not mint self-connection") });
	let mut clients: Clients = Clients { log, storage, network, time, config, device, audio, input: 0, graph: 0, resource, process, permission: perm_self_client, supervisor, services, usb, storage_media, storage_iso, storage_udf, storage_usb, storage_ram, storage_tmp, display_admin, input_admin, audio_admin, session, broker: bootstrap };
	let procsvc: u64 = match caps.take(CAP_PROCESS) {
		0 => unsafe { fail_bootstrap(bootstrap, b"process", b"process client not delivered") },
		handle => handle,
	};

	// 2. wait for the serve channel clients reach us on.
	let service: u64 = match caps.take(CAP_SERVE) {
		0 => unsafe { fail_bootstrap(bootstrap, b"serve", b"missing serve channel") },
		handle => handle,
	};

	// 3. launch each governed component under its manifest, accumulating one shared audit
	//    trail: sandbox_probe (granted storage + log, denied the rest) reads its one file and
	//    reports the bytes back; `date` (granted only time) is launched on demand through the
	//    `run` op and renders the wall clock to a captured stdout; request_probe (granted only
	//    log) asks for an undeclared capability at runtime to exercise the dynamic-request
	//    path; `cat` (granted only volumes) prints a file; and `ip` (granted only network)
	//    queries a fresh NetworkService sub-connection and renders it to captured stdout.
	let mut audit: Vec<AuditEntry> = Vec::new();
	let probe_read: Vec<u8> = unsafe { launch_under_manifest(procsvc, PROBE_NAME, &mut clients, &mut audit, &mut buf) }.unwrap_or_default();
	let date_read: Vec<u8> = unsafe { demonstrate_tool(procsvc, DATE_NAME, b"", &mut clients, &mut audit, &mut buf) };
	let request_read: Vec<u8> = unsafe { launch_under_manifest(procsvc, REQUEST_NAME, &mut clients, &mut audit, &mut buf) }.unwrap_or_default();
	let cat_read: Vec<u8> = unsafe { demonstrate_tool(procsvc, CAT_NAME, b"vol://system/hello.txt", &mut clients, &mut audit, &mut buf) };
	let ip_read: Vec<u8> = unsafe { demonstrate_tool(procsvc, IP_NAME, b"", &mut clients, &mut audit, &mut buf) };

	// 4. report in to the supervisor, then relay each governed component's proof and its
	//    decisions summary (exactly which capabilities it was and was not given): the bytes
	//    sandbox_probe read through its storage grant, the instant `date` printed through its
	//    time grant to a captured stdout, request_probe's verdict on its runtime request for an
	//    undeclared capability (its summary marks that refused request as a dynamic decision),
	//    then the complete stdout from the on-demand `cat` and `ip` tools plus `ip`'s exact
	//    network-only capability decisions.
	unsafe {
		send_blocking(bootstrap, b"PermissionManager: online", 0);
		send_blocking(bootstrap, &probe_read, 0);
		send_blocking(bootstrap, &summarize_for(&audit, PROBE_NAME), 0);
		send_blocking(bootstrap, &date_read, 0);
		send_blocking(bootstrap, &summarize_for(&audit, DATE_NAME), 0);
		send_blocking(bootstrap, &request_read, 0);
		send_blocking(bootstrap, &summarize_for(&audit, REQUEST_NAME), 0);
		send_blocking(bootstrap, &cat_read, 0);
		send_blocking(bootstrap, &ip_read, 0);
		send_blocking(bootstrap, &summarize_for(&audit, IP_NAME), 0);
	}

	// 5. serve generated lookup/audit/run requests until the supervisor drops the channel. The
	//    self-connection's server end is seeded into the client set alongside the root, so the
	//    governed `perm` command - granted the matching client end - is served like any other.
	//    OP_AUDIT opens a stream (the log-tail model): the trail is framed entry by entry onto
	//    a fresh sub-channel, so it never has to fit one reply.
	let mut manager: Manager = Manager { audit, procsvc, clients };
	let mut request: [u8; 512] = [0u8; 512];
	let mut reply: [u8; 4096] = [0u8; 4096];
	unsafe {
		serve_multi_seeded(service, &[perm_self_server], &mut request, &mut reply, |chan, req, handle, out, reply_handle| -> Option<usize> {
			let op: u16 = if req.len() >= 2 { u16::from_le_bytes([req[0], req[1]]) } else { 0 };
			if op == permission::OP_AUDIT {
				stream_audit(&mut manager, chan, req, handle);
				return None;
			}
			permission::dispatch(&mut manager, req, handle, out, reply_handle)
		});
	}
	exit();
}

// Serve one OP_AUDIT request: gather the trail snapshot, then stream the entries to
// the client over a fresh sub-channel (the reply carries the correlation id and the
// consumer endpoint out-of-band; closing the producer marks end-of-stream).
fn stream_audit(manager: &mut Manager, service: u64, request: &[u8], request_handle: &mut proto::codec::Handles) {
	let (corr, items): (u32, Vec<AuditEntry>) = match permission::audit_open(manager, request, request_handle) {
		Some(v) => v,
		None => return,
	};
	let (producer, consumer): (u64, u64) = match unsafe { channel() } {
		Some(pair) => pair,
		None => return,
	};
	let corr_bytes: [u8; 4] = corr.to_le_bytes();
	unsafe {
		send_blocking(service, &corr_bytes, consumer);
	}
	let mut frame: [u8; 1024] = [0u8; 1024];
	for (seq, item) in items.iter().enumerate() {
		let mut frame_handles = Handles::new();
		if let Some(n) = permission::audit_frame(seq as u32, item, &mut frame, &mut frame_handles) {
			unsafe {
				if !send_caps_blocking(producer, &frame[..n], frame_handles.as_slice()) {
					for handle in frame_handles.as_slice() {
						close(*handle);
					}
				}
			}
		} else {
			for handle in frame_handles.as_slice() {
				unsafe { close(*handle) };
			}
		}
	}
	unsafe {
		close(producer);
	}
}
