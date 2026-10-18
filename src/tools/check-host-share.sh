#!/usr/bin/env bash
# THE HOST SHARE, end to end on x86_64: a directory of this host served read-only by `virtiofsd`, reached by the
# development image's virtio-fs driver, mounted by StorageService as `vol://host` and granted to `share` alone.
#
#   1. WHAT `share` LISTS AND PRINTS IS THE DIRECTORY'S: its names and kinds, a file's bytes, a file read across
#      several of the share's 16 KiB reads line for line, a file in a subdirectory - and a change this script makes on
#      the host after the boot, read again.
#   2. NOTHING WALKS OUT OF IT: `..` and `.` in a path, a symbolic link to a file outside the share, read, and one to
#      the share's parent, walked through, are each refused, and a host name no volume can hold is not listed.
#   3. NOTHING ELSE SEES IT: `cat`, which holds every volume a tool is handed, cannot open a file of `vol://host`, and
#      `ls vol://host` lists nothing.
#
# The share directory is this run's, under its private state, and goes with it. The development image is the one
# `LIBER_DEVELOPMENT=1 ./image.sh --format iso` wrote - a shipping image carries no virtio-fs driver and no `share`.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."

VIRTIOFSD="${VIRTIOFSD:-/usr/libexec/virtiofsd}"

fail() {
	echo "host-share: $*" >&2
	exit 1
}

say() {
	echo "host-share: $*"
}

[[ -x "$VIRTIOFSD" ]] || fail "virtiofsd is not installed at $VIRTIOFSD (setup.sh installs it)"
command -v python3 >/dev/null || fail "python3 is not installed, and the lab is written in it"
[[ ! -S .build/boot/lab-ctl.sock ]] || fail "an ad-hoc lab guest is up (.build/boot/lab-ctl.sock) - take it down with ./lab.sh quit first"

state="$(mktemp -d "${TMPDIR:-/tmp}/liber-hostshare.XXXXXX")"
share="$state/share"
outside="$state/outside.txt"
mkdir -p "$share/docs"
export LIBER_DEV_STATE="$state"
HOSTFWD_PORT="$(python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])')"
export HOSTFWD_PORT
kept="$(pwd)/.build/logs/host-share"
rm -rf "$kept"
mkdir -p "$kept"
daemon=""

