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

The independent static corpus covers 4-bit grayscale, indexed color with `tRNS`,
16-bit RGBA, Adam7 RGB and a three-chunk consecutive-IDAT derivative preserving
the exact compressed stream. ImageMagick and Pillow produce exact RGBA8; every
file passes pngcheck 3.0.3. APNG Assembler 2.91 supplies ordinary three-frame and
`-f` separate-default-image files; APNG Disassembler 2.9 confirms full-canvas
pixels, 60 ms delays and loop count 2. The reciprocal `just png-conformance`
gate requires pngcheck-clean compression-0/100 PNG with exact ImageMagick/Pillow
pixels and APNG output with exact apngdis frames/timing. Status: **Verified
profile** for the listed static PNG profiles and both normative APNG layouts.

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
the independent gifsicle 1.96 corpus now also covers interlaced full/positioned
frames, a local color table, disposal 1/2/3, zero and nonzero delays, loop count
and image-data sub-blocks as short as one byte. LiberSystem and
`gifsicle --unoptimize` produce identical displayed canvases. ImageMagick differs
only after disposal 2 by clearing to transparent rather than the logical-screen
background, a documented implementation divergence. The reciprocal
`just gif-conformance` gate validates our timing/disposal/loop metadata with
gifsicle and all composited pixels through gifsicle plus ImageMagick. Status:
**Verified profile** for the implemented GIF87a/GIF89a animation subset.

### JPEG/JFIF

The decoder and encoder intentionally implement 8-bit baseline sequential DCT
with Huffman coding and grayscale or YCbCr components. Progressive, arithmetic,
hierarchical, lossless, 12-bit and CMYK/YCCK JPEG are typed `Unsupported`; they
must never be partially rendered as baseline. Output is JFIF-compatible baseline
JPEG and has no alpha. Status: **Subset**. Independent baseline output validation
now covers ImageMagick SOF0 grayscale and three-component YCbCr plus a real SOF2
progressive rejection fixture. ImageMagick and Pillow produce identical canonical
RGBA; LiberSystem is exact for grayscale and stays within max 2 / mean 0.232 byte
error for the independent YCbCr IDCT/chroma path. The reciprocal
`just jpeg-conformance` gate requires three-component SOF0/JFIF output, exact
ImageMagick/Pillow agreement, deterministic quality-10/100 artifact hashes and a
quality-100 RGB MSE floor. Status: **Verified profile** for 8-bit baseline
grayscale/YCbCr input and baseline RGB-derived output; progressive remains a typed
**Subset**.

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
**Verified profile** for these mask layouts. The independent corpus adds direct
ImageMagick 24bpp `BI_RGB` and V5 RGBA-mask files, direct Netpbm indexed 8bpp
`BI_RGB`, and header-only V3/V4 derivatives retaining the V5 masks and pixels.
ImageMagick and Pillow agree exactly on V3/V4/V5 alpha. A derived 32bpp `BI_RGB`
file deliberately carries non-opaque high bytes: Pillow and Netpbm ignore them
and match LiberSystem/Microsoft semantics, while ImageMagick interprets them as
alpha, a documented implementation divergence. Complete RGBA buffers are pinned
by FNV-1a. The reciprocal `just bmp-conformance` gate requires exact ImageMagick
and Netpbm pixels for LiberSystem 24bpp and indexed 8bpp output.

### ICO

The ICO leaf accepts icon directories with bounded, nonempty, nonoverlapping entries
backed by PNG or the selected 32bpp DIB profile, and selects the largest supported
image. For 32bpp DIB entries, the XOR bitmap's BGRA alpha is authoritative for every
pixel: the legacy 1-bit AND mask is ignored even when all XOR alpha bytes are zero,
and may be absent. This matches ImageMagick 7.1.1-43 and the deployed image-rs/Pillow/
Wine convention. The independent ImageMagick corpus covers PNG-backed, ordinary 32bpp
DIB and all-zero XOR-alpha DIB entries. ImageMagick and icoutils 0.32.3 produce exact
RGBA for every standard fixture; a maskless derivative preserves the XOR bytes and is
accepted by ImageMagick while strict icoutils rejects its missing AND bitmap. Complete
decoded buffers are pinned by FNV-1a. The reciprocal `just ico-conformance` gate
requires exact ImageMagick/icoutils pixels for LiberSystem PNG-backed 32/256 output.
Lower-depth DIB entries that require the AND mask, CUR hotspots and cursor output are
intentional **Subsets**. Directory zero-as-256 dimensions, doubled DIB height and
payload range validation are covered; zero-sized or overlapping entries are invalid.
Status: **Verified profile** for PNG and standard 32bpp DIB icon entries; maskless
32bpp DIB remains a documented deployed-convention subset.

