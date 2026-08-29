# LiberSystem Durable History

This file keeps durable decisions and lessons. Detailed chronology belongs in Git history.

## Architecture Decisions
- The system is a capability microkernel with isolated processes.
- Each process owns a handle table. Rights transfer is explicit.
- Services communicate through typed channel IPC.
- The service manifest owns staging and artifact ownership.
- Revocation increments an object generation. It does not scan every handle table.
- Resource Domains account bounded kernel resources and support deterministic cleanup.
- LiberFS uses copy-on-write metadata and delayed reclaim.
- StorageService owns volume authority.
- Executables are PIE `ET_DYN` files.
- Shared `.lslib` providers have manifest dependencies and identity notes.
- Immutable executable/provider pages may be shared.
- Writable process state remains private.
- The same syscall and package contracts are used on x86_64, AArch64 and RISC-V.
- x86_64 boots through UEFI with separate package modules.
- Direct AArch64/RISC-V boots embed packages.

## Development Decisions
- Generated manifests and reports are checkpoint artifacts.
- The hand-edited service manifest remains authoritative.
- Fast local builds may be targeted.
- Full three-target builds and audits remain the release checkpoint.
- Cache keys include source, target, toolchain, flags and manifest metadata.
- They also include provider identities.
- Failed builds publish nothing. Candidates are audited before atomic replacement.
- Package and image assembly is content-aware: unchanged bytes preserve inode and mtime.
- Application tests should move out of the kernel image.
- Kernel mechanism tests remain there.

## Validation Lessons
- Standalone kernel `cargo check` bypasses required target and build-std configuration.
- Its failures can be misleading.
- Cache-hit cost matters as much as compile cost.
- Hashing and ELF inspection must scale with the changed closure.
- Restore temporary source mutations explicitly.
- Persistent terminal `EXIT` traps are unsafe.
- Validate architecture claims per target.
- Package delivery and boot topology differ by architecture.
- Hardware tests, long integration runs and UI judgment remain expensive.

## Permanent References
- `docs/PERF.md`: measurements and performance methodology.
- `docs/THREAT_MODEL.md`: security assumptions and capability boundaries.
- `docs/LIBERFS.md`: filesystem format and correctness.
- `docs/LSIDL.md`: IPC schema and generated bindings.
- `docs/DYNAMIC_LINKING.md`: shared-image and loader rules.
- Git history and `docs/todo/`: implementation chronology and validation results.
