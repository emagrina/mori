# Mori security model

> This describes how Mori is designed to limit the impact of malicious files. It is defence in depth, not a guarantee: decoders, the operating system and the system web view can all have bugs.

Mori assumes every file on the drive may be malicious: malformed or oversized images, crafted video containers, hostile PDFs/SVG/HTML, disguised extensions, symlink tricks and spoofed filenames.

**Untrusted bytes are decoded outside the main process.**
- Image decoding happens in a separate, short-lived worker process, one per job (`Mori --mori-worker <op> <size>`, fixed arguments, no shell). Before reading any input, the worker:
  - drops privileges with the macOS `pure-computation` sandbox: no filesystem, no network, no new processes;
  - sets resource limits: CPU time, no file creation, few file descriptors, no core dumps;
  - on Windows is placed in a Job object (1 GB memory cap, a single process, no UI/clipboard access, killed with Mori).
- The worker never receives a path. Mori reads the file and pipes the bytes in, and the worker writes back a freshly encoded JPEG/PNG/GIF behind a tiny fixed binary header, which Mori validates. A test (`cargo test --test worker`) proves that inside the worker, reading files, writing files, network access and spawning processes all fail.
- Decoding uses pure-Rust, memory-safe decoders (`image` crate: JPEG, PNG, WebP, GIF only; no C codecs, no Quick Look, no FFmpeg). The format is taken from magic bytes. Decompression bombs are refused before allocation: at most 20,000 px per side, 100 megapixels, 512 MB decode budget, a 160 MB input file, and 600 GIF frames.
- Each job runs under a wall-clock timeout (12 s for thumbnails, 25 s for previews), and at most 3 workers run at once. A crash, hang or limit hit ends only that worker. Mori shows *Preview unavailable — file could not be safely processed.* and records the failure so it isn't retried.
- Videos are probed before the player ever sees them. The sandboxed worker reads just the container structure (the MP4/MOV `moov` box, located by walking top-level box headers even when it sits at the end of a huge file, or the head of a WebM file). That gives codecs, dimensions and duration, and any inconsistent structure is reported as damaged. Only codecs measured to work in this platform's webview are streamed (macOS: H.264, HEVC `hvc1`, MPEG-4 Part 2, MJPEG, ProRes, VP8, VP9; Windows: H.264, VP8/VP9, AV1). Everything else shows *Video format detected, but this codec is not supported for secure preview.* This matters: on Apple silicon without AV1 hardware, an AV1 WebM wedges WebKit's content process.
- Playable videos stream in ≤4 MB byte ranges straight from disk (a video is never loaded into memory) to the system webview's player, which decodes in the OS's own sandboxed media processes. Video thumbnails are single frames grabbed in the background, one at a time, never while a preview is open, and re-encoded by the worker.
- If decoding still goes wrong, it's contained in layers:
  - The player has a 20 s load timeout and a 25 s stall timeout. Errors, timeouts, or a video with no picture show *This video could not be safely previewed* with the reason.
  - Only one video session exists at a time. Switching or closing pauses the video, detaches its source, releases the decoder and ignores late results.
  - The UI pings Rust every second. If the pings stop for 10 s while Mori is in front, Rust terminates the wedged WebKit content process and reloads the UI in place.
  - If WebKit's content process crashes, Mori reloads it.
  - In both of those cases, the video that was playing (or had just been closed) is added to a permanent blocklist, and Mori tells you which file it was.
- A failing background thread can't take the app down: release builds unwind instead of aborting, every background thread is isolated with `catch_unwind`, and locks tolerate a failed holder.

**The UI has no filesystem access.**
- There's no asset protocol and no fs, shell, http or opener plugin. The UI is granted only Mori's own commands, each listed in `src-tauri/build.rs` and allowed individually in `capabilities/default.json`.
- The UI works with opaque file IDs, never paths. Every access goes through Rust, which:
  - looks the ID up in the index;
  - rejects `..` and absolute components;
  - canonicalizes the path and verifies it is still inside the canonical root, which defeats symlink and junction escapes;
  - opens the file with `O_NOFOLLOW | O_NONBLOCK`;
  - refuses anything that isn't a regular file.
- File bytes reach the webview only through Mori's `mori://` protocol: worker-made thumbnails and previews, plus verified video ranges. Responses carry `nosniff` and a sandboxing CSP.
- The folder picker runs on the Rust side, and settings are a fixed, validated set of fields, so the UI can't redirect Mori to another directory.

**Nothing is executed and nothing reaches the network.**
- The webview CSP is `default-src 'none'`, plus scripts and styles from the bundle, images and media from `mori:` only, and IPC. Prototypes are frozen, and navigation away from the bundled UI is blocked.
- There's no `eval`, no `innerHTML`, and no file content inserted as markup. Names, paths, text and metadata are rendered as plain text, so URLs inside files are never clickable. Filenames have control and bidi-override characters replaced, so `photo\u202Egpj.exe` can't pose as a `.jpg`.
- HTML, SVG and PDF are never rendered inside Mori.
- *Open in system* runs only when you click it, and only for passive formats (photos, video, audio, PDF, text, Office documents). It is refused for anything whose content is executable (Mach-O, PE, ELF, `#!` scripts) or contradicts its extension. Mori launches the OS opener directly (`/usr/bin/open`, `explorer.exe`) with an argument array and never uses a shell.
- The scanner never follows symlinks and never enters other mounted filesystems. It indexes only regular files and folders (no devices, FIFOs or sockets), stops 64 levels deep, and skips names that aren't valid Unicode.

**Mori is quiet and conservative about changes.** Browsing, previews and duplicate analysis open files for reading only. Release builds log nothing. Search history is not stored.

