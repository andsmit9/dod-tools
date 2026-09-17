//! Driving OBS Studio as an alternate capture path.
//!
//! See `docs/obs_alternate_capture.md` (#65). The design in one paragraph:
//! OBS records the game window in real time while dod-studio tells it when each
//! block starts and stops. HLAE issues no `mirv_recordmovie` at all, so the
//! engine simply plays back at `host_framerate 0` through the clip, and OBS
//! writes a finished file with audio already muxed.
//!
//! **How the two halves are joined.** The pipeline schedules commands at frame
//! ordinals inside the demo; OBS knows nothing about demo ticks. The bridge is
//! the engine's own console log: `build_safe_echos` already writes a marker at
//! every stage boundary of every block, and `-condebug` puts them in
//! `qconsole.log`. Measured over a real 17-block batch, markers arrive
//! **21-40 ms** after the tick that emitted them, under the heaviest I/O
//! configuration the pipeline has. That is the whole synchronisation mechanism
//! and it needs no new engine commands.
//!
//! `log_tail` reads that channel. `client` speaks obs-websocket v5.
//!
//! Everything here is blocking and thread-based, matching `capture_engine`,
//! and deliberately so: `CaptureCleanupGuard::drop` has to be able to send a
//! `StopRecord` on every path out of a batch, and a `Drop` has no async
//! runtime under it.
//!
//! That guard covers the paths the process survives to run. It does not cover
//! a panic — release builds are `panic = "abort"`, so nothing unwinds — nor a
//! hard kill or a power cut. `recover` handles those on the next start
//! instead, because from outside the process they are indistinguishable.

#![cfg(not(target_arch = "wasm32"))]

pub mod client;
pub mod log_tail;
pub mod provision;
pub mod recover;
pub mod session;

pub use client::{ObsClient, ObsError};
pub use log_tail::{LogTailer, Marker, MarkerKind};
pub use provision::{PROFILE_NAME as OBS_PROFILE_NAME, SCENE_NAME as OBS_SCENE_NAME};
pub use recover::{check as check_orphan, recover as recover_orphan, OrphanReport};
pub use session::{ObsSession, RecordedBlock};

/// Stage markers the capture path acts on.
///
/// `AUDIO_SYNC` rather than `SPEED_FLUSH` is the record trigger, and the reason
/// is measured: the early pre-roll does not run at real time, because the
/// engine is still settling out of fast-forward. `SPEED_FLUSH`->`AUDIO_SYNC` is
/// 4.0s of demo time but consistently took ~2.8-3.7s of wall-clock, whereas
/// `AUDIO_SYNC`->`START_RECORD` is 1.0s of demo time and measured 1.010s mean
/// across 15 blocks. Triggering at `AUDIO_SYNC` also leaves ~1s of pre-roll in
/// the file instead of five, so there is less to trim later.
///
/// 1.0s of lead against OBS's measured 59-85 ms start latency is a ~14x margin.
pub const TRIGGER_MARKER: MarkerKind = MarkerKind::AudioSync;

/// Marker that ends a block's recording.
///
/// The original design computed the stop time instead, because the log's
/// latency was unknown and a timer avoided depending on it twice. Measuring the
/// latency removed the reason, and dropping the timer removed the design's one
/// shaky assumption: demo time tracks wall-clock in the mean but with
/// sigma ~0.14s on a 1.0s span, and not every block gets a full lead — two of
/// seventeen were clamped where blocks chained or highlights merged. An
/// echo-driven stop is self-correcting and needs none of that.
pub const STOP_MARKER: MarkerKind = MarkerKind::StopRecord;
