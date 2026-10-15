AUDITOR'S REVIEW OF PLAN P02M0203 (2026-10-15T02:53:13Z):

**Rating: 4/10.** The decision is right and the milestone is small: the bootstrap staging is a fact about the driver, and the "no operator policy" rule is a fact about one device. But the plan cannot be implemented as written. Its central item marks "the binding StorageService chose", and nothing in the tree tells DeviceManager which one that is. Its stored-policy item and its oracle assume a boot-time behaviour that no driver has today.

Reviewed [the plan](/data/yellow/libersystem/docs/todo/P02M0203.md) and the decision it records, against the tree at HEAD 26faf58d. In the tree I read:
- DeviceManager's phase one, phase two, policy decision and stored-policy loading;
- ServiceManager's bring-up loop and its block hand-off;
- StorageService's system-volume choice and classification report;
- the device and config IDLs, `lsdev`, the manifest rows of the three drivers, and the binding library with its tests.

1. **High - DeviceManager cannot learn which binding holds `vol://system`, and the plan has no item that tells it.**

   [Item 2](/data/yellow/libersystem/docs/todo/P02M0203.md:29) has DeviceManager mark "the ONE binding whose block provider StorageService chose as `vol://system`". The choice is made, and kept, inside StorageService:
   - StorageService picks the volume in [`mount_by_uuid`](/data/yellow/libersystem/src/user/services/storage/src/service.rs:4182) by the loader's pairing uuid.
   - What leaves the service is only a per-probe format table behind its online report ([report](/data/yellow/libersystem/src/user/services/storage/src/service.rs:256)). The served probe is entered there as `FORMAT_LIBERFS` ([line](/data/yellow/libersystem/src/user/services/storage/src/service.rs:284)).
   - So is every other disk that mounts as LiberFS ([`classify_block`](/data/yellow/libersystem/src/user/services/storage/src/service.rs:4131)), for example the unpaired decoy that `boot-volume-controllers` puts beside the system disk. The table therefore does not identify the system disk.
   - ServiceManager reads that table only to route the FAT, ISO 9660 and UDF roles ([routing](/data/yellow/libersystem/src/user/services/core/src/service_manager/bootstrap.rs:1081)). It sends nothing about the choice to DeviceManager.
   - DeviceManager knows the order in which it minted the probes ([`sort_probe_slots`](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:1423)), but not which probe was chosen.

   Without this, item 2 can only guess (the first LiberFS disk), which is exactly the kind of choice the pairing uuid replaced.

   **Correct item 2** by adding the path:
   - StorageService reports the index of the probe it serves explicitly, not as a format.
   - ServiceManager relays that index to DeviceManager on its control channel, as a tagged message in the standing loop as `POLICYCFG` is.
   - DeviceManager maps the index to the node through the same probe order.
   - State the safe default: until that message arrives, and if it never does, every boot-capable binding keeps refusing, which is today's behaviour.
   - Add the decoy to the oracle as an ordinary disk. It is `FORMAT_LIBERFS` too, so it proves the mark comes from the choice and not from the format.

2. **High - Item 3 and the oracle describe a boot-time stored-policy behaviour that no driver has, and the plan does not say it is changing it.**

   DeviceManager reads stored policy in one place only: when its owner ConfigService connection arrives as `POLICYCFG` ([arrival](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:1209)). The order at boot is:
   - ServiceManager sends `POLICYCFG` after its bring-up loop has finished ([send](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1345)). Its comment says why: "The policy is about the NEXT bind in any case" ([comment](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1349)).
   - Phase two binds every volume-staged driver inside that loop: `DRIVERS` is sent when StorageService comes up ([send](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1049), [receipt](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:1084)).
   - So every driver bound at boot, in phase one or phase two, is already bound when its stored policy is read.
   - `load_stored_policy` leaves such a node running on purpose: "is disabled in stored policy and is already bound; it stays up and will not bind again" ([line](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:6584)).
   - `begin_bind` then refuses every later bind of it ([check](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:4487)).

   Today a stored disable on an ordinary device therefore means "not bound again". It does not mean "stopped at boot". This undermines two parts of the plan:
   - **The WHY.** It names the price as "a stored policy for it is never read". The difference that actually exists is that the live verbs are refused ([`decide_policy`](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:6469)).
   - **[Item 3](/data/yellow/libersystem/docs/todo/P02M0203.md:33) and [the oracle](/data/yellow/libersystem/docs/todo/P02M0203.md:39).** "Stopped right after" the mount, and "survives a reboot (the disk is stopped after the mount)", introduce teardown at policy load. That is either a rule for three drivers only, or an unplanned change for every driver.

   The teardown also has a consumer the plan does not mention. Before `POLICYCFG`:
   - the data disk was handed to StorageService as a probe;
   - if it carries FAT, ISO 9660 or UDF, it was handed to the media, ISO or UDF storage instance ([routing](/data/yellow/libersystem/src/user/services/core/src/service_manager/bootstrap.rs:1081)), and those instances are `escalate` services that are not restarted.

   Stopping the disk at that point removes a mounted volume from a service that will not come back.

   **Correct item 3 and the oracle** by choosing one of two rules and stating it:
   - **(a) The tree's rule.** After a reboot the data disk is bound and stays up. `lsdev` shows its stored disable, and it is not bound again after a crash or a retry. The live `--disable` tears it down at once. The oracle checks exactly that, for NVMe, AHCI and a second virtio-blk disk alike.
   - **(b) A new system-wide rule.** "A stored disable stops a bound device when the policy is loaded", applied to every driver. State what happens to consumers that already hold the device (mounted volumes first), and include an ordinary, non-storage driver in the oracle.

   Either way, a rule special to boot-capable storage drivers is the inconsistency to avoid.

