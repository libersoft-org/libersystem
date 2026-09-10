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
// `core::mem::variant_count` is what lets the grant vocabulary be checked against the SCHEMA at
// compile time - see the assertion beside `VOCABULARY`. Unstable, and pinned with the rest of the
// toolchain: a stabilisation turns this line into a warning, which this crate denies, which is
// the moment to delete it.
#![feature(variant_count)]

extern crate alloc;

#[path = "../../provider_subscription.rs"]
pub mod provider_subscription;
use proto::system::ProviderKind;
use provider_subscription::{ProviderWatch, open_provider};

use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::codec::Handles;
use proto::system::audio_admin;
use proto::system::display_admin;
use proto::system::input_admin;
use proto::system::network;
use proto::system::permission::{self, Service};
use proto::system::{AuditEntry, Capability, EnvVar, Error, LaunchContext, Manifest, PipelineResult, PipelineStage, SelectedFile, StartResult, config, process, volume, volume_admin};
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
// over nothing, and the launch is REFUSED rather than started without the authority it asked for).
// EVERY CAPABILITY THE GRANT LOOP CAN SEND, and the order it sends them in.
//
// IT IS EXHAUSTIVE AGAINST THE SCHEMA, AND THE BUILD SAYS SO. This array is what the grant loop
// WALKS, so a capability the generated `Capability` enum declares and this array omits is one no
// manifest row can deliver, however plainly the row grants it - and the failure is silent on both
// sides: the manager sends nothing and reports success, and a program reading its grants
// positionally takes the next capability under the missing one's tag. That happened twice
// (`Session`, `DevicePolicy`), and both were written down as known before anything checked.
// The anonymous constant below is the check: every schema variant is here exactly once, and a
// variant added to `security.lsidl` without a deliberate place in this order fails to compile.
// There is no second classification: every capability the schema declares is walked, because the
// manager is the one owner of every grant, and a capability it has no client for is a typed failed
// grant at launch rather than a quiet omission from this list.
const VOCABULARY: [Capability; 23] = [
	Capability::Storage,
	Capability::Log,
	Capability::Network,
	Capability::Device,
	// THE OPERATOR'S WRITE, AND IT WAS NOT IN THIS LIST AT ALL (added 2026-09-02).
	//
	// This array is what the grant loop WALKS - `for &cap in VOCABULARY.iter()` - so a capability
	// missing from it is one no manifest row can actually deliver, however plainly the row grants it.
	// `lsdev` has held `DevicePolicy` since the operator verbs were built and never once received
	// it: the loop skipped straight from `DEVICE` to `CONFIG`, and `lsdev` - which reads its grants
	// positionally - took the configuration client under the policy tag, answered "this boot granted
	// no device-policy authority", and every operator verb in the system was unreachable. Nothing
	// caught it because the only thing that drives one is a development check, and the development
	// instance would not boot.
	//
	// IT GOES BETWEEN `Device` AND `Config` because that is the order `lsdev` reads them in. This
	// list is a delivery ORDER as well as a set, and its one constraint is that it agrees with every
	// granted tool's receive sequence.
	Capability::DevicePolicy,
	Capability::Config,
	Capability::Time,
	Capability::Audio,
	Capability::Input,
	Capability::Graph,
	Capability::Resource,
	Capability::Process,
	Capability::Permission,
	Capability::Supervisor,
	// AND `Session`, WHICH WAS MISSING FOR THE SAME REASON. `kill` is granted it by its row and read
	// it with `unwrap_or(0)`, so it did not hang - it simply never had a SessionService client and
	// could not kill anything. Position is free here: `kill` is granted this and nothing else, so it
	// receives exactly one message whatever order this list is in.
	Capability::Session,
	Capability::Volumes,
	Capability::Services,
	Capability::Usb,
	Capability::Display,
	Capability::InputKeys,
	Capability::AudioStream,
	Capability::AudioCapture,
	Capability::AppAssets,
];

// THE ASSERTION THE COMMENT ABOVE PROMISES, evaluated by the compiler. Two halves: the array is as
// long as the enum (so nothing declared is missing), and no ordinal appears twice (so the length
// is not made up of a repeat). Together they say the array IS the enum, in some order - and the
// order is the delivery order, which the receive sequences of the granted tools pin.
// ANONYMOUS, because a named constant is evaluated only where it is used and this one has no user
// but the compiler.
const _: () = {
	assert!(VOCABULARY.len() == core::mem::variant_count::<Capability>(), "a capability the schema declares is not in the grant vocabulary");
	let mut seen: [bool; 64] = [false; 64];
	let mut index = 0;
	while index < VOCABULARY.len() {
		let ordinal = VOCABULARY[index] as usize;
		assert!(!seen[ordinal], "a capability is walked twice by the grant loop");
		seen[ordinal] = true;
		index += 1;
	}
};

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
		// THE DEVICE OPERATOR COMMAND holds both halves: the READ that renders the list, and the
		// WRITE that changes a binding. They are separate capabilities because nothing else in the
		// system needs the second - a tool that renders devices gets `device` and gets no closer.
		// AND `Config`, FOR THE INCIDENT THAT OUTLIVES ITS MANAGER. `--incident` reads DeviceManager's
		// live endpoint, and the report it serves dies with the process that holds it - which is the
		// one moment an operator most needs it, because DeviceManager dying is what killed the
		// driver subtree. The persisted copy is in ConfigService, which survives, and reading it is
		// what makes the snapshot visible after both deaths. A read of one key prefix rather than a
		// second write authority: `lsdev` already holds `DevicePolicy` for its four verbs.
		b"lsdev" => Some(granted("lsdev", alloc::vec![Capability::Device, Capability::DevicePolicy, Capability::Config])),
		b"config" => Some(granted("config", alloc::vec![Capability::Config])),
		b"set" => Some(granted("set", alloc::vec![Capability::Config])),
		b"beep" => Some(granted("beep", alloc::vec![Capability::Audio])),
		b"imgview" => Some(granted("imgview", alloc::vec![Capability::Volumes, Capability::Display, Capability::InputKeys])),
		b"licoview" => Some(granted("licoview", alloc::vec![Capability::Volumes, Capability::AppAssets])),
		b"licoedit" => Some(granted("licoedit", alloc::vec![Capability::Volumes, Capability::AppAssets])),
		// THE MANAGER GETS THE NARROW LAUNCH BROKER AND NOT PROCESS AUTHORITY. `Permission` lets it
		// ask for a NAMED program to be started, which then runs under that program's own manifest -
		// so an association or a command-bar line can launch something without lending it anything.
		// `Process` would be raw process creation, and is deliberately not here.
		//
		// `Session` IS NOT HERE, and it is now a POLICY question rather than a blocked one. The
		// reason it used to be blocked was a defect in the grant loop - `VOCABULARY` did not list
		// `Capability::Session`, so the loop never sent it and a tool that waits for the tag would
		// block on a message nobody will send. The vocabulary lists it now, and `check-grant-
		// vocabulary` keeps it listed. Whether a command bar should be able to end a session is a
		// decision for whoever needs it; nothing technical stops it any more.
		b"lico" => Some(granted("lico", alloc::vec![Capability::Volumes, Capability::AppAssets, Capability::Permission])),
		b"imgconv" => Some(granted("imgconv", alloc::vec![Capability::Volumes])),
		// A CONVERSION IS NOT A PLAYBACK. `audioconv` holds the volume bundle and nothing else - no
		// AudioService and no device authority - which is the distinction the bundle makes rather
		// than the code: converting a file is not playing one. It had no row here at all, so the
		// tool existed on the volume, was tested by the suite, and could not be launched on a real
		// system by any means.
		b"audioconv" => Some(granted("audioconv", alloc::vec![Capability::Volumes])),
		b"play" => Some(granted("play", alloc::vec![Capability::Volumes, Capability::AudioStream])),
		// A RECORDER HOLDS TWO THINGS: somewhere to write, and the authority to record. Not
		// `audio-stream`, which it has no use for, and not `audio`, which is the whole service.
		b"audiorec" => Some(granted("audiorec", alloc::vec![Capability::Volumes, Capability::AudioCapture])),
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
		// A pager reads files and nothing else. It runs as an interactive foreground job, which is
		// how it reaches the terminal - the tty is inherited stdio rather than a manifest grant.
		b"less" => Some(granted("less", alloc::vec![Capability::Volumes])),
		// A traceroute asks NetworkService one typed question per hop and holds no interface of its
		// own - which is the point of the op existing, since the conventional tool holds a raw
		// socket over every packet the machine sends.
		b"traceroute" => Some(granted("traceroute", alloc::vec![Capability::Network])),
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
		Capability::DevicePolicy => b"DEVPOLICY",
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
		Capability::AudioCapture => b"AUDIO_CAPTURE",
		Capability::Session => b"SESSION",
		Capability::AppAssets => b"APP_ASSETS",
	}
}

