# ICNS interoperability fixture

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

The test compares complete decoded RGBA images using 64-bit FNV-1a:

| Size | RGBA bytes | FNV-1a |
| ---: | ---: | ---: |
| 16 | 1,024 | `40dfaed03a6f7825` |
| 32 | 4,096 | `1f5c3caa89bf3ee5` |
| 128 | 65,536 | `cec7119daf9c2425` |

Artifact SHA-256:

```text
628ce149e216d76769438a9de3ddd3178c321fceac6dd38fce18c6432eed8235  external-gradient.icns
```

The fixture images and container were created for LiberSystem and are released
under the Unlicense with the rest of the project. `icnsutils` and ImageMagick are
host-side independent interoperability tools only; no source or runtime dependency
from either project is included.
