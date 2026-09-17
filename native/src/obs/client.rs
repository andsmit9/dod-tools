//! A blocking obs-websocket v5 client, scoped to what the capture path needs.
//!
//! Blocking rather than async on purpose. `CaptureCleanupGuard::drop` must be
//! able to send a `StopRecord` on every path out of a batch — a cancel, a
//! crashed game, a finished run — and a `Drop` has no async runtime under it.
//! A recording OBS left running after a cancelled batch fills the user's drive
//! silently, so that guarantee is worth more here than concurrency is.
//!
//! It does not extend to a panic: release builds abort rather than unwind, so
//! no destructor runs. `obs::recover` covers that on the next start.
//!
//! Verified against **OBS 32.2.2 / obs-websocket 5.7.4**: every request below
//! is present, `StopRecord` reports the output path, and `SetRecordDirectory`
//! steers the standard recording output (though *not* Custom Output (FFmpeg),
//! which keeps its own path — see `docs/obs_alternate_capture.md`).

use std::net::TcpStream;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// How long a single request waits for its reply.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// How long to wait for recording to actually begin after `StartRecord`.
///
/// Measured at 59-85 ms across five runs; this is a generous ceiling for a
/// machine under capture load, not an expectation.
const RECORD_START_TIMEOUT: Duration = Duration::from_secs(15);

/// How long to wait for the output file to be finalised after `StopRecord`.
///
/// Measured at ~1.06 s consistently. Worth noting that number is longer than
/// `MIN_TAKE_SEPARATION_SECONDS` (1.0 s), which is why that constant needs
/// raising for this path — see `obs_take_separation_seconds`.
const RECORD_STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// Socket read timeout. Nothing waits on a single read; the timeout only makes
/// the read loop interruptible so an overall deadline can be enforced.
const SOCKET_READ_TIMEOUT: Duration = Duration::from_millis(200);

/// Re-exported so callers holding an `ObsClient` do not have to reach into the
/// patcher for it. Defined in `patch::types` because the patcher reads it on
/// every target, including wasm32, where this module does not exist.
pub use crate::patch::types::OBS_TAKE_SEPARATION_SECONDS;

#[derive(Debug)]
pub enum ObsError {
    /// Could not reach OBS at all.
    Connect(String),
    /// Reached it, but the handshake failed.
    ///
    /// Carries whether OBS said the password was wrong (close code 4009) as
    /// opposed to anything else, because those want completely different
    /// advice: one is "check the password", the other is "this is a bug".
    Auth { wrong_password: bool, detail: String },
    /// A request was refused by OBS, with its own message.
    Request { request: String, detail: String },
    /// The connection dropped or a reply never came.
    Transport(String),
}

impl std::fmt::Display for ObsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(d) => write!(
                f,
                "could not reach OBS ({d}). Is OBS running, with Tools -> WebSocket Server \
                 Settings enabled?"
            ),
            Self::Auth { wrong_password: true, .. } => write!(
                f,
                "OBS rejected the password. Copy it from Tools -> WebSocket Server Settings -> \
                 Show Connect Info rather than retyping it."
            ),
            Self::Auth { detail, .. } => write!(f, "OBS refused the connection: {detail}"),
            Self::Request { request, detail } => write!(f, "OBS refused {request}: {detail}"),
            Self::Transport(d) => write!(f, "lost contact with OBS: {d}"),
        }
    }
}

impl std::error::Error for ObsError {}

impl ObsError {
    /// Whether this is the connection failing rather than OBS answering.
    ///
    /// The distinction decides whether a reconnect is worth attempting: a
    /// refused request means OBS is alive and said no, and retrying it down a
    /// fresh socket would just be refused again.
    pub fn is_transport(&self) -> bool {
        matches!(self, Self::Transport(_) | Self::Connect(_))
    }
}

/// What a preflight found, for reporting to the user before a batch starts.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ObsPreflight {
    pub obs_version: String,
    pub websocket_version: String,
    /// Requests this design needs that the install does *not* have. Empty is
    /// the expected case; anything here means the batch cannot run.
    pub missing_requests: Vec<String>,
    pub recording: bool,
    pub streaming: bool,
    pub record_directory: String,
    pub canvas_width: i64,
    pub canvas_height: i64,
    pub output_width: i64,
    pub output_height: i64,
    pub fps: f64,
    pub current_scene: String,
    pub current_profile: String,
    pub scene_collection: String,
    /// Non-fatal problems worth showing the user — a canvas that does not match
    /// the game, a low frame rate, no audio source in the scene.
    pub warnings: Vec<String>,
}

