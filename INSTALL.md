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

- system packages: `build-essential`, `git`, `curl`, `xorriso`, `gdisk`, `mtools`, `netpbm`, `imagemagick`, `icoutils`, `icnsutils`, `libicns-dev`, `python3-pil`, `pngcheck`, `apngasm`, `apngdis`, `gifsicle`, `webp`, `socat`, `qemu-system-x86`, `qemu-system-arm`, `qemu-system-riscv`, `qemu-utils`, `ovmf`, `qemu-efi-aarch64`, `u-boot-qemu`, `gdb`, `lld`, `llvm`, `clang` (`icoutils`, `icnsutils`, `libicns-dev`, `python3-pil`, `pngcheck`, `apngasm`, `apngdis`, `gifsicle` and `webp` are host-only image-conformance tools; `qemu-system-arm` + `qemu-efi-aarch64` are the ARM64 emulator and its UEFI firmware, `qemu-system-riscv` + `u-boot-qemu` the RISC-V emulator and its U-Boot UEFI firmware; omit the foreign-architecture packages if you only build for x86_64)
- `rustup` with the **nightly** toolchain plus the `rust-src` and `llvm-tools-preview` components (required for `build-std` and the kernel build)
- `just`, the task runner

The project pins the nightly toolchain via `rust-toolchain.toml`, so no global toolchain switch is needed.

## Build

The kernel is built for the `x86_64-unknown-none` target. From the `src` directory:

```sh
just build
```

