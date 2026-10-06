#!/usr/bin/env bash
# THE COM1 HANDOFF, on a development instance: the kernel lets COM1 go to its userspace driver, takes it back
# whenever that driver goes, and still gets a panic onto the wire while a driver holds COM1 and has stopped
# draining the kernel's output.
#
# THE KERNEL LETTING GO IS COUNTED. The development kernel counts, in the one access path every kernel access
# to COM1's registers goes through, each access an ordinary kernel path makes while a driver holds the port,
# and its reacquisition line states the count; this gate requires ZERO at every reacquisition, so a drain, an
# interrupt handler or a poll left running cannot hide behind a driver that also works.
#
# THE STEPS, each a line that ARRIVED - counted before and after, never merely present:
#   1. the kernel's handoff line and the driver's online line, which with the count at zero can only have
#      crossed the tap and the driver;
#   2. `lab sh` round-trips through the driver: typed into the UART it reads, answered through the writes it
#      serves;
#   3. the driver's process killed (a development kernel request, SIG_KILL as DeviceManager sends it): the
#      kernel's reacquisition line with a zero count, DeviceManager's restart, the next handoff line - and
#      `lab sh` works again through the restarted driver;
#   4. the binding disabled through DeviceManager's policy verb, so nobody restarts it: the reacquisition
#      line, and `lab sh` answered through the kernel's own path;
#   5. the binding enabled again through the same verb, the next handoff line awaited, `lab sh` through the
#      driver;
#   6. while the driver keeps serving, a development request holds the tap's reads and writes kernel lines
#      until the ring is past its bound - `lab sh` still answers - and last a panic asked for over the
#      development channel puts the terminal-path writer's dropped count, then `*** KERNEL PANIC ***` and its
#      message, in the log.
#
# ACROSS A SLEEP is P02M0197b's to add to this gate: there is no sleep entry to drive yet.
#
# IT BOOTS ITS OWN INSTANCE in private state, as the development lifecycle gate does, and takes it down from
# the EXIT trap whatever happened.
#
# THE DEVICE-TREE PORTS, `--arch aarch64` or `--arch riscv64`: the same handoff of their console UART - the PL011, the
# 16550 in a register window - to `console_uart`, as one cold development boot (`lab scenario-cold`), since the
# persistent instance is x86_64's alone. The scenario `harness/scenarios/serial-handoff-port.toml`, filled for the port,
# drives steps 2 to 6 above - with `--serial-broker`, so the round trips are typed into the UART and answered through
# it - and this script then reads from the serial log what happened before the scenario's first step: the kernel's
# handoff line naming the UART and its receive line, the driver's online line and ConsoleService's attachment, and
# every reacquisition line's count at zero.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."

fail() {
	echo "serial-handoff: $*" >&2
	exit 1
}

# THE PORT VARIANT: the scenario filled for one port, run cold, and the boot's own lines read from its log.
port_gate() {
	local arch="$1" uart line online row
	case "$arch" in
	aarch64) uart="the PL011 at 0x9000000" line="INTID 33" online="driver.console_uart: online - PL011 at 0x9000000" row="dt:/pl011@9000000" ;;
	riscv64) uart="the 16550 at 0x10000000" line="APLIC source 10" online="driver.console_uart: online - 16550 at 0x10000000" row="dt:/soc/serial@10000000" ;;
	*) fail "--arch takes aarch64 or riscv64 - x86_64's handoff is this gate run with no argument" ;;
	esac
	local work log kept
	work="$(mktemp -d "${TMPDIR:-/tmp}/liber-handoff.XXXXXX")"
	log="$(pwd)/.build/boot/cold-$arch.log"
	kept="$(pwd)/.build/logs/serial-handoff-$arch"
	sed -e "s|@ARCH@|$arch|g" -e "s|@UART@|$uart|g" -e "s|@LINE@|$line|g" -e "s|@ONLINE@|$online|g" -e "s|@ROW@|$row|g" src/harness/scenarios/serial-handoff-port.toml >"$work/serial-handoff.toml"
	local status=0
	./lab.sh scenario-cold --serial-broker "$arch" "$work/serial-handoff.toml" || status=$?
	mkdir -p "$kept"
	cp -f "$log" "$kept/serial.log" 2>/dev/null || true
	rm -rf -- "$work"
	((status == 0)) || fail "the $arch scenario failed (serial log: $kept/serial.log)"
	[[ -f "$log" ]] || fail "the $arch scenario left no serial log at $log"
	# THE BOOT'S OWN HANDOFF, before the scenario's first step: the kernel's line naming the UART and the receive line
	# it let go, the driver online over it, and ConsoleService attached to it.
	grep -a -q -F -- "console: $uart ($line) is handed to row" "$log" || fail "the kernel never handed $uart to a claim with its receive line let go"
	grep -a -q -F -- "$online" "$log" || fail "the console UART's driver never came online"
	grep -a -q -F -- "go through the kernel console's UART driver" "$log" || fail "ConsoleService never attached to the console UART's driver"
	# EVERY REACQUISITION SAID ZERO: a drain, a receive handler or a poll left running is counted at the access path.
	local back counted
	back="$(grep -a -F -- "console: $uart is the kernel's again" "$log" || true)"
	[[ -n "$back" ]] || fail "the kernel never took $uart back"
	counted="$(grep -v -F -- "- 0 kernel access(es) to its registers while the driver held it" <<<"$back" || true)"
	if [[ -n "$counted" ]]; then
		echo "$counted" >&2
		fail "a reacquisition counted kernel accesses to $uart while the driver held it"
	fi
	echo "serial-handoff: PASS on $arch - $uart went to console_uart at boot and came back to the kernel on a kill and on a disable with zero stray kernel accesses each time, the UART answered through the driver and through the kernel, and a panic under a driver that had stopped draining reached the wire after the dropped count"
}

