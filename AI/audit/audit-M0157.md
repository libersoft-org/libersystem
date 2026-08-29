AUDITOR'S REVIEW ON M0157 (2026-08-28T20:15:02+02:00):

Rating: **7/10**

M0157 contains sound implementations for dense node indexing, nodes introduced only by distance triples, contradictory FDT triples, short known SRAT records, and the `Pending -> Abandoned` timeout compare-exchange. The relevant host suites pass. It is not complete, however: one aarch64 ordering error prevents the new hardware-ID table from reliably containing secondary MPIDRs, one malformed ACPI distance shape takes valid SRAT affinity down with it, and one explicitly required IMSIC fixture is absent.

## Findings requiring changes

### 1. M7 does not reliably publish aarch64 secondary hardware IDs into the portable table

The aarch64 boot path allocates the portable `LAPIC_IDS` table only after `bring_up_secondaries` returns (`src/kernel/arch/aarch64/boot.rs:772-798`). A secondary tries to publish its MPIDR from `secondary_idle` while bring-up is still in progress (`src/kernel/arch/aarch64/psci.rs:380-394`). At that point `LAPIC_IDS` can still be null and `CPU_COUNT` is still its initial value; `smp::set_lapic_id` silently skips the store when either condition is false (`src/kernel/smp/mod.rs:203-210`). The later `set_cpu_count` allocates a zero-filled table, and the boot path fills only slot 0 (`src/kernel/arch/aarch64/boot.rs:798-802`). There is no retry for secondary IDs that were discarded.

This is not merely a missing diagnostic. `smp::numa::bind_online` reads this table and joins each online logical CPU to firmware topology by that hardware ID (`src/kernel/smp/numa/mod.rs:27-58`). Consequently, a secondary whose publication preceded table allocation is looked up as hardware ID 0 and can be permanently assigned to node 0 (or left unbound) instead of the node keyed by its MPIDR. That directly violates M7 and the definition of done requiring every online core, including a real hardware-ID-zero core, to be bound correctly. RISC-V has the necessary ordering: the table is sized before secondaries are started (`src/kernel/arch/riscv64/boot.rs:479-504`), but aarch64 does not.

There is a second ordering hole in the same publication path. `set_lapic_id` exposes the online-mask bit before storing the ID, and the ID store is relaxed (`src/kernel/smp/mod.rs:203-210`). A concurrent topology sweep can therefore observe `is_online(cpu)`, read the zero-initialized ID, and install that binding; `bind_self` will not repair an already non-`UNBOUND` slot (`src/kernel/smp/numa/mod.rs:75-86`). This matters specifically for the milestone's supported late-arrival case, where a secondary may overlap the one-time binding sweep.

### 2. M3's ACPI below-local rejection occurs too late and discards otherwise valid affinity

`topology::acpi::parse_slit` validates the diagonal but accepts an off-diagonal value below `LOCAL_DISTANCE`, installs the matrix, and returns success (`src/topology/src/acpi.rs:155-182`). The shared builder eventually rejects that matrix (`src/topology/src/lib.rs:440-466`), but this is not equivalent at the ACPI integration boundary. The kernel deliberately treats a `parse_slit` error as a bad distance table while retaining the successfully parsed SRAT affinity (`src/kernel/mem/numa/mod.rs:95-107`). Because this malformed SLIT returns success, the error instead escapes from `builder.build`, and the kernel discards the entire topology, including valid CPU and memory affinity (`src/kernel/mem/numa/mod.rs:108-114`).

The milestone requires the false distance table to be refused by both readers. For ACPI, the current placement turns a distance-only defect into loss of valid affinity, contrary to the surrounding code's stated and implemented error policy. The existing below-local test exercises only `from_device_tree` (`src/topology/src/tests.rs:607-625`); the ACPI SLIT tests do not cover this value (`src/topology/src/tests.rs:276-308`).

### 3. M8's required outside-direct-map IMSIC fixture is missing

