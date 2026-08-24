// A stand-in for the one executable that holds a release private key.
//
// IT EXISTS TO TEST THE CONTRACT, not to sign anything anybody should trust: it takes the seed on
// its command line, which is the opposite of what a real signer does. What it proves is that the
// contract `sign-manifest` states - the exact canonical message on stdin, exactly one raw 64-byte
// signature on stdout, and nothing else - is one a separate program can meet.
//
// A REAL SIGNER IS SOMEBODY ELSE'S PROGRAM. That is why `sign-manifest` verifies what comes back
// rather than trusting it: an executable that signs something other than what it was handed, or
// that prints a banner first, produces a manifest that fails at boot rather than at build time.

use std::io::{Read, Write};

fn main() {
	let Some(seed) = std::env::args().nth(1) else {
		eprintln!("test-signer: the seed is the first argument (64 hex characters)");
		std::process::exit(2)
	};
	let Some(seed) = (0..32).map(|i| u8::from_str_radix(seed.get(i * 2..i * 2 + 2).unwrap_or(""), 16).ok()).collect::<Option<Vec<u8>>>() else {
		eprintln!("test-signer: the seed is 64 hex characters");
		std::process::exit(2)
	};
	let mut message = Vec::new();
	std::io::stdin().read_to_end(&mut message).expect("read the message");
	use ed25519_dalek::Signer;
	let key = ed25519_dalek::SigningKey::from_bytes(&seed.try_into().expect("32 bytes"));
	std::io::stdout().write_all(&key.sign(&message).to_bytes()).expect("write the signature");
}
