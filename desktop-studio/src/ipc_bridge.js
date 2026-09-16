import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getVersion } from '@tauri-apps/api/app';
import { revealItemInDir } from '@tauri-apps/plugin-opener';
import { showToast } from './toast.js';
import { STRINGS } from './strings.js';

/** Writes one line to today's activity log — for frontend-only events (the
 *  Master Queue's Clear Untracked/Selected/All and row-delete actions) that
 *  have no other IPC call to log from. Best-effort: logs to the console on
 *  failure rather than toasting, since a lost log line isn't worth
 *  interrupting the user over. */
export function logFrontendEvent(message) {
  invoke('log_frontend_event', { message }).catch((err) => {
    console.error("IPC Execution Error (log_frontend_event):", err);
  });
}

/** Opens Explorer with today's activity log pre-selected — AppData/logs
 *  isn't somewhere most users would think to go looking on their own. */
export async function openActivityLog() {
  return invoke('get_activity_log_path')
    .then((path) => revealItemInDir(path))
    .catch((err) => {
      console.error("IPC Execution Error (get_activity_log_path):", err);
      showToast(STRINGS.IPC.logFileOpenFailed(err), 'error');
    });
}

export async function scanDirectory(scanPaths) {
  return invoke("scan_directory", { paths: scanPaths })
    .catch((err) => {
      console.error("IPC Execution Error (scan_directory):", err);
      showToast(STRINGS.IPC.scanError(err), 'error');
      throw err;
    });
}

// Map library checks. Deliberately quiet on failure: a demo whose map cannot be
// checked is still a demo the user can work with, so this reports and returns
// nothing rather than interrupting a scan that otherwise succeeded.
export async function checkDemoMaps(demoPaths, gamePath) {
  return invoke("check_demo_maps", { demoPaths, gamePath })
    .catch((err) => {
      console.error("IPC Execution Error (check_demo_maps):", err);
      return [];
    });
}

export async function downloadMap(mapName, expectedChecksum, gamePath) {
  return invoke("download_map", { mapName, expectedChecksum, gamePath })
    .catch((err) => {
      console.error("IPC Execution Error (download_map):", err);
      throw err;
    });
}


// What the pre-roll and post-roll have to cover. Quiet on failure: a timing
// hint that cannot be computed is not worth interrupting anyone over.
export async function getRollFloors(preRoll, postRoll, decalFlush, customCommands = []) {
  const recordStartLead = parseFloat(document.querySelector('#config-record-start-lead')?.value) || 0;
  const recordStopTrail = parseFloat(document.querySelector('#config-record-stop-trail')?.value) || 0;
  return invoke("roll_floors", { preRoll, postRoll, recordStartLead, recordStopTrail, decalFlush, customCommands })
    .catch((err) => {
      console.error("IPC Execution Error (roll_floors):", err);
      return null;
    });
}

export async function scanGameConfigs(
  gamePath,
  initCommands = [],
  customCommands = [],
  context = {}
) {
  return invoke("scan_game_configs", {
    gamePath,
    initCommands,
    customCommands,
    captureFps: context.captureFps ?? null,
    decalFlush: context.decalFlush ?? null,
  })
    .catch((err) => {
      console.error("IPC Execution Error (scan_game_configs):", err);
      return { unseen: [], overrides: [], shadowed: [], custom: [], bannedInit: [], bannedScheduled: [], decalDefaultRing: null, decalFlushIsNoop: false, noopInit: [], noopScheduled: [] };
    });
}

export async function validatePaths(hlaePath, hlPath) {
  return await invoke('validate_paths', { hlaePath, hlPath })
    .catch((err) => {
      console.error("IPC Execution Error (validate_paths):", err);
      showToast(STRINGS.IPC.validationError(err), 'error');
      throw err;
    });
}

/**
 * Whether HLAE can reach an FFmpeg of its own.
 *
 * A different question from Render Studio's FFmpeg: `mirv_movie_ffmpeg` makes
 * HLAE spawn one itself and it does not consult the app's resolution chain, so
 * a missing one is only discovered as a capture that finishes and produces no
 * video. See `docs/direct_to_video_capture.md`.
 */
/**
 * Whether each configured executable path points at a file. Returns a state per
 * path ("ok" | "empty" | "not_found" | "not_a_file"), not a message — the
 * wording lives in strings.js with everything else the user reads.
 */
