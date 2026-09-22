#!/usr/bin/env bash
# A DELIBERATE DEFECT IN EACH DRIVER DECISION MODULE, AND THE TEST THAT MUST CATCH IT.
#
# The roadmap asks every driver family for "pure descriptor/register/ring parsers with host tests and
# deterministic malformed mutations", and it says why in the sentence about the parser gate: a
# WATCHED-FAIL MUTATION is what proves a broken parser is caught. Without one, "it has host tests" is
# a claim about how many tests exist rather than about what they would notice, and a suite that
# passes its own mutation supports nothing.
#
# THE MUTATIONS ARE THE MISTAKES THE MODULES WERE WRITTEN AGAINST, one per family, each named in the
# comment beside the code it breaks: a queue length taken from a zero-based register, a phase bit
# trusted without its entry, a command-issue bit read as success, a card's capacity read with the
# wrong CSD version, and a codec verb built with the wrong payload width. Every one of them is silent
# in production - none produces an error anywhere - which is exactly why each has a test and why the
# test has to be shown failing.
#
# IT RUNS ON A COPY. Nothing here writes the working tree, which somebody may be editing while this
# runs; the path is fixed under `.build` rather than temporary so cargo's fingerprints stay valid and
# every mutation after the first is an incremental build of one crate.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."
REPO="$PWD"
WORK="$REPO/.build/mutations/drivers"
DRIVERS="src/user/drivers/core/Cargo.toml"
PROTOCOL="src/user/libs/driver/protocol/Cargo.toml"

fail() {
	echo "driver-mutations: $*" >&2
	exit 1
}

mkdir -p "$WORK"
# `--delete` so a file removed from the tree does not live on in the copy and get mutated for ever.
if command -v rsync >/dev/null; then
	rsync -a --delete "$REPO/src/" "$WORK/src/"
else
	rm -rf "$WORK/src"
	cp -a "$REPO/src" "$WORK/src"
fi
[[ -f "$REPO/Cargo.toml" ]] && cp -a "$REPO/Cargo.toml" "$WORK/" 2>/dev/null || true
# Everything the sync brought over is stamped new, so the first build after a sync cannot reuse an
# object compiled from a previous run's mutation. See `restore` below for why this is not paranoia.
find "$WORK/src" -name '*.rs' -exec touch {} +

# The unmutated copy must PASS first. A copy that cannot pass is a gate whose every mutation "fails"
# for a reason that has nothing to do with the mutation, which is the shape of a check that tests
# nothing while reporting success.
if ! (cd "$WORK" && cargo test --quiet --manifest-path "$DRIVERS" --lib >/dev/null 2>&1 && cargo test --quiet --manifest-path "$PROTOCOL" --lib >/dev/null 2>&1); then
	fail "the unmutated copy does not pass its own tests - every mutation below would 'fail' for that reason instead"
fi

planted=0
caught=0

# `mutate <manifest> <file> <old> <new> <test>`: restore the file from this tree, apply one literal
# replacement, and require the named test to fail. THE MANIFEST IS NAMED because the decisions live in
# two crates - the block wire is shared with a service and is in the protocol crate - and running a
# test under the wrong one finds no test at all, which would look exactly like a mutation nothing
# caught.
#
# A REPLACEMENT THAT MATCHES NOTHING IS AN ERROR AND NOT A SKIP, because a mutation that was never
# planted is a mutation nothing had to catch, and the gate would report success for having done
# nothing. The same goes for one that matches twice: the defect it plants is not the one it names.
# COPY THE FILE BACK AND MAKE IT LOOK NEW, and the second half is not decoration.
#
# `cp -a` and `rsync -a` PRESERVE MODIFICATION TIMES, so restoring a mutated file to its original
# content also restores its original timestamp - and cargo, whose fingerprints are mtime-based, then
# reuses the object it compiled from the MUTATED source. This gate caught itself doing exactly that:
# the copy's `nvme.rs` was byte-identical to the tree's and its tests failed anyway, reporting 63
# where the source says 64. A mutation harness that does not actually recompile is the purest form of
# the thing it exists to prevent - a check reporting on code nobody ran.
restore() {
	cp -a "$REPO/$1" "$WORK/$1"
	touch "$WORK/$1"
}

mutate() {
	local manifest="$1" file="$2" old="$3" new="$4" test="$5"
	restore "$file"
	python3 - "$WORK/$file" "$old" "$new" <<'PY'
import io, sys
path, old, new = sys.argv[1], sys.argv[2], sys.argv[3]
text = io.open(path, encoding="utf-8").read()
found = text.count(old)
if found != 1:
	sys.stderr.write("driver-mutations: %r appears %d time(s) in %s, not once\n" % (old, found, path))
	raise SystemExit(1)
io.open(path, "w", encoding="utf-8").write(text.replace(old, new))
PY
	touch "$WORK/$file"
	planted=$((planted + 1))
	# THE TEST MUST EXIST AND MUST FAIL. A filter that matches nothing prints a passing result with
	# no tests run, which is indistinguishable from a mutation that went uncaught unless the count is
	# checked too.
	local outcome
	# `|| true` BECAUSE THE FAILURE IS THE POINT. This script runs under `set -e` and `pipefail`, and
	# the command being measured is one that MUST fail - so without this the shell kills the harness
	# at the first successfully-planted defect, before it can record that the defect was caught. It
	# did exactly that: the trace showed the mutation planted, the test failing, and the script
	# ending there with no output at all.
	outcome="$(cd "$WORK" && cargo test --quiet --manifest-path "$manifest" --lib "$test" 2>/dev/null | grep -E "^test result:" | tail -1 || true)"
	case "$outcome" in
	*"0 passed; 0 failed"*)
		restore "$file"
		fail "no test named $test exists - the mutation in $file had nothing to catch it"
		;;
	*"ok."*)
		restore "$file"
		fail "$test still passes with $file mutated - it does not check what it is named for"
		;;
	esac
	caught=$((caught + 1))
	echo "driver-mutations: $test caught the defect planted in $(basename "$file")"
	restore "$file"
}

# NVMe: the queue size register is ZERO-BASED. Taking it as a size makes every queue one entry too
# long, and the entry past the end is where the controller writes.
mutate "$DRIVERS" src/user/drivers/core/src/nvme.rs \
	"max_entries: mqes + 1," \
	"max_entries: mqes," \
	the_queue_size_is_one_more_than_the_register_says

