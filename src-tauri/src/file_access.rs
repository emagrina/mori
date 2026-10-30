//! User-selected directory grants for App Sandbox. Paths are not permissions.
//!
//! Native URLs stay alive until the corresponding root/session is released.
//! Only the ordinary browser root gets a persistent app-scoped bookmark;
//! temporary/private inspections, analysis and copy destinations never do.
//! No broad folder entitlement, guessed path, or failed-bookmark fallback.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use tauri::Manager;

pub const BOOKMARK_FILE: &str = "sandbox-bookmark.json";
const MAX_BOOKMARK: usize = 1024 * 1024;
const MAX_RECORD: u64 = 6 * 1024 * 1024;
const MAX_GRANTS: usize = 128;

fn denied() -> String {
    tr!("That folder can't be opened")
}

/// In a Store build this is true even before signing, so tests/development
/// cannot accidentally rely on the current user's unrestricted filesystem.
pub fn required() -> bool {
    cfg!(feature = "mac-app-store") || native::sandboxed()
}

pub fn validate_store_process() -> Result<(), String> {
    if cfg!(feature = "mac-app-store") && !native::sandboxed() {
        return Err("Mac App Store build requires the signed App Sandbox entitlement".into());
    }
    Ok(())
}

/// The panic hook has no AppHandle; it must use the same container as setup.
#[cfg(target_os = "macos")]
pub fn sandbox_data_directory() -> Option<PathBuf> {
    native::directory(14).ok().map(|p| p.join(crate::localdata::IDENTIFIER))
}

/// Foundation resolves these directories inside the App Sandbox container.
/// Do not reach into the former non-sandboxed Application Support directory:
/// existing standalone data requires an explicit, separately reviewed migration.
pub fn app_directories(app: &tauri::AppHandle) -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    if required() {
        #[cfg(target_os = "macos")]
        return Ok((
            native::directory(14)?.join(crate::localdata::IDENTIFIER),
            native::directory(13)?.join(crate::localdata::IDENTIFIER),
        ));
        #[cfg(not(target_os = "macos"))]
        return Err(std::io::Error::other("Mac App Store build requires macOS").into());
    }
    Ok((app.path().app_data_dir()?, app.path().app_cache_dir()?))
}

struct Grant {
    root: PathBuf,
    #[cfg(target_os = "macos")]
    url: native::Scope,
}

#[derive(Default)]
struct Grants {
    root: Option<Arc<Grant>>,
    session: Vec<Arc<Grant>>,
}

/// Shared with the backend only. The webview receives neither bookmarks nor
/// the native URLs, and cannot manufacture a grant by sending a path.
pub struct Access {
    required: bool,
    file: PathBuf,
    grants: Mutex<Grants>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    bookmark: Vec<u8>,
}

impl Record {
    fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() as u64 > MAX_RECORD {
            return Err(denied());
        }
        let record: Self = serde_json::from_slice(bytes).map_err(|_| denied())?;
        if record.version != 1 || record.bookmark.is_empty() || record.bookmark.len() > MAX_BOOKMARK {
            return Err(denied());
        }
        Ok(record)
    }
}

impl Access {
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn grant_test_root(&self, root: &Path) {
        self.add(native::test_grant(root)).unwrap();
        self.set_root(root).unwrap();
    }

    pub fn new(data: &Path) -> Self {
        Self { required: required(), file: data.join(BOOKMARK_FILE), grants: Mutex::new(Grants::default()) }
    }

    fn grant(&self, path: &Path) -> Option<Arc<Grant>> {
        let g = self.grants.lock().unwrap_or_else(PoisonError::into_inner);
        g.root
            .iter()
            .chain(g.session.iter())
            .filter(|v| within(path, &v.root))
            .max_by_key(|v| v.root.components().count())
            .cloned()
    }

    pub fn permits(&self, path: &Path) -> bool {
        !self.required || self.grant(path).is_some()
    }

    pub fn require(&self, path: &Path) -> Result<(), String> {
        if self.permits(path) {
            Ok(())
        } else {
            Err(denied())
        }
    }

