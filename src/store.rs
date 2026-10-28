use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;

pub const APP_DIR: &str = "seenmark";

/// %LOCALAPPDATA%\seenmark
pub fn data_dir() -> PathBuf {
    let local = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let mut p = std::env::temp_dir();
            p.push("seenmark-fallback");
            p
        });
    local.join(APP_DIR)
}

pub fn state_path() -> PathBuf {
    data_dir().join("state.json")
}

pub fn icon_path() -> PathBuf {
    data_dir().join("seen.ico")
}

pub fn bin_dir() -> PathBuf {
    data_dir().join("bin")
}

/// path (lowercased, UNC-prefix-stripped) -> unix seconds of the mark
pub type MarkMap = HashMap<String, u64>;

pub fn normalize_str(path: &str) -> String {
    let p = path
        .strip_prefix(r"\\?\")
        .or_else(|| path.strip_prefix(r"\\?\"))
        .unwrap_or(path);
    p.to_lowercase()
}

pub fn load() -> MarkMap {
    try_load().unwrap_or_default()
}

/// Parse failure is reported, never silently turned into an empty map:
/// callers that write must not clobber state they failed to read.
pub fn try_load() -> Result<MarkMap, String> {
    let bytes = fs::read(state_path()).map_err(|e| format!("read: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parse: {e}"))
}

/// Atomic save: write to temp file, then rename over the target.
pub fn save(map: &MarkMap) -> io::Result<()> {
    let dir = data_dir();
    fs::create_dir_all(&dir)?;
    let target = state_path();
    let tmp = dir.join("state.json.tmp");
    let json = serde_json::to_vec_pretty(map)?;
    fs::write(&tmp, json)?;
    // rename can fail if the target exists on some setups; remove first.
    let _ = fs::remove_file(&target);
    fs::rename(&tmp, target)
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
