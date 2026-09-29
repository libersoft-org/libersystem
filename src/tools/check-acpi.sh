#!/usr/bin/env bash
# THE FIRMWARE'S NAMESPACE, end to end on x86_64 q35: the ACPI service interpreting QEMU's DSDT and the fixture SSDT
# (`harness/acpi-fixture.py`), the kernel publishing what it describes under its policy, DeviceManager handing the
# node channels and the connections, and the fixture driver's probes.
#
#   1. THE FIXTURE BOOT - native PCIe hot-plug, `swtpm` behind the CRB front-end, the vhost-user I2C and GPIO controllers,
#      an `ivshmem-plain` function over a file this script writes, two free CPU slots:
#        - every node accounted for, each with its reason: q35's DRAC a reservation over the MCFG window, the three
#          fixture companions joined, the ivshmem function firmware-held, COM1 one row from the kernel and `PNP0501`,
#          the TPM one row from its table and `MSFT0101`, the PCI interrupt links not devices, the processors the
#          service's own, a range the kernel holds refused;
#        - the virtio-i2c and virtio-gpio bindings - bound before the namespace existed - handed their companions' node
#          channels at the service's "namespace loaded" report, and the service granted the `_AEI` line, the field line
#          and the field address;
#        - `lsdev` showing the reservation, the companions and the firmware-held function;
#        - the `_AEI` line raised: `Notify(LSF1, 0x80)` on the fixture driver's node channel, whose probes read the dword
#          this script wrote, write the device's own region and read it back through the claim, are refused another
#          node's region over the claimed range, read the line field, are refused its write, write and read the bus
#          fixture's register, and get `_DSD`, `_DSM` and a refusal of `_INI`;
#        - THE POWER CLASSES: the fixture's battery (`PNP0C0A`), AC adapter (`ACPI0003`) and thermal zone bound by
#          `acpi_power` and read through PowerService by `acpipower`, a client with the read authority alone - the first
#          values in exact canonical units; the pages changed and the power line (3) raised while it is subscribed, and
#          the battery discharging, the adapter off line and the zone warmer arriving as updates; a storm of that line
#          with the zone's reading changed before each event, the reading written last arriving and the storm
#          coalesced by the zone's driver into fewer updates than events;
#        - two CPUs hot-added through QMP: `\_GPE._E02` run, `Notify(..., 1)` on each new processor, the event enabled
#          again after each;
#        - `_OSC` granting native hot-plug, and the kernel arming what it granted;
#        - THE SERVICE KILLED AND RESTARTED, with `LSFX0002` `_UID` 1's `_STA` byte cleared just before: every row,
#          reservation and companion the first instance published kept - no row added, every index unchanged, DRAC once -
#          except `_UID` 1, withdrawn at the new instance's report; the node channels handed again, the line granted
#          again, and the raised line delivering `Notify` to the fixture again; the power classes' bindings reading
#          the nodes handed again, and the first values - written back before the kill - published again;
#        - SMBIOS read on the UEFI boot.
#   2. THE FIRMWARE'S HOT-PLUG (`PCIE_HOTPLUG=acpi`): `_OSC` refusing native hot-plug, and the kernel leaving the slots
#      unarmed and saying so.
#
# WHAT IT DOES NOT CLAIM: an embedded controller (QEMU emulates none - its transport is host-tested and its acceptance a
# recorded run on hardware), and the device-tree ports - their tree runs are the ports' own.
#
# IT BOOTS ITS OWN INSTANCES in private state, one at a time, and takes the last one down from the EXIT trap.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."

fail() {
	echo "acpi: $*" >&2
	exit 1
}

command -v python3 >/dev/null || fail "python3 is not installed, and the lab is written in it"
command -v swtpm >/dev/null || fail "swtpm is not installed - setup.sh installs it, and this gate fails rather than skips without it"
[[ ! -S .build/boot/lab-ctl.sock ]] || fail "an ad-hoc lab guest is up (.build/boot/lab-ctl.sock) - take it down with ./lab.sh quit first"

