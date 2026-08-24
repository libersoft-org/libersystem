// Sign a boot manifest over the FINAL STAGED BYTES, and verify what was written before anything
// ships.
//
// AFTER STRIPPING AND STAGING, which is the whole reason this is its own step. A manifest made from
// the artifacts a compiler produced describes bytes that are not the ones the loader will read: the
// strip changes them, and so does whatever the staging does. Signing the staged file is the only
// way the signature is about what is executed.
//
// TWO SIGNER CONTRACTS, AND NO IMPLIED THIRD ONE.
//
// `test-trust` is the host/QEMU-closing profile. Its private key is a fixture in this repository -
// deterministic, so a build is reproducible - and it is accepted ONLY in this profile. The loader
// built for it carries an unmistakable identity and says so before it loads anything.
//
// `external-release` fixes a public key and key id in the loader and invokes ONE configured signer
// executable: this tool writes the exact canonical payload to its stdin and accepts exactly one raw
// 64-byte signature on its stdout. The path is configuration, not a secret; the private key belongs
// wholly to that executable and never appears in this tree's arguments, environment or output.
//
// What is NOT here, deliberately: a release command, an HSM service, a certificate authority, an
// operator procedure, key rotation and revocation. The key id exists so a later format does not
// have to change to rotate; making rotation work is not this milestone's.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

// THE TEST KEY, AND IT IS IN THE OPEN ON PURPOSE. A fixture private key that is secret is a fixture
// nobody can reproduce a build with; one that is published cannot be mistaken for a release key. It
// is accepted only under `--profile test-trust`, and the gate proves it cannot reach an
// `external-release` loader or image.
const TEST_SIGNING_KEY: [u8; 32] = [
	0x9d,
	0x61,
	0xb1,
	0x9d,
	0xef,
	0xfd,
	0x5a,
	0x60,
	0xba,
	0x84,
	0x4a,
	0xf4,
	0x92,
	0xec,
	0x2c,
	0xc4,
	0x44,
	0x49,
	0xc5,
	0x69,
	0x7b,
	0x32,
	0x69,
	0x19,
	0x70,
	0x3b,
	0xac,
	0x03,
	0x1c,
	0xae,
	0x7f,
	0x60,
];
pub const TEST_KEY_ID: u32 = 0x7e57_0001;

fn die(message: &str) -> ! {
	eprintln!("sign-manifest: {message}");
	std::process::exit(1)
}

struct Args {
	profile: String,
	product: String,
	arch: u8,
	source: u8,
	release: String,
	volume_uuid: [u8; 16],
	key_id: u32,
	signer: Option<String>,
	public_key: Option<[u8; 32]>,
	rows: Vec<(u8, String, String)>,
	out: String,
}

fn hex(text: &str, want: usize) -> Option<Vec<u8>> {
	if text.len() != want * 2 {
		return None;
	}
	(0..want).map(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()).collect()
}

fn parse() -> Args {
	let mut args = Args { profile: String::new(), product: String::new(), arch: 0, source: 0, release: String::new(), volume_uuid: [0; 16], key_id: 0, signer: None, public_key: None, rows: Vec::new(), out: String::new() };
	let mut argv = std::env::args().skip(1);
	while let Some(flag) = argv.next() {
		let mut value = || argv.next().unwrap_or_else(|| die(&format!("{flag} needs a value")));
		match flag.as_str() {
			"--profile" => args.profile = value(),
			"--product" => args.product = value(),
			"--release" => args.release = value(),
			"--out" => args.out = value(),
			"--signer" => args.signer = Some(value()),
			"--key-id" => {
				let raw = value();
				args.key_id = raw.parse().unwrap_or_else(|_| die("--key-id is a number"));
			}
			"--public-key" => {
				let raw = value();
				let bytes = hex(&raw, 32).unwrap_or_else(|| die("--public-key is 64 hex characters"));
				args.public_key = Some(bytes.try_into().expect("32 bytes"));
			}
			"--volume-uuid" => {
				let raw = value();
				let bytes = hex(&raw, 16).unwrap_or_else(|| die("--volume-uuid is 32 hex characters"));
				args.volume_uuid = bytes.try_into().expect("16 bytes");
			}
			"--arch" => {
				args.arch = match value().as_str() {
					"x86_64" => bootproto::manifest::ARCH_X86_64,
					"aarch64" => bootproto::manifest::ARCH_AARCH64,
					"riscv64" => bootproto::manifest::ARCH_RISCV64,
					other => die(&format!("--arch '{other}' is not one this format names")),
				}
			}
			"--source" => {
				args.source = match value().as_str() {
					"system-volume" => bootproto::manifest::SOURCE_SYSTEM_VOLUME,
					"live-image" => bootproto::manifest::SOURCE_LIVE_IMAGE,
					"boot-medium" => bootproto::manifest::SOURCE_BOOT_MEDIUM,
					other => die(&format!("--source '{other}' is not one this format names")),
				}
			}
			"--row" => {
				// `kind:path=file`: what the manifest calls it, and where the bytes are now.
				let raw = value();
				let (kind, rest) = raw.split_once(':').unwrap_or_else(|| die("--row is kind:path=file"));
				let (path, file) = rest.split_once('=').unwrap_or_else(|| die("--row is kind:path=file"));
				let kind = match kind {
					"kernel" => bootproto::manifest::KIND_KERNEL,
					"bootstrap-list" => bootproto::manifest::KIND_BOOTSTRAP_LIST,
					"program" => bootproto::manifest::KIND_PROGRAM,
					"system-volume" => bootproto::manifest::KIND_SYSTEM_VOLUME,
					"package" => bootproto::manifest::KIND_PACKAGE,
					other => die(&format!("--row kind '{other}' is not one this format names")),
				};
				args.rows.push((kind, path.to_string(), file.to_string()));
			}
			other => die(&format!("unknown argument '{other}'")),
		}
	}
	args
}

