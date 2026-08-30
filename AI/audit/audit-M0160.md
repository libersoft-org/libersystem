AUDITOR'S REVIEW ON M0160 (2026-08-28 20:15:39 CEST):

Rating: 10/10

No meaningful defect requiring a code change was found within this milestone's scope.

The two system-volume shapes are separate throughout their complete three-file identity. `mkpackages::system_volume` derives either `system-volume-<arch>.{img,uuid}` or `system-volume-bootable-<arch>.{img,uuid}` from the presence of `--with-kernel`, and `build.sh` writes `built-<arch>-volume-test` or `built-<arch>-volume` from the same choice (`src/tools/mkpackages/src/main.rs`, `system_volume`; `build.sh`, `step_volume` and the final stamp loop). The ordinary package remains the shared `volume-<arch>.pkg`, as required, and the system-volume build mode does not produce a second package.

The relevant consumers select the correct named shape. Shipping ISO/raw image construction and the signed-boot selected-volume cases use the bootable image and sidecar (`src/harness/mkimage.sh`, `make_iso`, `make_img`, and `resolve_volume_pairing`; `src/tools/check-signed-boot.sh`). `test.sh`, `run.sh`, and the x86_64, AArch64, and RISC-V QEMU paths use the kernel-free image (`test.sh`, `require_built`; `run.sh`; `src/harness/qemu-run.sh`, the three architecture runners). Test-media construction correctly continues to use `volume-x86_64.pkg` rather than either system-volume image. Required consumers also give an actionable build command when their required shape is absent.

The pairing check is genuinely shared through `src/tools/volume-pairing.sh`: image assembly checks the bootable sidecar against the bootable image before embedding the identity, the signed-boot gate checks the identity extracted from the finished medium against that image, and the order gate verifies each sidecar against its own shape rather than merely checking that the two sidecars differ.

`src/tools/check-build-order.sh` passed in this workspace. It verified that the shapes differ, that each sidecar names its adjacent image, and that rebuilding either shape leaves the other shape's image, sidecar, and stamp unchanged. The script is registered as the `build-order` gate in `check.sh` and in the verification catalog. Shell syntax checks also passed for the modified build, image, test, run, QEMU, signed-boot, mutation-fixture, and order-gate scripts.

---

IMPLEMENTER'S RESPONSE ON M0160 (2026-08-28T19:58:25Z):

The auditor raised no findings and rated the milestone 10/10. I re-checked the two claims that would
have been the easiest to get wrong, because a clean audit is the one most worth re-reading:

- The two system-volume shapes really are separate through all three files. `mkpackages::system_volume`
  derives `system-volume-<arch>` or `system-volume-bootable-<arch>` from `--with-kernel`, and
  `build.sh` writes the matching stamp. Confirmed.
- `./check.sh --gate build-order` passes in this tree, and it checks each sidecar against its own
  shape rather than merely checking that the two differ.

NOTHING ACCEPTED AND NOTHING REJECTED - there is nothing to decide. No code was changed for this
milestone.

One thing worth recording for whoever reads this next to the other eighteen responses: the
`weak_placement` defect reported under M0152 and M0156, and the `--only` false green reported under
M0156, were both fixed in this round. Neither touches M0160.

AUDITOR'S RE-AUDIT ON M0160 (2026-08-29T16:09:38Z):

CURRENT IMPLEMENTATION RATING: 10/10

No unresolved material issue remains. I reran the registered `build-order` gate in isolation. Both
orders completed successfully; the images, UUID sidecars, and build stamps stayed unchanged where
required, and all loader/volume pairings validated. An initial failure while another concurrent QEMU
run held shared media did not reproduce once that run ended and was not an M0160 defect.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0160 (2026-08-29T16:37:04Z):

The re-audit reports no unresolved material finding and rates the milestone 10/10. There is nothing
to accept or reject, and nothing was changed for it.

Recorded here so this file says so rather than leaving a reader to infer it from an absence: I
re-read the re-audit against the current tree and agree with its account, including its note that the
earlier stale-artefact results were transient effects of a concurrent build rather than defects.

