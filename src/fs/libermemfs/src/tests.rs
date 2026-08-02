use super::*;

fn capped(capacity: usize) -> LiberMemFs {
	LiberMemFs::mount(Policy::Capped, capacity).expect("a capped volume always mounts")
}

#[test]
fn files_and_directories_round_trip() {
	let mut fs = capped(1024);
	fs.mkdir(b"dir").expect("mkdir");
	fs.write_file(b"dir/a", b"hello").expect("write");
	assert_eq!(fs.read_file(b"dir/a").expect("read"), b"hello".to_vec());

	let entries = fs.list_entries(b"dir").expect("list");
	assert_eq!(entries.len(), 1);
	assert_eq!(entries[0].name, "a");
	assert_eq!(entries[0].size, 5);
	assert!(!entries[0].is_dir);

	// The root lists the directory, and a directory's reported size is what it contains.
	let root = fs.list_entries(b"").expect("list root");
	assert_eq!(root.len(), 1);
	assert!(root[0].is_dir);
	assert_eq!(root[0].size, 5);
}

#[test]
fn rewriting_a_file_replaces_it_rather_than_appending() {
	// The case a fresh implementation gets wrong: a second write must not leave the first
	// write's bytes behind, and must not need room for both copies at once.
	// Capacity 9 holds 8 bytes plus the one-byte name: a name counts against the capacity, so a
	// volume sized to the data alone has no room for what identifies it.
	let mut fs = capped(9);
	fs.write_file(b"f", b"aaaaaaaa").expect("fills the volume exactly");
	assert_eq!(fs.used(), 8);
	assert_eq!(fs.footprint(), 9, "the name is part of what the capacity bounds");
	fs.write_file(b"f", b"bb").expect("rewriting at capacity succeeds");
	assert_eq!(fs.read_file(b"f").expect("read"), b"bb".to_vec());
	assert_eq!(fs.used(), 2);
}

#[test]
fn a_path_cannot_walk_out_of_the_volume() {
	// `..` is refused rather than resolved. Resolving it would be the one way to name
	// something outside the volume, and this filesystem has no outside.
	let mut fs = capped(1024);
	assert_eq!(fs.write_file(b"../escape", b"x"), Err(FsError::BadName));
	assert_eq!(fs.read_file(b"a/../b"), Err(FsError::BadName));
	assert_eq!(fs.read_file(b"."), Err(FsError::BadName));

	// Separator noise names the same thing rather than a different one.
	fs.mkdir(b"d").expect("mkdir");
	fs.write_file(b"/d//x/", b"v").expect("redundant separators are tolerated");
	assert_eq!(fs.read_file(b"d/x").expect("read"), b"v".to_vec());
}

#[test]
fn directories_and_files_do_not_impersonate_each_other() {
	let mut fs = capped(1024);
	fs.mkdir(b"d").expect("mkdir");
	fs.write_file(b"f", b"x").expect("write");

	assert_eq!(fs.read_file(b"d"), Err(FsError::IsDir), "a directory is not readable as a file");
	assert_eq!(fs.write_file(b"d", b"x"), Err(FsError::IsDir), "a directory is not overwritable as a file");
	assert_eq!(fs.list_entries(b"f").err(), Some(FsError::NotDir), "a file is not listable as a directory");
	assert_eq!(fs.remove(b"d"), Err(FsError::IsDir), "remove refuses a directory");
	assert_eq!(fs.rmdir(b"f"), Err(FsError::NotDir), "rmdir refuses a file");
	assert_eq!(fs.write_file(b"f/under", b"x"), Err(FsError::NotDir), "a file cannot be a path component");
}

