use std::path::PathBuf;
use std::sync::{Arc, mpsc::Sender, atomic::{AtomicBool, Ordering}};
use crate::log_markdown;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CaptureJob {
    pub patched_demo_path: std::path::PathBuf,
}

#[derive(Clone, Debug)]
pub enum EngineEvent {
    Starting(usize),
    Launching(String),
    Finished(String),
    Error(String),
    AllCompleted,
    /// Posted when the cancellation token is raised mid-batch.
    /// Signals the GUI to reset the running flag and show a cancelled message.
    Cancelled,
    /// A demo in the batch has started playing, read from its `DEMO_START`
    /// console marker: (job_idx, total_jobs, clip_count), 1-based job_idx.
    /// Read from the engine console log, which `-condebug` produces on every
    /// launch. See issue #98.
    DemoLoading(u32, u32, u32),
    /// Playback is about to fast-forward towards a specific clip, read from
    /// its `NEXT_CLIP` console marker: (job_idx, total_jobs, clip_idx,
    /// clip_count), all 1-based. Same console-log source as DemoLoading.
    /// See issue #98.
    FastForwardToClip(u32, u32, u32, u32),
}

/// Longest a batch may go without a console marker before OBS mode calls it
/// stalled.
///
/// Every other guard in the capture loop keys off hl.exe being gone or a file
/// appearing, so none of them notices the engine being alive and well but no
/// longer playing the demo: `disconnect` typed into the console, a demo that
/// failed to load, a frozen engine. On the frame-sequence path that wastes
/// time. In OBS mode it records until the disk fills, which is why the
/// watchdog lives here.
///
/// The floor has to clear the longest *legitimate* silence, which is one
/// breadcrumb interval of real-time playback. That is not a fixed number of
/// seconds — demo ticks are frames, so their wall-clock spacing depends on the
/// fps the demo was recorded at. And an unfocused game stops fast-forwarding
/// altogether (GoldSrc throttles without focus), so alt-tabbing stretches gaps
/// that are normally seconds into minutes. Five minutes clears both.
///
/// Tripping this kills the game and loses the batch, so it is deliberately
/// biased towards waiting too long over firing on a slow one.
const MARKER_STALL_FLOOR: std::time::Duration = std::time::Duration::from_secs(300);

/// How long `hl.exe` may be up before a `qconsole.log` that has not grown is
/// treated as the engine not writing it at all.
///
/// GoldSrc echoes startup output within a second or two of the process
/// appearing, so this only has to clear a slow load. Deliberately generous:
/// the check only logs, but a false positive would tell someone their capture
/// is broken when it is fine.
const CONDEBUG_WRITE_GRACE: std::time::Duration = std::time::Duration::from_secs(60);

/// Size of the engine's console log, or 0 if it is not there.
///
/// Read through an *open handle* rather than `fs::metadata(path)`: on Windows a
/// file's directory entry is not necessarily updated while a handle to it is
/// open, and the engine holds this one open for the whole session. `LogTailer`
/// reads its length the same way, for the same reason.
#[cfg(not(target_arch = "wasm32"))]
fn console_log_len(path: &std::path::Path) -> u64 {
    std::fs::File::open(path)
        .and_then(|f| f.metadata())
        .map(|m| m.len())
        .unwrap_or(0)
}

/// How long to wait for the next marker, given the longest gap this batch has
/// already shown.
///
/// The adaptive term covers a demo whose breadcrumbs really are further apart
/// than the floor allows: a batch that has already demonstrated a four-minute
/// gap is not stalled at five.
fn marker_stall_deadline(longest_gap: std::time::Duration) -> std::time::Duration {
    std::cmp::max(MARKER_STALL_FLOOR, longest_gap * 3)
}

