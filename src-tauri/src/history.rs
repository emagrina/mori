//! Undo history for file operations made through Mori (this session only,
//! in memory). Undo is offered only where it can really be done:
//!
//! - **Move to Trash**: put back from the Trash when the platform reported
//!   where the item went (macOS) and the original place is still free.
//! - **Rename / move on the same volume**: rename back, never over an
//!   existing item.
//! - **Move to another volume** (a verified copy, then the original to the
//!   Trash): put the original back from the Trash, and only then move the
//!   copy to the Trash.
//! - **Created a file** (sanitized copy): move the copy to the Trash.
//! - **Deleted permanently / overwritten**: listed, but never undoable —
//!   Mori says so instead of pretending.

use crate::fileops;
use crate::policy::Policy;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

const MAX_RECORDS: usize = 200;

#[derive(Clone, Debug)]
pub enum Change {
    Trashed {
        original: PathBuf,
        trashed: Option<PathBuf>,
    },
    Renamed {
        from: PathBuf,
        to: PathBuf,
    },
    Created {
        path: PathBuf,
    },
    Deleted {
        path: PathBuf,
    },
    /// Moved across volumes: `to` is the copy, the original went to the Trash.
    CrossMoved {
        from: PathBuf,
        to: PathBuf,
        trashed: Option<PathBuf>,
    },
}

#[derive(Clone, Debug)]
pub struct Record {
    pub id: u64,
    pub at: i64,
    pub label: String,
    pub changes: Vec<Change>,
    pub undone: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordView {
    pub id: u64,
    pub at: i64,
    pub label: String,
    pub undone: bool,
    /// Can be undone now (as far as Mori can tell before trying).
    pub undoable: bool,
    /// Why not, when it can't.
    pub note: Option<String>,
}

#[derive(Default)]
pub struct History {
    records: Mutex<Vec<Record>>,
    next: Mutex<u64>,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UndoOutcome {
    pub restored: usize,
    pub failed: Vec<String>,
    /// Paths that changed (for refreshing views).
    #[serde(skip)]
    pub touched: Vec<PathBuf>,
    /// Items now back at their old path: (where they were, where they are
    /// again), so Mori's own records (privacy, favorites, tags) follow.
    #[serde(skip)]
    pub moved_back: Vec<(PathBuf, PathBuf)>,
}

fn why_not(changes: &[Change]) -> Option<String> {
    if changes.iter().any(|c| matches!(c, Change::Deleted { .. })) {
        return Some("Deleted permanently: this can't be undone.".into());
    }
    if changes
        .iter()
        .any(|c| matches!(c, Change::Trashed { trashed: None, .. } | Change::CrossMoved { trashed: None, .. }))
    {
        return Some(
            "Restore it from the system Trash (Mori wasn't told where the item went on this platform).".into(),
        );
    }
    None
}

impl History {
    pub fn record(&self, label: String, changes: Vec<Change>) {
        if changes.is_empty() {
            return;
        }
        let mut n = self.next.lock().unwrap_or_else(PoisonError::into_inner);
        *n += 1;
        let mut r = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        r.push(Record { id: *n, at: crate::index::now_millis(), label, changes, undone: false });
        if r.len() > MAX_RECORDS {
            r.remove(0);
        }
    }

    pub fn list(&self) -> Vec<RecordView> {
        let r = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        r.iter()
            .rev()
            .map(|x| {
                let note = why_not(&x.changes);
                RecordView {
                    id: x.id,
                    at: x.at,
                    label: x.label.clone(),
                    undone: x.undone,
                    undoable: !x.undone && note.is_none(),
                    note,
                }
            })
            .collect()
    }

    /// The newest record that can be undone.
    pub fn last_undoable(&self) -> Option<u64> {
        let r = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        r.iter().rev().find(|x| !x.undone && why_not(&x.changes).is_none()).map(|x| x.id)
    }

    pub fn undo(&self, id: u64, policy: &Policy) -> Result<UndoOutcome, String> {
        let rec = {
            let r = self.records.lock().unwrap_or_else(PoisonError::into_inner);
            r.iter().find(|x| x.id == id).cloned().ok_or("That operation is no longer in the history.")?
        };
        if rec.undone {
            return Err("Already undone.".into());
        }
        if let Some(n) = why_not(&rec.changes) {
            return Err(n);
        }
        let mut out = UndoOutcome::default();
        // Newest change first (e.g. a folder renamed after its file).
        for c in rec.changes.iter().rev() {
            let r = match c {
                Change::Trashed { original, trashed: Some(t) } => {
                    fileops::restore_from_trash(policy, t, original).map(|_| original.clone())
                }
                Change::Renamed { from, to } => fileops::rename_no_replace(policy, to, from).map(|_| {
                    out.moved_back.push((to.clone(), from.clone()));
                    from.clone()
                }),
                Change::Created { path } => fileops::move_to_trash(policy, path).map(|_| path.clone()),
                // The copy only goes once the original is safely back.
                Change::CrossMoved { from, to, trashed: Some(t) } => fileops::restore_from_trash(policy, t, from)
                    .and_then(|_| {
                        fileops::move_to_trash(policy, to)
                            .map_err(|e| format!("the original is back, but the copy stayed ({e})"))
                    })
                    .map(|_| from.clone()),
                Change::Trashed { trashed: None, .. }
                | Change::CrossMoved { trashed: None, .. }
                | Change::Deleted { .. } => continue,
            };
            match r {
                Ok(p) => {
                    out.restored += 1;
                    out.touched.push(p);
                }
                Err(e) => {
                    let name = match c {
                        Change::Trashed { original, .. } => original,
                        Change::Renamed { to, .. } | Change::CrossMoved { to, .. } => to,
                        Change::Created { path } | Change::Deleted { path } => path,
                    };
                    out.failed.push(format!(
                        "{}: {e}",
                        crate::secure::display_safe(
                            &name.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                        )
                    ));
                }
            }
        }
        if out.restored > 0 {
            let mut r = self.records.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(x) = r.iter_mut().find(|x| x.id == id) {
                x.undone = true;
            }
        }
        Ok(out)
    }

