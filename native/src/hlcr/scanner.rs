#![cfg(not(target_arch = "wasm32"))]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use walkdir::WalkDir;

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct ClipData {
    pub take_folder: String,
    pub clip_type: String, // "single" or "hud_only"
    pub img_folder: String,
    /// The wav to mux in, relative to `take_folder`. `None` for an OBS take:
    /// its audio is already muxed into `video_file`, so there is nothing to
    /// mux — `run_render_job` branches on this rather than trusting a wav is
    /// always there.
    pub wav_file: Option<String>,
    pub base_name: String,
    pub frame_count: usize,
    /// Frame resolution — `0` means unknown (e.g. an autosave-recovered stub
    /// that never had a real scan, or a stream whose dimensions couldn't be
    /// read). Used by `run_render_job`'s JIT drive-reservation estimate, not
    /// by the encode itself (FFmpeg reads the actual frame dimensions
    /// straight off the source). `#[serde(default)]` so autosaves written
    /// before this existed keep loading.
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    pub date: String,
    /// The video file inside `img_folder`, when the take was captured through
    /// `mirv_movie_ffmpeg` instead of as a frame sequence. `None` is a BMP take.
    ///
    /// `#[serde(default)]` because render autosaves written before this existed
    /// have to keep loading — a recovered batch must not fail on a field that
    /// was not there when it was saved.
    #[serde(default)]
    pub video_file: Option<String>,
    /// For a `hud_only` clip, the stream folder holding the alpha half of the
    /// pair — `img_folder` is the colour half. `None` on every other clip type.
    ///
    /// The renderer used to derive this by appending the literal `hudalpha`,
    /// which only works while HLAE spells the folder exactly that way. Carrying
    /// the name the scanner actually saw on disk means the renderer is told its
    /// partner rather than guessing it, and is the one piece of the standalone
    /// HLCR's design (see `docs/render_studio_hlcr_parity.md`) that does not
    /// depend on frame counts.
    ///
    /// `#[serde(default)]` for the same reason as `video_file`: autosaves
    /// written before this field existed must keep loading.
    #[serde(default)]
    pub alpha_folder: Option<String>,
}

/// Whether a clip is eligible for "Skip (keep original)" — no FFmpeg pass at
/// all, just a copy of `video_file` into the export pool.
///
/// Shared between the Tauri layer's per-job toggle gate
/// (`desktop-studio/src-tauri/src/render_manager.rs`'s `skip_available`) and
/// `run_render_job`'s own admission check, so the two can never quietly drift
/// apart about which clips qualify — the same reason `is_renderable_take` is
/// shared between the capture and render sides.
pub fn clip_is_skip_eligible(clip: &ClipData) -> bool {
    clip.clip_type != "hud_only" && clip.wav_file.is_none() && clip.video_file.is_some()
}

/// The file `mirv_movie_ffmpeg` is told to write, per stream. Also what the
/// scanner looks for to tell a video take from a frame sequence, so the two
/// cannot drift apart.
pub const VIDEO_FILE: &str = "video.avi";

/// `.wav` files sitting directly in a take folder, sorted case-insensitively
/// so take selection is deterministic.
fn collect_wav_files(take_folder: &Path) -> Vec<String> {
    let mut wav_files = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(take_folder) {
        for sub_entry in read_dir.flatten() {
            if let Ok(file_type) = sub_entry.file_type()
                && file_type.is_file() {
                    let path = sub_entry.path();
                    if let Some(ext) = path.extension()
                        && ext.to_string_lossy().to_lowercase() == "wav"
                            && let Some(name) = path.file_name() {
                                wav_files.push(name.to_string_lossy().into_owned());
                            }
                }
        }
    }
    wav_files.sort_by_key(|a| a.to_lowercase());
    wav_files
}

/// Immediate subdirectories of a take folder that hold one capture stream —
/// either an HLAE frame sequence, identified by its `00000.bmp` first frame, or
/// a single video written by `mirv_movie_ffmpeg`.
///
/// Both land in the same place: `<take0000>/<stream>/`, where `<stream>` is
/// `all`, `hudcolor`, `hudalpha` and so on. Measured against a real capture —
/// FFmpeg mode changes what is inside the stream folder and nothing else about
/// the layout.
fn collect_image_folders(take_folder: &Path) -> Vec<PathBuf> {
    let mut image_folders = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(take_folder) {
        for sub_entry in read_dir.flatten() {
            if let Ok(file_type) = sub_entry.file_type() {
                let path = sub_entry.path();
                if file_type.is_dir()
                    && (path.join("00000.bmp").exists() || stream_video_path(&path).is_some())
                {
                    image_folders.push(path);
                }
            }
        }
    }
    image_folders
}

/// Containers a stream folder's video may be in.
///
/// `mirv_movie_ffmpeg` writes `video.avi` because that is what the pipeline
/// asks it for. OBS writes whatever container the user's OBS settings name —
/// the install this was developed against defaults to `hybrid_mp4` — and its
/// output keeps the extension it was written with rather than being renamed,
/// since a file that lies about its own container breaks every tool that reads
/// it afterwards.
const VIDEO_EXTENSIONS: &[&str] = &["avi", "mp4", "mkv", "mov"];

/// The video inside a stream folder, whatever container it is in.
fn stream_video_path(folder: &Path) -> Option<PathBuf> {
    // `video.avi` first so an HLAE take resolves without touching the disk
    // more than once — it is by far the common case.
    let preferred = folder.join(VIDEO_FILE);
    if preferred.is_file() {
        return Some(preferred);
    }
    VIDEO_EXTENSIONS
        .iter()
        .map(|ext| folder.join(format!("video.{ext}")))
        .find(|p| p.is_file())
}

