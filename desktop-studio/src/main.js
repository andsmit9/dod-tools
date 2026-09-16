import { open, save, confirm } from '@tauri-apps/plugin-dialog';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import {
  scanDirectory,
  calculateExportPoolSpace,
  cancelScan,
  getSettings,
  saveSettings,
  logFrontendEvent,
  openActivityLog,
  checkHlaeFfmpeg,
  linkHlaeFfmpeg,
  diagnoseExecutablePaths,
  launchObs
} from './ipc_bridge.js';
import { renderMasterList, initMasterPane } from './master_pane.js';
import { initMapWarnings, refreshMapWarnings, resetMapWarnings } from './map_warnings.js';
import { initRollFloors } from './roll_floors.js';

import { renderDetailView, initDetailPane, updateStreakVisuals } from './detail_pane.js';
import { initCaptureUI, getCommandsState, hydrateCommandsState, refreshLaunchGuard, refreshInitCommandWarnings, runObsConnectionTest, renderTimingDiagram } from './capture_pane.js';
import { initRenderUI, checkRenderRecoveryOnStartup } from './render_pane.js';
import { initAuditorPane } from './auditor_pane.js';
import { initThemedConfirm, themedConfirm } from './themed_confirm.js';
import { initAnalyzerPane } from './analyzer_pane.js';
import { switchNavTab, setCaptureDetailSubtab } from './nav.js';
import { showToast } from './toast.js';
import { createListEditor } from './list_editor.js';
import { preserveHighlightState, streakUid, pruneTakeIndex, isDemoTracked } from './take_index.js';
import { getCheckedDemoPaths, clearCheckedPaths, setCheckedDemoPaths, getVisibleDemos, recordingPlayerStreaks } from './master_pane.js';
import { initErrorReporter } from './error_reporter.js';
import { STRINGS } from './strings.js';
import { applyStaticStrings } from './apply_strings.js';
import { initInfoTooltips } from './info_tooltip.js';
import { initOsNotifications, updateNotificationSettings } from './os_notifications.js';
import { initUpdater, checkForUpdatesNow } from './updater_pane.js';
import { initAppMenu } from './app_menu.js';

// Registered at module load, before DOMContentLoaded — so it's catching
// from the earliest possible moment, not just once the app's own init
// logic gets around to it.
initErrorReporter();

// ── Path Routing: does each configured path point at a real file? ────────────
// These fields accepted anything. `validate_paths` only ran at capture launch,
// so a typo sat there looking correct until a batch failed minutes later — and
// the FFmpeg override was never checked at all. The complaint goes under the
// field that caused it rather than into a banner elsewhere, so it is visible
// while you are still looking at the box you typed into.
const PATH_FIELDS = [
  ['#hl-path-input', '#hl-path-warning'],
  ['#hlae-path-input', '#hlae-path-warning'],
  ['#ffmpeg-override-path-input', '#ffmpeg-path-warning'],
];

async function refreshPathWarnings() {
  const rows = PATH_FIELDS
    .map(([input, warning]) => ({
      input: document.querySelector(input),
      warning: document.querySelector(warning),
    }))
    .filter((r) => r.input && r.warning);
  if (!rows.length) return;

  let states;
  try {
    states = await diagnoseExecutablePaths(rows.map((r) => r.input.value?.trim() || ""));
  } catch {
    // Already logged by the bridge. Clear rather than leave a stale complaint
    // standing next to a path it may no longer describe.
    rows.forEach((r) => { r.warning.style.display = 'none'; });
    return;
  }

  rows.forEach((row, i) => {
    let message = "";
    if (states[i] === 'not_found') message = STRINGS.CAPTURE_CONFIG.PATH_NOT_FOUND;
    else if (states[i] === 'not_a_file') message = STRINGS.CAPTURE_CONFIG.PATH_IS_A_FOLDER;
    // 'empty' says nothing on purpose: these are legitimately blank before they
    // are filled in, and the FFmpeg override is optional entirely.
    row.warning.textContent = message;
    row.warning.style.display = message ? '' : 'none';
  });
}

/** The selected capture mode id, defaulting to the path that always works. */
/**
 * OBS connection fields as the settings form currently holds them.
 */
function obsSettingsFromForm() {
  return {
    host: document.querySelector('#config-obs-host')?.value?.trim() || '127.0.0.1',
    port: parseInt(document.querySelector('#config-obs-port')?.value, 10) || 4455,
    password: document.querySelector('#config-obs-password')?.value || ''
  };
}

/**
 * Offers to stop an OBS recording left running by a previous session.
 *
 * The capture engine stops OBS on every exit path the process lives to run,
 * but a panic (release builds abort rather than unwind), a force-quit and a
 * power cut all leave nothing behind to run anything. OBS simply keeps
 * recording — into a folder only dod-tools would ever name — until the drive
 * fills. This is the only place that can notice.
 *
 * Silent unless there is something to act on: OBS not running is the ordinary
 * answer at startup, and a recording that is not ours is not ours to stop.
 */
async function checkObsOrphanOnStartup() {
  if (currentCaptureMode() !== 'obs') return;

  const report = await invoke('obs_check_orphan', obsSettingsFromForm()).catch((err) => {
    console.error('OBS orphan check failed:', err);
    return null;
  });
  if (!report?.recording || !report.ours) return;

  const stop = await confirm(STRINGS.CAPTURE_CONFIG.obsOrphanPrompt(report.directory), {
    title: STRINGS.CAPTURE_CONFIG.OBS_ORPHAN_TITLE,
    kind: 'warning',
    okLabel: STRINGS.CAPTURE_CONFIG.OBS_ORPHAN_STOP,
    cancelLabel: STRINGS.CAPTURE_CONFIG.OBS_ORPHAN_LEAVE
  }).catch(() => false);
  if (!stop) return;

  await invoke('obs_recover_orphan', obsSettingsFromForm())
    .then((video) => {
      showToast(
        video
          ? STRINGS.CAPTURE_CONFIG.obsOrphanRecovered(video)
          : STRINGS.CAPTURE_CONFIG.OBS_ORPHAN_GONE,
        'success'
      );
    })
    .catch((err) => {
      console.error('OBS orphan recovery failed:', err);
      showToast(STRINGS.CAPTURE_CONFIG.obsOrphanFailed(err), 'error');
    });
}

function currentCaptureMode() {
  return document.querySelector('#config-capture-mode')?.value || 'frame_sequence';
}

function applyCaptureModeUI() {
  const mode = currentCaptureMode();
  const video = mode === 'direct_to_video';
  const obs = mode === 'obs';

  // Kept in step rather than read: the backend still accepts `ffmpeg_capture`
  // from older payloads, and leaving it stale would make the two disagree for
  // anything that has not moved to the enum yet.
  const legacy = document.querySelector('#config-ffmpeg-capture');
  if (legacy) legacy.checked = video;

  document.querySelectorAll('.setting-label[data-capture-mode]').forEach((label) => {
    label.classList.toggle('active', (label.dataset.captureMode === 'video') === video);
  });
  // Hidden rather than disabled, in both other modes: frame-sequence mode
  // has its own, unrelated answer to "what codec" — Render Studio's own
  // codec picker, a different set of options (ProRes/DNxHR/H.264) for a
  // different purpose (final delivery, not the capture-time lossless
  // intermediate). OBS mode does not consume this setting today either. If
  // OBS capture grows its own codec choice later (it can already ask for a
  // container in Custom Output mode), this is where that would show — but
  // "will eventually" is not "does now", and showing it today would claim a
  // connection to OBS capture that does not exist yet.
  const codecGroup = document.querySelector('#capture-codec-group');
  if (codecGroup) codecGroup.style.display = video ? '' : 'none';

  // Capture FPS is non-real-time for the other two modes but meaningless for
  // OBS, which has its own separate OBS Capture FPS field below — showing
  // both invites setting the wrong one.
  const captureFpsGroup = document.querySelector('#capture-fps-group');
  if (captureFpsGroup) captureFpsGroup.style.display = obs ? 'none' : '';

  // The OBS block follows the same rule: hidden rather than disabled,
  // because showing a dead connection form in frame-sequence mode would
  // suggest OBS is involved when it is not.
  const obsGroup = document.querySelector('#obs-settings-group');
  if (obsGroup) obsGroup.style.display = obs ? '' : 'none';
}

// ── HLAE's own FFmpeg ─────────────────────────────────────────────────────────
// `mirv_movie_ffmpeg` makes HLAE spawn FFmpeg itself, and it does not consult
// the app's FFmpeg setting — it looks only in its own folder or at an ffmpeg.ini
// beside it. With neither present, direct-to-video capture runs to completion
// and produces no video, so the state is surfaced here rather than discovered
// after a batch. See docs/direct_to_video_capture.md.
async function refreshHlaeFfmpegStatus() {
  const statusEl = document.querySelector('#hlae-ffmpeg-status');
  const linkBtn = document.querySelector('#hlae-ffmpeg-link-btn');
  if (!statusEl || !linkBtn) return;

  const hlaePath = document.querySelector('#hlae-path-input')?.value?.trim() || "";
  const unknown = () => {
    statusEl.textContent = STRINGS.CAPTURE_CONFIG.HLAE_FFMPEG_UNKNOWN;
    linkBtn.style.display = 'none';
  };
  if (!hlaePath) return unknown();

  // Passed in so the check can say whether HLAE and Render Studio agree, not
  // just whether HLAE has an answer at all.
  const ffmpegPath =
    document.querySelector('#ffmpeg-override-path-input')?.value?.trim() || "ffmpeg";

  let result;
  try {
    result = await checkHlaeFfmpeg(hlaePath, ffmpegPath);
  } catch {
    // Already logged by the bridge. Say nothing rather than assert a state.
    return unknown();
  }

  const s = result?.state || {};
  // Outranks every message below it, because it questions the thing they are
  // all about: if the hook DLL is not there, capture cannot work regardless of
  // what HLAE's ffmpeg folder contains. Still only a note — an unusual layout
  // should not stop someone who knows their install works.
  if (result.missing_hook_dll) {
    statusEl.textContent =
      STRINGS.CAPTURE_CONFIG.HLAE_FFMPEG_NO_HOOK_DLL(result.missing_hook_dll);
    linkBtn.style.display = result?.can_link ? '' : 'none';
    return;
  }

  // The toggle is on and HLAE has nothing to pipe to. Worth saying more
  // sharply than the generic "no FFmpeg" line below, because this is the
  // combination that produces a capture which runs to completion and records
  // no video at all.
  if (document.querySelector('#config-ffmpeg-capture')?.checked && !result.usable) {
    statusEl.textContent = STRINGS.CAPTURE_CONFIG.FFMPEG_CAPTURE_UNAVAILABLE;
    linkBtn.style.display = result?.can_link ? '' : 'none';
    return;
  }

  switch (s.state) {
    case 'bundled':
      statusEl.textContent = STRINGS.CAPTURE_CONFIG.HLAE_FFMPEG_BUNDLED(s.path);
      break;
    case 'linked':
      // Outranks everything below: if the override is not FFmpeg, saying the
      // two "disagree" describes a real difference and hides the actual
      // problem, and the button would only write the wrong program in.
      if (result.app_ffmpeg_problem) {
        statusEl.textContent =
          STRINGS.CAPTURE_CONFIG.HLAE_FFMPEG_BAD_OVERRIDE(result.app_ffmpeg_problem);
      } else if (!s.target_exists) {
        // A stale pointer outranks a disagreement: it is not pointed at
        // anything at all, so which build it disagrees with is moot.
        statusEl.textContent = STRINGS.CAPTURE_CONFIG.HLAE_FFMPEG_STALE(s.target);
      } else if (result.agrees_with_app === false && result.app_ffmpeg) {
        statusEl.textContent =
          STRINGS.CAPTURE_CONFIG.HLAE_FFMPEG_DIVERGED(s.target, result.app_ffmpeg);
      } else {
        statusEl.textContent = STRINGS.CAPTURE_CONFIG.HLAE_FFMPEG_LINKED(s.target);
      }
      break;
    case 'missing':
      statusEl.textContent = result.app_ffmpeg_problem
        ? STRINGS.CAPTURE_CONFIG.HLAE_FFMPEG_BAD_OVERRIDE(result.app_ffmpeg_problem)
        : STRINGS.CAPTURE_CONFIG.HLAE_FFMPEG_MISSING;
      break;
    default:
      return unknown();
  }
  // Offered only where it can actually be acted on: never over a bundled
  // binary, and never over an existing ini, which is left alone on purpose.
  linkBtn.style.display = result?.can_link ? '' : 'none';
}

