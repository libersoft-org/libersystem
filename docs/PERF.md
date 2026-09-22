# Performance notes

Measured numbers for the changes whose goal includes a before/after
comparison. Methodology per entry; machine noise applies, so treat the times as
orders, not precision instruments.

## The 2D demo, live, at 640x480 (2026-09-15)

`test2d-sw --frames=600 --phase-frames=60 --size=640x480` inside a booted guest, reporting its own
two numbers: what a FRAME COSTS - record the list and replay it into the surface's image - and what
the loop's present INTERVAL came to with the display's pacing in it. They are different claims, and a
demo that reported only the second would look fast on a machine that throttled it.

| what | mean | worst |
| --- | ---: | ---: |
| draw (record + replay) | 79.6 ms | 104.3 ms |
| present interval | 132.4 ms | 174.7 ms |

Re-measured 2026-09-15 after the three backend changes recorded under `soft2d` below; it was 88.3 ms
mean and 116.4 ms worst, with a 141.4 ms interval. The demo's own scene gains less than the
benchmark's does, and the reason is worth knowing: its background is a four-stop CONIC GRADIENT, so
no tile of it is covered by an opaque solid fill and the largest of the three changes does not apply.
What it does gain is the working-space one, which its backdrop blur pays for per pixel.

**THIS IS THE RELEASE BUILD, ON THE TARGET, and the note here used to say the opposite** (corrected
2026-09-15). It claimed the debug profile because `./build.sh` builds the static services and drivers
at `dev` - which it does - and the staged PIE applications do not come from there: `build-shared`
compiles every consumer object and every provider library with `--release`, into an image target
directory that has no `debug` tree at all. So this IS the release measurement on the target that the
milestone asks for, and it is comparable with the headless benchmark below, which measures the same
profile on the host.

It is still not a FLOOR. The floor is the headless benchmark's, under its own frozen reference-host
conditions; this is a live figure with a real display, a real service and a real present path in it,
and it is reported separately for that reason. What it is good for beyond the number is the SHAPE:
the draw is most of the interval, so what the loop waits for is the renderer rather than the display.

### At a HiDPI scale

**The same scene, the same logical layout, every edge resolved at more pixels.** `DisplayService`
answers `set-scale` on its admin channel now, so there is a second scale to measure at - and what a
HiDPI measurement asks is exactly this: not what a bigger scene costs, but what the SAME scene costs
when its physical extent is its logical one times a ratio. Measured inside the guest suite, where the
admin channel is part of the harness, by the gate that already runs every phase of the demo:

| what | logical | physical | draw mean | draw worst |
| --- | --- | --- | ---: | ---: |
| scale 1:1 | 192x128 | 192x128 | 20.6 ms | 29.6 ms |
| scale 2:1 | 192x128 | 384x256 | 28.1 ms | 31.0 ms |

The same pair on the two emulated ports, where four times the pixels costs about twice the time
rather than 1.4 times - an emulator charges per instruction, and the terms that do not grow with
resolution are the ones it makes cheapest to repeat:

| port | scale 1:1 | scale 2:1 | |
| --- | ---: | ---: | ---: |
| aarch64 (TCG) | 406.6 ms | 673.7 ms | 1.7x |
| riscv64 (TCG) | 496.4 ms | 876.5 ms | 1.8x |

**Four times the pixels for 1.4 times the time**, which is the useful part. A frame's cost is not
proportional to its area here: recording the list, walking the scene and the per-tile setup do not
grow with resolution, and only the coverage and composite terms do. The ratio is what a HiDPI budget
should be planned against - doubling the scale is not doubling the frame - and it is measured rather
than assumed.

**Nothing in a booted image sets a scale yet.** The ratio is the system's to choose and only something
holding the whole screen may set it, which today is PermissionManager's admin connection and nothing
above it: there is no compositor and no operator control. That is why this row is measured in the
guest suite rather than in the live boot above, and it is a missing CONTROL rather than a missing
capability.

**The same demo inside the guest suite, at 192x128, on all three ports.** The gate
`kernel.services.the_2d_demo_draws_a_real_scene_with_real_damage` runs the same scene against a real
DisplayService with a stand-in GPU, and the demo reports its own draw time there too. The two
emulated ports are TCG - x86_64 has KVM - so what this table measures is the EMULATOR, and it is
here because it is the number a reader will otherwise mistake for the port being slow.

| port | draw mean | draw worst | before 2026-09-15 |
| --- | ---: | ---: | ---: |
| x86_64 (KVM) | 20.6 ms | 29.6 ms | 29.9 ms |
| aarch64 (TCG) | 406.6 ms | 516.8 ms | 412.8 ms |
| riscv64 (TCG) | 496.4 ms | 564.5 ms | 523.8 ms |

**The backend changes are worth a third on x86_64 and almost nothing under TCG**, which is the shape
to expect rather than a disappointment. Three of the four remove WORK - a decode per tile, a mask per
clip, a per-column evaluation - and an emulator charges for instructions retired rather than for the
memory traffic and the branch misses that make those changes worth making on real silicon. The fourth,
the wider intermediate, moves MORE bytes for fewer conversions, which under TCG is close to a wash.
The rows are here so a guest run on those ports that looks stuck can be recognised as the emulator.

Fourteen times and seventeen times the x86_64 figure, on a scene an eighth the area of the 640x480
measurement above - which is the ratio to remember whenever a guest run on those ports looks stuck.

## soft3d, the CPU 3D backend, stage by stage (2026-09-18)

`./bench.sh --suite soft3d` on the host, release, with the frozen benchmark scene: 192 triangles and
384 vertices at 640x480, arranged in a four-by-four grid that OVERLAPS on screen and whose nearest
column crosses the near plane, so the clipper and the overdraw are real rather than fixtures built to
look like them. Eight samples after two warm-up frames.

THE STAGES ARE MEASURED BY DIFFERENCE AND NOT BY A CLOCK INSIDE THE LIBRARY. `soft3d` is `no_std` and
has no clock; giving it one so a benchmark could read it would put a timer on every frame the system
ever renders. What the benchmark does instead is run the SAME geometry through five pipelines that
differ by exactly one stage each, and report the differences. The geometry row draws into a
one-pixel attachment, so every primitive is still transformed, clipped and culled and there is
nothing left to rasterise.

| stage | cumulative | attributable to |
| --- | ---: | --- |
| geometry | 0.9 ms | transform, clip and cull |
| raster | 144.8 ms | rasterisation, depth test, interpolation and attachment write |
| shading | 661.1 ms | the lit fragment stage |
| texturing | 945.3 ms | two bilinear texture samples per fragment |
| blending | 948.4 ms | the blend equation |

442,474 fragments a frame, 33 primitives clipped, 141 culled: **202 triangles/s and 466,566 shaded
fragments/s**, which is 1.05 frames a second at this workload.

### Where the fragment cost actually is, measured rather than reasoned about

`soft3d-bench --interpreter` runs a fragment module against a stub with no rasteriser in front of it,
which is the measurement that says whether a change to the interpreter did anything: a frame
measurement moves by a few percent for a change that halved it.

| module | statements | per run | per statement |
| --- | ---: | ---: | ---: |
| a constant colour | 4 | 139 ns | 35 ns |
| the lit stage | 36 | 1249 ns | 35 ns |
| the lit stage with two texture reads | 41 | 1448 ns | 35 ns |
| 36 composes, which read four operands and compute NOTHING | 36 | 1500 ns | 42 ns |

**The last row is the finding.** Thirty-six instructions that do no arithmetic at all cost MORE per
instruction than the lit stage's, so essentially none of the time is the arithmetic: it is the value
plumbing around it - reading operands out of the value table, building the `Val` an operation
returns, returning it through a `Result`, and moving it into its slot. A four-component add is four
multiplies and about two hundred and fifty bytes of memory traffic.

### The three changes this measurement bought, and the one it rejected

- **Operands are read by reference.** `read` returned an owned `Val` - a type plus sixteen inline
  words, about ninety bytes - for every operand of every instruction of every fragment; the lit stage
  read two per instruction over forty-odd instructions, which is seven kilobytes copied per FRAGMENT
  and a type's clone run ninety times. Every arm either reads the value component-wise or copies it
  into a fixed buffer, so a borrow serves them all; the two that genuinely need an owned value clone
  at the one place they need it. **1521 ms to 1100 ms.**
- **The perspective denominator is computed once per fragment.** It does not depend on the value
  being interpolated, and a stage with four `vec4` varyings was computing the identical number
  sixteen times - three multiplies, two adds and a finiteness test each. The DIVISION stays per
  component, because `a / b` and `a * (1 / b)` are not the same `f32` and this crate's geometry half
  is bit-exact by rule. Worth about a percent here and more on a stage with more varyings.
- **Four-word inline values were measured and REJECTED.** A fragment stage computes scalars and
  vectors, so four words would hold every value it produces and would halve a `Val`; the benchmark
  said that was worth about six percent. What it would also do is push every `mat4` onto the heap,
  and a vertex stage reads two matrix uniforms per vertex - so the saving is bought with an
  allocation per vertex per frame, against a crate whose stated property is that a warmed frame asks
  the allocator for nothing. Six percent is not what that is worth.

Together: **1521 ms to 1049 ms, a factor of 1.45** on the same frozen scene.

### Three more measured on 2026-09-18, of which two were rejected

THE NOISE FLOOR ON THIS HOST IS ABOUT EIGHT PERCENT at this workload - four consecutive runs of the
unchanged tree gave 1082, 1089, 1098 and 1182 ms - so a change is only a change when it moves the
frame further than that, or moves an interpreter row, which is far quieter.

- **The value table is stamped rather than cleared, and it did NOT move the frame.** Running a module
  used to `clear` the table and `resize(n, None)`, which WRITES every slot: an `Option<Val>` is about
  ninety bytes, so a forty-value module memset three and a half kilobytes before running a single
  instruction - once per covered pixel, four hundred thousand times a frame. It is now a run counter
  and a stamp per slot, so resetting is one increment. **1095 ms against a 1095 ms baseline**: the
  per-run reset was not where the time was. The change is KEPT because it is strictly less work per
  fragment and removes an `Option` from the hot path, and it is recorded here because the DISPROOF is
  what narrows the search: the cost is inside the instruction loop and not around it.
- **Building a value in one pass instead of two was measured and REJECTED.** `Words::from_slice` wrote
  its sixteen words twice - once as zeros from `[0; 16]` and once as the value from `copy_from_slice` -
  and building the array in a single `from_fn` pass should have halved that. It made things WORSE:
  1173, 1174 and 1198 ms against a 1089 ms baseline, and the compose row went from 51 to 55 ns a
  statement. A `copy_from_slice` of a few words is a call the compiler turns into a sized move, and
  `from_fn` over sixteen indices is sixteen bounds-checked reads it did not.
- **Moving the caller's buffer into the value was measured and REJECTED TOO, and it is the
  interesting one.** Every arithmetic arm computes into a fixed sixteen-word buffer and then hands it
  to `from_slice`, which copies it again; taking the array BY VALUE makes that a move. The compose row
  improved from 53.7 to 43.0 ns a statement - a real 20 percent on the pure-plumbing case, the largest
  single interpreter improvement measured since the borrow - and the FRAME got slower: 1142 to 1146 ms
  against 1082 to 1089. The lit and textured rows moved the wrong way too, 43.9 to 45.8 and 43.6 to
  46.3. Passing sixteen words by value forces a sixty-four-byte copy where a slice of four let the
  compiler copy four, so it helps exactly the case that fills all sixteen and costs every case that
  does not. **A change that improves the micro-benchmark and regresses the frame is a change that was
  measured on the wrong thing.**

