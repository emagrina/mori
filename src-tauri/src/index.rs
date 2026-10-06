//! The file index: scanning a root folder, persisting a small JSON snapshot,
//! and answering browse/search queries against it in memory.

use crate::secure::display_safe;
use crate::thumbs::fnv;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;
use walkdir::WalkDir;

const INDEX_VERSION: u32 = 2;
/// Never descend deeper than this (defends against pathological trees).
const MAX_DEPTH: usize = 64;
/// Longest relative path accepted into the index.
const MAX_PATH_LEN: usize = 4096;
/// Upper bound on items returned to the UI for a single query.
pub const RESULT_LIMIT: usize = 50_000;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Folder,
    Photo,
    Video,
    Gif,
    Document,
    Audio,
    Other,
}

impl Kind {
    fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "photo" => Kind::Photo,
            "video" => Kind::Video,
            "gif" => Kind::Gif,
            "document" => Kind::Document,
            "audio" => Kind::Audio,
            "other" => Kind::Other,
            _ => return None,
        })
    }
}

pub fn kind_for_ext(ext: &str) -> Kind {
    match ext {
        "jpg" | "jpeg" | "jpe" | "jfif" | "png" | "webp" | "heic" | "heif" | "avif" | "bmp" | "tif" | "tiff"
        | "svg" | "ico" | "dng" | "cr2" | "cr3" | "nef" | "arw" | "orf" | "rw2" | "raf" | "raw" => Kind::Photo,
        "gif" => Kind::Gif,
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" | "wmv" | "flv" | "mpg" | "mpeg" | "3gp" | "mts" | "m2ts"
        | "ts" | "ogv" => Kind::Video,
        "mp3" | "wav" | "flac" | "aac" | "m4a" | "ogg" | "oga" | "opus" | "aiff" | "aif" | "wma" | "alac" => {
            Kind::Audio
        }
        "pdf" | "txt" | "md" | "markdown" | "rtf" | "doc" | "docx" | "odt" | "xls" | "xlsx" | "ods" | "csv" | "tsv"
        | "ppt" | "pptx" | "odp" | "pages" | "numbers" | "key" | "epub" | "json" | "xml" | "html" | "htm" | "log"
        | "yaml" | "yml" => Kind::Document,
        _ => Kind::Other,
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Entry {
    /// Path relative to the root, always with `/` separators.
    pub path: String,
    pub name: String,
    /// Lower-case extension without the dot ("" for folders / no extension).
    pub ext: String,
    pub kind: Kind,
    pub size: u64,
    /// Milliseconds since the Unix epoch.
    pub modified: i64,
    pub created: Option<i64>,
    /// Folded (lower-case, accent-free) relative path used for search.
    #[serde(skip)]
    pub key: String,
    /// Opaque, stable identifier handed to the UI instead of a path.
    #[serde(skip)]
    pub id: u64,
}

pub fn id_for(rel: &str) -> u64 {
    fnv(rel.as_bytes())
}

pub fn id_str(id: u64) -> String {
    format!("{id:016x}")
}

pub fn parse_id(s: &str) -> Option<u64> {
    if s.len() != 16 {
        return None;
    }
    u64::from_str_radix(s, 16).ok()
}

/// What the UI sees: an opaque id plus display-safe strings. Never a real path.
#[derive(Serialize)]
pub struct Item {
    pub id: String,
    pub name: String,
    /// Display-only relative path ("Photos/2024/x.jpg").
    pub path: String,
    pub ext: String,
    pub kind: Kind,
    pub size: u64,
    pub modified: i64,
    pub created: Option<i64>,
    /// Folder relative to the current view (set by queries).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

impl From<&Entry> for Item {
    fn from(e: &Entry) -> Self {
        Item {
            id: id_str(e.id),
            name: display_safe(&e.name),
            path: display_safe(&e.path),
            ext: display_safe(&e.ext),
            kind: e.kind,
            size: e.size,
            modified: e.modified,
            created: e.created,
            location: None,
        }
    }
}

impl Entry {
    pub fn parent(&self) -> &str {
        parent_of(&self.path)
    }
}

pub fn parent_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

#[derive(Serialize, Deserialize, Default)]
pub struct Index {
    pub version: u32,
    pub root: String,
    pub scanned_at: i64,
    pub files: Vec<Entry>,
    pub dirs: Vec<Entry>,
    /// id -> (is_dir, position)
    #[serde(skip)]
    pub by_id: HashMap<u64, (bool, usize)>,
}

impl Index {
    pub fn new(root: String, scanned_at: i64, files: Vec<Entry>, dirs: Vec<Entry>) -> Index {
        let mut idx = Index { version: INDEX_VERSION, root, scanned_at, files, dirs, by_id: HashMap::new() };
        idx.build_lookup();
        idx
    }

    fn build_lookup(&mut self) {
        self.by_id.clear();
        for (i, e) in self.files.iter().enumerate() {
            self.by_id.insert(e.id, (false, i));
        }
        for (i, e) in self.dirs.iter().enumerate() {
            self.by_id.insert(e.id, (true, i));
        }
    }

    pub fn get(&self, id: &str) -> Option<&Entry> {
        let (is_dir, i) = *self.by_id.get(&parse_id(id)?)?;
        if is_dir {
            self.dirs.get(i)
        } else {
            self.files.get(i)
        }
    }

    pub fn file(&self, id: &str) -> Option<&Entry> {
        self.get(id).filter(|e| e.kind != Kind::Folder)
    }

    /// Relative path of a folder id ("" = root). None if unknown.
    pub fn folder_path(&self, id: &str) -> Option<&str> {
        if id.is_empty() {
            return Some("");
        }
        self.get(id).filter(|e| e.kind == Kind::Folder).map(|e| e.path.as_str())
    }
}

/// Lower-case and strip diacritics so "cumpleanos" finds "cumpleaños" and
/// NFC/NFD spellings of the same name (common on macOS volumes) compare equal.
pub fn fold(s: &str) -> String {
    s.nfd().filter(|c| !is_combining_mark(*c)).flat_map(char::to_lowercase).collect()
}

fn millis(t: std::io::Result<SystemTime>) -> Option<i64> {
    t.ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64)
}

pub fn now_millis() -> i64 {
    millis(Ok(SystemTime::now())).unwrap_or(0)
}

const PACKAGE_EXT: &[&str] = &[
    "app",
    "photoslibrary",
    "photolibrary",
    "migratedphotolibrary",
    "aplibrary",
    "imovielibrary",
    "fcpbundle",
    "theater",
    "musiclibrary",
    "tvlibrary",
    "lrdata",
    "bundle",
    "framework",
    "plugin",
    "xcodeproj",
    "xcworkspace",
    "pkg",
    "mpkg",
];

/// Folders and files that are never worth showing in a media browser.
fn should_skip(name: &str, is_dir: bool) -> bool {
    if name.starts_with('.') {
        return true;
    }
    if is_dir {
        let lower = name.to_ascii_lowercase();
        // macOS packages (Mori.app itself, Photos/iMovie libraries…) look like
        // folders but are opaque; their internals are caches and duplicates.
        let package = lower.rsplit_once('.').is_some_and(|(_, ext)| PACKAGE_EXT.contains(&ext));
        return package
            || matches!(lower.as_str(), "$recycle.bin" | "system volume information" | "$windows.~bt" | "found.000");
    }
    matches!(name.to_ascii_lowercase().as_str(), "thumbs.db" | "desktop.ini" | "ehthumbs.db")
}

/// Root-relative path with `/` separators. Names that aren't valid Unicode
/// are rejected rather than lossily mangled (they could never be re-opened).
fn rel_path(root: &Path, p: &Path) -> Option<String> {
    let s = p.strip_prefix(root).ok()?.to_str()?;
    if s.is_empty() || s.len() > MAX_PATH_LEN {
        return None;
    }
    Some(if cfg!(windows) { s.replace('\\', "/") } else { s.to_owned() })
}

pub fn make_entry(path: String, name: String, is_dir: bool, meta: &fs::Metadata) -> Entry {
    let (ext, kind) = if is_dir {
        (String::new(), Kind::Folder)
    } else {
        let ext = match name.rfind('.') {
            Some(i) if i > 0 => name[i + 1..].to_lowercase(),
            _ => String::new(),
        };
        let kind = kind_for_ext(&ext);
        (ext, kind)
    };
    let key = fold(&path);
    let id = id_for(&path);
    Entry {
        id,
        path,
        name,
        ext,
        kind,
        size: if is_dir { 0 } else { meta.len() },
        modified: millis(meta.modified()).unwrap_or(0),
        created: millis(meta.created()),
        key,
    }
}

/// A borrowed view of the files and folders found so far during a scan.
pub type Snapshot<'a> = Option<(&'a [Entry], &'a [Entry])>;

/// Walk `root` recursively. `cancelled` is polled to abort early (e.g. the user
/// switched drives); `progress` gets the running file count and, at most every
/// `snapshot_every`, a borrowed view of what has been found so far.
pub fn scan(
    root: &Path,
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(usize, Snapshot<'_>),
    snapshot_every: Option<Duration>,
) -> Option<Index> {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut last_snapshot = Instant::now();
    // Symlinks are never followed, mounted filesystems inside the root are not
    // entered, and anything that isn't a plain file or folder (devices, FIFOs,
    // sockets, links) is ignored.
    let walker =
        WalkDir::new(root).follow_links(false).same_file_system(true).max_depth(MAX_DEPTH).into_iter().filter_entry(
            |e| e.depth() == 0 || e.file_name().to_str().is_some_and(|n| !should_skip(n, e.file_type().is_dir())),
        );

    for (i, item) in walker.enumerate() {
        if i % 64 == 0 && cancelled() {
            return None;
        }
        // Unreadable entries (permissions, broken links) are simply skipped.
        let Ok(item) = item else { continue };
        if item.depth() == 0 {
            continue;
        }
        let ft = item.file_type();
        if !ft.is_dir() && !ft.is_file() {
            continue;
        }
        let Some(rel) = rel_path(root, item.path()) else { continue };
        let Ok(meta) = item.metadata() else { continue };
        let Some(name) = item.file_name().to_str().map(str::to_owned) else { continue };
        let entry = make_entry(rel, name, ft.is_dir(), &meta);
        if ft.is_dir() {
            dirs.push(entry);
        } else {
            files.push(entry);
            if files.len() % 250 == 0 {
                let snapshot = match snapshot_every {
                    Some(every) if last_snapshot.elapsed() >= every => {
                        last_snapshot = Instant::now();
                        Some((files.as_slice(), dirs.as_slice()))
                    }
                    _ => None,
                };
                progress(files.len(), snapshot);
            }
        }
    }
    progress(files.len(), None);
    Some(Index::new(root.to_string_lossy().into_owned(), now_millis(), files, dirs))
}

pub fn load(file: &Path, root: &Path) -> Option<Index> {
    let data = fs::read(file).ok()?;
    let mut idx: Index = serde_json::from_slice(&data).ok()?;
    if idx.version != INDEX_VERSION || Path::new(&idx.root) != root {
        return None;
    }
    for e in idx.files.iter_mut().chain(idx.dirs.iter_mut()) {
        e.key = fold(&e.path);
        e.id = id_for(&e.path);
    }
    idx.build_lookup();
    Some(idx)
}

pub fn save(file: &Path, idx: &Index) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec(idx)?)?;
    fs::rename(tmp, file)
}

// ---------------------------------------------------------------- queries

#[derive(Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct Query {
    /// Id of the folder being browsed ("" = root).
    pub folder: String,
    /// "folder" browses one folder; "library" lists matching files drive-wide.
    pub scope: String,
    /// "all" or a kind name.
    pub kind: String,
    pub search: String,
    /// "name" | "modified" | "created" | "size" | "type"
    pub sort: String,
    pub desc: bool,
    /// "Include subfolders": show everything below `folder` as one flat list.
    pub recursive: bool,
    /// Search the whole drive instead of the current folder.
    pub global: bool,
}

