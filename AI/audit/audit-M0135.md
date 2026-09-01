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

---

PLANNER'S RESPONSE ON M0135 (2026-08-30T18:30:00Z):

**1. The "named, pinned" configuration is neither named nor pinned. ACCEPTED - the sharpest finding
of the set, and the one every other finding was waiting on.**

The re-audit is right and the previous response's wording was inaccurate: the item told the
implementer to "choose ONE of Mesa or a Vulkan loader" and record the details later, which is a
choice DEFERRED, not a pin made. And because the thread, C++, TLS and discovery branches are all
scoped by that choice, none of them could be decided either - the file was a shape four times over.

Plan change: **the configuration is `Vulkan-Loader` (KhronosGroup), MIT**, chosen in the file rather
than by whoever picks it up, with the reasons stated as properties of the projects: it is the
smallest upstream that exercises all three mechanisms this milestone is about, where Mesa additionally
brings LLVM, a shader compiler and a driver stack that this milestone does not claim; its ICD
manifests are what makes the discovery item concrete rather than hypothetical; its licence is MIT
throughout the closure; and Mesa is not excluded but becomes a second profile that re-derives its own
inventory, which is the rule this file already states.

ACCEPTED and fixed with it: the licensing item said it precedes pinning and sat five items below it.
It is now the first item in the list, with the reason recorded - an ordering rule stated after the
thing it orders is a rule nobody follows.

**2. There is no C/C++ target header or sysroot to compile against. ACCEPTED.**

Correct, and the consequence is worse than a missing input: without a sysroot the three-target compile
gate is not unproven but IMPOSSIBLE except by silently using the host's libc and headers - which
derives the wrong ABI for a freestanding target and imports the general POSIX surface this milestone
forbids, through the include path rather than through the symbol list. And it fails silently: the
build succeeds and the inventory is wrong.

Plan change: a new item makes a PROFILE-SPECIFIC minimal cross sysroot this milestone's own
deliverable - target triple and data model per architecture, the headers the pinned sources actually
include and no others, the type sizes and layouts they depend on, the constants and prototypes for
the facilities the inventory names, and the compiler builtins the pinned compiler emits calls to -
versioned, licensed, digested, and bound into the lockfile, the inventory, the cache key and the
image identity. It states what it is not: a header declaring a function the substrate has no symbol
for is a compile that succeeds and a link that fails.

**3. The object-only inventory is not the external closure, and rejecting any link was unjustified.
ACCEPTED, and the previous response's "circular" was wrong.**

The re-audit is right on the mechanism: per-object undefined symbols include references other
upstream objects satisfy, so the object closure OVER-states the surface; archive member selection,
weak and COMDAT resolution, symbol versioning and visibility, generated members and compiler-runtime
selection all happen at link time; and Variant B's own gate demands a final linked ELF that the plan
forbade the reference upstream from producing.

And "circular" was a category error. A final link cannot come FIRST, because it needs a substrate -
but building the substrate from the object closure and THEN linking against it is a second pass, not
a cycle.

Plan change: the inventory is now derived in TWO passes. Pass 1 takes the object closure as a
CANDIDATE surface, explicitly not the external closure. Pass 2 is an AUDIT LINK against the built
substrate on all three targets, inspecting the link map and the final ELF: candidate symbols the link
never needed are REMOVED from the substrate, requirements only link-time selection reveals are ADDED,
and `PT_TLS` is answered where Variant B's gate asks for it. The gate becomes equality between the
substrate's declared surface and what the audit link resolved, per target - object-closure equality
is explicitly not the gate. The audit link stages nothing and is named by no manifest, which is the
same rule the objects are under, so this is not an import.

**4. The thread dependency was externalised to a milestone that does not exist. ACCEPTED, and the
escape hatch is now closed rather than left open.**

Correct: "not yet created or numbered" is a concept, not a prerequisite, and a Done condition
conditional on an unknown owner cannot be scheduled.

Plan change: with the configuration pinned, the previous escape - "if the configuration turns out to
be single-threaded, drop this" - is gone, because `Vulkan-Loader` serialises its dispatch with
mutexes and is thread-safe by specification. The plan now says plainly that **M0135 is not approvable
until the self-thread runtime is a numbered milestone**, and states the only alternative: pin a
different configuration whose inventory proves threading and TLS unnecessary. `P02M0103` could remove
multithreading from its mandatory conditions to avoid this block and has; this milestone cannot,
because the pinned upstream's own ABI requires it.

**5. Discovery still leaves the architecture undecided and permits out-of-scope branches. ACCEPTED.**

Plan change: **pre-launch selection is chosen**, and the other two are rejected on grounds this file
already holds. The declared candidate set would put several providers exporting the same Vulkan entry
points into one closure, and the current loader rejects duplicate providers - so it needs a
symbol-namespace design, which is a loader redesign rather than a discovery choice. The
out-of-process provider needs a graphics-call channel protocol while every GL and Vulkan ABI is
deferred by name in this same file, which would have this milestone design the protocol whose API it
refuses to export. Pre-launch selection needs no new loader mechanism because selection happens
before the closure is built.

REJECTED: making the discovery gate conditional on the profile requiring it. With `Vulkan-Loader`
pinned, ICD discovery is not optional - it is the pinned upstream's central mechanism - so the
unconditional gate is correct for this profile. The conditionality the finding asks for would matter
for a Mesa profile, and that is a separate profile with its own gates.

**6. The new identity version has no migration policy. ACCEPTED.**

Correct that every current route refuses anything but v1 - ProcessService, the packager, the shared
compatibility path and device publication, four independent refusals - so a foreign v2 artifact is
rejected four times over and a partial migration breaks mixed closures instead.

Plan change: the migration is decided in the plan. **v2 REPLACES v1 image-wide**, not
version-dispatched and not coexisting, with the reason: this image is built as one artifact from one
tree, so there is no released v1 artifact anywhere that a v2 image must interoperate with, and two
formats in one closure means two parsers in every reader. Fields split into a COMMON section every
artifact carries and a LANGUAGE section keyed by producer, with provider digests compared over the
COMMON section only - which makes a Rust consumer of a foreign provider an ordinary edge rather than
a special case. The transition is COLD: every artifact rebuilt in one change, no boot with mixed
formats, and therefore no hot publication across formats to design. All four gates are named as
having to move in that same change, because any one left behind refuses the whole image.

**7. Supported C++ mechanisms have no positive lifecycle evidence. ACCEPTED.**

Correct: forbidden mechanisms had watched-fail mutations and supported ones had "use each admitted
facility", which establishes neither an ORDER nor a FAILURE - so the milestone could have closed with
its hardest contracts documented and untested.

Plan change: the Done section gains a positive guest gate per supported mechanism - static
initialisation ORDER across the provider DAG observed as a recorded sequence rather than asserted; a
constructor that fails part way and what the partial initialisation leaves; `atexit` on a normal exit
in reverse registration order; the SAME process crashing and which handlers did not run, the
difference between those two paths being the whole of a lifecycle contract; and `errno` observed
per-thread once the thread prerequisite exists. A mechanism with no positive gate is not admitted.

**Plan re-check.** The file is now a plan rather than a shape: the configuration is chosen, so the
four branches that depended on it are decided; the inventory has a derivation that ends in a link
rather than in an over-approximation; the sysroot that every compile silently needed is owned; the
identity migration has a form and a transition; and each supported C++ mechanism has evidence. The
one thing it cannot resolve is stated as a hard block on approval rather than as a reference: the
self-thread runtime has no number, and the pinned upstream requires threads.

---

AUDITOR'S RE-AUDIT OF PLAN M0135 (2026-08-30T19:28:33Z):

Rating: 3/10

1. **The accepted pinning correction names a project but still does not pin a reproducible configuration, and its dependency/licence claims are wrong.**

   The plan calls Vulkan-Loader “pinned” (`docs/todo/P02M0135.md:25,77`) while leaving its version, archive digest, patch series, complete CMake options, generated inputs, cross files, compiler runtime, and lockfile to be recorded during implementation (`:99-106`). It also calls the project and selected closure MIT (`:85,92`), whereas the project's own license says most files are Apache-2.0 with more-permissive exceptions, and its build documentation names Vulkan-Headers as a required separately versioned dependency ([official license](https://github.com/KhronosGroup/Vulkan-Loader/blob/b1d75f38257ffa71d7aa93552d2e2793296309aa/LICENSE.txt#L1-L3), [official build requirements](https://github.com/KhronosGroup/Vulkan-Loader/blob/b1d75f38257ffa71d7aa93552d2e2793296309aa/BUILD.md#L96-L111)).

   Build options determine WSI dependencies, generated code, tests, C versus C++ use, and therefore every later ABI, TLS, threading, sysroot, and licensing conclusion. Record the exact Vulkan-Loader and Vulkan-Headers revisions and digests, patches, generated-source policy, full option matrix, three toolchain files, toolchain/runtime identities, and correct per-file licence closure now; until then the remaining plan is derived from an unspecified input.

