#!/usr/bin/env bash
# THE RELEASE SNAPSHOT: a detached worktree of one revision, sealed read-only for the run.
#
# `verify.sh --release` runs from one immutable revision and the evidence gate proves the seal; both
# call this so there is one definition of what "immutable for the run" means:
#
#   create DIR REVISION   a detached worktree of REVISION at DIR (DIR is replaced)
#   seal DIR              every file and directory under DIR sealed, except `.build` - the one
#                         subtree the producers write - and the worktree's `.git` link
#   unseal FILE           one file made writable again, for a producer whose declared work is an
#                         edit it undoes itself (the performance gate's probe)
#   reseal FILE           that file sealed again
#   remove DIR            the seal lifted (git needs to delete), then the worktree removed
#
# BEFORE/AFTER IS NOT IMMUTABILITY. A worktree left writable can be edited, consumed by a producer
# and restored before a post-check, and a Git tree identity does not move for an ordinary worktree
# edit at all. The seal is what makes a write fail at the moment it is attempted; the identity
# comparison the dossier makes afterwards is the second signal.
#
# WHAT THE SEAL IS. `chmod a-w` stops nobody who is root, and the runs here are root's. So the seal
# is the filesystem's IMMUTABLE attribute (`chattr +i`) on every file and directory, which refuses a
# write, a rename, a new entry and an unlink to root as well - and `chmod a-w` beside it, which is
# the whole of the seal where the attribute is not available (a filesystem without it, or a caller
# without `CAP_LINUX_IMMUTABLE`). `seal` says which of the two it applied.
set -euo pipefail

usage() {
	echo "usage: release-snapshot.sh create DIR REVISION | seal DIR | unseal FILE | reseal FILE | remove DIR" >&2
	exit 2
}

# The attribute is tried on one file first: a filesystem that refuses it refuses it everywhere.
immutable_supported() {
	local probe="$1"
	chattr +i "$probe" 2>/dev/null && chattr -i "$probe" 2>/dev/null
}

[[ $# -ge 2 ]] || usage
verb="$1"
dir="$2"
case "$verb" in
create)
	[[ $# -eq 3 ]] || usage
	revision="$3"
	if [[ -e "$dir" ]]; then
		chmod -R u+w "$dir" 2>/dev/null || true
		git worktree remove --force "$dir" >/dev/null 2>&1 || rm -rf "$dir"
	fi
	git worktree prune >/dev/null 2>&1 || true
	git worktree add --detach "$dir" "$revision" >/dev/null 2>&1 || {
		echo "release-snapshot: could not create a worktree of $revision at $dir" >&2
		exit 1
	}
	echo "$dir"
	;;
seal)
	[[ -d "$dir" ]] || {
		echo "release-snapshot: no snapshot at $dir" >&2
		exit 1
	}
	mkdir -p "$dir/.build"
	find "$dir" -path "$dir/.build" -prune -o -name .git -prune -o -print0 | xargs -0r chmod a-w
	if immutable_supported "$dir/README.md"; then
		find "$dir" -path "$dir/.build" -prune -o -name .git -prune -o -print0 | xargs -0r chattr +i
		echo "release-snapshot: sealed $dir (immutable attribute and read-only mode, except .build)"
	else
		echo "release-snapshot: sealed $dir (read-only mode only - the immutable attribute is not available here, and root is not held by a mode)"
	fi
	;;
unseal)
	[[ -e "$dir" ]] || {
		echo "release-snapshot: no file at $dir" >&2
		exit 1
	}
	chattr -i "$dir" 2>/dev/null || true
	chmod u+w "$dir"
	;;
reseal)
	[[ -e "$dir" ]] || {
		echo "release-snapshot: no file at $dir" >&2
		exit 1
	}
	chmod a-w "$dir"
	chattr +i "$dir" 2>/dev/null || true
	;;
remove)
	[[ -e "$dir" ]] || exit 0
	find "$dir" -path "$dir/.build" -prune -o -print0 2>/dev/null | xargs -0r chattr -i 2>/dev/null || true
	chmod -R u+w "$dir" 2>/dev/null || true
	git worktree remove --force "$dir" >/dev/null 2>&1 || rm -rf "$dir"
	git worktree prune >/dev/null 2>&1 || true
	;;
*) usage ;;
esac
