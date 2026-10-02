//! THE HIBERNATION IMAGE, AS THE IMAGE COMPONENT WRITES IT AND CHECKS IT - its layout on the hibernation partition, its
//! cryptography and the refusals, pure and host-tested. The component does the IO: the snapshot's pages from the
//! kernel, the sealing through TpmService, the partition through StorageService.
//!
//! THE PARTITION: one header block, then the chunks. A chunk is a head block - the page frame numbers of its pages, its
//! page count and its tag - followed by its pages, encrypted:
//!
//!   0            the header (`HEADER_BYTES`)
//!   HEADER_BYTES chunk 0: head (`CHUNK_HEAD`), then `CHUNK_PAGES` pages at most
//!   ...          chunk 1, and so on - every chunk but the last is full
//!
//! THE KEYS. A fresh IMAGE KEY - an encryption half and a MAC half, AES-128 each - encrypts every page in counter mode
//! and authenticates every chunk with CMAC: the chunk's index, its frame numbers and its ciphertext, so a chunk cannot
//! be moved, cut or altered unseen. The image key is stored in the header WRAPPED by a fresh 32-byte KEY-ENCRYPTION KEY
//! - counter mode under its first half - and the header's fields and the wrapped key are authenticated under its
//! second half; only the key-encryption key leaves this component, sealed through TpmService, and its sealed blob is in
//! the header. No key is ever on the disk in the clear - EXCEPT WHERE NO TPM SEALS, which the owner allows with a warning
//! (2026-10-02): the header then says so (`KeyProtection::Clear`) and holds the key-encryption key itself where the
//! sealed blob goes. Such an image is still authenticated - a corrupted one is refused - but anyone who can read the disk
//! reads every secret that was in memory, and anyone who can write it can make the machine resume what they wrote. So a
//! machine whose TPM seals refuses such an image (`Refusal::ClearOnSealingMachine`): an image in the clear is no way
//! around the TPM's seal.
//!
//! THE REFUSALS: a header that is not an image, or is invalidated; an image whose authentication fails anywhere; one
//! whose key does not unseal (the caller's, from TpmService's answer); one written by another system image; one
//! describing other hardware. Each is `Refusal`, and every one of them boots the machine fresh.

use crate::aes::{BLOCK, KEY, Key};
use crate::cmac::Stream;
use alloc::vec::Vec;

/// The header's first eight bytes - `partition`'s, which the system volume's service reads at mount.
pub const MAGIC: [u8; 8] = partition::HIBERNATION_IMAGE_MAGIC;
pub const VERSION: u32 = 1;
/// A header that holds an image, and one that holds none - never written, or invalidated after a restore or a refusal.
pub const STATE_IMAGE: u32 = partition::HIBERNATION_STATE_IMAGE;
pub const STATE_EMPTY: u32 = 0;
/// The page, the header and a chunk's head, in bytes; a chunk's pages.
pub const PAGE: usize = 4096;
pub const HEADER_BYTES: usize = 4096;
pub const CHUNK_HEAD: usize = 4096;
pub const CHUNK_PAGES: usize = 256;
/// The largest sealed blob TpmService answers.
pub const SEALED_MAX: usize = 1024;
/// The kernel's resume context, carried opaque: what the kernel's snapshot answered, handed back at the restore.
pub const CONTEXT: usize = 64;
/// The key-encryption key: an encryption half and a MAC half.
pub const KEK: usize = 32;

/// THE PARTITION TYPE: LiberSystem hibernation - `partition`'s.
pub const PARTITION_TYPE: [u8; 16] = partition::HIBERNATION_TYPE_GUID;

// The header's offsets.
const AT_MAGIC: usize = 0;
const AT_VERSION: usize = 8;
const AT_STATE: usize = partition::HIBERNATION_STATE_AT;
const AT_SEALED_LEN: usize = 16;
const AT_PROTECTION: usize = 20;
const AT_SEALED: usize = 24;
const AT_NONCE: usize = AT_SEALED + SEALED_MAX;
const AT_WRAPPED: usize = AT_NONCE + 8;
const AT_PAGES: usize = AT_WRAPPED + 2 * KEY;
const AT_CREATED: usize = AT_PAGES + 8;
const AT_SYSTEM: usize = AT_CREATED + 8;
const AT_HARDWARE: usize = AT_SYSTEM + 32;
const AT_CONTEXT: usize = AT_HARDWARE + 32;
const AT_MAC: usize = AT_CONTEXT + CONTEXT;
const HEADER_USED: usize = AT_MAC + BLOCK;

