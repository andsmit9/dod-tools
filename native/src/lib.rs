pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(not(target_arch = "wasm32"))]
use analysis::Analysis;
#[cfg(not(target_arch = "wasm32"))]
use filetime::FileTime;
#[cfg(not(target_arch = "wasm32"))]
use std::fs;
#[cfg(not(target_arch = "wasm32"))]
use std::io::Read;
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;
use web_time::SystemTime;

pub mod patch;

#[cfg(not(target_arch = "wasm32"))]
pub mod hlcr;

pub mod sys;
pub mod utils;
pub mod shared;

mod messages;

#[cfg(not(target_arch = "wasm32"))]
pub mod capture_engine;

/// Driving OBS Studio as an alternate capture path (#65).
#[cfg(not(target_arch = "wasm32"))]
pub mod obs;

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct FileInfo {
    pub created_at: SystemTime,
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
}

impl Default for FileInfo {
    fn default() -> Self {
        Self {
            created_at: SystemTime::UNIX_EPOCH,
            name: String::new(),
            path: String::new(),
            size_bytes: 0,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn run_analyzer(demo_path: &PathBuf) -> Result<(FileInfo, Analysis), String> {
    run_analyzer_with_progress(demo_path, |_, _| {})
}

#[cfg(not(target_arch = "wasm32"))]
pub fn run_analyzer_with_progress<F>(
    demo_path: &PathBuf,
    progress_cb: F,
) -> Result<(FileInfo, Analysis), String>
where
    F: FnMut(usize, usize),
{
    let mut file = fs::OpenOptions::new()
        .read(true)
        .open(demo_path)
        .map_err(|e| format!("Could not open the file: {}", e))?;

    let mut bytes: Vec<u8> = vec![];

    file.read_to_end(&mut bytes)
        .map_err(|e| format!("Could not read the file: {}", e))?;

    let analysis = Analysis::try_from_bytes_with_progress(bytes.as_slice(), progress_cb)?;
    let file_info = build_file_info(demo_path)?;

    Ok((file_info, analysis))
}

#[cfg(not(target_arch = "wasm32"))]
fn build_file_info(demo_path: &PathBuf) -> Result<FileInfo, String> {
    let metadata =
        fs::metadata(demo_path).map_err(|e| format!("Could not read metadata: {}", e))?;
    let size_bytes = metadata.len();
    let created_at = FileTime::from_last_modification_time(&metadata);
    let creation_offset = Duration::new(created_at.unix_seconds() as u64, created_at.nanoseconds());
    let created_at_system = SystemTime::UNIX_EPOCH + creation_offset;

    Ok(FileInfo {
        created_at: created_at_system,
        name: demo_path
            .file_name()
            .and_then(|s| s.to_str())
            .map(String::from)
            .unwrap_or_default(),

        path: demo_path.to_str().map(String::from).unwrap_or_default(),
        size_bytes,
    })
}

// Bump whenever `AnalyzerState`/`Player`/related computed fields change, so
// caches written by an older schema are treated as a miss instead of
// silently deserializing with new fields missing/defaulted.
#[cfg(not(target_arch = "wasm32"))]
const ANALYZER_CACHE_SCHEMA_VERSION: u32 = 2;

#[cfg(not(target_arch = "wasm32"))]
#[derive(serde::Deserialize)]
struct AnalyzerCacheEntry {
    size_bytes: u64,
    modified_unix_secs: u64,
    file_info: FileInfo,
    analysis: Analysis,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(serde::Serialize)]
struct AnalyzerCacheEntryRef<'a> {
    size_bytes: u64,
    modified_unix_secs: u64,
    file_info: &'a FileInfo,
    analysis: &'a Analysis,
}

#[cfg(not(target_arch = "wasm32"))]
fn analyzer_cache_path(demo_path: &PathBuf) -> Option<PathBuf> {
    let canonical = fs::canonicalize(demo_path).ok()?;
    let key = canonical.to_string_lossy();
    let hash = crate::utils::demo_hasher::fnv1a_hash(key.as_bytes());
    Some(
        crate::shared::paths::get_appdata_dir()
            .join("analyzer_cache")
            .join(format!("v{}", ANALYZER_CACHE_SCHEMA_VERSION))
            .join(format!("{:016x}.json", hash)),
    )
}

/// Same as `run_analyzer_with_progress`, but backed by an on-disk JSON cache
/// keyed on the demo's canonicalized path, size, and mtime. A cache hit skips
/// straight to a ~10-15ms file read + deserialize instead of the full ~1.3s
/// parse; `progress_cb` is not invoked on the cache-hit path. Returns whether
/// the result came from cache as the third tuple element.
#[cfg(not(target_arch = "wasm32"))]
pub fn run_analyzer_cached<F>(
    demo_path: &PathBuf,
    progress_cb: F,
) -> Result<(FileInfo, Analysis, bool), String>
where
    F: FnMut(usize, usize),
{
    let metadata =
        fs::metadata(demo_path).map_err(|e| format!("Could not read metadata: {}", e))?;
    let size_bytes = metadata.len();
    let modified_unix_secs = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let cache_path = analyzer_cache_path(demo_path);

    if let Some(cache_path) = &cache_path
        && let Ok(bytes) = fs::read(cache_path)
            && let Ok(entry) = serde_json::from_slice::<AnalyzerCacheEntry>(&bytes)
                && entry.size_bytes == size_bytes && entry.modified_unix_secs == modified_unix_secs {
                    return Ok((entry.file_info, entry.analysis, true));
                }

    let (file_info, analysis) = run_analyzer_with_progress(demo_path, progress_cb)?;

    if let Some(cache_path) = &cache_path {
        write_analyzer_cache_entry(cache_path, size_bytes, modified_unix_secs, &file_info, &analysis);
    }

    Ok((file_info, analysis, false))
}

#[cfg(not(target_arch = "wasm32"))]
fn write_analyzer_cache_entry(
    cache_path: &PathBuf,
    size_bytes: u64,
    modified_unix_secs: u64,
    file_info: &FileInfo,
    analysis: &Analysis,
) {
    if let Some(parent) = cache_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let entry_ref = AnalyzerCacheEntryRef {
        size_bytes,
        modified_unix_secs,
        file_info,
        analysis,
    };
    // Best-effort: a cache write failure must never fail the caller.
    if let Ok(json) = serde_json::to_vec(&entry_ref) {
        let _ = fs::write(cache_path, json);
    }
}

/// Writes `analysis` (already computed by a folder scan, e.g.
/// `scan_demo_for_highlights_with_analysis`) straight into the analyzer
/// cache, so a later `run_analyzer_cached` call for the same demo hits the
/// ~10-15ms cache path instead of re-parsing. Best-effort and silent on any
/// failure — cache warming must never affect the caller's own result.
#[cfg(not(target_arch = "wasm32"))]
pub fn warm_analyzer_cache(demo_path: &PathBuf, analysis: &Analysis) {
    let Ok(metadata) = fs::metadata(demo_path) else { return };
    let size_bytes = metadata.len();
    let modified_unix_secs = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let Some(cache_path) = analyzer_cache_path(demo_path) else { return };
    let Ok(file_info) = build_file_info(demo_path) else { return };

    write_analyzer_cache_entry(&cache_path, size_bytes, modified_unix_secs, &file_info, analysis);
}

#[cfg(not(target_arch = "wasm32"))]
static SESSION_HEADER_WRITTEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Date string (`YYYYMMDD`) `log_markdown` last wrote to, so a session
/// running past midnight can be detected and cross-referenced between the
/// two days' files instead of just silently stopping mid-file.
#[cfg(not(target_arch = "wasm32"))]
static LAST_LOG_DATE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// How many days' worth of activity logs to keep on disk — older files are
/// pruned the first time a given app launch logs anything.
#[cfg(not(target_arch = "wasm32"))]
const MAX_RETAINED_ACTIVITY_LOG_DAYS: usize = 30;

/// Directory `log_markdown` writes into.
#[cfg(not(target_arch = "wasm32"))]
pub fn activity_log_dir() -> std::path::PathBuf {
    redirected_log_dir().unwrap_or_else(|| crate::shared::paths::get_appdata_dir().join("logs"))
}

/// Where the activity log goes when it must not go to the user's own.
///
/// The activity log is this project's primary record of what a capture did —
/// the decal flush work was reconstructed from it repeatedly. Test runs
/// exercise real pipeline code that logs, so without this they append fixture
/// names like `no_such_demo_should_ever_be_read.dem` into
/// `%APPDATA%/dod-tools/logs` under their own session headers, indistinguishable
/// from a genuine capture that failed. It also means `cargo test` silently
/// mutates a user file outside the repo. See issue #64.
///
/// Two ways in. `DOD_TOOLS_LOG_DIR` redirects it for anyone who needs it —
/// an integration test, a packaging check, someone reproducing a bug without
/// stamping on their real record — and a `cfg(test)` build redirects itself.
/// The env var is checked first so a test binary compiled *without* `cfg(test)`
/// (an integration test, or another crate's tests linking this one as an
/// ordinary dependency) can still be pointed somewhere safe.
#[cfg(not(target_arch = "wasm32"))]
fn redirected_log_dir() -> Option<std::path::PathBuf> {
    if let Some(dir) = std::env::var_os("DOD_TOOLS_LOG_DIR") {
        return Some(std::path::PathBuf::from(dir));
    }
    if cfg!(test) {
        return Some(std::env::temp_dir().join("dod_tools_test_logs"));
    }
    None
}

/// Path `log_markdown` is currently writing to — one file per calendar day
/// (shared across every app launch that day), so this is recomputed from
/// the current date on every call rather than cached, and just starts
/// pointing at tomorrow's file on its own if a session runs past midnight.
/// Exposed so the frontend can offer a "View Logs" affordance without
/// duplicating the path logic.
#[cfg(not(target_arch = "wasm32"))]
pub fn activity_log_path() -> std::path::PathBuf {
    use chrono::Local;
    activity_log_dir().join(format!("activity_{}.md", Local::now().format("%Y%m%d")))
}

/// Deletes `activity_*.md` files beyond `MAX_RETAINED_ACTIVITY_LOG_DAYS` —
/// filenames are zero-padded `YYYYMMDD` dates, so lexical sort order is
/// chronological order.
#[cfg(not(target_arch = "wasm32"))]
fn prune_old_activity_logs(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<std::path::PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("activity_") && n.ends_with(".md"))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    if files.len() > MAX_RETAINED_ACTIVITY_LOG_DAYS {
        for old in &files[..files.len() - MAX_RETAINED_ACTIVITY_LOG_DAYS] {
            let _ = std::fs::remove_file(old);
        }
    }
}

pub fn log_markdown(msg: &str) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use chrono::Local;
        use std::io::Write;