/// Waits, briefly, for `hl.exe` to actually leave the process list after a
/// `taskkill /F` -- `taskkill` returning only means Windows has been asked to
/// end the process, not that it (and its open file handles) are gone yet.
/// `_cleanup_guard`'s `Drop` deletes this batch's demo files the moment the
/// closure that owns `sys` returns, right after every `taskkill` call site
/// below `break`s out of this loop -- without this wait, the very last demo
/// in the chain can still be open when that delete runs. See #198.
///
/// Bounded to 3 seconds so a process that was already gone when `taskkill`
/// ran (the game crashed on its own, or was closed by the user moments
/// earlier) can never turn this into an extra wait on every batch --
/// `remove_file_retrying` is the second, independent line of defence for
/// whatever this window doesn't cover.
fn wait_for_hl_exe_to_exit(sys: &mut sysinfo::System) {
    use sysinfo::{ProcessExt, SystemExt};
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        sys.refresh_processes();
        if !sys.processes().values().any(|p| p.name().eq_ignore_ascii_case("hl.exe")) {
            return;
        }
        if std::time::Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

struct CaptureCleanupGuard {
    exit_trigger: PathBuf,
    session_junction: PathBuf,
    auto_clear_logs: bool,
    auto_clear_temp_demos: bool,
    auto_clear_previews: bool,
    save_local_patched_copy: bool,
    /// OBS's connection, when this batch is driving one.
    ///
    /// Here rather than in the batch loop because this guard is the only thing
    /// that runs on every exit path the process survives — normal completion,
    /// cancel, a mid-batch crash of the game, an early return. Anywhere else
    /// and a cancelled batch leaves OBS recording indefinitely, filling the
    /// user's drive with no indication that anything is wrong.
    ///
    /// Not a panic, though: release builds are `panic = "abort"`, so nothing
    /// unwinds and no `Drop` runs. That gap, a hard kill and a power cut are
    /// all covered by `obs::recover` on the next start instead.
    obs: Option<std::sync::Arc<std::sync::Mutex<Option<crate::obs::ObsClient>>>>,
    _wake_lock: Option<keepawake::KeepAwake>,
}

impl CaptureCleanupGuard {
    fn new(
        exit_trigger: PathBuf,
        session_junction: PathBuf,
        auto_clear_logs: bool,
        auto_clear_temp_demos: bool,
        auto_clear_previews: bool,
        save_local_patched_copy: bool,
    ) -> Self {
        // Pre-clean any stale signal dirs/junctions from a previous aborted run.
        if let Err(e) = std::fs::remove_dir_all(&exit_trigger)
            && e.kind() != std::io::ErrorKind::NotFound {
                log::warn!("[GC::new] Failed to pre-clean exit_trigger {:?}: {}", exit_trigger, e);
            }
        if let Err(e) = std::fs::remove_dir(&session_junction)
            && e.kind() != std::io::ErrorKind::NotFound {
                log::warn!("[GC::new] Failed to pre-clean session_junction {:?}: {}", session_junction, e);
            }
        Self {
            exit_trigger,
            session_junction,
            auto_clear_logs,
            auto_clear_temp_demos,
            auto_clear_previews,
            save_local_patched_copy,
            obs: None,
            _wake_lock: keepawake::Builder::default()
                .display(false)
                .idle(true)
                .sleep(true)
                .create()
                .ok(),
        }
    }
}

impl Drop for CaptureCleanupGuard {
    fn drop(&mut self) {
        // First, before anything else can fail: OBS must not be left
        // recording. Best-effort and silent by design — there is nobody left
        // to report to, and a cleanup path that panics is worse than one that
        // quietly does nothing.
        if let Some(obs) = self.obs.take()
            && let Ok(mut guard) = obs.lock() {
                if let Some(client) = guard.as_mut() {
                    client.stop_record_quietly();
                }
                *guard = None;
            }

        if let Err(e) = std::fs::remove_dir_all(&self.exit_trigger)
            && e.kind() != std::io::ErrorKind::NotFound {
                log::warn!("[GC::drop] Failed to remove exit_trigger {:?}: {}", self.exit_trigger, e);
            }
        if let Err(e) = std::fs::remove_dir(&self.session_junction)
            && e.kind() != std::io::ErrorKind::NotFound {
                log::warn!("[GC::drop] Failed to remove session_junction {:?}: {}", self.session_junction, e);
            }

        if let Some(game_root) = self.exit_trigger.parent() {
            crate::shared::paths::clear_capture_scratch(
                game_root,
                self.auto_clear_logs,
                self.auto_clear_temp_demos,
                self.auto_clear_previews,
                self.save_local_patched_copy,
            );
        }
    }
}

pub fn spawn_capture_engine(
    jobs: Vec<CaptureJob>,
    _hlae_path: Arc<PathBuf>,
    hl_path: Arc<PathBuf>,
    tx: Sender<EngineEvent>,
    cancel_token: Arc<AtomicBool>,
    config: crate::patch::PatcherConfig,
    // Final per-drive headroom `build_batch_queue` already computed for every
    // drive this batch touches (see `native/src/patch/builder.rs`) — the
    // pre-launch check below re-validates these same numbers instead of
    // re-querying disk space itself for just the primary export dir.
    drive_headroom: Vec<crate::patch::DriveHeadroom>,
    // Each planned block's take folder, in the order the batch will record
    // them — the same flattened order the capture manifest uses.
    //
    // Only read in `CaptureMode::Obs`, where the engine has to know where a
    // block's output belongs *before* the recording starts, so OBS can be
    // pointed straight at it. In every other mode HLAE decides that from
    // `mirv_movie_filename` and this is empty.
    obs_take_folders: Vec<PathBuf>,
) {
    std::thread::Builder::new()
        .name("capture_engine".into())
        .spawn(move || {
            macro_rules! log_crash_abort {
                ($tx:expr, $msg:expr) => {
                    {
                        log::error!("{}", $msg);
                        crate::log_markdown(&$msg);
                        let _ = $tx.send(EngineEvent::Error(
                            format!("Capture Engine Aborted — {} (see View Logs for details)", $msg)
                        ));
                    }
                };
            }

            let total = jobs.len();
            if tx.send(EngineEvent::Starting(total)).is_err() {
                return;
            }

            let hl_exe_parent = match hl_path.parent() {
                Some(parent) => parent,
                None => {
                    log_crash_abort!(tx, "Invalid hl.exe path: hl_path has no parent");
                    return;
                }
            };
            let dod_dir = hl_exe_parent.join("dod");



            let mut active_dest_paths = Vec::new();
            let dummy_path = hl_exe_parent.join("DOD_BATCH_DONE");
            std::fs::remove_dir_all(&dummy_path).ok();

            let exit_trigger = hl_exe_parent.join("DOD_TOOLS_EXIT_TRIGGER");
            let session_junction = hl_exe_parent.join("dodtools_session");
            
            let mut _cleanup_guard = CaptureCleanupGuard::new(
                exit_trigger.clone(),
                session_junction.clone(),
                config.auto_clear_logs,
                config.auto_clear_temp_demos,
                config.auto_clear_previews,
                config.save_local_patched_copy,
            );

            let active_export_dir = config.primary_media_dir.clone().unwrap_or_else(|| {
                let exe_path = std::env::current_exe().expect("Failed to resolve absolute exe path");
                exe_path.parent().expect("Exe has no parent directory").to_path_buf()
            });
            let session_dir = if !config.session_id.is_empty() {
                active_export_dir.join(&config.session_id)
            } else {
                active_export_dir.clone()
            };

            let session_junction_str = session_junction.to_str().unwrap_or_default();
            let session_dir_str = session_dir.to_str().unwrap_or_default();
            if session_junction_str.is_empty() || session_dir_str.is_empty() {
                log_crash_abort!(tx, "Invalid UTF-8 in session paths");
                return;
            }

            match std::process::Command::new("cmd").args(["/C", "mklink", "/J", session_junction_str, session_dir_str]).output() {
                Ok(out) if !out.status.success() => {
                    log_crash_abort!(tx, format!("mklink failed for session_junction: {}", String::from_utf8_lossy(&out.stderr)));
                    return;
                }
                Err(e) => {
                    log_crash_abort!(tx, format!("mklink command failed: {}", e));
                    return;
                }
                _ => {}
            }

            let mut pool_junctions: Vec<std::path::PathBuf> = Vec::new();
            // `build_batch_queue` creates a `_route_N` junction beside hl.exe for
            // every drive it routes blocks to. It has no way to clean them up —
            // it returns long before the capture ends — so they are listed here
            // by the same indices and unlinked with everything else. Indices this
            // batch never routed to simply are not there, which `remove_dir`
            // reports as NotFound and the guard ignores.
            let route_junctions: Vec<std::path::PathBuf> = (0..config.capture_directories.len())
                .map(|idx| hl_exe_parent.join(format!("_route_{}", idx)))
                .collect();
            for (idx, target_dir) in config.capture_directories.iter().enumerate() {
                let junction_path = hl_exe_parent.join(format!("dod_pool_{}", idx));
                let _ = std::fs::remove_dir(&junction_path);
                if let Err(e) = std::fs::create_dir_all(target_dir) {
                    log_crash_abort!(tx, format!("Failed to create capture directory {:?}: {}", target_dir, e));
                    return;
                }
                let junction_str = junction_path.to_str().unwrap_or_default();
                let target_str = target_dir.to_str().unwrap_or_default();
                if junction_str.is_empty() || target_str.is_empty() {
                    log_crash_abort!(tx, "Invalid UTF-8 in pool junction paths");
                    return;
                }
                let status = std::process::Command::new("cmd")
                    .args([
                        "/C", "mklink", "/J",
                        junction_str,
                        target_str,
                    ])
                    .output();
                match status {
                    Ok(out) if out.status.success() => {
                        log_markdown(&format!("[pool] Junction created: {:?} -> {:?}", junction_path, target_dir));
                        pool_junctions.push(junction_path);
                    }
                    Ok(out) => {
                        let err_msg = String::from_utf8_lossy(&out.stderr);
                        log_crash_abort!(tx, format!("[pool] mklink failed for dod_pool_{}: {}", idx, err_msg));
                        return;
                    }
                    Err(e) => {
                        log_crash_abort!(tx, format!("[pool] Failed to run mklink for dod_pool_{}: {}", idx, e));
                        return;
                    }
                }
            }

            let _guard = crate::patch::WorkspaceGuard {
                session_junction: session_junction.clone(),
                exit_trigger: exit_trigger.clone(),
                pool_junctions: pool_junctions.clone(),
                route_junctions: route_junctions.clone(),
                auto_clear_logs: config.auto_clear_logs,
                auto_clear_temp_demos: config.auto_clear_temp_demos,
                auto_clear_previews: config.auto_clear_previews,
                save_local_patched_copy: config.save_local_patched_copy,
            };

            for job in &jobs {
                if cancel_token.load(Ordering::Relaxed) {
                    let _ = tx.send(EngineEvent::Cancelled);
                    return;
                }

                let demo_filename = match job.patched_demo_path.file_name() {
                    Some(name) => name.to_string_lossy().replace("-", "_"),
                    None => {
                        log_crash_abort!(tx, format!("Invalid demo path: {:?}", job.patched_demo_path));
                        continue;
                    }
                };

                let dest_demo_path = dod_dir.join(&demo_filename);
                let source_path_str = job.patched_demo_path.to_string_lossy().to_lowercase();
                let dest_path_str = dest_demo_path.to_string_lossy().to_lowercase();

                if source_path_str != dest_path_str {
                    #[cfg(target_os = "windows")]
                    {
                        use std::os::windows::fs::OpenOptionsExt;
                        let mut src_file = match std::fs::OpenOptions::new()
                            .read(true)
                            .share_mode(1)
                            .open(&job.patched_demo_path) {
                                Ok(f) => f,
                                Err(e) => {
                                    log_crash_abort!(tx, format!("Failed to open source demo for copy: {}", e));
                                    continue;
                                }
                            };

                        let mut dest_file_opt = None;
                        for i in 0..5 {
                            match std::fs::File::create(&dest_demo_path) {
                                Ok(f) => {
                                    dest_file_opt = Some(f);
                                    break;
                                }
                                Err(e) => {
                                    if e.raw_os_error() == Some(32) && i < 4 {
                                        std::thread::sleep(std::time::Duration::from_millis(150));
                                        continue;
                                    }
                                    break;
                                }
                            }
                        }

                        let mut dest_file = match dest_file_opt {
                            Some(f) => f,
                            None => {
                                log_crash_abort!(tx, format!("Failed to create dest demo file after retries. Source: {:?}, Dest: {:?}", job.patched_demo_path, dest_demo_path));
                                continue;
                            }
                        };

                        log_markdown(&format!("- [IO] Copying (Windows) from {}", source_path_str));
                        log_markdown(&format!("- [IO] Copying (Windows) to {}", dest_path_str));
                        match std::io::copy(&mut src_file, &mut dest_file) {
                            Ok(bytes) => log_markdown(&format!("- [IO] Copy SUCCESS! Bytes written: {}", bytes)),
                            Err(e) => {
                                log_markdown(&format!("- [IO] Copy FAILED! Error: {}", e));
                                log_crash_abort!(tx, format!("Failed to copy demo to game folder: {}", e));
                                continue;
                            }
                        }
                    }

                    #[cfg(not(target_os = "windows"))]
                    {
                        log_markdown(&format!("- [IO] Copying (*nix) from {}", source_path_str));
                        log_markdown(&format!("- [IO] Copying (*nix) to {}", dest_path_str));
                        match std::fs::copy(&job.patched_demo_path, &dest_demo_path) {
                            Ok(bytes) => log_markdown(&format!("- [IO] Copy SUCCESS! Bytes written: {}", bytes)),
                            Err(e) => {
                                log_markdown(&format!("- [IO] Copy FAILED! Error: {}", e));
                                log_crash_abort!(tx, format!("Failed to copy demo to game folder: {}", e));
                                continue;
                            }
                        }
                    }
                }

                if config.save_local_patched_copy {
                    let exe_path = std::env::current_exe().expect("Failed to resolve absolute exe path");
                    let base_dir = exe_path.parent().expect("Exe has no parent directory").to_path_buf();
                    let demos_dir = base_dir.join("demos");
                    let _ = std::fs::create_dir_all(&demos_dir);
                    let local_dest = demos_dir.join(&demo_filename);
                    match std::fs::copy(&job.patched_demo_path, &local_dest) {
                        Ok(_) => log_markdown(&format!("- [IO] Saved local copy to demos/{}", demo_filename)),
                        Err(e) => log::warn!("Failed to save local patched copy to {:?}: {}", local_dest, e),
                    }
                }

                active_dest_paths.push(dest_demo_path);
            }

            if active_dest_paths.is_empty() {
                let _ = tx.send(EngineEvent::AllCompleted);
                return;
            }

            if tx.send(EngineEvent::Launching("Batch Queue".into())).is_err() {
                log_crash_abort!(tx, "Failed to send Launching event (channel disconnected)");
                for path in &active_dest_paths {
                    let _ = std::fs::remove_file(path);
                }
                return;
            }

            for entry in &drive_headroom {
                if entry.free_bytes < crate::sys::disk::MIN_DRIVE_HEADROOM_BYTES {
                    let required_gb = crate::sys::disk::MIN_DRIVE_HEADROOM_BYTES as f64 / (1024.0 * 1024.0 * 1024.0);
                    log_crash_abort!(tx, format!(
                        "Capture aborted: {:?} has less than {:.1} GB free space.",
                        entry.path, required_gb
                    ));
                    return;
                }
            }

            // hl.exe's own install drive is never in `drive_headroom` (that
            // list is only for drives an actual recording block routed to —
            // patched demo files land directly in `hl_exe_parent`'s own
            // `dod/` folder now, not on any capture_directories entry, since
            // that's the only place GoldSrc's `playdemo` can find them; see
            // issue #8). Every patched file already exists on disk by this
            // point — real sizes, not an estimate. Doubled when Save Local
            // Patched Copy duplicates each file into `<exe_dir>/demos` too.
            // See issue #11.
            let total_patched_bytes: u64 = jobs.iter()
                .map(|j| std::fs::metadata(&j.patched_demo_path).map(|m| m.len()).unwrap_or(0))
                .sum();
            let hl_exe_required_bytes = if config.save_local_patched_copy {
                total_patched_bytes.saturating_mul(2)
            } else {
                total_patched_bytes
            };
            let hl_exe_drive_free = crate::sys::disk::get_available_bytes(hl_exe_parent);
            if hl_exe_drive_free < hl_exe_required_bytes {
                let required_gb = hl_exe_required_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
                log_crash_abort!(tx, format!(
                    "Capture aborted: {:?} (hl.exe's own drive) has less than {:.1} GB free space.",
                    hl_exe_parent, required_gb
                ));
                return;
            }

            // ── OBS capture mode ──────────────────────────────────────────────
            // Everything here happens *before* the game is spawned, on purpose.
            // Every failure below otherwise produces a batch that runs to
            // completion and captures nothing — the worst outcome this path has,
            // because it looks exactly like success until someone opens the
            // folder.
            let obs_mode = config.capture_mode == crate::patch::CaptureMode::Obs;
            let mut obs_session: Option<crate::obs::ObsSession> = None;
            let (marker_tx, marker_rx) = std::sync::mpsc::channel::<crate::obs::Marker>();
            let tail_cancel = Arc::new(AtomicBool::new(false));

            if obs_mode {
                if obs_take_folders.is_empty() {
                    log_crash_abort!(tx, "OBS capture was requested but no blocks were planned");
                    return;
                }
                match crate::obs::ObsSession::start(
                    &config.obs,
                    obs_take_folders.clone(),
                    config.resolution_width,
                    config.resolution_height,
                    config.obs_capture_fps,
                ) {
                    Ok((session, preflight)) => {
                        log_markdown(&format!(
                            "🎥 **OBS capture** — connected to OBS {} (obs-websocket {}) at {}. \
                             Canvas {}x{} @ {:.0}fps, scene `{}`. HLAE will record nothing; OBS is \
                             driven from the engine's own console markers.",
                            preflight.obs_version,
                            preflight.websocket_version,
                            config.obs.redacted(),
                            preflight.canvas_width,
                            preflight.canvas_height,
                            preflight.fps,
                            preflight.current_scene,
                        ));
                        // Hand the connection to the guard first, so that from
                        // this point on every exit path stops the recorder.
                        _cleanup_guard.obs = Some(session.stop_handle());
                        obs_session = Some(session);
                    }
                    Err(e) => {
                        log_crash_abort!(tx, format!("OBS capture could not start — {}", e));
                        return;
                    }
                }
            }

            // The console-log marker stream isn't OBS-specific: OBS mode needs
            // it to drive recording, but `DEMO_START` (per-demo progress, see
            // `EngineEvent::DemoLoading`) matters in every mode. `-condebug` is
            // now passed on every launch (`build_hlae_process`), so the log is
            // always being written and this always runs.
            //
            // The log is deleted at the end of a batch, not the start, so a file
            // left by a previous run is normal and its markers are history.
            // `LogTailer::at_end` is what stops a stale `START_RECORD` firing a
            // recording the moment this begins.
            let log_path = hl_exe_parent.join("qconsole.log");
            // Baseline for the write check in the poll loop. `-condebug` being on
            // the command line is not proof the log is being written, and
            // everything marker-driven degrades in silence when it is not — the
            // one failure mode this path cannot afford.
            //
            // The engine *appends* across launches rather than starting a fresh
            // log — measured, not assumed: one 1.5 MB `qconsole.log` here held
            // 100 startup banners. `auto_clear_logs` deleting it at the end of a
            // batch is the only thing that ever shortens it.
            //
            // So the test is "did the size change at all" rather than "did it
            // grow". Under appending the two agree, but `<=` would also report a
            // file that got deleted mid-run as a failure to write, which it is
            // not.
            let log_len_before_launch = console_log_len(&log_path);
            let tailer = crate::obs::LogTailer::at_end(&log_path);
            let cancel = Arc::clone(&tail_cancel);
            if let Err(e) = std::thread::Builder::new()
                .name("obs_log_tail".into())
                .spawn(move || tailer.run(marker_tx, cancel))
            {
                log_crash_abort!(tx, format!("could not start the console log reader: {}", e));
                return;
            }

            // MUST be embedded here, not appended to `cmd` below: HLAE's own
            // `-customLoader` argument parsing (`ProcessArgsCustomLoader` in
            // advancedfx/advancedfx's `hlae/Program.cs`) reads exactly six
            // flags and silently drops everything else on its own argv —
            // only text folded into this string reaches `hl.exe`, via the
            // single `-cmdLine` value `build_hlae_process` composes below.
            //
            // The HLAE alpha flags that used to be composed here now live in
            // `build_hlae_process` itself, so every `-customLoader` launch this
            // app makes carries them, not just a capture batch.
            let extra_args = "+exec dodtools_helper.cfg +playdemo primer".to_string();

            let dummy_path = active_export_dir.join("DOD_BATCH_DONE");
            let _ = std::fs::remove_dir_all(&dummy_path);

            let mut cmd = config.build_hlae_process(&extra_args);

            let cfg_path = dod_dir.join("dod_quit.cfg");
            std::fs::write(&cfg_path, "quit\n").ok();

            log_markdown(&format!("[HLAE] Spawning: {:?} {:?}", cmd.get_program(), cmd.get_args().collect::<Vec<_>>()));

            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    log_crash_abort!(tx, format!("Failed to spawn HLAE (OS Error): {}", e));
                    for path in &active_dest_paths {
                        let _ = std::fs::remove_file(path);
                    }
                    std::fs::remove_file(&cfg_path).ok();
                    return;
                }
            };
            log_markdown(&format!("[HLAE] Spawned (PID {})", child.id()));

            // The HLAE launcher process (`_child` above) injects into `hl.exe` and then
            // exits on its own under `-noGui -autoStart` — that is expected handoff
            // behaviour, not a crash. The thing that actually matters is whether
            // `hl.exe` itself is still alive, so liveness is tracked separately via
            // process name rather than via the launcher's own exit status.
            let start_time = std::time::Instant::now();
            let mut launcher_exit_logged = false;
            let mut hl_seen_alive = false;
            let mut failure_reason: Option<&'static str> = None;
            // Stall detection state. Both stay untouched outside OBS mode,
            // where no markers are drained and `last_marker_at` never leaves
            // `None` — the watchdog below is inert as a result.
            let mut last_marker_at: Option<std::time::Instant> = None;
            let mut longest_marker_gap = std::time::Duration::ZERO;
            // Armed from hl.exe coming up, not from the first marker. A batch
            // where markers never start at all — a demo that fails to load, a
            // game sitting at the menu — would otherwise never be watched,
            // because the deadline had nothing to count from. Seen in a real
            // run: 398s with hl.exe alive and not one marker.
            let mut hl_first_seen: Option<std::time::Instant> = None;
            // Set the instant the BATCH_COMPLETE marker is seen, in OBS mode
            // only — a faster, side-effect-free alternative to waiting on
            // `exit_trigger` there. `sys_capture_done_path`'s
            // `mirv_recordmovie_start`/`_stop` (which is what actually writes
            // `exit_trigger`) restarts the whole demo from the top as a side
            // effect — harmless in frame-sequence mode (which is already
            // mid-movie-recording throughout), but in OBS mode it replayed
            // primer.dem *and* chain_01.dem from tick 0, twice, adding ~50s to
            // a real batch before `exit_trigger` ever got written. The
            // BATCH_COMPLETE marker already exists and already fires
            // `ObsSession::on_marker`'s `end_block()` moments before this —
            // taskkilling right after it means hl.exe is dead before it ever
            // gets to process the command that would restart it. See #obs.
            let mut obs_batch_complete_seen = false;
            // One-shot, so a batch cannot spam the log with it.
            let mut condebug_write_checked = false;
            let mut sys = {
                use sysinfo::SystemExt;
                sysinfo::System::new_all()
            };
            loop {
                // Drain whatever the engine has echoed since the last pass.
                // This is the whole synchronisation mechanism: markers reach
                // the log 21-40 ms after the tick that emitted them, measured
                // over a 17-block batch, so a 500 ms poll below would be far
                // too coarse to act on them — hence the shorter sleep chosen
                // for OBS mode at the end of this loop.
                for marker in marker_rx.try_iter() {
                    let now = std::time::Instant::now();
                    if let Some(previous) = last_marker_at {
                        longest_marker_gap = longest_marker_gap.max(now - previous);
                    }
                    last_marker_at = Some(now);
                    if let Some(session) = obs_session.as_mut() {
                        session.on_marker(&marker);
                    }
                    if obs_mode && marker.kind == crate::obs::MarkerKind::BatchComplete {
                        obs_batch_complete_seen = true;
                    }
                    if marker.kind == crate::obs::MarkerKind::DemoStart
                        && let Some((job_idx, total, clips)) = marker.demo_progress {
                            let _ = tx.send(EngineEvent::DemoLoading(job_idx, total, clips));
                        }
                    if marker.kind == crate::obs::MarkerKind::NextClip
                        && let Some((job_idx, total, clip_idx, clip_count)) = marker.next_clip_progress {
                            let _ = tx.send(EngineEvent::FastForwardToClip(job_idx, total, clip_idx, clip_count));
                        }
                }

                if !launcher_exit_logged {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            launcher_exit_logged = true;
                            log_markdown(&format!(
                                "[HLAE] Launcher exited after {:.1}s (status: {:?}) — expected handoff behaviour, not a crash by itself",
                                start_time.elapsed().as_secs_f32(),
                                status
                            ));
                        }
                        Ok(None) => {}
                        Err(e) => log_markdown(&format!("[HLAE] try_wait() failed: {}", e)),
                    }
                }

                let hl_alive = {
                    use sysinfo::{SystemExt, ProcessExt};
                    sys.refresh_processes();
                    let alive = sys.processes().values().any(|p| p.name().eq_ignore_ascii_case("hl.exe"));
                    if alive {
                        hl_seen_alive = true;
                        hl_first_seen.get_or_insert_with(std::time::Instant::now);
                    }
                    alive
                };

                // Is the engine actually writing the console log? Checked once,
                // after the game has had long enough to produce startup output.
                //
                // Nothing else notices this. The tailer runs happily against a
                // file that never grows, no marker ever arrives, and every
                // feature that depends on one — per-demo progress, fast-forward
                // notifications, crash detection, and all of OBS capture —
                // simply never fires. The batch still finishes, so it looks like
                // success. Say so loudly instead.
                if !condebug_write_checked
                    && let Some(seen) = hl_first_seen
                        && seen.elapsed() >= CONDEBUG_WRITE_GRACE {
                            condebug_write_checked = true;
                            if console_log_len(&log_path) == log_len_before_launch {
                                log_markdown(&format!(
                                    "⚠️ **The engine is not writing its console log.** `{}` is unchanged \
                                     {:.0}s after the game started, so dod-tools is getting no console \
                                     markers from it.\n\n\
                                     Per-demo progress, fast-forward-to-clip notifications, crash \
                                     detection and OBS capture all read that log — without it they do \
                                     not fail, they simply never fire, and this batch may finish looking \
                                     like it worked.\n\n\
                                     `-condebug` is passed on every launch and this is the folder the \
                                     engine writes to, so the cause is on the writing side: the folder \
                                     may not be writable (antivirus and Controlled Folder Access can \
                                     block it as well as permissions), something else may be holding \
                                     the file open, or HLAE may not be passing the flag through to the \
                                     game.",
                                    log_path.display(),
                                    CONDEBUG_WRITE_GRACE.as_secs_f32(),
                                ));
                            }
                        }

                if cancel_token.load(Ordering::Relaxed) {
                    log_markdown(&format!("[HLAE] Cancelled by user after {:.1}s", start_time.elapsed().as_secs_f32()));
                    std::process::Command::new("taskkill").args(["/F", "/IM", "hl.exe"]).output().ok();
                    wait_for_hl_exe_to_exit(&mut sys);
                    break;
                }
                // Counts from the last marker, or from hl.exe first appearing
                // when none has arrived yet — the second case is a demo that
                // never started playing, which produces no markers to count
                // from at all. Both mean the same thing: the game is alive and
                // the batch is not advancing.
                if obs_mode
                    && let Some(since) = last_marker_at.or(hl_first_seen)
                        && hl_alive && since.elapsed() > marker_stall_deadline(longest_marker_gap) {
                            let never_started = last_marker_at.is_none();
                            log_markdown(&format!(
                                "[HLAE] No console markers for {:.0}s with hl.exe still running — treating the batch as stalled. {}",
                                since.elapsed().as_secs_f32(),
                                if never_started {
                                    "Playback never produced a single marker, so the demo most likely failed to load."
                                } else {
                                    "`disconnect` typed in the console, a demo that ended early, and a frozen engine all look like this from out here."
                                }
                            ));
                            failure_reason = Some(if never_started {
                                "the demo never started playing — hl.exe came up but produced no console markers at all"
                            } else {
                                "the batch stopped progressing while hl.exe was still running — no console markers arrived for several minutes"
                            });
                            std::process::Command::new("taskkill").args(["/F", "/IM", "hl.exe"]).output().ok();
                            wait_for_hl_exe_to_exit(&mut sys);
                            break;
                        }
                // OBS gone for good, after a reconnect was already tried.
                // Continuing would play the demo to the end capturing nothing
                // and then report the batch as finished.
                if obs_session.as_ref().is_some_and(|s| s.is_dead()) {
                    log_markdown("[HLAE] OBS is unreachable and could not be reconnected — aborting rather than finishing the batch with nothing recorded.");
                    failure_reason = Some("lost contact with OBS mid-batch and could not reconnect");
                    std::process::Command::new("taskkill").args(["/F", "/IM", "hl.exe"]).output().ok();
                    wait_for_hl_exe_to_exit(&mut sys);
                    break;
                }
                if start_time.elapsed().as_secs() > 10
                    && (dummy_path.exists() || exit_trigger.exists() || obs_batch_complete_seen)
                {
                    let via = if obs_batch_complete_seen && !exit_trigger.exists() {
                        // The common OBS-mode case: caught it off the marker,
                        // ahead of exit_trigger ever needing to be written.
                        "BATCH_COMPLETE marker".to_string()
                    } else {
                        format!(
                            "done marker: {}, exit trigger: {}",
                            dummy_path.exists(),
                            exit_trigger.exists()
                        )
                    };
                    log_markdown(&format!(
                        "[HLAE] Batch complete after {:.1}s (via {}) — taskkilling hl.exe",
                        start_time.elapsed().as_secs_f32(),
                        via
                    ));
                    std::process::Command::new("taskkill").args(["/F", "/IM", "hl.exe"]).output().ok();
                    wait_for_hl_exe_to_exit(&mut sys);
                    break;
                }
                // Only treat this as a real failure once the launcher has handed off
                // (or failed to) AND hl.exe itself is confirmed not running AND no
                // exit trigger has appeared — i.e. nothing is left that could ever
                // finish the batch. The 5s grace period covers the brief window
                // between the launcher exiting and hl.exe's own process becoming
                // visible to sysinfo.
                if launcher_exit_logged
                    && !hl_seen_alive
                    && start_time.elapsed().as_secs() > 5
                    && !dummy_path.exists()
                    && !exit_trigger.exists()
                {
                    log_markdown(&format!(
                        "[HLAE] hl.exe never came up after the launcher exited ({:.1}s elapsed) — treating as failure",
                        start_time.elapsed().as_secs_f32()
                    ));
                    failure_reason = Some("hl.exe never started after the HLAE launcher exited");
                    break;
                }
                // hl.exe was running and has now disappeared without ever writing
                // an exit trigger/done marker, unlike the normal quit-cfg-driven
                // exit which writes the trigger before the process goes away.
                //
                // Not necessarily a crash, and it used to say it was: `quit` in
                // the console, ALT+F4 and End Process all land here too, and all
                // of them are the user closing the game deliberately. Nothing
                // visible from out here separates them from an access violation
                // — the exit status belongs to the launcher, which handed off
                // long ago — so the wording covers both rather than guessing.
                if hl_seen_alive && !hl_alive && !dummy_path.exists() && !exit_trigger.exists() {
                    log_markdown(&format!(
                        "[HLAE] hl.exe is gone with no exit trigger after {:.1}s — either the game was closed (quit / ALT+F4 / End Process) or it crashed",
                        start_time.elapsed().as_secs_f32()
                    ));
                    failure_reason = Some("hl.exe ended before the batch finished — the game was either closed manually or crashed (no exit trigger was written)");
                    break;
                }
                // 500 ms is fine for watching a process; it is far too coarse
                // for acting on stage markers, where the engine's own flush
                // granularity is ~21 ms and the pre-roll budget being spent is
                // one second. The process checks above tolerate the faster
                // cadence — `refresh_processes` is the only real cost, and it
                // is cheap next to a capture.
                let poll = if obs_mode { 16 } else { 500 };
                std::thread::sleep(std::time::Duration::from_millis(poll));
            }

            // Stop anything still recording and put the scene back before the
            // teardown below starts removing junctions.
            if let Some(mut session) = obs_session.take() {
                tail_cancel.store(true, Ordering::Relaxed);
                for marker in marker_rx.try_iter() {
                    session.on_marker(&marker);
                }
                session.finish();
                log_markdown(&format!(
                    "🎥 **OBS capture** — {} block(s) recorded, {} skipped.",
                    session.recorded.len(),
                    session.skipped.len()
                ));
                for problem in &session.skipped {
                    log_markdown(&format!("⚠️ **OBS** — {problem}"));
                }
                for block in &session.recorded {
                    // The duration is wall-clock, which tracks the file on a
                    // clean stop and overstates it on a salvaged one — the
                    // recording was cut off, so the seconds after that are not
                    // in the file. Said out loud rather than silently printed
                    // as if it were a length.
                    log_markdown(&format!(
                        "- [obs] {} ({:.1}s{}) -> {}",
                        block.take_folder.display(),
                        block.seconds,
                        if block.salvaged { " of recording, salvaged — the file is shorter" } else { "" },
                        block.video.display()
                    ));
                }
            }
            tail_cancel.store(true, Ordering::Relaxed);

            if let Some(reason) = failure_reason.filter(|_| !cancel_token.load(Ordering::Relaxed)) {
                log_crash_abort!(tx, format!("{} — see [HLAE] lines above in this log for timing.", reason));
                for path in &active_dest_paths {
                    let _ = std::fs::remove_file(path);
                }
                std::fs::remove_file(&cfg_path).ok();
                std::fs::remove_dir_all(&dummy_path).ok();
                std::fs::remove_dir_all(&exit_trigger).ok();
                let _ = std::fs::remove_dir(&session_junction);
                for junction in &pool_junctions {
                    let _ = std::fs::remove_dir(junction);
                }
                return;
            }

            if cancel_token.load(Ordering::Relaxed) {
                for path in &active_dest_paths {
                    let _ = std::fs::remove_file(path);
                }
                std::fs::remove_file(&cfg_path).ok();
                std::fs::remove_dir_all(&dummy_path).ok();
                std::fs::remove_dir_all(&exit_trigger).ok();
                let _ = std::fs::remove_dir(&session_junction);
                for junction in &pool_junctions {
                    let _ = std::fs::remove_dir(junction);
                }
                let _ = tx.send(EngineEvent::Cancelled);
                return;
            }

            std::fs::remove_file(&cfg_path).ok();
            std::fs::remove_dir_all(&dummy_path).ok();
            std::fs::remove_dir_all(&exit_trigger).ok();
            let _ = std::fs::remove_dir(&session_junction);
            for junction in &pool_junctions {
                let _ = std::fs::remove_dir(junction);
            }

            let _ = tx.send(EngineEvent::Finished("Batch Queue".into()));

            let autosave_path = crate::shared::paths::get_appdata_dir().join(".autosave.json");
            if let Err(e) = std::fs::remove_file(&autosave_path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("[autosave] Failed to remove .autosave.json: {}", e);
                }
            } else {
                log::info!("[autosave] Lockfile removed after clean completion");
            }

            let _ = tx.send(EngineEvent::AllCompleted);
        })
        .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A batch that has shown nothing unusual gets the floor. Five minutes is
    /// the number that has to clear an unfocused game's stalled fast-forward.
    #[test]
    fn quiet_batch_waits_the_floor() {
        assert_eq!(marker_stall_deadline(Duration::ZERO), MARKER_STALL_FLOOR);
        assert_eq!(
            marker_stall_deadline(Duration::from_secs(30)),
            MARKER_STALL_FLOOR
        );
    }

    /// A demo whose breadcrumbs are genuinely further apart than the floor
    /// must not be killed for being slow — it has already demonstrated the gap
    /// is legitimate.
    #[test]
    fn adapts_to_a_batch_with_long_legitimate_gaps() {
        let observed = Duration::from_secs(240);
        assert_eq!(
            marker_stall_deadline(observed),
            Duration::from_secs(720),
            "three times the longest gap already survived"
        );
    }

    /// The adaptive term only ever widens the window. Narrowing it would make
    /// a fast batch trip on its first slow stretch.
    #[test]
    fn never_shortens_below_the_floor() {
        for secs in [0, 1, 10, 99, 100, 101] {
            assert!(marker_stall_deadline(Duration::from_secs(secs)) >= MARKER_STALL_FLOOR);
        }
    }

    /// A regression guard for #198's fix, not the retry behaviour itself --
    /// `remove_file_retrying` (`shared/paths.rs`) already covers that in
    /// isolation. This just confirms the guard still calls it correctly and
    /// leaves everything else in `dod/` untouched.
    #[test]
    fn dropping_the_guard_removes_primer_and_chain_demos_when_enabled() {
        let root = std::env::temp_dir().join(format!("dod_cleanup_guard_on_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dod = root.join("dod");
        std::fs::create_dir_all(&dod).unwrap();
        std::fs::write(dod.join("primer.dem"), b"x").unwrap();
        std::fs::write(dod.join("chain_01.dem"), b"x").unwrap();
        std::fs::write(dod.join("chain_02.dem"), b"x").unwrap();
        std::fs::write(dod.join("not_a_chain_demo.dem"), b"x").unwrap();

        {
            let _guard = CaptureCleanupGuard::new(
                root.join("exit_trigger"),
                root.join("session_junction"),
                false,
                true,
                false,
                false,
            );
        }

        assert!(!dod.join("primer.dem").exists());
        assert!(!dod.join("chain_01.dem").exists());
        assert!(!dod.join("chain_02.dem").exists());
        assert!(dod.join("not_a_chain_demo.dem").exists(), "must not touch an unrelated file");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// `save_local_patched_copy` means the user asked to keep these files --
    /// the guard must not clean them up even with `auto_clear_temp_demos` on.
    #[test]
    fn dropping_the_guard_keeps_demos_when_a_local_copy_was_requested() {
        let root = std::env::temp_dir().join(format!("dod_cleanup_guard_keep_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dod = root.join("dod");
        std::fs::create_dir_all(&dod).unwrap();
        std::fs::write(dod.join("primer.dem"), b"x").unwrap();
        std::fs::write(dod.join("chain_01.dem"), b"x").unwrap();

        {
            let _guard = CaptureCleanupGuard::new(
                root.join("exit_trigger"),
                root.join("session_junction"),
                false,
                true,
                false,
                true,
            );
        }

        assert!(dod.join("primer.dem").exists());
        assert!(dod.join("chain_01.dem").exists());

        let _ = std::fs::remove_dir_all(&root);
    }
}
