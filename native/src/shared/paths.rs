use std::path::{Path, PathBuf};

pub fn get_appdata_dir() -> PathBuf {
    let mut path = dirs::config_dir().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    path.push("dod-tools");
    let _ = std::fs::create_dir_all(&path);
    path
}

/// Stem of the primer demo `build_batch_queue` writes into the game's `dod/`
/// folder. The `dodtools_` prefix matches every other file this app writes
/// there (`dodtools_helper.cfg`, `dodtools_capture_done.cfg`) and keeps the
/// name out of the space a player's own demo could plausibly occupy (#197).
pub const PRIMER_DEMO_STEM: &str = "dodtools_primer";

/// Prefix of every patched chain demo `build_batch_queue` writes into `dod/`,
/// followed by a zero-padded job number. See [`PRIMER_DEMO_STEM`] for why the
/// `dodtools_` part is there.
pub const CHAIN_DEMO_PREFIX: &str = "dodtools_chain_";

/// True for exactly the filenames `build_batch_queue` gives patched chain
/// demos (`dodtools_chain_01.dem`, `dodtools_chain_9999.dem`, ...). No cap on
/// digit count -- a batch of over a hundred thousand demos is implausible, but
/// nothing here assumes an upper bound either. A plain
/// `starts_with(CHAIN_DEMO_PREFIX)` would also match a source demo that
/// happens to share the prefix -- requiring the rest of the name to be all
/// digits rules that out.
///
/// Bare `chain_NN.dem` names, which this app wrote before #197, are
/// deliberately *not* matched: the whole point of the prefix is that a name
/// without it might be the user's own demo, and leaving a stale file behind is
/// the strictly safer failure than deleting someone's recording.
pub fn is_chain_demo_filename(filename: &str) -> bool {
    filename
        .strip_prefix(CHAIN_DEMO_PREFIX)
        .and_then(|rest| rest.strip_suffix(".dem"))
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// Stable identity for one capture take, shared by the capture and render
/// pipelines so they can correlate without either knowing about the other.
///
/// Takes land at `<capture_dir>/<session_id>/dodtools_chain_JJ_bN/`, so the
/// key is normally the last two path components lowercased:
/// `session_20260818_142233/dodtools_chain_01_b0`. Deliberately *not* the absolute
/// path — capture output routinely gets moved onto a different drive before
/// rendering, which would invalidate it — and deliberately not the take name
/// alone, which repeats every batch.
///
/// HLAE's `mirv_movie` plugin auto-numbers each recording into a `take0000`,
/// `take0001`, ... subfolder *under* the block folder (`hlcr::scanner`'s own
/// `is_renderable_take` has to account for the same nesting) — Render
/// Studio's real folder scanner finds takes at that nested path, not the
/// block folder itself, so a trailing `take*` component is skipped to keep
/// both sides keying off the same block folder regardless of which literal
/// path was passed in.
pub fn take_key(take_folder: &Path) -> Option<String> {
    let is_take_number_folder = take_folder
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase().starts_with("take"))
        .unwrap_or(false);
    let block_folder = if is_take_number_folder {
        take_folder.parent()?
    } else {
        take_folder
    };
    let take_name = block_folder.file_name()?.to_string_lossy().to_lowercase();
    let session = block_folder.parent()?.file_name()?.to_string_lossy().to_lowercase();
    Some(format!("{}/{}", session, take_name))
}

/// Deletes the engine's `qconsole.log`.
///
/// `-condebug` writes it to `hl.exe`'s own folder, not the mod folder, so
/// cleanup that named `dod/qconsole.log` never matched anything and the log
/// accumulated across every session indefinitely. (`condump` is the separate
/// console command that drops a numbered `condump_NNN.txt`; nothing here
/// issues it.)
///
/// Beside `hl.exe` specifically, not in the launching process's working
/// directory: `build_hlae_process` runs HLAE with its CWD set to HLAE's own
/// folder, and the log still appears next to `hl.exe` with nothing in HLAE's
/// folder. So deriving the path from the game path is always right.
#[cfg(not(target_arch = "wasm32"))]
pub fn console_log_path(game_root: &Path) -> PathBuf {
    game_root.join("qconsole.log")
}

/// Best-effort removal of the console log, for callers outside a capture
/// batch. `clear_capture_scratch` does not go through this -- it needs the
/// retry-and-report treatment every other scratch file there gets -- but both
/// resolve the path through [`console_log_path`], so neither can drift from
/// the other on *where* the log lives.
#[cfg(not(target_arch = "wasm32"))]
pub fn remove_console_log(game_root: &Path) {
    let _ = std::fs::remove_file(console_log_path(game_root));
}

