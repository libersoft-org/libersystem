# The 3D rendering stack

WHAT THIS DOCUMENT IS. An overview of how a frame is rendered in this system and where the boundaries
are. It states no rule of its own: every normative answer - what a shader operation computes, how a
position is quantised and clipped, where a sample sits inside a pixel, what a depth format rounds to,
which features a conforming backend has - is in a GENERATED profile document bound to its registry by
hash, and this one links to it rather than repeating it. A second copy of a rule is a copy that
drifts.

| document | what it freezes |
| --- | --- |
| [`graphics/RENDER3D_PROFILE_1.md`](graphics/RENDER3D_PROFILE_1.md) | the closed feature list, the sample positions, the clip volume, the depth-format table, the quantised position and the minimum limits |
| [`graphics/SHADER_IR_1.md`](graphics/SHADER_IR_1.md) | the shader model: every operation, its operand order, its rounding, the no-FMA policy, non-finite handling and the transcendental accuracy table |
| [`graphics/SCENE3D_PROFILE_1.md`](graphics/SCENE3D_PROFILE_1.md) | the retained scene model: hierarchy, cameras, lights, queues, culling, picking and its limits |
| [`graphics/IMAGE_COLOR_PROFILE_1.md`](graphics/IMAGE_COLOR_PROFILE_1.md) | the colour model every image and attachment carries |
| [`GRAPHICS.md`](GRAPHICS.md) | the names, the layer stack and the hashing rule these documents live under |

## The model, in one paragraph

An application describes a frame as `render3d` values: resources, a pipeline with a programmable
vertex and fragment stage, attachments with their load and store operations, and a list of draws. A
BACKEND turns that description into pixels in two phases: `prepare` validates the shader modules
against the profile, checks the limits, lowers the pipelines and RESERVES every piece of scratch the
frame will need; `execute` runs the transform, the clip, the rasteriser, the fragment stage and the
attachment write, and in steady state allocates nothing. `soft3d` is the CPU backend and implements
the whole of `Render3D Core Profile 1`; a feature in the profile it cannot execute is a defect and
never an `Unsupported`. `scene3d` sits above the API as the retained layer that GENERATES those
descriptions for an application that would rather describe a scene than a command list.

WHY THE API IS IN THE MIDDLE. `render3d` is backend-neutral by construction: it contains no
rasteriser, no display and no allocation on a decision path, which is what lets a second
implementation plug under it without the application knowing - and what lets most of its own suite
run with no backend at all.

## The split the whole backend is organised around

**Geometry is BIT-EXACT and shading is not.** Coverage, clipping topology, the depth value a fragment
carries and the identity it writes are integer or quantised arithmetic and must be identical on every
architecture; the colour a fragment computes goes through `f32` transcendentals and is compared
within a tolerance. Mixing the two is what makes a conformance comparison either impossible to
satisfy or worthless.

The enforcement is at the SHADER, not at the rasteriser: `render-shader`'s validator walks the
complete dependency slice of every position - data and control, including the condition of every
enclosing branch and every index that selects a contributing value - and refuses at module load an
operation with no zero-ULP definition in the frozen accuracy table. "This shader is not
deterministic" is not something anybody can act on; the refusal names the operation, its bound, and
how many steps from the position it was found.

## What a frame is made of

* **Resources.** Buffers, 2D textures, cube maps, 2D arrays, 3D textures, samplers, mipmaps and
  render targets, each with its usage and its host visibility stated rather than inferred.
* **Geometry.** `u16` and `u32` indices, multiple vertex streams, configurable attributes, triangle
  list/strip/fan, line list/strip, point list, primitive restart, indexed and non-indexed draws,
  instancing, base vertex and base instance.
* **Pipelines.** A programmable vertex and fragment stage in the validated IR, vertex layout,
  topology, rasteriser state, depth and stencil state, blend state PER ATTACHMENT, colour write
  masks, viewport and scissor.
