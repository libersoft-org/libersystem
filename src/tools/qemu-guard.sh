# Make a gate's guest die when the gate does.
#
# WHAT THIS IS FOR. `timeout 300 ./run.sh --arch aarch64` ends the SCRIPT and not the
# `qemu-system-aarch64` underneath it, so an interrupted gate leaves a guest holding a write lock on
# the disk images under `.build/boot`. The next run then fails with a QEMU lock error naming an IMAGE
# rather than the run that holds it - a diagnosis `test.sh` already warns is unreadable when it
# happens for other reasons. Observed twice in one session: once from `check-signed-boot.sh`'s
# device-tree phase, once from `check-secure-boot.sh` surviving its parent, which left this:
#
#     1531063  bash tools/check-secure-boot.sh
#     1531082  timeout 120 qemu-system-x86_64      <- group leader, in a group of its own
#     1531083  qemu-system-x86_64
#
# `timeout` HANDLES ITS OWN EXPIRY CORRECTLY - measured, not assumed: it puts its child in a new
# process group and signals the GROUP, so a grandchild dies with it. The hole is only the other
# ending, where the gate itself is killed and nothing ever signals that group.
#
# WHY THIS IS A SWEEP AND NOT A LEDGER. The first version of this file recorded each guest's pid and
# process group as it was started and signalled those groups on the way out. That needed a rule for
# a guest started WITHOUT `timeout`, which shares the gate's own group - and signalling that group is
# how a cleanup kills the script running it, and through the group whatever else is in it. It also
# needed every guest to be started through a wrapper, which is a change at every call site and a
# thing to forget at the next one. Asking the question at EXIT instead removes all of it: whatever is
# still a descendant of a script that has finished is, by definition, something that outlived it.
#
# The walk is the one `test-kernel.sh` already uses to decide a guest is ITS OWN rather than any QEMU
# on the machine. Ancestry is what makes the victim ours; a process name is not.

# Every descendant of a pid, deepest first, so a parent is never signalled before its children.
_guest_descendants() {
	local parent="$1" child
	for child in $(ps -o pid= --ppid "$parent" 2>/dev/null); do
		_guest_descendants "$child"
		printf '%s\n' "$child"
	done
}

# End what this script started, and nothing else.
#
# A caller wires this into the trap it already has, rather than this file installing one: every gate
# that starts a guest already carries `trap 'rm -rf "$work"' EXIT`, and a second `trap ... EXIT` does
# not compose with the first, it REPLACES it. Sourcing this file would have silently taken the
# temp-directory cleanup out of five gates - a leak nobody would look for and which this file would
# appear to have nothing to do with. So:
#
#     trap 'guest_cleanup; rm -rf "$work"' EXIT INT TERM
#
# Guest first: the temp directory it is reading from should outlive it by the length of one signal.
guest_cleanup() {
	local victim
	for victim in $(_guest_descendants $$); do
		kill -TERM "$victim" 2>/dev/null || true
	done
}
