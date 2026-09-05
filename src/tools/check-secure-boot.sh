#!/usr/bin/env bash
# The firmware verifies the loader, or the loader does not run.
#
# WHAT THIS GATE ADDS TO THE ONE BELOW IT. `signed-boot` proves the loader refuses a manifest that
# was altered; it says nothing about who verified the LOADER. A signed manifest checked by a loader
# nobody authenticated is one link of a chain reported as the whole of it - so this profile enrols a
# test platform key into a private OVMF variable store, signs the loader with the matching
# certificate, and requires the guest to say the firmware is enforcing. Then it takes the signature
# off and requires the loader not to run at all.
#
# x86_64 ONLY, deliberately: one reproducible chain rather than a claim about every port. The
# aarch64 and riscv64 loaders run the same MANIFEST verifier, and firmware Secure Boot on those two
# is not a gate here and must not be documented as one.
set -euo pipefail

cd "$(dirname "$0")/../.."
BUILD=".build/boot"
SECDIR=".build/secureboot"
LOADER=".build/cargo/loader/x86_64-unknown-uefi/debug/libersystem-loader.efi"
OVMF_SECBOOT="${OVMF_SECBOOT:-/usr/share/OVMF/OVMF_CODE_4M.secboot.fd}"
OVMF_VARS_TEMPLATE="${OVMF_VARS_SRC:-/usr/share/OVMF/OVMF_VARS_4M.fd}"
# A stable GUID for the test owner. Written out rather than generated, so two runs enrol the same
# owner and a variable store can be compared between them.
OWNER_GUID="6b7c9e4d-0f2a-4c31-9a5e-1d8f3b7c2a90"

fail() {
	echo "secure-boot: $*" >&2
	exit 1
}

# PREFLIGHTED BY NAME, AND NOTHING IS SKIPPED. A verification that quietly does not run when a tool
# is missing is the failure this milestone exists to prevent: it passes, and it proves nothing.
missing=0
for tool in openssl sbsign sbverify virt-fw-vars qemu-system-x86_64; do
	command -v "$tool" >/dev/null || {
		echo "secure-boot: $tool is required by this profile and is not installed" >&2
		missing=1
	}
done
((missing == 0)) || fail "install the missing tools (setup.sh lists them: sbsigntool, python3-virt-firmware)"
[[ -f "$OVMF_SECBOOT" ]] || fail "no Secure-Boot-capable OVMF at $OVMF_SECBOOT (the 'ovmf' package ships it)"
[[ -f "$OVMF_VARS_TEMPLATE" ]] || fail "no OVMF variable template at $OVMF_VARS_TEMPLATE"
[[ -f "$LOADER" ]] || fail "no loader at $LOADER - run ./build.sh --arch x86_64 --part loader"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
# signed-boot can rebuild this path with another trust profile. Acquire only while its
# producer lock is held; signing and every later medium read use this run's immutable copy.
mkdir -p .build/state
(
	flock 9
	cp "$LOADER" "$work/loader.efi"
) 9>.build/state/kernel-test-build.lock
LOADER="$work/loader.efi"

# THE TEST CERTIFICATE, GENERATED ONCE AND CACHED. It is a test key: it is generated here rather
# than committed, so nothing in this repository is a key-shaped file somebody could mistake for one
# that matters, and it never leaves `.build`.
mkdir -p "$SECDIR"
(
	flock 9
	if [[ ! -f "$SECDIR/test-pk.pem" || ! -f "$SECDIR/test-pk.key" ]]; then
		openssl req -new -x509 -newkey rsa:2048 -nodes -sha256 -days 3650 \
			-subj "/CN=LiberSystem test platform key/" \
			-keyout "$SECDIR/test-pk.key" -out "$SECDIR/test-pk.pem" >/dev/null 2>&1 ||
			fail "could not generate the test platform key"
		echo "secure-boot: generated a test platform key in $SECDIR"
	fi
) 9>"$SECDIR/key.lock"

# SIGNED AFTER ITS BYTES ARE FINAL, and checked back by a different tool than the one that signed.
signed="$work/loader-signed.efi"
sbsign --key "$SECDIR/test-pk.key" --cert "$SECDIR/test-pk.pem" --output "$signed" "$LOADER" >/dev/null 2>&1 ||
	fail "sbsign could not sign the loader"
sbverify --cert "$SECDIR/test-pk.pem" "$signed" >/dev/null 2>&1 ||
	fail "sbverify does not accept the signature sbsign just made - the two disagree about the same file"
echo "secure-boot: the loader is signed and the signature verifies independently"

# THE VARIABLE STORE IS A PRIVATE COPY. The distribution template is never written: a gate that
# enrols into it changes what every other boot on this machine trusts.
vars="$work/vars-enrolled.fd"
virt-fw-vars --input "$OVMF_VARS_TEMPLATE" --output "$vars" \
	--set-pk "$OWNER_GUID" "$SECDIR/test-pk.pem" \
	--add-kek "$OWNER_GUID" "$SECDIR/test-pk.pem" \
	--add-db "$OWNER_GUID" "$SECDIR/test-pk.pem" \
	--secure-boot >/dev/null 2>&1 || fail "virt-fw-vars could not enrol the test key"
echo "secure-boot: enrolled PK/KEK/db into a private variable store"

