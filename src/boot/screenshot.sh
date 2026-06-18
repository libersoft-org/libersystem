#!/usr/bin/env bash
# screenshot.sh - capture an image of the kernel framebuffer.
#
# Usage: screenshot.sh <output-path>
#   The output format is chosen from the extension: png, jpg/jpeg, webp, gif, bmp
#   and ppm all work (anything ImageMagick can write; the netpbm fallback covers
#   png/jpg/ppm).
#
# If a `just run` instance is already up (its QEMU control-monitor socket exists
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
BUILD="$HERE/.build"
RUN_MON="$BUILD/qemu-monitor.sock" # control monitor exposed by a live `just run`
PPM="$BUILD/.screenshot.ppm"

mkdir -p "$BUILD"
rm -f "$PPM"

# Drive a QEMU HMP monitor over its unix socket to dump the framebuffer to $PPM.
screendump_via() {
	python3 - "$1" "$PPM" <<'PY'
import socket, sys, time
mon, ppm = sys.argv[1], sys.argv[2]
s = socket.socket(socket.AF_UNIX)
s.connect(mon)
time.sleep(0.2)
s.recv(65536)
s.sendall(("screendump %s\n" % ppm).encode())
time.sleep(1.0)
s.close()
PY
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
	python3 -c 'import socket,sys; socket.socket(socket.AF_UNIX).connect(sys.argv[1])' "$1" 2>/dev/null
}

# Fast path: a live `just run` is up - snap its current frame, no reboot.
if socket_live "$RUN_MON"; then
	echo "screenshot: attaching to the running QEMU (live frame)" >&2
	screendump_via "$RUN_MON"
	emit_image
	exit 0
fi

# Fallback: boot a throwaway headless instance and capture once it has booted.
KERNEL="$HERE/../kernel/target/x86_64-unknown-none/debug/kernel"
[[ -f "$KERNEL" ]] || {
	echo "screenshot: kernel ELF not found ($KERNEL) - run 'just build' first" >&2
	exit 1
}

ISO="$("$HERE/mkimage.sh" iso "$KERNEL")"
WAIT_LINE="${WAIT_LINE:-boot OK}"
TIMEOUT="${TIMEOUT:-30}"
LOG="$BUILD/.screenshot-serial.log"
MON="$BUILD/.screenshot-mon.sock"
rm -f "$LOG" "$MON"
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
cleanup() {
	kill "$QPID" 2>/dev/null || true
	pkill -f "tail -f $LOG" 2>/dev/null || true
	rm -f "$PPM" "$MON"
}
trap cleanup EXIT

# wait (bounded, no busy sleep) for the guest to finish booting
if ! timeout "$TIMEOUT" grep -q "$WAIT_LINE" <(tail -f "$LOG"); then
	echo "screenshot: '$WAIT_LINE' not seen within ${TIMEOUT}s, capturing current frame" >&2
fi
pkill -f "tail -f $LOG" 2>/dev/null || true

screendump_via "$MON"
emit_image
