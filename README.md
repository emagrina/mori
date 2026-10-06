<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/branding/mori-icon-white.png">
    <img src="assets/branding/mori-icon-black.png" width="128" alt="Mori">
  </picture>
</p>

<h1 align="center">Mori</h1>

<p align="center">
  A private, offline-first media and file browser for exploring local and external storage.
</p>

---

Mori is a small desktop app (Tauri + Rust + React) that you can keep on an external drive and open with a double-click. It browses that drive like a Finder-style gallery: no accounts, no cloud, no telemetry, and no network access at all.

## Features

- **Offline-first.** No accounts, no telemetry, no cloud, no update checks, and no network requests.
- **Local file browsing** with folder navigation, breadcrumbs and back history.
- **Gallery, Grid and List views**, virtualized so folders with tens of thousands of files stay smooth.
- **Include subfolders.** View a folder and everything beneath it as one flat list. Filters, search and sorting apply to that combined set, and each file shows where it lives relative to the current folder.
- **Search and filtering.**
  - Instant, case- and accent-insensitive search by name, extension and folder.
  - Two scopes: the current folder (optionally with its subfolders) or the entire drive.
  - Type filters (Photos, Videos, GIFs, Documents, Audio).
  - Sorting by name, date, size or type.
- **Secure media previews** for JPEG/PNG/WebP images and, on macOS, HEIC/HEIF (zoom and pan), animated GIFs, MP4/MOV/WebM video, and plain text.
- **Portable mode.** When run from an external drive, Mori opens that drive automatically. Otherwise it asks for a folder once and remembers it.
- **Local caching and indexing.** A small index and thumbnail cache stay on the computer. **Clear cache** removes them.
- **Light and dark interface** that follows the system setting.
- **Safe file management.**
  - Rename, and **Move to Trash** (the system Trash / Recycle Bin, so items can be restored). Mori has no permanent-delete function.
  - Multi-selection with Cmd/Ctrl+Click and Shift+Click; ⌘⌫ on macOS, Delete on Windows.
  - Several items or a folder ask for confirmation first. Views, search results and the index update immediately.
- **Analyze**: two separate tools that only run when you start them. Both let you choose the locations (the current drive, Home, Pictures, Movies/Videos, Downloads, Documents or any folder you pick) and whether to include subfolders, and both read only the folders you select.

### Exact duplicates

Uses content verification to identify identical files.

- Files are grouped by size, then a partial fingerprint, then a full BLAKE3 hash of the content. Names, dates and visual similarity are never used, and media is never decoded.
- Results are deterministic: every file in a group has exactly the same bytes.

### Similar media

Uses local perceptual analysis to suggest photos and videos that may represent the same media despite different encoding, resolution or metadata: a HEIC and its JPEG export, a WhatsApp copy, a resized or recompressed version, a lightly edited or slightly cropped one, a MOV converted to MP4, a 4K and a 1080p copy, a slightly trimmed clip.

**Similar-media results are probabilistic.** They are estimates shown with an *estimated similarity* (never "identical" unless a full hash confirms it), and every group should be reviewed before cleanup.

- **Photos** are decoded in Mori's sandboxed worker and reduced to small normalized grayscale fingerprints, which are compared with a perceptual hash and then checked region by region. A different subject position, different text in a screenshot or a different scene is enough to reject a match.
- **Videos** are sampled at 12 points by the system video engine (the same guarded path as previews) and compared frame by frame, in order and across the whole clip. A shared intro alone never matches. Trimmed copies get a second, aligned sampling.
- **Strict / Balanced / Broad** sensitivity. Balanced is recommended; Broad finds more edited or cropped versions and more false positives.
- **Review tools:** side-by-side comparison with zoom, Keep all, Keep this one, Ignore (for this session), and **Not duplicates**, which Mori remembers locally by file content, so unchanged files aren't suggested again even if they are renamed or moved.
- Mori suggests the best copy to keep (resolution, original format, camera metadata, compression, derivative-looking names such as "WhatsApp" or "compressed"). It is only a suggestion.
- Fingerprints are cached on this computer per file version. **Clear cache** removes them.

### Both analyzers

- **Live Photos.** A HEIC/JPEG + MOV pair with the same name in the same folder is treated as one item: both halves are kept or trashed together.
- **Review before anything changes.** You pick the copy to keep per group, or auto-select the suggested copies, then confirm in a final review. Only that last step moves files to the Trash.
- **Never removes every copy.** At least one copy of every group is always kept, and this is enforced in the Rust backend, not just the UI. Kept copies are re-checked just before cleanup; if one has changed or its drive is gone, that group is left untouched.
- **Private results.** Results stay in memory and are discarded with **Clear Analysis** or when Mori quits. Nothing is uploaded; there is no cloud vision or remote service of any kind.

### Not implemented yet

These have been discussed for Mori but **are not in the code yet**:

- HEIC / HEIF previews on Windows and Linux (macOS only for now).
- Apple Live Photos playback (Still / Live / Loop modes). Live Photos are recognised as pairs by the analyzers only.
- Matching heavily cropped images, or videos where a large part was cut.

## Security design

Mori treats every file on a drive as **untrusted input**. It is designed to reduce the impact of malicious media. It does not and cannot guarantee that a crafted file is harmless. The design is built around:

