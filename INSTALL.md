# LiberSystem - build and installation instructions

## Table of contents

- [**Prerequisites**](#prerequisites)
- [**Build**](#build)
- [**Run**](#run)
- [**Create bootable images**](#create-bootable-images)
- [**Test**](#test)
- [**Debugging**](#debugging)
- [**Development commands**](#development-commands)

## Prerequisites

LiberSystem is built with free, open-source tools. The toolchain currently targets **Linux** (Debian/Ubuntu) and builds a 64-bit (`x86_64`) kernel that boots in QEMU.

The kernel is a Rust `no_std` project. It is compiled with a nightly toolchain and `build-std`, booted with the [Limine](https://github.com/limine-bootloader/limine) bootloader, and run and tested under QEMU. All commands below are run through [`just`](https://github.com/casey/just) from the `src` directory.

### Automated setup (recommended)

The included setup script installs the entire toolchain. It is idempotent - safe to run repeatedly:

```sh
./setup.sh
```

It installs:

- system packages: `build-essential`, `git`, `curl`, `xorriso`, `gdisk`, `mtools`, `qemu-system-x86`, `qemu-utils`, `gdb`, `lld`, `llvm`, `clang`
- `rustup` with the **nightly** toolchain plus the `rust-src` and `llvm-tools-preview` components (required for `build-std` and the kernel build)
- `just`, the task runner
- the Limine bootloader (binary branch) into `~/.local/share/limine`

The project pins the nightly toolchain via `rust-toolchain.toml`, so no global toolchain switch is needed.

### Manual setup

If you prefer to install the tools yourself:

```sh
sudo apt install build-essential git curl xorriso gdisk mtools qemu-system-x86 qemu-utils gdb lld llvm clang
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain none
rustup toolchain install nightly --profile minimal --component rust-src --component llvm-tools-preview
cargo install just
```

Then clone the Limine `v11.x-binary` branch into `~/.local/share/limine` and run `make` in it (see `setup.sh` for the exact steps).

## Build

The kernel is built for the `x86_64-unknown-none` target. From the `src` directory:

```sh
just build
```

This compiles the kernel ELF into `kernel/target/x86_64-unknown-none/debug/kernel`. A plain build does not produce a disk image - the run step assembles a bootable ISO on demand, and you can build standalone images with [`just iso`](#create-bootable-images) and [`just img`](#create-bootable-images).

## Run

```sh
just run
```

This builds a bootable Limine ISO from the kernel ELF and launches QEMU headless, with the kernel's serial console wired to your terminal. You should see the boot log ending in `boot OK, halting`; the kernel then idle-spins. To quit QEMU, press <kbd>Ctrl</kbd>+<kbd>A</kbd> then <kbd>X</kbd>.

By default QEMU uses KVM (with `-cpu host`) when `/dev/kvm` is available, and four cores (`-smp 4`).

To capture the serial output to a file instead of the terminal (useful over SSH or in scripts):

```sh
SERIAL=file:boot.log just run
```

## Create bootable images

`just run` builds and boots a throwaway image automatically. To boot LiberSystem on real hardware - or to keep an image around - you can build standalone images explicitly. Both are written to `boot/.build/` and boot the same way under legacy BIOS or UEFI firmware.

### CD/DVD image (ISO)

```sh
just iso
```

Builds a hybrid BIOS+UEFI image at `boot/.build/libersystem.iso`. Burn it to a CD/DVD, or write it straight to a USB stick (the ISO is isohybrid, so it also boots from a flash drive):

```sh
sudo dd if=boot/.build/libersystem.iso of=/dev/sdX bs=4M conv=fsync status=progress
```

### Raw disk image (IMG)

```sh
just img        # default size 64M
just img 1G     # custom size (truncate-style suffixes: M, G, ...)
```

Builds a raw GPT disk image at `boot/.build/libersystem.img` for a USB stick, SD card or hard disk. It holds a small BIOS boot partition (carrying the Limine BIOS stage) plus an EFI System Partition with the kernel and bootloader, so it boots on both legacy BIOS and UEFI. Write it to a device with:

```sh
sudo dd if=boot/.build/libersystem.img of=/dev/sdX bs=4M conv=fsync status=progress
```

> Replace `/dev/sdX` with your target device (for example `/dev/sdb`). **Double-check the device name** - `dd` overwrites it without confirmation.

### Strip level

The kernel placed into an image is always stripped, because the debug info is never used at boot (Limine loads only the loadable segments, and the debugger reads symbols from the on-disk build). The amount stripped is selectable - it never affects booting, only the image size:

```sh
just iso          # STRIP=debug (default): drop DWARF, keep the symbol table
just iso all      # STRIP=all: also drop the symbol table (smallest image)
just img 128M all # same switch on the disk image (after the size)
```

## Test

LiberSystem ships an in-kernel test harness that runs under QEMU and reports the result through QEMU's `isa-debug-exit` device:

```sh
just test
```

A successful run prints each test with `[ok]` and exits zero.

## Debugging

Start QEMU so it waits for a debugger (a GDB stub on port `:1234`, with KVM disabled for reliable single-stepping):

```sh
just debug
```

Then, in a second terminal, attach GDB - it loads the kernel symbols and connects automatically:

```sh
just gdb
```

## Development commands

Run `just --list` to see every available command. The most useful ones:

| Command | Description |
| --- | --- |
| `just build` | Build the kernel. |
| `just run` | Build and boot in QEMU (headless, serial on stdout). |
| `just iso [strip]` | Build a hybrid BIOS+UEFI ISO into `boot/.build/` (`strip` = `debug` or `all`). |
| `just img [size] [strip]` | Build a raw GPT disk image (default `64M`) into `boot/.build/`. |
| `just test` | Run the in-kernel test harness in QEMU. |
| `just debug` | Boot in QEMU and wait for GDB on `:1234`. |
| `just gdb` | Attach GDB to a waiting QEMU instance. |
| `just fmt` | Format all code (Rust via `rustfmt`, shell via `shfmt`). |
| `just fmt-check` | Check formatting without writing changes (CI-friendly). |
| `just clean` | Remove build artifacts. |

> `just fmt` and `just fmt-check` additionally require [`shfmt`](https://github.com/mvdan/sh) on your `PATH`.
