#!/usr/bin/env bash
# THE EARLIER USB HOST CONTROLLERS, end to end on x86_64: the development image on the machine `USB_LEGACY=1` builds -
# no xHCI, and UHCI, OHCI and EHCI with its three UHCI companions, every device on one of them (see
# `qemu_attach_usb_legacy`). Each engine runs under the shared USB core, and what is proved is that the core binds
# and serves through every one of them what xHCI's driver binds and serves:
#
#   BOUND          every controller online over the shared interrupt lines the firmware routes every pin to; the EHCI
#                  driver hands the full-speed keyboard on its port 3 to its companion, and the companion binds it
#   KEYS           `hidcheck usb keys` holds a focused surface's key stream, and the host plugs the keyboard behind the
#                  UHCI's hub again and types three modifiers on it - shift, right shift, control: keys that type
#                  nothing into the shell - then the one on the EHCI's port 3, which the EHCI hands to its companion
#                  again: each usage arrives, down and up
#   POINTER        `hidcheck usb pointer` watches the cursor while the host plugs the OHCI's tablet again and moves it
#                  through QMP's `input-send-event`, then the EHCI's high-speed one: the cursor follows each
#   STORAGE        `vol://usb` reads the stick's file through whichever disk the USB volume took; then the host takes
#                  the OHCI's stick out - the volume follows to the EHCI's high-speed one and reads it - takes that out
#                  too - the read is refused - and plugs a stick of its own behind the UHCI's hub, one on the OHCI and
#                  one on the EHCI, each with a file no other stick has, read through `vol://usb` after each arrival
#   GIVEN BACK     each controller's DMA memory, as `lsdev` reads its driver's Domain, is the same after a stick it
#                  carried has left as before the stick arrived: behind the UHCI's hub, on the OHCI and on the EHCI
#
# THE HOST'S HALF ANSWERS THE PROBE'S CUES: every phase prints the line the helper below waits for before it watches,
# so nothing is typed, moved or plugged ahead of a reader. The sticks the host plugs are this run's own images.
set -euo pipefail
GUEST_GATE_NAME="usb-legacy"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"

guest_gate_arch "$@"
[[ "$GUEST_ARCH" == x86_64 ]] || {
	echo "usb-legacy: NOT PERFORMED: the machine is x86_64's - a shared level line, which these controllers' INTx needs, is x86_64's"
	exit 0
}

fail() { guest_gate_fail "$@"; }

guest_gate_require_programs ehci ohci uhci hidcheck
command -v mformat >/dev/null && command -v mcopy >/dev/null || fail "mtools (mformat, mcopy) is required for the sticks this gate plugs"

sticks="$(mktemp -d "${TMPDIR:-/tmp}/liber-usb-legacy.XXXXXX")"
helper=""
kept="$root/../.build/logs/usb-legacy"
finish() {
	if [[ -n "$helper" ]]; then
		kill "$helper" 2>/dev/null || true
	fi
	rm -rf "$kept"
	mkdir -p "$kept"
	cp -f "$sticks/host.log" "$kept/" 2>/dev/null || true
	if [[ -n "$GUEST_LOG" ]]; then
		cp -f "$GUEST_LOG" "$kept/guest.log" 2>/dev/null || true
	fi
	# AND QEMU'S OWN WORDS, which are where an emulated controller that gave up says so.
	if [[ -n "$guest_gate_work" ]]; then
		cp -f "$guest_gate_work/run" "$kept/run.log" 2>/dev/null || true
		cp -f "$guest_gate_work/driver" "$kept/console.log" 2>/dev/null || true
	fi
	rm -rf "$sticks"
	guest_gate_cleanup
}
trap finish EXIT

# THE STICKS THE HOST PLUGS: a FAT image each, carrying one file no other stick has.
for name in hub ohci ehci; do
	truncate -s 16M "$sticks/$name.img"
	mformat -i "$sticks/$name.img" :: >/dev/null
	printf 'the %s stick, plugged after the boot\n' "$name" | mcopy -i "$sticks/$name.img" - "::$name.txt"
done

