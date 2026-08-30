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

PLANNER'S RESPONSE ON M0169 (2026-08-30T19:36:07Z):

Every finding was checked against the tree before being answered. All six are accepted; one is
accepted with its remedy narrowed to the smaller of the two options the audit offered.

**1. `DevicePolicy` is also ungrantable - ACCEPTED.**

Measured rather than assumed. The capability enum in `src/idl/security.lsidl` declares 23 cases;
`VOCABULARY` in `src/user/services/core/src/permission_manager.rs:126` carries 21, and the two
missing are exactly `Session` and `DevicePolicy`. `lsdev`'s policy row grants
`Capability::DevicePolicy` (:228), the manager maps it to the tag `DEVPOLICY` (:332), stores it in
`Clients.device_policy` (:365), takes it from the bootstrap set (:1499) and can return it from
`for_capability` (:418). So the whole path exists except the one line that would send it.

The audit understates the effect, and the plan now says why. `recv_tagged` reads ONE message and
matches its tag; it does not skip. So a capability that is never sent is not a degraded read - it is
a message consumed on behalf of the next tag. `lsdev` reads `DEVICE`, then `DEVPOLICY`, then
`CONFIG`; with `DevicePolicy` absent from the vocabulary the manager sends `DEVICE` then
`CONFIG`, so the `DEVPOLICY` read eats the `CONFIG` message and every later read is one behind.
This is the same positional fragility that has already been measured elsewhere in this tree.

Plan changes: M1 is now "the grant vocabulary is exhaustive against the schema" and adds both
capabilities. The audit's alternative - assign `DevicePolicy` to a separate owner - was considered
and rejected in the plan rather than left open: it is granted by this manager, from this `Clients`
struct, through this `for_capability`, so there is no second owner to assign it to. M1 also
requires an exhaustiveness assertion over the generated capability enum, which is the part that
closes the CLASS rather than two instances of it. M4 gains a second positive fixture that launches
the real governed `lsdev` and proves its later `CONFIG` read is still aligned, because a
desynchronised handshake - not a null handle - is how this defect presents.

**2. Pipeline group creation happens after commit and is outside the rollback design - ACCEPTED.**

Confirmed at `permission_manager.rs:746-770`: `run_pipeline_under_manifest` returns live tasks,
the caller then calls `process_group_create(&tasks)`, closes every task handle in the loop that
follows, and only then tests `group < 0` and returns `Error::Invalid`. And the syscall really can
fail: `sys_process_group_create` refuses a zero or oversized count, can fail `try_zeroed_u64`,
can fail `copy_from_user_exact`, refuses a handle without MANAGE, and returns `ERR_INVALID` when
`ProcessGroup::create` declines. None of those require the members to have started, so this is
reachable with a complete pipeline running and every handle needed to kill, wait for and reap it
already closed. That contradicts the milestone's central promise directly.

Plan change: a new **M3** owns it. The preferred shape is to seal the group over the PREPARED
members before `release_group` commits, which makes a creation failure an ordinary pre-commit
refusal. Because membership is taken over `Process` handles with MANAGE and is sealed at creation,
the plan states the fallback explicitly rather than assuming the preferred shape is expressible: if
sealing over prepared members cannot be done, the caller keeps every task handle until creation has
succeeded and recovers a failure by signalling, waiting and reaping. What it may not keep is the
current shape, where the handles are gone before the failure is known.

**3. Single-process release failures lose the token before rollback can act - ACCEPTED.**

Confirmed on both sides. `ProcessService::release` (`process_service.rs:899-909`) does
`self.prepared.remove(index)` and only then calls `spawned.release()`, so a failure past that
point leaves nothing for `cancel` to find. `rt::process_release` (`lib.rs:2999-3005`) calls
`close(thread)` unconditionally, including when `SYS_THREAD_START` returned an error. And
PermissionManager collapses it: `if !matches!(process_client.release(&koid), Some(Ok(true)))` at
:849 and :997 turns a pre-start refusal, a post-removal failure and an unknown transport outcome
into one `None`.

Plan change: M2 now gives the SINGLE release the same typed outcomes the group release has, named
and separated - pre-start refusal, post-removal start failure (the release path owns synchronous
`forget` and Domain cleanup for the record it just orphaned), and uncertain transport/reply
(kill, wait for confirmed termination, reap). M4 requires a separate fault fixture for each of the
three, with the reason stated: they are different code paths and a fixture that drives only the
first proves nothing about the other two.

**4. `@rights` is unenforced on the stream-return dispatch branch - ACCEPTED.**

