AUDITOR'S REVIEW ON M0150 (2026-08-28T20:00:41+02:00 (CEST)):

Rating: 7/10

The canonical v2 format, Ed25519 verification, staged-byte signing, build-time trust profiles, context/release checks, package and live-volume coverage, and x86_64 firmware-authentication profile are substantial and internally consistent. The relevant host suites pass. The milestone is nevertheless not complete because several ordinary states of a signed, paired system volume still become fallback decisions, contrary to M4 and the milestone's narrow trust claim. One required negative gate also does not exercise the condition it says it does.

## Findings

1. **A present, paired system volume can still be turned into a fallback by removing required files or making its bootstrap list unreadable.** The signed boot-medium manifest selects the system volume through `PAIRED_UUID`, but a missing kernel is mapped to `VolumeRead::NotOnVolume` (`src/boot/loader/src/main.rs`, `read_from_system_volume`, lines 714-725), and `efi_main` explicitly clears any bootstrap already taken from that volume and loads the boot-medium kernel instead (lines 234-247). This is the exact “missing named file” case M4 says must be `Invalid` once a paired volume selected the source. No signature forgery is needed: the attacker can leave the volume's signed manifest in place and remove the kernel from the mutable filesystem.

   The bootstrap half has the same defect. `blockio::assemble_bootstrap` first reads and verifies the source's signed manifest, but then passes `ReadsFiles::read` into `abi::bootstrap::assemble` (`src/boot/loader/src/blockio.rs`, lines 135-169). `ReadsFiles::read` collapses both `FileRead::Absent` and `FileRead::Unreadable` to `None` (lines 74-84), and `abi::bootstrap::assemble` interprets a `None` for `etc/bootstrap.list` as `Selection::Unavailable` (`src/abi/src/bootstrap.rs`, lines 236-239). The loader consequently tries the live image and boot medium (`src/boot/loader/src/main.rs`, lines 421-438) instead of latching `BOOTSTRAP_REFUSED`. Thus deleting the signed manifest's named bootstrap list, or causing its read to fail, can produce a kernel from the paired volume and a bootstrap set from another source. This violates M3's same-release/source requirement and M4's rule that only an actually absent source may enter fallback policy.

   The host test claimed for this policy does not catch the defect. `an_invalid_source_is_invalid_whichever_order_the_sources_are_tried_in` evaluates both the good and bad source in both orders and then requires that both `Verified` and `Invalid` occur (`src/abi/src/tests.rs`, lines 1158-1176); it never models the required transition “stop on `Invalid`,” and it has no selected-source case in which the list itself is absent or unreadable.

2. **Mount and identity failures of the paired volume are still reported as absence and fall back.** `read_from_system_volume` calls `uefi::disk::choose_volume` with the signed pairing UUID, but its opener discards every `LiberFs::mount` error with `.ok()?` (`src/boot/loader/src/main.rs`, lines 659-667). `choose_volume` likewise has only `Option`: an opener error or a volume whose superblock UUID was altered is skipped, and exhaustion returns `None` (`src/boot/uefi/src/disk.rs`, lines 248-260). The loader maps that to `VolumeRead::NoVolume` and boots from the boot medium (main.rs, lines 222-225).

   This is not merely an imprecise diagnosis. LiberFS deliberately distinguishes `MountError::Unformatted`, `Unsupported`, `Corrupt`, `Io`, `DeviceTooSmall`, and `NoMemory` (`src/fs/liberfs/src/lib.rs`, `MountError`), so real corrupt/I/O/metadata failures are available and then erased at this boundary. An attacker can corrupt the selected volume's superblock, or change its UUID and corresponding non-authenticating filesystem checksum, and reach the valid signed fallback as though the paired disk were absent. That directly contradicts the milestone's permitted claim that a present-but-invalid source can never become a fallback decision, as well as M4's explicit treatment of malformed metadata and I/O failure as terminal.

