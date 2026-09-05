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

AUDITOR'S RE-AUDIT OF PLAN P02M0177 (2026-09-04T20:56:31Z):

Rating: 7/10

1. **The reducer's concrete contract still does not carry the decisions M1 assigns to it, and M2 still gives the same seam incompatible inputs.** M1 now defines a pure reducer which, from the record state and one event, decides admission, the resulting state, and `FailureCause` (`docs/todo/P02M0177.md:103-123`). M2 still says that reducer takes clock and arbitration inputs (`:143-146`; DoD `:553-556`), but its concrete call is `reduce(node.record.state, node.pop()?)` and `Outcome::Act(event)` yields only the raw event before the production match (`:200-207`). No selected next state or cause reaches that match, despite the later bullets and DoD saying those are reducer outputs (`:227-233`, `:590-593`). An implementation can therefore satisfy the structural gate with the admission-only predicate seam M1 rejects while independently deriving transition and cause in the arms, or it must invent an unspecified second clock/arbitration seam. The plan must separate event generation/arbitration from reduction (or remove the obsolete clock-input requirement), define an admitted action payload containing the event, next state and cause, and require `advance` to consume those decisions without re-deriving them. The host regression and structural mutation must cover that complete decision payload, not only whether the raw event reaches the match.

2. **Merge still has neither a concrete proposed-revision input nor enough information to reconstruct and validate the plan produced for the dirty inner change.** The only defined invocation remains `./verify.sh --merge FILE` (`docs/todo/P02M0177.md:315-318`, `:557-558`), while the new correction says merge also takes “the proposed revision” without defining whether it is `HEAD`, another argument, or a handoff field (`:421-428`). More fundamentally, after the ordinary dirty change is committed, the default change reader sees a clean worktree and cannot reproduce the inner selection (`verify.sh:346-357`). The handoff carries a plan digest, inner per-key outcomes, and deferred keys, but not the inner base/canonical change set, a canonical full plan, or its prerequisite-closed step graph (`docs/todo/P02M0177.md:341-351`). A digest with no independently reproducible preimage cannot validate that the deferred list is complete or that it is the plan for the proposed commit.

   Comparing the inner, merge-worktree, and commit-tree source digests proves equal final bytes, not an equal change: two commits with different parents can have the same final tree while their diffs touch different components and require different `PlanItemKey`s. The current fields can therefore accept an old handoff and execute its plan for a same-tree commit whose actual selector plan is different, contradicting the promised changed-base refusal. The plan must define the revision carrier and bind the handoff to the selector input—either carry the inner base and canonical change set and regenerate the plan for the proposed revision, or serialize a canonical full key/step plan plus sufficient base context. Merge must validate the disjoint inner/deferred union, plan digest, current lowering and prerequisite closure before execution, with a clean dirty-to-commit reconstruction test and a same-final-tree/different-base refusal.

PLANNER'S RESPONSE ON P02M0177 (2026-09-04T21:14:52Z):

Two findings, BOTH ACCEPTED. Both are the same shape: a correction applied at one end of a seam and
not carried to the other, so the structure I built to prevent an arrangement could be satisfied BY
that arrangement.

1. **The reducer's concrete contract does not carry the decisions M1 assigns it, and M2 still gives
   the same seam incompatible inputs** - ACCEPTED, all three parts.

   The clock line is simply stale: I restated the seam in M1 as `(state, event) -> decision` and left
   M2 saying the reducer "takes the clock and the arbitration as inputs", which is a DIFFERENT seam
   that would have to be invented on top of the one M1 defines. Worse, it is unnecessary: generation
   and reduction are separate concerns - a deadline expiring in the central wait is what MAKES a
   `TimedOut`, and the reducer's subject is what one already-made event does to a binding. Ordering
   two events needs two events applied in an order, not a clock.

   PLAN CHANGE. The clock and arbitration requirement is REMOVED from M2 and from the Definition of
   Done, with the generation/reduction split stated where it used to sit. The determinism claim is
   unaffected and is if anything plainer: the test applies `READY` then the queued `TimedOut`, or the
   reverse, and gets that order every run.

   The sharper part is the sketch. `Outcome::Act(event)` hands the arms back exactly what they were
   given, so no next state and no cause ever reach the `match` - which is the admission-only seam M1
   rejects, reached THROUGH the structure built to prevent it. The finding is exactly right that an
   implementation could satisfy the structural gate and still derive the transition and the cause in
   the arms.

   PLAN CHANGE. The admitted variant now carries the WHOLE decision: the admitted event, the
   `BindingState` the record moves to (or none), and - when the attempt ends - the `FailureCause` and
   whether the ending is a planned stop rather than an incident. Every one of those is a pure
   function of the state and the event as `advance` computes them TODAY - `Stopped` to
   `FailureCause::Stopped`, `Exited`/`Closed` to `DriverExited`, `TimedOut` to `HandshakeTimeout`,
   `Failed { code }` to `DriverReported(code)`, and `planned_stop` is set by the `Stopped` arm from
   the event that produced it - so this moves the decision without changing it, which is what keeps
   it a seam rather than a redesign. `advance` CONSUMES those fields.

   The gate follows the payload: a fifth mutant is added - an arm RECOMPUTING a field the decision
   already carries, a `move_to` or a `FailureCause::` written in an arm - and the host regression now
   asserts the whole decision in both compositions rather than only that the event arrives.

2. **Merge has neither a concrete proposed-revision input nor enough information to reconstruct the
   plan** - ACCEPTED, and the second half is the one that mattered.

   The revision carrier was left undefined, and the finding is right that "the proposed revision"
   appearing in a correction is not a specification.

   PLAN CHANGE. The proposed revision is `HEAD`, stated as a CONSEQUENCE rather than a choice: merge
   must run over a working tree whose content is the proposed commit's, this tree's workflow never
   moves git state so there is no checkout and no materialisation, and the three-way agreement
   enforces it. A revision that is not `HEAD` would fail that agreement immediately, so a flag naming
   one would be a flag whose every non-default value is refused; and the handoff does not carry it,
   because a handoff naming its own revision is a stale handoff asserting which commit it is about.

   The reconstruction half is the real defect. `--for-change` reads the WORKING TREE and dies with
   "the working tree is clean" after the ordinary commit, so merge could not compute a plan to
   compare the carried digest against - the digest sat beside nothing, and the deferred set could not
   be shown to be complete.

   PLAN CHANGE. Merge REGENERATES the plan from the commit: `changes::range` over `parent(P)..P`,
   then `Model::plan`. That reconstructs exactly, and for a reason worth recording - `plan` is a
   function of PATHS ALONE, so the dirty run's `Untracked` and the commit's `Added` select
   identically. Merge then requires, before executing anything: the model hash matches, the
   regenerated plan's digest matches, the inner and deferred key sets are disjoint and their union is
   exactly that plan's keys, and the deferred set lowers under the current model to a
   prerequisite-closed set of steps. A commit with no parent or more than one is refused rather than
   given a first-parent convention nobody asked for.

   On the same-final-tree/different-base case: the finding is right that a tree digest alone does not
   separate them, and my blanket "a changed base is refused" was a verdict without a mechanism. The
   mechanism is the regenerated plan - if the bases differ at any path, reaching one final tree means
   the two deltas differ there, so their path sets differ and the digests refuse it. What survives
   both checks is the case where the path sets AGREE, and the plan now says plainly that this is not
   a hole: the same bytes are proposed and the same keys were selected, so the evidence covers
   exactly the system and the scope being proposed. The two tests the finding asks for are in the
   Definition of Done.

No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN P02M0177 (2026-09-04T21:48:04Z):

Rating: 7/10

1. **Merge still cannot reproducibly regenerate the dirty-run plan, because `Model::plan` is not a function of paths alone.** The new correction relies on `changes::range(parent(HEAD)..HEAD)` followed by `Model::plan`, and explicitly claims that the result reconstructs exactly because planning depends only on paths (`docs/todo/P02M0177.md:397-406`). In the current architecture, however, `Planner::for_model` loads `.build/state/verify-history.json` (`src/tools/verify-model/src/plan.rs:167-195`; `src/tools/verify-model/src/history.rs:87-101`), and the near-full cost rule uses that history to decide whether to append additional `PlanItem`s (`src/tools/verify-model/src/plan.rs:344-405`; `src/tools/verify-model/src/history.rs:465-497`). The inner run itself records completed steps back into the same history (`verify.sh:830-854`), while neither the proposed source identity nor the model hash freezes this input. Consequently, unchanged model and changed paths can produce different key membership and a different plan digest when merge regenerates the plan, causing a valid ordinary handoff to be refused. The plan must either make plan membership independent of mutable history, or bind a canonical snapshot/digest of every membership-affecting cost input into the handoff and use that frozen input for regeneration. Its acceptance tests must mutate history (including through inner-step recording) between inner and merge and prove that reconstruction remains stable.

2. **The accepted whole-decision reducer correction is still incomplete in the Definition of Done.** The implementation section now requires the source gate to fail against five mutants, including an arm that recomputes a carried `move_to` or `FailureCause` field (`docs/todo/P02M0177.md:278-287`). The Definition of Done still says there are exactly four mutated copies and omits that recomputation mutant (`:684-692`). That leaves the milestone's completion gate satisfiable without proving the newly required invariant that effect arms consume all reducer decisions rather than taking a second opinion. The Definition of Done must include the fifth mutant and explicitly require the returned transition, cause, and planned-stop decision to be consumed without arm-local recomputation.

PLANNER'S RESPONSE ON P02M0177 (2026-09-05T09:12:26Z):

Two findings, BOTH ACCEPTED. The first is a factual error of mine about the planner, and the fix is
not either of the two the finding offers - a third answer is smaller and safer than both, and the
response says why.

1. **`Model::plan` is not a function of paths alone, so merge cannot regenerate the inner plan
   exactly** - ACCEPTED. Verified in the code, not conceded: `Planner::for_model` calls
   `History::load` on `.build/state/verify-history.json`, and the cost escalation reads measured
   per-key durations through `CostModel::estimate` and APPENDS `PlanItem`s from the result. The inner
   run writes that same file after every step. So my claim was wrong, and worse than wrong in
   direction: demanding an equal plan would refuse the ordinary handoff BECAUSE OF A CACHE THE
   ORDINARY WORKFLOW ITSELF UPDATES - the failure would appear exactly in the case the mechanism
   exists to serve.

   What survives from that claim is the narrow part I actually needed and stated too widely: the
   change KIND does not enter selection, so the dirty run's `Untracked` and the commit's `Added`
   select identically. The plan now says that and nothing more.

   PLAN CHANGE - and NEITHER of the two remedies the finding names. Making membership independent of
   mutable history means removing the cost escalation, which is a rewrite of the selection model this
   milestone says in as many words it does not do. Freezing a history snapshot into the handoff fixes
   the symptom and buys two worse things: a cache that exists to be updated, carried as evidence, and
   a handoff whose stale copy could SHRINK the plan merge computes - a weakening, in a mechanism whose
   whole point is that nothing is quietly skipped.

   THE TEST IS CONTAINMENT RATHER THAN EQUALITY. Merge regenerates with the LIVE history and requires
   every key of that plan to be covered by the handoff's inner or deferred set; a key in neither is
   not a refusal but WORK - merge runs it, names it, and says it came from the widening rather than
   from the handoff. A key the handoff covers and the regenerated plan does not is the inner run
   having done more than was needed, which costs nothing. That is safe in the only direction that
   matters, robust to history drift by construction rather than by freezing anything, and it carries
   no new state. The carried plan digest stays in the handoff and is REPORTED beside the regenerated
   one when they differ; it is a diagnostic about a cache having moved and decides nothing.

   Two consequences were followed through rather than left to contradict each other. The
   same-final-tree/different-base case is now answered by containment too - a different delta puts
   keys in the regenerated plan that the handoff does not cover, and merge runs and names them, which
   is a better answer than a refusal because it says what is missing instead of only that something
   is. And the partition invariant is restated as being over the keys of ONE plan - the inner run's -
   so keys merge adds by regenerating are extra work rather than a violation of "exactly one of the
   two". The Definition of Done gained the history-mutation test the finding asks for, including
   mutation through the inner run's own step recording.

2. **The Definition of Done omits the fifth mutant** - ACCEPTED. The implementation section requires
   five and the Definition of Done still said four, so the completion gate could be satisfied without
   proving the invariant that had just been added. That is exactly the failure mode this milestone
   keeps finding in itself: a requirement stated in the prose and not in the thing that decides
   whether the work is done.

   PLAN CHANGE. The Definition of Done now names FIVE mutated copies including the recomputation
   mutant, and gained a separate line stating the invariant itself in the affirmative: the admitted
   variant carries the event, the state the record moves to, and for an ending attempt the
   `FailureCause` and whether the ending is a planned stop, and `advance` consumes all of it - no arm
   calls `move_to` with a state of its own choosing, writes a `FailureCause::`, or re-derives the
   planned-stop flag. The source check's assertions were extended to match.

No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN P02M0177 (2026-09-05T00:19:54Z):

Rating: 7/10

1. **The live-plan containment correction is still contradictory for plan contraction and its acceptance test covers only expansion.** The plan first requires the inner plan to be partitioned exactly into passed inner keys and an exact deferred set, with every deferred key still owed to merge (`docs/todo/P02M0177.md:333-339`, `:378-383`; DoD `:671-680`). Its concrete containment paragraph then says that *any* handoff-covered key absent from the regenerated plan means the inner run already did extra work (`:414-433`). That is true for a passed inner key, but false for a deferred key: if `P0 = I ⊎ D` is the inner plan and live history produces a smaller `P1`, every key in `D \ P1` is still unexecuted. The cost rule can cross its 0.9 threshold in either direction as mutable history changes (`src/tools/verify-model/src/plan.rs:344-405`), so this is a reachable case rather than wording about an impossible set. An implementation following the concrete paragraph can drop original deferred work while satisfying the new expansion-only test, which merely asks that "any widened keys" run and need not force even that membership change (`docs/todo/P02M0177.md:691-699`). The plan must state the merge work set unambiguously as `D ∪ (P1 \ (I ∪ D))`, plus prerequisites, and test deterministic history changes across the threshold in both directions, proving that `D \ P1` still runs.

2. **A second mutable, ignored planner input can still invalidate the model hash during the inner run itself, so the ordinary handoff is not reliable.** `Model::load_with_candidate` discovers kernel tests from compiled executables and builds the catalog from that discovery (`src/tools/verify-model/src/lib.rs:101-119`). Discovery reads descriptor-bearing binaries under `.build/cargo/kernel/<target>/debug/deps` and marks targets with no such binary missing (`src/tools/verify-model/src/kerneltests.rs:48-72`, `:110-135`); those discovered architecture variants enter the catalog (`src/tools/verify-model/src/catalog.rs:560-564`) and are hashed into `model_hash` (`src/tools/verify-model/src/lib.rs:126-173`). `.build/` is ignored (`.gitignore:13`), yet a missing binary is an explicitly supported fresh-checkout state that produces a whole-suite fallback (`src/tools/verify-model/src/plan.rs:316-324`, `:491-513`), and executing the inner x86_64 suite compiles its test binary into that same directory (`src/harness/test-kernel.sh:311-367`; `src/kernel/.cargo/config.toml:1-3`). The next merge-side model can therefore contain newly discovered variants and a different hash even though the effective source tree is identical. The plan nevertheless requires every model-hash mismatch to be refused before execution (`docs/todo/P02M0177.md:424-430`; DoD `:691-704`), making a fresh checkout—and similarly a stale discovery binary replaced after a test-inventory edit—reject its own valid handoff. The plan must stabilize/replan the discovery-dependent model before emitting the handoff, or define a fail-closed reconciliation for artifact-only discovery drift that runs newly required keys while retaining refusal for genuine source-model changes. Acceptance must cover missing and stale kernel-test binaries across inner-to-merge.

PLANNER'S RESPONSE ON P02M0177 (2026-09-05T10:31:08Z):

Two findings, BOTH ACCEPTED. They turn out to be the same defect met from two directions - a plan
regenerated at merge is not the plan the inner run made, and I twice wrote prose about that instead
of arithmetic - so the correction to the first is what answers the second.

1. **The containment prose is contradictory for contraction, and only expansion is tested** -
   ACCEPTED. The sentence "a key the handoff covers and the regenerated plan does not is the inner
   run having done more than was needed" is true of a key in `I`, WHICH RAN, and false of a key in
   `D`, which did not. And contraction is reachable, not theoretical: the cost rule crosses its 0.9
   threshold in either direction as history moves, so a shrinking cache could silently discharge work
   the inner run promised - the exact failure this whole mechanism exists to prevent, written into
   the paragraph that was meant to prevent it.

   PLAN CHANGE. The work set is now stated as ARITHMETIC rather than described:

       merge runs   D + (P1 - (I + D))   closed under prerequisites

   Every deferred key runs BECAUSE IT WAS DEFERRED - `D` is a promise, and a promise is not
   renegotiated by a cache - and every key the regeneration adds runs because nothing has answered
   for it. The wrong sentence is corrected in place with the reason it was wrong. The Definition of
   Done now moves the history file DELIBERATELY ACROSS THE THRESHOLD IN BOTH DIRECTIONS between inner
   and merge, including through the inner run's own step recording, and requires on the contracting
   side that every key of `D - P1` still runs.

2. **A second mutable input - kernel-test discovery - can move `model_hash` during the inner run
   itself** - ACCEPTED, and verified in the code rather than taken on trust. `Catalog::build` sets
   each kernel test's variants from `Discovery`, whose per-architecture presence comes from binaries
   under `.build/cargo/kernel/<triple>/debug/deps` - the comment there says "derived per target from
   the compiled test binaries" - and those variants are hashed into `model_hash`. `.build/` is
   ignored, a checkout that has built nothing is an explicitly supported state, and running the inner
   x86_64 suite WRITES that binary. So a rule refusing every hash mismatch refuses a valid handoff for
   having done its own work, on the ordinary first change after a checkout.

   I checked the rest of `model_hash` before choosing the fix: the selector source digest, the
   registry and configuration text, the graph, the feature definitions and the arch-risk scan are all
   source-derived, and among the catalog's checks only the kernel-test variant lists come from
   artefacts. So the artefact-derived term is exactly one, and it can be separated cleanly rather than
   worked around.

   PLAN CHANGE. The handoff gates on a SOURCE MODEL HASH - everything `model_hash` covers except the
   kernel-test variant lists. Test ids and their `covers` come from `scan_source` and stay in; only
   "which targets was a binary found for" comes out. `model_hash` itself is NOT changed: shadow
   records and the history are keyed by it and this milestone does not touch that, so the source hash
   is a second digest beside it, carried and gated on, with `model_hash` recorded for the record.

   A source-model difference stays a REFUSAL - the system being verified changed. An artefact-only
   difference is not, and needed no special case at all once the first finding was answered: the
   regenerated plan simply holds keys the handoff does not cover, and `D + (P1 - (I + D))` already
   runs and names them. The `missing` target discovered between inner and merge is that shape exactly
   - its whole-suite stand-in was deferred and still runs, and the newly discovered scoped keys run
   beside it. That is more work than a perfect oracle would order, in the safe direction, on a
   transition that happens once per checkout. Acceptance covers both the missing and the stale
   discovery binary across inner-to-merge.

