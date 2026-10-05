//! THE KEY FUNCTIONS BEYOND LE SECURE CONNECTIONS' FOUR: `ah`, which resolves a peer's resolvable private address with
//! its identity key and makes this host's own; and `h6` and `h7`, through which a Secure Connections key on one radio
//! gives the other radio's key (cross-transport key derivation), so a peer paired once is bonded on both.
//!
//! The same discipline as `smp`: every value most significant first, each function a fixed message through AES or
//! CMAC, and each held to the specification's own sample data (Core, Vol 3, Part H, Appendix D).

use crate::aes::{BLOCK, Key};
use crate::cmac::mac;

#[cfg(test)]
mod tests;

/// `ah`, the random address hash: the low 24 bits of `e(k, 0^104 || r)`.
pub fn ah(irk: &[u8; BLOCK], prand: u32) -> u32 {
	let mut block = [0u8; BLOCK];
	block[13..].copy_from_slice(&prand.to_be_bytes()[1..]);
	let out = Key::new(irk).block(&block);
	u32::from_be_bytes([0, out[13], out[14], out[15]])
}

/// WHETHER A RESOLVABLE PRIVATE ADDRESS - most significant first, its top two bits `01` - was made with this identity
/// key: its high three bytes are `prand`, its low three `ah(irk, prand)`.
pub fn resolves(irk: &[u8; BLOCK], address: &[u8; 6]) -> bool {
	if address[0] >> 6 != 0b01 {
		return false;
	}
	let prand = u32::from_be_bytes([0, address[0], address[1], address[2]]);
	let hash = u32::from_be_bytes([0, address[3], address[4], address[5]]);
	ah(irk, prand) == hash
}

/// A RESOLVABLE PRIVATE ADDRESS for this host from its identity key and 22 random bits: `prand` with its top two bits
/// set to `01`, and the hash.
pub fn private_address(irk: &[u8; BLOCK], random: u32) -> [u8; 6] {
	let prand = (random & 0x3F_FFFF) | 0x40_0000;
	let hash = ah(irk, prand);
	let p = prand.to_be_bytes();
	let h = hash.to_be_bytes();
	[p[1], p[2], p[3], h[1], h[2], h[3]]
}

/// `h6(W, keyID) = AES-CMAC_W(keyID)`.
pub fn h6(w: &[u8; BLOCK], key_id: &[u8; 4]) -> [u8; BLOCK] {
	mac(&Key::new(w), key_id)
}

/// `h7(SALT, W) = AES-CMAC_SALT(W)`.
pub fn h7(salt: &[u8; BLOCK], w: &[u8; BLOCK]) -> [u8; BLOCK] {
	mac(&Key::new(salt), w)
}

// The salts `h7` takes - ASCII `tmp1` and `tmp2` in the low four bytes - and the key ids.
const SALT_TMP1: [u8; BLOCK] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, b't', b'm', b'p', b'1'];
const SALT_TMP2: [u8; BLOCK] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, b't', b'm', b'p', b'2'];

/// THE BR/EDR LINK KEY FROM AN LE SECURE CONNECTIONS LTK: through `h7` with the `tmp1` salt where both sides set the
/// CT2 bit, `h6` with `tmp1` otherwise, then `h6` with `lebr`.
pub fn link_key_from_ltk(ltk: &[u8; BLOCK], ct2: bool) -> [u8; BLOCK] {
	let ilk = if ct2 { h7(&SALT_TMP1, ltk) } else { h6(ltk, b"tmp1") };
	h6(&ilk, b"lebr")
}

/// THE LE LTK FROM A BR/EDR SECURE CONNECTIONS LINK KEY: `h7` with `tmp2` (CT2) or `h6` with `tmp2`, then `brle`.
pub fn ltk_from_link_key(link_key: &[u8; BLOCK], ct2: bool) -> [u8; BLOCK] {
	let ilk = if ct2 { h7(&SALT_TMP2, link_key) } else { h6(link_key, b"tmp2") };
	h6(&ilk, b"brle")
}
