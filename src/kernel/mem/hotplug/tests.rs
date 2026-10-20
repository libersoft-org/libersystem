use super::{MAX_ADDRESS, MAX_BLOCKS, Refusal, check, give, is_free, offline, online, register, state, take, totals};

const MIB: u64 = 1 << 20;

crate::tagged_test!(a_plugged_range_is_whole_blocks_over_nothing_the_machine_has, [Memory, Kernel], id = "kernel.mem.hotplug.a_plugged_range_is_whole_blocks_over_nothing_the_machine_has", covers = ["kernel"]);
fn a_plugged_range_is_whole_blocks_over_nothing_the_machine_has() {
	let ram = [(0, 2048 * MIB), (0x1_0000_0000, 2048 * MIB)];
	assert_eq!(check(0x2_0000_0000, 512 * MIB, 2 * MIB, &ram), Ok(()), "whole 2 MiB blocks above the RAM");
	assert_eq!(check(0x2_0000_0000, 512 * MIB, 3 * MIB, &ram), Err(Refusal::Unaligned), "a block that is no power of two");
	assert_eq!(check(0x2_0000_0000, 512 * MIB, 2048, &ram), Err(Refusal::Unaligned), "nor under a page");
	assert_eq!(check(0x2_0010_0000, 512 * MIB, 2 * MIB, &ram), Err(Refusal::Unaligned), "a base inside a block");
	assert_eq!(check(0x2_0000_0000, 3 * MIB, 2 * MIB, &ram), Err(Refusal::Unaligned), "a size that is not whole blocks");
	assert_eq!(check(0x2_0000_0000, 0, 2 * MIB, &ram), Err(Refusal::Unaligned), "no size");
	// AT MOST `MAX_BLOCKS`, so the state call always reports the whole region: 64 GiB in 2 MiB blocks, and not a block
	// more.
	assert_eq!(check(0x20_0000_0000, MAX_BLOCKS * 2 * MIB, 2 * MIB, &ram), Ok(()), "exactly the most blocks");
	assert_eq!(check(0x20_0000_0000, (MAX_BLOCKS + 1) * 2 * MIB, 2 * MIB, &ram), Err(Refusal::TooMany), "one block more");
	assert_eq!(check(0x1_7000_0000, 512 * MIB, 2 * MIB, &ram), Err(Refusal::Overlap), "over the RAM above 4 GiB");
	assert_eq!(check(MAX_ADDRESS - 2 * MIB, 4 * MIB, 2 * MIB, &ram), Err(Refusal::TooHigh), "past the direct map's shared slot");
	assert_eq!(check(u64::MAX - 2 * MIB + 1, 4 * MIB, 2 * MIB, &ram), Err(Refusal::TooHigh), "or wrapping");
}

crate::tagged_test!(a_plugged_block_is_used_last_and_goes_offline_only_when_every_frame_is_free, [Memory, Kernel], id = "kernel.mem.hotplug.a_plugged_block_is_used_last_and_goes_offline_only_when_every_frame_is_free", covers = ["kernel"]);
// A RANGE NOTHING BACKS, high above the test machine's memory and every window: brought online, it is mapped and
// counted and never touched - the test takes its frames' addresses and gives them back, and reads nothing through them.
fn a_plugged_block_is_used_last_and_goes_offline_only_when_every_frame_is_free() {
	const BASE: u64 = 0x40_0000_0000;
	const BLOCK: u64 = 2 * MIB;
	// A PORT WHOSE DIRECT MAP THIS KERNEL DOES NOT EXTEND refuses every region, and that refusal is what is checked there.
	if !cfg!(target_arch = "x86_64") {
		assert_eq!(register(BASE, 4 * BLOCK, BLOCK, &[]), Err(Refusal::Unsupported), "a port with no plugged memory refuses every region");
		crate::serial_println!("hotplug: NOT RUN - plugged memory is x86_64's, and this port refused the region");
		return;
	}
	let before = totals();
	let index = register(BASE, 4 * BLOCK, BLOCK, &[]).expect("a range nothing has");
	assert_eq!(register(BASE, 4 * BLOCK, BLOCK, &[]), Ok(index), "the same range asked for again is the same region");
	assert_eq!(register(BASE + BLOCK, 4 * BLOCK, BLOCK, &[]), Err(Refusal::Overlap), "a range over a region is refused");
	let mut blocks = [true; 4];
	assert_eq!(state(index, &mut blocks), Some(4));
	assert_eq!(blocks, [false; 4], "nothing online until it is brought online");
	assert_eq!(online(index, 0, 2), Ok(()));
	assert_eq!(online(index, 1, 1), Err(Refusal::State), "a block already online");
	assert_eq!(online(index, 3, 2), Err(Refusal::Range), "a range past the region");
	assert_eq!(totals(), (before.0 + 1024, before.1 + 1024), "two blocks' frames online and free");
	assert_eq!(is_free(BASE), Some(true));
	assert_eq!(crate::mem::frame::is_free(BASE), true, "and the allocator answers for them");
	assert!(crate::mem::frame::totals().0 >= 1024, "counted in the machine's total");
	// AND KEPT OUT OF THE BOOT POOLS' OWN COUNT, which is what a balloon's floor is judged against: its frames come from
	// the boot pools alone, so free plugged frames must not keep its room open while they drain.
	assert_eq!(crate::mem::frame::totals().0 - crate::mem::frame::boot_totals().0, totals().0, "the boot pools' count leaves plugged memory out");
	// A HELD FRAME KEEPS ITS BLOCK.
	let held = take().expect("a free frame of an online block");
	assert!((BASE..BASE + 2 * BLOCK).contains(&held));
	assert_eq!(is_free(held), Some(false));
	let held_block = ((held - BASE) / BLOCK) as usize;
	assert_eq!(offline(index, held_block, 1), Err(Refusal::Busy), "a block with a frame in use is not taken back");
	assert_eq!(totals().0, before.0 + 1024, "and nothing changed");
	assert!(give(held), "a region's frame goes back to its region");
	assert_eq!(offline(index, 0, 2), Ok(()), "every frame free, both blocks go");
	assert_eq!(totals(), before, "and the counts are as they were");
	assert_eq!(offline(index, 0, 1), Err(Refusal::State), "a block not online");
	assert!(give(BASE), "a frame of an offline block is the region's to refuse");
	assert_eq!(is_free(BASE), Some(false), "and refused: it is not free");
	assert_eq!(state(index, &mut blocks), Some(4));
	assert_eq!(blocks, [false; 4]);
}