Confirmed. `codegen.rs:549` skips stream methods in the ordinary dispatch loop
(`if stream_return(&m.ret).is_some() { continue; }`), and the separate `<method>_open` emitted at
:687-702 calls `service.{mname}(...)` with no `handle_carries` guard and no denied arm. Also
confirmed as a live gap in the same code: when the return type is not a `Result` with a
denied-bearing error enum, the generator emits the bare `let result = service.…` call, so the
annotation reaches the ABI and no check.

No schema in the tree annotates a stream method today, so this is a hole that costs nothing to close
now and would cost a silent failure to close later. Of the audit's two options - reject, or teach
`_open` the same guard - the plan takes REJECT, which is consistent with the milestone's own
stated stance that rejecting an unenforceable promise is the smaller correction. Plan change: M4 (now
M5) rejects `@rights` on any stream-return method, and the refusal list is written out with the
missing-denied case beside it. "What this milestone refuses" gains the corresponding line.

**5. Alias acceptance is inconsistent with the metadata code generation has - ACCEPTED, with the
remedy narrowed.**

Confirmed. `resolve.rs:210` gives `Item::Alias` a `kernel_type` of `None` - only
`Item::Resource` carries one - and `codegen.rs:573` matches `Type::Handle(_)` directly while
the required type comes from `self.resource_types`. So an accepted alias would fall back to
`NO_REQUIRED_TYPE` and silently drop the object-type half of the guard while reading in the schema
exactly like a written handle. The old M4 wording ("aliases which do not resolve to that exact shape
are rejected") implied the resolving ones were fine, which is the ambiguity worth removing.

ACCEPTED as a defect; the audit's first option is taken and its second is REJECTED. M5 now defines
the accepted shape as a parameter WRITTEN as `handle<resource>`, and rejects named aliases outright
- local and imported - with the reason recorded. Propagating resource and kernel-type identity
through every alias chain is the recursive-authorization redesign the milestone already refuses;
doing it for aliases while refusing it for options, lists and records would be an inconsistent
contract for no gain, since no alias in the tree resolves to a handle today. Compile-fail fixtures
cover local and imported aliases either way. "What this milestone refuses" now names alias-chain
propagation explicitly so the decision is not re-litigated at implementation time.

**6. The scheduler prose audit excludes a nearby false safety statement - ACCEPTED.**

Confirmed, and it is the more dangerous of the two false statements in that file. The migration
comment at `sched/mod.rs:1014-1017` says an address space live on two cores "has no shootdown" and
calls that "the open item in Phase 2". `mem/tlb.rs` implements a synchronous flush of the whole
translation buffer on every other online core which WAITS for each acknowledgement before frames are
returned to the allocator, and its own header says the waiting is the correctness argument. A
maintainer reading the scheduler comment would design around a hazard that was closed - and the
paragraph is inside the block that documents what has been checked to survive migration, which is
exactly where someone goes looking.

Plan change: M5 (now M6) covers this paragraph, requires the current blunt global flush to be
described accurately, and names the open refinements as a per-address-space active-CPU mask and
per-page invalidation. It states that the item changes no TLB behavior, and "What this milestone
refuses" now says so twice over. Dependencies gain P02M0115, which owns the shootdown.

**Plan re-check.** The plan is now six items where it was five, with M3 new and the old M3 renumbered
to M4. Ordering is implementable: M1 is independent; M2 and M3 are one launch-lifecycle change and
M4 is its regression; M5 and M6 are independent of both. Each item names the file and the shape it
changes rather than a subject area. The Definition of done was rewritten so every clause is
falsifiable by a named fixture, including the exhaustiveness assertion and the three single-release
outcomes. No source code was modified.

AUDITOR'S RE-AUDIT OF PLAN M0169 (2026-08-30T22:25:50Z):

Rating: 6/10

The planner substantially corrected the prior findings, but three material gaps remain.

1. **A lost or malformed `launch_prepared` reply still has no recovery owner.** M2 starts its ownership rules only after `launch_prepared` succeeds (`docs/todo/P02M0169.md:78-93`). The generated client can transfer the bootstrap handle and then receive a transport failure or malformed reply (`src/user/libs/protocol/process-proto/src/generated/liber/process/v1.rs:872-905`; `src/user/libs/ipc/ipc-client/src/lib.rs:38-52`), after ProcessService has already retained the prepared record (`src/user/services/core/src/process_service.rs:878-892`). PermissionManager then has neither the koid nor a task handle with which to cancel it (`src/user/services/core/src/permission_manager.rs:801-806`), and dropping the temporary client does not close the shared service channel whose teardown drives ProcessService cleanup. Give prepare a caller-chosen transaction identity with idempotent status/cancel, or an equivalently bounded owner connection whose teardown is confirmed, and fault-test lost and malformed prepare replies.

2. **Empty and non-name `@rights` arguments disappear before M5 can reject them.** The parser accepts zero arguments and numeric arguments, but `collect_rights` retains only names (`src/tools/lsidl-gen/src/parser.rs:186-223`, `:579-589`). Validation iterates the resulting vector and generation skips it when empty (`src/tools/lsidl-gen/src/validate.rs:220-232`; `src/tools/lsidl-gen/src/codegen.rs:573-576`). Thus `@rights()` and `@rights(1)` become indistinguishable from no annotation and can generate an unguarded method, contrary to the Definition of done. Preserve annotation presence, reject empty/non-name/malformed rights lists, and add compile-fail fixtures.

3. **The generated denial path leaks a decoded transferred handle.** Dispatch decodes the handle, clears the request handle list, and then returns `Denied` without closing the decoded capability (`src/tools/lsidl-gen/src/codegen.rs:554-618`; a generated instance is `src/user/libs/protocol/display-proto/src/generated/liber/display/v1.rs:824-865`). `Handles` is non-owning and `clear` only erases handle numbers (`src/wire/src/lib.rs:359-375`, `:493-497`); the serve loop closes only handles still in that list (`src/user/runtime/rt/src/lib.rs:1987-2005`). M5 must close every decoded request capability not handed to the service on denial and prove it with a close/resource-baseline fixture, not merely assert that the metadata list is empty.

PLANNER'S RESPONSE ON M0169 (2026-08-30T23:21:20Z):

All three findings are accepted; all three were verified in the tree before being answered.

**1. A lost or malformed `launch_prepared` reply has no recovery owner - ACCEPTED.**

Verified, and the evidence is the transport's own comment. `ChannelTransport::call` returns four
distinct endings and says why it keeps them apart: "a deadline (the request may already have been
acted on)". The one call site that could act on that distinction throws it away -
`match process_client.launch_prepared(..) { Some(Ok(started)) => .., _ => { close(manager_side);
return None; } }` - so a request that WAS acted on but whose reply was lost leaves ProcessService
holding a prepared record while PermissionManager holds neither the koid nor a task handle. M2
started its ownership rules at "after `launch_prepared` succeeds", which is one step too late.

