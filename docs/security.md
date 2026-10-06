# Mori security model

> This describes how Mori is designed to limit the impact of malicious files. It is defence in depth, not a guarantee: decoders, the operating system and the system web view can all have bugs.

Mori assumes every file on the drive may be malicious: malformed or oversized images, crafted video containers, hostile PDFs/SVG/HTML, disguised extensions, symlink tricks and spoofed filenames.

**Untrusted bytes are decoded outside the main process.**
- Image decoding happens in a separate, short-lived worker process, one per job (`Mori --mori-worker <op> <size>`, fixed arguments, no shell). Before reading any input, the worker:
  - drops privileges with the macOS `pure-computation` sandbox: no filesystem, no network, no new processes;
  - sets resource limits: CPU time, no file creation, few file descriptors, no core dumps;
  - on Windows is placed in a Job object (1 GB memory cap, a single process, no UI/clipboard access, killed with Mori).
- The worker never receives a path. Mori reads the file and pipes the bytes in, and the worker writes back a freshly encoded JPEG/PNG/GIF behind a tiny fixed binary header, which Mori validates. A test (`cargo test --test worker`) proves that inside the worker, reading files, writing files, network access and spawning processes all fail. The network part is checked on the loopback interface only, so the test never contacts an outside server.
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
- HTML and SVG are never rendered inside Mori. PDFs are only ever shown as bitmaps rasterised by the sandboxed worker (see *PDFs* below).
- *Open in system* runs only when you click it, and only for passive formats (photos, video, audio, PDF, text, Office documents). It is refused for anything whose content is executable (Mach-O, PE, ELF, `#!` scripts) or contradicts its extension. Mori launches the OS opener directly (`/usr/bin/open`, `explorer.exe`) with an argument array and never uses a shell.
- The scanner never follows symlinks and never enters other mounted filesystems. It indexes only regular files and folders (no devices, FIFOs or sockets), stops 64 levels deep, and skips names that aren't valid Unicode.

**Mori is quiet and conservative about changes.** Browsing, previews and duplicate analysis open files for reading only. Release builds log nothing. Search history is not stored.

**File changes** are limited to the operations below, each triggered only by an explicit user action and implemented in `src-tauri/src/fileops.rs` (plus `overwrite.rs`). They are always policy-checked first. The rest of this section was written for the first two; see *File operations* below for the others.

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

## PDFs (macOS)

- **Where.** `pdf.rs` runs only inside the worker (`pdfinfo`, `pdfpage` ops). The UI receives JPEG bitmaps of pages and a short `key=value` fact list (page count, encryption, info fields, presence of JavaScript / OpenAction / embedded files / forms).
- **Renderer.** CoreGraphics' PDF renderer draws page content only. It has no JavaScript engine and does not perform actions, follow links, open attachments or fetch anything.
- **Sandbox.** PDF jobs use a dedicated deny-by-default profile. The only additions over pure computation are read access to system fonts and frameworks (needed to draw the standard fonts documents reference without embedding) and metadata reads under system paths. No IPC services, no IOKit, no writes, no network, no processes. `cargo test --test worker` checks both that text renders and that user files, writes, network and process creation are denied.
- **Limits.** 160 MB input, 5,000 pages, page sizes up to 2,400 px, 25 s per page, absurd page boxes refused. Locked PDFs aren't rendered.

## Archives

- **Listing only.** `archive.rs` parses ZIP (central directory, ZIP64), TAR (ustar, pax and GNU long names) and gzip in the main process. Nothing is extracted or written; no entry is ever opened by another program.
- **Why not the worker.** Parsing is bounded, memory-safe Rust over directory metadata (plus streaming inflate for gzip and nested archives, with hard byte caps). This is a deliberate deviation from worker isolation, documented here.
- **Defences.** 200,000 entries, 128 MB central directory, 3 nesting levels, 64 MB per nested archive, 512 MB total inflation, 20 s time budget, `catch_unwind` around the whole listing. Declared sizes are never trusted for allocation.
- **Findings.** Traversal (`../`), absolute and drive-letter paths, control characters, symlinks, encrypted entries, zip-bomb ratios, entry counts, nesting depth, and partial listings.

