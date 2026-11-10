# Horama threat model

Updated with every change that moves a trust boundary: formats read, MCP server, programs
started, files written, dependencies and release.

## What we are building

A desktop viewer for images and RAW files (Windows and Ubuntu), run as a normal user, without
network. It reads folders and files chosen by the user and writes only:

- the files the user rotates or saves (lossless rotation, "Save a copy", "Overwrite the
  original" only for non-RAW formats);
- settings, ratings and logs in the user's configuration folder;
- the system Recycle Bin.

## Assets

| Size | Asset |
|---|---|
| L | The user's photos, RAW files included (never modified), and their ratings |
| L | The integrity of the build and the releases (whoever downloads `horama.exe`) |
| M | Availability of the app (a file must not close it or hang it) |
| S | Settings, logs (they contain file paths, nothing else) |

## Trust boundaries

```mermaid
flowchart LR
    F[Image and RAW files<br/>from anyone] -->|decoders| H(Horama)
    M[MCP client<br/>driven by a model] -->|JSON-RPC on stdin| H
    A[Command line,<br/>drag and drop] -->|paths| H
    H -->|start by absolute path| P[explorer.exe, rundll32.exe,<br/>"Open with" apps, lp, lpstat]
    H -->|atomic write| D[(User's files,<br/>settings)]
    S[Dependencies, CI] -->|build| H
```

## Threats and responses

| Element | Threat | Response |
|---|---|---|
| Image file | Decoder panicking on a malformed file | Decoders in Rust, `panic = "unwind"` and `catch_unwind` around every decode: the file gives an error, the app stays open; every panic is written to the log |
| Image file | Memory exhausted (huge file, image with huge declared dimensions) | Files read whole only up to 1 GiB, checked on the open file; decoder memory limit sized on each image (two copies of its pixels) and checked against the free memory before decoding, images above 200 MP refused; RAW files developed only below 200 MP and if free memory is enough |
| RAW file | Content that is not a RAW (damaged file) | Recognised by its signature and reported as damaged; for thumbnails no preview is even searched in it |
| MCP client | Malformed or hostile calls decided by a model | Every call becomes a typed command before reaching the interface: unknown fields, out-of-range values and texts beyond the limits are rejected; lines over 1 MiB discarded; errors repeat at most 200 characters of the input |
| MCP client | Modifying files | Only `rotate_image` writes, losslessly and with the same commands as the interface; RAW files are never modified |
| MCP client | File data read as instructions by the model | Metadata come back as JSON values, declared as data in the tool's description |
| External programs | A program with the same name run instead of the system's | Start by absolute path (Windows folder, `/usr/bin`), separate arguments, no shell |
| Printing | Option injected through a printer name, print file read or replaced by others | The printer must be one `lpstat` listed and cannot start with `-`; `lp` gets separate arguments with `--` before the file; print files in the user's own cache folder, new names, removed after a day |
| Writes | File truncated by a crash | Temporary file in the same folder, `sync_all`, then rename |
| Dependencies and CI | Vulnerable crate, incompatible licence, unexpected source, tampered action | `cargo deny` in every check and every day in CI; `Cargo.lock` and `--locked`; actions pinned to a SHA; read-only CI token; Dependabot with a wait on new versions |

## Accepted risks

- **Decoders in the same process**, not isolated: they are in Rust (memory-safe except for the
  dependencies with `unsafe`). To revisit if a decoder written in C is added.
- **The MCP server sees everything the user sees**: it can open every readable folder. The MCP
  client is the program that starts Horama, so it already acts as the user. To revisit if the
  server listened on a socket or on the network: authentication and an `Origin` check would be
  needed.
- **"Open with" on Windows** calls COM directly (`unsafe`, justified block by block). To revisit
  if the `windows` crate is adopted.
- **Releases without Authenticode signing** or signed `SHA256SUMS`: to add before distributing
  the app outside GitHub.

## Out of scope

An attacker who already controls the user's account or the machine.