// The grantable clients the manager holds and may hand onward (0 = not granted to it).
struct Clients {
	log: u64,
	storage: u64,
	network: u64,
	device: u64,
	// THE OPERATOR'S WRITE, held apart from the read. A tool granted `device` renders a list; one
	// granted this changes a binding, and nothing that holds the first is any closer to the second.
	device_policy: u64,
	config: u64,
	time: u64,
	audio: u64,
	input: u64,
	graph: u64,
	resource: u64,
	process: u64,
	permission: u64,
	supervisor: u64,
	// The PRIVATE StorageService admin endpoint, which mints a client confined to one directory
	// below the system volume. The manager keeps it and never grants it: an ordinary volume client
	// cannot mint a broader scope, and this one can, which is exactly the difference between the
	// thing that hands out authority and the authority itself.
	storage_admin: u64,
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
	usb_catalogue: u64,
	usb_providers: ProviderWatch,
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
			Capability::DevicePolicy => self.device_policy,
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
			Capability::Usb => self.usb_catalogue,
			Capability::Display | Capability::InputKeys | Capability::AudioStream | Capability::AudioCapture => 0,
			// The `volumes` capability has no single representative client - it is granted as a
			// bundle of five channels by `grant_volumes`, never through this single-channel path.
			// The system volume stands in here for the (headless-denied) dynamic-request path.
			Capability::Volumes => self.storage,
			// Minted per launch rather than held, so `for_capability` has nothing to answer with.
			// See `grant_handle`.
			Capability::AppAssets => 0,
		}
	}
}

// Mint a launch-scoped capability. Display binding consumes a duplicate of the exact
// task ProcessService just returned and atomically returns its associated connection;
// input/audio admins mint connections narrowed to their advertised operation subset.
unsafe fn grant_for_task(clients: &mut Clients, cap: Capability, task: u64, component: &str) -> u64 {
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
			// THE OTHER DIRECTION, FROM THE SAME ADMIN CHANNEL AND NOT THE SAME GRANT. The
			// connection this mints refuses `open-stream` and refuses `beep`, so a recorder holding
			// it cannot make a sound - which is what makes granting a microphone a decision the
			// manifest states rather than one that comes along with playback.
			Capability::AudioCapture => {
				if clients.audio_admin == 0 {
					return 0;
				}
				match audio_admin::Client::new(ChannelTransport { chan: clients.audio_admin }).open_captures() {
					Some(Ok(audio)) => audio,
					_ => 0,
				}
			}
			_ => grant_handle(clients, cap, component),
		}
	}
}

// Which asset directory under `bin/` a component reads its own data from.
//
// USUALLY ITS OWN NAME, and the exception is a FAMILY that ships one bundle between its members.
// LiberCommander is three programs - the manager, the editor and the viewer - over one set of
// syntax descriptors, and installing three copies would be three things to keep in step for no
// gain. They share `bin/lico`, which is where the descriptors are installed and what this milestone
// calls the asset bundle.
//
// A FUNCTION AND NOT A MANIFEST FIELD. A path beside the grant would be a place to write the wrong
// one, and "which files may this program read" is not something a policy table should be able to
// get subtly wrong - it should be answerable from the program's identity alone.
// Which components get a configuration client that may only READ.
//
// A FUNCTION AND NOT A MANIFEST FIELD, for the reason `asset_bundle` gives directly below: the
// manifest says whether a capability is granted at all, and how much of it a particular program
// should have is answerable from the program's identity. A status tool reads; it does not configure
// the system.
fn config_is_read_only(component: &str) -> bool {
	matches!(component, "lsdev")
}

