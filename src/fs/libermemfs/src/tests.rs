use super::*;

// An allocator that can be made to fail, so the reserved policy can be tested for what it is
// FOR: behaviour when memory runs out.
//
// Every test before this one ran on a host heap with room to spare, which made the whole class of
// transient-peak defects invisible - a rewrite that briefly needs two copies of a file passes
// happily when there is memory for four. The budget is thread-local and const-initialised, so
// arming it in one test does not disturb the others running beside it and no allocation happens
// while setting it up.
//
// Failing allocations return null. A fallible path (`try_reserve_exact`) turns that into
// `NoSpace`; an infallible one aborts the process. That asymmetry is the point: a test that dies
// here has found an allocation that should have been fallible and is not.
struct Budgeted;

thread_local! {
	static BUDGET: core::cell::Cell<usize> = const { core::cell::Cell::new(usize::MAX) };
}

unsafe impl std::alloc::GlobalAlloc for Budgeted {
	unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
		let allowed = BUDGET.with(|budget| {
			let left = budget.get();
			if left == usize::MAX {
				return true;
			}
			if layout.size() > left {
				return false;
			}
			budget.set(left - layout.size());
			true
		});
		if !allowed {
			return core::ptr::null_mut();
		}
		unsafe { std::alloc::System.alloc(layout) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
		BUDGET.with(|budget| {
			let left = budget.get();
			if left != usize::MAX {
				budget.set(left + layout.size());
			}
		});
		unsafe { std::alloc::System.dealloc(ptr, layout) }
	}
}

#[global_allocator]
static ALLOCATOR: Budgeted = Budgeted;

// Run `body` with at most `bytes` outstanding, and lift the cap afterwards however it ends.
fn within(bytes: usize, body: impl FnOnce()) {
	BUDGET.with(|budget| budget.set(bytes));
	let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
	BUDGET.with(|budget| budget.set(usize::MAX));
	if let Err(panic) = outcome {
		std::panic::resume_unwind(panic);
	}
}

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

	// Shrinking does NOT give the memory back, because the file keeps its allocation: a vector
	// that is cleared still owns its buffer. Reporting it as free is what let a volume be pushed
	// past its capacity by writing large and then truncating.
	fs.write_file(b"a", &[b'x'; 8]).expect("shrink");
	assert_eq!(fs.used(), 8, "the volume stores eight bytes");
	assert_eq!(fs.reserved_bytes(), 23, "but still holds what the file has not released");

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
	let sprawling: Vec<u8> = "s/".repeat(10_000).into_bytes();
	assert_eq!(fs.read_file(&sprawling), Err(FsError::TooLong));
	assert_eq!(fs.write_file(&sprawling, b"x"), Err(FsError::TooLong));

	// Exactly at the limit is still accepted, so the bound did not move.
	let deepest: Vec<u8> = "d/".repeat(MAX_PATH_DEPTH).into_bytes();
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
	assert_eq!(fs.free(), 0, "the file now holds the whole volume");

	// Writing less does not hand the difference back: the file keeps the allocation it grew to.
	fs.write_file(b"aaaa", &[b'x'; 10]).expect("back down");
	assert_eq!(fs.used(), 10, "ten bytes are stored");
	assert_eq!(fs.free(), 0, "and none of what it grew to is free again");

	// Removing it is what releases the allocation.
	fs.remove(b"aaaa").expect("remove");
	assert_eq!(fs.free(), 20);
}

// A deterministic pseudo-random sequence. No dependency, and the same run every time, so a
// failure is reproducible rather than a story about a machine that saw it once.
struct Lcg(u64);

impl Lcg {
	fn next(&mut self) -> u64 {
		self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
		self.0 >> 33
	}
}