    pub fn clear(&self) {
        self.records.lock().unwrap_or_else(PoisonError::into_inner).clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn lab(name: &str) -> PathBuf {
        let d =
            fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("mori-history-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn rename_and_create_undo_and_permanent_delete_is_never_undoable() {
        let d = lab("rename");
        let store = crate::privacy::Store::load(d.join("protected.json"));
        let p = Policy { read_only: false, protected: &store };
        fs::write(d.join("a.txt"), b"a").unwrap();
        fileops::rename_no_replace(&p, &d.join("a.txt"), &d.join("b.txt")).unwrap();
        let h = History::default();
        h.record("Renamed".into(), vec![Change::Renamed { from: d.join("a.txt"), to: d.join("b.txt") }]);
        h.record("Deleted".into(), vec![Change::Deleted { path: d.join("gone.txt") }]);
        let list = h.list();
        assert!(!list[0].undoable && list[0].note.as_deref().unwrap().contains("can't be undone"));
        assert!(h.undo(list[0].id, &p).is_err());
        assert_eq!(h.last_undoable(), Some(list[1].id));
        let ro = Policy { read_only: true, protected: &store };
        assert_eq!(h.undo(list[1].id, &ro).unwrap().failed.len(), 1, "Read-only Mode refuses undo too");
        let r = h.undo(list[1].id, &p).unwrap();
        assert_eq!(r.restored, 1);
        assert!(d.join("a.txt").exists() && !d.join("b.txt").exists());
        assert!(h.undo(list[1].id, &p).is_err(), "only once");
        // Undo of a rename never replaces an item that took the old name.
        fileops::rename_no_replace(&p, &d.join("a.txt"), &d.join("c.txt")).unwrap();
        fs::write(d.join("a.txt"), b"new").unwrap();
        h.record("Renamed".into(), vec![Change::Renamed { from: d.join("a.txt"), to: d.join("c.txt") }]);
        let id = h.last_undoable().unwrap();
        assert_eq!(h.undo(id, &p).unwrap().failed.len(), 1);
        assert_eq!(fs::read(d.join("a.txt")).unwrap(), b"new");
        fs::remove_dir_all(&d).unwrap();
    }

    /// Real system Trash round trip (macOS reports the item's place in the Trash).
    #[cfg(target_os = "macos")]
    #[test]
    fn trash_then_undo_puts_the_item_back() {
        let d = lab("trash");
        let store = crate::privacy::Store::load(d.join("protected.json"));
        let p = Policy { read_only: false, protected: &store };
        fs::write(d.join("note.txt"), b"keep me").unwrap();
        let t = fileops::move_to_trash(&p, &d.join("note.txt")).unwrap();
        assert!(t.as_ref().is_some_and(|t| t.exists()), "{t:?}");
        assert!(!d.join("note.txt").exists());
        let h = History::default();
        h.record("Trashed".into(), vec![Change::Trashed { original: d.join("note.txt"), trashed: t }]);
        let r = h.undo(h.last_undoable().unwrap(), &p).unwrap();
        assert_eq!(r.restored, 1, "{:?}", r.failed);
        assert_eq!(fs::read(d.join("note.txt")).unwrap(), b"keep me");
        fs::remove_dir_all(&d).unwrap();
    }

    /// Undo of a move to another volume: the original comes back from the
    /// Trash first, and only then does the copy go to the Trash.
    #[cfg(target_os = "macos")]
    #[test]
    fn cross_volume_move_undo_restores_the_original_before_removing_the_copy() {
        let d = lab("xmove");
        let store = crate::privacy::Store::load(d.join("protected.json"));
        let p = Policy { read_only: false, protected: &store };
        fs::create_dir_all(d.join("dest")).unwrap();
        fs::write(d.join("photo.jpg"), b"original").unwrap();
        fileops::copy_item(&p, &d.join("photo.jpg"), &d.join("dest/photo.jpg"), &|| false).unwrap();
        let t = fileops::move_to_trash(&p, &d.join("photo.jpg")).unwrap();
        let h = History::default();
        h.record(
            "Moved".into(),
            vec![Change::CrossMoved { from: d.join("photo.jpg"), to: d.join("dest/photo.jpg"), trashed: t }],
        );
        // Something took the original's place: nothing is undone, the copy stays.
        fs::write(d.join("photo.jpg"), b"newcomer").unwrap();
        let id = h.last_undoable().unwrap();
        assert_eq!(h.undo(id, &p).unwrap().failed.len(), 1);
        assert!(d.join("dest/photo.jpg").exists(), "the copy is kept when the original can't come back");
        fs::remove_file(d.join("photo.jpg")).unwrap();
        let r = h.undo(id, &p).unwrap();
        assert_eq!(r.restored, 1, "{:?}", r.failed);
        assert_eq!(fs::read(d.join("photo.jpg")).unwrap(), b"original");
        assert!(!d.join("dest/photo.jpg").exists());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_cross_volume_move_without_a_known_trash_place_is_not_undoable() {
        let h = History::default();
        h.record("Moved".into(), vec![Change::CrossMoved { from: "/a".into(), to: "/b".into(), trashed: None }]);
        assert!(!h.list()[0].undoable);
        assert_eq!(h.last_undoable(), None);
    }
}
