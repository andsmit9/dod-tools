// patch/types.rs
// Pure data layer: structs, enums, and their direct impl blocks.
// No file I/O, no thread spawning. All types except PatchEvent and
// CaptureWorker are WASM-safe.

use std::sync::{Arc, atomic::AtomicBool};

pub const MAX_PAYLOAD_LIMIT_BYTES: usize = 2_097_152;
pub const MAX_PAYLOAD_SIZE: usize = MAX_PAYLOAD_LIMIT_BYTES;

// ── Direct-to-video capture codec ─────────────────────────────────────────────

/// What `mirv_movie_ffmpeg` encodes to when direct-to-video capture is on.
///
/// **Lossless only, and RGB/4:4:4 only.** Two constraints, both load-bearing:
///
/// - The render pass is not optional — audio is a separate `sound.wav` that has
///   to be muxed, Separate HUD needs an `alphamerge`, and export routing happens
///   there. So a capture is always an intermediate that gets re-encoded, and a
///   lossy capture codec would bake in artefacts for no benefit.
/// - `hudAlpha` carries the HUD matte in its **red channel** and the renderer
///   reads it with `extractplanes=r`. Any chroma-subsampled codec would destroy
///   that silently — garbage HUD edges, no error anywhere.
///
/// Sizes measured on 3s of real footage at 1280x720/120fps, against the ~995 MB
/// BMP sequence being replaced. **These are transcode figures with every core
/// free, not capture-time.** HLAE pipes frames live, so an encoder that cannot
/// keep up slows the capture instead of failing, and the size ranking is
/// probably close to the inverse of the real-time-viability ranking.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[derive(Default)]
pub enum CaptureCodec {
    /// Built for real-time capture: fast, multithreaded, simple prediction.
    /// 486 MB (0.49x). The default, and the only one proven in a real capture.
    #[default]
    UtVideo,
    /// Smaller (420 MB, 0.42x) but a range coder with context modelling — far
    /// heavier per frame, and competing with `hl.exe` for cores during capture.
    Ffv1,
    /// Smallest lossless option at 267 MB (0.27x), and the heaviest to encode.
    X264Lossless,
    /// No compression — the same ~995 MB as the BMP sequence, and ~829 MB/s of
    /// disk write at 300fps/720p, past a SATA SSD's ceiling. Kept as the
    /// zero-CPU option for a machine that is CPU-bound rather than disk-bound.
    RawVideo,
}

impl CaptureCodec {
    /// The `-c:v ...` fragment placed at the head of the `mirv_movie_ffmpeg`
    /// options string.
    ///
    /// `-pix_fmt gbrp` is explicit on every entry rather than left to FFmpeg's
    /// negotiation. HLAE feeds `bgr24`, and the automatic choice happens to land
    /// on `gbrp` for utvideo today — but "happens to" is what silently destroys
    /// the HUD alpha the day it stops being true.
    pub fn args(self) -> &'static str {
        match self {
            Self::UtVideo => "-c:v utvideo -pix_fmt gbrp",
            Self::Ffv1 => "-c:v ffv1 -level 3 -pix_fmt gbrp",
            Self::X264Lossless => "-c:v libx264 -qp 0 -pix_fmt gbrp",
            Self::RawVideo => "-c:v rawvideo -pix_fmt bgr24",
        }
    }

    /// Round-trips with `from_str_id` so a persisted setting parses back
    /// without a second, drifting mapping — same contract as `RenderCodec`.
    pub fn to_str_id(self) -> &'static str {
        match self {
            Self::UtVideo => "utvideo",
            Self::Ffv1 => "ffv1",
            Self::X264Lossless => "x264_lossless",
            Self::RawVideo => "rawvideo",
        }
    }

    /// Unrecognised ids fall back to the default rather than failing: a settings
    /// file naming a codec this build does not have should not stop a capture.
    pub fn from_str_id(id: &str) -> Self {
        match id {
            "ffv1" => Self::Ffv1,
            "x264_lossless" => Self::X264Lossless,
            "rawvideo" => Self::RawVideo,
            _ => Self::UtVideo,
        }
    }
}


