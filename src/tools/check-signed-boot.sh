#!/usr/bin/env bash
# A boot that must NOT happen: the loader refusing a signed manifest that was altered after signing.
#
# ONE SUCCESSFUL SIGNED BOOT PROVES ALMOST NOTHING. It proves the pieces fit together; it does not
# prove the check is load-bearing, because a check that always answers yes passes exactly the same
# boot. The evidence a trust chain needs is the boot that STOPS - so this flips one byte of the
# medium's manifest and requires the loader to refuse before it loads a kernel.
#
# x86_64 ONLY, and by design: this is one reproducible chain rather than a claim about every port.
set -euo pipefail

cd "$(dirname "$0")/../.."
BUILD=".build/boot"
ISO="$BUILD/libersystem.iso"
OVMF_CODE="${OVMF_CODE:-/usr/share/OVMF/OVMF_CODE_4M.fd}"
OVMF_VARS="${OVMF_VARS_SRC:-/usr/share/OVMF/OVMF_VARS_4M.fd}"

fail() {
	echo "signed-boot: $*" >&2
	exit 1
}

[[ -f "$ISO" ]] || fail "no $ISO - run ./image.sh --format iso"
[[ -f "$OVMF_CODE" && -f "$OVMF_VARS" ]] || fail "OVMF firmware not found (install the 'ovmf' package)"
command -v qemu-system-x86_64 >/dev/null || fail "qemu-system-x86_64 is required"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# One boot, one medium, one serial log.
boot_medium() {
	local efi="$1" log="$2" vars="$work/vars.$$.fd"
	cp "$OVMF_VARS" "$vars"
	timeout 120 qemu-system-x86_64 \
		-machine q35 -m 2G -display none -no-reboot \
		-drive "if=pflash,format=raw,readonly=on,file=$OVMF_CODE" \
		-drive "if=pflash,format=raw,file=$vars" \
		-drive "format=raw,file=$efi" \
		-serial "file:$log" >/dev/null 2>&1 || true
	rm -f "$vars"
}

# THE UNALTERED MEDIUM FIRST, and this is not ceremony. A gate that only boots the tampered medium
# passes identically whether the loader refused the alteration or the machine never booted at all -
# and the second is the more likely of the two to happen quietly. So: this medium loads a kernel,
# and then the same medium with one byte changed does not.
efi="$work/efiboot.img"
cp "$BUILD/efiboot.img" "$efi"
good_log="$work/good.log"
boot_medium "$efi" "$good_log"
grep -aq "loader: kernel loaded" "$good_log" || {
	echo "signed-boot: the UNALTERED medium did not load a kernel - this test cannot tell a refusal from a broken boot" >&2
	sed -n '1,40p' "$good_log" >&2
	exit 1
}
# AND THAT BOOT IS ALSO THE "GENUINELY ABSENT SOURCE" CASE. No volume disk is attached, so the
# system volume is absent rather than invalid, and the loader falls back to the medium and boots -
# which is the distinction M4 draws, seen from the outside.
echo "signed-boot: the unaltered medium boots and loads a kernel (with no system volume attached, which is the absent-source case)"

cp "$BUILD/efiboot.img" "$efi"

# THE ALTERATION IS ONE BYTE OF THE SIGNED MANIFEST, and it is made where a tamperer would make it:
# on the medium, after the build. The digest of a payload is what it names, so this is the "manifest
# modified without changing a payload" case - the one a text manifest cannot tell from a legitimate
# rebuild.
mcopy -i "$efi" ::/etc/boot.manifest2 "$work/manifest2" || fail "the medium carries no signed manifest"
before="$(sha256sum "$work/manifest2" | cut -d' ' -f1)"
printf '\x01' | dd of="$work/manifest2" bs=1 seek=40 count=1 conv=notrunc status=none
after="$(sha256sum "$work/manifest2" | cut -d' ' -f1)"
[[ "$before" != "$after" ]] || fail "the manifest did not change - this test would prove nothing"
mcopy -o -i "$efi" "$work/manifest2" ::/etc/boot.manifest2

