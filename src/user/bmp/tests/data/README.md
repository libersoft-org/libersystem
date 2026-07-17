# BMP interoperability fixtures

The fixtures were generated on 2026-07-17 with ImageMagick 7.1.1-43, Netpbm
11.10.2 and Pillow 11.1.0. `external-rgb24.bmp` is direct ImageMagick BMP3
24-bit `BI_RGB`; `external-indexed8.bmp` is direct Netpbm 8-bit `BI_RGB` with a
palette; `external-v5-alpha.bmp` is direct ImageMagick 32-bit V5
`BI_BITFIELDS` with RGBA masks.

```sh
magick -size 19x7 -define gradient:angle=90 gradient:'#102030-#e0c080' \
  -fill '#ff1100' -draw 'rectangle 0,0 3,1' \
  -fill '#00d050' -draw 'rectangle 14,5 18,6' \
  -alpha off -type TrueColor -depth 8 BMP3:external-rgb24.bmp

magick -size 37x7 -define gradient:angle=90 gradient:'#102030-#e0c080' \
  -fill '#ff1100' -draw 'point 5,3' \
  -fill '#00d050' -draw 'point 31,4' \
  -alpha off -colors 64 -type Palette -depth 8 ppm:indexed-source.ppm
ppmtobmp -bpp=8 indexed-source.ppm > external-indexed8.bmp

magick -size 19x7 gradient:'#10203020-#e0c080f0' \
  -type TrueColorAlpha external-v5-alpha.bmp
```

ImageMagick emits a 124-byte V5 header. `derived-v4-alpha.bmp` and
`derived-v3-alpha.bmp` retain every pixel and mask byte while truncating only
the header to 108 and 56 bytes, respectively, and adjusting `bfSize`,
`bfOffBits`, and `biSize`. All three keep `BI_BITFIELDS` and the masks
`00ff0000`, `0000ff00`, `000000ff`, `ff000000`. ImageMagick and Pillow produce
byte-identical RGBA for V3/V4/V5.

`derived-rgb32.bmp` retains the V5 pixel bytes but uses a 40-byte info header
with `BI_RGB` and no masks. Pillow and Netpbm agree that its high byte is unused
and decode every pixel as opaque, matching the Microsoft definition and
LiberSystem. ImageMagick instead interprets those bytes as alpha; this
implementation divergence is documented rather than hidden.

| Fixture | Profile | RGBA FNV-1a | SHA-256 |
| --- | --- | --- | --- |
| `external-rgb24.bmp` | V3, 24bpp `BI_RGB` | `e6254907e10a8c80` | `3c57be1afd0dece379dbe35f45c796b69fa65fc1366fd266ffc4ec76a7e070e4` |
| `derived-rgb32.bmp` | V3, 32bpp `BI_RGB` | `ad993a3914fe5247` | `4de345a73ab5cc2ed0f1be43f4c45852c39a5d3628651d509e416bbb799d855a` |
| `external-indexed8.bmp` | V3, indexed 8bpp `BI_RGB` | `2941c44be6b719ed` | `38b416afe8ecdf194eba2a55671197cc6129b58122fd476874c70e51fa615159` |
| `derived-v3-alpha.bmp` | 56-byte V3, RGBA masks | `f61b87cde45b3532` | `17a26b7c06cfd71c94f2b2a6758d9e67a639cce4970fa2fa3ac5a97fe86ba7a3` |
| `derived-v4-alpha.bmp` | 108-byte V4, RGBA masks | `f61b87cde45b3532` | `8af21ebb114a56447d0bb1d16dfa48ccf6482eafa00d36e674e0fdd146fbe399` |
| `external-v5-alpha.bmp` | 124-byte V5, RGBA masks | `f61b87cde45b3532` | `4489c512b64dd96a9bfc3a5a884a4f7e72bba3d59843d1895b82a2be2489689a` |

The fixture images were created for LiberSystem and are released under the
Unlicense with the rest of the project. ImageMagick, Netpbm and Pillow are
host-side independent interoperability tools only; no source or runtime
dependency from those projects is included.