3. **Medium - The flag's new meaning, the identity of the mark and the refusal's reason are left open, and each touches code the plan does not list.**

   - **The flag.** [Item 1](/data/yellow/libersystem/docs/todo/P02M0203.md:26) says the manifest's meaning is split, but not what happens to `lifecycle = "boot-critical"` ([ahci](/data/yellow/libersystem/src/user/services/manifest.toml:825), [nvme](/data/yellow/libersystem/src/user/services/manifest.toml:2125), [virtio_blk](/data/yellow/libersystem/src/user/services/manifest.toml:3098)).
     - **What keeps it:** the staging rules ([system-manifest](/data/yellow/libersystem/src/tools/system-manifest/src/lib.rs:1915)), the generated registry field ([build.rs](/data/yellow/libersystem/src/user/services/core/build.rs:50)) and phase-one selection ([line](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:1297)).
     - **What must stop reading it:** `decide_policy` ([line](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:6469)) and `decide_retry` ([line](/data/yellow/libersystem/src/user/libs/driver/binding/src/lib.rs:1018)).
     - **Text that becomes wrong:** the phase-two summary that counts every boot-critical node as online ([line](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:1547)) is false once a data disk can be disabled. The manifest comments state the class-wide cost ([ahci](/data/yellow/libersystem/src/user/services/manifest.toml:814), and the same text on `nvme`).
     - **To decide:** whether the value is renamed (to the "boot-capable" of the title) or kept with a narrower definition.
   - **The mark belongs to the device, not to a binding.** A `BindingId` carries a generation ([type](/data/yellow/libersystem/src/user/libs/driver/binding/src/lib.rs:802)). The volume's driver is re-bound from the init package when it ends ([fallback](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:1751)). A mark held on the binding would be lost with that rebind, and the system disk would become one the operator can disable. Item 2 should mark the node, that is the PCI or platform function.
   - **The reason is part of the wire.** `policy-outcome.refused` is documented as "a boot-critical binding, whose policy would live on a volume that is not mounted" ([IDL](/data/yellow/libersystem/src/idl/device.lsidl:456)). `lsdev` prints that sentence ([text](/data/yellow/libersystem/src/user/apps/tools/src/lsdev.rs:184)), and a host test asserts the class rule ([test](/data/yellow/libersystem/src/user/libs/driver/binding/src/tests.rs:1051)). Item 2 needs either a redefined `refused` or a new outcome, plus the `lsdev` text and the test.

   **Correct items 1 and 2** by:
   - listing what keeps the flag and what moves to the node's mark;
   - naming the outcome change;
   - fixing the phase-two summary and the manifest comments in the same change.

What holds: keeping NVMe, AHCI and virtio-blk in the bootstrap set is the right call. The staged sizes confirm the order of magnitude:
- x86_64: about 325 KB for NVMe and AHCI together;
- aarch64: about 525 KB;
- riscv64: about 230 KB.

The init-package fallback already makes a pinned driver re-bindable after the mount. And `boot-volume-controllers` already boots two disks with a persistent disk across cold boots, so the oracle is feasible once findings 1 and 2 decide what it checks.

Validation: read-only inspection of the plan and of the sources linked above. No plan or source file was modified, and nothing was built or booted.


PLANNER'S RESPONSE ON P02M0203 (2026-10-15T03:09:02Z):

Every finding was checked against the tree at 26faf58d before it was acted on. The plan was rewritten whole. Items 2-5 and the rule "WHAT DOES NOT CHANGE" are new; item 1 is narrowed to a decided design.