log="$work/altered.log"
boot_medium "$efi" "$log"

refused=0
grep -aq "refusing to boot from it\|does not check out\|is not what the boot medium" "$log" && refused=1
loaded=0
grep -aq "loader: kernel loaded" "$log" && loaded=1

if ((refused == 0 || loaded == 1)); then
	echo "signed-boot: an altered signed manifest did NOT stop the boot" >&2
	echo "signed-boot: refused=$refused loaded=$loaded; the serial log follows" >&2
	sed -n '1,40p' "$log" >&2
	exit 1
fi
echo "signed-boot: the loader refused an altered signed manifest and loaded no kernel"

# AND THE PAYLOAD THE MEDIUM HANDS THE KERNEL. The system volume image is read as a filesystem and
# published as a module, so the manifest covers the whole of it - and a byte changed inside it must
# stop the boot the same way. This one is refused AFTER the kernel is loaded, because that is where
# the image is read; what matters is that it is refused rather than mounted.
cp "$BUILD/efiboot.img" "$efi"
mcopy -i "$efi" ::/system-volume.img "$work/volume.img" || fail "the medium carries no system volume image"
printf '\x01' | dd of="$work/volume.img" bs=1 seek=$((1024 * 1024)) count=1 conv=notrunc status=none
mcopy -o -i "$efi" "$work/volume.img" ::/system-volume.img
payload_log="$work/payload.log"
boot_medium "$efi" "$payload_log"
if grep -aq "the live system volume is not what the boot medium's signed manifest records" "$payload_log"; then
	echo "signed-boot: the loader refused a live system volume the manifest does not describe"
else
	echo "signed-boot: an altered system volume image did NOT stop the boot" >&2
	sed -n '1,40p' "$payload_log" >&2
	exit 1
fi

# A SELECTED SOURCE THAT FAILS DOES NOT FALL BACK. With a system volume attached, that volume is the
# selected source - and a manifest inside it that no longer verifies must stop the boot rather than
# send the loader on to the medium sitting right there with a perfectly good one. "Try everything
# until something boots" is not a policy, and this is the boot that says so.
volume="$BUILD/system-volume-x86_64.img"
if [[ -f "$volume" ]]; then
	cp "$BUILD/efiboot.img" "$efi"
	cp "$volume" "$work/volume-disk.img"
	# The signed manifest inside a LiberFS image, found by its own magic rather than by a host tool
	# that can write the format - there is not one, and this needs to change a byte rather than a
	# file.
	at="$(grep -abo 'LBRMAN' "$work/volume-disk.img" | head -1 | cut -d: -f1)"
	[[ -n "$at" ]] || fail "the system volume carries no signed manifest"
	printf '\x01' | dd of="$work/volume-disk.img" bs=1 seek=$((at + 40)) count=1 conv=notrunc status=none
	vars="$work/vars.vol.fd"
	cp "$OVMF_VARS" "$vars"
	volume_log="$work/volume.log"
	timeout 120 qemu-system-x86_64 \
		-machine q35 -m 2G -display none -no-reboot \
		-drive "if=pflash,format=raw,readonly=on,file=$OVMF_CODE" \
		-drive "if=pflash,format=raw,file=$vars" \
		-drive "format=raw,file=$efi" \
		-drive "format=raw,file=$work/volume-disk.img,if=none,id=vol0" \
		-device virtio-blk-pci,drive=vol0 \
		-serial "file:$volume_log" >/dev/null 2>&1 || true
	if grep -aq "signature does not check out\|was refused" "$volume_log" && ! grep -aq "loader: kernel loaded" "$volume_log"; then
		echo "signed-boot: a selected volume whose manifest fails stops the boot rather than falling back to the medium"
	else
		echo "signed-boot: a selected volume with a broken manifest did NOT stop the boot" >&2
		sed -n '1,40p' "$volume_log" >&2
		exit 1
	fi
else
	echo "signed-boot: no system volume image to test the selected-source rule against" >&2
	exit 1
fi
exit 0