#[test]
fn a_directory_with_entries_is_not_removed_silently() {
	let mut fs = capped(1024);
	fs.mkdir(b"d").expect("mkdir");
	fs.write_file(b"d/x", b"v").expect("write");
	assert_eq!(fs.rmdir(b"d"), Err(FsError::NotEmpty), "removing a tree by naming its root is a different operation");
	fs.remove(b"d/x").expect("remove the entry");
	fs.rmdir(b"d").expect("now empty");
	assert_eq!(fs.list_entries(b"").expect("list root").len(), 0);
}

#[test]
fn a_capped_volume_refuses_the_write_that_crosses_its_limit() {
	// The capped policy's whole behaviour: it mounts regardless, and the refusal lands on the
	// write rather than at mount.
	let mut fs = capped(12);
	fs.write_file(b"a", b"12345").expect("fits");
	assert_eq!(fs.free(), 6, "five bytes of data and a one-byte name");
	assert_eq!(fs.write_file(b"b", b"123456"), Err(FsError::NoSpace), "one byte over the cap is refused");
	fs.write_file(b"b", b"12345").expect("exactly the remainder fits");
	assert_eq!(fs.free(), 0);

	// A refused write leaves nothing behind.
	assert_eq!(fs.write_file(b"c", b"x"), Err(FsError::NoSpace));
	assert_eq!(fs.list_entries(b"").expect("list").len(), 2);
}

#[test]
fn a_reserved_volume_holds_its_capacity_from_the_moment_it_mounts() {
	// The reserved policy's whole behaviour: the memory is taken at mount, so a later write
	// cannot fail because something else took it. Both policies enforce the same capacity;
	// only the moment of charging differs.
	let mut fs = LiberMemFs::mount(Policy::Reserved, 16).expect("mount");
	assert_eq!(fs.policy(), Policy::Reserved);
	assert_eq!(fs.capacity(), 16);
	assert_eq!(fs.used(), 0);
	fs.write_file(b"a", b"123456789012345").expect("the whole capacity is writable");
	assert_eq!(fs.free(), 0);
	assert_eq!(fs.write_file(b"b", b"x"), Err(FsError::NoSpace));
}

#[test]
fn bounds_are_refusals_rather_than_truncation() {
	let mut fs = capped(MAX_FILE_BYTES + 1024);
	let long_name = alloc::vec![b'n'; MAX_NAME_BYTES + 1];
	assert_eq!(fs.write_file(&long_name, b"x"), Err(FsError::TooLong));

	let deep: Vec<u8> = (0..MAX_PATH_DEPTH + 1).map(|_| "d/").collect::<String>().into_bytes();
	assert_eq!(fs.read_file(&deep), Err(FsError::TooLong));

	// A name that is not UTF-8 is refused rather than stored under a mangled key.
	assert_eq!(fs.write_file(&[0xff, 0xfe], b"x"), Err(FsError::BadName));
}

#[test]
fn an_empty_volume_reports_nothing_and_a_missing_path_is_not_found() {
	let mut fs = capped(64);
	assert_eq!(fs.list_entries(b"").expect("root lists").len(), 0);
	assert_eq!(fs.read_file(b"absent"), Err(FsError::NotFound));
	assert_eq!(fs.remove(b"absent"), Err(FsError::NotFound));
	assert_eq!(fs.rmdir(b"absent"), Err(FsError::NotFound));
	assert_eq!(fs.list_entries(b"absent").err(), Some(FsError::NotFound));
	assert_eq!(fs.write_file(b"missing/dir/f", b"x"), Err(FsError::NotFound), "a write does not create parent directories");
}