// ── Command scheduling ────────────────────────────────────────────────────────

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct CustomCommand {
    pub command: String,
    pub offset: f32,
    pub relation: CommandRelation,  
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum CommandRelation {
    Before,
    After,
}

// ── High-level patch options (used by the dem-crate API path) ─────────────────

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct PatchOptions {
    pub exit_on_finish: bool,
    pub init_commands: Vec<String>,
    pub custom_commands: Vec<CustomCommand>,
    pub fast_forward_speed: Option<f32>,
    pub hltv_spec_player: Option<String>,
    pub initial_delay: Option<f32>,
    pub pre_record_buffer: Option<f32>,
    pub record_start_lead: Option<f32>,
    pub record_stop_trail: Option<f32>,
    pub post_record_buffer: Option<f32>,
    pub player_deaths: Option<Vec<f32>>,
}

// ── Capture result types ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum HighlightStatus {
    #[default]
    None,
    Pending,
    Captured,
    Rendered,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CaptureStreak {
    pub start_tick: i32,
    pub end_tick: i32,
    pub source_demo: String,
    pub target_player: Option<String>,
    pub kill_count: usize,
    pub timeline_string: String,
    pub duration_string: String,
    pub player_index: usize,
    /// Raw kill events: (tick, abs_time_secs, weapon). Stored so update_visuals
    /// can rebuild timeline_string from any sub-slice without needing frame_times.
    pub kills: Vec<(i32, f32, String)>,
    pub start_index: usize,
    pub end_index: usize,
    pub total_demo_frames: i32,
    pub demo_fps: f32,
    #[serde(default)]
    pub viewdemo_times: Vec<f32>,
    #[serde(skip, default)]
    pub frame_times: Arc<Vec<f32>>,
    #[serde(default)]
    pub status: HighlightStatus,
    #[serde(default)]
    pub match_start_tick: Option<i32>,
}

impl CaptureStreak {
    /// Rebuilds `timeline_string`, `duration_string`, `kill_count`, `start_tick`,
    /// and `end_tick` from `kills[start_index..=end_index]`. Must be called after
    /// any mutation of `start_index` or `end_index`.
    pub fn update_visuals(&mut self) {
        if self.kills.is_empty() {
            return;
        }
        let end = self.end_index.min(self.kills.len().saturating_sub(1));
        let start = self.start_index.min(end);
        let slice = &self.kills[start..=end];

        self.start_tick = slice[0].0;
        self.end_tick = slice[slice.len() - 1].0;
        self.kill_count = slice.len();

        let total_secs = (slice.last().unwrap().1 - slice[0].1).max(0.0).round() as i32;
        self.duration_string = format!("{}:{:02}", total_secs / 60, total_secs % 60);

        let mut parts: Vec<String> = Vec::with_capacity(slice.len());
        for (i, (_, abs_time, weapon)) in slice.iter().enumerate() {
            let weapon_clean = weapon.trim_start_matches("Weapon::").to_string();
            if i == 0 {
                parts.push(weapon_clean);
            } else {
                let gap_sec = (abs_time - slice[i - 1].1).max(0.0).round() as i32;
                parts.push(format!("(+{}:{:02}) {}", gap_sec / 60, gap_sec % 60, weapon_clean));
            }
        }
        self.timeline_string = parts.join(", ");
    }
}

/// One recording block — the unit HLAE actually writes to disk as a take folder.
///
/// Distinct from a highlight: `build_batch_queue` merges highlights that overlap
/// (once pre/post-roll is applied) into a single continuous recording, so one
/// block can be the source of several highlight rows. `source_streak_indices`
/// carries that fan-out, indexing into the `raw_streaks` slice the caller passed
/// in — i.e. positions in the dispatched capture payload.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CaptureBlock {
    /// Chained demo name this block belongs to, e.g. `chain_01`.
    pub demo_name: String,
    /// Index into the job's merged blocks; matches the `_route_{N}` alias and
    /// the `_b{N}` suffix on the take folder.
    pub block_index: usize,
    pub drive_index: usize,
    /// Absolute path HLAE is expected to write this take to.
    pub take_folder: std::path::PathBuf,
    /// `session_id/chain_JJ_bN` — see `crate::shared::paths::take_key`.
    pub take_key: String,
    pub source_streak_indices: Vec<usize>,
    pub start_tick: i32,
    pub end_tick: i32,
    /// Frame ordinal `sys_record_start` fires on — the first frame that lands
    /// in the take. Distinct from `start_tick`, which is the highlight's own
    /// bound and carries neither the record lead nor the pre-roll. The decal
    /// flush keys off these two: everything outside them is scrubbed.
    #[serde(default)]
    pub record_start_tick: i32,
    /// Frame ordinal `sys_record_stop` fires on, after the record trail and
    /// any end-of-demo clamp.
    #[serde(default)]
    pub record_stop_tick: i32,
}