        static LOG_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOG_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

        let dir = activity_log_dir();
        let _ = std::fs::create_dir_all(&dir);
        let today = Local::now().format("%Y%m%d").to_string();
        let log_path_md = dir.join(format!("activity_{}.md", today));
        let is_first_write = !SESSION_HEADER_WRITTEN.swap(true, std::sync::atomic::Ordering::SeqCst);

        let mut last_date = LAST_LOG_DATE.lock().unwrap_or_else(|e| e.into_inner());
        if is_first_write {
            prune_old_activity_logs(&dir);
        } else if last_date.as_deref().is_some_and(|prev| prev != today) {
            // Same session, but the date rolled over since the last log line —
            // leave a pointer in both files so this doesn't just look like the
            // session stopped mid-file to someone reading yesterday's log.
            let prev_path = dir.join(format!("activity_{}.md", last_date.as_deref().unwrap()));
            if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(&prev_path) {
                let _ = writeln!(f, "\n(session continues past midnight — see activity_{}.md)\n", today);
                let _ = f.sync_all();
            }
            if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(&log_path_md) {
                let _ = writeln!(f, "(continued from activity_{}.md — same session, crossed midnight)\n", last_date.as_deref().unwrap());
                let _ = f.sync_all();
            }
        }
        *last_date = Some(today);
        drop(last_date);

