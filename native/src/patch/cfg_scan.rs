// patch/cfg_scan.rs
// What the game's own config files set behind the pipeline's back.
//
// STRICTLY READ-ONLY. This module never writes, edits, moves or deletes a
// config file. The user's configs are theirs; the most this does is say what it
// found and let them decide.
//
// The problem it solves is concrete. The flush sizes its sweep to `r_decals`
// and derives its on-screen test from `mirv_fov`, and both were assumed to come
// from the app's own init commands. They do not have to. A `config.cfg` ending
// in `exec movie.cfg`, and a `movie.cfg` setting `mirv_fov 105`, means the
// engine renders at a FOV the pipeline never hears about — and the only symptom
// is a slightly-too-narrow cone deciding what a camera can see.
//
// Two things this deliberately does not treat as settings:
//
//   * `bind "F7" "r_decals 4000"` — the cvar is a payload, not an execution.
//     Nothing happens until a key is pressed.
//   * `alias foo "r_decals 0"` — same.
//
// Both appear in real DoD configs, and counting them would raise a warning
// about a value the engine never applied.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Cvars the pipeline reads or depends on. Anything here, set anywhere but the
/// app's init commands, is worth telling the user about.
pub const WATCHED_CVARS: &[&str] = &["r_decals", "mirv_fov", "default_fov"];

/// Configs the engine executes on its own at start-up. Everything else is only
/// reached by being `exec`'d from one of these.
const ENTRY_POINTS: &[&str] = &["valve.rc", "config.cfg", "autoexec.cfg", "userconfig.cfg"];

/// Bounds on following `exec` chains, so a config that execs itself — or a
/// folder of hundreds — cannot turn a scan into a hang.
const MAX_DEPTH: usize = 8;
const MAX_FILES: usize = 64;
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// A full `config.cfg` is a couple of hundred assignments. This is a ceiling on
/// something pathological, not a working limit.
const MAX_SETTINGS: usize = 4096;

/// Commands whose arguments are stored, not run.
const NOT_EXECUTED: &[&str] = &["bind", "unbind", "alias", "bindtoggle", "+bind"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CvarSetting {
    pub cvar: String,
    pub value: String,
    pub file: PathBuf,
    pub line: usize,
    /// Whether this file is reached from a config the engine runs by itself.
    /// A config nobody execs sets nothing.
    pub auto_executed: bool,
}

impl CvarSetting {
    pub fn file_name(&self) -> String {
        self.file
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.file.to_string_lossy().to_string())
    }
}

#[derive(Debug, Clone, Default)]
pub struct CfgScan {
    /// Every watched cvar assignment found, in the order the engine would reach
    /// them — so the last entry for a cvar is the one that wins.
    pub settings: Vec<CvarSetting>,
    pub files_read: usize,
    /// Configs present in the folder that nothing execs. Reported separately:
    /// they set nothing today, but a `movie.cfg` is one `exec` away from doing
    /// so, and a user reading a warning deserves to know it is there.
    pub unreferenced: Vec<PathBuf>,
}

impl CfgScan {
    /// The value the engine would end up with for a cvar, considering only
    /// configs it actually executes. Last one wins, as the console does.
    pub fn effective(&self, cvar: &str) -> Option<&CvarSetting> {
        self.settings
            .iter().rfind(|s| s.auto_executed && s.cvar.eq_ignore_ascii_case(cvar))
    }

    /// Every watched cvar that an executed config sets.
    pub fn effective_settings(&self) -> Vec<&CvarSetting> {
        WATCHED_CVARS
            .iter()
            .filter_map(|c| self.effective(c))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.settings.is_empty() && self.unreferenced.is_empty()
    }

    /// Which of these init commands will override something a config already
    /// sets.
    ///
    /// Init commands reach the engine after its configs have run, so where both
    /// name the same cvar the init command is the one that takes effect. That
    /// is usually what the user wants — it is the whole point of putting it
    /// there — but they should not have to find out by noticing their movie
    /// looks different. Silence here would mean a value they had set
    /// deliberately, years ago, quietly stopping applying.
    pub fn overrides_in(&self, init_commands: &[String]) -> Vec<CommandOverride> {
        let mut out = Vec::new();
        for command in init_commands {
            let trimmed = command.trim();
            let mut parts = trimmed.split_whitespace();
            let Some(head) = parts.next() else { continue };
            let Some(value) = parts.next() else { continue };
            if parts.next().is_some() {
                continue;
            }
            let Some(setting) = self.effective(head) else {
                continue;
            };
            // Setting it to what the config already says overrides nothing that
            // anyone would notice.
            if setting.value.eq_ignore_ascii_case(&unquote(value)) {
                continue;
            }
            out.push(CommandOverride {
                command: trimmed.to_string(),
                cvar: setting.cvar.clone(),
                init_value: unquote(value),
                cfg_value: setting.value.clone(),
                file: setting.file.clone(),
                line: setting.line,
            });
        }
        out
    }
}

/// One init command silently beaten by a later one in the same list.
///
/// The list the engine receives is the user's own init commands followed by the
/// ones the pipeline appends for itself, so a `mirv_movie_fps 500` typed by hand
/// is overwritten by the Capture FPS setting a few entries later. Nothing about
/// the list on screen shows that: both lines are there, and only the last one
/// happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandShadow {
    pub cvar: String,
    /// The command that will not take effect.
    pub shadowed: String,
    pub shadowed_value: String,
    /// The one that will.
    pub winner: String,
    pub winner_value: String,
    /// Position of the winner in the list, so a caller can tell whether it came
    /// from the user's own entries or was appended after them.
    pub winner_index: usize,
}

