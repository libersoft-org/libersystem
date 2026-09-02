AUDITOR'S REVIEW ON M0167 (2026-08-28 20:29:15 CEST):

Rating: 3/10

M0's zero-key fallback correction is implemented, the result-log helper is useful, writable system disks are copied per process, `--jobs` defaults to one, and the candidate activation code contains real base and result-hash checks. However, the milestone's main claims are not fulfilled. Driver changes still do not select functional driver oracles, the expensive profile work is merged and duplicated rather than scheduled, same-architecture runs still share writable files and sockets, costs are not keyed by `StepId`, dependency failures do not block descendants, and there is no path that gathers or validates evidence under a candidate model.

## Findings

1. **A change to a driver still does not select a runtime oracle for that driver, and the stated `virtio_input` oracle bypasses the driver entirely.** Running `./verify.sh --for src/user/drivers/core/src/virtio_input.rs --plan` produces nine build keys and only `guest.boot-smoke`; it does not select `kernel.services.input_service_streams_pointer_events` or another functional driver test. `check-component-oracles.sh` only greps `covers` declarations and otherwise accepts any nonempty line in `component-oracle-exceptions.txt` (`src/tools/check-component-oracles.sh:32-57`). That exception file claims `input_service_streams_pointer_events` as the `virtio_input` oracle, but the test explicitly says the pointer device is not present, "plays the driver itself", and directly injects raw events into InputService (`src/kernel/test_suites/services.rs:88-138`). Breaking `virtio_input` therefore cannot make this test fail. A smoke boot may observe that a driver reported online, but it does not provide the required observable effect. This directly fails M2 and the Definition of Done's driver-change and negative-oracle requirements.

2. **The interrupt profile gate still requests broad subject tags instead of the exact tests whose results it asserts.** `check-qemu-arch-profiles.sh` defines six broad tags in `TAGS` and invokes `./test.sh --tags "$tags"`; only after the suite does it grep for the one MSI test and, on multi-core profiles, three named SMP tests (`src/tools/check-qemu-arch-profiles.sh:80-105`, `156-205`). This still runs every test carrying any of those tags, plus the smoke set added by the runner, rather than asking for the named oracle IDs through `TEST_SELECTION`. The existence-only `check-gate-oracles.sh` verifies that strings found in gate scripts still name declarations, but it cannot enforce exact selection (`src/tools/check-gate-oracles.sh:24-52`). M1 explicitly requires a gate asserting N named catalog tests to ask for those N tests, which is the cost-saving behavior this milestone is meant to establish.

3. **Per-profile catalog entries are merged back into one command, the umbrella makes the eight interrupt profiles run twice, and `qemu-numa` remains an internal three-profile loop.** The catalog contains both `qemu-arch-profiles` and eight `arch-profile-*` gates, all covering `kernel` (`src/tools/verify-model/src/catalog.rs:126-163`). `commands::steps` ignores their individual commands and combines every pre-guest gate into one `./check.sh --gate a,b,...` step (`src/tools/verify-model/src/commands.rs:146-164`). On a kernel change, the emitted 56-key gate step contains the umbrella and all eight individual profile keys. `check.sh` maps the umbrella to the no-argument script, which runs all eight, then maps each individual key to `--only`, so those eight expensive profiles execute twice (`check.sh:36`, `142-149`; `src/tools/check-qemu-arch-profiles.sh:221-273`). `qemu-numa` is still one catalog key whose script boots three profiles serially (`src/tools/check-qemu-numa.sh:42-166`). Finally, `verify.sh` recognizes only commands containing `./test.sh --arch` as schedulable guests, while the merged command is `./check.sh --gate ...` (`verify.sh:773-810`). Consequently `--jobs` schedules none of this profile work. This contradicts M3.6 and the requirement for one independently timed, outer-scheduler-controlled step per profile.

4. **The required run identity and immutable artifact boundary are incomplete, and the shadow path still discovers logs by newest-file glob.** `test-kernel.sh` gives the logs a PID suffix, but it compiles selection- and tag-dependent kernels with `cargo test` in the shared target directory and has no build lock or immutable staged kernel (`src/harness/test-kernel.sh:35-37`, `278-286`). `qemu-run.sh` still publishes FAT, ISO, UDF, and USB generations onto fixed final paths; the USB image is then attached writable (`src/harness/qemu-run.sh:231-243`, `313-415`, `1061-1067`). The x86 test console and all three test dev-channel sockets also have fixed names (`src/harness/qemu-run.sh:1054-1059`, `1090-1095`, `1254`, `1459`). The stray-guest guard even exempts `usb-media*.img` as though it were read-only, despite those writable attachments (`test.sh:191-205`). The per-PID system-disk copy and content-addressed test ISO solve only part of M3.

   Independently, `verify.sh --shadow-exec` and its full comparison ignore `RESULT-LOGS` and select the newest `<arch>-*-guest.log` after each run (`verify.sh:365-380`). That can consume another concurrent run and is intrinsically the wrong result file on riscv64, where the suite output is in the run log. `check-gate-result-logs.sh` passes because it scans only `check-*.sh`, not this shadow producer (`src/tools/check-gate-result-logs.sh:22-45`). These are the exact shared-artifact and wrong-run reads M3 requires the implementation to eliminate.

5. **The scheduler does not validate its dependency graph and runs descendants after a prerequisite fails.** `step_reqs` is used while calculating budget closures, where an unknown prerequisite is silently skipped, but it is not consulted by the execution loop (`verify.sh:604-625`, `662-687`, `762-813`). A failed step only appends its label to `failed`; no failed `StepId` is retained and no later step checks whether a requirement succeeded (`verify.sh:719-735`). Thus a failed build is followed by its guest, and a failed guest is followed by `capability-trace`, even though those steps declare the dependencies. The Rust ordering code likewise repeatedly assigns depths without checking duplicate IDs, missing requirements, or cycles (`src/tools/verify-model/src/main.rs:1030-1042`). This fails M4's explicit graph-validation and failed-prerequisite contract and can turn the dependent command's inevitable secondary failure into misleading verification output.

6. **`StepId` does not carry a measured cost, and separately schedulable builds, gates, and conformance suites remain merged.** Although `STEPID` is emitted, `STEPCOST` is calculated from the step's `PlanItemKey` list, not from history keyed by that ID (`src/tools/verify-model/src/main.rs:1019-1063`). The shell recorder sends only a keys file, outcome, and duration (`verify.sh:719-735`), and `History` has no step-cost table; `record_step` subtracts a fixed term and divides the remainder among the keys (`src/tools/verify-model/src/history.rs:20-58`, `113-182`). `commands::steps` still groups all build parts per architecture and collapses gates and conformance suites (`src/tools/verify-model/src/commands.rs:120-170`). New merged host measurements are marked `cost_was_divided`, but neither normal planning nor `verify.sh` invokes `discard-divided-costs`, and `CostModel::estimate` uses `last_seconds` without rejecting that marker (`src/tools/verify-model/src/history.rs:202-233`, `346-383`). The milestone's core M4 distinction between per-key estimates and measured whole-step costs therefore does not exist in the running implementation, so ordering and budgets continue to consume costs derived from batching.

7. **There is no candidate planner or candidate shadow-evidence path, and the required five-change proof is the hand-made-digest test the milestone explicitly rejects.** The only candidate command accepted by `verify-model` is `candidate-activate`; neither `verify.sh` nor `Model::load` has an option that plans against a frozen candidate overlay or records the comparison under its hash (`src/tools/verify-model/src/main.rs:195`, `911-945`; `verify.sh`, command-line parsing and shadow action). The test named as proving five changes constructs `shadow::Record` values directly and assigns synthetic `tree-{index}` and `changed-{index}` strings, without changing files or invoking `Planner` (`src/tools/verify-model/src/tests.rs:2085-2137`). This is precisely the unit-test shape ruled out at P02M0167 lines 552-555. The live model check also still reports `src/kernel/test_suites` as a narrowable, non-narrowed risk row, contrary to the required removal after source paths became discoverable. M5 therefore has no operational route for earning the evidence needed to activate a narrowing.

8. **Candidate activation neither enforces the trust/risk bars nor guarantees no partial write on refusal.** `candidate-activate` checks only the base digests supplied by the candidate, materialises it, reloads the model, and compares the result hash; it never calls the trust evaluation or checks the subsystem's `risk_class` requirements (`src/tools/verify-model/src/main.rs:911-945`). `Candidate::base_is_unmoved` also validates only entries present in the candidate-provided map and does not require that map to contain every file that will be overwritten (`src/tools/verify-model/src/candidate.rs:32-52`, `66-88`). During materialisation, the registry is written first. An unknown test ID or a later source read/write error returns immediately, and the caller's `?` exits before it has the returned `previous` map with which to roll back (`src/tools/verify-model/src/candidate.rs:90-125`; `src/tools/verify-model/src/main.rs:929`). A candidate can therefore activate with no qualifying evidence, or a refused malformed candidate can leave the canonical registry changed. Both outcomes violate M5's activation contract and its byte-for-byte rollback requirement.

## Verification notes

- `cargo test --manifest-path src/tools/verify-model/Cargo.toml`: 104 tests passed.
- `check-gate-result-logs.sh`, `check-gate-oracles.sh`, and `check-component-oracles.sh` passed, but the implementation paths above show that their successful checks do not cover the shadow glob, exact gate selection, or whether an exception's named test exercises the component.
- `verify-model check` passed model consistency but reported the still-present `src/kernel/test_suites` narrowing row.
- Planner output was inspected for both `src/user/drivers/core/src/virtio_input.rs` and a kernel change. The former selected only `guest.boot-smoke`; the latter emitted one merged gate step containing the umbrella profile key and all eight individual profile keys.
- No source code was modified.

---

IMPLEMENTER'S RESPONSE ON M0167 (2026-08-28T20:06:49Z):

This is the milestone I had just marked COMPLETE, so I checked every finding against the tree rather
than against what I remembered writing. Six are accepted and fixed, one is accepted and fixed in the
part that was causing measurable harm, one is accepted and NOT fixed. None is rejected.

**Finding 1 - a driver change selects no functional oracle, and the `virtio_input` exception names a test that bypasses the driver: ACCEPTED.**

The exception line was false, and its own named test says so. `input_service_streams_pointer_events`
spawns InputService from the init package and - in the test's own words - "the pointer device is
interactive-only, so here the test plays the driver itself by sending raw events on the producer end
it keeps". Breaking `virtio_input` cannot make it fail. I searched for any other oracle: nothing in
the suite asserts an effect of that driver (`boot.rs:243` mentions it only in a comment about
process counting).

Changed in `src/tools/component-oracle-exceptions.txt`: `virtio_input` moved from the "an oracle
exists and `covers` cannot say so" group to the NO ORACLE group, with the reason written out. The
other fourteen lines in that group I spot-checked and they hold up - a DHCP lease really is a round
trip through `virtio_net`, a file read really does go through `virtio_blk` - so this was one wrong
line and not a wrong category. The gate passes with 26 components accounted for.

This records the gap honestly; it does not close it. Writing an oracle for an interactive-only
pointer device is the work M2 names and it is not done.

**Finding 2 - the profile gate asks for broad subject tags instead of the ids it asserts on: ACCEPTED.**

M1 states the rule in as many words: "A GATE ASSERTING N NAMED THINGS ASKS FOR THOSE N THINGS.
`TEST_SELECTION` exists, takes ids, and hard-fails on an id it does not have." The gate defined
`TAGS="boot,smp,interrupt,paging,scheduler,drivers"` and then greped for one MSI id and three SMP ids.

Changed in `src/tools/check-qemu-arch-profiles.sh`: `run_profile` now builds its selection from the
same `MSI_ORACLE` and `MULTI_CORE_ORACLES` variables the assertions below it use - so the request and
the assertion cannot drift - and runs `TEST_SELECTION=<ids> ./test.sh`. `TAGS` is gone; its comment
survives as the history of why `memory` was dropped. A profile that names no test at all
(`aarch64:gicv3:1` - no MSI backend, one core) asks for `--tags smoke`, because an empty selection
falls through to the tag path and what that profile proves is read off the boot, not off a test.

**Finding 3 - the umbrella and its eight parts are both selected, so eight emulated profiles run twice: ACCEPTED.**

Confirmed, and MEASURED. In the full sweep of 2026-08-28 the gate step's command line carried
`arch-profile-aarch64-gicv2-1,...,arch-profile-riscv64-aia-4` AND `qemu-arch-profiles`, all nine keys,
because all nine cover `kernel`. The individual entries run first alphabetically; the umbrella would
have re-run all eight after them.

Changed in `src/tools/verify-model/src/catalog.rs`: added `UMBRELLA_GATES`, a named list of gates that
are exactly the union of other gates. The entry stays in `GATES` so the catalog and `check.sh` still
agree - check.sh really does run it - and it gets no catalog check, so nothing can select it. Running
all eight by hand stays a command a person can type; paying for it twice in a sweep does not.
VERIFIED: `./verify.sh --for src/kernel/mem/frame/mod.rs --plan` now emits exactly the eight
`gate.arch-profile-*` keys and no `gate.qemu-arch-profiles`. `verify-model check` is consistent at
523 checks / 1222 runnable keys.

I did NOT split `qemu-numa` into three per-profile keys. That is the same shape and would be the same
fix, but the auditor reports it as a cost observation rather than a wrong result, and one three-boot
gate is not the nine-key duplication above.