export async function diagnoseExecutablePaths(paths) {
  return await invoke('diagnose_executable_paths', { paths })
    .catch((err) => {
      console.error("IPC Execution Error (diagnose_executable_paths):", err);
      throw err;
    });
}

export async function checkHlaeFfmpeg(hlaePath, ffmpegPath) {
  return await invoke('check_hlae_ffmpeg', { hlaePath, ffmpegPath })
    .catch((err) => {
      console.error("IPC Execution Error (check_hlae_ffmpeg):", err);
      throw err;
    });
}

/**
 * Writes HLAE's `ffmpeg.ini` so it can find the FFmpeg the app already uses.
 * Never overwrites an existing one — the backend refuses and says why.
 */
export async function linkHlaeFfmpeg(hlaePath, ffmpegPath, elevated = false) {
  return await invoke('link_hlae_ffmpeg', { hlaePath, ffmpegPath, elevated })
    .catch((err) => {
      console.error("IPC Execution Error (link_hlae_ffmpeg):", err);
      showToast(STRINGS.CAPTURE_CONFIG.HLAE_FFMPEG_LINK_FAILED(err), 'error');
      throw err;
    });
}

export async function analyzeDemoFull(demoPath) {
  return await invoke('analyze_demo_full', { demoPath })
    .catch((err) => {
      console.error("IPC Execution Error (analyze_demo_full):", err);
      showToast(STRINGS.IPC.analysisError(err), 'error');
      throw err;
    });
}

/** Every Weapon variant's resolved display name, keyed by its raw JSON tag —
 *  same names native/src/patch/scanner.rs bakes into a kill streak's
 *  timeline text. Best-effort, same as isDebugBuild(): a fetch failure just
 *  means the analyzer pane's own name-derivation fallback stays in use. */
export async function getWeaponDisplayNames() {
  return invoke("get_weapon_display_names").catch((err) => {
    console.error("IPC Execution Error (get_weapon_display_names):", err);
    return {};
  });
}

export async function startCaptureBatch(payload) {
  return invoke("start_capture_batch", { payload: payload })
    .catch((err) => {
      console.error("IPC Execution Error (start_capture_batch):", err);
      throw err;
    });
}

/** Patches the given demo's highlights into a single `<stem>_preview.dem`
 *  (BOOKMARK/director events at each highlight, regardless of selection or
 *  Min Kills — reuses an existing preview instead of regenerating one) and
 *  immediately launches it in HLAE via `+viewdemo`. */
export async function launchDemoPreview(hlaePath, gamePath, streaks) {
  return invoke("launch_demo_preview", { hlaePath, gamePath, streaks })
    .catch((err) => {
      console.error("IPC Execution Error (launch_demo_preview):", err);
      showToast(STRINGS.IPC.previewFailed(err), 'error');
      throw err;
    });
}

/** True if an `hl.exe`/`hlae.exe` instance is already running — used as a
 *  pre-flight guard before `launchDemoPreview` so a stale HLAE session
 *  doesn't corrupt the freshly-patched preview demo. */
export async function checkEngineProcesses() {
  return invoke("check_engine_processes")
    .catch((err) => {
      console.error("IPC Execution Error (check_engine_processes):", err);
      showToast(STRINGS.IPC.processCheckFailed(err), 'error');
      throw err;
    });
}

/** Launches HLAE against `hl.exe` directly with no demo loaded, applying the
 *  persisted resolution/HUD/init-command settings. */
export async function launchStandaloneGame() {
  return invoke("launch_standalone_game")
    .catch((err) => {
      console.error("IPC Execution Error (launch_standalone_game):", err);
      showToast(STRINGS.IPC.launchFailed(err), 'error');
      throw err;
    });
}

/** Reads a `.cfg` file's console commands (blank lines and full-line comments
 *  stripped) for the Commands tab's Import Config button — lets a manually
 *  exec'd movie config's lines land in Initial Commands directly, since
 *  STUFFTEXT there reliably wins over anything a config.cfg/movie.cfg sets. */
export async function readCfgCommands(path) {
  return invoke("read_cfg_commands", { path })
    .catch((err) => {
      console.error("IPC Execution Error (read_cfg_commands):", err);
      showToast(STRINGS.IPC.cfgImportFailed(err), 'error');
      throw err;
    });
}

