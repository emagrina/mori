# Changelog

All notable changes to Mori are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Mori uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [1.0.1] - 2026-10-07

A patch release focused on everyday file management, plus video and tag fixes. Mori remains local and offline-first.

### Added
- **Desktop-style selection** in every view, in search results and with *Include subfolders*: a click selects, a double-click or Return opens; Cmd/Ctrl+Click, Shift+Click, Shift+arrows and ⌘A/Ctrl+A select several items; Esc clears. A selection bar shows "N selected" with the common actions.
- **Move to… and Copy to…** (`M` / `⇧M`) for files and folders, one or many. Mori shows what will happen first; name conflicts offer Keep Both, Replace (the existing file goes to the Trash first) or Skip, and nothing is overwritten in place. Moves to another drive copy, verify, and only then send the original to the Trash. Partial failures are reported. Undo moves items back (for moves to another drive, on macOS only).
- **Drag and drop** onto folders in the grid, list and sidebar: the selection moves together through the same checks as Move to…. Sidebar folders expand when you hold over them; Option/Alt copies. Internal to Mori only.
- **Quick Cleanup**: review a folder one file at a time from the keyboard (Keep / Mark for Trash, Undo, Back, Skip). Decisions are only staged; nothing moves until you review the marked items and confirm once, and then they go to the system Trash, never a permanent delete. Unfinished cleanups can be resumed (never saved in temporary sessions).
- **File actions inside the preview**: a `…` menu with Get Info, Favorites, Checksum, Rename, Move to…, Copy to… and Move to Trash.
- **Rename Tag… and Delete Tag…** from a tag's right-click menu in the sidebar. Renaming keeps every tagged item; duplicate names are refused. Deleting a tag never deletes or changes files.

### Improved
- **Favorites**: a clear star on the thumbnail (filled when set, outline on hover) that can be clicked without opening or selecting the item, a star in List view, and a Favorites count in the sidebar.
- Larger, labelled private and protected markers in the sidebar folder list.
- F2 renames. Renaming a file keeps its favorite and tags, and Undo of a rename or move restores them too.
- The offline world map loads only when Places is opened.

### Fixed
- **Quick Cleanup cropped portrait videos**: a vertical clip (for example 720×1280) showed only the middle of the frame. Videos, photos and GIFs are now fitted whole, as in the normal viewer.
- **Video filmstrip** frames keep the video's real shape: portrait and rotated phone videos are no longer cropped into landscape boxes.
- Video details report the displayed size of rotated phone videos.
- *Manage Tags* no longer leaves a deleted tag's view open.

## [1.0.0]

First stable release. The features are listed by area rather than by development commit.

Builds: macOS (Apple Silicon, Intel), Windows x64 (installer and MSI), Linux x64 (AppImage and .deb). macOS is the primary, hand-tested platform; the Windows and Linux builds are built and tested in CI.

### Browsing
- Folder navigation with breadcrumbs and back history, in virtualized Gallery, Grid and List views that stay smooth with tens of thousands of files.
- **Include subfolders**: a folder and everything beneath it as one flat list.
- Instant search (case- and accent-insensitive) in the current folder, with its subfolders, or across the whole drive.
- Filters for Photos, Videos, GIFs, Documents, Audio, Screenshots and Screen Recordings; sorting by name, date, size or type.
- Command palette (⌘K), keyboard navigation, Mori Quick Look (Space).
- Portable mode: run from an external drive and Mori opens that drive.
- Light and dark interface following the system setting.

### Media
- Previews of JPEG, PNG, WebP, GIF and, on macOS, HEIC/HEIF, with zoom and pan.
- MP4, MOV and WebM video with codec probing, a filmstrip and hover scrub.
- MP3, AAC/M4A, WAV, AIFF and FLAC audio with a waveform.
- Slideshow.
- **Safe PDF preview**: pages rendered as bitmaps inside the sandboxed worker, with nothing interactive executed.
- **Archive inspection**: ZIP, JAR, OOXML, ODF, EPUB, TAR and gzip are listed, never extracted, with traversal, absolute-path, nesting and bomb checks.
- Screenshots and screen recordings recognised locally (names in many languages and the macOS screen-capture attribute), with user corrections.

