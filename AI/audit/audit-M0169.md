AUDITOR'S REVIEW OF PLAN M0169 (2026-08-30T16:21:53Z):

Rating: 4/10

The plan targets real, bounded defects and correctly distinguishes pre-commit refusal from a partially committed group release. It is not implementation-ready: two launch failure windows remain outside its ownership model, the grant vocabulary is still not exhaustive, and the LSIDL and scheduler work is based on incomplete readings of the current branches and comments.

## Material findings

1. **The proposed grant-vocabulary repair still leaves another declared capability ungrantable.**

   **What is wrong:** M1 adds only `Capability::Session` to `VOCABULARY` (`docs/todo/P02M0169.md:42-45`). `DevicePolicy` is also a schema capability (`src/idl/security.lsidl:32-44`) and is also absent from the 21-entry vocabulary (`src/user/services/core/src/permission_manager.rs:126-148`), even though `lsdev` requests it (`:219-228`) and PermissionManager already stores and maps its client (`:332`, `:357-365`, `:412-421`, `:1497-1499`).

   **Why it matters:** Implementing M1 exactly as written leaves a production manifest requesting authority the grant transaction can never supply. The claim that there is one canonical ordered vocabulary remains false, and governed `lsdev` behavior can still fail for the same class of defect this milestone is meant to close.

   **Correction:** Make M1 audit the vocabulary exhaustively against the capability schema and `Clients::for_capability`, adding `DevicePolicy` or assigning it to an explicitly separate owner with a justified contract. Add a governed `lsdev` regression and an exhaustiveness assertion that fails when a schema capability is not deliberately classified.

2. **Pipeline process-group creation occurs after commit and is absent from the rollback design.**

   **What is wrong:** `run_pipeline_under_manifest` calls `release_group` and returns live tasks before its caller invokes the fallible `process_group_create`; on group-creation failure the caller closes every task handle and returns `Error::Invalid` (`src/user/services/core/src/permission_manager.rs:746-770`, `:1133-1153`). The syscall can fail allocation or validation and does not require members already to be running (`src/kernel/syscall/mod.rs:2009-2041`). M2 and M3 cover release refusal/faults, but not this post-commit failure (`docs/todo/P02M0169.md:47-73`).

   **Why it matters:** A complete pipeline can be running after the broker has discarded the handles needed to kill, wait for, and reap it. This violates the milestone's central promise that a failed launch leaves no live process or resource record.

   **Correction:** Create and seal the process group over prepared process handles while cancellation is still possible, then retain the group as the cleanup owner through release. Specify recovery if release partially commits, and inject process-group creation failure to prove that no stage executes and no process, Domain, handle, or record remains.

3. **Single-process release failures lose the token before the plan's rollback owner can act.**

   **What is wrong:** `ProcessService::release` removes the prepared token before calling `Spawned::release` (`src/user/services/core/src/process_service.rs:899-909`), and the runtime consumes/closes the thread token even when `process_release` fails (`src/user/runtime/rt/src/lib.rs:2998-3005`). PermissionManager reduces every non-`Ok(true)` single release to a bare failure return (`src/user/services/core/src/permission_manager.rs:849-853`, `:997-1000`, `:1324-1327`). Cancellation can no longer find the token, while reap deliberately retains stopped records without completion until `forget` closes the record and Domain (`src/user/services/core/src/process_service.rs:429-469`, `:785-807`). M2 gives precise semantics only to group release.

   **Why it matters:** Ordinary, bounded, and selected-file single launches can leak an unreapable ProcessService record/Domain, or can have uncertain execution after the caller reports an ordinary refusal.

   **Correction:** Extend M2/M3 with typed single-release outcomes: pre-start refusal, post-removal start failure, and uncertain transport/reply. Define synchronous `forget`/Domain cleanup for confirmed failures and kill/wait/reap for uncertain starts. Fault-test all three single-launch paths, not only group release.

4. **The accepted `@rights` contract misses the stream-return dispatch branch.**

   **What is wrong:** Normal generated dispatch emits `handle_carries` guards (`src/tools/lsidl-gen/src/codegen.rs:548-617`), but stream-return methods are skipped there and their separate `<method>_open` path calls the service without the guard (`:548-550`, `:679-704`). M4's parameter-shape and denied-result rules do not explicitly reject a rights-bearing method whose result contains a stream, so validation can accept a promise that this generator branch does not enforce (`docs/todo/P02M0169.md:75-84`).

   **Why it matters:** The Definition of Done can pass while one accepted annotation reaches service code without its rights or kernel-object-type check.

   **Correction:** Either reject `@rights` on every stream-return shape in this language version or add the same guard to `_open`. Add positive, denied, and mutation fixtures that specifically compile and exercise the stream dispatch branch.

5. **Alias acceptance is inconsistent with the metadata available to code generation.**

   **What is wrong:** M4 implies that aliases resolving to the exact supported handle shape may be accepted (`docs/todo/P02M0169.md:75-80`), but guard generation matches direct `Type::Handle` (`src/tools/lsidl-gen/src/codegen.rs:573-597`). Resolver export metadata associates `kernel_type` with resources, not aliases (`src/tools/lsidl-gen/src/resolve.rs:207-219`), so local or imported alias resolution can lose the resource identity and fall back to `NO_REQUIRED_TYPE`.

   **Why it matters:** An alias can silently weaken object-type enforcement while appearing equivalent to a directly written handle in the schema.

   **Correction:** For this bounded milestone, either reject all named aliases on rights-bearing parameters or preserve the resolved resource and kernel-type identity through every local/imported alias chain. Test both local and imported aliases and mutate away the object-type check.

6. **The scheduler prose audit excludes a nearby false safety statement.**

   **What is wrong:** `src/kernel/sched/mod.rs:998-1017` accurately describes wake migration but then claims cross-core address spaces have no TLB shootdown and labels that an open Phase-2 item, while a synchronous all-core shootdown exists in `src/kernel/mem/tlb.rs:1-19`. M5 covers only the cooperative/non-migrating/top-level wording and says it changes no TLB behavior (`docs/todo/P02M0169.md:86-91`).

   **Why it matters:** The same contract-bearing scheduler documentation remains materially false after a milestone whose completion claim is that scheduler prose describes implemented migration behavior. A maintainer can make an incorrect address-space safety decision from it.

   **Correction:** Include this paragraph in the prose audit and document the current blunt global shootdown. This is a documentation correction, not a request to change TLB policy.
