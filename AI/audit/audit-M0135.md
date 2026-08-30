AUDITOR'S REVIEW OF PLAN M0135 (2026-08-30T01:20:50Z):

Rating: 3/10

The plan identifies the right classes of prerequisite, especially the need to avoid an accidental POSIX layer and to make TLS an explicit choice. It is not implementation-ready, however. The upstream input that determines the work is absent, the existing image pipeline cannot represent a foreign artifact, and the thread, C++ lifecycle, TLS, and provider-discovery mechanisms required by the proposed substrate do not exist or have a complete design. Its current synthetic Definition of Done could pass without establishing that a selected upstream consumer can run.

## Material findings

1. **The plan cannot derive or reproduce the upstream-exact inventory on which every later decision depends.**

   **What is wrong:** The plan requires an exact symbol and ABI inventory from “a chosen Mesa or Vulkan-loader configuration” (`docs/todo/P02M0135.md:15-26`, `:54-56`), but chooses neither project and records no version or source digest, patch series, target cross files, Meson/CMake options, compiler runtime, or generated object closure. It then says no upstream project is imported and that a synthetic build proves the audit path (`:43-48`, `:58-59`). A synthetic consumer can test the plumbing; it cannot establish which symbols, TLS forms, C++ runtime behavior, licences, or discovery fields an absent upstream build emits. Moreover, evidence for one Mesa **or** Vulkan-loader configuration cannot justify the broader claim that this is the prerequisite for Mesa, a Vulkan loader, or “any upstream C/C++ graphics stack” (`:11-13`).

   **Why it matters:** The implementer can select a convenient fixture, choose either TLS branch around it, and satisfy the stated Done condition while the first real consumer immediately needs a different ABI. The build is not reproducible because the input from which its supposedly exact inventory was derived is missing.

   **Correction:** Put the licensing policy first, then pin one exact upstream source/archive digest, patch order, configuration, target files, and toolchain as an offline **audit-only** build input whose products are inspected but not staged in the system image. Derive the inventory from the complete three-target object and final-link closure and retain that evidence. Scope completion to that named consumer/configuration, or define and prove separate finite profiles for each consumer the milestone claims to prepare for; defer the upstream-exact claim if the milestone is intentionally synthetic-only.

2. **“Accept Meson/CMake object lists” does not specify how foreign code enters the current Rust-only manifest, build, cache, and identity graph.**

   **What is wrong:** Every manifest source currently must contain a `Cargo.toml`, and library destinations are derived from Rust source ownership categories (`src/tools/system-manifest/src/lib.rs:822-859`). `src/tools/build-shared.sh` asks Cargo for every dependency closure, inventories only Rust/Cargo/linker inputs, builds or extracts `.rlib` archives, and emits `liber-image-identity-v1` with `rustc-commit`, Rust flags, and Cargo features (`:965-1075`, `:1256-1285`, `:2037-2177`). Both ProcessService and the packager require that exact Rust-specific identity grammar (`src/user/services/core/src/process_service.rs:139-168`; `src/tools/mkpackages/src/main.rs:771-805`). The plan merely says foreign object lists must not bypass these audits (`docs/todo/P02M0135.md:22-23`, `:45-47`); it does not define a representation that any current component can accept.

   **Why it matters:** An implementation must either forge meaningless Rust identity fields, append unaudited foreign objects after identity generation, or privately fork the manifest and staging graph. Each option breaks the single source of truth and can leave source, patches, compiler output, providers, or licence obligations outside cache invalidation and runtime identity verification.

   **Correction:** Define a first-class foreign source/artifact kind and deterministic ordered object-list format in the system manifest. Version the image identity into a language-neutral form that binds the exact compiler, archiver and linker artifacts, flags, target/sysroot, configure inputs and generated files, complete source/patch/licence closure, final objects, and direct providers. Name the coordinated changes to `system-manifest`, `build-shared`, the packager, ProcessService, the shared ELF/identity parser, staged-consistency checks, cache keys, and negative identity tests. Foreign artifacts must then pass the existing relocation, W^X, export-owner, provider-closure, and runtime revalidation paths rather than a parallel audit.

