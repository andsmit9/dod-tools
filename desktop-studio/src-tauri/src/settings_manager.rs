use crate::capture_manager::CustomCommandPayload;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub hlae_path: String,
    pub hl_path: String,
    pub ffmpeg_path: Option<String>,
    pub pinned_folders: Vec<String>,
    /// Analyzer sidebar's "Recent" quick-links tier — most-recent-first,
    /// capped at 10, pushed whenever a folder selection yields a non-empty
    /// demo listing. Mirrors dev's `settings.demo_folder_history`.
    #[serde(default)]
    pub demo_folder_history: Vec<String>,
    /// Gates the Demo Analyzer Explorer tree's per-subfolder "(N)" demo-count
    /// badge (Quick Links counts are always shown, matching dev — see
    /// docs/archive/tauri_parity_audit.md Area 1). Defaults to `false`, matching
    /// dev's own `AppSettings::default()`.
    #[serde(default)]
    pub scan_folders_for_demos: bool,
    /// Demo Analyzer Explorer sidebar's drag-to-resize width, in pixels.
    #[serde(default = "default_analyzer_explorer_width")]
    pub analyzer_explorer_width: i32,
    pub language: String,
    pub capture_fps: i32,
    /// OBS mode's own capture rate — see `PatcherConfig::obs_capture_fps`'s
    /// doc comment for why this is a separate field rather than sharing
    /// `capture_fps` with frame-sequence/direct-to-video.
    #[serde(default = "default_obs_capture_fps")]
    pub obs_capture_fps: i32,
    pub pre_roll_seconds: f32,
    pub post_roll_seconds: f32,
    #[serde(default = "default_resolution_width")]
    pub resolution_width: i32,
    #[serde(default = "default_resolution_height")]
    pub resolution_height: i32,
    /// Whether the pipeline sweeps the decal ring between clips.
    ///
    /// Distinct from `r_decals` in `init_commands`, which says how many decals
    /// the engine keeps. Off here means "capture as the engine would, bullet
    /// holes and all" — which `r_decals 0` cannot express, since that turns
    /// decals off entirely and changes how the capture looks.
    ///
    /// Defaults on, including for settings files written before it existed.
    #[serde(default = "default_decal_flush")]
    pub decal_flush: bool,
    /// Whether HLAE pipes frames to FFmpeg instead of writing a BMP sequence.
    #[serde(default)]
    pub ffmpeg_capture: bool,
    /// Codec id for direct-to-video capture. Stored as the string id rather
    /// than the enum so a settings file naming a codec this build does not
    /// know still loads — `CaptureCodec::from_str_id` falls back to the
    /// default instead of failing the whole file.
    #[serde(default = "default_capture_codec")]
    pub ffmpeg_capture_codec: String,
    /// How this batch records: `frame_sequence`, `direct_to_video` or `obs`.
    ///
    /// Stored as the string id for the same reason the codec is — a settings
    /// file naming a mode this build does not know loads and degrades to the
    /// path that always works, rather than failing the whole file.
    ///
    /// A file written before this existed has no value here and only
    /// `ffmpeg_capture`; `PatcherConfig::normalise_capture_mode` promotes that,
    /// so an upgrade keeps whatever the user had selected.
    #[serde(default = "default_capture_mode")]
    pub capture_mode: String,
    /// obs-websocket host, when `capture_mode` is `obs`.
    #[serde(default = "default_obs_host")]
    pub obs_host: String,
    #[serde(default = "default_obs_port")]
    pub obs_port: u16,
    /// The user's obs-websocket password. Empty when OBS has authentication
    /// switched off. Never logged — see `ObsConfig::redacted`.
    #[serde(default)]
    pub obs_password: String,
    /// Path to `obs64.exe`/`obs.exe`, for the "Launch OBS" button. Spawn-and-
    /// forget, same as `launch_standalone_game` — OBS is the user's own
    /// software, not ours to manage, so nothing here tracks its lifecycle.
    #[serde(default)]
    pub obs_exe_path: String,
    #[serde(default)]
    pub auto_clear_logs: bool,
    #[serde(default)]
    pub auto_clear_previews: bool,
    #[serde(default)]
    pub auto_clear_temp_demos: bool,
    #[serde(default)]
    pub record_start_lead: f32,
    #[serde(default)]
    pub record_stop_trail: f32,
    #[serde(default = "default_initial_delay")]
    pub initial_delay: f32,
    #[serde(default = "default_fast_forward_speed")]
    pub fast_forward_speed: f32,
    /// Capture Output pool — required, no fallback (see capture_pane.js's
    /// refreshLaunchGuard/buildCapturePayload). Doubles as Render Studio's
    /// scan-input locations (see render_pane.js's initRenderUI call) — the
    /// two used to be separately-configured lists (`render_folders`) until
    /// that redundancy was found to leave any capture written to a drive
    /// only in this list invisible to Render Studio's scan.
    #[serde(default)]
    pub target_drives: Vec<String>,
    #[serde(default)]
    pub init_commands: Vec<String>,
    #[serde(default)]
    pub custom_commands: Vec<CustomCommandPayload>,
    #[serde(default)]
    pub save_local_patched_copy: bool,
    #[serde(default = "default_render_codec")]
    pub render_codec: String,
    #[serde(default = "default_render_fps")]
    pub render_fps: i32,
    #[serde(default = "default_render_max_concurrent")]
    pub render_max_concurrent: i32,
    /// JIT multi-drive export pool for Render Studio.
    #[serde(default)]
    pub render_export_dirs: Vec<String>,
    /// OS toast notification toggles (issue #98) — each stage independently
    /// switchable, not one global mute.
    #[serde(default = "default_notify_patching")]
    pub notify_patching: bool,
    #[serde(default = "default_notify_demo_loading")]
    pub notify_demo_loading: bool,
    #[serde(default = "default_notify_between_clips")]
    pub notify_between_clips: bool,
    #[serde(default = "default_notify_captures_done")]
    pub notify_captures_done: bool,
    #[serde(default = "default_notify_renders_done")]
    pub notify_renders_done: bool,
    #[serde(default = "default_notify_error")]
    pub notify_error: bool,
    #[serde(default = "default_notify_updates")]
    pub notify_updates: bool,
    /// Which release channel `check_for_update` polls: `"stable"` (built from
    /// `main`) or `"experimental"` (built from `dev`, on-demand). See issue #133.
    #[serde(default = "default_update_channel")]
    pub update_channel: String,
    #[serde(default = "default_auto_check_updates")]
    pub auto_check_updates: bool,
}