### The register file, which is the fourth change and the largest since the borrow

**1095 ms to 959 ms on the frame, and 44 to 35 nanoseconds a statement on the lit stage** - a fifth
off the module the frame actually runs, and the interpreter rows moved together rather than one of
them moving: 36 to 35 on a constant colour, 44 to 35 on the lit stage, 44 to 35 with two texture
reads, 54 to 42 on the pure-plumbing composes.

WHAT IT REPLACED. The value table was `Vec<Option<Val>>`, and a `Val` is a type plus sixteen inline
words - about ninety bytes, with drop glue on two of its fields. Every instruction MOVED one out of
the arithmetic, through a `Result`, and into its slot: a four-component add wrote four useful words
and copied about two hundred bytes around them, once per instruction per COVERED PIXEL. The words are
a flat arena now, sixteen a slot, and an operation writes the words it computed and nothing else -
sixteen bytes for that add instead of two hundred.

**AND THE TYPE IS STILL THERE, WHICH IS THE PART WORTH RECORDING.** The obvious form of this change -
the one the milestone item names - is that a runtime need not carry a type per value at all, because
`Module::types` already states one. THAT IS NOT TRUE OF THIS IR AS IT STANDS: the declaration is not
authoritative. `render-shader`'s validator checks it in two specific places - that an indexed value is
an array, that a condition is a boolean scalar - and nowhere checks that the declared type of a value
matches what its operation produces. The interpreter has always used what the operation produced, so
reading the declaration instead would change what a module with a mismatched declaration does, and
would need a refusal the shader model does not have.

That is the shader model's question and not a performance pass's, so the type is written once per
assignment into a side table rather than being read from the module - which keeps the semantics
identical and still removes the ninety-byte move, which was the bulk of it. Making the declaration
authoritative is a separate change with a separate gate: it would need a type check over every
assignment, and it would REFUSE modules that run today.

### What is left, and what it would take

At 35 ns an IR statement the interpreter is about a hundred cycles per instruction. The register file
took the value MOVES out; what is left at that number is the per-instruction dispatch itself - a match
over the operation, a bounds-and-stamp check per operand, and a component loop that is four iterations
for a `vec4`. TWO routes remain - a third was measured and removed, below - and neither is a tuning
pass:

- **More than one thread.** The backend is tile-based and single-threaded, and the tiles are
  independent by construction. THIS IS THE ONLY ONE WITH THE FLOOR'S FACTOR IN IT: the gap is
  twenty-nine times and the other two below are worth a fraction each. The parallelism has to come
  from outside a `no_std` library that has no threads in it, which makes this an interface question
  before it is an optimisation.
- **Fewer instructions rather than faster ones.** Nothing in this stack folds constants, removes a
  dead assignment or fuses a multiply and an add in the IR before it runs. NOT WORTH ANYTHING ON THIS
  SCENE, and the reason is worth writing down: the benchmark's lit stage has no dead assignment and no
  operation whose operands are all constant, so a folding pass would remove nothing from it. It is
  still the right pass for shaders written by a generator rather than by hand, and it would be checked
  against the interpreter it feeds - but it is not what is between this backend and the floor.

**AND ONE OF THE THREE WAS MEASURED AND REMOVED FROM THE LIST (2026-09-19).** "A declared type that is
authoritative" was the obvious next route: `Module::types` states a type per value, so the
per-assignment type write looked like pure waste. It was probed by taking the type from the
declaration instead of writing one - correct on every module in the suite, which is itself the finding
that every declaration in this tree agrees with what its operation produces - and measured:
**960 ms against 959, and 34.8 nanoseconds a statement against 34.8.** Nothing. Only the
pure-plumbing compose row moved, 41.7 to 37.7, and the frame does not run that module.

So the route would have bought a type-inference pass and a refusal the shader model does not have, for
zero on the thing being optimised. It is off the list, and what that leaves is the one route with the
factor in it.

## The 3D demo, live, at three sizes (2026-09-18)

`test3d-sw --frames N --no-input --report` inside a booted x86_64 guest under QEMU/KVM, release, with
the scene rendered at the SURFACE's own size - which is what an application does, and what makes a
measurement at a stated resolution mean anything. A demo that always rendered at one internal size
and scaled would report the same 3D cost at every window size and call the difference a measurement.

| surface and scene | frame | rate | fragments |
| --- | ---: | ---: | ---: |
| 320x240 | 343.2 ms | 2.9 fps | 70,169 |
| 640x480 | 1184.5 ms | 0.8 fps | 280,592 |
| 800x600 | 1790.5 ms | 0.5 fps | 438,614 |

Per stage, at 640x480: the opaque scene 909.0 ms, the transparent pass 165.1 ms, the handover into
the shared image 8.0 ms, the 2D overlay 43.8 ms, the present 38.6 ms. The shape is the same at every
size: the 3D passes are about ninety percent of the frame, and everything the 2D half and the display
path do together is under eight percent of it. THE PRESENT IS FLAT at all three sizes - 38 ms
whatever the extent - which says what it is: the display's own pacing and not a copy whose cost grows
with the picture.

Memory is a function of the extent and is reported as one: two colour attachments at sixteen bytes a
pixel, a depth-stencil at five, and the shared image the overlay composites into at four.

| extent | colour | depth | scene image |
| --- | ---: | ---: | ---: |
| 320x240 | 2,400 kB | 375 kB | 300 kB |
| 640x480 | 9,600 kB | 1,500 kB | 1,200 kB |
| 800x600 | 15,000 kB | 2,343 kB | 1,875 kB |

## The 3D demo's EXTENDED phase, live, at three sizes (2026-09-22)

`test3d-sw --extended --frames 20 --no-input --report` inside a booted x86_64 guest under QEMU/KVM,
release - the same program, the same harness and the same three sizes as the core rows above, so the
two tables can be read line against line. What differs is the scene: `Scene3D Extended Profile 1`'s
own phase, a physically based sphere with a tangent-space normal map and a cast shadow, in place of
the core cube, ground and transparent panel. These are `f-ext`'s rows and no core gate reads them.

| surface and scene | frame | rate | shadow pass | lighting pass | fragments |
| --- | ---: | ---: | ---: | ---: | ---: |
| 320x240 | 489.2 ms | 2.0 fps | 40.1 ms | 375.7 ms | 74,561 |
| 640x480 | 1512.8 ms | 0.6 fps | 37.9 ms | 1362.4 ms | 242,760 |
| 800x600 | 2257.1 ms | 0.4 fps | 37.6 ms | 2081.0 ms | 368,889 |

THE SHADOW PASS IS FLAT AT ALL THREE SIZES - 38 to 40 ms whatever the window - and that is what it
is rather than a surprise: it draws through the LIGHT'S projection into the light's own 512x512 map,
whose extent is a property of the light and not of the surface. The present is flat for the same
kind of reason and at almost the same number, 37 to 38 ms, which is the display's pacing.

THE FRAGMENT COUNT INCLUDES BOTH PASSES, and the shadow pass contributes 18,512 of it at every size -
the sphere's silhouette in the light's map. So the lighting pass covers 56,049, 224,248 and 350,377
fragments at the three sizes, which is FEWER than the core scene covers at the same extents: a sphere
over a ground plane occupies less of the frame than a cube, a ground and a panel in front of it.

WHAT THE EXTENDED SHADING COSTS PER FRAGMENT, at 640x480 where the core's per-stage numbers are also
recorded: 1362.4 ms over 224,248 fragments is **6.08 us a fragment**, against the core scene's
1074.1 ms of opaque and transparent over 280,592 fragments, which is **3.83 us**. A factor of 1.59,
and the three things in it are named: GGX with Smith and Schlick in place of Lambert with a
Blinn-Phong highlight, a normal-map sample with a tangent frame re-orthogonalised per fragment, and a
shadow-map sample with its projective divide.

THE SHADOW PASS'S OWN FRAGMENTS ARE CHEAP AND ITS FRAME COST IS NOT ALL IN THE PASS. 37.9 ms over
18,512 fragments is 2.05 us a fragment - a divide and a compose, which is all its fragment stage does
- and beside it every frame clears 262,144 texels and reads the same number back into a texture. That
copy is the 25 ms the stages do not account for (489.2 against 465.1 summed, 1512.8 against 1487.5,
2257.1 against 2230.6): flat with the window, like the pass itself, and the price of a profile with
no depth-texture binding. A backend that could bind the attachment directly would not pay it.

Memory is the core table's plus the light's map: 512x512 at sixteen bytes a pixel is 4,096 kB for the
shadow attachment and 4,096 kB for the texture it is read back into, at every window size.

**THE FLOOR IN THIS FILE IS THE CORE DEMO'S AND THESE ROWS ARE NOT MEASURED AGAINST IT.** `f-ext` is
an optional part whose own completion clause asks for its own rows, and the milestone's frame budget
is closed by `i` over the core scene - which is why a shadow pass and a postprocess chain were kept
out of the benchmark scene deliberately. What these numbers are for is the SHAPE: what the Extended
material costs per fragment, and what a shadow map costs whatever the window.

WHAT IS NOT MEASURED HERE, AND WHY: the postprocess chain. `scene3d::postprocess` carries the
threshold, both kernels, the fog, the tone map and the pass graph that orders them, and no backend
runs a full-screen pass over an HDR target yet - so a bloom row would be a measurement of arithmetic
called in a loop written for the benchmark rather than of a pass the system executes. It is left out
rather than invented, and the HDR item in `P02M0103` records the same boundary.

**THE 30 FPS FLOOR AT 640x480 IS NOT MET AND THIS IS THE MEASUREMENT THAT SAYS SO.** The frame is
1185 ms where the floor is 33, which is a factor of thirty-six. The number is not a tuning gap: the
benchmark above locates the cost in the shader interpreter's value plumbing, and closing a gap of
that size needs the two structural changes it names - a register-file interpreter and parallel tile
execution - rather than more measurement. Recording the number here, unmet, is what keeps the floor a
floor.

## soft2d, the CPU 2D backend (2026-09-12)

`./bench.sh --suite soft2d` records five frozen scenes at 640x480 - four until 2026-09-19, when
`image-stress` split into `image-resample` and `image-convert` - and reports what a PREPARED
replay costs. It needs no surface, no DisplayService, no guest and no application: each scene is a
bounded `DrawList` recorded once and replayed into an `OwnedImage`, which is what makes the number a
person can get in a second on a host rather than a boot away.

**The reference host.** Intel Xeon Platinum 8272CL at 2.60 GHz, 100 logical CPUs, single-threaded
throughout - `soft2d` has no worker pool and Profile 1 does not ask for one. Built `--release` by
`rustc 1.93.1 (01f6ddf75 2026-02-11)` with the workspace's own flags, run from `src/tools`. The clock
is `std::time::Instant`. Five warmup frames are discarded and thirty are measured; the first replay
of a list touches every page of the reservation and would otherwise be divided into every sample.
Preparation is reported separately from replay because they are different claims: `prepare` flattens,
strokes, bins, builds pyramids and reserves ONCE, and `render` is what a repeated frame costs.

