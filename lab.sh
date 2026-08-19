#!/usr/bin/env bash
# Drive a live guest: boot one, run commands in it and read the answers, watch its log, take a
# picture of its screen, measure it, run the control protocol's conformance suite against it.
#
# Everything here needs a RUNNING SYSTEM, which is what separates it from check.sh. A gate reads
# artifacts and decides; these boot something and ask it. That distinction is why `perf-gate` and
# `proto-test` are subcommands here and not gates: `./check.sh` with no arguments must be runnable
# on a machine with nothing booted.
#
# Most subcommands are `boot/lab.py`'s own and are forwarded verbatim; three are scripts of their
# own that this names beside them, because "drive the instance" is one concern and it had three
# entry points.

SCRIPT_NAME=lab.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

help() {
	usage_and_exit <<EOF
usage: lab.sh SUBCOMMAND [ARGS...]

Drives a live guest instance. Every subcommand needs one running or boots one.

  boot [--fresh]        boot a throwaway instance (--fresh: a new data volume)
  sh COMMAND...         run a shell command in the guest and print its output
  wait [--timeout N]    wait for the shell prompt
  log [-f | PATTERN]    show, follow or grep the serial log
  key TEXT              type through the emulated keyboard
  int                   interrupt the foreground job
  monitor COMMAND...    one QEMU monitor command
  pcap on|off|dump      capture guest network traffic
  usb-attach            hot-plug the USB mass-storage stick; usb-detach unplugs it
  shot PATH             screenshot through the running instance
  test                  run the kernel test suite in the instance and summarize
  quit                  take the instance down
  scenario-cold ARGS    build a target, boot one guest, drive it, take it down - no persistent
                        instance, which is what lets a scenario run on aarch64 and riscv64
  screenshot PATH       the framebuffer to a file (format by extension: png, jpg, webp, ...);
                        snaps a live instance if one is up, else boots a throwaway
  proto-test [GROUPS]   the development-control protocol's conformance suite, all groups or these
  perf-gate             one warm no-change build and one warm leaf iteration against the running
                        instance, failing when either misses its budget

  --list                print the subcommands and exit
  -h, --help            this text

The PERSISTENT development instance is ./dev.sh, not this: ./dev.sh up, ./dev.sh status,
./dev.sh loop. One instance, one way to drive it.

examples:
  ./lab.sh boot --fresh
  ./lab.sh sh time ls
  ./lab.sh screenshot shot.png
  ./lab.sh proto-test registry publication
  ./lab.sh help                  # boot/lab.py's own help, with every option
EOF
}

# The three that are not `boot/lab.py` subcommands. Kept in one list so the help above and the
# dispatch below cannot disagree about which is which.
OWN_SCRIPT=(screenshot proto-test perf-gate)

[[ $# -ge 1 ]] || help

case "$1" in
-h | --help) help ;;
--list)
	echo "lab.py:      boot sh wait log key int monitor pcap usb-attach usb-detach shot test quit scenario-cold"
	echo "persistent:  ./dev.sh (up, down, status, console, log, loop, test, ...)"
	echo "own script:  ${OWN_SCRIPT[*]}"
	exit 0
	;;
screenshot)
	shift
	[[ $# -eq 1 ]] || die "screenshot needs exactly one path (the format comes from its extension)"
	exec bash -c 'cd "$1" && exec boot/screenshot.sh "$2"' _ "$SRC_DIR" "$1"
	;;
proto-test)
	shift
	exec bash -c 'cd "$1" && shift && exec boot/proto-test.py "$@"' _ "$SRC_DIR" "$@"
	;;
perf-gate)
	[[ $# -eq 1 ]] || die "perf-gate takes no arguments"
	exec bash -c 'cd "$1" && exec boot/perf-gate.py' _ "$SRC_DIR"
	;;
dev-*)
	# ONE WAY TO DRIVE THE PERSISTENT INSTANCE, and it is ./dev.sh. `boot/lab.py` answers
	# `dev-up` as well, so forwarding it from here would work - and would be a second spelling
	# of a command that already has one, which is the thing this milestone is removing.
	die "the persistent instance is ./dev.sh: try './dev.sh ${1#dev-}'"
	;;
-*)
	die "unexpected option '$1' (try --help)"
	;;
*)
	exec bash -c 'cd "$1" && shift && exec boot/lab.py "$@"' _ "$SRC_DIR" "$@"
	;;
esac
