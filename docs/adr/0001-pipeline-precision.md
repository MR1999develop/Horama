# ADR 0001 — Precision of the image pipeline

- **Status:** proposed (to approve before histogram and export in the egui interface)
- **Date:** 2026-11-02

## Context

Today every image becomes 8-bit RGB right after decoding (`DecodedImage::rgb_pixels`,
`to_rgb8()` in `image_loader.rs`). Everything else works on those values:

- colour management (`qcms`, which accepts only 8-bit data) converts embedded profile →
  chosen profile → monitor;
- histogram, crop, rotation and export start from the same 8 bits;
- the pyramid and the thumbnails average the encoded sRGB values.

Consequences:

1. The 16 bits of TIFF, PNG and RAW (RAW goes through `imagepipe`, which works in `f32`) are
   lost before colour management. A conversion between wide spaces (ProPhoto → sRGB, Rec.2020 →
   Display P3) applied to already rounded data can produce *banding* in gradients.
2. TIFF/PNG export is always 8-bit, even from a 16-bit source.
3. The histogram shows already rounded data.
4. The rendering intent is fixed (Perceptual) and there is no black point compensation.

The next phases of the egui interface (histogram, export, profile choice) would be built on top
of this limitation: better to decide first.

## Options

| | Memory for a 45 MP image | Precision | Work |
|---|---|---|---|
| **A. Stay at 8 bit** | 135 MB | as today | none |
| **B. 16-bit document (`u16`), 8-bit screen** | 270 MB document + 135 MB screen | keeps the files' 16 bits; 16-bit conversions | medium |
| **C. Linear floating-point document (`f32`)** | 540 MB document + 135 MB screen | maximum, also for future editing | high |

For colour management at 16 bit or in floating point `qcms` is not enough. The alternatives:

- **`moxcms`** (pure Rust, ICC v2/v4, `u8`/`u16`/`f32` data, rendering intents and black point
  compensation). It is already used by the `image` crate for colour space conversion: no C
  library, build unchanged on Windows and Linux.
- **`lcms2`** (Little CMS, C library): the most proven, but it adds a C dependency to the build
  and the release.

## Proposed decision

**Option B with `moxcms`.**

- `DecodedImage` keeps the document's pixels at 16 bits per channel (`Vec<u16>`); 8-bit files
  are widened without loss (`v * 257`).
- The profile → monitor conversion produces 8-bit pixels only for the screen, with optional
  dithering; the document is never rounded to 8 bit.
- Rendering intent (Perceptual, Relative colorimetric with or without black point
  compensation, Saturation, Absolute colorimetric) exposed as a setting, Perceptual by default.
- Histogram computed on the document's data (16 bit, grouped into 256 bins).
- TIFF/PNG export at 16 bit when the source has more than 8 bits; JPEG and WebP stay at 8 bit
  (format limit), with quantisation done only at the output.
- Pyramid and thumbnails filtered in linear light (already so for the pyramid), starting from
  the 16 bits.

Option C is not needed while Horama is a viewer with geometric changes (crop, rotation): it
costs twice the memory of B without a visible advantage. It can move to `f32` if tonal
adjustments arrive.

## Consequences

- **Memory:** the RAM cache holds about twice the bytes per image; the budget
  (`CACHE_FREE_RAM_RATIO`) stays the same, so about half as many images are preloaded. The RAW
  limits (200 MP, free memory) must be recomputed with 6 bytes per pixel instead of 3.
- **Performance:** colour conversion and pyramid work on twice the data; the opening time of a
  45 MP RAW must be measured before and after (goal: less than 20% more).
- **Code:** `color_manager.rs` moves from `qcms` to `moxcms`; `image_loader.rs` uses
  `to_rgb16()`; `image_saver.rs` receives `u16` and writes 16-bit TIFF/PNG; the rotation and
  crop tests work on the same data.
- **Migration:** in steps, each in a branch with `check.bat` green:
  1. `moxcms` instead of `qcms` with the same data (8 bit), comparing the results on the test
     images;
  2. 16-bit document in the library, 8-bit screen;
  3. 16-bit histogram and export;
  4. rendering intent and black point compensation in the settings.

## To verify before approving

- That `moxcms` handles the custom and monitor profiles read today (v2 profiles with
  parametric curves and LUTs).
- Colour difference (ΔE) between `qcms` and `moxcms` on the test images: it must stay below 1
  with the same intent.
