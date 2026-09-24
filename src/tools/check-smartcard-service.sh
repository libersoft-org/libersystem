#!/bin/bash
# SmartcardService, end to end, against the in-guest smart-card fixture - THE SERVICE AND ITS PINPAD
# POLICY. The fixture plays two readers and PIV cards over the provider contract the USB CCID class
# module will serve; success here establishes the service, its grants, its transactions and its pinpad
# policy, and says nothing about USB CCID transport or character/TPDU readers, neither of which exists.
#
# WHAT IS ASSERTED, AND WHO SAYS SO. What a client observes is the probes'; what REACHED A CARD, and in
# what order, is the fixture's own operation log, which the probes read and the gate's verdicts depend
# on; and the one cryptographic claim is checked by OpenSSL on the host, not by anything in the guest:
#
#   handles return to baseline  SmartcardService's handle count, read from the system graph, is the same
#                                 after the first probe and after the scenario
#   read-only is read-only      `cardread` describes and lists, and cannot acquire, verify or authenticate
#   insertion, ATR, allowlist   `cardcheck insert`: an announced insertion, the exact ATR, SELECT and both
#                                 GET DATA objects, the certificate continued past one response
#   the pinpad, and no PIN      `cardcheck pin`: verified on the reader's keypad, the template the reader
#                                 received carried only placeholders, and the authentication signature
#                                 VERIFIES WITH OPENSSL against the public key in the card's certificate
#   no pinpad, no PIN           `cardb`: verification on reader B is trusted-input-unavailable
#   refusals before the card    `cardcheck refuse`: a PIN-bearing VERIFY, another application, chained
#                                 and proprietary commands and a raw GENERAL AUTHENTICATE never reached a
#                                 reader
#   reader isolation            `cardcheck isolation`: a grant for reader A sees nothing of reader B
#   a queue and a clean handoff `cardhold hold` then `cardcheck queue`: the second client's first command
#                                 came after a full reset, and it inherited no verification
#   a dead owner's endpoint     `cardhold dup | cardcheck inherit`: a transferred endpoint did not keep
#                                 its dead owner's transaction
#   expiry and a late reply     `cardcheck expiry`: the lease cut the exchange, the late reply was dropped
#   removal and reinsertion     `cardcheck removal`: card-removed, and the new card waited for the abort
#   an abort never proven       `cardcheck stuck`: unavailable until the provider recovered it
#   event overflow              `cardcheck overflow`: closed at sixteen behind, a fresh snapshot after
#   a restart mid-exchange      `cardcheck restart` stops and starts the service through the supervisor
#                                 with an exchange in flight; `cardcheck session`: the new session waited
#                                 for the old work to drain
#   withdrawal                  `cardcheck republish`: the withdrawn reader's grant failed

set -euo pipefail
GUEST_GATE_NAME="smartcard-service"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"

guest_gate_arch "$@"
[[ "$GUEST_ARCH" == x86_64 ]] || guest_gate_fail "this gate is the x86_64 one; the ports cross-build the service and do not run it"

fail() { guest_gate_fail "$@"; }

# THE INDEPENDENT VERIFIER IS A PREREQUISITE, not an optional extra: without it the authentication claim
# would rest on the guest's own word.
command -v openssl >/dev/null || fail "openssl is required to verify the card's signature independently"
command -v xxd >/dev/null || fail "xxd is required to turn the probe's hex back into bytes"

guest_gate_require_programs smartcard_fixture cardcheck cardhold cardread cardb smartcard_service

# THE FIXTURE'S DEVICE, at the address its registry entry pins. Not 0x1f: on q35 that slot is the ICH9
# LPC bridge's, and QEMU refuses a second function 0 there before the guest starts.
export QEMU_EXTRA="-device edu,addr=0x1b"
# Several phases wait on five- and ten-second deadlines by design, and a restart follows them.
export GUEST_GATE_SECONDS="${GUEST_GATE_SECONDS:-300}"
export GUEST_GATE_TIMEOUT="${GUEST_GATE_TIMEOUT:-480}"

expect() {
	local lines="$1" line="$2" why="$3"
	grep -qF "$line" "$lines" || {
		echo "smartcard-service: expected \"$line\" - $why" >&2
		echo "--- guest log ---" >&2
		grep -aE 'cardcheck|cardhold|cardread|cardb|smartcard-fixture|SmartcardService' "$lines" >&2 || cat "$lines" >&2
		exit 1
	}
	echo "smartcard-service: $line"
}

# FROM AFTER THE FIRST PROBES, NOT FROM BOOT: PermissionManager resolves the service's roots by name the
# first time it mints a grant from them, and keeps each resolved connection - which the service counts as
# a handle. Measured: each root adds one, once, and nothing after. AND BEFORE THE RESTART: the graph keeps
# the process handle it was given at its own start, so after a restart it reads the instance that ended.
#
# THE RESTART IS THE PROBE'S, not the shell's `stop` and `start`: a replacement admits reader A only once
# the provider has answered its session, so every launch needing a reader-A grant is refused until the old
# exchange has drained - and the shell reports a refused launch by printing nothing.
guest_gate_run $'cardread\ngraph\ncardcheck insert\ncardcheck pin\ncardb\ncardcheck refuse\ncardcheck isolation\ncardhold hold 4 &\ncardcheck queue\ncardhold dup | cardcheck inherit\ncardcheck expiry\ncardcheck removal\ncardcheck stuck\ncardcheck overflow\ngraph\ncardcheck prime-slow\ncardhold slow &\ncardcheck inflight\ncardcheck restart\ncardcheck session\ncardcheck republish' ""
lines="$GUEST_LINES"