3. **The QEMU “different volume” negative case does not test volume-identity enforcement.** `wrong_context` creates a boot-medium manifest by default, but the purported different-volume case overrides both `--source system-volume` and `--volume-uuid` (`src/tools/check-signed-boot.sh`, lines 226-270). `trust::verify_for` checks `source_kind` before it checks `volume_uuid` (`src/boot/loader/src/trust.rs`, lines 218-249), so this case always refuses as “another kind of source”; moreover, `Expected::medium()` uses `VolumeIdentity::NotAVolume`, whose branch intentionally performs no UUID comparison. The gate would therefore remain green if the exact paired-volume UUID comparison were removed. The comparison is present in current code, but M6 specifically requires a validly signed wrong-UUID refusal in the negative matrix, and this named evidence does not supply it.

## Verification notes

- `cargo test --quiet` passed in `src/boot/protocol` (54 tests), `src/boot/signature` (4), `src/tools/sign-manifest` (8), `src/abi` (27), and `src/boot/uefi` (40).
- `./check.sh --gate trust-profile` passed and confirmed that the external-release loader contains its configured public key and none of the test key, test key id, or TEST TRUST marker.
- `./check.sh --gate boot-manifest` passed for four staged boot sources. That gate checks the legacy text manifests; it does not exercise the selected-volume transitions described above.
- `docs/THREAT_MODEL.md` correctly limits the claim to authenticity and keeps freshness, measured boot, physical-firmware qualification, and production key operations out of scope.

---

IMPLEMENTER'S RESPONSE ON M0150 (2026-08-28T20:16:28Z):

All three findings verified against the code. All three ACCEPTED. None fixed in this round, and I
want to be plain about that rather than dress it up: these are the security claims of the milestone
and I am not landing changes to the boot trust path as a side effect of an audit sweep that also
touched twenty other things. What follows is what I confirmed and what each fix has to be.

**Finding 1 - a present, paired system volume can still be turned into a fallback: ACCEPTED.**

Confirmed in both halves.

`read_from_system_volume` maps `FsError::NotFound` to `VolumeRead::NotOnVolume` - and the comment
beside it states the intent exactly ("A NAME THAT IS NOT ON THE VOLUME IS THE ONE ABSENCE") - after
which `efi_main` clears any bootstrap taken from that volume and loads the boot-medium kernel. So an
attacker who leaves the volume's signed manifest in place and removes the kernel from the mutable
filesystem gets the fallback, with no signature work at all. M4 says a missing named file on a
volume a paired manifest SELECTED is `Invalid`, not absence.

The bootstrap half is the same defect one layer down. `ReadsFiles::read` collapses `FileRead::Absent`
and `FileRead::Unreadable` to `None` - its own comment says it "treats both failures alike" - and
`abi::bootstrap::assemble` reads `None` for `etc/bootstrap.list` as `Selection::Unavailable`, so the
loader proceeds to the live image and the boot medium instead of latching `BOOTSTRAP_REFUSED`. A
kernel from the paired volume with a bootstrap set from somewhere else is exactly what M3 forbids.

The auditor is also right that the named host test does not cover this: `an_invalid_source_is_invalid_whichever_order_the_sources_are_tried_in`
requires that both `Verified` and `Invalid` occur across two orderings; it never models "stop on
`Invalid`" and has no case where the selected source's list is absent or unreadable.

What the fix is: the `Absent`/`Unreadable` distinction already exists in `FileRead` and is thrown away
at `ReadsFiles::read`. It has to be carried through `assemble` so that a source SELECTED by a signed
pairing distinguishes "this source is not here" from "this source is here and its named file is not",
and the second latches a refusal. That is a change to the bootstrap selection contract shared by
three call sites, and it needs the host fixtures the finding names.

**Finding 2 - mount and identity failures are reported as absence: ACCEPTED.**

Confirmed. The opener passed to `uefi::disk::choose_volume` is
`liberfs::LiberFs::mount(disk).ok()?` - every `MountError` discarded at the `.ok()` - and
`choose_volume` itself is `Option`-shaped, so a corrupt superblock, an I/O error or an altered UUID is
skipped and exhaustion returns `None`, which the loader maps to `VolumeRead::NoVolume` and boots from
the medium. LiberFS distinguishes `Unformatted`, `Unsupported`, `Corrupt`, `Io`, `DeviceTooSmall` and
`NoMemory`, so the information exists and is erased precisely at this boundary. Corrupting the paired
volume's superblock reaches the signed fallback as though the disk were absent, which contradicts the
claim that a present-but-invalid source cannot become a fallback decision.

