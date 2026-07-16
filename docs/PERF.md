# Performance notes

Measured numbers for the changes whose goal includes a before/after
comparison. Methodology per entry; machine noise applies, so treat the times as
orders, not precision instruments.

## Image conversion (2026-07-16)

`just image-bench` builds the same no_std leaves used by `imgconv` in an optimized
host profile and converts a deterministic 512x512 true-color RGBA fixture. Each row
measures full container encode and independent content-sniff/decode. A tracking global
allocator reports incremental peak heap above the live input/output baseline. The
standing gate is five seconds and 8 MiB per operation; WebP is held to 4 MiB encode and
2 MiB decode. RGB MSE covers profiles that retain the fixture dimensions. One x86 host
run produced:

| output profile | bytes | RGB MSE | encode | decode |
| --- | ---: | ---: | ---: | ---: |
| BMP 24-bit | 786,486 | 0 | 27.8 ms | 1.6 ms |
| BMP indexed quality 0, 16 colors | 262,262 | 1,390 | 49.9 ms | 1.8 ms |
| BMP indexed quality 100, up to 256 colors | 263,222 | 239 | 150.3 ms | 1.8 ms |
| PNG compression 0 | 1,049,321 | 0 | 42.1 ms | 19.1 ms |
| PNG compression 100 | 441,032 | 0 | 65.0 ms | 25.8 ms |
| PNG indexed quality 0, 16 colors | 57,625 | 1,390 | 56.5 ms | 5.5 ms |
| PNG indexed quality 100, up to 256 colors | 114,191 | 239 | 165.0 ms | 8.5 ms |
| PCX 24-bit RLE | 664,704 | 0 | 30.4 ms | 2.5 ms |
| PCX indexed quality 0, 16 colors | 200,451 | 1,390 | 50.6 ms | 2.0 ms |
| PCX indexed quality 100, up to 256 colors | 276,657 | 239 | 153.2 ms | 2.1 ms |
| PPM P6 | 786,447 | 0 | 27.0 ms | 3.0 ms |
| QOI RGBA | 1,048,595 | 0 | 27.6 ms | 1.0 ms |
| TGA RLE | 788,498 | 0 | 28.1 ms | 0.9 ms |
| ICO, 256x256 PNG-backed | 213,193 | - | 40.4 ms | 10.1 ms |
| ICNS, 32x32 classic RGB RLE + alpha | 3,176 | - | 25.9 ms | 0.01 ms |
| ICNS, 512x512 PNG-backed | 441,048 | 0 | 70.9 ms | 25.6 ms |
| JPEG quality 10 | 10,008 | 890 | 30.0 ms | 1.5 ms |
| JPEG quality 100 | 433,763 | 0 | 35.6 ms | 6.7 ms |
| WebP lossless effort 0 | 786,522 | 0 | 29.7 ms | 3.9 ms |
| WebP lossless effort 25 | 282 | 0 | 27.8 ms | 0.3 ms |
| WebP lossless effort 50 | 282 | 0 | 27.4 ms | 0.3 ms |
| WebP lossless effort 75 | 282 | 0 | 28.1 ms | 0.3 ms |
| WebP lossless effort 100 | 282 | 0 | 27.7 ms | 0.3 ms |
| WebP lossy quality 0, effort 100 | 7,104 | 923 | 34.5 ms | 2.8 ms |
| WebP lossy quality 100, effort 100 | 219,842 | 250 | 60.9 ms | 12.8 ms |
| WebP lossy quality 90, effort 0 | 91,104 | 256 | 44.8 ms | 7.6 ms |
| WebP lossy quality 90, effort 100 | 91,140 | 256 | 46.0 ms | 7.6 ms |
| APNG, one frame | 441,090 | 0 | 66.1 ms | 22.1 ms |
| GIF quality 0, 16 colors | 73,146 | 1,390 | 54.5 ms | 6.3 ms |
| GIF quality 100, up to 256 colors | 150,236 | 239 | 157.2 ms | 7.1 ms |
| WebP lossless animation, 256x256, 2 frames | 458 | 0 | 0.80 ms | 0.20 ms |

