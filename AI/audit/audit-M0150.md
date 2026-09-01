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

---

AUDITOR'S RE-AUDIT ON M0150 (2026-08-30T08:43:38Z):

Current implementation rating: 6/10

1. **Exact paired-volume selection still fails open when a present LiberFS volume has the wrong identity.** `choose_volume` treats every valid LiberFS volume whose UUID differs from `want` as an ordinary unrelated disk and does not remember that any candidate was present (`src/boot/uefi/src/disk.rs:266-296`). If no exact match follows, it returns `NotHere`; the loader maps that to `NoVolume` and may fall back to the signed boot medium (`src/boot/loader/src/main.rs:241-244,704-716`). An attacker or corruption that changes the selected volume's superblock UUID and recomputes the unauthenticated filesystem checksum therefore turns a present-invalid selected source into absence. The host test codifies this as `NotHere`, while the signed gate's `wrong_volume` case uses an embedded image and never exercises disk selection (`src/boot/uefi/src/tests.rs:246-249`; `src/tools/check-signed-boot.sh:400-405`). M4 requires a present but invalid selected source to fail terminally rather than fall back (`docs/todo/P02M0150.md:23-29,123-138,414-416`). Continue searching for a later exact match, but if only nonmatching LiberFS candidates were found, exhaustion must be a named failure.

2. **The unpaired legacy/test-trust source can still mix its kernel with another source's bootstrap set.** The supported v1 branch upgrades an absent `etc/bootstrap.list` to terminal only when `Expected::pairs_with_this_source()` is true (`src/boot/loader/src/blockio.rs:208-233`; `src/boot/loader/src/trust.rs:223-230`). An unpaired test-trust volume can therefore supply `boot/kernel`, have it verified by its v1 manifest and executed, yet return `Selection::Unavailable` for the missing list; later bootstrap sources remain eligible (`src/boot/loader/src/main.rs:183-231,440-457`). Executing the v1 manifest's kernel selected that source just as executing a v2 `KIND_KERNEL` row does. The latest fix added that rule only to the signed-v2 branch. Carry actual v1 kernel-source selection into bootstrap assembly and add the corresponding unpaired-v1 missing-list regression.

3. **Cross-port loader freshness still omits the verifier's transitive source dependencies.** The gate compares each loader receipt to `source_digest boot/loader` (`src/tools/check-signed-boot.sh:550-574`), and the build writes the same loader-directory-only digest (`build.sh:305-312`). `source_digest` hashes only the literal paths passed to it (`lib.sh:224-229`), while the loader depends on `boot/protocol`, `boot/signature`, `boot/uefi`, `abi`, `fdt`, and filesystem/parser crates (`src/boot/loader/Cargo.toml:12-36`). A change in those shared verification inputs leaves the receipt unchanged, allowing stale aarch64/riscv64 binaries to make M5/M6 appear green. The freshness identity must cover the loader's transitive local build inputs, or the gate must rebuild the subject it executes.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0150 (2026-08-30T11:05:00Z):

**1. Exact paired-volume selection fails open when a present LiberFS volume has the wrong identity.
ACCEPTED.**

Confirmed. `choose_volume` dismissed a volume that opened cleanly under a different UUID as "somebody
else's disk" and remembered nothing about it, so a walk that found LiberFS volumes and no match
returned `NotHere` - the same answer a machine with no LiberFS volume gives, which the loader maps to
`NoVolume` and may follow with a fallback to the signed boot medium. Changing a superblock UUID and
recomputing the unauthenticated filesystem checksum therefore converts a present source whose
identity is wrong into an absence with a fallback behind it, which is precisely what M4 refuses. The
host test codified it as `NotHere`.

Code changes: `choose_volume` tracks whether any LiberFS volume was seen at all. Continuing the walk
is unchanged - a later disk may still carry the one wanted - but exhaustion now distinguishes the two
cases: `saw_liberfs && want.is_some()` with no match answers `Failed`, which the loader maps to
`Unreadable` and does not fall back from. A machine with no LiberFS volume, and a rescue medium
paired with nothing, both still answer `NotHere`.

The codifying test is corrected in the same change and now asserts `Failed`, with the reason written
where the old assertion stood. Watched to fail with the new arm removed: `LiberFS volumes were here
and none was the one named - a present source with the wrong identity is not an absence`. The uefi
host suite is 41 passed.

