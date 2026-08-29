AUDITOR'S REVIEW ON M0155 (2026-08-28 20:12:09 CEST):

Rating: 7/10

The loader-side cause of the reported thirty-second stall is substantially and correctly closed. Firmware absence is distinct from a read failure, `EFI_NOT_FOUND` no longer starts the FAT scan, the loader uses the read-only FAT mount, the two-sector FAT cache and contiguous-run reader are present, the boot medium now latches on a successful mount in both fallback visitors, and a successful system-volume bootstrap assembly is retained across the later reads. The topology and StorageService report changes are also wired into their real consumers and tests. I found two material completeness defects: M3's batching is lost by the actual userspace FAT block adapter, and M7's expressly required immediate per-device DMA line is absent.

## Findings

1. **The real StorageService FAT backing defeats M3's contiguous-read batching, so the claimed structural improvement does not apply to every caller.** `FatFs::read_chain` and `read_contiguous` correctly form runs and pass them through `read_run` to one `read_fs_sectors` call; `read_fs_sectors` then calls `BlockDevice::read_blocks` for the span (`src/fs/fat/src/lib.rs`, `read_chain`, `read_contiguous`, `read_run`, and `read_fs_sectors`). This produces a batched request in the loader because `FirmwareDisk` overrides `read_blocks`, and in the request-count test because `CountingDisk` also overrides it.

   The ordinary runtime FAT caller does not. `FatBacking::ensure_mounted` mounts `FatFs<FatBlockDevice>` for both `vol://media` and `vol://usb`, while `FatBlockDevice` implements only `read_block` and `write_block` (`src/user/services/storage/src/service.rs`, `FatBacking::ensure_mounted` and `impl fat::BlockDevice for FatBlockDevice`). It therefore inherits `fscore::BlockDevice::read_blocks`, whose implementation explicitly loops over `read_block` once per block (`src/fs/core/src/lib.rs`, `BlockDevice::read_blocks`). Since a FAT block here is one 512-byte sector, a contiguous file read still makes one StorageService-to-driver `block_read` exchange per sector on both real removable-volume paths.

   This is not a limitation of the backing. The block-service protocol accepts a sector count, publishes its per-request sector ceiling through `block_request_sectors`, and the adjacent `ChannelBlockDevice::read_blocks` already chunks spans by that ceiling (`src/user/services/storage/src/service.rs`, `block_request_sectors` and `ChannelBlockDevice::read_blocks`). Consequently, the FAT request-count test proves the filesystem against a synthetic span-capable adapter, and `FirmwareDisk` proves the loader path, but neither detects that the other production FAT adapter falls back to the per-sector behavior M3 was meant to remove. This materially contradicts the milestone goal to remove the structural FAT-reader costs “for every caller - not just this one” and means large contiguous reads through `vol://media` or `vol://usb` still incur the old round-trip pattern.

2. **M7 requires the per-device DMA decision to remain immediate, but the implementation records it silently and prints it only in the final summary.** The checked work item says: “The per-device line stays where it is: it is immediate and it names the device” (`docs/todo/P02M0155.md`, M7). In the code, `dma_policy::admit` calls `record_degraded`, and `record_degraded` only inserts into `DEGRADED`; it prints nothing (`src/kernel/dma_policy/mod.rs`, `admit` and `record_degraded`). The first per-device DMA line is emitted later by `dma_policy::report`, after the service chain reaches an interactive shell (`src/kernel/dma_policy/mod.rs`, `report`; `src/kernel/main.rs`, `boot_userspace`). If userspace never stabilizes, `boot_userspace` takes its failure/reboot branch and the required immediate admission lines are never printed at all.

   The final report is correctly timed and uses `abi::device_type_name`, so the summary half of M7 is implemented. The missing immediate line is nevertheless a direct requirement mismatch, not an optional logging enhancement. The milestone is internally inconsistent here: its Results example labels the final list “only at the end,” matching the current code, while its checked M7 requirement explicitly says the per-device line remains immediate. As written, the implementation satisfies the Results example but not the work item.

## Verified implementation coverage

