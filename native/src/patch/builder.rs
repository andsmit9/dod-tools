// patch/builder.rs
// Batch job construction and the legacy channel-based worker spawner.
// Calls std::fs::create_dir_all and std::thread::spawn — native-only.

use std::sync::{Arc, atomic::AtomicBool};
use crate::patch::types::{
    CaptureStreak, PatchJob, PatcherConfig, CommandRelation,
    CaptureWorker, PatchEvent,
};
use crate::patch::engine::StreamPatcher;

// ── Frame-time helpers ───────────────────────────────────────────────────────

/// Walk backwards from `start_frame` (0-indexed) through `frame_times` until
/// `gap_seconds` of real demo time has been accumulated. Returns the 0-indexed
/// frame where that time boundary is reached. Clamps to frame 0 if the gap
/// exceeds the available history before the start frame.
fn find_tick_backwards(start_frame: usize, gap_seconds: f32, frame_times: &[f32], fps: f32) -> i32 {
    let fps = if fps > 0.0 { fps } else { 100.0 };
    if gap_seconds <= 0.0 {
        return start_frame as i32;
    }
    // If frame_times is empty or start_frame is out-of-bounds, fall back to linear math.
    // This is safe, consistent, and spaces commands chronologically when frame_times
    // is truncated or absent.
    if frame_times.is_empty() || start_frame >= frame_times.len() {
        let ticks_gap = (gap_seconds * fps).round() as i32;
        return (start_frame as i32 - ticks_gap).max(0);
    }
    let anchor_time = frame_times[start_frame];
    let target_time = anchor_time - gap_seconds;
    // Walk backwards until we cross target_time
    let mut frame = start_frame;
    while frame > 0 {
        frame -= 1;
        if frame_times[frame] <= target_time {
            return frame as i32;
        }
    }
    0
}

/// Walk forwards from `start_frame` (0-indexed) through `frame_times` until
/// `gap_seconds` of real demo time has accumulated. Returns the 0-indexed
/// frame where that time boundary is reached. Clamps to the last valid frame
/// if the end of the array is reached before the gap is satisfied.
fn find_tick_forwards(start_frame: usize, gap_seconds: f32, frame_times: &[f32], fps: f32) -> i32 {
    let fps = if fps > 0.0 { fps } else { 100.0 };
    if gap_seconds <= 0.0 {
        return start_frame as i32;
    }
    if frame_times.is_empty() || start_frame >= frame_times.len() {
        let ticks_gap = (gap_seconds * fps).round() as i32;
        return start_frame as i32 + ticks_gap;
    }
    let anchor_time = frame_times[start_frame];
    let target_time = anchor_time + gap_seconds;
    let last = frame_times.len().saturating_sub(1);
    let mut frame = start_frame;
    while frame < last {
        frame += 1;
        if frame_times[frame] >= target_time {
            return frame as i32;
        }
    }
    last as i32
}

const LOG_TAG: &str = "[dod-tools]";

/// How far ahead of the record start `stopsound` fires, to flush audio the
/// fast-forward left in a bad state. Clamped down to the pre-roll when the
/// pre-roll is shorter, so it never lands while still fast-forwarding.
const SOUND_FLUSH_LEAD_SECONDS: f32 = 1.0;

/// Minimum breathing room between one take's `mirv_recordmovie_stop` and the
/// next one's start. Two highlights closer than this are merged into a single
/// take rather than risking a stop/start cycle that tight.
///
/// This is a deliberately conservative guard, not a measured threshold — how
/// long HLAE needs to finalise a take (flush the BMP sequence, write the WAV)
/// before accepting a new one isn't something the demo side can observe. The
/// failure it guards against is a take landing without its audio, which is
/// exactly the "captured but not renderable" case take verification reports.
/// Merging instead costs a second of connective footage inside one clip, which
/// is a far cheaper outcome than a silent take.
pub const MIN_TAKE_SEPARATION_SECONDS: f32 = 1.0;

/// The separation this batch's capture mode actually needs.
///
/// `MIN_TAKE_SEPARATION_SECONDS` above is HLAE's, and was a deliberately
/// conservative guess because the demo side cannot observe how long HLAE takes
/// to finalise a take. The OBS figure is not a guess — it is measured, and it
/// is larger — so the mode has to choose.
///
/// Public so `find_overlaps` and the tests ask the same question the merge loop
/// does, rather than re-deriving it and drifting.
pub fn take_separation_seconds(config: &PatcherConfig) -> f32 {
    match config.capture_mode {
        crate::patch::CaptureMode::Obs => crate::patch::types::OBS_TAKE_SEPARATION_SECONDS,
        _ => MIN_TAKE_SEPARATION_SECONDS,
    }
}

fn build_safe_echos(tick: i32, message: &str) -> Vec<(i32, String)> {
    let mut result = Vec::new();
    let mut current_tick = tick;
    
    let mut words: Vec<&str> = message.split(' ').collect();
    if words.is_empty() {
        return result;
    }
    
    let mut current_chunk = String::new();
    let mut is_first = true;
    
    let mut i = 0;
    while i < words.len() {
        let word = words[i];
        let prefix = if is_first {
            format!("{} ", LOG_TAG)
        } else {
            "[dodtools] ->".to_string()
        };
        
        let test_message = if current_chunk.is_empty() {
            word.to_string()
        } else {
            format!("{} {}", current_chunk, word)
        };
        
        let full_string = format!("{}{}", prefix, test_message);
        
        if full_string.len() > crate::patch::MAX_ECHO_CHUNK_SIZE {
            if current_chunk.is_empty() {
                let limit = crate::patch::MAX_ECHO_CHUNK_SIZE.saturating_sub(prefix.len());
                let (part1, part2) = word.split_at(limit.min(word.len()));
                
                let cmd = format!("echo \"{}{}\"", prefix, part1);
                result.push((current_tick, cmd));
                current_tick += 1;
                
                is_first = false;
                words[i] = part2;
                continue;
            } else {
                let cmd = format!("echo \"{}{}\"", prefix, current_chunk);
                result.push((current_tick, cmd));
                current_tick += 1;
                
                current_chunk.clear();
                is_first = false;
                continue;
            }
        } else {
            current_chunk = test_message;
            i += 1;
        }
    }
    
    if !current_chunk.is_empty() {
        let prefix = if is_first {
            format!("{} ", LOG_TAG)
        } else {
            "[dodtools] ->".to_string()
        };
        let cmd = format!("echo \"{}{}\"", prefix, current_chunk);
        result.push((current_tick, cmd));
    }
    
    result
}


// ── Block merging ─────────────────────────────────────────────────────────────

/// Whether two windows around `prev_end` and `next_start` collide once the
/// given padding is applied to each side.
///
/// Called twice per highlight pair, with different padding, to answer two
/// different questions (see `build_batch_queue`'s merge loop):
///
/// - with start-lead/stop-trail: do the **recordings** overlap? If so the two
///   highlights physically cannot be separate takes and must be merged.
/// - with start-lead+pre-roll / stop-trail+post-roll: do the **speed-change**
///   windows collide? If so the clips stay separate takes, but the
///   fast-forward between them is dropped and playback just stays at normal
///   speed across the gap.
///
/// Public so the `find_overlaps` diagnostic can ask the same questions without
/// running a capture, and can't drift from the real decision.
/// Every command the engine will run at demo load: the user's own init
/// commands, then the ones the pipeline adds for itself.
///
/// Extracted so it can be asked ahead of a capture as well as during one. The
/// app's own additions override whatever the game's configs set — `capture_fps`
/// beats a `mirv_movie_fps` in `movie.cfg`, and the decal pin beats an
/// `r_decals` there — and a user is entitled to know that before it happens
/// rather than by noticing the result.
pub fn final_init_commands(config: &PatcherConfig) -> Vec<String> {
    let mut out = config.init_commands.clone();
    out.push("sys_autodir".to_string());
    out.push(format!("mirv_movie_fps {}", config.capture_fps));

    // OBS mode is real time: HLAE issues no `mirv_movie_start` at all, so
    // `mirv_movie_fps` above is inert on this path — nothing reads it. What
    // OBS actually records is however fast the engine renders, so that rate
    // has to be pinned to the same `obs_capture_fps` OBS's own canvas is set
    // to (obs::provision::ensure_dod_tools_setup), or the two drift against
    // each other. `fps_override 1` first: GoldSrc's default `fps_max`
    // ceiling (~100) is below what obs_capture_fps is commonly set to, and
    // `fps_max` alone is silently clamped under that ceiling without it.
    if config.capture_mode == crate::patch::CaptureMode::Obs {
        out.push("fps_override 1".to_string());
        out.push(format!("fps_max {}", config.obs_capture_fps));
    }

    // The decal flush needs the ring set once, at demo load, and never again.
    // r_decals bounds how far the rotating index may travel before it wraps; it
    // does not evict anything, so lowering it once decals have accumulated
    // strands every one sitting above the new limit.
    //
    // The sweep is sized to that same number, so there is only one number here
    // and `r_decals` is where the engine reads it, at the same precedence
    // `ring_limit` itself resolves (see that function): Initial Commands, then
    // an executed config, then the app's own default. When either of the first
    // two states it, that line is already the pin — appending a second one
    // here would either silently overrule what Initial Commands asked for, or
    // be a no-op duplicate of what the config already set. Only the true
    // "nothing anywhere states it" case needs one appended, to replace the
    // engine's own uncontrolled standing value with a known one.
    //
    // Not at the maximum, though. r_decals is clamped to MAX_RENDER_DECALS, so a
    // sweep that size turns a full revolution whatever the cvar happens to be —
    // any smaller ring simply gets swept several times over. Pinning then buys
    // nothing and costs the precondition the rest of this design works around:
    // that nothing else may touch r_decals.
    if config.decal_flush
        && crate::patch::ring_limit_from_init(&config.init_commands).is_none()
        && crate::patch::ring_limit_from_game_config(config).is_none()
    {
        let ring = config.decal_ring_limit.min(crate::patch::MAX_RENDER_DECALS);
        if ring > 0 && ring < crate::patch::MAX_RENDER_DECALS {
            out.push(format!("r_decals {}", ring));
        }
    }

    out
}

/// The shortest real-time run-up the engine will tolerate before recording.
///
/// `docs/goldsrc_dod_quirks.md`: fast-forwarding breaks the engine's audio
/// buffers, and the speed must return to real time "2 to 4 seconds prior to
/// injecting `mirv_recordmovie_start`" to flush and resync. This is the lower
/// end of that range — the floor, not the recommendation.
pub const AUDIO_RESYNC_SECONDS: f32 = 2.0;

/// What the pre-roll and post-roll have to cover, and which requirement is
/// currently setting the bar.
///
/// The rolls stopped being a matter of taste once other things started being
/// measured against them: the audio resync, the sound flush, the decal sweep's
/// lead, and any Scheduled Command's offset. Each is knowable, so the app can
/// say when a roll is too short instead of leaving it to be discovered in a
/// capture that looks almost right.
#[derive(Debug, Clone, PartialEq)]
pub struct RollFloors {
    pub pre_roll: f32,
    /// Which term set `pre_roll`.
    pub pre_roll_binding: &'static str,
    pub post_roll: f32,
    pub post_roll_binding: &'static str,
    pub audio_resync: f32,
    pub sound_flush: f32,
    /// The decal sweep's lead, or 0 when the flush is off.
    pub flush_lead: f32,
    /// Largest "Before" offset among the Scheduled Commands.
    pub scheduled_before: f32,
    /// Largest "After" offset among them.
    pub scheduled_after: f32,
}

/// Compute the floors for a configuration.
///
/// Deliberately does NOT fold in the burst's own span. At a 4,096 ring that
/// spans several seconds and would demand an enormous pre-roll — but the burst
/// is network messages in the demo stream rather than console commands, so
/// whether it needs real-time playback at all is unverified. Guessing a floor
/// from an unknown would be worse than leaving it out and saying so.
pub fn roll_floors(config: &PatcherConfig) -> RollFloors {
    let flush_lead = if config.decal_flush {
        crate::patch::DEFAULT_LEAD_SECONDS
    } else {
        0.0
    };
    let offset_for = |want_after: bool| {
        config
            .custom_commands
            .iter()
            .filter(|c| matches!(c.relation, CommandRelation::After) == want_after)
            .map(|c| c.offset)
            .fold(0.0f32, f32::max)
    };
    // Scheduled offsets anchor to the KILL, not to the record start — while the
    // speed drop sits a pre-roll before the record start, which is itself a
    // start-lead before the kill. So the real-time window opens
    // `record_start_lead + pre_roll` ahead of the kill, and a Before offset only
    // needs the pre-roll to cover what the start lead does not.
    //
    // Comparing the raw offset against the pre-roll alone is what made this warn
    // about a 10s command with a 5s lead and a 5s pre-roll, which lands exactly
    // on the speed drop and is fine.
    let scheduled_before = (offset_for(false) - config.record_start_lead).max(0.0);
    let scheduled_after = (offset_for(true) - config.record_stop_trail).max(0.0);

    // Highest wins, and the label names it so the message can be acted on.
    let pre_terms = [
        (AUDIO_RESYNC_SECONDS, "the audio resync after fast-forward"),
        (SOUND_FLUSH_LEAD_SECONDS, "the stopsound flush"),
        (flush_lead, "the decal flush's lead"),
        (scheduled_before, "a Scheduled Command set before the highlight"),
    ];
    let (pre_roll, pre_roll_binding) = pre_terms
        .iter()
        .copied()
        .fold((0.0f32, "nothing"), |acc, t| if t.0 > acc.0 { t } else { acc });

    let (post_roll, post_roll_binding) = if scheduled_after > 0.0 {
        (scheduled_after, "a Scheduled Command set after the highlight")
    } else {
        (0.0, "nothing")
    };

    RollFloors {
        pre_roll,
        pre_roll_binding,
        post_roll,
        post_roll_binding,
        audio_resync: AUDIO_RESYNC_SECONDS,
        sound_flush: SOUND_FLUSH_LEAD_SECONDS,
        flush_lead,
        scheduled_before,
        scheduled_after,
    }
}