/** Launches OBS Studio from the configured path, spawn-and-forget — no
 *  lifecycle tracking, OBS is the user's own software. */
export async function launchObs() {
  return invoke("launch_obs")
    .catch((err) => {
      console.error("IPC Execution Error (launch_obs):", err);
      throw err;
    });
}

/** Aggressively kills any running `hl.exe`/`hlae.exe` instances. */
export async function killEngineProcesses() {
  return invoke("kill_engine_processes")
    .catch((err) => {
      console.error("IPC Execution Error (kill_engine_processes):", err);
      showToast(STRINGS.IPC.killEngineFailed(err), 'error');
      throw err;
    });
}

/** Patches every demo's highlights into its own `<stem>_preview.dem` (grouped
 *  server-side by source demo) without launching HLAE, skipping any demo that
 *  already has one. Resolves to the number of preview demos freshly generated
 *  this call. */
export async function generateAllPreviews(hlaePath, gamePath, streaks) {
  return invoke("generate_all_previews", { hlaePath, gamePath, streaks })
    .catch((err) => {
      console.error("IPC Execution Error (generate_all_previews):", err);
      showToast(STRINGS.IPC.batchPreviewFailed(err), 'error');
      throw err;
    });
}

export async function cancelCaptureBatch() {
  return invoke("cancel_capture_batch")
    .catch((err) => {
      console.error("IPC Execution Error (cancel_capture_batch):", err);
      throw err;
    });
}

export async function calculateExportPoolSpace(paths) {
  return invoke("calculate_export_pool_space", { paths: paths })
    .catch((err) => {
      console.error("IPC Execution Error (calculate_export_pool_space):", err);
      throw err;
    });
}

/** Per-path reason a Capture Output entry isn't usable — "ok" | "not_absolute"
 *  | "malformed" | "not_found" | "not_a_directory". Only worth calling once
 *  the aggregate space check has already found a problem; a per-path OS stat
 *  isn't cheap enough to run on every keystroke. */
export async function diagnoseCaptureOutputPaths(paths) {
  return invoke("diagnose_capture_output_paths", { paths: paths })
    .catch((err) => {
      console.error("IPC Execution Error (diagnose_capture_output_paths):", err);
      throw err;
    });
}


export async function queueRenderBatch(payload) {
  // payload must match RenderBatchPayload:
  //   { render_directories, codec, fps, ffmpeg_path?, export_directories, max_concurrent_renders }
  // Resolves to the number of takes found and staged as Queued jobs.
  return invoke("queue_render_batch", { payload: payload })
    .catch((err) => {
      console.error("IPC Execution Error (queue_render_batch):", err);
      throw err;
    });
}

export async function startQueuedRender() {
  return invoke("start_queued_render")
    .catch((err) => {
      console.error("IPC Execution Error (start_queued_render):", err);
      throw err;
    });
}

export async function cancelRenderBatch() {
  return invoke("cancel_render_batch")
    .catch((err) => {
      console.error("IPC Execution Error (cancel_render_batch):", err);
      throw err;
    });
}

export async function cancelRenderJob(jobId) {
  return invoke("cancel_render_job", { jobId })
    .catch((err) => {
      console.error("IPC Execution Error (cancel_render_job):", err);
      throw err;
    });
}

export async function resetRenderJob(jobId) {
  return invoke("reset_render_job", { jobId })
    .catch((err) => {
      console.error("IPC Execution Error (reset_render_job):", err);
      throw err;
    });
}

/** Requeues every Cancelled/Finished/Error job in one shot — resumes the
 *  scheduler immediately if nothing else is currently Rendering, same as a
 *  single Reset does. */
export async function resetAllRenderJobs() {
  return invoke("reset_all_render_jobs")
    .catch((err) => {
      console.error("IPC Execution Error (reset_all_render_jobs):", err);
      throw err;
    });
}

/** Deletes one render job's row outright — no Reset back from this, unlike
 *  Cancel. Refused server-side for a Rendering job. */
export async function removeRenderJob(jobId) {
  return invoke("remove_render_job", { jobId })
    .catch((err) => {
      console.error("IPC Execution Error (remove_render_job):", err);
      throw err;
    });
}

