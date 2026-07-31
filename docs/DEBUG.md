# Debugging LiberSystem

The tools for poking at a live system, tracing where time goes, and reading what
the machine did. Everything here runs from the `src` directory; the build and run
basics are in [INSTALL.md](../INSTALL.md).

## The lab harness (`just lab`)

`boot/lab.py` drives a live instance end to end: it boots QEMU with the serial
console on a unix socket, keeps a broker attached to it (so no output is ever
lost), and turns the debug loop into single commands with real request/response
semantics - no `sendkey` pacing, no sleep-and-grep of a log file.

```sh
just lab boot --fresh     # boot; --fresh recreates the data volume first
just lab sh time ls       # run a shell command in the guest, print its output
just lab sh lsvol
just lab int              # Ctrl+C the foreground job (a stuck ping, a cat)
just lab log -f           # follow the serial log (or: lab log <pattern>)
just lab pcap on          # start capturing guest network traffic
just lab pcap dump        # decoded packet list (ARP/ICMP/UDP/TCP with seq/ack/win)
just lab monitor info usernet   # any QEMU monitor command
just lab key date         # type through the emulated keyboard (the HID path)
just lab shot shot.png    # screenshot the framebuffer
just lab test             # one kernel suite pass: prints RC and the [ok] count
just lab quit             # shut the instance down and clean up
```

How it works: `lab boot` starts `just run` with `SERIAL=unix:...,server` (QEMU
waits for the connection, so even the first boot line is captured) and forks a
broker that owns the serial connection, tees everything to
`boot/.build/lab-serial.log`, and serves a small control socket. `lab sh` sends
the command and collects output until the shell prompt returns, then prints
exactly the command's output (echo, colors and prompt stripped). `lab sh`
returns when the prompt does, so timing a command from the host is meaningful.

Notes:

- `sh` joins its arguments - `just lab sh time cat motd.txt` needs no quoting.
  A long-running command takes `--timeout <secs>` (default 30).
- `key` goes through QEMU `sendkey`, i.e. the virtio-input/USB HID path - use it
  when the keyboard pipeline itself is what you are testing; `sh` is the fast
  path for everything else.
- The broker lives as long as the instance; after editing `lab.py`, restart with
  `lab quit` + `lab boot` so the new broker code runs.
- `pcap` attaches a QEMU `filter-dump` to the NIC at runtime; `dump` decodes the
  capture with TCP flags, sequence numbers, windows and options - enough to spot
  a handshake or retransmit problem without leaving the terminal. The raw file
  (`boot/.build/lab.pcap`) opens in Wireshark when more is needed.

## The development instance and the fast loop

`just lab boot` above is the ad-hoc harness: it boots a guest for as long as you are looking at
it. The development instance is the other shape - one guest that stays up across many edits,
taking new builds of a tool without rebooting. Use it for ordinary work on a leaf tool or a
provider; use `lab` for one-off poking and for anything the persistent profile does not cover.

```sh
just dev-up                                            # boot once and keep it
just dev-loop uname boot/scenarios/shell-basics.toml   # build -> publish -> run
just dev-status                                        # what is running, and is it current
just dev-console                                       # attach a terminal (--read-only to watch)
just dev-down                                          # stop it
```

`dev-loop` is the whole iteration in one command: it builds the artifact, publishes it into the
running guest and runs the scenarios, stopping at the phase that failed. Each phase is exactly
the command a person would run by hand, and they are all available separately: `dev-build`,
`dev-publish`, `dev-test`, `dev-launch`, `dev-rollback`, `dev-reset`. A warm iteration is a few
seconds, and the build phase reports what it reused, so an unexpectedly broad rebuild is visible
on the summary line rather than only in the clock.

The persistent profile is x86_64 only. The control channel it uses is not: the same port is
present in the cold test configuration of all three targets.

### What iterates hot and what does not

Only an artifact the installed manifest already declares can iterate hot. Creating a new
governed tool or provider is a manifest change, and that costs one cold cycle before fast
iteration on it becomes available. That is the everyday consequence of everything below: the
first build of a new tool is always slow, and every build after it is fast.