#[derive(Debug, Clone)]
pub struct PatchJob {
    pub source_demo: String,
    pub output_demo: std::path::PathBuf,
    pub streaks: Vec<CaptureStreak>,
    pub target_player: Option<String>,
    pub init_commands: Vec<String>,
    pub scheduled_commands: Vec<(i32, String)>,
    /// (tick, label) pairs — each becomes a named `svc_director` STUFFTEXT event
    /// in the `viewdemo` Event List labelled "<N> kills: <timeline_string>".
    pub director_events: Vec<(i32, String)>,
    pub block_routes: Vec<(i32, i32, usize)>,
    /// Where each block's frames will land, and which payload highlights it
    /// covers. Empty for the primer job and for preview-only jobs.
    pub blocks: Vec<CaptureBlock>,
}

/// One drive `build_batch_queue`'s AOT allocation touched, and the headroom
/// figure `capture_engine`'s pre-launch check must re-validate before
/// spawning `hl.exe`.
#[derive(Debug, Clone)]
pub struct DriveHeadroom {
    pub path: std::path::PathBuf,
    pub free_bytes: u64,
}

// ── Capture mode ──────────────────────────────────────────────────────────────

/// How a batch's frames get off the screen and onto disk.
///
/// One enum rather than a pair of booleans, because the three are mutually
/// exclusive and a `ffmpeg_capture && obs_capture` state has no meaning — the
/// type is the place to make that unrepresentable rather than a validation step
/// somebody can forget to call.
///
/// See `docs/direct_to_video_capture.md` (#42) and
/// `docs/obs_alternate_capture.md` (#65).
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum CaptureMode {
    /// HLAE writes a numbered BMP per frame. Deterministic, frame-exact, and by
    /// far the most disk-hungry — a 17-block batch at 120fps with separate HUD
    /// measured on the order of 300 GB.
    #[default]
    FrameSequence,
    /// HLAE pipes frames to an FFmpeg it spawns itself (`mirv_movie_ffmpeg`).
    /// Same determinism, one video file per stream instead of the sequence.
    DirectToVideo,
    /// OBS records the game window in real time and dod-tools only tells it
    /// when to start and stop. HLAE records nothing at all in this mode.
    ///
    /// Not deterministic and not capable of high frame rates — it captures
    /// whatever the screen actually showed. In exchange the file is finished
    /// and playable the moment the clip ends, audio already muxed.
    Obs,
}

impl CaptureMode {
    pub fn to_str_id(self) -> &'static str {
        match self {
            Self::FrameSequence => "frame_sequence",
            Self::DirectToVideo => "direct_to_video",
            Self::Obs => "obs",
        }
    }

    /// Unknown ids fall back to the safe default rather than erroring: a
    /// settings file from a newer build naming a mode this one does not have
    /// should degrade to the path that always works, not refuse to load.
    pub fn from_str_id(id: &str) -> Self {
        match id {
            "direct_to_video" => Self::DirectToVideo,
            "obs" => Self::Obs,
            _ => Self::FrameSequence,
        }
    }

    /// Whether HLAE is doing the recording. False only for OBS, where
    /// `mirv_recordmovie` is never issued and the engine simply plays back.
    pub fn hlae_records(self) -> bool {
        !matches!(self, Self::Obs)
    }
}

