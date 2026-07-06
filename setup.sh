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
	xorriso         # ISO creation (UEFI)
	gdisk           # sgdisk: GPT partitioning for the disk image
	mtools          # mformat/mcopy: populate the FAT boot partition without root
	netpbm          # pnmtopng/pnmtojpeg: convert QEMU framebuffer screendumps
	imagemagick     # convert: framebuffer screenshots to png/jpg/webp/...
	socat           # drive the QEMU monitor unix socket for screenshots
	qemu-system-x86 # qemu-system-x86_64
	qemu-utils      # qemu-img
	ovmf            # UEFI firmware for QEMU (the platform boots through UEFI)
	gdb             # debugging via GDB stub
	lld             # LLVM linker (ld.lld)
	llvm            # llvm-objcopy and friends
	clang
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

# just (task runner) - via cargo, since it is not packaged in apt on Debian 12
if ! command -v just >/dev/null 2>&1; then
	info "Installing 'just' via cargo..."
	cargo install just
else
	info "'just' is installed."
fi

echo
info "${BOLD}Done.${RESET}"
echo "  - Rust nightly + rust-src + llvm-tools-preview"
echo "  - QEMU (x86_64), gdb, lld, xorriso, gdisk, mtools, just"
echo
echo "Next step: cd src/kernel && cargo build"
echo "Note: the project selects nightly via rust-toolchain.toml, no global switch needed."
