// messages.rs
// Rust-side counterpart to desktop-studio/src/strings.js: the Tauri command
// layer's own authored user-facing error/status text, centralized so the
// same message can't drift into two different wordings at two call sites
// (see the "Task join error: {}" cluster below, previously duplicated
// verbatim 13 times across 4 files) and shared boilerplate lives in exactly
// one place. Scope matches strings.js's own: text that reaches the frontend
// as a toast/dialog string. Developer-only log::error!/eprintln!
// diagnostics stay inline at their call site, same as strings.js leaves
// console.log/console.error alone. Issue #33, track 2 — a parallel catalog
// to strings.js, not a merge into it (Rust and JS strings are never shared
// cross-language here).
//
// Covers every Tauri command file's error/status strings: lib.rs,
// capture_manager.rs, render_manager.rs, audit_manager.rs, map_manager.rs,
// updater_manager.rs, settings_manager.rs, and dir_browser.rs. Not yet
// covered: native/analysis errors that bubble straight through Tauri
// commands unwrapped (a separate crate, out of scope for this pass — see
// issue #33).

use std::fmt::Display;

// ── spawn_blocking join-error flattening ────────────────────────────────────
//
// Two closure shapes appear at Tauri command call sites: one whose closure
// itself returns `Result<T, String>` (needs the outer JoinError flattened
// into that same Result via `?`), and one whose closure returns a plain `T`
// (the JoinError becomes the whole function's error case directly, no `?`
// needed). Each gets its own helper rather than forcing one shape to fit
// the other.

/// For a `spawn_blocking` closure returning `Result<T, String>`. Awaits the
/// handle and flattens `Result<Result<T, String>, JoinError>` into
/// `Result<T, String>`, converting a join failure (the blocking task itself
/// panicked) into the same error-string shape as any other command failure.
pub async fn flatten_spawn_blocking<T>(
    handle: tokio::task::JoinHandle<Result<T, String>>,
) -> Result<T, String> {
    handle.await.map_err(|e| format!("Task join error: {}", e))?
}

/// For a `spawn_blocking` closure returning a plain `T` (no inner Result).
/// Awaits the handle and turns a join failure into the same error-string
/// shape the rest of the command layer uses.
pub async fn spawn_blocking_result<T>(handle: tokio::task::JoinHandle<T>) -> Result<T, String> {
    handle.await.map_err(|e| format!("Task join error: {}", e))
}

// ── lib.rs ───────────────────────────────────────────────────────────────

/// Generic "Failed to write <path>: <err>" — same shape independently
/// authored at least twice (lib.rs's project session save, plus whatever
/// else reads a user-given path); reused rather than re-typed per call site.
pub fn failed_to_write_file(path: &str, err: impl Display) -> String {
    format!("Failed to write {}: {}", path, err)
}

/// See [`failed_to_write_file`] — the read-side counterpart, same reuse
/// rationale (lib.rs's project session load, dir_browser.rs's directory
/// listing).
pub fn failed_to_read_file(path: &str, err: impl Display) -> String {
    format!("Failed to read {}: {}", path, err)
}

pub const HLAE_EXECUTABLE_NOT_FOUND: &str = "HLAE executable not found at specified path.";
pub const HL_EXECUTABLE_NOT_FOUND: &str = "Half-Life executable not found at specified path.";

pub fn demo_file_not_found(demo_path: &str) -> String {
    format!("Demo file not found: {}", demo_path)
}

pub fn not_a_directory(path: &str) -> String {
    format!("Not a directory: {}", path)
}

pub fn analyzer_error(err: impl Display) -> String {
    format!("Analyzer error: {}", err)
}

pub fn ffmpeg_could_not_be_resolved(ffmpeg_path: &str) -> String {
    format!(
        "Could not resolve an FFmpeg from \"{}\". Set Render Studio's FFmpeg path to a real \
         ffmpeg.exe first — HLAE's ini needs an absolute path and cannot use a bare command \
         name.",
        ffmpeg_path
    )
}

// ── capture_manager.rs ──────────────────────────────────────────────────────

pub const CAPTURE_BATCH_ALREADY_RUNNING: &str = "Capture batch already in progress";
pub const NO_STREAKS_IN_PAYLOAD: &str = "No streaks in payload";