**The scenes are frozen and the runner checks that they are.** Each one's command count and resource
count are asserted against the numbers recorded in the tool before the clock starts, so a later
simplification cannot quietly lower the workload and report the same milliseconds against an easier
scene.

| scene | commands | resources | prepare | replay median | replay p99 | ceiling | verdict |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| UI-basic | 252 | 153 | 1.0 ms | 16.4 - 17.0 ms | 16.6 ms | 16.7 ms | **on the line** |
| UI-effects | 45 | 34 | 2.4 ms | 179 - 183 ms | 197 ms | 66.7 ms | 2.7x |
| vector-stress | 240 | 241 | 6.9 ms | 75.4 - 76.8 ms | 76.4 ms | 66.7 ms | 1.13x |
| image-stress | 25 | 3 | 38.9 ms | 344.7 ms | 348.0 ms | 16.7 ms | 20.7x |

**`UI-basic` SITS ON ITS CEILING, WHICH IS NOT THE SAME AS CLEARING IT, and a range is given rather
than a single median because a single median here is a coin toss.** Seven consecutive runs measured
16.36, 16.47, 16.70, 16.36, 16.66, 16.66 and 16.96 ms against a ceiling of 16.7: five met it and two
did not. It was 46.1 ms when this work started, which is the part worth having; "met" is not.

THE BUDGETS IN THE RUNNER STAY AT THE CEILINGS, deliberately. The item's rule is that a scene's frozen
budget is its first accepted measurement OR its ceiling, whichever is LOWER - so accepting this one
would freeze a number two runs in seven miss, and every later run would be testing the weather.

**AND THE SAMPLING WAS NOT CHANGED, which is worth writing down because the temptation is obvious.**
The runner discards five warmup frames and takes the median of thirty, and the run-to-run spread on
this host is about four percent - enough to move `UI-basic` across its ceiling either way. Reporting
the MINIMUM instead of the median would make it pass every time and would even be defensible in
general, since interference only ever adds time. It was not done, and must not be done while a scene
is sitting on its line: a measurement rule changed in the same breath as the verdict it decides is
not a measurement rule, it is a way of getting the answer somebody wanted. If the sampling is ever
reconsidered, it is reconsidered when nothing is balanced on it.

### What the frame is made of, measured rather than reasoned about

**THE FROZEN SCENES SAY WHETHER THE FLOOR IS MET AND NOT WHY IT IS NOT**, and three rounds of
optimisation were guided by guessing at the answer. `SOFT2D_BENCH_PROBE=1 ./bench.sh --suite soft2d`
runs scenes small enough to subtract from each other, at the same extent as the frozen four:

| probe | replay | what it isolates |
| --- | ---: | --- |
| empty | 0.001 ms | a tile no command reaches is not replayed at all |
| dot-per-tile | 0.081 ms | one pixel in every tile - see the damage note below; it was 17.0 ms |
| one-opaque-fullscreen | 10.4 ms | the store plus a full-screen composite, with the decode skipped |
| four-opaque-quarters | 11.6 ms | the SAME pixels as four commands, so the difference is a COMMAND |
| four-opaque-fullscreen | 14.9 ms | four times the PIXELS at four commands, so the difference is a pixel |
| half-of-every-tile | 9.6 ms | every tile touched and half of each dirtied |
| one-translucent-fullscreen | 19.8 ms | the same with the decode paid, so the difference is the DECODE |
| hundred-small-opaque | 16.0 ms | a hundred commands over a fifth of the area |

THE MIDDLE THREE WERE ADDED ON 2026-09-19 and they are the ones that answered the question below: the
first row varies the commands at a fixed pixel count and the second varies the pixels at a fixed
command count, which is what separates two terms that had only ever been measured multiplied together.

**AND THE FIVE IMAGE PROBES BESIDE THEM ARE WHAT PUTS A NUMBER UNDER THE TWO IMAGE SCENES'
CEILINGS (re-measured 2026-09-20).** One full-screen image draw, one probe each:

| probe | replay |
| --- | ---: |
| image-photo-bilinear | 47.7 ms |
| image-photo-mipmapped | 72.5 ms |
| image-photo-bicubic | 134.4 ms |
| image-widegamut-bilinear | 49.5 ms |
| image-yuv-bilinear | 54.7 ms |

**THE PER-PIXEL FLOOR IS 33 NANOSECONDS AND THE CEILING IS 54, AND THAT SETTLES A QUESTION THIS FILE
HAS BEEN CARRYING.** `one-opaque-fullscreen` is 10.125 ms over 307,200 pixels - one command, a solid
opaque colour, the backdrop decode already skipped - which is 33 ns for a pixel this backend does
almost nothing to. A 16.7 ms ceiling over the same frame is 54 ns per pixel FOR EVERYTHING.

SO `image-convert` CANNOT REACH ITS CEILING AT THIS FLOOR, whatever its sampler costs. It draws the
frame over twice: a full-screen mipmapped wide-gamut draw at 72.5 ms and a full-screen YUV bilinear
one at 54.7 ms, each measured alone and each already including that frame's single output encode. Two
of those is 127 ms of work against a 16.7 ms budget, and the twelve tile-sized draws are on top. The
same arithmetic puts `image-resample` out of reach: its eight downscales and three upscales cover
about 1.7 frames.

**AND THE FLOOR IS THE OUTPUT ENCODE.** What a covered pixel pays on the way out of the tile, in
`Encoder::encode_row` and `write_row`: an unpremultiply, THREE `TransferTable::encode` calls - each a
square root, two table reads and a lerp - a dither add, a re-premultiply and four quantisations. The
three square roots alone are most of the 33 ns.

WHY THAT IS NOT A TUNING PASS. A fused linear-float-to-encoded-byte table would remove the square
root, the lerp and the quantisation together, and it would CHANGE THE PIXELS: the conformance suite
compares output exactly, and the dither path adds its offset in encoded space between the encode and
the quantisation. So it is a change to what this backend produces, decided against the conformance
registry, and not something to slip into a performance pass. It is named here because it is the
lever, and because the two image scenes' ceilings are a question for the project owner that now has
a measurement under it rather than an estimate.

**A DAMAGE-LIMITED REDRAW NOW COSTS WHAT IT DRAWS.** A tile was decoded and re-encoded WHOLE, so a
drawing that touched three pixels of it paid sixty-four rows of conversion for them; the round trip is
now over the union of the bounds of the commands binned to that tile, which `prepare` already knows.
The `dot-per-tile` probe - one pixel in each of eighty tiles - went from 17.0 ms to 0.081 ms, and the
hundred-small scenes by about a sixth. IT DOES NOT MOVE THE FOUR FROZEN SCENES AT ALL, because every
one of them covers the frame; it is here because a compositor updating one corner is the case the
tiling exists for, and it was paying the whole frame's conversion to do it.

A tile whose bin holds a LAYER, a layer end or a clip mask keeps its whole round trip: a layer
composites over bounds that are a command FIELD rather than a binned bound, and a filter reaches past
what it reads. Conservative, and it costs nothing on the drawings this is for.

Read together: the decode is about 8 ms of a full-frame redraw, the encode about 9, and compositing
307,200 pixels about 6. A hundred small commands cost MORE than one that covers the whole screen,
which is the shape a user interface has and the reason the per-command path matters more than the
per-pixel one.

**AND THE TWO SCENES THAT ARE STILL OVER, TAKEN APART THE SAME WAY.** A frozen scene mixes a
rasteriser, a shader and a sampler, and a verdict over the mixture says nothing about which to work
on. Each of these is one full-screen draw of one kind, or the vector scene's own geometry with its
paint swapped:

| probe | replay | what it says |
| --- | ---: | --- |
| strokes-solid | 58.2 ms | the vector scene's rasteriser and composite, with a free paint |
| strokes-gradient | 74.2 ms | the same geometry with its linear gradient, so the shader is 16 ms |
| image-photo-bilinear | 82 ms | four texels per pixel |
| image-photo-bicubic | 263 ms | sixteen, so a texel fetch is about 15 ms of a full-screen draw |
| image-widegamut-bilinear | 90 ms | the same with a colour-space matrix |
| image-yuv-bilinear | 172 ms | planes reconstructed and matrixed per texel; no prepared fetch |

`vector-stress` CANNOT REACH ITS CEILING BY SHADER WORK: its geometry alone is 58.2 ms against 66.7,
so even a free paint leaves 13% of headroom for everything else, and the remaining gradient cost is
two divisions per pixel that cannot be turned into multiplications without changing the pixels. What
is left is the exact-area accumulation itself over NEAR-HORIZONTAL edges - a stroke of a flat curve
has outline edges that cross many columns of every row they touch, which is where that rasteriser is
most expensive and is the algorithm rather than its constants.

**THE LARGEST SINGLE FIND WAS A SOFTWARE SQUARE ROOT** (2026-09-15). `sqrt_f32` was four Newton
iterations from a bit-level estimate - four serially dependent f32 DIVISIONS, a dozen cycles each and
impossible to pipeline behind one another - and the encode table is indexed by the square root of its
input, so it ran three times for EVERY PIXEL of every frame. Replacing it with `libm::sqrtf`, which
lowers to the hardware instruction and is correctly rounded rather than approximate, took a
full-screen opaque fill from 26.9 ms to 16.2 ms on its own. There were TWO copies of it: `render2d`
had a private one whose documentation said "two Newton steps" while the loop ran four, on the
flattening path, which is why `vector-stress`'s preparation fell from 7.7 ms to 6.7 ms. Both are now
`graphics_core::composite::sqrt_f32` and there is one square root in the stack.

That row replaced this one on 2026-09-15, through seven changes measured one at a time on the same
host with the same fixtures:

| scene | before | after | | ceiling |
| --- | ---: | ---: | ---: | ---: |
| UI-basic | 46.1 ms | 16.8 ms | 2.7x | 16.7 ms |
| UI-effects | 336.1 ms | 202.8 ms | 1.7x | 66.7 ms |
| vector-stress | 99.0 ms | 75.8 ms | 1.3x | 66.7 ms |
| image-stress | 380.6 ms | 353.3 ms | 1.1x | 16.7 ms |

The four that removed WORK, then the three that made the remaining work cheaper:

| scene | before | skipped decode | rectangle clips | f32 space + solid span | narrowed rows | hardware sqrt | one bounds check per run | one sqrt in the stack |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| UI-basic | 46.1 | 35.6 | 32.2 | 30.2 | 29.2 | 19.0 | 19.3 | 17.7 |
| UI-effects | 336.1 | 322.7 | 325.1 | 225.3 | 232.6 | 209.8 | 211.1 | 211.4 |
| vector-stress | 99.0 | 97.2 | 97.5 | 97.2 | 89.2 | 79.7 | 76.6 | 76.6 |
| image-stress | 380.6 | 383.2 | 376.7 | 375.9 | 375.5 | 356.9 | 358.4 | 362.8 |

- **A tile nothing reads the backdrop of is not decoded.** Every tile was read out of the target into
  the working space before it was replayed and written back afterwards; a tile that some command
  covers COMPLETELY and OPAQUELY never needed the first half, because the only reason to read a
  backdrop is to blend with it. The conditions are narrow on purpose - a solid fully opaque paint at
  full opacity, `Normal` over or copied, over an axis-aligned rectangle, with no clip pushed and no
  layer open - and each one of them is a way a pixel could otherwise depend on what was beneath it.
  `UI-basic`'s full-screen panel qualifies and its eighty tiles each save a decode; the demo's conic
  gradient does not, which is why the live figure above moves less.