No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN P02M0177 (2026-09-05T00:36:26Z):

Rating: 7/10

1. **The artefact-only reconciliation still fixes the merge work set before stale emulated-target inventories are refreshed, so its stale-binary case can pass without running a newly applicable test.** Kernel-test discovery creates per-target variants only from binaries already present when the model is loaded; a source declaration missing from those binaries is merely listed as `declared_not_built` and is not selectable (`src/tools/verify-model/src/kerneltests.rs:48-72`, `:90-107`). Thus a stale-but-present aarch64 or riscv64 binary is considered enumerated while a newer test can be absent from `P1`. Lowering then fixes either an exact old-ID selection or the smoke fallback (`src/tools/verify-model/src/commands.rs:301-343`). The merge step does compile a current test binary (`src/harness/test-kernel.sh:311-365`), but the guest subsequently runs only those precomputed IDs or the smoke tag (`src/kernel/tests.rs:2551-2595`); there is no post-build discovery and replan. The source-model hash correctly remains stable, but `D + (P1 - (I + D))` cannot add a key that was absent from the pre-execution `P1`. Consequently the claim that artefact drift needs no special handling (`docs/todo/P02M0177.md:475-484`) and the stale-binary acceptance item (`:747-751`) are incomplete. The plan must ensure final selection uses target inventories built from the proposed source, re-reconcile after an inventory-producing build, or conservatively run an unfiltered suite for a stale target. The stale acceptance case must prove that a newly discovered test actually runs on every applicable deferred target, not merely that the handoff is accepted.

2. **The zero-exit contract can bypass the revision-side regeneration on which changed-base safety depends.** The default inner run may exit zero and declare that nothing further is owed whenever its original plan has no merge keys (`docs/todo/P02M0177.md:357-364`; DoD `:723-724`). Yet the plan handles a same-final-tree commit over a different parent only by invoking merge, regenerating `P1` from `parent(HEAD)..HEAD`, and running keys added by the different delta (`:398-448`, `:578-591`; DoD `:737-746`). An empty deferred set is reachable, including for the explicitly noted non-code case (`:629-630`; `src/tools/verify-model/src/plan.rs:516-520`). If such a dirty run returns zero and the same final tree is then proposed over a parent whose delta selects additional source paths, no merge is owed or invoked, so the additional `P1` keys are never discovered. This also contradicts the statement that merge is owed by whoever proposes the change at the proposed revision (`docs/todo/P02M0177.md:614-619`). A dirty inner result cannot be final solely because its `D` was empty: the plan must require revision-bound reconciliation even when that reconciliation ultimately has no work, or otherwise bind and validate the selector's base/delta before allowing zero. The acceptance suite needs the same-final-tree/different-base case with an initially empty `D`.

PLANNER'S RESPONSE ON P02M0177 (2026-09-05T11:26:44Z):

Two findings, BOTH ACCEPTED. Both are cases where I proved an arithmetic sound and then failed to
ask what it was computed FROM, or WHEN.

1. **The stale-binary case cannot be answered by the work set, because the key does not exist yet** -
   ACCEPTED. Checked in the code: `discover` builds `tests` from the binary's symbols alone, so a
   test declared in the source and absent from a target's binary produces no variant, no catalog
   check and no key. `declared_not_built` does not catch it either - it is global, and in this case
   the test IS in the freshly built x86_64 binary, so nothing is listed. The target is therefore not
   `missing`, it enumerates, and it enumerates the tests it was built with. `P1` cannot contain the
   new test's key for that target, so `D + (P1 - (I + D))` cannot add it. Merge then builds the
   current binary and hands the guest `TEST_SELECTION=` computed before that build. The new test does
   not run on that target and nothing says so.

   So my claim that artefact drift needs no special handling was right for the MISSING binary and
   wrong for the STALE one, and the plan now separates them instead of asserting one covers both. The
   arithmetic was sound; its input was stale.

   PLAN CHANGE. The selection is RECONCILED ONCE MORE, AFTER THE BUILDS. A target's kernel build is
   already a prerequisite of its guest step, so there is a point in the run where the inventory is
   current and nothing that consumes it has run - at that point merge re-derives discovery,
   regenerates `P1'`, and extends the work set by `P1' - (I + D + P1)` closed under prerequisites. It
   can only add, and it needs no new concept: it is the same regeneration the plan already does, at
   the moment its artefact input stopped being stale.

   I rejected the third remedy the finding offers - running an unfiltered suite for a stale target -
   because the keys would then not match the run. `commands.rs` carries a comment about exactly that
   defect being fixed once already: the plan said 195 keys, the run did 205, and ten tests ran
   unrecorded. Reconciling adds REAL keys, so the step that runs them records them.

   The pass is bounded: ONCE, not to a fixpoint. The test binaries are the only artefact input to the
   model, and after their builds they describe the proposed source. And acting on it is safe for the
   reason the previous round established - it is exactly the artefact-derived term the source model
   hash excludes, so a reconciliation that changes the selection is never one that changed the system.
   The acceptance case now requires the new test to RUN on every applicable deferred target and be
   recorded against its own key, rather than only that the handoff was accepted.

2. **A zero exit can skip the revision-side regeneration entirely** - ACCEPTED. An empty deferred set
   is reachable - a host-only change, and the non-code case most plainly - and I had written that
   such a run needs nothing further. Then nothing ever regenerates the plan at the proposed revision,
   and the commit's own delta against its own parent is where a rebase, an amend and a partial stage
   all show up. The plan answers all three at merge and I left a path that skips merge, which also
   contradicts its own row saying merge is owed by whoever proposes the change.

   PLAN CHANGE. What an inner zero CLAIMS is narrowed to what the inner run can see: everything this
   WORKING TREE's change selected ran and passed, and nothing is deferred. It does not say a commit is
   verified, because the inner run never saw one. Attesting a revision is merge's job always, `D`
   empty or not - and with `D` empty the work set is `P1 - I`, usually empty, so it costs a
   regeneration rather than a run. The cheap case stays cheap; what it stops being is silent. The
   Definition of Done says this in both places it was stated, and the same-final-tree/different-base
   case is now run TWICE - once with a non-empty deferred set and once with an EMPTY one, which is
   precisely when a skipped merge would have hidden it.

No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN P02M0177 (2026-09-05T01:07:18Z):

Rating: 6/10

1. **The claimed post-build reconciliation point does not exist in the current execution graph, so
   `P1'` would still be derived from the stale inventory.** The plan says the target's existing kernel
   build prerequisite makes the test inventory current before the guest consumes it
   (`docs/todo/P02M0177.md:506-521`). That prerequisite is an ordinary
   `./build.sh --arch ... --part ...` product build (`src/tools/verify-model/src/commands.rs:187-205`),
   and its kernel part runs `cargo build`, not a test build (`build.sh:107-113`). Discovery explicitly
   skips that descriptor-free ordinary kernel (`src/tools/verify-model/src/kerneltests.rs:54-63`). The
   descriptor-bearing `cargo build --tests` instead occurs inside `test-kernel.sh` in the same guest
   `Step`, immediately before QEMU is invoked (`src/harness/test-kernel.sh:311-400`). There is therefore
   no existing boundary at which all selected target inventories are current while no guest has run.
   Moreover, the current executor materializes one static steps file and fixes budget affordability
   before executing its first command (`verify.sh:630-650`, `:675-783`, `:897-924`), so a mid-run
   extension is not covered by the existing scheduling or budget contract. As written, the correction
   either rescans the same stale binaries or requires an unspecified restructuring, and the stale-test
   false green remains. The plan must define an actual descriptor-inventory preparation phase and
   barrier before `P1'`, then specify how the refreshed plan is re-lowered and included in final step,
   prerequisite, outcome, and budget accounting. The DoD must exercise that real phase rather than
   assuming the ordinary kernel prerequisite produced the inventory (`docs/todo/P02M0177.md:797-801`).

2. **The addition-only `P1'` rule cannot reconcile stale inventory contraction.** Per-target
   applicability deliberately comes from compiled symbols because source scanning cannot derive the
   scattered `cfg(target_arch)` conditions (`src/tools/verify-model/src/kerneltests.rs:1-8`,
   `:218-225`). A stale aarch64 binary can therefore expose test key `K` and place it in `D`/`P1`,
   while the proposed source has removed only that target applicability and retains the same test ID
   and `covers` elsewhere, so the source-model hash still agrees. Once a current test binary is built,
   `P1'` correctly omits `K`; however, the plan only adds `P1' - (I + D + P1)` and still requires every
   old `D` key to run (`docs/todo/P02M0177.md:431-450`, `:506-518`). The refreshed catalog can no longer
   lower that target key, while executing the pre-refresh exact selection hard-fails it as an unknown
   ID (`src/kernel/tests.rs:2562-2568`). Thus a valid artifact-only applicability contraction cannot
   complete, contradicting the promise that artifact-only drift is accepted
   (`docs/todo/P02M0177.md:488-494`; DoD `:793-801`). The plan must distinguish history contraction,
   where `D` remains a valid promise, from refreshed-inventory evidence that an artifact-derived target
   variant does not exist,
   and define how the latter is retired or otherwise discharged. Acceptance needs both inventory
   expansion and contraction cases.

3. **The revised zero-exit contract still exposes ordinary shell success before the mandatory
   revision attestation.** The plan itself uses `if ./verify.sh; then publish; fi` to explain why an
   incomplete inner result must not return zero (`docs/todo/P02M0177.md:357-362`), but it now permits
   zero when `D` is empty while expressly saying merge remains owed and is the only tier that attests
   the proposed revision (`:364-377`; DoD `:764-767`). A shell caller observes status zero, not the
   printed qualification that only the working tree was checked, so it can publish before the
   changed-base, partial-stage, and regenerated-`P1` checks run. This is the same machine-readable
   ambiguity the current runner avoids by assigning non-zero statuses to incomplete or unproven
   results (`verify.sh:1039-1045`, `:1055-1065`). Every dirty-tree inner result that still requires
   merge must remain non-success/incomplete even when `D` is empty, or the default workflow must
   perform the revision-bound finalization before returning zero.

PLANNER'S RESPONSE ON P02M0177 (2026-09-05T12:38:19Z):

Three findings, ALL THREE ACCEPTED. The first two are the same mistake as last round's, one level
down - I asserted a property of the execution graph without reading what the steps actually run - and
the third is my own argument used against my own exception, correctly.

1. **The post-build reconciliation point does not exist** - ACCEPTED, and verified rather than
   conceded. The guest step's prerequisite lowers to `./build.sh --arch X --part kernel`, whose
   kernel part is a plain `cargo build`. That produces an ORDINARY kernel with no test descriptors,
   and discovery skips exactly such a binary by design - the `names.is_empty()` guard, which exists
   because the ordinary kernel shares the `deps/` directory and shape. The descriptor-bearing binary
   comes from `cargo build --tests` INSIDE `test-kernel.sh`, in the same step as the guest it feeds
   and moments before QEMU. So a reconciliation placed at that "boundary" would have rescanned the
   same stale binaries and changed nothing, and the stale-test false green would have survived the
   correction meant to remove it.

   The executor point is right too: one steps file is materialised and budget affordability settled
   before the first command, so a work set that grows mid-run is outside that contract, and
   rewriting the scheduler is not what this milestone is for.

   PLAN CHANGE. The phase moves BEFORE THE LOWERING instead of into the run. Merge's order is now
   stated explicitly: regenerate `P1` and check what the earlier rows check; determine the touched
   targets; BUILD THE TEST INVENTORY for each from the proposed source, through a build-only harness
   entry the plan now owes because `test-kernel.sh` today builds and boots in one breath; re-derive
   discovery; regenerate as `P1'` and reconcile; then lower ONCE, price the budget over the final
   set, and execute. The executor's contract is untouched - one steps file, one budget, one lowering,
   simply computed after the artefact input has stopped being stale.

   The cost is stated rather than hidden: one `cargo build --tests` per touched target ahead of the
   one `test-kernel.sh` does anyway, and because the selection is baked in at compile time through
   `option_env!`, the later narrowed build is a second compile rather than a cache hit. Tens of
   seconds per target in a tier whose subject is tens of minutes. The Definition of Done now
   exercises the phase through its own entry point rather than assuming the ordinary kernel
   prerequisite produced an inventory.

2. **Addition-only reconciliation cannot handle inventory contraction** - ACCEPTED. Per-target
   applicability comes from compiled symbols precisely BECAUSE the `cfg(target_arch)` conditions
   cannot be read from source, so a stale binary can put a key in `D` for a test the proposed source
   no longer builds on that target, with the id and `covers` unchanged and the source model hash
   therefore agreeing. After the inventory phase the refreshed catalog cannot lower that key, and
   handing the pre-refresh selection to the guest hard-fails on an unknown id - by design. My rule
   would have turned a valid change into an unfinishable one, which is the opposite failure from the
   one it was written to prevent, in the same paragraph.

   PLAN CHANGE. The two contractions are told apart BY WHAT SAYS THE KEY IS GONE. A cost cache saying
   a key is no longer worth taking does not make the key untrue, and `D` still runs it - last round's
   correction stands unchanged. A BUILD OF THE PROPOSED SOURCE saying the test does not exist on that
   target is evidence about the system, not about a cache, and that key is RETIRED: named in the
   output with its reason, counted as retired rather than passed, never dropped silently. Retirement
   is permitted only for a key the refreshed catalog cannot lower and only while the source model
   hash agrees - a source-model difference was refused long before this point. Acceptance now covers
   expansion and contraction, with a cost-cache contraction in the same run still running every key
   of `D - P1`, so the two are proven distinct rather than merged.

3. **A zero exit still exposes shell success before the attestation** - ACCEPTED, and the finding is
   this plan's own argument turned on the exception I carved out of it. The paragraph explains that a
   deferred run must exit non-zero because `if ./verify.sh; then publish; fi` reads the STATUS and not
   the note - and I then permitted zero with an empty `D` and put the qualification in a note. A
   caller would publish before the changed-base, partial-stage and regenerated-`P1` checks had run.

   PLAN CHANGE. The default mode does NOT return zero while merge is owed, and merge is owed for every
   dirty run. It reports `INCOMPLETE` and exits 6 - the status this runner already has, with the
   sentence it already prints, "this run verified part of what the change needs" - and names its
   passed share and what remains. Whether `D` was empty changes what merge has to DO, not whether it
   is owed. Zero is reserved for a merge run that completed and for the flat release paths outside the
   tiers. The Definition of Done drives `if ./verify.sh; then publish; fi` over a PASSING inner run
   with an EMPTY deferred set and requires that it does not publish.

No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN P02M0177 (2026-09-05T01:33:48Z):

Rating: 6/10

1. **The pre-lowering inventory build still bypasses the sole budget, and its stated integration
   point does not match the current harness.** The new order performs one `cargo build --tests` per
   touched target and only afterwards lowers the final plan and prices `--budget`
   (`docs/todo/P02M0177.md:183-187`; DoD `:250`). Those builds are therefore outside the
   prerequisite-closed work whose affordability is decided before anything starts. That contradicts
   the existing budget contract, which specifically prevents a small budget from spending substantial
   time building and then declining to test (`verify.sh:679-689`, `:775-783`); the fresh-checkout case
   also makes the plan's unconditional “tens of seconds” assumption unsafe
   (`docs/todo/P02M0177.md:13-18`, `:249`). The planner's supporting claim is factually stale as well:
   `test-kernel.sh` already has a compile-only `--build-only` path
   (`src/harness/test-kernel.sh:5-14`, `:147-174`, `:464-471`), and `test.sh` already exposes it
   (`test.sh:14-37`, `:111-114`, `:288-300`). The top-level route is not usable as the proposed
   pre-prerequisite fresh-checkout phase because it first requires current system-volume artifacts
   (`test.sh:221-279`). The plan must name and reuse the direct inventory-build seam rather than add a
   duplicate, and must reserve/charge that work before it starts or explicitly reject budgeting for
   merge. The DoD needs a fresh target with an insufficient budget and must prove that no inventory
   build begins.

2. **Retirement is still incompatible with the plan's completion and lowering invariants.** A retired
   key is explicitly counted as retired rather than passed, yet merge is still allowed to complete
   while the general invariant says both shares must be present and passed
   (`docs/todo/P02M0177.md:161`, `:171-173`; DoD `:244`, `:247`, `:251`). The same paragraph both
   permits a refreshed-inventory key that cannot lower and requires every key in the provisional work
   set to lower (`:173`). It also motivates and tests only a disappearing key from `D`, although an
   artifact-derived key first added by stale `P1 - (I + D)` can disappear from `P1'` in the same way.
   An implementation following the pass invariant either cannot finish the required contraction case
   or must misreport retirement as a pass; one following only the `D` example can retain an
   unlowerable provisional `P1` key. The plan must define final per-key accounting over the entire
   provisional merge work set: current keys lower and pass, while source-bound unavailable variants
   retire as a distinct successful discharge. Its completion invariant and DoD must say that directly.

3. **The new preparation/execution window is not covered by the effective-source identity that merge
   attests.** The stated order performs the handoff/tree/model checks before inventory preparation,
   then builds and executes from mutable working-tree bytes without requiring the effective-tree
   identity to be checked again (`docs/todo/P02M0177.md:185-201`, DoD `:245-246`, `:253`). An ordinary
   source edit during that window can leave the narrower source-model hash unchanged, while the
   refreshed binary or later steps consume bytes different from `HEAD`; merge can then claim that
   `HEAD` completed. This tree already handles the corresponding persistent-movement fault in the
   shadow path by comparing source identities before and after work (`verify.sh:394-410`, `:617-623`).
   Without duplicating P02M0170's stronger immutable-evidence scope, this plan's own minimum identity
   contract must at least revalidate the effective-tree/source-model identity after inventory
   preparation and after execution, and refuse completion if it moved.

PLANNER'S RESPONSE ON P02M0177 (2026-09-05T13:52:07Z):

Three findings, ALL THREE ACCEPTED. The first contains a plain factual error of mine - I said a
harness entry had to be added that has existed all along - and the other two are invariants I stated
in one paragraph and contradicted in the next.

1. **The inventory phase bypasses the budget, and its integration point is stale** - ACCEPTED, both
   halves.

   The stale half first, because it is the least defensible: I wrote that "the harness gains a
   build-only entry, since `test-kernel.sh` today builds and boots in one breath". `test-kernel.sh`
   HAS `--build-only` - it is in its usage line - and `test.sh` exposes it. I asserted a gap without
   reading the script I was asserting it about.

   PLAN CHANGE. The phase invokes `src/harness/test-kernel.sh <arch> --build-only` DIRECTLY, and the
   plan now says why it must be the direct seam rather than the top-level one: `test.sh` first demands
   a current system volume and a matching build stamp, which is right for a suite that boots and
   impossible for a phase whose purpose is to run on a checkout that has built nothing. Nothing is
   added to the harness.

   The budget half is the same fault this milestone keeps finding in itself, and the contract even
   names it: `budget_select` costs whole prerequisite-closed branches because putting the builds
   outside it makes `--budget 10` "a run that spends forty minutes building and then declines to
   test". My phase compiles before the lowering, which is exactly outside it.

   PLAN CHANGE. MERGE REFUSES `--budget`. That costs nothing, because a budgeted merge cannot produce
   a merge run's only useful outcome: merge completes or it does not, and a partial merge is an
   `INCOMPLETE` the inner run already reports more cheaply. Refusing up front is also the honest
   answer on a fresh checkout - nothing is built and the run says why, instead of spending a cold
   build to discover it cannot finish. `--budget` is untouched for the inner tier, where no inventory
   phase exists. The Definition of Done drives exactly the case the finding asks for: fresh checkout,
   no test binary, budget far too small, and NO inventory build begins.

   The cost claim was wrong too and is corrected rather than restated. "Tens of seconds per target"
   read as a claim about the cold case, where a cold aarch64 build is 15-20 minutes. On a cold tree
   the phase is not new work - it is the SAME build the guest step would have done, moved earlier. The
   genuinely added cost is the second compile, because `TEST_SELECTION` is baked in through
   `option_env!`, and that one is incremental. The plan now says both, and says which is which. It
   also now states the division the phase implies: the inner loop plans from whatever binaries are in
   `.build` and stays fast, merge refreshes every touched target including x86_64, and nothing is
   attested on an inner answer.

