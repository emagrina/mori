//! The language of the messages Rust writes for the interface (errors,
//! findings, dialog titles). English is the source: strings are written in
//! English and are their own keys in `locales/<code>.json`, so a missing
//! translation falls back to English.
//!
//!     tr!("File unavailable")
//!     tr!("The header declares {w} × {h} pixels.", w = w, h = h)
//!     trn!(count, "{n} file", "{n} files")
//!
//! The catalogs are compiled in; the active language is set by the UI.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::{OnceLock, PoisonError, RwLock};

macro_rules! catalogs {
    ($($code:literal),+ $(,)?) => {
        /// Every supported language except English (the source).
        const CATALOGS: &[(&str, &str)] = &[$(($code, include_str!(concat!("../locales/", $code, ".json")))),+];
    };
}
catalogs![
    "es", "ca", "fr", "de", "it", "pt", "nl", "pl", "uk", "ru", "tr", "ar", "he", "hi", "bn", "id", "vi", "th",
    "zh-Hans", "zh-Hant", "ja", "ko"
];

static ACTIVE: RwLock<&'static str> = RwLock::new("en");

fn parsed() -> &'static HashMap<&'static str, HashMap<String, Value>> {
    static PARSED: OnceLock<HashMap<&'static str, HashMap<String, Value>>> = OnceLock::new();
    PARSED.get_or_init(|| {
        CATALOGS.iter().map(|(code, json)| (*code, serde_json::from_str(json).unwrap_or_default())).collect()
    })
}

/// Whether `code` is a supported language.
pub fn supported(code: &str) -> bool {
    code == "en" || CATALOGS.iter().any(|(c, _)| *c == code)
}

/// Sets the active language; unknown codes are ignored.
pub fn set_active(code: &str) {
    if let Some((c, _)) = CATALOGS.iter().find(|(c, _)| *c == code) {
        *ACTIVE.write().unwrap_or_else(PoisonError::into_inner) = c;
    } else if code == "en" {
        *ACTIVE.write().unwrap_or_else(PoisonError::into_inner) = "en";
    }
}

fn active() -> &'static str {
    *ACTIVE.read().unwrap_or_else(PoisonError::into_inner)
}

fn entry(text: &str) -> Option<&'static Value> {
    parsed().get(active())?.get(text)
}

/// The translation of `text`, or `text` itself.
pub fn lookup(text: &str) -> String {
    match entry(text) {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => text.to_string(),
    }
}

/// The translation of a plural message, keyed by its English plural form.
pub fn lookup_plural(n: u64, one: &str, other: &str) -> String {
    let s = match entry(other) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Object(m)) => {
            m.get(plural_category(active(), n)).or_else(|| m.get("other")).and_then(Value::as_str).map(str::to_string)
        }
        _ => None,
    };
    let s = s.unwrap_or_else(|| (if n == 1 { one } else { other }).to_string());
    s.replace("{n}", &n.to_string())
}

/// Replaces each `{name}` in `template`.
pub fn fill(mut template: String, args: &[(&str, String)]) -> String {
    for (k, v) in args {
        template = template.replace(&format!("{{{k}}}"), v);
    }
    template
}

/// CLDR plural category of a whole number, for the supported languages.
fn plural_category(lang: &str, n: u64) -> &'static str {
    let (m10, m100) = (n % 10, n % 100);
    match lang {
        "ja" | "ko" | "zh-Hans" | "zh-Hant" | "vi" | "th" | "id" => "other",
        "fr" | "pt" | "hi" | "bn" if n <= 1 => "one",
        "ru" | "uk" if m10 == 1 && m100 != 11 => "one",
        "ru" | "uk" | "pl" if (2..=4).contains(&m10) && !(12..=14).contains(&m100) => "few",
        "ru" | "uk" => "many",
        "pl" if n == 1 => "one",
        "pl" => "many",
        "ar" => match n {
            0 => "zero",
            1 => "one",
            2 => "two",
            _ if (3..=10).contains(&m100) => "few",
            _ if (11..=99).contains(&m100) => "many",
            _ => "other",
        },
        "he" if n == 2 => "two",
        _ if n == 1 => "one",
        _ => "other",
    }
}

/// Translate an English message; `name = value` pairs fill its `{name}` placeholders.
macro_rules! tr {
    ($text:literal) => {
        $crate::i18n::lookup($text)
    };
    ($text:literal, $($k:ident = $v:expr),+ $(,)?) => {
        $crate::i18n::fill($crate::i18n::lookup($text), &[$((stringify!($k), ($v).to_string())),+])
    };
}

/// Plural-aware `tr!`: `{n}` is the count; the catalog key is the English plural form.
macro_rules! trn {
    ($n:expr, $one:literal, $other:literal $(, $k:ident = $v:expr)* $(,)?) => {
        $crate::i18n::fill($crate::i18n::lookup_plural(($n) as u64, $one, $other), &[$((stringify!($k), ($v).to_string())),*])
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plural_categories() {
        assert_eq!(plural_category("ru", 1), "one");
        assert_eq!(plural_category("ru", 3), "few");
        assert_eq!(plural_category("ru", 11), "many");
        assert_eq!(plural_category("ru", 22), "few");
        assert_eq!(plural_category("pl", 5), "many");
        assert_eq!(plural_category("ar", 2), "two");
        assert_eq!(plural_category("ar", 105), "few");
        assert_eq!(plural_category("fr", 0), "one");
        assert_eq!(plural_category("en", 0), "other");
        assert_eq!(plural_category("ja", 1), "other");
    }

    #[test]
    fn catalogs_parse() {
        for (code, json) in CATALOGS {
            assert!(serde_json::from_str::<HashMap<String, Value>>(json).is_ok(), "{code}.json is not a valid catalog");
        }
    }

    #[test]
    fn fills_placeholders() {
        assert_eq!(fill("{w} × {h}".into(), &[("w", "3".into()), ("h", "4".into())]), "3 × 4");
    }
}