# NVMe: the completion's status is a TYPE and a CODE, and a non-zero type with a zero code is still a
# failure. Reading only the code calls it success.
mutate "$DRIVERS" src/user/drivers/core/src/nvme.rs \
	"		self.status_type == 0 && self.status_code == 0" \
	"		self.status_code == 0" \
	a_nonzero_status_type_is_a_failure_even_when_the_code_is_zero

# NVMe: an unaligned transfer spans one page more than its length suggests, and the missing page is
# the tail of the caller's data.
mutate "$DRIVERS" src/user/drivers/core/src/nvme.rs \
	"	let first_page_holds = page - (base & (page - 1));
	if len <= first_page_holds {" \
	"	let first_page_holds = page;
	if len <= first_page_holds {" \
	an_unaligned_transfer_needs_a_page_more_than_its_length_suggests

# NVMe: an entry whose phase arrived before the rest of it is not a completion. Collapsing it into
# "somebody else's completion" is what made this driver's bring-up fail one boot in three, and the
# rule lived in the driver where no test could reach it until it was moved here.
mutate "$DRIVERS" src/user/drivers/core/src/nvme.rs \
	"	if completion.command_id == NEVER_ISSUED {
		return Reaped::Arriving;
	}" \
	"	if false {
		return Reaped::Arriving;
	}" \
	a_phase_bit_that_arrived_before_its_entry_is_not_a_completion

# AHCI: the command-issue bit clears on COMPLETION and not on success, so a driver watching only it
# reports a bad sector as good data.
#
# THE ANCHOR CARRIES THE LINE ABOVE IT BECAUSE THE TEST ALONE IS NO LONGER UNIQUE. NCQ brought a
# second completion rule - `queued_outcome`, which reads `PxSACT` where this reads `PxCI` - and the
# error test is written the same way in both. A one-line anchor then matched twice and this gate
# refused rather than planting the defect in an arbitrary one of them, which is the right refusal and
# is how the ambiguity was noticed at all.
mutate "$DRIVERS" src/user/drivers/core/src/ahci.rs \
	"	if ci & (1 << slot) != 0 {
		return Outcome::Pending;
	}
	if tfd & TFD_ERR != 0 {" \
	"	if ci & (1 << slot) != 0 {
		return Outcome::Pending;
	}
	if false {" \
	a_cleared_slot_is_not_the_same_as_a_successful_command

# AHCI NCQ: a queued tag is outstanding while its bit is SET in `PxSACT`, which is the opposite
# register and the opposite sense from the single-command path's `PxCI`.
#
# A DRIVER THAT READ IT THE OTHER WAY ROUND WOULD ANSWER A READ BEFORE THE DEVICE HAD WRITTEN A BYTE.
# The mutation takes the test out, so every tag reads as complete the moment it is looked at - which
# is exactly what the oracle's batch of concurrent commands is there to catch.
mutate "$DRIVERS" src/user/drivers/core/src/ahci.rs \
	"	if sact & (1 << slot) != 0 {
		return Outcome::Pending;
	}" \
	"	if false {
		return Outcome::Pending;
	}" \
	a_queued_tag_is_outstanding_while_its_sact_bit_is_set

# AHCI: the port bitmap is not a port count, and walking the count reads registers of ports that are
# not there.
mutate "$DRIVERS" src/user/drivers/core/src/ahci.rs \
	"	(0..bound).filter(move |port| pi & (1 << port) != 0)" \
	"	(0..bound).filter(move |port| pi == pi || *port == 0)" \
	the_port_bitmap_is_not_a_port_count

# SDHCI: a version 1 CSD and a version 2 CSD are different arithmetic, and applying the wrong one
# reports a capacity wrong by orders of magnitude with no error anywhere.
mutate "$DRIVERS" src/user/drivers/core/src/sdhci.rs \
	"			let blocks = (c_size as u64 + 1) * 1024;" \
	"			let blocks = (c_size as u64 + 1) * 2048;" \
	a_version_one_card_and_a_version_two_card_are_sized_by_different_arithmetic

# SDHCI: the capacity bit is only valid once the card says it is ready, and reading it early drives a
# high-capacity card as a standard one - every request reading the first sector of the medium.
mutate "$DRIVERS" src/user/drivers/core/src/sdhci.rs \
	"	Ocr { ready, high_capacity: ready && value & (1 << 30) != 0 }" \
	"	Ocr { ready, high_capacity: value & (1 << 30) != 0 }" \
	the_capacity_bit_is_not_read_before_the_card_says_it_is_ready

# HDA: a four-bit verb carries a sixteen-bit payload and every other verb carries eight. Building one
# as the other shifts the command into the payload and the payload into the node, and the codec
# ANSWERS it.
mutate "$DRIVERS" src/user/drivers/core/src/hda.rs \
	"	let wide = command <= 0xF;" \
	"	let wide = false;" \
	a_four_bit_verb_carries_a_wide_payload_and_a_twelve_bit_one_does_not

# HDA: the format word is indices and not the numbers it names.
mutate "$DRIVERS" src/user/drivers/core/src/hda.rs \
	"	Some(base | (depth << 4) | (channels as u16 - 1))" \
	"	Some(base | (depth << 4) | channels as u16)" \
	the_format_word_is_indices_and_not_the_numbers_it_names

# SCSI: the block address is big-endian, which is the opposite of every other wire here. Written the
# other way round it names a plausible address on the same medium and nothing refuses it.
mutate "$DRIVERS" src/user/drivers/core/src/scsi.rs \
	"	cdb[2] = (lba >> 24) as u8;" \
	"	cdb[2] = lba as u8;" \
	a_block_address_is_big_endian_which_is_the_opposite_of_every_other_wire_here

# SCSI: READ CAPACITY answers with the LAST BLOCK, not the count. Read as a count it makes a driver
# read one block past the end of every medium it serves.
mutate "$DRIVERS" src/user/drivers/core/src/scsi.rs \
	"	Ok(Capacity { blocks: last as u64 + 1, block_bytes })" \
	"	Ok(Capacity { blocks: last as u64, block_bytes })" \
	the_capacity_answer_is_the_last_block_and_not_the_count

# SCSI: NOT READY covers a unit spinning up and one with no medium at all, and only the additional
# code tells them apart. The key alone either retries for ever or gives up a second too early.
mutate "$DRIVERS" src/user/drivers/core/src/scsi.rs \
	"			(0x04, 0x01) | (0x04, 0x02) | (0x04, 0x03) => Sense::NotReadyYet," \
	"			(0xFF, 0xFF) => Sense::NotReadyYet," \
	not_ready_covers_a_unit_spinning_up_and_one_with_no_medium_and_the_key_alone_cannot_tell

# SCSI: the LUN list length counts BYTES and not units. Read as a count, a target with four units
# reports four bytes of list, which does not hold one whole entry - so the driver serves none of them
# and nothing anywhere contradicts the number.
mutate "$DRIVERS" src/user/drivers/core/src/scsi.rs \
	"	let usable = claimed.min(arrived) / 8;" \
	"	let usable = claimed.min(arrived / 8);" \
	the_lun_list_length_counts_bytes_and_not_units

# SCSI: the top two bits of an addressing field choose how the rest is read. Skipping them reads a
# flat unit 0x0101 as peripheral unit 1 - which exists on most targets and answers, so the wrong
# medium is served under the right name.
mutate "$DRIVERS" src/user/drivers/core/src/scsi.rs \
	"		0b01 => Some((((field[0] & 0x3F) as u16) << 8) | field[1] as u16)," \
	"		0b01 => Some(field[1] as u16)," \
	the_addressing_method_decides_which_number_the_same_bytes_name

# SCSI: the list length is the DEVICE'S CLAIM and not the answer's length. A target says "ask again
# with more" by claiming a longer list, and a driver indexing by the claim reads past its own DMA
# page - and publishes whatever the last command left there as a medium.
mutate "$DRIVERS" src/user/drivers/core/src/scsi.rs \
	"	let usable = claimed.min(arrived) / 8;" \
	"	let usable = claimed / 8;" \
	a_list_longer_than_the_answer_is_clamped_to_what_arrived

# HID: an absolute axis is read in the signedness its descriptor declared. Read signed, an eight-bit
# axis over 0..255 reports minus sixteen for a touch near the right edge and clamps to the LEFT one -
# so the right half of the surface reads as the left half, and nothing refuses anything.
mutate "$DRIVERS" src/user/drivers/core/src/hid.rs \
	"		if self.logical_min >= 0 { field(body, bit, self.size) as i32 } else { signed_field(body, bit, self.size) }" \
	"		signed_field(body, bit, self.size)" \
	an_absolute_axis_is_read_in_the_signedness_its_descriptor_declared

# HID: a new contact begins at each contact identifier. Without that, every finger takes the LAST
# one's position - one pointer that jumps rather than several fingers.
mutate "$DRIVERS" src/user/drivers/core/src/hid.rs \
	"							if let Some(done) = open.take()
								&& written < out.len()" \
	"							if let Some(done) = None::<Contact>
								&& written < out.len()" \
	each_contact_identifier_begins_a_contact_and_its_axes_are_its_own

# HID: contact count says how many slots are real. A digitizer leaves the unused ones holding what
# was there before, so taking every declared slot reports phantom fingers at stale positions.
mutate "$DRIVERS" src/user/drivers/core/src/hid.rs \
	"			Some(count) => written.min(count)," \
	"			Some(_) => written," \
	contact_count_bounds_what_is_reported_so_an_untouched_slot_is_not_a_phantom_finger

# HID: the digitizer page survives the parse. It was one of the pages the filter dropped BEFORE
# decoding, which is what flattening a tablet into a mouse is here.
mutate "$DRIVERS" src/user/drivers/core/src/hid.rs \
	"PAGE_CONSUMER | PAGE_DIGITIZER);" \
	"PAGE_CONSUMER);" \
	a_digitizer_is_not_flattened_into_a_mouse

# HID: collection depth is counted and bounded. `Collection` is bytes the DEVICE chose, and a
# descriptor that opens them without closing them nested without bound.
mutate "$DRIVERS" src/user/drivers/core/src/hid.rs \
	"					if depth > MAX_COLLECTION_DEPTH {" \
	"					if false {" \
	a_descriptor_that_never_closes_its_collections_is_bounded_rather_than_nesting_for_ever

# HID: the unit exponent is a signed nibble. Read unsigned, a tablet's position is scaled by ten to
# the fifteenth instead of divided by ten - and the number is small either way.
mutate "$DRIVERS" src/user/drivers/core/src/hid.rs \
	"	if nibble > 7 { nibble - 16 } else { nibble }" \
	"	nibble" \
	the_unit_exponent_is_a_signed_nibble_and_not_a_byte

# CDC-ACM: the union names the data interface. Binding "the interface after the communications one"
# takes a bulk pair another function is using on a composite device - and neither half refuses.
mutate "$DRIVERS" src/user/drivers/core/src/cdc.rs \
	"	let (notify_in, notify_packet) = function.notify.unwrap_or((0, 0));" \
	"	let (notify_in, notify_packet) = function.notify.unwrap_or((0, 0));
	let data_interface = union_data + 1;" \
	the_union_names_the_data_interface_and_the_next_one_is_not_it

# CDC-ACM: a data interface's bulk pair is kept UNDER ITS OWN INTERFACE NUMBER, and the union is what
# selects which one. Keeping one best pair for the whole configuration is right while there is one
# data interface and wrong the moment a composite device carries two: the second function's union
# names interface three and the pair kept is interface one's, so either a good device is refused or
# one function is handed the other's endpoints and the two byte streams cross.
mutate "$DRIVERS" src/user/drivers/core/src/cdc.rs \
	"data.iter().flatten().find(|(iface, ..)| *iface == union_data)" \
	"data.iter().flatten().min_by_key(|(iface, ..)| *iface)" \
	each_serial_function_of_a_composite_device_binds_its_own_endpoints

# CDC-ACM: ONE stop bit is encoded as ZERO. Writing the number you mean asks for one and a half,
# which some devices accept and then frame every byte differently.
mutate "$DRIVERS" src/user/drivers/core/src/cdc.rs \
	"			StopBits::One => 0," \
	"			StopBits::One => 1," \
	the_stop_bits_are_an_enumeration_and_the_data_bits_are_a_count

# CDC-ACM: a stop-bits value the structure does not define is refused rather than rounded to the
# nearest one it does.
mutate "$DRIVERS" src/user/drivers/core/src/cdc.rs \
	"			2 => StopBits::Two,
			_ => return None," \
	"			_ => StopBits::Two," \
	a_line_coding_answer_outside_the_enumeration_is_refused_rather_than_rounded

# CDC-ACM: the PROTOCOL is part of what names a serial port. RNDIS is class 2, subclass 2 and a
# VENDOR protocol - the same two bytes an ACM port declares - so a binding that reads only the class
# and the subclass takes an Ethernet adapter and the boot loses its network provider with nothing
# saying why. Measured on QEMU's own `usb-net`, which is RNDIS by default.
mutate "$DRIVERS" src/user/drivers/core/src/cdc.rs \
	"&& protocol != PROTOCOL_VENDOR " \
	"" \
	an_acm_shaped_rndis_control_interface_is_not_a_serial_port

# CDC-ACM: the line-coding capability is bit ONE of the bitmap. Read as any bit set, a device that
# publishes only call management is asked for a line coding it stalls.
mutate "$DRIVERS" src/user/drivers/core/src/cdc.rs \
	"		self.capabilities & 0x02 != 0" \
	"		self.capabilities != 0" \
	a_device_with_no_acm_descriptor_supports_no_line_coding_and_is_still_a_byte_stream

# SCSI: the MISSED flag is OR'd into the event word, not a value of it. Compared whole, a driver
# stops recognising events the moment the device says it has already dropped some - which is exactly
# when its picture of the bus is stale, and reads as a quiet driver on a busy bus.
mutate "$DRIVERS" src/user/drivers/core/src/scsi.rs \
	"	let event = match word & 0x7FFF_FFFF {" \
	"	let event = match word {" \
	the_missed_flag_is_not_part_of_the_event_number

# SCSI: one transport-reset event number covers a unit arriving, leaving and resetting itself, and
# only the reason tells them apart. Acting on the number alone re-enumerates a unit that is gone.
mutate "$DRIVERS" src/user/drivers/core/src/scsi.rs \
	"			2 => Event::Removed," \
	"			2 => Event::Rescan," \
	a_transport_reset_is_three_different_things_and_the_reason_says_which

# UAS: the tag is big-endian in a transport that is little-endian everywhere else. Swapped, it
# matches no answer - and an answer matching nothing is indistinguishable from a device fault.
#
# THE ANCHOR CARRIES THE UNIT'S TYPE BYTE, because task management brought a second information unit
# that writes its tag exactly the same way - which is the point of the tag rules and is also what
# made a two-line anchor match twice. The gate refused rather than planting the defect in whichever
# one it found first, which is the right refusal.
mutate "$DRIVERS" src/user/drivers/core/src/uas.rs \
	"	iu[0] = IU_COMMAND;
	iu[2] = (tag >> 8) as u8;
	iu[3] = tag as u8;" \
	"	iu[0] = IU_COMMAND;
	iu[2] = tag as u8;
	iu[3] = (tag >> 8) as u8;" \
	the_tag_is_big_endian_in_a_transport_that_is_little_endian_everywhere_else

# UAS: a task-management request carries its OWN tag in the header and the DOOMED command's tag in a
# separate field, and a driver that put the managed tag in the header would ask the device to answer
# under a tag already outstanding - so the answer would match the command being aborted.
mutate "$DRIVERS" src/user/drivers/core/src/uas.rs \
	"	iu[0] = IU_TASK_MANAGEMENT;
	iu[2] = (tag >> 8) as u8;
	iu[3] = tag as u8;" \
	"	iu[0] = IU_TASK_MANAGEMENT;
	iu[2] = (managed_tag >> 8) as u8;
	iu[3] = managed_tag as u8;" \
	a_task_management_header_carries_the_requests_own_tag_and_not_the_doomed_ones

# UAS: a residue larger than the request is a device describing a transfer that did not happen, and
# subtracting it computes a negative length as an enormous positive one.
mutate "$DRIVERS" src/user/drivers/core/src/uas.rs \
	"	if residue > requested {
		return None;
	}" \
	"	if false {
		return None;
	}" \
	a_residue_larger_than_the_request_is_refused_rather_than_subtracted