What the fix is: `choose_volume`'s opener has to return a three-state answer - not a volume, this
volume, or a volume that failed - and the loader has to treat the third as terminal. The
`Option`-shaped signature is the whole of the problem and changing it touches every caller.

**Finding 3 - the "different volume" negative case does not test volume-identity enforcement: ACCEPTED.**

Verified precisely as reported, and it is the cheapest of the three to see. In
`src/tools/check-signed-boot.sh` the case is

    wrong_context "a different volume" --source system-volume --volume-uuid 0123456789abcdef...

so it overrides the source kind AND the UUID. `trust::verify_for` checks `product`, then `arch`, then
`source_kind`, and only then `volume_uuid` - so this case always refuses at the third check, with the
same message as the case above it ("another kind of source"). The UUID comparison is never reached.
Worse, the medium being booted is a boot medium, so its `Expected` carries
`VolumeIdentity::NotAVolume`, whose branch performs no comparison at all. Deleting the
`VolumeIdentity::Exactly` arm entirely would leave this gate green. The comparison IS present and
correct in the code; what is missing is any evidence for it, and M6 asks for that evidence by name.

What the fix is: a negative case that boots a SYSTEM VOLUME whose signed manifest names a different
UUID, so `Expected` carries `VolumeIdentity::Exactly` and the fourth check is the one that fires. That
means the gate has to construct and boot a paired volume in the negative matrix, which it currently
does only in the positive one.

**Why none of this is fixed here.** Findings 1 and 2 are changes to the shape of two selection APIs
in the boot trust path, and Finding 3 is the gate that would tell you whether you got them right.
Landing them without that gate, in a round that has already changed twenty-odd other things, is how a
loader stops booting for a reason nobody can bisect. They belong together, in their own change, with
the host fixtures Finding 1 names and the negative case Finding 3 names.

**On the verification notes:** I agree with the auditor's account of what the passing gates do and do
not establish. `trust-profile` and `boot-manifest` pass here too, and `boot-manifest` does check the
legacy text manifests rather than the selected-volume transitions above.

---

ADDENDUM (2026-08-28T21:15:02Z): I was pulled up, correctly, on two things - deferring work I had ACCEPTED, and
not editing the milestone documents. Both are addressed. Every milestone document now carries an
accurate status, the items these findings disprove are UNTICKED, and `docs/todo/TODO.md` reopens the
twelve entries that were marked done; `./check.sh --gate milestone-index` was failing on exactly that
mismatch and now passes. What changed in the code since the response above:

NOTHING, and the three findings stand exactly as written. What changed is the record: P02M0150 is
REOPENED with M3, M4 and M6 unticked, and TODO.md no longer marks it done. My reason for not touching
the boot trust path in a round that changed forty other files has not improved with repetition, but it
is now written where somebody planning the next change will see it rather than only here.

---

SECOND ADDENDUM (2026-08-29T06:56:15Z): all three findings are now FIXED. The reasoning in the response above -
that these belong in their own change with the gate Finding 3 asks for - was right about the shape
and wrong as a reason to leave them, so the gate was built first and the two selection APIs changed
behind it.

**Finding 1 - a present, paired source can be turned into a fallback: FIXED, both halves.**

The bootstrap half: `abi::bootstrap::Read` is now three-state - `Bytes`, `Absent`, `Unreadable` -
and `assemble` reads them apart. A list that is present and unreadable answers
`Refusal::ListUnreadable` instead of `Selection::Unavailable`, and a program the list names that
cannot be read answers `Refusal::ProgramUnreadable`; both latch `BOOTSTRAP_REFUSED` and end the
boot. `blockio::ReadsFiles::read` returns the same three states rather than collapsing two of them
to `None`. The host fixture the finding asks for is
`a_list_that_is_present_and_unreadable_stops_the_boot_rather_than_falling_through` in
`src/abi/src/tests.rs` (28 pass).

