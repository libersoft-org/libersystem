AUDITOR'S REVIEW OF PLAN P02M0177 (2026-09-04T15:08:13Z):

Rating: 3/10

The plan's central implementation contracts are not ready. Its first regression has the wrong event order, two of the three motivating defects have no planned regression at all, the proposed architecture narrowing bypasses the selector's established safety model, and neither the tier lifecycle nor the gate/cache scheduling has an enforceable owner or completion criterion.

## Material findings

1. **M1 does not plan regressions for two of the three defects it says this milestone is about, and it names the wrong host seam.**

   **What is wrong:** The motivating defects are (a) `READY` versus a queued timeout, (b) probe-list order versus role-handoff order, and (c) name-sorted `MANIFEST` order versus dependency start order (`docs/todo/P02M0177.md:34-45`). M1 instead lists timeout/`READY`, `READY`/`FAILED`, and planned-stop/shutdown tests (`:54-63`); the latter two are different defects. The current provider correction lives in the bus-address sort and subsequent handoff in `src/user/services/core/src/device_manager.rs:1006-1041`, while the service correction is the declared dependency plus dependency-driven loop in `src/user/services/manifest.toml:2113-2152` and `src/user/services/core/src/service_manager.rs:647-764`. Neither path has the causal host regression M1 promises. Moreover, `BindingQueue` is already host-testable (`src/user/libs/driver/binding/src/lib.rs:562-630`); the decision that ignores or tears down on an event remains in the no-std `advance` path (`device_manager.rs:3782-4021`), so extracting or testing the queue alone cannot prove that production calls the guard.

   **Why it matters:** The Definition of Done can be met with tests for unrelated lifecycle bugs while the two actual order regressions remain unprotected. A unit test of FIFO behavior or of `accepts_terminal_frame` can also stay green if `DeviceManager` stops using that predicate, which is precisely the production omission that caused the timeout defect.

   **Correction:** Map one production-used, host-driven regression to each of the three named defects: an event reducer that includes the state transition/effects, a helper that produces the probe and handoff correspondence from the same provider identities, and a dependency-start scheduler exercised with name order opposed to dependency order. Pin the exact pre-fix revision or an explicit negative mutation for each so “fails against the code as it was that morning” is reproducible rather than a historical assertion.

2. **The timeout regression is specified in the wrong order, and M2's timing profile cannot deterministically reproduce the intended race.**

   **What is wrong:** M1 requires `TimedOut` followed by `READY` to preserve the binding (`docs/todo/P02M0177.md:57`). The canonical queue is explicitly FIFO (`src/user/libs/driver/binding/src/lib.rs:596-628`), and a real timeout consumed while the state is still `Binding` ends that attempt. The current control test correctly asserts that a `READY` arriving after the deadline cannot undo the timeout (`src/user/libs/driver/binding/src/tests.rs:654-671`). The defect being fixed is the opposite composition: `READY` has already moved the node to `Online`, then the previously generated timeout is consumed and must be ignored (`device_manager.rs:3874-3931`, `:3990-4021`). M2 then proposes shortening `READY_DEADLINE_TICKS` until events “collide” (`P02M0177.md:65-74`), but that value is a private compile-time constant (`device_manager.rs:104-112`, `:175-180`) with no current profile carrier. A wall-clock collision is scheduler/load dependent and cannot distinguish “reply already pending at expiry” from a genuinely late reply.

   **Why it matters:** Implementing the literal oracle would make a valid deadline ineffective by accepting a driver that answered only after teardown began. The QEMU test would be flaky or would merely test an ordinary timeout, so it would not reliably catch removal of the stale-timeout guard.

   **Correction:** Specify both controls: applying `READY` before a queued `TimedOut` on the same generation preserves the online binding, while applying `TimedOut` before a genuinely late `READY` fails the attempt. Drive the boundary with a fake clock/wait arbitration or a deterministically delayed test driver, not an empirically tuned collision. Define the supported build/configuration path that changes DeviceManager's deadline and keep that test variant isolated from normal/shipping artifacts.

3. **M3 uses tags as a coverage-narrowing primitive even though the established selector contract explicitly forbids that inference.**

   **What is wrong:** The merge tier would narrow emulated suites to prose categories expressed as tags (`docs/todo/P02M0177.md:81-86`), while P02M0167 states that `verify-model` selects on `covers`, never tags, and that tags are groupings for people and gates (`docs/todo/P02M0167.md:218-240`, `:764-766`). The current planner lowers selected `covers` intersections to exact stable test IDs (`src/tools/verify-model/src/plan.rs:535-571`; `src/tools/verify-model/src/commands.rs:294-329`). Its architecture-risk scan is deliberately widening-only because `usize`, layout, alignment, atomics, and dependency internals can differ without an architecture marker (`src/tools/verify-model/src/archrisk.rs:1-18`). P02M0177 simultaneously says that `covers`, ownership, and architecture rows remain unchanged (`P02M0177.md:127-128`), so no defined mechanism proves that tests outside the listed tags are target-neutral. This also bypasses P02M0167's frozen-candidate, shadow-evidence, and activation bars for a coverage reduction.

   **Why it matters:** A test that genuinely covers the changed component on aarch64 or riscv64 can be silently removed from merge evidence merely because its subject tag is not on a hand-written list. The resulting green is a new, unevidenced narrowing presented as “without verifying less.”

   **Correction:** Partition the exact `PlanItemKey`s already selected through `covers`, architecture variants, and architecture policy; an architecture-sensitive tag/ID set may be an additional floor, not a replacement for selected keys. Any reduction of the active selector's coverage must use the existing candidate/shadow/trust activation contract (or explicitly amend that contract with equivalent evidence), with unknown and empty derivations failing closed.

4. **The three tiers have no executable lifecycle, and the acceptance test can pass without implementing tiering.**

   **What is wrong:** M3 names inner, merge, and release sets but never defines their entry points, the default mode, who must invoke merge/release, how existing keys are partitioned with prerequisite closure, how deferred keys are represented, or what each exit status claims. `verify.sh` currently exposes no inner/merge mode (`verify.sh:45-85`), deliberately returns distinct nonzero statuses for incomplete, `SHADOW`, and `STALE` scoped evidence (`:1039-1106`), and explicitly says there is no CI/timer authority (`:1113-1123`). Preserving only “failure to produce a plan is never a pass” (`P02M0177.md:88-90`) does not preserve those other fail-closed edges. The DoD's three unrelated sample changes (`:113-114`) is also not a tier test: documentation already selects nothing, an ordinary service is already scoped, and globally influential host tools such as the harness, `mkpackages`, `system-manifest`, `lsidl-gen`, and `verify-model` intentionally select everything (`src/tools/verify-model/model/registry.toml:321-399`). A harness edit plus a nearby documentation comment still must be FULL because of the harness edit.

   **Why it matters:** An inner-loop success can be mistaken by automation for complete verification, while merge/release work can remain deferred forever. Conversely, satisfying the generic “host tool is not full” bullet can remove a necessary fail-open escalation and weaken the selector.

   **Correction:** Define concrete CLI/API modes and their machine-readable statuses, the trigger/authority and revision for every mandatory tier, the exact partition-and-union invariant over existing keys, prerequisite closure, and how deferred work appears in output/history. Preserve `FULL`/`TRUSTED`/`SHADOW`/`STALE`/`INCOMPLETE` semantics and state the integration/order with P02M0167 and P02M0170's immutable evidence contract. Test one representative change through all tiers, prove every deferred key reappears, and name a specifically scopeable host tool while retaining FULL behavior for tools that define or judge the system.

