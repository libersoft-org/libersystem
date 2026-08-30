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

PLANNER'S RESPONSE ON M0170 (2026-08-30T19:36:07Z):

All seven findings are accepted. One is accepted in part: its staging half is already implemented and
its locking half is rejected as over-serialization that protects nothing.

**1. M1 is stale and proposes a broader lock boundary than the fix requires - ACCEPTED IN PART.**

The audit is right about the staging half. `src/harness/test-kernel.sh:317-360` already takes the
same-architecture build lock, compiles with `--message-format=json`, reads the exact executable
Cargo named for the `kernel` target, copies it to `kernel-test-$ARCH.$$.elf` while the lock is
still held, and then invokes `qemu-run.sh` directly on that copy rather than letting `cargo test`
re-resolve the shared target directory. Re-specifying that is duplication.

REJECTED: the requirement to build the medium under the same lock. The ELF is already run-private
once it is copied, and the medium is content-addressed from its complete inputs - so nothing between
the lock and the boot can change what the medium is keyed on. Holding the global Cargo lock across
media construction would serialize the slowest part of the run for no property.

ACCEPTED and kept: what the current code does NOT do. The copy is an ordinary writable file, its
digest is never computed, and nothing downstream is bound to it. Plan changes: a new "What is there
now" section records the implemented staging so no future reader re-specifies it; M1 is reduced to
the three things still missing - atomic publication, immutability, and a digest computed under the
lock and named by every downstream step - and says explicitly not to extend the lock over media
construction. M2 keeps the concurrency proof and its mutations are now aimed at the real bindings:
remove staging, make the copy writable and replace it mid-run, drop the digest binding.

**2. A clean release run has no step that builds the images its gates consume - ACCEPTED.**

Confirmed. `verify.sh:run_full` is `./build.sh --arch all`, three `./test.sh` runs and
`./check.sh`; nothing on that path runs `image.sh`. `check-secure-boot.sh:107` fails without
`$BUILD/libersystem.iso`, and `check-qemu-virtio-iommu-x86_64.sh:71-88` requires the ISO AND a
`.build-key` receipt that matches a key recomputed from this tree. So a genuinely fresh snapshot
cannot run its own mandatory gates. The asymmetry the audit points out is real and worse than it
sounds: the virtio-IOMMU gate catches a stale image by content, while the Secure Boot gate only
checks that the file exists, so in a reused workspace it can pass against media built from another
tree.

Plan change: a new **M5** puts every shipping-image construction into the catalog as producer rows
with their own keys, expresses the prerequisite edges from the gates that read them, and requires
the run to pass run-private immutable paths and digests to those gates. A gate whose named input was
not produced by this run is a failure, not a skip. M9 binds the digests into the dossier.

**3. The catalog cannot express or independently protect the release set - ACCEPTED.**

Confirmed. `catalog.rs:84-98`: a `Check` is `id`, `kind`, `covers`, `variants`,
`command`, and nothing else. `UMBRELLA_GATES` are deliberately kept OUT of the catalog (:392) so
nothing can select them, and the two `GuestFallback` rows carry no `covers` and are skipped by
`select` (:456-457) because they stand in for tests rather than joining them. So deriving "every
release obligation" from this catalog is not expressible today, and there is no data by which a
required concrete profile differs from an umbrella or a fallback.

Plan change: a new **M4** adds the fields and, more importantly, the independent invariants - a
stable fully qualified key, a closed class, a release-required bit set from a declared class rather
than per row, prerequisites, producers, artifact inputs and an evidence contract; validated against
per-architecture and per-class cardinality checks that do not come from the catalog itself, so the
same mutation cannot both shrink the work and shrink the expectation. A deleted or reclassified
mandatory row must fail before execution.

**4. No producer/collector protocol can create the promised dossier - ACCEPTED.**

Confirmed, and the failure-erases-evidence half is the sharpest part. `check-qemu-arch-profiles.sh`
does `work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT` and writes each profile's log inside it, so
a gate that fails deletes the record of which phase failed. `verify.sh:run_one_step` writes status
and duration only, and `record_one_step` records per-key outcome and seconds through the model -
no artifact digests, no result paths, no run identity.

Plan change: a new **M7** defines the envelope (run ID, exact key, identity block, every input
artifact path and digest, outcome, duration, result paths and digests), requires each producer to
publish it BEFORE its cleanup runs, and requires the collector to reject duplicate, unknown,
missing, cross-run and stale envelopes. It also distinguishes two failures that must not read the
same: a required key with no envelope, and an envelope recording a failure.

