// Signing a boot manifest, as a library so the build does not have to shell out to a binary to make
// one - and so the TEST KEY exists in exactly one place.
//
// THE VERIFIER IS A DIFFERENT CRATE ON PURPOSE. `bootsig` is linked into the loader, and a loader
// that carried a signing key could sign what it verifies, which is the same as verifying nothing.
// Nothing here is `no_std`, nothing here goes near a boot.

// THE PUBLISHED TEST KEY'S PRIVATE HALF, in the open on purpose. A fixture key that is secret is a
// fixture nobody can reproduce a build with; one that is published cannot be mistaken for a release
// key - and `check.sh --gate trust-profile` proves it cannot reach a release loader.
pub const TEST_SIGNING_KEY: [u8; 32] = [
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

// THE PUBLISHED RECOVERY TEST KEY'S PRIVATE HALF, in the open for the same reason. Its public half
// is scoped to the recovery purpose in the loader, and this one exists so the cross-use negatives
// - a recovery set signed with the boot key, an ordinary set signed with this one - can be made.
// The seed is SHA-256 of the sentence in the comment of the gate that uses it, so it is
// reproducible without this file.
pub const TEST_RECOVERY_SIGNING_KEY: [u8; 32] = [
	0x17,
	0x4c,
	0xe8,
	0x79,
	0x40,
	0x38,
	0xab,
	0x36,
	0x13,
	0x47,
	0x92,
	0xc9,
	0xe9,
	0xc7,
	0x21,
	0xbe,
	0x93,
	0xfc,
	0x56,
	0x17,
	0xd1,
	0xf1,
	0xe7,
	0x20,
	0x2f,
	0x45,
	0x4a,
	0x59,
	0xad,
	0xb7,
	0x29,
	0xe0,
];
pub const TEST_RECOVERY_KEY_ID: u32 = 0x7e57_0002;

// Which published key signs. The purpose a manifest CLAIMS is a header field; which key signs it
// is this - and the two are chosen independently on purpose, so a manifest can be made that claims
// a purpose its key may not sign for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TestKey {
	Boot,
	Recovery,
}

impl TestKey {
	pub fn key_id(self) -> u32 {
		match self {
			TestKey::Boot => TEST_KEY_ID,
			TestKey::Recovery => TEST_RECOVERY_KEY_ID,
		}
	}

	pub fn signing_key(self) -> ed25519_dalek::SigningKey {
		ed25519_dalek::SigningKey::from_bytes(match self {
			TestKey::Boot => &TEST_SIGNING_KEY,
			TestKey::Recovery => &TEST_RECOVERY_SIGNING_KEY,
		})
	}
}

// Encode a manifest, sign it with the published test key, and VERIFY WHAT WAS PRODUCED with the
// same parser and verifier the loader carries. A signing step that cannot check its own output
// moves every one of its failures to a boot.
pub fn sign_with_test_key(header: &bootproto::manifest::Header<'_>, rows: &mut [bootproto::manifest::Row<'_>]) -> Result<Vec<u8>, String> {
	sign_with_published_key(header, rows, TestKey::Boot)
}

// The same, with the published key named: the boot key for an ordinary set, the recovery key for a
// recovery set - and the header must name that key's id, because a manifest naming one key and
// signed with another is a manifest the loader refuses before it reads a row.
pub fn sign_with_published_key(header: &bootproto::manifest::Header<'_>, rows: &mut [bootproto::manifest::Row<'_>], key: TestKey) -> Result<Vec<u8>, String> {
	if header.key_id != key.key_id() {
		return Err(String::from("a manifest signed with a published key must name that key's id"));
	}
	let mut record = vec![0u8; bootproto::manifest::MAX_MANIFEST_BYTES];
	let payload_len = bootproto::manifest::encode_payload(header, rows, &mut record).map_err(|e| format!("the manifest will not encode: {e:?}"))?;
	let mut message = Vec::with_capacity(bootproto::manifest::DOMAIN.len() + payload_len);
	message.extend_from_slice(bootproto::manifest::DOMAIN);
	message.extend_from_slice(&record[..payload_len]);
	let signing = key.signing_key();
	let signature = {
		use ed25519_dalek::Signer;
		signing.sign(&message).to_bytes()
	};
	record[payload_len..payload_len + 64].copy_from_slice(&signature);
	record.truncate(payload_len + 64);

	let read_back = bootproto::manifest::Manifest::decode(&record).map_err(|e| format!("what this wrote does not parse: {e:?}"))?;
	let mut scratch = vec![0u8; bootproto::manifest::DOMAIN.len() + payload_len];
	if !bootsig::verifies(&signing.verifying_key().to_bytes(), &read_back.signature(), bootproto::manifest::DOMAIN, read_back.payload(), &mut scratch) {
		return Err(String::from("the signature this made does not verify"));
	}
	Ok(record)
}

#[cfg(test)]
mod tests {
	use super::*;
	use bootproto::manifest::*;

