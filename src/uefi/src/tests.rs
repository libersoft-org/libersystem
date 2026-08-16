//! The firmware cases QEMU will not produce.
//!
//! Every one of these is a behaviour the UEFI specification permits, real firmware exhibits, and
//! OVMF does not - which is why the loader's answers to them were argued in comments and never run.
//! The mock beside this file is a set of function pointers; the code under test is the code that
//! ships.

use alloc::vec;
use alloc::vec::Vec;

use crate::mock::{self, add_disk, descriptor, guard, state};
use crate::{acpi, disk, file, gop, memory};

// A device whose driver demands 4096-byte buffer alignment.
//
// `EFI_BLOCK_IO_MEDIA` carries `IoAlign` and the specification makes it a REQUIREMENT on the
// caller's buffer. The loader hands `read_blocks` whatever address the filesystem's block buffer
// happens to have, which for a `Vec` is 8-byte aligned - so on NVMe, SCSI and USB stacks that
// enforce it, every read of the system volume would be refused. OVMF does not enforce it, so this
// has never been visible from a boot.
#[test]
fn a_driver_that_demands_alignment_is_given_an_aligned_buffer() {
	use fscore::BlockDevice;
	let _guard = guard();
	// 512-byte device blocks, a whole disk of recognisable bytes.
	let contents: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
	add_disk(512, 4096, contents.clone());

	let mut found: Option<disk::FirmwareDisk> = None;
	disk::each_disk(mock::boot_services(), |d| {
		found = Some(d);
		true
	});
	let mut device = found.expect("the enumeration finds the disk");

	// A buffer deliberately off the alignment the driver wants: one byte into a larger allocation,
	// which is what a filesystem's block buffer looks like from the driver's point of view.
	let mut backing = vec![0u8; 2048 + 1];
	let buf = &mut backing[1..1 + 1024];
	assert!((buf.as_ptr() as usize) % 4096 != 0, "the test's own buffer must be misaligned or it proves nothing");
	assert!(device.read_block(0, buf), "the read succeeds through the bounce buffer");
	assert_eq!(&buf[..16], &contents[..16], "and the bytes are the disk's");

	let st = state();
	let disk = st.disks.first().expect("the disk is still there");
	assert!(!disk.misaligned, "the driver was never handed a buffer it would have refused");
	let (_, _, addr) = *disk.reads.first().expect("the driver was asked for the blocks");
	assert_eq!(addr % 4096, 0, "the address the driver saw satisfies IoAlign");
}

// The same device, asked for more than the bounce buffer holds.
//
// The bounce is a fixed 8 KiB stack array. A filesystem block larger than it cannot be lined up, and
// the honest answer is a refusal - reading a PART of it and reporting success is what this code
// refuses to do everywhere else.
#[test]
fn a_read_too_large_for_the_bounce_buffer_is_refused_rather_than_shortened() {
	use fscore::BlockDevice;
	let _guard = guard();
	add_disk(512, 4096, vec![7u8; 64 * 1024]);
	let mut found: Option<disk::FirmwareDisk> = None;
	disk::each_disk(mock::boot_services(), |d| {
		found = Some(d);
		true
	});
	let mut device = found.expect("the enumeration finds the disk");

	let mut backing = vec![0u8; 32 * 1024 + 1];
	let buf = &mut backing[1..1 + 16 * 1024];
	assert!(!device.read_block(0, buf), "a block larger than the bounce buffer is refused");
	assert!(state().disks[0].reads.is_empty(), "and the driver was not asked at all");
}

// A disk whose driver has no alignment requirement is read in place, with no copy.
#[test]
fn a_driver_with_no_alignment_requirement_reads_in_place() {
	use fscore::BlockDevice;
	let _guard = guard();
	let contents: Vec<u8> = (0..4096u32).map(|i| (i % 253) as u8).collect();
	add_disk(512, 0, contents.clone());
	let mut found: Option<disk::FirmwareDisk> = None;
	disk::each_disk(mock::boot_services(), |d| {
		found = Some(d);
		true
	});
	let mut device = found.expect("the enumeration finds the disk");

	let mut buf = vec![0u8; 1024];
	let expected = buf.as_ptr() as usize;
	assert!(device.read_block(1, &mut buf), "the read succeeds");
	assert_eq!(&buf[..], &contents[1024..2048], "at the right offset");
	let (lba, len, addr) = state().disks[0].reads[0];
	assert_eq!((lba, len), (2, 1024), "one device-block-indexed read of the whole filesystem block");
	assert_eq!(addr, expected, "straight into the caller's buffer");
}

// Two system volumes, presented in either order.
//
// The firmware decides which handle comes first, and nothing about that order is stable: it depends
// on enumeration, on which controller answered first, on the machine. A loader that takes the first
// match therefore boots a different disk on the same hardware from one power cycle to the next. The
// enumeration itself must at least be ORDER-FAITHFUL - it must report what the firmware reported, in
// the firmware's order, so the selection above it is choosing rather than guessing.
#[test]
fn the_enumeration_reports_every_disk_in_the_firmware_order() {
	let _guard = guard();
	add_disk(512, 0, vec![0xaa; 4096]);
	add_disk(4096, 0, vec![0xbb; 8192]);
	add_disk(512, 0, vec![0xcc; 2048]);

	let mut seen: Vec<(u32, u64)> = Vec::new();
	disk::each_disk(mock::boot_services(), |d| {
		seen.push((d.block_size(), d.last_block()));
		false
	});
	assert_eq!(seen, vec![(512, 7), (4096, 1), (512, 3)], "every disk, in the order the firmware gave them");

	// And a visitor that stops gets exactly what it asked for: the loader's volume search stops at
	// the first volume it recognises, which is only correct if "first" means the firmware's first.
	let mut count = 0usize;
	disk::each_disk(mock::boot_services(), |_| {
		count += 1;
		count == 2
	});
	assert_eq!(count, 2, "the walk stops when the visitor says so");
}

