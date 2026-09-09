# Useful prompts

## ALREADY IMPLEMENTED AUDIT:

### PLANNER - PLAN CREATION FROM EXISTING IMPLEMENTATION:

Thoroughly review the existing implementation in:

- src/loader/
- src/boot/
- src/bootproto/
- src/buddy/
- src/proto/
- src/fdt/
- src/uefi/
- src/user/drivers/
- src/kernel/
- src/abi/
- src/idl/
- src/fs/fat/
- src/fs/liberfs/
- src/fs/libermemfs/
- src/fs/iso9660/
- src/fs/udf/
- src/fs/partition/
- src/sdk/
- src/wasm/
- src/wire/
- src/term/

and create a new corrective implementation plan for a new milestone in `/docs/todo/PXXMYYYY.md`. Verify the complete relevant code, current project architecture, surrounding components, dependencies, conventions, failure paths, tests, and integration points. Identify only real and material defects, incomplete behavior, inconsistencies, missing integration, or insufficient verification that should genuinely be fixed. Do not report stylistic, theoretical, speculative, or extremely unlikely issues, and do not propose unnecessary refactoring, redesign, abstractions, optimizations, defensive mechanisms, or new features. Check related milestone files and existing plans so that the new plan does not duplicate work that is already implemented, planned, or deliberately outside this scope. Write a complete, correct, feasible, internally consistent, and properly scoped implementation plan with concrete tasks, affected components, required behavior, dependencies, and objective completion criteria. Do not leave important design decisions to the implementer. Do not modify any source code, tests, existing milestone file or audit files. Create only a new milestone file `/docs/todo/PXXMYYYY.md` and add it to `/docs/todo/TODO.md`.

### PLANNER - PLAN CREATION FROM REQUIREMENT:

Requirement:

`WRITE THE REQUIREMENT HERE`

Create a complete implementation plan for a new milestone in `/docs/todo/PXXMYYYY.md` that fulfills the requirement above. Critically verify the requirement against the current project architecture, relevant existing code, conventions, dependencies, supported configurations, and integration points. Determine what is already implemented, what can be reused, and what must actually change. Keep the plan strictly within the requested scope and do not add unrelated features, refactoring, redesign, abstractions, optimizations, future-proofing, or unnecessary defensive mechanisms. Resolve implementation details from the existing architecture wherever possible and clearly define all material behavior, ownership, failure handling, compatibility requirements, dependencies, and verification criteria instead of leaving important decisions to the implementer. Check related milestone files and existing plans so that the new plan does not duplicate or contradict existing work. Write a complete, correct, feasible, internally consistent, sufficiently detailed, and objectively verifiable implementation plan. Do not implement or modify any source code, tests, existing milestone files or audit files. Create only a new milestone file `/docs/todo/PXXMYYYY.md` and add it to `/docs/todo/TODO.md`.

### AUDITOR:

Check implementations of the following milestones:

- P02M0150
- P02M0151

... and write an audit for each milestone to `/AI/audit/code-audit-PXXMYYYY.md`. Critically verify the actual implementation against the milestone requirements and the relevant surrounding code. Focus strictly on the scope of each milestone and evaluate whether the implementation is complete, correct, internally consistent, and actually fulfills what the milestone requires. Do not do any overengineering and do not suggest architectural changes, refactoring, additional abstractions, defensive mechanisms, optimizations, or extra features unless they are genuinely necessary to satisfy the milestone itself. Do not report purely theoretical, stylistic, speculative, or extremely unlikely issues that do not materially affect the correctness or completeness of the milestone. Verify findings against the actual code before reporting them and avoid assumptions based only on filenames, comments, TODOs, naming, or incomplete context. Take existing project architecture and design decisions as given unless they directly prevent the milestone from being implemented correctly. Check relevant interactions with surrounding components where necessary to determine whether the milestone works as intended, but do not expand the audit into unrelated parts of the project. For every real issue you find, clearly explain what is wrong, why it matters for the milestone, and reference the relevant files, functions, structures, or code paths. Distinguish actual implementation defects from optional improvements. Give each milestone a rating from 0 to 10/10 for completeness and correctness, where 10/10 means that you found no meaningful issue that should require a code change within the scope of that milestone. Start every audit with the title `AUDITOR'S REVIEW ON PXXMYYYY ([TIMESTAMP IN UTC FORMAT]):`. Do not modify any source code. Your role is strictly to audit the implementation and report findings.