| what changed                           | iterates | why, and what it costs                                                                      |
| -------------------------------------- | -------- | ------------------------------------------------------------------------------------------- |
| a declared `.lsexe`                    | hot      | publish it; the next launch resolves the generation                                         |
| a declared `.lslib`, compatible        | hot      | publish it; the loader decides compatibility at the launch                                  |
| a declared `.lslib`, incompatible      | cold     | the registry keeps it, every launch refuses it; only a rebuild installs it                  |
| a new governed tool or provider        | cold     | undeclared artifacts cannot be published at all                                             |
| manifest grants or ownership           | cold     | the installed manifest is the launch authority and is read at boot                          |
| the shared boot contract (`bootproto`) | cold     | rebuilds every binary that consumes it, kernel and loader alike                             |
| ProcessService or PermissionManager    | cold     | the launch path itself changed, and it is staged in the volume package                      |
| a driver                               | cold     | staged in a package and bound at boot by DeviceManager                                      |
| the kernel                             | cold     | recompile, then image reassembly                                                            |
| the loader                             | cold     | recompile, then image reassembly                                                            |
| package layout                         | cold     | reassembly; on aarch64 and riscv64 also a kernel relink, since those two embed the packages |
| QEMU device topology                   | reboot   | the VM restarts; nothing is rebuilt                                                         |

A refused provider generation fails the launch rather than falling back to the installed image.
That is deliberate: falling back would run something other than what was published while looking
like success.

`dev-status` compares the running guest against the tree in six named input classes and tells
you which one moved. None of them is hot-publishable; each needs a rebuild, and some a reboot:

| class      | covers                              | a change costs                                |
| ---------- | ----------------------------------- | --------------------------------------------- |
| `protocol` | the shared boot contract            | rebuilds every binary that consumes it        |
| `kernel`   | kernel sources and the built kernel | recompiles the kernel, reassembles the image  |
| `loader`   | loader sources and the built EFI    | recompiles the loader, reassembles the image  |
| `packages` | `init.pkg`, `volume.pkg`            | reassembles the image from unchanged binaries |
| `image`    | the ISO and `mkimage.sh`            | reassembles the image                         |
| `topology` | `qemu-run.sh`                       | restarts the VM; the image is not rebuilt     |

The comparison is by content, so a rebuild producing identical bytes does not read as a change,
and a reattach keeps the recorded fingerprint - the broker changed, the guest did not.

Independently of all that, the full shared-image rebuild, the fresh-boot QEMU suites and the
three-target builds remain checkpoint gates. They are not run after each local edit, and the
fast loop is not a substitute for them.

### What a published generation means

Publication is transactional. Bytes stream into a bounded candidate, which is then verified for
complete length, digest, readable image, target architecture, canonical image identity, manifest
ownership (the record must name the artifact it is published as), declared providers covering
the image's own dependencies, and readable dynamic metadata. Every check runs before anything
becomes visible, and a refused candidate is dropped rather than kept, so it spends none of the
budget. The stated limits are 32 MB per artifact, 64 MB of live registry, three retained
generations per artifact and one publication in flight; each is checked before a byte is
reserved.

The registry is memory the development agent holds. It never touches the system volume, so a
publication cannot damage a cold boot and a reboot returns the guest to its built state. It also
does not survive the agent, which is what `just dev-restart` is for when the smaller `dev-reset`
is not enough.

A published `.lslib` may resolve before the installed provider only when it is compatible, and
compatibility is decided at the launch by the loader rather than trusted from the publication -
the registry holds incompatible generations too. A refused provider fails the launch instead of
falling back to the installed image, because falling back would run something other than what
was published while looking like success.

`dev-rollback <name>` returns an artifact to the generation before its newest; `dev-reset` drops
everything the registry holds. `dev-status` lists what currently shadows the volume, with each
generation's identity and age, so a forgotten override is visible rather than debugged as a
missing fix.

One limitation is inherent: a launch the development agent itself starts cannot be shadowed,
because the agent is inside the launcher call at that moment and cannot answer the resolution
query that launch triggers. Every launch that the agent does not start resolves normally, so
type at the terminal when you want to see a shadowed executable run.

### Scenarios

Application scenarios are versioned TOML under `boot/scenarios/`, run by `just dev-test`. The
interpreter is `boot/scenario.py` on the host, so nothing is staged in the guest and changing a
scenario or the runner costs no build at all.

A document is validated in full before its first step runs: the version, the step count, every
field's type, every payload size and every deadline, per step and total. There is no step that
hands a string to a shell, host or guest, and an unknown step or field is refused rather than
passed to whatever might understand it. The steps are `publish`, `input`, `key`, `pointer`,
`expect`, `absent`, `prompt`, `launch`, `output`, `finished`, `restored`, `reset` and `restart`.

`input` reaches the console over the control channel; `key` and `pointer` go through the
emulated devices instead, so they take the path a person's hand takes and are what to use when
the input stack itself is the subject. Fixtures are built by `boot/scenarios/make-fixtures.py`
from real staged artifacts, because the guest verifies the image it is given.

Every run tears its scope down whether it passed, failed or ran out of time, and then asks the
guest what it is still holding - a generation, a running program, a terminal that is not at a
prompt. Anything left is the run's failure even if every step passed.