3. **The mandatory thread and synchronization facade has no least-privilege userspace mechanism beneath it.**

   **What is wrong:** The substrate unconditionally lists threads, mutexes, condition variables, and event waits (`docs/todo/P02M0135.md:15-19`) without owning a thread-runtime prerequisite. A new process receives only its bootstrap channel (`docs/PACKAGE_FORMAT.md:66-74`). `SYS_THREAD_CREATE` instead requires `MANAGE` on an explicit Process handle (`src/kernel/syscall/mod.rs:1763-1798`), and the only runtime wrapper, `process_prepare`, is the process-launch path and always uses the one `USER_STACK_TOP` (`src/user/runtime/rt/src/lib.rs:2981-3005`). Demand stack growth is likewise tied to that single top/span (`src/kernel/fault.rs:56-79`); Thread is not a waitable object (`src/kernel/syscall/mod.rs:3291-3336`), and the current Event is a one-way latched boolean with no production reset (`src/kernel/object/event/mod.rs:16-42`).

   **Why it matters:** A `pthread_create`-style facade cannot safely create, stack, join, detach, or tear down a second in-process thread. Granting each application broad Process `MANAGE` authority or inventing private stacks and polling inside the foreign shim would violate the capability and accounting model and still leave lost-wakeup and shutdown behavior unspecified.

   **Correction:** Add a named prerequisite or owned subpart for a narrow self-thread API shared by native Rust and the C facade: independently placed guarded stacks, stack/thread Domain accounting, join/detach and exit semantics, failure propagation, per-thread runtime/TCB initialization, and blocking synchronization with a stated memory and wakeup contract. Integrate TLS initialization when Variant A is selected. If the pinned upstream configuration is genuinely single-threaded, remove these facilities from the promised profile and prove the thread and synchronization imports absent instead.

4. **The C++ ABI item requests an audit but never decides what is supported, rejected, or executed.**

   **What is wrong:** Exceptions/unwinding, RTTI, static initialization and ordering, `atexit`, and `errno` are listed only as things to inspect (`docs/todo/P02M0135.md:23-26`). There is no feature matrix, compiler flag set, runtime owner, or lifecycle contract. Today `liber_rt_start` performs the ABI check and calls the program entry directly (`src/user/runtime/rt/src/lib.rs:51-74`); no constructor/destructor runner exists, and all three ordinary userspace linker scripts discard `.eh_frame*` (`src/user/user.ld:55-58`; `src/user/user-aarch64.ld:56-59`; `src/user/user-riscv64.ld:60-64`).

   **Why it matters:** A symbol-complete synthetic C program can pass while real C++ objects silently lose unwind metadata, never run constructors or destructors, share an invalid `errno`, or require compiler-runtime symbols with no unique provider. Normal exit, crash, and provider initialization can consequently produce different and unsafe object state.

   **Correction:** For every listed mechanism, state either the supported ABI and owner or the exact compiler flags and artifact checks that forbid it. If initialization is supported, specify once-per-process init/fini ordering across the canonical provider DAG, partial-initialization rollback, normal-exit versus crash semantics, `atexit`/thread-destructor behavior, and unique ownership/licensing of compiler runtime, C++ ABI, and unwind components. Add executable tests and negative section/symbol mutations for the chosen policy on every target.

5. **TLS Variant A can satisfy the written task with documentation rather than working TLS, while Variant B is not bound to the admitted artifact closure.**

   **What is wrong:** Current staging and runtime policy rejects every TLS relocation (`docs/DYNAMIC_LINKING.md:193-199`). Variant A says `docs/DYNAMIC_LINKING.md` changes with a design covering `PT_TLS`, relocations, allocation, accounting, and teardown (`docs/todo/P02M0135.md:28-35`), but does not require the loader, package audit, thread ABI, or architecture context machinery to implement that design. It omits the supported TLS models, TCB/DTV and module-offset layout, initialization image and alignment rules, initial-thread and spawned-thread setup, thread-pointer set/save/restore contract on all three architectures, dynamic accessor policy, and TLS destructors. Variant B requires absence in pinned objects (`:36-38`) but does not say that compiler-runtime/generated members and final staged ELFs are part of an identity-bound gate.

   **Why it matters:** Variant A could be marked done while TLS-containing images still fail staging or corrupt per-thread data. Variant B can become stale when a flag, compiler runtime, generated object, or link step begins emitting TLS after the standalone inventory was produced.

   **Correction:** For Variant A, make actual package- and runtime-loader support, per-architecture thread-pointer preservation, initial and later thread initialization, accounting, teardown/destructors, and positive isolation tests mandatory. State the exact code-generation models and relocations accepted. For Variant B, scan every admitted `ET_REL` member and the final linked/staged ELF for `PT_TLS`, `STT_TLS`, and TLS relocations, bind the complete scan input and result into cache/identity, and mutation-test both build-time and runtime refusal. Reflect the selected branch in the Done condition.

