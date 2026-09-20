#!/usr/bin/env bash
# THE ONE WAIT, ENFORCED RATHER THAN ASKED FOR.
#
# DeviceManager's supervisor loop carries a note, forty lines above it, that states the rule the
# whole program depends on: "One wait, so a catalogue query cannot delay a supervisor message and a
# supervisor message cannot delay a query." Nothing enforced it, and on 2026-09-20 FIVE places broke
# it - a second wait in front of the one wait, each of them a blocking IPC call with no deadline on
# a peer this program does not control:
#
#   the supervisor message, the development agent's channel, the provider catalogue's client, the
#   device-policy client, and the heartbeat sent to every bound driver on every pass
#
# Any one of them stops the supervisor dead: no catalogue, no device policy, no bus events, no
# teardowns and no chassis events, for as long as that peer stays quiet. It was found by counting the
# loop's passes - 1, 2, 4, 8, 16, 32, and then never again - after a chassis power button press had
# been enqueued on the right channel and reported by the kernel.
#
# SO THE RULE IS A GATE NOW. A blocking receive or a blocking send inside the supervisor's call
# graph is refused, and a call that genuinely must wait says so on the line above it with the
# `SUPERVISOR-WAIT-OK:` marker and a reason. That makes each one a decision somebody wrote down
# rather than the default nobody noticed.
set -euo pipefail

cd "$(dirname "$0")/.."

FILE="user/services/core/src/device_manager.rs"

# What a wait with no end looks like. `send_blocking` is here for the same reason the receives are:
# it waits for ROOM with no deadline, and a peer that stopped reading never makes any.
PATTERN='recv_blocking\(|recv_caps_blocking\(|recv_vec_blocking\(|send_blocking\(|send_blocking_attenuated\('

# THE BOOTSTRAP HANDSHAKE IS NOT THE LOOP. Everything before the supervisor loop runs once, in a
# fixed order, against a supervisor that is waiting for exactly those messages; a wait there is the
# handshake and not a second wait in front of a first. The loop begins at the line that says so.
LOOP_ANCHOR='4. stand until ServiceManager drives phase 2'

check_file() {
	local path="$1"
	local start
	start="$(grep -n "$LOOP_ANCHOR" "$path" | head -n 1 | cut -d: -f1 || true)"
	if [[ -z "$start" ]]; then
		echo "supervisor-waits: cannot find the supervisor loop in $path - its anchor comment moved, and this gate is measuring nothing" >&2
		return 2
	fi
	local offenders=0
	while IFS=: read -r line text; do
		[[ -n "$line" ]] || continue
		((line > start)) || continue
		# A COMMENT ABOUT ONE IS NOT ONE. Every finding above is described in prose next to its fix.
		[[ "$text" =~ ^[[:space:]]*(//|///) ]] && continue
		# THE STATED EXCEPTION, ANYWHERE IN THE COMMENT BLOCK DIRECTLY ABOVE. A reason worth writing
		# is usually longer than a line, and this tree writes it that way; requiring the marker on
		# the last line would push every reason into one sentence or split it from its call.
		local marked=0 above=$((line - 1)) text_above
		while ((above > 0)); do
			text_above="$(sed -n "${above}p" "$path")"
			[[ "$text_above" =~ ^[[:space:]]*(//|///) ]] || break
			if [[ "$text_above" == *"SUPERVISOR-WAIT-OK:"* ]]; then
				marked=1
				break
			fi
			above=$((above - 1))
		done
		((marked)) && continue
		echo "supervisor-waits: $path:$line is a wait with no end inside the supervisor loop:" >&2
		echo "supervisor-waits:   ${text#"${text%%[![:space:]]*}"}" >&2
		offenders=$((offenders + 1))
	done < <(grep -nE "$PATTERN" "$path" || true)
	return $((offenders > 0 ? 1 : 0))
}

# PROVE IT REFUSES BEFORE TRUSTING IT TO APPROVE. A validator tested only over a tree that currently
# passes is not tested: it would report the same "clean" if its pattern matched nothing at all.
if [[ "${SUPERVISOR_WAITS_SELF_TEST:-}" != "1" ]]; then
	planted="$(mktemp)"
	trap 'rm -f "$planted"' EXIT
	cp "$FILE" "$planted"
	# One planted wait, after the loop's anchor, of each shape the rule is about.
	awk -v anchor="$LOOP_ANCHOR" '
		{ print }
		index($0, anchor) && !done { print "\t\tlet _ = recv_blocking(bootstrap, &mut buf);"; done = 1 }
	' "$FILE" >"$planted"
	if SUPERVISOR_WAITS_SELF_TEST=1 check_file "$planted" >/dev/null 2>&1; then
		echo "supervisor-waits: SELF-TEST FAILED - a planted blocking receive inside the loop was accepted, so this gate is guarding nothing" >&2
		exit 1
	fi
fi

if ! check_file "$FILE"; then
	status=$?
	if ((status == 2)); then
		exit 1
	fi
	echo "supervisor-waits: the loop's own note says why: \"One wait, so a catalogue query cannot delay a supervisor message and a supervisor message cannot delay a query.\"" >&2
	echo "supervisor-waits: take the message instead of waiting for it, and let an empty read be a pass - the handle is in the wait set like every other." >&2
	echo "supervisor-waits: a call that genuinely must wait carries \`SUPERVISOR-WAIT-OK:\` and a reason on the line above it." >&2
	exit 1
fi

echo "supervisor-waits: the supervisor loop holds one wait and no other"
