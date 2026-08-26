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

# THE ESP THIS GATE TESTS COMES OUT OF THE ISO IT NAMES.
#
# It used to be `$BUILD/efiboot.img`, which is a BY-PRODUCT: every medium this tree assembles writes
# that one path, so whichever `mkimage` ran last owns it. Any gate before this one that runs the test
# suite leaves the TEST medium there - which carries the factory archive and no system volume - and
# this gate then either fails on a file it never meant to read (`::/system-volume.img not found`) or,
# worse, passes having proved the refusal on a medium nobody ships. The gate already requires the
# shipping ISO two lines up; this reads the ESP out of THAT.
esp="$work/esp.img"
xorriso -osirrox on -indev "$ISO" -extract /boot/efiboot.img "$esp" >/dev/null 2>&1 || fail "could not read /boot/efiboot.img out of $ISO"
[[ -s "$esp" ]] || fail "the ESP extracted from $ISO is empty"
chmod u+w "$esp"

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
cp "$esp" "$efi"
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

cp "$esp" "$efi"

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
cp "$esp" "$efi"
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
# selected source - and a signed manifest inside it that cannot be READ must stop the boot rather
# than send the loader on to the text manifest sitting beside it. Damaging one file is how a
# downgrade is performed without forging anything, and this is the boot that refuses it.
volume="$BUILD/system-volume-x86_64.img"
if [[ -f "$volume" ]]; then
	# THE MEDIUM AND THE VOLUME HAVE TO HAVE BEEN BUILT TOGETHER, and this says so before the case
	# rather than after it.
	#
	# The loader selects a system volume by the uuid the medium names in `etc/system-volume.uuid`. If
	# the two were built in the wrong order - `./image.sh` before `./build.sh --part volume`, which
	# regenerates the volume AND its uuid - the medium names a volume that is not there, the loader
	# refuses it as unpaired, and the case below never reaches the volume's manifest at all. It then
	# reported "a damaged signed manifest on the selected volume did NOT stop the boot", which is a
	# sentence about the trust chain and was a sentence about the build order. Three diagnosis cycles
	# in one round went to that.
	paired="$(mcopy -i "$esp" ::/etc/system-volume.uuid - 2>/dev/null | tr -d '\r\n')"
	actual="$(dd if="$volume" bs=1 skip=80 count=16 status=none | od -An -tx1 | tr -d ' \n')"
	if [[ -z "$paired" ]]; then
		fail "the medium names no system volume, so the selected-source rule below has no selected source"
	fi
	if [[ "$paired" != "$actual" ]]; then
		echo "signed-boot: the medium is paired with volume $paired and the volume beside it is $actual" >&2
		echo "signed-boot:   they were not built together - the loader will refuse this volume as unpaired, and no case below would be testing what it says" >&2
		echo "signed-boot:   rebuild in order:  ./build.sh --arch x86_64 --part volume && ./image.sh --format iso" >&2
		exit 1
	fi
	cp "$esp" "$efi"
	cp "$volume" "$work/volume-disk.img"
	# The signed manifest inside a LiberFS image, found by its own magic: there is no host tool that
	# writes this format, and this needs to change a byte rather than a file.
	at="$(grep -abo -m 1 'LBRMAN' "$work/volume-disk.img" | cut -d: -f1)"
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
	if grep -aq "cannot be read - refusing to boot from it rather than falling back\|does not check out\|was refused" "$volume_log"; then
		echo "signed-boot: a selected volume whose signed manifest is damaged stops the boot instead of falling back to the text one"
	else
		echo "signed-boot: a damaged signed manifest on the selected volume did NOT stop the boot" >&2
		sed -n '1,40p' "$volume_log" >&2
		exit 1
	fi
else
	echo "signed-boot: no system volume image to test the selected-source rule against" >&2
	exit 1
fi

# THE CONTEXT THE SIGNATURE COVERS, AND WHAT HAPPENS WHEN IT DOES NOT DESCRIBE THIS MACHINE.
#
# The manifest format signs the product, the architecture, the kind of source and the volume's
# identity, and every one of those was decoded and then ignored - so what the check asserted was "a
# key this loader carries signed this manifest" while what it was read as asserting was "…and this
# manifest was written for THIS machine". A correctly signed manifest for another product, another
# port or another medium passed, with no signature broken and no key needed.
#
# Each case below is signed WITH THE REAL TEST KEY over the real rows, and differs from the medium's
# own manifest in exactly one header field. So a boot that accepts one is not accepting a forgery: it
# is accepting a valid release that was not made for it, which is the whole point.
signer="$PWD/src/tools/sign-manifest"
# The rows the medium's own manifest carries, rebuilt from the files on the ESP: the point is a
# manifest that is right about the CONTENT and wrong about the CONTEXT.
extract_from_esp() {
	local path="$1" out="$2"
	mcopy -i "$efi" "::$path" "$out" 2>/dev/null
}

