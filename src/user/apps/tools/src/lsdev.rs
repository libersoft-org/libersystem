// lsdev - list the system's device nodes, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it exactly
// one capability - a DeviceService client - and forwards it the shell's stdout console and
// the argument string (the sub-form: "" for text or "json"). lsdev lists the device nodes
// through its grant and prints each entry (as text or JSON) to the inherited stdout, then
// exits. A standalone command, not a shell built-in: it reaches the service only through the
// one capability the permission store granted it, and renders on the same terminal as the
// shell that launched it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use device_client::DeviceClient;
use device_client::DevicePolicyClient;
use proto::codec::JsonMode;
use proto::system::{LaunchContext, PolicyOutcome, PolicyVerb};
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our output
		//    renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string - the sub-form ("" for text, "json" /
		//    "json-min" for JSON).
		let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
			Some(context) => context,
			None => exit(),
		};
		let args: Vec<u8> = context.arguments.clone().into_bytes();
		// 3. receive the two capabilities the manifest grants: the READ - a DeviceService client -
		//    and the WRITE, the operator's device-policy endpoint. Two, because nothing that renders
		//    a device list needs the second, and holding the first gets a component no closer to it.
		let devsvc: u64 = recv_tagged(bootstrap, &mut buf, b"DEVICE").unwrap_or_else(|| exit());
		let policy: u64 = recv_tagged(bootstrap, &mut buf, b"DEVPOLICY").unwrap_or(0);
		// A verb, or the listing. `lsdev` with no verb reads and changes nothing, which is what a
		// command called `ls` had better do.
		if let Some(request) = parse_verb(&args) {
			apply_verb(policy, request);
			exit();
		}
		query_devices(devsvc, JsonMode::parse(&args));
	}
	exit();
}

// List the device nodes through the grant and print each entry, as text (the default) or as
// a JSON array, rendered on the client side - reporting a concise error if the query fails.
// What the operator asked for on the command line.
struct VerbRequest {
	index: u32,
	verb: PolicyVerb,
	artifact: String,
}

// `--disable N`, `--enable N`, `--retry N`, `--select N ARTIFACT`. None for a plain listing.
//
// FOUR VERBS AND NOT THREE. The fourth arrived because a state nothing can leave is not a policy -
// it is a way to lose a device until the next boot.
fn parse_verb(args: &[u8]) -> Option<VerbRequest> {
	let text = core::str::from_utf8(args).ok()?;
	let mut parts = text.split_whitespace();
	let verb = match parts.next()? {
		"--disable" => PolicyVerb::Disable,
		"--enable" => PolicyVerb::Enable,
		"--select" => PolicyVerb::Select,
		"--retry" => PolicyVerb::Retry,
		// THE DISPLAY, not a verb: it changes nothing and is here because it asks the same
		// endpoint. `--incident N` prints the bounded capture P02M0165 took.
		"--incident" => {
			let index: u32 = parts.next()?.parse().ok()?;
			return Some(VerbRequest { index, verb: PolicyVerb::Retry, artifact: String::from("\u{0}incident") });
		}
		_ => return None,
	};
	let index: u32 = parts.next()?.parse().ok()?;
	// `select` is the only one that names an artifact, and it must: a preference with nothing
	// preferred is not a preference.
	let artifact = match verb {
		PolicyVerb::Select => String::from(parts.next()?),
		_ => String::new(),
	};
	Some(VerbRequest { index, verb, artifact })
}

// Apply one verb and say what happened, in the words the outcome carries.
unsafe fn apply_verb(policy: u64, request: VerbRequest) {
	unsafe {
		if policy == 0 {
			eprint(b"lsdev: this boot granted no device-policy authority\n");
			return;
		}
		let mut client = DevicePolicyClient::new(policy);
		// The display path, marked by a sentinel the command line cannot produce.
		if request.artifact.starts_with('\u{0}') {
			match client.incident(&request.index) {
				Some(Ok(report)) => {
					if report.present {
						print(report.to_text().as_bytes());
					} else {
						print(b"nothing has gone wrong on this binding");
					}
					print(b"\n");
				}
				Some(Err(_)) => eprint(b"lsdev: no device has that index\n"),
				None => eprint(b"lsdev: the device policy endpoint did not answer\n"),
			}
			return;
		}
		match client.apply(&request.index, &request.verb, &request.artifact) {
			Some(Ok(outcome)) => {
				print(outcome_text(outcome));
				print(b"\n");
			}
			Some(Err(_)) => eprint(b"lsdev: the device policy endpoint refused the request\n"),
			None => eprint(b"lsdev: the device policy endpoint did not answer\n"),
		}
	}
}

// WHAT AN OUTCOME MEANS, in a sentence rather than a number. Each of these is a refusal an operator
// can act on, which is the whole reason they are separate values.
fn outcome_text(outcome: PolicyOutcome) -> &'static [u8] {
	match outcome {
		PolicyOutcome::Accepted => b"accepted",
		PolicyOutcome::NoSuchDevice => b"no device has that index",
		PolicyOutcome::NotACandidate => b"the registry never declared that driver for this device - policy narrows and never widens",
		PolicyOutcome::Quarantined => b"this device is quarantined: nothing confirmed it went quiet, and saying so does not make it so. disable, enable and select are still accepted; they apply at the next bind",
		PolicyOutcome::Busy => b"a disable is still tearing this binding down; try again once it reads as disabled",
		PolicyOutcome::Refused => b"this binding is boot-critical, and its policy would live on a volume that is not mounted when it is made",
		PolicyOutcome::NotStored => b"the preference could not be written down, so nothing was changed",
	}
}

unsafe fn query_devices(devsvc: u64, mode: Option<JsonMode>) {
	unsafe {
		let mut client = DeviceClient::new(devsvc);
		// THE BINDINGS FIRST, because they are what this command is for: which driver a device got,
		// under which rule, in which state and why. The device table alone says what hardware is
		// present, which is the smaller half.
		//
		// An older DeviceManager, or a boot that granted no catalogue connection, answers an error
		// rather than an empty list - and an empty list would read as "nothing is bound", which is a
		// different claim about the machine.
		match client.bindings() {
			Some(Ok(records)) => {
				if let Some(mode) = mode {
					let mut out = String::from("[");
					for (i, r) in records.iter().enumerate() {
						if i > 0 {
							out.push(',');
						}
						out.push_str(&r.to_json());
					}
					out.push(']');
					print(mode.render(out).as_bytes());
					print(b"\n");
					exit();
				}
				for r in &records {
					print(r.to_text().as_bytes());
					print(b"\n");
				}
			}
			Some(Err(_)) => eprint(b"lsdev: this system does not answer the binding query; showing the device table only\n"),
			None => eprint(b"lsdev: service unavailable\n"),
		}
		match client.list() {
			Some(Ok(entries)) => {
				if let Some(mode) = mode {
					let mut out = String::from("[");
					for (i, e) in entries.iter().enumerate() {
						if i > 0 {
							out.push(',');
						}
						out.push_str(&e.to_json());
					}
					out.push(']');
					print(mode.render(out).as_bytes());
					print(b"\n");
				} else {
					for e in &entries {
						print(e.to_text().as_bytes());
						print(b"\n");
					}
				}
			}
			Some(Err(_)) => eprint(b"lsdev: query error\n"),
			None => eprint(b"lsdev: service unavailable\n"),
		}
	}
}
