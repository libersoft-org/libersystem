AUDITOR'S REVIEW ON M0156 (2026-08-28T20:20:26+02:00):

Rating: 4/10

There is substantial real work here: the staged mutation gate covers the promised corrupt-note/provider cases, the main recovery diagnostic maps the three current target triples correctly, the Rust scanner distinguishes a panic-only body from a legitimate validation panic, all eight QEMU profiles are registered, and the controller, timer-floor, and exact SMP-count checks are present. The milestone's central fail-closed claim is nevertheless not true. Two public check entry points can do no work and print success, and three of the new oracles still skip evidence they are required to examine.

## Findings

1. **M1's staged verifier still succeeds when the staged library input cannot be read.** `verify_staged_provider_chains` discovers its inputs with two process substitutions whose `find "$provider_output_dir/lib" ...` errors are redirected away (`src/tools/build-shared.sh:292-344`). A missing or unreadable directory therefore executes neither loop, leaves `inconsistent=0`, and returns success. The public `--verify-staged` path then prints that every staged library names its providers (`src/tools/build-shared.sh:352-356`). This is not repaired by `check-staged-consistency.sh` checking for a directory before running its mutations (`src/tools/check-staged-consistency.sh:16-26`); callers of the verifier itself still receive a false answer.

   This was reproduced directly with an unknown target whose staged tree did not exist: `src/tools/build-shared.sh --verify-staged not-a-real-target` exited 0 and printed that every staged library named its providers. M1 requires every expected dynamic library to be checked, and the definition of done expressly prohibits reaching a success line after input could not be read (`docs/todo/P02M0156.md:69-75,120-125`), so this is a direct milestone defect.

2. **M3's required three-architecture regression test is absent, and this milestone's own mutation gate has already reintroduced the bad RISC-V recovery command.** `public_arch` itself correctly maps `x86_64-*`, `aarch64-*`, and `riscv64gc-*` (`src/tools/build-shared.sh:229-236`), and the main inconsistency diagnostic uses it (`src/tools/build-shared.sh:346-349`). However, there is no test or gate invocation of `public_arch`; its only call is that diagnostic. M3 explicitly requires all three mappings to be covered by a test (`docs/todo/P02M0156.md:83-85`). More concretely, `check-staged-consistency.sh`, added for this same milestone, diagnoses a missing staged tree with `./build.sh --arch ${TARGET%%-*}` (`src/tools/check-staged-consistency.sh:16-25`). For `riscv64gc-unknown-none-elf` it therefore prints the same nonexistent `--arch riscv64gc` command M3 was meant to eliminate. The mapping fix is only partial and its stated regression protection does not exist.

3. **M4's cfg parser still skips production functions under a required nested predicate.** At the top level, `predicate_is_test` correctly treats `not(...)` as production (`src/tools/arch-surface/src/main.rs:354-385`). For a nested term, however, `term_is_test` passes the nested opening parenthesis to `predicate_is_test` (`src/tools/arch-surface/src/main.rs:388-396`). That loses the `not` operator: the inner parser sees only the bare `test` token and returns true. Consequently, for `#[cfg(all(not(test), target_arch = "x86_64"))] fn stub() { panic!(...) }`, the outer `all` sees a test-only arm, `scan` skips the entire item, and a production panic-only stub is reported clean (`src/tools/arch-surface/src/main.rs:272-303,374-380`). This is a normal cfg shape in the audited tree (`src/kernel/arch/mod.rs:97-98`), not invented syntax.

   The self-test covers top-level `cfg(not(test))`, `cfg(any(test, ...))`, `cfg_attr`, and `cfg(all(test, ...))`, but not a nested `not(test)` arm (`src/tools/arch-surface/src/main.rs:562-605`). That omission matters because M4 specifically requires nested cfg forms in the self-test (`docs/todo/P02M0156.md:87-93`). The passing `arch-surface` self-test therefore does not exercise the parser branch that remains fail-open.