/// A stream folder's name exactly as it appears on disk. Paths on the clip are
/// resolved relative to the take folder, so the case only has to survive to
/// keep the record honest — but a name the scanner saw beats one reconstructed
/// from a literal, especially with two spellings in play.
fn on_disk_name(folder: &Path) -> String {
    folder
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// How much of an AVI to read looking for its header. The `hdrl` list sits at
/// the very front of the file, so this never has to grow — and it must not, as
/// the videos themselves run to gigabytes.
const AVI_HEADER_SCAN_BYTES: usize = 64 * 1024;

/// Frame count read out of an AVI's own header, without decoding it.
///
/// The take's frame count drives the render progress percentage, and it used to
/// come from counting `.bmp` files — which returns 0 for a video take and left
/// video renders showing no progress at all. The count is in the header, so
/// this costs one bounded read rather than a decode pass or an ffprobe spawn
/// per take.
///
/// **The video stream's `strh.dwLength` is the authority, not
/// `avih.dwTotalFrames`.** AVI's legacy RIFF chunk tops out around 1 GiB, so
/// FFmpeg's muxer continues past that into OpenDML `AVIX` segments — and
/// `avih.dwTotalFrames` then counts only the frames in the *first* chunk, while
/// `strh.dwLength` carries the true total.
///
/// Measured across one capture, which shows the split exactly: the ~1.2 GB and
/// ~1.5 GB `all` streams read 1067/1218 and 859/1229 (avih/strh), and in both
/// cases the first chunk works out to almost exactly 1 GiB of the file. The
/// 135 MB HUD streams fit in a single chunk and agree with themselves. So the
/// error only appears on long or high-fps takes — the ones where an accurate
/// progress bar actually matters. `avih` is kept only as a fallback for a file
/// with no video `strh` at all.
///
/// Zero from both means the file was never finalised — a capture killed
/// mid-write — which is worth surfacing rather than papering over.
fn avi_frame_count(path: &Path) -> Option<usize> {
    use std::io::Read;

    let mut file = std::fs::File::open(path).ok()?;
    // Heap-allocated and explicitly bounded: these files are gigabytes.
    let mut buf = vec![0u8; AVI_HEADER_SCAN_BYTES];
    let read = file.read(&mut buf).ok()?;
    buf.truncate(read);

    if buf.len() < 12 || &buf[0..4] != b"RIFF" || &buf[8..12] != b"AVI " {
        return None;
    }

    let u32_at = |b: &[u8], at: usize| -> Option<u32> {
        b.get(at..at + 4)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    };

    // Walk the chunks inside the RIFF body. `hdrl` is a LIST, so descend into
    // it rather than skipping past; everything wanted lives inside it.
    let mut pos = 12usize;
    let mut total_frames = 0u32;
    let mut stream_length = 0u32;
    while pos + 8 <= buf.len() {
        let id = &buf[pos..pos + 4];
        let size = u32_at(&buf, pos + 4)? as usize;
        let body = pos + 8;

        match id {
            b"LIST" => {
                // Step inside: body starts with the list type, then chunks.
                pos = body + 4;
                continue;
            }
            b"avih" => {
                total_frames = u32_at(&buf, body + 16).unwrap_or(0);
            }
            b"strh"
                // Only the video stream's length is meaningful here; an audio
                // stream's dwLength counts samples or blocks, not frames.
                if buf.get(body..body + 4) == Some(b"vids") && stream_length == 0 => {
                    stream_length = u32_at(&buf, body + 32).unwrap_or(0);
                }
            _ => {}
        }

        // Chunks are word-aligned: an odd size is followed by a pad byte.
        pos = body + size + (size & 1);
    }

    let frames = if stream_length > 0 { stream_length } else { total_frames };
    Some(frames as usize)
}

/// An AVI's frame resolution, read from the video stream's `strf`
/// (`BITMAPINFOHEADER`) chunk — a separate chunk walk from `avi_frame_count`
/// rather than folding into it, so that function's existing return shape and
/// tests stay untouched. `biHeight` is signed and negative for a top-down
/// bitmap, hence the `.abs()`.
fn avi_dimensions(path: &Path) -> Option<(u32, u32)> {
    use std::io::Read;

    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; AVI_HEADER_SCAN_BYTES];
    let read = file.read(&mut buf).ok()?;
    buf.truncate(read);

    if buf.len() < 12 || &buf[0..4] != b"RIFF" || &buf[8..12] != b"AVI " {
        return None;
    }

    let u32_at = |b: &[u8], at: usize| -> Option<u32> {
        b.get(at..at + 4)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    };
    let i32_at = |b: &[u8], at: usize| -> Option<i32> {
        b.get(at..at + 4)
            .map(|s| i32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    };

    let mut pos = 12usize;
    // `strf` describes whatever `strl`/`strh` it follows — this is only ever
    // set true right after a video `strh`, so the very next `strf` this
    // walk sees is guaranteed to be the video stream's, not audio's.
    let mut awaiting_video_strf = false;
    while pos + 8 <= buf.len() {
        let id = &buf[pos..pos + 4];
        let size = u32_at(&buf, pos + 4)? as usize;
        let body = pos + 8;

        match id {
            b"LIST" => {
                pos = body + 4;
                continue;
            }
            b"strh" => {
                awaiting_video_strf = buf.get(body..body + 4) == Some(b"vids");
            }
            b"strf" if awaiting_video_strf => {
                let width = u32_at(&buf, body + 4)?;
                let height = i32_at(&buf, body + 8)?.unsigned_abs();
                return Some((width, height));
            }
            _ => {}
        }

        pos = body + size + (size & 1);
    }
    None
}

/// A BMP frame sequence's resolution, read from the first frame's own
/// 54-byte header (`BITMAPFILEHEADER` + `BITMAPINFOHEADER`) — width/height
/// are little-endian `i32` at offsets 18/22. Same file `get_clip_date`
/// already opens for its metadata, and the same bounded-read shape as
/// `avi_frame_count` above.
fn read_bmp_dimensions(folder: &Path) -> Option<(u32, u32)> {
    use std::io::Read;

    let mut file = std::fs::File::open(folder.join("00000.bmp")).ok()?;
    let mut buf = [0u8; 54];
    file.read_exact(&mut buf).ok()?;

    let i32_at = |at: usize| -> i32 {
        i32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
    };
    let width = i32_at(18);
    let height = i32_at(22);
    if width <= 0 || height == 0 {
        return None;
    }
    Some((width as u32, height.unsigned_abs()))
}

/// A stream folder's frame resolution, whichever shape it was captured in —
/// mirrors `stream_frame_count`'s own AVI-vs-BMP branch. `(0, 0)` means
/// unknown (e.g. an OBS MP4/MKV take, whose container walk isn't worth doing
/// just for this — see `stream_frame_count`'s own reasoning); callers treat
/// that as "fall back to a conservative estimate" rather than a hard error.
fn stream_dimensions(folder: &Path) -> (u32, u32) {
    if let Some(video) = stream_video_path(folder) {
        return avi_dimensions(&video).unwrap_or((0, 0));
    }
    read_bmp_dimensions(folder).unwrap_or((0, 0))
}

/// Frames in a stream folder, whichever shape it was captured in.
fn stream_frame_count(folder: &Path) -> usize {
    if let Some(video) = stream_video_path(folder) {
        // Only AVI carries the count somewhere this cheap to reach. An
        // OBS-written MP4 or MKV keeps it behind a container walk that is not
        // worth doing for a progress bar, so 0 stands — which the UI shows as
        // an indeterminate bar rather than as a wrong number.
        return avi_frame_count(&video).unwrap_or(0);
    }
    count_bmps(folder)
}

/// The video inside a stream folder, when that is what it holds.
fn stream_video(folder: &Path) -> Option<String> {
    stream_video_path(folder)
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
}

/// How much of a container to read looking for an audio track.
///
/// Both ends, because the two formats disagree about where the index lives: an
/// AVI's `hdrl` is at the front, while an MP4 written without `+faststart` —
/// which is every OBS recording — puts `moov` at the end. A window at each end
/// finds both without reading a multi-gigabyte file.
const AUDIO_SCAN_BYTES: usize = 512 * 1024;

/// Whether a video file carries an audio stream.
///
/// Deliberately a byte scan rather than an FFprobe call. This runs inside the
/// scanner, which walks every folder the user has pointed Render Studio at, and
/// spawning a process per candidate take would make a library scan unusable.
///
/// Signature per container:
///
/// - **AVI** — a stream header (`strh`) whose `fccType` is `auds`.
/// - **MP4/MOV** — a handler box (`hdlr`) declaring the `soun` handler type.
/// - **MKV** — the Matroska `TrackType` element (`0x83`) with value 2, audio.
///
/// A false negative means a take is reported unrenderable and the user is told
/// why; a false positive means a silent clip renders as if fine. So when the
/// scan cannot tell, the answer is no.
fn video_has_audio(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};

    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(len) = file.metadata().map(|m| m.len()) else {
        return false;
    };

    let mut windows: Vec<Vec<u8>> = Vec::new();
    let mut head = vec![0u8; AUDIO_SCAN_BYTES.min(len as usize)];
    if file.read_exact(&mut head).is_ok() {
        windows.push(head);
    }
    if len > AUDIO_SCAN_BYTES as u64 {
        let tail_start = len.saturating_sub(AUDIO_SCAN_BYTES as u64);
        if file.seek(SeekFrom::Start(tail_start)).is_ok() {
            let mut tail = Vec::new();
            if file.take(AUDIO_SCAN_BYTES as u64).read_to_end(&mut tail).is_ok() {
                windows.push(tail);
            }
        }
    }

    windows.iter().any(|w| {
        contains_pair(w, b"strh", b"auds")
            || contains_pair(w, b"hdlr", b"soun")
            || contains_mkv_audio_track(w)
    })
}

