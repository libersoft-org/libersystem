AUDITOR'S REVIEW OF PLAN M0176 (2026-08-30T16:21:53Z):

Rating: 5/10

The audit-only boundary is appropriate, and the plan correctly avoids treating `unsafe` counts or adjacent comments as proof. Its completeness claim is not yet reproducible: the configuration matrix does not match actual shipped profiles, it omits the shipped WebAssembly component, the inventory mechanism is unspecified, and confirmed driver defects can be routed to an unbounded umbrella.

## Material findings

1. **The named configuration matrix does not identify the artifact configurations the project actually ships.**

   **What is wrong:** M1 names a “normal release kernel,” static/shared userspace, and a development closure (`docs/todo/P02M0176.md:20-26`) without fixing artifact roots, Cargo profiles, features, target flags, or manifest membership. The shipping kernel is built without `--release` and staged from `.../debug/kernel`; static userspace is likewise built in Cargo's dev profile (`build.sh:92-113`, `:145-153`). Shared images use release mode, `--no-default-features --features shared-image`, custom target/build-std flags, and a separate linking pipeline (`src/tools/build-shared.sh:1799-1815`, `:2056-2105`; `src/tools/build-consumer-object.sh:20-25`). `development` is enabled selectively for services and drivers. These choices materially change reachable unsafe sites: the shipped kernel includes `#[cfg(debug_assertions)]` code (`src/kernel/mem/frame/mod.rs:123-240`), while `shared-image` selects different runtime linkage, allocation, raw-memory, and assembly boundaries (`src/user/runtime/rt/src/lib.rs:37-197`).

   **Why it matters:** Interpreting “release kernel” as Cargo release omits code in the real shipping kernel. Treating static/shared as labels can include Cargo outputs that are not shipped or miss feature-specific code that is. A revision pin cannot repair an incorrectly enumerated closure.

   **Correction:** Derive and freeze a concrete artifact/configuration matrix from `build.sh`, `build-shared.sh`, Cargo metadata, and the canonical system manifest. For each row record shipped artifact roots, target triple/spec, Cargo profile, exact default/explicit features, build-std/Rust flags, relevant environment, and pinned compiler. Name the current kernel accurately as the shipping non-test dev-profile configuration and keep shipping, development-only, and test-only closures separate.

2. **The shipped WebAssembly component is missing from the production-userspace scope.**

   **What is wrong:** M1 promises every shipped static/shared userspace artifact shape and M3 names runtime, drivers, services, libraries, and applications (`docs/todo/P02M0176.md:23-26`, `:42-46`), but neither accounts for the third shipped executable shape: the SDK-built `wasm32-unknown-unknown` component. `build.sh:51-80` distinguishes its shipping default-feature build from `dev-diagnostics`, and the canonical manifest stages it at `components/liber_component/app.wasm` (`src/user/services/manifest.toml:1599-1606`). Its in-tree closure contains unsafe host imports and linkage attributes (`src/sdk/src/world.rs:8-18`; `src/sdk/examples/liber_component/src/lib.rs:39-41`, `:125-126`, `:156-158`).

   **Why it matters:** The audit can satisfy its literal static/shared matrix while missing a shipped unsafe boundary between component code and the project's interpreter, then incorrectly claim complete production-userspace coverage.

   **Correction:** Add the shipping WASM component and in-tree SDK closure to M1/M3 with its exact release/default-feature configuration; classify the separate `dev-diagnostics` build as development/test evidence. Alternatively, explicitly exclude components and narrow the goal and Definition of Done so they no longer claim all production userspace.

