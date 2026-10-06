//! Mori's version is written in several places (Cargo, the bundle config,
//! the frontend package). A release must never ship with them disagreeing.

use std::fs;
use std::path::Path;

fn json_version(path: &Path) -> String {
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    v["version"].as_str().unwrap_or_else(|| panic!("{path:?} has no version")).to_string()
}

#[test]
fn every_version_matches_cargo() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let cargo = env!("CARGO_PKG_VERSION");
    assert_eq!(json_version(&root.join("tauri.conf.json")), cargo, "tauri.conf.json");
    assert_eq!(json_version(&root.join("../package.json")), cargo, "package.json");
    assert_eq!(json_version(&root.join("../package-lock.json")), cargo, "package-lock.json");
    let lock = fs::read_to_string(root.join("Cargo.lock")).unwrap();
    assert!(lock.contains(&format!("name = \"mori\"\nversion = \"{cargo}\"")), "Cargo.lock");
}