/// Cvars that must not change once a demo is playing.
///
/// `r_decals` is the reason this list exists. It bounds how far the engine's
/// rotating decal index may travel before it wraps; it evicts nothing.
/// Lowering it mid-demo strands every decal sitting above the new limit —
/// permanently, for the rest of playback — and the decal flush's entire
/// design rests on the ring being set once at demo load and never touched
/// again.
///
/// `mirv_fov` is the same shape of problem: `decal_strip::capture_fov_resolved`
/// reads it once, from `init_commands`/the detected game config, as a pre-pass
/// before the demo plays, and sizes the whole sweep's on-screen test against
/// that single value. A Scheduled Command changing it mid-demo does not
/// retroactively resize anything — the flush already decided what counts as
/// on screen for the entire clip.
///
/// `gl_widescreenfov` widens the effective on-screen FOV for a wide aspect
/// ratio the same way `mirv_fov`/`default_fov` do, but `capture_fov_resolved`
/// never reads it at all — the pipeline has no idea it exists, let alone that
/// it changed. A mid-demo toggle is strictly worse than a mid-demo `mirv_fov`:
/// the flush's sizing goes wrong with nothing anywhere that could have caught it.
///
/// The rest are `builder::write_helper_cfg`'s own recording mechanics —
/// `mirv_movie_filename` is what the `<demo>_route_N` aliases set once per
/// block to route that block's frames to the right take folder, and a
/// scheduled one firing mid-clip would silently write frames into whatever
/// folder it named instead, with nothing to notice the manifest and the disk
/// have diverged. `mirv_recordmovie_start`/`_stop` are what `sys_record_start`/
/// `sys_record_stop` schedule at the block's own record bounds — a stray one
/// races that and can start or end a take at the wrong tick. `mirv_movie_fps`
/// is pinned once at load (see `builder::final_init_commands`) and everything
/// downstream — the fps stamped into take metadata, Render Studio's own
/// expectation — assumes that never changes mid-batch. `mirv_movie_ffmpeg`
/// configures the direct-to-video encoder pipe the same way, once, before
/// anything records into it.
///
/// `mirv_movie_separate_hud` deliberately is NOT here. It used to be, but the
/// only reason was that the pipeline always re-appended its own value to
/// Initial Commands after the user's, making anything the user set — Initial
/// or Scheduled — moot. That checkbox is gone (removed 2026-09-08; typing the
/// command into Initial Commands directly is the only way to use it now), and
/// with it the one confirmed reason to flag this cvar at all. Nothing in this
/// codebase has actually tested what a mid-demo toggle does — unlike
/// `r_decals`/`mirv_fov`, which are measured, this would be a guess by
/// analogy, so it stays untracked rather than asserting a mechanism nobody
/// has verified.
/// `host_framerate` is `sys_fast_forward`/`sys_normal_speed`'s own mechanism
/// for the real-time run-up before recording (`docs/goldsrc_dod_quirks.md`'s
/// audio-resync entry) — a scheduled one races that timing, not the record
/// itself (recording pins its own timestep regardless).
///
/// All of these share the same failure mode: the capture still completes and
/// still looks plausible.
pub const MID_DEMO_HAZARDS: &[&str] = &[
    "r_decals",
    "mirv_fov",
    "gl_widescreenfov",
    "mirv_movie_filename",
    "mirv_recordmovie_start",
    "mirv_recordmovie_stop",
    "mirv_movie_fps",
    "mirv_movie_ffmpeg",
    "host_framerate",
];

/// Commands refused wherever a command can be typed (Initial Commands and
/// Scheduled Commands alike), not merely shadowed or flagged as a mid-demo
/// hazard the way the rest of `MID_DEMO_HAZARDS` is. Two different reasons
/// land a command here:
///
///   * **the pipeline owns it outright** — no dedicated setting exists, and no
///     scenario has been found where a user typing one is anything but a
///     misunderstanding (`mirv_recordmovie_start`/`_stop`, `mirv_movie_ffmpeg`,
///     `host_framerate`);
///   * **DoD's own client has no legitimate non-default value for it at
///     all** — `r_drawentities`, `cl_lw`.
///
/// Distinct from `mirv_movie_fps`, which the pipeline also always pins but
/// which corresponds to a real setting (Output Format → Capture FPS) —
/// typing that is redundant, not dangerous, so it stays shadowed-with-a-
/// warning rather than refused. `mirv_movie_separate_hud` is not here either,
/// and not in `MID_DEMO_HAZARDS`: no setting exists behind it any more
/// (removed 2026-09-08), Initial Commands is simply the intended way to use
/// it, and typing it in Scheduled Commands instead is untracked rather than
/// flagged — see `MID_DEMO_HAZARDS`'s own doc comment for why. Also distinct
/// from `mirv_movie_filename`, which used to be here too — see
/// `SCHEDULED_BANNED_COMMANDS` for why it moved. User-confirmed tier list,
/// 2026-09-02 (`mirv_movie_filename` re-tiered 2026-09-05, `r_drawentities`/
/// `cl_lw` added 2026-09-08).
///
/// - `mirv_recordmovie_start` / `mirv_recordmovie_stop` — the pipeline's own
///   `sys_record_start`/`sys_record_stop` scheduling relies on being the only
///   thing calling these, at exactly the block's own record bounds.
/// - `mirv_movie_ffmpeg` — the direct-to-video encoder pipe, configured once
///   before anything records into it.
/// - `host_framerate` — `sys_fast_forward`/`sys_normal_speed`'s own mechanism
///   for the real-time run-up before recording. Floated as possibly having a
///   legitimate creative use (frame-by-frame stepping, per
///   `docs/goldsrc_dod_quirks.md`'s High-Precision Frame Pacing entry) and
///   rejected: "It's dangerous and nobody uses that."
/// - `r_drawentities` / `cl_lw` — DoD 1.3's `client.dll` runs a cvar-enforcement
///   check inside `CHud::Redraw`: if either is not `1`, it forces the correct
///   value back, prints an error, and calls `quit` — the process exits
///   outright rather than merely correcting course. Refused here for both,
///   because Initial/Scheduled Commands are cheap to restrict and this module
///   cannot see whether a user's own configs also turned cheats on.
///
///   The two are *not* equally reachable, though, and `FATAL_CVARS` draws the
///   distinction that matters when scanning a user's config files. `cl_lw` is
///   an ordinary client cvar and simply takes the value it is given.
///   `r_drawentities` is on GoldSrc's own hardcoded clamp list: while
///   `sv_cheats` is `0` the engine resets it to `1.0` and the value never
///   survives long enough for DoD's client to see it. Verified live and in
///   the binaries — `hw.dll` `0x1d455c9`, gated on `sv_cheats` at `0x1e56404`.
pub const BANNED_COMMANDS: &[&str] = &[
    "mirv_recordmovie_start",
    "mirv_recordmovie_stop",
    "mirv_movie_ffmpeg",
    "host_framerate",
    "r_drawentities",
    "cl_lw",
];

