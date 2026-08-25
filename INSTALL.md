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

`sbsigntool` and `python3-virt-firmware` are not build dependencies - nothing in `build.sh` uses
them. They belong to one verification gate, the Secure Boot profile, which signs the EFI loader with
a test certificate and enrols a test PK/KEK/db into a private OVMF variable store. Leaving them out
builds and boots the system exactly as before; the gate then reports which command it is missing
rather than skipping itself.

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

`build.sh` produces build artifacts, not bootable media. Building an image and booting it are
separate, explicit steps:

```sh
./build.sh    # 1. compile the system
./image.sh    # 2. assemble ISO, IMG and QCOW2
./run.sh      # 3. boot .build/boot/libersystem.iso
```

`image.sh` also runs the required x86_64 build itself, so the first command can be omitted when all
you need is fresh bootable media.

## Run

```sh
./run.sh                                        # boot .build/boot/libersystem.iso on x86_64
./run.sh --arch aarch64                         # a specific one (emulated if not the host's)
./run.sh --image path/to/another.iso            # boot a different existing x86_64 ISO
./run.sh --attach data.img                      # attach an extra disk or CD
```

`run.sh` compiles and assembles nothing. On x86_64, omitting `--image` boots
`.build/boot/libersystem.iso`; create it first with `./image.sh`. Explicit aarch64 and riscv64 runs
boot their architecture-specific build artifacts through a private per-run ESP. QEMU runs headless
with the serial console on your terminal, ending at a `vol://system>` prompt; `--serial file:boot.log`
redirects it. Guest size is `--smp N` and `--mem 4G`.

**Cores.** The guest gets the host's core count, capped at 8 on aarch64 and riscv64. aarch64 because QEMU's `virt` GICv2 addresses at most 8 CPU interfaces; riscv64 because U-Boot stops booting above roughly 50 harts - on a bigger host the guest produces no output at all while OpenSBI logs normally, which reads as a broken loader rather than as too many CPUs. Override with `--smp N`.

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

> The servers bind to `0.0.0.0` with no password. On a machine reachable from untrusted networks use `./run.sh --display vnc --vnc-addr 127.0.0.1:0` and tunnel with `ssh -L 5900:localhost:5900 user@HOST`. `--spice-port` sets the SPICE port.

### Screenshot

```sh
cd src
./lab.sh screenshot shot.png     # png, jpg, webp, gif, bmp, ppm by extension
```

Attaches to a live run and snaps the current frame; otherwise boots a throwaway instance, waits for the boot log and snaps that.

## Bootable images

```sh
./image.sh                               # ISO, IMG and QCOW2
./image.sh --format iso                  # Live CD
./image.sh --format img --size 1G        # installed system (default 128M)
./image.sh --format qcow2                # the same disk, stored sparsely
./image.sh --format iso --strip none     # development ISO with full kernel debug data
```

Written to `.build/boot/` as `libersystem.iso`, `libersystem.img` and `libersystem.qcow2`.

**ISO is a Live CD.** The medium carries a LiberFS system volume that the running system copies into memory at boot. Nothing is written back: no disk needed, and changes are gone when the session ends.

**IMG is an installed system.** Two partitions - an ESP with the loader and a recovery copy of the bootstrap programs, and a LiberFS system volume with the kernel and everything else. The loader finds the volume by its superblock rather than by device order, so it boots whatever else is attached.

Size suffixes are truncate's: `1G`, not `1GB`. Images use `--strip all` by default: DWARF and the
kernel symbol table are omitted from the medium, while the unstripped build artifact remains under
`.build/cargo/kernel/`. Use `--strip none` for a development image that should carry the complete
kernel, or `--strip debug` to drop DWARF but retain the symbol table. Stripping never affects booting,
only the diagnostics retained on the medium and its physical size.

```sh
sudo dd if=.build/boot/libersystem.img of=/dev/sdX bs=4M conv=fsync status=progress
```

> **Check the device name.** `dd` overwrites without confirmation.

## Test

**Start here.** `verify.sh` works out what a change needs verified - builds, host suites, gates, conformance runs and guest runs on the targets that can be affected - and runs exactly that:

```sh
./verify.sh                                  # for everything the working tree changed
./verify.sh --plan                           # print the plan, run nothing
./verify.sh --explain                        # ...and say why each item is in it
./verify.sh --for src/user/libs/audio/flac   # for a path instead of asking git
./verify.sh --sweep                          # every target, whole suite, one revision
```

A scoped verification of a codec change is about 2% of a full one, because it skips the two emulated targets rather than because it runs fewer tests. See [**Testing**](./docs/TESTING.md) for the model behind it and the measurements.

The pieces it calls can be run directly when you already know what you want:

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
cd src && ./lab.sh boot --fresh   # boot with a fresh data volume
cd src && ./lab.sh sh time ls     # run a command in the guest, get its output
cd src && ./lab.sh quit
```

See [docs/DEBUG.md](./docs/DEBUG.md) for the full toolbox. For a longer-lived instance you develop against - publish new binaries without rebooting, roll back, send keystrokes - use `./dev.sh <verb>` (`up`, `log`, `publish`, `rollback`, `key`, ...).

For kernel-level debugging, boot with a GDB stub on `:1234` and attach from a second terminal:

```sh
./run.sh --debug
./run.sh --gdb
```

## Command reference

Every script is at the repository root and answers `--help`.

| Command | Description |
| --- | --- |
| `./build.sh [--arch A] [--part P]` | Build the system or the parts you name. Anything after `--` goes to cargo. |
| `./run.sh [--arch A] [--image PATH] [--attach PATH] [--display D] [--smp N] [--mem S] [--serial SPEC] [--debug]` | Boot an existing ISO in QEMU on x86_64 (default `.build/boot/libersystem.iso`); builds nothing. |
| `./verify.sh [--for PATH] [--for-range A..B] [--plan] [--explain] [--sweep] [--release]` | Work out what a change needs verified, and run it. |
| `./test.sh [--arch A] [--tags T] [--fast] [--build-only] [--smp N] [--timeout S]` | Run the in-kernel test suites. |
| `./image.sh [--format F] [--size S] [--strip L]` | Build bootable images (`iso`, `img`, `qcow2`); omitting `--format` builds all three. |
| `./check.sh [--gate N] [--conformance F] [--refresh N] [--staged-image T] [--cache-check M] [--fast-path T]` | Build gates and image conformance suites; no arguments means all. `--refresh` regenerates what a gate checks instead of checking it. |
| `./clean.sh [--part P] [--dry-run]` | Remove build output (`cargo`, `boot`, `logs`). |
| `./dev.sh <verb> [args]` | Drive the persistent development guest. |
| `./format.sh [--changed]` | Format Rust and shell sources. |
| `./gen.sh [--check] [--accept-breaking]` | Regenerate the protocol bindings and the ABI manifests from `src/idl`. |
| `./lab.sh <subcommand> [args]` | Drive a live guest: boot it, run commands in it, screenshot it, measure it. |
| `./bench.sh [--suite N]` | Optimized host measurement (`audio`, `image`) and hostile-input runs (`image-mutate`). |

There is no task runner and nothing to install for one: every command above is a script in this
directory that takes flags and answers `--help`.

> `./format.sh` needs [`shfmt`](https://github.com/mvdan/sh) on your `PATH`.
