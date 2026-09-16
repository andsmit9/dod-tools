mod capture_manager;
mod render_manager;
mod settings_manager;
mod audit_manager;
mod dir_browser;
mod map_manager;
mod messages;
mod updater_manager;

use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use capture_manager::{CaptureManager, CapturePayload, launch_demo_preview, generate_all_previews, launch_standalone_game, launch_obs, check_engine_processes, kill_engine_processes, scan_orphaned_previews, delete_orphaned_previews, read_cfg_commands};
use render_manager::{
    RenderManager, queue_render_batch, start_queued_render, cancel_render_batch,
    cancel_render_job, reset_render_job, reset_all_render_jobs, remove_render_job,
    remove_non_rendering_render_jobs, set_render_job_codec, get_export_pool_free_gb,
    get_render_required_estimate_gb,
    check_render_autosave, discard_render_autosave, recover_render_batch,
};
use settings_manager::{AppSettings, SettingsManager};
use audit_manager::{AuditManager, SerializedDuplicateGroup};

// ── ScanManager ────────────────────────────────────────────────────────────────

pub struct ScanManager {
    pub is_scanning: Arc<AtomicBool>,
    pub cancel_token: Arc<AtomicBool>,
}

impl Default for ScanManager {
    fn default() -> Self {
        Self {
            is_scanning: Arc::new(AtomicBool::new(false)),
            cancel_token: Arc::new(AtomicBool::new(false)),
        }
    }
}

// ── Settings IPC Commands ──────────────────────────────────────────────────────

#[tauri::command]
async fn get_settings(state: tauri::State<'_, SettingsManager>) -> Result<AppSettings, String> {
    let inner_arc = Arc::clone(&state.inner);
    messages::flatten_spawn_blocking(tokio::task::spawn_blocking(move || {
        let guard = inner_arc.lock().unwrap_or_else(|p| p.into_inner());
        Ok(guard.clone())
    }))
    .await
}

#[tauri::command]
async fn save_settings(
    state: tauri::State<'_, SettingsManager>,
    settings: AppSettings,
) -> Result<(), String> {
    let inner_arc = Arc::clone(&state.inner);
    messages::flatten_spawn_blocking(tokio::task::spawn_blocking(move || {
        settings.save()?;
        let mut guard = inner_arc.lock().unwrap_or_else(|p| p.into_inner());
        *guard = settings;
        Ok(())
    }))
    .await
}

// ── Project Session IPC Commands ───────────────────────────────────────────────
// `fs:default` (capabilities/default.json) only grants read access to the app's
// own AppConfig/AppData dirs — it does NOT scope arbitrary user-picked paths, so
// the JS `@tauri-apps/plugin-fs` read/writeTextFile calls fail for every path a
// save/open dialog can return. Do the actual I/O in Rust (std::fs, unscoped)
// instead, same as `save_settings`/`get_settings` above.

#[tauri::command]
async fn save_project_session(path: String, contents: String) -> Result<(), String> {
    messages::flatten_spawn_blocking(tokio::task::spawn_blocking(move || {
        std::fs::write(&path, contents).map_err(|e| messages::failed_to_write_file(&path, e))
    }))
    .await
}

#[tauri::command]
async fn load_project_session(path: String) -> Result<String, String> {
    messages::flatten_spawn_blocking(tokio::task::spawn_blocking(move || {
        std::fs::read_to_string(&path).map_err(|e| messages::failed_to_read_file(&path, e))
    }))
    .await
}

// ── Auditor IPC Commands ───────────────────────────────────────────────────────

#[tauri::command]
async fn run_demo_audit(
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AuditManager>,
    paths: Vec<String>,
) -> Result<Vec<SerializedDuplicateGroup>, String> {
    audit_manager::run_demo_audit_impl(
        app_handle,
        Arc::clone(&state.is_running),
        Arc::clone(&state.cancel_token),
        paths,
    ).await
}

#[tauri::command]
fn delete_audit_files(paths: Vec<String>) -> Result<(), String> {
    audit_manager::delete_audit_files_impl(paths)
}

#[tauri::command]
fn cancel_audit(state: tauri::State<'_, AuditManager>) -> Result<(), String> {
    state.cancel_token.store(true, Ordering::SeqCst);
    Ok(())
}

