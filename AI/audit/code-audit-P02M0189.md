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
