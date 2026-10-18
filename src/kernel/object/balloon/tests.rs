use super::{Balloon, GiveRefusal};
use crate::mem::frame;

crate::tagged_test!(a_balloon_holds_only_free_frames_above_the_floor_and_gives_back_only_its_own, [Memory, Kernel], id = "kernel.object.balloon.a_balloon_holds_only_free_frames_above_the_floor_and_gives_back_only_its_own", covers = ["kernel"]);
// WHAT A HOST IS GIVEN IS MEMORY NOBODY USES, and what comes back is exactly what went: frames the allocator had free,
// none twice, none below the floor, every one of them returned by a give or by the balloon's end.
fn a_balloon_holds_only_free_frames_above_the_floor_and_gives_back_only_its_own() {
	// THE FLOOR: a sixteenth of the machine, never under 32 MiB, and nothing taken at or below it.
	assert_eq!(frame::balloon_floor(1 << 20), 1 << 16, "a sixteenth of a 4 GiB machine");
	assert_eq!(frame::balloon_floor(10_000), 8192, "and never fewer than 32 MiB of frames");
	assert_eq!(frame::balloon_room(10_000, 10_000, 256), 256);
	assert_eq!(frame::balloon_room(8_300, 10_000, 256), 108, "only the frames above the floor");
	assert_eq!(frame::balloon_room(8_192, 10_000, 256), 0, "and none at it");
	let before = frame::ballooned();
	let balloon = Balloon::new().expect("a balloon");
	let mut taken = [0u64; 256];
	assert_eq!(balloon.take(&mut taken), 256, "a machine this size has 256 frames above its floor");
	assert_eq!(frame::ballooned(), before + 256, "counted as the balloon's");
	assert_eq!(balloon.held(), 256);
	let mut sorted = taken.to_vec();
	sorted.sort_unstable();
	sorted.dedup();
	assert_eq!(sorted.len(), 256, "no frame twice");
	assert!(taken.iter().all(|phys| !frame::is_free(*phys)), "and none of them free while the balloon holds it");
	// A GIVE IS ALL OR NOTHING: a frame it does not hold, or one named twice, returns nothing.
	let stranger = frame::allocate().expect("a frame that is not the balloon's");
	assert_eq!(balloon.give(&[taken[0], stranger]), Err(GiveRefusal::NotHeld), "a frame that is somebody else's");
	assert_eq!(balloon.give(&[taken[1], taken[1]]), Err(GiveRefusal::NotHeld), "a frame named twice");
	assert_eq!(balloon.held(), 256, "and neither request gave anything back");
	assert!(!frame::is_free(stranger), "nor touched the stranger");
	// SAFETY: allocated above, never mapped, freed once.
	unsafe { frame::deallocate(stranger) };
	assert_eq!(balloon.give(&taken[..128]), Ok(128));
	assert!(taken[..128].iter().all(|phys| frame::is_free(*phys)), "a given frame is free again");
	assert_eq!((balloon.held(), frame::ballooned()), (128, before + 128));
	// THE END RETURNS THE REST.
	drop(balloon);
	assert_eq!(frame::ballooned(), before, "every frame went back with the balloon");
	assert!(taken[128..].iter().all(|phys| frame::is_free(*phys)), "and is free");
}
