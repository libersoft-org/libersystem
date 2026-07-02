# Performance notes

Measured numbers for the milestones whose "done when" includes a before/after
comparison. Methodology per entry; machine noise applies, so treat the times as
orders, not precision instruments.

## M74 - LiberFS allocator and free-map scaling (2026-07-02)

Benchmark: `cd src/fs/liberfs && cargo test --release bench_scaling -- --ignored --nocapture`
(a 1 GiB sparse RAM-backed volume; a 64 MiB incompressible file; 2000 small files
each committed individually). The device is RAM, so wall times understate the win
on a real disk - the I/O counts (added with M74) are the durable metric.

| scenario | M73 baseline | after M74 | I/O after M74 |
| --- | --- | --- | --- |
| 64 MiB write | 2.07 s | 1.72 s | 16 418 reads, 16 421 writes (~1+1 per data block) |
| 64 MiB sequential read | 354 ms | 204 ms | 16 400 reads (~1.001 per data block) |
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
