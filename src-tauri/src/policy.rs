//! The mutation policy: the single gate every change to the user's files
//! goes through. `fileops` functions require a `&Policy`, so no code path
//! can move, rename or delete anything without these checks.
//!
//! - **Read-only Mode** (a setting) rejects every mutation.
//! - **Protected folders** ("Never Modify") reject any change to the folder,
//!   anything inside it, or any folder that contains one.
//!
//! Mori's own app state (caches, index, settings, private/protected marks)
//! is not the user's filesystem and is not governed here.

use crate::privacy;
use std::path::Path;

pub const READ_ONLY: &str = "Read-only Mode is enabled.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Trash,
    Rename,
    Delete,
    Overwrite,
    /// Creating a new file (e.g. a sanitized copy) inside a folder.
    Create,
    /// Putting a trashed item back (undo).
    Restore,
    /// Moving an item to another folder (the source side; the destination
    /// is checked as `Create`).
    Move,
}

pub struct Policy<'a> {
    pub read_only: bool,
    pub protected: &'a privacy::Store,
}

fn name(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

impl Policy<'_> {
    /// `path` is the item acted on (for `Create`/`Restore`: the path being
    /// created). Canonical paths only.
    pub fn check(&self, op: Op, path: &Path) -> Result<(), String> {
        if self.read_only {
            return Err(READ_ONLY.into());
        }
        let target = match op {
            Op::Create | Op::Restore => path.parent().unwrap_or(path),
            _ => path,
        };
        if let Some(folder) = self.protected.covering(target) {
            return Err(format!(
                "“{}” is protected (Never Modify), so Mori won't change anything inside it.",
                name(&folder)
            ));
        }
        if matches!(op, Op::Trash | Op::Rename | Op::Delete | Op::Move) && self.protected.any_below(path) {
            return Err("This folder contains a protected folder (Never Modify), so Mori won't change it.".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn read_only_and_protected_ancestry_are_enforced() {
        let base = fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("mori-policy-{}", std::process::id()));
        fs::create_dir_all(base.join("Photos/Archive/2018")).unwrap();
        fs::create_dir_all(base.join("Photos/Loose")).unwrap();
        let store = privacy::Store::load(base.join("protected.json"));
        store.set(&base.join("Photos/Archive"), 1, true).unwrap();
        let p = Policy { read_only: false, protected: &store };
        let photo = base.join("Photos/Archive/2018/photo.jpg");
        assert!(p.check(Op::Trash, &photo).unwrap_err().contains("Archive"));
        assert!(p.check(Op::Delete, &base.join("Photos/Archive")).is_err());
        assert!(p.check(Op::Rename, &base.join("Photos/Archive/2018")).is_err());
        // An ancestor of a protected folder can't be trashed or renamed either.
        assert!(p.check(Op::Trash, &base.join("Photos")).unwrap_err().contains("contains a protected"));
        // Creating inside it is refused; elsewhere is fine.
        assert!(p.check(Op::Create, &base.join("Photos/Archive/new.jpg")).is_err());
        assert!(p.check(Op::Create, &base.join("Photos/Loose/new.jpg")).is_ok());
        assert!(p.check(Op::Trash, &base.join("Photos/Loose")).is_ok());
        // Moving out of (or a folder containing) a protected folder is refused.
        assert!(p.check(Op::Move, &photo).is_err());
        assert!(p.check(Op::Move, &base.join("Photos")).unwrap_err().contains("contains a protected"));
        assert!(p.check(Op::Move, &base.join("Photos/Loose")).is_ok());
        let ro = Policy { read_only: true, protected: &store };
        assert_eq!(ro.check(Op::Trash, &base.join("Photos/Loose")).unwrap_err(), READ_ONLY);
        fs::remove_dir_all(base).unwrap();
    }
}