# CDC: WHICH alternate setting of the data interface is selected. Setting zero carries no endpoints
# at all - that is what the specification means by "not carrying traffic" - and a device may publish
# several that do, in any order. Taking the first one SEEN rather than the lowest-numbered makes the
# choice depend on the device's descriptor order, which is a difference that shows up on one vendor's
# adapter and on no other.
mutate "$DRIVERS" src/user/drivers/core/src/cdc.rs \
	"		&& best.is_none_or(|(_, have, _, _, _, _)| alt < have)" \
	"		&& best.is_none()" \
	the_lowest_numbered_setting_that_carries_a_pair_wins_whatever_order_they_appear_in

# CDC: an NCM datagram names an offset and a length INTO THE BLOCK THE DEVICE SENT. Unchecked, the
# length is a read past the buffer - which is the one defect in this family that is not a wrong
# number but a wrong page.
mutate "$DRIVERS" src/user/drivers/core/src/cdc.rs \
	"			if len == 0 || at as usize + len as usize > self.block_length {" \
	"			if len == 0 {" \
	a_datagram_running_past_the_block_is_refused

# CDC: a datagram pointer may name the next one, and a device can point one at itself. Without the
# bound the walk never ends, inside a driver, on bytes a device chose.
mutate "$DRIVERS" src/user/drivers/core/src/cdc.rs \
	"			if self.visited > self.most {" \
	"			if false {" \
	a_pointer_chain_that_loops_ends_rather_than_spinning