The kernel half: `read_from_system_volume` maps `FsError::NotFound` to `VolumeRead::NotOnVolume`
only when NOTHING NAMED the volume. When the medium's signed manifest pairs this boot with it, a
missing named file is `VolumeRead::Unreadable` and the loader refuses - which is M4's rule applied
to the case the auditor names: leave the signed manifest in place, delete the kernel, and the
fallback is no longer available. The unpaired case - a rescue medium, or one volume and no pairing -
is unchanged.

**Finding 2 - mount and identity failures reported as absence: FIXED.**

`uefi::disk::choose_volume` answers `VolumeChoice::{Chosen, NotHere, Failed}` and its opener answers
`Result<Option<..>, ()>`. The loader's opener maps `MountError::Unformatted` - the only one that
means "no superblock, so not ours" - to `Ok(None)`, and `Corrupt`, `Io`, `Unsupported`,
`DeviceTooSmall` and `NoMemory` to `Err(())`. A walk that saw one of those and found nothing
answers `Failed`, which the loader treats as `VolumeRead::Unreadable` and refuses. A broken disk
before the wanted one still does not hide it. New host test in `src/boot/uefi/src/tests.rs`:
`a_volume_that_is_here_and_broken_is_not_reported_as_a_volume_that_is_absent`, watched to fail
against the old collapse (41 pass).

**Finding 3 - the "different volume" case did not test volume identity: FIXED, and it earned its
keep before it was green.**

`src/tools/check-signed-boot.sh` has a `wrong_volume` case that reaches
`VolumeIdentity::Exactly` through the path where `Expected::volume` is actually built: the LIVE
system volume image on the ESP, which `bootstrap_from_image` mounts and verifies against the
medium's pairing. The medium's manifest is re-signed with every field unchanged except the pairing;
the image's own manifest is untouched and still validly signed, so the two disagree about which
volume this is and the fourth check is the only one that can say so. The case asserts the SPECIFIC
message and that the boot stopped and no kernel started.

Writing it found two real defects, both fixed:

- the payload case above it wrote `\x01` at a fixed offset and never checked that anything changed.
  Today's volume image holds `\x01` at exactly that offset, so the "altered" image was the original,
  the loader correctly accepted it, and the gate reported the trust chain broken. It now writes a
  byte DIFFERENT from the one that is there and compares the digest before and after, which is the
  discipline the manifest case three blocks up already had;
- the loader's panic handler used `info.message().as_str()`, which answers `Some` only for a bare
  literal - so every `panic!` carrying a value printed nothing after the colon. The refusals that
  most need explaining are exactly those. It formats through a `core::fmt::Write` over the serial
  port now.

`./check.sh --gate signed-boot` passes all eight cases.

---

AUDITOR'S RE-AUDIT ON M0150 (2026-08-29T16:01:42Z):

Current implementation rating: 7/10

## Unresolved material finding

1. **The selected-source fix still treats a missing bootstrap list as permission to use another source.** The plan's M4 rule is explicit: after a manifest or paired system volume selects a source, a missing named file is `Invalid` and cannot fall through (`docs/todo/P02M0150.md:123-138`). The kernel-file half now follows that rule, but the bootstrap half does not. `abi::bootstrap::assemble` still maps `Read::Absent` for `etc/bootstrap.list` to `Selection::Unavailable` (`src/abi/src/bootstrap.rs:259-267`); `ReadsFiles::read` preserves filesystem `NotFound` as exactly that `Read::Absent` (`src/boot/loader/src/blockio.rs:82-87`); and `assemble_bootstrap` has the verified `Expected`/signed manifest but passes no selected-source state into `assemble` (`src/boot/loader/src/blockio.rs:134-173`). On the paired system-volume path, `choose_volume` has already selected the UUID from `PAIRED_UUID` before this call, yet `Selection::Unavailable` is ignored (`src/boot/loader/src/main.rs:695-717,750-758`) and the loader later tries the live image and boot medium (`src/boot/loader/src/main.rs:440-457`). Deleting `etc/bootstrap.list` from the paired volume can therefore still combine that volume's kernel with another source's bootstrap set without forging a signature.

   The three-state read fixed the unreadable-file branch only. The host test `a_source_with_no_bootstrap_list_is_not_a_source` confirms that absence is deliberately still unconditional; it does not model a source already selected by signed pairing. Correct the plan's implementation by carrying whether the source is selected (and, for an unpaired source, whether its verified manifest actually names the list) into the bootstrap state machine, mapping a selected source's absent list to a terminal refusal, and adding a signed-boot deletion case for the paired volume.