export USB_LEGACY=1
export GUEST_GATE_SECONDS="${GUEST_GATE_SECONDS:-420}"
export GUEST_GATE_TIMEOUT="${GUEST_GATE_TIMEOUT:-600}"
# THE COMPLETION LINE: the script's last line prints `usb-legacy-done`, after which nothing this gate needs can
# arrive. It also refuses a panic, which a late line could carry, so it keeps a settle window of 10 s after it.
export GUEST_GATE_DONE="usb-legacy-done" GUEST_GATE_SETTLE=10
qmp_socket="$GUEST_GATE_QMP"

# How many lines of the guest's log hold `text`, now - zero before the log exists.
seen() {
	local count
	count="$(grep -a -c -F -- "$1" "$guest_gate_work/guest" 2>/dev/null || true)"
	echo "${count:-0}"
}

# Wait, bounded, until the guest's log holds `text` more than `baseline` times; false when it never does.
wait_for() {
	local text="$1" baseline="${2:-0}" polls=0
	until (($(seen "$text") > baseline)); do
		sleep 0.2
		polls=$((polls + 1))
		((polls < 3000)) || return 1
	done
}

# QMP COMMANDS on one connection, each a JSON object on its own argument; every answer is logged.
qmp() {
	python3 - "$qmp_socket" "$@" <<'EOF' >>"$sticks/host.log" 2>&1
import json, socket, sys, time
s = socket.socket(socket.AF_UNIX)
s.settimeout(10)
s.connect(sys.argv[1])
f = s.makefile('rw')
def answer():
	while True:
		line = json.loads(f.readline())
		if 'event' not in line:
			return line
answer()
f.write(json.dumps({'execute': 'qmp_capabilities'}) + '\n')
f.flush()
answer()
for request in sys.argv[2:]:
	command = json.loads(request)
	if command.get('execute') == 'sleep':
		time.sleep(command['arguments']['seconds'])
		continue
	f.write(json.dumps(command) + '\n')
	f.flush()
	print(command.get('execute'), json.dumps(answer()))
EOF
}

# QEMU FEEDS ONE KEYBOARD AND ONE POINTER, and which one is QEMU's choice rather than the gate's: the keyboard realized
# last, and the pointer the guest began polling last. Naming the device in `input-send-event` would be the way to aim,
# and this QEMU aborts on it - its lookup by device name reads a property its fixed text console does not have - so each
# phase PLUGS ITS DEVICE AGAIN instead: the one taken out and a new one put on the same port, which makes it the one QEMU
# feeds once the guest has bound it, and is a departure and an arrival through the controller besides.
replug() {
	local id="$1" driver="$2" bus="$3" port="$4" extra="${5:-}"
	printf '{"execute": "device_del", "arguments": {"id": "%s"}}\n' "$id"
	printf '{"execute": "sleep", "arguments": {"seconds": 2}}\n'
	printf '{"execute": "device_add", "arguments": {"driver": "%s", "bus": "%s", "port": "%s", "id": "%s"%s}}\n' "$driver" "$bus" "$port" "$id" "$extra"
	printf '{"execute": "sleep", "arguments": {"seconds": 5}}\n'
}

# THREE MODIFIERS, down and up, on the keyboard QEMU feeds - keys that type nothing into the shell.
type_keys() {
	local key down
	for key in shift shift_r ctrl; do
		for down in true false; do
			printf '{"execute": "input-send-event", "arguments": {"events": [{"type": "key", "data": {"down": %s, "key": {"type": "qcode", "data": "%s"}}}]}}\n' "$down" "$key"
			printf '{"execute": "sleep", "arguments": {"seconds": 0.3}}\n'
		done
	done
}

# THE POINTER QEMU FEEDS, moved across the screen in absolute positions - from `base`, so two phases never land on one
# position and the probe, which counts the positions it had not seen, counts each phase's own.
move_pointer() {
	local base="$1" step position
	for step in $(seq 1 10); do
		position=$((base + step * 2800))
		printf '{"execute": "input-send-event", "arguments": {"events": [{"type": "abs", "data": {"axis": "x", "value": %s}}, {"type": "abs", "data": {"axis": "y", "value": %s}}]}}\n' "$position" "$position"
		printf '{"execute": "sleep", "arguments": {"seconds": 0.3}}\n'
	done
}

