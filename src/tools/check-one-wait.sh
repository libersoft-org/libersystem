#!/usr/bin/env bash
# THE SUPERVISOR HAS ONE WAIT, AND EVERY HANDLE IT ANSWERS IS IN IT.
#
# `device_manager.rs` states the rule itself - "One wait, so a catalogue query cannot delay a
# supervisor message and a supervisor message cannot delay a query" - and then broke it in the one
# configuration nothing boots. A development build watched the agent's bootstrap in a SECOND wait at
# the top of the standing loop, `wait_any(&[bootstrap, dev.bootstrap], 0)`, which every pass parked
# in before it reached the real one. That set has no catalogue root in it, so from the moment the
# agent existed this program answered the supervisor and the agent and nothing else: the first
# service to ask the catalogue for a connection - AudioService, whose `CATALOGUE` role the supervisor
# mints before the service reports online - waited for a reply that could only be sent after a
# message the supervisor was itself blocked from sending. The development configuration deadlocked
# there, and every service behind that one never started.
#
# It was invisible because nothing boots that configuration: `development-build` proves it compiles
# and `development-gate` inspects the built volume. This gate does not boot it either - what it does
# is refuse the SHAPE the defect had, which is cheap, deterministic, and is the thing that was
# actually wrong. A wait in this program is over a set the program BUILT; an array literal at the
# call site is a second, narrower wait by construction.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
subject="$root/../user/services/core/src/device_manager.rs"
failed=0

fail() {
	echo "one-wait: $1" >&2
	failed=1
}

[[ -f "$subject" ]] || {
	echo "one-wait: $subject is not there to read" >&2
	exit 1
}

# Comments are where the defect is DESCRIBED, and describing it is how the next reader learns why
# this gate exists. Only code counts, so the leading `//` lines go before anything is matched.
code="$(sed 's://.*::' "$subject")"

# 1. NO AD-HOC WAIT SET. This is the defect's exact shape and the only rule that would have caught it.
literals="$(grep -c 'wait_any(&\[' <<<"$code" || true)"
if [[ "$literals" != "0" ]]; then
	fail "device_manager.rs waits on an array literal $literals time(s) - a wait written at the call site is a second, narrower wait than the one this program builds"
	grep -n 'wait_any(&\[' <<<"$code" >&2 || true
fi

# 2. THE STANDING LOOP'S WAIT IS OVER THE WHOLE BUILT SET, and there is one of it.
built="$(grep -c 'wait_any(&waiting\[\.\.waiting_count\]' <<<"$code" || true)"
if [[ "$built" != "1" ]]; then
	fail "the standing loop's wait over the built set appears $built time(s), and it must appear exactly once"
fi

# 3. EVERY PARTY THIS PROGRAM ANSWERS IS IN THAT SET. Each of these is a handle whose peer blocks
#    until DeviceManager reads it, so one left out of the wait is a deadlock and not a slow reply.
for handle in bootstrap catalogue_service policy_service dev.bootstrap; do
	if ! grep -q "waiting\[waiting_count\] = $handle;\|waiting\[0\] = $handle;" <<<"$code"; then
		fail "$handle is not placed in the standing loop's wait set - whoever is on the other end of it waits for a read that never happens"
	fi
done

# 4. AND THE AGENT'S HANDLE IS SERVED FROM ITS INDEX IN THAT SET, rather than from a wait of its own.
if ! grep -q 'at == dev_at' <<<"$code"; then
	fail "the development agent's handle is not dispatched by its index in the one wait set"
fi

if ((failed != 0)); then
	echo "one-wait: the supervisor does not have one wait" >&2
	exit 1
fi
echo "one-wait: DeviceManager has one wait, built rather than written at the call site, and the supervisor, catalogue, policy and development-agent handles are all in it"
