AUDITOR'S REVIEW OF PLAN P02M0206 (2026-10-22T18:17:30Z):

**Rating: 4/10.** Most of the plan's diagnosis is right. The list of what it changed or dropped from the chat proposal is sound. Several items can be implemented as written: B1, B3, most of B6, C4, D3, E1, E2, F1 and F2. But the items meant to deliver THE TARGET either cannot reach it or work against it:
- 26 gates hold 6.8 hours of exclusive, serial time, and no item touches them. That alone is more than L2's three to four hours and more than the six hours L3 allows, before anything else runs.
- A1's budgets ignore plans that select everything. A change to the kernel, a harness script or the selector makes such a plan, and so does every change this milestone makes.
- B4 adds more of the profile switching the plan measures as a cost. B5's cache cannot avoid that switching. And separate staging is deferred because of a disk figure that is wrong by two orders of magnitude.
- C3 is built on an overlap incident that is misreported: two of its three gates also failed when run alone, and overlap was not the cause.
- C1's hand-written declarations have the same weakness as the catalog's guest list, which has already drifted. And every catalog change throws away the prices that A2 and C6 rely on.

I reviewed [the plan](/data/yellow/libersystem/docs/todo/P02M0206.md) against the tree at HEAD 493ef7cf, and against the evidence of the 2026-10-20/21 runs. In the tree I read:
- `guest-gate.sh`, `guest-console.py`, `image.sh`, and `build.sh`'s development switch and step mark;
- `verify.sh`'s tiers, budget, sweep and guest barrier;
- the verify model:
  - the catalog's guest and exclusive lists;
  - the tier partition;
  - the cost model and the history;
  - the model hash;
  - the registry's list of components that select everything;
  - the release-required keys;
  - a run's identity block;
- `check.sh`'s trace handling, `check-run-verdict.sh`, and `check-guest-verdict.py`'s inventory test;
- `test-kernel.sh`'s selection build, `check-hibernate-ports.py`, `check-tickless-idle.py` with the kernel's console line, and `spool_service.rs`.

