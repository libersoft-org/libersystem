# Binary and package format

This document is the on-disk / on-the-wire contract for the artifacts LiberSystem
boots and loads: the program ELF the kernel's loader executes, the `PKGARCH1`
archive that bundles those programs, the two packages built at compile time
(`init.pkg` and `volume.pkg`), and the `BootInfo` handoff the own UEFI loader
passes the kernel. Everything here is a fixed, versioned layout; both the writer
(the kernel's `build.rs`) and the readers (the kernel's `pkg.rs`, the userspace
storage runtime, the loader) decode it through one shared implementation
(`abi::Package`, `bootproto`), so the format and its parser never drift apart.

The platform is UEFI-only and boots through its own loader (`src/loader`). On x86
the loader is a native `x86_64-unknown-uefi` PE; on aarch64 / riscv64 it is an ELF
static-PIE with a hand-written PE header (the Linux EFI-stub technique). All three
read the same files off a FAT boot filesystem and hand the kernel the same
`BootInfo`.

## Artifact filename conventions

A LiberSystem-specific suffix identifies the loader contract a native artifact is
intended to satisfy. It does not replace format validation: every consumer still
validates the complete ELF structure and its own bounded input contract before it
maps anything.

- **`.lsexe` - native userspace executable.** Tools, services, components, probes
  and userspace drivers all use the canonical physical basename `<name>.lsexe`;
  their role and capability policy come from the system manifest, not from another
  filename suffix. The shell may expose the convenient short command `<name>`, but
  ProcessService records and reports the complete physical basename. Only the final
  extension classifies the artifact: an extensionless file is not a native
  executable, and removing exactly one final `.lsexe` forms its short command name.
  Thus `ping.lsexe.lsexe` is an executable whose short name is `ping.lsexe`, not
  `ping`. This naming migration is specified by M125; until it is complete, existing
  system images may still contain legacy extensionless program entries.
- **`.lslib` - native shared library.** A library uses `<name>.lslib`, never the
  Unix-style `lib<name>.so`. The complete filename is its image-internal identity:
  it is used as the ELF `DT_SONAME`, appears unchanged in consumers' `DT_NEEDED`
  entries, and resolves under `vol://system/lib/`. A `.lslib` is a load-time provider
  for a `.lsexe`, not a directly launchable program. These libraries are rebuilt as
  part of one immutable system image and are not a stable cross-release or
  third-party ABI; see [Dynamic linking](DYNAMIC_LINKING.md).

These suffixes do not replace standard formats outside the native program loader.
WebAssembly components remain `.wasm`, UEFI applications remain `.efi`, and bootable
media remain `.iso` or `.img`. Data and media extensions are hints to applications,
which validate or sniff the file contents. The current `init.pkg` and `volume.pkg`
files are internal `PKGARCH1` boot archives described below; no filename suffix has
yet been chosen for a future installable application package.

## 1. The program ELF contract

Every userspace program (services, drivers, tools) is a freestanding
`no_std` / `no_main` ELF built for a bare-metal target (`x86_64-unknown-none`,
`aarch64-unknown-none`, `riscv64gc-unknown-none-elf`). The kernel's ELF loader is
deliberately minimal, so a program must obey a narrow contract:

- **`ET_EXEC`, fixed address.** Built with `-C relocation-model=static` (riscv
  also `-C code-model=medium`), so the image is a non-PIE executable linked at a
  fixed base. The loader maps each `PT_LOAD` segment at its link-time `p_vaddr`
  and applies **no relocations** - the program must use absolute addressing.
- **Segments only.** The loader consults the program headers, maps the loadable
  segments (with their `p_flags` permissions - a writable segment is never also
  executable, W^X), zero-fills the `.bss` tail (`p_memsz > p_filesz`), and jumps
  to `e_entry`. Section headers, the symbol table and debug info are ignored at
  load time (and stripped from the staged image, see below).
- **Entry stub.** `e_entry` is the rt entry stub, which sets up the stack, reads
  the bootstrap handle the kernel passed in a register, and calls the program's
  `__user_main(bootstrap: u64)`.
- **The bootstrap channel.** A freshly spawned process starts with exactly one
  capability: a bootstrap Channel handle, passed in the first argument register.
  Everything else - service clients, the argument string, the stdout console, the
  cwd - arrives as messages on that channel (see the per-program bootstrap
  handshakes and `CapSet`). A program reaches nothing it was not handed: there is
  no ambient authority.