#[test]
fn the_reservation_survives_ten_thousand_random_operations() {
	// Five of the ten defects in this filesystem were the footprint and the reservation drifting
	// apart, and every one was found by READING the code - the tests written alongside it all
	// checked hand-picked sequences, so they only ever asked the questions their author had
	// already thought of.
	//
	// This asks the one question mechanically: after EVERY operation, whatever it was and whether
	// it succeeded or was refused, does the volume still hold exactly what it should? The capacity
	// is deliberately tiny so that refusals - where four of those five defects lived - are the
	// common case rather than the exception.
	const CAPACITY: usize = 64;
	let paths: [&[u8]; 8] = [b"a", b"bb", b"ccc", b"d", b"d/x", b"d/yy", b"e", b"e/f"];
	let mut fs = LiberMemFs::mount(Policy::Reserved, CAPACITY).expect("mount");
	let mut rng = Lcg(0x5eed);
	let mut refusals = 0usize;
	let mut successes = 0usize;
	let mut model = Model::default();

	for step in 0..10_000 {
		let path = paths[(rng.next() % paths.len() as u64) as usize];
		let kind = rng.next() % 6;
		let wrote = if kind == 0 { (rng.next() % 40) as usize } else { 0 };
		let outcome = match kind {
			0 => fs.write_file(path, &alloc::vec![b'z'; wrote]),
			1 => fs.write_file(path, b""),
			2 => fs.mkdir(path),
			3 => fs.remove(path),
			4 => fs.rmdir(path),
			_ => fs.read_file(path).map(|_| ()),
		};
		if outcome.is_ok() {
			successes += 1
		} else {
			refusals += 1
		}
		model.apply(kind, outcome.is_ok(), path, wrote);

		// Checked against a model kept BESIDE the filesystem rather than against the filesystem's
		// own arithmetic. `resync_reservation` computes its target from `footprint()`, so a test
		// asserting `footprint + reserved == capacity` agrees with the implementation even when
		// `footprint()` is systematically wrong: the two share the error. The model adds the same
		// number up from what the operations are known to have stored.
		assert_eq!(model.footprint(), fs.footprint(), "step {step}: the model and the volume disagree about what is stored");

		// The invariant the reserved policy IS.
		assert_eq!(fs.footprint() + fs.reserved_bytes(), CAPACITY as u64, "step {step}: stored plus held is no longer the capacity (footprint {}, reserved {})", fs.footprint(), fs.reserved_bytes());
		// And the bound it enforces, which no refusal may leave crossed.
		assert!(fs.footprint() <= CAPACITY as u64, "step {step}: the footprint passed the capacity");
	}

	// A run that never stored anything, or never refused anything, would pass the assertions above
	// while proving nothing. Both paths have to have been taken.
	assert!(successes > 500, "the sequence barely mutated the volume: {successes} succeeded");
	assert!(refusals > 500, "the sequence barely refused anything: {refusals} refused");
}

#[test]
fn a_file_used_as_a_directory_is_not_found_missing() {
	// `lookup` returned `Option`, so it could not tell "absent" from "a file used as a directory".
	// Reads answered NotFound while writes on the SAME path answered NotDir through the mutable
	// walk - one wrong path, two different errors, and `FsError::NotDir` exists for this case.
	let mut fs = capped(1024);
	fs.write_file(b"f", b"x").expect("write");
	fs.mkdir(b"d").expect("mkdir");

	assert_eq!(fs.read_file(b"f/child"), Err(FsError::NotDir), "a file is not a directory to read through");
	assert_eq!(fs.list_entries(b"f/child").err(), Some(FsError::NotDir));
	assert_eq!(fs.write_file(b"f/child", b"x"), Err(FsError::NotDir));
	assert_eq!(fs.mkdir(b"f/child"), Err(FsError::NotDir));

	// An absent directory is still NotFound, which is the distinction being drawn.
	assert_eq!(fs.read_file(b"absent/child"), Err(FsError::NotFound));
	assert_eq!(fs.write_file(b"absent/child", b"x"), Err(FsError::NotFound));
	assert_eq!(fs.mkdir(b"absent/child"), Err(FsError::NotFound));
	assert_eq!(fs.read_file(b"d/child"), Err(FsError::NotFound));
}

#[test]
fn a_full_volume_still_says_why_a_path_is_wrong() {
	// The capacity used to be checked before the parent was known to exist, so every wrong path on
	// a full volume came back NoSpace - the one error that says nothing about the path. Worse, a
	// large write to a path that could never exist released the reservation and copied the whole
	// payload before finding out.
	let mut fs = capped(8);
	fs.write_file(b"f", b"1234567").expect("fills it");
	assert_eq!(fs.free(), 0, "the volume is full");

	assert_eq!(fs.write_file(b"missing/x", b""), Err(FsError::NotFound), "an absent parent is NotFound even with no room");
	assert_eq!(fs.write_file(b"f/x", b""), Err(FsError::NotDir), "a file as parent is NotDir even with no room");
	assert_eq!(fs.mkdir(b"missing/x"), Err(FsError::NotFound));
	assert_eq!(fs.mkdir(b"f/x"), Err(FsError::NotDir));
	assert_eq!(fs.write_file(b"g", b"x"), Err(FsError::NoSpace), "a writable path still reports the real reason");
}