- **A rectangular clip needs no mask.** The clip stack has always had a rectangle level that costs no
  storage and no per-pixel multiply, and nothing ever pushed one: every clip rasterised its edges
  into a full-tile mask, zeroed first. `UI-basic` clips fifty times, in every one of eighty tiles.
  The fast path is taken only when the rectangle is PIXEL-ALIGNED, because a rectangle level answers
  one or zero and an edge between two pixel centres has an answer in between.
- **The working intermediate is four singles and was four halves.** This machine has no hardware half
  conversion in reach of a `no_std` build, so every read and write of a working pixel went through a
  branchy software routine twice per channel. Doubling the scratch - hundreds of kilobytes against a
  sixty-four megabyte ceiling - removes eight conversions per pixel per access, and it is MORE
  accurate rather than less: the arithmetic above it was `f32` throughout and the half was rounding
  between every pair of composites. `UI-effects` is where it shows, because a filter graph reads and
  writes intermediates for every node. Measured with the solid-span fill below, which was too small
  to separate: asking a solid paint for its colour per pixel is an enum match inside the hot loop.
- **A row is only as wide as the shape reached.** The area rasteriser zeroed two accumulators across
  the whole width of its bounds, summed them across the whole width, evaluated the winding rule at
  every column, and handed the caller a full-width row to scan for the covered part. A row of a thin
  stroke crossing a sixty-four-wide tile touches two or three columns. It now tracks the columns its
  edges reached, keeps the accumulators clean by zeroing only those, and emits the covered run with
  the index it starts at - the columns outside it have ONE answer each, which is a fill where it is
  not zero and nothing at all where it is. `vector-stress` is where it shows, which is the scene
  made of thin outlines.

- **One bounds check per RUN and not one per channel.** The row converters read each channel with
  `get(index).copied().unwrap_or(0)` - four branches and a panic path per pixel - and the compiler
  cannot remove them, because a chunk whose size is a runtime value could be shorter than index
  three. The two four-byte orders every target in this tree presents are spelt out against
  `chunks_exact(4)`, whose size the compiler knows; every other format still goes through the general
  path unchanged. The tile round trip fell from 21.0 ms to 17.0 ms.

- **A sampler prepares its own texel fetch.** `read_row`'s own documentation says "the row lookup is
  the cost, not the pixel... doing that per pixel is most of the time a conversion spends" - and a
  SAMPLER did exactly that per TEXEL, which a bilinear tap does four times and a bicubic sixteen
  times for every pixel of an image draw. Each fetch re-derived the row's start from the origin and
  the pitch, asked the storage enum for the minimum row bytes, took two bounds-checked slices and
  matched the format again. A sampler is built once per image per frame, so all of it is the same
  answer every time: a full-screen bilinear image draw went from 97 ms to 82 ms and a bicubic one
  from 315 ms to 263 ms.
- **A fully covered opaque run is a copy.** `Cs + Cb * (1 - as)` with `as = 1` is `Cs + Cb * 0`,
  which for any finite backdrop is exactly `Cs` - so the backdrop read, the four multiplies of the
  coverage scale and the eight of the blend all compute a number already in hand. That is the
  INTERIOR of every filled shape; the edge, where coverage is partial, is unchanged. The conditions
  are the ones that make it an identity and no wider, and the scan that checks the run's weights are
  all one costs a pass and saves three.
- **The dither row, and the blur's edge, asked once instead of per pixel.** `dither_offset` takes two
  modulos and indexes a matrix, and `y` is constant for a row - so the row's eight offsets are taken
  once. A blur tap asked whether it had fallen off the source, `2r + 1` times per pixel, and a pixel
  at least `r` from either end cannot have: the interior runs without the test, in the same order,
  which is what makes it the same number rather than a close one.
- **The blur's second pass reads its column as a RUN.** It walked columns with `get(x, y)` per pixel,
  recomputing local coordinates, a bounds check and an offset for each - twice, once each way - while
  the first pass had used spans for its rows all along.

**Where it still is not enough, and what it would take.** `UI-basic` is at its ceiling within one
percent and `vector-stress` is 14% over; both are now dominated by the TILE ROUND TRIP - a decode and
an encode of every pixel touched, about 17 ms of a 16.8 ms frame's worth of work, against 6 ms of
actual compositing. That is a per-pixel transfer conversion with a table lookup in it, which is the
thing that does not vectorise, and closing it means changing what the intermediate IS rather than
finding another constant factor.

`UI-effects` at 3.0x is a direct Gaussian: `2r + 1` taps of a four-channel multiply-add per pixel per
pass, which for the sigma this scene uses is sixty. Every constant factor around it has now been
taken out and the arithmetic is what remains; going faster means a box-blur approximation, and the
profile specifies a Gaussian. `image-stress` at 21x is the fixture question the item itself raises.

The row above replaced this one on 2026-09-14, when the rasteriser stopped sampling the vertical
direction and started accumulating area. Same host, same fixtures, same flags:

| scene | replay median before | after | |
| --- | ---: | ---: | ---: |
| UI-basic | 78.8 ms | 46.4 ms | 1.7x |
| UI-effects | 385.1 ms | 332.0 ms | 1.2x |
| vector-stress | 245.8 ms | 97.0 ms | 2.5x |
| image-stress | 419.0 ms | 377.7 ms | 1.1x |

THE CHANGE WAS MADE FOR CORRECTNESS AND PAID FOR ITSELF IN SPEED, which is the shape the analysis
below predicted: sixteen sub-scanline sweeps, each with a sort of its crossings, became one pass in
which every edge is clipped to the pixels it crosses and two numbers are accumulated. `vector-stress`
is where it shows most, because a stroke's six hundred edges were being crossed sixteen times a row.

**THE FLOOR IS NOT MET.** The ceilings are fixed independently of the implementation and stay where
they are; these are the measurements as they stand, and the gap is between one and a half and
twenty-three times. The
image scene grew its YUV source when the multi-plane model landed, which is a full-screen `NV12`
frame reconstructed, matrixed, transfer-decoded and converted from Rec. 2020 per pixel - it is the
workload the scene is for, and it moved that row from 267 ms to 419 ms.
The numbers above are already between three and six times better than the first working version
(UI-basic 289 ms, UI-effects 2401 ms, vector-stress 1198 ms, image-stress 786 ms), through changes
that were worth making on their own:

- The transfer functions became TABLES built once per frame rather than a `powf` per channel per
  pixel. A 640x480 frame decodes and re-encodes nearly two million channels; the tables agree with
  the exact functions within the profile's own round-trip tolerance, which a fixture holds them to.
- Shaders are built ONCE PER FRAME instead of once per tile. A solid paint's colour conversion, a
  gradient's ramp and an image's sampler were each being rebuilt for every tile the command touched -
  eighty times over, for two hundred commands.
- The rasteriser keeps an ACTIVE EDGE LIST. It was testing every edge of a shape against every
  sub-scanline, which for a stroke of six hundred edges over a thousand sub-scanlines is the product
  of the two. This alone took vector-stress from 1025 ms to 275 ms.
- The target's working copy of a tile is `f32` rather than the canonical half-float format. Layers
  and filter intermediates stay `R16G16B16A16_FLOAT`, which the profile fixes; the tile copy is this
  backend's own scratch and never leaves it, and holding it as halves cost eight conversions per
  pixel per composite.
- A surface addresses its own bytes instead of building a checked `ImageView` per pixel access.

- A TILE NO COMMAND REACHES IS NOT REPLAYED (2026-09-14). An empty bin still decoded the tile into
  the working space and re-encoded it, which is what made an EMPTY draw list cost 21 ms; the four
  scenes above cover the whole frame so their medians do not move, and a compositor redrawing one
  damaged corner stops paying that for every other tile of the frame.
- The rasteriser accumulates AREA instead of sampling the vertical direction (2026-09-14). It used to
  cut each pixel row into sixteen sub-scanlines, compute and SORT the crossings on each, and add the
  inside intervals at a sixteenth of a level; it now clips every edge to the pixels it crosses and
  accumulates two numbers per pixel, which a left-to-right sweep turns into coverage. One pass, no
  sort, and exact in both directions rather than quantised in one - the change was made because the
  sampling did not meet the frozen coverage threshold, and the speed is what fell out of it.

What the remaining gap is made of, measured on the same host with a 640x480 target: an EMPTY list
cost 21 ms, which was the tile decode and re-encode of the whole frame and nothing else; a list with
no commands now touches no tile at all, and what remains of that term is the load and store of the
tiles a drawing DOES reach, which is still the largest single term in `UI-basic`. Of the three things the earlier analysis named as needed,
the exact-area rasteriser is DONE; what is left is a vector span composite over more than four scalar
lanes, and a specialised path for an opaque solid fill that skips reading the backdrop entirely.
Neither is a change to what is drawn, and both are ordinary work rather than a redesign.

## Development loop baseline (2026-07-26)

`./dev.sh baseline <cold|warm|leaf|provider> [test-tags]` records one
x86_64 sample under `.build/dev-baseline/<timestamp>-<scenario>/` and appends its
machine-readable row to `.build/dev-baseline/samples.tsv`. Each sample retains the
shared-image and kernel-test transcripts, raw host-nanosecond timing events and a
self-contained `summary.tsv`. Kernel compile/link time is the Cargo interval after
subtracting measured init and volume package assembly. The command
does not mutate source files: `leaf` and `provider` label a sample after a real edit;
`cold` forces all shared-image artifacts through compile, link and audit while retaining
the global Cargo cache; `warm` measures the unchanged path.

The host has 52 logical CPUs; routine QEMU tests use four vCPUs and KVM. The selected
`smoke` union ran seven tests. The leaf sample used a semantic-neutral whitespace change
to `uname.rs`; the provider sample used the same kind of change to `keys/src/lib.rs`.
Both probes were restored immediately after measurement and left no source diff.

| scenario | total | source | graph | provider link/audit/stage | consumer link/audit/stage | kernel compile/link | init package | volume package | image | QEMU start | guest boot | scenario | shutdown | output |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| cold shared image | 414.63 s | 1 s | 30 s | 265 s | 99 s | 1.23 s | 0.37 s | 3.04 s | 0.54 s | 3.92 s | 0.48 s | 24 ms | 71 ms | 136 lines / 16,641 B |
| warm no change | 10.21 s | 0 s | 0 s | 0 s | 0 s | 0.23 s | - | - | 0.48 s | 3.86 s | 0.48 s | 39 ms | 57 ms | 3 lines / 432 B |
| one leaf tool | 101.67 s | 1 s | 2 s | 39 s | 40 s | 1.25 s | 0.36 s | 3.01 s | 0.52 s | 3.95 s | 0.49 s | 40 ms | 58 ms | 5 lines / 648 B |
| one provider | 94.73 s | 1 s | 3 s | 25 s | 46 s | 1.28 s | 0.33 s | 2.96 s | 0.52 s | 4.07 s | 0.51 s | 40 ms | 31 ms | 24 lines / 3,081 B |