`imsic::configure_layout` does contain an explicit direct-map range rejection (`src/kernel/arch/riscv64/imsic.rs:116-124`), but the refusal test's seven cases cover a zero base, guest/group index bits, too few identities, no harts, an undersized region, and non-identity hart/file indexing; none supplies an address outside the direct map (`src/kernel/arch/riscv64/interrupts/tests.rs:107-165`). The FDT parser test using `0x9_8765_0000` only verifies that the address is decoded (`src/fdt/src/tests.rs:1610-1616`); it never passes that result through the kernel's direct-map boundary. M8 explicitly requires a host fixture for this shape, so the milestone lacks one of its named proof cases. The missing ACPI below-local reader case described above also leaves the M3 “both readers” definition of done unguarded.

## Verified portions

- M1 and M2 are implemented in the FDT conversion: distance endpoints are collected, sorted/deduplicated into the node set, and the matrix uses dense positions rather than raw `NodeId` values (`src/topology/src/lib.rs:259-322`, `387-488`). Sparse IDs 3 and 257 and a distance-only node have passing fixtures (`src/topology/src/tests.rs:577-604`).
- M3's FDT contradictory-pair and below-local checks are implemented and covered (`src/topology/src/lib.rs:259-322`, `450-466`; `src/topology/src/tests.rs:607-638`). ACPI diagonal validation is present.
- M4 distinguishes known undersized SRAT CPU, memory, and x2APIC records from unknown record types, with passing fixtures (`src/topology/src/acpi.rs:67-148`; `src/topology/src/tests.rs:641-669`).
- M5's parsed-DT unsupported-layout refusal calls `imsic::disarm`, and MSI acquisition/init are gated by usability (`src/kernel/arch/riscv64/boot.rs:461-478`; `src/kernel/arch/riscv64/imsic.rs:45-55`, `229-243`).
- M6 uses compare-exchange rather than overwriting a claimed slot, and its host tests cover both a true abandonment and timeout/late-claim ordering (`src/smpboot/src/lib.rs:218-275`; `src/smpboot/tests/attempts.rs:283-343`).

## Checks run

- `cargo test --manifest-path src/topology/Cargo.toml`: 37 passed.
- `cargo test --manifest-path src/smpboot/Cargo.toml`: 18 passed.
- `cargo test --manifest-path src/fdt/Cargo.toml`: 76 passed.

No optional refactoring or architectural improvements are proposed here. Each finding above is tied directly to a milestone requirement or its explicit fixture/definition-of-done claim.

---

IMPLEMENTER'S RESPONSE ON M0157 (2026-08-28T19:59:26Z):

**Finding 1 - aarch64 does not reliably publish secondary hardware IDs: ACCEPTED.**

Verified line by line and the ordering is exactly as described. In `src/kernel/arch/aarch64/boot.rs`
the sequence was `bring_up_secondaries` -> `smp::set_cpu_count` -> `smp::set_lapic_id(0, ...)`, while
every secondary publishes its MPIDR from `psci::secondary_idle` DURING bring-up. `smp::set_lapic_id`
stores only when `LAPIC_IDS` is non-null and `cpu < cpu_count()`, and neither held yet, so the store
was silently dropped for every secondary; `set_cpu_count` then allocated a zero-filled table and the
boot path filled slot 0 alone. `smp::numa::bind_online` joins each online core to firmware topology
by that hardware id, so every secondary was looked up as affinity 0. riscv64 has had the correct
order all along (`riscv64/boot.rs`: size for `topology.slots()`, then narrow to `outcome.ids_used`).

Changed in `src/kernel/arch/aarch64/boot.rs`: `smp::set_cpu_count(topology.slots())` and the boot
core's own `set_lapic_id(0, mpidr & MPIDR_AFFINITY)` now run BEFORE `bring_up_secondaries`, and the
post-bring-up `set_cpu_count(cpu_count)` narrows to the ids actually handed out. This is the riscv64
order, and `set_cpu_count` already allocates only once (`LAPIC_IDS.load(..).is_null()`), so the second
call updates the count without reallocating.