    /// Keep an owned lease while an asynchronous operation uses its files.
    /// Dropping a session can remove its authorisation without invalidating
    /// work that already holds a lease (no partially copied file on revocation).
    pub fn lease(&self, path: &Path) -> Result<Lease, String> {
        if !self.required {
            return Ok(Lease(None));
        }
        self.grant(path).map(|g| Lease(Some(g))).ok_or_else(denied)
    }

    /// A new request must acquire its own grant, even if a revoked grant is
    /// still retained by an older operation. The returned FD pins the opened file.
    pub fn open_inside(&self, root: &Path, rel: &str) -> Result<(fs::File, fs::Metadata, PathBuf), String> {
        let _lease = self.lease(root)?;
        crate::secure::open_inside(root, rel).map_err(|_| denied())
    }

    pub fn set_root(&self, path: &Path) -> Result<(), String> {
        if !self.required {
            return Ok(());
        }
        let new = self.grant(path).ok_or_else(denied)?;
        let mut g = self.grants.lock().unwrap_or_else(PoisonError::into_inner);
        g.session.retain(|v| !Arc::ptr_eq(v, &new));
        if let Some(old) = g.root.replace(new.clone()) {
            if !Arc::ptr_eq(&old, &new) {
                // Previous locations can still be referenced by analysis/Undo.
                g.session.push(old);
            }
        }
        Ok(())
    }

    pub fn clear_session(&self) {
        self.grants.lock().unwrap_or_else(PoisonError::into_inner).session.clear();
    }

    pub fn close_root(&self) {
        self.grants.lock().unwrap_or_else(PoisonError::into_inner).root = None;
    }

    /// Explicitly forgetting permissions is metadata-only, never user content.
    pub fn forget_saved(&self) -> Result<(), String> {
        match fs::remove_file(&self.file) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(tr!("Could not save settings")),
        }
    }

    pub fn remember_root(&self, path: &Path) -> Result<(), String> {
        if !self.required {
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        {
            let grant = self.grant(path).ok_or_else(denied)?;
            if grant.root != path {
                return Err(denied());
            }
            // Do not turn an ancestor grant into a bookmark for another folder.
            // The chooser's URL is the only authority we persist.
            write_record(&self.file, &native::bookmark(&grant.url)?)
        }
        #[cfg(not(target_os = "macos"))]
        Err(denied())
    }

    /// Resolve before touching the saved path. A moved bookmarked folder is
    /// followed by Foundation; a missing/stale/unusable bookmark never falls
    /// back to a similarly named folder or the legacy settings.root string.
    pub fn restore_root(&self) -> Result<Option<PathBuf>, String> {
        if !self.required || !self.file.exists() {
            return Ok(None);
        }
        let record = read_record(&self.file)?;
        #[cfg(target_os = "macos")]
        {
            let (grant, stale) = native::resolve(&record.bookmark)?;
            if stale {
                // Resolve + start access first. Refresh atomically; preserve
                // the previous bytes if refresh fails, never silently replace.
                write_record(&self.file, &native::bookmark(&grant.url)?)?;
            }
            let path = grant.root.clone();
            self.add(grant)?;
            self.set_root(&path)?;
            Ok(Some(path))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = record;
            Err(denied())
        }
    }

    fn add(&self, grant: Grant) -> Result<PathBuf, String> {
        let mut g = self.grants.lock().unwrap_or_else(PoisonError::into_inner);
        let path = grant.root.clone();
        // Re-selecting an existing root needs no new slot, even at the limit.
        // Drop the duplicate native grant immediately (balanced scope stop).
        if g.root.iter().chain(g.session.iter()).any(|v| v.root == path) {
            return Ok(path);
        }
        if g.session.len() + usize::from(g.root.is_some()) >= MAX_GRANTS {
            return Err(denied());
        }
        g.session.push(Arc::new(grant));
        Ok(path)
    }
}

/// Opaque retained resource; its final owner balances the security scope.
pub struct Lease(Option<Arc<Grant>>);
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self.0.take();
    }
}