**5. The required development row has no runnable lifecycle - ACCEPTED.**

Confirmed. The three development checks are catalogued with commands that invoke `dev-selftest.py`,
`proto-test.py` and `perf-gate.py` (`catalog.rs:481`), `commands.rs:237` gives them
`requires: Vec::new()` and a note saying they need `./dev.sh up`, and nothing on the release path
starts one. `check-development-build.sh` says in its own header that it does not boot the guest.
So the row either depends on a guest someone left running - which is bound to no revision,
configuration or artifact - or cannot execute.

Plan change: a new **M6** requires explicit setup, build, immutable-image, boot, readiness, test and
teardown steps (or one self-contained gate) with run-private sockets, disks, state and logs and
guaranteed teardown, declares them as the three checks' prerequisites, and records the boot artifact
digest in every development result. It keeps `check-development-build.sh` as the compile-only gate
it is and says it is not evidence that the configuration boots.

**6. Source, tool, firmware and configuration identity are labels - ACCEPTED.**

Confirmed that the two existing helpers answer different questions. `lib.sh:239-245` hashes a
`find` over selected directories filtered to `.rs`, `.toml` and `.lsidl`.
`shadow.rs:967-1000` hashes HEAD plus the working tree's changed paths, rename origins, change
kinds and bytes. Two materially different trees can agree under one and differ under the other.
Confirmed too that QEMU and OVMF are host-selected and environment-overridable
(`qemu-run.sh:962-963`, `OVMF_CODE`/`OVMF_VARS_SRC`), and that a version string does not
identify the executed binary.

Plan change: a new **M8** defines ONE source identity used in both places - NUL-safe over tracked
path, mode and bytes plus submodules, lockfiles and declared generated inputs, or explicitly the Git
tree identity for a clean snapshot, labelled as such - and a closed influencing-input manifest with
resolved tool paths and hashes, lock identity, firmware image hashes rather than package names,
target/feature/profile flags and the permitted environment variables. Undeclared overrides are
refused, and the source identity is verified before and after the run.

**7. The dossier has no durable publication owner - ACCEPTED.**

Confirmed. The detached sweep creates its worktree and installs
`trap "git worktree remove --force …" EXIT` (`verify.sh:299-300`); the failure path prints that
the worktree survives until the shell exits, and then the trap removes it. Nothing names an output
root, a run-ID directory, atomic finalization or failed-run retention.

Plan change: M9 requires a durable output root OUTSIDE the disposable worktree, a collision-free
run-ID directory, atomic and immutable finalization, retention of incomplete and failed evidence
with an explicit state, and a record of both the audited snapshot identity and the publisher version.

**Plan re-check.** The plan went from five items to ten, which is the honest count once the catalog,
the envelope, the image producers, the development lifecycle and the identity contract are separate
work rather than adjectives in M4. Ordering is implementable and stated: M4 (catalog model) precedes
M5 (producers) and M7 (envelope schema) and M9 (dossier); M8 supplies the identity block M7 embeds;
M1-M3 are independent of all of them. A "What this milestone refuses" line was added saying this is
not a general CI system - the envelope, the class model and the identity manifest exist to make one
release run's claim checkable, and nothing more. Dependencies now name P02M0173, whose five profile
rows M4's class model has to accept whichever milestone lands first. No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN M0170 (2026-08-30T22:25:50Z):

Rating: 6/10

The revised plan resolves most prior structural findings, but three evidence-integrity gaps remain.

1. **Content addressing does not provide the stable medium boundary that M1-M2 assume.** M1 categorically rejects holding the build lock across medium construction because the medium is content-addressed, while M2 requires two same-architecture suites to overlap and both succeed (`docs/todo/P02M0170.md:52-70`). The current builder explicitly runs producers outside its lock and aborts if an input changes during assembly (`src/harness/mkimage.sh:722-726`); the M0167 implementation history records this exact disjoint same-architecture overlap making one run fail (`AI/audit/audit-M0167.md:491-502`). Detection is not stabilization. The ISO is also renamed to an ordinary writable pathname that QEMU later opens (`src/harness/mkimage.sh:351-359`, `:446-455`; `src/harness/qemu-run.sh:925-952`, `:1018-1024`). Require serialization through a stable snapshot point or run-private immutable snapshots of every medium input, plus digest-verified handoff to QEMU, and exercise concurrent construction and pathname replacement.

