#!/usr/bin/env bash
# IPMI, END TO END ON x86_64 q35: the system's interfaces to its baseboard management controllers, the BMC service and
# the `bmc` tool, against QEMU's simulated BMCs (`ipmi-bmc-sim`, given the SDR and FRU files `ipmi-harness-bmc.py`
# writes) and the harness's own BMC behind `ipmi-bmc-extern`.
#
# WHAT QEMU 10.0 ALLOWS IN ONE BOOT, and why the interfaces are spread over boots: every `ipmi-bmc-sim` and every
# `ipmi-bmc-extern` registers under one migration instance, so a boot holds one simulated BMC and one harness BMC at
# most - the harness BMC stands in for a second simulated one; the simulator's realize overwrites its `device_id`
# property with 0x20, so each BMC here is told apart by its PRODUCT ID; and QEMU gives every SMBIOS type 38 record the
# one handle 0x3000, of which OVMF installs one - so an ISA or SSIF interface's SMBIOS cross-check is made in a boot
# where it is the only IPMI interface.
#
#   1. THE FIVE INTERFACES, each with a BMC of its own, found through `IPI0001` (ISA KCS and BT with `irq=0` - every
#      form polls - and SSIF, `smbus-ipmi` at 0x10) or through their class (PCI KCS and BT), each answering its own
#      product ID: ISA KCS alone and SSIF alone, both simulated - the SMBIOS type 38 record's match id on the node, the
#      sensors and the FRU as the files hold them, the boot's event once, and the BMC service killed and restarted
#      writing no second one; ISA BT alone, the harness's, with its match id; and PCI KCS (the harness's) beside PCI BT
#      (simulated, given no GUID, so named by its device and its binding, and never a pair).
#   2. A PAIR: a simulated BMC on ISA KCS and the harness's on ISA BT given ONE GUID, reported as one BMC reached
#      twice, its boot event written through exactly one of the two.
#   3. THE HARNESS BMC: LAN configuration and users as scripted, identify on and off, a scripted temperature reaching a
#      PowerService subscriber within two polling periods, an event it adds arriving through the BMC service's follow,
#      an answer past the bound refused, and silence and a closed socket making the BMC unavailable - its zones
#      unknown - and recovery when it answers again.
#   4. MALFORMED RECORDS: an SDR record whose length lies, a FRU area whose checksum is wrong and a SEL record of an
#      undefined type, each refused while the rest is still read.
#   5. AN ORDERLY REBOOT: the graceful-shutdown record in the log the next boot reads - the simulator's log survives the
#      reset - ahead of that boot's own boot record.
#   6. A SLEEP WITH BOTH DRIVERS BOUND: the harness BMC on ISA KCS and a simulated one on SSIF, S3 offered, the watchdog
#      policy naming the BMC at 30 s - not the bridge bound. A suspend to idle and an S3 cycle, each answered by both
#      `ipmi` bindings and `smbus_ich9`; SSIF identifying its BMC again after the S3; and in the harness BMC's record of
#      what it received, for each sleep and in this order: the announcement's longest timeout, the driver's disarm ("don't
#      stop" clear), after the resume the driver's re-arm with the bridge bound, and only then the service's restore of
#      its configured timeout.
#   7. THE OUT-OF-BAND CROSS-CHECK: OpenIPMI's `ipmi_sim` behind `ipmi-bmc-extern` on ISA KCS, its LAN side on a
#      loopback port. The LAN configuration `ipmitool` writes over the LAN is what `bmc lan` reads through KCS, the users
#      `ipmi_sim` is configured with are what `bmc users` reads and `ipmitool` lists, and the boot's event the BMC
#      service writes through KCS is the record `ipmitool` reads over the LAN - its id, type, generator, sensor type and
#      number, direction, data and timestamp.
#
# WHAT IT DOES NOT CLAIM HERE: the protected screen's cases (the log's erasure approved, declined and cancelled by an
# event, and the chassis stops) are `check-ipmi-admin.sh`'s; the BMC's watchdog outside a sleep is `check-watchdog.sh`'s.
# An event sent from outside (`ipmitool event`) is not one: `ipmi_sim` does not log a platform event its LAN side takes.
#
# IT BOOTS ITS OWN DEVELOPMENT INSTANCES in private state, one at a time, drives the shell over the serial console
# (`lab.sh sh`), and takes the last one down from the EXIT trap whatever happened.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."

fail() {
	echo "ipmi: $*" >&2
	exit 1
}

case "${1:-}" in
"" | --arch)
	[[ "${2:-x86_64}" == x86_64 ]] || fail "the IPMI interfaces' guests are x86_64's: QEMU puts ISA and SMBus IPMI on q35"
	;;
