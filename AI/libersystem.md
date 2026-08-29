# LiberSystem Repository Context

## Working Model
- Primary source tree: `src/`; there is no root Cargo workspace.
- Run checks from `src/`: `just fmt-check`, `just gen-check` and `just source-hygiene`.
- Use focused `just test-tags <tags>` runs.
- Prefer focused x86_64 QEMU tests.
- Run AArch64/RISC-V tests for architecture-specific behavior. Otherwise cross-build.
- `rg` is unavailable. Use `grep` or workspace search tools.
- The user runs `./commit.sh`; do not commit automatically.

## Stable Facts
- `src/user/services/manifest.toml` owns source paths and staged destinations.
- It also owns factory files and runtime paths.
- `init.pkg` contains boot-critical artifacts.
- `volume.pkg` contains the manifest-owned system volume.
- Package format remains `PKGARCH1`.
- x86_64 loads packages as boot modules. AArch64 and RISC-V embed them in the kernel.
- The manifest currently declares 78 sources, 61 libraries, 86 programs and 21 services.
- Shared images currently contain 61 providers and 70 dynamic consumers per architecture.
- `./check.sh --gate dynamic-report` validates all three target graphs; `(cd src && just dynamic-report-update)` rewrites them.
- Existing valid LiberFS volumes mount unchanged even when the factory archive changes.

## Build And Test Entry Points
- `tools/build-shared.sh` is the single shared-image builder for all three targets.
- `just dev-build <artifact> [target]` builds one manifest artifact and its closure only.
- `just dev-build --explain <artifact>` names the first invalidating edge.
- `just dev-baseline <cold|warm|leaf|provider> [tags]` records one measured sample.
- `just shared-cache-check [quick|provider]` validates cache invalidation behavior.
- `harness/test-preflight.sh <write|check> <arch>` stamps the non-kernel test inputs.
- The `test-*-fast` recipes verify that stamp instead of rebuilding userspace.
- `tools/source-path.sh` caches manifest source paths for lazy Justfile substitution.

## Known Baseline
- The long-standing `imgconv` failure was a wrong test, not a codec defect, and the WebP encoder
  needed no change. `imgconv_cross_volume_and_failed_overwrite_preserve_destination` bounded the
  RGB error of a 2x2 lossy WebP whose four pixels are red, green, blue and white. Lossy WebP is
  4:2:0, so a 2x2 image carries exactly one chroma sample - the average of all four - and those
  four average to 127.5 in every channel, which is neutral grey. The colour cannot survive, so
  the assertion asked for the arithmetically impossible and had never passed.
- Proved by encoding the same input with libwebp through Pillow: it produces the same greys
  (76, 150, 29 against our 77, 150, 29), so our encoder matches the reference to a rounding unit.
- The trap to avoid repeating: sweeping sizes 2, 4, 8, 16, 32 showed greyscale only at 2, which
  reads as "a bug specific to 2x2" and is the wrong conclusion. Only at 2 do four distinct
  colours share one chroma sample, so only there must the colour go. An independent encoder is
  what settles encoder questions; a size sweep is not.
- The assertion now bounds luma, which is full resolution and is what that size can carry.
- Do not change image code during unrelated work.
- Do not restore temporary mutations with `trap EXIT` in persistent terminals.
- Restore files explicitly before returning.
- Use `harness/test-kernel.sh <arch> <tags> --build-only` instead of standalone kernel `cargo check`.

## Active Work
- The current priority is the incremental build and persistent QEMU workflow in `docs/todo/`.
- Completed work includes targeted artifact builds and proportional cache validation.
- Cache decisions are explainable and publication is transactional.
- Package and image assembly is content-aware.
- The incremental build section of that document is now fully checked off.
- `llvm-objcopy --dump-section` given one file edits that file in place.
- Always name an explicit output when reading a section, otherwise verification republishes
  the artifact with identical bytes under a new inode and mtime.
- That silently defeats any cache keyed on stat rather than on content.
- Both build paths now carry a per-artifact state: a plan record plus a stat signature per
  input, settled against one batched `stat`.