#[tauri::command]
fn reveal_in_explorer(path: String) -> Result<(), String> {
    audit_manager::reveal_in_explorer_impl(path)
}

// ── Tauri commands ─────────────────────────────────────────────────────────────

/// Triggers a batch capture run from the Vite frontend.
///
/// Async so the Tauri dispatcher never blocks the main thread.
/// The heavy lifting is delegated inside `start_capture_batch_impl`
/// via `tokio::task::spawn_blocking`.
#[tauri::command]
async fn start_capture_batch(
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, CaptureManager>,
    payload: CapturePayload,
) -> Result<(), String> {
    capture_manager::start_capture_batch_impl(app_handle, &state, payload).await
}

/// Cancel a running capture batch.
#[tauri::command]
async fn cancel_capture_batch(state: tauri::State<'_, CaptureManager>) -> Result<(), String> {
    native::log_markdown(&format!(
        "[capture] Cancel requested (is_running={})",
        state.is_running()
    ));
    state
        .cancel_token
        .store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// Writes one line to today's activity log from the frontend. Exists for
/// events that have no other Tauri command to piggyback logging on — the
/// Master Demo Queue's Clear Untracked/Selected/All and row-delete actions
/// are pure frontend array mutations with nothing else calling into Rust.
#[tauri::command]
fn log_frontend_event(message: String) {
    native::log_markdown(&message);
}

/// Absolute path to today's activity log, for the top nav's "View Logs"
/// button — most users won't know to go looking under AppData on their own.
#[tauri::command]
fn get_activity_log_path() -> String {
    native::activity_log_path().to_string_lossy().to_string()
}

#[tauri::command]
async fn scan_directory(
    app_handle: tauri::AppHandle,
    scan_state: tauri::State<'_, ScanManager>,
    paths: Vec<String>,
) -> Result<Vec<capture_manager::SerializedDemo>, String> {
    capture_manager::scan_directory_impl(
        app_handle,
        Arc::clone(&scan_state.is_scanning),
        Arc::clone(&scan_state.cancel_token),
        paths,
    ).await
}

#[tauri::command]
fn cancel_scan(scan_state: tauri::State<'_, ScanManager>) -> Result<(), String> {
    scan_state.cancel_token.store(true, Ordering::SeqCst);
    Ok(())
}

#[tauri::command]
fn calculate_export_pool_space(paths: Vec<String>) -> Result<u64, String> {
    let mut total: u64 = 0;
    for path_str in paths {
        let p = std::path::PathBuf::from(path_str);
        let space = native::sys::disk::get_available_bytes(&p);
        if space != u64::MAX {
            total += space;
        }
    }
    Ok(total)
}

#[derive(serde::Serialize)]
struct CaptureOutputDiagnostic {
    path: String,
    status: &'static str, // "ok" | "not_absolute" | "malformed" | "not_found" | "not_a_directory"
    // Whether get_available_bytes (the same function backing the aggregate
    // sum/footer) would actually count this path — a "not_found" path whose
    // drive is real and mounted still passes (many output folders are
    // auto-created at write time), while one on an unmounted/nonexistent
    // drive doesn't. Reusing that exact check here, rather than deriving a
    // second opinion from `status` alone, is what keeps this list and the
    // footer's byte total from disagreeing about the same path.
    usable: bool,
}

#[tauri::command]
fn diagnose_capture_output_paths(paths: Vec<String>) -> Vec<CaptureOutputDiagnostic> {
    paths
        .into_iter()
        .map(|path_str| {
            let p = std::path::PathBuf::from(&path_str);
            let status = match native::sys::disk::diagnose_path(&p) {
                native::sys::disk::PathStatus::Ok => "ok",
                native::sys::disk::PathStatus::NotAbsolute => "not_absolute",
                native::sys::disk::PathStatus::Malformed => "malformed",
                native::sys::disk::PathStatus::NotFound => "not_found",
                native::sys::disk::PathStatus::NotADirectory => "not_a_directory",
            };
            let usable = native::sys::disk::get_available_bytes(&p) != u64::MAX;
            CaptureOutputDiagnostic { path: path_str, status, usable }
        })
        .collect()
}

#[tauri::command]
async fn validate_paths(hlae_path: String, hl_path: String) -> Result<bool, String> {
    let hlae_p = std::path::Path::new(&hlae_path);
    let hl_p = std::path::Path::new(&hl_path);

    if !hlae_p.exists() || !hlae_p.is_file() {
        return Err(messages::HLAE_EXECUTABLE_NOT_FOUND.into());
    }
    if !hl_p.exists() || !hl_p.is_file() {
        return Err(messages::HL_EXECUTABLE_NOT_FOUND.into());
    }
    Ok(true)
}

/// Whether HLAE can reach an FFmpeg of its own, which is a different question
/// from whether Render Studio can.
///
/// `mirv_movie_ffmpeg` makes HLAE spawn FFmpeg itself and it does not consult
/// the app's resolution chain, so this has to be reported before a batch rather
/// than discovered after one — the failure is a capture that runs to completion
/// and produces no video. See `native::shared::hlae_ffmpeg`.
#[tauri::command]
async fn check_hlae_ffmpeg(
    hlae_path: String,
    ffmpeg_path: Option<String>,
) -> Result<serde_json::Value, String> {
    use native::shared::hlae_ffmpeg as hf;

    let state = hf::detect(std::path::Path::new(&hlae_path));

    // "HLAE is pointed somewhere" and "HLAE is pointed at the same FFmpeg
    // Render Studio uses" are different questions, and only the second keeps
    // both halves of the pipeline encoding with the same build — which was the
    // stated reason for writing an ini instead of copying the binary. Nothing
    // was checking it stayed true, so changing the app's FFmpeg silently left
    // HLAE on the old one.
    let app_ffmpeg = hf::resolve_absolute(ffmpeg_path.as_deref().unwrap_or("ffmpeg"));
    let agrees_with_app = match (&state, &app_ffmpeg) {
        (hf::HlaeFfmpeg::Linked { target, .. }, Some(app)) => Some(hf::same_file(target, app)),
        // A bundled binary is HLAE's own and is meant to differ; with nothing
        // linked there is nothing to disagree with.
        _ => None,
    };

    // "It exists" was the only test the picker applied, and ffplay.exe and
    // ffprobe.exe sit in the same folder as ffmpeg.exe. Checking here as well as
    // at link time means the row says so before the button is pressed, instead
    // of reporting a disagreement between two paths one of which cannot record.
    //
    // Chaining this off `app_ffmpeg` was a hole: a path that does not resolve
    // produces `None`, so the check never ran and a typo'd override said
    // nothing at all. Failing to resolve is itself the problem worth reporting.
    let configured = ffmpeg_path.as_deref().unwrap_or("").trim().to_string();
    let app_ffmpeg_problem = match &app_ffmpeg {
        Some(p) => hf::verify_is_ffmpeg(p).err(),
        None if configured.is_empty() => {
            Some("no ffmpeg.exe was found on PATH, and no override is set".to_string())
        }
        None => Some(format!("there is no file at \"{}\"", configured)),
    };

    // Whether the HLAE Executable above is a working HLAE install, answered by
    // the file the pipeline actually consumes rather than by what the exe calls
    // itself: `build_hlae_process` passes this DLL as `-hookDllPath`, so its
    // absence means a capture cannot work whatever the exe is named. Advisory —
    // reported, never enforced.
    let missing_hook_dll = hf::missing_hook_dll(std::path::Path::new(&hlae_path))
        .map(|p| p.to_string_lossy().into_owned());

    Ok(serde_json::json!({
        "state": state,
        "usable": state.is_usable(),
        "can_link": state.can_link(),
        "app_ffmpeg": app_ffmpeg.map(|p| p.to_string_lossy().into_owned()),
        "agrees_with_app": agrees_with_app,
        "app_ffmpeg_problem": app_ffmpeg_problem,
        "missing_hook_dll": missing_hook_dll,
    }))
}

/// Whether each configured executable path actually points at a file.
///
/// The Path Routing fields accepted anything: `validate_paths` only ran at
/// capture launch, so a typo sat there looking fine until a batch failed
/// minutes later. Returns a state per path rather than a message, so the
/// wording stays in `strings.js` with every other user-facing string.
#[tauri::command]
async fn diagnose_executable_paths(paths: Vec<String>) -> Result<Vec<String>, String> {
    Ok(paths
        .iter()
        .map(|raw| {
            let path = raw.trim();
            if path.is_empty() {
                // Not a complaint: these fields are legitimately blank before
                // they are filled in, and the FFmpeg override is optional.
                return "empty";
            }
            let p = std::path::Path::new(path);
            if p.is_file() {
                "ok"
            } else if p.is_dir() {
                // The classic mistake these fields invite — the folder rather
                // than the executable inside it.
                "not_a_file"
            } else {
                "not_found"
            }
        })
        .map(str::to_string)
        .collect())
}

/// Points HLAE at an FFmpeg by writing `ffmpeg.ini`, on request only.
///
/// Never overwrites an existing ini: HLAE installs are shared with Source work
/// and other projects, so silently repointing one would break somebody else's
/// setup to fix ours. `link` refuses in that case and the error says so.
/// `elevated` retries the same write through a UAC prompt. The HLAE installer
/// puts the target under `Program Files`, so an unelevated write fails for most
/// people — the frontend asks first, then calls back with this set.
#[tauri::command]
async fn link_hlae_ffmpeg(
    hlae_path: String,
    ffmpeg_path: String,
    elevated: Option<bool>,
) -> Result<serde_json::Value, String> {
    use native::shared::hlae_ffmpeg::{self, LinkError};

    let ffmpeg = hlae_ffmpeg::resolve_absolute(&ffmpeg_path)
        .ok_or_else(|| messages::ffmpeg_could_not_be_resolved(&ffmpeg_path))?;

    let hlae = std::path::Path::new(&hlae_path);
    let result = if elevated.unwrap_or(false) {
        hlae_ffmpeg::link_elevated(hlae, &ffmpeg)
    } else {
        hlae_ffmpeg::link(hlae, &ffmpeg)
    };

    match result {
        Ok(ini) => {
            native::log_markdown(&format!(
                "[hlae-ffmpeg] wrote {} pointing at {} — HLAE can now spawn FFmpeg for \
                 `mirv_movie_ffmpeg`.",
                ini.display(),
                ffmpeg.display()
            ));
            Ok(serde_json::json!({ "ini": ini.to_string_lossy() }))
        }
        // Reported as data, not an error string: the frontend has to tell this
        // apart from a real failure so it can offer the prompt rather than show
        // somebody "Access is denied. (os error 5)" and leave them there.
        Err(LinkError::NeedsElevation { ini }) => Ok(serde_json::json!({
            "needs_elevation": true,
            "ini": ini.to_string_lossy(),
        })),
        Err(e) => Err(e.to_string()),
    }
}

// ── Full-fidelity analysis payload for the standalone Demo Analyzer tab ─────────
// Passes the typed `analysis::DemoInfo`/`AnalyzerState` straight through so the frontend can
// reconstruct every report sub-view (Summary/Scoreboard/Player Details/Team
// Details/Timeline/Rounds/Chat) with full fidelity, matching the data the
// egui report views on `dev` were built from.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct AnalyzerReportPayload {
    pub file_name: String,
    pub file_path: String,
    pub file_dir: String,
    pub file_size_mb: f64,
    pub file_created_unix_secs: f64,
    pub demo_info: analysis::DemoInfo,
    pub state: analysis::AnalyzerState,
}

