#!/usr/bin/env bash
# A typed dispatch may not be reached through the single-capability receive.
#
# The kernel has always let a message carry four capabilities and has the syscalls for it;
# `SYS_CHANNEL_RECV` takes the first and DROPS THE REST, which the kernel's own comment says. For a
# bootstrap handshake or a stream frame that is right, because those carry at most one by
# construction. For a typed server it is silent destruction: a client sending stdin, stdout and
# stderr had two of its capabilities closed before the dispatch was reached.
#
# That is what `rt::recv_caps_blocking` and `rt::try_recv_caps` exist for, and the migration is
# finished - but the enumeration was the cheap part. Thirteen sites did their own receive because
# each was written by hand, and nothing stops a fourteenth. This is that something.
#
# Banned: building a capability list from ONE received handle. `Handles::from_slice(&[handle])` was
# the exact shape at every one of the thirteen, and `from_slice` itself no longer exists - so what
# this catches is the pattern coming back under the fallible name.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# The runtime primitives themselves, and the crate that defines the type, are where the single
# handle is legitimately turned into a list.
allowed_paths=(
	"src/wire/src/lib.rs"
	"src/user/runtime/rt/src/lib.rs"
)

failed=0
while IFS= read -r hit; do
	file="${hit%%:*}"
	rel="${file#"$root"/}"
	rel="src/${rel}"
	skip=0
	for allowed in "${allowed_paths[@]}"; do
		[[ "$rel" == "$allowed" ]] && skip=1
	done
	((skip)) && continue
	echo "check-single-cap-receive: $hit" >&2
	failed=1
	# Comment lines are excluded: the sites this replaced carry a note naming the old pattern, and a
	# gate that cannot tell an explanation from an occurrence forbids writing down what was fixed.
done < <(grep -rn --include='*.rs' -E '^[^:]+:[0-9]+:[[:space:]]*[^/[:space:]].*Handles::(try_)?from_slice\(&\[[a-z_]+\]\)' "$root" 2>/dev/null || true)

if ((failed)); then
	cat >&2 <<'MSG'

A typed dispatch is being handed a capability list built from ONE received handle. The single-handle
receive keeps the first capability and drops the rest, so this destroys whatever the client sent
past it - not refuses, destroys.

Use `rt::recv_caps_blocking` (blocking) or `rt::try_recv_caps` (polling), which take the whole list.
MSG
	exit 1
fi

echo "single-cap receive: no typed dispatch builds its handle list from one received handle"