/// Minimum gap between one take's stop and the next one's start, for OBS.
///
/// `builder::MIN_TAKE_SEPARATION_SECONDS` is 1.0 s and was derived for HLAE as
/// a deliberately conservative guess, because the demo side cannot observe how
/// long HLAE needs to finalise a take. The OBS figure is not a guess: OBS took
/// **~1.065 s** to finalise a file after `StopRecord` returned, measured
/// repeatedly — so the existing guard is already too tight for this path.
///
/// 2.0 s rather than 1.1 s because the measurement is one machine writing one
/// container, the cost of being wrong is a lost take, and the cost of being
/// generous is a second of connective footage inside a merged clip.
///
/// Lives here rather than beside the client so the patcher can read it on every
/// target — `patch` compiles for wasm32, `obs` does not.
pub const OBS_TAKE_SEPARATION_SECONDS: f32 = 2.0;

/// Where OBS is and how to talk to it.
///
/// Only read when `capture_mode` is `Obs`. The password is the user's own
/// obs-websocket secret; it is stored with the rest of the settings and never
/// logged — see `ObsConfig::redacted` for the form that goes into log lines.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ObsConfig {
    pub host: String,
    pub port: u16,
    /// Empty when OBS has authentication switched off.
    pub password: String,
}

impl Default for ObsConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 4455,
            password: String::new(),
        }
    }
}

impl ObsConfig {
    pub fn address(&self) -> String {
        format!("ws://{}:{}", self.host, self.port)
    }

    /// Safe to log. The password is reduced to whether one is set at all,
    /// which is the only part of it worth knowing from a log line. No scene/
    /// profile to report any more — `obs::provision` always targets its own
    /// fixed, named profile/scene rather than a configurable one.
    pub fn redacted(&self) -> String {
        format!(
            "ws://{}:{} (password: {})",
            self.host,
            self.port,
            if self.password.is_empty() { "none" } else { "set" },
        )
    }
}

