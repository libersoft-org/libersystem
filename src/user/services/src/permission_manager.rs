// permission_manager - the userspace permission-policy manager (PermissionManager).
//
// PermissionManager is the policy over the kernel's capability mechanism. ServiceManager
// starts it from the init package and hands it the clients it is allowed to grant onward
// (a StorageService, a LogService, and a NetworkService client), the init package (so it
// can launch the components it governs), and a "SERVE" channel its clients reach it on.
//
// Its policy is a typed permission manifest per component - a `Manifest` of `Capability`
// grants, the typed source of truth for what a component may be given (never a text or
// JSON file). When it launches a component it grants that component exactly its manifest's
// capabilities and nothing else - the strict app sandbox - and records every decision
// (grant or denial) in an audit trail. Over the SERVE channel callers speak the generated
// `liber:system` Permission bindings: `lookup` returns a component's manifest, `audit`
// returns the trail.
//
// This milestone it launches one governed component, sandbox_probe, whose manifest grants
// storage and log but not network. The probe reaches exactly those two capabilities and
// reports back the file it read through its storage grant; the manager relays that proof
// and a decisions summary to the supervisor, then serves the Permission contract until the
// supervisor drops its bootstrap channel.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::system::permission::{self, Service};
use proto::system::{AuditEntry, Capability, Error, Manifest};
use rt::*;

// The governed component this milestone launches, and the rights a granted client is
// duplicated with before it is transferred (send + receive + wait + transfer onward - the
// set a service client needs, never more than the manager itself holds).
const PROBE_NAME: &[u8] = b"sandbox_probe";
const GRANT_RIGHTS: u32 = RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER;

// The full grantable vocabulary, in the fixed order the manager evaluates a manifest: for
// each, it grants the held client if the manifest lists the capability, or records a denial
// if not. This is also the order a launched component receives its grants in.
const VOCABULARY: [Capability; 3] = [Capability::Storage, Capability::Log, Capability::Network];

// The manager's policy: the permission manifest declared for each component it governs -
// the typed source of truth for what that component may be granted.
fn manifest_for(component: &[u8]) -> Option<Manifest> {
	match component {
		b"sandbox_probe" => Some(Manifest { component: String::from("sandbox_probe"), grants: alloc::vec![Capability::Storage, Capability::Log] }),
		_ => None,
	}
}

// The bootstrap tag a granted capability's client is transferred under - matched by the
// launched component's receive order.
fn tag_for(cap: Capability) -> &'static [u8] {
	match cap {
		Capability::Log => b"LOG",
		Capability::Storage => b"STORAGE",
		Capability::Network => b"NETWORK",
	}
}

// The grantable clients the manager holds and may hand onward (0 = not granted to it).
struct Clients {
	log: u64,
	storage: u64,
	network: u64,
}

impl Clients {
	// The held client for a grantable capability.
	fn for_capability(&self, cap: Capability) -> u64 {
		match cap {
			Capability::Log => self.log,
			Capability::Storage => self.storage,
			Capability::Network => self.network,
		}
	}
}

// The manager's serve state. The manifest table is fixed policy (served read-only by
// `lookup`); the audit trail is the mutable record of every grant decision made.
struct Manager {
	audit: Vec<AuditEntry>,
}

impl Service for Manager {
	fn lookup(&mut self, component: String) -> Result<Manifest, Error> {
		manifest_for(component.as_bytes()).ok_or(Error::NotFound)
	}
	fn audit(&mut self) -> Result<Vec<AuditEntry>, Error> {
		Ok(self.audit.clone())
	}
}

