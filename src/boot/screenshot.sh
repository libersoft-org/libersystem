#!/usr/bin/env bash
# screenshot.sh - capture an image of the kernel framebuffer.
#
# Usage: screenshot.sh <output-path>
#   The output format is chosen from the extension: png, jpg/jpeg, webp, gif, bmp
#   and ppm all work (anything ImageMagick can write; the netpbm fallback covers
#   png/jpg/ppm).
#
# If a `./run.sh` instance is already up (its QEMU control-monitor socket exists
# and accepts a connection), this attaches to it and snaps the CURRENT frame, so
# a screenshot can be taken at any moment during a live run - no reboot. If no
# run is up, it boots a throwaway headless instance, waits for the boot log to
# finish, snaps that, and shuts it down.
#
# Env:
#   WAIT_LINE  serial-log line to wait for in the fallback boot (default "boot OK")
#   TIMEOUT    seconds to wait for that line before capturing anyway (default 30)
#   NOKVM=1    disable KVM in the fallback boot

set -euo pipefail

OUT="${1:?usage: screenshot.sh <output-path> (e.g. screenshot.png, shot.jpg, shot.webp)}"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
BUILD="$REPO_ROOT/.build/boot"
RUN_MON="$BUILD/qemu-monitor.sock" # control monitor exposed by a live `./run.sh`

mkdir -p "$BUILD"
# A RUN DIRECTORY OF ITS OWN, because every path here used to be a singleton.
#
# `.screenshot.ppm`, `.screenshot-serial.log` and `.screenshot-mon.sock` were fixed names deleted at
# startup, so two captures overwrote each other's framebuffer, log and monitor socket, and one
# cleanup removed paths the other was still using. A live-instance capture and a fallback capture
# raced on the same PPM too. Nothing here is shared any more, so concurrent captures cannot see each
# other at all.
SCRATCH="$(mktemp -d "$BUILD/.screenshot.XXXXXX")"
PPM="$SCRATCH/frame.ppm"

# Drive a QEMU HMP monitor over its unix socket to dump the framebuffer to $PPM.
# The trailing pause keeps the connection open long enough for QEMU to run the
# screendump and flush the file before socat closes it.
screendump_via() {
	{
		printf 'screendump %s\n' "$PPM"
		sleep 1
	} | socat - UNIX-CONNECT:"$1" >/dev/null 2>&1
}

# Convert the captured PPM to the requested output format (chosen by the output
# extension) and print the final path. ImageMagick handles png/jpg/webp/gif/bmp;
# the netpbm fallback covers png/jpg/ppm.
emit_image() {
	[[ -s "$PPM" ]] || {
		echo "screenshot: framebuffer dump failed (no PPM produced)" >&2
		exit 1
	}
	mkdir -p "$(dirname "$OUT")"
	local ext="${OUT##*.}"
	ext="${ext,,}"
	if command -v convert >/dev/null 2>&1; then
		convert "$PPM" "$OUT"
	else
		case "$ext" in
		png) pnmtopng "$PPM" >"$OUT" 2>/dev/null ;;
		jpg | jpeg) pnmtojpeg "$PPM" >"$OUT" 2>/dev/null ;;
		ppm) cp "$PPM" "$OUT" ;;
		*)
			echo "screenshot: '.$ext' needs ImageMagick (install imagemagick); netpbm fallback writes png/jpg/ppm only" >&2
			rm -f "$PPM"
			exit 1
			;;
		esac
	fi
	rm -f "$PPM"
	echo "screenshot: wrote $OUT" >&2
	echo "$OUT"
}

# True if a process is listening on the unix socket (not just a stale file).
socket_live() {
	[[ -S "$1" ]] || return 1
	socat -u OPEN:/dev/null UNIX-CONNECT:"$1" 2>/dev/null
}

# Fast path: a live `./run.sh` is up - snap its current frame, no reboot.
if socket_live "$RUN_MON"; then
	echo "screenshot: attaching to the running QEMU (live frame)" >&2
	screendump_via "$RUN_MON"
	emit_image
	exit 0
fi

# Fallback: boot a throwaway headless instance and capture once it has booted.
KERNEL="$REPO_ROOT/.build/cargo/kernel/x86_64-unknown-none/debug/kernel"
[[ -f "$KERNEL" ]] || {
	echo "screenshot: kernel ELF not found ($KERNEL) - run 'just build' first" >&2
	exit 1
}

ISO="$("$HERE/mkimage.sh" iso "$KERNEL")"
WAIT_LINE="${WAIT_LINE:-boot OK}"
TIMEOUT="${TIMEOUT:-30}"
LOG="$SCRATCH/serial.log"
MON="$SCRATCH/mon.sock"
: >"$LOG"

QEMU_ARGS=(
	-machine q35
	-m 512M
	-cdrom "$ISO"
	-boot d
	-serial "file:$LOG"
	-display none
	-no-reboot
	-monitor "unix:$MON,server,nowait"
)
if [[ "${NOKVM:-0}" != "1" && -e /dev/kvm ]]; then
	QEMU_ARGS+=(-enable-kvm -cpu host -smp 4)
else
	QEMU_ARGS+=(-cpu qemu64 -smp 4)
fi

echo "screenshot: no live run found, booting a throwaway instance" >&2
qemu-system-x86_64 "${QEMU_ARGS[@]}" &
QPID=$!
# EXACT PIDS, never a pattern. Cleanup used to `pkill -f "tail -f $LOG"`, which kills every process
# on the machine whose command line matches - including an unrelated `tail -f` a person had open on
# the same file. The follower this starts is a child whose pid is right here.
TAILPID=""
cleanup() {
	kill "$QPID" 2>/dev/null || true
	[[ -n "$TAILPID" ]] && kill "$TAILPID" 2>/dev/null
	rm -rf "$SCRATCH"
}
trap cleanup EXIT

# wait (bounded, no busy sleep) for the guest to finish booting
tail -f "$LOG" >"$SCRATCH/follow" &
TAILPID=$!
if ! timeout "$TIMEOUT" bash -c 'while ! grep -q "$1" "$2"; do sleep 0.2; done' _ "$WAIT_LINE" "$SCRATCH/follow"; then
	echo "screenshot: '$WAIT_LINE' not seen within ${TIMEOUT}s, capturing current frame" >&2
fi
kill "$TAILPID" 2>/dev/null || true
TAILPID=""

screendump_via "$MON"
emit_image