#[tauri::command]
async fn analyze_demo_full(
    app_handle: tauri::AppHandle,
    demo_path: String,
) -> Result<AnalyzerReportPayload, String> {
    messages::flatten_spawn_blocking(tokio::task::spawn_blocking(move || {
        use tauri::Emitter;

        let path = std::path::PathBuf::from(&demo_path);

        if !path.exists() || !path.is_file() {
            return Err(messages::demo_file_not_found(&demo_path));
        }

        // Throttled to ~30fps per CLAUDE.md's telemetry-throttling guardrail —
        // `try_from_bytes_with_progress` calls back every ~500 frames, which
        // can be 1000+ times for a large demo.
        let mut last_emit = std::time::Instant::now() - std::time::Duration::from_secs(1);
        let progress_cb = |processed: usize, total: usize| {
            let now = std::time::Instant::now();
            if now.duration_since(last_emit) >= std::time::Duration::from_millis(33) || processed == total {
                last_emit = now;
                let _ = app_handle.emit(
                    "analyzer_progress",
                    serde_json::json!({ "processed": processed, "total": total }),
                );
            }
        };

        match native::run_analyzer_cached(&path, progress_cb) {
            Ok((file_info, analysis, _from_cache)) => {
                // Independent metadata lookup (not `file_info.created_at`) to avoid
                // depending on `web_time::SystemTime`'s exact type identity here.
                let created_unix_secs = std::fs::metadata(&path)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);

                let file_dir = std::path::Path::new(&file_info.path)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();

                Ok(AnalyzerReportPayload {
                    file_name: file_info.name,
                    file_path: file_info.path,
                    file_dir,
                    file_size_mb: file_info.size_bytes as f64 / 1_048_576.0,
                    file_created_unix_secs: created_unix_secs,
                    demo_info: analysis.demo_info,
                    state: analysis.state,
                })
            }
            Err(e) => Err(messages::analyzer_error(e)),
        }
    }))
    .await
}