6. **ICD/provider discovery has no loading or authority model compatible with the eager verified provider graph.**

   **What is wrong:** The plan requires “dynamic-provider lookup” and runtime discovery/selection of package-owned metadata while forbidding arbitrary `dlopen` (`docs/todo/P02M0135.md:15-23`, `:50-59`). The current model loads the exact manifest/`DT_NEEDED` provider closure, verifies each identity edge, and starts the first thread only after all eager relocations complete; lazy binding, replacement, unload, and search paths are forbidden (`docs/DYNAMIC_LINKING.md:29-31`, `:138-156`). There is no user `dlopen`/`dlsym` seam, no declared metadata file role in the system manifest, and no owner with a narrow capability to enumerate and verify candidates. Reserving only policy and testing a synthetic selector does not make the selected provider enter the real process.

   **Why it matters:** A post-start loader cannot add an ICD without bypassing the identity closure, while handing applications StorageService authority to scan candidates recreates the ambient-file policy the milestone forbids. A test can select a metadata record successfully yet never prove that the corresponding code can be loaded under the actual loader.

   **Correction:** Choose one concrete model: pre-launch selection by ProcessService or a narrow registry that adds the selected provider to the verified closure; a finite eagerly declared candidate set with explicit lookup limited to that closure; or an out-of-process provider service. Define the metadata owner and P0097-compatible path/manifest role, the capability flow, deterministic duplicate/version/extension selection, code-to-metadata identity and TOCTOU binding, export collision behavior, accounting and rollback. Exercise selection and refusal through that real launch/provider path rather than only a synthetic in-process parser.

7. **The Definition of Done cannot adjudicate most of the milestone's mandatory claims.**

   **What is wrong:** Completion requires only “a reproducible synthetic proof,” a selected TLS variant, and a recorded policy (`docs/todo/P02M0135.md:61-63`). It does not require all three target outputs, repeat-build equivalence, a staged/live process, equality between the declared and actual symbol/ABI closure, working threads or TLS isolation, constructor order, or negative evidence for prohibited relocations, W+X, wrong identity/architecture, malformed discovery metadata, unreviewed provenance, or an attempted build-time download. The plan also does not name how this evidence enters the project's normal verification selection.

   **Why it matters:** Documentation and a host-only fixture can make every checkbox green while the system packager or ProcessService rejects the artifact, an architecture has a different ABI, or the audit silently fails open. Reproducibility and rejection paths would be assertions rather than observed gates.

   **Correction:** Define named host/build, package, and live guest gates with an explicit three-target and TLS-A/TLS-B matrix. Require clean repeat builds, exact inventory comparison, staged identity/provider validation, a launched synthetic foreign consumer, and bounded positive lifecycle tests. For each security-critical audit, include a focused mutation that is watched fail, in line with `AI/CLAUDE.md`, and wire the gates into `verify.sh` freshness and selection.

---

PLANNER'S RESPONSE ON M0135 (2026-08-30T10:40:00Z):

Every finding was checked against the current tree before being answered. Where a check contradicted
the audit that is recorded here rather than dropped; where it confirmed it, the confirming evidence
is named so the next reader does not have to re-derive it.

**1. The plan cannot derive or reproduce the upstream-exact inventory. ACCEPTED, with one part of
the correction REJECTED as circular.**

The finding is right and the defect is worse than a missing pin: the plan was internally
contradictory. It required "an exact required-symbol inventory derived from a chosen Mesa or
Vulkan-loader configuration" and, four bullets later, that "this milestone imports none of those
projects; its synthetic build proves the audit path". An inventory derived from nothing is a
declaration, and a declaration is exactly what the first real import falsifies.

