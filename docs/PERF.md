# Performance notes

Measured numbers for the changes whose goal includes a before/after
comparison. Methodology per entry; machine noise applies, so treat the times as
orders, not precision instruments.

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