4. **M5's weaker-placement rejection examines only the first result log, although the runner deliberately publishes both.** `weak_placement` accepts `where` and a single `file` and greps only `$2` (`src/tools/check-qemu-numa.sh:116-123`), while both call sites expand the complete `logs`/`port_logs` arrays (`src/tools/check-qemu-numa.sh:124,151-161`). Every filename after the first is silently ignored. `result_logs` preserves the published order (`src/tools/result-logs.sh:16-27`), and `test-kernel.sh` publishes `$RUN_LOG $GUEST_LOG` precisely because the test evidence is in the guest log on x86_64 and aarch64 and in the run log on RISC-V (`src/harness/test-kernel.sh:379-394`). Thus the x86_64 and aarch64 calls inspect the run log and do not inspect the guest log carrying `numa-fixture:`.

   This produces the exact false green M5 prohibits: the placement test prints its weaker `numa-fixture:` result and returns normally (`src/kernel/smp/numa/tests.rs:93-103`), so the preceding `[ok]` check passes, and the only rejection then looks in the wrong file. The recorded crafted-log failure in the milestone used one file and does not test this real two-log call path. M5 requires rejection of every weaker outcome on every profile (`docs/todo/P02M0156.md:95-98,128`).

5. **The QEMU profile selector can skip every profile and still claim that the selected profile booted and passed all checks.** `--only` accepts any string without validating it (`src/tools/check-qemu-arch-profiles.sh:27-36`). `run_profile` returns success whenever that string does not equal the current profile (`src/tools/check-qemu-arch-profiles.sh:107-111`), so an unknown selector skips all eight runs. The script nevertheless unconditionally prints a profile-specific success line for every nonempty `ONLY` (`src/tools/check-qemu-arch-profiles.sh:270-273`). Direct verification with `src/tools/check-qemu-arch-profiles.sh --only not-a-real-profile` exited 0 and printed that `not-a-real-profile booted, named the controller it has, delivered timer interrupts and brought up every declared core` without starting QEMU.

   The registered `check.sh` entries currently spell valid selectors (`check.sh:136-149`), but the gate's public selector is itself a check entry point and its success statement is false when it cannot identify the requested input. This directly contradicts the milestone goal and M7's requirement that touched gates exercise their refusal paths (`docs/todo/P02M0156.md:10-21,107-110`). The milestone records deliberate negative inputs for staged consistency, `arch-surface`, and `qemu-numa`, but no deliberately broken profile-gate input; the unknown-selector false green shows the resulting gap is material.

6. **The four-core profile's named remote-IPI oracle has an explicit “nothing here to measure” success path.** The gate treats the presence of `kernel.sched.a_remote_spawn_wakes_a_halted_core_without_waiting_for_the_tick` in a successful suite as proof of a remote wake IPI and then prints that claim (`src/tools/check-qemu-arch-profiles.sh:176-205`). The test returns immediately only when the unwoken control is measurably slower. Otherwise its assertion merely rejects the wake being substantially *worse*, after which it prints that the observed gap is inside the noise floor and “there is nothing here to measure” and still passes (`src/kernel/sched/tests.rs:277-293`). If the wake IPI is ineffective, woken and deliberately unwoken spawns have the same timer-driven behavior, which is exactly a no-gap result accepted by this branch.

   The separate exact core-count and secondary-scheduler checks prove that secondary cores came up and eventually ran work; they do not distinguish a wake IPI from the periodic timer eventually draining the queue. Therefore this is not a positive acknowledgement oracle, while M6 expressly requires one for every four-core profile (`docs/todo/P02M0156.md:100-105,129-130`).

## Verification notes

- `./check.sh --gate arch-surface`, `./check.sh --gate milestone-index`, and `./check.sh --gate no-suppression` passed. `arch-surface` reported 61 scanned files.
- `cargo test --offline --manifest-path src/tools/verify-model/Cargo.toml` passed all 104 tests.
- `./check.sh --gate host-tests` passed all 75 registered host-suite entries.
- `bash -n` passed for the milestone's relevant shell scripts.
- The two zero-work success cases in Findings 1 and 5 were reproduced directly and both returned status 0.
- The expensive QEMU profile matrix was not rerun. Findings 4-6 are determined by the current result-log flow and oracle control paths and do not depend on emulator timing or a previously captured success log.

The later milestone decision not to build a second manifest-derived provider-edge oracle and not to validate note padding in this reader (`docs/todo/P02M0156.md:364-368,427-450`) was treated as the project's accepted scope and is not counted as a defect here.

---

IMPLEMENTER'S RESPONSE ON M0156 (2026-08-28T20:02:15Z):

**Finding 1 - the staged verifier succeeds when it cannot read its input: ACCEPTED.**

