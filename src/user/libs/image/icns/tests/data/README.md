# ICNS interoperability fixtures

`external-gradient.icns` was generated on 2026-07-17 with Debian trixie
`icnsutils 0.8.1.83.g921f972-0.1+b2` from three deterministic PNG files made by
ImageMagick 7.1.1-43:

```sh
for size in 16 32 128; do
    magick -size ${size}x${size} gradient:red-blue \
        \( -size ${size}x${size} gradient:black-white \) \
        -alpha off -compose CopyOpacity -composite -depth 8 \
        icon_${size}x${size}.png
done
png2icns external-gradient.icns \
    icon_16x16.png icon_32x32.png icon_128x128.png
icns2png -x external-gradient.icns
```

The resulting entry inventory is `is32+s8mk`, `il32+l8mk`, and PNG-backed
`ic07`. `icns2png` reproduced the 16 and 32 pixel source images with zero changed
pixels. Its 128 pixel export dropped alpha, so the modern golden is the PNG
payload embedded by `png2icns`; that payload compares with zero changed pixels
against the 128 pixel source in ImageMagick.

The remaining classic profiles were generated from the same deterministic
gradient construction. `png2icns` directly emits the 48-pixel pair. For legacy
128 pixels, `png2icns` selects modern `ic07`, so the checked-in host-only
`generate-legacy.rs` requests the explicit `it32` and `t8mk` types through the
public libicns API. The generator is LiberSystem source under the Unlicense; the
external library supplies the independent RLE/container implementation.

```sh
magick -size 48x48 gradient:red-blue \
    \( -size 48x48 gradient:black-white \) \
    -alpha off -compose CopyOpacity -composite -depth 8 icon_48x48.png
png2icns external-48.icns icon_48x48.png

magick icon_128x128.png -depth 8 rgba:icon_128x128.rgba
rustc --edition 2024 -O generate-legacy.rs -l icns -o generate-legacy
./generate-legacy icon_128x128.rgba external-128-legacy.icns
```

The host versions are Debian trixie libicns/icnsutils
`0.8.1.83.g921f972-0.1+b2` and ImageMagick 7.1.1-43. `icns2png` reports
`ih32+h8mk` for `external-48.icns` and `it32+t8mk` for
`external-128-legacy.icns`; extracting either changes zero pixels against its
source image.

The test compares complete decoded RGBA images using 64-bit FNV-1a:

| Fixture/profile | Size | RGBA bytes | FNV-1a |
| --- | ---: | ---: | ---: |
| `external-gradient.icns` / `is32+s8mk` | 16 | 1,024 | `40dfaed03a6f7825` |
| `external-gradient.icns` / `il32+l8mk` | 32 | 4,096 | `1f5c3caa89bf3ee5` |
| `external-48.icns` / `ih32+h8mk` | 48 | 9,216 | `2fc27b31ff9d8545` |
| `external-gradient.icns` / `ic07` | 128 | 65,536 | `cec7119daf9c2425` |
| `external-128-legacy.icns` / `it32+t8mk` | 128 | 65,536 | `cec7119daf9c2425` |

Artifact SHA-256:

```text
628ce149e216d76769438a9de3ddd3178c321fceac6dd38fce18c6432eed8235  external-gradient.icns
9972a34f93e96cb39dec177850bce5c7765d7fb883748ed4a7fd26721fc1443b  external-48.icns
91a5926ac49952159d98bac684c30ab6a70033cd665b6a4a99d8a07aa3f6332d  external-128-legacy.icns
```

The fixture images and container were created for LiberSystem and are released
under the Unlicense with the rest of the project. `icnsutils` and ImageMagick are
host-side independent interoperability tools only; no source or runtime dependency
from either project is included.
