#!/bin/bash
# The IPv6 layer against a controllable peer, which is the only way its wire behaviour can be an
# oracle rather than an assertion about a code path.
#
# WHAT EACH ROW PROVES, and why it needs a peer rather than QEMU's user-mode networking:
#
#   quiet    nothing answers. The guest must still report its solicited-node group BEFORE detection
#            and from the unspecified source, send its detection probe, report every joined group
#            again once the address is its own, and keep soliciting a router. Slirp answers router
#            solicitations itself, so none of this is observable behind it.
#   router   one advertisement with an autonomous /64, a recursive server and a smaller MTU. The
#            guest must form an address, run detection on it, install the route and STOP soliciting.
#   hostile  an advertisement whose preferred lifetime exceeds its valid one, an autonomous prefix
#            that is not a /64, and a router that withdraws itself. The guest must refuse the first
#            two, form no address from either, and start soliciting again when the list empties.
#
# THE ORACLE IS THE CAPTURE PLUS THE GUEST LOG. The capture says what went onto the wire and with
# which hop limit and options; the guest log says what the host decided. A row asserts on both,
# because either alone can be satisfied by an implementation that is wrong in the other.

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."

work="$(mktemp -d)"
trap 'rm -rf "$work"; [[ -n "${peer_pid:-}" ]] && kill "$peer_pid" 2>/dev/null || true' EXIT

# A FAILURE PRINTS THE EVIDENCE IT JUDGED. A gate over a captured conversation that says only "not
# found" leaves the next person to reproduce the whole run before they can see what was there.
fail() {
	echo "ipv6-peer: $*" >&2
	if [[ -n "${capture:-}" && -s "${capture:-}" ]]; then
		echo "ipv6-peer: the capture held:" >&2
		sed 's/^/ipv6-peer:   /' "$capture" >&2
	fi
	if [[ -n "${guest:-}" && -s "${guest:-}" ]]; then
		echo "ipv6-peer: the guest said:" >&2
		grep -a "ipv6:" "$guest" | sed 's/^/ipv6-peer:   /' >&2 || true
	fi
	exit 1
}

note() {
	echo "ipv6-peer: $*"
}

# A port nothing else is on. Chosen per row so two rows cannot collide.
free_port() {
	python3 - <<'PY'
import socket
probe = socket.socket()
probe.bind(("127.0.0.1", 0))
print(probe.getsockname()[1])
probe.close()
PY
}

# One row: start the peer, boot the guest against it, and hand both records back.
row() {
	local scenario="$1" seconds="$2" link_mtu="${3:-}" label="${4:-$1}"
	local port capture guest
	port="$(free_port)"
	capture="$work/$label.capture"
	guest="$work/$label.guest"
	python3 src/harness/ipv6-peer.py --port "$port" --scenario "$scenario" --capture "$capture" --seconds "$seconds" >"$work/$label.peer" 2>&1 &
	peer_pid=$!
	# The peer prints its listening line before the port is usable, so waiting for it removes the
	# race rather than sleeping a guessed amount.
	local waited=0
	while ! grep -q "listening on $port" "$work/$label.peer" 2>/dev/null; do
		sleep 0.2
		waited=$((waited + 1))
		[[ "$waited" -lt 100 ]] || fail "$label: the peer never started listening"
	done
	NET_PEER_PORT="$port" NET_LINK_MTU="$link_mtu" SERIAL="file:$guest" timeout "$((seconds + 60))" ./run.sh --arch x86_64 --smp 2 >"$work/$label.run" 2>&1 || true
	wait "$peer_pid" 2>/dev/null || true
	peer_pid=""
	[[ -s "$capture" ]] || fail "$label: the peer captured nothing - the guest never reached the wire"
	printf '%s\n' "$capture" "$guest"
}

# 1. NOTHING ANSWERS.
note "quiet: nothing answers, and the guest must still configure itself and keep asking"
mapfile -t records < <(row quiet 45)
capture="${records[0]}"
guest="${records[1]}"

# THE FIRMWARE IS ON THIS LINK TOO, and its traffic is not the system's. UEFI brings the NIC up
# before the kernel does and runs its own IPv6: a version-1 listener report and a detection probe for
# the modified-EUI-64 address of the NIC's MAC, seconds before this system exists. Asserting on "an
# MLD report" would pass against the firmware's while the system sent none, so every line below names
# the system's own address, which the guest log states and the firmware cannot produce.
# `sed -n '1s//p'` RATHER THAN `| head -1`: under `pipefail` a reader that stops early closes the
# pipe, the writer takes SIGPIPE, and a MATCH reads as a failed pipeline. This reads all of it and
# prints only the first line's substitution.
assigned="$(grep -ao 'ipv6: link-local fe80::[0-9a-f:]*' "$guest" | sed -n '1s/^ipv6: link-local //p')"
[[ -n "$assigned" ]] || fail "quiet: the guest never reported an assigned link-local address"
note "  the system's address is $assigned"
group="$(
	python3 - "$assigned" <<'PY'