#[test]
fn a_path_of_separators_is_refused_without_being_walked() {
	// The depth limit is checked inside the loop, which bounds the vector but not the work: a path
	// of nothing but separators has no non-empty segments, so it never reaches the limit however
	// long it is - and `from_utf8` walks the whole thing first regardless.
	let mut fs = capped(64);
	let slashes = alloc::vec![b'/'; MAX_PATH_BYTES + 1];
	assert_eq!(fs.read_file(&slashes), Err(FsError::TooLong));
	assert_eq!(fs.write_file(&slashes, b"x"), Err(FsError::TooLong));
	assert_eq!(fs.mkdir(&slashes), Err(FsError::TooLong));

	let deepest: Vec<u8> = "d/".repeat(MAX_PATH_DEPTH).into_bytes();
	assert!(deepest.len() <= MAX_PATH_BYTES);
	assert_eq!(fs.read_file(&deepest), Err(FsError::NotFound), "legal depth is merely absent, not too long");
}

#[test]
fn rewriting_reuses_the_files_own_allocation() {
	// A rewrite used to build a second vector while the old contents were still in the tree, so a
	// volume at its capacity needed twice the file to write into itself - the very thing the
	// capacity check claims to avoid by subtracting the previous size.
	let mut fs = LiberMemFs::mount(Policy::Reserved, 128).expect("mount");
	fs.write_file(b"f", &[b'x'; 100]).expect("write");
	assert_eq!(fs.used(), 100);

	fs.write_file(b"f", &[b'y'; 10]).expect("shrink");
	assert_eq!(fs.read_file(b"f").expect("read"), alloc::vec![b'y'; 10]);
	fs.write_file(b"f", &[b'z'; 100]).expect("regrow to the original size");
	assert_eq!(fs.read_file(b"f").expect("read"), alloc::vec![b'z'; 100]);
	assert_eq!(fs.footprint() + fs.reserved_bytes(), 128, "the invariant holds across the round trip");

	// A refused rewrite still leaves the previous contents intact: the reserve happens before the
	// clear, which the in-place path must not break.
	assert_eq!(fs.write_file(b"f", &[b'q'; 200]), Err(FsError::NoSpace));
	assert_eq!(fs.read_file(b"f").expect("read"), alloc::vec![b'z'; 100], "the refused rewrite changed nothing");
}

#[test]
fn a_same_size_rewrite_fits_in_a_heap_the_size_of_the_volume() {
	// The defect this stands for: a rewrite built a SECOND vector while the old contents were
	// still in the tree, so a volume at its capacity needed twice the file to write into itself.
	// On a host with spare memory that is invisible - which is why fifteen readings of the code
	// found it and no test did.
	//
	// The payloads are built OUTSIDE the budget, because a caller's buffer is not part of the
	// volume: in the storage service the bytes arrive in a transferred buffer. What the budget
	// covers is the volume itself, with a small allowance for the tree around it.
	const CAPACITY: usize = 64 * 1024;
	let first = alloc::vec![b'x'; CAPACITY - 1];
	let second = alloc::vec![b'y'; CAPACITY - 1];
	within(CAPACITY + 16 * 1024, || {
		let mut fs = LiberMemFs::mount(Policy::Capped, CAPACITY).expect("mount");
		fs.write_file(b"f", &first).expect("the volume fills");
		fs.write_file(b"f", &second).expect("a same-size rewrite fits without a second copy");
		assert_eq!(fs.used(), CAPACITY as u64 - 1);
	});
}

#[test]
fn a_refused_write_does_not_compete_with_its_own_reservation() {
	// A late refusal used to resync while the refused file's bytes were still allocated: the
	// volume asked for its capacity back while holding a copy of the data it had just declined to
	// store, so the regrow could fall short for exactly that memory.
	//
	// The parent is now resolved before anything is released, so the state cannot arise.
	// The budget is calibrated so that HOLDING the refused payload makes the regrow fall short:
	// the volume plus a little, minus half a volume of payload, is less than the volume. With the
	// old ordering the reservation could not come back; with the parent resolved first there is
	// nothing to come back from.
	const CAPACITY: usize = 32 * 1024;
	let payload = alloc::vec![b'z'; CAPACITY / 2];
	within(CAPACITY + 4 * 1024, || {
		let mut fs = LiberMemFs::mount(Policy::Reserved, CAPACITY).expect("mount");
		assert_eq!(fs.reserved_bytes(), CAPACITY as u64);

		assert_eq!(fs.write_file(b"missing/f", &payload), Err(FsError::NotFound), "an absent parent refuses before allocating");
		assert_eq!(fs.footprint() + fs.reserved_bytes(), CAPACITY as u64, "a refused write leaves the reservation whole");

		// And the volume still works up to its capacity afterwards, which is the guarantee.
		fs.write_file(b"f", &payload).expect("the capacity is still writable");
		assert_eq!(fs.footprint() + fs.reserved_bytes(), CAPACITY as u64);
	});
}