- Warm full-graph verification is about 9 seconds and one leaf edit about 11.
- Continue with the persistent QEMU workflow, the next unchecked section there.
- Keep manifest rerun edges in `src/user/services/core/build.rs` current.
- Update its source-hygiene allowance when manifest-derived runtime paths change.
- The development-control protocol over the second virtio-serial port is delivered and
  ticked: codec, handshake, ping, failure taxonomy, publication, registry queries, rollback,
  terminal input, reset, launch/output/stop. Scenario launch/stop/result were answered by the
  host-side interpreter instead; terminal output as a protocol event is the one thing unbuilt.
- The protocol lives in `dev_protocol.rs` behind a byte stream and a `Sink`, not in the
  driver, so the development agent can host it on a channel without a rewrite.
- Publication verifies length and digest at commit, against `bootproto::sha256`, the same
  digest the build system and the loader already speak.
- The guest holds published bytes but installs nothing; the registry that installs them is
  a later roadmap item.
- Its memory is capped by construction: one candidate and four generations of at most a
  megabyte each, all reported in the handshake.
- Terminal input and guest-state reset are delivered; terminal output is not.
- Output would need a second tap in `console_service`, and its consumer is the development
  agent, so wiring it to the driver now would build the coupling that the split above exists
  to avoid.
- `SYS_CONSOLE_FEED` now takes a second argument choosing the serial arrival path and
  returns whether the console took the byte.
- The serial path matters because the console service drops keystrokes while its display is
  unfocused, and a driven guest has no display to focus.
- No ABI bump: the argument and the return were both an ignored zero before.
- The console input queue is short - measured at 41 bytes before it refused.
- So terminal input must report how much landed and the host must resume from that count;
  anything else silently loses most of a pasted line.
- Provider compatibility is decided by `bootproto::compat`, host-tested with
  `just bootproto-host-test`.
- It reads two images only, so the guest, the build system and any audit tool agree.
- Read an image's machine from its header before parsing it; `Elf::parse` uses the running
  architecture, so a valid image for another target otherwise reads as a broken file.
- Match exports with a resuming walk, not a rescan per symbol: a rebuild emits its dynamic
  symbols in the same order with new ones appended.
- On `lsrt` (672 exports) that is 8 ms instead of 810 ms.
- Publication now verifies the image, so any test fixture that expects a commit must be a
  real staged `.lslib` published under the artifact name its identity record carries.
- Synthetic bytes are still fine for cases expected to be refused before verification.
- The whole staged library set is 2.7 MB, so the 64 MB registry budget cannot be reached
  with real artifacts and its refusal path is asserted by construction, not exercised.
- The registry is memory the driver holds; it never touches the system volume, so a reboot
  returns the guest to its built state and a publication cannot damage a cold boot.
- `SYS_BOOT_PROFILE` lets userspace ask which profile the firmware selected.
- The registry is gated on it, but the protocol is not: the same channel is present in the
  cold test configuration of all three targets, where a runner must still handshake, ping,
  type and reset without being allowed to hot-publish.
- The gate's negative side cannot be reached through the facility itself, so it is a kernel
  test asserting a boot that named no profile reports none.
- An ordinary interactive `lab boot` attaches no development channel at all; only the
  persistent development profile and the cold test configurations do.
- The registry now lives in `dev_agent`, its own process; `dev_channel` is a byte pipe.
- DeviceManager starts the agent when it binds the device, handing it a bootstrap and
  transferring the driver's byte channel over it.
- Never report in on that byte channel: it is the wire, so anything sent there is written
  straight out of the port as unframed noise.
- A static program staged to the volume must be `role = "service"` or `"driver"` with
  `linkage = "static"`; dynamic linkage needs ProcessService, which DeviceManager's plain
  `spawn` does not use, and `pinned` would put a development-only program in the init
  package.
- `kernel/build.rs` stages `"driver" | "service"` at volume stage; a static volume service
  had no precedent before `dev_agent`.
- Any change to `rt` shifts recorded sizes in `docs/DYNAMIC_*.tsv`; regenerate with
  `just dynamic-report-update` and check the diff is sizes only.
- The compatibility verdict is recorded against the INSTALLED artifact on the volume, not
  against the generation published before it; that is the comparison a launch will make.