// A volume's identity, read the way the loader reads it: mount, ask, keep. Here the identity is the
// first sixteen bytes of the disk, which stands in for the superblock UUID a LiberFS mount answers
// with - the pairing rule is what is under test, not LiberFS.
fn read_uuid(mut device: disk::FirmwareDisk) -> Option<([u8; 16], [u8; 16])> {
	let mut header = vec![0u8; 512];
	if !fscore::BlockDevice::read_block(&mut device, 0, &mut header) {
		return None;
	}
	if header[..16] == [0u8; 16] {
		// Nothing mounted here: not a volume of ours.
		return None;
	}
	let mut uuid = [0u8; 16];
	uuid.copy_from_slice(&header[..16]);
	Some((uuid, uuid))
}

// TWO LIBERSYSTEM VOLUMES IN ONE MACHINE, in either handle order.
//
// This is the finding the loader's milestone is named for. The boot medium names the volume it
// belongs to (`etc/system-volume.uuid`), the volume carries that identity in its superblock, and the
// loader is supposed to pair them - so that two LiberSystem disks in one machine do not both match
// and let the firmware's handle order decide which system boots. The mechanism was finished and had
// never been run: it lives in a UEFI binary.
//
// Both orders, because "it worked" on a machine where the wanted volume happened to be first is
// exactly the evidence this case exists to refuse.
#[test]
fn the_paired_volume_is_chosen_whichever_order_the_firmware_reports_it_in() {
	let _guard = guard();
	// Two disks, each carrying its identity in its first sixteen bytes - which stands in for the
	// superblock UUID a real LiberFS mount reads.
	let mut ours = vec![0u8; 4096];
	ours[..16].copy_from_slice(&[0xd4; 16]);
	let mut theirs = vec![0u8; 4096];
	theirs[..16].copy_from_slice(&[0x77; 16]);

	for (first, second, label) in [(ours.clone(), theirs.clone(), "ours first"), (theirs.clone(), ours.clone(), "theirs first")] {
		// A fresh set of disks per order, so neither run inherits the other's.
		state().disks.clear();
		add_disk(512, 0, first);
		add_disk(512, 0, second);
		let chosen = disk::choose_volume(mock::boot_services(), Some([0xd4; 16]), read_uuid);
		assert_eq!(chosen, Some([0xd4; 16]), "{label}: the paired volume is the one chosen");
	}

	// With no pairing there is nothing to choose by, and the first volume wins - which is the
	// single-disk case and the reason the loader says so on the serial port when the medium names
	// no volume.
	state().disks.clear();
	add_disk(512, 0, theirs.clone());
	add_disk(512, 0, ours.clone());
	let chosen = disk::choose_volume(mock::boot_services(), None, read_uuid);
	assert_eq!(chosen, Some([0x77; 16]), "with no pairing the first volume that opens wins");

	// And a pairing that names a volume this machine does not have finds NOTHING rather than
	// falling back to somebody else's system.
	let chosen = disk::choose_volume(mock::boot_services(), Some([0x5e; 16]), read_uuid);
	assert_eq!(chosen, None, "a volume that is not here is not substituted for");
}

// Two FAT volumes, and a disk that is not a volume at all.
//
// The same rule from the other side: `open` returning None is "this disk is not one of ours", and it
// must not stop the walk. A machine with a Windows ESP, a data partition and the system volume
// presents exactly this, and stopping at the first disk that fails to mount would find nothing.
#[test]
fn a_disk_that_does_not_open_does_not_end_the_search() {
	let _guard = guard();
	add_disk(512, 0, vec![0x00; 2048]); // not a volume
	add_disk(512, 0, vec![0x00; 2048]); // nor this one
	let mut wanted = vec![0u8; 2048];
	wanted[..16].copy_from_slice(&[0x9c; 16]);
	add_disk(512, 0, wanted);

	let mut opened = 0usize;
	let chosen = disk::choose_volume(mock::boot_services(), Some([0x9c; 16]), |device| {
		opened += 1;
		read_uuid(device)
	});
	assert_eq!(chosen, Some([0x9c; 16]), "the third disk is found");
	assert_eq!(opened, 3, "and every disk before it was tried");
}

// What a sidecar has to say to name a volume.
//
// The other half of the pairing, and the half a damaged or truncated file reaches first. A partly
// read identity that still parsed would pair the medium against a volume it does not belong to,
// which is worse than not pairing at all: the loader says so on the serial port and takes the first
// volume, which on a one-disk machine is right.
#[test]
fn a_sidecar_names_a_volume_only_when_it_names_a_whole_one() {
	assert_eq!(disk::parse_pairing(b"d491e82ea5b5a58744c9e0cf1bf4a03f"), Some([0xd4, 0x91, 0xe8, 0x2e, 0xa5, 0xb5, 0xa5, 0x87, 0x44, 0xc9, 0xe0, 0xcf, 0x1b, 0xf4, 0xa0, 0x3f]));
	// Written the way UUIDs are written, and with the trailing newline a file has.
	assert_eq!(disk::parse_pairing(b"d491e82e-a5b5-a587-44c9-e0cf1bf4a03f\n"), disk::parse_pairing(b"d491e82ea5b5a58744c9e0cf1bf4a03f"), "dashes and whitespace are punctuation");
	assert_eq!(disk::parse_pairing(b"D491E82EA5B5A58744C9E0CF1BF4A03F"), disk::parse_pairing(b"d491e82ea5b5a58744c9e0cf1bf4a03f"), "and case is not part of the identity");

	assert_eq!(disk::parse_pairing(b""), None, "an empty sidecar names nothing");
	assert_eq!(disk::parse_pairing(b"d491e82ea5b5a587"), None, "half an identity is not an identity");
	assert_eq!(disk::parse_pairing(b"d491e82ea5b5a58744c9e0cf1bf4a03f00"), None, "and neither is one with more digits than a UUID has");
	assert_eq!(disk::parse_pairing(b"d491e82ea5b5a58744c9e0cf1bf4a03g"), None, "a digit that is not hex is not read as zero");
	assert_eq!(disk::parse_pairing(b"libersystem-vol\0"), None, "the string literal this mechanism used to pair on is not a UUID either");
}

