IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0176 (2026-09-09T18:10:00Z):

Scope: the four items of `docs/todo/P02M0176.md` - the derived configuration matrix and the
inventory mechanism (M1), the kernel audit by trust boundary (M2), the userspace audit by cohort
(M3), and the published decisions with bounded follow-ups (M4). Audit only: no production code was
changed. This record says what was built to make the audit checkable and what the audit found.

## What was built

- `src/tools/unsafe-inventory` (new host tool, Rust over `syn`): reads `matrix.toml` (15 rows:
  the kernel on three architectures, static userspace on three, the development closure,
  shared-image userspace on three, the WASM component and its dev-diagnostics build, the three
  loaders), enumerates each row's in-tree closure through `cargo metadata` (normal dependencies
  only; registry crates excluded and named), walks every module file with the row's `cfg` set plus
  the crate's resolved features, reads `OUT_DIR` generated files through `include!`, records
  `macro_rules!` templates with per-crate invocation counts, assigns stable ids, merges the rows,
  applies `AI/audit/unsafe-classification.toml` (group rules with file/kind/item/crate/test-only
  matchers, first match wins, and per-site overrides) and writes
  `AI/audit/unsafe-inventory.json` + `.md`. `--expand` reconciles each crate's
  `-Zunpretty=expanded` output against the source sites; `--strict` fails on an unclassified site
  or a scan problem.
- Fixtures under `src/tools/unsafe-inventory/fixtures` (a probe crate with a cfg-only, a
  macro-produced, a generated, a test-only and a baseline site, and a dependency crate) and 8 tests
  that prove each class is found, provenance and reachability are right, a missing `OUT_DIR` is a
  reported problem, ids are stable and `cfg` evaluates as rustc would.
- `AI/audit/unsafe-classification.toml`: 43 groups covering every site (0 unclassified).
- `AI/audit/unsafe-boundaries.md`, rendered by `AI/audit/render-unsafe-boundaries.py` from the
  JSON so its numbers are never typed by hand: the matrix, totals, the kernel by trust boundary
  (18 groups with invariant, establisher, rows and falsification evidence), userspace by cohort
  (21 groups), findings by severity, the expansion reconciliation, the completeness fixtures.
- `docs/todo/P02M0178.md` (new): the one corrective item the audit opened - narrowing the
  runtime's propagated `unsafe` signatures (F1) and bounding the kernel's `read_user<T>` (F2);
  registered in `docs/todo/TODO.md`. F3 (the AArch64 bring-up block driver's untranslated DMA) is
  attached to P02M0173 M3, which already owns it.

## What the audit found

- 5732 sites; 4775 reachable in a shipping row; 936 test-only; 30 macro templates; 0 generated
  (the generated files carry data and safe code - measured, and the fixture proves a generated
  boundary would be seen).
- By category: required-caller-enforced 1016, contained-implementation 2290, overly-broad 1937,
  abi-linkage 489, unclear-defect 0.
- No confirmed unsoundness or material defect in shipped code (section 5 of the report says what
  was checked to say so). The material finding is the safety STORY: 1932 reachable userspace sites
  are propagation from the runtime's `unsafe fn` wrapper signatures.
- The expansion reconciliation surfaced one thing a source scan cannot see and which is NOT a
  boundary of this tree: every `derive(Clone, Copy)` on this nightly emits
  `unsafe impl ::core::clone::TrivialClone`, `#[automatically_derived]`; the scanner skips derived
  impls so the comparison is like with like.

## Limits, stated

- The aarch64 and riscv64 kernel rows report a scan problem: their newest `OUT_DIR` predates the
  build script that writes `dma_registry.rs` (those kernels have not been rebuilt since P02M0172
  changed the build); the problem clears with the port rebuild at the end of the batch and is
  reported rather than silent until then.
- The expansion reconciliation compares per-kind counts per crate, not sites one by one.
- The classification's invariants are stated per group (the plan permits grouping by shared
  invariant); the report's kernel table names the falsifying gate or suite for each.

## Verification

- `cargo test` in `src/tools/unsafe-inventory`: 8 passed.
- `cargo run -- --repo ../../..` (offline): the inventory in 59 s, 0 unclassified.
- `cargo run -- --row wasm-component --expand`: the reconciliation ran (macro-produced
  `unsafe-impl` from derives reported, then excluded); `--row kernel-x86_64 --row loader-x86_64
  --expand` ran to completion (exit 0) before the derived-impl exclusion; the rerun with the
  exclusion and the corrected closure was started and is recorded when it finishes.
- `bash src/tools/check-milestone-index.sh`: clean after the new milestone was added.

## Verification, continued (2026-09-09T19:20:00Z)

- `cargo run -- --row kernel-x86_64 --expand` with the derived-impl exclusion: 28 crates expanded and
  compared, no failure; 27 reconcile exactly; the kernel crate reports four kinds higher in the
  expansion (`extern-abi-fn` 14 -> 248, from the four interrupt-stub templates now recorded as
  macro-template sites; `unsafe-fn` +4, `unsafe-block` +3, `raw-pointer` +5 from their bodies) and
  nothing lower. The report's section 6 carries these numbers.
- The scanner now records `extern` in a template as an `extern-abi-fn` producer (4 kernel
  templates found) and skips `#[automatically_derived]` impls; `cargo test`: 8 passed.
