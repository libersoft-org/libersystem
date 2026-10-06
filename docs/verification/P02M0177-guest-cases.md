# P02M0177 guest case inventory

The launchers and predicates below cover EVERY `GATES_THAT_BOOT_A_GUEST`, EVERY `PROFILE_ROW_GATES`
row, and `concurrent-selection` in `verify-model/src/catalog.rs`, and the gate that reads this file
checks that by name rather than by count.

(It said "all nine" and "all sixteen", and both numbers were the ones those lists held when this file
was written. The DMA-mode rows, the rollback rows, the evidence gate and the twelve virtio-iommu port
rows were added to the catalog afterwards and named nowhere here, so a file whose whole purpose is to
say what each guest gate boots and what proves it had been silently describing a subset. Counting in
prose is what let that happen: the count agreed with nothing, so nothing disagreed with it. Corrected
2026-09-14, and the two numbers are gone rather than updated.) Umbrella gates
run the listed cases serially; only `verify.sh` schedules separate steps. Each case keeps its existing
post-run assertions. `src/tools/guest-verdict.py` now supplies the named early termination predicates;
its `verdict` function is also the production seam exercised by negative fixtures.

A loader refusal ends at the panic handler's final refusal (the handler prints and hangs), or at
`read_pairing`'s exact final `loader: FATAL ... signed manifest ... cannot be established` line,
which immediately calls `arch::halt`. The earlier verifier reason alone is insufficient. A
bootstrap refusal is latched, so its earlier reason must be followed by `refusing to hand off`.
`loader: kernel loaded` is sufficient only for cases whose entire claim is kernel loading; it is
an intermediate signal in the altered-volume, pairing and missing-bootstrap-list cases.

## Signed boot: sixteen boots

All x86_64 cases launch `qemu-system-x86_64` with run-private ESP, serial log and OVMF variable
store through `check-signed-boot.sh`'s `boot_medium` or its attached-volume variants. Their timeout
backstop remains 120 seconds. Loader-only cases have no guest-health observation interval: after
the listed final signal no required assertion remains. Port cases launch `run.sh --arch PORT --smp 1`
with `UEFI=1` and run-private serial logs; their timeout backstop remains 300 seconds.

| Case / watcher predicate | Final signal | Required refusal/failure checks |
| --- | --- | --- |
| Unaltered medium / `signed-clean` | `loader: kernel loaded` | A positive control must load; no attached disk also covers an absent source. |
| Altered signed manifest / `signed-manifest` | Manifest refusal reason and final loader panic/FATAL halt | No kernel loaded; the mutation's digest must differ. |
| Altered embedded system volume / `signed-payload` | Exact live-system-volume manifest mismatch and loader panic | Do not stop at kernel loaded; payload digest must differ. No kernel handoff. |
| Damaged manifest on selected volume / `signed-selected-volume` | Selected signed-manifest refusal and loader panic | No kernel loaded; medium must name the selected disk's UUID. |
| Wrong product / `signed-context` | Context refusal and final loader panic/FATAL halt | Correctly signed wrong-product manifest; no kernel loaded. |
| Wrong architecture / `signed-context` | Context refusal and final loader panic/FATAL halt | Correctly signed wrong-architecture manifest; no kernel loaded. |
| Wrong source kind / `signed-context` | Context refusal and final loader panic/FATAL halt | Correctly signed wrong-kind manifest; no kernel loaded. |
| Wrong paired volume / `signed-volume-pairing` | Exact different-volume reason and `refusing to hand off` | No `LiberSystem kernel is starting`; preserve both original assertions. |
| Selected volume without bootstrap list / `signed-absent-list` | Exact missing-list reason and `refusing to hand off` | No kernel started; sign for the fixture's UUID; original shared image/UUID/stamp bytes and mtimes remain unchanged. |
| Mixed releases / `signed-mixed-release` | Exact different-release reason and loader panic | Re-signed medium and selected volume remain separate valid releases; no kernel handoff. |
| Missing signed manifest, test trust / `signed-downgrade-test` | Unauthenticated-kernel warning and kernel loaded | Warning cannot substitute for the successful fallback it describes. |
| Missing signed manifest, external release / `signed-downgrade-release` | Exact missing-signed-manifest refusal and loader panic | No kernel loaded. |
| aarch64 unaltered / `signed-port-clean` | Kernel loaded | Current loader receipt and positive control required. |
| aarch64 altered / `signed-port-manifest` | Manifest refusal and final loader panic/FATAL halt | No kernel loaded. |
| riscv64 unaltered / `signed-port-clean` | Kernel loaded | Current loader receipt and positive control required. |
| riscv64 altered / `signed-port-manifest` | Manifest refusal and final loader panic/FATAL halt | No kernel loaded. |

## Secure boot, performance anchor and IOMMU

| Gate and case / watcher predicate | Launcher and final signal | Observation, failures and timeout backstop |
| --- | --- | --- |
| `secure-boot`: signed loader / `secure-signed` | `check-secure-boot.sh:boot`, enforcing OVMF; firmware enforcing line | Loader-only, no extra interval; 120 s. Independent host signature verification retained. |
| `secure-boot`: unsigned loader / `secure-unsigned` | Same launcher; no positive serial oracle | Retain full 120 s calibrated silence backstop. Any loader trust banner fails; early QEMU exit cannot prove firmware refusal. |
| `secure-boot`: altered signed loader / `secure-altered-loader` | Same launcher; no positive serial oracle | Retain full 120 s calibrated silence backstop and absence of loader trust banners. |
| `secure-boot`: altered manifest / `secure-altered-manifest` | Same launcher; manifest refusal and final loader panic/FATAL halt | Loader-only, 120 s; kernel loaded fails. |
| `perf-anchor`: development-trace / `perf-trace` | `qemu-run.sh x86_64`; boot report and `PERF tsc_hz` | Report-only claim, no later userspace assertion; 90 s outer backstop. |
| `perf-anchor`: development / `perf-plain` | Same launcher; boot report | PERF anchor forbidden through final report; 90 s. |
| `qemu-virtio-iommu-x86_64`: DMA fixture | `test.sh --arch x86_64 --tags dma` with dedicated enforcing devices | Suite completion/debug-exit (shared suite predicate below); translating bypass-off line, all five hostile-case outcomes, no absent/skipped fixture. |
| IOMMU ordinary translated network / `iommu-traffic` | Direct QEMU shipping ISO; controller translating, endpoint attached, virtio-net online and DHCP acquired | Preserve entire 300 s window/backstop; panic, loader fatal, reset or early guest exit fails. |
| IOMMU default machine / `iommu-default` | `run.sh --smp 4`; translated summary, GPU online, displayed frame | Preserve entire 120 s window/backstop. Reject later panic/fatal, reset/reboot (multiple banners), degraded isolation, retracted isolation, IOMMU fault, GPU restart/repeated online, failed frame and early exit. All existing shell assertions remain. |
| IOMMU explicit no-IOMMU / `iommu-plain` | `run.sh --no-iommu --smp 4`; controller absent and degraded isolation | Preserve entire 120 s window/backstop; panic/fatal, reset/reboot or early guest exit fails. |

