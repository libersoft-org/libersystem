#!/bin/bash
# ModemService and NetworkService's raw-IP link, end to end, against the in-guest modem fixture - THE
# SERVICE, ITS GRANTS AND THE LINK. The fixture plays a modem, its SIM and the network behind it over the
# provider contract the USB CDC-MBIM class module will serve, with every command, reply and datagram
# carried through the staged MBIM validators; success here establishes the service, its four
# authorities, the transaction rules and the uplink lifecycle, and says nothing about USB MBIM transport,
# which does not exist yet.
#
# TWO MACHINES, because the uplink rules differ with and without a NIC and a NIC cannot be unplugged:
#
# WITHOUT A NIC (`NET_NONE=1`)
#   handles return to baseline  ModemService's handles, read from the system graph with no probe connected,
#                                 and its clients in use are the same after the first probe and after the
#                                 scenario
#   a data-only client          `modemdata`: identity and management withheld, and an installation sent on
#                                 an ordinary network connection installs nothing
#   PIN, counted                `modemcheck pin`: counters known, one attempt per call, no replay
#   identity, separately        `modemcheck identity`: the identity grant answers for its own modem only
#   traffic over the modem      `modemcheck activate`: with no NIC the context becomes the uplink; address,
#                                 DNS and MTU through `info`; echo and DNS both ways, counted by the fixture;
#                                 a replayed activation reply changes nothing; deactivation unlinks
#   one context                 `modemcheck context`: a second activation while one context is up is
#                                 refused before the modem sees it, and the slot comes back when it ends
#                                 (the provider and client bounds are refused and given back in the kernel
#                                 scenario `kernel.services.modem_service_refuses_at_its_provider_and_client_bounds_and_gives_them_back`)
#   context loss                `modemcheck drop`: the network ending the context removes the link
#   rollback                    `modemcheck rollback`: an installation NetworkService refuses (IPv6-only)
#                                 is deactivated again and leaves nothing
#   a dead owner's endpoint     `modemhold | modemcheck inherit`: the context ends with its owner, and a
#                                 transferred copy of the endpoint can do nothing
#   a failed prepared launch    `modemfail`: its data grant was minted and its launch then failed; the
#                                 grant was retired, which the handle baseline proves
#   SIM replacement             `modemcheck sim`: a new SIM ends the context and the grants bound to it
#   an unanswered PIN           `modemcheck lock`, `modemcheck uncertain`: outcome-unknown, sent once,
#                                 reconciled by a fresh query
#   limits                      `modemcheck limits`: four providers, 32 clients, one context, none held
#   withdrawal                  `modemcheck republish`: a withdrawn modem's grant fails; republication is
#                                 another modem
#
# WITH A NIC
#   busy                        `modemcheck busy`: a grant that may not replace the NIC is refused before
#                                 the modem does anything, and the NIC is untouched
#   replacement and fallback    `modemswap`: a grant that may replace it does, carries traffic, and on
#                                 deactivation the NIC comes back as a new interface generation

set -euo pipefail
GUEST_GATE_NAME="modem-service"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"

guest_gate_arch "$@"
[[ "$GUEST_ARCH" == x86_64 ]] || guest_gate_fail "this gate is the x86_64 one; the ports cross-build the service and do not run it"

fail() { guest_gate_fail "$@"; }

guest_gate_require_programs modem_fixture modemcheck modemhold modemswap modemdata modemfail modem_service network_service

# THE FIXTURE'S DEVICE, at the address its registry entry pins.
export QEMU_EXTRA="-device edu,addr=0x1c"
# An unanswered PIN waits out its five-second deadline by design, and two machines boot.
export GUEST_GATE_SECONDS="${GUEST_GATE_SECONDS:-240}"
export GUEST_GATE_TIMEOUT="${GUEST_GATE_TIMEOUT:-400}"

expect() {
	local lines="$1" line="$2" why="$3"
	grep -qF "$line" "$lines" || {
		echo "modem-service: expected \"$line\" - $why" >&2
		echo "--- guest log ---" >&2
		grep -aE 'modemcheck|modemhold|modemswap|modemdata|modemfail|modem-fixture|ModemService|network:' "$lines" >&2 || cat "$lines" >&2
		exit 1
	}
	echo "modem-service: $line"
}

refused() {
	local lines="$1"
	if grep -aq 'modemcheck: FAIL\|modemhold: FAIL\|modemswap: FAIL\|modemdata: FAIL\|modemfail: FAIL' "$lines"; then
		grep -a 'modemcheck: FAIL\|modemhold: FAIL\|modemswap: FAIL\|modemdata: FAIL\|modemfail: FAIL' "$lines" >&2
		fail "a probe reported a failure"
	fi
}

# ------------------------------------------------------------------ the machine with no NIC