The provider sample rebuilt `keys.lslib` and 19 dependent executables. The leaf sample
reported 61 provider cache hits and only one object/executable miss, yet still spent 39
seconds in the provider phase. The current cache therefore avoids output replacement but
does not avoid all expensive Cargo/provider graph work before proving those hits. Package
assembly adds roughly 3.4 seconds whenever the kernel build script reruns. Even on the
true warm path, boot-image assembly plus fresh QEMU startup and guest boot cost about 4.8
seconds for a scenario whose guest execution is only 39 ms. These measurements establish
the targets for proportional single-artifact builds and the persistent guest loop.

## Development loop phase review (2026-07-29)

`LIBER_TIMING_LOG=<file> src/tools/dev-build.sh <artifact> <target>` appends host-nanosecond
phase events in the same three-column format the kernel test driver and the QEMU runner already
emit: `build`, `source`, `graph`, `providers` and `consumers` boundaries, plus one per-unit
event `<kind>:<hit|miss>:<name>` for every provider, object and executable the build decided
about. A unit event marks the decision, not the work, so a miss appears before the compile it
causes; the surrounding boundaries are what time the work.

`cd src && ./lab.sh perf-gate` measures the two loop budgets against a running development instance
and asserts the shape of the work beside its cost, so a loop cannot look fast by skipping the
test or by publishing something other than what it built.

Sample rows recorded by `./dev.sh baseline` now carry a `schema` column. Rows from the
2026-07-26 baseline predate it and measured a different set of phases, so the recorder refuses
to append to that file rather than let the two be read as one series.

The warm leaf build, segment by segment. Two samples on the documented 52-CPU host, taken with
the caches warm and a one-line comment appended to `uname.rs`, so exactly one object and one
executable were rebuilt and the single provider in the closure was reused.

| segment | ms | what it is |
| --- | ---: | --- |
| script start to source stage | 181-185 | interpreter start, manifest query, artifact kind and closure resolution |
| source inventory | 201-210 | hashing the selected closure's sources |
| source stage to graph stage | 265-273 | targeted plan and per-artifact state resolution |
| Cargo image graph | 240-244 | resolving rlib paths for the closure |
| providers | 384-662 | proving the one provider in the closure unchanged |
| consumer plan | 347-370 | cache keys, identity record and provider index for the consumer |
| consumer compile, link and audit | 989-1003 | the only segment that produces the artifact |
| last stage to script end | 502-517 | writing per-artifact state, the summary and cleanup |
| total | 3158-3448 | |

About one second of a 3.2 to 3.4 second leaf build compiles, links and audits. The other two
thirds prove that nothing else needed to. That is the cost of proportionality rather than waste,
since each segment answers a question the build must answer before it can skip work safely, but
it is now the dominant term, which it was not when the same iteration cost 101.67 seconds.

Loop budgets, measured against a persistent instance:

| path | measured | budget | verdict |
| --- | ---: | ---: | --- |
| warm no-change build | 0.37-0.40 s | 1.00 s | met, and rebuilt nothing |
| warm leaf, build phase | 3.20 s | 3.60 s | met |
| warm leaf, publish phase | 0.40 s | 0.60 s | met |
| warm leaf, scenario phase | 2.30 s | 2.60 s | met |
| warm leaf, total | 5.6-6.0 s | 6.00 s | met, with no margin |

The total was five seconds until 2026-07-30 and was restated against the measurement in that
same row, not to turn a red gate green: the loop runs at 5.6 to 6.0 s, the one implemented
optimization recovered a tenth of a second where most of the gap was expected, and the
proposals that would close the rest buy time with correctness. Read the last row as it is
written - the worst sample equals the budget, so the total has no headroom and any regression
is a red gate. The per-phase budgets were tightened at the same time, from build 4.0 / publish
1.0 / run 2.5, which were loose enough that publication could grow to two and a half times its
cost and still report `ok`. Each is now a margin for noise over the worst of three consecutive
samples. They sum to more than the total on purpose: the total asks whether the loop is fast
enough to work in, a phase budget asks which part changed.

The first leaf iteration after `dev-up` costs 8.4 s, with the build phase at 5.90 s against the
3.20 s every later sample shows, so it is excluded from these rows. A gate run straight after
bringing an instance up fails for a reason that is not a regression.

Against the 2026-07-26 baseline the same one-line leaf edit fell from 101.67 s to about 5.8 s,
and the unchanged path from 10.21 s to 0.40 s. Most of that is work no longer done at all rather
than work done faster: with a persistent guest, QEMU startup (3.92 s), guest boot (0.48 s),
volume package assembly (3.04 s) and boot-image assembly (0.54 s) are not paid per iteration.

Which scenario the loop runs is a large share of what remains. The same iteration measures 2.10 s
of scenario with `shell-basics`, 3.20 s with `registry-shadow` and 4.90 s with `launch-program`,
which launches three programs. Reporting a loop time without naming its scenario is therefore
not a measurement.

### Where a full rebuild's time goes

A forced whole-image rebuild of x86_64 measures 406 s: source 1 s, Cargo graph 31 s, providers
209 s, consumers 107 s. During it the host runs at 5 to 7 percent of its 52 cores.

Those two facts fit together rather than contradicting each other. Sampled every 200 ms through a
rebuild, `rustc` is running in 229 of 300 samples, so the time is spent compiling; but one
artifact is compiled at a time, and a small crate's compile is single-threaded - 0.36 s of wall
time against 0.39 s of CPU, a ratio of 1.07. One core of fifty-two is the utilisation observed.

This is the opposite balance to a warm leaf iteration, where compilation is 15 percent of the
time and proving things is the rest. The two numbers describe different work and are easy to
mistake for each other.

Concurrency does not come from running the existing per-artifact `cargo rustc` invocations at
once. They share one `CARGO_TARGET_DIR`, and cargo locks it:

| tools compiled | in sequence | concurrently | speedup |
| --- | ---: | ---: | ---: |
| 4 | 1.78 s | 1.13 s | 1.57x |
| 8 | 3.42 s | 2.55 s | 1.34x |

The gain shrinks as more are added, which is what lock contention looks like. One cargo
invocation building the same set instead measured 74 s of CPU in 25 s of wall time, since cargo
schedules its own unit graph. The dependency graph does not stand in the way either: derived
from the manifest, the libraries are six levels deep and the 74 dynamic
volume programs depend on no other program at all. The current structural counts are in
`docs/DYNAMIC_EXECUTABLES.tsv` (74 tools x 3 targets), `docs/DYNAMIC_WAVES.tsv` (6 waves x 3
targets) and `docs/DYNAMIC_IMAGE.tsv` (3 targets); the numbers that used to be written out here
went stale the first time a tool was added, which is why they now name the file instead.

### The cold invalidation classes, measured

`./dev.sh baseline <kernel|loader|topology>` labels a sample after the operator has edited the
class it names, the way `leaf` and `provider` already did. Each probe below was a comment
appended to one file, measured, then restored, with a settling `./build.sh` between samples: a
restore is itself a source change, and without settling the next sample pays for the previous
one's, which is how the first attempt at this produced a `loader` row that recompiled the kernel.

| sample | total | kernel test binary | packages | image | QEMU start | guest boot |
| --- | ---: | ---: | ---: | --- | ---: | ---: |
| warm, nothing edited | 11.86 s | 1.32 s | 3.84 s | cache hit | 4.94 s | 0.52 s |
| `topology` (`qemu-run.sh`) | 11.70 s | 1.27 s | 3.70 s | cache hit | 4.99 s | 0.50 s |
| `kernel` (`kernel/main.rs`) | 12.25 s | 1.30 s | 3.98 s | rebuilt | 4.89 s | 0.52 s |

The three rows are within half a second of each other and of editing nothing at all, and the only
thing that actually varied is whether the boot image was reassembled. That is the finding rather
than a measurement problem: through the checkpoint test path the cost is a floor rather than a
function of what changed. The kernel test binary is rebuilt every time, because `cargo test`
builds a different unit from `cargo build`; the kernel build script reruns and recomputes both
package images every time, which costs 3.7 to 4.0 s even when it then publishes nothing because
the bytes are unchanged; and a fresh QEMU and guest boot are paid unconditionally, at about 5.4 s
together.

The invalidation classes are real and the instance detects them correctly. What these numbers say
is that their cost through the cold path is structural, not proportional, which is the argument
for the persistent instance rather than against the classes.

A `loader` sample is not recorded, and the reason is worth more than the number would have been:
the kernel test path never rebuilds the loader. `harness/test-kernel.sh` compiles the kernel and
runs it; `mkimage.sh` consumes an already-built `libersystem-loader.efi`; only `./build.sh`
compiles one. So a loader edit is invisible to `./test.sh`, which boots whatever loader was last
built, and a loader-only sample has to be taken through a path that assembles the image from a
fresh loader instead.

## Cold invalidation classes (2026-07-31)

The three sample kinds the phase review left unrecorded. Each is a cold invalidation - a change
no fast iteration can carry - so what they measure is the checkpoint path rather than the loop,
and none of them is held to the loop's budgets. What they are for is the shape of the cost: what
a developer pays when a change reaches past the artifact it touched.

Schema `liber-dev-baseline-v2`, the same the phase review rows carry, so these are comparable
with those and not with the 2026-07-26 baseline, which measured a different set of phases.

| class | total | build | init package | volume package | image | QEMU start | guest boot | scenario |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| kernel-only | 15.75 s | 2.98 s | 0.68 s | 4.45 s | 0.96 s | 5.15 s | 0.51 s | 25 ms |
| loader-only | 19.38 s | 7.26 s | 0.40 s | 3.83 s | 0.66 s | 5.89 s | - | 42 ms |
| boot-topology | 7.56 s | 0.24 s | - | - | 0.86 s | 5.09 s | 0.52 s | 24 ms |

Read against the leaf iteration's 5.6 to 6.0 s, the cheapest of the three costs more than the
most expensive fast path, which is the whole argument for the invalidation matrix: these are the
changes worth knowing are cold before making them, not after.

The floor is the boot-topology row: nothing is rebuilt, and it still costs 7.56 s, of which
5.09 s is QEMU starting. Every cold class pays that, so no amount of build work removes it - a
change that needs a fresh guest costs five seconds before anything of its own is measured.

The volume package dominates the two that rebuild: 4.45 s of the kernel row and 3.83 s of the
loader row, against 0.96 s and 0.66 s to assemble the image from it. A kernel change pays it
because the kernel's build script regenerates the packages; a loader change pays it for the
same reason, through the same path.

The loader row had to be taken by hand, and the reason is worth keeping: a loader edit is
invisible to `./test.sh`, which boots whatever loader was built last, so the recorder's path
measures everything except the thing being changed. Taken through `./build.sh`, which does
compile it, the loader costs 7.26 s to rebuild - the largest single build cost of the three.
The first attempt at this sample also read 2.19 s rather than 7.26 s, because the probe comment
was the same text as the previous attempt's and cargo served the earlier compilation: a probe
that repeats itself measures a cache hit. The recorded row uses a probe carrying the clock, as
the performance gate does for the same reason.

## Image conversion (2026-07-16)

`./bench.sh --suite image` builds the same no_std leaves used by `imgconv` in an optimized
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
| GIF quality 0, 16 colors | 71,170 | 1,450 | 49.8 ms | 7.2 ms |
| GIF quality 100, up to 256 colors | 149,950 | 240 | 153.3 ms | 6.9 ms |
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