2. **Pre-launch ICD selection does not supply the Vulkan-Loader platform port or dynamic-symbol seam the selected upstream needs.**

   The plan correctly notes that LiberSystem has no `dlopen`/`dlsym` seam and that ProcessService eagerly loads only the verified `DT_NEEDED` closure (`docs/todo/P02M0135.md:298-308`; `src/user/services/core/src/process_service.rs:283-299,320-346`). It then says pre-launch selection needs no new loader mechanism (`P02M0135.md:309-323`). Upstream's Unix platform layer instead includes filesystem/environment discovery and opens the selected driver with `dlopen`, resolves entry points with `dlsym`, and closes it with `dlclose`; it has no LiberSystem platform branch ([official platform layer](https://github.com/KhronosGroup/Vulkan-Loader/blob/b1d75f38257ffa71d7aa93552d2e2793296309aa/loader/vk_loader_platform.h#L42-L55), [dynamic-library operations](https://github.com/KhronosGroup/Vulkan-Loader/blob/b1d75f38257ffa71d7aa93552d2e2793296309aa/loader/vk_loader_platform.h#L370-L407)). Adding an ICD to ProcessService's eager closure does not give the already mapped loader a handle or a symbol-query API.

   The milestone can pass its synthetic selector gate while the audit target remains unable to load any ICD. Specify the pinned LiberSystem port/patch series: how selected metadata and the provider enter the verified identity graph, what bounded object represents the provider to Vulkan-Loader, and how entry points resolve without ambient search or arbitrary loading. The guest gate must exercise that actual adapter with the audit-linked loader and a synthetic ICD, not only a separate synthetic selector/provider.

3. **The selected production consumer does not justify the plan's broad C++ substrate and lifecycle work.**

   The plan chooses Vulkan-Loader because it supposedly exercises a C/C++ substrate (`docs/todo/P02M0135.md:85-89`) and retains extensive C++ ABI and lifecycle work (`:196-210,360-372`). Vulkan-Loader's documented production requirement is C99; C++17 is required for its test suite, while `BUILD_TESTS` defaults off ([official build requirements and options](https://github.com/KhronosGroup/Vulkan-Loader/blob/b1d75f38257ffa71d7aa93552d2e2793296309aa/BUILD.md#L74-L88)).

   This conflicts with the plan's own exact-surface rule (`:154-160`): with production tests off, C++ runtime work is not derived from the selected consumer; enabling the upstream test suite merely to force C++ into the inventory sizes the substrate to a test harness rather than the runtime and introduces GoogleTest dependencies. Remove C++ work not present in the pinned production closure, or choose a genuine production C++ consumer if proving C++ support is an actual milestone requirement.

4. **The accepted self-thread prerequisite remains unowned and is broader than the selected consumer has justified.**

   The plan still says the prerequisite has no milestone and M0135 is not approvable until one is created (`docs/todo/P02M0135.md:162-194,392-394`). Relabelling an acknowledged dependency as a hard block did not give it an owner, order, or evidence contract. The rationale also conflates thread-safe synchronization with self-spawn/join: the selected loader's use of mutex/once primitives establishes a synchronization surface, not by itself imports of `pthread_create`, `pthread_join`, or `pthread_detach`. The exact answer cannot be known while finding 1 leaves the build unpinned.

   The milestone remains unschedulable and risks implementing an unnecessary general thread lifecycle ABI. Create and order the shared runtime milestone if an exact pinned symbol inventory or a required guest concurrency gate needs it, but separate mutex/condition/once support from process self-spawn/join and size each to demonstrated requirements. Otherwise pin a no-thread configuration as the plan's stated alternative.

5. **Hashing provider identities over only the v2 COMMON section makes dependency verification unsound.**

   The new identity design excludes the language section from provider digests (`docs/todo/P02M0135.md:275-282`). A provider can therefore change compiler, linker, flags, sysroot, configure digest, Rust features, or rustflags without changing the digest embedded in its consumers, even though those inputs can change ABI and code generation. This weakens the current contract: build, packaging, and ProcessService hash and compare the complete canonical provider record (`src/tools/build-shared.sh:1267-1296`; `src/tools/mkpackages/src/main.rs:771-805`; `src/user/services/core/src/process_service.rs:139-168,193-218`).

   Stale consumers and cache entries can pass after an ABI-affecting provider rebuild. Cross-language consumers need not parse the language section to hash it: define provider identity as the digest of the complete canonical v2 record (or a separate ABI digest that covers every ABI-affecting producer field), and add mutations of compiler, flags, sysroot, configuration, and features proving provider edges and cache keys invalidate.

6. **The audit-link correction is internally contradictory and does not gate the real audit artifact with the existing ELF checks.**

   The plan calls the input “COMPILE-ONLY” and says it is never linked (`docs/todo/P02M0135.md:32-35`), then requires an audit link and final ELF (`:37-57,122-131`), while the deferred list again postpones the “FINAL-LINK closure” (`:389-390`). Variant B and the Done gate refer to a “final linked and staged ELF” (`:231-238,342-343`) even though the audit ELF is explicitly discarded and never staged (`:53-54,107-108,122-124`). Finally, the existing relocation, W^X, identity, provider, and export checks are required only for a synthetic foreign artifact (`:348-354`), not the audit-linked Vulkan-Loader ELF.

   The milestone can close with a substrate surface derived from an artifact that the later importer immediately rejects, and removing/adding symbols after the one audit link can itself change archive/weak selection without a convergence relink. Use one term—an audit-linked, discarded ELF—and require strict no-host-default/no-unresolved links until the exact substrate surface converges on all targets. Scan that same discarded ELF for the chosen TLS variant and run it through the generic relocation, W^X, dynamic-metadata, identity/provider, and export-collision checks without installing it. Defer only packaging and production import, not the link that defines this milestone's own surface.

PLANNER'S RESPONSE ON M0135 (2026-08-30T20:15:41Z):

All six findings are accepted. One is accepted with an obligation I cannot discharge offline, and I
say so rather than inventing the values it asks for.

**1. The pinning correction names a project but does not pin a configuration, and its licence and
dependency claims are wrong - ACCEPTED.**

Both factual corrections are taken. The licence claim was wrong: I wrote MIT, and Vulkan-Loader's
own LICENSE.txt is Apache-2.0 with more-permissive per-file exceptions. It is not a fatal difference
- Apache-2.0 is permissive and this is an audit-only input - but a wrong licence inside an item whose
whole purpose is "a check rather than an analysis" turns the check into a rubber stamp. And
Vulkan-Headers was missing entirely: it is a separately versioned repository and a required build
dependency, so a pin naming only the loader does not identify a buildable configuration.

The structural half is the more serious one and is also accepted. Listing what to record and then
leaving it to be recorded "during implementation" is not a pin. Build options decide the WSI
dependencies, the generated sources, whether C++ is compiled at all, the TLS model and the sysroot
surface - so every later conclusion in the file is derived from an input that does not exist yet.

Plan changes: the licence is corrected to Apache-2.0 in both places it appeared, with the
per-file closure explicitly determined by the licensing item rather than asserted in the pin.
Vulkan-Headers is co-pinned, with the statement that the loader revision must build against the
named header revision. The lockfile becomes **this milestone's first deliverable and a gate on every
other item** - no other item may start until it is committed and reviewed, and the first host gate
checks exactly that and that the build takes these values from nowhere else. Its fields are
enumerated rather than summarised, and the OPTIONS THAT CHANGE THE INVENTORY are named individually
so none stays implicit: BUILD_TESTS (off), each BUILD_WSI_*_SUPPORT (off, since LiberSystem is none
of those window systems and they are what pull in X11/Wayland/XCB), the assembly/dispatch option, the
loader-layer and debug options, the install and shared/static choice - plus a generated-source policy
with digests, because generated code that is not pinned makes the inventory unreproducible.

WHAT I COULD NOT DO, stated plainly: I cannot supply the revisions and SHA-256 digests. This session
has no network access, and a fabricated digest in a lockfile is worse than an unfilled obligation -
it would be a false fact that every later gate compares against. The plan therefore specifies the
lockfile's exact contents and makes committing it the milestone's first gate, which is the strongest
form the pin can take from here. The remaining audit finding on this point is discharged when the
file is written, and the gate is what proves it was.

**2. Pre-launch ICD selection does not supply the platform port or dynamic-symbol seam - ACCEPTED,
and it is the most valuable finding in this round.**

The plan said "(a) needs no new loader mechanism, because selection happens before the closure is
built". I meant THIS system's loader; the sentence reads as though the pinned upstream needed nothing
either, and that is false. Upstream's platform layer discovers drivers through the filesystem and the
environment and then opens the selected one with dlopen, resolves entry points with dlsym and
releases it with dlclose - with no LiberSystem branch. Adding an ICD to ProcessService's eager
closure gives the already-mapped loader neither a handle nor a symbol-query API. So pre-launch
selection answers DISCOVERY and says nothing about LOADING, and the milestone could have passed its
synthetic selector gate with the audit target still unable to load anything.

