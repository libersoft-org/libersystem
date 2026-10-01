// tpmprobe - the TPM gate's other component. DEVELOPMENT-ONLY.
//
// It holds the observation and sealing grants and the files, and NOT measurement - which is its half of the gate:
//   `extend23`   an extend of PCR 23, a PCR `pcr-extend` accepts, refused `not-granted` TWICE - by `tpm-client` in
//                this process, since no `tpm-measure` grant was minted for it, and by TpmService when the same
//                generated client sends it on the observation connection, which only the service can answer - and
//                PCR 23 read unchanged around both;
//   `other FILE` the `tpm` tool's sealed object, which the TPM would open and TpmService refuses `other-component`;
//   `own`        a secret sealed to PCR 16 and unsealed, its own;
//   `hold`       ONE `tpm` connection held across a restart of the driver, `random` asked on it every fifth of a
//                second, and each change of answer printed: a value, `unavailable`, `interrupted`.

//
// A STATIC PROBE IN THIS CRATE, behind `required-features = ["development"]` as every other development-only program is,
// so a shipping build never compiles it - it links `tpm-client` statically, which exercises the same library code a
// dynamic program reaches through `tpm-client.lslib`.

#![no_std]
#![no_main]

extern crate alloc;
// THE TRAMPOLINES `tpm-client` CALLS THROUGH, linked into this static program: nothing in Rust names the crate, so it is
// named here, or the linker never sees the symbols it defines.
extern crate tpm_client_provider;

use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::path;
use proto::system::{LaunchContext, OpenOpts, Outcome, volume};
use rt::*;
use tpm_client::TpmClient;

// How long `hold` holds on: past the scenario's disable, the driver's absence and its enable.
const HOLD_TICKS: u64 = 12_000;
const HOLD_PERIOD_TICKS: u64 = 20;

fn verdict(pass: bool, what: &[u8]) {
	print(if pass { b"tpmprobe: PASS " } else { b"tpmprobe: FAIL " });
	print(what);
	print(b"\n");
}

fn outcome_name(outcome: Outcome) -> &'static [u8] {
	match outcome {
		Outcome::Done => b"a value",
		Outcome::NotGranted => b"not-granted",
		Outcome::PcrNotAllowed => b"pcr-not-allowed",
		Outcome::Bounds => b"bounds",
		Outcome::Busy => b"busy",
		Outcome::PolicyRefused => b"policy-refused",
		Outcome::OwnerHierarchyUnavailable => b"owner-hierarchy-unavailable",
		Outcome::OtherComponent => b"other-component",
		Outcome::Interrupted => b"interrupted",
		Outcome::Unavailable => b"unavailable",
		Outcome::Tpm => b"tpm",
		Outcome::Fault => b"fault",
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let Some(context) = recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) else { exit() };
	// The files first, then the grants - PermissionManager's vocabulary order.
	let volumes = Volumes::receive(bootstrap, &mut buf);
	let observe = recv_grant(bootstrap, &mut buf, b"TPM").unwrap_or(0);
	let seal = recv_grant(bootstrap, &mut buf, b"TPMSEAL").unwrap_or(0);
	if observe == 0 || seal == 0 {
		verdict(false, b"the observation and sealing grants were not both delivered");
		exit();
	}
	let mut client = TpmClient::new(observe, 0, seal);
	let arguments = context.arguments.clone();
	let words: Vec<&str> = arguments.split_whitespace().collect();
	match words.as_slice() {
		["extend23"] => extend23(&mut client, observe),
		["other", file] => other(&mut client, &volumes, &context.cwd, file),
		["own"] => own(&mut client),
		["hold"] => hold(observe),
		_ => verdict(false, b"usage: tpmprobe extend23 | other FILE | own | hold"),
	}
	exit();
}

fn pcr23(client: &mut TpmClient) -> Option<Vec<u8>> {
	match client.pcr_read(23) {
		Some(Ok(answer)) if answer.outcome == Outcome::Done => Some(answer.bytes),
		_ => None,
	}
}

fn extend23(client: &mut TpmClient, observe: u64) {
	let before = pcr23(client);
	let digest = service_logic::tpm::sha256(b"tpmprobe was here");
	// IN THIS PROCESS: the library holds no measurement grant, and says so without a message.
	let library = matches!(client.pcr_extend(23, &digest), Some(Ok(status)) if status.outcome == Outcome::NotGranted);
	// AT THE SERVICE: the same generated client, handed the observation connection in the measurement slot - so the
	// request is sent, on a connection whose grant does not carry it, and only TpmService can refuse it.
	let service = matches!(TpmClient::new(0, observe, 0).pcr_extend(23, &digest), Some(Ok(status)) if status.outcome == Outcome::NotGranted);
	let after = pcr23(client);
	verdict(library, b"an extend of PCR 23 is refused not-granted by tpm-client in this process");
	verdict(service, b"an extend of PCR 23 is refused not-granted by TpmService on the observation connection");
	verdict(before.is_some() && before == after, b"PCR 23 is unchanged");
}

