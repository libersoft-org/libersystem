AUDITOR'S REVIEW OF PLAN P02M0193 (2026-09-25T22:49:59Z):

**Rating: 6/10.** The plan's account of what tiles share is largely accurate and the `soft3d` shape fits, but its oracle is weaker than the rule it inherits, and it omits the per-tile allocations and non-`Sync` inputs that a parallel walk must remove.

Reviewed [the plan](/data/yellow/libersystem/docs/todo/P02M0193.md) at commit `07371c44af82a11c1d275b0cbd832899f712e62a`, together with:

- [P02M0103's parallel 3D plan](/data/yellow/libersystem/docs/todo/P02M0103.md:6124) and [honesty rule](/data/yellow/libersystem/docs/todo/P02M0103.md:2630);
- [P02M0189](/data/yellow/libersystem/docs/todo/P02M0189.md) and `docs/PERF.md`;
- the `soft2d` backend, `soft3d::frame`, `rt::pool`, the `rt` heap, the Render2D profile and `soft2d-bench`.

The plan's "WHAT MAKES 2D DIFFERENT" claims check out:

- [one tile surface and one scratch set](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/backend.rs:203) serve [every tile in turn](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/backend.rs:734), charged against the [64 MiB prepared-scratch ceiling](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/backend.rs:688);
- the [glyph cache is written during replay](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/backend.rs:1164);
- tiles are [64 pixels](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/lib.rs:54);
- [cancellation is checked between tiles](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/backend.rs:738).

Tiles read no neighbour's pixels, because a filter's backdrop is [the tile's own surface](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/backend.rs:940). [`rt::pool::for_each`](/data/yellow/libersystem/src/user/runtime/rt/src/pool.rs:298) is generic, so one pool can serve both backends.

1. **Medium - The verification lets the parallel walk differ from the serial walk within the profile's tolerance, contradicting the rule it inherits.**

   The [verification item](/data/yellow/libersystem/docs/todo/P02M0193.md:41) asks for pixels identical "bit for bit on the parts the profile says are bit-exact and within its tolerance where it says tolerance". The profile's tolerances measure [an implementation against the analytic answer](/data/yellow/libersystem/docs/graphics/RENDER2D_PROFILE_1.md:268), not two schedules of one implementation. P02M0103, whose rule this plan applies, says so explicitly: "The scalar reference and its parallel paths must still agree bit-exactly WITH EACH OTHER - they are one algorithm - and that is a different claim from a conformance tolerance against another backend" ([P02M0103](/data/yellow/libersystem/docs/todo/P02M0103.md:2637)). `soft3d` met that bar, [bit-identical at every worker count](/data/yellow/libersystem/docs/todo/P02M0103.md:6196), and the plan's [own opening paragraph](/data/yellow/libersystem/docs/todo/P02M0193.md:7) cites that result.

   Each tile runs the same code over disjoint pixels, so no legitimate difference exists. A tolerance would instead admit exactly the defects parallelism introduces, such as a lane reading another tile's stale scratch, provided the error stays within 2/255.

   **Correct the verification and `test2d-sw` items** to require bit-identical targets at every worker count, in the host differential tests and in the guest comparison alike. Changing one phrase is enough.

2. **Medium - The inventory of per-tile state is incomplete: replay allocates in every tile through a spin-locked heap, and the inputs read inside the tile loop are not `Sync`.**

   The plan lists the [rasteriser, surface pool, mask pool and span buffers](/data/yellow/libersystem/docs/todo/P02M0193.md:12) as what a lane must own. Replay also allocates:

   - it [builds a clip stack and a layer stack for every tile](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/backend.rs:789), and the first [`reset` pushes into an empty `Vec`](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/clip.rs:81);
   - it [flattens and edge-builds every outline glyph in every tile](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/backend.rs:1168), where [`flatten` returns a `Vec`](/data/yellow/libersystem/src/user/libs/graphics/render2d/src/flatten.rs:202) and [`Edges::build` allocates](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/raster.rs:68).

   This contradicts the backend's own claim that [render allocates nothing](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/backend.rs:8). `soft2d` has no allocation-count test to catch it; `soft3d` has a [serial one](/data/yellow/libersystem/src/user/libs/graphics/soft3d/src/tests.rs:1851) and a [pooled one](/data/yellow/libersystem/src/user/libs/graphics/soft3d/src/tests.rs:2865). In the guest, every such allocation takes the process's [single test-and-set heap lock](/data/yellow/libersystem/src/user/runtime/rt/src/heap.rs:205), which the host benchmark's allocator does not reproduce.

   `rt::pool` also requires a [`Sync` work closure](/data/yellow/libersystem/src/user/runtime/rt/src/pool.rs:298). Yet [`ImageSource`](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/target.rs:29), [`GlyphProvider`](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/glyph.rs:72) and [`Cancellation`](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/backend.rs:48) are not `Sync`. The image source is [consulted inside a tile](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/backend.rs:885), cancellation is checked before every tile, and the existing cancellation fixture [counts through a `Cell`](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/tests.rs:850). The 3D plan named both requirements: [`Source` gains `Sync`](/data/yellow/libersystem/docs/todo/P02M0103.md:6142), and [NO STEADY-STATE ALLOCATION](/data/yellow/libersystem/docs/todo/P02M0103.md:6147).

   As written, "the warmed-frame allocation count through a pool" has no achievable target, and the lane-scratch item omits state that a lane must own.

   **Correct the interface, lane-scratch and verification items** as follows:

   - move the clip stack, the layer stack and the glyph edge buffers into lane scratch reserved at prepare;
   - add a serial and a pooled warmed-frame allocation test, each with its expected count stated;
   - state the `Sync` bounds on `ImageSource` and `Cancellation`, and update their implementors; `GlyphProvider` can stay non-`Sync` if it is called only at prepare.

3. **Low - The glyph item misdescribes what the cache holds and leaves a read-only replay with no defined behaviour on a cache miss.**

   The plan says a glyph ["is rasterised the first time a tile meets it"](/data/yellow/libersystem/docs/todo/P02M0193.md:16) and asks for glyphs ["rasterised at prepare"](/data/yellow/libersystem/docs/todo/P02M0193.md:33). The cache actually holds [decoded forms](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/glyph.rs:32) (outlines, masks, bitmaps, colour layers), and outlines are rasterised in every tile. Pre-rasterising an outline into a mask would change text pixels, because masks composite through the [coverage gamma](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/backend.rs:1208) and outlines do not.

   The cache is also bounded and [evicts in insertion order](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/glyph.rs:145), and it outlives any one prepared list. A prepared list is bound only to the [generation that `clear` bumps](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/glyph.rs:130). An entry decoded at prepare can therefore be evicted before render without the prepared list's key noticing.

   **Correct the glyph item** to say "decoded at prepare", and define what happens on a miss. Possible rules are to refuse at prepare when the list's glyph set does not fit the cache, to decode into lane-private storage, or to bump the generation on eviction. This is a small change.

4. **Low - Nothing orders this milestone against P02M0189, whose measured subject it changes.**

   P02M0189 opens on the live 2D demo's [79.6 ms draw against a 132.4 ms interval](/data/yellow/libersystem/docs/todo/P02M0189.md:5) and measures ["the path the demos already use"](/data/yellow/libersystem/docs/todo/P02M0189.md:38). This plan makes [`test2d-sw` render through `rt::pool`](/data/yellow/libersystem/docs/todo/P02M0193.md:47), and the 3D precedent [defaults to one worker per core](/data/yellow/libersystem/src/user/apps/tools/src/test3d_sw.rs:1443). If this milestone lands while P02M0189 is measuring, three things follow:

   - the draw term changes;
   - worker threads compete with DisplayService on the guest's [two vCPUs](/data/yellow/libersystem/src/tools/guest-gate.sh:100);
   - one account then mixes two systems, and the [recorded demo rows](/data/yellow/libersystem/docs/PERF.md:16) no longer describe the demo.

   **Correct the `test2d-sw` item** to keep `--workers 1` as the serial reference. Either state when the default changes relative to P02M0189, or require P02M0189 to record the worker count. Re-measure the PERF.md demo rows with the worker count stated. This is a small change.

Validation was by inspecting the source, plans and profile. No plan or source code was modified, and no build, benchmark or guest run was performed for this audit.

PLANNER'S RESPONSE ON P02M0193 (2026-09-26T02:37:49Z):

Every finding was checked against the tree: `soft2d`'s `backend.rs` (the tile loop, `replay`, `draw_glyphs`, the per-frame shader table), `clip.rs`, `raster.rs`, `glyph.rs`, `target.rs`, `filter.rs`, `paint.rs`, `tests.rs` and `lib.rs`; `render2d::flatten`; `soft3d`'s counting allocator, `frame.rs` (`Workers`, `Lane`, `recycle`) and its two allocation tests; `rt::pool::for_each` and the `rt` heap lock; `test2d_sw.rs`, `test3d_sw.rs` and the guest test that runs `test3d-sw --workers 4 --compare`; `RENDER2D_PROFILE_1.md` (prepared-list validity, limits, conformance thresholds); P02M0103 at the honesty rule and the parallel 3D plan; P02M0189; `docs/PERF.md`; `guest-gate.sh`. The coordination decision that P02M0189 measures the serial walk and pins `--workers 1` once this milestone lands is applied.

1. **ACCEPTED - the parallel walk must be bit-identical, not within tolerance.** Confirmed: the profile's thresholds compare an implementation against the analytic area, and P02M0103 states that the scalar reference and its parallel paths agree bit-exactly with each other; `soft3d` met that bar and `test3d-sw --compare` checks exact equality. The verification item now requires the target BIT-IDENTICAL to the serial walk's at every worker count with no tolerance, and says why. The same requirement is in the `soft2d-bench` item (bit-identical to one worker's picture at every count) and in the `test2d-sw --compare` item.

2. **ACCEPTED - per-unit allocations and the `Sync` bounds.** Confirmed, and the inventory was wider than the finding listed: besides the clip stack and layer stack built per tile (`ClipStack::new` then `reset` pushes into an empty vector) and the per-tile `flatten` plus `Edges::build` of every outline and colour-layer glyph, `filter::evaluate` allocates its node table and `kernel()` its weights per filtered layer per tile, and `render` collects its shader table and builds each gradient's `Ramp` every frame. `rt::pool::for_each` takes a `Sync` closure; `ImageSource` (consulted inside a tile through `Lookup`), `Cancellation` and `GlyphProvider` are not `Sync`, and the `StopAfter` fixture uses a `Cell`. Plan changes: WHAT MAKES 2D DIFFERENT gains a bullet on the allocations and the heap lock and one on the missing `Sync`; a new `Sync` item gives `ImageSource` and `Cancellation` `Sync` supertraits (every image source in the tree is plain data; the fixture moves to an atomic) and leaves `GlyphProvider` unchanged because it is called only at prepare; the lane item now names every piece a lane owns (tile surface, rasteriser, surface pool, mask pool, spans, clip stack, layer stack, filter node table), each reserved at prepare for the list, including the rasteriser's active list, which `reserve` does not size today, and glyph edges in the widest-edge count; a new item makes the replay allocate nothing (ramps at prepare, the shader table's storage recycled as `soft3d::frame` does, kernel weights at prepare, glyph edges at prepare); the verification gains a warmed-frame test with the counting allocator added to `soft2d`'s test build and the expected count stated: ZERO through `Serial` and ZERO through a four-lane pool on the test thread. I declined the part of the recommendation that puts the glyph edge buffers in lane scratch. The edges are built once per placed glyph at prepare instead, the same place a fill's edges are built. This needs no new flatten-into API, does less work than rebuilding per tile, and produces the same edges.

3. **ACCEPTED - the glyph cache holds decoded forms, and a miss rule is decided.** Confirmed: `GlyphImage` is outline, mask, bitmap or layers; outlines are flattened and filled in every tile; only masks go through `coverage_through_gamma`, so pre-rasterising outlines would move text pixels; the cache evicts from the front and only `clear` bumps the generation. The glyph item now reads "decoded and placed at prepare": prepare computes each placed glyph's key and device origin (today's arithmetic, moved), decodes misses through the provider, builds outline and colour-layer edges at the placed origin, and holds mask and bitmap forms by reference; nothing is pre-rasterised. THE MISS RULE CHOSEN: there is no miss at render. Cache entries become shared (`Arc`), the prepared list holds every form it draws, and eviction drops only the cache's reference, which is the profile's "a live prepared snapshot retains its own resources". The cache's byte bound counts what the cache holds, `clear` still bumps the generation carried in the prepared key, and `GlyphRaster::get` keeps its signature. I rejected the other three rules. Refusing at prepare would make a frame that draws today fail. Lane-private decoding would allocate in the replay and put the provider on the lanes. Bumping the generation on every eviction would invalidate unrelated lists, and `render` does not check the generation anyway.

4. **ACCEPTED - ordering against P02M0189.** Confirmed: `test3d-sw` defaults to one worker per core, the guest gate runs `--smp 2`, and the PERF.md demo rows are single-configuration numbers. The `test2d-sw` item now keeps `--workers=1` as the serial walk and the serial reference and makes the demo print its lane count. A new item re-measures the PERF.md 2D demo rows at `--workers=1` and at the default, each row stating its worker count and noting that the pooled row shares two vCPUs with DisplayService. The same item states that P02M0189's account is the serial walk, and that its gate pins `--workers=1` once this lands, with the pooled draw recorded as an additional row that is not part of the account (P02M0189 owns that side). The spelling `--workers=N` follows `test2d-sw`'s own `--key=value` arguments.

Re-check of the whole plan: every item states its design decision. The lane inventory, the no-allocation item, the glyph item and the `Sync` item fit together: lanes read only the prepared list, the shader and transfer tables and a `Sync` image source, and write only their own reserved scratch and their own unit of the target. The oracles (bit-identical differential, zero-allocation counts, whole-or-untouched cancellation, `--compare` in the guest) can be met with the design as written. The unit-of-work item now names the same three options as the introduction. Owner decisions and EXCLUDES are unchanged, and no version changes. No source, test or script was modified, and nothing was built or run.

Final consistency check (2026-09-26T02:56:37Z): the demo-rows item said the pooled row runs "on the guest's two vCPUs". Guest gates pin `--smp 2`, the harness otherwise defaults to the host's core count, and P02M0189's account gate pins four, so the item now requires each re-measured row to name its worker count AND the guest's vCPU count. No other change; no source was modified.


AUDITOR'S RE-AUDIT OF PLAN P02M0193 (2026-09-26T04:01:12Z):

**Rating: 8/10.** The four corrections hold and the design can be built, but the lane budget leaves open a choice that can break the plan's own bit-identity rule, and the guest comparison can be built in a way that cannot fail.

I read the complete history: the original review of `07371c44`, the planner's response, and its final consistency check. I checked the current plan and the planner's diff (`07371c44..0dd5da07`) against the tree:

- `soft2d`: the tile loop, `replay`, `draw_glyphs`, the prepare budget with its optional copies, and its clip, layer, raster, glyph, target, filter, paint and tile modules;
- `render2d`, the `graphics-core` sampler and pyramid, and `soft3d::frame` with its counting-allocator tests;
- `rt::pool` and the heap lock;
- `test2d-sw`, `test3d-sw`, both demo guest tests, `soft2d-bench` and `soft3d-bench`;
- the profile, `docs/PERF.md` and the harness scripts;
- P02M0189 and P02M0103.

All four corrections hold:

- Bit-identity replaces the tolerance everywhere.
- The per-unit allocation inventory and the `Sync` bounds match the code. I checked every `ImageSource` and `Cancellation` implementor.
- The glyph miss rule is sound. Declining lane-scratch glyph edges is justified, because fills already build their edges at prepare.
- Both plans now say the same about `--workers=1` and the pooled row.

Bit-identity is achievable feature by feature:

- No tile reads another tile's output. A backdrop filter reads the tile's own surface.
- Layers and masks are cleared when they are taken.
- The dither phase is the target position.
- The stale tile that a skipped decode leaves is overwritten exactly.

The one exception is finding 1.

1. **Medium - The lane-count rule does not say whether extra lanes may take the room of the optional decoded image copies. At the ceiling, that choice decides whether the picture stays bit-identical across worker counts.**

   This is a new finding. It bears on the bit-identity rule that original finding 1 introduced.

   The plan prices lanes only: the frame ["computes what one lane costs and runs with as many as fit, down to one"](/data/yellow/libersystem/docs/todo/P02M0193.md:55). It also requires the target [bit-identical at every worker count](/data/yellow/libersystem/docs/todo/P02M0193.md:85), and the bench picture [bit-identical to one worker's at every count](/data/yellow/libersystem/docs/todo/P02M0193.md:95).

   The same ceiling already has a second claimant. Prepare builds an optional level-zero copy for every image drawn [`Bilinear` or `Bicubic`](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/backend.rs:628). It charges one set of scratch in [`fixed`](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/backend.rs:688) and [gives copies back until the total fits](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/backend.rs:693). Its stated reason is that [dropping one "costs time and nothing else"](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/backend.rs:691), and the shader comment says sampling the copy is ["the same picture"](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/paint.rs:414).

   That is not true to the bit:

   - The copy is stored as [`R16G16B16A16Float`](/data/yellow/libersystem/src/user/libs/graphics/core/src/sample.rs:420), and a shader samples it [whenever it exists](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/paint.rs:422).
   - The direct path [decodes each tap](/data/yellow/libersystem/src/user/libs/graphics/core/src/sample.rs:185) through a table of [`f32`](/data/yellow/libersystem/src/user/libs/graphics/core/src/pixel.rs:538).
   - So the two paths compute different values, and keeping or giving back a copy can change output pixels.

   With N lanes, the lane part of `fixed` grows N-fold, and the plan allows two readings:

   - Fit lanes first and give copies back to make room. The set of copies, and so the picture, then depends on the worker count.
   - Settle the copies for one lane and fit lanes into what remains. The picture then does not depend on the worker count.

   One frozen scene sits on this boundary. UI-effects:

   - draws its 512x512 photo `Bilinear` across the frame ([`main.rs:536`](/data/yellow/libersystem/src/tools/soft2d-bench/src/main.rs:536));
   - opens eleven filtered layers, with blurs up to sigma 6 ([`main.rs:550`](/data/yellow/libersystem/src/tools/soft2d-bench/src/main.rs:550), [`main.rs:558`](/data/yellow/libersystem/src/tools/soft2d-bench/src/main.rs:558)).

   By the code's own arithmetic:

   - A blur reaches [3 sigma](/data/yellow/libersystem/src/user/libs/graphics/render2d/src/filter.rs:111), which gives an expansion of 18 and a [scratch extent](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/backend.rs:671) of 100.
   - The pool holds [19 surfaces](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/backend.rs:676) at [16 bytes a pixel](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/target.rs:253), about 3.1 MB a lane.
   - The photo's copy is 512x512 at 8 bytes, 2 MiB.
   - Against the [64 MiB ceiling](/data/yellow/libersystem/docs/graphics/RENDER2D_PROFILE_1.md:256), twenty lanes fit beside the copy, and a twenty-first fits only if the copy is given back.

   The plan's own [`--scaling` run to sixty-four workers](/data/yellow/libersystem/docs/todo/P02M0193.md:94) crosses that boundary.

   The rule also has no place for a cost that exists only when more than one lane runs. The [tile-major intermediate](/data/yellow/libersystem/docs/todo/P02M0193.md:76) that the unit-of-work item may choose is a whole-frame buffer, not per-lane scratch. If it is not charged, the reported scratch understates the ceiling. If it is charged to the one-lane frame, a frame that fits today can be refused, which contradicts ["exactly as today"](/data/yellow/libersystem/docs/todo/P02M0193.md:56).

   **Correct the lane item.** State four things:

   - The optional copies are settled exactly as today, against one lane's scratch.
   - Lanes beyond the first are fitted only into what is left, so a lane never displaces a copy.
   - Any cost that exists only with more than one lane (the intermediate, if chosen) is charged together with those lanes. A frame whose second lane does not fit runs on one lane without that cost.
   - A host case draws an image whose copy fits beside one lane but not beside the pool's lanes, and its target is bit-identical at every worker count.

2. **Medium - The guest `--compare` test sets no floor on how many units a frame has. At the extent of the test it is told to copy, a soft2d frame is one unit, so the comparison cannot fail.**

   This is a new finding.

   The plan asks for a guest test that runs the demo with several workers ["the way `test3d-sw --workers 4 --compare` is run"](/data/yellow/libersystem/docs/todo/P02M0193.md:100), and for a demo that ["prints how many lanes it used"](/data/yellow/libersystem/docs/todo/P02M0193.md:98).

   How the 3D precedent works:

   - It runs at [64x48](/data/yellow/libersystem/src/kernel/test_suites/applications.rs:2575).
   - It asserts only ["shading on 4 worker(s)"](/data/yellow/libersystem/src/kernel/test_suites/applications.rs:2600), which the demo derives from [the pool's threads plus one](/data/yellow/libersystem/src/user/apps/tools/src/test3d_sw.rs:1143) and not from the work shared.
   - It works because soft3d's tile is [32 pixels](/data/yellow/libersystem/src/user/libs/graphics/soft3d/src/raster.rs:32), so 64x48 is four tiles, one per lane.

   Copied to 2D, it tests nothing:

   - soft2d's tile is [64 pixels](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/lib.rs:54) and the tiling [rounds up](/data/yellow/libersystem/src/user/libs/graphics/soft2d/src/tile.rs:36), so 64x48 is one tile and one band.
   - `rt::pool::for_each` wakes at most [`items - 1` helpers](/data/yellow/libersystem/src/user/runtime/rt/src/pool.rs:302). A one-unit frame therefore runs entirely on the demo's thread, and the "pooled" pass is the serial walk compared with itself.
   - A lane count that reports how many lanes the frame ran with would still say four.

   Even the 2D harness's [192x128 scanout](/data/yellow/libersystem/src/kernel/test_suites/services.rs:3176) gives only two 64-row bands, fewer than four workers, if the measurement chooses bands. The host suite's ["crowded one"](/data/yellow/libersystem/docs/todo/P02M0193.md:83) states no extent either, and "backwards" and "handing units out twice" mean nothing on a one-unit frame.

   **Correct the `test2d-sw` and differential items.** State three things:

   - The guest test's surface is cut into at least as many units as the workers it asks for, whichever unit the measurement chooses. Four 64-row bands need 256 rows.
   - The demo prints the frame's unit count beside its lane count, and the test asserts both.
   - The host suite's crowded scene has the same floor.

Validation: I did a read-only inspection of the plan and its history in git (`07371c44`, `0dd5da07`), the audit file, P02M0189, P02M0103 and the source files cited above. The lane arithmetic uses only the code's own constants and the bench's frozen scene. No plan, source or audit file was modified, and nothing was built, tested, benchmarked or booted.