*) fail "unexpected argument '$1'" ;;
esac
command -v python3 >/dev/null || fail "python3 is not installed, and the lab is written in it"
command -v ipmi_sim >/dev/null || fail "ipmi_sim is not installed - setup.sh installs openipmi, and this gate fails rather than skips without it"
command -v ipmitool >/dev/null || fail "ipmitool is not installed - setup.sh installs it, and this gate fails rather than skips without it"
[[ ! -S .build/boot/lab-ctl.sock ]] || fail "an ad-hoc lab guest is up (.build/boot/lab-ctl.sock) - take it down with ./lab.sh quit first"

state="$(mktemp -d "${TMPDIR:-/tmp}/liber-ipmi.XXXXXX")"
export LIBER_DEV_STATE="$state"
HOSTFWD_PORT="$(python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])')"
export HOSTFWD_PORT
kept="$(pwd)/.build/logs/ipmi"
rm -rf "$kept"
mkdir -p "$kept"
label="none"
harness=""
helper=""

keep_log() {
	cp -f "$state/dev-serial.log" "$kept/serial-$label.log" 2>/dev/null || true
}

cleanup() {
	local status=$?
	for pid in "$helper" "$harness"; do
		if [[ -n "$pid" ]]; then
			kill "$pid" 2>/dev/null || true
			wait "$pid" 2>/dev/null || true
		fi
	done
	keep_log
	./dev.sh down >"$state/down.log" 2>&1 || echo "ipmi: teardown reported a problem (see $kept/down.log)" >&2
	cp -f "$state"/*.log "$kept/" 2>/dev/null || true
	rm -f .build/boot/*"-dev-$(basename "$state")"* 2>/dev/null || true
	rm -rf "$state"
	return "$status"
}
trap cleanup EXIT

python3 src/harness/ipmi-harness-bmc.py --sdr-out "$state/sdr.bin" --fru-out "$state/fru.bin"

serial_log() {
	echo "$state/dev-serial.log"
}

# How many lines of this boot's serial log hold `text`, right now.
seen() {
	grep -a -c -F -- "$1" "$(serial_log)" 2>/dev/null || true
}

# Wait for a line holding `text` to ARRIVE past `baseline`, or fail saying which did not.
await_line() {
	local needle="$1" what="$2" baseline="$3" limit="${4:-180}"
	for _ in $(seq 1 "$limit"); do
		if (($(seen "$needle") > baseline)); then
			return 0
		fi
		sleep 1
	done
	fail "$what (waited ${limit} s for a new line holding: $needle)"
}

# ONE SHELL LINE over the serial console, its output kept with this boot's; a line that ends in no prompt fails.
sh_line() {
	local line="$1" limit="${2:-120}"
	echo "\$ $line" >>"$state/out-$label.log"
	./lab.sh sh --timeout "$limit" "$line" >>"$state/out-$label.log" 2>&1 || fail "'$line' did not come back to the prompt (see $kept/out-$label.log)"
}

# ONE SHELL LINE WHOSE EFFECT PRINTS AFTER ITS PROMPT - a service killed and restarted says so on its own clock - so
# the console's last word is not a prompt, and the line is judged by what it printed instead.
sh_async() {
	local line="$1" wanted="$2" limit="${3:-30}"
	echo "\$ $line" >>"$state/out-$label.log"
	./lab.sh sh --timeout "$limit" "$line" >>"$state/out-$label.log" 2>&1 || true
	grep -aqF -- "$wanted" "$state/out-$label.log" || fail "'$line' did not print '$wanted' (see $kept/out-$label.log)"
}

# WHAT THIS BOOT SAID - the serial log and the shell's outputs, the latter clean of the terminal's escapes - gathered
# into one file, since a pipeline into a reader that stops at its first match fails under pipefail.
said() {
	cat "$(serial_log)" "$state/out-$label.log" >"$state/said" 2>/dev/null || true
	echo "$state/said"
}

expect() {
	local line="$1" why="$2"
	grep -aqE -- "$line" "$(said)" || {
		echo "ipmi: expected /$line/ - $why" >&2
		echo "--- $label ---" >&2
		grep -aE 'ipmi|bmc|Bmc|smbus|firmware: ' "$(said)" | tail -80 >&2 || true
		exit 1
	}
	echo "ipmi: $line"
}

refuse() {
	if grep -aqE -- "$1" "$(said)"; then
		echo "ipmi: found /$1/ - $2" >&2
		exit 1
	fi
}

count() {
	local found
	found="$(grep -a -c -F -- "$1" "$(said)" || true)"
	echo "${found:-0}"
}

boot() {
	label="$1"
	echo "ipmi: boot '$label' (state $state, host port $HOSTFWD_PORT)"
	: >"$state/out-$label.log"
	if ! ./dev.sh up --timeout 400 >"$state/up-$label.log" 2>&1; then
		tail -20 "$state/up-$label.log" >&2
		fail "the development instance for '$label' did not come up (see $kept/up-$label.log)"
	fi
	await_line "BmcService: online" "the BMC service never came online" 0
	# THE STANDING LOOP'S FIRST ANSWER, which the watchdog service reports on its own clock: a line printed after a
	# prompt leaves the console's last word not a prompt, so the shell is driven only once it has been said.
	await_line "WatchdogService: ServiceManager answered" "ServiceManager's standing loop never answered the watchdog service" 0
}

take_down() {
	keep_log
	cp -f "$state/out-$label.log" "$kept/" 2>/dev/null || true
	./dev.sh down >"$state/down-$label.log" 2>&1 || fail "the '$label' instance did not come down"
}

# THE HARNESS BMC, listening before QEMU starts, since QEMU's chardev connects to it.
start_harness() {
	local name="$1"
	shift
	rm -f "$state/ready"
	python3 src/harness/ipmi-harness-bmc.py --serve "$state/bmc.sock" --control "$state/control.sock" --record "$state/record-$name.log" --ready "$state/ready" "$@" >"$state/harness-$name.log" 2>&1 &
	harness=$!
	for _ in $(seq 1 100); do
		[[ -e "$state/ready" ]] && break
		sleep 0.05
	done
	[[ -e "$state/ready" ]] || fail "the harness BMC did not start"
}

stop_harness() {
	kill "$harness" 2>/dev/null || true
	wait "$harness" 2>/dev/null || true
	harness=""
}

control() {
	python3 - "$state/control.sock" "$1" <<'EOF'
import socket
import sys
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
s.sendall((sys.argv[2] + '\n').encode())
s.settimeout(5)
print(s.recv(4096).decode().strip())
EOF
}

guid() { printf '11111111-2222-3333-4444-%012x' "$1"; }
harness_guid() { printf '111111112222333344440000%08x' "$1"; }
name_of() { printf 'bmc:111111112222333344440000%08x' "$1"; }
sim() {
	local id="$1" product="$2" guid_arg=""
	[[ -n "${3:-}" ]] && guid_arg=",guid=$3"
	printf -- '-device ipmi-bmc-sim,id=%s,product_id=%s%s,sdrfile=%s,frudatafile=%s ' "$id" "$product" "$guid_arg" "$state/sdr.bin" "$state/fru.bin"
}
extern_bmc() {
	printf -- '-chardev socket,id=extbmc,path=%s,reconnect-ms=1000 -device ipmi-bmc-extern,id=bmcx,chardev=extbmc ' "$state/bmc.sock"
}

# THE SENSORS AND THE FRU of a simulated BMC, as the SDR and FRU files hold them.
repository_read() {
	local line number sensor_name value
	while IFS= read -r line; do
		case "$line" in
		sensor*)
			read -r _ number _ <<<"$line"
			sensor_name="${line#sensor "$number" }"
			sensor_name="${sensor_name%% type *}"
			expect "^$number $sensor_name" "the sensor $sensor_name must be listed as the SDR file declares it"
			;;
		fru*)
			value="${line##* }"
			expect "  [a-z ]+ +.*$value" "the FRU field $value must be read as the file holds it"
			;;
		esac
	done < <(python3 src/harness/ipmi-harness-bmc.py --describe)
	expect "0x30 CPU Temp +type 0x01 unknown unc 70\.000 uc 85\.000 unr 95\.000" "a full record's thresholds must be converted, its reading unknown on a simulator that reads only compact records"
}

# THE BOOT'S EVENT, once to each of `bmcs` BMCs, waited for.
boot_events() {
	local bmcs="$1"
	await_line "BmcService: the boot completed - its event is in the log of" "the boot's event was not written" $((bmcs - 1))
	[[ "$(seen 'BmcService: the boot completed - its event is in the log of')" == "$bmcs" ]] || fail "the boot's event must be written once to each of the $bmcs BMC(s)"
	[[ "$(seen "the system's event 'boot completed' was sent to the BMC's log - Answered")" == "$bmcs" ]] || fail "each BMC's driver must send the boot's event once"
}

# THE BMC SERVICE KILLED AND RESTARTED: the replacement writes no second boot event.
restart_writes_none() {
	local name="$1"
	# The shell hands a tool the rest of its line as it was typed, quotes included - so the verb goes bare.
	sh_async "stop !crash bmc_service" "CRASHED"
	await_line "BmcService: online" "the BMC service was not restarted" 1
	await_line "BmcService: this instance is a restarted one - the boot's event was written by the first, and is not written again" "the restarted service did not see that it was restarted" 0 60
	sh_line "bmc"
	sh_line "bmc sel $name"
	[[ "$(seen 'BmcService: the boot completed - its event is in the log of')" == 1 ]] || fail "the restarted service wrote the boot's event again"
	expect "this system's boot completed" "the boot's event must be read back from the log"
}

# ------------------------------------------------------------------ 1. the five interfaces

kcs="$(name_of 1)"
bt="$(name_of 2)"
ssif="$(name_of 3)"
pcikcs="$(name_of 4)"

# 1a. ISA KCS, simulated, alone.
export QEMU_EXTRA="$(sim bmc0 0x21 "$(guid 1)")-device isa-ipmi-kcs,bmc=bmc0,irq=0"
boot kcs
boot_events 1
sh_line "bmc"
sh_line "bmc sensors $kcs"
sh_line "bmc fru $kcs"
sh_line "bmc sel $kcs"
expect "driver\.ipmi: acpi:[^ ]+: _IFT 1," "the ISA KCS interface must be found through IPI0001"
expect "driver\.ipmi: acpi:[^ ]+: Kcs interface: idle" "the KCS interface must be idle at bind"
expect "^$kcs" "the BMC must be named by its GUID"
expect "product 0x0021" "the BMC must answer its own product ID"
expect "firmware: acpi:[^ ]+ agrees with smbios:38#" "the ISA KCS node must carry its SMBIOS type 38 record's match id"
refuse "which no SMBIOS record describes" "the only IPMI node must agree with the SMBIOS record"
repository_read
restart_writes_none "$kcs"
take_down
echo "ipmi: ISA KCS through IPI0001 agreeing with SMBIOS; sensors, FRU and one boot event; a restart wrote none again"

# 1b. SSIF, simulated, alone.
export QEMU_EXTRA="$(sim bmc2 0x23 "$(guid 3)")-device smbus-ipmi,bmc=bmc2,address=0x10"
boot ssif
boot_events 1
sh_line "bmc"
sh_line "bmc sensors $ssif"
sh_line "bmc fru $ssif"
sh_line "bmc sel $ssif"
expect "driver\.ipmi: acpi:[^ ]+: _IFT 4," "the SSIF interface must be found through IPI0001"
expect "driver\.smbus-ich9: online" "the ICH9 SMBus controller's driver must bind"
expect "driver\.ipmi: acpi:[^ ]+: Ssif interface: " "the SSIF interface must read its capabilities over the ICH9 SMBus"
expect "^$ssif" "the BMC must be named by its GUID"
expect "product 0x0023" "the BMC must answer its own product ID"
expect "firmware: acpi:[^ ]+ agrees with smbios:38#" "the SSIF node must carry its SMBIOS type 38 record's match id"
refuse "which no SMBIOS record describes" "the only IPMI node must agree with the SMBIOS record"
repository_read
restart_writes_none "$ssif"
take_down
echo "ipmi: SSIF through IPI0001 over the ICH9 SMBus agreeing with SMBIOS; sensors, FRU and one boot event; a restart wrote none again"

# 1c. ISA BT, the harness's, alone.
start_harness bt --guid "$(harness_guid 2)" --product-id 0x22
export QEMU_EXTRA="$(extern_bmc)-device isa-ipmi-bt,bmc=bmcx,irq=0"
boot bt
boot_events 1
sh_line "bmc"
stop_harness
expect "driver\.ipmi: acpi:[^ ]+: _IFT 3," "the ISA BT interface must be found through IPI0001"
expect "driver\.ipmi: acpi:[^ ]+: Bt interface: 1 outstanding" "the BT interface must read its capabilities"
expect "^$bt" "the BMC must be named by its GUID"
expect "product 0x0022" "the BMC must answer its own product ID"
expect "firmware: acpi:[^ ]+ agrees with smbios:38#" "the ISA BT node must carry its SMBIOS type 38 record's match id"
refuse "which no SMBIOS record describes" "the only IPMI node must agree with the SMBIOS record"
take_down
echo "ipmi: ISA BT through IPI0001 agreeing with SMBIOS"

# 1d. PCI KCS, the harness's, and PCI BT, simulated with no GUID.
start_harness pci --guid "$(harness_guid 4)" --product-id 0x24
export QEMU_EXTRA="$(extern_bmc)-device pci-ipmi-kcs,bmc=bmcx $(sim bmc4 0x25)-device pci-ipmi-bt,bmc=bmc4"
boot pci
boot_events 2
sh_line "bmc"
stop_harness
expect "driver\.ipmi: pci:0000:[0-9a-f:.]+: Kcs interface" "the PCI KCS interface must bind by its class"
expect "driver\.ipmi: pci:0000:[0-9a-f:.]+: Bt interface" "the PCI BT interface must bind by its class"
expect "^$pcikcs" "the PCI KCS BMC must be named by its GUID"
expect "product 0x0024" "the PCI KCS BMC must answer its own product ID"
expect "^bmc:00000-0025-20@pci:0000:" "the BMC without a GUID must be named by its manufacturer, product and device ID with its binding"
refuse "REACHED TWICE" "a BMC without a GUID is never a pair"
take_down
echo "ipmi: PCI KCS and BT by their class; a BMC without a GUID named by its device and its binding"

# ------------------------------------------------------------------ 2. a pair

pair="$(name_of 9)"
start_harness pair --guid "$(harness_guid 9)" --product-id 0x32
export QEMU_EXTRA="$(sim bmc0 0x31 "$(guid 9)")-device isa-ipmi-kcs,bmc=bmc0,irq=0 $(extern_bmc)-device isa-ipmi-bt,bmc=bmcx,irq=0"
boot pair
await_line "is reached through two interfaces" "one GUID through two interfaces must be reported as a pair" 0
boot_events 1
sh_line "bmc"
stop_harness
expect "BmcService: $pair is reached through two interfaces" "one GUID through two interfaces must be reported as a pair"
expect "REACHED TWICE" "the tool must say the BMC is reached twice"
[[ "$(seen "the system's event 'boot completed' was sent to the BMC's log - Answered")" == 1 ]] || fail "a pair's boot event must be written through exactly one of its interfaces"
harness_events="$(grep -a -c '^0x04 0x02 ' "$state/record-pair.log" || true)"
[[ "${harness_events:-0}" -le 1 ]] || fail "the harness side of the pair received the boot's event more than once"
take_down
echo "ipmi: one GUID through two interfaces is one BMC reached twice, its boot event written once (the harness side received ${harness_events:-0})"

# ------------------------------------------------------------------ 3. the harness BMC

# A cue past `baseline`, waited for in this boot's serial log.
wait_for() {
	local text="$1" baseline="${2:-0}" polls=0
	until (($(seen "$text") > baseline)); do
		sleep 0.2
		polls=$((polls + 1))
		((polls < 3000)) || return 1
	done
}

extern="bmc:0102030405060708090a0b0c0d0e0f10"
host_side() {
	wait_for "bmccheck: watching - change the reading now" || return 0
	control "reading 0x30 55" >>"$state/host.log"
	wait_for "bmccheck: following - add an event now" || return 0
	control "sel-add" >>"$state/host.log"
	wait_for "bmccheck: PASS follow" || return 0
	control "mode oversize" >>"$state/host.log"
	wait_for "bmccheck: watching - take the BMC away now" || return 0
	control "mode silent" >>"$state/host.log"
	wait_for "bmccheck: every zone is unknown - bring the BMC back now" || return 0
	control "mode normal" >>"$state/host.log"
	wait_for "bmccheck: watching - take the BMC away now" 1 || return 0
	control "mode close" >>"$state/host.log"
	wait_for "bmccheck: every zone is unknown - bring the BMC back now" 1 || return 0
	control "mode normal" >>"$state/host.log"
}

start_harness harness --guid 0102030405060708090a0b0c0d0e0f10
export QEMU_EXTRA="$(extern_bmc)-device isa-ipmi-kcs,bmc=bmcx,irq=0"
boot harness
boot_events 1
host_side &
helper=$!
sh_line "bmc lan"
sh_line "bmc users"
sh_line "bmc chassis identify 5"
sh_line "bmc chassis"
sh_line "bmc chassis identify off"
sh_line "bmccheck zones 55000" 60
sh_line "bmccheck follow $extern" 60
sh_line "bmc sensors"
sh_line "bmccheck unknown" 300
sh_line "bmccheck unknown" 300
wait "$helper" 2>/dev/null || true
helper=""
stop_harness
if grep -aq 'bmccheck: FAIL' "$(said)"; then
	grep -a 'bmccheck: FAIL' "$(said)" >&2
	fail "the probe reported a failure"
fi
expect "channel 1: static address 10\.0\.2\.15 mask 255\.255\.255\.0 gateway 10\.0\.2\.2 mac 52:54:00:12:34:56 vlan 42" "the LAN configuration must be read as scripted"
expect "channel 1 user 1 admin enabled privilege 4" "the users must be read as scripted"
expect "channel 1 user 2 operator enabled privilege 3" "each user as scripted"
expect "bmc: $extern: identify on for 5 s" "identify on must reach the BMC"
expect "identify on, timed" "and the chassis status must report it"
expect "bmc: $extern: identify off" "identify off must reach the BMC"
expect "bmccheck: PASS zones" "a scripted temperature must reach a PowerService subscriber within two polling periods"
expect "bmccheck: PASS follow" "an added event must arrive through the BMC service's follow"
expect "was refused - TooLong" "an answer past the bound must be refused"
[[ "$(count 'bmccheck: PASS unknown')" -ge 2 ]] || fail "silence and a closed socket must each make the BMC unavailable and its zones unknown, and it must come back"
[[ "$(seen 'the BMC is unavailable - three transactions in a row went unanswered')" -ge 2 ]] || fail "the driver must say the BMC went away, each time"
[[ "$(seen 'the BMC answers again')" -ge 2 ]] || fail "and that it came back"
grep -aq '^0x00 0x04 05 ' "$state/record-harness.log" || fail "the harness BMC did not receive identify on for 5 s"
take_down
echo "ipmi: the harness BMC's LAN, users, identify, a live temperature, the follow, the bound and two absences all hold"

# ------------------------------------------------------------------ 4. malformed records

start_harness malformed --guid 0102030405060708090a0b0c0d0e0f10 --malformed
export QEMU_EXTRA="$(extern_bmc)-device isa-ipmi-kcs,bmc=bmcx,irq=0"
boot malformed
boot_events 1
sh_line "bmc sensors"
sh_line "bmc fru"
sh_line "bmc sel"
stop_harness
expect "bmc: 1 record\(s\) refused as malformed" "an SDR record whose length lies must be refused"
expect "0x30 CPU Temp" "and the rest of the repository still read"
expect "refused: Area\(\"an area's checksum\"\)" "a FRU area with a wrong checksum must be refused"
expect "product name +Gate Node" "and the other areas still read"
expect "refused - record type 0x10, which the specification does not define" "a SEL record of an undefined type must be refused"
expect "this system's boot completed" "and the log's other records still read"
take_down
echo "ipmi: malformed SDR, FRU and SEL records refused, the rest still read"

# ------------------------------------------------------------------ 5. an orderly reboot

rebooted="$(name_of 7)"
export QEMU_EXTRA="$(sim bmc0 0x41 "$(guid 7)")-device isa-ipmi-kcs,bmc=bmc0,irq=0"
boot reboot
boot_events 1
./lab.sh sh --timeout 20 reboot >/dev/null 2>&1 || true
await_line "BmcService: the boot completed - its event is in the log of" "the next boot's event was not written" 1 300
expect "BmcService: an orderly reboot - its event is in the log of $rebooted" "the BMC service must have written the orderly shutdown's event"
sh_line "bmc sel $rebooted"
order="$(grep -a -o -E "this system's (boot completed|orderly shutdown)" "$state/out-$label.log" | tr '\n' '|' || true)"
[[ "$order" == "this system's boot completed|this system's orderly shutdown|this system's boot completed|" ]] || fail "the log must hold the first boot, its orderly shutdown and the second boot, in that order (read: $order)"
take_down
echo "ipmi: an orderly reboot's shutdown record is in the log ahead of the next boot's"

# ------------------------------------------------------------------ 6. a sleep with both drivers bound

qmp() {
	python3 - "$state/qemu-qmp.sock" "$1" <<'EOF'
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
def command(execute):
	stream.write(json.dumps({'execute': execute}) + '\n')
	stream.flush()
	return answer()
answer()
command('qmp_capabilities')
result = command(sys.argv[2])
if sys.argv[2] == 'query-status':
	print(result['return']['status'])
EOF
}

await_state() {
	for _ in $(seq 1 "$(($2 * 2))"); do
		[[ "$(qmp query-status 2>/dev/null || true)" == "$1" ]] && return 0
		sleep 0.5
	done
	fail "sleep: QEMU was not $1 after $2 s"
}

# THE HARNESS BMC'S SET WATCHDOG TIMER REQUESTS since line `from` of its record, in the order the case requires.
watchdog_order() {
	local from="$1" what="$2"
	python3 - "$state/record-sleep.log" "$from" "$what" <<'EOF' || fail "sleep: $what - the BMC did not receive the watchdog's step in order (see $kept/record-sleep.log)"
import sys
path, start, what = sys.argv[1], int(sys.argv[2]), sys.argv[3]
seen = []
for line in open(path).read().splitlines()[start:]:
	words = line.split()
	if len(words) >= 3 and words[0] == '0x06' and words[1] == '0x24':
		data = bytes.fromhex(words[2])
		seen.append((data[0], int.from_bytes(data[4:6], 'little')))
want = [('the announcement\'s longest timeout', lambda use, count: use & 0x40 and count == 65535),
	('the driver\'s disarm', lambda use, count: not use & 0x40),
	('the driver\'s re-arm with the bridge bound', lambda use, count: use & 0x40 and count == 1200),
	('the service\'s restore of its configured timeout', lambda use, count: use & 0x40 and count == 300)]
at = 0
for use, count in seen:
	if at < len(want) and want[at][1](use, count):
		at += 1
if at < len(want):
	sys.exit(f'{what}: {want[at][0]} did not follow in order; the requests were {seen}')
print(f'ipmi: sleep: {what} - the announcement\'s longest timeout, the disarm, the re-arm with the bridge bound and the restore, in order')
EOF
}

start_harness sleep --guid 0102030405060708090a0b0c0d0e0f11
export QEMU_EXTRA="$(extern_bmc)-device isa-ipmi-kcs,bmc=bmcx,irq=0 $(sim bmc1 0x43 "$(guid 9)")-device smbus-ipmi,bmc=bmc1,address=0x10 -global ICH9-LPC.disable_s3=0"
boot sleep
for pair in "watchdog.enabled on" "watchdog.device bmc" "watchdog.timeout-ms 30000" "watchdog.period-ms 4000" "watchdog.deadline-ms 2000"; do
	# shellcheck disable=SC2086 # the key and its value are two words, as `set` takes them
	out="$(./dev.sh launch --timeout 60 set $pair 2>&1)" || fail "sleep: set $pair was not run: $out"
	grep -q "ok" <<<"$out" || fail "sleep: set $pair was refused: $out"
done
armed=$(seen "WatchdogService: armed bmc at")
./dev.sh launch --timeout 60 stop watchdog_service >/dev/null 2>&1 || true
./dev.sh launch --timeout 60 start watchdog_service >/dev/null 2>&1 || true
await_line "WatchdogService: armed bmc at" "sleep: the watchdog service did not arm the BMC's watchdog" "$armed" 60
for kind in idle ram; do
	from=$(wc -l <"$state/record-sleep.log")
	ended=$(seen "ServiceManager: sleep: the transaction ended")
	restored=$(seen "WatchdogService: restored bmc to")
	answered=$(seen "the BMC answers again")
	if [[ "$kind" == idle ]]; then
		./dev.sh launch --timeout 120 sleepctl suspend idle 5 >"$state/sleepctl-$kind.log" 2>&1 || true
	else
		./dev.sh launch --timeout 200 sleepctl suspend ram >"$state/sleepctl-$kind.log" 2>&1 &
		await_state suspended 60
		sleep 2
		qmp system_wakeup >/dev/null
		await_state running 30
	fi
	await_line "ServiceManager: sleep: the transaction ended" "sleep: the $kind sleep never ended" "$ended" 180
	ended="$(grep -a "ServiceManager: sleep: the transaction ended" "$(serial_log)" | tail -1 || true)"
	grep -q "slept and woke" <<<"$ended" || fail "sleep: the $kind sleep did not sleep and wake"
	await_line "WatchdogService: restored bmc to" "sleep: the watchdog service did not restore its timeout after the $kind sleep" "$restored" 30
	for binding in "driver.ipmi: " "driver.smbus-ich9: "; do
		tail -n 400 "$(serial_log)" | grep -a -q "${binding}.*resumed" || fail "sleep: $binding did not answer the $kind sleep's resume"
	done
	# BOTH BMCS IDENTIFIED AGAIN at the resume - SSIF's included - each binding saying its BMC answers.
	await_line "the BMC answers again" "sleep: the BMCs were not identified again after the $kind sleep" "$((answered + 1))" 60
	watchdog_order "$from" "the $kind sleep"
done
take_down
stop_harness
cp -f "$state/record-sleep.log" "$kept/" 2>/dev/null || true
echo "ipmi: a sleep with KCS and SSIF bound: both answered to idle and S3, SSIF identified again, and the BMC received the watchdog's step in order"

# ------------------------------------------------------------------ 7. the out-of-band cross-check

# OPENIPMI'S `ipmi_sim` BEHIND `ipmi-bmc-extern`, on ISA KCS, with its LAN side on a loopback port the host reads with
# `ipmitool`: one BMC, reached from inside through the system's interface and from outside over its network side.
oob_free_port() {
	python3 -c 'import socket, sys
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM if sys.argv[1] == "udp" else socket.SOCK_STREAM)
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])' "$1"
}
oob_lan="$(oob_free_port udp)"
oob_vm="$(oob_free_port tcp)"
mkdir -p "$state/sim/state"
cat >"$state/sim/lan.conf" <<EOF
name "liberbmc"
set_working_mc 0x20
  startlan 1
    addr 127.0.0.1 $oob_lan
    priv_limit admin
    allowed_auths_callback none md2 md5 straight
    allowed_auths_user none md2 md5 straight
    allowed_auths_operator none md2 md5 straight
    allowed_auths_admin none md2 md5 straight
    guid a123456789abcdefa123456789abcdef
  endlan
  serial 15 127.0.0.1 $oob_vm codec VM
  startnow false
  user 1 true  ""        "test" user     10       none md2 md5 straight
  user 2 true  "ipmiusr" "test" admin    10       none md2 md5 straight
EOF
cat >"$state/sim/bmc.emu" <<'EOF'
mc_setbmc 0x20
mc_add 0x20 0 no-device-sdrs 0x23 9 8 0x9f 0x1291 0xf02 persist_sdr
sel_enable 0x20 1000 0x0a
mc_enable 0x20
EOF
(cd "$state/sim" && exec ipmi_sim -c lan.conf -f bmc.emu -s "$state/sim/state" -n >"$state/ipmi-sim.log" 2>&1) &
helper=$!
outside() {
	ipmitool -I lanplus -H 127.0.0.1 -p "$oob_lan" -U ipmiusr -P test "$@" 2>/dev/null
}
for _ in $(seq 1 50); do
	grep -q "Product ID" <<<"$(outside mc info || true)" && break
	sleep 0.2
done
grep -q "Product ID" <<<"$(outside mc info || true)" || fail "oob: ipmi_sim did not answer ipmitool over its LAN side (see $kept/ipmi-sim.log)"
# THE LAN CONFIGURATION, WRITTEN FROM OUTSIDE, before the guest boots.
for setting in "ipsrc static" "ipaddr 192.168.77.10" "netmask 255.255.255.0" "defgw ipaddr 192.168.77.1" "macaddr 52:54:00:aa:bb:cc" "vlan id 7"; do
	# shellcheck disable=SC2086 # the parameter and its value are words of their own, as ipmitool takes them
	outside lan set 1 $setting >/dev/null || fail "oob: ipmitool could not set the LAN's $setting"
done
grep -q "IP Address  *: 192.168.77.10" <<<"$(outside lan print 1 || true)" || fail "oob: the LAN configuration written from outside did not read back from outside"
export QEMU_EXTRA="-chardev socket,id=simbmc,host=127.0.0.1,port=$oob_vm,reconnect-ms=1000 -device ipmi-bmc-extern,id=bmcs,chardev=simbmc -device isa-ipmi-kcs,bmc=bmcs,irq=0"
boot oob
boot_events 1
sh_line "bmc lan"
sh_line "bmc users"
sh_line "bmc sel"
# OUTSIDE IN: what ipmitool wrote over the LAN is what the BMC service reads through KCS.
expect "channel 1: static address 192\.168\.77\.10 mask 255\.255\.255\.0 gateway 192\.168\.77\.1 mac 52:54:00:aa:bb:cc vlan 7" "the LAN configuration written from outside must be read through KCS"
expect "channel 1 user 1 \(unnamed\) enabled privilege 2" "ipmi_sim's first user, a user, as configured"
expect "channel 1 user 2 ipmiusr enabled privilege 4" "ipmi_sim's second user, an administrator, as configured"
grep -qE '^2 +ipmiusr +true +false +true +ADMINISTRATOR' <<<"$(outside user list 1 || true)" || fail "oob: ipmitool did not list the administrator the guest read"
# INSIDE OUT: the boot's event the BMC service wrote through KCS is the record ipmitool reads over the LAN, field by field.
inside="$(grep -a -oE '0x0001 type 0x02 at [0-9]+ generator 0xf041 sensor type 0x1f sensor 0x00 asserted event 0x6f data 06 ff ff' "$(said)" | head -n 1)"
[[ -n "$inside" ]] || fail "oob: the guest did not read the boot's event as record 1 (see $kept/out-oob.log)"
outside sel get 1 >"$state/oob-sel.log" || fail "oob: ipmitool could not read record 1"
for field in "SEL Record ID *: 0001" "Record Type *: 02" "Generator ID *: f041" "Sensor Type *: OS Boot" "Sensor Number *: 00" "Event Direction *: Assertion Event" "Event Data *: 06ffff"; do
	grep -qE -- "$field" "$state/oob-sel.log" || fail "oob: record 1 read from outside has no '$field' (see $kept/oob-sel.log)"
done
stamp="$(awk '{print $5}' <<<"$inside")"
grep -qE "^ +1 \|.*\| *0*$stamp *\| OS Boot" <<<"$(outside sel list || true)" || fail "oob: record 1's timestamp $stamp read inside is not the one read from outside"
take_down
cp -f "$state/oob-sel.log" "$state/ipmi-sim.log" "$kept/" 2>/dev/null || true
kill "$helper" 2>/dev/null || true
wait "$helper" 2>/dev/null || true
helper=""
echo "ipmi: oob: ipmi_sim's LAN configuration written by ipmitool read through KCS, its users alike, and the boot's event written through KCS read over the LAN as the same record, field by field"
echo "ipmi: PASS - five interfaces, each its own BMC; SMBIOS agreeing with every ISA and SSIF node; sensors, FRU and one boot event through KCS and SSIF, a restart writing none again; a pair reported and written once; the harness BMC's LAN, users, identify, temperatures, follow, bound and absences; malformed records refused; an orderly reboot's shutdown record ahead of the next boot's; a sleep with KCS and SSIF bound, the BMC's watchdog stepped in order; ipmi_sim's LAN configuration and users read through KCS as ipmitool wrote and lists them, and the boot's event read over the LAN as the record written"
