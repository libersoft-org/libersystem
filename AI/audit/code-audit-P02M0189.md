IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0189 (2026-09-27T04:53:05Z):

Status: IN PROGRESS. This record is updated as the work proceeds; the final state is at its end.

## The baseline re-measure, taken before any instrument code landed

Tree: `ff08ea18b68700605be021ea93c2923a0e3c7097` with no working-tree change under `src/` (only the audit
files were modified). Host: Intel Xeon Platinum 8272CL @ 2.60 GHz, 100 CPUs; QEMU 10.0.11 (Debian); KVM,
`-cpu host`, q35 with the virtio-iommu translating (`dma: boot DMA mode enforcing-required`),
`virtio-vga,iommu_platform=on`, headless (`-display none`).

1. The 2026-09-15 command, unchanged (the demo has no `--workers` yet), at the lab guest's default core
   count (`./lab.sh boot`: `-smp 100`, of which the kernel brings 64 online - "36 declared core(s) past the
   64 this kernel holds stay parked"), no boot profile named:
   `./lab.sh sh --timeout 300 "test2d-sw --frames=600 --phase-frames=60 --size=640x480"`, three runs.

   | run | draw mean | draw worst | interval mean | interval worst |
   | --- | ---: | ---: | ---: | ---: |
   | 1 | 58.870 ms | 72.382 ms | 111.915 ms | 141.736 ms |
   | 2 | 57.651 ms | 68.108 ms | 110.290 ms | 132.231 ms |
   | 3 | 57.800 ms | 69.072 ms | 110.251 ms | 132.537 ms |

   Each run: presented=600 partial=58 multi-rect=58 - so 484 whole-surface presents.

2. The gate's dormant run in the gate's conditions: `DEV_PROFILE=1 SMP=4 ./lab.sh boot` ("LiberSystem boot
   profile: development", "4 of 4 cores online"), image `libersystem-dev.iso` sha256
   `ff18d7e803b02540e49992ce3798f140567f1fbff34221dccc4f30d023026d06`:
   `./lab.sh sh --timeout 300 "test2d-sw --no-input --no-second-surface --frames=160 --phase-frames=40 --size=640x480"`,
   three runs.

   | run | draw mean | draw worst | interval mean | interval worst |
   | --- | ---: | ---: | ---: | ---: |
   | 1 | 54.469 ms | 68.969 ms | 97.122 ms | 132.501 ms |
   | 2 | 56.408 ms | 73.642 ms | 99.024 ms | 132.980 ms |
   | 3 | 59.454 ms | 85.883 ms | 102.157 ms | 127.779 ms |

   Each run: presented=160 partial=38 multi-rect=38 - so 84 whole-surface presents. Run-to-run spread of
   the interval mean: 97.1 to 102.2 ms (5.0 ms, about 5 %).

## What was implemented (the instrument)

- `src/perfbuf` (new `no_std` crate, host-tested, linked by the kernel as `src/acpi` is): the 32-byte
  `Record` (`site`, `cycles`, `value`, `thread`, `core`, `kind`, `detail`), the 8 MiB bound
  (`CAPACITY` = 262,144), `Buffer` with `attach`/`arm`/`disarm`/`push`/`push_group`/`drain`, and
  `drain_lines`, which writes the records as `\x1ePERF` lines, then the `\x1ePERF-THREAD` table, then
  `\x1ePERF-END <records> <refused> <incomplete> <stale>`. An append is one `fetch_add` on the index, the
  fields, and a release store of the kind byte that marks the slot complete; a record past the bound is
  refused and counted, never wrapped. A drain disarms, waits (bounded) for each claimed slot's kind, and
  counts an unfinished slot as incomplete and a record older than the arm as stale. 13 host tests
  (`cargo test --manifest-path perfbuf/Cargo.toml`): layout and bound, unarmed drop not counted, a full
  buffer refusing and counting and keeping the FIRST records, re-arm resetting, stale and incomplete
  slots, a name group contiguous and refused whole, the exact drain line form, a tag that cannot split a
  line, the switch/wake vocabulary, and four threads appending concurrently with every record accounted.
- ABI (`src/abi`): `SYS_PERF_RECORD = 89`, `SYS_PERF_CONTROL = 90`, `PERF_CONTROL_{ARM,DISARM,DRAIN}`,
  `PERF_RECORD_{UNARMED,REFUSED}`; the snapshot in `abi/src/tests.rs` extended. ABI version unchanged.
- Kernel (`src/kernel/perf.rs`, new): `init` attaches the buffer ONLY when `arch::boot_profile()` is
  `development-trace` (2048 contiguous frames through the direct map; a failure is logged and the boot
  runs unmeasured); `sys_record` stamps the caller's thread (low 32 bits of its koid) and core;
  `sys_control` arms, disarms or drains. The drain writes through `arch::serial::write_bytes` with
  `flush_sync` whenever the ring is full, under the print lock, so no line is dropped and the console
  mirror is not flooded. Outside the profile both syscalls answer `ERR_UNSUPPORTED`. Each thread is
  named once per armed window (`Thread::perf_window`, a new `AtomicU64` field): a four-record group
  holding its process's koid and name, from which the drain's table is built.
- Scheduler (`src/kernel/sched/mod.rs`): `reschedule` became `reschedule_as(disp, why)`; a SWITCH record
  is appended at every real switch (thread to thread, thread to idle on block/exit/drain expiry) with
  the outgoing thread and why it left (`blocked`, `preempted` - only `on_timer_preempt` passes it -,
  `yielded`, `exited`); `enqueue` gained a cause and appends a WAKE record (woken thread, waker = the
  thread running here or 0 for a deadline, core, cause). `wake_object_for(koid, cause)` is new;
  `wake_object` passes OTHER; the channel send path (`object/channel/mod.rs`) passes MESSAGE;
  `check_deadlines` passes DEADLINE. Unarmed, each hook is `if crate::perf::armed()`.
- `rt` (`src/user/runtime/rt/src/lib.rs`): `perf_site(tag, value)` - one test of a cached flag (`SITES`,
  resolved from `boot_profile()` on first use) unless the boot is `development-trace`, then one
  `SYS_PERF_RECORD` stamped with `perf_clock()`; `perf_clock()` is `perf_now()` on x86_64/aarch64 and
  `rdtime` on riscv64 (the kernel's clock there is `time`); `perf_sites_live()`; `perf_control(op)`.
  `perf_mark`/`PERF` are unchanged.
- Sites: `graphics-app` `FrameLoop` (`acq-beg`/`acq-end`, `ready`, `prs-beg` with the rectangle count,
  `prs-end` with the serial, and every park's `park-ns`, `park-tk`, `park-end`); `test2d-sw` (`rec-beg`,
  `rec-end`, `drw-beg`, `drw-end`, `shape`, `acct-beg`, `acct-end`); DisplayService (`ds-req`, `ds-rply`
  on surface channels, `ds-acc`/`ds-accs` when a present is accepted, `ds-blt0`/`ds-blt1`/`ds-dev0`/
  `ds-dev1` exactly where `presentation-stats` takes its clock, then `ds-srcpx`, `ds-outpx`, `ds-path`,
  `ds-scan`, `ds-cmpl`, and `ds-wait`/`ds-woke` around the dispatch loop's `wait_any`); virtio-gpu
  (`gp-call`, `gp-rply`); the shared virtio queue (`vq-ntfy`, `vq-done` with `polls | yields << 32`).
- `test2d-sw` modes: `--account` (arms after eight presents, records `acct-beg` with the clock reading
  taken when the eighth present returned, drains at the end, prints a second `armed` report with
  nanosecond totals), `--hidden-surfaces=N`, `--offscreen` (same scene and phase walk into private
  memory of a surface image's layout, no display capability requested), `--primitives` (100,000
  `SYS_DEBUG_NOOP`, a MemoryObject of 640x480 and 1280x800 created/mapped/touched/copied/unmapped 20
  times each, 10,000 site-clock steps, 1,000,000 dormant sites against an empty loop).
- Harness: `GPU_SIZE=WxH` in `qemu-run.sh` (`virtio-vga,xres=,yres=`); the collector
  `src/harness/frame_account.py`, reached as `perf-trace.py --frame-account LOG` (the console tracer's
  3/4-field markers still parse; 7-field records are ignored by it); its fixture tests
  `src/tools/check-frame-account-collector.py` (13 cases, including the four rejections the plan names
  plus a failed serial join, lost lines, no anchor, a disagreeing demo report and several drains in one
  log).
- Gate `qemu-2d-account` (`src/tools/check-qemu-2d-account.sh`): collector fixtures first; refuses to
  start with a QEMU running; three boots (`development-trace` default scanout: scaled, direct,
  15 hidden surfaces, offscreen 640x480 and 1280x800; `development-trace` with `GPU_SIZE=640x480`: direct;
  `development`: dormant run and `--primitives`), SMP=4 and `DISPLAYS` unset; records per boot the image
  digest, the staged kernel/driver/DisplayService/demo artifacts with digests and which tree each came
  from (following the manifest's `linkage`), QEMU version, host CPU, the QEMU command line, and the
  guest's profile, cores, DMA mode, `tsc_hz` and buffer lines; refuses if the boots booted different
  images; runs the collector per drain against the demo's armed report; with
  `CARGO_PROFILE_DEV_OPT_LEVEL` and `ACCOUNT_REFERENCE` set, refuses the optimised row unless image,
  kernel and driver digests differ from the reference and DisplayService and the demo do not. The gate
  has no `--workers` pin yet (`PIN=()`): the demo has no `--workers` until P02M0193. Registered in
  `check.sh`, in `verify-model`'s `GATES` (subject `bin.test2d-sw`), `GATES_THAT_BOOT_A_GUEST`, the covers
  (render2d, soft2d, surface, graphics-app, bin.display_service, bin.virtio_gpu, kernel) and
  `release-required.toml`; `host.perfbuf` added to the release set the model derives.

Verification so far: `cargo test` for `perfbuf` (13 passed), `abi` (28 passed) and `verify-model` (157
passed, after adding the gate and `host.perfbuf`); the collector fixtures (13 passed); `./build.sh`
x86_64 passed; a manual `development-trace` boot and one `--account` run drained 10,960 records with 0
refused, 0 incomplete, 0 stale, and the collector closed all three shapes with 0.00 % residue and agreed
with the demo's armed report within the 1 % clock-conversion bound.

## Continuation (2026-09-27, from 14:55Z): the instrument finished, the account measured

The instrument above was committed by the owner at 15:06Z as it stood (11dfd1f2); every change below is on
top of it.

### Changes made to the instrument, and why each was needed

- MILESTONE IDS REMOVED FROM SOURCE COMMENTS (`kernel/perf.rs`, `frame_account.py`, the gate, `rt`, the
  frame loop, DisplayService, the drivers, `test2d-sw`, `check.sh`, `qemu-run.sh`, `abi`): the project's rule
  keeps them in `docs/todo` and `AI/` only.
- `--primitives` MEASURED A DORMANT SITE WRONG. It reported 29.8 ns per dormant site, fifty times a flag
  test. Diagnosed in the guest, three steps: a loop of `perf_sites_live()` after it cost 0.6 ns per call (so
  the image's view of the cached flag was right), and a SECOND loop of the same million sites cost 0.61 ms in
  all - the whole 29.8 ms was the FIRST site of the process, which resolves the flag through
  `SYS_BOOT_PROFILE`. The first site is now timed on its own (`site-first-ns`) and the loop after it is the
  dormant site: 0.63 ns per site against a 0.59 ns empty loop.
- THE FIRST SITE'S 29 ms IS A FINDING, NOT FIXED HERE: `arch::boot_profile()` on x86_64 re-reads the fw_cfg
  file directory on every call (`fwcfg::read_file`, one `inb` from port 0x511 per byte of a 64-byte entry per
  file, each a VM exit), so every `SYS_BOOT_PROFILE` costs ~29 ms of vCPU time. The reader also has one
  selector and one cursor shared by every core with no lock, so two cores reading at once can interleave.
  Written down for the account and left alone, as the plan's exception rule requires.
- `--warm-core=MS` ADDED to `test2d-sw` (spins MS before each draw, outside the draw's clock): the draw
  through the real path costs ~16 ms more than the same draw offscreen, and that excess is the same at
  640x480 and 1280x800 (a fixed cost, not a per-pixel one). Four temporary experiments located it, then were
  removed and only this mode kept, because it alone reproduces the evidence: drawing a second time into the
  same frame right after the first cost 46.1 ms (the offscreen figure); drawing into private memory in the
  live loop still cost 62.9 ms (so not the target memory); drawing offscreen in a process that had opened the
  surface first cost 45.5 ms (so not the process's state); spinning 5 / 20 / 60 ms before each draw gave
  58.5 / 50.3 / 45.9 ms. So the draw is slow because the CORE HAD BEEN IDLE: this host is itself a KVM guest
  (`systemd-detect-virt` = kvm, no cpufreq in it), and after the L2 vCPU halts the physical core comes back
  slower and takes tens of milliseconds of busy time to recover - a condition of the machine, entered every
  frame because the frame loop idles between frames. The gate runs it once in the dormant boot.

### The runs

- `ordinary-1` (06:02Z, before the changes above) and `ordinary-2` (16:30Z, the final instrument):
  `./check.sh --gate qemu-2d-account` PASSED both times (ordinary-2: 491 s). Results under
  `.build/logs/qemu-2d-account/ordinary-2`.
- THE DORMANT COMPARISON NEEDED A REFERENCE TAKEN UNDER THE SAME HOST CONDITIONS. The morning re-measure
  (97.1 - 102.2 ms interval, draws 54.5 - 59.5) and the afternoon dormant runs (104.5 - 105.6, draws 61.6 -
  63.6) differ mostly in the DRAW, which carries no site - the host drifted (the warm-core finding shows how
  much the draw depends on the host core). So the pre-instrument tree ff08ea18 was extracted with
  `git archive` to `/data/yellow/lsref` (no git state touched, no working file replaced) and built there, and
  the dormant command was taken alternating between the two trees, three runs per boot, two rounds:
  reference 103.0 / 103.6 / 104.4 / 103.8 / 104.8 / 104.1 ms (draw 60.5 - 61.8), instrumented 104.9 / 104.6 /
  107.8 / 105.2 / 104.6 / 105.1 ms (draw 62.1 - 62.9). The ~1.2 ms draw difference is in code with no site in
  it, and every library differs between the two builds - even `soft2d.lslib`, whose source is identical -
  because the build path enters the binaries (crate disambiguators and path strings), so the two trees also
  differ in code layout. A third tree, the instrumented source at the same-length path `/data/yellow/lsins`,
  is being built to separate the instrument from the path (result below).
- THE THIRD TREE SEPARATED THE TWO (16:51Z - 17:04Z, second A/B session, three trees alternating, three runs per boot,
  two rounds; the commands and every line in `scratchpad/ab3/summary.txt` of this session):
  | tree | interval runs (ms) | mean | draw mean |
  | --- | --- | ---: | ---: |
  | ff08ea18 at `/data/yellow/lsref` (no instrument) | 103.91 104.23 103.71 102.83 103.48 103.07 | 103.54 | 60.75 |
  | instrument at `/data/yellow/lsins` (same path length) | 104.28 103.77 104.37 103.68 104.32 102.69 | 103.85 | 61.38 |
  | instrument at `/data/yellow/libersystem` (the repository) | 104.20 104.40 103.75 103.81 105.20 104.55 | 104.32 | 61.80 |
  The instrument at a path of the same length is +0.31 ms and inside the reference's run-to-run spread
  (102.83 - 104.23); the same instrumented source moved to the repository's path is another +0.47 ms. So the
  dormant instrument is within the reference's spread, and the path, not the instrument, is the larger term -
  the draw, which carries no site, moves with both, which is code layout.
- THE OPTIMISED RUN'S FIRST ATTEMPT (about 17:05Z) FAILED IN THE HARNESS, NOT IN THE GUEST: the kernel at opt-level 2
  settled the boot chain in 739 ticks instead of 2201, so the shell prompted BEFORE the network's
  asynchronous `ipv6: link-local ...` lines, which then followed the prompt; `lab`'s boot wait accepts a
  prompt only as the log's last bytes and timed out at 240 s with the prompt in the log. The CHANGE MADE FOR
  MEASUREMENT (the plan's exception: the optimised comparison is impossible without it), minimal and in the
  harness only: `lab.py` - the broker's `WAIT` takes an optional `nudge`, which `cmd_boot` alone passes; with
  it, after `BOOT_NUDGE_QUIET` (5 s) of guest silence with no prompt at the end and `shell attached` in the
  log, the broker types ONE empty line, which the shell answers with a prompt. No other `WAIT` (the
  persistent development instance's, `lab.sh wait`) changes, since a person may have a half-typed line there.
- AND THE GATE LEFT THAT GUEST RUNNING: `boot()` set `BOOTED=1` only after `lab.sh boot` succeeded, so a boot
  whose wait failed skipped the exit handler's `lab.sh quit`. `BOOTED=1` is now set before the boot.
- THE OPTIMISED RUN: after the harness fix, its provenance check itself failed once on a relative
  `ACCOUNT_REFERENCE` (the gate runs from `src/`); the reference is now resolved against the repository as
  RESULTS is. The rerun PASSED (17:19Z - 17:25Z, 354 s): all four accounts closed, and "the optimised row is from
  another image: kernel and driver differ, DisplayService and the demo do not" (image `efe5ca33...`, kernel
  `4933537b...`, driver `48a27444...`). The unoptimised build's cost on a frame is 0.77 - 0.99 ms (acquire
  0.75 -> 0.07, the four transport crossings 0.32 -> 0.06, DisplayService's dispatch and the driver 0.07 ->
  0.02); no term above 5 % moves. The primitives in both builds are in the account.
- THE RAMP, re-measured with the tree's own mode in one boot of the ordinary image (`b45cbc...` again - the
  build is reproducible): `--warm-core=` 0 / 5 / 20 / 60 -> draw 61.6 / 58.4 / 49.6 / 45.7 ms.

### Written, and the state at this point (2026-09-27, 17:30Z)

- `docs/PERF.md`: the new section "Where a 2D frame's time goes, layer by layer (2026-09-27)" - conditions,
  profiles and staged artifacts, the re-measure, the instrument's dormant and armed cost with the A/B, the
  per-shape term table with work / blocked / runnable, the two sizes and the scaled row, the idle-core
  finding, copies and mappings, the dispatch loop against its handle count, the primitives, the offscreen
  comparison as ratio and cost with and without the chosen wait and the unoptimised build's cost, the
  optimised comparison, the verdict with every term above 5 % classified, four follow-ups, disproofs; and the
  2026-09-15 row's label corrected with its frame-shape counts.
- `docs/todo/P02M0189.md`: 36 items ticked; open are the x86_64-first/ports item, the one-tree item, the
  serial-walk item (the pooled row and the `--workers=1` pin come with P02M0193), the emulated ports' check
  and the "no item closed by an argument" item - all five wait for the end of the job, where the ports'
  check must be taken with a fresh x86_64 row on the tree it runs on, since P02M0193 and later milestones
  change code on the path. Follow-ups named there. `TODO.md` row updated.
- Checks run: `rustfmt --check` on `test2d_sw.rs` clean; `shfmt -d` on the gate clean; `python3 -c ast.parse`
  on `lab.py`; the collector fixtures (13) inside every gate run. `./check.sh --gate source-hygiene` FAILS,
  on seven harness files this work did not touch (`midi-source.py`, `mtp-root.py`, `printer-sink.py`,
  `ups-sim.py`, `usb_ffs.py`, `usb_gadgetfs.py`, `usbredir_device.py` - a shebang with mode 644 in git since
  an earlier commit); none of this work's files is in its list.
- The reference trees `/data/yellow/lsref` and `/data/yellow/lsins` were removed after the measurement (8 GB);
  `git archive ff08ea18` / `git archive HEAD` recreate them.

## The ports' first run with the sites in (2026-10-01)

- The aarch64 development boot of `check-tickless-idle.py` never reached its shell: `virtio-blk`'s driver died at
  its first perf site, again at each rebind, until its node failed - "ring-3 general protection fault (code
  0x6234f901) at 0x212bf4", the PC `rt::perf_now`, the syndrome's class 0x18 (a trapped MRS). A site is stamped with
  CNTVCT_EL0 in user mode, `perf_site_record` reads the clock before it asks whether sites are live, and the aarch64
  kernel never set CNTKCTL_EL1.EL0VCTEN - whose value at reset is the firmware's, clear on QEMU with or without EDK2.
  So on aarch64 every process reaching a site died at its first one, trace boot or not. FIXED in the kernel
  (`arch/aarch64/gic.rs`, `arm_local_timer`, run by every core): EL0VCTEN set, the physical count and every timer
  register left EL1's. riscv64 already lets U-mode read `time` (`scounteren`); x86_64's `rdtsc` needs nothing.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0189 (2026-10-08T03:25:49Z):

Read the complete plan, existing implementation record, collector, gate and cold scenario runner. Existing ordinary x86_64 evidence also includes the successful 2026-10-03 gate (675 s), but there are no completed port account artifacts and no final same-tree optimized comparison. The port branch already runs the specified scenario; unlike x86_64 it does not preserve the boot conditions and staged-artifact identities. Final measurement remains deferred until all implementation work is stable.

Status: continuation in progress; no unrun checks are claimed.

### Collector check and final measurement preparation

- PASSED: `python3 src/tools/check-frame-account-collector.py` (13 tests, 0.016 s).
- No harness source change is needed to capture the remaining measurement conditions: the cold runner retains its ESP and staged kernel, and the live QEMU command identifies its private files. The final run will retain these identities, the manifest-selected per-layer artifacts and the runner log beside each port account. Ports use `virtio-gpu-pci` under TCG, not x86_64's `virtio-vga` under KVM; their clock names will be stated explicitly.
- Required final runs are the ordinary and optimized x86_64 gate on the final tree, followed by the port gate's scaled run on aarch64 and riscv64. No new guest run has been performed yet.

### Same-tree completion requirements checked before final runs

Re-read the complete specification against the proposed final measurement. The one-tree item explicitly distinguishes changes before the insertion comparison from changes after it: for the latter, “the dormant comparison stands” and every current account/optimized/port row must be re-taken with that tree's dormant run as its reference. The recorded insertion A/B finished at 17:04Z on 2026-09-27; P02M0193's initial implementation record begins at 17:35:02Z, and the later timer/runtime changes and this continuation follow it. The specification therefore preserves that historical insertion proof and does not require reconstructing a current tree with the instrument removed. Comparing a current dormant run directly with the old `ff08ea18` interval as though only instrumentation changed would be invalid and will not be done.

Reviewed the instrument changes since the P02M0193 commit: `rt::perf_site` and its cached-flag path, `perfbuf` and the kernel's dormant armed-flag test retain their implementation; later `kernel/perf.rs` changes concern test configuration, allocation annotations and the post-run serial drain. Current `--primitives` still measures the cached site's cost and first-use cost independently.

Minimum final evidence remains: the complete ordinary x86_64 gate (including the current dormant reference, offscreen/direct/scaled/hidden/primitives/pooled rows), its same-source optimized comparison with the staged-artifact provenance check, then both emulated scaled accounts. Instrument overhead must compare the ordinary 160-frame report of the armed run with the ordinary 160-frame dormant report; the 152-frame armed-only report has a different shape mix and serves the collector's consistency check instead. A single-run delta will be reported as such, with no claim that it lies within the old tree's spread. If a material delta leaves the instrument's effect unresolved, targeted repeats are needed before closure. No build or guest was launched for this requirements review.

Latest final-source cross-build: `LIBER_DEVELOPMENT=1 ./build.sh --arch all` PASS (1277 s; `.build/logs/end-of-job/continuation-build-all-async.log`), SDK, libraries, userspace, kernel, loader, packages and volumes for x86_64, aarch64 and riscv64. This supersedes the earlier build as compiled-source evidence and includes the asynchronous provider/policy IO corrections plus the final additive fixture operation. Current service-logic tests also PASS (955, one pre-existing ignored; `continuation-service-logic-async.log`); source-hygiene/model/model-tests PASS (208 s) and generation drift check PASS (19 s). Runtime gates and milestone-specific completion limitations remain separately recorded.

### Final same-source measurement: ordinary x86_64 completed

After the final functional gates released every guest and fixture, ran the complete ordinary gate with fresh result/status paths: `env -u CARGO_PROFILE_DEV_OPT_LEVEL -u ACCOUNT_REFERENCE -u LIBER_BOOT_PROFILE -u DISPLAYS -u GPU_SIZE -u USB_GADGET LIBER_DEVELOPMENT=1 ACCOUNT_RESULTS=/data/yellow/libersystem/.build/logs/qemu-2d-account/continuation-20261008/ordinary RUN_STATUS_FILE=/data/yellow/libersystem/.build/logs/qemu-2d-account/continuation-20261008/ordinary.status ./check.sh --gate qemu-2d-account`. PASS, exit 0, 687 s. All four collected scenarios passed their closure and demo-report checks; pinned runs used one lane and the pooled row used four. Logs, raw reports, trace drains, JSON accounts and per-boot conditions are retained under that result directory; the adjacent ordinary.log/status hold the wrapper's terminal result.

The measured build-input identity includes 2,798 tracked/new files under src, .cargo, shell build/harness entry points, product.conf and toolchain.lock; prose is excluded. It was `58d9026ae15c0247044c3765db7526986167c76ccca36e7dfb58eb113cb5e6e4` both before and after the ordinary gate (`source-start.json`, `source-after-ordinary.json` beside the results). Same-source optimized and emulated-port measurements are still required and not yet claimed passed.

The optimized x86_64 gate also PASSed, exit 0, 490 s: the same command with `CARGO_PROFILE_DEV_OPT_LEVEL=2`, `ACCOUNT_REFERENCE=.../continuation-20261008/ordinary`, and fresh optimized result/status paths retained that override for every build/boot. Its provenance check proved that the image, staged kernel and virtio_gpu changed while the release DisplayService and demo remained byte-identical. Every account and worker-pin check passed. The full build-input hash remained identical through this gate (`source-after-optimized-before-collector.json`).

Independent review then identified an analysis defect which the original collector tests had missed: an early deadline wake can cause a second park against the same fixed 16 ms pacing deadline, but the collector summed both remaining requested durations as if they were separate policy waits. This overstates the chosen delay and understates rounding/dispatch overshoot without changing the total pacing term or residue. Both original gate PASS statuses, original account JSON/text and the pre-correction collector/fixtures are preserved; the originals live under each result directory's `original-collector`. The port runs are paused for the minimal collector/fixture correction and offline replay of the already captured x86 traces. No guest or instrumentation behavior is being changed, and original chosen/rounding figures are not accepted as final evidence.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0189 (2026-10-08T06:31:07Z):

Independent continuation review of the complete plan, existing account, current collector and ordinary run found
that all four accounts have 152 frames (76/38/38), complete drains and negligible numeric residue; the matched
160-frame armed/dormant reports and artifact provenance are valid. However `park_breakdown` sums remaining
requested delays over repeated parks against one pacing deadline. In the captured first scaled frame, a park
requested 15.952673 ms and returned after 15.215657 ms; the retry requested the remaining 0.725688 ms and returned
after 10.421902 ms. The old collector counted 16.678361 ms as policy delay even though these waits share the
same nominal 16 ms deadline. Frame closure hides the error because the same overcount is subtracted from its
rounding figure. `FrameLoop::park` records remaining time; `Pacing::on_present` sets the deadline once.

The authorized correction is limited to the host collector and its meaningful regression fixtures. Actual wait
inside the requested deadline and actual overshoot must be separated per park, including message wakes and early
deadline retries, with execution between parks left in the surrounding frame-loop term. No runtime, instrumentation,
policy wait, kernel timer or guest build is changed. Both original collectors and original account outputs have
already been archived by the performance continuation; guest runs are paused while this correction is verified.

Implemented `park_breakdown` in `src/harness/frame_account.py` using actual finite park spans: each contributes
`min(waited, remaining request)` within its requested deadline and `max(waited - remaining request, 0)` beyond
it, including message-ended waits. Repeated parks do not recreate a full policy interval, and between-park
execution remains in the surrounding frame-loop term. Provided complete park pairs are clipped to the account
window while preserving their original deadline; this does not recover trace records missing from that window.
A timeout remains finite even when its recorded relative tick count has expired to zero. Verified the actual ABI
`ERR_TIMED_OUT = -11` (`src/abi/src/lib.rs`); other negative wait results are refused rather than mislabeled as
deadlines. Message wakes with neither a recorded finite request nor finite tick count stay in the main loop term.
The nominal `Pacing` interval remains 16 ms; these fields measure actual time inside parks, and the excess includes
tick, wake and scheduling delay. Existing JSON field names are retained for compatibility; the text renderer and
frame-term comment explicitly describe the corrected meaning.

Added five meaningful regression cases in `src/tools/check-frame-account-collector.py`: early timeout followed by
a retry against the same deadline; early and late message wakes; finite expired versus unbounded release waits;
window clipping without moving the original deadline; and rejecting a non-timeout wait error. Corrected the old
synthetic timeout value from -110 to the actual -11 ABI value. With the old production collector, the first 17-test
run had the 13 existing tests pass and three new failures; its log is preserved as
`.build/logs/qemu-2d-account/continuation-20261008/collector-pacing-before-17-tests.log`. The final 18-test fixture set
against that same old collector failed four cases, also preserved in `collector-pacing-before.log`. After the
correction, `python3 src/tools/check-frame-account-collector.py` PASSed all 18 tests in 0.016 s; final output is
`collector-pacing-after.log` in that directory. Parsing both edited Python files with `ast.parse` and
`git diff --check` PASSed. Root and performance continuation independently reviewed the formula and tests; no
runtime or instrumentation source changed. Offline replay of all eight preserved x86 accounts and final static
checks are delegated and still pending at this point, so original erroneous pacing splits are not final evidence.

The reviewed collector correction is now applied and frozen; its worker records the fail-before/pass-after regressions separately below/above in this file. Independently reviewed finite-deadline clipping, early retry and message-wake attribution, unbounded waits and rejection of non-timeout errors. Replayed all eight retained x86 accounts with `python3 src/harness/frame_account.py <serial-log> --drain <1|2|3> --demo-report <run-report> --json <profile>/corrected-collector/account-<scenario>.json`; exact absolute commands are retained in `.../continuation-20261008/corrected-replay.log`. All eight PASS; wrapper `frame-account-offline-replay` reports exit 0, 4 s in corrected-replay.status. Separate corrected-collector directories hold the final JSON/text, without overwriting the original outputs or claiming another guest run.

`collector-source-change-proof.json` compares every source/build input against the pre-correction snapshot. Exactly `src/harness/frame_account.py` and `src/tools/check-frame-account-collector.py` changed. The remaining 2,796 inputs, including every guest source, instrumentation site/payload and build input, have identical SHA-256 `8a522d442bf066528e2581f2fbb281c9b5d3bb9035f1ea7c64fc1a7f05a8ee6c`; therefore the original executed traces remain valid. The wider identity including analysis changed from `58d9026ae15c0247044c3765db7526986167c76ccca36e7dfb58eb113cb5e6e4` to `f982499934b845776c3a5824f091f6bbcc148b4fc9792a408fc7a03053b1e6bc`. Both individual collector/fixture hashes are retained in that proof. This is an offline analysis repair, not a new full-build or guest verification claim.

All corrected shape accounts still close with only floating-point residue. The ordinary scaled mean time actually inside parks before the requested deadline is 15.962/15.963/15.966 ms (whole/partial/multi-rect), and measured overshoot is 6.081/5.442/5.154 ms. The fixed policy remains 16 ms; loop work between parks is not counted as another chosen delay, and overshoot includes dispatch/scheduling delay as well as tick rounding. Earlier historical chosen/rounding figures will be explicitly superseded in docs/PERF.md rather than silently retained as valid classifications. Port guest runs and final documentation/restoration are still pending.

The performance continuation replayed all eight ordinary/optimized x86 accounts from the unchanged captured raw
logs with the corrected collector: PASS, 4 s. Exact `python3 src/harness/frame_account.py ... --drain ...
--demo-report ... --json ...` commands and each exit-0 result are preserved in
`.build/logs/qemu-2d-account/continuation-20261008/corrected-replay.log`; corrected JSON/text are under each profile's
`corrected-collector` directory and originals remain under `original-collector`. Independent read-only Python
comparison of every JSON field confirmed that only each shape's two pacing subterms differ (six fields per
account). All frame counts, trace records, named intervals, CPU/blocked/runnable splits, device, IPC, dispatch and
demo comparisons are unchanged. Recomputed named-term sums and CPU-state sums close within floating-point
residue (~1e-14 ms) in all eight accounts. The 24 shape means now place 15.952–15.992 ms within requested deadlines,
with nonnegative overshoot. Ordinary scaled whole is 15.961499 ms within deadlines plus 6.080761 ms beyond,
inside its unchanged 22.107357 ms pacing/frame-loop term; the remaining 0.065097 ms stays attributed to loop work.

Independent optimized provenance review also confirmed the same image/kernel/virtio_gpu across its trace,
dormant and small-scanout boots, opt-level 2 retained throughout, and release DisplayService/demo identical to
ordinary. Its matched 160-frame reports give 100.292 ms armed versus 101.208 ms dormant mean intervals; the
observed -0.916 ms difference is confounded by run variation and is not negative causal instrumentation overhead.
Both profiles' trace accounts remain the separate 152-frame 76/38/38 shape sample; worker reports confirm one
pinned lane versus four pooled lanes. Host primitive cached-site totals are single-aggregate measurements, not
precision estimates of causal overhead. Final prose interpretation and port runtime gates are owned by the
performance continuation and still pending here; no further collector or runtime changes are planned.

The subsequent ARM native gate completed its 160-frame scenario but exited 1 after 751 s solely because its
port postcheck required the literal `boot profile: development-trace` banner. Independent source review confirmed
that only x86 `kmain` prints that banner. ARM and RISC-V instead call common `announce_measurement`, whose anchor
requires the exact `development-trace` profile; `perf::init` independently checks that same profile before buffer
attachment, and both performance syscalls return `ERR_UNSUPPORTED` without it. The ARM run had its 62.5 MHz anchor,
attached buffer and complete `PERF-END 16463 0 0 0` drain, with 152 armed frames (76/38/38), so the banner check was
an incorrect architecture assumption. The original failing script, native result and raw log are retained.

After that guest terminated and its script was archived, changed only the port guard in
`src/tools/check-qemu-2d-account.sh` to require the existing trace-only buffer attachment text, with a comment
explaining the architecture distinction. Existing worker, anchor, complete-drain and matching armed-demo checks
remain. No kernel, runtime, instrumentation or scenario was changed. `bash -n` and `shfmt -d` on this script,
`git diff --check`, and all 18 collector fixtures PASSed (0.017 s). The retained ARM log returned exit 1 for the
old banner grep, exit 0 for the new guard, and exit 0 for the unchanged full collector with `--drain 1` and the
same log as `--demo-report`. A focused offline CLI check also confirmed refusal of a dormant-only log, a run
missing its END, a run missing its anchor, and a fake attachment/anchor/empty-drain log: all exit 1 for the
appropriate missing evidence. Exact commands/results and the small check script are preserved as
`continuation-20261008/port-oracle-check.log` and `check-port-oracle.py`; independently collected JSON/text live in
`aarch64/portable-oracle-checks`. The native banner failure remains a historical failure; corrected offline
postchecks of that same completed guest are recorded separately by the performance continuation.

Final measurement continuation (2026-10-08T07:22:19Z):

The emulated checks ran last and alone, after both x86 accounts and corrected offline replay. The canonical
ARM postchecks are preserved in `continuation-20261008/recheck-aarch64.sh`, `aarch64-final-check.log` and
`aarch64-final-check.status`: PASS, exit 0, 1 s. They require the exact production trace-buffer line, one
pinned lane and the complete collector against the same actual 160-frame cold scenario. Corrected output is
`aarch64/corrected-check/account-scaled-aarch64.{json,txt}`. The native gate's exit 1 / 751 s banner failure
remains unchanged in `aarch64.log` and `aarch64.status`; no second ARM guest invocation is claimed.

RISC-V ran the corrected gate natively: PASS, exit 0, 1,111 s, with 152 accounted frames (76 whole / 38 partial /
38 two-rectangle), 17,344 records and `PERF-END 17344 0 0 0`. Its build stage passed in 68 s. Both port commands
cleared `CARGO_PROFILE_DEV_OPT_LEVEL`, `ACCOUNT_REFERENCE`, `LIBER_BOOT_PROFILE`, `DISPLAYS`, `GPU_SIZE` and
`USB_GADGET`, set `LIBER_DEVELOPMENT=1 SMP=4 ACCOUNT_RESULTS=<port>` and ran the following wrapper, with separate
absolute status/log paths under `.build/logs/qemu-2d-account/continuation-20261008/`:

```sh
bash -c 'SCRIPT_NAME=frame-account-port; source ./lib.sh; arm_run_verdict; src/tools/check-qemu-2d-account.sh --arch "$1"' _ aarch64
bash -c 'SCRIPT_NAME=frame-account-port; source ./lib.sh; arm_run_verdict; src/tools/check-qemu-2d-account.sh --arch "$1"' _ riscv64
```

The port cold runners initially logged unanswered development-channel handshakes while their boot continued.
These are the existing bounded readiness retries, not failed attempts to run the account or relaxed timeouts.
Each completed its original scenario. Both guests and owned fixtures terminated; no QEMU remained before
restoring the x86 image. RISC-V's native output is `riscv64/account-scaled-riscv64.{json,txt}` and its complete
serial log is `riscv64/serial-riscv64.log`.

For both ports, `conditions-live.json` captures the actual running QEMU argv, 4 vCPUs / 512 MiB / TCG / virtio-gpu-pci,
QEMU 10.0.13, host Xeon Platinum 8272CL, kernel/driver dev opt-level 0, release PIE DisplayService/demo,
firmware inputs, private ESP and staged kernel hashes. `conditions-final.json` records observed DMA/profile
buffer evidence, all four cores/harts online and unchanged private ESP/kernel bytes at completion. ARM uses
CNTVCT_EL0 at 62.5 MHz; RISC-V uses global rdtime at 10 MHz, not per-hart cycle counts. ARM whole / partial /
multi-rectangle intervals are 1691.692 / 1238.187 / 1227.816 ms; RISC-V's are 2464.371 / 1767.117 / 1755.028 ms.
Every shape's named account and CPU/blocked/runnable split closes with floating-point residue only and agrees
with the demo's own armed report. These are emulated consistency checks, never native hardware-speed claims.

`source-after-riscv64.json` equals the pre-RISC snapshot. The final proof
`collector-and-port-gate-source-change-proof.json` records exactly three changes since the initial x86 execution:
`src/harness/frame_account.py`, `src/tools/check-frame-account-collector.py` and
`src/tools/check-qemu-2d-account.sh`. All 2,795 remaining guest/source/build inputs are identical with SHA-256
`ae4fb91668fb7bf60b8372755454f2f0b40b2a938334e9200451bcdd80340210`. The final wide hash is
`52b900132b4e7a0dbbb1b116d391a87b0523419f8a84def07d73837458274b5f`. Individual old/new hashes and original
analysis files remain preserved. This proves that replay and the portable guard correction did not change the
guest path; it does not claim a new full build for the collector-only edit.

Wrote the final dated section in `docs/PERF.md`, with every named boundary for both x86 build profiles, all
three size/scale conditions and both ports; damage pixels/copies/mappings; hidden-surface dispatch; system
charge relative to each profile's offscreen row; current dormant/armed and cached-site measurements; syscall,
IPC, allocation/map/copy/unmap and scheduler primitives; device wall/spin/blocked/runnable/poll/yield figures;
the pinned and pooled rows; classification, disproofs and named missing-primitive follow-ups. Historical
chosen-wait/rounding classifications are explicitly superseded, preserving the original history and insertion
A/B. The current single-pair negative armed/dormant deltas are not claimed as negative causal overhead or as
lying within the old tree's spread. Actual park spans include CPU syscall/instrument/return work; neither the
before-deadline nor beyond-deadline number is exclusively blocked time, and large TCG overshoot is not charged
entirely to native tick quantization. The 16 ms policy itself is unchanged.

Restored ordinary x86 artifacts with the opt/profile/display/gadget overrides unset and
`LIBER_DEVELOPMENT=1 ./image.sh --format iso --dma-mode enforcing-required`: PASS, outer exit 0; nested x86 full
build PASS 73 s, then default ISO cache hit. Log: `continuation-20261008/restore-ordinary.log`. The restored
kernel, virtio_gpu, DisplayService and demo hashes match the ordinary measured artifacts exactly. Default ISO
SHA-256 is `ddb8e63b14a3eac14a752fd0c1579ed6c3a18a2afc83513c008ca7f1776fcf81`; this is the default image, not
the gate-private measurement ISO. Root was notified that artifact-dependent dynamic-report refresh can now
run. No further guest run or full-suite rerun is required by this milestone, and none is claimed here.

Independent final read-only review checked all 30 shape accounts' named-term/state sums and counts, every
published cell in the eight full per-layer table sets, all 18 charge/ratio rows, port artifact/profile hashes,
counter anchors and record totals. The reviewer found no remaining arithmetic or specification-interpretation
blocker and no reason for another run. `git diff --check` passes on the changed milestone documents; bytewise
prefix checks against HEAD confirm the existing P0189, P0192 and P0193 audit contents remain intact. Marked only
the four outstanding completion checkboxes and status in P02M0189, and its TODO row, COMPLETE. All required
implementation and measurement verification for P0189 is now satisfied. The ARM failed native invocation and
its successful corrected offline postchecks retain their distinct statuses. No audit rating is assigned.

Final coordinator consistency checks: `./check.sh --gate milestone-index` PASS (3 s; `.build/logs/end-of-job/continuation-milestone-index-final.log`), `git diff --check` PASS. All thirteen original audit files remain exact byte prefixes from baseline `d0f54598`, each with its UTC continuation record. Plan/index review confirms ten completed requested milestones and only P02M0196, P02M0197 and P02M0202 open for their explicitly recorded hardware/design requirements. No owned QEMU, TCPCI backend or UPS simulator remains running.