fn default_resolution_width() -> i32 { 1280 }
fn default_obs_capture_fps() -> i32 { 120 }
fn default_resolution_height() -> i32 { 720 }
fn default_initial_delay() -> f32 { 3.0 }
fn default_fast_forward_speed() -> f32 { 0.05 }
fn default_render_codec() -> String { "prores".to_string() }
fn default_render_fps() -> i32 { 300 }
fn default_render_max_concurrent() -> i32 { 2 }
fn default_analyzer_explorer_width() -> i32 { 260 }
fn default_decal_flush() -> bool { true }
fn default_capture_codec() -> String {
    native::patch::CaptureCodec::default().to_str_id().to_string()
}
fn default_capture_mode() -> String {
    native::patch::CaptureMode::default().to_str_id().to_string()
}
fn default_obs_host() -> String { "127.0.0.1".to_string() }
fn default_obs_port() -> u16 { 4455 }
fn default_notify_patching() -> bool { true }
fn default_notify_demo_loading() -> bool { true }
fn default_notify_between_clips() -> bool { true }
fn default_notify_captures_done() -> bool { true }
fn default_notify_renders_done() -> bool { true }
fn default_notify_error() -> bool { true }
fn default_notify_updates() -> bool { true }
fn default_update_channel() -> String { "stable".to_string() }
fn default_auto_check_updates() -> bool { true }

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            hlae_path: String::new(),
            hl_path: String::new(),
            ffmpeg_path: None,
            pinned_folders: Vec::new(),
            demo_folder_history: Vec::new(),
            scan_folders_for_demos: false,
            analyzer_explorer_width: default_analyzer_explorer_width(),
            language: "en".to_string(),
            capture_fps: 300,
            obs_capture_fps: default_obs_capture_fps(),
            pre_roll_seconds: 2.0,
            post_roll_seconds: 0.6,
            resolution_width: default_resolution_width(),
            resolution_height: default_resolution_height(),
            decal_flush: default_decal_flush(),
            ffmpeg_capture: false,
            ffmpeg_capture_codec: default_capture_codec(),
            capture_mode: default_capture_mode(),
            obs_host: default_obs_host(),
            obs_port: default_obs_port(),
            obs_password: String::new(),
            obs_exe_path: String::new(),
            auto_clear_logs: false,
            auto_clear_previews: false,
            auto_clear_temp_demos: false,
            record_start_lead: 0.0,
            record_stop_trail: 0.0,
            initial_delay: default_initial_delay(),
            fast_forward_speed: default_fast_forward_speed(),
            target_drives: Vec::new(),
            // Seeded on first run only. `Default` is reached when there is no
            // settings file at all, so nobody's saved init commands are ever
            // appended to — silently changing the FOV or the decal ring of a
            // capture someone had already configured would be a far worse
            // trade than a new user having to discover these two lines exist.
            //
            // Both are values the pipeline reads back: `r_decals` states the
            // decal ring the flush sizes its sweep to, and `mirv_fov` states
            // the FOV the on-screen test is derived from. 90 is the engine's
            // own default and the right starting point; change it here and the
            // flush follows.
            init_commands: vec!["r_decals 256".to_string(), "mirv_fov 90".to_string()],
            custom_commands: Vec::new(),
            save_local_patched_copy: false,
            render_codec: default_render_codec(),
            render_fps: default_render_fps(),
            render_max_concurrent: default_render_max_concurrent(),
            render_export_dirs: Vec::new(),
            notify_patching: default_notify_patching(),
            notify_demo_loading: default_notify_demo_loading(),
            notify_between_clips: default_notify_between_clips(),
            notify_captures_done: default_notify_captures_done(),
            notify_renders_done: default_notify_renders_done(),
            notify_error: default_notify_error(),
            notify_updates: default_notify_updates(),
            update_channel: default_update_channel(),
            auto_check_updates: default_auto_check_updates(),
        }
    }
}