state="$(mktemp -d "${TMPDIR:-/tmp}/liber-acpi.XXXXXX")"
fixture="$state/fixture"
mkdir -p "$fixture"
export LIBER_DEV_STATE="$state"
HOSTFWD_PORT="$(python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])')"
export HOSTFWD_PORT
kept="$(pwd)/.build/logs/acpi"
rm -rf "$kept"
mkdir -p "$kept"
label="none"
backend_pid=""
tpm_pid=""

keep_log() {
	cp -f "$state/dev-serial.log" "$kept/serial-$label.log" 2>/dev/null || true
}

stop_helpers() {
	for pid in "$backend_pid" "$tpm_pid"; do
		if [[ -n "$pid" ]]; then
			kill "$pid" 2>/dev/null || true
			wait "$pid" 2>/dev/null || true
		fi
	done
	backend_pid=""
	tpm_pid=""
}

cleanup() {
	local status=$?
	keep_log
	./dev.sh down >"$state/down.log" 2>&1 || echo "acpi: teardown reported a problem (see $kept/down.log)" >&2
	stop_helpers
	cp -f "$state"/*.log "$fixture"/*.log "$kept/" 2>/dev/null || true
	rm -f .build/boot/*"-dev-$(basename "$state")"* 2>/dev/null || true
	rm -rf "$state"
	return "$status"
}
trap cleanup EXIT

serial_log() {
	echo "$state/dev-serial.log"
}

seen() {
	grep -a -c -F -- "$1" "$(serial_log)" 2>/dev/null || true
}

has() {
	grep -a -q -F -- "$1" "$(serial_log)" || fail "$2 (no line containing: $1)"
}

await_line() {
	local needle="$1" what="$2" baseline="$3" limit="${4:-120}"
	for _ in $(seq 1 "$limit"); do
		if (($(seen "$needle") > baseline)); then
			return 0
		fi
		sleep 1
	done
	fail "$what (waited ${limit} s for a new line containing: $needle)"
}

launch() {
	./dev.sh launch --timeout 60 "$@" 2>&1
}

# One command to the vhost-user backend's control socket; its one-line answer.
control() {
	python3 - "$fixture/control.sock" "$1" <<'EOF'
import socket
import sys
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
s.sendall((sys.argv[2] + '\n').encode())
s.settimeout(5)
print(s.recv(4096).decode().strip())
EOF
}

# HOT-ADD ONE CPU into the first free slot, through QMP.
hot_add_cpu() {
	python3 - "$state/qemu-qmp.sock" "$1" <<'EOF'
import json
import socket
import sys
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
f = s.makefile('rw')

def answer():
	while True:
		line = json.loads(f.readline())
		if 'event' not in line:
			return line

def command(execute, arguments=None):
	f.write(json.dumps({'execute': execute, 'arguments': arguments or {}}) + '\n')
	f.flush()
	return answer()

answer()
command('qmp_capabilities')
free = [slot for slot in command('query-hotpluggable-cpus')['return'] if 'qom-path' not in slot]
if not free:
	sys.exit('no free CPU slot')
slot = free[-1]
result = command('device_add', {'driver': slot['type'], 'id': sys.argv[2], **slot['props']})
if 'error' in result:
	sys.exit(f"device_add refused: {result['error']}")
print(f"hot-added {slot['type']} {slot['props']}")
EOF
}

start_helpers() {
	python3 src/harness/acpi-fixture.py --out "$fixture/fixture.aml" --memory "$fixture/ivshmem.bin"
	# THE DWORD `RDVL` READS: 0xC0C0EE01, little-endian at the harness's page 0.
	python3 src/harness/acpi-fixture.py --poke "$fixture/ivshmem.bin" 0 01eec0c0
	python3 src/harness/vhost-i2c-gpio.py --i2c "$fixture/i2c.sock" --gpio "$fixture/gpio.sock" --control "$fixture/control.sock" --ready "$fixture/ready" >"$fixture/backend.log" 2>&1 &
	backend_pid="$!"
	swtpm socket --tpm2 --tpmstate "dir=$fixture" --ctrl "type=unixio,path=$fixture/swtpm.sock" --log "file=$fixture/swtpm.log" &
	tpm_pid="$!"
	for _ in $(seq 1 100); do
		[[ -e "$fixture/ready" && -S "$fixture/swtpm.sock" ]] && break
		sleep 0.05
	done
	[[ -e "$fixture/ready" ]] || fail "the vhost-user backend did not start (see $kept/backend.log)"
	[[ -S "$fixture/swtpm.sock" ]] || fail "swtpm did not open its control socket"
	export ACPI_FIXTURE="$fixture/fixture.aml" ACPI_FIXTURE_MEMORY="$fixture/ivshmem.bin"
	export I2C_FIXTURE=bus I2C_SOCKET="$fixture/i2c.sock" GPIO_SOCKET="$fixture/gpio.sock"
	export TPM_SOCKET="$fixture/swtpm.sock" TPM_FRONTEND=crb
	export SMP="2,maxcpus=4"
}

boot() {
	label="$1"
	echo "acpi: boot '$label' (state $state, host port $HOSTFWD_PORT)"
	if ! ./dev.sh up --timeout 400 >"$state/up-$label.log" 2>&1; then
		tail -20 "$state/up-$label.log" >&2
		fail "the development instance for '$label' did not come up (see $kept/up-$label.log)"
	fi
	await_line "AcpiService: online - instance 1" "the ACPI service never came online" 0
}

take_down() {
	keep_log
	./dev.sh down >"$state/down-$label.log" 2>&1 || fail "the '$label' instance did not come down"
}

# THE ROWS lsdev lists, one line per row: index, presence, state, identity, companion.
rows() {
	launch lsdev | python3 -c '
import re, sys
for line in sys.stdin.read().splitlines():
	if "present=" not in line or "type=" not in line:
		continue
	field = lambda name: (re.search(r"(?:^|[{ ,])" + name + r"=([^,}]*)", line) or [None, ""])[1] or "-"
	print(field("index"), field("present"), field("state"), field("identity"), field("companion"))
'
}

# Whether the first line matching `pattern` names the same value twice - its two groups equal. In Python, since a
# backreference is not something every `grep` on a host reads the same way.
same() {
	python3 - "$(serial_log)" "$1" <<'EOF'
import re
import sys
text = open(sys.argv[1], 'rb').read().decode('utf-8', 'replace')
match = re.search(sys.argv[2], text)
sys.exit(0 if match and match.group(1) == match.group(2) else 1)
EOF
}

# THE FIXTURE DRIVER'S PROBES for round `n`, all of them, as the line says them.
probes() {
	local n="$1"
	has "acpi-fixture: round $n: RDVL answered 0xc0c0ee01" "round $n: RDVL did not answer the dword the harness wrote"
	same "acpi-fixture: round $n: WRVL wrote (0x[0-9a-f]+) and the claim's window reads (0x[0-9a-f]+)" || fail "round $n: the region WRVL wrote is not what the claim's window reads"
	has "acpi-fixture: round $n: another node's region over the claimed range was refused" "round $n: another node's region over the claimed range was not refused"
	has "acpi-fixture: round $n: GPRD read line 5 as 1" "round $n: the line field did not read the level the harness set"
	has "acpi-fixture: round $n: a GeneralPurposeIo write was refused" "round $n: a GeneralPurposeIo write was not refused"
	same "acpi-fixture: round $n: GSBW wrote (0x[0-9a-f]+) and GSBR read (0x[0-9a-f]+)" || fail "round $n: the serial-bus field did not read back what it wrote"
	has "acpi-fixture: round $n: _DSD answered a" "round $n: _DSD did not answer"
	has "acpi-fixture: round $n: _DSM function 1 answered 42" "round $n: _DSM did not answer"
	has "acpi-fixture: round $n: _INI was refused" "round $n: a platform method was not refused"
}

raise_aei() {
	local n="$1" before answer
	before=$(seen "acpi-fixture: round $n: _INI")
	control "lower 2" >/dev/null
	sleep 0.3
	answer="$(control "raise 2")"
	[[ "$answer" == *fired* ]] || fail "raising line 2 fired no event in the GPIO backend ($answer)"
	await_line "acpi-fixture: round $n: _INI" "round $n of the fixture's probes did not run after the line was raised" "$before" 60
	probes "$n"
	echo "acpi: the _AEI line delivered Notify(LSF1, 0x80) and round $n of the probes passed"
}

# THE POWER NODES, as the service names them.
POWER_NODES=('\_SB_.BAT0' '\_SB_.ADP0' '\_TZ_.TZ00')

# THE POWER LINE: line 3, whose `_E03` notifies the battery, the adapter and the zone with 0x80.
raise_power() {
	local answer
	control "lower 3" >/dev/null
	sleep 0.3
	answer="$(control "raise 3")"
	[[ "$answer" == *fired* ]] || fail "raising line 3 fired no event in the GPIO backend ($answer)"
}

# ONE `acpipower` PHASE, launched in the background with its output in a file: `host` answers the probe's cue, and the
# phase must then pass.
power_phase() {
	local phase="$1" cue="$2" host="$3" out="$state/acpipower-$1.log" pid
	./dev.sh launch --timeout 180 acpipower "$phase" >"$out" 2>&1 &
	pid=$!
	for _ in $(seq 1 1200); do
		grep -a -q -F -- "$cue" "$out" && break
		kill -0 "$pid" 2>/dev/null || break
		sleep 0.1
	done
	if ! grep -a -q -F -- "$cue" "$out"; then
		cat "$out" >&2
		fail "acpipower $phase never cued the host"
	fi
	"$host"
	wait "$pid" || true
	cp -f "$out" "$kept/"
	if ! grep -a -q -F -- "acpipower: PASS $phase" "$out"; then
		cat "$out" >&2
		fail "acpipower $phase did not pass"
	fi
}

# THE HOST'S CHANGE WHILE `watch` IS SUBSCRIBED: discharging at 7000 mW with 20000 mWh left, off line, the zone at
# 318.2 K - then the power line.
watch_host() {
	python3 src/harness/acpi-fixture.py --set "$fixture/ivshmem.bin" battery-state=1 battery-rate=7000 battery-remaining=20000 ac-online=0 zone-temperature=3182
	raise_power
}

storm_host() {
	python3 src/harness/acpi-fixture.py --power-storm "$fixture/ivshmem.bin" "$fixture/control.sock" >"$state/storm.log" 2>&1 || {
		cat "$state/storm.log" >&2
		fail "the host could not raise the power line storm"
	}
	cat "$state/storm.log"
}

# ------------------------------------------------------------------ 1. the fixture boot

start_helpers
boot fixture
log="$(serial_log)"

# EVERY NODE ACCOUNTED FOR, with its reason.
has "AcpiService: acpi:\\_SB_.DRAC is a reservation (row" "q35's DRAC is not a reservation"
has "firmware: acpi:\\_SB_.PCI0.SA0_ is the companion of 0000:00:14.0" "the ivshmem function's companion was not joined"
has "firmware: acpi:\\_SB_.PCI0.SA8_ is the companion of 0000:00:15.0" "the virtio-i2c function's companion was not joined"
grep -a -q -F -- "firmware: acpi:\\_SB_.PCI0.SB0_ is the companion of 0000:00:16.0" "$log" || fail "the virtio-gpio function's companion was not joined"
grep -a -q -F -- "_AEI lines [0x1000002,0x1000003,0x1000004], field lines [0x5]" "$log" || fail "the GPIO controller's companion does not list its three _AEI lines and its field line"
has "firmware: 0000:00:14.0 is now FIRMWARE-HELD" "the ivshmem function was not made firmware-held"
has "AcpiService: acpi:\\_SB_.LSF1 is a platform device (row" "the fixture device LSFX0001 was not published"
has "AcpiService: acpi:\\_SB_.LSF4 is a method-only device (row" "LSFX0002 _UID 0 was not published"
has "AcpiService: acpi:\\_SB_.LSF5 is a method-only device (row" "LSFX0002 _UID 1 was not published"
has "device: acpi:\\_SB_.PCI0.SF8_.COM1 is the device row" "COM1 was not one row from the kernel and PNP0501"
has "device: acpi:\\_SB_.TPM_ is the device row" "the TPM was not one row from its table and MSFT0101"
has "is not a device - a PCI interrupt link" "the PCI interrupt links were not accounted for"
has "AcpiService: acpi:\\_SB_.CPUS is a processor - the service's own" "the processor container was not accounted for"
grep -a -q -E -- "firmware: the region acpi:[^ ]+ declares at 0x[0-9a-f]+\.\.0x[0-9a-f]+ is not mapped - the kernel holds it" "$log" || fail "no region over a range the kernel holds was refused"
has "AcpiService: the walk found" "the walk did not report how many nodes it found"
echo "acpi: every node of the DSDT and the fixture SSDT is accounted for"

# THE BINDINGS FROM BEFORE THE NAMESPACE, handed their companions' node channels at the report; the grants.
has "DeviceManager: handed virtio-i2c its node channel (acpi:\\_SB_.PCI0.SA8_) at the ACPI service's namespace-loaded report" "the virtio-i2c binding was not handed its companion's node channel at the report"
grep -a -q -F -- "DeviceManager: handed virtio-gpio its node channel (" "$log" || fail "the virtio-gpio binding was not handed its companion's node channel"
has "DeviceManager: granted the ACPI service 4 connection(s) of acpi:\\_SB_.PCI0.SB0_" "the GPIO controller's three _AEI lines and field line were not granted"
has "DeviceManager: granted the ACPI service 1 connection(s) of acpi:\\_SB_.PCI0.SA8_" "the I2C controller's field address was not granted"
has "acpi-fixture: its node is acpi:\\_SB_.LSF1" "the fixture driver was not handed its node channel with its claim"
# THE POWER CLASSES' BINDINGS, each publishing - settled before lsdev's first snapshot, so every row's state is too.
has "AcpiService: acpi:\\_SB_.BAT0 is a method-only device (row" "the battery was not published"
has "AcpiService: acpi:\\_SB_.ADP0 is a method-only device (row" "the AC adapter was not published"
has "AcpiService: acpi:\\_TZ_.TZ00 is a thermal zone (row" "the thermal zone was not published"
for node in "${POWER_NODES[@]}"; do
	await_line "driver.acpi-power: acpi:$node: publishes its" "acpi_power did not bind and publish $node" 0 90
done

# LSDEV: the reservation, the companions, the firmware-held function.
rows >"$state/rows-before.log"
cp "$state/rows-before.log" "$kept/"
grep -q "reservation acpi:\\\\_SB_.DRAC" "$state/rows-before.log" || fail "lsdev does not list DRAC as a reservation"
grep -q "firmware-held - acpi:\\\\_SB_.PCI0.SA0_" "$state/rows-before.log" || fail "lsdev does not show the ivshmem function firmware-held with its companion"
grep -q "acpi:\\\\_SB_.PCI0.SB0_$" "$state/rows-before.log" || fail "lsdev does not show the virtio-gpio function's companion"
echo "acpi: lsdev lists the reservation, the companions and the firmware-held function"

# THE LINE FIELD'S LEVEL, then the _AEI line: round 1.
control "level 5 1" >/dev/null
raise_aei 1
grep -a -q -F -- "is not mapped - acpi:\\_SB_.LSF1 holds it through a claim" "$log" || fail "the kernel did not refuse LSFX0003's region over LSFX0001's claimed range"

# THE POWER CLASSES THROUGH POWERSERVICE: the first values, a live client's updates, and the storm.
out="$(./dev.sh launch --timeout 150 acpipower list 2>&1)" || true
echo "$out" >"$state/acpipower-list.log"
grep -q -F "acpipower: PASS list" <<<"$out" || fail "acpipower list did not pass: $out"
echo "acpi: the battery, the adapter and the zone read through PowerService in exact canonical units"
power_phase watch "acpipower: watching - change the pages and raise the power line now" watch_host
grep -a -q -F -- "AcpiService: Notify(\_TZ_.TZ00, 0x80)" "$log" || fail "_E03 did not notify the zone"
echo "acpi: a live client saw the battery discharge, the adapter go off line and the zone warm after the power line rose"
dropped_before=$(seen "a Notify for acpi:\_TZ_.TZ00 was not taken by its driver's stream")
power_phase storm "acpipower: storming - raise the power line now" storm_host
fired="$(grep -o -E '[0-9]+ events fired' "$state/storm.log" | grep -o -E '^[0-9]+' || true)"
updates="$(grep -a -o -E 'acpipower: storm: zone updates [0-9]+' "$state/acpipower-storm.log" | grep -o -E '[0-9]+$' || true)"
[[ -n "$fired" && -n "$updates" ]] || fail "the storm's events or the client's updates were not counted"
((updates < fired)) || fail "the storm was not coalesced: $updates zone update(s) for $fired event(s)"
grep -a -q -E -- "driver\.acpi-power: acpi:\\\\_TZ_\.TZ00: [0-9]+ notification\(s\) coalesced into later readings" "$log" || fail "the zone's driver did not coalesce the storm's notifications"
(($(seen "a Notify for acpi:\_TZ_.TZ00 was not taken by its driver's stream") == dropped_before)) || fail "the zone's driver fell behind its notification stream during the storm"
echo "acpi: a storm of $fired events on the power line reached the client as $updates zone update(s), the last reading among them"
# THE FIRST VALUES BACK, unraised: after the restart below, the bindings read them from the nodes handed again.
python3 src/harness/acpi-fixture.py --set "$fixture/ivshmem.bin" battery-state=2 battery-rate=5000 battery-remaining=24000 ac-online=1 zone-temperature=3002

# THE CPU HOT-PLUG GPE, twice.
for n in 1 2; do
	ran=$(seen "AcpiService: general-purpose event 0x02 - running _E02")
	enabled=$(seen "AcpiService: general-purpose event 0x02 is enabled again")
	notified=$(grep -a -c -E -- "AcpiService: Notify\(\\\\_SB_\.CPUS\.C[0-9A-F]{3}, 0x1\)" "$log" || true)
	hot_add_cpu "hotcpu$n" || fail "CPU $n could not be hot-added"
	await_line "AcpiService: general-purpose event 0x02 - running _E02" "the CPU hot-plug GPE did not run _E02 for CPU $n" "$ran" 60
	await_line "AcpiService: general-purpose event 0x02 is enabled again" "the CPU hot-plug GPE was not enabled again after CPU $n" "$enabled" 60
	now=$(grep -a -c -E -- "AcpiService: Notify\(\\\\_SB_\.CPUS\.C[0-9A-F]{3}, 0x1\)" "$log" || true)
	((now > notified)) || fail "no Notify(..., 1) on the new processor for CPU $n"
done
echo "acpi: two hot-added CPUs each latched GPE 0x02, ran _E02, notified the new processor and re-enabled the event"

# _OSC.
has "AcpiService: acpi:\\_SB_.PCI0 is a PCI host bridge (segment 0, buses 0x00..0xff) - its _OSC grants native hot-plug" "q35's host bridge _OSC did not grant native hot-plug"
has "pci: _OSC grants native hot-plug on buses 0x00..0xff" "the kernel did not take the hot-plug grant"
has "carries hot-plug slot" "the kernel armed no hot-plug slot after the grant"

# SMBIOS on the UEFI boot.
has "smbios: " "no SMBIOS line on the UEFI boot"

# THE RESTART: _UID 1's _STA byte cleared, the service killed.
drac="$(grep -a -o -m 1 -E -- "AcpiService: acpi:\\\\_SB_\.DRAC is a reservation \(row [0-9]+\)" "$log")"
arrivals=$(grep -a -c -E -- "firmware: acpi:[^ ]+ is row [0-9]+" "$log" || true)
python3 src/harness/acpi-fixture.py --poke "$fixture/ivshmem.bin" 0x11 00
restarted=$(seen "supervisor: acpi_service restarted")
out="$(launch stop '!crash acpi_service')" || fail "the crash hook was not reached: $out"
grep -q "CRASHED" <<<"$out" || fail "ServiceManager did not kill the ACPI service: $out"
await_line "AcpiService: online - instance 2" "the restarted ACPI service did not come online" 0 120
has "firmware: the ACPI service's instance 1 ended - its general-purpose events are disabled" "the kernel did not disable the dead instance's events"
has "firmware: acpi:\\_SB_.LSF5 (row" "LSFX0002 _UID 1 was not withdrawn"
grep -a -q -E -- "firmware: the ACPI service's instance 2 has loaded its namespace - [0-9]+ reported, 1 withdrawn" "$log" || fail "the new instance's report did not withdraw exactly one row"
now=$(grep -a -c -E -- "firmware: acpi:[^ ]+ is row [0-9]+" "$log" || true)
((now == arrivals)) || fail "the new instance's walk added rows ($now arrivals, $arrivals before)"
[[ "$(grep -a -c -F -- "$drac" "$log")" == "2" ]] || fail "DRAC was not reported again as the same reservation ($drac)"
await_line "DeviceManager: granted the ACPI service 4 connection(s) of acpi:\\_SB_.PCI0.SB0_" "the new instance was not granted the GPIO lines" 1 60
await_line "acpi-fixture: its node is acpi:\\_SB_.LSF1" "the fixture driver was not handed its node again" 1 60
rows >"$state/rows-after.log"
cp "$state/rows-after.log" "$kept/"
python3 - "$state/rows-before.log" "$state/rows-after.log" <<'EOF' || fail "the rows across the restart are not what the first instance published"
import sys
before = [line.split() for line in open(sys.argv[1]) if line.strip()]
after = [line.split() for line in open(sys.argv[2]) if line.strip()]
if len(before) != len(after):
	sys.exit(f"{len(before)} rows before and {len(after)} after")
for old, new in zip(before, after):
	if old[0] != new[0] or old[3] != new[3]:
		sys.exit(f"row {old} became {new}")
	withdrawn = old[3].endswith("LSF5")
	if withdrawn:
		if new[1] != "false":
			sys.exit(f"LSFX0002 _UID 1 is still present: {new}")
	elif old != new:
		sys.exit(f"row {old} changed to {new}")
print(f"acpi: {len(after)} rows kept across the restart, LSFX0002 _UID 1 withdrawn")
EOF
raise_aei 2
for node in "${POWER_NODES[@]}"; do
	await_line "driver.acpi-power: acpi:$node: its node is handed again - reading it" "acpi_power did not take $node's node from the new instance" 0 60
done
out="$(./dev.sh launch --timeout 150 acpipower list 2>&1)" || true
echo "$out" >"$state/acpipower-list-restarted.log"
grep -q -F "acpipower: PASS list" <<<"$out" || fail "the power classes did not publish what the nodes the new instance serves answer: $out"
echo "acpi: the service was killed and restarted - every row kept, _UID 1 withdrawn, the line granted and delivering again, the power classes read again"
take_down
stop_helpers
unset ACPI_FIXTURE ACPI_FIXTURE_MEMORY I2C_FIXTURE I2C_SOCKET GPIO_SOCKET TPM_SOCKET TPM_FRONTEND SMP

# ------------------------------------------------------------------ 2. the firmware's hot-plug

export PCIE_HOTPLUG=acpi
boot firmware-hotplug
log="$(serial_log)"
grep -a -q -E -- "AcpiService: acpi:\\\\_SB_\.PCI0 is a PCI host bridge .* - its _OSC grants native (PME|AER|capability|LTR|nothing)" "$log" || fail "the host bridge's _OSC answer is not reported"
grep -a -q -F -- "its _OSC grants native hot-plug" "$log" && fail "_OSC granted native hot-plug with the firmware's ACPI hot-plug on"
has "pci: the firmware keeps hot-plug control of buses 0x00..0xff (_OSC) - left unarmed" "the kernel did not say it leaves the firmware's slots unarmed"
grep -a -q -F -- "carries hot-plug slot" "$log" && fail "the kernel armed a hot-plug slot the firmware keeps"
echo "acpi: with the firmware's hot-plug on, _OSC refused native hot-plug and the kernel left the slots unarmed"
take_down
echo "acpi: PASS"
