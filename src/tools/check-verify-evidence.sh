#!/usr/bin/env bash
# THE EVIDENCE PATH'S OWN FIXTURES: what a release run's claims rest on, each broken on purpose.
#
#   1. A KEPT LOG OUTLIVES A FAILING PRODUCER. A producer keeps its phase log from its cleanup and
#      exits non-zero; its temporary directory is gone, the copy in the run is readable and matches
#      the digest its envelope records; the dossier refuses the key for FAILING, not for a missing
#      log; an altered copy and a deleted copy are each refused by name. A real gate through
#      `check.sh` publishes its envelope with its captured output.
#   2. A REPLACED TOOL AND A REPLACED FIRMWARE IMAGE ARE NOT WHAT BOOTS. The runner binds every
#      input through a descriptor and, under the fixture hook, holds after binding. In that window -
#      after the hash, before the exec and before QEMU's own open - the QEMU executable on `PATH`
#      is replaced by a script and the OVMF image by zeros, at their PATHNAMES. The boot reaches
#      its marker on the bound bytes, the run log names the digests that ran, and the substitute
#      never runs. Mutate-use-restore on the two input classes the source seal does not cover.
#   3. A SEALED SNAPSHOT REFUSES A WRITE, AND A MOVED TREE FAILS ITS DOSSIER. A detached worktree of
#      HEAD, sealed: appending to a tracked file and creating a file both fail; its identity is a
#      Git tree id; one file unsealed and changed makes the dossier refuse with the tree-changed
#      reason; restored, the refusal is gone.
#   4. THE RELEASE PATH, REHEARSED. `verify.sh --release --in-place --required` over three cheap
#      keys: the run starts, the plan is narrowed to them, the executor publishes the fallback
#      envelope for the host suite, the gate runner publishes the gates', the dossier renders in a
#      `rehearsal` state and never `complete`.
#
# Minutes: one short x86_64 boot to the loader's marker, one worktree, one rehearsal.
set -euo pipefail
export LIBER_RUN_MODE="${LIBER_RUN_MODE:-gate}"
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."
REPO="$PWD"
# shellcheck source=evidence.sh
source "$HERE/evidence.sh"

fail() {
	echo "verify-evidence: $*" >&2
	exit 1
}