GIF also reserves an exact Global Color Table entry for the logical-screen
background. Its alpha follows the first-frame transparent-index convention measured
with ImageMagick; subsequent disposal restores that RGBA value. This may consume one
palette slot, but prevents conversion from silently changing partial-frame visuals.

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

`./bench.sh --suite audio` is the standing optimized-host MP3 throughput gate. The host-only
benchmark uses the same atomized decoder leaf as `play`, reparses the staged
`volume/audio/test.mp3` on every iteration, drains signed-i16 output in bounded
1,024-frame chunks and decodes at least 60 seconds of logical audio. It fails below
real time. One x86 host run produced:

| codec/container | staged rate | fixture frames | iterations | wall | realtime |
| --- | ---: | ---: | ---: | ---: | ---: |
| WAV PCM | 44,100 Hz | 328,104 | 9 | 0.008 s | 8,843.7x |
| WAV IMA ADPCM | 44,100 Hz | 328,104 | 9 | 0.018 s | 3,805.8x |
| WAV MS ADPCM | 44,100 Hz | 328,104 | 9 | 0.013 s | 5,251.3x |
| AIFF PCM | 44,100 Hz | 328,104 | 9 | 0.009 s | 7,555.9x |
| AIFC PCM | 44,100 Hz | 328,104 | 9 | 0.008 s | 8,403.2x |
| FLAC | 44,100 Hz | 328,104 | 9 | 0.163 s | 411.5x |
| MP3 | 44,100 Hz | 328,104 | 9 | 0.065 s | 1,022.5x |
| Ogg Vorbis | 44,100 Hz | 328,104 | 9 | 0.136 s | 492.7x |
| WavPack mono | 44,100 Hz | 328,104 | 9 | 0.159 s | 420.4x |
| WavPack stereo | 44,100 Hz | 328,104 | 9 | 0.271 s | 246.9x |

MP3 playback parses the first-frame Xing/Info tag and applies its 12-bit encoder
delay/end-padding fields together with the decoder synthesis delay. The informational
frame itself is not rendered. The staged stream therefore exposes the same 328,104
frames as the independent FFmpeg PCM golden instead of 330,624 raw decoder frames;
full-stream mean sample error is at most two. `play` is launched as the tty foreground
job, so ConsoleService delivers Ctrl+C to its Process handle and the caught interrupt
closes the PCM stream cleanly. The built-in Rust `x86_64-unknown-none` target uses a
soft-float ABI and disables SSE, making `nanomp3` execute its float-heavy synthesis
through compiler-builtins helpers; guest profiling measured 4,236 ms of decode work.
The LiberSystem x86 userspace target instead selects the normal SSE2 hard-float ABI,
after the kernel enables x87/SSE on every core and eagerly preserves each thread's
FXSAVE state. The same decode work then measures 44 ms. `play` therefore keeps the
bounded 1,024-frame streaming path with no whole-file predecode or startup pause; only
the `mp3` and `nanomp3` packages use dev opt-level 3. Live playback completes in 7.375 s
for the 7.44-second fixture. The governed test requires twelve consecutive nonempty MP3
hardware periods before interrupt cleanup. A QEMU WAV capture measures 7.445 s and
174.86 dB PSNR against playback of the derived WAV, with the same source-silence
intervals and no added underrun gaps.

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
`AUDIO_WAV=/tmp/libersystem-audio.wav ./lab.sh boot --fresh`, run
`./lab.sh sh play audio/test.mp3`, then `./lab.sh quit`; the captured WAV traverses the live
shell -> governed player -> StorageService -> MP3 -> AudioService -> virtio-sound
-> host-audio path and remains inspectable in CI or headless development.

## Application surface presentation (2026-07-14)

Measured by the tagged x86 KVM display test (`cd src && ./test.sh --tags display`).
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
through `./check.sh --gate host-tests`; surface lifecycle is exercised by the live console and
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
an on-disk package measurement; release binaries are not yet what `./build.sh --part user` stages.

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
many concurrent processes can still justify `lsrt.lslib` and package-specific protocol
provider sharing.

## System-image dynamic linking (2026-07-14)

The system image uses an eager ELF64 module loader and an image-internal shared build. The bare-metal
Rust targets support neither Cargo `dylib` nor `cdylib`, so the reproducible builder emits
full-graph PIC rlibs and links their object members with the pinned `rust-lld -shared`.
The original x86 KVM integration launched an assembly-only staged `dyn_probe` through the real
StorageService and ProcessService. ProcessService reads its `DT_NEEDED` DAG
(`pix.lslib`, `proto.lslib`, `lsrt.lslib`), the kernel eagerly applies RELA/PLT symbol
relocations, and the probe calls exports from both leaf providers before its first IPC.

Cold start is measured from sending the ProcessService `launch` request to receiving
`dynamic link ok` from userspace. The immediately repeated launch keeps the first
Process handle alive, so immutable provider pages are already in the physical-page
cache; ProcessService still reads and parses all provider files from StorageService.

A DATED SNAPSHOT, not a live table: these numbers were measured on 2026-07-14, on the host and tree
of that day, and nothing regenerates them. They are kept because the RATIO is the finding - the
repeated launch costs the same as the cold one - and that conclusion does not depend on the
absolute values. Do not compare them against a current run without re-measuring both.

| x86 KVM scenario (2026-07-14 snapshot) | latency |
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

### Dynamic executable waves (2026-07-23)

The completed dynamic command graph has a checked structural baseline in
`docs/DYNAMIC_EXECUTABLES.tsv` and its per-wave aggregate in
`docs/DYNAMIC_WAVES.tsv`. `pie_bytes` sums stripped executable files.
`unique_provider_bytes` counts each provider file once per wave. `private_bytes` is the
simultaneous per-process sum of page-rounded writable executable and provider ranges;
`shared_bytes` sums immutable executable ranges plus each wave provider once. The report
checker independently reconstructs every closure and reproduces these values on all
three targets.

| target | wave | tools | PIE bytes | unique provider bytes | private bytes | shared bytes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| x86_64 | 1 | 12 | 74,960 | 429,592 | 311,296 | 454,656 |
| x86_64 | 2 | 11 | 129,480 | 532,088 | 602,112 | 585,728 |
| x86_64 | 3 | 13 | 97,512 | 700,776 | 667,648 | 774,144 |
| x86_64 | 4 | 8 | 63,144 | 523,104 | 397,312 | 516,096 |
| x86_64 | 5 | 4 | 50,112 | 2,113,328 | 479,232 | 1,998,848 |
| AArch64 | 1 | 12 | 83,744 | 500,160 | 327,680 | 495,616 |
| AArch64 | 2 | 11 | 140,088 | 618,440 | 880,640 | 630,784 |
| AArch64 | 3 | 13 | 104,744 | 817,448 | 917,504 | 847,872 |
| AArch64 | 4 | 8 | 68,056 | 607,512 | 561,152 | 557,056 |
| AArch64 | 5 | 4 | 52,992 | 2,302,016 | 757,760 | 2,060,288 |
| RISC-V | 1 | 12 | 92,384 | 507,016 | 327,680 | 421,888 |
| RISC-V | 2 | 11 | 160,256 | 617,552 | 802,816 | 536,576 |
| RISC-V | 3 | 13 | 112,560 | 809,080 | 888,832 | 712,704 |
| RISC-V | 4 | 8 | 77,656 | 612,128 | 540,672 | 462,848 |
| RISC-V | 5 | 4 | 59,384 | 2,210,784 | 712,704 | 1,613,824 |

The dynamic runtime gate launches one representative from each wave twice through
StorageService and ProcessService. It requires both timings to be nonzero, identical
first/repeated page counts, exact target-specific counts derived from the checked ELF
reports, clean exit after bootstrap closure, and one physically shared `lsrt` text frame.
Timing is observational rather than a threshold: host load and KVM/TCG make a strict
first-versus-warm ordering flaky. One x86 KVM debug run measured:

| wave representative | first launch | repeated launch | private pages | shared pages |
| --- | ---: | ---: | ---: | ---: |
| `echo` | 185.869 ms | 186.510 ms | 14 | 80 |
| `cat` | 245.242 ms | 249.176 ms | 22 | 107 |
| `date` | 243.670 ms | 245.442 ms | 21 | 93 |
| `ip` | 244.464 ms | 246.684 ms | 21 | 106 |
| `imgconv` | 337.806 ms | 336.565 ms | 46 | 370 |

Sharing is also verified between different executables. Concurrent `cat` and `write`
processes map the same physical first text page of `volume-client.lslib`; concurrent
`imgconv` and `imgview` processes map the same physical first text page of `jpeg.lslib`.
The checked canonical orders place those providers at slots 4 and 5 respectively on all
three targets, so the test addresses are deterministic. The comparison happens after
clean process exit while both Process handles retain their address spaces, avoiding any
interference with a live instruction stream.

The whole command image now has a checked aggregate that includes the exact current
ET_REL objects, not an ambiguous historical object-cache glob. Staged bytes count 48 PIE
files plus every provider needed by any command exactly once; archive framing, identity
records and non-tool volume entries are intentionally outside this graph payload metric.

| target | current ET_REL objects | PIE bytes | unique provider bytes | staged graph bytes | private bytes | shared bytes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| x86_64 | 444,816 | 415,208 | 2,445,600 | 2,860,808 | 2,457,600 | 2,826,240 |
| AArch64 | 496,496 | 449,624 | 2,689,536 | 3,139,160 | 3,444,736 | 2,932,736 |
| RISC-V | 525,080 | 502,240 | 2,579,464 | 3,081,704 | 3,272,704 | 2,387,968 |

`docs/DYNAMIC_IMAGE.tsv` is the machine-checked source for this table. Its acceptance
gate requires 48 current objects on every target, valid ET_REL identity/hash records,
positive footprint fields, exact staged-byte arithmetic and byte-for-byte regeneration.

### Image-build cache (2026-07-19)

The manifest-driven image builder separates four validated layers: Cargo's coherent
target graph, content-addressed consumer ET_REL objects, linked ELF/identity/order
artifacts, and keyed audit results. ET_REL keys include the dedicated compile helper,
toolchain/target/build-std/features, exact bin/package sources and recursive provider API
identities. Linked keys additionally bind the whole linker/audit builder, start object
and binary provider identities. Audit records compare the build key, ELF and identity
content hashes, and exact `DT_NEEDED`; a mismatch runs the full ELF/note/W^X audit.

Measured on the 52-core development host after the graph was warm:

| x86 image-build scenario | wall time | invalidation |
| --- | ---: | --- |
| cold object/link population | 405-408 s | 46 providers, 67 consumers |
| previous no-change cache | 78-80 s | 46/46 provider and 67/67 executable hits |
| split seed/object/link/audit cache | 49-58 s | same complete hit set |
| source inventory and target locking | 34 s | same complete hit set |
| memoized audit and order validation | 18-19 s | same complete hit set |
| one tool source edit | 75 s | `echo` only: one object + one executable miss |
| one provider implementation edit | 75 s | `volume-client` rebuild; six consumer relinks, six object hits |

Warm AArch64 and RISC-V graphs retain 46/46 provider and 67/67 executable hits at
73-74 s. `./check.sh --cache-check quick` pins no-change, one-tool and restored-variant
reuse; `./check.sh --cache-check provider` derives the direct `volume-client` consumers
from the manifest and requires every relink to reuse its ET_REL object. Consumer Cargo
jobs remain sequential for cold misses. Parallel Cargo writers against one target
directory are deferred until a cold-build measurement justifies their locking and memory
cost; ordinary edits no longer expose enough independent compilation to benefit.