# vsock: the credit counters are free-running u32s and the window between them is MODULAR. A
# subtraction that is not wrapping gives four gibibytes of window at the wrap and zero the other way
# - the first sends into a buffer that is not there, the second stalls a healthy connection for ever.
mutate "$DRIVERS" src/user/drivers/core/src/vsock.rs \
	"	let in_flight = sent.wrapping_sub(fwd_cnt);" \
	"	let in_flight = sent.saturating_sub(fwd_cnt);" \
	the_window_survives_the_counters_wrapping

# vsock: a peer claiming to have taken more than it was ever sent is claiming a window wider than its
# own buffer. Trusting it is the overrun the item calls a hostile credit update.
mutate "$DRIVERS" src/user/drivers/core/src/vsock.rs \
	"	if in_flight > buf_alloc {
		return None;
	}" \
	"	if false {
		return None;
	}" \
	a_peer_claiming_more_than_it_was_sent_is_refused

# vsock: a shutdown is DIRECTIONAL. Closing the whole connection on one direction loses every byte
# this side still had to write, and the loss is silent at both ends.
mutate "$DRIVERS" src/user/drivers/core/src/vsock.rs \
	"			if peer_done && we_done { State::Closed } else { State::HalfClosed { peer_done, we_done } }" \
	"			State::Closed" \
	shutdown_is_directional_and_one_direction_is_not_a_close

