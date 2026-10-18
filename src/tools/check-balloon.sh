#!/usr/bin/env bash
# THE MEMORY BALLOON, end to end on x86_64: a development instance of 2 GiB with a `virtio-balloon-pci`, its host's
# requests made over QMP and read back there, and the guest's own account of its memory read with `free`.
#
#   1. THE HOST ASKS FOR 512 MiB AND IS GIVEN IT: QEMU's `actual` falls by exactly that, and `free` shows the balloon
#      holding it - pages the kernel had free, through ResourceManager's allowance.
#   2. THE HOST ASKS FOR NEARLY EVERYTHING AND IS GIVEN WHAT THE POLICY ALLOWS: the balloon stops short, the machine
#      keeps its reserve free, and ResourceManager says it is holding the rest back.
#   3. A PROGRAM NEEDS MEMORY THE BALLOON HOLDS: `memhog` asks for 768 MiB, and the balloon gives back - QEMU's
#      `actual` rises while it runs - until the program holds all of it; nothing it holds is taken from it.
#   4. THE HOST CANCELS: asked for nothing, the balloon gives everything back, and `free` shows none held.
#   5. THE HOST IS TOLD THE MACHINE'S MEMORY: QEMU's guest statistics name its total and its free memory.
#
# IT BOOTS ITS OWN INSTANCE in private state and takes it down from the EXIT trap. The development image is the one
# `LIBER_DEVELOPMENT=1 ./image.sh --format iso` wrote - `memhog` is in no shipping image.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."

GIB=$((1 << 30))
MIB=$((1 << 20))

fail() {
	echo "balloon: $*" >&2
	exit 1
}

say() {
	echo "balloon: $*"
}

command -v python3 >/dev/null || fail "python3 is not installed, and the lab is written in it"
[[ ! -S .build/boot/lab-ctl.sock ]] || fail "an ad-hoc lab guest is up (.build/boot/lab-ctl.sock) - take it down with ./lab.sh quit first"

state="$(mktemp -d "${TMPDIR:-/tmp}/liber-balloon.XXXXXX")"
export LIBER_DEV_STATE="$state"
HOSTFWD_PORT="$(python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])')"
export HOSTFWD_PORT
export BALLOON=1 MEM=2G
kept="$(pwd)/.build/logs/balloon"
rm -rf "$kept"
mkdir -p "$kept"
hog=""

