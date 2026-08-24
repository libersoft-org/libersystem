// The public keys this loader will accept a boot manifest from, and nothing else.
//
// PUBLIC HALVES ONLY. A loader that carried a private key could sign, and a thing that can sign what
// it verifies verifies nothing. These are compiled in rather than read from the medium, which is the
// whole of what makes them something an attacker holding the disk cannot replace: a manifest names a
// KEY ID, and a key id this loader does not carry is a refusal - it can never nominate a key.
//
// TWO PROFILES, CHOSEN AT BUILD TIME AND NAMED IN THE BINARY.
//
// `test-trust` is the host/QEMU-closing profile. Its key's private half is a published fixture, so a
// build made with it is reproducible and cannot be mistaken for a release. The loader says TEST
// TRUST before it loads anything, because a boot that trusts a published key and does not say so is
// the failure this whole milestone is about.
//
// `external-release` carries one key given at build time - `LIBER_TRUST_KEY` and
// `LIBER_TRUST_KEY_ID` - and no test key at all. A build that asks for it without them does not
// compile, which is the only place the failure costs nothing.

// The published test key's PUBLIC half. Its private half is in `tools/sign-manifest`, in the open.
const TEST_KEY: [u8; 32] = hex32("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
const TEST_KEY_ID: u32 = 0x7e57_0001;

// The marker a test-trust loader prints and a release loader does not contain. The gate greps for
// exactly this string, so it is written once and never assembled from pieces.
pub const TEST_TRUST_MARKER: &str = "TEST TRUST (published key)";

// Which profile this binary was built for. `test-trust` unless the build says otherwise, because a
// developer's build should be the one that cannot be mistaken for a release.
const PROFILE: &str = match option_env!("LIBER_TRUST_PROFILE") {
	Some(profile) => profile,
	None => "test-trust",
};

pub const IS_TEST_TRUST: bool = konst_eq(PROFILE, "test-trust");

// One key this loader accepts manifests from.
pub struct Root {
	pub key_id: u32,
	pub key: [u8; 32],
}

// THE ROOTS, and the compile stops here if a release build was asked for without a key. "Fails
// before an image is written" is a rule about when, and there is no earlier when than this.
pub const ROOTS: &[Root] = if IS_TEST_TRUST { &[Root { key_id: TEST_KEY_ID, key: TEST_KEY }] } else { &[Root { key_id: release_key_id(), key: hex32(release_key()) }] };

const fn release_key() -> &'static str {
	match option_env!("LIBER_TRUST_KEY") {
		Some(key) => key,
		None => panic!("an external-release loader needs LIBER_TRUST_KEY: the public key it will accept manifests from"),
	}
}

const fn release_key_id() -> u32 {
	match option_env!("LIBER_TRUST_KEY_ID") {
		Some(id) => match konst_u32(id) {
			Some(id) => id,
			None => panic!("LIBER_TRUST_KEY_ID is a decimal number"),
		},
		None => panic!("an external-release loader needs LIBER_TRUST_KEY_ID: the key id its manifests will name"),
	}
}

// The root for `key_id`, or None - which is a refusal rather than a fallback. A loader that tried
// the next key when one did not match would accept a manifest signed by any key it carries, which
// is not what a key id is for.
pub fn root_for(key_id: u32) -> Option<&'static Root> {
	ROOTS.iter().find(|root| root.key_id == key_id)
}

// Compile-time helpers. Small enough to read, and they exist because a key belongs in the binary
// rather than in a file the binary reads.
const fn konst_eq(a: &str, b: &str) -> bool {
	let (a, b) = (a.as_bytes(), b.as_bytes());
	if a.len() != b.len() {
		return false;
	}
	let mut i = 0;
	while i < a.len() {
		if a[i] != b[i] {
			return false;
		}
		i += 1;
	}
	true
}