**File changes** are limited to two operations, each triggered only by an explicit user action and implemented in `src-tauri/src/fileops.rs`:

- **Rename.** The new name is validated: no separators, `..`, leading dots, control or bidi characters, reserved Windows names, or trailing dots/spaces. The rename is atomic and never replaces an existing item (`renamex_np(RENAME_EXCL)` / `renameat2(RENAME_NOREPLACE)` / `MoveFileExW` without `REPLACE_EXISTING`).
- **Move to Trash** through the OS Trash / Recycle Bin (`NSFileManager` on macOS, the shell on Windows, freedesktop trash on Linux). There is no permanent-delete code path. If the platform refuses, the item stays where it is and the failure is reported.
- **Target checks.** Before either operation, the parent folder is canonicalized and must lie inside the selected root. The item itself must be a regular file or folder, never a symlink, and never the root.

**Exact duplicates.**
- **Scope.** It reads only the locations you select. The UI sends opaque keys, not paths, and a folder chosen with the native picker is authorized for the current session only.
- **Exact matches only.** Comparison uses raw bytes (size, a partial fingerprint, then a streaming BLAKE3 hash with a fixed-size buffer). Nothing is decoded.
- **Cleanup safety.** A cleanup plan is validated in Rust: every group must keep at least one copy, and half of a Live Photo can't be trashed alone. Before each group is cleaned, the kept copies are re-checked (still present, same size and modification time, same fingerprint), and each file to be trashed is re-checked too. Only files the OS actually moved are reported as removed.
- **Privacy.** Results and hashes live only in memory. Thumbnails of analyzed files outside the browsed root are also kept only in memory, and Clear Analysis drops them.

Known limits: on Windows the worker relies on Job object limits and the absence of any path, not on a filesystem-denying sandbox. Linux uses rlimits and `no_new_privs`, without seccomp yet. Video decoding trusts the OS webview's sandbox. Crash/hang recovery of the web content process is implemented for macOS; on Windows a hung WebView2 page is reloaded, without process termination. WebM thumbnails usually fall back to an icon because WebKit doesn't expose WebM frames to a canvas.

## HEIC / HEIF decoding (macOS)

HEVC decoding is done by Apple's own sandboxed decoder service, which the default `pure-computation` profile cannot reach. A HEIC job (decided by magic bytes, passed to the worker as a fixed flag) therefore runs under a dedicated profile, `HEIF_PROFILE` in `worker.rs`:

- **Files:** no reads outside system locations (`/System`, `/usr/lib`, `/usr/share`, `/Library/Apple`, the dyld cache) and Mori's own app bundle; no writes of any kind.
- **Network and processes:** no network, no process creation.
- **IPC:** no service lookups except `com.apple.coremedia.videodecoder`.
- **IOKit:** no clients except `IOSurfaceRootUserClient` (decoded frames come back as shared surfaces).
- **Everything else denied:** shared memory, sockets, signals to other processes, preference writes and similar.

The worker still receives only bytes, never a path. The same resource limits (dimensions, pixels, CPU time, output size) apply, and a decode that fails lazily (an empty image) is treated as a failure. `tests/worker.rs` checks that the HEIF profile still denies file access, writes, the network and new processes.

## Similar media analysis

- **Photos:** decoded only by the worker (the `fingerprint` op), which returns two 64×64 grayscale miniatures; all comparison in Mori is arithmetic on those pixels.
- **Videos:** sampled by the webview through the same guarded path as previews: sandboxed probe and codec allow-list, blocklist, media-session watchdog. The page reduces each frame to 64×64 grayscale and sends raw pixels; Rust validates sizes and ranges and never parses anything from the page.
- **Cache:** fingerprints are cached under hash names in the app cache directory, versioned and tied to each file's path, size and modification time. They are strictly parsed on read; anything malformed is treated as a miss.
- **Decisions:** "Not duplicates" decisions are stored as pairs of 16-byte content identities (size plus partial BLAKE3), with no names or paths.
- **Cleanup:** uses the same validated executor as exact duplicates (at least one copy kept, kept copies re-checked, OS Trash only, Live Photo halves together).
- **Failures:** a file that can't be decoded or sampled is reported as "could not safely analyze" and remembered until it changes; the rest of the analysis continues.

## Supported formats (detail)

Mori deliberately keeps its attack surface small. Every file is listed and searchable, but only a few formats are ever decoded:

| Preview | Formats (judged by content, not extension) | How |
|---|---|---|
| Images | JPEG, PNG, WebP | Decoded and re-encoded by the sandboxed worker (up to 3072 px) |
| HEIC / HEIF (macOS) | HEIF stills | Decoded by macOS ImageIO inside the worker under the HEIF profile (below), then re-encoded like other images |
| GIFs | GIF | Re-encoded frame by frame by the worker, so they still animate |
| Videos | MP4 / MOV (H.264/HEVC), WebM | Container verified by magic bytes, then played by the system webview's sandboxed media engine |
| Text | txt, md, csv, tsv, log, json, srt, vtt, yaml, ini | Shown as inert plain text (first 256 KB) |

Everything else (HEIC on Windows/Linux, AVIF, RAW, TIFF, SVG, HTML, PDF, Office files, audio, MKV/AVI and so on) shows **Preview not supported** with the file's details. If the format is passive, an **Open in system** button hands it to its default app. Thumbnails exist only for JPEG, PNG, WebP, GIF and (on macOS) HEIC images and for playable videos. Other files show a type icon.

Library filters (Photos, Videos, GIFs, Documents, Audio) group files by extension for browsing only. That grouping is never used to decide how to parse a file.