/// `needle` followed by `tag` within a short distance.
///
/// Both AVI and MP4 place the type immediately after the box name — 4 bytes for
/// `strh`, 8 for `hdlr` after its version/flags — so a small window is enough,
/// and keeping it small is what stops an unrelated occurrence of `soun`
/// elsewhere in the file reading as an audio track.
fn contains_pair(haystack: &[u8], needle: &[u8; 4], tag: &[u8; 4]) -> bool {
    haystack.windows(4).enumerate().any(|(i, w)| {
        if w != needle {
            return false;
        }
        let from = i + 4;
        let to = (from + 16).min(haystack.len());
        haystack[from..to].windows(4).any(|c| c == tag)
    })
}

/// Matroska `TrackType` (element id `0x83`), size 1, value 2 (audio).
fn contains_mkv_audio_track(haystack: &[u8]) -> bool {
    haystack.windows(3).any(|w| w == [0x83, 0x81, 0x02])
}

/// Whether Render Studio's scanner would admit this folder as a renderable take.
///
/// Shared with the capture-side take verification so "the capture succeeded"
/// and "Render Studio can actually see it" can never silently disagree — if
/// this predicate changes, both sides change together.
///
/// HLAE's `mirv_movie` plugin auto-numbers each recording into a `take0000`,
/// `take0001`, ... subfolder under whatever directory `mirv_movie_filename`
/// points at, to avoid overwriting a previous take written to the same path —
/// so the wav/bmp sequence actually lands one level deeper than the folder we
/// asked it to write to. `scan_folder_background` below dodges this for free
/// because `WalkDir` recurses into every subdirectory on its own; this check
/// has to look explicitly since it only tests one specific folder.
pub fn is_renderable_take(take_folder: &Path) -> bool {
    if take_shape_is_renderable(take_folder) {
        return true;
    }
    if let Ok(read_dir) = std::fs::read_dir(take_folder) {
        for entry in read_dir.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
                && entry.file_name().to_string_lossy().to_lowercase().starts_with("take")
                && take_shape_is_renderable(&entry.path()) {
                    return true;
                }
        }
    }
    false
}

