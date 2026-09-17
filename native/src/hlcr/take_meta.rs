//! What a take was actually captured at, recorded beside it.
//!
//! Render Studio's FPS is a per-job setting the user picks (the VirtualDub-style
//! model), and nothing ever connected it to the FPS the take was recorded at.
//! `-framerate` before the BMP input tells FFmpeg how to interpret the
//! sequence's timing, so a value disagreeing with the capture produces a wrong
//! computed duration and `-shortest` trims the audio against it: a 120fps take
//! rendered at 300 comes out 2.5x too fast, silently, and the render reports
//! success. `docs/engineering_backlog.md` has the full diagnosis — it was found
//! by ear, from a render that "sounds like a helicopter".
//!
//! There was no source of truth to check the render setting against. This is it:
//! one small file per take, written into its block folder after the batch has
//! been verified, recording the `mirv_movie_fps` it was produced at.
//!
//! Per take, not per session, so it travels with the take. Two batches at
//! different rates already land in different session folders, so a session-level
//! file would be correct for that — but only until somebody moves a take, at
//! which point it would inherit whatever the folder it landed in says. That is
//! this exact bug reintroduced one level up.
//!
//! Deliberately **advisory**. Nothing here overrides the user's render setting —
//! a take folder can be moved, copied or hand-assembled, and a renderer that
//! silently substituted a number found in a neighbouring file would be a worse
//! surprise than the one it fixes. It exists so the disagreement can be *stated*.

use std::path::{Path, PathBuf};

/// Bump when the shape changes. A file with an unrecognised format is ignored
/// rather than guessed at — an advisory check that mis-reads is worse than one
/// that stays quiet.
pub const FORMAT: u32 = 1;

/// Lives in the **block** folder — `<capture_dir>/<session>/chain_JJ_bN/` —
/// beside the take it describes, so it travels with the take when the folder is
/// moved or copied.
///
/// The obvious alternative is one file per session folder, and it is worse for
/// exactly the reason it looks tidier: it describes a *location*, not a take.
/// Drag a take into another session's folder and it silently inherits that
/// session's settings, which is the failure this whole feature exists to
/// prevent, reintroduced one level up.
///
/// Putting it in the block folder does collide with `take_folder_has_content`,
/// which decides a take was captured by asking whether that folder is non-empty
/// — so an empty block plus this file would look successful. That is handled by
/// `is_metadata`, which the emptiness check consults, rather than by writing
/// somewhere else and hoping nobody re-runs verification later.
pub const TAKE_FILE: &str = "dodtools_take.json";

/// Whether a directory entry is one of ours, and so must not be mistaken for
/// captured output. `take_folder_has_content` asks this.
pub fn is_metadata(file_name: &std::ffi::OsStr) -> bool {
    file_name.eq_ignore_ascii_case(TAKE_FILE)
}

/// How far up from a take folder to look. HLAE nests its own `take0000` inside
/// the block folder, and callers hand out either — Render Studio's scanner
/// admits both — so the file is one level up about as often as it is in the
/// folder given. Two is enough to cover that and stops well short of the
/// session folder, where a file would describe a batch rather than this take.
const MAX_ANCESTOR_DEPTH: usize = 2;

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct SessionMeta {
    pub format: u32,
    pub session_id: String,
    /// The `mirv_movie_fps` the batch was captured at.
    pub capture_fps: i32,
}

impl SessionMeta {
    pub fn new(session_id: impl Into<String>, capture_fps: i32) -> Self {
        Self {
            format: FORMAT,
            session_id: session_id.into(),
            capture_fps,
        }
    }
}

/// Writes a take's capture settings into its block folder.
///
/// Best-effort by design: a capture that succeeded must not be reported as
/// failed because a metadata file could not be written, so callers log the
/// error and carry on.
pub fn write(block_folder: &Path, meta: &SessionMeta) -> std::io::Result<()> {
    let json = serde_json::to_vec_pretty(meta)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(block_folder.join(TAKE_FILE), json)
}

/// The capture settings recorded for a take, or `None` when there are none.
///
/// `None` is the normal answer for every take captured before this existed, and
/// for any folder assembled by hand — so it must read as "nothing to say", never
/// as a problem.
pub fn read_for_take(take_folder: &Path) -> Option<SessionMeta> {
    for dir in search_path(take_folder) {
        let candidate = dir.join(TAKE_FILE);
        let Ok(bytes) = std::fs::read(&candidate) else {
            continue;
        };
        let Ok(meta) = serde_json::from_slice::<SessionMeta>(&bytes) else {
            continue;
        };
        if meta.format == FORMAT {
            return Some(meta);
        }
    }
    None
}