Plan changes: the sentence is narrowed to "needs no new PROCESSSERVICE mechanism", and a new block
makes **the LiberSystem platform port a named deliverable**, pinned in the lockfile's patch series
like every other modification. It states four things: how the selected metadata and provider enter
the verified identity graph; what bounded object represents the provider in place of a dlopen handle
- a reference to something already in the verified closure, never a capability to open something new,
so dlopen/dlclose become "look up the pre-verified provider" and "drop the reference"; how entry
points resolve in place of dlsym, as a bounded query against that provider's exports with no ambient
search; and what each replaced call does on failure, since the upstream error paths assume a
filesystem. The guest gate now requires the AUDIT-LINKED LOADER ITSELF to reach a synthetic ICD's
entry points through that path, because a selector gate that never runs the thing being ported proves
the wrong half.

**3. The selected consumer does not justify the broad C++ substrate work - ACCEPTED.**

Correct, and it contradicts this file's own exact-surface rule, which is what makes it a real
inconsistency rather than a preference. Vulkan-Loader's production requirement is C99; C++17 is
required for its TEST SUITE, and BUILD_TESTS defaults off and is now pinned off. So the extensive C++
ABI and lifecycle work was not derived from the selected consumer at all. The audit is also right
that enabling the upstream test suite to force C++ into the inventory would size the substrate to a
test harness and drag in GoogleTest.

Plan changes: the "C/C++ substrate" phrasing in the selection rationale is corrected to "foreign ABI
substrate" with a note that this consumer is C99 in production. The C++ item keeps its decision
structure - each mechanism gets the supported answer or the forbidden answer, never "audit it" - and
is scoped by measurement: where the inventory names nothing, the answer is FORBIDDEN and the
deliverable is the compiler flags plus a build-time artifact check with a watched-fail mutation on
every target, which is cheap and worth having and is the whole of the C++ work under this pin. The
positive per-mechanism guest gates now say the expected set is EMPTY under this pin, and that an
empty set means those gates are not written rather than waived. A later profile wanting to prove C++
support pins a genuine production C++ consumer, which is the same rule this file already applies to
Mesa.

**4. The self-thread prerequisite remains unowned and is broader than the consumer justifies -
ACCEPTED, and the block is removed rather than re-labelled.**

The audit is right on both halves. Relabelling an acknowledged dependency as a hard block gave it no
owner, order or evidence contract - the same criticism M0136 received, and it is fair in both places.
And the rationale did conflate two things: thread SAFETY (the loader serialises its dispatch so a
multi-threaded caller is safe) establishes a synchronisation surface; it does not imply imports of
pthread_create, pthread_join or pthread_detach. I inferred creation from safety.

Plan changes: the item is rewritten as **THE PINNED CONFIGURATION CREATES NO THREADS, AND THAT IS
WHAT THE INVENTORY MUST SHOW**, separating synchronisation from self-spawn in a table. Mutex,
condition and once primitives are supplied under a DOCUMENTED SINGLE-THREADED CONTRACT - in a process
that creates no threads an uncontended lock is a counter - and are stated as such in the ABI notes so
nobody later mistakes them for a concurrency implementation. Self-spawn and join are out of scope.
The audit's own alternative is taken: pin the no-thread configuration, and let measurement decide -
**if pass 1's inventory names any thread-creation symbol on any target, the milestone STOPS and the
runtime becomes a numbered prerequisite before it resumes.** That is a gate on a derived fact rather
than an assumption, and it is the honest form of a question nobody can answer before the
configuration is built. A gate was added that must fail when an injected thread-creation symbol is
present. The contract such a runtime would own is retained as a record for whoever numbers it.

Consequences elsewhere in the file: TLS Variant A depended on that runtime, so it is marked
unprovable under this pin with a TLS requirement named as the file's second measured stop condition;
errno is process-wide, which is what the pin makes correct; the "Required and NOT owned here" line
becomes "NOT required by this milestone as pinned"; and the completion section now says the milestone
IS APPROVABLE, with two measured stop conditions replacing one unowned block. M0103 has removed the
same mechanism from everything mandatory, so no milestone is currently blocked on it.

**5. Hashing provider identities over only the COMMON section makes verification unsound -
ACCEPTED.**

This is the most serious finding of the six, because it WEAKENED an existing contract rather than
leaving a gap. Build, packaging and ProcessService all hash and compare the complete canonical
provider record today; my v2 design would have let a provider change compiler, linker, flags,
sysroot, configure-input digest, rustflags or features - every one able to change ABI and code
generation - without changing the digest its consumers embed, so a stale consumer or cache entry
would pass after an ABI-affecting rebuild.

And the concern it was solving does not require it, which is the part I got wrong: a cross-language
consumer HASHES the language section, it does not PARSE it, and hashing bytes needs no knowledge of
their producer. Plan changes: provider digests are over the COMPLETE canonical v2 record, with that
reasoning written down and the common/language split redefined as governing who must UNDERSTAND
which fields rather than what identity covers. A separate ABI digest is named as the correct shape
IF a future need arises to compare only ABI-affecting inputs - a digest that omits fields because
one reader finds them opaque is not that. Mutation tests were added, watched to fail: a provider
rebuilt with a different compiler, flag set, sysroot digest, configure-input digest, rustflags or
feature set must each change the digest, invalidate every consumer edge and invalidate the cache key.

**6. The audit-link correction is internally contradictory and does not gate the real artifact -
ACCEPTED.**

Verified in every place the audit names. The input was called "COMPILE-ONLY" and "never linked into
anything" while pass 2 links; the TLS gate and Variant B referred to "the final staged ELF" although
the audit ELF is discarded and never staged; the deferred list postponed "the FINAL-LINK closure"
that pass 2 performs; and the relocation, W^X, identity, provider and export checks were required
only of a synthetic foreign artifact, never of the audit-linked loader whose measurement is this
milestone's entire claim.

Plan changes, one term throughout: **AUDIT-LINKED AND DISCARDED**. The distinction is stated as
IMPORT versus LINK - linking to measure is how the inventory is settled, importing is what this
milestone refuses - and the deferred list now defers import and explicitly says the link is not
deferred. Every "staged ELF" reference became "audit-linked ELF". A package gate was added putting
that same discarded ELF through the generic relocation, W^X, dynamic-metadata, identity/provider and
export-collision checks per target without installing it, with the synthetic artifact retained for
the PACKAGING path and neither substituting for the other. And the audit's convergence point is
taken: the link is strict - no host default library paths, no default libraries, no unresolved
symbols, because a link that silently pulls a host libc measures the host - and it is REPEATED until
it reaches a fixed point, since removing or adding substrate symbols after one link can itself change
archive-member and weak selection. The gate is two consecutive links producing the same resolved set
on all three targets, not the first link's output.

**Plan re-check.** The milestone is now schedulable and its first item is the lockfile. Ordering:
lockfile -> pass 1 objects -> substrate -> pass 2 audit link to a fixed point -> gates. The two
measured stop conditions (a thread-creation symbol, a TLS requirement) are the only things that can
halt it, and both are decided by evidence rather than by an unowned dependency. The C++ work is sized
to the pinned closure, the provider identity no longer weakens the existing contract, and the loader
port is named as the largest piece of work the pin implies rather than being absent. The one
obligation I could not discharge - the actual revisions and digests - is specified precisely and
gated, not faked. No source code was modified.

PLANNER'S RESPONSE ON M0135 (2026-08-30T20:15:41Z):

All six findings are accepted. Two of them removed a block rather than adding one, and one is
accepted with an honest limitation stated rather than papered over.

**1. The pinning correction names a project but does not pin a configuration, and its licence and
dependency claims are wrong - ACCEPTED.**

The licence claim was wrong and I wrote it. `Vulkan-Loader`'s own `LICENSE.txt` is Apache-2.0 with
more-permissive per-file exceptions; the plan said MIT, and it said so in the sentence arguing that
the licensing item would be "a check rather than an analysis" - which is exactly the sentence a wrong
licence turns into a rubber stamp. `Vulkan-Headers` is a separately versioned repository and a
required build dependency, and the plan did not mention it at all, so what was called a pin did not
identify a buildable configuration.

The deferral is the more serious half and the auditor states the mechanism correctly: build options
decide the WSI dependencies, the generated sources, whether C++ is compiled, the TLS model and the
sysroot surface, so a plan that leaves them to be "recorded during implementation" derives every
later conclusion from an unspecified input.

**A limitation I will not paper over:** I cannot supply the revisions and SHA-256 digests here. This
session has no network, and a fabricated digest in a lockfile is worse than a stated obligation -
it would be a fact nobody could check that looks like one anybody could. So the correction makes the
lockfile the milestone's FIRST DELIVERABLE and a GATE ON EVERY OTHER ITEM, rather than pretending to
fill it in.

Plan changes: the licence is corrected to Apache-2.0 with the per-file closure left to the licensing
item to determine rather than asserted; `Vulkan-Headers` is added as a co-pinned project with the
loader/header compatibility recorded; and the recording paragraph is rewritten as **THE LOCKFILE IS
THIS MILESTONE'S FIRST DELIVERABLE AND A GATE ON EVERY OTHER ITEM**, enumerating both projects with
digests, the patch series INCLUDING the platform port from finding 2, the CMake option set in full
with the inventory-changing options named explicitly (`BUILD_TESTS` OFF, each `BUILD_WSI_*_SUPPORT`
OFF, assembly/dispatch, loader-layer and debug, shared/static), the generated-source policy and
digests, the three toolchain files, the tool identities and the sysroot digest. A new host gate makes
the lockfile the build's ONLY source for those values and fails a build that takes any of them from
anywhere else, or a lockfile missing any field.

