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
	"	let (notify_in, notify_packet) = notify.unwrap_or((0, 0));" \
	"	let (notify_in, notify_packet) = notify.unwrap_or((0, 0));
	let data_interface = union_data + 1;" \
	the_union_names_the_data_interface_and_the_next_one_is_not_it

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

echo "driver-mutations: $caught of $planted planted defect(s) were caught by the test named for each"
[[ "$planted" -eq "$caught" ]] || fail "a planted defect went uncaught"