#[test]
fn a_read_too_large_for_the_heap_is_refused_rather_than_fatal() {
	// `read_file` cloned the stored bytes, and `Vec::clone` aborts when the memory is not there.
	// In a storage service that is a crash where a refusal would do. If this test DIES rather than
	// failing, an infallible allocation has come back.
	const CAPACITY: usize = 64 * 1024;
	let mut fs = LiberMemFs::mount(Policy::Capped, CAPACITY).expect("mount");
	// Less the name, which counts toward the capacity like any other byte.
	fs.write_file(b"big", &alloc::vec![b'x'; CAPACITY - b"big".len()]).expect("write");
	within(4 * 1024, || {
		assert_eq!(fs.read_file(b"big"), Err(FsError::NoSpace), "a read that cannot be satisfied is refused, not fatal");
	});
	assert_eq!(fs.read_file(b"big").expect("read").len(), CAPACITY - b"big".len(), "the file is untouched by the refusal");
}

#[test]
fn a_listing_too_large_for_the_heap_is_refused_rather_than_fatal() {
	// Same for `list_entries`, which cloned every name and collected them infallibly.
	let mut fs = capped(64 * 1024);
	for i in 0..64 {
		fs.write_file(alloc::format!("entry-{i:04}").as_bytes(), b"x").expect("write");
	}
	within(512, || {
		assert_eq!(fs.list_entries(b"").err(), Some(FsError::NoSpace), "a listing that cannot be satisfied is refused, not fatal");
	});
	assert_eq!(fs.list_entries(b"").expect("list").len(), 64);
}

#[test]
fn a_mount_that_cannot_take_its_capacity_fails_rather_than_pretending() {
	// The reserved policy's whole claim is that the memory is taken at mount. Under a budget
	// smaller than the capacity, the mount must fail - not succeed holding less.
	within(16 * 1024, || {
		assert_eq!(LiberMemFs::mount(Policy::Reserved, 1024 * 1024).err(), Some(FsError::NoSpace), "a reserved mount that cannot take its capacity fails");
		// A capped volume of the same size mounts, because it takes nothing yet.
		let fs = LiberMemFs::mount(Policy::Capped, 1024 * 1024).expect("a capped volume always mounts");
		assert_eq!(fs.reserved_bytes(), 0);
	});
}

// What the volume should contain, tracked independently of the filesystem's own accounting.
//
// It deliberately calls nothing the implementation uses to answer the same question: it adds up
// what the operations are known to have stored, so a systematic error in `footprint()` cannot hide
// behind agreeing with itself.
#[derive(Default)]
struct Model {
	files: alloc::collections::BTreeMap<alloc::string::String, usize>,
	dirs: alloc::collections::BTreeSet<alloc::string::String>,
}

impl Model {
	fn apply(&mut self, kind: u64, succeeded: bool, path: &[u8], wrote: usize) {
		if !succeeded {
			return;
		}
		let name = core::str::from_utf8(path).expect("test paths are utf-8");
		match kind {
			// A file holds the largest size ever written to it until it is removed, because a
			// cleared vector keeps its buffer. The model derives that from the sizes it asked
			// for - it never reads a length back from the implementation it is checking.
			0 | 1 => {
				let held = self.files.get(name).copied().unwrap_or(0).max(wrote);
				self.files.insert(alloc::string::String::from(name), held);
			}
			2 => {
				self.dirs.insert(alloc::string::String::from(name));
			}
			3 => {
				self.files.remove(name);
			}
			4 => {
				self.dirs.remove(name);
			}
			_ => {}
		}
	}

	// The same number `footprint()` reports, derived from the model instead: every file's data,
	// plus the final segment of every path, which is the name its parent holds.
	fn footprint(&self) -> u64 {
		let data: usize = self.files.values().sum();
		let names: usize = self.files.keys().chain(self.dirs.iter()).map(|path| path.rsplit('/').next().map_or(0, str::len)).sum();
		(data + names) as u64
	}
}

