#!/usr/bin/env bash
# The DMA mode on x86_64, booted: every row of the matrix this machine has, and every refusal the
# loader and the kernel owe.
#
# WHAT THIS GATE IS. The mode is a statement about the machine, carried to admission by exactly one
# producer per entry path, and the only proof that the producer chain works is a boot that reaches
# admission with the right value - and a boot that does NOT reach it when the chain is broken. So:
#
#   the development pair    the SAME tag-0 image booted on the default machine and under
#                            `--no-iommu`, through the loader's `harness` relay of the `fw_cfg`
#                            record. The first admits the driver that requires translation; the
#                            second refuses it by name and value and admits the trusted rows with
#                            visible degradation. The `fw_cfg` record is still readable at kernel
#                            entry, and the kernel says it revalidated the relay.
#   the shipping pair        the enforcing image on the default machine, the degraded image under
#                            `--no-iommu` - the signed field, with `signed` provenance.
#   the host-side pairing    `--no-iommu` on the enforcing image and the degraded image without the
#                            flag are refused by `run.sh` before QEMU starts, with both values named.
#   the refusals             a missing record, a malformed one, one claiming `signed` provenance, an
#                            ESP file beside `fw_cfg` (an independent producer), a `fw_cfg` record
#                            beside a signed set (a second producer), and a mixed selected set.
#
# The one case named in the plan that a boot cannot produce is a valid `fw_cfg` record that names
# a mode OTHER than the one the loader relayed: the loader and the kernel read the same static file,
# so the two cannot disagree on a real machine. That case is the codec's `InputDisagrees`, proved by
# `bootproto`'s host tests, and it is said here rather than faked.
#
# EACH BOOT IS ~40 s UNDER KVM, and there are eleven of them. The gate assembles the development
# image and the degraded image itself, and puts the tree back the way an enforcing `./image.sh`
# leaves it, so the gates that check the shipping image's freshness are not disturbed.
set -euo pipefail
# THE PHASE LOGS OUTLIVE THIS SCRIPT when a run is collecting evidence: copied into the run from the
# EXIT trap, before the directory is removed - on failure too, which is when they matter.
# shellcheck source=evidence.sh
source "$(dirname "${BASH_SOURCE[0]}")/evidence.sh"
# THIS IS A NAMED GATE, AND IT SAYS SO BEFORE INVOKING ANYTHING. The run mode is the one carrier of
# which matrix row a boot is on, set by the outermost entry point that knows and left alone by the
# runners it invokes.
export LIBER_RUN_MODE="${LIBER_RUN_MODE:-gate}"

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."
BUILD=".build/boot"

fail() {
	echo "dma-mode-x86_64: $*" >&2
	exit 1
}

command -v qemu-system-x86_64 >/dev/null || fail "qemu-system-x86_64 is not installed"
command -v xorriso >/dev/null || fail "xorriso is not installed"
command -v mcopy >/dev/null || fail "mtools is not installed"
work="$(mktemp -d)"
trap 'evidence_keep_gate "$work"/*.log "$work"/*/*.log; rm -rf "$work"' EXIT

# THE THREE IMAGES. The enforcing one keeps the plain name and is what every other gate checks; the
# gate assembles the degraded one and the development one, which rebuild the bootable volume in
# turn, and the enforcing one LAST so the tree ends where `./image.sh` leaves it.
echo "dma-mode-x86_64: assembling the development image (signed with no mode) and the degraded image"
./image.sh --format iso --dma-mode harness >"$work/image-dev.log" 2>&1 || {
	tail -20 "$work/image-dev.log" >&2
	fail "the development image could not be assembled"
}
./image.sh --format iso --dma-mode no-iommu >"$work/image-degraded.log" 2>&1 || {
	tail -20 "$work/image-degraded.log" >&2
	fail "the degraded image could not be assembled"
}
./image.sh --format iso --dma-mode enforcing-required >"$work/image-enforcing.log" 2>&1 || {
	tail -20 "$work/image-enforcing.log" >&2
	fail "the enforcing image could not be assembled"
}
DEV="$BUILD/libersystem-dev.iso"
DEGRADED="$BUILD/libersystem-no-iommu.iso"
ENFORCING="$BUILD/libersystem.iso"
for image in "$DEV" "$DEGRADED" "$ENFORCING"; do
	[[ -f "$image" ]] || fail "no $image after assembly"
done
# The signed field, read off each medium: the thing the whole gate turns on.
[[ "$(src/harness/image-dma-mode.sh "$DEV")" == harness ]] || fail "the development image is not signed with tag 0"
[[ "$(src/harness/image-dma-mode.sh "$DEGRADED")" == no-iommu ]] || fail "the degraded image's signed field does not read no-iommu"
[[ "$(src/harness/image-dma-mode.sh "$ENFORCING")" == enforcing-required ]] || fail "the enforcing image's signed field does not read enforcing-required"
echo "dma-mode-x86_64:   signed fields off the media: dev=harness degraded=no-iommu enforcing=enforcing-required"