/// Requests the capture path issues. Checked up front so a batch fails before
/// it spawns the game rather than half way through.
const REQUIRED_REQUESTS: &[&str] = &[
    "StartRecord",
    "StopRecord",
    "GetRecordStatus",
    "GetRecordDirectory",
    "GetVideoSettings",
    "GetSceneList",
    // Added for the dod-studio-owned profile/scene auto-provisioning
    // (see obs::provision) — every one of these is load-bearing for it now,
    // not just advisory the way GetProfileParameter is below.
    "GetProfileList",
    "CreateProfile",
    "SetCurrentProfile",
    "SetVideoSettings",
    "CreateScene",
    "GetInputList",
    "GetInputSettings",
    "SetInputSettings",
    "CreateInput",
    "CreateSceneItem",
    "GetSceneItemId",
    "SetSceneItemTransform",
    "SetInputMute",
];

pub struct ObsClient {
    ws: tungstenite::WebSocket<TcpStream>,
    next_id: u64,
    last_close: Option<String>,
}

impl ObsClient {
    /// Connects and completes the v5 handshake.
    ///
    /// `password` may be empty when OBS has authentication switched off; it is
    /// only used if the server's Hello asks for it.
    pub fn connect(url: &str, password: &str) -> Result<Self, ObsError> {
        let host = url.trim_start_matches("ws://");
        let stream = TcpStream::connect(host)
            .map_err(|e| ObsError::Connect(format!("{host}: {e}")))?;
        stream
            .set_read_timeout(Some(SOCKET_READ_TIMEOUT))
            .map_err(|e| ObsError::Connect(e.to_string()))?;
        let uri = url
            .parse::<tungstenite::http::Uri>()
            .map_err(|e| ObsError::Connect(e.to_string()))?;
        let (ws, _) = tungstenite::client(uri, stream)
            .map_err(|e| ObsError::Connect(format!("websocket handshake: {e}")))?;

        let mut client = Self { ws, next_id: 0, last_close: None };

        let hello = client
            .read_op(0, Duration::from_secs(10))
            .ok_or_else(|| ObsError::Transport("no Hello (op 0) from OBS".into()))?;

        let mut identify = json!({ "rpcVersion": 1 });
        if let Some(auth) = hello["authentication"].as_object() {
            if password.is_empty() {
                return Err(ObsError::Auth {
                    wrong_password: true,
                    detail: "OBS requires a password but none is configured".into(),
                });
            }
            let challenge = auth["challenge"].as_str().unwrap_or_default();
            let salt = auth["salt"].as_str().unwrap_or_default();
            identify["authentication"] = Value::String(auth_string(password, salt, challenge));
        }
        client.send(json!({ "op": 1, "d": identify }))?;

        if client.read_op(2, Duration::from_secs(10)).is_none() {
            let detail = client.last_close.clone().unwrap_or_else(|| "no reply".into());
            // 4009 is obs-websocket's own "authentication failed". Anything
            // else is not a password problem and should not be reported as one.
            return Err(ObsError::Auth {
                wrong_password: detail.contains("4009"),
                detail,
            });
        }
        Ok(client)
    }

