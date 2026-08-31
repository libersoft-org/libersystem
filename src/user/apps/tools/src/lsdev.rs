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
use config_client::ConfigClient;
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
		// AND THE READ THAT OUTLIVES THE MANAGER. Not a third authority over devices: a client of
		// ConfigService, where DeviceManager persists each incident, for the case where the endpoint
		// above cannot answer because the process holding it is gone.
		let config: u64 = recv_tagged(bootstrap, &mut buf, b"CONFIG").unwrap_or(0);
		// A verb, or the listing. `lsdev` with no verb reads and changes nothing, which is what a
		// command called `ls` had better do.
		if let Some(request) = parse_verb(&args) {
			apply_verb(policy, config, devsvc, request);
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
unsafe fn apply_verb(policy: u64, config: u64, devsvc: u64, request: VerbRequest) {
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
				// THE MANAGER IS GONE, WHICH IS WHEN THIS QUESTION MATTERS MOST.
				//
				// The live report is held by DeviceManager and dies with it - and DeviceManager
				// dying is what killed the driver subtree, so an operator asking what happened is
				// asking a process that is no longer there. The persisted copy is in ConfigService,
				// which survives, and it carries the whole record rather than a summary of it.
				None => stored_incident(config, devsvc, request.index),
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

// THE ADDRESS OF THE DEVICE AT ROW `index`, if the stored map can be CHECKED against this machine.
//
// `None` means the question cannot be answered by row here: no map, no device at that row, or a map
// whose recorded MMIO length disagrees with what the kernel reports for that row now - which is a map
// written by a boot with a different inventory. See `stored_incident`.
fn resolved_address(client: &mut ConfigClient, devsvc: u64, index: u32) -> Option<String> {
	if devsvc == 0 {
		return None;
	}
	let mut key = String::from("device.policy.incident-at.");
	let mut digits = [0u8; 20];
	let written = decimal(index as u64, &mut digits);
	key.push_str(core::str::from_utf8(&digits[..written]).ok()?);
	let Some(Ok(stored)) = client.get(&key) else { return None };
	// `<mmio-len>.<bus>.<dev>.<func>` - the length first because it is the part that is checked.
	let (len, address) = stored.split_once('.')?;
	let recorded: u64 = len.parse().ok()?;
	let entry = DeviceClient::new(devsvc).get(&index)?.ok()?;
	if entry.mmio_len != recorded {
		return None;
	}
	Some(String::from(address))
}

// One decimal number into `out`, answering how many bytes it wrote. This program has no formatter.
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
	for i in 0..at {
		out[i] = digits[at - 1 - i];
	}
	at
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

// THE INCIDENT AS CONFIGSERVICE HOLDS IT, for when the manager that served the live one is gone.
//
// DeviceManager writes each incident under `device.policy.incident.<bus>.<dev>.<func>` with the
// whole record - cause, state, generation, attempts, last opcode, silence, the device's address and
// the Domain's counters. It used to write a summary of that, so the persisted copy could not answer
// what the live endpoint answered and "the snapshot outlives the manager" was true of a summary.
//
// KEYED BY ADDRESS, NOT BY INDEX, because the index is the kernel's row number for THIS boot and the
// address is what the record is about. That is why this path cannot answer a question asked by row
// number: the inventory that gives a row its meaning is DeviceManager's, and reaching this code
// means DeviceManager is gone. It says so rather than matching a number that named a different
// device in the boot that wrote it.

unsafe fn stored_incident(config: u64, devsvc: u64, index: u32) {
	unsafe {
		if config == 0 {
			eprint(b"lsdev: the device policy endpoint did not answer, and this boot granted no configuration read to look for the stored copy\n");
			return;
		}
		let mut client = ConfigClient::new(config);
		// THE ROW NUMBER IS RESOLVED AGAINST THE HARDWARE, NOT AGAINST A REMEMBERED LIST.
		//
		// DeviceManager writes a separate `incident-at.<row>` record carrying the length of that
		// device's MMIO window beside its address. `DeviceService` answers that length for the same
		// row out of the KERNEL's device table, which is there whether DeviceManager is or not - so
		// the map is checked rather than believed, and a row whose stored length does not match what
		// the machine reports for it now is a map from another inventory and is refused.
		//
		// This is what makes `--incident N` answerable on the path where the manager is gone. Listing
		// everything by address, below, is the fallback for when the check cannot be made or fails.
		if let Some(at) = resolved_address(&mut client, devsvc, index) {
			let mut key = String::from("device.policy.incident.");
			key.push_str(&at);
			match client.get(&key) {
				Some(Ok(value)) => {
					print(value.as_bytes());
					print(b"\n");
					eprint(b"lsdev: DeviceManager did not answer - this is the persisted copy for the device at that row\n");
					return;
				}
				// The row resolves and has no record: the honest answer, and the same one the live
				// endpoint gives for a binding that has never failed.
				Some(Err(_)) | None => {
					print(b"nothing has gone wrong on this binding\n");
					eprint(b"lsdev: DeviceManager did not answer - no incident is stored for the device at that row\n");
					return;
				}
			}
		}
		// A ROW NUMBER CANNOT BE RESOLVED WITHOUT THE MANAGER, AND THIS PRETENDED IT COULD
		// (corrected 2026-08-30).
		//
		// The record is KEYED by the device's address, which is its identity across boots. It also
		// carried the row number of the boot that wrote it, and this matched on that - so a record
		// written when row 3 was the NIC answered a question about row 3 in a boot where that row is
		// the audio controller, and several old records could match one row. The index is gone from
		// the record for that reason.
		//
		// What is left is honest: this path runs only when DeviceManager is gone, and the row number
		// is a position in the inventory DeviceManager serves. So the number cannot be turned into a
		// device here, and rather than guess, this says so and prints every stored incident BY ITS
		// ADDRESS - which is the durable name the operator can match against `lspci`. Where the
		// manager is alive the question is answered by index, from the live inventory that gives the
		// number meaning, and this path is not reached.
		eprint(
			b"lsdev: DeviceManager is gone, so a row number cannot be resolved to a device - the stored incidents are listed by the address that identifies them across boots
",
		);
		let _ = index;
		let mut found = false;
		match client.list() {
			Some(Ok(entries)) => {
				for entry in entries.iter() {
					// FILTERED HERE, because `list` answers with the whole tree and the incidents
					// are one prefix of it. An operator asking what went wrong is not asking for
					// every configuration value this system holds.
					if !entry.key.starts_with("device.policy.incident.") {
						continue;
					}
					found = true;
					print(entry.key.as_bytes());
					print(b"  ");
					print(entry.value.as_bytes());
					print(b"\n");
				}
			}
			_ => {
				eprint(b"lsdev: the device policy endpoint did not answer and the stored copy could not be read either\n");
				return;
			}
		}
		if !found {
			eprint(b"lsdev: the device policy endpoint did not answer, and no incident is stored for any device\n");
			return;
		}
		eprint(b"lsdev: DeviceManager did not answer - the records above are the persisted copies, each keyed by the address of the device it belongs to\n");
	}
}
