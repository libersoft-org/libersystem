AUDITOR'S REVIEW OF PLAN M0170 (2026-08-30T16:21:53Z):

Rating: 4/10

The intended revision-bound release dossier is valuable, but the plan does not yet define a self-contained release run or an evidence protocol capable of supporting its own claims. It also treats the selection-specific executable race as current even though the current harness already stages and directly boots the exact Cargo artifact.

## Material findings

1. **M1 is stale and proposes a broader lock boundary than the current fix requires.**

   **What is wrong:** The plan says the remaining race is an unstaged selection-specific test kernel and requires compiling, discovering, staging, and building the medium under one build lock (`docs/todo/P02M0170.md:11-16`, `:20-26`). The current harness already obtains Cargo's exact executable from JSON, copies it to a run-private path while holding the same-architecture lock, and invokes `qemu-run.sh` directly on that copy without another Cargo resolution (`src/harness/test-kernel.sh:303-365`, `:367-392`). Media construction is content-addressed from its complete inputs, so holding the global Cargo lock during that later work does not protect the now-private ELF.

   **Why it matters:** Implementing M1 literally duplicates an existing mechanism and unnecessarily serializes slow media construction. It also distracts M2 from the actual missing work: a regression and evidence binding for the existing staging contract.

   **Correction:** Rebase M1 on the current implementation. Keep the lock through atomic publication of the private ELF, make the copy read-only or otherwise immutable, record and verify its digest, then assemble content-addressed media after releasing the lock. M2 should mutate/remove that staging or digest binding and prove overlapping selections execute their own stable IDs; it should not reimplement the already-present path.

2. **A clean detached release run has no step that builds the shipping images its mandatory gates consume.**

   **What is wrong:** The current full/release path runs `build.sh --arch all`, the guest suites, then `check.sh` (`verify.sh:216-240`, `:268-280`). `build.sh` builds parts but never invokes `image.sh` (`build.sh:163`, `:265-280`), while Secure Boot and virtio-IOMMU gates consume `.build/boot/libersystem.iso` and expect an image receipt (`src/tools/check-secure-boot.sh:90-106`; `src/tools/check-qemu-virtio-iommu-x86_64.sh:54-89`). M4 requires a clean detached worktree but names no image prerequisites or artifact flow (`docs/todo/P02M0170.md:42-52`).

   **Why it matters:** A genuinely fresh snapshot fails for a missing ISO, while a reused workspace can falsely pass using media built elsewhere. Either result defeats the claim that one immutable revision and artifact set was tested.

   **Correction:** Put all required shipping-image constructions in the canonical catalog, express their prerequisite order, and pass the resulting run-private immutable paths to dependent gates. Bind every gate result to the exact image digest recorded in the dossier; do not let it discover a shared `.build/boot` artifact by convention.

3. **The canonical catalog cannot currently express or independently protect the required release set.**

   **What is wrong:** A catalog `Check` has only `id`, `kind`, `covers`, `variants`, and `command` (`src/tools/verify-model/src/catalog.rs:76-98`). It has no release-required bit/class, profile identity, prerequisites, artifact inputs, or evidence contract. It also deliberately omits umbrella gates and contains fallback rows that are not ordinary required work (`:379-392`, `:439-457`). M4 nevertheless derives every release obligation solely from this mutable catalog, and M0173 expects to add five required profile rows to it (`docs/todo/P02M0170.md:45-52`; `docs/todo/P02M0173.md:98-100`, `:129-130`).

   **Why it matters:** Deleting or reclassifying a catalog row can shrink both the work and the expected evidence, allowing the same mutation to self-certify as complete. There is also no executable way to distinguish a required concrete profile from an optional umbrella or fallback.

   **Correction:** Define a stable fully qualified plan-item/variant key, closed release class/profile identity, prerequisites, producers, artifact inputs, and evidence schema. Validate the derived catalog against independent arch/class/cardinality invariants, and add a mutation that deletes a mandatory row/class and must fail before execution.

