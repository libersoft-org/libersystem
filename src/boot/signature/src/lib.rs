//! Whether a boot manifest was signed by a key this loader carries.
//!
//! ONE QUESTION, AND IT IS NOT "IS THIS MANIFEST WELL FORMED". `bootproto::manifest` answers that,
//! dependency-free, and hands back the exact payload a signature has to cover. This answers whether
//! a given key made that signature over that payload, and nothing else: no key management, no
//! rotation, no trust store - the loader's trust roots are compiled into it, which is what makes
//! them something an attacker with the medium cannot replace.

#![no_std]

use ed25519_dalek::{Signature, VerifyingKey};

/// Whether `signature` is `key`'s over `payload`, domain-separated.
///
/// THE DOMAIN GOES IN FRONT HERE rather than being stored in the manifest, so a manifest's bytes are
/// not a message some other protocol could be persuaded to sign. A signature that verifies here says
/// "this is a LiberSystem boot manifest", not merely "these bytes were signed".
///
/// `verify_strict` rather than `verify`: it rejects the small-order public keys and the non-canonical
/// encodings that make a signature verify under more than one key, which is a property a boot chain
/// wants and a chat protocol can live without.
pub fn verifies(key: &[u8; 32], signature: &[u8; 64], domain: &[u8], payload: &[u8], scratch: &mut [u8]) -> bool {
	let Ok(key) = VerifyingKey::from_bytes(key) else {
		return false;
	};
	// THE SCRATCH BUFFER IS THE CALLER'S, and that is not fastidiousness. Ed25519 hashes R, A and
	// the whole message together, so there is no streaming verify to hand two slices to - the
	// message has to be contiguous. A buffer this size on a loader's stack is a buffer on the
	// FIRMWARE'S stack, which is neither ours nor generous; the loader has a heap and can say where
	// this lives.
	//
	// (`verify_prehashed` would take a digest instead, but that is Ed25519ph - a different
	// algorithm with different signatures, not a streaming form of this one.)
	let Some(total) = domain.len().checked_add(payload.len()) else {
		return false;
	};
	if total > scratch.len() {
		return false;
	}
	scratch[..domain.len()].copy_from_slice(domain);
	scratch[domain.len()..total].copy_from_slice(payload);
	key.verify_strict(&scratch[..total], &Signature::from_bytes(signature)).is_ok()
}

#[cfg(test)]
mod tests {
	extern crate std;
	use std::vec;

	// A key and a signature made by the same library this verifies with, so the test is about the
	// wrapper's rules - the domain, the buffer, the strictness - rather than about Ed25519.
	fn signed(message: &[u8]) -> ([u8; 32], [u8; 64]) {
		use ed25519_dalek::{Signer, SigningKey};
		let signing = SigningKey::from_bytes(&[7u8; 32]);
		let signature = signing.sign(message);
		(signing.verifying_key().to_bytes(), signature.to_bytes())
	}

	const DOMAIN: &[u8] = b"libersystem-boot-manifest-v2\0";

	#[test]
	fn a_signature_over_the_domain_and_the_payload_verifies() {
		let payload = b"the canonical bytes";
		let mut message = vec![];
		message.extend_from_slice(DOMAIN);
		message.extend_from_slice(payload);
		let (key, signature) = signed(&message);
		let mut scratch = vec![0u8; 1024];
		assert!(super::verifies(&key, &signature, DOMAIN, payload, &mut scratch));
	}

	#[test]
	fn a_signature_over_the_payload_alone_does_not() {
		// THE WHOLE POINT OF THE DOMAIN. A signature made over the bytes without it is a signature
		// some other protocol could have produced over the same bytes, and it must not pass here.
		let payload = b"the canonical bytes";
		let (key, signature) = signed(payload);
		let mut scratch = vec![0u8; 1024];
		assert!(!super::verifies(&key, &signature, DOMAIN, payload, &mut scratch));
	}

	#[test]
	fn one_bit_anywhere_is_a_different_message() {
		let payload = b"the canonical bytes";
		let mut message = vec![];
		message.extend_from_slice(DOMAIN);
		message.extend_from_slice(payload);
		let (key, signature) = signed(&message);
		let mut scratch = vec![0u8; 1024];
		let mut altered = payload.to_vec();
		altered[0] ^= 1;
		assert!(!super::verifies(&key, &signature, DOMAIN, &altered, &mut scratch));
		let mut bad_sig = signature;
		bad_sig[0] ^= 1;
		assert!(!super::verifies(&key, &bad_sig, DOMAIN, payload, &mut scratch));
		let mut other_key = key;
		other_key[0] ^= 1;
		assert!(!super::verifies(&other_key, &signature, DOMAIN, payload, &mut scratch));
	}

	#[test]
	fn a_scratch_buffer_too_small_refuses_rather_than_verifying_a_prefix() {
		// The failure that would otherwise be silent: verifying the first N bytes of a message and
		// calling it the message.
		let payload = b"the canonical bytes";
		let mut message = vec![];
		message.extend_from_slice(DOMAIN);
		message.extend_from_slice(payload);
		let (key, signature) = signed(&message);
		let mut scratch = vec![0u8; message.len() - 1];
		assert!(!super::verifies(&key, &signature, DOMAIN, payload, &mut scratch));
		let mut exact = vec![0u8; message.len()];
		assert!(super::verifies(&key, &signature, DOMAIN, payload, &mut exact), "and exactly enough is enough");
	}
}