This first builds the userspace programs (the services, drivers and command-line tools that make up the init package) and the SDK component, then compiles the kernel ELF - which embeds the init package - into `kernel/target/x86_64-unknown-none/debug/kernel`. A plain build does not produce a disk image - the run step assembles a bootable ISO on demand, and you can build standalone images with [`just iso`](#create-bootable-images) and [`just img`](#create-bootable-images).

## Run

```sh
just run
```

`just run` builds and boots the **host's native architecture** (the `x86_64` build on an x86_64 host, the `aarch64` build on an ARM64 host). It launches QEMU headless, with the system's serial console wired to your terminal. The boot log reports each service coming online and ends at an interactive shell prompt:

```
vol://system>
```

To capture the serial output to a file instead of the terminal (useful over SSH or in scripts):

```sh
SERIAL=file:boot.log just run
```

QEMU uses KVM (with `-cpu host`) when `/dev/kvm` is available, and gives the guest as many cores as the host has (`nproc`); override the count with `SMP=<n> just run`.

### Running a specific architecture

`just run` picks the host's architecture; to force a particular one - it runs emulated when it is not the host's - use the explicit recipes:

```sh
just run-x86_64        # the x86_64 build (native with KVM on an x86_64 host)
just run-aarch64       # the ARM64 build via QEMU's direct -kernel load (the quick path)
just run-aarch64-uefi  # the ARM64 build booted through the system's own UEFI loader (AAVMF)
just run-riscv64       # the RISC-V build via QEMU's direct -kernel load over OpenSBI (the quick path)
just run-riscv64-uefi  # the RISC-V build booted through the system's own UEFI loader (U-Boot)
```

They all reach the same interactive shell. `run-aarch64` boots the kernel the fast way (QEMU loads it directly); `run-aarch64-uefi` exercises the full firmware path - the AAVMF UEFI firmware runs the system's own `BOOTAA64.EFI` loader, which reads the kernel off a FAT boot volume and hands off exactly as it would on real hardware. The ARM64 build is emulated on an x86_64 host (no KVM), so it boots more slowly than the native run. The ARM64 runs attach the **same device set as x86_64** - `virtio-gpu` (the graphical display), `virtio-keyboard` / `virtio-tablet` input, `virtio-sound`, `virtio-net`, `virtio-serial` and the xHCI USB stack - so the `vnc` / `spice` displays below work identically. The one difference is the boot log: QEMU's `virt` machine has no VGA framebuffer, so the kernel does not draw the boot log pixel-by-pixel as on x86_64; instead the log is replayed as text onto the virtio-gpu display once ConsoleService takes over, so it still appears on screen.

The RISC-V build is always emulated. `run-riscv64` boots the kernel the fast way (QEMU's `-kernel` load over OpenSBI, which hands off in S-mode with the device tree); `run-riscv64-uefi` exercises the full firmware path - QEMU runs the S-mode U-Boot on OpenSBI, and U-Boot's EFI boot manager launches the system's own `BOOTRISCV64.EFI` loader off a FAT boot volume, which reads the kernel and hands off exactly as it would on real hardware. The RISC-V runs are **serial-console only** (headless, no `virtio-gpu`), so `vnc` / `spice` do not apply; they attach the storage volumes, a `virtio-net` NIC and an xHCI USB stack (keyboard / tablet / mass-storage). Override the core count with `SMP=<n>` (the recipes default to `SMP=4`).

Like the native run, the ARM64 runs give the guest as many cores as the host has, but capped at **8** - the GICv2 interrupt controller QEMU's `virt` machine emulates addresses at most 8 CPU interfaces. Override the count on any run/test with `SMP=<n>` (e.g. `SMP=4 just run-aarch64`, `SMP=1 just test-aarch64`).

### Networking

Interactive runs attach a `virtio-net` NIC on QEMU's user-mode (SLIRP) network: the guest configures itself over DHCP (address `10.0.2.15`, gateway `10.0.2.2`), so `ping`, `nslookup`, `tcp` and the other net tools reach the outside world through the host with no setup. The host itself is reachable from the guest as `10.0.2.2`. In the other direction, the host's `127.0.0.1:5555` is forwarded to the guest's port 80, so a server started in the guest (`httpd &`) is reachable from the host:

```sh
curl http://127.0.0.1:5555/
```

### Graphical display (VNC / SPICE)

The graphical displays apply to the **x86_64 and ARM64** builds - the x86_64 run (`just run` on an x86_64 host, or `just run-x86_64` anywhere) and the ARM64 runs (`just run-aarch64` / `just run-aarch64-uefi`); the RISC-V runs are serial-console only, so `vnc` / `spice` do not apply there. Every run is headless by default - the framebuffer is still rendered internally, but no window is shown. To watch it live, attach a display server as an argument; the two combine freely with each other (and with any other `just run` arguments):

```sh
just run vnc        # VNC server on port 5900
just run spice      # SPICE server on port 5930
just run vnc spice  # both at the same time
```

Then connect from your machine - for example a VNC viewer to `HOST:5900`, or `remote-viewer spice://HOST:5930`. The serial console keeps running on your terminal alongside the graphical display.

The servers bind to all interfaces (`0.0.0.0`) without a password. On a machine reachable from untrusted networks, restrict the bind to localhost and connect over an SSH tunnel instead:

```sh
VNC_ADDR=127.0.0.1:0 just run vnc      # VNC on localhost:5900 only
ssh -L 5900:localhost:5900 user@HOST   # from your machine, then point the viewer at localhost:5900
```

`VNC_ADDR` sets the VNC bind/display (default `0.0.0.0:0`); `SPICE_PORT` sets the SPICE port (default `5930`).

### Audio

Interactive runs attach a `virtio-sound` device that the userspace `driver.virtio-snd` + `AudioService` drive for PCM playback. The shell `beep [hz] [ms]` command plays a tone (default 440 Hz for 200 ms). Audio is routed to the host through SPICE, so to hear it run with a SPICE display and connect a SPICE client:

```sh
just run spice                         # then: remote-viewer spice://HOST:5930
```

Without a SPICE display the device is still present (the guest plays into a null sink, nothing is emitted). The headless test path attaches no sound device, so there `beep` reports `no audio device`.

### Screenshot

To save an image of the framebuffer, pass an output path - the format is taken from the extension (`png`, `jpg`, `webp`, `gif`, `bmp`, `ppm`):

```sh
just screenshot shot.png
just screenshot /root/screenshot.webp
```

If a `just run` instance is already up, it attaches to it and snaps the **current** frame with no reboot - so you can grab a screenshot at any moment during a live run. Otherwise it boots a throwaway headless instance, waits for the boot log to finish, snaps that, and shuts it down. Format conversion uses ImageMagick (`png`/`jpg`/`webp`/...); a `netpbm`-only system can still write `png`/`jpg`/`ppm`.

## Create bootable images

`just run` builds and boots a throwaway image automatically. To boot LiberSystem on real hardware - or to keep an image around - you can build standalone images explicitly. Both are written to `boot/.build/` and boot on any UEFI machine.

### CD/DVD image (ISO)

```sh
just iso
```

Builds a UEFI-only bootable image at `boot/.build/libersystem.iso`. Burn it to a CD/DVD, or write it straight to a USB stick (the EFI boot image is exposed as a GPT partition, so it also boots from a flash drive):

```sh
sudo dd if=boot/.build/libersystem.iso of=/dev/sdX bs=4M conv=fsync status=progress
```

### Raw disk image (IMG)

```sh
just img        # default size 64M
just img 1G     # custom size (truncate-style suffixes: M, G, ...)
```

Builds a raw GPT disk image at `boot/.build/libersystem.img` for a USB stick, SD card or hard disk. It holds a single EFI System Partition with the own UEFI loader, the kernel and the packages, so it boots on any UEFI machine. Write it to a device with:

```sh
sudo dd if=boot/.build/libersystem.img of=/dev/sdX bs=4M conv=fsync status=progress
```

> Replace `/dev/sdX` with your target device (for example `/dev/sdb`). **Double-check the device name** - `dd` overwrites it without confirmation.

### Strip level

The kernel placed into an image is always stripped, because the debug info is never used at boot (the loader loads only the loadable segments, and the debugger reads symbols from the on-disk build). The amount stripped is selectable - it never affects booting, only the image size:

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

The same suite runs on the ARM64 and RISC-V builds (emulated on an x86_64 host), where the result is reported through Arm / RISC-V semihosting instead of `isa-debug-exit`:

```sh
just test-aarch64          # the ARM64 build (all host cores, capped at 8 - see below)
SMP=1 just test-aarch64    # a single core
just test-riscv64          # the RISC-V build (RISC-V semihosting; SMP=4 by default)
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

| Command | Description |
| --- | --- |
| `just build` | Build everything: the userspace programs, the SDK component, and the kernel (whose build embeds the init package). |
| `just run [vnc] [spice]` | Build and boot the **host's native architecture** in QEMU (headless by default; on the x86_64 build add `vnc` and/or `spice` for a live VNC `:5900` / SPICE `:5930` display - they combine). |
| `just run-x86_64 [vnc] [spice]` | Force the x86_64 build (native with KVM on an x86_64 host, emulated elsewhere). |
| `just run-aarch64` | Force the ARM64 build via QEMU's direct `-kernel` load (headless serial; native on an ARM64 host, emulated on x86_64). |
| `just run-aarch64-uefi` | Force the ARM64 build booted through the system's own UEFI loader under the AAVMF firmware. |
| `just run-riscv64` | Force the RISC-V build via QEMU's direct `-kernel` load over OpenSBI (headless serial; always emulated). |
| `just run-riscv64-uefi` | Force the RISC-V build booted through the system's own UEFI loader under U-Boot's EFI boot manager. |
| `just screenshot <path>` | Save a framebuffer image to `<path>` (format by extension: png/jpg/webp/...); snaps a live `just run` if one is up, else boots a throwaway. |
| `just iso [strip]` | Build a hybrid BIOS+UEFI ISO into `boot/.build/` (`strip` = `debug` or `all`). |
| `just img [size] [strip]` | Build a raw GPT disk image (default `64M`) into `boot/.build/`. |
| `just test` | Run the in-kernel test harness in QEMU. |
| `just test-aarch64` | Run the in-kernel test harness for the ARM64 build under QEMU (Arm semihosting maps pass/fail; defaults to all host cores capped at 8 - the GICv2 limit - override with `SMP=<n>`). |
| `just test-riscv64` | Run the in-kernel test harness for the RISC-V build under QEMU (RISC-V semihosting maps pass/fail; defaults to `SMP=4` - override with `SMP=<n>`). |
| `just static-image-check` | Temporarily inject an `ET_EXEC` header into a staged dynamic executable on all three targets; package assembly must reject it before rewriting the system volume, then restore each artifact. |
| `just undeclared-edge-check` | Temporarily change a staged executable's `DT_NEEDED` provider from declared `lsrt.lslib` to staged but undeclared `wire.lslib` on all three targets; package assembly must reject it before rewriting the system volume, then restore each artifact. |
| `just duplicate-edge-check` | Temporarily make two staged `DT_NEEDED` entries name the same provider on all three targets; package assembly must reject the duplicate before rewriting the system volume, then restore each artifact. |
| `just malformed-dynamic-check` | Temporarily inject a second `PT_DYNAMIC`, remove `DT_NULL`, and duplicate `DT_STRTAB` metadata in a staged executable on all three targets; package assembly must reject each form before rewriting the system volume, then restore each artifact. |
| `just malformed-symbol-relocation-check` | Temporarily inject an invalid `DT_SYMENT`, oversized SysV symbol count, and misaligned `DT_PLTRELSZ` into a staged executable on all three targets; package assembly must reject each form before rewriting the system volume, then restore each artifact. |
| `just dependency-graph-check` | Temporarily reorder a staged executable's canonical provider sidecar on all three targets; package assembly independently recomputes the manifest graph and must reject the drift before rewriting the system volume, then restore the sidecar. |
| `just dynamic-report-check` | Build all three target graphs and verify `docs/DYNAMIC_EXECUTABLES.tsv` against every dynamic tool's imports, providers, closure, PIE size and private writable footprint. |
| `just dynamic-report-update` | Build all three target graphs and regenerate the checked dynamic executable report. |
| `just lab <cmd>` | Drive a live instance for debugging (boot, run guest shell commands, logs, packet capture - see [docs/DEBUG.md](./docs/DEBUG.md)). |
| `just debug` | Boot in QEMU and wait for GDB on `:1234`. |
| `just gdb` | Attach GDB to a waiting QEMU instance. |
| `just user` | Build only the userspace programs (services, drivers, tools). |
| `just sdk` | Build the SDK's Wasm component and stage it into the system volume. |
| `just gen` | Regenerate the typed service bindings and docs from the LSIDL definitions (`idl/*.lsidl`). |
| `just fmt` | Format all code (Rust via `rustfmt`, shell via `shfmt`). |
| `just fmt-check` | Check formatting without writing changes (CI-friendly). |
| `just clean` | Remove build artifacts. |

> `just fmt` and `just fmt-check` additionally require [`shfmt`](https://github.com/mvdan/sh) on your `PATH`.