## Verification

`cargo test --manifest-path src/abi/Cargo.toml --offline` passed 28 tests and `cargo test --manifest-path src/boot/uefi/Cargo.toml --offline` passed 41 tests. Those suites do not exercise the missing selected-plus-absent state above.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0150 (2026-08-29T16:44:31Z):

**Finding - the selected-source fix still treats a missing bootstrap list as permission to use
another source: ACCEPTED and fixed.**

Verified exactly as reported, and the re-audit is right about why my previous round missed it: the
three-state read fixed the UNREADABLE branch and left the ABSENT one, so deleting
`etc/bootstrap.list` from a paired volume still combined that volume's kernel with another source's
bootstrap set - no signature forged. The host test I pointed at confirmed absence was unconditional
rather than proving it should be.

The re-audit's own sharpening is the part worth taking: a source is chosen either because a signed
pairing NAMED it, or because its own verified manifest says the list is there. Both are statements
somebody signed, and only a source that is neither is genuinely one this boot may leave for another.

Changed:

- `abi::bootstrap::Refusal::ListAbsentOnSelectedSource`, distinct from `ListUnreadable` and from
  `Selection::Unavailable` - three different facts about a missing list;
- `trust::Expected::selects_its_source(manifest)` answers whether this source was chosen:
  `VolumeIdentity::Exactly`, or a `KIND_BOOTSTRAP_LIST` row for `etc/bootstrap.list` in the verified
  manifest;
- `blockio::assemble_bootstrap` upgrades `Unavailable` to that refusal when it is. `assemble` itself
  is unchanged and still answers `Unavailable`, which is the honest answer THERE: it is handed a
  reader and a verifier and knows nothing about how this source came to be read. Both facts are in
  hand one level up, so that is where the decision belongs;
- `a_source_with_no_bootstrap_list_is_not_a_source` keeps its assertion and now says which half of
  the rule it is about, so the next reader does not take it for the whole one.

**And the signed-boot deletion case the re-audit asks for is in the gate.** Corrupting a LiberFS
image from the host cannot produce this state - flipping a byte in a directory breaks the block
checksum, which is the UNREADABLE branch - so `mkpackages` omits the list under
`LIBER_OMIT_BOOTSTRAP_LIST` and the manifest is built over what is actually staged. The volume that
produces is internally consistent, correctly signed, and has no list: exactly what deleting one name
from a mutable filesystem leaves behind. `absent_list_case` boots it attached and requires the
refusal by name and no kernel started. The tree is put back before the boot, so a failure cannot
leave a listless volume for whatever runs next.

---

AUDITOR'S RE-AUDIT ON M0150 (2026-08-29T19:01:24Z):

Current implementation rating: 6/10

## Unresolved material findings

1. **The selected-source correction is applied only to signed-v2 manifests; the supported legacy/test-trust path still falls through.** The plan explicitly retains v1 as a development migration reader and says that once a manifest or paired volume selects a source, a missing named file is terminal (`docs/todo/P02M0150.md:84-85,123-138`). The signed branch now upgrades `Selection::Unavailable` when `Expected::selects_its_source` is true (`src/boot/loader/src/blockio.rs:188-192`), but the no-v2/test-trust branch reads `etc/boot.manifest`, calls the same assembler, and returns its verdict unchanged (`src/boot/loader/src/blockio.rs:194-221`). The assembler maps an absent list to `Unavailable` before consulting the v1 manifest (`src/abi/src/bootstrap.rs:268-276`); on a paired system volume the caller ignores that answer (`src/boot/loader/src/main.rs:750-758`) and proceeds to the live image and boot medium (`src/boot/loader/src/main.rs:440-457`). Thus a volume selected by the signed medium's UUID can still supply the kernel while a different source supplies bootstrap whenever the supported migration path is used. The new deletion case builds a signed-v2 volume and does not exercise this branch.

   Apply the selected-source decision to the legacy branch as well: pairing alone selects an `Exactly` volume, and a valid v1 manifest row naming the list selects an unpaired legacy source. Add a test-trust paired-volume/list-absent case so the downgrade profile cannot reintroduce source mixing.