/// Whether a scheduled command lands while playback is still fast-forwarding.
///
/// A block runs at `host_framerate 0.05` until the pre-roll drops it back to
/// real time at `speed_drop_tick`, and resumes fast-forwarding once the
/// post-roll ends at `post_roll_end_tick`. Outside that window a command still
/// executes — it just executes with the engine racing through frames and its
/// audio buffers unflushed.
///
/// Whether that matters depends entirely on the command, so this reports rather
/// than warns. Setting a cvar early is usually harmless: `hud_deathnotice_time`
/// ten seconds before a clip simply takes effect ten seconds before the clip.
/// What misbehaves is anything that depends on playback running at real speed —
/// sound, recording start/stop, rendering. Phrasing this as a hazard would cry
/// wolf on the common, correct case, which is how a warning gets ignored.
pub fn runs_during_fast_forward(
    target_tick: i32,
    speed_drop_tick: i32,
    post_roll_end_tick: i32,
) -> bool {
    target_tick < speed_drop_tick || target_tick > post_roll_end_tick
}

pub fn blocks_merge(prev_end: i32, next_start: i32, lead_ticks: i32, trail_ticks: i32) -> bool {
    (next_start - lead_ticks).max(0) <= prev_end + trail_ticks
}

/// Frame index of a highlight's first recorded kill, honouring a Kill Range
/// edit. Falls back to the streak's tick bound when kill data is absent.
///
/// The merge decision and the scheduled record marks must both key off this
/// rather than `start_tick`, or a Kill Range edit moves the recording without
/// moving the decision about whether it collides with its neighbour.
fn first_kill_frame(streak: &CaptureStreak) -> i32 {
    streak.kills.get(streak.start_index).map(|k| k.0).unwrap_or(streak.start_tick)
}

/// Frame index of a highlight's last recorded kill, honouring a Kill Range
/// edit. For a merged block this is the absorbed highlight's final kill.
fn last_kill_frame(streak: &CaptureStreak) -> i32 {
    let idx = streak.end_index.min(streak.kills.len().saturating_sub(1));
    streak.kills.get(idx).map(|k| k.0).unwrap_or(streak.end_tick)
}

// ── Drive allocation ──────────────────────────────────────────────────────────

/// First-Fit-Decreasing: assigns each block (identified by its index into
/// `block_estimates`) to the earliest drive — starting from `*active_drive_idx`
/// and wrapping around — with at least `estimate + threshold` bytes free,
/// largest blocks first. Placing the biggest clips while every drive still has
/// maximum headroom means a later, smaller clip can backfill whatever's left,
/// instead of naive arrival-order first-fit where an earlier small clip can
/// strand a later large one on a drive that would otherwise have fit it.
///
/// Mutates `drive_free` in place (bytes consumed per drive) and advances
/// `*active_drive_idx` to the last drive used, biasing the next call (e.g. the
/// next demo's blocks) to keep filling it. Returns `(block_index, drive_index)`
/// pairs in allocation order, or `Err(block_index)` for the first block that
/// couldn't fit anywhere.
fn allocate_blocks_first_fit_decreasing(
    block_estimates: &[u64],
    drive_free: &mut [u64],
    active_drive_idx: &mut usize,
    threshold: u64,
) -> Result<Vec<(usize, usize)>, usize> {
    let num_drives = drive_free.len();

    let mut allocation_order: Vec<usize> = (0..block_estimates.len()).collect();
    allocation_order.sort_by_key(|&i| std::cmp::Reverse(block_estimates[i]));

    let mut result = Vec::with_capacity(block_estimates.len());

    for block_index in allocation_order {
        let clip_byte_estimate = block_estimates[block_index];

        let mut allocated = false;
        let mut drives_checked = 0;
        let mut current_drive_idx = *active_drive_idx;
        loop {
            if drives_checked >= num_drives {
                break;
            }

            if drive_free[current_drive_idx] >= clip_byte_estimate + threshold {
                drive_free[current_drive_idx] -= clip_byte_estimate;
                result.push((block_index, current_drive_idx));
                *active_drive_idx = current_drive_idx;
                allocated = true;
                break;
            } else {
                current_drive_idx = (current_drive_idx + 1) % num_drives;
                drives_checked += 1;
            }
        }

        if !allocated {
            return Err(block_index);
        }
    }

    Ok(result)
}

// ── Batch queue builder ───────────────────────────────────────────────────────