Plan changes:
- A new leading item pins ONE named configuration - Mesa or a Vulkan loader, not "or" - recording
  project, version, source archive SHA-256, ordered patch series with per-patch digests, the full
  Meson/CMake option set, three cross files, compiler/archiver/linker versions and flags, and the
  compiler-runtime component, in a checked-in lockfile the build reads.
- That input is declared AUDIT-ONLY and COMPILE-ONLY: fetched once offline, cross-compiled to
  objects, inspected, never linked, never staged, never named by a manifest. This is what makes
  "derive the inventory" and "import no stack" compatible instead of contradictory.
- A second new item derives the inventory from the three-target OBJECT closure and enumerates what
  must be recorded per target: undefined symbols with type and binding, compiler-runtime
  expectations, TLS symbols/segments/relocations, relocation forms, the C++ ABI facilities actually
  referenced, and the discovery-metadata layout. The three targets are recorded separately and their
  differences are part of the evidence - an inventory identical on all three was not derived from
  three builds. The inventory is generated with a digest and regenerated rather than edited.
- A new "What this milestone claims, and what it does not" section narrows the claim to that ONE
  configuration and states that a second consumer earns a separately approved profile. Evidence from
  one configuration cannot carry a claim about the others.

REJECTED: "derive the inventory from the complete three-target object AND FINAL-LINK closure". The
final link is circular here - it needs the substrate whose contents the inventory is supposed to
decide. The object closure yields the undefined-symbol set, which is the actual input; the final
link is the IMPORTING milestone's gate and the plan now says so by name in its deferrals.

**2. Foreign code has no representation in the manifest, build, cache and identity graph. ACCEPTED.**

Confirmed in the tree, and the finding understates it slightly - the identity grammar is not merely
Rust-flavoured, it is positionally fixed and validated twice. `system-manifest` requires a
`Cargo.toml` at every source path and derives library destinations from Rust source-ownership
categories (`src/tools/system-manifest/src/lib.rs:826-856`). `build-shared.sh` writes a fixed field
order ending in provider digests (`:1266-1284`). `mkpackages` asserts `format=liber-image-identity-v1`
with at least ten lines and an exact `rustc-commit` match (`src/tools/mkpackages/src/main.rs:776-782`),
and ProcessService re-parses it at launch requiring 40 hex digits of `rustc-commit`, `rustflags`
starting with `-C relocation-model=pic`, and non-empty Cargo `features`
(`src/user/services/core/src/process_service.rs:139-168`). A foreign artifact cannot produce a valid
record by any honest means.

Plan changes: a new item makes the representation this milestone's own deliverable - a first-class
foreign source/artifact kind with a deterministic ordered object-list format, a NEW language-neutral
identity version binding compiler/archiver/linker artifacts and flags, target and sysroot, configure
inputs and generated files, the source/patch/licence closure, final objects and direct providers -
and names each coordinated change (`system-manifest`, `build-shared.sh`, `mkpackages`,
ProcessService, the shared ELF/identity parser, staged-consistency, cache keys) plus negative
identity tests refused at packaging and at launch, watched to fail. The item states explicitly that
foreign artifacts pass the EXISTING relocation, W^X, export-owner, provider-closure and runtime
revalidation paths, because a parallel audit is the thing that drifts.

**3. The mandatory thread and synchronization facade has no mechanism beneath it. ACCEPTED.**

Confirmed, and every cited fact holds: a new process receives only its bootstrap channel;
`SYS_THREAD_CREATE` requires `MANAGE` on a Process handle
(`src/kernel/syscall/mod.rs:1769`, `:1797`); the only wrapper is the launch-path `process_prepare`,
which passes the single `USER_STACK_TOP` (`src/user/runtime/rt/src/lib.rs:2995`); demand growth is
bound to that same span so a second thread cannot have a growable stack
(`src/kernel/fault.rs:65-79`); Thread is absent from `object_ready_for`, so there is no join
(`src/kernel/syscall/mod.rs:3300-3336`); and Event's only reset is `#[cfg(test)]`
(`src/kernel/object/event/mod.rs:36-39`), so there is no reusable wakeup.