	// A signed manifest a test can take apart. Two rows and a full header, so every field a mutation
	// could reach is present.
	fn signed() -> (Vec<u8>, [u8; 32]) {
		let mut rows = [Row { kind: KIND_KERNEL, path: b"kernel", length: 4, digest: sha(b"abcd") }, Row { kind: KIND_PROGRAM, path: b"libexec/init", length: 2, digest: sha(b"hi") }];
		let header = Header { key_id: TEST_KEY_ID, product: b"LiberSystem", arch: ARCH_X86_64, source_kind: SOURCE_SYSTEM_VOLUME, release: b"0.0.1", security_generation: 1, purpose: PURPOSE_BOOT, volume_uuid: [0x33; 16], dma_mode: None };
		let record = sign_with_test_key(&header, &mut rows).expect("it signs");
		let public = ed25519_dalek::SigningKey::from_bytes(&TEST_SIGNING_KEY).verifying_key().to_bytes();
		(record, public)
	}

	fn sha(bytes: &[u8]) -> [u8; 32] {
		bootproto::sha256::digest(bytes)
	}

	// Decode and verify, the way the loader does: the parser first, then the signature over exactly
	// the payload the parser hands back.
	fn accepted(record: &[u8], public: &[u8; 32]) -> bool {
		let Ok(manifest) = Manifest::decode(record) else { return false };
		let mut scratch = vec![0u8; DOMAIN.len() + manifest.payload().len()];
		bootsig::verifies(public, &manifest.signature(), DOMAIN, manifest.payload(), &mut scratch)
	}

	#[test]
	fn what_was_signed_verifies() {
		let (record, public) = signed();
		assert!(accepted(&record, &public), "the manifest this tool made verifies with the key it names");
	}

	#[test]
	fn every_byte_of_the_payload_is_covered() {
		// THE MUTATION SWEEP, and it is every byte rather than every field: a field this test forgot
		// to name is a field nothing would have checked. Each one flipped, one at a time, and the
		// manifest must be refused by the parser or by the signature - it does not matter which,
		// only that nothing gets through.
		let (record, public) = signed();
		let payload_len = record.len() - 64;
		for index in 0..payload_len {
			let mut altered = record.clone();
			altered[index] ^= 0x01;
			assert!(!accepted(&altered, &public), "byte {index} of the payload changed and the manifest was still accepted");
		}
	}

	#[test]
	fn every_byte_of_the_signature_is_covered() {
		let (record, public) = signed();
		let payload_len = record.len() - 64;
		for index in payload_len..record.len() {
			let mut altered = record.clone();
			altered[index] ^= 0x01;
			assert!(!accepted(&altered, &public), "byte {index} of the signature changed and the manifest was still accepted");
		}
	}

	#[test]
	fn a_payload_changed_after_signing_fails_and_so_does_a_manifest_changed_without_it() {
		// The two directions the milestone names, stated as themselves rather than as bytes: a
		// digest that no longer describes the file, and a file that no longer matches its digest.
		// Both are the same refusal here because the manifest is what is signed - which is the
		// point of signing the manifest rather than each file.
		let (record, public) = signed();
		let manifest = Manifest::decode(&record).expect("decodes");
		let row = manifest.find(KIND_KERNEL, b"kernel").expect("the kernel row");
		assert_eq!(row.digest, sha(b"abcd"), "the row records the file it was made from");
		assert_ne!(row.digest, sha(b"abce"), "and a file changed after signing does not match it");

		// The manifest changed without the payload: the digest is edited to match a different file.
		let mut altered = record.clone();
		let at = altered.windows(32).position(|window| window == sha(b"abcd")).expect("the digest is in the record");
		altered[at..at + 32].copy_from_slice(&sha(b"abce"));
		assert!(!accepted(&altered, &public), "a manifest edited to name different content is refused");
	}

