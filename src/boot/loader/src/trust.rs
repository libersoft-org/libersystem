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

// WHAT THIS BOOT IS, so a signature can be checked against it rather than only checked.
//
// A SIGNATURE OVER FACTS NOBODY COMPARES IS A SIGNATURE OVER NOTHING. The manifest format signs the
// product, the architecture, the kind of source and the volume's identity, and every one of those
// was decoded and then ignored: what the check asserted was "a key this loader carries signed this
// manifest", and what it was read as asserting was "…and this manifest is for THIS machine, THIS
// architecture and THIS source". A correctly signed manifest for another product, another port or
// another medium passed. So the caller now has to say what it expects before it can ask.
pub(crate) struct Expected {
	pub product: &'static [u8],
	pub arch: u8,
	pub source_kind: u8,
	// Which volume this manifest must be for. THREE STATES, BECAUSE THERE ARE THREE SITUATIONS and
	// two of them are not "no uuid": a boot medium is not a volume and its manifest must name none;
	// a paired volume must be the one the medium named; and a volume reached without a pairing file
	// is a volume this boot cannot put a name to, which is a weaker claim than either and is stated
	// as one rather than being folded into a wildcard or a refusal.
	pub volume_uuid: VolumeIdentity,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum VolumeIdentity {
	// A source that is not a volume - the boot medium. Its manifest's volume field is not a claim
	// about the source; it is the PAIRING, naming which volume this medium belongs with, and zero
	// there means it names none. So there is nothing to compare it against, and the value is read
	// out of the verified manifest rather than checked against something read elsewhere.
	NotAVolume,
	// A volume, and the boot medium said which one. Anything else is a signed release being used on
	// a volume it was not made for, which is what pairing exists to stop.
	Exactly([u8; 16]),
	// A volume, and this boot has nothing to compare its name against - the medium named none. The
	// manifest must still be a volume's, so a boot-medium manifest cannot be presented as one.
	Unnamed,
}

// The architecture this loader was built for. A cfg rather than a value passed in: a loader cannot
// be wrong about which port it is, and nothing should be able to tell it otherwise.
pub(crate) const THIS_ARCH: u8 = if cfg!(target_arch = "x86_64") {
	bootproto::manifest::ARCH_X86_64
} else if cfg!(target_arch = "aarch64") {
	bootproto::manifest::ARCH_AARCH64
} else {
	bootproto::manifest::ARCH_RISCV64
};

// The product this loader belongs to. Compiled in for the same reason the keys are.
//
// THE THIRD COPY OF `PRODUCT_NAME`, and that is a fact worth writing down rather than a thing to be
// pleased about: `product.conf` is the single source of truth, and `mkpackages` and `mkimage.sh` both
// hold the literal too. They agree today. If one of them ever did not, every boot would refuse for a
// reason that reads like tampering - so whoever changes the product's name changes it in all four
// places, and the milestone that gives the loader a build script reading `product.conf` removes this
// one.
pub(crate) const THIS_PRODUCT: &[u8] = b"LiberSystem";

impl Expected {
	pub(crate) fn source(source_kind: u8, volume_uuid: VolumeIdentity) -> Expected {
		Expected { product: THIS_PRODUCT, arch: THIS_ARCH, source_kind, volume_uuid }
	}

	// A volume, named by the boot medium's pairing file where it has one.
	pub(crate) fn volume(paired: Option<[u8; 16]>) -> Expected {
		let identity = match paired {
			Some(uuid) => VolumeIdentity::Exactly(uuid),
			None => VolumeIdentity::Unnamed,
		};
		Expected::source(bootproto::manifest::SOURCE_SYSTEM_VOLUME, identity)
	}

	// WHETHER THIS SOURCE WAS CHOSEN, which decides what a missing named file on it means.
	//
	// Two ways to be chosen, and both are statements somebody signed. The medium's manifest NAMED
	// this volume, so the pairing selected it; or this source's own verified manifest has a row for
	// `etc/bootstrap.list`, which is the manifest saying the file is there. A source that is neither
	// - a volume nothing named, whose manifest does not mention a list - is one this boot may leave
	// for another, and that is the only case an absence is an absence.
	pub(crate) fn selects_its_source(&self, manifest: &bootproto::manifest::Manifest<'_>) -> bool {
		matches!(self.volume_uuid, VolumeIdentity::Exactly(_)) || manifest.find(bootproto::manifest::KIND_BOOTSTRAP_LIST, b"etc/bootstrap.list").is_some()
	}