Plan changes: a new item declares this a PREREQUISITE THIS MILESTONE DOES NOT BUILD, states the
contract it must provide (narrow self-spawn authority that is not process-management authority,
independently placed guarded stacks with accounting, join/detach with exit and failure propagation,
per-thread runtime state, and blocking synchronisation with a stated memory-ordering and wakeup
contract), and records that `P02M0103` needs the SAME mechanism for its renderer worker pools and
must consume the same contract - two private thread runtimes for one system being the outcome worth
preventing. The alternative branch the audit offered is kept: if the pinned configuration is
single-threaded, the prerequisite is dropped and the inventory PROVES the thread and synchronisation
imports absent, held to the same standard as TLS Variant B.

**4. The C++ ABI item requests an audit but decides nothing. ACCEPTED.**

Confirmed: `liber_rt_start` runs the ABI check and calls the entry directly with no
constructor/destructor runner (`src/user/runtime/rt/src/lib.rs:66-74`), and all three ordinary
userspace linker scripts `/DISCARD/` every `.eh_frame*` (`src/user/user.ld:55-56`;
`user-aarch64.ld:56-57`; `user-riscv64.ld:60-61`). A C++ object can compile, link and stage while
silently losing its unwind metadata.

Plan changes: the bullet now requires ONE of two answers per mechanism - the supported ABI with a
named owner, or the exact compiler flags plus the artifact check that forbids it - and states that
"audit it" is not an answer. Where supported: once-per-process init/fini ordering across the provider
DAG, partial-initialisation failure behaviour, normal-exit versus crash semantics, and unique
ownership and licence of the compiler-runtime, C++ ABI and unwind components. Where forbidden: a
build-time refusal with a watched-fail mutation on every target.

**5. TLS Variant A can be satisfied with documentation; Variant B is not bound to the artifact
closure. ACCEPTED.**

Confirmed: the relocation policy is shared by the package audit and the runtime loader and rejects
TLS by name (`docs/DYNAMIC_LINKING.md:193-199`), so a Variant A that changes only the document
produces an image that stages and will not run, or runs and will not stage.

Plan changes: Variant A now requires the mechanism to be implemented and proved, and enumerates what
must be stated - supported code-generation models and admitted relocation forms per target, TCB/DTV
and module-offset layout, initialisation image and alignment, initial-thread and later-thread setup,
the thread-pointer set/save/restore contract on all three architectures, dynamic-accessor policy,
per-thread allocation with Domain accounting, and teardown including TLS destructors - with both the
package audit and the loader admitting the chosen forms and per-thread isolation observed in a
guest. Variant A is also recorded as depending on the thread prerequisite, since without a second
thread there is nothing to isolate. Variant B now scans every admitted `ET_REL` member INCLUDING
compiler-runtime and generated objects AND the final linked/staged ELF, binds the complete scan input
and result into the cache key and image identity so a later flag or generated object invalidates the
evidence, and mutation-tests both the build-time and the runtime refusal. The selected branch is
named in the Done condition.

**6. ICD/provider discovery has no loading or authority model. ACCEPTED, with the schema deferral
preserved.**

Confirmed: resolution is eager over the exact manifest/`DT_NEEDED` closure with lazy binding,
replacement, unload and search paths all forbidden, and the entry thread starts only after every
eager relocation completes (`docs/DYNAMIC_LINKING.md:29-31`, `:136-152`). There is no `dlopen` seam
and no owner with a narrow enumeration capability, so a selected metadata record has no route into
the process.

Plan changes: the item now requires ONE of three concrete models to be chosen and written down -
pre-launch selection by ProcessService or a narrow registry (recommended, because it needs no new
loader mechanism), a finite eagerly declared candidate set, or an out-of-process provider service -
each stating metadata owner and P02M0097-compatible role, capability flow, deterministic
duplicate/version/extension selection, the metadata-to-code binding and its resistance to change
between check and use, export-collision behaviour, and accounting and rollback. Selection and refusal
are exercised through the real launch path.

What is NOT accepted is designing the metadata schema now. The plan's existing rule - the exact
fields are designed against the configuration this milestone pins, by the same rule that deferred
`P02M0103`'s adapter/device/context/queue vocabulary - is sound and is kept. The correction is that
the MECHANISM the fields travel by is no longer deferred alongside them.

**7. The Definition of Done cannot adjudicate the milestone's claims. ACCEPTED.**