resign_with() {
	# Re-sign the medium's manifest rows with one header field changed. Every argument after the
	# output path is passed through to the signer.
	local out="$1"
	shift
	local rows=()
	local kernel="$work/kernel.bin"
	extract_from_esp "/kernel" "$kernel" || return 1
	rows+=(--row "kernel:kernel=$kernel")
	(cd "$signer" && cargo run --quiet -- --profile test-trust --product LiberSystem \
		--arch x86_64 --source boot-medium --release 0.0.1 \
		--volume-uuid 00000000000000000000000000000000 "$@" "${rows[@]}" --out "$out") >/dev/null 2>&1
}

wrong_context() {
	local what="$1"
	shift
	local manifest="$work/wrong.manifest2"
	if ! resign_with "$manifest" "$@"; then
		echo "signed-boot: could not sign the $what case" >&2
		return 1
	fi
	cp "$esp" "$efi"
	mcopy -o -i "$efi" "$manifest" ::/etc/boot.manifest2
	local log="$work/wrong.log"
	boot_medium "$efi" "$log"
	if grep -aq "loader: kernel loaded" "$log"; then
		echo "signed-boot: a manifest signed for $what was ACCEPTED - the signature is checked and what it says is not" >&2
		grep -a -m 6 "loader:" "$log" >&2
		return 1
	fi
	grep -aq "refusing to boot from it" "$log" || {
		echo "signed-boot: the $what case neither loaded a kernel nor refused - it did something else" >&2
		grep -a -m 8 "loader:" "$log" >&2
		return 1
	}
	echo "signed-boot: a manifest signed for $what is refused"
}

if [[ -n "$(command -v mcopy)" ]]; then
	status=0
	wrong_context "another product" --product NotLiberSystem || status=1
	wrong_context "another architecture" --arch aarch64 || status=1
	wrong_context "another kind of source" --source system-volume || status=1
	wrong_context "a different volume" --source system-volume --volume-uuid 0123456789abcdef0123456789abcdef || status=1
	((status == 0)) || exit 1
fi

# AND TWO VALID RELEASES DO NOT COMPOSE INTO A SYSTEM.
#
# The release case is not like the four above and testing it the same way was wrong: a lone manifest
# naming another release is not a violation - nothing in the loader knows which release it is until
# something tells it, and the first thing verified is what tells it. The rule is that the SECOND
# source has to agree with the first. Each manifest was verified on its own and none was compared
# with any other, so a kernel from one signed release and a bootstrap set from another - each
# perfectly valid, each signed by the same key - composed into a system nobody ever built or tested.
#
# So: the volume keeps its release and is the selected source, and the medium's manifest is re-signed
# for another one. The volume's kernel is verified first and fixes which release this boot is; the
# medium is then a different one.
if [[ -f "$volume" ]]; then
	mixed="$work/mixed.manifest2"
	if resign_with "$mixed" --release 9.9.9; then
		cp "$esp" "$efi"
		mcopy -o -i "$efi" "$mixed" ::/etc/boot.manifest2
		cp "$volume" "$work/volume-mixed.img"
		vars="$work/vars.mixed.fd"
		cp "$OVMF_VARS" "$vars"
		mixed_log="$work/mixed.log"
		timeout 120 qemu-system-x86_64 \
			-machine q35 -m 2G -display none -no-reboot \
			-drive "if=pflash,format=raw,readonly=on,file=$OVMF_CODE" \
			-drive "if=pflash,format=raw,file=$vars" \
			-drive "format=raw,file=$efi" \
			-drive "format=raw,file=$work/volume-mixed.img,if=none,id=vol0" \
			-device virtio-blk-pci,drive=vol0 \
			-serial "file:$mixed_log" >/dev/null 2>&1 || true
		if grep -aq "belongs to a different release than the one already verified in this boot" "$mixed_log"; then
			echo "signed-boot: two sources from two signed releases do not compose into a system"
		else
			echo "signed-boot: a boot took its kernel from one signed release and its bootstrap set from another" >&2
			grep -a -m 10 "loader:" "$mixed_log" >&2
			exit 1
		fi
	else
		fail "could not sign the mixed-release case"
	fi
fi

# AND THE DOWNGRADE THAT NEEDED NO FORGERY AT ALL: DELETE ONE FILE.
#
# A signed manifest that was ABSENT dropped the source to `etc/boot.manifest`, which is a checksum
# list an attacker recomputes along with the payload. So the whole authenticity claim could be removed
# by removing a file - no signature broken, no key needed, nothing to detect. A present-but-damaged
# manifest was already refused correctly, which is what made the hole easy to miss: the case that was
# tested was the harder one.
#
# WHETHER A BUILD TAKES THAT IS A PROFILE, and this proves the two profiles differ on exactly it. The
# same medium, with the same file removed, booted by two loaders: the release one refuses and names
# what it refused; the test-trust one boots and says the boot is not authenticated.
LOADER_DIR="$PWD/src/boot/loader"
RELEASE_KEY="d04ab232742bb4ab3a1368bd4615e4e6d0224ab71a016baf8520a332c9778737"
LOADER_OUT=".build/cargo/loader/x86_64-unknown-uefi/debug/libersystem-loader.efi"

cp "$esp" "$efi"
mdel -i "$efi" ::/etc/boot.manifest2 2>/dev/null || fail "the medium carries no signed manifest to remove"