if grep -aq 'cardcheck: FAIL\|cardhold: FAIL\|cardread: FAIL\|cardb: FAIL' "$lines"; then
	grep -a 'cardcheck: FAIL\|cardhold: FAIL\|cardread: FAIL\|cardb: FAIL' "$lines" >&2
	fail "a probe reported a failure"
fi
expect "$lines" "cardread: PASS" "a read-only grant must describe and list, and must not acquire, verify or authenticate"
expect "$lines" "cardcheck: PASS insert" "an insertion must be announced, the ATR exact, and the allowlisted commands answered"
expect "$lines" "cardcheck: PASS pin" "the pinpad must verify with no PIN in any message, and authentication must sign"
expect "$lines" "cardb: PASS" "verification on a reader without a pinpad must be trusted-input-unavailable"
expect "$lines" "cardcheck: PASS refuse" "disallowed commands must fail before any reader sees them"
expect "$lines" "cardcheck: PASS isolation" "a grant for reader A must see nothing of reader B"
expect "$lines" "cardcheck: PASS queue" "a queued client must get the slot only after a full reset, with no inherited verification"
expect "$lines" "cardcheck: PASS inherit" "a transferred endpoint must not keep its dead owner's transaction"
expect "$lines" "cardcheck: PASS expiry" "a lease must cut an exchange once, and the late reply must be dropped"
expect "$lines" "cardcheck: PASS removal" "removal must be terminal, and a reinsertion must wait for the abort"
expect "$lines" "cardcheck: PASS stuck" "an abort never proven must leave the slot unavailable until its provider recovers it"
expect "$lines" "cardcheck: PASS overflow" "an event stream sixteen behind must be closed, and a new one must be current"
expect "$lines" "SmartcardService: an event stream fell sixteen behind and is closed" "the service must say it closed the stream"
expect "$lines" "cardcheck: an exchange the fixture holds is in flight" "the restart must land on an exchange in flight"
expect "$lines" "cardcheck: PASS restart" "the service must be stopped and started with the exchange in flight, and the replacement's session answered"
expect "$lines" "cardcheck: PASS session" "the restarted service's session must wait for the old work to drain"
expect "$lines" "cardcheck: PASS republish" "a withdrawn reader's grant must fail"

# THE HANDLES CAME BACK: the count after the first probe and after the scenario, from the system graph - which
# holds the service's process and reads the kernel's own count - with no probe connected either time.
counts="$(grep -aoE '\{name=smartcard_service, type=service, [^{]*counters=\{messages-sent=[0-9]+, messages-received=[0-9]+, handles=[0-9]+' "$lines" | grep -oE '[0-9]+$')"
[[ "$(wc -l <<<"$counts")" == 2 ]] || fail "the service's handle count was not read twice from the system graph"
[[ "$(sed -n 1p <<<"$counts")" == "$(sed -n 2p <<<"$counts")" ]] || fail "SmartcardService's handles did not return to their baseline ($(tr '\n' ' ' <<<"$counts"))"
echo "smartcard-service: the service's handles returned to their baseline ($(sed -n 1p <<<"$counts"))"

# THE SIGNATURE, VERIFIED BY OPENSSL against the public key in the certificate the card itself returned.
work="$guest_gate_work/signature"
mkdir -p "$work"
certificate="$(grep -aoE 'cardcheck: certificate [0-9a-f]+' "$lines" | awk '{print $3}')"
challenge="$(grep -aoE 'cardcheck: challenge [0-9a-f]+' "$lines" | awk '{print $3}')"
signature="$(grep -aoE 'cardcheck: signature [0-9a-f]+' "$lines" | awk '{print $3}')"
[[ -n "$certificate" && -n "$challenge" && -n "$signature" ]] || fail "the probe did not print the certificate, the challenge and the signature"
xxd -r -p <<<"$certificate" >"$work/certificate.der"
xxd -r -p <<<"$challenge" >"$work/challenge.bin"
xxd -r -p <<<"$signature" >"$work/signature.der"
openssl x509 -inform DER -in "$work/certificate.der" -pubkey -noout >"$work/public.pem" || fail "the card's certificate is not an X.509 certificate OpenSSL can read"
openssl pkeyutl -verify -pubin -inkey "$work/public.pem" -in "$work/challenge.bin" -sigfile "$work/signature.der" >"$work/verdict" 2>&1 || {
	cat "$work/verdict" >&2
	fail "OpenSSL did not verify the card's signature over the challenge"
}
echo "smartcard-service: OpenSSL verified the PIV authentication signature against the card's certificate"

echo "smartcard-service: PASS - grants, the allowlist, the pinpad with no PIN, an independently verified signature, queues, loss, recovery, events and a restart (the service and its pinpad policy; not USB CCID transport)"