* **Passes.** Load (clear, load, discard) and store (store, discard) per attachment, multiple colour
  attachments, a depth-stencil attachment, MSAA resolve, offscreen targets and render-to-texture.
* **Depth and stencil.** `Depth16`, `Depth24`, `Depth32F`, `Depth24Stencil8` and `Depth32FStencil8`,
  every compare operation, depth write and bias; stencil compare, read and write masks, the three
  operations and separate front and back state.
* **Multisampling.** 1x, 2x and 4x with the profile's own sample positions, sample masks,
  alpha-to-coverage and resolve.
* **Sampling.** Nearest and linear minification, magnification and mip filtering, trilinear,
  clamp/repeat/mirror/border addressing, LOD bias and clamps, depth-compare sampling and anisotropic
  filtering.
* **Identity.** An INTEGER attachment written by the same fragment that wrote the colour, which is
  what picking is: an application asks what is under a pixel, and the only answer that survives
  lighting, texturing and transparency is one the rasteriser wrote at the same time.

## The boundary a GPU backend takes over

`soft3d::frame`'s two entry points are the whole of it:

```rust
fn prepare(limits: Render3DLimits, pipelines: Vec<Pipeline>, draws: Vec<Draw>, width: u32, height: u32) -> Result<Prepared, Error>;
fn execute(prepared: &mut Prepared, attachments: &mut Attachments<'_>, source: &dyn Source) -> Result<Stats, Error>;
```

A Vulkan, Gallium or Mesa-backed implementation replaces exactly this and nothing above it: it takes
the same pipelines, the same draws and the same `Source` for vertex attributes, uniforms and texture
reads, and writes into the same attachments. What it must NOT change is what the frame means - the
profile documents are the contract both backends answer to.

THE API IS EXPERIMENTAL UNTIL A SECOND BACKEND EXISTS, and that is this milestone's own rule rather
than a caveat added here: a shape with fewer than two real consumers is a guess, and `render3d` has
exactly one. It may be designed, implemented and used; it may not be declared stable.

## Memory, and the controls over it

Everything a frame costs is a function of the extent and the limits, and both are stated rather than
discovered:

* **The attachments** are the caller's. A colour attachment is sixteen bytes a pixel a sample - four
  singles, because the working space is `f32` throughout and a half would round between every
  composite - and a depth-stencil is the format's depth bytes plus one stencil byte a sample. A
  frame with two colour attachments at 640x480 and no multisampling is therefore
  `640 * 480 * 16 * 2` plus `640 * 480 * 5`, which is 9,600 kB and 1,500 kB.
* **The backend's scratch** is worked out during `prepare` and refused up front: the clipper's
  bounded output, the bin lists, the interpolated varyings and the interpreter's value table. A frame
  that could start allocating in the middle is a frame whose cost depends on the allocator rather
  than on the geometry, and `soft3d`'s own suite asserts that a warmed frame asks the allocator for
  nothing.
* **The limits** are `Render3DLimits`, whose guaranteed minima are in the profile document - so
  "supports Profile 1" cannot mean "accepts four vertex attributes".
* **The shared image** an application composites into is its own, charged to its own Domain.

## Conformance

The suite is `user/libs/graphics/conformance3d`: **one scene per profile feature, each with its own
stated pass condition**, driven by the machine-readable registries rather than by a list somebody
remembered to write. A feature added to either profile with no scene is a FAILURE OF THE SUITE and is
reported by name, which is what keeps the two in step.

| profile | features | scenes | result |
| --- | --- | --- | --- |
| `Render3D Core Profile 1` | 89 | 89 | conforms |
| `Scene3D Core Profile 1` | 71 | 71 | conforms |

