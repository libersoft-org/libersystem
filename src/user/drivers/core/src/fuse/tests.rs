use super::*;
use std::vec::Vec;

fn answer(unique: u64, error: i32, body: &[u8]) -> Vec<u8> {
	let mut out = Vec::new();
	out.extend_from_slice(&((OUT_HEADER + body.len()) as u32).to_le_bytes());
	out.extend_from_slice(&error.to_le_bytes());
	out.extend_from_slice(&unique.to_le_bytes());
	out.extend_from_slice(body);
	out
}

fn attr_bytes(ino: u64, size: u64, mode: u32, mtime: u64) -> Vec<u8> {
	let mut out = alloc::vec![0u8; ATTR];
	out[0..8].copy_from_slice(&ino.to_le_bytes());
	out[8..16].copy_from_slice(&size.to_le_bytes());
	out[32..40].copy_from_slice(&mtime.to_le_bytes());
	out[60..64].copy_from_slice(&mode.to_le_bytes());
	out[64..68].copy_from_slice(&3u32.to_le_bytes());
	out[68..72].copy_from_slice(&1000u32.to_le_bytes());
	out[72..76].copy_from_slice(&1000u32.to_le_bytes());
	out
}

fn dirent(next: u64, name: &[u8], kind: u32) -> Vec<u8> {
	let mut out = Vec::new();
	out.extend_from_slice(&77u64.to_le_bytes());
	out.extend_from_slice(&next.to_le_bytes());
	out.extend_from_slice(&(name.len() as u32).to_le_bytes());
	out.extend_from_slice(&kind.to_le_bytes());
	out.extend_from_slice(name);
	while out.len() % 8 != 0 {
		out.push(0);
	}
	out
}

#[test]
// THE OFFSETS ARE THE PROTOCOL'S: a header four bytes out is a request the server reads as another one.
fn a_request_is_the_header_then_its_body() {
	let header = in_header(56, LOOKUP, 9, ROOT);
	assert_eq!(u32::from_le_bytes(header[0..4].try_into().unwrap()), 56);
	assert_eq!(u32::from_le_bytes(header[4..8].try_into().unwrap()), LOOKUP);
	assert_eq!(u64::from_le_bytes(header[8..16].try_into().unwrap()), 9);
	assert_eq!(u64::from_le_bytes(header[16..24].try_into().unwrap()), ROOT);
	assert!(header[24..].iter().all(|byte| *byte == 0), "the caller is root, and nothing else is said");
	assert_eq!(&init_in()[..8], &[7, 0, 0, 0, 31, 0, 0, 0], "protocol 7.31");
	let read = read_in(0xAB, 4096, 512, O_RDONLY);
	assert_eq!((u64::from_le_bytes(read[0..8].try_into().unwrap()), u64::from_le_bytes(read[8..16].try_into().unwrap()), u32::from_le_bytes(read[16..20].try_into().unwrap())), (0xAB, 4096, 512));
	let mut body = [0u8; 300];
	assert_eq!(lookup_in(b"notes.txt", &mut body), Ok(10));
	assert_eq!(&body[..10], b"notes.txt\0", "a name and its NUL");
}

#[test]
fn an_answer_is_believed_only_for_its_own_request_and_within_its_length() {
	let ok = answer(5, 0, &[1, 2, 3]);
	assert_eq!(out_header(&ok, 5), Ok((0, &[1u8, 2, 3][..])));
	assert_eq!(out_header(&ok, 6), Err(Fault::Unique), "an answer to another request");
	let enoent = answer(5, -2, &[]);
	assert_eq!(out_header(&enoent, 5), Ok((2, &[][..])), "ENOENT, as the host said it");
	assert_eq!(out_header(&answer(5, 4, &[]), 5), Err(Fault::Body), "a positive error is no FUSE answer");
	let mut lying = ok.clone();
	lying[0..4].copy_from_slice(&64u32.to_le_bytes());
	assert_eq!(out_header(&lying, 5), Err(Fault::Short), "a length past what arrived");
	assert_eq!(out_header(&ok[..10], 5), Err(Fault::Short));
	let mut init = alloc::vec![0u8; 64];
	init[0..4].copy_from_slice(&7u32.to_le_bytes());
	init[4..8].copy_from_slice(&38u32.to_le_bytes());
	init[20..24].copy_from_slice(&(128 * 1024u32).to_le_bytes());
	assert_eq!(init_out(&init), Ok((38, 128 * 1024)));
	init[0..4].copy_from_slice(&8u32.to_le_bytes());
	assert_eq!(init_out(&init), Err(Fault::Version));
}

