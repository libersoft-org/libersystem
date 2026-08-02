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
	let mut fs = capped(8);
	fs.write_file(b"f", b"aaaaaaaa").expect("fills the volume exactly");
	assert_eq!(fs.used(), 8);
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
	let mut fs = capped(10);
	fs.write_file(b"a", b"12345").expect("fits");
	assert_eq!(fs.free(), 5);
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
	fs.write_file(b"a", b"1234567890123456").expect("the whole capacity is writable");
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
	assert_eq!(fs.reserved_bytes(), 24, "what a file uses is no longer held separately");

	// Shrinking must return the difference to the reservation, not to the heap.
	fs.write_file(b"a", &[b'x'; 8]).expect("shrink");
	assert_eq!(fs.reserved_bytes(), 56, "shrinking gives the memory back to the volume");

	// So must deleting.
	fs.remove(b"a").expect("remove");
	assert_eq!(fs.reserved_bytes(), 64, "an emptied reserved volume holds all of it again");
	assert_eq!(fs.used(), 0);

	// And the guarantee still holds afterwards: the whole capacity is writable again.
	fs.write_file(b"b", &[b'y'; 64]).expect("the full capacity is available after the round trip");
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
