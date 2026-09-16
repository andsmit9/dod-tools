// map_manager.rs
// Whether the demos on screen can actually be played, and on the right map.
//
// Kept out of the scan pipeline deliberately. Scanning parses every frame of
// every demo and is the slow, threaded part of loading a folder; this reads 544
// bytes per demo and one map file per distinct map, so it runs after a scan
// without slowing it, and a failure here costs a badge rather than the folder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use native::patch::map_check::{self, MapStatus};
use crate::capture_manager::CustomCommandPayload;
use native::patch::map_fetch;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MapCheckRow {
    pub demo_path: String,
    pub demo_name: String,
    pub map_name: String,
    /// The build the demo was recorded on. Absent for HLTV demos, which do not
    /// record one.
    pub expected_checksum: Option<u32>,
    /// `ok` | `wrongBuild` | `missing` | `unverifiable` | `unreadableMap` |
    /// `unreadableDemo`
    pub state: String,
    pub detail: String,
    /// Whether the demo can be played at all.
    pub playable: bool,
}

/// Where maps live for a configured `hl.exe`, or an error a person can act on.
fn maps_dir(game_path: &str) -> Result<PathBuf, String> {
    let exe = Path::new(game_path);
    map_check::maps_dir_for_exe(exe).ok_or_else(|| crate::messages::no_map_folder_beside_exe(game_path))
}

