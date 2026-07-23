# TGA interoperability fixtures

The fixtures were generated on 2026-07-17 with ImageMagick 7.1.1-43 from an
asymmetric 11x7 RGBA image. The four source quadrants use red, green, blue and
50%-alpha yellow so orientation and alpha errors cannot cancel out. The 24-bit
fixtures use an opaque view of the same pixels.

```sh
magick -size 11x7 xc:none \
  -fill '#ff2100' -draw 'rectangle 0,0 4,2' \
  -fill '#00d43f' -draw 'rectangle 5,0 10,2' \
  -fill '#174cff' -draw 'rectangle 0,3 4,6' \
  -fill '#e4c90080' -draw 'rectangle 5,3 10,6' source.png

magick source.png -alpha off -type TrueColor -define tga:bits-per-pixel=24 \
  -orient TopLeft -compress none raw24-top-left.tga
magick source.png -alpha off -flop -type TrueColor -define tga:bits-per-pixel=24 \
  -orient TopRight -compress none raw24-top-right.tga
magick source.png -alpha off -flip -type TrueColor -define tga:bits-per-pixel=24 \
  -orient BottomLeft -compress rle rle24-bottom-left.tga
magick source.png -alpha off -rotate 180 -type TrueColor \
  -define tga:bits-per-pixel=24 -orient BottomRight -compress rle \
  rle24-bottom-right.tga

magick source.png -type TrueColorAlpha -define tga:bits-per-pixel=32 \
  -orient TopLeft -compress none raw32-top-left.tga
magick source.png -rotate 180 -type TrueColorAlpha \
  -define tga:bits-per-pixel=32 -orient BottomRight \
  -set comment 'LiberSystem TGA corpus' -compress none \
  raw32-bottom-right-id.tga
magick source.png -flop -type TrueColorAlpha -define tga:bits-per-pixel=32 \
  -orient TopRight -compress rle rle32-top-right.tga
magick source.png -flip -type TrueColorAlpha -define tga:bits-per-pixel=32 \
  -orient BottomLeft -compress rle rle32-bottom-left.tga
```

The inverse image transforms make every orientation describe the same visual
image. `magick fixture.tga -auto-orient -depth 8 rgba:fixture.rgba` produces one
canonical RGBA buffer for every 24-bit fixture and another for every 32-bit
fixture. The complete-buffer FNV-1a values are `d82e2877e771e430` for opaque
24-bit output and `6d9c01b7743f6b90` for alpha-preserving 32-bit output.
`raw32-bottom-right-id.tga` carries the 22-byte image ID `LiberSystem TGA corpus`.

| Fixture | Type | Depth | Origin | SHA-256 |
| --- | ---: | ---: | --- | --- |
| `raw24-top-left.tga` | 2 | 24 | top-left | `19b0ce70f7c107ff7f26de42ad8d39a7627a28e8d8261a157196f786bee463fd` |
| `raw24-top-right.tga` | 2 | 24 | top-right | `a97a966103e8bb6a51ff307c04a5024cbd3ff6b9279fafbfc5eb15e014e69c4e` |
| `rle24-bottom-left.tga` | 10 | 24 | bottom-left | `b247a3f080c0b91494d2c9249011ee497c1bb0e2fe0f61f176bef8b523863bb9` |
| `rle24-bottom-right.tga` | 10 | 24 | bottom-right | `45803bb25a39fd46d363a6897b462ce8a7b84c790baf4ab60416929aa3407131` |
| `raw32-top-left.tga` | 2 | 32 | top-left | `fae93516ee1b2ebf2db57b0dfa568587bf997b5664e975bd0cc7fbe4adfe3282` |
| `raw32-bottom-right-id.tga` | 2 | 32 | bottom-right | `b09627a878df7f4861b06bb300ca74a34262ee8bf62d6ae8c0d85ffe0df0b7b6` |
| `rle32-top-right.tga` | 10 | 32 | top-right | `85f51437339de427a6cfc1529d074f1b577dcc967ee96fd1228b13e2dc23c70d` |
| `rle32-bottom-left.tga` | 10 | 32 | bottom-left | `90c204a06f2b7a01e3f31fd6084990d55a0c972e7b027628717f6e8ace12d15f` |

The fixture images were created for LiberSystem and are released under the
Unlicense with the rest of the project. ImageMagick is a host-side independent
interoperability tool only; no source or runtime dependency is included.