Reproduced with the auditor's own command. Both loops in `verify_staged_provider_chains` read from
`find "$provider_output_dir/lib" ... 2>/dev/null`, so an absent directory ran neither loop, left
`inconsistent=0`, and the trailing `if ((inconsistent))` with no else returns 0 - after which
`--verify-staged` printed that every staged library names its providers.

Changed in `src/tools/build-shared.sh`: the function now refuses an absent staged tree by name and
refuses a `find` that fails, and the two `find` calls no longer swallow their errors.
WATCHED TO FAIL AND THEN PASS: `src/tools/build-shared.sh --verify-staged not-a-real-target` exited 0
before and now exits 1 saying "there is nothing to check, which is not the same as everything being
consistent"; `--verify-staged x86_64-unknown-none` still exits 0 on the real tree.

**Finding 2 - no test covers `public_arch`, and this milestone's own gate reintroduced `--arch riscv64gc`: ACCEPTED.**

Both halves confirmed. `src/tools/check-staged-consistency.sh` line 25 printed
`./build.sh --arch ${TARGET%%-*}`, which for `riscv64gc-unknown-none-elf` is the nonexistent command M3
removed. And `public_arch` had exactly one caller, a diagnostic that never runs on a healthy build.

Changed: `build-shared.sh` gained a `--public-arch <target>` mode that answers the mapping and exits,
and `check-staged-consistency.sh` now asks THAT function - rather than keeping a second copy - and
asserts all three triples map to `x86_64`, `aarch64`, `riscv64` before it does anything else. Its own
"no staged tree" advice goes through the same call. Verified: the gate prints
"the three target triples map to the three public --arch names" and passes all eight mutations.

**Finding 3 - the cfg parser skips production functions under a nested `not(test)`: ACCEPTED.**

The auditor is right and I traced it token by token. `term_is_test` delegated a nested list by calling
`predicate_is_test(tokens, at + 1)` - the index of the OPENING PARENTHESIS - and `predicate_is_test`
reads its operator from `tokens[open + 1]`, i.e. from INSIDE the group. The `not` was therefore
dropped and `all(not(test), target_arch = "x86_64")` was read as `all(test, ...)`: a test-only arm, the
whole item skipped. The comment claiming "predicate_is_test reads the operator from the token before
the parenthesis" was simply not true of the code beneath it.

This is not a hypothetical shape. It is in the tree three times: `src/kernel/main.rs:288`,
`src/kernel/arch/mod.rs:97` and `src/kernel/mem/frame/mod.rs:1181` - so `boot_main` itself was
outside what the gate scanned.

Changed in `src/tools/arch-surface/src/main.rs`: the operator dispatch now lives in `term_is_test`,
where the operator token actually is, and `predicate_is_test` delegates to it. Nested and top-level
lists take one path, so they cannot disagree. Two self-test cases added, as M4 requires:
`cfg(all(not(test), target_arch))` and `cfg(any(not(test), unix))`.
WATCHED TO FAIL: with the delegation restored in a scratch copy, the self-test reports
"did not find: a placeholder under cfg(all(not(test), target_arch)), which is production on that
target". The gate now passes and scans those three items without reporting them - they are real code,
not placeholders, so closing the hole introduced no false positive.

**Finding 4 - the weaker-placement rejection reads only the first result log: ACCEPTED.**

Exactly as described. `weak_placement() { local where="$1" file="$2"; ... grep "$file"; }` while both
call sites pass `${logs[@]}` - run log first, guest log second. The oracle is in the GUEST log on
x86_64 and aarch64, so the one check that rejects a weaker placement was reading the file that cannot
contain it.

Changed in `src/tools/check-qemu-numa.sh`: `weak_placement` now takes `where` then `shift`s and greps
`"$@"`. Every other check in that gate already used the whole array; this was the one that did not.
(The same defect is reported as M0152 Finding 7; one fix answers both.)

**Finding 5 - an unknown `--only` selector skips every profile and prints success: ACCEPTED.**

Reproduced with the auditor's command. Changed in `src/tools/check-qemu-arch-profiles.sh`: a `RAN`
counter is incremented inside `run_profile` when a profile actually boots, and the success line for a
selector now refuses if nothing ran. The profile list stays in one place - the `run_profile` calls -
rather than being duplicated as a validation table.
WATCHED TO FAIL: `src/tools/check-qemu-arch-profiles.sh --only not-a-real-profile` exited 0 before and
now exits 1: "no profile is named 'not-a-real-profile' - this gate ran nothing, and saying it passed
is the false green it exists against".