#[test]
fn a_shrunk_file_keeps_its_allocation_and_the_volume_says_so() {
	// The regression this stands for: rewriting in place stopped the transient second copy but
	// `Vec::clear` keeps the buffer, so a file shrunk to nothing still owned its memory while the
	// accounting counted `len` and reported the volume empty. That is worse than what it replaced
	// - the old defect held two copies briefly and the numbers showed both; this one held one
	// copy forever and the numbers showed neither.
	//
	// Measured against real memory, not accounting: 64 KiB volume, 80 KiB heap. If the shrink
	// released the file's buffer, both writes fit one after the other. If it did not - and it does
	// not - the volume must refuse the second write rather than accept it and run the heap out.
	let big = alloc::vec![b'x'; 60 * 1024];
	within(80 * 1024, || {
		let mut fs = LiberMemFs::mount(Policy::Capped, 64 * 1024).expect("mount");
		fs.write_file(b"a", &big).expect("write");
		fs.write_file(b"a", b"").expect("shrink to nothing");
		assert_eq!(fs.used(), 0, "nothing is stored any more");
		assert!(fs.free() < 8 * 1024, "but the volume still counts what the file holds: free={}", fs.free());
		assert_eq!(fs.write_file(b"b", &big), Err(FsError::NoSpace), "a second large file is refused, not accepted into memory that is gone");

		// Removing the file is what actually releases it, and then the write fits.
		fs.remove(b"a").expect("remove");
		assert_eq!(fs.free(), 64 * 1024, "the name goes with the file");
		fs.write_file(b"b", &big).expect("after the release the volume has the room it reports");
	});
}

#[test]
fn growing_past_a_files_high_water_mark_needs_both_blocks() {
	// The growth case the earlier test only appeared to cover. Rewriting 100 -> 10 -> 100 never
	// reallocates: the file kept the capacity it grew to, so the last write reuses it. A real
	// growth past the high-water mark has to move the buffer, and while it does, the old block
	// and the new one both exist - which is what this measures.
	//
	// The budget holds the volume, the new block and a little slack, but NOT the old block as
	// well. If the write is refused, it is refused cleanly and the file is intact; the point is
	// that it cannot corrupt or abort.
	const SMALL: usize = 16 * 1024;
	const LARGE: usize = 48 * 1024;
	let small = alloc::vec![b'a'; SMALL];
	let large = alloc::vec![b'b'; LARGE];
	within(LARGE + 8 * 1024, || {
		let mut fs = LiberMemFs::mount(Policy::Capped, 64 * 1024).expect("mount");
		fs.write_file(b"f", &small).expect("the first write fits");
		assert_eq!(fs.footprint(), SMALL as u64 + 1);

		// 16 KiB held plus 48 KiB wanted is over the budget, so the reallocation cannot happen.
		// It must come back as a refusal, with the file still readable.
		match fs.write_file(b"f", &large) {
			Ok(()) => assert_eq!(fs.used(), LARGE as u64, "if it fit, it fit completely"),
			Err(error) => {
				assert_eq!(error, FsError::NoSpace, "a growth that cannot be satisfied is refused");
				assert_eq!(fs.read_file(b"f").expect("the file survives").len(), SMALL, "a refused growth leaves the file whole");
			}
		}
	});

	// With room for both blocks it simply succeeds, which shows the refusal above was about
	// memory and not about the capacity rule.
	within(SMALL + LARGE + 8 * 1024, || {
		let mut fs = LiberMemFs::mount(Policy::Capped, 64 * 1024).expect("mount");
		fs.write_file(b"f", &small).expect("write");
		fs.write_file(b"f", &large).expect("the growth fits when both blocks do");
		assert_eq!(fs.used(), LARGE as u64);
	});
}

#[test]
fn giving_reservation_back_actually_reduces_what_is_held() {
	// Guards the reservation's accounting across ordinary use: what is stored plus what is held is
	// the capacity, and storing more holds less.
	//
	// It does NOT reach the shrink branch, and saying so is the point. That branch subtracted
	// `chunk.len()`, which is zero by design because only a chunk's capacity is taken - so it
	// would free every chunk while the accounting claimed the memory was still held. The bug is
	// fixed; this test cannot prove it, because every operation that grows the footprint releases
	// the whole reservation first and so always takes the GROW path. The branch is unreachable
	// through the public API today, which is why the defect sat there unnoticed and why it will
	// bite the first time an operation changes the footprint without releasing.
	let mut fs = LiberMemFs::mount(Policy::Reserved, 64 * 1024).expect("mount");
	assert_eq!(fs.reserved_bytes(), 64 * 1024);

	// Storing something forces the reservation down to the remainder.
	fs.write_file(b"f", &alloc::vec![b'x'; 16 * 1024]).expect("write");
	let held = fs.reserved_bytes();
	assert_eq!(fs.footprint() + held, 64 * 1024, "what is stored plus what is held is the capacity");

	// Storing more forces it down again - the step the broken arithmetic could not take.
	fs.write_file(b"g", &alloc::vec![b'y'; 16 * 1024]).expect("write");
	assert!(fs.reserved_bytes() < held, "holding less after storing more: {} then {}", held, fs.reserved_bytes());
	assert_eq!(fs.footprint() + fs.reserved_bytes(), 64 * 1024);
	assert!(fs.reservation_intact(), "and the volume still holds exactly what it should");
}