2. **An envelope cannot preserve logs that cleanup deletes.** M7 records paths and digests and publishes before cleanup, and M9 calls the resulting dossier durable (`docs/todo/P02M0170.md:113-141`). Current multi-boot gates keep logs below trap-deleted temporary directories (`src/tools/check-qemu-arch-profiles.sh:46-65`, `:130`, `:212-218`; `src/tools/check-qemu-virtio-iommu-x86_64.sh:91-112`, `:147-158`). Persist the log bytes in the durable run-owned output before cleanup, make envelopes reference those objects, and test that failed-gate logs remain readable and digest-matching after producer exit.

3. **Pre/post identities do not prove that source bytes were immutable while used.** M8 relies on before/after identity checks, and M9 permits a clean detached worktree (`docs/todo/P02M0170.md:122-141`). A detached worktree remains writable; a file can be changed, consumed, and restored before the post-check. A Git tree identity does not change for ordinary worktree edits at all. M10's generic in-progress mutation fixture does not cover mutate-use-restore. Make the source snapshot genuinely non-writable or bind each producer to verified immutable input bytes, and add that regression.

PLANNER'S RESPONSE ON M0170 (2026-08-30T23:21:20Z):

All three findings are accepted. The first corrects a conclusion I drew last round from a true
premise.

**1. Content addressing does not provide the stable medium boundary M1-M2 assume - ACCEPTED.**

The auditor is right and the correction is mine to make: last round I rejected holding any lock
across medium construction on the grounds that the medium is content-addressed. That premise is true
and the conclusion does not follow. `mkimage.sh` locks its own assembly, but its own comment says
producers are NOT covered by that lock - "an input can be replaced while the image is being written"
- and what it implements is DETECTION: it recomputes the key afterwards and dies with "an input
changed while the image was being assembled". So a corrupt medium is impossible and a concurrent run
FAILS, which is precisely the outcome M2 forbids, since M2 requires two overlapping suites and both
must succeed. P02M0167's history records this exact overlap making one run fail.

The ISO pathname point is right too: the assembled image is renamed into an ordinary writable path
that QEMU opens later, so nothing binds the bytes QEMU boots to the bytes that were assembled.

Plan changes: M1's paragraph is rewritten as "CONTENT ADDRESSING IS DETECTION, NOT STABILISATION",
requiring either serialisation through one snapshot point before assembly or RUN-PRIVATE IMMUTABLE
SNAPSHOTS of every medium input - the technique M1 already applies to the ELF, extended to the rest -
plus a digest-verified handoff at the moment QEMU is invoked. Holding the CARGO lock across assembly
stays refused, with the reason narrowed: it serialises compilation for a property that belongs to the
medium's inputs. M2 now requires both overlapping runs to SUCCEED, which is the assertion that tells
a stable boundary from a detected collision, and its mutation list gains replacing a medium input
during concurrent assembly and replacing the assembled medium at its pathname before launch.

**2. An envelope cannot preserve logs that cleanup deletes - ACCEPTED.**

Verified: the multi-boot gates write phase logs under a `mktemp -d` removed by an EXIT trap, and the
trap fires on failure too - which is when the log matters. M7 said "publish before cleanup" and
recorded paths and digests, so the envelope would faithfully describe a file that no longer exists.

Plan changes: M7 now distinguishes publishing a path from preserving a log. The producer COPIES THE
LOG BYTES into the durable run-owned output before cleanup and the envelope references that stored
object. The regression is explicit and is the auditor's: after a gate fails and its process has
exited, every log its envelope names is still readable and still matches its recorded digest.

**3. Pre/post identities do not prove the source was immutable while used - ACCEPTED.**

Correct, and the second half is the part I would have missed: a detached worktree stays writable, so
mutate-use-restore passes a before/after comparison - and a Git TREE identity does not change for an
ordinary worktree edit at all, so the check can be blind to the edit it is looking for.

Plan changes: M8 now requires the snapshot to be made genuinely NON-WRITABLE for the run, or each
producer to be bound to verified immutable input bytes read through a path nothing else can replace;
the before/after check is demoted to a cheap second signal and explicitly stops being the argument.
M10 gains the MUTATE-USE-RESTORE regression, with the note that the generic in-progress mutation
fixture does not cover it.

**Plan re-check.** Item count unchanged at ten. The three corrections all move requirements from
detection to prevention, which is the same move in three places and is now stated that way. No source
code was modified.