pub fn build_batch_queue(raw_streaks: Vec<CaptureStreak>, config: &PatcherConfig, global_arrays: &std::collections::HashMap<std::path::PathBuf, std::sync::Arc<Vec<f32>>>) -> Result<(Vec<PatchJob>, Vec<crate::patch::types::DriveHeadroom>), std::io::Error> {
    // tickrate is extracted dynamically from streaks per-demo.
    // Each streak is carried alongside its index in `raw_streaks` so the blocks
    // built below can point back at the exact highlights the caller dispatched,
    // even after the overlap merge collapses several into one recording.
    let mut grouped: std::collections::HashMap<(&str, Option<&str>), Vec<(usize, &CaptureStreak)>> = std::collections::HashMap::new();
    for (idx, streak) in raw_streaks.iter().enumerate() {
        grouped.entry((streak.source_demo.as_str(), streak.target_player.as_deref())).or_default().push((idx, streak));
    }

    // The rolls are load-bearing now — the audio resync, the sound flush, the
    // decal sweep's lead and any Scheduled Command's offset all measure against
    // them — so say when one is too short rather than leaving it to be found in
    // a capture that looks almost right.
    let floors = roll_floors(config);
    if config.pre_roll_seconds < floors.pre_roll {
        crate::log_markdown(&format!(
            "⚠️ **Pre-roll is shorter than this capture needs** — {:.1}s, against a {:.1}s floor \
             set by {}. Playback returns to real time {:.1}s before recording, which is not enough \
             for it: the engine's audio buffers are left unflushed by the fast-forward. Raise the \
             pre-roll to at least {:.1}s.",
            config.pre_roll_seconds,
            floors.pre_roll,
            floors.pre_roll_binding,
            config.pre_roll_seconds,
            floors.pre_roll
        ));
    }
    if config.post_roll_seconds < floors.post_roll {
        crate::log_markdown(&format!(
            "⚠️ **Post-roll is shorter than this capture needs** — {:.1}s, against a {:.1}s floor \
             set by {}. Anything past the post-roll fires while playback is fast-forwarding again.",
            config.post_roll_seconds, floors.post_roll, floors.post_roll_binding
        ));
    }

    // Sort grouped chronologically by the start_tick of their first streak
    let mut sorted_groups: Vec<_> = grouped.into_iter().collect();
    sorted_groups.sort_by_key(|(_, streaks)| streaks.iter().map(|(_, s)| s.start_tick).min().unwrap_or(0));

    let mut jobs = Vec::new();
    let total_jobs = sorted_groups.len();
    
    let date_time = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let mut helper_cfg_content = String::new();

    let game_path_buf = std::path::PathBuf::from(&config.game_path);
    let dod_dir = match game_path_buf.parent() {
        Some(parent) => parent.join("dod"),
        None => return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Target game output directory not found"
        )),
    };

    // Remove stale config from dod_dir
    let _ = std::fs::remove_file(dod_dir.join("dodtools_helper.cfg"));
    let _ = std::fs::remove_file(dod_dir.join("dodtools_capture_done.cfg"));
    let _ = std::fs::remove_file(dod_dir.join("dod_quit.cfg"));
    if let Ok(entries) = std::fs::read_dir(&dod_dir) {
        for entry in entries.flatten() {
            let filename = entry.file_name().to_string_lossy().to_string();
            if filename.starts_with("dodtools_chain_") && filename.ends_with(".cfg") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    
    // ── AOT disk-space simulation ─────────────────────────────────────────────
    // Snapshot current free bytes for every configured capture directory so we
    // can route each clip to the drive with sufficient headroom at build time.
    const FAILOVER_THRESHOLD: u64 = crate::sys::disk::MIN_DRIVE_HEADROOM_BYTES;
    let mut drive_free: Vec<u64> = config
        .capture_directories
        .iter()
        .map(|p| crate::sys::disk::get_available_bytes(p))
        .collect();
    let mut active_drive_idx: usize = 0;
    // If no capture directories are configured, fall back to a single sentinel
    // that has "unlimited" free space so existing single-drive behaviour is
    // preserved without an error.
    if drive_free.is_empty() {
        drive_free.push(u64::MAX);
    }
    
    let active_export_dir = match config.primary_media_dir.clone() {
        Some(dir) => dir,
        None => return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Target game output directory not found"
        )),
    };
    let session_dir = if !config.session_id.is_empty() {
        active_export_dir.join(&config.session_id)
    } else {
        active_export_dir
    };
    if !session_dir.exists() {
        let _ = std::fs::create_dir_all(&session_dir);
    }
    
    helper_cfg_content.push_str(&format!(
        "# dodtools_helper.cfg\n# Created by: dod_tools.exe v{}\n# Date: {}\n\n",
        crate::VERSION,
        date_time
    ));
    // Marks exactly where this file execs relative to the engine's own
    // config.cfg/movie.cfg chain, which lands in qconsole.log too (-condebug
    // is passed on every launch) — cheap enough to leave in permanently rather than
    // re-add it every time this ordering question comes up again. See
    // docs/goldsrc_dod_quirks.md's Command Precedence entry.
    helper_cfg_content.push_str("echo dodtools_helper.cfg exec'd here\n\n");

    helper_cfg_content.push_str("# Global aliases\n");
    helper_cfg_content.push_str("alias sys_autodir \"spec_autodirector 1\"\n");
    helper_cfg_content.push_str("alias sys_normal_speed \"sys_autodir; clear; host_framerate 0\"\n");
    helper_cfg_content.push_str("alias sys_fast_forward \"host_framerate 0.05\"\n");
    helper_cfg_content.push_str("alias sys_sound \"stopsound\"\n");
    // In OBS mode HLAE records nothing: the recorder is an external process
    // driven off the console-log markers these same stages already echo, so the
    // record aliases must not issue `mirv_recordmovie_*` at all.
    //
    // This is what keeps the clip at real time. `mirv_recordmovie_start` pins
    // the engine's timestep to `1/mirv_movie_fps`; without it the engine simply
    // stays at the `host_framerate 0` that `sys_normal_speed` set at the
    // pre-roll, which is exactly what OBS needs in order to capture a clip that
    // plays at the right speed.
    //
    // `stopsound` is kept. It exists to flush the audio buffers fast-forward
    // corrupts, and that corruption happens whoever is recording. It lands
    // ~1s before the first captured frame, inside the pre-roll head that gets
    // trimmed, so it is not audible in the finished clip.
    //
    // `sys_record_stop` becomes an echo rather than an empty alias: GoldSrc
    // treats an alias with an empty body as a parse oddity, and a no-op that
    // announces itself is easier to recognise in a log than one that vanishes.
    if config.capture_mode.hlae_records() {
        helper_cfg_content.push_str("alias sys_record_start \"mirv_recordmovie_start; stopsound\"\n");
        helper_cfg_content.push_str("alias sys_record_stop \"mirv_recordmovie_stop\"\n");
    } else {
        helper_cfg_content.push_str("alias sys_record_start \"stopsound\"\n");
        helper_cfg_content.push_str("alias sys_record_stop \"echo [dod-tools] OBS_MODE_NO_HLAE_STOP\"\n");
    }
    helper_cfg_content.push_str("alias sys_capture_done_path \"mirv_movie_filename DOD_TOOLS_EXIT_TRIGGER; mirv_recordmovie_start; mirv_recordmovie_stop\"\n");

    // Direct-to-video (docs/direct_to_video_capture.md), driven by the capture-
    // mode toggle. The probe that introduced this settled all four of its open
    // questions against a real capture: {AFX_STREAM_PATH} resolves to the
    // per-stream folder, it follows mirv_movie_filename (so the _route_N
    // junction routing keeps working), takes are still auto-numbered into
    // take0000, and sound.wav still lands beside the video.
    //
    // `all` here is HLAE's stream *group*, not the folder name it happens to
    // share with the composited stream. Measured with mirv_movie_separate_hud
    // on: the group does reach hudColor and hudAlpha, so all three streams get
    // a video and no per-stream enabled/options lines are needed. The folders
    // HLAE writes are lowercase (`hudcolor`/`hudalpha`) even though the command
    // names those streams in camelCase.
    //
    // Set here, once, at load — never as an injected ConsoleCommand frame. The
    // options string is several times GoldSrc's 64-byte Cbuf_AddTextToBuffer
    // limit with no staggering available (one argument to one command), and an
    // injected frame would shift every later frame ordinal by +1 and desync the
    // scheduled capture commands. Same rule r_decals follows.
    //
    // Not inside an alias either: GoldSrc cannot parse nested quotes in one, and
    // this argument has to carry them. As a plain command in an exec'd cfg it is
    // ordinary syntax. {QUOTE} is HLAE's own token for a literal quote, which is
    // what keeps the inner path quoting away from the engine's parser entirely.
    //
    // The codec comes from the config — see `CaptureCodec` for why every option
    // is lossless and RGB/4:4:4, and why the size ranking there is not the
    // real-time-viability ranking.
    if config.capture_mode == crate::patch::CaptureMode::DirectToVideo {
        let codec_args = config.ffmpeg_capture_codec.args();
        helper_cfg_content.push_str("\n# Direct-to-video capture\n");
        helper_cfg_content.push_str("mirv_movie_ffmpeg all enabled 1\n");
        helper_cfg_content.push_str(&format!(
            "mirv_movie_ffmpeg all options \"{} {{QUOTE}}{{AFX_STREAM_PATH}}\\{}{{QUOTE}}\"\n",
            codec_args,
            crate::hlcr::scanner::VIDEO_FILE,
        ));
        crate::log_markdown(&format!(
            "🎬 **Direct-to-video capture** — HLAE pipes frames to FFmpeg (`{}`) instead of \
             writing a BMP sequence, one `{}` per stream folder. Render Studio reads these the \
             same way it reads frame sequences.",
            codec_args,
            crate::hlcr::scanner::VIDEO_FILE,
        ));
    }



    // 1. Primer Job
    if total_jobs > 0 {
        let first_source = sorted_groups[0].0.0.to_string();
        let mut primer_init = config.init_commands.clone();
        primer_init.push("sys_autodir".to_string());

        // Every patched demo lands directly in the game's own dod/ folder --
        // that's the only place GoldSrc's `playdemo` can find it. This used
        // to resolve to capture_directories[0] instead, with capture_engine.rs
        // copying it into dod/ as a second step -- pure redundant I/O, since
        // that copy loop already ran everything upfront before hl.exe even
        // launched, so dod/'s drive needed the full batch footprint either
        // way. capture_directories is only ever about where recorded video
        // blocks land now, not these small demo files. See issue #8.
        let primer_out = dod_dir.join("primer.dem");

        // Delay playdemo chain_01 to tick 500 (~5 seconds) to allow the engine to fully finish the 
        // 2-second GoldSrc server handshake without buffer overflows before jumping to the first real chain.
        let mut primer_scheduled = Vec::new();
        helper_cfg_content.push_str("# Demo specific next demos\n");
        helper_cfg_content.push_str("alias primer_next \"playdemo chain_01\"\n");
        primer_scheduled.push((crate::patch::PRIMER_DELAY_TICKS, "primer_next".to_string()));

        jobs.push(PatchJob {
            source_demo: first_source.clone(),
            output_demo: primer_out,
            streaks: Vec::new(),
            target_player: None,
            init_commands: primer_init,
            scheduled_commands: primer_scheduled,
            director_events: Vec::new(),
            block_routes: Vec::new(),
            blocks: Vec::new(),
        });
    }

    // 2. Chained Jobs
    // Every patched demo file now lands directly in dod/ (see the primer's
    // own resolution above), not on any capture_directories entry -- so
    // utilized_drives only ever needs to track drives that actually receive
    // a real recording block. See issue #8.
    let mut utilized_drives = std::collections::HashSet::new();
    for (job_idx, ((source_demo, target_player), mut streak_refs)) in sorted_groups.into_iter().enumerate() {
        // Sort by start_tick in ascending order
        streak_refs.sort_by_key(|(_, s)| s.start_tick);
        let (streak_payload_indices, streaks): (Vec<usize>, Vec<CaptureStreak>) =
            streak_refs.into_iter().map(|(idx, s)| (idx, s.clone())).unzip();

        let total_demo_frames = streaks.first().map(|s| s.total_demo_frames).unwrap_or(0);

        // One svc_director STUFFTEXT event per streak — label mirrors the highlight table:
        // "#<row>: <kill_count> kills: <timeline_string>"
        let mut director_events: Vec<(i32, String)> = streaks.iter().enumerate().map(|(i, s)| {
            let label = format!("#{}: {} kills: {}", i + 1, s.kill_count, s.timeline_string);
            (s.start_tick, label)
        }).collect();

        if let Some(first_streak) = streaks.first() {
            let match_tick = first_streak.match_start_tick.unwrap_or(0);
            director_events.push((match_tick, "echo [dod-tools] MATCH_START".to_string()));
            let demo_end_tick = total_demo_frames;
            director_events.push((demo_end_tick, "echo [dod-tools] DEMO_END".to_string()));
        }
        director_events.sort_by_key(|e| e.0);

        let demo_fps = streaks.first().map(|s| s.demo_fps).filter(|&fps| fps > 0.0).unwrap_or(30.0);

        let demo_name = format!("chain_{:02}", job_idx + 1);
        let next_demo_name = format!("chain_{:02}", job_idx + 2);
        let output_name = format!("{}.dem", demo_name);
        // Lands directly in dod/ -- see the primer's own resolution above for why.
        let output_demo = dod_dir.join(&output_name);

        // ── AOT failover routing (Per-Block) ───────────────────────────────────
        
        // Block cutting.
        //
        // Two separate questions, deliberately not conflated (they used to be,
        // which made every roll-window collision collapse into one take full of
        // dead air between the two highlights):
        //
        //  1. Do the recordings themselves overlap? Only then must the two
        //     highlights become a single take — you can't run two
        //     mirv_recordmovie sessions at once.
        //  2. Otherwise they stay separate takes, but if the fast-forward
        //     round trip between them doesn't fit, it's dropped and playback
        //     just stays at normal speed across the gap. Costs a couple of
        //     seconds of real-time playback and yields two clean clips instead
        //     of one blob. The stopsound flush goes with it — that exists to
        //     repair audio the fast-forward desyncs, and there's no
        //     fast-forward here to repair.
        //
        // `merged_sources` stays index-aligned with `merged_streaks`, recording
        // which dispatched highlights each block covers — the merge keeps only
        // the first streak's fields, so the mapping back to the caller's
        // highlights would otherwise be lost here. `chained_to_previous` marks
        // blocks that run straight on from the one before at normal speed.
        let dynamic_pre_roll_ticks = (config.pre_roll_seconds * demo_fps) as i32;
        let dynamic_post_roll_ticks = (config.post_roll_seconds * demo_fps) as i32;
        let start_lead_ticks = (config.record_start_lead * demo_fps) as i32;
        let stop_trail_ticks = (config.record_stop_trail * demo_fps) as i32;

        let mut merged_streaks: Vec<CaptureStreak> = Vec::new();
        let mut merged_sources: Vec<Vec<usize>> = Vec::new();
        let mut chained_to_previous: Vec<bool> = Vec::new();

        for (current, payload_idx) in streaks.into_iter().zip(streak_payload_indices) {
            if merged_streaks.is_empty() {
                merged_streaks.push(current);
                merged_sources.push(vec![payload_idx]);
                chained_to_previous.push(false);
                continue;
            }

            let prev_stop = last_kill_frame(merged_streaks.last().unwrap());
            let next_start = first_kill_frame(&current);

            // Recordings overlap, or sit too close for a safe stop/start cycle.
            // OBS needs longer between takes than HLAE does, and the number is
            // measured rather than guessed: ~1.065s to finalise a file after
            // `StopRecord` returned, against the 1.0s this guard promises. So
            // the HLAE-derived constant is already too tight for that path by a
            // small margin, and two highlights that merge today would otherwise
            // ask OBS for a stop/start cycle it cannot service.
            let min_separation_ticks = (take_separation_seconds(config) * demo_fps) as i32;
            if blocks_merge(
                prev_stop,
                next_start,
                start_lead_ticks,
                stop_trail_ticks + min_separation_ticks,
            ) {
                let last = merged_streaks.last_mut().unwrap();
                last.end_tick = last.end_tick.max(current.end_tick);
                // The record-stop mark is derived from kills[end_index] below,
                // not from end_tick, so the absorbed highlight's final kill has
                // to join this block's kill list too. Without it recording stops
                // at the *first* highlight's last kill and everything merged in
                // after that is missing from the take — while still looking like
                // one successfully captured block.
                let absorbed_last = current.end_index.min(current.kills.len().saturating_sub(1));
                if let Some(kill) = current.kills.get(absorbed_last).cloned() {
                    last.kills.push(kill);
                    last.end_index = last.kills.len() - 1;
                }
                merged_sources.last_mut().unwrap().push(payload_idx);
            } else {
                let rolls_collide = blocks_merge(
                    prev_stop,
                    next_start,
                    start_lead_ticks + dynamic_pre_roll_ticks,
                    stop_trail_ticks + dynamic_post_roll_ticks,
                );
                merged_streaks.push(current);
                merged_sources.push(vec![payload_idx]);
                chained_to_previous.push(rolls_collide);
            }
        }

        let mut block_routes = Vec::new();

        // Byte estimate per block, index-aligned with merged_streaks.
        let block_estimates: Vec<u64> = merged_streaks.iter().map(|streak| {
            let anchor_duration = ((streak.end_tick - streak.start_tick) as f32) / demo_fps.max(1.0);
            let clip_duration_secs = config.calculate_total_capture_duration(anchor_duration);
            crate::sys::disk::calculate_raw_sequence_bytes(
                config.resolution_width,
                config.resolution_height,
                config.capture_fps,
                clip_duration_secs,
            )
        }).collect();

        let assignments = allocate_blocks_first_fit_decreasing(
            &block_estimates,
            &mut drive_free,
            &mut active_drive_idx,
            FAILOVER_THRESHOLD,
        ).map_err(|_| std::io::Error::other(
            "Insufficient space across all mapped drives to allocate a block"
        ))?;

        // Route aliases key off each block's original index in merged_streaks,
        // not allocation order, so the scheduled-command lookup below (which
        // still walks merged_streaks in its original order) is unaffected.
        let mut blocks: Vec<crate::patch::types::CaptureBlock> = Vec::new();
        for (block_index, drive_idx) in assignments {
            let streak = &merged_streaks[block_index];
            block_routes.push((streak.start_tick, streak.end_tick, drive_idx));
            utilized_drives.insert(drive_idx);
            helper_cfg_content.push_str(&format!(
                "alias {}_route_{} \"mirv_movie_filename _route_{}/{}_b{}\"\n",
                demo_name, block_index, drive_idx, demo_name, block_index
            ));

            // Mirror of the junction target built after this loop, so the two
            // can't drift: _route_{drive} links to <capture_dir>/<session_id>,
            // and HLAE writes <demo_name>_b<block_index> underneath it.
            let take_folder = match config.capture_directories.get(drive_idx) {
                Some(out_dir) => {
                    let absolute_drive = std::path::absolute(out_dir)?;
                    let session_root = if config.session_id.is_empty() {
                        absolute_drive
                    } else {
                        absolute_drive.join(&config.session_id)
                    };
                    session_root.join(format!("{}_b{}", demo_name, block_index))
                }
                // No capture directories configured — the drive_free sentinel
                // path. Falls back to the session dir resolved from
                // primary_media_dir above, matching where output actually lands.
                None => session_dir.join(format!("{}_b{}", demo_name, block_index)),
            };

            blocks.push(crate::patch::types::CaptureBlock {
                demo_name: demo_name.clone(),
                block_index,
                drive_index: drive_idx,
                take_key: crate::shared::paths::take_key(&take_folder).unwrap_or_default(),
                take_folder,
                source_streak_indices: merged_sources[block_index].clone(),
                start_tick: streak.start_tick,
                end_tick: streak.end_tick,
                // Filled in by the scheduling loop below, which is where the
                // record bounds are actually derived. Left at 0 here rather
                // than duplicating that arithmetic.
                record_start_tick: 0,
                record_stop_tick: 0,
            });
        }
        blocks.sort_by_key(|b| b.block_index);

        if job_idx < total_jobs - 1 {
            helper_cfg_content.push_str(&format!("alias {}_next \"playdemo {}\"\n", demo_name, next_demo_name));
        } else {
            helper_cfg_content.push_str(&format!("alias {}_next \"sys_capture_done_path\"\n", demo_name));
        }

        // Generate scheduled commands
        let mut scheduled_commands = Vec::new();

        // Initialize Engine Speed after Initial Load Delay
        let initial_delay_ticks = (config.initial_delay * demo_fps) as i32;
        scheduled_commands.push((initial_delay_ticks, "sys_fast_forward".to_string()));

        // One marker per demo, at its very first scheduled tick — the engine's
        // own live signal that this demo has started playing. Read back by
        // `native/src/obs/log_tail.rs` as `MarkerKind::DemoStart`, carrying
        // (job_idx, total_jobs, clip_count) 1-based, and forwarded up as
        // `EngineEvent::DemoLoading` for the "loading demo N of M" OS
        // notification (issue #98). `merged_streaks.len()` is this job's
        // final clip count, already settled by the merge loop above.
        for (t, echo_cmd) in build_safe_echos(
            initial_delay_ticks,
            &format!("DEMO_START {} {} {}", job_idx + 1, total_jobs, merged_streaks.len()),
        ) {
            scheduled_commands.push((t, echo_cmd));
        }

        // Fast-forward towards clip 1 starts at the very same tick as
        // DEMO_START above — playback has nothing to do before it. Read back
        // as `MarkerKind::NextClip` for the "fast-forwarding to clip N of M"
        // OS notification (issue #98).
        for (t, echo_cmd) in build_safe_echos(
            initial_delay_ticks,
            &format!("NEXT_CLIP {} {} {} {}", job_idx + 1, total_jobs, 1, merged_streaks.len()),
        ) {
            scheduled_commands.push((t, echo_cmd));
        }

        for (i, streak) in merged_streaks.iter().enumerate() {


            let frame_times_ref = global_arrays
                .get(std::path::Path::new(&streak.source_demo))
                .map(|a| a.as_slice())
                .unwrap_or_else(|| streak.frame_times.as_slice());

            // Always use streak.total_demo_frames (which maps to the demo's true
            // playback_frames from the header) as the authoritative final frame count.
            // Do not trust frame_times_ref.len() on its own, as it is truncated
            // at the demo section boundary (type_byte == 5) during scanning.
            let absolute_final_frame = streak.total_demo_frames.max(frame_times_ref.len() as i32);
            let exit_frame = absolute_final_frame.saturating_sub(5);
            let danger_zone = absolute_final_frame.saturating_sub(10);

            // Resolve the physical frame index for the start/end kills directly from
            // kills[i].0 (= frame_index, the 1-based frame counter used during scanning),
            // which is aligned with frame_times_ref[].
            //
            // NOTE: Do NOT apply a .min(frame_times_ref.len()-1) clamp here.
            // When frame_times_ref is empty that saturates to 0 and destroys the index.
            // find_tick_backwards / find_tick_forwards already clamp start_frame
            // internally, and return start_frame as-is when frame_times is empty.
            //
            // NOTE: Do NOT use viewdemo_times here. viewdemo_times[i] stores
            // viewdemo_offset (= SVC_TIME server game clock, e.g. 300.0 s from server
            // start), while frame_times_ref[] contains demo-playback timestamps starting
            // near 0.0. The two domains are incompatible — a position() search would
            // always return None → unwrap_or(0) → every command collapsed to tick 0.
            let physical_frame = first_kill_frame(streak).max(0) as usize;
            let physical_end_frame = last_kill_frame(streak).max(0) as usize;

            let record_start_tick = find_tick_backwards(physical_frame, config.record_start_lead, frame_times_ref, demo_fps);
            // Pre-roll is the settle window: playback drops back to normal speed
            // this far ahead of the record start so audio isn't glitched by the
            // fast-forward, then stopsound flushes just before recording begins.
            // The sound flush is clamped so it can never precede the speed drop
            // (which it would for any pre-roll under a second).
            let s_speed_tick = find_tick_backwards(record_start_tick.max(0) as usize, config.pre_roll_seconds, frame_times_ref, demo_fps);
            let sound_lead = config.pre_roll_seconds.min(SOUND_FLUSH_LEAD_SECONDS);
            let s_sound_tick = find_tick_backwards(record_start_tick.max(0) as usize, sound_lead, frame_times_ref, demo_fps);
            let mut r_stop = find_tick_forwards(physical_end_frame, config.record_stop_trail, frame_times_ref, demo_fps);
            let mut s_end = find_tick_forwards(r_stop.max(0) as usize, config.post_roll_seconds, frame_times_ref, demo_fps);

            let mut is_clutch = false;
            if s_end >= danger_zone {
                let demo_file_name = std::path::Path::new(&streak.source_demo).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                crate::log_markdown(&format!("⚠️ **EOF Boundary Reached:** Highlight #{} in demo '{}' (Player: {}) has a post-roll that exceeds the demo's end frame. Post-roll truncated to save batch.", i + 1, demo_file_name, streak.target_player.as_deref().unwrap_or("Unknown")));
                is_clutch = true;
                r_stop = r_stop.min(exit_frame);
                s_end = exit_frame;
            } else {
                r_stop = r_stop.min(exit_frame);
                s_end = s_end.min(exit_frame);
            }

            // Hand the finished record bounds back to the block. This loop and
            // the block-allocation loop above both walk `merged_streaks` in its
            // original order, so `i` is the block index — matched on rather
            // than indexed with, since allocation may reorder `blocks`.
            // The decal flush is the consumer: it needs the frames that end up
            // in the take, which `start_tick`/`end_tick` are not.
            if let Some(block) = blocks.iter_mut().find(|b| b.block_index == i) {
                block.record_start_tick = record_start_tick;
                block.record_stop_tick = r_stop;
            }

            // Custom command overrides
            for (idx, custom) in config.custom_commands.iter().enumerate() {
                let relation_str = match custom.relation {
                    CommandRelation::Before => "BEFORE",
                    CommandRelation::After => "AFTER",
                };
                let target_tick = match custom.relation {
                    CommandRelation::Before => {
                        let mut t = find_tick_backwards(physical_frame, custom.offset, frame_times_ref, demo_fps);
                        if t == s_speed_tick || t == s_sound_tick || t == record_start_tick {
                            t += 1;
                        }
                        t
                    }
                    CommandRelation::After => {
                        let mut t = find_tick_forwards(physical_end_frame, custom.offset, frame_times_ref, demo_fps);
                        if t == r_stop {
                            t += 1;
                        }
                        t
                    }
                };

                for (t, echo_cmd) in build_safe_echos(target_tick, &format!("CUSTOM_CMD{}_{} - Tick {}", idx + 1, relation_str, target_tick)) {
                    scheduled_commands.push((t, echo_cmd));
                }
                let cmd_len = custom.command.len();
                if cmd_len > crate::patch::CUSTOM_CMD_WARN_LIMIT {
                    crate::log_markdown(&format!("⚠️ **WARNING:** Custom command exceeds 60 bytes and will likely be dropped by the GoldSrc Cbuf: {}", custom.command));
                }

                // Playback runs at `host_framerate 0.05` until the pre-roll
                // drops it back to real time at `s_speed_tick`, and resumes
                // fast-forwarding once the post-roll ends at `s_end`. A command
                // landing outside that window still executes, but it executes
                // while the engine is racing through frames with its audio
                // buffers in a bad state — so anything about sound, timing or
                // rendering does something other than what it looks like it
                // does, and nothing in the captured video explains why.
                if runs_during_fast_forward(target_tick, s_speed_tick, s_end) {
                    let (where_, fix) = if target_tick < s_speed_tick {
                        (
                            "before playback drops back to real time",
                            format!(
                                "keep the offset under the {:.1}s pre-roll, or raise the pre-roll",
                                config.pre_roll_seconds
                            ),
                        )
                    } else {
                        (
                            "after the post-roll ends and fast-forward resumes",
                            format!(
                                "keep the offset under the {:.1}s post-roll, or raise the post-roll",
                                config.post_roll_seconds
                            ),
                        )
                    };
                    crate::log_markdown(&format!(
                        "ℹ️ **Scheduled command runs during fast-forward** — `{}` is set {} {:.1}s \
                         {} the highlight, which lands at tick {}, {}. Setting a cvar there is \
                         usually fine — it simply takes effect early. It is worth checking only \
                         for commands that depend on playback running at real speed: anything \
                         touching sound, recording, or rendering, since the engine is at \
                         `host_framerate 0.05` with its audio buffers unflushed. To have it run \
                         at normal speed instead, {}.",
                        custom.command,
                        relation_str.to_lowercase(),
                        custom.offset,
                        if matches!(custom.relation, CommandRelation::Before) { "before" } else { "after" },
                        target_tick,
                        where_,
                        fix
                    ));
                }
                scheduled_commands.push((target_tick, custom.command.clone()));
            }

            // Stages 1 and 1.5 are the exit from fast-forward. A block chained
            // to the one before it never left normal speed, so there's nothing
            // to drop back to and no fast-forward-induced audio drift to flush.
            let resumes_from_fast_forward = !chained_to_previous.get(i).copied().unwrap_or(false);

            if resumes_from_fast_forward {
                // At Speed Flush (Stage 1)
                scheduled_commands.push((s_speed_tick, "sys_normal_speed".to_string()));
                scheduled_commands.push((s_speed_tick + 1, "sys_normal_speed".to_string()));
                scheduled_commands.push((s_speed_tick + 2, "sys_normal_speed".to_string()));
                for (t, echo_cmd) in build_safe_echos(s_speed_tick, &format!("SPEED_FLUSH - Tick {}", s_speed_tick)) {
                    scheduled_commands.push((t, echo_cmd));
                }

                // At Sound Flush (Stage 1.5)
                scheduled_commands.push((s_sound_tick, "sys_sound".to_string()));
                for (t, echo_cmd) in build_safe_echos(s_sound_tick, &format!("AUDIO_SYNC - Tick {}", s_sound_tick)) {
                    scheduled_commands.push((t, echo_cmd));
                }
            }

            // At Start Frame (Stage 2)
            scheduled_commands.push((record_start_tick, format!("{}_route_{}", demo_name, i)));
            scheduled_commands.push((record_start_tick, "sys_record_start".to_string()));
            for (t, echo_cmd) in build_safe_echos(record_start_tick, &format!("START_RECORD - Tick {}", record_start_tick)) {
                scheduled_commands.push((t, echo_cmd));
            }

            // At End Frame (Stage 3)
            scheduled_commands.push((r_stop, "sys_record_stop".to_string()));
            for (t, echo_cmd) in build_safe_echos(r_stop, &format!("STOP_RECORD - Tick {}", r_stop)) {
                scheduled_commands.push((t, echo_cmd));
            }

            // At Post-Roll End (Stage 4).
            // Skipped when the next block starts too soon for the round trip to
            // fit — playback just stays at normal speed into it instead.
            let next_block_chained = chained_to_previous.get(i + 1).copied().unwrap_or(false);
            if !next_block_chained {
                scheduled_commands.push((s_end, "sys_fast_forward".to_string()));
                for (t, echo_cmd) in build_safe_echos(s_end, &format!("FAST_FORWARD - Tick {}", s_end)) {
                    scheduled_commands.push((t, echo_cmd));
                }
                // `chained_to_previous.get(i + 1)` also returns the "false"
                // default when there simply is no block i+1 (the last block
                // in the demo) -- that's not a real next clip to announce, so
                // this needs its own bounds check rather than reusing
                // `!next_block_chained` alone.
                if i + 1 < merged_streaks.len() {
                    for (t, echo_cmd) in build_safe_echos(
                        s_end,
                        &format!("NEXT_CLIP {} {} {} {}", job_idx + 1, total_jobs, i + 2, merged_streaks.len()),
                    ) {
                        scheduled_commands.push((t, echo_cmd));
                    }
                }
            }

            if i == merged_streaks.len() - 1 {
                // At Absolute EOF
                if job_idx == total_jobs - 1 {
                    let echos = build_safe_echos(s_end, "BATCH_COMPLETE");
                    let echos_len = echos.len() as i32;
                    for (t, echo_cmd) in echos {
                        scheduled_commands.push((t, echo_cmd));
                    }
                    let final_tick = if is_clutch { exit_frame } else { s_end + echos_len };
                    scheduled_commands.push((final_tick, format!("{}_next", demo_name)));
                } else {
                    let final_tick = if is_clutch { exit_frame } else { s_end };
                    scheduled_commands.push((final_tick, format!("{}_next", demo_name)));
                }
            }
        }

        // Implement Global Breadcrumb Loop
        let total_demo_frames = merged_streaks.first().map(|s| s.total_demo_frames).unwrap_or(0);
        let mut step = 0;
        while step < total_demo_frames {
            scheduled_commands.push((
                step, 
                format!("echo \"[dod-tools] BREADCRUMB - Tick {}\"", step)
            ));
            step += crate::patch::BREADCRUMB_INTERVAL_TICKS;
        }

        // Sort scheduled_commands by tick
        scheduled_commands.sort_by_key(|(tick, _)| *tick);

        let final_init_commands = final_init_commands(config);

        jobs.push(PatchJob {
            source_demo: source_demo.to_string(),
            output_demo,
            streaks: merged_streaks,
            target_player: target_player.map(|s| s.to_string()),
            init_commands: final_init_commands,
            scheduled_commands,
            director_events,
            block_routes,
            blocks,
        });
    }
    
    // Create directory junctions for utilized drives
    let game_path_buf = std::path::PathBuf::from(&config.game_path);
    let hl_exe_parent = game_path_buf.parent().unwrap_or(std::path::Path::new(""));
    for &drive_idx in &utilized_drives {
        if let Some(out_dir) = config.capture_directories.get(drive_idx) {
            let absolute_drive = std::path::absolute(out_dir)?;
            let session_dir = if !config.session_id.is_empty() {
                absolute_drive.join(&config.session_id)
            } else {
                absolute_drive
            };
            
            if !session_dir.exists() {
                let _ = std::fs::create_dir_all(&session_dir);
            }
            
            let junction_path = hl_exe_parent.join(format!("_route_{}", drive_idx));
            let _ = std::fs::remove_dir(&junction_path);
            
            let junction_str = junction_path.to_str().unwrap_or_default();
            let target_str = session_dir.to_str().unwrap_or_default();
            
            if !junction_str.is_empty() && !target_str.is_empty() {
                let _ = std::process::Command::new("cmd")
                    .args(["/C", "mklink", "/J", junction_str, target_str])
                    .output();
            }
        }
    }

    // Write dodtools_helper.cfg to dod_dir
    if !dod_dir.exists() {
        std::fs::create_dir_all(&dod_dir)?;
    }
    let cfg_path = dod_dir.join("dodtools_helper.cfg");
    std::fs::write(&cfg_path, helper_cfg_content)?;

    // Final per-drive headroom for every drive this batch actually touches,
    // handed back so the pre-launch abort in `capture_engine.rs` re-validates
    // the exact numbers this allocation pass already computed instead of
    // recomputing a third, narrower (primary-drive-only) answer.
    let drive_headroom: Vec<crate::patch::types::DriveHeadroom> = utilized_drives
        .into_iter()
        .filter_map(|idx| config.capture_directories.get(idx).map(|p| crate::patch::types::DriveHeadroom {
            path: p.clone(),
            free_bytes: drive_free[idx],
        }))
        .collect();

    Ok((jobs, drive_headroom))
}