// A chunk head's offsets: the frame numbers, then the page count and the tag.
const AT_CHUNK_COUNT: usize = CHUNK_PAGES * 8;
const AT_CHUNK_TAG: usize = AT_CHUNK_COUNT + 8;

/// Why an image is not restored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
	/// No image: another magic, or a header invalidated.
	NoImage,
	/// Another format's version.
	Version,
	/// A field out of its bounds.
	Malformed,
	/// Its authentication failed: the header or a chunk was modified.
	Modified,
	/// The key-encryption key did not unseal - TpmService's `policy-refused`: the loader changed.
	KeyNotUnsealed,
	/// Written by another system image.
	SystemChanged,
	/// Describing other hardware.
	HardwareChanged,
	/// Its key in the clear, on a machine whose TPM seals - see the head of this file.
	ClearOnSealingMachine,
}

impl Refusal {
	pub fn text(self) -> &'static str {
		match self {
			Refusal::NoImage => "no image",
			Refusal::Version => "an image of another format version",
			Refusal::Malformed => "a malformed header",
			Refusal::Modified => "its authentication failed - it was modified",
			Refusal::KeyNotUnsealed => "its key did not unseal - the loader changed since it was written",
			Refusal::SystemChanged => "it was written by another system image",
			Refusal::HardwareChanged => "it describes other hardware",
			Refusal::ClearOnSealingMachine => "its key is in the clear, and this machine's TPM seals - anyone who could write the disk could have written it",
		}
	}
}

/// HOW THE KEY-ENCRYPTION KEY IS KEPT in the header: sealed through TpmService, or in the clear where no TPM seals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyProtection {
	Tpm,
	Clear,
}

impl KeyProtection {
	fn code(self) -> u32 {
		match self {
			KeyProtection::Tpm => 1,
			KeyProtection::Clear => 2,
		}
	}

	fn from_code(code: u32) -> Option<KeyProtection> {
		match code {
			1 => Some(KeyProtection::Tpm),
			2 => Some(KeyProtection::Clear),
			_ => None,
		}
	}
}

/// THE IMAGE KEY: the pages' encryption half and the chunks' MAC half.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageKey {
	pub encrypt: [u8; KEY],
	pub mac: [u8; KEY],
}

impl ImageKey {
	pub fn from_bytes(bytes: &[u8; 2 * KEY]) -> ImageKey {
		let mut encrypt = [0u8; KEY];
		let mut mac = [0u8; KEY];
		encrypt.copy_from_slice(&bytes[..KEY]);
		mac.copy_from_slice(&bytes[KEY..]);
		ImageKey { encrypt, mac }
	}

	fn bytes(&self) -> [u8; 2 * KEY] {
		let mut out = [0u8; 2 * KEY];
		out[..KEY].copy_from_slice(&self.encrypt);
		out[KEY..].copy_from_slice(&self.mac);
		out
	}
}

/// THE HEADER, as the writer fills it and the restorer reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
	/// How the key-encryption key is kept, and the key as TpmService sealed it - or, in the clear, the key itself.
	pub protection: KeyProtection,
	pub sealed: Vec<u8>,
	/// The wrap's counter prefix, and the image key wrapped.
	pub nonce: [u8; 8],
	pub wrapped: [u8; 2 * KEY],
	/// The pages the image holds, in whole chunks of `CHUNK_PAGES` and one shorter last one.
	pub pages: u64,
	/// When it was written, in Unix seconds.
	pub created: u64,
	/// The system image's digest and the hardware's, as the kernel described them.
	pub system: [u8; 32],
	pub hardware: [u8; 32],
	/// The kernel's resume context.
	pub context: [u8; CONTEXT],
	/// The authentication of every field above under the key-encryption key's MAC half.
	pub mac: [u8; BLOCK],
}