Plan changes: M2 gains a paragraph opening the window before the call returns, requiring a
CALLER-CHOSEN TRANSACTION IDENTITY sent with the request and keyed on by ProcessService, so
`status(id)` and `cancel(id)` are idempotent and can answer for a transaction whose reply never
arrived. The audit's alternative - a per-prepare owner connection whose teardown ProcessService
confirms - is kept as an explicitly acceptable substitute, because it delivers the same property. The
four transport outcomes are now handled distinctly by name, and M4 fault-tests a lost reply and a
malformed reply, each proving no prepared record survives.

**2. Empty and non-name `@rights` arguments disappear before M5 can reject them - ACCEPTED.**

Verified end to end: `annotations()` accepts a parenthesised list with no arguments, `ann_arg`
accepts `Tok::Num`, `collect_rights` keeps only `Arg::Name`, so `@rights()` and `@rights(1)` both
yield an EMPTY vector; validation then finds nothing to reject and generation skips the guard for an
empty list. A malformed annotation is therefore indistinguishable from no annotation and produces an
unguarded method - which is the outcome M5's own Definition of done forbids, reached through the one
path M5 did not consider.

Plan changes: M5 now requires the parser to preserve that a `@rights` annotation was WRITTEN,
separately from what it resolved to, and the validator to reject an empty list, a non-name argument
and a repeated right. The compile-fail fixture list gains `@rights()`, `@rights(1)` and a duplicated
right, and the Definition of done names the empty and non-name cases explicitly so the class is
closed rather than the two instances.

**3. The generated denial path leaks a decoded transferred handle - ACCEPTED, and it is the sharpest
of the three.**

Verified: dispatch decodes the handle, calls `request_handles.clear()`, and only then evaluates the
guard and returns `Denied`. `Handles` documents itself as NON-OWNING and is `Copy`; `clear` zeroes
the array; and the serve loop's cleanup closes only handles still present in that list - so after
`clear` there is nothing left for it to find. The capability the guard refused stays open in the
service's process. A refusal that keeps what it refused is worse than never checking, because the
caller is told the handle was rejected.

Plan changes: M5 requires generation to close every decoded request capability not handed to the
service on the denial path, and the gate is a RESOURCE-BASELINE fixture - repeated denied calls
return the process's handle count to its starting value. The audit's point about the weak assertion
is written in: proving the metadata list is empty proves nothing, because `clear` already made it
empty. The Definition of done carries the baseline requirement.

**Plan re-check.** No new items; three existing ones gained the contracts they were missing. Ordering
is unchanged. The Definition of done now states the recovery, the annotation-shape rejection and the
denial-path close as three separately falsifiable clauses. No source code was modified.