// A drive with no medium is skipped, and a drive with a zero block size is not divided by.
#[test]
fn a_drive_with_no_medium_is_not_offered() {
	let _guard = guard();
	add_disk(512, 0, vec![0x11; 4096]);
	add_disk(512, 0, vec![0x22; 4096]);
	state().disks[0].media.media_present = false;
	state().disks[1].media.block_size = 0;

	let mut seen = 0usize;
	disk::each_disk(mock::boot_services(), |_| {
		seen += 1;
		false
	});
	assert_eq!(seen, 0, "an empty bay and a nonsense geometry are both skipped");
}

// Six hundred memory descriptors.
//
// `MAX_REGIONS` is 512 and the boot protocol carries no more. A firmware with a more fragmented map
// than that is not this loader's fault, and handing the kernel the first 512 regions is: it would
// look complete and be missing its tail, which is the worst available failure mode for the one
// structure that says which RAM exists.
#[test]
fn a_memory_map_larger_than_the_boot_protocol_carries_is_refused() {
	let _guard = guard();
	let mut descriptors: Vec<crate::MemoryDescriptor> = Vec::new();
	// Six hundred, each its own region with a gap after it so nothing coalesces.
	for i in 0..600u64 {
		descriptors.push(descriptor(crate::CONVENTIONAL_MEMORY, i * 0x4000, 1));
	}
	state().descriptors = descriptors;

	let (buf, pages, map_size, desc_size) = memory::memory_map_snapshot(mock::boot_services()).expect("the map is taken");
	let mut regions = vec![bootproto::MemRegion { base: 0, length: 0, kind: 0, _pad: 0 }; memory::MAX_REGIONS];
	assert!(memory::translate_map(buf, map_size, desc_size, regions.as_mut_ptr()).is_none(), "a map with more regions than the protocol carries is refused, not truncated");

	// And a map that fits is translated whole.
	state().descriptors = (0..500u64).map(|i| descriptor(crate::CONVENTIONAL_MEMORY, i * 0x4000, 1)).collect();
	let (buf, _, map_size, desc_size) = memory::memory_map_snapshot(mock::boot_services()).expect("the smaller map is taken");
	assert_eq!(memory::translate_map(buf, map_size, desc_size, regions.as_mut_ptr()), Some(500), "every region survives");
	unsafe { ((*mock::boot_services()).free_pages)(buf as u64, pages) };
}

// The map is sorted and coalesced, and a firmware that reports it out of order is not trusted to
// have reported it in order.
#[test]
fn the_translated_map_is_sorted_and_coalesced() {
	let _guard = guard();
	state().descriptors = vec![
		descriptor(crate::CONVENTIONAL_MEMORY, 0x3000, 1),
		descriptor(crate::CONVENTIONAL_MEMORY, 0x1000, 1),
		descriptor(crate::CONVENTIONAL_MEMORY, 0x2000, 1),
		descriptor(crate::ACPI_RECLAIM_MEMORY, 0x4000, 1),
	];
	let (buf, pages, map_size, desc_size) = memory::memory_map_snapshot(mock::boot_services()).expect("the map is taken");
	let mut regions = vec![bootproto::MemRegion { base: 0, length: 0, kind: 0, _pad: 0 }; memory::MAX_REGIONS];
	let n = memory::translate_map(buf, map_size, desc_size, regions.as_mut_ptr()).expect("the map translates");
	assert_eq!(n, 2, "three adjacent usable pages become one region, and the ACPI page stays its own");
	assert_eq!((regions[0].base, regions[0].length, regions[0].kind), (0x1000, 0x3000, bootproto::MEM_USABLE));
	assert_eq!((regions[1].base, regions[1].kind), (0x4000, bootproto::MEM_ACPI_RECLAIMABLE));
	unsafe { ((*mock::boot_services()).free_pages)(buf as u64, pages) };
}

// A FIRMWARE THAT KEEPS HANDING BACK THE ADDRESS THE KERNEL HAS TO GO TO.
//
// This is the riscv64 case, made ordinary. On QEMU `virt` the kernel is linked at 0x8020_0000, which
// is where U-Boot itself runs, and its `AllocateAddress` there SUCCEEDS because that U-Boot does not
// reserve its own image - so the loader overwrote the firmware it was still calling into, and the
// next firmware call ran into the kernel. The answer is to stage the kernel elsewhere and copy it
// into place after the last firmware call, which only works if "elsewhere" is actually elsewhere.
//
// A guest test cannot ask a firmware to place allocations adversarially. A mock can.
#[test]
fn staging_memory_is_taken_clear_of_the_kernels_destination() {
	let _guard = guard();
	const DEST_LOW: u64 = 0x8020_0000;
	const DEST_HIGH: u64 = 0x8030_0000;
	let pages = 16usize;
	let span = pages as u64 * 4096;

	// The firmware offers, in order: the middle of the destination, its first page, a block ending
	// one byte inside it, and finally somewhere clear.
	state().forced_pages = vec![DEST_LOW + 0x8000, DEST_LOW, DEST_LOW - span + 4096, 0x9000_0000];
	let scratch = memory::staging_clear_of(mock::boot_services(), pages, DEST_LOW, DEST_HIGH).expect("a clear block is found");
	assert_eq!(scratch, 0x9000_0000, "the first block that does not overlap the destination");
	assert!(state().forced_pages.is_empty(), "and every offer before it was asked for and rejected");

	// Immediately BELOW the destination is clear - the bound is the span's end, not its base.
	state().forced_pages = vec![DEST_LOW - span];
	assert_eq!(memory::staging_clear_of(mock::boot_services(), pages, DEST_LOW, DEST_HIGH), Some(DEST_LOW - span), "a block that ends exactly where the destination starts is clear of it");

	// And immediately ABOVE is clear too.
	state().forced_pages = vec![DEST_HIGH];
	assert_eq!(memory::staging_clear_of(mock::boot_services(), pages, DEST_LOW, DEST_HIGH), Some(DEST_HIGH), "so is one that starts exactly where it ends");
}

// A firmware with nothing but the destination to offer is refused rather than forced.
#[test]
fn staging_gives_up_rather_than_placing_the_kernel_on_top_of_itself() {
	let _guard = guard();
	const DEST_LOW: u64 = 0x8020_0000;
	const DEST_HIGH: u64 = 0x8030_0000;
	// Seventeen offers inside the destination against sixteen reject slots.
	state().forced_pages = (0..17u64).map(|i| DEST_LOW + i * 4096).collect();
	assert_eq!(memory::staging_clear_of(mock::boot_services(), 4, DEST_LOW, DEST_HIGH), None, "a machine whose free memory is exactly where the kernel goes is a refusal");
	// The rejects are deliberately NOT given back: freeing one invites the next request to return
	// the same block, and they are firmware pages that `ExitBootServices` reclaims anyway.
	assert!(state().frees.is_empty(), "nothing was freed on the way out");
}