import ipaddress, sys
octets = ipaddress.IPv6Address(sys.argv[1]).packed
solicited = bytes.fromhex("ff0200000000000000000001ff") + octets[13:16]
print(ipaddress.IPv6Address(solicited).compressed)
PY
)"

# THE REPORT COMES FIRST AND FROM `::`, with hop limit 1 and the Router Alert option. A listener that
# reports after detection fails this line, and that is the ordering the whole item exists for: the
# probe below is addressed to the group this report is what asks the link to forward.
grep -q "saw mld-report-v2 src=:: .*hop=1 alert=yes group=$group\b" "$capture" || fail "quiet: no pre-detection listener report for $group sourced from :: with hop limit 1 and a Router Alert"
grep -q "saw neighbour-solicitation src=:: .*hop=255 .*target=$assigned\b" "$capture" || fail "quiet: no detection probe for $assigned from the unspecified source at hop limit 255"
grep -q "saw router-solicitation .*hop=255" "$capture" || fail "quiet: the guest never solicited a router"
# AND THE RE-REPORT, from the address itself, once detection finished.
grep -q "saw mld-report-v2 src=$assigned .*hop=1 alert=yes" "$capture" || fail "quiet: no post-detection re-report from $assigned"
grep -q "routers=0" "$guest" || fail "quiet: the guest claims a router on a link that has none"
# AND THE SCHEDULE IS THE ONE RFC 7559 ADOPTS, measured on the wire rather than asserted about a
# formula. The first interval is IRT plus a tenth either way; each later one is between 1.9 and 2.1
# times the last. A single-shot solicitation has no second line at all; one that retried at a fixed
# interval fails the growth; one whose jitter multiplies twice the previous interval fails the band.
# The tolerance is one scheduler tick at each end, because the deadline is rounded up to one.
python3 - "$capture" <<'PY' || fail "quiet: the solicitation schedule is not the one the standard fixes"
import sys

times = []
for line in open(sys.argv[1], encoding="utf-8"):
	if "saw router-solicitation" in line and "src=fe80::" in line or ("saw router-solicitation" in line and "src=::" in line):
		times.append(float(line.split()[0]))
if len(times) < 3:
	print(f"ipv6-peer: only {len(times)} solicitation(s) were sent; a single shot is not a schedule", file=sys.stderr)
	raise SystemExit(1)
gaps = [later - earlier for earlier, later in zip(times, times[1:])]
# FIFTY MILLISECONDS, AND THE REASON IS AN ASYMMETRY IN WHAT IS MEASURED, not slack.
#
# Both ends of every interval are timestamped at the PEER, on ARRIVAL. The first solicitation leaves
# in a burst behind two other frames - the listener report and the detection probe, all queued by the
# same bring-up - so it reaches the peer later than it was sent by however long those two took
# through the virtio ring. Every later solicitation goes out alone. The effect is one-directional:
# it makes the FIRST measured interval short and nothing else, and it was measured at 27ms.
#
# The band is 800ms wide, so a tolerance of 50ms still fails everything it exists to fail: a
# single-shot solicitation has no second line, a fixed-interval retry never grows, and a schedule
# whose jitter multiplies twice the previous interval lands hundreds of milliseconds out.
tick = 0.05
first = gaps[0]
if not (3.6 - tick <= first < 4.4 + tick):
	print(f"ipv6-peer: the first interval was {first:.3f}s, outside [3.6, 4.4)", file=sys.stderr)
	raise SystemExit(1)
for index, (previous, current) in enumerate(zip(gaps, gaps[1:]), start=1):
	low, high = 1.9 * previous - tick, 2.1 * previous + tick
	if not (low <= current <= high):
		print(f"ipv6-peer: interval {index + 1} was {current:.3f}s, outside [{low:.3f}, {high:.3f}] for a previous of {previous:.3f}s", file=sys.stderr)
		raise SystemExit(1)
print(f"ipv6-peer:   the solicitation intervals were {', '.join(f'{gap:.2f}s' for gap in gaps)}")
PY
note "  the report precedes detection and is sourced from ::, the probe follows, the re-report follows that, and the retransmission schedule is the standard's"