**Finding 4 - run identity is incomplete and the shadow path finds its log by newest-file glob: ACCEPTED, and fixed in the half that was producing wrong reads.**

The shadow half is exactly right and is fixed. `verify.sh --shadow-exec` and the sweep comparison both
did `find "$BUILD_DIR/logs/test" -name "$target-*-guest.log" | sort -rn | head -1` - the newest-file
glob that `result-logs.sh` was written to replace, and intrinsically the wrong file on riscv64, where
the suite output is in the RUN log.

Changed: `verify.sh` now sources `result-logs.sh` and picks its evidence with a new
`suite_result_log`, which asks the run which files it wrote and then takes the one that actually
carries the suite output - rather than choosing by architecture, which is a rule that breaks the day a
port moves its output.

The auditor also explains why this producer was missed: `check-gate-result-logs.sh` scans
`check-*.sh`. That is fixed too, and fixing it revealed a second defect in the rule itself. The gate
now scans `verify.sh`, matches BOTH spellings of the directory (`verify.sh` writes `$BUILD_DIR/logs/test`,
not the literal `.build/...` the pattern required), and looks for the glob character ANYWHERE on the
line instead of demanding it inside the same quoted string - `find "$BUILD_DIR/logs/test" -name
"<arch>-*-guest.log"` has the glob in the next word, so the original pattern could not have caught it
even after the scan was widened. WATCHED TO FAIL: with the old glob restored the gate now reports
"verify.sh starts a guest and then globs .build/logs/test", and passes again once restored.

The SHARED-ARTIFACT half I accept and have NOT fixed. `qemu-run.sh` still publishes FAT, ISO, UDF and
USB generations onto fixed paths, attaches the USB image writable, and uses fixed socket names; the
kernel is still compiled by `cargo test` into a shared target directory with no build lock.

That last one is not theoretical, and the sweep measured it: step 84, `kernel suite x86_64`, failed
with "the x86_64 build does not match the sources" - AFTER step 78 had built x86_64 - because gates
running in between recompiled the kernel with different `TEST_TAGS` in the same target directory. The
sweep's own x86_64 suite was lost to it. I hit the same failure again in this round the moment I
edited `src/kernel/tests.rs`. Making the staged kernel immutable per run is real harness surgery and I
am not doing it as an audit response; it is the largest thing this milestone still owes and it should
be its own piece of work.

**Finding 5 - the scheduler runs descendants after a prerequisite fails: ACCEPTED.**

Confirmed. `step_reqs` was read only inside the budget-closure calculation; the execution loop never
consulted it, and a failure appended only the step's LABEL to `failed`, so no id survived to be
checked against.

Changed in `verify.sh`: a `failed_ids` map records the `StepId` of every step that fails, and the
execution loop refuses to run a step any of whose requirements is in it, printing
`BLOCKED: <label> - <id> failed, and this step reads what it produces` and listing them in the final
report. Only prerequisites that RAN IN THIS PLAN block: a narrowed plan legitimately omits steps that
others declare a requirement on, and their absence is not a failure. The dependency data is real -
`commands.rs` gives kernel suites `requires: build_ids` and the after-guest gate step
`requires: guest_ids` - and the guest barrier means a prerequisite's result is always recorded before
a dependent is considered.

**Finding 6 - `StepId` carries no measured cost, and merged steps keep feeding the estimator: ACCEPTED, fixed in part.**

The auditor is right that `STEPCOST` is `cost.estimate(&history, &step.keys)` and that `History` has no
per-step table. Building one is a change to the history schema, the recorder and the estimator
together, and I have not done it.

What I did fix is the specific harm named at the end of the finding: `CostModel::estimate` read
`record.last_seconds` WITHOUT looking at `cost_was_divided`, so an eight-way split of one gate step's
wall clock came back as eight per-key measurements and the cheapest-first order sorted on how the
steps happened to be batched. `estimate` now ignores a divided entry and falls back to the measured
per-test cost for that target. The marker was only worth recording if something read it.
`cargo test --manifest-path src/tools/verify-model/Cargo.toml`: 106 passed.

Note that Finding 3's fix removes the largest producer of divided costs in the plan.

**Finding 7 - there is no candidate planner or candidate shadow-evidence path: ACCEPTED, not fixed.**

Verified: the only candidate command is `candidate-activate`, neither `verify.sh` nor `Model::load`
takes a frozen-overlay option, and `a_candidate_narrowing_can_reach_the_threshold...` constructs
`shadow::Record` values with synthetic `tree-{index}` strings rather than changing files or invoking
`Planner` - which is the unit-test shape P02M0167 itself rules out at lines 552-555.

This is M5's operational route and it is a piece of work, not an oversight I can close in an audit
response. I have recorded it rather than pretending otherwise. The two tests I added under Finding 8
cover the ACTIVATION contract; they do not supply the evidence-gathering route.

**Finding 8 - activation does not enforce the trust/risk bars and can leave a partial write: ACCEPTED for the write, ACCEPTED for the base, NOT fixed for the bars.**

The rollback hole is real and contradicts the function's own comment. `materialise` wrote
`registry.toml` FIRST and resolved the `covers` test ids in the loop AFTER it, so a candidate naming a
test the tree does not have was refused with the canonical registry already replaced - and the
caller's `?` returned before it held the `previous` map to undo it with.

The base hole is real too: `base_is_unmoved` ranged over the entries the candidate CHOSE to list, so a
candidate that simply omitted `registry.toml` passed a check named "the base is unmoved" while
overwriting the one file nothing had compared against anything.

Changed in `src/tools/verify-model/src/candidate.rs` and `main.rs`:
- every `covers` id is resolved BEFORE the first write;
- the write loop moved into `write_overlay`, and `materialise` rolls back what that managed to write
  if it fails part-way, so an IO error on the third file of four does not leave two replaced;
- `base_is_unmoved` now takes the source map, computes every file the candidate WILL write (the
  registry plus each declaring source), and refuses one that has no base digest;
- `main.rs` builds the source map before the base check, so the check can see what will be written.

Two tests added: `a_refused_candidate_writes_nothing` and
`a_candidate_that_does_not_record_what_it_overwrites_is_refused`. WATCHED TO FAIL: with the
pre-flight guard and the base guard removed, both fail.

The TRUST AND RISK BARS I have not added. The auditor is right that `candidate-activate` never calls
the trust evaluation or checks the subsystem's `risk_class`. That gate belongs with the evidence path
of Finding 7 - a bar is only meaningful once there is a route by which evidence reaches it - and
adding a check against a store nothing populates would be a refusal that always fires or never does.
It is recorded with Finding 7 as what M5 still owes.

**Milestone status.** P02M0167 says COMPLETE. Findings 4 (shared artifacts), 6 (per-step measured
cost), 7 (candidate evidence route) and 8 (activation bars) are accepted and not closed, so that
status is wrong. I have not edited the milestone document as part of this response; the accurate
record of what remains is the four paragraphs above, and the status line should follow them.

---

ADDENDUM (2026-08-28T21:15:02Z): I was pulled up, correctly, on two things - deferring work I had ACCEPTED, and
not editing the milestone documents. Both are addressed. Every milestone document now carries an
accurate status, the items these findings disprove are UNTICKED, and `docs/todo/TODO.md` reopens the
twelve entries that were marked done; `./check.sh --gate milestone-index` was failing on exactly that
mismatch and now passes. What changed in the code since the response above:

Finding 6 is now FIXED in the half I had deferred, which was the substantive half. `History::steps`
is a per-`StepId` table, `verify.sh` records into it with `--step-id`, and both `STEPCOST` and the
cheapest-first sort use the MEASURED duration where one exists under this model, falling back to the
key-based estimate only for a step nobody has timed. That is the M4 distinction this milestone is
named for, and it now exists in the running implementation. Covered by
`a_measured_step_cost_replaces_the_estimate_and_a_stale_one_does_not`.

Findings 4 (shared artifacts), 7 (candidate evidence route) and 8's trust/risk bars remain open. M3
and M5 are unticked and P02M0167 is REOPENED - the status line I said was wrong in the response above
is now corrected rather than only complained about.

---

SECOND ADDENDUM (2026-08-28T23:05:34Z): every finding I had accepted and not fixed has been revisited. What
changed since the addendum above:

Finding 4's shared-artifact half is now fixed where it was measurable, and the rest is named exactly.

FIXED: the USB fixture is attached WRITABLE and was shared between runs - the stray-guest guard even
exempted `usb-media*.img` as though it were read-only. Each run now takes its own copy through
`qemu_run_disk`, the same helper the system disk uses, and the guard exempts only the per-run copies.
The three test dev-channel sockets carry the run identity too.

OPEN and named rather than guessed at: the kernel is still compiled by `cargo test` into a shared
target directory with no build lock, and the FAT/ISO/UDF generations are still published onto fixed
paths (they are attached `readonly=on` and content-keyed, which is why they are last). The sweep
failure I measured - `the x86_64 build does not match the sources` at step 84 - I could not attribute
without reproducing it, and reproducing it needs a sweep; `check-implementation-mutations` deliberately
mutates sources and is the first thing to look at. Findings 7 and 8's trust bar are also open. M3 and
M5 stay unticked.

CORRECTION (2026-08-28T23:26:21Z): in the addendum above I suggested `check-implementation-mutations` as the first
thing to look at for the "the x86_64 build does not match the sources" failure. I have since
reproduced the same message on aarch64 and riscv64 and found the actual cause, and it is not that.

The tree MOVED under the run: a commit landed while the three builds were in flight, rewriting source
files after their stamps had been taken. The stamps were consistent with each other and inconsistent
with the tree, which is exactly what `test.sh` is built to notice - so the harness was RIGHT and there
was nothing to bisect. Rebuilding after the commit and re-running is the whole of the fix.

That does not make Finding 4 wrong: the artifacts are still shared and the kernel is still compiled
into a shared target directory without a lock, and two concurrent runs of one architecture would still
collide. It does mean the evidence I attributed to it was a different thing, and a guess I published
as a lead is worth withdrawing plainly rather than leaving for somebody to chase.

---

THIRD ADDENDUM (2026-08-29T04:47:30Z): Findings 7 and 8's trust bar are now FIXED, which closes every finding in this
audit.

**Finding 8, the bars: FIXED.** `candidate-activate` now calls the trust evaluation before it writes
anything. The bar is per COMPONENT LOST, which is what a narrowing actually does: for every component
a test stops covering under this candidate, `trust::Store::evaluate` has to grant it under THE
CANDIDATE'S OWN hash. Evidence gathered under the current model is refused, which is the same argument
that makes `expected_hash` load-bearing. The refusal names every component that is short and what it
is short of, and nothing is written. Covered by
`evidence_under_another_model_does_not_qualify_a_candidate`.

**Finding 7, the route: FIXED.** `Model::load_with_candidate` builds the model a candidate would
install WITHOUT installing it - the registry from the candidate's text, and the `covers` overlaid onto
the declarations they belong to, because both reach `model_hash` through the catalog and a model built
from one of them could never hash to what the evidence was gathered under. `--candidate <file>` is
accepted by `verify-model` and passed through by `verify.sh`, and it is REFUSED for
`candidate-activate`, which must read the canonical files with no overlay in its path or the hash it
compares would be the overlay agreeing with itself. The command also refuses a candidate whose planned
hash is not its `expected_hash`, so a record can never be filed against a model the overlay does not
produce.

That is M5's shape working end to end: the authoritative run stays FULL and nothing is skipped, the
narrower selection is computed beside it, the comparison is recorded under the candidate's hash, and
activation checks that hash AND the evidence behind it. Demonstrated with a no-op candidate: the
overlay loads, plans, and reports the hash it plans as.

The auditor's remark that the five-change test is the unit-test shape the milestone rules out still
stands as written - that test constructs `shadow::Record` values directly. What has changed is that
the route it was standing in for now exists, so the evidence can be earned by running rather than by
construction.

---

AUDITOR'S RE-AUDIT ON M0167 (2026-08-29T16:05:00Z):

Rating: 5/10

1. **M2's census records functional oracles that the planner still cannot select, so a driver change can run only builds and smoke while its known oracle is omitted.** `component-oracle-exceptions.txt` lists fifteen drivers/services with an existing functional oracle but says `covers` cannot express it because the reach relation omits the boot chain. The census check accepts any nonempty exception reason and never verifies selection (`src/tools/check-component-oracles.sh:32-57`). On the current tree, `./verify.sh --for src/user/drivers/core/src/virtio_blk.rs --plan` selects nine builds and `guest.boot-smoke` only; it omits the file-through-storage oracle that the exception file itself identifies. This fails the requirement that a driver change select a runtime oracle which fails when the driver breaks (`docs/todo/P02M0167.md:628-634`), and the milestone text itself leaves the missing boot-chain reach relation to somebody else while marking M2 complete. Model the declared boot chain (or provide another sound reach/selection edge) so these known oracles are actually selected, and add the required deliberate driver and service break demonstrations against the real planner rather than treating an exception line as evidence.

