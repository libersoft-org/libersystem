#!/usr/bin/env bash
# SpoolService, end to end, against printers the kernel harness plays - THE SERVICE, ITS JOBS AND ITS GRANT.
# The production service runs in the test kernel with a real `spool_probe` for every step, and the harness is
# the catalogue and every printer's typed backend: it decides how each write is answered and records every byte
# accepted. Success here establishes the job contract, admission and staging, transmission by acknowledged
# prefix, deadlines and endings, withdrawal, reset and recovery, and the production PermissionManager's grant
# and denial of `spool`; it says nothing about USB printer transport, which does not exist yet.
#
#   admission and staging    `inventory`: languages from device IDs, port observations and evidence-only
#                              details, typed refusals, two jobs a grant context, size-limit, an oversized
#                              frame refused before dispatch, exact submit, repeated submit, no write after
#   two jobs, exact          `two`: prefix acceptance and `again`, contiguous and in submission order
#   a stall                  `stall`: another printer, the client and the heartbeat go on; the job finishes
#   nothing unsubmitted      `abandon`, `crash`: closed, left at exit, cut off mid-write - no byte arrives
#   a detached job           `detach`: the owner exits after submitting and the job completes, once
#   withdrawal               `unplug`: unplugged with honest evidence; the replacement gets nothing of it
#   reset                    `reset`: a failed write, a reset that replays nothing, a stale attachment
#   failed recovery          `recovery`: the printer is unavailable and admits nothing
#   the grant                PermissionManager resolves the root by name and mints per launch; `date` gets
#                              no spool authority

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/evidence.sh"
export LIBER_RUN_MODE="${LIBER_RUN_MODE:-gate}"

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."
source "$HERE/result-logs.sh"

fail() {
	echo "spool-service: $*" >&2
	exit 1
}

[[ "${1:-}" == "" || "${1:-}" == "--arch" && "${2:-x86_64}" == "x86_64" ]] || fail "this gate is the x86_64 one; the ports cross-build the service and do not run it"
command -v qemu-system-x86_64 >/dev/null || fail "qemu-system-x86_64 is not installed"

work="$(mktemp -d)"
trap 'evidence_keep_gate "$work"/*.log; rm -rf "$work"' EXIT

tests=(
	kernel.services.spool_service_sends_submitted_jobs_exactly_once
	kernel.applications.permission_manager_grants_spool_and_import_to_their_probes_alone
)
selection="$(
	IFS=,
	echo "${tests[*]}"
)"

echo "spool-service: booting the test kernel with the spool scenario and the grant scenario"
TEST_SELECTION="$selection" ./test.sh --arch x86_64 >"$work/run.log" 2>&1 || {
	echo "spool-service: the spool tests failed" >&2
	grep -a "spool" "$work/run.log" | tail -40 >&2 || true
	tail -20 "$work/run.log" >&2
	exit 1
}
mapfile -t logs < <(result_logs "$work/run.log") || fail "the run did not say which logs it wrote"
((${#logs[@]})) || fail "the run named no readable log"

# EVERY STEP'S OWN WORD, and not only the suite's exit. A step the harness skipped would leave the suite
# green and this list short.
for step in inventory two stall abandon crash detach unplug reset recovery; do
	grep -aqh "spool-probe: PASS $step" "${logs[@]}" || fail "the $step step did not report a pass"
	echo "spool-service: $step passed"
done
# A HERE-STRING, NOT A PIPE: under `pipefail` a reader that stops at its first line fails the pipeline.
evidence="$(grep -ah "spool-probe: unplugged acknowledged=" "${logs[@]}" || true)"
[[ -n "$evidence" ]] || fail "the withdrawn printer's job reported no evidence"
sed -n '1s/^.*spool-probe: /spool-service: /p' <<<"$evidence"
for test in "${tests[@]}"; do
	grep -aqh "$test" "${logs[@]}" || fail "$test did not run"
done
if grep -aqh "\[failed\]" "${logs[@]}"; then
	fail "a selected test failed"
fi

echo "spool-service: PASS - the job contract, bounded staging, exact transmission by acknowledged prefix, stalls, endings, withdrawal, reset and recovery, and the spool grant (the service against a harness sink; not USB printer transport)"