fn within(path: &Path, root: &Path) -> bool {
    path.is_absolute()
        && !path.components().any(|c| matches!(c, std::path::Component::ParentDir))
        && path.starts_with(root)
}

fn read_record(path: &Path) -> Result<Record, String> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let f = options.open(path).map_err(|_| denied())?;
    let meta = f.metadata().map_err(|_| denied())?;
    if !meta.is_file() || meta.len() > MAX_RECORD {
        return Err(denied());
    }
    let mut bytes = Vec::new();
    f.take(MAX_RECORD + 1).read_to_end(&mut bytes).map_err(|_| denied())?;
    Record::decode(&bytes)
}

fn write_record(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    if bytes.is_empty() || bytes.len() > MAX_BOOKMARK {
        return Err(denied());
    }
    let parent = path.parent().ok_or_else(denied)?;
    if !fs::symlink_metadata(parent).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
        || fs::symlink_metadata(path).is_ok_and(|m| !m.is_file() || m.file_type().is_symlink())
    {
        return Err(denied());
    }
    let tmp = parent.join(format!(".mori-bookmark-{}-{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = options.open(&tmp).map_err(|_| tr!("Could not save settings"))?;
    let result = (|| {
        serde_json::to_writer(&mut f, &Record { version: 1, bookmark: bytes.to_vec() })?;
        f.flush()?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(|_| tr!("Could not save settings"))
}

/// Always use the original native NSURL in sandboxed builds. Converting a
/// panel result to a string loses the security scope and move-tracking data.
pub async fn pick_folder(app: &tauri::AppHandle, title: String) -> Result<PathBuf, String> {
    use tauri::Manager;
    let required = app.state::<crate::AppState>().file_access.required;
    #[cfg(target_os = "macos")]
    if required {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        app.run_on_main_thread(move || {
            let _ = tx.send(native::pick(&title));
        })
        .map_err(|_| denied())?;
        let grant = tauri::async_runtime::spawn_blocking(move || rx.recv())
            .await
            .map_err(|_| denied())?
            .map_err(|_| denied())??;
        return app.state::<crate::AppState>().file_access.add(grant);
    }
    if required {
        return Err(denied());
    }
    use tauri_plugin_dialog::DialogExt;
    let picked = app.dialog().file().set_title(title).blocking_pick_folder();
    picked.ok_or("cancelled")?.into_path().map_err(|_| denied())
}

#[cfg(not(target_os = "macos"))]
mod native {
    pub fn sandboxed() -> bool {
        false
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use std::ffi::{c_void, CStr, CString};
    use std::os::unix::ffi::OsStringExt;
    use std::ptr;

    #[link(name = "Security", kind = "framework")]
    extern "C" {
        fn SecTaskCreateFromSelf(allocator: *const c_void) -> *const c_void;
        fn SecTaskCopyValueForEntitlement(
            task: *const c_void,
            key: *const c_void,
            error: *mut *const c_void,
        ) -> *const c_void;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(value: *const c_void);
        fn CFGetTypeID(value: *const c_void) -> usize;
        fn CFBooleanGetTypeID() -> usize;
        fn CFBooleanGetValue(value: *const c_void) -> u8;
    }

    pub fn directory(kind: usize) -> Result<PathBuf, std::io::Error> {
        objc2::rc::autoreleasepool(|_| unsafe {
            let manager: *mut AnyObject = msg_send![class!(NSFileManager), defaultManager];
            let mut error: *mut AnyObject = ptr::null_mut();
            let url: *mut AnyObject = msg_send![manager, URLForDirectory: kind, inDomain: 1usize,
                appropriateForURL: ptr::null::<AnyObject>(), create: true, error: &mut error];
            if url.is_null() {
                return Err(std::io::Error::other("Sandbox data directory unavailable"));
            }
            let path: *const std::ffi::c_char = msg_send![url, fileSystemRepresentation];
            if path.is_null() {
                return Err(std::io::Error::other("Sandbox data directory unavailable"));
            }
            Ok(PathBuf::from(std::ffi::OsString::from_vec(CStr::from_ptr(path).to_bytes().to_vec())))
        })
    }

    pub fn sandboxed() -> bool {
        static VALUE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *VALUE.get_or_init(|| {
            objc2::rc::autoreleasepool(|_| unsafe {
                let task = SecTaskCreateFromSelf(ptr::null());
                if task.is_null() {
                    return false;
                }
                let key: *mut AnyObject =
                    msg_send![class!(NSString), stringWithUTF8String: c"com.apple.security.app-sandbox".as_ptr()];
                let value = SecTaskCopyValueForEntitlement(task, key.cast(), ptr::null_mut());
                let yes =
                    !value.is_null() && CFGetTypeID(value) == CFBooleanGetTypeID() && CFBooleanGetValue(value) != 0;
                if !value.is_null() {
                    CFRelease(value);
                }
                CFRelease(task);
                yes
            })
        })
    }

    /// NSURL is immutable and thread safe. The only scope start happens
    /// before publication; stop happens exactly once when the last Arc drops.
    pub struct Scope(Retained<AnyObject>);
    unsafe impl Send for Scope {}
    unsafe impl Sync for Scope {}
    impl Drop for Scope {
        fn drop(&mut self) {
            unsafe {
                let _: () = msg_send![&*self.0, stopAccessingSecurityScopedResource];
            }
        }
    }

    unsafe fn grant(url: Retained<AnyObject>) -> Result<Grant, String> {
        let scope = Scope(url);
        let p: *const std::ffi::c_char = msg_send![&*scope.0, fileSystemRepresentation];
        if p.is_null() {
            return Err(denied());
        }
        let path = PathBuf::from(std::ffi::OsString::from_vec(CStr::from_ptr(p).to_bytes().to_vec()));
        let canonical = fs::canonicalize(&path).map_err(|_| denied())?;
        if !canonical.is_dir() || !fs::symlink_metadata(&path).is_ok_and(|m| !m.file_type().is_symlink()) {
            return Err(denied());
        }
        Ok(Grant { root: canonical, url: scope })
    }

    pub fn pick(title: &str) -> Result<Grant, String> {
        objc2::rc::autoreleasepool(|_| unsafe {
            let panel: *mut AnyObject = msg_send![class!(NSOpenPanel), openPanel];
            if panel.is_null() {
                return Err(denied());
            }
            let title = CString::new(title).map_err(|_| denied())?;
            let label: *mut AnyObject = msg_send![class!(NSString), stringWithUTF8String: title.as_ptr()];
            let _: () = msg_send![panel, setTitle: label];
            let _: () = msg_send![panel, setCanChooseFiles: false];
            let _: () = msg_send![panel, setCanChooseDirectories: true];
            let _: () = msg_send![panel, setAllowsMultipleSelection: false];
            let _: () = msg_send![panel, setCanCreateDirectories: false];
            let _: () = msg_send![panel, setResolvesAliases: false];
            let result: isize = msg_send![panel, runModal];
            if result != 1 {
                return Err("cancelled".into());
            }
            let raw: *mut AnyObject = msg_send![panel, URL];
            let url = Retained::retain(raw).ok_or_else(denied)?;
            // Powerbox already starts the URL's scope. Own that one start and
            // balance it when the retained grant expires, including errors.
            grant(url)
        })
    }

    #[cfg(test)]
    pub(super) fn test_grant(path: &Path) -> Grant {
        objc2::rc::autoreleasepool(|_| unsafe {
            let value = CString::new(path.to_str().unwrap()).unwrap();
            let text: *mut AnyObject = msg_send![class!(NSString), stringWithUTF8String: value.as_ptr()];
            let url: *mut AnyObject = msg_send![class!(NSURL), fileURLWithPath: text];
            grant(Retained::retain(url).unwrap()).unwrap()
        })
    }

    pub fn bookmark(scope: &Scope) -> Result<Vec<u8>, String> {
        objc2::rc::autoreleasepool(|_| unsafe {
            let mut error: *mut AnyObject = ptr::null_mut();
            // NSURLBookmarkCreationWithSecurityScope; no implicit bookmark
            // sharing with another process and no read/write widening.
            let data: *mut AnyObject = msg_send![&*scope.0,
                bookmarkDataWithOptions: (1usize << 11),
                includingResourceValuesForKeys: ptr::null::<AnyObject>(),
                relativeToURL: ptr::null::<AnyObject>(), error: &mut error];
            if data.is_null() {
                return Err(denied());
            }
            let len: usize = msg_send![data, length];
            let bytes: *const u8 = msg_send![data, bytes];
            if len == 0 || len > MAX_BOOKMARK || bytes.is_null() {
                return Err(denied());
            }
            Ok(std::slice::from_raw_parts(bytes, len).to_vec())
        })
    }

    pub fn resolve(bytes: &[u8]) -> Result<(Grant, bool), String> {
        if bytes.is_empty() || bytes.len() > MAX_BOOKMARK {
            return Err(denied());
        }
        objc2::rc::autoreleasepool(|_| unsafe {
            let data: *mut AnyObject = msg_send![class!(NSData), dataWithBytes: bytes.as_ptr(), length: bytes.len()];
            let mut stale = objc2::runtime::Bool::NO;
            let mut error: *mut AnyObject = ptr::null_mut();
            // WithSecurityScope | WithoutUI | WithoutMounting. Startup must
            // never display authentication UI or mount a disconnected drive.
            let raw: *mut AnyObject = msg_send![class!(NSURL), URLByResolvingBookmarkData: data,
                options: ((1usize << 10) | (1usize << 8) | (1usize << 9)),
                relativeToURL: ptr::null::<AnyObject>(), bookmarkDataIsStale: &mut stale, error: &mut error];
            let url = Retained::retain(raw).ok_or_else(denied)?;
            let started: bool = msg_send![&*url, startAccessingSecurityScopedResource];
            if !started {
                return Err(denied());
            }
            Ok((grant(url)?, stale.as_bool()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "mori-bookmark-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn paths_are_not_permissions_and_no_legacy_fallback() {
        let d = scratch();
        let access = Access { required: true, file: d.join(BOOKMARK_FILE), grants: Mutex::new(Grants::default()) };
        assert!(!access.permits(&d));
        assert!(access.lease(&d).is_err());
        assert!(access.set_root(&d).is_err());
        assert!(access.restore_root().unwrap().is_none());
        fs::write(d.join("settings.json"), br#"{"root":"/Users"}"#).unwrap();
        assert!(access.restore_root().unwrap().is_none());
        fs::write(&access.file, b"broken").unwrap();
        assert!(access.restore_root().is_err());
        assert_eq!(fs::read(&access.file).unwrap(), b"broken");
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn bounded_versioned_bookmarks_preserve_previous_on_invalid_write() {
        let d = scratch();
        let p = d.join(BOOKMARK_FILE);
        write_record(&p, b"original").unwrap();
        assert_eq!(read_record(&p).unwrap().bookmark, b"original");
        assert!(write_record(&p, &[]).is_err());
        assert!(write_record(&p, &vec![0; MAX_BOOKMARK + 1]).is_err());
        assert_eq!(read_record(&p).unwrap().bookmark, b"original");
        assert!(Record::decode(br#"{"version":2,"bookmark":[1]}"#).is_err());
        assert!(Record::decode(br#"{"version":1,"bookmark":[],"path":"/"}"#).is_err());
        write_record(&p, b"refreshed").unwrap();
        assert_eq!(read_record(&p).unwrap().bookmark, b"refreshed");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        fs::remove_dir_all(d).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn bookmarked_metadata_never_follows_a_symlink() {
        let d = scratch();
        let p = d.join(BOOKMARK_FILE);
        let original = d.join("user-file");
        fs::write(&original, b"preserve me").unwrap();
        std::os::unix::fs::symlink(&original, &p).unwrap();
        assert!(read_record(&p).is_err());
        assert!(write_record(&p, b"replacement").is_err());
        let access = Access::new(&d);
        access.forget_saved().unwrap();
        assert_eq!(fs::read(&original).unwrap(), b"preserve me");
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn grant_boundaries_are_components_not_string_prefixes() {
        let root = Path::new("/Users/example/Photos");
        assert!(within(root, root));
        assert!(within(&root.join("Album/image.jpg"), root));
        assert!(!within(Path::new("/Users/example/Photos-private/image.jpg"), root));
        assert!(!within(&root.join("../Secrets"), root));
        assert!(!within(Path::new("relative/Photos"), root));
    }

    #[test]
    fn session_cleanup_never_writes_permissions_or_touches_content() {
        let d = scratch();
        let access = Access::new(&d);
        fs::write(d.join("user-file"), b"original").unwrap();
        access.clear_session();
        access.close_root();
        assert!(!access.file.exists());
        assert_eq!(fs::read(d.join("user-file")).unwrap(), b"original");
        fs::remove_dir_all(d).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_resolver_rejects_untrusted_bookmark_bytes() {
        assert!(native::resolve(b"not an Apple bookmark").is_err());
        assert!(native::resolve(&[]).is_err());
    }
}

/// Publishing and revoking session results share this lock. A cancellation flag
/// alone cannot prevent a worker which has already finished from publishing.
#[derive(Default)]
pub struct Session(Mutex<u64>);
impl Session {
    pub fn lock(&self) -> std::sync::MutexGuard<'_, u64> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
    pub fn current(&self) -> u64 {
        *self.lock()
    }
    pub fn enter(&self, generation: u64) -> Option<std::sync::MutexGuard<'_, u64>> {
        let guard = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        (*guard == generation).then_some(guard)
    }
    pub fn invalidate(&self) -> std::sync::MutexGuard<'_, u64> {
        let mut guard = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        *guard = guard.wrapping_add(1);
        guard
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;
    #[test]
    fn completed_job_cannot_republish_after_clear() {
        let session = Session::default();
        let generation = session.current();
        let result = Mutex::new(Some("private result"));
        {
            let _revocation = session.invalidate();
            *result.lock().unwrap() = None;
        }
        if let Some(_publication) = session.enter(generation) {
            *result.lock().unwrap() = Some("late result");
        }
        assert_eq!(*result.lock().unwrap(), None);
        assert!(session.enter(session.current()).is_some());
    }
    #[test]
    fn clear_waits_for_publication_then_removes_it() {
        let session = Arc::new(Session::default());
        let result = Arc::new(Mutex::new(None));
        let generation = session.current();
        let publishing = session.enter(generation).unwrap();
        let other = session.clone();
        let value = result.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            tx.send(()).unwrap();
            let _revocation = other.invalidate();
            *value.lock().unwrap() = None;
        });
        rx.recv().unwrap();
        *result.lock().unwrap() = Some("result");
        drop(publishing);
        worker.join().unwrap();
        assert_eq!(*result.lock().unwrap(), None);
        assert!(session.enter(generation).is_none());
    }
}

#[cfg(all(test, target_os = "macos"))]
mod grant_regressions {
    use super::*;
    #[test]
    fn transitioning_preserves_new_root_and_revokes_old_requests() {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("mori-grant-transition-{}", std::process::id()));
        let a = base.join("temporary");
        let b = base.join("normal");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("secret.txt"), b"temporary").unwrap();
        fs::write(b.join("photo.txt"), b"normal").unwrap();
        let access = Access { required: true, file: base.join(BOOKMARK_FILE), grants: Mutex::new(Grants::default()) };
        access.add(native::test_grant(&a)).unwrap();
        access.set_root(&a).unwrap();
        let _in_flight = access.lease(&a).unwrap();
        assert!(access.open_inside(&a, "secret.txt").is_ok());
        access.add(native::test_grant(&b)).unwrap();
        access.set_root(&b).unwrap();
        access.clear_session();
        assert!(!access.permits(&a));
        assert!(access.lease(&a).is_err());
        assert!(
            access.open_inside(&a, "secret.txt").is_err(),
            "old native lease must not authorize a new protocol open"
        );
        assert!(access.open_inside(&b, "photo.txt").is_ok());
        assert!(!access.file.exists(), "temporary root must not gain a bookmark");
        drop(_in_flight);
        drop(access);
        fs::remove_dir_all(base).unwrap();
    }
}
