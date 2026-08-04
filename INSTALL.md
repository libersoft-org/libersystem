# LiberSystem - build and installation

- [Prerequisites](#prerequisites)
- [Build](#build)
- [Run](#run)
- [Bootable images](#bootable-images)
- [Test](#test)
- [Debugging](#debugging)
- [Command reference](#command-reference)

## Prerequisites

Linux (Debian/Ubuntu) host. One portable Rust `no_std` kernel builds for **x86_64**, **aarch64** and **riscv64** and boots in QEMU on all three - natively with KVM on a matching host, emulated otherwise (riscv64 is always emulated). Every architecture boots the same way a real machine does: UEFI firmware runs the system's own loader, which reads the kernel and the bootstrap programs off the system volume.

As root:

```sh
apt update
apt -y install git
git clone https://github.com/libersoft-org/libersystem.git
cd libersystem && ./setup.sh
```

`setup.sh` is idempotent and installs everything: build tools and QEMU for all three architectures with their UEFI firmware (`ovmf`, `qemu-efi-aarch64`, `u-boot-qemu`), image tools (`xorriso`, `gdisk`, `mtools`, `udftools`), host-side conformance tools (`imagemagick`, `netpbm`, `icoutils`, `pngcheck`, `webp`, ...), `gdb`/`llvm`/`clang`, and `rustup` with the nightly toolchain plus `rust-src` and `llvm-tools-preview` for `build-std`. The toolchain is pinned by `rust-toolchain.toml`.

If you only build for x86_64, the foreign-architecture QEMU packages can be omitted.

## Build

From the repository root:

```sh
./build.sh                              # everything, x86_64
./build.sh --arch all                   # all three architectures
./build.sh --part kernel                # one part
./build.sh --part user -- -p imgconv    # anything after -- goes to cargo
```

Parts: `sdk`, `libs`, `user`, `kernel`, `loader`, `packages`, `volume`, `all`.

**The kernel does not contain the userspace.** The programs that run before a disk is readable are files on the system volume, listed in `etc/bootstrap.list`; the loader reads them and hands them over. One kernel binary, whatever userspace it is given.

Building produces no image. `./image.sh` assembles one, `./run.sh` boots one.

## Run

```sh
./run.sh                                        # the host's architecture
./run.sh --arch aarch64                         # a specific one (emulated if not the host's)
./run.sh --image .build/boot/libersystem.iso    # boot a medium you already built
./run.sh --attach data.img                      # attach an extra disk or CD
```

`run.sh` builds nothing - `build.sh` does that. QEMU runs headless with the serial console on your terminal, ending at a `vol://system>` prompt. `SERIAL=file:boot.log ./run.sh` redirects it.

**Cores.** The guest gets the host's core count, capped at 8 on aarch64 and riscv64. aarch64 because QEMU's `virt` GICv2 addresses at most 8 CPU interfaces; riscv64 because U-Boot stops booting above roughly 50 harts - on a bigger host the guest produces no output at all while OpenSBI logs normally, which reads as a broken loader rather than as too many CPUs. Override with `SMP=<n>`.

**Devices.** x86_64 and aarch64 attach the same set: `virtio-gpu`, keyboard/tablet, `virtio-sound`, `virtio-net`, `virtio-serial` and xHCI USB. riscv64 is serial-console only. On aarch64 the boot log is replayed onto the display once ConsoleService takes over, because the `virt` machine has no VGA framebuffer to draw it on directly.

### Networking

A `virtio-net` NIC on QEMU's user-mode network: the guest gets `10.0.2.15` by DHCP, the host is `10.0.2.2`, and outbound traffic works with no setup. The host's `127.0.0.1:5555` forwards to the guest's port 80, so `httpd &` in the guest answers `curl http://127.0.0.1:5555/`.

### Display and audio

```sh
./run.sh --display vnc         # VNC on :5900
./run.sh --display spice       # SPICE on :5930
./run.sh --display vnc,spice   # both
```

x86_64 and aarch64 only. The serial console keeps running alongside. Audio is routed through SPICE, so `beep [hz] [ms]` in the shell needs a SPICE display and a connected client (`remote-viewer spice://HOST:5930`); without one the device plays into a null sink.

> The servers bind to `0.0.0.0` with no password. On a machine reachable from untrusted networks use `VNC_ADDR=127.0.0.1:0 ./run.sh --display vnc` and tunnel with `ssh -L 5900:localhost:5900 user@HOST`. `SPICE_PORT` sets the SPICE port.

### Screenshot

```sh
cd src
just screenshot shot.png     # png, jpg, webp, gif, bmp, ppm by extension
```

Attaches to a live run and snaps the current frame; otherwise boots a throwaway instance, waits for the boot log and snaps that.

## Bootable images

```sh
./image.sh --format iso                  # LiveCD
./image.sh --format img --size 1G        # installed system (default 128M)
./image.sh --format qcow2                # the same disk, stored sparsely
./image.sh --format iso --strip all      # smaller: drop the symbol table too
```

Written to `.build/boot/`, bootable on any UEFI machine.

**ISO is a LiveCD.** The medium carries a LiberFS system volume that the running system copies into memory at boot. Nothing is written back: no disk needed, and changes are gone when the session ends.

**IMG is an installed system.** Two partitions - an ESP with the loader and a recovery copy of the bootstrap programs, and a LiberFS system volume with the kernel and everything else. The loader finds the volume by its superblock rather than by device order, so it boots whatever else is attached.

Size suffixes are truncate's: `1G`, not `1GB`. Stripping never affects booting, only image size.

```sh
sudo dd if=.build/boot/libersystem.img of=/dev/sdX bs=4M conv=fsync status=progress
```

> **Check the device name.** `dd` overwrites without confirmation.

## Test

```sh
./test.sh                                    # x86_64, every test
./test.sh --arch all                         # all three, in turn
./test.sh --arch riscv64 --tags filesystem   # selected tags (--list-tags to see them)
./test.sh --fast                             # reuse a verified userspace preflight
./test.sh --build-only                       # compile without booting
```

The suite runs **inside** a booted kernel and reports through `isa-debug-exit` (x86_64) or semihosting (aarch64, riscv64). `test.sh` builds nothing; run `./build.sh` first.

Host-side gates that inspect artifacts without booting anything are separate:

```sh
./check.sh                                   # every gate and conformance suite
./check.sh --gate volume-layout
./check.sh --conformance png,webp
```

## Debugging

```sh
cd src && just lab boot --fresh   # boot with a fresh data volume
cd src && just lab sh time ls     # run a command in the guest, get its output
cd src && just lab quit
```

See [docs/DEBUG.md](./docs/DEBUG.md) for the full toolbox. For a longer-lived instance you develop against - publish new binaries without rebooting, roll back, send keystrokes - use `./dev.sh <verb>` (`up`, `log`, `publish`, `rollback`, `key`, ...).

For kernel-level debugging, boot with a GDB stub on `:1234` and attach from a second terminal:

```sh
cd src && just debug
cd src && just gdb
```

## Command reference

Every script is at the repository root and answers `--help`.

| Command | Description |
| --- | --- |
| `./build.sh [--arch A] [--part P]` | Build the system or the parts you name. Anything after `--` goes to cargo. |
| `./run.sh [--arch A] [--image PATH] [--attach PATH] [--display D] [--debug]` | Boot in QEMU. Builds nothing. |
| `./test.sh [--arch A] [--tags T] [--fast] [--build-only]` | Run the in-kernel test suites. |
| `./image.sh [--format F] [--size S] [--strip L]` | Build bootable images (`iso`, `img`, `qcow2`). |
| `./check.sh [--gate N] [--conformance F]` | Build gates and image conformance suites; no arguments means all. |
| `./clean.sh [--part P] [--dry-run]` | Remove build output (`cargo`, `boot`, `logs`). |
| `./dev.sh <verb> [args]` | Drive the persistent development guest. |
| `./format.sh [--changed]` | Format Rust and shell sources. |

A Justfile in `src/` keeps the specialist recipes: the IDL generator (`gen`, `gen-check`), host-side test crates (`fs-host-test`, `services-host-test`, `proto-test`), benchmarks (`audio-bench`, `image-bench`, `perf-gate`), the two loader builds whose bodies differ per architecture, source hygiene checks, and the debugging entry points above. `cd src && just --list` shows them.

> `./format.sh` needs [`shfmt`](https://github.com/mvdan/sh) on your `PATH`.