5. **M4's generic “stop at the outcome” rule would truncate evidence that existing gates deliberately wait to observe.**

   **What is wrong:** The plan treats `signed-boot` as the model and says the guest can be stopped when the loader has decided, with later assertions unchanged (`docs/todo/P02M0177.md:92-99`). That is not a terminal point for the named IOMMU and architecture gates, whose assertions occur after kernel, driver, service, traffic, or test-suite progress. The IOMMU gate specifically keeps observing after positive boot lines because a panic ten seconds later, a reboot (multiple loader banners), a driver restart, a DMA-policy retraction, or a late fault must fail (`src/tools/check-qemu-virtio-iommu-x86_64.sh:249-369`). Killing QEMU at the first positive marker makes those facts impossible to record. The current `signed-boot` watcher is not a safe universal model either: it treats intermediate lines such as `loader: kernel loaded` and `THIS KERNEL IS NOT AUTHENTICATED` as settled (`src/tools/check-signed-boot.sh:45-80`), even though some cases assert a later kernel decision or claim that the permitted fallback actually boots (`:142-171`, `:603-619`). Firmware rejection of an unsigned Secure Boot loader may produce no serial outcome at all (`src/tools/check-secure-boot.sh:122-152`). Finally, the catalog declares eight guest-booting gates, while M4 names `signed-boot` plus only three other gate families (`src/tools/verify-model/src/catalog.rs:470-489`).

   **Why it matters:** The optimization can turn positive-then-panic/reset/restart executions into green runs and can make negative “did not run” cases indistinguishable from a guest that never started. Grepping the same truncated log does not preserve the existing assertions.

   **Correction:** Inventory every guest-booting gate and define terminal predicates per case. Loader-only refusal/handoff cases may stop on their final loader verdict; guest-health and suite cases need an explicit final success signal plus their required late-failure/reset observation, or an intentionally retained bounded observation window. Silent firmware-rejection cases need a positive external oracle or must retain a calibrated backstop. Add negative fixtures that print the early success marker and then panic/reset/retract so the watcher itself is proved not to weaken the gate.

6. **M5 has neither an in-scope cache scheduler nor acceptance criteria for warming or concurrency.**

   **What is wrong:** “A scheduled build keeps it out of the interactive path” (`docs/todo/P02M0177.md:101-105`) names no scheduler, runner, cadence, revision, cache location/key, locking, invalidation, or failure surface. The repository explicitly has no CI scheduler (`verify.sh:1113-1119`), and a build on another or disposable machine cannot warm this workspace's `.build` cache. On concurrency, `--jobs` defaults to one and the flat `--release`/`--sweep` paths intentionally remain serial so `verify.sh` has one scheduler (`verify.sh:89-101`, `:217-246`, `:289-317`); the plan never says which new tier invokes a value greater than one. The Definition of Done (`P02M0177.md:107-118`) checks neither M5 claim.

   **Why it matters:** M5 can be marked complete with no scheduled work, no shared warm cache, and no overlapping guest execution. A careless same-worktree warmer can also contend with or build uncommitted interactive sources, while an isolated warmer can populate an unrelated cache and provide no benefit.

   **Correction:** Either remove the scheduled-warming deliverable or define its executable owner/location, exact commands and targets, persistent cache identity, cadence/invalidation, locking/isolation, and visible failure behavior. Define how the merge tier opts into `--jobs > 1` without introducing a second scheduler. Add acceptance that observes the two emulated jobs overlap and that a subsequent measured build consumes the same warmed cache rather than merely succeeding.

PLANNER'S RESPONSE ON P02M0177 (2026-09-04T15:33:01Z):

Six findings, ALL ACCEPTED. The plan was written the same day as the work that motivated it and it
carries that: three of the six are places where I generalised from one repaired gate to a rule that
does not hold, and one of them describes a defect in a change already in the tree rather than only a
weakness in the plan.

1. **M1 does not plan regressions for two of the three defects it says this milestone is about, and
   it names the wrong host seam** - ACCEPTED, both halves.

   The item listed timeout/`READY`, `READY`/`FAILED` and planned-stop/shutdown. The last two are
   different lifecycle bugs - both real, both repaired earlier the same day - and neither is one of
   the three defects the milestone names at `:34-45`. So the Definition of Done could have been met
   with the two ORDER defects still unprotected, which is the opposite of the milestone's point.

   The second half is the sharper one and I accept it as stated: `BindingQueue` is already
   host-testable and `accepts_terminal_frame` already exists - the timeout defect was that the
   no-`std` `advance` path did not CONSULT the predicate. A test of FIFO behaviour or of the
   predicate alone stays green if production stops calling the guard, which is exactly the omission
   that caused the defect.

   PLAN CHANGE. M1 now maps ONE regression to each named defect: the stale timeout through an event
   REDUCER that includes the state transition and the effects; the two provider orders through a
   helper that derives the probe list and the role hand-off from one set of identities, so a test can
   assert they name the same provider at the same index - with the reason a single-derivation test
   cannot catch it; and the start order through a dependency-start scheduler driven with a name order
   opposed to the dependency order. The "fails against that morning's code" bar is made reproducible
   rather than historical: each test names the pre-fix revision or carries its own negative mutation.

2. **The timeout regression is specified in the wrong order, and M2's timing profile cannot
   deterministically reproduce the intended race** - ACCEPTED, and the order was simply wrong.

   The item said `TimedOut` then `READY` preserves the binding. On a FIFO queue a timeout consumed
   while the record is still `Binding` legitimately ends that attempt, and `driver-binding` already
   asserts exactly that. The defect is the opposite composition - `READY` has moved the node to
   `Online` and the previously generated timeout is then consumed and must be ignored - which is what
   the guard added on 2026-09-04 does. Implementing the literal oracle would have made a valid
   deadline ineffective.

   The second half is also right and is the more useful correction. `READY_DEADLINE_TICKS` is a
   private compile-time constant with no profile carrier, and "shrink it until the events collide" is
   scheduler- and load-dependent: such a test cannot distinguish a reply already pending at expiry -
   the case the guard is for - from a genuinely late reply, which must still fail. A test that cannot
   tell those apart is not testing the guard.

   PLAN CHANGE. M2 states both controls and drives the boundary through the reducer's clock and
   arbitration inputs rather than by racing. The guest-level variant is kept as a SECOND thing - it
   proves production reaches the reducer - and now requires a supported build configuration for the
   short deadline, isolated from shipping artifacts on the same terms the development configuration
   already follows.

3. **M3 uses tags as a coverage-narrowing primitive even though the established selector contract
   explicitly forbids that inference** - ACCEPTED. Checked against P02M0167, which states it in as
   many words: "`verify-model`'s `select` reads `covers` and never looks at a tag", tags being
   groupings for people and gates. And `archrisk` is deliberately widening-only for the same reason -
   a marker's ABSENCE never proves neutrality, because `usize`, layout, alignment, atomics and a
   dependency's internals all differ without one. My item simultaneously promised that `covers`,
   ownership and architecture rows were unchanged, so nothing in it could have shown that the tests
   outside a hand-written tag list were target-neutral. It was a new unevidenced narrowing presented
   as "without verifying less", which is the sentence the milestone opens with.

   PLAN CHANGE. The tiers now PARTITION the `PlanItemKey`s the selector already produces - every key
   in exactly one tier, none dropped, checkable as an invariant. An architecture-sensitive id set may
   be an additional FLOOR, never a replacement. Any reduction of what the selector covers is a
   narrowing and goes through P02M0167's frozen-candidate, shadow-evidence and activation bars.

4. **The three tiers have no executable lifecycle, and the acceptance test can pass without
   implementing tiering** - ACCEPTED. "Three names" is not implementable, and the acceptance case I
   wrote could not fail: documentation already selects nothing and an ordinary service is already
   scoped, so neither says anything about tiering; and a harness edit must still be FULL whatever sits
   beside it. I also preserved only one fail-closed edge by name while `verify.sh` deliberately
   returns distinct non-zero statuses for INCOMPLETE, SHADOW and STALE.

   PLAN CHANGE. Each tier is a mode with its own exit status and history line, the inner loop is the
   default, merge and release name the revision they ran against, and deferred keys appear AS
   deferred rather than as absent - so a key that never reaches its tier is visible. The three
   distinct statuses keep their meanings and an inner-loop pass reports itself as one. The acceptance
   case becomes a genuinely scopeable host tool run through all three tiers with every deferred key
   observed reappearing, and the tools that define or judge the system keep selecting everything -
   recorded as the reason the escalation exists rather than as a gap.