## Shared kernel-suite predicate

`test.sh` calls `test-kernel.sh`, which compiles and copies under `kernel-test-build.lock`, then
starts `qemu-run.sh` on that immutable kernel. The terminal predicate is the complete suite plus
the guest debug-exit success status. Test failure, unknown selection, missing completion, reset,
wall timeout and stalled progress remain failures. There is no extra post-completion interval:
the suite intentionally exits when its last assertion is complete. Both run-owned result logs are
used; riscv64 suite output may be in the run log.

Unchanged full/tag backstops are x86_64 15/3 minutes, aarch64 70/45 minutes and riscv64 90/45
minutes. The progress windows are 900, 2400 and 2400 seconds respectively. Named selections use
the existing harness's chosen timeout; no profile adds a shorter bound. `--build-only` stops after
the locked compile and copy and never starts this predicate or a guest.

## The architecture and NUMA profile rows

`qemu-arch-profiles` is the umbrella for the first thirteen rows below, launched through
`check-qemu-arch-profiles.sh:run_profile`. `qemu-numa` is the umbrella for the last three,
through `check-qemu-numa.sh`. Every row uses the shared suite predicate and backstop above, with
its own selection and run-owned logs. Four-core interrupt rows additionally require the three
named remote-wake, shootdown and cross-core-scheduling oracles and every declared core online.
All interrupt rows require their controller identity and at least five delivered timer ticks.

| Catalog row | Profile-specific final assertions |
| --- | --- |
| `arch-profile-aarch64-gicv2-1` | Direct GICv2; MSI acquisition, delivery, binding and release. |
| `arch-profile-aarch64-gicv2-4` | Same, plus four-core oracles. |
| `arch-profile-aarch64-gicv3-1` | Direct GICv3 with ITS off; no MSI claim. |
| `arch-profile-aarch64-gicv3-4` | Same, plus four-core oracles. |
| `arch-profile-aarch64-gicv3-its-1` | Direct GICv3 ITS; MSI oracle. |
| `arch-profile-aarch64-gicv3-its-4` | Same, plus four-core oracles. |
| `arch-profile-aarch64-gicv3-its-device-4` | UEFI ITS, actual device-originated interrupt and teardown; four-core oracles. |
| `arch-profile-aarch64-uefi-1` | UEFI GICv2 and MSI; no static no-DT descriptor selection. |
| `arch-profile-riscv64-aia-1` | Direct IMSIC; MSI oracle. |
| `arch-profile-riscv64-aia-4` | Same, plus four-core oracles. |
| `arch-profile-riscv64-uefi-1` | UEFI IMSIC and MSI; no static no-DT descriptor selection. |
| `arch-profile-aarch64-no-dt-1` | Private treeless loader, matching compiled kernel authorization and named static controller; MSI oracle. |
| `arch-profile-riscv64-no-dt-1` | Same authorization and absence-of-DT contract for the riscv64 descriptor; MSI oracle. |
| `numa-profile-x86_64` | Two ACPI nodes with exact memory/CPU assignment and distances; all named allocation/placement tests, complete matrix and model trace; no weak placement. |
| `numa-profile-aarch64` | Same two-node placement claims through device tree on direct boot. |
| `numa-profile-riscv64` | Same two-node placement claims through device tree on direct boot. |

## The DMA-mode rows, and what each entry path proves

`dma-mode-x86_64`, `dma-mode-aarch64` and `dma-mode-riscv64` are the rows of `dma-mode-ports`, and
`dma-mode-carrier` is the producer gate beside them. WHAT THEY ARE ABOUT IS THE STATED MODE AND NOT
THE HARDWARE: one producer per entry path declares the boot's DMA mode, admission compares the
machine against that statement, and a degraded boot is required to SAY so rather than to look like an
enforcing one that happened to admit an endpoint untranslated.