- A publication is refused unless the installed manifest declares its name, so the registry
  can shadow what the system already has and nothing else.
- A volume client is a request and reply channel: give a second reader its own connection
  with `service_connect`, never a `duplicate` of the handle, or the two take each other's
  replies.
- `ProcessService::identity_matches_dependencies` compares each module against the digest of
  the INSTALLED image at that name, not the loaded one, so a proven-compatible generation
  passes an unchanged comparison and the compatibility argument is made once per module.
- A transmit deadline shorter than the session's own idle deadline drops replies from a host
  that is merely slow to read; both deadlines answer the same question, so they match.
- Pipelining is reliable at the advertised 16 outstanding and not beyond it: 16 maximum
  frames is already about a megabyte of replies, more than a socket buffer holds, so a host
  past the bound deadlocks against its own buffer and no guest can rescue it.
- A host that pipelines must read while it writes; sending a whole burst first is not
  pipelining.
- Terminal output over the protocol is still missing and wants a console-hosted PTY for the
  agent, not a tap on the foreground VT's output.
- `PTY_OPEN` already exists but is requested over a VT control channel, which the agent does
  not have, so it needs a console client routed to it the same way its volume client was.
- ConsoleService has no way to mint a control channel for a program it did not start, so the
  agent's PTY needs that capability added before the routing is worth doing.
- Every development socket is owner-only: `umask 0077` before QEMU covers the ones it makes
  in all three cold test configurations, and the broker chmods the two it binds itself.
- Narrow a socket where it is created, never from the parent after forking the broker: the
  chmod would race a bind that has not happened.
- The guest draws a per-boot value and reports it in every handshake; `dev-up` records it and
  every session compares, so a tool cannot publish into a guest that restarted under it.
- A host whose predecessor died mid-frame has its first handshake swallowed as that
  fragment's payload, so the client retries until the guest's fragment deadline clears it.
- Fuzzing found that, not a review: random framed headers leave the guest mid-payload, which
  is exactly the state a crashed publisher leaves behind.
- The development units are gated at compile time, not at runtime: `required-features` on the
  two binaries, `development = true` in the manifest, and `kernel/build.rs` skipping such
  programs unless its own feature is on.
- `LIBER_DEVELOPMENT=1` selects it and only `dev-up` sets it; `just development-gate-check`
  proves the shipping configuration builds and stages neither binary.
- The expected volume-entry set is derived in two places, so `volume_destinations` takes the
  configuration; staging one set and checking against the other reads like corruption.
- Switching between the development and shipping configurations rebuilds those crates, which
  is the price of the units being absent rather than disabled.
- The agent cannot be killed from outside: DeviceManager spawns it, so `ps` (ProcessService)
  never lists it, and no protocol input faults it - so agent restart stays untested.
- The driver exits when the agent's channel closes, so the agent's death takes the port with
  it; recovery needs DeviceManager to watch the agent's bootstrap and hand the driver a fresh
  channel.
- Ask the guest about the installed volume rather than the host's staged tree when proving
  the registry touched nothing; the host tree is not what the guest reads.
- A test that types into the guest must send Ctrl-C first: an earlier test that typed without
  a newline leaves its characters in the shell's line editor and they prefix the command.
- Do not drive build-script behaviour from a cargo feature at all. `cfg!(feature = ...)` is
  silently false there, and `CARGO_FEATURE_<NAME>` reads correctly but cannot be declared with
  `rerun-if-env-changed`, so a script that emits `rerun-if-changed` reuses its old output when
  the feature flips.
- Use a plain variable (`LIBER_DEVELOPMENT`) and declare it; that is what the kernel does.
- Both failure modes are silent and leave every assertion inside the build agreeing with
  itself while the image is the wrong configuration.
- A gate check that only inspects crates cannot see that, so `check-development-gate.sh` also
  compares the built volume package against the configuration that was requested.
- Scenarios are versioned TOML data under `src/harness/scenarios/`, run by `just dev-test`;
  the interpreter is `src/harness/scenario.py` on the host, so nothing is staged in the guest
  and changing either a scenario or the runner costs no build at all.
- The format has no step that hands a string to a shell, and an unknown step or field is
  refused before the first step runs.