## Metadata

- **Parsing in the worker.** `metadata.rs` (EXIF via the pure-Rust `kamadak-exif`, plus Mori's own bounded readers for XMP, IPTC, MP4/QuickTime atoms, ID3v2 and FLAC) runs only in the worker, under the pure-computation sandbox.
- **What the worker gets.**
  - MP4/MOV: the `moov` box, located by walking box headers.
  - Other files: the first 4 MB (scan) or 32 MB (inspector).
  - Output is JSON; the host re-checks every string (control and bidi characters, lengths) and every coordinate.
- **XMP is read as text.** There is no DTD, no entity definitions and no external references. Only the five XML entities and numeric references are decoded.
- **Limits.** 1,500 fields, 400 characters per value, 2 MB XMP, MP4 nesting depth 8, 4,096 boxes per level, the exif crate's own IFD-count cap.
- **One failure, one result.** Scans parse files in batches of 32 in one worker. If a batch's worker fails, each file is re-parsed in its own worker. Inside the worker each item is also isolated with `catch_unwind`.
- **Sanitized copies.** `sanitize.rs` rewrites containers in the worker (no re-encoding).
  - JPEG: only JFIF, ICC, Adobe and the image segments are kept; everything after the main image's EOI is dropped.
  - PNG: a whitelist of image chunks.
  - WebP: image chunks only, with the VP8X flags fixed.
  - Verification before writing: the output must decode to the same dimensions as the original, and a fresh metadata read must find no sensitive field or position.
  - Writing: `fileops::create_new` checks the mutation policy (`Create`), then `openat(O_CREAT|O_EXCL|O_NOFOLLOW)` under an `O_NOFOLLOW` directory handle. It never replaces anything and never writes through a symlink. The file is then read back and hashed.
- **Map.** Land outlines are bundled (`src/assets/world.ts`, Natural Earth 1:110m, public domain). The CSP already blocks every remote origin, so even a bug couldn't load tiles.

## Open in Isolation and Safe Inspection Mode

- **Isolation** is a view, not a container. The `mori://iso-*` routes are explicit per-file requests. The isolated view never offers *Open in system* and never falls back to the original. Video is shown as worker-re-encoded still frames (the frames themselves are decoded by the system web view's sandboxed media engine, behind the codec probe, blocklist and watchdog).
- **Safe Inspection Mode** is enforced in the protocol handler. For files under a drive in this mode, `thumb`, `preview`, `media`, `frame` and `pdf` requests without the `iso-` prefix return 403, and `store_frame` refuses non-explicit frames. The UI also skips video-thumbnail capture. A counter records how many media were decoded since the drive was opened.
- **Drive detection** polls `/Volumes` every 3 s for real, browsable mount points (macOS). Drives present at launch and drives Mori already knows are not announced. Nothing on the drive is read until you choose *Inspect Safely*.

## Media intelligence

- **Difference view.** It is computed in the web view from the two worker-made previews (never the originals), read through Mori's protocol with CORS. Nothing new is decoded outside the existing paths.
- **Capture times for bursts.** They are read only for photos already in Similar Media groups, through the worker metadata op (1 MB per file).
- **Screenshot detection.** It uses file names plus, on macOS, one `getxattr(…, XATTR_NOFOLLOW)` size query per photo/video during the index scan. No content is read.
- **Corrections.** They are stored like private folders (`capture-not.json`, `capture-yes.json`), keyed per volume.

## Storage, empty folders and media health

- **Storage** is pure arithmetic over the in-memory index. Private-folder contents are excluded.
- **Empty folders.** The index hides dotfiles, so every candidate is re-walked on disk with `symlink_metadata` (no link is followed; any link makes the folder non-empty), within 32 levels and 10,000 entries.
  - Only `.DS_Store`, `.localized`, `Thumbs.db`, `ehthumbs.db`, `desktop.ini`, `Icon\r` and small `._*` AppleDouble files count as clutter.
  - The walk is repeated right before each move to the Trash, through `fileops::move_to_trash` and the mutation policy.
- **Media Health.**
  - Type and risk checks read only the head and tail.
  - Images are decoded by the worker with the usual limits. A cached thumbnail for the same file version counts as decoded.
  - Videos use the sandboxed container probe and the blocklist. Audio gets type checks only.
  - Worker failures are reported precisely: unsupported, damaged data, safety limits, timeout or crash.
  - Every check is isolated with `catch_unwind`, and decodes count toward Safe Inspection Mode's "Media decoded".

## Organization and sessions

- **Where records live.** Favorites and tags are records in Mori's app data, keyed per volume like private folders. Nothing is written into files, extended attributes or metadata. Tag names are filtered of control and bidi characters and capped in length.
- **Temporary session** (`AppState.temp`). While it is on:
  - index caches are not saved;
  - thumbnails go to the in-memory cache only;
  - Similar Media uses no fingerprint cache;
  - the folder isn't remembered;
  - every command that changes Mori's records (private, protected, favorites, tags, screenshot corrections, drive previews) refuses with an explanation.
  - Verified on the real app with a debug-only hook: no file in app data or the cache changed during a temporary session.
  - The video blocklist (opaque hashes, no paths) is still kept, because it protects the web view.
- **Forget This Drive.** It deletes only through `remove_app_path`, which refuses anything not strictly inside Mori's own data or cache directory (tested).
  - What is removed: index caches whose recorded root is on the volume, the per-volume thumbnail folder, and the volume's records in every store.
  - Verified on the real app: the drive's files were identical before and after.
  - Similar-media fingerprints are content-addressed and can't be attributed to a drive; Clear Cache removes them.
  - Thumbnails made before this version sit in the old shared layout until Clear Cache.

## Audio, filmstrips and scrubbing

- **The `mori://audio` route** serves bounded byte ranges only for MP3, AAC/M4A, WAV, AIFF and FLAC identified by magic bytes. A PNG or an executable named `.mp3` is refused (tested).
  - The route respects Safe Inspection Mode and the media blocklist.
  - Playback and the waveform decode run in the system web view's media engine, inside a media session, so the existing hang/crash recovery and blocklist cover them.
  - The waveform is decoded at 3 kHz, only for files of at most 32 MB.
  - The isolated view doesn't play audio, because that would be the original bytes.
- **Filmstrip and hover-scrub frames** go through the same capture path as the isolated view: the webview decodes, the worker re-encodes, and the cache stores the copies. Non-explicit requests are refused in Safe Inspection Mode.
- **Quick Look** is Mori's own preview in a floating panel. Mori never invokes the system Quick Look or any other viewer.

## File operations

- **Trash.** On macOS, Mori calls NSFileManager `trashItemAtURL:resultingItemURL:` directly and keeps the resulting location (in memory, for this session) so Undo can move the item back.
  - Undo goes through `fileops::restore_from_trash`. It checks the policy (`Restore`), then uses an exclusive rename (`renamex_np(RENAME_EXCL)`), so it never overwrites an item that took the name.
  - Other platforms use the `trash` crate and don't offer Undo for Trash.
- **Operation plans** (`plan_operation`) run the same confinement and policy checks as the real operation, without doing anything.
- **Permanent delete** (`fileops::delete_permanently`).
  - Policy `Delete`: refused for protected folders, or folders containing one.
  - Links are unlinked, never followed.
  - Folders are removed with std's `remove_dir_all`, which never follows symlinks.
  - `delete_items` re-plans on the backend and requires the literal confirmation `DELETE` for folders and large batches, whatever the UI sends.
- **Secure Overwrite** (`overwrite.rs`).
  - Eligibility is computed from `statfs` (file system type) and IOKit "Device Characteristics → Medium Type" for the volume's device. Anything other than a confirmed rotational disk with an in-place file system is refused, with a reason.
  - The overwrite opens with `O_NOFOLLOW`, rejects links and files with other hard links, and checks the inode didn't change between check and open.
  - It writes one pass of PRNG data, then `fsync` + `F_FULLFSYNC`, then unlinks.
  - Tests check that the bytes are replaced in place (read through a handle opened before), that link targets and hard-linked twins are never touched, and that the system disk (APFS/SSD) is refused.
- **History** (`history.rs`) is in memory only. Permanent deletions are recorded as not undoable.

## Mutation policy

- **One gate.** All changes to the user's files go through `fileops`, whose mutating functions (`move_to_trash`, `rename_no_replace`, and later operations) take a `&Policy` and call `policy.check` first.
- **The policy refuses:**
  - everything in Read-only Mode;
  - any change to a path that is, or is inside, a protected folder;
  - trashing, renaming or deleting a folder that contains a protected folder;
  - creating files inside a protected folder.
- **Not covered:** Mori's own app state (index, caches, settings, marks).

## Type detection and risk indicators

- **Detection** (`filetype.rs`) reads only the first bytes (and, for truncation checks, the last kilobyte) and matches signatures. Nothing is executed or decoded.
- **Risk indicators** (`risk.rs`) are factual checks with explanations. Image dimensions come from header fields only.
- **No verdicts.** The UI never says a file is safe. With no findings it says "No anomaly detected".

## Symbolic links

- **Indexed, never followed.** The scanner records links as their own entries (link text plus whether it points outside the root, decided lexically without touching the target).
- **Never opened through the link.** Links can't be opened, previewed or read through Mori: `open_by_id` and the `mori://` protocol refuse them.
- **Mutations act on the link itself.** Trash and rename (`confined_item` uses `symlink_metadata`) never touch the target.
- **Analyses** (`dupes::collect`) skip links entirely.

## Private folders (visibility boundaries)

Private folders are a Mori visibility rule, **not a security boundary against other software**:

- **Nothing on disk changes.** No encryption, permission changes, hidden marker files or renames are made, and anyone with access to the drive can read the files.
- **Enforcement is central.**
  - When an index is published, every entry gets the path length of the deepest private folder above it (`Index::apply_boundaries`).
  - `index::query` and `index::stats` show an entry only if that private folder is not strictly below the current scope. That covers library categories, All Files, global and folder search, filters, "Include subfolders" and counts, so no view can forget the rule.
- **Analyses.** The shared walker in `dupes::collect` (Exact Duplicates and Similar Media) stops at private folders below each chosen location before reading anything inside them. A private folder chosen directly as a location is analysed, with nested private folders still skipped. Making a folder private also drops its files from analysis results already computed from outside it.
- **Storage.** Records live only in app data (`private-folders.json`):
  - per volume, identified by the filesystem UUID on macOS (getattrlist `ATTR_VOL_UUID`), or by the mount path elsewhere;
  - each record holds the folder's path relative to the volume and its inode.
- **Renames and moves.**
  - Renames made through Mori update the records.
  - A private folder renamed or moved within the same volume outside Mori is matched again by inode during the next scan, before partial results are shown.
  - A record that can't be matched is kept, never silently dropped.
- **Files stay reachable by id.** Opening a file by an id that was obtained while browsing inside the folder still works. The rule governs what is listed, not access.

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

## Privacy, offline operation and ephemeral sessions

The privacy model, the inventory of everything Mori stores, Temporary Session / Private Inspection behaviour and its regression test are documented in [privacy.md](privacy.md). The threat model and vulnerability reporting are in [../SECURITY.md](../SECURITY.md).

- **Web view.** It uses a non-persistent data store. Its Content-Security-Policy refuses every remote origin; `connect-src` allows only IPC and Mori's own `mori:` protocol, which the audio waveform reads.
- **Checksums.** SHA-256 is computed in the main process by reading bytes in 1 MiB chunks (no parsing). Results are cached in memory, keyed by device/inode/size/mtime/ctime.
- **Integrity snapshots.** Opt-in JSON in `integrity/`. Snapshot ids are validated (16 hex characters) before any file path is built.
- **Diagnostics.** They run on synthetic fixtures. Worker capabilities count as verified only when a real worker run succeeded; a unit test runs them without a worker and asserts none of them pass.