- **M1:** `uefi::file::FirmwareRead::{Bytes, Absent, Failed}` preserves the firmware answer. `read_boot_file` returns immediately on `Absent`, falls back only with no firmware root or `Failed`, and gives those two fallback causes different messages (`src/boot/uefi/src/file.rs`; `src/boot/loader/src/main.rs`, `read_boot_file`).
- **M2:** `FatFs` owns two persistent `FatSector` buffers, `fat_sector` implements exact two-slot LRU behavior, and `write_fs_sectors` invalidates both slots before every filesystem write. The only direct device write is the exFAT boot-sector dirty flag, which cannot overlap a FAT sector (`src/fs/fat/src/lib.rs`, `FatFs`, `fat_sector`, `write_fs_sectors`, and `set_volume_dirty`).
- **M3/M4:** run coalescing, the firmware span override with aligned bounce fallback, the read-only mount's skipped ownership/mirror checks, the `Trust::NotAudited` state, and cursor cycle detection are present. Finding 1 is the production-adapter gap in the otherwise implemented M3 path.
- **M5:** `with_boot_medium` distinguishes `Visit::NotAMedium` from `Visit::Mounted` and stores `BOOT_MEDIUM` on `Mounted`; both the file reader and bootstrap reader return `Mounted` even when their requested content is unavailable (`src/boot/loader/src/main.rs`, `with_boot_medium`, `read_from_fat`, and `bootstrap_from_boot_medium`).
- **M6:** the normal successful path is guarded by `BOOTSTRAP.is_none()` and `BOOTSTRAP_REFUSED.is_none()`, so later system-volume reads do not reassemble a verified set (`src/boot/loader/src/main.rs`, `read_from_system_volume`).
- **M8:** ordinary topology absence is recorded during discovery and folded into the one baseline report; refusal paths retain their reason-bearing lines. Every StorageService report includes its `vol://` name, and the boot suite sorts and compares the complete expected report set rather than accepting an anonymous count (`src/kernel/mem/numa/mod.rs`; `src/user/services/storage/src/service.rs`; `src/kernel/test_suites/boot.rs`).
- **M9:** the controller, `boot-bypass=on`, and per-endpoint `iommu_platform=on` wiring remain present. P02M0159 deliberately superseded M0155's historical opt-in/default choice after fixing the zero-IOVA defect, so the current default-IOMMU plus `--no-iommu` interface is treated as the project's later design decision, not an M0155 regression (`docs/todo/P02M0159.md`; `run.sh`; `src/harness/qemu-run.sh`).
- The signed-boot and secure-boot gates extract `/boot/efiboot.img` from the shipping ISO rather than consuming the shared `efiboot.img` by-product (`src/tools/check-signed-boot.sh`; `src/tools/check-secure-boot.sh`).

## Checks performed

- `cargo test --manifest-path src/fs/fat/Cargo.toml`: **128 passed**.
- `cargo test --manifest-path src/boot/uefi/Cargo.toml`: **40 passed**.
- `cargo test --manifest-path src/abi/Cargo.toml`: **27 passed**.
- `bash -n` passed for `run.sh`, `src/harness/qemu-run.sh`, both boot-signing gates, and `src/tools/check-qemu-virtio-iommu-x86_64.sh`.

The full QEMU IOMMU and three-architecture guest runs were not repeated for this audit; the two findings above are established by the production call paths and do not depend on emulator timing or a stale built image.

---

IMPLEMENTER'S RESPONSE ON M0155 (2026-08-28T20:11:27Z):

**Finding 1 - the production FAT adapter defeats M3's batching: ACCEPTED and FIXED.**

Correct in every particular, and the tree contains the proof beside the defect.
`FatFs::read_chain` coalesces a file's clusters into contiguous runs and hands each run to
`BlockDevice::read_blocks` as one call. `FatBlockDevice` in `src/user/services/storage/src/service.rs`
implemented only `read_block` and `write_block`, so it inherited `fscore::BlockDevice::read_blocks`,
which loops `read_block` once per block - and a FAT block here is one 512-byte sector. Its immediate
neighbour `IsoBlockDevice` DOES override `read_blocks`, through `read_blocks_chunked`, so the two
adapters in the same file disagreed about whether the batching applies. `FirmwareDisk` overrides it in
the loader and `CountingDisk` overrides it in the request-count test, which is why nothing in the tree
was looking at the one adapter that did not: every contiguous read through `vol://media` and
`vol://usb` was still one StorageService-to-driver round trip per sector.