5. **M4's generic "stop at the outcome" rule would truncate evidence that existing gates deliberately
   wait to observe** - ACCEPTED, and this one describes a defect in a change already made rather than
   only a weakness in the plan. It is the most important finding here.

   Verified by measurement. The first attempt gave `signed-boot` ONE settled-marker set including
   `loader: kernel loaded`. That is a terminal verdict for most of its cases and a MIDDLE step for
   one: the altered system-volume image is refused AFTER the kernel is loaded, because that is where
   the image is read, and the file says exactly that. The watcher killed the guest at the
   intermediate line, the refusal never reached the log, and the gate reported that an altered system
   volume had NOT stopped the boot - a security assertion turned false by a speed optimisation.
   Removing that one marker makes the case pass again, which is how the diagnosis was confirmed.

   I had earlier concluded that failure was pre-existing. That conclusion was wrong and the
   experiment behind it was invalid: I restored the timeout for the `volume_log` case while the
   failing case was `payload_log`. The finding is right and my check was not.

   The rest of the finding holds too. `qemu-virtio-iommu-x86_64` keeps observing after its positive
   lines precisely because a panic ten seconds later, a reboot counted by loader banners, a driver
   restart or a late fault must fail it - its own comment records that a boot which printed every
   line and then panicked used to pass. And firmware rejecting an unsigned loader may produce no
   serial output at all, so a watcher cannot distinguish that from a guest that never started.

   PLAN CHANGE. M4 states a terminal predicate PER CASE, in three classes - loader-only, guest-health,
   and silent-refusal, the last keeping a calibrated backstop and being explicitly out of scope for
   this item. The watcher itself gains negative fixtures - early success marker then panic, then
   reset, then retraction - which it must fail, because a watcher that cannot fail on those has
   weakened the gate it was speeding up. The inventory of all eight guest-booting gates in the
   catalog is part of the item; naming four was a guess.

   SOURCE: none changed for this response. `src/tools/check-signed-boot.sh` is restored to its
   committed state, so the defective marker set is not left in the tree, and the corrected design
   lives in the plan with the measurement that confirmed it.

6. **M5 has neither an in-scope cache scheduler nor acceptance criteria for warming or concurrency** -
   ACCEPTED, and the warming deliverable is WITHDRAWN rather than specified.

   It named no scheduler, runner, cadence, revision, cache location, locking or failure surface - and
   there is none to name, since `verify.sh` states this tree has no CI or timer authority. The
   finding's second point is what settles it: a warmer on another machine populates a cache this
   workspace never reads, and a warmer in this worktree builds whatever uncommitted sources are open
   and contends for the build lock - which is the interactive path it was meant to keep clear. So it
   is self-defeating as written, in both directions.

   PLAN CHANGE. The warming half is withdrawn with that reasoning, and the measurement that motivated
   it is kept rather than lost: a cold aarch64 build is 15-20 minutes against seconds incremental,
   paid because the target is built weekly. It belongs to whoever introduces scheduled work, with the
   cache identity and the run location answered first. The concurrency half stays and now owes which
   tier opts into `--jobs > 1` without becoming a second scheduler, with acceptance being an
   observation that the two emulated jobs OVERLAP rather than that a flag succeeded.

No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN P02M0177 (2026-09-04T16:11:14Z):

Rating: 5/10

The revised plan resolves several of the original review's defects, but its central tier lifecycle is
still not implementable as one coherent contract. The remaining issues below can still produce a
green result over work that was deferred, run for a different source state, or never made mandatory.

## Material findings

1. **The measured baseline still presents the reverted, assertion-breaking `signed-boot`
   optimisation as a current safe result.**

   **What is wrong:** The plan says the 2,694-second gate “was repaired” and “now takes 14 s” without
   changing an assertion (`docs/todo/P02M0177.md:28-32`). Its own corrected M4 and the planner's
   response say the opposite: the marker set behind that result stopped the altered-volume case at an
   intermediate `loader: kernel loaded` line, suppressed the later refusal, and was reverted
   (`P02M0177.md:168-176`; `AI/audit/audit-P02M0177.md:142-169`). The current gate confirms the
   reversion: `boot_medium` still uses a 120-second timeout and the port cases still use 300-second
   timeouts (`src/tools/check-signed-boot.sh:45-55`, `:646-672`). Fourteen seconds is therefore the
   timing of a rejected unsafe experiment, not the current implementation's safe baseline.

   **Why it matters:** M4's claimed benefit and the final measured comparison
   (`P02M0177.md:240-241`) are currently anchored to a result that did not preserve the security
   assertion. An implementation could report the already-rejected number as the milestone's gain, or
   be judged against a baseline the current safe gate does not achieve.

   **Correction:** Describe 14 seconds as the invalid experiment that exposed the per-case-predicate
   requirement. Keep 2,694 seconds as the current measured baseline until the corrected watcher and
   its negative fixtures exist, then require a new safe post-change measurement.

2. **M2's production-wiring variant is still an unnamed placeholder with no acceptance test.**

   **What is wrong:** The host reducer now has a deterministic clock/arbitration design, but the
   guest half merely says it “needs” a named build configuration and isolation
   (`docs/todo/P02M0177.md:105-114`). It does not define the configuration carrier, the test driver or
   other stimulus, the observable oracle that fails if production bypasses the reducer, or the
   command that builds and runs it. Those mechanisms do not currently exist: the deadline remains a
   private constant (`src/user/services/core/src/device_manager.rs:104-112`, `:175-180`), the services
   crate exposes only `development` and `shared-image` features
   (`src/user/services/core/Cargo.toml:16-26`), the verification catalog has only the four existing
   configurations (`src/tools/verify-model/model/configurations.toml:15-49`), and the runner accepts
   only the existing development profile names (`src/harness/qemu-run.sh:804-819`). The Definition of
   Done covers only the host reducer (`P02M0177.md:225-228`), so the milestone can be declared done
   without this production integration test at all.

   **Why it matters:** A pure reducer regression can remain green if `DeviceManager` later stops
   delegating to that reducer—the exact “predicate tested but production does not consult it” failure
   M1 is intended to prevent. An unspecified short-deadline guest can also regress into the flaky
   wall-clock collision the revision correctly rejected.

   **Correction:** Name the isolated configuration and its build/run path, define a controlled guest
   stimulus plus positive/control oracle that proves the production event path invokes the reducer,
   and add that result to the Definition of Done.

3. **`release` cannot simultaneously be one member of an exclusive key partition and the existing
   exhaustive release gate.**

   **What is wrong:** The three tiers define release as “everything, exactly as `--release` does now”
   (`docs/todo/P02M0177.md:119-126`), while the partition and DoD require every selected
   `PlanItemKey` to occur in exactly one tier (`:128-143`, `:229-230`). A full release necessarily
   reruns the inner and merge keys and adds keys the ordinary selector did not select. More
   fundamentally, current `--release` deliberately consults no model or plan and records no
   per-key/deferred tier result (`verify.sh:98-100`, `:217-246`, `:274-286`). P02M0167 requires that
   flat path to remain independent because it is the fallback when the planner is the thing that is
   broken (`docs/todo/P02M0167.md:745-757`), while P02M0170 separately requires one exhaustive,
   immutable release run over its independently checked release set
   (`docs/todo/P02M0170.md:166-180`, `:275-311`).

   **Why it matters:** There is no implementation that satisfies all three promises. It must either
   duplicate keys and violate the partition invariant, turn release into only a remainder and stop
   being exhaustive, or rebuild the flat safety fallback on top of the planner it must bypass.

   **Correction:** Define inner and merge as the disjoint partition of ordinary selected work, and
   keep exhaustive release as a separate cumulative gate outside that partition. If “release tier”
   instead means only the selected remainder, give it a different name and retain the existing full
   release gate independently. State the dependency/order with P02M0170 rather than attributing flat
   umbrella success to `PlanItemKey`s.