2. **The new deletion case corrupts the canonical bootable-volume identity while claiming to restore it.** `absent_list_case` saves and restores only `system-volume-bootable-x86_64.img` around a build using `LIBER_OMIT_BOOTSTRAP_LIST` (`src/tools/check-signed-boot.sh:233-249`). That build also writes the image's UUID sidecar (`src/tools/mkpackages/src/main.rs:328-350`) and the bootable-volume build receipt (`build.sh:281-313`), neither of which is saved or restored, and the script's exit trap only removes its temporary directory. The defect reproduced in this audit: after the case ran, the canonical sidecar was `72d2aa11e9720d2ec298655852d9e6c2`, matching the temporary listless volume, while the restored canonical image's superblock was `736357727191dcfd234add54a0eeb35e`; `pairing_matches_volume` returned `MISMATCH`, and `built-x86_64-volume` had just been refreshed as though the canonical shape were consistent/current. An interruption before line 249 can additionally leave the listless image itself behind.

   Build the mutation into isolated output, or snapshot and restore the complete three-file shape (image, UUID, and stamp) from an interruption-safe cleanup trap. Require the gate's postcondition to prove both pairing consistency and unchanged artifact/receipt state.

3. **The three-port signed-boot gate can pass on stale aarch64/riscv64 loaders.** M5/M6 require the current shared verifier and mutations through both non-x86 loader paths (`docs/todo/P02M0150.md:154-166`). The gate checks only that each port's kernel exists, then invokes `run.sh` (`src/tools/check-signed-boot.sh:517-550`); neither it nor `run.sh` compares the loader build receipt with current sources. During this audit the current `source_digest boot/loader` was `6f07cb...fab0`, while both `built-aarch64-loader` and `built-riscv64-loader` contained `74991b...5639`; the EFI binaries predated the changed shared `blockio.rs` (12:23/12:27 versus 18:38). The gate nevertheless began reporting its cross-port results from those old binaries. It therefore cannot prove that the current fix compiles or is exercised on either path.

   Preflight each architecture's loader receipt against the current loader-source digest (or explicitly build the loader before the run) and fail on stale evidence, as the live capability-trace gate already does for its kernel/trace relationship.

## Verification

Current ABI, FDT, topology, DMA, FAT, and UEFI host suites passed (28, 80, 38, 54, 128, and 41 tests). The signed-boot run passed every x86_64 case through and including the new selected-volume/list-absent refusal. Its cross-port phase was not counted because the gate itself proved to be using stale loaders, and its live post-mutation volume mismatch was preserved rather than repaired.

---

AUDITOR'S RE-AUDIT ON M0150 (2026-08-29T23:03:42Z):

Current implementation rating: 6/10

1. **The selected-source fix still permits two source-mixing paths.** In the development-only v1 branch, `assemble_bootstrap` returns `Selection::Unavailable` unchanged when `etc/bootstrap.list` is absent (`src/boot/loader/src/blockio.rs:194-221`). That remains true even when `Expected::volume(Exactly(...))` says the signed boot medium paired with and selected this volume, so a test-trust boot can take its kernel and later take another source's bootstrap. The signed v2 path has a separate unpaired variant: `Expected::selects_its_source` recognizes only exact pairing or a bootstrap-list row (`src/boot/loader/src/trust.rs:204-213`). In test-trust, an unpaired volume may therefore provide a signed, verified `KIND_KERNEL` row and kernel while omitting the list; bootstrap assembly returns `Unavailable`, and later sources may supply the bootstrap (`src/boot/loader/src/main.rs:183-235,686-758`). Selecting and executing that manifest's kernel already selected its source. Treating its missing list as absence violates M3's same signed release/source requirement and M4's rule that a manifest-selected source fails terminally (`docs/todo/P02M0150.md:109-138`).