cleanup() {
	local status=$?
	cp -f "$state/dev-serial.log" "$kept/serial.log" 2>/dev/null || true
	./dev.sh down >"$state/down.log" 2>&1 || echo "host-share: teardown reported a problem (see $kept/down.log)" >&2
	if [[ -n "$daemon" ]]; then
		kill "$daemon" 2>/dev/null || true
		wait "$daemon" 2>/dev/null || true
	fi
	cp -f "$state"/*.log "$kept/" 2>/dev/null || true
	rm -f .build/boot/*"-dev-$(basename "$state")"* 2>/dev/null || true
	rm -rf "$state"
	return "$status"
}
trap cleanup EXIT

has() {
	grep -a -q -F -- "$1" "$state/dev-serial.log" || fail "$2 (no line containing: $1)"
}

launch() {
	./dev.sh launch --timeout 60 "$@" 2>&1
}

# A LAUNCH, its output kept in `$state/<log>.log`.
launched() {
	local log="$1"
	shift
	launch "$@" >"$state/$log.log" || true
	grep -a -q -F "lab: (output was truncated" "$state/$log.log" && fail "the output of '$*' was cut short" || true
}

says() {
	grep -a -q -F -- "$2" "$state/$1.log" || fail "$3 (the output of $1 has no: $2)"
}

never() {
	! grep -a -q -F -- "$2" "$state/$1.log" || fail "$3 (the output of $1 has: $2)"
}

# THE DIRECTORY: a file, one in a subdirectory, one longer than two of the share's reads, a link to a file outside the
# share and one to its parent, and a name no volume holds.
printf 'hello from the host share\n' >"$share/hello.txt"
printf 'the inner line\n' >"$share/docs/inner.txt"
python3 -c 'import sys
with open(sys.argv[1], "w") as out:
    for line in range(1, 641):
        out.write(f"line {line:05d} of the long file, past two of the share'"'"'s reads\n")' "$share/long.txt"
printf 'a secret of the host, outside the share\n' >"$outside"
ln -s "$outside" "$share/outside"
ln -s .. "$share/up"
python3 -c 'import os, sys; open(os.path.join(os.fsencode(sys.argv[1]), b"bad\xff"), "w").close()' "$share"
long_lines="$(wc -l <"$share/long.txt")"
long_size="$(stat -c %s "$share/long.txt")"
# PAST TWO OF THE SHARE'S 16 KiB READS, and inside the 64 KiB of a launch's output the development agent keeps.
((long_size > 32768 && long_size < 60000)) || fail "the long file is $long_size bytes, not past two 16 KiB reads and inside a launch's output"

# THE DAEMON, read-only, sandboxed, without extended attributes or a cache - so a change on the host is read again.
"$VIRTIOFSD" --shared-dir "$share" --socket-path "$state/virtiofs.sock" --readonly --sandbox namespace --cache never >"$state/virtiofsd.log" 2>&1 &
daemon=$!
for _ in $(seq 1 50); do
	[[ -S "$state/virtiofs.sock" ]] && break
	kill -0 "$daemon" 2>/dev/null || fail "virtiofsd exited before it listened (see $kept/virtiofsd.log)"
	sleep 0.2
done
[[ -S "$state/virtiofs.sock" ]] || fail "virtiofsd did not listen on its socket"
export VIRTIOFS_SOCKET="$state/virtiofs.sock"
# WITHOUT THE IOMMU: `virtiofsd` offers no IOTLB, so the device cannot sit behind the controller (see `qemu-run.sh`).
export IOMMU=0

say "booting the development image with the share"
./dev.sh up --timeout 400 >"$state/up.log" 2>&1 || fail "the development guest did not come up (see $kept/up.log)"
has "driver.virtio-fs: online" "the virtio-fs driver did not bind the share"
has "tag host, read-only" "the driver did not read the device's tag"
has "StorageService: online (vol://host" "StorageService did not serve the host share's volume"

# 1. WHAT IT LISTS AND PRINTS.
launched list share ls
says list " hello.txt" "the listing has no hello.txt"
says list "d          0 docs" "the listing does not name docs a directory"
says list "$(printf -- '- %10d long.txt' "$long_size")" "the listing does not give long.txt its size"
says list " outside" "the listing does not name the link at all"
never list "bad" "a host name no volume can hold was listed"
says list "share: 5 entries" "the listing is not the five names a volume can hold"
launched hello share cat hello.txt
says hello "hello from the host share" "the file's bytes are not the host's"
launched inner share cat docs/inner.txt
says inner "the inner line" "a file in a subdirectory is not the host's"
launched long share cat long.txt
says long "line 00001 of the long file" "the long file's first line is missing"
says long "line 00320 of the long file" "the long file's middle is missing"
says long "line $(printf '%05d' "$long_lines") of the long file" "the long file's last line is missing"
[[ "$(grep -a -c 'of the long file, past two' "$state/long.log")" == "$long_lines" ]] || fail "the long file did not come back whole, line for line"
printf 'changed on the host after the boot\n' >"$share/hello.txt"
launched changed share cat hello.txt
says changed "changed on the host after the boot" "a change on the host was not read again"
say "the share's names, kinds, sizes and bytes are the host's, and a change on the host is read again"

# 2. NOTHING WALKS OUT OF IT.
# A DOT OR A DOT-DOT is refused by the volume API's own path rule before the share is asked (`not found`: no volume
# path has one); a link is refused by the share (`refused`). Either way the answer is an error line and no bytes.
for path in "../outside.txt" "docs/../hello.txt" "./hello.txt"; do
	launched escape share cat "$path"
	says escape "share: $path: not found" "'share cat $path' was not refused by the path rule"
	never escape "a secret of the host" "'share cat $path' reached a file outside the share"
	never escape "hello from" "'share cat $path' reached a file by another name"
done
for path in "outside" "up/hello.txt" "up/outside.txt"; do
	launched escape share cat "$path"
	says escape "share: $path: refused" "'share cat $path' was not refused"
	never escape "a secret of the host" "'share cat $path' reached a file outside the share"
	never escape "hello from" "'share cat $path' reached a file by another name"
done
launched escape-list share ls up
says escape-list "share: up: refused" "a link to the share's parent was walked through"
say "a dot, a dot-dot and both links are refused, and nothing outside the share was read"

# 3. NOTHING ELSE SEES IT.
launched others cat vol://host/hello.txt
says others "cannot open" "cat, which holds every volume a tool is handed, opened a file of the host share"
never others "changed on the host" "cat read the host share"
launched others-list ls vol://host
never others-list "hello.txt" "ls listed the host share"
say "neither cat nor ls - which hold every volume a tool is handed - reaches the host share"
say "PASS"