- `lab.py` filtered options by their leading dashes, which left `--timeout`'s value behind as
  a positional; use `take_arg`, which removes both.
- A launch goes through `security::permission::Client::run(name, args, cwd, stdout)`; the
  agent passes a channel as stdout and reads the program's own output from it.
- The launcher is delivered late: ServiceManager sends it to DeviceManager's control channel
  when PermissionManager starts, and DeviceManager forwards it to the agent's bootstrap.
- That bootstrap is now kept rather than dropped, and the agent waits on the wire, the
  bootstrap and any launch output together.
- A closed channel is permanently ready: never keep it in a `wait_any` set, or the loop spins
  and starves the cooperative scheduler. That bug appeared three times now, in three programs.
- Its third form: detecting a dead peer by peeking for a message. A closed channel reports
  nothing to read, so the peek finds nothing, the wait returns at once and the loop spins. Use
  `try_recv` and observe `Polled::Closed` where it happens.
- `manifest_for` in PermissionManager is an explicit table; a program declared in the system
  manifest but absent there cannot be launched (`echo` is one).
- ProcessService resolves executables and providers from the registry at the one point it
  turns a name into bytes; a provider generation must additionally pass `bootproto::compat`
  against the installed image, decided by the loader rather than trusted from the publication.
- Publication records a verdict, it does not gate: the registry holds incompatible generations
  too, so the refusal has to happen at the launch that would load one.
- A refused provider fails the launch instead of falling back to the installed image; falling
  back would run something other than what was published while looking like success.
- No compatibility rule applies to an executable and none is missing - nothing has resolved
  against a program that has not started yet.
- One unanswered registry query ends the questioning for the rest of that launch: a launch asks
  once per provider, so an agent gone quiet would otherwise cost the timeout on every one.
- A governed launch that fails prints nothing: the shell recognised the command, so its
  unknown-command line never appears. Assert such a refusal as absent output.
- `process-proto` renders what `ps` prints, so publishing it changes an unchanged program's
  output - the cheapest proof that a provider, and only a provider, was replaced.
- A fixture patched in place keeps its identity record, so its digest still compares equal and
  the compatibility rule is never consulted; alter `source-sha256` too or the test tests
  nothing.
- A string can live in an image twice, in rodata and as an instruction immediate; a fixture
  builder must state the occurrence count it expects rather than patch the first one it finds.
- Make a channel before either end exists when two services come up in an order neither
  controls: ServiceManager makes the pair, one end goes out at bootstrap, the other later.
- Never query a channel whose peer may not exist: have the peer announce itself first, or the
  query blocks the service every launch goes through and the boot never finishes.
- A request/reply channel with a timeout must drop anything queued before asking; an
  abandoned reply otherwise leaves it one answer behind forever, which looks like it works.
- `wait` on a channel does not help if the reply already arrived: peek first, then wait.
- A launch the agent itself starts cannot be shadowed - it is inside the launcher call and
  cannot answer its own resolution query.
- DeviceManager supervises the development agent because it started it: the agent's bootstrap
  closing is the death notice, and a replacement gets a fresh wire handed to the driver.
- Capabilities delivered once, after a program is already running, must be retained by whoever
  forwards them, or a replacement of that program silently has fewer than the original.
- Hand a duplicate, keep the original: only one agent is alive at a time, so duplicating a
  channel endpoint per generation of the process is sound.
- A per-boot value drawn by a process that can be replaced announces a reboot that never
  happened; draw it in something whose life is the boot and hand it down.
- The dev agent's registry does not survive its restart - it was that process's memory. That
  is the operation, and `reset` is the smaller tool.
- `just dev-restart` replaces the agent and waits for the replacement's console report before
  writing to the port; asking sooner splits a handshake across the driver's discard window and
  costs several seconds of fragment deadline.
- `just proto-test [group...]` is the protocol conformance suite (79 cases, seven groups)
  against a running instance; `harness/proto-test.py` imports `lab.py` for sockets and helpers.
- The dev-protocol session belongs to the PORT, not to a host connection: reconnecting does not
  start a new session or reset the request watermark. Reaching a closed session means waiting
  out the idle deadline.