| Gate / case | Launcher and final predicate | Observation and backstop |
| --- | --- | --- |
| `dma-mode-x86_64`: admits / `dma-admits` | Shipping ISO under the translating machine; the produced record says enforcing-required, every bus master is translated, and the driver the degraded row refuses by name comes online and passes traffic | Health-watched; panic, loader fatal, degraded isolation or an `iommu-required` refusal fails. |
| `dma-mode-x86_64`: degraded / `dma-degraded` | The same image with no controller; the record says no-iommu and the degraded profile is announced by name with every affected device listed | The `iommu-required` entry must be refused, not admitted quietly. |
| `dma-mode-x86_64`: signed pair / `dma-signed-admits`, `dma-signed-degraded` | The same two boots over a SIGNED medium, so the mode travels with a manifest a verifier accepted rather than with a command line | Same predicates; a mode that changed across the signature fails. |
| `dma-mode-x86_64`: refusals / `dma-loader-refused`, `dma-kernel-refused` | A machine whose statement and hardware disagree, refused at the loader and at the kernel respectively | Each must refuse at ITS OWN stage; refusing later is a different claim. |
| `dma-mode-aarch64`, `dma-mode-riscv64` | The port rows of the same subject through `run.sh`, on the reduced ordinary machine | See the port cases below. |
| `dma-mode-ports`: admits / `dma-port-admits`, `dma-port-admits-direct` | `run.sh --arch PORT` UEFI and a direct `-kernel` boot; enforcing-required, everything translated, and on the UEFI row the NIC online with a DHCP lease | The direct row promotes no root, so it asserts on the kernel's audit rather than on traffic. |
| `dma-mode-ports`: degraded / `dma-port-degraded`, `dma-port-degraded-direct` | The same two with `--no-iommu`; the degraded profile announced and the `iommu-required` entry refused | A degraded boot that admitted it untranslated fails. |
| `dma-mode-ports`: refusal / `dma-port-loader-refused` | A port boot whose statement the loader must refuse | Loader-only; no kernel handoff. |
| `dma-mode-carrier`: two producers / `dma-port-two-producers` | Two independent producers of the boot-policy node - the ESP file and the firmware device tree - which the loader must refuse rather than choose between | The refusal is the claim; a boot that picked one fails. |

## The rollback floor

`rollback-floor-x86_64` boots the shipping medium against a provisioned floor and requires the
answer to depend on the RECEIPT rather than on the image's own opinion of itself.

| Case | Final predicate | Failures |
| --- | --- | --- |
| `rollback-accepted` | A release at or above the stored floor boots | A refusal here would make every ordinary boot a rollback. |
| `rollback-refused` | A release below the floor is refused by name | Loading it anyway is the defect the floor exists for. |
| `rollback-unprovisioned` | A machine with no stored floor says the floor is not enforced and boots | Inventing a floor is as wrong as ignoring one. |
| `rollback-manifest-refused` | A floor record whose signature does not verify is refused | A floor nobody signed is not a floor. |
| `rollback-not-enforced` | A trust profile that does not enforce the floor says so | Silence would read as enforcement. |

## The virtio-iommu port rows

`iommu-ports` is the umbrella; each row is `<arch>:<profile>` and each PHASE of a row is a catalog
key of its own, because a single green row key binding hostile evidence to shipping claims is
exactly the shape this gate refuses. The hostile and direct-transition phases boot the TEST kernel
with the enforcing fixture; every other phase boots the built system through `run.sh`.

EVERY PHASE KEEPS ITS CENSUS - the kernel's own list of the bus masters it admitted, with their
addresses and translation - so a bus master added to a machine without a policy decision shows up as
a census difference rather than as a quiet pass. And every phase reports its BOOT WINDOW, because the
reduced machine exists precisely because an emulated port does not finish attach-and-map for a dozen
endpoints in DeviceManager's window, and any phase that adds an endpoint back is making a claim about
how long that takes.

| Catalog row | Launcher and final predicate | Observation and backstop |
| --- | --- | --- |
| `iommu-aarch64-direct-gicv2` | The umbrella for the two rows below. | Serial. |
| `iommu-aarch64-direct-gicv2-hostile` | `test.sh --arch aarch64 --tags dma` with the controller, virtio-net and two `edu` functions; the transition confirmed and the five hostile cases plus the forced release refused BY THE HARDWARE | Shared suite predicate; a case that reports itself absent or skipped fails. |
| `iommu-aarch64-direct-gicv2-ordinary` / `iommu-port-ordinary-direct` | `run.sh` on the reduced machine; translating with bypass read back off, every bus master translated, and the block driver online behind the controller | A direct boot promotes no root, so traffic is proved on the UEFI row. Degraded or untranslated admission fails. |
| `iommu-aarch64-direct-gicv3-its` | The umbrella for the two rows below, on the ITS interrupt path. | Serial. |
| `iommu-aarch64-direct-gicv3-its-transition` | The test kernel again, for the transition on the ITS path | Same predicates as the hostile row's transition half. |
| `iommu-aarch64-direct-gicv3-its-ordinary` / `iommu-port-ordinary-direct` | As the GICv2 ordinary row, on the ITS path. | Same. |
| `iommu-aarch64-uefi-gicv2` | The umbrella for the three rows below. | Serial. |
| `iommu-aarch64-uefi-gicv2-transition` / `iommu-port-transition` | The FULL machine through `run.sh` with AAVMF; every firmware-touched endpoint quiesced BY CLASS - virtio and xHCI - and bypass read back off before any driver mastered the bus | The full machine is deliberate: the xHCI controller is the endpoint that needs proving on. |
| `iommu-aarch64-uefi-gicv2-ordinary` / `iommu-port-ordinary-uefi` | The reduced machine; a DHCP lease and the system volume through translated endpoints | Nothing degraded, no fault. |
| `iommu-aarch64-uefi-gicv2-display` / `iommu-port-display` | The reduced machine PLUS EXACTLY ONE ENDPOINT, the GPU; the display driver online once, never restarted, and a FRAME REACHED THE DISPLAY | The driver reports online before any frame exists, so "online" is not the oracle; a boot where every present failed behind the controller looks identical until ConsoleService says which. |
| `iommu-riscv64-direct-aia` | The umbrella for the two rows below, on the IMSIC interrupt path. | Serial. |
| `iommu-riscv64-direct-aia-hostile` | As the aarch64 hostile row, on AIA/IMSIC. | Same. |
| `iommu-riscv64-direct-aia-ordinary` / `iommu-port-ordinary-direct` | As the aarch64 direct ordinary row. | Same. |
| `iommu-riscv64-uefi-aia` | The umbrella for the three rows below. | Serial. |
| `iommu-riscv64-uefi-aia-transition` / `iommu-port-transition` | The full machine through U-Boot; virtio, NVMe and xHCI quiesced by class | The riscv64 ESP is an NVMe namespace, so `CC.EN` is proved here. |
| `iommu-riscv64-uefi-aia-ordinary` / `iommu-port-ordinary-uefi` | As the aarch64 UEFI ordinary row; the ESP is kept because it is what the loader read. | Same. |
| `iommu-riscv64-uefi-aia-display` / `iommu-port-display` | As the aarch64 display row, on AIA/IMSIC. | Same. |