GIF, explicit indexed PNG, indexed BMP and indexed PCX use the same bounded no_std
`quantize.lslib`. It builds one deterministic weighted
median-cut palette across all supplied images, preserves exact palettes when they fit,
reserves one binary-transparency entry when needed and maps rows with bounded
Floyd-Steinberg error buffers. Quality 0 through 100 maps to 16 through 256 total
entries; tests require quality 100 to beat quality 0 on RGB squared error and cap its
mean squared error at 256. PNG/BMP/PCX without `--quality` keep their previous
RGBA/true-color output. Supplying `--quality` explicitly selects indexed output; PNG
partial alpha is rejected rather than silently thresholded, while binary alpha is
represented by PLTE/tRNS. BMP/PCX remain opaque-only because their selected output
profiles carry no alpha.

Classic ICNS output uses the format's component-wise PackBits variant for
`is32/il32/ih32` RGB and pairs it with `s8mk/l8mk/h8mk` 8-bit alpha. The decoder also
accepts `it32/t8mk` 128-pixel classic input, while the encoder prefers the modern
PNG-backed `ic07` entry at 128 pixels and above.

Animated WebP decoding preserves the bounded `ANMF` rectangle, timing, blend and
background-disposal metadata. The shared `pix::Compositor` supplies the visual canvas
for static previews and cross-format conversion. Lossless WebP animation output uses
canonical full-canvas VP8L frames, preserving displayed pixels and timing while avoiding
format-local duplicate compositing code.

Lossless WebP effort is a deterministic search over the encoder's valid plain and
predictor VP8L profiles. Effort 0 emits plain; intermediate levels analyze a growing
row sample and choose from residual variation; effort 100 encodes both and selects the
smaller output. On this smooth fixture efforts 25/50/75 choose the 282-byte predictor
profile with 2,542,735-byte encode heap, while exhaustive effort 100 uses 3,670,706
bytes and proves no larger than either candidate. Decode peaks at 1,052,892 bytes.

Lossy WebP uses the native no_std VP8 keyframe encoder. Quality maps to the normative
DC/AC quantizer tables; independent effort progressively searches DC, vertical,
horizontal and true-motion chroma prediction. The benchmark requires quality 100 to
beat quality 0 and caps its RGB MSE at 300. Effort endpoints at fixed quality must
produce different deterministic bitstreams, proving the control is not ignored. Raw
`ALPH` chunks preserve alpha exactly outside the lossy VP8 color payload.

The first governed integration uses a seeded writable LiberFS block stand-in:
`imgconv.lsexe` receives only the system volume slot, converts staged BMP to indexed PNG
at quality and compression 100, exits, and the kernel reopens the destination through
StorageService and independently decodes its exactly representable palette to exact
RGBA. A separate PermissionManager run reaches the
destination-conflict path under the `volumes`-only policy without mutating its read-only
scenario volume. `imgview` now calls the same central content sniffer and converts straight
RGBA to display BGRX only at render time, so viewer and converter support cannot drift and
transparent pixels are not destroyed at decode time.

The expanded governed integration runs two real StorageService instances: writable
LiberFS as `vol://system` and writable FAT16 as `vol://media`. `imgconv.lsexe` converts
the staged system BMP into an indexed media BMP, StorageService reopens it, and the BMP
leaf independently verifies exact RGBA. The same output is then opened by the real
`imgview.lsexe`; its display/input stand-ins observe nonblank presentation, focus-scoped
key subscription, `q`, surface release and clean process exit. A second FAT16 image has
every free cluster allocated and an existing `KEEP.BMP`; forced resized conversion
returns a storage failure and the old bytes remain exactly unchanged, pinning the
filesystem publication guarantee end to end.

The same governed process also writes a quality-100/effort-100 lossy `CROSS.WEBP`
across the volume boundary. StorageService reopens it, the test verifies the simple
opaque `RIFF/WEBP/VP8 ` profile and the independent WebP decoder checks dimensions plus
bounded RGB error. The focused x86 capability/storage/process/filesystem run is 57/57.
Complete shared libraries and userspace build on x86_64, AArch64 and RISC-V; the native
encoder plus VP8L search changes `webp.lslib` to 349,912 / 442,936 / 384,000 bytes
respectively. The governed scenario also emits lossless `CROSSL.WEBP` at effort 50,
reopens it through FAT16 StorageService and verifies exact RGBA independently.

Current limits are deliberate and typed: lossy animated WebP output is unsupported
rather than silently flattening frames. ICNS JPEG2000
entries remain unsupported, and image output is deliberately a fully encoded whole-file
StorageService write. LiberFS publishes that write through its CoW transaction and FAT
uses allocate/write/new-entry-swap/free-old ordering, so a failed backend write preserves
the previous destination without requiring a temporary filename in the tool.

