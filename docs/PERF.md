# Performance notes

Measured numbers for the changes whose goal includes a before/after
comparison. Methodology per entry; machine noise applies, so treat the times as
orders, not precision instruments.

## Application surface presentation (2026-07-14)

Measured by the tagged x86 KVM display test (`cd src && just test-tags display`).
The real userspace DisplayService drives a stand-in virtio-gpu channel with the same
synchronous `PRESENT` / `OK` protocol as the driver. Its private typed counters read
`SYS_CLOCK_MONO_NS` around (a) the CPU blit/scale and (b) the driver transfer+flush
acknowledgement. The benchmark scales a Doom-class 320x200 B8G8R8X8 surface into a
1024x768 scanout (1024x640 output, centered) and then presents a 32x20 source damage
rectangle. Two debug-profile KVM runs establish the unoptimized range; the final column
optimizes only the small shared `libpix` dependency at opt-level 2 while retaining debug
information and unoptimized service control flow.

| scenario | debug baseline | incremental damage, debug | incremental damage + optimized `libpix` |
| --- | --- | --- | --- |
| CPU blit/scale | 234-252 ms | 2.37-2.40 ms | 0.085 ms |
| synchronous driver ACK | 0.045-0.065 ms | 0.028 ms | 0.018-0.033 ms |
| scanout pixels written | 1,441,792 | 6,592 | 6,592 |

The final full first frame is 8.41 ms, below the approximately 28 ms end-to-end budget
for 35 FPS before application rendering is counted. Incremental scaled damage maps source
bounds conservatively with floor/ceil and is 27.9x faster than its debug equivalent;
compared with the old full-scanout behavior it writes 218x fewer pixels. A new surface's
first present still clears/copies the full frame, regardless of the submitted damage, so
pixels from the previous foreground client cannot leak outside a small first rectangle.
Scanout resize invalidates this initialized state and forces another full safe repaint.

Build-profile result: optimizing the whole `services` package was rejected because its
test boot fell back from the 4x4 stand-in GPU backing to the boot framebuffer. Isolating
the hot loop in host-tested `libpix` preserved behavior and changed the debug
`display_service` ELF from 4,314,224 to 4,315,888 bytes (+1,664 B). Current comparison
sizes (debug information included) are: ConsoleService 5,641,624 B, shell 5,470,632 B,
and the governed graphics grant probe 3,653,528 B. These numbers reinforce the later
shared-library/image-size work; they are not stripped deployment sizes.

QEMU caveat: the stand-in ACK isolates CPU work and IPC scheduling but does not model a
real host display refresh. Even a live virtio-gpu resource-flush acknowledgement means
the host accepted the command, not that a VNC/SPICE client visibly scanned the pixel.
Treat the latency as a regression metric and budget gate, not a physical-GPU prediction.

## Application library factoring and startup (2026-07-14)

The first application-side libraries are single-concern no_std crates with real standing
consumers: `libpix` (pixel vocabulary and bounded blitters, used by DisplayService),
`libsurface` (typed DisplayService client plus RAII MemoryObject mapping, used by
ConsoleService), `libkeys` (canonical HID usages and held-key edge state, used by
InputService), and `libpcm` (format/frame validation, little-endian sample decoding,
mono expansion and rate phase, used by AudioService). Pure helpers run nine host tests
through `just app-libs-test`; libsurface lifecycle is exercised by the live console and
display tagged tests.

Cold start is measured in the permission integration scenario with the guest monotonic
clock: immediately before `permission.run("graphics_probe")` sends its request until the
governed process receives its process-bound display, key-only input and playback-only
audio grants and writes its first stdout message. One x86 KVM debug-profile run measured
1.347 ms. This includes ProcessService volume loading, ELF spawn, PermissionManager admin
mint/bind calls, bootstrap transfers, entrypoint and first IPC output; it excludes shell
parsing and terminal presentation.

Representative ELF sizes compare the ordinary debug staged profile (debug information,
mostly opt-level 0) with Cargo release builds. This is a build-profile decision aid, not
an on-disk package measurement; release binaries are not yet what `just user` stages.

| binary | debug ELF | release ELF | reduction |
| --- | ---: | ---: | ---: |
| DisplayService | 4,315,904 B | 45,920 B | 98.9% |
| ConsoleService | 5,691,848 B | 204,096 B | 96.4% |
| InputService | 4,394,512 B | 35,904 B | 99.2% |
| AudioService | 4,383,920 B | 39,176 B | 99.1% |
| shell | 5,470,632 B | 146,528 B | 97.3% |
| graphics grant probe | 3,653,528 B | 20,208 B | 99.4% |
| **total** | **27,910,344 B** | **491,832 B** | **98.2%** |

The profile win is already two orders of magnitude, so a stripped/release staged-image
profile should be measured before paying the loader/ABI cost of dynamic linking. Later
shared-library work still measures aggregate image and resident-memory sharing: static release binaries may
remain the better choice for small tools, while duplicated runtime/protocol text across
many concurrent processes can still justify `liblsrt`/`libproto` sharing.

## Kernel wake path (2026-07-06)