- The registry budget cannot be exhausted from the protocol: 32 MB per artifact, only verified
  images retained, and the whole staged library set is a few megabytes.
- A test fixture derived by patching an identity record should move `rustc-commit` (always
  present, fixed 40 hex chars); `features` is `-` for some artifacts and unusable.
- A wrong-target image is cheaper to derive than to stage: patch e_machine at offset 18.
- Terminal input over the control protocol reaches the console directly and proves nothing
  about the input stack; `key`/`pointer` steps inject into the emulated devices instead.
- The HMP monitor has `sendkey` and no pointer command; QMP `input-send-event` does both, and
  both sockets exist for every QEMU run including the development instance.
- The guest's pointer is a `virtio-tablet-pci`, so pointer positions are absolute over QEMU's
  0..32767 axis range; scenarios name a fraction of the screen instead.
- `serial_since` strips ANSI, so terminal restoration can only be asserted on the raw bytes;
  `serial_raw_since` exists for that.
- `licoview` redraws only when the wheel actually moves its position, which is what makes a
  second banner a usable assertion that a pointer event arrived.
- Leaving an interactive program running wedges the scenario runner: `dev_state` looks for a
  prompt, so an alternate screen reads as "starting". Quit it before the next run.
- ServiceManager's admin channel already stops a named service and its dependent closure; the
  shell holds the channel but only ever sends the power verbs. The governed `stop` tool is the
  way in, so a scenario can disconnect a service with the `launch` step it already has.
- A service is restartable exactly when it is reachable again, which is four properties: the
  supervisor holds its serve root, it knows how to re-run its bootstrap, the service serves
  `serve_multi` (a factory, so connections can be minted), and its clients resolve by name.
  `restartable()` in ServiceManager is the list; it grows with the broker and not before it.
- A service served with `serve` instead of `serve_multi` cannot be resolved: `service_connect`
  on its root blocks forever waiting for a CONNECT_OP reply nobody serves. That hangs the
  caller, which is what a shell command looked like when the migration was half done.
- `system_graph_service` is the only service whose stop closure is itself (only the shell
  depends on it, and the shell is exempt), which is why it is the one a scenario can stop.
- The admin protocol: bare name stops, `+name` starts, `!poweroff` / `!reboot` are the power
  verbs. `start` is a governed tool beside `stop` with the same single grant.
- The `restored` scenario step asserts terminal modes came back IN ORDER, matching the kernel
  harness. Real order on the way out: mouse, raw, cursor, screen.
- An interactive tool (`Shape::InteractiveArgs`) is only routed when the line carries an
  argument: a bare `lico` is an unknown command to the shell.
- `vol://system/hello.txt` is on the real volume and is the same file the kernel harnesses
  open, so a scenario and a harness can assert the same content.
- Every cold test configuration already attaches the control channel on its own socket
  (`dev-channel-<target>-test.sock`); a cold cross-target scenario run needs only the runner
  to point at one. Publication cannot follow: a cold boot is not the development profile.
- Governed tools do not run in a bounded Domain, so `usage` counters do not move whatever a
  scenario does. There is no resource baseline to check until that changes.
- `launch-bounded` now makes a Domain per launch and drops its handle: a process holds its
  Domain (`Process { domain: Arc<Domain> }`), so it lives exactly as long as what it accounts.
  The limit is per process, which is what `imgconv`'s measured single-run peak assumed.
- A per-launch Domain is invisible to ResourceManager, which reports only Domains it was given,
  and only tools with a stated limit get one - so `usage` still cannot see a scenario's scope.
- Reporting per-launch Domains fights their lifetime: ResourceManager holding a handle to one
  would keep it alive past its process, which is the property that makes it self-reaping.
  Pick one. The run-level question ("did this run leave anything") is answered at process and
  registry level instead, which is what the scenario teardown checks.
- `just dev-loop <artifact> <scenario...>`: build, publish, run, stopping at the failing phase.
  Warm 3.7 s, cold 8.2 s; only the build phase is cached.
- `just dev-clean [--dry-run]` prunes test logs, baseline samples, dead builds' scratch and
  stale sockets. It never touches the artifact caches or staged images - those are build
  inputs, and `just clean` is what discards them.