// ── Patcher configuration ─────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PatcherConfig {
    pub pre_roll_ticks: i32,
    pub post_roll_ticks: i32,
    pub capture_fps: i32,
    /// OBS mode's own capture rate — separate from `capture_fps` because the
    /// two modes have different constraints entirely. `capture_fps` (via
    /// `mirv_movie_fps`) is non-real-time for frame-sequence/direct-to-video:
    /// each frame just takes however long it takes to render, so 300 costs
    /// nothing there. OBS mode is real time — this drives both OBS's own
    /// `SetVideoSettings` FPS and the game's `fps_max` (see
    /// `final_init_commands`), and has to stay inside whatever the encoder
    /// can actually sustain at the configured resolution or frames get
    /// dropped (confirmed live: 300fps @ 2560x1440 lost 98.5% of frames).
    pub obs_capture_fps: i32,
    pub exit_on_finish: bool,
    pub init_commands: Vec<String>,
    pub custom_commands: Vec<CustomCommand>,
    pub pre_roll_seconds: f32,
    pub post_roll_seconds: f32,
    pub record_start_lead: f32,
    pub record_stop_trail: f32,
    pub initial_delay: f32,
    pub fast_forward_speed: f32,
    pub tickrate: f32,
    pub capture_directories: Vec<std::path::PathBuf>,
    /// How this batch records. The authority — `ffmpeg_capture` below is kept
    /// only so older settings files and payloads keep deserialising, and is
    /// reconciled into this by `PatcherConfig::normalise_capture_mode`.
    #[serde(default)]
    pub capture_mode: CaptureMode,
    /// Where OBS is, when `capture_mode` is `Obs`. Ignored otherwise.
    #[serde(default)]
    pub obs: ObsConfig,
    /// Capture straight to video through `mirv_movie_ffmpeg` instead of writing
    /// a BMP frame sequence. See `docs/direct_to_video_capture.md`.
    ///
    /// **Superseded by `capture_mode`.** Retained so settings written by builds
    /// predating the enum still load, and so nothing that already reads this
    /// field silently changes meaning. Kept in step by
    /// `normalise_capture_mode`; do not branch on it in new code.
    pub ffmpeg_capture: bool,
    /// Codec `mirv_movie_ffmpeg` encodes to. Only read when `ffmpeg_capture`
    /// is set.
    pub ffmpeg_capture_codec: CaptureCodec,
    pub resolution_width: i32,
    pub resolution_height: i32,
    pub primary_media_dir: Option<std::path::PathBuf>,
    pub movie_config: String,
    pub save_local_patched_copy: bool,
    pub session_id: String,
    pub hlae_path: String,
    pub game_path: String,
    pub ffmpeg_override_path: Option<String>,
    pub auto_clear_logs: bool,
    pub auto_clear_previews: bool,
    pub auto_clear_temp_demos: bool,
    /// Clear accumulated wall decals ahead of every recorded clip, so the
    /// second and later takes cut from one demo don't start dirty. Runs as a
    /// pre-pass inside `StreamPatcher::patch` — see `patch::decal_strip`.
    #[serde(default = "default_decal_flush")]
    pub decal_flush: bool,
    /// Decal ring size the flush pins `r_decals` to. A sweep costs one injected
    /// message per slot regardless of how dirty the walls actually are, so this
    /// trades injected bytes against how many decals a clip may accumulate
    /// before the ring starts eating its own. NOTHING else in the pipeline may
    /// set `r_decals` — lowering it mid-demo is precisely what strands decals.
    #[serde(default = "default_decal_ring_limit")]
    pub decal_ring_limit: u32,
    /// Horizontal field of view the capture will run at, in degrees. Together
    /// with the frame shape it decides how far off the view axis a flush
    /// position may sit and still be in shot.
    ///
    /// `mirv_fov` in `init_commands` overrides this when present, since that is
    /// the value the engine will actually use and two settings that disagree
    /// would be worse than one.
    #[serde(default = "default_capture_fov")]
    pub capture_fov: f32,
}

fn default_decal_flush() -> bool {
    true
}

fn default_decal_ring_limit() -> u32 {
    256
}

fn default_capture_fov() -> f32 {
    90.0
}

impl PatcherConfig {
    /// Reconciles `capture_mode` with the legacy `ffmpeg_capture` flag, and
    /// applies the constraints the mode implies.
    ///
    /// Call once, after building a config from settings or a payload, before
    /// anything branches on the mode. Cheap and idempotent.
    ///
    /// Three things happen:
    ///
    /// - A config from an older build has `capture_mode` at its default and
    ///   only `ffmpeg_capture` set. That is promoted, so the enum is right
    ///   without every caller having to know the old field exists.
    /// - `ffmpeg_capture` is then written back from the enum, so the two can
    ///   never disagree for anything still reading it.
    pub fn normalise_capture_mode(&mut self) {
        if self.capture_mode == CaptureMode::FrameSequence && self.ffmpeg_capture {
            self.capture_mode = CaptureMode::DirectToVideo;
        }
        self.ffmpeg_capture = self.capture_mode == CaptureMode::DirectToVideo;
    }