boot() {
	# boot VERDICT LOG [env...] -- run.sh args
	local verdict="$1" log="$2"
	shift 2
	local -a env_pairs=()
	while [[ $# -gt 0 && "$1" != "--" ]]; do
		env_pairs+=("$1")
		shift
	done
	[[ "${1:-}" == "--" ]] && shift
	env "${env_pairs[@]}" src/tools/guest-verdict.py "$verdict" "$log" -- ./run.sh --smp 4 --serial "file:$log" "$@"
}

# 1. THE DEVELOPMENT PAIR: one image, two machines, the harness relay each time.
echo "dma-mode-x86_64: development row, default machine - the fw_cfg record relayed with harness provenance"
boot dma-admits "$work/dev-default.log" LIBER_RUN_MODE=development -- --image "$DEV"
grep -aq "dma: boot DMA mode enforcing-required (harness provenance, the loader's BootInfo, fw_cfg relay checked)" "$work/dev-default.log" || fail "the default development boot did not say it revalidated the fw_cfg relay"
grep -aq "loader: DMA mode enforcing-required (harness" "$work/dev-default.log" || fail "the loader did not report the harness hand-off"
echo "dma-mode-x86_64:   the loader relayed enforcing-required, the kernel revalidated the record still at fw_cfg, and virtio_net was admitted"

echo "dma-mode-x86_64: development row, --no-iommu - the same image, the degraded value"
boot dma-degraded "$work/dev-degraded.log" LIBER_RUN_MODE=development -- --image "$DEV" --no-iommu
grep -aq "under entry \`virtio_net\` REFUSED - the entry declares iommu-required and the boot mode is no-iommu" "$work/dev-degraded.log" || fail "virtio_net was not refused by name and value"
grep -aq "dma:   virtio-blk at" "$work/dev-degraded.log" || fail "the degraded inventory does not list the trusted block driver"
echo "dma-mode-x86_64:   virtio_net refused by name and value, the trusted rows admitted and listed"

# 2. THE SHIPPING PAIR: the signed field, `signed` provenance, no harness record anywhere.
echo "dma-mode-x86_64: public row, the enforcing image on the default machine"
boot dma-signed-admits "$work/signed-default.log" -- --image "$ENFORCING"
if grep -aq "harness provenance" "$work/signed-default.log"; then
	fail "the shipping image took a harness value"
fi
echo "dma-mode-x86_64:   the signed field admitted the machine, signed provenance end to end"

echo "dma-mode-x86_64: public row, the degraded image under --no-iommu"
boot dma-signed-degraded "$work/signed-degraded.log" -- --image "$DEGRADED" --no-iommu
echo "dma-mode-x86_64:   the degraded image boots the untranslated machine and admits only what the degraded row admits"

# 3. THE HOST-SIDE PAIRING CHECK: refused before QEMU starts, with both values named.
echo "dma-mode-x86_64: --no-iommu on the enforcing image is refused by the host"
if ./run.sh --image "$ENFORCING" --no-iommu >"$work/host-refuse-1.log" 2>&1; then
	fail "run.sh booted the enforcing image on a machine with no controller"
fi
grep -q "signed for DMA mode 'enforcing-required' and this machine is built for 'no-iommu'" "$work/host-refuse-1.log" || fail "the host refusal did not name both values: $(tail -3 "$work/host-refuse-1.log")"
if ./run.sh --image "$DEGRADED" >"$work/host-refuse-2.log" 2>&1; then
	fail "run.sh booted the degraded image on a machine with a controller"
fi
grep -q "signed for DMA mode 'no-iommu' and this machine is built for 'enforcing-required'" "$work/host-refuse-2.log" || fail "the host refusal did not name both values: $(tail -3 "$work/host-refuse-2.log")"
if grep -q "qemu-system" "$work/host-refuse-1.log" "$work/host-refuse-2.log"; then
	fail "the refusal started QEMU"
fi
echo "dma-mode-x86_64:   both mismatched pairings refused before QEMU, both values named"

# 4. THE REFUSALS ON THE TAG-0 IMAGE: the carrier missing, malformed, or claiming authentication.
for fixture in absent malformed signed-provenance; do
	echo "dma-mode-x86_64: development row with the fw_cfg record $fixture - the loader refuses"
	boot dma-loader-refused "$work/refuse-$fixture.log" LIBER_RUN_MODE=development "DMA_RECORD=$fixture" -- --image "$DEV"
	case "$fixture" in
	absent) grep -aq "harness carrier is absent" "$work/refuse-$fixture.log" || fail "the absent record was not named" ;;
	malformed) grep -aq "harness carrier is malformed (not eight bytes)" "$work/refuse-$fixture.log" || fail "the malformed record was not named" ;;
	signed-provenance) grep -aq "harness carrier is malformed (a provenance that is not \`harness\`" "$work/refuse-$fixture.log" || fail "the signed-provenance record was not refused as malformed" ;;
	esac
	echo "dma-mode-x86_64:   refused before the kernel started"