#[test]
fn a_reserved_mount_that_cannot_take_its_slot_fails_rather_than_aborting() {
	// A reserved mount that cannot be satisfied must be refused rather than fatal. If this test
	// DIES instead of failing, an infallible allocation is back on the mount path.
	//
	// It does not isolate the slot in the outer vector: with a budget this small the chunk itself
	// is refused first, so the `try_reserve` guarding the slot is never the thing that fires.
	// Calibrating a budget that admits a megabyte chunk and refuses a pointer-sized slot is not
	// something this harness can do reliably, so the slot's fallibility is argued from the code
	// rather than demonstrated here.
	within(2 * 1024, || {
		assert_eq!(LiberMemFs::mount(Policy::Reserved, 1024 * 1024).err(), Some(FsError::NoSpace), "a mount that cannot be satisfied is refused, not fatal");
	});
}

#[test]
fn an_owned_rewrite_is_judged_by_the_write_it_will_actually_do() {
	// The guard and the write disagreed. `write_file_owned` charged the incoming vector's
	// CAPACITY unconditionally, while `adopt` keeps the file's existing buffer whenever the new
	// contents fit and copies into it - taking on nothing new at all.
	//
	// That matters because an allocator may hand back more than was asked for, which this
	// filesystem is careful to respect everywhere else. A stream that fits the destination could
	// therefore be refused for the size of the buffer it happened to arrive in.
	//
	// Built to be unambiguous: the volume has room for the file and nothing more, and the
	// incoming vector holds less data in a much larger allocation.
	let mut fs = LiberMemFs::mount(Policy::Capped, 4096 + 1).expect("mount");
	fs.write_file(b"a", &alloc::vec![b'x'; 4096]).expect("seed a file that fills the volume");

	let mut incoming: Vec<u8> = Vec::with_capacity(8192);
	incoming.extend_from_slice(&[b'y'; 1024]);
	assert!(incoming.capacity() >= 8192, "the point of the test is an over-allocated buffer");

	fs.write_file_owned(b"a", incoming).expect("a rewrite that fits the file's own buffer is not refused for the size of the vector it came in");
	assert_eq!(fs.read_file(b"a").map(|bytes| bytes.to_vec()), Ok(alloc::vec![b'y'; 1024]), "and the bytes stored are the bytes sent");
	// The name costs the one byte the capacity was given over the file, so a volume holding both
	// is exactly full - and stays that way, because the rewrite took on nothing new.
	assert_eq!(fs.free(), 0, "the file kept its allocation, so the volume is no freer than before");
}

#[test]
fn a_stream_is_charged_to_the_volume_while_it_arrives() {
	// The conflict this closes: the storage service accumulated a stream in ITS heap, so the
	// volume's accounting knew nothing about it. `free()` kept reporting room that was already
	// spent, and a reserved volume - whose whole promise is that the memory is there - could not
	// cover the operation that needed the promise most.
	//
	// Measured against real memory, not against the numbers: a 64 KiB volume and an 80 KiB heap.
	// Accumulating outside the volume, two 40 KiB streams both "fit" and the second runs the heap
	// out. Charged as they arrive, the second is refused by the volume instead.
	let chunk = alloc::vec![b'x'; 20 * 1024];
	within(80 * 1024, || {
		let mut fs = LiberMemFs::mount(Policy::Capped, 64 * 1024).expect("mount");
		fs.stream_begin(b"a").expect("begin");
		fs.stream_push(&chunk).expect("first chunk");
		assert!(fs.free() < 46 * 1024, "the volume counts what has arrived: free={}", fs.free());
		fs.stream_push(&chunk).expect("second chunk");
		fs.stream_commit().expect("commit");
		// `used()` rather than reading it back: `read_file` COPIES, so a read-back inside this
		// budget would fail for the reader's allocation and say nothing about the accounting.
		assert_eq!(fs.used(), 40 * 1024, "and the file holds what was streamed");

		// A second stream of the same size does not fit, and is refused by the VOLUME rather than
		// by the allocator.
		fs.stream_begin(b"b").expect("begin the second");
		fs.stream_push(&chunk).expect("its first chunk");
		assert_eq!(fs.stream_push(&chunk), Err(FsError::NoSpace), "the volume refuses what it cannot hold");
		assert!(!fs.streaming(), "a refused push abandons the stream rather than leaving it half-received");
		assert_eq!(fs.used(), 40 * 1024, "and the file that was already there is untouched");
	});
}