/// Every scratch file a capture batch leaves in the game folder, cleared
/// according to the four auto-clear settings.
///
/// `game_root` is the folder holding `hl.exe`; the batch writes into it and
/// into its `dod/` subfolder. Nothing here is fatal — a capture that finished
/// should not fail because a leftover could not be deleted — but nothing here
/// is silent either: every removal goes through `remove_scratch_file`, which
/// retries and then reports a file it could not delete against the checkbox
/// that asked for it. See #218.
///
/// This is one function because it used to be two verbatim copies, in
/// `CaptureCleanupGuard::drop` (capture_engine.rs) and `WorkspaceGuard::drop`
/// (patch/builder.rs). Both guards run for the same batch and clear the same
/// files; adding a scratch file to one and forgetting the other left it in the
/// user's game folder with no error and no log line. Add new scratch filenames
/// here and both paths get them.
///
/// Gated like the helpers it calls: this is all `std::fs`, and per CLAUDE.md's
/// WASM guardrail direct file I/O stays off the wasm32 build.
#[cfg(not(target_arch = "wasm32"))]
pub fn clear_capture_scratch(
    game_root: &Path,
    auto_clear_logs: bool,
    auto_clear_temp_demos: bool,
    auto_clear_previews: bool,
    save_local_patched_copy: bool,
) {
    let dod_dir = game_root.join("dod");

    if auto_clear_logs {
        remove_scratch_file(&console_log_path(game_root), "Auto-clear Logs");
        remove_scratch_file(&dod_dir.join("dodtools_helper.cfg"), "Auto-clear Logs");
        remove_scratch_file(&dod_dir.join("dodtools_capture_done.cfg"), "Auto-clear Logs");
        remove_scratch_file(&dod_dir.join("dod_quit.cfg"), "Auto-clear Logs");
        // Legacy only: nothing has written a per-chain cfg since the helper cfg
        // absorbed those aliases. Kept so a user upgrading from a build that
        // did write them still gets them cleared, not because anything current
        // produces one.
        if let Ok(entries) = std::fs::read_dir(&dod_dir) {
            for entry in entries.flatten() {
                let filename = entry.file_name().to_string_lossy().to_string();
                if filename.starts_with("dodtools_chain_") && filename.ends_with(".cfg") {
                    remove_scratch_file(&entry.path(), "Auto-clear Logs");
                }
            }
        }
    }

    if auto_clear_temp_demos && !save_local_patched_copy {
        // Retried, and logged loudly on final failure -- unlike the best-effort
        // removals above, the last demo in a batch can still have hl.exe's file
        // handle attached here (see #198's investigation), and a leftover chain
        // file after auto-clear went completely unnoticed the first time this
        // happened.
        let primer_demo = format!("{PRIMER_DEMO_STEM}.dem");
        if let Some(e) = remove_file_retrying(&dod_dir.join(&primer_demo)) {
            crate::log_markdown(&format!(
                "⚠️ **Cleanup** — could not remove {primer_demo} after retrying: {e} (auto_clear_temp_demos left it behind; hl.exe may still have had it open)"
            ));
        }
        if let Ok(entries) = std::fs::read_dir(&dod_dir) {
            for entry in entries.flatten() {
                let filename = entry.file_name().to_string_lossy().to_string();
                if is_chain_demo_filename(&filename) {
                    remove_scratch_file(&entry.path(), "Auto-clear Temp Demos");
                }
            }
        }
    }

    if auto_clear_previews {
        // Only a `_preview.dem` with its hidden `.dodtools_preview` sidecar is
        // ours. A demo the user named that way themselves has no sidecar and is
        // left alone.
        for scan_dir in [dod_dir.clone(), game_root.to_path_buf()] {
            let Ok(entries) = std::fs::read_dir(scan_dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let Some(filename) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if !filename.ends_with("_preview.dem") {
                    continue;
                }
                let sidecar = path.with_extension("dodtools_preview");
                if !sidecar.exists() {
                    continue;
                }
                // Order matters, and the sidecar goes second on purpose. The
                // sidecar is the *only* thing marking this demo as ours; drop
                // it while the demo itself is still on disk and the demo can
                // never be recognised for cleanup again, so it would sit in
                // the game folder forever. Removing it only once the demo is
                // actually gone means a failure here is retried next batch
                // instead of becoming permanent.
                if remove_scratch_file(&path, "Auto-clear Previews") {
                    remove_scratch_file(&sidecar, "Auto-clear Previews");
                }
            }
        }
    }
}