// Ask the configured executable for a signature over exactly these bytes.
//
// EXACTLY ONE SIGNATURE, AND NOTHING ELSE ON STDOUT. A signer that prints a banner produces a
// "signature" that is a banner followed by 64 bytes, and a tool that took the last 64 would sign
// whatever it was handed. The length is the check.
fn sign_externally(signer: &str, message: &[u8]) -> [u8; 64] {
	let mut child = Command::new(signer).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap_or_else(|e| die(&format!("could not run the signer '{signer}': {e}")));
	child.stdin.take().unwrap_or_else(|| die("the signer has no stdin")).write_all(message).unwrap_or_else(|e| die(&format!("could not write to the signer: {e}")));
	let mut signature = Vec::new();
	child.stdout.take().unwrap_or_else(|| die("the signer has no stdout")).read_to_end(&mut signature).unwrap_or_else(|e| die(&format!("could not read the signer: {e}")));
	let status = child.wait().unwrap_or_else(|e| die(&format!("the signer did not finish: {e}")));
	if !status.success() {
		die(&format!("the signer exited with {status}"));
	}
	if signature.len() != 64 {
		die(&format!("the signer produced {} bytes, and a signature is 64", signature.len()));
	}
	signature.try_into().expect("64 bytes")
}

fn main() {
	let args = parse();
	if args.out.is_empty() || args.product.is_empty() || args.release.is_empty() || args.arch == 0 || args.source == 0 {
		die("--product, --arch, --source, --release and --out are all required");
	}
	if args.rows.is_empty() {
		die("a manifest with no rows covers nothing");
	}

	// WHICH KEY, DECIDED BEFORE ANYTHING IS READ. `external-release` without its public-key identity
	// or its signer fails here - before an image is written, which is the only place the failure is
	// free.
	let (key_id, public_key, signer) = match args.profile.as_str() {
		"test-trust" => {
			let signing = ed25519_dalek::SigningKey::from_bytes(&TEST_SIGNING_KEY);
			(TEST_KEY_ID, signing.verifying_key().to_bytes(), None)
		}
		"external-release" => {
			let Some(public_key) = args.public_key else { die("external-release needs --public-key: the loader carries a public key and this must be that one") };
			let Some(signer) = args.signer.clone() else { die("external-release needs --signer: one executable that holds the private key") };
			if args.key_id == 0 {
				die("external-release needs --key-id: the loader refuses a manifest whose key id it does not carry");
			}
			(args.key_id, public_key, Some(signer))
		}
		other => die(&format!("--profile '{other}' is not one of test-trust or external-release")),
	};

	// The rows, over the bytes AS STAGED.
	let mut contents: Vec<(u8, String, Vec<u8>)> = Vec::new();
	for (kind, path, file) in &args.rows {
		let bytes = std::fs::read(file).unwrap_or_else(|e| die(&format!("could not read {file}: {e}")));
		contents.push((*kind, path.clone(), bytes));
	}
	let mut rows: Vec<bootproto::manifest::Row<'_>> = contents.iter().map(|(kind, path, bytes)| bootproto::manifest::Row { kind: *kind, path: path.as_bytes(), length: bytes.len() as u64, digest: bootproto::sha256::digest(bytes) }).collect();

	let header = bootproto::manifest::Header { key_id, product: args.product.as_bytes(), arch: args.arch, source_kind: args.source, release: args.release.as_bytes(), volume_uuid: args.volume_uuid };
	let mut record = vec![0u8; bootproto::manifest::MAX_MANIFEST_BYTES];
	let payload_len = bootproto::manifest::encode_payload(&header, &mut rows, &mut record).unwrap_or_else(|e| die(&format!("the manifest will not encode: {e:?}")));

	// The message is the domain string and the payload, which is what the loader will verify.
	let mut message = Vec::with_capacity(bootproto::manifest::DOMAIN.len() + payload_len);
	message.extend_from_slice(bootproto::manifest::DOMAIN);
	message.extend_from_slice(&record[..payload_len]);

	let signature = match &signer {
		Some(signer) => sign_externally(signer, &message),
		None => {
			use ed25519_dalek::Signer;
			ed25519_dalek::SigningKey::from_bytes(&TEST_SIGNING_KEY).sign(&message).to_bytes()
		}
	};
	record[payload_len..payload_len + 64].copy_from_slice(&signature);
	record.truncate(payload_len + 64);

	// VERIFIED HERE, WITH THE PARSER AND THE VERIFIER THE LOADER USES. A signing tool that cannot
	// check its own output moves every one of its failures to a boot - and an external signer is
	// somebody else's program, which may sign something other than what it was handed.
	let read_back = bootproto::manifest::Manifest::decode(&record).unwrap_or_else(|e| die(&format!("what this wrote does not parse: {e:?}")));
	let mut scratch = vec![0u8; bootproto::manifest::DOMAIN.len() + payload_len];
	if !bootsig::verifies(&public_key, &read_back.signature(), bootproto::manifest::DOMAIN, read_back.payload(), &mut scratch) {
		die("the signature does not verify against the public key this manifest names - nothing was written");
	}
	if read_back.key_id != key_id {
		die("the manifest that was read back names a different key");
	}

	std::fs::write(&args.out, &record).unwrap_or_else(|e| die(&format!("could not write {}: {e}", args.out)));
	println!("sign-manifest: {} - {} row(s), key {:#010x}, {} bytes, verified", args.out, read_back.row_count(), key_id, record.len());
}