### IMPLEMENTER:

Check all audits in `/AI/audit/code-audit-PXXMYYYY.md` for the following milestones:

- P02M0150
- P02M0151

Critically verify each statement and finding against the actual implementation. Do not assume the auditor is always correct. Reject findings that are incorrect, irrelevant, speculative, outside the milestone scope, or would introduce unnecessary overengineering. Keep all changes strictly within the scope of the respective milestone unless a minimal adjacent change is technically necessary for the fix. Do not refactor unrelated code, redesign working components, add unnecessary abstractions, or implement features that the milestone does not require. After making the necessary changes, re-check the implementation against the milestone requirements and verify that your changes did not introduce regressions. At the end of each `/AI/audit/code-audit-PXXMYYYY.md` file, append your response under this title: "IMPLEMENTER'S RESPONSE ON PXXMYYYY ([TIMESTAMP IN UTC FORMAT]):". Address every auditor's finding individually. State whether you ACCEPTED or REJECTED the finding. If rejected, briefly explain why. For accepted findings, describe exactly what was changed and reference the relevant files/functions. If you need to run long tests, do it at the very end of this whole job, so we don't waste time. Do not modify or delete the original audit, only append your response below it. If you necessarily need to run long tests, do it only at the very end of this, it takes very long time.

### AUDITOR (RE-AUDIT):

Re-audit implementations of the following milestones:

- P02M0150
- P02M0151

Read the original audit and the implementer's response in each `/AI/audit/code-audit-PXXMYYYY.md`, then verify all claims, fixes, and rejected findings against the current code and milestone requirements. Do not assume that either the auditor or implementer was always correct. Report only unresolved issues, incorrect or incomplete fixes, unjustified rejections, regressions, or newly found defects that materially affect the milestone. Do not repeat correctly resolved findings, do not do any overengineering, or suggest changes outside the milestone scope. Append the result to the end of the corresponding audit file under the title `AUDITOR'S RE-AUDIT ON PXXMYYYY ([TIMESTAMP IN UTC FORMAT]):`. Give the current implementation a rating from 0 - 10/10. Do not modify any source code and do not modify or delete the original audit, only append your re-audit below it.

### IMPLEMENTER (RE-IMPLEMENTATION):

Check all audits in `/AI/audit/code-audit-PXXMYYYY.md` for the following milestones:

- P02M0150
- P02M0151

Critically verify every audit and re-audit finding against the current implementation and milestone requirements. Do not assume that either the auditor or implementer was always correct. Reject findings that are incorrect, already resolved, irrelevant, speculative, outside the milestone scope, or would introduce unnecessary overengineering. Fix all valid findings while keeping changes strictly within the milestone scope and avoiding unrelated refactoring or redesign. After the changes, verify that the milestone is complete and that no regressions were introduced. Append your response to the end of each audit file under the title `IMPLEMENTER'S RESPONSE TO RE-AUDIT ON PXXMYYYY ([TIMESTAMP IN UTC FORMAT]):`. Address every finding individually, state whether it was ACCEPTED or REJECTED, briefly explain rejected findings, and describe the code changes for accepted findings. Run long tests only at the end of the whole job. Do not modify or delete the original audit, only append your response below it. If you necessarily need to run long tests, do it only at the very end of this, it takes very long time.


## NOT-IMPLEMENTED MILESTONE AUDIT:

### AUDITOR:

Audit the implementation plan for the milestones:

- P02M0150
- P02M0151

in `/docs/todo/PXXMYYYY.md` and create the audit to `/AI/audit/plan-audit-PXXMYYYY.md`. Critically verify the plan against the milestone requirements, current project architecture, relevant existing code, conventions, dependencies, and integration points. Evaluate whether the plan is complete, correct, feasible, internally consistent, properly scoped, and sufficiently detailed for implementation. Do not assume the planner is always correct. Report only material problems that could lead to an incomplete, incorrect, incompatible, or unnecessarily overengineered implementation. For every finding, explain what is wrong, why it matters, and what part of the plan needs to be corrected. Give the plan a rating from 0 - 10/10, where 10/10 means that no meaningful change is required before implementation. Start the the audit with the title `AUDITOR'S REVIEW OF PLAN PXXMYYYY ([TIMESTAMP IN UTC FORMAT]):`. Do not modify the plan or any source code.