#[test]
fn a_stream_that_grows_past_its_spare_capacity_reserves_fallibly() {
	// Growing a stream past what the heap can give reports NoSpace rather than aborting.
	//
	// It does NOT reproduce the defect it was written for, and that is worth stating rather than
	// implying. `stream_push` reserved `want - capacity` where `try_reserve_exact(additional)`
	// guarantees `len + additional`; asking from CAPACITY is too little whenever `len < capacity`,
	// and the append then grows infallibly. But this path reserves exactly what it appends, so
	// `len` and `capacity` move together and the gap only opens if the allocator returns more than
	// it was asked for. Forcing that needs control of the allocator this harness does not have.
	//
	// What it does cover is the fallible path itself: a volume with room for the data and a heap
	// without it must refuse, not abort.
	let first = alloc::vec![b'a'; 24 * 1024];
	let second = alloc::vec![b'b'; 40 * 1024];
	within(48 * 1024, || {
		let mut fs = LiberMemFs::mount(Policy::Capped, 1024 * 1024).expect("mount");
		fs.stream_begin(b"g").expect("begin");
		fs.stream_push(&first).expect("the first chunk fits");
		// The volume has room for the whole stream; the HEAP does not. Growing must report that
		// rather than abort, which is only possible if the reservation asked for the right amount.
		assert_eq!(fs.stream_push(&second), Err(FsError::NoSpace), "growing past the spare capacity is refused, not aborted");
		assert!(!fs.streaming(), "and the refused stream is abandoned rather than left half-received");
	});
}

#[test]
fn a_streamed_rewrite_is_not_promised_the_room_an_ordinary_one_has() {
	// The two answers differ, and the difference is real rather than an accounting slip.
	//
	// An ordinary rewrite replaces a file's contents in place, so it may spend what that file
	// already holds. A stream cannot: it keeps the old contents until the commit - which is what
	// makes a failed transfer leave the file as it was - and builds the new ones beside them.
	//
	// Answering both with the same figure let a stream be admitted by the preflight and refused by
	// its first chunk, on a volume that had told it there was room.
	let mut fs = LiberMemFs::mount(Policy::Capped, 4096).expect("mount");
	let payload = alloc::vec![b'x'; 4000];
	fs.write_file(b"f", &payload).expect("fill the volume");

	let ordinary = fs.writable_len(b"f").expect("an ordinary rewrite has an answer");
	let streamed = fs.stream_len(b"f").expect("so does a streamed one");
	assert!(ordinary >= 4000, "an ordinary rewrite may reuse what the file holds: {ordinary}");
	assert!(streamed < 100, "a streamed rewrite may not, because the old contents stay: {streamed}");

	// And the preflight now matches what the stream can actually do.
	assert_eq!(fs.stream_begin(b"f"), Ok(()), "opening the stream is still allowed");
	assert_eq!(fs.stream_push(&payload), Err(FsError::NoSpace), "but its first chunk is refused, as the plan said");
}

#[test]
fn what_the_stream_preflight_promises_a_nested_path_is_what_it_can_take() {
	// The preflight subtracted the final NAME while the stream charged the whole PATH, so a
	// destination a few directories deep was told a limit it could not reach: accepted under its
	// declared ceiling and refused with `NoSpace` before getting there. The deeper the path, the
	// bigger the lie.
	//
	// The property is one sentence: write exactly what you were promised, and it commits.
	let mut fs = capped(4096);
	fs.mkdir(b"a").expect("a");
	fs.mkdir(b"a/b").expect("a/b");
	fs.mkdir(b"a/b/c").expect("a/b/c");
	let path: &[u8] = b"a/b/c/file";
	let promised = fs.stream_len(path).expect("a nested destination has an answer");
	fs.stream_begin(path).expect("open the stream");
	let payload = alloc::vec![b'x'; promised];
	assert_eq!(fs.stream_push(&payload), Ok(()), "the promised bytes are accepted");
	assert_eq!(fs.stream_commit(), Ok(()), "and the volume takes them");
	assert_eq!(fs.read_file(path).as_deref(), Ok(&payload[..]), "all of them");
}