## Audio decoding and governed playback (2026-07-15)

`just audio-bench` is the standing optimized-host throughput gate. The host-only
benchmark depends on the same atomized decoder leaves as `play`, reparses each staged
fixture on every iteration, drains signed-i16 output in bounded 1,024-frame chunks, and
decodes at least 60 seconds of logical audio per row. It fails if any path falls below
real time. `tools/generate-audio-tests.sh` derives every fixture from the user-facing
7.44-second, 44.1 kHz mono `volume/test.mp3`; the stereo WavPack variant uses the same
signal with an inverted right channel. One x86 host run produced:

| codec/container | staged rate | fixture frames | iterations | wall | realtime |
| --- | ---: | ---: | ---: | ---: | ---: |
| WAV PCM | 44,100 Hz | 328,104 | 9 | 0.008 s | 8,843.7x |
| WAV IMA ADPCM | 44,100 Hz | 328,104 | 9 | 0.018 s | 3,805.8x |
| WAV MS ADPCM | 44,100 Hz | 328,104 | 9 | 0.013 s | 5,251.3x |
| AIFF PCM | 44,100 Hz | 328,104 | 9 | 0.009 s | 7,555.9x |
| AIFC PCM | 44,100 Hz | 328,104 | 9 | 0.008 s | 8,403.2x |
| FLAC | 44,100 Hz | 328,104 | 9 | 0.163 s | 411.5x |
| MP3 | 44,100 Hz | 330,624 | 9 | 0.065 s | 1,033.7x |
| Ogg Vorbis | 44,100 Hz | 328,104 | 9 | 0.136 s | 492.7x |
| WavPack mono | 44,100 Hz | 328,104 | 9 | 0.159 s | 420.4x |
| WavPack stereo | 44,100 Hz | 328,104 | 9 | 0.271 s | 246.9x |

The focused x86 KVM `audio` test now connects two real `play` processes to one real
StorageService and AudioService through separate playback-only scopes. It holds WAV's
first hardware period pending, queues Ogg Vorbis behind it, and then acknowledges the
driver period. The next 48 kHz output starts with the exact pinned mixed sample `2`.
Six hardware periods arrive continuously before both long players receive caught
`SIG_INT`; the bounded accepted tail then drains without underrun. One debug-profile
KVM run produced:

| governed playback metric | measured |
| --- | ---: |
| launch to first hardware period | 17.06 ms |
| Vorbis launch, parse, decode and queue | 87.03 ms |
| driver ACK to mixed period | 0.356 ms |
| peak queued source frames during overlap | 683 |
| WAV peak working set | 1,745,822 B |
| Vorbis peak working set | 1,205,779 B |
| underruns across six expected periods | 0 |

The working-set counters combine resident ELF/stack pages, the child Domain's private
MemoryObject high-water mark, and the mapped input file. Domain high-water accounting is
transactional: a failed ancestor-limit charge is rolled back without raising the peak,
and refunds do not erase an observed peak. A separate long-WavPack path delivers caught
`SIG_INT` while `play` is blocked by bounded backpressure. The player explicitly closes
and exits; AudioService drains 11 already accepted periods (bounded below the asserted
64-period ceiling), emits its stop sentinel, and releases the hardware stream.

Live output remains reproducibly inspectable with QEMU's WAV backend rather than a
listener and a particular SPICE client. Boot with
`AUDIO_WAV=/tmp/libersystem-audio.wav just lab boot --fresh`, run
`just lab sh play test.wv`, then `just lab quit`; the captured WAV traverses the live
shell -> governed player -> StorageService -> WavPack -> AudioService -> virtio-sound
-> host-audio path and remains inspectable in CI or headless development.

## Application surface presentation (2026-07-14)

Measured by the tagged x86 KVM display test (`cd src && just test-tags display`).
The real userspace DisplayService drives a stand-in virtio-gpu channel with the same
synchronous `PRESENT` / `OK` protocol as the driver. Its private typed counters read
`SYS_CLOCK_MONO_NS` around (a) the CPU blit/scale and (b) the driver transfer+flush
acknowledgement. The benchmark scales a Doom-class 320x200 B8G8R8X8 surface into a
1024x768 scanout (1024x640 output, centered) and then presents a 32x20 source damage
rectangle. Two debug-profile KVM runs establish the unoptimized range; the final column
optimizes only the small shared `pix` dependency at opt-level 2 while retaining debug
information and unoptimized service control flow.