### ICNS

The leaf accepts modern PNG-backed icon types and classic
`is32/il32/ih32/it32` component-RLE images paired with
`s8mk/l8mk/h8mk/t8mk` alpha masks. It emits classic 16/32/48-pixel entries and
PNG-backed entries from 128 pixels upward. JPEG 2000 payloads are typed
`Unsupported`. The independent `icnsutils 0.8.1.83.g921f972` corpus contains
classic `is32+s8mk`, `il32+l8mk`, `ih32+h8mk` and `it32+t8mk`, plus PNG-backed
`ic07`, generated from deterministic ImageMagick RGBA gradients. `png2icns`
directly emits the 16/32/48 and modern 128 profiles; a reproducible host-only
helper requests the legacy 128 types through the public libicns API. Complete
decoded pixel buffers are pinned by FNV-1a. Classic entries round-trip through
`icns2png`, while the modern embedded PNG is validated directly with ImageMagick
because this `icns2png` version drops alpha during `ic07` export. The reciprocal
`just icns-conformance` gate externally validates LiberSystem's classic 16/32/48
and modern 128 output, then compares independent 48 and legacy 128 decoding in
both implementations. Status: **Verified profile** for every supported classic
entry and modern `ic07`, under a **Source uncertain** format family. An
Apple-generated fixture remains open as provenance strengthening, not a known
codec behavior gap.

### PCX

The selected profile accepts and emits ZSoft version-5, 8-bit-per-plane RLE:
one indexed plane with trailing 256-color palette or three RGB planes. Earlier
1/2/4-bit and monochrome variants are **Subsets**. Output is opaque because these
profiles define no alpha. The central sniffer now requires the 128-byte header,
manufacturer, version 5, RLE marker, 8-bit samples, ordered bounds, one or three
planes and a sufficient bytes-per-line value before claiming PCX. Older versions
and other depths return typed `Unsupported`. A legal TGA with image-ID length
`0x0a` proves that the old first-byte collision is closed.

The independent ImageMagick 7.1.1-43 corpus contains a 17x9 indexed one-plane
file with trailing 256-color palette and a 19x7 RGB three-plane file. Both use
odd `bytes_per_line` equal to width, demonstrating that readers must honor the
declared stride rather than impose common even-padding advice. ImageMagick and
Netpbm `pcxtoppm` produce byte-identical RGBA results; the leaf pins complete
buffers with FNV-1a. The reciprocal `just pcx-conformance` gate encodes both
profiles with LiberSystem and requires exact pixels from both independent
decoders. Status: **Verified profile** for version-5 indexed and RGB RLE.

### PPM/PNM

The leaf accepts one P3 plain or P6 raw RGB image and emits the conservative P6
subset with `Maxval=255`. PBM, PGM, PAM, concatenated images and 16-bit PPM output
are **Subsets**. Input scaling and comments must follow the current Netpbm PPM
description; alpha input is rejected for output rather than silently composited.
The independent Netpbm 11.10.2 corpus covers commented P3 with `Maxval=31` and
P6 with `Maxval=65535` two-byte big-endian samples. Complete RGBA buffers are
pinned by FNV-1a. Netpbm nearest-rounding matches the decoder for low-Maxval P3;
ImageMagick 7.1.1-43 truncates some converted 8-bit samples by one, a documented
consumer quantization difference. Both agree exactly on the 16-bit P6 fixture.
The reciprocal `just ppm-conformance` gate requires exact LiberSystem P6/255
output from both implementations. Status: **Verified profile** for selected P3
and P6 input plus conservative P6/255 output.

### QOI