work="$(mktemp -d)"
snapshot=""
cleanup() {
	evidence_keep_gate "$work"/*.log "$work"/*.out
	[[ -z "$snapshot" ]] || src/tools/release-snapshot.sh remove "$snapshot"
	rm -rf "$work"
}
trap cleanup EXIT

model() {
	(cd src/tools/verify-model && cargo run --quiet --manifest-path Cargo.toml -- "$@")
}
refusals() {
	python3 -c 'import json, sys; d = json.load(open(sys.argv[1])); print("\n".join(r["reason"] for r in d["refusals"]))' "$1"
}

# 1. A KEPT LOG OUTLIVES A FAILING PRODUCER.
echo "verify-evidence: 1. a kept log outlives a failing producer, and its envelope still matches"
run="$(model run-start --out-root "$work/runs")" || fail "run-start refused (an undeclared LIBER_/OVMF_/QEMU_/TEST_ override in this environment?)"
[[ -f "$run/run-id" && -f "$run/identity.json" ]] || fail "run-start made no run at $run"
key="gate.volume-layout / host / host / default"
cat >"$work/producer.sh" <<PRODUCER
#!/usr/bin/env bash
set -euo pipefail
source "$REPO/src/tools/evidence.sh"
phase="\$(mktemp -d)"
trap 'evidence_keep "$key" "\$phase"/*.log; rm -rf "\$phase"' EXIT
echo "phase one: booted, then refused" >"\$phase/phase-one.log"
echo "\$phase" >"$work/producer.dir"
exit 1
PRODUCER
status=0
LIBER_VERIFY_RUN="$run" bash "$work/producer.sh" >"$work/producer.out" 2>&1 || status=$?
((status == 1)) || fail "the fixture producer exited $status, not 1"
[[ ! -d "$(<"$work/producer.dir")" ]] || fail "the producer's temporary directory survived its exit"
kept="$run/logs/gate.volume-layout+host+host+default/phase-one.log"
[[ -f "$kept" ]] || fail "the phase log was not kept in the run at $kept"
grep -q "phase one: booted" "$kept" || fail "the kept log does not carry the producer's bytes"
LIBER_VERIFY_RUN="$run" evidence_publish "$key" "the runner, for a producer that published nothing" failed 3 --if-absent
envelope="$run/evidence/gate.volume-layout+host+host+default.json"
[[ -f "$envelope" ]] || fail "no envelope at $envelope"
grep -q 'phase-one.log"' "$envelope" || fail "the envelope does not name the kept log"
cat >"$work/required.toml" <<REQUIRED
schema = 1
keys = ["$key"]
REQUIRED
model dossier --run "$run" --required "$work/required.toml" >"$work/dossier-1.out" 2>&1 && fail "a required key that FAILED rendered a complete dossier"
refusals "$run/dossier.json" >"$work/reasons"
grep -qx "failed-required" "$work/reasons" || fail "the failing producer's key was not refused as failed-required: $(tr '\n' ' ' <"$work/reasons")"
! grep -q "log-missing\|log-altered" "$work/reasons" || fail "the kept log was reported missing or altered while it is intact"
echo "verify-evidence:   kept, named, digest intact, refused as FAILED and not as missing"
chmod u+w "$kept"
echo "altered afterwards" >>"$kept"
model dossier --run "$run" --required "$work/required.toml" >/dev/null 2>&1 || true
grep -qx "log-altered" <(refusals "$run/dossier.json") || fail "an altered kept log was not refused as log-altered"
rm -f "$kept"
model dossier --run "$run" --required "$work/required.toml" >/dev/null 2>&1 || true
grep -qx "log-missing" <(refusals "$run/dossier.json") || fail "a deleted kept log was not refused as log-missing"
echo "verify-evidence:   an altered copy is log-altered, a deleted copy is log-missing"
# And a real gate through the gate runner.
run_gate="$(model run-start --out-root "$work/runs")"
LIBER_VERIFY_RUN="$run_gate" ./check.sh --gate volume-layout >"$work/gate.out" 2>&1 || fail "the volume-layout gate failed under a run: $(tail -3 "$work/gate.out")"
gate_envelope="$run_gate/evidence/gate.volume-layout+host+host+default.json"
[[ -f "$gate_envelope" ]] || {
	tail -8 "$work/gate.out" >&2
	fail "check.sh published no envelope for the gate it ran (expected $gate_envelope)"
}
grep -q '"outcome": "passed"' "$gate_envelope" || fail "the gate's envelope does not record a pass"
grep -q '"producer": "check.sh gate volume-layout"' "$gate_envelope" || fail "the gate's envelope does not name its producer"
gate_log="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["logs"][0]["path"])' "$gate_envelope")"
[[ -s "$run_gate/$gate_log" ]] || fail "the gate's captured output was not kept at $gate_log"
grep -q "volume package entries match manifest" "$run_gate/$gate_log" || fail "the kept gate log is not the gate's output: $(head -3 "$run_gate/$gate_log")"
echo "verify-evidence:   the gate runner published the gate's envelope with its captured output"

# 2. A REPLACED TOOL AND A REPLACED FIRMWARE IMAGE ARE NOT WHAT BOOTS.
echo "verify-evidence: 2. a tool and a firmware image replaced after the hash and before the use are not what boots"
ISO="$(evidence_image libersystem.iso)" || fail "no libersystem.iso produced by this run"
[[ -f "$ISO" ]] || fail "no $ISO - run ./image.sh --format iso"
real_tool="$(command -v qemu-system-x86_64)" || fail "qemu-system-x86_64 is required"
real_firmware="${OVMF_CODE:-/usr/share/OVMF/OVMF_CODE_4M.fd}"
[[ -f "$real_firmware" ]] || fail "no OVMF at $real_firmware"
mkdir -p "$work/bin"
cp "$real_tool" "$work/bin/qemu-system-x86_64"
cp "$real_firmware" "$work/ovmf-code.fd"
tool_digest="$(sha256sum "$work/bin/qemu-system-x86_64" | cut -d' ' -f1)"
firmware_digest="$(sha256sum "$work/ovmf-code.fd" | cut -d' ' -f1)"
mkfifo "$work/hold"
port="$(python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])')"
boot_log="$work/boot.log"
: >"$boot_log"
# THE RUNNER DIRECTLY, NOT THROUGH THE VERDICT TOOL, because that tool discards the runner's own
# output and the hold and the digests are printed there. The verdict is read off the serial log
# below, bounded, and the guest is stopped through the pid file QEMU writes.
(
	# COLD: an image set of its own, so a person's development instance refuses nothing here.
	PATH="$work/bin:$PATH" OVMF_CODE="$work/ovmf-code.fd" LIBER_HARNESS_HOLD="$work/hold" BOOT_IMAGE="$ISO" COLD=1 HOSTFWD_PORT="$port" SERIAL="file:$boot_log" QEMU_EXTRA="-pidfile $work/qemu.pid" \
		src/harness/qemu-run.sh x86_64
) >"$work/runner.out" 2>&1 &
boot_pid=$!
# TERM, then KILL: a QEMU asked to terminate has been seen to sit in its main loop for good.
stop_guest() {
	local pid
	pid="$(cat "$work/qemu.pid" 2>/dev/null || true)"
	[[ -n "$pid" ]] && kill "$pid" 2>/dev/null || true
	kill "$boot_pid" 2>/dev/null || true
	for _ in $(seq 1 50); do
		kill -0 "$boot_pid" 2>/dev/null || break
		sleep 0.2
	done
	[[ -n "$pid" ]] && kill -KILL "$pid" 2>/dev/null || true
	kill -KILL "$boot_pid" 2>/dev/null || true
	wait "$boot_pid" 2>/dev/null || true
}
for _ in $(seq 1 600); do
	grep -q "qemu-run: HELD" "$work/runner.out" 2>/dev/null && break
	kill -0 "$boot_pid" 2>/dev/null || {
		cat "$work/runner.out" >&2
		fail "the runner exited before it reached the hold"
	}
	sleep 0.1
done
grep -q "qemu-run: HELD" "$work/runner.out" || {
	stop_guest
	tail -5 "$work/runner.out" >&2
	fail "the runner never reached the hold"
}
grep -qF "qemu-system-x86_64 sha256=$tool_digest" "$work/runner.out" || fail "the runner did not bind the tool copy it was given"
grep -qF "firmware OVMF_CODE sha256=$firmware_digest" "$work/runner.out" || fail "the runner did not bind the firmware copy it was given"
# THE MUTATIONS, IN THE WINDOW: the names now resolve to a substitute and to zeros.
cat >"$work/bin/qemu-system-x86_64.new" <<'SUBSTITUTE'
#!/bin/sh
echo "SUBSTITUTED TOOL RAN" >&2
exit 3
SUBSTITUTE
chmod +x "$work/bin/qemu-system-x86_64.new"
mv -f "$work/bin/qemu-system-x86_64.new" "$work/bin/qemu-system-x86_64"
head -c "$(stat -c %s "$work/ovmf-code.fd")" /dev/zero >"$work/ovmf-code.fd.new"
mv -f "$work/ovmf-code.fd.new" "$work/ovmf-code.fd"
echo go >"$work/hold"
# THE VERDICT: the loader's rollback line - printed AFTER its hand-off line, so it is the one to
# wait for - within a bounded wait, and never a FATAL.
for _ in $(seq 1 1200); do
	grep -aq "loader: rollback floor" "$boot_log" 2>/dev/null && break
	grep -aq "loader: FATAL" "$boot_log" 2>/dev/null && break
	kill -0 "$boot_pid" 2>/dev/null || break
	sleep 0.1
done
stop_guest
if grep -q "SUBSTITUTED TOOL RAN" "$work/runner.out"; then fail "the substituted tool RAN: the harness executed the name, not the bound object"; fi
if grep -aq "loader: FATAL" "$boot_log"; then
	grep -a "loader:" "$boot_log" | sed -n '1,10p' >&2
	fail "the loader refused after the mutation - the zeroed firmware or the substitute was used"
fi
grep -aq "loader: rollback floor - not enforced by this build" "$boot_log" || {
	tail -20 "$work/runner.out" >&2
	fail "the guest did not print the loader's marker after the mutation - the substitute or the zeroed firmware was used"
}
grep -aq "loader: kernel loaded" "$boot_log" || fail "the loader printed its marker but never handed the kernel over"
"$work/bin/qemu-system-x86_64" 2>"$work/substitute.out" || true
grep -q "SUBSTITUTED TOOL RAN" "$work/substitute.out" || fail "the mutation did not take: the pathname still runs the real tool"
[[ "$(sha256sum "$work/ovmf-code.fd" | cut -d' ' -f1)" != "$firmware_digest" ]] || fail "the firmware mutation did not take"
echo "verify-evidence:   bound: tool $tool_digest, firmware $firmware_digest; the names were replaced in the window and the guest booted the bound bytes"

# 3. A SEALED SNAPSHOT REFUSES A WRITE, AND A MOVED TREE FAILS ITS DOSSIER.
echo "verify-evidence: 3. a sealed snapshot refuses a write, and a moved tree fails its dossier"
snapshot="$REPO/.build/release/fixture-$$"
src/tools/release-snapshot.sh create "$snapshot" HEAD >/dev/null || fail "no snapshot"
src/tools/release-snapshot.sh seal "$snapshot" >"$work/seal.out" || fail "the snapshot could not be sealed"
cat "$work/seal.out"
# AS WHOEVER RUNS THIS: root is not held by a mode, so on a filesystem with the immutable attribute
# the seal must refuse root too, and the fixture says which seal it proved.
if (echo "// moved" >>"$snapshot/README.md") 2>/dev/null; then fail "the sealed snapshot accepted an append to a tracked file (uid $(id -u); $(cat "$work/seal.out"))"; fi
if (touch "$snapshot/new-file") 2>/dev/null; then fail "the sealed snapshot accepted a new file (uid $(id -u); $(cat "$work/seal.out"))"; fi
[[ -w "$snapshot/.build" ]] || fail "the snapshot's .build is not writable, and the producers write there"
echo "verify-evidence:   sealed: an append and a new file are both refused; .build is writable"
run3="$(LIBERSYSTEM_ROOT="$snapshot" model run-start --out-root "$work/runs")" || fail "run-start refused the snapshot"
grep -q '"kind": "git-tree"' "$run3/identity.json" || fail "a clean detached snapshot's identity is not its Git tree id"
cat >"$work/required-none.toml" <<'REQUIRED'
schema = 1
keys = []
REQUIRED
# THE DOSSIER FROM THIS TREE'S MODEL, WITH THE SNAPSHOT AS ITS SOURCE. A release runs the snapshot's
# own binary; here the snapshot is HEAD and this working tree's binary may not parse HEAD's manifest,
# so the catalog comes from this tree and `--source` points the tree-moved comparison at the snapshot.
model dossier --run "$run3" --required "$work/required-none.toml" --source "$snapshot" >"$work/dossier-3a.out" 2>&1 || fail "the untouched snapshot's dossier was refused: $(tail -2 "$work/dossier-3a.out")"
cp "$snapshot/README.md" "$work/README.original"
src/tools/release-snapshot.sh unseal "$snapshot/README.md"
echo "// moved during the run" >>"$snapshot/README.md"
if model dossier --run "$run3" --required "$work/required-none.toml" --source "$snapshot" >"$work/dossier-3b.out" 2>&1; then
	fail "a tree that moved during the run rendered its dossier"
fi
grep -q "source-changed" <(refusals "$run3/dossier.json") || fail "the moved tree was not refused as source-changed: $(tail -2 "$work/dossier-3b.out")"
cp "$work/README.original" "$snapshot/README.md"
src/tools/release-snapshot.sh reseal "$snapshot/README.md"
model dossier --run "$run3" --required "$work/required-none.toml" --source "$snapshot" >"$work/dossier-3c.out" 2>&1 || fail "the restored tree's dossier was refused: $(tail -2 "$work/dossier-3c.out")"
echo "verify-evidence:   identity git-tree; changed -> source-changed; restored -> accepted"
src/tools/release-snapshot.sh remove "$snapshot"
snapshot=""

# 4. THE RELEASE PATH, REHEARSED.
echo "verify-evidence: 4. the release path rehearsed in place over three cheap keys"
cat >"$work/required-rehearsal.toml" <<'REQUIRED'
schema = 1
keys = [
	"host.abi / host / host / default",
	"gate.volume-layout / host / host / default",
	"gate.dma-mode-carrier / host / host / default",
]
REQUIRED
if ! env -u LIBER_VERIFY_RUN -u LIBER_GATE_KEY ./verify.sh --release --in-place --required "$work/required-rehearsal.toml" --out-root "$work/rehearsal" >"$work/rehearsal.out" 2>&1; then
	tail -30 "$work/rehearsal.out" >&2
	fail "the rehearsal did not finish"
fi
rehearsal_run="$(sed -n 's/^verify.sh: run: //p' "$work/rehearsal.out" | tail -1)"
[[ -d "$rehearsal_run" ]] || fail "the rehearsal named no run directory: $(grep -m3 run: "$work/rehearsal.out")"
grep -q "^# Release dossier - run .* (rehearsal (" "$rehearsal_run/dossier.md" || fail "the rehearsal's dossier does not say it is a rehearsal: $(head -1 "$rehearsal_run/dossier.md")"
! grep -q "^# Release dossier - run .* (complete)" "$rehearsal_run/dossier.md" || fail "an in-place rehearsal rendered as complete"
for k in host.abi+host+host+default gate.volume-layout+host+host+default gate.dma-mode-carrier+host+host+default; do
	[[ -f "$rehearsal_run/evidence/$k.json" ]] || fail "the rehearsal published no envelope for $k"
done
grep -q '"producer": "verify.sh step' "$rehearsal_run/evidence/host.abi+host+host+default.json" || fail "the host suite's envelope was not the runner's fallback"
grep -q '"producer": "check.sh gate volume-layout"' "$rehearsal_run/evidence/gate.volume-layout+host+host+default.json" || fail "the gate's envelope was not the gate runner's"
# By mode, not by `-w`: root writes through a mode, and what the run promises is the mode.
[[ "$(stat -c %a "$rehearsal_run/dossier.json")" == 444 ]] || fail "the finished run was not made read-only (mode $(stat -c %a "$rehearsal_run/dossier.json"))"
grep -q "release run finished: rehearsal" "$work/rehearsal.out" || fail "the rehearsal did not report its state: $(tail -2 "$work/rehearsal.out")"
echo "verify-evidence:   run $(basename "$rehearsal_run"): three envelopes, two producers, dossier in a rehearsal state, run sealed"

echo "verify-evidence: a kept log outlives its producer, a replaced tool or firmware image is not what boots, a sealed snapshot refuses a write, a moved tree fails its dossier, and the release path renders a rehearsal"