window.addEventListener("DOMContentLoaded", async () => {
  // Applies every [data-str]/[data-str-title]/[data-str-placeholder]/
  // [data-str-aria-label] element's text/attribute from STRINGS before any
  // other DOM-dependent init runs below.
  applyStaticStrings();
  initInfoTooltips();

  // Not awaited: the permission prompt (first run only) shouldn't block the
  // rest of startup, and every call site in os_notifications.js already
  // no-ops silently until permission is granted.
  initOsNotifications();

  let scanPaths = [];
  // Analyzer Explorer sidebar's "Recent" quick-links tier — most-recent-first,
  // capped at 10, pushed via recordDemoFolderVisit() below whenever browsing
  // into a folder yields a non-empty demo listing. Mirrors dev's
  // `settings.demo_folder_history` (see docs/archive/tauri_parity_audit.md Area 3).
  let demoFolderHistory = [];
  // Gates the Analyzer Explorer tree's per-subfolder demo-count badge.
  // Mirrors dev's `settings.scan_folders_for_demos`, default false.
  let scanFoldersForDemos = false;
  // Analyzer Explorer sidebar's drag-to-resize width (analyzer_pane.js).
  let analyzerExplorerWidth = 260;
  // Doubles as Render Studio's scan-input locations — see initRenderUI below.
  let targetDrives = [];
  let renderExportDirs = []; // JIT multi-drive export pool for Render Studio
  let currentScannedDemos = [];
  let selectedDemoIdx = null;
  // The project session file last loaded or saved in this window, if any —
  // once set, "Save Session" writes straight back to it instead of asking
  // Save-As every time (matches Ctrl+S's behavior in every other app).
  let currentSessionPath = null;
  // take_key -> uid[]. Recorded by capture_pane.js when a batch verifies a
  // block on disk; resolved by render_pane.js when that take finishes
  // rendering, so status can auto-advance even after a restart or re-scan
  // replaced the original streak objects. Persisted in the project file.
  let takeIndex = {};
  // True whenever project state (scanned demos, takeIndex, scanPaths) has
  // changed since the last successful save or load — gates the "unsaved
  // changes" prompt on window close. Cleared by saveProjectSession() and
  // Load Session; set by markProjectDirty() at every mutation site.
  let hasUnsavedChanges = false;
  /** Every highlight's durable uid across every currently-scanned demo — the
   *  "still exists" set pruneTakeIndex() checks the take index against on save. */
  function collectAllUids() {
    const uids = [];
    currentScannedDemos.forEach(demo => {
      (demo.streaks || []).forEach(streak => uids.push(streakUid(demo.path, streak)));
    });
    return uids;
  }

  // Whether saveProjectSession() would actually have something to write: a
  // non-empty queue is always savable, but so is an emptied one once a
  // session file exists to write it back to — clearing everything is a
  // real, meaningful change relative to that file, not a no-op.
  function hasSavableProject() {
    return currentScannedDemos.length > 0 || !!currentSessionPath;
  }

  function updateSessionFileIndicator() {
    const el = document.querySelector('#session-file-indicator');
    if (!el) return;
    const dirtySuffix = hasUnsavedChanges ? ' • unsaved' : '';
    if (currentSessionPath) {
      const filename = currentSessionPath.split(/[\\/]/).pop() || currentSessionPath;
      el.textContent = filename + dirtySuffix;
      el.title = currentSessionPath;
    } else {
      el.textContent = STRINGS.NAV.NO_SESSION_LOADED + dirtySuffix;
      el.title = '';
    }
  }

  // Marks Capture Studio's project state as changed since the last save —
  // called at every mutation site for currentScannedDemos/takeIndex/
  // scanPaths. Gates the close-window "unsaved changes" prompt below.
  function markProjectDirty() {
    hasUnsavedChanges = true;
    updateSessionFileIndicator();
  }

  // Initialize modular UI panes
  initThemedConfirm();
  initAuditorPane();

  async function pickTargetDrive() {
    try {
      return await open({ directory: true, multiple: false, title: STRINGS.MAIN.SELECT_CAPTURE_OUTPUT_DIR_TITLE });
    } catch (err) {
      console.error("Error opening capture output directory dialog:", err);
      return null;
    }
  }

  async function pickRenderExportDir() {
    try {
      return await open({ directory: true, multiple: false, title: STRINGS.MAIN.SELECT_RENDER_EXPORT_DIR_TITLE });
    } catch (err) {
      console.error("Error opening render export directory dialog:", err);
      return null;
    }
  }

  // Shared editable-list widget (list_editor.js) for the two folder/drive
  // pools — Capture Locations (doubles as Render Studio's scan input) and
  // Render Studio's Export Drives — get add/edit/remove/reorder/browse from
  // one implementation.
  const driveOverridesEditor = createListEditor({
    container: document.querySelector('#target-drive-list'),
    getItems: () => targetDrives,
    fields: [{ key: 'value', type: 'text', primitive: true, placeholder: STRINGS.CAPTURE_CONFIG.OUTPUT_DIR_PLACEHOLDER }],
    unique: true,
    browse: pickTargetDrive,
    onChange: () => {
      updateExportPoolIndicator();
      persistAppSettings();
      refreshLaunchGuard({ targetDrives, currentScannedDemos });
    },
  });

  const renderExportDirsEditor = createListEditor({
    container: document.querySelector('#render-export-dir-list'),
    getItems: () => renderExportDirs,
    fields: [{ key: 'value', type: 'text', primitive: true, placeholder: STRINGS.RENDER.EXPORT_DIR_ROW_PLACEHOLDER }],
    unique: true,
    browse: pickRenderExportDir,
    onChange: () => persistAppSettings(),
  });

  // Helper to persist application settings
  async function persistAppSettings() {
    const hlaePath = document.querySelector('#hlae-path-input')?.value?.trim() || "";
    const hlPath = document.querySelector('#hl-path-input')?.value?.trim() || "";
    const ffmpegPath = document.querySelector('#ffmpeg-override-path-input')?.value?.trim() || null;
    const captureFps = parseInt(document.querySelector('#config-capture-fps')?.value, 10) || 300;
    const obsCaptureFps = parseInt(document.querySelector('#config-obs-capture-fps')?.value, 10) || 120;
    const preRoll = parseFloat(document.querySelector('#config-pre-roll')?.value) || 2.0;
    const postRoll = parseFloat(document.querySelector('#config-post-roll')?.value) || 0.6;

    const resWidth = parseInt(document.querySelector('#config-res-width')?.value, 10) || 1280;
    const resHeight = parseInt(document.querySelector('#config-res-height')?.value, 10) || 720;
    // Defaults on when the element is missing, matching the backend default —
    // `?? true` rather than `|| false`, which would silently disable it.
    const decalFlush = document.querySelector('#config-decal-flush')?.checked ?? true;
    const captureMode = currentCaptureMode();
    // Derived from the mode, never read from the checkbox — the select is the
    // authority and the checkbox is a compatibility mirror.
    const ffmpegCapture = captureMode === 'direct_to_video';
    const ffmpegCaptureCodec = document.querySelector('#config-capture-codec')?.value || 'utvideo';
    const obsHost = document.querySelector('#config-obs-host')?.value?.trim() || '127.0.0.1';
    const obsPort = parseInt(document.querySelector('#config-obs-port')?.value, 10) || 4455;
    const obsPassword = document.querySelector('#config-obs-password')?.value || '';
    const obsExePath = document.querySelector('#config-obs-exe-path')?.value?.trim() || '';

    const autoClearLogs = document.querySelector('#config-auto-clear-logs')?.checked || false;
    const autoClearPreviews = document.querySelector('#config-auto-clear-previews')?.checked || false;
    const autoClearTempDemos = document.querySelector('#config-auto-clear-temp-demos')?.checked || false;

    // OS notification toggles (issue #98) — default on, matching decalFlush's
    // `?? true` style above rather than `|| false`, which would silently
    // disable them for anyone whose settings predate this field.
    const notifyPatching = document.querySelector('#config-notify-patching')?.checked ?? true;
    const notifyDemoLoading = document.querySelector('#config-notify-demo-loading')?.checked ?? true;
    const notifyBetweenClips = document.querySelector('#config-notify-between-clips')?.checked ?? true;
    const notifyCapturesDone = document.querySelector('#config-notify-captures-done')?.checked ?? true;
    const notifyRendersDone = document.querySelector('#config-notify-renders-done')?.checked ?? true;
    const notifyError = document.querySelector('#config-notify-error')?.checked ?? true;
    const notifyUpdates = document.querySelector('#config-notify-updates')?.checked ?? true;
    const updateChannel = document.querySelector('#config-update-channel')?.value || 'stable';
    const autoCheckUpdates = document.querySelector('#config-auto-check-updates')?.checked ?? true;

    const recordStartLead = parseFloat(document.querySelector('#config-record-start-lead')?.value) || 0.0;
    const recordStopTrail = parseFloat(document.querySelector('#config-record-stop-trail')?.value) || 0.0;
    const initialDelay = parseFloat(document.querySelector('#config-initial-delay')?.value) || 3.0;
    const fastForwardSpeed = parseFloat(document.querySelector('#config-fast-forward-speed')?.value) || 0.05;

    const saveLocalPatchedCopy = document.querySelector('#config-save-local-patched')?.checked || false;

    const renderCodec = document.querySelector('#render-codec-select')?.value || 'prores';
    const renderFps = parseInt(document.querySelector('#render-fps-input')?.value, 10) || 300;
    const renderMaxConcurrent = parseInt(document.querySelector('#render-max-concurrent-input')?.value, 10) || 2;

    const { init_commands, custom_commands } = getCommandsState();

    const settingsPayload = {
      hlae_path: hlaePath,
      hl_path: hlPath,
      ffmpeg_path: ffmpegPath,
      pinned_folders: scanPaths,
      demo_folder_history: demoFolderHistory,
      scan_folders_for_demos: scanFoldersForDemos,
      analyzer_explorer_width: analyzerExplorerWidth,
      language: "en",
      capture_fps: captureFps,
      obs_capture_fps: obsCaptureFps,
      pre_roll_seconds: preRoll,
      post_roll_seconds: postRoll,
      resolution_width: resWidth,
      resolution_height: resHeight,
      decal_flush: decalFlush,
      ffmpeg_capture: ffmpegCapture,
      ffmpeg_capture_codec: ffmpegCaptureCodec,
      capture_mode: captureMode,
      obs_host: obsHost,
      obs_port: obsPort,
      obs_password: obsPassword,
      obs_exe_path: obsExePath,
      auto_clear_logs: autoClearLogs,
      auto_clear_previews: autoClearPreviews,
      auto_clear_temp_demos: autoClearTempDemos,
      notify_patching: notifyPatching,
      notify_demo_loading: notifyDemoLoading,
      notify_between_clips: notifyBetweenClips,
      notify_captures_done: notifyCapturesDone,
      notify_renders_done: notifyRendersDone,
      notify_error: notifyError,
      notify_updates: notifyUpdates,
      update_channel: updateChannel,
      auto_check_updates: autoCheckUpdates,
      record_start_lead: recordStartLead,
      record_stop_trail: recordStopTrail,
      initial_delay: initialDelay,
      fast_forward_speed: fastForwardSpeed,
      target_drives: targetDrives,
      init_commands,
      custom_commands,
      save_local_patched_copy: saveLocalPatchedCopy,
      render_codec: renderCodec,
      render_fps: renderFps,
      render_max_concurrent: renderMaxConcurrent,
      render_export_dirs: renderExportDirs
    };
    // Reflects a just-flipped toggle immediately, rather than waiting on the
    // save round-trip below to come back through a settings reload.
    updateNotificationSettings(settingsPayload);
    try {
      await saveSettings(settingsPayload);
    } catch (err) {
      console.error("Error auto-saving settings:", err);
    }
  }

  // Load persistent settings on startup
  let settings = null;
  try {
    settings = await getSettings();
    if (settings) {
      updateNotificationSettings(settings);
      if (settings.hlae_path) {
        const inputEl = document.querySelector('#hlae-path-input');
        if (inputEl) inputEl.value = settings.hlae_path;
      }
      if (settings.hl_path) {
        const inputEl = document.querySelector('#hl-path-input');
        if (inputEl) inputEl.value = settings.hl_path;
        // The game folder is only known once the persisted path lands here, so
        // this is where the config scan can first say anything useful.
        // Deferred: the init commands hydrate later in this same load, and the
        // warning is about how those two interact.
      }
      if (settings.ffmpeg_path) {
        const inputEl = document.querySelector('#ffmpeg-override-path-input');
        if (inputEl) inputEl.value = settings.ffmpeg_path;
      }
      if (settings.hlae_path) {
        // Issue #101: this reads both #hlae-path-input and
        // #ffmpeg-override-path-input, so it has to run after *both* are
        // populated above, not right after hlae_path alone — calling it
        // there read the override field before its own value had landed,
        // silently fell back to the bare "ffmpeg" PATH lookup, and produced
        // a false "there is no file at \"ffmpeg\"" warning that then never
        // got re-checked, since setting .value programmatically fires no
        // 'change' event to trigger the listener further down this file.
        //
        // Not awaited: it is a status line, and blocking startup on a
        // filesystem check of somebody else's install directory would trade a
        // real cost for a cosmetic one.
        refreshHlaeFfmpegStatus();
      }
      if (settings.capture_fps) {
        const inputEl = document.querySelector('#config-capture-fps');
        if (inputEl) inputEl.value = settings.capture_fps;
      }
      if (settings.obs_capture_fps) {
        const inputEl = document.querySelector('#config-obs-capture-fps');
        if (inputEl) inputEl.value = settings.obs_capture_fps;
      }
      if (settings.pre_roll_seconds) {
        const inputEl = document.querySelector('#config-pre-roll');
        if (inputEl) inputEl.value = settings.pre_roll_seconds;
      }
      if (settings.post_roll_seconds) {
        const inputEl = document.querySelector('#config-post-roll');
        if (inputEl) inputEl.value = settings.post_roll_seconds;
      }
      if (settings.resolution_width) {
        const inputEl = document.querySelector('#config-res-width');
        if (inputEl) inputEl.value = settings.resolution_width;
      }
      if (settings.resolution_height) {
        const inputEl = document.querySelector('#config-res-height');
        if (inputEl) inputEl.value = settings.resolution_height;
      }
      const decalFlushEl = document.querySelector('#config-decal-flush');
      if (decalFlushEl) decalFlushEl.checked = settings.decal_flush !== false;
      const ffmpegCaptureEl = document.querySelector('#config-ffmpeg-capture');
      if (ffmpegCaptureEl) ffmpegCaptureEl.checked = !!settings.ffmpeg_capture;
      const captureCodecEl = document.querySelector('#config-capture-codec');
      if (captureCodecEl && settings.ffmpeg_capture_codec) captureCodecEl.value = settings.ffmpeg_capture_codec;
      const captureModeEl = document.querySelector('#config-capture-mode');
      if (captureModeEl) {
        // A settings file written before the selector existed has no
        // capture_mode and only ffmpeg_capture, so fall back to it rather than
        // silently resetting the user's choice to frame sequence.
        captureModeEl.value =
          settings.capture_mode || (settings.ffmpeg_capture ? 'direct_to_video' : 'frame_sequence');
      }
      const obsHostEl = document.querySelector('#config-obs-host');
      if (obsHostEl) obsHostEl.value = settings.obs_host || '127.0.0.1';
      const obsPortEl = document.querySelector('#config-obs-port');
      if (obsPortEl) obsPortEl.value = settings.obs_port || 4455;
      const obsPasswordEl = document.querySelector('#config-obs-password');
      if (obsPasswordEl) obsPasswordEl.value = settings.obs_password || '';
      const obsExePathEl = document.querySelector('#config-obs-exe-path');
      if (obsExePathEl) obsExePathEl.value = settings.obs_exe_path || '';
      applyCaptureModeUI();
      // Deliberately NOT checked here at startup — OBS is the user's own
      // program and, like HLAE, is not expected to already be running just
      // because dod-tools opened. The connectivity check runs when actively
      // switching into OBS mode below, and again as Start Capture Batch's
      // own pre-flight (capture_pane.js) — both are moments the user is
      // actually about to use it, unlike app launch.
      const autoClearLogsEl = document.querySelector('#config-auto-clear-logs');
      if (autoClearLogsEl) autoClearLogsEl.checked = !!settings.auto_clear_logs;
      const autoClearPreviewsEl = document.querySelector('#config-auto-clear-previews');
      if (autoClearPreviewsEl) autoClearPreviewsEl.checked = !!settings.auto_clear_previews;
      const autoClearTempDemosEl = document.querySelector('#config-auto-clear-temp-demos');
      if (autoClearTempDemosEl) autoClearTempDemosEl.checked = !!settings.auto_clear_temp_demos;
      const notifyPatchingEl = document.querySelector('#config-notify-patching');
      if (notifyPatchingEl) notifyPatchingEl.checked = !!settings.notify_patching;
      const notifyDemoLoadingEl = document.querySelector('#config-notify-demo-loading');
      if (notifyDemoLoadingEl) notifyDemoLoadingEl.checked = !!settings.notify_demo_loading;
      const notifyBetweenClipsEl = document.querySelector('#config-notify-between-clips');
      if (notifyBetweenClipsEl) notifyBetweenClipsEl.checked = !!settings.notify_between_clips;
      const notifyCapturesDoneEl = document.querySelector('#config-notify-captures-done');
      if (notifyCapturesDoneEl) notifyCapturesDoneEl.checked = !!settings.notify_captures_done;
      const notifyRendersDoneEl = document.querySelector('#config-notify-renders-done');
      if (notifyRendersDoneEl) notifyRendersDoneEl.checked = !!settings.notify_renders_done;
      const notifyErrorEl = document.querySelector('#config-notify-error');
      if (notifyErrorEl) notifyErrorEl.checked = !!settings.notify_error;
      const notifyUpdatesEl = document.querySelector('#config-notify-updates');
      if (notifyUpdatesEl) notifyUpdatesEl.checked = settings.notify_updates !== false;
      const updateChannelEl = document.querySelector('#config-update-channel');
      if (updateChannelEl) updateChannelEl.value = settings.update_channel || 'stable';
      const autoCheckUpdatesEl = document.querySelector('#config-auto-check-updates');
      if (autoCheckUpdatesEl) autoCheckUpdatesEl.checked = settings.auto_check_updates !== false;
      if (settings.record_start_lead) {
        const inputEl = document.querySelector('#config-record-start-lead');
        if (inputEl) inputEl.value = settings.record_start_lead;
      }
      if (settings.record_stop_trail) {
        const inputEl = document.querySelector('#config-record-stop-trail');
        if (inputEl) inputEl.value = settings.record_stop_trail;
      }
      if (settings.initial_delay) {
        const inputEl = document.querySelector('#config-initial-delay');
        if (inputEl) inputEl.value = settings.initial_delay;
      }
      // All five timing fields are set by this point — reflect the loaded
      // values in the Timings tab's visual timeline (#150).
      renderTimingDiagram();
      if (settings.fast_forward_speed) {
        const inputEl = document.querySelector('#config-fast-forward-speed');
        if (inputEl) inputEl.value = settings.fast_forward_speed;
      }
      const saveLocalPatchedEl = document.querySelector('#config-save-local-patched');
      if (saveLocalPatchedEl) saveLocalPatchedEl.checked = !!settings.save_local_patched_copy;
      if (settings.render_codec) {
        const inputEl = document.querySelector('#render-codec-select');
        if (inputEl) inputEl.value = settings.render_codec;
      }
      if (settings.render_fps) {
        const inputEl = document.querySelector('#render-fps-input');
        if (inputEl) inputEl.value = settings.render_fps;
      }
      if (settings.render_max_concurrent) {
        const inputEl = document.querySelector('#render-max-concurrent-input');
        if (inputEl) inputEl.value = settings.render_max_concurrent;
      }
      if (Array.isArray(settings.pinned_folders) && settings.pinned_folders.length > 0) {
        scanPaths = [...settings.pinned_folders];
      }
      if (Array.isArray(settings.demo_folder_history) && settings.demo_folder_history.length > 0) {
        demoFolderHistory = [...settings.demo_folder_history];
      }
      scanFoldersForDemos = !!settings.scan_folders_for_demos;
      if (settings.analyzer_explorer_width) {
        analyzerExplorerWidth = settings.analyzer_explorer_width;
      }
      if (Array.isArray(settings.target_drives) && settings.target_drives.length > 0) {
        targetDrives = [...settings.target_drives];
        driveOverridesEditor.render();
        updateExportPoolIndicator();
      }
      if (Array.isArray(settings.render_export_dirs) && settings.render_export_dirs.length > 0) {
        renderExportDirs = [...settings.render_export_dirs];
        renderExportDirsEditor.render();
      }
      hydrateCommandsState(settings.init_commands, settings.custom_commands);
      // Both halves of the question are now in the DOM: the game path, and the
      // commands that will run against whatever its configs set.
      refreshInitCommandWarnings();
    }
  } catch (err) {
    console.error("Error loading startup settings:", err);
  }
  // Wires the Updates tab's buttons regardless of whether settings loaded —
  // only the startup auto-check itself is conditional on settings being
  // present. Not awaited: a background check shouldn't block startup.
  initUpdater(settings, persistAppSettings);
  initAppMenu();

  // Save Project Session — also called from the Clear All modal's "Save
  // Session First" action, so it lives here as a plain function rather than
  // only inline in the button's click handler. Returns whether it actually
  // wrote a file (false on "nothing to save" or a cancelled Save-As dialog).
  async function saveProjectSession() {
    if (!hasSavableProject()) {
      showToast(STRINGS.MAIN.NOTHING_TO_SAVE, 'info');
      return false;
    }
    // Already matches what's on disk — skip the write and the misleading
    // "saved" toast, but still report success so callers that gate on the
    // return value (Clear All's Save-First, the close-window prompt) treat
    // this the same as an actual save rather than a failure.
    if (!hasUnsavedChanges) {
      showToast(STRINGS.MAIN.ALREADY_SAVED, 'info');
      return true;
    }
    try {
      // Once a session's been loaded or saved once in this window, keep
      // writing back to that same file instead of asking Save-As again.
      const filePath = currentSessionPath || await save({
        title: STRINGS.MAIN.SAVE_PROJECT_SESSION_TITLE,
        defaultPath: 'dod_project.json',
        filters: [{ name: STRINGS.MAIN.JSON_PROJECT_FILTER_NAME, extensions: ['json'] }]
      });
      if (!filePath) return false;

      const hlaePath = document.querySelector('#hlae-path-input')?.value || "";
      const hlPath = document.querySelector('#hl-path-input')?.value || "";
      const projectData = JSON.stringify({
        version: "0.12.0",
        scanPaths: scanPaths,
        demos: currentScannedDemos,
        hlaePath: hlaePath,
        hlPath: hlPath,
        // Pruned against what's actually still scanned so the index
        // doesn't accumulate uids for demos removed from the project.
        takeIndex: pruneTakeIndex(takeIndex, collectAllUids()),
        // Kept for older-file/older-version compatibility — nothing on the
        // reading side branches on it any more (Quick-Clip mode is gone).
        mode: 'workspace'
      }, null, 2);
      await invoke('save_project_session', { path: filePath, contents: projectData });
      currentSessionPath = filePath;
      hasUnsavedChanges = false;
      updateSessionFileIndicator();
      showToast(STRINGS.MAIN.projectSavedToast(filePath), 'success');
      return true;
    } catch (err) {
      console.error("Save project error:", err);
      showToast(STRINGS.MAIN.SAVE_PROJECT_ERROR, 'error');
      return false;
    }
  }

  const viewLogsBtn = document.querySelector('#view-logs-btn');
  if (viewLogsBtn) {
    viewLogsBtn.addEventListener('click', () => openActivityLog());
  }

  const saveProjectBtn = document.querySelector('#save-project-btn');
  if (saveProjectBtn) {
    saveProjectBtn.addEventListener('click', () => saveProjectSession());
  }

  // Load Project Session
  const loadProjectBtn = document.querySelector('#load-project-btn');
  if (loadProjectBtn) {
    loadProjectBtn.addEventListener('click', async () => {
      // Loading replaces currentScannedDemos/takeIndex wholesale — same
      // data-loss risk as closing the window, so it gets the same prompt
      // before that happens. Identical guard to the window-close handler
      // below, reusing the same modal (requestUnsavedChangesConfirmation,
      // hoisted function declaration defined later in this scope).
      if (hasUnsavedChanges && hasSavableProject()) {
        const outcome = await requestUnsavedChangesConfirmation();
        if (!outcome) return; // Cancel — abort the load, keep current state
        // 'save' already wrote the file inside the modal's Save button
        // handler; 'discard' falls through to load over it either way.
      }
      try {
        const selected = await open({
          multiple: false,
          filters: [{ name: STRINGS.MAIN.JSON_PROJECT_FILTER_NAME, extensions: ['json'] }]
        });
        if (selected) {
          const content = await invoke('load_project_session', { path: selected });
          const data = JSON.parse(content);
          if (data) {
            currentSessionPath = selected;
            hasUnsavedChanges = false;
            updateSessionFileIndicator();
            // Load Session is reachable from any tab (#122) — jump to Studio
            // so the loaded project is actually visible, same cross-tab-jump
            // pattern as detail_pane.js's "View Match Telemetry" button.
            switchNavTab('workspace');
            clearCheckedPaths();
            if (data.hlaePath) {
              const hlaeInput = document.querySelector('#hlae-path-input');
              if (hlaeInput) hlaeInput.value = data.hlaePath;
            }
            if (data.hlPath) {
              const hlInput = document.querySelector('#hl-path-input');
              if (hlInput) hlInput.value = data.hlPath;
            }
            // Tolerant: a 0.10.0 project file has no takeIndex at all — load
            // as empty rather than reject the file. Auto-Rendered just won't
            // retroactively apply to takes captured before this existed.
            takeIndex = data.takeIndex || {};
            // Deliberately verbose: this is the only place takeIndex is ever
            // populated from disk, so logging it here — with exactly what
            // came out of the file, before anything else touches it — is
            // what makes it possible to prove a later auto-Rendered flip
            // came from this loaded data and not a leftover in-memory state.
            console.log(`[take-index] Loaded from ${selected}: ${Object.keys(takeIndex).length} take(s)`, takeIndex);
            if (data.demos) {
              currentScannedDemos = data.demos;
              // timeline_string is a derived field, saved as a convenience
              // snapshot rather than the source of truth — recompute it from
              // each streak's raw kills on every load so a display-only fix
              // (e.g. a weapon-name-resolution bug) shows correctly for
              // sessions saved before the fix, instead of replaying whatever
              // text got baked in at save time.
              currentScannedDemos.forEach(demo => (demo.streaks || []).forEach(updateStreakVisuals));
              selectedDemoIdx = currentScannedDemos.length > 0 ? 0 : null;
              renderMasterList(currentScannedDemos, selectedDemoIdx, selectDemoAndRenderDetail);
              if (currentScannedDemos.length > 0) {
                selectDemoAndRenderDetail(currentScannedDemos[0], selectedDemoIdx);
              }
              updateDemoFooter(currentScannedDemos);
              showToast(STRINGS.MAIN.loadedDemosToast(currentScannedDemos.length), 'success');
            }
          }
        }
      } catch (err) {
        console.error("Load project error:", err);
        showToast(STRINGS.MAIN.LOAD_PROJECT_ERROR, 'error');
      }
    });
  }

  // New Session (#122/#149) — resets to the same blank state the app starts
  // in: no session file, no demos, no take index. Reuses replaceScannedDemos
  // (defined below, hoisted) for the demo-queue reset — same as Clear All —
  // then overrides the dirty flag it sets, since a brand new untitled
  // session has nothing to prompt about saving.
  async function newSession() {
    if (hasUnsavedChanges && hasSavableProject()) {
      const outcome = await requestUnsavedChangesConfirmation();
      if (!outcome) return; // Cancel — abort, keep current state
    }
    replaceScannedDemos([]);
    currentSessionPath = null;
    takeIndex = {};
    hasUnsavedChanges = false;
    updateSessionFileIndicator();
    switchNavTab('workspace');
    showToast(STRINGS.MAIN.NEW_SESSION_TOAST, 'success');
  }

  document.querySelector('#new-session-btn')?.addEventListener('click', () => newSession());

  // Executable & Path Browse Dialog Pickers
  const hlaeBrowseBtn = document.querySelector('#hlae-browse-btn');
  if (hlaeBrowseBtn) {
    hlaeBrowseBtn.addEventListener('click', async () => {
      try {
        const selected = await open({
          multiple: false,
          filters: [{ name: STRINGS.MAIN.EXECUTABLE_FILTER_NAME, extensions: ['exe'] }],
          title: STRINGS.MAIN.SELECT_HLAE_EXE_TITLE
        });
        if (selected) {
          const path = Array.isArray(selected) ? selected[0] : selected;
          const inputEl = document.querySelector('#hlae-path-input');
          if (inputEl) inputEl.value = path;
          await persistAppSettings();
          await refreshHlaeFfmpegStatus();
          await refreshPathWarnings();
        }
      } catch (err) {
        console.error("Error selecting HLAE executable:", err);
      }
    });
  }

  // Typing a path by hand is the other way in, so re-check on blur/Enter as
  // well as after the picker. The FFmpeg override matters too: it does not
  // change what HLAE points at, which is exactly why a change there can leave
  // the two pointed at different builds without anything saying so.
  for (const id of ['#hlae-path-input', '#ffmpeg-override-path-input']) {
    document.querySelector(id)
      ?.addEventListener('change', () => { refreshHlaeFfmpegStatus(); });
  }
  // Every path field, including Half-Life, which the row above says nothing
  // about.
  for (const [input] of PATH_FIELDS) {
    document.querySelector(input)
      ?.addEventListener('change', () => { refreshPathWarnings(); });
  }
  refreshPathWarnings();

  // Toggling capture-to-video changes what the row above needs to say: with it
  // on, "HLAE has no FFmpeg" stops being a note about an unused feature and
  // becomes the reason the next batch will record nothing.
  document.querySelector('#config-ffmpeg-capture')
    ?.addEventListener('change', () => {
      applyCaptureModeUI();
      refreshHlaeFfmpegStatus();
    });
  // The mode selector is the authority. It keeps the legacy checkbox in step
  // and then fires its 'change', so persistence and the HLAE FFmpeg status row
  // — both of which already hang off that event — keep working unchanged.
  document.querySelector('#config-capture-mode')
    ?.addEventListener('change', () => {
      applyCaptureModeUI();
      const legacy = document.querySelector('#config-ffmpeg-capture');
      if (legacy) legacy.dispatchEvent(new Event('change', { bubbles: true }));
      // Deliberately no auto-check here: OBS is not expected to already be
      // open just because the user switched into OBS mode, and warning about
      // that on every switch was pure noise. Test Connection covers the
      // manual case; Start Capture Batch runs the real pre-flight (and
      // launches/retries OBS itself) right before it matters.
    });
  applyCaptureModeUI();

  document.querySelector('#obs-test-btn')?.addEventListener('click', () => runObsConnectionTest());

  const obsBrowseBtn = document.querySelector('#obs-browse-btn');
  if (obsBrowseBtn) {
    obsBrowseBtn.addEventListener('click', async () => {
      try {
        const selected = await open({
          multiple: false,
          filters: [{ name: STRINGS.MAIN.EXECUTABLE_FILTER_NAME, extensions: ['exe'] }],
          title: STRINGS.MAIN.SELECT_OBS_EXE_TITLE
        });
        if (selected) {
          const path = Array.isArray(selected) ? selected[0] : selected;
          const inputEl = document.querySelector('#config-obs-exe-path');
          if (inputEl) inputEl.value = path;
          await persistAppSettings();
        }
      } catch (err) {
        console.error("Error selecting OBS executable:", err);
      }
    });
  }

  const obsLaunchBtn = document.querySelector('#obs-launch-btn');
  if (obsLaunchBtn) {
    obsLaunchBtn.addEventListener('click', async () => {
      const label = obsLaunchBtn.textContent;
      obsLaunchBtn.disabled = true;
      obsLaunchBtn.textContent = STRINGS.CAPTURE_CONFIG.OBS_LAUNCHING;
      try {
        await launchObs();
      } catch (err) {
        showToast(STRINGS.CAPTURE_CONFIG.obsLaunchFailed(err), 'error');
      } finally {
        obsLaunchBtn.disabled = false;
        obsLaunchBtn.textContent = label;
      }
    });
  }

  const hlaeFfmpegLinkBtn = document.querySelector('#hlae-ffmpeg-link-btn');
  if (hlaeFfmpegLinkBtn) {
    hlaeFfmpegLinkBtn.addEventListener('click', async () => {
      const hlaePath = document.querySelector('#hlae-path-input')?.value?.trim() || "";
      // Whatever Render Studio was told to use, so both halves of the pipeline
      // encode with the same build. Empty means "system ffmpeg", which the
      // backend resolves to an absolute path — HLAE's ini cannot take a bare
      // command name.
      const ffmpegPath =
        document.querySelector('#ffmpeg-override-path-input')?.value?.trim() || "ffmpeg";
      hlaeFfmpegLinkBtn.disabled = true;
      try {
        let result = await linkHlaeFfmpeg(hlaePath, ffmpegPath);

        // HLAE can live anywhere — zip or installer — so a protected location
        // like Program Files is a real possibility rather than a rare one. Ask
        // before raising the UAC prompt, so the prompt is never a surprise, and
        // say what it is for.
        if (result?.needs_elevation) {
          const agreed = await confirm(
            STRINGS.CAPTURE_CONFIG.HLAE_FFMPEG_ELEVATE_PROMPT(result.ini),
            {
              title: STRINGS.CAPTURE_CONFIG.HLAE_FFMPEG_ELEVATE_TITLE,
              okLabel: STRINGS.CAPTURE_CONFIG.HLAE_FFMPEG_ELEVATE_CONFIRM
            }
          );
          // Say so rather than going quiet. Declining is a choice, but a button
          // that does nothing visible reads as a button that failed.
          if (!agreed) {
            showToast(STRINGS.CAPTURE_CONFIG.HLAE_FFMPEG_ELEVATE_REFUSED, 'info');
            return;
          }
          result = await linkHlaeFfmpeg(hlaePath, ffmpegPath, true);
        }

        if (result?.ini && !result.needs_elevation) {
          showToast(STRINGS.CAPTURE_CONFIG.HLAE_FFMPEG_LINKED_OK(result.ini), 'success');
        }
      } catch {
        // The bridge already toasted the reason, which for a refusal is the
        // point — an existing ini is reported, never replaced.
      } finally {
        hlaeFfmpegLinkBtn.disabled = false;
        await refreshHlaeFfmpegStatus();
      }
    });
  }

  const hlBrowseBtn = document.querySelector('#hl-browse-btn');
  if (hlBrowseBtn) {
    hlBrowseBtn.addEventListener('click', async () => {
      try {
        const selected = await open({
          multiple: false,
          filters: [{ name: STRINGS.MAIN.EXECUTABLE_FILTER_NAME, extensions: ['exe'] }],
          title: STRINGS.MAIN.SELECT_HL_EXE_TITLE
        });
        if (selected) {
          const path = Array.isArray(selected) ? selected[0] : selected;
          const inputEl = document.querySelector('#hl-path-input');
          if (inputEl) inputEl.value = path;
          await persistAppSettings();
          await refreshPathWarnings();
        }
      } catch (err) {
        console.error("Error selecting Half-Life executable:", err);
      }
    });
  }

  const ffmpegBrowseBtn = document.querySelector('#ffmpeg-browse-btn');
  if (ffmpegBrowseBtn) {
    ffmpegBrowseBtn.addEventListener('click', async () => {
      try {
        const selected = await open({
          multiple: false,
          filters: [{ name: STRINGS.MAIN.EXECUTABLE_FILTER_NAME, extensions: ['exe'] }],
          title: STRINGS.MAIN.SELECT_FFMPEG_EXE_TITLE
        });
        if (selected) {
          const path = Array.isArray(selected) ? selected[0] : selected;
          const inputEl = document.querySelector('#ffmpeg-override-path-input');
          if (inputEl) inputEl.value = path;
          await persistAppSettings();
          // Picking a different FFmpeg does not move what HLAE points at, which
          // is precisely why the row below has to be re-checked: that is how
          // the two end up on different builds without anything saying so.
          await refreshHlaeFfmpegStatus();
          await refreshPathWarnings();
        }
      } catch (err) {
        console.error("Error selecting FFmpeg executable:", err);
      }
    });
  }

  // ── Shared "a demo row was selected" handler ────────────────────────────────
  // Extracted so every renderMasterList call site behaves identically —
  // renderMasterList persists whichever callback it was last given
  // (master_pane.js's currentOnSelectDemo) as the fallback for re-renders
  // that don't pass one, so an inline callback that drifted from this one
  // would apply inconsistently depending on which code path rendered last.
  function selectDemoAndRenderDetail(demo, idx) {
    selectedDemoIdx = idx;
    renderDetailView(demo, selectedDemoIdx);
  }

  // ── Demo list footer helper ────────────────────────────────────────────────
  function updateDemoFooter(demos) {
    const footerEl = document.querySelector('#demo-list-footer');
    if (footerEl) {
      // Same recording-player filter the Highlights column uses (M2) — this
      // used to sum every player's streaks in the demo, not just the
      // recording player's, and could read in the hundreds where the
      // visible Highlights column summed to a fraction of that.
      const totalHighlights = (demos || []).reduce((sum, d) => sum + recordingPlayerStreaks(d).length, 0);
      footerEl.textContent = STRINGS.WORKSPACE.demoListFooter((demos || []).length, totalHighlights);
    }
    // Clear Untracked/Clear All only make sense with something in the
    // queue — Clear Selected already gates on its own checkbox state
    // (master_pane.js), this is the same idea for the other two.
    const isEmpty = !demos || demos.length === 0;
    const clearUntrackedBtnEl = document.querySelector('#clear-untracked-btn');
    if (clearUntrackedBtnEl) clearUntrackedBtnEl.disabled = isEmpty;
    const clearAllBtnEl = document.querySelector('#clear-all-btn');
    if (clearAllBtnEl) clearAllBtnEl.disabled = isEmpty;
  }

  // ── scan_progress event listener (registered once on load) ────────────────
  let unlistenScanProgress = null;
  listen('scan_progress', (event) => {
    const p = event.payload || {};
    const scanStatusEl = document.querySelector('#scan-status');
    const cancelScanBtn = document.querySelector('#cancel-scan-btn');
    const masterTableBody = document.querySelector('#master-demo-table-body');

    if (p.cancelled) {
      if (scanStatusEl) scanStatusEl.textContent = STRINGS.MAIN.cancelledStatus(p.found);
      if (cancelScanBtn) cancelScanBtn.disabled = true;
    } else if (p.status === 'Complete') {
      if (scanStatusEl) scanStatusEl.textContent = STRINGS.MAIN.readyFoundStatus(p.found);
      if (cancelScanBtn) cancelScanBtn.disabled = true;
    } else {
      if (scanStatusEl) scanStatusEl.textContent = STRINGS.MAIN.statusGeneric(p.status);
      if (cancelScanBtn) cancelScanBtn.disabled = false;
    }
  }).then(fn => { unlistenScanProgress = fn; });

  // ── Cancel Scan button ────────────────────────────────────────────────────
  const cancelScanBtn = document.querySelector('#cancel-scan-btn');
  if (cancelScanBtn) {
    cancelScanBtn.disabled = true;
    cancelScanBtn.addEventListener('click', async () => {
      cancelScanBtn.disabled = true;
      try {
        await cancelScan();
        showToast(STRINGS.MAIN.SCAN_CANCEL_REQUESTED_TOAST, 'info');
      } catch (_) { /* already toasted in ipc_bridge */ }
    });
  }

  // Scans only the given paths and merges the results into the existing
  // master list (replacing entries with matching `path`, appending new ones).
  // `scanPaths` itself is a separately-persisted "known library" list reused
  // by the capture batch payload (`capture_directories`) — it must NOT be
  // re-walked on every add, or every scan re-processes every folder ever
  // added across the app's lifetime (dev only ever re-ingests the paths just
  // picked in that action; see views/capture/workspace.rs Add Files/Add Folder).
  async function triggerAutoScan(pathsToScan) {
    if (!pathsToScan || pathsToScan.length === 0) return;

    const scanStatusEl = document.querySelector('#scan-status');
    const scanSpinnerEl = document.querySelector('#scan-spinner');
    const addFilesBtn = document.querySelector('#add-files-btn');
    const addFolderBtn = document.querySelector('#add-folder-btn');
    const cancelScanBtnInner = document.querySelector('#cancel-scan-btn');

    if (addFilesBtn) addFilesBtn.disabled = true;
    if (addFolderBtn) addFolderBtn.disabled = true;
    if (cancelScanBtnInner) cancelScanBtnInner.disabled = false;
    if (scanSpinnerEl) scanSpinnerEl.style.display = 'inline-block';
    if (scanStatusEl) scanStatusEl.textContent = STRINGS.MAIN.SCANNING_STATUS;
    showToast(STRINGS.MAIN.SCANNING_TOAST, 'info');

    const masterTableBody = document.querySelector('#master-demo-table-body');
    if (masterTableBody) masterTableBody.innerHTML = `<tr style="text-align:center"><td colspan="8">${STRINGS.MAIN.SCANNING_PLEASE_WAIT_ROW}</td></tr>`;

    try {
      const newlyScanned = await scanDirectory(pathsToScan);

      // Merge: replace any existing demo with the same path, append new ones.
      // (Prior behavior replaced the whole master list with the result of
      // re-scanning everything in `scanPaths`, which is what caused a single
      // "Add Demo Files" click to report hundreds of demos.)
      const indexByPath = new Map(currentScannedDemos.map((d, i) => [d.path, i]));
      newlyScanned.forEach((demo) => {
        const existingIdx = indexByPath.get(demo.path);
        if (existingIdx !== undefined) {
          // A re-scan produces brand new streak objects, so replacing
          // outright would wipe every status, selection, note and Kill
          // Range edit on this demo — carry that user-owned state across by
          // highlight uid instead.
          currentScannedDemos[existingIdx] = preserveHighlightState(currentScannedDemos[existingIdx], demo);
        } else {
          indexByPath.set(demo.path, currentScannedDemos.length);
          currentScannedDemos.push(demo);
        }
      });

      // footer is also updated on the Complete scan_progress event, but set
      // it here in case the event arrives before renderMasterList finishes.
      updateDemoFooter(currentScannedDemos);
      if (newlyScanned.length > 0) markProjectDirty();
      showToast(STRINGS.MAIN.scanCompleteToast(newlyScanned.length), 'success');
      selectedDemoIdx = newlyScanned.length > 0
        ? currentScannedDemos.indexOf(newlyScanned[0])
        : (currentScannedDemos.length > 0 ? 0 : null);
      // renderMasterList calls this with (null, null) whenever the queue is
      // empty (e.g. after Clear All) — selectDemoAndRenderDetail resets the
      // telemetry UI instead of dereferencing a demo that isn't there.
      renderMasterList(currentScannedDemos, selectedDemoIdx, selectDemoAndRenderDetail);
      if (selectedDemoIdx !== null) {
        selectDemoAndRenderDetail(currentScannedDemos[selectedDemoIdx], selectedDemoIdx);
      }
      // Reads 544 bytes per demo and one map file per distinct map, so it runs
      // after the scan rather than inside it. Not awaited: the queue is already
      // usable, and a demo whose map is missing is still worth listing.
      refreshMapWarnings(
        newlyScanned.map((d) => d.path),
        document.querySelector('#hl-path-input')?.value?.trim() || ''
      );
    } catch (err) {
      console.error("Error scanning directories:", err);
      showToast(STRINGS.MAIN.scanErrorToast(err), 'error');
      if (scanStatusEl) scanStatusEl.textContent = STRINGS.MAIN.scanErrorStatus(err);
    } finally {
      if (addFilesBtn) addFilesBtn.disabled = false;
      if (addFolderBtn) addFolderBtn.disabled = false;
      if (cancelScanBtnInner) cancelScanBtnInner.disabled = true;
      if (scanSpinnerEl) scanSpinnerEl.style.display = 'none';
    }
  }

  // Native Demo Files Ingestion (+ Add Demo Files)
  const addFilesBtn = document.querySelector('#add-files-btn');
  if (addFilesBtn) {
    addFilesBtn.addEventListener('click', async () => {
      try {
        const selected = await open({
          multiple: true,
          filters: [{ name: STRINGS.MAIN.DEMO_FILES_FILTER_NAME, extensions: ['dem'] }],
          title: STRINGS.MAIN.SELECT_DEMO_FILES_TITLE
        });
        if (selected) {
          const files = Array.isArray(selected) ? selected : [selected];
          files.forEach(f => {
            if (!scanPaths.includes(f)) {
              scanPaths.push(f);
              markProjectDirty();
            }
          });
          await persistAppSettings();
          // Scan only the files just picked, not the full accumulated
          // scanPaths history — see triggerAutoScan's doc comment.
          await triggerAutoScan(files);
        }
      } catch (err) {
        console.error("Error opening demo files dialog:", err);
      }
    });
  }

  // Native Demo Folder Ingestion (+ Add Folder)
  const addFolderBtn = document.querySelector('#add-folder-btn');
  if (addFolderBtn) {
    addFolderBtn.addEventListener('click', async () => {
      try {
        const selected = await open({
          directory: true,
          multiple: false,
          title: STRINGS.MAIN.SELECT_DEMO_FOLDER_TITLE
        });
        if (selected) {
          const folder = Array.isArray(selected) ? selected[0] : selected;
          if (!scanPaths.includes(folder)) {
            scanPaths.push(folder);
            markProjectDirty();
            await persistAppSettings();
            await triggerAutoScan([folder]);
          }
        }
      } catch (err) {
        console.error("Error opening demo directory dialog:", err);
      }
    });
  }

  async function updateExportPoolIndicator() {
    const indicator = document.querySelector('#export-pool-free-indicator');
    if (!indicator) return;
    if (targetDrives.length === 0) {
      indicator.textContent = STRINGS.MAIN.EXPORT_POOL_FREE_DEFAULT;
      return;
    }
    try {
      const bytes = await calculateExportPoolSpace(targetDrives);
      const gb = bytes / (1024 * 1024 * 1024);
      indicator.textContent = STRINGS.MAIN.exportPoolFree(gb.toFixed(1));
    } catch (err) {
      console.error("Error calculating export pool space:", err);
      indicator.textContent = STRINGS.MAIN.EXPORT_POOL_ERROR;
    }
  }

  // Target drives management
  document.querySelector('#add-drive-btn').addEventListener('click', () => {
    const driveEl = document.querySelector('#drive-path-input');
    const drivePath = driveEl.value.trim();
    if (drivePath && driveOverridesEditor.addItem(drivePath)) {
      driveEl.value = "";
    }
  });

  const browseDriveBtn = document.querySelector('#browse-drive-btn');
  if (browseDriveBtn) {
    browseDriveBtn.addEventListener('click', async () => {
      const selected = await pickTargetDrive();
      if (selected) driveOverridesEditor.addItem(selected);
    });
  }

  // JIT multi-drive export pool for Render Studio
  const addRenderExportDirBtn = document.querySelector('#add-render-export-dir-btn');
  if (addRenderExportDirBtn) {
    addRenderExportDirBtn.addEventListener('click', () => {
      const inputEl = document.querySelector('#render-export-dir-input');
      const path = inputEl?.value?.trim();
      if (path && renderExportDirsEditor.addItem(path) && inputEl) {
        inputEl.value = '';
      }
    });
  }

  const browseRenderExportBtn = document.querySelector('#browse-render-export-btn');
  if (browseRenderExportBtn) {
    browseRenderExportBtn.addEventListener('click', async () => {
      const selected = await pickRenderExportDir();
      if (selected) renderExportDirsEditor.addItem(selected);
    });
  }

  // Export Configuration tab switching
  const tabBtns = document.querySelectorAll('.config-tab-btn');
  tabBtns.forEach(btn => {
    btn.addEventListener('click', () => {
      tabBtns.forEach(b => b.classList.remove('active'));
      document.querySelectorAll('.config-tab-content').forEach(content => {
        content.style.display = 'none';
      });
      btn.classList.add('active');
      const targetId = btn.getAttribute('data-tab');
      const targetEl = document.getElementById(targetId);
      if (targetEl) targetEl.style.display = 'block';
    });
  });

  // Top nav bar view routing (shared with detail_pane.js — see nav.js)
  switchNavTab('workspace');

  const navTabBtns = document.querySelectorAll('.nav-tab-btn');
  navTabBtns.forEach(btn => {
    btn.addEventListener('click', () => switchNavTab(btn.getAttribute('data-nav')));
  });

  // Capture Studio in-workflow phase switch (Highlights <-> Configuration) —
  // replaces the old "Batch Capture Config" top-level nav tab, see nav.js.
  document.querySelectorAll('.capture-detail-subtab-btn').forEach((btn) => {
    btn.addEventListener('click', () => setCaptureDetailSubtab(btn.dataset.captureSubtab));
  });

  // Initialize Capture Batch UI
  // Shared by both auto-status paths (a verified capture, a finished render):
  // both tables read status and neither observes the streak objects on its
  // own, so re-render both — the Master Queue for its Pending/Captured/
  // Rendered counts, and the detail view for the per-row status dropdowns.
  const onHighlightStatusChange = () => {
    markProjectDirty();
    renderMasterList(currentScannedDemos, selectedDemoIdx);
    if (selectedDemoIdx !== null && currentScannedDemos[selectedDemoIdx]) {
      renderDetailView(currentScannedDemos[selectedDemoIdx], selectedDemoIdx);
    }
  };

  initCaptureUI(() => ({
    scanPaths,
    targetDrives,
    currentScannedDemos
  }), persistAppSettings, onHighlightStatusChange, () => takeIndex, updateExportPoolIndicator);

  // Initialize Render Studio UI. First arg doubles as Render's scan-input
  // locations — see the driveOverridesEditor/targetDrives comment above.
  initRenderUI(() => targetDrives, () => renderExportDirs, persistAppSettings, {
    getTakeIndex: () => takeIndex,
    getAllDemos: () => currentScannedDemos,
    onStatusChange: onHighlightStatusChange
  });

  // Render-batch crash-recovery prompt — checked once on startup, same
  // pattern as dev's StartupState::PendingRenderRecovery. Render is now a
  // subtab of the single 'workspace' nav destination, not its own navKey.
  checkRenderRecoveryOnStartup(() => {
    switchNavTab('workspace');
    setCaptureDetailSubtab('render');
  });

  checkObsOrphanOnStartup();

  // Flush any not-yet-persisted settings edit before the window actually
  // closes. list_editor.js (Init/Custom Commands, numeric fields) only
  // writes to disk on 'change' (blur/Enter), not every keystroke — closing
  // the app while a field still has focus (never blurred) would otherwise
  // silently drop that edit even though it's already reflected in the
  // in-memory state persistAppSettings() reads from. Confirmed as a real,
  // reproducible data-loss case 2026-08-23 (see engineering_backlog.md).
  const appWindow = getCurrentWindow();
  appWindow.onCloseRequested(async (event) => {
    event.preventDefault();
    // Capture Studio project state (scanned demos, takeIndex, scanPaths)
    // changed since the last save — offer to save, discard, or cancel the
    // close before losing it. See markProjectDirty() call sites above.
    // Gated on hasSavableProject() too, matching saveProjectSession()'s own
    // guard — without this, clearing a *fresh, never-saved* queue to empty
    // then closing would show the prompt but "Save & Close" would just hit
    // the "Nothing to save" toast and leave the modal stuck open. Emptying
    // a queue that *did* come from a loaded session is still real, savable
    // work (writes the now-empty project back), so that case still prompts.
    if (hasUnsavedChanges && hasSavableProject()) {
      const outcome = await requestUnsavedChangesConfirmation();
      if (!outcome) return; // Cancel — leave the window open
      // 'save' already wrote the file inside the modal's Save button
      // handler; 'discard' falls through to close as-is either way.
    }
    await persistAppSettings();
    await appWindow.destroy();
  });

  // Page-level reload (F5, Ctrl+R, or any other in-place navigation) doesn't
  // go through Tauri's onCloseRequested above at all — it's a WebView2
  // navigation, not a window close — so it needs its own guard. Unlike the
  // themed modal above, beforeunload's confirmation dialog is browser-native
  // and cannot be styled or given custom button text (a deliberate web
  // platform restriction against sites faking dialogs); setting returnValue
  // is what triggers it, and its own text is what's shown, not this string.
  window.addEventListener('beforeunload', (event) => {
    if (hasUnsavedChanges && hasSavableProject()) {
      event.preventDefault();
      event.returnValue = '';
    }
  });

  // Delete callback: remove a demo from the active scan list and re-render.
  // Called by master_pane.js when the 🗑 button is clicked on a row. Returns
  // the new selectedDemoIdx so the caller's own renderMasterList call can
  // pass it through for the row-highlight — master_pane.js doesn't otherwise
  // know this file's selectedDemoIdx, and previously always re-rendered with
  // no selection at all (visually dropping the highlight) even when the
  // deleted row wasn't the selected one and the selection should have held.
  const onDeleteDemo = (deletedOriginalIdx, updatedDemos) => {
    currentScannedDemos = updatedDemos;
    markProjectDirty();
    // If the deleted demo was the selected one, clear the detail view.
    if (selectedDemoIdx === deletedOriginalIdx) {
      selectedDemoIdx = currentScannedDemos.length > 0 ? 0 : null;
      if (selectedDemoIdx !== null) {
        renderDetailView(currentScannedDemos[0], selectedDemoIdx);
      } else {
        renderDetailView(null, null);
      }
    } else if (selectedDemoIdx !== null && selectedDemoIdx > deletedOriginalIdx) {
      // Shift selection index down if a demo above it was removed.
      selectedDemoIdx -= 1;
    }
    updateDemoFooter(currentScannedDemos);
    return selectedDemoIdx;
  };

  // Shared by all three Clear actions below: swaps the scanned-demo list,
  // resets checkboxes, and re-renders both the queue and the detail view.
  //
  // Selection is preserved by object identity, not just reset to row 0:
  // Clear Untracked/Selected/All can all leave the previously-selected demo
  // still in the queue (it just wasn't the one removed), and jumping the
  // selection to whatever's now first is jarring — you were looking at one
  // demo's highlights and suddenly a different one's are shown. Only falls
  // back to row 0 (or nothing) when the selected demo was actually the one
  // that got removed.
  function replaceScannedDemos(newDemos) {
    const previouslySelectedDemo = selectedDemoIdx !== null ? currentScannedDemos[selectedDemoIdx] : null;
    currentScannedDemos = newDemos;
    markProjectDirty();
    const preservedIdx = previouslySelectedDemo ? currentScannedDemos.indexOf(previouslySelectedDemo) : -1;
    selectedDemoIdx = preservedIdx !== -1 ? preservedIdx : (currentScannedDemos.length > 0 ? 0 : null);
    clearCheckedPaths();
    updateDemoFooter(currentScannedDemos);
    renderMasterList(currentScannedDemos, selectedDemoIdx);
    renderDetailView(selectedDemoIdx !== null ? currentScannedDemos[selectedDemoIdx] : null, selectedDemoIdx);
    // The banner is about demos in the queue. With the queue empty there is
    // nothing left for it to be about.
    if (currentScannedDemos.length === 0) resetMapWarnings();
  }

  // One-line callout appended to Clear actions' toasts/summaries whenever an
  // active search filter narrowed what got acted on, so "Clear All" (etc.)
  // doesn't silently do less than its name implies without the user noticing.
  function filterScopeNote(visibleCount, totalCount) {
    return visibleCount < totalCount
      ? STRINGS.MAIN.filterScopeNote(visibleCount, totalCount)
      : '';
  }

  // Clear Untracked — removes only demos with no tracked work (isDemoTracked).
  // Scoped to the currently search-filtered demos, matching the select-all
  // checkbox — a demo hidden by the search box is left untouched no matter
  // its status.
  const clearUntrackedBtn = document.querySelector('#clear-untracked-btn');
  if (clearUntrackedBtn) {
    clearUntrackedBtn.addEventListener('click', () => {
      if (currentScannedDemos.length === 0) {
        showToast(STRINGS.MAIN.QUEUE_ALREADY_EMPTY, 'info');
        return;
      }
      const visible = getVisibleDemos();
      if (visible.length === 0) {
        showToast(STRINGS.MAIN.NO_DEMOS_MATCH_SEARCH, 'info');
        return;
      }
      const trackedVisibleCount = visible.filter(isDemoTracked).length;
      const untrackedVisible = new Set(visible.filter((d) => !isDemoTracked(d)).map((d) => d.path));
      if (untrackedVisible.size === 0) {
        showToast(STRINGS.MAIN.NOTHING_TRACKED_TO_CLEAR, 'info');
        return;
      }
      const totalCount = currentScannedDemos.length;
      const removedNames = currentScannedDemos.filter((d) => untrackedVisible.has(d.path)).map((d) => d.name || d.path);
      replaceScannedDemos(currentScannedDemos.filter((d) => !untrackedVisible.has(d.path)));
      // "Kept N with tracked work" only ever refers to visible demos that
      // were actually evaluated and found tracked — never demos hidden by
      // the search filter, which weren't touched for a completely different
      // reason and would otherwise get mislabeled as "kept ... tracked".
      const keptNote = trackedVisibleCount > 0 ? STRINGS.MAIN.keptWithTrackedWork(trackedVisibleCount) : '';
      const scopeNote = filterScopeNote(visible.length, totalCount);
      showToast(
        STRINGS.MAIN.removedUntrackedToast(untrackedVisible.size, keptNote, scopeNote),
        'success'
      );
      logFrontendEvent(STRINGS.MAIN.clearUntrackedLog(untrackedVisible.size, keptNote, scopeNote, removedNames.join(', ')));
    });
  }

  // Shared "tracked work at risk" confirmation modal — a pure yes/no
  // primitive (with an optional Save-First detour), awaited by every caller
  // that needs to warn before removing tracked demos: Clear Selected, Clear
  // All, and the row-level delete button (master_pane.js, via
  // requestTrackedDeleteConfirm below). It does NOT perform the removal
  // itself — Clear All/Selected replace the whole scanned-demo list, while
  // the row delete button needs to preserve its own selection-shift logic
  // instead, so "how to remove" stays with each caller; the modal only
  // answers "should we." Resolves `false` on Cancel, `'confirm'` on Confirm,
  // `'save-first'` once a save actually succeeded — the last two are both
  // truthy but let callers word their success toast accordingly.
  let pendingConfirmResolve = null;
  const clearAllModal = document.querySelector('#clear-all-modal');

  function requestTrackedClearConfirmation(targets, { title, verb, filterNote, confirmLabel }) {
    const trackedCount = targets.filter(isDemoTracked).length;
    const plural = targets.length === 1 ? STRINGS.MAIN.DEMO_SINGULAR : STRINGS.MAIN.DEMO_PLURAL;
    const titleEl = document.querySelector('#clear-all-title');
    if (titleEl) titleEl.textContent = title;
    const confirmBtnEl = document.querySelector('#clear-all-confirm-btn');
    if (confirmBtnEl) confirmBtnEl.textContent = confirmLabel || STRINGS.MAIN.CLEAR_ANYWAY_DEFAULT;
    const summaryEl = document.querySelector('#clear-all-summary');
    if (summaryEl) {
      summaryEl.textContent = (trackedCount > 0
        ? STRINGS.MAIN.clearSummaryTracked(verb, targets.length, plural, trackedCount)
        : STRINGS.MAIN.clearSummaryUntracked(verb, targets.length, plural)
      ) + (filterNote || '');
    }
    if (clearAllModal) clearAllModal.style.display = 'flex';
    return new Promise(resolve => { pendingConfirmResolve = resolve; });
  }

  if (clearAllModal) {
    document.querySelector('#clear-all-cancel-btn')?.addEventListener('click', () => {
      clearAllModal.style.display = 'none';
      pendingConfirmResolve?.(false);
      pendingConfirmResolve = null;
    });
    document.querySelector('#clear-all-confirm-btn')?.addEventListener('click', () => {
      clearAllModal.style.display = 'none';
      pendingConfirmResolve?.('confirm');
      pendingConfirmResolve = null;
    });
    document.querySelector('#clear-all-save-first-btn')?.addEventListener('click', async () => {
      // Saves the whole current queue (not just whatever's about to be
      // removed) — the point is that everything at risk is still
      // recoverable from the saved file afterward, whether this is a
      // single tracked delete, Clear Selected, or Clear All.
      const saved = await saveProjectSession();
      if (!saved) return; // leave the modal open — nothing was lost yet
      clearAllModal.style.display = 'none';
      pendingConfirmResolve?.('save-first');
      pendingConfirmResolve = null;
    });
  }

  // Unsaved-changes prompt, shown by the window close handler above whenever
  // hasUnsavedChanges is set. Same Promise-resolution shape as the modal
  // above, but its own two-way branch ('save'/'discard') since closing is a
  // binary "keep the work or don't" rather than a "confirm a removal."
  let pendingUnsavedChangesResolve = null;
  const unsavedChangesModal = document.querySelector('#unsaved-changes-modal');

  function requestUnsavedChangesConfirmation() {
    if (unsavedChangesModal) unsavedChangesModal.style.display = 'flex';
    return new Promise(resolve => { pendingUnsavedChangesResolve = resolve; });
  }

  if (unsavedChangesModal) {
    document.querySelector('#unsaved-changes-cancel-btn')?.addEventListener('click', () => {
      unsavedChangesModal.style.display = 'none';
      pendingUnsavedChangesResolve?.(false);
      pendingUnsavedChangesResolve = null;
    });
    document.querySelector('#unsaved-changes-discard-btn')?.addEventListener('click', () => {
      unsavedChangesModal.style.display = 'none';
      pendingUnsavedChangesResolve?.('discard');
      pendingUnsavedChangesResolve = null;
    });
    document.querySelector('#unsaved-changes-save-btn')?.addEventListener('click', async () => {
      const saved = await saveProjectSession();
      if (!saved) return; // Save-As cancelled/failed — leave the modal open
      unsavedChangesModal.style.display = 'none';
      pendingUnsavedChangesResolve?.('save');
      pendingUnsavedChangesResolve = null;
    });
  }

  // Clear Selected — removes checked rows regardless of status, same in
  // both modes (the user explicitly checked them). Whenever any checked
  // demo is tracked, escalate from a plain confirm() to the shared modal.
  // Scoped to visible rows too: a row checked, then hidden by a later
  // search, is left in the queue AND stays checked — the action never saw
  // it, so it shouldn't lose that selection just because clearCheckedPaths()
  // (inside replaceScannedDemos) resets everything by default.
  const clearSelectedBtn = document.querySelector('#clear-selected-btn');
  if (clearSelectedBtn) {
    clearSelectedBtn.addEventListener('click', async () => {
      const checkedPaths = new Set(getCheckedDemoPaths());
      if (checkedPaths.size === 0) {
        showToast(STRINGS.MAIN.NO_DEMOS_SELECTED, 'info');
        return;
      }
      const visiblePaths = new Set(getVisibleDemos().map((d) => d.path));
      const targets = currentScannedDemos.filter(d => checkedPaths.has(d.path) && visiblePaths.has(d.path));
      if (targets.length === 0) {
        showToast(STRINGS.MAIN.allSelectedHiddenToast(checkedPaths.size), 'info');
        return;
      }
      const hiddenCheckedCount = checkedPaths.size - targets.length;
      const hiddenNote = hiddenCheckedCount > 0
        ? STRINGS.MAIN.hiddenCheckedNote(hiddenCheckedCount)
        : '';
      let savedFirst = false;
      if (targets.some(isDemoTracked)) {
        const outcome = await requestTrackedClearConfirmation(targets, { title: STRINGS.MAIN.CLEAR_SELECTED_TITLE, verb: STRINGS.MAIN.VERB_REMOVES, filterNote: hiddenNote, confirmLabel: STRINGS.MAIN.CLEAR_SELECTED_ANYWAY });
        if (!outcome) return;
        savedFirst = outcome === 'save-first';
      } else if (!(await themedConfirm(STRINGS.MAIN.removeSelectedConfirm(targets.length, hiddenNote), { title: STRINGS.MAIN.CLEAR_SELECTED_TITLE }))) {
        return;
      }
      const removePaths = new Set(targets.map((d) => d.path));
      const removedNames = targets.map((d) => d.name || d.path);
      // Preserve checkboxes on rows the action never touched (checked, but
      // hidden by the search filter) — captured before replaceScannedDemos
      // wipes the whole checked set via clearCheckedPaths().
      const survivingHiddenChecked = Array.from(checkedPaths).filter((p) => !removePaths.has(p));
      replaceScannedDemos(currentScannedDemos.filter(d => !removePaths.has(d.path)));
      if (survivingHiddenChecked.length > 0) setCheckedDemoPaths(survivingHiddenChecked);
      showToast(STRINGS.MAIN.removedSelectedToast(savedFirst, targets.length, hiddenNote), 'success');
      logFrontendEvent(STRINGS.MAIN.clearSelectedLog(targets.length, savedFirst ? STRINGS.MAIN.SAVED_SESSION_FIRST_NOTE : '', hiddenNote, removedNames.join(', ')));
    });
  }

  // Clear All — escalates to the shared modal (enumerating what would be
  // lost, offering to save first) whenever something tracked is actually at
  // risk, same threshold as Clear Selected/row delete. Also scoped to the
  // search filter, same as the other two Clear actions — "All" means "all
  // visible," with an explicit callout whenever that's fewer than the full
  // queue, so it never silently does less than its name implies.
  const clearAllBtn = document.querySelector('#clear-all-btn');
  if (clearAllBtn) {
    clearAllBtn.addEventListener('click', async () => {
      if (currentScannedDemos.length === 0) {
        showToast(STRINGS.MAIN.QUEUE_ALREADY_EMPTY, 'info');
        return;
      }
      const targets = getVisibleDemos();
      if (targets.length === 0) {
        showToast(STRINGS.MAIN.NO_DEMOS_MATCH_SEARCH, 'info');
        return;
      }
      const note = filterScopeNote(targets.length, currentScannedDemos.length);
      let savedFirst = false;
      if (targets.some(isDemoTracked)) {
        const outcome = await requestTrackedClearConfirmation(targets, { title: STRINGS.MAIN.CLEAR_ALL_TITLE, verb: STRINGS.MAIN.VERB_REMOVES, filterNote: note, confirmLabel: STRINGS.MAIN.CLEAR_ALL_ANYWAY });
        if (!outcome) return;
        savedFirst = outcome === 'save-first';
      } else if (!(await themedConfirm(STRINGS.MAIN.removeAllConfirm(targets.length, note), { title: STRINGS.MAIN.CLEAR_ALL_TITLE }))) {
        return;
      }
      const removePaths = new Set(targets.map((d) => d.path));
      const removedNames = targets.map((d) => d.name || d.path);
      replaceScannedDemos(currentScannedDemos.filter((d) => !removePaths.has(d.path)));
      showToast(STRINGS.MAIN.clearedAllToast(savedFirst, targets.length, note), 'success');
      logFrontendEvent(STRINGS.MAIN.clearAllLog(targets.length, savedFirst ? STRINGS.MAIN.SAVED_SESSION_FIRST_NOTE : '', note, removedNames.join(', ')));
    });
  }

  // Single-row tracked delete (master_pane.js's 🗑 button) reuses the same
  // modal via this thin wrapper, so a tracked demo gets the exact same
  // Save-First affordance as Clear Selected/All instead of a lesser plain
  // confirm() just because it's one row. Returns whether the caller should
  // proceed — master_pane.js still owns the actual splice + selection-shift
  // logic, since that's specific to a single-row delete.
  async function requestTrackedDeleteConfirm(demo) {
    const outcome = await requestTrackedClearConfirmation([demo], { title: STRINGS.MAIN.REMOVE_TRACKED_DEMO_TITLE, verb: STRINGS.MAIN.VERB_REMOVES, confirmLabel: STRINGS.MAIN.REMOVE_ANYWAY });
    return !!outcome;
  }

  initMasterPane(onDeleteDemo, requestTrackedDeleteConfirm);
  // Read at click time, not captured: the hl.exe path can be set after a scan
  // has already run and left the banner up.
  initMapWarnings(() => document.querySelector('#hl-path-input')?.value?.trim() || '');
  // capture_pane.js owns the Scheduled Command list, so the floor check reads
  // it from there rather than keeping a second copy.
  initRollFloors(() => getCommandsState().custom_commands);
  // Scanned once the persisted hl.exe path is in the DOM, and again whenever it
  // changes — a config file the app cannot see is exactly what this warns about.
  const hlPathInput = document.querySelector('#hl-path-input');
  if (hlPathInput) {
    refreshInitCommandWarnings();
    hlPathInput.addEventListener('change', () => refreshInitCommandWarnings());
  }
  initDetailPane(() => currentScannedDemos, () => {
    // Fired on every detail-pane re-render, not just edits (also runs when
    // switching the selected demo, or after a capture/render completes) —
    // selection moves required capture bytes (refreshLaunchGuard) and status
    // moves the Master Queue's Highlights/Pending/Captured/Rendered columns
    // (renderMasterList), both cheap enough to just always re-derive here.
    // Must NOT mark the project dirty — see onDirty below for that.
    refreshLaunchGuard({ targetDrives, currentScannedDemos });
    renderMasterList(currentScannedDemos, selectedDemoIdx);
  }, () => {
    // Fired only from an actual highlights-table field edit (selection,
    // kill range, status, notes) — all of it is part of the `demos` written
    // by saveProjectSession(), so all of it marks the project dirty.
    markProjectDirty();
  });

  // Demo Analyzer's Explorer sidebar Pinned tier shares the same
  // pinned_folders/scanPaths state as Capture Studio (matches dev's real
  // design — its own pinned_folders field is app-wide, not analyzer-scoped).
  // Pinning/unpinning here doesn't trigger Capture Studio's heavier
  // highlight-scan pipeline, just persists the path.
  async function pinAnalyzerFolder(folder) {
    if (!scanPaths.includes(folder)) {
      scanPaths.push(folder);
      await persistAppSettings();
    }
  }
  async function unpinAnalyzerFolder(folder) {
    scanPaths = scanPaths.filter((f) => f !== folder);
    await persistAppSettings();
  }
  // Recent tier: most-recent-first, capped at 10, deduped — mirrors dev's
  // `demo_folder_history` push-front/truncate logic exactly.
  async function recordDemoFolderVisit(folder) {
    demoFolderHistory = demoFolderHistory.filter((f) => f !== folder);
    demoFolderHistory.unshift(folder);
    if (demoFolderHistory.length > 10) demoFolderHistory.length = 10;
    await persistAppSettings();
  }
  // A pinned/recent folder that no longer exists on disk is silently
  // dropped from history when clicked, matching dev's Quick Links behavior.
  async function forgetDemoFolderVisit(folder) {
    if (demoFolderHistory.includes(folder)) {
      demoFolderHistory = demoFolderHistory.filter((f) => f !== folder);
      await persistAppSettings();
    }
  }
  async function setScanFoldersForDemos(enabled) {
    scanFoldersForDemos = enabled;
    await persistAppSettings();
  }
  async function setAnalyzerExplorerWidth(px) {
    analyzerExplorerWidth = px;
    await persistAppSettings();
  }
  initAnalyzerPane({
    getPinnedFolders: () => scanPaths,
    pinFolder: pinAnalyzerFolder,
    unpinFolder: unpinAnalyzerFolder,
    getDemoFolderHistory: () => demoFolderHistory,
    recordDemoFolderVisit,
    forgetDemoFolderVisit,
    getScanFoldersForDemos: () => scanFoldersForDemos,
    setScanFoldersForDemos,
    getAnalyzerExplorerWidth: () => analyzerExplorerWidth,
    setAnalyzerExplorerWidth,
  });

  // Context-Aware Shortcut Dispatcher
  window.addEventListener('keydown', (e) => {
    const isCtrlO = (e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'o';
    const isCtrlS = (e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 's';
    const isCtrlN = (e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'n';
    const isCtrlW = (e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'w';
    // WebView2 keeps the browser's reload shortcuts live by default — a
    // desktop app has no "refresh the page" affordance at all, so these are
    // swallowed unconditionally rather than routed through the dirty-state
    // check the beforeunload listener below does for any other reload path
    // (e.g. devtools once opened — the context menu's own Reload entry is
    // gone entirely, see the contextmenu listener further down).
    const isReload = e.key === 'F5' || ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'r');

    if (isCtrlO || isCtrlS || isCtrlN || isCtrlW || isReload) {
      e.preventDefault();
    }

    // New/Save/Load Session work from any tab (#122) — previously gated to
    // `activeTab === 'workspace'` because the buttons themselves only
    // existed in the Studio nav-actions area; now that they live in the
    // always-visible File menu, the shortcuts aren't tab-scoped either.
    if (isCtrlN) document.querySelector('#new-session-btn')?.click();
    if (isCtrlO) document.querySelector('#load-project-btn')?.click();
    if (isCtrlS) document.querySelector('#save-project-btn')?.click();
  });

  // WebView2's native right-click menu (Reload/Inspect/browser Cut-Copy-
  // Paste) reads as a web page, not an app — suppressed entirely. Doesn't
  // affect actual clipboard functionality: Ctrl+C/X/V are OS-level keyboard
  // bindings that don't route through this menu, so text fields keep normal
  // copy/paste with no menu at all.
  window.addEventListener('contextmenu', (e) => e.preventDefault());
});