## The no-device-tree profile rows

| Catalog row | Profile-specific final assertions |
| --- | --- |
| `arch-profile-aarch64-no-dt-absent-1` | The same treeless contract as `-no-dt-1` with the descriptor ABSENT: the kernel must refuse to select a controller it was never authorized for, rather than falling back to one it recognises. |
| `arch-profile-riscv64-no-dt-absent-1` | The same absence contract for the riscv64 descriptor. |

## The evidence gate

`verify-evidence` boots a guest to prove the BINDING rather than the boot: every input is bound -
the medium, the firmware image and the kernel opened once and hashed through their descriptors, the
QEMU executable copied to a content-addressed run-private path and hashed there - and the fixture
then replaces the tool and the firmware AT THEIR PATHNAMES inside that window through
`LIBER_HARNESS_HOLD`. The boot must use the bound bytes anyway. A run that used the replacement, or
one that noticed nothing because it re-opened by name, is the defect.

## Remaining ordinary gates and concurrent selection

| Gate / case | Launcher and final predicate | Observation and backstop |
| --- | --- | --- |
| `provider-media-order` | `MEDIA_ORDER=swapped test.sh --arch x86_64 --tags boot`; suite completion and real routed file reads with FAT and UDF exchanged on the bus | Shared suite predicate/backstop; the boot test checks volume contents, including USB, rather than accepting enumeration order. |
| `virtio-multiport` | `test.sh --arch x86_64 --tags boot` against a machine whose virtio-console carries TWO ports on separate chardevs; the generic port's capture holds the stamp the driver wrote and the console port's capture holds its own output | Shared suite predicate/backstop. The claim is SEPARATION, so the stamp appearing on the console capture as well fails: one capture proving a byte arrived cannot distinguish two ports from one. A console port that lost its own output under multiport fails too. |
| `qemu-2d-demo` | `lab.sh boot` (or the instance already up) and `lab.sh sh test2d-sw --frames=600 --phase-frames=60`; three frames captured live through the running instance and `test2d-sw: done` in the demo's own output | The captured frames are the oracle, not the exit status: `check-2d-demo-frames.py` requires the scene the demo draws - animation between frames, analytic coverage, blending, filtering and a colour glyph. A run that completed with a blank or static display fails. |
| `qemu-3d-demo` | The same instance and `lab.sh sh test3d-sw <scene>`; frames captured through the instance, plus one after the demo leaves | Live frames must show a bounded central object, a repeating ground texture, a blended panel, the 2D overlay in the same frame, two stated poses and two aspect ratios - and the console must be RESTORED afterwards, which is a separate capture and a separate assertion. |
| `qemu-pcie-hotplug` | The same instance driven through `lab.sh monitor device_add`/`device_del` against the profile's empty hot-plug slot | The machine must carry the slot at all (a missing fixture fails rather than passes quietly), the driver must let go BEFORE the slot powers down - the two log lines are compared by position, because the reverse order is a surprise removal - and the slot must take a SECOND device, since a slot left powered down works exactly once and every line of the first cycle is already in the log. |
| `audio-routing` | The lab instance (booted by the gate with `LIBER_DEVELOPMENT=1` unless one is up), `lab.sh sh` running `audioprobe` and `audioctl`, and `lab.sh monitor device_add virtio-sound-pci,bus=hotplug0`/`device_del` while `audioprobe hold` plays | `audioprobe: PASS inventory` with at least two devices and `audioctl devices` naming a default output; the hold's lines counted, not matched - playing, moved to the card that arrived, returned when it left - and `audioprobe: PASS hold`; `PASS operator`, `PASS voice`; `audioctl counters` with no phone-stream overflow or underrun. 200 s for the hold. |
| `qemu-pcie-aer` | The same instance with injected PCIe errors through the monitor | The kernel must have found extended config space at all, or an error record is unreachable; a corrected error is surfaced, a fatal one on a bridge is said and not acted on, and a fatal one on a bound device quarantines it WITHOUT recording a removal - the device is still in the machine. |
| `smp-core-cap` | `test.sh` x86_64 smoke with `MAX_CPUS + 8`; suite completion, surplus parked, no unreachable shootdown, no retired pages | Shared suite predicate/backstop; no later assertion remains after suite exit. |
| `implementation-mutations`: duplicate-rights widening | Isolated mutation tree, `test.sh` with exact capability conformance fixture; failed suite with the widening assertion | `run_mutation` requires expected assertion, not compile error or unrelated failure; existing 900 s outer backstop. |
| `implementation-mutations`: stale generation | Same, with expected stale-handle assertion | Same. |
| `implementation-mutations`: cloning transfer | Same, with expected linear-transfer assertion | Same. |
| `implementation-mutations`: closed-table resurrection | Same, with expected closed-table assertion | Same. |
| `implementation-mutations`: wrong receive identity | Same, with expected named-message assertion | Same. |
| `concurrent-selection`: selection A | Concurrent exact x86_64 selection through `test.sh`; both suites must finish; this log contains all A and no B IDs | Shared suite predicate/backstop; own medium and result log paths. |
| `concurrent-selection`: selection B | Same, all B and no A IDs | Same. Gate retains two requested scheduler slots. |

## Shared producer and writable-output review

- Kernel selections already copy their executable while holding the producer lock. x86_64 loader
  staging in `qemu-run.sh` already builds and copies under that same lock. Profile no-DT loaders
  have private Cargo target directories. Ordinary port loader builds are prerequisite work; profile
  overrides do not overwrite their outputs.