Measured live in QEMU/KVM as the end-to-end round-trip of a shell command typed
over serial (the lab harness sends the line and waits for the prompt to return;
wall clock on the host, five runs). Before = the tree at HEAD (serial input
polled from the 100 Hz idle hook, one global waiter list, no cross-core kick);
after = this change (UART receive interrupt, per-object wait buckets, the
remote-spawn wake IPI). The in-guest `time uname` (~5 ms) is unchanged - the
spawn pipeline was never the bottleneck; the win is the input-delivery path.

| scenario | before | after |
| --- | --- | --- |
| serial command round-trip (`uname`, end to end) | 182-197 ms | 122-133 ms |
| remote spawn onto a halted core | up to one 10 ms tick | < 4 ms bound, test-pinned (microseconds typical) |

The remaining ~120 ms floor is dominated by the console output path (echo and
present quantization), not input delivery - the serial byte now reaches the
shell's waiter in interrupt context.

## Contiguous DMA and full-size I/O (2026-07-05)

Measured live in QEMU/KVM with the shell's `time` over serial: a whole-file read
of a 5.2 MB file from the LiberFS system volume (`time cat /bin/console_service`,
virtio-blk), and a 4 MB HTTP fetch from a host-side server printed to the console
(`time tcp 10.0.2.2 8888`, virtio-net + the TCP stack). Before = the tree at
HEAD (per-page DMA, 16-descriptor rings, one-sector block requests, MSS-less
TCP); after = this change.

| scenario | before | after |
| --- | --- | --- |
| 5.2 MB file read (virtio-blk, LiberFS) | 115 ms | 54 ms |
| 4 MB TCP bulk fetch | stalls (never completes) | 1.46 s (~2.9 MB/s incl. console rendering) |

The disk read halves: extent-sized block requests (a contiguous extent = one
request) ride the driver's whole-span virtio-blk chains over contiguous DMA
buffers, so a large `cat` is a handful of device round-trips instead of one per
sector. The TCP "before" is honest: bulk receive at HEAD hit a latent stack bug
(the padding of a minimum-size Ethernet frame counted as TCP payload, advancing
`rcv_nxt` past data the peer had not sent, so the transfer wedged on the first
bare ACK) - it went unnoticed while our optionless SYN kept the peer's segments
small and ACKs piggybacked. The MSS option added here surfaced it; the fix
(trim the frame to the IP total length) plus window scaling gives the working
number above.

## LiberFS format and modernity (2026-07-02)

Same benchmark as the allocator/free-map entry below. The CRC32C rewrite (slice-by-8, previously byte-at-a-time)
and the LZ4 codec (previously LZSS) move the CPU side; compression now defaults
OFF, so the incompressible-write benchmark no longer pays a futile compression
pass at all.

| scenario | after allocator rework | after format rework |
| --- | --- | --- |
| 64 MB write | 1.72 s | 137 ms (and 19 reads - the source-verify reads belonged to the compression pass) |
| 64 MB sequential read | 204 ms | 67 ms |
| 2000 small files | 503 ms | 223 ms |
| 2000 stats | 164 ms | 46 ms |

The host test-suite run also fell from ~82 s to ~0.4 s (the CRC dominated the
unoptimized debug profile; the crate now tests with opt-level 2).

## LiberFS allocator and free-map scaling (2026-07-02)

Benchmark: `cd src/fs/liberfs && cargo test --release bench_scaling -- --ignored --nocapture`
(a 1 GB sparse RAM-backed volume; a 64 MB incompressible file; 2000 small files
each committed individually). The device is RAM, so wall times understate the win
on a real disk - the I/O counts (added with this rework) are the durable metric.

| scenario | baseline | after this rework | I/O after |
| --- | --- | --- | --- |
| 64 MB write | 2.07 s | 1.72 s | 16 418 reads, 16 421 writes (~1+1 per data block) |
| 64 MB sequential read | 354 ms | 204 ms | 16 400 reads (~1.001 per data block) |
| 2000 small files (2000 commits) | 1.45 s | 0.50 s | ~12.6 reads, ~9.6 writes per commit |
| 2000 stats | 179 ms | 164 ms | ~8 reads per stat |

What changed structurally:

- Commit no longer rewalks the volume: the free map is maintained incrementally
  (per-transaction drop lists, deferred one generation; pinned snapshot blocks
  honored bit-by-bit). Commit cost stopped scaling with live metadata - the
  2000-file loop's 2.9x is this; on a big volume the gap grows without bound.
- The allocator went from an O(pool) scan per block to next-fit cursors with
  byte-wide bitmap scanning, plus an up-front contiguous run reservation for
  whole-file writes.
- Checksum blocks are batched: the write path assembles a run's checksum block in
  memory and writes it once (previously a read-modify-write per data block); the
  read path verifies a checksum block once per run instead of once per block
  (previously 2 reads per data block, now ~1).
- Path resolution and stats ride bounded inode/dentry caches.

The equivalence of the incremental free map with a full volume walk is asserted
after every mutation kind by the standing test
`the_incremental_free_map_matches_a_full_rederivation`.