    /// Checks the install can do what the capture path needs, and gathers what
    /// is worth telling the user before a batch runs.
    ///
    /// `game_width`/`game_height` are the pipeline's own capture resolution,
    /// used to catch the misconfiguration that silently costs the most quality:
    /// a canvas larger than the game means the source is scaled up onto it and
    /// the whole canvas scaled back down, discarding most of the pixels before
    /// the encoder sees them.
    pub fn preflight(
        &mut self,
        game_width: i32,
        game_height: i32,
    ) -> Result<ObsPreflight, ObsError> {
        let version = self.request("GetVersion", json!({}))?;
        let available: Vec<String> = version["availableRequests"]
            .as_array()
            .map(|v| v.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let missing_requests = REQUIRED_REQUESTS
            .iter()
            .filter(|r| !available.iter().any(|a| a == *r))
            .map(|r| r.to_string())
            .collect();

        let status = self.request("GetRecordStatus", json!({}))?;
        let stream = self.request("GetStreamStatus", json!({})).unwrap_or(json!({}));
        let dir = self.request("GetRecordDirectory", json!({}))?;
        let video = self.request("GetVideoSettings", json!({}))?;
        let scene = self.request("GetCurrentProgramScene", json!({})).unwrap_or(json!({}));
        let collection = self
            .request("GetSceneCollectionList", json!({}))
            .unwrap_or(json!({}));
        let current_profile = self.profile_list().map(|(_, current)| current).unwrap_or_default();

        let fps_num = video["fpsNumerator"].as_f64().unwrap_or(0.0);
        let fps_den = video["fpsDenominator"].as_f64().unwrap_or(1.0);
        let fps = if fps_den > 0.0 { fps_num / fps_den } else { 0.0 };

        let canvas_width = video["baseWidth"].as_i64().unwrap_or(0);
        let canvas_height = video["baseHeight"].as_i64().unwrap_or(0);
        let output_width = video["outputWidth"].as_i64().unwrap_or(0);
        let output_height = video["outputHeight"].as_i64().unwrap_or(0);

        let mut warnings = Vec::new();
        if canvas_width != game_width as i64 || canvas_height != game_height as i64 {
            warnings.push(format!(
                "OBS canvas is {canvas_width}x{canvas_height} but the game renders at \
                 {game_width}x{game_height}. The capture is scaled onto the canvas and scaled \
                 again to the output, which throws away detail for nothing. Set Settings -> \
                 Video -> Base (Canvas) Resolution to {game_width}x{game_height}."
            ));
        }
        if output_width != canvas_width || output_height != canvas_height {
            warnings.push(format!(
                "OBS output is {output_width}x{output_height} against a {canvas_width}x\
                 {canvas_height} canvas, so every frame is rescaled. Matching them avoids it."
            ));
        }
        if fps > 0.0 && fps < 60.0 {
            warnings.push(format!(
                "OBS output is {fps:.0} fps. On this path OBS's rate is the clip's rate — \
                 HLAE's capture FPS does not apply — so 60 is usually the sensible floor."
            ));
        }
        if stream["outputActive"].as_bool().unwrap_or(false) {
            warnings.push(
                "OBS is streaming. Driving its recorder during a live stream is not something \
                 dod-studio should do uninvited; stop the stream or use a different capture mode."
                    .to_string(),
            );
        }
        warnings.extend(self.output_settings_warnings());

        Ok(ObsPreflight {
            obs_version: version["obsVersion"].as_str().unwrap_or("?").to_string(),
            websocket_version: version["obsWebSocketVersion"].as_str().unwrap_or("?").to_string(),
            missing_requests,
            recording: status["outputActive"].as_bool().unwrap_or(false),
            streaming: stream["outputActive"].as_bool().unwrap_or(false),
            record_directory: dir["recordDirectory"].as_str().unwrap_or("").to_string(),
            canvas_width,
            canvas_height,
            output_width,
            output_height,
            fps,
            current_scene: scene["currentProgramSceneName"].as_str().unwrap_or("").to_string(),
            current_profile,
            scene_collection: collection["currentSceneCollectionName"]
                .as_str()
                .unwrap_or("")
                .to_string(),
            warnings,
        })
    }

    /// Whether OBS is recording right now.
    pub fn is_recording(&mut self) -> Result<bool, ObsError> {
        Ok(self.request("GetRecordStatus", json!({}))?["outputActive"]
            .as_bool()
            .unwrap_or(false))
    }

    /// Whether OBS is streaming right now.
    pub fn is_streaming(&mut self) -> Result<bool, ObsError> {
        Ok(self.request("GetStreamStatus", json!({}))?["outputActive"]
            .as_bool()
            .unwrap_or(false))
    }

    /// Refuses if OBS is already recording or streaming, checked against
    /// whatever is currently active — before anything below this call in the
    /// connect path switches profile or scene.
    ///
    /// The ordering is the point: `obs::provision::ensure_dod_studio_setup`
    /// mutates the user's live OBS (profile switch, scene switch, source
    /// creation/repair), and that must never happen out from under a stream
    /// that has nothing to do with dod-studio, or a recording already running
    /// under whatever profile the user was on. Call this first.
    pub fn refuse_if_busy(&mut self) -> Result<(), ObsError> {
        if self.is_recording()? {
            return Err(ObsError::Request {
                request: "StartRecord".into(),
                detail: "OBS is already recording. Stop it before starting a batch.".into(),
            });
        }
        if self.is_streaming()? {
            return Err(ObsError::Request {
                request: "StartRecord".into(),
                detail: "OBS is streaming. dod-studio will not drive its recorder during a live \
                         stream."
                    .into(),
            });
        }
        Ok(())
    }

    /// Where OBS is currently set to write recordings.
    pub fn record_directory(&mut self) -> Result<String, ObsError> {
        Ok(self.request("GetRecordDirectory", json!({}))?["recordDirectory"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    /// `(outputTotalFrames, outputSkippedFrames)` from `GetStats` — system-
    /// wide output-thread counters, not scoped to one recording. Callers
    /// snapshot this at the start of a block and diff against the end to get
    /// a per-block figure, the same way OBS's own "skipped frames due to
    /// encoding lag" log line is derived internally.
    ///
    /// Like `GetProfileParameter` below, deliberately not in
    /// `REQUIRED_REQUESTS`: this only produces advice (a warning about
    /// dropped frames), and an OBS too old to answer it should still be able
    /// to capture. `None` on any failure, for the same reason.
    pub fn output_frame_stats(&mut self) -> Option<(i64, i64)> {
        let stats = self.request("GetStats", json!({})).ok()?;
        Some((
            stats["outputTotalFrames"].as_i64()?,
            stats["outputSkippedFrames"].as_i64()?,
        ))
    }

    /// A profile setting, read-only.
    ///
    /// `GetProfileParameter` is deliberately not in `REQUIRED_REQUESTS`: these
    /// lookups only produce advice, and an OBS too old to answer them should
    /// still be able to capture. Every failure here is silent for that reason.
    fn profile_param(&mut self, category: &str, name: &str) -> Option<String> {
        let v = self
            .request(
                "GetProfileParameter",
                json!({ "parameterCategory": category, "parameterName": name }),
            )
            .ok()?;
        let value = v["parameterValue"]
            .as_str()
            .or_else(|| v["defaultParameterValue"].as_str())?;
        (!value.is_empty()).then(|| value.to_string())
    }

    /// Warnings about how the recording output is configured.
    ///
    /// Reads only. The profile is the user's file and the same discipline
    /// applies to it as to their game `.cfg`s: detect and warn, never write.
    fn output_settings_warnings(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();
        let advanced = self
            .profile_param("Output", "Mode")
            .is_some_and(|m| m.eq_ignore_ascii_case("Advanced"));
        let category = if advanced { "AdvOut" } else { "SimpleOutput" };

        if advanced
            && self
                .profile_param("AdvOut", "RecType")
                .is_some_and(|t| t == "FFmpegOutput")
        {
            warnings.push(
                "OBS is in Custom Output (FFmpeg) mode. Per-block routing does not work there \
                 — `SetRecordDirectory` only steers the standard output, measured — so every \
                 block would be written to the same path. Switch Output -> Recording back to \
                 Standard for this capture mode."
                    .to_string(),
            );
        }

        if let Some(container) = self.profile_param(category, "RecFormat2") {
            // Measured on OBS 32.2.2 rather than assumed, because the received
            // wisdom here is worse than the reality: an MP4 from an OBS killed
            // mid-recording still played, decoded up to the moment it died, and
            // reported only "partial file" with a damaged audio frame at the
            // end. Truncated, not destroyed. MKV and the fragmented/hybrid MP4
            // variants lose nothing at all, which is still worth having, but
            // this is a preference and not the disaster it is usually called.
            if matches!(container.as_str(), "mp4" | "mov") {
                warnings.push(format!(
                    "OBS is recording to {container}. If OBS is killed mid-batch that block is \
                     recoverable but cut short, with its last moment damaged. MKV or hybrid MP4 \
                     lose nothing instead — dod-studio keeps whatever container OBS wrote."
                ));
            }
        }

        // Splitting turns one block into several files and `StopRecord` reports
        // only the last, so the rest of the clip would be stranded under names
        // nothing downstream resolves.
        if self
            .profile_param(category, "RecSplitFile")
            .is_some_and(|v| v == "true")
        {
            warnings.push(
                "OBS has automatic file splitting enabled. Any block long enough to split \
                 would keep only its final piece. Turn it off in Output -> Recording."
                    .to_string(),
            );
        }

        warnings
    }

    /// Scene names in the active collection, for the settings picker.
    pub fn scene_names(&mut self) -> Result<Vec<String>, ObsError> {
        let list = self.request("GetSceneList", json!({}))?;
        Ok(list["scenes"]
            .as_array()
            .map(|v| {
                v.iter()
                    .filter_map(|s| s["sceneName"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }

    pub fn current_scene(&mut self) -> Result<String, ObsError> {
        Ok(self.request("GetCurrentProgramScene", json!({}))?
            ["currentProgramSceneName"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    /// Profile names and the currently active one, for the settings picker.
    /// One request carries both, unlike scenes (`scene_names`/`current_scene`
    /// are separate calls) — `GetProfileList` already returns
    /// `currentProfileName` alongside the list.
    pub fn profile_list(&mut self) -> Result<(Vec<String>, String), ObsError> {
        let list = self.request("GetProfileList", json!({}))?;
        let profiles = list["profiles"]
            .as_array()
            .map(|v| v.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let current = list["currentProfileName"].as_str().unwrap_or_default().to_string();
        Ok((profiles, current))
    }

    /// Switches the active scene.
    ///
    /// A live-state change rather than a settings edit: reversible, destroys
    /// nothing, and exactly what a user picking a scene is asking for. The
    /// caller is expected to restore the previous scene when the batch ends.
    pub fn set_scene(&mut self, scene: &str) -> Result<(), ObsError> {
        self.request("SetCurrentProgramScene", json!({ "sceneName": scene }))?;
        Ok(())
    }

    /// Switches the active profile. Heavier than `set_scene`: a profile
    /// bundles Video settings (canvas/output resolution, FPS) alongside
    /// Output/Audio/Advanced, so this can change what `preflight` needs to
    /// validate — callers must switch before preflighting, not after. The
    /// caller is expected to restore the previous profile when the batch ends.
    pub fn set_profile(&mut self, profile: &str) -> Result<(), ObsError> {
        self.request("SetCurrentProfile", json!({ "profileName": profile }))?;
        Ok(())
    }

    /// Points OBS's recording output at `dir`.
    ///
    /// Only steers the **standard** recording output. Custom Output (FFmpeg)
    /// keeps its own path in `AdvOut/FFFilePath` and ignores this — measured,
    /// and the reason the OBS path ships on Standard mode first.
    pub fn set_record_directory(&mut self, dir: &str) -> Result<(), ObsError> {
        self.request("SetRecordDirectory", json!({ "recordDirectory": dir }))?;
        Ok(())
    }

    // ── dod-studio-owned profile/scene provisioning (obs::provision) ──────────
    //
    // Everything below writes to OBS, unlike `profile_param`/
    // `output_settings_warnings` above, which are deliberately read-only —
    // "the profile is the user's file, detect and warn, never write" — the
    // same discipline CLAUDE.md holds the game's own .cfg files to. That still
    // holds for the user's *own* profiles/scenes. What's below only ever
    // touches the profile/scene/inputs dod-studio creates and names itself
    // (obs::provision::PROFILE_NAME/SCENE_NAME) — never anything the user
    // already had, which is the entire reason that profile/scene exists as
    // its own dedicated thing instead of switching into whatever the user
    // picked.

    /// Creates a profile. Fails if one by this name already exists —
    /// callers check `profile_list()` first.
    pub fn create_profile(&mut self, name: &str) -> Result<(), ObsError> {
        self.request("CreateProfile", json!({ "profileName": name }))?;
        Ok(())
    }

    /// Creates an empty scene. Fails if one by this name already exists in
    /// the current scene collection — callers check `scene_names()` first.
    pub fn create_scene(&mut self, name: &str) -> Result<(), ObsError> {
        self.request("CreateScene", json!({ "sceneName": name }))?;
        Ok(())
    }

    /// Every input's name, across the whole OBS instance — inputs are global
    /// objects in obs-websocket's model, not scoped to one scene, so this is
    /// not the same question as "what's in scene X."
    pub fn input_names(&mut self) -> Result<Vec<String>, ObsError> {
        let list = self.request("GetInputList", json!({}))?;
        Ok(list["inputs"]
            .as_array()
            .map(|v| {
                v.iter()
                    .filter_map(|s| s["inputName"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Creates a new input of `kind` inside `scene`, with `settings` applied
    /// immediately. Fails if an input by this name already exists anywhere in
    /// OBS (names are global) — callers check `input_names()` first and call
    /// `set_input_settings` instead to repair an existing one.
    pub fn create_input(
        &mut self,
        scene: &str,
        name: &str,
        kind: &str,
        settings: Value,
    ) -> Result<(), ObsError> {
        self.request(
            "CreateInput",
            json!({
                "sceneName": scene,
                "inputName": name,
                "inputKind": kind,
                "inputSettings": settings,
            }),
        )?;
        Ok(())
    }

    /// Adds an *existing* input (by name) as a scene item in `scene` — the
    /// input itself is untouched, only its placement in this particular
    /// scene changes. Needed alongside `create_input`: inputs are global in
    /// obs-websocket's model, so an input that already existed somewhere
    /// else (a prior manual setup, an earlier dod-studio run against a scene
    /// that has since been renamed) never goes through `create_input` at
    /// all — `provision::ensure_game_capture_source`/`ensure_game_audio_source`
    /// call this when `scene_item_id` comes back empty for exactly that case.
    pub fn create_scene_item(&mut self, scene: &str, source: &str) -> Result<(), ObsError> {
        self.request(
            "CreateSceneItem",
            json!({ "sceneName": scene, "sourceName": source }),
        )?;
        Ok(())
    }

    /// Overwrites an existing input's settings. `overlay: false` — a full
    /// replace, not a merge, so a setting this call omits reverts to that
    /// kind's default rather than surviving from whatever was there before.
    /// That's the repair behaviour provisioning wants: a drifted or
    /// hand-edited setting on the dod-studio-owned input gets put back
    /// exactly, not partially.
    pub fn set_input_settings(&mut self, name: &str, settings: Value) -> Result<(), ObsError> {
        self.request(
            "SetInputSettings",
            json!({ "inputName": name, "inputSettings": settings, "overlay": false }),
        )?;
        Ok(())
    }

    /// Mutes or unmutes an input by name. Best-effort by design at the call
    /// site: a renamed or removed default "Desktop Audio"/"Mic/Aux" input is
    /// a real possibility on someone else's OBS install and not worth failing
    /// provisioning over.
    pub fn set_input_mute(&mut self, name: &str, muted: bool) -> Result<(), ObsError> {
        self.request("SetInputMute", json!({ "inputName": name, "inputMuted": muted }))?;
        Ok(())
    }

    /// The scene item id of `source` inside `scene` — needed for
    /// `SetSceneItemTransform`, which addresses items by id rather than name.
    pub fn scene_item_id(&mut self, scene: &str, source: &str) -> Result<i64, ObsError> {
        Ok(self
            .request("GetSceneItemId", json!({ "sceneName": scene, "sourceName": source }))?
            ["sceneItemId"]
            .as_i64()
            .unwrap_or(0))
    }

    /// Sets a scene item's bounding box to scale-inner-fit within
    /// `width`x`height` — matches a manually-configured working setup
    /// (`OBS_BOUNDS_SCALE_INNER`, `bounds_type: 2` in OBS's own scene JSON).
    /// A no-op in practice once the canvas already matches the game's own
    /// resolution (`set_video_settings` below), but kept explicit rather than
    /// assumed so the scene item is still correct if that ever changes.
    pub fn set_scene_item_bounds(
        &mut self,
        scene: &str,
        item_id: i64,
        width: f64,
        height: f64,
    ) -> Result<(), ObsError> {
        self.request(
            "SetSceneItemTransform",
            json!({
                "sceneName": scene,
                "sceneItemId": item_id,
                "sceneItemTransform": {
                    "boundsType": "OBS_BOUNDS_SCALE_INNER",
                    "boundsWidth": width,
                    "boundsHeight": height,
                }
            }),
        )?;
        Ok(())
    }

    /// Sets canvas (base) and output resolution to the same `width`x`height`,
    /// and the output frame rate to `fps_num`/`fps_den`. Canvas == output is
    /// deliberate: a mismatch means every frame is scaled twice for nothing
    /// (see `preflight`'s own warning for this), and dod-studio owns this
    /// profile specifically so it can just set both correctly instead of
    /// warning about it.
    pub fn set_video_settings(
        &mut self,
        width: i32,
        height: i32,
        fps_num: i32,
        fps_den: i32,
    ) -> Result<(), ObsError> {
        self.request(
            "SetVideoSettings",
            json!({
                "baseWidth": width,
                "baseHeight": height,
                "outputWidth": width,
                "outputHeight": height,
                "fpsNumerator": fps_num,
                "fpsDenominator": fps_den,
            }),
        )?;
        Ok(())
    }

    /// Starts recording and waits until frames are actually being written.
    ///
    /// The wait is the point. `StartRecord` returns almost immediately, but
    /// recording begins at `RecordStateChanged: OBS_WEBSOCKET_OUTPUT_STARTED` —
    /// measured 59-85 ms later. Returning before that would put the clip's
    /// first frames outside the file.
    pub fn start_record(&mut self) -> Result<(), ObsError> {
        self.request("StartRecord", json!({}))?;
        self.wait_for_record_state("OBS_WEBSOCKET_OUTPUT_STARTED", RECORD_START_TIMEOUT)
            .ok_or_else(|| {
                ObsError::Transport("OBS never reported that recording started".into())
            })?;
        Ok(())
    }

    /// Stops recording and returns the finished file's path.
    ///
    /// Prefers the path from the `STOPPED` event over the one in the request's
    /// own reply: the event fires when the file is finalised, and by then the
    /// file exists. Falls back to the reply's path when the event does not
    /// arrive, since a path that is probably right beats none at all.
    pub fn stop_record(&mut self) -> Result<String, ObsError> {
        let reply = self.request("StopRecord", json!({}))?;
        let reply_path = reply["outputPath"].as_str().unwrap_or_default().to_string();
        let event_path = self
            .wait_for_record_state("OBS_WEBSOCKET_OUTPUT_STOPPED", RECORD_STOP_TIMEOUT)
            .and_then(|e| e["outputPath"].as_str().map(str::to_string))
            .filter(|p| !p.is_empty());
        match event_path.or(if reply_path.is_empty() { None } else { Some(reply_path) }) {
            Some(p) => Ok(p),
            None => Err(ObsError::Transport(
                "OBS stopped recording but reported no output path".into(),
            )),
        }
    }

    /// Best-effort stop for cleanup paths.
    ///
    /// Never fails and never blocks long: used from `Drop`, where the only
    /// thing that matters is that OBS is not left recording, and where there is
    /// nobody left to report an error to.
    pub fn stop_record_quietly(&mut self) {
        let _ = self.request("StopRecord", json!({}));
    }

    // ── protocol ─────────────────────────────────────────────────────────────

    fn send(&mut self, v: Value) -> Result<(), ObsError> {
        self.ws
            .send(tungstenite::Message::Text(v.to_string().into()))
            .map_err(|e| ObsError::Transport(format!("send: {e}")))
    }

    fn read_op(&mut self, op: u64, timeout: Duration) -> Option<Value> {
        self.read_until(timeout, |m| (m["op"].as_u64() == Some(op)).then(|| m["d"].clone()))
    }

    fn read_until<T>(
        &mut self,
        timeout: Duration,
        mut f: impl FnMut(&Value) -> Option<T>,
    ) -> Option<T> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let msg = match self.ws.read() {
                Ok(m) => m,
                // A read timeout is "nothing yet", not a failure — that is what
                // makes the overall deadline enforceable.
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    continue
                }
                Err(_) => return None,
            };
            let text = match msg {
                tungstenite::Message::Text(t) => t.to_string(),
                tungstenite::Message::Close(frame) => {
                    self.last_close = Some(match frame {
                        Some(fr) => format!("code {} — {}", u16::from(fr.code), fr.reason),
                        None => "closed with no detail".to_string(),
                    });
                    return None;
                }
                _ => continue,
            };
            let Ok(parsed) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if let Some(found) = f(&parsed) {
                return Some(found);
            }
        }
        None
    }

    fn request(&mut self, request_type: &str, data: Value) -> Result<Value, ObsError> {
        self.next_id += 1;
        let id = format!("dodstudio-{}", self.next_id);
        self.send(json!({
            "op": 6,
            "d": { "requestType": request_type, "requestId": id, "requestData": data }
        }))?;

        let response = self
            .read_until(REQUEST_TIMEOUT, |m| {
                if m["op"].as_u64() != Some(7) {
                    return None;
                }
                if m["d"]["requestId"].as_str() != Some(id.as_str()) {
                    return None;
                }
                Some(m["d"].clone())
            })
            .ok_or_else(|| {
                ObsError::Transport(format!("{request_type}: no response within 15s"))
            })?;

        let status = &response["requestStatus"];
        if status["result"].as_bool() == Some(true) {
            Ok(response["responseData"].clone())
        } else {
            Err(ObsError::Request {
                request: request_type.to_string(),
                detail: format!(
                    "{} (code {})",
                    status["comment"].as_str().unwrap_or("refused"),
                    status["code"]
                ),
            })
        }
    }

    fn wait_for_record_state(&mut self, state: &str, timeout: Duration) -> Option<Value> {
        let want = state.to_string();
        self.read_until(timeout, |m| {
            if m["op"].as_u64() != Some(5) {
                return None;
            }
            if m["d"]["eventType"].as_str() != Some("RecordStateChanged") {
                return None;
            }
            let d = &m["d"]["eventData"];
            (d["outputState"].as_str() == Some(want.as_str())).then(|| d.clone())
        })
    }
}

/// obs-websocket v5 authentication.
///
/// ```text
/// secret = base64(sha256(password + salt))
/// auth   = base64(sha256(secret + challenge))
/// ```
///
/// Getting this wrong does not look like a bug: OBS simply never sends
/// `Identified`, and it reads as a wrong password no matter what is actually
/// wrong. Hence the test below, pinned against an independent implementation.
fn auth_string(password: &str, salt: &str, challenge: &str) -> String {
    use base64::Engine as _;
    use sha2::{Digest, Sha256};

    let b64 = base64::engine::general_purpose::STANDARD;
    let secret = b64.encode(Sha256::digest(format!("{password}{salt}").as_bytes()));
    b64.encode(Sha256::digest(format!("{secret}{challenge}").as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the handshake against OpenSSL rather than a published vector.
    ///
    /// The four algorithm steps are quoted from obs-websocket's
    /// `docs/generated/protocol.md`; the value below is this input run through
    /// OpenSSL, which agrees with `auth_string` to the character:
    ///
    /// ```text
    /// SECRET=$(printf '%s' "supersecretpassword${SALT}" \
    ///   | openssl dgst -sha256 -binary | openssl base64 -A)
    /// printf '%s' "${SECRET}${CHAL}" \
    ///   | openssl dgst -sha256 -binary | openssl base64 -A
    /// ```
    ///
    /// Said plainly so nobody later "fixes" this against a mismatched example:
    /// two independent implementations of the quoted steps agree, and that is
    /// what is being asserted.
    #[test]
    fn auth_string_matches_an_independent_implementation() {
        assert_eq!(
            auth_string(
                "supersecretpassword",
                "lM1GncleQOaCu9lT1yeUZhFYnqhsLLP1G5lAGo3ixaI=",
                "+IxH4CnCiqpX1rM9scsNynZzbOe4KhDeYcTNS3PDaeY="
            ),
            "1Ct943GAT+6YQUUX47Ia/ncufilbe6+oD6lY+5kaCu4="
        );
    }

    /// The intermediate secret is base64 of the *binary* digest, not of its hex
    /// rendering — the easiest way to get this wrong, and it fails silently as
    /// an auth rejection.
    #[test]
    fn secret_is_base64_of_the_binary_digest() {
        use base64::Engine as _;
        use sha2::{Digest, Sha256};
        let secret = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(
            b"supersecretpasswordlM1GncleQOaCu9lT1yeUZhFYnqhsLLP1G5lAGo3ixaI=",
        ));
        assert_eq!(secret, "H1IfVz1pSREUQzbFTVnX/Tyb+gMhMik5x7yUBCY0PTs=");
    }

    /// A wrong password and a broken handshake produce the same silence from
    /// OBS, so the distinction has to survive into the error type or the user
    /// gets told to check a password that is already correct.
    #[test]
    fn auth_errors_distinguish_a_wrong_password_from_everything_else() {
        let wrong = ObsError::Auth {
            wrong_password: true,
            detail: "code 4009 — Authentication failed.".into(),
        };
        assert!(wrong.to_string().contains("rejected the password"));

        let other = ObsError::Auth {
            wrong_password: false,
            detail: "code 4008 — unsupported rpc version".into(),
        };
        assert!(!other.to_string().contains("rejected the password"));
        assert!(other.to_string().contains("4008"));
    }

    /// OBS needs ~1.06s to finalise a file; the HLAE-derived merge guard is
    /// 1.0s. If this ever drops back below that, takes start disappearing.
    #[test]
    fn obs_take_separation_clears_the_measured_finalise_time() {
        assert!(
            OBS_TAKE_SEPARATION_SECONDS > 1.065,
            "OBS took ~1.065s to finalise a file; a shorter guard loses takes"
        );
        assert!(
            OBS_TAKE_SEPARATION_SECONDS > crate::patch::builder::MIN_TAKE_SEPARATION_SECONDS,
            "the OBS guard must be looser than the HLAE one, not tighter"
        );
    }
}
