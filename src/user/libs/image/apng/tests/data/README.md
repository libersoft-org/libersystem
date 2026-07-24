# APNG interoperability fixtures

The fixtures were assembled on 2026-07-17 with APNG Assembler 2.91 from three
deterministic 31x19 ImageMagick PNG32 frames. Both use a 3/50 second frame delay
and loop count 2; every container passes pngcheck 3.0.3.

```sh
apngasm external-animation.png frame0.png frame1.png frame2.png 3 50 -l2 -z0
apngasm external-separate-default.png frame0.png frame1.png frame2.png \
  3 50 -l2 -f -z0
```

The ordinary file has three animation frames (`fcTL` count 3). The `-f` file
uses frame0 only as the backward-compatible default IDAT image and has two
animation frames (`fcTL` count 2); LiberSystem must not expose the default image
as frame zero. APNG Disassembler 2.9 extracts the following full-canvas RGBA
buffers:

| Container/frame | RGBA FNV-1a |
| --- | --- |
| ordinary frame 1 | `5fada2efc37e917f` |
| ordinary frame 2 / separate frame 1 | `fa2fff147b885f15` |
| ordinary frame 3 / separate frame 2 | `566d1dacd369b6ab` |

Artifact SHA-256:

```text
20bbd03bc7b3d70ac2f4a349b61ef80a6f529cb156d32304797ad2a1286bee9a  external-animation.png
c738e4eb987d02352b98b497fc28a27720ea0ce8a6ddbdb51d046eca80e2e1b4  external-separate-default.png
```

The fixture images were created for LiberSystem and are released under the
Unlicense with the rest of the project. apngasm, apngdis, pngcheck and
ImageMagick are host-side independent interoperability tools only; no source or
runtime dependency from those projects is included.