- **Untrusted media.** File types are decided by magic bytes, never by extension, and disguised files are flagged and never handed to another app.
- **Isolated processing.**
  - Images are decoded in a separate, short-lived worker process with memory-safe Rust decoders.
  - On macOS the worker runs inside the system sandbox with no file or network access. HEIC/HEIF is decoded by macOS's own decoder inside a dedicated, equally closed profile that may only talk to Apple's sandboxed video-decoder service.
  - The worker receives bytes, never paths.
- **Sanitized previews.** The UI only shows freshly re-encoded images produced by the worker. Video containers and codecs are probed before playback, and only codecs known to work with the system player are streamed.
- **Least privilege and minimal IPC.**
  - The UI has no filesystem access and works with opaque file IDs.
  - Each IPC command is allow-listed individually.
  - Every path is canonicalized and confined to the selected folder, and symlinks are never followed.
- **No networking.** A strict Content-Security-Policy blocks every remote origin, and the Rust side has no network code.
- **Conservative file changes.**
  - Browsing and analysis are read-only.
  - The only changes Mori can make are a rename (which never overwrites an existing item) and moving items to the system Trash, always as an explicit user action.
  - Symlinks are never followed or acted on.
  - Both analyzers read only the folders you select. A folder picked with *Choose a Folder…* is remembered only for the current session.
- **Resource limits.**
  - Decoders have pixel, memory, input-size and time limits.
  - At most three workers run at once.
  - Video is streamed in bounded chunks, never loaded whole.
- **Graceful decoder failure.**
  - A crashed, hung or over-limit decoder only fails that one preview ("This video could not be safely previewed").
  - If the system video engine hangs or crashes, Mori restarts the web view and blocks that file from being loaded again.

The full design, limits and platform caveats are described in [docs/security.md](docs/security.md).

## Getting started

Requirements: [Node.js](https://nodejs.org) 20+, [Rust](https://rustup.rs) stable, and the [Tauri prerequisites](https://tauri.app/start/prerequisites/) for your OS.

```bash
npm install
npm run app:dev     # run in development mode
npm run app:build   # build release bundles into src-tauri/target/release/bundle/
```

Checks:

```bash
npm run typecheck
cd src-tauri
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test          # includes hostile-input tests against the sandboxed worker
```

### Portable use

Copy the built app into a folder on the drive (for example `Drive/Mori/Mori.app`, or `Mori.exe` on Windows) and open it. Mori browses the root of the drive it is running from:
- **macOS:** `/Volumes/<Drive>`
- **Windows:** a non-system drive letter
- **Linux:** a mount point under `/media`, `/run/media` or `/mnt`

The macOS build is not signed with a Developer ID. A copy that came from another Mac or a download may be quarantined. Open it with right-click → Open, or clear the flag once with `xattr -dr com.apple.quarantine Mori.app`.

## Data stored on your computer

Mori stores nothing on the browsed drive. It changes your files only when you rename an item or move it to the Trash (on an external drive, the system keeps trashed items in that drive's own Trash folder). Everything Mori itself stores lives in per-user app directories:

| | macOS | Windows |
|---|---|---|
| Settings, index, video blocklist | `~/Library/Application Support/app.mori.viewer/` | `%APPDATA%\app.mori.viewer\` |
| Thumbnail cache | `~/Library/Caches/app.mori.viewer/thumbs/` | `%LOCALAPPDATA%\app.mori.viewer\thumbs\` |
| Similar-media fingerprints | `~/Library/Caches/app.mori.viewer/similar/` | `%LOCALAPPDATA%\app.mori.viewer\similar\` |
| "Not duplicates" decisions | `…/app.mori.viewer/similar-dismissed.bin` | `%APPDATA%\app.mori.viewer\similar-dismissed.bin` |

Thumbnails are small re-encodes stored under hash names. Full-size previews are kept only in memory. Analysis results, and thumbnails of analyzed files outside the browsed drive, are never written to disk. Similar-media fingerprints are 64×64 grayscale miniatures stored under hash names (no file names), and "not duplicates" decisions are stored as pairs of content hashes (no names or paths).

## Platform status

- **macOS (Apple Silicon)** is the primary, tested platform.
- **Windows:** the platform-specific code (drive detection, worker Job object limits, Explorer integration) compiles for Windows, but the app has not yet been built or tested on Windows. The Windows worker is protected by resource limits and by never receiving a path, not by a filesystem-denying sandbox.
- **Linux** is structurally supported but untested.

## Project layout

```
src/                 React + TypeScript UI
src-tauri/src/       Rust: index & search, path confinement, sandboxed worker,
                     mori:// protocol, video probing and recovery, Trash/rename
                     (fileops), exact duplicates (dupes), similar media (similar)
src-tauri/tests/     hostile-input and sandbox tests for the worker
assets/branding/     Mori marks (dark/light folder) and the macOS app icon master
scripts/             build-icons.py: regenerates every icon from the branding artwork
docs/                design notes and screenshots
```

## Branding

Mori's mark is a folder with a pixel-art incognito character, in two variants:
- **Dark folder with a light character:** the primary mark and the app icon. It is used on light UI backgrounds.
- **Light folder with a dark character:** used on dark UI backgrounds.

All icons are generated from the original artwork by `python3 scripts/build-icons.py` (needs Pillow, NumPy and macOS `iconutil`). The original full-resolution artwork lives in `assets/branding/source/` and is not committed.
- **macOS:** the icon follows Apple's icon grid (a squircle with transparent margins), so macOS 26 doesn't shrink it onto a grey plate.
- **Windows and Linux:** the icons are the free-form folder.
- **16–48 px:** these sizes get a tighter crop and light sharpening so the character stays recognisable.

## License

No license has been chosen yet.
