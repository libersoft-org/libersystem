IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0171 (2026-09-09T16:20:00Z):

Scope: the seven items of `docs/todo/P02M0171.md` - the signed security generation (M1), the
`rollback-enforcing` build identity with its provisioning ceremony and marker (M2), the frozen
64-byte state record and the typed variable path (M3), the fail-closed compare-and-advance with
convergence of both slots (M4), the signed purpose field against purpose-scoped roots (M5), the
signer rotation that closes the pre-policy loader (M6), and the persistent multi-boot gate with its
host fixtures (M7). This record is written as the work proceeds; the verification section at the
end says what was actually run.

## What the tree has, verified before designing

- Manifest v3 (`LBRMAN\x03\x00`, domain `libersystem-boot-manifest-v3\0`) already carries
  `security-generation: u64` and `purpose: u32` (`1` boot, `2` recovery), covered by the signature,
  refused for any other purpose value - landed with P02M0172's manifest evolution so the format
  changed once, as this milestone's Dependencies ask. `sign-manifest` takes `--generation` (default
  1) and `--purpose`; `mkpackages` reads `LIBER_SECURITY_GENERATION`; `mkimage.sh` passes both.
  What is missing is everything that CONSUMES them: no latch across manifests, no purpose-scoped
  roots, no floor.
- The loader's trust is compile-time: `LIBER_TRUST_PROFILE` is `test-trust` (default) or
  `external-release`, `ROOTS` is a compiled-in array of `Root { key_id, key }`, `verify_for`
  latches the release string across every manifest of one boot and records the DMA-mode tag.
- The UEFI layer (`src/boot/uefi/src/variables.rs`) types exactly `GetVariable` for `SecureBoot`
  and `SetupMode`, collapses every status to `None`, and leaves `SetVariable` as an untyped pointer
  with the reason written in the file.
- Ordinary QEMU runs copy a fresh OVMF variables image per boot (`qemu-run.sh`), and the Secure Boot
  gate clones its enrolled store per boot. `virt-fw-vars` on this machine takes `--set-json` (any
  variable: name, GUID, attributes, hex data), `-d NAME`, `--set-dbx FILE` and `--output-json`,
  which is enough to perform the ceremony, delete slots, and read the store back from the host.
- LiberFS checksums every data block (CRC32C), so a volume's embedded manifest cannot be re-signed
  by splicing bytes into the image; a volume at another generation is BUILT
  (`LIBER_SECURITY_GENERATION=N ./image.sh ...`).
- `Vulkan`-style separate signer identities do not exist yet: the Secure Boot gate has one test
  platform key (`.build/secureboot/test-pk.*`) that signs whichever loader it is given.

## Decisions

- The shared codec, state classification and compare/advance DECISION live in
  `bootproto::rollback` (host-tested, `no_std`), beside `bootproto::dma_mode`; the loader executes
  the decision (reads, writes, readbacks, halts) in a new `rollback.rs`. The product identity is
  `sha256(THIS_PRODUCT)` over the loader's existing compiled-in constant - no fifth copy.
- The generation and purpose latches share ONE mechanism (`bootproto::latch::Latch<T>`), which the
  release latch in `trust.rs` also moves onto, so the three cannot drift.
- `rollback-enforcing` is a third `LIBER_TRUST_PROFILE`. Its manifest roots are the external release
  key when `LIBER_TRUST_KEY`/`_ID` are given, otherwise the published test keys - because the QEMU
  gate signs its own artifacts with the published keys and the profile is the x86_64 OVMF completion
  profile. It announces itself as enforcing before it loads anything.
- Purpose-scoped roots: `Root` gains `purposes: u32` (a bitmask of the manifest purpose values). The
  test profile carries TWO published keys - the existing boot key `0x7e570001` and a new recovery
  key `0x7e570002` - so cross-use negatives can be signed. `sign-manifest --signing-key boot|recovery`
  selects which published key signs, independently of `--purpose`.
- The enforcing loader is signed for Secure Boot by a SECOND test platform key
  (`.build/secureboot/rollback-pk.*`); the new variables image enrols only that signer and puts the
  old signer's certificate in `dbx`. The non-enforcing loaders keep the old signer.
- The gate cannot cut power between two UEFI variable writes in QEMU on demand; it CONSTRUCTS the
  interrupted state `{A=N, B=N+1}` with the ceremony tool instead - which is the state the
  interruption leaves - and proves the equal-generation boot converges it. The host fixtures over a
  mocked firmware cover torn, short, oversized, wrong-product, wrong-attribute, absent, access-denied,
  device-error, write-failed and readback-mismatch outcomes, both slots invalid, the two marker cases,
  and the cross-slot copy.

## Implementation record (2026-09-09T16:40:00Z)

### M1 - the signed generation, latched
- `bootproto::latch::Latch<T>` (new, host-tested): first value latched, every later one must
  equal it, a conflict names both. `trust.rs` keeps `GENERATION: Latch<u64>` and `PURPOSE:
  Latch<u32>` and records both in `verify_for` after the release latch; a mismatch refuses with
  the two values named ("refusing to compose a system from two generations") or the purpose
  sentence. `trust::latched_generation()` is the only input to the floor.
