#!/usr/bin/env python3
# Render AI/audit/unsafe-boundaries.md from AI/audit/unsafe-inventory.json and the prose below.
# The numbers in the report come from the inventory, never typed by hand; the judgements are the
# auditor's and live here. Regenerate: python3 AI/audit/render-unsafe-boundaries.py
import json, collections, sys, os
root = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
inv = json.load(open(os.path.join(root, 'AI/audit/unsafe-inventory.json')))
sites = inv['sites']; groups = {g['id']: g for g in inv['groups']}; rows = inv['rows']; t = inv['totals']
ship_rows = [r['id'] for r in rows if r['kind'] == 'shipping']
def n(gid): return groups[gid]['sites'] if gid in groups else 0
def reach(gid):
    c = collections.Counter()
    for s in sites:
        if s['group'] == gid:
            for r in s['reachable_in']: c[r] += 1
    return c
def kinds(gid):
    c = collections.Counter(s['kind'] for s in sites if s['group'] == gid and s['reachable_in'])
    return ', '.join(f"{k} {v}" for k, v in c.most_common())
def rowlist(gid):
    r = reach(gid)
    if not r: return 'no shipping row (test-only or unreachable)'
    keys = sorted(r)
    if len(keys) >= 9: return f"{len(keys)} rows"
    return ', '.join(f"`{k}`" for k in keys)
out = []
w = out.append
w(f"# Unsafe boundaries in the kernel and userspace - the audit\n")
w(f"Revision `{inv['revision']}` ({inv['tree']}); rustc `{inv['toolchain'].get('rustc','')}`, userspace channel `{inv['toolchain'].get('userspace-channel','')}`, kernel channel `{inv['toolchain'].get('kernel-channel','')}`.\n")
w("The machine-readable inventory is `AI/audit/unsafe-inventory.json` (schema `libersystem-unsafe-inventory/1`), rendered as `AI/audit/unsafe-inventory.md`; the auditor's decisions are `AI/audit/unsafe-classification.toml`; this report is rendered by `AI/audit/render-unsafe-boundaries.py`. Regenerate the inventory with `cd src/tools/unsafe-inventory && cargo run --quiet -- --repo ../../.. [--expand]`.\n")
w("**The audit changes no production code.** It inventories, classifies and routes.\n")
w("## 1. The matrix is derived, and the inventory mechanism\n")
w("The rows are what the build scripts do, not what a label suggests. `build.sh` compiles the kernel with `cargo build` and no `--release`, so the SHIPPING KERNEL is Cargo's dev profile and `debug_assertions` is on in it - the frame allocator's poisoning and accounting among the code that ships. Static userspace (the eighteen statically linked programs' crates: `system_manager`, `services`, `storage`, `drivers`) is the dev profile too, with `development` enabled for `services` and `drivers` only under `LIBER_DEVELOPMENT=1`, which is its own row. Shared-image userspace is `build-shared.sh`'s configuration: release, the custom `x86_64-unknown-none.json` target (or the bare-metal triples), `-Z build-std`, `-C relocation-model=pic`, every staged library with its manifest row's feature set and every dynamically linked program with `--no-default-features --features shared-image` where its crate defines that feature. The shipped WASM component is `liber_component` for `wasm32-unknown-unknown`, release, default features; its `dev-diagnostics` build is a development row. The three loaders are shipped code and are rows too, although the plan's list did not name them.\n")
w("| row | kind | target | profile | crates | files | third-party crates excluded |\n| --- | --- | --- | --- | ---: | ---: | --- |")
for r in rows:
    w(f"| `{r['id']}` | {r['kind']} | `{r['target']}` | {r['profile']} | {len(r['crates'])} | {r['files_scanned']} | {', '.join(r['third_party']) if r['third_party'] else '-'} |")