// A stale `MapKey` is the one status worth retrying.
//
// `ExitBootServices` answers `EFI_INVALID_PARAMETER` when the map moved between the caller's
// `GetMemoryMap` and the exit - an ordinary event, because allocating the buffer to read the map
// into can itself change the map. Every other refusal means something different is wrong, and the
// loader's loop used to retry on all of them forever: a machine that hangs at the hand-off with
// nothing said.
#[test]
fn only_a_stale_map_key_is_worth_retrying() {
	let _guard = guard();
	assert!(memory::exit_retryable(crate::STATUS_INVALID_PARAMETER), "a stale map key is retryable");
	assert!(!memory::exit_retryable(crate::STATUS_NOT_FOUND), "and nothing else is");
	assert!(!memory::exit_retryable(crate::STATUS_BUFFER_TOO_SMALL), "including a status that looks like a sizing problem");
	assert!(!memory::exit_retryable(crate::STATUS_SUCCESS), "success is not a refusal at all");
}

// A firmware whose map moves under the caller: the key it reports is stale by the time it is used,
// and the retry loop must converge rather than spin.
#[test]
fn a_map_that_moves_between_the_two_calls_is_retried_and_converges() {
	let _guard = guard();
	state().descriptors = (0..8u64).map(|i| descriptor(crate::CONVENTIONAL_MEMORY, i * 0x1000, 1)).collect();
	// The first two exits are refused with the stale-key status; the third succeeds.
	state().exit_status = crate::STATUS_INVALID_PARAMETER;
	state().exit_refusals = 2;

	let bs = mock::boot_services();
	let mut attempts = 0usize;
	loop {
		attempts += 1;
		let mut size = 0usize;
		let mut key = 0usize;
		let mut desc_size = 0usize;
		let mut desc_ver = 0u32;
		let status = unsafe { ((*bs).get_memory_map)(&mut size, core::ptr::null_mut(), &mut key, &mut desc_size, &mut desc_ver) };
		assert_eq!(status, crate::STATUS_BUFFER_TOO_SMALL, "the sizing call reports the size");
		let mut buf = vec![0u8; size];
		let status = unsafe { ((*bs).get_memory_map)(&mut size, buf.as_mut_ptr() as *mut crate::MemoryDescriptor, &mut key, &mut desc_size, &mut desc_ver) };
		assert!(!crate::is_error(status), "the map is readable");
		let status = unsafe { ((*bs).exit_boot_services)(core::ptr::null_mut(), key) };
		if !crate::is_error(status) {
			break;
		}
		assert!(memory::exit_retryable(status), "the loop only goes round for a stale key");
		assert!(attempts < 8, "and it converges");
	}
	assert_eq!(attempts, 3, "two refusals, then the exit");
	assert_eq!(state().exit_attempts, 3, "and the firmware saw exactly those calls");
}

// A `FileProtocol` that hands back fewer bytes than the file has.
//
// This returned a slice of the DECLARED length however much had arrived, so a file whose `FileInfo`
// said one megabyte and whose second read failed became a one-megabyte slice with a tail of whatever
// those freshly allocated pages held - handed on as a kernel image, a bootstrap package or a volume.
// A short read is not a file.
#[test]
fn a_file_that_short_reads_is_not_a_file() {
	let _guard = guard();
	let bytes: Vec<u8> = (0..4096u32).map(|i| (i % 241) as u8).collect();
	state().file_bytes = bytes.clone();
	state().file_declared_size = bytes.len() as u64;
	// Handed back 512 bytes at a time, and the fourth read fails: 1536 bytes of a 4096-byte file.
	state().file_read_chunk = 512;
	state().file_reads_before_failure = 3;

	let answer = file::read_file(mock::boot_services(), mock::file_protocol(), "kernel");
	assert!(answer.is_none(), "a file that stopped arriving is not returned");
	// And the pages went back rather than being left holding a partial image.
	assert!(!state().frees.is_empty(), "the allocation was released");
	assert!(state().allocations.is_empty(), "and nothing was left allocated");
}

// The same file, arriving in pieces, IS a file: a firmware that answers a read with fewer bytes
// than asked for and keeps going is ordinary, and the loop is what makes it work.
#[test]
fn a_file_that_arrives_in_pieces_is_read_whole() {
	let _guard = guard();
	let bytes: Vec<u8> = (0..4096u32).map(|i| (i % 241) as u8).collect();
	state().file_bytes = bytes.clone();
	state().file_declared_size = bytes.len() as u64;
	state().file_read_chunk = 512;

	let answer = file::read_file(mock::boot_services(), mock::file_protocol(), "kernel").expect("the file is read");
	assert_eq!(answer.len(), bytes.len(), "the whole declared length");
	assert_eq!(answer, &bytes[..], "and every byte of it");
	assert_eq!(state().file_reads, 8, "in eight reads of 512 bytes");
}

// A name that does not fit is not this name.
//
// `to_utf16` used to widen BYTES - so a name with any non-ASCII character named something else - and
// to stop when full, so a path longer than the buffer opened a DIFFERENT, SHORTER path. Opening the
// wrong file is worse than opening none.
#[test]
fn a_name_that_does_not_fit_opens_nothing() {
	let _guard = guard();
	let mut out = [0u16; 8];
	assert!(file::to_utf16("kernel", &mut out), "a name that fits is encoded");
	assert_eq!(&out[..7], &[b'k' as u16, b'e' as u16, b'r' as u16, b'n' as u16, b'e' as u16, b'l' as u16, 0], "NUL-terminated");
	assert!(!file::to_utf16("a-name-far-too-long-for-this", &mut out), "one that does not fit is refused");

	// And a non-ASCII name is encoded as CHARACTERS, not as its UTF-8 bytes.
	let mut wide = [0u16; 8];
	assert!(file::to_utf16("jádro", &mut wide), "a non-ASCII name is encoded");
	assert_eq!(&wide[..6], &[b'j' as u16, 0x00e1, b'd' as u16, b'r' as u16, b'o' as u16, 0], "five characters, not six bytes");

	// The whole path through `read_file`: a name that does not fit never reaches the firmware.
	state().file_bytes = vec![0u8; 16];
	state().file_declared_size = 16;
	let long = "a-file-name-longer-than-the-sixty-four-units-the-encoder-is-given-which-is-quite-a-lot-of-characters";
	assert!(file::read_file(mock::boot_services(), mock::file_protocol(), long).is_none(), "refused before opening");
	assert!(state().file_opened.is_empty(), "and the firmware was never asked to open anything");
}