### Self-tests

```sh
just dev-selftest   # three generations, one refusal, one rollback, one boot
just proto-test     # the control protocol's conformance suite
just perf-gate      # the loop's timing budgets, and that the work stayed proportional
```

### Where the logs are

- Guest serial: `.build/boot/dev-serial.log`, or `just dev-log` to follow it.
- QEMU's own output, including the build that produced the image: `.build/boot/dev-qemu.log`.
- Build and guest transcripts from test runs: `.build/logs/test`.
- One stderr file per artifact from the shared build: `.build/image/<target>/logs/<name>.stderr`.
  This is where a compile failure's real message is, and it survives the run.
- Timing samples: `.build/dev-baseline`. Setting `LIBER_TIMING_LOG=<file>` on a build or a test
  run appends machine-readable phase events to it.

### When something is wrong

`dev-status` reports exactly one state and exits zero only when the instance is both ready and
running current inputs:

- `down` - nothing running. `just dev-up`.
- `starting` - booting.
- `ready` - usable.
- `stale` - sockets left by an instance that is gone. `just dev-down` clears them.
- `detached` - the guest outlived its broker. `just dev-up` reconnects a new broker to the same
  guest rather than rebooting it.
- `foreign` - another worktree owns the profile; it names the owner and where to release it.

Other things that have bitten, and what they look like:

- `dev-up`'s timeout covers the build as well as the boot, and it defaults to 240 s. On a tree
  that needs a full rebuild - anything that changed the build tooling itself invalidates every
  artifact's cache key - the build alone can outlast it, and the failure reads as a serial
  socket that never appeared. Build first, or pass `--timeout`.
- A `dev-up` that fails part way leaves its QEMU running, and nothing reaps it: the lock goes
  away with the Python process, so `dev-status` reports `down` and `dev-down` finds no instance
  to stop. The next `dev-up` then fails with `Could not set up host forwarding rule`, which
  names the port and not the cause. `pgrep -af qemu-system` finds the orphan; check its command
  line mentions `dev-serial.sock` before killing it, so an unrelated guest is not the casualty.
- A guest that restarted under the tools is refused by every session, because the guest draws a
  value per boot and each session compares it. That is deliberate: it stops a tool publishing
  into a guest that is no longer the one it was talking to.
- A build that died partway can leave an artifact whose executable is proved but whose object is
  gone. The next build now says `<name> has no valid current ET_REL object; rebuilding it` and
  rebuilds it, which is the same work it would have done on a cache miss. This used to be fatal
  and unrepairable from outside; if you meet an older message about it, that is what changed.
  Deleting the artifact cache directory itself is still wrong - the builder writes into it
  without creating it and dies on the missing path. `just clean` is the supported way to discard
  build inputs.
- An interactive program left running wedges the scenario runner, because the guest reads as
  "starting" while an alternate screen is up. Quit it before the next run.
- Ctrl-C does nothing to a program in raw mode: the byte is delivered and no signal is raised.
  Escape or `q` is what ends the tree's interactive tools.

### What the build cache keeps

A tool that has not changed is not recompiled, and the decision is made per artifact rather than
for the tree as a whole. Each compiled object is stored under a digest of everything that went
into it: the source content, the compiler and its flags, the artifact's manifest row and the API
digest of every provider it links against. Beside it, `executable-<name>.object` names which
digest is the current one.

That is what makes skipping safe. A stale object cannot be reused, because reuse requires the
digest to match, and a matching digest means the inputs were identical - so the bytes are the
bytes that source produces. Change one byte of the source and the digest differs, and the tool
is rebuilt.

One generation is kept per artifact: the one the reference names. The build drops the others as
it replaces them, so undoing a change recompiles rather than restoring an older cached object.
`just dev-clean` sweeps up any backlog left by artifacts that have not been rebuilt since.

### Cleaning up

`just dev-clean [--dry-run]` prunes what the host accumulates: test logs, baseline samples,
scratch directories from builds whose process is gone, and sockets no instance owns. It keeps
the twenty newest runs and samples, reports what it removed, and leaves anything a running
instance still refers to. It deliberately does not touch the artifact caches or the staged
images: those are inputs to the next build, and `just clean` is what discards them.

## Timing inside the guest

- The shell's `time <command>` prints the wall time of any command, measured in
  the guest: `time cat /bin/console_service`.
- `boot/perf-trace.py` traces the console path on a fine-grained shared TSC
  timeline: the kernel and services emit `PERF` markers to the debug serial, and
  the tool prints a per-phase breakdown (shell produce, console render, gpu
  present) for one command. See its header for usage.