	#[test]
	fn a_signature_from_another_key_is_not_this_manifest_s() {
		let (record, _) = signed();
		let other = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32]).verifying_key().to_bytes();
		assert!(!accepted(&record, &other), "a key that did not sign it does not verify it");
	}

	#[test]
	fn a_manifest_for_another_product_or_architecture_is_a_different_manifest() {
		// Signed correctly, by the right key, over the wrong machine. The signature is valid; what
		// refuses it is the loader reading `arch` and `product` - so this test asserts the FIELDS
		// carry what a loader would refuse on, and that changing them invalidates the signature.
		let (record, public) = signed();
		let manifest = Manifest::decode(&record).expect("decodes");
		assert_eq!(manifest.arch, ARCH_X86_64);
		assert_eq!(manifest.product, b"LiberSystem");
		assert_eq!(manifest.source_kind, SOURCE_SYSTEM_VOLUME);
		assert_eq!(manifest.key_id, TEST_KEY_ID);
		assert_eq!(manifest.volume_uuid, [0x33; 16]);

		let mut rows = [Row { kind: KIND_KERNEL, path: b"kernel", length: 4, digest: sha(b"abcd") }];
		let other = Header { key_id: TEST_KEY_ID, product: b"SomethingElse", arch: ARCH_AARCH64, source_kind: SOURCE_BOOT_MEDIUM, release: b"0.0.1", security_generation: 1, purpose: PURPOSE_BOOT, volume_uuid: [0; 16], dma_mode: None };
		let elsewhere = sign_with_test_key(&other, &mut rows).expect("it signs");
		let read = Manifest::decode(&elsewhere).expect("decodes");
		assert!(accepted(&elsewhere, &public), "it is correctly signed - the signature is not what makes it wrong");
		assert_ne!(read.arch, ARCH_X86_64, "and it is not for this machine");
		assert_ne!(read.product, b"LiberSystem");
	}

	#[test]
	fn a_signed_dma_mode_round_trips_and_its_presence_tag_is_covered_by_the_signature() {
		// The DMA record travels inside the signed payload: a tag-1 manifest reads back its mode, and
		// altering the presence tag without resigning fails signature validation - which the
		// every-byte sweep above also proves, byte by byte, and this states for the one byte the
		// milestone names.
		let mut rows = [Row { kind: KIND_KERNEL, path: b"kernel", length: 4, digest: sha(b"abcd") }];
		let header = Header { key_id: TEST_KEY_ID, product: b"LiberSystem", arch: ARCH_X86_64, source_kind: SOURCE_BOOT_MEDIUM, release: b"0.0.1", security_generation: 1, purpose: PURPOSE_BOOT, volume_uuid: [0x44; 16], dma_mode: Some(bootproto::dma_mode::MODE_NO_IOMMU) };
		let record = sign_with_test_key(&header, &mut rows).expect("it signs");
		let public = ed25519_dalek::SigningKey::from_bytes(&TEST_SIGNING_KEY).verifying_key().to_bytes();
		assert!(accepted(&record, &public));
		let read = Manifest::decode(&record).expect("decodes");
		assert_eq!(read.dma_mode, Some(bootproto::dma_mode::MODE_NO_IOMMU));
		// The tag byte is the one right after the sixteen-byte uuid.
		let tag_at = record.windows(16).position(|window| window == [0x44u8; 16]).expect("the uuid is in the record") + 16;
		assert_eq!(record[tag_at], DMA_MODE_PRESENT);
		let mut cleared = record.clone();
		cleared[tag_at] = DMA_MODE_ABSENT;
		assert!(!accepted(&cleared, &public), "a presence tag altered without resigning fails validation");
		let mut flipped = record.clone();
		flipped[tag_at + 1] = bootproto::dma_mode::MODE_ENFORCING_REQUIRED as u8;
		assert!(!accepted(&flipped, &public), "and so does the mode value");
	}

	#[test]
	fn an_older_correctly_signed_release_still_verifies() {
		// THE POSITIVE TEST THAT EXISTS TO STOP A CLAIM. This milestone authenticates; it does not
		// enforce freshness. An old release signed with the same key verifies exactly as a new one
		// does - and it must, because the alternative would be an anti-rollback property the
		// mechanism does not have and the documentation must never acquire.
		let mut rows = [Row { kind: KIND_KERNEL, path: b"kernel", length: 4, digest: sha(b"abcd") }];
		let old = Header { key_id: TEST_KEY_ID, product: b"LiberSystem", arch: ARCH_X86_64, source_kind: SOURCE_SYSTEM_VOLUME, release: b"0.0.0", security_generation: 1, purpose: PURPOSE_BOOT, volume_uuid: [0x33; 16], dma_mode: None };
		let record = sign_with_test_key(&old, &mut rows).expect("it signs");
		let public = ed25519_dalek::SigningKey::from_bytes(&TEST_SIGNING_KEY).verifying_key().to_bytes();
		assert!(accepted(&record, &public), "an older release signed by the same key verifies");
		assert_eq!(Manifest::decode(&record).expect("decodes").release, b"0.0.0");
	}

	#[test]
	fn a_signature_over_a_shorter_payload_is_not_over_this_manifest() {
		// The refusal the API makes impossible, asserted anyway: a signature made over a PREFIX of
		// the payload does not verify against the whole of it. `payload()` returns all of it and
		// there is no accessor for less, so this is the property that keeps that true.
		let (record, public) = signed();
		let manifest = Manifest::decode(&record).expect("decodes");
		let payload = manifest.payload();
		let mut message = Vec::new();
		message.extend_from_slice(DOMAIN);
		message.extend_from_slice(&payload[..payload.len() - 1]);
		let signature = {
			use ed25519_dalek::Signer;
			ed25519_dalek::SigningKey::from_bytes(&TEST_SIGNING_KEY).sign(&message).to_bytes()
		};
		let mut scratch = vec![0u8; DOMAIN.len() + payload.len()];
		assert!(!bootsig::verifies(&public, &signature, DOMAIN, payload, &mut scratch), "a signature over a prefix is not one over the record");
	}
}