/// Removes one scratch file, retrying, and says so in the app log if it still
/// could not be removed. Returns whether the file is gone.
///
/// Every removal here used to be a bare `let _ = std::fs::remove_file(..)`,
/// which is doubly silent: the result is discarded, and the `log::warn!` the
/// surrounding code reached for has no registered backend in this app. #198
/// fixed that for the temp demos only. The same silence applies to every other
/// file cleared here for a reason that has nothing to do with hl.exe -- a
/// `qconsole.log` the user has open in a text editor to read is the obvious
/// one -- and the symptom is identical: the checkbox is on, the file is still
/// there, and nothing anywhere says why.
///
/// `setting` names the auto-clear checkbox that asked for this, so the log line
/// points at the control the user would go looking for.
#[cfg(not(target_arch = "wasm32"))]
fn remove_scratch_file(path: &Path, setting: &str) -> bool {
    match remove_file_retrying(path) {
        None => true,
        Some(e) => {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned());
            crate::log_markdown(&format!(
                "⚠️ **Cleanup** — could not remove {name} after retrying: {e} ({setting} left it behind; something may still have had it open)"
            ));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Scratch;

    #[test]
    fn test_take_key_uses_last_two_components_lowercased() {
        let key = take_key(Path::new(r"D:\Captures\Session_20260818_142233\Dodtools_Chain_01_b0"));
        assert_eq!(key, Some("session_20260818_142233/dodtools_chain_01_b0".to_string()));
    }

    #[test]
    fn test_take_key_is_stable_across_drives() {
        // The same take copied to a different drive must produce the same key —
        // this is the whole reason the absolute path isn't used.
        let a = take_key(Path::new(r"D:\Captures\session_1\dodtools_chain_01_b0"));
        let b = take_key(Path::new(r"X:\somewhere\else\session_1\dodtools_chain_01_b0"));
        assert_eq!(a, b);
        assert!(a.is_some());
    }

    #[test]
    fn test_take_key_distinguishes_sessions() {
        let a = take_key(Path::new(r"D:\c\session_1\dodtools_chain_01_b0"));
        let b = take_key(Path::new(r"D:\c\session_2\dodtools_chain_01_b0"));
        assert_ne!(a, b);
    }

    #[test]
    fn test_take_key_none_without_a_parent_component() {
        assert_eq!(take_key(Path::new("")), None);
    }

    #[test]
    fn test_take_key_matches_across_the_take_number_nesting() {
        // The capture side (native/src/patch/builder.rs) computes take_key
        // from the block folder it asked HLAE to write to. The render side
        // (render_manager.rs) computes it from ClipData.take_folder, which
        // scan_folder_background sets to wherever it actually found the
        // wav/bmp — one level deeper, inside HLAE's own take0000 auto-numbered
        // subfolder. Both must resolve to the same key or auto-Rendered can
        // never correlate a finished render back to its highlights.
        let capture_side = take_key(Path::new(r"D:\Captures\session_1\dodtools_chain_01_b0"));
        let render_side = take_key(Path::new(r"D:\Captures\session_1\dodtools_chain_01_b0\take0000"));
        assert_eq!(capture_side, render_side);
        assert_eq!(capture_side, Some("session_1/dodtools_chain_01_b0".to_string()));
    }

    #[test]
    fn test_take_key_handles_higher_numbered_takes() {
        let key = take_key(Path::new(r"D:\Captures\session_1\dodtools_chain_01_b0\take0003"));
        assert_eq!(key, Some("session_1/dodtools_chain_01_b0".to_string()));
    }

    #[test]
    fn console_log_is_cleared_beside_hl_exe() {
        let root = Scratch::new("qconsole_root");
        let log = root.join("qconsole.log");
        std::fs::write(&log, b"console spam").unwrap();

        remove_console_log(&root);

        assert!(!log.exists(), "qconsole.log beside hl.exe should be removed");
    }

    /// Cleanup runs whether or not the engine was launched with `-condebug`,
    /// so an absent log is ordinary rather than an error worth surfacing.
    #[test]
    fn a_missing_console_log_is_not_an_error() {
        let root = Scratch::new("qconsole_none");
        remove_console_log(&root);
    }

    #[test]
    fn is_chain_demo_filename_matches_real_output_names() {
        assert!(is_chain_demo_filename("dodtools_chain_01.dem"));
        assert!(is_chain_demo_filename("dodtools_chain_9999.dem"));
    }

    /// No digit-count cap: a batch large enough to need `dodtools_chain_100500.dem`
    /// must still be cleaned up correctly, not silently left behind.
    #[test]
    fn is_chain_demo_filename_has_no_upper_bound_on_digit_count() {
        assert!(is_chain_demo_filename("dodtools_chain_100500.dem"));
    }

    /// A source demo that happens to share the "chain_" prefix must never be
    /// mistaken for a patched output file and deleted.
    #[test]
    fn is_chain_demo_filename_rejects_lookalike_source_demos() {
        assert!(!is_chain_demo_filename("chain_harrington_round1.dem"));
        assert!(!is_chain_demo_filename("chain_.dem"));
        assert!(!is_chain_demo_filename("dodtools_chain_01.dem.bak"));
        assert!(!is_chain_demo_filename("prefix_chain_01.dem"));
        assert!(!is_chain_demo_filename("dodtools_chain_01.cfg"));
    }

    /// Nothing but that one file may be touched — the game folder holds the
    /// user's own configs, and anything wider here would be unrecoverable.
    #[test]
    fn nothing_but_the_console_log_is_touched() {
        let root = Scratch::new("qconsole_keep");
        let dod = root.join("dod");
        std::fs::create_dir_all(&dod).unwrap();
        let keep = [
            dod.join("config.cfg"),
            dod.join("movie.cfg"),
            root.join("debug.log"),
            root.join("condump_001.txt"),
        ];
        for f in &keep {
            std::fs::write(f, b"mine").unwrap();
        }
        std::fs::write(root.join("qconsole.log"), b"spam").unwrap();

        remove_console_log(&root);

        for f in &keep {
            assert!(f.exists(), "{:?} is the user's file and must survive", f);
        }
    }
}

/// Deletes a file, retrying a few times with a short pause between attempts.
///
/// `taskkill /F` returning does not mean hl.exe's open file handles are
/// released yet, and even confirming the process itself has left the process
/// list (`sysinfo`) does not guarantee it either -- kernel object cleanup can
/// lag a beat past both. The demo files hl.exe was just playing
/// (`dodtools_primer.dem`, `dodtools_chain_NN.dem`) are what this exists for,
/// and both cleanup guards (`CaptureCleanupGuard` in `capture_engine.rs`,
/// `WorkspaceGuard` in `patch/builder.rs`) call it for them. See #198.
///
/// #218 asked whether the sibling auto-clear settings share that race. They do
/// not, and the answer is worth recording because it is not guessable from the
/// Rust side:
///
/// * `qconsole.log` is **not** held open across a session. `Con_DebugLog` in
///   `hw.dll` does `_open(O_WRONLY|O_CREAT|O_APPEND)` / `_write` / `_close`
///   once per console line, so outside the microseconds of a single line
///   there is no handle to wait on at all.
/// * The `.cfg` files are read whole and released; nothing keeps them open.
/// * `_preview.dem` files belong to a separate preview session, not to the
///   capture batch whose guard runs this.
///
/// Every scratch file goes through this anyway -- `remove_scratch_file` is a
/// thin reporting wrapper over it. When the file is not locked, which per the
/// above is the overwhelmingly common case, the first attempt succeeds and it
/// costs nothing, and it means one uniform path instead of a fast one and a
/// careful one that can drift apart.
///
/// Returns the last error seen, or `None` on success or if the file was
/// already gone -- callers decide how loudly to report a real failure; this
/// only decides how hard to try first.
pub fn remove_file_retrying(path: &Path) -> Option<std::io::Error> {
    const ATTEMPTS: u32 = 5;
    const DELAY: std::time::Duration = std::time::Duration::from_millis(200);
    let mut last_err = None;
    for attempt in 1..=ATTEMPTS {
        match std::fs::remove_file(path) {
            Ok(()) => return None,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                last_err = Some(e);
                if attempt < ATTEMPTS {
                    std::thread::sleep(DELAY);
                }
            }
        }
    }
    last_err
}