#[test]
// THE HOST'S IDENTITIES STAY THE HOST'S: inode, link count, owner and group are decoded past and never kept.
fn an_attribute_crosses_as_its_kind_size_permissions_and_time() {
	let attr = attr(&attr_bytes(0xdead_beef, 1234, 0o100644, 1_700_000_000)).expect("whole");
	assert_eq!(attr, Attr { kind: Kind::File, size: 1234, permissions: 0o644, mtime: 1_700_000_000 });
	assert_eq!(kind_of(0o040755), Kind::Directory);
	assert_eq!(kind_of(0o120777), Kind::Other, "a symbolic link is never followed");
	assert_eq!(kind_of(0o020666), Kind::Other, "nor a device opened");
	let mut entry = alloc::vec![0u8; 40];
	entry[0..8].copy_from_slice(&42u64.to_le_bytes());
	entry.extend_from_slice(&attr_bytes(9, 0, 0o040700, 5));
	assert_eq!(entry_out(&entry), Ok((42, Attr { kind: Kind::Directory, size: 0, permissions: 0o700, mtime: 5 })));
	let mut zero = entry.clone();
	zero[0..8].fill(0);
	assert_eq!(entry_out(&zero), Err(Fault::Body), "node id zero is no node");
	assert_eq!(entry_out(&entry[..100]), Err(Fault::Body), "a short entry");
}

#[test]
// A NAME IS HOW A SHARE REACHES OUTSIDE ITSELF: `..`, a separator or a NUL would name something the share does not
// hold, and each is refused before the server is asked.
fn a_name_that_would_walk_out_of_the_share_is_refused() {
	assert_eq!(check_name(b"report-2026.txt"), Ok(()));
	assert_eq!(check_name("příliš žluťoučký".as_bytes()), Ok(()), "UTF-8 is a name");
	assert_eq!(check_name(b""), Err(NameRefusal::Empty));
	assert_eq!(check_name(b".."), Err(NameRefusal::Dots));
	assert_eq!(check_name(b"."), Err(NameRefusal::Dots));
	assert_eq!(check_name(b"../etc"), Err(NameRefusal::Separator));
	assert_eq!(check_name(b"a/b"), Err(NameRefusal::Separator));
	assert_eq!(check_name(b"a\0b"), Err(NameRefusal::Separator));
	assert_eq!(check_name(&[0xff, 0xfe]), Err(NameRefusal::Encoding));
	assert_eq!(check_name(&[b'x'; 255]), Ok(()));
	assert_eq!(check_name(&[b'x'; 256]), Err(NameRefusal::TooLong));
	assert_eq!(check_name(b"..."), Ok(()), "three dots are a name like any other");
	let mut small = [0u8; 4];
	assert_eq!(lookup_in(b"abcd", &mut small), Err(NameRefusal::TooLong), "no room for the NUL");
}

