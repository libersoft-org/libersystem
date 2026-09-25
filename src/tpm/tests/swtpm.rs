// THE TYPED OPERATIONS AGAINST A REAL TPM 2.0: `swtpm`, which is libtpms - the TCG's reference implementation -
// behind a socket that takes raw commands. Nothing here is a model this crate's author wrote: every answer is the
// reference implementation's, and every value checked is computed on this side (a PCR's new value) or checked by
// a third party (OpenSSL verifying the quote's signature with the key the TPM made).
//
// IT DOES NOT SKIP. `setup.sh` installs `swtpm` for exactly this, and a suite that passes because the tool it
// needs was missing is a suite that stops testing on the first machine without it - so a missing `swtpm` or
// `openssl` fails here, by name.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tpm::command::Transport;
use tpm::ops::Tpm;
use tpm::{Error, TransportError};

// A running `swtpm` in its own directory, stopped and removed however the test ends.
struct Swtpm {
	child: Child,
	dir: PathBuf,
}

impl Drop for Swtpm {
	fn drop(&mut self) {
		let _ = self.child.kill();
		let _ = self.child.wait();
		let _ = std::fs::remove_dir_all(&self.dir);
	}
}

fn start(name: &str) -> (Swtpm, UnixStream) {
	let dir = std::env::temp_dir().join(format!("liber-swtpm-{name}.{}", std::process::id()));
	let _ = std::fs::remove_dir_all(&dir);
	std::fs::create_dir_all(&dir).expect("a directory for the TPM's state");
	let data = dir.join("data.sock");
	let child = Command::new("swtpm").args(["socket", "--tpm2", "--flags", "not-need-init"]).arg("--tpmstate").arg(format!("dir={}", dir.display())).arg("--server").arg(format!("type=unixio,path={}", data.display())).arg("--ctrl").arg(format!("type=unixio,path={}", dir.join("ctrl.sock").display())).stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("swtpm runs - setup.sh installs it, and this suite fails rather than skips without it");
	let swtpm = Swtpm { child, dir };
	let deadline = Instant::now() + Duration::from_secs(10);
	loop {
		if let Ok(stream) = UnixStream::connect(&data) {
			return (swtpm, stream);
		}
		assert!(Instant::now() < deadline, "swtpm did not open its command socket");
		std::thread::sleep(Duration::from_millis(20));
	}
}

// Raw commands over the socket: the command, then the response by its own header.
struct Socket(UnixStream);

impl Transport for Socket {
	fn execute(&mut self, command: &[u8], response: &mut Vec<u8>, duration_ms: u64) -> Result<(), TransportError> {
		self.0.set_read_timeout(Some(Duration::from_millis(duration_ms.max(1)))).map_err(|_| TransportError::Fault)?;
		self.0.write_all(command).map_err(|_| TransportError::Fault)?;
		let mut header = [0u8; 10];
		self.0.read_exact(&mut header).map_err(|_| TransportError::TimedOut)?;
		let size = u32::from_be_bytes([header[2], header[3], header[4], header[5]]) as usize;
		if !(10..=tpm::MAX_MESSAGE).contains(&size) {
			return Err(TransportError::Length);
		}
		response.extend_from_slice(&header);
		let mut rest = vec![0u8; size - 10];
		self.0.read_exact(&mut rest).map_err(|_| TransportError::Length)?;
		response.extend_from_slice(&rest);
		Ok(())
	}
}

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
	bootproto::sha256::digest(&parts.concat())
}

// An integer for DER: no leading zeros, and a zero in front if the top bit would make it negative.
fn der_integer(value: &[u8]) -> Vec<u8> {
	let trimmed: Vec<u8> = value.iter().copied().skip_while(|byte| *byte == 0).collect();
	let mut body = if trimmed.is_empty() { vec![0] } else { trimmed };
	if body[0] & 0x80 != 0 {
		body.insert(0, 0);
	}
	[vec![0x02, body.len() as u8], body].concat()
}