# The local stream wire: a send declares its own length, and the header is a claim another address
# space made. Checked against the bound alone, a header claiming four kilobytes in a message that
# carried eight bytes is admitted.
mutate "$PROTOCOL" src/user/libs/driver/protocol/src/stream.rs \
	"			if arg > MAX_PAYLOAD || arg as usize != payload.len() {" \
	"			if arg > MAX_PAYLOAD {" \
	a_send_claiming_more_than_it_carries_is_refused

# The block wire, which four drivers and one service now share: `STATUS_INVALID` exists so a caller
# can tell a request it got wrong from a device that failed one it got right.
mutate "$PROTOCOL" src/user/libs/driver/protocol/src/block.rs \
	"pub const STATUS_INVALID: u32 = 2;" \
	"pub const STATUS_INVALID: u32 = 1;" \
	the_three_statuses_stay_distinct_and_a_refusal_is_not_a_device_failure

# ----------------------------------------------------------------------------------------------
# THE DECISION MODULES THAT HAD FIXTURES AND NOTHING THAT PROVED THEY WOULD NOTICE (DRV-016).
#
# Thirteen modules below this line reached the tree with host tests and no planted defect, which is
# the claim DRV-016 refuses to accept on its own: "it has host tests" is a statement about how many
# tests exist, not about what they would catch. The counts said the same thing - nine families
# carried forty-two mutations between them and thirteen modules carried none - so the coverage the
# suite reported was concentrated in the families somebody had already gone through.
#
# ONE OF THEM FOUND A HOLE IMMEDIATELY, which is the whole argument for doing this. The console's
# lock-key test is named `a_lock_key_toggles_once_per_press` and its own comment says "an autorepeat
# must not toggle it again, or holding it flickers" - and it fed the key a press and a release and
# never an autorepeat. A lock that toggled on value 2 passed it. The test now feeds the repeat it
# always claimed to hold.
# ----------------------------------------------------------------------------------------------
# Descriptors: the DECLARED length bounds the walk and the received count does not. A device may
# answer with more bytes than the descriptor it carries - the page is reused between transfers - so a
# walk sized by what arrived reads the PREVIOUS descriptor's records as this one's.
mutate "$DRIVERS" src/user/drivers/core/src/descriptor.rs \
	"	Ok(declared)
" \
	"	Ok(received as u16)
" \
	a_transfer_shorter_than_the_descriptor_it_claims_is_refused

# Descriptors: a record is its own bytes and nothing past them. A record that ran to the end of the
# transfer instead would answer every field read of it - with the next record's header, which is a
# class byte, a subclass and a protocol that belong to another interface.
mutate "$DRIVERS" src/user/drivers/core/src/descriptor.rs \
	"bytes: &self.bytes[self.offset..end]" \
	"bytes: &self.bytes[self.offset..]" \
	a_field_past_a_records_own_length_is_refused_rather_than_read

# Descriptors: the two-byte fields are LITTLE-endian, like every other number on this bus. Read the
# other way round, an endpoint's maximum packet size of 512 reads as 2 - and the driver then asks the
# controller for transfers the endpoint cannot carry.
mutate "$DRIVERS" src/user/drivers/core/src/descriptor.rs \
	"Ok(self.field(offset)? as u16 | (self.field(offset + 1)? as u16) << 8)" \
	"Ok((self.field(offset)? as u16) << 8 | self.field(offset + 1)? as u16)" \
	a_field_past_a_records_own_length_is_refused_rather_than_read

# Bulk-only transport: the residue is checked BEFORE it is subtracted. A device claiming a residue
# larger than the transfer describes bytes nobody asked for, and the subtraction wraps into a moved
# count of four gigabytes - which the caller then reads out of a buffer that holds one page.
mutate "$DRIVERS" src/user/drivers/core/src/usb.rs \
	"	if residue > wanted {
		return Err(BotFault::Residue);
	}
	let moved = wanted - residue;" \
	"	let moved = wanted.wrapping_sub(residue);" \
	a_residue_past_the_transfer_is_refused_before_it_is_subtracted

# Bulk-only transport: a short transfer is refused where the command did not allow one. Allowed the
# other way round, a read of eight sectors that moved four reports success, and the four sectors the
# device never wrote are whatever the bounce page held.
mutate "$DRIVERS" src/user/drivers/core/src/usb.rs \
	"	if !allow_short && moved < wanted {" \
	"	if allow_short && moved < wanted {" \
	a_short_transfer_is_refused_rather_than_read_as_a_success

# Bulk-only transport: "this unit has no cache" is ONE refusal with ONE meaning - an illegal request
# naming an invalid operation code. Read as any illegal request, a unit that refused the flush for
# some other reason reports a barrier that held, and the filesystem above it orders its commits
# against a promise nothing kept.
mutate "$DRIVERS" src/user/drivers/core/src/usb.rs \
	"if key & 0x0f == SENSE_ILLEGAL_REQUEST && asc == ASC_INVALID_COMMAND" \
	"if key & 0x0f == SENSE_ILLEGAL_REQUEST" \
	a_flush_that_failed_is_a_failure_unless_the_unit_has_no_cache

# virtio: a descriptor id is an INDEX into a ring of `size` entries, so `size` itself is not one of
# them. Admitted, it reads the descriptor one past the table - which is the available ring's own
# bytes, read as an address and a length.
mutate "$DRIVERS" src/user/drivers/core/src/virtio.rs \
	"	if id >= size as u32 {" \
	"	if id > size as u32 {" \
	the_pure_checks_refuse_a_foreign_id_an_oversized_length_and_an_index_past_what_was_posted

# virtio: the used element's length is the DEVICE's claim about how much it wrote, and the chain it
# was given is what bounds it. Unchecked, a device reports more than the chain could hold and the
# driver hands the layer above it bytes from the next buffer in the pool.
mutate "$DRIVERS" src/user/drivers/core/src/virtio.rs \
	"	if len > max_len {
		return Err(UsedFault::Length);
	}