// THE VOLUME BUNDLE, in PermissionManager's fixed order - seven tagged messages with no count in front of them, so all
// seven are read whether or not this probe uses them.
struct Volumes {
	system: u64,
	media: u64,
	iso: u64,
	udf: u64,
	usb: u64,
	ram: u64,
	tmp: u64,
}

impl Volumes {
	fn receive(bootstrap: u64, buf: &mut [u8]) -> Volumes {
		Volumes { system: recv_tagged(bootstrap, buf, b"SYSTEM").unwrap_or(0), media: recv_tagged(bootstrap, buf, b"MEDIA").unwrap_or(0), iso: recv_tagged(bootstrap, buf, b"ISO").unwrap_or(0), udf: recv_tagged(bootstrap, buf, b"UDF").unwrap_or(0), usb: recv_tagged(bootstrap, buf, b"USB").unwrap_or(0), ram: recv_tagged(bootstrap, buf, b"RAM").unwrap_or(0), tmp: recv_tagged(bootstrap, buf, b"TMP").unwrap_or(0) }
	}

	fn client_for(&self, cwd: &str, argument: &[u8]) -> u64 {
		path::volume_client(cwd, argument, self.system, self.media, self.iso, self.udf, self.usb, self.ram, self.tmp)
	}
}

// A GRANT AFTER THE VOLUMES, by its exact tag: the bootstrap terminator that may end the bundle is passed over, and
// anything else is not this grant.
fn recv_grant(bootstrap: u64, buf: &mut [u8], tag: &[u8]) -> Option<u64> {
	loop {
		match recv_blocking(bootstrap, buf) {
			Received::Message { len, handle } if &buf[..len] == BOOTSTRAP_READY && handle == 0 => continue,
			Received::Message { len, handle } if &buf[..len] == tag && handle != 0 => return Some(handle),
			Received::Message { handle, .. } => {
				if handle != 0 {
					close(handle);
				}
				return None;
			}
			Received::Closed => return None,
		}
	}
}

// A FILE OF AT MOST `limit` BYTES, read through the volume its path names; the file's capability closed on every path.
fn read_file(volumes: &Volumes, cwd: &str, argument: &str) -> Option<Vec<u8>> {
	const LIMIT: usize = 4096;
	let uri = path::resolve(cwd, argument.as_bytes())?;
	let storage = volumes.client_for(cwd, argument.as_bytes());
	if storage == 0 {
		return None;
	}
	let opened = match volume::Client::new(ChannelTransport { chan: storage }).open(&OpenOpts { path: uri, write: false, create: false }) {
		Some(Ok(opened)) if opened.file != 0 => opened,
		_ => return None,
	};
	let length = match usize::try_from(opened.size) {
		Ok(length) if length <= LIMIT => length,
		_ => {
			close(opened.file);
			return None;
		}
	};
	let mut bytes = Vec::new();
	if length == 0 || bytes.try_reserve_exact(length).is_err() {
		close(opened.file);
		return (length == 0).then_some(bytes);
	}
	// SAFETY: the object is the file just opened, mapped whole; `length` is its size, read before the unmap.
	let read = unsafe {
		match map_object(opened.file) {
			Some(address) => {
				bytes.extend_from_slice(core::slice::from_raw_parts(address as *const u8, length));
				unmap_object(opened.file);
				true
			}
			None => false,
		}
	};
	close(opened.file);
	read.then_some(bytes)
}

fn other(client: &mut TpmClient, volumes: &Volumes, cwd: &str, file: &str) {
	let Some(sealed) = read_file(volumes, cwd, file) else {
		verdict(false, b"the tool's sealed file could not be read");
		return;
	};
	let refused = matches!(client.unseal(&sealed), Some(Ok(answer)) if answer.outcome == Outcome::OtherComponent && answer.bytes.is_empty());
	verdict(refused, b"the tool's sealed secret is refused other-component and nothing of it is returned");
}

fn own(client: &mut TpmClient) {
	let sealed = match client.seal(16, b"tpmprobe's own") {
		Some(Ok(answer)) if answer.outcome == Outcome::Done => answer.bytes,
		_ => {
			verdict(false, b"its own secret could not be sealed");
			return;
		}
	};
	let opened = matches!(client.unseal(&sealed), Some(Ok(answer)) if answer.outcome == Outcome::Done && answer.bytes == b"tpmprobe's own");
	verdict(opened, b"its own sealed secret opens for it");
}

// ONE CONNECTION, HELD: what `random` answers on it, printed each time the answer changes.
fn hold(observe: u64) {
	let mut client = TpmClient::new(observe, 0, 0);
	let mut last: Option<Outcome> = None;
	let end = clock() + HOLD_TICKS;
	print(b"tpmprobe: hold - one connection held\n");
	while clock() < end {
		let now = match client.random(8) {
			Some(Ok(answer)) => answer.outcome,
			_ => Outcome::Fault,
		};
		if last != Some(now) {
			print(b"tpmprobe: hold ");
			print(outcome_name(now));
			print(b"\n");
			last = Some(now);
		}
		sleep_until(clock() + HOLD_PERIOD_TICKS);
	}
	print(b"tpmprobe: hold - done\n");
}
