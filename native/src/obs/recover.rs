//! Recovering an OBS recording that outlived the app.
//!
//! **Why this exists rather than a better cleanup path.** `CaptureCleanupGuard`
//! stops OBS on every exit path the process actually gets to run — a cancel, a
//! crashed game, a finished batch. It does not run on a panic: this workspace
//! builds release with `panic = "abort"` (`Cargo.toml`), so there is no
//! unwinding and no `Drop`. It also cannot run on a hard kill, an access
//! violation, a `tauri dev` restart, or a power cut.
//!
//! Those four are indistinguishable from the outside and no in-process
//! mechanism can cover them, because in all four the process is simply gone.
//! So the recovery is on the way back in rather than on the way out: ask OBS,
//! on start-up, whether it is still recording into a folder that could only be
//! ours.
//!
//! Without this, an orphaned recording runs until the disk fills. `preflight`
//! refuses to start a batch while OBS is already recording, so it surfaces
//! eventually — but "eventually" is the next time the user starts a batch, and
//! the disk does not wait.

use std::path::{Path, PathBuf};

use crate::patch::ObsConfig;

use super::client::{ObsClient, ObsError};
use super::session::{fold_into_take, STREAM_FOLDER, TAKE_FOLDER};

/// What start-up found.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OrphanReport {
    /// OBS is recording right now.
    pub recording: bool,
    /// Where it is writing.
    pub directory: String,
    /// Whether that folder is one of ours — see `looks_like_ours`.
    pub ours: bool,
}

impl OrphanReport {
    /// Whether there is something worth offering to clean up.
    pub fn actionable(&self) -> bool {
        self.recording && self.ours
    }
}

/// Whether a record directory could only have been set by this app.
///
/// The test is the path shape, not a configured root, and that is deliberate:
/// take folders are handed out by the export pool and can land on any drive it
/// routes to, so there is no single root to compare against. What every one of
/// them does share is the tail `<take folder>/take0000/all`, written by
/// `ObsSession::begin_block` and by nothing else. OBS's own default is the
/// user's Videos folder, which cannot collide with that shape by accident.
///
/// Being wrong in the cautious direction only costs a missed recovery. Being
/// wrong the other way would mean stopping somebody's unrelated recording,
/// which is why nothing here acts on a directory that fails this test.
fn looks_like_ours(dir: &Path) -> bool {
    let mut parts = dir.components().rev().filter_map(|c| match c {
        std::path::Component::Normal(s) => Some(s.to_string_lossy().to_lowercase()),
        _ => None,
    });
    let Some(stream) = parts.next() else {
        return false;
    };
    let Some(take) = parts.next() else {
        return false;
    };
    // A third component has to exist: the take folder itself. A bare
    // `C:\take0000\all` is not something this app would ever produce.
    stream == STREAM_FOLDER && take == TAKE_FOLDER && parts.next().is_some()
}

/// Asks OBS whether it is still recording one of our takes. Read-only.
pub fn check(cfg: &ObsConfig) -> Result<OrphanReport, ObsError> {
    let mut client = ObsClient::connect(&cfg.address(), &cfg.password)?;
    let recording = client.is_recording()?;
    if !recording {
        return Ok(OrphanReport {
            recording: false,
            directory: String::new(),
            ours: false,
        });
    }
    let directory = client.record_directory()?;
    let ours = looks_like_ours(Path::new(&directory));
    Ok(OrphanReport {
        recording,
        directory,
        ours,
    })
}

/// Stops an orphaned recording and folds its file into the take folder.
///
/// Refuses anything that does not pass `looks_like_ours`, so a user who
/// happened to be recording something of their own when they opened dod-tools
/// keeps their recording.
pub fn recover(cfg: &ObsConfig) -> Result<Option<PathBuf>, ObsError> {
    let mut client = ObsClient::connect(&cfg.address(), &cfg.password)?;
    if !client.is_recording()? {
        return Ok(None);
    }
    let directory = client.record_directory()?;
    let dest = PathBuf::from(&directory);
    if !looks_like_ours(&dest) {
        return Err(ObsError::Request {
            request: "StopRecord".into(),
            detail: format!(
                "OBS is recording into {directory}, which is not a dod-tools take folder. \
                 Leaving it alone."
            ),
        });
    }
    let path = client.stop_record()?;
    let video = fold_into_take(Path::new(&path), &dest).map_err(|detail| ObsError::Request {
        request: "StopRecord".into(),
        detail,
    })?;
    Ok(Some(video))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape `begin_block` writes, and the only one recovery acts on.
    #[test]
    fn recognises_a_take_folder() {
        assert!(looks_like_ours(Path::new(
            r"D:\dod-tools\exports\dodtools_chain_01_b0\take0000\all"
        )));
        assert!(looks_like_ours(Path::new(
            "/mnt/exports/dodtools_chain_01_b0/take0000/all"
        )));
    }

    /// Case is not meaningful on Windows and the path comes back from OBS, not
    /// from us, so it is not ours to assume.
    #[test]
    fn is_case_insensitive() {
        assert!(looks_like_ours(Path::new(
            r"D:\Exports\Chain_01_b0\Take0000\All"
        )));
    }

    /// Anything else is somebody's own recording. Getting this wrong stops a
    /// stream or a session the user cared about.
    #[test]
    fn refuses_everything_else() {
        for path in [
            r"C:\Users\chris\Videos",
            r"C:\Users\chris\Videos\take0000",
            // The stream folder without a take folder above it.
            r"C:\all",
            // Right names, wrong order.
            r"D:\exports\block\all\take0000",
            // A take folder with no take root above it.
            r"C:\take0000\all",
        ] {
            assert!(!looks_like_ours(Path::new(path)), "should refuse {path}");
        }
    }
}