pub fn settings_path() -> PathBuf {
    let config_dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    let dir = config_dir.join("dod-tools");
    if let Err(e) = fs::create_dir_all(&dir) {
        log::warn!("Failed to create settings directory {:?}: {}", dir, e);
    }
    dir.join("settings.json")
}

impl AppSettings {
    pub fn load_or_default() -> Self {
        let path = settings_path();
        if path.exists()
            && let Ok(content) = fs::read_to_string(&path)
                && let Ok(settings) = serde_json::from_str::<AppSettings>(&content) {
                    return settings;
                }
        Self::default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = settings_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(crate::messages::failed_to_serialize_settings)?;
        fs::write(&path, json)
            .map_err(|e| crate::messages::failed_to_write_settings_file(&path, e))?;
        Ok(())
    }
}

pub struct SettingsManager {
    pub inner: Arc<Mutex<AppSettings>>,
}

impl SettingsManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(AppSettings::load_or_default())),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A settings.json saved by a pre-2026-08-18 build still has an
    /// `allocation_strategy` key on disk (the field was removed from
    /// `AppSettings` when the Chronological drive-allocation strategy was
    /// deleted). Since `AppSettings` doesn't set `#[serde(deny_unknown_fields)]`,
    /// serde must silently ignore the stale key rather than fail deserialization
    /// — otherwise every existing user's settings would revert to defaults on
    /// their next launch after upgrading.
    #[test]
    fn test_deserialize_ignores_legacy_allocation_strategy_field() {
        let legacy_json = r#"{
            "hlae_path": "C:/hlae/hlae.exe",
            "hl_path": "C:/dod/hl.exe",
            "ffmpeg_path": null,
            "pinned_folders": [],
            "language": "en",
            "capture_fps": 300,
            "pre_roll_seconds": 2.0,
            "post_roll_seconds": 0.6,
            "allocation_strategy": "Chronological"
        }"#;

        let settings: AppSettings = serde_json::from_str(legacy_json)
            .expect("legacy allocation_strategy field must not break deserialization");

        assert_eq!(settings.hlae_path, "C:/hlae/hlae.exe");
        assert_eq!(settings.capture_fps, 300);
    }

    #[test]
    fn test_default_settings_serialize_and_deserialize_roundtrip() {
        let original = AppSettings::default();
        let json = serde_json::to_string(&original).expect("default settings must serialize");
        let restored: AppSettings =
            serde_json::from_str(&json).expect("serialized default settings must deserialize");

        assert_eq!(restored.language, original.language);
        assert_eq!(restored.capture_fps, original.capture_fps);
        assert_eq!(restored.fast_forward_speed, original.fast_forward_speed);
        assert_eq!(restored.render_codec, original.render_codec);
    }
}