# One QMP session over the requests a function printed, one per line.
qmp_lines() {
	local requests=()
	mapfile -t requests
	qmp "${requests[@]}"
}

unplug() {
	qmp "{\"execute\": \"device_del\", \"arguments\": {\"id\": \"$1\"}}"
}

# A STICK OF THIS RUN'S, plugged: its image as a block node, and a `usb-storage` on a bus and port.
plug() {
	local name="$1" bus="$2" port="$3"
	qmp "{\"execute\": \"blockdev-add\", \"arguments\": {\"driver\": \"raw\", \"node-name\": \"plug-$name\", \"file\": {\"driver\": \"file\", \"filename\": \"$sticks/$name.img\"}}}" \
		"{\"execute\": \"device_add\", \"arguments\": {\"driver\": \"usb-storage\", \"bus\": \"$bus\", \"port\": \"$port\", \"drive\": \"plug-$name\", \"id\": \"plug-$name\"}}"
}

# THE HOST'S HALF OF EACH PHASE, answering the probe's cue in the order the commands below run: the keyboard behind the
# UHCI's hub plugged again behind it, then the full-speed keyboard plugged again on the EHCI's port 3 - which hands it to
# its companion again - each typed on; the OHCI's tablet and the EHCI's high-speed one plugged again, each moved.
host_side() {
	wait_for "hidcheck: usb keys - type now" 0 || return 0
	{
		replug lkbd usb-kbd luhci.0 1.1
		type_keys
	} | qmp_lines
	wait_for "hidcheck: usb keys - type now" 1 || return 0
	{
		replug lkbd2 usb-kbd lehci.0 3 ', "usb_version": 1'
		type_keys
	} | qmp_lines
	wait_for "hidcheck: usb pointer - move now" 0 || return 0
	{
		replug ltab usb-tablet lohci.0 1
		move_pointer 2000
	} | qmp_lines
	wait_for "hidcheck: usb pointer - move now" 1 || return 0
	{
		replug ltab2 usb-tablet lehci.0 2
		move_pointer 1000
	} | qmp_lines
	wait_for "hidcheck: usb wait - act now" 0 || return 0
	unplug lstick
	wait_for "hidcheck: usb wait - act now" 1 || return 0
	unplug lstick2
	wait_for "hidcheck: usb wait - act now" 2 || return 0
	plug hub luhci.0 1.2
	wait_for "hidcheck: usb wait - act now" 3 || return 0
	unplug plug-hub
	plug ohci lohci.0 2
	wait_for "hidcheck: usb wait - act now" 4 || return 0
	unplug plug-ohci
	plug ehci lehci.0 1
	wait_for "hidcheck: usb wait - act now" 5 || return 0
	unplug plug-ehci
}

host_side &
helper=$!
# THE DMA MEMORY EACH CONTROLLER HOLDS is read with `lsdev` - its driver's Domain, through DeviceManager - once with
# no stick on any of them, and again after each stick plugged since has left its controller: behind the UHCI's hub at
# the fourth pause, on the OHCI at the fifth, on the EHCI at the sixth. Each reading is marked, so the checks below
# read the right one.
guest_gate_run $'hidcheck usb keys\nhidcheck usb keys\nhidcheck usb pointer\nhidcheck usb pointer\ncat vol://usb/hello.txt\nhidcheck usb wait\ncat vol://usb/hello.txt\nhidcheck usb wait\ncat vol://usb/hello.txt\necho usb-legacy-dma before\nlsdev\nhidcheck usb wait\ncat vol://usb/hub.txt\nhidcheck usb wait\necho usb-legacy-dma uhci\nlsdev\ncat vol://usb/ohci.txt\nhidcheck usb wait\necho usb-legacy-dma ohci\nlsdev\ncat vol://usb/ehci.txt\nhidcheck usb wait\necho usb-legacy-dma ehci\nlsdev\necho usb-legacy-done' ""
wait "$helper" 2>/dev/null || true
helper=""

expect() {
	grep -aqE -- "$1" "$GUEST_LINES" || fail "$2 (no line matching /$1/)"
}