/// The two admissible shapes, checked against one specific folder.
///
/// 1. **Audio beside a stream folder.** How HLAE writes a take, whether that
///    stream folder holds a BMP sequence or a `mirv_movie_ffmpeg` video: the
///    audio is always a separate `sound.wav` that the render pass muxes in.
/// 2. **A stream folder holding a video that already contains its audio.** How
///    an OBS take arrives, because OBS records A/V together and there is no
///    wav to find.
///
/// The second case tests for the audio *stream*, not merely for a video file.
/// A muted OBS source produces a video that is valid in every respect and
/// silent, and admitting it would mean the render pass emits a clip with no
/// sound and no error — exactly the plausible-looking-but-wrong output this
/// pipeline keeps being bitten by. The check is cheap because the answer is in
/// the container header.
fn take_shape_is_renderable(folder: &Path) -> bool {
    let streams = collect_image_folders(folder);
    if streams.is_empty() {
        return false;
    }
    if !collect_wav_files(folder).is_empty() {
        return true;
    }
    streams
        .iter()
        .filter_map(|s| stream_video_path(s))
        .any(|v| video_has_audio(&v))
}

pub fn scan_folder_background(
    source_folders: Vec<PathBuf>,
    tx: mpsc::Sender<ClipData>,
    status_tx: mpsc::Sender<String>,
) -> usize {
    let mut count = 0;
    let mut processed_folders = HashSet::new();
    let mut accumulated_clips = Vec::new();

    for source_folder in source_folders {
        if !source_folder.exists() || !source_folder.is_dir() {
            continue;
        }

        for entry in WalkDir::new(&source_folder)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_dir() {
                continue;
            }

            let take_folder = entry.path().to_path_buf();
            if processed_folders.contains(&take_folder) {
                continue;
            }

            let mut image_folders = collect_image_folders(&take_folder);
            if image_folders.is_empty() {
                continue;
            }
            // Shared with the capture-side take-verification predicate so the
            // two can never silently disagree about what counts as a take —
            // admits both the wav-beside-a-stream shape and an OBS take
            // (a stream folder whose video already carries its own audio).
            if !take_shape_is_renderable(&take_folder) {
                continue;
            }
            let wav_files = collect_wav_files(&take_folder);

            // Valid take found!
            processed_folders.insert(take_folder.clone());
            let _ = status_tx.send(format!("Found take: {}", take_folder.file_name().unwrap_or_default().to_string_lossy()));

            // Prioritize sound.wav if it exists. `None` means an OBS take —
            // its audio is already muxed into the video, so there is no wav
            // to pick among.
            let sound_wav_exists = wav_files.iter().any(|f| f.to_lowercase() == "sound.wav");
            let wav_to_use = if sound_wav_exists {
                Some("sound.wav".to_string())
            } else {
                wav_files.first().cloned()
            };

            let take_name = take_folder.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let demo_name = take_folder.parent()
                .and_then(|p| p.file_name())
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();

            let base_name = match &wav_to_use {
                Some(wav) => {
                    let wav_stem = Path::new(wav).file_stem().unwrap_or_default().to_string_lossy().into_owned();
                    if wav_stem.to_lowercase() == "sound" {
                        format!("{}-{}-{}", demo_name, take_name, wav_stem)
                    } else {
                        wav_stem
                    }
                }
                None => format!("{}-{}-obs", demo_name, take_name),
            };

            // Keyed lowercase. The BMP path produces `all`/`hudcolor`/`hudalpha`,
            // but HLAE names those same streams `hudColor`/`hudAlpha` in the
            // `mirv_movie_ffmpeg` command, and nothing here has proven which
            // spelling FFmpeg mode uses for the folder. An exact-case lookup
            // that guessed wrong would not error — it would quietly skip the
            // HUD bundling and emit two unrelated clips instead of one merged
            // render, which is the kind of wrong output this pipeline is bad at
            // noticing. Windows resolves the paths case-insensitively either
            // way, so only this lookup was ever at risk.
            //
            // `docs/render_studio_hlcr_parity.md` notes the standalone HLCR
            // pairs folders generically instead — any `alpha`/`mask` folder with
            // a same-frame-count `color`/`rgb` one — which is more robust than
            // literal names and remains the better long-term shape.
            let folder_names: HashMap<String, PathBuf> = image_folders.iter()
                .map(|p| {
                    let name = p.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
                    (name, p.clone())
                })
                .collect();

            // Bundle HLAE split streams if "all", "hudcolor", and "hudalpha" exist
            if folder_names.contains_key("all") && folder_names.contains_key("hudcolor") && folder_names.contains_key("hudalpha") {
                let all_folder = folder_names.get("all").unwrap();
                let hud_color_folder = folder_names.get("hudcolor").unwrap();
                let hud_alpha_folder = folder_names.get("hudalpha").unwrap();
                let frame_count = stream_frame_count(all_folder);
                let (width, height) = stream_dimensions(all_folder);
                let date = get_clip_date(all_folder);

                // `take_shape_is_renderable` above only asked whether *some*
                // stream in this take has audio, which is enough to admit the
                // take but not enough to admit `all` specifically — the same
                // reasoning the "process remaining folders" loop below applies
                // per stream. And a HUD/alpha composite always needs a real
                // wav regardless: the alpha stream is a mask, never a mix, so
                // `run_render_job` rejects `hud_only` with `wav_file: None`
                // outright — bundling one without a wav would only queue a
                // job that is guaranteed to fail at render time.
                let all_has_audio = wav_to_use.is_some()
                    || stream_video_path(all_folder).map(|v| video_has_audio(&v)).unwrap_or(false);

                if all_has_audio {
                    let clip_all = ClipData {
                        take_folder: take_folder.to_string_lossy().into_owned(),
                        clip_type: "single".to_string(),
                        img_folder: on_disk_name(all_folder),
                        wav_file: wav_to_use.clone(),
                        base_name: base_name.clone(),
                        frame_count,
                        width,
                        height,
                        date: date.clone(),
                        video_file: stream_video(all_folder),
                        alpha_folder: None,
                    };
                    accumulated_clips.push(clip_all);

                    if wav_to_use.is_some() {
                        let clip_hud = ClipData {
                            take_folder: take_folder.to_string_lossy().into_owned(),
                            clip_type: "hud_only".to_string(),
                            // Both halves of the pair are named from what is actually on
                            // disk, so the renderer never has to reconstruct either one
                            // from a literal. The lookup keys above are lowercased for
                            // matching only — HLAE writes `hudColor`/`hudAlpha` in the
                            // `mirv_movie_ffmpeg` command, and these carry whichever
                            // spelling the capture produced.
                            img_folder: on_disk_name(hud_color_folder),
                            wav_file: wav_to_use.clone(),
                            base_name: base_name.clone(),
                            frame_count,
                            width,
                            height,
                            date: date.clone(),
                            video_file: stream_video(hud_color_folder),
                            alpha_folder: Some(on_disk_name(hud_alpha_folder)),
                        };
                        accumulated_clips.push(clip_hud);
                    }
                }

                // Remove bundled folders from list to avoid double-processing
                image_folders.retain(|p| {
                    let name = p.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
                    name != "all" && name != "hudcolor" && name != "hudalpha"
                });
            }

            // Process remaining folders
            for img_folder in image_folders {
                // `take_shape_is_renderable` above only asked whether *some*
                // stream in this take has audio — enough to admit the take,
                // not enough to admit every stream in it. With no take-level
                // wav, a stream whose own video is silent must still be
                // skipped individually, or it would render with no audio and
                // no error: exactly what `video_has_audio` exists to prevent
                // for a lone OBS take.
                if wav_to_use.is_none() {
                    let has_own_audio = stream_video_path(&img_folder)
                        .map(|v| video_has_audio(&v))
                        .unwrap_or(false);
                    if !has_own_audio {
                        continue;
                    }
                }
                let frame_count = stream_frame_count(&img_folder);
                let (width, height) = stream_dimensions(&img_folder);
                let folder_name = img_folder.file_name().unwrap_or_default().to_string_lossy().into_owned();
                let date = get_clip_date(&img_folder);
                let clip = ClipData {
                    take_folder: take_folder.to_string_lossy().into_owned(),
                    clip_type: "single".to_string(),
                    img_folder: folder_name,
                    wav_file: wav_to_use.clone(),
                    base_name: base_name.clone(),
                    frame_count,
                    width,
                    height,
                    date,
                    video_file: stream_video(&img_folder),
                    alpha_folder: None,
                };
                accumulated_clips.push(clip);
            }
        }
    }

    // Deterministic sorting
    accumulated_clips.sort_by(|a, b| {
        a.take_folder.cmp(&b.take_folder)
            .then_with(|| a.img_folder.cmp(&b.img_folder))
            .then_with(|| a.clip_type.cmp(&b.clip_type))
    });

    for clip in accumulated_clips {
        let _ = tx.send(clip);
        count += 1;
    }

    count
}