// COUNTER MODE: each 16-byte block XORed with the cipher of `prefix || counter`, the counter from `first` - so the
// same key never takes the same counter block twice, as long as the prefixes differ.
fn ctr(key: &Key, prefix: [u8; 8], first: u64, data: &mut [u8]) {
	for (at, piece) in data.chunks_mut(BLOCK).enumerate() {
		let mut counter = [0u8; BLOCK];
		counter[..8].copy_from_slice(&prefix);
		counter[8..].copy_from_slice(&(first + at as u64).to_le_bytes());
		let stream = key.block(&counter);
		for (byte, mask) in piece.iter_mut().zip(stream.iter()) {
			*byte ^= *mask;
		}
	}
}

fn halves(kek: &[u8; KEK]) -> (Key, Key) {
	let mut first = [0u8; KEY];
	let mut second = [0u8; KEY];
	first.copy_from_slice(&kek[..KEY]);
	second.copy_from_slice(&kek[KEY..]);
	(Key::new(&first), Key::new(&second))
}

impl Header {
	/// A NEW IMAGE'S HEADER: the image key wrapped under `kek` with `nonce`, and every field authenticated. `sealed` is
	/// TpmService's blob for `KeyProtection::Tpm`; for `KeyProtection::Clear` it is ignored, and `kek` itself is kept.
	#[allow(clippy::too_many_arguments)]
	pub fn new(kek: &[u8; KEK], protection: KeyProtection, sealed: &[u8], nonce: [u8; 8], image: &ImageKey, pages: u64, created: u64, system: [u8; 32], hardware: [u8; 32], context: [u8; CONTEXT]) -> Result<Header, Refusal> {
		let sealed: &[u8] = if protection == KeyProtection::Clear { kek } else { sealed };
		if sealed.is_empty() || sealed.len() > SEALED_MAX {
			return Err(Refusal::Malformed);
		}
		let (encrypt, _) = halves(kek);
		let mut wrapped = image.bytes();
		ctr(&encrypt, nonce, 0, &mut wrapped);
		let mut header = Header { protection, sealed: sealed.to_vec(), nonce, wrapped, pages, created, system, hardware, context, mac: [0u8; BLOCK] };
		header.mac = header.authentication(kek);
		Ok(header)
	}

	// THE HEADER'S TAG: every field of the encoded header but the state and the tag itself, so invalidating an image
	// changes no tag and a flipped field anywhere else fails it.
	fn authentication(&self, kek: &[u8; KEK]) -> [u8; BLOCK] {
		let (_, mac) = halves(kek);
		let encoded = self.encode(STATE_IMAGE);
		let mut stream = Stream::new(&mac);
		stream.update(&encoded[AT_MAGIC..AT_STATE]);
		stream.update(&encoded[AT_SEALED_LEN..AT_MAC]);
		stream.finish()
	}

	/// The header block, with `state`.
	pub fn encode(&self, state: u32) -> [u8; HEADER_BYTES] {
		let mut out = [0u8; HEADER_BYTES];
		out[AT_MAGIC..AT_MAGIC + 8].copy_from_slice(&MAGIC);
		out[AT_VERSION..AT_VERSION + 4].copy_from_slice(&VERSION.to_le_bytes());
		out[AT_STATE..AT_STATE + 4].copy_from_slice(&state.to_le_bytes());
		out[AT_SEALED_LEN..AT_SEALED_LEN + 4].copy_from_slice(&(self.sealed.len() as u32).to_le_bytes());
		out[AT_PROTECTION..AT_PROTECTION + 4].copy_from_slice(&self.protection.code().to_le_bytes());
		out[AT_SEALED..AT_SEALED + self.sealed.len()].copy_from_slice(&self.sealed);
		out[AT_NONCE..AT_NONCE + 8].copy_from_slice(&self.nonce);
		out[AT_WRAPPED..AT_WRAPPED + 2 * KEY].copy_from_slice(&self.wrapped);
		out[AT_PAGES..AT_PAGES + 8].copy_from_slice(&self.pages.to_le_bytes());
		out[AT_CREATED..AT_CREATED + 8].copy_from_slice(&self.created.to_le_bytes());
		out[AT_SYSTEM..AT_SYSTEM + 32].copy_from_slice(&self.system);
		out[AT_HARDWARE..AT_HARDWARE + 32].copy_from_slice(&self.hardware);
		out[AT_CONTEXT..AT_CONTEXT + CONTEXT].copy_from_slice(&self.context);
		out[AT_MAC..AT_MAC + BLOCK].copy_from_slice(&self.mac);
		out
	}