# FROM AFTER THE FIRST PROBES, NOT FROM BOOT: PermissionManager resolves the service's roots by name the
# first time it mints a grant from them, and keeps each resolved connection - which the service counts as
# a handle. Measured: each root adds one, once, and nothing after.
NET_NONE=1 guest_gate_run $'modemcheck clients\ngraph\nmodemdata\nmodemcheck pin\nmodemcheck identity\nmodemcheck activate\nmodemcheck context\nmodemcheck drop\nmodemcheck rollback\nmodemhold | modemcheck inherit\nmodemfail\nmodemcheck sim\nmodemcheck lock\nmodemcheck uncertain\nmodemcheck limits\ngraph\nmodemcheck clients\nmodemcheck republish' ""
unlinked="$guest_gate_work/unlinked"
cp "$GUEST_LINES" "$unlinked"
refused "$unlinked"
expect "$unlinked" "network: no network provider on this boot - NetworkService is up without a link" "the first machine must boot with no NIC"
expect "$unlinked" "driver.modem-fixture: online" "the fixture must bind"
expect "$unlinked" "ModemService: a modem was admitted" "the service must admit the fixture's modem"
expect "$unlinked" "modemdata: PASS" "a data-only client must hold no identity or management grant, and a network client must install nothing"
expect "$unlinked" "modemcheck: PASS pin" "PIN attempts must be counted, one per call"
expect "$unlinked" "modemcheck: PASS identity" "the identity grant must answer for its own modem only"
expect "$unlinked" "modemcheck: PASS activate" "the context must carry echo and DNS through NetworkService"
expect "$unlinked" "network: modem link installed - 10.64.0.2/30" "NetworkService must install the modem's configuration"
expect "$unlinked" "modemcheck: PASS context" "a second context must be refused while one is up, and admitted once it ends"
expect "$unlinked" "modemcheck: PASS drop" "the network ending the context must remove the link"
expect "$unlinked" "modemcheck: PASS rollback" "a refused installation must be deactivated"
expect "$unlinked" "modemhold: activated, sent its endpoint and context, and exits" "the holder must activate and hand its endpoint on"
expect "$unlinked" "modemcheck: PASS inherit" "a transferred endpoint must not keep its dead owner's context"
expect "$unlinked" "ModemService: a grant's owner ended - its grant is retired" "the service must observe owners, not channels"
expect "$unlinked" "modemcheck: PASS sim" "a new SIM must end the context and its grants"
expect "$unlinked" "modemcheck: PASS uncertain" "an unanswered PIN must be outcome-unknown, sent once and reconciled"
expect "$unlinked" "modemcheck: PASS limits" "the limits must be stated and nothing left held"
expect "$unlinked" "modemcheck: PASS republish" "a withdrawn modem's grant must fail"
if grep -aq "modemfail: FAIL" "$unlinked"; then
	fail "modemfail ran: its launch was not refused after its data grant was minted"
fi

# THE HANDLES AND THE CLIENTS CAME BACK: after the first probe and after the scenario. The handles from the
# system graph, which holds the service's process and reads the kernel's own count, with no probe connected; the
# clients from the service, with the one probe that asks connected both times.
handles="$(grep -aoE '\{name=modem_service, type=service, [^{]*counters=\{messages-sent=[0-9]+, messages-received=[0-9]+, handles=[0-9]+' "$unlinked" | grep -oE '[0-9]+$')"
clients="$(grep -aoE 'modemcheck: service clients [0-9]+' "$unlinked" | awk '{print $4}')"
[[ "$(wc -l <<<"$handles")" == 2 ]] || fail "the service's handle count was not read twice from the system graph"
[[ "$(wc -l <<<"$clients")" == 2 ]] || fail "the service's clients were not read twice"
[[ "$(sed -n 1p <<<"$handles")" == "$(sed -n 2p <<<"$handles")" ]] || fail "ModemService's handles did not return to their baseline ($(tr '\n' ' ' <<<"$handles"))"
[[ "$(sed -n 1p <<<"$clients")" == "$(sed -n 2p <<<"$clients")" ]] || fail "ModemService's clients did not return to their baseline ($(tr '\n' ' ' <<<"$clients"))"
echo "modem-service: the service's handles ($(sed -n 1p <<<"$handles")) and clients ($(sed -n 1p <<<"$clients")) returned to their baseline"

# ------------------------------------------------------------------ the machine with a NIC

guest_gate_run $'modemcheck pin\nmodemcheck busy\nmodemswap' ""
linked="$guest_gate_work/linked"
cp "$GUEST_LINES" "$linked"
refused "$linked"
expect "$linked" "network: configured via DHCP" "the second machine must have its NIC configured"
expect "$linked" "modemcheck: PASS busy" "a grant that may not replace the NIC must be refused before the modem acts"
expect "$linked" "modemswap: PASS" "a grant that may replace the NIC must, and the NIC must come back"

# ------------------------------------------------------------------ the provider and client bounds

# REFUSED AT AND GIVEN BACK, in the kernel's scenario: five modems published and four admitted, the fifth never
# opened, and a withdrawn modem's slot taken by a new publication; thirty-two client connections and the next
# refused, and a closed one's slot taken. The fixture plays one modem, so the kernel harness plays these two
# bounds, through the same provider and observation wires.
source "$root/tools/result-logs.sh"
bounds="kernel.services.modem_service_refuses_at_its_provider_and_client_bounds_and_gives_them_back"
TEST_SELECTION="$bounds" ./test.sh --arch x86_64 >"$guest_gate_work/bounds.log" 2>&1 || {
	tail -30 "$guest_gate_work/bounds.log" >&2
	fail "the bounds scenario failed"
}
mapfile -t logs < <(result_logs "$guest_gate_work/bounds.log") || fail "the bounds run did not say which logs it wrote"
((${#logs[@]})) || fail "the bounds run named no readable log"
grep -aqh "$bounds" "${logs[@]}" || fail "the bounds scenario did not run"
if grep -aqh "\[failed\]" "${logs[@]}"; then
	fail "the bounds scenario failed"
fi
echo "modem-service: four modems and thirty-two clients were admitted, the next of each refused, and both slots given back"

echo "modem-service: PASS - grants and their separation, PIN handling without replay, traffic through NetworkService, loss, rollback, replacement, fallback, reclamation and the provider, client and context bounds (the service and the raw-IP link; not USB MBIM transport)"