pub struct WorkspaceGuard {
    pub session_junction: std::path::PathBuf,
    pub exit_trigger: std::path::PathBuf,
    pub pool_junctions: Vec<std::path::PathBuf>,
    /// The `_route_N` junctions `build_batch_queue` creates beside `hl.exe`, one
    /// per drive a batch routes blocks to.
    ///
    /// These were created but never unlinked: the only thing that removed one
    /// was the *next* batch reusing that index, so a `_route_0` sat in the game
    /// folder indefinitely after a capture. Tracked here so they go the same way
    /// as the pool junctions — when the batch ends, not when another begins.
    pub route_junctions: Vec<std::path::PathBuf>,
    pub auto_clear_logs: bool,
    pub auto_clear_temp_demos: bool,
    pub auto_clear_previews: bool,
    pub save_local_patched_copy: bool,
}

impl Drop for WorkspaceGuard {
    fn drop(&mut self) {
        // Junction link: remove_dir unlinks without touching the junction target.
        if let Err(e) = std::fs::remove_dir(&self.session_junction)
            && e.kind() != std::io::ErrorKind::NotFound {
                log::warn!("[WorkspaceGuard::drop] Failed to remove session_junction {:?}: {}", self.session_junction, e);
            }
        // Unlink every dod_pool_N and _route_N junction. `remove_dir` unlinks a
        // junction without touching what it points at, and NotFound is expected
        // for any index this batch did not route to.
        for junction in self.pool_junctions.iter().chain(self.route_junctions.iter()) {
            if let Err(e) = std::fs::remove_dir(junction)
                && e.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("[WorkspaceGuard::drop] Failed to remove pool junction {:?}: {}", junction, e);
                }
        }
        // Signal dirs (DOD_TOOLS_EXIT_TRIGGER) are directories, not files.
        // Use remove_dir_all; silently ignore NotFound, log anything else.
        if let Err(e) = std::fs::remove_dir_all(&self.exit_trigger)
            && e.kind() != std::io::ErrorKind::NotFound {
                log::warn!("[WorkspaceGuard::drop] Failed to remove exit_trigger {:?}: {}", self.exit_trigger, e);
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

// ── Channel-based worker spawner ──────────────────────────────────────────────
// Retained for the cancellation test and any callers that still reference it.
// The primary patch path now uses the inline patch_worker in capture/select.rs.

pub fn spawn_patch_batch(
    jobs: Vec<PatchJob>,
    config: PatcherConfig,
    cancel_token: Arc<AtomicBool>,
) -> CaptureWorker {
    let (tx, rx) = std::sync::mpsc::channel();
    let cancel_token_clone = cancel_token.clone();

    let handle = std::thread::spawn(move || {
        let total_jobs = jobs.len();
        if tx.send(PatchEvent::Starting(total_jobs)).is_err() {
            return;
        }

        let mut cancelled = false;
        for (idx, job) in jobs.iter().enumerate() {
            let start_pct = (idx as f32 / total_jobs as f32) * 100.0;
            if tx.send(PatchEvent::Progress(job.source_demo.clone(), start_pct)).is_err() {
                return;
            }

            let patcher = StreamPatcher::new(&job.source_demo, &job.output_demo);
            match patcher.patch(job, &config, &cancel_token_clone) {
                Ok(()) => {
                    let end_pct = ((idx + 1) as f32 / total_jobs as f32) * 100.0;
                    if tx.send(PatchEvent::Progress(job.source_demo.clone(), end_pct)).is_err() {
                        return;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                    std::fs::remove_file(&job.output_demo).ok();
                    let _ = tx.send(PatchEvent::Cancelled);
                    cancelled = true;
                    break;
                }
                Err(e) => {
                    if tx.send(PatchEvent::Error(format!("Failed to patch {}: {}", job.source_demo, e))).is_err() {
                        return;
                    }
                }
            }
        }

        if !cancelled {
            let _ = tx.send(PatchEvent::Completed);
        }
    });

    CaptureWorker {
        receiver: rx,
        is_running: true,
        cancel_token,
        handle: Some(handle),
    }
}

// ── Preview-only patcher ──────────────────────────────────────────────────────
// Builds minimal PatchJobs that inject ONLY svc_director STUFFTEXT events into
// a copy of the original demo — no capture commands, no chaining, no CFG files.
// One output demo per source demo, saved as "<stem>_preview.dem" next to the
// original (or inside `output_dir` if configured).

/// Makes a demo stem safe as a `playdemo`/`viewdemo` target.
///
/// `launch_demo_preview` passes the output stem straight into HLAE's
/// `-cmdLine`, which becomes hl.exe's own startup command line — GoldSrc
/// tokenizes that on whitespace and treats any `+`/`-` prefixed token as the
/// start of a new launch parm, so an embedded hyphen (common in
/// match-recorded demo names, e.g. "team1-vs-team2") silently truncates the
/// `+viewdemo` target at the first one instead of failing loudly. Confirmed
/// live: a source stem of "wsod25-po_r3_sf-..." loaded as bare "wsod25".
/// Per docs/goldsrc_dod_quirks.md, playdemo/viewdemo targets must also stay
/// under ~40 characters, which this stem was already over before appending
/// "_preview" — both constraints are enforced here, once, at the point the
/// output filename is chosen, so neither preview entry point (this one, or
/// `generate_all_previews`'s later manual load) can hit it again.
fn playdemo_safe_stem(raw: &str) -> String {
    // Reserve room for the "_preview" suffix appended below, and stay a few
    // characters under the documented ~40 char limit rather than right at it.
    const SUFFIX_LEN: usize = "_preview".len();
    const BUDGET: usize = 36;
    const BASE_BUDGET: usize = BUDGET - SUFFIX_LEN;

    let sanitized: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    if sanitized.len() <= BASE_BUDGET {
        return sanitized;
    }
    // Too long even after sanitizing — truncate, but append a hash of the
    // *original* stem so two long names sharing a prefix don't collide onto
    // the same output file.
    let hash_suffix = format!("_{:08x}", crate::utils::demo_hasher::fnv1a_hash(raw.as_bytes()) as u32);
    let keep = BASE_BUDGET.saturating_sub(hash_suffix.len());
    format!("{}{}", &sanitized[..keep], hash_suffix)
}

pub fn build_preview_patch_jobs(
    raw_streaks: Vec<CaptureStreak>,
    output_dir: Option<&std::path::Path>,
) -> Vec<PatchJob> {
    // Group all streaks by source demo path.
    let mut grouped: std::collections::HashMap<String, Vec<CaptureStreak>> =
        std::collections::HashMap::new();
    for streak in raw_streaks {
        grouped.entry(streak.source_demo.clone()).or_default().push(streak);
    }

    let mut jobs = Vec::new();

    for (source_demo, mut streaks) in grouped {
        // Sort chronologically.
        streaks.sort_by_key(|s| s.start_tick);

        // Build (tick, label) for each streak — same format as the highlight table.
        let mut director_events: Vec<(i32, String)> = streaks.iter().enumerate().map(|(i, s)| {
            let label = format!("#{}/{}: {} kills: {}", i + 1,streaks.len(), s.kill_count, s.timeline_string);
            let preview_tick = find_tick_backwards(s.start_tick as usize, 3.0, &s.frame_times, s.demo_fps);
            (preview_tick, label)
        }).collect();

        if let Some(first_streak) = streaks.first() {
            let match_frame_idx = 0; // Float time unavailable for match start
            director_events.push((match_frame_idx, "echo [dod-tools] MATCH_START".to_string()));
            let total_demo_frames = if first_streak.total_demo_frames > 0 { first_streak.total_demo_frames } else { first_streak.frame_times.len() as i32 };
            let demo_end_tick = total_demo_frames;
            director_events.push((demo_end_tick, "echo [dod-tools] DEMO_END".to_string()));
        }
        director_events.sort_by_key(|e| e.0);

        // Resolve output path: "<stem>_preview.dem" beside original, or in output_dir.
        let source_path = std::path::PathBuf::from(&source_demo);
        let stem = source_path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy();
        let safe_stem = playdemo_safe_stem(&stem);
        let preview_name = format!("{}_preview.dem", safe_stem);
        let output_demo = if let Some(dir) = output_dir {
            dir.join(&preview_name)
        } else {
            source_path.with_file_name(&preview_name)
        };

        jobs.push(PatchJob {
            source_demo,
            output_demo,
            streaks,
            target_player: None,
            // No capture init commands — preview only.
            init_commands: Vec::new(),
            // No scheduled capture commands — preview only.
            scheduled_commands: Vec::new(),
            director_events,
            block_routes: Vec::new(),
            blocks: Vec::new(),
        });
    }

    jobs
}

// ── svc_director payload builder ──────────────────────────────────────────────

/// Build a GoldSrc `svc_director` (OpCode 0x33) net-message payload for a
/// `DRC_CMD_MESSAGE` (sub-command 0x06) HLTV title card.
///
/// The returned `Vec<u8>` is a self-contained net-message body ready to be
/// embedded inside a `Dem_NetworkBuffer` (frame type 0x00 / 0x01) payload.
///
/// Fixed wire layout (30 bytes before the text):
///
/// | Offset | Size | Value       | Meaning                 |
/// |--------|------|-------------|-------------------------|
/// | 0      | 1    | 0x33        | svc_director opcode     |
/// | 1      | 1    | payload_len | total bytes after opcode|
/// | 2      | 1    | 0x06        | DRC_CMD_MESSAGE         |
/// | 3      | 1    | 0x00        | effect (none)           |
/// | 4      | 4    | FF A0 00 00 | RGBA colour #FFA000FF   |
/// | 8      | 4    | -1.0 f32 LE | position X (centered)   |
/// | 12     | 4    | 0.85 f32 LE | position Y              |
/// | 16     | 4    | 0.5  f32 LE | fade-in  (seconds)      |
/// | 20     | 4    | 0.5  f32 LE | fade-out (seconds)      |
/// | 24     | 4    | 3.0  f32 LE | hold time (seconds)     |
/// | 28     | 4    | 0.0  f32 LE | FX time                 |
/// | 32     | N+1  | text + \0   | null-terminated string  |
///
/// `payload_len` = 30 (fields 2-31) + text_len + 1 (null), capped at 255.
pub fn build_director_message(text: &str) -> Vec<u8> {
    // Null-terminate and clamp so payload_len fits in one byte.
    // payload_len covers everything from the sub-command byte (offset 2) to the
    // end of the null-terminated string, i.e. 30 fixed bytes + string + NUL.
    // Maximum payload_len = 255, so maximum text bytes = 255 - 30 - 1 = 224.
    const FIXED_OVERHEAD: usize = 30; // bytes 2..31 (sub-cmd through FX time)
    const MAX_TEXT_BYTES: usize = 255 - FIXED_OVERHEAD - 1; // 224

    let raw = text.as_bytes();
    let text_len = raw.len().min(MAX_TEXT_BYTES);
    let text_bytes = &raw[..text_len];

    // payload_len is everything after the opcode and length byte itself.
    let payload_len: u8 = (FIXED_OVERHEAD + text_len + 1) as u8;

    let mut msg: Vec<u8> = Vec::with_capacity(2 + FIXED_OVERHEAD + text_len + 1);

    // Opcode + payload length
    msg.push(0x33);          // svc_director
    msg.push(payload_len);

    // Sub-command and effect
    msg.push(0x06);          // DRC_CMD_MESSAGE
    msg.push(0x00);          // effect: none

    // RGBA colour #FFA000FF
    msg.extend_from_slice(&[0xFF, 0xA0, 0x00, 0x00]);

    // Position (X = -1.0 → engine centers horizontally; Y = 0.85)
    msg.extend_from_slice(&(-1.0f32).to_le_bytes());
    msg.extend_from_slice(&(0.85f32).to_le_bytes());

    // Timing
    msg.extend_from_slice(&(0.5f32).to_le_bytes());  // fade in
    msg.extend_from_slice(&(0.5f32).to_le_bytes());  // fade out
    msg.extend_from_slice(&(3.0f32).to_le_bytes());  // hold time
    msg.extend_from_slice(&(0.0f32).to_le_bytes());  // FX time

    // Null-terminated text payload
    msg.extend_from_slice(text_bytes);
    msg.push(0x00);

    msg
}

/// Build a GoldSrc `svc_director` (OpCode 0x33) net-message for a
/// `DRC_CMD_STUFFTEXT` (sub-command 0x0A) executable command.
///
/// The engine executes `command` on the client console when the event fires
/// in the `viewdemo` event list. The returned `Vec<u8>` is a self-contained
/// net-message body ready to embed inside a `Dem_NetworkBuffer` frame.
///
/// Wire layout:
///
/// | Offset | Size | Value        | Meaning              |
/// |--------|------|--------------|----------------------|
/// | 0      | 1    | 0x33         | svc_director opcode  |
/// | 1      | 1    | payload_len  | 1 + text_len + 1     |
/// | 2      | 1    | 0x0A         | DRC_CMD_STUFFTEXT    |
/// | 3      | N    | command      | raw command string   |
/// | 3+N    | 1    | 0x00         | null terminator      |
///
/// Maximum `command` length is 253 bytes (keeps `payload_len` ≤ 255).
pub fn build_director_stufftext(command: &str) -> Vec<u8> {
    const MAX_TEXT_BYTES: usize = 253; // keeps payload_len <= 255

    let raw      = command.as_bytes();
    let text_len = raw.len().min(MAX_TEXT_BYTES);
    let text_bytes = &raw[..text_len];

    // payload_len = sub-command byte (1) + text + NUL
    let payload_len: u8 = (1 + text_len + 1) as u8;

    let mut msg: Vec<u8> = Vec::with_capacity(2 + 1 + text_len + 1);
    msg.push(0x33);           // svc_director
    msg.push(payload_len);
    msg.push(0x0A);           // DRC_CMD_STUFFTEXT
    msg.extend_from_slice(text_bytes);
    msg.push(0x00);           // null terminator
    msg
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playdemo_safe_stem_leaves_short_alphanumeric_names_alone() {
        assert_eq!(playdemo_safe_stem("demo1"), "demo1");
    }

    #[test]
    fn playdemo_safe_stem_replaces_hyphens_that_truncate_the_goldsrc_cmdline() {
        // Confirmed live: GoldSrc's startup command-line tokenizer treats an
        // embedded "-" as the start of a new launch parm, so this exact stem
        // loaded as bare "wsod25" via +viewdemo instead of the real demo.
        let stem = playdemo_safe_stem("wsod25-po_r3_sf-warchyld_ih_m2_thunder_h1");
        assert!(!stem.contains('-'), "sanitized stem must not contain a hyphen: {stem}");
        assert!(stem.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    }

    #[test]
    fn playdemo_safe_stem_stays_under_the_documented_length_limit() {
        // docs/goldsrc_dod_quirks.md: playdemo/viewdemo targets must stay
        // under ~40 characters. This name alone is 41 before "_preview".
        let stem = playdemo_safe_stem("wsod25-po_r3_sf-warchyld_ih_m2_thunder_h1");
        assert!(
            stem.len() + "_preview".len() < 40,
            "stem + _preview suffix must stay under the limit: {stem}"
        );
    }

    #[test]
    fn playdemo_safe_stem_disambiguates_names_sharing_a_long_prefix() {
        let a = playdemo_safe_stem("a_very_long_shared_prefix_that_overflows_team1");
        let b = playdemo_safe_stem("a_very_long_shared_prefix_that_overflows_team2");
        assert_ne!(a, b, "two different overflowing names must not collide: {a} vs {b}");
    }

    #[test]
    fn test_build_batch_queue_merging() {
        let mut config = PatcherConfig::default(); // pre = 200, post = 60
        let temp_game_path = std::env::temp_dir().join("dod_test_mock");
        std::fs::create_dir_all(temp_game_path.join("dod")).expect("Failed to create dummy dod dir");
        config.game_path = temp_game_path.to_string_lossy().to_string();
        config.primary_media_dir = Some(temp_game_path.clone());
        let raw_streaks = vec![
            CaptureStreak {
                start_tick: 1000,
                end_tick: 1200,
                source_demo: "demo1.dem".to_string(),
                target_player: None,
                kill_count: 3,
                timeline_string: String::new(),
                duration_string: String::new(),
                player_index: 0,
                kills: Vec::new(),
                start_index: 0,
                end_index: 2,
                total_demo_frames: 3000,
                demo_fps: 100.0,
                viewdemo_times: Vec::new(),
                frame_times: std::sync::Arc::new(Vec::new()),
                match_start_tick: None,
                status: Default::default(),
            },
            CaptureStreak {
                start_tick: 1300,
                end_tick: 1500,
                source_demo: "demo1.dem".to_string(),
                target_player: None,
                kill_count: 3,
                timeline_string: String::new(),
                duration_string: String::new(),
                player_index: 0,
                kills: Vec::new(),
                start_index: 0,
                end_index: 2,
                total_demo_frames: 3000,
                demo_fps: 100.0,
                viewdemo_times: Vec::new(),
                frame_times: std::sync::Arc::new(Vec::new()),
                match_start_tick: None,
                status: Default::default(),
            },
            CaptureStreak {
                start_tick: 2000,
                end_tick: 2200,
                source_demo: "demo1.dem".to_string(),
                target_player: None,
                kill_count: 3,
                timeline_string: String::new(),
                duration_string: String::new(),
                player_index: 0,
                kills: Vec::new(),
                start_index: 0,
                end_index: 2,
                total_demo_frames: 3000,
                demo_fps: 100.0,
                viewdemo_times: Vec::new(),
                frame_times: std::sync::Arc::new(Vec::new()),
                match_start_tick: None,
                status: Default::default(),
            },
        ];

        let (jobs, _) = build_batch_queue(raw_streaks, &config, &std::collections::HashMap::new()).unwrap();
        assert_eq!(jobs.len(), 2);

        // Every patched demo lands directly in dod/ now -- see issue #8.
        let expected_dod_dir = std::path::Path::new(&config.game_path).parent().unwrap().join("dod");

        let primer = &jobs[0];
        assert_eq!(primer.output_demo, expected_dod_dir.join("primer.dem"));
        assert_eq!(primer.streaks.len(), 0);

        let job = &jobs[1];
        assert_eq!(job.source_demo, "demo1.dem");
        assert_eq!(job.output_demo, expected_dod_dir.join("chain_01.dem"));
        assert_eq!(job.streaks.len(), 2);
        assert_eq!(job.streaks[0].start_tick, 1000);
        assert_eq!(job.streaks[0].end_tick, 1500); // Merged 1000-1200 and 1300-1500
        assert_eq!(job.streaks[1].start_tick, 2000);
        assert_eq!(job.streaks[1].end_tick, 2200);

        // The merge above collapsed raw streaks 0 and 1 into one recording
        // block — the manifest has to say so, or a finished take can't be
        // traced back to every highlight it actually covers.
        assert_eq!(job.blocks.len(), 2, "one block per merged recording");
        assert_eq!(job.blocks[0].block_index, 0);
        assert_eq!(job.blocks[0].source_streak_indices, vec![0, 1]);
        assert_eq!(job.blocks[1].block_index, 1);
        assert_eq!(job.blocks[1].source_streak_indices, vec![2]);

        // Block bounds mirror the merged streak, and the take folder/key follow
        // the naming the helper cfg's _route_N alias writes to.
        assert_eq!(job.blocks[0].start_tick, 1000);
        assert_eq!(job.blocks[0].end_tick, 1500);
        assert_eq!(job.blocks[0].demo_name, "chain_01");
        assert!(
            job.blocks[0].take_folder.ends_with("chain_01_b0"),
            "expected take folder to end with chain_01_b0, got {:?}",
            job.blocks[0].take_folder
        );
        assert!(
            job.blocks[0].take_key.ends_with("/chain_01_b0"),
            "expected take key to end with /chain_01_b0, got {:?}",
            job.blocks[0].take_key
        );

        // The primer never records anything, so it must carry no blocks.
        assert!(primer.blocks.is_empty());
    }

    #[test]
    fn test_drive_headroom_omits_drive_0_when_no_block_is_routed_there() {
        // Patched demo files land directly in dod/ now (see issue #8), not
        // on any capture_directories entry, so drive 0 no longer needs a
        // special unconditional include -- it should be omitted from
        // drive_headroom exactly like any other drive that received no
        // real recording block.
        let mut config = mock_config();
        let temp_drive = std::env::temp_dir().join("dod_test_headroom_drive0_no_block");
        std::fs::create_dir_all(&temp_drive).expect("failed to create dummy capture drive");
        config.capture_directories = vec![temp_drive.clone()];

        let (jobs, drive_headroom) =
            build_batch_queue(Vec::new(), &config, &std::collections::HashMap::new()).unwrap();

        assert!(jobs.is_empty(), "no streaks should produce no jobs");
        assert!(
            drive_headroom.is_empty(),
            "no block was ever routed to drive 0, so it shouldn't be reported at all, got {:?}",
            drive_headroom
        );
    }

    #[test]
    fn test_drive_headroom_includes_drive_0_when_a_block_actually_lands_there() {
        let mut config = mock_config();
        let temp_drive = std::env::temp_dir().join("dod_test_headroom_drive0_with_block");
        std::fs::create_dir_all(&temp_drive).expect("failed to create dummy capture drive");
        config.capture_directories = vec![temp_drive.clone()];
        let raw_streaks = vec![streak_with_kills(1000, 1200, &[1000, 1200])];

        let (_jobs, drive_headroom) =
            build_batch_queue(raw_streaks, &config, &std::collections::HashMap::new()).unwrap();

        assert_eq!(drive_headroom.len(), 1, "the one real block routes to drive 0, so it should be reported");
        assert_eq!(drive_headroom[0].path, temp_drive);
        assert!(
            drive_headroom[0].free_bytes > 0 && drive_headroom[0].free_bytes < u64::MAX,
            "expected a real free-byte count for an existing directory, got {}",
            drive_headroom[0].free_bytes
        );
    }

    #[test]
    fn test_patched_demos_land_directly_in_dod_not_on_a_capture_directory() {
        // See issue #8: capture_directories is only about where recorded
        // video blocks land now, never these small demo files -- both the
        // primer and every chained job's output_demo must resolve under
        // the game's own dod/ folder (derived from game_path), regardless
        // of what capture_directories is configured to.
        let mut config = mock_config();
        config.capture_directories = vec![std::env::temp_dir().join("dod_test_unused_capture_dir")];
        let raw_streaks = vec![streak_with_kills(1000, 1200, &[1000, 1200])];

        let (jobs, _drive_headroom) =
            build_batch_queue(raw_streaks, &config, &std::collections::HashMap::new()).unwrap();

        let expected_dod_dir = std::path::Path::new(&config.game_path)
            .parent()
            .unwrap()
            .join("dod");
        for job in &jobs {
            assert_eq!(
                job.output_demo.parent(),
                Some(expected_dod_dir.as_path()),
                "expected {:?} to land in dod/, not a capture_directories entry",
                job.output_demo
            );
        }
    }

    #[test]
    fn test_drive_headroom_omits_unconfigured_drives_when_capture_directories_is_empty() {
        // No capture directories configured at all -> allocate_blocks_first_fit_decreasing
        // runs against the single u64::MAX sentinel drive (index 0), but
        // there's no real path at config.capture_directories[0] to report a
        // headroom entry for, so the returned vector must come back empty
        // rather than panicking on the out-of-bounds lookup.
        let config = mock_config(); // capture_directories left at its Vec::new() default
        let raw_streaks = vec![streak_with_kills(1000, 1200, &[1000, 1200])];

        let (jobs, drive_headroom) =
            build_batch_queue(raw_streaks, &config, &std::collections::HashMap::new()).unwrap();

        assert_eq!(jobs.len(), 2, "primer + one chained job");
        assert!(
            drive_headroom.is_empty(),
            "no configured capture directories means nothing to report headroom for, got {:?}",
            drive_headroom
        );
    }

    #[test]
    fn workspace_guard_drop_actually_removes_chain_demos_when_auto_clear_is_on() {
        // Verifies the fix for issue #12: the cleanup filter matched
        // "dodtools_chain_*.dem", but build_batch_queue names output demos
        // "chain_NN.dem" (no prefix) -- see `demo_name` above -- so the
        // filter never matched and these files were never cleaned up
        // regardless of the auto_clear_temp_demos setting.
        let exit_trigger = std::env::temp_dir().join("dod_test_workspace_guard_exit_trigger");
        let dod_dir = exit_trigger.parent().unwrap().join("dod");
        std::fs::create_dir_all(&dod_dir).unwrap();
        std::fs::create_dir_all(&exit_trigger).unwrap();
        let chain_demo = dod_dir.join("chain_01.dem");
        std::fs::write(&chain_demo, b"fake demo bytes").unwrap();

        {
            let _guard = WorkspaceGuard {
                session_junction: std::env::temp_dir().join("dod_test_workspace_guard_session_junction_nonexistent"),
                exit_trigger: exit_trigger.clone(),
                pool_junctions: Vec::new(),
                route_junctions: Vec::new(),
                auto_clear_logs: false,
                auto_clear_temp_demos: true,
                auto_clear_previews: false,
                save_local_patched_copy: false,
            };
        }

        assert!(
            !chain_demo.exists(),
            "chain_01.dem should have been removed by WorkspaceGuard::drop with auto_clear_temp_demos on"
        );

        let _ = std::fs::remove_dir_all(&dod_dir);
    }

    fn streak_with_kills(start_tick: i32, end_tick: i32, kill_frames: &[i32]) -> CaptureStreak {
        CaptureStreak {
            start_tick,
            end_tick,
            source_demo: "demo1.dem".to_string(),
            target_player: None,
            kill_count: kill_frames.len(),
            timeline_string: String::new(),
            duration_string: String::new(),
            player_index: 0,
            kills: kill_frames.iter().map(|&f| (f, f as f32 / 100.0, "k98".to_string())).collect(),
            start_index: 0,
            end_index: kill_frames.len().saturating_sub(1),
            total_demo_frames: 30000,
            demo_fps: 100.0,
            viewdemo_times: Vec::new(),
            frame_times: std::sync::Arc::new(Vec::new()),
            match_start_tick: None,
            status: Default::default(),
        }
    }

    /// A game folder of this test's own.
    ///
    /// One folder per call, because `build_batch_queue` writes its helper and
    /// chain `.cfg` files into the game folder and deletes the previous run's
    /// on the way in. A shared path meant tests running in parallel deleted
    /// each other's files mid-write, which surfaces as a sharing violation on
    /// Windows and an `Err` out of a call every test `unwrap`s.
    ///
    /// `game_path` names the executable, not the folder — the engine's content
    /// sits beside `hl.exe` and every caller reads `game_path.parent()`. Naming
    /// the folder here put `dod_dir` one level too high, at the temp root, so
    /// the tests were writing to a single shared `%TEMP%/dod` regardless.
    /// Named after the running test, which the harness puts on the thread. That
    /// is unique per test and stable across runs, so folders are isolated
    /// without a counter that would leave a fresh one behind every run.
    fn mock_config() -> PatcherConfig {
        let tag = std::thread::current()
            .name()
            .map(|n| n.replace(|c: char| !c.is_ascii_alphanumeric(), "_"))
            .unwrap_or_else(|| "mock".to_string());
        mock_config_in(&tag)
    }

    fn mock_config_in(tag: &str) -> PatcherConfig {
        let root = std::env::temp_dir().join(format!("dod_test_{}", tag));
        std::fs::create_dir_all(root.join("dod")).expect("Failed to create dummy dod dir");
        let mut config = PatcherConfig::default();
        config.game_path = root.join("hl.exe").to_string_lossy().to_string();
        config.primary_media_dir = Some(root);
        config
    }

    #[test]
    fn obs_mode_pins_fps_override_and_fps_max_to_obs_capture_fps() {
        let mut config = PatcherConfig::default();
        config.capture_mode = crate::patch::CaptureMode::Obs;
        config.capture_fps = 300; // must not leak into fps_max — see below
        config.obs_capture_fps = 120;
        let commands = final_init_commands(&config);
        assert!(
            commands.iter().any(|c| c == "fps_override 1"),
            "expected fps_override 1, got: {commands:?}"
        );
        assert!(
            commands.iter().any(|c| c == "fps_max 120"),
            "expected fps_max 120 (from obs_capture_fps, not capture_fps), got: {commands:?}"
        );
    }

    #[test]
    fn frame_sequence_and_direct_to_video_never_touch_fps_max() {
        for mode in [crate::patch::CaptureMode::FrameSequence, crate::patch::CaptureMode::DirectToVideo] {
            let mut config = PatcherConfig::default();
            config.capture_mode = mode;
            config.capture_fps = 120;
            config.obs_capture_fps = 120;
            let commands = final_init_commands(&config);
            assert!(
                !commands.iter().any(|c| c.starts_with("fps_override") || c.starts_with("fps_max")),
                "{mode:?} should not touch fps_override/fps_max, got: {commands:?}"
            );
        }
    }

    #[test]
    fn the_pre_roll_floor_names_whichever_requirement_is_binding() {
        // With nothing else configured the audio resync sets the bar, since it
        // is the longest of the fixed terms.
        let mut config = mock_config();
        config.custom_commands.clear();
        let f = roll_floors(&config);
        assert_eq!(f.pre_roll, AUDIO_RESYNC_SECONDS);
        assert!(f.pre_roll_binding.contains("audio"), "{}", f.pre_roll_binding);
        assert_eq!(f.post_roll, 0.0, "nothing needs post-roll on its own");

        // A Scheduled Command further out than that takes over, because
        // anything beyond the real-time window fires during fast-forward.
        config.custom_commands = vec![crate::patch::CustomCommand {
            command: "mirv_movie_fps 500".to_string(),
            offset: 8.0,
            relation: CommandRelation::Before,
        }];
        let f = roll_floors(&config);
        assert_eq!(f.pre_roll, 8.0);
        assert!(f.pre_roll_binding.contains("Scheduled"), "{}", f.pre_roll_binding);

        // The start lead covers part of that distance, because the offset
        // anchors to the kill and recording starts a lead before it. The real
        // configuration this got wrong: a 10s command with a 5s start lead
        // needs only 5s of pre-roll, and warning at 10 was a false alarm.
        config.record_start_lead = 5.0;
        config.custom_commands[0].offset = 10.0;
        let f = roll_floors(&config);
        assert_eq!(f.scheduled_before, 5.0, "10s out, 5s of it covered by the lead");
        assert_eq!(f.pre_roll, AUDIO_RESYNC_SECONDS.max(5.0));

        // And a lead longer than the offset leaves nothing for the pre-roll.
        config.record_start_lead = 12.0;
        let f = roll_floors(&config);
        assert_eq!(f.scheduled_before, 0.0, "never negative");

        // And an "After" command is the only thing that asks for post-roll.
        config.custom_commands = vec![crate::patch::CustomCommand {
            command: "echo done".to_string(),
            offset: 3.0,
            relation: CommandRelation::After,
        }];
        let f = roll_floors(&config);
        assert_eq!(f.post_roll, 3.0);
        assert_eq!(f.pre_roll, AUDIO_RESYNC_SECONDS, "an After command asks nothing of pre-roll");
    }

    #[test]
    fn turning_the_flush_off_drops_its_term_from_the_floor() {
        let mut config = mock_config();
        config.custom_commands.clear();

        config.decal_flush = true;
        assert_eq!(roll_floors(&config).flush_lead, crate::patch::DEFAULT_LEAD_SECONDS);

        config.decal_flush = false;
        assert_eq!(roll_floors(&config).flush_lead, 0.0);
    }

    #[test]
    fn a_command_outside_the_real_time_window_is_flagged() {
        // Real time runs from the speed drop (pre-roll) to the end of the
        // post-roll. Either side of that the engine is at host_framerate 0.05.
        let (speed_drop, post_roll_end) = (1000, 2000);

        assert!(runs_during_fast_forward(999, speed_drop, post_roll_end), "before the speed drop");
        assert!(runs_during_fast_forward(2001, speed_drop, post_roll_end), "after the post-roll");
        assert!(!runs_during_fast_forward(1000, speed_drop, post_roll_end), "the drop itself");
        assert!(!runs_during_fast_forward(1500, speed_drop, post_roll_end), "mid-clip");
        assert!(!runs_during_fast_forward(2000, speed_drop, post_roll_end), "the last post-roll tick");
    }

    #[test]
    fn test_merged_block_records_through_the_absorbed_highlights_last_kill() {
        // Two highlights close enough to merge into one continuous recording.
        // The merge keeps the first streak's fields and only extends end_tick —
        // but the record-stop mark is derived from kills[end_index], so unless
        // the absorbed streak's final kill is carried over, recording stops at
        // the FIRST highlight's last kill and the merged-in one never lands in
        // the take at all (while still being reported as one captured block).
        let config = mock_config();
        let raw_streaks = vec![
            streak_with_kills(1000, 1200, &[1000, 1200]),
            streak_with_kills(1300, 1500, &[1300, 1500]),
        ];

        let (jobs, _) = build_batch_queue(raw_streaks, &config, &std::collections::HashMap::new()).unwrap();
        let job = &jobs[1];

        assert_eq!(job.streaks.len(), 1, "the two highlights should merge into one block");
        assert_eq!(job.blocks[0].source_streak_indices, vec![0, 1]);

        let record_stop = job
            .scheduled_commands
            .iter()
            .find(|(_, cmd)| cmd == "sys_record_stop")
            .map(|(tick, _)| *tick)
            .expect("merged block must schedule a record stop");

        assert!(
            record_stop >= 1500,
            "recording stopped at tick {} — before the merged-in highlight's last kill at 1500, \
             so that highlight was never actually captured",
            record_stop
        );
    }

    #[test]
    fn test_demo_start_marker_carries_job_idx_total_and_clip_count() {
        // Two demos, one block each -> two real jobs (plus the primer at
        // jobs[0], which carries no DEMO_START of its own — see issue #98).
        let config = mock_config();
        let mut raw_streaks = vec![streak_with_kills(1000, 1200, &[1000, 1200])];
        let mut second_demo = streak_with_kills(1000, 1200, &[1000, 1200]);
        second_demo.source_demo = "demo2.dem".to_string();
        raw_streaks.push(second_demo);

        let (jobs, _) = build_batch_queue(raw_streaks, &config, &std::collections::HashMap::new()).unwrap();
        assert_eq!(jobs.len(), 3, "primer + one job per demo");
        assert!(
            jobs[0].scheduled_commands.iter().all(|(_, c)| !c.contains("DEMO_START")),
            "the primer plays no real demo and must not announce one"
        );

        for (job, expected_idx) in [(&jobs[1], 1), (&jobs[2], 2)] {
            let echo = job
                .scheduled_commands
                .iter()
                .find(|(_, c)| c.contains("DEMO_START"))
                .map(|(_, c)| c.clone())
                .unwrap_or_else(|| panic!("job {:?} has no DEMO_START echo", job.output_demo));
            assert!(
                echo.contains(&format!("DEMO_START {} 2 1", expected_idx)),
                "expected job idx {} of 2, 1 clip in {:?}",
                expected_idx,
                echo
            );
        }
    }

    #[test]
    fn test_next_clip_marker_announces_clip_1_and_each_later_non_chained_clip() {
        // Two demos, two non-overlapping (non-chained) clips each -> NEXT_CLIP
        // for clip 1 alongside DEMO_START, NEXT_CLIP for clip 2 alongside the
        // Stage-4 FAST_FORWARD, and nothing after the last clip in the demo.
        let config = mock_config();
        let mut raw_streaks = vec![
            streak_with_kills(1000, 1200, &[1000, 1200]),
            streak_with_kills(5000, 5200, &[5000, 5200]),
        ];
        let mut second_demo = streak_with_kills(1000, 1200, &[1000, 1200]);
        second_demo.source_demo = "demo2.dem".to_string();
        let mut second_demo_b = streak_with_kills(5000, 5200, &[5000, 5200]);
        second_demo_b.source_demo = "demo2.dem".to_string();
        raw_streaks.push(second_demo);
        raw_streaks.push(second_demo_b);

        let (jobs, _) = build_batch_queue(raw_streaks, &config, &std::collections::HashMap::new()).unwrap();
        assert_eq!(jobs.len(), 3, "primer + one job per demo");

        for (job, expected_job_idx) in [(&jobs[1], 1), (&jobs[2], 2)] {
            let next_clip_echoes: Vec<&String> = job
                .scheduled_commands
                .iter()
                .filter(|(_, c)| c.contains("NEXT_CLIP"))
                .map(|(_, c)| c)
                .collect();
            assert_eq!(
                next_clip_echoes.len(), 2,
                "expected one NEXT_CLIP for clip 1 and one for clip 2, got {:?}",
                next_clip_echoes
            );
            assert!(
                next_clip_echoes.iter().any(|c| c.contains(&format!("NEXT_CLIP {} 2 1 2", expected_job_idx))),
                "missing clip-1-of-2 NEXT_CLIP for job {} in {:?}",
                expected_job_idx,
                next_clip_echoes
            );
            assert!(
                next_clip_echoes.iter().any(|c| c.contains(&format!("NEXT_CLIP {} 2 2 2", expected_job_idx))),
                "missing clip-2-of-2 NEXT_CLIP for job {} in {:?}",
                expected_job_idx,
                next_clip_echoes
            );
        }
    }

    /// Ticks at which a given command is scheduled, in order.
    fn ticks_for(job: &PatchJob, cmd: &str) -> Vec<i32> {
        job.scheduled_commands
            .iter()
            .filter(|(_, c)| c == cmd)
            .map(|(t, _)| *t)
            .collect()
    }

    #[test]
    fn test_non_overlapping_recordings_stay_separate_takes() {
        // 40s apart at 100fps — far outside every window. Two independent
        // takes, each with its own fast-forward round trip.
        let config = mock_config();
        let raw_streaks = vec![
            streak_with_kills(1000, 1200, &[1000, 1200]),
            streak_with_kills(5000, 5200, &[5000, 5200]),
        ];

        let (jobs, _) = build_batch_queue(raw_streaks, &config, &std::collections::HashMap::new()).unwrap();
        let job = &jobs[1];

        assert_eq!(job.streaks.len(), 2, "should not merge");
        assert_eq!(job.blocks.len(), 2);
        assert_eq!(ticks_for(job, "sys_record_start").len(), 2);
        assert_eq!(ticks_for(job, "sys_record_stop").len(), 2);
        // Each block drops out of fast-forward for itself, and the first block
        // resumes it afterwards.
        assert_eq!(ticks_for(job, "sys_normal_speed").len(), 6, "3-frame redundancy per block");
        assert_eq!(ticks_for(job, "sys_sound").len(), 2);
    }

    #[test]
    fn test_blocks_carry_the_record_bounds_the_decal_flush_keys_off() {
        // A block's start_tick/end_tick are the highlight's own bounds — not
        // the frames HLAE records between, which are computed separately in the
        // scheduling loop. The decal flush strips every decal outside a block's
        // recorded frames, so bounds that came back wrong (or as 0) would scrub
        // the very clip the pass exists to protect.
        let config = mock_config();
        let raw_streaks = vec![
            streak_with_kills(1000, 1200, &[1000, 1200]),
            streak_with_kills(5000, 5200, &[5000, 5200]),
        ];

        let (jobs, _) = build_batch_queue(raw_streaks, &config, &std::collections::HashMap::new()).unwrap();
        let job = &jobs[1];

        assert_eq!(job.blocks.len(), 2);

        // scheduled_commands is tick-sorted and the blocks don't overlap, so
        // the Nth start pairs with the Nth stop.
        let mut expected: Vec<(i32, i32)> = ticks_for(job, "sys_record_start")
            .into_iter()
            .zip(ticks_for(job, "sys_record_stop"))
            .collect();
        expected.sort_unstable();

        let mut reported: Vec<(i32, i32)> = job
            .blocks
            .iter()
            .map(|b| (b.record_start_tick, b.record_stop_tick))
            .collect();
        reported.sort_unstable();

        assert_eq!(
            reported, expected,
            "blocks must report the same frames the capture actually records between"
        );
        for b in &job.blocks {
            assert!(
                b.record_start_tick > 0 && b.record_stop_tick >= b.record_start_tick,
                "block {} has an unusable record window {}..{}",
                b.block_index, b.record_start_tick, b.record_stop_tick
            );
        }
    }

    #[test]
    fn test_decal_flush_pins_the_ring_once_at_demo_load_and_never_again() {
        // r_decals bounds how far the rotating decal index may travel before it
        // wraps; it evicts nothing. Setting it a second time, lower, strands
        // every decal above the new limit permanently — so exactly one command
        // may own it, and it has to land at demo load.
        let mut config = mock_config();
        config.decal_ring_limit = 128;

        let (jobs, _) = build_batch_queue(
            vec![streak_with_kills(1000, 1200, &[1000, 1200])],
            &config,
            &std::collections::HashMap::new(),
        ).unwrap();
        let job = &jobs[1];

        assert_eq!(
            job.init_commands.iter().filter(|c| c.starts_with("r_decals")).count(),
            1,
            "exactly one command may own the ring: {:?}",
            job.init_commands
        );
        assert_eq!(
            job.init_commands.last().map(String::as_str),
            Some("r_decals 128"),
            "and it has to land at demo load: {:?}",
            job.init_commands
        );
        assert!(
            !job.scheduled_commands.iter().any(|(_, c)| c.starts_with("r_decals")),
            "r_decals must never be touched mid-demo — that is what strands decals"
        );
    }

    #[test]
    fn test_an_init_command_owns_the_ring_and_is_not_pinned_over() {
        // The sweep is sized to the ring, so the two are one number and
        // r_decals is where the engine reads it. Appending a second value could
        // only overrule what was asked for, with nothing on screen to show it:
        // the capture would run a ring the user did not choose, swept correctly
        // for a size they did not ask for.
        let mut config = mock_config();
        config.decal_ring_limit = 128;
        config.init_commands = vec!["r_decals 512".to_string()];

        let (jobs, _) = build_batch_queue(
            vec![streak_with_kills(1000, 1200, &[1000, 1200])],
            &config,
            &std::collections::HashMap::new(),
        ).unwrap();

        let pins: Vec<&String> = jobs[1]
            .init_commands
            .iter()
            .filter(|c| c.starts_with("r_decals"))
            .collect();
        assert_eq!(
            pins,
            vec![&"r_decals 512".to_string()],
            "the user's own line is the pin: {:?}",
            jobs[1].init_commands
        );
    }

    #[test]
    fn test_decals_switched_off_entirely_is_left_alone() {
        // r_decals 0 means no decals at all. There is no ring to turn and
        // nothing to clear, so the flush has no work and no business pinning
        // the cvar back up to a value that would start collecting them.
        let mut config = mock_config();
        config.init_commands = vec!["r_decals 0".to_string()];

        let (jobs, _) = build_batch_queue(
            vec![streak_with_kills(1000, 1200, &[1000, 1200])],
            &config,
            &std::collections::HashMap::new(),
        ).unwrap();

        assert_eq!(
            jobs[1].init_commands.iter().filter(|c| c.starts_with("r_decals")).count(),
            1,
            "the user's r_decals 0 must survive untouched: {:?}",
            jobs[1].init_commands
        );
    }

    #[test]
    fn test_decal_flush_disabled_leaves_r_decals_untouched() {
        let mut config = mock_config();
        config.decal_flush = false;

        let (jobs, _) = build_batch_queue(
            vec![streak_with_kills(1000, 1200, &[1000, 1200])],
            &config,
            &std::collections::HashMap::new(),
        ).unwrap();

        assert!(
            !jobs[1].init_commands.iter().any(|c| c.starts_with("r_decals")),
            "with the flush off the pipeline must not touch the cvar at all"
        );
    }

    #[test]
    fn test_a_maximum_ring_sweep_stops_pinning_the_cvar() {
        // r_decals is clamped to MAX_RENDER_DECALS, so a sweep that size turns
        // a full revolution whatever the cvar is. Pinning then buys nothing and
        // costs the precondition the rest of the design works around — that
        // nothing else may set r_decals.
        let mut config = mock_config();
        config.decal_ring_limit = crate::patch::MAX_RENDER_DECALS;

        let (jobs, _) = build_batch_queue(
            vec![streak_with_kills(1000, 1200, &[1000, 1200])],
            &config,
            &std::collections::HashMap::new(),
        ).unwrap();

        assert!(
            !jobs[1].init_commands.iter().any(|c| c.starts_with("r_decals")),
            "a maximum sweep must leave the cvar alone: {:?}",
            jobs[1].init_commands
        );
    }

    /// As `mock_config`, with a `config.cfg` in the game folder so the config
    /// scan sees exactly what the test put there and nothing else.
    ///
    /// The tag is fixed rather than counted: scans are cached per folder, so a
    /// case has to name its own folder to be sure of what it is reading.
    fn mock_config_with_game_cfg(tag: &str, cfg_body: &str) -> PatcherConfig {
        let config = mock_config_in(&format!("ring_{}", tag));
        let cfg = std::path::Path::new(&config.game_path)
            .parent()
            .expect("game folder")
            .join("dod")
            .join("config.cfg");
        if cfg_body.is_empty() {
            let _ = std::fs::remove_file(&cfg);
        } else {
            std::fs::write(&cfg, cfg_body).expect("config.cfg");
        }
        config
    }

    #[test]
    fn test_a_game_config_setting_the_cvar_to_zero_is_now_left_alone() {
        // Regression: this used to assert the opposite — that a config's
        // `r_decals 0` got silently overridden with a pin (to MAX_RENDER_DECALS
        // in this exact case), on the reasoning that adopting it would leave a
        // ring the engine keeps nothing in. That override is gone: r_decals now
        // follows the same config precedence mirv_fov always has, and a
        // resulting 0 is caught and reported as its own fact
        // (`decal_flush_is_noop` / `prepare_flushed_source`'s own zero-check)
        // rather than silently patched away.
        let mut config = mock_config_with_game_cfg("zero", "r_decals 0\nfps_max 999\n");
        config.decal_ring_limit = crate::patch::MAX_RENDER_DECALS;

        let (jobs, _) = build_batch_queue(
            vec![streak_with_kills(1000, 1200, &[1000, 1200])],
            &config,
            &std::collections::HashMap::new(),
        ).unwrap();

        assert!(
            !jobs[1].init_commands.iter().any(|c| c.starts_with("r_decals")),
            "the config's own 0 already stands, nothing left to pin: {:?}",
            jobs[1].init_commands
        );
        assert_eq!(crate::patch::ring_limit(&config), 0, "and ring_limit agrees");
    }

    #[test]
    fn test_a_nonzero_game_config_value_is_adopted_without_a_pin() {
        // The config's own line already achieves what a pin would — appending
        // a second, identical-in-effect command would be pure noise.
        let config = mock_config_with_game_cfg("nonzero", "r_decals 512\n");
        // decal_ring_limit stays at its default (256): if the config's value
        // were being ignored in favor of it, the pin would read 256, not 512.

        let (jobs, _) = build_batch_queue(
            vec![streak_with_kills(1000, 1200, &[1000, 1200])],
            &config,
            &std::collections::HashMap::new(),
        ).unwrap();

        assert!(
            !jobs[1].init_commands.iter().any(|c| c.starts_with("r_decals")),
            "the config's own line is already the pin, nothing to append: {:?}",
            jobs[1].init_commands
        );
        assert_eq!(crate::patch::ring_limit(&config), 512);
    }

    #[test]
    fn test_a_maximum_ring_leaves_the_cvar_alone_when_no_config_touches_it() {
        // The other half of the rule, against a game folder proven empty rather
        // than one that merely happens to be.
        let mut config = mock_config_with_game_cfg("clean", "");
        config.decal_ring_limit = crate::patch::MAX_RENDER_DECALS;

        let (jobs, _) = build_batch_queue(
            vec![streak_with_kills(1000, 1200, &[1000, 1200])],
            &config,
            &std::collections::HashMap::new(),
        ).unwrap();

        assert!(
            !jobs[1].init_commands.iter().any(|c| c.starts_with("r_decals")),
            "nothing else sets the cvar, so the pin buys nothing: {:?}",
            jobs[1].init_commands
        );
    }

    #[test]
    fn test_a_config_never_overrides_what_init_commands_state() {
        // The config only decides whether the pin is worth spending. It is
        // never adopted as a value — `init_commands` stays the authority, and a
        // stated maximum is still the user's own line and still the pin.
        let mut config = mock_config_with_game_cfg("stated", "r_decals 0\n");
        config.decal_ring_limit = 256;
        config.init_commands = vec!["r_decals 4096".to_string()];

        let (jobs, _) = build_batch_queue(
            vec![streak_with_kills(1000, 1200, &[1000, 1200])],
            &config,
            &std::collections::HashMap::new(),
        ).unwrap();

        let pins: Vec<&String> = jobs[1]
            .init_commands
            .iter()
            .filter(|c| c.starts_with("r_decals"))
            .collect();
        assert_eq!(
            pins,
            vec![&"r_decals 4096".to_string()],
            "exactly the stated line, appended once"
        );
    }

    #[test]
    fn test_anything_below_the_maximum_still_pins() {
        // Below the ceiling the sweep only covers the ring it was sized for, so
        // the cvar has to be held there or the sweep under-clears.
        let mut config = mock_config();
        config.decal_ring_limit = crate::patch::MAX_RENDER_DECALS - 1;

        let (jobs, _) = build_batch_queue(
            vec![streak_with_kills(1000, 1200, &[1000, 1200])],
            &config,
            &std::collections::HashMap::new(),
        ).unwrap();

        assert_eq!(
            jobs[1].init_commands.last().map(String::as_str),
            Some("r_decals 4095")
        );
    }

    #[test]
    fn test_colliding_rolls_keep_separate_takes_without_fast_forwarding_between() {
        // Recordings are 2s apart (200 ticks @ 100fps): clear of the 1s
        // separation guard so they stay two takes, but inside the 2.6s the
        // pre-roll and post-roll need, so the fast-forward round trip between
        // them can't fit and is dropped instead of collapsing the two
        // highlights into one clip full of dead air.
        let mut config = mock_config();
        config.pre_roll_seconds = 2.0;
        config.post_roll_seconds = 0.6;
        let raw_streaks = vec![
            streak_with_kills(1000, 1200, &[1000, 1200]),
            streak_with_kills(1400, 1600, &[1400, 1600]),
        ];

        let (jobs, _) = build_batch_queue(raw_streaks, &config, &std::collections::HashMap::new()).unwrap();
        let job = &jobs[1];

        assert_eq!(job.streaks.len(), 2, "recordings don't overlap, so don't merge");
        assert_eq!(job.blocks.len(), 2);
        assert_eq!(job.blocks[0].source_streak_indices, vec![0]);
        assert_eq!(job.blocks[1].source_streak_indices, vec![1]);

        // Both clips still get their own recording.
        assert_eq!(ticks_for(job, "sys_record_start").len(), 2);
        assert_eq!(ticks_for(job, "sys_record_stop").len(), 2);

        // But only the first block exits fast-forward, and nothing re-enters it
        // between the two — playback simply stays at normal speed across the gap.
        assert_eq!(
            ticks_for(job, "sys_normal_speed").len(), 3,
            "only the first block should drop out of fast-forward"
        );
        assert_eq!(
            ticks_for(job, "sys_sound").len(), 1,
            "no fast-forward before the second clip means no audio to flush"
        );
        let fast_forwards = ticks_for(job, "sys_fast_forward");
        let record_stops = ticks_for(job, "sys_record_stop");
        assert!(
            !fast_forwards.iter().any(|&t| t > record_stops[0] && t < record_stops[1]),
            "fast-forward must not be scheduled between the two takes, got {:?}",
            fast_forwards
        );
    }

    #[test]
    fn test_takes_too_close_together_are_merged_rather_than_restarted() {
        // Only 0.5s (50 ticks @ 100fps) between one recording stopping and the
        // next starting — under MIN_TAKE_SEPARATION_SECONDS, so they merge
        // instead of risking a stop/start cycle that tight.
        let config = mock_config();
        let raw_streaks = vec![
            streak_with_kills(1000, 1200, &[1000, 1200]),
            streak_with_kills(1250, 1400, &[1250, 1400]),
        ];

        let (jobs, _) = build_batch_queue(raw_streaks, &config, &std::collections::HashMap::new()).unwrap();
        let job = &jobs[1];

        assert_eq!(job.streaks.len(), 1, "too close to be separate takes");
        assert_eq!(job.blocks[0].source_streak_indices, vec![0, 1]);
        assert_eq!(ticks_for(job, "sys_record_start").len(), 1);
        assert_eq!(ticks_for(job, "sys_record_stop").len(), 1);
    }

    #[test]
    fn test_blocks_merge_only_when_padding_closes_the_gap() {
        // 100-tick gap between blocks. No padding: stays separate.
        assert!(!blocks_merge(1000, 1100, 0, 0));
        // Pre-roll alone reaches back far enough.
        assert!(blocks_merge(1000, 1100, 100, 0));
        // Post-roll alone reaches forward far enough.
        assert!(blocks_merge(1000, 1100, 0, 100));
        // Split across both, summing to exactly the gap — boundary is inclusive.
        assert!(blocks_merge(1000, 1100, 60, 40));
        // One tick short.
        assert!(!blocks_merge(1000, 1100, 60, 39));
    }

    #[test]
    fn test_blocks_merge_clamps_negative_adjusted_start() {
        // A highlight near tick 0 with a large pre-roll must clamp at 0 rather
        // than going negative and merging with something it doesn't touch.
        assert!(blocks_merge(0, 50, 500, 0));
        assert!(!blocks_merge(-100, 50, 500, 0));
    }

    #[test]
    fn test_ffd_allocation_succeeds_where_arrival_order_would_strand_a_block() {
        // Two drives: only drive 0 (capacity 7) is big enough for the size-7
        // block; drive 1 tops out at 6. In arrival order (7 is block 1, not
        // first), naive first-fit would let the size-3 block (block 0) land on
        // drive 0 first, leaving only 4 free there — too little for the size-7
        // block, which drive 1 could never fit either. FFD places the size-7
        // block first (while drive 0 is still empty), so both other blocks can
        // still find room afterward.
        let block_estimates = vec![3u64, 7u64, 2u64];
        let mut drive_free = vec![7u64, 6u64];
        let mut active_drive_idx = 0usize;

        let assignments = allocate_blocks_first_fit_decreasing(
            &block_estimates,
            &mut drive_free,
            &mut active_drive_idx,
            0,
        ).expect("FFD should find a placement that naive arrival-order first-fit would miss");

        assert_eq!(assignments.len(), 3);
        let drive_of = |block: usize| assignments.iter().find(|(b, _)| *b == block).unwrap().1;

        // The size-7 block must land on drive 0 — it's the only drive with room for it.
        assert_eq!(drive_of(1), 0);
        assert_eq!(drive_free[0], 0); // 7 - 7 exactly consumed
        assert_eq!(drive_free[1], 6 - 3 - 2); // blocks 0 and 2 backfilled drive 1
    }

    #[test]
    fn test_ffd_allocation_fails_when_no_drive_has_room() {
        let block_estimates = vec![10u64];
        let mut drive_free = vec![5u64, 5u64];
        let mut active_drive_idx = 0usize;

        let result = allocate_blocks_first_fit_decreasing(
            &block_estimates,
            &mut drive_free,
            &mut active_drive_idx,
            0,
        );

        assert_eq!(result, Err(0));
    }

    /// A regression guard for #198's fix -- `workspace_guard_drop_actually_
    /// removes_chain_demos_when_auto_clear_is_on` above already covers the
    /// enabled case; this is the one combination it doesn't, and the reason
    /// `remove_file_retrying` is called through `auto_clear_temp_demos &&
    /// !save_local_patched_copy` rather than unconditionally.
    #[test]
    fn dropping_the_workspace_guard_keeps_demos_when_a_local_copy_was_requested() {
        let root = std::env::temp_dir().join(format!("dod_workspace_guard_keep_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dod = root.join("dod");
        std::fs::create_dir_all(&dod).unwrap();
        std::fs::write(dod.join("primer.dem"), b"x").unwrap();
        std::fs::write(dod.join("chain_01.dem"), b"x").unwrap();

        {
            let _guard = WorkspaceGuard {
                session_junction: root.join("session_junction"),
                exit_trigger: root.join("exit_trigger"),
                pool_junctions: vec![],
                route_junctions: vec![],
                auto_clear_logs: false,
                auto_clear_temp_demos: true,
                auto_clear_previews: false,
                save_local_patched_copy: true,
            };
        }

        assert!(dod.join("primer.dem").exists());
        assert!(dod.join("chain_01.dem").exists());

        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod builder_grouping_tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_build_batch_queue_grouping() {
        let mut config = PatcherConfig::default();
        let temp_game_path = std::env::temp_dir().join("dod_test_mock");
        std::fs::create_dir_all(temp_game_path.join("dod")).expect("Failed to create dummy dod dir");
        config.game_path = temp_game_path.to_string_lossy().to_string();
        config.primary_media_dir = Some(temp_game_path.clone());

        let streak1 = CaptureStreak {
            start_tick: 100,
            end_tick: 200,
            source_demo: "demo_A.dem".to_string(),
            target_player: Some("Player1".to_string()),
            kill_count: 2,
            timeline_string: String::new(),
            duration_string: String::new(),
            player_index: 1,
            kills: vec![],
            start_index: 0,
            end_index: 0,
            total_demo_frames: 1000,
            demo_fps: 100.0,
            viewdemo_times: vec![],
            frame_times: Arc::new(vec![]),
            status: crate::patch::types::HighlightStatus::None,
            match_start_tick: None,
        };

        let streak2 = CaptureStreak {
            start_tick: 300,
            end_tick: 400,
            source_demo: "demo_A.dem".to_string(),
            target_player: Some("Player1".to_string()),
            kill_count: 2,
            timeline_string: String::new(),
            duration_string: String::new(),
            player_index: 1,
            kills: vec![],
            start_index: 0,
            end_index: 0,
            total_demo_frames: 1000,
            demo_fps: 100.0,
            viewdemo_times: vec![],
            frame_times: Arc::new(vec![]),
            status: crate::patch::types::HighlightStatus::None,
            match_start_tick: None,
        };

        let streak3 = CaptureStreak {
            start_tick: 100,
            end_tick: 200,
            source_demo: "demo_B.dem".to_string(),
            target_player: Some("Player2".to_string()),
            kill_count: 2,
            timeline_string: String::new(),
            duration_string: String::new(),
            player_index: 2,
            kills: vec![],
            start_index: 0,
            end_index: 0,
            total_demo_frames: 1000,
            demo_fps: 100.0,
            viewdemo_times: vec![],
            frame_times: Arc::new(vec![]),
            status: crate::patch::types::HighlightStatus::None,
            match_start_tick: None,
        };

        let raw_streaks = vec![streak1, streak2, streak3];
        let global_arrays = HashMap::new();

        let (jobs, _) = build_batch_queue(raw_streaks, &config, &global_arrays).unwrap();

        assert_eq!(jobs.len(), 3, "Expected exactly 3 patch jobs (1 primer + 2 grouped chains) after grouping by source demo and player");
    }
}
