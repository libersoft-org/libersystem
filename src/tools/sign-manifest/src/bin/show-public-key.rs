// The public half of a seed, so a build can be told which key its loader must carry.
//
// A SEPARATE COMMAND because the two halves have separate lives: the seed belongs to whoever signs,
// and the public key belongs in the loader and in configuration. Printing one from the other is the
// only place they meet, and it is a place with no secret in its output.

fn main() {
	let Some(seed) = std::env::args().nth(1) else {
		eprintln!("show-public-key: the seed is the first argument (64 hex characters)");
		std::process::exit(2)
	};
	let Some(seed) = (0..32).map(|i| u8::from_str_radix(seed.get(i * 2..i * 2 + 2).unwrap_or(""), 16).ok()).collect::<Option<Vec<u8>>>() else {
		eprintln!("show-public-key: the seed is 64 hex characters");
		std::process::exit(2)
	};
	let key = ed25519_dalek::SigningKey::from_bytes(&seed.try_into().expect("32 bytes"));
	for byte in key.verifying_key().to_bytes() {
		print!("{byte:02x}");
	}
	println!();
}
