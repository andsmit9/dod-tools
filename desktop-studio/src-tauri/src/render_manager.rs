// desktop-studio/src-tauri/src/render_manager.rs
//
// Calls into native::hlcr's real render pipeline (renderer.rs/config.rs/
// autosave.rs/scanner.rs — byte-identical to dev, previously orphaned, see
// docs/archive/tauri_parity_audit.md Area 5) instead of the from-scratch
// reimplementation this module used to carry. Owns the Tauri-side
// orchestration dev's now-deleted `hlcr/ui.rs` used to provide: concurrent
// job scheduling, a JIT multi-drive export pool, per-job progress/state,
// a `.render_autosave.json` crash-recovery lockfile, and a wake lock held
// for the batch's duration.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use serde::{Deserialize, Serialize};
use native::hlcr::autosave::{RenderJob as AutosaveJob, RenderJobStatus as AutosaveJobStatus, RenderSessionData};
use native::hlcr::config::{RenderCodec, RenderConfig};
use native::hlcr::renderer::{hold_render_wake_lock, job_reservation_estimate, run_render_job, RenderUpdate, RenderWakeLock};
use native::hlcr::scanner::{scan_folder_background, clip_is_skip_eligible, ClipData};
use native::shared::paths::take_key;
use native::log_markdown;
use tauri::{AppHandle, Emitter};

/// Codec is a string id ("prores" | "dnxhr" | "h264" | "h264_nvenc") mapped
/// to `RenderCodec` via `RenderCodec::from_str_id` — see
/// docs/archive/tauri_parity_audit.md Area 5 for why `h264`/software libx264 exists
/// alongside dev's NVENC-only H.264 variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderBatchPayload {
    pub render_directories: Vec<String>,
    pub codec: String,
    pub fps: u32,
    pub ffmpeg_path: Option<String>,
    /// JIT multi-drive export pool, priority order — `run_render_job` picks
    /// the first entry with 20 GiB+ free.
    pub export_directories: Vec<String>,
    pub max_concurrent_renders: usize,
}

