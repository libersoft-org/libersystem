# The 2D drawing stack

WHAT THIS DOCUMENT IS. An overview of how a drawing is made in this system and where the boundaries
are. It states no rule of its own: every normative answer - what an operator computes, how a gradient
interpolates, what a clip's antialiased edge is, which features a conforming backend has - is in a
GENERATED profile document bound to its registry by hash, and this one links to it rather than
repeating it. A second copy of a rule is a copy that drifts.

| document | what it freezes |
| --- | --- |
| [`graphics/RENDER2D_PROFILE_1.md`](graphics/RENDER2D_PROFILE_1.md) | the closed feature list, its limits and its conformance thresholds |
| [`gen/render2d-spec/profile-1.canonical`](gen/render2d-spec/profile-1.canonical) | the numeric semantics: every operator and blend equation, the flattening tolerance, the boolean answers, the shape conventions and the per-node filter contract |
| [`graphics/IMAGE_COLOR_PROFILE_1.md`](graphics/IMAGE_COLOR_PROFILE_1.md) | the colour model every image carries: spaces, transfers, alpha, YUV and the conversions between them |
| [`GRAPHICS.md`](GRAPHICS.md) | the five names, the layer stack and the hashing rule these documents live under |

## The model, in one paragraph

An application records a drawing into a `Canvas`. A canvas produces a `DrawList`: an immutable,
versioned, bounded list of commands with a typed resource table beside it - paths, gradient stops,
images, dash patterns, glyph runs and filter graphs - and it draws nothing at all. A BACKEND turns
that list into pixels in two phases: `prepare` flattens curves in device space, strokes, bins the
work into tiles, builds image pyramids, resolves bounds and RESERVES its scratch; `render` replays
the prepared form into an `ImageViewMut` and allocates nothing. `soft2d` is the CPU backend and
implements the whole of Profile 1; a command in the profile it cannot execute is a defect and never
an `Unsupported`.

WHY THE LIST IS IN THE MIDDLE. It is what makes the same drawing replayable, cacheable, damage-
analysable and testable with NO backend at all - most of `render2d`'s own suite runs without one -
and it is what a second backend plugs under without the application knowing.

## What a drawing is made of

* **Geometry.** Paths of move/line/quad/cubic/close under the non-zero or even-odd rule, and the
  shape constructors: rectangle, rounded rectangle, circle, ellipse, arc, line, polyline, polygon.
  Where each shape STARTS and which way it is wound is frozen in the spec, because a dash phase, a
  stroke's first cap and a hit test all depend on it.
* **Strokes.** Width, three caps, three joins, the miter limit, and a dash pattern with a phase.
  Where the width is measured under a transform - user space or device space - is a stated choice
  (`StrokeScaling`) rather than a backend's habit.
* **Paints.** Solid, linear, radial and conic gradients with multi-stop, clamp/repeat/mirror spread
  and a transform of their own, and image patterns. Stops interpolate in LINEAR light.
* **Clips and layers.** Rect, rounded rect, path, nested, alpha-mask and inverse clips; layers with
  group opacity, a blend mode and bounded nesting.
* **Compositing.** The twelve Porter-Duff operators plus additive `Plus`, every separable blend mode
  and the four non-separable ones.
* **Images.** Source and destination rectangles, arbitrary projective transforms, nearest, bilinear,
  bicubic and mipmapped sampling, three wrap modes, opacity, and colour-space conversion.
* **Filters.** A bounded acyclic GRAPH - blur, offset, colour matrix, flood, composite, blend,
  keep-inside, convolution, dilate, erode, displacement map, crop, tile and the BACKDROP - each node
  declaring the input rectangle it needs for a given output rectangle, which is what makes a blur
  over a small dirty region cost a small blur.
* **Text.** Glyph RUNS and never strings: outlines, grayscale and subpixel masks, bitmap strikes,
  colour-layer glyphs, embedded colour bitmaps, glyph transforms and subpixel positioning. The
  shaping stack above the run seam - `font-shape`, `text-layout`, `text-pipeline` and the
  `font-catalogue` - is its own stack with its own conformance run (`bin/textconf.lsexe`) and is not
  this document's subject: what `render2d` owes is the DRAWING of a run.
