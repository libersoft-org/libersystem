#!/usr/bin/env bash
# The kernel's own trace, replayed against the model that describes it.
#
# WHAT THIS GATE IS. `docs/spec/capability/Transfer.tla` says which actions are enabled from which
# states, and `check-capability-model.sh` explores it exhaustively. That is a statement about the
# MODEL. This is the other half: the running kernel emits what it actually did in the same
# vocabulary, and `trace-check` replays it, asking of every step whether the model allowed it.
#
# WHAT IT IS NOT, and the milestone says so in as many words: this is SAMPLED trace refinement over a
# selected boundary. It shows the steps this run took are model steps. It does not show that every
# model trace is a Rust execution.
#
# THE REFERENCE, AND WHY IT IS COMMITTED. The gate must run without QEMU - `check.sh` is not the test
# suite - so the trace the conformance fixture emits is kept under `docs/spec/capability/trace/` in
# NORMALIZED form: message and channel identities renumbered by first appearance, since the raw ones
# come from counters that have been running since boot. Where a suite HAS been run, this gate holds
# the live trace against that reference and fails on a difference, so a fixture that changes cannot
# leave a stale reference passing quietly.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."
REFERENCE="docs/spec/capability/trace/reference.trace"
CHECKER=".build/cargo/shared/debug/trace-check"

fail() {
	echo "capability-trace: $*" >&2
	exit 1
}

[[ -f "$REFERENCE" ]] || fail "$REFERENCE is not here"

(cd src/tools/trace-check && cargo build --quiet --offline) || fail "the checker does not build"
[[ -x "$CHECKER" ]] || fail "$CHECKER was not produced by the build"

# 1. THE CHECKER MUST BE ABLE TO REFUSE. A checker that accepts everything passes every run it will
#    ever see, so it is asked to refuse a suite of deliberate defects before it is trusted with one.
"$CHECKER" --self-test "$REFERENCE" || fail "the checker failed its own mutation suite"

# 2. AND THE REFERENCE MUST STILL BE WHAT THE KERNEL EMITS. Only where a suite has actually run: a
#    tree with no guest log is not evidence of a match, and it is not evidence of a mismatch either.
# The newest guest log, WITHOUT a reader that stops early: the names carry a sortable timestamp, so
# the last of a sorted glob is the newest one and nothing has to be interrupted to find it.
shopt -s nullglob
logs=(.build/logs/test/x86_64-*-guest.log)
shopt -u nullglob
latest=""
if ((${#logs[@]})); then
	readarray -t logs < <(printf '%s\n' "${logs[@]}" | sort)
	latest="${logs[-1]}"
fi
if [[ -z "$latest" ]]; then
	echo "capability-trace: no x86_64 guest log in this tree - the reference was not held against a live run"
	echo "capability-trace: the checker refuses its deliberate defects and the reference replays"
	exit 0
fi
if ! grep -q "captrace: begin" "$latest"; then
	echo "capability-trace: the newest x86_64 guest log has no trace in it (the suite ran without the object tags)"
	echo "capability-trace: the checker refuses its deliberate defects and the reference replays"
	exit 0
fi

live="$(mktemp)"
trap 'rm -f "$live"' EXIT
"$CHECKER" --normalize "$latest" >"$live" || fail "the live trace could not be read out of $latest"
if ! diff -u "$REFERENCE" "$live"; then
	fail "the live trace differs from $REFERENCE
    The fixture changed and the reference did not. Refresh it with:
      $CHECKER --normalize $latest > $REFERENCE
    and read the diff first - a trace that changed shape is a change in what the kernel does."
fi

# 3. And the live one replays, covers and all.
"$CHECKER" "$latest" || fail "the live trace is not a model behaviour"
echo "capability-trace: the live trace matches the reference and replays against the model"