#[derive(Serialize)]
pub struct QueryResult<'a> {
    pub items: Vec<&'a Entry>,
    pub total: usize,
    pub truncated: bool,
    /// Relative path the results are scoped to ("" = whole drive); item
    /// locations are shown relative to it.
    pub base: &'a str,
}

/// Case-insensitive natural ordering: "IMG_2" < "IMG_10".
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = a.peek().copied().filter(char::is_ascii_digit) {
                    na.push(c);
                    a.next();
                }
                let mut nb = String::new();
                while let Some(c) = b.peek().copied().filter(char::is_ascii_digit) {
                    nb.push(c);
                    b.next();
                }
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                a.next();
                b.next();
            }
        }
    }
}

fn compare(a: &Entry, b: &Entry, sort: &str) -> Ordering {
    let by_name = || natural_cmp(&a.name, &b.name);
    match sort {
        "modified" => a.modified.cmp(&b.modified).then_with(by_name),
        "created" => a.created.unwrap_or(a.modified).cmp(&b.created.unwrap_or(b.modified)).then_with(by_name),
        "size" => a.size.cmp(&b.size).then_with(by_name),
        "type" => a.ext.cmp(&b.ext).then_with(by_name),
        _ => by_name(),
    }
}

pub fn query<'a>(idx: &'a Index, q: &Query) -> QueryResult<'a> {
    let empty = QueryResult { items: Vec::new(), total: 0, truncated: false, base: "" };
    let tokens: Vec<String> = fold(&q.search).split_whitespace().map(String::from).collect();
    let searching = !tokens.is_empty();
    let kind = Kind::parse(&q.kind);
    let kind_ok = |e: &Entry| kind.is_none_or(|k| e.kind == k);

    // Scope: the whole drive (library views, global search) or one folder,
    // optionally including everything beneath it. Everything comes from the
    // in-memory index; nothing touches the disk here.
    let drive_wide = q.scope == "library" || (searching && q.global);
    let base: &str = if drive_wide {
        ""
    } else {
        match idx.folder_path(&q.folder) {
            Some(p) => p,
            None => return empty,
        }
    };
    let recursive = drive_wide || q.recursive;
    let folded_base = fold(base);

    // The part of an entry's (folded) path below `base`, if it is below it.
    let rel_key = |e: &'a Entry| -> Option<&'a str> {
        if base.is_empty() {
            return Some(e.key.as_str());
        }
        let below = e.path.len() > base.len() && e.path.as_bytes()[base.len()] == b'/' && e.path.starts_with(base);
        if !below || !e.key.starts_with(&folded_base) {
            return None;
        }
        e.key.get(folded_base.len() + 1..)
    };
    let in_scope = |e: &'a Entry| if recursive { rel_key(e).is_some() } else { e.parent() == base };
    // Search matches the path *relative to the scope*, so searching "family"
    // while inside Family doesn't trivially match everything.
    let matches = |e: &'a Entry| tokens.iter().all(|t| rel_key(e).is_some_and(|k| k.contains(t.as_str())));
    let wanted = |e: &'a Entry| in_scope(e) && (!searching || matches(e));

    // Folders: always listed when browsing a single level (whatever the type
    // filter, so you can still navigate down); listed as matches of a search
    // only when no type filter is active; never in recursive (flattened) views.
    let show_dirs =
        q.scope != "library" && if searching { kind.is_none() && (drive_wide || !recursive) } else { !recursive };
    let mut dirs: Vec<&Entry> = if show_dirs { idx.dirs.iter().filter(|e| wanted(e)).collect() } else { Vec::new() };
    let mut files: Vec<&Entry> = idx.files.iter().filter(|e| kind_ok(e) && wanted(e)).collect();

    let sort = q.sort.as_str();
    // Folders have no meaningful size/type, so they're always ordered by name
    // unless sorting by date.
    let dir_sort = if matches!(sort, "modified" | "created") { sort } else { "name" };
    dirs.sort_by(|a, b| compare(a, b, dir_sort));
    files.sort_by(|a, b| compare(a, b, sort));
    if q.desc {
        dirs.reverse();
        files.reverse();
    }

    let total = dirs.len() + files.len();
    let mut items = dirs;
    items.extend(files);
    items.truncate(RESULT_LIMIT);
    QueryResult { truncated: total > items.len(), items, total, base }
}