done

# 5. A SECOND PRODUCER BESIDE A SIGNED SET: the enforcing image with a fw_cfg record that AGREES.
echo "dma-mode-x86_64: the enforcing image with an agreeing fw_cfg record beside it - two producers"
boot dma-loader-refused "$work/second-producer.log" DMA_RECORD=ok -- --image "$ENFORCING"
grep -aq "AND a harness carrier is present - two producers" "$work/second-producer.log" || fail "the second producer was not named"
echo "dma-mode-x86_64:   refused although the values agree"

# 6. AN INDEPENDENT INPUT ON THIS PATH: an EFI/BOOT/LSDM file on the development image's ESP, beside
#    a valid fw_cfg record. The medium is re-assembled around the same signed ESP with one file added
#    - a file the manifest does not cover, because it is a harness input and not an artifact.
echo "dma-mode-x86_64: an EFI/BOOT/LSDM file on the medium beside fw_cfg - an independent producer"
xorriso -osirrox on -indev "$DEV" -extract /boot/efiboot.img "$work/esp.img" >/dev/null 2>&1 || fail "could not extract the development image's ESP"
python3 src/harness/dma-mode-record.py record enforcing-required >"$work/LSDM"
mcopy -i "$work/esp.img" "$work/LSDM" ::/EFI/BOOT/LSDM
mkdir -p "$work/iso_root/boot"
cp "$work/esp.img" "$work/iso_root/boot/efiboot.img"
xorriso -as mkisofs -quiet --efi-boot boot/efiboot.img -efi-boot-part --efi-boot-image --protective-msdos-label "$work/iso_root" -o "$work/dev-with-lsdm.iso" 2>/dev/null || fail "could not re-assemble the medium"
boot dma-loader-refused "$work/independent.log" LIBER_RUN_MODE=development -- --image "$work/dev-with-lsdm.iso"
grep -aq "an EFI/BOOT/LSDM file on the boot medium is present beside this entry path's DMA-mode input" "$work/independent.log" || fail "the independent ESP input was not named"
echo "dma-mode-x86_64:   refused, the independent input named"

# 7. A MIXED SELECTED SET: the development image's medium manifest re-signed with tag 1 around its
#    tag-0 volume. Both signatures check out; the set does not agree; the loader refuses at the latch.
echo "dma-mode-x86_64: a medium signed for a mode around a volume signed with none - a mixed set"
xorriso -osirrox on -indev "$DEV" -extract /boot/efiboot.img "$work/mixed.img" >/dev/null 2>&1 || fail "could not extract the development image's ESP"
mcopy -i "$work/mixed.img" ::/kernel "$work/mixed-kernel"
mcopy -i "$work/mixed.img" ::/system-volume.img "$work/mixed-volume.img"
mcopy -i "$work/mixed.img" ::/etc/boot.manifest2 "$work/mixed-manifest2"
inspect="$(cd src/tools/sign-manifest && cargo run --quiet -- --inspect "$work/mixed-manifest2")"
uuid="$(sed -n 's/^volume-uuid: //p' <<<"$inspect")"
release="$(sed -n 's/^release: //p' <<<"$inspect")"
[[ -n "$uuid" && -n "$release" ]] || fail "could not read the medium manifest's uuid and release"
(cd src/tools/sign-manifest && cargo run --quiet -- --profile test-trust --product LiberSystem --arch x86_64 --source boot-medium --release "$release" --volume-uuid "$uuid" --generation 1 --purpose boot --dma-mode enforcing-required --row "kernel:kernel=$work/mixed-kernel" --row "system-volume:system-volume.img=$work/mixed-volume.img" --out "$work/mixed-signed") >/dev/null || fail "could not re-sign the medium manifest with a mode"
mcopy -o -i "$work/mixed.img" "$work/mixed-signed" ::/etc/boot.manifest2
rm -rf "$work/iso_root"
mkdir -p "$work/iso_root/boot"
cp "$work/mixed.img" "$work/iso_root/boot/efiboot.img"
xorriso -as mkisofs -quiet --efi-boot boot/efiboot.img -efi-boot-part --efi-boot-image --protective-msdos-label "$work/iso_root" -o "$work/mixed.iso" 2>/dev/null || fail "could not re-assemble the mixed medium"
[[ "$(src/harness/image-dma-mode.sh "$work/mixed.iso")" == enforcing-required ]] || fail "the re-signed medium does not read enforcing-required"
boot dma-loader-refused "$work/mixed.log" -- --image "$work/mixed.iso"
grep -aq "do not agree on whether they carry a DMA mode" "$work/mixed.log" || fail "the mixed set was not refused at the latch: $(grep -a 'loader:' "$work/mixed.log" | tail -5)"
echo "dma-mode-x86_64:   refused at the latch, without admitting anything"

echo "dma-mode-x86_64: the development pair, the shipping pair, the host pairing check and every producible refusal hold on x86_64"