// Launch a component under its permission manifest: look it up in the init package, spawn
// it with a fresh bootstrap channel, then for every capability in the vocabulary grant the
// held client if the manifest lists it (recording the grant) or withhold it (recording the
// denial). The component receives exactly its manifest's capabilities, in vocabulary order,
// and can reach nothing else - the sandbox. Returns the bytes the component reported back
// (its proof the granted storage capability is live), or None if the launch failed.
unsafe fn launch_under_manifest(package: &Package, component: &[u8], clients: &Clients, audit: &mut Vec<AuditEntry>, buf: &mut [u8]) -> Option<Vec<u8>> {
	unsafe {
		let manifest: Manifest = manifest_for(component)?;
		let elf: &[u8] = package.lookup(component)?;
		let (manager_side, child_side): (u64, u64) = channel()?;
		let proc: i64 = spawn(elf, child_side);
		if proc < 0 {
			return None;
		}
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
					close(proc as u64);
					return None;
				}
			}
			audit.push(AuditEntry { component: String::from_utf8_lossy(component).into_owned(), capability: cap, granted });
		}
		// Wait for the component's report - the bytes it read through its storage grant -
		// so the launch is complete and bounded before the manager moves on.
		let result: Option<Vec<u8>> = match recv_blocking(manager_side, buf) {
			Received::Message { len, .. } => Some(buf[..len].to_vec()),
			Received::Closed => None,
		};
		close(manager_side);
		close(proc as u64);
		result
	}
}

// Build the human-readable decisions summary from the audit trail - one `cap=grant` or
// `cap=deny` token per recorded decision, in order. The supervisor relays this as the
// manager's proof of exactly which capabilities the launched component was and was not
// given; the typed trail itself is served verbatim over the Permission contract.
fn summarize(audit: &[AuditEntry]) -> Vec<u8> {
	let mut out: String = String::new();
	for (i, e) in audit.iter().enumerate() {
		if i != 0 {
			out.push(' ');
		}
		out.push_str(&e.capability.to_text());
		out.push('=');
		out.push_str(if e.granted { "grant" } else { "deny" });
	}
	out.into_bytes()
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 512] = [0u8; 512];

	// 1. receive the grantable clients the manager may hand onward, then the init package
	//    it launches governed components from. A client the supervisor does not grant
	//    arrives as 0 (the manager simply cannot grant what it does not hold).
	let storage: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"STORAGE") }.unwrap_or(0);
	let log: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"LOG") }.unwrap_or(0);
	let network: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"NETWORK") }.unwrap_or(0);
	let clients: Clients = Clients { log, storage, network };
	let (_pkg_handle, archive): (u64, &[u8]) = unsafe { recv_package(bootstrap, &mut buf) }.unwrap_or_else(|| exit());
	let package: Package = Package::parse(archive).unwrap_or_else(|| exit());

	// 2. wait for the serve channel clients reach us on.
	let service: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"SERVE") }.unwrap_or_else(|| exit());

	// 3. launch the governed component under its manifest, building the audit trail.
	let mut audit: Vec<AuditEntry> = Vec::new();
	let read_back: Vec<u8> = unsafe { launch_under_manifest(&package, PROBE_NAME, &clients, &mut audit, &mut buf) }.unwrap_or_default();

	// 4. report in to the supervisor, then relay the sandbox proof: the bytes the
	//    sandboxed component read through its one granted storage capability, and the
	//    decisions summary (exactly which capabilities it was and was not given).
	unsafe {
		send_blocking(bootstrap, b"PermissionManager: online", 0);
		send_blocking(bootstrap, &read_back, 0);
		send_blocking(bootstrap, &summarize(&audit), 0);
	}

	// 5. serve generated lookup/audit requests until the supervisor drops the channel.
	let mut manager: Manager = Manager { audit };
	let mut request: [u8; 512] = [0u8; 512];
	let mut reply: [u8; 1024] = [0u8; 1024];
	unsafe {
		serve_multi(service, &mut request, &mut reply, |_chan: u64, req: &[u8], handle: u64, out: &mut [u8], reply_handle: &mut u64| -> Option<usize> { permission::dispatch(&mut manager, req, handle, out, reply_handle) });
	}
	exit();
}