fn asset_bundle(component: &str) -> &str {
	match component {
		"lico" | "licoedit" | "licoview" => "lico",
		other => other,
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
unsafe fn grant_handle(clients: &mut Clients, cap: Capability, component: &str) -> u64 {
	unsafe {
		// A LAUNCH-SCOPED, DIRECTORY-CONFINED CLIENT, minted fresh for every launch.
		//
		// The scope is DERIVED FROM WHO IS BEING LAUNCHED rather than written in the manifest beside
		// the grant, so a component cannot be given somebody else's assets by a typo: `asset_bundle`
		// is a function, and the manifest says only whether the capability is granted at all.
		// Minting per launch also means the grant dies with the process rather than being a handle
		// the manager keeps and hands out repeatedly.
		if cap == Capability::AppAssets {
			if clients.storage_admin == 0 {
				return 0;
			}
			let mut path = String::from("vol://system/bin/");
			path.push_str(asset_bundle(component));
			let minted: u64 = match volume_admin::Client::new(ChannelTransport { chan: clients.storage_admin }).open_directory(&path) {
				Some(Ok(client)) => client,
				_ => return 0,
			};
			let dup = duplicate(minted, GRANT_RIGHTS);
			close(minted);
			return if dup >= 0 { dup as u64 } else { 0 };
		}
		if cap == Capability::Usb {
			clients.usb_providers.poll();
			if clients.usb_providers.channel == 0 {
				clients.usb_providers = ProviderWatch::subscribe(clients.usb_catalogue, ProviderKind::UsbBus);
			}
			for info in &clients.usb_providers.entries {
				let minted = open_provider(clients.usb_catalogue, info);
				if minted == 0 {
					continue;
				}
				let narrowed = duplicate(minted, GRANT_RIGHTS);
				close(minted);
				return if narrowed > 0 { narrowed as u64 } else { 0 };
			}
			return 0;
		}
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
		// AND A READ-ONLY CONFIGURATION GRANT IS SEALED BEFORE IT IS HANDED OVER.
		//
		// `lsdev` needs to read one persisted record - a device's last incident, under the reserved
		// prefix - after DeviceManager has died. Granting it `Capability::Config` for that gave it a
		// full configuration client, whose `set` accepts any key outside the reserved namespace:
		// authority over unrelated system configuration, granted to a status tool, to answer a
		// question about a device.
		//
		// The connection is sealed HERE, by the authority that mints it, so what the tool receives
		// can only read. Sealing is irreversible and belongs to the connection, so asking again
		// produces another sealed one.
		if cap == Capability::Config && config_is_read_only(component) && config::Client::new(ChannelTransport { chan: minted }).seal().is_none_or(|answer| answer.is_err()) {
			close(minted);
			return 0;
		}
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
		unsafe { run_tool_under_manifest(self.procsvc, name.as_bytes(), args.as_bytes(), cwd.as_bytes(), &environment, stdout, 0, &mut self.clients, &mut self.audit) }
	}

	// The same launch, and the caller's terminal with it. See the op's own comment in
	// `security.lsidl` for why the terminal travels as a capability rather than as an escape
	// sequence, and why this is an op of its own rather than an optional handle on `run`.
	//
	// THE TERMINAL IS OWNED EXACTLY THE WAY `stdout` IS, and that is deliberate. Both arrive as
	// transfers, both are handed to `run_tool_under_manifest`, and past the point it forwards them
	// to the child they belong to the child - so a caller that closed one on a late failure would be
	// closing a number the kernel may already have reused. The refusal above happens BEFORE that
	// point, which is why it is the one place both are closed here.
	fn run_interactive(&mut self, name: String, args: String, cwd: String, environment: Vec<EnvVar>, stdout: u64, control: u64) -> Result<StartResult, Error> {
		if !environment_is_acceptable(&environment) {
			unsafe {
				close(stdout);
				close(control);
			}
			return Err(Error::Invalid);
		}
		unsafe { run_tool_under_manifest(self.procsvc, name.as_bytes(), args.as_bytes(), cwd.as_bytes(), &environment, stdout, control, &mut self.clients, &mut self.audit) }
	}

	// Start a program over ONE SELECTED FILE, with an attenuated grant in place of the volume
	// bundle its manifest would otherwise give it.
	//
	// THE NARROWING IS THE POINT. A viewer opened on one file holds a client scoped to that path
	// and nothing else - it cannot reopen the file beside it, and it cannot list the directory to
	// find out what those are. `writable` is the difference between "show this" and "edit this",
	// and StorageService enforces it: a read-only grant that asks for a transactional writer is
	// refused there rather than trusted here.
	fn run_with_file(&mut self, name: String, args: String, cwd: String, file: String, writable: bool, stdout: u64) -> Result<StartResult, Error> {
		// THE TARGET MUST BE ONE THE TABLE ADMITS FOR THIS. A grant that could be minted for any
		// program would be a way to hand a file to something that was never meant to receive one,
		// and the closed set is checked here as well as in the caller because the caller is not the
		// thing being trusted.
		if !matches!(name.as_str(), "licoview" | "licoedit" | "imgview" | "play") {
			unsafe { close(stdout) };
			return Err(Error::Denied);
		}
		unsafe { run_tool_over_file(self.procsvc, name.as_bytes(), args.as_bytes(), cwd.as_bytes(), &file, writable, stdout, &mut self.clients, &mut self.audit) }
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
			// EVERY ENDPOINT IS THE TRANSACTION'S FROM HERE. Each edge end, the terminal and the error
			// duplicates are handed to the stage that owns them as it is installed, and the ones a
			// failed transaction never installed are closed by the transaction itself - it is the only
			// thing that knows which numbers were transferred and which are still this broker's.
			// (This used to close every edge on failure, including the ones a stage had already been
			// handed: a consumed handle number, closed again after the grants in between had minted
			// new handles, is somebody else's handle.)
			run_pipeline_under_manifest(self.procsvc, &requests, cwd.as_bytes(), &environment, &mut self.clients, &mut self.audit)
		}
	}
}

// A pipeline may not ask for more stages than the shell grammar can express, so the two
// bounds cannot disagree about what is a legal line.
const MAX_PIPELINE_STAGES: usize = 8;

// ONE LAUNCH TRANSACTION IS ONE CONNECTION TO PROCESSSERVICE.
//
// `procsvc` is the ProcessService client the supervisor minted for this manager, and it is a
// client of a `serve_multi` service - so it can MINT: `service_connect` sends the reserved CONNECT
// request and the service answers with a fresh, independent connection. Every launch transaction
// runs on one of those, from its first prepare to its release, and the connection is closed on
// every path out. `procsvc` itself goes on serving what is not a transaction: the CONNECT that
// mints these, and the `list` that reaps.
//
// The connection IS the transaction, and that is what makes the rollback structural:
//   - ProcessService keys prepared state by the channel it was prepared on, and abandons everything
//     a departed client prepared. A failure this manager can NAME is cancelled through the service
//     - synchronously, so the record and its Domain are gone when this returns - and the connection
//     is closed behind it;
//   - a reply that never comes, or comes back unreadable, is recovered by DROPPING the connection.
//     Nothing more is asked on it: a generated client consumes exactly the next reply on its
//     channel, so asking for status where a late reply may still land would take that reply as the
//     status answer and leave the real one queued to poison the call after it. Dropped, the late
//     reply lands on a dead endpoint, and the service's disconnect cleanup abandons what was
//     prepared;
//   - two transactions at once cannot read each other's replies, because they are two channels;
//   - a pipeline is prepared, sealed and group-released on ONE connection, because `release-group`
//     requires every koid to be the caller's, and two connections are two callers.
//
// NO DEADLINE IS SET ON THESE CALLS, deliberately. ProcessService is the loading mechanism - a
// prepare reads the program off the volume, which on an emulated target takes as long as it takes -
// and a deadline that fired on a slow-but-healthy load would turn a launch into a kill. Its death is
// a closed peer, which is handled; a hang in the one service every launch goes through stalls the
// caller either way. The `TimedOut` ending is still classified below, so a deadline can be added
// without the recovery changing shape.
struct Transaction {
	chan: u64,
	client: process::Client<ChannelTransport>,
	// Every stage prepared on this connection, in order: its start result - whose `task` is the
	// manager's job-control handle - and the manager's end of its bootstrap channel.
	stages: Vec<Stage>,
}

struct Stage {
	started: StartResult,
	manager_side: u64,
}

// How a request on the transaction connection ended without the answer the caller wanted.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
	// The request never reached the service, or the service answered a typed refusal: nothing
	// changed on its side. What this transaction had already prepared is cancelled by name.
	Refused,
	// The reply is lost, malformed, or came from a peer that then went away: the service MAY have
	// acted. Nothing more is asked on this connection - dropping it is the recovery.
	Uncertain,
}

