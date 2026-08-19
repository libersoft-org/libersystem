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
	# ORDINARY SUITES RUN ORDINARILY. This was `-- --include-ignored` for EVERY discovered crate, so
	# every `#[ignore]` in the tree became a mandatory part of the gate - and there is already one
	# that must not be: `bench_scaling` in `fs/liberfs` is a benchmark, deliberately ignored, and the
	# gate ran it on every invocation. Every future long, manual or experimental `#[ignore]` would
	# have joined it without anybody deciding to, which is the opposite of what `#[ignore]` is for.
	#
	# The two tests that genuinely need it get a targeted run below, named one by one.
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

# THE ARTIFACT-DEPENDENT TESTS, BY NAME.
#
# `#[ignore]` is how a test says "not under a bare `cargo test`" - these two need a wasm32 artifact
# this tree builds elsewhere, and running them is right for the gate, which is the place that
# artifact exists. What was wrong was applying that decision to every `#[ignore]` in the tree by
# accident rather than to these two on purpose.
#
# NAMED RATHER THAN DISCOVERED, deliberately. A pattern like "every ignored test in `src/wasm`"
# brings the next one along silently, which is the defect being fixed. Adding a third means adding
# a line here, which is somebody deciding.
#
# `--exact` so a name is a name and not a prefix; `--ignored` rather than `--include-ignored` so
# this run is exactly these tests and a typo is a run of nothing rather than a silent re-run of the
# whole crate.
# The `dev-diagnostics` half is a feature of the SDK ARTIFACT, not of this crate - `./build.sh --part sdk` builds
# both wasm binaries and the test picks the one it needs - so both entries run the `wasm` crate as
# the model enumerates it, with no `--features` of their own.
artifact_tests=(
	"src/wasm	world::tests::the_sdks_own_panic_handler_reaches_the_host_as_a_trap_with_its_line_logged"
	"src/wasm	world::tests::with_dev_diagnostics_the_real_guests_panic_reaches_the_log_it_was_granted"
)
for entry in "${artifact_tests[@]}"; do
	IFS=$'\t' read -r dir test <<<"$entry"
	args=(--quiet --manifest-path "../$dir/Cargo.toml")
	# `--exact` names one test, and a run that matched none is a silent pass - so the count is
	# checked. This is the same floor the inventory above has, for the same reason.
	output="$(cargo test "${args[@]}" -- --ignored --exact "$test" 2>&1)" || {
		echo "$output" >&2
		echo "host-tests: artifact test $test FAILED" >&2
		failed+=("$test")
		status=1
		continue
	}
	if ! grep -qE '1 passed' <<<"$output"; then
		echo "$output" >&2
		echo "host-tests: artifact test $test matched no test - the name moved and this gate would have passed over it" >&2
		failed+=("$test")
		status=1
	fi
done

if ((status != 0)); then
	echo "host-tests: ${#failed[@]} of $count failed: ${failed[*]}" >&2
fi
exit "$status"