#[test]
fn a_listing_s_records_are_held_to_the_buffer_and_its_dots_dropped() {
	let mut body = dirent(1, b".", 4);
	body.extend(dirent(2, b"..", 4));
	body.extend(dirent(3, b"docs", 4));
	body.extend(dirent(4, b"hello.txt", 8));
	body.extend(dirent(5, b"link", 10));
	let mut seen = Vec::new();
	let mut offsets = Vec::new();
	assert_eq!(
		dirents(&body, |entry, next| {
			offsets.push(next);
			if let Some(entry) = entry {
				seen.push((entry.name.to_vec(), entry.kind, entry.next));
			}
		}),
		Ok(0)
	);
	assert_eq!(seen, alloc::vec![(b"docs".to_vec(), Kind::Directory, 3), (b"hello.txt".to_vec(), Kind::File, 4), (b"link".to_vec(), Kind::Other, 5)]);
	assert_eq!(offsets, alloc::vec![1, 2, 3, 4, 5], "every record's offset, the dropped ones among them, so a continued listing neither repeats nor skips");
	let cut = &body[..body.len() - 9];
	assert_eq!(dirents(cut, |_, _| {}), Err(Fault::Record), "a record whose name runs past what arrived");
	let mut evil = dirent(1, b"../../etc", 8);
	evil.extend(dirent(2, b"a/b", 8));
	evil.extend(dirent(3, &[0xff, 0xfe], 8));
	evil.extend(dirent(4, b"ok", 8));
	let mut kept = Vec::new();
	assert_eq!(dirents(&evil, |entry, _| kept.extend(entry.map(|entry| entry.name.to_vec()))), Ok(3), "three listed names that break a rule are dropped and counted");
	assert_eq!(kept, alloc::vec![b"ok".to_vec()], "and never handed on");
	let mut long = alloc::vec![0u8; DIRENT];
	long[16..20].copy_from_slice(&300u32.to_le_bytes());
	long.extend(alloc::vec![b'a'; 300]);
	assert_eq!(dirents(&long, |_, _| {}), Err(Fault::Record), "a name longer than any name");
	let last = dirent(9, b"a", 8);
	assert_eq!(dirents(&last[..DIRENT + 1], |_, _| {}), Ok(0), "the last record's padding may be cut");
}

#[test]
// A HANDLE IS THIS SYSTEM'S NAME FOR A NODE: the server's id stays behind it, a second lookup of the same node is the
// same handle with one more lookup to give back, and a forgotten handle is not handed out again.
fn nodes_are_known_by_handles_that_hide_the_server_s_ids() {
	let mut nodes = Nodes::new();
	assert_eq!(nodes.get(ROOT_HANDLE), Some((ROOT, Kind::Directory)), "the root is handle one, a directory");
	assert_eq!(nodes.forget(ROOT_HANDLE), None, "and is never forgotten");
	let docs = nodes.looked_up(0x7f00_1234, Kind::Directory).expect("room");
	assert_ne!(docs, 0x7f00_1234 as u32, "the handle is not the server's id");
	assert_eq!(nodes.looked_up(0x7f00_1234, Kind::Directory), Some(docs), "the same node, the same handle");
	assert_eq!(nodes.forget(docs), Some((0x7f00_1234, 2)), "both lookups are given back");
	assert_eq!(nodes.get(docs), None);
	let again = nodes.looked_up(0x7f00_1234, Kind::File).expect("room");
	assert_ne!(again, docs, "a forgotten handle is not reused");
	assert_eq!(nodes.get(again), Some((0x7f00_1234, Kind::File)), "and the kind is the node's as answered");
	for at in 0..(MAX_NODES as u64 - 2) {
		nodes.looked_up(1000 + at, Kind::File).expect("room up to the bound");
	}
	assert_eq!(nodes.len(), MAX_NODES);
	assert_eq!(nodes.looked_up(99_999, Kind::File), None, "one past the bound");
	assert_eq!(nodes.get(0), None, "handle zero is no node");
	let given: Vec<(u64, u64)> = nodes.drain().collect();
	assert_eq!(given.len(), MAX_NODES - 1, "a connection that has gone gives back every node but the root");
	assert_eq!(nodes.len(), 1);
	assert_eq!(nodes.get(ROOT_HANDLE), Some((ROOT, Kind::Directory)));
}

#[test]
fn a_host_errno_becomes_a_meaning_and_never_the_number() {
	use proto::system::Error;
	assert_eq!(errno_error(2), Error::NotFound);
	assert_eq!(errno_error(13), Error::Denied);
	assert_eq!(errno_error(30), Error::Denied, "a share the host exports read-only");
	assert_eq!(errno_error(20), Error::Invalid, "a path through something that is not a directory");
	assert_eq!(errno_error(38), Error::Unsupported);
	assert_eq!(errno_error(5), Error::Io);
	assert_eq!(errno_error(9999), Error::Io, "a number this driver does not know is an I/O failure, not a guess");
}
