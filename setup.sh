#!/usr/bin/env bash
# Development environment setup (Debian/Ubuntu)
# Idempotent: safe to run repeatedly. Installs everything needed to build and
# debug the kernel in QEMU (x86_64 first).
#
# Usage:
#   ./setup.sh

set -euo pipefail

# colors
BOLD="\033[1m"
GREEN="\033[32m"
YELLOW="\033[33m"
RED="\033[31m"
RESET="\033[0m"
info() { echo -e "${GREEN}[*]${RESET} $*"; }
warn() { echo -e "${YELLOW}[!]${RESET} $*"; }
err() { echo -e "${RED}[x]${RESET} $*" >&2; }

if [[ "$(uname -s)" != "Linux" ]]; then
	err "This script is for Linux only."
	exit 1
fi

SUDO=""
if [[ "$(id -u)" -ne 0 ]]; then
	if command -v sudo >/dev/null 2>&1; then SUDO="sudo"; else
		err "Not root and sudo is unavailable."
		exit 1
	fi
fi

# apt packages
APT_PACKAGES=(
	build-essential # gcc, make, ...
	git
	curl
	jq                # parse Cargo's machine-readable executable artifact records
	xorriso           # ISO creation (UEFI)
	gdisk             # sgdisk: GPT partitioning for the disk image
	mtools            # mformat/mcopy: populate the FAT boot partition without root
	exfatprogs        # mkfs.exfat: the FAT fixture medium the aarch64 and riscv64 runners have no fallback for
	udftools          # mkfs.udf: build the UDF fixture required by the storage test topology
	netpbm            # pnmtopng/pnmtojpeg: convert QEMU framebuffer screendumps
	imagemagick       # convert: framebuffer screenshots to png/jpg/webp/...
	icoutils          # icotool: independent ICO conformance extraction
	icnsutils         # png2icns/icns2png: independent ICNS conformance
	libicns-dev       # libicns: legacy 128px ICNS fixture generation from Rust
	python3-pil       # Pillow: independent alpha-aware BMP conformance decoder
	pngcheck          # structural PNG/APNG CRC and chunk validation
	apngasm           # independent APNG fixture assembly
	apngdis           # independent APNG frame extraction
	gifsicle          # independent GIF structure/disposal validation
	webp              # libwebp cwebp/dwebp/webpmux/webpinfo/anim_dump conformance tools
	socat             # drive the QEMU monitor unix socket for screenshots
	qemu-system-x86   # qemu-system-x86_64 (the native x86_64 build)
	qemu-system-arm   # qemu-system-aarch64 (the ARM64 build, emulated on an x86 host)
	qemu-system-riscv # qemu-system-riscv64 (the RISC-V build, emulated on an x86 host)
	qemu-utils        # qemu-img
	ovmf              # UEFI firmware for QEMU x86_64 (the platform boots through UEFI)
	qemu-efi-aarch64  # AAVMF: UEFI firmware for QEMU ARM64 (the own aarch64 UEFI loader)
	u-boot-qemu       # U-Boot EFI firmware for QEMU RISC-V (the own riscv64 UEFI loader)
	gdb               # debugging via GDB stub
	lld               # LLVM linker (ld.lld)
	llvm              # llvm-objcopy and friends
	clang
	libssl-dev # OpenSSL headers required to build Taplo schema support

	# THESE THREE ARE NOT BUILD DEPENDENCIES. Nothing in `./build.sh` touches them: they belong to
	# ONE gate - the Secure Boot profile, which signs the EFI loader with a test
	# certificate and enrols a PK/KEK/db into a private OVMF variable store. They are installed here
	# so a developer's machine can run the whole check suite, and the gate still PREFLIGHTS each one
	# by name rather than assuming setup ran: a verification that skips itself when a tool is missing
	# is the failure that milestone exists to prevent.
	sbsigntool            # sbsign/sbverify: Authenticode-sign the loader and check the signature back
	python3-virt-firmware # virt-fw-vars: enrol the test PK/KEK/db into a per-run OVMF VARS copy
)

info "Updating apt and installing packages..."
# 'update' may fail because of third-party repositories (e.g. MariaDB GPG error) -
# we do not want to abort on that; packages install from the working sources.
$SUDO apt-get update -y || warn "apt-get update partially failed (third-party repo?), continuing."
$SUDO apt-get install -y "${APT_PACKAGES[@]}"

# rustup / Rust
if ! command -v rustup >/dev/null 2>&1; then
	info "Installing rustup..."
	curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain none
	# shellcheck disable=SC1091
	source "$HOME/.cargo/env"
else
	info "rustup is installed."
fi

# nightly + components (rust-src and llvm-tools are required for build-std and the kernel build)
info "Ensuring nightly toolchain + components (rust-src, llvm-tools-preview)..."
rustup toolchain install nightly --profile minimal --component rust-src --component llvm-tools-preview

# taplo (TOML formatter) - shared by CLI format gates and VS Code format-on-save
TAPLO_VERSION="0.10.0"
if ! command -v taplo >/dev/null 2>&1 || [[ "$(taplo --version)" != "taplo $TAPLO_VERSION" ]]; then
	info "Installing taplo $TAPLO_VERSION via cargo..."
	cargo install taplo-cli --version "$TAPLO_VERSION" --locked --force
else
	info "taplo $TAPLO_VERSION is installed."
fi

echo
info "${BOLD}Done.${RESET}"
echo "  - Rust nightly + rust-src + llvm-tools-preview"
echo "  - QEMU (x86_64 + aarch64 + riscv64), gdb, lld, xorriso, gdisk, mtools, exfatprogs, udftools, taplo"
echo
echo "Next step: ./build.sh"
echo "Note: the project selects nightly via rust-toolchain.toml, no global switch needed."