// A 16-bit `PixelBitMask` mode.
//
// The specification permits any channel masks; `PixelBitMask` is where firmware puts a format that
// is neither of the two named layouts. A 16-bit mode - 5:6:5, which is what a great many embedded
// and virtualised GPUs report - has channels narrower than a byte, and the shift/size decoding is
// what tells the renderer where they are.
#[test]
fn a_pixel_bitmask_mode_is_decoded_by_its_masks() {
	let _guard = guard();
	// 5:6:5 in a 16-bit pixel.
	assert_eq!((gop::mask_shift(0xf800), gop::mask_size(0xf800)), (11, 5), "red");
	assert_eq!((gop::mask_shift(0x07e0), gop::mask_size(0x07e0)), (5, 6), "green");
	assert_eq!((gop::mask_shift(0x001f), gop::mask_size(0x001f)), (0, 5), "blue");
	// And the ordinary 32-bit layouts.
	assert_eq!((gop::mask_shift(0x00ff_0000), gop::mask_size(0x00ff_0000)), (16, 8), "8-bit red high in a dword");
	assert_eq!((gop::mask_shift(0), gop::mask_size(0)), (0, 0), "an absent channel says so rather than reading as bit 0");

	state().gop = Some(mock::GopConfig { width: 640, height: 480, stride: 640, format: crate::PIXEL_BIT_MASK, mask: crate::PixelBitmask { red: 0xf800, green: 0x07e0, blue: 0x001f, reserved: 0 }, base: 0x8000_0000, size: 640 * 480 * 2 });
	let fb = gop::locate_framebuffer(mock::boot_services());
	assert!(fb.present, "the framebuffer is reported");
	assert_eq!((fb.width, fb.height), (640, 480));
	assert_eq!((fb.red_shift, fb.red_size), (11, 5), "the mask decides where red is, not the format name");
	assert_eq!((fb.green_shift, fb.green_size), (5, 6));
	assert_eq!((fb.blue_shift, fb.blue_size), (0, 5));
}

// The two NAMED formats do not carry masks, and reading them as though they did would decode every
// channel as absent.
#[test]
fn the_named_pixel_formats_are_decoded_by_their_names() {
	let _guard = guard();
	for (format, expected) in [(crate::PIXEL_RGB, (0u8, 16u8)), (crate::PIXEL_BGR, (16u8, 0u8))] {
		state().gop = Some(mock::GopConfig {
			width: 1024,
			height: 768,
			stride: 1024,
			format,
			// Zero masks, which is what firmware reports for a named format - and what would decode
			// as "no channels" if the name were ignored.
			mask: crate::PixelBitmask { red: 0, green: 0, blue: 0, reserved: 0 },
			base: 0x9000_0000,
			size: 1024 * 768 * 4,
		});
		let fb = gop::locate_framebuffer(mock::boot_services());
		assert!(fb.present, "the framebuffer is reported");
		assert_eq!((fb.red_shift, fb.blue_shift), expected, "the named format places its channels");
		assert_eq!(fb.red_size, 8, "eight bits each");
		assert_eq!(fb.bpp, 32, "and a 32-bit pixel");
	}
}

// A firmware with no GOP at all is a headless machine, not a failure.
#[test]
fn no_graphics_output_is_a_headless_machine() {
	let _guard = guard();
	let fb = gop::locate_framebuffer(mock::boot_services());
	assert!(!fb.present, "no framebuffer is reported");
	assert_eq!(fb.phys, 0, "and nothing is described");
}

// `BltOnly` is a mode with no linear framebuffer: there is no memory to hand the kernel, and
// pretending otherwise would hand it a base address of zero and a size to write into.
#[test]
fn a_blt_only_mode_has_no_framebuffer_to_hand_over() {
	let _guard = guard();
	state().gop = Some(mock::GopConfig { width: 800, height: 600, stride: 800, format: crate::PIXEL_BLT_ONLY, mask: crate::PixelBitmask { red: 0, green: 0, blue: 0, reserved: 0 }, base: 0, size: 0 });
	let fb = gop::locate_framebuffer(mock::boot_services());
	assert!(!fb.present, "a blt-only mode is not a linear framebuffer");
}

// ---------------------------------------------------------------------------------------------
// SPCR: where a machine that has no device tree says its console is.
// ---------------------------------------------------------------------------------------------

// ACPI's older pointers are THIRTY-TWO BITS WIDE, and a host's heap address is not - so these
// tests cannot hand the reader raw `&[u8]` addresses the way the device-tree tests do. They publish
// each blob at a made-up physical address low enough to fit in a `u32` and translate on the way in,
// which is exactly what the loader's identity map and the kernel's direct map each do.
//
// It also stops a test from passing for the wrong reason: an RSDT test that quietly truncated its
// own pointer would read a wild address and crash, which is what the first version of this did.
static REGIONS: std::sync::Mutex<Vec<(u64, u64, u64)>> = std::sync::Mutex::new(Vec::new());

fn publish_at(real: u64, len: u64) -> u64 {
	let mut regions = REGIONS.lock().expect("the region table");
	let fake = 0x1000_0000u64 + regions.len() as u64 * 0x10_0000;
	regions.push((fake, real, len));
	fake
}

fn publish(bytes: &'static [u8]) -> u64 {
	publish_at(bytes.as_ptr() as u64, bytes.len() as u64)
}