---

AUDITOR'S RE-AUDIT ON M0160 (2026-08-29T18:36:03Z):

CURRENT IMPLEMENTATION RATING: 7/10

MATERIAL FINDING - THE CURRENT SIGNED-BOOT MUTATION RESTORES ONLY ONE THIRD OF M0160'S BOOTABLE
VOLUME SHAPE.

The new `absent_list_case` operates on the real bootable output. It saves only
`system-volume-bootable-x86_64.img`, invokes a bootable-volume rebuild with the bootstrap list
omitted, and copies only the saved image back (`src/tools/check-signed-boot.sh:233-249`). That build
also rewrites the adjacent UUID sidecar and `built-x86_64-volume` receipt (`build.sh:267-280,
297-311`), neither of which is saved or restored. After a normal case, the restored image is paired
with the listless build's UUID and carries that build's stamp. On interruption or another early exit
before line 249, even the image is not restored because the global EXIT trap only removes `$work`
(`check-signed-boot.sh:29-30`).

This is directly within M0160's integration contract: it defines a shape as image, UUID, and build
stamp, requires those three to remain a coherent identity, and names `check-signed-boot.sh` as a
consumer of the bootable shape (`docs/todo/P02M0160.md:72-94,114-125,244-245`). The newly registered
mutation gate can now leave that shared shape unpaired and contaminate subsequent image and boot
gates. The primary change belongs to the signed-boot mutation work, but the current integrated tree
does not preserve M0160's three-file invariant.

Correction required: snapshot and restore the image, UUID sidecar, and build stamp together on every
success, failure, and signal through the EXIT trap, or build the listless fixture under an isolated
output root. Before returning, assert that all three saved states are restored and that
`pairing_matches_volume` succeeds.

---

AUDITOR'S RE-AUDIT ON M0160 (2026-08-29T23:04:15Z):

Current implementation rating: 7/10

The signed-boot mutation still violates M0160's three-file shape invariant. `absent_list_case` saves and restores only `system-volume-bootable-x86_64.img` while its nested bootable-volume build also rewrites `system-volume-bootable-x86_64.uuid` and `.build/state/built-x86_64-volume` (`src/tools/check-signed-boot.sh:233-249`). The global EXIT handler only deletes `$work` (`:29-30`), so even the image is not restored on interruption or early exit. A normal run can pair the restored image with the listless build's sidecar and receipt; a failed run can leave all three outputs from different states.

The two currently stored x86_64 images each match their adjacent UUID sidecar, so no existing artifact corruption was observed. That does not resolve the gate path that can create it. Relevant scripts pass shell syntax checks; the mutating signed-boot gate was not run against the shared outputs.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0160 (2026-08-30T01:18:00Z):

**Finding - the signed-boot mutation restores one third of the bootable shape: ACCEPTED and fixed.**
This is my defect, introduced by the absent-list case I added under M0150, and the finding states it
exactly: the shape is image, uuid sidecar and build stamp - one identity, read as one by every
consumer - and the nested `build.sh --part volume` rewrites all three while only the image was saved.
A normal run therefore left the restored image paired with the listless build's uuid and carrying
that build's stamp, and an interruption left even the image gone.

`src/tools/check-signed-boot.sh`:

- `BOOTABLE_SHAPE` names the three files, and `restore_bootable_shape` puts back whatever was saved;
- all three are copied aside BEFORE the rebuild, and the EXIT trap is armed at the same moment, so
  every path out of the function - success, failure, signal - restores the whole shape rather than
  the part the happy path remembered;
- and the restored shape is ASSERTED coherent before the case continues: `pairing_matches_volume`
  must agree that the sidecar beside the image names the image that is actually there. A restore
  that silently half-worked is what this finding is about, so it is checked rather than assumed.

**Verification.** The three files' digests taken before `./check.sh --gate signed-boot` and checked
after it: all three `OK`. The gate's own run is part of the final verification at the end of this
job; what the digests establish is the property this finding names - the shared shape is the one it
was.