	// The medium this loader itself came off. Not a volume.
	pub(crate) fn medium() -> Expected {
		Expected::source(bootproto::manifest::SOURCE_BOOT_MEDIUM, VolumeIdentity::NotAVolume)
	}
}

// Whether this manifest was signed by a key this loader carries AND was written for this boot.
//
// FIVE REFUSALS, AND THEY ARE DIFFERENT MACHINES TO BE STANDING IN FRONT OF: bytes that are not a
// manifest, a manifest naming a key this loader does not have, a manifest whose signature does not
// check out, a manifest signed for a different product or port, and one signed for a different
// source or volume. The first is a medium that predates signing; the second is somebody else's
// release; the third is tampering; the last two are a valid release being pointed at a machine it
// was not made for.
pub(crate) fn verify_for<'a>(bytes: &'a [u8], expected: &Expected, scratch: &mut [u8]) -> Option<bootproto::manifest::Manifest<'a>> {
	let manifest = verify(bytes, scratch)?;
	if manifest.product != expected.product {
		crate::arch::serial::write_str("loader: the manifest is signed for another product - refusing to boot from it\n");
		return None;
	}
	if manifest.arch != expected.arch {
		crate::arch::serial::write_str("loader: the manifest is signed for another architecture - refusing to boot from it\n");
		return None;
	}
	if manifest.source_kind != expected.source_kind {
		crate::arch::serial::write_str("loader: the manifest is signed for another kind of source - refusing to boot from it\n");
		return None;
	}
	match expected.volume_uuid {
		VolumeIdentity::Exactly(uuid) => {
			if manifest.volume_uuid != uuid {
				crate::arch::serial::write_str("loader: the manifest is signed for a different volume than the one this medium is paired with - refusing to boot from it\n");
				return None;
			}
		}
		VolumeIdentity::Unnamed => {
			if manifest.volume_uuid == [0u8; 16] {
				crate::arch::serial::write_str("loader: this source is a volume and its manifest names none - refusing to boot from it\n");
				return None;
			}
		}
		// THE PAIRING IS THE POINT OF THIS FIELD ON A BOOT MEDIUM, so a non-zero value here is not a
		// contradiction to refuse - it is the medium saying which volume it belongs with, signed.
		// This used to require a zero, because the pairing lived beside the manifest as a plain text
		// file that nothing signed: anyone who could write the medium could repoint it at another
		// signed volume, or delete it and get "any signed volume, whichever disk enumerates first".
		VolumeIdentity::NotAVolume => {}
	}
	if !same_release(manifest.release) {
		crate::arch::serial::write_str("loader: this source belongs to a different release than the one already verified in this boot - refusing to compose a system from two of them\n");
		return None;
	}
	Some(manifest)
}

// THE RELEASE THIS BOOT IS, LATCHED BY THE FIRST THING VERIFIED.
//
// Every manifest was verified on its own and none was compared with any other, so a kernel from one
// signed release and a bootstrap set from another - each perfectly valid, each signed by the same
// key - composed into a system nobody ever built or tested. The first verification in a boot fixes
// which release this is; every later one has to agree.
static mut RELEASE: [u8; bootproto::manifest::MAX_NAME_BYTES] = [0u8; bootproto::manifest::MAX_NAME_BYTES];
static mut RELEASE_LEN: usize = 0;

fn same_release(release: &[u8]) -> bool {
	// SAFETY: the loader is single-threaded and this is reached only from its own boot path. The
	// raw pointer rather than a reference is what keeps this from being a shared reference to a
	// mutable static, which the compiler refuses for the reason this comment exists.
	unsafe {
		let held: *mut u8 = (&raw mut RELEASE).cast();
		if RELEASE_LEN == 0 {
			if release.len() > bootproto::manifest::MAX_NAME_BYTES {
				return false;
			}
			core::ptr::copy_nonoverlapping(release.as_ptr(), held, release.len());
			RELEASE_LEN = release.len();
			return true;
		}
		if RELEASE_LEN != release.len() {
			return false;
		}
		core::slice::from_raw_parts(held, RELEASE_LEN) == release
	}
}

// Whether this manifest was signed by a key this loader carries, and what it says if it was.
fn verify<'a>(bytes: &'a [u8], scratch: &mut [u8]) -> Option<bootproto::manifest::Manifest<'a>> {
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
