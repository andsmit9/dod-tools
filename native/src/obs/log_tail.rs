//! Reading the engine's console log as a per-block signalling channel.
//!
//! `-condebug` makes GoldSrc mirror the console into `qconsole.log` beside
//! `hl.exe`, and the pipeline already echoes a marker at every stage boundary
//! (`native/src/patch/builder.rs`, `build_safe_echos`). This turns that into a
//! stream of typed events.
//!
//! **Measured behaviour this relies on**, from a real 17-block batch at 120fps
//! with three BMP streams in flight — the heaviest I/O the pipeline produces:
//!
//! - Markers arrive **21-40 ms** after the tick that emitted them.
//! - Commands scheduled one tick apart arrive one read apart. Nothing
//!   accumulates and nothing flushes late.
//! - No marker was ever missing from any block.
//!
//! So the log is effectively unbuffered and can carry a start signal.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

/// Prefix `build_safe_echos` puts on every marker.
const LOG_TAG: &str = "[dod-tools]";

/// How often the tailer looks for new bytes.
///
/// Matches the ~16ms polling cadence `capture_engine` uses for external
/// processes. It has to be at least this fine: a poll interval coarser than the
/// engine's own flush granularity would add latency that is ours rather than
/// the engine's, on the one path where latency is the whole point.
const POLL: Duration = Duration::from_millis(16);

/// A stage boundary, as the engine announced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerKind {
    /// Playback drops out of fast-forward back to real time.
    SpeedFlush,
    /// `stopsound` fires to flush the audio buffers fast-forward corrupted.
    /// One second of demo time before the recording starts, and the point from
    /// which the engine is measurably at real time.
    AudioSync,
    /// The clip's first frame.
    StartRecord,
    /// The clip's last frame.
    StopRecord,
    /// Playback resumes fast-forwarding towards the next block.
    FastForward,
    /// Every job in the batch is done.
    BatchComplete,
    /// Periodic position report, emitted throughout regardless of blocks.
    Breadcrumb,
    /// The first block of a demo in the batch — one per demo, at the very
    /// start of its schedule. Carries (job_idx, total_jobs, clip_count) via
    /// `Marker::demo_progress`, 1-based. See issue #98 — this is what a
    /// "loading demo N of M" notification hooks onto.
    DemoStart,
    /// Playback is about to fast-forward towards a specific upcoming clip —
    /// one per clip, fired alongside `DemoStart` for clip 1 and alongside
    /// `FastForward` for every later clip that isn't chained to the one
    /// before it (a chained pair never fast-forwards between them, so there
    /// is nothing to announce). Carries (job_idx, total_jobs, clip_idx,
    /// clip_count) via `Marker::next_clip_progress`, 1-based. See issue #98.
    NextClip,
    /// Anything else the pipeline echoed — custom commands, chunk
    /// continuations. Carried rather than dropped so a caller can log it.
    Other,
}

impl MarkerKind {
    fn parse(label: &str) -> Self {
        let head = label.split(&[' ', '-'][..]).next().unwrap_or("");
        match head {
            "SPEED_FLUSH" => Self::SpeedFlush,
            "AUDIO_SYNC" => Self::AudioSync,
            "START_RECORD" => Self::StartRecord,
            "STOP_RECORD" => Self::StopRecord,
            "FAST_FORWARD" => Self::FastForward,
            "BATCH_COMPLETE" => Self::BatchComplete,
            "BREADCRUMB" => Self::Breadcrumb,
            "DEMO_START" => Self::DemoStart,
            "NEXT_CLIP" => Self::NextClip,
            _ => Self::Other,
        }
    }
}

/// One marker line, parsed.
#[derive(Clone, Debug)]
pub struct Marker {
    pub kind: MarkerKind,
    /// The demo frame ordinal the echo names, when it names one.
    /// `BATCH_COMPLETE` and chunk continuations carry none.
    pub tick: Option<i64>,
    /// (job_idx, total_jobs, clip_count), 1-based job_idx — only present on
    /// `MarkerKind::DemoStart`, e.g. "DEMO_START 5 14 2".
    pub demo_progress: Option<(u32, u32, u32)>,
    /// (job_idx, total_jobs, clip_idx, clip_count), 1-based — only present on
    /// `MarkerKind::NextClip`, e.g. "NEXT_CLIP 5 14 2 3".
    pub next_clip_progress: Option<(u32, u32, u32, u32)>,
    /// The text after the tag, for logging.
    pub label: String,
}

/// Tails `qconsole.log` and sends every `[dod-tools]` marker onward.
pub struct LogTailer {
    path: PathBuf,
    offset: u64,
    pending: String,
}

impl LogTailer {
    /// Starts at the **end** of any existing file.
    ///
    /// This matters: the log is not cleared between batches — cleanup removes
    /// it after a run, so a file left by a crashed or cancelled batch is
    /// normal, and one built up over several runs is common. Reading from byte
    /// zero would replay every historical marker as if it had just happened,
    /// and the first thing acted on would be a `START_RECORD` from a batch that
    /// finished days ago.
    pub fn at_end(path: &Path) -> Self {
        let offset = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        Self {
            path: path.to_path_buf(),
            offset,
            pending: String::new(),
        }
    }

