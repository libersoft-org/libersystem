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
if [[ "$generation" == a ]]; then
	# Cache age says nothing about a new reader. Reuse old templates, then let B sweep before
	# this run opens either private copy. No file descriptor protects this acquisition interval.
	touch -d '2 days ago' "$usb" "$system"
	qemu_prepare_usb_image -test >>"$work/$generation.format.log" 2>&1
	[[ "$USB_DISK" == "$usb" ]]
	[[ "$(qemu_prepare_system_disk "$work/$generation/system.img" "$QEMU_BUILD_DIR/system.img")" == "$system" ]]
fi
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
# Once both runner PIDs have exited, every generation no live run leases is reclaimed with its leases, whatever its
# age; the generation a live run selects is kept, and its exited readers' leases are discarded.
system_template="$(tail -1 "$work/a.paths")"
private_disk="${system_template%.img}.$$.img"
touch -d '2 days ago' "$private_disk"
kept_template="$(tail -1 "$work/b.paths")"
media_sweep "$work/shared/system." .img "$kept_template"
media_sweep "$work/shared/usb-media-test." .img ""
[[ -f "$private_disk" ]]
while IFS= read -r template; do
	[[ ! -f "$template" ]]
	if compgen -G "$template.lease.*" >/dev/null; then
		echo "media-generations: an exited runner retained its lease" >&2
		exit 1
	fi
done <"$work/a.paths"
[[ ! -f "$(head -1 "$work/b.paths")" ]] || {
	echo "media-generations: a generation no live run leases was kept" >&2
	exit 1
}
for lease in "$kept_template".lease.*; do
	[[ "$lease" == "$kept_template.lease.$$" ]]
done
[[ -f "$kept_template" ]]
unobservable="$work/shared/unobservable.$(printf '%064d' 0).img"
touch -d '2 days ago' "$unobservable"
command() {
	if [[ "$*" == '-v fuser' ]]; then return 1; fi
	builtin command "$@"
}
media_sweep "$work/shared/unobservable." .img ""
[[ -f "$unobservable" ]]
echo "media-generations: each generation survived its acquisition, and the ones no live run leases were reclaimed after exit"
# A RUN'S FILES GO WITH THE RUN. `run-files.py sweep` takes every file an ended pid owns, whatever its prefix, and leaves
# a live pid's, a template with its key, and a set's lock; a development set goes once its state directory has; and
# `reap` waits for its owner, then takes the owner's disks and keeps what a gate reads after the run.
files="$work/run-files"
mkdir -p "$files" "$work/state-here"
dead="$(bash -c 'echo $$')"
key="$(printf '%064d' 7)"
for name in "virtio-blk.$key.img" "virtio-blk.$key.img.key" "virtio-blk.$key.$dead.img" "virtio-blk.$key.img.lease.$dead" \
	"esp-x86_64.$dead.img" "virtio-blk.$key.$$.img" "virtio-blk-dev-x.lock" "virtio-console.$dead.out" \
	"virtio-blk-dev-gone.$key.img" "usb-media-dev-gone.lock" "virtio-blk-dev-here.$key.img"; do
	: >"$files/$name"
done
printf '%s\n' "$work/state-gone" >"$files/state-dev-gone.path"
printf '%s\n' "$work/state-here" >"$files/state-dev-here.path"
python3 "$HERE/run-files.py" sweep "$files"
for name in "virtio-blk.$key.$dead.img" "virtio-blk.$key.img.lease.$dead" "esp-x86_64.$dead.img" "virtio-console.$dead.out" \
	"virtio-blk-dev-gone.$key.img" "usb-media-dev-gone.lock" "state-dev-gone.path"; do
	[[ ! -e "$files/$name" ]] || {
		echo "run-files: the sweep kept $name, which no live run owns" >&2
		exit 1
	}
done
for name in "virtio-blk.$key.img" "virtio-blk.$key.img.key" "virtio-blk.$key.$$.img" "virtio-blk-dev-x.lock" \
	"virtio-blk-dev-here.$key.img" "state-dev-here.path"; do
	[[ -e "$files/$name" ]] || {
		echo "run-files: the sweep removed $name, which is in use or no run's" >&2
		exit 1
	}
done
sleep 30 &
owner=$!
for name in "virtio-blk.$key.$owner.img" "scsi-scratch.$owner.img" "virtio-console.$owner.out" "usb-media.$key.$owner.img"; do
	: >"$files/$name"
done
python3 "$HERE/run-files.py" reap "$owner" "$files" &
reaper=$!
sleep 0.5
[[ -e "$files/virtio-blk.$key.$owner.img" ]] || {
	echo "run-files: the reaper took a live run's disk" >&2
	exit 1
}
kill "$owner"
wait "$owner" 2>/dev/null || true
wait "$reaper"
for name in "virtio-blk.$key.$owner.img" "scsi-scratch.$owner.img"; do
	[[ ! -e "$files/$name" ]] || {
		echo "run-files: the reaper kept $name after its run ended" >&2
		exit 1
	}
done
for name in "virtio-console.$owner.out" "usb-media.$key.$owner.img"; do
	[[ -e "$files/$name" ]] || {
		echo "run-files: the reaper removed $name, which a gate reads after the run" >&2
		exit 1
	}
done
echo "run-files: an ended run's files swept whatever their prefix, a live run's kept, a set gone with its directory, and a run's disks reaped at its end"