const fn konst_u32(text: &str) -> Option<u32> {
	let bytes = text.as_bytes();
	if bytes.is_empty() {
		return None;
	}
	let mut value: u32 = 0;
	let mut i = 0;
	while i < bytes.len() {
		let digit = bytes[i];
		if digit < b'0' || digit > b'9' {
			return None;
		}
		value = match value.checked_mul(10) {
			Some(value) => value,
			None => return None,
		};
		value = match value.checked_add((digit - b'0') as u32) {
			Some(value) => value,
			None => return None,
		};
		i += 1;
	}
	Some(value)
}

const fn hex32(text: &str) -> [u8; 32] {
	let bytes = text.as_bytes();
	if bytes.len() != 64 {
		panic!("a public key is 64 hex characters");
	}
	let mut out = [0u8; 32];
	let mut i = 0;
	while i < 32 {
		out[i] = nibble(bytes[i * 2]) << 4 | nibble(bytes[i * 2 + 1]);
		i += 1;
	}
	out
}

const fn nibble(byte: u8) -> u8 {
	match byte {
		b'0'..=b'9' => byte - b'0',
		b'a'..=b'f' => byte - b'a' + 10,
		b'A'..=b'F' => byte - b'A' + 10,
		_ => panic!("a public key is hex"),
	}
}

// Whether this manifest was signed by a key this loader carries, and what it says if it was.
//
// THREE REFUSALS, AND THEY ARE DIFFERENT MACHINES TO BE STANDING IN FRONT OF: bytes that are not a
// manifest, a manifest naming a key this loader does not have, and a manifest whose signature does
// not check out. The first is a medium that predates signing; the second is somebody else's
// release; the third is tampering.
pub(crate) fn verify<'a>(bytes: &'a [u8], scratch: &mut [u8]) -> Option<bootproto::manifest::Manifest<'a>> {
	let manifest = match bootproto::manifest::Manifest::decode(bytes) {
		Ok(manifest) => manifest,
		Err(reason) => {
			crate::arch::serial::write_str("loader: etc/boot.manifest2 is not a manifest this loader reads (");
			crate::arch::serial::write_str(match reason {
				bootproto::manifest::Refusal::NotAManifest => "not this format",
				bootproto::manifest::Refusal::TooLarge => "too large",
				bootproto::manifest::Refusal::Truncated => "truncated",
				bootproto::manifest::Refusal::TrailingBytes => "trailing bytes",
				bootproto::manifest::Refusal::UnknownValue => "a value this version does not define",
				bootproto::manifest::Refusal::TooManyRows => "too many rows",
				bootproto::manifest::Refusal::InvalidPath => "a path no manifest may name",
				bootproto::manifest::Refusal::NotCanonical => "rows out of canonical order",
				bootproto::manifest::Refusal::Overflow => "an extent that overflows",
			});
			crate::arch::serial::write_str(")\n");
			return None;
		}
	};
	let Some(root) = root_for(manifest.key_id) else {
		crate::arch::serial::write_str("loader: the manifest names a key this loader does not carry - refusing to boot from it\n");
		return None;
	};
	if !bootsig::verifies(&root.key, &manifest.signature(), bootproto::manifest::DOMAIN, manifest.payload(), scratch) {
		crate::arch::serial::write_str("loader: the manifest's signature does not check out - refusing to boot from it\n");
		return None;
	}
	Some(manifest)
}

// Say which keys this loader trusts, BEFORE it loads anything. A boot that trusts a published key
// and does not say so is the failure this milestone is named for.
pub(crate) fn announce() {
	if IS_TEST_TRUST {
		crate::arch::serial::write_str("loader: ");
		crate::arch::serial::write_str(TEST_TRUST_MARKER);
		crate::arch::serial::write_str(" - this build accepts a manifest signed by a key whose private half is published in this repository\n");
	} else {
		crate::arch::serial::write_str("loader: release trust - one compiled-in public key\n");
	}
}