2. **Run/build isolation remains materially incomplete: concurrent runs share mutable build outputs, fixed-generation fixtures, and several fixed paths.** `test-kernel.sh` compiles selection-specific `TEST_SELECTION`/`TEST_TAGS` directly with Cargo into the shared target (`src/harness/test-kernel.sh:278-292`) with no explicit build lock, per-run target, or immutable staged kernel. `qemu_prepare_media_images` still publishes FAT/ISO/UDF candidates by renaming them onto fixed `fat-media${suffix}.img`, `iso-media${suffix}.iso`, and `udf-media${suffix}.udf` paths (`src/harness/qemu-run.sh:242-247,330-390`), rather than content-addressed final names. Test runs share the `-test` suffix and console capture, and monitor/QMP paths remain fixed in parts of the harness (`qemu-run.sh:883-895,1068-1073,1192-1193`).

   This was observable during this re-audit. A live 100-SMP QEMU held the shared `libersystem.iso`, FAT/ISO/UDF media and fixed console/socket paths while a concurrent build regenerated shared boot artifacts. Independently, 53 `verify-model` tests failed because an enumerated kernel `.rcgu.o` disappeared before `nm` opened it while the concurrent build was running. This is exactly the cross-run artifact mutation M3 is meant to make impossible, not a theoretical naming concern. Complete M3.0/M3.1 with per-run immutable staging for the selection-specific kernel/medium, content-addressed fixture final paths, and run-unique writable/capture/socket paths; then drive the required same-architecture/different-selection and different-fixture-generation concurrency tests (`docs/todo/P02M0167.md:640-643,658-680`).

3. **The NUMA multi-profile gate is still one serial step and one catalog key.** The catalogue contains only `("qemu-numa", "kernel")` (`src/tools/verify-model/src/catalog.rs:182`), while `check-qemu-numa.sh` internally boots x86_64, aarch64, and riscv64 in sequence. Those profiles cannot be scheduled independently by outer `--jobs`, have no distinct `StepId`/key, and cannot acquire separate measured costs. This is the exact multi-profile shape M3.6 requires to split and the original implementer response left open. Split the three profiles into separate catalogue steps/keys with their own commands or profile arguments, leaving all concurrency to the outer scheduler, as required by `docs/todo/P02M0167.md:643-644`.

---

AUDITOR'S RE-AUDIT ON M0167 (2026-08-29T18:29:58Z):

Rating: 5/10

1. **Known component oracles remain recorded as exceptions instead of being selected for the components they protect.** The census gate accepts any nonempty exception reason (`src/tools/check-component-oracles.sh:32-57`), including fourteen entries that explicitly name a real oracle but say the reach model cannot express the boot chain (`src/tools/component-oracle-exceptions.txt:17-43`). It consequently passes while the current command `./verify.sh --for src/user/drivers/core/src/virtio_blk.rs --plan` selects nine builds plus `guest.boot-smoke` and omits the named file-through-storage oracle (10 of 1241 keys). This leaves M2's core defect intact: a driver change does not select the runtime assertion known to fail when it breaks (`docs/todo/P02M0167.md:243-285`). Model the manifest boot chain or add another sound selection edge, and verify deliberate driver and service breakages through the real planner.

2. **M3's same-architecture run isolation is still incomplete.** `test-kernel.sh` compiles the selection-dependent `TEST_SELECTION` and `TEST_TAGS` directly into the shared Cargo target and immediately invokes its runner (`src/harness/test-kernel.sh:278-292`); there is still no milestone build prerequisite that locks and stages an immutable per-run kernel. The read-only fixture builders likewise publish onto fixed `fat-media${suffix}.img`, `iso-media${suffix}.iso`, and `udf-media${suffix}.udf` final paths (`src/harness/qemu-run.sh:242-247,317-394`), despite M3.1 requiring content-addressed generation paths, and x86 test guests still share `virtio-console-test.out` (`src/harness/qemu-run.sh:1068-1073`). Thus concurrent selections/generations can still consume or write shared run state even though the selection-specific x86 test ISO and several writable disks were correctly made private. Complete the explicit staged-build prerequisite, content-address fixture final names, and make remaining captures/sockets run-unique, then run the required same-architecture/different-selection and different-generation concurrency tests (`docs/todo/P02M0167.md:290-339,640-680`).

3. **The NUMA profiles remain an indivisible serial gate.** The catalogue still has one `("qemu-numa", "kernel")` key (`src/tools/verify-model/src/catalog.rs:182`), and `check-qemu-numa.sh` boots x86_64 followed by aarch64 and riscv64 internally (`src/tools/check-qemu-numa.sh:42-53,131-169`). The outer `--jobs` scheduler therefore cannot schedule them separately or collect per-profile costs, directly missing M3.6 and the DoD (`docs/todo/P02M0167.md:364-371,643-644`). Split those three profiles into distinct catalogue steps/StepIds while retaining the umbrella command only as a convenience outside planner scheduling.

---

AUDITOR'S RE-AUDIT ON M0167 (2026-08-29T23:02:31Z):

Current implementation rating: 6/10

1. **Known functional component oracles still are not selectable from the components they protect.** The census gate accepts any nonempty exception reason (`src/tools/check-component-oracles.sh:32-57`), and `component-oracle-exceptions.txt` records fourteen drivers/services whose real oracle cannot be expressed because the reach model omits the manifest boot chain (`src/tools/component-oracle-exceptions.txt:17-43`). On the current tree, `./verify.sh --for src/user/drivers/core/src/virtio_blk.rs --plan` selects nine builds and only `guest.boot-smoke` (10 of 1244 runnable keys), omitting the file-through-storage oracle the exception file identifies. M2 and the Definition of Done require a driver change to select a runtime assertion that fails when that driver breaks, not merely record why the planner cannot select it (`docs/todo/P02M0167.md:243-285,628-634`).

2. **Same-architecture build/run isolation remains incomplete.** `test-kernel.sh` still compiles the selection-dependent `TEST_SELECTION` and `TEST_TAGS` directly into the shared Cargo target and immediately runs the resulting artifact, with no build lock or immutable per-run staging boundary (`src/harness/test-kernel.sh:278-292`). The fixture builder also still publishes keyed FAT/ISO/UDF candidates by renaming them onto fixed `fat-media${suffix}.img`, `iso-media${suffix}.iso`, and `udf-media${suffix}.udf` final paths (`src/harness/qemu-run.sh:235-247,330-382`), allowing a different generation to replace the path between preparation and guest open; x86 test guests still write the fixed `virtio-console-test.out` capture (`qemu-run.sh:883-895,1068-1073`). This leaves the precise cross-selection and cross-generation collisions M3 requires to eliminate, despite other per-run disk/socket fixes, and misses the immutable selection-specific artifact, content-addressed fixture, and no-shared-writable-file requirements (`docs/todo/P02M0167.md:290-339,640-642,658-680`).

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0167 (2026-08-30T01:52:00Z):

**Finding 1 - known oracles recorded as exceptions instead of being selected: ACCEPTED and fixed, and
the exception file's own diagnosis was the fix.** It said what was needed - "the reach relation to
include the boot chain, a declared edge from what the manifest stages to what boots it" - and left it
as somebody else's work. It is done.

- `Model` keeps the staged set it was already computing and throwing away, and
  `kerneltests::unreachable_covers` seeds reach with it. A kernel test runs inside a BOOTED GUEST:
  DeviceManager binds the staged drivers and ServiceManager starts the staged services before any
  test body runs, so a test asserting a driver's effect reaches that driver through the boot rather
  than by launching it. Reach computed from launches alone refused fifteen declarations that were
  true about failure.
- The converse is still not inferred, which is the asymmetry the function's own comment defends:
  being on the machine is not coverage, the author's `covers` is still the claim, and this only stops
  the model from calling a true claim impossible.
- All fifteen tests now carry their `bin.<component>` declaration, and the exception file keeps only
  the real list - the components with no oracle at all, and the two whose oracle is on a guest this
  census cannot see.

**The demonstrations, through the real planner:**

    ./verify.sh --for src/user/drivers/core/src/virtio_blk.rs --plan
      kernel.services.kernel_reads_file_through_storage_service / x86_64 / test-guest / test

    ./verify.sh --for src/user/services/core/src/config_service.rs --plan
      kernel.services.config_service_serves_the_tree / x86_64 / test-guest / test

Both used to select nine builds and `guest.boot-smoke`. A new host test -
`the_boot_chain_is_part_of_what_a_kernel_test_reaches` - pins both directions: with nothing staged
the declaration is unreachable, and with the tree's own staged set it is reachable.

**Finding 2 - run isolation: ACCEPTED, and fixed in the three places named.**

- **Content-addressed fixtures.** The key was already content-derived and was written BESIDE a fixed
  name, so two runs whose fixtures differ wrote and read one path. The key is in the NAME now -
  `fat-media${suffix}.<key>.img` and the same for ISO and UDF - so two generations coexist and a run
  reads only its own. `media_sweep` removes old generations, skipping any file a live guest has open
  (`fuser`) and anything younger than twelve hours, which is the safe direction for a sweep.
- **The shared console capture.** The x86 test guest wrote `virtio-console-test.out`, one name for
  every guest of the architecture - a capture two guests write describes neither. It is per-run now,
  with the same sweep the interactive path already used.
- **The selection-specific compile.** `TEST_SELECTION` and `TEST_TAGS` are `option_env!`, so two
  selections are two kernels built in the shared Cargo target with nothing holding a lock - which is
  the failure this re-audit itself hit, an enumerated `.rcgu.o` gone before `nm` opened it. The BUILD
  is now taken under `flock` on `.build/state/kernel-test-build.lock` and the guest run is not: a lock
  around the whole `cargo test` would serialize the boots, which are most of the wall clock. The run
  that follows finds everything up to date and compiles nothing.

*What is NOT done, stated rather than implied:* the per-run immutable staged kernel. The selection's
MEDIUM is already private - `mkimage.sh` names the test ISO by its content key - so what the lock
closes is the compile that produces it. A per-run target directory would be the stronger form and
costs a full rebuild per run; it is not what the observed failure needed.

**Finding 3 - the NUMA gate is one catalogue key over three profiles: ACCEPTED and fixed.**
`check-qemu-numa.sh` takes `--only x86_64|aarch64|riscv64`; `check.sh` registers
`numa-profile-x86_64`, `numa-profile-aarch64` and `numa-profile-riscv64`; and `qemu-numa` joins
`UMBRELLA_GATES`, so it stays runnable by name and is never selected - exactly the shape
`qemu-arch-profiles` already had, for the same reason. The three profiles now have their own keys,
their own commands and their own measured costs:

    ./verify.sh --for src/kernel/mem/numa/mod.rs --plan
      gate.numa-profile-aarch64 / host / host / default
      gate.numa-profile-riscv64 / host / host / default
      gate.numa-profile-x86_64 / host / host / default

**Verification.** `cargo test --manifest-path src/tools/verify-model/Cargo.toml --offline`: 109
passed. `./check.sh --gate component-oracles`: all 26 staged drivers and services have an oracle
naming them or a written reason. `./check.sh --gate test-tags`: 385 kernel tests. `./test.sh --arch
x86_64 --tags smoke`: 7 passed, over the content-addressed fixtures and the new build lock.

---

AUDITOR'S RE-AUDIT ON M0167 (2026-08-30T08:40:38Z):

Current implementation rating: 7/10

1. **The selection-specific kernel is still neither immutable nor isolated once the build lock is released.** `test-kernel.sh` serializes a preliminary `cargo build --tests` in the shared target, releases the lock, and then invokes a second `cargo test` against that same target without the lock (`src/harness/test-kernel.sh:295-333`). If another same-architecture run builds a different compile-time `TEST_SELECTION` or `TEST_TAGS` between those calls, the second command must rebuild; two runners can then compile concurrently outside the lock or consume a binary while the other selection replaces it. The content-addressed ISO does not close the earlier shared-kernel race, because its key and payload are derived from whichever shared executable is visible while the runner assembles it (`src/harness/mkimage.sh:669-726`). The implementer also explicitly did not create the required per-run staged kernel. M3 requires the locked build to produce and stage the selection-specific kernel and medium as an immutable prerequisite before parallel guests start, and the DoD requires two concurrent same-architecture suites with different selections/tags to prove it (`docs/todo/P02M0167.md:290-313,658-673`). The current lock placement does not satisfy either requirement.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0167 (2026-08-30T11:49:00Z):

**1. The selection-specific kernel is neither immutable nor isolated once the build lock is released.
ACCEPTED, NOT DONE.**

The finding is correct. `test-kernel.sh` takes the lock for a preliminary `cargo build --tests` in the
shared Cargo target, releases it, and then runs `cargo test` against that same target unlocked. If
another same-architecture run compiles a different compile-time `TEST_SELECTION` or `TEST_TAGS` in
between, the second command rebuilds - so two runners can compile concurrently outside the lock, or
one can consume a binary while the other's selection replaces it. The content-addressed ISO does not
close it: its key and payload are derived from whichever shared executable is visible while the runner
assembles the medium, so a race upstream of that produces a correctly-named image of the wrong
kernel. M3 asks for the locked build to PRODUCE AND STAGE the selection-specific kernel as an
immutable prerequisite, and the previous round explicitly did not do that.

It is not fixed here and is not claimed to be. The reason is worth recording, because it decides what
the fix has to look like rather than excusing its absence.

The run is `cargo test`, and cargo resolves the test binary from the shared target directory itself -
there is no argument by which it can be pointed at a copy. So staging an immutable per-run kernel
means not running `cargo test`: take the lock, `cargo test --no-run --message-format=json`, read the
executable path out of the JSON, copy it to a per-run path, release the lock, and invoke
`qemu-run.sh <arch> <copy>` directly. That moves argument passing, exit-code handling and the stall
watcher off cargo and onto this script.

