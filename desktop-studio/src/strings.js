// strings.js
//
// Single centralized source for every user-facing English string in the
// desktop-studio frontend. NOT an i18n system — no language switching, no
// key-based lookup abstraction beyond plain named constants/functions. The
// point is purely "one place to find/edit any UI string."
//
// Grouped by pane/feature area for readability. Static strings are plain
// values; strings built from a template/interpolation are functions that
// return the built string (keeps the interpolation logic here, not
// scattered across the pane files).
//
// Rust backend error strings (src-tauri/) and console.log/console.error
// developer diagnostics are explicitly OUT OF SCOPE and are not represented
// here — only text actually shown to the end user.

export const STRINGS = {
  // ── Top Navigation / Header ─────────────────────────────────────────────
  NAV: {
    APP_TITLE: 'DoD Tools Studio',
    // OS window title (taskbar/Alt-Tab) — set by updater_pane.js once the
    // running build's version is known. baseVersion excludes the
    // experimental channel's `-<run number>` suffix — the title just needs
    // "what kind of build is this", not which exact run. buildKind: 'local'
    // (npm run tauri dev), 'debug' (tauri build --debug — a real bundle,
    // just not a release-profile one), 'experimental'
    // (release_experimental.yml), or anything else for a real stable build
    // (no parenthetical).
    appWindowTitle: (baseVersion, buildKind) => {
      const tags = { local: 'local build', debug: 'debug build', experimental: 'experimental build' };
      const tag = tags[buildKind];
      return `DoD Tools Studio — v${baseVersion}${tag ? ` (${tag})` : ''}`;
    },
    STUDIO_TAB: 'Studio',
    DEMO_AUDITOR_TAB: 'Demo Auditor',
    DEMO_ANALYZER_TAB: 'Demo Analyzer',
    NO_SESSION_LOADED: 'No session loaded',
    FILE_MENU: 'File',
    NEW_SESSION_BUTTON: 'New Session',
    NEW_SESSION_TITLE: 'Start a new, empty session',
    HELP_MENU: 'Help',
    ABOUT_MENU: 'About',
    SAVE_SESSION_BUTTON: 'Save Session',
    SAVE_SESSION_TITLE: 'Save Project Session',
    LOAD_SESSION_BUTTON: 'Load Session',
    LOAD_SESSION_TITLE: 'Load Project Session',
  },

  // ── Workspace Pane: Directory Scan & Master Demo Queue ──────────────────
  WORKSPACE: {
    SCAN_PANEL_TITLE: 'Directory Scan & Management',
    ADD_FILES_BUTTON: '+ Add Demo Files',
    ADD_FOLDER_BUTTON: '+ Add Folder',
    CANCEL_SCAN_BUTTON: 'Cancel Scan',
    CANCEL_SCAN_TITLE: 'Cancel the running directory scan',
    SCAN_STATUS_READY: 'Status: Ready',
    MASTER_QUEUE_TITLE: 'Master Demo Queue',
    SEARCH_PLACEHOLDER: 'Search filename or map...',
    CLEAR_UNTRACKED_BUTTON: 'Clear Untracked',
    CLEAR_UNTRACKED_TITLE: 'Remove demos with no Captured/Rendered status, notes, or edited kill range. Tracked demos are kept. Only affects demos matching the current search.',
    CLEAR_SELECTED_BUTTON: 'Clear Selected',
    CLEAR_SELECTED_TITLE_DEFAULT: 'Check one or more rows first.',
    CLEAR_ALL_BUTTON: 'Clear All',
    CLEAR_ALL_TITLE_TOOLTIP: 'Remove every demo matching the current search — the whole queue if search is empty.',
    SELECT_ALL_CB_TITLE: 'Select/deselect all visible demos',
    TABLE_HEADER_DEMO_FILE: 'Demo File',
    TABLE_HEADER_HIGHLIGHTS: 'Highlights',
    TABLE_HEADER_SELECTED: 'Selected',
    TABLE_HEADER_PENDING: 'Pending',
    TABLE_HEADER_CAPTURED: 'Captured',
    TABLE_HEADER_RENDERED: 'Rendered',
    TABLE_HEADER_ACTIONS: 'Actions',
    TABLE_EMPTY_NO_DEMOS: "No demos scanned yet. Use '+ Add Demo Files' or '+ Add Folder' to get started.",
    TABLE_EMPTY_NO_DEMOS_IN_DIRS: 'No demos found in specified directories.',
    TABLE_EMPTY_NO_MATCH_SEARCH: 'No demos match your search.',
    DEMO_LIST_FOOTER_DEFAULT: 'Loaded Demos: 0 | Total Highlights: 0',
    demoListFooter: (loaded, highlights) => `Loaded Demos: ${loaded} | Total Highlights: ${highlights}`,
    REMOVE_DEMO_TITLE: 'Remove demo from queue',
    removeDemoConfirm: (name) => `Remove "${name}" from the queue? It has tracked work (Captured/Rendered status, a note, or an edited kill range) that will be lost.`,
    trackedBadgeTooltip: (reasons) => `Tracked — has ${reasons.join(', ')}. Protected from Clear Untracked in Workspace mode.`,
    rowDeleteLog: (name, trackedNote) => `[queue] Row delete: removed "${name}"${trackedNote}`,
    TRACKED_NOTE_SUFFIX: ' (had tracked work; user confirmed)',
    REASON_STATUS: 'a Pending/Captured/Rendered status',
    REASON_NOTE: 'a note',
    REASON_RANGE: 'an edited kill range',
    EMPTY_DASH: '—',
  },

  // ── Highlight Details (detail_pane.js) + Advanced Diagnostics ───────────
  HIGHLIGHTS: {
    // Placeholder naming — "Capture"/"Render" is a stand-in until better
    // names are picked (per #81 discussion), not a final decision.
    SUBTAB_HIGHLIGHTS: 'Capture',
    SUBTAB_RENDER: 'Render',
    SUBTAB_CONFIGURATION: 'Configuration',
    DEFAULT_TITLE: 'Highlight Details',
    detailTitle: (name) => `Highlight Details: ${name}`,
    LAUNCH_PREVIEW_BUTTON: 'Launch Preview',
    LAUNCH_PREVIEW_TITLE: 'Patches this demo (or reuses an existing preview) and launches it in HLAE via viewdemo.',
    LAUNCHING: 'Launching…',
    VIEW_TELEMETRY_BUTTON: 'View Match Telemetry',
    SELECT_ALL_CB_TITLE: 'Select/deselect all visible highlights',
    GENERATE_ALL_PREVIEWS_BUTTON: 'Generate All Previews',
    GENERATE_ALL_PREVIEWS_TITLE: 'Generates _preview.dem files with a BOOKMARK event for every highlight across all demos, skipping any demo that already has one.',
    GENERATING: 'Generating…',
    LAUNCH_STANDALONE_BUTTON: 'Launch Game (HLAE)',
    LAUNCH_STANDALONE_TITLE: 'Boots HLAE against hl.exe directly with no demo loaded.',
    MIN_KILLS_LABEL: 'Min Kills:',
    EMPTY_SELECT_DEMO: 'Select a demo in the Master List to view its highlights.',
    EMPTY_NO_STREAKS: 'No highlights detected in this demo.',
    ADVANCED_DIAGNOSTICS_SUMMARY: 'Advanced Diagnostics (Canvas Timeline & Telemetry)',
    TIMELINE_NO_DATA: 'No highlight timeline available',
    COL_ROW_NUM: 'Row #',
    COL_SEL: 'Sel',
    COL_KILL_RANGE: 'Kill Range',
    COL_KILLS: 'Kills',
    COL_TIME: 'Time',
    COL_DUR: 'Dur.',
    COL_STATUS: 'Status',
    COL_NOTES: 'Notes',
    COL_DETAILS: 'Details',
    NOTES_PLACEHOLDER: 'Add note...',
    KR_RESET_TITLE: 'Reset to full range',
    fallbackKillCount: (count) => `${count} kills`,
    STATUS_OPTIONS: ['None', 'Pending', 'Captured', 'Rendered'],
    // What an untouched highlight (streak.status still undefined) displays
    // and counts as. 'Pending' is now a deliberate, user-set flag ("capture
    // this one later") rather than the implicit default every unset row
    // used to show — see isHighlightTracked's doc comment (take_index.js).
    STATUS_UNSET_DEFAULT: 'None',
    mergedTakeBadge: (takeName) => `merged → ${takeName}`,
    mergedBadgeTitle: (mergedCount) => `Merged with ${mergedCount - 1} other highlight(s) into one take — they were recorded together and share this take folder.`,
    tickLabel: (tick) => `Tick ${tick}`,
    secondsSuffix: (n) => `${n}s`,
    HLAE_PATH_REQUIRED: 'Configure the HLAE and Half-Life executable paths in Batch Capture Config before previewing.',
    PREVIEW_LAUNCHING_TOAST: 'Preview launching in HLAE...',
    generatedPreviews: (count) => count === 0
      ? 'Every demo already had a preview — nothing new to generate.'
      : `Generated ${count} preview demo(s). Load them manually via HLAE.`,
    copiedViewCommand: (cmd) => `Copied "${cmd}" to clipboard.`,
    COPY_VIEW_COMMAND_FAILED: 'Failed to copy the view command to clipboard.',
    LAUNCHING_HLAE_TOAST: 'Launching HLAE...',
  },

  // ── Half-Life Preview Detector modal ─────────────────────────────────────
  PROCESS_DETECTOR_MODAL: {
    TITLE: 'Half-Life Preview Detector',
    BODY: 'The Half-Life engine is already running (hl.exe / hlae.exe). Launching a new preview now can corrupt the capture session — close the running instance first, or force a relaunch.',
    // A batch fails differently and later: GoldSrc refuses the second instance
    // outright, but not until every demo in the queue has been patched, so the
    // work is already done by the time the error box appears.
    TITLE_BATCH: 'Day of Defeat Is Already Running',
    BODY_BATCH: 'The Half-Life engine is already running (hl.exe / hlae.exe). Day of Defeat allows only one instance, so the batch would patch every demo and then fail to launch. Close the running game first, or force a relaunch to close it now.',
    FORCE_RELAUNCH_BUTTON: 'Force Relaunch',
    COPY_VIEW_COMMAND_BUTTON: 'Copy View Command',
    CANCEL_BUTTON: 'Cancel',
  },

  // ── Export Configuration & Batch Capture Pipeline ────────────────────────
  CAPTURE_CONFIG: {
    PANEL_TITLE: 'Export Configuration & Batch Capture Pipeline',
    // Grouped by what a field does, not by what it historically sat next to.
    // Capture FPS moved out of Timing — it is the recording rate and has
    // nothing to do with when anything happens — and joined resolution, which
    // is the other half of "what the video is".
    TAB_PATH_ROUTING: 'Paths',
    TAB_OUTPUT_FORMAT: 'Output Format',
    TAB_TIMING_OPTIONS: 'Timing',
    TAB_PIPELINE: 'Pipeline',
    TAB_CAPTURE_OUTPUT: 'Destinations',
    TAB_CUSTOM_COMMANDS: 'Commands',
    TAB_RENDER_OUTPUT: 'Render Settings',
    TAB_NOTIFICATIONS: 'Notifications',
    HLAE_EXEC_LABEL: 'HLAE Executable:',
    HLAE_EXEC_PLACEHOLDER: 'Path to hlae.exe',
    HL_EXEC_LABEL: 'Half-Life Executable:',
    HL_EXEC_PLACEHOLDER: 'Path to hl.exe',
    // HLAE spawns its own FFmpeg for `mirv_movie_ffmpeg` and does not consult
    // the app's FFmpeg setting, so this is reported separately from it.
    HLAE_FFMPEG_LABEL: 'HLAE FFmpeg:',
    HLAE_FFMPEG_LINK_BUTTON: 'Point HLAE at FFmpeg',
    HLAE_FFMPEG_UNKNOWN: 'Set the HLAE executable above to check.',
    // Shown under the field that caused it, so a typo is obvious while you are
    // still looking at the box you typed it into. Nothing is blocked — Start
    // Capture Batch has its own guard.
    CAPTURE_MODE_FRAMES: 'Frame sequence',
    CAPTURE_MODE_FRAMES_TITLE:
        'HLAE writes every frame as its own bitmap. What this pipeline has always done, and what Render Studio was built around.',
    CAPTURE_MODE_VIDEO: 'Video',
    CAPTURE_MODE_VIDEO_TITLE:
        'HLAE pipes frames straight to FFmpeg as one lossless video per take. Same picture, roughly half the disk, and far fewer files. Needs the HLAE FFmpeg row above to be set.',
    CAPTURE_MODE_SWITCH_TITLE: 'Switch between capturing a bitmap frame sequence and a video file',
    CAPTURE_MODE_LABEL: 'Capture Mode:',
    CAPTURE_MODE_TITLE:
        'How frames get onto disk. Frame sequence and Video are both HLAE, deterministic and capable of any frame rate. OBS records the screen in real time instead, which is faster to a finished file but captures whatever actually rendered.',
    CAPTURE_MODE_OBS: 'OBS (real time)',
    CAPTURE_MODE_OBS_TITLE:
        'OBS records the game window while dod-tools tells it when each clip starts and stops. HLAE records nothing. Output is a finished, playable file with audio already in it — but capture runs at real time, so frames drop if the machine cannot keep up, and high capture rates are not possible. Separate HUD is not available on this path.',
    // Shown beside the progress bar while a batch runs, not in the settings —
    // there is nothing to configure and no mode it does not apply to. The
    // throttle is the engine's: GoldSrc slows its frame loop when the window is
    // not focused and `host_framerate` fast-forward stops with it, so the gaps
    // between clips play out in real time. Nothing is lost and no HLAE flag
    // defeats it — `engine_no_focus_sleep` is Source 2 only. See
    // docs/goldsrc_dod_quirks.md.
    FOCUS_REMINDER:
        'Keep Day of Defeat focused — it stops fast-forwarding between clips when it loses focus, and the batch takes far longer.',
    // ── OBS connection ──────────────────────────────────────────────────────
    OBS_SECTION_TITLE: 'OBS Connection',
    OBS_EXE_PATH_LABEL: 'OBS Path:',
    OBS_EXE_PATH_PLACEHOLDER: 'Path to obs64.exe',
    OBS_LAUNCH_BUTTON: 'Launch OBS',
    OBS_LAUNCHING: 'Launching…',
    obsLaunchFailed: (err) => `Could not launch OBS: ${err}`,
    OBS_HOST_LABEL: 'Host:',
    OBS_PORT_LABEL: 'Port:',
    OBS_PASSWORD_LABEL: 'Password:',
    OBS_PASSWORD_PLACEHOLDER: 'From Tools → WebSocket Server Settings',
    OBS_PASSWORD_TITLE:
        'The obs-websocket password, if OBS has authentication enabled. Use the Copy button in OBS rather than retyping it.',
    OBS_TEST_BUTTON: 'Test Connection',
    OBS_TESTING: 'Connecting…',
    OBS_LAUNCHING_AND_CONNECTING: 'Launching OBS and waiting for it to be ready…',
    OBS_UNREACHABLE: 'Could not reach OBS.',
    obsConnectedSummary: (obsVersion, websocketVersion) =>
        `Connected — OBS ${obsVersion} (obs-websocket ${websocketVersion})`,
    // Read-only — dod-tools always targets its own fixed profile/scene, there
    // is nothing here for the user to change.
    obsUsingSummary: (profile, scene) => `Using OBS profile "${profile}", scene "${scene}"`,
    obsCanvasSummary: (canvas, output, fps) => `Canvas ${canvas}, output ${output} @ ${Math.round(fps)} fps`,
    obsRecordingToSummary: (directory) => `Recording to ${directory}`,
    obsMissingRequests: (requests) => `This OBS is missing: ${requests.join(', ')} — capture cannot run.`,
    OBS_ALREADY_RECORDING: 'OBS is already recording — stop it before starting a batch.',
    OBS_ALREADY_STREAMING: 'OBS is streaming — dod-tools will not drive its recorder.',
    obsTestFailed: (err) => `OBS test failed: ${err}`,
    OBS_CAPTURE_FPS_LABEL: 'OBS Capture FPS:',
    OBS_CAPTURE_FPS_TITLE:
        "OBS's own live recording rate and the game's fps_max — separate from Capture FPS above, which is non-real-time for the other two modes. Set above what your machine can sustain and OBS drops frames instead of falling behind, making the clip choppy or unusable. Test a short capture first and watch for dropped-frame warnings before committing to a value.",
    OBS_CAPTURE_FPS_WARNING:
        'Too high for your machine and OBS drops frames — test a short capture first.',
    OBS_ENABLE_HINT:
        'OBS 28+: enable this under Tools → WebSocket Server Settings (the checkbox, not the Connect Info panel).',
    OBS_PROVISION_HINT:
        'dod-tools manages its own OBS profile/scene ([DoD-Tools]) — your own setup is never touched.',
    // ── Orphaned recording left by a previous run ───────────────────────────
    OBS_ORPHAN_TITLE: 'OBS is still recording',
    obsOrphanPrompt: (directory) =>
        `OBS is still recording into a dod-tools take folder:\n\n${directory}\n\nA previous session ended without stopping it — a crash, a force-quit or a power cut. It will keep recording until the drive fills.\n\nStop it and keep the clip?`,
    OBS_ORPHAN_STOP: 'Stop and keep',
    OBS_ORPHAN_LEAVE: 'Leave it',
    obsOrphanRecovered: (video) => `Stopped OBS and kept the recording: ${video}`,
    OBS_ORPHAN_GONE: 'OBS had already stopped recording.',
    obsOrphanFailed: (err) => `Could not stop the orphaned OBS recording: ${err}`,
    CAPTURE_CODEC_LABEL: 'Capture Codec:',
    // Says why the list is short, so the absence of H.264/HEVC reads as a
    // decision rather than an omission. The sizes are transcode measurements,
    // deliberately described as such — how each one behaves while competing
    // with the game for cores during a live capture is not measured.
    CAPTURE_CODEC_TITLE:
        'All lossless: the render pass always re-encodes, so a lossy capture would cost quality for nothing. Ut Video is the only one built for real-time and is the safe default. The others are smaller per frame but heavier to encode, and the capture slows down if the encoder cannot keep up.',
    CODEC_UTVIDEO: 'Ut Video (fastest, recommended)',
    CODEC_FFV1: 'FFV1 (smaller, slower)',
    CODEC_X264_LOSSLESS: 'x264 lossless (smallest, slowest)',
    CODEC_RAWVIDEO: 'Uncompressed (no CPU cost, huge)',
    // The tooltip above already hedges this, but a hover-only warning is easy
    // to miss — put it where it stays visible regardless of which option is
    // picked, since "unverified" doesn't change once you've stopped hovering.
    CAPTURE_CODEC_UNVERIFIED_HINT:
        'Only Ut Video has been proven in a real capture. The others are sized from a transcode with every core free — during a live capture they compete with the game, and that ranking is likely to change.',
    // Turning it on without HLAE having an FFmpeg produces a capture that runs
    // and records nothing, so it is worth saying before the batch rather than
    // after it.
    FFMPEG_CAPTURE_UNAVAILABLE:
        "Capture to video is on, but HLAE has no FFmpeg — the capture would run and produce no video. Sort the HLAE FFmpeg row above first.",
    PATH_NOT_FOUND: "There's no file at this path — check it for a typo.",
    PATH_IS_A_FOLDER: "That's a folder, not the program itself. Pick the .exe inside it.",
    HLAE_FFMPEG_BUNDLED: (path) => `Installed in HLAE's own folder (${path}).`,
    HLAE_FFMPEG_LINKED: (target) => `Pointed at ${target}.`,
    // Both halves of the pipeline encoding with the same FFmpeg build was the
    // whole reason for writing an ini instead of copying the binary, so a
    // divergence is worth stating rather than leaving to be discovered.
    HLAE_FFMPEG_DIVERGED: (target, app) =>
        `Pointed at ${target}, but Render Studio uses ${app}. Capture and render would use different FFmpeg builds — re-point HLAE unless that's deliberate.`,
    // ffplay.exe and ffprobe.exe live beside ffmpeg.exe and are one misclick
    // apart in a file picker, so this is worth naming rather than letting it
    // through to a capture that records nothing.
    // Checks the file the capture pipeline actually passes as -hookDllPath,
    // rather than what the exe calls itself. Advisory: nothing is blocked, since
    // an unusual install layout should not stop someone who knows it works.
    HLAE_FFMPEG_NO_HOOK_DLL: (dll) =>
        `AfxHookGoldSrc.dll isn't beside the HLAE Executable above (expected ${dll}). Capture needs that file — either the path isn't HLAE, or the DLL is missing or quarantined by antivirus.`,
    HLAE_FFMPEG_BAD_OVERRIDE: (why) =>
        `The FFmpeg Override Path above isn't FFmpeg: ${why}. Pick ffmpeg.exe — HLAE can't record with anything else.`,
    HLAE_FFMPEG_STALE: (target) =>
        `HLAE's ffmpeg.ini points at ${target}, which isn't there. Direct-to-video capture will produce no video until that path is fixed or the ini is deleted.`,
    HLAE_FFMPEG_MISSING:
        "HLAE has no FFmpeg of its own, so direct-to-video capture would run and produce no video. This is separate from Render Studio's FFmpeg.",
    HLAE_FFMPEG_LINKED_OK: (ini) => `Wrote ${ini}. HLAE can now find FFmpeg.`,
    HLAE_FFMPEG_LINK_FAILED: (err) => `Could not point HLAE at FFmpeg: ${err}`,
    // HLAE can live anywhere — it ships as a zip as well as an installer — so a
    // protected location like Program Files is one real possibility among
    // several, and needs a route through rather than a raw OS error.
    HLAE_FFMPEG_ELEVATE_TITLE: 'Administrator rights needed',
    HLAE_FFMPEG_ELEVATE_PROMPT: (ini) =>
        `${ini} is inside a protected folder, so Windows won't let dod-tools write there directly.\n\nContinue and Windows will ask for permission, then write a two-line file pointing HLAE at your FFmpeg. Nothing else is changed, and an existing ffmpeg.ini is never replaced.`,
    HLAE_FFMPEG_ELEVATE_CONFIRM: 'Ask Windows for permission',
    HLAE_FFMPEG_ELEVATE_REFUSED: 'Permission was declined, so nothing was written.',
    FFMPEG_OVERRIDE_LABEL: 'FFmpeg Override Path:',
    FFMPEG_OVERRIDE_PLACEHOLDER: 'Optional path to ffmpeg.exe',
    GOLDSRC_HOOKS_LABEL: 'GoldSrc Hooks DLL:',
    GOLDSRC_HOOKS_PLACEHOLDER: 'Optional path to goldsrc_hooks.dll (blank = bundled default)',
    BROWSE_BUTTON: 'Browse',
    WIDTH_LABEL: 'Width:',
    HEIGHT_LABEL: 'Height:',
    DECAL_FLUSH_LABEL: 'Flush Decals Between Clips',
    DECAL_FLUSH_TITLE:
      'Clear bullet holes and blood off the walls between one clip and the next, so a later capture does not inherit the damage from an earlier one. Off captures the walls exactly as the engine leaves them. How many decals the engine keeps is a separate thing — set r_decals in Initial Commands.',
    SAVE_LOCAL_PATCHED_LABEL: 'Save Local Patched Copy',
    PRE_ROLL_LABEL: 'Pre-roll (s):',
    PRE_ROLL_HINT: 'Time between fast-forward stopping and capture starting.',
    POST_ROLL_LABEL: 'Post-roll (s):',
    POST_ROLL_HINT: 'Time between capture stopping and fast-forward resuming.',
    START_LEAD_LABEL: 'Start Lead (s):',
    START_LEAD_HINT: 'Time between capture starting and the first kill.',
    STOP_TRAIL_LABEL: 'Stop Trail (s):',
    STOP_TRAIL_HINT: 'Time between the last kill and capture stopping.',
    INITIAL_DELAY_LABEL: 'Initial Delay (s):',
    INITIAL_DELAY_HINT: 'Time between the demo loading and fast-forward starting.',
    // Timing diagram (#150) — a visual timeline under the fields above,
    // showing how they relate to each other and to the recording window.
    timingDiagramInitialDelayNote: (v) => `This timeline starts after Initial Delay (${v}s) has already passed — a once-per-demo wait, not part of the per-clip cycle below.`,
    TIMING_DIAGRAM_COL_TIME: 'Relative Time',
    TIMING_DIAGRAM_COL_EVENT: 'Event',
    // Signed offset from the first kill (0.0s) — matches the pre-Tauri egui
    // build's "+{n} sec" / "{n} sec" formatting.
    timingDiagramTime: (v) => (v >= 0 ? `+${v.toFixed(1)}s` : `${v.toFixed(1)}s`),
    timingDiagramPreRollEvent: (v) => `Pre-roll ends (speed back to normal) — Pre-roll is ${v}s`,
    timingDiagramRecordStartEvent: (v) => `Recording starts — Start Lead is ${v}s`,
    TIMING_DIAGRAM_FIRST_KILL_EVENT: 'First kill (anchor)',
    TIMING_DIAGRAM_LAST_KILL_EVENT: 'Last kill (anchor — illustrative gap, not a real streak length)',
    timingDiagramRecordStopEvent: (v) => `Recording stops — Stop Trail is ${v}s`,
    timingDiagramPostRollEvent: (v) => `Post-roll ends, fast-forward resumes — Post-roll is ${v}s`,
    FF_SPEED_LABEL: 'FF Speed (x):',
    FF_SPEED_TITLE: 'Locked, matching dev — not currently user-editable.',
    FF_SPEED_HINT: 'How fast playback races toward each clip between kills, as a multiple of real time. Locked at 0.05x, matching dev — not currently user-editable.',
    CAPTURE_FPS_LABEL: 'Capture FPS:',
    // Worth stating outright. This used to sit under Timing Options, and the
    // adjacency invited exactly the confusion that cost real time: the demo's
    // own tickrate is a different number entirely, and conflating the two is
    // how a "3 second" margin turned out to be 0.6.
    CAPTURE_FPS_TITLE:
      'Frames per second written to the recorded video. Nothing to do with the demo\'s own tickrate, which is a property of how the demo was recorded and is not adjustable here. Not used in OBS mode — see OBS Capture FPS in the OBS Connection section below.',
    OUTPUT_DIR_PLACEHOLDER: 'Capture output directory path...',
    ADD_DIRECTORY_BUTTON: 'Add Directory',
    DESTINATIONS_HELP_TEXT: 'Captures are written here, and Render Studio scans these same locations for takes to render.',
    AUTO_CLEAR_LOGS_LABEL: 'Auto-clear Logs',
    AUTO_CLEAR_PREVIEWS_LABEL: 'Auto-clear Previews',
    AUTO_CLEAR_TEMP_DEMOS_LABEL: 'Auto-clear Temp Demos',
    CLEAR_PREVIEWS_BUTTON: 'Clear Previews...',
    NOTIFY_PATCHING_LABEL: 'Patching Started/Complete',
    NOTIFY_PATCHING_TITLE: 'One notification when patching begins, one when your demos are ready and capture is about to start. Not per-demo — decal clearing makes patching take real time now, but a toast per demo patched would be noise.',
    NOTIFY_DEMO_LOADING_LABEL: 'Demo Loading',
    NOTIFY_DEMO_LOADING_TITLE: 'Fires each time a new demo starts playing during capture, showing which demo and how many clips are on it. Automatically skipped when Fast-Forward to Clip is also on, since that notification covers the same ground with more detail.',
    NOTIFY_BETWEEN_CLIPS_LABEL: 'Fast-Forward to Clip',
    NOTIFY_BETWEEN_CLIPS_TITLE: 'Fires as playback starts fast-forwarding toward each clip, including the first one in a demo.',
    NOTIFY_CAPTURES_DONE_LABEL: 'Captures Done',
    NOTIFY_CAPTURES_DONE_TITLE: 'Fires once when the whole capture batch finishes.',
    NOTIFY_RENDERS_DONE_LABEL: 'Renders Done',
    NOTIFY_RENDERS_DONE_TITLE: 'Fires once when the whole render batch finishes.',
    NOTIFY_ERROR_LABEL: 'Errors',
    NOTIFY_ERROR_TITLE: 'Fires immediately if a patch, capture, or render step fails.',
    INIT_COMMANDS_LABEL: 'Initial Commands (run once at demo load):',
    INIT_COMMANDS_INFO_TITLE:
      "If you already exec a movie config from your config.cfg or autoexec, you don't need to add anything here. If you exec one manually instead, use Import Config below to pull its lines in as Initial Commands.",
    ADD_INIT_COMMAND_BUTTON: '+ Add Initial Command',
    IMPORT_CFG_BUTTON: 'Import Config...',
    importedCfgToast: (n) => `Imported ${n} command(s) from the config.`,
    IMPORTED_CFG_EMPTY_TOAST: 'That config had no commands to import.',
    // Both lists on this tab are custom commands; only one is scheduled, so
    // that is what the label says. Paired with INIT_COMMANDS_LABEL's "once at
    // demo load", the two read as the distinction they actually are.
    CUSTOM_COMMANDS_LABEL: 'Scheduled Commands (run relative to each highlight):',
    ADD_CUSTOM_COMMAND_BUTTON: '+ Add Scheduled Command',
    START_CAPTURE_BUTTON: 'Start Capture Batch',
    CANCEL_BATCH_BUTTON: 'Cancel Batch',
    STATUS_WAITING: 'Status: Waiting...',
    INIT_COMMAND_PLACEHOLDER: 'e.g. mirv_streams add all',
    CUSTOM_COMMAND_PLACEHOLDER: 'Command',
    CUSTOM_COMMAND_RELATION_OPTIONS: ['Before', 'After'],
    footerRequiredSpace: (gb) => `Required: ${gb} GB`,
    REQUIRED_SPACE_DEFAULT: 'Required: 0.00 GB',
  },

  // ── capture_pane.js runtime text (toasts, warnings, confirms) ───────────
  CAPTURE: {
    pathProblem: {
      notAbsolute: (p) => `"${p}" isn't a full path (it needs a drive letter, e.g. C:\\...)`,
      malformed: (p) => `"${p}" isn't a valid path (invalid characters or formatting)`,
      notFound: (p) => `"${p}" doesn't exist on this computer (check the spelling, or that the drive is connected)`,
      notADirectory: (p) => `"${p}" points to a file, not a folder`,
      unusable: (p) => `"${p}" is unusable`,
    },
    andNMore: (n) => `...and ${n} more`,
    NO_HIGHLIGHTS_SELECTED_WARNING: 'No highlights selected — pick at least one in the Highlights tab before starting a capture.',
    NO_DRIVES_CONFIGURED_WARNING: 'No Capture Output directories configured — add at least one with free space before starting a capture.',
    OBS_NOT_CONNECTED_WARNING: 'Not connected to OBS — capture mode is OBS, but the last connection check failed. Fix the connection in Configuration → Output Format before starting a capture.',
    OBS_CHECKING_WARNING: 'Checking the OBS connection…',
    bannedCommandsWarning: (n) => `${n} command${n === 1 ? '' : 's'} in Initial or Scheduled Commands ${n === 1 ? 'is' : 'are'} not allowed — remove ${n === 1 ? 'it' : 'them'} in the Commands tab before starting a capture.`,
    // Measured 2026-08-28, see docs/direct_to_video_capture.md. Spelled out
    // because both halves report success and the broken output only shows up
    // after rendering — the user has no other way to find out.
    noUsableSpaceProblem: (desc) => `Capture Output problem:\n${desc}`,
    NO_USABLE_SPACE_WARNING: 'None of the configured Capture Output directories have any free space.',
    insufficientSpaceWarning: (required, available) => `Insufficient disk space: capture needs ~${required} GB, only ${available} GB available across the export pool.`,
    partialProblemsWarning: (wontBeUsed, desc) => `Some Capture Output ${wontBeUsed}:\n${desc}\nCapture will proceed using the other configured directory/directories.`,
    ENTRY_WONT_BE_USED_SINGULAR: "entry won't be used",
    ENTRY_WONT_BE_USED_PLURAL: "entries won't be used",
    willBeCreatedSingle: (path, doesnt) => `${path} ${doesnt} exist yet — it'll be created when the capture starts.`,
    willBeCreatedMultiple: (doesnt, list) => `These Capture Output entries ${doesnt} exist yet — they'll be created when the capture starts:\n${list}`,
    DOESNT_SINGULAR: "doesn't",
    DOESNT_PLURAL: "don't",
    DELETE_SELECTED_DEFAULT: 'Delete Selected',
    deleteNSelected: (n) => `Delete ${n} Selected`,
    NO_ORPHANED_PREVIEWS: 'No orphaned preview demos found.',
    SCANNING_ORPHANED_PREVIEWS: 'Scanning for orphaned preview demos...',
    SCAN_COMPLETE: 'Scan complete.',
    SCAN_FAILED: 'Scan failed.',
    scanFailedRow: (e) => `Scan failed: ${e}`,
    CONFIGURE_HL_PATH_FIRST: 'Configure the Half-Life Executable (hl.exe) path before auditing previews.',
    deletePreviewsConfirm: (n) => `Permanently delete ${n} orphaned preview demo(s)?`,
    deletedPreviews: (n) => `Deleted ${n} orphaned preview demo(s).`,
    deletionFailed: (e) => `Deletion failed: ${e}`,
    CAPTURING_DEFAULT: 'Capturing',
    CAPTURING_ELLIPSIS_DEFAULT: 'Capturing...',
    capturingWithName: (status, name) => `${status}: ${name}`,
    captureErrorToast: (status) => `Capture error: ${status}`,
    CAPTURE_ERROR_STATUS_DEFAULT: 'Unknown error',
    captureErrorStatusText: (status) => `Error: ${status}`,
    CAPTURE_ERROR_TEXT_DEFAULT: 'Capture failed',
    CANCELLED: 'Cancelled',
    BATCH_CANCELLED_TOAST: 'Batch capture cancelled.',
    COMPLETED: 'Completed',
    BATCH_COMPLETED_TOAST: 'Batch capture completed successfully!',
    takesFoundMissing: (captured, total) => `${captured}/${total} takes found on disk — ${total - captured} missing.`,
    takesRenderStudioMiss: (captured, total, missingRender) => `${captured}/${total} takes captured, but ${missingRender} won't be seen by Render Studio.`,
    allTakesVerified: (total) => `All ${total} takes verified on disk.`,
    highlightsMarkedCaptured: (n) => ` ${n} highlight(s) marked Captured.`,
    BOTH_PATHS_REQUIRED: 'Please specify valid file paths for both HLAE Executable (hlae.exe) and Half-Life Executable (hl.exe).',
    NO_CAPTURE_OUTPUT_DIR: 'Configure at least one Capture Output directory before starting a capture.',
    NO_CAPTURE_OUTPUT_DIR_WITH_SPACE: 'Configure at least one Capture Output directory with free space before starting a capture.',
    insufficientDiskSpaceToast: (required, available) => `Insufficient disk space. Required: ${required} GB, Available: ${available} GB`,
    INITIALIZING_CAPTURE_BATCH: 'Initializing capture batch...',
    BATCH_QUEUED_TOAST: 'Batch capture queued successfully!',
    startBatchError: (err) => `Error starting batch: ${err}`,
    CANCELLING_BATCH_TOAST: 'Cancelling batch...',
    EMPTY_DASH: '—',
    megabytesLabel: (mb) => `${mb} MB`,
  },

  // ── Render Studio panel + render_pane.js ─────────────────────────────────
  RENDER: {
    PANEL_TITLE: 'Render Studio',
    CODEC_LABEL: 'Codec:',
    CODEC_PRORES: 'ProRes 422 HQ',
    CODEC_DNXHR: 'DNxHR HQ',
    CODEC_H264: 'H.264 (Software, MP4)',
    CODEC_H264_NVENC: 'H.264 (NVENC GPU, MP4)',
    SOURCE_FPS_LABEL: 'Source FPS:',
    MAX_CONCURRENT_LABEL: 'Max Concurrent Renders:',
    EXPORT_DIR_PLACEHOLDER: 'Add export drive/folder...',
    EXPORT_DIR_ROW_PLACEHOLDER: 'Export drive/folder path...',
    ADD_DRIVE_BUTTON: 'Add Drive',
    BROWSE_DRIVE_BUTTON: 'Browse Drive',
    TOTAL_EXPORT_POOL_FREE_LABEL: 'Total Export Pool Free:',
    EXPORT_POOL_FREE_DEFAULT: '0.0 GB',
    exportPoolFreeGb: (gb) => `${gb} GB`,
    SCAN_FOR_TAKES_BUTTON: 'Scan for Takes',
    TABLE_HEADER_CLIP_NAME: 'Clip Name',
    TABLE_HEADER_STREAM: 'Stream',
    TABLE_HEADER_FRAMES: 'Frames',
    TABLE_HEADER_DATE: 'Date',
    TABLE_HEADER_SETTINGS: 'Settings',
    TABLE_HEADER_SETTINGS_TITLE: "Codec/FPS this job is queued to render with — its own setting, not necessarily what Configuration currently shows",
    TABLE_HEADER_STATUS: 'Status',
    TABLE_HEADER_SPEED: 'Speed',
    TABLE_HEADER_PROGRESS: 'Progress',
    TABLE_HEADER_FILE_SIZE: 'File Size',
    TABLE_HEADER_FILE_SIZE_TITLE: "The real encoded size, once finished — not shown ahead of time, since it can't be predicted accurately (see Required (Over-estimated) in the footer).",
    fileSizeGb: (gb) => `${gb} GB`,
    fileSizeMb: (mb) => `${mb} MB`,
    FILE_SIZE_UNKNOWN: '—',
    TABLE_HEADER_ACTIONS: 'Actions',
    TABLE_EMPTY: 'No render jobs queued. Scan a folder, then click Start Render Batch.',
    START_RENDER_BUTTON: 'Start Render Batch',
    CANCEL_ALL_BUTTON: 'Cancel All',
    RESET_ALL_BUTTON: 'Reset All',
    REMOVE_ALL_BUTTON: 'Remove All (Not Rendering)',
    STATUS_WAITING: 'Status: Waiting...',

    CANCEL_JOB_TITLE: 'Cancel this job',
    RESET_JOB_TITLE: 'Reset to Queued',
    REMOVE_JOB_TITLE: 'Remove this row — cannot be undone',
    SKIP_TOGGLE_LABEL: 'Skip',
    SKIP_TOGGLE_TITLE: 'Leave this OBS take exactly as recorded — no re-encode, just routed into the export pool under the pipeline naming.',
    setJobCodecFailed: (err) => `Could not change this job's render setting: ${err}`,
    VIEW_LOG_TITLE: 'View error log',
    VIEW_LOG_BUTTON: '⚠️ View Log',
    OPEN_OUTPUT_FOLDER_TITLE: "Open the rendered file's folder",
    OPEN_TAKE_FOLDER_TITLE: 'Open the source take folder',
    OPEN_OUTPUT_BUTTON: '📁 Open Output',
    OPEN_TAKE_FOLDER_BUTTON: '📁 Open Take Folder',
    QUEUE_SUMMARY_DEFAULT: '0 queued · 0 rendering · 0 done',
    queueSummary: (queued, rendering, done) => `${queued} queued · ${rendering} rendering · ${done} done`,
    ERROR_LOG_TITLE_DEFAULT: 'FFmpeg Error Log',
    errorLogTitleForJob: (name) => `FFmpeg Error Log — ${name}`,
    exportPoolFreeFooter: (gb) => `Export Pool Free: ${gb} GB`,
    RENDER_POOL_FREE_DEFAULT: 'Export Pool Free: 0.0 GB',
    // "Over-estimated" matters here in a way it doesn't for Capture's
    // Required figure — this is a deliberately loose upper bound (raw-frame
    // math), not a tight prediction. See get_render_required_estimate_gb.
    requiredEstimatedFooter: (gb) => `Required (Over-estimated): ${gb} GB`,
    REQUIRED_ESTIMATED_FOOTER_TITLE: "A loose upper bound, not a tight prediction — most codecs compress well below this, and it can't be known ahead of time how much. Covers every job that hasn't finished yet (Queued + Rendering).",
    REQUIRED_ESTIMATED_DEFAULT: 'Required (Over-estimated): 0.00 GB',
    recoveredJobsToast: (completed, pending) => `Recovered ${completed} completed, ${pending} pending render job(s).`,
    recoverFailed: (err) => `Failed to recover render batch: ${err}`,
    renderingStatus: (done, total) => `Status: Rendering (${done}/${total} done)`,
    BATCH_FINISHED_WITH_ERRORS: 'Render batch finished with errors — check job rows for details.',
    BATCH_CANCELLED: 'Render batch cancelled.',
    BATCH_COMPLETED: 'Render batch completed successfully!',
    // Mixed-outcome batches report every non-zero count instead of one label,
    // so cancelling the takes you did not want does not read as "nothing
    // rendered". Singular/plural matters here — these numbers are often 1.
    countRendered: (n) => `${n} rendered`,
    countFailed: (n) => (n === 1 ? '1 failed' : `${n} failed`),
    countCancelled: (n) => `${n} cancelled`,
    batchSummary: (parts) => `Render batch finished — ${parts}.`,
    STATUS_FINISHED: 'Status: Finished',
    scanFoundSoFar: (n) => `Status: Scanning… found ${n} take(s) so far`,
    ADD_RENDER_DIR_REQUIRED: 'Please add at least one render directory.',
    SCANNING_TOAST: 'Scanning render directories...',
    STATUS_SCANNING: 'Status: Scanning…',
    scannedTakesToast: (count) => `Scanned ${count} render take(s).`,
    scanCompleteStatus: (count) => `Status: Scan complete — ${count} take(s) found`,
    NO_RENDER_TAKES_DETECTED: 'No render takes detected.',
    scanDirError: (err) => `Error scanning render directories: ${err}`,
    STATUS_SCAN_FAILED: 'Status: Scan failed',
    INITIALIZING_RENDER_BATCH: 'Initializing render batch...',
    STATUS_SCANNING_FOR_TAKES: 'Status: Scanning for takes...',
    RENDER_BATCH_QUEUED: 'Render batch queued successfully!',
    renderBatchError: (err) => `Error executing render batch: ${err}`,
    CANCELLING_RENDER_BATCH: 'Cancelling render batch...',
    nvencWarning: (n) => `${n} concurrent NVENC renders may exceed your GPU's encoder session limit (often 3-5 on consumer GeForce cards). If renders start failing, lower Max Concurrent Renders.`,
    highlightsMarkedRendered: (n) => `${n} highlight(s) marked Rendered.`,
    UNKNOWN_SOURCE_FOLDER: '(unknown)',
    frameCountLabel: (n) => `${n} frames`,
  },

  // ── FFmpeg Error Log modal ───────────────────────────────────────────────
  ERROR_LOG_MODAL: {
    TITLE_DEFAULT: 'FFmpeg Error Log',
    CLOSE_BUTTON: 'Close',
  },

  // ── Render batch crash-recovery modal ────────────────────────────────────
  RENDER_RECOVERY_MODAL: {
    TITLE: '🎬 Render Batch Interrupted',
    BODY: "The last render batch didn't finish cleanly (app closed or crashed mid-batch).",
    SOURCE_LABEL: 'Source:',
    COMPLETED_LABEL: '✅ Completed:',
    PENDING_LABEL: '⏳ Pending:',
    RECOVER_BUTTON: '🔄 Recover Render Batch',
    DISCARD_BUTTON: '🗑 Discard',
  },

  // ── Demo Auditor pane + auditor_pane.js ──────────────────────────────────
  AUDITOR: {
    PANEL_TITLE: 'Demo Auditor (Deduplication)',
    TARGET_FOLDER_LABEL: 'Target Folder:',
    TARGET_FOLDER_PLACEHOLDER: 'Folder to scan for duplicate demos...',
    BROWSE_BUTTON: 'Browse',
    START_AUDIT_BUTTON: 'Start Audit',
    CANCEL_SCAN_BUTTON: 'Cancel Scan',
    STATUS_READY: 'Status: Ready to audit',
    RESULTS_TITLE: 'Duplicate Groups Found',
    DELETE_SELECTED_BUTTON: 'Delete Selected Files',
    TABLE_HEADER_STATUS: 'Status',
    TABLE_HEADER_SIZE: 'Size',
    TABLE_HEADER_FILE_PATH: 'File Path',
    TABLE_HEADER_ACTION: 'Action',
    TABLE_EMPTY: 'Choose a folder and run audit to find duplicate demo files.',
    FOOTER_DEFAULT: 'Duplicates Found: 0 | Wasted Space: 0.00 GB',
    footerSummary: (count, gb) => `Duplicates Found: ${count} | Wasted Space: ${gb} GB`,

    SELECT_FOLDER_DIALOG_TITLE: 'Select Folder to Audit',
    foundSoFarHtml: (n) => `<strong>Found ${n} demo file(s) so far&hellip;</strong>`,
    statusLineHtml: (status) => `<br><span class="text-muted">${status}</span>`,
    CHOOSE_FOLDER_FIRST: 'Choose a target folder before starting an audit.',
    INITIALIZING_HTML: '<strong>Initializing&hellip;</strong>',
    AUDITING_IN_PROGRESS_ROW: 'Auditing in progress...',
    auditFailedRow: (e) => `Audit failed: ${e}`,
    CANCELLING_HTML: '<strong>Cancelling&hellip;</strong>',
    NO_DUPLICATES_ROW: 'No duplicates found! Your demos are clean.',
    groupToggleLabel: (expanded, count) => `${expanded ? '▼' : '▶'} Group (${count} files)`,
    identicalHash: (hash) => `Identical Hash: ${hash}`,
    ORIGINAL_FILE_TITLE: 'Original file (kept)',
    FILE_ROW_LABEL: '   ↳ File',
    COPY_PATH_BUTTON: '📋 Copy Path',
    OPEN_FOLDER_BUTTON: '📁 Open Folder',
    PATH_COPIED_TOAST: 'Path copied to clipboard.',
    COPY_PATH_FAILED_TOAST: 'Failed to copy path.',
    deleteNSelectedFiles: (n) => `Delete ${n} Selected File(s)`,
    deleteConfirm: (n) => `Are you sure you want to permanently delete ${n} files?`,
    deletedFilesToast: (n) => `Successfully deleted ${n} duplicate files.`,
    deletionFailedToast: (e) => `Deletion failed: ${e}`,
    megabytesLabel: (mb) => `${mb} MB`,
  },

  // ── Clear Previews modal ─────────────────────────────────────────────────
  CLEAR_PREVIEWS_MODAL: {
    TITLE: 'Clear Previews',
    SELECT_ALL_BUTTON: 'Select All',
    SCANNING_STATUS: 'Scanning for orphaned preview demos...',
    TABLE_HEADER_FILE: 'File',
    TABLE_HEADER_SIZE: 'Size',
    TABLE_HEADER_MODIFIED: 'Modified',
    SCANNING_ROW: 'Scanning...',
    FOOTER_DEFAULT: 'Found: 0 | Reclaimable: 0.00 GB',
    foundReclaimable: (count, gb) => `Found: ${count} | Reclaimable: ${gb} GB`,
    DELETE_SELECTED_BUTTON: 'Delete Selected',
    CLOSE_BUTTON: 'Close',
  },

  // ── Demo Analyzer pane (explorer, filters, 7 report tabs) ────────────────
  ANALYZER: {
    EXPLORER_TITLE: 'Explorer',
    REFRESH_TREE_TITLE: 'Refresh folder tree',
    REFRESH_TREE_BUTTON: '⟳ Refresh',
    EXPLORER_SETTINGS_SUMMARY: '⚙ Explorer Settings',
    SHOW_FOLDER_COUNTS_LABEL: 'Show folder demo counts in the tree',
    ADD_PIN_BUTTON: '➕ Add Pin…',
    RESIZE_HANDLE_TITLE: 'Drag to resize',
    DEMOS_TITLE: 'Demos',
    SEARCH_NAME_MAP_PLACEHOLDER: 'Search name/map...',
    TYPE_ALL: 'All',
    TYPE_POV: 'POV',
    TYPE_HLTV: 'HLTV',
    MAP_PLACEHOLDER: 'Map',
    MIN_DATE_PLACEHOLDER: 'Min Date (YYYY-MM-DD)',
    MAX_DATE_PLACEHOLDER: 'Max Date (YYYY-MM-DD)',
    RESET_BUTTON: 'Reset',
    DEMO_TABLE_HEADERS: { name: 'Name', type: 'Type', map: 'Map', date: 'Date' },
    COL_TYPE: 'Type',
    COL_MAP: 'Map',
    COL_DATE: 'Date',
    ANALYZER_TITLE: 'Demo Analyzer',
    BROWSE_DEMO_BUTTON: 'Browse Demo...',
    SUBTAB_SUMMARY: 'Summary',
    SUBTAB_SCOREBOARD: 'Scoreboard',
    SUBTAB_PLAYER_DETAILS: 'Player Details',
    SUBTAB_TEAM_DETAILS: 'Team Details',
    SUBTAB_TIMELINE: 'Timeline',
    SUBTAB_ROUNDS: 'Rounds',
    SUBTAB_CHAT: 'Chat Log',
    EMPTY_PICK_DEMO: 'Pick a folder and demo on the left, browse for a file, or select one from the Workspace and click "View Match Telemetry".',
    EMPTY_PICK_DEMO_JS_FALLBACK: 'Browse for a demo file, or select one from the Workspace and click "View Match Telemetry".',

    SCANNING_WORKSPACE: 'Scanning workspace…',
    NO_DEMO_FOLDERS_FOUND: 'No demo folders found.',
    TIER_PINNED: '📌 Pinned',
    TIER_RECENT: '🕒 Recent',
    TIER_LOCAL: '📂 Local',
    PIN_FOLDER_TITLE: 'Pin folder',
    UNPIN_FOLDER_TITLE: 'Unpin folder',
    ADD_PINNED_FOLDER_DIALOG_TITLE: 'Add Pinned Folder',
    SELECT_DEMO_DIALOG_TITLE: 'Select Demo to Analyze',
    LOADING_LABEL: 'Loading…',
    THIS_PC_LABEL: '💻 This PC',
    PICK_FOLDER_FROM_SIDEBAR: 'Pick a folder from the Explorer sidebar.',
    NO_DEMOS_IN_FOLDER: 'No demos found in this folder.',
    NO_DEMOS_MATCH_FILTERS: 'No demos match the current filters.',
    ANALYZING_ELLIPSIS: 'Analyzing…',
    ANALYZING_DEMO_ELLIPSIS: 'Analyzing demo…',
    analyzingPct: (pct) => `Analyzing… ${pct}%`,
    analyzingDemoPct: (pct) => `Analyzing demo… ${pct}%`,
    analyzeFailed: (err) => `Failed to analyze demo: ${err}`,
    NO_DEMO_LOADED: 'No demo loaded',

    NO_PLAYERS_FOUND: 'No players found in this demo.',
    NO_WEAPON_DATA: 'No weapon data.',
    NO_KILL_STREAKS: 'No kill streaks recorded.',
    NO_TEAM_SCORE_EVENTS: 'No team score events recorded',
    NO_COMPLETED_ROUNDS: 'No completed rounds recorded.',
    NO_MESSAGES_MATCH_FILTERS: 'No messages match the current filters.',
    HIDE_ALL_TITLE: 'Hide all',
    SHOW_ALL_TITLE: 'Show all',
    SEARCH_SENDER_TEXT_PLACEHOLDER: 'Search sender or text...',
    CHAT_HEADING: 'Chat &amp; System Log',
    SELECT_ALL_BUTTON: 'Select All',
    CLEAR_ALL_BUTTON: 'Clear All',
    ALL_CHAT_LABEL: 'All Chat',
    TEAM_CHAT_LABEL: 'Team Chat',
    STATUS_ALL: 'All',
    STATUS_ALIVE: 'Alive',
    STATUS_DEAD: 'Dead',
    TEAM_LABEL: 'Team:',
    SYSTEM_LOGS_LABEL: 'System Logs:',
    JOINS_LEAVES_LABEL: 'Joins/Leaves',
    TEAM_CHANGES_LABEL: 'Team Changes',
    GAMEPLAY_LABEL: 'Gameplay',
    OTHER_SYSTEM_LABEL: 'Other System',

    scoreboardHeading: (alliesLabel, alliesScore, cmp, axisScore) => `Scoreboard: ${alliesLabel} (${alliesScore}) ${cmp} Axis (${axisScore})`,
    compareGlyph: (a, b) => (a > b ? '>' : (a === b ? '=' : '<')),
    durationLong: (h, m, s) => {
      if (h > 0) return `${h}h ${m}m ${s}s`;
      if (m > 0) return `${m}m ${s}s`;
      return `${s}s`;
    },
    secondsSuffix: (n) => `${n}s`,
    megabytesLabel: (mb) => `${mb} MB`,
    KD_BADGE_LABEL: 'K/D',
    partialRecordingBoth: (fmt) => `Partial recording — demo started with ${fmt} remaining and ended before the match concluded.`,
    partialRecordingStartedLate: (fmt) => `Partial recording — demo started with ${fmt} remaining on the clock.`,
    partialRecordingEndedEarly: (fmt) => `Partial recording — demo ended before the match concluded (${fmt} remaining at cutoff).`,
    groupLabelWithCount: (label, count) => `${label} — ${count} player(s)`,
    COL_NAME: 'Name',
    COL_CLASS: 'Class',
    COL_SCORE: 'Score',
    COL_KILLS: 'Kills',
    COL_DEATHS: 'Deaths',
    AXIS_LABEL: 'Axis',
    SPECTATORS_LABEL: 'Spectators',
    UNASSIGNED_LABEL: 'Unassigned',
    RECONNECTED_TITLE: 'Player reconnected mid-demo',
    PRE_DEMO_ACTIVITY_TITLE: 'Player had pre-existing stats when recording started',
    UNKNOWN_CLASS: 'Unknown',

    PLAYER_LABEL: 'Player:',
    LEGIT_PROOF_LINK_TITLE: 'Search this player on Legit-Proof',
    LEGIT_PROOF_TEXT: 'Legit-Proof',
    STEAM_PROFILE_TEXT: 'Steam Profile',
    NO_STEAM_ID: 'No Steam ID',
    STEAM_ID_LABEL: 'Steam ID: ',
    CLOCK_UNKNOWN: '??:??',
    TIMELINE_START_LABEL: '0:00',
    connectedSlot: (id) => `Connected (Slot ${id})`,
    DISCONNECTED: 'Disconnected',
    RECONNECTED_MID_DEMO: '🔄 Reconnected mid-demo',
    PRE_EXISTING_STATS: '* Pre-existing stats',
    MATCH_SCORE_TITLE: 'Match Score',
    KILLS_TITLE: 'Kills',
    DEATHS_TITLE: 'Deaths',
    AVG_LIFESPAN_TITLE: 'Avg. Lifespan',
    minMaxBadge: (min, max) => `Min: ${min}s / Max: ${max}s`,
    WEAPON_BREAKDOWN_TITLE: 'Weapon Breakdown',
    COL_WEAPON: 'Weapon',
    COL_PCT_TOTAL: '% of Total',
    COL_TEAM_KILLS: 'Team Kills',
    KILL_STREAKS_TITLE: 'Kill Streaks',
    COL_WAVE: 'Wave',
    COL_TIME: 'Time',
    COL_DURATION: 'Duration',
    WEAPON_CATEGORY_GRENADES: 'Grenades',
    WEAPON_CATEGORY_MELEE: 'Melee',
    WEAPON_CATEGORY_ALLIED: 'Allied',
    WEAPON_CATEGORY_OTHER: 'Other',

    TEAM_DETAILS_HEADING: 'Team Details',
    MATCH_OVERVIEW_TITLE: 'Match Overview',
    ROUND_SCORE_LABEL: 'Round Score',
    TOTAL_KILLS_LABEL: 'Total Kills',
    TOTAL_DEATHS_LABEL: 'Total Deaths',
    TEAM_KD_LABEL: 'Team K/D',
    ACTIVE_PLAYERS_LABEL: 'Active Players',
    TEAM_WEAPON_PERFORMANCE_TITLE: 'Team Weapon Performance',
    ALLIES_US_LABEL: 'Allies (US)',
    ALLIES_LABEL: 'Allies',
    BRITISH_LABEL: 'British',

    TEAM_SCORE_TIMELINE_TITLE: 'Team Score Timeline',
    TIMELINE_TOOLTIP_ELAPSED_LABEL: 'Time Elapsed:',
    TIMELINE_TOOLTIP_TIMESTAMP_LABEL: 'Demo Timestamp:',

    ROUNDS_TITLE: 'Rounds',
    COL_ROUND_NUM: '#',
    COL_START_TIME: 'Start Time',
    COL_WINNER: 'Winner',
    COL_KILLS_BY_WINNER: 'Kills by Winner',

    // Summary tab
    FILE_INFO_SECTION: 'File Information',
    FILE_NAME_LABEL: 'File name',
    FILE_PATH_LABEL: 'File path',
    FILE_SIZE_LABEL: 'File size',
    FILE_CREATED_LABEL: 'File created',
    GAME_DETAILS_SECTION: 'Game Details',
    GAME_MOD_LABEL: 'Game mod',
    MAP_NAME_LABEL: 'Map name',
    MAP_CHECKSUM_LABEL: 'Map checksum',
    SERVER_INFO_SECTION: 'Server Information',
    SERVER_NAME_LABEL: 'Server name',
    SERVER_ADDRESS_LABEL: 'Server address',
    DEMO_MATCH_DETAILS_SECTION: 'Demo & Match Details',
    RECORDED_BY_LABEL: 'Recorded by',
    DEMO_TYPE_LABEL: 'Demo type',
    MATCH_TYPE_LABEL: 'Match type',
    DEMO_DURATION_LABEL: 'Demo duration',
    MATCH_DURATION_LABEL: 'Match duration',
    TECH_SPECS_SECTION: 'Technical Specifications',
    DEMO_PROTOCOL_LABEL: 'Demo protocol',
    NETWORK_PROTOCOL_LABEL: 'Network protocol',
    GAME_MOD_DOD: 'Day of Defeat',
    GAME_MOD_CS: 'Counter-Strike',
    GAME_MOD_HL: 'Half-Life',
    MATCH_TYPE_PUBLIC: 'Public / Pickup',
    MATCH_TYPE_PREGAME: 'Clan Match (Pre-game)',
    MATCH_TYPE_INCOMPLETE: 'Clan Match (Incomplete Recording)',
    MATCH_TYPE_FULL: 'Clan Match (Fully Recorded)',
    RECORDED_BY_HLTV_DEFAULT: 'HLTV',
    RECORDED_BY_UNKNOWN: 'Unknown',
    WEAPON_UNKNOWN: 'Unknown',
    EMPTY_DASH: '—',
    CHAT_DEAD_BADGE: '*DEAD*',
    CHAT_SYSTEM_TAG: '[system]',
    CHAT_TEAM_BADGE: '(Team)',
    CHAT_SENDER_UNKNOWN: 'Unknown',
  },

  // ── Clear All / Clear Selected / Remove Tracked Demo modal ───────────────
  CLEAR_ALL_MODAL: {
    TITLE_DEFAULT: 'Clear All Demos',
    SAVE_SESSION_FIRST_BUTTON: 'Save Session First',
    CLEAR_ANYWAY_DEFAULT: 'Clear Anyway',
    CANCEL_BUTTON: 'Cancel',
  },

  // ── Generic themed Confirm/Cancel dialog (themed_confirm.js) ─────────────
  THEMED_CONFIRM_MODAL: {
    TITLE_DEFAULT: 'Confirm',
    CONFIRM_BUTTON: 'Confirm',
    CANCEL_BUTTON: 'Cancel',
  },

  // ── Unsaved-changes prompt on window close (Capture Studio) ─────────────
  UNSAVED_CHANGES_MODAL: {
    TITLE: 'Unsaved Changes',
    MESSAGE: 'Capture Studio has unsaved changes. Save your session before closing?',
    SAVE_BUTTON: 'Save & Close',
    DISCARD_BUTTON: 'Close Without Saving',
    CANCEL_BUTTON: 'Cancel',
  },

  // ── main.js: sessions, settings dialogs, scan status, Clear actions ─────
  // Map library warnings. A demo names the map it was recorded on and stamps
  // that map's build alongside it, so "missing" and "wrong build" are different
  // problems: one cannot be played at all, the other plays and is quietly wrong.
  MAPS: {
    BANNER_TITLE: 'Maps needed',
    MISSING_LABEL: 'missing',
    WRONG_BUILD_LABEL: 'different build',
    UNREADABLE_LABEL: 'unreadable',
    DOWNLOAD_BUTTON: 'Download',
    DOWNLOAD_ALL_BUTTON: 'Download all',
    DISMISS_BUTTON: 'Dismiss',
    DOWNLOADING: 'Downloading…',
    NO_GAME_PATH: 'Set the hl.exe path in Configuration to check demo maps.',
    demoCount: (n) => (n === 1 ? '1 demo' : `${n} demos`),
    missingSummary: (maps, demos) =>
      `${maps === 1 ? '1 map' : `${maps} maps`} needed by ${demos === 1 ? '1 demo' : `${demos} demos`}`,
    wrongBuildDetail: (map, wanted, found) =>
      `${map} — these demos need build ${wanted}, the installed map is ${found}`,
    missingDetail: (map, demos) => `${map} — not installed, needed by ${demos}`,
    installedToast: (map) => `Installed ${map}`,
    alreadyCorrectToast: (map) => `${map} was already the right build`,
    replacedNote: (path) => `Previous map kept at ${path}`,
    downloadFailedToast: (map, err) => `Could not install ${map}: ${err}`,
    checkFailed: (err) => `Could not check demo maps: ${err}`,
    UNVERIFIABLE_NOTE:
      'HLTV demos do not record which map build they need, so those can only be checked for the map being present.',
  },

  // The game's own config files setting cvars this app reads. Advisory only —
  // nothing in this app writes to a config file.
  CFG: {
    BANNER_TITLE: "Your game's config files set values this app reads:",
    ADVICE:
      'These are set outside the app, so it cannot see them when it plans a capture. Either remove them from your configs, or state them in Initial Commands below so the pipeline works from the same values the engine does. Nothing here changes your config files.',
    location: (file, line) => `set in ${file}, line ${line}`,
    OVERRIDE_TITLE: 'These Initial Commands will override your config files:',
    OVERRIDE_ADVICE:
      'Initial Commands run after the game loads its configs, so these values win. That is usually the point — but the config line stops applying, and nothing else would tell you.',
    FROM_APP_NOTE: 'added by the app',
    SHADOWED_TITLE: 'These Initial Commands will not take effect:',
    SHADOWED_ADVICE:
      'The app appends its own commands after yours, and the last one wins. Change the setting that owns the value instead — editing the line here cannot win.',
    shadowedByApp: (cvar, yours, winner, setting) =>
      `${cvar} ${yours} never applies — the app sets ${winner} from ${setting}`,
    shadowedByYou: (cvar, yours, winner) =>
      `${cvar} ${yours} never applies — a later Initial Command sets ${winner}`,
    // Which setting owns a value the pipeline appends for itself, so the advice
    // can name the control rather than leaving the user to hunt for it.
    SETTING_FOR_CVAR: {
      mirv_movie_fps: 'Output Format → Capture FPS',
      r_decals: 'Pipeline → Flush Decals Between Clips',
    },
    UNKNOWN_SETTING: 'its own setting',
    BANNED_TITLE: 'These commands are not allowed:',
    BANNED_ADVICE:
      'Too dangerous to run at all — remove them. Start Capture Batch stays disabled while any are present.',
    // Why each is banned, and where to go instead when there's a real setting
    // for it.
    BANNED_REASONS: {
      mirv_movie_ffmpeg: 'set Capture Mode to Video in the Output Format tab instead',
      host_framerate: 'fast-forwarding is handled by the app; this will break the automation',
      mirv_recordmovie_start: 'the app schedules this itself; a manual one will break the automation',
      mirv_recordmovie_stop: 'the app schedules this itself; a manual one will break the automation',
      mirv_movie_filename: 'set the save location in the Destinations tab instead',
      r_drawentities:
        "the engine resets this to 1 by itself, so it does nothing -- and if cheats are on instead, DoD's client closes the game",
      cl_lw: "DoD's client quits the game outright if this is not 1 -- there is no other value",
      // Fine as Initial Commands — that's how the decal flush is meant to be
      // configured — but banned_scheduled only ever reports these three when
      // they show up as Scheduled Commands instead, so the reason is always
      // about the mid-demo change, never about the cvar itself being wrong.
      r_decals: "can't change mid-demo — set it in Initial Commands instead",
      mirv_fov: "can't change mid-demo — set it in Initial Commands instead",
      gl_widescreenfov: "can't change mid-demo — set it in Initial Commands instead",
    },
    bannedRowDetailed: (command, reason) => (reason ? `${command} — not allowed: ${reason}` : `${command} — not allowed`),
    HAZARD_TITLE: 'These Scheduled Commands are redundant with a Configuration setting:',
    HAZARD_ADVICE:
      "mirv_movie_fps is already pinned every capture from Output Format's own Capture FPS setting — a scheduled one here just fights the value the pipeline sets on its own. Not dangerous, just pointless.",
    CUSTOM_TITLE: 'These Scheduled Commands override earlier values:',
    CUSTOM_ADVICE:
      'Scheduled commands run during playback, so they come after your configs and after the Initial Commands — they are the last word on whatever they set, and the only place a value changes partway through a capture.',
    hazardRow: (command) => `${command} — runs during playback`,
    customOverridesInit: (cvar, value, previous) =>
      `${cvar} ${value} replaces ${previous}, set before the demo loads`,
    customOverridesConfig: (cvar, value, previous, source) =>
      `${cvar} ${value} replaces ${previous} from ${source}`,
    override: (cvar, initValue, cfgValue, file, line) =>
      `${cvar} ${initValue} replaces ${cfgValue} from ${file}, line ${line}`,
    DECAL_DEFAULT_TITLE: 'No r_decals value is set anywhere:',
    DECAL_DEFAULT_ADVICE:
      "The engine will use its default, 256, for the decal ring. That's a safe value on most maps — state r_decals in Initial Commands if you want a different one.",
    decalDefaultRow: (ring) => `r_decals — defaulting to ${ring}`,
    DECAL_NOOP_TITLE: 'Flush Decals Between Clips has nothing to clear:',
    DECAL_NOOP_ADVICE:
      "r_decals is 0, so the flush's sweep finds an empty ring every clip — real work for no effect. State a nonzero r_decals in Initial Commands, or turn off Flush Decals Between Clips in the Pipeline tab.",
    DECAL_NOOP_ROW: 'r_decals 0 — clears nothing',
    FATAL_TITLE: 'These config values will quit the game:',
    FATAL_ADVICE:
      "DoD's own client checks these whenever the HUD is on screen, and for most cvars it just forces the right value back silently. For these it also closes the game outright rather than merely correcting course. Nothing here changes your config files -- open the file named above and remove the line, or give it the value DoD requires. Setting it in Initial Commands instead is not a way round this: the app refuses these there, for the same reason.",
    fatalRow: (cvar, value, required, file, line) =>
      `${cvar} ${value} — DoD requires ${required}, set in ${file}, line ${line}`,
    NOOP_TITLE: 'These commands have no effect:',
    NOOP_ADVICE:
      'The pipeline (or the engine itself) always overrides or drops these before they could ever apply — not wrong, just wasted keystrokes.',
    NOOP_REASONS: {
      mirv_movie_filename: 'the pipeline sets this itself before every clip is captured',
      exec: 'GoldSrc drops this when injected into a demo',
      quit: 'GoldSrc drops this when injected into a demo',
    },
    noopRow: (command, reason, source) => {
      const isPlainSource = source === 'Initial Commands' || source === 'Scheduled Commands';
      return isPlainSource ? `${command} — ${reason}` : `${command} — ${reason} (${source})`;
    },
  },

  // Pre-roll and post-roll are load-bearing: playback returns to real time one
  // pre-roll before recording, so everything that must happen at normal speed
  // has to fit inside it.
  ROLLS: {
    BANNER_TITLE: 'These timings are shorter than this capture needs:',
    ADVICE:
      'Playback only returns to real time one pre-roll before recording starts. Anything that has to happen at normal speed — the engine flushing its audio buffers after the fast-forward, the decal sweep, a Scheduled Command — has to fit inside that window, or it happens while the engine is still racing through frames.',
    tooShort: (name, have, need, binding) =>
      `<code>${name} ${have.toFixed(1)}s</code> — needs at least <code>${need.toFixed(1)}s</code> for ${binding}`,
  },

  MAIN: {
    SELECT_CAPTURE_OUTPUT_DIR_TITLE: 'Select Capture Output Directory',
    SELECT_RENDER_EXPORT_DIR_TITLE: 'Select Render Export Directory',
    SAVE_PROJECT_SESSION_TITLE: 'Save Studio Project Session',
    SELECT_HLAE_EXE_TITLE: 'Select HLAE Executable (hlae.exe)',
    SELECT_HL_EXE_TITLE: 'Select Half-Life Executable (hl.exe)',
    SELECT_FFMPEG_EXE_TITLE: 'Select FFmpeg Executable (ffmpeg.exe)',
    SELECT_GOLDSRC_HOOKS_DLL_TITLE: 'Select GoldSrc Hooks DLL (goldsrc_hooks.dll)',
    SELECT_OBS_EXE_TITLE: 'Select OBS Executable (obs64.exe)',
    SELECT_DEMO_FILES_TITLE: 'Select Demo Files (.dem)',
    SELECT_DEMO_FOLDER_TITLE: 'Select Demo Folder',

    JSON_PROJECT_FILTER_NAME: 'JSON Project File',
    EXECUTABLE_FILTER_NAME: 'Executable',
    DLL_FILTER_NAME: 'DLL',
    CONFIG_FILTER_NAME: 'Config File',
    SELECT_MOVIE_CFG_TITLE: 'Select Movie Config',
    DEMO_FILES_FILTER_NAME: 'Demo Files',

    NOTHING_TO_SAVE: 'Nothing to save yet — add demo files or load a session first.',
    ALREADY_SAVED: 'Already saved — no changes since the last save.',
    projectSavedToast: (path) => `Project session saved successfully to ${path}`,
    NEW_SESSION_TOAST: 'Started a new session.',
    SAVE_PROJECT_ERROR: 'Error saving project session.',
    loadedDemosToast: (count) => `Loaded ${count} demos from project file`,
    LOAD_PROJECT_ERROR: 'Error loading project session.',

    cancelledStatus: (count) => `Status: Cancelled — ${count} demo(s) found before cancel`,
    readyFoundStatus: (count) => `Status: Ready — ${count} demo(s) found`,
    statusGeneric: (status) => `Status: ${status}`,
    SCAN_CANCEL_REQUESTED_TOAST: 'Scan cancellation requested.',
    SCANNING_STATUS: 'Status: Scanning...',
    SCANNING_TOAST: 'Scanning directories...',
    SCANNING_PLEASE_WAIT_ROW: 'Scanning... please wait.',
    scanCompleteToast: (count) => `Scan complete (${count} demo(s) found)`,
    scanErrorToast: (err) => `Error: ${err}`,
    scanErrorStatus: (err) => `Status: Error — ${err}`,

    EXPORT_POOL_FREE_DEFAULT: 'Capture Output Free: 0.0 GB',
    exportPoolFree: (gb) => `Capture Output Free: ${gb} GB`,
    EXPORT_POOL_ERROR: 'Capture Output Free: Error calculating space',

    QUEUE_ALREADY_EMPTY: 'Queue is already empty.',
    NO_DEMOS_MATCH_SEARCH: 'No demos match the current search.',
    filterScopeNote: (visible, total) => ` (search filter active — only considered ${visible} of ${total} demo(s) in the queue)`,
    NOTHING_TRACKED_TO_CLEAR: 'Nothing to clear — every visible demo has tracked work on it.',
    removedUntrackedToast: (count, keptNote, scopeNote) => `Removed ${count} untracked demo(s)${keptNote}.${scopeNote}`,
    keptWithTrackedWork: (count) => `, kept ${count} with tracked work`,
    clearUntrackedLog: (count, keptNote, scopeNote, names) => `[queue] Clear Untracked: removed ${count} demo(s)${keptNote}.${scopeNote} — ${names}`,

    CLEAR_SELECTED_TITLE: 'Clear Selected Demos',
    CLEAR_ALL_TITLE: 'Clear All Demos',
    REMOVE_TRACKED_DEMO_TITLE: 'Remove Tracked Demo',
    CLEAR_SELECTED_ANYWAY: 'Clear Selected Anyway',
    CLEAR_ALL_ANYWAY: 'Clear All Anyway',
    REMOVE_ANYWAY: 'Remove Anyway',
    CLEAR_ANYWAY_DEFAULT: 'Clear Anyway',
    DEMO_SINGULAR: 'demo',
    DEMO_PLURAL: 'demos',
    VERB_REMOVES: 'removes',
    clearSummaryTracked: (verb, count, plural, trackedCount) => `This ${verb} ${count} ${plural} — ${trackedCount} of them have tracked work (Captured/Rendered status, a note, or an edited kill range) that will be lost. This cannot be undone.`,
    clearSummaryUntracked: (verb, count, plural) => `This ${verb} ${count} ${plural}. None currently have tracked work on them. This cannot be undone.`,

    NO_DEMOS_SELECTED: 'No demos selected — check rows in the queue first.',
    allSelectedHiddenToast: (count) => `All ${count} selected demo(s) are hidden by the current search — nothing visible to remove.`,
    hiddenCheckedNote: (count) => ` (${count} other selected demo(s) hidden by the search filter were left untouched)`,
    removeSelectedConfirm: (count, hiddenNote) => `Remove ${count} selected demo(s) from the queue?${hiddenNote}`,
    removedSelectedToast: (savedFirst, count, hiddenNote) => `${savedFirst ? 'Saved, then removed' : 'Removed'} ${count} demo(s) from the queue.${hiddenNote}`,
    clearSelectedLog: (count, savedNote, hiddenNote, names) => `[queue] Clear Selected: removed ${count} demo(s)${savedNote}.${hiddenNote} — ${names}`,
    removeAllConfirm: (count, note) => `Remove ${count} demo(s) from the queue? None have tracked work on them.${note}`,
    clearedAllToast: (savedFirst, count, note) => `${savedFirst ? 'Saved, then cleared' : 'Cleared'} ${count} demo(s) from the queue.${note}`,
    clearAllLog: (count, savedNote, note, names) => `[queue] Clear All: removed ${count} demo(s)${savedNote}.${note} — ${names}`,
    SAVED_SESSION_FIRST_NOTE: ' (saved session first)',
  },

  // ── list_editor.js: shared row-list widget (Browse/Move/Remove) ─────────
  LIST_EDITOR: {
    BROWSE_TITLE: 'Browse…',
    MOVE_UP_TITLE: 'Move up',
    MOVE_DOWN_TITLE: 'Move down',
    REMOVE_TITLE: 'Remove',
    REMOVE_ARIA_LABEL: 'Remove',
  },

  // ── ipc_bridge.js: error-toast prefixes wrapping backend errors ─────────
  IPC: {
    scanError: (err) => `Scan error: ${err}`,
    validationError: (err) => `Validation error: ${err}`,
    analysisError: (err) => `Analysis error: ${err}`,
    previewFailed: (err) => `Preview failed: ${err}`,
    cfgImportFailed: (err) => `Could not read that config: ${err}`,
    processCheckFailed: (err) => `Process check failed: ${err}`,
    launchFailed: (err) => `Launch failed: ${err}`,
    killEngineFailed: (err) => `Failed to close running engine processes: ${err}`,
    batchPreviewFailed: (err) => `Batch preview generation failed: ${err}`,
    cancelScanError: (err) => `Cancel scan error: ${err}`,
    settingsLoadFailed: (err) => `Failed to load settings: ${err}`,
    settingsSaveFailed: (err) => `Failed to save settings: ${err}`,
    auditFailed: (err) => `Audit failed: ${err}`,
    deletionFailed: (err) => `Deletion failed: ${err}`,
    cancelAuditError: (err) => `Cancel audit error: ${err}`,
    folderOpenFailed: (err) => `Could not open folder: ${err}`,
    previewScanFailed: (err) => `Preview scan failed: ${err}`,
    previewDeletionFailed: (err) => `Preview deletion failed: ${err}`,
    logFileOpenFailed: (err) => `Could not open the log file: ${err}`,
    updateCheckFailed: (err) => `Update check failed: ${err}`,
    updateInstallFailed: (err) => `Update install failed: ${err}`,
  },

  // ── error_reporter.js: the one user-facing crash toast ───────────────────
  ERROR_REPORTER: {
    somethingWentWrong: (message) => `Something went wrong (${message}). Details logged to crash_log.md.`,
  },

  // ── Footer ────────────────────────────────────────────────────────────
  FOOTER: {
    VIEW_LOGS_BUTTON: 'View Logs',
    VIEW_LOGS_TITLE: "Open today's activity log in Explorer",
    CHECK_UPDATES_BUTTON: 'Check for Updates',
    CHECK_UPDATES_TITLE: 'Check for app updates',
    UPDATE_AVAILABLE_BUTTON: 'Update Available',
    UPDATE_AVAILABLE_TITLE: 'A new version is ready — click to install it.',
  },

  // ── About modal (issue #122) ─────────────────────────────────────────────
  ABOUT_MODAL: {
    BLURB: 'A capture, patching, and analytics pipeline for Day of Defeat 1.3 demos — batch-records highlight clips through HLAE and parses matches for scoreboards, kills, chat, and rounds.',
    CREDIT: 'Built by ccoventry',
    GITHUB_LINK: 'github.com/ccoventry/dod-tools',
  },

  // ── Update check/install modal (issue #133) ──────────────────────────────
  UPDATE_MODAL: {
    TITLE: 'Check for Updates',
    currentVersionLabel: (v) => `Current version: v${v}`,
    CHANNEL_LABEL: 'Update Channel:',
    CHANNEL_STABLE: 'Stable (from main)',
    CHANNEL_EXPERIMENTAL: 'Experimental (from dev, on-demand builds)',
    AUTO_CHECK_LABEL: 'Automatically Check on Startup',
    NOTIFY_LABEL: 'OS Notification on Found',
    NOTIFY_TITLE: 'Fires an OS toast when a background or manual check finds a newer version.',
    CHECK_NOW_BUTTON: 'Check for Updates Now',
    DOWNLOAD_INSTALL_BUTTON: 'Download & Install',
    RESTART_BUTTON: 'Restart to Apply',
    CLOSE_BUTTON: 'Close',
    STATUS_CHECKING: 'Checking for updates…',
    STATUS_UP_TO_DATE: 'Up to date.',
    STATUS_AVAILABLE: (v) => `Update available: v${v}`,
    STATUS_DOWNLOADING: 'Downloading update…',
    STATUS_READY: 'Update downloaded — restart to apply.',
    STATUS_CHECK_FAILED: (err) => `Update check failed: ${err}`,
  },

  // ── OS Toast Notifications (issue #98) ──────────────────────────────────
  // Titles/bodies for os_notifications.js's notify() calls. Bodies reuse the
  // existing CAPTURE/RENDER in-app-toast strings where the wording already
  // fits, rather than duplicating near-identical copy.
  NOTIFICATIONS: {
    CAPTURES_DONE_TITLE: 'Captures complete',
    CAPTURES_ERROR_TITLE: 'Capture error',
    RENDERS_DONE_TITLE: 'Renders complete',
    RENDERS_ERROR_TITLE: 'Render errors',
    patchingStartedTitle: (total) => `Patching ${total} demo${total === 1 ? '' : 's'}`,
    PATCHING_STARTED_BODY: 'Preparing demos for capture…',
    PATCHING_FINISHED_TITLE: 'Patching complete',
    patchingFinishedBody: (total) => `${total} demo${total === 1 ? '' : 's'} ready — starting capture`,
    // Shared by both the demo-loading and fast-forward-to-clip toasts —
    // same demo, same title, whichever of the two actually fires.
    demoLoadingTitle: (index, total) => `Viewing demo ${index} of ${total}`,
    demoLoadingBody: (clipCount, clipsSoFar, totalBatchClips) => {
      const clipWord = clipCount === 1 ? 'clip' : 'clips';
      const onThisDemo = `${clipCount} ${clipWord} on this demo`;
      return totalBatchClips ? `${onThisDemo} · ${clipsSoFar} of ${totalBatchClips} clips total` : onThisDemo;
    },
    fastForwardToClipBody: (clipIndex, clipCountThisDemo, clipsSoFar, totalBatchClips) => {
      const onThisDemo = `Fast-forwarding to clip ${clipIndex} of ${clipCountThisDemo}`;
      return totalBatchClips ? `${onThisDemo} · ${clipsSoFar} of ${totalBatchClips} clips total` : onThisDemo;
    },
  },
};