case "${1:-}" in
"") ;;
--arch)
	if [[ "${2:-x86_64}" != x86_64 ]]; then
		port_gate "$2"
		exit 0
	fi
	;;
*) fail "unexpected argument '$1'" ;;
esac

command -v python3 >/dev/null || fail "python3 is not installed, and the lab is written in it"
# `lab sh` talks to an ad-hoc `lab boot` guest when one is up, and this gate's own instance only when none is.
[[ ! -S .build/boot/lab-ctl.sock ]] || fail "an ad-hoc lab guest is up (.build/boot/lab-ctl.sock) - take it down with ./lab.sh quit first"

state="$(mktemp -d "${TMPDIR:-/tmp}/liber-sh.XXXXXX")"
export LIBER_DEV_STATE="$state"
HOSTFWD_PORT="$(python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])')"
export HOSTFWD_PORT
kept="$(pwd)/.build/logs/serial-handoff"

cleanup() {
	local status=$?
	./dev.sh down >"$state/down.log" 2>&1 || echo "serial-handoff: teardown reported a problem (see $kept/down.log)" >&2
	mkdir -p "$kept"
	cp -f "$state"/*.log "$kept/" 2>/dev/null || true
	rm -f .build/boot/*"-dev-$(basename "$state")"* 2>/dev/null || true
	rm -rf "$state"
	return "$status"
}
trap cleanup EXIT

lab() {
	./lab.sh "$@"
}

# THE INSTANCE'S OWN SERIAL LOG, in its private state - `lab log` reads an ad-hoc `lab boot` guest's.
serial_log() {
	echo "$state/dev-serial.log"
}

# How many lines of the guest's serial log match a pattern, right now.
seen() {
	grep -a -c -- "$1" "$(serial_log)" 2>/dev/null || true
}

# Wait for a line to ARRIVE past `baseline`, or fail saying which did not.
await_line() {
	local needle="$1" what="$2" baseline="$3" limit="${4:-90}"
	for _ in $(seq 1 "$limit"); do
		if (($(seen "$needle") > baseline)); then
			return 0
		fi
		sleep 1
	done
	fail "$what (waited ${limit} s for a new line matching: $needle)"
}

# EVERY reacquisition line so far says zero kernel accesses while the driver held COM1.
all_counts_zero() {
	local lines
	lines="$(grep -a -- "COM1 is the kernel's again" "$(serial_log)" 2>/dev/null || true)"
	[[ -n "$lines" ]] || return 0
	local counted
	counted="$(grep -v -- "- 0 kernel access(es) to its registers while the driver held it" <<<"$lines" || true)"
	if [[ -n "$counted" ]]; then
		echo "$counted" >&2
		fail "a reacquisition counted kernel accesses to COM1 while the driver held it"
	fi
}

# `lab sh` round trip: the command's output came back.
round_trip() {
	local word="$1" how="$2" out
	out="$(lab sh --timeout 60 echo "$word" 2>&1)" || fail "lab sh did not complete $how: $out"
	grep -q -- "$word" <<<"$out" || fail "lab sh did not answer $how (got: $out)"
	echo "serial-handoff: lab sh answered $how"
}

HANDED="console: COM1 (IRQ 4) is handed to row"
ONLINE="driver.uart16550: online"
BACK="console: COM1 is the kernel's again"
ATTACHED="go through the kernel console's UART driver"

echo "serial-handoff: state $state, host port $HOSTFWD_PORT - bringing a development instance up"
if ! ./dev.sh up --timeout 300 >"$state/up.log" 2>&1; then
	tail -20 "$state/up.log" >&2
	fail "the development instance did not come up (see $kept/up.log)"
fi

# 1. THE HANDOFF, AT BOOT.
await_line "$HANDED" "the kernel never handed COM1 to a claim" 0
await_line "$ONLINE" "the console UART's driver never came online" 0
await_line "$ATTACHED" "ConsoleService never attached to the console UART's driver" 0
all_counts_zero

# 2. THROUGH THE DRIVER.
round_trip "handoff-through-the-driver" "through the driver"

# 3. THE DRIVER KILLED: the kernel takes COM1 back, DeviceManager restarts the driver, the next handoff.
back=$(seen "$BACK")
restarted=$(seen "DeviceManager: restarting")
handed=$(seen "$HANDED")
attached=$(seen "$ATTACHED")
./dev.sh kernel-console kill-holder >/dev/null || fail "the development kernel would not kill the console UART's driver"
await_line "$BACK" "the kernel never took COM1 back after its driver was killed" "$back"
await_line "DeviceManager: restarting" "DeviceManager never reported the killed driver and restarted it" "$restarted"
await_line "$HANDED" "COM1 was never handed to the restarted driver" "$handed"
await_line "$ATTACHED" "ConsoleService never attached to the restarted driver" "$attached"
all_counts_zero
round_trip "handoff-after-the-restart" "through the restarted driver"

# 4. DISABLED: nobody restarts the driver, and the kernel's own path answers.
back=$(seen "$BACK")
out="$(./dev.sh launch --timeout 60 lsdev --disable kernel:com1 2>&1)" || fail "the disable was not run: $out"
grep -q "accepted" <<<"$out" || fail "the disable was not accepted: $out"
await_line "$BACK" "the kernel never took COM1 back when the binding was disabled" "$back"
all_counts_zero
round_trip "handoff-through-the-kernel" "through the kernel's own path"

# 5. ENABLED AGAIN: the next handoff, and the driver answers.
handed=$(seen "$HANDED")
attached=$(seen "$ATTACHED")
out="$(./dev.sh launch --timeout 60 lsdev --enable kernel:com1 2>&1)" || fail "the enable was not run: $out"
grep -q "accepted" <<<"$out" || fail "the enable was not accepted: $out"
await_line "$HANDED" "COM1 was never handed to the driver after the enable" "$handed"
await_line "$ATTACHED" "ConsoleService never attached again after the enable" "$attached"
round_trip "handoff-after-the-enable" "through the driver after the enable"
all_counts_zero

# 6. THE CASE THE TERMINAL PATH EXISTS FOR: a driver holding COM1 and not draining a full ring, then a panic.
out="$(./dev.sh kernel-console hold-and-flood 2>&1)" || fail "the flood was refused: $out"
echo "serial-handoff: $out"
round_trip "handoff-while-the-ring-is-full" "through the driver while the kernel's ring is past its bound"
dropped=$(seen "byte(s) of kernel output were dropped at the ring's bound before this point")
panicked=$(seen "\*\*\* KERNEL PANIC \*\*\*")
./dev.sh kernel-console panic >/dev/null || fail "the panic request was not sent"
await_line "byte(s) of kernel output were dropped at the ring's bound before this point" "the terminal-path writer did not put the dropped count on the wire" "$dropped" 60
await_line "\*\*\* KERNEL PANIC \*\*\*" "the panic did not reach the wire" "$panicked" 60
await_line "a panic asked for over the development channel" "the panic's message did not reach the wire" 0 60
# IN THAT ORDER: the count, then the panic.
order="$(grep -a -E -- "dropped at the ring's bound before this point|\*\*\* KERNEL PANIC \*\*\*" "$(serial_log)" 2>/dev/null | tail -2)"
grep -q "dropped at the ring's bound" <<<"$(head -1 <<<"$order")" || fail "the dropped count did not come before the panic: $order"
echo "serial-handoff: PASS - COM1 went to its driver and came back to the kernel on a kill and on a disable with zero stray kernel accesses each time, lab sh answered through both, and a panic under a driver that had stopped draining reached the wire after the dropped count"
