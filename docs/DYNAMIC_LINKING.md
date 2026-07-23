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
  `png.lslib`, `storage-proto.lslib`, and `lsrt.lslib`.
- Library crates also use prefix-free owner directories. A normal leaf lives at
  `src/user/<name>/`, and its generated objects stay with that owner under
  `src/user/<name>/shared/<target>/<name>.lslib`. The runtime and generated protocol
  providers use equivalent owner-specific `src/user/<name>/shared/` paths.
  The system image still installs all of them into the flat, resolver-owned
  `vol://system/lib/` namespace.
- Resolution is eager and deterministic. Lazy PLT binding, `LD_PRELOAD`, environment
  search paths, runtime library replacement, symbol interposition, and unload are not
  supported.
- Every native executable staged under `vol://system/bin/` is a PIE `ET_DYN` consumer
  of system libraries. A static `/bin` executable is an image-construction error. Pinned
  boot-critical programs in `init.pkg` may remain self-contained until their providers
  are available before StorageService, but this exception never creates a static copy on
  the mounted system volume.

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
provider's dynamic exports. `rt` depends only on `abi`. The transport-independent
codec and representation foundation is `wire.lslib`, which depends on `lsrt.lslib`;
`ipc-client.lslib` owns channel and resolver transports over `wire + lsrt`.
Generated protocol ownership is package-scoped. All 15 LSIDL packages have dedicated
providers: `audio-proto`, `base-proto`, `config-proto`, `device-proto`, `display-proto`,
`input-proto`, `log-proto`, `network-proto`, `observability-proto`, `process-proto`,
`resources-proto`, `security-proto`, `session-proto`, `storage-proto` and `time-proto`.
Each leaf owns its generated types, codecs, stream helpers and concrete channel
implementation thunks. Network address parsing/rendering lives with `network-proto` and
timestamp rendering with `time-proto`, because inherent methods must be defined by the
crate that owns their generated type.

The `proto` Rust crate remains a compile-time compatibility facade that reexports all
package crates. It emits no runtime provider. `wire` remains available as `proto::codec`
and storage path resolution as `proto::path` for source compatibility. The path
implementation and runtime symbols belong to `storage-proto`; shell parsing belongs to
`service-util`. Leaf rlibs are archive linked against their explicit provider set.

`lsidl-gen` resolves the complete input graph for validation and documentation, then may
emit selected Rust packages into separate crate roots. An external-package map replaces
the corresponding compatibility module with a reexport, so one package has exactly one
Rust type identity. Generated value codec primitives are public across these internal
crate boundaries. Generation and check recipes cover all 15 package roots and the
compatibility root together, including stale-output manifests.

The shared-image builder checks every provider's exact runtime edges after each link.
Cargo type dependencies that inline completely do not create a false `DT_NEEDED` edge;
out-of-line imported codecs require their package owner directly. Every direct runtime
import has exactly one owner in the declared closure. RISC-V build-std uses a 32 MB rustc
worker stack; smaller stacks have crashed the pinned compiler while elaborating drops in
`core`.

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
support exports such as `memcpy`, `memset`, and the pinned core panic paths. Higher leaves
depend only on their declared lower libraries.
Cycles are rejected by the image builder and by ProcessService.

The production executable graph must compile providers and consumers with one pinned
toolchain/profile/feature identity. The initial pilot compiled runtime providers and
ordinary tools under different feature identities, so arbitrary Rust-mangled imports
could not be assumed to resolve. Transport ownership is now split as follows:

- `lsrt.lslib` owns core/alloc/compiler builtins, allocator, syscalls, channels, waits,
  stdio and process primitives;
- `wire.lslib` owns transport-independent codec readers/writers, `Transport`, buffers and
  representation modes;
- `ipc-client.lslib` owns `ChannelTransport`, resolver transport and shared-buffer staging
  over `wire + lsrt`; its only direct runtime imports currently cross explicit
  image-internal `recv_vec_blocking` and `resolve` symbols;
- generated LSIDL domain-client libraries depend on those roots, remain separate by
  contract domain and export concrete channel/resolver clients. The generic
  `Client<T: Transport>` remains available for tests/special transports but is not
  monomorphized into production `/bin` executables;
- a generated architecture entry object supplies `_start` and calls the executable's
  `__user_main` without statically linking another runtime.

This is an image-internal Rust ABI, never a cross-image promise. The image builder
rejects duplicate compiled identities for one provider and duplicate allocator/panic/
compiler-runtime ownership.

## Ownership split

ProcessService owns dependency policy because it already holds the StorageService
capability used to read `vol://system/bin/*`. For a launch it:

1. reads the main ELF and its matching `id/bin/*` record, whose SHA-256 must match
  the ELF's embedded identity note;
