# PCX interoperability fixtures

The fixtures were generated on 2026-07-17 with ImageMagick 7.1.1-43. Odd widths
exercise PCX `bytes_per_line` values that are not rounded to an even number by the
external writer.

```sh
magick -size 17x9 gradient:red-blue -colors 16 -type Palette indexed.pcx
magick -size 19x7 gradient:red-blue -type TrueColor rgb.pcx
pcxtoppm indexed.pcx > indexed.ppm
pcxtoppm rgb.pcx > rgb.ppm
```

`indexed.pcx` is version 5, 8-bit, one-plane RLE with `bytes_per_line=17` and a
trailing `0x0c` plus 256-entry RGB palette. `rgb.pcx` is version 5, 8-bit,
three-plane RLE with `bytes_per_line=19`. Netpbm `pcxtoppm` and ImageMagick
produced byte-identical RGBA output for both files.

| Fixture | Dimensions | RGBA bytes | FNV-1a | SHA-256 |
| --- | ---: | ---: | ---: | --- |
| `indexed.pcx` | 17x9 | 612 | `847cd3e9781c6ac8` | `0e4bdaec186280a431996be45e75ac03ffc55668cdfa2dc43a5f6a4778c0c1b4` |
| `rgb.pcx` | 19x7 | 532 | `4f4ff2d90ee4ac63` | `e252f6bdd42072037a043e329cb1426991a1bef1d22a9cb44ee76fa4cc4f7073` |

The fixture images were created for LiberSystem and are released under the
Unlicense with the rest of the project. ImageMagick and Netpbm are host-side
independent interoperability tools only; no source or runtime dependency from
either project is included.
