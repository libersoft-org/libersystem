#!/usr/bin/env bash
# check-qemu-pcie-hotplug.sh - a device plugged into a LIVE machine, and taken out of it again.
#
# WHAT NO HOST TEST CAN SHOW. The slot protocol's own tests drive a fake config space and the
# inventory's tests drive the table directly; both are about pieces. What this is about is the whole
# of it happening at once, on a machine that is already running: a port asserts, a kernel reads a
# slot, an inventory grows a row, a manager binds a driver to it - and then somebody asks for the
# device back, the driver is stopped, its claim is released, and only THEN does the slot go down.
#
# THE ORDER IS THE CLAIM. A removal that powered the slot off first would work just as well in a log
# that only counted the lines, and would be the surprise removal this whole path is defined against.
# So the lines are read IN ORDER, and the run fails if the slot goes down before the driver says it
# has let go.
#
# IT BOOTS ITS OWN GUEST unless one is already up, and takes down only what it started.

SCRIPT_NAME=check-qemu-pcie-hotplug.sh
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

# THE SLOT HAS TO BE THERE BEFORE ANYTHING IS PLUGGED INTO IT, and the boot says so: an empty slot is
# invisible otherwise, and a gate that plugged into a machine with no slot would report the kernel
# ignoring a device it was never told about.
"$REPO_ROOT/lab.sh" log "carries hot-plug slot" 2>/dev/null | grep -q . || die "this machine has no hot-plug slot - the fixture is missing from the profile"

# How many lines of the guest's log match a pattern, right now.
#
# COUNTED AND NOT MATCHED, and this is the difference between a gate and a gate that passes on
# yesterday's evidence. An instance that is already up has a log with every previous cycle in it, so
# "does a line matching this exist" is answered `yes` before anything is plugged in - and the whole
# run then passes in three seconds without a device having moved. What a step means is that a line
# ARRIVED, which is a count that GREW.
seen() {
	"$REPO_ROOT/lab.sh" log "$1" 2>/dev/null | grep -c . || true
}

# Wait for a new line to appear, or give up saying which one did not come.
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

# 1. PLUG. A virtio-serial function, because this system has a driver for one and the point is that a
#    driver BINDS - a device nothing can drive would prove the kernel noticed and nothing more.
arrived=$(seen "a device arrived in the slot behind")
bound=$(seen "DeviceManager: a device arrived on the bus")
monitor "device_add virtio-serial-pci,bus=hotplug0,id=liberhp"
await_line "a device arrived in the slot behind" "the kernel never noticed the device that was plugged in" "$arrived"
await_line "DeviceManager: a device arrived on the bus" "the kernel saw the arrival and nothing was told to bind it" "$bound"

# 2. ASK FOR IT BACK. `device_del` presses the attention button; it does NOT take the device out.
requested=$(seen "the removal of device .* was requested at the slot behind")
asked=$(seen "device was removed from the bus; stopping it")
landed=$(seen "the node is removed from the bus")
powered_down=$(seen "nothing holds the device .* powering the slot down")
emptied=$(seen "the slot behind .* is empty")
monitor "device_del liberhp"
await_line "the removal of device .* was requested at the slot behind" "the request never reached the kernel" "$requested"
await_line "device was removed from the bus; stopping it" "the driver was never asked to stop" "$asked"
await_line "the node is removed from the bus" "the binding never landed at removed" "$landed"
await_line "nothing holds the device .* powering the slot down" "the slot was never powered down, so the device is still in it" "$powered_down"
await_line "the slot behind .* is empty" "the port never took the device out" "$emptied"

# 3. AND THE ORDER, WHICH IS THE WHOLE CLAIM. Read the lines back and check that the slot went down
#    AFTER the driver let go. A run where they came the other way round is a surprise removal, and
#    every line above would be present for it.
# `lab log PATTERN` is a grep over the whole serial log, which is what "in order" needs: the tail
# the bare form prints is the last few lines and the first cycle's are long gone by then.
log="$("$REPO_ROOT/lab.sh" log "the node is removed from the bus\|powering the slot down" 2>/dev/null)"
stopped=$(grep -n "the node is removed from the bus" <<<"$log" | tail -n 1 | cut -d: -f1)
powered=$(grep -n "powering the slot down" <<<"$log" | tail -n 1 | cut -d: -f1)
[[ -n "$stopped" && -n "$powered" ]] || die "the two lines the order is about are not both in the log"
((stopped < powered)) || die "the slot was powered down at line $powered, before the driver let go at line $stopped - that is a surprise removal"

# 4. AND THE SLOT WORKS TWICE. A slot left powered down after a removal is a slot that works exactly
#    once, and nothing above would notice: every line of the first cycle is already in the log.
before=$(seen "a device arrived in the slot behind")
monitor "device_add virtio-serial-pci,bus=hotplug0,id=liberhp2"
await_line "a device arrived in the slot behind" "the slot took a device once and not twice - it was left powered down" "$before"
monitor "device_del liberhp2"

note "a device was plugged into a live machine and bound, asked for and given back in that order, and the slot took another one afterwards"