cleanup() {
	local status=$?
	[[ -z "$hog" ]] || wait "$hog" 2>/dev/null || true
	cp -f "$state/dev-serial.log" "$kept/serial.log" 2>/dev/null || true
	./dev.sh down >"$state/down.log" 2>&1 || echo "balloon: teardown reported a problem (see $kept/down.log)" >&2
	cp -f "$state"/*.log "$kept/" 2>/dev/null || true
	rm -f .build/boot/*"-dev-$(basename "$state")"* 2>/dev/null || true
	rm -rf "$state"
	return "$status"
}
trap cleanup EXIT

# ONE QMP COMMAND on the instance's own socket, its `return` printed as JSON.
qmp() {
	python3 - "$state/qemu-qmp.sock" "$@" <<'PY'
import json, socket, sys
path, execute = sys.argv[1], sys.argv[2]
arguments = json.loads(sys.argv[3]) if len(sys.argv) > 3 else None
s = socket.socket(socket.AF_UNIX)
s.connect(path)
f = s.makefile("rw")
json.loads(f.readline())
def ask(command, args=None):
    request = {"execute": command}
    if args is not None:
        request["arguments"] = args
    f.write(json.dumps(request) + "\n")
    f.flush()
    while True:
        line = json.loads(f.readline())
        if "return" in line or "error" in line:
            return line
ask("qmp_capabilities")
answer = ask(execute, arguments)
if "error" in answer:
    sys.exit("qmp: " + json.dumps(answer["error"]))
print(json.dumps(answer["return"]))
PY
}

actual() {
	qmp query-balloon | python3 -c 'import json, sys; print(json.load(sys.stdin)["actual"])'
}

ask_for() {
	qmp balloon "{\"value\": $1}" >/dev/null
}

# WAIT until QEMU's `actual` reads `want`, at most `limit` seconds.
await_actual() {
	local want="$1" what="$2" limit="${3:-90}" now=""
	for _ in $(seq 1 "$limit"); do
		now="$(actual)"
		[[ "$now" == "$want" ]] && return 0
		sleep 1
	done
	fail "$what (actual $now, wanted $want, after ${limit} s)"
}

# WAIT until `actual` has not moved for `quiet` seconds, at most `limit`; prints it.
await_settled() {
	local quiet="$1" limit="$2" last="" now still=0
	for _ in $(seq 1 "$limit"); do
		now="$(actual)"
		if [[ "$now" == "$last" ]]; then
			still=$((still + 1))
			((still >= quiet)) && {
				echo "$now"
				return 0
			}
		else
			still=0
		fi
		last="$now"
		sleep 1
	done
	echo "$last"
}

launch() {
	./dev.sh launch --timeout 120 "$@" 2>&1
}

# THE BALLOON'S SIZE AS `free` STATES IT, in MiB: zero when it says none.
held_mib() {
	launch free >"$state/free.log" || true
	python3 - "$state/free.log" <<'PY'
import re, sys
text = open(sys.argv[1], errors="replace").read()
m = re.search(r"Balloon: given to the host (\d+)", text)
print(int(m.group(1)) // (1 << 20) if m else 0)
PY
}

free_mib() {
	python3 - "$state/free.log" <<'PY'
import re, sys
text = open(sys.argv[1], errors="replace").read()
m = re.search(r"Mem: +total \d+, used \d+, free (\d+)", text)
print(int(m.group(1)) // (1 << 20) if m else -1)
PY
}

say "booting a 2 GiB development instance with a balloon"
./dev.sh up --timeout 400 >"$state/up.log" 2>&1 || fail "the development guest did not come up (see $kept/up.log)"
grep -a -q -F "driver.virtio-balloon: online" "$state/dev-serial.log" || fail "the balloon driver did not bind"
total="$(actual)"
[[ "$total" == "$((2 * GIB))" ]] || fail "QEMU says the guest has $total bytes, not 2 GiB, before anything was asked"

# 1. 512 MiB ASKED FOR, 512 MiB GIVEN.
ask_for "$((2 * GIB - 512 * MIB))"
await_actual "$((2 * GIB - 512 * MIB))" "the balloon did not give the host the 512 MiB it asked for"
[[ "$(held_mib)" == "512" ]] || fail "free does not say the balloon holds 512 MiB ($(cat "$state/free.log"))"
say "the host asked for 512 MiB back and was given it, and free says the balloon holds it"

# 2. NEARLY EVERYTHING ASKED FOR, THE RESERVE KEPT.
ask_for "$((128 * MIB))"
settled="$(await_settled 5 120)"
((settled > 128 * MIB)) || fail "the balloon gave the host everything it asked for ($settled bytes left) - nothing was held back"
held="$(held_mib)"
free_now="$(free_mib)"
((held > 512)) || fail "the balloon did not grow past 512 MiB toward the larger request (holds $held MiB)"
((free_now >= 200)) || fail "the machine kept only $free_now MiB free, under its reserve of an eighth"
grep -a -q "ResourceManager: the host asks for .* held back to keep" "$state/dev-serial.log" || fail "ResourceManager did not say it held part of the request back"
say "asked for all but 128 MiB, the balloon stopped at $held MiB with $free_now MiB kept free, and ResourceManager said so"

# 3. A PROGRAM THAT NEEDS IT: the balloon gives back while the program waits.
launch memhog 768 20 >"$state/memhog.log" &
hog=$!
risen=0
for _ in $(seq 1 60); do
	now="$(actual)"
	if ((now >= settled + 512 * MIB)); then
		risen=1
		break
	fi
	sleep 1
done
wait "$hog" || true
hog=""
grep -a -q "memhog: 768 MiB held" "$state/memhog.log" || fail "the program that needed 768 MiB did not get it ($(tr '\n' ' ' <"$state/memhog.log"))"
grep -a -q "memhog: done" "$state/memhog.log" || fail "the program was not left to finish with what it held"
((risen == 1)) || fail "QEMU's actual did not rise by 512 MiB while the program needed memory - the balloon did not give back"
say "a program needing 768 MiB got all of it while the balloon gave back, and kept it to the end"

# 4. THE HOST CANCELS.
ask_for "$((2 * GIB))"
await_actual "$((2 * GIB))" "the balloon did not give everything back when the host asked for nothing"
[[ "$(held_mib)" == "0" ]] || fail "free still says the balloon holds memory ($(cat "$state/free.log"))"
say "the host asked for nothing, and the balloon gave everything back"

# 5. THE HOST IS TOLD.
qmp qom-get '{"path": "/machine/peripheral/balloon0", "property": "guest-stats"}' >"$state/stats.json" || fail "QEMU has no guest statistics"
python3 - "$state/stats.json" <<'PY' || fail "the guest statistics QEMU holds are not the machine's ($(cat "$state/stats.json"))"
import json, sys
stats = json.load(open(sys.argv[1]))["stats"]
total, free = stats.get("stat-total-memory", -1), stats.get("stat-free-memory", -1)
assert 1 << 30 < total <= 2 << 30, total
assert 0 < free < total, (free, total)
PY
say "QEMU's guest statistics name the machine's total and free memory"
say "PASS"
