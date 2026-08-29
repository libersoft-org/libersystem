# Deferred Verification And Performance Backlog

Status: NON-NORMATIVE. GIT-IGNORED. NOT A DEPENDENCY OF THE WARM-BUILD FIX.

## Why This File Exists

An earlier warm-build plan grew into a combined benchmark platform, kernel-test optimization,
planner redesign, execution-history store, guest instrumentation project and documentation audit.
Those ideas are preserved here so they are not lost, but they are deliberately outside the tracked
milestone that fixes redundant dynamic-report work.

The tracked warm-build work is limited to mode-first dispatch, a name-only inventory path, one full
ELF sweep, deterministic regression tests, error propagation, safe publication of the three reports,
direct build integration and the documentation those changes touch.

Nothing below may block that work. Promotion requires a separate tracked proposal with one owner,
bounded scope, explicit prerequisites and its own definition of done.

## 1. General Benchmark And Evidence Platform

### Preserved intent

- Reproducible before/after measurements with exact revisions, commands, tool versions and raw logs.
- Immutable campaign and decision manifests.
- Independent evidence validation and explicit ownership of thresholds.
- Protection against optional stopping, stale baselines, partial evidence publication and concurrent
  writers.
- Crash recovery, capacity accounting, receipts and provenance for long-running measurements.

### Why it was removed

The warm-build root cause is a local control-flow error. A general attestation platform, append-only
registries, cryptographic identities, cgroups, pidfds, OFD-lock hierarchies and multi-generation
recovery are not prerequisites for proving that an inventory command performs no ELF scan.

### Recommended disposition

Do not create this project unless there is a real consumer that needs a reusable certification
service or an untrusted-operator threat model. If approved, begin with a short threat model and a
minimal experiment manifest. Add signing or distributed coordination only when a stated threat or
concurrent deployment requires it.

## 2. Permission-Test Coverage And Fixture Reuse

This area now has tracked roadmap ownership split across a correctness boundary and a later
performance boundary. Maintain the normative scope there. The local backlog retains only the rule
that planner work is not automatic: remeasure the ordinary workflow after fixture reuse and open a
planner proposal only when a concrete plan decision is the measured remaining bottleneck.

## 3. Guest And QEMU Instrumentation

### Preserved intent

- Emit structured per-test started, passed, failed, timed-out and interrupted events.
- Separate guest execution time from QEMU launch, boot and suite-discovery overhead.
- Restore mutable media and state between benchmark attempts.
- Measure raw and instrumented baselines before changing planner or history behavior.
- Validate that instrumentation overhead stays within a named budget.

### Suggested project boundary

Build the event schema and a small conformance fixture first. Then instrument one guest path, verify
failure and timeout behavior, and only afterwards expand to full QEMU benchmark collection.

## 4. Verify Planner Selection And Cost Handoff

### Preserved problems

- The current handoff compares selected-test count with total-test count instead of comparing expected
  costs.
- The boundary behavior around the handoff percentage needs one exact inclusive/exclusive rule.
- Per-target runnable totals, source descriptors and cross-architecture unique counts are different
  quantities and must not be mixed.
- A plan that expands to the full suite must explain that expansion and record the actual requested
  set.
- Shared setup costs mean a group is not always the sum of its members.

### Suggested project boundary

1. Correct selection coverage first.
2. Define one typed cost input and exact handoff inequality.
3. Add group/setup terms only for fixtures that actually share work.
4. Test exact boundary, empty selection, unavailable history and full-suite expansion.
5. Surface the decision in `verify.sh --explain` and machine-readable output.

## 5. Execution Results, History And Crash Recovery

### Preserved problems

- Requested, started, completed, freshness-valid, cost-valid and failed are distinct states.
- A partial or interrupted run must not make an unexecuted test look fresh.
- A full-suite expansion must record all tests that actually ran.
- Cost learning must exclude invalid or incomplete observations without erasing failure evidence.
- Concurrent writers, partial records and process loss need deterministic recovery if history becomes
  a production authority.

### Suggested split

- First project: record actual execution and partial failures correctly in memory and in one append
  format.
- Second project: separate freshness, cost and failure projections.
- Third project, only if required: durable StateStore migration, compaction and crash recovery.
- Optional later hardening: supervised cgroups, pidfds and resource leases for environments that can
  support them.

A production process supervisor must not be smuggled into a planner correctness patch.

## 6. Kernel-Test Cost Calibration

### Preserved intent

- Freeze calibration inputs before fitting.
- Model base, per-test and shared-fixture costs separately where the data supports them.
- Keep fitting points distinct from held-out validation points.
- Compare model predictions with real workflow totals, not only hand-driven test selections.
- Recalibrate after fixture topology or instrumentation changes.

### Suggested project boundary

Start with a plain versioned data file, a deterministic fitter and held-out error report. Avoid a
general evidence registry until the calibration is proven useful and stable.

## 7. Independent CLI And Documentation Hygiene

### Preserved items

- Correct test-tag parsing for names containing hyphens.
- Detect documentation that invokes missing Just recipes.
- Replace stale command examples with the public root scripts.
- Distinguish logical waves, target-wave rows, manifest tools and unrelated build-cache counts.
- Update general performance tables only from fresh measurements.

### Suggested disposition

Land small correctness fixes independently. A broad documentation-liveness scanner can be a separate
tooling task; it should not block a performance fix unless the stale text describes that exact path.

## 8. Full Integration Replay

### Preserved intent

- Re-run build, guest, planner and kernel benchmarks on one final revision.
- Ensure results from independently changed subsystems still compose.
- Retain failures and superseded runs instead of selecting only favorable results.

### Recommended disposition

Create this only after the underlying projects exist. Each project should close with its own tests and
measurements; a later integration replay must not become a permanent big-bang gate for unrelated
changes.

## Deferred Fragments From Otherwise Relevant Work

The following fragments were also removed from the warm-build plan:

- generic campaign, attestation, lease and protected-receipt machinery around a simple build baseline;
- capability maps and broad performance-document audits unrelated to dynamic reports;
- general test-tag, command-liveness and performance-table cleanup;
- guest, planner and kernel evidence aggregation in the final closure step;
- crash-atomic publication and recovery for the three dynamic-report files beyond safe disposable
  output testing;
- a content-addressed ELF-metric cache before profiling the mode-dispatch and duplicate-sweep fixes.

The last item remains a possible optimization only if a post-fix explicit full report gate is still a
material bottleneck. Its cache key would need the exact ELF digest, parser identity, generator/schema
version and relevant target parameters. The name-only inventory path does not need such a cache.

## Promotion Checklist

Before moving any section into the tracked roadmap:

- name one concrete user-visible problem;
- identify the smallest source and runtime boundary that owns it;
- separate correctness work from optional performance work;
- define direct regression tests before adding infrastructure;
- record dependencies on other projects explicitly;
- keep task sizes independently reviewable and revertible;
- state what is intentionally out of scope.