    /// Reads whatever has been appended since the last call.
    ///
    /// Handles the file being deleted or truncated underneath by rewinding to
    /// zero. A shrinking file is a new file, and its content is live rather than
    /// history.
    ///
    /// The only thing that actually shortens it is a batch's own cleanup
    /// (`auto_clear_logs`). The engine **appends** across launches — measured on
    /// a 1.5 MB `qconsole.log` holding 100 startup banners — so this rewind is
    /// defensive rather than a path the engine drives.
    pub fn poll(&mut self) -> Vec<Marker> {
        let Ok(mut file) = std::fs::File::open(&self.path) else {
            // Not there yet, or just removed. Either way the next thing written
            // starts from the beginning.
            self.offset = 0;
            self.pending.clear();
            return Vec::new();
        };
        let Ok(len) = file.metadata().map(|m| m.len()) else {
            return Vec::new();
        };
        if len < self.offset {
            self.offset = 0;
            self.pending.clear();
        }
        if len == self.offset {
            return Vec::new();
        }
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        let mut buf = Vec::new();
        if file.read_to_end(&mut buf).is_err() {
            return Vec::new();
        }
        self.offset += buf.len() as u64;

        // The engine writes whatever encoding the user's console produced, and
        // a partial line at a read boundary is normal. Lossy is right here:
        // this is a signalling channel, not a transcript, and a mangled byte in
        // a player name must never stop a marker being seen.
        self.pending.push_str(&String::from_utf8_lossy(&buf));
        let mut lines: Vec<String> = self.pending.split('\n').map(str::to_string).collect();
        // Last element is either empty (clean boundary) or a partial line.
        self.pending = lines.pop().unwrap_or_default();

        lines.iter().filter_map(|l| parse_marker(l)).collect()
    }

    /// Polls until `cancel` is raised, sending every marker to `tx`.
    ///
    /// Returns when cancelled or when the receiver hangs up. Intended to own a
    /// thread of its own; `capture_engine` reads the channel.
    pub fn run(mut self, tx: Sender<Marker>, cancel: Arc<AtomicBool>) {
        while !cancel.load(Ordering::Relaxed) {
            for marker in self.poll() {
                if tx.send(marker).is_err() {
                    return;
                }
            }
            std::thread::sleep(POLL);
        }
    }
}

fn parse_marker(line: &str) -> Option<Marker> {
    let line = line.trim_end_matches('\r').trim();
    let (_, rest) = line.split_once(LOG_TAG)?;
    let label = rest.trim().to_string();
    if label.is_empty() {
        return None;
    }
    let kind = MarkerKind::parse(&label);
    Some(Marker {
        demo_progress: if kind == MarkerKind::DemoStart { parse_demo_progress(&label) } else { None },
        next_clip_progress: if kind == MarkerKind::NextClip { parse_next_clip_progress(&label) } else { None },
        kind,
        tick: parse_tick(&label),
        label,
    })
}

/// The tick an echo names, e.g. `START_RECORD - Tick 41450`.
fn parse_tick(label: &str) -> Option<i64> {
    let idx = label.find("Tick ")?;
    label[idx + 5..]
        .split(|c: char| !c.is_ascii_digit())
        .find(|s| !s.is_empty())?
        .parse()
        .ok()
}

/// (job_idx, total_jobs, clip_count) from a `DEMO_START` label, e.g.
/// "DEMO_START 5 14 2" -> (5, 14, 2).
fn parse_demo_progress(label: &str) -> Option<(u32, u32, u32)> {
    let mut nums = label
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<u32>().ok());
    Some((nums.next()?, nums.next()?, nums.next()?))
}

