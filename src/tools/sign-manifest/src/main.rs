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

use sign_manifest::TestKey;

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
	// The version-3 header fields. The generation is what the rollback floor compares; the purpose
	// is what a root may sign for; the DMA mode is `None` for a tag-0 manifest - the test and
	// development builders, whose boots take the mode from the harness carrier - and a mode for a
	// shipping builder, which signs the value the medium is assembled for.
	security_generation: u64,
	purpose: u32,
	dma_mode: Option<u32>,
	dma_mode_given: bool,
	volume_uuid: [u8; 16],
	// Whether `--generation` was given: an external release must say its generation rather than
	// take the default, because the default is what every test build carries.
	generation_given: bool,
	key_id: u32,
	signer: Option<String>,
	public_key: Option<[u8; 32]>,
	// Which PUBLISHED key signs under `test-trust`; the boot key unless told otherwise.
	test_key: TestKey,
	rows: Vec<(u8, String, String)>,
	out: String,
}

fn hex(text: &str, want: usize) -> Option<Vec<u8>> {
	if text.len() != want * 2 {
		return None;
	}
	(0..want).map(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()).collect()
}

fn parse() -> (Args, Option<bootproto::rollback::Slot>, bool) {
	let mut args = Args { profile: String::new(), product: String::new(), arch: 0, source: 0, release: String::new(), security_generation: 1, purpose: bootproto::manifest::PURPOSE_BOOT, dma_mode: None, dma_mode_given: false, volume_uuid: [0; 16], generation_given: false, key_id: 0, signer: None, public_key: None, test_key: TestKey::Boot, rows: Vec::new(), out: String::new() };
	let mut rollback_record: Option<bootproto::rollback::Slot> = None;
	let mut rollback_marker: bool = false;
	let mut argv = std::env::args().skip(1);
	while let Some(flag) = argv.next() {
		let mut value = || argv.next().unwrap_or_else(|| die(&format!("{flag} needs a value")));
		match flag.as_str() {
			"--profile" => args.profile = value(),
			"--generation" => {
				let raw = value();
				args.generation_given = true;
				args.security_generation = raw.parse().unwrap_or_else(|_| die("--generation is an unsigned 64-bit number"));
			}
			"--purpose" => {
				args.purpose = match value().as_str() {
					"boot" => bootproto::manifest::PURPOSE_BOOT,
					"recovery" => bootproto::manifest::PURPOSE_RECOVERY,
					other => die(&format!("--purpose '{other}' is not one of boot or recovery")),
				}
			}
			"--dma-mode" => {
				// REQUIRED, AND `harness` IS A STATEMENT RATHER THAN A DEFAULT. A manifest that
				// carries no mode is a manifest whose boot takes the mode from the harness carrier,
				// and a builder that meant to sign one must say so - a tag-0 manifest produced by
				// omission is exactly how a shipping medium would come to depend on a harness.
				args.dma_mode = match value().as_str() {
					"harness" => None,
					"enforcing-required" => Some(bootproto::dma_mode::MODE_ENFORCING_REQUIRED),
					"no-iommu" => Some(bootproto::dma_mode::MODE_NO_IOMMU),
					other => die(&format!("--dma-mode '{other}' is not one of harness, enforcing-required or no-iommu")),
				};
				args.dma_mode_given = true;
			}
			"--inspect" => inspect(&value()),
			// THE PROVISIONING TOOL'S HALF: one slot record, or the marker, as hex on stdout - the
			// same codec the loader validates with, so the ceremony and the loader cannot disagree
			// about a byte. Needs `--product` and `--generation`; nothing else is read.
			"--rollback-record" => {
				rollback_record = Some(match value().as_str() {
					"a" | "A" => bootproto::rollback::Slot::A,
					"b" | "B" => bootproto::rollback::Slot::B,
					other => die(&format!("--rollback-record '{other}' is not one of a or b")),
				})
			}
			"--rollback-marker" => rollback_marker = true,
			"--signing-key" => {
				args.test_key = match value().as_str() {
					"boot" => TestKey::Boot,
					"recovery" => TestKey::Recovery,
					other => die(&format!("--signing-key '{other}' is not one of boot or recovery")),
				}
			}
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
	(args, rollback_record, rollback_marker)
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

// PRINT WHAT A SIGNED MANIFEST SAYS, and exit. The parse is the loader's; no signature is checked,
// because this answers "what does this record claim" for a host tool that decides which machine
// to build around it - `run.sh` refusing to boot an enforcing image without a controller - and the
// loader is what decides whether the claim is believed.
fn inspect(path: &str) -> ! {
	let bytes = std::fs::read(path).unwrap_or_else(|e| die(&format!("could not read {path}: {e}")));
	let manifest = match bootproto::manifest::Manifest::decode(&bytes) {
		Ok(manifest) => manifest,
		Err(bootproto::manifest::Refusal::LegacyVersion) => {
			println!("version: legacy");
			std::process::exit(0)
		}
		Err(e) => die(&format!("{path} is not a manifest this tool reads: {e:?}")),
	};
	println!("version: 3");
	println!("release: {}", String::from_utf8_lossy(manifest.release));
	println!("generation: {}", manifest.security_generation);
	println!(
		"purpose: {}",
		match manifest.purpose {
			bootproto::manifest::PURPOSE_RECOVERY => "recovery",
			_ => "boot",
		}
	);
	println!(
		"dma-mode: {}",
		match manifest.dma_mode.and_then(bootproto::dma_mode::Mode::from_code) {
			Some(mode) => mode.name(),
			None => "harness",
		}
	);
	println!("volume-uuid: {}", manifest.volume_uuid.iter().map(|byte| format!("{byte:02x}")).collect::<String>());
	println!("rows: {}", manifest.row_count());
	std::process::exit(0)
}

fn main() {
	let (args, rollback_record, rollback_marker) = parse();
	if rollback_marker {
		println!("{:02x}", bootproto::rollback::MARKER_VALUE);
		return;
	}
	if let Some(slot) = rollback_record {
		if args.product.is_empty() || !args.generation_given {
			die("--rollback-record needs --product and --generation");
		}
		let record = bootproto::rollback::encode(args.security_generation, slot, &bootproto::rollback::product_identity(args.product.as_bytes()));
		println!("{}", record.iter().map(|byte| format!("{byte:02x}")).collect::<String>());
		return;
	}
	if args.out.is_empty() || args.product.is_empty() || args.release.is_empty() || args.arch == 0 || args.source == 0 {
		die("--product, --arch, --source, --release and --out are all required");
	}
	if !args.dma_mode_given {
		die("--dma-mode is required: harness for a manifest whose boot takes the mode from the harness carrier, or enforcing-required / no-iommu for a shipping medium");
	}
	if args.rows.is_empty() {
		die("a manifest with no rows covers nothing");
	}

	// WHICH KEY, DECIDED BEFORE ANYTHING IS READ. `external-release` without its public-key identity
	// or its signer fails here - before an image is written, which is the only place the failure is
	// free.
	let (key_id, public_key, signer) = match args.profile.as_str() {
		"test-trust" => {
			let signing = args.test_key.signing_key();
			(args.test_key.key_id(), signing.verifying_key().to_bytes(), None)
		}
		"external-release" => {
			let Some(public_key) = args.public_key else { die("external-release needs --public-key: the loader carries a public key and this must be that one") };
			let Some(signer) = args.signer.clone() else { die("external-release needs --signer: one executable that holds the private key") };
			if args.key_id == 0 {
				die("external-release needs --key-id: the loader refuses a manifest whose key id it does not carry");
			}
			// THE GENERATION IS STATED, NEVER DEFAULTED, for a release: the default is the number every
			// test build carries, and a release that took it by omission would be one the rollback
			// floor compares against a value nobody chose.
			if !args.generation_given {
				die("external-release needs --generation: the security generation the rollback floor compares, stated rather than defaulted");
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

	let header = bootproto::manifest::Header { key_id, product: args.product.as_bytes(), arch: args.arch, source_kind: args.source, release: args.release.as_bytes(), security_generation: args.security_generation, purpose: args.purpose, volume_uuid: args.volume_uuid, dma_mode: args.dma_mode };
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
			args.test_key.signing_key().sign(&message).to_bytes()
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
	println!(
		"sign-manifest: {} - {} row(s), key {:#010x}, {} bytes, generation {}, dma-mode {}, verified",
		args.out,
		read_back.row_count(),
		key_id,
		record.len(),
		read_back.security_generation,
		match read_back.dma_mode.and_then(bootproto::dma_mode::Mode::from_code) {
			Some(mode) => mode.name(),
			None => "harness",
		}
	);
}
