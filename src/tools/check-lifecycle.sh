#!/bin/bash
# STATIC INITIALISATION IS A MECHANISM THIS SYSTEM ADMITS, so it gets positive gates.
#
# WHY IT IS ADMITTED AND NOT FORBIDDEN. The converged closure of the pinned foreign configuration
# names exactly one constructor and one destructor, on all three targets. That is a MEASUREMENT, and
# under this milestone's own rule a measured `.init_array` makes the lifecycle gates mandatory - and
# makes the runner that walks it a deliverable rather than an assumption, because until it was built
# `liber_rt_start` performed the ABI check and called the entry point directly.
#
# WHAT IS OBSERVED, AND WHY EACH NEEDS A RUNNING PROCESS:
#
#   ORDER ACROSS THE DAG    a provider's constructor must run before its consumer's, which is the
#                             only order in which a constructor can rely on what it was built
#                             against. The guest RECORDS the sequence rather than asserting it ran:
#                             a constructor runs before the console is adopted and cannot speak, so
#                             it writes, and the program prints what it finds.
#   PARTIAL INITIALISATION  what a constructor that did not finish leaves behind - which is a
#                             different question from whether the completing one completed, and a
#                             single flag would make "never started" look like "stopped half way".
#   DESTRUCTORS ON EXIT     in reverse of the construction order, across the DAG: the consumer's
#                             first, then the provider's.
#   THE SAME PROCESS CRASHING   and which of those did NOT run. The difference between the two paths
#                             is the whole of what a lifecycle contract says.
#   `errno` PROCESS-WIDE    two images ask for it and get one address, which is what the
#                             no-thread-creation pin makes correct.

set -euo pipefail
GUEST_GATE_NAME="lifecycle"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"

guest_gate_arch "$@"

fail() { guest_gate_fail "$@"; }

provider="$root/../.build/image/$(guest_gate_triple)/lib/foreign/lifecycle.lslib"
consumer="$root/../.build/image/$(guest_gate_triple)/libexec/lifecheck"
[[ -f "$provider" && -f "$consumer" ]] || fail "the lifecycle fixture is not staged - build the image first"

# BOTH IMAGES ACTUALLY CARRY THE ARRAYS. Everything below observes what RAN; this observes that there
# was something to run, so a guest log full of the right lines cannot come from a fixture that lost
# its constructors to a linker that dropped them.
for artifact in "$provider" "$consumer"; do
	sections="$(llvm-readelf -S --wide "$artifact")"
	for section in .init_array .fini_array; do
		# CAPTURED, THEN MATCHED: `grep -q` stops at the first match, the writer takes SIGPIPE, and
		# under `pipefail` a section that IS there reads as a failed pipeline.
		grep -qF " $section " <<<"$sections" || fail "$(basename "$artifact") carries no $section - there is nothing for the runner to run"
	done
done

# THREE RUNS IN ONE BOOT: a normal exit, the same program crashing, and a normal exit again. The
# difference between the first and the second is what a lifecycle contract says.
guest_gate_run 'lifecheck
lifecheck crash
lifecheck' lifecheck
lines="$GUEST_LINES"

expect() {
	grep -qxF "lifecheck: $1" "$lines" || {
		echo "lifecycle: expected \"lifecheck: $1\" - $2" >&2
		cat "$lines" >&2
		exit 1
	}
	echo "lifecycle: $1"
}

expect "sequence=PQC" "the provider's two constructors must run, in priority order, before the consumer's"
expect "provider initialisation complete" "the completing constructor must reach its own completion step"
expect "partial initialisation left behind" "a constructor that stopped half way must leave a marker that says so"
expect "errno is process-wide" "two images must get one errno slot"

# THE ORDER OF THE DESTRUCTORS, READ AS AN ORDER. Both lines existing says they ran; only their
# POSITIONS say they ran in reverse of the construction order.
# `sed` RATHER THAN `grep | head`. Under `pipefail` a reader that stops early makes the writer take
# SIGPIPE and the pipeline fail, which here would turn a FOUND line into a missing one; `sed` quits
# after the first match on its own.
consumer_line="$(sed -n '/lifecheck: consumer destructor/{=;q;}' "$lines")"
provider_line="$(sed -n '/lifecheck: provider destructor/{=;q;}' "$lines")"
[[ -n "$consumer_line" && -n "$provider_line" ]] || fail "a normal exit must run both destructors"
((consumer_line < provider_line)) || fail "the consumer's destructor must run before its provider's - destruction is construction in reverse"
echo "lifecycle: destructors run in reverse of the construction order, across the provider DAG"

# AND THE CRASH, WHICH IS THE SAME PROGRAM. What makes this an observation rather than an assumption
# is that the crashing run printed everything up to the fault and nothing after it: the run that
# crashed contributed a "crashing" line and no destructor line of its own.
crashes="$(grep -c 'lifecheck: crashing' "$lines" || true)"
[[ "$crashes" == 1 ]] || fail "the crashing run did not reach its fault"
exits="$(grep -c 'lifecheck: exiting' "$lines" || true)"
consumer_destructors="$(grep -c 'lifecheck: consumer destructor' "$lines" || true)"
[[ "$exits" == "$consumer_destructors" ]] || fail "there are $exits normal exits and $consumer_destructors destructor runs - a crash ran one, or an exit did not"
echo "lifecycle: a crash runs no destructor, and every normal exit runs exactly one"