/** Clears every row except whatever is actively Rendering. */
export async function removeNonRenderingRenderJobs() {
  return invoke("remove_non_rendering_render_jobs")
    .catch((err) => {
      console.error("IPC Execution Error (remove_non_rendering_render_jobs):", err);
      throw err;
    });
}

export async function setRenderJobCodec(jobId, codec) {
  return invoke("set_render_job_codec", { jobId, codec })
    .catch((err) => {
      console.error("IPC Execution Error (set_render_job_codec):", err);
      throw err;
    });
}

export async function getExportPoolFreeGb(directories) {
  return invoke("get_export_pool_free_gb", { directories })
    .catch((err) => {
      console.error("IPC Execution Error (get_export_pool_free_gb):", err);
      return 0;
    });
}

/** Summed required-bytes estimate across every job still ahead of the export
 *  pool (Queued + Rendering, not Finished/Error/Cancelled). A loose upper
 *  bound, not a tight prediction — codec compression isn't known ahead of
 *  time (issue #119). */
export async function getRenderRequiredEstimateGb() {
  return invoke("get_render_required_estimate_gb")
    .catch((err) => {
      console.error("IPC Execution Error (get_render_required_estimate_gb):", err);
      return 0;
    });
}

export async function checkRenderAutosave() {
  return invoke("check_render_autosave")
    .catch((err) => {
      console.error("IPC Execution Error (check_render_autosave):", err);
      return null;
    });
}

export async function discardRenderAutosave() {
  return invoke("discard_render_autosave")
    .catch((err) => {
      console.error("IPC Execution Error (discard_render_autosave):", err);
      throw err;
    });
}

export async function recoverRenderBatch() {
  return invoke("recover_render_batch")
    .catch((err) => {
      console.error("IPC Execution Error (recover_render_batch):", err);
      throw err;
    });
}

export async function cancelScan() {
  return invoke("cancel_scan")
    .catch((err) => {
      console.error("IPC Execution Error (cancel_scan):", err);
      showToast(STRINGS.IPC.cancelScanError(err), 'error');
      throw err;
    });
}

export async function getSettings() {
  return invoke("get_settings")
    .catch((err) => {
      console.error("IPC Execution Error (get_settings):", err);
      showToast(STRINGS.IPC.settingsLoadFailed(err), 'error');
      throw err;
    });
}

export async function saveSettings(settings) {
  return invoke("save_settings", { settings })
    .catch((err) => {
      console.error("IPC Execution Error (save_settings):", err);
      showToast(STRINGS.IPC.settingsSaveFailed(err), 'error');
      throw err;
    });
}

export async function runDemoAudit(paths) {
  return invoke("run_demo_audit", { paths })
    .catch((err) => {
      console.error("IPC Execution Error (run_demo_audit):", err);
      showToast(STRINGS.IPC.auditFailed(err), 'error');
      throw err;
    });
}

export async function deleteAuditFiles(paths) {
  return invoke("delete_audit_files", { paths })
    .catch((err) => {
      console.error("IPC Execution Error (delete_audit_files):", err);
      showToast(STRINGS.IPC.deletionFailed(err), 'error');
      throw err;
    });
}

export async function cancelAudit() {
  return invoke("cancel_audit")
    .catch((err) => {
      console.error("IPC Execution Error (cancel_audit):", err);
      showToast(STRINGS.IPC.cancelAuditError(err), 'error');
      throw err;
    });
}

export async function revealInExplorer(path) {
  return invoke("reveal_in_explorer", { path })
    .catch((err) => {
      console.error("IPC Execution Error (reveal_in_explorer):", err);
      showToast(STRINGS.IPC.folderOpenFailed(err), 'error');
      throw err;
    });
}

/** Sweeps `<hl>/dod` for orphaned `*_preview.dem` bookmark previews (files
 *  that still carry their `.dodtools_preview` sidecar) left behind by prior
 *  capture sessions. `gameDir` is the configured hl.exe path. */
export async function scanOrphanedPreviews(gameDir) {
  return invoke("scan_orphaned_previews", { gameDir })
    .catch((err) => {
      console.error("IPC Execution Error (scan_orphaned_previews):", err);
      showToast(STRINGS.IPC.previewScanFailed(err), 'error');
      throw err;
    });
}