#[test]
fn a_reserved_volume_keeps_its_guarantee_when_files_shrink_or_go() {
	// The first version only ever shrank the reservation, so freeing bytes handed them to the
	// heap instead of back to the reservation: the volume kept reporting its capacity while no
	// longer holding it, and a later write could fail for memory something else had taken.
	// The footprint is what matters, and the only handle a test has on it is the reservation
	// tracking the unused part exactly.
	let mut fs = LiberMemFs::mount(Policy::Reserved, 64).expect("mount");
	assert_eq!(fs.reserved_bytes(), 64, "an empty reserved volume holds all of it");

	fs.write_file(b"a", &[b'x'; 40]).expect("write");
	assert_eq!(fs.reserved_bytes(), 23, "what a file and its name use is no longer held separately");

	// Shrinking must return the difference to the reservation, not to the heap.
	fs.write_file(b"a", &[b'x'; 8]).expect("shrink");
	assert_eq!(fs.reserved_bytes(), 55, "shrinking gives the memory back to the volume");

	// So must deleting.
	fs.remove(b"a").expect("remove");
	assert_eq!(fs.reserved_bytes(), 64, "an emptied reserved volume holds all of it again");
	assert_eq!(fs.used(), 0);

	// And the guarantee still holds afterwards: the whole capacity is writable again.
	fs.write_file(b"b", &[b'y'; 63]).expect("the full capacity is available after the round trip");
}

#[test]
fn a_failed_write_leaves_the_previous_contents_intact() {
	// The entry used to be replaced by an empty file BEFORE the bytes were allocated, so a
	// write that could not be satisfied destroyed what was already there. A refused write must
	// change nothing.
	let mut fs = capped(16);
	fs.write_file(b"f", b"original").expect("write");

	assert_eq!(fs.write_file(b"f", &[b'x'; 32]), Err(FsError::NoSpace), "a write past the cap is refused");
	assert_eq!(fs.read_file(b"f").expect("still there"), b"original".to_vec(), "the refused write left the file untouched");
	assert_eq!(fs.used(), 8);

	// What this test can reach is the CAPACITY refusal, which returns before the entry is
	// touched. The bug it stands for was on the other path - an allocation failure after the
	// entry had already been replaced - and that one cannot be provoked here without an
	// allocator that can be made to fail. The fix was to build the bytes first and insert once
	// they exist, so no ordering is left where a failure can truncate the previous contents.
}

#[test]
fn a_reserved_write_draws_on_the_reservation_instead_of_allocating_beside_it() {
	// The reservation exists so a write up to the capacity cannot fail for want of memory. That
	// only holds if the bytes are RELEASED before the file is allocated: allocating first leaves
	// both outstanding at once, so a volume at its capacity would need twice its capacity to
	// write into itself - failing for memory it was sitting on.
	//
	// The observable consequence is that the two never exceed the capacity between them.
	let mut fs = LiberMemFs::mount(Policy::Reserved, 100).expect("mount");
	// Each write replaces the same file, so what is stored is simply the last size written -
	// grown, then grown again to the whole capacity, then shrunk.
	for write in [30usize, 99, 10] {
		fs.write_file(b"f", &alloc::vec![b'z'; write]).expect("write within capacity");
		assert_eq!(fs.used(), write as u64);
		assert_eq!(fs.footprint() + fs.reserved_bytes(), 100, "stored plus held is always the capacity, never more");
	}
}

#[test]
fn no_refused_operation_leaves_a_reserved_volume_holding_less_than_it_should() {
	// `footprint + reserved == capacity` is the invariant the reserved policy IS. Every refusal has
	// to preserve it, and the ones that refuse LATE are the dangerous ones: a write releases
	// reservation bytes before it allocates, so a refusal after that point must give them back.
	// `parent_mut` refusing an absent or non-directory parent is exactly such a late refusal,
	// and returning through `?` there used to walk straight past the resync - leaving the volume
	// permanently holding less than its capacity with nothing stored to account for it.
	let mut fs = LiberMemFs::mount(Policy::Reserved, 128).expect("mount");
	fs.mkdir(b"d").expect("mkdir");
	fs.write_file(b"d/keep", b"1234").expect("write");
	fs.write_file(b"afile", b"12345678").expect("write");

	let refusals: [(&[u8], &[u8]); 6] = [
		(b"missing/f", b"x"),      // parent does not exist
		(b"afile/under", b"x"),    // a file used as a path component
		(b"d", b"x"),              // the target is a directory
		(b"../escape", b"x"),      // a rejected path
		(b"", b"x"),               // the root
		(b"d/keep", &[b'x'; 200]), // past the capacity
	];
	for (path, data) in refusals {
		assert!(fs.write_file(path, data).is_err(), "{:?} must be refused", core::str::from_utf8(path));
		assert_eq!(fs.footprint() + fs.reserved_bytes(), 128, "a refused write left the reservation short: {:?}", core::str::from_utf8(path));
	}

	// The contents are untouched by all of it, and the volume still works.
	assert_eq!(fs.read_file(b"d/keep").expect("read"), b"1234".to_vec());
	assert_eq!(fs.read_file(b"afile").expect("read"), b"12345678".to_vec());
	assert_eq!(fs.used(), 12);
	fs.write_file(b"afile", &[b'y'; 108]).expect("the rest of the capacity is still writable");
	assert_eq!(fs.footprint() + fs.reserved_bytes(), 128);
}