* **Queries.** Path boolean operations, hit testing against a fill or a stroke, tight and stroke
  bounds, path length, and the point and tangent at a distance.

## The boundary a GPU backend takes over

`render2d::backend::Backend` is the whole of it:

```rust
fn identity(&self) -> (&'static str, u32);
fn prepare(&mut self, list: &DrawList, target: &TargetDescription) -> Result<Self::Prepared, Error>;
fn render(&mut self, prepared: &Self::Prepared, target: &mut ImageViewMut<'_>) -> Result<(), Error>;
```

A Vulkan, Gallium or Mesa-backed implementation replaces exactly this and nothing above it: it takes
the same `DrawList`, prepares its own form, and writes into the same target description. What it must
NOT change is what the drawing means - the profile documents are the contract both backends answer to,
and the conformance suite below is how a second backend demonstrates that it does.

The prepared form is bound to the backend's identity and version and to a `PreparedKey` covering the
profile hash, the target format, colour space, extent and scale, the resource generations and the
glyph-cache generation. A mismatch is a typed re-prepare requirement naming what changed, never a
silent re-preparation.

## Memory, and the controls over it

* **The list** is bounded by `Render2DLimits`: commands, resources, path verbs and points, subpaths,
  clip and layer depth, filter nodes and radius, glyphs per run, image extent, layer pixels, prepared
  scratch, cache bytes and display-list bytes. The guaranteed minima are in the profile document, so
  "supports Profile 1" cannot mean "accepts ten path points".
* **The backend's scratch** is worked out during `prepare` and refused up front. `soft2d` composites
  in 64-pixel tiles, so a layer, a clip mask and a filter intermediate are sized to a tile plus what a
  filter reaches past it rather than to the surface: a four-thousand-pixel-wide drawing with three
  nested layers costs three tiles of scratch and not three screens.
  As a formula: `tile_pixels = 64 * 64`, and a layer costs `tile_pixels * 16 bytes` in the canonical
  intermediate - four singles per pixel - grown on each side by a filter's own declared reach. A clip
  MASK is one byte per pixel and only when it is not a rectangle: an axis-aligned pixel-aligned clip
  is two corners and no storage at all.
  THE INTERMEDIATE WAS FOUR HALVES AND IS FOUR SINGLES (2026-09-15). There is no hardware half
  conversion in reach of a `no_std` build here, so every read and write of a working pixel went
  through a branchy software routine twice per channel; doubling the scratch removes eight
  conversions per pixel per access, and it is MORE accurate rather than less, because the arithmetic
  above it was `f32` throughout and the half rounded between every pair of composites.
* **The glyph cache** is bounded and evicts in insertion order, which for text is close to
  least-recently-used and is deterministic: an eviction that depended on a clock would make two runs
  of one drawing do different work.
* **The application's own images** are its own: a surface's presentable images are created by the
  client, charged to the client's Domain, and imported by the display service.

## Conformance

`Render2D Core Profile 1` is a closed list of 110 features, and the suite walks it entry by entry:
`user/libs/graphics/conformance2d` holds one scene per feature with its own stated pass condition,
plus two scenes for the output path that is not a feature - wide-gamut composition and the dither at
the end of a frame. The scenes are DRIVEN BY THE REGISTRY: each carries a `@covers:` marker, and a
feature added to the profile without a scene fails the generator's coverage gate.

`bin/test2d-conformance-sw.lsexe` is the same library run inside a booted guest, and it holds no
capability at all.

| where | result |
| --- | --- |
| host (`cargo test -p render2d-conformance`) | 112 scenes, all pass |
| guest, x86_64 | `112 passed, 0 failed, 0 unsupported, 0 untested` - conforms |
| guest, aarch64 | `112 passed, 0 failed, 0 unsupported, 0 untested` - conforms |
| guest, riscv64 | `112 passed, 0 failed, 0 unsupported, 0 untested` - conforms |