### PLANNER - PLAN REVISION:

Read the latest audit of the implementation plan of the following milestones:

- P02M0150
- P02M0151

in `/AI/audit/plan-audit-PXXMYYYY.md` and critically verify every finding against the milestone requirements, current project architecture, relevant code, and the current plan in `/docs/todo/PXXMYYYY.md`. Do not assume the auditor is always correct. Reject findings that are incorrect, irrelevant, speculative, outside the milestone scope, or would introduce unnecessary overengineering. For every valid finding, update `/docs/todo/PXXMYYYY.md` so that it contains the complete and current corrected implementation plan. After the changes, re-check that the plan is complete, correct, feasible, internally consistent, and ready for implementation. Do not implement or modify any source code. Append your response to `/AI/audit/plan-audit-PXXMYYYY.md` under the title `PLANNER'S RESPONSE ON PXXMYYYY ([TIMESTAMP IN UTC FORMAT]):`. Address every finding from the latest audit individually, state whether it was ACCEPTED or REJECTED, briefly explain rejected findings, and describe the exact plan changes made for accepted findings. Do not modify or delete any existing audit content.

### AUDITOR - PLAN RE-AUDIT:

Re-audit the updated implementation plan for the following milestones:

- P02M0150
- P02M0151

in `/docs/todo/PXXMYYYY.md`. Read the complete history in `/AI/audit/plan-audit-PXXMYYYY.md` and verify the planner's responses. Do not assume that either the planner or previous auditor was always correct. Report only unresolved issues, incomplete or incorrect corrections, unjustified rejections, contradictions, or newly discovered material defects. Do not repeat findings that were correctly resolved, do not do any overengineering, or suggest changes outside the milestone scope. Append the result to `/AI/audit/plan-audit-PXXMYYYY.md` under the title `AUDITOR'S RE-AUDIT OF PLAN PXXMYYYY ([TIMESTAMP IN UTC FORMAT]):`. Give the current plan a new rating from 0 - 10/10. Do not modify the plan or any source code, and do not modify or delete existing audit content, only append it.

## FIRST IMPLEMENTATION:

Implement the following milestones:

- P02M0150
- P02M0151

Read each complete plan in `/docs/todo/PXXMYYYY.md`. Review the relevant existing code, architecture, conventions, dependencies, and integration points before making changes.

Implement all requirements of each plan, including the specified integration, failure handling, compatibility, and tests. Reuse existing functionality where appropriate and follow the project's established patterns. Keep changes strictly within the milestone scope unless a minimal adjacent change is technically necessary. Do not add unrelated features, refactoring, redesign, abstractions, optimizations, or unnecessary defensive mechanisms. Do not leave required behavior as stubs, placeholders, or TODOs.

Treat the plan as the implementation specification, but verify its assumptions against the actual code. If a material contradiction or missing design decision prevents correct implementation, explicitly report the blocker rather than inventing requirements or silently changing the plan. Do not weaken requirements or completion criteria to match the implementation.

For each milestone, create `/AI/audit/code-audit-PXXMYYYY.md` when you begin implementing it and start an implementation record under the title `IMPLEMENTER'S INITIAL IMPLEMENTATION ON PXXMYYYY ([TIMESTAMP IN UTC FORMAT]):`. Update your record as implementation progresses, documenting what was actually implemented, relevant files and functions, material implementation decisions, verification results, and any remaining blockers or incomplete work. Record actual work and results, not merely intended changes. These notes will serve as context for the subsequent independent code audit; do not assign an audit rating. If the file already exists, preserve its existing content and append your own section.

After implementation, verify every completion criterion and check relevant interactions for regressions. Run targeted checks as needed during implementation, but run long tests only at the end of the whole job and only if it's totally necessary as it takes a lot of time. Clearly distinguish passed, failed, and unperformed verification, including the commands used and any limitations. Do not claim unrun tests passed.

Update implementation status in `/docs/todo/PXXMYYYY.md` and `/docs/todo/TODO.md` according to existing project conventions, marking completion only when the requirements and required verification are satisfied. Preserve the plan's requirements and do not modify any existing audit content.

Finish by ensuring that each implementation record reflects the final state of the work, and provide a brief summary of completed milestones, verification results, and any remaining blockers or incomplete work.