1. **ACCEPTED - DeviceManager cannot learn which binding holds `vol://system`.**

   Confirmed:
   - StorageService picks the volume inside `mount_by_uuid`.
   - It enters the served probe as `FORMAT_LIBERFS` at `service.rs:284`, and `classify_block` gives every other mountable LiberFS disk the same code.
   - ServiceManager uses the table only to route the media roles and sends DeviceManager nothing.

   Plan changes:
   - **Item 2:** StorageService enters the disk it serves as a sixth code, `FORMAT_SERVED`. `parse_classification_report` accepts it, and the role routing treats it as LiberFS.
   - **Item 3:** ServiceManager relays the index to DeviceManager as a tagged `SYSVOL` message on its control channel, read in the loop as `POLICYCFG` and `ACPI` are. DeviceManager records, while it mints the probes, which node each probe is for, in `sort_probe_slots` order, and maps the index to the node with that record.
   - **Item 4:** the safe default (every bootstrap node holds the volume until `SYSVOL`, and kernel fixtures without probes keep it).
   - **Item 7:** the decoy is used as the ordinary disk.

   A code in the existing table was chosen over a new message from StorageService. The table already travels behind the report in probe order, so no new message order is introduced.

2. **ACCEPTED - the stored-policy item and the oracle described a boot-time behaviour no driver has.**

   Confirmed:
   - `POLICYCFG` is sent after the bring-up loop (`service_manager.rs:1345`).
   - `DRIVERS` is sent inside it, when StorageService is `Ready` (`service_manager.rs:1049`).
   - `load_stored_policy` leaves a bound node up (`device_manager.rs:6584`).
   - `begin_bind` refuses the later binds (`device_manager.rs:4487`).

   I took the audit's rule (a), the tree's own rule. Rule (b), stopping a device at boot when its stored disable is read, would change every driver's boot behaviour. It would also pull mounted volumes from `escalate` storage instances that are never restarted. That is not this milestone's question.

   Plan changes:
   - **WHY rewritten.** The incorrect "a stored policy for it is never read" is gone. The missing identification is stated as the second reason the tree cannot do better.
   - **A new section "WHAT DOES NOT CHANGE: WHEN A STORED POLICY TAKES EFFECT"** states the rule for every driver, and that it applies to the data disks too.
   - **A new section "NOT IN THIS MILESTONE"** names the system-wide question and leaves it to the owner. It is put to him separately.
   - **Item 4** says stored policy applies to other bootstrap nodes "under the rule above".
   - **The oracle (item 7) now checks the real rule:**
     - the decoy is disabled live and its provider is gone;
     - after a cold boot it is bound in phase one, and DeviceManager says it is disabled in stored policy and stays up;
     - `lsdev` shows the stored disable;
     - `--enable` removes it.

3. **ACCEPTED IN PART - the flag's meaning, the identity of the mark, and the refusal's reason.**

   - **The flag: ACCEPTED, decided.**
     - Confirmed: `DriverLifecycle` documents what a driver binds to, and `system-manifest` uses the lifecycle in only two rules, both about `pinned` (`lib.rs:1914-1917`). The generated registry carries only the derived boolean (`build.rs:50`). All three pinned drivers provide `block`.
     - Item 1 now removes `boot-critical` from the enum. The three drivers declare `lifecycle = "function"` and keep `stage = "pinned"`.
     - The two validator rules become one: a pinned driver must provide `block`. Volume-staged block providers stay allowed; the manifest has three: `virtio_scsi`, `sdhci` and `xhci`.
     - The generated flag becomes `bootstrap`, derived from the stage. Item 1 lists every consumer of the old name to change with it.
   - **The phase-two summary sub-point: REJECTED.** Under rule (a) the summary at `device_manager.rs:1547` is true when it is printed. Every bootstrap node is bound at that point, whatever its stored policy says. It only follows the rename.
   - **The mark on the node: ACCEPTED (item 4).** It holds across every rebind, because a `BindingId` carries a generation.
   - **The wire: ACCEPTED (item 5).**
     - `policy-outcome.refused` is redefined in `device.lsidl`;
     - `lsdev` prints the new sentence;
     - the generated docs follow;
     - the binding library's test of the class rule becomes item 6's tests.

Re-check after the changes. The plan now:
- names how the volume's device is identified, from StorageService's choice to DeviceManager's node;
- keeps the safe default when the identification never arrives;
- keeps one stored-policy rule for every driver and says so;
- decides the manifest's shape and lists its consumers;
- names the wire change;
- has an oracle that can fail on each of these (the decoy shares the system disk's format, so a mark taken from the format would refuse the wrong row).

Its oracle runs in QEMU through an existing gate. Nothing in it depends on P02M0204, which refers to item 4 for the volume's device. It is ready for implementation once the owner says so.