// What a single release came back as.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Release {
	Started,
	// A PRE-START REFUSAL: the service holds no such prepared launch of this transaction's, or the
	// request never left. Nothing ran and there is nothing to recover.
	Refused,
	// A POST-REMOVAL START FAILURE: the token is spent, the service has forgotten the record and
	// its Domain, nothing runs. There is nothing left to cancel and nothing to kill.
	StartFailed,
	// The reply is missing: the program may be running.
	Uncertain,
}

// What a group release came back as.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GroupRelease {
	Committed,
	// A pre-commit refusal: no stage ran.
	Refused,
	// A partial commit: the service took every token, started some members and could not start
	// the rest. The ones that started are running; the ones that did not are forgotten.
	Partial,
	// The reply is missing: any prefix of the group may be running.
	Uncertain,
}

// How long a killed launch is given to confirm its termination before the fault is reported
// anyway. Termination is the kernel's work and takes a tick or two; this is a bound against a
// wedged process, not an expectation.
const RECOVERY_TICKS: u64 = 500;

impl Transaction {
	// Mint the connection. A mint that fails is the cleanest refusal there is: nothing was
	// prepared, nothing ran, and there is no record anywhere to cancel.
	unsafe fn open(procsvc: u64) -> Option<Transaction> {
		let chan: u64 = unsafe { service_connect(procsvc) }?;
		Some(Transaction { chan, client: process::Client::new(ChannelTransport { chan }), stages: Vec::new() })
	}

	// Classify an answer that was not the one wanted. The generated client folds the four transport
	// endings into two typed errors - `again` for a request that never left, `commit-uncertain` for a
	// closed peer, a refused receive and a deadline - and answers `None` for a reply it could not
	// decode, which is as uncertain as no reply at all: bytes arrived and said nothing this caller
	// can act on. A typed service refusal is exactly that, a refusal.
	fn fault<T>(answer: &Option<Result<T, Error>>) -> Fault {
		match answer {
			Some(Err(Error::CommitUncertain)) | None => Fault::Uncertain,
			Some(Err(_)) | Some(Ok(_)) => Fault::Refused,
		}
	}

	// Whether the last request never left this process: the one ending after which a handle the
	// request was carrying is still ours to close.
	fn send_refused(&self) -> bool {
		matches!(self.client.last_error(), Some(proto::codec::TransportError::SendRefused | proto::codec::TransportError::NoRoute))
	}

	// Prepare one stage on this connection and hold its handles until the transaction ends.
	// `child_side` is the bootstrap end the service hands the program; it is transferred by the
	// request and belongs to the service from then on, except when the request never left.
	unsafe fn prepare(&mut self, name: &str, child_side: u64, manager_side: u64, memory_limit: Option<u64>) -> Result<usize, Fault> {
		let answer: Option<Result<StartResult, Error>> = match memory_limit {
			Some(limit) => self.client.launch_prepared_bounded(name, &limit, &child_side),
			None => self.client.launch_prepared(name, &child_side),
		};
		match answer {
			Some(Ok(started)) => {
				self.stages.push(Stage { started, manager_side });
				Ok(self.stages.len() - 1)
			}
			other => {
				let fault = Transaction::fault(&other);
				unsafe {
					if self.send_refused() {
						close(child_side);
					}
					close(manager_side);
				}
				Err(fault)
			}
		}
	}

	fn task(&self, stage: usize) -> u64 {
		self.stages[stage].started.task
	}

	// Release one stage. The transaction goes on holding its handles either way; what the outcome
	// means for them is the caller's decision.
	unsafe fn release(&mut self, stage: usize) -> Release {
		let koid: u64 = self.stages[stage].started.info.koid;
		match self.client.release(&koid) {
			Some(Ok(true)) => Release::Started,
			Some(Ok(false)) => Release::Refused,
			Some(Err(Error::CommitUncertain)) | None => Release::Uncertain,
			Some(Err(_)) if self.send_refused() => Release::Refused,
			Some(Err(_)) => Release::StartFailed,
		}
	}

	// Release every stage as one group.
	unsafe fn release_group(&mut self) -> GroupRelease {
		let koids: Vec<u64> = self.stages.iter().map(|stage| stage.started.info.koid).collect();
		match self.client.release_group(&koids) {
			Some(Ok(true)) => GroupRelease::Committed,
			Some(Ok(false)) => GroupRelease::Refused,
			Some(Err(Error::CommitUncertain)) | None => GroupRelease::Uncertain,
			Some(Err(_)) if self.send_refused() => GroupRelease::Refused,
			Some(Err(_)) => GroupRelease::Partial,
		}
	}

	// Roll the transaction back: close every stage's handles and the connection.
	//
	// The connection closing is what guarantees the cleanup - the service abandons a departed
	// client's prepared launches - and on a `Refused` fault each stage is also cancelled by name
	// first, which is what makes the cleanup SYNCHRONOUS: by the time this returns the record and
	// its Domain are gone rather than pending the service's next turn. On an `Uncertain` fault
	// nothing is asked on the connection at all, because the next reply on it may be the late one.
	unsafe fn abandon(self, fault: Fault) {
		let Transaction { chan, mut client, stages } = self;
		let mut cancelling: bool = fault == Fault::Refused;
		unsafe {
			for stage in stages {
				close(stage.manager_side);
				if cancelling {
					let cancelled = client.cancel(&stage.started.info.koid);
					// A cancel that came back uncertain means the connection cannot be trusted
					// for the next one either; the close below finishes the job.
					if Transaction::fault(&cancelled) == Fault::Uncertain {
						cancelling = false;
					}
				}
				close(stage.started.task);
			}
			close(chan);
		}
	}