- SOLVED, and the note that used to be here was wrong about the cause. The builder's
  "intermittent" object/ELF-kind failures were `llvm-readelf ... | grep -q ...` under `pipefail`:
  `grep -q` exits at its first match and closes the pipe, the llvm tools report the resulting
  EPIPE as exit 74, and `pipefail` makes that the pipeline's status, so a successful match read
  as a failed read. Measured 1 failure in 2000 idle and 377 in 2000 with the host busy, with
  `grep` returning 0 every time. 28 such pipelines across eight scripts now match against
  captured output; the same measurement afterwards is 0 in 2000. `source-hygiene` rejects the
  construct. Four negative checks are written out rather than routed through the helper, because
  there a failed read must not be indistinguishable from a clean result - one of them, the
  forbidden dynamic-loader gate, had been silently PASSING what it exists to catch.
- The authoritative rebuild is reliable now and `just fast-path-check` passes: `uname`,
  `base-proto` and `lsrt` byte-identical across two independent runs.
- `dev-build <library>` with a bare name never worked: the kind was resolved with
  `.programs[$name] | select(...)`, and a jq `if` whose condition emits nothing produces nothing,
  so the `elif` naming the library was unreachable and all 61 libraries read as unknown. Test the
  fields directly. The `.lslib` suffix takes another branch and always worked, which is why the
  earlier `pix.lslib` validation passed over it.
- A build that dies partway used to leave an artifact whose executable was proved beside an
  object that was gone, and refused it forever. It repairs itself now: rebuilding that object is
  the same work the miss path does. Do NOT delete the artifact cache directory itself -
  build-shared writes into it without creating it and dies on a missing path.
- One object generation is kept per artifact, the one `executable-<name>.object` names; the rest
  are dropped as they are replaced, and `dev-clean` sweeps any backlog. Before that, aarch64 and
  riscv64 held about 18 generations per tool.
- Every scratch file the builder makes goes in `.build/tmp`, so a leak is a directory that should
  have been empty. The leak that found this: the exit path dropped the object inputs and the
  source inventory but not the identity record in flight, so every build that died between
  creating one and finishing with it left one behind - thirty over five days.
- Never pass a relative `CARGO_TARGET_DIR` to a command that cds first: it resolves against the
  new directory and drops a 200 MB build tree inside the source tree.
- Every `##` work section in `docs/todo/M*.md` carries the milestone identifier
  (`## M0134e - Gates and completion`), so every group of checkboxes is addressable. Applied
  retroactively to M0119 and M0120. Two exceptions with reasons: M0035's `##` is a container
  whose `### M0035a-k` children carry the identifiers, and TODO.md is not a milestone document
  (its Phase 0/1/2 are the project phases that contain milestones).
- The scenario runner is `just dev-test` (it was `dev-scenario`).
- `proto_session` warns once per process when the instance predates the tree, naming the input
  class and the recovery. `LIBER_DEV_INPUTS_CHECKED=1` suppresses it for child processes, which
  is how `dev-loop` pays the ~235 ms comparison once instead of per phase.
- `instance_inputs()` hashes content, not mtimes: `touch` on a kernel source changes nothing.
- `just gen` and `just gen-check` wrote and compared `../..//src/...`: the recipes `cd` into
  the generator first and only then expand `$(tools/source-path.sh …)`, which is not reachable
  from there. Every path collapsed, so gen wrote litter and gen-check compared nothing. Fixed
  by resolving the output path before the cd.
- `imgconv` cannot convert the installed 174 kB wallpaper inside its 96 MB Domain; that
  predates the Domain change and the limit wants revisiting.
- The scenario runner asks the GUEST what it still holds after teardown (registry, launch,
  prompt) and fails the run if anything is left, passing steps or not.
- `scenario.run` takes an injected guest, which is how the teardown enforcement is tested:
  subclass `LabGuest` with a `stop_launch` that does nothing and the run must report it.
- Ctrl+C does nothing to a program in raw mode - the byte is delivered, no signal is raised.
  Escape or `q` is what ends the tree's interactive tools; the scenario teardown escalates.