    pub fn build_hlae_process(&self, extra_engine_args: &str) -> std::process::Command {
        let hlae_exe = &self.hlae_path;
        let hl_exe = &self.game_path;

        let dll_path = match std::path::Path::new(hlae_exe).parent() {
            Some(parent) => parent.join("AfxHookGoldSrc.dll"),
            None => std::path::PathBuf::from("AfxHookGoldSrc.dll"),
        };

        let hook_dll_str = dll_path.to_string_lossy().replace("/", "\\\\");
        let program_path_str = hl_exe.replace("/", "\\\\");

        // HLAE's alpha flags, on every `-customLoader` launch without exception.
        //
        // `-afxForceAlpha8` is read by AfxHookGoldSrc.dll off the *game's*
        // command line, not by HLAE.exe. HLAE's own Launch GoldSrc dialog
        // appends it when its alpha box is ticked -- but that dialog is the
        // path we do not use. Under `-customLoader` HLAE composes nothing for
        // us (its `ProcessArgsCustomLoader`, in advancedfx/advancedfx's
        // `hlae/Program.cs`, reads neither the flag nor the dialog's saved
        // `<Launcher><ForceAlpha>`), so unless it is built here the hook never
        // sees it. HLAE's own `-forceAlpha` is a Launcher-mode switch that
        // `-customLoader` does not recognise at all, so passing it would be a
        // silent no-op. Not passed for that reason.
        //
        // `-afxForceAlpha8` TAKES A VALUE. From HLAE's own Launcher.cs, the
        // dialog builds it as:
        //
        //     " -afxForceAlpha8 " + (cfg.ForceAlpha ? 1 : 0).ToString()
        //
        // Passing it bare -- which is what two earlier attempts did -- makes
        // the hook read the following token as its argument, find something
        // that is not `1`, and leave the alpha channel off. That is exactly
        // the observed behaviour: the flag parses, and the captured hudAlpha
        // bitmaps come out byte-for-byte identical to a run without it.
        //
        // `-afxRenderMode` takes one of standard|fBO|memoryDC the same way.
        // `-32bpp` because a framebuffer that is not 32-bit has no alpha bits
        // to force in the first place. `-afxOptimizeCaptureVis` is left out:
        // it is a visibility optimisation unrelated to alpha, and would be one
        // more variable over the capture itself.
        //
        // NOT gated on a "Separate HUD" setting. One used to exist and gated
        // this from inside `capture_engine`, which meant the two hand-driven
        // launches -- "Launch Game (HLAE)" and Launch Preview, both of which
        // pass through here -- started a session with the alpha buffer off no
        // matter what. A user typing `mirv_movie_separate_hud 1` in the
        // console of such a session got a `hudalpha` stream that is pure
        // white in every frame, an all-opaque matte that `alphamerge` can
        // never turn into transparency, with nothing anywhere reporting a
        // fault. Forcing the alpha bits costs nothing when no HUD stream is
        // being written, and this app cannot know what the user will type
        // mid-session, so the flags go on every launch regardless. The
        // checkbox itself was removed later (there was no meaningful setting
        // behind `mirv_movie_separate_hud` beyond typing it into Initial
        // Commands anyway); see `docs/direct_to_video_capture.md`.
        //
        // `-condebug` is not optional either. It is what makes GoldSrc mirror
        // its console into `qconsole.log`, and the Studio reads that log for
        // per-demo progress, fast-forward-to-clip progress, crash detection and
        // the markers that drive OBS capture — so with it off, those features
        // do not fail loudly, they silently never fire. It lives here rather
        // than in any one caller's `extra_engine_args` so that *every* way the
        // app starts the game gets it: a capture batch, a preview, and Launch
        // Game (which used to ignore the setting altogether, #226).
        let cmd_line_str = format!(
            "-game dod -insecure -windowed -w {} -h {} -gl -32bpp -afxRenderMode standard -afxForceAlpha8 1 -condebug {}",
            self.resolution_width, self.resolution_height, extra_engine_args
        );

        let mut cmd = std::process::Command::new(hlae_exe);
        cmd.args([
            "-customLoader",
            "-noGui",
            "-autoStart",
            "-hookDllPath",
            &hook_dll_str,
            "-programPath",
            &program_path_str,
            "-cmdLine",
            &cmd_line_str,
        ]);
        cmd.env("SteamAppId", "30");

        if let Some(parent) = std::path::Path::new(hlae_exe).parent() {
            cmd.current_dir(parent);
        }

        cmd
    }