2. resolves only canonical names under `vol://system/lib/`, verifying each matching
  `id/lib/*` record and identity note before the provider enters the graph;
3. builds a bounded dependency DAG, rejects cycles/duplicates/missing libraries, and
   orders providers before consumers;
4. requires every identity record's direct-provider digests to match the resolved
  provider identities;
5. asks the kernel to load each library, then the main image, into the new process;
6. starts the entry thread only after every eager relocation succeeds.

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

The runtime loader and package staging audit use one relocation policy. Every staged
provider and executable is scanned before packaging; the audit permits only
`RELATIVE` with symbol index zero plus `64`, `GLOB_DAT` and `JUMP_SLOT` on x86_64;
`RELATIVE` plus `ABS64`, `GLOB_DAT` and `JUMP_SLOT` on AArch64; and `RELATIVE`
plus `64` and `JUMP_SLOT` on RISC-V. Any other relocation form, including TLS,
COPY and architecture-specific forms outside this list, rejects the artifact before
it reaches the system volume.

The focused dynamic-link gate launches the staged `dyn_probe` through
ProcessService and requires its `pix.lslib -> lsrt.lslib` dependency DAG to load,
relocate and report successfully. It also mutates `echo.lsexe` in an
in-memory system-volume snapshot: replacing its `lsrt.lslib` dependency with either
the absent `none.lslib` or the staged but incompatible `wire.lslib` must return a
failed launch reply with no Process capability. The same gate rejects a drifted
canonical provider-order file. These checks run on x86_64, AArch64 and RISC-V.

The identity gate also substitutes the valid `wire.lslib` bytes into the staged
`lsrt.lslib` slot and independently corrupts `id/lib/lsrt`. Both mutations must
fail before ProcessService creates a Process capability. This binds a staged name,
its artifact bytes, its identity record and its direct provider chain into one
runtime-checked launch contract.

Every resolved provider closure has exactly one owner for each loader-visible
dynamic export. Package staging indexes defined global or weak `NOTYPE`, `OBJECT`
and `FUNC` symbols with default or protected visibility using the same eligibility
rules as the kernel loader. A duplicate owner rejects the graph before an artifact
is accepted; the loader repeats the check while adding a module's exports and rolls
back the module mapping on failure. A provider whose dynamic string table is
mutated to collide with a runtime export must therefore fail before ProcessService
returns a Process capability, even when its staged identity record and note remain
well-formed.

The package-stage static-image gate temporarily changes a staged dynamic executable's
ELF type to `ET_EXEC` on every supported target. The kernel package build must reject
that artifact before rewriting the target's system-volume archive; the gate then
restores the original bytes and verifies the prior archive hash returns unchanged.

The package-stage undeclared-edge gate similarly replaces an executable's declared
`lsrt.lslib` `DT_NEEDED` entry with the staged but undeclared `wire.lslib` entry.
The exact manifest comparison must reject it before archive output changes. At runtime,
ProcessService separately rejects the same mutated edge because the executable identity
record's provider digest chain does not match the resolved graph, and it returns no
Process capability.

The duplicate-edge gate changes a second `DT_NEEDED` value to the first provider's
string-table offset without altering the identity record or note. Package staging rejects
the repeated provider before archive output changes, while ProcessService rejects the
duplicate dependency name before resolving or loading modules.

The malformed dynamic-metadata gate independently injects a second `PT_DYNAMIC`
program header, removes every `DT_NULL` terminator, and duplicates the singleton
`DT_STRTAB` tag. The shared ELF parser, package staging and ProcessService must reject
each form before a Process capability is created. The package check restores the artifact
after every mutation and verifies that no failed form rewrites the system-volume archive.

The malformed symbol and relocation gate sets `DT_SYMENT` to a non-ELF64 size, makes
the SysV hash symbol count exceed the parser limit, and makes `DT_PLTRELSZ` indivisible
by the ELF64 RELA entry size. Package staging walks the complete bounded dynamic symbol
table in addition to relocation records, and ProcessService applies the same parser
rules before resolver traversal. Each mutation must fail without rewriting the archive
or creating a Process capability.

Package staging independently reconstructs each executable's complete provider closure
from the manifest and derives the lexicographically deterministic provider-before-
consumer topological order. The staged order sidecar must match that exact sequence, not
merely contain valid unique library names. The dependency-graph gate swaps two valid
sidecar entries and requires package rejection without archive changes. Its runtime half
changes `wire.lslib` to depend on itself; ProcessService's bounded DFS cycle guard must
reject the launch before loading a module or creating a Process capability.