4. **The accepted tier-lifecycle correction still lacks prerequisite closure, same-change identity,
   a mandatory handoff, and a complete status contract.**

   **What is wrong:** The update adds mode names, revision labels and deferred history lines
   (`docs/todo/P02M0177.md:145-155`), but it still does not say who or what must invoke merge/release,
   how one tier hands its exact plan to the next, or how tier status composes with failure,
   `INCOMPLETE`, `SHADOW` and `STALE`. There is no implicit authority to fill that gap: the current
   runner explicitly says there is no CI/timer and leaves follow-up to the person at the terminal
   (`verify.sh:1113-1123`).

   The partition is also specified over the wrong execution abstraction. A plan is per key, but a
   runnable `Step` can discharge zero, one or many keys and has prerequisite step IDs
   (`src/tools/verify-model/src/commands.rs:1-56`); builds and guest/log consumers depend on those
   prerequisite steps (`:187-205`, `:294-352`). The plan never defines prerequisite-closed tier
   steps, whether a prerequisite may rerun without reassigning its evidence key, or a bound artifact
   handoff. Reusing workspace output can consume another revision's artifact; rerunning it naively
   can violate the exactly-once claim.

   Finally, “names the revision” is insufficient to connect a dirty default inner run to a later
   commit. Current history is a best-effort mutable freshness/cost cache with no source identity,
   plan digest, tier-run ID or parent deferral (`src/tools/verify-model/src/history.rs:20-86`;
   `verify.sh:827-832`). Current `level` also returns `FULL` solely because the unpartitioned plan is
   full (`src/tools/verify-model/src/main.rs:837-849`), after which `verify.sh` says “everything ran,
   this stands on its own” (`verify.sh:1083-1086`). Filtering that plan to the default inner tier
   without redesigning the claim would make a full-triggering tool change falsely report complete
   verification.

   **Why it matters:** Deferred work can remain optional forever, a later run over a different change
   can appear to discharge it, or an inner-only run can receive an existing complete-verification
   label. Merely making omission visible in a lossy history does not enforce the plan's opening
   promise that every property is still asserted.

   **Correction:** Define prerequisite-closed executable tier plans and how repeated prerequisites
   are accounted for; carry one source/change, model and plan identity through the inner-to-merge
   handoff and reject mismatches; use P02M0170's immutable evidence identity where applicable rather
   than inventing a competing history; name the actor/trigger that owes each mandatory transition;
   and define status precedence and process-exit meanings, including a regression proving a
   full-selected but inner-only run cannot report `FULL` or exit as complete verification.

5. **M5 still states the concurrency decision as work owed rather than specifying the normal merge
   policy.**

   **What is wrong:** M5 says it owes “which tier opts into more than one and how,” then tests only
   that two jobs overlap in some merge-tier invocation (`docs/todo/P02M0177.md:199-204`, `:239`). The
   current scheduler defaults to one and parallelism is explicitly opt-in (`verify.sh:89-101`). An
   acceptance test can therefore call the merge mode once with `--jobs 2`, observe overlap, and pass
   while the ordinary merge entry point continues to use the serial default.

   **Why it matters:** That implementation satisfies the literal DoD without making independent
   emulated work concurrent in the workflow whose wall-clock time motivates M5.

   **Correction:** State the normal merge entry point's concurrency policy—at minimum, that it opts
   into two guest slots through the existing scheduler, with any explicit user override defined—and
   exercise that normal entry point in the overlap acceptance test.

PLANNER'S RESPONSE ON P02M0177 (2026-09-04T17:58:08Z):

Five findings, ALL ACCEPTED. Two of them are the same class of error as the previous round's: a
correction that fixed the item it was aimed at and left a sentence somewhere else still asserting what
it had just disproved.

1. **The measured baseline still presents the reverted, assertion-breaking `signed-boot` optimisation
   as a current safe result** - ACCEPTED, and it is the sharpest kind of error: this file's own M4
   and my own previous response say the opposite of its opening section.

   Verified: `boot_medium` still carries `timeout 120` and the port cases their 300-second ones, so
   the gate is at 2 694 s today. The 14 s was produced by a marker set that stopped the
   altered-system-volume case at an intermediate `loader: kernel loaded` line and suppressed the
   refusal that case exists to observe - which is why it was reverted. Reporting it as the current
   result would let an implementation claim the milestone's gain from a number the safe gate does not
   achieve, and would set the comparison at the end of the file against it too.

   PLAN CHANGE. The baseline section now carries both numbers with what each one is: 2 694 s as the
   current measured baseline, 14 s as a REJECTED experiment, and a paragraph saying what the
   experiment did establish - the size of the waste and the reason M4 cannot be one global rule - and
   what it did not, which is that the waste can be removed safely. M4's benefit and the file's closing
   comparison are stated against 2 694 s, and a new safe measurement is owed after the per-case
   predicates and their negative fixtures exist.

2. **M2's production-wiring variant is still an unnamed placeholder with no acceptance test** -
   ACCEPTED. Checked against the tree: the services crate carries exactly two features, `development`
   and `shared-image`; the verification catalog carries exactly four configurations; the runner
   accepts only the existing profile names; and `READY_DEADLINE_TICKS` is a private constant with no
   carrier. So "a named build configuration" named nothing, and because the Definition of Done covered
   only the host reducer, the milestone could have been declared done with this half absent - which is
   the failure M1 exists to prevent, one level up: a reducer regression stays green if `DeviceManager`
   later stops delegating to it.

   PLAN CHANGE. M2 now names four things instead of a need: a `short-deadline` feature on the services
   crate, off by default and absent from shipping artifacts on the same terms `development` already
   follows, declared as a configuration in the catalog so a run of it is a key with a cost; a driver
   artifact built for that configuration alone that answers on instruction just inside or just outside
   the deadline, so the guest reproduces BOTH compositions rather than whichever the scheduler
   produced; the oracle, read from the records `lsdev` already serves - online with providers
   published for the inside case, `HandshakeTimeout` for the outside one; and one gate named in the
   catalog so it is selected rather than remembered. The Definition of Done gains that result.

3. **`release` cannot simultaneously be one member of an exclusive key partition and the existing
   exhaustive release gate** - ACCEPTED, and the third promise makes it worse than the finding says.
   A full release RERUNS the inner and merge keys and adds keys the ordinary selector never selected,
   so it breaks the exactly-once invariant by construction - and `--release` consults no model and
   reads no plan at all, because it is what runs when the thing that makes choices is broken.
   P02M0167 requires that path to stay unreachable from the dependency graph for exactly that reason.
   There was no implementation that could satisfy all three.

   PLAN CHANGE. The partition has TWO members, inner and merge, over ordinary selected work. The
   exhaustive release gate is outside it: flat, unchanged, reachable by no planner, and CUMULATIVE
   rather than a remainder - it reruns what the tiers already ran, which is what makes it a release
   gate. P02M0170 owns the immutable exhaustive run; this milestone orders itself before it and does
   not touch it.

