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

LiberSystem is built with free, open-source tools. The toolchain currently targets **Linux** (Debian/Ubuntu). One portable kernel builds for three architectures - **`x86_64`**, **`aarch64` (ARM64)** and **`riscv64` (RISC-V)** - and boots in QEMU on all three. On an x86_64 host the x86_64 build runs natively (with KVM) and the ARM64 and RISC-V builds run emulated, and vice versa on an ARM64 host; the RISC-V build is always emulated (there is no RISC-V host path here).

The kernel is a Rust `no_std` project. It is compiled with a nightly toolchain and `build-std`, booted by the system's own UEFI loader through **UEFI** (QEMU runs with the OVMF firmware; the `ovmf` package is required), and run and tested under QEMU. All commands below are run through [`just`](https://github.com/casey/just) from the `src` directory.

Download the latest version of this software and install required tools.

**On Linux (Debian / Ubuntu):**

Log in as **root** and then run in terminal:

The included setup script installs the entire toolchain. It is idempotent - safe to run repeatedly:

```sh
apt update
apt -y upgrade
apt -y install git
git clone https://github.com/libersoft-org/libersystem.git
./setup.sh
cd src
```

This will install:

- system packages: `build-essential`, `git`, `curl`, `xorriso`, `gdisk`, `mtools`, `udftools`, `netpbm`, `imagemagick`, `icoutils`, `icnsutils`, `libicns-dev`, `python3-pil`, `pngcheck`, `apngasm`, `apngdis`, `gifsicle`, `webp`, `socat`, `qemu-system-x86`, `qemu-system-arm`, `qemu-system-riscv`, `qemu-utils`, `ovmf`, `qemu-efi-aarch64`, `u-boot-qemu`, `gdb`, `lld`, `llvm`, `clang` (`udftools` supplies `mkfs.udf` for the UDF storage fixture; `icoutils`, `icnsutils`, `libicns-dev`, `python3-pil`, `pngcheck`, `apngasm`, `apngdis`, `gifsicle` and `webp` are host-only conformance tools; `qemu-system-arm` + `qemu-efi-aarch64` are the ARM64 emulator and its UEFI firmware, `qemu-system-riscv` + `u-boot-qemu` the RISC-V emulator and its U-Boot UEFI firmware; omit the foreign-architecture packages if you only build for x86_64)
- `rustup` with the **nightly** toolchain plus the `rust-src` and `llvm-tools-preview` components (required for `build-std` and the kernel build)
- `just`, the task runner

The project pins the nightly toolchain via `rust-toolchain.toml`, so no global toolchain switch is needed.

## Build

The kernel is built for the `x86_64-unknown-none` target. From the `src` directory:

```sh
./build.sh
```

This builds the userspace programs (the services, drivers and command-line tools), the SDK component, the kernel ELF into `.build/cargo/kernel/x86_64-unknown-none/debug/kernel`, the system's own UEFI loader, and the LiberFS **system volume** the loader reads all of it from.

The kernel does not contain the userspace. It used to - the programs that run before a disk is readable were compiled into the kernel binary, which made building the kernel require a built userspace and put those programs somewhere the user could not look. They are files on the system volume now, listed in `etc/bootstrap.list`, and the loader reads them and hands them over. One kernel binary, whatever userspace it is given.