#[test]
fn a_reserved_volume_opens_a_stream_out_of_its_own_reservation() {
	// A reserved volume holds its whole free capacity, so allocating the pending path BESIDE the
	// reservation competes with memory the volume is itself sitting on: the stream can be refused
	// for want of bytes the volume was holding for exactly this. The ordinary write path has
	// released before allocating for as long as the reservation has existed.
	//
	// The budget is what makes it visible, and this test was once deleted for want of it: with a
	// host heap of gigabytes the extra allocation succeeds either way and the assertion is
	// decoration. Capped just past the volume, the path's bytes can only come from the reservation.
	const CAPACITY: usize = 4096;
	// The slack is SMALLER than the path: without releasing first, those bytes are simply not
	// there, which is the whole condition. At `CAPACITY + 1024` the test passed with the fix
	// reverted, because a kilobyte of slack covers a 200-byte name however the volume behaves.
	// Built BEFORE the budget: `vec![...]` is an infallible allocation, and one made inside the cap
	// aborts the process rather than failing - which is the harness working as intended and is why
	// the test's own scaffolding has to be allocated outside it.
	//
	// A long FLAT name: it costs real bytes without needing parent directories, which would have to
	// be created first and would themselves be refused on a budget this tight.
	let name = alloc::vec![b'n'; 255];
	within(CAPACITY + 320, || {
		let mut fs = LiberMemFs::mount(Policy::Reserved, CAPACITY).expect("a reserved volume mounts");
		assert!(fs.reserved_bytes() > 0, "the volume is holding its capacity");
		assert_eq!(fs.stream_begin(&name), Ok(()), "a stream opens against the volume's own reservation");
		assert!(fs.reservation_intact(), "and the volume still keeps its promise");
		fs.stream_abort();
		assert!(fs.reservation_intact(), "as it does after giving the stream up");
	});
}

#[test]
fn receiving_into_the_volume_costs_one_buffer_rather_than_two() {
	// The earlier accounting test built its chunk OUTSIDE the budgeted block, so the transport's
	// own allocation was never measured - and that allocation was the point: the service used to
	// receive a message into its own vector and the filesystem then copied it, so every chunk
	// existed twice.
	//
	// Here the space comes from the volume and is written in place, so the peak is one buffer.
	// Sized so two copies would not fit: a 64 KiB volume in a 96 KiB heap, taking 48 KiB in.
	within(96 * 1024, || {
		let mut fs = LiberMemFs::mount(Policy::Capped, 64 * 1024).expect("mount");
		fs.stream_begin(b"a").expect("begin");
		let spare = fs.stream_spare(48 * 1024).expect("the volume hands out room for the chunk");
		assert_eq!(spare.len(), 48 * 1024, "and it is the size that was asked for");
		// What a receive would do: fill it in place. No second buffer exists at any point.
		for (i, byte) in spare.iter_mut().enumerate() {
			*byte = (i & 0xff) as u8;
		}
		fs.stream_advance(48 * 1024, 48 * 1024);
		fs.stream_commit().expect("commit");
		assert_eq!(fs.used(), 48 * 1024, "the file holds what was received");
	});
}

// P02M0109, fourth round: the residue of the reserved stream's commit.

#[test]
fn a_reserved_stream_commit_leaves_the_reservation_whole() {
	// `stream_commit` takes the pending write out of the volume - which stops the PATH counting
	// toward the footprint - and then has to keep that path alive while the store reads it. The
	// resync therefore ran with those bytes held and unaccounted, and on a heap with no room to
	// spare the regrow could fail over them. Nothing resynced afterwards, so a commit that
	// SUCCEEDED could leave the volume no longer holding its capacity.
	//
	// Smaller than the version this replaced - that one competed with the whole file's buffer,
	// this one with a name - which is why it survived: only a heap capped just past the volume
	// makes the difference reachable at all.
	//
	// NOTHING is asserted inside the budgeted block. A failing assertion there panics, the panic
	// machinery allocates, the cap refuses it and the process ABORTS - so the first version of this
	// test turned an ordinary red into a dead harness, and did it only when run after its
	// neighbours. Recording inside and judging outside costs one struct and cannot do that.
	const CAPACITY: usize = 4096;
	let name = alloc::vec![b'n'; 255];
	let data = alloc::vec![0xA5u8; 512];
	let mut committed: Result<(), FsError> = Err(FsError::Invalid);
	let mut intact = false;
	let mut held = (0u64, 0u64);
	let mut volume: Option<LiberMemFs> = None;
	within(CAPACITY + 512, || {
		let mut fs = LiberMemFs::mount(Policy::Reserved, CAPACITY).expect("a reserved volume mounts");
		if fs.stream_begin(&name).is_err() || fs.stream_push(&data).is_err() {
			return;
		}
		committed = fs.stream_commit();
		intact = fs.reservation_intact();
		held = (fs.footprint(), fs.reserved_bytes());
		// The volume comes OUT of the block still holding what it holds. Reading the file inside
		// would allocate the copy against the same cap and fail for a reason that has nothing to do
		// with what is being tested.
		volume = Some(fs);
	});
	let readback = volume.as_mut().and_then(|fs| fs.read_file(&name).ok());
	assert_eq!(committed, Ok(()), "the commit lands");
	assert!(intact, "and the volume still holds the capacity it promised (footprint {}, reserved {})", held.0, held.1);
	assert_eq!(held.0 as usize + held.1 as usize, CAPACITY, "stored plus held is the capacity, as it always is");
	assert_eq!(readback.as_deref(), Some(data.as_slice()), "the file is there and whole");
}
