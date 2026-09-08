#!/usr/bin/env bash
# Two overlapping preparations must keep their own fixture generation until the run copies it.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
# Load the production preparation and cleanup helpers without starting a guest.
sed -n '/^qemu_prepare_system_disk() {/,/^# The per-device virtio options/p; /^scratch_sweep() {/,/^}/p' "$HERE/qemu-run.sh" >"$work/helpers.sh"
mkdir -p "$work/shared"
for generation in a b; do
	mkdir -p "$work/$generation/boot" "$work/$generation/volume"
	printf '%s\n' "$generation" >"$work/$generation/volume/hello.txt"
	printf 'fixture %s\n' "$generation" >"$work/$generation/volume/motd.txt"
	printf '%s\n' "$generation" >"$work/$generation/system.img"
done
cat >"$work/run.sh" <<'RUN'
set -euo pipefail
work="$1" generation="$2"
source "$work/helpers.sh"
QEMU_BUILD_DIR="$work/shared"
QEMU_BOOT_DIR="$work/$generation/boot"
wait_for() {
	local attempt
	for ((attempt=0; attempt<1000; attempt++)); do
		[[ -f "$1" ]] && return 0
		sleep .01
	done
	echo "media-generations: the other preparation did not reach its barrier" >&2
	return 1
}
[[ "$generation" == a ]] || wait_for "$work/a.ready"
qemu_prepare_usb_image -test >"$work/$generation.format.log" 2>&1
usb="$USB_DISK"
system="$(qemu_prepare_system_disk "$work/$generation/system.img" "$QEMU_BUILD_DIR/system.img")"
printf '%s\n%s\n' "$usb" "$system" >"$work/$generation.paths"
touch "$work/$generation.ready"
# A deliberately waits until B has published both templates before opening its own copies.
[[ "$generation" == b ]] || wait_for "$work/b.ready"
usb_copy="$(qemu_run_disk "$usb")"
system_copy="$(qemu_run_disk "$system")"
mtype -i "$usb_copy" ::hello.txt >"$work/$generation.usb"
head -c 2 "$system_copy" >"$work/$generation.system"
RUN
bash "$work/run.sh" "$work" a &
a_pid=$!
bash "$work/run.sh" "$work" b &
b_pid=$!
status=0
wait "$a_pid" || status=1
wait "$b_pid" || status=1
[[ "$status" == 0 ]] || {
	echo "media-generations: fixture preparation failed" >&2
	exit 1
}
for generation in a b; do
	for disk in usb system; do
		[[ "$(<"$work/$generation.$disk")" == "$generation" ]] || {
			echo "media-generations: run $generation copied another run's $disk generation" >&2
			exit 1
		}
	done
done
[[ "$(head -1 "$work/a.paths")" != "$(head -1 "$work/b.paths")" ]]
[[ "$(tail -1 "$work/a.paths")" != "$(tail -1 "$work/b.paths")" ]]
# Without a way to observe live readers, cleanup must retain an old generation.
source "$work/helpers.sh"
touch -d '2 days ago' "$work/shared/unobservable.old.img"
command() {
	if [[ "$*" == '-v fuser' ]]; then return 1; fi
	builtin command "$@"
}
media_sweep "$work/shared/unobservable." .img ""
[[ -f "$work/shared/unobservable.old.img" ]]
echo "media-generations: overlapping USB and system preparations retained their own content"
