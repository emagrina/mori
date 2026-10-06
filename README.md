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
- **Secure media previews** for JPEG/PNG/WebP images (zoom and pan), animated GIFs, MP4/MOV/WebM video, and plain text.
- **Portable mode.** When run from an external drive, Mori opens that drive automatically. Otherwise it asks for a folder once and remembers it.
- **Local caching and indexing.** A small index and thumbnail cache stay on the computer. **Clear cache** removes them.
- **Light and dark interface** that follows the system setting.
- **Read-only.** Mori never renames, moves, deletes or writes next to your files.

### Not implemented yet

These have been discussed for Mori but **are not in the code yet**:

- HEIC / HEIF previews. HEIC files are listed but shown as *Preview not supported*.
- Apple Live Photos (Still / Live / Loop modes).
- Duplicate file analyzer and exact-duplicate detection.
- Safe Trash / Recycle Bin operations. Mori is currently strictly read-only.

## Security design

Mori treats every file on a drive as **untrusted input**. It is designed to reduce the impact of malicious media. It does not and cannot guarantee that a crafted file is harmless. The design is built around:

- **Untrusted media.** File types are decided by magic bytes, never by extension, and disguised files are flagged and never handed to another app.
- **Isolated processing.**
  - Images are decoded in a separate, short-lived worker process with memory-safe Rust decoders.
  - On macOS the worker runs inside the system sandbox with no file or network access.
  - The worker receives bytes, never paths.
- **Sanitized previews.** The UI only shows freshly re-encoded images produced by the worker. Video containers and codecs are probed before playback, and only codecs known to work with the system player are streamed.
- **Least privilege and minimal IPC.**
  - The UI has no filesystem access and works with opaque file IDs.
  - Each IPC command is allow-listed individually.
  - Every path is canonicalized and confined to the selected folder, and symlinks are never followed.
- **No networking.** A strict Content-Security-Policy blocks every remote origin, and the Rust side has no network code.
- **Read-only access** to your files wherever possible (currently everywhere).
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

Mori writes nothing to the browsed drive. Everything lives in per-user app directories:

| | macOS | Windows |
|---|---|---|
| Settings, index, video blocklist | `~/Library/Application Support/app.mori.viewer/` | `%APPDATA%\app.mori.viewer\` |
| Thumbnail cache | `~/Library/Caches/app.mori.viewer/thumbs/` | `%LOCALAPPDATA%\app.mori.viewer\thumbs\` |

Thumbnails are small re-encodes stored under hash names. Full-size previews are kept only in memory.

## Platform status

- **macOS (Apple Silicon)** is the primary, tested platform.
- **Windows:** the platform-specific code (drive detection, worker Job object limits, Explorer integration) compiles for Windows, but the app has not yet been built or tested on Windows. The Windows worker is protected by resource limits and by never receiving a path, not by a filesystem-denying sandbox.
- **Linux** is structurally supported but untested.

## Project layout

```
src/                 React + TypeScript UI
src-tauri/src/       Rust: index & search, path confinement, sandboxed worker,
                     mori:// protocol, video probing and recovery
src-tauri/tests/     hostile-input tests for the worker
assets/branding/     Mori icon sources (black and white variants)
docs/                design notes
```

## License

No license has been chosen yet.