3. **“Syntax-aware inventory” is not a reproducible completeness mechanism as specified.**

   **What is wrong:** M1 does not choose an inventory method or explain how source syntax is reconciled with compiled reachability, macro expansion, generated/build-script output, and per-artifact `cfg`. M4 gives the companion inventory no fixed path, format, schema, stable identifier, or regeneration command (`docs/todo/P02M0176.md:20-30`, `:48-52`). A source parser counts disabled branches and macro definitions but cannot determine artifact reachability; compiler-expanded output can lose stable source/generator provenance. This tree uses both generated `include!(concat!(env!("OUT_DIR"), ...))` code (`src/user/services/core/src/service_manager.rs:125-129`; `src/user/services/core/src/device_manager.rs:4555`) and architecture-selected macros that emit `global_asm!` (`src/user/libs/clients/network-client-provider/src/lib.rs:5-34`).

   **Why it matters:** Different reasonable implementations can produce incompatible inventories while each claims to be syntax-aware. Missing a generated, macro-produced, or configuration-only site is not mechanically detectable, so “every discovered site/group” is circular rather than a completeness test.

   **Correction:** Define a machine-readable inventory path/schema with stable site/group IDs, source or generator provenance, artifact/config reachability, boundary category, owner, and disposition. Specify a toolchain-pinned regeneration pipeline that starts from the canonical artifact matrix, captures build-script outputs and macro/cfg-expanded code, reconciles them with source-level sites, and records commands/tool versions/digests. Add completeness fixtures for cfg-only, macro-produced, generated, in-tree dependency, and test-only sites.

4. **Routing driver findings to P02M0099 contradicts the requirement for bounded corrective owners.**

   **What is wrong:** M3 directs driver findings to their family/item in P02M0099 (`docs/todo/P02M0176.md:42-46`), while M4 and the Definition of Done require narrowly scoped corrective milestones and bounded owners for material defects (`:48-53`, `:72-73`). P02M0099 explicitly says its existing driver debt is not part of the umbrella's Definition of Done and is deferred until the affected family's next item (`docs/todo/P02M0099.md:823-829`).

   **Why it matters:** A confirmed soundness defect in a currently shipped driver can be attached to unrelated future family work indefinitely while M0176 still reports every material defect as owned. That is not a bounded disposition.

   **Correction:** Use P02M0099 attachment only for safe-encapsulation opportunities or necessary boundaries that genuinely belong to planned family work. Give every confirmed current-product unsoundness or material defect its own bounded corrective item, severity, owner, and disposition identifier, while keeping M0176 itself audit-only.


PLANNER'S RESPONSE ON M0176 (2026-08-30T19:36:07Z):

All four findings are accepted. Every configuration claim was checked against the build scripts and
the canonical manifest rather than against the label.

**1. The named configuration matrix does not identify the artifact configurations the project ships
- ACCEPTED.**

Confirmed, and the "release kernel" reading would have been wrong in the direction that OMITS
shipped code. `build.sh:step_kernel` runs `cargo build $flag` with NO `--release`, and
`step_volume` stages from `.build/cargo/kernel/<triple>/debug/kernel`. So the shipping kernel is
Cargo's dev profile, and `#[cfg(debug_assertions)]` code IS in it - `kernel/mem/frame/mod.rs`
alone has five such sites in the allocator. `step_user` is likewise `cargo build $flag
$(dev_features)` with no `--release`. Shared images are a materially different configuration:
`build-shared.sh` uses `--release`, `--no-default-features --features shared-image`, a custom
target with `-Z build-std=core,alloc,compiler_builtins`, its own RUSTFLAGS and a separate linking
pipeline, which selects different runtime linkage, allocation, raw-memory and assembly boundaries in
the same source.

Plan change: a new section, "The configuration matrix is derived, not named", records each of those
facts with its source, and M1 now DERIVES the matrix from the build scripts, Cargo metadata and the
canonical manifest rather than naming it - per row: shipped artifact roots, target triple or spec,
Cargo profile, exact default and explicit features, build-std and Rust flags, relevant environment
and the pinned compiler. The kernel is named accurately as the shipping non-test dev-profile
configuration, and shipping, development-only and test-only closures stay separate. "What this
milestone refuses" gained a line saying the matrix records what ships and does not propose changing a
build profile to make itself tidier.

**2. The shipped WebAssembly component is missing from production userspace - ACCEPTED.**