`Unsupported` is a FAILURE here: the profile is closed, so a backend refusing a Profile 1 drawing is
not a backend with a gap.

## The application, and what it looks like running

`bin/test2d-sw.lsexe` is the interactive proof: one scene made of the things a real drawing is made
of - filled and stroked Beziers under both fill rules, a shallow antialiased edge, a projectively
transformed image, a multi-stop conic gradient, a rounded-rectangle clip with content scrolling under
it holding a nested and an inverse clip, a group-opacity layer, three blend modes including a
non-separable one, a backdrop-blurred panel and a line of text with a colour glyph - drawn through
`render2d` into a real surface's images, paced by the frame loop. It holds `display` and `input-keys`
and nothing else.

Two gates watch it. `kernel.services.the_2d_demo_draws_a_real_scene_with_real_damage` runs it against
a real DisplayService and reads the DAMAGE at the device end: the multi-rect phase's two distant
regions arrive as TWO rectangles, a resize is survived by a rebuild, a SCALE CHANGE is told apart
from that resize and survived too, the screen is taken away and given back with nothing presented in
between, and every surface it held is reclaimed when its process ends.

A SCALE CHANGE AND A RESIZE ARE BOTH A NEW GENERATION, and what tells them apart is the
configuration: a surface keeps the LOGICAL extent it has - a window is the same size on the desk -
and its PHYSICAL extent becomes that times the output's ratio, so the same logical extent with a
different ratio is a scale change and nothing else is. The ratio is the SYSTEM's to choose:
`display-admin`'s `set-scale` sits beside `set-visible` because a client that could set it would be
deciding how much memory every other client's images need. `./check.sh --gate qemu-2d-demo` boots a guest and checks PIXELS: three timed frames that
differ, partially covered pixels along the shallow edge, a group-opacity overlap that is a mix of its
children, a filtered image with more than two shade bands, and a text row carrying both white glyphs
and a colour one.

TO SEE IT: `./lab.sh boot` then `./lab.sh sh "test2d-sw"`, and `./lab.sh shot frame.png` for a
picture. The screenshot the milestone asks this document to carry is not committed here yet - this
tree holds no binary assets under `docs/` - and the command above is what produces one.

## Performance

The numbers and their conditions are in [`PERF.md`](PERF.md): the headless benchmark
(`./bench.sh --suite soft2d`) measures four frozen 640x480 scenes at `--release` on a stated
reference host, and the live measurement (`test2d-sw --size=640x480`) reports what a frame costs and
what the loop's present interval came to inside a guest. BOTH ARE `--release`: `build-shared` compiles
every staged PIE and every provider library that way, which the performance document used to deny.
They are still reported separately, because one has a real display, a real service and a real present
path in it and the other has none of the three - and only the headless one is a floor.

A HiDPI figure is beside them, which is the same scene at the same logical size with its physical
extent doubled. It is measured in the guest suite rather than the live boot for a reason worth
knowing: nothing in a booted image SETS a scale yet, because the only thing holding the display admin
channel is PermissionManager. That is a missing operator control, not a missing capability.

## The tri-architecture matrix

| what | x86_64 | aarch64 | riscv64 |
| --- | --- | --- | --- |
| guest conformance run (`test2d-conformance-sw`) | conforms | conforms | conforms |
| guest demo gate (damage, resize, visibility, reclamation) | pass | pass | pass |
| live pixel gate (`qemu-2d-demo`) | pass | not applicable | not applicable |

THE HOST SUITES ARE NOT IN THAT TABLE, deliberately: they run on the machine that builds the tree and
say nothing about a target. What makes the three-architecture claim is the GUEST row - the same
scenes, the same arithmetic, inside a booted system on each port - and it is the row a reader should
look at.

The live pixel gate is x86_64 only by design: it drives a guest through the display and reads its
framebuffer, and the other two ports are emulated at minutes per frame. What it proves there is the
PICTURE, which is not architecture-specific once the conformance row above holds on all three.