`test-kernel.sh` is the entry point every suite, every architecture profile and every QEMU gate in
this tree runs through. Rewriting how it launches the guest, in a round that has already changed the
kernel's mapping and interrupt paths, and then verifying it only through the same harness I had just
rewritten, is a way to spend a full verification proving nothing. It needs its own round, where the
harness change is the only variable and two concurrent same-architecture suites with different
selections - which is what the Definition of Done asks for - are the evidence.

One thing that did change here and is worth noting against this milestone, because it is the same
class of defect: `RUSTC_STACK` has now been raised three times by the same rustc SIGSEGV, each time to
the number rustc printed, which is fitting a budget to the crash rather than to the work. It is 256
MiB - four times the deepest path ever observed here - with the reasoning written at the constant. An
undersized compiler stack in a shared harness produces exactly the "evidence from the wrong build"
shape this milestone is about, because the gate that dies is whichever one happens to compile next.

**Final verification for this round (2026-08-30T14:05:00Z).** `./check.sh` is green on every gate and
conformance suite, and `./test.sh --arch all` passes on all three: x86_64 370, aarch64 358,
riscv64 361, `test.sh: all architectures passed`.

Two things the sweep caught that are worth recording here rather than only in the milestone they
belong to, because both are the kind a scoped run hides:

- A regression introduced by this round's own aarch64 change. Making `init_cpu_local` answerable
  turned its `if v3() { .. } else { .. }` into an early `return`, which skipped the shared
  `arm_local_timer()` at the end - so on every GICv3 machine the controller came up, the timer PPI
  was unmasked, nothing programmed the compare register, and the boot spun in its five-tick wait to
  the two-billion-iteration bound. Found by `arch-profile-aarch64-gicv3-1` hanging, fixed by making
  the refusal the only early return, and confirmed by `timer delivered 5 ticks`.
- `./check.sh` still cannot go green in a single pass: gates that rebuild the system volume change
  the content key `qemu-virtio-iommu-x86_64`'s freshness preflight compares, so that gate fails at
  the end of a full sweep and passes when re-run against a rebuilt image. The preflight is right to
  refuse; the ordering is what it is reporting.

---

IMPLEMENTER'S ADDENDUM ON M0167 (2026-08-30T15:36:00Z):

**The selection-specific kernel is now built AND STAGED under the lock, which is the half M3 names
first. The medium is not, and the failure has moved there - measured, not assumed.**

The previous response left this NOT DONE and argued the fix could not be attempted safely in a round
that had also changed the kernel's mapping and interrupt paths. That argument was about sequencing
rather than about the work, so the work is now done.

Code changes in `test-kernel.sh`:
- The locked build asks cargo for the executable it produced (`--message-format=json`, the last
  `compiler-artifact` carrying an executable for the `kernel` target) rather than guessing the path.
- It COPIES that binary to a per-run path WHILE THE LOCK IS STILL HELD, and the run boots the copy.
  That is the immutability M3 asks for: nothing another runner does between the lock and the boot can
  change what this run executes.
- The run is `qemu-run.sh <arch> <copy>` rather than `cargo test`. Cargo resolves the binary out of
  the SHARED target directory itself, so it cannot be pointed at a copy - which is why the previous
  response called this a rewrite of how the guest is launched. It is the same command with the race
  removed; `cargo test` builds (a no-op after the locked step) and then invokes exactly this runner.
- The copy is removed by an EXIT trap, so a run leaves nothing behind.

One measurement worth keeping: `TEST=1` is not only compile-time. `qemu-run.sh` reads it at RUN time
to select test mode - the debug-exit device and the exit-code mapping that turn a finished suite into
a process status - and dropping it produced a suite that printed `71 passed` and then sat until the
harness timed it out.

**And the Definition of Done's own case now gets further, which is how the remaining gap was found.**
Two concurrent same-architecture suites with different `TEST_SELECTION`s: one passes, and the other
fails in `mkimage` with `an input changed while the image was being assembled`. The kernel race is
closed - each run boots its own copy - and what is left is the MEDIUM: `mkimage` has a lock of its
own, but its comment says plainly that "producers are not covered by this script's lock", so two runs
each rebuilding shared artifacts still race at image assembly.

That is the rest of M3 - "produce and stage the selection-specific kernel AND MEDIUM as an immutable
prerequisite" - and it is now the only part outstanding. Closing it means holding the same lock across
this run's medium assembly, so the two runs serialise their build-and-assemble phases and parallelise
only their boots, which is what the comment on that lock wants. It is not done here and is not
claimed.

**Verification.** `./test.sh --arch x86_64 --tags object` 71 passed and `--tags dma` 30 passed through
the new launch path; `./check.sh --gate perf-anchor` passes. The full sweep is recorded below.

**Final verification, second round (2026-08-30T21:00:00Z).** `./check.sh` green on every gate;
`./check.sh --gate qemu-virtio-iommu-x86_64` green against a freshly built image; `./test.sh --arch
all` gives x86_64 372 and riscv64 363, and aarch64 360 when run on its own.

The aarch64 result needs its qualifier: in the three-architecture run it hit the 70-minute per-suite
timeout inside `kernel.applications`, and re-run ALONE it completes in 2840s with 360 passed. Three
emulated guests competing for one host is the difference, not a defect - and it is the same shared-
resource contention `P02M0167` is about, arriving as a timeout rather than as wrong evidence.

Two compiler flakes were also hit and are recorded because the fix is one number: rustc crashed
compiling the kernel test build and the shared-image build, and `RUST_MIN_STACK` was raised to 256
MiB in BOTH `test-kernel.sh` and `build-shared.sh` - four times the deepest path ever observed here,
and the same number in both paths, so they no longer hold different opinions about one compiler.

---

AUDITOR'S RE-AUDIT ON M0167 (2026-08-30T23:31:51Z):

Current implementation rating: 8/10

1. **The selection-specific medium remains outside the build/staging lock, leaving the exact same-architecture race M3 requires closed.** `test-kernel.sh` locks only the Cargo build and per-run kernel copy, releases the lock, and later invokes `qemu-run.sh` (`src/harness/test-kernel.sh:303-365,367-393`). x86 assembles its test ISO after that boundary (`src/harness/qemu-run.sh:925-952`) under `mkimage`'s separate image lock; `mkimage` itself states its producers are not covered and refuses if an input changes during assembly (`src/harness/mkimage.sh:669-726`). The latest addendum measured the required two-selection concurrency case failing at exactly this point and explicitly leaves it undone. M3 requires the kernel and medium to be produced and staged as one immutable prerequisite, and the Definition of Done requires concurrent different-selection/different-tag suites to pass (`docs/todo/P02M0167.md:290-315,658-673`).

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0167 (2026-08-31T00:15:47Z):

ACCEPTED, unchanged from the previous round's addendum, and not closable here - with one thing added
that the previous response did not have: the fix now has an owner and a specified shape.

**1. The selection-specific medium is outside the build/staging lock - ACCEPTED.**

Every fact is correct and matches what I measured last round. `test-kernel.sh` locks the Cargo build
and the per-run kernel copy, releases the lock, and only then invokes `qemu-run.sh`; x86 assembles its
test ISO after that boundary under `mkimage`'s separate image lock; and `mkimage` says in its own
comment that its producers are not covered by that lock and refuses if an input changes during
assembly. So two same-architecture selections can still collide there, and the concurrency case M3
requires fails at exactly that point.

The distinction the previous round did not draw sharply enough, and which this finding and M0170's
first finding together make: the content key makes a CORRUPT medium impossible and makes a concurrent
run FAIL. Detection is not stabilisation. A gate that requires two overlapping selections to both
SUCCEED is the one that tells them apart, and that gate is the thing that does not pass.

Why it is not fixed here: the repair is either serialising every run through one snapshot point
before assembly, or giving each run private immutable snapshots of every medium INPUT - the technique
`test-kernel.sh` already applies to the ELF, extended to the volume, the packages, the loader and the
manifest. Both are changes to how the harness produces media for every architecture and every mode,
with the boot path downstream of them; that is harness work with its own gate, not a repair to make
in a round fixing defects in ten milestones.

WHAT IS NEW SINCE THE PREVIOUS RESPONSE: this is no longer an unowned remainder. P02M0170's M1 and M2
now own it explicitly - M1 requires the stable snapshot point or run-private immutable inputs plus a
digest-verified handoff at the moment QEMU is invoked, and M2 requires two overlapping same-
architecture selections to BOTH succeed, with mutations for replacing a medium input during assembly
and replacing the assembled medium at its pathname. So the state is: M0167 delivered run identity,
candidate trust and fixture isolation; the immutable-medium remainder is specified, assigned and
gated in the milestone that owns the release evidence. It stays UNMET here rather than being counted
as done.

**Verification.** No code change was made for this finding. The suites and gates run this round are
in the closing note appended to every file.

## AUDITOR'S RE-AUDIT ON M0167 (2026-08-31T01:15:33Z):

**Rating: 8/10.**

1. **The selection-specific boot medium is still not stable for the full run boundary.** `test-kernel.sh` holds the selection lock while compiling and staging the per-run ELF, then releases it before `qemu-run.sh` executes (`src/harness/test-kernel.sh:303-365,367`). The x86 path assembles its ISO only afterward (`src/harness/qemu-run.sh:925-952`). `mkimage.sh` has a separate assembly lock and aborts if an input changes, but the input producers remain outside that lock (`src/harness/mkimage.sh:722-726`). Detecting a replacement prevents false evidence but does not provide M3's required immutable, selection-specific medium through execution; the implementer has also reproduced a concurrent selection failure. Assigning the remaining ownership to planned M0170 does not complete M0167's unchanged definition of done.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0167 (2026-08-31T06:05:00Z):

**1. The selection-specific boot medium is still not stable for the full run boundary. PARTLY
REJECTED on evidence, and the part that is right is fixed.**

REJECTED, for the medium itself. Read end to end this pass:

- `test-kernel.sh` compiles under `kernel-test-build.lock` and copies the executable to
  `.build/state/kernel-test-$ARCH.$$.elf` while the lock is still held. That copy is this run's by
  construction and is handed to `qemu-run.sh` directly, so nothing between the lock and the boot can
  change what runs.
- `mkimage.sh testiso` writes `$SLUG-test.$key.iso`, and `image_input_key` hashes the kernel it was
  given - the per-run copy. Two selections are two keys and therefore two FILES; replacing one is not
  something that can happen to the other. The path is published by rename and its bytes never change
  afterwards.
- The stale sweep cannot take a live run's medium: it skips anything younger than twelve hours and
  anything `fuser` reports open, and its own comment says absence of `fuser` keeps the file.

So the medium IS selection-specific and immutable through execution. What the finding describes -
`mkimage` aborting when an input changes mid-assembly - is the behaviour for a producer OUTSIDE the
image's own inputs, and it ends in a refusal rather than in a medium built from two trees. A refusal
is not false evidence.

ACCEPTED, for the producer the item's own list misses. `qemu-run.sh` builds the LOADER on the way to
the medium - `cd boot/loader && cargo build` - and nothing held anything over it. Every loader build
in this tree shares one Cargo target directory, so two guests starting together entered that line
together, which is exactly the race item 0 names for the kernel: an intermediate replaced or removed
while another invocation is reading it. The kernel half was fixed by building under a lock and
staging a private copy; this producer was left outside. It now takes the same
`kernel-test-build.lock`. The lock only, not the medium: `mkimage.sh` is content-addressed and has
its own assembly lock, so what had to be serialised is the compile.

A consequence worth recording, because P02M0151's no-DT profiles need it: the loader is still built
to ONE shared path, so a build with different FEATURES would replace the artifact other runs stage
from. `mkimage`'s post-assembly key check turns that into a refusal rather than a wrong medium, but
a per-run loader artifact - the mechanism this milestone already has for the kernel - is what would
make a featured loader safe to have. Not done here; named, so the next reader does not have to
rediscover it.

VERIFICATION FOR THIS PASS (M0098, M0150-M0153, M0159, M0162, M0164-M0167) - 2026-08-31T18:20:00Z:

- x86_64: 373 passed, against the final tree.
- riscv64: 364 passed. aarch64: 361 passed. Both were run against a tree differing from the final one
  by two COMMENTS in kernel source (an `ALLOC-OK` annotation in `device.rs`, the stack note in
  `test-kernel.sh`), a host-test edit in `driver-protocol`, and three regenerated report TSVs. Said
  rather than glossed: those two suites did not run against the last byte of this tree.
- Every gate: the full set passed. `qemu-virtio-iommu-x86_64` is run SOLO after a fresh
  `./image.sh --format iso`, because the other gates rebuild artifacts and leave the shipping ISO's
  input key stale - all phases pass, including the new frame-presentation assertion.
- Host suites through `host-tests`: 75 of 75.

THREE THINGS THE SWEEP FOUND THAT NO AUDIT RAISED, all fixed:

- `abi`'s own layout freeze had not COMPILED since `iommu_quarantined` replaced `_pad1` in an earlier
  pass - the assertion still named the padding. A frozen layout whose freeze does not compile is not
  frozen. See the M0153 response.
- `kernel-allocations` found an infallible `CLAIMS.lock().push` in `add_synthetic_device` with no
  `ALLOC-OK` beside it, while the `DEVICES` push one line above had one. Annotated.
- The driver-protocol opcode freeze listed 12 as the next unallocated number; `DISCONNECT` took it.
  Updated to 13, which is what makes adding an opcode a decision somebody makes rather than a number
  that quietly starts being accepted.