Confirmed as a real third shipped executable shape, not a variant of the other two.
`build.sh:step_sdk` builds `--release --target wasm32-unknown-unknown --workspace` with default
features and calls that "THE SHIPPING BUILD, and the one the image stages"; the canonical manifest
stages it at `components/liber_component/app.wasm` (`manifest.toml:1606`). The separate
`dev-diagnostics` build goes to its own target directory and is a different row. Its closure carries
unsafe host imports and linkage attributes, so the audit could have satisfied its literal static and
shared matrix while missing a shipped unsafe boundary and still claiming complete production
coverage.

Plan change: M1's coverage list now includes the shipping WASM component and its in-tree SDK closure
as their own matrix row, with `dev-diagnostics` classified as development evidence. M3's cohort
list gained the SDK/component cohort and requires the boundary to be audited on BOTH sides - the
guest's host imports and linkage attributes, and the interpreter's side of the same contract - since
one side alone does not establish the invariant. The Definition of done names the component
explicitly.

**3. "Syntax-aware inventory" is not a reproducible completeness mechanism - ACCEPTED.**

Confirmed, including the two mechanisms that make it hard here: services and DeviceManager pull in
generated code through `include!(concat!(env!("OUT_DIR"), ...))` - ten such sites in the services
crate alone - and architecture-selected macros emit `global_asm!`. A source parser counts disabled
branches and macro definitions and cannot decide artifact reachability; expanded output loses source
provenance. And M4 gave the companion inventory no path, format, schema, stable identifier or
regeneration command, which is what made "every discovered site is represented" circular rather than
a completeness test.

Plan change: M1 now specifies the MECHANISM - a toolchain-pinned pipeline starting from the artifact
matrix, capturing build-script output and macro- and cfg-expanded code, reconciling it back to
source-level sites, and recording the commands, tool versions and digests it used - with the two
in-tree mechanisms named as the reason that reconciliation is required. M4 requires the inventory to
be a machine-readable file at a fixed path with a fixed schema and regeneration command: stable site
and group IDs, source or generator provenance, artifact and configuration reachability, boundary
category, owner and disposition. It adds completeness fixtures for a cfg-only, macro-produced,
generated, in-tree dependency and test-only site, so a missing CLASS of site is mechanically
detectable rather than a matter of care.

**4. Routing driver findings to P02M0099 contradicts the requirement for bounded owners - ACCEPTED.**

Confirmed against P02M0099's own text: "THIS DEBT BELONGS TO THE FAMILIES IT TOUCHES, NOT TO THE
MILESTONE AS A WHOLE. It is not in the ordering above and it is not in the Definition of done", and
each finding "is closed when that family's next item is". That deferral is correct for what it was
written about - sixteen driver findings should not gate forty new driver items - but it means a
CONFIRMED soundness defect in currently shipped code, routed there, is deferred indefinitely while
this audit still reports it as owned. That is a disposition wearing the word "owned", and it
contradicts M4's own requirement for narrowly scoped corrective milestones.

Plan change: M4 now routes by SEVERITY rather than by subject area. A safe-encapsulation opportunity
or a boundary that genuinely belongs to planned family work may be attached to its P02M0099 family
item; a confirmed unsoundness or material defect in shipped code may not, and the reason - quoted
from P02M0099 - is written into the plan so the next reader does not have to go and find it. Every
such finding gets its own narrowly scoped corrective item with a severity, an owner and a disposition
identifier. The audit-only boundary is preserved: this milestone still edits no production code.
"Dependencies and ordering" now says P02M0099 receives only the dispositions M4 permits it to
receive, and the Definition of done states that nothing material is parked on an umbrella that defers
it indefinitely.

**Plan re-check.** The item count is unchanged at four - all four corrections belong inside existing
items - and the audit-only scope is untouched. The plan is longer in one place only: the derived
matrix section, which exists because the enumeration is the completeness claim and getting it from a
label rather than from the build scripts is how this audit would have been wrong without anyone being
able to tell. Ordering is unchanged: M1 (matrix and inventory mechanism) -> M2 and M3 (the two
audits, independent of each other) -> M4 (publication and routing). The Definition of done was
rewritten so each clause names something falsifiable: the derived matrix including the three shipped
shapes, the completeness fixtures, and the severity-based routing. No source code was modified.
