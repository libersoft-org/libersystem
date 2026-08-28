#!/usr/bin/env bash
# WHICH FILES A TEST RUN PUT ITS RESULT IN, taken from the run rather than guessed from the tree.
#
# Every gate that boots a guest used to find its evidence by globbing
# `.build/logs/test/<arch>-*-guest.log` and taking the newest. Two runs of one architecture in flight
# and a gate reads the OTHER one's log and passes - a green that is about somebody else's guest. And
# it is wrong on one target of three even alone, because on riscv64 the suite's output lands in the
# RUN log while the guest log holds only U-Boot and the loader.
#
# `test-kernel.sh` prints `RESULT-LOGS <run> <guest>` for exactly this. Both, because the oracle is
# in a different one of the two depending on the port, and the runner itself reads them together.
#
# Usage:  mapfile -t logs < <(result_logs "$captured_output_file")
# Fails loudly rather than falling back to a glob: a gate that cannot find out which run it made is a
# gate that must not go looking for one it did not.
result_logs() {
	local captured="$1" line
	line="$(sed -n 's/^\[test-[a-z0-9_]*\] RESULT-LOGS //p' "$captured" | tail -1)"
	if [[ -z "$line" ]]; then
		echo "result-logs: the run at $captured printed no RESULT-LOGS line, so which logs it wrote is not known - and guessing is what this replaces" >&2
		return 1
	fi
	local path
	for path in $line; do
		[[ -f "$path" ]] || continue
		printf '%s\n' "$path"
	done
}