#[cfg(test)]
mod auto_clear_previews_tests {
    use super::*;
    use crate::test_support::Scratch;

    fn make_preview(dir: &Path, stem: &str) -> (PathBuf, PathBuf) {
        let demo = dir.join(format!("{stem}_preview.dem"));
        let sidecar = demo.with_extension("dodtools_preview");
        std::fs::write(&demo, b"demo").unwrap();
        std::fs::write(&sidecar, b"").unwrap();
        (demo, sidecar)
    }

    fn scratch(name: &str) -> Scratch {
        let root = Scratch::new(format_args!("acp_{name}"));
        std::fs::create_dir_all(root.join("dod")).unwrap();
        root
    }

    #[test]
    fn a_preview_with_its_sidecar_is_removed() {
        let root = scratch("pair");
        let (demo, sidecar) = make_preview(&root.join("dod"), "match1");

        clear_capture_scratch(&root, false, false, true, false);

        assert!(!demo.exists(), "the preview demo should be gone");
        assert!(!sidecar.exists(), "its sidecar should be gone with it");
    }

    #[test]
    fn a_user_named_preview_without_a_sidecar_is_left_alone() {
        let root = scratch("nosidecar");
        let demo = root.join("dod").join("my_own_preview.dem");
        std::fs::write(&demo, b"demo").unwrap();

        clear_capture_scratch(&root, false, false, true, false);

        assert!(demo.exists(), "a demo the user named this way is not ours to delete");
    }