	/// A HEADER READ BACK: an image's, or why it is none. Nothing here is authenticated yet - `open` does that once
	/// the key-encryption key is unsealed.
	pub fn decode(block: &[u8]) -> Result<Header, Refusal> {
		if block.len() < HEADER_USED || block[AT_MAGIC..AT_MAGIC + 8] != MAGIC {
			return Err(Refusal::NoImage);
		}
		let u32_at = |at: usize| u32::from_le_bytes([block[at], block[at + 1], block[at + 2], block[at + 3]]);
		let u64_at = |at: usize| {
			let mut bytes = [0u8; 8];
			bytes.copy_from_slice(&block[at..at + 8]);
			u64::from_le_bytes(bytes)
		};
		if u32_at(AT_VERSION) != VERSION {
			return Err(Refusal::Version);
		}
		if u32_at(AT_STATE) != STATE_IMAGE {
			return Err(Refusal::NoImage);
		}
		let sealed_len = u32_at(AT_SEALED_LEN) as usize;
		if sealed_len == 0 || sealed_len > SEALED_MAX {
			return Err(Refusal::Malformed);
		}
		let protection = KeyProtection::from_code(u32_at(AT_PROTECTION)).ok_or(Refusal::Malformed)?;
		if protection == KeyProtection::Clear && sealed_len != KEK {
			return Err(Refusal::Malformed);
		}
		let mut header = Header { protection, sealed: block[AT_SEALED..AT_SEALED + sealed_len].to_vec(), nonce: [0u8; 8], wrapped: [0u8; 2 * KEY], pages: u64_at(AT_PAGES), created: u64_at(AT_CREATED), system: [0u8; 32], hardware: [0u8; 32], context: [0u8; CONTEXT], mac: [0u8; BLOCK] };
		header.nonce.copy_from_slice(&block[AT_NONCE..AT_NONCE + 8]);
		header.wrapped.copy_from_slice(&block[AT_WRAPPED..AT_WRAPPED + 2 * KEY]);
		header.system.copy_from_slice(&block[AT_SYSTEM..AT_SYSTEM + 32]);
		header.hardware.copy_from_slice(&block[AT_HARDWARE..AT_HARDWARE + 32]);
		header.context.copy_from_slice(&block[AT_CONTEXT..AT_CONTEXT + CONTEXT]);
		header.mac.copy_from_slice(&block[AT_MAC..AT_MAC + BLOCK]);
		if header.pages == 0 {
			return Err(Refusal::Malformed);
		}
		Ok(header)
	}

	/// THE KEY-ENCRYPTION KEY OF AN IMAGE KEPT IN THE CLEAR - `None` for a sealed one, whose key only TpmService opens.
	pub fn clear_key(&self) -> Option<[u8; KEK]> {
		if self.protection != KeyProtection::Clear {
			return None;
		}
		self.sealed[..].try_into().ok()
	}

	/// THE IMAGE KEY, once the header's authentication under `kek` holds - `Modified` when it does not.
	pub fn open(&self, kek: &[u8; KEK]) -> Result<ImageKey, Refusal> {
		if !equal(&self.authentication(kek), &self.mac) {
			return Err(Refusal::Modified);
		}
		let (encrypt, _) = halves(kek);
		let mut bytes = self.wrapped;
		ctr(&encrypt, self.nonce, 0, &mut bytes);
		Ok(ImageKey::from_bytes(&bytes))
	}

	/// WHETHER THIS IMAGE MAY BE RESTORED HERE: written by this system image, for this hardware.
	pub fn fits(&self, system: &[u8; 32], hardware: &[u8; 32]) -> Result<(), Refusal> {
		if &self.system != system {
			return Err(Refusal::SystemChanged);
		}
		if &self.hardware != hardware {
			return Err(Refusal::HardwareChanged);
		}
		Ok(())
	}

	/// How many chunks the image holds.
	pub fn chunks(&self) -> u64 {
		self.pages.div_ceil(CHUNK_PAGES as u64)
	}
}

/// THE BYTES AN IMAGE OF `pages` PAGES TAKES ON THE PARTITION, header included.
pub fn image_bytes(pages: u64) -> u64 {
	HEADER_BYTES as u64 + pages.div_ceil(CHUNK_PAGES as u64) * CHUNK_HEAD as u64 + pages * PAGE as u64
}