    pub fn calculate_total_capture_duration(&self, base_action_secs: f32) -> f32 {
        self.record_start_lead + base_action_secs + self.record_stop_trail
    }
}

impl Default for PatcherConfig {
    fn default() -> Self {
        Self {
            pre_roll_ticks: 200,
            post_roll_ticks: 60,
            capture_fps: 300,
            obs_capture_fps: 120,
            exit_on_finish: true,
            init_commands: Vec::new(),
            custom_commands: Vec::new(),
            pre_roll_seconds: 2.0,
            post_roll_seconds: 0.6,
            record_start_lead: 0.0,
            record_stop_trail: 0.0,
            initial_delay: 3.0,
            fast_forward_speed: 0.05,
            tickrate: 100.0,
            capture_directories: Vec::new(),
            capture_mode: CaptureMode::default(),
            obs: ObsConfig::default(),
            ffmpeg_capture: false,
            ffmpeg_capture_codec: CaptureCodec::default(),
            resolution_width: 1280,
            resolution_height: 720,
            primary_media_dir: None,
            movie_config: String::new(),
            save_local_patched_copy: false,
            session_id: String::new(),
            hlae_path: String::new(),
            game_path: String::new(),
            ffmpeg_override_path: None,
            auto_clear_logs: false,
            auto_clear_previews: false,
            auto_clear_temp_demos: false,
            decal_flush: default_decal_flush(),
            decal_ring_limit: default_decal_ring_limit(),
            capture_fov: default_capture_fov(),
        }
    }
}

// ── Scanner filter rules ──────────────────────────────────────────────────────

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HighlightRules {
    pub max_time_gap: Option<f32>,
}

// ── Worker event channel types (native-only: require std threading) ───────────

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone)]
pub enum PatchEvent {
    Starting(usize),
    Progress(String, f32),
    Completed,
    Cancelled,
    Error(String),
}

#[cfg(not(target_arch = "wasm32"))]
pub struct CaptureWorker {
    pub receiver: std::sync::mpsc::Receiver<PatchEvent>,
    pub is_running: bool,
    pub cancel_token: Arc<AtomicBool>,
    pub handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(test)]
mod capture_codec_tests {
    use super::CaptureCodec;

    const ALL: [CaptureCodec; 4] = [
        CaptureCodec::UtVideo,
        CaptureCodec::Ffv1,
        CaptureCodec::X264Lossless,
        CaptureCodec::RawVideo,
    ];

    #[test]
    fn test_str_id_round_trips_through_every_codec() {
        // Settings persist the string id, so these must stay inverses or a
        // saved choice silently reverts to the default on next launch.
        for codec in ALL {
            assert_eq!(CaptureCodec::from_str_id(codec.to_str_id()), codec);
        }
    }

    #[test]
    fn test_unknown_id_falls_back_to_default() {
        // A settings file naming a codec this build does not have must not
        // stop a capture.
        assert_eq!(CaptureCodec::from_str_id("h265"), CaptureCodec::default());
        assert_eq!(CaptureCodec::from_str_id(""), CaptureCodec::default());
    }

    #[test]
    fn test_every_codec_pins_an_explicit_pixel_format() {
        // hudAlpha carries the HUD matte in its red channel and the renderer
        // reads it with extractplanes=r. Leaving pix_fmt to FFmpeg's
        // negotiation is what would silently destroy that.
        for codec in ALL {
            assert!(
                codec.args().contains("-pix_fmt"),
                "{:?} must pin a pixel format",
                codec
            );
        }
    }

    #[test]
    fn test_no_codec_uses_chroma_subsampling() {
        // Any yuv420p/yuv422p entry would wreck the HUD alpha with no error
        // anywhere — the failure would only show up in a finished render.
        for codec in ALL {
            let args = codec.args();
            assert!(
                !args.contains("yuv420") && !args.contains("yuv422"),
                "{:?} must stay RGB/4:4:4, got {}",
                codec,
                args
            );
        }
    }

