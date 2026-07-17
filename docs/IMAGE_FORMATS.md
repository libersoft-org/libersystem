# Image format conformance

This document is the maintained specification audit for the image codecs used by
`imgconv` and `imgview`. It describes the profiles LiberSystem intentionally accepts
and emits; it does not claim that a format leaf is a universal decoder merely because
it recognizes the format.

Audit statuses:

- **Verified**: the listed profile has been compared with the cited primary
  specification and has direct tests for its normative structure.
- **Gap**: accepted or emitted behavior conflicts with, or does not yet cover, a
  requirement of the profile LiberSystem claims to support.
- **Subset**: the omitted profile is valid but deliberately returns typed
  `Unsupported`; this is not malformed-input handling.
- **Source uncertain**: no maintained public normative specification exists. The
  cited source is the best surviving vendor material, and interoperability corpus
  behavior is part of the contract.

The audit was last checked on 2026-07-17. Metadata is deliberately stripped by
`imgconv` version one. A decoder may ignore optional metadata where the format permits
that, but it must still validate structural lengths, ordering and checksums required to
locate image data safely.

## Source registry

| Format | Authority and revision | Audit source | Source status |
| --- | --- | --- | --- |
| PNG and APNG | W3C, *Portable Network Graphics (PNG) Specification (Third Edition)*, Recommendation 24 June 2025 | https://www.w3.org/TR/2025/REC-png-3-20250624/ | Current normative source; APNG, `image/apng`, `acTL`, `fcTL` and `fdAT` are included in this edition. |
| GIF | CompuServe, *Graphics Interchange Format Version 89a*, 31 July 1990 | https://www.w3.org/Graphics/GIF/spec-gif89a.txt | Final vendor specification, preserved by W3C. |
| JPEG | ITU-T T.81 (09/1992) / ISO/IEC 10918-1 and JFIF 1.02, September 1992 | https://www.w3.org/Graphics/JPEG/itu-t81.pdf and https://www.w3.org/Graphics/JPEG/jfif3.pdf | Normative coding recommendation plus the interchange wrapper emitted by LiberSystem. |
| BMP/DIB | Microsoft Win32 GDI bitmap storage and header definitions | https://learn.microsoft.com/en-us/windows/win32/gdi/bitmap-storage | Maintained vendor documentation; the format is split across structure pages rather than one versioned standard. |
| ICO/CUR | Microsoft Win32, *About Icons* and icon resource documentation | https://learn.microsoft.com/en-us/windows/win32/menurc/about-icons | **Source uncertain**: Microsoft documents icon resources and API behavior but publishes no complete versioned ICO/CUR byte-stream specification. CUR is in the format family but is not currently a LiberSystem input or output. |
| ICNS | Apple Icon Services constants and archived high-resolution icon guidance | https://developer.apple.com/library/archive/documentation/GraphicsAnimation/Conceptual/HighResolutionOSX/Optimizing/Optimizing.html | **Source uncertain**: Apple documents icon representations and type codes but publishes no complete ICNS byte-stream specification. |
| PCX | ZSoft, *PCX Technical Reference Manual*, revision 5 lineage | https://www.fileformat.info/format/pcx/egff.htm | **Source uncertain**: the vendor is defunct and no stable vendor-hosted copy remains; surviving technical references and independent files define interoperability. |
| PPM/PNM | Netpbm project, current PPM format description | https://netpbm.sourceforge.net/doc/ppm.html | Maintained reference implementation documentation; conventional rather than an ISO standard. |
| QOI | Dominic Szablewski, *The Quite OK Image Format Specification*, version 1.0, 24 December 2021 | https://qoiformat.org/qoi-specification.pdf | Author-maintained normative specification. |
| TGA | Truevision, *TGA File Format Specification 2.0*, Technical Guide 2.2, January 1991 | https://www.loc.gov/preservation/digital/formats/fdd/fdd000180.shtml and https://www.fileformat.info/format/tga/egff.htm | **Source uncertain** only in hosting: the publisher is defunct; the Library of Congress format record and surviving technical description identify the primary document. |
| WebP container | Google, *WebP Container Specification*, checked 17 July 2026 | https://developers.google.com/speed/webp/docs/riff_container | Maintained vendor specification for RIFF, `VP8X`, `ALPH`, `ANIM` and `ANMF`. |
| VP8 | IETF RFC 6386, *VP8 Data Format and Decoding Guide*, November 2011 | https://www.rfc-editor.org/rfc/rfc6386 | Published bitstream specification used by the native LiberSystem keyframe encoder. |
| VP8L | Google, *Specification for WebP Lossless Bitstream*, 9 March 2023 | https://developers.google.com/speed/webp/docs/webp_lossless_bitstream_specification | Maintained vendor bitstream specification. |

