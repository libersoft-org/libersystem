# GIF interoperability fixtures

`external-animation.gif` was generated on 2026-07-17 with ImageMagick 7.1.1-43
single-frame inputs and gifsicle 1.96. It has a 29x17 logical screen, loop count
2, and three interlaced frames:

| Frame | Rectangle | Delay | Disposal |
| ---: | --- | ---: | --- |
| 0 | 29x17 at 0,0 | 0 ms | keep/asis |
| 1 | 11x9 at 5,4 | 30 ms | background |
| 2 | 9x7 at 12,2 | 50 ms | previous |

`derived-local-subblocks.gif` preserves every palette index and compressed LZW
byte. It adds a local 8-entry color table to frame 1 by copying the global table
and setting the descriptor flag, then repartitions each image-data stream into
short sub-blocks. Descriptor bytes are `40`, `c2`, `40`; block lengths are
`1,2,3,5,8,13,1,2,3,4`, `1,2,3,5,4`, and `1,2,3,5,3`. gifsicle validates both
files and reports the local table only in the derivative.

LiberSystem and `gifsicle --unoptimize` produce identical full-canvas RGBA:

| Frame | RGBA FNV-1a |
| ---: | --- |
| 0 | `16b257ac54aedc1c` |
| 1 | `c053dfb1daa4c81e` |
| 2 | `1e5fd5b6c7a7055a` |

ImageMagick agrees on frames 0 and 1. Before frame 2 it clears disposal-2 pixels
to transparent green rather than the GIF logical-screen background; gifsicle,
LiberSystem and the GIF89a restore-to-background semantics agree on the opaque
background result. The difference is documented instead of weakening disposal
behavior to match one implementation.

Artifact SHA-256:

```text
f36b8641335516a87e58cb0d5bff2d6a0970c26d207816a182219517b254c402  external-animation.gif
1dd1dfbf8ae6a39b2dc2f17c57a2134426c5db5a315efc64406e866f8123fda5  derived-local-subblocks.gif
```

The fixture images were created for LiberSystem and are released under the
Unlicense with the rest of the project. ImageMagick and gifsicle are host-side
independent interoperability tools only; no source or runtime dependency from
either project is included.