    #[test]
    fn test_no_codec_is_lossy() {
        // The render pass always re-encodes, so a lossy capture codec costs
        // quality for no benefit. x264 is admitted only at -qp 0.
        for codec in ALL {
            let args = codec.args();
            if args.contains("libx264") {
                assert!(args.contains("-qp 0"), "x264 must be lossless: {}", args);
            }
        }
    }

    #[test]
    fn test_default_is_the_one_proven_in_a_real_capture() {
        assert_eq!(CaptureCodec::default(), CaptureCodec::UtVideo);
    }
}

#[cfg(test)]
mod launch_args_tests {
    use super::PatcherConfig;

    /// The `-cmdLine` string `build_hlae_process` hands to HLAE.
    ///
    /// Read back off the built `Command` rather than reconstructed, so the test
    /// fails if the flag stops reaching the argument that actually carries it.
    fn cmd_line_of(cfg: &PatcherConfig, extra: &str) -> String {
        let cmd = cfg.build_hlae_process(extra);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let at = args
            .iter()
            .position(|a| a == "-cmdLine")
            .expect("build_hlae_process must pass -cmdLine");
        args[at + 1].clone()
    }

    #[test]
    fn test_alpha_flags_are_sent_on_every_launch_shape() {
        // The three call sites, by their `extra_engine_args`: a capture batch,
        // Launch Preview, and the standalone "Launch Game (HLAE)" button. The
        // last two used to get no alpha flags at all, so a hand-driven session
        // that enabled separate HUD in the console wrote an all-white
        // `hudalpha` — an opaque matte, silently unusable. Pin all three.
        for extra in ["-condebug +exec dodtools_helper.cfg +playdemo primer", "+viewdemo stem", ""] {
            let line = cmd_line_of(&PatcherConfig::default(), extra);
            assert!(
                line.contains("-afxForceAlpha8 1"),
                "alpha flag missing for extra={extra:?}: {line}"
            );
            assert!(line.contains("-32bpp"), "missing -32bpp for extra={extra:?}: {line}");
            assert!(
                line.contains("-afxRenderMode standard"),
                "missing render mode for extra={extra:?}: {line}"
            );
        }
    }

    #[test]
    fn test_condebug_is_passed_on_every_launch() {
        // The Studio reads the engine's console log for per-demo progress,
        // fast-forward-to-clip progress, crash detection and OBS markers, and
        // none of those fail loudly without it — they silently never fire. It
        // used to be a checkbox; there is deliberately no longer any way to
        // launch without it. See #226.
        let cfg = PatcherConfig::default();
        for extra in ["", "+viewdemo foo", "+exec dodtools_helper.cfg +playdemo primer"] {
            let line = cmd_line_of(&cfg, extra);
            assert!(
                line.contains("-condebug"),
                "launch args must carry -condebug, got: {line}"
            );
        }
    }

    #[test]
    fn test_engine_flags_precede_console_commands() {
        // GoldSrc parses `-` switches off the command line and queues `+`
        // commands after; an alpha flag landing after a `+` would be read as
        // an argument to that command instead of a switch.
        let line = cmd_line_of(&PatcherConfig::default(), "+playdemo primer");
        let first_plus = line.find('+').expect("the console command is present");
        let alpha = line.find("-afxForceAlpha8").expect("the alpha flag is present");
        assert!(alpha < first_plus, "alpha flag must precede any +command: {line}");
    }

    #[test]
    fn test_condebug_precedes_the_callers_own_args() {
        // `+exec`/`+playdemo` are commands the engine runs at startup, so a
        // switch placed after them is parsed as their argument rather than as a
        // switch. Keeping -condebug ahead of `extra_engine_args` is what stops
        // that, and nothing else in the process would report it.
        let line = cmd_line_of(&PatcherConfig::default(), "+playdemo primer");
        let condebug = line.find("-condebug").expect("-condebug present");
        let playdemo = line.find("+playdemo").expect("+playdemo present");
        assert!(
            condebug < playdemo,
            "-condebug must come before the caller's own args, got: {line}"
        );
    }
}
