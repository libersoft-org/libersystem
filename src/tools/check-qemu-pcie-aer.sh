#!/usr/bin/env bash
# check-qemu-pcie-aer.sh - a PCIe function reports an error on a LIVE machine, and is quarantined.
#
# WHAT NO HOST TEST CAN SHOW. The AER tests drive a fake config space and the inventory's tests drive
# the table directly; both are about pieces. What this is about is the whole of it happening at once:
# hardware sets a status bit, a kernel reads it through a window ACPI described, says what happened in
# words, and - for a FATAL one - stops the function mastering the bus and tells whoever binds drivers,
# which takes the binding to `Quarantined`.
#
# THE INJECTION IS THE POINT. Every other gate here drives the system from outside through an
# interface it offers; this one makes the HARDWARE misbehave, which is the only way to reach this path
# at all. A machine that reports no error is a machine where none happened, and no amount of driving
# it produces one.
#
# THREE CLAIMS, AND THE THIRD IS THE ONE THE ITEM IS ABOUT:
#   1. A CORRECTED error is surfaced. Nothing failed - the hardware fixed it - and a machine that only
#      reported the fatal ones would report nothing until the disk went away.
#   2. A FATAL error on a function nothing binds is SAID and not acted on. The root port is a bridge;
#      there is no binding to quarantine, and the kernel says that rather than pretending.
#   3. A FATAL error on a BOUND function quarantines it. The driver is stopped, the binding lands at
#      `Quarantined` and not at `Removed` - the device has not gone anywhere - and nothing binds it
#      again.
#
# IT BOOTS ITS OWN GUEST unless one is already up, and takes down only what it started.

SCRIPT_NAME=check-qemu-pcie-aer.sh
source "$(dirname "${BASH_SOURCE[0]}")/../../lib.sh"

BOOTED=0
cleanup() {
	if ((BOOTED)); then
		"$REPO_ROOT/lab.sh" quit >/dev/null 2>&1 || true
	fi
}
trap cleanup EXIT

if ! "$REPO_ROOT/lab.sh" sh uname >/dev/null 2>&1; then
	note "no instance is up - booting one"
	"$REPO_ROOT/lab.sh" boot >/dev/null || die "the guest did not boot"
	BOOTED=1
fi

# THE MACHINE HAS TO BE ABLE TO REPORT ONE BEFORE ANYTHING IS INJECTED. An error record lives in
# EXTENDED config space, which the legacy port pair cannot address at all - so on a machine whose
# firmware published no MCFG this whole path is unreachable, and a gate that injected into it would
# report a kernel ignoring an error it was never able to see.
"$REPO_ROOT/lab.sh" log "extended config space at" 2>/dev/null | grep -q . || die "this kernel found no extended config space - an error record is unreachable without it"
"$REPO_ROOT/lab.sh" log "reports errors" 2>/dev/null | grep -q . || die "no function on this machine reports errors - the fixture is missing from the profile"

# How many lines of the guest's log match a pattern, right now. COUNTED AND NOT MATCHED: an instance
# that is already up has every previous run's lines in its log, so "does a line matching this exist"
# is answered before anything is injected. What a step means is that a line ARRIVED.
seen() {
	"$REPO_ROOT/lab.sh" log "$1" 2>/dev/null | grep -c . || true
}

await_line() {
	local needle="$1" what="$2" baseline="$3"
	for _ in $(seq 1 60); do
		if (($(seen "$needle") > baseline)); then
			return 0
		fi
		sleep 1
	done
	die "$what (waited for a new line matching: $needle)"
}

monitor() {
	"$REPO_ROOT/lab.sh" monitor "$1" >/dev/null 2>&1 || die "the monitor refused: $1"
}

# 1. A CORRECTED ERROR. Bit 0 of the correctable status is a receiver error: the link took a bad
#    symbol and recovered. Nothing failed, and a rising count of them is a link about to stop working.
corrected=$(seen "corrected a receiver error")
monitor "pcie_aer_inject_error -c hotplug0 0x00000001"
await_line "corrected a receiver error" "a corrected error was injected and the kernel never surfaced it" "$corrected"

# 2. A FATAL ERROR ON A BRIDGE. Bit 18 is a malformed TLP, fatal by the port's own severity register.
#    The port is not an endpoint and nothing binds it, so there is nothing to quarantine - and what
#    the kernel must NOT do is invent something.
reported=$(seen "reported a malformed TLP")
nothing=$(seen "not in this kernel's inventory, so there is nothing to quarantine")
monitor "pcie_aer_inject_error hotplug0 0x00040000"
await_line "reported a malformed TLP" "a fatal error was injected and the kernel never surfaced it" "$reported"
await_line "not in this kernel's inventory, so there is nothing to quarantine" "the kernel quarantined something for an error on a bridge nothing binds" "$nothing"

# 3. A FATAL ERROR ON A BOUND FUNCTION, WHICH IS WHAT THE ITEM IS ABOUT. A virtio-serial function is
#    plugged into the live slot WITH ERROR REPORTING ON, so it is an endpoint this system has a driver
#    for and can report an error about. Then the error is injected into IT.
arrived=$(seen "a device arrived in the slot behind")
watched=$(seen "reports errors")
monitor "device_add virtio-serial-pci,bus=hotplug0,id=liberaer,aer=on"
await_line "a device arrived in the slot behind" "the kernel never noticed the device that was plugged in" "$arrived"
await_line "DeviceManager: a device arrived on the bus" "the kernel saw the arrival and nothing was told to bind it" "$(($(seen "DeviceManager: a device arrived on the bus") - 1))"

faulted=$(seen "reported a malformed TLP")
quarantined=$(seen "is quarantined - it no longer masters the bus")
told=$(seen "reported a fatal error; quarantining it")
monitor "pcie_aer_inject_error liberaer 0x00040000"
await_line "reported a malformed TLP" "the error was injected into the endpoint and never surfaced" "$faulted"
await_line "is quarantined - it no longer masters the bus" "a fatal error on a bound device did not quarantine it" "$quarantined"
await_line "reported a fatal error; quarantining it" "the kernel quarantined the device and nothing was told to stop driving it" "$told"

# AND IT IS A QUARANTINE AND NOT A REMOVAL, which is the distinction the whole state is for: the
# device has NOT gone anywhere - it is still in the slot, still addressable - and a binding that
# landed at `Removed` would say it had.
removed_after=$(seen "the node is removed from the bus")
sleep 2
(($(seen "the node is removed from the bus") == removed_after)) || die "the faulted device's binding was recorded as REMOVED - it is still in the machine, and a removal says it is not"

monitor "device_del liberaer" || true
note "a corrected error was surfaced, a fatal one on a bridge was said and not acted on, and a fatal one on a bound device quarantined it without calling it a removal"
