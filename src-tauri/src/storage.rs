//! Storage: where the space goes (by type, by year, largest items, a folder
//! treemap) and empty folders. Everything here is computed from the
//! in-memory index — no file is opened — except the on-disk check that an
//! "empty" folder really is empty before it can be moved to the Trash.
//!
//! Contents of private folders are not counted or listed (they show up as
//! an unmeasured "Private" block), like every other drive-wide view.

use crate::index::{Entry, Index, Item, Kind};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

const TOP: usize = 100;

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Bucket {
    pub key: String,
    pub bytes: u64,
    pub count: u64,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FolderSize {
    pub id: String,
    pub name: String,
    pub path: String,
    pub bytes: u64,
    pub files: u64,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Tile {
    /// "folder" | "file" | "files" (the remaining files, grouped) | "private".
    pub kind: &'static str,
    pub id: String,
    pub name: String,
    pub bytes: u64,
    pub files: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub total_bytes: u64,
    pub total_files: u64,
    pub by_kind: Vec<Bucket>,
    pub by_year: Vec<Bucket>,
    pub largest_files: Vec<Item>,
    pub largest_videos: Vec<Item>,
    pub largest_images: Vec<Item>,
    pub largest_folders: Vec<FolderSize>,
    /// The treemap of `folder`: its subfolders and biggest files.
    pub folder: String,
    pub folder_bytes: u64,
    pub tiles: Vec<Tile>,
    /// Private folders on the drive (their contents are not measured).
    pub private_folders: u64,
}

fn year_of(ms: i64) -> i64 {
    // Civil year from days since 1970 (UTC).
    let days = ms.div_euclid(86_400_000);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let y = yoe + era * 400;
    if mp >= 10 {
        y + 1
    } else {
        y
    }
}

fn top_by_size<'a>(it: impl Iterator<Item = &'a Entry>, n: usize) -> Vec<Item> {
    let mut v: Vec<&Entry> = it.collect();
    if v.len() > n {
        v.select_nth_unstable_by(n, |a, b| b.size.cmp(&a.size));
        v.truncate(n);
    }
    v.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.path.cmp(&b.path)));
    v.into_iter().map(Item::from).collect()
}

fn parent(p: &str) -> &str {
    p.rfind('/').map_or("", |i| &p[..i])
}

/// `folder`: relative path of the treemap folder ("" = the drive).
pub fn report(idx: &Index, folder: &str) -> Report {
    let visible: Vec<&Entry> = idx.files.iter().filter(|e| e.visible_from("") && e.kind != Kind::Link).collect();
    let mut kinds: HashMap<&'static str, (u64, u64)> = HashMap::new();
    let mut years: HashMap<i64, (u64, u64)> = HashMap::new();
    // Recursive size of every folder that holds visible files.
    let mut dirs: HashMap<&str, (u64, u64)> = HashMap::new();
    let mut total = 0;
    for e in &visible {
        total += e.size;
        let k = match e.kind {
            Kind::Photo => "photo",
            Kind::Video => "video",
            Kind::Gif => "gif",
            Kind::Document => "document",
            Kind::Audio => "audio",
            _ => "other",
        };
        let s = kinds.entry(k).or_default();
        s.0 += e.size;
        s.1 += 1;
        let y = years.entry(year_of(e.created.unwrap_or(e.modified).min(e.modified.max(1)))).or_default();
        y.0 += e.size;
        y.1 += 1;
        for (i, _) in e.path.match_indices('/') {
            let d = dirs.entry(&e.path[..i]).or_default();
            d.0 += e.size;
            d.1 += 1;
        }
    }
    let mut by_kind: Vec<Bucket> =
        kinds.into_iter().map(|(k, (b, c))| Bucket { key: k.into(), bytes: b, count: c }).collect();
    by_kind.sort_by_key(|b| std::cmp::Reverse(b.bytes));
    let mut by_year: Vec<Bucket> =
        years.into_iter().map(|(y, (b, c))| Bucket { key: y.to_string(), bytes: b, count: c }).collect();
    by_year.sort_by(|a, b| b.key.cmp(&a.key));

    let name_of = |p: &str| p.rsplit('/').next().unwrap_or(p).to_owned();
    let private: HashSet<&str> = idx.dirs.iter().filter(|d| d.private).map(|d| d.path.as_str()).collect();
    let mut folders: Vec<FolderSize> = dirs
        .iter()
        .map(|(p, (b, c))| FolderSize {
            id: crate::index::id_str(crate::index::id_for(p)),
            name: name_of(p),
            path: p.to_string(),
            bytes: *b,
            files: *c,
        })
        .collect();
    folders.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    folders.truncate(TOP / 2);

    // Treemap of `folder`.
    let mut tiles = Vec::new();
    for d in idx.dirs.iter().filter(|d| parent(&d.path) == folder && d.visible_from(folder)) {
        if private.contains(d.path.as_str()) {
            tiles.push(Tile {
                kind: "private",
                id: crate::index::id_str(d.id),
                name: d.name.clone(),
                bytes: 0,
                files: 0,
            });
        } else if let Some((b, c)) = dirs.get(d.path.as_str()) {
            tiles.push(Tile {
                kind: "folder",
                id: crate::index::id_str(d.id),
                name: d.name.clone(),
                bytes: *b,
                files: *c,
            });
        }
    }
    let mut here: Vec<&&Entry> = visible.iter().filter(|e| parent(&e.path) == folder).collect();
    here.sort_by_key(|e| std::cmp::Reverse(e.size));
    for e in here.iter().take(30) {
        tiles.push(Tile {
            kind: "file",
            id: crate::index::id_str(e.id),
            name: e.name.clone(),
            bytes: e.size,
            files: 1,
        });
    }
    if here.len() > 30 {
        let rest = &here[30..];
        tiles.push(Tile {
            kind: "files",
            id: String::new(),
            name: format!("{} more files", rest.len()),
            bytes: rest.iter().map(|e| e.size).sum(),
            files: rest.len() as u64,
        });
    }
    tiles.retain(|t| t.bytes > 0 || t.kind == "private");
    tiles.sort_by_key(|t| std::cmp::Reverse(t.bytes));
    let folder_bytes = if folder.is_empty() { total } else { dirs.get(folder).map_or(0, |d| d.0) };

    Report {
        total_bytes: total,
        total_files: visible.len() as u64,
        by_kind,
        by_year,
        largest_files: top_by_size(visible.iter().copied(), TOP),
        largest_videos: top_by_size(visible.iter().copied().filter(|e| e.kind == Kind::Video), TOP / 2),
        largest_images: top_by_size(
            visible.iter().copied().filter(|e| matches!(e.kind, Kind::Photo | Kind::Gif)),
            TOP / 2,
        ),
        largest_folders: folders,
        folder: folder.to_owned(),
        folder_bytes,
        tiles,
        private_folders: private.len() as u64,
    }
}