fn here(address: u64) -> u64 {
	let regions = REGIONS.lock().expect("the region table");
	for (fake, real, len) in regions.iter() {
		if address >= *fake && address < fake + len {
			return real + (address - fake);
		}
	}
	// A read outside every published blob is a reader following a pointer it invented, and the
	// useful answer is a diagnosable panic rather than a segmentation fault.
	panic!("the reader followed {address:#x}, which is not inside any table this test published");
}

// An ACPI table with a correct header: signature, length, revision, and a checksum over the whole
// of it. Real firmware writes one; a reader that skipped the checksum would accept anything.
fn table(signature: &[u8; 4], revision: u8, body: &[u8]) -> Vec<u8> {
	let mut out = Vec::new();
	out.extend_from_slice(signature);
	out.extend_from_slice(&((36 + body.len()) as u32).to_le_bytes());
	out.push(revision);
	out.push(0); // checksum, filled below
	out.extend_from_slice(b"LIBER "); // OEM id
	out.extend_from_slice(b"LIBERSYS"); // OEM table id
	out.extend_from_slice(&1u32.to_le_bytes()); // OEM revision
	out.extend_from_slice(b"LIBE"); // creator id
	out.extend_from_slice(&1u32.to_le_bytes()); // creator revision
	out.extend_from_slice(body);
	let sum = out.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
	out[9] = (!sum).wrapping_add(1);
	out
}

// An SPCR body: interface type, three reserved bytes, then the Generic Address Structure.
fn spcr_body(interface: u8, space: u8, access: u8, base: u64) -> Vec<u8> {
	let mut body = vec![interface, 0, 0, 0];
	body.push(space);
	body.push(8); // bit width
	body.push(0); // bit offset
	body.push(access);
	body.extend_from_slice(&base.to_le_bytes());
	// Revision 1 SPCR runs to 80 bytes; the rest is interrupt and baud description this does not
	// read, and a table that stopped at the GAS would be a table real firmware does not write.
	body.resize(80 - 36, 0);
	body
}

// An RSDP, an XSDT and one table, laid out in one buffer and leaked so the addresses inside it
// stay valid - which is the shape the firmware's tables have.
fn machine(tables: &[Vec<u8>], revision: u8) -> u64 {
	let mut blob: Vec<u8> = Vec::new();
	// Reserve the RSDP (36 bytes) and the XSDT header plus one pointer per table.
	let rsdp_len = 36usize;
	let xsdt_len = 36 + 8 * tables.len();
	blob.resize(rsdp_len + xsdt_len, 0);
	let mut addresses = Vec::new();
	for entry in tables {
		addresses.push(blob.len());
		blob.extend_from_slice(entry);
	}
	let leaked: &'static mut [u8] = Vec::leak(blob);
	let origin = publish_at(leaked.as_ptr() as u64, leaked.len() as u64);

	// The XSDT, with its own header and checksum. The checksum is written because real firmware
	// writes it: a fixture that left it zero would be testing the reader against a table no machine
	// produces, which is how a reader that never checked one passed for two rounds.
	let xsdt_at = rsdp_len;
	leaked[xsdt_at..xsdt_at + 4].copy_from_slice(b"XSDT");
	leaked[xsdt_at + 4..xsdt_at + 8].copy_from_slice(&(xsdt_len as u32).to_le_bytes());
	leaked[xsdt_at + 8] = 1;
	for (index, offset) in addresses.iter().enumerate() {
		let at = xsdt_at + 36 + index * 8;
		leaked[at..at + 8].copy_from_slice(&(origin + *offset as u64).to_le_bytes());
	}
	checksum_at(leaked, xsdt_at, xsdt_len, xsdt_at + 9);

	// The RSDP: signature, revision, the XSDT pointer, and BOTH checksums - the ACPI 1.0 one over
	// the first twenty bytes and, from revision 2, the extended one over the whole structure. The
	// XSDT pointer lives in the part the first does not cover.
	leaked[0..8].copy_from_slice(b"RSD PTR ");
	leaked[15] = revision;
	leaked[24..32].copy_from_slice(&(origin + xsdt_at as u64).to_le_bytes());
	if revision >= 2 {
		leaked[20..24].copy_from_slice(&(rsdp_len as u32).to_le_bytes());
	}
	checksum_at(leaked, 0, 20, 9);
	if revision >= 2 {
		checksum_at(leaked, 0, rsdp_len, 32);
	}
	origin
}

// Write the byte at `slot` that makes `length` bytes from `start` sum to zero.
fn checksum_at(bytes: &mut [u8], start: usize, length: usize, slot: usize) {
	bytes[slot] = 0;
	let sum = bytes[start..start + length].iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
	bytes[slot] = (!sum).wrapping_add(1);
}

#[test]
fn spcr_names_the_console_of_an_acpi_machine() {
	// The case the whole change is for: server-class aarch64 has no device tree, so the console
	// address the loader used to store to after `ExitBootServices` was a literal from QEMU's virt
	// machine. SPCR is where such a machine says it.
	let rsdp = machine(&[table(b"SPCR", 2, &spcr_body(0x0e, 0, 1, 0xffff_0000_0000_9000))], 2);
	let acpi = acpi::Acpi::new(rsdp, here);
	assert!(acpi.is_valid(), "a well-formed RSDP is accepted");
	assert_eq!(acpi.console(), Some(acpi::Console { uart: acpi::Uart::Pl011, base: 0xffff_0000_0000_9000, reg_shift: 0 }), "an SBSA generic UART is a PL011 for the two registers this drives");
}

#[test]
fn a_sixteen_five_fifty_with_dword_access_is_four_bytes_between_registers() {
	// The 16550 half, and the field that is easy to ignore: a table describing 32-bit accesses is
	// describing registers four bytes apart. Writing the character at +5 instead of +20 puts it in
	// the modem-control register.
	let rsdp = machine(&[table(b"SPCR", 2, &spcr_body(0x00, 0, 3, 0x1000_0000))], 2);
	assert_eq!(acpi::Acpi::new(rsdp, here).console(), Some(acpi::Console { uart: acpi::Uart::Ns16550, base: 0x1000_0000, reg_shift: 2 }));
}