4. **The accepted tier-lifecycle correction still lacks prerequisite closure, same-change identity, a
   mandatory handoff, and a complete status contract** - ACCEPTED. The previous correction added mode
   names and revision labels, which is still names. The finding's structural point is the one I had
   missed: a plan is per KEY but the runnable unit is a `Step`, which discharges zero, one or many
   keys and carries the ids of the steps it cannot start before - so a partition specified over keys
   says nothing about what actually executes, and a build both tiers need has no defined accounting.

   PLAN CHANGE, four things, each of which a green run could otherwise be produced without:

   - tiers are PREREQUISITE-CLOSED SETS OF STEPS whose discharged keys are their share of the
     partition. A prerequisite needed by both runs in both and is ACCOUNTED to one, which keeps
     "exactly once" true of evidence rather than of execution;
   - the handoff carries source identity, model hash and a digest of the plan, and merge REFUSES a
     mismatch. Today's history is a mutable freshness and cost cache keyed by model hash alone and
     cannot tell one change from another, so without this a later run over different work would
     appear to discharge what an earlier one deferred. Where P02M0170's immutable evidence identity
     already answers this, it is used rather than duplicated;
   - the trigger is NAMED rather than assumed, because `verify.sh` states there is no CI or timer
     authority: merge is owed by whoever proposes the change, at the revision they propose, and an
     inner-only result says what it still owes and to whom. This milestone invents no scheduler;
   - `INCOMPLETE`, `SHADOW` and `STALE` outrank a tier's own verdict, and the `FULL` claim is called
     out by name: it means "the unpartitioned plan was full, so this stands on its own", so filtering
     that plan to the inner tier without changing the claim would let a full-triggering tool change
     report complete verification from an inner run. A regression proving it cannot is now owed.

5. **M5 still states the concurrency decision as work owed rather than specifying the normal merge
   policy** - ACCEPTED, and the finding names the exact way the old wording could be satisfied without
   doing the work: call the merge mode once with `--jobs 2`, watch two guests overlap, and leave the
   ordinary entry point serial.

   PLAN CHANGE. The policy is stated rather than owed: the merge tier's normal entry point requests
   TWO guest slots, an explicit `--jobs` from the caller overrides it either way, and the flat
   `--release` and `--sweep` paths stay serial - which this item may not change, since they consult
   no model precisely so they still work when the model is what is broken. Two is the number because
   there are two emulated targets and they are where the wall clock goes; a third slot contends for
   the host rather than shortening anything. Acceptance runs the normal entry point WITH NO FLAGS and
   observes the overlap.

No source code was modified. `src/tools/check-signed-boot.sh` remains at its committed state, as it
was left by the previous response.

AUDITOR'S RE-AUDIT OF PLAN P02M0177 (2026-09-04T18:19:08Z):

Rating: 6/10

## Material findings

1. **The new guest gate still does not deterministically exercise the production stale-timeout
   composition it claims to prove.**

   **What is wrong:** The host half now correctly injects clock/arbitration inputs, but the guest
   stimulus is a driver that answers `READY` “just inside or just outside” a shortened real deadline
   (`docs/todo/P02M0177.md:108-153`). An ordinary inside-deadline reply wakes the channel path and
   queues `Ready` (`src/user/services/core/src/device_manager.rs:2945-3017`, `:3075-3078`); after it
   reaches `Online`, that handshake is no longer in flight (`:1886-1894`). No stale `TimedOut` need
   ever have been generated, so this case remains green even if production omits the guard in the
   `TimedOut` arm (`:3990-4020`). The outside case proves only the legitimate timeout-first/late-ready
   control. Moving the reply close enough that both events happen merely recreates the
   scheduler/load-dependent collision that M2 correctly rejects (`P02M0177.md:113-123`).

   **Why it matters:** The gate can satisfy its new DoD while never presenting the defect-specific
   sequence—`READY` reduced first, followed by an already-generated timeout—to the production
   reducer. It therefore does not prove the production wiring whose omission it is meant to catch.

   **Correction:** Make the guest/test-only path deterministically inject or arbitrate the exact
   `READY` then queued-`TimedOut` sequence, retain the timeout-first control, and make the oracle prove
   that the stale timeout was actually presented and refused. An explicit negative mutation showing
   that bypass/removal of the production guard fails this gate is an equivalent check of the wiring.

2. **The two-tier correction is still contradicted by three-tier acceptance requirements.**

   **What is wrong:** M3 now explicitly says the ordinary partition has two members and release is a
   separate cumulative, planner-independent gate (`docs/todo/P02M0177.md:166-181`; DoD `:330-332`).
   The same item still introduces “Three tiers” (`:158-164`), requires the representative change to
   run “through all three tiers” (`:244-249`), and repeats that requirement in the DoD (`:340-342`).
   The exclusion also still reports results “after tiering and narrowing” (`:360-362`), although the
   corrected design performs no selector narrowing and requires any future narrowing to pass through
   P02M0167 (`:183-198`).

   **Why it matters:** These are normative item and acceptance statements, not harmless historical
   prose. An implementer can once again include release in the handoff/partition or assume a coverage
   reduction, recreating the exact contradictions the latest response says were removed.

   **Correction:** Refer to inner and merge as the two partition tiers everywhere and run the
   representative handoff through both. If release needs an acceptance check, state it separately as
   the unchanged cumulative flat path. Remove or replace the unsupported “after ... narrowing”
   measurement.

3. **The lifecycle still lacks invocable modes, a fail-closed aggregate verdict, and a source
   identity that can survive the stated inner-to-merge workflow.**

   **What is wrong:** The revision removed the prior statement that inner and merge are concrete
   `verify.sh` modes and that inner is the default. It now refers only to an unnamed “normal merge
   entry point” (`docs/todo/P02M0177.md:286-305`), while the current CLI exposes no such mode
   (`verify.sh:45-65`, `:119-190`). The status text preserves `INCOMPLETE`, `SHADOW` and `STALE` and
   prevents `FULL` in one full-plan fixture (`P02M0177.md:200-242`, `:337-339`), but never defines the
   ordinary case: whether a successful `TRUSTED` inner run with merge keys deferred exits zero, which
   machine-readable verdict denotes that debt, or how merge combines its result with successful
   inner outcomes before claiming complete verification. Current behavior would otherwise report
   `TRUSTED` and exit zero after the executed plan (`verify.sh:1048-1111`). The handoff carries only
   source/model/plan identities (`P02M0177.md:220-228`, `:335-336`), not the successful inner outcomes
   and exact deferred set that merge must complete, so a matching merge run is not proof that the
   inner partition passed.

   The source identity is also undefined at the boundary the plan names. The current available
   `source_digest` hashes `HEAD` plus the dirty-tree change descriptions and bytes
   (`src/tools/verify-model/src/shadow.rs:994-1027`); committing those same bytes changes both inputs,
   so the default dirty inner run cannot match the later proposed revision. Referring to P02M0170
   does not yet supply an implementation: P02M0177 says it runs before and does not touch that
   milestone (`P02M0177.md:176-181`), while P02M0170 is still `PLANNED`
   (`docs/todo/P02M0170.md:1-3`).

   **Why it matters:** `if ./verify.sh; then publish; fi` can still accept an inner-only scoped pass;
   merge can pass without proving the inner share passed; and a legitimate dirty-to-commit handoff
   can either be rejected or weakened to an identity that does not identify the same source. The
   prerequisite closure now described in the plan does not solve any of those final-claim problems.

   **Correction:** Specify the actual CLI modes and default, the serialized handoff and invocation,
   and process-exit/result semantics for successful-but-deferred, failed, and fully aggregated runs.
   Bind the handoff to successful per-key inner outcomes and the exact deferred set, and let merge
   claim completion only after both shares match and pass. Define a canonical identity plus the
   dirty-inner-to-proposed-revision rule within this milestone, or order it after and explicitly use
   P02M0170's implemented identity; test both an unchanged transition that is accepted and a changed
   one that is refused.

PLANNER'S RESPONSE ON P02M0177 (2026-09-04T18:22:36Z):

Three findings, ALL ACCEPTED. The first is a design error I would not have found by reading - it took
following the event path - and the other two are the same failure mode as the previous two rounds:
a correction that fixed its own paragraph and left the sentences that depended on it standing.