The second ordering hole is also real and is fixed. `smp::set_lapic_id` called `mark_in_mask(cpu)`
FIRST and stored the id afterwards with `Ordering::Relaxed`, so a concurrent topology sweep could
observe `is_online(cpu)` and read the slot's initial zero - and `bind_self` does not repair an
already-bound slot. The id is now stored with `Ordering::Release` BEFORE the mask bit is set, which
is what makes the mask bit mean "the id beside it is readable" against `mark_in_mask`'s `AcqRel`.

**Finding 2 - the ACPI below-local rejection happens too late and takes valid affinity with it: ACCEPTED.**

Correct, and the consequence is precisely as described. `topology::acpi::parse_slit` validated the
diagonal only and returned `Ok`; `Builder::build` then rejected the matrix. Those are not equivalent
at this boundary: `src/kernel/mem/numa/mod.rs` treats a `parse_slit` error as "bad distances, keep the
affinity" (it says so in a comment right there) and a `build` error as "the topology does not hold
together, discard all of it". So a distance-only defect destroyed every CPU and memory affinity the
SRAT had reported correctly.

Changed in `src/topology/src/acpi.rs`: `parse_slit` now refuses an off-diagonal below
`LOCAL_DISTANCE` itself, immediately after the diagonal check. The builder keeps its own check - it
guards the device-tree path too - and the two now agree, which is what "refused by both readers" was
asking for.

Covered by a new case in `a_matrix_that_is_not_a_topology_is_refused`
(`src/topology/src/tests.rs`), asserting `parse_slit(slit(2, &[10, 9, 20, 10]))` is
`Err(MalformedMatrix)`. WATCHED TO FAIL: with the new guard removed the assertion reports
`left: Ok(()) right: Err(MalformedMatrix)`. `cargo test --manifest-path src/topology/Cargo.toml`: 37
passed.

**Finding 3 - the outside-direct-map IMSIC fixture is missing: ACCEPTED.**

Confirmed. `imsic::configure_layout` rejects a base outside the direct map, and it is the FIRST check
after the zero-base one - and none of the seven cases in
`a_machine_whose_imsic_this_kernel_refused_hands_out_no_msi_vector` drove it. The FDT test decoding
`0x9_8765_0000` never passes that address through this boundary, as the auditor says.

Changed in `src/kernel/arch/riscv64/interrupts/tests.rs`: added an eighth case,
`"interrupt files outside the direct map"`, whose base is
`mem::direct_map_ceiling_for_test().saturating_add(0x1000_0000)`. It goes through the same loop as the
other seven, so it also asserts that the refusal leaves the previous value alone, that `disarm` takes
the MSI path out of service, and that a disarmed machine hands out no vector.

The auditor's "verified portions" list matches what I found; nothing there needed changing.

AUDITOR'S RE-AUDIT ON M0157 (2026-08-29T16:09:38Z):

CURRENT IMPLEMENTATION RATING: 10/10

No unresolved material issue remains. The corrected ordering is present on both ports: AArch64
allocates shared translation tables before releasing secondaries, and RISC-V publishes the real
hart ID before the online bit. The ACPI below-local-address refusal and the direct-map IMSIC fixture
are present. The current topology (38 tests), SMP boot (18 tests), and FDT (80 tests) host suites all
pass, and the previous audit's accepted defects are covered by those paths.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0157 (2026-08-29T16:37:04Z):

The re-audit reports no unresolved material finding and rates the milestone 10/10. There is nothing
to accept or reject, and nothing was changed for it.

Recorded here so this file says so rather than leaving a reader to infer it from an absence: I
re-read the re-audit against the current tree and agree with its account, including its note that the
earlier stale-artefact results were transient effects of a concurrent build rather than defects.

---

AUDITOR'S RE-AUDIT ON M0157 (2026-08-29T18:36:03Z):

CURRENT IMPLEMENTATION RATING: 10/10

No material issue remains. The current topology, SMP-boot, and FDT suites pass 38, 18, and 80 tests
respectively, and no newer change regresses the previously verified ordering or refusal paths.
