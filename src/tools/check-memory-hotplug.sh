#!/usr/bin/env bash
# PLUGGED MEMORY, end to end on x86_64: a development instance of 2 GiB with a `virtio-mem-pci` device over a 1 GiB
# region in 2 MiB blocks, the host's requests made over QMP and read back there, the guest's account read with `free`.
#
#   1. THE HOST PLUGS 512 MiB IN: QEMU's plugged size reaches it, and `free`'s total grows by exactly that.
#   2. A PROGRAM USES IT: `memhog` takes more than the machine had before the plug, which only the plugged memory makes
#      possible.
#   3. THE HOST ASKS FOR IT BACK WHILE THE PROGRAM HOLDS IT: what nothing holds goes back, what the program holds stays
#      plugged - QEMU's size does not fall to nothing - and ResourceManager says memory in use is staying.
#   4. THE PROGRAM ENDS: the rest goes back, and `free`'s total is what it was before the plug.
#   5. THE HOST CHANGES ITS MIND HALF WAY: a gigabyte asked for and nothing asked for at once - the device ends with
#      nothing plugged, and `free`'s total is the boot's again.
#
# IT BOOTS ITS OWN INSTANCE in private state and takes it down from the EXIT trap. The development image is the one
# `LIBER_DEVELOPMENT=1 ./image.sh --format iso` wrote - `memhog` is in no shipping image.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."

MIB=$((1 << 20))
DEVICE=/machine/peripheral/vmemdev

fail() {
	echo "memory-hotplug: $*" >&2
	exit 1
}

say() {
	echo "memory-hotplug: $*"
}

command -v python3 >/dev/null || fail "python3 is not installed, and the lab is written in it"
[[ ! -S .build/boot/lab-ctl.sock ]] || fail "an ad-hoc lab guest is up (.build/boot/lab-ctl.sock) - take it down with ./lab.sh quit first"

state="$(mktemp -d "${TMPDIR:-/tmp}/liber-hotplug.XXXXXX")"
export LIBER_DEV_STATE="$state"
HOSTFWD_PORT="$(python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])')"
export HOSTFWD_PORT
export VIRTIO_MEM=1 MEM=2G
kept="$(pwd)/.build/logs/memory-hotplug"
rm -rf "$kept"
mkdir -p "$kept"
hog=""

cleanup() {
	local status=$?
	[[ -z "$hog" ]] || wait "$hog" 2>/dev/null || true
	cp -f "$state/dev-serial.log" "$kept/serial.log" 2>/dev/null || true
	./dev.sh down >"$state/down.log" 2>&1 || echo "memory-hotplug: teardown reported a problem (see $kept/down.log)" >&2
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

plugged() {
	qmp qom-get "{\"path\": \"$DEVICE\", \"property\": \"size\"}"
}

request() {
	qmp qom-set "{\"path\": \"$DEVICE\", \"property\": \"requested-size\", \"value\": $1}" >/dev/null
}

await_plugged() {
	local want="$1" what="$2" limit="${3:-90}" now=""
	for _ in $(seq 1 "$limit"); do
		now="$(plugged)"
		[[ "$now" == "$want" ]] && return 0
		sleep 1
	done
	fail "$what (plugged $now, wanted $want, after ${limit} s)"
}

launch() {
	./dev.sh launch --timeout 120 "$@" 2>&1
}

# `free`'s Mem row, in MiB: total, free.
memory() {
	launch free >"$state/free.log" || true
	python3 - "$state/free.log" <<'PY'
import re, sys
text = open(sys.argv[1], errors="replace").read()
m = re.search(r"Mem: +total (\d+), used \d+, free (\d+)", text)
print(f"{int(m.group(1)) >> 20} {int(m.group(2)) >> 20}" if m else "-1 -1")
PY
}

say "booting a 2 GiB development instance with a 1 GiB virtio-mem region"
./dev.sh up --timeout 400 >"$state/up.log" 2>&1 || fail "the development guest did not come up (see $kept/up.log)"
grep -a -q -F "driver.virtio-mem: online" "$state/dev-serial.log" || fail "the virtio-mem driver did not bind"
[[ "$(plugged)" == "0" ]] || fail "QEMU says memory is plugged before anything was asked"
read -r boot_total boot_free < <(memory)
((boot_total > 0)) || fail "free did not answer ($(cat "$state/free.log"))"

# 1. 512 MiB PLUGGED IN.
request "$((512 * MIB))"
await_plugged "$((512 * MIB))" "the 512 MiB the host plugged in did not come online"
read -r total free_now < <(memory)
((total == boot_total + 512)) || fail "free's total is $total MiB, not the boot's $boot_total and 512"
say "the host plugged 512 MiB in, and free's total grew from $boot_total to $total MiB"

# 2. A PROGRAM THAT NEEDS MORE THAN THE BOOT HAD.
want=$((boot_free + 256))
((want < free_now)) || fail "the machine has $free_now MiB free, too little for a program of $want MiB to prove anything"
launch memhog "$want" 30 >"$state/memhog.log" &
hog=$!
for _ in $(seq 1 60); do
	grep -a -q "memhog: $want MiB held" "$state/memhog.log" 2>/dev/null && break
	sleep 1
done
grep -a -q "memhog: $want MiB held" "$state/memhog.log" || fail "a program needing $want MiB - more than the $boot_free MiB the boot had free - did not get it"
say "a program took $want MiB, more than the $boot_free MiB the machine had free before the plug"

# 3. ASKED BACK WHILE IT IS HELD.
request 0
sleep 12
held_back="$(plugged)"
((held_back > 0)) || fail "everything was unplugged while a program held memory in it"
grep -a -q "ResourceManager: the host asks for plugged memory back and .* in use here" "$state/dev-serial.log" || fail "ResourceManager did not say plugged memory in use is staying"
say "asked for it back while the program ran, $((held_back >> 20)) MiB stayed plugged, and ResourceManager said why"

# 4. THE PROGRAM ENDS, AND THE REST GOES.
wait "$hog" || true
hog=""
grep -a -q "memhog: done" "$state/memhog.log" || fail "the program was not left to finish with what it held"
await_plugged 0 "what the program held did not go back once it ended"
read -r total _ < <(memory)
((total == boot_total)) || fail "free's total is $total MiB after everything went back, not the boot's $boot_total"
say "the program ended, the rest went back, and free's total is the boot's again"

# 5. A CHANGE OF MIND HALF WAY.
request "$((1024 * MIB))"
sleep 1
request 0
await_plugged 0 "a cancelled request did not end with nothing plugged"
read -r total _ < <(memory)
((total == boot_total)) || fail "free's total is $total MiB after a cancelled request, not the boot's $boot_total"
say "a gigabyte asked for and cancelled a second later ended with nothing plugged"
say "PASS"