**Nothing here is compared against a golden image.** A frame compared against a stored one fails for
every reason at once and says nothing about which feature broke; worse, a baseline captured from this
backend agrees with this backend by construction, which makes it a regression test and not a
correctness oracle. Every scene states a property that can fail for ONE reason: two triangles across a
shared edge leave neither a gap nor a doubly covered pixel; a scissor removes fragments and shades
none of them; the transparent queue runs back to front; a normal under a squash stays perpendicular to
its surface; a light at its range delivers exactly zero.

**And `Unsupported` is a failure, not a gap.** A profile is a closed list, so a backend that refuses a
Profile 1 frame is not a backend with something missing - it is a backend that does not conform. The
run reports the two apart so a reader can tell "this is wrong" from "this is missing", and fails for
either.

The same library runs in two places, which is the point of it being a library:

| where | what it proves |
| --- | --- |
| `cargo test -p render3d-conformance` | every scene passes and every profile entry has one, on the machine that builds the tree |
| `bin/test3d-conformance-sw.lsexe` | the same scenes inside a booted guest, on the target's own floating point |
| `kernel.applications.the_3d_profiles_conform_on_the_target` | that guest run, governed, with each profile's count asserted separately |

**Two tallies and not one.** An implementation may carry the command layer and not the retained layer
above it, so a single number could not say which conformed; the run prints a line per profile and the
gate asserts both.

What else is proved, and by what:

| claim | where |
| --- | --- |
| the backend's own arithmetic - clipping, coverage, depth, texture addressing, blending, the interpreter | `cargo test -p soft3d`, 81 host tests |
| the scene layer's own arithmetic - transforms, bounds, queues, lighting, picking | `cargo test -p scene3d`, 54 host tests |
| the shader model's refusals and the strict-float dependency slice | `cargo test -p render-shader` |
| the whole path from a scene to a surface, against real services | `kernel.applications.the_3d_demo_renders_a_lit_scene_and_survives_a_resize` |
| the picture, off a real screen | `./check.sh --gate qemu-3d-demo` |

### What the suite found

A conformance suite that finds nothing is a suite written against the implementation. This one was
written against the registries and found three things the backend and the API did not do:

* **`Scissor` was absent from `soft3d` entirely.** It is a mandatory Profile 1 feature; the rasteriser
  had a viewport and no scissor at all, and `CommandList::set_scissor` recorded a command nothing
  executed. It is implemented now, as a bound on the raster loop rather than a guard on the write - a
  fragment outside the rectangle is never shaded, which is most of what a scissor is for.
* **`VertexLayout::validate` admitted two attributes at one location.** A stage reads a location and
  gets one value, so a layout offering two is a description with two answers to one question, and
  which one a backend picks is exactly the sort of thing two backends decide differently.
* **A texture was never sampled from inside a draw.** Every sampling scene read the entry point
  directly - deliberately, so a failure cannot be the interpolator's - which left a backend whose draw
  path never reached the sampler passing all of them. Each entry point is now also read once from
  within a pass.

## The application, and what it looks like running

`bin/test3d-sw.lsexe` is the interactive proof and the 2D/3D interop proof at once: a rotating
indexed cube with flat per-face normals and visibly distinct per-face colours above a ground plane,
lit by ambient plus a directional light plus a point light that circles the scene, with a specular
highlight, a perspective camera and a dark non-flat background. One cube face carries a
procedurally generated checkerboard read through a CLAMPING sampler and the ground a repeating one
read through a REPEATING sampler; a semi-transparent panel stands in front, blended source-over with
the depth test on and the depth write off. Every surface writes its object identity into an integer
attachment beside the colour one, and `P` shows that buffer instead of the scene.

ONE FRAME TRAVELS `render3d -> soft3d -> OwnedImage -> render2d -> soft2d -> Surface`. The 3D half
renders into an image in the shared image model; the 2D half composites a heads-up display over it -
a translucent rounded panel, an antialiased indicator and a line of text as a glyph run - and the
result is presented. That is the path a game, an editor and a map application all take, and it is the
one nothing else in this tree exercises end to end.