- `sign-manifest`: `external-release` now REFUSES a manifest without an explicit `--generation`;
  `test-trust` keeps the default of 1 (every test build's value). Manifest v3's canonical `u64`
  and its refusals were landed with P02M0172 and are unchanged.

### M2 - the enforcing profile and the ceremony
- `LIBER_TRUST_PROFILE=rollback-enforcing` is the third profile: `IS_ROLLBACK_ENFORCING`, roots
  from `LIBER_TRUST_KEY`/`_ID` (and `LIBER_TRUST_RECOVERY_KEY`/`_ID`) when given, the published
  test keys otherwise; an unknown profile name is a compile error (const assertion). The loader
  announces `ROLLBACK FLOOR ENFORCED` before it loads anything; the other two profiles print
  `rollback floor - not enforced by this build's trust profile` and keep the per-run fresh
  variables image.
- The separate signer: the gate generates `.build/secureboot/rollback-pk.*` and signs only the
  enforcing loader with it; the non-enforcing loaders keep `test-pk`; the rotated store enrols
  the new signer as PK/KEK/db and puts the old signer's x509 certificate into `dbx` (an ESL built
  by the gate, written with `virt-fw-vars --set-json`).
- The marker, the partial-state table and the ceremony order (slot A, read back; slot B, read
  back; marker last) are implemented in `bootproto::rollback::classify` and in the gate's
  `ceremony_until`, which produces the interrupted prefixes on purpose.
- `build-loader-private.sh` accepts the third profile; `check-trust-profile.sh` builds it with the
  release key and asserts the marker and the absence of both published keys, and that an unknown
  profile name does not build.

### M3 - the record and the typed variable path
- `bootproto::rollback` (new, `no_std`, 86 host tests across bootproto): the 64-byte record at
  the frozen offsets, the 57-byte tag input with the slot index byte (`0x00`/`0x01`), the vendor
  GUID and the three names (also as UTF-16 constants), attributes `NON_VOLATILE |
  BOOTSERVICE_ACCESS`, `Read` with distinct outcomes (absent, present, oversized, access denied,
  device error, failed), `slot_state`, `marker_state`, `classify`, `decide`, and `enforce` over a
  `Firmware` trait with the readback after every write.
- `uefi::variables`: `RuntimeServices.set_variable` typed; `RollbackStore` implements the trait
  over `GetVariable`/`SetVariable` for exactly the three variables under the vendor GUID (a
  closed selector, no name parameter), mapping `NOT_FOUND`, `BUFFER_TOO_SMALL`, `ACCESS_DENIED`,
  `DEVICE_ERROR` and every other status distinctly. The file's argument against a write path is
  answered in place. Two new host tests (the ninth entry's offset, the namespace).
- `sign-manifest --rollback-record a|b --product P --generation N` prints the record; the gate
  and the loader therefore share one codec.

### M4 - compare and advance
- `bootproto::rollback::decide`: below refuses; equal writes only a lagging slot; above writes the
  lagging (or A) slot first and the other second, each read back before the next, so the steady
  state after any accepted boot is both slots equal. The loader (`rollback.rs`) runs `enforce`
  after `dma_mode::resolve` and before `hand_off`, prints the verdict, and halts on every fault.

### M5 - the purpose field against purpose-scoped roots
- `Root { purposes }`; `verify` checks `root.purposes & manifest.purpose` after the signature;
  the test profile carries a second published key (`0x7e570002`, recovery) and `sign-manifest
  --signing-key boot|recovery` selects which signs, independently of `--purpose`. `mkpackages`
  reads `LIBER_MANIFEST_PURPOSE` (boot|recovery) and signs the volume with the matching key;
  `mkimage.sh` passes both to the medium's manifest.

### M6/M7 - the gate
- `src/tools/check-rollback-floor-x86_64.sh` (registered in `check.sh` and the verify-model
  catalog, `bin.libersystem-loader`, boots a guest): builds the enforcing loader privately, the
  two signers, the two stores, and four media through `image.sh` at generations 3, 2 (boot and
  recovery) and 1 (last, so the tree's image is left at the default); then the sequence in the
  gate's header. `guest-verdict.py` gained `rollback-accepted/refused/unprovisioned/
  manifest-refused/not-enforced`.

### Known limits, stated
- The interrupted advance is CONSTRUCTED (`{A=N, B=N+1}` written by the ceremony tool) rather than
  produced by cutting power between two firmware writes; the torn/refused/readback-mismatch
  writes are host fixtures over the mocked firmware. Said so in the gate header and here.
- The "pre-policy loader" and the "current, correctly signed, non-enforcing loader" are the same
  artifact in this tree (the ordinary loader signed by the old signer); the gate boots it under
  both stores and says so.
- The enforcing completion profile is x86_64 OVMF; the aarch64 and riscv64 loaders compile the
  same trust code (build checked at the end of the batch) and are not gated for the floor.

## Verification (2026-09-09T16:45:00Z)

- `cargo test` in `src/boot/protocol`: 86 passed (rollback: 12 fixtures incl. the layout, the
  cross-slot copy, every invalid shape, the state table, the decision order, the advance, the
  interrupted advance, damaged slots, unprovisioned/no-state, every firmware failure, failed and
  torn writes; latch: 2).
- `cargo test` in `src/boot/uefi`: 43 passed. `src/tools/sign-manifest`: 9 passed.
- `./check.sh --gate trust-profile`: PASSED (three profiles, the enforcing marker only on the
  enforcing build, unknown profile refused).
- `./check.sh --gate rollback-floor-x86_64`: PASSED end to end - 27 reported steps, one persistent
  store, generations 1/2/3, every refusal and repair row, the four manifest negatives, the three
  partial provisioning states, the deleted marker, the pre-policy loader accepted under the old
  store and refused under the rotated one, the unsigned enforcing loader refused, and the
  comparison-skipping loader boot that shows the assertions are load-bearing. The tree's image
  was left at generation 1 (checked with `image-dma-mode.sh` and `sign-manifest --inspect`).
- Not run here: the aarch64/riscv64 loader builds with the new trust code (started, recorded
  when they finish).
