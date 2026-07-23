# PNG interoperability fixtures

The fixtures were generated on 2026-07-17 with ImageMagick 7.1.1-43 and Pillow
11.1.0. Every file passes pngcheck 3.0.3.

- `external-gray4.png`: ImageMagick PNG00, color type 0, depth 4.
- `external-indexed-trns.png`: Pillow `P` mode, color type 3, depth 8, four
  palette entries and a `tRNS` alpha table whose fourth entry is transparent.
- `external-rgba16.png`: ImageMagick PNG64, color type 6, depth 16.
- `external-adam7-rgb.png`: ImageMagick PNG24, color type 2, depth 8, Adam7.
- `derived-multi-idat.png`: byte-identical IHDR/ancillary/zlib stream from the
  Adam7 fixture, with its one IDAT payload split into three consecutive IDAT
  chunks and each chunk CRC recomputed. No compressed or pixel byte changed.

ImageMagick and Pillow agree on complete RGBA8 output. The multi-IDAT derivative
decodes exactly like its single-IDAT source.

| Fixture | Dimensions | RGBA FNV-1a | SHA-256 |
| --- | ---: | --- | --- |
| `external-gray4.png` | 17x9 | `aa3a76465cdcdbf8` | `221dc82c25756ab3259f84a1602f7eaf3d612a802e064a384a6ab1542bc1ad20` |
| `external-indexed-trns.png` | 19x7 | `9016c6cb8c8b27d1` | `f7ec2ac7a1a494c3cc4383cf5741b157f625526bef16a64b126ace51011239b4` |
| `external-rgba16.png` | 13x11 | `16587931a19e490a` | `de64f27a260a8cc7768674b0b34a7f92eec867f12d61650e5d0343b06f8e66a5` |
| `external-adam7-rgb.png` | 23x15 | `8cb7e5da66d851a1` | `71e6810f0a2d5b01bffa9a8e9259a734a71f54bbb96e3fa06209d0436cd5fc92` |
| `derived-multi-idat.png` | 23x15 | `8cb7e5da66d851a1` | `5c0342f0a5ebd5a04e8298d7cc86937998c336581baf41b55a6a5a699fd577a5` |

The fixture images were created for LiberSystem and are released under the
Unlicense with the rest of the project. ImageMagick, Pillow and pngcheck are
host-side independent interoperability tools only; no source or runtime
dependency from those projects is included.