/// (job_idx, total_jobs, clip_idx, clip_count) from a `NEXT_CLIP` label, e.g.
/// "NEXT_CLIP 5 14 2 3" -> (5, 14, 2, 3).
fn parse_next_clip_progress(label: &str) -> Option<(u32, u32, u32, u32)> {
    let mut nums = label
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<u32>().ok());
    Some((nums.next()?, nums.next()?, nums.next()?, nums.next()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("dod_logtail_{}_{}", name, std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn parses_the_markers_the_pipeline_actually_emits() {
        // Exactly as they appeared in the measured batch.
        let cases = [
            ("[dod-tools] SPEED_FLUSH - Tick 86371", MarkerKind::SpeedFlush, Some(86371)),
            ("[dod-tools] AUDIO_SYNC - Tick 88267", MarkerKind::AudioSync, Some(88267)),
            ("[dod-tools] START_RECORD - Tick 88741", MarkerKind::StartRecord, Some(88741)),
            ("[dod-tools] STOP_RECORD - Tick 93481", MarkerKind::StopRecord, Some(93481)),
            ("[dod-tools] FAST_FORWARD - Tick 93955", MarkerKind::FastForward, Some(93955)),
            ("[dod-tools] BREADCRUMB - Tick 45000", MarkerKind::Breadcrumb, Some(45000)),
            ("[dod-tools] BATCH_COMPLETE", MarkerKind::BatchComplete, None),
            ("[dod-tools] DEMO_START 5 14 2", MarkerKind::DemoStart, None),
            ("[dod-tools] NEXT_CLIP 5 14 2 3", MarkerKind::NextClip, None),
        ];
        for (line, kind, tick) in cases {
            let m = parse_marker(line).unwrap_or_else(|| panic!("no marker from {:?}", line));
            assert_eq!(m.kind, kind, "kind for {:?}", line);
            assert_eq!(m.tick, tick, "tick for {:?}", line);
        }
    }

    #[test]
    fn demo_start_carries_job_idx_total_and_clip_count() {
        let m = parse_marker("[dod-tools] DEMO_START 5 14 2").unwrap();
        assert_eq!(m.kind, MarkerKind::DemoStart);
        assert_eq!(m.demo_progress, Some((5, 14, 2)));
    }

    /// Only `DemoStart` carries a payload — every other kind's field stays
    /// `None`, even one that happens to be all digits after the tag.
    #[test]
    fn non_demo_start_markers_carry_no_demo_progress() {
        let m = parse_marker("[dod-tools] START_RECORD - Tick 88741").unwrap();
        assert_eq!(m.demo_progress, None);
    }

    #[test]
    fn next_clip_carries_job_idx_total_clip_idx_and_clip_count() {
        let m = parse_marker("[dod-tools] NEXT_CLIP 5 14 2 3").unwrap();
        assert_eq!(m.kind, MarkerKind::NextClip);
        assert_eq!(m.next_clip_progress, Some((5, 14, 2, 3)));
        assert_eq!(m.demo_progress, None, "NextClip must not also carry demo_progress");
    }

    /// The engine prefixes its own text; the tag can sit mid-line.
    #[test]
    fn finds_the_tag_anywhere_in_the_line() {
        let m = parse_marker("some engine noise [dod-tools] START_RECORD - Tick 7").unwrap();
        assert_eq!(m.kind, MarkerKind::StartRecord);
        assert_eq!(m.tick, Some(7));
    }

    #[test]
    fn ignores_lines_without_the_tag() {
        assert!(parse_marker("Server: map changed").is_none());
        assert!(parse_marker("").is_none());
    }

    /// A custom command echo is carried rather than dropped, so it can be
    /// logged — but it must not be mistaken for a stage boundary.
    #[test]
    fn unknown_labels_are_other_not_a_stage() {
        let m = parse_marker("[dod-tools] CUSTOM_CMD1_BEFORE - Tick 41947").unwrap();
        assert_eq!(m.kind, MarkerKind::Other);
        assert_eq!(m.tick, Some(41947));
    }

    /// Starting mid-file is the whole point: an existing log is history, and
    /// replaying it would fire a record on a batch that finished days ago.
    #[test]
    fn starts_at_the_end_of_an_existing_log() {
        let p = temp("history");
        std::fs::write(&p, "[dod-tools] START_RECORD - Tick 1\n").unwrap();
        let mut t = LogTailer::at_end(&p);
        assert!(t.poll().is_empty(), "history must not be replayed");

        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        writeln!(f, "[dod-tools] STOP_RECORD - Tick 2").unwrap();
        f.flush().unwrap();
        let got = t.poll();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, MarkerKind::StopRecord);
        let _ = std::fs::remove_file(&p);
    }

    /// A line split across two reads must not be lost or truncated — the
    /// engine flushes per line, but a read can still land mid-line.
    #[test]
    fn reassembles_a_line_split_across_reads() {
        let p = temp("partial");
        std::fs::write(&p, "").unwrap();
        let mut t = LogTailer::at_end(&p);

        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        write!(f, "[dod-tools] START_RE").unwrap();
        f.flush().unwrap();
        assert!(t.poll().is_empty(), "half a line is not a marker yet");

        writeln!(f, "CORD - Tick 99").unwrap();
        f.flush().unwrap();
        let got = t.poll();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, MarkerKind::StartRecord);
        assert_eq!(got[0].tick, Some(99));
        let _ = std::fs::remove_file(&p);
    }

    /// Cleanup deletes the log between batches. A shrinking file is a new file,
    /// and its contents are live rather than history.
    #[test]
    fn a_truncated_log_is_read_from_the_start() {
        let p = temp("truncate");
        std::fs::write(&p, "[dod-tools] BREADCRUMB - Tick 1\n[dod-tools] BREADCRUMB - Tick 2\n").unwrap();
        let mut t = LogTailer::at_end(&p);
        assert!(t.poll().is_empty());

        std::fs::write(&p, "[dod-tools] START_RECORD - Tick 5\n").unwrap();
        let got = t.poll();
        assert_eq!(got.len(), 1, "content after a truncation is live");
        assert_eq!(got[0].tick, Some(5));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_missing_log_is_not_an_error() {
        let mut t = LogTailer::at_end(&temp("absent"));
        assert!(t.poll().is_empty());
    }
}