/// Every `Weapon` variant's resolved display name, keyed by its raw JSON tag
/// (e.g. `"ScopedK98"` -> "Scoped Kar98k") — the same names
/// `native::patch::scanner` bakes into a kill streak's timeline text, so the
/// frontend's weapon tables (analyzer_pane.js) can show identical text
/// instead of independently re-deriving a name from the raw enum tag.
#[tauri::command]
fn get_weapon_display_names() -> std::collections::HashMap<String, String> {
    analysis::all_weapon_display_names()
}

// ── App entry point ────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(CaptureManager::new())
        .manage(RenderManager::new())
        .manage(ScanManager::default())
        .manage(SettingsManager::new())
        .manage(AuditManager::default())
        .manage(updater_manager::UpdaterState::default())
        .setup(|app| {
            // Dev/debug builds find the repo-root `localizations/` folder via
            // analysis::translate_key's own walk-up-from-exe search, since the
            // exe runs from inside the source tree. A packaged install runs from
            // e.g. Program Files, nowhere near that folder — its `resources`
            // directory (populated by tauri.conf.json's `bundle.resources`) is
            // the only place weapon-name strings can come from there.
            use tauri::Manager;
            if let Ok(resource_dir) = app.path().resource_dir() {
                analysis::add_localization_search_path(resource_dir.join("localizations"));
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            log_frontend_event,
            get_activity_log_path,
            validate_paths,
            check_hlae_ffmpeg,
            diagnose_executable_paths,
            link_hlae_ffmpeg,
            analyze_demo_full,
            get_weapon_display_names,
            start_capture_batch,
            launch_demo_preview,
            generate_all_previews,
            capture_manager::obs_test_connection,
            capture_manager::obs_check_orphan,
            capture_manager::obs_recover_orphan,
            launch_obs,
            launch_standalone_game,
            read_cfg_commands,
            check_engine_processes,
            kill_engine_processes,
            scan_orphaned_previews,
            delete_orphaned_previews,
            cancel_capture_batch,
            scan_directory,
            cancel_scan,
            calculate_export_pool_space,
            diagnose_capture_output_paths,
            queue_render_batch,
            start_queued_render,
            cancel_render_batch,
            cancel_render_job,
            reset_render_job,
            reset_all_render_jobs,
            remove_render_job,
            remove_non_rendering_render_jobs,
            set_render_job_codec,
            get_export_pool_free_gb,
            get_render_required_estimate_gb,
            check_render_autosave,
            discard_render_autosave,
            recover_render_batch,
            get_settings,
            save_settings,
            save_project_session,
            load_project_session,
            run_demo_audit,
            delete_audit_files,
            cancel_audit,
            reveal_in_explorer,
            dir_browser::browse_directory,
            dir_browser::default_browse_dir,
            dir_browser::count_demo_files_in_folder,
            dir_browser::scan_demo_folders,
            map_manager::check_demo_maps,
            map_manager::download_map,
            map_manager::scan_game_configs,
            map_manager::roll_floors,
            updater_manager::check_for_update,
            updater_manager::download_and_install_update,
            updater_manager::restart_app,
            updater_manager::is_debug_build,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