#[test]
fn a_write_after_a_short_regrow_still_reaches_the_capacity() {
	// The reservation can legitimately fall short: regrowing it is best effort, so after a
	// delete on a tight machine it may hold less than the capacity. The write path must not
	// assume it is perfectly in step - releasing "just the difference" would then release less
	// than the file needs, and the write would fail on a volume that has the room.
	//
	// Releasing all of it makes the amount held irrelevant to whether the write can proceed.
	let mut fs = LiberMemFs::mount(Policy::Reserved, 64).expect("mount");
	fs.write_file(b"a", &[b'x'; 63]).expect("fill it");
	assert_eq!(fs.reserved_bytes(), 0, "a full reserved volume holds nothing back");

	// From full, straight to full again with a different file: the release has to cover the
	// whole write, not the difference from a reservation that is currently empty.
	fs.remove(b"a").expect("remove");
	fs.write_file(b"b", &[b'y'; 63]).expect("the whole capacity is writable again");
	assert_eq!(fs.used(), 63);
	assert_eq!(fs.footprint() + fs.reserved_bytes(), 64);
}

#[test]
fn names_cannot_be_used_to_exceed_the_capacity() {
	// A name is memory the caller asked for. Counting only file contents left a hole: a volume
	// could be filled with long names holding nothing and end up a megabyte over its capacity -
	// a quarter of a 4 MiB reserved volume, which would make "reserved" mean nothing.
	//
	// `mkdir` is the sharper case, because a directory stores no data at all and used to have no
	// capacity check whatsoever.
	let mut fs = capped(20);
	fs.mkdir(b"aaaaaaaaaa").expect("ten bytes of name fit");
	assert_eq!(fs.used(), 0, "a directory stores nothing");
	assert_eq!(fs.footprint(), 10, "but its name is memory the volume is accountable for");
	assert_eq!(fs.free(), 10);

	fs.mkdir(b"bbbbbbbbbb").expect("the rest fits exactly");
	assert_eq!(fs.free(), 0);
	assert_eq!(fs.mkdir(b"c"), Err(FsError::NoSpace), "a one-byte name past a full volume is refused");
	assert_eq!(fs.write_file(b"d", b""), Err(FsError::NoSpace), "so is an empty file, which is all name");

	// Freeing a name frees its bytes back.
	fs.rmdir(b"bbbbbbbbbb").expect("rmdir");
	assert_eq!(fs.free(), 10);
	fs.write_file(b"e", b"123456789").expect("nine bytes plus a one-byte name");
	assert_eq!(fs.free(), 0);
}