## Profile audit

### PNG and APNG

Static PNG decoding accepts color types 0, 2, 3, 4 and 6 at their legal bit
depths, both non-interlaced and Adam7 data, all five method-0 row filters, `PLTE`
and `tRNS`, and converts straight alpha to RGBA8. Static output is non-interlaced
RGBA8 or indexed color at 1/2/4/8 bits with optional binary `tRNS`. Using a
narrower legal output profile is conforming.

APNG output intentionally uses non-interlaced 8-bit color type 6. Decode inherits
the complete static PNG sample pipeline by reconstructing each bounded frame with
the stream's `IHDR`, `PLTE` and `tRNS`; indexed, grayscale, RGB, alpha, 16-bit and
Adam7 profiles therefore use the same validation and RGBA8 conversion as static
PNG. It validates CRCs, shared `fcTL`/`fdAT` sequence numbers, multiple consecutive
`IDAT` chunks, frame rectangles, loop count, blend and all three disposal
operations. Both normative layouts are accepted: the static image may be the first
animation frame or may be separate from the animation. Indexed multi-`IDAT` and
static-image-not-a-frame regressions directly pin these behaviors.

The central APNG sniffer walks bounded PNG chunks and recognizes `acTL` only
before the first `IDAT`; bytes inside compressed image data cannot reclassify a
static PNG. A regression fixture embeds the literal bytes `acTL` in a static
pixel payload and remains classified as PNG. Corrupt recognized APNG versus
corrupt static PNG still needs explicit error-classification coverage.

### GIF89a

The leaf accepts GIF87a/GIF89a image streams with global/local palettes, LZW,
interlacing, transparency, frame rectangles, delay and disposal. Output is GIF89a
with a deterministic global palette, LZW image data, graphic controls and the
Netscape loop extension. Plain Text, Comment and unknown Application extensions
are non-rendering data and may be ignored safely; malformed block sizes and
reserved values remain errors. Zero delay is preserved. The logical-screen
background index selects an exact Global Color Table RGB value. Following the
deployed ImageMagick/browser convention, it is transparent only when the first
frame's Graphic Control Extension names that same index as transparent; later
frame transparency does not alter the global background. The shared compositor
uses that RGBA value for initial canvas and disposal method 2. Encoding reserves
an exact background palette entry, writes the matching logical-screen index and,
for transparent background, makes the first frame use that transparent index.
Opaque and transparent-colored backgrounds round-trip exactly; partial background
alpha is typed `Unsupported`. An ImageMagick 7.1.1-43 fixture pins initial and
disposed canvases. Status: **Verified profile** for background/transparency;
deferred-clear and unusual sub-block boundaries still need independent coverage.

### JPEG/JFIF

The decoder and encoder intentionally implement 8-bit baseline sequential DCT
with Huffman coding and grayscale or YCbCr components. Progressive, arithmetic,
hierarchical, lossless, 12-bit and CMYK/YCCK JPEG are typed `Unsupported`; they
must never be partially rendered as baseline. Output is JFIF-compatible baseline
JPEG and has no alpha. Status: **Subset**. Independent baseline output validation
and explicit progressive rejection fixtures remain required.

### BMP/DIB

LiberSystem accepts the Windows BMP file header and the implemented indexed and
direct-color DIB profiles, including legal row padding and top-down/bottom-up
orientation. Output is bottom-up 24-bit `BI_RGB` or 8-bit indexed `BI_RGB` with
`RGBQUAD` palette. Those emitted profiles are **Verified** against Microsoft GDI
layout. OS/2 headers, embedded JPEG/PNG, color-management payloads and unimplemented
compression modes are **Subsets** and must return `Unsupported` after a structural
BMP match. For 16/32bpp `BI_BITFIELDS` and `BI_ALPHABITFIELDS`, RGB masks must be
nonzero, contiguous, disjoint and inside the declared pixel width. A nonzero fourth
mask in a 56-byte-or-larger V3/V4/V5 header, or the required external fourth mask
for `BI_ALPHABITFIELDS`, supplies straight alpha to RGBA decode. In contrast,
Microsoft defines the high byte of 32bpp `BI_RGB` as unused, so it remains opaque.
The legacy `decode` API remains BGRX while `decode_rgba` exposes explicit masked
alpha. Embedded V4 and external four-mask regressions pin alpha 0/128/255. Status:
**Verified profile** for these mask layouts.

### ICO