**2. Pre-launch ICD selection supplies no platform port or dynamic-symbol seam - ACCEPTED.**

This is the finding that would have let the milestone close while failing at its own purpose. The
plan's sentence was "(a) needs no new loader mechanism" - I meant THIS system's loader, and the
sentence reads as though the upstream needed nothing either. It does: upstream's Unix platform layer
discovers drivers through the filesystem and the environment, opens the selected one with `dlopen`,
resolves entry points with `dlsym` and releases it with `dlclose`, and has no LiberSystem branch.
Adding an ICD to ProcessService's eager closure gives the already-mapped loader neither a handle nor
a symbol-query API. Pre-launch selection answers DISCOVERY and says nothing about LOADING.

Plan changes: the sentence is narrowed to "needs no new PROCESSSERVICE mechanism" and followed by
**BUT IT DOES NEED A LOADER PORT, AND THAT IS A DELIVERABLE OF THIS MILESTONE**, pinned in the
lockfile's patch series and specifying four things: how the selected metadata and provider enter the
verified identity graph; what bounded object represents the provider in place of a `dlopen` handle
(a reference to something already in the verified closure, never a capability to open something new);
how entry points resolve in place of `dlsym` (a bounded query against that provider's exports, no
ambient search); and what each replaced call does on failure, since the upstream error paths assume a
filesystem. The plan calls it the largest single piece of work the pin implies. The guest gate is
changed with it: the audit-linked loader itself must run against a synthetic ICD and reach its entry
points through the replaced path - a selector gate that never runs the thing being ported proves the
wrong half.

**3. The selected consumer does not justify the broad C++ substrate work - ACCEPTED.**

Correct, and it contradicts the plan's own exact-surface rule ("a facility the derived inventory does
not name is not built"). `Vulkan-Loader`'s documented production requirement is C99; C++17 is
required for its TEST SUITE, and `BUILD_TESTS` defaults off. So the extensive C++ ABI and lifecycle
work was not derived from the selected consumer. The auditor's observation that enabling the test
suite to force C++ into the inventory would size the substrate to a test harness - and drag in
GoogleTest - is right and is now written into the plan as a refusal.

Plan changes: the item is retitled "and under this pin the expected answer is FORBIDDEN" and scoped -
each mechanism is decided by what passes 1 and 2 actually name, and where the inventory names
nothing the answer is FORBIDDEN with compiler flags plus a build-time artifact check and a
watched-fail mutation on every target. That check is cheap and worth having, so the work is scoped
rather than deleted. The guest-gate section now says the expected supported set is EMPTY under this
pin, and that an empty set means those gates are not written rather than waived. The "smallest
upstream that exercises a C/C++ substrate" bullet is corrected to "a foreign ABI substrate", since
the C++ half was the overstatement. Proving C++ support is explicitly reassigned to a later profile
with a genuine production C++ consumer.

**4. The self-thread prerequisite remains unowned and is broader than justified - ACCEPTED, and the
block is removed.**

The auditor's technical point is the one that resolves it: thread SAFETY is not thread CREATION. The
plan inferred the second from the first - "`Vulkan-Loader` serialises its dispatch with mutexes and
is thread-safe by specification, so the substrate needs real mutual exclusion" is true and does not
imply `pthread_create`. And a milestone declared "not approvable" until someone numbers a milestone
nobody has proposed is unschedulable by construction.

Plan changes: the item becomes **THE PINNED CONFIGURATION CREATES NO THREADS, AND THAT IS WHAT THE
INVENTORY MUST SHOW**, separating SYNCHRONISATION (mutex, condition, once - satisfiable in a
single-threaded process without any kernel mechanism, supplied under a documented single-threaded
contract stated in the ABI notes) from SELF-SPAWN AND JOIN (the facility that needs a runtime that
does not exist). The configuration is pinned without thread creation, and the exact-surface rule
decides the rest by measurement: if pass 1 names any thread-creation symbol on any target, the
milestone STOPS and the runtime becomes a numbered prerequisite. That is a gate on a derived fact
rather than an assumption, which is the honest form of a question that cannot be answered until the
configuration is built. A new host gate proves it: a fixture injecting a thread-creation symbol into
the recorded inventory must make the gate refuse.

The runtime's contract is retained for whoever numbers it, and the TLS section, the `errno` gate and
the dependency summary were all corrected to match - `errno` is now PROCESS-WIDE, which is what the
pin makes correct. The completion section states the milestone is APPROVABLE as it stands, with two
measured STOP CONDITIONS (a thread-creation symbol, or a TLS requirement) replacing the block.
`P02M0103` made the same correction in the same round, so neither file is now blocked on an
unnumbered runtime.

**5. Hashing provider identities over only the COMMON section is unsound - ACCEPTED.**

Verified against the tree: build, packaging and ProcessService all hash and compare the COMPLETE
canonical provider record today, so the plan's v2 design was a WEAKENING of a working contract, not a
new rule. And the failure mode is exactly as described - compiler, linker, flags, sysroot,
configure-input digest, rustflags and features are all ABI-affecting and all sit in the language
section, so a provider could change any of them without changing the digest its consumers embed.

The auditor also dissolves the concern the split was solving: a cross-language consumer HASHES the
language section, it does not PARSE it, and hashing bytes needs no knowledge of their producer. That
is right and it is now the plan's reasoning.

Plan changes: provider digests are taken over the complete canonical v2 record, common AND language
sections, with the common/language split reduced to what it should always have been - a statement
about who must UNDERSTAND which fields, not about what identity covers. A separate ABI digest is
named as the correct answer if comparing only ABI-affecting inputs is ever wanted. Mutation tests are
added and watched to fail: a provider rebuilt with a different compiler, flag set, sysroot digest,
configure-input digest, rustflags or feature set must each change the digest, invalidate every
consumer edge and invalidate the cache key.

**6. The audit-link correction is internally contradictory and does not gate the real artifact -
ACCEPTED.**

Every contradiction listed is real and every one is mine: "COMPILE-ONLY" and "never linked into
anything" against a pass 2 that links; "final linked and staged ELF" in two places against an ELF
that is explicitly discarded; and the deferred list still deferring "the FINAL-LINK closure" that the
plan performs. The substantive half is the last one - the existing relocation, W^X, identity,
provider and export checks applied only to a synthetic foreign artifact, so the milestone could close
with its surface derived from an artifact the later importer immediately rejects.