- `signed-boot` uses `build-loader-private.sh`: both trust profiles share one Cargo target in the
  run's private directory, and each build and copy occurs under `kernel-test-build.lock`. The
  ordinary loader's bytes and metadata are preserved; rebuilding and restoring its profile would
  change the PE identity and invalidate shipping image receipts. Its guest media use only private
  copies. `check-guest-verdict.py` checks both profiles' isolation and forces a producer to contend
  at the old unprotected copy point, demonstrating that the old sequence loses the intended profile.
- `secure-boot` acquires its unsigned loader while holding the same lock. Its signed loader and
  enrolled variable template are now private to the run; only cached test-key generation remains
  shared, under its own lock. Every boot copies its writable variable store and medium.
- `perf-anchor` still builds its loader and ISO through the direct harness. Each profile passes
  `LIBER_IMAGE_OUTPUT` to `mkimage.sh iso`, so the image and its build-key/digest receipts stay in
  the gate's private directory. Its fresh loader cannot replace the shipping ISO another merge
  step is checking or booting. The host regression exercises the actual gate and image-output
  routing with payload producers stubbed; removing either opt-in or producer support fails the
  shipping image and receipt preservation check.
  The old path reproduced an IOMMU preflight failure: identical loader bytes under a private
  basename changed the shared shipping ISO's input key. ISO and disk-image assembly now timestamp
  a temporary loader copy for FAT staging, preserving the shared Cargo loader's bytes and metadata.
  The host fixture executes both production staging blocks with real mtools and verifies the FAT
  timestamp, original loader preservation and cleanup; direct stamping of the original fails it.
- The missing-bootstrap-list fixture formerly rebuilt and restored a shared volume. It now passes
  `--output-dir` to `mkpackages`, directing the image, UUID and fallback bootstrap files into its
  private directory. The shared build receipt is never rewritten; original byte/mtime assertions
  still verify preservation of the ordinary shape.
- The scheduler exports its concurrency bound; `qemu-run.sh` uses run-specific writable media and
  log ownership for concurrent architecture/profile/default-machine guests. Direct signed/secure/
  IOMMU launchers already put writable ESPs/variables/disks in private work directories. ISO inputs
  are read-only. `implementation-mutations` writes its separate tree, not the active source tree.

## Measurement status

Predicate and contention fixtures are host tests. They do not establish a guest performance result.
The historical complete signed-boot duration is 2,694 seconds. Matched before/after complete-gate
measurements and the milestone's warm/cold protocol remain required; no 14-second global-marker
experiment is treated as a baseline or a completed result.

## The destination-service and platform gates

These gates boot an x86_64 guest unless the row says otherwise, and assert on the probe, driver and
service lines they name; a gate prints its PASS only after every named line was found. Four launchers
recur. `guest_gate_run` (`guest-gate.sh`) boots `./run.sh --arch ARCH --smp 2` with `guest-console.py`
typing one command a line, inside a console window (`GUEST_GATE_SECONDS`) and a `timeout` deadline
(`GUEST_GATE_TIMEOUT`); the row gives the pair. `lab.sh scenario-cold` builds the development target
and its bootable volume, boots one guest, drives a `harness/scenarios/*.toml` through the console and
QEMU's emulated keyboard and takes it down; the scenario's own `timeout` is the backstop. `dev.sh up`
brings up a development instance in the gate's private `LIBER_DEV_STATE`, driven by `lab.sh sh` and
`dev.sh launch` and taken down from the EXIT trap; every gate that uses it refuses to start while an
ad-hoc `lab.sh boot` guest is up. A `test.sh` row uses the shared suite predicate above and also reads
each named step's own line from the run's result logs, because a step the harness skipped leaves the
suite green.