/// A cvar DoD's own client quits the game over, plus what it takes for that to
/// actually be reachable.
///
/// A command typed into Initial or Scheduled Commands is refused outright via
/// `BANNED_COMMANDS` — but a config file the user already has is a different
/// problem: nothing here writes to it (STRICTLY READ-ONLY, module-level
/// doc), so the most this can do is detect and warn. See `fatal_cvar_hazards`.
///
/// Both cvars are checked inside `CHud::Redraw` (`client.dll` RVA `0x1936e20`),
/// so the failure is not confined to start-up: a config that only sets one
/// after the HUD is already drawing is exactly as fatal as setting it before
/// launch. The check runs when the HUD *draws*, which is also why it does not
/// fire while the console is open — see `docs/goldsrc_dod_quirks.md`.
pub struct FatalCvar {
    pub cvar: &'static str,
    /// The only value that does not quit the game.
    pub required: &'static str,
    /// Whether reaching DoD's client requires cheats to be on.
    ///
    /// GoldSrc clamps some renderer cvars itself, on a hardcoded list gated on
    /// `sv_cheats` (`hw.dll` `0x1d455c9`). While `sv_cheats` is `0` the engine
    /// resets `r_drawentities` to `1.0` and the value never survives to be
    /// read, so a config setting it is inert — reporting that as fatal would
    /// block a capture over a harmless line. `cl_lw` has no such clamp and is
    /// always fatal.
    pub needs_sv_cheats: bool,
}

pub const FATAL_CVARS: &[FatalCvar] = &[
    FatalCvar { cvar: "r_drawentities", required: "1", needs_sv_cheats: true },
    FatalCvar { cvar: "cl_lw", required: "1", needs_sv_cheats: false },
];

/// Whether an executed config turns cheats on, which is what decides if a
/// `needs_sv_cheats` entry can reach DoD's client at all. Any non-zero numeric
/// value counts; anything unparseable is treated as off, matching the engine's
/// own float coercion of a cvar string.
fn sv_cheats_enabled(scan: &CfgScan) -> bool {
    scan.effective("sv_cheats")
        .is_some_and(|s| s.value.trim().parse::<f32>().is_ok_and(|v| v != 0.0))
}

/// One cvar a config sets to a value DoD's own client will quit the game over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FatalCvarSetting {
    pub cvar: String,
    pub value: String,
    /// The only value that does not crash — always `"1"` today, but named
    /// rather than hardcoded so a caller's message stays correct if
    /// `FATAL_CVARS` ever gains an entry with a different one.
    pub required: String,
    pub file: PathBuf,
    pub line: usize,
}

impl FatalCvarSetting {
    pub fn file_name(&self) -> String {
        self.file
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.file.to_string_lossy().to_string())
    }
}

/// Every `FATAL_CVARS` entry an executed config sets to something other than
/// its required value.
///
/// Uses `CfgScan::effective`, the same last-one-wins resolution as everything
/// else here — a `movie.cfg` that first sets `r_drawentities 0` and later
/// corrects it back to `1` is not a hazard, because that is genuinely the
/// value the engine ends up with.
pub fn fatal_cvar_hazards(scan: &CfgScan) -> Vec<FatalCvarSetting> {
    let cheats = sv_cheats_enabled(scan);
    FATAL_CVARS
        .iter()
        .filter_map(|entry| {
            if entry.needs_sv_cheats && !cheats {
                // The engine clamps it back before anything downstream reads
                // it, so the config line is inert rather than fatal.
                return None;
            }
            let setting = scan.effective(entry.cvar)?;
            (setting.value != entry.required).then(|| FatalCvarSetting {
                cvar: setting.cvar.clone(),
                value: setting.value.clone(),
                required: entry.required.to_string(),
                file: setting.file.clone(),
                line: setting.line,
            })
        })
        .collect()
}

/// Commands from `list` that appear in `commands`, as (matched cvar, whole
/// trimmed line) pairs, in the order they were found.
fn commands_matching(list: &[&str], commands: &[String]) -> Vec<(String, String)> {
    commands
        .iter()
        .filter_map(|raw| {
            let trimmed = raw.trim();
            let head = trimmed.split_whitespace().next()?;
            list.iter()
                .find(|h| head.eq_ignore_ascii_case(h))
                .map(|h| ((*h).to_string(), trimmed.to_string()))
        })
        .collect()
}

/// Commands from `MID_DEMO_HAZARDS` that must not be run during playback.
pub fn mid_demo_hazards(commands: &[String]) -> Vec<(String, String)> {
    commands_matching(MID_DEMO_HAZARDS, commands)
}

/// Commands from `BANNED_COMMANDS` present anywhere in `commands` — Initial
/// or Scheduled alike. Refused outright, not merely shadowed or flagged.
pub fn banned_commands(commands: &[String]) -> Vec<(String, String)> {
    commands_matching(BANNED_COMMANDS, commands)
}

/// Cvars that are fine — expected, even — in Initial Commands, but must be
/// refused outright rather than merely flagged as a `MID_DEMO_HAZARDS`
/// warning when they show up in Scheduled Commands instead.
///
/// `r_decals`, `mirv_fov` and `gl_widescreenfov` all feed the decal flush's
/// one-time sizing pre-pass: `r_decals` bounds the decal ring, `mirv_fov`
/// sizes the on-screen sweep test against `decal_strip::capture_fov_resolved`'s
/// single read of it before the demo plays, and `gl_widescreenfov` widens the
/// effective FOV the same way without the flush even knowing the cvar exists.
/// A Scheduled Command changing any of them mid-demo does not retroactively
/// resize anything — the flush already decided what counts as on screen for
/// the entire clip — so a warning banner isn't enough here the way it is for
/// the rest of `MID_DEMO_HAZARDS`: the capture would complete and look
/// plausible while quietly being wrong. User-requested escalation from hazard
/// to refused, 2026-09-05.
///
/// `mirv_movie_filename` is a different shape of exception, moved here from
/// `BANNED_COMMANDS` the same day: in Initial Commands (or a config) it is
/// not merely safe, it is inert — `build_batch_queue` schedules a fresh
/// `<demo>_route_N` alias (which sets it) at the same tick as every block's
/// own `sys_record_start`, for every block including the first, so a value
/// stated at demo load never survives to any actual recording (see
/// `NOOP_IN_INIT_COMMANDS`, which reports exactly that). Scheduled instead,
/// it fires mid-clip — between one block's route alias and the next — and
/// genuinely misroutes that block's frames, which is the danger it was
/// originally banned everywhere for.
pub const SCHEDULED_BANNED_COMMANDS: &[&str] =
    &["r_decals", "mirv_fov", "gl_widescreenfov", "mirv_movie_filename"];

