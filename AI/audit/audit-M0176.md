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