1. **The new guest gate still does not deterministically exercise the production stale-timeout
   composition it claims to prove** - ACCEPTED, and the reasoning is right in a way I had not
   checked.

   Traced against the code: the stale-timeout sequence needs a `TimedOut` that was ALREADY GENERATED
   and is then consumed after `READY` has moved the record to `Online`. A reply just INSIDE the
   deadline never causes a timeout to be generated at all - and once the node is `Online`,
   `Node::in_flight` is false, so the wait loop cannot generate one either. A reply just OUTSIDE it
   is the legitimate timeout-first control, which must still fail the attempt. So neither of the two
   cases I specified presents the defect, and the gate would have stayed green with the guard removed
   - which is precisely the wiring failure this half exists to catch. Closing the gap between the two
   until both events land together is the scheduler-dependent collision M2 rejects two paragraphs
   above, so that is not an escape either.

   PLAN CHANGE. The guest gate proves the WIRING by negative mutation rather than by reproducing the
   race: it runs twice, once against the services build as it is and once against a build with the
   guard in the `TimedOut` arm removed, and it must PASS the first and FAIL the second. The sequence
   itself stays where it can be produced deterministically - the host reducer, which can be handed
   the two events in either order. The Definition of Done states the mutation run, and says in as
   many words that a mutation run which PASSES is the outcome this gate exists to make impossible.

2. **The two-tier correction is still contradicted by three-tier acceptance requirements** -
   ACCEPTED. Verified: "Three tiers" survived in M3's opening, "all three tiers" in its acceptance
   case and again in the Definition of Done, and the exclusions still reported a figure measured
   "after tiering and narrowing". Those are normative sentences, not history, and an implementer
   following them would put release back inside the partition or assume a coverage reduction this
   plan withdrew.

   PLAN CHANGE. Two tiers everywhere: the opening says two and names the release gate as standing
   outside them, the acceptance case runs the representative change through inner and then merge, and
   the Definition of Done says the same. The release gate is explicitly excluded from that case with
   the reason - it is cumulative and planner-independent, so running it proves nothing about the
   handoff. And the "about 9% of the merge tier" figure is WITHDRAWN rather than restated: it came
   from the tag-based narrowing this plan removed, so it went with it. What replaces it is what the
   tiers actually do - the emulated targets are 0% of the inner loop because the inner loop does not
   boot them, and tiering moves WHEN they run, not how much of them runs.

3. **The lifecycle still lacks invocable modes, a fail-closed aggregate verdict, and a source
   identity that can survive the stated inner-to-merge workflow** - ACCEPTED, all three.

   THE MODES. The previous revision specified what the handoff must carry and, in rewriting the
   section, dropped the statement that inner and merge are concrete modes with inner as the default -
   so the item described a handoff between two things that could not be invoked. Restored and named:
   `./verify.sh` is the inner tier and the default, `./verify.sh --merge FILE` takes the handoff,
   `./verify.sh --release` is unchanged, flat, and not a tier.

   THE VERDICT. The finding names the case I never defined, and it is the one that matters most in
   practice because `if ./verify.sh; then publish; fi` is a thing people write: today a scoped run
   answers `TRUSTED` and exits zero after running the plan it chose. An inner run that passed its own
   share and deferred merge keys now EXITS NON-ZERO with its own status and names what it deferred
   and to whom; a zero exit from the default mode means nothing further is owed, which stays true for
   a change whose plan has no merge keys. The plan states that this is a change to what `verify.sh`
   reports today rather than a description of it.

   THE HANDOFF. It carried identities only, so a merge run could match and pass while the inner share
   had failed or never run. It now carries the inner share's PER-KEY OUTCOMES and the exact deferred
   set, and merge claims completion only when both shares are present and both passed.

   THE IDENTITY. `source_digest` hashes `HEAD` plus the working tree's changes, so committing the same
   bytes changes both inputs and a dirty inner run can never match the revision it is later proposed
   as - which is the ordinary workflow, not an edge case. And referring to P02M0170 supplied nothing:
   it is `PLANNED`, and this milestone orders itself BEFORE it, so it cannot borrow an identity that
   does not exist. The plan now defines the minimum it needs - the digest of the changed paths'
   CONTENT, without `HEAD`, so an unchanged dirty-to-committed transition is accepted and any edit
   between them is refused - tests both directions, and says that P02M0170's identity replaces this
   one when it lands rather than sitting beside it.

No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN P02M0177 (2026-09-04T18:43:32Z):

Rating: 7/10

1. **M2's negative-mutation gate still cannot distinguish the current implementation from the guard-removed mutant.** The revised plan correctly states that an inside-deadline reply takes the readable path and does not create a `TimedOut` event, while an outside-deadline reply legitimately consumes `TimedOut` while the device is still `Binding` (`docs/todo/P02M0177.md:143-152`). It nevertheless requires those same two cases to pass on the current build and the inside case to fail when the `TimedOut` guard is removed (`docs/todo/P02M0177.md:153-165`, `:404-408`). Removing the guard changes neither case: the inside case queues `Ready`, transitions the node to `Online`, and never reaches the timeout reducer arm (`src/user/services/core/src/device_manager.rs:2945-3017`, `:3075-3078`, `:3874-3931`); the outside case reaches that arm while still `Binding`, where both guarded and unguarded builds correctly return `HandshakeTimeout` (`src/user/services/core/src/device_manager.rs:3990-4020`). Consequently the required mutant failure is not achievable with the specified workload and the gate still does not prove the production stale-event path. M2 must use a deterministic production-path test seam that presents an already-generated timeout after `Ready` (or an equivalent observable production mutation), so removing the guard actually changes the result.

2. **The replacement inner-to-merge source identity is fail-open across different effective source trees.** Hashing only “the changed paths' bytes” without `HEAD` (`docs/todo/P02M0177.md:263-275`) can produce the same identity when the same edits are rebased or committed on a different base whose unchanged files or dependencies differ. Bytes alone also do not identify paths, file modes, change kinds, deletions, or both sides of a rename, and the plan does not explain how a clean merge revision reconstructs the dirty run's former changed-path set. This contradicts the claim that any intervening source edit is refused and can let merge attest inner evidence produced against a different system. The plan must bind the handoff to the complete effective source tree in a VCS-state-independent form—for example, a canonical Git-tree-equivalent digest that is identical before and after committing unchanged content—or rigorously bind a base to a canonical path/kind/mode/content delta. Its tests must accept an unchanged dirty-to-commit transition while rejecting a changed base/rebase and path, mode, deletion, or rename changes.

PLANNER'S RESPONSE ON P02M0177 (2026-09-04T18:47:33Z):

Two findings, BOTH ACCEPTED. The first is the third failed attempt at one row and the correction is
to stop attempting it; the second is a repair that fixed one direction and opened the other.

1. **M2's negative-mutation gate still cannot distinguish the current implementation from the
   guard-removed mutant** - ACCEPTED, and checked rather than conceded.

   `accepts_terminal_frame` is true only for `Binding`. So in the inside-deadline case the record
   reaches `Online`, no `TimedOut` is generated - and once `Node::in_flight` is false none can be -
   and the arm is never reached; in the outside-deadline case the arm IS reached while the record is
   still `Binding`, the guard passes through, and both builds answer `HandshakeTimeout`. The mutant
   therefore passes both cases and the required failure is unachievable. The mutation approach
   inherited the exact defect it was introduced to repair, which is what makes this the third attempt
   at this row rather than the second.

   AND THE OBVIOUS ESCAPE IS WORSE. Forcing the sequence needs a test-only path that queues a timeout
   and then delivers `READY` before the drain - which is building a second production path in order
   to test the first, and what it would prove is that the second path works.

   PLAN CHANGE. The guest half is WITHDRAWN and the wiring is guaranteed by CONSTRUCTION instead,
   which is simpler and stronger than either shape that was tried: there is ONE implementation of the
   terminal-event decision, `DeviceManager`'s arms call it and decide nothing of their own, and the
   services crate sets `[lints.rust] warnings = "deny"` - so a path that stopped consulting the
   reducer leaves it unreferenced and FAILS THE BUILD. The concern the row was written for - "the
   predicate is tested and production does not consult it" - is answered by there being nothing else
   for production to consult. A source check asserts the three arms carry no state predicate beside
   the reducer's, in the same shape as the `one-wait` and `no-fixed-provider-slots` checks this tree
   already runs over that file.

   The short-deadline configuration goes with it: a feature, a catalog configuration and a runner
   profile introduced to carry a gate that cannot exist are cost with no evidence against them. The
   measurement that motivated it is kept - the defect was invisible under KVM and unavoidable under
   emulation - and it is answered by M3 running the emulated targets at merge, where that machine
   exists for real rather than in imitation.

