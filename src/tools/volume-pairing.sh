#!/usr/bin/env bash
# Whether a boot medium and a system volume were built for each other.
#
# ONE ANSWER, ASKED WHEREVER A PAIRING IS USED. The check used to exist only in `mkimage.sh`, where
# the pairing is WRITTEN - and it passed on every medium in a round where the tree was broken,
# because the mismatch was created after the ISO had already been assembled. A pairing is a claim
# about two artifacts, so whatever attaches a volume to a medium can ask it, and both callers ask the
# same code rather than each carrying its own arithmetic.
#
# THE UUID IS 16 RAW BYTES AT OFFSET 80 of the LiberFS superblock, which is block 0 of the image.
# That offset is the only thing here that knows the format, and it is written once.

# The uuid of the volume image at $1, lowercase hex, no separators.
volume_superblock_uuid() {
	dd if="$1" bs=1 skip=80 count=16 status=none | od -An -tx1 | tr -d ' \n'
}

# Whether the signed manifest at $1 names the volume at $2.
#
# ASKED OF THE MANIFEST'S BYTES rather than by parsing to the field. The header carries a
# variable-length product string and a variable-length release string before the uuid, so locating it
# means implementing the format twice; a sixteen-byte value does not appear in a signed manifest by
# accident. Callers that need the value itself parse it properly - this answers a yes-or-no question.
manifest_names_volume() {
	local manifest="$1" volume="$2" want
	want="$(volume_superblock_uuid "$volume")"
	od -An -tx1 -v "$manifest" | tr -d ' \n' | grep -q "$want"
}

# Whether the pairing value $1 (hex, any case, separators allowed) names the volume at $2.
#
# A pairing naming a volume that is not on THIS medium is legitimate - that is the multi-disk case
# the mechanism is for, an ESP naming a volume on another disk - so a caller with no volume to
# compare against has nothing to check and nothing wrong. Said here rather than at each call site.
pairing_matches_volume() {
	local declared="$1" volume="$2" actual
	[[ -f "$volume" ]] || return 0
	declared="$(tr -d '[:space:]-' <<<"$declared" | tr 'A-F' 'a-f')"
	actual="$(volume_superblock_uuid "$volume")"
	[[ "$declared" == "$actual" ]]
}
