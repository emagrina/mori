//! Store ratings: the system's own review prompt and the "write a review" page.
//!
//! Mori ships outside the stores today, so all of this is off unless a store
//! build sets its listing id at compile time:
//!
//!     MORI_MAC_APP_STORE_ID=1234567890     Mac App Store (the app's Apple ID)
//!     MORI_MS_STORE_ID=9NBLGGH4R32N        Microsoft Store (product id)
//!
//! When the prompt is shown is decided by the UI (src/review.ts); the system
//! itself also limits how often the prompt really appears.

use serde::{Deserialize, Serialize};

const MAC_APP_STORE_ID: Option<&str> = option_env!("MORI_MAC_APP_STORE_ID");
const MS_STORE_ID: Option<&str> = option_env!("MORI_MS_STORE_ID");

#[cfg(target_os = "macos")]
#[link(name = "StoreKit", kind = "framework")]
extern "C" {}

/// The store this build is listed in, and its listing id.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Store {
    Mac(&'static str),
    Microsoft(&'static str),
}

/// What the UI needs to know: no store means no prompt and no menu item.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    pub store: Option<&'static str>,
    /// The system has an in-app review prompt for this store.
    pub native_prompt: bool,
    pub version: &'static str,
}

/// Counters kept in settings.json for deciding when to ask (see src/review.ts).
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct State {
    pub sessions: u32,
    pub usage_ms: u64,
    pub successes: u32,
    pub successes_since_ask: u32,
    pub first_seen_at: Option<u64>,
    pub last_asked_at: Option<u64>,
    pub ask_count: u32,
    pub rated_version: Option<String>,
    pub last_problem_at: Option<u64>,
    pub last_launch: Option<u64>,
}

/// App Store ids are numbers; Microsoft Store product ids are 12 letters/digits.
/// Both end up in a URL, so nothing else is accepted.
fn valid_mac_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 12 && id.bytes().all(|b| b.is_ascii_digit())
}

fn valid_ms_id(id: &str) -> bool {
    id.len() == 12 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// The store of this build on this platform, if any.
pub fn store() -> Option<Store> {
    let mac = MAC_APP_STORE_ID.filter(|id| valid_mac_id(id)).map(Store::Mac);
    let ms = MS_STORE_ID.filter(|id| valid_ms_id(id)).map(Store::Microsoft);
    if cfg!(target_os = "macos") {
        mac
    } else if cfg!(windows) {
        ms
    } else {
        None
    }
}

pub fn info() -> Info {
    let store = store();
    Info {
        store: store.map(|s| match s {
            Store::Mac(_) => "mac",
            Store::Microsoft(_) => "microsoft",
        }),
        // Windows' rate-and-review dialog needs the WinRT StoreContext, which
        // Mori doesn't link: there the review page is opened from the menu only.
        native_prompt: matches!(store, Some(Store::Mac(_))),
        version: env!("CARGO_PKG_VERSION"),
    }
}

/// The store's "write a review" page for this app.
pub fn review_url(store: Store) -> String {
    match store {
        Store::Mac(id) => format!("macappstore://apps.apple.com/app/id{id}?action=write-review"),
        Store::Microsoft(id) => format!("ms-windows-store://review/?ProductId={id}"),
    }
}

/// Asks the system to show its review prompt. It may decide not to (Apple
/// shows it at most three times a year), so nothing is assumed about it.
pub fn request_native(app: &tauri::AppHandle) -> bool {
    if !info().native_prompt {
        return false;
    }
    // StoreKit wants the main thread.
    #[cfg(target_os = "macos")]
    let asked = app
        .run_on_main_thread(|| unsafe {
            use objc2::msg_send;
            use objc2::runtime::AnyClass;
            if let Some(cls) = AnyClass::get(c"SKStoreReviewController") {
                let _: () = msg_send![cls, requestReview];
            }
        })
        .is_ok();
    #[cfg(not(target_os = "macos"))]
    let asked = {
        let _ = app;
        false
    };
    asked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_ids_are_validated() {
        assert!(valid_mac_id("1234567890"));
        assert!(!valid_mac_id("12a"));
        assert!(!valid_mac_id(""));
        assert!(!valid_mac_id("1;rm -rf"));
        assert!(valid_ms_id("9NBLGGH4R32N"));
        assert!(!valid_ms_id("9NBLGGH4R32N&x=1"));
    }

    #[test]
    fn review_pages() {
        assert_eq!(review_url(Store::Mac("123")), "macappstore://apps.apple.com/app/id123?action=write-review");
        assert_eq!(review_url(Store::Microsoft("9NBLGGH4R32N")), "ms-windows-store://review/?ProductId=9NBLGGH4R32N");
    }

    #[test]
    fn off_without_a_store_id() {
        if MAC_APP_STORE_ID.is_none() && MS_STORE_ID.is_none() {
            let i = info();
            assert!(i.store.is_none() && !i.native_prompt);
        }
    }
}
