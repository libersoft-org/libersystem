# QOI interoperability fixtures

The fixtures were generated on 2026-07-17 with Debian Netpbm 11.10.2
(`pamtoqoi`) from deterministic 8-bit PAM images made by ImageMagick 7.1.1-43.
The RGB source deliberately combines small deltas, luma deltas, repeated runs,
index reuse and abrupt colors so every non-alpha QOI opcode family occurs.

```sh
magick \
  \( -size 257x1 -define gradient:angle=90 gradient:'#000000-#ffffff' \) \
  \( -size 65x1 -define gradient:angle=90 gradient:'#000000-#c0f0a0' \
     -background '#c0f0a0' -gravity west -extent 257x1 \) \
  \( -size 257x1 xc:'#204060' -fill '#ff1207' -draw 'point 80,0' \
     -fill '#204060' -draw 'point 160,0' \) \
  -append -alpha off -type TrueColor -depth 8 pam:rgb.pam
pamtoqoi rgb.pam > external-rgb.qoi

magick -size 17x9 gradient:'#08102000-#e0c080ff' \
  -fill '#20406040' -draw 'rectangle 0,0 4,1' \
  -fill '#f01020c0' -draw 'point 8,4' \
  -fill '#20406040' -draw 'point 14,7' \
  -type TrueColorAlpha -depth 8 pam:rgba.pam
pamtoqoi rgba.pam > external-rgba.qoi
```

Both `qoitopam` from Netpbm and ImageMagick decode each fixture to the exact
source RGBA bytes. The QOI colorspace field is 0 (sRGB with linear alpha).

| Fixture | Dimensions | Channels | INDEX | DIFF | LUMA | RUN | RGB | RGBA | RGBA FNV-1a | SHA-256 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- |
| `external-rgb.qoi` | 257x3 | 3 | 1 | 256 | 64 | 10 | 2 | 0 | `849367b233036f72` | `f1b3a798f522230db8284f3c4c582659d7ea3b4c71cb931eb83163caf64d2109` |
| `external-rgba.qoi` | 17x9 | 4 | 2 | 0 | 0 | 13 | 0 | 13 | `e8e4862424b712f2` | `cfc6fef7b1fed2e9e8ff10de52ba85370428add86001e9a2258831b232d23a69` |

The fixture images were created for LiberSystem and are released under the
Unlicense with the rest of the project. Netpbm and ImageMagick are host-side
independent interoperability tools only; no source or runtime dependency from
either project is included.