The ICO leaf accepts icon directories with bounded, nonempty, nonoverlapping entries
backed by PNG or the selected 32bpp DIB profile, and selects the largest supported
image. For 32bpp DIB entries, the XOR bitmap's BGRA alpha is authoritative for every
pixel: the legacy 1-bit AND mask is ignored even when all XOR alpha bytes are zero,
and may be absent. This matches ImageMagick 7.1.1-43 and the deployed image-rs/Pillow/
Wine convention. Synthetic conflict, all-zero-alpha and maskless fixtures pin the
same RGBA results. Lower-depth DIB entries that require the AND mask, CUR hotspots and
cursor output are intentional **Subsets**. Output uses PNG-backed entries. Directory
zero-as-256 dimensions, doubled DIB height and payload range validation are covered;
zero-sized or overlapping entries are invalid. Status: **Verified profile** for PNG
and 32bpp DIB icon entries.

### ICNS

The leaf accepts modern PNG-backed icon types and classic
`is32/il32/ih32/it32` component-RLE images paired with
`s8mk/l8mk/h8mk/t8mk` alpha masks. It emits classic 16/32/48-pixel entries and
PNG-backed entries from 128 pixels upward. JPEG 2000 payloads are typed
`Unsupported`. Status: **Source uncertain**; the contract must therefore be
anchored by Apple-generated and independently decoded fixtures for every type code,
not by self-round-trip alone.

### PCX

The selected profile accepts and emits ZSoft version-5, 8-bit-per-plane RLE:
one indexed plane with trailing 256-color palette or three RGB planes. Earlier
1/2/4-bit and monochrome variants are **Subsets**. Output is opaque because these
profiles define no alpha. The central sniffer now requires the 128-byte header,
manufacturer and RLE marker, 8-bit samples, ordered bounds, one or three planes
and a sufficient bytes-per-line value before claiming PCX. A legal TGA with
image-ID length `0x0a` proves that the old first-byte collision is closed. PCX
version handling remains a leaf-profile audit item.

### PPM/PNM

The leaf accepts one P3 plain or P6 raw RGB image and emits the conservative P6
subset with `Maxval=255`. PBM, PGM, PAM, concatenated images and 16-bit PPM output
are **Subsets**. Input scaling and comments must follow the current Netpbm PPM
description; alpha input is rejected for output rather than silently composited.

### QOI

The leaf accepts and emits QOI 1.0 with three or four channels, validates the
header, dimensions, run/index/diff/luma/RGB/RGBA opcodes, exact pixel count and
eight-byte end marker, and preserves RGBA8 exactly. The colorspace byte is a
declarative hint and does not alter samples. Status: **Verified profile**, pending
independent decoder coverage for both channel counts and opcode families.

### TGA

The selected profile accepts true-color image types 2 and 10 at 24 or 32 bits,
both image origins, and emits type-10 RLE with top-left origin. Color-mapped,
grayscale, 15/16-bit, extension-area and developer-area profiles are **Subsets**.
The central probe now validates the 18-byte header, no-color-map selected profile,
type, nonzero geometry, depth, reserved descriptor bits and bounded image-ID start.
The PCX/TGA image-ID collision is covered through the public `decode_frame` path.

### WebP, VP8 and VP8L

The container parser covers simple `VP8 `, simple `VP8L`, extended `VP8X`, raw
or VP8L-compressed `ALPH`, and `ANIM`/`ANMF` frame rectangles, duration, blend and
background disposal. Static lossless output is VP8L. Static lossy output is a
native RFC 6386 keyframe in a simple `VP8 ` container, or `VP8X + ALPH + VP8 `
when alpha is present. Lossless animation emits full-canvas VP8L `ANMF` frames;
lossy animation is a typed **Subset**.

The native VP8 encoder's boolean coding, prediction, transform, quantization and
coefficient tokens are covered by independent decode and fidelity tests. VP8L
encoding uses the local no_std dependency implementation and must remain covered by
external decoding rather than only its paired decoder. Animation preserves the
`ANIM` BGRA background as RGBA and raw 24-bit frame durations including zero. The
shared compositor initializes and disposes frame rectangles to that background;
preview and static-frame extraction use the same path. APNG destinations with no
equivalent represented background, and GIF destinations with unrepresentable partial
background alpha, are canonicalized to displayed full-canvas frames so cross-format
conversion preserves visuals and timing. Representable GIF backgrounds remain exact.
The parser requires
`VP8X` first, `ANIM` before `ANMF`, reconstruction chunks before trailing metadata,
zero RIFF padding and zero reserved bits in `VP8X`/`ANMF`. Status: **Verified
profile** for the implemented animation subset; independent external decoding remains
required for the corpus gate.

## Closure order

1. Add independent corpora for the remaining verified/subset claims, prioritizing
   ICNS, PCX and TGA where the primary-source chain is weakest.

No format moves from **Gap** or **Source uncertain** to **Verified** without an
independently sourced fixture or a structural test that directly exercises the cited
requirement.