/// Where chunk `index` begins on the partition, in bytes - every chunk before it full.
pub fn chunk_offset(index: u64) -> u64 {
	HEADER_BYTES as u64 + index * (CHUNK_HEAD + CHUNK_PAGES * PAGE) as u64
}

/// How many pages chunk `index` of an image of `pages` pages holds.
pub fn chunk_pages(pages: u64, index: u64) -> usize {
	let before = index * CHUNK_PAGES as u64;
	pages.saturating_sub(before).min(CHUNK_PAGES as u64) as usize
}

// A chunk's tag: its index, its frame numbers and its ciphertext.
fn chunk_tag(image: &ImageKey, index: u64, frames: &[u64], data: &[u8]) -> [u8; BLOCK] {
	let mut stream = Stream::new(&Key::new(&image.mac));
	stream.update(&index.to_le_bytes());
	stream.update(&(frames.len() as u64).to_le_bytes());
	for frame in frames {
		stream.update(&frame.to_le_bytes());
	}
	stream.update(data);
	stream.finish()
}

// THE COUNTER'S PREFIX FOR A CHUNK: its index, with the top bit set so no chunk's prefix is ever the wrap's.
fn chunk_prefix(index: u64) -> [u8; 8] {
	(index | 1 << 63).to_le_bytes()
}

/// ONE CHUNK SEALED: `data` - its pages, in the order of `frames` - encrypted in place, and its head block answered.
pub fn seal_chunk(image: &ImageKey, index: u64, frames: &[u64], data: &mut [u8]) -> Result<[u8; CHUNK_HEAD], Refusal> {
	if frames.is_empty() || frames.len() > CHUNK_PAGES || data.len() != frames.len() * PAGE {
		return Err(Refusal::Malformed);
	}
	ctr(&Key::new(&image.encrypt), chunk_prefix(index), 0, data);
	let tag = chunk_tag(image, index, frames, data);
	let mut head = [0u8; CHUNK_HEAD];
	for (at, frame) in frames.iter().enumerate() {
		head[at * 8..at * 8 + 8].copy_from_slice(&frame.to_le_bytes());
	}
	head[AT_CHUNK_COUNT..AT_CHUNK_COUNT + 8].copy_from_slice(&(frames.len() as u64).to_le_bytes());
	head[AT_CHUNK_TAG..AT_CHUNK_TAG + BLOCK].copy_from_slice(&tag);
	Ok(head)
}

/// ONE CHUNK OPENED: its head read, its tag checked over its frames and `data` before anything is decrypted, then
/// `data` decrypted in place. The frames, in the pages' order; `Modified` when the tag fails, `Malformed` for a head
/// whose count is not the one the header's page count gives this chunk.
pub fn open_chunk(image: &ImageKey, index: u64, expected: usize, head: &[u8], data: &mut [u8]) -> Result<Vec<u64>, Refusal> {
	if head.len() < CHUNK_HEAD || expected == 0 || expected > CHUNK_PAGES || data.len() != expected * PAGE {
		return Err(Refusal::Malformed);
	}
	let mut count = [0u8; 8];
	count.copy_from_slice(&head[AT_CHUNK_COUNT..AT_CHUNK_COUNT + 8]);
	if u64::from_le_bytes(count) != expected as u64 {
		return Err(Refusal::Malformed);
	}
	let frames: Vec<u64> = (0..expected)
		.map(|at| {
			let mut bytes = [0u8; 8];
			bytes.copy_from_slice(&head[at * 8..at * 8 + 8]);
			u64::from_le_bytes(bytes)
		})
		.collect();
	let mut tag = [0u8; BLOCK];
	tag.copy_from_slice(&head[AT_CHUNK_TAG..AT_CHUNK_TAG + BLOCK]);
	if !equal(&chunk_tag(image, index, &frames, data), &tag) {
		return Err(Refusal::Modified);
	}
	ctr(&Key::new(&image.encrypt), chunk_prefix(index), 0, data);
	Ok(frames)
}

// A comparison that does not stop at the first difference.
fn equal(a: &[u8; BLOCK], b: &[u8; BLOCK]) -> bool {
	a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// THE HARDWARE'S DIGEST, from the kernel's description of it - the RAM, the PCI functions and the cores.
pub fn hardware_digest(description: &[u8]) -> [u8; 32] {
	crate::sha256::digest(description)
}

#[cfg(test)]
mod tests;