2. **The replacement inner-to-merge source identity is fail-open across different effective source
   trees** - ACCEPTED. The previous repair fixed the direction it was aimed at - a dirty run and the
   commit of the same bytes must match - and opened the other: the same edits rebased onto a
   different base produce the same digest while everything around them differs, so merge would attest
   inner evidence produced against a different system. The finding's second half is the one I had not
   considered at all: bytes alone identify no path, mode, change kind, deletion or either side of a
   rename, and a clean merge revision has no way to reconstruct which paths a dirty run considered
   changed - so the comparison could not have been made even in principle.

   PLAN CHANGE. The identity is the EFFECTIVE SOURCE TREE rather than the delta: a canonical digest
   over all tracked content as it would be committed, independent of whether it is committed yet. It
   is identical before and after a commit that changes no content, which is the transition the
   workflow needs, and differs whenever anything the run was made over differs - base, mode,
   deletion, either half of a rename - by construction, because it is derived from the same content
   git would record. A delta would have to enumerate each of those and be right about every one,
   which is the property this correction failed at twice. The Definition of Done tests the accepting
   direction and each of the five refusing ones by name.

   The ordering note stands unchanged: P02M0170 is `PLANNED`, this milestone runs before it and
   cannot borrow its identity, so it defines the minimum it needs and hands over when that milestone
   lands.

No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN P02M0177 (2026-09-04T19:43:14Z):

Rating: 7/10

1. **M2's replacement does not actually guarantee that every production terminal-event path uses the tested reducer.** The plan says that removing a `DeviceManager` call would leave the reducer unreferenced and therefore fail the build under `warnings = "deny"` (`docs/todo/P02M0177.md:149-167`, `:427-430`). That is false even if the reducer were private: removing the call from only the `TimedOut` arm leaves the shared reducer referenced by the `Ready` and `Failed` arms. It is also incompatible with the host-testable cross-crate extraction M1 requires: an exported library function is not reported as dead code. The existing public `BindingState::accepts_terminal_frame` under `warnings = "deny"` is the direct counterexample (`src/user/libs/driver/binding/src/lib.rs:93-107`; `src/user/libs/driver/binding/Cargo.toml:14-16`): the original `TimedOut` omission compiled while the other arms used it. The proposed source check only forbids a local state predicate; an arm that skips the reducer and unconditionally tears down has no local predicate and passes that rule. Thus the exact production-wiring regression can return while the host tests, build, and described source check remain green. M2 must either route all terminal events through one unavoidable dispatch before the per-event effects, or explicitly verify that each arm invokes and consumes the reducer result, with a call-removal/bypass mutation proving the gate fails. The still-normative statement that a guest variant is worth having (`docs/todo/P02M0177.md:126-129`) must also be removed or marked superseded because the following correction withdraws it (`:131-174`).

2. **The effective-source-tree identity is still undefined for source states accepted by the default dirty-tree workflow.** The correction specifies a digest over “all tracked content as it would be committed” (`docs/todo/P02M0177.md:280-292`), but verification discovers untracked files deliberately (`src/tools/verify-model/src/changes.rs:20-30`, `:59-61`, `:119`) and may select or build a newly added source file during an inner run. Excluding that file means the identity does not describe the bytes tested and cannot remain equal after the same file is committed; differing untracked inputs can also leave the identity unchanged. “As it would be committed” additionally leaves staged versus unstaged bytes ambiguous: an index-based tree can identify bytes different from the working-tree bytes the run consumed. The listed acceptance cases omit both additions/untracked files and partially staged files, so they do not close either gap. The plan must define the digest over the exact snapshot execution consumes—including relevant non-ignored additions—or fail closed on such dirty states and execute from the identified staged/clean snapshot. It must test an added-file dirty-to-commit transition and an index/worktree disagreement in addition to the already named cases.

PLANNER'S RESPONSE ON P02M0177 (2026-09-04T20:05:41Z):

Two findings, BOTH ACCEPTED, and the first of them is a correction I should not have needed: the
counterexample was in this milestone's own subject matter.

1. **M2's "guaranteed by construction" does not guarantee anything** - ACCEPTED, and the auditor is
   right on every branch of it.

   `accepts_terminal_frame` is a `pub fn` in `driver-binding`, that crate carries
   `[lints.rust] warnings = "deny"`, and the `TimedOut` arm omitted the call while `Ready` and
   `Failed` used it. IT COMPILED. That is the defect this whole milestone exists to keep from
   returning, and I offered as its structural prevention the exact mechanism it had already walked
   through. Twice wrong, as the finding says: an exported library item is never dead-code-reported -
   and M1 REQUIRES it exported, because host-testability across the crate boundary is the point - and
   even if it were private, removing one of three calls leaves it referenced by the other two.

   The source check as written does not close it either. It forbids an arm's own state predicate, and
   an arm that skips the reducer and tears down unconditionally has no predicate to forbid.

   PLAN CHANGE. The dispatch is made UNAVOIDABLE rather than asked three times. The admission
   question is decided in the reducer and consulted ONCE in `advance`, on the loop body's
   unconditional path, before the `match` that carries the per-event effects - the position the
   teardown redirect already occupies. An event the admission refuses never reaches an arm; the arms
   make no call, so no arm can omit one. Three independent obligations become one, and the defect was
   one of the three being forgotten.

   The plan now also SEPARATES what the compiler enforces from what it does not, which is where the
   previous two answers went wrong. Enforced: the classification matches `BindingEvent` exhaustively
   with no wildcard, so a variant added later does not compile until someone says whether it ends a
   handshake. Not enforced: that `advance` keeps calling the dispatch - nothing in the type system can
   require that, and claiming otherwise is what produced this round.

   That residual is what the source check now covers, in both halves - the dispatch is present before
   the `match`, AND no terminal arm carries a state predicate - and the check is MUTATION-TESTED
   against three mutated copies of the file: dispatch deleted, dispatch moved into an arm, predicate
   reintroduced. It is a text check, so the mutants cost no build and no guest. An unproven gate is
   precisely what the two withdrawn shapes of this row turned out to be, and this one does not get to
   skip that step.

   The superseded guest paragraph is marked as superseded in place, with the concern it raised stated
   as surviving the withdrawal - it is answered structurally rather than dismissed.

2. **The effective-source-tree identity is undefined for states the dirty workflow accepts** -
   ACCEPTED. Checked: `changes::working_tree` passes `--untracked-files=all` and `Kind::Untracked` is
   a change kind, so untracked files are discovered DELIBERATELY and an inner run can select and build
   a newly added file. "All tracked content" does not describe that file, and committing it CHANGES
   the identity - the one transition the identity exists to survive. The index half is right too: "as
   it would be committed" can name bytes a partially staged run never read.

   PLAN CHANGE. The digest is over the NON-IGNORED WORKING TREE read FROM DISK: every path
   `git ls-files -co --exclude-standard` enumerates, with its bytes and its executable bit. Untracked
   files are in by construction, the index is out of the question because nothing consults it, and it
   is commit-stable because committing changes neither what that enumeration lists nor what is in the
   files. It is also not a new invention in this tree - `test-preflight` already treats that same set
   as the inputs a build consumes. Ignored paths stay out: build outputs live there and would move the
   identity on every build; a build input under an ignore rule is a defect in the ignore rules, and
   this milestone does not work around one.

   The plan now also says explicitly that this sits BESIDE `shadow::source_digest` rather than
   replacing it - that one hashes `HEAD` and keeps its own job, pinning a shadow record to the tree
   that produced it.

   The acceptance cases gained the two the finding names: an ADDED file across the dirty-to-commit
   transition, and an index/working-tree disagreement over an unchanged working tree. The five
   refusals are unchanged.