| scenario | debug baseline | incremental damage, debug | incremental damage + optimized `pix` |
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
the hot loop in host-tested `pix` preserved behavior and changed the debug
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
consumers: `pix` (pixel vocabulary and bounded blitters, used by DisplayService),
`surface` (typed DisplayService client plus RAII MemoryObject mapping, used by
ConsoleService), `keys` (canonical HID usages and held-key edge state, used by
InputService), and `pcm` (format/frame validation, little-endian sample decoding,
mono expansion and rate phase, used by AudioService). Pure helpers run nine host tests
through `just app-libs-test`; surface lifecycle is exercised by the live console and
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
many concurrent processes can still justify `lsrt.lslib`/`proto.lslib` sharing.

## System-image dynamic linking (2026-07-14)

M123 adds an eager ELF64 module loader and an image-internal shared build. The bare-metal
Rust targets support neither Cargo `dylib` nor `cdylib`, so the reproducible builder emits
full-graph PIC rlibs and links their object members with the pinned `rust-lld -shared`.
The x86 KVM integration launches an assembly-only staged `dyn_probe` through the real
StorageService and ProcessService. ProcessService reads its `DT_NEEDED` DAG
(`pix.lslib`, `proto.lslib`, `lsrt.lslib`), the kernel eagerly applies RELA/PLT symbol
relocations, and the probe calls exports from both leaf providers before its first IPC.

Cold start is measured from sending the ProcessService `launch` request to receiving
`dynamic link ok` from userspace. The immediately repeated launch keeps the first
Process handle alive, so immutable provider pages are already in the physical-page
cache; ProcessService still reads and parses all provider files from StorageService.

| x86 KVM scenario | latency |
| --- | ---: |
| static governed `graphics_probe` in the focused runs | 2.108-2.373 ms |
| dynamic probe, cold | 95.176-209.965 ms |
| dynamic probe, providers resident | 96.569-211.950 ms |

The repeated launch shows that the present bottleneck is dependency file I/O/parsing,
not page allocation/copy. A future image-index or ProcessService immutable-byte cache is
required before dynamic launch latency can compete with a small static tool.

The dynamic process owns 16 private pages (RW/BSS/GOT plus stack) and references 149
immutable shared pages. With two concurrent Process handles the test observes 32 private
pages plus 298 shared references to the same 149 physical frames. Therefore:

$$
	ext{unshared}=2(16+149)=330\text{ pages},\qquad
	ext{shared}=2(16)+149=181\text{ pages}
$$

The measured saving at $N=2$ is 149 pages, or 610,304 bytes. The test additionally
compares the two processes' first `lsrt.lslib` text mappings and requires the exact same
physical frame. RW relocation targets remain private and text relocations are rejected.

The complete first shared graph is atomized as `lsrt.lslib`, `proto.lslib`, `pix.lslib`,
`inflate.lslib`, `bmp.lslib`, `png.lslib`, `keys.lslib`, `pcm.lslib`, and
`surface.lslib`. Raw x86 release
objects plus the probe total 799,448 bytes. After package staging strips non-runtime
symbols, their payload is 644,840 bytes plus 320 bytes of archive entries; the equivalent
factory `volume.pkg` is 12,193,513 bytes versus a computed 11,548,353-byte image with those
entries removed.

| x86 shared artifact | raw release ELF |
| --- | ---: |
| `lsrt.lslib` | 414,920 B |
| `proto.lslib` | 317,200 B |
| `pix.lslib` | 7,528 B |
| `inflate.lslib` | 13,984 B |
| `bmp.lslib` | 10,200 B |
| `png.lslib` | 13,936 B |
| `keys.lslib` | 5,232 B |
| `pcm.lslib` | 4,064 B |
| `surface.lslib` | 9,808 B |
| `dyn_probe` | 2,576 B |

Decision: keep the loader, tri-architecture shared graph, and staged dynamic probe, but
do not broadly convert small tools yet. The earlier six representative static release
ELFs total only 491,832 bytes, below even the runtime/protocol/pixel pilot graph, and
their cold start is far lower. Large applications and many concurrent consumers can
cross the RAM break-even; conversion remains per-target and measurement-gated rather
than ideological.

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
