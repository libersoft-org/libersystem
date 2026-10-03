#!/usr/bin/env bash
# HIBERNATION, end to end on x86_64 q35 with S4 offered - set explicitly (`-global ICH9-LPC.disable_s4=0`), never trusted
# as a default - `swtpm` behind QEMU's CRB front-end, and a system disk SET UP FOR IT: a GPT whose first partition is the
# system volume and whose second is a hibernation partition a little larger than the guest's memory.
#
# THE ORACLES ARE THE HOST'S. QEMU exits on S4 exactly as on a soft-off, so this script HOLDS ITS OWN QMP CONNECTION from
# before each request until QEMU closes it, and reads `SUSPEND_DISK` on it - the one evidence that tells the two apart;
# where S4 is not offered, `SHUTDOWN` with no `SUSPEND_DISK`, and the kernel's line naming the power-off path. The image's
# header is read from the disk file between the boots: its magic and its state, valid or invalidated.
#
#   1. HIBERNATE AND RESTORE. A counter runs in the background at the serial shell, a line every 100 ms; `sleepctl
#      hibernate`; the image is written and the machine enters S4. A new QEMU on the same disk and the same TPM restores
#      it: the header is invalidated, the transaction ends "slept and woke" woken by the restore, and the counter goes on
#      at its next value with its monotonic clock moved by less than the time off and its boot-time clock by at least it -
#      the same program, the same counter.
#   2. THE BOOT AFTER A RESUME sees no image: the machine rebooted boots fresh, and `sleepctl status` says so.
#   3. WHERE S4 IS NOT OFFERED hibernation powers the machine off, which the kernel names; then that image, ONE BYTE OF A
#      CHUNK FLIPPED on the host, is refused as modified and the machine boots fresh.
#   4. AN IMAGE FROM ANOTHER MACHINE is refused - the next boot has another core count.
#   5. AN IMAGE FROM ANOTHER SYSTEM IMAGE is refused - the next boot names a development variant, which the kernel's
#      digest of the system image takes in (see the kernel's `development_variant`): this gate cannot boot a second build
#      of one tree.
#   6. HYBRID SLEEP: the image written, then S3 instead of the power-off. Woken by the host, the machine runs on and the
#      image is discarded. Again, and QEMU killed while suspended - the battery that died in the night: the next boot
#      restores the image, and the counter goes on.
#   7. WITHOUT A TPM: hibernation is set up, with the warning in `sleepctl status`; an image is written with its key in the
#      clear - the line saying so carries the warning, and the header says so - and a new QEMU, still without a TPM,
#      restores it with the counter going on. Then such an image, written again, is refused on a boot whose TPM seals.
#   8. A REFUSAL AFTER EVERY BINDING WAS STOPPED: the development switch names the kernel's replacement absent
#      (`opt/org.libersystem/absent` = `replacement`), so an image read, authenticated and held by the kernel is refused
#      at its commit, after the restore's door stopped every binding - the disks among them. The machine restarts, and
#      its next boot - the same QEMU - is a fresh one that finds no image.
# AFTER EVERY REFUSAL the header is invalidated on the disk and `sleepctl status` names why.
#
# IT BOOTS ITS OWN INSTANCES in private state, one at a time, and takes the last one down from the EXIT trap.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."

fail() {
	echo "hibernate: $*" >&2
	exit 1
}

say() {
	echo "hibernate: $*"
}

command -v python3 >/dev/null || fail "python3 is not installed, and the lab is written in it"
command -v swtpm >/dev/null || fail "swtpm is not installed - setup.sh installs it, and this gate fails rather than skips without it"
command -v sgdisk >/dev/null || fail "sgdisk is not installed"
[[ ! -S .build/boot/lab-ctl.sock ]] || fail "an ad-hoc lab guest is up (.build/boot/lab-ctl.sock) - take it down with ./lab.sh quit first"

# THE GUEST: memory the hibernation partition exceeds, and a core count the other-machine case changes.
MEM_MIB=2048
CORES=4
HIBERNATION_MIB=$((MEM_MIB + 64))

