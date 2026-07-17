# PPM interoperability fixtures

The fixtures were generated on 2026-07-17 with Debian Netpbm 11.10.2 from a
deterministic 13x5 8-bit PAM source made by ImageMagick 7.1.1-43.

```sh
magick -size 13x5 gradient:'#102030-#e0c080' \
  -fill '#ff1100' -draw 'rectangle 0,0 2,1' \
  -fill '#00d050' -draw 'rectangle 9,3 12,4' \
  -fill '#174cff' -draw 'point 6,2' \
  -alpha off -type TrueColor -depth 8 pam:source.pam

pamdepth 31 source.pam | pamtopnm | pnmtoplainpnm > base-p3.ppm
awk 'NR==1 { print; print "# Netpbm 11.10.2 external P3"; next }
     NR==2 { print "# dimensions follow"; print;
             print "# scaled five-bit samples"; next }
     { print }' base-p3.ppm > external-p3-max31.ppm

pamdepth 65535 source.pam | pamtopnm > external-p6-max65535.ppm
```

`external-p3-max31.ppm` exercises comments between header tokens, plain-text
samples and five-bit `Maxval` scaling. Netpbm `pamdepth 255` uses nearest
rounding, matching LiberSystem exactly. ImageMagick truncates 66 of the 195
color samples by one when converting this low-Maxval image to RGBA8; no sample
differs by more than one. Both interpretations are deterministic and the test
pins the Netpbm-native result.

`external-p6-max65535.ppm` exercises the required two-byte big-endian raw
samples. Netpbm and ImageMagick produce byte-identical RGBA8 for this fixture.

| Fixture | Profile | Dimensions | RGBA FNV-1a | SHA-256 |
| --- | --- | ---: | --- | --- |
| `external-p3-max31.ppm` | P3, comments, Maxval 31 | 13x5 | `baa7ce5824206a93` | `28329043c70ac13008826226b2ca4714d40d84e9e8f1aff3d9f43e972c3f9c6f` |
| `external-p6-max65535.ppm` | P6, 16-bit big-endian | 13x5 | `571dbaab58b175f0` | `6bb30111f944d1d21e9e043c7b16da49ed6cc203bc18083bf955247981e0528b` |

The fixture images were created for LiberSystem and are released under the
Unlicense with the rest of the project. Netpbm and ImageMagick are host-side
independent interoperability tools only; no source or runtime dependency from
either project is included.