#[test]
fn directory_operations_keep_the_reservation_in_step_too() {
	// Once names count toward the footprint, `mkdir` and `rmdir` change it - so they are
	// reservation-affecting operations, which they had not been. Neither touched the reservation
	// after that change, so a reserved volume drifted out of step on every directory operation:
	// `mkdir` allocated a name while the reservation still held its bytes, and `rmdir` freed one
	// without taking it back.
	//
	// The existing invariant test only exercised files, which is why this went unnoticed.
	let mut fs = LiberMemFs::mount(Policy::Reserved, 64).expect("mount");
	assert_eq!(fs.footprint() + fs.reserved_bytes(), 64);

	fs.mkdir(b"alpha").expect("mkdir");
	assert_eq!(fs.footprint(), 5, "the name is the whole of a directory's cost");
	assert_eq!(fs.footprint() + fs.reserved_bytes(), 64, "mkdir must take its name from the reservation");

	fs.write_file(b"alpha/f", b"1234").expect("write");
	assert_eq!(fs.footprint(), 10, "five for the directory, one for the file name, four of data");
	assert_eq!(fs.footprint() + fs.reserved_bytes(), 64);

	fs.remove(b"alpha/f").expect("remove");
	fs.rmdir(b"alpha").expect("rmdir");
	assert_eq!(fs.footprint(), 0);
	assert_eq!(fs.reserved_bytes(), 64, "rmdir must give the name's bytes back to the reservation");

	// A refused mkdir must not disturb it either.
	fs.mkdir(b"beta").expect("mkdir");
	assert_eq!(fs.mkdir(b"beta"), Err(FsError::Exists));
	assert_eq!(fs.footprint() + fs.reserved_bytes(), 64, "a refused mkdir changes nothing");
}

#[test]
fn a_path_limit_bounds_the_parsing_and_not_only_the_answer() {
	// The depth limit used to be checked after the whole path had been split, so a path of a
	// thousand segments was parsed into a thousand entries before being refused for having too
	// many. That allocation is sized by the caller, charged to nothing, and happens on every
	// operation including reads on a volume with no capacity left.
	//
	// The refusal is the same; what changed is that it now costs a bounded amount of work.
	let mut fs = capped(0);
	let sprawling: Vec<u8> = core::iter::repeat("s/").take(10_000).collect::<String>().into_bytes();
	assert_eq!(fs.read_file(&sprawling), Err(FsError::TooLong));
	assert_eq!(fs.write_file(&sprawling, b"x"), Err(FsError::TooLong));

	// Exactly at the limit is still accepted, so the bound did not move.
	let deepest: Vec<u8> = core::iter::repeat("d/").take(MAX_PATH_DEPTH).collect::<String>().into_bytes();
	assert_eq!(fs.read_file(&deepest), Err(FsError::NotFound), "sixteen segments is a legal path, merely absent");
}

#[test]
fn free_is_room_for_data_and_a_new_entry_still_pays_for_its_name() {
	// `free()` subtracts the names already held, which is what the capacity bounds - but it cannot
	// subtract the name of a file that does not exist yet. So the number is exact for REWRITING an
	// existing file and one name short for CREATING one, and the comment that used to sit on it
	// claimed otherwise.
	//
	// This is a contract, not a defect: nothing can know how long the next name will be. It is
	// pinned here because a wrong belief about it is what produced two of the accounting defects
	// in this filesystem.
	let mut fs = capped(20);
	fs.write_file(b"aaaa", &[b'x'; 10]).expect("write");
	assert_eq!(fs.free(), 6, "twenty less ten of data and four of name");

	// Rewriting into the existing entry can use all of it, because the name is already paid for.
	fs.write_file(b"aaaa", &[b'x'; 16]).expect("a rewrite may use the whole of free()");
	fs.write_file(b"aaaa", &[b'x'; 10]).expect("back down");

	// Creating a new one cannot: the name is charged on top.
	assert_eq!(fs.write_file(b"b", &[b'x'; 6]), Err(FsError::NoSpace), "free() bytes plus a name is over");
	fs.write_file(b"b", &[b'x'; 5]).expect("free() less the name fits exactly");
	assert_eq!(fs.free(), 0);
}