The leaf accepts and emits QOI 1.0 with three or four channels, validates the
header, dimensions, run/index/diff/luma/RGB/RGBA opcodes, exact pixel count and
eight-byte end marker, and preserves RGBA8 exactly. The colorspace byte is a
declarative hint and does not alter samples. Opaque output uses the three-channel
profile; an image with any non-opaque pixel uses four channels. The independent
Netpbm 11.10.2 corpus covers both channel counts and all six opcode families;
ImageMagick 7.1.1-43 and Netpbm `qoitopam` produce byte-identical RGBA, pinned by
whole-buffer FNV-1a. The reciprocal `just qoi-conformance` gate requires exact
pixels from both decoders for LiberSystem RGB and RGBA output. Status: **Verified
profile** for QOI 1.0 RGB/RGBA input and output.

### TGA

The selected profile accepts true-color image types 2 and 10 at 24 or 32 bits,
both image origins, and emits type-10 RLE with top-left origin. Color-mapped,
grayscale, 15/16-bit, extension-area and developer-area profiles are **Subsets**.
The central probe now validates the 18-byte header, no-color-map selected profile,
type, nonzero geometry, depth, reserved descriptor bits and bounded image-ID start.
The PCX/TGA image-ID collision is covered through the public `decode_frame` path.

The independent ImageMagick 7.1.1-43 corpus contains raw and RLE 24/32-bit
true-color files spanning all four top/bottom and left/right origin combinations.
An asymmetric alpha-bearing source makes every orientation observable, and one
bottom-right raw 32-bit fixture carries a 22-byte image-ID payload. Complete
canonical RGBA buffers are pinned by FNV-1a after independent `-auto-orient`
decoding. The reciprocal `just tga-conformance` gate encodes raw and RLE 24/32-bit
profiles with LiberSystem and requires exact pixels from ImageMagick. Status:
**Verified profile** for the selected true-color subset.

### WebP, VP8 and VP8L

The container parser covers simple `VP8 `, simple `VP8L`, extended `VP8X`, raw
or VP8L-compressed `ALPH`, and `ANIM`/`ANMF` frame rectangles, duration, blend and
background disposal. Static lossless output is VP8L. Static lossy output is a
native RFC 6386 keyframe in a simple `VP8 ` container, or `VP8X + ALPH + VP8 `
when alpha is present. Lossless animation emits full-canvas VP8L `ANMF` frames;
lossy animation is a typed **Subset**.

The native VP8 encoder's boolean coding, prediction, transform, quantization and
coefficient tokens are covered by independent decode and fidelity tests. The
libwebp 1.5.0 corpus covers simple VP8, extended `VP8X+ALPH+VP8`, simple VP8L and
`VP8X+ANIM+ANMF`; `dwebp` and ImageMagick agree exactly on every static RGBA
buffer. Animation preserves the
`ANIM` BGRA background as RGBA and raw 24-bit frame durations including zero. The
shared compositor initializes and disposes frame rectangles to that background;
preview and static-frame extraction use the same path. APNG destinations with no
equivalent represented background, and GIF destinations with unrepresentable partial
background alpha, are canonicalized to displayed full-canvas frames so cross-format
conversion preserves visuals and timing. Representable GIF backgrounds remain exact.
The parser requires
`VP8X` first, `ANIM` before `ANMF`, reconstruction chunks before trailing metadata,
zero RIFF padding and zero reserved bits in `VP8X`/`ANMF`. libwebp `anim_dump`
clears background-disposed rectangles to transparent black and ImageMagick keeps
alpha-zero source RGB, while LiberSystem uses the declared ANIM background; raw
ANMF frames and metadata agree and this viewer-policy divergence is documented.
The reciprocal `just webp-conformance` gate validates our VP8 quality endpoints,
VP8L, ALPH and canonical full-canvas animation through webpinfo, dwebp, anim_dump
and ImageMagick. Status: **Verified profile** for the implemented static and
animation subsets.

## Closure order

1. Add independent corpora for the remaining verified/subset claims, prioritizing
  formats still covered only by self-round-trip and adding an Apple-generated ICNS
  fixture when one is available.

No format moves from **Gap** or **Source uncertain** to **Verified** without an
independently sourced fixture or a structural test that directly exercises the cited
requirement.