# 2. A ROUTER ANSWERS.
note "router: one advertisement with an autonomous /64, a recursive server and a smaller MTU"
mapfile -t records < <(row router 45)
capture="${records[0]}"
guest="${records[1]}"
grep -q "sent router-advertisement lifetime=1800 prefixes=1 rdnss=1" "$capture" || fail "router: the peer did not advertise"
# The guest must run detection on the address it formed, which means a second probe for a global one.
grep -q "saw neighbour-solicitation src=:: .*target=2001:db8:a:" "$capture" || fail "router: no detection probe for the address formed from the advertised prefix"
grep -q "addresses=2" "$guest" || fail "router: the guest did not add the formed address to the ones it holds"
grep -q "routers=1" "$guest" || fail "router: the advertised router was not installed"
# THE ROUTE TABLE IS STATE, NOT A DERIVED VIEW. One on-link prefix, and three routes: link-local,
# that prefix, and the default route through the router that advertised it.
grep -q "prefixes=1, routes=3" "$guest" || fail "router: the on-link prefix and the routes it implies were not installed"
grep -q "mtu=1400" "$guest" || fail "router: the advertised MTU was not taken"
# AND SOLICITATION STOPS, which only an installed default route does.
solicitations="$(grep -c "saw router-solicitation" "$capture" || true)"
[[ "$solicitations" -le 2 ]] || fail "router: solicitation continued after a default route was installed ($solicitations sent)"
# AND THE OTHER FAMILY IS STILL THERE, in the SAME boot. A configured IPv6 host that had quietly
# taken the interface away from IPv4 would pass every assertion above.
grep -q "static config - 10.0.2.15/24 via 10.0.2.2" "$guest" || fail "router: the guest lost its IPv4 address"
grep -q "saw ipv4-echo-reply .*src4=10.0.2.15" "$capture" || fail "router: the guest stopped answering IPv4 pings"
note "  the address was formed and proven, the route and the MTU were taken, solicitation stopped, and IPv4 kept working"

# 3. AN ADVERTISEMENT THAT LIES.
note "hostile: preferred beyond valid, an autonomous prefix that is not a /64, and a withdrawal"
mapfile -t records < <(row hostile 45)
capture="${records[0]}"
guest="${records[1]}"
grep -q "sent router-advertisement lifetime=1800 prefixes=2" "$capture" || fail "hostile: the peer did not advertise"
grep -q "sent router-advertisement lifetime=0" "$capture" || fail "hostile: the peer did not withdraw"
# NEITHER PREFIX MAY BECOME AN ADDRESS: the first is discarded whole, the second is not a /64.
! grep -q "target=2001:db8:b:" "$capture" || fail "hostile: an address was formed from a prefix whose preferred lifetime exceeded its valid one"
! grep -q "target=2001:db8:c:" "$capture" || fail "hostile: an address was formed from an autonomous prefix that is not a /64"
grep -q "addresses=1" "$guest" || fail "hostile: the guest holds more than its link-local address"
# AND THE WITHDRAWAL RESTARTS SOLICITATION, because the list is empty however it emptied.
after="$(awk '/sent router-advertisement lifetime=0/{seen=1; next} seen && /saw router-solicitation/{count++} END{print count+0}' "$capture")"
[[ "$after" -ge 1 ]] || fail "hostile: the guest stopped soliciting after its only router withdrew itself"
note "  both malformed prefixes were refused, no address was formed, and the withdrawal restarted solicitation"

# 4. A FLOOD OF MALFORMED PACKETS.
note "flood: sixty-four packets whose extension chain is a lie, while the host is bringing itself up"
mapfile -t records < <(row flood 45)
capture="${records[0]}"
guest="${records[1]}"
[[ "$(grep -c "sent malformed extension chain" "$capture")" -ge 32 ]] || fail "flood: the peer did not flood"
# THE HOST MUST STILL COME UP. A parser that refused a malformed chain by falling over would fail
# here, and so would one whose refusal path allocated: the address is formed, proven and reported
# while the flood is running.
# `sed -n '1s//p'` RATHER THAN `| head -1`: under `pipefail` a reader that stops early closes the
# pipe, the writer takes SIGPIPE, and a MATCH reads as a failed pipeline. This reads all of it and
# prints only the first line's substitution.
assigned="$(grep -ao 'ipv6: link-local fe80::[0-9a-f:]*' "$guest" | sed -n '1s/^ipv6: link-local //p')"
[[ -n "$assigned" ]] || fail "flood: the host did not finish bringing itself up under the flood"
grep -q "saw neighbour-solicitation src=:: .*hop=255 .*target=$assigned\\b" "$capture" || fail "flood: no detection probe under the flood"
# AND IT MUST NOT ANSWER THEM. Sixty-four malformed packets that provoked sixty-four ICMPv6 errors
# would be this guest amplifying somebody else's flood; the bucket and the suppression rules exist
# to stop exactly that.
answers="$(grep -c "saw parameter-problem\|saw destination-unreachable" "$capture" || true)"
[[ "$answers" -le 20 ]] || fail "flood: the host emitted $answers errors, past the twenty-token burst"
grep -q "dropped=0" "$guest" || note "  (the outbound queue dropped frames under the flood, which is the bound doing its job)"
note "  the host came up under the flood, and answered at most the burst"