	// Commit: the caller takes the stages - their tasks are its job-control handles now - and the
	// connection is closed. Every launch on it was released, so nothing is prepared on it any more
	// and its closing abandons nothing.
	unsafe fn commit(self) -> Vec<Stage> {
		unsafe { close(self.chan) };
		self.stages
	}
}

// A LAUNCH THAT MAY BE RUNNING IS ENDED, and its ending is confirmed.
//
// This is the recovery for a release whose reply was lost: the manager still holds the task, so
// it can `SIG_KILL` the program and wait for the kernel to say it is gone. Returns whether it did
// within the bound; a program that could not be confirmed dead is reported as such rather than
// assumed.
unsafe fn recover_started(task: u64) -> bool {
	unsafe {
		signal(task, SIG_KILL);
		wait(task, clock() + RECOVERY_TICKS) == 0
	}
}

// The same for a pipeline, through the group handle that was minted over its prepared members
// before any of them was released - which is why it exists before a release can go wrong.
unsafe fn recover_group(group: u64) -> bool {
	unsafe {
		process_group_signal(group, SIG_KILL);
		wait(group, clock() + RECOVERY_TICKS) == 0
	}
}

// Ask ProcessService to reap what has ended, on the manager's ordinary client. A killed launch's
// record - and the Domain a bounded one ran in - goes at the service's next reap, and this is that
// reap rather than whenever the next launch happens to trigger one.
unsafe fn reap_through(procsvc: u64) {
	let _ = process::Client::new(ChannelTransport { chan: procsvc }).list();
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
unsafe fn launch_under_manifest(procsvc: u64, component: &[u8], clients: &mut Clients, audit: &mut Vec<AuditEntry>, buf: &mut [u8]) -> Option<Vec<u8>> {
	unsafe {
		let name: String = String::from_utf8_lossy(component).into_owned();
		let mut transaction: Transaction = Transaction::open(procsvc)?;
		let Some((manager_side, child_side)) = channel() else {
			transaction.abandon(Fault::Refused);
			return None;
		};
		// PREPARED, not started: the component is built but does not run until every grant
		// below has been installed. A launch that fails partway is then a process that never
		// ran at all, rather than one that observed half its capabilities and started work on
		// the strength of them.
		let stage: usize = match transaction.prepare(&name, child_side, manager_side, None) {
			Ok(stage) => stage,
			Err(fault) => {
				transaction.abandon(fault);
				return None;
			}
		};
		let task: u64 = transaction.task(stage);
		let Some(policy_name) = executable::logical_name(&transaction.stages[stage].started.info.name).map(String::from) else {
			transaction.abandon(Fault::Refused);
			return None;
		};
		let Some(manifest) = manifest_for(policy_name.as_bytes()) else {
			transaction.abandon(Fault::Refused);
			return None;
		};
		// Grant exactly the manifest's capabilities, auditing every decision. A granted
		// client is duplicated (the manager keeps its own) with only the rights a client
		// needs, then transferred under its tag; a withheld capability is recorded denied
		// and simply never handed over - so the component cannot reach it. A capability the
		// manifest grants and the manager cannot produce is a FAILED LAUNCH, not a program
		// started without the authority it asked for.
		for &cap in VOCABULARY.iter() {
			let granted: bool = manifest.grants.contains(&cap);
			if granted {
				let handle: u64 = grant_for_task(clients, cap, task, &policy_name);
				if handle == 0 || !hand_over(manager_side, tag_for(cap), handle) {
					transaction.abandon(Fault::Refused);
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
		match transaction.release(stage) {
			Release::Started => {}
			Release::Refused | Release::StartFailed => {
				transaction.abandon(Fault::Refused);
				return None;
			}
			Release::Uncertain => {
				recover_started(task);
				transaction.abandon(Fault::Uncertain);
				reap_through(procsvc);
				return None;
			}
		}
		let mut stages: Vec<Stage> = transaction.commit();
		let Stage { started, manager_side } = stages.remove(stage);
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
		close(started.task);
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
			let handle: u64 = grant_handle(clients, cap, &String::from_utf8_lossy(component));
			if handle != 0 && hand_over(manager_side, tag_for(cap), handle) {
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
//
// `stdout` and `control` belong to the child from the moment they are queued on its bootstrap
// channel; a failure after that closes the channel, which drops them with it, and a failure
// before it closes them here. Either way the caller does not close them - see `run_interactive`.
unsafe fn run_tool_under_manifest(procsvc: u64, name: &[u8], args: &[u8], cwd: &[u8], environment: &[EnvVar], stdout: u64, control: u64, clients: &mut Clients, audit: &mut Vec<AuditEntry>) -> Result<StartResult, Error> {
	unsafe {
		// The endpoints are ours until they are queued. Every refusal before that point closes
		// them; every refusal after it closes the channel they were queued on.
		let close_endpoints = |stdout: u64, control: u64| {
			close(stdout);
			if control != 0 {
				close(control);
			}
		};
		let Ok(name_str) = core::str::from_utf8(name) else {
			close_endpoints(stdout, control);
			return Err(Error::NotFound);
		};
		let Some(mut transaction) = Transaction::open(procsvc) else {
			close_endpoints(stdout, control);
			return Err(Error::NotFound);
		};
		let Some((manager_side, child_side)) = channel() else {
			close_endpoints(stdout, control);
			transaction.abandon(Fault::Refused);
			return Err(Error::NotFound);
		};
		// Prepared, never started here: every tool goes through the same gate a pipeline stage
		// does, so the single-stage and multi-stage paths cannot drift in how a process is
		// built. The release is at the bottom, once the whole graph this tool can see exists.
		//
		// THE BOUNDED TOOL IS PREPARED TOO. `imgconv` runs in a Domain of its own with a memory
		// limit; it used to be the one tool that was STARTED first and granted afterwards, because
		// the only bounded launch was a live one - so a grant that failed could not be restored to
		// "the tool never ran". `launch-prepared-bounded` is the same limit behind the same gate.
		let memory_limit: Option<u64> = if name == b"imgconv" { Some(IMGCONV_MEMORY_LIMIT) } else { None };
		let stage: usize = match transaction.prepare(name_str, child_side, manager_side, memory_limit) {
			Ok(stage) => stage,
			Err(fault) => {
				debug_write(name);
				debug_write(b"\n");
				close_endpoints(stdout, control);
				transaction.abandon(fault);
				return Err(Error::NotFound);
			}
		};
		let task: u64 = transaction.task(stage);
		let Some(policy_name) = executable::logical_name(&transaction.stages[stage].started.info.name).map(String::from) else {
			close_endpoints(stdout, control);
			transaction.abandon(Fault::Refused);
			return Err(Error::NotFound);
		};
		let Some(manifest) = manifest_for(policy_name.as_bytes()) else {
			close_endpoints(stdout, control);
			transaction.abandon(Fault::Refused);
			return Err(Error::NotFound);
		};
		// Forward the stdout console first (the tool's `inherit_stdout` reads the first
		// message), then the argument string, then the manifest grants.
		// The launch endpoints, named and ended by READY. A governed tool gets the caller's
		// console, which is full duplex, so it reads and writes the same channel.
		// CHECKED, AND CLOSED ON FAILURE: a prepared launch whose bootstrap end is gone cannot take the
		// console, and a manager that went on would keep the caller's stdout for ever.
		if !hand_over(manager_side, CAP_STDOUT, stdout) {
			if control != 0 {
				close(control);
			}
			transaction.abandon(Fault::Refused);
			return Err(Error::NotFound);
		}
		// AND THE TERMINAL, when this launch is a foreground job on one. `run` passes zero and nothing
		// is sent; `run-interactive` passes the caller's control channel and the child finds it under
		// `CONTROL`, which is what makes `tty_set_mode` answer true instead of false - and a
		// full-screen program that cannot ask runs cooked, with every key meant for it swallowed by
		// the line editor.
		//
		// A NAMED CAPABILITY, so its absence is not a hole in a sequence: the child takes each one by
		// name out of the set this ends with READY, and reads zero for a name that never arrived. That
		// is how a pipeline stage and a background job go on getting no terminal at all.
		if control != 0 && !hand_over(manager_side, CAP_CONTROL, control) {
			transaction.abandon(Fault::Refused);
			return Err(Error::NotFound);
		}
		send_ready(manager_side);
		if !send_launch_context(manager_side, args, cwd, environment) {
			transaction.abandon(Fault::Refused);
			return Err(Error::NotFound);
		}
		// THE PLACEHOLDER IS NOT OPTIONAL. A program that reads `SELECTED_FILE` has to find the tag
		// in a fixed position whether or not it was opened over a file: `recv_tagged` BLOCKS, and a
		// tag read where nothing was sent consumes the message that was actually next and then waits
		// forever for one nobody will send. That is the recorded ordered-bootstrap hazard,
		// and it reappeared here the moment a grant became conditional.
		//
		// Sent only to the programs that READ it, because sending it to the other fifty would shift
		// their sequences by one - the same trap from the other side.
		if reads_selected_file(&policy_name) && !send_blocking(manager_side, CAP_SELECTED_FILE, 0) {
			transaction.abandon(Fault::Refused);
			return Err(Error::NotFound);
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
					let handle: u64 = grant_for_task(clients, cap, task, &policy_name);
					handle != 0 && hand_over(manager_side, tag_for(cap), handle)
				};
				if !ok {
					transaction.abandon(Fault::Refused);
					return Err(Error::NotFound);
				}
			}
			audit.push(AuditEntry { component: policy_name.clone(), capability: cap, granted, dynamic: false });
		}
		// Commit: the tool's stdout, arguments, grants and cwd are all queued, so what it will
		// observe is complete. Anything that failed above returned without releasing, which
		// leaves a process that never ran rather than one that started on half a grant set.
		// AND THE THREE ENDINGS OF A RELEASE ARE THREE ANSWERS. A refusal and a start failure both
		// leave nothing running and are reported as a launch that did not happen; a lost reply is
		// recovered - the program is killed and its ending confirmed - and reported as what it is,
		// a commit whose outcome could not be known, never as a refusal.
		match transaction.release(stage) {
			Release::Started => {}
			Release::Refused | Release::StartFailed => {
				transaction.abandon(Fault::Refused);
				return Err(Error::NotFound);
			}
			Release::Uncertain => {
				recover_started(task);
				transaction.abandon(Fault::Uncertain);
				reap_through(procsvc);
				return Err(Error::CommitUncertain);
			}
		}
		let mut stages: Vec<Stage> = transaction.commit();
		let Stage { started, manager_side } = stages.remove(stage);
		close(manager_side);
		Ok(started)
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
// stdio and grants, SEAL the group over the prepared members, and only then release them
// together.
//
// The ordering is the point. Every stage is created behind the start gate, so a failure at
// ANY stage - an unknown tool, a missing manifest, an endpoint that cannot be transferred -
// returns having released nothing, and no stage has run. A pipeline that half-exists is
// worse than one that does not: stage B reading from a producer that will never be started
// would block forever, and stage A writing into an endpoint nobody will read would too.
//
// Each stage's stdio goes over in ONE message carrying ordered capabilities - stdout first,
// stdin second when it has one - because a receiver cannot tell a second message that was
// never sent from the next handoff in its bootstrap sequence. That is what the handle-migration
// multi-capability work exists for, and it is why this needs no "does this stage have
// stdin?" agreement between the two sides.
//
// THE GROUP IS CREATED BEFORE THE RELEASE, over the prepared members' task handles. It used to
// be minted afterwards, from stages that were already running - so a group-creation failure
// arrived when nothing could be undone, and a group handle that failed to exist left the running
// pipeline with no owner able to signal, wait for or reap it. `sys_process_group_create` asks
// nothing of a member that a prepared process cannot satisfy - a Process handle with MANAGE,
// sealed at creation - so the seal is pre-commit, always: a creation failure is an ordinary
// refusal that cancels every prepared member and starts none, and a release that goes wrong has
// the group handle as its cleanup owner from the first moment anything could be running.
//
// What comes back is the shell's job: the group and the stage count. The refusals are
// `not-found`, and a commit that went wrong and was recovered - some stage may have run - is
// `commit-uncertain`, never relabelled as a refusal.
unsafe fn run_pipeline_under_manifest(procsvc: u64, stages: &[StageRequest], cwd: &[u8], environment: &[EnvVar], clients: &mut Clients, audit: &mut Vec<AuditEntry>) -> Result<PipelineResult, Error> {
	unsafe {
		// The stdio endpoints a stage has not been handed yet are still this broker's, and a
		// transaction that fails closes exactly those: everything from the first stage whose
		// installation did not complete onward. (An endpoint whose send succeeded inside a
		// partially installed stage is a spent number; closing it again here, with nothing
		// allocated in between, is refused by the kernel rather than aimed at somebody else.)
		let close_from = |first: usize| {
			for stage in &stages[first..] {
				close(stage.stdout);
				if stage.stdin != 0 {
					close(stage.stdin);
				}
				if stage.stderr != 0 {
					close(stage.stderr);
				}
			}
		};
		let Some(mut transaction) = Transaction::open(procsvc) else {
			close_from(0);
			return Err(Error::NotFound);
		};
		for (index, stage) in stages.iter().enumerate() {
			let Ok(name_str) = core::str::from_utf8(stage.name) else {
				close_from(index);
				transaction.abandon(Fault::Refused);
				return Err(Error::NotFound);
			};
			let Some((manager_side, child_side)) = channel() else {
				close_from(index);
				transaction.abandon(Fault::Refused);
				return Err(Error::NotFound);
			};
			let prepared: usize = match transaction.prepare(name_str, child_side, manager_side, None) {
				Ok(prepared) => prepared,
				Err(fault) => {
					close_from(index);
					transaction.abandon(fault);
					return Err(Error::NotFound);
				}
			};
			let task: u64 = transaction.task(prepared);
			let Some(policy_name) = executable::logical_name(&transaction.stages[prepared].started.info.name).map(String::from) else {
				close_from(index);
				transaction.abandon(Fault::Refused);
				return Err(Error::NotFound);
			};
			let Some(manifest) = manifest_for(policy_name.as_bytes()) else {
				close_from(index);
				transaction.abandon(Fault::Refused);
				return Err(Error::NotFound);
			};
			// stdout, then stdin when there is one, as ordered capabilities in one message.
			// A stage writes into one edge and reads from another, so the two endpoints are
			// named separately rather than told apart by how many arrived.
			let installed: bool = send_blocking(manager_side, CAP_STDOUT, stage.stdout) && (stage.stdin == 0 || send_blocking(manager_side, CAP_STDIN, stage.stdin)) && (stage.stderr == 0 || send_blocking(manager_side, CAP_STDERR, stage.stderr)) && send_ready(manager_side);
			if !installed {
				close_from(index);
				transaction.abandon(Fault::Refused);
				return Err(Error::NotFound);
			}
			if !send_launch_context(manager_side, stage.args, cwd, environment) {
				close_from(index + 1);
				transaction.abandon(Fault::Refused);
				return Err(Error::NotFound);
			}
			for &cap in VOCABULARY.iter() {
				let granted: bool = manifest.grants.contains(&cap);
				if granted {
					let ok: bool = if cap == Capability::Volumes {
						grant_volumes(manager_side, clients)
					} else {
						let handle: u64 = grant_for_task(clients, cap, task, &policy_name);
						handle != 0 && hand_over(manager_side, tag_for(cap), handle)
					};
					if !ok {
						close_from(index + 1);
						transaction.abandon(Fault::Refused);
						return Err(Error::NotFound);
					}
				}
				audit.push(AuditEntry { component: policy_name.clone(), capability: cap, granted, dynamic: false });
			}
		}
		// SEAL, over the prepared members. Each task carries MANAGE - it is the same handle the
		// caller signals with - and membership is fixed at creation, so the group is the one
		// object that can end the whole pipeline whatever the release below does.
		let tasks: Vec<u64> = transaction.stages.iter().map(|stage| stage.started.task).collect();
		let group: i64 = process_group_create(&tasks);
		if group < 0 {
			transaction.abandon(Fault::Refused);
			return Err(Error::NotFound);
		}
		let group: u64 = group as u64;
		// COMMIT, IN ONE TRANSITION (IDL-002).
		//
		// `release-group` is the primitive that makes "a failure at any stage starts none of them"
		// true: ProcessService checks every token first - every koid prepared, and every one of
		// them this connection's - and only then queues them. A refusal therefore comes back with
		// nothing started, and this can return having run nothing at all rather than having
		// half-run a pipeline.
		let count: u32 = transaction.stages.len() as u32;
		match transaction.release_group() {
			GroupRelease::Committed => {
				// The broker's own copies of the edge endpoints are spent: each was transferred to
				// the stage that owns it, and holding a duplicate here would keep a pipe open after
				// its writer exits, so the reader would never see end-of-stream. The tasks go too:
				// the group is the job-control handle now.
				for stage in transaction.commit() {
					close(stage.manager_side);
					close(stage.started.task);
				}
				Ok(PipelineResult { group, stages: count })
			}
			GroupRelease::Refused => {
				// Nothing ran, so there is nothing to signal - only the handles this transaction
				// opened, which go back the way a refused preparation's do.
				close(group);
				transaction.abandon(Fault::Refused);
				Err(Error::NotFound)
			}
			GroupRelease::Partial => {
				// The service took every token: some members are running and the rest are
				// forgotten. What is running is ended through the group, and the fault is
				// reported as what it is rather than as a refusal - a stage may have written,
				// sent or printed before the kill reached it.
				recover_group(group);
				close(group);
				transaction.abandon(Fault::Refused);
				reap_through(procsvc);
				Err(Error::CommitUncertain)
			}
			GroupRelease::Uncertain => {
				// The reply is lost, so any prefix may be running: the group ends it, dropping
				// the connection abandons whatever was still prepared, and nothing more is asked
				// on the connection the late reply may still land on.
				recover_group(group);
				close(group);
				transaction.abandon(Fault::Uncertain);
				reap_through(procsvc);
				Err(Error::CommitUncertain)
			}
		}
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

// Start a tool over ONE selected file.
//
// The same launch as `run_tool_under_manifest` with one substitution: where that hands over the
// whole volume bundle, this mints a client scoped to a single path and hands that over instead,
// beside a record naming what was opened. Every other grant in the tool's manifest is unchanged -
// a viewer still gets its syntax descriptors, a player still gets its audio stream - because
// narrowing the file authority is not a reason to take away the rest.
//
// A FAILURE TO MINT THE GRANT ENDS THE LAUNCH. Starting the program without it would give it a
// process, a terminal and no file, which is a window with nothing in it and no way to say why.
#[allow(clippy::too_many_arguments)]
unsafe fn run_tool_over_file(procsvc: u64, name: &[u8], args: &[u8], cwd: &[u8], file: &str, writable: bool, stdout: u64, clients: &mut Clients, audit: &mut Vec<AuditEntry>) -> Result<StartResult, Error> {
	unsafe {
		if clients.storage_admin == 0 {
			close(stdout);
			return Err(Error::NotFound);
		}
		let Ok(name_str) = core::str::from_utf8(name) else {
			close(stdout);
			return Err(Error::NotFound);
		};
		let Some(mut transaction) = Transaction::open(procsvc) else {
			close(stdout);
			return Err(Error::NotFound);
		};
		let Some((manager_side, child_side)) = channel() else {
			close(stdout);
			transaction.abandon(Fault::Refused);
			return Err(Error::NotFound);
		};
		let stage: usize = match transaction.prepare(name_str, child_side, manager_side, None) {
			Ok(stage) => stage,
			Err(fault) => {
				close(stdout);
				transaction.abandon(fault);
				return Err(Error::NotFound);
			}
		};
		let task: u64 = transaction.task(stage);
		let Some(policy_name) = executable::logical_name(&transaction.stages[stage].started.info.name).map(String::from) else {
			close(stdout);
			transaction.abandon(Fault::Refused);
			return Err(Error::NotFound);
		};
		let Some(manifest) = manifest_for(policy_name.as_bytes()) else {
			close(stdout);
			transaction.abandon(Fault::Refused);
			return Err(Error::NotFound);
		};
		if !hand_over(manager_side, CAP_STDOUT, stdout) {
			transaction.abandon(Fault::Refused);
			return Err(Error::NotFound);
		}
		send_ready(manager_side);
		if !send_launch_context(manager_side, args, cwd, &[]) {
			transaction.abandon(Fault::Refused);
			return Err(Error::NotFound);
		}
		// THE ATTENUATED GRANT, minted from the private admin endpoint the manager holds and grants
		// to nobody - the thing that hands out a narrowed authority must not itself be one of the
		// things handed out.
		let minted: u64 = match volume_admin::Client::new(ChannelTransport { chan: clients.storage_admin }).open_file(file, &writable) {
			Some(Ok(client)) => client,
			_ => {
				transaction.abandon(Fault::Refused);
				return Err(Error::NotFound);
			}
		};
		let granted: i64 = duplicate(minted, GRANT_RIGHTS);
		close(minted);
		if granted < 0 || !hand_over(manager_side, CAP_SELECTED_FILE, granted as u64) {
			transaction.abandon(Fault::Refused);
			return Err(Error::NotFound);
		}
		// The record travels AFTER the capability and says what the capability cannot: which URI to
		// open through it, what to show a person, and whether a write-back will be accepted.
		let selected = SelectedFile { uri: String::from(file), name: String::from(last_path_component(file)), writable };
		let Some(bytes) = selected.encode_vec() else {
			transaction.abandon(Fault::Refused);
			return Err(Error::NotFound);
		};
		if !send_blocking(manager_side, &bytes, 0) {
			transaction.abandon(Fault::Refused);
			return Err(Error::NotFound);
		}
		// EVERY OTHER GRANT IS UNCHANGED, except `volumes` - which is exactly what the selected
		// file replaced. The audit records the substitution rather than hiding it.
		for &cap in VOCABULARY.iter() {
			if cap == Capability::Volumes {
				audit.push(AuditEntry { component: policy_name.clone(), capability: cap, granted: false, dynamic: true });
				continue;
			}
			let want: bool = manifest.grants.contains(&cap);
			if want {
				let handle: u64 = grant_for_task(clients, cap, task, &policy_name);
				if handle == 0 || !hand_over(manager_side, tag_for(cap), handle) {
					transaction.abandon(Fault::Refused);
					return Err(Error::NotFound);
				}
			}
			audit.push(AuditEntry { component: policy_name.clone(), capability: cap, granted: want, dynamic: false });
		}
		match transaction.release(stage) {
			Release::Started => {}
			Release::Refused | Release::StartFailed => {
				transaction.abandon(Fault::Refused);
				return Err(Error::NotFound);
			}
			Release::Uncertain => {
				recover_started(task);
				transaction.abandon(Fault::Uncertain);
				reap_through(procsvc);
				return Err(Error::CommitUncertain);
			}
		}
		let mut stages: Vec<Stage> = transaction.commit();
		let Stage { started, manager_side } = stages.remove(stage);
		close(manager_side);
		Ok(started)
	}
}

// Which programs read a `SELECTED_FILE` tag out of their bootstrap, and therefore must be sent one
// - bare when they were not opened over a file. The same closed set `run-with-file` admits, named
// once so the sender and the check cannot disagree about who expects the message.
fn reads_selected_file(name: &str) -> bool {
	matches!(name, "licoview" | "licoedit")
}

// The last component of a URI, which is the name to show a person. A grant over
// `vol://system/notes.txt` is presented as `notes.txt`.
fn last_path_component(uri: &str) -> &str {
	match uri.rfind('/') {
		Some(at) => &uri[at + 1..],
		None => uri,
	}
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
// HAND ONE CAPABILITY TO A PREPARED LAUNCH, OR CLOSE IT. `send_blocking` leaves the handle in this
// process's table when the send fails - the prepared launch's bootstrap end is gone, or the queue
// cannot take it - so a hand-over that only reads the result LEAKS the handle on exactly the path
// that then abandons the launch. The fault cohort counted it: one handle more in the manager's
// Domain after a bounded launch whose grant could not be delivered. A zero handle is a placeholder
// tag with nothing to close. On failure the caller owns nothing it did not send.
unsafe fn hand_over(manager_side: u64, tag: &[u8], handle: u64) -> bool {
	unsafe {
		if send_blocking(manager_side, tag, handle) {
			return true;
		}
		if handle != 0 {
			close(handle);
		}
		false
	}
}

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
			if !hand_over(manager_side, tag, minted) {
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
		let started: StartResult = match run_tool_under_manifest(procsvc, name, args, b"", &[], console, 0, clients, audit) {
			Ok(s) => s,
			Err(_) => {
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
	// The operator's WRITE. Optional: a boot that granted none has no operator path rather than a
	// half-built one, and `for_capability` answering 0 refuses the grant by itself.
	let device_policy: u64 = caps.take(CAP_DEVPOLICY);
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
	// The admin endpoint scoped grants are minted from. Absent on a minimal boot, which makes
	// `app-assets` a capability the manager records as denied rather than one it cannot describe.
	let storage_admin: u64 = caps.take(CAP_STORAGE_ADMIN);
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
	let usb_catalogue = caps.take(b"CATALOGUE");
	let usb_providers = ProviderWatch::subscribe(usb_catalogue, ProviderKind::UsbBus);
	// Mint the manager's self-connection: a dedicated channel pair whose server end is seeded
	// into the serve set below (so requests on it are dispatched like any other client's) and
	// whose client end the manager holds as the grantable `permission` capability. The governed
	// `perm` command thus reaches the very audit trail this manager serves over a connection of
	// its own - a capability the manager grants to a copy of itself, on a dedicated channel so a
	// granted tool's queries never race the supervisor's own connection.
	let (perm_self_server, perm_self_client): (u64, u64) = unsafe { channel() }.unwrap_or_else(|| unsafe { fail_bootstrap(bootstrap, b"channel", b"could not mint self-connection") });
	let mut clients: Clients = Clients { log, storage, network, time, config, device, device_policy, audio, input: 0, graph: 0, resource, process, permission: perm_self_client, supervisor, services, usb_catalogue, usb_providers, storage_media, storage_iso, storage_udf, storage_usb, storage_ram, storage_tmp, display_admin, input_admin, audio_admin, session, storage_admin, broker: bootstrap };
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