- `OP_LAUNCH_STOP` ends an agent-launched program; its output stays readable afterwards.
- `exit()` carries no status anywhere in the system, so there is no exit code to assert.
- A control driver must block on its device interrupt, never poll with `yield_now`.
- An earlier polling version handshook and pinged correctly but the guest never reached a
  shell prompt, because a runnable spinner starves the cooperative scheduler.
- QEMU stops consuming a virtio-serial transmit queue when the host end backs up, rather
  than discarding what it cannot deliver.
- A polled `Queue::submit` that gives up therefore leaves the device owning a descriptor
  that the next write overwrites, which corrupts the ring for the rest of the guest's life.
- Transmit on such a port must be async: reap completions and never refill the buffer until
  the device returns it, so the port recovers by itself when the host reads again.
- A virtio-serial port without MULTIPORT reports no open and no close, so the guest cannot
  see a host disconnect.
- Bound the session by silence instead: a handshake opens it and an idle deadline closes it,
  which makes a crash and an orderly exit end the same way.
- The dev-channel driver is exercised on x86_64 only; the aarch64 and riscv64 smoke suites
  run the kernel test binary, which never launches DeviceManager.
- `just run-aarch64` cannot boot the full system in this environment: `qemu_append_audio`
  hits a bash nameref collision and `virtio-sound-pci` is then given a missing `audiodev`.
- That failure predates this work and is unrelated to it.
- A full forced rebuild is 406 s at 5 to 7 percent of 52 cores. Sampled every 200 ms, `rustc`
  runs in 229 of 300 samples, so it is compile-bound: one artifact at a time, and a small crate's
  compile is single-threaded (0.36 s wall against 0.39 s CPU). A warm leaf iteration is the
  opposite balance - 15 percent compile, the rest proof. Do not read one figure for the other.
- Parallelising the existing per-artifact `cargo rustc` calls is nearly worthless: they share one
  `CARGO_TARGET_DIR` and cargo locks it. Four at once measured 1.57x, eight at once 1.34x - the
  gain shrinks as contention grows.
- `cargo rustc --bins` is refused: extra rustc arguments need a single selected target. That is
  why 55 separate invocations exist.
