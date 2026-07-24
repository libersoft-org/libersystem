# ICO interoperability fixtures

The fixtures were generated on 2026-07-17 with ImageMagick 7.1.1-43.
ImageMagick writes a 32x32 `BITMAPINFOHEADER` DIB entry and a 256x256
PNG-backed entry for these commands:

```sh
magick -size 32x32 gradient:'#10203020-#e0c080f0' \
  -type TrueColorAlpha external-dib-alpha.ico
magick -size 32x32 xc:'#10203000' -fill '#e0c08000' \
  -draw 'rectangle 16,0 31,31' -type TrueColorAlpha \
  external-dib-zero-alpha.ico
magick -size 256x256 gradient:'#10203020-#e0c080f0' \
  -type TrueColorAlpha external-png.ico
```

`external-dib-zero-alpha.ico` has zero in every XOR BGRA alpha byte. Both
ImageMagick and icoutils 0.32.3 preserve those zero alpha values instead of
falling back to the AND mask.

`external-dib-maskless.ico` is derived from `external-dib-alpha.ico` by removing
the final 128-byte, 32-row AND bitmap and subtracting 128 from the directory
payload length. No XOR bytes are changed. ImageMagick decodes it identically to
the source fixture. icoutils rejects this deployed maskless convention as an
incorrect total bitmap size, so maskless acceptance remains a documented
ImageMagick/image-rs/Pillow interoperability subset rather than a universal ICO
claim.

ImageMagick and icoutils produce byte-identical RGBA for every standard fixture.
The complete decoded buffers are pinned with FNV-1a:

| Fixture | Entry | Dimensions | RGBA FNV-1a | SHA-256 |
| --- | --- | ---: | --- | --- |
| `external-png.ico` | PNG | 256x256 | `58a21c35773784bc` | `3cc4dfce2f9fc52ba1f634081f9157c3803a2350a233f7487a74ca6b168083af` |
| `external-dib-alpha.ico` | 32bpp DIB + AND | 32x32 | `2cae8d72a65bcac1` | `c7655ecbe9231d93dd50e9b9b2a37ddba3cb4b7c81ab59207f3e167254dffe25` |
| `external-dib-zero-alpha.ico` | 32bpp DIB + AND | 32x32 | `8fa6b411bfca0325` | `d4ed898b2a95fa958e6dd429a616a098b3774a06ce36c2cf05d9e4ba9570d804` |
| `external-dib-maskless.ico` | 32bpp DIB, no AND | 32x32 | `2cae8d72a65bcac1` | `bf96162de1136c50fb4f413a4cfe9acad97f0f9850e07e04156b1a2b3161af9a` |

The fixture images were created for LiberSystem and are released under the
Unlicense with the rest of the project. ImageMagick and icoutils are host-side
independent interoperability tools only; no source or runtime dependency from
either project is included.
