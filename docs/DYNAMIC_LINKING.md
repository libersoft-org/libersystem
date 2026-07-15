# Dynamic linking

LiberSystem shared libraries are an internal system-image optimization, not a
cross-release application ABI. Every executable and shared object in one image is
built together by the pinned Rust toolchain, staged together, measured together,
and replaced together. Rust symbol names, layouts, calling conventions, and compiler
intrinsics may change in the next image without compatibility shims.

## Compatibility boundary

- The immutable system image is the compatibility unit. There are no public soname
  version promises and no mixing libraries from different images.
- Rust-mangled symbols are valid inside that image because all producers and consumers
  use the same compiler revision and build graph. Exported C ABI wrappers are reserved
  for a future third-party package boundary.
- System libraries live under `vol://system/lib/`. Per-application third-party
  libraries remain part of the phase-3 package format rather than entering the global
  namespace.
- System-library filenames follow the central [artifact filename
  conventions](PACKAGE_FORMAT.md#artifact-filename-conventions): they use the
  LiberSystem-specific `.lslib` suffix and no Unix `lib` prefix, for example
  `png.lslib`, `proto.lslib`, and `lsrt.lslib`.
- Library crates also use prefix-free owner directories. A normal leaf lives at
  `src/user/<name>/`, and its generated objects stay with that owner under
  `src/user/<name>/shared/<target>/<name>.lslib`. The runtime and generated protocol
  providers use the equivalent `src/user/rt/shared/` and `src/proto/shared/` paths.
  The system image still installs all of them into the flat, resolver-owned
  `vol://system/lib/` namespace.
- Resolution is eager and deterministic. Lazy PLT binding, `LD_PRELOAD`, environment
  search paths, runtime library replacement, symbol interposition, and unload are not
  supported.

## Build model

The bare-metal Rust targets intentionally support neither Cargo's `dylib` nor
`cdylib` artifact kind. A system shared object is therefore built in two deterministic
steps:

1. Cargo builds the library and its `core`/`alloc` graph with
   `RUSTFLAGS=-C relocation-model=pic` (plus the target's required code model).
2. The pinned `rust-lld` links the resulting crate archive with `-shared`, an explicit
   target emulation and soname. The output must be ELF64 `ET_DYN` with separate R,
   RX, and RW `PT_LOAD` segments and no W+X segment.

`lsrt.lslib` is linked from the extracted PIC object members of `core`, `alloc`,
`compiler_builtins`, `abi`, and `rt`; direct object linking preserves the root
provider's dynamic exports. `rt`'s generated-protocol transport adapter is an optional
default feature excluded from this root, removing a dependency cycle. `proto.lslib` is
the only generated-protocol provider and depends on `lsrt.lslib`. Leaf rlibs remain archive
linked against their explicit provider set.

Because Cargo cannot consume a Rust dylib on these targets, consumers cross a generated
image-internal export boundary. A small explicit unmangled smoke ABI currently pins this
path; ordinary Rust-mangled exports remain available to components built in the same
image. The eventual image builder generates these wrappers/exports with the whole graph,
never as a stable third-party ABI.

This was probed with the complete graph on all supported targets. The emitted relocation forms
that the loader contract recognizes are:

| Architecture | Relative | Imported data/function slot |
| --- | ---: | ---: |
| x86_64 | `R_X86_64_RELATIVE` (8) | `R_X86_64_GLOB_DAT` (6), `R_X86_64_JUMP_SLOT` (7) |
| aarch64 | `R_AARCH64_RELATIVE` (1027) | `R_AARCH64_GLOB_DAT` (1025), `R_AARCH64_JUMP_SLOT` (1026) |
| riscv64 | `R_RISCV_RELATIVE` (3) | `R_RISCV_64` (2), `R_RISCV_JUMP_SLOT` (5) |

`lsrt.lslib` is the root symbol provider. In addition to the runtime API it owns compiler
support exports such as `memcpy`, `memset`, and the pinned core panic paths. `proto.lslib`
depends on `lsrt.lslib`; higher leaves depend only on their declared lower libraries.
Cycles are rejected by the image builder and by ProcessService.

## Ownership split

ProcessService owns dependency policy because it already holds the StorageService
capability used to read `vol://system/bin/*`. For a launch it:

1. reads the main ELF and its `DT_NEEDED` names;
2. resolves only canonical names under `vol://system/lib/`;
3. builds a bounded dependency DAG, rejects cycles/duplicates/missing libraries, and
   orders providers before consumers;
4. asks the kernel to load each library, then the main image, into the new process;
5. starts the entry thread only after every eager relocation succeeds.

The kernel never performs filesystem I/O and never invents search policy. It owns the
mechanisms that require privilege: page allocation, shared-frame caching, mapping into
a foreign address space, W^X, load-bias selection, symbol registry construction, and
relocation writes.

## Mapping and sharing

Each module receives a deterministic, page-aligned load bias in a reserved user
address window. `PT_LOAD` ranges must be non-overlapping, `p_filesz <= p_memsz`, and
`p_offset`/`p_vaddr` must satisfy `p_align`. The entry point must lie in an executable
segment.

- RX and immutable R pages of a system library are cached by image identity and mapped
  read-only into every consumer. One physical frame set is therefore resident for N
  processes.
- RW pages, BSS, GOT, and relocation targets are private per process.
- Relocations are applied eagerly through the kernel's direct physical mapping. A
  relocation target must lie wholly inside a private writable segment; text
  relocations are rejected.
- After relocation, mappings retain their final ELF permissions. No writable alias of
  shared executable text is exposed to userspace.

The cache key includes the architecture, complete library bytes, and image generation.
A process holds references to shared frames for its lifetime; immutable system images
make invalidation an image-replacement operation rather than a live-update protocol.

## Hostile-input rules

ELF and dynamic metadata are untrusted even for system-volume files. The loader checks
all header sizes, table multiplication/addition, virtual-to-file translations, string
termination, symbol indices, relocation entry sizes/counts, load ranges, alignment,
and arithmetic before allocation or access. A dynamic table must contain `DT_NULL`.
Unknown mandatory relocation forms, unresolved strong symbols, duplicate providers,
out-of-range targets, W+X segments, and malformed dependencies fail the process load
with `ERR_INVALID`; they never partially start a process or panic the kernel.

Limits are explicit: bounded module count, dependency depth, symbol count, string-table
bytes, relocation count, and mapped bytes per process/domain. Failed loads release all
private frames and shared-cache references acquired by that transaction.

## Measurement gate

Dynamic conversion is retained only where aggregate image and resident-memory numbers
beat optimized static release builds. `docs/PERF.md` records per-binary size, system
volume size, cold-start latency, and resident frames for concurrent processes. Small
single-purpose tools may remain static when the loader and relocation cost outweighs
sharing.