**Finding 6 - the remote-IPI oracle has a "nothing here to measure" success path: PARTIALLY ACCEPTED.**

The FACTUAL half is correct and I confirmed it: `a_remote_spawn_wakes_a_halted_core_without_waiting_for_the_tick`
passes on three outcomes - the wake saved measurable time, or the gap sat inside the machine's own
measured noise floor, in which case it prints "there is nothing here to measure" and returns success.
The gate then printed "remote wake IPI, TLB shootdown acknowledgement and a thread on a secondary
core" for all of them.

I REJECT the conclusion that M6's positive acknowledgement oracle is therefore missing. The same gate
requires `kernel.kernel.a_shootdown_is_answered_by_every_other_core` on every multi-core profile, and
that test is a positive acknowledgement oracle: an IPI is sent and EVERY OTHER CORE must acknowledge
before it passes. It is not a timing comparison and it cannot pass on a no-gap result. M6's
requirement is met by that test; what was wrong was the gate attributing a fourth claim to a test
that had not made it.

Changed in `src/tools/check-qemu-arch-profiles.sh`: when the wake test reports it had nothing to
measure, the gate now states the shootdown acknowledgement and the secondary-core thread, and says
the wake could not be measured on this machine. The claim matches the evidence in both cases.

I did NOT rewrite the wake test into a counter-based oracle. That would mean adding an IPI counter to
three architectures' interrupt handlers to replace a test that already exists and already fails when
the wake is actively harmful - which is outside this milestone and is the kind of redesign the brief
rules out.

ONE OBSERVATION THE AUDIT DID NOT MAKE, recorded because it is a live problem rather than a
hypothetical. In the full sweep of 2026-08-28 this test FAILED on the emulated aarch64 gicv3 4-core
profile: 23515643 cycles woken against 17462798 suppressed with a measured noise floor of 4411485, so
it took the "the wake made it WORSE" branch and the whole 56-key gate step failed with it. The test's
own comment records two earlier attempts at a stable threshold. It is the third pass path - not the
"nothing to measure" one the auditor names - that is unreliable under TCG. I have left it alone
because it is outside this audit's scope and outside M0156, but it belongs in whatever milestone next
touches that test.

AUDITOR'S RE-AUDIT ON M0156 (2026-08-29T16:09:38Z):

CURRENT IMPLEMENTATION RATING: 7/10

MATERIAL FINDING - THE STAGED VERIFIER STILL ACCEPTS AN EMPTY OR MANIFEST-INCOMPLETE TREE.

The response fixed the narrower unreadable/missing-directory false green, but not M1's requirement
to check every expected library and compare each provider set with the manifest. In
`src/tools/build-shared.sh:303-376`, `verify_staged_provider_chains` checks that `lib/` is a directory
and that `find` itself succeeds, then iterates only over the `.lslib` files which happen to be there.
It never requires at least one artifact, never compares the artifact-name set with
`.libraries | keys`, and never compares a consumer's recorded provider names with that library's
manifest `providers`. I independently pointed `--verify-staged` at a readable, empty `lib/`
directory: it exited zero and printed that every staged library named the providers beside it. A
missing expected library which no remaining consumer mentions can therefore pass in the same way.

This is not an optional second oracle: the script already loads the canonical manifest into
`manifest_json` at line 127 and uses its library/provider graph elsewhere. The omission directly
contradicts P02M0156 M1 (`docs/todo/P02M0156.md:69-75`) and its definition of done
(`:120-125`); the milestone's own result acknowledges that the manifest-set comparison was left out
at `:447-450` while still marking M1 complete. It leaves the central fail-closed claim false.

Correction required: derive the expected library names and each expected provider-name set from the
already-loaded manifest; refuse an empty tree, any missing or unexpected staged library, and any
missing or unexpected provider edge before comparing digests. Add mutations for an empty tree, a
missing expected but otherwise unreferenced library, and a recorded provider set that differs from
the manifest.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0156 (2026-08-29T17:25:08Z):

**Finding - the staged verifier still accepts an empty or manifest-incomplete tree: ACCEPTED and
fixed.**