" \
	"" \
	the_pure_checks_refuse_a_foreign_id_an_oversized_length_and_an_index_past_what_was_posted

# virtio: the used index is a free-running u16 and the difference WRAPS. Read as a signed move, the
# wrap at 65535 reads as no completions at all - so a queue that has been running for an hour stops
# reaping, which looks like a device that went quiet rather than like arithmetic.
mutate "$DRIVERS" src/user/drivers/core/src/virtio.rs \
	"	let advanced = now.wrapping_sub(last);" \
	"	let advanced = if now >= last { now - last } else { 0 };" \
	the_pure_checks_refuse_a_foreign_id_an_oversized_length_and_an_index_past_what_was_posted

# virtio: the available ring's header is SIX bytes - flags, index and the used-event field the
# specification puts after them. Sized at four, the used ring begins inside the available one and each
# overwrites the other's last entries, which is a device and a driver disagreeing about one address.
mutate "$DRIVERS" src/user/drivers/core/src/virtio.rs \
	"	let used_off: u64 = align_up(avail_off + 6 + 2 * size as u64, 4);" \
	"	let used_off: u64 = align_up(avail_off + 4 + 2 * size as u64, 4);" \
	the_layout_is_the_one_setup_queue_used_to_compute_by_hand

# The class budget: the ceiling is the last admission and not the first refusal. One device past it
# is admitted with no error anywhere, and the DMA it was charged for is a page the controller was
# never asked for.
mutate "$DRIVERS" src/user/drivers/core/src/usb_class.rs \
	"		if devices > limits.devices {" \
	"		if devices > limits.devices + 1 {" \
	a_class_module_is_refused_at_its_own_ceiling_and_says_which

# The class budget: a release SATURATES, deliberately. Checked arithmetic there panics a driver
# during a teardown, which is the one moment it must not - the device is already gone and the panic
# takes the rest of the module's devices with it.
mutate "$DRIVERS" src/user/drivers/core/src/usb_class.rs \
	"		slot.devices = slot.devices.saturating_sub(1);" \
	"		slot.devices = slot.devices - 1;" \
	releasing_more_than_was_charged_stops_at_zero

# The class budget: each class module holds its OWN pool. Charged to another module's slot, a full
# disk budget refuses a keyboard and a second disk is admitted against the keyboard's ceiling - and
# the refusal names a module that has room.
mutate "$DRIVERS" src/user/drivers/core/src/usb_class.rs \
	"			ClassKind::Storage => &mut self.storage," \
	"			ClassKind::Storage => &mut self.hid," \
	two_class_modules_do_not_share_one_pool

# Audio: the used length is checked against the whole status structure before the status is believed.
# Refusing only a zero-length completion, a device that wrote four bytes has its status read out of
# the rest of the page - which on a recycled period is the previous one's, and it said OK.
mutate "$DRIVERS" src/user/drivers/core/src/snd.rs \
	"	if written < STATUS_BYTES {" \
	"	if written == 0 {" \
	a_short_status_write_is_refused_before_the_status_is_believed

# Audio capture: the device owes the PCM as well as the status. Bounded by the period alone, a
# capture that filled nothing but the status word is a success - and the bytes handed to the recorder
# are the previous period's, which is audio from a moment that has passed.
mutate "$DRIVERS" src/user/drivers/core/src/snd.rs \
	"	if written < period_bytes.saturating_add(STATUS_BYTES) {" \
	"	if written < period_bytes {" \
	a_capture_that_filled_less_than_a_period_is_refused

# Audio: a refusal is an EMPTY reply. Answered as "OK", a period the device rejected reaches
# AudioService as a period it played, and the stream's clock runs on sound nobody heard.
mutate "$DRIVERS" src/user/drivers/core/src/snd.rs \
	"		Err(_) => &[]," \
	"		Err(_) => b\"OK\"," \
	a_device_refusal_is_a_failure_and_not_a_success

# USB audio: a sample rate is TWENTY-FOUR bits, which is the field in this class most often read as
# thirty-two. Strided by four, the second rate of a two-rate device is read one byte late and the
# driver selects a frequency no device has.
mutate "$DRIVERS" src/user/drivers/core/src/uac.rs \
	"		let at = 8 + index * 3;" \
	"		let at = 8 + index * 4;" \
	a_sample_rate_is_twenty_four_bits_and_not_thirty_two

# USB audio: a rate count of zero means a RANGE and not an empty list. Read as a count, a device that
# offers 32 kHz to 96 kHz continuously offers nothing - and a driver that finds no rate binds no
# device, which reads as a device that does not do audio.
mutate "$DRIVERS" src/user/drivers/core/src/uac.rs \
	"	let entries = if kind == 0 { 2 } else { kind as usize };" \
	"	let entries = kind as usize;" \
	a_zero_rate_count_is_a_range_and_not_an_empty_list

# USB audio: the SUBFRAME size is part of the format and not a detail of it. Matched on the
# resolution alone, a device carrying sixteen bits in a four-byte subframe is handed half a period and
# told it is whole - so every period plays twice as fast as the one before it was written for.
mutate "$DRIVERS" src/user/drivers/core/src/uac.rs \
	" || self.subframe_bytes as u32 != bits as u32 / 8 {" \
	" {" \
	a_format_must_match_exactly_and_the_subframe_size_is_part_of_it

# USB audio: bits 1:0 of the endpoint's attributes are the transfer type and the bits above them are
# the synchronisation and usage types. Compared whole, an adaptive isochronous endpoint reads as a
# transfer type this driver does not speak - so the devices that set those bits bind nothing.
mutate "$DRIVERS" src/user/drivers/core/src/uac.rs \
	"	attributes & 0x03 == 0x01 && address & 0x80 == 0" \
	"	attributes == 0x01 && address & 0x80 == 0" \
	only_an_isochronous_out_endpoint_is_a_playback_pipe

# USB audio: a period is several isochronous packets and the LAST ONE IS SHORT. Divided down, the
# remainder is never sent: every period loses its tail, which is a click at the period rate rather
# than an error.
mutate "$DRIVERS" src/user/drivers/core/src/uac.rs \
	"	period_bytes.div_ceil(max_packet as u32) as usize" \
	"	(period_bytes / max_packet as u32) as usize" \
	a_period_is_several_isochronous_packets_and_the_last_one_is_short