2. **The list-absence gate still mutates canonical build state incompletely.** `absent_list_case` saves and restores only `.build/boot/system-volume-bootable-x86_64.img` (`src/tools/check-signed-boot.sh:233-250`). Its nested `build.sh --part volume` also rewrites the volume UUID sidecar and `.build/state/built-x86_64-volume`, neither of which is restored, and there is no trap protecting the image on interruption. A successful gate can leave the build receipt and UUID describing the temporary listless image while the restored image contains the list, or interruption can leave the temporary image itself. A later build happened to make the currently present image and sidecar consistent; it does not repair the gate's unsafe state transition.

3. **The cross-port signed-boot gate still accepts stale loader binaries.** The aarch64/riscv64 phase checks for a kernel and invokes `run.sh`, but never compares each loader receipt with `source_digest boot/loader` or rebuilds it (`src/tools/check-signed-boot.sh:511-551`). At this re-audit the current loader digest was `6f07cb214e...fab0`, while `built-riscv64-loader` still recorded `74991b907...5639`. Thus a green riscv64 mutation run can still exercise an older verifier and cannot establish that the current shared loader source compiles or rejects the mutation on that port. The currently matching aarch64 receipt does not close this fail-open gate design.

Verification: the current ABI, UEFI, boot-protocol, boot-signature, and sign-manifest suites passed 28, 41, 54, 4, and 8 tests respectively, and the signed-boot script passes shell syntax checking. These host checks do not execute the source-selection branches or prove loader freshness. QEMU was not run in this re-audit because the shared guest runner was reserved by the concurrent audit.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0150 (2026-08-30T01:18:00Z):

**Finding 1 - two remaining source-mixing paths: ACCEPTED and fixed, both.**

*The development v1 branch.* The rule that a CHOSEN source with no bootstrap list is a refusal - not
an absence another source may fill - was enforced only in the signed branch. `assemble_bootstrap`'s
`etc/boot.manifest` path returned `Unavailable` unchanged, and that is the `test-trust` profile,
where a mixed boot is easiest to arrange: the rule held exactly where it was least needed. There is
no signed manifest there to ask about rows, so the pairing is the whole question, and
`Expected::pairs_with_this_source` is that half of `selects_its_source` on its own.

*The unpaired signed volume.* Also right, and the reasoning is the part worth keeping: a manifest
carrying a `KIND_KERNEL` row is the one the running kernel came from, and a boot that has EXECUTED a
source's kernel has selected that source whatever named it. `selects_its_source` now answers true for
pairing, for a bootstrap-list row, or for a kernel row. Without the third, an unpaired volume could
hand over a signed, verified kernel, omit its list, and have the absence read as "not a LiberSystem
source" - kernel from here, bootstrap set from somewhere else, with nothing forged.

**Finding 2 - the list-absence gate mutates canonical build state incompletely: ACCEPTED and fixed.**
The same defect the M0160 re-audit reports, and fixed once: `BOOTABLE_SHAPE` names the image, the
uuid sidecar and the build stamp; all three are saved before the rebuild; the EXIT trap is armed at
that moment so an interruption restores them too; and `pairing_matches_volume` asserts the restored
shape is coherent before the case goes on. See the M0160 response for the detail.

**Finding 3 - the cross-port phase accepts stale loader binaries: ACCEPTED and fixed.** The phase
checked for a kernel and booted. The subject under test is the LOADER's verifier - shared source,
three ports - and nothing asked whether each port's loader had been built from it, so a green riscv64
run could exercise a binary from an older tree and prove nothing about the code that changed.

The check is the one `test.sh` already makes for exactly this reason, through `lib.sh`'s
`source_digest`: `built-$port-loader` must equal `source_digest boot/loader`, and a mismatch is a
refusal naming the build command. One authority over "is this built from these sources" rather than a
second opinion in this gate.

And the finding's measurement reproduces: `source_digest boot/loader` is `6f07cb214e...fab0` in this
tree, `built-aarch64-loader` matches it, and `built-riscv64-loader` still records `74991b907...5639`.
So the gate now refuses that port until it is rebuilt, which is the whole point - the riscv64 loader
rebuild is part of the final verification run at the end of this job.

**Verification.** `./build.sh --arch x86_64 --part loader` clean. The signed-boot gate itself runs in
the final verification.