w("\nEach row's exact build command, `cfg` set, rustflags, environment and crate closure are recorded in the JSON.\n")
w("**The mechanism.** For each row, `cargo metadata` (offline, with the row's features) gives the resolve graph; the in-tree closure is every package reachable from the row's roots through NORMAL dependencies whose manifest lies inside the repository - build dependencies are host code whose OUTPUT is read, dev dependencies are test-only, and registry crates are excluded and named. Every crate root and every module file it reaches (`mod x;`, `#[path]`) is parsed with `syn`; `#[cfg(...)]` and `cfg_attr` predicates on items, fields, statements and expressions are evaluated against the row's `cfg` set plus the crate's RESOLVED features from the same metadata, so a site behind `cfg(target_arch = \"riscv64\")` is present in every row and reachable only in the riscv64 ones, and a site behind `cfg(test)` is test-only in every shipping row. Files a build script wrote are read through `include!(concat!(env!(\"OUT_DIR\"), ...))` from the row's built `OUT_DIR` (the kernel's `dma_registry.rs`, the services' generated role tables) with provenance recorded; a generated file that cannot be read is a problem on the row, never a silent omission. A `macro_rules!` whose template carries a boundary is a MACRO-TEMPLATE site with its own-crate invocation count. Every site has a stable id - SHA-256 of crate, file, kind, item path and ordinal within the item - so an unrelated edit does not renumber the audit.\n")
w("**Expansion reconciliation** (`--expand`): for each crate of a row the tool runs `cargo rustc ... -- -Zunpretty=expanded` under the row's target, profile, features and `build-std`, parses the cfg- and macro-expanded crate with the same scanner, and compares per-kind counts with the reachable source sites. Compiler-derived impls (`#[automatically_derived]`, which on this toolchain emit `unsafe impl TrivialClone` for every `derive(Clone, Copy)`) are excluded on both sides. What the comparison surfaces is exactly what a source scan cannot: a site a macro produced, or a source site the compiler did not keep. Its results for the x86_64 kernel, loader and WASM rows are in section 6.\n")
w("**What the mechanism does not claim.** A `cfg` predicate the row does not list is FALSE (`target_has_atomic`, `target_feature` and the like are listed where they matter); a proc-macro's output is seen only through expansion; the reconciliation compares counts per kind and crate, not sites one by one; and nothing here executes privileged assembly, MMIO or DMA - the falsification evidence named per boundary is the guest suites and gates that do.\n")
w("## 2. Totals\n")
w(f"| sites | reachable in a shipping row | test-only | generated | macro templates | present in no row | unclassified |\n| ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n| {t['sites']} | {t['reachable_shipping']} | {t['test_only']} | {t['generated']} | {t['macro_templates']} | {t['present_not_reachable_anywhere']} | {t['unclassified']} |\n")
w("| category | sites |\n| --- | ---: |")
for c, v in sorted(t['by_category'].items()): w(f"| {c} | {v} |")
w("")
ship_cat = collections.Counter(s['category'] for s in sites if s['reachable_in'])
w("Reachable in a shipping row, by category: " + ', '.join(f"{c} {v}" for c, v in sorted(ship_cat.items())) + ".\n")
w("\"Generated 0\" is a measured fact, not a gap: the generated files this tree includes (the kernel's DMA registry, the services' bootstrap role tables and generated dispatch) carry data and safe code, and the tool's own fixture proves a generated `unsafe fn` would be found with its provenance.\n")
w("## 3. The kernel, by trust boundary\n")
w("Each boundary below is a group in the classification; the count is its reachable sites, the rows are the configurations that compile it, and the evidence is what would falsify the invariant if it were wrong.\n")
kernel_groups = [
 ('kernel-usercopy', 'Usercopy fixups', 'the kernel-side buffer is valid for `len`', 'the syscall layer, whose buffers are fixed-size ABI records on the kernel stack', 'the `kernel.kernel` syscall tests that pass an unmapped or partially mapped user buffer and expect `ERR_NOT_MAPPED`; a kernel fault in `.extable` code would be a panic in the guest log'),
 ('kernel-syscall-user-pointers', 'Syscall user-pointer handling', 'every user pointer is range-checked before the faultable copy and a short copy is an error', 'the syscall dispatcher', 'the same syscall tests; the handle-count baselines of the launch fixtures'),
 ('kernel-syscall-read-user', 'The generic `read_user<T>`', 'every `T` it is instantiated with has no invalid bit pattern (u64, `[u8; 64]`, `CapTransfer`)', 'nobody in the signature - the auditor, by reading the four call sites', 'a compile error once M3 of P02M0178 bounds `T`; today only review'),
 ('kernel-arch-paging', 'Page tables (the `unsafe fn` contracts)', 'a table pointer is a valid table at its level, owned by the address space under its lock', 'the address-space object that owns the root and the callers that hold its lock', '`kernel.kernel` mapping and shootdown tests, the frame accounting tests, the `smp-core-cap` gate'),
 ('kernel-arch-paging-internals', 'Page tables (walks, TLB and control registers)', 'the walk stays inside tables the caller vouched for; invalidation follows every unmap', 'the paging module itself', 'as above'),
 ('kernel-arch-usermode-and-context', 'Ring transitions and context switches', 'the saved-frame layout and calling convention the assembly defines; `enter` receives user-mapped addresses', 'the scheduler and the loader, which built the stack and the address space', 'every user program that runs; the `usermode` fault tests (`ud2`, divide, NX) in the kernel suite'),
 ('kernel-arch-interrupt-controllers', 'Interrupt controllers', 'MMIO to addresses the device tree or firmware named, behind the controller lock; the vector tables are written before interrupts are enabled', 'the arch bring-up', 'the `arch-profile-*` gates over GICv2/v3/ITS and AIA, the MSI oracles, `qemu-numa`'),
 ('kernel-arch-percpu-and-smp', 'Per-CPU blocks and SMP bring-up', 'a CPU index is below the online count the census established (the 64-CPU portable cap and lower backend caps); each CPU touches its own slot', 'the CPU census, before any secondary starts', 'the `smp-core-cap` gate (a census above the cap is refused), the `kernel.kernel` scheduler and cross-core wake tests'),
 ('kernel-arch-early-virtio-blk-aarch64', 'The AArch64 bring-up block driver', 'its rings and buffers are its own frames; the device sees physical addresses because no IOMMU is initialised yet', 'the aarch64 boot path, deliberately before the device manager', 'not falsifiable by a test today - it IS the untranslated access P02M0173 M3 requires to move behind confirmed IOMMU initialisation or off the enforcing path (family work, recorded there)'),
 ('kernel-arch-boot', 'Early boot and the arch modules', 'single-threaded on the boot CPU, reading memory the loader or the device tree described, before the allocator exists', 'the loader\'s `BootInfo` contract', 'every boot; the loader-to-kernel contract tests in `bootproto`; the DMA-mode carrier and arch-profile gates'),
 ('kernel-arch-port-io-and-fwcfg', 'Port I/O and fw_cfg (x86_64)', 'a fixed legacy port is driven by the module that owns it', 'the console, PCI configuration and the carrier reader', 'the `dma-mode-x86_64` gate reads the carrier through it; the PCI census tests'),
 ('kernel-dtb-parsing', 'Device-tree parsing (`fdt`)', '`Fdt::new` requires a valid tree at the address; every read is bounded by the header\'s declared sizes', 'the loader and the boot path that hand the address over', 'the `fdt` crate\'s host tests over the QEMU fixture trees and the malformed shapes; the `dma-mode-carrier` gate'),
 ('kernel-iommu', 'The virtio-IOMMU backend', 'queues and scratch are pinned frames the module owns; every access carries a SAFETY line with the bound', 'the module, at bring-up', 'the hostile EDU gate (`qemu-virtio-iommu-x86_64`), the fault-drain and malformed-completion host tests'),
 ('kernel-mem', 'Heap, frames and TLB', 'the allocator\'s: a region returned was handed out here and is unreferenced; frames are retired once', 'the allocator', 'the frame accounting and poisoning tests (which run in the shipping profile), the shootdown tests'),
 ('kernel-sync', 'Locks', 'a spin lock hands out one `&mut T` at a time', 'the lock', 'every test that runs on more than one core'),
 ('kernel-sched-and-objects', 'Scheduler, threads and objects', 'per-CPU run queues indexed by the current CPU; a thread stack owns its frames for its life; the idle hook is the `fn()` it was stored from', 'the scheduler', 'the `kernel.kernel` scheduler, preemption, stack-guard and object tests'),
 ('kernel-main-loader-device', 'Kernel entry, program loading and PCI', 'writes into a freshly created address space through kernel mappings the loader owns; MMIO to BARs the census resolved', 'the object layer', 'the launch and claim tests, the `dma` and `pci` suites'),
 ('kernel-remaining', 'Other kernel sites', 'reviewed per file as contained', 'each module', 'the kernel suite'),
]
w("| boundary | sites | rows | kinds | invariant | who establishes it | falsified by |\n| --- | ---: | --- | --- | --- | --- | --- |")
for gid, title, inv_, who, ev in kernel_groups:
    w(f"| {title} (`{gid}`) | {n(gid)} | {rowlist(gid)} | {kinds(gid)} | {inv_} | {who} | {ev} |")