/// Commands GoldSrc itself silently drops whenever they arrive via a demo's
/// own message stream — Initial Commands (STUFFTEXT, injected right after
/// `DemoStart`) and Scheduled Commands (injected `ConsoleCommand` frames)
/// alike, since the engine's filter does not care when in the stream a
/// command arrives. Confirmed the hard way, twice: `dod-tools` originally
/// planned to `exec` a per-demo generated config to set `mirv_movie_filename`
/// (dodging the quoting/escaping an alias would need) and to inject `quit` at
/// batch end to close the game automatically — neither ever did anything.
/// See `docs/hlae_protocols.md`'s "Sandbox Escape" entry, which documents the
/// actual workaround this pipeline ships instead of `quit` (a
/// `mirv_movie_filename` exit-trigger folder, polled for by the orchestrator).
///
/// Does NOT apply to a command inside a game config the engine execs
/// normally at boot (`config.cfg`, `movie.cfg`, ...) — that is a completely
/// different code path from the demo's own message stream, and an `exec`
/// inside one of those is exactly how config chaining works.
pub const NOOP_EVERYWHERE_COMMANDS: &[&str] = &["exec", "quit"];

/// Commands that do nothing specifically in Initial Commands (or a config
/// the engine executes) — not because the engine drops them, but because the
/// pipeline itself always overwrites the value before anything downstream
/// could read it. Scheduling one instead is a different, genuinely dangerous
/// story — see `SCHEDULED_BANNED_COMMANDS`.
pub const NOOP_IN_INIT_COMMANDS: &[&str] = &["mirv_movie_filename"];

/// `NOOP_IN_INIT_COMMANDS` and `NOOP_EVERYWHERE_COMMANDS` combined, checked
/// against Initial Commands.
pub fn noop_commands_in_init(commands: &[String]) -> Vec<(String, String)> {
    let mut hits = commands_matching(NOOP_IN_INIT_COMMANDS, commands);
    hits.extend(commands_matching(NOOP_EVERYWHERE_COMMANDS, commands));
    hits
}

/// The Scheduled Commands half of the same check — `NOOP_EVERYWHERE_COMMANDS`
/// only. `NOOP_IN_INIT_COMMANDS` entries are dangerous rather than inert once
/// scheduled (see `SCHEDULED_BANNED_COMMANDS`), so they are refused there,
/// not reported here.
pub fn noop_commands_in_scheduled(commands: &[String]) -> Vec<(String, String)> {
    commands_matching(NOOP_EVERYWHERE_COMMANDS, commands)
}

/// Commands from `SCHEDULED_BANNED_COMMANDS` present in `commands`. Only
/// meaningful against Scheduled Commands — these three are exactly how the
/// decal flush is meant to be configured when used as Initial Commands, so
/// callers must not run this against `init_commands`.
pub fn scheduled_banned_commands(commands: &[String]) -> Vec<(String, String)> {
    commands_matching(SCHEDULED_BANNED_COMMANDS, commands)
}

/// What a list of commands leaves a cvar set to, last one winning.
pub fn effective_in(commands: &[String], cvar: &str) -> Option<String> {
    commands.iter().rev().find_map(|raw| {
        let trimmed = raw.trim();
        let mut parts = trimmed.split_whitespace();
        let head = parts.next()?;
        let value = parts.next()?;
        if parts.next().is_some() || !head.eq_ignore_ascii_case(cvar) {
            return None;
        }
        Some(unquote(value))
    })
}

/// The cvar a command assigns to, if it assigns to one at all.
pub fn assigned_cvar(command: &str) -> Option<(String, String)> {
    let trimmed = command.trim();
    let mut parts = trimmed.split_whitespace();
    let head = parts.next()?;
    let value = parts.next()?;
    if parts.next().is_some() || !is_cvar_name(head) {
        return None;
    }
    Some((head.to_string(), unquote(value)))
}

/// Commands in this list that a later command in the same list overrides.
///
/// Last one wins, as the console does — so every entry for a cvar except the
/// final one is dead, and each is reported against the one that beat it.
pub fn self_overrides(commands: &[String]) -> Vec<CommandShadow> {
    let assignments: Vec<(usize, String, String, String)> = commands
        .iter()
        .enumerate()
        .filter_map(|(i, raw)| {
            let trimmed = raw.trim();
            let mut parts = trimmed.split_whitespace();
            let head = parts.next()?;
            let value = parts.next()?;
            if parts.next().is_some() || !is_cvar_name(head) {
                return None;
            }
            Some((i, head.to_lowercase(), unquote(value), trimmed.to_string()))
        })
        .collect();

    let mut out = Vec::new();
    for (index, (_, cvar, value, command)) in assignments.iter().enumerate() {
        let Some((w_pos, _, w_value, w_command)) = assignments
            .iter()
            .skip(index + 1)
            .find(|(_, other, _, _)| other == cvar)
        else {
            continue;
        };
        // Repeating a value changes nothing anyone would notice.
        if value.eq_ignore_ascii_case(w_value) {
            continue;
        }
        out.push(CommandShadow {
            cvar: cvar.clone(),
            shadowed: command.clone(),
            shadowed_value: value.clone(),
            winner: w_command.clone(),
            winner_value: w_value.clone(),
            winner_index: *w_pos,
        });
    }
    out
}

/// An init command that will take precedence over a config file's value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOverride {
    pub command: String,
    pub cvar: String,
    pub init_value: String,
    pub cfg_value: String,
    pub file: PathBuf,
    pub line: usize,
}

impl CommandOverride {
    pub fn file_name(&self) -> String {
        self.file
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.file.to_string_lossy().to_string())
    }
}

/// Scan a mod folder's configs. Never writes anything.
///
/// `game_dir` is the folder holding the configs — for DoD, `<hl.exe dir>/dod`.
pub fn scan(game_dir: &Path) -> CfgScan {
    let mut out = CfgScan::default();
    if !game_dir.is_dir() {
        return out;
    }

    let mut visited: HashSet<PathBuf> = HashSet::new();
    for entry in ENTRY_POINTS {
        let path = game_dir.join(entry);
        if path.is_file() {
            read_config(&path, game_dir, 0, &mut visited, &mut out, true);
        }
    }

    // Configs sitting in the folder that nothing reached.
    if let Ok(dir) = std::fs::read_dir(game_dir) {
        let mut loose: Vec<PathBuf> = dir
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.is_file()
                    && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("cfg"))
                    && !visited.contains(&normalise(p))
            })
            .collect();
        loose.sort();
        out.unreferenced = loose;
    }

    out
}