2. **Retirement contradicts the completion and lowering invariants** - ACCEPTED, and the finding
   catches a third contradiction I had not seen: retirement was motivated only for a key from `D`,
   while a key added by `P1 - (I + D)` from the same stale inventory disappears from `P1'` in exactly
   the same way. Reading the pass invariant literally, an implementation could not finish a valid
   contraction without reporting a retirement as a pass.

   PLAN CHANGE - order first, then vocabulary. RETIREMENT HAPPENS IN THE RECONCILIATION, BEFORE THE
   LOWERING: the provisional work set is `D + (P1 - (I + D))`, reconciliation removes every key the
   refreshed catalog cannot lower - from either half - and adds `P1' - (I + D + P1)`, and it is the
   RESULT that must lower prerequisite-closed. Nothing unlowerable ever reaches the lowering, so the
   two requirements stop contradicting each other instead of being reconciled by an exception.

   Then the accounting: every key of the provisional set ends in EXACTLY ONE state - passed, failed,
   or retired - and DISCHARGED means passed or retired. Completion is stated over discharge rather
   than over passing, because a retired key is a key the proposed source says does not exist, which is
   a real answer rather than a missing one. The output names each retirement with its reason and its
   count, so "discharged" can never quietly mean "not run". Both statements of the completion
   invariant - the handoff row and the Definition of Done - now say this, and the contraction case is
   proven TWICE, once from `D` and once from the `P1` remainder.

3. **The preparation and execution window is outside the identity merge attests** - ACCEPTED. The
   checks run once at the start; the inventory build and every step after it read the working tree,
   which is mutable and belongs to somebody who may still be working in it. An edit to a `.rs` file
   changes what is built and tested and leaves the SOURCE model hash untouched - that hash covers the
   selector's own sources, the registry, the graph, the features and the arch-risk scan, not the
   kernel file just saved - so merge could attest `HEAD` over bytes that were never `HEAD`.

   PLAN CHANGE. Merge recomputes the effective-tree identity AFTER the inventory phase and AFTER the
   last step, and refuses completion if it moved, naming both digests. This is not a new mechanism:
   the shadow path already digests the tree before and after and voids the comparison if they differ,
   and its comment makes this exact argument - pinning to the model hash "does not close the race"
   because an ordinary edit "changes what the sweep is testing and leaves the model's identity
   untouched". A tree digest either side of the work costs nothing against the work. The Definition of
   Done edits a `.rs` file mid-run and requires that merge does not report the revision as completed.
   This is the minimum this milestone needs and no more; the immutable evidence scope stays with the
   milestone this one orders itself before.

No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN P02M0177 (2026-09-05T03:15:07Z):

Rating: 6/10

1. **The selected inventory seam is not actually build-only, so the central pre-lowering refresh
   still cannot run as specified.** The plan and response now say that merge can invoke the existing
   `src/harness/test-kernel.sh <arch> --build-only` directly and that no harness change is needed
   (`docs/todo/P02M0177.md:191-203`; DoD `:268`). In the current script, that flag only changes the
   label and three-minute limit and appends `--no-run` to a `TEST_ARGS` array that is never consumed
   (`src/harness/test-kernel.sh:147-174`). After the locked `cargo build --tests`, execution proceeds
   unconditionally to `qemu-run.sh` (`:311-400`); `BUILD_ONLY` is not consulted again until the guest
   has exited or timed out (`:444-471`). The proposed inventory phase would therefore boot every
   touched target before lowering, and the emulated full suites would run under the three-minute
   “build-only” limit rather than return after compilation. On the fresh checkout the plan explicitly
   supports, this can additionally reach the guest path without the boot artifacts that `test.sh`'s
   skipped preflight normally requires. Merge cannot reliably reach discovery and reconciliation, so
   the latest correction is still based on a false current-code claim. The plan must include repairing
   this flag or using a genuinely compile-only seam, and its DoD must prove that inventory preparation
   produces discoverable descriptors without invoking QEMU or requiring system-volume artifacts.

2. **The changed-base acceptance contract remains internally contradictory.** The identity section
   explains that a same-final-tree proposal over a different parent is handled by regenerating `P1`,
   running any newly selected keys, and accepting the case when the path sets already agree; in the
   same paragraph it still blanket-lists “a changed base” among cases that are refused
   (`docs/todo/P02M0177.md:219`). The DoD likewise requires the same-final-tree/different-base case to
   have its additional keys found and run, both with non-empty and empty `D` (`:265-266`). Refusing
   solely because the base changed and completing after containment adds the uncovered work are
   mutually exclusive implementations. The stale blanket refusal must be removed or narrowed to the
   actual source-identity/work-coverage failures so the implementation and regression have one
   verdict.

3. **The source-stability correction brackets only merge, leaving the carried inner evidence without
   the same minimum binding.** Merge trusts the handoff's per-key inner outcomes and source snapshot
   (`docs/todo/P02M0177.md:161`, DoD `:262-264`), but the new after-work checks are explicitly only
   for merge's inventory phase and merge's final step (`:205-207`; DoD `:273`). The plan neither fixes
   the inner snapshot at the point before planning/execution nor requires it to be recomputed before
   emitting the handoff. A conforming implementation could therefore label outcomes with an identity
   captured after the tree moved, or carry the initial identity despite work having consumed a
   persistently changed tree that is restored before merge; merge's own checks cannot reconstruct what
   the inner steps read. This is the same ordinary-movement fault the plan now closes for merge, not
   P02M0170's stronger immutable-artifact problem. The inner tier must take the effective-tree identity
   before it plans/runs, revalidate it after its last step, and refuse to emit a usable handoff if it
   changed; the DoD's mid-run source-edit regression must cover inner as well as merge.

PLANNER'S RESPONSE ON P02M0177 (2026-09-05T14:41:26Z):

Three findings, ALL THREE ACCEPTED. The first is the second wrong thing I have said about one script
in two rounds, and I have stopped guessing about it and read it.

1. **`--build-only` is not build-only, so the inventory phase cannot run as specified** - ACCEPTED,
   and verified line by line this time. The flag sets `MODE` and a three-minute `DEFAULT_TIMEOUT` and
   appends `--no-run` to a `TEST_ARGS` array that IS NEVER READ AGAIN - the build is a
   `cargo build --tests` under the lock, and execution then falls straight through to `qemu-run.sh`.
   `BUILD_ONLY` is consulted after that only to word the result. So `--build-only` today builds,
   BOOTS, and does it under a timeout sized for a compile - and on a fresh checkout it would reach
   the guest path without the artefacts `test.sh`'s preflight normally insists on, because invoking
   the script directly skips that preflight.

   Two rounds ago I said the script "builds and boots in one breath" and owed a new entry point; last
   round I said the flag already exists and nothing is owed. The first was right about the behaviour
   and wrong about the cause; the second was right about the flag existing and wrong about what it
   does. Neither was read.

   PLAN CHANGE. THIS MILESTONE OWNS THE REPAIR, because the phase cannot exist without it and the
   flag is already documented in the script's usage and in `test.sh`. The fix is small and local:
   after the locked build and its staging copy, a build-only run REPORTS AND EXITS before the guest
   is started, and the dead `TEST_ARGS` array goes with it - the `cargo test` invocation it was
   written for no longer exists. What that leaves is a compile needing no system volume, no medium
   and no QEMU, which is exactly what discovery needs. The Definition of Done proves it directly: run
   on a checkout with NO system volume and no medium, require a descriptor-bearing binary that `nm`
   can read, and require that NO QEMU process is started.

2. **The changed-base contract is internally contradictory** - ACCEPTED. The paragraph explains at
   length that a base change is answered by the regenerated plan and by containment, and then lists
   "a changed base" flatly among the refusals. Two verdicts for one case, and an implementation could
   only pick one - and the Definition of Done had already picked the other, requiring the
   same-final-tree case to have its keys found and run.

   PLAN CHANGE. The refusal is restated as what it actually is: a TREE MISMATCH. A rebase whose
   result differs from the tested snapshot is refused because the snapshot is gone - which is the
   ordinary outcome and is already covered by "a changed path". A base change that yields the SAME
   tree is not refused at all; the regenerated plan holds whatever keys the different delta selects
   and containment runs and names them. Both places that state the verdict now say this, so there is
   one verdict per case and the regression asserts that one.

   While correcting that paragraph I also repaired a run-on it had carried since before the
   reformatting: a sentence ended without a full stop and the reflow joined it to the next.

3. **The source-stability bracket covers merge but not the inner tier** - ACCEPTED, and it is the
   same argument I accepted last round, applied one tier over. Merge TRUSTS the handoff's per-key
   outcomes and its source snapshot; if the inner run labels those outcomes with an identity taken
   before the tree moved, or runs its steps over bytes that are later restored, merge cannot
   reconstruct what those steps read and its own brackets prove nothing about them.

   PLAN CHANGE. The inner tier takes the effective-tree identity BEFORE it plans, revalidates it
   after its last step, and REFUSES TO EMIT A USABLE HANDOFF if it moved - the same rule applied to
   the tier that produces the evidence rather than only to the one that consumes it. The Definition
   of Done drives the mid-run source-edit regression against the inner tier AND against merge, and
   neither may report its work as complete. This stays the ordinary-movement minimum; the immutable
   artefact scope remains with the milestone this one orders itself before.

No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN P02M0177 (2026-09-05T03:34:00Z):

Rating: 7/10

1. **Merge's new stability bracket still does not bind the proposed revision for the duration of the
   run.** Merge regenerates `P1` from the `HEAD` present at its initial checks, but the checks after
   inventory preparation and after the last step compare only the commit-stable effective-tree
   identity (`docs/todo/P02M0177.md:191`, `:209-225`; DoD `:270-272`, `:280`). `HEAD` can move during
   that window to a commit with a different parent and the same final tree. The effective-tree and
   source-model hashes then remain equal, although `parent(HEAD)..HEAD` may name a different changed
   path set and select additional keys that were never included in `P1` or `P1'`. Merge can therefore
   return completion while the revision now being proposed has not undergone the very changed-base
   containment reconciliation the plan requires. The stale claim that the effective-tree digest
   itself changes when “the base” changes (`:217`) is also false for this same-tree case and conflicts
   with the corrected acceptance rule at `:225` and in the DoD. Merge must capture the proposed
   `HEAD` object ID, use that fixed revision for regeneration and commit-tree comparison, and require
   the ID to remain unchanged after inventory preparation and after execution. A regression must move
   `HEAD` mid-run to a different-parent, same-tree commit whose delta selects extra keys and require
   refusal rather than completion.

PLANNER'S RESPONSE ON P02M0177 (2026-09-05T15:19:44Z):

One finding, ACCEPTED. It is the exact complement of the bracket I added last round: I made the
CONTENT hold still and left the NAME free to move.

1. **The stability bracket does not bind the proposed revision** - ACCEPTED, and the mechanism is
   precisely the property the identity was designed to have. The effective-tree digest is
   COMMIT-STABLE - that is the whole reason it exists, so a dirty run and the commit of the same
   bytes agree - which means it is silent about WHICH COMMIT a tree belongs to. `HEAD` is a moving
   name and merge re-reads it implicitly. So a commit made during the run with a different parent and
   the same final tree leaves the effective-tree digest equal AND the source model hash equal, while
   `parent(HEAD)..HEAD` names a different delta that can select keys `P1` and `P1'` never held. Merge
   would complete over a reconciliation that never happened for the revision it was naming. Every
   guard I added was looking at the bytes, and none of them was looking at the label.

   PLAN CHANGE. Merge RESOLVES `HEAD` ONCE, at its first check, and works from that COMMIT OBJECT ID
   for the rest of the run: `parent(REV)..REV` is what `P1` and `P1'` are regenerated from, and
   `REV`'s tree is what the three-way agreement compares. The pinned id joins the identity in both
   brackets - after the inventory phase and after the last step - and a `HEAD` that no longer resolves
   to it REFUSES COMPLETION, naming both ids. One `git rev-parse`, and it is what turns "the proposed
   revision" from a lookup into a fact.

   The stale claim is removed too, and the finding is right that it was stale. The identity paragraph
   said the digest "differs whenever anything the run was made over differs, including the base". It
   does not: a commit-stable digest is stable across exactly the same-tree/different-parent case, and
   the sentence contradicted the acceptance rule further down in the same paragraph and in the
   Definition of Done. It now says what actually separates two commits sharing a tree - the
   regenerated plan and the pinned revision - and says that this digest is not it.

   The Definition of Done gained the regression the finding asks for: `HEAD` moved mid-run to a
   different-parent, same-tree commit whose delta selects extra keys, where EVERY DIGEST STILL AGREES
   and merge must refuse rather than complete. It sits beside the mid-run source-edit regression,
   which covers the other half - the bytes moving while the name holds still.

No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN P02M0177 (2026-09-05T09:51:10Z):

Rating: 10/10

No unresolved material issues remain. The current plan is ready for implementation without further
meaningful correction.


## 2026-09-05 readability and implementation-readiness revision

The user requested a shorter active specification, implementation-sized M3 steps, and a concrete
measurement scenario with cache conditions. The active milestone now lives in
[docs/todo/P02M0177.md](../../docs/todo/P02M0177.md). The exact pre-rewrite working copy is archived
below so previous corrections and their rationale remain available without mixing withdrawn
requirements into the active specification. Earlier audit ratings apply to their reviewed versions.

The source review identified the following additions to the active requirements:

- Derive the source model hash's kernel test identities and coverage directly from all source
  declarations. Removing only variant lists from the current catalog hash is insufficient: discovery
  adds whole catalog rows after the first build (kerneltests.rs:88-100, catalog.rs:562-563,
  verify-model/src/lib.rs:166-170). Verify stability before and after first compilation.
- Test startup ordering using the production manifest. The historical repair in ad3e28c7 added the
  three dependencies on storage_service; the scheduler already respected declared dependencies.
  A synthetic scheduler fixture alone cannot reproduce their omission.
- Cover shared producer outputs when enabling default guest concurrency. signed-boot builds the
  loader under a lock but copies it after releasing that lock (check-signed-boot.sh:585-589). A
  concurrent producer can replace it in between. This is a source-derived interleaving, not a
  reproduced execution; require stable acquisition and a regression.
- Define inventory targets from the whole relevant plan, including inner-owned x86_64 work, rather
  than only the deferred work. Include profile rows and concurrent-selection in the gate inventory;
  the eight-name GATES_THAT_BOOT_A_GUEST list explicitly excludes them.
- Describe the reducer's protection in terms of variable scope and verified dispatch wiring.
  BindingEvent is Copy, so passing it to a function alone does not consume an existing binding.

The historical 2694-second signed-boot measurement remains the baseline. The 14-second experiment
was rejected. The old ten-local-boot explanation is also not an authoritative inventory: source
review counts twelve local boots when the system-volume case is available, plus four port boots.
The active M4 requires an explicit case inventory instead of relying on that shorthand.

Only planning documents are changed in this revision. No implementation, performance measurement,
or new independent audit rating is claimed.

### Archived milestone before this rewrite

This is historical material, including superseded statements. The linked active milestone governs
implementation. SHA-256 of the exact archived milestone bytes: `d8cfdd0d563659c48b10bee61c6ca74e6a65a9bb2960d19674f5881b04040532`.

````markdown
# P02M0177 - A change is verified in proportion to itself, and integration bugs are catchable without a guest

Status: PLANNED.

## Goal

Make the cost of verifying a change proportional to the change, without verifying less.

This is not a request to run fewer checks. Every property the tree asserts today stays asserted; what changes is WHEN it is asserted, WHERE the evidence comes from, and how long a developer waits to find out. The measurements below are from one working day, 2026-09-04, and each one is a number rather than an impression.

## What it costs today, measured

    x86_64 boot-tag suite                        25 s
    aarch64 boot-tag suite                      198 s
    riscv64 boot-tag suite                      225 s
    cold aarch64 build                       15-20 min
    `signed-boot` gate                        2 694 s
    full `verify.sh --for-change` sweep           4.5 h

The sweep is 164 steps and it runs in full whenever a change touches a component that `selects_everything` - the harness, the kernel, the ABI. On 2026-09-04 a one-line edit to `qemu-run.sh` selected all 164 steps, and so did a comment in a planning document that happened to be in the same working tree.

AND THE SLOW PART IS NOT WHERE IT LOOKS. Of the sweep's measured time, 58% is gates that have nothing to do with any architecture, 39% is the two emulated targets, and 3% is x86_64. Inside that 58%, one gate - `signed-boot` - is 2 694 s, of which about 2 400 s is waiting for QEMU timeouts on boots whose answer has been in the serial log for seconds.

    the current measured baseline for `signed-boot`            2 694 s
    an experiment that was REJECTED and reverted                  14 s

THE 14 SECONDS IS NOT A BASELINE AND MUST NOT BE READ AS ONE (corrected 2026-09-04). An earlier version of this file reported it as "a 190x change that altered nothing about what it asserts". It altered something: the marker set behind that number stopped the altered-system-volume case at an intermediate `loader: kernel loaded` line, so the refusal that case exists to observe never reached the log and the gate reported that a tampered volume had not stopped the boot. The change was reverted and the gate is at 2 694 s today - `boot_medium` still carries its 120-second timeout and the port cases their 300-second ones.

What the 14 seconds established is the SIZE of the waste and the reason M4 cannot be written as one global rule; it is not evidence that the waste can be removed safely, and no number replaces it until the per-case predicates and their negative fixtures exist. M4's benefit and the closing comparison of this file are measured against 2 694 s.

## The three defects this milestone is really about

Three real bugs were found on 2026-09-04. What they have in common is the point:

  - a queued `TimedOut` event tore down a binding that had ALREADY answered `READY`, because `TimedOut` was the one terminal event with no "this handshake is over" guard. On x86_64 the answer always beat the deadline, so the race was never taken; on an emulated machine the two landed together. Two ports had been red for six days;
  - the probe list and the role hand-off walked block providers in DIFFERENT orders - publication order against bus order - which was harmless until something indexed one by the other;
  - the generated service `MANIFEST` is sorted by NAME, so a plan that said start order "is manifest order and nothing else" was wrong, and moving a block in the file changed nothing.

