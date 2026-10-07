//! Unfinished Quick Cleanup sessions, so one can be resumed after Mori quits.
//!
//! Quick Cleanup never changes a file while you review: Keep / Mark for
//! Trash decisions live in the UI until the final, confirmed "Move to Trash"
//! (which goes through `trash_items` like every other Trash operation). What
//! is saved here is only enough to resume: the folder's opaque id, the
//! options, the opaque ids of decided items and the position. No names, no
//! paths. Roots and volumes are stored as hashes.
//!
//! Never used in a temporary session or Private Inspection (the caller
//! refuses, see `AppState::persistent`): their cleanup decisions stay in
//! memory and end with the session.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

const VERSION: u32 = 1;
/// Sessions kept at most (the oldest go first).
const MAX_SESSIONS: usize = 16;
/// Decisions kept per session at most.
const MAX_DECISIONS: usize = 200_000;

pub const KINDS: &[&str] = &["all", "photo", "video", "gif", "document", "audio", "other"];
pub const ORDERS: &[&str] = &["browser", "oldest", "newest", "largest", "smallest"];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Options {
    pub recursive: bool,
    pub kind: String,
    pub order: String,
}

/// What the UI sends and gets back.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    /// Folder id ("" = the root).
    pub folder: String,
    pub options: Options,
    pub kept: Vec<String>,
    pub marked: Vec<String>,
    /// Index of the item being reviewed.
    pub cursor: usize,
    #[serde(default)]
    pub saved_at: i64,
}

#[derive(Serialize, Deserialize, Clone)]
struct Saved {
    /// Hash of the canonical root folder.
    root: String,
    /// Hash of its volume (so "Forget This Drive" can drop it).
    vol: String,
    session: Session,
}

#[derive(Serialize, Deserialize, Default)]
struct Data {
    version: u32,
    sessions: Vec<Saved>,
}

pub struct Store {
    file: PathBuf,
    data: Mutex<Data>,
}