A plain build does not produce a disk image - the run step assembles one on demand, and you can build standalone images with [`./image.sh`](#create-bootable-images).

## Run

```sh
./run.sh
```

`./run.sh` builds and boots the **host's native architecture** (the `x86_64` build on an x86_64 host, the `aarch64` build on an ARM64 host). It launches QEMU headless, with the system's serial console wired to your terminal. The boot log reports each service coming online and ends at an interactive shell prompt:

```
vol://system>
```

To capture the serial output to a file instead of the terminal (useful over SSH or in scripts):

```sh
SERIAL=file:boot.log ./run.sh
```

QEMU uses KVM (with `-cpu host`) when `/dev/kvm` is available, and gives the guest as many cores as the host has (`nproc`); override the count with `SMP=<n> ./run.sh`.

### Running a specific architecture

`./run.sh` picks the host's architecture; to force a particular one - it runs emulated when it is not the host's - name it:

```sh
./run.sh --arch x86_64    # the x86_64 build (native with KVM on an x86_64 host)
./run.sh --arch aarch64   # the ARM64 build, booted through the system's own UEFI loader (AAVMF)
./run.sh --arch riscv64   # the RISC-V build, booted through the system's own UEFI loader (U-Boot)
```

They all reach the same interactive shell, and all three take the **same path a real machine takes**: firmware runs the system's own loader (`BOOTX64.EFI`, `BOOTAA64.EFI`, `BOOTRISCV64.EFI`), the loader reads the kernel and the bootstrap programs off the system volume, and hands off. There used to be a second, faster way in for ARM64 and RISC-V - QEMU's direct `-kernel` load, with the userspace passed as one packaged blob - and separate `run-aarch64-uefi` / `run-riscv64-uefi` recipes for the firmware path. That blob was retired with the packaged bootstrap archive it carried, so there is one way in now and the `-uefi` recipes are gone. The ARM64 build is emulated on an x86_64 host (no KVM), so it boots more slowly than the native run. The ARM64 runs attach the **same device set as x86_64** - `virtio-gpu` (the graphical display), `virtio-keyboard` / `virtio-tablet` input, `virtio-sound`, `virtio-net`, `virtio-serial` and the xHCI USB stack - so the `vnc` / `spice` displays below work identically. The one difference is the boot log: QEMU's `virt` machine has no VGA framebuffer, so the kernel does not draw the boot log pixel-by-pixel as on x86_64; instead the log is replayed as text onto the virtio-gpu display once ConsoleService takes over, so it still appears on screen.

The RISC-V build is always emulated. QEMU runs the S-mode U-Boot on OpenSBI, and U-Boot's EFI boot manager launches the system's own `BOOTRISCV64.EFI` loader, which reads the kernel and hands off exactly as it would on real hardware. The RISC-V runs are **serial-console only** (headless, no `virtio-gpu`), so `vnc` / `spice` do not apply; they attach the storage volumes, a `virtio-net` NIC and an xHCI USB stack (keyboard / tablet / mass-storage). Override the core count with `SMP=<n>` (the default is the host's core count, capped at 8 - see below).

Like the native run, the ARM64 runs give the guest as many cores as the host has, but capped at **8** - the GICv2 interrupt controller QEMU's `virt` machine emulates addresses at most 8 CPU interfaces. The RISC-V runs are capped at 8 for a different reason: U-Boot stops booting above roughly 50 harts, and on a host with more cores than that the guest produced no output at all while OpenSBI logged normally, which reads as a broken loader rather than as too many CPUs. Override the count on any run/test with `SMP=<n>` (e.g. `SMP=4 ./run.sh --arch aarch64`, `SMP=1 ./test.sh --arch aarch64`).

### Networking

Interactive runs attach a `virtio-net` NIC on QEMU's user-mode (SLIRP) network: the guest configures itself over DHCP (address `10.0.2.15`, gateway `10.0.2.2`), so `ping`, `nslookup`, `tcp` and the other net tools reach the outside world through the host with no setup. The host itself is reachable from the guest as `10.0.2.2`. In the other direction, the host's `127.0.0.1:5555` is forwarded to the guest's port 80, so a server started in the guest (`httpd &`) is reachable from the host:

```sh
curl http://127.0.0.1:5555/
```

### Graphical display (VNC / SPICE)

The graphical displays apply to the **x86_64 and ARM64** builds - the x86_64 run (`./run.sh` on an x86_64 host, or `./run.sh --arch x86_64` anywhere) and the ARM64 runs (`./run.sh --arch aarch64`); the RISC-V runs are serial-console only, so `vnc` / `spice` do not apply there. Every run is headless by default - the framebuffer is still rendered internally, but no window is shown. To watch it live, attach a display server as an argument; the two combine freely with each other (and with any other `./run.sh` arguments):

```sh
./run.sh --display vnc         # VNC server on port 5900
./run.sh --display spice       # SPICE server on port 5930
./run.sh --display vnc,spice   # both at the same time
```

Then connect from your machine - for example a VNC viewer to `HOST:5900`, or `remote-viewer spice://HOST:5930`. The serial console keeps running on your terminal alongside the graphical display.

The servers bind to all interfaces (`0.0.0.0`) without a password. On a machine reachable from untrusted networks, restrict the bind to localhost and connect over an SSH tunnel instead:

```sh
VNC_ADDR=127.0.0.1:0 ./run.sh --display vnc   # VNC on localhost:5900 only
ssh -L 5900:localhost:5900 user@HOST   # from your machine, then point the viewer at localhost:5900
```

`VNC_ADDR` sets the VNC bind/display (default `0.0.0.0:0`); `SPICE_PORT` sets the SPICE port (default `5930`).

### Audio

Interactive runs attach a `virtio-sound` device that the userspace `driver.virtio-snd` + `AudioService` drive for PCM playback. The shell `beep [hz] [ms]` command plays a tone (default 440 Hz for 200 ms). Audio is routed to the host through SPICE, so to hear it run with a SPICE display and connect a SPICE client:

```sh
./run.sh --display spice                      # then: remote-viewer spice://HOST:5930
```

Without a SPICE display the device is still present (the guest plays into a null sink, nothing is emitted). The headless test path attaches no sound device, so there `beep` reports `no audio device`.

### Screenshot

To save an image of the framebuffer, pass an output path - the format is taken from the extension (`png`, `jpg`, `webp`, `gif`, `bmp`, `ppm`):

```sh
just screenshot shot.png
just screenshot /root/screenshot.webp
```

If a `./run.sh` instance is already up, it attaches to it and snaps the **current** frame with no reboot - so you can grab a screenshot at any moment during a live run. Otherwise it boots a throwaway headless instance, waits for the boot log to finish, snaps that, and shuts it down. Format conversion uses ImageMagick (`png`/`jpg`/`webp`/...); a `netpbm`-only system can still write `png`/`jpg`/`ppm`.

## Create bootable images

`./run.sh` builds and boots a throwaway image automatically. To boot LiberSystem on real hardware - or to keep an image around - you can build standalone images explicitly. Both are written to `.build/boot/` and boot on any UEFI machine.

### CD/DVD image (ISO)

```sh
./image.sh --format iso
```

Builds a UEFI-only bootable image at `.build/boot/libersystem.iso`. Burn it to a CD/DVD, or write it straight to a USB stick (the EFI boot image is exposed as a GPT partition, so it also boots from a flash drive).

It is a **LiveCD**: the medium carries a LiberFS system volume, which the running system copies into memory at boot. Nothing is written back - the machine needs no disk, and a session's changes are gone when it stops.

```sh
sudo dd if=.build/boot/libersystem.iso of=/dev/sdX bs=4M conv=fsync status=progress
```

### Raw disk image (IMG)

```sh
./image.sh --format img              # default size 128M
./image.sh --format img --size 1G    # custom size (truncate-style suffixes: M, G - no trailing B)
```

Builds a raw GPT disk image at `.build/boot/libersystem.img` for a USB stick, SD card or hard disk. Unlike the ISO this is an **installed** system: two partitions, an EFI System Partition holding the loader and a recovery copy of the bootstrap programs, and a LiberFS system volume holding the kernel and everything else. The loader finds the volume by its superblock rather than by device order, so it boots on any UEFI machine whatever else is attached. Write it to a device with:

```sh
sudo dd if=.build/boot/libersystem.img of=/dev/sdX bs=4M conv=fsync status=progress
```

> Replace `/dev/sdX` with your target device (for example `/dev/sdb`). **Double-check the device name** - `dd` overwrites it without confirmation.

### Strip level

The kernel placed into an image is always stripped, because the debug info is never used at boot (the loader loads only the loadable segments, and the debugger reads symbols from the on-disk build). The amount stripped is selectable - it never affects booting, only the image size:

```sh
./image.sh --format iso                      # --strip debug (default): drop DWARF, keep symbols
./image.sh --format iso --strip all          # also drop the symbol table (smallest image)
./image.sh --format img --size 128M --strip all
```

## Test

LiberSystem ships an in-kernel test harness that runs under QEMU and reports the result through QEMU's `isa-debug-exit` device:

```sh
./test.sh
```

A successful run prints each test with `[ok]` and exits zero.

The same suite runs on the ARM64 and RISC-V builds (emulated on an x86_64 host), where the result is reported through Arm / RISC-V semihosting instead of `isa-debug-exit`:

```sh
./test.sh --arch aarch64        # the ARM64 build (all host cores, capped at 8 - see below)
SMP=1 ./test.sh --arch aarch64  # a single core
./test.sh --arch riscv64        # the RISC-V build (RISC-V semihosting; host cores, capped at 8)
./test.sh --arch all            # all three, in turn
```

## Debugging

The lab harness drives a live instance from the host - boot it, run shell commands
in the guest and get their output back, follow the serial log, capture network
traffic - without typing into the console by hand:

```sh
just lab boot --fresh     # boot with a freshly created data volume
just lab sh time ls       # run a shell command in the guest, print its output
just lab quit             # shut the instance down
```

See [docs/DEBUG.md](./docs/DEBUG.md) for the full debugging toolbox (all `lab`
subcommands, timing and tracing, packet capture).

For kernel-level debugging, start QEMU so it waits for a debugger (a GDB stub on port `:1234`, with KVM disabled for reliable single-stepping):

```sh
just debug
```

Then, in a second terminal, attach GDB - it loads the kernel symbols and connects automatically:

```sh
just gdb
```

## Development commands

Run `just --list` to see every available command. The most useful ones:

The build interface is a set of scripts at the repository root. Each takes flags rather than
encoding its arguments in its name, and each answers `--help`:

| Command | Description |
| --- | --- |
| `./build.sh [--arch A] [--part P]` | Build the system, or the parts you name (`kernel`, `user`, `libs`, `loader`, `packages`, `volume`, `sdk`, `all`). Anything after `--` goes to cargo. |
| `./run.sh [--arch A] [--display D] [--debug]` | Build and boot in QEMU. Defaults to the host's architecture. |
| `./test.sh [--arch A] [--tags T] [--fast] [--build-only]` | Run the in-kernel test suites. |
| `./image.sh [--format F] [--size S] [--strip L]` | Build bootable images (`iso`, `img`, `qcow2`). |
| `./check.sh [--gate N] [--conformance F]` | Run the build gates and image conformance suites; no arguments means all of them. |
| `./clean.sh [--part P] [--dry-run]` | Remove build output (`cargo`, `boot`, `logs`). |
| `./dev.sh <verb> [args]` | Drive the persistent development guest. |

A Justfile remains in `src/` for the specialist recipes that are not part of this interface -
formatting, the IDL generator, host-side test crates, benchmarks and the two loader builds whose
bodies genuinely differ per architecture. `just --list` shows them.

| Justfile recipe | Description |
| --- | --- |
| `./check.sh --gate undeclared-edge` | Temporarily change a staged executable's `DT_NEEDED` provider from declared `lsrt.lslib` to staged but undeclared `wire.lslib` on all three targets; package assembly must reject it before rewriting the system volume, then restore each artifact. |
| `./check.sh --gate duplicate-edge` | Temporarily make two staged `DT_NEEDED` entries name the same provider on all three targets; package assembly must reject the duplicate before rewriting the system volume, then restore each artifact. |
| `./check.sh --gate malformed-dynamic` | Temporarily inject a second `PT_DYNAMIC`, remove `DT_NULL`, and duplicate `DT_STRTAB` metadata in a staged executable on all three targets; package assembly must reject each form before rewriting the system volume, then restore each artifact. |
| `./check.sh --gate malformed-symbol-relocation` | Temporarily inject an invalid `DT_SYMENT`, oversized SysV symbol count, and misaligned `DT_PLTRELSZ` into a staged executable on all three targets; package assembly must reject each form before rewriting the system volume, then restore each artifact. |
| `./check.sh --gate identity-note` | Temporarily corrupt the embedded identity record in a staged dynamic executable on all three targets; package assembly must reject it before rewriting the system volume, then restore the ELF. |
| `./check.sh --gate dynamic-report` | Build all three target graphs and verify detailed, per-wave and whole-image reports against current ET_REL objects, imports, providers, closure, PIE/provider size and private/shared footprint. |
| `just dynamic-report-update` | Build all three target graphs and regenerate all checked dynamic executable reports. |
| `just lab <cmd>` | Drive a live instance for debugging (boot, run guest shell commands, logs, packet capture - see [docs/DEBUG.md](./docs/DEBUG.md)). |
| `just debug` | Boot in QEMU and wait for GDB on `:1234`. |
| `just gdb` | Attach GDB to a waiting QEMU instance. |
| `./build.sh --part user` | Build the userspace programs (services, drivers, tools) for one architecture. |
| `./build.sh --part sdk` | Build the SDK component. |
| `just gen` | Regenerate the typed service bindings and docs from the LSIDL definitions (`idl/*.lsidl`). |
| `just fmt` | Format all code (Rust via `rustfmt`, shell via `shfmt`). |
| `just fmt-check` | Check formatting without writing changes (CI-friendly). |

> `just fmt` and `just fmt-check` additionally require [`shfmt`](https://github.com/mvdan/sh) on your `PATH`.
