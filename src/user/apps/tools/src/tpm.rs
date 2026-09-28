// tpm - the machine's TPM 2.0, as a person sees it: its identity, random bytes, PCRs, sealed secrets and quotes.
//
// A DEMONSTRATION AND A TOOL. It is what a third-party program does with `tpm-client`, written as a command: every
// line it prints is what the TPM answered - a digest, bytes, the quote's attestation, signature and public point -
// and every refusal by the name the contract gives it, so what a person reads is the device's answer and never
// something this tool computed. The one thing it computes is the digest `extend` measures: the SHA-256 of the
// text it was given, which is what "extend with TEXT" means.
//
// IT HOLDS ALL THREE TPM GRANTS - observation, measurement and sealing - and the files it keeps sealed objects and
// quotes in. It is the one shipping component that holds any of them.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::system::{InterfaceKind, LaunchContext, Outcome, WriterMode};
use rt::*;
use storage_proto::path;
use tools::{VolumeSet, parse_u64, read_volume_file, split_args};
use tpm_client::TpmClient;
use volume_client::VolumeClient;

const USAGE: &[u8] = b"usage: tpm info | random N | pcr N | extend N TEXT | seal N TEXT FILE | unseal FILE | quote N NONCE FILE\n";

// The largest sealed object or quote file this reads back.
const MAX_FILE: usize = 4096;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	// The files, then the three grants: PermissionManager hands a launch its grants in its vocabulary's order, and
	// the volumes come before the TPM there.
	let volumes = VolumeSet::receive(bootstrap, &mut buf);
	let observe = VolumeSet::recv_grant(bootstrap, &mut buf, b"TPM").unwrap_or(0);
	let measure = VolumeSet::recv_grant(bootstrap, &mut buf, b"TPMMEASURE").unwrap_or(0);
	let seal = VolumeSet::recv_grant(bootstrap, &mut buf, b"TPMSEAL").unwrap_or(0);
	let mut client = TpmClient::new(observe, measure, seal);
	let arguments: Vec<u8> = context.arguments.clone().into_bytes();
	let args: Vec<&[u8]> = split_args(&arguments).collect();
	let number = |at: usize| args.get(at).and_then(|text| parse_u64(text)).and_then(|value| u32::try_from(value).ok()).unwrap_or_else(|| usage());
	let cwd = context.cwd.clone();
	match (args.first().copied(), args.len()) {
		(None, _) | (Some(b"info"), 1) => info(&mut client),
		(Some(b"random"), 2) => {
			let answer = client.random(number(1));
			bytes_line(b"random", answer, |bytes| hex_line(bytes));
		}
		(Some(b"pcr"), 2) => {
			let pcr = number(1);
			let answer = client.pcr_read(pcr);
			bytes_line(b"pcr", answer, |bytes| hex_line(bytes));
		}
		(Some(b"extend"), 3) => {
			let pcr = number(1);
			let digest = service_logic::tpm::sha256(args[2]);
			match client.pcr_extend(pcr, &digest) {
				Some(Ok(status)) if status.outcome == Outcome::Done => {
					print(b"tpm: extended PCR ");
					print_number(pcr as u64);
					print(b" with ");
					hex_line(&digest);
				}
				Some(Ok(status)) => refusal(b"extend", status.outcome, status.code),
				_ => unreachable_service(),
			}
		}
		(Some(b"seal"), 4) => {
			let pcr = number(1);
			match client.seal(pcr, args[2]) {
				Some(Ok(answer)) if answer.outcome == Outcome::Done => {
					if write_file(&volumes, &cwd, args[3], &answer.bytes) {
						print(b"tpm: sealed to PCR ");
						print_number(pcr as u64);
						print(b" - ");
						print_number(answer.bytes.len() as u64);
						print(b" bytes written to ");
						print(args[3]);
						print(b"\n");
					}
				}
				Some(Ok(answer)) => refusal(b"seal", answer.outcome, answer.code),
				_ => unreachable_service(),
			}
		}
		(Some(b"unseal"), 2) => {
			let Some(sealed) = read_file(&volumes, &cwd, args[1]) else { exit() };
			match client.unseal(&sealed) {
				Some(Ok(answer)) if answer.outcome == Outcome::Done => {
					print(b"tpm: unsealed: ");
					print(&answer.bytes);
					print(b"\n");
				}
				Some(Ok(answer)) => refusal(b"unseal", answer.outcome, answer.code),
				_ => unreachable_service(),
			}
		}
		(Some(b"quote"), 4) => {
			let pcr = number(1);
			match client.quote(pcr, args[2]) {
				Some(Ok(answer)) if answer.outcome == Outcome::Done => {
					let Some(quote) = answer.quote else { unreachable_service() };
					// THE FILE CARRIES WHAT A VERIFIER NEEDS, each part by its length: the attestation, r, s, x, y.
					let mut file: Vec<u8> = Vec::new();
					for part in [&quote.attest, &quote.signature_r, &quote.signature_s, &quote.point_x, &quote.point_y] {
						file.extend_from_slice(&(part.len() as u16).to_be_bytes());
						file.extend_from_slice(part);
					}
					for (label, part) in [
						(&b"attest"[..], &quote.attest),
						(b"r", &quote.signature_r),
						(b"s", &quote.signature_s),
						(b"x", &quote.point_x),
						(b"y", &quote.point_y),
						(b"pcr-digest", &quote.pcr_digest),
					] {
						print(b"tpm: quote ");
						print(label);
						print(b" ");
						hex_line(part);
					}
					if write_file(&volumes, &cwd, args[3], &file) {
						print(b"tpm: quote written to ");
						print(args[3]);
						print(b"\n");
					}
				}
				Some(Ok(answer)) => refusal(b"quote", answer.outcome, answer.code),
				_ => unreachable_service(),
			}
		}
		_ => usage(),
	}
	exit();
}