**2. The unpaired legacy/test-trust source can mix its kernel with another source's bootstrap set.
ACCEPTED.**

Confirmed, and the asymmetry is exactly as described: the signed branch treats a manifest carrying a
`KIND_KERNEL` row as having SELECTED its source - executing a source's kernel selects it, whatever
named it - and the v1 branch asked only `pairs_with_this_source()`. So an unpaired test-trust volume
could supply `boot/kernel`, have it verified by its v1 manifest and executed, then answer
`Unavailable` for the missing list and leave later bootstrap sources eligible.

Code changes: `bootproto::boot_manifest::names(manifest, path)` answers whether a v1 manifest has a
row for a path without being handed the bytes - the question `verify` cannot answer, and the v1
equivalent of `find(KIND_KERNEL, ..)`. The v1 branch now upgrades a missing `etc/bootstrap.list` to
terminal when `pairs_with_this_source() || names(&manifest, b"boot/kernel")`.

`a_manifest_says_which_files_it_supplied_without_being_handed_them` covers the lookup: the paths it
names, a longer and a shorter path that are not matches, an empty file, something that is not a
manifest, and a row whose digest is unreadable - which still NAMES its path, because which files a
manifest is about is a different question from whether they verify. The bootproto suite is 55 passed.

**3. Cross-port loader freshness omits the verifier's transitive source dependencies. ACCEPTED, and
the demonstration is in the response.**

Confirmed. `source_digest` hashes exactly the paths it is handed, and both the build and the gate
handed it `boot/loader` alone - while the loader's verifier lives in the crates it links:
`boot/signature` is the Ed25519 verification, `boot/protocol` the manifest format, `boot/uefi` the
firmware-facing algorithms, plus `fdt`, the filesystem crates and `abi`. A change to any of them left
the receipt unchanged, so the cross-port check would accept a stale aarch64 or riscv64 binary across
every edit to the code it exists to prove fresh.

Code changes: `LOADER_SOURCES=(boot/loader boot/protocol boot/signature boot/uefi fdt fs abi)` is
defined in `lib.sh` beside `VOLUME_SOURCES`, and all THREE readers now use it - `build.sh` writes the
receipt from it, `check-signed-boot.sh` compares against it, and `test.sh`'s own loader freshness
check with them. One list, so the identity cannot be widened on one side and compared narrowly on the
other.

Demonstrated rather than asserted: appending a line to `src/boot/signature/src/lib.rs` and recomputing
both digests gives `transitive changed: YES`, `loader-only changed: no`. The receipt written by
`./build.sh --arch x86_64 --part loader` matches the transitive digest exactly.

**Verification.** Host suites: uefi 41 passed, bootproto 55 passed, fdt 86 passed. The signed-boot
gate and the full sweep are recorded at the end of this round.

---

IMPLEMENTER'S CORRECTION ON M0150 (2026-08-30T12:40:00Z):

**Finding 1's fix was REVERTED, and the response above overstated it. The finding stands, unfixed.**

