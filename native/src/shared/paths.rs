use std::path::{Path, PathBuf};

pub fn get_appdata_dir() -> PathBuf {
    let mut path = dirs::config_dir().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    path.push("dod-tools");
    let _ = std::fs::create_dir_all(&path);
    path
}

/// True for exactly the filenames `build_batch_queue` gives patched chain
/// demos (`chain_01.dem`, `chain_9999.dem`, ...). No cap on digit count --
/// a batch of over a hundred thousand demos is implausible, but nothing here
/// assumes an upper bound either. A plain `starts_with("chain_")` would also
/// match a source demo that happens to share the prefix, e.g. a player named
/// "chain" with a demo called `chain_harrington_round1.dem` -- requiring the
/// rest of the name to be all digits rules that out.
pub fn is_chain_demo_filename(filename: &str) -> bool {
    filename
        .strip_prefix("chain_")
        .and_then(|rest| rest.strip_suffix(".dem"))
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// Stable identity for one capture take, shared by the capture and render
/// pipelines so they can correlate without either knowing about the other.
///
/// Takes land at `<capture_dir>/<session_id>/chain_JJ_bN/`, so the key is
/// normally the last two path components lowercased:
/// `session_20260818_142233/chain_01_b0`. Deliberately *not* the absolute
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
pub fn remove_console_log(game_root: &Path) {
    let _ = std::fs::remove_file(game_root.join("qconsole.log"));
}

/// Every scratch file a capture batch leaves in the game folder, cleared
/// according to the four auto-clear settings.
///
/// `game_root` is the folder holding `hl.exe`; the batch writes into it and
/// into its `dod/` subfolder. Nothing here is fatal — a capture that finished
/// should not fail because a leftover could not be deleted — so most removals
/// are best-effort, with two deliberate exceptions noted inline.
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
        remove_console_log(game_root);
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
    }

    if auto_clear_temp_demos && !save_local_patched_copy {
        // Retried, and logged loudly on final failure -- unlike the best-effort
        // removals above, the last demo in a batch can still have hl.exe's file
        // handle attached here (see #198's investigation), and a leftover chain
        // file after auto-clear went completely unnoticed the first time this
        // happened.
        if let Some(e) = remove_file_retrying(&dod_dir.join("primer.dem")) {
            crate::log_markdown(&format!(
                "⚠️ **Cleanup** — could not remove primer.dem after retrying: {e} (auto_clear_temp_demos left it behind; hl.exe may still have had it open)"
            ));
        }
        if let Ok(entries) = std::fs::read_dir(&dod_dir) {
            for entry in entries.flatten() {
                let filename = entry.file_name().to_string_lossy().to_string();
                if is_chain_demo_filename(&filename)
                    && let Some(e) = remove_file_retrying(&entry.path()) {
                        crate::log_markdown(&format!(
                            "⚠️ **Cleanup** — could not remove {filename} after retrying: {e} (auto_clear_temp_demos left it behind; hl.exe may still have had it open)"
                        ));
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
                if sidecar.exists() {
                    let _ = std::fs::remove_file(&path);
                    let _ = std::fs::remove_file(sidecar);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_take_key_uses_last_two_components_lowercased() {
        let key = take_key(Path::new(r"D:\Captures\Session_20260818_142233\Chain_01_b0"));
        assert_eq!(key, Some("session_20260818_142233/chain_01_b0".to_string()));
    }

    #[test]
    fn test_take_key_is_stable_across_drives() {
        // The same take copied to a different drive must produce the same key —
        // this is the whole reason the absolute path isn't used.
        let a = take_key(Path::new(r"D:\Captures\session_1\chain_01_b0"));
        let b = take_key(Path::new(r"X:\somewhere\else\session_1\chain_01_b0"));
        assert_eq!(a, b);
        assert!(a.is_some());
    }

    #[test]
    fn test_take_key_distinguishes_sessions() {
        let a = take_key(Path::new(r"D:\c\session_1\chain_01_b0"));
        let b = take_key(Path::new(r"D:\c\session_2\chain_01_b0"));
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
        let capture_side = take_key(Path::new(r"D:\Captures\session_1\chain_01_b0"));
        let render_side = take_key(Path::new(r"D:\Captures\session_1\chain_01_b0\take0000"));
        assert_eq!(capture_side, render_side);
        assert_eq!(capture_side, Some("session_1/chain_01_b0".to_string()));
    }

    #[test]
    fn test_take_key_handles_higher_numbered_takes() {
        let key = take_key(Path::new(r"D:\Captures\session_1\chain_01_b0\take0003"));
        assert_eq!(key, Some("session_1/chain_01_b0".to_string()));
    }

    #[test]
    fn console_log_is_cleared_beside_hl_exe() {
        let root = std::env::temp_dir().join(format!("dod_qconsole_root_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let log = root.join("qconsole.log");
        std::fs::write(&log, b"console spam").unwrap();

        remove_console_log(&root);

        assert!(!log.exists(), "qconsole.log beside hl.exe should be removed");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Cleanup runs whether or not the engine was launched with `-condebug`,
    /// so an absent log is ordinary rather than an error worth surfacing.
    #[test]
    fn a_missing_console_log_is_not_an_error() {
        let root = std::env::temp_dir().join(format!("dod_qconsole_none_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();

        remove_console_log(&root);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn is_chain_demo_filename_matches_real_output_names() {
        assert!(is_chain_demo_filename("chain_01.dem"));
        assert!(is_chain_demo_filename("chain_9999.dem"));
    }

    /// No digit-count cap: a batch large enough to need `chain_100500.dem`
    /// must still be cleaned up correctly, not silently left behind.
    #[test]
    fn is_chain_demo_filename_has_no_upper_bound_on_digit_count() {
        assert!(is_chain_demo_filename("chain_100500.dem"));
    }

    /// A source demo that happens to share the "chain_" prefix must never be
    /// mistaken for a patched output file and deleted.
    #[test]
    fn is_chain_demo_filename_rejects_lookalike_source_demos() {
        assert!(!is_chain_demo_filename("chain_harrington_round1.dem"));
        assert!(!is_chain_demo_filename("chain_.dem"));
        assert!(!is_chain_demo_filename("chain_01.dem.bak"));
        assert!(!is_chain_demo_filename("prefix_chain_01.dem"));
        assert!(!is_chain_demo_filename("chain_01.cfg"));
    }

    /// Nothing but that one file may be touched — the game folder holds the
    /// user's own configs, and anything wider here would be unrecoverable.
    #[test]
    fn nothing_but_the_console_log_is_touched() {
        let root = std::env::temp_dir().join(format!("dod_qconsole_keep_{}", std::process::id()));
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
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// Deletes a file, retrying a few times with a short pause between attempts.
///
/// `taskkill /F` returning does not mean hl.exe's open file handles are
/// released yet, and even confirming the process itself has left the process
/// list (`sysinfo`) does not guarantee it either -- kernel object cleanup can
/// lag a beat past both. Both cleanup guards (`CaptureCleanupGuard` in
/// `capture_engine.rs`, `WorkspaceGuard` in `patch/builder.rs`) call this for
/// the demo files hl.exe just had open (`primer.dem`, `chain_NN.dem`) rather
/// than the single unretried `let _ = std::fs::remove_file(..)` every other
/// file they clean up gets, because those other files were never mid-close
/// the way a demo hl.exe was just playing can be. See #198.
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
mod remove_file_retrying_tests {
    use super::*;

    #[test]
    fn a_missing_file_is_not_an_error() {
        let dir = std::env::temp_dir().join(format!("dod_rfr_missing_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(remove_file_retrying(&dir.join("nope.dem")).is_none());
    }

    #[test]
    fn an_unlocked_file_is_removed_on_the_first_attempt() {
        let dir = std::env::temp_dir().join(format!("dod_rfr_plain_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("chain_01.dem");
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

        let dir = std::env::temp_dir().join(format!("dod_rfr_delayed_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("chain_02.dem");
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