pub fn configure_paths_before(action: &str) -> String {
    format!("Configure the HLAE and Half-Life executable paths before {}.", action)
}

pub const HLAE_NOT_FOUND_AT_CONFIGURED_PATH: &str = "HLAE executable not found at the configured path.";
pub const HL_NOT_FOUND_AT_CONFIGURED_PATH: &str = "Half-Life executable not found at the configured path.";
pub const NO_HIGHLIGHTS_TO_PREVIEW: &str = "This demo has no highlights to preview.";
pub const FAILED_TO_BUILD_PREVIEW_JOBS: &str = "Failed to build any preview patch jobs.";
pub const CONFIGURE_OBS_PATH_BEFORE_LAUNCHING: &str = "Configure the OBS executable path before launching.";
pub const OBS_NOT_FOUND_AT_CONFIGURED_PATH: &str = "OBS executable not found at the configured path.";

pub fn obs_connection_test_failed(err: impl Display) -> String {
    format!("OBS connection test failed to run: {}", err)
}

pub fn obs_orphan_check_failed(err: impl Display) -> String {
    format!("OBS orphan check failed to run: {}", err)
}

pub fn obs_orphan_recovery_failed(err: impl Display) -> String {
    format!("OBS orphan recovery failed to run: {}", err)
}

pub fn failed_to_create_dod_directory(err: impl Display) -> String {
    format!("Failed to create dod directory: {}", err)
}

pub fn failed_to_patch_preview_demo(source_demo: &str, err: impl Display) -> String {
    format!("Failed to patch preview demo for {}: {}", source_demo, err)
}

pub fn failed_to_write_preview_sidecar(err: impl Display) -> String {
    format!("Failed to write preview sidecar: {}", err)
}

pub fn failed_to_launch_hlae_for_preview(err: impl Display) -> String {
    format!("Failed to launch HLAE for preview: {}", err)
}

pub fn failed_to_launch_hlae(err: impl Display) -> String {
    format!("Failed to launch HLAE: {}", err)
}

pub fn failed_to_launch_obs(err: impl Display) -> String {
    format!("Failed to launch OBS: {}", err)
}

pub fn game_directory_not_found(game_dir: &str) -> String {
    format!("Game directory not found: {}", game_dir)
}

/// Same text independently authored twice (`resolve_preview_env` and
/// `resolve_dod_dir_for_sweep`) — both derive `dod_dir` from `hl.exe`'s
/// parent and fail identically when that parent can't be resolved.
pub const COULD_NOT_RESOLVE_DOD_DIRECTORY: &str = "Could not resolve the 'dod' directory next to hl.exe";
pub const FAILED_TO_BUILD_PREVIEW_PATCH_JOB: &str = "Failed to build the preview patch job";
pub const COULD_NOT_RESOLVE_PREVIEW_FILE_STEM: &str = "Could not resolve the preview demo's file stem";

pub fn failed_to_read_dod_directory(err: impl Display) -> String {
    format!("Failed to read dod directory: {}", err)
}

// ── render_manager.rs ────────────────────────────────────────────────────

pub const RENDER_BATCH_ALREADY_RUNNING_LONG: &str = "A render batch is already running.";
pub const BATCH_ALREADY_QUEUED: &str = "A batch is already queued — start it or cancel it before scanning again.";
pub const RENDER_BATCH_ALREADY_IN_PROGRESS: &str = "Render batch already in progress";
pub const NOTHING_QUEUED_TO_RENDER: &str = "Nothing queued to render — scan for takes first.";
pub const SKIP_ONLY_FOR_OBS_TAKE: &str = "Skip (keep original) is only available for a captured OBS take (its own audio, not a HUD/alpha clip).";

pub fn no_such_job(job_id: &str) -> String {
    format!("No such job: {}", job_id)
}

pub fn job_not_queued(job_id: &str, status: impl Display) -> String {
    format!("Job {} is {} — only a Queued job's codec can be changed", job_id, status)
}

pub fn job_still_rendering(job_id: &str) -> String {
    format!("Job {} is still rendering — cancel it first", job_id)
}

// ── audit_manager.rs ─────────────────────────────────────────────────────

pub fn failed_to_delete_audit_file(path: &str, err: impl Display) -> String {
    format!("Failed to delete {}: {}", path, err)
}

pub fn path_no_longer_exists(path: &str) -> String {
    format!("Path no longer exists: {}", path)
}