### Analysis
- **Exact Duplicates** by size, partial fingerprint and full BLAKE3 content hash.
- **Similar Media**: perceptual photo and video comparison with Strict, Balanced and Broad sensitivity. Results are probabilistic and clearly labelled as estimates.
- **Compare** views (A | B, Slider, Difference) and a suggested **Best Copy** with the reasons behind it; bursts detected from capture times.
- Live Photo pairs handled as one item.
- The backend guarantees at least one copy of every group is always kept.
- **Metadata inspector** (EXIF, XMP, IPTC, QuickTime/MP4, ID3, FLAC) and a **Sensitive Metadata** scan for location, people, device and identifier fields.
- **Places**: positions on an offline map of bundled outlines; no map service is contacted.
- **Sanitized copies** of JPEG, PNG and WebP, verified before writing; originals are never modified.
- **Media Health**: risk-flagged, broken, unsupported and decode-failed media.
- **Analysis Center**: every analysis in one place, with pause, resume and cancel where supported.

### Integrity
- **Calculate Checksum** (SHA-256) with the file's size, date and detected type; copied to the clipboard only on request.
- **Compare Integrity** for two files: SAME CONTENT or DIFFERENT CONTENT, exact binary equality.
- Opt-in **integrity snapshots** of files and folders, stored in Mori's own data and never next to your files.
  - Verifying a snapshot reports Unchanged, Changed, Missing and New.
  - Snapshots respect private folders and never follow links.

### Storage
- **Storage** view computed from the index: totals by type and year, largest items, and a treemap with drill-down.
- **Empty Folders**, re-verified on disk before anything moves to the Trash.

### Organization
- **Tags** and **Favorites**, stored per volume in Mori's own data; files are never modified.
- **Forget This Drive** removes everything Mori stores about a drive.
- **Clear Session Data** forgets in-memory results, recent locations and search.

### File Operations
- Rename, Move to Trash and **Undo** (Trash put-back, rename, sanitized copy).
- **Operation Preview** before multi-item or folder changes and before permanent deletion.
- **Delete Permanently**, with typed confirmation for folders and large batches, enforced in the backend.
- **Secure Overwrite** offered only where it is meaningful (in-place file systems on spinning disks) and refused elsewhere with the reason.

### Privacy
- No account, cloud, telemetry, analytics, update check or remote processing.
- **Private folders**: a visibility boundary that keeps contents out of views, search, counts, storage and analyses started outside them.
- **Temporary Session** and **Private Inspection** keep everything in memory, refuse to save records and clear everything on *End Session*.
- **Privacy & Local Data**: every item Mori stores, with sizes and locations. Clear any category, or **Reset Mori**; your files are never touched.
- Non-persistent web view storage. The folder picker's remembered location is removed after use.
- No logs in release builds; the crash marker holds only time, thread and source line.

### Security
- Real file-type detection by content (about 90 formats).
- Risk indicators: disguised executables, double extensions, direction-control characters, extreme dimensions, truncation, quarantine and more.
- Decoding in short-lived worker processes that receive bytes, never paths.
  - On macOS each runs in a system sandbox with no file, network or process access.
  - Resource limits on pixels, memory, input size and time.
- **Open in Isolation** shows only worker-made copies; originals are never handed to other apps automatically.
- **Safe Inspection Mode** for new drives: metadata-only indexing and no automatic decoding.
- **Read-only Mode** and **Never Modify** folders enforced by a backend mutation policy.
- Symbolic links are never followed.
- A strict Content-Security-Policy that refuses every remote origin, and a minimal allow-listed IPC surface.

### Diagnostics
- **Diagnostics** with a self-test on synthetic files: media support, worker sandbox profiles, resource limits, malformed input, type detection, archive traversal, symlinks, Read-only enforcement, protected folders, network denial, ephemeral mode and logging.
- Results are ✓ verified, △ limited, ✕ not working, or — a static fact, with no hard-coded check marks.

### Fixed during release verification
- Thumbnails didn't appear after **Generate previews** in Safe Inspection Mode or Private Inspection until the folder was reopened.
- The macOS bundle is now signed ad hoc as a whole (it was only linker-signed), so a downloaded copy passes code-signature verification instead of being reported as damaged.

[1.0.0]: https://github.com/emagrina/mori/releases/tag/v1.0.0