- For one-off profiling of a suspected stage, `rt`'s `clock_ns()` around the
  code in question plus a `print` is still the quickest probe; take it out
  again once the number is known.

## Kernel-level debugging (GDB)

```sh
just debug     # boot QEMU stopped, with a GDB stub on :1234 (KVM off)
just gdb       # in a second terminal: attach, symbols loaded automatically
```

A wedged live instance can also be inspected without a restart: attach with
`gdb -x boot/gdb-init` while the machine runs, `thread apply all bt` shows what
every vCPU is executing - this is how a userspace spin was pinned down to a
single syscall in the past (find the RIP, then `objdump` the user binaries to
name it).

## The test suite

`just test` (or `just lab test`, which also cleans up a stale volume first) runs
the in-kernel harness under QEMU; each test prints `[ok]` and the run exits zero
on success. One pass takes a few minutes. When a driver or service changed,
delete `boot/.build/virtio-blk.img` before the run - services are seeded onto
the volume only when the image is created, so a stale image runs stale binaries.
That stale-volume trap also applies to live boots; `just lab boot --fresh` is
the shortcut that avoids it.

The focused dynamic-link gate exercises a real provider DAG plus hostile provider
and canonical-order inputs without the full service integration workload:

```sh
just test-tags dynamic
just test-tags-aarch64 dynamic
just test-tags-riscv64 dynamic
```

## The gates, and what each one costs

The Justfile has 118 recipes, 65 of them checks or tests, and nothing says which to run before
releasing or how long any of them takes. Measured on the documented host:

| gate                      |   time | what it settles                                       |
| ------------------------- | -----: | ----------------------------------------------------- |
| `bootproto-host-test`     |  < 1 s | the shared boot contract's own unit tests             |
| `services-host-test`      |  < 1 s | service unit tests (also pulled in by any test run)   |
| `development-gate-check`  |    1 s | a shipping build contains no development units        |
| `volume-layout-check`     |    2 s | the built volume package matches the manifest         |
| `app-libs-test`           |    3 s | the application libraries' unit tests                 |
| `artifact-metadata-check` |    4 s | no stray identity or order files under the image      |
| `gen-check`               |    5 s | generated bindings match the interface definitions    |
| `image-conformance`       |    9 s | eleven codecs against reference implementations       |
| `fmt-check`               |   12 s | formatting, all languages                             |
| `source-hygiene`          |   18 s | tree hygiene and ownership rules                      |
| `shared-libs-verify`      |  406 s | the whole image rebuilt from nothing and re-audited   |
| `dynamic-report-check`    | ~500 s | the checked provider/consumer reports, three targets  |
| `static-image-check`      | ~700 s | static injection audits, three targets                |
| `fast-path-check`         |  ~20 m | targeted and authoritative builds produce equal bytes |
| `test-all`                |      - | the QEMU suites on x86_64, aarch64 and riscv64        |

What overlaps, so a release does not pay twice:

- `test-all` runs the kernel suite for all three architectures, so it contains `just test`,
  `just test-aarch64` and `just test-riscv64`.
- `test-tags-check` is a dependency of every preflight, and it in turn pulls
  `artifact-metadata-check` and `services-host-test`. Any test run has already paid for those.
- The six static-injection recipes are one script in six modes sharing four dependencies. Run
  together they build the three targets once; run separately, four times.

What looks like duplication and is not: the twelve `test-*-fast*` recipes differ from their plain
counterparts in one step, `test-preflight.sh check` rather than `write`. They verify the input
stamp instead of rebuilding it, which is what makes them fast and what makes them wrong to use
when userspace has actually changed.

Two ordering constraints are worth knowing before running anything:

- `development-gate-check` compares the built volume package against the shipping configuration,
  and `dev-up` builds the development one. After any use of the persistent instance it fails
  until the tree is rebuilt without `LIBER_DEVELOPMENT`. That is the gate working, not breaking.
- `image-conformance` cross-checks the JPEG codec against Pillow through whatever `python3`
  resolves to. `setup.sh` provides it as the `python3-pil` system package, so the failure mode
  to recognise is not a missing dependency but a shadowing one: a virtualenv earlier on `PATH`
  hides the system packages unless it was created with `--system-site-packages`.

## Reading what the machine did

- Serial log: `boot/.build/lab-serial.log` under the harness (`lab log`), or
  wherever `SERIAL=file:...` pointed a manual run.
- The system journal survives reboots on the volume: `log` in the guest shell,
  `log --boot <n>` for an earlier boot's records, `dmesg` for the kernel's line.
- The QEMU monitor (`lab monitor ...`) answers device-side questions:
  `info usernet` (SLIRP sockets and queues), `info virtio`, `screendump`,
  `sendkey`, `system_reset`.