// ------------------------------------------------------------ empty folders

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct EmptyFolder {
    pub id: String,
    pub name: String,
    pub path: String,
    /// Empty subfolders inside it (removed with it).
    pub nested: u64,
    /// Inside a "Never Modify" folder: listed, but Mori won't remove it.
    pub protected: bool,
    /// What the on-disk check found ("" = empty).
    pub note: String,
}

/// Folders that hold no files (in the index), topmost only, outside private
/// folders. Each is then checked on disk.
pub fn empty_folders(idx: &Index, root: &Path) -> Vec<EmptyFolder> {
    let mut used: HashSet<&str> = HashSet::new();
    for e in &idx.files {
        for (i, _) in e.path.match_indices('/') {
            used.insert(&e.path[..i]);
        }
    }
    let empty: Vec<&Entry> =
        idx.dirs.iter().filter(|d| d.visible_from("") && !d.private && !used.contains(d.path.as_str())).collect();
    let set: HashSet<&str> = empty.iter().map(|d| d.path.as_str()).collect();
    let mut out: Vec<EmptyFolder> = empty
        .iter()
        .filter(|d| !set.contains(parent(&d.path)))
        .map(|d| {
            let prefix = format!("{}/", d.path);
            let note = match verify_empty(&root.join(&d.path)) {
                Ok(()) => String::new(),
                Err(e) => e,
            };
            EmptyFolder {
                id: crate::index::id_str(d.id),
                name: d.name.clone(),
                path: d.path.clone(),
                nested: set.iter().filter(|p| p.starts_with(&prefix)).count() as u64,
                protected: d.guarded,
                note,
            }
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// System clutter that doesn't make a folder "non-empty".
fn is_junk(name: &str, len: u64) -> bool {
    matches!(name, ".DS_Store" | ".localized" | "Thumbs.db" | "ehthumbs.db" | "desktop.ini" | "Icon\r")
        || (name.starts_with("._") && len <= 4096)
}

/// Re-check on disk, without following links, that a folder holds nothing
/// but empty subfolders and system clutter. The index hides dotfiles, so a
/// folder with only `.git` inside looks empty there — this catches it.
pub fn verify_empty(dir: &Path) -> Result<(), String> {
    fn walk(dir: &Path, depth: usize, seen: &mut usize) -> Result<(), String> {
        if depth > 32 {
            return Err("too deeply nested to check".into());
        }
        let meta = fs::symlink_metadata(dir).map_err(|_| "no longer exists".to_string())?;
        if !meta.is_dir() {
            return Err("no longer a folder".into());
        }
        for item in fs::read_dir(dir).map_err(|_| "can't be read".to_string())? {
            *seen += 1;
            if *seen > 10_000 {
                return Err("too many items to check".into());
            }
            let item = item.map_err(|_| "can't be read".to_string())?;
            let name = item.file_name().to_string_lossy().into_owned();
            let m = fs::symlink_metadata(item.path()).map_err(|_| "can't be read".to_string())?;
            if m.file_type().is_symlink() {
                return Err(format!("contains a link (“{}”)", crate::secure::display_safe(&name)));
            }
            if m.is_dir() {
                walk(&item.path(), depth + 1, seen)?;
            } else if !(m.is_file() && is_junk(&name, m.len())) {
                return Err(format!("contains “{}”", crate::secure::display_safe(&name)));
            }
        }
        Ok(())
    }
    walk(dir, 0, &mut 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lab() -> std::path::PathBuf {
        let d =
            std::env::temp_dir().join(format!("mori-storage-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        fs::canonicalize(d).unwrap()
    }

    #[test]
    fn report_aggregates_without_private_contents() {
        let d = lab();
        fs::create_dir_all(d.join("Photos/2024")).unwrap();
        fs::create_dir_all(d.join("Secret")).unwrap();
        fs::write(d.join("Photos/2024/a.jpg"), vec![0u8; 3000]).unwrap();
        fs::write(d.join("Photos/b.mp4"), vec![0u8; 5000]).unwrap();
        fs::write(d.join("notes.txt"), vec![0u8; 100]).unwrap();
        fs::write(d.join("Secret/hidden.jpg"), vec![0u8; 9000]).unwrap();
        let mut idx = crate::index::scan(&d, &|| false, &mut |_, _| {}, None).unwrap();
        idx.apply_boundaries(&["Secret".to_string()].into());
        let r = report(&idx, "");
        assert_eq!(r.total_bytes, 8100, "the private folder's contents are not counted");
        assert_eq!(r.largest_files[0].name, "b.mp4");
        assert_eq!(r.largest_videos.len(), 1);
        assert_eq!(r.largest_images[0].name, "a.jpg");
        assert_eq!(r.largest_folders[0].path, "Photos");
        assert_eq!(r.largest_folders[0].bytes, 8000);
        assert!(r.tiles.iter().any(|t| t.kind == "private" && t.name == "Secret" && t.bytes == 0));
        assert_eq!(r.tiles[0].name, "Photos");
        assert_eq!(r.private_folders, 1);
        let sub = report(&idx, "Photos");
        assert_eq!(sub.folder_bytes, 8000);
        assert_eq!(sub.tiles.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["b.mp4", "2024"]);
        assert_eq!(by_key(&r.by_kind, "video"), 5000);
        fs::remove_dir_all(d).unwrap();
    }

    fn by_key(b: &[Bucket], k: &str) -> u64 {
        b.iter().find(|x| x.key == k).map_or(0, |x| x.bytes)
    }

    #[cfg(unix)]
    #[test]
    fn empty_folders_are_verified_on_disk() {
        let d = lab();
        fs::create_dir_all(d.join("Empty/Inner/Deeper")).unwrap();
        fs::write(d.join("Empty/.DS_Store"), b"junk").unwrap();
        fs::create_dir_all(d.join("Repo/.git")).unwrap();
        fs::write(d.join("Repo/.git/HEAD"), b"ref").unwrap();
        fs::create_dir_all(d.join("Linked")).unwrap();
        std::os::unix::fs::symlink("/", d.join("Linked/root")).unwrap();
        fs::create_dir_all(d.join("Full")).unwrap();
        fs::write(d.join("Full/a.txt"), b"x").unwrap();
        let idx = crate::index::scan(&d, &|| false, &mut |_, _| {}, None).unwrap();
        let e = empty_folders(&idx, &d);
        let get = |p: &str| e.iter().find(|x| x.path == p);
        let empty = get("Empty").expect("listed");
        assert_eq!(empty.nested, 2, "Inner and Deeper go with it");
        assert_eq!(empty.note, "", "only clutter inside");
        assert!(get("Empty/Inner").is_none(), "topmost only");
        assert!(get("Full").is_none());
        assert!(get("Repo").unwrap().note.contains("HEAD"), "hidden content is found on disk");
        // A link alone isn't in the index as a file? It is (as a link) — not empty either way.
        assert!(get("Linked").is_none_or(|x| !x.note.is_empty()));
        assert!(verify_empty(&d.join("Linked")).is_err());
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn years() {
        assert_eq!(year_of(0), 1970);
        assert_eq!(year_of(1_704_067_199_000), 2023); // 2023-12-31 23:59:59
        assert_eq!(year_of(1_704_067_200_000), 2024);
    }
}
