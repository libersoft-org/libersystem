#!/usr/bin/env bash
# Build the AArch64 UEFI firmware an AHCI system disk boots through: Debian's own AAVMF (edk2 2025.02, the
# no-secboot build of the version this host's QEMU ships beside) with the three edk2 drivers that put an
# AHCI disk behind Block I/O added - SataControllerDxe, AtaAtapiPassThruDxe and AtaBusDxe, which OVMF
# already carries and AAVMF does not. Without them the loader on aarch64 sees no disk on an AHCI controller,
# and `boot-volume-controllers` has no AHCI boot to prove there.
#
# Writes .build/firmware/AAVMF_CODE.ahci.fd and AAVMF_VARS.ahci.fd, padded to the 64 MiB a `virt` flash bank
# takes, and prints their digests; qemu-run.sh records the digest of the firmware every boot ran. The source
# is the Debian source package, fetched once beside them; iasl is unpacked from its Debian package rather
# than installed. Takes twenty to thirty minutes. NOTHING HERE IS STAGED INTO AN IMAGE: it is test firmware.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
out="$root/.build/firmware"
version="2025.02-8+deb13u1"
work="$out/edk2-ahci"
src="$work/edk2-2025.02"
mkdir -p "$work"
if [[ ! -d "$src" ]]; then
	(cd "$work" && apt-get source --download-only "edk2=$version" >fetch.log 2>&1 && dpkg-source -x "edk2_$version.dsc" edk2-2025.02 >extract.log 2>&1) || {
		echo "build-aavmf-ahci: the edk2 $version source package could not be fetched (see $work/fetch.log)" >&2
		exit 1
	}
fi
cd "$src"
grep -q SataControllerDxe ArmVirtPkg/ArmVirtQemu.dsc || sed -i '/MdeModulePkg\/Bus\/Pci\/NvmExpressDxe\/NvmExpressDxe.inf/a\  MdeModulePkg/Bus/Pci/SataControllerDxe/SataControllerDxe.inf\n  MdeModulePkg/Bus/Ata/AtaAtapiPassThru/AtaAtapiPassThru.inf\n  MdeModulePkg/Bus/Ata/AtaBusDxe/AtaBusDxe.inf' ArmVirtPkg/ArmVirtQemu.dsc
grep -q SataControllerDxe ArmVirtPkg/ArmVirtQemuFvMain.fdf.inc || sed -i '/INF MdeModulePkg\/Bus\/Pci\/NvmExpressDxe\/NvmExpressDxe.inf/a\  INF MdeModulePkg/Bus/Pci/SataControllerDxe/SataControllerDxe.inf\n  INF MdeModulePkg/Bus/Ata/AtaAtapiPassThru/AtaAtapiPassThru.inf\n  INF MdeModulePkg/Bus/Ata/AtaBusDxe/AtaBusDxe.inf' ArmVirtPkg/ArmVirtQemuFvMain.fdf.inc
# The LTO stub the GCC5 toolchain links against, assembled the way Debian's own build does.
[[ -f BaseTools/Bin/GccLto/liblto-aarch64.a ]] || aarch64-linux-gnu-gcc -c -fpic BaseTools/Bin/GccLto/liblto-aarch64.s -o BaseTools/Bin/GccLto/liblto-aarch64.a
export PYTHON_COMMAND=python3
make -C BaseTools/Source/C -j16 >"$work/basetools.log" 2>&1
set +u
# shellcheck disable=SC1091
. ./edksetup.sh >"$work/edksetup.log" 2>&1
set -u
export GCC5_AARCH64_PREFIX=aarch64-linux-gnu-
[[ -x "$out/acpica/root/usr/bin/iasl" ]] || {
	mkdir -p "$out/acpica"
	(cd "$out/acpica" && apt-get download acpica-tools >/dev/null 2>&1 && dpkg -x acpica-tools_*.deb root)
}
export PATH="$out/acpica/root/usr/bin:$PATH"
# Debian's no-secboot AAVMF build flags; the vendor and version strings are left at edk2's defaults.
build -a AARCH64 -t GCC5 -b RELEASE -n 32 -p ArmVirtPkg/ArmVirtQemu.dsc \
	-DCC_MEASUREMENT_ENABLE=TRUE -DNETWORK_HTTP_BOOT_ENABLE=TRUE -DNETWORK_IP6_ENABLE=TRUE -DNETWORK_TLS_ENABLE \
	--pcd PcdUninstallMemAttrProtocol=TRUE >"$work/build.log" 2>&1 || {
	echo "build-aavmf-ahci: the edk2 build failed (see $work/build.log)" >&2
	exit 1
}
fv="Build/ArmVirtQemu-AARCH64/RELEASE_GCC5/FV"
for driver in SataController AtaAtapiPassThruDxe AtaBusDxe; do
	compgen -G "$fv/Ffs/*$driver" >/dev/null || {
		echo "build-aavmf-ahci: $driver is not in the firmware volume" >&2
		exit 1
	}
done
cp "$fv/QEMU_EFI.fd" "$out/AAVMF_CODE.ahci.fd"
cp "$fv/QEMU_VARS.fd" "$out/AAVMF_VARS.ahci.fd"
truncate -s 64M "$out/AAVMF_CODE.ahci.fd" "$out/AAVMF_VARS.ahci.fd"
sha256sum "$out/AAVMF_CODE.ahci.fd" "$out/AAVMF_VARS.ahci.fd"