crate::tagged_test!(a_balloon_stops_at_the_boot_pools_floor_while_plugged_memory_is_free, [Memory, Kernel], id = "kernel.mem.hotplug.a_balloon_stops_at_the_boot_pools_floor_while_plugged_memory_is_free", covers = ["kernel"]);
// A BALLOON'S FRAMES COME FROM THE BOOT POOLS, so their floor is what stops it - and free plugged memory, which a balloon
// never takes, must not keep its room open while they drain. Judged against every frame the machine has, sixteen
// megabytes of free plugged memory let a host's target take the boot pools that far below the floor the kernel's heap,
// page tables, stacks and DMA buffers are kept by. Driven to the end here: a plugged range online and free, and the
// balloon asked for frames until it refuses.
fn a_balloon_stops_at_the_boot_pools_floor_while_plugged_memory_is_free() {
	use crate::mem::frame::{balloon_floor, balloon_return, balloon_take, boot_totals};
	const BASE: u64 = 0x50_0000_0000;
	const BLOCK: u64 = 2 * MIB;
	const BLOCKS: usize = 8;
	if !cfg!(target_arch = "x86_64") {
		crate::serial_println!("hotplug: NOT RUN - plugged memory is x86_64's");
		return;
	}
	let index = register(BASE, BLOCKS as u64 * BLOCK, BLOCK, &[]).expect("a range nothing has");
	assert_eq!(online(index, 0, BLOCKS), Ok(()));
	let plugged = totals();
	let (total, _) = boot_totals();
	let floor = balloon_floor(total);
	// THE FRAMES TAKEN ARE KEPT IN A LIST THROUGH THEMSELVES: each holds the address of the one taken before it. A list
	// on the heap would take what is being measured.
	let hhdm = crate::mem::hhdm_offset();
	let mut last: u64 = 0;
	let mut taken = 0usize;
	while taken < total {
		let Some(frame) = balloon_take() else { break };
		// SAFETY: a free frame the balloon took for this test; nothing else holds or maps it but the direct map.
		unsafe { ((hhdm + frame) as *mut u64).write_volatile(last) };
		last = frame;
		taken += 1;
	}
	let (_, free) = boot_totals();
	let stopped = free;
	let still = totals();
	// GIVEN BACK BEFORE ANYTHING IS ASSERTED, so a failure leaves the machine as it found it.
	while last != 0 {
		// SAFETY: written above; every frame in the list is one `balloon_take` handed this test, returned once.
		let before = unsafe { ((hhdm + last) as *const u64).read_volatile() };
		unsafe { balloon_return(last) };
		last = before;
	}
	assert_eq!(offline(index, 0, BLOCKS), Ok(()), "the plugged range goes back offline");
	assert!(taken > 0, "the balloon took frames before it stopped");
	// AT THE FLOOR, give or take what the rest of the kernel allocated or freed while the test drained the pools.
	assert!(stopped + 256 >= floor, "the balloon stopped at the boot pools' floor of {floor} frames, not below it: {stopped} left free after {taken} taken, with {} plugged frames free", plugged.1);
	assert!(stopped <= floor + 256, "and not short of it: {stopped} left free against a floor of {floor}");
	assert_eq!(still, plugged, "and it took none of the plugged frames, which were all still free");
	crate::serial_println!("  the balloon took {taken} frame(s) and stopped with {stopped} free against the boot pools' floor of {floor}, beside {} free plugged frame(s)", plugged.1);
}
