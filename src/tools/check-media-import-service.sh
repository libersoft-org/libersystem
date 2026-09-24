#!/usr/bin/env bash
# MediaImportService, end to end, against a camera the kernel harness plays - THE SERVICE, ITS PARSER AND ITS
# GRANT. The production service runs in the test kernel with a real `import_probe` for every step and a real
# StorageService volume as the destination, and the harness is the catalogue and a bounded PTP responder: it
# answers the read-only subset with standard containers generated on demand and records every operation it was
# sent. Success here establishes the import contract, the PTP container and dataset validation at the service
# boundary, bounded snapshots and pages, scoped identities, validated completion, removal as an explicit partial
# ending, and PermissionManager's grant of `media-import`; it says nothing about USB Still Image transport.
#
#   browse    limits, the device and its storages, three pages of exact metadata out of 40 000 handles, the
#               65 537-handle storage refused explicitly, another client's identity denied, an unknown
#               operation ending the connection
#   stale     a reported change stales the cursor and the objects named under it
#   import    a 12 kB photo and a zero-byte object complete, committed to the destination
#   removal   the camera pulled after one chunk: partial with its count, the destination untouched
#   the grant PermissionManager resolves the root by name and mints per launch; `date` gets nothing

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/evidence.sh"
export LIBER_RUN_MODE="${LIBER_RUN_MODE:-gate}"

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."
source "$HERE/result-logs.sh"

fail() {
	echo "media-import-service: $*" >&2
	exit 1
}

[[ "${1:-}" == "" || "${1:-}" == "--arch" && "${2:-x86_64}" == "x86_64" ]] || fail "this gate is the x86_64 one; the ports cross-build the service and do not run it"
command -v qemu-system-x86_64 >/dev/null || fail "qemu-system-x86_64 is not installed"

work="$(mktemp -d)"
trap 'evidence_keep_gate "$work"/*.log; rm -rf "$work"' EXIT

tests=(
	kernel.services.media_import_service_reads_snapshots_and_whole_objects_and_nothing_else
	kernel.applications.permission_manager_grants_spool_and_import_to_their_probes_alone
)
selection="$(
	IFS=,
	echo "${tests[*]}"
)"

echo "media-import-service: booting the test kernel with the responder scenario and the grant scenario"
TEST_SELECTION="$selection" ./test.sh --arch x86_64 >"$work/run.log" 2>&1 || {
	echo "media-import-service: the import tests failed" >&2
	grep -a "import" "$work/run.log" | tail -40 >&2 || true
	tail -20 "$work/run.log" >&2
	exit 1
}
mapfile -t logs < <(result_logs "$work/run.log") || fail "the run did not say which logs it wrote"
((${#logs[@]})) || fail "the run named no readable log"

# EVERY STEP'S OWN WORD, and not only the suite's exit.
for step in browse stale import removal; do
	grep -aqh "import-probe: PASS $step" "${logs[@]}" || fail "the $step step did not report a pass"
	echo "media-import-service: $step passed"
done
for test in "${tests[@]}"; do
	grep -aqh "$test" "${logs[@]}" || fail "$test did not run"
done
if grep -aqh "\[failed\]" "${logs[@]}"; then
	fail "a selected test failed"
fi

echo "media-import-service: PASS - the read-only import contract, PTP validation at the service boundary, bounded snapshots and pages, scoped identities, validated completion and explicit partial endings, and the media-import grant (the service against a harness responder; not USB Still Image transport)"