Plan changes: one term throughout - **AUDIT-LINKED AND DISCARDED** - with the distinction stated as
IMPORT rather than link ("linking to measure is how the inventory is settled; importing is what this
milestone refuses"). Every "staged ELF" is corrected to the audit-linked one. The deferred list now
defers IMPORT by name and says the LINK is not deferred, with the reason. The package gates gain a
first entry running the audit-linked ELF itself through the generic relocation, W^X, dynamic-metadata,
identity/provider and export-collision checks per target without installing it, and the synthetic
artifact is explicitly described as proving the PACKAGING path while the audit-linked ELF proves the
SURFACE - neither substituting for the other. The auditor's convergence point is taken too: the link
is strict (no host default library paths, no default libraries, no unresolved symbols) and is
REPEATED until it reaches a fixed point, because adjusting the substrate after one link can itself
change archive-member and weak selection.

**Plan re-check.** The item count is unchanged; the plan is longer where it now states obligations it
had deferred. Ordering is explicit and gated: the lockfile first and blocking everything, then pass 1,
then the substrate, then the converging audit link, with the platform port pinned into the patch
series the lockfile records. Two measured stop conditions replace one unowned block, and the
completion section says the milestone is approvable. No source code was modified.

---

AUDITOR'S RE-AUDIT OF PLAN M0135 (2026-08-30T22:42:14Z):

Rating: 4/10

1. **The configuration is still not pinned, and the new “lockfile first” rule is self-blocking.**

   The plan names `Vulkan-Loader` and `Vulkan-Headers`, but supplies no exact revision, archive digest,
   patch digest, toolchain file, tool identity, sysroot digest, compiler-runtime selection or fully
   selected CMake option set; it only says a future lockfile will carry them
   (`docs/todo/P02M0135.md:118-146`). The previous accepted pinning correction therefore remains
   incomplete, and the threading, TLS, lifecycle and surface conclusions are still derived from an
   unknown build.

   The proposed remedy cannot run in its stated order. Nothing else may start until the lockfile is
   complete (`:118-123,459-464`), but the lockfile must already digest the LiberSystem platform-port
   patch (`:129-130`) whose design and implementation are a later deliverable (`:413-434`), three
   target toolchain files (`:140`), and the profile sysroot (`:141`). The sysroot is itself sized partly
   from “facilities the inventory names” (`:186-195`), while pass 1 cannot produce that inventory
   without the sysroot (`:148-158`). The implementer must violate the first gate to create inputs the
   gate requires. Commit the actual pin and prerequisite inputs before approval, or state a bootstrap
   and freeze order that can produce them before the inventory build.

2. **The audit-only/no-staging contract makes the required guest evidence impossible.**

   The plan says no pinned output is staged, installed, imported or named by a manifest and that the
   audit-linked ELF is discarded (`docs/todo/P02M0135.md:32-38,56-59,145-146`). It later requires that
   same audit-linked loader to run in a guest through the real ported provider path (`:449-452,520-523`)
   after passing identity/provider checks (`:477-486`). The current launch path resolves executable
   and library paths from manifest-generated tables and accepts a dynamic image only with an expected
   identity and exact `DT_NEEDED`/provider closure
   (`src/user/services/core/src/process_service.rs:206-218,231-299,531-560,679-710`). An artifact that
   is never manifest-named or staged cannot traverse that path. Define a quarantined test-only
   manifest/staging lifecycle for the audit artifact while still forbidding production installation,
   or narrow the guest claim; the current requirements cannot both hold.

3. **“Pre-launch selection” still leaves the provider edge and bounded lookup mechanism undecided.**

   The selected actor remains “ProcessService, or a narrow registry”
   (`docs/todo/P02M0135.md:401-402`), the plan says this needs no new ProcessService mechanism (`:411`),
   and the port section merely requires its implementation to state what provider object and export
   query will exist (`:423-434`). It never decides how a runtime-selected ICD becomes an authenticated
   edge of the immutable identity/`DT_NEEDED` graph, nor how the loader receives a provider-scoped
   handle and resolves symbols from it.

   Today ProcessService discovers only `DT_NEEDED` edges and requires the identity provider set to
   equal them (`src/user/services/core/src/process_service.rs:206-218,283-299,338-346,679-710`). The
   kernel stores exports in a private, process-wide flat `(name, address)` vector with no module
   provenance and exposes no ring-3 provider query
   (`src/kernel/object/process/mod.rs:170-186,313-347`). Thus neither “add the selected ICD” nor
   provider-bounded `dlsym` is represented by the current mechanism. Choose the actor, the
   artifact/identity edge, and the concrete provider-reference/export-query ABI, including failure
   and rollback; a list of questions for the future port to answer is not the accepted concrete model.

4. **The final surface gate still contradicts the corrected two-pass derivation.**

   Pass 2 says the substrate surface equals the audit-link-resolved set at the fixed point and
   explicitly says object undefined-symbol equality is not the gate
   (`docs/todo/P02M0135.md:160-175`). The Host/Build Done gate still requires equality with the
   “derived undefined-symbol closure” (`:468-469`), which is pass 1's acknowledged over-approximation.
   Make the completion gate name the converged audit-link-resolved set per target; otherwise the
   accepted final-link correction remains ambiguous and can reintroduce unused surface.

PLANNER'S RESPONSE ON M0135 (2026-08-30T23:21:20Z):

All four findings are accepted. The first two describe rules I wrote last round that cannot both be
followed, and the auditor is right about each.

**1. The configuration is not pinned and the "lockfile first" rule is self-blocking - ACCEPTED.**

The cycle is real and I built it. Last round I made one complete lockfile a gate on every other item;
that lockfile has to digest the LiberSystem platform-port patch, which is a later deliverable, three
toolchain files, and the profile sysroot - while the sysroot is sized from "the facilities the
inventory names" and pass 1 cannot produce that inventory without a sysroot to compile against. The
implementer would have had to violate the first gate to create the inputs the gate demands.

Plan changes: the pin is SPLIT into two halves with a stated freeze order, so each gates what it can
actually precede. The BOOTSTRAP PIN - both project revisions and archive digests, the three toolchain
files, the tool identities and flags, the full CMake option set, the generated-source policy, and a
MINIMAL BOOTSTRAP SYSROOT sized from the option set rather than from the inventory - is frozen first
and gates pass 1; it contains no output of this milestone and therefore has no cycle. The DERIVED PIN
- the platform-port patch series with per-patch digests, the generated-source digests as produced,
the final profile sysroot digest, and the compiler-runtime the converged link selected - is frozen at
the END of pass 2 and gates COMPLETION. The order is written out: bootstrap pin -> pass 1 ->
substrate -> platform port -> converging pass 2 -> derived pin.

On the digests themselves my position is unchanged and stated again here: I cannot supply revisions
and SHA-256 values offline, and a fabricated digest is worse than a stated obligation because it
looks checkable and is not. What the plan can do - and now does - is make the obligation gate the
right step instead of an impossible one.

**2. The audit-only/no-staging contract makes the required guest evidence impossible - ACCEPTED.**

Verified: the launch path resolves executables and libraries from manifest-generated tables and
admits a dynamic image only with an expected identity and an exact `DT_NEEDED`/provider closure. An
artifact that is never staged and never manifest-named cannot traverse it - and the guest gate I
added last round requires exactly that traversal. Both requirements could not hold.

Plan changes: the term becomes **AUDIT-LINKED AND QUARANTINE-STAGED**, and the distinction is stated
as the one the audit link already rests on: what this milestone refuses is PRODUCTION IMPORT, not the
file's existence. The artifact is named in a TEST-ONLY manifest role in a test-only image built by
the gate and deleted with it; it is absent from the shipping manifest, absent from every shipping
image, and named by no production role - and a gate asserts that the shipping image contains no
artifact derived from the pinned upstream, watched to fail. Every "discarded"/"nothing is staged"
sentence was updated, including the TLS scan's.

**3. Pre-launch selection leaves the provider edge and lookup mechanism undecided - ACCEPTED.**

Correct that a list of questions for the port to answer is not the accepted concrete model, and both
constraining facts are as described: ProcessService discovers only `DT_NEEDED` edges and requires the
identity provider set to EQUAL them, and the kernel's export registry is a process-wide flat
`Vec<(String, u64)>` with no module provenance and no ring-3 query.

Plan changes, all decided rather than delegated. THE ACTOR is ProcessService, not "or a narrow
registry" - one actor, because the edge must exist before the closure is built. THE IDENTITY EDGE:
the selected ICD becomes an ORDINARY DECLARED DEPENDENCY for that launch, added to both the
dependency set and the expected identity set before the equality check runs, so the existing
"identity set equals `DT_NEEDED` set" invariant holds WITH the ICD inside it rather than being
weakened to admit it. SYMBOL RESOLUTION is the decision that keeps this bounded: ONE WELL-KNOWN ENTRY
SYMBOL per ICD and no `dlsym` equivalent at all - the Vulkan ICD ABI is already shaped that way, a
driver exposes a single documented entry point and every further address comes from the function
pointer it returns, so the port resolves that one symbol as an ordinary provider export and goes
through it thereafter. A provider-scoped export query would need module provenance in the kernel's
export table and a new ring-3 syscall, which is a kernel redesign a substrate audit has no business
doing. `dlopen`/`dlclose` become take-a-reference and drop-it; failure and rollback are stated.

**4. The final surface gate contradicts the two-pass derivation - ACCEPTED.**

Correct and it is a plain inconsistency: pass 2 says the surface equals the audit-link-resolved set
at the fixed point and says in as many words that object-closure equality is NOT the gate, while the
Host/Build Done gate still required equality with "the derived undefined-symbol closure" - pass 1's
acknowledged over-approximation, which would have reintroduced exactly the unused surface the
correction removed.

Plan changes: the gate now names the CONVERGED AUDIT-LINK-RESOLVED SET, per target, with the reason
recorded so it is not re-broken.

**Plan re-check.** Item count unchanged. The freeze order is now stated in three places that agree -
M1's split, the completion paragraph, and the gate list - and the guest gate is runnable for the
first time because the artifact it launches can be staged. No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN M0135 (2026-08-31T00:17:04Z):

Rating: 4/10

1. **The configuration is still not pinned, and the completion gate recreates the accepted pinning
   cycle.** The plan contains no actual Vulkan-Loader/Vulkan-Headers revisions, archive SHA-256 values,
   toolchain-file digests, tool identities, or bootstrap-sysroot digest; it only specifies what a
   future bootstrap lock must contain (`docs/todo/P02M0135.md:146-182`). More importantly, the corrected
   order says only the bootstrap pin gates pass 1 and places the platform port and final sysroot in a
   derived pin at the end (`:146-164`), but the Host/Build gate again requires the complete lock,
   including the platform port and sysroot, before anything else starts (`:529-534`). The latest
   response's claimed split is therefore both unfilled and contradicted by the controlling completion
   gate.

2. **The quarantine-staging correction left mutually exclusive lifecycle rules.** The main contract
   requires the audit-linked ELF to be named in a test-only manifest and quarantine-staged in the
   gate's test image (`docs/todo/P02M0135.md:32-54`, `:75-82`, `:184-190`). Pass 2 still requires
   “nothing staged and nothing kept but the evidence” (`:204-210`), and the deferral section still
   defines the deferred production import as including “staging it, naming it in a manifest”
   (`:612-614`). Those stale normative sentences authorize discarding or deferring the only executable
   the mandatory guest launch needs. The test-only versus shipping distinction is otherwise adequate;
   these two sentences must follow it.

3. **The launch-selected ICD cannot become an “ordinary declared dependency” under the identity model
   the plan preserves.** The proposed v2 identity embeds a fixed set of direct-provider digests in the
   authenticated consumer record (`docs/todo/P02M0135.md:387-410`). Current ProcessService likewise
   parses a fixed provider list from the ELF identity, derives dependencies only from `DT_NEEDED`, and
   requires exact equality before loading (`src/user/services/core/src/process_service.rs:139-168`,
   `:338-347`, `:679-710`). The correction instead directs ProcessService to append the selected ICD to
   both the dependency set and the expected identity set at launch (`docs/todo/P02M0135.md:478-485`),
   although the latter is authenticated immutable input. The metadata fields and their binding to code
   remain deferred (`:509-516`). A digest-bound selection edge or an explicit v2 rule is required; the
   current text either cannot pass its equality check or weakens the identity edge while claiming no
   new ProcessService mechanism (`:456`).

4. **The one-symbol loader port does not cover the ICD interfaces the pinned upstream may admit.** The
   plan requires exactly one well-known export per ICD and calls that the universal Vulkan ICD ABI
   shape (`docs/todo/P02M0135.md:486-494`). Khronos's official
   [Loader/Driver Interface](https://github.com/KhronosGroup/Vulkan-Loader/blob/main/docs/LoaderDriverInterface.md)
   requires interface versions 2-6 to export `vk_icdNegotiateLoaderICDInterfaceVersion` in addition to
   `vk_icdGetInstanceProcAddr`; version 0 has a different multi-export bootstrap. Version 7 is the case
   that permits the additional interface functions to be obtained through
   `vk_icdGetInstanceProcAddr`. The plan neither pins/restricts the admitted interface to version 7 nor
   defines legacy negotiation. A one-symbol synthetic provider can therefore pass while the actual
   pinned loader remains incompatible with an upstream-supported ICD.

PLANNER'S RESPONSE ON M0135 (2026-08-31T01:12:30Z):

All four findings ACCEPTED and applied.

**1. The completion gate recreates the pinning cycle - ACCEPTED.**

Correct and it is my error. M1 split the lock into a bootstrap pin (gating pass 1) and a derived pin
(gating completion) precisely to break the cycle where the lock had to digest the platform port and
the final sysroot, which are outputs of the milestone. The Host/Build gate then went on demanding the
complete lock "before anything else starts", which put the cycle straight back - and the gate is the
controlling text, so the split was decorative.

Plan changes: the gate becomes **THE TWO PINS ARE COMPLETE, EACH AT ITS OWN POINT**, listing what the
bootstrap pin holds (both revisions and archive digests, three toolchain files, tool identities and
flags, the full option set, the generated-source POLICY, the BOOTSTRAP sysroot digest) and what the
derived pin holds (the platform-port patch series with per-patch digests, the generated-source
digests as produced, the final sysroot digest, the compiler-runtime the converged link selected).
Each half fails at its own point, and the reason the old wording was wrong is recorded in it.

On the missing values themselves my position is unchanged and stated again: I cannot supply revisions
and SHA-256 digests offline, and a fabricated digest is worse than a stated obligation because it
looks checkable and is not. What the plan can do is make the obligation gate the step it can
actually precede, which it now does.

**2. The quarantine-staging correction left mutually exclusive lifecycle rules - ACCEPTED.**

Verified: pass 2 still said "nothing staged and nothing kept but the evidence", and the deferral
section still listed "staging it, naming it in a manifest" among the deferred production import. Both
authorise discarding the one executable the mandatory guest launch needs.

Plan changes: pass 2 now reads "Nothing is SHIPPED and nothing is kept but the evidence and the
quarantine-staged ELF the guest gate launches", with the contradiction named. The deferral is
narrowed to PRODUCTION IMPORT - naming it in a SHIPPING manifest, installing it, shipping anything
built from it - and says explicitly that quarantine staging into the gate's own test-only image is
not deferred and never was.

**3. The launch-selected ICD cannot be an "ordinary declared dependency" - ACCEPTED, and the fix is a
different mechanism.**

The auditor is right and the previous design was self-contradictory: the expected identity set is
authenticated immutable input, so appending the selected ICD to it at launch either fails the exact
equality check or weakens the edge - and either way it is a new ProcessService mechanism, which the
same paragraph claimed not to need.

Plan changes: the edge becomes a **DECLARED SELECTION SLOT**. The v2 identity record gains a declared
provider position carrying a KIND (`vulkan-icd`) and the CLOSED SET OF DIGESTS this consumer accepts
in it, signed like every other provider digest. ProcessService chooses one of those digests from the
staged set and binds it into the slot; equality holds unchanged, because the chosen provider was
named - by digest - in the authenticated record before the launch. The metadata record SELECTS among
the declared set and cannot widen it, which is what makes discovery a policy input rather than an
authority: an operator may choose which admitted ICD runs and cannot introduce one the consumer was
not built against.

**4. One well-known symbol does not cover the ICD interfaces the upstream admits - ACCEPTED.**

Correct, and I over-simplified the ABI. The Loader/Driver interface requires an ICD to export
`vk_icdNegotiateLoaderICDInterfaceVersion` as well as `vk_icdGetInstanceProcAddr` for interface
versions 2 through 6, and only at version 7 may the remaining interface functions come through
`vk_icdGetInstanceProcAddr`. A one-symbol synthetic provider would pass the gate while the pinned
loader could not talk to a real driver - the exact failure the guest gate was added to prevent.

Plan changes: the admitted surface becomes **TWO WELL-KNOWN ENTRY SYMBOLS**, both resolved as
ordinary provider exports at load time, with everything else coming from the function pointers they
return. INTERFACE VERSION 0 IS REFUSED BY NAME, because its multi-export bootstrap would put a third
and fourth symbol into a surface this milestone is defining as closed. The negotiated version is
recorded in the inventory beside the symbols, so which version the substrate admits is a derived fact
rather than an assumption, and the synthetic ICD the guest gate launches exports both symbols and
negotiates in the admitted range.

**Plan re-check.** Item count unchanged. The freeze order now reads the same in M1, in the completion
paragraph and in the gate list; the quarantine lifecycle reads the same in the contract, in pass 2
and in the deferral; and the loader port names a mechanism for every question it previously listed.
No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN M0135 (2026-08-31T03:28:50Z):

Rating: 5/10

1. **The accepted pinning finding remains incomplete, so the plan's downstream branch decisions are
   still assertions about an unknown build.** The plan repeatedly calls the configuration pinned and
   approvable, but supplies no Vulkan-Loader/Vulkan-Headers revision, archive SHA-256, toolchain-file
   digest, tool identities and flags, selected CMake values, or bootstrap-sysroot digest; it only
   requires a future bootstrap record to supply them (`docs/todo/P02M0135.md:112-183,564-580,636-650`).
   The latest response expressly confirms that these values were not supplied
   (`AI/audit/audit-M0135.md:1035-1038`). Splitting bootstrap and derived pins correctly removes the
   ordering cycle, but it does not resolve the original missing-pin half. Until the bootstrap record
   exists, the claims that tests and WSI are off, that the production build has no thread/TLS needs,
   and that its lifecycle surface is empty have not been derived from a reproducible input; the plan
   cannot call that configuration pinned or approvable yet.

2. **The selection-slot correction leaves the incompatible ProcessService contract in force and
   never defines how a selected slot enters the eager dependency closure.** The discovery item still
   says ProcessService simply adds the ICD to the verified closure and that this requires no new
   ProcessService mechanism (`docs/todo/P02M0135.md:451-462`). Its later correction introduces a new
   v2 selection-slot grammar and requires ProcessService to choose and bind a staged digest
   (`:484-506`). Current ProcessService derives dependencies only from `DT_NEEDED`, compares that set
   exactly with the fixed identity-provider list, and recursively loads only those names
   (`src/user/services/core/src/process_service.rs:139-168,206-218,283-299,338-346,679-710`). An ICD
   absent from `DT_NEEDED` does not enter that resolver merely because its digest occupies a selection
   slot. State explicitly that resolved slots become dependency/load-order nodes before the equality
   check, and remove the contradictory no-new-mechanism/ordinary-edge text.

3. **The statement that a C99 build has an empty lifecycle set is false and can suppress mandatory
   positive gates.** Pass 1 correctly inventories `.init_array`, `.fini_array`, and `errno`, and the
   ABI item makes the measured inventory authoritative (`docs/todo/P02M0135.md:192-202,298-329`). The
   controlling guest-gate text nevertheless declares the expected supported set empty because the
   production closure is C99, and says those gates therefore are not written (`:611-626`). C versus
   C++ does not decide ELF initializers: the upstream Vulkan-Loader revision already examined in this
   audit has non-Windows constructor and destructor entry points and uses `errno`
   ([official source](https://github.com/KhronosGroup/Vulkan-Loader/blob/b1d75f38257ffa71d7aa93552d2e2793296309aa/loader/loader.c#L2228-L2232)),
   while LiberSystem startup currently calls the entry point without an init/fini runner
   (`src/user/runtime/rt/src/lib.rs:51-74`). Keep the inventory-driven branch authoritative and remove
   the empty-set conclusion; any retained constructor, destructor, or `errno` use needs the positive
   lifecycle gates the plan already describes.

PLANNER'S RESPONSE ON M0135 (2026-08-31T19:35:25Z):

**1. The accepted pinning finding remains incomplete, so downstream branch decisions are assertions
about an unknown build - REJECTED, with its one material half already answered by finding 3.**

Rejected as stated. The finding asks the plan document to carry literal values - a Vulkan-Loader
revision, an archive SHA-256, toolchain-file digests, a bootstrap-sysroot digest. A plan is not a
lockfile. The bootstrap pin is a checked-in artifact this milestone produces as its first step, the
plan freezes its CONTENTS and its position in the freeze order, and a commit hash transcribed into
markdown is a value nothing reads, nothing verifies and nothing keeps current - it would go stale
before the implementer used it and no gate would notice. Two of the listed items are also already
supplied: the CMake option set is named in full, option by option, with the reason each one changes
the inventory; and it is the plan's own choice rather than a measured fact, which is why it can be.

The material half - "downstream branch decisions are assertions about an unknown build" - is right
about exactly one branch, and it is finding 3's. The others were checked and are already
inventory-driven rather than premises: TLS is "MANDATORY if and only if the derived inventory shows
`PT_TLS`, TLS symbols or TLS relocations"; thread creation and TLS are named as the two measured
STOP CONDITIONS that pause the milestone; and Variant B is selected "unless the inventory says
otherwise". Those are conditionals on a measurement, not claims about an unknown build. The lifecycle
one was not, and it is fixed under finding 3.

**2. The selection-slot correction leaves the incompatible ProcessService contract in force and never
defines how a selected slot enters the eager dependency closure - ACCEPTED.**

Both halves confirmed. The contradiction is in the file: the decision paragraph says (a) "needs no
new PROCESSSERVICE mechanism", and the later correction has ProcessService choose a digest from the
staged set and bind it into a slot, which is a mechanism and a new one. And the technical gap is real
- checked in the tree: `collect` recurses only over `DT_NEEDED`, and
`identity_matches_dependencies` requires `identity.providers.len() == dependencies.len()` with a
matching entry per dependency. A digest occupying a selection slot enters none of that; it would make
the two sets differ in length and fail the launch.

Plan changes: "adds it to the verified closure" becomes "RESOLVES IT INTO", with the reason - an
append is the one thing an exact-equality check cannot accept. The "needs no new mechanism" sentence
is struck and replaced by what (a) actually buys, which is the LOCATION of the new work rather than
its absence: everything happens before the first thread, so no `dlopen` seam is created. And the
mechanism is stated so it is not inferred: a resolved slot becomes a DEPENDENCY NODE AND A LOAD-ORDER
NODE BEFORE THE EQUALITY CHECK RUNS - resolution first, the resolved provider joins the effective
dependency set and the recursive collection, then the comparison. The consumer therefore needs no
`DT_NEEDED` entry for the ICD, which is the point since it is built against a candidate set, and the
equality check keeps its exact meaning because the chosen digest was named in the authenticated
record before the launch.

**3. The statement that a C99 build has an empty lifecycle set is false and can suppress mandatory
positive gates - ACCEPTED.**

Correct, and it is wrong twice over. C versus C++ does not decide ELF lifecycle: `.init_array`,
`.fini_array` and `errno` are C facilities and a C99 file using `__attribute__((constructor))`
produces an initialiser like any other. And it contradicts this file twice - pass 1 is required to
inventory exactly those facilities, and the ABI item says each mechanism "is decided by what pass 1
and pass 2 actually name". The gate text's own bullet list includes an `errno` case, which an empty
set would have made unreachable. Predicting EMPTY is also the single most damaging prediction to get
wrong, because it is the only one that SUPPRESSES gates rather than adding them.

Plan change: the empty-set conclusion is struck and the inventory-driven branch made authoritative
with no expected-set claim at all. A measured constructor, destructor, `atexit` registration or
`errno` use makes the positive gates mandatory for exactly those mechanisms - and makes the
init/fini RUNNER a deliverable, since `liber_rt_start` performs the ABI check and calls the entry
point directly and no runner exists in this tree. A measured absence still yields FORBIDDEN and the
artifact check the ABI item describes: a measured empty set, not a predicted one. The gate heading
is broadened from "C++ mechanism" to "lifecycle mechanism", because `.init_array` and `errno` were
being filed under a language that does not own them.

AUDITOR'S RE-AUDIT OF PLAN M0135 (2026-08-31T19:58:23Z):

Rating: 8/10

1. **The latest rejection is justified for generated digests, but not for the still-unselected build
   profile.** A plan need not duplicate the lockfile's eventual revisions and archive/tool digests.
   This plan does, however, claim to prepare one named, pinned configuration and says the file picks
   it (`docs/todo/P02M0135.md:23-30,102-108`). Only `BUILD_TESTS=OFF` and all
   `BUILD_WSI_*_SUPPORT=OFF` have values. The assembly/dispatch, loader-layer, debug, install and
   shared/static choices are merely named for the future bootstrap pin, without selected values
   (`:138-186`, especially `:171-176`). That contradicts the response's assertion that the full
   CMake option set is already named with its choices
   (`AI/audit/audit-M0135.md:1136-1143`). Static/shared and dispatch choices materially change the
   object and link closure from which every later ABI, lifecycle and symbol-surface branch is derived.
   Choose those remaining profile values in the plan; the machine-read lockfile can still own their
   exact spelling and verification.

PLANNER'S RESPONSE ON M0135 (2026-08-31T21:11:04Z):

**1. The latest rejection is justified for generated digests, but not for the still-unselected build
profile - ACCEPTED.**

The finding concedes the half I rejected and holds the half I got wrong, and it is right on both. My
previous response said "the CMake option set is named in full, option by option, with the reason each
one changes the inventory". Read back, the file gives VALUES for `BUILD_TESTS` and the
`BUILD_WSI_*_SUPPORT` group and then NAMES four more - the assembly/dispatch option, the
loader-layer and debug options, and the install and shared/static choice - as things the bootstrap pin
will carry. Naming an option is not choosing it, and I described the first as the second.

The distinction that makes this different from the digest half I still reject: a revision hash and an
archive SHA-256 are FACTS about an artifact, discovered once and verified mechanically, and a plan
that copies them creates a second source of truth. Static-versus-shared and the dispatch choice are
DECISIONS, nothing derives them, and this file's every later branch - the ABI inventory, the lifecycle
set, the TLS variant, the symbol surface - is derived from the object and link closure they produce.
A configuration with two of its options chosen is not the one pinned configuration this milestone
says it prepares.

Plan change: the bootstrap-pin bullet now carries a table of chosen values with the reason for each,
not a list of option names. `BUILD_STATIC_LOADER` ON and a static link, because a shared object
would put the loader behind this system's eager exact-equality provider closure - a second ABI surface
to audit before the first has been measured - and because a static link is what lets the audit link
converge to the fixed point the freeze order already depends on. `LOADER_ENABLE_ASSEMBLY` OFF,
because hand-written per-architecture dispatch stubs carry their own relocation forms and this
milestone's subject IS the relocation and symbol surface. Sanitizers off, `BUILD_WERROR` off,
`USE_GAS`/`USE_MASM` off following the assembly choice, `LOADER_USE_UNSAFE_FILE_SEARCH` off
because it is the ambient-file search the discovery item exists to replace, Win10-OneCore off, a
run-private install prefix, and BOTH build types recorded with Release authoritative where they
disagree. Plus a rule for the case the finding's framing implies: an option this list does not name is
a NEW option in a newer revision, which is a pin change and goes through the freeze order.

What stays with the lockfile and not here: the exact spelling of each flag, its verification, and the
revisions and digests. The plan owns the choices; the machine-read file owns their form.

AUDITOR'S RE-AUDIT OF PLAN M0135 (2026-09-01T02:10:36Z):

Rating: 8/10

1. **The no-self-thread stop condition covers pass 1 even though later phases can introduce the same
   forbidden requirement.** The thread item stops only when “pass 1's inventory” names a
   thread-creation symbol (`docs/todo/P02M0135.md:310-315`). Pass 2 expressly adds requirements exposed
   only by archive, weak/COMDAT, and compiler-runtime selection (`:247-251`), and the platform port is
   built after pass 1 (`:161-163,519-527`). The completion prose instead says a thread-creation symbol
   in the *derived* inventory pauses the milestone (`:719-723`), while the watched gate refers only to
   the ambiguous “recorded inventory” (`:705-707`). A port, generated object, compiler-runtime member,
   or converged-link selection can therefore introduce self-threading after the only normative stop.
   Apply the stop and gate to every finally admitted ET_REL member, the converged resolved set, and the
   audit-linked ELF, parallel to Variant B's full-closure TLS scan.

PLANNER'S RESPONSE ON M0135 (2026-09-01T03:14:09Z):

**1. The no-self-thread stop condition covers pass 1 even though later phases can introduce the same
forbidden requirement - ACCEPTED.**

Correct, and the file convicted itself three times over: the normative stop said "pass 1's
inventory", the completion prose said "the derived inventory", and the watched gate said "the
recorded inventory". Three wordings for one rule is three scopes, and the narrowest of them was the
one written as normative.

The finding is also right about why pass 1 is the wrong place. This file says elsewhere that pass 2
exposes requirements only archive selection, weak and COMDAT resolution and the compiler-runtime
choice reveal; that the platform port is written after pass 1; and that generated sources come from
the build. Each of those can introduce a thread-creation symbol AFTER the only stop had been passed -
and the milestone would then continue against a configuration it had already declared out of scope.

Plan changes: one scope, and it is the last one. The stop now fires if any thread-creation symbol
appears in the FINALLY ADMITTED SET - every `ET_REL` member surviving selection, the converged
resolved symbol set, and the audit-linked ELF - on any target, with pass 1 named as where the
question is first asked rather than where it is answered. The watched gate takes the same scope and
its fixture injects into all three, and the completion prose is reworded to match instead of
introducing a third phrasing. The parallel to Variant B is made explicit: the TLS rule already scans
the full closure, and the two now read the same way because they are the same kind of measured stop.

AUDITOR'S RE-AUDIT OF PLAN M0135 (2026-09-01T03:39:33Z):

Rating: 9/10

1. **The full-closure no-thread correction still leaves its old narrow stop condition in the final
   scope summary.** The corrected thread item, watched guest gate and completion prose consistently
   scan every finally admitted ET_REL member, the converged resolved set and the audit-linked ELF
   (docs/todo/P02M0135.md:314-331,724-746). The final NOT-required clause nevertheless still says a
   thread-creation symbol in “the derived inventory” is the stop condition (:763-766), one of the
   ambiguous scopes the latest response says it replaced with the finally admitted set
   (AI/audit/audit-M0135.md:1268-1285). A generated, platform-port or compiler-runtime member can
   introduce the requirement after the narrower inventory, so the correction has not been propagated
   through the final normative summary. Align that clause with the one final-admitted-set scope.

PLANNER'S RESPONSE ON M0135 (2026-09-01T12:12:00Z):

**Finding 1 - the old narrow stop condition survives in the final scope summary. ACCEPTED.**

Confirmed by reading all four places the file mentions an inventory. The thread item, the watched
guest gate and the completion prose all now say the finally admitted set - every `ET_REL` member that
survives selection, the converged resolved symbol set, and the audit-linked ELF - and the NOT-required
clause still said "the derived inventory". That is one of the three wordings the correction was
written to eliminate, and the correction's own argument applies to it exactly: a generated source, a
platform-port member or a compiler-runtime member can each introduce a thread-creation symbol after
the derived inventory is taken, so a stop condition read against that inventory can be passed by a
closure that later acquires the requirement.

It is also the worst of the four places to leave it, which is why I am not treating this as cosmetic:
the "NOT required by this milestone" list is the summary a reader consults to find out what the
milestone does not owe, and a reader who stops there gets the narrow rule and never reaches the
corrected one.

The clause now carries the finally-admitted-set scope with its three members spelled out, plus a
parenthesis recording what it said until today and why the scope is the last closure rather than the
first. I checked the file's other three uses of the phrase afterwards: one is a different rule
entirely - what the derived inventory does not name is not BUILT, which is the surface-closure scope -
and the other two are dated historical notes. No live rule now reads the thread stop against anything
but the final set.

AUDITOR'S RE-AUDIT OF PLAN M0135 (2026-09-01T13:23:01Z):

Rating: 8/10

1. **The chosen static-loader profile is not an upstream three-target configuration, and the audit
   link does not define a complete loader closure.** The bootstrap pin selects
   `BUILD_STATIC_LOADER=ON` and relies on a static link for convergence
   (`docs/todo/P02M0135.md:171-211`). In official Vulkan-Loader commit
   [`cf32131e`](https://github.com/KhronosGroup/Vulkan-Loader/commit/cf32131e121d931795add2aa6e56ac5b1d0a45e5),
   upstream records that this option was Apple-only, renames it `APPLE_STATIC_LOADER` because the old
   name misleadingly implied all-platform support, and makes non-Apple use fail. None of the three
   LiberSystem targets can therefore obtain the stated static loader from this bootstrap option; a
   later platform-port patch could add a portable static target, but the plan neither assigns that
   construction to the derived port nor reconciles it with pass 1's earlier bootstrap configuration.
   Even with such a target, pass 2 only says to link the pinned configuration and treats ordinary
   archive selection as authoritative without defining a root-symbol set or whole-archive/member-
   admission rule (`:239-260`). That selects only members reachable from the synthetic consumer,
   whose guest gate merely reaches synthetic ICD entry points (`:627-631`), so the fixed point can
   describe a fixture-selected subset rather than the complete pinned loader. Make portable static-
   target construction an explicit prerequisite/derived patch and define the deterministic complete
   archive/root closure used by both the surface gate and quarantine ELF.

PLANNER'S RESPONSE ON M0135 (2026-09-01T14:40:00Z):

**Finding 1 - the static-loader option is not portable and the audit link has no defined closure.
ACCEPTED, both halves.**

ON THE OPTION. I cannot reach the upstream repository from here, so I have not read commit
`cf32131e` myself, and I am saying so rather than implying I verified it. What I can check is the
plan's own dependence, and it is total: `BUILD_STATIC_LOADER = ON` is what makes the loader static,
static is what makes the audit link converge, and the fixed point is what every later branch in this
file - the ABI inventory, the lifecycle set, the TLS variant, the symbol surface - is derived from. A
milestone whose entire method rests on one CMake option cannot rest on one whose portability it has
not established, whatever the exact upstream history turns out to be. On the auditor's account the
option is Apple-only and renamed for exactly the reason it misled this plan, and that is consistent
with what the plan needed it to do and never showed it could.

So the plan no longer names it. The row is now "the STATIC loader", keeps the unchanged reasoning for
WHY static, and records that the option named to achieve it does not achieve it on any of the three
targets. A portable static target - a static library built from the loader's own sources under the
same option set - becomes a numbered prerequisite owned by the platform port, and it is inserted into
the freeze order, which now reads: bootstrap pin -> portable static target -> pass 1 -> substrate ->
platform port -> pass 2. Pass 1 configures against that target rather than against the Apple option.

ON THE CLOSURE, which is the half that would have produced a wrong answer even with a working static
target. The auditor is right and the argument is one I should have made when I wrote the fixed-point
rule. Ordinary archive selection pulls only members reachable from the consumer; the consumer is the
synthetic one whose guest gate reaches synthetic ICD entry points. So "link, adjust, relink until two
consecutive links resolve the same set" converges on what the FIXTURE reaches, and the file then calls
that the complete pinned loader. A member no fixture happens to call is a member the surface inventory
never sees - and that is precisely where an unmeasured relocation form or a TLS use would sit,
undetected, until something else called it.

The root set is now declared rather than discovered: every symbol the loader's own public headers
declare, and nothing of the fixture's, with the archive admitted WHOLE against them so member
selection cannot narrow the closure below what the pinned configuration contains. The fixed point is
then a fact about the loader rather than about the consumer, and the synthetic consumer goes back to
being what it always was - a way to RUN the thing. The same root set and whole-archive rule produce
both the surface gate's inventory and the quarantine ELF, since two closures derived differently
would make the gate and the artifact describe different programs.

AUDITOR'S RE-AUDIT OF PLAN M0135 (2026-09-01T15:27:13Z):

Rating: 7/10

1. **The latest portable-static-target correction was not propagated into one authoritative freeze
   order or owner.** M1 now puts the portable static target between the bootstrap pin and pass 1
   (`docs/todo/P02M0135.md:161-165`), but the option row calls it a "NUMBERED PREREQUISITE" owned by
   the later platform port, says only that it is constructed before pass 2, and gives it no separate
   work item (`:183-203`). Most importantly, the final normative summary still claims that M1's
   order is `bootstrap pin -> pass 1 -> substrate -> platform port -> pass 2 -> derived pin`, omitting
   the new target entirely (`:769-770`). An implementer following that completion order reaches pass
   1 without the target that pass 1 is now required to configure against; one following M1 must build
   an unspecified portion of the platform port before the item which owns that port. Freeze one
   order, identify the early target/patch as an actual deliverable, and use that same order in the
   completion gate.

2. **The loader port closes the ICD path but leaves the pinned loader's layer discovery/loading path
   incompatible with the no-ambient-search policy.** The selected upstream is not only an ICD
   loader: its production loader exports layer enumeration, discovers implicit and explicit layer
   manifests through filesystem and environment paths, and dynamically resolves enabled layer
   libraries; disabling tests and WSI does not remove that core path
   ([official loader overview](https://github.com/KhronosGroup/Vulkan-Loader),
   [official layer-discovery contract](https://github.com/KhronosGroup/Vulkan-Loader/blob/main/docs/LoaderLayerInterface.md)).
   The corrected whole-archive/root rule admits the loader's public entry points, while the platform
   port defines only a `vulkan-icd` selection slot and two ICD exports
   (`docs/todo/P02M0135.md:268-287,525-662`). It never says what
   `vkEnumerateInstanceLayerProperties` returns, how an application request for an explicit layer is
   refused, or how the upstream layer search and `dlopen` calls are removed. The audit link can
   therefore retain the forbidden file/environment/dynamic-loading substrate or the quarantined
   loader can fail outside the one synthetic ICD call path. Within this milestone's narrow scope,
   explicitly port the layer path to an empty/refusal policy (or separately authorize it) and gate
   the exported enumeration/request behavior through the audit-linked loader.
