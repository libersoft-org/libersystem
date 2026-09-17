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
mutate "$DRIVERS" src/user/drivers/core/src/ahci.rs \
	"	if tfd & TFD_ERR != 0 {" \
	"	if false {" \
	a_cleared_slot_is_not_the_same_as_a_successful_command

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

# UAS: the tag is big-endian in a transport that is little-endian everywhere else. Swapped, it
# matches no answer - and an answer matching nothing is indistinguishable from a device fault.
mutate "$DRIVERS" src/user/drivers/core/src/uas.rs \
	"	iu[2] = (tag >> 8) as u8;
	iu[3] = tag as u8;" \
	"	iu[2] = tag as u8;
	iu[3] = (tag >> 8) as u8;" \
	the_tag_is_big_endian_in_a_transport_that_is_little_endian_everywhere_else

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
	"			if best.is_none_or(|(_, have, _, _, _, _)| alt < have) {" \
	"			if best.is_none() {" \
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