- `RUSTFLAGS="--emit=obj,link"` plus `cargo build --bins --keep-going` does emit one object per
  binary under cargo's own scheduler, but applied to this build it dies at `Cargo image graph did
  not stop after emitting its ET_REL seed object`: RUSTFLAGS reaches the graph step, which
  harvests its seed by controlling exactly what rustc emits. The emit contract is already spoken
  for, so adopting cargo's scheduler means reworking the graph and seed step too.
- The build is deterministic: two forced rebuilds of the same source give identical bytes. A
  staged artifact embeds its crate's source digest, so a probe comment changes the binary even
  though a comment cannot reach the code.
- `dev-status` used to report `foreign` during bring-up, because the lock is taken before the
  identity is written. A failed `dev-up` also left its QEMU running with nobody to reap it, and
  the next attempt then failed on a host forwarding rule that named a port and not a cause. Both
  fixed; `dev-up` also has a separate build budget, since its timeout used to cover the build.
- `just dev-reboot` is the fast recovery for corrupted guest state: 5.9 s to a prompt, instance
  and volume survive. No QMP snapshot - `savevm` needs qcow2 and every drive here is raw, and a
  restored snapshot would hide the persistence bugs a reboot leaves visible.
- Three gates are red or conditional on a clean tree. `app-libs-test` fails
  `converts_opaque_bmp_to_explicit_indexed_png`, and that test has NEVER passed: it asks for an
  exact round-trip through `--quality 0`, whose palette budget is `16 + quality * 240 / 100` = 16
  colours, from a fixture with 21 distinct colours. Test and formula arrived in the same commit
  (aa0e3b7, 2026-07-16) and neither the fixture nor the formula has changed since, so it was
  broken from birth and nothing ran it. It is NOT the WebP failure recorded above. Second,
  `image-conformance` fails when a virtualenv earlier on `PATH` shadows the system `python3`:
  `setup.sh` installs Pillow as `python3-pil`, and a venv without `--system-site-packages` hides
  it. The repo's `.venv` exists only for `git-filter-repo` and did exactly that. Third,
  `development-gate-check` fails after any `dev-up` until the tree is rebuilt in the shipping
  configuration, which is the gate working rather than failing.
- That a test can sit broken for thirteen days is the argument for a single release command: the
  gates that would have caught it are ones no routine flow invokes.
- Markdown is not formatted by `format.sh` - it covers `.rs`, `.sh`, `.toml` and `src/Justfile`
  only. Do not run prettier over the docs; it reflows whole files and buries the real change.
- The kernel test harness must send ProcessService a `REGISTRY` capability. It receives that
  handle unconditionally by design - the channel is made before either end exists so the service
  never learns about a capability arriving late - so a caller that omits it blocks the service in
  its bootstrap, before its serve loop, and every launch behind it waits on a reply that cannot
  come. The harness omitted it in all four places it starts ProcessService.
- What that looked like from the outside is the lesson: the suite reported "PermissionManager did
  not request a fresh NetworkService client", three layers downstream, because PermissionManager
  stalled on the first of five components and never reached the fifth, which is the only one
  granted network. A test reports the first expectation that failed, not the place that broke.
  Three rounds of tracing walked it back; reading the message literally sends you to the network
  stack, where nothing is wrong.
- Adding a capability to a service's bootstrap is a change to every caller that starts it, and
  one of those callers is a test harness no ordinary run exercises.
- Fixing that stall revealed a second permission test that had never been reachable, failing on
  "imgview did not present after arrow-key pan". That one is untouched and is its own subject.
- The kernel suite stops at the first failing test, so one red test hides every test after it. On
  2026-07-29 that chain was four deep: a WebP assertion that had never passed hid an `imgview`
  scenario that contradicted its own specification, which hid page-count constants stale since
  2026-07-28, which hid an xHCI driver report-in failure. Fixing the first took the x86_64 run
  from 59 tests to 96. Expect a fix here to reveal work rather than finish it.
- The wave page counts in `test_suites/dynamic.rs` are hardcoded per architecture and nothing
  keeps them in step with the images, unlike the `docs/DYNAMIC_*.tsv` reports that
  `just dynamic-report-update` regenerates. Re-measured 2026-07-29 on all three targets. The
  drift has no single direction: aarch64 moved every representative one page from writable to
  immutable, x86_64 moved two tools in mixed directions, riscv64 gained a writable page on two.
  To measure them, print `private_image_pages()`/`shared_image_pages()` before the assertions -
  the perf line comes after them, so a failing run never shows the numbers you need.
- `imgview` panning is continuous while a key is held, so a harness that presses without
  releasing keeps receiving presents, and how many arrive before the next step depends on target
  speed - emulated riscv64 is about twenty-five times slower than x86_64 here. Release the key,
  and let the wait for the surface release tolerate presents still in flight.
- M0122 already specifies that arrow keys must not pan when the whole image fits the viewport;
  panning is enabled only after explicit zoom. A kernel scenario asserted the opposite for weeks.
  Check the milestone that owns a behaviour before assuming a failing test found a bug.
- OPEN DEFECT, real production code and the only x86_64 failure left: the xhci driver enumerates
  the bus correctly, prints `driver.xhci: online (4 device(s)) (keyboard) (pointer) (storage)` -
  exactly the string the test asserts - and then takes a ring-3 page fault, code 0x6, CR2 = 0x0 at
  0x20173f, so the report never reaches the channel. The test says "the xhci driver should report
  in: Empty", which is the opposite of what happened and sends the reader looking for a driver
  that failed to start. Recorded in M0062. Not yet done: map 0x20173f to a symbol and read what
  runs between the report and `service_loop`.
- Separate landmine in the same place: that report is assembled into a `[u8; 64]` and the maximum
  content is exactly 64 bytes. A two-digit device count overruns it - an index panic, not this
  page fault, so it is not the current bug, but ten USB devices will find it.
- Read the driver's console output before believing a "did not report in" style failure. Twice
  today a message named the wrong subject: this one, and "PermissionManager did not request a
  fresh NetworkService client" which was really ProcessService blocked in its bootstrap.