        if let Ok(mut f) = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&log_path_md)
        {
            if is_first_write {
                let time_str = Local::now().format("%Y-%m-%d @ %H:%M %Z").to_string();
                let _ = writeln!(f, "\n\n========== New Session: {} ====================\n", time_str);
            }
            let time_str = Local::now().format("%H:%M:%S").to_string();
            let _ = writeln!(f, "* [{}] {}", time_str, msg);
            let _ = f.sync_all();
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        log::info!("{}", msg);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod activity_log_tests {
    use super::*;

    /// `DOD_TOOLS_LOG_DIR` is process-global, not thread-local, but `cargo
    /// test` runs every test in this file on its own thread of the same
    /// process by default. Without this, `the_env_override_outranks_everything`
    /// temporarily overriding the var could interleave with
    /// `writing_a_line_lands_in_the_redirected_directory`'s write-then-read
    /// (both go through `redirected_log_dir()`), so the write landed under
    /// one directory and the read checked another — reproduced reliably in CI
    /// (2/2 runs) once these tests started running in the same binary as
    /// enough others to make the interleaving likely. Any test that reads or
    /// writes through `redirected_log_dir()` must hold this for its full
    /// touch-env-through-assert span, not just the `set_var` call.
    static ENV_VAR_TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The invariant: a test run must not write outside the repo and its
    /// scratch. This is the one that matters — everything else here just
    /// explains how it is held.
    #[test]
    fn a_test_run_never_logs_into_the_users_own_activity_log() {
        let real = crate::shared::paths::get_appdata_dir().join("logs");
        assert_ne!(
            activity_log_dir(),
            real,
            "cargo test is appending fixture sessions to the real capture record"
        );
        assert!(
            !activity_log_path().starts_with(&real),
            "the log file landed inside the user's own log directory"
        );
    }

    #[test]
    fn the_env_override_outranks_everything() {
        let _guard = ENV_VAR_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // The override exists for the case cfg(test) cannot reach: a binary or
        // integration test that links this crate as an ordinary dependency, so
        // it is compiled without cfg(test) and would otherwise write to the
        // user's real log.
        let want = std::env::temp_dir().join("dod_tools_override_probe");
        let previous = std::env::var_os("DOD_TOOLS_LOG_DIR");
        unsafe { std::env::set_var("DOD_TOOLS_LOG_DIR", &want) };
        let got = redirected_log_dir();
        match previous {
            Some(p) => unsafe { std::env::set_var("DOD_TOOLS_LOG_DIR", p) },
            None => unsafe { std::env::remove_var("DOD_TOOLS_LOG_DIR") },
        }
        assert_eq!(got, Some(want));
    }

    #[test]
    fn writing_a_line_lands_in_the_redirected_directory() {
        let _guard = ENV_VAR_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // Not just the path calculation — the whole write path, since
        // `log_markdown` computes the directory itself rather than taking one.
        log_markdown("activity log redirect probe");
        let path = activity_log_path();
        assert!(path.exists(), "nothing was written to {}", path.display());
        let body = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(body.contains("activity log redirect probe"));
    }
}