# 5. ECHO TRAFFIC, AND A REPORT ABOUT IT.
note "echo: an echo request to the address the guest formed, and a Packet Too Big quoting the reply"
mapfile -t records < <(row echo 45)
capture="${records[0]}"
guest="${records[1]}"
grep -q "learned the guest's global address 2001:db8:a:" "$capture" || fail "echo: the guest never formed an address from the advertised prefix"
grep -q "sent echo-request id=16962" "$capture" || fail "echo: the peer never asked"
# ICMPv6 IS PART OF THE HOST, NOT AN OPTIONAL PING CODEC. The guest has no IPv6 transport yet and
# must still answer this, from the right address and with the identifier and sequence it was given.
grep -q "saw echo-reply src=2001:db8:a:" "$capture" || fail "echo: the guest did not answer the echo request"
# AND THE ERROR ABOUT IT IS VALIDATED AND DELIVERED. The quoted source is an address this interface
# holds, which is the strongest check a layer with no transport state can make; the consumer that
# owns the flow makes the other one, and it is the next milestone's.
grep -q "sent packet-too-big mtu=1300" "$capture" || fail "echo: the peer never quoted the reply"
grep -q "errors=[1-9]" "$guest" || fail "echo: the guest did not accept the quoted error"
# THE QUOTED ERRORS, INCLUDING THE ONE THAT MUST NOT COUNT. Three arrive: the Packet Too Big about
# the reply, and two out-of-order complaints from two different hops about successive Echo Requests
# this address supposedly sent. A fourth quotes an address this interface does NOT hold and is
# somebody else's complaint, so it is counted and reaches nobody.
grep -q "sent quoted-error type=1 .*seq=2 " "$capture" || fail "echo: the peer did not send the out-of-order errors"
grep -q "errors-seen=3, errors-dropped=1" "$guest" || fail "echo: the guest did not validate exactly the errors that quote its own address"
note "  the guest answered the echo from its formed address, took the error quoting it, and refused the one quoting somebody else"

# 6. A LINK THAT CANNOT CARRY IPv6 AT ALL.
#
# 1279 RATHER THAN A ROUND NUMBER. It is the largest link that still cannot carry IPv6, so it is the
# one an implementation comparing with `>` instead of `>=` gets wrong, and every smaller link fails
# the same test for the same reason.
note "narrow link: the device reports 1279 bytes, one below what IPv6 requires"
mapfile -t records < <(row quiet 45 1279 narrow)
capture="${records[0]}"
guest="${records[1]}"
# THE FAMILY IS REFUSED, and it says so. Raising the number instead would leave a host writing frames
# the link will not take; ignoring the link and running anyway would leave one that cannot send a
# legal packet.
grep -q "ipv6: refused on this link" "$guest" || fail "narrow link: the guest did not refuse IPv6 on a link below the minimum"
! grep -q "ipv6: link-local" "$guest" || fail "narrow link: the guest configured an address on a link that cannot carry IPv6"
# NOTHING OF THIS HOST'S IS ON THE WIRE. The firmware's own traffic is still there, which is why the
# check names the system's own emissions rather than counting frames.
! grep -q "saw mld-report-v2" "$capture" || fail "narrow link: the system emitted a listener report on a refused family"
# AND IPv4 IS UNAFFECTED, which is the whole point of a per-family refusal: not merely that the link
# still carries frames, but that this host answers on it.
grep -q "saw arp" "$capture" || fail "narrow link: IPv4 stopped working too"
grep -q "saw ipv4-echo-reply .*src4=10.0.2.15" "$capture" || fail "narrow link: the guest did not answer an IPv4 ping"
# THE FRAME BUFFERS ARE STILL THE LINK'S SIZE. A refusal that had also resized them would be a
# different bug wearing the same message, and this is the number they were cut to at boot.
grep -q "static config - 10.0.2.15/24 via 10.0.2.2 mtu=1279" "$guest" || fail "narrow link: the frame buffers are not the size the link reported"
note "  IPv6 refused, IPv4 answering, and nothing of this host's on the wire"

note "the IPv6 layer answers a controllable peer: the ordering holds, a valid advertisement configures the host, and a lying one configures nothing"