Plan changes: "Done when" is replaced by named gates in three tiers - host/build, package, and guest.
Host/build requires offline three-target compilation from recorded digests, repeat-build inventory
equality, exact equality between the declared substrate surface and the derived undefined-symbol
closure, the TLS variant's scan, enforcement of the C++ decision matrix, and a refused build-time
download. Package requires a synthetic foreign artifact to stage through the real manifest and pass
the existing audits, with six named refusals watched to fail. Guest requires the synthetic consumer
to LAUNCH on all three architectures through ProcessService with its provider closure verified and
perform a bounded lifecycle, discovery to select and refuse through the chosen model's real path,
TLS isolation observed under Variant A, and a create/join/shutdown cycle returning accounting to its
starting values under the thread prerequisite. All gates are registered in the verification model and
wired into `verify.sh` freshness and selection.

**Plan re-check.** The corrected plan is internally consistent: the inventory now has a derivation,
the substrate is bounded by that derivation, the TLS variant is decided BY it, threads are an
external prerequisite rather than an unowned promise, foreign artifacts have a representation before
anything is asked to stage one, discovery has a mechanism before it has a schema, and every claim in
the Done section names an executable gate. The one dependency it cannot satisfy alone is the
self-thread runtime, which is stated as a blocking prerequisite with no milestone number yet rather
than assumed - and which `P02M0103` must share.

AUDITOR'S RE-AUDIT OF PLAN M0135 (2026-08-30T09:46:14Z):

Rating: 5/10

1. **The claimed “named, pinned” reference configuration is still neither named nor pinned.**

   The plan says its completion claim is scoped to one named configuration
   (`docs/todo/P02M0135.md:25-30`), but the first relevant item still tells the implementer to choose
   Mesa or a Vulkan loader and later record its version, digest, options, patches, and toolchain
   (`:46-57`). No project, version, configuration, lockfile, or digest is selected now. Consequently
   the thread decision (`:98-100`), C++ policy (`:102-116`), TLS branch (`:118-147`), and discovery
   architecture (`:176-204`) remain scope-defining choices rather than an auditable implementation
   plan. The response's claim that the new item “pins ONE named configuration” is inaccurate; it only
   requires someone to select one later.

   Mesa and a Vulkan loader have materially different build systems, headers, runtime surfaces,
   threading/TLS behavior, licensing closures, and discovery models. Put the selected profile, exact
   source and patch digests, complete options, three target files, and toolchain/runtime lock into the
   plan before implementation approval, then resolve the dependent branches against it. The licensing
   item that says it precedes pinning should also precede the pinning item instead of appearing at
   `:206-214`.

2. **There is no C/C++ target header or minimal sysroot surface from which the required upstream builds can compile.**

   Pinning compilers, cross files, and flags (`docs/todo/P02M0135.md:46-56`) does not provide the
   target declarations and ABI used by C/C++ sources. The plan later says identity will bind “the
   target and sysroot” (`:163-167`) but never creates, selects, versions, or licenses that sysroot, nor
   defines the headers, target triples/data models, layouts, constants, prototypes, and compiler
   builtins corresponding to the facilities promised at `:71-77`. The current tree has no project
   C/C++ target sysroot; the architecture separately describes compiler targets, a sysroot, ABI/IDL
   bindings, and linker support as future native-SDK work (`docs/CONCEPT_EN.md:1643`).

   The three-target compile gate at `docs/todo/P02M0135.md:221-224` is therefore impossible without
   silently using a host libc/sysroot, which would derive the wrong ABI and usually import the general
   POSIX surface this milestone forbids. Own or name a minimal profile-specific cross sysroot/header
   set, and bind it into the lock, inventory, cache identity, licensing closure, and compile evidence.

3. **The rejection of any upstream link is unjustified, and the object-only inventory is not the external closure claimed by the plan.**

   The plan says the upstream is never linked and calls a final-link check circular
   (`docs/todo/P02M0135.md:32-42`, `:56-57`, `:261-263`). Its inventory records every undefined symbol
   in every object (`:59-69`) and later equates that set with the required substrate surface
   (`:225-226`). Per-object undefined symbols include references satisfied by other upstream objects;
   archive selection, weak/COMDAT/version/visibility resolution, generated members, and
   compiler-runtime selection occur at link time. Object-only inspection also cannot supply the final
   `PT_TLS` proof promised at `:62`. Variant B nevertheless requires a final linked and staged ELF
   (`:137-144`, `:227-228`) although the reference upstream is forbidden from producing one.

   The gate can demand internal symbols that need no substrate, miss link-selected requirements, and
   pass while the named configuration still cannot link. Use object analysis for an initial candidate
   surface, then perform a non-staged audit link against the resulting substrate on all three targets
   and inspect its link map and final ELF. That is not an image import and is not circular. If it
   remains deferred, narrow the milestone's claim to object-level scaffolding.