Provider metadata is parsed lazily for relinks. One combined
`llvm-readelf -d --dyn-syms` invocation per needed direct provider populates dependency
and symbol-owner maps; canonical ordering and every consumer ownership check reuse those
maps. A warm artifact hit never builds the symbol index. This reduces a broad relink of
all 67 consumers with 67 ET_REL hits from 372 s to 203 s. A narrow `volume-client`
implementation change remains 52-53 s for one provider rebuild and six relinks with six
ET_REL hits, and no-change x86 remains 49-70 s because neither path previously spent most
of its time in repeated ownership lookup. The remaining cost is source-tree hashing,
provider/link work and shell process overhead, not consumer Rust compilation.

The source-inventory pass resolves the manifest's local Cargo roots once and reads
each relevant Rust/Cargo/toolchain/linker input once into a content-hashed in-memory
inventory. Crate, API, package-closure, executable and image-graph digests reuse those
bytes without mtime heuristics; their serialized digest format remains byte-compatible
with the prior cache keys. A per-target `flock` serializes the coherent Cargo target
directory and mutable artifact cache, while independent architectures remain parallel;
the invalidation harness holds that same x86 lock across mutation and restore. On the
final clean warm x86 graph the source stage is 0-1 s, graph validation 0-1 s, provider
validation 7 s and consumer validation 24 s, for 34 s total with 46/46 provider and
67/67 executable hits. Further source-digest optimization is therefore not justified;
the remaining measured work was provider/consumer artifact validation and shell process
overhead.