In the evidence I read:
- the gate `RESULT` lines of [the rehearsal's log](/data/yellow/libersystem/.build/claude-drafts/release-5.log);
- the reruns of 2026-10-21 ([verdicts](/data/yellow/libersystem/.build/claude-drafts/reruns/unreached/verdicts.txt) and the gate logs beside them, and [the riscv64 reruns](/data/yellow/libersystem/.build/claude-drafts/reruns/verdicts.txt));
- the disk usage of `.build`.

1. **High - These items cannot meet the night and release budgets in THE TARGET. No item touches 6.8 hours of exclusive gate time, and C6 puts the longest work last.**

   The rehearsal's log records the results of 133 gates, 66,366 s in all. The 64 of them on `EXCLUSIVE_GUEST_GATES` took 55,553 s of that, which is 84 %. These gates run behind the barrier that drains every guest ([verify.sh:1176](/data/yellow/libersystem/verify.sh:1176)). So C5's width does nothing for them until the reason each one is exclusive is removed. The catalog states those reasons ([catalog.rs:1087](/data/yellow/libersystem/src/tools/verify-model/src/catalog.rs:1087)). Grouped by reason, from the same log:
   - the lab broker and cold-run namespace, or the dev-up image producer ([reason](/data/yellow/libersystem/src/tools/verify-model/src/catalog.rs:1126)): 23 gates, 15,724 s;
   - the Python cold scenarios' per-architecture cold QMP/media namespace ([reason](/data/yellow/libersystem/src/tools/verify-model/src/catalog.rs:1150)): 3 gates, 8,648 s, of which `hibernate-ports` alone took 6,842 s;
   - the development-image gates ([reason](/data/yellow/libersystem/src/tools/verify-model/src/catalog.rs:1097)): 28 gates, 27,230 s;
   - the global USB gadget (4 gates, 2,026 s), the direct x86_64 image consumers (4 gates, 1,755 s), and three single gates.

   No item removes the first two reasons:
   - B1 and B2 change `guest-console.py` and `guest_gate_run`, which only the gates built on `guest-gate.sh` and the IPv6 gate use. The lab gates drive their guests through `lab.py`, and the cold scenarios through their own Python drivers.
   - C3 makes private the x86_64 guest's monitor and QMP sockets, its host port, an injection trace and a build mark. It does not make private the lab broker namespace or the cold namespaces.
   - [C1](/data/yellow/libersystem/docs/todo/P02M0206.md:146) lists "the lab instance" as one shared resource. That keeps the 23 lab gates serial by design.

   So 26 gates and 24,372 s (6.8 h) stay serial and as slow as they are now. That alone is more than L2's three to four hours, and more than the six hours L3 allows, before any other step runs.

   Most of the lab gates also rebuild the x86_64 staging in the development profile: `lab.sh boot`, `lab.sh scenario-cold` and `dev.sh up` are among B4's paths. Under C1, that shared staging ties them to the 22 x86_64 development-image gates (14,626 s). The result is one serial chain of about 8.4 hours before B's savings, and B saves time only on the 22.

   The order makes this worse. `hibernate-ports` is one step that runs both ports one after the other ([TARGETS](/data/yellow/libersystem/src/tools/check-hibernate-ports.py:56), [loop](/data/yellow/libersystem/src/tools/check-hibernate-ports.py:395)). [C6](/data/yellow/libersystem/docs/todo/P02M0206.md:159) runs cheap work first. Cheapest-first is right for finding failures early, but wrong for total run time. A 6,842-s step that starts late ends the run no sooner than its start plus 1.9 hours.

   **Correct THE TARGET, A1 and C.**
   - Add items that remove the exclusivity reasons that hold the time:
     - a private lab broker and cold-run namespace per run (or per architecture);
     - a private cold QMP/media namespace per architecture;
     - `hibernate-ports` split into one gate per port.
   - If those are out of scope, set the L2 and L3 budgets to what the serial remainder allows.
   - Before A1's figures are adopted, write the arithmetic down for each level: the serial remainder after B, the longest single step, and the sum divided by the width.
   - Make C6's rule longest-first by measured cost on the guest slots, with cheap host work first on a separate host lane.

2. **High - A1's budgets ignore plans that select everything. That is the most common kind of change, and it includes every change this milestone makes.**

   The registry lists 15 components that select everything ([registry.toml:377](/data/yellow/libersystem/src/tools/verify-model/model/registry.toml:377)):
   - `kernel`, `abi`, `bootproto`, `wire` and `rt`;
   - `manifest`, `idl` and `product`;
   - `mkpackages`, `system-manifest`, `lsidl-gen` and `verify-model`;
   - `harness.boot` (`src/harness`), `harness.tools` (`src/tools`), and `harness.scripts` (`lib.sh`, `build.sh`, `check.sh`, `test.sh`, `verify.sh` and more) ([ownership](/data/yellow/libersystem/src/tools/verify-model/model/registry.toml:121)).

   A change to any of them makes the plan FULL. 14 of the last 30 commits (since 2026-10-12) touched such a path. Every item of P02M0206 changes `harness.tools`, `harness.boot` or `harness.scripts`, and C1 also edits `verify-model`.

   For a FULL plan:
   - **L0 is not "under ten minutes".** The inner tier sets aside only the ports, the development guests and the guest-booting gates ([tier.rs:162](/data/yellow/libersystem/src/tools/verify-model/src/tier.rs:162)). It still runs every host gate and the whole x86_64 suite. It has no default budget either: `--budget` applies only when it is passed ([verify.sh:77](/data/yellow/libersystem/verify.sh:77)). The host gates alone took more than an hour and a half of step time across the rehearsal and its reruns.
   - **L1 becomes every guest gate on every target**, which is L2.

   A1 gives no level for the plan's own changes either. Without one, they can be verified only by the L2-sized runs the milestone is meant to retire.

   **Correct A1.**
   - Define what L0 and L1 run, and their budgets, when the plan is FULL. For example: L0 is builds, host suites and the x86_64 suite, and the host gates move to L1; L1 is a fixed representative set for each target.
   - Say what verifies a change to the harness itself, P02M0206 included.
   - If harness changes should get a narrower plan, that is a change to the registry, and the plan should say so.

3. **High - B4 adds the cost the plan measures, B5's cache cannot avoid it, and the reason given for deferring separate staging is wrong.**

   The plan measures 205 to 275 s per gate for switching the profile and back ([line 25](/data/yellow/libersystem/docs/todo/P02M0206.md:25)). Against that:
   - **B4 adds that switch back to six more paths.** These are `dev.sh up`/`down`, `lab.sh boot`, `lab.sh scenario-cold`, `check-tickless-idle.py`, `check-hibernate.sh` and `check-bluetooth-independent.py`. The next step that needs the development profile then pays for the switch again. On 2026-10-21, the shipping rebuild after `development-lifecycle` had switched the staging took 127 s (`x86-build-final` in the verdicts).
   - **B5 saves only the assembly of the medium.** `image.sh` runs `build.sh` in the caller's profile before `mkimage` ([image.sh:143](/data/yellow/libersystem/image.sh:143)). The development ISO's key is computed from what that build staged, so the profile switch has to happen before the key exists. When the staging was already in the profile it needed, a complete `image.sh` run, its own build step included, took 59 to 61 s (`producer-dev-iso`, `producer-iso` and `producer-no-iommu-iso`).
   - **Restoring after every gate also stops C2 from overlapping anything.** Between switches, the development-image gates only read the staging. With a restore after each gate, every one of them writes its architecture's staging twice, so C2 can never run two of them together. That is the 28 gates and 27,230 s of finding 1 ([switch](/data/yellow/libersystem/src/tools/guest-gate.sh:37), [restore](/data/yellow/libersystem/src/tools/guest-gate.sh:43)).
   - **The disk figure is wrong.** Separate staging is deferred because it "doubles staging on a disk that is 97 % full" ([line 82](/data/yellow/libersystem/docs/todo/P02M0206.md:82)). But `.build/image` is 53 MB, and all of `.build/boot` is 2.5 GB, against 28 GB free. What fills the disk is `.build/cargo` (236 GB), `.build/logs` (62 GB) and `.build/unsafe-audit` (57 GB). The profile switch is one cargo feature on three crates ([build.sh:32](/data/yellow/libersystem/build.sh:32)), so separating the staging does not require splitting the cargo output.
   - **C1 cannot group by profile.** Its resource list names "the staging of an architecture (read or written)" but not the profile, so the scheduler has nothing to group by.

   **Correct B4, B5, C1 and the "separate staging" entry.**
   - Make the staging's profile part of the resource C1 declares.
   - Have the scheduler run the steps that need one profile together: one switch into it, one restore when the group ends, and B3's stamp to refuse anything that slips through.
   - Restore once at the end of a run, not on every path.
   - B5's single development ISO then follows without further work, because the staging stays the same between the gates that build it.
   - Weigh separate staging directories again, using the real figures.

4. **High - C3 rests on an overlap incident that is misreported. Two of its three gates also failed alone, and overlap was not the cause.**

   The plan says three host gates "failed for each other" on 2026-10-21, and that "Alone, each passed" ([line 38](/data/yellow/libersystem/docs/todo/P02M0206.md:38)). The rerun logs show otherwise:
   - **`virtio-iommu-protocol` failed alone** (11:05:43, exit 1). The cause was a compile error in its extractor ("cannot find function `map_for_device_within`"), a real regression. It was fixed at 11:28, and the gate passed at 11:28:45.
   - **`run-verdict` failed alone** (11:09:11), on the same complaint it gave beside the other gates: "a finished build left its step mark behind: .build/state/build-step.3192434". That mark came from an earlier run that was killed. `build.sh` already names the mark after its own pid ([build.sh:371](/data/yellow/libersystem/build.sh:371)). What fails is that `check-run-verdict.sh` accepts no mark anywhere in the tree ([check-run-verdict.sh:643](/data/yellow/libersystem/src/tools/check-run-verdict.sh:643)), whether it is a dead build's or a concurrent build's live one.
   - **`staged-consistency` failed beside the host lane and passed alone** (11:05:41). The failure was "an identity note carrying another format's name was ACCEPTED". No other gate failed because of its mutations, so nothing supports "mutated staging another gate was reading". Which writer changed the staging under it is not known.
   - **`check.sh`'s trace loop is a real problem, but it broke no gate.** On any failure, `check.sh` prints and deletes every `liber-injection-trace.*.log` in `/tmp` ([check.sh:706](/data/yellow/libersystem/check.sh:706)). On 2026-10-21 that put another gate's trace into the wrong log, and failed nothing.

   So C3's oracle, "the three host gates ... pass run side by side", cannot be watched failing for the reason C3 removes, and VERIFICATION requires exactly that. The oracle already passes for `virtio-iommu-protocol`. And C3's build-mark item describes what `build.sh` already does.

   **Correct the measurement text and C3.**
   - `check-run-verdict.sh` looks only at the marks of builds it started, and `build.sh` or the runner removes marks whose pid is dead.
   - Each gate keeps its own injection trace.
   - Find `staged-consistency`'s actual writer, by running it beside each candidate, before declaring its resources.
   - Build each oracle from a conflict that has been reproduced.

5. **Medium - C1's declarations will drift as the catalog's guest list already has, and nothing mechanical catches it.**

   Five gates boot a guest but are missing from `GATES_THAT_BOOT_A_GUEST` ([catalog.rs:971](/data/yellow/libersystem/src/tools/verify-model/src/catalog.rs:971)):
   - `icd-selection`, `lifecycle`, `foreign-loader-guest` and `foreign-facilities-guest` all call `guest_gate_run` ([icd-selection](/data/yellow/libersystem/src/tools/check-icd-selection.sh:75), [lifecycle](/data/yellow/libersystem/src/tools/check-lifecycle.sh:55), [loader](/data/yellow/libersystem/src/tools/check-foreign-loader-guest.sh:92), [facilities](/data/yellow/libersystem/src/tools/check-foreign-facilities-guest.sh:34)). Each one therefore also switches the x86_64 staging to the development profile and back.
   - `ipv6-peer` types into its guests through `guest-console.py`.

   The catalog gives these five a userspace build as their only prerequisite ([catalog.rs:323](/data/yellow/libersystem/src/tools/verify-model/src/catalog.rs:323), [:530](/data/yellow/libersystem/src/tools/verify-model/src/catalog.rs:530)). They are emitted as host steps with no guest slot ([commands.rs:384](/data/yellow/libersystem/src/tools/verify-model/src/commands.rs:384)). In the rehearsal, three of them took 473 to 477 s each.

   Today the drain barrier hides this, because a host step waits for every guest and then runs alone. C2 removes that wait for steps whose declarations do not conflict. A gate declared as these five are would then run beside other guests while it rebuilds their staging.

   The only existing check on the list goes from the list to the inventory ([check-guest-verdict.py:104](/data/yellow/libersystem/src/tools/check-guest-verdict.py:104)), never from the gate scripts to the list. C1's own check, which "refuses a gate with none", has the same gap. It catches a missing declaration, not a wrong one, and a wrong one is what corrupts a run once C2 overlaps steps on its word.

   **Correct C1.**
   - Derive the declarations from what a gate's script calls (`guest-gate.sh`, `run.sh`, `lab.sh`, `dev.sh`, `image.sh`, `build.sh`), or check them against what a gate is seen to write during a run.
   - Use these five rows as the first watch-fail of that check.

6. **Medium - Every catalog change discards A2's estimates and C6's kept prices, and C6's seed reverses a deliberate rule.**

   A step's recorded price is used only while the model hash is the same ([history.rs:436](/data/yellow/libersystem/src/tools/verify-model/src/history.rs:436), [:488](/data/yellow/libersystem/src/tools/verify-model/src/history.rs:488), [:530](/data/yellow/libersystem/src/tools/verify-model/src/history.rs:530)). The model hash covers ([lib.rs:136](/data/yellow/libersystem/src/tools/verify-model/src/lib.rs:136)):
   - the selector's own sources;
   - the registry;
   - the dependency graph;
   - every crate's feature definitions;
   - every catalogued check and its variants, including every kernel test id.

   So adding one kernel test, editing `catalog.rs` (which C1 does for every gate) or changing a crate's dependencies leaves every step unpriced. An unpriced step is ordered last ([main.rs:1285](/data/yellow/libersystem/src/tools/verify-model/src/main.rs:1285)). Recording the history in the tree's `.build/state`, as C6 proposes, keeps the file but not the prices. After most changes, A2's estimate will say "unpriced".

   C6 also gives an unmeasured host step a "cheap" seed. The cost model refuses to do that, on purpose. It gives an unmeasured host step no seed, because "a host gate can rebuild the entire runtime" and "an arbitrary positive minimum is not a conservative seed" ([history.rs:500](/data/yellow/libersystem/src/tools/verify-model/src/history.rs:500)). The price decides what a budget lets start ([verify.sh:876](/data/yellow/libersystem/verify.sh:876)). A cheap seed would let A1's budgets start unmeasured long host gates: on 2026-10-21, `targeted-cache` took 989 s and `run-verdict` 464 s.

   **Correct C6 and A2.**
   - Tie a step's price to the step itself (its id, its command, and the inputs that change its cost), not to the hash of the whole model.
   - Keep the ordering guess separate from the budget's price. A guess by kind of step may decide the order, but a budget still treats an unmeasured step as unpriced.

7. **Medium - B leaves out a change that is safe for most `guest_gate_run` gates without any per-gate work, and three of the four long gates B2 names first do not need B2.**

   `guest_gate_run` starts two things ([guest-gate.sh:169](/data/yellow/libersystem/src/tools/guest-gate.sh:169)):
   - the console driver, with `--seconds` (70 s on x86_64 and 600 s on the ports, unless the gate sets its own value);
   - `run.sh`, under a `timeout` of `GUEST_GATE_TIMEOUT` (200 s and 900 s, unless the gate sets its own).

   The driver stops recording when `--seconds` runs out. Of the 26 gates that call `guest_gate_run`, 23 never type `poweroff`. In each of them, the guest keeps running for the difference between the two limits, with nothing recording its console:
   - 130 s on x86_64 by default;
   - 300 s on the ports by default;
   - 120 to 300 s in the gates that set their own limits (300 s in `firmware-tree`).

   Most of these gates judge only the console log, which the driver stops recording when it exits. Four gates also touch QEMU's own output file (`camera-usb`, `firmware-tree`, `usb-legacy` and `virtio-gpu-edid`), and all four only copy it for diagnosis. For these gates, stopping the guest when the driver exits changes nothing they judge, and needs no watch-fail per gate.

   Three gates also judge the log of a simulator on the host, which keeps growing until QEMU stops:
   - `power-ups` reads `ups-sim.log`, and refuses any line about a control the simulated UPS does not have;
   - `brightness-usb` reads `monitor-sim.log` the same way;
   - `camera-usb` reads `camera.log`.
   
   They keep their window, as B2's gates with negative checks do.

   B2 names four long gates to convert first. Three of them already type `poweroff`: `camera-usb` ([script](/data/yellow/libersystem/src/tools/check-camera-usb.sh:36)), `virtio-gpu-edid` ([script](/data/yellow/libersystem/src/tools/check-virtio-gpu-edid.sh:31)) and `boot-volume-controllers` ([script](/data/yellow/libersystem/src/tools/check-boot-volume-controllers.sh:141)). Their 1,200 s and 3,901 s on the ports are the driver spinning on after the console has closed ([loop](/data/yellow/libersystem/src/harness/guest-console.py:87)). B1 alone ends that. Only `firmware-tree` needs B2.

   **Correct B.**
   - Add the item: `guest_gate_run` stops the guest when its console driver exits. The gates that judge a simulator's log on the host opt out until they name a completion line under B2.
   - Narrow B2's first list to `firmware-tree` and to the gates whose measured times show the most idle time.

8. **Medium - The measurement the milestone is judged by cannot be made as written, and it repeats the run the owner rejected.**

   [VERIFICATION](/data/yellow/libersystem/docs/todo/P02M0206.md:205) judges the milestone by "the same release rehearsal as on 2026-10-20/21, on one tree, before the first item and after the last". This does not work:
   - The items change the tree, so the two runs cannot be on one tree.
   - The "before" run is a new rehearsal of 22 hours or more. The one of 2026-10-20/21 reached 183 of its 373 steps and was stopped.
   - The owner has asked for long runs only at the very end of a job.
   - It measures only the release level, while THE TARGET sets three.

   THE TARGET also gives a milestone's end one hour ([line 63](/data/yellow/libersystem/docs/todo/P02M0206.md:63)), while A1 gives L1 ninety minutes ([line 106](/data/yellow/libersystem/docs/todo/P02M0206.md:106)).

   **Correct VERIFICATION and THE TARGET.**
   - Take the baseline from what has already been measured: the rehearsal's step durations and the reruns of 2026-10-21.
   - Judge the milestone by running each level on representative changes and comparing each run with that level's budget:
     - a change to one leaf driver or service;
     - a kernel change;
     - a harness change.
   - Add one rehearsal after the last item.
   - Use the same L1 figure in THE TARGET and in A1.

9. **Medium - Nothing starts the "nightly" runs, and A1's own working rule leaves D2 and C5 without the runs they depend on.**

   The nightly runs are promised in three places:
   - L2 is "nightly or before a large commit" ([line 108](/data/yellow/libersystem/docs/todo/P02M0206.md:108));
   - D2 runs the whole port suites "once a night";
   - C5 raises the width only "after a week of nightly runs".

   But A1's working rule says "L2 and L3 only on the owner's request" ([line 110](/data/yellow/libersystem/docs/todo/P02M0206.md:110)), and nothing in the tree starts a run at night: there is no CI job, no timer and no cron entry.

   The existing tier closest to L2 is `--sweep`, and it does not fit:
   - It runs serially ([verify.sh:444](/data/yellow/libersystem/verify.sh:444)), so C2's overlap never applies to it.
   - It refuses a dirty tree ([verify.sh:426](/data/yellow/libersystem/verify.sh:426)), and the owner works in this tree with uncommitted changes.
   - It creates a git worktree ([verify.sh:431](/data/yellow/libersystem/verify.sh:431)).

   An L2 run in place, on the other hand, is broken by any edit under `src/` while it runs.

   A1 says the levels are presets of the existing tiers ([line 96](/data/yellow/libersystem/docs/todo/P02M0206.md:96)), but it does not say which tier L1 and L2 use. And no tier today selects what L1 asks for, the whole x86_64 suite.

   **Correct A1, C5 and D2.**
   - Name the tier and the tree each level runs on. For example: a snapshot of HEAD, as `--sweep` takes, but run through the scheduler.
   - Say who or what starts L2, and when.
   - State that C5's widening and D2's whole-suite promise depend on those runs actually happening, or replace "a week of nightly runs" with a number of L2 runs.

10. **Low - D1 and D2 disagree about the suite's release key.**

    D1 says the parts "discharge the suite's key only together" ([line 165](/data/yellow/libersystem/docs/todo/P02M0206.md:165)). D2 says the whole suite still runs "in every release". The release keys are `suite.kernel / <arch> / test-guest / test` ([release-required.toml:386](/data/yellow/libersystem/src/tools/verify-model/model/release-required.toml:386)). Either way something has to change:
    - If the parts discharge those keys, a release can be completed without the whole suite, and nothing enforces D2.
    - If they do not, the parts need keys of their own. `verify-model check` compares the release list against the catalog exactly, in both directions, so that list and its check have to change too.

    D1 also has a cost it does not count. `TEST_SELECTION` is fixed when the test kernel is compiled, and that compile takes a lock for each architecture ([test-kernel.sh:340](/data/yellow/libersystem/src/harness/test-kernel.sh:340)). So every part, and every restart after a failure under D3, is one more compile that runs after the others, not beside them.

    **Correct D1.** Say which keys the parts discharge, and include the compiles in D1's cost.

11. **Low - F3 and B6 change what a gate accepts, which EXCLUDES rules out, and F3's spool case is not caused by load.**

    - **The spool test does not fail because of load.** It fails when run alone on riscv64, two times out of two (`riscv64-spool-1` and `riscv64-spool-2` in [the reruns](/data/yellow/libersystem/.build/claude-drafts/reruns/verdicts.txt)). Its 200-tick bound is the product service's own "two seconds" ([spool_service.rs:41](/data/yellow/libersystem/src/user/services/core/src/spool_service.rs:41)). Running it alone cannot help, and a new bound would be a product change. It belongs with the code defects under OPEN, not with the flakes.
    - **B6's `tickless-idle` item needs a source that does not exist yet.** The gate reads which interrupt line to count from the kernel's boot line ([check-tickless-idle.py:79](/data/yellow/libersystem/src/tools/check-tickless-idle.py:79), [main.rs:1459](/data/yellow/libersystem/src/kernel/main.rs:1459)). The ports' console driver prints no such line. Counting "the line the console's holder has after a handoff" therefore needs a new line printed by product code, and the item should say so.
    - **EXCLUDES is too absolute.** It says no item changes "what passes" a check ([line 219](/data/yellow/libersystem/docs/todo/P02M0206.md:219)). But B6's `concurrent-selection` and `tickless-idle` items change what those gates check, and F3 changes bounds.

    **Correct F3, B6 and EXCLUDES.**
    - Move the spool test to OPEN.
    - Name the line B6's `tickless-idle` item adds, and what prints it.
    - Narrow EXCLUDES to "no product feature and no weaker oracle".

What holds:
- The six heaviest aarch64 tests, and their times, match the suite's log, and none of them is tagged `Slow`.
- `guest-console.py` cannot tell a closed console from an empty read, and spins to its deadline ([loop](/data/yellow/libersystem/src/harness/guest-console.py:87)). B1's oracle is the right one.
- These match the tree:
  - a private development ISO misses `mkimage`'s cache;
  - 65 gates are exclusive;
  - every guest is drained before a host step ([verify.sh:1176](/data/yellow/libersystem/verify.sh:1176));
  - unpriced steps are ordered last ([main.rs:1285](/data/yellow/libersystem/src/tools/verify-model/src/main.rs:1285)).
- C4's gap is real, and its oracle has a home: the guard's fixture already exists ([ExistingQemuAdmission](/data/yellow/libersystem/src/tools/check-guest-verdict.py:118)).
- E1 has what it needs. A run's identity block already records:
  - the source tree;
  - the tools: cargo, rustc, the three QEMUs, `qemu-img`, `xorriso`, `sbsign` and others;
  - the firmware digests.

  So "identical" can be checked without new evidence fields.
- The plan rejects the right proposals: the lint, the blanket `poweroff`, removing the barrier blindly, and splitting the suites instead of running them whole.

Validation: read-only inspection of the plan, the sources linked above, the rehearsal's log and the 2026-10-21 rerun logs. Disk usage was measured with `du` and `df`, and the gate times were summed from the logs' `RESULT` lines. No plan or source file was modified, and nothing was built or booted.


PLANNER'S RESPONSE ON P02M0206 (2026-10-22T19:37:59Z):

I checked every finding against the tree at 493ef7cf and the logs of 2026-10-20/21 before acting on it. No finding is rejected as a whole:
- six are accepted as written (3, 4, 5, 7, 9 and 10);
- three are accepted with a figure or a wording corrected (1, 2 and 6);
- two have one part rejected (8 and 11).

I revised [the plan](/data/yellow/libersystem/docs/todo/P02M0206.md) in place. Its row in `docs/todo/TODO.md` now matches it. No source file was changed and nothing was built or booted.

1. **ACCEPTED, with a corrected figure: the night and release budgets cannot be met with the items as written.**

   The figures check out: 64 exclusive gates took 55,553 of 66,366 s, and the lab and cold-scenario gates drive their guests through `lab.py`, which no B item touches.

   The 6.8 hours assumes one lab resource shared by all targets. That is not how the files are named. `lab.py` names a cold run's files by target (`lab.py:3156-3268`), so if each target is declared separately, the 26 gates do not form one chain. The conclusion still holds, for two reasons:
   - the x86_64 lab gates alone run 14,046 s (3.9 h) in a row, which is L2's whole figure before anything else runs;
   - C3 made private only 2 of a cold run's 8 names (monitor and QMP), and only on x86_64.

   Plan changes:
   - **WHAT IS MEASURED:** a new bullet, "WHERE THE GATE TIME GOES". It gives the 84 %, the time per exclusivity reason, and why two lab runs of one target cannot run at once.
   - **New C7:** the persistent instance's names and a cold run's names move under the run's private state directory. Oracle: two cold scenarios of one target, run together.
   - **New C8:** `hibernate-ports` becomes one gate per port, with two release-required keys in place of one.
   - **New A3:** the arithmetic (serial remainder, longest step, sum divided by the width) is done before A1's figures are adopted. A figure it cannot meet is changed.
   - **C6:** the guest slots start the longest priced steps first; cheap host work runs first on its own lane.
   - **THE TARGET:** every figure is checked by A3.

   Not taken: the audit's alternative of lowering the budgets now. A3 decides that, on measured prices, once C7 and C8 have landed.

2. **ACCEPTED, with the wording corrected: A1 ignored plans that select everything.**

   "The most common kind of change" overstates it. It was 14 of the last 30 commits, about half. The substance holds:
   - for such a plan the inner tier runs every host gate and the whole x86_64 suite (`tier.rs:162-164`), with no budget unless one is passed;
   - this milestone's own changes are all such changes.

   I also checked whether `--budget` alone could bound L0. It could not: a budgeted inner run that skips anything exits INCOMPLETE before `tier-inner-finish` (`verify.sh:1277-1283`), so it writes no handoff.

   Plan changes:
   - **WHAT IS MEASURED:** a new bullet, "PLANS THAT SELECT EVERYTHING".
   - **A1, L0:** `./verify.sh --budget 600`. A key the budget skipped moves into the handoff's deferred set, owed by L1; a failed key still fails the run. The oracle is in the tier's tests.
   - **A1, L1:** ninety minutes for a plan that does not select everything. A plan that does gets an L1 the size of L2, with L2's budget.
   - **A1's working rule, and VERIFICATION:** this milestone's harness changes are verified by their items' oracles and by the gates they change, run alone. One L2 runs after the last item.
   - **EXCLUDES:** narrowing the registry's "selects everything" list. The registry's own rule is that a selector cannot vouch for a change to itself.

   Not taken: the audit's example of moving the host gates to L1. The budget does the same for any plan, cheapest priced branch first.

3. **ACCEPTED: B4 added the cost the plan measured, B5 could not avoid it, and the disk reason was wrong.**

   Plan changes:
   - **B4, rewritten:** the profile is switched when a step needs the other one.
     - Under `verify.sh`, `guest-gate.sh` no longer puts the staging back. The runner builds the profile the next step needs, knowing it from C1's declaration, and until C1 lands from B3's refusal followed by one retry. It restores the shipping profile once, at the end of the run.
     - By hand, `guest-gate.sh` still puts the staging back. The other paths leave it as they found it, and B3 refuses at once if it is wrong.
     - New oracle: the profile stamp changes twice where today the staging is rebuilt four times, and a hand run after `lab.sh scenario-cold` refuses at once.
   - **B3:** the refusal has an exit status of its own and names the profile the step needs.
   - **B5:** with B4, the key matches across consecutive development gates.
   - **C1 and C2:** the staging's profile is part of C1's declaration, and C2 runs the steps that need one profile together.
   - **"WHAT WAS PROPOSED AND IS CHANGED OR DROPPED":**
     - the "separate staging" entry is still deferred, but now because it changes every build and boot path. The old disk reason is gone (53 MB and 2.5 GB, against 28 GB free);
     - a new entry, "PUT THE PROFILE BACK ON EVERY PATH THAT SWITCHES IT", marked changed;
     - a new fact in WHAT IS MEASURED, "THE DISK".

4. **ACCEPTED: the overlap incident C3 rested on was misreported.**

   The rerun logs confirm the audit:
   - `virtio-iommu-protocol` failed alone on the extractor regression;
   - `run-verdict` failed alone on `build-step.3192434`;
   - `staged-consistency` passed alone.

   Plan changes:
   - **WHAT IS MEASURED:** the bullet on the three gates now gives the real causes, and adds the trace loop at `check.sh:706`.
   - **C3, rewritten:**
     - `check.sh` prints and removes only the trace of the gate it ran;
     - `check-run-verdict.sh` judges only the marks of the builds it started, and `build.sh` removes marks whose pid is gone;
     - the x86_64 guest's sockets and host port become private;
     - `staged-consistency`'s failure is reproduced before anything is declared for it, and goes to F2's ledger if it does not reproduce.
     - Each oracle is a conflict reproduced before its fix.
   - **The "REMOVE THE BARRIER" entry** now says what actually broke.

5. **ACCEPTED: C1's declarations would drift.**

   The five gates are confirmed: none is in `GATES_THAT_BOOT_A_GUEST`, `PROFILE_ROW_GATES`, or the concurrent list.

   Plan changes:
   - **C1:** a host test follows each gate's command in `check.sh` to the scripts it runs. It checks the declaration in both directions:
     - a guest wherever the scripts call `guest_gate_run`, `run.sh`, `qemu-run.sh`, `lab.sh`, `lab.py` or `dev.sh`;
     - a staging write wherever they call `build.sh` or `image.sh`.
   - The five gates are that test's first watch-fail.

   Not taken: checking declarations against what each gate is seen to write in a run. That needs a traced run of every gate, which is the long run this milestone exists to remove, and the static check already catches all five known cases.

6. **ACCEPTED, with a corrected detail: prices did not survive a model change, and C6's seed reversed a deliberate rule.**

   Correction: after a model change, a guest step does not become unpriced. It falls back to the seed (`history.rs:526-532`). Host steps do lose their prices, and they sort last.

   Plan changes:
   - **WHAT IS MEASURED** ("A RELEASE RUN CANNOT BE CONTINUED"): adds the model-hash binding and what it covers.
   - **C6, rewritten:**
     - the history is kept in the tree's `.build/state`;
     - a step's price is its last measured duration under its step id and command, whatever model hash it was measured under. The hash binds evidence, not cost;
     - a guest step never measured keeps the seed;
     - a host step never measured stays unpriced for a budget, keeping `history.rs:500-513`, and is ordered by its kind;
     - the longest guest steps start first, and cheap host work runs on its own lane;
     - new oracles.
   - **A2:** marks a price measured under an older model.

   Not taken: keying prices by "the inputs that change its cost". No such inputs are defined, and a budget is an estimate, not a timeout.

7. **ACCEPTED: B lacked a stop that is safe for most gates, and three of B2's first four were already ended by B1.**

   Confirmed:
   - 23 of the 26 gates never type `poweroff`;
   - their guests run 120 to 300 s past the end of their recording;
   - four gates only copy QEMU's own output;
   - `power-ups` and `brightness-usb` refuse lines in `ups-sim.log` and `monitor-sim.log`, which keep growing until QEMU stops.

   Plan changes:
   - **New B7:** the guest stops when its console driver does. Those two gates opt out until they name a completion line. The oracle is a stand-in `run.sh` that never exits, plus two gates with the same verdict before and after.
   - **B2:** its first list is now `firmware-tree`.
   - **WHAT IS MEASURED** ("A GUEST GATE COSTS ITS WHOLE BUDGET"): adds the run-on and which gates already type `poweroff`.
   - **The "END A GATE" entry** names B7.

8. **ACCEPTED, with one part rejected: the measurement the milestone is judged by.**

   Rejected part: "on one tree" meant that each run is made on one fixed tree, not that the same tree is used before and after. The sentence was ambiguous, though, and it is replaced anyway.

   Accepted:
   - a "before" run would be a new 22-hour run, and the owner wants long runs only at the very end of a job;
   - the baseline already exists;
   - one level does not measure three;
   - THE TARGET said 60 minutes for L1 and A1 said 90.

   Plan changes:
   - **VERIFICATION, rewritten:**
     - the baseline is the 2026-10-20/21 rehearsal's measurements, and no run is repeated to get it;
     - after the last item: A3's arithmetic; L0 on three changes (a leaf driver or service, a kernel change, a harness change); L1 on the first of them; and one L2.
   - **THE TARGET:** ninety minutes for L1, as in A1.

9. **ACCEPTED: the nightly runs had nothing to start them, and the levels did not name their tiers or trees.**

   I also checked that the sealed `--release` refuses a dirty tree too (`verify.sh:356`). But it runs from a snapshot, so the tree can be edited once the run has started.

   Plan changes:
   - **A1 names the tier and the tree of each level:**
     - L0 is the inner tier, on the working tree;
     - L1 is the merge tier, at the committed HEAD;
     - L2 is `--release`, from the snapshot of a clean tree, and in place only when nobody will edit the tree;
     - L3 is an L2 that continued nothing from another run, declared the release.
   - **L2 and L3 are one plan, with one figure (four hours)** in THE TARGET. OPEN asks the owner about this.
   - **D2:** "every L2 and L3".
   - **C5:** "five L2 runs".
   - **E1:** rewritten in terms of L2 and L3.
   - **A new entry, "NIGHTLY RUNS",** marked dropped.
   - **EXCLUDES:** no runner that starts levels on its own.

   Not taken: adding a runner. When the long levels run is the owner's decision.

10. **ACCEPTED: D1 and D2 disagreed about `suite.kernel`.**

    Plan changes:
    - **D1:** the parts discharge only the kernel-test keys of the tests they ran, never `suite.kernel`, so the release list does not change. The compiles, which run one after another under the build lock, are counted in D1's cost.
    - **D2:** only the whole suite discharges `suite.kernel`.
    - **D3:** each restart is one more compile.

11. **ACCEPTED, with one part rejected: F3, B6 and EXCLUDES.**

    Rejected part: "the ports' console driver prints no such line", and so a new line from product code is needed. The kernel already prints a handoff line naming the wired source it gives away (`console_uart/mod.rs:601`; COM1's at `x86_64/serial.rs:577`).

    What is not settled is whether that source is the identity the wake counters use for the driver's claim. On riscv64, the identity armed at boot is the kernel's own delivery id (`riscv64/serial.rs:333`), not the source number. So the item now establishes this first. Only if no line names the identity does the existing handoff line gain it.

    Accepted:
    - the spool test is not a load flake: it fails alone, two runs of two, on the product's own two-second answer bound;
    - EXCLUDES was too absolute.

    Plan changes:
    - **F3:** now covers only `typec-tcpci-riscv64`.
    - **OPEN:** the spool test, listed with the code defects as P02M0186's.
    - **WHAT IS MEASURED** ("FLAKES BY TIMING"): a note on the spool test.
    - **B6:** the `tickless-idle` sub-bullet now says where the identity comes from.
    - **EXCLUDES:**
      - product bounds and timeouts belong to their own milestones;
      - no oracle is made weaker: one changes only where it checks the wrong thing, and is watched failing for its old reason first.

Re-check after the changes:
- **Complete.** Every item has an oracle that can be watched failing, and THE ORDER covers all 27 items (A1 to A3, B1 to B7, C1 to C8, D1 to D4, E1, E2, F1 to F3).
- **Consistent across levels.** The levels map onto tiers that exist. THE TARGET, A1, A3 and VERIFICATION use the same figures.
- **Consistent on the profile.** B3, B4, B5, C1 and C2 describe one model for it: switched on demand, grouped by the scheduler, and refused at once when it is wrong.
- **Consistent on the suites.** D1 and D2 agree on the keys. E1 and the "CONTINUE A RELEASE RUN" entry agree on what a release may reuse.
- **Clean of audit references.** No item points at an audit.

The plan is ready to implement once the owner has answered OPEN (the budgets, and the D4 measurement) and says to start. Nothing has been implemented.


AUDITOR'S RE-AUDIT OF PLAN P02M0206 (2026-10-22T21:04:17Z):

**Rating: 6/10.** The revision resolves most of the first review:
- the exclusive-gate time has items aimed at it (C7, C8 and A3);
- L0 hands what its budget skipped on to L1;
- guests stop when their console driver does (B7);
- C3 is built from reproduced conflicts;
- prices survive a model change without inventing seeds;
- D1 and D2 agree on `suite.kernel`;
- the spool test moved to OPEN.

The planner's two partial rejections are justified:
- the old VERIFICATION sentence was ambiguous, and it has been replaced;
- the kernel does print a handoff line ([console_uart/mod.rs:601](/data/yellow/libersystem/src/kernel/arch/common/console_uart/mod.rs:601)), and riscv64's armed identity is the kernel's delivery id, not the source number ([riscv64/serial.rs:333](/data/yellow/libersystem/src/kernel/arch/riscv64/serial.rs:333)).

Five problems remain. Four are new in the revision, or were missed by both earlier passes:

1. **High - L2 now runs in a sealed snapshot, which builds everything from nothing in an empty `.build`. Neither the four hours nor the disk allow for that.**

   The revision made L2 `./verify.sh --release` on a clean tree, "so that the tree can be edited while it runs" ([A1](/data/yellow/libersystem/docs/todo/P02M0206.md:155)). That run happens in a new git worktree ([release-snapshot.sh:50](/data/yellow/libersystem/src/tools/release-snapshot.sh:50)), whose `.build` starts empty ([release-snapshot.sh:61](/data/yellow/libersystem/src/tools/release-snapshot.sh:61)). `verify.sh` moves every step there ([verify.sh:383-386](/data/yellow/libersystem/verify.sh:383)), and nothing seeds it.

   The cargo output directories are relative to the tree ([src/.cargo/config.toml:2](/data/yellow/libersystem/src/.cargo/config.toml:2), [src/kernel/.cargo/config.toml:3](/data/yellow/libersystem/src/kernel/.cargo/config.toml:3), [src/user/.cargo/config.toml:3](/data/yellow/libersystem/src/user/.cargo/config.toml:3)). So every L2 compiles from nothing:
   - the planner;
   - all three targets and their test kernels;
   - every host tool;
   - whatever else lives in `.build` (firmware, the foreign artifacts, the oracles).

   Two consequences:
   - **Time.** The baseline was an in-place run on a warm `.build`, where builds took 0.1 hours ([line 16](/data/yellow/libersystem/docs/todo/P02M0206.md:16)). A3 and the four-hour figure would price builds from that history, so they would not describe a cold L2.
   - **Disk.** The main tree's cargo output is 236 GB, and the disk has 28 GB free. Nobody has measured how much a cold build of three targets needs, and nothing checks the free space before the run starts. If the shared host's disk fills, everyone working on it is stopped.

   **Correct A1, A3 and VERIFICATION.** Choose L2's tree and state what it costs. The options are:
   - in place, on a warm `.build`, with the tree left alone during the run;
   - a snapshot whose build output is reused from the main tree, saying how;
   - a cold snapshot, with its build time priced in A3 and a free-space check before it starts.

   L3 can stay sealed, with the same accounting.

2. **Medium - L1 as `--merge` covers only the change since the last commit, and only when the commit is exactly the tested tree.**

   `--merge` completes one inner handoff at HEAD. It refuses unless HEAD's tree is exactly the tree the inner run tested ([tier.rs:360-363](/data/yellow/libersystem/src/tools/verify-model/src/tier.rs:360)), and its plan is HEAD against its parent ([tier.rs:365](/data/yellow/libersystem/src/tools/verify-model/src/tier.rs:365)). This fails in two ways:
   - **Several commits.** If a milestone is committed in several commits, every earlier handoff's deferred keys are never run: the ports, the guest gates, and whatever L0's budget skipped.
   - **Formatting at commit time.** `commit.sh` formats the changed files before it commits ([commit.sh:27](/data/yellow/libersystem/commit.sh:27)). Any file the formatter touches makes the commit differ from the tested tree, and `--merge` refuses.

   L1's widening has a gap of its own. The merge tier refuses an outcome for a key it does not own ([tier.rs:465-467](/data/yellow/libersystem/src/tools/verify-model/src/tier.rs:465)), and neither the whole x86_64 suite nor the ports' split suites are in its work set.

   **Correct A1's L1.**
   - Plan the milestone's commit range on a clean tree, with `./verify.sh --for-range BASE..HEAD`, which already exists ([verify.sh:62](/data/yellow/libersystem/verify.sh:62)).
   - Complete it with `--merge` of that run's own handoff, at the same HEAD.
   - Add the widening keys to the plan the tiers own.

3. **Medium - The staging model has one dimension, and the gates flip a second one: the bootable volume's DMA mode.**

   One shared file, `.build/boot/system-volume-bootable-x86_64.img`, is rewritten for two different modes. Its mode is recorded beside it ([mkpackages main.rs:382](/data/yellow/libersystem/src/tools/mkpackages/src/main.rs:382)) and checked when a medium is made ([mkimage.sh:704](/data/yellow/libersystem/src/harness/mkimage.sh:704)):
   - every x86_64 development-image gate builds its medium with `--dma-mode harness` ([guest-gate.sh:129](/data/yellow/libersystem/src/tools/guest-gate.sh:129)), which rebuilds the volume signed for that mode ([image.sh:143](/data/yellow/libersystem/image.sh:143));
   - every x86_64 cold run rebuilds the same volume signed `enforcing-required` ([lab.py:3145](/data/yellow/libersystem/src/harness/lab.py:3145)).

   This breaks three items:
   - **C2 and B5.** C2 groups steps by profile. Both kinds of step are development steps, so within one group the volume flips back and forth. Each flip changes the content B5's key is made from, so B5's single ISO is rebuilt after every cold run.
   - **The enforcing gates.** A gate that needs the enforcing-required volume (`perf-anchor`, the gates that recompute the default image's key) can find a harness one.
   - **C7.** C7 makes a cold run's names private, but each cold run still starts by building the target's staging in the development profile ([lab.py:3122](/data/yellow/libersystem/src/harness/lab.py:3122), [:3129](/data/yellow/libersystem/src/harness/lab.py:3129)) and, on x86_64, the shared bootable volume. Two cold runs of one target, which is C7's own oracle, write those files at the same time.

   **Correct B3, B4, C1, C2 and C7.**
   - Make the bootable volume's DMA mode part of the declared staging state, switched on demand and grouped as the profile is. Alternatively, give each mode its own volume file, which removes the flips altogether.
   - Split a cold run's build from the run itself: the build becomes a separate step, skipped when the stamps already match, so that cold runs in parallel only read.

4. **Medium - B4's interim switch (a refusal, then one retry) cannot pass in a release run, and B3 contradicts B4 on hand runs.**

   - **The retry.** In a release run, `check.sh` publishes an envelope for every gate, failed ones included, with `--if-absent` ([check.sh:689](/data/yellow/libersystem/check.sh:689)). A key takes one envelope per run ([evidence.rs:141-142](/data/yellow/libersystem/src/tools/verify-model/src/evidence.rs:141), [:154-155](/data/yellow/libersystem/src/tools/verify-model/src/evidence.rs:154)). So the refused first attempt's failed envelope stays, the retry's pass is never written, and the dossier lists the key as failed. L2 and L3 are release runs, and that is where the switching happens.
   - **The contradiction.** B3 says `guest-gate.sh` refuses a profile mismatch. B4 says that, run by hand, `guest-gate.sh` "puts the staging back as it does today", which implies it also switches into the development profile itself, as it does today.

   **Correct B3 and B4.**
   - A B3 refusal publishes no envelope and records no history. Alternatively, the runner reads the stamp before it starts a step, rather than retrying after a refusal.
   - B3 should say that `guest-gate.sh` refuses only under the runner, and still switches by itself when run by hand.

5. **Low - C1's list of calls that mean "boots a guest" is incomplete, so a check run "in both directions" would refuse correct declarations.**

   Nine gates the catalogue lists as booting a guest call none of the scripts C1 lists:
   - eight boot one only through `test.sh`: `spool-service`, `i2c-bus`, `media-import-service`, `qemu-numa`, `implementation-mutations`, `concurrent-selection`, `provider-media-order` and `smp-core-cap`;
   - `secure-boot` boots one only through `guest-verdict.py`.

   **Correct C1.**
   - Add `test.sh`, `test-kernel.sh` and `guest-verdict.py` to the list.
   - Take today's catalogue (`GATES_THAT_BOOT_A_GUEST` plus the five gates) as the check's expected answer, so the list is tested against all of it, not only against the five.

Validation: read-only inspection of:
- the revised plan and the whole history of this file;
- `release-snapshot.sh` and `verify.sh`'s release and tier paths;
- `tier.rs`;
- the cargo configurations;
- `commit.sh`;
- `check.sh`'s envelope publishing and `evidence.rs`;
- `guest-gate.sh`, `image.sh`, `lab.py`'s cold run, `mkpackages` and `mkimage.sh`;
- the scripts of the catalogued guest gates.

Disk usage was measured with `df`. No plan or source file was modified, and nothing was built or booted.


PLANNER'S RESPONSE ON P02M0206 (2026-10-23T01:42:59Z):

I checked every finding of the re-audit against the tree before acting on it. All five are accepted. For three of them I had to choose between the remedies the re-audit offered, and I say below why I chose as I did.

I revised [the plan](/data/yellow/libersystem/docs/todo/P02M0206.md) in place, and its row in `docs/todo/TODO.md` now matches it. No source file was changed, and nothing was built or booted.

1. **ACCEPTED: L2 in a sealed snapshot would build everything from nothing.**

   The re-audit is right. `release-snapshot.sh` creates a new worktree (`:50`) with an empty `.build` (`:61`), `verify.sh` moves every step into it (`:383-386`), and the cargo output directories are relative to the tree.

   **Remedy chosen: L2 runs in place.**
   - Not taken: a snapshot that reuses the main tree's build output. Cargo identifies a path package by its location, so a worktree at another path compiles everything again, whatever target directory it is pointed at.
   - Not taken: a cold snapshot for every L2. It does not fit the disk, which had 28 GB free.

   Plan changes:
   - **A1, L2:** `./verify.sh --release --in-place`, on the tree's own build output. The tree is left alone while it runs, and its dossier is marked a rehearsal, as it is today (`verify.sh:353`).
   - **A1, L3:** `./verify.sh --release` on the revision the owner names. It is sealed, builds from nothing, and continues nothing from another run. Before it starts, it checks the free space against the size A3 measured. The first sealed run starts only after the owner has freed room for it.
   - **A3:** also measures the time and the size of the sealed build.
   - **THE TARGET:** L3 is L2's four hours plus the sealed build.
   - **WHAT IS MEASURED:** a new bullet, "THE TIERS AND THE EVIDENCE".
   - **Elsewhere:**
     - the "LEVELS" entry describes the split between L2 and L3;
     - E1 says L3 reuses nothing;
     - VERIFICATION runs its final L2 in place, with the tree left alone.
   - **OPEN:** the figures are restated, and there is a new question about room on the disk for a sealed release.

2. **ACCEPTED: L1 as `--merge` covered only one commit.**

   Confirmed in four places:
   - `tier.rs:360-365` binds a merge to one commit;
   - `commit.sh:27` reformats files before committing;
   - `tier.rs:465-467` refuses outcomes for keys the tier does not own;
   - `--for-range` exists (`verify.sh:62`) and runs the inner tier (`verify.sh:829-831`), so L1 needs no new tier.

   Plan changes:
   - **A1, L1:** `./verify.sh --for-range BASE..HEAD` on a clean tree, then `--merge` of that run's own handoff, at the same HEAD.
     - BASE is the revision at which the last L1 or L2 passed. The runner records it in `.build/state`; until one has passed, it is given by hand.
     - The preset adds `suite.kernel` on x86_64 and the ports' kernel-test keys to the range's plan before the plan is partitioned, so each tier owns what it runs.
     - The tier's tests get a new oracle.
   - **A1, L0:** I dropped the change my previous response planned for the inner tier, which moved keys the budget skipped into the handoff's deferred set. L1 now plans the whole range again, so nothing L0 skipped needs carrying in a handoff. L0's INCOMPLETE exit (status 6) is its expected outcome. This makes A1 smaller.
   - **WHAT IS MEASURED:** "THE TIERS AND THE EVIDENCE" records that a merge covers one commit, and that `commit.sh` formats before it commits.

3. **ACCEPTED: the bootable volume's DMA mode flips the way the profile does, and cold runs still build.**

   Confirmed:
   - `guest-gate.sh:129`, through `image.sh:143`, signs the volume `harness`;
   - `lab.py:3145` signs it `enforcing-required`;
   - both write one shared file. Its mode is recorded at `main.rs:382` and checked at `mkimage.sh:704`;
   - `lab.py:3129` and `:3145` build every time.

   One detail I checked further: `build-shared.sh:706` takes a lock per target, but only for the shared-library stage. The cargo builds, the packaging and the volume are not serialized, so the overlap the re-audit describes is real.

   **Remedy chosen: the mode becomes a second dimension of the staging state.**
   - Not taken: one volume file per mode. Every reader of the volume would have to change (`qemu-run.sh:1695`, `:2376`, `:2895`, `:3291`, `signed-boot`, the guest-verdict tests). That is the same objection the plan already makes to separate staging directories.
   - The mode is already recorded, and `mkimage` already refuses a mismatch.

   Plan changes:
   - **B3:** retitled "THE STAGING SAYS ITS PROFILE, AND THE BOOTABLE VOLUME ITS DMA MODE". Where `qemu-run.sh` pairs the volume with a guest, it refuses a volume recorded for another mode, as `mkimage` does.
   - **B4:** the runner brings the staging and the volume to what the next step needs. At the end it restores the shipping profile and the `enforcing-required` volume.
   - **B5:** cold runs are grouped apart from the `harness`-mode gates, so B5's key holds.
   - **C1:** declares the mode.
   - **C2:** groups by profile, then by mode, with a new fixture.
   - **C7:**
     - a cold run builds nothing when the staging and the volume are already current;
     - under the runner, the build it does need is a declared step of its own;
     - its oracle now also checks that no build starts.
   - **WHAT IS MEASURED:** the mode flip is added to "THE PROFILE IS SWITCHED BACK AND FORTH".
   - **"WHAT WAS PROPOSED AND IS CHANGED OR DROPPED":** the barrier and separate-staging entries name the mode. A new entry, "ONE BOOTABLE VOLUME FILE PER DMA MODE", records why that remedy was not taken.

4. **ACCEPTED: B4's retry left a failed envelope, and B3 contradicted B4.**

   Confirmed: `check.sh:689` publishes with `--if-absent`, and `evidence.rs:141-142` and `:154-155` allow one envelope per key.

   Plan changes:
   - **B3, the refusal:** it has an exit status of its own, publishes no envelope and records no history. The step's outcome is that of its next run. New oracle: a step refused in a release rehearsal leaves no envelope.
   - **B3, who refuses:** only `test.sh` and the steps that boot the shipping staging refuse a profile mismatch. `guest-gate.sh`, `dev.sh`, `lab.sh` and the cold runs switch into the development profile themselves, as today. Under the runner they do not switch back (B4). By hand, `guest-gate.sh` still puts the staging back.
   - **B4:** the outcome of the second run is the step's. New oracle: a suite refused once ends with one envelope, from its second run.
   - **After C1 lands:** the runner reads each step's declaration before starting it, so refusals become the exception. B4 already said this.

5. **ACCEPTED: C1's list of calls was incomplete.**

   Confirmed: nine gates boot a guest without any listed call, eight of them through `test.sh` and `secure-boot` through `guest-verdict.py`.

   Plan changes:
   - **C1, the list:** adds `test.sh`, `test-kernel.sh` and `guest-verdict.py`.
   - **C1, the expected answer:** today's catalogue, that is `GATES_THAT_BOOT_A_GUEST` plus the five.
   - **C1, the oracle:** the check must also pass every gate in today's catalogue, the nine included.

Re-check after the changes:
- **The levels:** each one names a command that exists today and the tree it runs on: L0 `--budget 600`, L1 `--for-range` then `--merge`, L2 `--release --in-place`, L3 `--release`. What each one changes is stated. THE TARGET, A1, A3, OPEN and VERIFICATION use the same figures.
- **The staging state:** it has two dimensions, the profile and the volume's DMA mode. B3, B4, B5, C1, C2 and C7 treat both the same way: stamped, switched on demand, grouped, and refused at once when wrong, with no evidence left by a refusal.
- **The release evidence:** the rule of one envelope per key per run is respected. L3 continues nothing (E1), and the "CONTINUE A RELEASE RUN" entry agrees with that.
- **The order:** THE ORDER still covers all 27 items. B3 and B4 now include the mode, and their place in the order is unchanged.
- **OPEN:** it asks the owner about the budgets, room on the disk for a sealed release, and D4.
- **References:** the plan points at no audit.

The plan is ready to implement once the owner has answered OPEN and says to start. Nothing has been implemented.
