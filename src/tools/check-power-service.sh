#!/bin/bash
# PowerService, end to end, against the in-guest power fixture - SERVICE AND NORMALISATION COVERAGE.
#
# WHAT THIS IS NOT. The fixture publishes a HID-shaped UPS and ACPI-shaped power sources from decoded
# data written out in its program, through the same `power_model` adapters a real driver calls. That
# proves the service, its canonical units, its subscriptions and its controls. It is NOT real HID or
# AML coverage: QEMU attaches no UPS here and nothing evaluates a battery's methods. The real producers
# are a separate item, and their evidence belongs to their own gates.
#
# WHY IT NEEDS A GUEST. The conversions and the state machines are held by host tests on stated
# numbers; what only a running system can show is that the catalogue scope, the provider wire, the
# supervisor's grants and PermissionManager's policy fit together - that a read client really cannot
# reach a control, that a publication past a driver's declaration really reaches nothing, and that an
# unanswered control really completes in five seconds while everything else keeps moving.
#
# WHAT IS ASSERTED:
#
#   exact units                 `powercheck list exact`: every source, in canonical units, equal to the
#                                 values the fixture's decoded data states
#   a reader cannot control     `powerread control` / `publish`: a read client holds no control grant,
#     or publish                  and a control or a publication sent on its connection closes it
#   a command reaches one       `powercheck control`: nothing reached the fixture before (so the read
#     outlet                      client's attempt arrived nowhere), and the operator's command reaches
#                                 exactly outlet 1 of the UPS
#   forged and invalid          `powercheck denied`: forged generation, local and binding; a read-only
#     requests are refused        zone; an absent outlet; a delay past a day
#   subscriptions               `watch`, `alarm`, `coalesce`, `overflow`: a change during a subscription
#                                 arrives after its snapshot, an alarm transition is delivered, a slow
#                                 reader gets the latest measurement rather than every one, and
#                                 transitions a reader does not take close it - a new one is current
#   a withheld reply            `powercheck withhold`: indeterminate in five seconds, never replayed,
#                                 no conflicting control before a fresh query reconciles, and the other
#                                 provider served throughout
#   an unauthorized publisher   `powercheck extra`: the fixture offers one publication past its
#                                 declaration; DeviceManager refuses it, and PowerService never sees it
#   removal erases live state   `powercheck remove`: a removed source and a withdrawn publication leave
#                                 nothing live, and the replacement is other sources
#   a restart reconstructs      after `stop`/`start` of the service: every source again, from the
#                                 catalogue, under a NEW epoch

set -euo pipefail
GUEST_GATE_NAME="power-service"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"

guest_gate_arch "$@"
[[ "$GUEST_ARCH" == x86_64 ]] || guest_gate_fail "this gate is the x86_64 one; the ports cross-build the service and do not run it"

fail() { guest_gate_fail "$@"; }

guest_gate_require_programs power_fixture powercheck powerread power_service

# THE FIXTURE'S DEVICE, at the address its registry entry pins. A machine without it never binds the
# fixture, which is what keeps it out of every image this gate did not build.
export QEMU_EXTRA="-device edu,addr=0x1e"
# The withheld reply alone is eleven seconds of waiting, and a restart follows it.
export GUEST_GATE_SECONDS="${GUEST_GATE_SECONDS:-200}"
export GUEST_GATE_TIMEOUT="${GUEST_GATE_TIMEOUT:-360}"

expect() {
	local lines="$1" line="$2" why="$3"
	grep -qF "$line" "$lines" || {
		echo "power-service: expected \"$line\" - $why" >&2
		echo "--- guest log ---" >&2
		grep -aE 'powercheck|powerread|power-fixture|PowerService|DeviceManager: power_fixture' "$lines" >&2 || cat "$lines" >&2
		exit 1
	}
	echo "power-service: $line"
}

guest_gate_run $'powercheck list exact\npowerread control\npowerread publish\npowercheck control\npowercheck denied\npowercheck watch\npowercheck alarm\npowercheck coalesce\npowercheck overflow\npowercheck withhold\npowercheck extra\npowercheck remove\nstop power_service\nstart power_service\npowercheck list' ""
lines="$GUEST_LINES"

if grep -aq 'powercheck: FAIL\|powerread: FAIL' "$lines"; then
	grep -a 'powercheck: FAIL\|powerread: FAIL' "$lines" >&2
	fail "a probe reported a failure"
fi
expect "$lines" "powercheck: ups remaining 6300000 uAh full 7000000 uAh design 7200000 uAh soc 9000 bp voltage 13650000 uV power -180000000 uW runtime 2400 s load 3600 bp temperature 26850 mC outlets 2" "the HID-shaped UPS must enumerate in canonical units"
expect "$lines" "powercheck: battery remaining 24000000 uWh full 48000000 uWh design 50000000 uWh soc 5000 bp voltage 11100000 uV power 5000000 uW" "the ACPI-shaped battery must enumerate in canonical units, as energy and power"
expect "$lines" "powercheck: thermal-zone temperature 27050 mC critical 95050 mC hot 90050 mC passive 80050 mC active0 70050 mC active1 60050 mC" "the ACPI-shaped zone must enumerate in millidegrees Celsius"
expect "$lines" "powercheck: PASS list: the HID-shaped UPS and the ACPI-shaped battery, adapter and zone enumerate with exact units" "every source's canonical values must be the ones its decoded data states"
expect "$lines" "powerread: PASS a read client lists sources, holds no control authority, and a control sent on its connection closed it" "a read connection must not carry a control"
expect "$lines" "powerread: PASS a publication sent on a read connection closed it" "a read connection must not carry a publication"
expect "$lines" "powercheck: PASS control" "an operator's command must reach exactly one outlet, and nothing may have reached the fixture before it"
expect "$lines" "powercheck: PASS denied" "forged identities and invalid requests must be refused"
expect "$lines" "powercheck: PASS watch" "a change during a subscription must arrive after its snapshot"
expect "$lines" "powercheck: PASS alarm" "an alarm transition must be delivered"
expect "$lines" "powercheck: PASS coalesce" "a slow reader must receive the latest measurement rather than every one"
expect "$lines" "powercheck: PASS overflow" "transitions a reader does not take must close its subscription, and a new one must be current"
expect "$lines" "PowerService: a subscription is closed - its reader fell behind, and continuity is lost" "the service must say that it closed the subscription"
expect "$lines" "power-fixture: a control reply is withheld" "the fixture must have withheld the reply"
expect "$lines" "powercheck: PASS withhold" "an unanswered control must be indeterminate, unreplayed and reconciled before a conflict, with the other provider served"
expect "$lines" "power_fixture offered more providers of one kind than it declares in \`provides\`; refused" "DeviceManager must refuse a publication past the fixture's declaration"
expect "$lines" "powercheck: PASS extra" "the refused publication must reach nothing"
expect "$lines" "powercheck: PASS remove" "removal and withdrawal must erase live state, and a replacement must be other sources"
expect "$lines" "powercheck: PASS list: four sources are enumerated" "after a restart the service must reconstruct every source from the catalogue"

epochs="$(grep -aoE 'powercheck: epoch [0-9]+' "$lines" | awk '{print $3}' | sort -u | wc -l)"
[[ "$epochs" == 2 ]] || fail "the restarted service reported $epochs distinct epochs across the two listings; a restart must start a new one"
echo "power-service: the restarted service reconstructed its sources under a new epoch"

echo "power-service: PASS - exact units, read denial, one-outlet control, refusals, coherent subscriptions, a withheld reply, an unauthorized publication, removal and a restart (service and normalisation coverage; not real HID or AML)"