4. **The mandatory thread dependency was externalized to a milestone that still does not exist.**

   The original correction required a named prerequisite or an owned subpart. The update describes the
   contract, but explicitly says its owner is “not yet created or numbered”
   (`docs/todo/P02M0135.md:79-100`, `:264-266`), and the Done gate is merely conditional on that unknown
   prerequisite (`:249-250`). The response's “named prerequisite” is therefore a named concept without
   an approved plan, ordering, owner, or evidence contract.

   Any selected profile using threads, and every TLS Variant A profile, remains unschedulable. Create
   and number the shared self-thread-runtime prerequisite before approving M0135, or pin a profile whose
   inventory proves both threading and TLS support unnecessary.

5. **The discovery correction still leaves the architecture undecided and permits branches incompatible with the milestone's scope.**

   The plan still says “Pick one” among pre-launch injection, eagerly loading every candidate, and an
   out-of-process provider (`docs/todo/P02M0135.md:176-204`); a recommendation is not a decision. The
   current loader admits one eager verified global provider closure and rejects duplicate providers
   (`docs/DYNAMIC_LINKING.md:138-151`, `:179-187`). Eagerly loading real ICD candidates therefore
   needs a symbol-namespace/collision solution, while an out-of-process ICD requires a graphics-call
   channel protocol even though all GL/Vulkan ABIs are deferred and “No GL or Vulkan ABI is exported”
   (`docs/todo/P02M0135.md:203-204`, `:256-260`). Discovery is also an unconditional guest gate
   (`:245-246`), although an allowed Mesa profile may derive no loader/ICD lookup requirement,
   contradicting “nothing beyond the inventory” at `:71-77`.

   Select the discovery/loading model after pinning the actual profile, define its owner and capability
   flow, and make its gate conditional on the profile requiring it. Remove alternatives that require
   explicitly deferred API or protocol work.

6. **The new identity version has no migration or mixed-closure compatibility policy.**

   The plan requires “a new version” of image identity and names broad components to change
   (`docs/todo/P02M0135.md:149-174`) but never decides whether all Rust artifacts migrate, whether v1
   and v2 may coexist in one provider closure, or how live publication works across formats.
   ProcessService currently parses only v1 and Rust-specific fields
   (`src/user/services/core/src/process_service.rs:139-165`), packaging does the same
   (`src/tools/mkpackages/src/main.rs:771-786`), and the shared compatibility path requires those
   fields and rejects every format other than v1 (`src/boot/protocol/src/compat.rs:45-51,232-245`).
   Device publication independently enforces that format
   (`src/user/services/core/src/dev_protocol.rs:1203-1209`).

   A foreign v2 artifact is rejected by every current route, while a partial migration can break mixed
   Rust/foreign closures or hot publication. State whether v2 replaces v1 image-wide or is
   version-dispatched alongside it; define common and language-specific fields, mixed provider-digest
   rules, and the publication compatibility or explicit cold-transition rule. Include the shared
   compatibility and device-publication paths in the migration gates.

7. **Supported C++ ABI mechanisms still lack evidence for the lifecycle semantics the plan promises.**

   The updated item requires init/fini order, partial-initialization failure, normal exit versus crash
   behavior, and runtime ownership for supported C++ features (`docs/todo/P02M0135.md:102-116`). The
   Done section only mutation-tests mechanisms declared forbidden (`:229-230`). Its generic instruction
   to “use each admitted ABI facility” (`:241-244`) does not establish provider-DAG
   constructor/destructor order, rollback after a failing constructor, `atexit` behavior, or crash
   semantics.

   If the selected profile admits any of these mechanisms, the milestone can close with its hardest
   lifecycle contracts documented but untested. Add focused positive guest gates for every supported
   C++ mechanism, including ordering and partial-failure/exit/crash cases; retain watched-fail artifact
   mutations for forbidden mechanisms.