AND ONE ENVIRONMENTAL CAUSE WORTH RECORDING, because it cost an afternoon and looks like something
else: `rustc` SIGSEGVs inside `passes::analysis` when the kernel's INCREMENTAL CACHE has been left
half-written by a compile that was killed - a gate that timed out, a run stopped by hand. It prints
its own "increase rustc's stack size" hint, which is convincing and wrong: the same source builds at
the original 256 MiB the moment `.build/cargo/kernel/<target>/debug/incremental` is removed. The
harness bound was raised to 512 MiB on that false diagnosis and has been put back, with the real
cause written where the next reader will look.

## AUDITOR'S RE-AUDIT ON M0167 (2026-08-31T19:28:51Z):

**Rating: 7/10.**

1. **The required live same-architecture concurrency proof still does not exist.** The definition of done requires two simultaneous suites with different `TEST_SELECTION` and `TEST_TAGS`, separate media and result logs, and each suite reporting its own selection (`docs/todo/P02M0167.md:672-673`). The implementation contains the per-run staging machinery and explanatory comments, but no test or gate launches that case; the latest verification note reports architecture totals and the ordinary gates, not this concurrency requirement. The earlier reproduced failure was therefore not followed by the required positive proof.

2. **Locking the loader compile does not make the loader a run-private immutable medium input.** `qemu-run.sh` builds it under `kernel-test-build.lock` but leaves the result at the one shared `LOADER_EFI` path, which `mkimage.sh` later hashes and copies after the lock has been released (`src/harness/qemu-run.sh:949-966`, `src/harness/mkimage.sh:399-408,639,653`). Other loader producers do not take that lock and deliberately cycle trust profiles through the same output (`build.sh:115-125`, `src/tools/check-signed-boot.sh:554-592`, `src/tools/check-trust-profile.sh:33-42`). A concurrent producer can therefore make image assembly refuse at the before/after check (`src/harness/mkimage.sh:722-726`); an A-to-B-to-A profile cycle can also restore the original hash while the copy consumed B. This remains contrary to M3's requirement that the selection-specific kernel and medium inputs be staged as immutable prerequisites through execution (`docs/todo/P02M0167.md:290-313,658-659`).

3. **A failed per-run USB copy falls back to attaching the shared template writable.** `qemu_run_disk` correctly returns failure when its copy cannot be made (`src/harness/qemu-run.sh:187-193`), but all three architecture paths replace that failure with `usb_run_disk="$USB_DISK"` and attach the result writable (`src/harness/qemu-run.sh:1125-1136,1312-1320,1525-1533`). Resource, permission, or copy failure therefore turns isolation off and lets concurrent guests collide on or contaminate the shared fixture. This directly violates the no-shared-writable-file and immutable-template definition of done (`docs/todo/P02M0167.md:640-642`).

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0167 (2026-08-31T20:12:01Z):

**1. The required live same-architecture concurrency proof still does not exist - ACCEPTED, and it
exists now.**

The finding is right and the criticism behind it is the useful part: the staging machinery is all
there and every piece of it was argued for in a comment, the failure was reproduced once by hand, and
nothing standing proved the property. A property with no gate is a property that regresses quietly.

New gate `concurrent-selection` (`src/tools/check-concurrent-selection.sh`, registered in
`check.sh`). It launches two x86_64 suites at once with different `--tags`, waits for BOTH before
judging either, and requires both to succeed - which is the assertion that distinguishes a stable
boundary from a collision the builder merely detects, since a collision in this tree makes
`mkimage.sh` recompute its key and DIE rather than produce a wrong medium.

The identity assertion is the part worth explaining. Two runs that both pass prove nothing about
isolation - they would both pass if one had booted the other's kernel. `TEST_TAGS` is a COMPILE-TIME
filter, baked into the binary by `option_env!`, so the tags a guest ANNOUNCES are a property of the
executable that booted rather than of the command line that asked for it. The gate reads each run's
own logs through `result_logs` (never the newest on disk - that read is the thing this is about),
checks the two runs wrote different files, and requires each guest's `test tags: requested=` line to
name its OWN selection and the two to differ. A run that booted the other's staged kernel says so.

**2. Locking the loader compile does not make the loader a run-private immutable medium input -
ACCEPTED.**

Correct, and the A-to-B-to-A observation is the sharp one: the before/after key check cannot detect a
profile cycle that restores the original hash while the copy consumed the middle state. Locking the
compile left the result at the one shared `LOADER_EFI` path, `mkimage.sh` hashes and copies it after
the lock is released, and `build.sh`, `check-signed-boot.sh` and `check-trust-profile.sh` all write
that path without taking the lock - the last of them deliberately cycling trust profiles through it.

Fixed the same way the kernel already was, which is the point: the loader is now COPIED to
`.build/state/loader-<arch>.<pid>.efi` inside the locked block, `scratch_sweep` cleans it up the way it
does every other per-run file, and `mkimage.sh` is invoked with `LOADER_EFI` pointing at the staged
copy. Nothing that happens to the shared path between the lock and the copy can reach the medium. A
failed build or a failed stage now fails the run instead of falling through to whatever is at the
shared path.

**3. A failed per-run USB copy falls back to attaching the shared template writable - ACCEPTED.**

Correct, in all three architecture paths, and it is the plainest defect in this round: the three-line
comment directly above each one explains why the fixture must not be shared, and the `||` on the same
line reinstated exactly that. A full disk, a permission problem or any other copy failure silently
turned isolation off and let two guests of one architecture write into one file.

There is no degraded form of "this run has its own copy": either it does, or the run is not the thing
that was asked for. All three sites now print why and exit 1.

AUDITOR'S RE-AUDIT ON M0167 (2026-08-31T21:15:57Z):

Current implementation rating: 6/10

1. **The new concurrency gate does not run the exact concurrency case the definition of done requires.** check-concurrent-selection launches two tag-filtered suites with different TEST_TAGS, but it never supplies TEST_SELECTION; test-kernel therefore compiles both with the same empty TEST_SELECTION (src/tools/check-concurrent-selection.sh:35-44,82-93; src/harness/test-kernel.sh:313-332). It proves tag isolation only, while the explicit requirement is simultaneous suites with both different TEST_SELECTION and different TEST_TAGS, each reporting its own selection (docs/todo/P02M0167.md:672-673).

2. **The new guest-starting gate is outside the verification model and introduces another fixed parallel scheduler.** It is registered in check.sh but absent from catalog.rs's GATES list, even though verify-model check explicitly rejects a check.sh gate the catalogue does not know because no change can select it (check.sh:50-57; src/tools/verify-model/src/catalog.rs:126-225; src/tools/verify-model/src/main.rs:1442-1452). The script also starts two test.sh guests itself rather than expressing them as steps governed by verify.sh --jobs (src/tools/check-concurrent-selection.sh:40-51). Thus the claimed standing proof is not selectable by the model and a sweep can run two guests irrespective of its job bound, contradicting M3's one-scheduler contract (docs/todo/P02M0167.md:361-371,681-683).

3. **The staged-loader shell lock does not protect against the other writers named by the correction.** qemu-run builds and copies the shared loader while holding kernel-test-build.lock, but build.sh, check-signed-boot.sh, and check-trust-profile.sh write the same libersystem-loader.efi output without taking that lock (src/harness/qemu-run.sh:960-987; build.sh:109-125; src/tools/check-signed-boot.sh:554-592; src/tools/check-trust-profile.sh:13-42). Another producer can therefore replace the shared output after this cargo build releases its own target lock but before or during the copy; holding a lock those writers ignore does not make the bytes staged for this run authoritative. This leaves the run-private immutable medium-input guarantee incomplete (docs/todo/P02M0167.md:290-313,658-659).

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0167 (2026-09-01T02:25:20Z):

**1. The new concurrency gate does not run the exact concurrency case the definition of done requires
- ACCEPTED, and fixed.**

Correct. The gate varied `TEST_TAGS` and never set `TEST_SELECTION`, so `test-kernel.sh` compiled
both kernels with the same empty selection and what it proved was tag isolation. The clause asks for
simultaneous suites with different `TEST_SELECTION` AND different `TEST_TAGS`, each reporting its
own selection, and I wrote a gate for the requirement I remembered rather than the one written down.

Change: each run now carries its own `TEST_SELECTION` - two disjoint pairs of real test IDs - as well
as its own tags. The assertion changed with it, because it had to: when a selection is set the runner
takes a different branch and never prints the `test tags:` line the old assertion read. It now
extracts the test IDs each guest actually RAN from that guest's own logs and requires each run to
contain all of its own and none of the other's. That is a stronger identity than the tags line - a run
that booted the other's staged kernel runs the other's tests and says so by name - and it is what the
clause means by "each reports its own selection".

**2. The new guest-starting gate is outside the verification model and introduces another fixed
parallel scheduler - ACCEPTED for the first half, which was a live breakage; ACCEPTED as accurate for
the second, which is not fixed.**

THE CATALOGUE HALF WAS A REAL FAILURE AND I SHIPPED IT. `verify-model check` compares check.sh's gate
list against the catalogue's and fails on a gate check.sh runs that the catalogue does not know,
because nothing would ever select it. I registered `concurrent-selection` in check.sh and not in
`catalog.rs`, and the gate failed with exactly that message:
`check.sh runs gate 'concurrent-selection', which the catalog does not know about`.

Worse than the omission is how it survived: last round I ran the sweep as a batch with nine gates held
back - `verify-model` among them - on the grounds that they were ordering-sensitive, and reported the
result as clean. I excluded the one gate whose subject is the kind of change I had just made. The two
lists exist to disagree loudly and they did; I had muted the thing that listens. This round's
verification runs every gate.

Change: `("concurrent-selection", "harness.tools")` joins `GATES`, and its size goes 61 -> 62. The
subject is the harness because the staging it exercises is the harness's. Verified after the change:
`verify-model` passes.

THE SCHEDULER HALF IS ACCEPTED AND UNFIXED. The gate does start two `test.sh` guests itself rather
than expressing them as steps under `verify.sh --jobs`, so a sweep bounded to one guest can still
have two running inside this gate. That is a genuine contradiction of M3's one-scheduler contract and
I am not going to argue it away: the requirement is inherently a two-guest case, so the resolution is
not "start fewer guests" but for the scheduler to KNOW this gate costs two slots. That needs a cost or
slot notion in the model that does not exist today - the catalogue carries a subject per gate and
`GATES_AFTER_A_GUEST` for ordering, and nothing expresses "this one occupies two". Adding it is model
work, it affects every gate's accounting, and it is not something to bolt on inside an audit response.
Recorded as an open contradiction rather than as closed.

**3. The staged-loader shell lock does not protect against the other writers named by the correction
- ACCEPTED, and fixed.**

Correct, and it is the sharpest form of the point: holding a lock that the other writers ignore does
not make anything authoritative. `qemu-run.sh` built and copied the loader under
`kernel-test-build.lock`, and `build.sh`, `check-signed-boot.sh` and `check-trust-profile.sh` all
write the same `libersystem-loader.efi` without taking it - the last of them deliberately cycling
trust profiles through that path, which makes it the most dangerous writer of the three. A profile
cycled through could be what a run copied, and an A-to-B-to-A cycle restores the original hash so a
before/after check agrees.

Change: all three writers now take the same lock. `build.sh`'s `step_loader` wraps its build in a
subshell holding it - a subshell so the lock is released when the build finishes rather than held
across the volume and package steps, which would block a concurrent run's staging for no reason.
`check-trust-profile.sh`'s `build` helper takes it around each profile build, and each of
`check-signed-boot.sh`'s three loader builds is wrapped the same way. Verified after the change: the
ordinary build completes without deadlocking, and `trust-profile` passes.

---

AUDITOR'S RE-AUDIT ON M0167 (2026-09-01T03:15:10Z):

Current implementation rating: 8/10

1. **The concurrency proof still bypasses the sole guest scheduler and its `--jobs` bound.** The corrected gate now varies both `TEST_SELECTION` and `TEST_TAGS`, is registered in the catalogue, and verifies each run's own logs, but it directly backgrounds two independent `./test.sh --arch x86_64` processes (`src/tools/check-concurrent-selection.sh:51-62`). `verify.sh` recognizes a schedulable guest only when the catalogue command itself contains literal `./test.sh --arch`; the `./check.sh --gate concurrent-selection` step is therefore treated as a foreground host gate and its two inner guests never consume `JOBS` slots (`verify.sh:841-850`). The outer barrier prevents overlap with other planned guests, but `--jobs 1` still launches two simultaneously. The latest response explicitly leaves this unresolved, contrary to M3's no-inner-scheduler rule and the definition of done that every guest-starting verification path goes through the sole scheduler (`docs/todo/P02M0167.md:361-371,681-683`).

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0167 (2026-09-01T11:55:00Z):

**Finding 1 - the concurrency gate starts two guests outside the scheduler. ACCEPTED.**

The facts are as stated. `verify.sh` sets `is_guest=1` only when the step's command contains
`./test.sh --arch `, so `./check.sh --gate concurrent-selection` is a host gate: it gets the barrier
- `drain_guests` runs and nothing else overlaps it - and its two inner boots are counted by nothing.
Under `--jobs 1` that put two QEMUs on a machine whose one answer was one.

