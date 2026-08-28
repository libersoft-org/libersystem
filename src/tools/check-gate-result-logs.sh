#!/usr/bin/env bash
# A GATE READS THE RUN IT MADE, checked against the source rather than remembered.
#
# Every gate that boots a guest used to find its evidence by globbing
# `.build/logs/test/<arch>-*-guest.log` and taking the newest. That is a correct-looking read of
# ANOTHER guest's result the moment two runs of one architecture overlap - a green about somebody
# else's boot - and it is wrong on one target of three even when nothing overlaps, because on riscv64
# the suite's output lands in the RUN log while the guest log holds only U-Boot and the loader.
#
# `test-kernel.sh` prints `RESULT-LOGS <run> <guest>` and `result-logs.sh` reads it. This refuses the
# glob coming back: a gate that starts a guest takes the paths that guest named.
#
# WHAT IT DOES NOT COVER, and says so rather than implying otherwise: a gate that reads existing
# evidence it did not produce - `capability-trace` looks for the newest trace in the tree on purpose,
# because its question is "is there a trace newer than this kernel" rather than "what did my run
# write". It starts no guest and is not in scope here.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
failed=0

# EVERY PRODUCER THAT BOOTS A GUEST, not only the ones whose filename starts with `check-`.
#
# This scanned `check-*.sh` alone, and `verify.sh`'s own shadow path started a guest and then took the
# newest `<arch>-*-guest.log` out of the shared directory - the exact pattern refused below, in the
# script that RUNS the gates, passed over because of how it is named. A rule about how a run finds its
# evidence has nothing to do with the producer's filename.
for gate in "$root"/check-*.sh "$root/../../verify.sh"; do
	[[ -f "$gate" ]] || continue
	name="${gate##*/}"
	# Code only: a `#` line is prose, and several of these files explain the defect in their headers.
	code="$(grep -vE '^[[:space:]]*#' "$gate")"
	# This file names the pattern it refuses, so it would refuse itself.
	[[ "$name" == "check-gate-result-logs.sh" ]] && continue
	grep -q './test.sh --arch' <<<"$code" || continue
	# THE RULE IS ABOUT REACHING INTO THE SHARED LOG DIRECTORY, not about which helper is used. A
	# gate that captures `test.sh`'s own output and greps that is already reading its own run and
	# needs nothing from here; `implementation-mutations` does exactly that. What is refused is
	# selecting a file out of `.build/logs/test/` by pattern, because the pattern cannot tell one
	# run's guest from another's.
	# EITHER SPELLING OF THE DIRECTORY, AND A GLOB ANYWHERE ON THE LINE.
	#
	# Two things were wrong with the old pattern. It matched the literal `.build/logs/test` only, and
	# `verify.sh` writes `$BUILD_DIR/logs/test` - so the one producer this rule had not been applied to
	# was also the one it could not have seen. And it required the `*` to follow the directory inside
	# ONE quoted string (`[^"]*\*`), which `find "$BUILD_DIR/logs/test" -name "<arch>-*-guest.log"`
	# is not: the glob is in the next word. Both spellings, and the glob is looked for on the line
	# rather than at a fixed distance from the directory.
	if grep -E '(\.build|\$\{?BUILD_DIR\}?)/logs/test' <<<"$code" | grep -q '\*'; then
		echo "gate-result-logs: $name starts a guest and then globs .build/logs/test - that reads whichever run finished last, which is not necessarily its own" >&2
		failed=1
		continue
	fi
	if grep -qE '(\.build|\$BUILD_DIR|\$\{BUILD_DIR\})/logs/test' <<<"$code" && ! grep -q 'result_logs' <<<"$code"; then
		echo "gate-result-logs: $name starts a guest and reaches into .build/logs/test without asking which files its run wrote - source result-logs.sh" >&2
		failed=1
		continue
	fi
	echo "gate-result-logs:     $name reads the run it made"
done

((failed == 0)) || exit 1
echo "gate-result-logs: every gate that boots a guest reads the logs that guest named"