The reconstructed closure and runtime resolver share hard admission bounds: at most 64
unique provider modules and dependency depths 0 through 15. Package staging propagates
the longest path reaching each provider, so a shallow occurrence cannot hide a second
path that exceeds the depth limit. Host boundary tests pin acceptance at 63 loaded
modules and depth 15, rejection at 64 loaded modules and depth 16, saturated hostile
counts, and active-path re-entry.

Governed command coverage includes a runnable representative from every migration wave.
The network wave uses `ip`: PermissionManager grants all eight network tools exactly one
fresh client minted through `network.open`, never a duplicate of the shared root request
channel. Each tool consumes the standard governed bootstrap sequence of arguments,
tagged grants and cwd. The focused scenario launches `ip` through PermissionManager,
answers its typed `network.info` request, verifies the complete rendered stdout and clean
exit, and requires an audit containing only the network grant. Existing focused scenarios
cover the runtime/inventory, storage/path, query/admin and multimedia waves.

`docs/DYNAMIC_EXECUTABLES.tsv` is the checked inventory of all dynamic command waves on
x86_64, AArch64 and RISC-V. Each row records the linked ELF's complete undefined import
set, manifest-declared providers, independently parsed `DT_NEEDED` set, canonical
transitive provider order, validated ET_REL object bytes, stripped PIE bytes, provider bytes, private image bytes,
immutable shared bytes and the governed test command. `docs/DYNAMIC_WAVES.tsv` aggregates
the same data with providers deduplicated inside each target/wave. Private image bytes are
the page-rounded writable `PT_LOAD` ranges of the executable plus every provider in its
closure; immutable ranges use the loader's shared-frame rule. `just dynamic-report-check` rebuilds
the three target graphs and requires the report to reproduce byte for byte. It also fails
if the five wave lists do not partition the manifest's tool inventory exactly, a direct
provider differs from `DT_NEEDED`, a sidecar omits a root, or an artifact/provider is
missing. `just dynamic-report-update` is the explicit regeneration command.

ET_REL attribution never selects the newest or only object matching a filename pattern.
The image builder atomically publishes one current-object record per accepted executable,
binding its exact object key, basename, SHA-256 and byte count. Cache hits validate or
restore that record from the independently validated object cache. The report checker
requires the referenced file and sidecars, recomputes its SHA-256, requires ELF `ET_REL`
and exactly one global `__user_main` definition, then records its size. Historical cache
objects may coexist without ambiguity. `docs/DYNAMIC_IMAGE.tsv` sums all 48 current
objects and PIE files per target, adds each provider file exactly once, and checks that
staged bytes equal PIE plus unique-provider bytes.

The detailed report also maps every undefined executable import to exactly one
loader-visible owner in that executable's canonical provider closure. The checker indexes
defined global or weak `NOTYPE`, `OBJECT` and `FUNC` exports with default or protected
visibility, matching the loader and linker ownership rules. Zero or multiple closure
owners fail the report even on a warm artifact hit. Private generated implementation
symbols and `ChannelClient` are forbidden in every `/bin` object.

Every `/bin` import now crosses a concrete provider or single-concern leaf boundary.
`du`, `ls`, `lsblk`, `lsvol`, `snap`, `volume`, `imgconv` and `imgview` route their
storage operations through `volume-client.lslib`; `imgview` also routes key subscription
through `surface.lslib`. The image builder pins each tool's required concrete entry points
and rejects private generated storage implementations, `ChannelClient`,
`ChannelTransport` and `VecWriter`. The checked report permits no generic transport
residual in any target/tool row.

Physical sharing is pinned across different executable graphs, not only repeated launches
of one binary. Canonical provider order places `volume-client.lslib` in slot 4 for both
`cat` and `write`, and `jpeg.lslib` in slot 5 for both `imgconv` and `imgview` on every
target. The dynamic gate holds each pair concurrently, closes both bootstraps, then
requires the corresponding provider virtual address in both process page tables to name
the same physical text frame. This proves domain-client and codec text reuse between
unrelated consumers while their writable mappings remain private.

## Measurement and optimization

Dynamic linking is the required `/bin` architecture, not an optional optimization gate.
`docs/PERF.md` records per-binary PIE size, direct/transitive provider bytes, system-volume
size, cold/warm launch latency, relocation/symbol-lookup cost, private pages and shared
resident frames. Measurements drive optimization of provider atomization, relocation
batching, symbol lookup/cache, page I/O and build profiles. They do not authorize static
copies of runtime, generated protocols or codecs inside utility executables.

The 2026-07-16 baseline is 48 static stripped tools totaling 11,885,992 bytes: ordinary
tools account for 6,202,912 bytes and `imgview`/`play`/`imgconv` for 5,683,080 bytes. The
existing shared runtime/protocol roots are being split further by ownership before the
complete tri-architecture executable conversion.
