# Horama

[![License: GPL-3.0-only](https://img.shields.io/badge/License-GPLv3-blue.svg)](https://www.gnu.org/licenses/gpl-3.0)
[![Rust](https://img.shields.io/badge/Rust-2024%20Edition-orange.svg)](https://www.rust-lang.org/)
[![egui](https://img.shields.io/badge/GUI-egui%200.37-purple.svg)](https://github.com/emilk/egui)
[![Platform](https://img.shields.io/badge/Platform-Windows%20%7C%20Linux-lightgrey.svg)]()

**Horama** (from the Greek ὅραμα, "what is seen") is a high-performance viewer for images and Camera RAW files written in **Rust** and **egui**, designed for photographers and imaging professionals, with advanced ICC colour profile management and a modern darkroom interface.

---

## Main features

- **Supported formats**:
  - **Standard raster**: JPEG, PNG, TIFF (8 and 16 bit), WebP, BMP, GIF, ICO, TGA.
  - **Camera RAW**: Canon (`.CR2`, `.CR3`), Nikon (`.NEF`), Sony (`.ARW`), Adobe (`.DNG`), Fujifilm (`.RAF`), Olympus (`.ORF`), Panasonic (`.RW2`), Pentax (`.PEF`) and others.
- **Navigation and performance**:
  - Sub-millisecond extraction of the EXIF IFD1 thumbnails embedded in RAW/JPEG files.
  - Predictive multi-gigabyte RAM cache with background decoding.
  - Expandable/collapsible folder tree that reopens the last subfolder at startup.
  - Horizontal filmstrip with thumbnails synchronised in real time, which also scrolls with the mouse wheel.
- **Editing and precision**:
  - Image rotation (90° clockwise, counter-clockwise, 180°) saved to disk automatically **without quality loss**:
    - **JPEG / TIFF**: only the EXIF/TIFF orientation tag (and XMP `tiff:Orientation`) is updated; the compressed data and all metadata stay identical byte for byte.
    - **PNG / WebP / BMP**: the pixels are re-encoded with lossless encoders (PNG keeps 16 bit and transparency; ICC profile, EXIF and texts are kept).
    - **RAW**: rotation is on screen only, RAW files are never modified.
    - Writes are atomic (temporary file + rename) and serialised; on exit the app waits for the writes in progress to finish.
  - Non-destructive crop in memory: explicit saving with *Save / Export* (or "save all" on exit, with sequential names `name_1.jpg`).
  - Zoom to 100% centred on the mouse cursor with a left double click, and *Fit* mode.
  - Round 6× magnifier (middle mouse button held) and RGB/HSL colour picker (right button held).
  - Single and multiple file renaming (F2) following the system's conventions: the original extension is always kept, no existing file is ever overwritten and on error all the renames are undone.
  - Deletion to the **Recycle Bin** (Windows: system Recycle Bin; macOS: Finder; Linux: freedesktop trash), without confirmation: a notice in the view offers "Undo" for a few seconds; drives without a Recycle Bin ask for confirmation before deleting permanently.
  - Opening an image from File Explorer ("Open with…") or by dragging it into the window: its folder opens, with the image selected and the folder highlighted in the tree on the left. Dragging several images opens only that selection; dragging a folder opens the folder.
  - Native *"Open with"* context menu with the applications registered in the operating system.
  - Real-time histogram (RGB and luminance) and complete EXIF photographic metadata.
  - Instant filters by text/partial name and star rating (★).
- **ICC colour management**:
  - `qcms` engine with support for embedded profiles, sRGB, Adobe RGB, ProPhoto RGB, Display P3, Rec.2020 and custom ICC/ICM profiles.
  - Images **with an embedded profile**: choosing another profile performs a *conversion* (embedded → chosen → monitor), like Photoshop's "Convert to Profile". The look on screen stays the same (it changes only if the chosen space is smaller and some colours must be clipped), while the pixel values, the histogram, the colour picker and the exported file change. Images **without a profile**: the chosen profile is *assigned* and the look changes.
  - Monitor profile: Windows (per monitor, updated when the window moves to another screen), Linux through `colord` (`colormgr`), macOS treated as sRGB.
  - Display pipeline at 8 bits per channel (16-bit files are read and shown at 8 bit; the lossless rotation of 16-bit PNGs keeps the original depth instead).
  - Export to JPEG, PNG, 8-bit TIFF, lossless WebP and BMP with the ICC profile and the original EXIF (orientation reset, dimensions updated). DNG export is not supported.
- **MCP (Model Context Protocol) integration**:
  - Native MCP server on `stdio` that lets **Claude** or **Gemini** control the application directly (opening folders, selecting images, rotation, zoom, filters and reading metadata).

---

## Quick start

### Prerequisites
- **Rust toolchain** 1.99 (the version is pinned in `rust-toolchain.toml`): [rustup.rs](https://rustup.rs/)
- **C compiler (MinGW-w64 on Windows, GCC/Clang on Linux)**

### Running on Windows
With the quick start script:
```bat
run_windows.bat
```
Or with Cargo:
```powershell
cargo run --release
```

### Starting as an MCP server (for Claude Desktop / Gemini)
Add the `--mcp` flag:
```powershell
horama.exe --mcp
```
*(See [`mcp_client_config.json`](mcp_client_config.json) for the configuration with Claude Desktop: replace `C:\\path\\to\\horama.exe` with the real path of the executable, e.g. `...\\target\\release\\horama.exe`).*

Instead of the flag you can set the environment variable `HORAMA_MCP`.

Available MCP tools: `open_folder`, `select_image`, `rotate_image` (lossless rotation as above), `set_zoom`, `set_rating` (0–5), `filter_images` (text, minimum rating from −1 = only unrated to 5, format `JPG`/`PNG`/`TIFF`/`WEBP`/`RAW`), `get_current_image_info`, `list_images`.

The server treats the client as untrusted: every call is checked (fields, values, lengths) before it reaches the interface, and an invalid call gets an error without effects. The only tool that modifies files is `rotate_image`, without quality loss; RAW files are never modified.

### Development
Before every merge into `main` a single command runs, the same one the CI runs:
```powershell
scripts\check.bat
```
(`scripts/check.sh` on Linux and macOS). It checks formatting, clippy, tests, dependencies and the release build. The dependency check (known vulnerabilities, licences compatible with GPL-3.0, sources) uses `cargo-deny`, to be installed once with `cargo install cargo-deny --locked`.

---

## Default keyboard shortcuts

All shortcuts can be customised in the application from the **`Help -> Keyboard Shortcuts...`** menu.

| Action | Default shortcut |
| :--- | :--- |
| **Next image** | `Right arrow` |
| **Previous image** | `Left arrow` |
| **Zoom in / Zoom out** | `+` (or `=`) / `-` (or `_`) |
| **Fit to window** | `Ctrl+0` (or `Space`) |
| **Zoom 100% (1 image pixel = 1 screen pixel)** | `Ctrl+1` (or *left double click*) |
| **Rotate clockwise (90°)** | `R` |
| **Rotate counter-clockwise (90°)** | `L` |
| **Rename file** | `F2` |
| **Delete selected** | `Delete` |
| **Refresh folder** | `F5` |
| **Open file(s)...** | `Ctrl+O` |
| **Open folder...** | `Ctrl+Shift+O` |
| **Save / Export...** | `Ctrl+S` |
| **Rating** | Number keys `0` - `5` |

---

## Interface

The interface is built on [egui](https://github.com/emilk/egui).

```
cargo run --release
```

What it does:

- opens a folder or a file (menu, recent folders, command line, dragging; dragging several images opens only those), with the folder tree (it reopens the last subfolder) and the filmstrip of thumbnails, search and multiple selection; the folder refreshes by itself when other programs add, delete or rename files, and the thumbnails of those files carry the New, Modified or Renamed badge (it also warns if the open image is deleted); the folder panel can be hidden from View > Folders;
- shows the image with colour management (monitor profile updated when the window moves to another screen), fit to window, 100%, zoom on the cursor, preset zoom levels (10%–800%) and panning; a double click goes to 100% at the clicked point and back to fit; beyond 100% pixels are shown sharp; round 6× magnifier (middle button) and RGB/HSL colour picker (right button);
- preloads the nearby images (the state of the RAM cache is in the status bar); one load at a time, so scrolling fast decodes only the image you stop on;
- ratings (0–5, stars in the status bar or on the thumbnails) and all the filters (stars, format, aspect ratio, colour profile, profile changed, cropped);
- information panel with histogram, colour management (embedded profile, profile in use, monitor) and metadata (I);
- choice and conversion of the colour profile (Image menu, P for the next one) and custom ICC/ICM profiles;
- crop (C) with a free aspect ratio or with those of the "Aspect ratio" filter (also custom ones, which are added from the crop bar, stay saved and are deleted with the X next to them in the aspect ratio menu), frame rotatable by 90° (X or the button in the bar), Undo/Redo (Ctrl+Z / Ctrl+Y) for crops, ratings, profiles and rotations, saving crops as new files next to the originals (Ctrl+S) or over the original with File > Overwrite the original (not for RAW files, always after a confirmation; JPEGs are compressed like the original) and a prompt on closing if unsaved crops remain;
- comparison of up to nine selected images, side by side;
- lossless rotation (R / L; RAW files and formats that cannot be rewritten without loss rotate on screen only), Recycle Bin without confirmation with Undo for 8 seconds (Del), renaming one or more files (F2), "Save a copy as" with the choice of JPEG quality (Ctrl+Shift+S), "Open with" from the right-click menu on the filmstrip;
- customisable shortcuts (View > Keyboard shortcuts), F6 to move from one panel to another;
- toolbar with zoom and colour profile lock;
- look in line with Windows 11: colours, Segoe UI and system icons; System / Light / Dark theme and language (English by default, Italian, German, French, Spanish, Chinese or the system's) in File > Preferences (Ctrl+,); Chinese uses a system font (Microsoft YaHei, PingFang, Noto Sans CJK); Help menu with shortcuts and information; names and states of the controls for screen readers;
- MCP server (`horama --mcp`).

Ratings are saved in the `ratings.jsonl` file, next to `settings.json` in the settings folder. On the first start, the ratings saved by earlier versions inside `settings.json` are moved there; a copy of the file as it was stays in `settings.json.before-ratings`.

## Log file

Horama writes a log file, useful to understand a problem also in the release version (which has no console):

- **Windows:** `%APPDATA%\Horama\logs\horama.log`
- **Linux:** `~/.config/Horama/logs/horama.log`
- **macOS:** `~/Library/Application Support/Horama/logs/horama.log`

The first line of every start reports version, commit and operating system. Beyond 1 MB the file is rotated (`horama.1.log`, `horama.2.log`, `horama.3.log`; the oldest are deleted). The default level is `info`; the environment variable `HORAMA_LOG` (`error`, `warn`, `info`, `debug`, `trace`) changes it. The log contains file paths and error causes, never the content of the images.

---

## Security

Vulnerabilities are reported privately, as described in [SECURITY.md](SECURITY.md). The threats considered and the accepted risks are in [docs/threat-model.md](docs/threat-model.md).

---

## Licence

Copyright (C) 2026 MR

Open-source project released under the **GNU General Public License v3.0 (GPL-3.0-only)**. See the [LICENSE](LICENSE) file for details.
