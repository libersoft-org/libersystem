# WebP interoperability fixtures

The fixtures were generated on 2026-07-17 with libwebp 1.5.0 command-line
tools from deterministic ImageMagick 7.1.1-43 PNG sources.

```sh
cwebp -q 82 opaque.png -o external-vp8.webp
cwebp -q 82 -alpha_q 100 alpha.png -o external-alph-vp8.webp
cwebp -lossless -z 9 alpha.png -o external-vp8l.webp
webpmux \
  -frame frame0.webp +0+0+0+1-b \
  -frame frame1.webp +37+2+2+0+b \
  -loop 3 -bgcolor 200,9,19,29 \
  -o external-animation.webp
```

`webpinfo` identifies simple lossy `VP8 `, extended `VP8X+ALPH+VP8 `, simple
lossless `VP8L`, and animated `VP8X+ANIM+ANMF+ANMF`. `dwebp` and ImageMagick
produce byte-identical RGBA for every static fixture:

| Fixture | Dimensions | RGBA FNV-1a | SHA-256 |
| --- | ---: | --- | --- |
| `external-vp8.webp` | 23x15 | `f2da7877eabb5d1e` | `e9926578e7f146b290a4b24637443aa4fdbfaf6fc3a45757e40d0b15070e2eae` |
| `external-alph-vp8.webp` | 19x13 | `3bb46987825ab3fc` | `f6a434ea9afa35aba3a38b0891aa94c924d186bc81e4da4c4662ed43c031cece` |
| `external-vp8l.webp` | 19x13 | `35fa330e03913460` | `36b25dfb15ca9e8e63c0e9d351332b747cba3580c66b8e9607bc67ae16750d6b` |

The animation has canvas 23x15, ARGB background `c8 09 13 1d`, loop 3, and
these raw ANMF frames:

| Frame | Rectangle | Duration | Blend | Disposal | Raw RGBA FNV-1a |
| ---: | --- | ---: | --- | --- | --- |
| 0 | 23x15 at 0,0 | 0 ms | source | background | `8cb7e5da66d851a1` |
| 1 | 19x13 at 2,2 | 37 ms | over | keep | `35fa330e03913460` |

The artifact SHA-256 is
`6525c4067f0bdc0f4c79fb5fdb8c81b94f9657f7d40fc6bb63ada8e1d3c24bdb`.
LiberSystem applies the declared ANIM background when disposal requests
background, yielding displayed hashes `8cb7e5da66d851a1` and
`a9ed68c4c84d1792`. libwebp `anim_dump` clears that rectangle to transparent
black (`e59582f055d7fee0` for frame 1); ImageMagick preserves alpha-zero source
RGB (`57feaba058e91ad0`). Raw frames and metadata agree; the displayed difference
is an external background-policy choice and is documented rather than hidden.

The reciprocal conformance runner verifies LiberSystem VP8/VP8L/ALPH output
through `webpinfo`, `dwebp`, and ImageMagick. LiberSystem animation output is
canonicalized to full-canvas source frames, so libwebp `anim_dump` reproduces
its displayed pixels exactly without depending on an external background
policy.

The fixture images were created for LiberSystem and are released under the
Unlicense with the rest of the project. libwebp and ImageMagick are host-side
independent interoperability tools only; no source or runtime dependency from
either project is included.