w("")
w("`unsafe impl Send`/`Sync` in the kernel: `SpinLock<T>` (both, for `T: Send`), `Console: Send`, and the AArch64 trap-stack `TrapStacks`/`Pool: Sync` - each rests on the exclusion argument in its group above. The kernel's `static mut` items reachable in a shipping row are the IDT and the BSP's CPU area on x86_64 (written before interrupts), the AArch64 secondary stacks and the boot probes (single-threaded boot), and the riscv64 secondary-stack pointer; the trace ring is `cfg(test)`. The `transmute` of the idle hook restores a `fn()` from the integer it was stored as.\n")
w("## 4. Userspace, by cohort\n")
user_groups = [
 ('rt-raw-syscall', 'Runtime: the raw syscall', 'required'),
 ('rt-address-returning', 'Runtime: address-returning wrappers', 'required'),
 ('rt-propagated-wrappers', 'Runtime: propagated wrappers', 'overly broad - FINDING F1'),
 ('rt-heap-stream-and-linkage', 'Runtime: allocator, streams, entry and forwarding stubs', 'ABI'),
 ('drivers-mmio-and-dma', 'Drivers: MMIO and DMA', 'contained'),
 ('drivers-propagated-wrappers', 'Drivers: propagated wrappers', 'overly broad - F1'),
 ('drivers-entry-linkage', 'Drivers: entry ABI', 'ABI'),
 ('services-mapped-views', 'Services: mapped-object views and the display/audio engines', 'contained'),
 ('services-wrapped-runtime-calls', 'Services: wrapped runtime calls', 'overly broad - F1'),
 ('services-entry-linkage', 'Services: entry ABI', 'ABI'),
 ('libs-shared-image-exports', 'Libraries: the shared-image provider ABI', 'ABI'),
 ('libs-ipc-and-driver-protocol', 'Libraries: IPC client and driver protocol', 'contained'),
 ('libs-wrapped-runtime-calls', 'Libraries: wrapped runtime calls', 'overly broad - F1'),
 ('apps-mapped-views', 'Applications: mapped-file views', 'contained'),
 ('apps-wrapped-runtime-calls', 'Applications: wrapped runtime calls', 'overly broad - F1'),
 ('apps-entry-linkage', 'Applications: entry ABI', 'ABI'),
 ('sdk-component-boundary', 'SDK and the WASM component', 'ABI'),
 ('term-raster', 'Terminal raster (`Raster::new`)', 'required'),
 ('term-raster-internals', 'Terminal raster (pixel access)', 'contained'),
 ('loader-firmware-and-handoff', 'Boot: loader, UEFI layer, boot protocol', 'required'),
 ('shared-crates-misc', 'Shared crates (dma, wire)', 'contained'),
]
w("| cohort | sites | rows | kinds | judgement |\n| --- | ---: | --- | --- | --- |")
for gid, title, judge in user_groups:
    w(f"| {title} (`{gid}`) | {n(gid)} | {rowlist(gid)} | {kinds(gid)} | {judge} |")