// OpenSSL's verdict on an ECDSA P-256 signature over `message`, by the key at point (x, y).
fn openssl_verifies(dir: &std::path::Path, x: &[u8], y: &[u8], r: &[u8], s: &[u8], message: &[u8]) -> bool {
	let spki_prefix = [0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00, 0x04];
	let public = [&spki_prefix[..], x, y].concat();
	let integers = [der_integer(r), der_integer(s)].concat();
	let signature = [vec![0x30, integers.len() as u8], integers].concat();
	std::fs::write(dir.join("key.der"), public).unwrap();
	std::fs::write(dir.join("sig.der"), signature).unwrap();
	std::fs::write(dir.join("attest.bin"), message).unwrap();
	let status = Command::new("openssl").args(["dgst", "-sha256", "-keyform", "DER", "-verify"]).arg(dir.join("key.der")).arg("-signature").arg(dir.join("sig.der")).arg(dir.join("attest.bin")).stdout(Stdio::null()).stderr(Stdio::null()).status().expect("openssl runs - the smart-card gate needs it too, and this suite does not skip without it");
	status.success()
}

#[test]
fn the_typed_operations_hold_against_a_real_tpm() {
	let (swtpm, stream) = start("operations");
	let mut tpm = Tpm::new(Socket(stream));

	tpm.startup().expect("Startup(CLEAR)");
	tpm.startup().expect("and again, which a started TPM answers with INITIALIZE - still started");
	tpm.self_test().expect("the full self-test");

	// RANDOM BYTES, whole and different.
	let first = tpm.random(40).expect("forty random bytes");
	let second = tpm.random(40).expect("forty more");
	assert_eq!((first.len(), second.len()), (40, 40));
	assert_ne!(first, second, "two draws from a generator are not the same forty bytes");

	// A PCR EXTENDED, and read back as the hash chain computed here.
	let before = tpm.pcr_read(16).expect("PCR 16");
	let measurement = sha256(&[b"liber measurement"]);
	tpm.pcr_extend(16, &measurement).expect("PCR 16 extended");
	let after = tpm.pcr_read(16).expect("PCR 16 again");
	assert_eq!(after, sha256(&[&before, &measurement]), "the TPM's PCR is SHA-256(what it was || what was measured)");
	assert_eq!(tpm.pcr_extend(17, &measurement), Err(Error::Locality), "the dynamic root's PCRs are not locality zero's");

	// A SECRET SEALED TO PCR 16: opened while the PCR holds its value, refused once it has moved.
	let sealed = tpm.seal(b"liber sealed secret", 16).expect("sealed");
	assert_eq!(tpm.unseal(&sealed).as_deref(), Ok(&b"liber sealed secret"[..]), "unsealed while PCR 16 holds the value it was sealed to");
	assert_eq!(tpm.unseal(&sealed).as_deref(), Ok(&b"liber sealed secret"[..]), "and again: an unseal consumes nothing");
	tpm.pcr_extend(16, &sha256(&[b"something else ran"])).expect("PCR 16 moved");
	assert_eq!(tpm.unseal(&sealed), Err(Error::PolicyRefused), "and refused once it has moved, by the policy");

	// EVERYTHING IS FLUSHED: a TPM holds three transient objects at once, so a leak of one an operation would
	// stop these by the fourth.
	for round in 0..6 {
		let sealed = tpm.seal(b"round", 16).unwrap_or_else(|error| panic!("seal in round {round}: {error:?}"));
		assert_eq!(tpm.unseal(&sealed).as_deref(), Ok(&b"round"[..]), "round {round}");
	}

	// A QUOTE: this nonce, the digest of PCR 16 as it is now, signed by a key only this TPM has - and verified by
	// OpenSSL with the public key the TPM made, not by this crate.
	let now = tpm.pcr_read(16).expect("PCR 16 now");
	let nonce = tpm.random(16).expect("a nonce");
	let quote = tpm.quote(&nonce, 16).expect("a quote");
	assert_eq!(quote.pcr_digest, sha256(&[&now]).to_vec(), "the quote carries the digest of PCR 16's value");
	assert!(openssl_verifies(&swtpm.dir, &quote.x, &quote.y, &quote.r, &quote.s, &quote.attest), "OpenSSL verifies the TPM's signature over the attestation");
	let mut forged = quote.attest.clone();
	let last = forged.len() - 1;
	forged[last] ^= 1;
	assert!(!openssl_verifies(&swtpm.dir, &quote.x, &quote.y, &quote.r, &quote.s, &forged), "and refuses it over an attestation one bit different");
	assert!(matches!(tpm.quote(&[0; 65], 16), Err(Error::Bounds)), "a nonce past the bound is not sent");
}