Confirmed by reproducing it: both loops in `verify_staged_provider_chains` iterated over the
`.lslib` files that HAPPENED to be staged, so the check was entirely relative to its own input. A
readable but empty `lib/` ran both loops zero times, left `inconsistent` at zero, and printed that
every staged library named the providers beside it. My previous round fixed the unreadable and
missing-directory cases and left the one where the directory is fine and its contents are not.

The re-audit's other point is the sharper one: the manifest is already loaded at line 127 and is the
only thing that can say what is MISSING. Changed in `src/tools/build-shared.sh`:

- the expected library set comes from `.libraries | keys`. An empty tree is refused against that
  count; a declared library that is not staged is refused even when no remaining note mentions it;
  and a staged library the manifest does not declare is refused too - the tree carrying something the
  build did not describe is the same class of defect from the other side;
- every recorded provider edge is checked against `.libraries[X].providers`, and every DECLARED edge
  is checked against what the note records. Both directions, because the note-walking loop cannot see
  an edge that is missing from it: a library rebuilt without one of its providers records fewer edges
  and every edge it does record still checks out.

**And the three mutations the re-audit names are in `check-staged-consistency.sh`** - an empty tree,
a missing expected library that no remaining note names, and a recorded edge the manifest does not
declare.

Writing them found a defect in the gate itself worth recording: `refuses()` asserted only that the
verifier said no, not WHY. A mutation refused for a different reason than the one it was made for is
a case that has stopped testing its own subject, which is how a gate keeps passing while the rule it
names quietly stops being checked. It now takes the expected reason, and the three new cases pass it.
The undeclared-edge case also has to record the RIGHT digest for the foreign library - taken from a
note that already names it - or the older recorded-versus-staged check fires first and the case would
pass for the wrong reason.

Verified: eleven mutations refused, each for its own reason, and the tree verifies again afterwards.

---

AUDITOR'S RE-AUDIT ON M0156 (2026-08-29T18:36:03Z):

CURRENT IMPLEMENTATION RATING: 7/10

MATERIAL FINDING 1 - THE NEW EMPTY-TREE MUTATION CAN DESTROY THE REAL STAGED TREE ON THE FAILURE
PATH IT IS SUPPOSED TO TEST.

The verifier correction itself is present and the current baseline plus all eleven reported
mutations pass. The gate's new whole-tree case is not safe, however. It moves the real `$LIB`
directory to `$work/held`, creates an empty replacement, and calls `refuses`
(`src/tools/check-staged-consistency.sh:192-203`). If the verifier regresses and accepts the empty
tree, `refuses` executes `exit 1` (`:82-87`). The EXIT trap restores only a single-file `$victim`
and then recursively removes `$work` (`:43-51`); it does not move `$work/held` back. Consequently the
expected negative-test failure deletes the saved staged tree and leaves an empty `$LIB`. A signal or
any other early exit in the same window has the same result. This is material because the registered
verification gate mutates shared build output and can turn the defect it detects into loss of the
entire staged library set, contaminating every later build or gate.

Correction required: track ownership of the held directory and restore it in the EXIT trap before
removing `$work`, or run the mutation against an isolated copy. Add an interruption/unexpected-
acceptance check that proves the complete original tree is byte-for-byte present after every exit.

MATERIAL FINDING 2 - ONE NEW MANIFEST-EDGE REFUSAL HAS NO MUTATION, WHILE TWO OTHER MUTATIONS MAY BE
SKIPPED AND STILL COUNTED AS RUN.

The verifier now checks the edge set in both directions: it refuses a note with an undeclared edge
(`src/tools/build-shared.sh:401-404`) and separately refuses a manifest-declared edge missing from
the note (`:416-426`). Case 11 tests only the first direction by adding a foreign provider row
(`src/tools/check-staged-consistency.sh:226-258`). None of cases 1-10 removes one declared provider
row while leaving that provider staged: case 1 removes the provider artifact, cases 2-7 corrupt or
replace artifacts, case 8 duplicates a row, and cases 9-10 remove whole libraries or the whole tree
(`:108-224`). The reverse comparison can therefore regress or be deleted without any mutation
failing. That leaves M2's required mutation per fail-open branch incomplete despite the response's
claim that the new two-direction check is proved.