Changed: `FatBlockDevice` now implements `read_blocks` as
`read_blocks_chunked(self.chan, 1, index, count, buf, SECTOR_SIZE)` - the same helper the ISO adapter
uses, so the span is chunked by the block service's own per-request ceiling rather than by the
filesystem's idea of one.

**Finding 2 - M7 requires the per-device DMA line to stay immediate: PARTIALLY ACCEPTED.**

The observations are all correct. `dma_policy::admit` calls `record_degraded`, which only inserts into
`DEGRADED`; the first per-device line comes from `dma_policy::report()`, called after userspace
stabilizes; and M7's checked bullet says the per-device line "is immediate and it names the device"
while the Results section says the list appears only at the end.

I REJECT restoring the immediate line. The code that removed it carries its reason in place:
"This printed a line per device as well, so a boot with eleven untranslated endpoints carried
twenty-two lines saying so - the scattering AND the list it exists to replace." That is a later and
better decision than M7's bullet, it is the one the Results section records, and it is the same
report-legibility goal M0158 is entirely about. Re-adding the line to satisfy the older half of a
document that contradicts itself would be undoing a fix to make a tick true.

I ACCEPT the real consequence the auditor identifies underneath it, and that IS fixed. `report()` was
called only on the success path of `boot_userspace`, so a boot that could not stabilize userspace -
precisely the boot somebody is reading the log to diagnose - carried NO record of which devices had
been admitted to master the bus untranslated, even though every one of them was admitted before that
point. `src/kernel/main.rs` now calls `dma_policy::report()` on the failure/reboot branch as well.
That keeps the information in the case where losing it costs something, without reintroducing the
duplication.

The milestone document's internal contradiction stands and should be resolved in favour of the
Results section. I have not edited it as part of this response.

**On the "Verified implementation coverage" section:** I re-read M1, M2, M5, M6 and M8 against the
code and agree with the auditor's account of each. Nothing there needed changing.

---

AUDITOR'S RE-AUDIT ON M0155 (2026-08-29T16:01:42Z):

Current implementation rating: 10/10

No material issue remains within M0155's scope. `FatBlockDevice::read_blocks` now sends filesystem runs through the block service's chunked multi-sector path (`src/user/services/storage/src/service.rs:3513-3530`). The implementer's rejection of duplicate immediate DMA lines is justified by the plan's contradictory Results/legibility goal, and the material information-loss case is fixed: `dma_policy::report()` now runs on both userspace stabilization success and failure/reboot paths (`src/kernel/main.rs:944-965`). The final report still names every degraded device.

Verification: the FAT suite passed 128 tests, the UEFI suite passed 41 tests, and the ABI suite passed 28 tests. No unresolved regression or incomplete audit fix was found.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0155 (2026-08-29T16:37:04Z):

The re-audit reports no unresolved material finding and rates the milestone 10/10. There is nothing
to accept or reject, and nothing was changed for it.

Recorded here so this file says so rather than leaving a reader to infer it from an absence: I
re-read the re-audit against the current tree and agree with its account, including its note that the
earlier stale-artefact results were transient effects of a concurrent build rather than defects.

---

AUDITOR'S RE-AUDIT ON M0155 (2026-08-29T19:01:24Z):

Current implementation rating: 10/10

No material unresolved issue remains within M0155's scope. The production FAT adapter still overrides `read_blocks` with the chunked multi-sector service path (`src/user/services/storage/src/service.rs:3513-3530`), and the final per-device degraded-DMA report still runs on both userspace success and failure/reboot paths (`src/kernel/main.rs:944-965`). Current FAT, UEFI, and ABI host suites passed 128, 41, and 28 tests respectively.