#[test]
fn a_console_that_is_not_in_memory_or_not_a_uart_we_drive_is_refused() {
	// Three ways SPCR describes something this loader must not store to, each of which a reader
	// that only looked at the address would accept.
	let ports = machine(&[table(b"SPCR", 2, &spcr_body(0x00, 1, 1, 0x3f8))], 2);
	assert_eq!(acpi::Acpi::new(ports, here).console(), None, "an I/O-port console is not a store away");

	let unknown = machine(&[table(b"SPCR", 2, &spcr_body(0x05, 0, 1, 0x1000_0000))], 2);
	assert_eq!(acpi::Acpi::new(unknown, here).console(), None, "a UART family with no driver here is not guessed at");

	let nowhere = machine(&[table(b"SPCR", 2, &spcr_body(0x03, 0, 1, 0))], 2);
	assert_eq!(acpi::Acpi::new(nowhere, here).console(), None, "and address zero is not an address");
}

#[test]
fn a_machine_with_no_spcr_answers_none_rather_than_the_first_table_it_finds() {
	let rsdp = machine(&[table(b"FACP", 2, &[0u8; 200]), table(b"APIC", 2, &[0u8; 100])], 2);
	let acpi = acpi::Acpi::new(rsdp, here);
	assert!(acpi.table(b"APIC").is_some(), "the walk finds the tables that are there");
	assert_eq!(acpi.table(b"SPCR"), None);
	assert_eq!(acpi.console(), None);
}

#[test]
fn a_configuration_table_entry_that_is_not_an_rsdp_is_not_walked() {
	// The loader hands this whatever the firmware's configuration table held under the ACPI GUID.
	// An entry that points at something else is a pointer into arbitrary memory, and the checksum
	// is what tells the difference - a signature alone can occur by accident in a data table.
	let rubbish: &'static [u8] = Vec::leak(vec![0x41u8; 4096]);
	let acpi = acpi::Acpi::new(publish(rubbish), here);
	assert!(!acpi.is_valid());
	assert_eq!(acpi.console(), None);

	// A correct signature with a wrong checksum is the case that matters, because it is what a
	// stale or half-written table looks like.
	let mut bytes = vec![0u8; 64];
	bytes[0..8].copy_from_slice(b"RSD PTR ");
	bytes[15] = 2;
	let leaked: &'static [u8] = Vec::leak(bytes);
	assert!(!acpi::Acpi::new(publish(leaked), here).is_valid(), "the signature is right and the checksum is not");
}

#[test]
fn an_acpi_one_point_zero_machine_is_read_through_its_rsdt() {
	// Revision 0 has no XSDT, and a reader that always took the 64-bit pointer at offset 24 would
	// read two words of the OEM id as an address. Older aarch64 firmware is rare; older x86
	// firmware is not.
	let spcr = table(b"SPCR", 1, &spcr_body(0x03, 0, 1, 0x9000_0000));
	let mut blob: Vec<u8> = vec![0u8; 36];
	let rsdt_at = blob.len();
	blob.resize(rsdt_at + 36 + 4, 0);
	let spcr_at = blob.len();
	blob.extend_from_slice(&spcr);
	let leaked: &'static mut [u8] = Vec::leak(blob);
	let origin = publish_at(leaked.as_ptr() as u64, leaked.len() as u64);
	leaked[rsdt_at..rsdt_at + 4].copy_from_slice(b"RSDT");
	leaked[rsdt_at + 4..rsdt_at + 8].copy_from_slice(&40u32.to_le_bytes());
	leaked[rsdt_at + 36..rsdt_at + 40].copy_from_slice(&((origin + spcr_at as u64) as u32).to_le_bytes());
	checksum_at(leaked, rsdt_at, 40, rsdt_at + 9);
	leaked[0..8].copy_from_slice(b"RSD PTR ");
	leaked[15] = 0;
	leaked[16..20].copy_from_slice(&((origin + rsdt_at as u64) as u32).to_le_bytes());
	checksum_at(leaked, 0, 20, 9);

	let console = acpi::Acpi::new(origin, here).console().expect("revision 0 is read through the RSDT");
	assert_eq!(console.base, 0x9000_0000);
}

#[test]
fn a_table_that_fails_its_checksum_is_not_read() {
	// THE HALF THE SIGNATURE CANNOT TELL. A stale or half-written table keeps its signature and
	// stops summing to zero, and this parser walks to an address the loader then WRITES to after
	// `ExitBootServices`. Three structures carry a checksum on the way there and none was checked.

	// 1. The RSDP's extended checksum. Revision 2 carries `Length` and a second sum over the whole
	// structure, and the XSDT pointer lives in the part the first twenty bytes do not cover - so
	// the pointer this parser follows first was taken from bytes nothing had summed.
	let rsdp = machine(&[table(b"SPCR", 2, &spcr_body(0x03, 0, 1, 0x9000_0000))], 2);
	assert!(acpi::Acpi::new(rsdp, here).is_valid(), "the well-formed one is accepted");
	let bytes = unsafe { core::slice::from_raw_parts_mut(here(rsdp) as *mut u8, 36) };
	bytes[32] = bytes[32].wrapping_add(1);
	assert!(!acpi::Acpi::new(rsdp, here).is_valid(), "an RSDP whose extended checksum is off by one is not an RSDP");
	assert_eq!(acpi::Acpi::new(rsdp, here).console(), None, "and nothing is read through it");
	bytes[32] = bytes[32].wrapping_sub(1);

	// 2. The root table's signature. It was taken on trust: a length in range was the whole test,
	// so entries were read out of a structure only assumed to be a table.
	let rsdp = machine(&[table(b"SPCR", 2, &spcr_body(0x03, 0, 1, 0x9000_0000))], 2);
	let root = unsafe { core::slice::from_raw_parts_mut(here(rsdp + 36) as *mut u8, 4) };
	root.copy_from_slice(b"XSDX");
	assert_eq!(acpi::Acpi::new(rsdp, here).table(b"SPCR"), None, "a root table whose signature is neither XSDT nor RSDT is not walked");

	// 3. The root table's own checksum.
	let rsdp = machine(&[table(b"SPCR", 2, &spcr_body(0x03, 0, 1, 0x9000_0000))], 2);
	let root = unsafe { core::slice::from_raw_parts_mut(here(rsdp + 36) as *mut u8, 45) };
	root[9] = root[9].wrapping_add(1);
	assert_eq!(acpi::Acpi::new(rsdp, here).table(b"SPCR"), None, "a root table that does not sum to zero is not walked");

	// 4. And the table the entry names. An SPCR whose checksum is off by one gives no console
	// rather than a wrong one, which is the address the loader would have stored to.
	let rsdp = machine(&[table(b"SPCR", 2, &spcr_body(0x03, 0, 1, 0x9000_0000))], 2);
	let spcr = acpi::Acpi::new(rsdp, here).table(b"SPCR").expect("the good one is found");
	let body = unsafe { core::slice::from_raw_parts_mut(here(spcr) as *mut u8, 80) };
	body[9] = body[9].wrapping_add(1);
	assert_eq!(acpi::Acpi::new(rsdp, here).table(b"SPCR"), None, "an SPCR that does not sum to zero is not a table");
	assert_eq!(acpi::Acpi::new(rsdp, here).console(), None, "and the loader gets no console rather than a wrong one");
}

