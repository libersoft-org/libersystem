use super::*;
use alloc::vec;

const KEK_A: [u8; KEK] = [7u8; KEK];
const KEK_B: [u8; KEK] = [9u8; KEK];

fn image_key() -> ImageKey {
	ImageKey { encrypt: [0x11; KEY], mac: [0x22; KEY] }
}

fn header() -> Header {
	Header::new(&KEK_A, &[0xAB; 300], [1, 2, 3, 4, 5, 6, 7, 8], &image_key(), 1000, 1_790_000_000, [3; 32], [4; 32], [5; CONTEXT]).expect("a header")
}

// A HEADER WRITTEN AND READ BACK gives the image key back under the same key-encryption key, and fits the system and
// hardware it names.
#[test]
fn a_header_read_back_opens_under_its_key_and_fits_where_it_was_written() {
	let written = header();
	let read = Header::decode(&written.encode(STATE_IMAGE)).expect("an image");
	assert_eq!(read, written);
	assert_eq!(read.open(&KEK_A), Ok(image_key()));
	assert_eq!(read.fits(&[3; 32], &[4; 32]), Ok(()));
	assert_eq!(read.chunks(), 4, "1000 pages in chunks of 256");
	assert_ne!(&written.wrapped[..], &image_key().bytes()[..], "the image key is never in the clear");
}

// EVERY BYTE OF THE HEADER THAT MEANS SOMETHING IS AUTHENTICATED: one flipped anywhere but the state fails it, and so does
// another key-encryption key.
#[test]
fn a_header_modified_anywhere_or_opened_under_another_key_is_refused() {
	let block = header().encode(STATE_IMAGE);
	assert_eq!(Header::decode(&block).expect("an image").open(&KEK_B), Err(Refusal::Modified));
	for at in [AT_SEALED + 17, AT_NONCE + 3, AT_WRAPPED + 1, AT_PAGES, AT_CREATED + 2, AT_SYSTEM + 31, AT_HARDWARE, AT_CONTEXT + 9, AT_MAC + 15] {
		let mut changed = block;
		changed[at] ^= 0x40;
		let decoded = Header::decode(&changed).expect("still shaped as an image");
		assert_eq!(decoded.open(&KEK_A), Err(Refusal::Modified), "a byte at {at}");
	}
}

// AN INVALIDATED HEADER IS NO IMAGE; another magic, another version and a sealed blob out of bounds are refused as such.
#[test]
fn an_invalidated_or_foreign_header_is_no_image() {
	let written = header();
	assert_eq!(Header::decode(&written.encode(STATE_EMPTY)), Err(Refusal::NoImage));
	assert_eq!(Header::decode(&[0u8; HEADER_BYTES]), Err(Refusal::NoImage));
	let mut version = written.encode(STATE_IMAGE);
	version[AT_VERSION] = 2;
	assert_eq!(Header::decode(&version), Err(Refusal::Version));
	let mut sealed = written.encode(STATE_IMAGE);
	sealed[AT_SEALED_LEN..AT_SEALED_LEN + 4].copy_from_slice(&(SEALED_MAX as u32 + 1).to_le_bytes());
	assert_eq!(Header::decode(&sealed), Err(Refusal::Malformed));
	assert!(Header::new(&KEK_A, &[0; SEALED_MAX + 1], [0; 8], &image_key(), 1, 0, [0; 32], [0; 32], [0; CONTEXT]).is_err());
}

// ANOTHER SYSTEM IMAGE, OTHER HARDWARE: each refused by name.
#[test]
fn an_image_from_another_system_image_or_other_hardware_does_not_fit() {
	let written = header();
	assert_eq!(written.fits(&[0; 32], &[4; 32]), Err(Refusal::SystemChanged));
	assert_eq!(written.fits(&[3; 32], &[0; 32]), Err(Refusal::HardwareChanged));
}

fn pages(count: usize, seed: u8) -> Vec<u8> {
	(0..count * PAGE).map(|at| (at as u8).wrapping_mul(31).wrapping_add(seed)).collect()
}

// A CHUNK SEALED AND OPENED gives its pages and frames back; the ciphertext is not the pages, and the same pages in
// another chunk encrypt differently.
#[test]
fn a_chunk_sealed_and_opened_gives_its_pages_back() {
	let frames = vec![0x100, 0x2a7, 0x3];
	let clear = pages(3, 5);
	let mut data = clear.clone();
	let head = seal_chunk(&image_key(), 2, &frames, &mut data).expect("sealed");
	assert_ne!(data, clear);
	let mut other = clear.clone();
	seal_chunk(&image_key(), 3, &frames, &mut other).expect("sealed");
	assert_ne!(other, data, "another chunk's counter");
	assert_eq!(open_chunk(&image_key(), 2, 3, &head, &mut data), Ok(frames));
	assert_eq!(data, clear);
}

// A CHUNK ALTERED, MOVED OR CUT is refused before anything is decrypted.
#[test]
fn a_chunk_altered_moved_or_cut_is_refused() {
	let frames = vec![0x10, 0x11];
	let mut data = pages(2, 9);
	let head = seal_chunk(&image_key(), 0, &frames, &mut data).expect("sealed");
	let mut flipped = data.clone();
	flipped[PAGE + 100] ^= 1;
	assert_eq!(open_chunk(&image_key(), 0, 2, &head, &mut flipped), Err(Refusal::Modified), "a ciphertext byte");
	let mut moved_frame = head;
	moved_frame[8] ^= 1;
	assert_eq!(open_chunk(&image_key(), 0, 2, &moved_frame, &mut data.clone()), Err(Refusal::Modified), "a frame number");
	assert_eq!(open_chunk(&image_key(), 1, 2, &head, &mut data.clone()), Err(Refusal::Modified), "the chunk put at another index");
	assert_eq!(open_chunk(&ImageKey { encrypt: [0x11; KEY], mac: [0x23; KEY] }, 0, 2, &head, &mut data.clone()), Err(Refusal::Modified), "another key");
	assert_eq!(open_chunk(&image_key(), 0, 1, &head, &mut data[..PAGE].to_vec()), Err(Refusal::Malformed), "a count the header does not give");
	assert_eq!(open_chunk(&image_key(), 0, 2, &head, &mut data), Ok(frames), "and the untouched chunk still opens");
}

// THE LAYOUT: every chunk but the last full, each after a head block.
#[test]
fn the_partition_holds_a_header_then_full_chunks_and_a_shorter_last_one() {
	assert_eq!(chunk_pages(600, 0), 256);
	assert_eq!(chunk_pages(600, 2), 88);
	assert_eq!(chunk_pages(600, 3), 0);
	assert_eq!(chunk_offset(0), HEADER_BYTES as u64);
	assert_eq!(chunk_offset(1), (HEADER_BYTES + CHUNK_HEAD + 256 * PAGE) as u64);
	assert_eq!(image_bytes(600), (HEADER_BYTES + 3 * CHUNK_HEAD + 600 * PAGE) as u64);
	assert!(seal_chunk(&image_key(), 0, &[], &mut []).is_err(), "no page is no chunk");
	assert!(seal_chunk(&image_key(), 0, &[1; CHUNK_PAGES + 1], &mut vec![0; (CHUNK_PAGES + 1) * PAGE]).is_err(), "nor is one past the bound");
}
