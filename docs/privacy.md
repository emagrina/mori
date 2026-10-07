# Mori privacy model and local data inventory

Mori is designed so that inspecting files leaves as little Mori-generated
state behind as is reasonably possible, and so that whatever it does keep is
listed, explained and removable. This document is the inventory; **Privacy &
Local Data** in the app shows the same categories with live sizes.

## Where things go

- **Nothing leaves the computer.**
  - No account, login, cloud service, telemetry, analytics, crash reporting, update check or remote processing.
  - The web view's Content-Security-Policy refuses every remote origin.
  - Decoder workers are denied network access by the OS sandbox (verified by the self-test on the loopback interface).
  - The main process contains no network code. It is not sandboxed from the network, so this is a property of the code rather than an OS-enforced barrier.
- **Nothing is written next to your files**, except what you explicitly ask for: a rename, Trash, a sanitized copy, or a permanent delete.
- **The web view stores nothing on disk.** It uses a non-persistent data store: no cookies, local storage, IndexedDB, HTTP cache or tracking statistics.

## Persistent data (normal sessions)

App data: `~/Library/Application Support/app.mori.viewer/` (Windows `%APPDATA%\app.mori.viewer\`, Linux `~/.local/share/app.mori.viewer/`).
Cache: `~/Library/Caches/app.mori.viewer/` (Windows `%LOCALAPPDATA%\app.mori.viewer\`, Linux `~/.cache/app.mori.viewer/`).

| What | Where | Why | Deleted when | Can reveal |
|---|---|---|---|---|
| Thumbnails, video frames | cache `thumbs/<volume>/` | Fast browsing | Clear Cache, Forget This Drive, Clear Mori Data, Reset | What images and videos look like |
| Similarity fingerprints | cache `similar/` | Faster Similar Media | Clear Cache, Clear Mori Data, Reset | 64×64 grayscale miniatures (no names) |
| Folder indexes | data `index-v2/` | Instant browsing and search | Clear Cache, Forget Drive, Clear Mori Data, Reset | File and folder names, sizes, dates |
| Settings | data `settings.json` | View preferences, Read-only Mode, last folder | Reset (last folder: Clear History) | Last folder's path |
| Known drives | data `drives.json` | Safe Inspection Mode per drive | Clear History, Forget Drive, Reset | Volume IDs and names |
| Private / protected folder rules | data `private-folders.json`, `protected-folders.json` | Visibility boundaries, Never Modify | Clear Mori Data (rules), Forget Drive, Reset | Paths of those folders |
| Tags, favorites, screenshot corrections | data `tags.json`, `favorites.json`, `capture-*.json` | Organization | Clear Mori Data (tags & favorites), Forget Drive, Reset | Paths of tagged items, tag names |
| "Not duplicates" decisions | data `similar-dismissed.bin` | Don't suggest them again | Clear Mori Data (analysis), Reset | Pairs of content hashes only |
| Media-engine blocklist | data `blocked-media.json` | Never reload a video that froze the system engine | Clear Mori Data (analysis), Reset | Opaque hashes only |
| Unfinished Quick Cleanup sessions (normal sessions only) | data `cleanup-sessions.json` | Resume a cleanup later | Discard in Quick Cleanup, finishing it, Clear Mori Data (analysis), Forget Drive, Reset | Opaque item and folder ids with Keep / Mark for Trash decisions; hashes of the root and volume. No names or paths |
| Integrity snapshots | data `integrity/` | Verify files later (opt-in) | Delete in Integrity Snapshots, Clear Mori Data, Reset | Relative paths, sizes, SHA-256 |
| Crash marker | data `last-panic.txt` | Diagnose crashes | Clear History, Reset | Time, thread, source-code line (no message, path or name) |
| macOS preferences | `~/Library/Preferences/app.mori.viewer.plist` | Window state | Reset | The folder picker's last folder is removed after each use, at quit and at launch |

**In memory only** (gone at quit): previews, PDF pages, archive listings, metadata results, checksums, analysis results, undo history, recent locations, Move/Copy destinations picked this session, search.

There are **no logs** in release builds. Developer builds print diagnostics only when explicitly enabled with `MORI_DEBUG_*` environment variables, and those may contain paths.

## Temporary Session and Private Inspection

During a session, Mori:
- keeps the index, thumbnails, previews, frames, PDF pages, metadata, checksums, analysis results, undo history and search in memory;
- refuses to save tags, favorites, folder rules, integrity snapshots, drive settings, "not duplicates" decisions and Quick Cleanup queues (cleanup decisions stay in memory and end with the session);
- uses no fingerprint cache;
- doesn't open files in other apps or reveal them in Finder.

**Ending the session**:
1. cancels analyses and checks;
2. drops every in-memory cache, result and history entry;
3. clears the folder picker's record;
4. returns to the normal folder.

There is no temporary directory to clean, because session data is never written to disk.

**At launch**, Mori removes leftovers of writes interrupted by a crash (`*.tmp`, `*.part`), only inside its own folders and never through a link. It also removes the picker record if a previous run couldn't.

**Tested.** `tests/ephemeral.rs` runs the real app with an isolated home folder:
- it browses, searches, makes thumbnails and previews, renders a PDF page, reads metadata, hashes files and lists an archive in a Private Inspection, then ends it;
- it checks that **no file Mori writes changed**;
- it checks that nothing contains the inspected names or the search term;
- it checks that the saved Read-only setting was untouched.

The test fails if, for example, thumbnails start being written during a session (checked by deliberately breaking that guard).

## Clear Mori Data and Reset Mori

- **Clear Mori Data** removes the selected categories: cache & thumbnails, analysis data, history, tags & favorites, folder rules, integrity snapshots.
- **Reset Mori** (confirmed by typing RESET) removes all of them, plus settings, preferences and old web view data, and returns Mori to a fresh state.
- **Deletion is guarded:** it only removes paths strictly inside Mori's own folders (app data, cache, and WebKit's folder for Mori's identifier). It refuses `..` and links. **Files on your drives are never deleted.**

## What Mori cannot control

Mori minimises its own persistent state. It cannot remove traces created outside it, including:
- file-system journals;
- the operating system's swap and memory compression;
- APFS or Time Machine snapshots and other backups;
- system crash reports;
- storage-controller behaviour (SSD wear-levelling);
- Spotlight or other indexing of the files themselves;
- system "recent places" kept by file dialogs;
- third-party monitoring software.

Memory isn't wiped. Mori releases buffers when it no longer needs them, zeroes its hashing buffer, and avoids keeping full file contents.