refuse() {
	! grep -aqE -- "$1" "$GUEST_LINES" || fail "$2 (a line matches /$1/)"
}

grep -aq 'usb-legacy-done' "$GUEST_LINES" || fail "the script did not run to its end"
refuse 'panicked at' "nothing panicked"

# BOUND.
expect 'driver\.ehci: online \(00:1d\.7, [0-9]+ device\(s\)\)' "the EHCI controller must come online"
expect 'driver\.ehci: port 3 carries a full-speed device - handed to its companion controller' "the full-speed keyboard on EHCI port 3 must be handed to its companion"
expect 'driver\.ohci: online \(' "the OHCI controller must come online"
uhcis="$(grep -acE 'driver\.uhci: online \(' "$GUEST_LINES" || true)"
((uhcis >= 4)) || fail "the PIIX3 UHCI and the EHCI's three companions must each come online (saw $uhcis)"
expect 'driver\.uhci: port 1 \(hub [0-9]+\.1\) 0627:' "the keyboard behind the UHCI's hub must be addressed through the hub"
echo "usb-legacy: every controller bound, and the EHCI handed its full-speed keyboard to its companion"

# KEYS: both keyboards, each modifier down and up.
keys="$(grep -a -o 'hidcheck: key 0x[0-9a-f]* [a-z]*' "$GUEST_LINES" || true)"
for usage in 0xe1 0xe5 0xe0; do
	for edge in down up; do
		count="$(grep -c "key $usage $edge" <<<"$keys" || true)"
		((count >= 2)) || fail "usage $usage $edge did not arrive from both keyboards (saw it $count time(s))"
	done
done
expect 'hidcheck: usb keys: 6 event\(s\)' "a keyboard's three modifiers must arrive as six events"
echo "usb-legacy: the keyboard behind the UHCI's hub and the one on the EHCI's companion delivered every key, down and up"

# POINTER: both tablets moved the cursor.
moves="$(grep -a -o 'hidcheck: usb pointer: [0-9]* position' "$GUEST_LINES" | grep -o '[0-9]* position' | grep -o '[0-9]*' || true)"
[[ "$(wc -l <<<"$moves")" -ge 2 ]] || fail "both pointer phases must report"
while read -r count; do
	((count >= 3)) || fail "a tablet's moves did not move the cursor (a phase saw $count position(s))"
done <<<"$moves"
echo "usb-legacy: the OHCI's tablet and the EHCI's high-speed tablet each moved the cursor"

# STORAGE.
hellos="$(grep -ac 'Hello from the OS ramdisk!' "$GUEST_LINES" || true)"
((hellos >= 2)) || fail "vol://usb must read the stick's file at the boot and again from the EHCI's stick after the OHCI's left (saw $hellos)"
expect 'driver\.ohci: the disk left' "the OHCI's stick must be seen leaving"
expect 'driver\.ehci: the disk left' "the EHCI's stick must be seen leaving"
expect 'cat: vol://usb/hello\.txt: cannot open' "with both sticks out, the read must be refused"
expect 'the hub stick, plugged after the boot' "the stick plugged behind the UHCI's hub must be read"
expect 'driver\.uhci: the disk left' "and seen leaving"
expect 'the ohci stick, plugged after the boot' "the stick plugged on the OHCI must be read"
expect 'the ehci stick, plugged after the boot' "the stick plugged on the EHCI must be read"
echo "usb-legacy: vol://usb followed every stick out and in - behind the UHCI's hub, on the OHCI and on the EHCI - and read each one"

# GIVEN BACK: the DMA memory each controller's driver holds, after the stick it carried left, is what it held before
# that stick arrived. The PIIX3 UHCI is the one not on the EHCI's slot 1d, whose three UHCIs are its companions.
python3 "$root/tools/usb-dma-readings.py" "$GUEST_LINES" usb-legacy-dma before uhci:uhci ohci:ohci ehci:ehci ||
	fail "a controller kept DMA memory a stick that left had held"
echo "usb-legacy: every controller gave back what each stick it carried had held"
echo "usb-legacy: PASS"