pub fn failed_to_open_explorer(err: impl Display) -> String {
    format!("Failed to open explorer: {}", err)
}

// These three are the macOS and Linux arms of `audit_manager::reveal_in_explorer`
// (its `#[cfg(target_os = "macos")]` and `#[cfg(not(any(windows, macos)))]`
// blocks). They are genuinely reachable there, but every build this repo
// actually produces is Windows, so rustc compiles their only call sites out and
// reports them as dead. Allowed rather than gated, so the tests below keep
// covering the wording on every platform.
#[allow(dead_code)]
pub fn failed_to_open_finder(err: impl Display) -> String {
    format!("Failed to open Finder: {}", err)
}

#[allow(dead_code)]
pub const NO_PARENT_DIRECTORY_FOR_PATH: &str = "No parent directory for path";

#[allow(dead_code)]
pub fn failed_to_open_folder(err: impl Display) -> String {
    format!("Failed to open folder: {}", err)
}

// ── map_manager.rs ───────────────────────────────────────────────────────

pub fn no_map_folder_beside_exe(game_path: &str) -> String {
    format!(
        "no map folder beside `{}` — maps are expected at `<hl.exe folder>/dod/maps`",
        game_path
    )
}

pub fn map_check_failed(err: impl Display) -> String {
    format!("map check failed: {}", err)
}

pub fn config_scan_failed(err: impl Display) -> String {
    format!("config scan failed: {}", err)
}

pub fn map_download_failed(err: impl Display) -> String {
    format!("map download failed: {}", err)
}

// ── updater_manager.rs ───────────────────────────────────────────────────

pub fn unknown_update_channel(channel: &str) -> String {
    format!("Unknown update channel: {}", channel)
}

pub fn invalid_updater_endpoint_url(err: impl Display) -> String {
    format!("Invalid updater endpoint URL: {}", err)
}

pub fn failed_to_set_updater_endpoint(err: impl Display) -> String {
    format!("Failed to set updater endpoint: {}", err)
}

pub fn failed_to_build_updater(err: impl Display) -> String {
    format!("Failed to build updater: {}", err)
}

pub const NO_UPDATE_AVAILABLE_TO_INSTALL: &str = "No update available to install — call check_for_update first";

// ── settings_manager.rs ──────────────────────────────────────────────────

pub fn failed_to_serialize_settings(err: impl Display) -> String {
    format!("Failed to serialize settings: {}", err)
}

pub fn failed_to_write_settings_file(path: impl std::fmt::Debug, err: impl Display) -> String {
    format!("Failed to write settings file {:?}: {}", path, err)
}