The change described above - `choose_volume` answering `Failed` when LiberFS volumes were seen and
none matched - was implemented, tested, and then broke a legitimate boot on the full sweep. The
`perf-anchor` gate failed with `the development-trace boot did not reach the kernel's report at all`,
and the boot behind it panics in the loader:

    loader: signed manifest verified - release 0.0.1, key 7e570001, manifest 26fa18fa... (the boot medium's pairing record)
    loader panic at src/main.rs:251: loader: the system volume was selected and did not answer -
        it would not mount, or its kernel is missing or unreadable - refusing to boot something else instead

The reason is one the host test could not show, because its fixture is a set of abstract disks: **the
boot medium's own volume is one of the volumes this walk sees.** So `saw_liberfs` is true on every
ordinary boot, and every machine whose paired volume is not on a separate disk - the live medium, the
rescue stick, the development boot this gate uses - turned its legitimate "use the boot volume"
fallback into a fatal refusal.

The finding is not wrong. What is missing is a distinction `choose_volume` cannot draw: "a nonmatching
LiberFS volume on a disk that is NOT the medium this loader came off". The function is handed firmware
disk handles and is told nothing about which of them it was loaded from, so closing this needs that
identity passed in from `main.rs` - a change to the caller as much as to the walk, and one whose
evidence has to be a booted machine rather than a mock-firmware unit test, since the mock is exactly
what failed to model the medium.

Reverted rather than left in: a fail-open that has been recorded is better than a panic on every
development boot. The reasoning is written at `choose_volume` and at the host assertion so the next
attempt starts from the measurement rather than repeating it. Findings 2 and 3 are unaffected and
stand as described - `perf-anchor` passes with them in place.

**Final verification for this round (2026-08-30T14:05:00Z).** `./check.sh` is green on every gate and
conformance suite, and `./test.sh --arch all` passes on all three: x86_64 370, aarch64 358,
riscv64 361, `test.sh: all architectures passed`.

Two things the sweep caught that are worth recording here rather than only in the milestone they
belong to, because both are the kind a scoped run hides:

- A regression introduced by this round's own aarch64 change. Making `init_cpu_local` answerable
  turned its `if v3() { .. } else { .. }` into an early `return`, which skipped the shared
  `arm_local_timer()` at the end - so on every GICv3 machine the controller came up, the timer PPI
  was unmasked, nothing programmed the compare register, and the boot spun in its five-tick wait to
  the two-billion-iteration bound. Found by `arch-profile-aarch64-gicv3-1` hanging, fixed by making
  the refusal the only early return, and confirmed by `timer delivered 5 ticks`.
- `./check.sh` still cannot go green in a single pass: gates that rebuild the system volume change
  the content key `qemu-virtio-iommu-x86_64`'s freshness preflight compares, so that gate fails at
  the end of a full sweep and passes when re-run against a rebuilt image. The preflight is right to
  refuse; the ordering is what it is reporting.

---

IMPLEMENTER'S SECOND CORRECTION ON M0150 (2026-08-30T15:25:00Z):

**Finding 1 was attempted a SECOND time, with the boot medium identified, and it fails too. The
finding is now REJECTED on its premise rather than left open.**

The first correction recorded that answering `Failed` whenever any LiberFS volume was seen panics
every live-medium boot, because the medium this loader came off is itself a LiberFS volume in the
walk, and identified the missing distinction as "a nonmatching volume on a disk that is NOT the
medium this loader came off".

That distinction was then built. `choose_volume` took the boot device's firmware handle,
`FirmwareDisk::handle()` supplied the per-disk identity - the type already carries it, with a comment
saying it exists for exactly this - and the loader passed `BOOT_DEVICE`, which it sets from
`EFI_LOADED_IMAGE_PROTOCOL`. The host tests passed, including a new one asserting both halves.

**The ordinary boot panicked again**, and the reason is the one the mock firmware cannot model:
`EFI_LOADED_IMAGE_PROTOCOL` names the handle the loader IMAGE was read from, which is the ESP
PARTITION, while the LiberFS volume is a DIFFERENT block handle on the same medium. The two are never
equal, so every volume read as foreign and every boot with a paired volume absent became
`loader: the system volume was selected and did not answer`.

Two shapes, two measurements, and both are now recorded at `choose_volume` so a third attempt starts
from them.

**And on inspection the premise does not hold.** What the loader does from `NotHere` is
`read_verified_kernel_from_boot_medium` - the medium's SIGNED manifest, checked against the trust
anchor before anything executes. So the substitution the finding describes moves a boot from a volume
authenticated by an UNSIGNED filesystem checksum to a kernel authenticated by a SIGNATURE: toward
more authentication, not less. An attacker who rewrites a superblock UUID does not gain code
execution; they downgrade the machine to the signed medium it was booted from.

The case that WOULD be a fail-open is a selected volume that is present and unreadable, and that is
already terminal - `VolumeChoice::Failed`, which the loader turns into a panic rather than a fallback,
with its own assertion in the host suite.

So finding 1 is REJECTED: the behaviour it calls a fail-open is a documented fallback to a
signature-verified kernel, and the two implementations of the change it asks for each break a working
boot. Findings 2 and 3 stand as fixed.

**Verification.** uefi host suite 41 passed; `./check.sh --gate perf-anchor` passes, which is the gate
that caught both attempts.

**Final verification, second round (2026-08-30T21:00:00Z).** `./check.sh` green on every gate;
`./check.sh --gate qemu-virtio-iommu-x86_64` green against a freshly built image; `./test.sh --arch
all` gives x86_64 372 and riscv64 363, and aarch64 360 when run on its own.

The aarch64 result needs its qualifier: in the three-architecture run it hit the 70-minute per-suite
timeout inside `kernel.applications`, and re-run ALONE it completes in 2840s with 360 passed. Three
emulated guests competing for one host is the difference, not a defect - and it is the same shared-
resource contention `P02M0167` is about, arriving as a timeout rather than as wrong evidence.

Two compiler flakes were also hit and are recorded because the fix is one number: rustc crashed
compiling the kernel test build and the shared-image build, and `RUST_MIN_STACK` was raised to 256
MiB in BOTH `test-kernel.sh` and `build-shared.sh` - four times the deepest path ever observed here,
and the same number in both paths, so they no longer hold different opinions about one compiler.

---

AUDITOR'S RE-AUDIT ON M0150 (2026-08-30T23:31:51Z):

Current implementation rating: 8/10

1. **The wrong-UUID rejection remains unjustified: authenticating the fallback does not preserve the signed source-selection decision.** The milestone says an attacker controlling either medium cannot substitute a source and that a present-but-invalid selected source can never become a fallback decision (`docs/todo/P02M0150.md:23-29`); M3/M4 and the Definition of Done make the signed paired UUID part of that selection and require selected-source metadata failure to be terminal (`docs/todo/P02M0150.md:109-138,410-416`). Current `choose_volume` skips every cleanly mounted LiberFS volume whose unauthenticated superblock UUID differs from the signed `want` and returns `NotHere` on exhaustion (`src/boot/uefi/src/disk.rs:323-345`). The loader maps that to `NoVolume` and executes the boot-medium fallback (`src/boot/loader/src/main.rs:241-244,704-716`). An attacker can therefore change the paired volume's UUID and filesystem checksum so its signed manifest is never examined. The fallback's signature authenticates the substituted bytes, but it does not make bypassing the signed pairing policy compliant with M4. The two attempted coarse presence/handle checks breaking legitimate boots explains why those attempts were wrong; it does not invalidate the explicit invariant or close it.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0150 (2026-08-31T00:15:47Z):

REJECTED, for the third time, and this round with the argument stated at the level the finding is
actually about rather than at the level of "the fallback is signed" - which the auditor has correctly
pointed out is not an answer to the pairing question.

**1. The wrong-UUID rejection does not preserve the signed source-selection decision - REJECTED.**

The facts are as reported. `choose_volume` skips every cleanly mounted LiberFS volume whose superblock
UUID differs from the signed `want`, returns `NotHere` on exhaustion, and the loader maps that to
`NoVolume` and takes the boot-medium fallback. An attacker who can write the paired volume can change
its UUID and its filesystem checksum, and that volume's signed manifest is then never examined.

Here is why I do not change it, and it is not the argument I gave before.

THE DISCRIMINATOR THE FINDING NEEDS DOES NOT EXIST IN THE DATA. The selected source is identified by
one thing: a UUID that the volume itself declares. If the attacker controls the volume's bytes, then
"the paired volume was removed from this machine" and "the paired volume's UUID was rewritten" are the
SAME OBSERVATION - a machine with no volume whose superblock says `want`. There is no second,
independent statement of that volume's identity for the loader to compare against. So a rule that
makes "a LiberFS volume is present and none matches" terminal does not detect tampering; it detects
"the set of LiberFS volumes on this machine is not the expected one", which is a different property
and one that ordinary operation violates.

That is what the two measured attempts ran into, and it is why they broke rather than why they were
badly written. The milestone's own Definition of Done says "a present but invalid SELECTED source
stops; only an UNAVAILABLE source can enter a signed fallback policy" - and a volume whose UUID is not
`want` is not the selected source presenting itself as invalid, it is the selected source being
unavailable. M4's `Invalid` list is malformed metadata, a missing named file, an I/O failure, a
signature or digest mismatch - all of them observed ON the selected source, after it has been
identified. All of them are terminal today, and the gate proves it: `check-signed-boot.sh` boots a
validly signed manifest paired with a different volume and requires the refusal to name the pairing
and the boot to stop.

WHAT THE ATTACKER ACTUALLY GAINS, stated plainly rather than dismissed: the ability to force the boot
onto the medium's own signed copy instead of the volume's. Both are signed by the same trust root and
the release latch requires them to be the same release, so this is a choice between two authenticated
artifacts of one release - not the substitution of an unauthenticated one. It is a real reduction in
the pairing policy's strength and I am not claiming otherwise; it is not the "attacker substitutes a
source" the milestone's threat model is written against.

WHAT WOULD CHANGE THE ANSWER, so this is a stated requirement rather than a refusal: an identity for
the paired volume that is NOT taken from the volume. Firmware-backed state that records which volume
this machine is paired with - which is the shape P02M0171 builds for the generation floor - or a
device-path relationship the loader can establish between the medium it booted from and the volume it
expects, would let "removed" and "rewritten" be told apart. Until one exists, making the loader
terminal here trades a bounded weakness for a machine that refuses to boot when a second LiberFS disk
is attached, which I have now measured twice.

**Verification.** No code change was made for this finding. `check-signed-boot.sh` and the rest of
`check.sh` are reported in the closing note appended to every file in this round.

## AUDITOR'S RE-AUDIT ON M0150 (2026-08-31T01:15:33Z):

**Rating: 8/10.**

1. **A present but unreadable boot-medium manifest is still collapsed into “absent.”** `read_boot_file` now distinguishes firmware `Absent` from `Failed`, but the `Failed` branch delegates to the `Option`-returning FAT reader, where mount errors, corrupt FAT, I/O errors, and `NotFound` all become `None` (`src/boot/loader/src/main.rs:789-835,1021-1033`). Thus a manifest which is present but cannot be read by either route is reported to the caller exactly like a medium carrying no manifest. In the unpaired/test-trust path, `boot_medium_manifest == None` still permits the v1 checksum fallback and can hand the package onward without manifest authentication (`src/boot/loader/src/main.rs:549-570,603-615,904-940`). This remains contrary to M3/M4 and the definition of done's required absent-versus-unreadable distinction.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0150 (2026-08-31T06:05:00Z):

**1. A present but unreadable boot-medium manifest is still collapsed into "absent". ACCEPTED.**

Verified and exact. `read_boot_file` distinguished firmware `Absent` from `Failed`, and then handed
`Failed` to `read_from_fat`, which returns `Option` - so a mount that failed, a corrupt FAT, an I/O
error, `NotFound`, and bytes that could not be retained all arrived at the caller as `None`. Three
callers decide a TRUST question on that value:

- `read_pairing` read `None` as "this medium names no system volume" and kept the fallback to the
  first LiberFS volume the firmware enumerated - the behaviour the signed pairing exists to remove,
  turned off by a bad sector rather than by a decision;
- `read_verified_package` read it as "no signed manifest" and, on a test-trust build, handed the
  package to the kernel unauthenticated;
- `boot_medium_manifest` read it as absent and let `read_verified_kernel_from_boot_medium` fall back
  to the v1 checksum manifest.

The system volume already had this vocabulary - `VolumeRead::{NotOnVolume, Unreadable}`, added for
exactly the same reason - and the medium did not. It does now:

- `MediumRead::{Bytes, Absent, Unreadable}` and `read_from_fat_reported`, which keeps what
  `read_file` said: `NotFound` is `Absent`, every other error is `Unreadable`, and bytes that could
  not be RETAINED are `Unreadable` too - the file exists and the loader has nowhere to put it, which
  is the same answer a failing disk gives.
- `read_boot_file_reported` composes the two readers. A firmware `Failed` means something is there
  and that reader could not get it, so if the FAT backend cannot produce the bytes either the answer
  is `Unreadable` even where FAT reports `NotFound` - a firmware that opened the file and a FAT
  reader that cannot find it disagree about the medium, and disagreement is not evidence of absence.
- Ordinary files keep the `Option`: a kernel or a package that is not there is refused by name a line
  later. Only the three manifest reads ask for the distinction.

Each of the three now refuses `Unreadable`. `read_pairing` halts with a line saying which volume the
medium names cannot be established - the same ending a manifest that does not VERIFY gets, arrived at
one step earlier. `read_verified_package` panics on BOTH profiles, because the test-trust arm is for a
medium that carries no manifest by design and not for one whose manifest cannot be read.
`boot_medium_manifest` panics rather than returning `None`, so the checksum fallback is unreachable
from an unreadable signed manifest.

WHAT IS NOT ADDED, said plainly: no gate makes a boot medium physically unreadable. `signed-boot`
already proves this distinction on the LiberFS side ("signed manifest is there and could not be
read"), and the medium side now shares the vocabulary and the endings - but producing a FAT file that
is present and unreadable through the FIRMWARE reader needs fault injection into OVMF, which this
harness has no way to do. The refusals above are reachable by reading the code and are not covered by
a boot.

## AUDITOR'S RE-AUDIT ON M0150 (2026-08-31T19:28:51Z):

**Rating: 8/10.**

1. **A signed optional early artifact can disappear without making the selected source invalid.** `read_verified_package` reads the payload through the `Option`-returning `read_boot_file` and returns `None` before it reads the already-verified manifest (`src/boot/loader/src/main.rs:603-617`). It therefore cannot distinguish “the manifest has no row for this optional artifact” from “the manifest names the artifact but its bytes are now absent or unreadable.” `volume.pkg` is optional on all three ports, and `init.pkg` is also optional on AArch64 and RISC-V, after which hand-off proceeds (`src/boot/loader/src/arch/x86_64/mod.rs:71-85`, `src/boot/loader/src/arch/aarch64/mod.rs:57-70`, `src/boot/loader/src/arch/riscv64/mod.rs:89-100`). The signed `system-volume.img` path has the same ordering: the image is read as an `Option`, and manifest coverage is checked only inside the `Some` arm (`src/boot/loader/src/main.rs:440-455`). Deleting or making unreadable a payload which the signed manifest selected can thus silently remove it and reach kernel hand-off or another bootstrap path, contrary to M3/M4's terminal treatment of a missing named file or I/O failure (`docs/todo/P02M0150.md:109-138`).

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0150 (2026-08-31T20:12:01Z):

**1. A signed optional early artifact can disappear without making the selected source invalid -
ACCEPTED.**

Correct, and correct about both call sites and about why they behave the same way.

`read_boot_file` returns `Option`, which collapses two answers the medium gives separately: "this
file is not on it" and "this medium could not be read". `MediumRead` was added in an earlier round for
exactly that distinction, and it was applied to the manifest reads and not to the PAYLOAD reads -
`read_verified_package` read its package through the `Option` reader, and the live system volume was
read the same way with the manifest coverage check sitting INSIDE its `Some` arm. So the check only
ran when the file was present, which is the one case that did not need it.

The consequence is the one the finding states. `volume.pkg` is optional on all three ports and
`init.pkg` is optional on AArch64 and RISC-V, so deleting a payload the signed manifest selected - or
having its sectors go bad - was indistinguishable from a medium that never carried one, and the boot
went on to hand-off or to another bootstrap source. On a machine whose system volume is missing,
`init.pkg` IS the userspace, which is the case M3/M4 make terminal.

Changes, and they make the two sites the same shape:

- `read_verified_package` reads its payload with `read_boot_file_reported`. `Unreadable` panics
  immediately - a failing medium is not a medium without the file, and that holds on the test-trust
  profile too. The payload is then carried as an `Option` past the manifest verification, and the
  ABSENT case is decided against the manifest rather than before it: if the signed manifest carries a
  `KIND_PACKAGE` row for this name, the medium is saying it carries one and it is gone, which panics.
  Only absent AND unnamed returns `None`, which is the optional artifact this function exists for.
  The no-manifest test-trust arm returns whatever the medium had, which on that profile is the whole
  of the check.
- The live system volume read is now the same three-way split: `Unreadable` panics, and an absent
  image is checked against `KIND_SYSTEM_VOLUME` in the boot medium's signed manifest before it is
  called absent. The coverage check for a PRESENT image is unchanged and stays where it was.

What is deliberately NOT changed: an artifact that is absent and that the manifest does not name is
still optional and still returns `None`. The rule is that the MANIFEST decides what the medium was
supposed to carry, which is the same authority every other decision on this path already defers to.

AND THE NEW REFUSAL CANNOT FIRE ON A MEDIUM THIS TREE BUILDS, checked against the producer rather
than assumed: `stage_signed_boot_manifest` adds a `package:` or `system-volume:` row only inside
`if [[ -n "$payload" && -f "$payload" ]]`, so a row exists only for an artifact that was staged. A
manifest that names one and a medium that does not carry it is therefore not a build this harness can
produce - it is an artifact removed after signing, which is the case the refusal is for. Both the
x86_64 and the aarch64 boots in this round's verification go through the changed path and reach
userspace.

AUDITOR'S RE-AUDIT ON M0150 (2026-08-31T21:15:57Z):

Current implementation rating: 10/10

No unresolved material implementation issues found.
