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

## Reading what the machine did

- Serial log: `boot/.build/lab-serial.log` under the harness (`lab log`), or
  wherever `SERIAL=file:...` pointed a manual run.
- The system journal survives reboots on the volume: `log` in the guest shell,
  `log --boot <n>` for an earlier boot's records, `dmesg` for the kernel's line.
- The QEMU monitor (`lab monitor ...`) answers device-side questions:
  `info usernet` (SLIRP sockets and queues), `info virtio`, `screendump`,
  `sendkey`, `system_reset`.