/// Pins every function/constant above against the exact `format!`/literal it
/// replaced at its original call site (text copied verbatim from the pre-PR
/// source, not re-derived), so this refactor can't have silently changed any
/// user-visible wording.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lib_rs_messages_match_their_original_inline_text() {
        // failed_to_write_file/failed_to_read_file were originally
        // project_session_write_failed/project_session_read_failed —
        // generalized (part 2) once dir_browser.rs turned out to need the
        // identical "Failed to read {}: {}" shape independently.
        assert_eq!(
            failed_to_write_file("proj.json", "disk full"),
            format!("Failed to write {}: {}", "proj.json", "disk full")
        );
        assert_eq!(
            failed_to_read_file("proj.json", "not found"),
            format!("Failed to read {}: {}", "proj.json", "not found")
        );
        assert_eq!(HLAE_EXECUTABLE_NOT_FOUND, "HLAE executable not found at specified path.");
        assert_eq!(HL_EXECUTABLE_NOT_FOUND, "Half-Life executable not found at specified path.");
        assert_eq!(
            demo_file_not_found("demo.dem"),
            format!("Demo file not found: {}", "demo.dem")
        );
        assert_eq!(
            not_a_directory("C:/some/file.txt"),
            format!("Not a directory: {}", "C:/some/file.txt")
        );
        assert_eq!(analyzer_error("bad header"), format!("Analyzer error: {}", "bad header"));
        assert_eq!(
            ffmpeg_could_not_be_resolved("ffmpeg"),
            format!(
                "Could not resolve an FFmpeg from \"{}\". Set Render Studio's FFmpeg path to a real \
                 ffmpeg.exe first — HLAE's ini needs an absolute path and cannot use a bare command \
                 name.",
                "ffmpeg"
            )
        );
    }

    #[test]
    fn capture_manager_messages_match_their_original_inline_text() {
        assert_eq!(CAPTURE_BATCH_ALREADY_RUNNING, "Capture batch already in progress");
        assert_eq!(NO_STREAKS_IN_PAYLOAD, "No streaks in payload");
        assert_eq!(
            configure_paths_before("previewing"),
            format!("Configure the HLAE and Half-Life executable paths before {}.", "previewing")
        );
        assert_eq!(
            configure_paths_before("launching"),
            format!("Configure the HLAE and Half-Life executable paths before {}.", "launching")
        );
        assert_eq!(
            HLAE_NOT_FOUND_AT_CONFIGURED_PATH,
            "HLAE executable not found at the configured path."
        );
        assert_eq!(
            HL_NOT_FOUND_AT_CONFIGURED_PATH,
            "Half-Life executable not found at the configured path."
        );
        assert_eq!(NO_HIGHLIGHTS_TO_PREVIEW, "This demo has no highlights to preview.");
        assert_eq!(FAILED_TO_BUILD_PREVIEW_JOBS, "Failed to build any preview patch jobs.");
        assert_eq!(
            CONFIGURE_OBS_PATH_BEFORE_LAUNCHING,
            "Configure the OBS executable path before launching."
        );
        assert_eq!(
            OBS_NOT_FOUND_AT_CONFIGURED_PATH,
            "OBS executable not found at the configured path."
        );
        assert_eq!(
            obs_connection_test_failed("timeout"),
            format!("OBS connection test failed to run: {}", "timeout")
        );
        assert_eq!(
            obs_orphan_check_failed("timeout"),
            format!("OBS orphan check failed to run: {}", "timeout")
        );
        assert_eq!(
            obs_orphan_recovery_failed("timeout"),
            format!("OBS orphan recovery failed to run: {}", "timeout")
        );
        assert_eq!(
            failed_to_create_dod_directory("access denied"),
            format!("Failed to create dod directory: {}", "access denied")
        );
        assert_eq!(
            failed_to_patch_preview_demo("demo1.dem", "bad frame"),
            format!("Failed to patch preview demo for {}: {}", "demo1.dem", "bad frame")
        );
        assert_eq!(
            failed_to_write_preview_sidecar("disk full"),
            format!("Failed to write preview sidecar: {}", "disk full")
        );
        assert_eq!(
            failed_to_launch_hlae_for_preview("not found"),
            format!("Failed to launch HLAE for preview: {}", "not found")
        );
        assert_eq!(
            failed_to_launch_hlae("not found"),
            format!("Failed to launch HLAE: {}", "not found")
        );
        assert_eq!(
            failed_to_launch_obs("not found"),
            format!("Failed to launch OBS: {}", "not found")
        );
        assert_eq!(
            game_directory_not_found("C:/games/dod"),
            format!("Game directory not found: {}", "C:/games/dod")
        );
        assert_eq!(
            COULD_NOT_RESOLVE_DOD_DIRECTORY,
            "Could not resolve the 'dod' directory next to hl.exe"
        );
        assert_eq!(FAILED_TO_BUILD_PREVIEW_PATCH_JOB, "Failed to build the preview patch job");
        assert_eq!(
            COULD_NOT_RESOLVE_PREVIEW_FILE_STEM,
            "Could not resolve the preview demo's file stem"
        );
        assert_eq!(
            failed_to_read_dod_directory("access denied"),
            format!("Failed to read dod directory: {}", "access denied")
        );
    }

    #[test]
    fn render_manager_messages_match_their_original_inline_text() {
        assert_eq!(RENDER_BATCH_ALREADY_RUNNING_LONG, "A render batch is already running.");
        assert_eq!(
            BATCH_ALREADY_QUEUED,
            "A batch is already queued — start it or cancel it before scanning again."
        );
        assert_eq!(RENDER_BATCH_ALREADY_IN_PROGRESS, "Render batch already in progress");
        assert_eq!(NOTHING_QUEUED_TO_RENDER, "Nothing queued to render — scan for takes first.");
        assert_eq!(
            SKIP_ONLY_FOR_OBS_TAKE,
            "Skip (keep original) is only available for a captured OBS take (its own audio, not a HUD/alpha clip)."
        );
        assert_eq!(no_such_job("job-1"), format!("No such job: {}", "job-1"));
        assert_eq!(
            job_not_queued("job-1", "Rendering"),
            format!("Job {} is {} — only a Queued job's codec can be changed", "job-1", "Rendering")
        );
        assert_eq!(
            job_still_rendering("job-1"),
            format!("Job {} is still rendering — cancel it first", "job-1")
        );
    }

    #[test]
    fn audit_manager_messages_match_their_original_inline_text() {
        assert_eq!(
            failed_to_delete_audit_file("demo.dem", "in use"),
            format!("Failed to delete {}: {}", "demo.dem", "in use")
        );
        assert_eq!(
            path_no_longer_exists("demo.dem"),
            format!("Path no longer exists: {}", "demo.dem")
        );
        assert_eq!(
            failed_to_open_explorer("not found"),
            format!("Failed to open explorer: {}", "not found")
        );
        assert_eq!(
            failed_to_open_finder("not found"),
            format!("Failed to open Finder: {}", "not found")
        );
        assert_eq!(NO_PARENT_DIRECTORY_FOR_PATH, "No parent directory for path");
        assert_eq!(
            failed_to_open_folder("not found"),
            format!("Failed to open folder: {}", "not found")
        );
    }

    #[test]
    fn map_manager_messages_match_their_original_inline_text() {
        assert_eq!(
            no_map_folder_beside_exe("C:/games/dod/hl.exe"),
            format!(
                "no map folder beside `{}` — maps are expected at `<hl.exe folder>/dod/maps`",
                "C:/games/dod/hl.exe"
            )
        );
        assert_eq!(map_check_failed("panic"), format!("map check failed: {}", "panic"));
        assert_eq!(config_scan_failed("panic"), format!("config scan failed: {}", "panic"));
        assert_eq!(map_download_failed("panic"), format!("map download failed: {}", "panic"));
    }

    #[test]
    fn updater_manager_messages_match_their_original_inline_text() {
        assert_eq!(
            unknown_update_channel("nightly"),
            format!("Unknown update channel: {}", "nightly")
        );
        assert_eq!(
            invalid_updater_endpoint_url("bad url"),
            format!("Invalid updater endpoint URL: {}", "bad url")
        );
        assert_eq!(
            failed_to_set_updater_endpoint("bad url"),
            format!("Failed to set updater endpoint: {}", "bad url")
        );
        assert_eq!(
            failed_to_build_updater("bad config"),
            format!("Failed to build updater: {}", "bad config")
        );
        assert_eq!(
            NO_UPDATE_AVAILABLE_TO_INSTALL,
            "No update available to install — call check_for_update first"
        );
    }

    #[test]
    fn settings_manager_messages_match_their_original_inline_text() {
        assert_eq!(
            failed_to_serialize_settings("bad value"),
            format!("Failed to serialize settings: {}", "bad value")
        );
        assert_eq!(
            failed_to_write_settings_file("C:/settings.json", "disk full"),
            format!("Failed to write settings file {:?}: {}", "C:/settings.json", "disk full")
        );
    }

    // Every non-panicking call site's spawn_blocking wrapping was already
    // exercised implicitly by the crate's existing capture_manager/map_manager
    // tests once this PR rewired them through these two helpers — what's
    // unique to test here is the join-error text itself, which only surfaces
    // if the spawned closure panics.
    #[tokio::test]
    async fn flatten_spawn_blocking_reports_a_panic_with_the_original_wording() {
        let handle: tokio::task::JoinHandle<Result<(), String>> =
            tokio::task::spawn_blocking(|| panic!("boom"));
        let err = flatten_spawn_blocking(handle).await.unwrap_err();
        assert!(
            err.starts_with("Task join error: "),
            "expected the original \"Task join error: {{}}\" prefix, got {err:?}"
        );
    }

    #[tokio::test]
    async fn spawn_blocking_result_reports_a_panic_with_the_original_wording() {
        let handle: tokio::task::JoinHandle<()> = tokio::task::spawn_blocking(|| panic!("boom"));
        let err = spawn_blocking_result(handle).await.unwrap_err();
        assert!(
            err.starts_with("Task join error: "),
            "expected the original \"Task join error: {{}}\" prefix, got {err:?}"
        );
    }
}