NONE OF THE THREE IS ABOUT HARDWARE. Every one is about ORDER and STATE - which event arrives first, which list is indexed by which, which service starts before which. They were found by booting a guest on the slowest target available, at roughly twenty minutes per attempt, because that was the only place they were visible. That is the shape this milestone exists to change.

## Items

- [ ] **M1 - each of the three defects above has a host regression, and each drives the seam that actually decided.**

  `driver-binding` already owns the rules a test can drive - `disable_action`, `decide_retry`, `cursor_after_an_attempt`, `shutdown_step` - and this is the same extraction one level up.

  ONE TEST PER DEFECT, AND THE FIRST DRAFT NAMED THE WRONG ONES (corrected 2026-09-04). It listed timeout/`READY`, `READY`/`FAILED` and planned-stop/shutdown - the last two are different lifecycle bugs, both real and both already repaired, and neither is one of the three defects this milestone is about. So the Definition of Done could have been met with the two order regressions still unprotected. The mapping is now explicit:

  - **the stale timeout** - apply `READY` first so the record reaches `Online`, THEN apply the previously generated `TimedOut` on the same generation, and require the binding to survive. AND ITS CONTROL, which is the opposite composition and must keep failing the attempt: `TimedOut` first, then a genuinely late `READY`. `driver-binding` already asserts the second (`a_ready_after_its_deadline_is_not_up`); the first is what was missing
  - **the two orders** - a helper that derives the probe list and the role hand-off from ONE set of provider identities, so a test can assert they name the same provider at the same index. The defect was that one walked publication order and the other bus order; a test that compares two orderings produced by the same function cannot catch it, and a test that compares two INDEPENDENT derivations can
  - **the start order** - a dependency-start scheduler driven with a name order deliberately opposed to the dependency order, asserting the dependency wins. The production loop reads `deps_satisfied` over a `MANIFEST` the generator sorts by NAME, which is the fact the plan itself got wrong

  AND A TEST OF THE PREDICATE IS NOT A TEST OF ITS USE. `BindingQueue` is already host-testable and `accepts_terminal_frame` already exists; the timeout defect was that the no-`std` `advance` path did not CONSULT the predicate. So the first regression must drive an event REDUCER, not the queue's FIFO behaviour and not the predicate alone - otherwise it stays green if production stops calling the guard, which is the exact omission.

  AND THE SEAM IS DECISION / EFFECT, STATED ONCE HERE AND OBEYED BY M2 (corrected 2026-09-04). This line used to say the reducer includes "the state transition and the effects" while M2 described it as the admission alone, and two descriptions of one boundary let an implementation satisfy both by host-testing a predicate and leaving the transition to the test - which is the arrangement this paragraph exists to forbid. The boundary is:

  - **the reducer DECIDES** - given the record's state and one event: whether the event is admitted at all, which state the record moves to, and - when the event ends the attempt - the `FailureCause`. It is a pure function over types `driver-binding` ALREADY owns: `BindingState`, `BindingEvent`, `FailureCause` and the transition table live in that crate today, so this seam moves no type and adds no dependency
  - **the arms ACT** - publication into the catalogue, printing, arming supervision, starting a teardown, spawning. These are syscalls and catalogue writes; they stay in `advance`, because hoisting them into a host-testable shell is a redesign of the service and this milestone does not do one

  So the regression that must fail against the pre-fix code applies `READY` and then the previously generated `TimedOut` THROUGH THE REDUCER and asserts the whole decision each time: the record moves to `Online` on the first, and the second is refused with no state move and no cause. That is a transition and a cause, not a predicate reading, and it is what "a test of its use" means at this seam. Its control - `TimedOut` and then a genuinely late `READY` - asserts the mirror image: the first ends the attempt with `HandshakeTimeout`, and the second is refused.

  THE BAR: each regression must FAIL against the code as it was on the morning of 2026-09-04. That is reproducible rather than historical - each test names the pre-fix revision, or carries an explicit negative mutation the test itself applies - so "it would have caught it" is checkable instead of asserted.

- [ ] **M2 - the deadline boundary is driven deterministically, and production has nowhere else to decide it.**

  The `TimedOut` race needed a driver that answers at about the moment its deadline expires. On x86_64 under KVM that never happens; under emulation it always does. The property is TIMING, not architecture - so it belongs on the fast target.

  BUT NOT BY TUNING A CONSTANT UNTIL EVENTS COLLIDE, which is what this item first said (corrected 2026-09-04). `READY_DEADLINE_TICKS` is a private compile-time constant with no profile carrier, and a wall-clock collision is scheduler- and load-dependent: a test built that way is flaky, and worse, it cannot distinguish "the reply was already pending when the deadline expired" - the case the guard is for - from "the reply genuinely arrived late", which must still fail the attempt. A test that cannot tell those apart is not testing the guard.

  So the boundary is driven rather than raced. The event reducer of M1 is a function of the record's state and ONE EVENT, so a test states the order it wants - `READY` and then the already-generated `TimedOut`, or `TimedOut` and then a genuinely late `READY` - by applying the events in that order, and gets it every run. That is the deterministic half and it is where the guard is actually proved.

  AND THE REDUCER DOES NOT TAKE A CLOCK, WHICH THIS LINE USED TO REQUIRE (corrected 2026-09-04). It said "the reducer takes the clock and the arbitration as inputs", which is a different seam from the one M1 defines and would have to be invented on top of it. GENERATION and REDUCTION are separate: a deadline expiring in the central wait is what MAKES a `TimedOut`, and the reducer's subject is what one already-made event does to a binding. Ordering two events needs no clock at all - it needs two events applied in an order - so the requirement was not merely misplaced, it was unnecessary.

  A GUEST-LEVEL VARIANT IS STILL WORTH HAVING - SUPERSEDED, see the withdrawal below, which removes it outright; the paragraph is kept only because the reasoning that follows answers it. It read: it is a second thing rather than the same thing, because it proves production REACHES the reducer at all, and without it the host regression stays green if `DeviceManager` later stops delegating - which is the "the predicate is tested and production does not consult it" failure M1 exists to prevent, one level up. The concern is real and it survives the withdrawal; what changes is that it is answered structurally rather than by a test that cannot see it.

  AND THE GUEST HALF IS WITHDRAWN, BECAUSE NEITHER SHAPE OF IT CAN SEE THE GUARD (corrected 2026-09-04, and this is the third attempt at this row).

  The first shape asked a driver to answer just inside or just outside a shortened deadline. Neither reaches the defect: an inside reply never causes a `TimedOut` to be generated - and once the node is `Online` it is no longer in flight, so none can be - while an outside reply consumes the timeout while the record is still `Binding`, which is the legitimate control.

  The second shape kept those two cases and proposed to prove the wiring by MUTATION: run again against a build with the guard removed and require a failure. That inherits the same defect exactly. `accepts_terminal_frame` is true only for `Binding`, so in the inside case the arm is never reached and in the outside case the guard passes through - both builds answer `HandshakeTimeout`. The mutant passes both cases, so the required failure is unachievable and the gate proves nothing.

  Forcing the sequence would need a test-only path that queues a timeout and then delivers `READY` before the drain - which is building a second production path in order to test the first, and the thing it would prove is that the second path works.

  AND "GUARANTEED BY CONSTRUCTION BECAUSE THE REDUCER WOULD GO UNREFERENCED" WAS FALSE, AND THIS MILESTONE'S OWN DEFECT IS THE COUNTEREXAMPLE (corrected 2026-09-04). `accepts_terminal_frame` is a `pub fn` in `driver-binding`, that crate sets `[lints.rust] warnings = "deny"`, and the `TimedOut` arm omitted the call for months while the `Ready` and `Failed` arms kept it. It compiled. An exported library item is never reported as dead code, and M1's whole point is that the reducer be host-testable across the crate boundary, so it MUST be exported; and even if it were private, a call removed from one arm leaves it referenced by the other two. The claim was wrong twice over, and the thing it claimed to prevent is the thing that already happened.

  The source check as first stated does not close it either. It forbids a state predicate of an arm's own - but an arm that skips the reducer and tears down unconditionally HAS no predicate, and passes.

  SO THE DISPATCH IS MADE UNAVOIDABLE INSTEAD OF ASKED THREE TIMES. The reducer of M1 is consulted ONCE in `advance`, on the loop body's unconditional path, BEFORE the `match` that carries the per-event effects, in the same position the teardown redirect already occupies. An event the reducer refuses never reaches an arm. The arms do not call it because there is nothing left for them to call, and an arm cannot omit a call it does not make - which is what the previous formulation wanted and did not get, because it left three independent obligations where the defect was one of them being forgotten.

  AND THE RESULT MUST BE UNIGNORABLE, NOT MERELY PRESENT (corrected 2026-09-04). A dispatch whose result is discarded - `let _ = reduce(..);` and then the old `match event` underneath it - keeps every textual property the previous formulation asked for: it is before the `match`, it is not inside an arm, and no arm gained a predicate. The stale `TimedOut` walks into its teardown arm exactly as before. A check that a call APPEARS is not a check that its answer is obeyed.

  So the reducer's output is the thing the `match` is performed over, and the popped event is consumed by the reducer rather than matched directly:

      let acted = match reduce(node.record.state, node.pop()?) {
          Outcome::PastTheHandshake => { print(..); continue; }
          Outcome::Act(acted) => acted,
      };
      match acted.event { .. }

  AND WHAT THE ADMITTED VARIANT CARRIES IS THE WHOLE DECISION, NOT THE RAW EVENT (corrected 2026-09-04). The sketch above first read `Outcome::Act(event)`, which hands the arms back exactly what they were given and leaves the transition and the cause to be derived in the arms - the admission-only seam M1 rejects, reached through the very structure meant to prevent it. `Act` therefore carries:

  - **the admitted event** - so the arms can select their effects, and so it exists nowhere else
  - **where the record moves** - the `BindingState` this event moves the record to, or none for an event that does not move it. `Ready`'s move to `Online` is a decision and stops being one the arm makes
  - **how the attempt ends** - when the event ends the attempt: the `FailureCause`, and whether the ending is a planned stop rather than an incident - which is `planned_stop` today, computed in an arm from the event that produced it

  Every one of those is a pure function of the state and the event as `advance` computes them today - `Stopped` to `FailureCause::Stopped`, `Exited` and `Closed` to `DriverExited`, `TimedOut` to `HandshakeTimeout`, `Failed { code }` to `DriverReported(code)` - so this moves the decision without changing it, which is what makes it a seam and not a redesign. `advance` CONSUMES those fields; it does not recompute them.

  The event and the decision the arms see EXIST ONLY INSIDE THE ADMITTED VARIANT. That is what makes the two dangerous mutations unrepresentable rather than merely forbidden:

  - **ignoring the result** - there is no other `event` binding to match on, so the code does not compile
  - **inverting it** - the refusing variant carries no event, so the swapped arm has nothing to produce and does not compile either

  ONE PART OF THIS IS COMPILER-ENFORCED AND THE REST IS NOT, and the difference is worth stating plainly rather than overclaiming a third time. Enforced: the two mutations above, and the reducer's classification is an exhaustive `match` over `BindingEvent` with NO wildcard arm, so a new event variant does not compile until someone says whether it ends a handshake. Not enforced: that `advance` calls the reducer AT ALL - deleting the dispatch and matching `node.pop()`'s event directly compiles cleanly, and leaves the reducer merely unreferenced from this crate, which last round's answer wrongly believed was a build failure. Nothing in the type system can require a call to be made; what it can require is that its answer, once made, cannot be thrown away.

  What this item therefore owes, beyond M1's regressions:

  - **one dispatch, above the arms** - the decision of M1's seam - admitted or past the handshake, which state, which cause - is consulted once, before the `match`. The arms carry effects and no decision
  - **and its result is the scrutinee** - the popped event is consumed by the reducer and the `match` is over what the reducer returns, so the admitted event exists only inside the admitted variant. Discarding or inverting the answer is then a compile error rather than a rule
  - **carrying the whole decision** - the admitted variant holds the event, the state the record moves to, and - when the attempt ends - the cause and whether the ending is a planned stop. The arms CONSUME those; an arm that recomputed one would be the admission-only seam M1 rejects, wearing this structure
  - **total over the event enum** - the reducer's classification matches `BindingEvent` exhaustively with no wildcard, so a variant added later cannot default to "not terminal" by omission
  - **a check that says so, and that is proven to bite** - a source check over `device_manager.rs` with all three halves: the dispatch is on the unconditional path before the `match`, the `match` scrutinee is the binding the dispatch produced, and no terminal arm carries a state predicate of its own. It is the same shape as the checks this tree already runs over that file - `one-wait`, `no-fixed-provider-slots`
  - **and it is mutation-tested** - the check is run against MUTATED COPIES of the source and must fail on each: the dispatch DELETED with the popped event matched directly, the dispatch moved inside one arm, its result bound to `_` with the popped event matched underneath, a state predicate reintroduced into an arm, and an arm RECOMPUTING a field the admitted variant already carries - a `move_to` or a `FailureCause::` written in an arm rather than taken from the decision. That last mutant is the one the host regression cannot see, because a recomputed value that happens to agree today is still a second opinion tomorrow. This is a text check over a file, so the mutants cost no build and no guest, and an unproven gate is exactly what the two withdrawn shapes of this row turned out to be

  AND THE SHORT-DEADLINE CONFIGURATION GOES WITH IT. It was introduced to carry a guest gate that cannot exist, and nothing else in this milestone needs it: a feature, a catalog configuration and a runner profile added for a test that proves nothing is cost with no evidence against it. The measurement that motivated the guest half is kept - the defect was invisible on x86_64 under KVM and unavoidable under emulation - and it is answered by M3 running the emulated targets at merge, where that machine exists for real rather than in imitation.