The artifact-validation pass now stores a structured atomic audit record instead of
recomputing an audit-key digest for every hit. Valid records compare in Bash without ELF
tool subprocesses. Canonical provider ordering uses in-process availability and
topological-membership maps; each executable's `.order` file has a content-hash sidecar.
A missing or mismatched order sidecar recomputes the canonical order and only reuses the
artifact when its saved order matches exactly. The cache harness deletes the `echo` order
sidecar and corrupts its order file, proving that the former is restored after validation
and the latter relinks only `echo` while reusing its ET_REL object. After the new sidecars
are populated, x86 warm graphs take 18-19 s: source and graph validation each take 0-1 s,
provider validation 5-6 s and consumer validation 10-11 s, still with 46/46 provider and
67/67 executable hits. The complete AArch64 and RISC-V graphs remain valid.

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
of a 5.2 MB file from the LiberFS system volume (`time cat /libexec/console_service.lsexe`,
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

## What one full-frame pass costs, measured (2026-09-16)

THE SINGLE MOST USEFUL NUMBER THIS SUITE PRODUCES is not a scene's median. It is the probe
`one-opaque-fullscreen`: one command, one solid opaque fill over the whole 640x480 frame, taken
through the covered-run fast path that skips the backdrop read and the composite entirely.

    one-opaque-fullscreen        14.55 ms      307,200 pixels, 47 ns each
    one-translucent-fullscreen   24.30 ms      the same fill with the read and composite it needs
    hundred-small-opaque         19.38 ms
    hundred-small-translucent    20.09 ms
    dot-per-tile                  0.08 ms      one pixel in each of eighty tiles

FOURTEEN AND A HALF MILLISECONDS IS 87 PERCENT OF THE 16.7 ms CEILING that a 60 Hz scene is held to.
So on this renderer that ceiling is a budget for ONE pass over the frame plus about two milliseconds,
and a scene named for its rectangles, glyphs, images and clips spends roughly an eighth of its time
on them. Anything aiming at that ceiling has to make the per-pixel pass cheaper; nothing else in
the frame is big enough to matter.

`dot-per-tile` at 0.08 ms is the other end of the same fact: the tiling work means a drawing that
touches eighty pixels pays for eighty pixels, not for a frame. Every one of the four frozen scenes
covers the whole frame, so none of them can see that, which is worth remembering about the suite.

### A hypothesis about those 47 nanoseconds, tested and failed

The transfer encode is the most expensive thing the store-back does per pixel - a square root and an
interpolated table lookup per channel - and a solid fill hands it the same input for every pixel of a
run. A one-entry memo per channel was added, bit-identical by construction because a memo returns
exactly what the computation returned for the same input.

    UI-basic   16.85 ms before   16.89 ms after

Nothing. If the encode were the bulk of those 47 nanoseconds, a fullscreen solid fill would have
collapsed; it did not, so the encode is not where the time is. The change was reverted rather than
kept for the roughly two percent it may have moved on the two image-heavy scenes, which is inside
this suite's run-to-run spread. RECORDED BECAUSE THE DISPROOF IS THE RESULT: the next attempt at
that 47 nanoseconds starts knowing it is not the encode.

### Those 47 nanoseconds were `Tile::store`, and UI-basic now meets its ceiling (2026-09-19)

The number was never a pixel's drawing cost. Three probes were added to vary the command count and the
pixel count SEPARATELY instead of leaving them multiplied together:

    probe                      commands    pixels     time
    dot-per-tile                     88        88    0.09 ms
    half-of-every-tile                8   153,600   11.71 ms
    one-opaque-fullscreen             1   307,200   14.29 ms
    four-opaque-quarters              4   307,200   15.43 ms
    four-opaque-fullscreen            4 1,228,800   18.60 ms

**Four times the pixels costs 4.3 ms more - 4.7 nanoseconds a pixel, not 47.** A command is about a
microsecond: eighty-eight of them draw in nine hundredths of a millisecond. That left about thirteen
of the 14.3 milliseconds in neither term, and emptying `Tile::store`'s loop found them: **14.29 ms
became 1.34**. Ninety-one percent of a solid full-screen fill was the way OUT of the tile.

**And what it was doing is the defect.** `Tile::store` encoded one pixel at a time through
`encode_tabled`, taking every decision the encode makes - the transfer, whether there is a table, the
premultiply, the dither - once per pixel instead of once per row. `Encoder::encode_row` exists for
exactly that, says so in its own comment, and `Surface::store` beside it has always used it; this path
had the same loop written out by hand. The dither phase is the target's own x and y either way, which
is why the run form takes the row's first column.

| scene | before | after | ceiling | |
| --- | ---: | ---: | ---: | --- |
| UI-basic | 16.85 ms | **12.80 ms** | 16.7 ms | **met** |
| UI-effects | 184.85 ms | 179.11 ms | 66.7 ms | over 2.68x |
| vector-stress | 78.29 ms | 73.57 ms | 66.7 ms | over 1.10x |
| image-stress | 345.03 ms | 340.54 ms | 16.7 ms | over 20.4x |

UI-basic is the first of the four to meet its ceiling, and vector-stress is within ten percent of its
own. The 112-scene 2D conformance suite passes unchanged.

**And `Tile::load` had the same loop, which is the other half of the round trip.** It decoded one pixel
at a time through `decode_tabled`, asking whether there is a usable table and whether the working
space is linear once for every pixel instead of once for the row; `Decoder::decode_row` takes both
decisions at the top and is what the other surface type has always called. The frozen four barely
notice - they are held by other things - but the probes that actually PAY the decode do:
`one-translucent-fullscreen` 19.76 to 17.80 ms and `hundred-small-translucent` 16.62 to 15.15, about
ten percent each.

An encode memo, a faster square root and a cheaper store-back were all changes to the four point seven
nanoseconds. None of them was in `Tile::store`, which is why none of them moved anything.

**Two probes along the way were invalid and are recorded as such.** `Surface::load` and
`Surface::store` were emptied first and changed nothing, which read as two clean eliminations.
`replay` does not call them - it works on a `Tile`, and `target.rs` carries two types with a
`load`/`store` pair each. An experiment that measures a function nothing calls answers about nothing,
and it answers confidently.

### The same defect one layer up: the shader was dispatched per pixel (2026-09-19)

The span loop asked `Shader::at(x, y)` for every pixel of every non-solid run, and `at` matches on the
shader - the arm, the spread, the ramp, the transform's shape - once for each of them. The solid arm
beside it already avoided exactly this and said why: a match inside the hot loop is both the
arithmetic and what stops the loop being specialised at all. `Shader::row` takes the decision once.

    scene            before     after    ceiling
    vector-stress    73.42 ms  67.61 ms   66.7 ms   over 1.01x
    UI-effects      183.51 ms 174.82 ms   66.7 ms   over 2.62x
    image-stress    340.22 ms 333.34 ms   16.7 ms   over 20.0x

**vector-stress is now one percent over its ceiling**, from ten percent this morning, and it is stable
there: three runs gave 67.45, 67.87 and 67.61 ms.

THE ARITHMETIC IS UNCHANGED, PIXEL FOR PIXEL, and that is a constraint rather than a note. A gradient's
position is a linear function of `x`, so it could be advanced by a constant along the row whenever the
paint's transform is affine - which is most of them. That is NOT done: accumulating a step rounds
differently from evaluating the expression, and the 112-scene conformance suite compares these pixels
exactly. What was removed is the dispatch, not a multiply.

AND THE LAST ONE PERCENT WAS LEFT, deliberately. `spread_position` and `Ramp::at` still take their own
decisions per pixel, and hoisting those means either duplicating the loop once per spread mode or
duplicating the ramp's arithmetic in a second place - which is the thing that makes two implementations
drift apart, for one percent of one scene. The composite path was checked for the same defect and does
not have it: `composite_span` decides once and has a vectorised source-over run.

### The image shader, and a projective multiply that was computed twice (2026-09-19)

The same hoist for `Shader::Image`, which matched its `(pyramid, quality)` pair per pixel to choose
between two quite different bodies - an anisotropic walk over a pyramid, or a single sample. And one
exact removal beside it: `footprint` mapped the device point through the paint's inverse transform to
find `here`, which is the IDENTICAL expression the caller had just evaluated to find the texel, so a
mipmapped pixel paid FOUR projective multiplies where it needs three. The caller passes it now.

    probe                    before     after
    image-photo-bilinear    78.52 ms  73.90 ms
    image-photo-mipmapped   75.97 ms  71.38 ms
    image-photo-bicubic    257.43 ms 256.23 ms
    image-yuv-bilinear     162.68 ms 161.36 ms

**And the four frozen scenes now stand at:**

| scene | this morning | now | ceiling | |
| --- | ---: | ---: | ---: | --- |
| UI-basic | 16.85 ms | **12.44 ms** | 16.7 ms | **met** |
| vector-stress | 78.29 ms | 66.5-67.0 ms | 66.7 ms | **at the line** |
| UI-effects | 184.85 ms | 176.99 ms | 66.7 ms | over 2.65x |
| image-stress | 345.03 ms | 329.75 ms | 16.7 ms | over 19.7x |

**vector-stress is AT its ceiling and not under it.** Four consecutive runs gave 66.79, 66.84, 66.54
and 66.97 against 66.7 - one of the four under. This file's own rule about not freezing a number two
runs in seven would miss applies to calling it met: it is at the line, which is a different fact and
the one worth recording.

The two that are far out are held by things neither the tile round trip nor the shader dispatch
reaches: UI-effects by layers and filters, image-stress by the sampling itself - bicubic alone is 256
of its 330 milliseconds. Each needs its own probe before anything is changed.

### Where the two that are far out actually spend it (2026-09-19)

Both were measured the same way - empty the suspect, run the scene - before anything was changed, and
neither turned out to have the shape the first four did.

**UI-effects has no dominant term.** The layer composite is the obvious suspect: it walks the layer
pixel by pixel through `get`, `get`, `set` on a `&mut dyn Raster`, which is three virtual calls and a
clip-stack walk per pixel, with the opacity clamped again each time. Emptying it moves the scene from
177.0 to 172.8 ms - **four milliseconds of a hundred and seventy-seven.** Emptying the BLUR instead
moves it to 128.1, so the filter is about forty-nine of them, twenty-eight percent. The remaining
hundred and twenty-eight are spread across the forty-five commands and the tiles they touch. A hoist
does not reach a scene shaped like that, and the layer composite would have been a rewrite for two
percent.

**And the blur is already the careful version.** Separable, read and written a run at a time in both
passes, with the interior split from the edges so the fall-off-the-end test is answered once per pixel
rather than once per tap - and the tap order preserved deliberately, which is what makes it the same
number rather than a close one. What is left in it is the convolution: two passes of sixty taps over
four channels for a large sigma is the algorithm, and the usual way to go faster - three box blurs -
computes a DIFFERENT picture. That is a question about the profile's tolerance for a blur and not an
optimisation.

**image-stress is the sampler.** `image-photo-bicubic` alone is 256 of the scene's 330 milliseconds,
at sixteen taps a pixel; `image-yuv-bilinear` is another 161 in its own probe. The dispatch hoist and
the duplicate projective multiply were worth about six percent there, which is what was available
without touching the sampling itself.

SO NEITHER OF THE TWO IS CLOSABLE BY THE MOVE THAT CLOSED THE OTHERS, and that is the finding. What
they need is stated rather than guessed at: for UI-effects, a reason the hundred and twenty-eight
milliseconds outside the filter are what they are, which no probe here separates yet; for
image-stress, a faster bicubic and a faster YUV path, both of which are arithmetic per tap rather than
decisions per pixel.

### image-stress is a per-TAP transfer decode, and the fix is a memory decision (2026-09-19)

`Sampler::texel` was emptied, then its decode alone was emptied, which splits the image paths into
three terms:

    probe                     whole    no decode    no texel at all
    image-photo-bicubic     254.2 ms    115.2 ms          44.9 ms
    image-photo-bilinear     73.9 ms     40.5 ms          27.7 ms
    image-yuv-bilinear      161.4 ms    114.1 ms          27.8 ms

**The decode is 139 of bicubic's 254 milliseconds** - the single largest term in the whole 2D suite -
and the fetch is another 70. A bicubic pixel takes sixteen taps and decodes the transfer function on
every one of them, from sRGB to linear, for texels its neighbours decoded again a moment later.

**AND THE FIX IS ALREADY IN THE FILE, FOR ONE CASE.** `Pyramid` holds its levels "in the canonical
premultiplied linear float format" and is built in `prepare`; a mipmapped draw samples that and costs
71 ms where the bicubic draw of the same image costs 254. Extending it - decoding the source once for
bilinear and bicubic too - is the same move, and the decoded value would be bit-identical because it
is the same decoder, the same table and the same order: decode each texel, then weight it.

**IT IS NOT DONE HERE BECAUSE IT IS A MEMORY DECISION AND NOT AN OPTIMISATION.** `wants_pyramid`
exists precisely to decide which images pay for a decoded copy, level zero is sixteen bytes a texel,
and `max_prepared_scratch_bytes` is a profile limit. Trading replay time for prepared memory is the
right trade by this file's own measurement rules - the ceiling is on prepared replay and preparation
is reported separately - but which images pay it is the item's choice to make, not a patch's.

For the record of what it would be worth: removing the per-tap decode entirely would take bicubic from
254 to 115 ms and the YUV path from 161 to 114, which is most of `image-stress`'s 330. It would still
not be 16.7.

### The decoded source: the per-tap decode is gone, and what it cost (2026-09-19)

The section above said the decode was 139 of bicubic's 254 milliseconds, that removing it would take
bicubic to 115 and the YUV path to 114, and that the reason it was not done is that WHICH images pay
for a decoded copy is a decision rather than a patch. The decision is made and written down below;
this is what it measured.

| probe | before | after |
| --- | ---: | ---: |
| image-photo-bicubic | 256.4 ms | 135.5 ms |
| image-photo-bilinear | 73.9 ms | 48.8 ms |
| image-yuv-bilinear | 164.7 ms | 55.0 ms |
| image-widegamut-bilinear | 86.4 ms | 49.5 ms |
| image-photo-mipmapped | 74.0 ms | 74.0 ms |

The last row is the control: a mipmapped draw ALREADY sampled a decoded copy and did not move, which
is what says the other four moved for the reason claimed. The three bilinear probes now cost within
six milliseconds of each other whatever their source encoding is - sRGB, Rec. 2020 and a three-plane
YUV - because after the copy they are all the same canonical format, and the decoder that made them
different ran once per texel instead of once per tap.

And the frozen four, two consecutive runs:

| scene | prepare | replay median | ceiling | verdict |
| --- | ---: | ---: | ---: | --- |
| UI-basic | 1.14 / 0.74 ms | 11.93 / 11.81 ms | 16.7 ms | met |
| UI-effects | 20.98 / 20.81 ms | 152.10 / 149.81 ms | 66.7 ms | over 2.28x, was 2.65x |
| vector-stress | 7.57 / 7.08 ms | 67.02 / 66.55 ms | 66.7 ms | on the line |
| image-stress | 48.99 / 49.60 ms | 189.51 / 189.70 ms | 16.7 ms | over 11.4x, was 19.7x |

**AND THE 3D FRAME WENT 966 TO 934 ms ON THE THIRD AXIS OF A FLAT TEXTURE (2026-09-19).** `fetch`
wrapped an address on all three axes per tap, and every texture in the scene, the conformance suite
and the demo has depth one - where the caller only ever asks for `z = 0` and every wrap mode maps
index zero inside an extent of one to zero. A third of the per-tap addressing computed a constant.

| form | runs (ms/frame) | median | floor |
| --- | ---: | ---: | ---: |
| addressing all three axes | 966.3 / 991.3 / 944.3 | 966.3 ms | 33 ms |
| skipping a flat z | 937.3 / 933.9 / 924.8 | 933.9 ms | 33 ms |

The ranges barely overlap and the conformance suite is unchanged - `112 passed, 0 failed` and
`160 passed, 0 failed`. It is still a factor of twenty-eight.

**AND A ROUTE WAS REMOVED BY MEASUREMENT.** `soft3d`'s direct-mapped texel cache has NO consumer -
the conformance harness, `test3d-sw` and the benchmark all call the uncached entry point. Wiring it
into the benchmark moved the texturing stage from 289.6 to 288.1 ms, inside the spread, because the
fixture's checkerboards are not the sRGB-without-a-chain case it was built for. The wiring was taken
back out: a benchmark using a cache no renderer uses measures a path nobody takes.

**AND vector-stress CROSSED THE LINE (2026-09-19).** The linear gradient's row decided
`length_squared <= 0.0` once per PIXEL about a value that does not vary at all; it is decided once
per row now, and the pixels are identical - the same `ramp.at(1.0)` the arm already produced.
Measured both ways, three runs each, because a third of a millisecond on a scene sitting on its
budget is exactly the size of claim that needs it:

| form | runs | median | ceiling | verdict |
| --- | ---: | ---: | ---: | --- |
| the test inside the loop | 66.835 / 66.473 / 66.606 | 66.606 ms | 66.7 ms | one of three over |
| the test hoisted out | 66.157 / 66.320 / 66.250 | 66.250 ms | 66.7 ms | three of three met |

**AND `image-stress` IS TWO SCENES (2026-09-19), on the project owner's answer to the question this
floor put to them.** The split is where the scene's own comments already drew it: resampling over one
source with source and target in the same space, and colour conversion whose full-frame draws put
three hundred thousand pixels through a transfer function and a matrix.

| scene | median | ceiling | over |
| --- | ---: | ---: | ---: |
| image-resample | 80.6 ms | 16.7 ms | 4.8x |
| image-convert | 127.1 ms | 16.7 ms | 7.6x |

The single number was 11.4x and said which half was slow only by accident of how the two were
summed. The frozen counts came with them - 11 commands over 1 resource and 14 over 2, summing to the
25 and 3 the one scene recorded.

**CORRECTED THE SAME DAY: IT IS NOT MET.** Six later runs of the same binary read 67.13, 67.09,
67.46, 67.69, 67.24 and 67.41 - every one over 66.7. What changed between the two sets is not the
gradient, which is exact and conformance-proved: the suite gained a fifth scene, and this scene moves
by more than half a percent when anything else in the process does. The hoist is worth about 0.35 ms
measured back to back; the scene's sensitivity to its neighbours is larger. `vector-stress` is STILL
AT ITS LINE. The floor asks for four scenes and has one - `UI-basic` at 11.8 against 16.7.

**WHICH IMAGES PAY FOR IT IS A DECISION AND IT IS TWO PREDICATES.** `wants_pyramid` is unchanged and
decides which images NEED a chain - only a `Mipmapped` draw cannot be served without one.
`wants_decoded` decides which merely go FASTER with a decoded source: the ones the list samples
`Bilinear` or `Bicubic`. The first kind is required, the second is optional, and they are built in
that order.

**AN OPTIONAL COPY IS GIVEN BACK RATHER THAN REFUSING A FRAME.** `max_prepared_scratch_bytes` is a
profile limit and a decoded level zero is sixteen bytes a texel, so a list that fitted before this
existed must still fit: the optional copies are dropped newest-first until the prepared total is
under the ceiling, and only then is a frame that still does not fit refused. An optional copy that
will not ALLOCATE is not an error either - the image samples the way it always did, one decode per
tap.

**AND AN OPTIONAL COPY IS LEVEL ZERO ALONE.** A bilinear or bicubic draw reads level zero and nothing
else; the halvings under it are prepare time and prepared memory no draw touches. Building the whole
chain for them cost 12.6 ms of UI-effects' preparation and a third of the bytes, for nothing:
`Pyramid::base_from_sampler` is the level-zero constructor and `from_sampler` is now the chain built
on top of it, so the two callers ask for exactly what they read.

    UI-effects prepare    2.4 ms  ->  33.5 ms  ->  20.9 ms
    image-stress prepare 38.8 ms  ->  53.5 ms  ->  49.3 ms
                         before      whole       level zero
                                     chain       alone

**IT TRADES REPLAY TIME FOR PREPARE TIME AND PREPARED MEMORY**, which is the trade this file's rules
ask for rather than one it tolerates: the ceiling is on prepared REPLAY, preparation happens once for
a drawing that is replayed, and both are measured and reported separately above.

WHAT IS LEFT IN THE TWO SCENES THAT MISS. image-stress is now the fetch and the arithmetic the
earlier probes separated out - 70 ms and 45 of the old bicubic's 254 - plus what a sixteen-byte texel
costs to walk compared with a four-byte one, which is the price of the copy showing up on the other
side. UI-effects is where it was: the blur is 49 ms of it and the remaining hundred and twenty-eight
are spread over forty-five commands, with no dominant term and no probe here separating one.