- **Stripped when staged.** The build strips each program to its loadable image
  before packing it (the loader executes only the segments, so the symbol / debug
  sections are dead weight in the kernel binary and in boot memory). The raw
  debug ELF is multiple megabytes; stripped it is tens to a few hundred kilobytes
  - the residual size is unoptimized (`opt-level = 0`, debug) codegen plus the
  generated `proto` codec each program links, a deliberate build-iteration vs
  size tradeoff, not carried bulk. A size-optimized build (`opt-level = "z"`)
  reclaims roughly 80% but is not used, since the staged size is not a constraint.

## 2. The `PKGARCH1` archive format

A package is a tiny read-only archive: a header, a fixed-size entry table, then
the concatenated file blobs. All integers are little-endian. The constants live
in `abi` (`PKG_MAGIC`, `PKG_HEADER_LEN`, `PKG_ENTRY_LEN`, `PKG_NAME_LEN`).

```
offset  size  field
------  ----  -----------------------------------------------------------
Header (PKG_HEADER_LEN = 16 bytes)
  0      8    magic = "PKGARCH1"
  8      4    entry count (u32)
 12      4    reserved (u32, 0)

Entry table: `count` entries, PKG_ENTRY_LEN = 32 bytes each
  0     24    name, NUL-padded (PKG_NAME_LEN); compared up to the first NUL
 24      4    blob offset from the start of the archive (u32)
 28      4    blob size in bytes (u32)

Blobs
  the file bodies, concatenated, each at the offset its entry names
```

Parsing (`abi::Package::parse`) validates the magic and that the entry table
fits; `lookup(name)` scans the table, matches the name up to its first NUL, and
returns the blob slice (bounds-checked). `name(index)` / `len()` enumerate the
archive. The reader borrows the underlying bytes - nothing is copied - so a
package mapped `'static` (a boot module) hands out `'static` program slices.

## 3. The two build-time packages

The kernel's `build.rs` assembles two `PKGARCH1` archives from the shared service
manifest (`src/user/services/manifest.txt`), keyed by each program's manifest
name:

- **`init.pkg`** - the pinned bootstrap set: the services on the path to
  mounting the system volume (LogService, DeviceManager, StorageService and its
  media/iso/udf/usb instances' binary, ProcessService) plus the bootstrap
  `virtio_blk` driver, and `SystemManager` / `ServiceManager` themselves. These
  cannot be loaded from the volume because they are what makes the volume
  mountable, so they ride in the init package.
- **`volume.pkg`** - the system-volume seed: every other service, driver, tool
  and component (staged under `bin/`, `drivers/`, ...), plus the plain seed files
  under `src/volume/` (`hello.txt`, `motd.txt`). StorageService formats a fresh
  LiberFS from this archive on first boot and serves its files over `vol://system`.

Both are staged (by `boot/mkimage.sh`) onto the FAT boot filesystem next to the
loader, under their `product.conf` names (`INIT_PACKAGE` / `VOLUME_PACKAGE`), and
the loader loads them as boot modules alongside the kernel.

## 4. The `BootInfo` handoff (`bootproto`)

The loader passes the kernel a single `#[repr(C)]` `BootInfo` (defined in the
zero-dependency `bootproto` crate), its layout frozen by a `MAGIC` + `VERSION`
guard the kernel checks before trusting anything. The kernel receives a pointer
to it (in a register, or - on the device-tree arches - reached by peeking the
magic at the pointed-to address to tell a `BootInfo` from a raw DTB). Key fields:

- `magic` / `version` - the compatibility guard; a mismatch refuses the boot.
- `hhdm_offset` - the higher-half direct map base: `virt = phys + hhdm_offset`
  for all physical memory.
- `memmap` / `memmap_len` - the physical memory map (`MemRegion`s), which seeds
  the frame allocator's usable regions.
- `modules` / `modules_len` - the loaded packages (`Module`s: base + length +
  name), how the kernel finds `init.pkg` and `volume.pkg` in memory.
- `framebuffer` / `fb_present` - the boot framebuffer geometry and pixel format
  (from the UEFI GOP). On x86 `framebuffer.addr` is an HHDM virtual address the
  loader already mapped; on the device-tree arches it is the framebuffer's
  physical base (the loader builds no page tables, so the kernel maps it through
  its own direct map).
- `rsdp` - the ACPI RSDP physical address on x86 (the kernel parses the MADT to
  enumerate LAPICs and wake the APs); 0 elsewhere.
- `smp_trampoline` - a reserved sub-1 MiB page for the x86 AP real-mode
  bring-up trampoline.
- `dtb` - the flattened device tree's physical address on aarch64 / riscv64 (0 on
  x86); the kernel reads its RAM / CPU / device inventory from it.