/// Where an entry lives, relative to the folder being viewed ("" = right there).
pub fn location<'a>(e: &'a Entry, base: &str) -> &'a str {
    let parent = e.parent();
    if base.is_empty() {
        return parent;
    }
    parent.strip_prefix(base).map_or(parent, |rest| rest.trim_start_matches('/'))
}

#[derive(Serialize, Default)]
pub struct Stats {
    pub files: usize,
    pub folders: usize,
    pub photo: usize,
    pub video: usize,
    pub gif: usize,
    pub document: usize,
    pub audio: usize,
    pub other: usize,
    pub bytes: u64,
}

pub fn stats(idx: &Index) -> Stats {
    let mut s = Stats { files: idx.files.len(), folders: idx.dirs.len(), ..Default::default() };
    for e in &idx.files {
        s.bytes += e.size;
        match e.kind {
            Kind::Photo => s.photo += 1,
            Kind::Video => s.video += 1,
            Kind::Gif => s.gif += 1,
            Kind::Document => s.document += 1,
            Kind::Audio => s.audio += 1,
            _ => s.other += 1,
        }
    }
    s
}

/// Breadcrumb trail (id, display name) for a folder id, root excluded.
pub fn crumbs(idx: &Index, folder_id: &str) -> Vec<(String, String)> {
    let Some(path) = idx.folder_path(folder_id) else { return Vec::new() };
    let mut out = Vec::new();
    let mut end = 0;
    for part in path.split('/').filter(|p| !p.is_empty()) {
        end += part.len() + if end == 0 { 0 } else { 1 };
        out.push((id_str(id_for(&path[..end])), display_safe(part)));
    }
    out
}