# One medium, with whichever loader it was given.
#
# THE ESP COMES OUT OF THE ISO, not out of `$BUILD/efiboot.img`. That path is a BY-PRODUCT every
# medium this tree assembles writes, so whichever `mkimage` ran last owns it - and a gate before this
# one that runs the test suite leaves the TEST medium there. This gate would then have proved its
# claim about a medium nobody ships, and said nothing about it. Same reasoning as `check-signed-boot`.
medium_with() {
	local loader="$1" out="$2"
	cp "$esp" "$out"
	mcopy -o -i "$out" "$loader" ::/EFI/BOOT/BOOTX64.EFI
}

boot() {
	local efi="$1" log="$2" verdict="$3" store="$work/vars.$$.fd"
	cp "$vars" "$store"
	src/tools/guest-verdict.py "$verdict" "$log" -- qemu-system-x86_64 \
		-machine q35,smm=on -m 2G -display none -no-reboot \
		-global driver=cfi.pflash01,property=secure,value=on \
		-drive "if=pflash,format=raw,unit=0,readonly=on,file=$OVMF_SECBOOT" \
		-drive "if=pflash,format=raw,unit=1,file=$store" \
		-drive "format=raw,file=$efi" \
		-serial "file:$log" || return 1
	rm -f "$store"
}

[[ -f "$BUILD/libersystem.iso" ]] || fail "no $BUILD/libersystem.iso - run ./image.sh --format iso"
esp="$work/esp.img"
xorriso -osirrox on -indev "$BUILD/libersystem.iso" -extract /boot/efiboot.img "$esp" >/dev/null 2>&1 || fail "could not read /boot/efiboot.img out of $BUILD/libersystem.iso"
[[ -s "$esp" ]] || fail "the ESP extracted from $BUILD/libersystem.iso is empty"
chmod u+w "$esp"

signed_medium="$work/signed.img"
medium_with "$signed" "$signed_medium"
signed_log="$work/signed.log"
boot "$signed_medium" "$signed_log" secure-signed

grep -aq "loader: firmware SecureBoot=1 SetupMode=0 (enforcing)" "$signed_log" || {
	echo "secure-boot: the guest did not report enforcing firmware" >&2
	grep -a "loader: firmware" "$signed_log" >&2 || echo "secure-boot: (the loader printed no firmware line at all)" >&2
	sed -n '1,30p' "$signed_log" >&2
	exit 1
}
echo "secure-boot: a signed loader runs and reports SecureBoot=1 SetupMode=0"

# AND THE UNSIGNED ONE MUST NOT RUN AT ALL. Not "must refuse" - must not reach its own first line,
# because the firmware is what refuses it. A gate that only booted the signed loader would pass
# identically on firmware that verifies nothing.
unsigned_medium="$work/unsigned.img"
medium_with "$LOADER" "$unsigned_medium"
unsigned_log="$work/unsigned.log"
boot "$unsigned_medium" "$unsigned_log" secure-unsigned

if grep -aq "loader: TEST TRUST\|loader: release trust" "$unsigned_log"; then
	echo "secure-boot: an UNSIGNED loader ran under enforcing firmware" >&2
	sed -n '1,30p' "$unsigned_log" >&2
	exit 1
fi
echo "secure-boot: an unsigned loader does not run under enforcing firmware"

# A BIT-MODIFIED SIGNED LOADER IS THE SAME REFUSAL, and it is worth its own boot: a signature that
# covers the file is not the same claim as a signature that was merely present.
altered="$work/altered.efi"
cp "$signed" "$altered"
size=$(stat -c%s "$altered")
printf '\x01' | dd of="$altered" bs=1 seek=$((size / 2)) count=1 conv=notrunc status=none
altered_medium="$work/altered.img"
medium_with "$altered" "$altered_medium"
altered_log="$work/altered.log"
boot "$altered_medium" "$altered_log" secure-altered-loader
if grep -aq "loader: TEST TRUST\|loader: release trust" "$altered_log"; then
	echo "secure-boot: a bit-modified signed loader ran under enforcing firmware" >&2
	sed -n '1,30p' "$altered_log" >&2
	exit 1
fi
echo "secure-boot: a bit-modified signed loader does not run either"

# AND THE TWO LINKS TOGETHER. A signed loader under enforcing firmware, carrying a manifest that was
# altered after signing: the firmware lets it run, and it refuses on its own. What must not happen is
# a kernel - the loader may say why it stopped and nothing after that.
both="$work/both.img"
medium_with "$signed" "$both"
mcopy -i "$both" ::/etc/boot.manifest2 "$work/manifest2" || fail "the medium carries no signed manifest"
printf '\x01' | dd of="$work/manifest2" bs=1 seek=40 count=1 conv=notrunc status=none
mcopy -o -i "$both" "$work/manifest2" ::/etc/boot.manifest2
both_log="$work/both.log"
boot "$both" "$both_log" secure-altered-manifest
if grep -aq "loader: kernel loaded" "$both_log"; then
	echo "secure-boot: a signed loader with an altered manifest still loaded a kernel" >&2
	sed -n '1,30p' "$both_log" >&2
	exit 1
fi
grep -aq "refusing to boot from it\|does not check out\|was refused" "$both_log" || {
	echo "secure-boot: a signed loader with an altered manifest neither refused nor loaded - it did something else" >&2
	sed -n '1,30p' "$both_log" >&2
	exit 1
}
echo "secure-boot: a signed loader under enforcing firmware refuses an altered manifest and loads no kernel"