4. **No producer/collector protocol can create the promised per-ID dossier.**

   **What is wrong:** `check.sh` records umbrella output/status and stops at failures, multi-boot gates keep important phase logs in trap-deleted temporary directories, `verify.sh` records only step status/duration, and `test-kernel.sh` prints result-log paths without a common envelope containing the source, staged ELF, medium, or plan-item digest (`check.sh:246-275`, `:381-383`; `src/tools/check-qemu-arch-profiles.sh:46-65`, `:130`; `src/tools/check-qemu-virtio-iommu-x86_64.sh:91-92`; `verify.sh:757-793`; `src/harness/test-kernel.sh:486-502`). M2/M4/M5 specify report contents but not how producers publish them.

   **Why it matters:** The aggregator cannot distinguish evidence from a different sub-boot, artifact, profile, concurrent run, or stale workspace. An early failure can also erase the very evidence needed to show which required rows did and did not run.

   **Correction:** Define a run-private, atomically published machine-readable evidence envelope containing run ID, exact plan-item/variant key, revision/config/tool identities, every input artifact path and digest, outcome, duration, and result paths/digests. Update each gate/guest producer to publish before cleanup; preserve failures and reject duplicate, unknown, missing, cross-run, or stale envelopes.

5. **The required development build-and-boot row has no runnable lifecycle.**

   **What is wrong:** Catalog development commands directly invoke `dev-selftest.py`, `proto-test.py`, and `perf-gate.py` (`src/tools/verify-model/src/catalog.rs:459-482`), but those checks require a running `./dev.sh up` guest (`src/tools/verify-model/src/commands.rs:234-237`). The full/release path never starts one, and `src/tools/check-development-build.sh:11-14` proves compilation only. M4 merely lists “development build and boot” (`docs/todo/P02M0170.md:47-50`).

   **Why it matters:** A release run either depends on an unrelated pre-existing guest or cannot execute the required rows. Such a guest is not revision-, configuration-, or artifact-bound.

   **Correction:** Add explicit development setup/build, immutable image creation, boot, readiness, test, and teardown dependencies—or one self-contained gate—with run-private sockets, disks, variable state, logs, and guaranteed teardown. Record the exact boot artifact digest in every development result.

6. **Source, tool, firmware, and configuration identity are labels rather than reproducible contracts.**

   **What is wrong:** M4 requests a “complete source digest,” tool versions, firmware identities, build configuration, and trust profile but defines none (`docs/todo/P02M0170.md:42-46`). The existing source helpers are not equivalent: the model verifier largely hashes HEAD plus changed paths/bytes, while the shell helper hashes selected directories/extensions (`src/tools/verify-model/src/shadow.rs:964-1000`; `lib.sh:239-245`). QEMU and firmware are host-selected or environment-overrideable (`src/harness/qemu-run.sh:962-970`, `:1246-1247`, `:1479`), and `toolchain.lock` covers only part of the executable toolchain.

   **Why it matters:** Two materially different source trees, firmware images, QEMU binaries, features, or environment overrides can receive the same dossier identity. Version strings alone also do not identify the executed binary.

   **Correction:** Define one NUL-safe source identity over tracked path, mode, bytes, submodules, lockfiles, and declared generated inputs—or explicitly use and label a Git tree identity for a clean snapshot. Define a closed influencing-input manifest with resolved paths, hashes/versions, Rust/Cargo lock identity, firmware hashes, feature/profile flags, and permitted environment variables; reject undeclared overrides and verify source identity before and after the run.

7. **The dossier has no durable publication owner outside the disposable snapshot.**

   **What is wrong:** Existing detached sweeps delete their worktree via an exit trap (`verify.sh:293-311`). M4/M5 do not specify an owner-worktree/output location, atomic finalization, collision-free run identity, or failed-run retention for the dossier and evidence (`docs/todo/P02M0170.md:42-58`).

   **Why it matters:** A successful or failed release's evidence can disappear with the snapshot, or concurrent runs can overwrite a shared output. A report that cannot be retained independently is not a release dossier.

   **Correction:** Designate a durable output root outside the disposable worktree, publish into a run-ID directory atomically, keep incomplete/failure evidence with an explicit state, and make finalization immutable. Record both the audited snapshot identity and the publisher version.