| Gate | Launcher and final predicate | Observation and backstop |
| --- | --- | --- |
| `bluetooth-service` | `guest_gate_run` twice over one `RUN_DISK` made from the bootable volume, `edu` at 0x1d: boot one types `btread` and the `btcheck` steps around a `start bluetooth_service`; boot two `btcheck reuse`/`forget` and `btctl scan`/`pair`/`enable` | Every `btcheck: PASS <step>` (limits and reuse twice in boot one), boot one run from the paired block volume, one fixture pairing whose key fingerprint is the one boot two presents with no pairing before the forget, and `btctl`'s one pairing after it. 260 s / 420 s per boot. |
| `bluetooth-classic` | `guest_gate_run` twice over one `RUN_DISK` made from the bootable volume, `edu` at 0x1d: boot one types `btclassic pair`, `btclassic policy`, `stop`/`start bluetooth_service` and `btclassic reuse`; boot two `btclassic reuse` and `forget` | Every `btclassic: PASS <step>`, boot one run from the paired block volume, the keyboard's pairing fingerprint presented again after the restart as a remembered key and after the cold reboot as the key the fixture was handed, and no pairing after the first boot's. 240 s / 400 s per boot. |
| `bluetooth-input` | `guest_gate_run` of `gamepad --lines \| btclassic gamepad` and `btclassic input`, `edu` at 0x1d; the fixture's keyboard then types `btclassic typed`, Ctrl+Alt+Delete, its Power key and `btclassic alive` at the prompt | `btclassic: PASS gamepad`, `btclassic: PASS input`, the fixture's typing, `btclassic: PASS typed`, both chords, `btclassic: PASS alive` after them, and one `BluetoothService: online`. 90 s / 240 s. |
| `bluetooth-le` | `guest_gate_run` of `btclassic le`, `btgatt` and `btclassic ctkd`, `edu` at 0x1d | `btclassic: PASS le`, `btgatt: PASS` and `btclassic: PASS ctkd`: the tag from a private address bonded under its identity by Numeric Comparison, the display by a typed passkey, the remote by legacy only after `pair-legacy`, three LE links, the host's name read, and the tag and remote back through the accept list on their stored keys; the application's grant reaching its two services and nothing else; the dual-mode phone's LE half encrypted on the key derived from its BR/EDR pairing, and its BR/EDR half authenticated on the key derived from its LE pairing, each fingerprint the fixture's own derivation. 160 s / 330 s. |
| `bluetooth-audio` | `guest_gate_run` of `btclassic audio`, `edu` at 0x1d | `btclassic: PASS audio`: the headset configured for SBC and its 150 ms delay in AudioService's latency, a second of a tone heard by the fixture's own SBC reader as 48 kHz joint stereo, bitpool 53, every CRC good and the third subband loudest; level 80 sent as absolute volume 102 and the headset's own 127 back as 100; its play answered not implemented; the phone's stream listed as a route and playing on the default output; the operator's pause at the phone; the headset's output gone with its link. 150 s / 300 s. |
| `bluetooth-transfer` | `guest_gate_run` of `btclassic transfer`, a file written by `sleepcheck count 10 >` and counted by `wc`, `btctl send` to the phone and the serial device, two `btclassic push 3100` each followed by `btctl receive`, then `btctl connect PHONE pan`, `btclassic tether held`, a disconnect, `btctl connect PHONE pan replace`, `btclassic tether up`, `ping -c 3 192.168.44.1` and a disconnect, `edu` at 0x1d | `btclassic: PASS transfer` with the phone's push refused while nobody waits; the file's `wc` byte count received by the phone over L2CAP (ERTM) and by the serial device over RFCOMM with one digest, and `btctl` reporting both; the phone's 3100-byte note received into `note.txt` whole - `wc` counting 3100 bytes, last line `line 00100 of the phone's note`; the same push refused at a 1000-byte bound before a byte of it (`0xcd`); the PAN link held by NetworkService with no lease asked, then made the uplink by `replace` with the phone's lease of 192.168.44.2 and pings answered across BNEP, then gone and NetworkService back on its NIC. 260 s / 400 s. |
| `bluetooth-le-audio` | `guest_gate_run` of `btclassic leaudio` and `btclassic broadcast`, `edu` at 0x1d | `btclassic: PASS leaudio`: the fixture's controller made an LE Audio one (extended advertising, periodic sync, CIS central, synchronized receiver) from the reset a power cycle sends; the left earbud paired by the operator with Just Works and trusted for audio, the right found by its RSI and bonded as the set's (`earbud R bonded`) and trusted for audio; one AudioService device - default output, stereo at 48 kHz, its own level - and a 16 kHz voice device; a 1 kHz / 2 kHz stereo stream configuring each sink for its own channel at 48 kHz / 120 octets, both CISes established, each earbud's fixture LC3 reader taking 50 frames whole with the loudest line at its own tone; level 80 as volume setting 204 on both members and a member's own 255 back as 100; a voice session winding the music down and configuring 16 kHz / 40 octets both ways, the session's tone heard by the earbud and the microphone's tone (1 kHz, by its zero crossings) read by the session; the call rung, answered from the earbud through the telephone bearer (`answered success`), active, hung up from it and ended; the microphone's stream released with the session. `btclassic: PASS broadcast`: the broadcast heard as `fixture broadcast`, its periodic train and BIG of 2 streams joined, a stereo 48 kHz route in AudioService, the earbuds hearing 1.5 kHz left and 3 kHz right; stopped, its route gone; encrypted - no route without the code, `BIG refused a wrong Broadcast Code`, joined with the right one; one member's link gone the device stays, both gone it leaves. 240 s / 380 s. |
| `power-service` | `guest_gate_run`, `edu` at 0x1e; the `powercheck` and `powerread` steps, then `stop`/`start power_service` and `powercheck list` | The exact canonical-unit lines for the UPS, battery and zone, every `PASS` step, DeviceManager's refusal of the undeclared publication, no `FAIL`, and two distinct epochs across the restart. 300 s / 480 s. |
| `brightness-acpi` | `acpi-fixture.py --brightness` with its GPIO backend, `VGA=std`, `usb-gadget.sh setup consumer-keys` with `keys-sim.py` on root port 3; a development instance of its own, reset once on the same volume; `brightcheck` through `dev.sh launch` | the panel joined to output 0 as `firmware-adapter`, the source the boot framebuffer decoded by 00:01.0, `_DOS` 0x04; the region at the AC default, then 40, 0x86/0x87/0x85 one step each, 0x88 to the floor, 0x89 nothing; the keypad's up, up, down; 300 and 10 lx to 100 and 70 through `_ALR`; 60 stored and restored after the reset with the settings kept. |
| `brightness-usb` | `usb-gadget.sh setup monitor` (root and `dummy_hcd`, failing rather than skipping without them) on xHCI root port 3 with `monitor-sim.py` as its firmware; `guest_gate_run` with `NET_NONE=1`, `brightcheck` through idle off, list, set, percent, up, down, zero with and without the explicit zero, `brightread`, automatic brightness on and off, `stop`/`start power_service` and a set after it | `driver.xhci: monitor bound` with its EDID and sensor; the backlight joined to output 0 as `single-output`; each level the probe sets, the floor at 5 without `--zero` and 0 with it; the read refused a set; the level reaching 82 and 13 as the light steps; the restarted policy's stored settings and a set it forwards; the firmware's log holds every level in order and no request it has no control for. 300 s / 480 s; the gadget's teardown is verified on every exit. |
| `power-ups` | `usb-gadget.sh setup ups` (root and `dummy_hcd`, failing rather than skipping without them) on xHCI root port 3 with `ups-sim.py` as its firmware; `guest_gate_run` with `NET_NONE=1`, `upscheck list` and `upscheck control` | `driver.xhci: HID power device bound`, `upscheck: PASS list` and `PASS control` with the on-battery and back-on-mains updates; the firmware's log holds exactly one turn-off and one cancel and no request it has no control for. 300 s / 480 s; the gadget's teardown is verified on every exit. |
| `ipmi` | `dev.sh up --timeout 400`, one instance at a time: `kcs`, `ssif`, `bt`, `pci`, `pair`, `harness`, `malformed`, `reboot`, `sleep` and `oob`, over `ipmi-bmc-sim`, `ipmi-harness-bmc.py` behind `ipmi-bmc-extern`, and OpenIPMI's `ipmi_sim` | Each boot waits for `BmcService: online` and the watchdog service's first answer, then each case's `bmccheck` and driver lines; the harness BMC's record shows the sleep's watchdog steps in order, and `ipmitool` over the LAN reads what KCS wrote. Fails rather than skips without `ipmi_sim` or `ipmitool`. |
| `qemu-ipmi-admin` | `lab.sh scenario-cold` six times, one BMC a boot: `ipmi-sel-clear`, `ipmi-sel-clear-cancelled` (the harness BMC), `ipmi-hard-reset`, `ipmi-power-down` and `ipmi-soft-shutdown` (both `-no-shutdown`), `ipmi-power-cycle` | AdminService's journal order, the harness BMC's reservation, added event and clear refused with 0xc5, exactly two boots after the hard reset, no power-button line on the power down and one after the request on the soft shutdown, and the 0xd5 refusal recorded `failed`. 900 s per scenario. |
| `smartcard-service` | `guest_gate_run`, `edu` at 0x1b; `cardread`, `cardb`, the `cardcheck` steps, `cardhold` holders and pipes, and `graph` twice | Every `PASS` step, SmartcardService's handle count equal in the two `graph` readings, and the card's authentication signature verified by OpenSSL on the host against its certificate. 300 s / 480 s. |
| `qemu-modem-service` | `guest_gate_run` twice, `edu` at 0x1c: `NET_NONE=1`, then with the NIC; then `test.sh --arch x86_64` with `TEST_SELECTION` naming the kernel's bounds scenario and `QEMU_EXTRA` unset | No-NIC boot: every `modemcheck` and `modemdata` `PASS`, with handles and clients back at baseline; NIC boot: `PASS busy` and `modemswap: PASS`; the bounds scenario ran with nothing `[failed]`. 240 s / 400 s per boot. |
| `qemu-camera-service` | `guest_gate_run`, `edu` at 0x1a, `NET_NONE=1`; `camread`, the `camcheck` steps, `camhold` holders, `camfail`, and `fg` before the second `graph` | Every `camcheck` `PASS` step, `camread: PASS`, `camhold: PASS retain`, and CameraService's handles equal in the two `graph` readings. 240 s / 400 s. |
| `qemu-midi-service` | `guest_gate_run`, `edu` at 0x19, `NET_NONE=1`; `midiread`, the `midicheck` and `midiump` steps, `midihold`, `midifail`, and `graph midi_service` twice | Every `PASS` step and MidiService's handles back at baseline. 270 s / 430 s. |
| `qemu-gamepad-tool` | `guest_gate_run`, `edu` at 0x17, `NET_NONE=1`; one typed line, `gamepad --lines \| gamepadcheck`, whose probe waits, bounded, for each step's line before the next | `driver.gamepad-fixture: online` and `gamepadcheck: PASS which gamepad pressed what`, with no `FAIL`. 180 s / 360 s. |
| `spool-service` | `test.sh --arch x86_64` selecting the spool scenario (printers the kernel harness plays) and the PermissionManager grant scenario | Shared suite predicate, plus `spool-probe: PASS` for `inventory`, `two`, `stall`, `abandon`, `crash`, `detach`, `unplug`, `reset` and `recovery`, the withdrawn job's evidence line, and both tests named with none `[failed]`. |
| `media-import-service` | The same, selecting the PTP responder scenario and the grant scenario | Shared suite predicate, plus `import-probe: PASS` for `browse`, `stale`, `import` and `removal`, and both tests named with none `[failed]`. |
| `i2c-bus` | `IOMMU=1 I2C_FIXTURE=bus test.sh --arch x86_64` selecting the virtio-i2c and virtio-gpio oracles, the controllers behind `vhost-i2c-gpio.py` with `iommu_platform=on`; a QEMU missing either vhost-user device or the virtio-iommu fails the gate | Shared suite predicate, plus the runner's and the kernel's enforcing-required lines, no `NOT RUN`, each oracle's claim line, and both tests named with none `[failed]`. |
| `i2c-hid` | `vhost-i2c-gpio.py --hid` with `I2C_FIXTURE=hid`, then `guest_gate_run` with `NET_NONE=1` and S3 offered; the host drives the backend's control socket on each probe cue. `--arch` boots a port over the tree's `hid-over-i2c` nodes | `hidcheck: PASS watch` first and again after each resume, `PASS storm`, `PASS cycle` and `PASS malformed`. 480 s / 720 s. |
| `qemu-admin-path` | `lab.sh scenario-cold x86_64 admin-path.toml`, the executor's `edu` at 0x18 and a persistent `RUN_DISK`; every decision a key through the emulated keyboard | The scenario's probe verdicts in order (1750 s), then no probe `FAIL`, the run from the paired block volume, exactly four executor writes, three AdminService instances with rising epochs and recovered records, and both `screendump` frames the protected screen with no hostile green. |
| `qemu-dfu-tool` | `lab.sh scenario-cold` of `dfu-tool.toml`, then of `dfu-backup.toml`, against `usbredir_device.py`'s runtime DFU target over `usb-redir`, the images on the run's own stick | One detach and one expected image for one confirmation, nothing for the decline, a backup of a no-upload target refused before the screen; then a declined backup that never detaches, a confirmed one byte for byte the target's image, and a bounded one stopped with no file. 900 s per scenario. |
| `qemu-tpm-tool` | `lab.sh scenario-cold` of `tpm-tool.toml` twice, `swtpm` behind QEMU's CRB front-end and then TIS | Per run, on the host from the serial log: two different draws, PCR 16 the SHA-256 chain of the typed text, PCR 8 unchanged, the quote verified by OpenSSL and a one-bit-altered attestation refused, and the driver online twice. 1200 s per scenario; fails without `swtpm` or `openssl`. |
| `shell-large-output` | `lab.sh scenario-cold x86_64 shell-large-output.toml` | The scenario's `expect` steps: the 5 KiB file's last line, `sleepcheck: count done`, through `cat`, `cat \| tail -n 1` and `head -n 101`. 400 s. |
| `typec-ucsi` | `dev.sh up --timeout 400` five times, a `ucsi-ppm.py` profile each: `v12`, `v21`, `slow`, `spoiling`, and `v21` again with S3 offered; driven by `dev.sh launch typeccheck`, `typec` and `sleepctl` | Each boot's `typeccheck` verdicts and `typec` lines; the PPM's command log fails any boot that breaks the command discipline, and in the sleep boot no command may fall between the driver's `SUSPENDED` and `sleep: resumed`. |
| `typec-tcpci` | One `dev.sh up --timeout 400` instance with the `I2C_FIXTURE=tcpc` SSDT, `vhost-i2c-gpio.py --tcpc` and `tcpc_partner.py` as the source; driven by `dev.sh launch typeccheck` | Each case's verdict and the partner's record (no request outside the offers, the sink path's rules held); the 200-negotiation timing run gated under KVM at p99 at most 15 ms and never 24 ms, and recorded but not gated without KVM. |
| `serial-handoff` | One `dev.sh up --timeout 300` instance; `lab sh` round trips, a driver kill, `lsdev --disable`/`--enable kernel:com1`, and a development panic request | Each handoff and reacquisition line counted as newly arrived, zero stray kernel accesses at every reacquisition, `lab sh` answered through the driver and through the kernel, and the dropped count then `*** KERNEL PANIC ***` on the wire. |
| `watchdog` | `dev.sh up --timeout 300` five times, one device a boot: `i6300esb`, `reset`, `tco`, `wdat` (`wdat-table.py` through `-acpitable`) and `bmc` | QEMU's run state through `query-status` under `-action watchdog=pause`: `running` through three timeouts, `watchdog` once ServiceManager stops answering `alive`, inside the stated window with a 3 s margin; the reset and BMC boots read the cause from the next boot's driver. |
| `acpi` | `dev.sh up --timeout 400` twice: the fixture boot (fixture SSDT, native PCIe hot-plug, `swtpm` CRB, vhost-user I2C and GPIO, `ivshmem-plain`, two free CPU slots), then `PCIE_HOTPLUG=acpi` | Every node accounted for with its reason, node channels and grants handed, the fixture driver's probes, the power classes through `acpipower`, two CPUs hot-added over QMP, and a service restart keeping every row but the cleared one; then `_OSC` refusing native hot-plug and the slots left unarmed. |
| `sleep` | `dev.sh up` five times: the main instance (S3 offered, `swtpm` CRB), then the platform, battery, fallback and `i6300esb` instances; the host stamps every serial line | S3 by QMP run state (`suspended`, then `running`) and suspend to idle by the stamped serial timing; after every wake the network, a file, a frame, the serial console, the wall clock and a wait; the lid, buttons, TAD, fans, battery and soft-off cases; `running` through a long sleep and `watchdog` after a hung resume. |
| `hibernate` | `dev.sh up --timeout 900`, thirteen boots, on a GPT disk with a hibernation partition, S4 offered and `swtpm` CRB; the script holds its own QMP connection across each request | `SUSPEND_DISK` tells S4 from a soft-off, and the image's header is read from the disk between boots; a restore with the counter going on, and the no-S4, modified, other-machine, other-system, hybrid, no-TPM and late-refusal cases, each refusal invalidating the header. |
| `hibernate-ports` | `check-hibernate-ports.py`: aarch64 then riscv64, one guest at a time, each a development build through `qemu-run.sh` (`UEFI=1`, four cores, 1 GiB) on a GPT disk with a hibernation partition, `swtpm` behind `tpm-tis-device` and then no TPM | QEMU exits at the firmware's power-off; the header is read between boots; a sealed and a clear-key restore with every core back and the counter unbroken, the no-TPM warning, and a modified image refused. The serial line is awaited 300 s and the shell 1800 s; fails without `swtpm`. |
| `processor-power` | `dev.sh up --timeout 400` three times, `processors`, `stopped` and `immediate`, with the fixture's processors, zone and fans; the script reads the registers the kernel wrote from the `ivshmem-plain` region | The states written under each profile, `_PPC` and load, the `_LPI` entries or their reasons, and a relaunch installing every table; past `_CRT` the orderly power-off with QEMU gone within 30 s, past the immediate threshold off within 5 s with no orderly sequence. |
| `qemu-2d-account` | Three `lab.sh boot`s of one development ISO, by digest, `SMP=4` and headless: `development-trace`, the same at `GPU_SIZE=640x480`, then `development`; each run `lab.sh sh --timeout 900 test2d-sw ...`; refuses to start beside another guest | Every run reaches `test2d-sw: done`, and `frame_account.py` closes each account within five percent against the demo's armed report, pinned runs on one lane and the pooled run on more; the collector's fixture logs are checked before any boot. |
| `tickless-idle` | `check-tickless-idle.py`: x86_64, aarch64 and riscv64 one at a time, each a development build through `qemu-run.sh` with its serial line on a socket the gate holds; the ports with `UEFI=1` and `IDLE_FIXTURE=1` | Typed bytes echoed within 1 s, wakes attributed to the console UART's receive line in the per-core idle records, a QMP hot-plug into the empty port seen; on the ports the idle states entered and a suspend to idle. The shell is awaited 300 s on x86_64 and 1800 s on the ports. |
| `boot-volume-controllers` | `guest_gate_run` seven times on x86_64 with `SYSTEM_DISK`: for `nvme` and then `ahci` two boots on one `RUN_DISK` made from the paired volume (`lsblk`, a file written, `poweroff`; then `lsblk`, the file read, `poweroff`), one `nvme` boot beside an unpaired LiberFS volume on a `virtio-blk` decoy, and one `virtio-scsi` boot | Each served boot must be the paired block volume and not a copy in memory, with `lsblk` naming the controller under `vol://system`; the file must read back cold; the decoy must not be chosen or make the choice ambiguous; the `virtio-scsi` boot must print `selected root volume is missing; refused` and serve no `vol://system`. 150/200 s a served boot, 120/170 s the decoy, 90/120 s the refusal. |
