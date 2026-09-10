#!/usr/bin/env bash
# THE DEVELOPMENT GUEST HAS A RUNNABLE LIFECYCLE: image, boot, readiness, the four development
# checks against that instance, teardown - and nothing here depends on a guest a person left running.
#
# WHAT THIS GATE IS. The four development rows - the self-test, the protocol suite, the performance
# gate and the display-driver restart - each require a running development instance and used to
# require it of the PERSON: `./dev.sh up` first, by hand, against the tree's shared state. That is a
# guest nobody built as part of the run, whose image nobody recorded, and which the next `dev-up`
# reuses however stale it is. This gate owns the whole lifecycle instead:
#
#   1. STATE     a run-private state directory: the instance's lock, sockets, logs, boot record and
#                the immutable copy of the image it boots live there and nowhere else, its writable
#                image set is named after it, and its host port is one nobody else has - so it
#                coexists with a person's instance and is refused by nothing of theirs
#   2. IMAGE     `./dev.sh up` builds the development image and boots an IMMUTABLE COPY of it, and
#                prints the copy's digest; every result below records that digest as its input
#   3. READINESS the shell prompt and the development agent's handshake, which `dev-up` requires
#   4. CHECKS    the four development checks, in the order the catalog runs them, each publishing its
#                own envelope under its own key with the boot artifact as its input - a failure in
#                one does not stop the others, and the gate fails at the end
#   5. TEARDOWN  `./dev.sh down` from the EXIT trap whatever happened, the serial and QEMU logs kept
#                in the run first, the private image set removed
#
# KVM, so this is minutes: the image build, one boot, and the checks - the performance gate rebuilds
# and republishes one program several times. `check-development-build.sh` stays the compile-only
# gate; it is not evidence that the configuration boots, and this is.
set -euo pipefail
export LIBER_RUN_MODE="${LIBER_RUN_MODE:-gate}"
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."
# shellcheck source=evidence.sh
source "$HERE/evidence.sh"

fail() {
	echo "development-lifecycle: $*" >&2
	exit 1
}

command -v python3 >/dev/null || fail "python3 is not installed, and the lab is written in it"
[[ -f .build/boot/init-x86_64.pkg ]] || fail "no x86_64 build - run ./build.sh --arch x86_64 first"

KEY="dev.lifecycle / x86_64 / dev-guest / development"
started=$SECONDS
# SHORT, because a Unix socket path is limited to 108 bytes and this directory holds four of them.
state="$(mktemp -d "${TMPDIR:-/tmp}/liber-dev.XXXXXX")"
export LIBER_DEV_STATE="$state"
# A HOST PORT OF ITS OWN: the persistent instance forwards 5556 and an ad-hoc run 5555; this one
# takes whatever the kernel says is free.
HOSTFWD_PORT="$(python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])')"
export HOSTFWD_PORT
outcome=failed
artifact=""

cleanup() {
	local status=$?
	# TEARDOWN FIRST, so the logs are complete when they are kept, then the evidence, then the state.
	./dev.sh down >"$state/down.log" 2>&1 || echo "development-lifecycle: teardown reported a problem (see the kept down.log)" >&2
	evidence_keep "$KEY" "$state/up.log" "$state/dev-serial.log" "$state/dev-qemu.log" "$state/down.log"
	local -a inputs=()
	[[ -n "$artifact" && -f "$artifact" ]] && inputs=(--input "$artifact")
	evidence_publish "$KEY" "check-development-lifecycle.sh" "$outcome" "$((SECONDS - started))" "${inputs[@]}"
	# The private image set the runner made for this instance, named after the state directory.
	rm -f .build/boot/*"-dev-$(basename "$state")"* 2>/dev/null || true
	rm -rf "$state"
	return "$status"
}
trap cleanup EXIT

# 1 + 2 + 3. THE INSTANCE, from nothing, in private state.
echo "development-lifecycle: state $state, host port $HOSTFWD_PORT"
echo "development-lifecycle: bringing the development instance up (image build, boot, readiness)"
if ! ./dev.sh up --timeout 300 >"$state/up.log" 2>&1; then
	tail -20 "$state/up.log" >&2
	fail "the development instance did not come up (see the kept up.log)"
fi
artifact="$(sed -n 's/^lab: boot artifact sha256=[0-9a-f]* (\(.*\))$/\1/p' "$state/up.log" | tail -1)"
digest="$(sed -n 's/^lab: boot artifact sha256=\([0-9a-f]*\) (.*$/\1/p' "$state/up.log" | tail -1)"
[[ -f "$artifact" && "$digest" =~ ^[0-9a-f]{64}$ ]] || fail "the bring-up did not name the boot artifact it booted"
[[ "$artifact" == "$state/"* ]] || fail "the instance booted $artifact, not an immutable copy under its own state"
[[ "$(stat -c %a "$artifact")" == 444 ]] || fail "the boot artifact $artifact is not read-only (mode $(stat -c %a "$artifact"))"
[[ "$(sha256sum "$artifact" | cut -d' ' -f1)" == "$digest" ]] || fail "the boot artifact's bytes do not match the digest the bring-up printed"
# AND THE GUEST BOOTED THOSE BYTES: the runner binds the medium through a held descriptor and prints
# the digest it hashed through it, which is the binding a mode cannot give root.
grep -q "qemu-run: medium sha256=$digest " "$state/dev-qemu.log" || fail "the runner did not bind the guest to the boot artifact's digest $digest"
echo "development-lifecycle:   up; boot artifact sha256=$digest, bound by the runner through a held descriptor"
grep -q "development instance ready" "$state/up.log" || fail "the bring-up did not report readiness"
# The lock and every socket are the private instance's, and the tree's state was not touched.
[[ -S "$state/dev-channel.sock" && -S "$state/dev-serial.sock" ]] || fail "the instance's sockets are not under its own state"
echo "development-lifecycle:   readiness: shell prompt and agent handshake, sockets under $state"

# 4. THE CHECKS, each its own envelope.
failures=0
run_check() {
	local id="$1" script="$2" key="$1 / x86_64 / dev-guest / development" log="$state/$1.log" status=0 at=$SECONDS
	echo "development-lifecycle: $id ($script)"
	(cd src && "$script") >"$log" 2>&1 || status=$?
	if ((status == 0)); then
		echo "development-lifecycle:   $id passed ($((SECONDS - at))s)"
		evidence_publish "$key" "check-development-lifecycle.sh" passed "$((SECONDS - at))" --input "$artifact" --log "$log"
	else
		tail -15 "$log" | sed 's/^/    /' >&2
		echo "development-lifecycle:   $id FAILED (exit $status, $((SECONDS - at))s)" >&2
		evidence_publish "$key" "check-development-lifecycle.sh" failed "$((SECONDS - at))" --input "$artifact" --log "$log"
		failures=$((failures + 1))
	fi
	evidence_keep "$KEY" "$log"
}
run_check dev.selftest harness/dev-selftest.py
run_check dev.proto-test harness/proto-test.py
run_check dev.perf-gate harness/perf-gate.py
run_check dev.gpu-restart harness/dev-gpu-restart.py

# THE SAME GUEST THROUGHOUT: the boot record the bring-up wrote is the one the checks ended on.
[[ -f "$state/dev-instance.boot" ]] || fail "the instance's boot record is gone"
if ((failures > 0)); then
	fail "$failures of 4 development checks failed against the instance this gate brought up"
fi
outcome=passed
echo "development-lifecycle: image, boot, readiness, four checks and teardown - one lifecycle, private state, boot artifact $digest"