# Console keys: each SIDE of a modifier is tracked separately. Folded onto one bit, releasing the
# right Shift cancels a left one that is still held - which drops the capital out of the middle of a
# word typed with both hands, and which nobody reproduces on purpose.
mutate "$DRIVERS" src/user/drivers/core/src/keys.rs \
	"			mods.side(HELD_RIGHT_SHIFT, value != 0);" \
	"			mods.side(HELD_LEFT_SHIFT, value != 0);" \
	releasing_one_shift_does_not_cancel_the_other

# Console keys: a lock toggles on the press and ignores the release, which is what makes it a lock
# rather than a modifier. Toggled on both, Caps Lock ends every press where it started and the key
# does nothing at all.
mutate "$DRIVERS" src/user/drivers/core/src/keys.rs \
	"		KEY_CAPSLOCK => {
			if value == 1 {" \
	"		KEY_CAPSLOCK => {
			if value != 0 {" \
	a_lock_key_toggles_once_per_press

# Console keys: an unplugged device releases what it HELD, and a lock is not held. Cleared with the
# modifiers, Caps Lock turns itself off whenever a keyboard is unplugged or a hub is reset - which is
# a person's setting being undone by somebody else's cable.
mutate "$DRIVERS" src/user/drivers/core/src/keys.rs \
	"		self.meta = false;
	}" \
	"		self.meta = false;
		self.caps = false;
	}" \
	releasing_everything_clears_every_side

# virtio-console: port `n` owns queues `2n + 2` and `2n + 3`, and the pair the offset skips is the
# CONTROL pair. Without it, the first generic port claims queues 2 and 3 - so the port's byte stream
# and the driver's own control messages share a queue and each reads the other's.
mutate "$DRIVERS" src/user/drivers/core/src/console.rs \
	"		let receive = u16::try_from(port * 2 + 2).ok()?;" \
	"		let receive = u16::try_from(port * 2).ok()?;" \
	a_port_owns_the_queue_pair_the_specification_gives_it

# virtio-console: `max_nr_ports` is a number the DEVICE writes into its own configuration space.
# Unclamped, a device announcing four thousand ports has the driver index a sixteen-entry array with
# whatever it announced.
mutate "$DRIVERS" src/user/drivers/core/src/console.rs \
	"announced: announced.min(MAX_PORTS) }" \
	"announced }" \
	a_device_cannot_choose_how_many_ports_this_driver_tracks

# virtio-console: a repeat is not a second event. Without the check, a host that sends PORT_ADD twice
# has the driver publish two providers for one port - and the catalogue then holds two names for one
# byte stream, of which one is dead.
mutate "$DRIVERS" src/user/drivers/core/src/console.rs \
	"			event::PORT_ADD => {
				if self.ports[index].added {
					return Action::Settled;
				}
" \
	"			event::PORT_ADD => {
" \
	the_same_message_twice_is_one_transition

# virtio-console: an event the specification does not define is REFUSED and not settled. Absorbed
# silently, a device speaking a later revision has its new events read as handled, and the driver
# reports a port state it never reached.
mutate "$DRIVERS" src/user/drivers/core/src/console.rs \
	"			_ => Action::Refused(Refusal::UnknownEvent)," \
	"			_ => Action::Settled," \
	a_truncated_message_is_refused_rather_than_read_past

# GPU damage: the union SATURATES rather than wrapping. The rectangles come off a service channel, so
# they are input - and a wrapped union is a rectangle whose corner is before its origin, which every
# consumer of it then computes from as though it were a shape.
mutate "$DRIVERS" src/user/drivers/core/src/gpu.rs \
	"	let x1 = a.0.saturating_add(a.2).max(b.0.saturating_add(b.2));" \
	"	let x1 = a.0.wrapping_add(a.2).max(b.0.wrapping_add(b.2));" \
	the_union_of_two_rectangles_saturates_rather_than_wrapping

# GPU: a backing is the product of three numbers and the extent is what bounds them. Without it, a
# device reporting a display larger than this driver will allocate for gets an allocation that
# succeeds at some smaller size - a backing shorter than the picture it is said to hold.
mutate "$DRIVERS" src/user/drivers/core/src/gpu.rs \
	"	(width <= MAX_EXTENT && height <= MAX_EXTENT).then_some(total)" \
	"	Some(total)" \
	a_backing_size_that_does_not_fit_is_refused

# GPU: a rectangle is CLIPPED to the display and not clamped to its size. Bounded by the extent
# instead of by what is left after the origin, a rectangle near the right edge keeps its full width
# and the transfer runs off the end of the row into the next one.
mutate "$DRIVERS" src/user/drivers/core/src/gpu.rs \
	"	let width = width.min(extent.0 - x);" \
	"	let width = width.min(extent.0);" \
	a_rectangle_outside_the_display_presents_nothing

# GPU: merging is a MEASUREMENT and not a habit. Unconditional, two opposite corners of a screen
# become one rectangle covering the screen - the whole framebuffer transferred for sixteen pixels,
# which is the cost `WSI Profile 1` names when it forbids the unconditional union.
mutate "$DRIVERS" src/user/drivers/core/src/gpu.rs \
	"			if area(merged) <= area(self.rects[index]).saturating_add(area(rect)) {" \
	"			if true {" \
	two_corners_are_two_transfers_and_not_the_screen_between_them

# GPU: a rectangle with no pixels is not damage. Refused only when BOTH extents are zero, a
# zero-width rectangle takes a slot in a set of sixteen and is transferred as a region - which costs
# a slot that real damage then has to merge its way into.
mutate "$DRIVERS" src/user/drivers/core/src/gpu.rs \
	"		if rect.2 == 0 || rect.3 == 0 {" \
	"		if rect.2 == 0 && rect.3 == 0 {" \
	the_set_is_bounded_and_ignores_what_is_not_damage

# GPU: the transfer offset is the last arithmetic before the device is told where to read. Unchecked,
# it wraps and names an address inside the resource that has nothing to do with the rectangle - which
# is a read the device performs and nothing refuses.
mutate "$DRIVERS" src/user/drivers/core/src/gpu.rs \
	"	row.checked_add(x as u64)?.checked_mul(BYTES_PER_PIXEL as u64)" \
	"	Some(row.wrapping_add(x as u64).wrapping_mul(BYTES_PER_PIXEL as u64))" \
	a_transfer_offset_that_does_not_fit_is_refused

# Network: the MTU sizes every buffer this driver allocates and it comes from the device's own
# configuration space. Refusing only a zero, a device claiming sixty-five thousand gets half a
# megabyte of pool allocated on its word, and one claiming four bytes gets a link that is not one.
mutate "$DRIVERS" src/user/drivers/core/src/net.rs \
	"		Some(mtu) if (MIN_MTU..=MAX_MTU).contains(&mtu) => mtu," \
	"		Some(mtu) if mtu > 0 => mtu," \
	the_link_mtu_is_bounded_rather_than_believed

# Network: a received length past the slot it claims to be in reads into the NEXT slot of the pool.
# The shared ring check bounds the descriptor; this is the arithmetic on top of it, and without it the
# frame handed up carries the tail of somebody else's packet.
mutate "$DRIVERS" src/user/drivers/core/src/net.rs \
	"	if len <= NET_HDR_LEN || len > slot_bytes {" \
	"	if len <= NET_HDR_LEN {" \
	a_used_element_that_does_not_describe_a_frame_is_refused

# Network: a transmitted frame sits BEHIND the virtio header, so the room it has is the slot less
# that header. Measured against the whole slot, a maximum-sized frame overruns the buffer by twelve
# bytes - into the next slot, which is a frame waiting to be sent.
mutate "$DRIVERS" src/user/drivers/core/src/net.rs \
	"	frame_len > 0 && (frame_len as u64) <= slot_bytes.saturating_sub(NET_HDR_LEN)" \
	"	frame_len > 0 && (frame_len as u64) <= slot_bytes" \
	a_transmitted_frame_has_to_fit_behind_its_header

# Ports: a port that is recorded and no longer connected is a DETACH. Settled instead, the device's
# queues and its publication stay up for ever - the catalogue keeps offering a disk that was
# unplugged, and the next device in that port is refused because its slot is taken.
mutate "$DRIVERS" src/user/drivers/core/src/port.rs \
	"		(false, true) => PortAction::Detach," \
	"		(false, true) => PortAction::Settled," \
	a_window_of_changes_produces_one_action_per_port

# Ports: taking the pending change LEAVES NONE. Read without clearing, every reconcile finds a change
# pending and walks every port's registers for ever - a driver that spins at the speed of its own
# loop, which reads as a busy bus.
mutate "$DRIVERS" src/user/drivers/core/src/port.rs \
	"		core::mem::take(&mut self.pending)" \
	"		self.pending" \
	a_change_seen_by_a_wait_survives_until_the_loop_reads_it

# Block: a zero-sector request is refused and not admitted as a no-op. Admitted, it reaches the
# transport as a command with no data stage, which several devices answer with a stall - and the
# driver then resets a link that was healthy.
mutate "$DRIVERS" src/user/drivers/core/src/blk.rs \
	"	if count == 0 || count as u64 > max_sectors {" \
	"	if count as u64 > max_sectors {" \
	a_zero_count_and_a_count_above_the_request_limit_are_refused_not_clamped

# Block: the range is checked at its END. Checked at its start, a request beginning on the last
# sector and running past it is admitted, and the sectors past the medium are whatever the device
# answers for them - which on most devices is the first sectors of the medium.
mutate "$DRIVERS" src/user/drivers/core/src/blk.rs \
	"	if end > capacity_sectors {" \
	"	if lba > capacity_sectors {" \
	a_request_past_the_last_sector_is_refused_by_range

# Block: a write's source has to be READABLE through the handle it arrived on. Checked for any right
# at all, a handle that maps but cannot be read is admitted and the copy reads a page this process was
# never granted.
mutate "$DRIVERS" src/user/drivers/core/src/blk.rs \
	"	if info.rights & RIGHT_READ == 0 {" \
	"	if info.rights == 0 {" \
	a_write_source_must_be_a_readable_memory_object_at_least_as_long_as_the_request

# Block: a ten-byte command's block address ENDS at 2^32, so the last block it can name is 2^32 - 1
# and the bound is exclusive. Off by one, the last block of a two-terabyte medium is refused as
# unaddressable - a device that works everywhere except at its own end.
mutate "$DRIVERS" src/user/drivers/core/src/blk.rs \
	"	if end > u32::MAX as u64 + 1 {" \
	"	if end > u32::MAX as u64 {" \
	an_address_that_does_not_fit_the_command_is_refused_rather_than_truncated

# Input: the absolute maximum is the SECOND of five unsigned words, so a block under eight bytes does
# not contain it. Read out of a shorter one, the pointer's whole coordinate system is whatever the
# configuration window held - and the clamp that follows is against that number.
mutate "$DRIVERS" src/user/drivers/core/src/input.rs \
	"	if size < 8 {" \
	"	if size < 4 {" \
	an_absolute_axis_range_is_believed_only_when_it_is_one

# Input: an absolute axis SETS and a relative one nudges. Folded as a relative move, a tablet's
# reported position is added to where the pointer already was, so the pointer walks to the corner and
# stays there while the device reports positions all over its surface.
mutate "$DRIVERS" src/user/drivers/core/src/input.rs \
	"			AXIS_X => state.x = value.clamp(0, max_x)," \
	"			AXIS_X => state.x = state.x.saturating_add(value).clamp(0, max_x)," \
	absolute_and_relative_motion_fold_the_way_each_is_defined

# Input: the wheel accumulates and SATURATES. A device reporting two billion ticks in one group wraps
# it into a scroll the other way, which is a document that jumps to its end when it was asked to go
# further down.
mutate "$DRIVERS" src/user/drivers/core/src/input.rs \
	"REL_WHEEL => *wheel = wheel.saturating_add(value)," \
	"REL_WHEEL => *wheel = wheel.wrapping_add(value)," \
	buttons_are_bits_and_the_wheel_is_a_delta

# Input: a range that is not a range has to ANSWER rather than divide. Refusing only a zero, a
# negative maximum reaches a clamp whose lower bound is above its upper one and a division by a number
# read as four billion - so the axis the device got wrong takes the driver with it.
mutate "$DRIVERS" src/user/drivers/core/src/input.rs \
	"	if max <= 0 {" \
	"	if max == 0 {" \
	a_position_normalises_onto_the_range_a_consumer_receives

echo "driver-mutations: $caught of $planted planted defect(s) were caught by the test named for each"
[[ "$planted" -eq "$caught" ]] || fail "a planted defect went uncaught"