fn opaque_id(s: &str) -> bool {
    s.len() == 16 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Reject anything that isn't exactly the minimal record described above.
pub fn validate(s: &Session) -> Result<(), String> {
    let bad = "Invalid cleanup session.";
    if !(s.folder.is_empty() || opaque_id(&s.folder)) {
        return Err(bad.into());
    }
    if !KINDS.contains(&s.options.kind.as_str()) || !ORDERS.contains(&s.options.order.as_str()) {
        return Err(bad.into());
    }
    if s.kept.len() + s.marked.len() > MAX_DECISIONS || !s.kept.iter().chain(&s.marked).all(|i| opaque_id(i)) {
        return Err(bad.into());
    }
    if s.cursor > MAX_DECISIONS * 4 {
        return Err(bad.into());
    }
    Ok(())
}

impl Store {
    pub fn load(file: PathBuf) -> Store {
        let data = fs::read(&file)
            .ok()
            .and_then(|b| serde_json::from_slice::<Data>(&b).ok())
            .filter(|d| d.version == VERSION)
            .map(|mut d| {
                d.sessions.retain(|s| validate(&s.session).is_ok());
                d
            })
            .unwrap_or(Data { version: VERSION, sessions: Vec::new() });
        Store { file, data: Mutex::new(data) }
    }

    fn save(&self, d: &Data) -> Result<(), String> {
        let fail = |_| "Could not save the cleanup session.".to_string();
        if d.sessions.is_empty() {
            let _ = fs::remove_file(&self.file);
            return Ok(());
        }
        if let Some(dir) = self.file.parent() {
            fs::create_dir_all(dir).map_err(fail)?;
        }
        let tmp = self.file.with_extension("tmp");
        fs::write(&tmp, serde_json::to_vec(d).map_err(|_| "Could not save the cleanup session.")?).map_err(fail)?;
        fs::rename(tmp, &self.file).map_err(fail)
    }

    pub fn get(&self, root: &str, folder: &str) -> Option<Session> {
        let d = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        d.sessions.iter().find(|s| s.root == root && s.session.folder == folder).map(|s| s.session.clone())
    }

    pub fn put(&self, root: &str, vol: &str, mut session: Session) -> Result<(), String> {
        validate(&session)?;
        session.saved_at = crate::index::now_millis();
        let mut d = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        d.version = VERSION;
        d.sessions.retain(|s| !(s.root == root && s.session.folder == session.folder));
        d.sessions.push(Saved { root: root.to_owned(), vol: vol.to_owned(), session });
        if d.sessions.len() > MAX_SESSIONS {
            d.sessions.sort_by_key(|s| s.session.saved_at);
            let extra = d.sessions.len() - MAX_SESSIONS;
            d.sessions.drain(..extra);
        }
        self.save(&d)
    }

    pub fn discard(&self, root: &str, folder: &str) {
        let mut d = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        let before = d.sessions.len();
        d.sessions.retain(|s| !(s.root == root && s.session.folder == folder));
        if d.sessions.len() != before {
            let _ = self.save(&d);
        }
    }

    /// "Forget This Drive".
    pub fn forget_volume(&self, vol: &str) {
        let mut d = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        d.sessions.retain(|s| s.vol != vol);
        let _ = self.save(&d);
    }

    /// Clear Mori Data (the file itself is removed by `localdata`).
    pub fn clear(&self) {
        self.data.lock().unwrap_or_else(PoisonError::into_inner).sessions.clear();
        let _ = fs::remove_file(&self.file);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.data.lock().unwrap_or_else(PoisonError::into_inner).sessions.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(folder: &str) -> Session {
        Session {
            folder: folder.into(),
            options: Options { recursive: true, kind: "photo".into(), order: "oldest".into() },
            kept: vec!["00000000000000aa".into()],
            marked: vec!["00000000000000bb".into(), "00000000000000cc".into()],
            cursor: 3,
            saved_at: 0,
        }
    }

    fn file(name: &str) -> PathBuf {
        let f = std::env::temp_dir().join(format!("mori-cleanup-{name}-{}.json", std::process::id()));
        let _ = fs::remove_file(&f);
        f
    }

    #[test]
    fn sessions_persist_resume_and_discard() {
        let f = file("persist");
        let s = Store::load(f.clone());
        s.put("r1", "v1", session("0123456789abcdef")).unwrap();
        s.put("r1", "v1", session("")).unwrap();
        s.put("r2", "v2", session("")).unwrap();
        let again = Store::load(f.clone());
        let got = again.get("r1", "0123456789abcdef").unwrap();
        assert_eq!((got.cursor, got.marked.len(), got.kept.len()), (3, 2, 1));
        assert!(again.get("r1", "fedcba9876543210").is_none());
        again.discard("r1", "0123456789abcdef");
        assert!(Store::load(f.clone()).get("r1", "0123456789abcdef").is_none());
        again.forget_volume("v2");
        assert!(again.get("r2", "").is_none() && again.get("r1", "").is_some());
        again.clear();
        assert!(!f.exists());
    }

    #[test]
    fn only_opaque_ids_and_known_options_are_accepted() {
        let s = Store::load(file("validate"));
        let mut bad = session("");
        bad.marked.push("/Users/me/secret.jpg".into());
        assert!(s.put("r", "v", bad).is_err());
        let mut bad = session("../x");
        bad.folder = "../x".into();
        assert!(s.put("r", "v", bad).is_err());
        let mut bad = session("");
        bad.options.order = "random".into();
        assert!(s.put("r", "v", bad).is_err());
        assert_eq!(s.len(), 0);
        // What's written holds no names or paths: only hashes and hex ids.
        let f = file("contents");
        let s = Store::load(f.clone());
        s.put("abc123", "def456", session("0123456789abcdef")).unwrap();
        let text = fs::read_to_string(&f).unwrap();
        assert!(!text.contains('/') && !text.contains('\\'), "{text}");
        s.clear();
    }

    #[test]
    fn the_number_of_sessions_is_bounded() {
        let f = file("bounded");
        let s = Store::load(f.clone());
        for i in 0..(MAX_SESSIONS + 5) {
            s.put(&format!("r{i}"), "v", session("")).unwrap();
        }
        assert_eq!(s.len(), MAX_SESSIONS);
        s.clear();
    }
}
