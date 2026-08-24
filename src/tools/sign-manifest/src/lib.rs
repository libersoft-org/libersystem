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

// Encode a manifest, sign it with the published test key, and VERIFY WHAT WAS PRODUCED with the
// same parser and verifier the loader carries. A signing step that cannot check its own output
// moves every one of its failures to a boot.
pub fn sign_with_test_key(header: &bootproto::manifest::Header<'_>, rows: &mut [bootproto::manifest::Row<'_>]) -> Result<Vec<u8>, String> {
	if header.key_id != TEST_KEY_ID {
		return Err(String::from("a manifest signed with the test key must name the test key id"));
	}
	let mut record = vec![0u8; bootproto::manifest::MAX_MANIFEST_BYTES];
	let payload_len = bootproto::manifest::encode_payload(header, rows, &mut record).map_err(|e| format!("the manifest will not encode: {e:?}"))?;
	let mut message = Vec::with_capacity(bootproto::manifest::DOMAIN.len() + payload_len);
	message.extend_from_slice(bootproto::manifest::DOMAIN);
	message.extend_from_slice(&record[..payload_len]);
	let signing = ed25519_dalek::SigningKey::from_bytes(&TEST_SIGNING_KEY);
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
