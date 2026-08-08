#!/usr/bin/env bash
# Run the host test suite of every crate that has one.
#
# It used to run nine, from a list kept by hand, while fifty-eight crates had suites - FAT, ISO9660,
# UDF, LiberMemFS, every image codec, MP3, Vorbis, Ogg, both compression leaves, `abi`, `proto`,
# `term`. Those suites are milliseconds each and pin behaviour far more finely than a boot can, and
# nothing ran them. That is a suite in name only, and a hand-written inventory is how it stayed that
# way: there was no moment at which anybody was told the list had fallen behind.
#
# So the list is DERIVED now. `verify-model host-suites` scans the tree for crates containing a
# `#[test]` and prints one line per (crate, configuration) the catalog says is runnable. Adding a
# crate with tests adds a line here on the next run, with nobody remembering to do anything.
#
# The in-kernel suite (test.sh) is the other half and not a substitute: it answers what the SYSTEM
# does with these crates, and this answers what the crates do.
set -euo pipefail

cd "$(dirname "$0")/.."

# The floor. A scan that silently stops discovering crates returns success with an empty list, which
# is indistinguishable from a tree that has no tests - and it is the exact shape of the failure this
# script was rewritten to prevent. Measured 2026-08-08: 58 crates, 58 runnable (crate, configuration)
# pairs. Raise it when the number grows; a drop is a defect until proven otherwise.
MINIMUM_SUITES=55

# Prove the floor REFUSES before trusting it to approve.
#
# The floor below is the whole defence against a scanner that quietly stops discovering crates: a
# shrunken inventory returns success over the handful it still finds, and looks exactly like a tree
# with fewer tests. So the gate first hands itself an enumeration that is too short and requires
# itself to reject it. A validator tested only over a currently-valid tree is not tested.
if [[ "${HOST_TESTS_SELF_TEST:-}" != "1" ]]; then
	if HOST_TESTS_SELF_TEST=1 HOST_TESTS_ENUMERATOR="printf 'src/abi\tabi\tdefault\n'" "$0" >/dev/null 2>&1; then
		echo "host-tests: SELF-TEST FAILED - a one-crate inventory was accepted, so the floor is not guarding anything" >&2
		exit 1
	fi
fi

enumerate="${HOST_TESTS_ENUMERATOR:-cargo run --quiet --manifest-path tools/verify-model/Cargo.toml -- host-suites}"
suites="$(eval "$enumerate")" || {
	echo "host-tests: the model could not enumerate the suites; refusing to report a pass over an unknown list" >&2
	exit 1
}

count="$(printf '%s\n' "$suites" | grep -c . || true)"
if ((count < MINIMUM_SUITES)); then
	echo "host-tests: found only $count suite(s), expected at least $MINIMUM_SUITES" >&2
	echo "host-tests: a shrinking inventory is a broken scanner until proven otherwise - check verify-model's discovery before lowering this floor" >&2
	exit 1
fi

echo "host-tests: $count suite(s)"
status=0
failed=()
while IFS=$'\t' read -r dir crate configuration; do
	[[ -n "$dir" ]] || continue
	# Run from `src/` rather than from inside the crate. `src/user/.cargo/config.toml` names a
	# bare-metal target and builds `core` from source, which is right for the volume and fatal for a
	# host test: the harness needs `std` and `test`, and a second `core` collides with the one `std`
	# already carries. Cargo takes its config from the working directory, so staying out of that
	# subtree is what selects the host.
	args=(--quiet --manifest-path "../$dir/Cargo.toml")
	if [[ "$configuration" != default ]]; then
		args+=(--no-default-features --features "$configuration")
	fi
	if ! cargo test "${args[@]}"; then
		echo "host-tests: $crate ($configuration) FAILED" >&2
		failed+=("$crate/$configuration")
		status=1
	fi
done <<<"$suites"

if ((status != 0)); then
	echo "host-tests: ${#failed[@]} of $count failed: ${failed[*]}" >&2
fi
exit "$status"