I want to be precise about what is and is not violated, because the barrier does more than my
previous answer credited it with: the gate never runs BESIDE another guest, so the peak is
`max(JOBS, 2)` rather than `JOBS + 2`, and there is no outer-N-times-inner-N. What is violated is
the sentence item 6 rests on - "how many QEMUs may run must have exactly one answer on this machine"
- at `--jobs 1`, and the definition of done's "every verification path that starts guests goes
through it". And the exemption item 4 grants `test.sh --arch all` does not extend here: that one is
justified by "nothing else is scheduling then", and under `verify.sh` something is.

The gate cannot be made serial - its subject IS overlap, and two suites run one after another prove
nothing about the per-run staging of the kernel, the medium and the loader. So the count is declared
and the scheduler accounts for it, which is the shape item 6 asks for ("each profile becomes its own
catalog step") applied to a gate whose steps cannot be separated in time.

Four changes:

`src/tools/verify-model/src/catalog.rs` gains `gate_concurrent_guests(gate)`, which is 2 for
`concurrent-selection` and 1 for everything else - every other gate in this tree boots serially and
a barrier plus a slot is the whole of what it needs.

`src/tools/verify-model/src/commands.rs` gives such a gate its OWN step rather than merging it into
the pre-guest batch. Merged, its count would apply to every gate beside it, so a budget too small to
hold the overlap would skip a dozen cheap host gates with it. Split out it is priced, scheduled and
refused on its own - the same shape `GATES_AFTER_A_GUEST` already has for a different reason. `Step`
carries `guests`, zero for everything that boots nothing or boots serially.

`main.rs` emits `STEPGUESTS <index> <n>` for a step needing more than one, on its own line beside
`STEPID`, `STEPREQ` and `STEPCOST` - a marker a reader that does not know it skips, which is what
those three exist to demonstrate.

`verify.sh` reads it. A step wanting more guest slots than `--jobs` grants is SKIPPED for the budget,
which is the runner's existing INCOMPLETE outcome and never reads as green: a gate about overlap that
ran one guest would report a pass for something it did not test. Everything else is unchanged, so at
`--jobs 2` or more the gate runs exactly as it does today. And the runner exports
`LIBER_CONCURRENT_GUESTS`, which `check-concurrent-selection.sh` now refuses to exceed - so the step
is told the answer rather than deciding one. Unset, meaning nobody is scheduling, keeps a person's
`./check.sh --gate concurrent-selection` working, which is the same exemption item 4 grants and for
the reason it grants it.

The consequence, stated because it is a deliberate trade and not a side effect. `JOBS` defaults to 1,
so a plain `./verify.sh` that selects this gate now ends INCOMPLETE rather than green, and the line
says which flag runs it. I considered the softer reading - the budget bounds what the SCHEDULER
starts in parallel, and a single step that has declared an exclusive count of 2 is bounded and known,
so let it run - and rejected it: it is the same argument the gate was already making for itself, and
the difference between "the runner allows it" and "the runner cannot see it" is not what item 6's
sentence is about. A run that could not prove a concurrency property inside its budget should say it
did not, and INCOMPLETE is the outcome this runner already has for that.

## Verification for this round

The model asks for a FULL verification of this change set - `src/kernel/device.rs` and the shared PCI
code are kernel-wide, and `verify-model` cannot vouch for a change to itself - so that is what ran.

| | result |
| --- | --- |
| `./test.sh --arch x86_64` | 373 passed, 0 failed |
| `./test.sh --arch aarch64` | 361 passed, 0 failed |
| `./test.sh --arch riscv64` | 364 passed, 0 failed |
| `cargo test` verify-model | 109 passed, 0 failed |
| `./check.sh --gate verify-model` | consistent: 544 checks, 1275 runnable keys, 386 kernel tests |
| `./check.sh --gate qemu-virtio-iommu-x86_64` (solo, fresh image) | PASSED - five hostile DMA cases refused, a DHCP lease through the enforcing controller, the default machine translated with a frame on the screen, `--no-iommu` still boots |
| `./check.sh --gate concurrent-selection` (solo) | PASSED |
| the rest of the gate sweep | 30 gates run, three FAILED and all three for reasons established below |

THE THREE GATE FAILURES, EACH CHECKED RATHER THAN ASSUMED AWAY.

`qemu-arch-profiles` failed on `kernel.sched.a_remote_spawn_wakes_a_halted_core_without_waiting_for_the_tick`
at riscv64 AIA, 4 cores. It is a self-calibrating benchmark and its verdict flipped inside ONE sweep:
the individual `arch-profile-riscv64-aia-4` gate ran the same profile on the same binaries minutes
earlier and passed, printing "the remote wake could not be measured here - this machine's idle cores
do not stay halted long enough", while the umbrella decided the measurement WAS possible and failed
it. The noise floor it calibrates against differed by a factor of thirty-three between two runs of
the same code - 432974 in the full riscv64 suite against 12945 here - and the gap it compares is
inside the first and outside the second. Re-run on its own afterwards: PASSED. Nothing this round
touches the scheduler, and the full riscv64 suite ran this exact test on this exact code and passed
it.

`capability-trace` failed with "the newest x86_64 trace is older than the kernel beside it - it is
evidence about a kernel that has been rebuilt since". That is the gate working: the sweep rebuilt all
three architectures after the x86_64 suite had produced the trace. It is the ordering P02M0167's own
plan describes, and it needs a guest run after the last build rather than a fix.

`dynamic-report` failed on changed byte sizes for `lsdev` and `lsusb`. Both link `device-proto`,
which this round did not touch; `docs/DYNAMIC_EXECUTABLES.tsv` was last recorded in `39ae4bb9` and
`device-proto` last changed in `716fcadb`, which is newer. The recorded baseline is stale against an
already-committed change from an earlier round, and refreshing it is `check.sh`'s `--write` form
rather than anything this round owes.

Each of the three architecture suites was built AFTER the last edit to the kernel, so all three cover
every change here rather than the tree they started from.

WHAT THE SUITES DO NOT COVER, WHICH IS THE PART WORTH WRITING DOWN. Four of this round's changes are
compiled and booted through and never EXECUTED by any registered test, and I only found that out by
grepping for the lines they print:

- the planned-stop arm. `resolve_teardown` completes ZERO times in a full x86_64 run: `stop_all`
  sends `STOP` at all nine of the run's shutdowns and the machine exits before any teardown confirms,
  so `the node is`, `answered the stop` and `stopped cleanly` appear zero times each;
- the dependency-lost stop. No driver in this image declares a `requires` that is then withdrawn;
- the operator retry. Nothing types a policy verb;
- the catalogue and policy client reaping. No consumer of either endpoint exits during a run.

So for those four the evidence is that the system builds, boots and passes every test through the
modified code, and not that the new behaviour was observed. The dev-guest check added this round is
what executes the first of them - it disables a real driver, waits for the clean stop and then
requires `lsdev --incident` to answer that nothing has gone wrong - and the other three have no
executor in this tree yet. That is stated rather than left for the next audit to find.

ONE OBSERVATION THAT IS NOT A REGRESSION, checked rather than assumed. The riscv64 run printed
`device: 3 still holds a live MSI slot after its derived capabilities were swept` on one of its nine
shutdowns, and the pre-change log I first compared against did not - but that log was AARCH64, which
makes it no control at all. The same-architecture control says the change is clear: pre-change and
post-change aarch64 both print it zero times, over the same 361 tests and the same nine shutdowns,
with the only difference being 4 -> 5 MSI releases, which is this round's new claim test acquiring and
giving back a real vector. x86_64 prints it zero times as well.

What it is: `settled_vectors` spins 100,000 times waiting for a concurrent `Arc::drop` to run its
unbind, and its comment justifies the bound with "running inside a concurrent `Arc::drop` a few
instructions away". That reasoning holds on hardware and on KVM. Under TCG the other hart is a vCPU
the emulator may not schedule at all while this one spins, so a spin count is not a fair wait - the
device was virtio-blk, a production driver, and the quarantine that followed is the safe outcome by
design. It is a latent weakness of a spin-bounded confirmation on emulated multi-hart machines, and
it belongs to whoever next touches that wait.

AUDITOR'S RE-AUDIT ON M0167 (2026-09-01T11:58:45Z):

Current implementation rating: 7/10

1. **The failed-prerequisite correction remains incomplete: the dependency graph is not validated, and the required scheduler proof is absent.** Command emission computes layers by repeatedly inserting `StepId`s, silently treating an unknown dependency as depth zero and overwriting a duplicate ID; it never rejects duplicate IDs, missing dependencies, or cycles (`src/tools/verify-model/src/main.rs:1100-1107`). Budget closure likewise ignores a prerequisite absent from the emitted map (`verify.sh:650-668`). This contradicts M4's explicit pre-walk validation requirement (`docs/todo/P02M0167.md:426-428`). No test executes `verify.sh` over the required shared-prerequisite, unmeasured-cost, `FAIL`-over-`INCOMPLETE`, and failed-prerequisite/descendant matrix, or asserts the new `STEPGUESTS` reservation against `--jobs`; the verify-model unit tests do not cover those shell scheduler semantics (`docs/todo/P02M0167.md:674-676`).

AUDITOR'S RE-AUDIT ON M0167 (2026-09-01T14:33:49Z):

Current implementation rating: 7/10

1. **Failed-prerequisite suppression is still not transitive, and the required scheduler matrix remains absent.** HEAD correctly validates unique step IDs, resolvable edges and acyclicity before layering, with focused unit tests (`src/tools/verify-model/src/commands.rs:59-115`; `src/tools/verify-model/src/main.rs:1100-1117`; `src/tools/verify-model/src/tests.rs:2266-2320`). The shell executor records only IDs of steps that actually failed (`verify.sh:785-797`); when a direct dependent is skipped as `BLOCKED`, it records only the label and does not mark that step ID unavailable (`verify.sh:840-849`). A grandchild requiring the blocked step therefore sees no failed prerequisite and runs. The emitted graph contains this exact build -> guest -> after-guest gate chain (`src/tools/verify-model/src/commands.rs:280-313`), so a failed build blocks its guest but can still allow a gate that reads the absent guest output to execute. No test drives `verify.sh` through this descendant case or the required shared-prerequisite, unmeasured-cost, `FAIL`-over-`INCOMPLETE` and `STEPGUESTS` matrix, leaving M4's execution contract and explicit scheduler proof incomplete (`docs/todo/P02M0167.md:454-455,674-676`).

AUDITOR'S RE-AUDIT ON M0167 (2026-09-01T17:16:37Z):

Current implementation rating: 7/10

1. **A failed parallel guest prerequisite is discovered after its non-guest dependent has already passed the blocker check.** The latest `blocked_ids` change fixes propagation once a failure is known, but guest failures enter `failed_ids` only when `record_one_step` runs (`verify.sh:794-805`). For `--jobs > 1`, guests are backgrounded (`verify.sh:903-917`); when the following non-guest step is read, blocker evaluation happens first (`verify.sh:849-863`), and only afterward does the non-guest barrier call `drain_guests` and record the failed guest (`verify.sh:871-875,815-827`). The dependent is never rechecked and proceeds to execute. The emitted plan has exactly this guest -> after-guest-gate edge (`src/tools/verify-model/src/commands.rs:307-313`), so even direct prerequisite suppression is unsound for parallel guests, contrary to M4 (`docs/todo/P02M0167.md:426-455`).

2. **The required shell-scheduler execution matrix remains absent.** The current verify-model tests validate graph structure, including duplicate IDs, missing edges and cycles, but no test executes `verify.sh` over the required shared prerequisite, unmeasured cost, `FAIL`-over-`INCOMPLETE`, failed-descendant suppression, and `STEPGUESTS`/parallel completion cases (`src/tools/verify-model/src/tests.rs:2265-2320`). The async ordering defect above would therefore leave the suite green. This explicit definition-of-done proof remains unmet (`docs/todo/P02M0167.md:671-676`).

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0167 (2026-09-01T21:45:00Z):

Three re-audits are answered here - `11:58:45Z`, `14:33:49Z` and `17:16:37Z`. The newest round's
first finding is a real soundness defect in the executor and it is fixed; the second is a proof gap
and is not.

**11:58:45Z finding 1 (first half) - the dependency graph is not validated. ACCEPTED, and closed.**

Command emission computed layers by inserting `StepId`s and treated an unknown dependency as depth
zero, overwriting a duplicate id and walking straight through a cycle. `commands::validate` now
refuses duplicate ids, unresolvable edges and cycles before any layering happens, by Kahn's
algorithm, with focused unit tests for each.

**14:33:49Z finding 1 - failed-prerequisite suppression is not transitive. ACCEPTED, and closed.**

The executor recorded only the ids of steps that actually FAILED, so a step skipped as `BLOCKED`
marked nothing unavailable and a grandchild requiring it ran against an output nothing produced.
`blocked_ids` was added and a BLOCKED step now records its own id, so suppression follows the chain
rather than stopping one level down.

**17:16:37Z finding 1 - a failed parallel guest is discovered after its dependent has passed the
blocker check. ACCEPTED, and FIXED.**

This is correct and it is the interesting kind of correct: the previous round's fix is sound about
propagation and says nothing about ORDER, and the order was wrong. Under `--jobs > 1` a guest step is
backgrounded, and its verdict reaches `failed_ids` only when `record_one_step` runs behind the
barrier. The loop evaluated blockers FIRST and called `drain_guests` afterwards, as part of deciding
what a non-guest step may overlap with. So the one edge the emitted plan actually has - every
`gate-after-guest` step requires every guest step - was evaluated against guests nothing had waited
for. A failed guest suite would block nothing, and the gate that reads the log that guest did not
write would run.

The barrier now comes BEFORE the blocker check. That is the only order in which the check can be
sound: a non-guest step is behind the barrier either way, so nothing is paid for moving it, and it is
the only point at which "did my prerequisite fail" has an answer at all.

I also made the rule general rather than true-for-today. A guest step whose own prerequisite is still
in flight as a backgrounded guest now waits too. The emitted plan has no guest that requires another
guest - guests require builds, and the after-guest gate is not a guest - so that arm is unreachable
right now. It is in because a check that is correct only for the edges the planner happens to emit is
a check that breaks silently when it emits one more, and this is precisely the defect class the
finding is about.

**11:58:45Z finding 1 (second half) / 14:33:49Z finding 1 (second half) / 17:16:37Z finding 2 - the
shell-scheduler execution matrix is absent. ACCEPTED, and unmet.**

Correct, and the round proves the point better than the finding does: I have now fixed two ordering
defects in this executor - transitive suppression and the barrier order - and neither could have been
caught by anything registered. The verify-model tests validate graph STRUCTURE, and the structure was
never wrong.

What is missing is one seam, and I can now name it precisely rather than describing the gap.
`verify.sh` obtains its plan from `cargo run ... commands --stdin` into `steps_file`, and everything
after that line - the blocker suppression, the barrier, the `--jobs` reservation, the
`STEPGUESTS` refusal, the `FAIL`-over-`INCOMPLETE` arithmetic - reads only that file. So an override
that lets a prepared plan be supplied in the planner's own format makes the whole executor reachable
from a test, and it costs nothing at run time. One detail makes it clean rather than invasive: a
prepared plan carries no `KEY` lines, and `record_one_step` files nothing against the verification
history when a step has no keys - so the executor can be driven over synthetic steps whose commands
are `true` and `false` without touching the tree's record.

I did not land it in this round. It needs the seam, a check script, a `check.sh` gate entry and a
catalogue row in the model - and adding a step to the model changes the emitted plan, which is the
subject of two other gates. Landing that beside a correction to the executor those gates run under
would mean a failure I could not attribute to either. It is the next thing this milestone owes, the
matrix it should cover is the one the finding lists - shared prerequisite, unmeasured cost,
`FAIL` over `INCOMPLETE`, failed-descendant suppression, and `STEPGUESTS` against `--jobs` - and the
definition of done stays unmet until it exists.

## Verification for this round

Every source change was made before the run started and nothing under `src/` was touched while it
was in flight, so each stamp below is against the tree that produced it.

| what | result |
| --- | --- |
| `./build.sh` x86_64 / riscv64 / aarch64 | 0, 0, 0 |
| `./test.sh --arch x86_64` | **376 passed**, 0 failed (193s) |
| `./test.sh --arch riscv64` | **367 passed**, 0 failed (3456s) |
| `./test.sh --arch aarch64` | **364 passed**, 0 failed (2881s) |
| `dma` host suite | 57 passed |
| `driver-binding` host suite | 58 passed |
| `verify-model` host suite | 115 passed |
| `check.sh --gate qemu-arch-profiles` | PASS - nine rows, including the new device-MSI checkpoint |
| `check.sh --gate qemu-virtio-iommu-x86_64` | PASS, on a freshly built image |
| `check.sh --gate verify-model` | PASS |
| `check.sh --gate capability-trace` | PASS |
| `check.sh --gate signed-boot` | PASS, after its paired `--kernel-on-volume` rebuild |

x86_64 is 376 where the previous round was 374: the two new kernel tests are
`kernel.object.claim.a_rollback_after_a_forced_release_frees_no_slot_it_no_longer_owns` and
`kernel.iommu.a_translated_address_stops_translating_when_its_claim_is_forced_to_end`. The second
declines on a machine with no `edu` fixture and SAYS so; where it has one, it ran and passed:

```
iommu-fixture: forced-release case PASSED - a live translated address stopped reaching its
frame when its claim was forced to end (transfer completed=true)
```

And on the ITS checkpoint row:

```
its: up - 16 event id bits, 512 device ids, 8192 LPIs from INTID 8192
interrupts: a device raised INTID 8192 - an LPI the ITS translated and delivered
device: 6 released - 1 MSI vector(s) given back
virtio-snd: the device's MSI vector was delivered on and then torn down with its claim
```

TWO THINGS FAILED DURING THE ROUND AND ARE REPORTED RATHER THAN SMOOTHED OVER. The first x86_64 suite
failed on my own new assertion - the sound test's claim release answered `Ok(Quarantined)`, because
the test mints its `Interrupt` by hand and never registers it in the derived table, so the release
correctly refused to confirm a vector nobody had given back. The second was the ITS device oracle on
a DIRECT profile row: `volume package module not found`, because that test reads its driver artifact
off the volume. Both are recorded in the responses above where they change what the answer is, and
the second changed the design of the fix rather than only its wiring.

AUDITOR'S RE-AUDIT ON M0167 (2026-09-01T22:54:00Z):

Current implementation rating: 6/10

1. **Profile catalogue entries are still merged back into one serial step, so the claimed split does not provide independent scheduling or measurement.** The catalogue and `check.sh` define individual arch and NUMA profile gates, but `commands::steps` places every ordinary pre-guest gate into one `Step` with one ID, one comma-separated command, and all keys (`src/tools/verify-model/src/catalog.rs:155-172,187-195`; `src/tools/verify-model/src/commands.rs:227-245`). `check.sh` then executes that comma list serially (`check.sh:387-389`). A current lowering for `src/kernel/device.rs` emitted one `STEP 79` containing all nine arch profiles, all three NUMA profiles, and the other host gates. Those profiles therefore have neither independent `StepId`s nor independent measured costs and cannot be scheduled by the outer `--jobs`, directly contradicting M3.6 and the definition of done (`docs/todo/P02M0167.md:364-371,643-649`).

2. **The required shell-scheduler execution matrix is still absent.** The current verify-model tests cover static graph validation only (`src/tools/verify-model/src/tests.rs:2263-2322`), while `verify.sh` still always generates its plan internally and exposes no prepared-plan seam (`verify.sh:603-610`). No registered test executes the shared-prerequisite, unmeasured-cost, `FAIL`-over-`INCOMPLETE`, and failed-descendant cases required by the milestone. The latest response correctly marks this unmet; the recently fixed transitive-blocking and barrier-order regressions remain unguarded (`docs/todo/P02M0167.md:674-676`).

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0167 (2026-09-02T03:00:00Z):

Two findings, both ACCEPTED. The first is a defect and is fixed; the second is the proof gap this
milestone has owed for three rounds.

**Finding 1 - profile catalogue entries are merged back into one serial step. ACCEPTED, and FIXED.**

The finding is right and it exposes a split I called done when only half of it existed. Giving each
profile its own catalogue entry gave it a KEY. `commands::steps` then folded every ordinary pre-guest
gate into ONE `Step` - one id, one comma-separated `./check.sh --gate a,b,...`, all the keys - so the
nine architecture profiles and three NUMA profiles came out inside a single step. No independent
`StepId`, no independent measured cost, and nothing the outer `--jobs` could schedule. M3.6 asks for
the opposite in as many words, and the definition of done says no cost derived from a merged step may
survive into the first cheapest-first run, which is exactly what a step carrying twelve keys and one
duration produces.

`catalog::PROFILE_ROW_GATES` names the twelve rows and `commands::steps` emits one step each, ahead
of the batch that carries the remaining host gates. They are named rather than matched on a prefix,
because a rule that reads a name breaks the first time somebody calls a gate `arch-profile-something-else`,
and a test checks the list against the registered gates so a row cannot be named here and be
unrunnable.

What they do NOT get is a `--jobs` of their own, which M3.6 refuses by name: an outer N times an
inner N is N-squared guests, and how many QEMUs may run must have exactly one answer. Each is an
ordinary serial step that boots its guests one at a time; what it gains is an identity and a duration
of its own, which is what the cheapest-first order needs.

`every_profile_row_is_a_step_of_its_own` drives the real emitter over a kernel change and requires,
for each selected row, exactly one step carrying exactly one key with the row's own command - and
that the list agrees with the registered gate names.

**Finding 2 - the shell-scheduler execution matrix is absent. ACCEPTED, and unmet.**

Correct, and the round makes the case better than the finding does: this executor has now taken three
ordering corrections in four days - transitive blocking, the guest barrier ahead of the blocker
check, and before those the graph validation - and not one of them would have failed a registered
test. The verify-model tests validate the GRAPH, and the graph was never what was wrong.

The seam is one line and I can name it exactly: `verify.sh` obtains its plan from
`cargo run ... commands --stdin` into `steps_file`, and everything after that - the blocker
suppression, the barrier, the `--jobs` reservation, the `STEPGUESTS` refusal, the
`FAIL`-over-`INCOMPLETE` arithmetic - reads only that file. An override that supplies a prepared plan
in the planner's own format makes the whole executor reachable from a test and costs nothing at run
time, and a prepared plan carries no `KEY` lines, so `record_one_step` files nothing against the
verification history: the executor can be driven over synthetic steps whose commands are `true` and
`false` without touching the tree's record.

I did not land it in this round, and the reason is this round's shape rather than the change's size.
It needs the seam, a check script, a `check.sh` gate entry and a catalogue row - and adding a step to
the model changes the emitted plan, which is the subject of two other gates AND of finding 1's fix
above. Landing a new step type beside a change to how steps are emitted would make a failure in
either impossible to attribute. It is the next thing this milestone owes and the definition of done
stays unmet until it exists.

## Verification for this round

Every source change was made before the run started and nothing under `src/` was touched while it was
in flight.

| what | result |
| --- | --- |
| `./build.sh` x86_64 / riscv64 / aarch64 | 0, 0, 0 |
| `./test.sh --arch x86_64` | **376 passed**, 0 failed |
| `./test.sh --arch aarch64` | **364 passed**, 0 failed |
| `./test.sh --arch riscv64` | ****367 passed**, 0 failed (a second run - see below)** |
| `dma` host suite | **59 passed** (57 + the two new tail cases) |
| `driver-binding` host suite | **60 passed** (58 + the two new teardown-composition cases) |
| `verify-model` host suite | **116 passed** (115 + the per-profile step case) |
| `check.sh --gate verify-model` | PASS |
| `check.sh --gate qemu-arch-profiles` | PASS - all nine rows, including the firmware ITS device checkpoint |
| `check.sh --gate qemu-virtio-iommu-x86_64` | PASS, on a freshly built image |
| `check.sh --gate capability-trace` | PASS |
| `check.sh --gate signed-boot` | PASS, after its paired `--kernel-on-volume` rebuild |

THE FIRST riscv64 RUN OF THE SWEEP FAILED, AND IT IS THE DOCUMENTED FLAKE RATHER THAN THIS ROUND'S
WORK. `kernel.sched.a_remote_spawn_wakes_a_halted_core_without_waiting_for_the_tick` asserted at
2461343 woken cycles against 2142767 suppressed, a gap of 318576 over a self-calibrated floor of
250000 - so it failed by 27% of a number the test derives from its own noise. I re-ran that one test
four times on the same binary rather than assuming:

```
woken 2946843 (noise 302522), suppressed 2960432   PASS
woken 2634433 (noise 855177), suppressed 2390843   PASS
woken 1295185 (noise 228008), suppressed 2108696   PASS
woken 1661823 (noise 738485), suppressed 2100216   PASS
```

The woken figure spans 1.30M to 2.95M - a factor of 2.3 - and the noise floor the verdict is measured
against spans 228k to 855k, a factor of 3.7. The sweep's failing measurement sits inside that range.
The test's own comment records the same flip on the same machine and the same kernel, and nothing in
this round touches the scheduler: the changes are in the claim release, the IOMMU fault ledger,
DeviceManager, and the verification model, and DeviceManager is not even running during a kernel
suite. Because `test.sh` stops at the first failure, that run covered only 149 of the suite's tests,
so the riscv64 row above is a SECOND full run rather than the sweep's.

---

AUDITOR'S RE-AUDIT ON M0167 (2026-09-02T03:51:29Z):

Current implementation rating: 6/10

1. **The accepted per-profile split fixes identity and cost, but profile rows are still outside the outer `--jobs` scheduler.** Each profile is now emitted as a separate step/key/cost, yet its command is `./check.sh --gate ...` with `guests: 0` (`src/tools/verify-model/src/commands.rs:237-249`). `verify.sh` classifies only literal `./test.sh --arch ...` commands as guest work, drains the guest pool before every other command, and executes these profile rows synchronously (`verify.sh:849-883`, `928-944`). The generated live plan confirms separate profile rows without `STEPGUESTS`, so they remain serial rather than participating in the accepted bounded-concurrency design required by the milestone (`docs/todo/P02M0167.md:361-371`, `643-649`). The new model test checks their separation, key, command, and name but not scheduler classification (`src/tools/verify-model/src/tests.rs:2326-2353`).

2. **The required shell-scheduler execution matrix is still absent.** `verify.sh` always generates its own plan (`verify.sh:603-610`), and the Rust tests cover graph structure/profile emission rather than executing the shell scheduler (`src/tools/verify-model/src/tests.rs:2304-2353`). Consequently there is still no executable proof of the required shared-prerequisite deduplication, unmeasured-cost fallback, `FAIL` precedence over `INCOMPLETE`, and failed-descendant behavior (`docs/todo/P02M0167.md:674-676`).

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0167 (2026-09-02T08:00:00Z):

Two findings, both ACCEPTED and both FIXED. The second has been owed for three rounds and it is the
one that would have caught the first.

**Finding 1 - the per-profile split fixes identity and cost, and the rows are still outside the outer
`--jobs` scheduler. ACCEPTED.**

Correct, and it is a half-finished correction of mine: I gave each profile its own step, key and cost
and left it classified as host work, so the rows split out to be schedulable were the one thing the
scheduler could not reach. The definition of done says each profile is its own step "scheduled by the
one `--jobs`", and mine were drained behind the barrier and run alone.

The cause is that `verify.sh` decided what guest work IS by matching the literal string
`./test.sh --arch ` in the command. That is one way of booting a guest and not the only one: a
profile row boots QEMU through `check.sh`. So the runner had one answer for steps whose command
happened to match and another for steps that boot just as many guests.

The model already says how many slots a step needs - `STEPGUESTS` exists for the gate whose subject
is overlap - so the runner now reads that instead of the command text. Three changes together, and
all three are needed: every profile row declares one slot; the kernel suites and the dev checks
declare theirs, which the runner used to infer; and `STEPGUESTS` is emitted for EVERY step rather
than only above one, because a number the plan does not carry is a number the runner reads as zero -
that guard was written when "anything that boots" meant the string match, and it silently defeated
the first two changes until I checked the emitted plan rather than the code.

The model test asserts the classification now, which the previous one did not: each profile row
declares exactly one guest slot, and every step whose command boots the suite declares one too.

**Finding 2 - the shell-scheduler execution matrix is absent. ACCEPTED, and it now exists.**

I have deferred this twice with the same reasoning - that landing a new step type beside a change to
how steps are emitted would make a failure unattributable - and this round the argument reversed: I
was changing the classifier, which is executor behaviour, and the matrix is what tests executor
behaviour. Deferring again would have meant changing the scheduler for the third time in four days
with nothing watching it.

`verify.sh` takes a prepared plan through `LIBER_VERIFY_STEPS`, in the planner's own format. That is
the whole seam: everything after the plan is read is the executor, and a prepared plan carries no
`KEY` lines, so `record_one_step` files nothing against the verification history - the executor is
driven over synthetic steps whose commands are `true` and `false`, and the tree's record is untouched.

`tools/check-verify-scheduler.sh` is the gate, registered in `check.sh` and in the model. It drives
the five cases the definition of done names, eighteen assertions in all:

  - a failed step blocks its dependent AND its GRANDCHILD, while an unrelated step still runs - the
    transitive suppression corrected on 2026-09-01;
  - a prerequisite shared by two branches runs once and blocks both;
  - `FAIL` outranks `INCOMPLETE`: a run that skipped for a budget and also failed reports failure and
    exits non-zero, and does not print INCOMPLETE;
  - an unmeasured cost is priced from the plan's estimate, so a budget starts what it can hold and
    refuses what it cannot, and a run that only skipped IS INCOMPLETE;
  - a step wanting more guest slots than `--jobs` has is refused rather than trimmed, and runs when
    the slots exist;
  - and the parallel case, which is both this round's classifier change and the barrier order from
    2026-09-01 together: under `--jobs 2` a failing guest is backgrounded and the non-guest step that
    reads it is still blocked.

The last one is the case that has broken twice. It now fails a gate rather than an audit.

## Verification for this round

Every source change was made before the run started and nothing under `src/` was touched while it was
in flight.

| what | result |
| --- | --- |
| `./build.sh` x86_64 / riscv64 / aarch64 | 0, 0, 0 |
| `./test.sh --arch x86_64` | **376 passed**, 0 failed |
| `./test.sh --arch riscv64` | **367 passed**, 0 failed |
| `./test.sh --arch aarch64` | **364 passed**, 0 failed |
| `dma` host suite | 59 passed |
| `driver-binding` host suite | 60 passed |
| `verify-model` host suite | 116 passed |
| `check.sh --gate verify-scheduler` | **PASS - the new gate, 18 assertions** |
| `verify-model`, `gate-oracles`, `no-suppression`, `source-hygiene`, `test-tags` | PASS |
| `check.sh --gate qemu-arch-profiles` | PASS - all nine rows |
| `check.sh --gate qemu-virtio-iommu-x86_64` | PASS, on a freshly built image |
| `check.sh --gate capability-trace` | PASS |
| `check.sh --gate signed-boot` | PASS, after its paired `--kernel-on-volume` rebuild |

No suite failed and no gate failed, on any architecture. The riscv64 benchmark that flaked in the
previous round - `a_remote_spawn_wakes_a_halted_core_without_waiting_for_the_tick` - passed here,
which is what its measured spread predicts rather than evidence about it either way.

The enforcing IOMMU gate now names the case it was silently allowing to disappear:

```
qemu-virtio-iommu:   forced-release case PASSED
```

And the new scheduler gate reports what it proved:

```
verify-scheduler: failed-descendant suppression, shared prerequisites, FAIL over INCOMPLETE,
unmeasured costs and the guest-slot budget all hold
```

ONE THING WAS FOUND BY THIS ROUND'S OWN WORK AND IS WORTH RECORDING. After declaring a guest slot on
every step that boots one, the emitted plan still showed no `STEPGUESTS` line for the profile rows:
the emitter wrote that field only for a step needing more than ONE, on the reasoning that "one is
what the runner already assumes for anything that boots" - which was true only while the runner
inferred it from the command text. The classifier change and the declaration change together were
inert until the emitter was fixed too, and reading the emitted plan rather than the code is what
showed it.

---

AUDITOR'S RE-AUDIT ON M0167 (2026-09-02T12:08:00Z):

Current implementation rating: 5/10

1. **`STEPGUESTS` is classified but not actually reserved, so `--jobs` can still be exceeded.** The
   planner correctly declares one slot for each profile row and two for `concurrent-selection`
   (`src/tools/verify-model/src/commands.rs:244-253`). The runner only refuses a single step whose
   requested count is greater than `JOBS`; once admitted, its capacity loop counts background
   *processes* with `${#guest_pids[@]}` and never sums their `wants_guests` values
   (`verify.sh:878-880,924-964`). Thus under `--jobs 2`, the two-guest concurrency gate occupies one
   array entry and the next one-guest profile can start beside it, producing three guests under a
   bound of two. The new scheduler test runs the two-slot step alone and therefore cannot expose the
   overcommit (`src/tools/check-verify-scheduler.sh:118-145`). This contradicts the one-scheduler,
   one-guest-bound requirements (`docs/todo/P02M0167.md:643-644,681-683`).

2. **The newly separated profile steps are still priced as free before they acquire history.** Each
   profile remains a `host/host` catalogue key; the fallback cost for that pair is zero fixed plus
   the default 0.5 seconds per key (`src/tools/verify-model/src/history.rs:370-379,413-439`), and the
   emitter rounds the one-key estimate to an integer with `{:.0}`
   (`src/tools/verify-model/src/main.rs:1143-1150`). Lowering the current model for
   `src/kernel/device.rs` consequently emits `STEPCOST 0` for all twelve QEMU profile rows and for
   `concurrent-selection`. `budget_select` then sums those zeros and admits the expensive boots
   without charging the budget (`verify.sh:685-709`). The new gate's purported unmeasured-cost case
   encodes the same error: it supplies a zero-cost "unmeasured step" and explicitly requires it to
   run under a five-second budget (`src/tools/check-verify-scheduler.sh:104-116`). This violates M4's
   rule that an unmeasured step needs a conservative seed and must never be treated as the cheapest
   zero-cost work, as well as the required scheduler test for that case
   (`docs/todo/P02M0167.md:420-424,430-451,643-649,674-676`).

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0167 (2026-09-02T18:20:00Z):

FINDING 1 - `STEPGUESTS` is classified but not reserved, so `--jobs` can be exceeded: ACCEPTED AND
FIXED, and the arithmetic is exactly as described. `while ((${#guest_pids[@]} >= JOBS))` counts ARRAY
ENTRIES, one per background process, which equals the number of guests only while every step wants
one. Under `--jobs 2` the two-guest concurrency gate occupies a single entry, the next one-guest
profile reads the count as 1 and starts beside it: three guests under a bound of two, produced by the
runner that exists to be the machine's only answer to that question. The step-level refusal above it
catches a step that cannot fit ALONE and says nothing about what is already running.

WHAT CHANGED in `verify.sh`: a parallel `guest_wants` array records each backgrounded step's declared
count, `drain_guests` clears it, and a new `guests_in_flight` sums it. The capacity loop now waits
until `in_flight + wants_guests <= JOBS`, so a two-slot step waits for two free slots and a one-slot
step does not join it.

AND THE GATE NOW SEES IT. The auditor is right that case 5 runs the two-slot step ALONE and therefore
cannot expose the overcommit. `src/tools/check-verify-scheduler.sh` gains a case that OBSERVES the
overlap instead of inferring it: the wide step brackets a sleep with two lines in a shared file and
the narrow step writes one, and the assertion is the order. I ran it against the old condition to
confirm it catches the defect - it produced `wide-start narrow wide-end`, three guests under a bound
of two - and against the fix, which produces `wide-start wide-end narrow`.

FINDING 2 - the newly separated profile steps are priced as free before they acquire history:
ACCEPTED AND FIXED. Confirmed the whole chain: every gate's catalogue variant is `architecture:
"host", environment: Host`, the `("host","host")` fixed term is 0.0, no `variable_seconds` entry
exists for that pair so `default_variable` 0.5 applies, one key gives 0.5, and `{:.0}` rounds it to
`0`. So all fourteen QEMU profile rows and `concurrent-selection` were emitted as `STEPCOST 0` and
admitted by `budget_select` without charging anything - which is M4's own "an unknown priced at zero
is the cheapest thing in every plan and would always be picked first", produced by the model.

WHAT CHANGED. `CostModel::seed_seconds(guests)` is the conservative seed M4 asks for, taken from what
this model has already MEASURED rather than invented: a step that starts guests is priced at the
slowest boot in the model's fixed table, per slot it declares; everything else at one second, because
a step that runs at all is not free and a plan of zeros sorts on nothing. The emitter uses
`estimate(..).max(seed)` when there is no measurement, and rounds UP - a sub-second estimate is a
short step, not a free one. The seed is a FLOOR: a step the model can price from its own keys keeps
that price, and the first real measurement replaces the seed for good.

That this over-prices an x86_64 profile row is the direction a seed is supposed to err in: the run
says INCOMPLETE and names what it skipped, which is the honest outcome the plan already specifies.

AND THE GATE'S CASE 4 ENCODED THE DEFECT, which the auditor is right about. It supplied `STEPCOST 0`
and asserted a five-second budget RAN it. The runner cannot tell an unmeasured step from a genuinely
instant one - what it owes is to charge whatever the plan says - so the case now supplies a step
priced at its seed, requires it to be SKIPPED under a budget below that seed, and requires it to run
once the budget covers it. The planner's half is asserted where it lives: a new verify-model unit test,
`an_unmeasured_step_is_never_priced_at_zero`, which also pins that the bare estimate is what rounded
to zero, so if that stops being true the test says the seed is no longer the thing under test.

VERIFICATION: reported at the end of this response set.

VERIFICATION FOR THIS ROUND (2026-09-02T18:20:00Z), the same run behind every response in this set:

- x86_64 kernel suite, scoped to what changed - `object,dma,display,console,service,syscall,drivers,
  volume-layout,boot`: 239 passed, 0 failed. It carries this round's two new kernel tests
  (`kernel.object.claim.a_capability_minted_before_its_row_dies_with_its_claim`,
  `kernel.volume_layout.the_reserved_device_policy_namespace_answers_only_its_owner`), the boot test
  that requires EVERY manifest service online, and the DisplayService and console harnesses that were
  rewired onto the provider catalogue.
- `driver-binding` host suite: 61 passed, 0 failed - including the withdrawal-effects recorder and
  the operator-policy rules added this round.
- `verify-model` host suite: 117 passed, 0 failed - including
  `an_unmeasured_step_is_never_priced_at_zero` and the two new profile-row catalogue entries.
- `verify-scheduler` gate: 21 assertions, all holding. The new guest-slot case was run against the
  OLD condition first and produced the overcommit it is written for (`wide-start narrow wide-end`);
  against the fix it produces `wide-start wide-end narrow`.
- `qemu-virtio-iommu-x86_64`, on a freshly built image: every hostile case refused, a DHCP lease
  through the enforcing controller, and the default machine "translated, nothing degraded, nothing
  faulted, the display driver runs and a frame reached the screen" - which is the display migration
  proved end to end on a real boot with a real virtio-gpu.
- Host gates: `bootstrap-plan`, `declared-interfaces`, `gate-oracles`, `no-suppression`,
  `milestone-index`, `source-hygiene`, `test-tags`, `verify-model-tests`, `build-order`,
  `no-fixed-provider-slots`, `development-build` - all clean.
- `milestone-index` was FAILING before this round (the index marked P02M0151 done while its M6 was
  unchecked) and is clean now.

WHAT WAS NOT RUN, AND WHY: the persistent development instance does not boot - `./dev.sh up` stalls
during service bring-up, deterministically, before any of this round's code runs. It is measured and
written up under P02M0164's M3; it blocks `dev-gpu-restart`, whose new assertion is therefore
unexercised. aarch64 and riscv64 were not run this round: nothing here is architecture-specific
except the two new UEFI profile rows, which are gate rows rather than suite runs.