/// `scan`, reading each folder at most once per process.
///
/// The pipeline asks per demo — once to resolve the capture FOV and once to
/// report — so a batch of forty demos would otherwise re-read and re-parse the
/// same configs eighty times. Configs do not change mid-batch, and if the user
/// edits one the next run picks it up.
pub fn scan_cached(game_dir: &Path) -> std::sync::Arc<CfgScan> {
    static CACHE: std::sync::OnceLock<
        std::sync::RwLock<std::collections::HashMap<PathBuf, std::sync::Arc<CfgScan>>>,
    > = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let key = normalise(game_dir);

    if let Ok(read) = cache.read()
        && let Some(hit) = read.get(&key) {
            return std::sync::Arc::clone(hit);
        }

    let scanned = std::sync::Arc::new(scan(game_dir));
    if let Ok(mut write) = cache.write() {
        // Another thread may have inserted while this one was scanning. Either
        // result is correct; keeping the stored one keeps them all identical.
        return std::sync::Arc::clone(write.entry(key).or_insert(scanned));
    }
    scanned
}

/// `dirs::canonicalize` would resolve symlinks and fail on missing files; all
/// this needs is a stable key so an `exec` cycle is recognised.
fn normalise(path: &Path) -> PathBuf {
    PathBuf::from(path.to_string_lossy().to_lowercase().replace('\\', "/"))
}

fn read_config(
    path: &Path,
    game_dir: &Path,
    depth: usize,
    visited: &mut HashSet<PathBuf>,
    out: &mut CfgScan,
    auto_executed: bool,
) {
    if depth > MAX_DEPTH || out.files_read >= MAX_FILES {
        return;
    }
    if !visited.insert(normalise(path)) {
        return;
    }
    if std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) > MAX_FILE_BYTES {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    out.files_read += 1;

    for (index, raw) in text.lines().enumerate() {
        for command in commands_in(raw) {
            let mut parts = command.split_whitespace();
            let Some(head) = parts.next() else { continue };
            let head_lower = head.to_lowercase();

            if NOT_EXECUTED.contains(&head_lower.as_str()) {
                continue;
            }

            if head_lower == "exec" {
                if let Some(target) = parts.next() {
                    let name = unquote(target);
                    // Config paths are relative to the mod folder, and a config
                    // is not a place to accept a path that climbs out of it.
                    if name.contains("..") || name.starts_with('/') || name.starts_with('\\') {
                        continue;
                    }
                    let next = game_dir.join(&name);
                    let next = if next.extension().is_none() {
                        next.with_extension("cfg")
                    } else {
                        next
                    };
                    if next.is_file() {
                        read_config(&next, game_dir, depth + 1, visited, out, auto_executed);
                    }
                }
                continue;
            }

            // Every assignment is recorded, not just the ones the pipeline
            // reads, so an init command can be checked against whatever the
            // user actually has — most people's configs do not mention
            // `r_decals` at all, and the ones that surprise you are the ones
            // nobody thought to watch for.
            if out.settings.len() >= MAX_SETTINGS || !is_cvar_name(head) {
                continue;
            }
            let Some(value) = parts.next() else {
                // No argument is a command, not an assignment: `+mlook`,
                // `stopsound`, `clear`.
                continue;
            };
            if parts.next().is_some() {
                // More than one argument is a sub-command, not an assignment —
                // `mirv_fov handleZoom enabled 1` appears in real movie configs
                // and sets no FOV.
                continue;
            }
            out.settings.push(CvarSetting {
                cvar: head.to_string(),
                value: unquote(value),
                file: path.to_path_buf(),
                line: index + 1,
                auto_executed,
            });
        }
    }
}

/// Split one config line into the commands the engine would run: comments
/// stripped, `;` separating commands, and semicolons inside quotes left alone.
fn commands_in(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                current.push(c);
            }
            '/' if !in_quotes && chars.peek() == Some(&'/') => break,
            ';' if !in_quotes => {
                out.push(std::mem::take(&mut current));
            }
            _ => current.push(c),
        }
    }
    out.push(current);

    out.into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Whether a token reads as a cvar name rather than a console verb.