/// Check a list of demos against the map library.
///
/// Map files are read once each however many demos want them, because the
/// checksum walks the whole file and a folder of 400 demos covers maybe twenty
/// maps.
#[tauri::command]
pub async fn check_demo_maps(
    demo_paths: Vec<String>,
    game_path: String,
) -> Result<Vec<MapCheckRow>, String> {
    let dir = maps_dir(&game_path)?;

    tokio::task::spawn_blocking(move || {
        let mut seen: HashMap<String, MapStatus> = HashMap::new();
        let mut rows = Vec::with_capacity(demo_paths.len());

        for path in demo_paths {
            let p = PathBuf::from(&path);
            let demo_name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.clone());

            let reference = match map_check::map_reference(&p) {
                Ok(r) => r,
                Err(e) => {
                    rows.push(MapCheckRow {
                        demo_path: path,
                        demo_name,
                        map_name: String::new(),
                        expected_checksum: None,
                        state: "unreadableDemo".to_string(),
                        detail: e,
                        playable: false,
                    });
                    continue;
                }
            };

            // Keyed on name AND wanted build: two demos of the same map name
            // wanting different builds are genuinely different questions.
            let cache_key = format!(
                "{}:{}",
                reference.map_name,
                reference.expected_checksum.unwrap_or(0)
            );
            let status = seen
                .entry(cache_key)
                .or_insert_with(|| map_check::status_of(&reference, &dir))
                .clone();

            let state = match &status {
                MapStatus::Ok { .. } => "ok",
                MapStatus::WrongBuild { .. } => "wrongBuild",
                MapStatus::Missing => "missing",
                MapStatus::Unverifiable => "unverifiable",
                MapStatus::Unreadable { .. } => "unreadableMap",
            };

            rows.push(MapCheckRow {
                demo_path: path,
                demo_name,
                detail: status.summary(&reference.map_name),
                playable: status.is_playable(),
                map_name: reference.map_name,
                expected_checksum: reference.expected_checksum,
                state: state.to_string(),
            });
        }

        rows
    })
    .await
    .map_err(crate::messages::map_check_failed)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CfgWarningRow {
    pub cvar: String,
    pub value: String,
    pub file: String,
    pub line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CfgOverrideRow {
    pub command: String,
    pub cvar: String,
    pub init_value: String,
    pub cfg_value: String,
    pub file: String,
    pub line: usize,
    /// True when the command comes from the pipeline itself rather than from
    /// something the user typed — those override a config too, and the user has
    /// no other way to find out.
    pub from_app: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CfgShadowRow {
    pub cvar: String,
    /// The command that will not take effect.
    pub shadowed: String,
    pub shadowed_value: String,
    pub winner_value: String,
    /// True when the winning command is one the pipeline appends for itself, in
    /// which case the fix is a setting rather than an edit to this list.
    pub winner_from_app: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomCommandWarning {
    pub command: String,
    pub cvar: String,
    /// `hazard` | `overridesInit` | `overridesConfig`
    pub kind: String,
    /// What this displaces, and where that came from.
    pub replaced_value: String,
    pub source: String,
}

/// A command from `cfg_scan::BANNED_COMMANDS` found in either command list.
/// Refused outright — see that constant's doc comment for why these specific
/// cvars, and not the wider `MID_DEMO_HAZARDS` set, get this treatment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BannedCommandRow {
    pub cvar: String,
    pub command: String,
}

/// A config sets a `cfg_scan::FATAL_CVARS` entry to something other than the
/// one value DoD's own client will not quit the game over.
///
/// Distinct from `BannedCommandRow`: that one is a command the user *typed*
/// into Initial or Scheduled Commands, which the app can simply refuse to run.
/// This is a value already sitting in a config file the app never writes to
/// (see `cfg_scan`'s module doc) -- the most it can do is say so.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CfgFatalRow {
    pub cvar: String,
    pub value: String,
    pub required: String,
    pub file: String,
    pub line: usize,
}

/// A command from `cfg_scan::NOOP_IN_INIT_COMMANDS` or
/// `NOOP_EVERYWHERE_COMMANDS` found somewhere that has no effect — never
/// blocking, just something the user should stop expecting to matter.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoopCommandRow {
    pub cvar: String,
    pub command: String,
    /// "Initial Commands", "Scheduled Commands", or "<file>, line <n>" for a
    /// config the engine executes.
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CfgReport {
    /// Values the pipeline reads that a config sets and no init command names.
    pub unseen: Vec<CfgWarningRow>,
    /// Init commands that will win over a config's value.
    pub overrides: Vec<CfgOverrideRow>,
    /// Init commands beaten by a later entry in the same list.
    pub shadowed: Vec<CfgShadowRow>,
    /// Scheduled commands that displace something, or that must not run mid-demo.
    pub custom: Vec<CustomCommandWarning>,
    /// Banned commands (`cfg_scan::BANNED_COMMANDS`) found in Initial
    /// Commands. Not merely advisory: `start_capture_batch` must refuse to
    /// run while this is non-empty.
    pub banned_init: Vec<BannedCommandRow>,
    /// Same, found in Scheduled (Custom) Commands, plus
    /// `cfg_scan::SCHEDULED_BANNED_COMMANDS` — cvars that are fine in Initial
    /// Commands but refused here because the decal flush sizes itself
    /// against them once, before the demo plays.
    pub banned_scheduled: Vec<BannedCommandRow>,
    /// The `r_decals` ring size this capture will silently use, when nothing
    /// — no config file, no Initial Command — states one (see
    /// `ring_limit_from_init` / `ring_limit_from_game_config`). `None`
    /// whenever Flush Decals Between Clips is off (nothing pins a value at
    /// all) or either of those does state it — their value applies, not the
    /// default. Informational, not a warning: there is nothing wrong with
    /// taking the default, only a silent decision worth surfacing.
    pub decal_default_ring: Option<u32>,
    /// True when Flush Decals Between Clips is on but the `r_decals` value
    /// that will actually reach the engine (`ring_limit`) is 0 — the flush
    /// still runs its full sweep every clip and finds nothing in the ring to
    /// clear, real work for no effect. Reachable by an explicit `r_decals 0`
    /// in Initial Commands, or an executed config stating it with nothing in
    /// Initial Commands overriding — `ring_limit` gives both equal standing,
    /// same as `capture_fov_resolved` does for `mirv_fov`.
    pub decal_flush_is_noop: bool,
    /// Commands that do nothing wherever they were found — see
    /// `cfg_scan::NOOP_IN_INIT_COMMANDS` / `NOOP_EVERYWHERE_COMMANDS`. Found
    /// in Initial Commands, a config the engine executes, or Scheduled
    /// Commands (the last only for `NOOP_EVERYWHERE_COMMANDS` — GoldSrc drops
    /// those from its own message stream regardless of when they arrive;
    /// `NOOP_IN_INIT_COMMANDS` entries are dangerous rather than inert once
    /// scheduled, so they show up in `banned_scheduled` instead).
    pub noop_init: Vec<NoopCommandRow>,
    pub noop_scheduled: Vec<NoopCommandRow>,
    /// `cfg_scan::FATAL_CVARS` entries a config sets wrong -- DoD's own
    /// client quits the game outright the moment it renders a HUD frame with
    /// one of these not at its required value. Not blocking (nothing here can
    /// force a fix to a file the app never writes), but the most severe
    /// warning this report carries: everything else degrades a capture,
    /// this one crashes the game.
    pub fatal_cvars: Vec<CfgFatalRow>,
}

/// Scheduled commands in the order the engine reaches them.
///
/// Everything set before the highlight runs first — larger offsets are further
/// back, so they come earlier — then everything set after it, nearest first.
/// Which one is first matters: only the first command to touch a cvar displaces
/// what the configs and init commands left it at. The rest displace each other.
fn application_order(commands: &[CustomCommandPayload]) -> Vec<&CustomCommandPayload> {
    let is_after = |c: &CustomCommandPayload| c.relation == "After";
    let mut before: Vec<&CustomCommandPayload> =
        commands.iter().filter(|c| !is_after(c)).collect();
    before.sort_by(|a, b| {
        b.offset_seconds
            .partial_cmp(&a.offset_seconds)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut after: Vec<&CustomCommandPayload> = commands.iter().filter(|c| is_after(c)).collect();
    after.sort_by(|a, b| {
        a.offset_seconds
            .partial_cmp(&b.offset_seconds)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    before.into_iter().chain(after).collect()
}

/// What the game's own config files set, and what the app's init commands will
/// override.
///
/// Read-only, and advisory. Nothing in this app writes, edits or removes a
/// config file — the fix is always the user's to make.
#[tauri::command]
pub async fn scan_game_configs(
    game_path: String,
    init_commands: Vec<String>,
    custom_commands: Vec<CustomCommandPayload>,
    capture_fps: Option<i32>,
    decal_flush: Option<bool>,
) -> Result<CfgReport, String> {
    let exe = PathBuf::from(&game_path);
    let Some(dir) = exe.parent().map(|p| p.join("dod")) else {
        return Ok(CfgReport::default());
    };
    if !dir.is_dir() {
        return Ok(CfgReport::default());
    }

    tokio::task::spawn_blocking(move || {
        let scan = native::patch::cfg_scan::scan(&dir);

        // The list the engine will actually receive, so the app's own additions
        // — the movie fps, the decal pin — are checked too. game_path is
        // needed for `ring_limit`/`final_init_commands` to find the same
        // executed configs `scan` above already found — without it, they
        // cannot see anything a config states and would disagree with what a
        // real capture actually does.
        let mut cfg = native::patch::PatcherConfig {
            init_commands: init_commands.clone(),
            game_path: game_path.clone(),
            ..Default::default()
        };
        if let Some(v) = capture_fps {
            cfg.capture_fps = v;
        }
        if let Some(v) = decal_flush {
            cfg.decal_flush = v;
        }

        // Nothing states r_decals themselves — neither Initial Commands nor
        // an auto-executed config (`ring_limit_from_game_config`, same
        // precedence `ring_limit` itself resolves) — so the app's own default
        // is about to silently apply. Worth saying so, even though there is
        // nothing actually wrong with taking the default. Guarded to a
        // nonzero ring: a 0 default would mean the flush is a no-op, which is
        // a different (and louder) fact than "here is the default" — see
        // decal_flush_is_noop below.
        let decal_default_ring = (cfg.decal_flush
            && native::patch::ring_limit_from_init(&cfg.init_commands).is_none()
            && native::patch::ring_limit_from_game_config(&cfg).is_none())
        .then(|| native::patch::ring_limit(&cfg))
        .filter(|&ring| ring > 0);
        // The "louder fact" the comment above alludes to: a 0 ring makes the
        // flush provably pointless, not merely undocumented. Checked
        // independently of decal_default_ring above — this fires whether the
        // 0 came from an explicit Initial Command, an executed config (same
        // precedence as everything else here — see `ring_limit`), or, if it
        // ever becomes settable, decal_ring_limit's own default being 0.
        let decal_flush_is_noop = cfg.decal_flush && native::patch::ring_limit(&cfg) == 0;
        let effective_commands = native::patch::final_init_commands(&cfg);
        let user_typed: std::collections::HashSet<String> =
            init_commands.iter().map(|c| c.trim().to_string()).collect();

        // A command that never applies cannot override anything, so the ones
        // beaten later in the list are dropped rather than reported twice with
        // opposite implications.
        let dead: std::collections::HashSet<String> =
            native::patch::cfg_scan::self_overrides(&effective_commands)
                .into_iter()
                .map(|s| s.shadowed)
                .collect();

        // One row per cvar. Typing `mirv_movie_fps 120` when the app appends the
        // same value produces two commands that both override movie.cfg, and
        // listing the identical consequence twice reads as a bug rather than as
        // two facts. Only the last one applies, so that is the one reported.
        let mut by_cvar: indexmap::IndexMap<String, CfgOverrideRow> = indexmap::IndexMap::new();
        for o in scan.overrides_in(&effective_commands) {
            if dead.contains(&o.command) {
                continue;
            }
            by_cvar.insert(
                o.cvar.to_lowercase(),
                CfgOverrideRow {
                    from_app: !user_typed.contains(&o.command),
                    file: o.file_name(),
                    command: o.command,
                    cvar: o.cvar,
                    init_value: o.init_value,
                    cfg_value: o.cfg_value,
                    line: o.line,
                },
            );
        }
        let overrides: Vec<CfgOverrideRow> = by_cvar.into_iter().map(|(_, v)| v).collect();

        // An init command later in the list beats an earlier one, so a value the
        // user typed can be dead on arrival without anything on screen saying
        // so — the pipeline appends its own commands after theirs.
        let user_count = init_commands.len();
        let shadowed = native::patch::cfg_scan::self_overrides(&effective_commands)
            .into_iter()
            .filter(|s| user_typed.contains(&s.shadowed))
            .map(|s| CfgShadowRow {
                winner_from_app: s.winner_index >= user_count,
                cvar: s.cvar,
                shadowed: s.shadowed,
                shadowed_value: s.shadowed_value,
                winner_value: s.winner_value,
            })
            .collect::<Vec<_>>();

        // Anything the pipeline reads that a config sets and no init command
        // even names — the genuinely silent case. Naming it at the same value
        // still counts as seeing it, so that is not reported as unseen.
        let named: std::collections::HashSet<String> = effective_commands
            .iter()
            .filter_map(|c| c.split_whitespace().next().map(str::to_lowercase))
            .collect();
        // mirv_fov/default_fov are read directly from an executed config
        // whenever neither is stated in Initial Commands
        // (decal_strip::capture_fov_resolved) — no pin ever names them in
        // effective_commands, so `named` alone would never catch that they
        // are seen, and reporting them here too would tell the user to do
        // something the pipeline is already doing. Scoped to exactly that
        // case: a config naming one fov cvar while Initial Commands name the
        // *other* is a real (separate, more involved) cross-cvar precedence
        // question, left alone here.
        //
        // r_decals now works the identical way (`ring_limit_from_game_config`
        // — see `ring_limit`'s doc comment for why it used to be different):
        // a config's own value is read and used directly, with no pin needed
        // to make it "seen" the way `named` otherwise requires.
        let capture_fov_stated = native::patch::capture_fov_from_init(&init_commands).is_some();
        let r_decals_stated_in_init =
            native::patch::ring_limit_from_init(&cfg.init_commands).is_some();
        let unseen = scan
            .effective_settings()
            .into_iter()
            .filter(|s| !named.contains(&s.cvar.to_lowercase()))
            .filter(|s| {
                let is_fov_cvar =
                    s.cvar.eq_ignore_ascii_case("mirv_fov") || s.cvar.eq_ignore_ascii_case("default_fov");
                !is_fov_cvar || capture_fov_stated
            })
            .filter(|s| {
                !s.cvar.eq_ignore_ascii_case("r_decals") || !cfg.decal_flush || r_decals_stated_in_init
            })
            .map(|s| CfgWarningRow {
                cvar: s.cvar.clone(),
                value: s.value.clone(),
                file: s.file_name(),
                line: s.line,
            })
            .collect();

        // Refused outright, in both lists — see BANNED_COMMANDS' doc comment.
        let banned_init: Vec<BannedCommandRow> = native::patch::cfg_scan::banned_commands(&init_commands)
            .into_iter()
            .map(|(cvar, command)| BannedCommandRow { cvar, command })
            .collect();

        // Does nothing wherever found — see NOOP_IN_INIT_COMMANDS's doc
        // comment for why. Only mirv_movie_filename is checked against the
        // config scan: `exec`/`quit` (NOOP_EVERYWHERE_COMMANDS) can never
        // appear there in the first place — the scanner follows a config's
        // own `exec` as the real exec chain it is rather than recording it as
        // a setting, and `quit` takes no argument to record as one either.
        let mut noop_init: Vec<NoopCommandRow> = native::patch::cfg_scan::noop_commands_in_init(&init_commands)
            .into_iter()
            .map(|(cvar, command)| NoopCommandRow { cvar, command, source: "Initial Commands".to_string() })
            .collect();
        for &cvar in native::patch::cfg_scan::NOOP_IN_INIT_COMMANDS {
            if let Some(setting) = scan.effective(cvar) {
                noop_init.push(NoopCommandRow {
                    cvar: cvar.to_string(),
                    command: format!("{} {}", cvar, setting.value),
                    source: format!("{}, line {}", setting.file_name(), setting.line),
                });
            }
        }

        // Custom commands are scheduled into playback, so they run after the
        // configs AND after the init commands — they are the last word on any
        // cvar they touch, and the only place a value can change mid-demo.
        let mut custom = Vec::new();
        let command_texts: Vec<String> =
            custom_commands.iter().map(|c| c.command.clone()).collect();
        let mut banned_scheduled: Vec<BannedCommandRow> = native::patch::cfg_scan::banned_commands(&command_texts)
            .into_iter()
            .map(|(cvar, command)| BannedCommandRow { cvar, command })
            .collect();
        // Fine as Initial Commands — that's how the decal flush is meant to be
        // configured — but refused outright here: see
        // `cfg_scan::SCHEDULED_BANNED_COMMANDS`.
        banned_scheduled.extend(
            native::patch::cfg_scan::scheduled_banned_commands(&command_texts)
                .into_iter()
                .map(|(cvar, command)| BannedCommandRow { cvar, command }),
        );
        // GoldSrc drops these from its own message stream regardless of when
        // they arrive — see NOOP_EVERYWHERE_COMMANDS. mirv_movie_filename is
        // not included here: scheduled, it is dangerous rather than inert
        // (already reported above via banned_scheduled).
        let noop_scheduled: Vec<NoopCommandRow> = native::patch::cfg_scan::noop_commands_in_scheduled(&command_texts)
            .into_iter()
            .map(|(cvar, command)| NoopCommandRow { cvar, command, source: "Scheduled Commands".to_string() })
            .collect();
        // Every scheduled `r_decals` breaks the flush, however many there are,
        // so the hazard list is not deduplicated the way the overrides are.
        for (cvar, command) in native::patch::cfg_scan::mid_demo_hazards(&command_texts) {
            custom.push(CustomCommandWarning {
                command,
                cvar,
                kind: "hazard".to_string(),
                replaced_value: String::new(),
                source: String::new(),
            });
        }

        // Only the FIRST scheduled command to touch a cvar displaces the config
        // or init value. A paired set — `hud_deathnotice_time 555` before the
        // clip and `1` after it — is one override and one restore, and
        // reporting the restore against the config file too says the same thing
        // twice while describing the second one wrongly.
        let mut already_reported: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for payload in application_order(&custom_commands) {
            let command = &payload.command;
            let Some((cvar, _)) = native::patch::cfg_scan::assigned_cvar(command) else {
                continue;
            };
            if custom.iter().any(|w| w.command == command.trim()) {
                continue;
            }
            if !already_reported.insert(cvar.to_lowercase()) {
                continue;
            }
            if let Some(existing) =
                native::patch::cfg_scan::effective_in(&effective_commands, &cvar)
            {
                custom.push(CustomCommandWarning {
                    command: command.trim().to_string(),
                    cvar,
                    kind: "overridesInit".to_string(),
                    replaced_value: existing,
                    source: String::new(),
                });
            } else if let Some(setting) = scan.effective(&cvar) {
                custom.push(CustomCommandWarning {
                    command: command.trim().to_string(),
                    cvar,
                    kind: "overridesConfig".to_string(),
                    replaced_value: setting.value.clone(),
                    source: format!("{}, line {}", setting.file_name(), setting.line),
                });
            }
        }

        // Config-file values DoD's own client will quit the game over --
        // distinct from banned_init/banned_scheduled, which is about commands
        // typed into the pipeline's own fields (see CfgFatalRow's doc comment).
        let fatal_cvars: Vec<CfgFatalRow> = native::patch::cfg_scan::fatal_cvar_hazards(&scan)
            .into_iter()
            .map(|f| CfgFatalRow {
                file: f.file_name(),
                cvar: f.cvar,
                value: f.value,
                required: f.required,
                line: f.line,
            })
            .collect();

        CfgReport {
            unseen,
            overrides,
            shadowed,
            custom,
            banned_init,
            banned_scheduled,
            decal_default_ring,
            decal_flush_is_noop,
            noop_init,
            noop_scheduled,
            fatal_cvars,
        }
    })
    .await
    .map_err(crate::messages::config_scan_failed)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MapFetchResult {
    pub map_name: String,
    pub installed_path: String,
    pub checksum: u32,
    pub bytes: u64,
    pub already_correct: bool,
    /// Where an existing file was moved to, when one was in the way. Nothing is
    /// ever overwritten in place.
    pub replaced_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RollFloorReport {
    pub pre_roll: f32,
    pub pre_roll_floor: f32,
    pub pre_roll_binding: String,
    pub post_roll: f32,
    pub post_roll_floor: f32,
    pub post_roll_binding: String,
    pub audio_resync: f32,
    pub sound_flush: f32,
    pub flush_lead: f32,
    pub scheduled_before: f32,
    pub scheduled_after: f32,
}

/// What the pre-roll and post-roll have to cover for this configuration.
///
/// The rolls stopped being a matter of taste once the audio resync, the sound
/// flush, the decal sweep's lead and the Scheduled Command offsets all started
/// measuring against them. Reported rather than enforced: the terms are
/// knowable, the engine's audio guidance is a 2-4s range rather than a
/// constant, and whether the decal burst itself needs real-time playback is
/// still unverified — so the number is advice, not a clamp.
#[tauri::command]
pub fn roll_floors(
    pre_roll: f32,
    post_roll: f32,
    record_start_lead: Option<f32>,
    record_stop_trail: Option<f32>,
    decal_flush: Option<bool>,
    custom_commands: Vec<CustomCommandPayload>,
) -> RollFloorReport {
    // The lead and trail matter: a scheduled offset anchors to the kill, and
    // recording starts a lead before that, so the lead already covers part of
    // the distance a Before command has to reach back.
    let mut cfg = native::patch::PatcherConfig {
        pre_roll_seconds: pre_roll,
        post_roll_seconds: post_roll,
        record_start_lead: record_start_lead.unwrap_or(0.0),
        record_stop_trail: record_stop_trail.unwrap_or(0.0),
        ..Default::default()
    };
    if let Some(v) = decal_flush {
        cfg.decal_flush = v;
    }
    cfg.custom_commands = custom_commands
        .iter()
        .map(|c| native::patch::CustomCommand {
            command: c.command.clone(),
            offset: c.offset_seconds,
            relation: match c.relation.as_str() {
                "After" => native::patch::CommandRelation::After,
                _ => native::patch::CommandRelation::Before,
            },
        })
        .collect();

    let f = native::patch::builder::roll_floors(&cfg);
    RollFloorReport {
        pre_roll,
        pre_roll_floor: f.pre_roll,
        pre_roll_binding: f.pre_roll_binding.to_string(),
        post_roll,
        post_roll_floor: f.post_roll,
        post_roll_binding: f.post_roll_binding.to_string(),
        audio_resync: f.audio_resync,
        sound_flush: f.sound_flush,
        flush_lead: f.flush_lead,
        scheduled_before: f.scheduled_before,
        scheduled_after: f.scheduled_after,
    }
}

/// Download one map and install it, verified against the build the demo wants.
///
/// This writes into the user's game folder and talks to the network, so it is
/// only ever reached from an explicit action — never from a scan, and never as
/// a side effect of starting a capture.
#[tauri::command]
pub async fn download_map(
    map_name: String,
    expected_checksum: Option<u32>,
    game_path: String,
) -> Result<MapFetchResult, String> {
    let dir = maps_dir(&game_path)?;

    tokio::task::spawn_blocking(move || {
        map_fetch::fetch_map(
            &map_name,
            expected_checksum,
            &dir,
            map_fetch::DEFAULT_MIRROR,
        )
        .map(|o| MapFetchResult {
            map_name: o.map_name,
            installed_path: o.installed.to_string_lossy().to_string(),
            checksum: o.checksum,
            bytes: o.bytes,
            already_correct: o.already_correct,
            replaced_path: o.replaced.map(|p| p.to_string_lossy().to_string()),
        })
    })
    .await
    .map_err(crate::messages::map_download_failed)?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_game_path_with_no_map_folder_says_so_rather_than_failing_later() {
        let err = maps_dir("Z:/nowhere/hl.exe").unwrap_err();
        assert!(err.contains("dod/maps"), "{}", err);
    }

    // ── Config warning report ────────────────────────────────────────────────
    //
    // These drive the real command rather than a extracted helper, so the wiring
    // is covered too: the scan, `final_init_commands`, and the assembly. Two
    // display bugs in this report were found by screenshot, which is the wrong
    // way to find them.

    /// A game folder as the engine expects it: `hl.exe` with `dod/` beside it,
    /// a `config.cfg` that execs `movie.cfg`, and the values that caused all
    /// this in `movie.cfg`.
    fn fake_game(tag: &str) -> String {
        let root = std::env::temp_dir().join(format!("dod_cfgrep_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dod = root.join("dod");
        std::fs::create_dir_all(&dod).unwrap();
        std::fs::write(dod.join("config.cfg"), "bind \"F7\" \"r_decals 4000\"\nexec movie.cfg\n")
            .unwrap();
        std::fs::write(
            dod.join("movie.cfg"),
            // hud_deathnotice_time is here because it is the cvar people
            // genuinely pair around a clip — raised before, restored after.
            "r_decals \"0\"\nmirv_movie_fps \"300\"\nhud_deathnotice_time \"10\"\nmirv_fov \"105\"\n",
        )
        .unwrap();
        let exe = root.join("hl.exe");
        std::fs::write(&exe, b"").unwrap();
        exe.to_string_lossy().to_string()
    }

    /// Same shape as `fake_game`, but movie.cfg never touches `r_decals` at
    /// all — every other `fake_game`-based test relies on it being there
    /// (0, specifically, to exercise the flush's own zero-ring case), so the
    /// one test that needs the genuinely-nothing-anywhere case gets its own
    /// fixture rather than changing that shared one out from under them.
    fn fake_game_without_r_decals(tag: &str) -> String {
        let root = std::env::temp_dir().join(format!("dod_cfgrep_nodecals_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dod = root.join("dod");
        std::fs::create_dir_all(&dod).unwrap();
        std::fs::write(dod.join("config.cfg"), "exec movie.cfg\n").unwrap();
        std::fs::write(dod.join("movie.cfg"), "mirv_movie_fps \"300\"\n").unwrap();
        let exe = root.join("hl.exe");
        std::fs::write(&exe, b"").unwrap();
        exe.to_string_lossy().to_string()
    }

    /// Same shape again, but movie.cfg states `r_decals <value>` and nothing
    /// else — for the tests that need a config-stated value other than the
    /// 0 the shared `fake_game` fixture always carries.
    fn fake_game_with_r_decals(tag: &str, value: &str) -> String {
        let root = std::env::temp_dir().join(format!("dod_cfgrep_decals_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dod = root.join("dod");
        std::fs::create_dir_all(&dod).unwrap();
        std::fs::write(dod.join("config.cfg"), "exec movie.cfg\n").unwrap();
        std::fs::write(dod.join("movie.cfg"), format!("r_decals \"{}\"\n", value)).unwrap();
        let exe = root.join("hl.exe");
        std::fs::write(&exe, b"").unwrap();
        exe.to_string_lossy().to_string()
    }

    /// Same shape again, movie.cfg assigning `mirv_movie_filename` — the
    /// shared `fake_game` fixture's config.cfg only `bind`s it, which is not
    /// an assignment the scanner records at all.
    fn fake_game_with_mirv_movie_filename(tag: &str) -> String {
        let root = std::env::temp_dir().join(format!("dod_cfgrep_moviefn_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dod = root.join("dod");
        std::fs::create_dir_all(&dod).unwrap();
        std::fs::write(dod.join("config.cfg"), "exec movie.cfg\n").unwrap();
        std::fs::write(dod.join("movie.cfg"), "mirv_movie_filename \"clip\"\n").unwrap();
        let exe = root.join("hl.exe");
        std::fs::write(&exe, b"").unwrap();
        exe.to_string_lossy().to_string()
    }

    /// Same shape again, movie.cfg assigning `r_drawentities` to a value other
    /// than 1. Only fatal when the config also turns cheats on: while
    /// `sv_cheats` is 0 GoldSrc clamps the cvar back itself and DoD's client
    /// never sees the value (`cfg_scan::FatalCvar::needs_sv_cheats`).
    fn fake_game_with_r_drawentities(tag: &str, value: &str, cheats: bool) -> String {
        let root = std::env::temp_dir().join(format!("dod_cfgrep_fatal_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dod = root.join("dod");
        std::fs::create_dir_all(&dod).unwrap();
        std::fs::write(dod.join("config.cfg"), "exec movie.cfg\n").unwrap();
        let cheat_line = if cheats { "sv_cheats \"1\"\n" } else { "" };
        std::fs::write(
            dod.join("movie.cfg"),
            format!("{}r_drawentities \"{}\"\n", cheat_line, value),
        )
        .unwrap();
        let exe = root.join("hl.exe");
        std::fs::write(&exe, b"").unwrap();
        exe.to_string_lossy().to_string()
    }

    fn scheduled(command: &str, relation: &str, offset: f32) -> CustomCommandPayload {
        CustomCommandPayload {
            command: command.to_string(),
            relation: relation.to_string(),
            offset_seconds: offset,
        }
    }

    fn report_scheduled(tag: &str, custom: Vec<CustomCommandPayload>) -> CfgReport {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(scan_game_configs(
            fake_game(tag),
            Vec::new(),
            custom,
            Some(120),
            Some(true),
        ))
        .unwrap()
    }

    #[test]
    fn a_set_and_restore_pair_is_one_override_not_two() {
        // The real shape: raise a cvar before the clip, put it back after.
        // Only the first one displaces what movie.cfg left it at — the second
        // displaces the first. Reporting both against the config file says the
        // same thing twice and describes the second one wrongly.
        let r = report_scheduled(
            "pair",
            vec![
                scheduled("hud_deathnotice_time 555", "Before", 10.0),
                scheduled("hud_deathnotice_time 1", "After", 5.0),
            ],
        );

        let rows: Vec<_> = r
            .custom
            .iter()
            .filter(|c| c.cvar.eq_ignore_ascii_case("hud_deathnotice_time"))
            .collect();
        assert_eq!(rows.len(), 1, "{:?}", r.custom);
        assert_eq!(rows[0].command, "hud_deathnotice_time 555", "the one that displaces");
    }

    #[test]
    fn the_earliest_before_command_is_the_one_that_displaces() {
        // Larger "Before" offsets are further back, so they run first.
        let r = report_scheduled(
            "order",
            vec![
                scheduled("hud_deathnotice_time 1", "Before", 2.0),
                scheduled("hud_deathnotice_time 555", "Before", 10.0),
            ],
        );

        let rows: Vec<_> = r
            .custom
            .iter()
            .filter(|c| c.cvar.eq_ignore_ascii_case("hud_deathnotice_time"))
            .collect();
        assert_eq!(rows.len(), 1, "{:?}", r.custom);
        assert_eq!(rows[0].command, "hud_deathnotice_time 555", "10s back runs before 2s back");
    }

    #[test]
    fn every_scheduled_r_decals_is_still_flagged_however_many_there_are() {
        // Deduplication is about which command displaces a config value. Each
        // scheduled r_decals breaks the flush on its own, so they all count.
        let r = report_scheduled(
            "hazards",
            vec![
                scheduled("r_decals 128", "Before", 5.0),
                scheduled("r_decals 4096", "After", 1.0),
            ],
        );

        assert_eq!(r.custom.iter().filter(|c| c.kind == "hazard").count(), 2, "{:?}", r.custom);
    }

    fn report(
        tag: &str,
        init: &[&str],
        custom: &[&str],
        fps: i32,
    ) -> CfgReport {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(scan_game_configs(
            fake_game(tag),
            init.iter().map(|s| s.to_string()).collect(),
            custom.iter().map(|s| scheduled(s, "Before", 2.0)).collect(),
            Some(fps),
            Some(true),
        ))
        .unwrap()
    }

    #[test]
    fn a_cvar_only_a_config_sets_is_reported_as_unseen() {
        // r_decals is only ever named in effective_commands when Flush
        // Decals is on — with it off, nothing pins the cvar at all, so a
        // config setting it really is invisible to the pipeline. That is
        // the genuinely silent case this category exists for.
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let r = rt
            .block_on(scan_game_configs(
                fake_game("unseen"),
                Vec::new(),
                Vec::new(),
                Some(120),
                Some(false),
            ))
            .unwrap();

        assert!(
            r.unseen.iter().any(|u| u.cvar == "r_decals" && u.value == "0"),
            "{:?}",
            r.unseen
        );
    }

    #[test]
    fn a_config_only_fov_is_not_reported_as_unseen() {
        // Regression: decal_strip::capture_fov_resolved already adopts a
        // config's mirv_fov/default_fov whenever Initial Commands state
        // neither — reporting it as unseen here would tell the user to do
        // something the pipeline is already doing (the same reasoning
        // r_decals already gets when Flush Decals is on).
        let r = report("fov_seen_via_resolve", &[], &[], 120);

        assert!(
            !r.unseen.iter().any(|u| u.cvar == "mirv_fov"),
            "capture_fov_resolved already reads this: {:?}",
            r.unseen
        );
        assert!(
            !r.unseen.iter().any(|u| u.cvar == "r_decals"),
            "the decal pin names r_decals when flush is on, so it is not unseen: {:?}",
            r.unseen
        );
    }

    #[test]
    fn naming_a_cvar_at_the_config_s_own_value_still_counts_as_seeing_it() {
        // Regression: matching on differing values put a cvar the user HAD
        // typed into the "the app cannot see these" list.
        let r = report("agrees", &["mirv_fov 105"], &[], 120);

        assert!(
            !r.unseen.iter().any(|u| u.cvar == "mirv_fov"),
            "it is in Init Commands — it is seen: {:?}",
            r.unseen
        );
    }

    #[test]
    fn a_banned_command_in_initial_commands_is_reported() {
        let r = report("banned_init", &["mirv_recordmovie_start"], &[], 120);

        assert_eq!(r.banned_init.len(), 1, "{:?}", r.banned_init);
        assert_eq!(r.banned_init[0].cvar, "mirv_recordmovie_start");
        assert!(r.banned_scheduled.is_empty());
    }

    #[test]
    fn r_drawentities_and_cl_lw_in_initial_commands_are_reported_as_banned() {
        let r = report("banned_fatal_init", &["r_drawentities 0", "cl_lw 0"], &[], 120);

        let cvars: Vec<&str> = r.banned_init.iter().map(|b| b.cvar.as_str()).collect();
        assert_eq!(cvars, vec!["r_drawentities", "cl_lw"], "{:?}", r.banned_init);
    }

    #[test]
    fn r_drawentities_and_cl_lw_in_scheduled_commands_are_reported_as_banned() {
        let r = report("banned_fatal_scheduled", &[], &["r_drawentities 0", "cl_lw 0"], 120);

        let cvars: Vec<&str> = r.banned_scheduled.iter().map(|b| b.cvar.as_str()).collect();
        assert_eq!(cvars, vec!["r_drawentities", "cl_lw"], "{:?}", r.banned_scheduled);
    }

    #[test]
    fn a_config_setting_r_drawentities_to_zero_is_reported_as_fatal() {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let r = rt
            .block_on(scan_game_configs(
                fake_game_with_r_drawentities("fatal_config", "0", true),
                vec![],
                vec![],
                Some(120),
                Some(true),
            ))
            .unwrap();

        assert_eq!(r.fatal_cvars.len(), 1, "{:?}", r.fatal_cvars);
        assert_eq!(r.fatal_cvars[0].cvar, "r_drawentities");
        assert_eq!(r.fatal_cvars[0].value, "0");
        assert_eq!(r.fatal_cvars[0].required, "1");
        assert_eq!(r.fatal_cvars[0].file, "movie.cfg");
    }

    #[test]
    fn r_drawentities_without_sv_cheats_is_not_reported_as_fatal() {
        // The engine clamps it back to 1.0 on its own, so the line is inert
        // and flagging it would block a capture over nothing. Confirmed live:
        // setting r_drawentities with cheats off does not close the game.
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let r = rt
            .block_on(scan_game_configs(
                fake_game_with_r_drawentities("fatal_no_cheats", "0", false),
                vec![],
                vec![],
                Some(120),
                Some(true),
            ))
            .unwrap();

        assert!(r.fatal_cvars.is_empty(), "{:?}", r.fatal_cvars);
    }

    #[test]
    fn a_config_setting_r_drawentities_to_one_is_not_reported_as_fatal() {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let r = rt
            .block_on(scan_game_configs(
                fake_game_with_r_drawentities("fatal_config_ok", "1", true),
                vec![],
                vec![],
                Some(120),
                Some(true),
            ))
            .unwrap();

        assert!(r.fatal_cvars.is_empty(), "{:?}", r.fatal_cvars);
    }

    #[test]
    fn mirv_movie_filename_in_initial_commands_is_reported_as_a_noop_not_banned() {
        let r = report("noop_init_typed", &["mirv_movie_filename foo"], &[], 120);

        assert!(r.banned_init.is_empty(), "{:?}", r.banned_init);
        assert_eq!(r.noop_init.len(), 1, "{:?}", r.noop_init);
        assert_eq!(r.noop_init[0].cvar, "mirv_movie_filename");
        assert_eq!(r.noop_init[0].source, "Initial Commands");
    }

    #[test]
    fn mirv_movie_filename_a_config_states_is_reported_as_a_noop() {
        // fake_game()'s config.cfg has a bind, not an assignment — this needs
        // a fixture that actually assigns the cvar.
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let r = rt
            .block_on(scan_game_configs(
                fake_game_with_mirv_movie_filename("noop_config"),
                Vec::new(),
                Vec::new(),
                Some(120),
                Some(false),
            ))
            .unwrap();

        assert_eq!(r.noop_init.len(), 1, "{:?}", r.noop_init);
        assert_eq!(r.noop_init[0].cvar, "mirv_movie_filename");
        assert!(r.noop_init[0].source.contains("movie.cfg"), "{:?}", r.noop_init[0]);
    }

    #[test]
    fn exec_and_quit_in_initial_commands_are_reported_as_noops() {
        let r = report("noop_exec_quit_init", &["exec somefile.cfg", "quit"], &[], 120);

        let flagged: Vec<&str> = r.noop_init.iter().map(|n| n.cvar.as_str()).collect();
        assert_eq!(flagged, vec!["exec", "quit"], "{:?}", r.noop_init);
        assert!(r.noop_init.iter().all(|n| n.source == "Initial Commands"));
    }

    #[test]
    fn exec_and_quit_in_scheduled_commands_are_reported_as_noops() {
        let r = report("noop_exec_quit_scheduled", &[], &["exec somefile.cfg", "quit"], 120);

        let flagged: Vec<&str> = r.noop_scheduled.iter().map(|n| n.cvar.as_str()).collect();
        assert_eq!(flagged, vec!["exec", "quit"], "{:?}", r.noop_scheduled);
        assert!(r.noop_scheduled.iter().all(|n| n.source == "Scheduled Commands"));
    }

    #[test]
    fn mirv_movie_filename_scheduled_is_banned_not_reported_as_a_noop() {
        let r = report("noop_vs_banned_scheduled", &[], &["mirv_movie_filename foo"], 120);

        assert!(r.noop_scheduled.is_empty(), "{:?}", r.noop_scheduled);
        assert!(r.banned_scheduled.iter().any(|b| b.cvar == "mirv_movie_filename"), "{:?}", r.banned_scheduled);
    }

    #[test]
    fn a_banned_command_in_scheduled_commands_is_reported() {
        let r = report("banned_scheduled", &[], &["host_framerate 0.05"], 120);

        assert_eq!(r.banned_scheduled.len(), 1, "{:?}", r.banned_scheduled);
        assert_eq!(r.banned_scheduled[0].cvar, "host_framerate");
        assert!(r.banned_init.is_empty());
    }

    #[test]
    fn tier_2_cvars_and_initial_command_decal_cvars_are_never_reported_as_banned() {
        // mirv_movie_fps is redundant-with-a-setting, not dangerous, in either
        // list. r_decals/mirv_fov/gl_widescreenfov are exactly how the decal
        // flush is meant to be configured when set once as Initial Commands —
        // only scheduling one of them is refused (see the test below).
        let r = report(
            "not_banned",
            &["mirv_movie_fps 500", "r_decals \"256\"", "mirv_fov 90", "gl_widescreenfov 1"],
            &["mirv_movie_fps 500"],
            120,
        );

        assert!(r.banned_init.is_empty(), "{:?}", r.banned_init);
        assert!(r.banned_scheduled.is_empty(), "{:?}", r.banned_scheduled);
    }

    #[test]
    fn scheduling_a_decal_flush_cvar_is_reported_as_banned() {
        // Fine in Initial Commands (previous test); refused outright once
        // scheduled instead — see cfg_scan::SCHEDULED_BANNED_COMMANDS.
        let r = report(
            "scheduled_decal_cvars_banned",
            &[],
            &["r_decals 512", "mirv_fov 105", "gl_widescreenfov 1"],
            120,
        );

        let flagged: Vec<&str> = r.banned_scheduled.iter().map(|b| b.cvar.as_str()).collect();
        assert_eq!(flagged, vec!["r_decals", "mirv_fov", "gl_widescreenfov"], "{:?}", r.banned_scheduled);
        assert!(r.banned_init.is_empty());
    }

    #[test]
    fn the_default_ring_is_reported_when_nothing_states_r_decals() {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let r = rt
            .block_on(scan_game_configs(
                fake_game_without_r_decals("decal_default_unset"),
                Vec::new(),
                Vec::new(),
                Some(120),
                Some(true),
            ))
            .unwrap();
        assert_eq!(r.decal_default_ring, Some(native::patch::PatcherConfig::default().decal_ring_limit));
    }

    #[test]
    fn a_config_stating_r_decals_zero_is_reported_as_a_flush_noop_not_the_default() {
        // Regression, in two stages. First: this used to fire even though
        // movie.cfg names r_decals (fake_game's movie.cfg always does) — "no
        // r_decals value is set anywhere" was simply false whenever a config
        // states one. Second: once that was fixed to defer to an `overrides`
        // row instead, r_decals stopped being silently pinned to the app's
        // default at all (see cfg_scan / ring_limit's doc comments — a config
        // now gets the same standing Initial Commands do, same as mirv_fov
        // already had) — so there is no override to defer to either, and the
        // config's own 0 flows straight through as the noop it actually is.
        let r = report("decal_default_config_states_it", &[], &[], 120);
        assert_eq!(r.decal_default_ring, None, "{:?}", r.decal_default_ring);
        assert!(r.decal_flush_is_noop, "{:?}", r);
        assert!(
            !r.overrides.iter().any(|o| o.cvar == "r_decals"),
            "nothing overrides it anymore — the config's own value now stands: {:?}",
            r.overrides
        );
    }

    #[test]
    fn the_default_ring_is_not_reported_once_the_user_states_one() {
        let r = report("decal_default_stated", &["r_decals \"512\""], &[], 120);
        assert_eq!(r.decal_default_ring, None);
    }

    #[test]
    fn an_explicit_zero_r_decals_is_reported_as_a_flush_noop() {
        // A stated 0 is respected (same as any other stated value) — the
        // pipeline's pin only ever fires when nothing states one at all — so
        // the flush genuinely runs against an empty ring the whole demo.
        let r = report("decal_noop_stated_zero", &["r_decals \"0\""], &[], 120);
        assert!(r.decal_flush_is_noop, "{:?}", r);
        // Not the same fact as "no value is set anywhere" — a value IS set,
        // it is just zero.
        assert_eq!(r.decal_default_ring, None);
    }

    #[test]
    fn a_nonzero_effective_r_decals_is_not_reported_as_a_flush_noop() {
        // fake_game()'s movie.cfg states r_decals 0 (needed for the test
        // above) and is now genuinely a noop case, so this one needs a
        // fixture that states nothing at all — falling through to the app's
        // nonzero default.
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let r = rt
            .block_on(scan_game_configs(
                fake_game_without_r_decals("decal_noop_nonzero"),
                Vec::new(),
                Vec::new(),
                Some(120),
                Some(true),
            ))
            .unwrap();
        assert!(!r.decal_flush_is_noop, "{:?}", r);
    }

    #[test]
    fn a_nonzero_r_decals_a_config_states_is_not_reported_as_a_flush_noop() {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let r = rt
            .block_on(scan_game_configs(
                fake_game_with_r_decals("decal_noop_config_nonzero", "512"),
                Vec::new(),
                Vec::new(),
                Some(120),
                Some(true),
            ))
            .unwrap();
        assert!(!r.decal_flush_is_noop, "{:?}", r);
        assert!(!r.overrides.iter().any(|o| o.cvar == "r_decals"), "{:?}", r.overrides);
    }

    #[test]
    fn a_zero_r_decals_is_not_reported_as_a_flush_noop_when_flush_is_off() {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let r = rt
            .block_on(scan_game_configs(
                fake_game("decal_noop_flush_off"),
                vec!["r_decals \"0\"".to_string()],
                Vec::new(),
                Some(120),
                Some(false),
            ))
            .unwrap();
        assert!(!r.decal_flush_is_noop, "{:?}", r);
    }

    #[test]
    fn the_default_ring_is_not_reported_when_flush_is_off() {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let r = rt
            .block_on(scan_game_configs(
                fake_game("decal_default_flush_off"),
                Vec::new(),
                Vec::new(),
                Some(120),
                Some(false),
            ))
            .unwrap();
        assert_eq!(r.decal_default_ring, None);
    }

    #[test]
    fn a_quoted_r_decals_the_user_typed_is_respected_not_shadowed() {
        // Regression: real .cfg syntax quotes every value, and the app used
        // to parse r_decals from Initial Commands without unquoting first —
        // `r_decals "512"` silently read as "nothing stated" and the app
        // appended its own default afterward, shadowing the user's own line
        // even though nothing was actually wrong with it.
        let r = report("quoted_decals", &["r_decals \"512\""], &[], 120);

        assert!(
            !r.shadowed.iter().any(|s| s.cvar.eq_ignore_ascii_case("r_decals")),
            "the user's own r_decals must not be reported as dead: {:?}",
            r.shadowed
        );
    }

    #[test]
    fn one_override_row_per_cvar_even_when_two_commands_set_it() {
        // Regression: typing the value the app also appends produced two rows
        // saying the identical thing, which reads as a bug rather than as two
        // facts.
        let r = report("dupe", &["mirv_movie_fps 120"], &[], 120);

        let fps_rows: Vec<_> = r
            .overrides
            .iter()
            .filter(|o| o.cvar.eq_ignore_ascii_case("mirv_movie_fps"))
            .collect();
        assert_eq!(fps_rows.len(), 1, "{:?}", r.overrides);
        assert_eq!(fps_rows[0].cfg_value, "300");
    }

    #[test]
    fn a_typed_command_the_app_overrides_is_reported_against_the_setting() {
        // The screenshot case: mirv_movie_fps 500 typed by hand, Capture FPS at
        // 120 appended after it. The typed value never applies.
        let r = report("shadow", &["mirv_movie_fps 500"], &[], 120);

        assert_eq!(r.shadowed.len(), 1, "{:?}", r.shadowed);
        assert_eq!(r.shadowed[0].shadowed_value, "500");
        assert_eq!(r.shadowed[0].winner_value, "120");
        assert!(r.shadowed[0].winner_from_app, "the app appended the winner");

        assert!(
            !r.overrides.iter().any(|o| o.command == "mirv_movie_fps 500"),
            "a command that never applies overrides nothing: {:?}",
            r.overrides
        );
    }

    #[test]
    fn a_scheduled_r_decals_is_reported_as_breaking_the_flush() {
        let r = report("hazard", &[], &["r_decals 128"], 120);

        let hazards: Vec<_> = r.custom.iter().filter(|c| c.kind == "hazard").collect();
        assert_eq!(hazards.len(), 1, "{:?}", r.custom);
        assert_eq!(hazards[0].cvar, "r_decals");
        assert_eq!(
            r.custom.iter().filter(|c| c.command == "r_decals 128").count(),
            1,
            "the hazard must not also be listed as an ordinary override"
        );
    }

    #[test]
    fn a_scheduled_command_is_reported_against_whatever_it_displaces() {
        // Scheduled commands run last of all, so they beat the init commands as
        // well as the configs. Not mirv_fov/r_decals/gl_widescreenfov — those
        // are hazards regardless of what they'd otherwise displace, and are
        // covered by their own tests below.
        let r = report("custom", &["sensitivity 3"], &["sensitivity 5"], 120);

        let row = r
            .custom
            .iter()
            .find(|c| c.cvar.eq_ignore_ascii_case("sensitivity"))
            .expect("reported");
        assert_eq!(row.kind, "overridesInit");
        assert_eq!(row.replaced_value, "3");
    }

    #[test]
    fn a_bound_key_in_a_config_is_never_treated_as_a_setting() {
        // config.cfg here binds F7 to `r_decals 4000`. Nothing happens until
        // someone presses F7, and warning about it would be noise.
        let r = report("bind", &[], &[], 120);

        assert!(
            !r.overrides.iter().any(|o| o.cfg_value == "4000"),
            "{:?}",
            r.overrides
        );
    }
}