fn usage() -> ! {
	eprint(USAGE);
	exit()
}

fn unreachable_service() -> ! {
	eprint(b"tpm: TpmService did not answer\n");
	exit()
}

fn info(client: &mut TpmClient) {
	match client.info() {
		Some(Ok(answer)) if answer.outcome == Outcome::Done => {
			let Some(info) = answer.info else { unreachable_service() };
			print(b"tpm: interface ");
			print(match info.interface {
				InterfaceKind::Crb => b"CRB",
				InterfaceKind::Fifo => b"FIFO",
			});
			print(b", manufacturer ");
			print(info.manufacturer.trim_end().as_bytes());
			print(b", vendor ");
			print(info.vendor.trim_end().as_bytes());
			print(b", firmware ");
			hex32(info.firmware_1);
			print(b".");
			hex32(info.firmware_2);
			print(if info.sealing { b", seal unseal and quote available\n" } else { b", seal unseal and quote UNAVAILABLE - the owner hierarchy is provisioned or disabled\n" });
		}
		Some(Ok(answer)) => refusal(b"info", answer.outcome, answer.code),
		_ => unreachable_service(),
	}
}

fn bytes_line(what: &[u8], answer: Option<Result<proto::system::BytesAnswer, proto::system::Error>>, show: impl FnOnce(&[u8])) {
	match answer {
		Some(Ok(answer)) if answer.outcome == Outcome::Done => {
			print(b"tpm: ");
			print(what);
			print(b" ");
			show(&answer.bytes);
		}
		Some(Ok(answer)) => refusal(what, answer.outcome, answer.code),
		_ => unreachable_service(),
	}
}

// EVERY REFUSAL BY ITS NAME, and the TPM's own code when it is the TPM's.
fn refusal(what: &[u8], outcome: Outcome, code: u32) {
	print(b"tpm: ");
	print(what);
	print(b" refused: ");
	print(match outcome {
		Outcome::Done => b"done",
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
	});
	if outcome == Outcome::Tpm {
		print(b" 0x");
		hex32(code);
	}
	print(b"\n");
}

fn hex_line(bytes: &[u8]) {
	let mut line: Vec<u8> = Vec::with_capacity(bytes.len() * 2 + 1);
	for byte in bytes {
		line.push(b"0123456789abcdef"[(byte >> 4) as usize]);
		line.push(b"0123456789abcdef"[(byte & 0xf) as usize]);
	}
	line.push(b'\n');
	print(&line);
}

fn hex32(value: u32) {
	let mut out = [0u8; 8];
	for (at, slot) in out.iter_mut().enumerate() {
		*slot = b"0123456789abcdef"[((value >> ((7 - at) * 4)) & 0xf) as usize];
	}
	print(&out);
}

fn print_number(value: u64) {
	let mut text = String::new();
	tools::push_decimal(&mut text, value);
	print(text.as_bytes());
}

fn write_file(volumes: &VolumeSet, cwd: &str, argument: &[u8], bytes: &[u8]) -> bool {
	let Some(uri) = path::resolve(cwd, argument) else {
		eprint(b"tpm: an invalid path\n");
		return false;
	};
	let mut client = VolumeClient::new(volumes.client_for(cwd, argument));
	let Some(Ok(mut writer)) = client.open_writer(&uri, WriterMode::Replace) else {
		eprint(b"tpm: the file could not be opened for writing\n");
		return false;
	};
	let written = matches!(writer.write(bytes), Some(Ok(_))) && matches!(writer.commit(), Some(Ok(_)));
	if !written {
		let _ = writer.abort();
		eprint(b"tpm: the file could not be written\n");
	}
	close(writer.handle());
	written
}

fn read_file(volumes: &VolumeSet, cwd: &str, argument: &[u8]) -> Option<Vec<u8>> {
	let Some(uri) = path::resolve(cwd, argument) else {
		eprint(b"tpm: an invalid path\n");
		return None;
	};
	match unsafe { read_volume_file(volumes.client_for(cwd, argument), &uri, MAX_FILE) } {
		Ok(bytes) => Some(bytes),
		Err(_) => {
			eprint(b"tpm: the file could not be read\n");
			None
		}
	}
}