/** Deletes the given orphaned preview demos (and their sidecars). Resolves
 *  to the number of preview demos actually removed. */
export async function deleteOrphanedPreviews(filePaths) {
  return invoke("delete_orphaned_previews", { filePaths })
    .catch((err) => {
      console.error("IPC Execution Error (delete_orphaned_previews):", err);
      showToast(STRINGS.IPC.previewDeletionFailed(err), 'error');
      throw err;
    });
}

// NOTE: onRenderStatus and initRenderProgressListener have been intentionally
// removed from this module. render_pane.js registers 'render_jobs_snapshot'/
// 'render_batch_finished' listeners directly to avoid double-registration
// bugs (ipc_bridge.js must not create a second competing listener).

/** Lists the immediate subfolders and `.dem` files of `path` (drive roots
 *  when `path` is null/undefined) for the Demo Analyzer's folder/demo picker
 *  widgets. Errors (e.g. permission denied) are surfaced inline by the
 *  caller rather than as a global toast, since browsing into an
 *  inaccessible folder is an expected, recoverable event. */
export async function browseDirectory(path) {
  return invoke("browse_directory", { path: path ?? null })
    .catch((err) => {
      console.error("IPC Execution Error (browse_directory):", err);
      throw err;
    });
}

export async function defaultBrowseDir() {
  return invoke("default_browse_dir")
    .catch((err) => {
      console.error("IPC Execution Error (default_browse_dir):", err);
      return null;
    });
}

/** Non-recursive `.dem` count for a single folder — used by the Explorer
 *  sidebar's Quick Links rows (Pinned/Recent/Local). */
export async function countDemoFiles(path) {
  return invoke("count_demo_files_in_folder", { path })
    .catch((err) => {
      console.error("IPC Execution Error (count_demo_files_in_folder):", err);
      return 0;
    });
}

/** Bounded background scan (depth-4, 2000-folder cap) for folders containing
 *  at least one `.dem` file, rooted at `root` (or the default browse dir).
 *  Feeds the Explorer sidebar's "Local" Quick Links tier. */
export async function scanDemoFolders(root) {
  return invoke("scan_demo_folders", { root: root ?? null })
    .catch((err) => {
      console.error("IPC Execution Error (scan_demo_folders):", err);
      return [];
    });
}

/** Checks `channel` ("stable" or "experimental") for a newer release. Resolves to
 *  `{ version, current_version, notes, pub_date }` or `null` when already
 *  up to date. See issue #133. */
export async function checkForUpdate(channel) {
  return invoke("check_for_update", { channel })
    .catch((err) => {
      console.error("IPC Execution Error (check_for_update):", err);
      showToast(STRINGS.IPC.updateCheckFailed(err), 'error');
      throw err;
    });
}

/** Downloads and installs whatever update the last checkForUpdate() call
 *  found — throws if none is pending. Progress arrives via the
 *  `update_download_progress`/`update_ready` events, not this call's
 *  return value. */
export async function downloadAndInstallUpdate() {
  return invoke("download_and_install_update")
    .catch((err) => {
      console.error("IPC Execution Error (download_and_install_update):", err);
      showToast(STRINGS.IPC.updateInstallFailed(err), 'error');
      throw err;
    });
}

/** Reads the actual compiled-in app version (Cargo.toml's `version`, stamped
 *  by the release workflows) — never hardcoded, so it always matches what
 *  was really built, whether that's a stable release, an experimental build,
 *  or a local `npm run tauri dev` session. Best-effort: a version label isn't
 *  worth interrupting the user over. */
export async function getAppVersion() {
  return getVersion().catch((err) => {
    console.error("IPC Execution Error (getVersion):", err);
    return null;
  });
}

/** Whether this binary was compiled with debug_assertions on — true for
 *  `tauri build --debug` (a real installed bundle, unlike `npm run tauri
 *  dev`), false for a genuine `--release` build. Best-effort, same as
 *  getAppVersion(): a mislabeled build kind isn't worth interrupting the
 *  user over. */
export async function isDebugBuild() {
  return invoke("is_debug_build").catch((err) => {
    console.error("IPC Execution Error (is_debug_build):", err);
    return false;
  });
}

export async function restartApp() {
  return invoke("restart_app")
    .catch((err) => {
      console.error("IPC Execution Error (restart_app):", err);
      throw err;
    });
}
