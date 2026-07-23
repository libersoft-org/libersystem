# JPEG interoperability fixtures

The fixtures were generated on 2026-07-17 with ImageMagick 7.1.1-43 at quality
92. Marker inspection pins 8-bit SOF0 with one component for grayscale, 8-bit
SOF0 with three components for YCbCr, and SOF2 with three components for the
progressive rejection fixture.

```sh
magick -size 23x11 -define gradient:angle=90 gradient:black-white \
  -colorspace Gray -sampling-factor 1x1 -interlace none -quality 92 \
  external-gray-baseline.jpg
magick -size 19x13 gradient:'#102030-#e0c080' \
  -fill '#ff1100' -draw 'rectangle 0,0 3,2' \
  -sampling-factor 2x2 -interlace none -quality 92 \
  external-ycbcr-baseline.jpg
magick -size 19x13 gradient:'#102030-#e0c080' \
  -sampling-factor 2x2 -interlace Plane -quality 92 \
  external-progressive.jpg
```

ImageMagick and Pillow 11.1.0 produce byte-identical RGBA for all three files.
LiberSystem's grayscale baseline decode is also byte-identical. Its independent
zune JPEG IDCT/chroma path differs from the canonical YCbCr RGBA by at most 2,
with mean absolute error 0.232 per RGBA byte; the deterministic LiberSystem RGBA
FNV-1a is `aa27fe0ac440e9e4`. The canonical YCbCr RGBA bytes are checked in so the
tolerance compares actual pixels rather than two codec implementations sharing
one result.

| Fixture | Profile | RGBA FNV-1a | SHA-256 |
| --- | --- | --- | --- |
| `external-gray-baseline.jpg` | SOF0, grayscale | `114ecee60ce1ccc3` | `e53103ef2e67abf7ab5e8484d3ffc758be6cef07421fe6cc456b48dbdeea8697` |
| `external-ycbcr-baseline.jpg` | SOF0, 3-component YCbCr | `f24fbec952b2a933` | `91687240e27dab31a2b5b08753a1ec8a6d28b770f25e0296135ec32d6f6a4acf` |
| `external-ycbcr-baseline.rgba` | canonical decoded RGBA | `f24fbec952b2a933` | `b5e163306bc388f00badd8b5dffc7345d620827a1b56de618723c80a3ea3b39a` |
| `external-progressive.jpg` | SOF2, progressive | `fdd405b45a4e1ba6` | `8ddf40b707b2dee803e1f8ae118d54188432353c07e9988e1adb4a10d3ad8d73` |

The reciprocal `jpeg-conformance` runner encodes a deterministic RGB source at
quality 10 and 100. Both outputs are three-component SOF0/JFIF, and ImageMagick
and Pillow decode each to identical RGBA. Encoded artifacts are deterministic:
quality 10 is 939 bytes with FNV-1a `56b6ea1065d7fb11` and RGB MSE 2755.800;
quality 100 is 9,833 bytes with FNV-1a `70a6ab10e167dbb8` and RGB MSE 0.566.

The fixture images were created for LiberSystem and are released under the
Unlicense with the rest of the project. ImageMagick and Pillow are host-side
independent interoperability tools only; no source or runtime dependency from
either project is included.