///
/// `+mlook` and `-attack` are commands with a sign prefix, never assignments.
fn is_cvar_name(token: &str) -> bool {
    let mut chars = token.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub(crate) fn unquote(token: &str) -> String {
    token.trim().trim_matches('"').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dod_cfg_scan_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_exec_chain_is_followed_and_the_last_value_wins() {
        // The real shape: config.cfg ends with `exec movie.cfg`, and movie.cfg
        // is where the interesting values are.
        let dir = scratch("chain");
        std::fs::write(dir.join("config.cfg"), "r_decals \"300\"\nexec movie.cfg\n").unwrap();
        std::fs::write(dir.join("movie.cfg"), "r_decals\t\"0\"\nmirv_fov \"105\"\n").unwrap();

        let scan = scan(&dir);
        assert_eq!(scan.effective("r_decals").unwrap().value, "0");
        assert_eq!(scan.effective("r_decals").unwrap().file_name(), "movie.cfg");
        assert_eq!(scan.effective("mirv_fov").unwrap().value, "105");
    }

    #[test]
    fn a_bound_key_is_not_a_setting() {
        // `bind "F7" "r_decals 4000"` changes nothing until F7 is pressed.
        // Counting it would warn about a value the engine never applied.
        let dir = scratch("bind");
        std::fs::write(
            dir.join("config.cfg"),
            "bind \"F6\" \"r_decals 0; hud_deathnotice_time 5\"\nbind \"F7\" \"r_decals 4000\"\nalias clean \"r_decals 1\"\n",
        )
        .unwrap();

        let scan = scan(&dir);
        assert!(scan.effective("r_decals").is_none(), "{:?}", scan.settings);
    }

    #[test]
    fn a_comment_is_not_a_setting_and_a_semicolon_separates_commands() {
        let dir = scratch("comment");
        std::fs::write(
            dir.join("config.cfg"),
            "// r_decals 999\nmirv_fov 90; r_decals 128\n",
        )
        .unwrap();

        let scan = scan(&dir);
        assert_eq!(scan.effective("mirv_fov").unwrap().value, "90");
        assert_eq!(scan.effective("r_decals").unwrap().value, "128");
    }

    #[test]
    fn a_subcommand_is_not_an_assignment() {
        // `mirv_fov handleZoom enabled 1` appears in real movie configs.
        let dir = scratch("subcommand");
        std::fs::write(dir.join("config.cfg"), "mirv_fov handleZoom enabled \"1\"\n").unwrap();

        assert!(scan(&dir).effective("mirv_fov").is_none());
    }

    #[test]
    fn a_config_nothing_execs_sets_nothing_but_is_still_reported() {
        // A movie.cfg no one runs is not a warning about the current capture —
        // but it is one `exec` away from being one.
        let dir = scratch("unreferenced");
        std::fs::write(dir.join("config.cfg"), "cl_showfps 1\n").unwrap();
        std::fs::write(dir.join("movie.cfg"), "mirv_fov \"105\"\n").unwrap();

        let scan = scan(&dir);
        assert!(scan.effective("mirv_fov").is_none());
        assert_eq!(scan.unreferenced.len(), 1);
        assert_eq!(scan.unreferenced[0].file_name().unwrap(), "movie.cfg");
    }

    #[test]
    fn an_init_command_that_overrides_a_config_value_is_reported() {
        // The case that matters: someone sets mirv_fov in Init Commands with a
        // different value sitting in movie.cfg. The init command wins, which is
        // the point — but they should be told, not left to notice.
        let dir = scratch("override");
        std::fs::write(dir.join("config.cfg"), "exec movie.cfg\n").unwrap();
        std::fs::write(dir.join("movie.cfg"), "mirv_fov \"105\"\nr_decals \"0\"\n").unwrap();

        let scan = scan(&dir);
        let hits = scan.overrides_in(&[
            "mirv_fov 90".to_string(),
            "sys_autodir".to_string(),
            "cl_showfps 1".to_string(),
        ]);

        assert_eq!(hits.len(), 1, "{:?}", hits);
        assert_eq!(hits[0].cvar, "mirv_fov");
        assert_eq!(hits[0].init_value, "90");
        assert_eq!(hits[0].cfg_value, "105");
        assert_eq!(hits[0].file_name(), "movie.cfg");
        assert_eq!(hits[0].line, 1);
    }

    #[test]
    fn a_command_beaten_by_a_later_one_in_the_same_list_is_reported() {
        // The real shape: the user types `mirv_movie_fps 500`, and the pipeline
        // appends the Capture FPS setting after it. Both lines are on screen and
        // only the second one happens.
        let hits = self_overrides(&[
            "mirv_fov 105".to_string(),
            "mirv_movie_fps 500".to_string(),
            "sys_autodir".to_string(),
            "mirv_movie_fps 120".to_string(),
        ]);

        assert_eq!(hits.len(), 1, "{:?}", hits);
        assert_eq!(hits[0].cvar, "mirv_movie_fps");
        assert_eq!(hits[0].shadowed_value, "500");
        assert_eq!(hits[0].winner_value, "120");
        assert_eq!(hits[0].winner_index, 3, "so a caller can tell who appended it");
    }

    #[test]
    fn a_scheduled_r_decals_is_flagged_however_it_is_written() {
        // Setting the ring mid-demo strands every decal above the new limit and
        // breaks the flush, while the capture still completes and still looks
        // plausible — so this is the one that has to be caught by name.
        let hits = mid_demo_hazards(&[
            "sensitivity 3".to_string(),
            "R_Decals 128".to_string(),
        ]);

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "r_decals");
        assert_eq!(hits[0].1, "R_Decals 128");
    }

    #[test]
    fn a_scheduled_mirv_fov_is_flagged() {
        // The decal flush sizes its whole sweep's on-screen test against
        // capture_fov_resolved, read once as a pre-pass before the demo
        // plays — a Scheduled Command changing it mid-clip doesn't
        // retroactively resize anything the flush already decided.
        let hits = mid_demo_hazards(&[
            "sensitivity 3".to_string(),
            "mirv_fov 105".to_string(),
        ]);

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "mirv_fov");
        assert_eq!(hits[0].1, "mirv_fov 105");
    }

    #[test]
    fn a_scheduled_gl_widescreenfov_is_flagged() {
        // Widens the effective on-screen FOV the same way mirv_fov/default_fov
        // do, but capture_fov_resolved never reads it — a mid-demo toggle
        // invalidates the flush's sizing with nothing that could have caught it.
        let hits = mid_demo_hazards(&["gl_widescreenfov 1".to_string()]);

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "gl_widescreenfov");
        assert_eq!(hits[0].1, "gl_widescreenfov 1");
    }

    #[test]
    fn every_pipeline_owned_recording_mechanic_is_flagged() {
        // mirv_movie_filename races the block-routing aliases and can
        // misroute frames to the wrong take folder; mirv_recordmovie_start/
        // stop race the pipeline's own record-bounds scheduling;
        // mirv_movie_fps is pinned once at load and everything downstream
        // assumes it never changes; mirv_movie_ffmpeg configures the
        // direct-to-video pipe before anything records into it;
        // host_framerate races sys_fast_forward/sys_normal_speed's own
        // timing. All of them dangerous scheduled mid-demo — whether typing
        // them anywhere at all is banned outright is `banned_commands`'
        // narrower list, tested separately below. mirv_movie_separate_hud is
        // deliberately absent — see `MID_DEMO_HAZARDS`'s own doc comment.
        let hits = mid_demo_hazards(&[
            "mirv_movie_filename foo".to_string(),
            "mirv_recordmovie_start".to_string(),
            "mirv_recordmovie_stop".to_string(),
            "mirv_movie_fps 500".to_string(),
            "mirv_movie_ffmpeg all enabled 1".to_string(),
            "host_framerate 0.05".to_string(),
        ]);

        let flagged: Vec<&str> = hits.iter().map(|(cvar, _)| cvar.as_str()).collect();
        assert_eq!(
            flagged,
            vec![
                "mirv_movie_filename",
                "mirv_recordmovie_start",
                "mirv_recordmovie_stop",
                "mirv_movie_fps",
                "mirv_movie_ffmpeg",
                "host_framerate",
            ]
        );
    }

    #[test]
    fn mirv_movie_separate_hud_is_untracked_everywhere() {
        // No setting exists behind it any more (removed 2026-09-08), and
        // nothing in this codebase has verified what a mid-demo toggle does
        // — unlike r_decals/mirv_fov, which are measured. Rather than assert
        // a mechanism nobody has checked, it gets no special treatment at
        // all: not banned, not a mid-demo hazard, not shadowed.
        let commands = vec!["mirv_movie_separate_hud 1".to_string()];
        assert!(banned_commands(&commands).is_empty());
        assert!(mid_demo_hazards(&commands).is_empty());
        assert!(scheduled_banned_commands(&commands).is_empty());
    }

    #[test]
    fn banned_commands_covers_exactly_the_tier_1_set() {
        // No dedicated setting corresponds to any of these, and no scenario
        // has been found where typing one is anything but a misunderstanding
        // — banned outright, unlike mirv_movie_fps (redundant with a real
        // setting, so shadowed-with-a-warning instead) or r_decals/mirv_fov
        // (the user's own stated value wins). Does NOT
        // include mirv_movie_filename any more — see
        // `scheduled_banned_commands_flags_the_decal_flush_cvars_and_mirv_movie_filename`.
        // r_drawentities/cl_lw are here for a different reason (no legitimate
        // non-default value, not "no dedicated setting"), but the ban itself
        // is identical, so one list-membership test covers both reasons.
        let hits = banned_commands(&[
            "mirv_recordmovie_start".to_string(),
            "mirv_recordmovie_stop".to_string(),
            "mirv_movie_ffmpeg all enabled 1".to_string(),
            "host_framerate 0.05".to_string(),
            "r_drawentities 0".to_string(),
            "cl_lw 0".to_string(),
        ]);

        let flagged: Vec<&str> = hits.iter().map(|(cvar, _)| cvar.as_str()).collect();
        assert_eq!(
            flagged,
            vec![
                "mirv_recordmovie_start",
                "mirv_recordmovie_stop",
                "mirv_movie_ffmpeg",
                "host_framerate",
                "r_drawentities",
                "cl_lw",
            ]
        );
    }

    #[test]
    fn fatal_cvar_hazards_flags_only_a_non_default_value() {
        let dir = scratch("fatal");
        std::fs::write(
            dir.join("config.cfg"),
            "sv_cheats \"1\"\nr_drawentities \"0\"\ncl_lw \"1\"\nsensitivity \"3\"\n",
        )
        .unwrap();

        let scan = scan(&dir);
        let hazards = fatal_cvar_hazards(&scan);

        // cl_lw is explicitly 1 (the required value) and sensitivity is not a
        // FATAL_CVARS entry at all -- neither should be reported. sv_cheats is
        // set only so r_drawentities is reachable at all; see
        // r_drawentities_alone_is_not_fatal_because_the_engine_clamps_it.
        assert_eq!(hazards.len(), 1, "{hazards:?}");
        assert_eq!(hazards[0].cvar, "r_drawentities");
        assert_eq!(hazards[0].value, "0");
        assert_eq!(hazards[0].required, "1");
        assert_eq!(hazards[0].file_name(), "config.cfg");
    }

    #[test]
    fn fatal_cvar_hazards_resolves_last_value_wins_like_everything_else() {
        // A config that sets it wrong and then corrects it is not a hazard --
        // that is genuinely the value the engine ends up running with.
        let dir = scratch("fatal_corrected");
        std::fs::write(dir.join("config.cfg"), "r_drawentities \"0\"\nr_drawentities \"1\"\n").unwrap();

        let scan = scan(&dir);
        assert!(fatal_cvar_hazards(&scan).is_empty());
    }

    #[test]
    fn fatal_cvar_hazards_ignores_an_unreferenced_config() {
        // A movie.cfg sitting in the folder that nothing execs sets nothing --
        // same rule `CfgScan::effective` already applies to everything else.
        let dir = scratch("fatal_unreferenced");
        std::fs::write(dir.join("movie.cfg"), "r_drawentities \"0\"\n").unwrap();

        let scan = scan(&dir);
        assert!(fatal_cvar_hazards(&scan).is_empty());
    }

    #[test]
    fn fatal_cvars_are_in_banned_commands_too() {
        // The config-file hazard above and the typed-command ban are two
        // halves of the same fact; letting them drift apart would leave one
        // route to the crash unblocked while the other still warns about it.
        for entry in FATAL_CVARS {
            assert!(
                BANNED_COMMANDS.contains(&entry.cvar),
                "{} missing from BANNED_COMMANDS",
                entry.cvar
            );
        }
    }

    #[test]
    fn r_drawentities_alone_is_not_fatal_because_the_engine_clamps_it() {
        // GoldSrc resets r_drawentities to 1.0 itself while sv_cheats is 0
        // (hw.dll 0x1d455c9), so the value never reaches DoD's client and the
        // config line is inert. Reporting it would block a capture over
        // something harmless -- confirmed live, the game does not quit.
        let dir = scratch("clamped");
        std::fs::write(dir.join("config.cfg"), "r_drawentities \"0\"\n").unwrap();

        assert!(fatal_cvar_hazards(&scan(&dir)).is_empty());
    }

    #[test]
    fn r_drawentities_is_fatal_once_a_config_also_enables_cheats() {
        // A non-zero sv_cheats skips the engine's clamp entirely, so the value
        // sticks and DoD's own CHud::Redraw check quits the game.
        let dir = scratch("clamped_cheats");
        std::fs::write(
            dir.join("config.cfg"),
            "sv_cheats \"1\"\nr_drawentities \"0\"\n",
        )
        .unwrap();

        let hazards = fatal_cvar_hazards(&scan(&dir));
        assert_eq!(hazards.len(), 1, "{hazards:?}");
        assert_eq!(hazards[0].cvar, "r_drawentities");
    }

    #[test]
    fn cl_lw_is_fatal_with_or_without_cheats() {
        // No engine clamp on this one -- it takes whatever value it is given,
        // which is why it is the half that reproduced live.
        for (tag, extra) in [("cllw_plain", ""), ("cllw_cheats", "sv_cheats \"1\"\n")] {
            let dir = scratch(tag);
            std::fs::write(dir.join("config.cfg"), format!("{extra}cl_lw \"0\"\n")).unwrap();

            let hazards = fatal_cvar_hazards(&scan(&dir));
            assert_eq!(hazards.len(), 1, "extra={extra:?} -> {hazards:?}");
            assert_eq!(hazards[0].cvar, "cl_lw");
        }
    }

    #[test]
    fn banned_commands_no_longer_catches_mirv_movie_filename() {
        let hits = banned_commands(&["mirv_movie_filename foo".to_string()]);
        assert!(hits.is_empty(), "{:?}", hits);
    }

    #[test]
    fn scheduled_banned_commands_flags_the_decal_flush_cvars_and_mirv_movie_filename() {
        let hits = scheduled_banned_commands(&[
            "sensitivity 3".to_string(),
            "r_decals 512".to_string(),
            "mirv_fov 105".to_string(),
            "gl_widescreenfov 1".to_string(),
            "mirv_movie_filename foo".to_string(),
        ]);

        let flagged: Vec<&str> = hits.iter().map(|(cvar, _)| cvar.as_str()).collect();
        assert_eq!(flagged, vec!["r_decals", "mirv_fov", "gl_widescreenfov", "mirv_movie_filename"]);
    }

    #[test]
    fn scheduled_banned_commands_ignores_everything_else() {
        let hits = scheduled_banned_commands(&[
            "mirv_movie_fps 500".to_string(),
            "host_framerate 0.05".to_string(),
            "exec somefile.cfg".to_string(),
            "quit".to_string(),
        ]);

        assert!(hits.is_empty(), "{:?}", hits);
    }

    #[test]
    fn noop_commands_in_init_covers_mirv_movie_filename_and_engine_dropped_commands() {
        let hits = noop_commands_in_init(&[
            "sensitivity 3".to_string(),
            "mirv_movie_filename foo".to_string(),
            "exec somefile.cfg".to_string(),
            "quit".to_string(),
        ]);

        let flagged: Vec<&str> = hits.iter().map(|(cvar, _)| cvar.as_str()).collect();
        assert_eq!(flagged, vec!["mirv_movie_filename", "exec", "quit"]);
    }

    #[test]
    fn noop_commands_in_scheduled_covers_only_the_engine_dropped_commands() {
        // mirv_movie_filename is dangerous, not merely inert, once scheduled
        // — refused via scheduled_banned_commands, not reported here.
        let hits = noop_commands_in_scheduled(&[
            "mirv_movie_filename foo".to_string(),
            "exec somefile.cfg".to_string(),
            "quit".to_string(),
        ]);

        let flagged: Vec<&str> = hits.iter().map(|(cvar, _)| cvar.as_str()).collect();
        assert_eq!(flagged, vec!["exec", "quit"]);
    }

    #[test]
    fn banned_commands_does_not_catch_tier_2_or_tier_3_cvars() {
        // mirv_movie_fps is redundant-with-a-setting (shadowed, not banned);
        // mirv_movie_separate_hud is untracked entirely (see
        // `mirv_movie_separate_hud_is_untracked_everywhere`); r_decals/
        // mirv_fov/gl_widescreenfov are either respected (Tier 3) or only a
        // Scheduled-Commands hazard, not an everywhere-ban.
        let hits = banned_commands(&[
            "mirv_movie_fps 500".to_string(),
            "mirv_movie_separate_hud 1".to_string(),
            "r_decals 256".to_string(),
            "mirv_fov 90".to_string(),
            "gl_widescreenfov 1".to_string(),
        ]);

        assert!(hits.is_empty(), "{:?}", hits);
    }

    #[test]
    fn the_last_assignment_in_a_list_is_the_one_that_holds() {
        let commands = vec![
            "mirv_movie_fps 500".to_string(),
            "sys_autodir".to_string(),
            "mirv_movie_fps 120".to_string(),
        ];

        assert_eq!(effective_in(&commands, "mirv_movie_fps").as_deref(), Some("120"));
        assert_eq!(effective_in(&commands, "mirv_fov"), None);
        assert_eq!(
            assigned_cvar("mirv_movie_fps 500"),
            Some(("mirv_movie_fps".to_string(), "500".to_string()))
        );
        assert_eq!(assigned_cvar("sys_autodir"), None);
    }

    #[test]
    fn repeating_the_same_value_shadows_nothing_worth_saying() {
        assert!(self_overrides(&[
            "mirv_movie_fps 120".to_string(),
            "mirv_movie_fps 120".to_string(),
        ])
        .is_empty());
    }

    #[test]
    fn setting_a_cvar_to_what_the_config_already_says_is_not_an_override() {
        let dir = scratch("agrees");
        std::fs::write(dir.join("config.cfg"), "mirv_fov \"105\"\n").unwrap();

        assert!(scan(&dir).overrides_in(&["mirv_fov 105".to_string()]).is_empty());
    }

    #[test]
    fn every_assignment_is_recorded_not_just_the_ones_the_pipeline_reads() {
        // Most configs never mention r_decals. The collisions that surprise
        // people are the ones nobody thought to watch for.
        let dir = scratch("all_cvars");
        std::fs::write(
            dir.join("config.cfg"),
            "volume \"0.5\"\nzoom_sensitivity_ratio \"1.2\"\n+mlook\nstopsound\n",
        )
        .unwrap();

        let scan = scan(&dir);
        assert_eq!(scan.effective("volume").unwrap().value, "0.5");
        assert!(scan.effective("+mlook").is_none(), "a verb is not an assignment");
        assert!(scan.effective("stopsound").is_none(), "nor is a bare command");
        assert_eq!(
            scan.overrides_in(&["volume 1".to_string()]).len(),
            1,
            "and a collision on any of them is worth reporting"
        );
    }

    #[test]
    fn a_config_that_execs_itself_terminates() {
        let dir = scratch("cycle");
        std::fs::write(dir.join("config.cfg"), "exec loop.cfg\n").unwrap();
        std::fs::write(dir.join("loop.cfg"), "exec config.cfg\nr_decals 8\n").unwrap();

        assert_eq!(scan(&dir).effective("r_decals").unwrap().value, "8");
    }

    #[test]
    fn an_exec_cannot_climb_out_of_the_mod_folder() {
        let dir = scratch("escape");
        std::fs::write(dir.join("config.cfg"), "exec ../../../windows/win.ini\n").unwrap();

        // Nothing to assert about the outcome beyond it not being read: the
        // point is that the traversal is refused rather than attempted.
        assert_eq!(scan(&dir).files_read, 1);
    }
}