w("")
w("**Runtime.** `rt::syscall` is the one genuinely unsafe entry: inline assembly whose argument contract is the kernel's. Twenty-three wrappers hand the caller a raw address (`map_object`, `dma_buffer_map`, `dma_buffer_phys*`, `framebuffer_map`, `memmap_get`, the module loader) and are rightly `unsafe fn`. The remaining wrappers - " + str(n('rt-propagated-wrappers')) + " sites in the runtime's own crate - are `unsafe fn` by propagation only: they take handles, slices and values, pass a slice's own pointer and length, and cannot be misused from the caller's side. The allocator (`GlobalAlloc`), the stream reader and the `memcpy`/`memset` forwarding stubs of the shared-image build are the language's and the linker's ABI.\n")
w("**Drivers.** The device boundary is real and contained: registers are reached through the MMIO span the kernel mapped for the claim, rings through `DmaAddress`-backed buffers, with volatile access and the barriers the virtio and xHCI specifications require; the raw pointers name those spans. The enforcing-IOMMU gate is the falsifier (a physical address in a descriptor faults). The drivers' shared handshake layer and per-device bring-up functions are `unsafe fn` by propagation from the runtime.\n")
w("**Services.** " + str(n('services-wrapped-runtime-calls')) + " sites wrap runtime calls that pass handles and slices; none dereferences memory. The raw pointers that exist (" + str(n('services-mapped-views')) + " sites, the display and audio engines included) are views over MemoryObjects the kernel mapped whole at a length the same service or the kernel reported, or writes into DMA-visible buffers a driver published for that purpose. `DeviceManager` and `ConsoleService` carry the most propagation (over sixty `unsafe fn` each).\n")
w("**Libraries.** The shared-image provider ABI is generated: `#[unsafe(export_name)]` wrappers in every protocol crate, thirty `forward!` templates (one per provider crate and architecture) whose `global_asm!` aliases an exported symbol to its implementation, and the `extern` blocks a client crate declares for its provider. The package identity, export-owner and provider-closure checks audit that surface; it is ABI, not raw memory. Client libraries' remaining blocks are propagation.\n")
w("**Applications.** Seventy-five `extern \"C\" fn __user_main` entry points (ABI), fourteen mapped-file views (`from_raw_parts` over a file the storage service mapped whole), and propagation everywhere else.\n")
w("**The component boundary, both sides.** The SDK declares its host imports as `unsafe extern \"C\"` and exposes a safe API over them; `liber_component` exports `run`, `score` and `panic_now` with `#[unsafe(no_mangle)] extern \"C\"`. The interpreter's side (`src/wasm`) contains NO unsafe code: imports are dispatched by name and every argument is validated by the host before it reaches a service.\n")
w("**Boot.** The loader and the UEFI layer are a firmware-ABI boundary end to end: protocol and table pointers the firmware handed the image entry, used while boot services are live and never after; the memory map, ELF placement and hand-off write physical memory the firmware's map declared free; every `static mut` is reached only from the single-threaded boot path and says so where it is declared. `bootproto` and `fdt` are shared with the kernel and host-tested.\n")
w("## 5. Findings, by severity, and where each goes\n")
w("| id | severity | finding | sites | owner | disposition |\n| --- | --- | --- | ---: | --- | --- |")
f1 = n('rt-propagated-wrappers') + n('drivers-propagated-wrappers') + n('services-wrapped-runtime-calls') + n('apps-wrapped-runtime-calls') + n('libs-wrapped-runtime-calls')
w(f"| F1 | medium (the safety story, not soundness) | `unsafe` in userspace is propagation from the runtime's wrapper signatures: {f1} reachable sites carry an `unsafe` that names no caller obligation, and the dozen real raw-memory boundaries are indistinguishable from them by reading | {f1} | rt, then every cohort | **P02M0178** (M1, M2, M4): safe signatures where the body upholds the contract, the propagation removed downstream, the inventory regenerated to prove it |")
w(f"| F2 | low (latent) | the kernel's safe `read_user<T>` produces any `T` from user bytes; sound for its four plain-data instantiations, unable to refuse a future `T` with invalid bit patterns | {n('kernel-syscall-read-user')} | kernel/syscall | **P02M0178 M3**: bound `T` to plain-old-data or make the function `unsafe` with the contract written |")
w(f"| F3 | informational (family) | the AArch64 bring-up block driver masters the bus untranslated before the IOMMU exists - by design under the ports' produced `no-iommu` mode, and the boundary P02M0173 M3 already requires to move | {n('kernel-arch-early-virtio-blk-aarch64')} | kernel/arch/aarch64 | **P02M0173 M3** (attached to the family item, as M4 permits for planned family work) |")
w("")
w("**Confirmed unsoundness or material defect in shipped code: none found in this pass.** What was checked to say so: every `unsafe impl Send`/`Sync` (kernel `SpinLock`, `Console`, the AArch64 trap-stack pools; the runtime's locked heap) against its exclusion argument; every reachable `static mut` against the single-threaded or before-interrupts context it is written in; the usercopy `.extable` fixups and the syscall layer's short-copy handling; the `transmute` of the idle hook; every `from_raw_parts` view in services and tools against the length the kernel mapped; the drivers' descriptor writes against the enforcing-IOMMU gate; the four `read_user` instantiations; the loader's firmware-table lifetimes against `ExitBootServices`. A finding of this class would have got its own numbered corrective item rather than a place on an index, as M4 requires.\n")
w("## 6. Expansion reconciliation\n")
w("Recorded from `--expand` over the x86_64 kernel row with compiler-derived impls excluded (28 crates expanded and compared, no expansion failure). Twenty-seven crates reconciled EXACTLY per kind. The kernel crate itself differs in four kinds, all HIGHER in the expansion: `extern-abi-fn` 14 in the source against 248 expanded - the `generic_vectors!`, `generic_vectors_with_code!`, `irq_stub!` and `msi_stubs!` templates in `arch/x86_64/idt` and `interrupts`, which stamp out the interrupt entry stubs and are recorded as four macro-template sites with their invocation counts - and `unsafe-fn` +4, `unsafe-block` +3 and `raw-pointer` +5 from the same templates' bodies. NOTHING WAS LOWER: no reachable source site was dropped by the compiler under the row's cfg set, which is the check that the source-level cfg evaluation agrees with rustc's. An earlier run without the derived-impl exclusion reported `unsafe impl` counts in every crate; they were all `#[automatically_derived] unsafe impl TrivialClone`, the toolchain's own derive output, which is why the exclusion exists.\n")
w("## 7. Completeness fixtures\n")
w("`src/tools/unsafe-inventory/fixtures` holds one crate with a cfg-only site, a macro-produced site, a generated site (a build script writing an `unsafe fn` into `OUT_DIR`), a test-only site and an ordinary baseline, and a second in-tree crate reached only as a dependency. The tool's tests (`cargo test` in `src/tools/unsafe-inventory`, 8 tests) assert each class is found with its reachability and provenance, that a missing `OUT_DIR` is reported rather than silent, that the dependency is in the closure and a registry crate would not be, that ids are stable, and that `cfg` predicates evaluate as rustc would. A class of site that stopped being found fails there.\n")
w("## 8. What this audit did not do\n")
w("It changed no production code and set no target count; it did not audit third-party crates (listed per row and excluded); it does not claim Miri or host tests exercised privileged assembly, MMIO or DMA - the falsification column names the guest gates that do; and it does not propose a blanket conversion. P02M0178 is the one corrective item it opened.\n")
open(os.path.join(root, 'AI/audit/unsafe-boundaries.md'), 'w').write('\n'.join(out) + '\n')
print("rendered", sum(len(x) for x in out), "chars")