    /// The sidecar is the only thing marking a `_preview.dem` as ours. If it is
    /// removed while the demo itself could not be, the demo stops being
    /// recognisable and sits in the game folder permanently -- cleanup can
    /// never pick it up again on any later batch. So a failed demo removal must
    /// leave the sidecar in place.
    ///
    /// Windows only, for the same reason `remove_file_retrying`'s own
    /// lock test is: `File::open`'s default share mode includes
    /// FILE_SHARE_DELETE, so a plain open would not block the delete and this
    /// would pass without testing anything.
    #[cfg(target_os = "windows")]
    #[test]
    fn a_sidecar_outlives_a_demo_that_could_not_be_removed() {
        use std::os::windows::fs::OpenOptionsExt;

        let root = scratch("orphan");
        let (demo, sidecar) = make_preview(&root.join("dod"), "locked");

        let handle = std::fs::OpenOptions::new().read(true).share_mode(1).open(&demo).unwrap();
        assert!(
            std::fs::remove_file(&demo).is_err(),
            "the fixture itself is wrong if a plain remove_file succeeds here"
        );

        clear_capture_scratch(&root, false, false, true, false);

        assert!(demo.exists(), "the locked demo could not have been removed");
        assert!(
            sidecar.exists(),
            "the sidecar must survive, or this demo can never be cleaned up again"
        );

        drop(handle);
    }
}

#[cfg(test)]
mod remove_file_retrying_tests {
    use super::*;
    use crate::test_support::Scratch;

    #[test]
    fn a_missing_file_is_not_an_error() {
        let dir = Scratch::new("rfr_missing");
        assert!(remove_file_retrying(&dir.join("nope.dem")).is_none());
    }

    #[test]
    fn an_unlocked_file_is_removed_on_the_first_attempt() {
        let dir = Scratch::new("rfr_plain");
        let file = dir.join("dodtools_chain_01.dem");
        std::fs::write(&file, b"demo").unwrap();

        assert!(remove_file_retrying(&file).is_none());
        assert!(!file.exists());
    }

    // Only on Windows: `std::fs::File::open`'s default share mode there
    // already includes FILE_SHARE_DELETE, so holding a plain handle open
    // would not actually block a delete and this test would pass without the
    // retry loop doing anything. `share_mode(1)` (FILE_SHARE_READ only,
    // matching the pattern already used in `capture_engine.rs`'s demo-copy
    // path) withholds delete sharing, which is what reproduces a file hl.exe
    // still has open the way this function exists to wait out.
    #[cfg(target_os = "windows")]
    #[test]
    fn a_file_that_becomes_removable_partway_through_succeeds() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = Scratch::new("rfr_delayed");
        let file = dir.join("dodtools_chain_02.dem");
        std::fs::write(&file, b"demo").unwrap();

        let handle = std::fs::OpenOptions::new().read(true).share_mode(1).open(&file).unwrap();
        assert!(
            std::fs::remove_file(&file).is_err(),
            "the fixture itself is wrong if a plain remove_file succeeds while this handle is open"
        );

        let releaser = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(350));
            drop(handle);
        });

        let result = remove_file_retrying(&file);
        releaser.join().unwrap();

        assert!(result.is_none(), "{result:?}");
        assert!(!file.exists());
    }
}