// An FADT body with ARM Boot Architecture Flags at offset 129 (revision 5 and later). The header is
// 36 bytes, so the flags are at body offset 93.
fn fadt_body(flags: u16) -> Vec<u8> {
	let mut body = vec![0u8; 240 - 36];
	body[93..95].copy_from_slice(&flags.to_le_bytes());
	body
}

#[test]
fn the_fadt_says_which_instruction_reaches_psci() {
	// THE ACPI HALF of the question the loader used to answer by looking at its own exception level.
	// A machine that hands firmware EL1 has something below it, and what that something answers is
	// stated by the platform - here, in the FADT's ARM Boot Architecture Flags.
	let hvc = machine(&[table(b"FACP", 5, &fadt_body(0x0003))], 2);
	assert_eq!(acpi::Acpi::new(hvc, here).psci_conduit(), Some(acpi::PsciConduit::Hvc), "PSCI_COMPLIANT and PSCI_USE_HVC");

	let smc = machine(&[table(b"FACP", 5, &fadt_body(0x0001))], 2);
	assert_eq!(acpi::Acpi::new(smc, here).psci_conduit(), Some(acpi::PsciConduit::Smc), "PSCI_COMPLIANT without PSCI_USE_HVC is SMC - which is most server-class AArch64");

	// NOT COMPLIANT IS NOT "SMC BY DEFAULT". A firmware that says it has no PSCI has none, and the
	// caller must treat that as no secondaries rather than as a reason to try an instruction.
	let none = machine(&[table(b"FACP", 5, &fadt_body(0x0002))], 2);
	assert_eq!(acpi::Acpi::new(none, here).psci_conduit(), None, "PSCI_USE_HVC without PSCI_COMPLIANT describes nothing");

	let no_fadt = machine(&[table(b"SPCR", 2, &spcr_body(0x03, 0, 1, 0x9000_0000))], 2);
	assert_eq!(acpi::Acpi::new(no_fadt, here).psci_conduit(), None, "a machine with no FADT states nothing");

	// A table too short to hold the field has no answer either - the flags arrived in FADT
	// revision 5, and reading them out of a shorter table reads whatever follows it.
	let short = machine(&[table(b"FACP", 1, &vec![0u8; 116 - 36])], 2);
	assert_eq!(acpi::Acpi::new(short, here).psci_conduit(), None, "an ACPI 1.0 FADT has no ARM boot flags");
}

// P02M0129, sixth round: the stride, the whole-number rule and the second `GetMemoryMap`.
#[test]
fn a_descriptor_stride_that_cannot_be_one_is_refused() {
	let _guard = guard();
	state().descriptors = vec![descriptor(crate::CONVENTIONAL_MEMORY, 0x1000, 1), descriptor(crate::CONVENTIONAL_MEMORY, 0x3000, 1)];
	let (buf, pages, map_size, desc_size) = memory::memory_map_snapshot(mock::boot_services()).expect("the map is taken");
	let mut regions = vec![bootproto::MemRegion { base: 0, length: 0, kind: 0, _pad: 0 }; memory::MAX_REGIONS];

	// A stride SMALLER than a descriptor reads each entry past its own end into the next - or, at
	// the last, past the buffer. `desc_size == 0` was refused and this was not; the specification
	// allows the firmware's stride to be larger than the structure, never smaller.
	assert!(memory::translate_map(buf, map_size, core::mem::size_of::<crate::MemoryDescriptor>() - 1, regions.as_mut_ptr()).is_none(), "a stride shorter than a descriptor is not a stride");

	// A map that is not a whole number of descriptors had its partial tail silently discarded, which
	// for the one structure that says which RAM exists is the same looks-complete-and-is-not failure
	// the `MAX_REGIONS` refusal exists for.
	assert!(memory::translate_map(buf, map_size - 1, desc_size, regions.as_mut_ptr()).is_none(), "a map that is not a whole number of descriptors is refused rather than truncated");

	// And the honest pair still translates, so these are rules about the arguments and not a
	// refusal of the ordinary case.
	assert_eq!(memory::translate_map(buf, map_size, desc_size, regions.as_mut_ptr()), Some(2), "the map itself is fine");
	unsafe { ((*mock::boot_services()).free_pages)(buf as u64, pages) };
}

#[test]
fn a_region_whose_end_leaves_the_address_space_is_refused() {
	// `page_count * PAGE_SIZE` was checked and `phys_start + length` was not, so a descriptor whose
	// base is near the top of the address space and whose length takes it past described a region no
	// machine has - which the kernel would then carve, seed or map.
	let _guard = guard();
	state().descriptors = vec![descriptor(crate::CONVENTIONAL_MEMORY, u64::MAX - 0x1000, 4)];
	let (buf, pages, map_size, desc_size) = memory::memory_map_snapshot(mock::boot_services()).expect("the map is taken");
	let mut regions = vec![bootproto::MemRegion { base: 0, length: 0, kind: 0, _pad: 0 }; memory::MAX_REGIONS];
	assert!(memory::translate_map(buf, map_size, desc_size, regions.as_mut_ptr()).is_none(), "a region that leaves the address space is refused");
	unsafe { ((*mock::boot_services()).free_pages)(buf as u64, pages) };
}