state="$(mktemp -d "${TMPDIR:-/tmp}/liber-hibernate.XXXXXX")"
export LIBER_DEV_STATE="$state"
HOSTFWD_PORT="$(python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])')"
export HOSTFWD_PORT
export MEM="${MEM_MIB}M"
export PYTHONUNBUFFERED=1
S_OFFERED="-global ICH9-LPC.disable_s3=0 -global ICH9-LPC.disable_s4=0"
kept="$(pwd)/.build/logs/hibernate"
rm -rf "$kept"
mkdir -p "$kept"
tpm_pid=""
watcher=0
boots=0

cleanup() {
	local status=$?
	kill $(jobs -p) 2>/dev/null || true
	./dev.sh down >"$state/down.log" 2>&1 || echo "hibernate: teardown reported a problem (see $kept/down.log)" >&2
	if [[ -n "$tpm_pid" ]]; then
		kill "$tpm_pid" 2>/dev/null || true
		wait "$tpm_pid" 2>/dev/null || true
	fi
	cp -f "$state"/*.log "$kept/" 2>/dev/null || true
	rm -f .build/boot/*"-dev-$(basename "$state")"* 2>/dev/null || true
	rm -rf "$state"
	return "$status"
}
trap cleanup EXIT

serial="$state/dev-serial.log"

seen() {
	grep -a -c -F -- "$1" "$serial" 2>/dev/null || true
}

await_line() {
	local needle="$1" what="$2" baseline="$3" limit="${4:-120}"
	for _ in $(seq 1 "$limit"); do
		if (($(seen "$needle") > baseline)); then
			return 0
		fi
		sleep 1
	done
	fail "$what (waited ${limit} s for a new line matching: $needle)"
}

launch() {
	./dev.sh launch --timeout 60 "$@" 2>&1
}

# A LINE TYPED AT THE SERIAL CONSOLE, byte by byte through the console socket, as a person's terminal sends it.
type_serial() {
	python3 - "$state/dev-console.sock" "$1" <<'EOF'
import socket, sys, threading, time
sock = socket.socket(socket.AF_UNIX)
sock.connect(sys.argv[1])
sock.sendall(b'ATTACH rw\n')
sock.settimeout(5)
granted = b''
while not granted.endswith(b'\n'):
	granted += sock.recv(1)
if not granted.startswith(b'OK rw'):
	sys.exit(f'the console refused the attach: {granted!r}')
def drain():
	try:
		while sock.recv(65536):
			pass
	except OSError:
		pass
threading.Thread(target=drain, daemon=True).start()
for byte in sys.argv[2].encode() + b'\r':
	sock.sendall(bytes([byte]))
	time.sleep(0.02)
time.sleep(0.5)
EOF
}

# ONE QMP COMMAND on a connection of its own, events skipped.
qmp() {
	python3 - "$state/qemu-qmp.sock" "$@" <<'EOF'
import json, socket, sys
sock = socket.socket(socket.AF_UNIX)
sock.settimeout(10)
sock.connect(sys.argv[1])
stream = sock.makefile('rw')
def answer():
	while True:
		line = json.loads(stream.readline())
		if 'event' not in line:
			return line
def command(execute, arguments=None):
	stream.write(json.dumps({'execute': execute, 'arguments': arguments or {}}) + '\n')
	stream.flush()
	return answer()
answer()
command('qmp_capabilities')
result = command(sys.argv[2])
if 'error' in result:
	print(result['error'].get('desc', 'error'))
	sys.exit(1)
if sys.argv[2] == 'query-status':
	print(result['return']['status'])
EOF
}

run_state() {
	local now
	for _ in 1 2 3; do
		if now="$(qmp query-status 2>/dev/null)"; then
			echo "$now"
			return 0
		fi
		sleep 0.5
	done
	echo "unanswered"
}

await_state() {
	local want="$1" limit="$2" what="$3"
	for _ in $(seq 1 "$((limit * 2))"); do
		[[ "$(run_state)" == "$want" ]] && return 0
		sleep 0.5
	done
	fail "$what: QEMU was $(run_state), not $want, after ${limit} s"
}

# THE HELD QMP CONNECTION: opened before a request, and read until QEMU closes it at its exit - every event written down
# with the host's time, then `closed`.
hold_qmp() {
	local events="$1"
	python3 - "$state/qemu-qmp.sock" "$events" <<'EOF' &
import json, socket, sys, time
sock = socket.socket(socket.AF_UNIX)
sock.connect(sys.argv[1])
stream = sock.makefile('rw')
stream.readline()
stream.write(json.dumps({'execute': 'qmp_capabilities'}) + '\n')
stream.flush()
with open(sys.argv[2], 'w') as out:
	for line in stream:
		message = json.loads(line)
		if 'return' in message and not out.tell():
			out.write('connected\n')
		elif 'event' in message:
			out.write(f"{time.time():.3f} {message['event']} {json.dumps(message.get('data', {}))}\n")
		out.flush()
	out.write('closed\n')
EOF
	watcher=$!
	for _ in $(seq 1 50); do
		grep -q '^connected' "$events" 2>/dev/null && return 0
		sleep 0.2
	done
	fail "the held QMP connection was not taken"
}

# QEMU GONE: the held connection closed, within `limit` seconds.
await_exit() {
	local events="$1" limit="$2" what="$3"
	for _ in $(seq 1 "$limit"); do
		grep -q '^closed' "$events" 2>/dev/null && return 0
		sleep 1
	done
	fail "$what: QEMU did not exit within ${limit} s (events: $(tr '\n' ' ' <"$events"))"
}

# ------------------------------------------------------------------ the disk and the TPM

# THE SYSTEM DISK SET UP FOR HIBERNATION: the development medium's paired volume in a LiberFS partition, and the
# hibernation partition after it, both sparse. Built once; every boot of this gate attaches it.
disk="$state/system-disk.img"
make_disk() {
	local volume=".build/boot/system-volume-bootable-x86_64.img"
	[[ -f "$volume" ]] || fail "no bootable system volume at $volume"
	local volume_mib=$((($(stat -c%s "$volume") + 1048575) / 1048576 + 64))
	truncate -s "$((volume_mib + HIBERNATION_MIB + 4))M" "$disk"
	sgdisk "$disk" -n "1:2048:+${volume_mib}M" -t 1:4C424653-0001-4000-8000-4C6962657246 -c 1:system >/dev/null
	sgdisk "$disk" -n "2:0:+${HIBERNATION_MIB}M" -t 2:4C424653-0002-4000-8000-4C6962657246 -c 2:hibernation >/dev/null
	dd if="$volume" of="$disk" bs=1M seek=1 conv=notrunc status=none
	area_at=$(($(sgdisk -i 2 "$disk" | awk '/^First sector:/ {print $3}') * 512))
}

# THE HEADER ON THE DISK: `image` while it holds an image, `empty` once invalidated, `none` where nothing was written.
header() {
	python3 - "$disk" "$area_at" <<'EOF'
import struct, sys
with open(sys.argv[1], 'rb') as disk:
	disk.seek(int(sys.argv[2]))
	block = disk.read(16)
if block[:8] != b'LSHIBRN1':
	print('none')
else:
	print({1: 'image', 0: 'empty'}.get(struct.unpack_from('<I', block, 12)[0], 'unknown'))
EOF
}

# HOW THE HEADER KEEPS THE IMAGE'S KEY: `tpm` (sealed) or `clear`.
protection() {
	python3 - "$disk" "$area_at" <<'EOF'
import struct, sys
with open(sys.argv[1], 'rb') as disk:
	disk.seek(int(sys.argv[2]))
	block = disk.read(24)
print({1: 'tpm', 2: 'clear'}.get(struct.unpack_from('<I', block, 20)[0], 'unknown'))
EOF
}

# ONE BYTE OF CHUNK 2's DATA flipped on the host: the image's authentication must fail.
modify_image() {
	python3 - "$disk" "$area_at" <<'EOF'
import sys
at = int(sys.argv[2]) + 4096 + 2 * (4096 + 256 * 4096) + 4096 + 1234
with open(sys.argv[1], 'r+b') as disk:
	disk.seek(at)
	byte = disk.read(1)
	disk.seek(at)
	disk.write(bytes([byte[0] ^ 0x40]))
EOF
}

tpm_state="$state/tpm"
start_tpm() {
	local dir="$1"
	mkdir -p "$dir"
	swtpm socket --tpm2 --tpmstate "dir=$dir" --ctrl "type=unixio,path=$state/swtpm.sock" --log "file=$state/swtpm.log" &
	tpm_pid="$!"
	for _ in $(seq 1 100); do
		[[ -S "$state/swtpm.sock" ]] && return 0
		sleep 0.05
	done
	fail "swtpm did not open its control socket"
}

stop_tpm() {
	if [[ -n "$tpm_pid" ]]; then
		kill "$tpm_pid" 2>/dev/null || true
		wait "$tpm_pid" 2>/dev/null || true
		tpm_pid=""
	fi
	rm -f "$state/swtpm.sock"
}

# ------------------------------------------------------------------ one boot

# A BOOT: the last one's QEMU down, its log kept aside, the TPM started over its state (or none), and the instance up
# with `extra` QEMU arguments and `cores` cores. `label` names the logs.
boot() {
	local label="$1" extra="$2" tpm="${3:-$tpm_state}" cores="${4:-$CORES}"
	boots=$((boots + 1))
	./dev.sh down >"$state/down-$label.log" 2>&1 || true
	stop_tpm
	[[ -f "$serial" ]] && cp -f "$serial" "$state/serial-before-$label.log"
	: >"$serial" 2>/dev/null || true
	if [[ "$tpm" == none ]]; then
		unset TPM_SOCKET TPM_FRONTEND
	else
		start_tpm "$tpm"
		export TPM_SOCKET="$state/swtpm.sock" TPM_FRONTEND=crb
	fi
	export RUN_DISK="$disk" QEMU_EXTRA="$extra" SMP="$cores"
	say "boot $boots ($label)"
	./dev.sh up --timeout 900 >"$state/up-$label.log" 2>&1 || fail "$label: the instance did not come up (see $kept/up-$label.log)"
}

status_line() {
	launch sleepctl status >"$state/status-$1.log" 2>&1 || true
	cat "$state/status-$1.log"
}

# A HIBERNATION ASKED, its QEMU held until it exits: the answer, and the image written.
hibernate() {
	local label="$1" events="$state/qmp-$1.log" written
	written=$(seen "HibernationService: the image is written")
	hold_qmp "$events"
	launch sleepctl hibernate >"$state/sleepctl-$label.log" 2>&1 &
	await_line "HibernationService: the image is written" "$label: no image was written" "$written" 600
	await_exit "$events" 120 "$label"
	./dev.sh down >"$state/down-$label.log" 2>&1 || true
	[[ "$(header)" == image ]] || fail "$label: the partition's header does not hold an image after the write ($(header))"
}

# THE REFUSAL at the boot after, by its reason: the machine boots fresh, the header is invalidated, and the status says
# why.
refused() {
	local label="$1" why="$2"
	await_line "HibernationService: the image is refused, and the machine boots fresh - " "$label: the image was not refused" 0 600
	refused="$(grep -a -F "HibernationService: the image is refused" "$serial" || true)"
	grep -q -F -- "$why" <<<"$refused" || fail "$label: the image was refused, but not as '$why': $refused"
	await_line "ServiceManager: restore: no image is restored - the boot goes on" "$label: the boot did not go on after the refusal" 0 60
	[[ "$(header)" == empty ]] || fail "$label: the refused image's header was not invalidated ($(header))"
	grep -q "the image found at this boot: refused: .*$why" <<<"$(status_line "$label")" || fail "$label: the status does not name the refusal: $(cat "$state/status-$label.log")"
	say "$label: refused ($why), the machine booted fresh and the header is invalidated"
}

# THE COUNTER: started in the background at the serial shell, its lines REDIRECTED into a file - a line every 100 ms on
# the serial console would keep the instance's readiness from ever seeing a quiet prompt - which the redirection
# publishes only when the program ends, cleanly: one file for its whole life, before the time off and after it.
COUNTER_S=60
start_counter() {
	local name="$1"
	type_serial "sleepcheck count $COUNTER_S > $name &" || fail "the serial console took no input"
	sleep 5
}

# THE COUNTER ACROSS THE TIME OFF, read from its file once it has ended: one unbroken run of values from 1 - the same
# program, never started again - whose widest step of the boot-time clock is the time off, at least `off_ms`, while
# the monotonic clock moved by less than five seconds across it.
counter_goes_on() {
	local label="$1" name="$2" off_ms="$3" out
	for _ in $(seq 1 $((COUNTER_S * 2 + 60))); do
		out="$(launch cat "$name")"
		grep -q "sleepcheck: count done" <<<"$out" && break
		sleep 1
	done
	printf '%s\n' "$out" >"$state/counter-$label.log"
	python3 - "$state/counter-$label.log" "$off_ms" "$label" <<'EOF' || fail "$label: the counter across the time off (see $kept/counter-$label.log)"
import re, sys
path, off_ms, label = sys.argv[1], int(sys.argv[2]), sys.argv[3]
text = open(path, 'rb').read().decode('utf-8', 'replace')
if 'sleepcheck: count done' not in text:
	sys.exit('the counter never finished, so its file was never published')
rows = [tuple(int(value) for value in found) for found in re.findall(r'sleepcheck: count (\d+) mono-ms (\d+) boot-ms (\d+)', text)]
if not rows or [row[0] for row in rows] != list(range(1, len(rows) + 1)):
	sys.exit('the counter did not run one unbroken sequence from 1 - it skipped, repeated or started again')
gap = max(range(1, len(rows)), key=lambda at: rows[at][2] - rows[at - 1][2])
mono, boot = rows[gap][1] - rows[gap - 1][1], rows[gap][2] - rows[gap - 1][2]
if boot < off_ms:
	sys.exit(f'the boot-time clock moved {boot} ms at its widest step, under the {off_ms} ms off')
if mono > 5000:
	sys.exit(f'the monotonic clock moved {mono} ms across the time off')
print(f'hibernate: {label}: count {rows[gap - 1][0]} then {rows[gap][0]}: monotonic +{mono} ms, boot-time +{boot} ms')
EOF
}

# ------------------------------------------------------------------ the run

say "building the development image and the disk set up for hibernation"
# THE BUILD THE INSTANCE'S OWN `up` RUNS (the development medium, signed for the harness's DMA mode, debug data kept), so
# the volume copied onto the disk is the one that medium is paired with, and every `up` below finds it built.
LIBER_DEVELOPMENT=1 ./image.sh --format iso --dma-mode harness --strip none >"$state/image.log" 2>&1 || fail "the development image did not build (see $kept/image.log)"
make_disk
[[ "$(header)" == none ]] || fail "a new hibernation partition is not empty"

# 1. HIBERNATE AND RESTORE.
boot fresh "$S_OFFERED"
status="$(status_line fresh)"
grep -q "hibernation: set up" <<<"$status" || fail "hibernation is not set up on the disk set up for it: $status"
grep -q "the image found at this boot: none" <<<"$status" || fail "a fresh disk's boot names an image: $status"
start_counter counter-first.txt
hibernate first
grep -q '"SUSPEND_DISK"\|SUSPEND_DISK' "$state/qmp-first.log" || fail "first: QEMU reported no SUSPEND_DISK - the machine did not enter S4 (events: $(tr '\n' ' ' <"$state/qmp-first.log"))"
grep -a -q -F "hibernate: the image is written - the registered \\_S4" "$serial" || fail "first: the kernel did not name the registered \\_S4"
off_started=$(date +%s%3N)
say "hibernated - the image is written, QEMU reported SUSPEND_DISK and exited"
boot restore "$S_OFFERED"
await_line "HibernationService: the image is authenticated and in the kernel" "restore: the image was not restored" 0 600
off_ms=$(($(date +%s%3N) - off_started))
await_line "ServiceManager: sleep: the transaction ended - slept and woke" "restore: the restored machine's transaction did not end" 0 300
grep -a -q -F "sleep: resumed (the restore of a hibernation image" "$serial" || fail "restore: the kernel did not resume as the image restored"
[[ "$(header)" == empty ]] || fail "restore: the header was not invalidated before the jump ($(header))"
counter_goes_on restore counter-first.txt "$((off_ms / 2))"
say "restored - the same program, the same counter, and the header invalidated"

# 2. THE BOOT AFTER A RESUME.
boot after-restore "$S_OFFERED"
grep -q "the image found at this boot: none" <<<"$(status_line after-restore)" || fail "after-restore: the boot after a resume found an image: $(cat "$state/status-after-restore.log")"
(($(seen "HibernationService: an image of") == 0)) || fail "after-restore: the boot after a resume read an image"
say "the boot after the resume found no image to resume"

# 3. S4 NOT OFFERED: powered off, and that image modified.
boot no-s4 "-global ICH9-LPC.disable_s3=0 -global ICH9-LPC.disable_s4=1"
hibernate no-s4
grep -q 'SUSPEND_DISK' "$state/qmp-no-s4.log" && fail "no-s4: QEMU reported SUSPEND_DISK where S4 is not offered"
grep -q 'SHUTDOWN' "$state/qmp-no-s4.log" || fail "no-s4: QEMU reported no SHUTDOWN"
grep -a -q -F "hibernate: the image is written - no \\_S4 registered, so the machine is powered off" "$serial" || fail "no-s4: the kernel did not name the power-off path"
say "where S4 is not offered the machine powered off with its image written"
modify_image
boot modified "$S_OFFERED"
refused modified "modified"

# 4. ANOTHER MACHINE - an image written by this system image on four cores, booted on two. FIRST, because the header
#    is held to the system image before the hardware: the image of the next case is written on the two cores this one
#    boots, so the only difference it then meets is the system image's.
hibernate other-machine-image
boot other-machine "$S_OFFERED" "$tpm_state" "$((CORES / 2))"
refused other-machine "other hardware"

# 5. ANOTHER SYSTEM IMAGE - the same two cores, the development variant named.
hibernate other-system-image
boot other-system "$S_OFFERED -fw_cfg name=opt/org.libersystem/system-variant,string=other" "$tpm_state" "$((CORES / 2))"
refused other-system "another system image"

# 6. HYBRID SLEEP: woken, the image discarded; then the battery that died.
boot hybrid "$S_OFFERED"
ended=$(seen "ServiceManager: sleep: the transaction ended")
written=$(seen "HibernationService: the image is written")
launch sleepctl hibernate hybrid >"$state/sleepctl-hybrid.log" 2>&1 &
await_line "HibernationService: the image is written" "hybrid: no image was written" "$written" 600
await_state suspended 120 "hybrid: S3 after the image"
[[ "$(header)" == image ]] || fail "hybrid: the header holds no image while the machine is in S3 ($(header))"
qmp system_wakeup >/dev/null || fail "QEMU refused system_wakeup"
await_state running 30 "hybrid after system_wakeup"
await_line "ServiceManager: sleep: the transaction ended - slept and woke" "hybrid: the transaction did not end" "$ended" 180
await_line "HibernationService: the image is discarded" "hybrid: the image was not discarded after the S3 resume" 0 60
[[ "$(header)" == empty ]] || fail "hybrid: the image is still valid after the S3 resume ($(header))"
say "hybrid: the image written, S3, and discarded once the machine ran on"
start_counter counter-hybrid.txt
written=$(seen "HibernationService: the image is written")
launch sleepctl hibernate hybrid >"$state/sleepctl-hybrid-lost.log" 2>&1 &
await_line "HibernationService: the image is written" "hybrid-lost: no image was written" "$written" 600
await_state suspended 120 "hybrid-lost: S3 after the image"
off_started=$(date +%s%3N)
qmp quit >/dev/null 2>&1 || true
boot hybrid-restore "$S_OFFERED"
await_line "HibernationService: the image is authenticated and in the kernel" "hybrid-restore: the image was not restored" 0 600
off_ms=$(($(date +%s%3N) - off_started))
await_line "ServiceManager: sleep: the transaction ended - slept and woke" "hybrid-restore: the transaction did not end" 0 300
counter_goes_on hybrid-restore counter-hybrid.txt "$((off_ms / 2))"
say "hybrid: the power lost in S3, the next boot restored the image"

# 7. WITHOUT A TPM: set up and warned, written in the clear and restored; and refused where a TPM seals.
NO_TPM_WARNING="WARNING: no TPM is bound to seal the image's key, so the image's key is written beside it in the clear"
boot no-tpm "$S_OFFERED" none
status="$(status_line no-tpm)"
grep -q -F "hibernation: set up - $NO_TPM_WARNING" <<<"$status" || fail "no-tpm: the status does not say hibernation is set up with the warning: $status"
start_counter counter-no-tpm.txt
hibernate no-tpm
grep -a -q -F "its key NOT sealed - $NO_TPM_WARNING" "$serial" || fail "no-tpm: the image written did not say its key is in the clear, with the warning"
[[ "$(protection)" == clear ]] || fail "no-tpm: the header does not keep the key in the clear ($(protection))"
off_started=$(date +%s%3N)
boot no-tpm-restore "$S_OFFERED" none
await_line "HibernationService: its key was written in the clear - no TPM sealed it" "no-tpm-restore: the image's key was not taken from the header" 0 600
await_line "HibernationService: the image is authenticated and in the kernel" "no-tpm-restore: the image was not restored" 0 600
off_ms=$(($(date +%s%3N) - off_started))
await_line "ServiceManager: sleep: the transaction ended - slept and woke" "no-tpm-restore: the restored machine's transaction did not end" 0 300
[[ "$(header)" == empty ]] || fail "no-tpm-restore: the header was not invalidated before the jump ($(header))"
counter_goes_on no-tpm-restore counter-no-tpm.txt "$((off_ms / 2))"
say "no TPM: set up with the warning, the image written with its key in the clear and restored - the counter went on"
hibernate no-tpm-again
[[ "$(protection)" == clear ]] || fail "no-tpm-again: the header does not keep the key in the clear ($(protection))"
boot clear-on-tpm "$S_OFFERED"
refused clear-on-tpm "its key is in the clear, and this machine"

# 8. A REFUSAL AFTER EVERY BINDING WAS STOPPED: the machine restarts and boots fresh.
hibernate late-refusal-image
boot late-refusal "$S_OFFERED -fw_cfg name=opt/org.libersystem/absent,string=replacement"
grep -a -q -F "hibernate: the development switch names the replacement absent" "$serial" || fail "late-refusal: the kernel did not refuse the replacement the switch names absent"
grep -a -q -F "HibernationService: the image is refused after every binding was stopped for it, and the machine restarts and boots fresh" "$serial" || fail "late-refusal: the image was not refused as one whose bindings were stopped"
grep -a -q -F "ServiceManager: restore: every binding was stopped for a replacement that did not happen - the machine restarts, and boots fresh" "$serial" || fail "late-refusal: ServiceManager did not restart the machine"
(($(seen "HibernationService: online") >= 2)) || fail "late-refusal: no second boot followed the restart"
[[ "$(header)" == empty ]] || fail "late-refusal: the header was not invalidated before the bindings were stopped ($(header))"
grep -q "the image found at this boot: none" <<<"$(status_line late-refusal)" || fail "late-refusal: the boot after the restart found an image: $(cat "$state/status-late-refusal.log")"
say "late-refusal: refused after every binding was stopped - the machine restarted and booted fresh"

say "PASS - $boots boots: an image written and entered S4 (SUSPEND_DISK), restored with the counter going on; the boot after found none; powered off where S4 is not offered; a modified image, another system image and another machine each refused with the header invalidated; hybrid discarded on the S3 resume and restored after the power was lost; without a TPM set up with the warning, written with its key in the clear and restored, and such an image refused where a TPM seals; refused after its bindings were stopped, the machine restarted and booted fresh"