/// Resolve the FFmpeg binary path using the same fallback chain as the
/// legacy `settings::resolve_ffmpeg_path()`: override → bundled local →
/// system PATH.
fn resolve_ffmpeg(override_path: Option<&String>) -> PathBuf {
    if let Some(p) = override_path {
        let pb = PathBuf::from(p);
        if !p.trim().is_empty() && pb.exists() {
            return pb;
        }
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent() {
            let local = parent.join("local/tools/ffmpeg.exe");
            if local.exists() {
                return local;
            }
        }
    PathBuf::from("ffmpeg")
}

fn autosave_path() -> PathBuf {
    native::shared::paths::get_appdata_dir().join(".render_autosave.json")
}

// ── Per-job runtime state ───────────────────────────────────────────────────

struct RenderJobRuntime {
    id: String,
    clip: ClipData,
    status: String, // "Queued" | "Rendering" | "Finished" | "Error" | "Cancelled"
    speed: String,
    progress: u32,
    error_log: Option<String>,
    /// Absolute path to the encoded file, populated once the job finishes
    /// successfully (empty until then, and for recovered jobs — the autosave
    /// snapshot doesn't persist it).
    output_path: String,
    /// Real size of the encoded file, stat'd once at the moment `output_path`
    /// is set (never re-stat'd afterward, so a file deleted later still
    /// shows what it was). `None` until then — the frontend shows a dash
    /// rather than an estimate for anything not yet Finished; see
    /// `job_reservation_estimate`'s own doc comment for why a per-row
    /// estimate isn't shown instead.
    output_size_bytes: Option<u64>,
    cancel_flag: Arc<AtomicBool>,
    // Snapshotted onto the job itself (VirtualDub-style job queue, not one
    // shared live config) so Reset to Queued always re-renders with exactly
    // what this job was queued with — never silently picks up a codec/fps
    // change made to the panel afterward, and never silently ignores one
    // either: the Settings column shows precisely what's about to run.
    codec: RenderCodec,
    fps: u32,
}

#[derive(Clone, Serialize)]
pub struct RenderJobView {
    pub id: String,
    pub name: String,
    pub stream: String,
    pub frames: usize,
    pub date: String,
    pub status: String,
    pub speed: String,
    pub progress: u32,
    pub error_log: Option<String>,
    /// e.g. "ProRes @ 300fps" — this job's own settings, not the panel's
    /// current ones. A single summary string rather than separate
    /// codec/fps columns so a future new setting doesn't need its own
    /// column too.
    pub settings_summary: String,
    /// Encoded output file, once finished. Empty until then.
    pub output_path: String,
    /// Real size of the encoded file, once finished. `None` otherwise — the
    /// frontend shows a dash rather than a per-row estimate for anything not
    /// yet Finished.
    pub output_size_bytes: Option<u64>,
    /// Source take folder (captured BMPs/WAV) — always known, even before
    /// this job has rendered, so "reveal in Explorer" works pre- and
    /// post-render.
    pub take_folder: String,
    /// This job's codec as a `RenderCodec::to_str_id()` value — what
    /// `set_render_job_codec` expects back, and what the frontend checks
    /// against `"source_copy"` to show the Skip toggle as checked.
    pub codec_id: String,
    /// Whether "Skip (keep original)" is an offerable choice for this job —
    /// only true for an OBS-shaped clip (its own muxed-in audio, not HUD/alpha,
    /// a captured video). `set_render_job_codec` enforces the same rule; this
    /// is what lets the frontend decide whether to show the toggle at all.
    pub skip_available: bool,
}

impl RenderJobRuntime {
    fn to_view(&self) -> RenderJobView {
        let is_source_copy = self.codec == RenderCodec::SourceCopy;
        RenderJobView {
            id: self.id.clone(),
            name: self.clip.base_name.clone(),
            stream: if self.clip.clip_type == "hud_only" { "HUD ONLY".to_string() } else { self.clip.img_folder.clone() },
            frames: self.clip.frame_count,
            date: self.clip.date.clone(),
            status: self.status.clone(),
            speed: self.speed.clone(),
            progress: self.progress,
            error_log: self.error_log.clone(),
            // Skip mode never reads `fps` — showing it would imply a setting
            // that has no effect on a plain file copy.
            settings_summary: if is_source_copy {
                self.codec.label().to_string()
            } else {
                format!("{} @ {}fps", self.codec.label(), self.fps)
            },
            output_path: self.output_path.clone(),
            output_size_bytes: self.output_size_bytes,
            take_folder: self.clip.take_folder.clone(),
            codec_id: self.codec.to_str_id().to_string(),
            skip_available: clip_is_skip_eligible(&self.clip),
        }
    }
}

// ── Manager state ────────────────────────────────────────────────────────────

pub struct RenderManager {
    jobs: Arc<Mutex<Vec<RenderJobRuntime>>>,
    is_rendering: Arc<AtomicBool>,
    global_cancel: Arc<AtomicBool>,
    wake_lock: Arc<Mutex<Option<RenderWakeLock>>>,
    render_session: Arc<Mutex<Option<RenderSessionData>>>,
    /// Persisted so Reset can resume the scheduler on the existing job list
    /// without a fresh scan+payload (mirrors dev's `start_rendering()`
    /// operating on `self.jobs` directly, independent of the scan step).
    last_config: Arc<Mutex<Option<RenderConfig>>>,
    /// JIT export-drive reservation ledger — bytes each concurrently-running
    /// `run_render_job` has provisionally claimed per drive, so one job's
    /// drive check accounts for what other in-flight jobs have already
    /// claimed instead of only seeing the drive's live free-space number
    /// (which several jobs starting in the same scheduler tick would all
    /// see as identical). See docs/capture-render-studio-merge-scope.md §4.
    export_reservations: Arc<Mutex<HashMap<PathBuf, u64>>>,
}

struct SchedulerHandles {
    jobs: Arc<Mutex<Vec<RenderJobRuntime>>>,
    is_rendering: Arc<AtomicBool>,
    global_cancel: Arc<AtomicBool>,
    wake_lock: Arc<Mutex<Option<RenderWakeLock>>>,
    render_session: Arc<Mutex<Option<RenderSessionData>>>,
    export_reservations: Arc<Mutex<HashMap<PathBuf, u64>>>,
}

impl RenderManager {
    pub fn new() -> Self {
        Self {
            jobs: Arc::new(Mutex::new(Vec::new())),
            is_rendering: Arc::new(AtomicBool::new(false)),
            global_cancel: Arc::new(AtomicBool::new(false)),
            wake_lock: Arc::new(Mutex::new(None)),
            render_session: Arc::new(Mutex::new(None)),
            last_config: Arc::new(Mutex::new(None)),
            export_reservations: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn handles(&self) -> SchedulerHandles {
        SchedulerHandles {
            jobs: self.jobs.clone(),
            is_rendering: self.is_rendering.clone(),
            global_cancel: self.global_cancel.clone(),
            wake_lock: self.wake_lock.clone(),
            render_session: self.render_session.clone(),
            export_reservations: self.export_reservations.clone(),
        }
    }
}

fn write_autosave(render_session: &Arc<Mutex<Option<RenderSessionData>>>, jobs: &[RenderJobRuntime], config: &RenderConfig) {
    let session = RenderSessionData {
        source_folder: config.source_folder.clone(),
        fps: config.fps,
        target_codec: config.target_codec.to_str_id().to_string(),
        jobs: jobs.iter().map(|j| AutosaveJob {
            take_folder: j.clip.take_folder.clone(),
            output_path: j.output_path.clone(),
            status: AutosaveJobStatus::Pending,
            name: j.clip.base_name.clone(),
        }).collect(),
    };
    if let Ok(json) = serde_json::to_string_pretty(&session)
        && let Err(e) = std::fs::write(autosave_path(), json) {
            log::warn!("[render_autosave] Failed to write lockfile: {}", e);
        }
    *render_session.lock().unwrap() = Some(session);
}

fn emit_jobs_snapshot(app: &AppHandle, jobs: &Arc<Mutex<Vec<RenderJobRuntime>>>) {
    let views: Vec<RenderJobView> = jobs.lock().unwrap().iter().map(RenderJobRuntime::to_view).collect();
    let _ = app.emit("render_jobs_snapshot", views);
}

fn apply_render_update(
    app: &AppHandle,
    jobs: &Arc<Mutex<Vec<RenderJobRuntime>>>,
    render_session: &Arc<Mutex<Option<RenderSessionData>>>,
    update: RenderUpdate,
) {
    match update {
        RenderUpdate::Progress(id, pct) => {
            if let Some(job) = jobs.lock().unwrap().iter_mut().find(|j| j.id == id) {
                job.progress = pct;
            }
        }
        RenderUpdate::Speed(id, speed) => {
            if let Some(job) = jobs.lock().unwrap().iter_mut().find(|j| j.id == id) {
                job.speed = speed;
            }
        }
        RenderUpdate::Status(id, status) => {
            if let Some(job) = jobs.lock().unwrap().iter_mut().find(|j| j.id == id) {
                job.status = status;
            }
        }
        RenderUpdate::OutputPath(id, path) => {
            if let Some(job) = jobs.lock().unwrap().iter_mut().find(|j| j.id == id) {
                // Stat'd once, here, rather than on every `to_view()` call —
                // this only ever arrives once per job, right after the file
                // has been fully written (see run_render_job), so this is
                // both the one moment the size is knowable and the only
                // moment it needs computing.
                job.output_size_bytes = std::fs::metadata(&path).ok().map(|m| m.len());
                job.output_path = path;
            }
        }
        RenderUpdate::Finished(id, success, err_log) => {
            // `just_finished` is keyed on `success` alone, deliberately not
            // on the `job.status == "Rendering"` guard below: on the real
            // completion path run_render_job sends Status("Finished") *then*
            // Finished(true, None), so by the time this arrives job.status
            // already reads "Finished" and that guard has already tripped —
            // it exists to stop this generic transition from clobbering a
            // more specific status a preceding Status update already set
            // (e.g. never overwrite "Cancelled" with "Error"), not to gate
            // "is this the one authoritative completion". Each run_render_job
            // invocation sends exactly one Finished, at its single return
            // point, so success=true can't double-fire this within one run;
            // recover_render_batch separately can't trigger it at all, since
            // it repopulates already-Finished jobs by constructing them
            // directly rather than replaying this update.
            let mut just_finished = None;
            let mut just_failed = None;
            {
                let mut guard = jobs.lock().unwrap();
                if let Some(job) = guard.iter_mut().find(|j| j.id == id) {
                    if job.status == "Rendering" {
                        job.status = if err_log.is_some() { "Error".to_string() } else { "Finished".to_string() };
                        if job.status == "Finished" {
                            job.progress = 100;
                        }
                    }
                    if success {
                        just_finished = Some((
                            job.clip.take_folder.clone(),
                            job.clip.base_name.clone(),
                            job.clip.clip_type.clone(),
                            job.output_path.clone(),
                        ));
                    } else {
                        just_failed = Some((job.clip.base_name.clone(), err_log.clone()));
                    }
                    job.error_log = err_log;
                }
            }
            if let Some((take_folder, base_name, clip_type, _)) = &just_finished {
                let key = take_key(std::path::Path::new(take_folder));
                log_markdown(&format!(
                    "[render-take-finished] job {} take_key={:?} take_folder={} base_name={} clip_type={}",
                    id, key, take_folder, base_name, clip_type
                ));
                let _ = app.emit("render_take_finished", serde_json::json!({
                    "job_id": id.clone(),
                    "take_key": key,
                    "take_folder": take_folder,
                    "base_name": base_name,
                    "clip_type": clip_type,
                }));
            }
            if let Some((base_name, err_log)) = &just_failed {
                log_markdown(&format!(
                    "[render-take-failed] job {} base_name={} error={}",
                    id, base_name, err_log.as_deref().unwrap_or("(no error log)")
                ));
            }
            // Autosave only tracks success — matches dev's ui.rs exactly.
            if success
                && let Some(session) = render_session.lock().unwrap().as_mut() {
                    if let Ok(idx) = id.parse::<usize>()
                        && let Some(rj) = session.jobs.get_mut(idx) {
                            rj.status = AutosaveJobStatus::Completed;
                            if let Some((_, _, _, output_path)) = &just_finished {
                                rj.output_path = output_path.clone();
                            }
                        }
                    if let Ok(json) = serde_json::to_string_pretty(session) {
                        let _ = std::fs::write(autosave_path(), json);
                    }
                }
        }
    }
}

/// Concurrent job scheduler — ports dev's `HlcrState::update_channels`
/// per-frame scheduling loop (`hlcr/ui.rs`, deleted at HEAD) to a periodic
/// async poll: starts queued jobs up to `max_concurrent_renders`, giving a
/// lone job all available CPU threads via the same `effective_concurrent`
/// calculation dev used, and tears down (wake lock, autosave lockfile) once
/// nothing is Rendering or Queued.
fn spawn_scheduler(app: AppHandle, handles: SchedulerHandles, config: RenderConfig) {
    tokio::spawn(async move {
        let (tx, rx) = std::sync::mpsc::channel::<RenderUpdate>();
        let max_concurrent = config.max_concurrent_renders.max(1);

        loop {
            if handles.global_cancel.load(Ordering::SeqCst) {
                let mut guard = handles.jobs.lock().unwrap();
                for job in guard.iter_mut() {
                    if job.status == "Rendering" || job.status == "Queued" {
                        job.cancel_flag.store(true, Ordering::Relaxed);
                        if job.status == "Queued" {
                            job.status = "Cancelled".to_string();
                        }
                    }
                }
            }

            let mut dirty = false;
            while let Ok(update) = rx.try_recv() {
                apply_render_update(&app, &handles.jobs, &handles.render_session, update);
                dirty = true;
            }

            let started_any = {
                let mut guard = handles.jobs.lock().unwrap();
                let active_count = guard.iter().filter(|j| j.status == "Rendering").count();
                let mut started = 0usize;
                if active_count < max_concurrent {
                    let limit = max_concurrent - active_count;
                    let queued_count = guard.iter().filter(|j| j.status == "Queued").count();
                    let jobs_starting = queued_count.min(limit);
                    // Real concurrent count (already-running + newly starting), capped —
                    // gives a lone job all available threads instead of only 1/max of them.
                    let effective_concurrent = (active_count + jobs_starting).min(max_concurrent).max(1);

                    for job in guard.iter_mut() {
                        if started >= limit {
                            break;
                        }
                        if job.status == "Queued" {
                            job.status = "Rendering".to_string();
                            let job_id = job.id.clone();
                            let clip = job.clip.clone();
                            let cancel_flag = job.cancel_flag.clone();
                            // Codec/fps come from the job itself, not the
                            // scheduler's shared config — see the comment on
                            // RenderJobRuntime. Everything else (ffmpeg path,
                            // export pool, concurrency) genuinely is
                            // batch-wide, so still comes from config.
                            let mut job_config = config.clone();
                            job_config.target_codec = job.codec;
                            job_config.fps = job.fps;
                            job_config.max_concurrent_renders = effective_concurrent;
                            let tx2 = tx.clone();
                            let reservations = handles.export_reservations.clone();
                            tokio::spawn(async move {
                                run_render_job(job_id, clip, job_config, tx2, cancel_flag, reservations).await;
                            });
                            started += 1;
                        }
                    }
                }
                started > 0
            };

            if dirty || started_any {
                emit_jobs_snapshot(&app, &handles.jobs);
            }

            let has_active_or_queued = {
                let guard = handles.jobs.lock().unwrap();
                guard.iter().any(|j| j.status == "Rendering" || j.status == "Queued")
            };
            if !has_active_or_queued {
                break;
            }

            tokio::time::sleep(Duration::from_millis(150)).await;
        }

        log_markdown("[render] scheduler loop exiting: nothing left Rendering or Queued");
        handles.is_rendering.store(false, Ordering::SeqCst);
        handles.global_cancel.store(false, Ordering::SeqCst);
        *handles.wake_lock.lock().unwrap() = None; // Drop releases the wake lock.

        let _ = std::fs::remove_file(autosave_path());
        *handles.render_session.lock().unwrap() = None;

        emit_jobs_snapshot(&app, &handles.jobs);
        let _ = app.emit("render_batch_finished", serde_json::json!({}));
    });
}

/// Scans and stages a batch as `Queued` jobs — real rows in the job table,
/// each with its own settings snapshot and (for an OBS-shaped take) a
/// Skip toggle — without starting the scheduler. Split from starting so a
/// batch has a genuine "queued, not yet running" window: the previous
/// single Start action queued and launched the scheduler in the same call,
/// which for a small batch (especially Skip jobs, which are a plain file
/// copy) left no realistic time to review or toggle anything before it was
/// already done.
#[tauri::command]
pub async fn queue_render_batch(
    app: AppHandle,
    state: tauri::State<'_, RenderManager>,
    payload: RenderBatchPayload,
) -> Result<usize, String> {
    if state.is_rendering.load(Ordering::SeqCst) {
        return Err(crate::messages::RENDER_BATCH_ALREADY_RUNNING_LONG.to_string());
    }
    if state.jobs.lock().unwrap().iter().any(|j| j.status == "Queued" || j.status == "Rendering") {
        return Err(crate::messages::BATCH_ALREADY_QUEUED.to_string());
    }
    log_markdown(&format!(
        "[render] queue_render_batch starting: {} source dir(s), codec={}, fps={}, max_concurrent={}",
        payload.render_directories.len(), payload.codec, payload.fps, payload.max_concurrent_renders
    ));
    state.global_cancel.store(false, Ordering::SeqCst);

    let scan_app = app.clone();
    let scan_result: Vec<ClipData> = crate::messages::spawn_blocking_result(tokio::task::spawn_blocking({
        let dirs = payload.render_directories.clone();
        move || {
            let source_folders: Vec<PathBuf> = dirs.into_iter().map(PathBuf::from).collect();
            let (clip_tx, clip_rx) = std::sync::mpsc::channel();
            let (status_tx, status_rx) = std::sync::mpsc::channel::<String>();

            // scan_folder_background sends one status line per take it finds
            // (blocking, on this thread); drain it concurrently on its own
            // thread so the frontend gets live progress instead of the whole
            // batch arriving at once when the scan finishes. Exits on its own
            // once status_tx drops (scan_folder_background returns below).
            let status_thread = std::thread::spawn(move || {
                while let Ok(msg) = status_rx.recv() {
                    let _ = scan_app.emit("render_scan_status", msg);
                }
            });

            // scan_folder_background is blocking and exhausts sends before returning.
            // try_recv is therefore race-free here.
            scan_folder_background(source_folders, clip_tx, status_tx);
            let _ = status_thread.join();

            let mut clips: Vec<ClipData> = Vec::new();
            while let Ok(clip) = clip_rx.try_recv() {
                clips.push(clip);
            }
            clips
        }
    }))
    .await?;

    let count = scan_result.len();
    if scan_result.is_empty() {
        log_markdown("[render] queue_render_batch: no takes found in the scanned directories");
        return Ok(0);
    }
    log_markdown(&format!("[render] queue_render_batch: {} take(s) found, staged as Queued", count));

    let ffmpeg_path = resolve_ffmpeg(payload.ffmpeg_path.as_ref()).to_string_lossy().into_owned();
    let export_directories: Vec<PathBuf> = payload.export_directories.iter().map(PathBuf::from).collect();
    let config = RenderConfig {
        ffmpeg_path,
        source_folder: payload.render_directories.first().cloned().unwrap_or_default(),
        export_directories,
        fps: payload.fps.max(1),
        target_codec: RenderCodec::from_str_id(&payload.codec),
        max_concurrent_renders: payload.max_concurrent_renders.max(1),
    };

    let jobs: Vec<RenderJobRuntime> = scan_result.into_iter().enumerate().map(|(i, clip)| RenderJobRuntime {
        id: i.to_string(),
        clip,
        status: "Queued".to_string(),
        speed: String::new(),
        progress: 0,
        error_log: None,
        output_path: String::new(),
        output_size_bytes: None,
        cancel_flag: Arc::new(AtomicBool::new(false)),
        codec: config.target_codec,
        fps: config.fps,
    }).collect();

    write_autosave(&state.render_session, &jobs, &config);
    *state.jobs.lock().unwrap() = jobs;
    *state.last_config.lock().unwrap() = Some(config.clone());

    emit_jobs_snapshot(&app, &state.jobs);

    Ok(count)
}

/// Starts the scheduler on whatever `queue_render_batch` already staged.
/// Takes no settings of its own — a queued job carries its own
/// codec/fps snapshot (including any Skip toggle applied while it sat
/// Queued), and batch-wide infrastructure (ffmpeg path, export pool,
/// concurrency) came from `queue_render_batch`'s payload via `last_config`.
#[tauri::command]
pub async fn start_queued_render(app: AppHandle, state: tauri::State<'_, RenderManager>) -> Result<(), String> {
    if state.is_rendering.swap(true, Ordering::SeqCst) {
        log_markdown("[render] start_queued_render rejected: a batch is already in progress");
        return Err(crate::messages::RENDER_BATCH_ALREADY_IN_PROGRESS.to_string());
    }
    let config = state.last_config.lock().unwrap().clone();
    let has_queued = state.jobs.lock().unwrap().iter().any(|j| j.status == "Queued");
    let Some(config) = config.filter(|_| has_queued) else {
        state.is_rendering.store(false, Ordering::SeqCst);
        return Err(crate::messages::NOTHING_QUEUED_TO_RENDER.to_string());
    };

    log_markdown("[render] start_queued_render: starting scheduler");
    state.global_cancel.store(false, Ordering::SeqCst);
    *state.wake_lock.lock().unwrap() = hold_render_wake_lock();
    spawn_scheduler(app, state.handles(), config);

    Ok(())
}

#[tauri::command]
pub async fn cancel_render_batch(app: AppHandle, state: tauri::State<'_, RenderManager>) -> Result<(), String> {
    log_markdown("[render] cancel_render_batch requested");
    if state.is_rendering.load(Ordering::SeqCst) {
        state.global_cancel.store(true, Ordering::SeqCst);
    } else {
        // Nothing is actively rendering, so this is a staged batch (queued
        // via queue_render_batch, Start not yet clicked) — no scheduler loop
        // is running to ever observe global_cancel in that state. Mark every
        // Queued job Cancelled directly instead — mirrors exactly what the
        // scheduler loop above does when global_cancel fires mid-render, so
        // Cancel All behaves the same regardless of whether Start was ever
        // clicked. Rows stay in the list (Reset can still revive one, Remove
        // All clears them) rather than being wiped outright.
        let mut jobs = state.jobs.lock().unwrap();
        for job in jobs.iter_mut() {
            if job.status == "Queued" {
                job.cancel_flag.store(true, Ordering::Relaxed);
                job.status = "Cancelled".to_string();
            }
        }
        drop(jobs);
        emit_jobs_snapshot(&app, &state.jobs);
    }
    Ok(())
}

/// Changes one Queued job's codec — the VirtualDub-style per-job setting
/// `RenderJobRuntime` already carries, exposed here specifically so a job can
/// be flipped to/from `SourceCopy` ("Skip, keep original") before it starts.
/// Refused once a job is no longer Queued: a running or finished job's
/// settings are fixed, matching `reset_render_job`'s own doc comment on why
/// per-job settings never change out from under a job in flight.
#[tauri::command]
pub async fn set_render_job_codec(app: AppHandle, state: tauri::State<'_, RenderManager>, job_id: String, codec: String) -> Result<(), String> {
    let requested = RenderCodec::from_str_id(&codec);
    let mut jobs = state.jobs.lock().unwrap();
    let Some(job) = jobs.iter_mut().find(|j| j.id == job_id) else {
        return Err(crate::messages::no_such_job(&job_id));
    };
    if job.status != "Queued" {
        return Err(crate::messages::job_not_queued(&job_id, &job.status));
    }
    if requested == RenderCodec::SourceCopy && !clip_is_skip_eligible(&job.clip) {
        return Err(crate::messages::SKIP_ONLY_FOR_OBS_TAKE.to_string());
    }
    job.codec = requested;
    log_markdown(&format!("[render] set_render_job_codec {} -> {}", job_id, requested.to_str_id()));
    drop(jobs);
    emit_jobs_snapshot(&app, &state.jobs);
    Ok(())
}

#[tauri::command]
pub async fn cancel_render_job(app: AppHandle, state: tauri::State<'_, RenderManager>, job_id: String) -> Result<(), String> {
    log_markdown(&format!("[render] cancel_render_job requested for {}", job_id));
    let mut jobs = state.jobs.lock().unwrap();
    let mutated = if let Some(job) = jobs.iter_mut().find(|j| j.id == job_id) {
        job.cancel_flag.store(true, Ordering::Relaxed);
        if job.status == "Queued" {
            job.status = "Cancelled".to_string();
            true
        } else {
            false
        }
    } else {
        false
    };
    drop(jobs);
    // A Queued job's status just changed here, directly — not from inside
    // the scheduler loop, which is what every other snapshot emission relies
    // on and isn't running at all if Start Render Batch hasn't been clicked
    // yet. Without this, the row silently stays "Queued" in the UI until
    // something unrelated happens to trigger the next snapshot. See #121.
    if mutated {
        emit_jobs_snapshot(&app, &state.jobs);
    }
    Ok(())
}

/// Once a removal empties the job list entirely, there's nothing left to
/// Reset against — clear the same batch-scoped state `cancel_render_batch`
/// used to wipe unconditionally, so a stale `last_config`/autosave file
/// doesn't linger describing a batch that no longer has any rows.
fn clear_batch_state_if_empty(state: &RenderManager) {
    if state.jobs.lock().unwrap().is_empty() {
        *state.last_config.lock().unwrap() = None;
        let _ = std::fs::remove_file(autosave_path());
        *state.render_session.lock().unwrap() = None;
    }
}

/// Deletes one row outright — unlike Cancel, there's no Reset back from this.
/// Refused for a Rendering job: pulling its row out from under a live ffmpeg
/// process (still writing to `export_reservations`) has no defined outcome.
/// The frontend never shows the button in that state either; this is the
/// defense-in-depth backstop, matching `set_render_job_codec`'s own guard.
#[tauri::command]
pub async fn remove_render_job(app: AppHandle, state: tauri::State<'_, RenderManager>, job_id: String) -> Result<(), String> {
    let mut jobs = state.jobs.lock().unwrap();
    let Some(idx) = jobs.iter().position(|j| j.id == job_id) else {
        return Err(crate::messages::no_such_job(&job_id));
    };
    if jobs[idx].status == "Rendering" {
        return Err(crate::messages::job_still_rendering(&job_id));
    }
    jobs.remove(idx);
    drop(jobs);
    log_markdown(&format!("[render] remove_render_job {} removed", job_id));
    clear_batch_state_if_empty(&state);
    emit_jobs_snapshot(&app, &state.jobs);
    Ok(())
}

/// Bulk form of `remove_render_job` — clears every row except whatever is
/// actively Rendering (Queued, Cancelled, Finished and Error all qualify).
#[tauri::command]
pub async fn remove_non_rendering_render_jobs(app: AppHandle, state: tauri::State<'_, RenderManager>) -> Result<(), String> {
    let mut jobs = state.jobs.lock().unwrap();
    let before = jobs.len();
    jobs.retain(|j| j.status == "Rendering");
    let removed_count = before - jobs.len();
    drop(jobs);
    if removed_count == 0 {
        return Ok(());
    }
    log_markdown(&format!("[render] remove_non_rendering_render_jobs removed {} job(s)", removed_count));
    clear_batch_state_if_empty(&state);
    emit_jobs_snapshot(&app, &state.jobs);
    Ok(())
}

/// Resets a Finished/Error/Cancelled job back to Queued. If the batch has
/// already fully drained (nothing else Rendering/Queued), immediately
/// resumes the scheduler on the existing job list — a small UX improvement
/// over dev, which required a separate "Start Render" click after Reset to
/// notice the re-queued job at all.
///
/// Deliberately does not touch `codec`/`fps` — a reset job re-renders with
/// exactly the settings it already carries (VirtualDub-style: each job owns
/// its settings, the panel is only ever a template for *new* jobs), never
/// whatever the panel currently shows. The `last_config` used below to
/// resume the scheduler only supplies batch-wide infrastructure (ffmpeg
/// path, export pool, concurrency) — never per-job creative settings.
/// Shared tail of `reset_render_job`/`reset_all_render_jobs`: a reset job is
/// worthless sitting at Queued forever, so bring the scheduler back if
/// nothing is already driving it. If one is already running (mid-batch reset
/// of one row while others are still Rendering), it will pick the newly
/// Queued row(s) up on its own next tick — nothing more to do here.
fn resume_scheduler_if_idle(app: AppHandle, state: &RenderManager, context: &str) {
    if !state.is_rendering.swap(true, Ordering::SeqCst) {
        let config = state.last_config.lock().unwrap().clone();
        match config {
            Some(config) => {
                log_markdown(&format!("[render] {} resumed the scheduler", context));
                state.global_cancel.store(false, Ordering::SeqCst);
                if state.wake_lock.lock().unwrap().is_none() {
                    *state.wake_lock.lock().unwrap() = hold_render_wake_lock();
                }
                spawn_scheduler(app, state.handles(), config);
            }
            None => {
                log_markdown(&format!(
                    "[render] {} could not resume: no last_config to schedule against",
                    context
                ));
                state.is_rendering.store(false, Ordering::SeqCst);
            }
        }
    } else {
        log_markdown(&format!(
            "[render] {} left as Queued — a scheduler is already running and will pick it up",
            context
        ));
    }
}

#[tauri::command]
pub async fn reset_render_job(app: AppHandle, state: tauri::State<'_, RenderManager>, job_id: String) -> Result<(), String> {
    let mut previous_status = None;
    {
        let mut jobs = state.jobs.lock().unwrap();
        if let Some(job) = jobs.iter_mut().find(|j| j.id == job_id) {
            previous_status = Some(job.status.clone());
            job.status = "Queued".to_string();
            job.progress = 0;
            job.speed = String::new();
            job.error_log = None;
            job.output_size_bytes = None;
            job.cancel_flag = Arc::new(AtomicBool::new(false));
        }
    }
    log_markdown(&format!(
        "[render] reset_render_job {} (was {:?}) -> Queued",
        job_id, previous_status
    ));
    emit_jobs_snapshot(&app, &state.jobs);
    resume_scheduler_if_idle(app, &state, &format!("reset_render_job {}", job_id));
    Ok(())
}

/// Bulk form of `reset_render_job` — every Cancelled/Finished/Error row goes
/// back to Queued in one shot, e.g. after fixing whatever caused a batch of
/// failures and wanting to retry all of them without clicking Reset one row
/// at a time.
#[tauri::command]
pub async fn reset_all_render_jobs(app: AppHandle, state: tauri::State<'_, RenderManager>) -> Result<(), String> {
    let mut reset_count = 0usize;
    {
        let mut jobs = state.jobs.lock().unwrap();
        for job in jobs.iter_mut() {
            if job.status == "Cancelled" || job.status == "Finished" || job.status == "Error" {
                job.status = "Queued".to_string();
                job.progress = 0;
                job.speed = String::new();
                job.error_log = None;
                job.output_size_bytes = None;
                job.cancel_flag = Arc::new(AtomicBool::new(false));
                reset_count += 1;
            }
        }
    }
    if reset_count == 0 {
        return Ok(());
    }
    log_markdown(&format!("[render] reset_all_render_jobs: {} job(s) -> Queued", reset_count));
    emit_jobs_snapshot(&app, &state.jobs);
    resume_scheduler_if_idle(app, &state, "reset_all_render_jobs");
    Ok(())
}

/// Summed required-bytes estimate across every job still ahead of the
/// export pool — Queued and Rendering, not Finished/Error/Cancelled, which
/// have either already landed on disk or never will. Exposed so the Render
/// footer can show a "Required (Estimated)" figure mirroring Capture's own
/// Free+Required pair (issue #119).
///
/// Deliberately whole-queue, not just `export_reservations` (the JIT ledger,
/// which only ever holds bytes for jobs *currently* rendering) — a figure
/// that only reflects what's already in flight can't answer "will my batch
/// fit," which is the question this exists to answer. And deliberately a
/// loose upper bound, not a tight prediction, unlike Capture's figure — see
/// `job_reservation_estimate`'s own doc comment and #116's PR description
/// for real-vs-estimated size ratios observed live (~70-160x for
/// h264_nvenc). The frontend's label says so explicitly rather than implying
/// a number this loose is a real prediction.
#[tauri::command]
pub fn get_render_required_estimate_gb(state: tauri::State<'_, RenderManager>) -> f64 {
    let total: u64 = state.jobs.lock().unwrap().iter()
        .filter(|j| j.status == "Queued" || j.status == "Rendering")
        .map(|j| job_reservation_estimate(&j.clip, j.codec == RenderCodec::SourceCopy))
        .sum();
    total as f64 / 1_073_741_824.0
}

#[tauri::command]
pub fn get_export_pool_free_gb(directories: Vec<String>) -> f64 {
    let mut total: u64 = 0;
    for dir in &directories {
        let free = native::sys::disk::get_available_bytes(&PathBuf::from(dir));
        if free != u64::MAX {
            total += free;
        }
    }
    total as f64 / 1_073_741_824.0
}

#[derive(Serialize)]
pub struct RenderAutosaveSummary {
    pub source_folder: String,
    pub pending_count: usize,
    pub completed_count: usize,
}

#[tauri::command]
pub fn check_render_autosave(state: tauri::State<'_, RenderManager>) -> Option<RenderAutosaveSummary> {
    // A render can legitimately still be running when this fires: pressing
    // F5 reloads the frontend only, not this Rust process, so the autosave
    // file (written continuously as jobs finish, not just at batch end)
    // genuinely exists on disk even though nothing was actually interrupted
    // — the "recovery" prompt was firing on every reload of an active batch.
    // Only treat it as a real interruption when nothing is actively running.
    if state.is_rendering.load(Ordering::SeqCst) {
        return None;
    }
    let json = std::fs::read_to_string(autosave_path()).ok()?;
    let session: RenderSessionData = serde_json::from_str(&json).ok()?;
    let pending_count = session.jobs.iter().filter(|j| j.status == AutosaveJobStatus::Pending).count();
    let completed_count = session.jobs.iter().filter(|j| j.status == AutosaveJobStatus::Completed).count();
    Some(RenderAutosaveSummary { source_folder: session.source_folder, pending_count, completed_count })
}

#[tauri::command]
pub fn discard_render_autosave() -> Result<(), String> {
    match std::fs::remove_file(autosave_path()) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// Repopulates the job table from `.render_autosave.json` — Completed jobs
/// show as "Finished"/100%, Pending ones as "Queued"/0%, both via stub
/// `ClipData` (the snapshot only stores take_folder/name/status, not the
/// full scanner output). Matches dev's own recovery flow exactly
/// (`main.rs`'s render-recovery modal rebuilds the same kind of stub with
/// blank stream/frames/date — dev never had richer data to recover either).
/// Does not auto-start rendering; the user still clicks Start Render.
#[tauri::command]
pub fn recover_render_batch(state: tauri::State<'_, RenderManager>) -> Result<Vec<RenderJobView>, String> {
    let json = std::fs::read_to_string(autosave_path()).map_err(|e| e.to_string())?;
    let session: RenderSessionData = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    // The autosave snapshot only ever recorded one session-wide codec/fps
    // (written once at batch start, before per-job settings existed) — every
    // recovered job gets that same pair. A job individually changed before a
    // crash — via `reset_render_job`'s codec/fps carry-over, or via
    // `set_render_job_codec`'s "Skip" toggle — won't recover with that
    // override; a known, pre-existing recovery-fidelity gap (see the doc
    // comment below), not a regression from per-job settings. A recovered
    // "Skip" job simply comes back as whatever `session.target_codec` was
    // for the batch, and needs re-toggling by hand if that was Skip.
    let recovered_codec = RenderCodec::from_str_id(&session.target_codec);
    let recovered_fps = session.fps;

    let jobs: Vec<RenderJobRuntime> = session.jobs.iter().enumerate().map(|(i, rj)| {
        let (status, progress) = if rj.status == AutosaveJobStatus::Completed {
            ("Finished".to_string(), 100u32)
        } else {
            ("Queued".to_string(), 0u32)
        };
        // Only worth a stat for a job that actually finished and has a real
        // path recorded — a re-queued job has neither yet.
        let output_size_bytes = (rj.status == AutosaveJobStatus::Completed && !rj.output_path.is_empty())
            .then(|| std::fs::metadata(&rj.output_path).ok().map(|m| m.len()))
            .flatten();
        RenderJobRuntime {
            id: i.to_string(),
            clip: ClipData {
                take_folder: rj.take_folder.clone(),
                clip_type: "single".to_string(),
                img_folder: String::new(),
                // The autosave snapshot stores take_folder/name/status only, so
                // this reconstruction cannot know which kind of take it was —
                // wav-and-frames or OBS-shaped — the same reason `img_folder`
                // and `video_file` are blank below. Guessing `Some("sound.wav")`
                // here used to be harmless when every take had one; now that an
                // OBS take legitimately has none, guessing would misrepresent
                // it. `None` matches the rest of this stub's "unknown, a
                // re-scan is what fills it in" treatment, and fails at
                // `run_render_job`'s clear "no audio source" guard instead of a
                // misleading "sound.wav not found" if resumed without a rescan.
                wav_file: None,
                base_name: rj.name.clone(),
                frame_count: 0,
                width: 0,
                height: 0,
                date: String::new(),
                video_file: None,
                alpha_folder: None,
            },
            status,
            speed: String::new(),
            progress,
            error_log: None,
            output_path: rj.output_path.clone(),
            output_size_bytes,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            codec: recovered_codec,
            fps: recovered_fps,
        }
    }).collect();

    let views: Vec<RenderJobView> = jobs.iter().map(RenderJobRuntime::to_view).collect();

    *state.jobs.lock().unwrap() = jobs;
    *state.render_session.lock().unwrap() = Some(session);
    let _ = std::fs::remove_file(autosave_path());

    Ok(views)
}