pub fn subfolders<'a>(idx: &'a Index, parent_id: &str) -> Vec<&'a Entry> {
    let Some(parent) = idx.folder_path(parent_id) else { return Vec::new() };
    let mut v: Vec<&Entry> = idx.dirs.iter().filter(|e| e.parent() == parent).collect();
    v.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> tempdir::Dir {
        let d = tempdir::Dir::new();
        for p in [
            "familia/hermana/laura-playa-2024.jpg",
            "fotos/hermana_cumpleaños.JPG",
            "fotos/IMG_2.png",
            "fotos/IMG_10.png",
            "videos/clip.mov",
            "docs/notes.pdf",
            "docs/anim.gif",
            ".hidden/secret.jpg",
            "Mori/Mori.app/Contents/MacOS/mori",
            "Fototeca.photoslibrary/originals/a.jpg",
            "Thumbs.db",
        ] {
            let full = d.0.join(p);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, b"x").unwrap();
        }
        d
    }

    mod tempdir {
        pub struct Dir(pub std::path::PathBuf);
        impl Dir {
            pub fn new() -> Self {
                // Tests run in parallel: a process-wide counter keeps every tree separate.
                static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let p = std::env::temp_dir().join(format!("mori-test-{}-{}-{}", std::process::id(), n, rand_bits()));
                std::fs::create_dir_all(&p).unwrap();
                Dir(p)
            }
        }
        fn rand_bits() -> u32 {
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos()
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    fn scan_tree(d: &tempdir::Dir) -> Index {
        scan(&d.0, &|| false, &mut |_, _| {}, None).unwrap()
    }

    fn names(r: &QueryResult) -> Vec<String> {
        r.items.iter().map(|e| e.path.clone()).collect()
    }

    #[test]
    fn scan_skips_hidden_bundles_and_junk() {
        let d = tree();
        let idx = scan_tree(&d);
        let paths: Vec<_> = idx.files.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(idx.files.len(), 7, "{paths:?}");
        assert!(!paths.iter().any(|p| p.contains("hidden") || p.contains(".app") || p.contains("Thumbs")));
        assert!(idx.dirs.iter().any(|e| e.path == "familia/hermana"));
    }

    #[test]
    fn search_matches_name_folder_and_partial_case_insensitive() {
        let d = tree();
        let idx = scan_tree(&d);
        let q = |s: &str| Query { search: s.into(), kind: "all".into(), global: true, ..Default::default() };
        let r = query(&idx, &q("hermana"));
        let n = names(&r);
        assert!(n.contains(&"familia/hermana/laura-playa-2024.jpg".to_string()));
        assert!(n.contains(&"fotos/hermana_cumpleaños.JPG".to_string()));
        assert!(n.contains(&"familia/hermana".to_string()), "folder itself matches");
        assert_eq!(query(&idx, &q("HERM")).items.len(), 3);
        assert_eq!(query(&idx, &q("cumpleanos")).items.len(), 1, "accent-insensitive");
        assert_eq!(query(&idx, &q("jpg playa")).items.len(), 1, "multi-token AND");
        assert_eq!(query(&idx, &q(".pdf")).items.len(), 1, "extension");
        let mut photos = q("hermana");
        photos.kind = "photo".into();
        assert_eq!(query(&idx, &photos).items.len(), 2, "kind filter drops folders");
    }

    #[test]
    fn folder_browse_and_natural_sort() {
        let d = tree();
        let idx = scan_tree(&d);
        let fotos = id_str(id_for("fotos"));
        let r = query(&idx, &Query { folder: fotos.clone(), sort: "name".into(), ..Default::default() });
        assert_eq!(names(&r), ["fotos/hermana_cumpleaños.JPG", "fotos/IMG_2.png", "fotos/IMG_10.png"]);
        let root = query(&idx, &Query::default());
        assert_eq!(names(&root), ["docs", "familia", "fotos", "Mori", "videos"]);
        let lib = query(&idx, &Query { scope: "library".into(), kind: "video".into(), ..Default::default() });
        assert_eq!(names(&lib), ["videos/clip.mov"]);
        assert_eq!(stats(&idx).gif, 1);
        assert!(query(&idx, &Query { folder: "../etc".into(), ..Default::default() }).items.is_empty());
        let deep = id_str(id_for("familia/hermana"));
        assert_eq!(
            crumbs(&idx, &deep),
            vec![(id_str(id_for("familia")), "familia".into()), (deep.clone(), "hermana".into())]
        );
        assert_eq!(idx.get(&fotos).unwrap().path, "fotos");
    }

    fn family_tree() -> tempdir::Dir {
        let d = tempdir::Dir::new();
        for p in [
            "Photos/Family/2024/beach.jpg",
            "Photos/Family/2024/dinner.jpg",
            "Photos/Family/2025/birthday.jpg",
            "Photos/Family/2025/party.mp4",
            "Photos/Family/sister.jpg",
            "Photos/Travel/Japan/tokyo.jpg",
            "Photos/Travel/family-trip.jpg",
            "Photos/random.jpg",
        ] {
            let full = d.0.join(p);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, b"x").unwrap();
        }
        d
    }

    #[test]
    fn include_subfolders_is_scoped_to_the_current_folder() {
        let d = family_tree();
        let idx = scan_tree(&d);
        let family = id_str(id_for("Photos/Family"));
        let base = Query { folder: family.clone(), sort: "name".into(), kind: "all".into(), ..Default::default() };

        // Normal view: direct children only.
        let flat = query(&idx, &base);
        assert_eq!(names(&flat), ["Photos/Family/2024", "Photos/Family/2025", "Photos/Family/sister.jpg"]);

        // Include subfolders: every file below Family, nothing from Travel, no folders.
        let rec = query(&idx, &Query { recursive: true, ..base.clone() });
        let mut got = names(&rec);
        got.sort();
        assert_eq!(
            got,
            [
                "Photos/Family/2024/beach.jpg",
                "Photos/Family/2024/dinner.jpg",
                "Photos/Family/2025/birthday.jpg",
                "Photos/Family/2025/party.mp4",
                "Photos/Family/sister.jpg"
            ]
        );
        assert_eq!(rec.items.len(), 5);
        assert!(rec.items.iter().all(|e| e.path.starts_with("Photos/Family/")));
        assert_eq!(rec.base, "Photos/Family");
        let locs: Vec<_> = rec.items.iter().map(|e| location(e, rec.base)).collect();
        assert!(locs.contains(&"2024") && locs.contains(&"2025") && locs.contains(&""));

        // Filters respect the recursive scope.
        let vids = query(&idx, &Query { recursive: true, kind: "video".into(), ..base.clone() });
        assert_eq!(names(&vids), ["Photos/Family/2025/party.mp4"]);

        // Folder search: relative to Family, so "family" doesn't match everything,
        // and Travel/family-trip.jpg is out of scope.
        let s = |text: &str, recursive: bool, global: bool| {
            names(&query(&idx, &Query { search: text.into(), recursive, global, ..base.clone() }))
        };
        assert_eq!(s("sister", false, false), ["Photos/Family/sister.jpg"]);
        assert_eq!(s("birthday", false, false), Vec::<String>::new(), "not a direct child");
        assert_eq!(s("birthday", true, false), ["Photos/Family/2025/birthday.jpg"]);
        assert_eq!(s("2025", true, false), ["Photos/Family/2025/birthday.jpg", "Photos/Family/2025/party.mp4"]);
        assert_eq!(s("family", true, false), Vec::<String>::new());
        assert!(s("family", true, true).contains(&"Photos/Travel/family-trip.jpg".to_string()), "global search");
        // A drive-wide search lists matching folders too, even with "Include subfolders" on.
        assert!(s("family", true, true).contains(&"Photos/Family".to_string()), "global search shows folders");
        // Same from the drive root (folder ""), as the UI sends it.
        let at_root = names(&query(
            &idx,
            &Query { folder: String::new(), search: "family".into(), recursive: true, global: true, ..base.clone() },
        ));
        assert!(at_root.contains(&"Photos/Family".to_string()), "{at_root:?}");

        // A type filter never hides subfolders while browsing one level.
        let photos_flat = query(&idx, &Query { kind: "photo".into(), ..base.clone() });
        assert_eq!(names(&photos_flat), ["Photos/Family/2024", "Photos/Family/2025", "Photos/Family/sister.jpg"]);

        // Sorting applies to the combined results.
        let by_name_desc = query(&idx, &Query { recursive: true, desc: true, ..base.clone() });
        assert_eq!(by_name_desc.items[0].name, "sister.jpg");
        assert_eq!(by_name_desc.items[4].name, "beach.jpg");

        // Newest first across all subfolders.
        let set = |p: &str, secs: u64| {
            let f = fs::File::options().write(true).open(d.0.join(p)).unwrap();
            f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)).unwrap();
        };
        set("Photos/Family/2024/beach.jpg", 1_700_000_000);
        set("Photos/Family/2025/birthday.jpg", 1_800_000_000);
        set("Photos/Family/sister.jpg", 1_750_000_000);
        let idx2 = scan_tree(&d);
        let newest = query(
            &idx2,
            &Query { recursive: true, sort: "modified".into(), desc: true, kind: "photo".into(), ..base.clone() },
        );
        let order: Vec<_> = newest.items.iter().map(|e| e.name.as_str()).collect();
        let pos = |n: &str| order.iter().position(|x| *x == n).unwrap();
        assert!(pos("birthday.jpg") < pos("sister.jpg") && pos("sister.jpg") < pos("beach.jpg"), "{order:?}");

        // Entering 2025 moves the recursive root.
        let y2025 = id_str(id_for("Photos/Family/2025"));
        let inner = query(&idx, &Query { folder: y2025, recursive: true, ..base.clone() });
        assert_eq!(names(&inner), ["Photos/Family/2025/birthday.jpg", "Photos/Family/2025/party.mp4"]);
        // Root + recursive = everything.
        assert_eq!(query(&idx, &Query { folder: String::new(), recursive: true, ..base.clone() }).items.len(), 8);
    }

    #[test]
    fn index_round_trips() {
        let d = tree();
        let idx = scan_tree(&d);
        let f = d.0.join("idx.json");
        save(&f, &idx).unwrap();
        let back = load(&f, &d.0).unwrap();
        assert_eq!(back.files.len(), idx.files.len());
        assert!(!back.files[0].key.is_empty());
    }
}
