# Unsafe boundaries in the kernel and userspace - the audit

Revision `f76cbb521695d707b6a777cd3af6306473bfaca7` (dirty (123 changed path(s))); rustc `rustc 1.93.1 (01f6ddf75 2026-02-11)`, userspace channel `nightly-2026-06-16`, kernel channel `nightly-2026-06-16`.

The machine-readable inventory is `AI/audit/unsafe-inventory.json` (schema `libersystem-unsafe-inventory/1`), rendered as `AI/audit/unsafe-inventory.md`; the auditor's decisions are `AI/audit/unsafe-classification.toml`; this report is rendered by `AI/audit/render-unsafe-boundaries.py`. Regenerate the inventory with `cd src/tools/unsafe-inventory && cargo run --quiet -- --repo ../../.. [--expand]`.

**The audit changes no production code.** It inventories, classifies and routes.

## 1. The matrix is derived, and the inventory mechanism

The rows are what the build scripts do, not what a label suggests. `build.sh` compiles the kernel with `cargo build` and no `--release`, so the SHIPPING KERNEL is Cargo's dev profile and `debug_assertions` is on in it - the frame allocator's poisoning and accounting among the code that ships. Static userspace (the eighteen statically linked programs' crates: `system_manager`, `services`, `storage`, `drivers`) is the dev profile too, with `development` enabled for `services` and `drivers` only under `LIBER_DEVELOPMENT=1`, which is its own row. Shared-image userspace is `build-shared.sh`'s configuration: release, the custom `x86_64-unknown-none.json` target (or the bare-metal triples), `-Z build-std`, `-C relocation-model=pic`, every staged library with its manifest row's feature set and every dynamically linked program with `--no-default-features --features shared-image` where its crate defines that feature. The shipped WASM component is `liber_component` for `wasm32-unknown-unknown`, release, default features; its `dev-diagnostics` build is a development row. The three loaders are shipped code and are rows too, although the plan's list did not name them.

| row | kind | target | profile | crates | files | third-party crates excluded |
| --- | --- | --- | --- | ---: | ---: | --- |
| `kernel-x86_64` | shipping | `x86_64-unknown-none` | dev | 17 | 226 | - |
| `kernel-aarch64` | shipping | `aarch64-unknown-none` | dev | 17 | 226 | - |
| `kernel-riscv64` | shipping | `riscv64gc-unknown-none-elf` | dev | 17 | 226 | - |
| `static-user-x86_64` | shipping | `src/user/x86_64-unknown-none.json` | dev | 41 | 266 | - |
| `static-user-aarch64` | shipping | `aarch64-unknown-none` | dev | 41 | 266 | - |
| `static-user-riscv64` | shipping | `riscv64gc-unknown-none-elf` | dev | 41 | 266 | - |
| `development-user-x86_64` | development | `src/user/x86_64-unknown-none.json` | dev | 32 | 241 | - |
| `shared-image-x86_64` | shipping | `src/user/x86_64-unknown-none.json` | release | 93 | 461 | ai-image-webp 0.2.4, jpeg-encoder 0.7.1, libm 0.2.16, miniz_oxide 0.9.1, nanomp3 0.1.1, no_std_io 0.6.0, qoi 0.4.1, tinyvec 1.12.0, weezl 0.2.1, zune-core 0.5.3, zune-jpeg 0.5.15 |
| `shared-image-aarch64` | shipping | `aarch64-unknown-none` | release | 93 | 461 | ai-image-webp 0.2.4, jpeg-encoder 0.7.1, libm 0.2.16, miniz_oxide 0.9.1, nanomp3 0.1.1, no_std_io 0.6.0, qoi 0.4.1, tinyvec 1.12.0, weezl 0.2.1, zune-core 0.5.3, zune-jpeg 0.5.15 |
| `shared-image-riscv64` | shipping | `riscv64gc-unknown-none-elf` | release | 93 | 461 | ai-image-webp 0.2.4, jpeg-encoder 0.7.1, libm 0.2.16, miniz_oxide 0.9.1, nanomp3 0.1.1, no_std_io 0.6.0, qoi 0.4.1, tinyvec 1.12.0, weezl 0.2.1, zune-core 0.5.3, zune-jpeg 0.5.15 |
| `wasm-component` | shipping | `wasm32-unknown-unknown` | release | 2 | 6 | - |
| `wasm-component-dev-diagnostics` | development | `wasm32-unknown-unknown` | release | 2 | 6 | - |
| `loader-x86_64` | shipping | `x86_64-unknown-uefi` | dev | 9 | 63 | ed25519-dalek 2.2.0, sha2 0.10.9 |
| `loader-aarch64` | shipping | `aarch64-unknown-uefi` | dev | 9 | 63 | ed25519-dalek 2.2.0, sha2 0.10.9 |
| `loader-riscv64` | shipping | `riscv64gc-unknown-none-elf` | dev | 9 | 63 | ed25519-dalek 2.2.0, sha2 0.10.9 |

Each row's exact build command, `cfg` set, rustflags, environment and crate closure are recorded in the JSON.

**The mechanism.** For each row, `cargo metadata` (offline, with the row's features) gives the resolve graph; the in-tree closure is every package reachable from the row's roots through NORMAL dependencies whose manifest lies inside the repository - build dependencies are host code whose OUTPUT is read, dev dependencies are test-only, and registry crates are excluded and named. Every crate root and every module file it reaches (`mod x;`, `#[path]`) is parsed with `syn`; `#[cfg(...)]` and `cfg_attr` predicates on items, fields, statements and expressions are evaluated against the row's `cfg` set plus the crate's RESOLVED features from the same metadata, so a site behind `cfg(target_arch = "riscv64")` is present in every row and reachable only in the riscv64 ones, and a site behind `cfg(test)` is test-only in every shipping row. Files a build script wrote are read through `include!(concat!(env!("OUT_DIR"), ...))` from the row's built `OUT_DIR` (the kernel's `dma_registry.rs`, the services' generated role tables) with provenance recorded; a generated file that cannot be read is a problem on the row, never a silent omission. A `macro_rules!` whose template carries a boundary is a MACRO-TEMPLATE site with its own-crate invocation count. Every site has a stable id - SHA-256 of crate, file, kind, item path and ordinal within the item - so an unrelated edit does not renumber the audit.

**Expansion reconciliation** (`--expand`): for each crate of a row the tool runs `cargo rustc ... -- -Zunpretty=expanded` under the row's target, profile, features and `build-std`, parses the cfg- and macro-expanded crate with the same scanner, and compares per-kind counts with the reachable source sites. Compiler-derived impls (`#[automatically_derived]`, which on this toolchain emit `unsafe impl TrivialClone` for every `derive(Clone, Copy)`) are excluded on both sides. What the comparison surfaces is exactly what a source scan cannot: a site a macro produced, or a source site the compiler did not keep. Its results for the x86_64 kernel, loader and WASM rows are in section 6.

**What the mechanism does not claim.** A `cfg` predicate the row does not list is FALSE (`target_has_atomic`, `target_feature` and the like are listed where they matter); a proc-macro's output is seen only through expansion; the reconciliation compares counts per kind and crate, not sites one by one; and nothing here executes privileged assembly, MMIO or DMA - the falsification evidence named per boundary is the guest suites and gates that do.

## 2. Totals

| sites | reachable in a shipping row | test-only | generated | macro templates | present in no row | unclassified |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 5736 | 4779 | 936 | 0 | 34 | 0 | 0 |

| category | sites |
| --- | ---: |
| abi-linkage | 489 |
| contained-implementation | 2294 |
| overly-broad | 1937 |
| required-caller-enforced | 1016 |

Reachable in a shipping row, by category: abi-linkage 489, contained-implementation 1358, overly-broad 1937, required-caller-enforced 1016.

"Generated 0" is a measured fact, not a gap: the generated files this tree includes (the kernel's DMA registry, the services' bootstrap role tables and generated dispatch) carry data and safe code, and the tool's own fixture proves a generated `unsafe fn` would be found with its provenance.

## 3. The kernel, by trust boundary

Each boundary below is a group in the classification; the count is its reachable sites, the rows are the configurations that compile it, and the evidence is what would falsify the invariant if it were wrong.

| boundary | sites | rows | kinds | invariant | who establishes it | falsified by |
| --- | ---: | --- | --- | --- | --- | --- |
| Usercopy fixups (`kernel-usercopy`) | 30 | `kernel-aarch64`, `kernel-riscv64`, `kernel-x86_64` | raw-pointer 12, inline-asm 6, unsafe-block 6, unsafe-fn 6 | the kernel-side buffer is valid for `len` | the syscall layer, whose buffers are fixed-size ABI records on the kernel stack | the `kernel.kernel` syscall tests that pass an unmapped or partially mapped user buffer and expect `ERR_NOT_MAPPED`; a kernel fault in `.extable` code would be a panic in the guest log |
| Syscall user-pointer handling (`kernel-syscall-user-pointers`) | 12 | `kernel-aarch64`, `kernel-riscv64`, `kernel-x86_64` | raw-pointer 8, unsafe-block 2, extern-abi-fn 1, linkage-attr 1 | every user pointer is range-checked before the faultable copy and a short copy is an error | the syscall dispatcher | the same syscall tests; the handle-count baselines of the launch fixtures |
| The generic `read_user<T>` (`kernel-syscall-read-user`) | 5 | `kernel-aarch64`, `kernel-riscv64`, `kernel-x86_64` | unsafe-block 3, raw-pointer 2 | every `T` it is instantiated with has no invalid bit pattern (u64, `[u8; 64]`, `CapTransfer`) | nobody in the signature - the auditor, by reading the four call sites | a compile error once M3 of P02M0178 bounds `T`; today only review |
| Page tables (the `unsafe fn` contracts) (`kernel-arch-paging`) | 17 | `kernel-aarch64`, `kernel-riscv64`, `kernel-x86_64` | unsafe-fn 17 | a table pointer is a valid table at its level, owned by the address space under its lock | the address-space object that owns the root and the callers that hold its lock | `kernel.kernel` mapping and shootdown tests, the frame accounting tests, the `smp-core-cap` gate |
| Page tables (walks, TLB and control registers) (`kernel-arch-paging-internals`) | 183 | `kernel-aarch64`, `kernel-riscv64`, `kernel-x86_64` | unsafe-block 100, raw-pointer 50, inline-asm 29, extern-block 4 | the walk stays inside tables the caller vouched for; invalidation follows every unmap | the paging module itself | as above |
| Ring transitions and context switches (`kernel-arch-usermode-and-context`) | 93 | `kernel-aarch64`, `kernel-riscv64`, `kernel-x86_64` | unsafe-block 31, inline-asm 18, global-asm 16, raw-pointer 8, extern-block 7, unsafe-fn 5, linkage-attr 4, extern-abi-fn 4 | the saved-frame layout and calling convention the assembly defines; `enter` receives user-mapped addresses | the scheduler and the loader, which built the stack and the address space | every user program that runs; the `usermode` fault tests (`ud2`, divide, NX) in the kernel suite |
| Interrupt controllers (`kernel-arch-interrupt-controllers`) | 125 | `kernel-aarch64`, `kernel-riscv64`, `kernel-x86_64` | unsafe-block 54, raw-pointer 36, inline-asm 19, extern-abi-fn 9, macro-template 4, unsafe-fn 2, static-mut 1 | MMIO to addresses the device tree or firmware named, behind the controller lock; the vector tables are written before interrupts are enabled | the arch bring-up | the `arch-profile-*` gates over GICv2/v3/ITS and AIA, the MSI oracles, `qemu-numa` |
| Per-CPU blocks and SMP bring-up (`kernel-arch-percpu-and-smp`) | 166 | `kernel-aarch64`, `kernel-riscv64`, `kernel-x86_64` | unsafe-block 80, raw-pointer 40, inline-asm 21, unsafe-fn 6, extern-abi-fn 4, linkage-attr 4, extern-block 3, static-mut 3, global-asm 3, unsafe-impl 2 | a CPU index is below the online count the census established (the 64-CPU portable cap and lower backend caps); each CPU touches its own slot | the CPU census, before any secondary starts | the `smp-core-cap` gate (a census above the cap is refused), the `kernel.kernel` scheduler and cross-core wake tests |
| The AArch64 bring-up block driver (`kernel-arch-early-virtio-blk-aarch64`) | 30 | `kernel-aarch64` | unsafe-block 13, raw-pointer 8, unsafe-fn 8, inline-asm 1 | its rings and buffers are its own frames; the device sees physical addresses because no IOMMU is initialised yet | the aarch64 boot path, deliberately before the device manager | not falsifiable by a test today - it IS the untranslated access P02M0173 M3 requires to move behind confirmed IOMMU initialisation or off the enforcing path (family work, recorded there) |
| Early boot and the arch modules (`kernel-arch-boot`) | 191 | `kernel-aarch64`, `kernel-riscv64`, `kernel-x86_64` | unsafe-block 107, raw-pointer 40, inline-asm 31, unsafe-fn 3, extern-abi-fn 2, static-mut 2, extern-block 2, global-asm 2, linkage-attr 2 | single-threaded on the boot CPU, reading memory the loader or the device tree described, before the allocator exists | the loader's `BootInfo` contract | every boot; the loader-to-kernel contract tests in `bootproto`; the DMA-mode carrier and arch-profile gates |
| Port I/O and fw_cfg (x86_64) (`kernel-arch-port-io-and-fwcfg`) | 31 | `kernel-aarch64`, `kernel-riscv64`, `kernel-x86_64` | unsafe-block 15, unsafe-fn 7, inline-asm 5, raw-pointer 4 | a fixed legacy port is driven by the module that owns it | the console, PCI configuration and the carrier reader | the `dma-mode-x86_64` gate reads the carrier through it; the PCI census tests |
| Device-tree parsing (`fdt`) (`kernel-dtb-parsing`) | 108 | `kernel-aarch64`, `kernel-riscv64`, `kernel-x86_64`, `loader-aarch64`, `loader-riscv64`, `loader-x86_64` | unsafe-block 64, unsafe-fn 43, raw-pointer 1 | `Fdt::new` requires a valid tree at the address; every read is bounded by the header's declared sizes | the loader and the boot path that hand the address over | the `fdt` crate's host tests over the QEMU fixture trees and the malformed shapes; the `dma-mode-carrier` gate |
| The virtio-IOMMU backend (`kernel-iommu`) | 60 | `kernel-aarch64`, `kernel-riscv64`, `kernel-x86_64` | unsafe-block 33, raw-pointer 16, unsafe-fn 11 | queues and scratch are pinned frames the module owns; every access carries a SAFETY line with the bound | the module, at bring-up | the hostile EDU gate (`qemu-virtio-iommu-x86_64`), the fault-drain and malformed-completion host tests |
| Heap, frames and TLB (`kernel-mem`) | 34 | `kernel-aarch64`, `kernel-riscv64`, `kernel-x86_64` | unsafe-block 18, unsafe-fn 8, raw-pointer 7, unsafe-impl 1 | the allocator's: a region returned was handed out here and is unreferenced; frames are retired once | the allocator | the frame accounting and poisoning tests (which run in the shipping profile), the shootdown tests |
| Locks (`kernel-sync`) | 8 | `kernel-aarch64`, `kernel-riscv64`, `kernel-x86_64` | unsafe-impl 3, unsafe-block 3, raw-pointer 2 | a spin lock hands out one `&mut T` at a time | the lock | every test that runs on more than one core |
| Scheduler, threads and objects (`kernel-sched-and-objects`) | 22 | `kernel-aarch64`, `kernel-riscv64`, `kernel-x86_64` | unsafe-block 17, raw-pointer 5 | per-CPU run queues indexed by the current CPU; a thread stack owns its frames for its life; the idle hook is the `fn()` it was stored from | the scheduler | the `kernel.kernel` scheduler, preemption, stack-guard and object tests |
| Kernel entry, program loading and PCI (`kernel-main-loader-device`) | 27 | `kernel-aarch64`, `kernel-riscv64`, `kernel-x86_64` | raw-pointer 12, unsafe-block 11, extern-abi-fn 2, linkage-attr 1, unsafe-fn 1 | writes into a freshly created address space through kernel mappings the loader owns; MMIO to BARs the census resolved | the object layer | the launch and claim tests, the `dma` and `pci` suites |
| Other kernel sites (`kernel-remaining`) | 41 | `kernel-aarch64`, `kernel-riscv64`, `kernel-x86_64` | unsafe-block 23, raw-pointer 8, unsafe-fn 4, inline-asm 3, extern-block 2, global-asm 1 | reviewed per file as contained | each module | the kernel suite |

`unsafe impl Send`/`Sync` in the kernel: `SpinLock<T>` (both, for `T: Send`), `Console: Send`, and the AArch64 trap-stack `TrapStacks`/`Pool: Sync` - each rests on the exclusion argument in its group above. The kernel's `static mut` items reachable in a shipping row are the IDT and the BSP's CPU area on x86_64 (written before interrupts), the AArch64 secondary stacks and the boot probes (single-threaded boot), and the riscv64 secondary-stack pointer; the trace ring is `cfg(test)`. The `transmute` of the idle hook restores a `fn()` from the integer it was stored as.

## 4. Userspace, by cohort

| cohort | sites | rows | kinds | judgement |
| --- | ---: | --- | --- | --- |
| Runtime: the raw syscall (`rt-raw-syscall`) | 9 | `development-user-x86_64`, `shared-image-aarch64`, `shared-image-riscv64`, `shared-image-x86_64`, `static-user-aarch64`, `static-user-riscv64`, `static-user-x86_64` | unsafe-block 3, inline-asm 3, unsafe-fn 3 | required |
| Runtime: address-returning wrappers (`rt-address-returning`) | 23 | `development-user-x86_64`, `shared-image-aarch64`, `shared-image-riscv64`, `shared-image-x86_64`, `static-user-aarch64`, `static-user-riscv64`, `static-user-x86_64` | unsafe-block 10, unsafe-fn 9, raw-pointer 4 | required |
| Runtime: propagated wrappers (`rt-propagated-wrappers`) | 304 | `development-user-x86_64`, `shared-image-aarch64`, `shared-image-riscv64`, `shared-image-x86_64`, `static-user-aarch64`, `static-user-riscv64`, `static-user-x86_64` | unsafe-block 142, unsafe-fn 129, raw-pointer 30, inline-asm 3 | overly broad - FINDING F1 |
| Runtime: allocator, streams, entry and forwarding stubs (`rt-heap-stream-and-linkage`) | 74 | `development-user-x86_64`, `shared-image-aarch64`, `shared-image-riscv64`, `shared-image-x86_64`, `static-user-aarch64`, `static-user-riscv64`, `static-user-x86_64` | linkage-attr 27, unsafe-block 14, extern-abi-fn 12, unsafe-fn 7, global-asm 6, raw-pointer 5, unsafe-impl 2, extern-block 1 | ABI |
| Drivers: MMIO and DMA (`drivers-mmio-and-dma`) | 287 | `development-user-x86_64`, `static-user-aarch64`, `static-user-riscv64`, `static-user-x86_64` | unsafe-block 169, raw-pointer 118 | contained |
| Drivers: propagated wrappers (`drivers-propagated-wrappers`) | 159 | `development-user-x86_64`, `static-user-aarch64`, `static-user-riscv64`, `static-user-x86_64` | unsafe-fn 159 | overly broad - F1 |
| Drivers: entry ABI (`drivers-entry-linkage`) | 16 | `development-user-x86_64`, `static-user-aarch64`, `static-user-riscv64`, `static-user-x86_64` | extern-abi-fn 8, linkage-attr 8 | ABI |
| Services: mapped-object views and the display/audio engines (`services-mapped-views`) | 124 | `development-user-x86_64`, `shared-image-aarch64`, `shared-image-riscv64`, `shared-image-x86_64`, `static-user-aarch64`, `static-user-riscv64`, `static-user-x86_64` | unsafe-block 84, raw-pointer 31, unsafe-fn 7, linkage-attr 1, extern-abi-fn 1 | contained |
| Services: wrapped runtime calls (`services-wrapped-runtime-calls`) | 984 | `development-user-x86_64`, `shared-image-aarch64`, `shared-image-riscv64`, `shared-image-x86_64`, `static-user-aarch64`, `static-user-riscv64`, `static-user-x86_64` | unsafe-block 720, unsafe-fn 264 | overly broad - F1 |
| Services: entry ABI (`services-entry-linkage`) | 58 | `development-user-x86_64`, `shared-image-aarch64`, `shared-image-riscv64`, `shared-image-x86_64`, `static-user-aarch64`, `static-user-riscv64`, `static-user-x86_64` | linkage-attr 29, extern-abi-fn 29 | ABI |
| Libraries: the shared-image provider ABI (`libs-shared-image-exports`) | 179 | 10 rows | linkage-attr 124, macro-template 30, extern-block 12, global-asm 12, extern-abi-fn 1 | ABI |
| Libraries: IPC client and driver protocol (`libs-ipc-and-driver-protocol`) | 17 | 10 rows | unsafe-block 9, raw-pointer 5, unsafe-fn 3 | contained |
| Libraries: wrapped runtime calls (`libs-wrapped-runtime-calls`) | 92 | `development-user-x86_64`, `shared-image-aarch64`, `shared-image-riscv64`, `shared-image-x86_64`, `static-user-aarch64`, `static-user-riscv64`, `static-user-x86_64` | unsafe-block 90, unsafe-fn 2 | overly broad - F1 |
| Applications: mapped-file views (`apps-mapped-views`) | 14 | `shared-image-aarch64`, `shared-image-riscv64`, `shared-image-x86_64` | raw-pointer 14 | contained |
| Applications: wrapped runtime calls (`apps-wrapped-runtime-calls`) | 393 | `shared-image-aarch64`, `shared-image-riscv64`, `shared-image-x86_64` | unsafe-block 258, unsafe-fn 132, global-asm 3 | overly broad - F1 |
| Applications: entry ABI (`apps-entry-linkage`) | 154 | `shared-image-aarch64`, `shared-image-riscv64`, `shared-image-x86_64` | linkage-attr 77, extern-abi-fn 77 | ABI |
| SDK and the WASM component (`sdk-component-boundary`) | 8 | `wasm-component`, `wasm-component-dev-diagnostics` | extern-abi-fn 3, linkage-attr 3, extern-block 2 | ABI |
| Terminal raster (`Raster::new`) (`term-raster`) | 1 | 10 rows | unsafe-fn 1 | required |
| Terminal raster (pixel access) (`term-raster-internals`) | 8 | 10 rows | raw-pointer 4, unsafe-block 4 | contained |
| Boot: loader, UEFI layer, boot protocol (`loader-firmware-and-handoff`) | 704 | 13 rows | raw-pointer 329, unsafe-block 283, unsafe-fn 35, inline-asm 33, static-mut 18, unsafe-impl 2, extern-abi-fn 2, global-asm 1, linkage-attr 1 | required |
| Shared crates (dma, wire) (`shared-crates-misc`) | 9 | 10 rows | unsafe-block 4, extern-block 2, raw-pointer 2, unsafe-fn 1 | contained |

**Runtime.** `rt::syscall` is the one genuinely unsafe entry: inline assembly whose argument contract is the kernel's. Twenty-three wrappers hand the caller a raw address (`map_object`, `dma_buffer_map`, `dma_buffer_phys*`, `framebuffer_map`, `memmap_get`, the module loader) and are rightly `unsafe fn`. The remaining wrappers - 304 sites in the runtime's own crate - are `unsafe fn` by propagation only: they take handles, slices and values, pass a slice's own pointer and length, and cannot be misused from the caller's side. The allocator (`GlobalAlloc`), the stream reader and the `memcpy`/`memset` forwarding stubs of the shared-image build are the language's and the linker's ABI.

**Drivers.** The device boundary is real and contained: registers are reached through the MMIO span the kernel mapped for the claim, rings through `DmaAddress`-backed buffers, with volatile access and the barriers the virtio and xHCI specifications require; the raw pointers name those spans. The enforcing-IOMMU gate is the falsifier (a physical address in a descriptor faults). The drivers' shared handshake layer and per-device bring-up functions are `unsafe fn` by propagation from the runtime.

**Services.** 984 sites wrap runtime calls that pass handles and slices; none dereferences memory. The raw pointers that exist (124 sites, the display and audio engines included) are views over MemoryObjects the kernel mapped whole at a length the same service or the kernel reported, or writes into DMA-visible buffers a driver published for that purpose. `DeviceManager` and `ConsoleService` carry the most propagation (over sixty `unsafe fn` each).

**Libraries.** The shared-image provider ABI is generated: `#[unsafe(export_name)]` wrappers in every protocol crate, thirty `forward!` templates (one per provider crate and architecture) whose `global_asm!` aliases an exported symbol to its implementation, and the `extern` blocks a client crate declares for its provider. The package identity, export-owner and provider-closure checks audit that surface; it is ABI, not raw memory. Client libraries' remaining blocks are propagation.

**Applications.** Seventy-five `extern "C" fn __user_main` entry points (ABI), fourteen mapped-file views (`from_raw_parts` over a file the storage service mapped whole), and propagation everywhere else.

**The component boundary, both sides.** The SDK declares its host imports as `unsafe extern "C"` and exposes a safe API over them; `liber_component` exports `run`, `score` and `panic_now` with `#[unsafe(no_mangle)] extern "C"`. The interpreter's side (`src/wasm`) contains NO unsafe code: imports are dispatched by name and every argument is validated by the host before it reaches a service.

**Boot.** The loader and the UEFI layer are a firmware-ABI boundary end to end: protocol and table pointers the firmware handed the image entry, used while boot services are live and never after; the memory map, ELF placement and hand-off write physical memory the firmware's map declared free; every `static mut` is reached only from the single-threaded boot path and says so where it is declared. `bootproto` and `fdt` are shared with the kernel and host-tested.

## 5. Findings, by severity, and where each goes

| id | severity | finding | sites | owner | disposition |
| --- | --- | --- | ---: | --- | --- |
| F1 | medium (the safety story, not soundness) | `unsafe` in userspace is propagation from the runtime's wrapper signatures: 1932 reachable sites carry an `unsafe` that names no caller obligation, and the dozen real raw-memory boundaries are indistinguishable from them by reading | 1932 | rt, then every cohort | **P02M0178** (M1, M2, M4): safe signatures where the body upholds the contract, the propagation removed downstream, the inventory regenerated to prove it |
| F2 | low (latent) | the kernel's safe `read_user<T>` produces any `T` from user bytes; sound for its four plain-data instantiations, unable to refuse a future `T` with invalid bit patterns | 5 | kernel/syscall | **P02M0178 M3**: bound `T` to plain-old-data or make the function `unsafe` with the contract written |
| F3 | informational (family) | the AArch64 bring-up block driver masters the bus untranslated before the IOMMU exists - by design under the ports' produced `no-iommu` mode, and the boundary P02M0173 M3 already requires to move | 30 | kernel/arch/aarch64 | **P02M0173 M3** (attached to the family item, as M4 permits for planned family work) |

**Confirmed unsoundness or material defect in shipped code: none found in this pass.** What was checked to say so: every `unsafe impl Send`/`Sync` (kernel `SpinLock`, `Console`, the AArch64 trap-stack pools; the runtime's locked heap) against its exclusion argument; every reachable `static mut` against the single-threaded or before-interrupts context it is written in; the usercopy `.extable` fixups and the syscall layer's short-copy handling; the `transmute` of the idle hook; every `from_raw_parts` view in services and tools against the length the kernel mapped; the drivers' descriptor writes against the enforcing-IOMMU gate; the four `read_user` instantiations; the loader's firmware-table lifetimes against `ExitBootServices`. A finding of this class would have got its own numbered corrective item rather than a place on an index, as M4 requires.

## 6. Expansion reconciliation

Recorded from `--expand` over the x86_64 kernel row with compiler-derived impls excluded (28 crates expanded and compared, no expansion failure). Twenty-seven crates reconciled EXACTLY per kind. The kernel crate itself differs in four kinds, all HIGHER in the expansion: `extern-abi-fn` 14 in the source against 248 expanded - the `generic_vectors!`, `generic_vectors_with_code!`, `irq_stub!` and `msi_stubs!` templates in `arch/x86_64/idt` and `interrupts`, which stamp out the interrupt entry stubs and are recorded as four macro-template sites with their invocation counts - and `unsafe-fn` +4, `unsafe-block` +3 and `raw-pointer` +5 from the same templates' bodies. NOTHING WAS LOWER: no reachable source site was dropped by the compiler under the row's cfg set, which is the check that the source-level cfg evaluation agrees with rustc's. An earlier run without the derived-impl exclusion reported `unsafe impl` counts in every crate; they were all `#[automatically_derived] unsafe impl TrivialClone`, the toolchain's own derive output, which is why the exclusion exists.

## 7. Completeness fixtures

`src/tools/unsafe-inventory/fixtures` holds one crate with a cfg-only site, a macro-produced site, a generated site (a build script writing an `unsafe fn` into `OUT_DIR`), a test-only site and an ordinary baseline, and a second in-tree crate reached only as a dependency. The tool's tests (`cargo test` in `src/tools/unsafe-inventory`, 8 tests) assert each class is found with its reachability and provenance, that a missing `OUT_DIR` is reported rather than silent, that the dependency is in the closure and a registry crate would not be, that ids are stable, and that `cfg` predicates evaluate as rustc would. A class of site that stopped being found fails there.

## 8. What this audit did not do

It changed no production code and set no target count; it did not audit third-party crates (listed per row and excluded); it does not claim Miri or host tests exercised privileged assembly, MMIO or DMA - the falsification column names the guest gates that do; and it does not propose a blanket conversion. P02M0178 is the one corrective item it opened.