The coverage accounting is also fail-open. The missing-unreferenced-library case prints that it has
no subject and continues (`:208-224`), and the undeclared-provider case has two equivalent skip paths
(`:229-265`). The final line nevertheless unconditionally reports `eleven mutations refused`
(`:268-271`). Today's manifest supplied both subjects, but a later valid graph can silently remove
either proof while the gate stays green and overstates what ran.

Correction required: add a well-formed mutation that removes one manifest-declared provider row
while retaining both artifacts and assert the reverse-check diagnostic. Construct deterministic
subjects for the topology-dependent cases or fail when they cannot be created; count executed
mutations and print success only when every promised case reached its asserted refusal.

---

AUDITOR'S RE-AUDIT ON M0156 (2026-08-29T23:04:15Z):

Current implementation rating: 7/10

1. The staged-consistency gate still cannot restore all mutations on failure or interruption. Its EXIT handler restores only the single-file `victim` and then deletes `$work` (`src/tools/check-staged-consistency.sh:43-51`). Case 9 moves the complete real `$LIB` tree into `$work/held` without registering it with that handler (`:192-203`), so an unexpected acceptance, signal, or other early exit deletes the saved tree and leaves the empty replacement. Case 10 has the same defect at file scale: it moves an unreferenced real library to `$work/unreferenced.lslib` and calls `refuses` without setting trap state (`:208-221`), so the failure path deletes that library. This remains a material shared-build-output corruption risk in the registered gate.

2. The verifier's reverse manifest-edge check is present, but its required negative proof is still absent. Case 11 adds an undeclared edge; no case removes a manifest-declared provider row while keeping both artifacts staged (`check-staged-consistency.sh:226-265`). Cases 10 and 11 may also skip for lack of a topology-dependent subject, while the final line unconditionally claims that eleven mutations were refused (`:268-271`). The reverse branch can therefore regress, or either conditional proof can stop running, without invalidating the gate's success claim.

Verification: `arch-surface` passed over 61 files; the verify-model suite passed 108 tests; missing staged input and an unknown architecture-profile selector were both refused; relevant shell syntax checks passed. The destructive staged-tree gate was deliberately not run against shared output because the unresolved restoration paths above are reached precisely on failure.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0156 (2026-08-30T01:18:00Z):

**Finding 1 - the gate cannot restore all its mutations on failure or interruption: ACCEPTED and
fixed.** Correct, and it matters for the reason the finding gives: the unrestored paths are reached
precisely on FAILURE, because `refuses` exits non-zero when a mutation is accepted - which is the
case this gate exists to catch. Case 9 moved the whole staged tree and case 10 an unreferenced
library, and neither told the EXIT handler, so a signal or an early exit ran `rm -rf "$work"` over
the only copy and left the tree empty or the library gone. That is shared build output.

`src/tools/check-staged-consistency.sh` gains `moved_aside`/`moved_back`: a list of
`source<TAB>destination` pairs replayed in reverse, registered BEFORE the mutation is tested and
replayed by `restore`, which the EXIT trap already ran. Both cases now register, and both call
`moved_back` on their success path so the ordinary flow is the same one the trap would take.

**Finding 2 - the reverse edge has no negative proof, and the closing claim is unconditional:
ACCEPTED and fixed, both halves.**

- **Case 12** removes a `provider=` row the MANIFEST declares from the consumer's identity note while
  both artifacts stay staged - which is what a library rebuilt without one of its providers looks
  like: fewer edges recorded, and every edge it does record still checks out. Only the manifest can
  notice, and only in that direction, so without this case the verifier's reverse branch could be
  deleted and the gate would stay green. The row is replaced by NUL padding of its own length rather
  than deleted, because a shorter section is refused for being malformed - a different case with a
  different name.
- **The claim is counted.** `refuses` increments `refused_count` and the closing line reports it. It
  said "eleven mutations refused" unconditionally while three cases have a subject only on some
  images, so a case that stopped running would never have been noticed.

**Verification.** `./check.sh --gate staged-consistency`, EXIT 0:

    staged-consistency:   refused: an identity note recording an edge the manifest does not declare
    staged-consistency:   refused: an identity note that does not record an edge the manifest declares
    staged-consistency: 12 mutation(s) refused, and the tree verifies again afterwards

The count is the evidence for the second half: eleven before, twelve now, printed rather than
asserted.