/// The folder itself first, then its parents — nearest wins, so a take carrying
/// its own file is never overruled by one further up.
fn search_path(take_folder: &Path) -> Vec<PathBuf> {
    take_folder
        .ancestors()
        .take(MAX_ANCESTOR_DEPTH + 1)
        .map(PathBuf::from)
        .collect()
}

/// The warning to show when a render is about to interpret a take at a rate it
/// was not captured at, or `None` when there is nothing to say.
///
/// Returns a message rather than logging, so the same wording can go to the
/// activity log and to a job's own render log without drifting apart.
pub fn fps_mismatch_warning(take_folder: &Path, render_fps: u32) -> Option<String> {
    let meta = read_for_take(take_folder)?;
    // A non-positive recorded rate is meaningless and would divide by zero, so
    // there is nothing trustworthy to compare against. The cast below is safe
    // only because of this guard.
    if meta.capture_fps <= 0 || meta.capture_fps as u32 == render_fps {
        return None;
    }
    // Frames are interpreted at the render rate, so the output runs fast when
    // the render rate is the higher of the two and slow when it is lower.
    let ratio = render_fps as f32 / meta.capture_fps as f32;
    Some(format!(
        "this take was captured at {} fps but is being rendered at {} — FFmpeg reads the frame \
         sequence at the render rate, so the result will run {:.2}x {} and the audio will be cut \
         to match. Set the render FPS to {} unless this is deliberate.",
        meta.capture_fps,
        render_fps,
        if ratio > 1.0 { ratio } else { 1.0 / ratio },
        if ratio > 1.0 { "fast" } else { "slow" },
        meta.capture_fps,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dod_take_meta_{}", name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    /// The real layout: `<capture>/<session>/chain_JJ_bN/take0000/`. Returns the
    /// block folder (where the file goes) and the nested take folder HLAE makes.
    fn block_with_take(name: &str, session: &str, block: &str) -> (PathBuf, PathBuf) {
        let root = scratch(name);
        let block_folder = root.join(session).join(block);
        let take = block_folder.join("take0000");
        std::fs::create_dir_all(&take).expect("take dirs");
        (block_folder, take)
    }

    #[test]
    fn a_take_is_found_from_either_folder_render_studio_hands_out() {
        let (block, take) = block_with_take("finds", "session_20260827_120000", "dodtools_chain_01_b0");
        write(&block, &SessionMeta::new("session_20260827_120000", 120)).expect("write");

        // The scanner admits a take at the block folder or at the nested
        // `take0000` inside it, depending on how it was found, so both resolve.
        assert_eq!(read_for_take(&block).map(|m| m.capture_fps), Some(120));
        assert_eq!(read_for_take(&take).map(|m| m.capture_fps), Some(120));
    }

    #[test]
    fn two_batches_at_different_rates_do_not_contaminate_each_other() {
        // The question this design has to answer: capture some highlights at
        // 120, then more at 300. Each batch gets its own session folder and each
        // take carries its own file, so neither can speak for the other.
        let root = scratch("two_batches");
        let slow = root.join("session_20260827_120000").join("dodtools_chain_01_b0");
        let fast = root.join("session_20260827_130000").join("dodtools_chain_01_b0");
        std::fs::create_dir_all(&slow).expect("dirs");
        std::fs::create_dir_all(&fast).expect("dirs");
        write(&slow, &SessionMeta::new("session_20260827_120000", 120)).expect("write");
        write(&fast, &SessionMeta::new("session_20260827_130000", 300)).expect("write");

        assert_eq!(read_for_take(&slow).map(|m| m.capture_fps), Some(120));
        assert_eq!(read_for_take(&fast).map(|m| m.capture_fps), Some(300));
        assert_eq!(fps_mismatch_warning(&slow, 120), None);
        assert!(fps_mismatch_warning(&fast, 120).is_some());
    }

    #[test]
    fn a_take_moved_into_another_session_keeps_its_own_settings() {
        // The reason the file is per take rather than per session. Somebody
        // consolidating takes by hand would otherwise silently relabel them with
        // whatever folder they were dropped into — the exact bug this feature
        // exists to catch, one level up.
        let root = scratch("moved");
        let origin = root.join("session_A").join("dodtools_chain_01_b0");
        let elsewhere = root.join("session_B");
        std::fs::create_dir_all(&origin).expect("dirs");
        std::fs::create_dir_all(&elsewhere).expect("dirs");
        write(&origin, &SessionMeta::new("session_A", 120)).expect("write");
        // A neighbour in the destination that says something different.
        let neighbour = elsewhere.join("dodtools_chain_02_b0");
        std::fs::create_dir_all(&neighbour).expect("dirs");
        write(&neighbour, &SessionMeta::new("session_B", 300)).expect("write");

        let moved = elsewhere.join("dodtools_chain_01_b0");
        std::fs::rename(&origin, &moved).expect("move the take");

        assert_eq!(
            read_for_take(&moved).map(|m| m.capture_fps),
            Some(120),
            "the take was relabelled by the folder it was moved into"
        );
    }

    #[test]
    fn a_take_with_no_metadata_is_not_a_problem() {
        // Every take captured before this existed, and any folder assembled by
        // hand. Silence is the correct answer, not a warning.
        let (_block, take) = block_with_take("absent", "session_x", "dodtools_chain_01_b0");
        assert_eq!(read_for_take(&take), None);
        assert_eq!(fps_mismatch_warning(&take, 300), None);
    }

    #[test]
    fn an_unreadable_or_future_format_is_ignored_rather_than_guessed() {
        let (block, take) = block_with_take("garbage", "session_x", "dodtools_chain_01_b0");
        std::fs::write(block.join(TAKE_FILE), b"{not json").expect("write");
        assert_eq!(read_for_take(&take), None);

        let ahead = format!(
            r#"{{"format":{},"session_id":"s","capture_fps":120}}"#,
            FORMAT + 1
        );
        std::fs::write(block.join(TAKE_FILE), ahead).expect("write");
        assert_eq!(read_for_take(&take), None, "a newer format was read anyway");
    }

    #[test]
    fn a_matching_rate_says_nothing() {
        let (block, take) = block_with_take("match", "session_x", "dodtools_chain_01_b0");
        write(&block, &SessionMeta::new("s", 120)).expect("write");
        assert_eq!(fps_mismatch_warning(&take, 120), None);
    }

    #[test]
    fn the_warning_states_the_direction_and_the_factor() {
        let (block, take) = block_with_take("mismatch", "session_x", "dodtools_chain_01_b0");
        write(&block, &SessionMeta::new("s", 120)).expect("write");

        // The bug as it actually happened: captured at 120, rendered at 300.
        let fast = fps_mismatch_warning(&take, 300).expect("a mismatch must be reported");
        assert!(fast.contains("2.50x fast"), "{}", fast);
        assert!(fast.contains("Set the render FPS to 120"), "{}", fast);

        // And the other direction, which is just as wrong and reads differently.
        let slow = fps_mismatch_warning(&take, 60).expect("a mismatch must be reported");
        assert!(slow.contains("2.00x slow"), "{}", slow);
    }

    #[test]
    fn a_nonsense_recorded_rate_is_not_used_to_scold_the_user() {
        // A zero would divide by zero and a negative is meaningless; either way
        // there is nothing trustworthy to compare against.
        let (block, take) = block_with_take("zero", "session_x", "dodtools_chain_01_b0");
        write(&block, &SessionMeta::new("s", 0)).expect("write");
        assert_eq!(fps_mismatch_warning(&take, 300), None);
    }

    #[test]
    fn the_walk_stops_before_it_reaches_a_capture_drive() {
        // A file this far up describes a batch, or a drive, not this take.
        let root = scratch("too_far");
        let deep = root.join("a").join("b").join("c").join("take0000");
        std::fs::create_dir_all(&deep).expect("dirs");
        write(&root, &SessionMeta::new("s", 120)).expect("write");
        assert_eq!(read_for_take(&deep), None);
    }

    #[test]
    fn our_own_file_is_recognisable_so_it_cannot_pass_as_captured_output() {
        // `take_folder_has_content` decides a take landed by asking whether its
        // folder is non-empty. Without this, an empty block plus our file would
        // report a capture that never happened.
        assert!(is_metadata(std::ffi::OsStr::new(TAKE_FILE)));
        assert!(is_metadata(std::ffi::OsStr::new("DODTOOLS_TAKE.JSON")));
        assert!(!is_metadata(std::ffi::OsStr::new("00000.bmp")));
        assert!(!is_metadata(std::ffi::OsStr::new("sound.wav")));
    }
}