fn count_bmps(folder: &Path) -> usize {
    let mut count = 0;
    if let Ok(read_dir) = std::fs::read_dir(folder) {
        for entry in read_dir.flatten() {
            if let Ok(file_type) = entry.file_type()
                && file_type.is_file() {
                    let path = entry.path();
                    if let Some(ext) = path.extension()
                        && ext.to_string_lossy().to_lowercase() == "bmp" {
                            count += 1;
                        }
                }
        }
    }
    count
}

fn get_clip_date(img_folder_path: &Path) -> String {
    let bmp_path = img_folder_path.join("00000.bmp");
    if let Ok(metadata) = std::fs::metadata(&bmp_path).or_else(|_| std::fs::metadata(img_folder_path))
        && let Ok(created) = metadata.created().or_else(|_| metadata.modified()) {
            return chrono::DateTime::<chrono::Local>::from(created)
                .format("%Y-%m-%d %I:%M %p")
                .to_string();
        }
    "-".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dod_scanner_test_{}", name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("failed to create scratch dir");
        dir
    }

    fn write_frames(take: &Path, stream: &str) {
        let folder = take.join(stream);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("00000.bmp"), b"bmp").unwrap();
    }

    #[test]
    fn test_renderable_take_needs_both_wav_and_frames() {
        let take = scratch_dir("complete");
        std::fs::write(take.join("sound.wav"), b"wav").unwrap();
        write_frames(&take, "all");
        assert!(is_renderable_take(&take));
    }

    #[test]
    fn test_take_without_wav_is_not_renderable() {
        // The realistic partial-capture case: frames landed, audio never flushed.
        let take = scratch_dir("no_wav");
        write_frames(&take, "all");
        assert!(!is_renderable_take(&take));
    }

    #[test]
    fn test_take_without_frames_is_not_renderable() {
        let take = scratch_dir("no_frames");
        std::fs::write(take.join("sound.wav"), b"wav").unwrap();
        assert!(!is_renderable_take(&take));
    }

    #[test]
    fn test_subfolder_without_first_frame_does_not_count() {
        // A frame folder is identified by 00000.bmp specifically — an empty or
        // partially-written stream folder must not qualify.
        let take = scratch_dir("empty_stream");
        std::fs::write(take.join("sound.wav"), b"wav").unwrap();
        std::fs::create_dir_all(take.join("all")).unwrap();
        assert!(!is_renderable_take(&take));
    }

    #[test]
    fn test_missing_take_folder_is_not_renderable() {
        let missing = std::env::temp_dir().join("dod_scanner_test_does_not_exist");
        let _ = std::fs::remove_dir_all(&missing);
        assert!(!is_renderable_take(&missing));
    }

    /// A minimal but structurally real AVI header: RIFF/hdrl/avih plus an
    /// optional strl/strh video stream header. Only the two frame-count fields
    /// carry meaningful values; the rest is zero padding of the right width.
    fn build_avi(avih_total_frames: u32, vids_stream_length: Option<u32>) -> Vec<u8> {
        fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(id);
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
            out.extend_from_slice(body);
            if body.len() % 2 == 1 {
                out.push(0); // chunks are word-aligned
            }
            out
        }

        let mut avih = vec![0u8; 56];
        avih[16..20].copy_from_slice(&avih_total_frames.to_le_bytes());

        let mut hdrl = b"hdrl".to_vec();
        hdrl.extend_from_slice(&chunk(b"avih", &avih));

        if let Some(len) = vids_stream_length {
            let mut strh = vec![0u8; 56];
            strh[0..4].copy_from_slice(b"vids");
            strh[32..36].copy_from_slice(&len.to_le_bytes());
            let mut strl = b"strl".to_vec();
            strl.extend_from_slice(&chunk(b"strh", &strh));
            hdrl.extend_from_slice(&chunk(b"LIST", &strl));
        }

        let mut body = b"AVI ".to_vec();
        body.extend_from_slice(&chunk(b"LIST", &hdrl));
        chunk(b"RIFF", &body)
    }

    fn write_video(take: &Path, stream: &str, avi: &[u8]) {
        let folder = take.join(stream);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(VIDEO_FILE), avi).unwrap();
    }

    fn scratch(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("dod_scan_{}_{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// A stream folder holding a video with the given bytes, named `video.<ext>`.
    fn write_named_video(take: &Path, stream: &str, ext: &str, body: &[u8]) -> PathBuf {
        let folder = take.join(stream);
        std::fs::create_dir_all(&folder).unwrap();
        let p = folder.join(format!("video.{ext}"));
        std::fs::write(&p, body).unwrap();
        p
    }

    /// Enough of an MP4 to carry the audio-handler signature the scan looks
    /// for: an `hdlr` box declaring `soun`, padded the way a real one is.
    fn mp4_with_audio() -> Vec<u8> {
        let mut v = b"\x00\x00\x00\x20ftypisom".to_vec();
        v.extend_from_slice(b"hdlr");
        v.extend_from_slice(&[0u8; 8]); // version/flags + pre_defined
        v.extend_from_slice(b"soun");
        v.extend_from_slice(&[0u8; 64]);
        v
    }

    fn mp4_without_audio() -> Vec<u8> {
        let mut v = b"\x00\x00\x00\x20ftypisom".to_vec();
        v.extend_from_slice(b"hdlr");
        v.extend_from_slice(&[0u8; 8]);
        v.extend_from_slice(b"vide");
        v.extend_from_slice(&[0u8; 64]);
        v
    }

    /// The OBS shape: one stream folder, a video with its audio already in it,
    /// and no `sound.wav` anywhere. This is the case the predicate was widened
    /// for, and both the capture side and Render Studio go through it.
    #[test]
    fn a_video_take_with_audio_is_renderable_without_a_wav() {
        let root = scratch("obs_take");
        let take = root.join("chain_01_b0").join("take0000");
        write_named_video(&take, "all", "mp4", &mp4_with_audio());
        assert!(collect_wav_files(&take).is_empty(), "no wav, by construction");
        assert!(is_renderable_take(&take));
        // And through the take* nesting, which is how the capture side asks.
        assert!(is_renderable_take(&root.join("chain_01_b0")));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A muted OBS source produces a video that is valid in every respect and
    /// silent. Admitting it would render a clip with no sound and no error.
    #[test]
    fn a_silent_video_take_is_not_renderable() {
        let root = scratch("silent");
        let take = root.join("chain_01_b0").join("take0000");
        write_named_video(&take, "all", "mp4", &mp4_without_audio());
        assert!(
            !is_renderable_take(&take),
            "a video with no audio track and no wav must not pass"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// OBS writes whatever container its settings name, and the file keeps the
    /// extension it was written with rather than being renamed to `.avi`.
    #[test]
    fn video_takes_are_found_in_any_supported_container() {
        for ext in ["mp4", "mkv", "mov", "avi"] {
            let root = scratch(&format!("ext_{ext}"));
            let take = root.join("b0").join("take0000");
            write_named_video(&take, "all", ext, &mp4_with_audio());
            assert!(
                !collect_image_folders(&take).is_empty(),
                "a .{ext} video should mark its folder as a stream"
            );
            assert!(is_renderable_take(&take), ".{ext} take should be renderable");
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    /// The wav path must keep working exactly as before — a BMP sequence beside
    /// a `sound.wav` is still the common case and is not audited for audio.
    #[test]
    fn the_wav_shape_still_passes_untouched() {
        let root = scratch("wav");
        let take = root.join("b0").join("take0000");
        let stream = take.join("all");
        std::fs::create_dir_all(&stream).unwrap();
        std::fs::write(stream.join("00000.bmp"), b"x").unwrap();
        std::fs::write(take.join("sound.wav"), b"x").unwrap();
        assert!(is_renderable_take(&take));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An empty folder, or one holding only our own metadata, is not a take.
    #[test]
    fn an_empty_folder_is_not_renderable() {
        let root = scratch("empty");
        assert!(!is_renderable_take(&root));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The audio signature must not be matched by an unrelated occurrence of
    /// the tag somewhere in the payload — hence the bounded window.
    #[test]
    fn a_stray_soun_in_the_payload_is_not_an_audio_track() {
        let root = scratch("stray");
        let take = root.join("b0").join("take0000");
        let mut body = b"\x00\x00\x00\x20ftypisom".to_vec();
        body.extend_from_slice(b"hdlr");
        body.extend_from_slice(&[0u8; 8]);
        body.extend_from_slice(b"vide");
        body.extend_from_slice(&[0u8; 256]); // well past the window
        body.extend_from_slice(b"soun");
        let p = write_named_video(&take, "all", "mp4", &body);
        assert!(
            !video_has_audio(&p),
            "`soun` far from any hdlr is not an audio handler"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_stream_header_length_wins_over_avih_total_frames() {
        // Measured on a real capture: FFmpeg's AVI muxer wrote 1067 into
        // avih.dwTotalFrames while the video strh.dwLength read 1218, and 1218
        // is what ffprobe and the file's own 10.15s-at-120fps duration agree
        // on. Preferring avih would silently under-report render progress.
        let avi = build_avi(1067, Some(1218));
        let dir = scratch_dir("avi_disagreement");
        let path = dir.join(VIDEO_FILE);
        std::fs::write(&path, &avi).unwrap();
        assert_eq!(avi_frame_count(&path), Some(1218));
    }

    #[test]
    fn test_avih_used_when_there_is_no_video_stream_header() {
        let avi = build_avi(900, None);
        let dir = scratch_dir("avi_no_strh");
        let path = dir.join(VIDEO_FILE);
        std::fs::write(&path, &avi).unwrap();
        assert_eq!(avi_frame_count(&path), Some(900));
    }

    #[test]
    fn test_unfinalised_avi_reports_zero_rather_than_guessing() {
        // A capture killed mid-write leaves both counts at 0. Zero is the
        // honest answer — the renderer already treats it as "no percentage".
        let avi = build_avi(0, Some(0));
        let dir = scratch_dir("avi_unfinalised");
        let path = dir.join(VIDEO_FILE);
        std::fs::write(&path, &avi).unwrap();
        assert_eq!(avi_frame_count(&path), Some(0));
    }

    #[test]
    fn test_non_avi_file_is_not_parsed_as_one() {
        let dir = scratch_dir("avi_garbage");
        let path = dir.join(VIDEO_FILE);
        std::fs::write(&path, b"this is not a RIFF file at all").unwrap();
        assert_eq!(avi_frame_count(&path), None);
    }

    #[test]
    fn test_video_take_reports_frames_from_the_container() {
        // The end-to-end shape: a video take used to scan as 0 frames, which
        // left Render Studio showing no progress percentage for it.
        let take = scratch_dir("video_frame_count");
        std::fs::write(take.join("sound.wav"), b"wav").unwrap();
        write_video(&take, "all", &build_avi(0, Some(1218)));

        let clips = scan(&take);
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].frame_count, 1218);
        assert_eq!(clips[0].video_file.as_deref(), Some(VIDEO_FILE));
    }

    #[test]
    fn test_bmp_take_still_counts_bitmaps() {
        let take = scratch_dir("bmp_frame_count");
        std::fs::write(take.join("sound.wav"), b"wav").unwrap();
        let folder = take.join("all");
        std::fs::create_dir_all(&folder).unwrap();
        for i in 0..3 {
            std::fs::write(folder.join(format!("{:05}.bmp", i)), b"bmp").unwrap();
        }

        let clips = scan(&take);
        assert_eq!(clips[0].frame_count, 3);
    }

    /// Collect what `scan_folder_background` emits for a scratch tree.
    fn scan(root: &Path) -> Vec<ClipData> {
        let (tx, rx) = mpsc::channel();
        let (status_tx, _status_rx) = mpsc::channel();
        scan_folder_background(vec![root.to_path_buf()], tx, status_tx);
        rx.into_iter().collect()
    }

    #[test]
    fn test_hud_pair_carries_both_folder_names_as_written() {
        // HLAE names these streams `hudColor`/`hudAlpha` in the
        // `mirv_movie_ffmpeg` command. The pair must still be recognised (the
        // lookup keys are lowercased), and both halves must be reported with
        // the spelling that is actually on disk rather than a literal.
        let take = scratch_dir("hud_pair_case");
        std::fs::write(take.join("sound.wav"), b"wav").unwrap();
        write_frames(&take, "all");
        write_frames(&take, "hudColor");
        write_frames(&take, "hudAlpha");

        let clips = scan(&take);
        let hud = clips
            .iter()
            .find(|c| c.clip_type == "hud_only")
            .expect("hud pair was not bundled");
        assert_eq!(hud.img_folder, "hudColor");
        assert_eq!(hud.alpha_folder.as_deref(), Some("hudAlpha"));

        // Bundling consumed all three folders, so nothing is emitted twice.
        assert_eq!(clips.len(), 2, "expected exactly the all + hud pair");
        let all = clips
            .iter()
            .find(|c| c.clip_type == "single")
            .expect("all stream missing");
        assert_eq!(all.img_folder, "all");
        assert_eq!(all.alpha_folder, None);
    }

    #[test]
    fn test_video_take_hud_pair_names_the_video_in_both_halves() {
        // The FFmpeg-capture shape: one video per stream folder instead of a
        // numbered sequence. `alpha_folder` is what lets the renderer find the
        // second video without reconstructing the folder name.
        let take = scratch_dir("hud_pair_video");
        std::fs::write(take.join("sound.wav"), b"wav").unwrap();
        for stream in ["all", "hudcolor", "hudalpha"] {
            let folder = take.join(stream);
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join(VIDEO_FILE), b"avi").unwrap();
        }

        let clips = scan(&take);
        let hud = clips
            .iter()
            .find(|c| c.clip_type == "hud_only")
            .expect("hud pair was not bundled");
        assert_eq!(hud.video_file.as_deref(), Some(VIDEO_FILE));
        assert_eq!(hud.alpha_folder.as_deref(), Some("hudalpha"));
    }

    #[test]
    fn test_non_hud_clip_has_no_alpha_partner() {
        let take = scratch_dir("solo_stream");
        std::fs::write(take.join("sound.wav"), b"wav").unwrap();
        write_frames(&take, "all");

        let clips = scan(&take);
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].alpha_folder, None);
    }

    #[test]
    fn test_renderable_take_nested_under_hlae_take_number_folder() {
        // HLAE's mirv_movie plugin auto-numbers each recording into a
        // take0000, take0001, ... subfolder under the directory we point
        // mirv_movie_filename at, to avoid overwriting a previous take
        // written to the same path — confirmed on a real capture where the
        // block folder itself was empty except for exactly this layout.
        let block_folder = scratch_dir("nested_under_take_number");
        let take0000 = block_folder.join("take0000");
        std::fs::create_dir_all(&take0000).unwrap();
        std::fs::write(take0000.join("sound.wav"), b"wav").unwrap();
        write_frames(&take0000, "all");
        assert!(is_renderable_take(&block_folder));
    }

    /// The gap issue #82 exists to close: an OBS take — no `sound.wav`, its
    /// audio already muxed into the video — used to be silently dropped by
    /// the scanner's hard `wav_files.is_empty()` gate even though
    /// `is_renderable_take` already recognised the shape.
    #[test]
    fn scan_finds_an_obs_take_with_no_wav() {
        let root = scratch("obs_scan");
        let take = root.join("chain_01_b0").join("take0000");
        write_named_video(&take, "all", "mp4", &mp4_with_audio());

        let clips = scan(&take);
        assert_eq!(clips.len(), 1, "an OBS-shaped take must be scanned into exactly one clip");
        assert_eq!(clips[0].wav_file, None, "no wav exists for an OBS take to name");
        assert_eq!(clips[0].video_file.as_deref(), Some("video.mp4"));
        assert_eq!(clips[0].img_folder, "all");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Without a wav to derive a name from, the base name still has to be
    /// distinctive per take — demo/take, same as the wav path's own fallback
    /// for a literal `sound.wav`, plus a marker so it reads as an OBS clip.
    #[test]
    fn scan_names_an_obs_take_from_demo_and_take_not_a_wav() {
        let root = scratch("obs_naming");
        let take = root.join("some_demo").join("chain_02_b1").join("take0000");
        write_named_video(&take, "all", "mp4", &mp4_with_audio());

        let clips = scan(&take);
        assert_eq!(clips.len(), 1);
        assert!(clips[0].base_name.contains("chain_02_b1"), "{}", clips[0].base_name);
        assert!(clips[0].base_name.ends_with("-obs"), "{}", clips[0].base_name);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A muted OBS source must stay invisible to the scanner too, not just to
    /// `is_renderable_take` — otherwise the scanner would queue a render job
    /// for a clip with no audio and no error.
    #[test]
    fn scan_does_not_find_a_silent_obs_take() {
        let root = scratch("obs_silent_scan");
        let take = root.join("chain_01_b0").join("take0000");
        write_named_video(&take, "all", "mp4", &mp4_without_audio());

        let clips = scan(&take);
        assert!(clips.is_empty(), "a silent video with no wav must not become a render job");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The wav-and-frames shape must still scan to `Some(wav)`, unaffected by
    /// `ClipData.wav_file` becoming optional.
    #[test]
    fn scan_still_carries_the_wav_name_for_the_ordinary_shape() {
        let take = scratch_dir("wav_shape_still_some");
        std::fs::write(take.join("sound.wav"), b"wav").unwrap();
        write_frames(&take, "all");

        let clips = scan(&take);
        assert_eq!(clips[0].wav_file.as_deref(), Some("sound.wav"));
    }

    /// `take_shape_is_renderable` only asks whether *some* stream in the take
    /// has audio, which is enough to admit the take as a whole — it is not
    /// enough to admit every stream inside it. With no take-level wav, a
    /// stream whose own video is silent must be skipped individually rather
    /// than scanned with `wav_file: None`, or it would render with no audio
    /// and no error.
    #[test]
    fn a_silent_stream_beside_an_audible_one_is_not_scanned_even_though_the_take_is() {
        let root = scratch("mixed_streams");
        let take = root.join("chain_01_b0").join("take0000");
        write_named_video(&take, "all", "mp4", &mp4_with_audio());
        write_named_video(&take, "second", "mp4", &mp4_without_audio());

        let clips = scan(&take);
        assert_eq!(clips.len(), 1, "only the audible stream should be scanned: {:?}", clips.iter().map(|c| &c.img_folder).collect::<Vec<_>>());
        assert_eq!(clips[0].img_folder, "all");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The HUD-bundling branch must apply the same per-stream audio rule the
    /// "process remaining folders" loop does — a silent `all` stream must not
    /// be admitted just because bundling happened to trigger, and a HUD/alpha
    /// composite (which never carries its own audio) must not be scanned at
    /// all without a real wav, since `run_render_job` always rejects it.
    #[test]
    fn a_hud_bundle_with_no_wav_and_a_silent_all_stream_is_not_scanned() {
        let root = scratch("hud_bundle_silent_all");
        let take = root.join("chain_01_b0").join("take0000");
        // "all" is silent, but some other stream carries audio, so the take
        // as a whole still passes `take_shape_is_renderable`.
        write_named_video(&take, "all", "mp4", &mp4_without_audio());
        write_named_video(&take, "hudcolor", "mp4", &mp4_with_audio());
        write_named_video(&take, "hudalpha", "mp4", &mp4_without_audio());

        let clips = scan(&take);
        assert!(clips.is_empty(), "a silent `all` stream must not be bundled just because the take passes overall: {:?}", clips.iter().map(|c| &c.img_folder).collect::<Vec<_>>());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An audible `all` stream with no take-level wav can still be admitted
    /// as the plain "single" clip — but the paired `hud_only` clip must not
    /// be, since HUD compositing always needs a real wav and would only ever
    /// fail at render time.
    #[test]
    fn a_hud_bundle_with_no_wav_admits_all_but_not_the_hud_pair() {
        let root = scratch("hud_bundle_no_wav");
        let take = root.join("chain_01_b0").join("take0000");
        write_named_video(&take, "all", "mp4", &mp4_with_audio());
        write_named_video(&take, "hudcolor", "mp4", &mp4_without_audio());
        write_named_video(&take, "hudalpha", "mp4", &mp4_without_audio());

        let clips = scan(&take);
        assert_eq!(clips.len(), 1, "expected only the `all` single clip: {:?}", clips.iter().map(|c| (&c.img_folder, &c.clip_type)).collect::<Vec<_>>());
        assert_eq!(clips[0].clip_type, "single");
        assert_eq!(clips[0].img_folder, "all");
        let _ = std::fs::remove_dir_all(&root);
    }
}
