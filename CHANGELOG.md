# Changelog

All notable changes to Mori are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Mori uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Drag and drop, favorites
- **Drag and drop** files and folders onto folders in Grid, Gallery and List views, the sidebar tree and the drive: the selection moves together, through the same backend Move as Move to… (conflicts, protected folders, Read-only Mode, cross-drive moves, Undo). Clear accept / refuse feedback, sidebar folders expand when you hold over them, folder cards spring open, Option/Alt copies. Internal only: not to or from other apps.
- **Favorite star** on the thumbnail (top-right, filled when set, outline on hover), clickable without opening or selecting; a dedicated star in List view; a Favorites count in the sidebar.

### File management
- **Desktop selection** in every view, search results and *Include subfolders*: a click selects and a double-click (or Return) opens; Cmd/Ctrl+Click, Shift+Click, Shift+arrows, ⌘A/Ctrl+A, Esc. Items are tracked by id, never by name.
- A **selection bar** ("3 selected") with Move to…, Copy to…, Tags, Favorite and Move to Trash; the context menu acts on the whole selection.
- **Move to…** and **Copy to…** for files and folders (`M` / `⇧M`): a backend dry run first, Keep Both / Replace / Skip for name conflicts (Replace sends the existing file to the Trash first; nothing is ever overwritten in place), cross-drive moves that copy, verify and only then send the original to the Trash, links copied as links, half-finished copies removed, partial failures reported, Undo.
- **File actions inside the preview**: a `…` menu with Get Info, Favorites, Checksum, Rename, Move to…, Copy to… and Move to Trash. After trashing or moving the viewed file, the preview continues with the next one.
- Rename with F2. Renaming a file keeps its favorite and tags; undoing a rename or move carries Mori's records back.
- Larger, labelled private/protected markers in the sidebar folder tree.

### Quick Cleanup
- Go through a folder (optionally with subfolders, filtered by type, in a chosen order) one file at a time: Keep (→/K) or Mark for Trash (←/D), Undo, Back, Skip, progress and counters.
- Decisions are **only staged in memory**; files are moved to the system Trash after the Review Marked screen and one confirmation, through the existing Trash command (with Undo). Never a permanent deletion.
- Held keys and bounced presses can't mark extra files; protected files can't be marked; files that vanish leave the queue.
- Unfinished sessions can be resumed in normal sessions (opaque ids only); never saved in Temporary Sessions or Private Inspection, and the final Trash step is unavailable while read-only.

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