It holds `display` and `input-keys` and nothing else: every vertex is computed, every texture is
generated and every glyph form is carried by the program, so it needs no volume, no font catalogue
and no storage.

**The controls.** Esc or `q` exits, Space pauses, `R` resets, `P` toggles the picking overlay, the
arrow keys orbit the camera and `+`/`-` change its distance. The deterministic controls are launch
ARGUMENTS and not keys, because a key that changed the run would be a key a person could press by
accident: `--frames N` ends the run after N presents, `--pose N` fixes the rotation to a stated
value, `--no-input` skips the key subscription, and `--scene-width`/`--scene-height` fix the extent
the scene renders at rather than following the window - which is the control a person reaches for on
a machine that renders slowly, and the one the pixel gate uses.

Two gates watch it. `kernel.applications.the_3d_demo_renders_a_lit_scene_and_survives_a_resize` runs
it against stand-in display and input services and reads what it did: a surface opened, frames that
DIFFER presented repeatedly, a surface moved under it and rebuilt for without the camera or the
animation moving, a key delivered through the focus capability its surface minted, and a clean exit.
It also runs the demo with the display grant REFUSED, which must be a program that says so and
leaves rather than one that waits for a capability nobody is going to send.

`./check.sh --gate qemu-3d-demo` boots a guest and checks PIXELS: three timed frames that differ, a
lit object standing in front of the horizon in a bounded central region, a ground whose texture
crosses its own mean several times along one line, a region tinted towards the translucent panel that
still carries what is behind it, an overlay panel lighter than the sky with a line of letters in it,
two STATED POSES whose visible faces the checker computes from the same camera the demo uses, two
aspect ratios, and a console the `q` key gave back.

TO SEE IT: `./lab.sh boot` then `./lab.sh sh "test3d-sw --scene-width 320 --scene-height 240"`, and
`./lab.sh shot frame.png` for a picture. The screenshot the milestone asks this document to carry is
not committed here - this tree holds no binary assets under `docs/` - and the command above is what
produces one.

## Performance

The numbers and their conditions are in [`PERF.md`](PERF.md): the headless benchmark
(`./bench.sh --suite soft3d`) measures one frozen 640x480 scene at `--release` on the host, stage by
stage, and the live measurement (`test3d-sw --report`) reports what a frame costs inside a guest at
three sizes. Both are `--release`: `build-shared` compiles every staged PIE and every provider
library that way.

**THE 30 FPS FLOOR AT 640x480 IS NOT MET.** The live frame is 1185 ms where the floor is 33, and the
benchmark locates the cost precisely: the shader interpreter spends about a hundred and twenty-five
cycles per IR instruction, and a measurement with the arithmetic removed shows that essentially all
of it is the value plumbing rather than the computation. Three measured improvements are in; closing
a gap of this size needs the two structural changes the performance document names - a register-file
interpreter and parallel tile execution - and neither is a tuning pass. The number is recorded unmet
rather than the floor lowered.

## The tri-architecture matrix

| what | x86_64 | aarch64 | riscv64 |
| --- | --- | --- | --- |
| guest demo gate (presents, resize, key, denied capability) | pass | not run | not run |
| live pixel gate (`qemu-3d-demo`) | pass | not applicable | not applicable |
| guest conformance run (`test3d-conformance-sw`) | conforms | not run | not run |

THE HOST SUITES ARE NOT IN THAT TABLE, deliberately: they run on the machine that builds the tree and
say nothing about a target. What would make a three-architecture claim is the GUEST row, and its other
two cells are empty - the ports have not been run for this part. This table is what it is until they
are, and "the profiles are implemented on all three architectures" is not a sentence this document
says while two thirds of that row is blank.

The live pixel gate is x86_64 only by design: it drives a guest through the display and reads its
framebuffer, and the other two ports are emulated at minutes per frame.
