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
