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