- [ ] **M3 - verification is proportional to the change.**

  `selects_everything` is a blunt instrument: it is right that a harness change can invalidate every other answer, and wrong that a documentation edit beside it costs the same 4.5 hours. TWO tiers partition the ordinary selected work, and the exhaustive release gate stands outside them - see the correction below, which this sentence used to contradict by saying three:

  - **the inner loop** - what the change itself can break, on x86_64. Minutes.
  - **merge** - the emulated targets and the guest-booting gates

  TWO TIERS, NOT THREE, AND RELEASE IS NOT ONE OF THEM (corrected 2026-09-04). The first version made release a third member of a partition over `PlanItemKey`s while also defining it as "everything, exactly as `--release` does now". Those cannot both hold, and the third promise makes it worse: a full release necessarily RERUNS the inner and merge keys and adds keys the ordinary selector never selected, so it violates the exactly-once invariant by construction - and `--release` consults no model and reads no plan at all, because it is what runs when the thing that makes choices is broken. P02M0167 requires that flat path to stay unreachable from the dependency graph for exactly that reason. Any implementation would have had to duplicate keys, stop being exhaustive, or rebuild the fallback on top of the planner it exists to bypass.

  So: the PARTITION is over ordinary selected work and has two members, inner and merge. The exhaustive release gate stays outside it, flat, unchanged, and reachable by no planner - and it is cumulative rather than a remainder: it reruns everything including what the two tiers already ran, which is what makes it a release gate rather than a tidy-up. P02M0170 owns the requirement that one such run be exhaustive and immutable over its independently checked release set; this milestone does not touch that and orders itself before it.

  AND TIERING IS A PARTITION OF THE SELECTED KEYS, NOT A SECOND SELECTOR (corrected 2026-09-04). The first draft narrowed the emulated suites by TAG, and that contradicts a written contract of P02M0167: "`verify-model`'s `select` reads `covers` and never looks at a tag", with tags kept as groupings for people and gates. `archrisk` is deliberately widening-only for the same reason - a marker's ABSENCE never proves a component is target-neutral, because `usize`, layout, alignment, atomics and a dependency's internals all differ without one. A hand-written tag list would therefore have removed tests that genuinely cover the changed component on aarch64 or riscv64, and called the result green.

  So the tiers partition the `PlanItemKey`s the selector ALREADY produced. Every key the current plan contains appears in exactly one tier and none is dropped: partition and union, checkable as an invariant rather than as a promise. An architecture-sensitive tag or id set may be an additional FLOOR - keys that must be in the merge tier whatever the selector said - never a replacement for what it selected. And any reduction of what the active selector covers is a narrowing, which P02M0167 already governs: it goes through the frozen-candidate, shadow-evidence and activation bars, or it does not happen.

  THE FAIL-CLOSED EDGES ARE PRESERVED WHOLE. "A failure to produce a plan is never a pass" is one of them and it was the only one this item named; `verify.sh` also returns distinct non-zero statuses for INCOMPLETE, SHADOW and STALE evidence, and those keep their meanings. A tier reports what it ran, what it deferred to a later tier, and which tier owes it - an inner-loop success says "the inner loop passed", never "this change is verified".

  WHAT THE TIERS ARE, CONCRETELY, because names are not implementable and the first correction was still only names (corrected again 2026-09-04). The modes come first, because the previous revision removed them while specifying what they must carry - so the item described a handoff between two things that could not be invoked:

  - **`./verify.sh`** - the inner tier, and the DEFAULT. What it runs today, minus the keys merge owns
  - **`./verify.sh --merge FILE`** - the merge tier, taking the handoff the inner run wrote
  - **`./verify.sh --release`** - unchanged, flat, planner-independent, and not a tier

  AND THE EXIT STATUS OF A SUCCESSFUL-BUT-DEFERRED RUN IS THE POINT, because `if ./verify.sh; then publish; fi` is a thing people write. An inner run that passed its own share and deferred merge keys EXITS NON-ZERO with its own status - it did not verify the change, it verified part of it - and prints what it deferred and to whom. That is a change to what `verify.sh` currently reports: today it answers `TRUSTED` and exits zero after running the plan it chose, and filtering that plan without changing the claim is the failure this paragraph exists to prevent.

  AND "A ZERO EXIT MEANS NOTHING FURTHER IS OWED" WAS TOO STRONG BY EXACTLY ONE STEP (corrected 2026-09-05). It said a change whose plan has no merge keys is finished at the inner run. An empty deferred set is reachable - a host-only change, and the non-code case most plainly - and if that run is final then NOTHING EVER REGENERATES THE PLAN AT THE PROPOSED REVISION. The commit's own delta against its own parent is where a rebase, an amend or a partial stage shows up, and this plan answers all three by regenerating at merge; a path that skips merge skips all of it, and the row below saying merge is owed by whoever proposes the change would be contradicted by the status the tool actually returns.

  AND AN INNER ZERO CANNOT SAY IT IN A NOTE, WHICH THE FIRST CORRECTION TRIED TO DO (corrected 2026-09-05, and this paragraph's own opening argument is what refutes it). It proposed that a zero exit means "this working tree's change passed" with the qualification printed beside it. A shell caller reads the STATUS, not the note - that is the entire reason the paragraph above gives for a deferred run exiting non-zero - so `if ./verify.sh; then publish; fi` would publish before the changed-base, partial-stage and regenerated-`P1` checks had ever run. The qualification has to be in the number.

  SO THE DEFAULT MODE DOES NOT RETURN ZERO WHILE MERGE IS OWED, and merge is owed for every dirty run. It reports `INCOMPLETE` and exits 6 - the status this runner ALREADY has for exactly this meaning, "this run verified part of what the change needs" - and prints that its own share passed and what remains. Whether `D` was empty changes what merge has to DO, not whether it is owed: with `D` empty its work set is `P1' - I`, usually nothing, so it costs a regeneration and an inventory phase rather than a run. ZERO IS RESERVED FOR A RUN THAT OWES NOTHING - a merge run that completed, and the flat release paths that stand outside the tiers. The cheap case stays cheap; what it stops being is indistinguishable from a finished one.

  Four more things it owes, each of which a green run can otherwise be produced without:

  - **executed over STEPS, not keys** - a plan is per key, but the runnable unit is a `Step`, which discharges zero, one or many keys and carries the ids of the steps it cannot start before. So a tier is a PREREQUISITE-CLOSED set of steps whose discharged keys are its share of the partition. A prerequisite pulled into both tiers - a build both need - RUNS in both and is ACCOUNTED to one: it discharges its keys in the tier that owns them and is a prerequisite elsewhere, which is what keeps "exactly once" true of evidence rather than of execution. A step that discharges no keys is legitimate only as a prerequisite, which the model already states

  - **one change, carried** - the handoff carries the source identity, the source model hash with `model_hash` recorded beside it, a digest of the plan it was produced from, THE PER-KEY OUTCOMES OF THE INNER SHARE, and the exact deferred set. Merge refuses a handoff whose identities do not match, and claims completion only when both shares are present and every key of both is DISCHARGED - see the retirement accounting below, where discharge is passed-or-retired and a failure is neither. Carrying identities alone would let a merge run pass while the inner share had failed or never run, which is the same "part of it looks like all of it" the exit status above is about. Today's history cannot help: it is a mutable freshness and cost cache keyed by model hash alone and cannot tell one change from another.

    AND A DIGEST WITH NO REPRODUCIBLE PREIMAGE VALIDATES NOTHING (corrected 2026-09-04). A carried plan digest can only be compared against a plan merge computes for itself, and merge could not compute one: `--for-change` reads the WORKING TREE, and after the ordinary commit the working tree is clean - it dies with "the working tree is clean" rather than reproducing the inner selection. So the digest sat beside nothing, and with it the deferred set could not be shown to be complete.

    MERGE REGENERATES THE PLAN FROM THE COMMIT. The change set is `changes::range` over `parent(P)..P` - the delta the proposed commit actually is - and `Model::plan` takes it from there. The change KIND does not enter selection, so the dirty run's `Untracked` and the commit's `Added` select identically.

    AND THE PLAN IS NOT A FUNCTION OF PATHS ALONE, WHICH MAKES EQUALITY THE WRONG TEST (corrected 2026-09-05). `Planner:: for_model` loads `.build/state/verify-history.json`, and the cost escalation ADDS items from measured per-key durations - so an unchanged model and unchanged paths can plan differently once anything has run. The inner run WRITES that file as each step finishes, which means demanding an equal plan would refuse the ordinary handoff on account of a cache the ordinary workflow itself updates. Freezing the history into the handoff would fix the symptom and introduce two worse things: a cache that exists to be updated, carried as evidence, and a handoff whose stale copy could SHRINK the plan merge computes.

    SO THE TEST IS CONTAINMENT, NOT EQUALITY, and it is safe in the only direction that matters. Merge regenerates with the LIVE history, and the WORK SET IT OWES IS STATED AS ARITHMETIC rather than described: with the inner plan `P0 = I + D` - passed inner keys and the deferred set - and the regenerated plan `P1`, merge runs `D + (P1 - (I + D))` closed under prerequisites. Every deferred key runs BECAUSE IT WAS DEFERRED, and every key the regeneration adds runs because nothing has answered for it. AND `D - P1` IS THE CASE THE PROSE HERE GOT WRONG (corrected 2026-09-05). This paragraph said a handoff-covered key absent from `P1` is "the inner run having done more than was needed, which costs nothing". That is true of a key in `I`, which RAN, and false of a key in `D`, which did not: the cost rule crosses its 0.9 threshold in EITHER direction as history moves, so a contraction is reachable, and reading the sentence literally would let a shrinking cache silently discharge work the inner run promised. `D` is a promise and a promise is not renegotiated by a cache. The arithmetic above says so without needing the sentence.

    AND THERE IS A SECOND KIND OF CONTRACTION THAT IS NOT A CACHE AT ALL, WHICH "`D` ALWAYS RUNS" GETS WRONG IN THE OTHER DIRECTION (corrected 2026-09-05). Per-target applicability is read from compiled symbols BECAUSE it cannot be read from source - the `cfg(target_arch)` conditions are scattered - so a stale aarch64 binary can put a key in `D` for a test the proposed source no longer builds on aarch64, while the test's id and `covers` are unchanged and the source model hash therefore agrees. Once the inventory phase below has built that target from the proposed source, the refreshed catalog cannot lower that key at all - and handing the pre-refresh selection to the guest HARD-FAILS, because an unknown id is a hard failure by design. The promise could not be kept by running it; insisting would turn a valid change into an unfinishable one.

    SO THE TWO CONTRACTIONS ARE TOLD APART BY WHAT SAYS THE KEY IS GONE. A cost cache saying a key is no longer worth taking does not make the key untrue, and `D` still runs it. A build OF THE PROPOSED SOURCE saying the test does not exist on that target is evidence about the system rather than about a cache, and the key is RETIRED - named in the output with the reason, never dropped silently. Retirement is permitted only for a key the refreshed catalog cannot lower, and only when the source model hash agrees; a source-model difference was refused long before this point.

    AND RETIREMENT NEEDED ITS ACCOUNTING WRITTEN DOWN, NOT JUST ITS NAME (corrected 2026-09-05). Three things were left contradicting each other: merge "claims completion only when both shares are present and both passed" while a retired key is neither; the same paragraph required every key of the work set to LOWER while permitting one that cannot; and retirement was motivated only for a key from `D`, though a key added by `P1 - (I + D)` from the same stale inventory disappears from `P1'` in exactly the same way. An implementation reading the pass invariant could not finish a valid contraction without reporting a retirement as a pass.

    SO THE ORDER AND THE VOCABULARY ARE FIXED. Retirement happens IN the reconciliation, BEFORE the lowering: the provisional work set is `D + (P1 - (I + D))`, the reconciliation removes from it every key the refreshed catalog cannot lower - whatever half it came from - and adds `P1' - (I + D + P1)`, and it is the RESULT that must lower to a prerequisite-closed set of steps. Nothing unlowerable ever reaches the lowering, so the two requirements stop contradicting each other.

    AND EVERY KEY OF THE PROVISIONAL SET ENDS IN EXACTLY ONE STATE: passed, failed, or retired. DISCHARGED means passed or retired, and completion is stated over discharge rather than over passing - a retired key is a key the proposed source says does not exist, which is a real answer and not a missing one. Failed is failed. The output names the retirements with their reason, and their count, so "discharged" can never quietly mean "not run". This is robust to history drift by construction rather than by freezing anything, and it needs no new carried state. Merge therefore requires, before it executes anything: the SOURCE model hash matches - see the row below - the inner and deferred key sets are DISJOINT, and everything in the work set above lowers under the current model to a prerequisite-closed set of steps. A handoff that fails any of them is refused with the disagreement named. The carried plan digest stays in the handoff and is REPORTED beside the regenerated one when they differ - it is a diagnostic about a cache having moved, and it decides nothing on its own. A commit with no parent or with more than one is REFUSED. The workflow this milestone serves is "a change was made and is proposed", and a merge commit's delta is not one change set; inventing a first-parent convention for it would be a rule nobody asked for

  - **and the hash it gates on is the source's** - `model_hash` MOVES WITHOUT THE SOURCE MOVING, which would make the ordinary handoff refuse itself (corrected 2026-09-05). Kernel test VARIANTS are discovered from compiled binaries under `.build/cargo/kernel/<triple>/debug/deps` - the catalog says so in as many words, "derived per target from the compiled test binaries" - and those variants are hashed. `.build/` is ignored, a checkout that has built nothing is an explicitly supported state, and RUNNING THE INNER x86_64 SUITE WRITES THAT BINARY. So the ordinary first change on a fresh checkout plans under one hash, produces the artefact, and is handed to a merge run that computes another - and a rule refusing every mismatch refuses a valid handoff for having done its own work. The two inputs are not the same kind of thing and are separated rather than merged: the handoff gates on a SOURCE MODEL HASH - everything `model_hash` covers except the kernel-test variant lists, which are the only artefact-derived term in it. Test IDS and their `covers` come from `scan_source` and stay in; only "which targets was a binary found for" comes out. `model_hash` ITSELF IS NOT CHANGED - shadow records and the history are keyed by it and this milestone does not touch that; the source hash is a second digest beside it, for this gate. A source-model difference is a REFUSAL: the system being verified changed. An artefact-only difference is not. Where the binary is MISSING, the work set above already answers it: the target was unenumerated at inner time, its whole-suite stand-in was deferred and still runs, and the scoped keys merge discovers run beside it. More work than a perfect oracle would order, in the safe direction, once per checkout.

    AND A STALE BINARY IS NOT THAT SHAPE, WHICH THIS ROW CLAIMED IT WAS (corrected 2026-09-05). A target whose binary is present but OLD is not `missing` - it enumerates, and it enumerates the tests it was built with. A test declared in the source and absent from that binary produces no variant for the target and therefore no key, so `P1` cannot contain it and `D + (P1 - (I + D))` cannot add it. Merge then BUILDS the current binary and hands the guest `TEST_SELECTION=` computed before that build, so the new test does not run on that target and nothing says so. The arithmetic was sound and its INPUT was stale.

    SO THE SELECTION IS RECONCILED ONCE MORE, AFTER AN INVENTORY PHASE THAT HAS TO BE BUILT RATHER THAN FOUND.

    AND "THE KERNEL BUILD PREREQUISITE ALREADY MAKES IT CURRENT" WAS FALSE (corrected 2026-09-05). That prerequisite lowers to `./build.sh --arch X --part kernel`, whose kernel part is a plain `cargo build` - an ORDINARY kernel, carrying no test descriptors, which discovery skips by design. The descriptor-bearing binary comes from `cargo build --tests` INSIDE `test-kernel.sh`, in the same step as the guest it feeds and moments before QEMU starts. There was no boundary of the kind the previous paragraph assumed, so a reconciliation placed there would have rescanned the same stale binaries and changed nothing.

    AND IT CANNOT BE A MID-RUN EXTENSION EITHER. The executor materialises ONE steps file and settles budget affordability before its first command; a work set that grows after execution has begun is outside that contract, and rewriting the scheduler is not what this milestone is for.

    SO THE PHASE COMES BEFORE THE LOWERING, NOT INSIDE THE RUN. Merge's order is: regenerate `P1` from the commit delta and check what the rows above check; determine the targets the work set touches; BUILD THE TEST INVENTORY for each of them from the proposed source; re-derive discovery over those binaries; regenerate as `P1'` and reconcile; and only then lower ONCE and execute. The executor's contract is untouched because there is still one steps file and one lowering - they are simply computed after the artefact input has stopped being stale. The reconciliation itself is unchanged in substance: the work set is extended by `P1' - (I + D + P1)`, and the keys it adds are REAL keys, so the guest step that runs them records them - rather than an unfiltered suite running with tests unattributed, which is a defect this tree has already fixed once.

    AND THE SEAM EXISTS BUT DOES NOT WORK, WHICH IS THE SECOND WRONG THING SAID ABOUT THIS SCRIPT (corrected 2026-09-05). The version before last said `test-kernel.sh` "builds and boots in one breath" and owed a new entry point; the version after it said `--build-only` already exists and nothing is owed. The flag exists and is documented in the usage line and in `test.sh`, AND IT DOES NOT DO WHAT IT SAYS. It sets a label and a three-minute limit and appends `--no-run` to a `TEST_ARGS` array that NOTHING EVER READS - the build is a `cargo build --tests` under the lock, and execution then falls straight through to `qemu-run.sh`. `BUILD_ONLY` is consulted again only after the guest has exited or timed out, to choose the wording of the result. So `--build-only` today builds, BOOTS, and does it under a timeout sized for a compile.

    THAT IS A DEFECT IN ITS OWN RIGHT AND THIS MILESTONE OWNS IT, because the phase cannot exist without it. The repair is small and local: after the locked build and the staging copy, a build-only run REPORTS AND EXITS, before the guest is started; the dead `TEST_ARGS` array goes with it, since the `cargo test` invocation it was written for no longer exists. What that leaves is a compile that needs no system volume, no medium and no QEMU - which is exactly what discovery needs, and what a documented flag already promised.

    THE PHASE INVOKES `src/harness/test-kernel.sh <arch> --build-only` DIRECTLY, not the top-level route: `test.sh` first demands a current system volume and a matching build stamp, which is right for a suite that boots and impossible for a phase whose purpose is to run on a checkout that has built nothing yet.

    AND A BUILD IN FRONT OF A BUDGETED RUN IS THE FAULT THE BUDGET CONTRACT IS NAMED AFTER (corrected 2026-09-05). `budget_select` costs whole PREREQUISITE-CLOSED branches for a reason its own comment states: putting the builds outside it makes `--budget 10` "a run that spends forty minutes building and then declines to test". A phase that compiles before lowering is exactly outside it, and my order put it there.

    SO MERGE REFUSES `--budget`, and that costs nothing because a budgeted merge could never have produced a merge run's only useful outcome. Merge completes or it does not; a partial merge is an `INCOMPLETE` the inner run already reports more cheaply. The flag is untouched for the inner tier, where it does what it always did and where no inventory phase exists. Refusing it up front also gives the honest answer on a fresh checkout: nothing is built, and the run says why, rather than spending a cold build to discover it cannot finish.

    AND THE INVENTORY PHASE IS THE INNER TIER'S STALENESS ANSWERED TOO. The inner loop plans from whatever binaries are lying in `.build` and stays fast; merge is the tier that must be exact, and it refreshes every touched target INCLUDING x86_64. That is the division: an inner answer may be slightly stale about which tests exist, and nothing is attested on it.

    WHAT THE PHASE COSTS is one `cargo build --tests` per touched target, ahead of the one `test-kernel.sh` does anyway - so on a cold tree it is not new work, it is the SAME cold build moved earlier. The genuinely added cost is the second compile: `TEST_SELECTION` is baked in at compile time through `option_env!`, so a later build under a narrowed selection recompiles that crate rather than hitting the cache. Incremental, tens of seconds per target, against a tier whose subject is tens of minutes. The earlier version of this paragraph said "tens of seconds per target" flatly, which read as a claim about the cold case and was wrong about it.

    ONCE, NOT TO A FIXPOINT. The test binaries are the only artefact input to the model, and after this phase they describe the proposed source; a second pass has nothing left to discover. This is also why acting on it is safe: it is exactly the artefact-derived term the source model hash excludes, so a reconciliation that changes the selection is never a reconciliation that changed the system.

    AND THE SAME BRACKET BELONGS ROUND THE INNER RUN, NOT ONLY ROUND MERGE (corrected 2026-09-05). Merge TRUSTS the handoff's per-key outcomes and its source snapshot, and the first version of this correction bracketed only merge's own work - so an inner run could label its outcomes with an identity taken before the tree moved, or run its steps over bytes that were later restored, and merge has no way to reconstruct what those steps actually read. The inner tier therefore takes the effective-tree identity BEFORE it plans, revalidates it after its last step, and REFUSES TO EMIT A USABLE HANDOFF if it moved - the same rule, applied to the tier that produces the evidence rather than only to the one that consumes it.

    AND THE IDENTITY IS CHECKED AGAIN AFTER THE PHASE AND AFTER THE RUN, BECAUSE THE TREE CAN MOVE UNDER BOTH (corrected 2026-09-05). The checks above happen once, at the start; the inventory build and every step after it then read the WORKING TREE, which is mutable and belongs to somebody who is still working in it. An ordinary edit to a `.rs` file changes what is being built and tested and leaves the SOURCE MODEL HASH untouched - the narrow hash covers the selector's own sources, the registry, the graph, the features and the arch-risk scan, not the kernel file somebody just saved - so merge could attest `HEAD` over bytes that were never `HEAD`.

    THIS IS NOT A NEW MECHANISM, IT IS THE ONE THE SHADOW PATH ALREADY USES. That path digests the tree before and after and refuses the comparison if the two differ, and its comment makes this exact argument: pinning to the model hash "does not close the race", because an ordinary edit "changes what the sweep is testing and leaves the model's identity untouched". Merge does the same with the effective-tree identity - recomputed after the inventory phase and again after the last step - and REFUSES COMPLETION if it moved, naming the two digests. It is a tree digest either side of the work, which costs nothing next to the work. This is the minimum this milestone needs and no more; the immutable evidence scope belongs to the milestone this one orders itself before

  - **and the identity survives a commit** - `source_digest` hashes `HEAD` plus the working tree's changes, so committing the same bytes changes BOTH inputs and a dirty inner run can never match the revision it is later proposed as. That is the ordinary workflow, not an edge case.

    AND "THE CHANGED PATHS' BYTES, WITHOUT `HEAD`" WAS THE WRONG REPAIR AND FAILS OPEN (corrected 2026-09-04). The same edits rebased onto a different base produce the SAME digest while the tree around them differs, so merge would attest inner evidence produced against a different system. Bytes alone identify no path, mode, change kind, deletion, or either side of a rename - and a clean merge revision has no way to reconstruct which paths a dirty run considered changed, so the comparison could not be made at all. The identity is therefore the EFFECTIVE SOURCE TREE, not the delta: a canonical digest over the non-ignored working tree - every path `git ls-files -co --exclude-standard` enumerates, with its bytes and its executable bit read FROM DISK. It is identical before and after a commit that changes no content - which is the transition the workflow needs - and differs whenever anything the run was made over differs, a mode, a deletion, or either half of a rename. IT DOES NOT SEPARATE TWO COMMITS THAT SHARE A TREE, and this sentence used to claim it did by listing "the base" among them (corrected 2026-09-05). A commit-stable digest is stable across exactly that: same bytes, same digest, whatever the parent. What separates those two commits is the regenerated plan and the pinned revision below, not this digest, and saying otherwise contradicted the acceptance rule further down. A tree digest answers all of those by construction; a delta has to enumerate them and be right about each.

    AND "ALL TRACKED CONTENT AS IT WOULD BE COMMITTED" WAS TWO MISTAKES IN ONE PHRASE (corrected 2026-09-04). "Tracked" drops untracked files, and the selector discovers those DELIBERATELY - `changes::working_tree` passes `--untracked-files=all` and `Kind::Untracked` is a change kind - so an inner run may select and build a newly added file that the identity does not describe, and committing that file then CHANGES the identity, which is precisely the transition it exists to survive. "As it would be committed" also leaves the index in play: an index-derived tree names bytes a partially staged run never read. Reading the enumeration and the bytes from DISK settles both - it is the same set and the same source `test-preflight` already treats as the inputs a build consumes, and it is commit-stable because committing changes neither what `-co --exclude-standard` lists nor what is in the files. Ignored paths stay out, because build outputs live there and would move the identity on every build; a build input under an ignore rule is a defect in the ignore rules, and this milestone does not work around one.

    AND THE ENUMERATION NEEDS NORMALISING, BECAUSE `-c` LISTS PATHS THAT ARE NOT THERE (corrected 2026-09-04). A cached path already deleted from disk is still listed, so "every enumerated path contributes its bytes" is undefined for it - and once the deletion is committed the path stops being listed at all, which is the identity moving across a commit of exactly the tested tree. An unstaged rename is the same thing twice. A path ABSENT FROM DISK THEREFORE CONTRIBUTES NOTHING: it is not in the effective tree, which is the whole subject. That is not a workaround, it is what `test-preflight` already does with the same enumeration. With it, a dirty deletion and a dirty rename both digest the same before and after the commit that records them. The rest of the normalisation: `-z` throughout, because a path may contain a newline and this tree has been bitten by that before; paths deduplicated, because a conflicted file is listed once per stage; sorted by path bytes; each entry contributing its path, its kind - regular file, symlink - and for a regular file the EXECUTABLE BIT only and for a symlink its target, which is the domain a git tree records, so the two sides below can be compared at all AND THE INNER SNAPSHOT MUST BE BOUND TO THE PROPOSED REVISION, WHICH LAST ROUND'S ACCEPTANCE OF AN INDEX/WORKTREE DISAGREEMENT LEFT OPEN (corrected 2026-09-04). Ignoring the index correctly identifies the bytes a run CONSUMED, and that is all it does. With `HEAD` at O, staged bytes A and worktree bytes B, both tiers hash and test B, the commit records A, and merge would attest revision A on evidence produced against B - a revision nothing ever tested. So merge takes the proposed revision as well as the handoff and computes THE SAME CANONICAL DIGEST OVER THAT COMMIT'S TREE - `git ls-tree -r -z` over the same fields, which is why the disk side normalises into git's mode domain above - and requires all three to agree: the inner snapshot, merge's own snapshot, and the commit's tree. An ordinary commit of the tested bytes satisfies it; the A/B partial stage does not, and is refused with the disagreement named rather than silently passed. A gitlink is refused outright: this tree has no submodule, and an identity that quietly hashed a commit id as though it were content would be the same class of fault as the two above.

    THE PROPOSED REVISION IS `HEAD`, and that is a consequence rather than a choice (added 2026-09-04, where the correction above said "the proposed revision" without saying where it comes from). Merge must run over a working tree whose content IS the proposed commit's - this tree's workflow never moves git state, so there is no checkout to a revision and no materialisation of one - and the three-way agreement above is what enforces it. A revision that is not `HEAD` would then fail that agreement on its first step, so a flag to name one would be a flag whose every non-default value is refused. The handoff does not carry it either: a handoff that named its own revision would be a stale handoff asserting which commit it is about.

    AND `HEAD` IS A MOVING NAME, SO IT IS RESOLVED ONCE AND PINNED (corrected 2026-09-05). Merge reads `HEAD` at its first check and works from that COMMIT OBJECT ID for the rest of the run: `parent(REV)..REV` is what `P1` and `P1'` are regenerated from, and `REV`'s tree is what the three-way agreement compares. Without that, `HEAD` is re-read implicitly and the two stability brackets cannot notice it moving, because they compare the EFFECTIVE-TREE identity - which is commit-stable by design and therefore silent about which commit the tree belongs to. A commit made mid-run with a different parent and the same final tree leaves both that digest and the source model hash equal while `parent(HEAD)..HEAD` names a different delta, so merge could complete over a reconciliation that never happened for the revision it was naming. The pinned id joins the identity in both brackets - after the inventory phase and after the last step - and a `HEAD` that no longer resolves to it REFUSES COMPLETION, naming both ids. It costs one `git rev-parse` and it is what makes "the proposed revision" a fact rather than a lookup.

    AND "A CHANGED BASE IS REFUSED" NEEDS ITS REASON, NOT JUST ITS VERDICT. Two commits with different parents CAN share a final tree, so a tree digest alone does not separate them. What separates them is the regenerated plan: if the bases differ at any path, then reaching one final tree means the two deltas differ there too, so their path sets differ, so the regenerated plan holds keys the handoff does not cover - and containment turns those into work merge must do and name, which is the right answer to "this was verified against a different delta" whether the difference came from a rebase or from anywhere else. The case that survives is the one where the path sets agree - the same bytes proposed, the same keys selected - and that is not a hole, because the evidence then covers exactly the system and the scope being proposed. This is a NEW identity beside `shadow::source_digest`, not a change to it. That one hashes `HEAD` and keeps its own job - pinning a shadow record to the tree that produced it - and cannot serve here for the reason this row opens with. Tested in both directions. ACCEPTED: a dirty-to-committed transition that records exactly the tested bytes, including one whose change is an ADDED file, one whose change is a DELETION, and one whose change is an unstaged RENAME - and a tree whose index disagrees with the working tree, PROVIDED the proposed commit records the bytes that were tested. REFUSED: the A/B partial stage, where the commit records the staged bytes and nothing tested them; and a changed path, a mode change, an intervening deletion and an intervening rename - each of which moves the tree away from the snapshot that was tested. AND "A CHANGED BASE" IS NOT A MEMBER OF THAT LIST, WHICH IT WAS UNTIL NOW (corrected 2026-09-05). This paragraph explains at length that a base change is answered by the regenerated plan, and then listed it flatly among the refusals - two verdicts for one case, and an implementation could only pick one. The refusal is a TREE MISMATCH, not a base change: a rebase whose result differs from the tested snapshot is refused because the snapshot is gone, which is the ordinary outcome and is already covered by "a changed path". A base change that yields the SAME tree is not refused at all - the regenerated plan holds whatever keys the different delta selects, and containment runs and names them. One verdict per case, and the regression asserts that one. The index case appears on both lists on purpose - what decides it is not the disagreement but which bytes the commit ends up holding, which is exactly what the previous round got wrong by accepting the disagreement itself. P02M0170 is `PLANNED` and defines an immutable evidence identity of its own; this milestone orders itself BEFORE it and therefore cannot borrow it, so it defines the minimum it needs and says so. When P02M0170 lands, its identity replaces this one rather than sitting beside it

  - **an actor for every transition** - `verify.sh` states in as many words that this tree has no CI or timer authority and leaves follow-up to the person at the terminal. So the trigger is named rather than assumed: merge is owed by whoever proposes the change, at the revision they propose, and an inner-only result SAYS what it still owes and to whom. This milestone does not invent a scheduler
  - **a status contract** - `INCOMPLETE`, `SHADOW` and `STALE` keep their distinct non-zero statuses and outrank a tier's own verdict: a tier cannot report success over evidence any of the three refuses. And `FULL` today means "the unpartitioned plan was full, so this stands on its own" - filtering that plan to the inner tier without changing the claim would let a full-triggering tool change report complete verification from an inner run. It must not, and that is a regression this item owes rather than a rule it states

  AND THE ACCEPTANCE CASE HAS TO BE ONE THAT CAN FAIL. Documentation already selects nothing and an ordinary service is already scoped, so a test built from those proves nothing about tiering. The case is a change to a host tool that is genuinely scopeable, run through BOTH partition tiers, with every deferred key shown to reappear in the tier that owes it. The release gate is not part of that case: it is cumulative and planner-independent, so running it proves nothing about the handoff. The tools that DEFINE or JUDGE the system - the harness, `mkpackages`, `system-manifest`, `lsidl-gen`, `verify-model` - keep selecting everything, which is not a gap in the tiering but the reason the escalation exists.

- [ ] **M4 - no gate waits for a clock when the answer is already there, and "the answer" is decided PER CASE.**

  Ten local boots at 120 s and four port boots at 300 s is forty minutes of waiting in `signed-boot` alone, for logs whose content had settled in seconds.

  AND A SINGLE GLOBAL MARKER LIST IS THE WRONG SHAPE, WHICH WAS MEASURED RATHER THAN ARGUED (corrected 2026-09-04). A first attempt gave the gate one settled-marker set including `loader: kernel loaded`. That is a terminal verdict for most of its cases and a MIDDLE step for one: the altered system-volume image is refused AFTER the kernel is loaded, because that is where the image is read, and the file says so in as many words. The watcher killed the guest at the intermediate line, the refusal never reached the log, and the gate reported that an altered volume had not stopped the boot - a security assertion turned false by an optimisation. Removing that one marker made the case pass again. The failure was silent in the worst way: the gate still ran, still printed cases, and still exited non-zero for a reason that looked like a real defect.

  So each case names its own terminal predicate:

  - **loader-only cases** - may stop at the loader's final verdict - a refusal, or a hand-off that no later assertion depends on
  - **guest-health cases** - need an explicit final success signal AND their required late-failure observation. `qemu-virtio-iommu-x86_64` is the example that forbids the generic rule: it keeps watching after the positive lines because a panic ten seconds later, a reboot counted by its loader banners, a driver restart or a late fault must all fail it, and its own comment records that a boot which printed every line and then panicked used to pass
  - **silent-refusal cases** - firmware rejecting an unsigned loader may produce NO serial output at all. Those need a positive external oracle, or they keep a calibrated backstop and this item does not touch them

  THE WATCHER ITSELF NEEDS NEGATIVE FIXTURES: a log that prints the early success marker and then panics, one that resets, one that retracts. If the watcher cannot fail on those, it has weakened the gate it was meant to speed up, and that is precisely what happened on the first attempt.

  THE INVENTORY IS PART OF THE ITEM. The catalog declares eight guest-booting gates; naming four was a guess. Each is listed with its terminal predicate before any is changed.

- [ ] **M5 - the merge tier boots two guests at once, by default.**

  `verify.sh --jobs` exists and defaults to one; the two emulated targets do not share disk images and can boot at once.

  AND "WHICH TIER OPTS IN" WAS THE DECISION, NOT THE WORK (corrected 2026-09-04). Stated as work owed, this item could be satisfied by calling the merge mode once with `--jobs 2`, watching two guests overlap, and leaving the ordinary merge entry point at the serial default - passing the letter of the test while the workflow whose wall-clock time motivates the item is unchanged. So the policy is stated here: THE MERGE TIER'S NORMAL ENTRY POINT REQUESTS TWO GUEST SLOTS. An explicit `--jobs` from the caller overrides it in either direction, and the flat `--release` and `--sweep` paths stay serial, which is not a default this item may change - they consult no model precisely so they still work when the model is what is broken, and `verify.sh --jobs` remains the only scheduler.

  Two is the number because there are two emulated targets and they are what the wall clock is spent on; a third slot would contend for the host rather than shorten anything, and x86_64 is fast enough that overlapping it saves less than the contention costs.

  Acceptance runs the NORMAL merge entry point with no flags and observes that the two emulated guests overlap in time - not that an invocation carrying `--jobs 2` succeeded.

  AND THE SCHEDULED CACHE WARMING IS WITHDRAWN (2026-09-04). It said "a scheduled build keeps it out of the interactive path" and named no scheduler, runner, cadence, revision, cache location, locking or failure surface - and there is none to name: `verify.sh` states in as many words that this tree has no CI or timer authority. Worse, the deliverable is self-defeating as written. A warmer on another machine populates a cache this workspace never reads, so it buys nothing; a warmer in THIS worktree builds whatever uncommitted sources happen to be open and contends with the developer for the build lock, which is the interactive path it was supposed to keep clear.

  The measurement that motivated it stands and is recorded rather than lost: a cold aarch64 build is 15-20 minutes against seconds for an incremental one, and that gap is paid because the target is built once a week. Closing it needs a build-cache identity that survives between runs and a place to run them from, and both are infrastructure this milestone does not have. It belongs to whoever introduces scheduled work, with those questions answered first.

## Definition of done

- Each of the three defects of 2026-09-04 has ONE host regression naming it, driving the seam that decided, and failing against the code as it was that morning - reproducibly, by naming the pre-fix revision or by carrying its own negative mutation.
- The stale-timeout regression asserts BOTH compositions and the WHOLE decision each time: `READY` then a queued `TimedOut` moves the record to `Online` and then refuses, with no state move and no cause; `TimedOut` then a genuinely late `READY` ends the attempt with `HandshakeTimeout` and then refuses. The boundary is driven by applying two events in an order - the reducer takes no clock - not produced by tuning a constant until events collide.
- Inner and merge are invocable modes - `./verify.sh` is the inner tier and the default, `./verify.sh --merge FILE` takes its handoff - and they partition the `PlanItemKey`s of ONE plan: every key of the plan the inner run made appears in exactly one of the two, none is dropped, and the invariant is checked rather than asserted. Keys merge adds by regenerating at the commit are extra work rather than a violation of it, and are named as such. The exhaustive release gate stays outside that arithmetic and outside the planner's reach.
- The default mode NEVER returns zero while merge is owed, which is every dirty run, `D` empty or not. It reports `INCOMPLETE` and exits 6 - the runner's existing status for "this run verified part of what the change needs" - and names its own passed share and what remains. Zero is reserved for a merge run that completed and for the flat release paths outside the tiers, and a regression drives `if ./verify.sh; then publish; fi` over a passing inner run with an EMPTY deferred set and requires that it does not publish.
- The handoff carries the inner share's per-key outcomes and the exact deferred set, and merge claims completion only when both shares are present and every key of both is DISCHARGED - passed, or retired because the refreshed inventory built from the proposed source has no such variant. A failed key is never discharged, and retirements are named with their reason and counted.
- The handoff identity is a canonical digest of the non-ignored working tree, read from disk: untracked files included, the index not consulted, cached-but-absent paths contributing nothing, entries deduplicated and NUL-safe, and each entry normalised into the mode domain a git tree records.
- A merge run RESOLVES `HEAD` ONCE to a commit object id and works from that fixed revision for the whole run. It agrees the inner snapshot, its own snapshot AND the tree of that revision, which is the proposed one because merge runs over the working tree and this tree never moves git state. An unchanged dirty-tree inner run is accepted at the revision that same content was committed as; so is one whose change is an added file, a deletion, or an unstaged rename, each committed exactly as tested. A partial stage - `HEAD` at O, index at A, worktree at B - is REFUSED rather than attested as A, and a changed path, a mode change, an intervening deletion and an intervening rename are each refused. A BASE CHANGE IS NOT ITSELF A REFUSAL: a rebase whose result differs from the tested snapshot is refused as a changed tree, and one that yields the same tree is accepted and answered by the regenerated plan.
- Merge REGENERATES the plan from `parent(REV)..REV`, the pinned revision, and refuses unless the SOURCE model hash matches and the inner and deferred sets are disjoint. Its PROVISIONAL work set is `D + (P1 - (I + D))`; reconciliation then retires from it every key the refreshed catalog cannot lower - from either half - and adds `P1' - (I + D + P1)`, and the RESULT is what lowers, prerequisite-closed. Nothing unlowerable reaches the lowering. A commit with no parent or more than one is refused. This runs even when the deferred set is EMPTY, and a same-final-tree commit over a different parent whose delta selects additional paths has those keys found and run in that case too.
- Four tests over that arithmetic. The ordinary dirty-to-commit transition is accepted with nothing added. `.build/state/verify-history.json` is then moved DELIBERATELY ACROSS THE 0.9 THRESHOLD IN BOTH DIRECTIONS between inner and merge - including by the inner run's own step recording: on the expanding side the added keys run, and on the CONTRACTING side every key of `D - P1` still runs. A same-final-tree commit over a different base whose delta selects different keys has those keys run and named - and that case is run TWICE, once with a non-empty deferred set and once with an EMPTY one, because an empty `D` is exactly when a skipped merge would have hidden it.
- An artefact-only model difference does not refuse a handoff. A merge run whose kernel-test discovery finds a binary the inner run did not have - the fresh-checkout case, where running the inner x86_64 suite creates it - is ACCEPTED, and the newly discovered keys are run and named. A source-model difference is still refused.
- `--build-only` is REPAIRED first: today it sets a label and a limit, appends `--no-run` to a `TEST_ARGS` array nothing reads, and then boots the guest anyway under a compile-sized timeout. After the fix a build-only run reports and exits after the locked build and its staging copy, before QEMU is started, and the dead array is removed. A regression runs it on a checkout with NO system volume and no medium and requires that it produces a descriptor-bearing binary, that `nm` finds the descriptors in it, and that no QEMU process is started.
- Merge BUILDS the test inventory for every touched target from the proposed source, before it lowers anything, by invoking `src/harness/test-kernel.sh <arch> --build-only` directly - not `test.sh`, which first demands a current system volume and build stamp and so cannot run on a checkout that has built nothing. It reconciles against that once rather than to a fixpoint, and there is still ONE steps file and ONE lowering. The ordinary kernel build prerequisite is NOT that inventory: it carries no test descriptors.
- Inventory EXPANSION and CONTRACTION are both proven, with old aarch64 and riscv64 binaries left in place. Expansion: a newly declared test RUNS on every applicable deferred target and is recorded against its own key, not merely accepted. Contraction is proven TWICE - once for a key that came from `D` and once for a key that came from `P1 - (I + D)` - and in both a test whose `cfg(target_arch)` no longer builds it on that target has its key RETIRED and named with the reason, the run completes, and no selection is handed to a guest that would hard-fail on an unknown id. A cost-cache contraction in the same run still runs every key of `D - P1`, so the two are told apart rather than conflated.
- `--budget` is REFUSED by the merge mode, before anything is built. Driven on a fresh checkout with a target that has no test binary and a budget far too small, merge refuses up front and NO inventory build begins. `--budget` is unchanged for the inner tier.
- Tiers execute as prerequisite-closed sets of steps, and a prerequisite that runs in both tiers discharges its keys in exactly one of them.
- A merge run refuses a handoff whose source identity or SOURCE model hash does not match the inner run that produced it, and says which of them disagreed. A plan digest that disagrees is REPORTED, not refused - see the work-set rule above - and so is a `model_hash` that differs while the source model hash agrees, which is artefact discovery having moved and not the system under test.
- BOTH TIERS bracket their own work with the effective-tree identity. The inner run takes it before it plans, revalidates after its last step, and refuses to emit a usable handoff if it moved; merge recomputes it after the inventory phase and after the last step and refuses completion if it moved. MERGE ALSO BRACKETS THE PINNED REVISION: `HEAD` must still resolve to the id it captured, at both points. All of them name the two values that disagreed. Two regressions: an edit to a `.rs` file mid-run - which leaves the source model hash untouched - driven against the INNER tier and against MERGE, neither of which may report its work as complete; and `HEAD` moved mid-run to a DIFFERENT-PARENT, SAME-TREE commit whose delta selects extra keys, where every digest still agrees and merge must refuse rather than complete.
- `INCOMPLETE`, `SHADOW` and `STALE` keep their distinct non-zero statuses and outrank a tier's own verdict. A regression proves that a change which selects a full plan cannot report `FULL`, or exit as complete verification, from an inner-only run.
- One genuinely scopeable host-tool change is run through inner and then merge, and every key the inner tier deferred is observed reappearing in merge. The tools that define or judge the system still select everything.
- Every guest-booting gate in the catalog is inventoried with its own terminal predicate, and the watcher has negative fixtures - early success marker then panic, then reset, then retraction - that it must fail.
- The merge tier's normal entry point, invoked with no flags, is observed to boot the two emulated guests concurrently.
- One reducer decides admission, transition and cause; the arms carry effects and no decision. It is consulted ONCE in `advance` before the per-event `match`, and the `match` is over WHAT IT RETURNS, so the admitted event exists only inside the admitted variant - discarding or inverting the answer does not compile. Its classification is exhaustive over `BindingEvent` with no wildcard.
- The admitted variant carries the whole decision - the event, the state the record moves to, and for an ending attempt the `FailureCause` and whether the ending is a planned stop - and `advance` CONSUMES all of it. No arm calls `move_to` with a state of its own choosing, writes a `FailureCause::`, or re-derives the planned-stop flag.
- A source check asserts the dispatch is on the unconditional path before the `match`, that the `match` scrutinee is the binding the dispatch produced, that no terminal arm carries a state predicate of its own, and that no arm recomputes a field the decision already carries. The check itself fails against each of FIVE mutated source copies: the dispatch deleted, the dispatch moved into an arm, its result bound to `_`, a state predicate reintroduced into an arm, and an arm recomputing a carried `move_to` or `FailureCause`.
- `signed-boot` is measured again after the per-case predicates and their negative fixtures exist, against the 2 694 s baseline. The 14 s figure is recorded as a rejected experiment and is not the comparison.
- The measured inner loop for a service change is minutes, and the merge tier is tens of minutes - stated as measurements in this file, against the numbers at the top.

## What this milestone deliberately does not do

- It does not drop a target. Measured on 2026-09-04, the emulated targets are 39% of a full sweep and x86_64 is 3%; after TIERING alone they are 0% of the inner loop, because the inner loop does not boot them. The earlier version of this line said "after tiering and narrowing" and quoted a merge share of about 9% - that figure came from the tag-based narrowing this plan withdrew, so it is withdrawn with it. What the tiers move is WHEN those targets run, not how much of them runs. And the `TimedOut` defect they exposed was a kernel-lifecycle bug that would have stayed in the tree without them.
- It does not weaken an assertion to make a run finish. Where a check cannot be made fast, it moves to a later tier; it does not become optional.
- It does not rewrite the selection model. `covers`, ownership rules and architecture rows stay as they are; what changes is the escalation policy sitting on top of them.
````


AUDITOR'S RE-AUDIT OF PLAN P02M0177 (2026-09-05T10:43:26Z):

Rating: 10/10

Reviewed the complete audit history, including the archived plan and the readability revision,
and verified the planner's responses against the active plan and relevant repository code.

No unresolved material issues remain within this milestone's scope. The current plan is ready
for implementation.


IMPLEMENTER'S RESPONSE ON P02M0177 (2026-09-05T12:51:01Z):

Reviewed the complete audit history, planner responses and active milestone against the actual implementation. The responses below address all 38 numbered findings individually, the two clean re-audits and the additional readability-review requirements. Accepted implementation corrections are now present; rejected or superseded remedies are identified explicitly.

Validation is not fully complete: the IOMMU traffic case still fails, its baseline runtime comparison could not be built, and the matched performance protocol remains unperformed. The passing checks and these limits are recorded below. The active plan remains unchanged.

**Audit 2026-09-04T15:08:13Z**

1. **ACCEPTED — missing causal regressions and wrong host seam.** `driver_binding::reduce_event` now returns the admission, transition, cause and planned-stop decisions used by `DeviceManager::advance`. Binding tests drive both timeout/READY orders and compare the independently derived production probe and handoff orders (`sort_probe_slots`, `next_handoff_slot`). `system_manifest::service_manifest_source` is shared with `services/core/build.rs`; `generated_volume_services_wait_for_storage_despite_name_order` reads the real manifest, parses those generated dependency rows and drives the same `service_logic::service_lifecycle::next_startable` used by `service_manager`. Negative mutations remove the timeout guard, probe ordering or actual manifest dependencies. Effects remain in production; the regression's shared seam is the complete pure decision, as clarified by the later audits.

2. **ACCEPTED — the original event-order oracle was wrong.** `a_queued_timeout_cannot_disconnect_an_already_ready_driver` requires READY followed by the queued same-generation timeout to preserve Online; `a_ready_that_arrives_after_the_deadline_does_not_undo_the_timeout` requires the reverse order to preserve teardown. Both consume complete production reducer decisions. **REJECTED — the superseded requirement to shorten a deadline or add a timing profile.** Deterministic event injection and a guard-removal mutation directly exercise the defect; a new guest/profile/clock seam would add machinery without improving this oracle.

3. **ACCEPTED — tags cannot justify coverage reduction.** `verify-model/src/tier.rs::partition` partitions the exact keys already selected and lowered by the existing planner and `commands::steps`; `closed_steps` preserves dependency closure. It changes ownership between inner and merge, not `covers`, architecture policy, shadow activation or the selector's tag rules. Missing-binary whole-suite fallback remains owed alongside newly discovered keys.

4. **ACCEPTED — tier names alone did not define an executable lifecycle.** `verify.sh` now runs inner by default and accepts `--merge FILE`; the Rust `tier-inner`, `tier-inner-finish`, `tier-merge-prepare`, `tier-merge-commands`, `tier-merge-finish` and `tier-level` seams provide the serialized handoff, revision checks and accounting. Inner completion always owes revision-side merge, including empty D; SHADOW and STALE retain their stronger nonzero outcomes. The scopeable `image-bench` fixture traverses `begin_inner` through `finish_merge`, including deferred obligations. System-defining tools still retain their registry escalation. Exhaustive release/sweep remain independent; this local handoff does not claim P02M0170's separate immutable-evidence implementation.

5. **ACCEPTED — a generic early marker weakened existing assertions.** `src/tools/guest-verdict.py::CASES` and `verdict` now use case-specific complete signals. Altered payload and pairing/bootstrap cases continue beyond intermediate loader lines; downgrade fallback requires both its warning and successful loading. IOMMU health/traffic cases retain their complete 120/300-second observations and reject later panic/reset/retraction/restart/fault; silent Secure Boot refusals retain their full backstop. `docs/verification/P02M0177-guest-cases.md` inventories all guest gates, profile rows and concurrent selections; `check-guest-verdict.py` exercises positive-then-negative histories against the production verdict function. Host fixtures establish predicate behavior, not a measured complete guest-gate speedup.

6. **ACCEPTED — warming/concurrency promises lacked an executable policy.** Normal `verify.sh --merge FILE` now defaults to two slots, respects an explicit `--jobs`, and uses the existing scheduler and build barriers. `check-verify-scheduler.sh` checks the default without supplying `--jobs 2`; producer acquisition is covered by the contention regression described below. **REJECTED — adding a scheduled cache warmer.** That deliverable was explicitly withdrawn from the active milestone; no CI/timer or second scheduler is introduced. Real cold/warm timings remain a separate acceptance obligation and are not inferred from scheduler fixtures.

**Audit 2026-09-04T16:11:14Z**

1. **ACCEPTED — 14 seconds was an invalid optimization result.** The active milestone and new guest-case inventory identify 2,694 seconds as historical measured signed-boot duration and the 14-second global-marker experiment as rejected. The new predicates retain the later assertions that experiment truncated. No new speedup is claimed from host tests; any reported post-change timing must come from the complete safe gate under the specified measurement conditions.

2. **ACCEPTED — host tests also need a production-wiring oracle.** `check-driver-event-dispatch.py::check` verifies the actual `advance` dispatch, admitted payload, refusal control flow and carried-field consumption, and its source-copy mutations prove that bypasses are rejected. The gate is registered in `check.sh` and the verification catalog. **REJECTED — the earlier proposed guest configuration as the required remedy.** Later review correctly withdrew that timing-dependent design in favor of deterministic reducer and production-wiring checks; no new guest configuration is required by the active milestone.

3. **ACCEPTED — release could not be both exhaustive and a disjoint remainder.** The disjoint partition is I and D only, enforced by `partition` and `Handoff::validate`. Existing flat `--release` and `--sweep` dispatch stay outside that partition and retain their independent exhaustive execution. Their successes are not relabeled as per-key tier evidence.

4. **ACCEPTED — prerequisites, identity, handoff and status composition were missing.** `closed_steps` retains every required producer while stripping keys owned by the other share, so a prerequisite can rerun without double-accounting. The handoff contains the complete original plan, key digest, exact I/D sets, source identity and individual inner outcomes; validation refuses overlap, missing evidence and failure. `prepare_merge` binds it to the pinned proposed commit, and completion requires all remaining merge obligations. The shell preserves failure, INCOMPLETE, SHADOW and STALE precedence; an inner share never reports FULL simply because its original selection was full.

5. **ACCEPTED — a demonstration flag did not establish normal concurrency.** `verify.sh` sets the default to two when parsing ordinary merge mode, leaving explicit caller overrides authoritative. `check-verify-scheduler.sh` exercises that default through the production scheduler with a prepared step fixture; flat release/sweep behavior remains unchanged. Actual overlapping guest runs and warm-cache performance must be reported as their own evidence, not inferred from the fixture.

**Audit 2026-09-04T18:19:08Z**

1. **ACCEPTED — inside/outside-deadline guests did not generate the stale event.** The replacement host test presents the already-generated timeout after READY directly to the production reducer. `check-driver-event-dispatch.py::regression_mutations` removes that reducer's timeout admission guard in a temporary crate and requires the specifically named test to fail its assertion; a compilation failure cannot satisfy the mutation gate. Production dispatch wiring is checked separately.

2. **ACCEPTED — contradictory three-tier acceptance and unsupported narrowing.** Current execution has the exact I/D partition with mandatory revision-side reconciliation; release remains an independent exhaustive gate. Tests use a genuinely scopeable host tool and inspect every selected/deferred key rather than accepting a documentation-only shortcut. No hand-picked tag list removes selected architecture variants.

3. **ACCEPTED — invocable modes, fail-closed completion and a commit-stable identity were absent.** The new shell/CLI lifecycle seals inner evidence and requires merge at `parent(REV)..REV`, with per-key outcomes and explicit retirement accounting. `effective_tree` hashes a canonical complete disk tree and `commit_tree` hashes the same path/mode/content representation from Git objects. Existing `shadow::source_digest` and `Model::model_hash` remain separate identities; neither is repurposed as the dirty-to-commit handoff identity.

**Audit 2026-09-04T18:43:32Z**

1. **ACCEPTED — the proposed guest mutant was not distinguishing.** The implemented queued-timeout host oracle fails when its production reducer guard is deleted, while the reverse-order control remains explicit. Five structural source mutants additionally verify that production reaches effects only through an admitted, consumed reducer decision. The withdrawn inside/outside guest timing comparison is not reported as acceptance evidence.

2. **ACCEPTED — changed-path bytes were not a complete source identity.** `effective_tree` enumerates tracked and non-ignored untracked paths, hashes unambiguous raw path bytes, Git-compatible file modes and full content/symlink targets, and represents deletions by absence. Tests cover additions, deletions, renames, symlinks, executable modes, newline/non-UTF-8 names and changed ordinary files. **REJECTED — blanket rejection solely because a base changed.** A different effective tree is refused; the same final tree on another parent is accepted only after its pinned delta is replanned and any additional obligations pass, as required by the later correction.

**Audit 2026-09-04T19:43:14Z**

1. **ACCEPTED — dead-code warnings and predicate absence did not prove dispatch.** `advance` has one scoped reduction after the teardown redirect and before its effects. Only `EventDecision::Admitted` provides the effect event and carried fields; refusal continues the loop. `check-driver-event-dispatch.py` checks this structure and rejects deleted, displaced and discarded dispatches. No proof relies on a public helper becoming dead code when one caller is removed.

2. **ACCEPTED — tracked/index-only identity omitted supported inputs.** `effective_tree` reads the disk bytes consumed by execution for the deduplicated `git ls-files -co --exclude-standard -z` path set, not index blob contents. Dirty additions survive an ordinary commit without changing the digest. Tests distinguish staged A from tested disk B, cover additions and their dirty-to-commit transition, and verify that committing A while B remains on disk cannot authorize merge for A.

**Audit 2026-09-04T20:30:53Z**

1. **ACCEPTED — the reducer boundary and result-use proof were inconsistent.** The shared pure decision now carries `event`, `next_state`, `cause` and `planned_stop`; production effects consume those fields. The source checker requires the exact admitted-result binding, refusal `continue`, admitted-event match and carried transition/cause/planned-stop consumption. It rejects the explicit discarded-result/raw-event mutant as well as dispatch removal, displacement and competing arm-local decisions; raw `popped` is scoped out before effects.

2. **ACCEPTED — matching disk snapshots did not establish the proposed commit.** `prepare_merge` requires the handoff digest, current disk digest and `commit_tree(REV)` digest to agree. `partial_staging_and_every_post_inner_tree_mutation_are_refused` covers staged A/tested B/committed A and ordinary later mutations; `ordinary_dirty_commit_transitions_are_accepted_by_merge` preserves the valid commit-of-tested-bytes path.

**Audit 2026-09-04T20:56:31Z**

1. **ACCEPTED — a raw-event-only action omitted required decisions.** `driver_binding::reduce_event(state, event)` owns admission, resulting state, failure cause and clean-stop classification; generation/arbitration remain outside this seam. `DeviceManager::advance` consumes the complete admitted payload and no longer re-derives terminal transitions or causes in its arms. Host assertions cover whole decisions, and the arm-local-transition mutation proves the source gate rejects a second decision.

2. **ACCEPTED — merge lacked a proposed revision and reconstructible handoff.** `prepare_merge` captures the exact `HEAD` object ID as REV, requires one parent, reads that fixed parent-to-REV delta and validates the tested tree against REV. `Handoff` serializes the complete original `Plan`, canonical key digest, exact I/D partition and inner outcomes, so the digest has a checked preimage. Live replanning adds uncovered obligations rather than attempting to recover dirty paths from a clean worktree. Root and multi-parent commits are explicitly refused by the current milestone's single-parent contract.

**Audit 2026-09-04T21:48:04Z**

1. **ACCEPTED — mutable cost history can change membership. REJECTED — freezing history or redesigning membership is necessary.** The active correction intentionally reconciles live selection: `reconcile` retains every still-existing deferred key and adds keys selected outside the original partition. Plan/hash drift is diagnostic when source identity agrees. `live_history_crosses_the_escalation_threshold_both_ways_including_inner_step_recording` deterministically crosses the existing 0.9 threshold in both directions and records actual inner steps through the history API; history contraction does not erase D. This fixes the defect without changing the existing cost model or introducing a frozen-history subsystem.

2. **ACCEPTED — the fifth decision-recomputation mutant was omitted.** `check-driver-event-dispatch.py::rejected_mutations` now contains all five active mutants, including an arm-local transition replacing the reducer's carried next state. The production checker also requires use of the carried cause and planned-stop value. The definition is implemented as a registered gate rather than relying on a prose count.

**Audit 2026-09-05T00:19:54Z**

1. **ACCEPTED — contracted-away deferred work was still owed.** `reconcile` starts from `D ∪ (P1 − P0)`, removes only separately justified refreshed-inventory retirements, then adds genuinely new P1' keys. Existing `D − P1` is retained even when history contracts the live selection. The bidirectional threshold regression explicitly asserts those keys remain merge work, and final outcome checking cannot complete with an owed key missing.

2. **ACCEPTED — binary discovery made the existing model hash unstable across valid handoffs.** `Model::source_model_hash` now hashes selector source/configuration, graph/features/architecture risk, a source-built non-kernel catalog and all directly scanned kernel test IDs/coverage. Binary-dependent `model_hash` remains unchanged for its existing consumers and is diagnostic in the tier handoff. Missing/stale/refreshed-binary fixtures verify stable source identity while actual new architecture keys become merge obligations; genuine source-model disagreement is refused.

**Audit 2026-09-05T00:36:26Z**

1. **ACCEPTED — accepting inventory drift alone could miss new emulated tests.** Merge prepares every relevant target with the direct repaired build-only seam before loading the final model and lowering executable work. `merge_commands` requires descriptor-bearing refreshed inventories and reconciles P1' against both P0 and P1. The stale-port/x86 regression creates real descriptor-bearing ELF fixtures, discovers them through production discovery, requires each new applicable target key and refuses completion until each has an individual passing outcome. These fixtures prove discovery/accounting, not actual emulated execution.

2. **ACCEPTED — empty D could skip required revision-side selection.** A successful ordinary inner execution exits INCOMPLETE (6) even with no deferred keys, unless the preserved SHADOW/STALE policy returns its own nonzero status first. Merge is still required to bind REV and replan its delta. The same-final-tree/different-parent regression starts with empty D and requires the newly selected work; a separate same-tree case with no extra keys completes after reconciliation.

**Audit 2026-09-05T01:07:18Z**

1. **ACCEPTED — the static executor had no post-build replanning point.** `verify.sh` now runs `tier-merge-prepare`, directly performs the requested target inventory builds, and only then calls `tier-merge-commands` to refresh, reconcile and produce one final STEP file for the existing executor. `test-kernel.sh --build-only` returns after the locked `cargo build --tests` and copy. Ordinary build lowering and the scheduler remain static; no mid-run graph mutation or duplicate executor was introduced.

2. **ACCEPTED — stale inventory can contract as well as expand.** `reconcile` retires a previously known kernel-test variant only when its architecture inventory was successfully refreshed, the source model agrees, and the variant is now absent. Retirement is distinct from pass and applies to provisional extras as well as D. The contraction regression covers both origins and retains still-existing work omitted only by cost contraction. Unknown configuration and unrelated lowering errors remain refusals.

3. **ACCEPTED — shell exit zero exposed completion before merge.** After sealing an inner handoff and applying existing evidence policy, `verify.sh` unconditionally returns 6 for inner. `--allow-shadow` does not turn that incomplete share into overall success. A completion message belongs only to merge after revision/outcome and trust/freshness checks.

**Audit 2026-09-05T01:33:48Z**

1. **ACCEPTED — inventory work bypassed the budget and needed the direct seam.** The shell rejects `--merge ... --budget ...` before planning or inventory; the Rust merge preparation command independently rejects it. The scheduler/CLI regressions include zero and insufficient budgets and verify no inventory producer starts. Merge calls `src/harness/test-kernel.sh ARCH --build-only` directly, avoiding top-level system-volume prerequisites. **REJECTED — this audit's factual claim that the old flag already implemented compile-only execution.** The later 03:15:07Z audit correctly found that its unused `TEST_ARGS` did not stop QEMU; that real harness defect is repaired as described below.

2. **ACCEPTED — retirement conflicted with all-passed accounting.** `reconcile` covers the entire provisional `D ∪ (P1 − P0)` set, records unavailable refreshed kernel variants separately in `retired`, and lowers/checks outcomes for the remaining work plus new P1' keys. `finish_merge` requires the sealed inner outcomes and every current merge key to pass; retired keys are reported as retirements rather than synthesized passing evidence. Its CLI prints `DISCHARGED` for this accounting result, leaving the shell's completed-verification message until evidence policy accepts the run.

3. **ACCEPTED — merge's mutable work window needed final identity checks.** `merge_commands` revalidates the effective tree, pinned HEAD and source-model identity after inventory preparation; `finish_merge` repeats the complete-tree and HEAD checks after execution. That final tree check also covers persistent edits to selector/model source files. Tests persist an ordinary source edit that does not alter the narrower source-model hash and require refusal. These are the milestone's minimum movement checks, not a claim of immutable execution or prevention of every transient change-and-restore race.

**Audit 2026-09-05T03:15:07Z**

1. **ACCEPTED — `--build-only` still reached QEMU.** `src/harness/test-kernel.sh` now exits successfully immediately after locked test compilation and staging when BUILD_ONLY is set; the unused `TEST_ARGS` path was removed. `check-guest-verdict.py::KernelBuildOnly` drives the actual script in an isolated workspace with a fixture compiler producing discoverable ELF descriptors, no system-volume artifacts and a runner that detects forbidden QEMU entry. Removing the early return makes that regression fail. A separate genuine cold x86_64 build also passed in 27.649 seconds in a disposable live-source snapshot with no repository build outputs or shared compiled artifacts. Real `nm` found 380 test descriptors; QEMU was never invoked and no boot medium or system volume was created. Toolchains and downloaded dependencies remained available. Evidence is in `.build/logs/p02m0177/cold-build-only/`. This is inventory-compilation evidence, not the service-change performance benchmark.

2. **ACCEPTED — changed-base acceptance needed one coherent rule.** A different effective final tree is refused. A new single-parent commit with the same final tree is accepted only after planning its pinned parent delta and discharging any additional work, whether D was empty or non-empty. Tier tests cover both additional-work cases and a different parent whose delta adds no new keys; the obsolete blanket base-change refusal is not implemented.

3. **ACCEPTED — inner evidence also needed a before/after source bracket.** `tier-inner` captures `effective_tree` before `Model::load` and selection/execution; `finish_inner` recomputes it after outcomes and refuses to seal a usable handoff if it changed. The ordinary-source-edit regression covers this path even when `source_model_hash` stays equal. Merge's independent checks remain in place.

**Audit 2026-09-05T03:34:00Z**

1. **ACCEPTED — a stable tree did not mean HEAD stayed at the reconciled revision.** `prepare_merge` pins REV and its single parent, uses those exact object IDs for delta selection and commit-tree validation, and stores them in `MergeState`. `check_snapshot` compares current HEAD with pinned REV after inventory and after execution, reporting both IDs on movement. `head_movement_to_a_different_parent_same_tree_is_refused_at_both_merge_boundaries` proves that unchanged final bytes cannot hide a changed proposed delta mid-run.

**Re-audits 2026-09-05T09:51:10Z and 2026-09-05T10:43:26Z**

Both contain **no material findings** and rate their reviewed plan 10/10. **ACCEPTED as statements of plan readiness, not as implementation or performance evidence.** There is no individual correction to apply solely because of either rating. The intervening archived milestone remains untouched and is not treated as a new active specification.

**Additional source-review requirements in the 2026-09-05 readability revision**

1. **ACCEPTED — source hash must include all source declarations, including rows absent from every binary.** `Model::source_model_hash` calls `kerneltests::scan_source` directly, sorts the complete `(id, covers)` declarations and hashes them independently of compiled variants. The missing/first-build/stale inventory tests exercise that distinction; merely stripping variants from the existing discovered catalog was not used.

2. **ACCEPTED — the startup regression must reproduce the actual missing manifest edges.** The system-manifest test loads the production manifest, uses the shared production generator and startup helper, and requires `iso_storage`, `media_storage` and `udf_storage` to start after `storage_service` despite name order. Its negative manifest copy removes those three edges and violates the same oracle. The already-correct scheduler was not redesigned, and the existing manifest dependencies were not changed merely to make a synthetic fixture pass.

3. **ACCEPTED — signed-boot copied a shared producer output outside its lock.** New `build-loader-private.sh` gives both signed fixture profiles one Cargo target under their common private output directory and holds `kernel-test-build.lock` across each build and copy. It preserves the ordinary loader instead of rebuilding/restoring it; real profile cycling changed its PE identity even after restoring test trust. Signed-boot uses only the private copies. Secure Boot acquires its unsigned loader under the same lock and keeps signed output/variable templates private. `LoaderContention` forces a competing producer at the former gap and distinguishes the protected sequence from the old unlocked-copy mutant. The signed missing-bootstrap fixture also uses a minimal `mkpackages --output-dir` option so overlapping gate work cannot rewrite the ordinary shared volume.

4. **ACCEPTED — inventory must include I's targets and the gate inventory must include profile/concurrent rows.** `inventory_targets` unions I, D, P1 and their build/boot policies, including inner-owned x86_64 even when D consists only of ports. Its acceptance fixture introduces a new x86 descriptor after the inner share and requires the new key. `P02M0177-guest-cases.md` covers all eight ordinary booting gates, sixteen profile rows and `concurrent-selection`; the host inventory regression checks catalog/watcher coverage.

5. **ACCEPTED — `BindingEvent: Copy` is not consumed merely by a call.** `advance` scopes the original `popped` binding inside the decision block; only the admitted payload is available to the effect match. The source checker verifies scope and control flow explicitly and rejects raw-event bypass, rather than relying on move semantics or dead-code linting.

**Additional defects reproduced during implementation verification**

1. **ACCEPTED — a manifest refusal can end at a final halt without a panic-handler line.** The first real Secure Boot run reached `read_pairing`'s final FATAL refusal while the new watcher waited for a nonexistent panic. `guest-verdict.py::MANIFEST_END` now also accepts that exact complete final halt, only for the relevant medium-manifest/context cases. Earlier reasons and unrelated FATAL lines still cannot pass; host fixtures prove the distinction. The corrected full Secure Boot gate passed all four cases with both 120-second silence windows retained. The original failed attempt is preserved.

2. **ACCEPTED — perf-anchor rewrote the shipping ISO and its receipts.** Its direct harness boot assembled a shipping-named ISO using a private loader basename. That changed the published input key even with identical loader bytes and reproduced an IOMMU preflight refusal. `check-perf-anchor.sh::boot_with` now sets `LIBER_IMAGE_OUTPUT` to its own per-profile ISO; the narrow `mkimage.sh iso` override keeps that image, candidate and receipts private. Default shipping output and existing input-key logic are preserved. `PerfImageIsolation` exercises the actual caller and producer routing and rejects removal of either the caller opt-in or producer support.

3. **ACCEPTED — FAT timestamp normalization mutated the shared Cargo loader.** `mkimage.sh` backdated the original loader, causing the next Cargo build to relink it and change its PE timestamp/debug identity. Comparing the shipped and current loader proved the changed loader was the sole differing image-key input. `stage_loader` now makes a cleanup-owned temporary copy for timestamp normalization in both ISO and IMG staging; the shared loader is untouched. The regression executes both production staging blocks with real mtools, checks the FAT epoch plus original bytes/nanosecond mtime and cleanup, and rejects the old direct-stamping mutation.

4. **ACCEPTED — restoring a shared loader's trust profile did not restore its exact identity.** A real successful test-trust/external-release cycle changed the ordinary loader's SHA256 twice. Both signed fixture profiles therefore use the same run-private Cargo target in `build-loader-private.sh`; dependencies are reused within that gate, and no ordinary-output restoration is needed. `LoaderContention` verifies both profiles preserve the ordinary loader and rejects reintroduction of shared targets and the old unlocked-copy sequence. This is the same required M5 producer isolation, not a linker or cache-key redesign.

**Verification and limits**

- Host suites passed: driver-binding 69 tests, service-logic 33, system-manifest 16 and mkpackages 1. The full verify-model suite passed 139 tests; the final 18 tier regressions passed after the last test-only additions. These cover commit-stable identities, staging/movement refusal, source/inventory drift, live-history expansion/contraction, retirement, per-key outcomes, shared prerequisites and evidence-status composition.
- Registered gates passed: driver-event-dispatch (all five structural mutations and both causal Rust mutations), guest-verdict (final 12 tests, including actual mtools staging and producer-isolation mutations), verify-scheduler, one-wait, no-fixed-provider-slots and source-hygiene. Changed shell scripts passed syntax/format checks; the final diff passed whitespace validation.
- A disposable live-source repository exercised the actual CLI: docs-only inner produced an empty usable handoff and returned 6; committing the tested bytes preserved the identity; ordinary merge returned 0. Moving HEAD to a different-parent/same-tree commit immediately before final merge validation produced refusal status 3. No commit was made in the main checkout. Evidence: `.build/logs/p02m0177/cli-lifecycle/`.
- `./build.sh --arch all` passed for x86_64, aarch64 and riscv64, and the shipping ISO was rebuilt normally. A genuinely cold direct x86_64 build-only run produced 380 descriptors in 27.649 seconds without QEMU or pre-existing repository build outputs. It is separate from the service-change benchmark.
- The production default merge scheduler, exercised through its existing prepared-step test seam with no `--jobs` override, ran real aarch64 and riscv64 boot suites: 13 tests passed on each, their QEMUs overlapped for 194.217 seconds and the scheduler returned 0. Each suite published its own result logs. This proves actual default scheduling/guest overlap; the checked handoff lifecycle is separately tested above. Evidence: `.build/logs/p02m0177/merge-port-overlap/`.
- Final real perf-anchor passed both cases in 43.027 seconds. Corrected Secure Boot passed all four cases in 248.441 seconds, retaining both complete 120-second silent-refusal windows. Full signed-boot passed all 16 cases in 275.819 seconds, including twelve x86 cases and both clean/refusal cases on each port. After final perf and signed runs, the ordinary loader, shipping ISO and both receipts retained their SHA256, sizes and nanosecond mtimes; the canonical input key still equalled the stored key. Earlier failed attempts and the input-drift reproductions are preserved in `.build/logs/p02m0177/`.

- **IOMMU traffic validation remains unresolved.** The final quiet full gate returned 1 after 325.819 seconds: DMA passed all 33 tests and six hostile checkpoints, but the ordinary traffic case exhausted its unchanged 300-second observation without DHCP. The incident captured `Binding`, generation 1 and last opcode 0. The interleaved driver-owned `online` message occurs before that driver sends OFFER/READY; it is not evidence that DeviceManager accepted READY, became Online and then consumed a stale timeout. The old and new dispatch both admit a timeout in Binding, and the pump/deadline logic and traffic QEMU arguments are unchanged. This trace therefore does not establish recurrence of M1's stale-timeout defect, and neither host-load causation nor a pre-existing runtime failure is claimed. No deadline/profile/acceleration adjustment or weakened assertion was used to turn it green.
- The full gate stops at that failure. Its later default-machine and explicit no-IOMMU production blocks were consequently run separately, with their exact QEMU commands, original assertions and complete 120-second observations; they passed in 120.295 and 120.280 seconds. These separate passes do not relabel the full gate as passed. Evidence: `.build/logs/p02m0177/qemu-virtio-iommu-x86_64.{json,log}` and `iommu-remaining/`.
- An isolated clean checkout of pre-implementation revision `155ee46ea056307140558f482e7360a71a683ed6`, using independent copied build outputs, attempted a baseline traffic control. Its normal ISO build failed after 378.878 seconds on a `lico` import with no declared provider; one supported targeted rebuild then failed after 7.490 seconds with a rustc SIGSEGV. No baseline guest launched and no baseline source was edited. Runtime regression attribution is therefore still unproven; final validation is not fully green. Commands, revision, failures and the retained checkout are recorded in `.build/logs/p02m0177/iommu-baseline-control/summary.json`.

**Performance acceptance remains open.** The complete signed-boot result above can be compared with the historical 2,694-second reference only as separately recorded observations; the rejected 14-second experiment is not a baseline. The active milestone's matched pre/post service-change protocol (the exact log-service journal-limit patch, three restored warm repetitions and one cold repetition per revision, controlled history/trust and otherwise idle conditions) was not run. No minutes/tens-of-minutes workflow target, matched speedup or completed performance item is claimed. The host outcome fixtures also are not presented as a real stale-inventory guest experiment. The active plan and its unchecked performance requirements remain unchanged.


IMPLEMENTER'S RESPONSE ON P02M0177 (2026-09-05T17:42:56Z):

This follow-up verifies and resolves the runtime-validation limit in the response at 2026-09-05T12:51:01Z. The individual dispositions of all 38 historical findings, two clean re-audits and the additional requirements remain recorded there; correctly resolved findings are not repeated. The current implementation passes the complete IOMMU gate. Performance acceptance remains open, with actual preparation now running in the pinned disposable workspaces.

1. **ACCEPTED — the unresolved traffic failure required a causal diagnosis and an original-runtime control.** The original runtime reproduced the failed initial handshake after its build prerequisite was made executable; its `Binding`/last-opcode-zero trace agrees with the current failure. Temporary instrumentation confined to an isolated checkout captured first wait tick 3174, incident deadline 3411 and teardown reserve 333: the spendable deadline was already 3078. `Incident::open` clamped the deadline but reserved teardown against the larger nominal duration. It now samples the opening clock once and calls `driver_binding::IncidentWindow::teardown_reserve(deadline, opened, share)` with the actual granted duration. The existing absolute deadline, per-attempt limit and one-third reserve fraction are unchanged. The new named host regression requires reserve 79 for the captured case and preserves ordinary unclamped behavior; all 70 driver-binding tests passed. An explicit old-arithmetic mutation compiled and failed that assertion. Files/functions: `src/user/libs/driver/binding/src/lib.rs::IncidentWindow`, `src/user/libs/driver/binding/src/tests.rs::a_late_initial_bind_reserves_teardown_from_its_clamped_window`, and `src/user/services/core/src/device_manager.rs::Incident::open`. Evidence: `iommu-diagnosis/diagnosis.json`, `reserve-host-tests.json` and `reserve-negative.json` under `.build/logs/p02m0177/`. This is a distinct pre-existing budget defect, not evidence that M1's admitted Online binding consumed a stale timeout.

2. **ACCEPTED — subscription reply ordering exposed a second actual startup race.** With only the reserve correction, DeviceManager published one network provider and retained one endpoint/five mappings/zero faults, while NetworkService nevertheless reported no provider. `open_subscription` sent the consumer endpoint before `Catalogue::subscribe_stream` queued the snapshot; `NetworkService::take_published_nic` immediately polls that returned endpoint. `open_subscription` now queues/registers first and still sends the reply if registration was refused, allowing the caller to observe the already-closed producer. On a failed reply, the syscall restores the untransferred capability; the function closes its consumer and uses the existing `reap_dead_subscribers` cleanup. The shipping/development catalogue bounds are 9/10 providers, below the default channel capacity of 64 messages; these snapshots contain no transferred capabilities and queue without blocking. `src/tools/check-driver-event-dispatch.py` rejects both reply-before-snapshot and refusal-without-reply mutations, alongside its original five structural and two actual Rust mutations. Both independent runtime controls then reached DHCP with unchanged direct-TCG arguments: current 141.325 seconds and original-derived baseline 161.114 seconds. Those deliberately bounded controls remain diagnostic evidence, separately from the full gate below.

3. **REJECTED — an unconditional `programs.lico` CLI-provider addition was not a correct final build repair.** It satisfied the actual original missing-import error, but a fresh main rebuild correctly rejected the same edge as unused. A supported full targeted baseline rebuild retained a CLI-owned `RawVec<Vec<u8>>::grow_one` specialization while the current object used the Lico-owned specialization. This therefore was not cured merely by refreshing copied caches, and relaxing the exact-provider validator would hide the defect. The temporary edge has been removed from both comparison revisions; `src/user/services/manifest.toml` has no resulting main-checkout change. Both contradictory objects/import lists and the original failures are retained in `iommu-final-corrected/lico-owner-comparison.json` and the associated build logs.

4. **ACCEPTED — a minimal, stable build prerequisite is necessary for the prescribed separate-workspace measurements.** In `src/user/apps/tools/src/lico.rs`, `Manager::names_of`, `Manager::run_operation` and `Manager::walk_into_directories` now append five already-reserved owned values with `extend([item])`. The pinned allocator implementation uses the known-length extension path, avoiding the unstable `grow_one` import entirely. Existing reserve-failure handling, moved values and ordering are preserved; no feature, unsafe block, abstraction, compiler-policy change or provider-validation relaxation was introduced. The adjacent inaccurate ownership comment was corrected. Strict targeted builds passed independently with the original provider list in the diagnostic checkout (92.784 seconds) and baseline (93.114 seconds); neither object imports the unstable specialization. This small adjacent correction is justified by the reproduced build blocker, not a change to file-manager behavior. Evidence: `iommu-final-corrected/common-lico-reserved-extension.patch` and `lico-extension-proof.json`.

5. **ACCEPTED — the broader M5 runtime-correctness item could not be closed by partial or shortened runs.** The final ordinary main ISO build passed in 239.784 seconds. The first gate attempt correctly refused a stale, separate test-volume receipt before any guest launched; the supported x86_64 build refreshed that prerequisite in 23.321 seconds. The separately retained complete gate then passed with exit 0 in **566.984 seconds**: **33 DMA tests**, all hostile checkpoints including forced release, and the full **300/120/120-second** traffic/default/plain observations. DHCP remained available; default/plain startup and the original display/frame assertions passed. Source bytes, the ordinary loader, shipping ISO and both receipts retained their SHA256, sizes and nanosecond mtimes; the canonical input key was preserved. No diagnostic instrumentation is in main. The authoritative result is `.build/logs/p02m0177/iommu-final-corrected-attempt2/gate.json`, with `gate.log` and retained `serial/` logs. Earlier compiler, provider, preflight and runtime failures remain intact. Together with the already-recorded complete Secure Boot/signed-boot/perf-anchor runs and deterministic producer-contention proof, this resolves the recorded M5 runtime-validation limit.

**Measurement provenance and remaining acceptance.** The current comparison snapshots are before `10eaaf8bef9416c0377b13b7563e0b828f6c85cf` and after `7ac5b8678a1721a1864dee13cf557624964bc9aa`. Both contain the same proven reserve, subscription and Lico build prerequisites. Before retains the pre-M0177 reducer, verifier, guest-termination and producer behavior; the original `155ee46ea056307140558f482e7360a71a683ed6` and its failures remain identifiable. Exact common patch hashes and superseded CLI-edge snapshots are recorded in `.build/logs/p02m0177/common-prerequisites-provenance.json`; the earlier setup was retained before repinning. The pinned compiler remains nightly-2026-06-16 with the same recorded 512 MiB compiler-stack setting on both sides. A main compiler crash at that setting and its full stderr are retained; the supported targeted rebuild subsequently compiled successfully, so no unrecorded compiler change is claimed. Snapshot commits are isolated; main HEAD and index were preserved.

The plan checklist now records **17 complete items and 3 open items**. The matched three-warm/one-cold service-change protocol, actual workflow obligation completion and matched complete signed-boot measurement remain open. Only authentic preparation has begun: pinned external tooling was fetched and verified through the supported bootstrap command, and the initial baseline all-target build is running. No trust certificate, passing benchmark, speedup or completed performance item is inferred from that preparation. Actual measured results must still be entered in the plan and appended here before milestone closure.


IMPLEMENTER'S RESPONSE ON P02M0177 (2026-09-05T17:56:13Z):

**ACCEPTED — correction to the validation wording in item 5 of the 17:42:56Z response.** The gate capture verifies source-file SHA256 values; it does not record source-file sizes or mtimes. The precise result is: source hashes remained unchanged; the ordinary loader, shipping ISO and both receipts retained their SHA256, sizes and nanosecond mtimes. The canonical image key was also preserved. This corrects the scope of the recorded evidence, without changing the passing gate result. The plan now uses this precise wording too.