# The test-trust loader, which may take the downgrade and must say so.
(cd "$LOADER_DIR" && env LIBER_TRUST_PROFILE=test-trust cargo build --quiet) || fail "the test-trust loader did not build"
mcopy -o -i "$efi" "$LOADER_OUT" ::/EFI/BOOT/BOOTX64.EFI
downgrade_log="$work/downgrade-test-trust.log"
boot_medium "$efi" "$downgrade_log"
grep -aq "THIS KERNEL IS NOT AUTHENTICATED" "$downgrade_log" || {
	echo "signed-boot: the test-trust loader took the unsigned fallback without saying the boot is not authenticated" >&2
	grep -a -m 8 "loader:" "$downgrade_log" >&2
	exit 1
}
echo "signed-boot: with the signed manifest removed, a test-trust build boots and SAYS the kernel is not authenticated"

# The release loader, which must not.
(cd "$LOADER_DIR" && env LIBER_TRUST_PROFILE=external-release LIBER_TRUST_KEY="$RELEASE_KEY" LIBER_TRUST_KEY_ID=42 cargo build --quiet) || fail "the release loader did not build"
mcopy -o -i "$efi" "$LOADER_OUT" ::/EFI/BOOT/BOOTX64.EFI
release_log="$work/downgrade-release.log"
boot_medium "$efi" "$release_log"
if grep -aq "loader: kernel loaded" "$release_log"; then
	echo "signed-boot: a RELEASE build took the unsigned fallback - deleting one file removes the whole authenticity claim" >&2
	grep -a -m 8 "loader:" "$release_log" >&2
	exit 1
fi
grep -aq "carries no SIGNED manifest, and this build authenticates what it boots" "$release_log" || {
	echo "signed-boot: the release build refused for some other reason than the missing signed manifest" >&2
	grep -a -m 8 "loader:" "$release_log" >&2
	exit 1
}
echo "signed-boot: and a release build refuses it, naming the missing signed manifest"

# THE TREE IS LEFT WITH THE LOADER IT HAD. A gate that leaves a release-profile binary in the build
# directory hands the next `./image.sh` a loader carrying a key nothing in this tree can sign with.
(cd "$LOADER_DIR" && env LIBER_TRUST_PROFILE=test-trust cargo build --quiet) || fail "the test-trust loader did not rebuild"

# AND THE OTHER TWO PORTS. The Definition of done names three architectures for the MANIFEST claim
# and one for the firmware one, and this gate collapsed both into "x86_64 only, and by design" - so
# the manifest verifier, which is the same code on all three, was proved on one.
#
# These have no shipping ISO: their medium is the ESP the runner assembles, so the tamper happens
# there. Emulated, so this is minutes rather than seconds - and it is the reason this phase is last.
if [[ "${SIGNED_BOOT_PORTS:-1}" == "1" ]]; then
	for port in aarch64 riscv64; do
		kernel=".build/cargo/kernel/$(case $port in aarch64) echo aarch64-unknown-none ;; riscv64) echo riscv64gc-unknown-none-elf ;; esac)/debug/kernel"
		if [[ ! -f "$kernel" ]]; then
			echo "signed-boot: no $port kernel - run ./build.sh --arch $port" >&2
			exit 1
		fi
		clean_log="$work/$port-clean.log"
		echo "signed-boot: booting $port through its own UEFI loader"
		# BOUNDED BY WHAT THE BOOT NEEDS, NOT BY PATIENCE. `run.sh` boots into a shell and never
		# exits, so the timeout is what ends it - and every second of it is spent after the line this
		# gate reads. Emulated, these reach the loader's verdict inside a minute; four boots at the
		# fifteen this started with made one gate an hour long.
		UEFI=1 SERIAL="file:$clean_log" timeout 300 ./run.sh --arch "$port" --smp 1 >/dev/null 2>&1 || true
		grep -aq "loader: kernel loaded" "$clean_log" || {
			echo "signed-boot: the unaltered $port medium did not load a kernel - every case below would be meaningless" >&2
			grep -a -m 10 "loader:" "$clean_log" >&2
			exit 1
		}
		echo "signed-boot:   $port: the unaltered medium loads a kernel"

		tampered_log="$work/$port-tampered.log"
		DAMAGE_SIGNED_MANIFEST=1 UEFI=1 SERIAL="file:$tampered_log" timeout 300 ./run.sh --arch "$port" --smp 1 >/dev/null 2>&1 || true
		if grep -aq "loader: kernel loaded" "$tampered_log"; then
			echo "signed-boot: $port loaded a kernel from a medium whose signed manifest was altered" >&2
			grep -a -m 10 "loader:" "$tampered_log" >&2
			exit 1
		fi
		grep -aq "does not check out\|was refused\|refusing to boot from it" "$tampered_log" || {
			echo "signed-boot: $port neither refused the altered manifest nor loaded a kernel - it did something else" >&2
			grep -a -m 10 "loader:" "$tampered_log" >&2
			exit 1
		}
		echo "signed-boot:   $port: an altered signed manifest is refused and no kernel is loaded"
	done
fi

echo "signed-boot: the signature is checked, and so is every fact it covers"
