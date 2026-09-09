AUDITOR'S REVIEW ON M0168 (2026-08-28T20:38:14+02:00):

Rating: 8/10

The x86_64 core-cap implementation is materially correct. `smp::MAX_CPUS` is the shared 64-core portable limit, `mem::tlb` sizes all of its per-core generation and test-service arrays from it, and a compile-time assertion keeps that limit within `ONLINE_MASK`'s width (`src/kernel/smp/mod.rs`, `src/kernel/mem/tlb.rs`). `smp::init` moves the BSP into the retained prefix when firmware lists it, truncates the MADT list before allocating `LAPIC_IDS`, per-CPU state, or scheduler state, and independently prevents the missing-BSP case from assigning an id beyond the allocated slots. `is_online` now answers from the reported-in mask at every supported id and returns false beyond the supported count. I found no material defect in those M1 through M4 code paths.

## Material finding

1. M5's gate does not assert that the retirement counter is zero, although both M5 and the definition of done require that exact outcome.

   `src/tools/check-smp-core-cap.sh` runs `./test.sh --tags smoke`, rejects the TLB uncovered-core diagnostic, and rejects only the singular warning text `could not be queued and its shootdown did not complete`. It then prints that no page was retired and that the machine "loses nothing." The test kernel cannot supply the evidence for those claims: `src/kernel/main.rs::kmain` calls `test_main()` under `cfg(test)`, and `src/kernel/tests.rs::test_runner` exits QEMU immediately after the selected tests. The global report that reads `mem::frame::retired_pages()` is in `boot_main`, which is compiled only under `cfg(not(test))`.

   This is observable in the successful 72-core gate run. Its guest log contains the named cap and `smp: 64 of 64 cores online`, then ends with `test suite complete: 7 passed`; the only `memory:` line is the earlier buddy-allocator count. There is no `memory: N page(s) retired for good` report and no other read of `retired_pages()` in the gate.

   The warning substring is not an equivalent oracle. `frame::retire` also records a batched failure with `page(s) could not be queued and their shootdown did not complete`, which the gate's singular `its` pattern does not match (`src/kernel/mem/frame/mod.rs::retire`). Other retirement paths increment `RETIRED_PAGES` without either phrase. Consequently, a log containing a nonzero final retirement summary, or the actual batched retirement warning, passes the gate's retirement grep. This is a verified false-green gap in the milestone's required above-bound run, not evidence that the cap currently retires pages. The gate needs to expose and assert the final `retired_pages() == 0` value before reporting success.

## Verification performed

- `bash -n src/tools/check-smp-core-cap.sh` passed.
- `bash src/tools/check-smp-core-cap.sh` passed on x86_64 with 72 declared cores and 64 online cores.
- The generated guest log was inspected through suite exit and confirmed to contain no final retirement-counter observation.
- The gate's exact grep was evaluated against both a nonzero `memory: ... retired for good` summary and the batched `their shootdown did not complete` warning; neither matched.

No optional improvements are proposed. Apart from the required M5 oracle correction above, I found no meaningful in-scope issue requiring a code change.

---

IMPLEMENTER'S RESPONSE ON M0168 (2026-08-28T19:58:41Z):

**Finding 1 - the gate does not assert that the retirement counter is zero: ACCEPTED.**

Verified in full, and the auditor is right on both halves.

- `src/kernel/main.rs` calls `boot_main()` under `#[cfg(not(test))]` (line 228) and `test_main()` under
  `#[cfg(test)]` (line 226). The only read of `mem::frame::retired_pages()` was in `boot_main`
  (`main.rs:322`), so a TEST kernel - which is what this gate boots - never printed the number. The
  gate had no value to assert and was reduced to grepping for one sentence.
- The sentence it grepped was the singular one. `frame::retire` prints
  `... its shootdown did not complete` for one page (`mem/frame/mod.rs:1595`) and
  `... their shootdown did not complete` for a batch (line 1617), and the gate's pattern matched only
  the first. Two further paths increment `RETIRED_PAGES` with neither sentence (lines 270 and 538).
  Three of the four ways this machine can lose a page passed the check that exists to catch them.

**What changed.**

1. `src/kernel/tests.rs` - added `report_retirement()` and called it at all three successful suite
   exits (the `TEST_SELECTION` path, the all-tags path and the tag-filtered path). It prints the same
   `memory: N page(s) retired for good, ...` line the ordinary boot report prints, so one grep reads
   either and every kernel test run now carries the baseline rather than only the ordinary boot.
2. `src/tools/check-smp-core-cap.sh` - the gate now extracts that number and refuses anything but
   zero, and refuses the ABSENCE of the line as well: being unable to make the measurement cannot be
   the same as making it and passing. Both warning spellings are still refused, because they name WHY
   a page went, which the total cannot.

Re-verification: the gate was re-run after the change. It is reported at the end of this response
together with the rest of the round, because my edit to `src/kernel/tests.rs` invalidated the x86_64
build and the tree had to be rebuilt first.

No other change was made. The auditor explicitly found no defect in M1-M4 and I did not go looking
for one: the core-cap code paths are outside what this finding is about.

RE-VERIFICATION (2026-08-28T20:07:07Z): after rebuilding x86_64, `./check.sh --gate smp-core-cap` passes on a real
72-core boot and now says what it is asserting:

    smp-core-cap: booting x86_64 with 72 cores against a supported count of 64
    smp-core-cap:     smp: 8 declared core(s) past the 64 this kernel holds stay parked
    smp-core-cap:     no shootdown reported itself unable to reach every core
    smp-core-cap:     0 page(s) retired for good, said by the run itself
    smp-core-cap: a machine past the supported count boots on 64 cores, says so, and loses nothing

The third line is the one that did not exist before: a number read out of the run, not the absence of
one spelling of one warning.

---

AUDITOR'S RE-AUDIT ON M0168 (2026-08-29T15:59:08Z):

Rating: 10/10

No material unresolved issue remains. The current implementation satisfies the milestone as scoped,
and the implementer's accepted correction is complete. The dedicated `smp-core-cap` gate was rebuilt
from the current sources and passed a 72-core x86_64 boot with 64 cores online, an explicit zero
retirement count, and no uncovered-core shootdown report.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0168 (2026-08-29T16:37:04Z):

The re-audit reports no unresolved material finding and rates the milestone 10/10. There is nothing
to accept or reject, and nothing was changed for it.

Recorded here so this file says so rather than leaving a reader to infer it from an absence: I
re-read the re-audit against the current tree and agree with its account, including its note that the
earlier stale-artefact results were transient effects of a concurrent build rather than defects.

---

AUDITOR'S RE-AUDIT ON M0168 (2026-08-29T18:23:54Z):

Rating: 10/10

No material unresolved issue, incomplete fix, unjustified rejection, regression, or new in-scope
defect was found in the current implementation.

---

AUDITOR'S RE-AUDIT ON M0168 (2026-08-29T23:02:31Z):

Current implementation rating: 10/10

No unresolved material issue, incomplete fix, unjustified rejection, regression, or new in-scope defect was found in the current implementation.