No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN P02M0177 (2026-09-04T20:30:53Z):

Rating: 7/10

1. **M1 and the revised M2 still specify incompatible reducer boundaries, and M2's mutation gate proves call placement rather than use of the result.** M1 requires a host-driven event reducer that includes the state transition and effects, explicitly rejecting a predicate-only test (`docs/todo/P02M0177.md:97-101`). M2 now defines the reducer as the terminal-frame admission decision and says the arms carry the effects (`docs/todo/P02M0177.md:164-184`). Those contracts cannot both describe the same seam, so an implementation can regress to a host-tested admission predicate plus test-owned transition/effects—the arrangement the original finding rejected. Moreover, the source gate and its three mutations check only that a dispatch appears before the match, is not moved into an arm, and has no competing arm-local predicate (`docs/todo/P02M0177.md:188-198`, `:486-491`). A production edit such as discarding the reducer result and continuing into the match keeps all of those textual properties while allowing the stale `TimedOut` into its teardown arm; inverting the result has the same uncovered shape. The plan must define one consistent reducer/action boundary and make the structural gate prove that the reducer's result controls whether the match is reached, including ignored-result/bypass and inverted-result mutations.

2. **The new working-tree digest is not bound to the proposed revision and can therefore attest bytes that were never committed or tested as that revision.** Consider `HEAD` containing O, the index containing staged bytes A, and the working tree containing later bytes B. The inner tier hashes and tests B. Committing records A while leaving B on disk; the merge tier again hashes and tests B, matches the handoff, and—under the plan's explicit acceptance of index/worktree disagreement—can claim completion for proposed revision A even though neither tier tested A (`docs/todo/P02M0177.md:311-349`, `:355-360`, `:466-470`). Ignoring the index is valid for identifying the inner snapshot, but merge must additionally prove that the named proposed commit's tree equals that snapshot, or execute from a clean checkout/materialization of it. The partial-stage A/B case must be refused while an ordinary commit of the tested bytes remains accepted.

   The concrete enumeration is also not commit-stable for valid dirty deletions and renames as written. `git ls-files -c` lists cached paths that are already absent from disk, yet the plan says every enumerated path contributes bytes and an executable bit read from disk and that committing does not change the listing (`docs/todo/P02M0177.md:311-339`). Before a deletion or unstaged rename is committed, the old missing path is still enumerated; afterward it disappears. The cited `test-preflight` precedent explicitly filters nonexistent paths (`src/harness/test-preflight.sh:163-174`). The identity must define NUL-safe existing-entry/absence normalization and prove that a dirty deletion or rename followed by committing exactly that effective tree preserves identity, separately from the already listed tests that reject an intervening deletion or rename.

PLANNER'S RESPONSE ON P02M0177 (2026-09-04T20:52:10Z):

Two findings, BOTH ACCEPTED. Each is the same class of fault as the round before it - a rule stated
where a structure was needed - and this round the structure is available in both cases.

1. **M1 and M2 describe incompatible seams, and the mutation gate proves placement rather than use**
   - ACCEPTED, both halves.

   The contradiction is real and it is mine: M1 says the reducer includes "the state transition and
   the effects", M2 says it is the admission and the arms carry the effects. Two descriptions of one
   boundary are satisfiable by host-testing a predicate and leaving the transition to the test, which
   is precisely the arrangement M1 was written to forbid.

   PLAN CHANGE. The seam is stated ONCE, in M1, as DECISION / EFFECT: the reducer decides admission,
   the resulting state, and the `FailureCause`; the arms carry publication, printing, supervision,
   teardown and spawning. This costs nothing to arrange - `BindingState`, `BindingEvent`,
   `FailureCause` and the transition table are ALREADY in `driver-binding`, so the seam moves no type
   and adds no dependency. The effects stay in `advance` because they are syscalls and catalogue
   writes, and hoisting them into a host-testable shell is a redesign of the service. M1's regression
   is restated against that seam: `READY` then the stale `TimedOut` THROUGH the reducer, asserting
   the record is still `Online` with no cause - a transition, not a predicate reading. M2 now refers
   to that reducer instead of describing a second one.

   The second half is the sharper of the two and it is correct. `let _ = reduce(..)` above the old
   `match event` keeps every textual property I asked for - before the match, not inside an arm, no
   arm-local predicate - and the stale `TimedOut` walks into its teardown arm exactly as before. A
   check that a call APPEARS is not a check that its answer is obeyed, and I had written the former
   while claiming the latter.

   PLAN CHANGE. The reducer's result is made UNIGNORABLE rather than merely required: the popped
   event is consumed by the reducer and the `match` is performed over what the reducer RETURNS, with
   the admitted event living only inside the admitted variant. Ignoring the result then leaves no
   `event` to match on and does not compile; inverting it asks the refusing variant - which carries
   no event - to produce one, and does not compile either. Both dangerous mutations become
   unrepresentable instead of forbidden.

   What stays unenforced is that `advance` calls the reducer at all: deleting the dispatch and
   matching the popped event directly compiles cleanly and merely leaves the reducer unreferenced -
   which last round's answer wrongly believed was a build failure, and the plan now says so in the
   same place. That residual is the source check's job, which gained a third assertion (the `match`
   scrutinee is the binding the dispatch produced) and a fourth mutant (the result bound to `_`).

2. **The working-tree digest is not bound to the proposed revision, and the enumeration is not
   commit-stable for deletions and renames** - ACCEPTED, both halves, and the first is a hole I
   opened last round.

   The A/B walkthrough holds exactly as written: `HEAD` at O, index at A, worktree at B, both tiers
   hash and test B, the commit records A, and merge attests A on evidence produced against B. My own
   acceptance case - "an index/working-tree disagreement is accepted" - is what let it through.
   Ignoring the index correctly identifies the bytes a run CONSUMED, and that is all it does; it
   never claimed anything about the revision those bytes are proposed as, and I treated it as though
   it had.

   PLAN CHANGE. Merge takes the proposed revision as well as the handoff and computes THE SAME
   canonical digest over that commit's tree (`git ls-tree -r -z`, same fields), requiring all three
   to agree: the inner snapshot, merge's own snapshot, and the commit's tree. An ordinary commit of
   the tested bytes passes; the A/B partial stage is refused with the disagreement named. The test
   list now carries the index case on BOTH sides, because what decides it is not the disagreement but
   which bytes the commit ends up holding - which is the distinction the previous round missed.

   The second half is correct too. `git ls-files -c` lists cached paths already gone from disk, so
   "every enumerated path contributes its bytes" is undefined for a dirty deletion, and after the
   deletion is committed the path stops being listed - the identity moving across a commit of exactly
   the tested tree, which is the one thing it must not do. An unstaged rename is that twice. The
   `test-preflight` precedent I cited does filter nonexistent paths, and I cited it without carrying
   that part across.

   PLAN CHANGE. A path ABSENT FROM DISK CONTRIBUTES NOTHING - it is not in the effective tree, which
   is the subject - so a dirty deletion and a dirty rename digest the same before and after the commit
   that records them, and both are now named acceptance cases distinct from the intervening-deletion
   and intervening-rename refusals. The rest of the normalisation is spelled out with them: `-z`
   throughout, entries deduplicated because a conflicted path is listed once per stage, sorted by path
   bytes, and each entry reduced to path, kind, and either the executable bit or a symlink target -
   the mode domain a git tree records, which is what makes the disk side and the commit side
   comparable at all. A gitlink is refused outright rather than hashed as though a commit id were
   content.

No source code was modified.
