#![cfg(not(target_arch = "wasm32"))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use tokio::process::Command;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};


use super::config::RenderConfig;
use super::scanner::ClipData;

#[derive(Clone, Debug)]
pub enum RenderUpdate {
    Progress(String, u32),     // (job_id, percentage)
    Speed(String, String),     // (job_id, speed_text)
    Status(String, String),    // (job_id, status_text)
    OutputPath(String, String), // (job_id, absolute path to the encoded file)
    Finished(String, bool, Option<String>), // (job_id, success, error_log)
}

/// Safe, deliberately conservative upper bound on the bytes a render job
/// will need on its export drive — used only to size the JIT reservation in
/// `run_render_job` below, never to predict the real (compressed) output.
/// Raw, uncompressed frame-sequence size (mirrors the AOT capture
/// allocator's own estimate, `patch::builder::block_estimates`): every codec
/// this app renders to compresses well below this in practice, so it's
/// loose but always safe. Falls back to a fixed conservative figure when a
/// clip's resolution is unknown (`ClipData::width`/`height` are `0` — e.g.
/// an autosave-recovered stub, or a take whose container isn't cheap to
/// read dimensions from) rather than reserving nothing for it.
fn estimate_reservation_bytes(clip: &ClipData) -> u64 {
    const UNKNOWN_RESOLUTION_FALLBACK_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB
    if clip.width == 0 || clip.height == 0 {
        return UNKNOWN_RESOLUTION_FALLBACK_BYTES;
    }
    clip.width as u64 * clip.height as u64 * 3 * clip.frame_count as u64
}

/// Best-effort required-bytes estimate for one job, whatever its status —
/// exact for a SourceCopy job (the source file already exists on disk, so
/// this just measures it), the same conservative raw-frame estimate as
/// `estimate_reservation_bytes` above for everything else. Shared by
/// `run_render_job`'s own JIT drive routing (only ever called there for a
/// job about to start) and the Render footer's whole-queue "Required
/// (Estimated)" figure (called for every Queued/Rendering job, not just the
/// one about to run).
pub fn job_reservation_estimate(clip: &ClipData, is_source_copy: bool) -> u64 {
    if is_source_copy {
        let take_folder = PathBuf::from(&clip.take_folder);
        let video_name = clip.video_file.as_deref().unwrap_or_default();
        std::fs::metadata(take_folder.join(&clip.img_folder).join(video_name))
            .map(|m| m.len())
            .unwrap_or_else(|_| estimate_reservation_bytes(clip))
    } else {
        estimate_reservation_bytes(clip)
    }
}

/// RAII release for a JIT export-drive reservation claimed in
/// `run_render_job`'s routing block below. `run_render_job` has many early
/// returns after a drive is selected (success, error, cancellation) —
/// releasing on `Drop` means every one of them releases correctly, including
/// any added later, without a release call having to be hand-placed at each.
struct ReservationGuard {
    ledger: Arc<Mutex<HashMap<PathBuf, u64>>>,
    dir: PathBuf,
    amount: u64,
    // For the matching [render-reservation] claim log below — a future
    // "render says it can't find space but the drive looks fine" report is
    // the difference between guessing and reading exactly what the ledger
    // saw, at the cost of one log line per job.
    job_id: String,
}

impl Drop for ReservationGuard {
    fn drop(&mut self) {
        let mut ledger = self.ledger.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(reserved) = ledger.get_mut(&self.dir) {
            *reserved = reserved.saturating_sub(self.amount);
        }
        let remaining = ledger.get(&self.dir).copied().unwrap_or(0);
        crate::log_markdown(&format!(
            "[render-reservation] job {} released {:.2} GB on {:?} ({:.2} GB still reserved by other jobs)",
            self.job_id,
            self.amount as f64 / (1024.0 * 1024.0 * 1024.0),
            self.dir,
            remaining as f64 / (1024.0 * 1024.0 * 1024.0),
        ));
    }
}

pub async fn run_render_job(
    job_id: String,
    clip: ClipData,
    config: RenderConfig,
    tx: mpsc::Sender<RenderUpdate>,
    cancel_rx: Arc<AtomicBool>,
    reservations: Arc<Mutex<HashMap<PathBuf, u64>>>,
) {
    // "Skip" — leave an OBS take as OBS wrote it. No FFmpeg involved at all,
    // just a copy into the export pool with the pipeline's naming applied.
    let is_source_copy = config.target_codec == super::config::RenderCodec::SourceCopy;
    let ffmpeg_path = PathBuf::from(&config.ffmpeg_path);
    let fps = config.fps.to_string();

    let take_folder = PathBuf::from(&clip.take_folder);
    // `None` is an OBS take: its audio is already muxed into `video_file`,
    // so there is nothing to join against a wav.
    let wav_file: Option<PathBuf> = clip.wav_file.as_ref().map(|w| take_folder.join(w));
    let is_hud = clip.clip_type == "hud_only";

    let is_global = config.ffmpeg_path == "ffmpeg";

    // Initial validations. Skip mode never spawns FFmpeg, so a misconfigured
    // FFmpeg path must not block a plain file copy.
    if !is_source_copy && !is_global && (!ffmpeg_path.exists() || !ffmpeg_path.is_file()) {
        let _ = tx.send(RenderUpdate::Finished(
            job_id.clone(),
            false,
            Some(format!("FFmpeg not found at: {}", ffmpeg_path.display())),
        ));
        return;
    }
    if !take_folder.exists() || !take_folder.is_dir() {
        let _ = tx.send(RenderUpdate::Finished(
            job_id.clone(),
            false,
            Some(format!("Take folder not found: {}", take_folder.display())),
        ));
        return;
    }
    if let Some(wav) = &wav_file
        && !wav.exists() {
            let _ = tx.send(RenderUpdate::Finished(
                job_id.clone(),
                false,
                Some(format!("Audio file not found: {}", wav.display())),
            ));
            return;
        }
    // The alpha stream carries no sound of its own, so a HUD/alpha composite
    // always needs a separate wav — an OBS take's muxed-in audio can't cover it.
    if is_hud && wav_file.is_none() {
        let _ = tx.send(RenderUpdate::Finished(
            job_id.clone(),
            false,
            Some("HUD/alpha clips need a separate audio track (sound.wav), but this take has none.".to_string()),
        ));
        return;
    }
    if clip.video_file.is_none() && wav_file.is_none() {
        let _ = tx.send(RenderUpdate::Finished(
            job_id.clone(),
            false,
            Some("No audio source for this take: no sound.wav and no audio-bearing video.".to_string()),
        ));
        return;
    }
    // Gated on the same predicate the Tauri layer uses to decide whether to
    // offer the toggle at all (`clip_is_skip_eligible`), so this can never
    // quietly diverge from what the frontend thinks is skippable — this is
    // the actual enforcement, the frontend gate is only a convenience.
    if is_source_copy && !super::scanner::clip_is_skip_eligible(&clip) {
        let reason = if is_hud {
            "Skip (keep original) isn't available for HUD/alpha clips — pick a codec to render them."
        } else if clip.wav_file.is_some() {
            "Skip (keep original) isn't available for a take with its own separate audio track — pick a codec to render it."
        } else {
            "Skip (keep original) requires a captured video file, but this take has none."
        };
        let _ = tx.send(RenderUpdate::Finished(job_id.clone(), false, Some(reason.to_string())));
        return;
    }

    // The FPS below is fed to `-framerate` *before* the BMP input, so it is not
    // an output preference — it is how FFmpeg is told to interpret the frame
    // sequence's timing. A value that disagrees with the capture produces a
    // wrong computed duration and `-shortest` then trims the audio to match, so
    // the render succeeds and the clip is simply the wrong speed. This is
    // advisory only: the take may have been moved or hand-assembled, and
    // silently substituting a number found in a neighbouring file would be a
    // worse surprise than the one it fixes. See `hlcr::take_meta`.
    //
    // Logged, not sent as a `Status` update: `job.status` is a state field with
    // a fixed vocabulary ("Queued" | "Rendering" | "Finished" | "Error" |
    // "Cancelled") that the scheduler's own transitions test against, so putting
    // a sentence in it would show a paragraph where a status chip belongs and
    // strand the job if anything returned before "Rendering" overwrote it.
    if let Some(warning) = super::take_meta::fps_mismatch_warning(&take_folder, config.fps) {
        crate::log_markdown(&format!("[render-fps-mismatch] job {} — {}", job_id, warning));
    }

    let clip_type = clip.clip_type.as_str();

    // ── JIT export-drive routing ──────────────────────────────────────────────
    // Select the first directory with enough room for *this specific job* —
    // "enough room" meaning live free space minus whatever other
    // concurrently-running jobs have already provisionally reserved on that
    // same drive (`reservations`), not just the live free-space number
    // alone. Without that ledger, several jobs starting in the same
    // scheduler tick can all see the same live free-space number and all
    // pick the same drive before any of them has written a byte — the flat
    // 20 GiB threshold this replaced was only ever sized to be safe for one
    // job at a time. See docs/capture-render-studio-merge-scope.md §4.
    const SAFETY_MARGIN_BYTES: u64 = 1024 * 1024 * 1024; // 1 GiB
    let reservation_estimate = job_reservation_estimate(&clip, is_source_copy);

    let mut selected_export_dir: Option<PathBuf> = None;
    {
        let mut ledger = reservations.lock().unwrap_or_else(|p| p.into_inner());
        for dir in &config.export_directories {
            let live_free = crate::sys::disk::get_available_bytes(dir);
            let already_reserved = *ledger.get(dir).unwrap_or(&0);
            if live_free.saturating_sub(already_reserved) > reservation_estimate + SAFETY_MARGIN_BYTES {
                *ledger.entry(dir.clone()).or_insert(0) += reservation_estimate;
                selected_export_dir = Some(dir.clone());
                // Shows this job's own estimate against what else was already
                // reserved on this drive *before* this job's own addition, so
                // concurrent jobs' numbers are directly comparable in the log —
                // this is what made it possible to confirm live, under a real
                // concurrent batch, that the ledger correctly reflects other
                // in-flight jobs' reservations rather than every job seeing the
                // same live free-space number.
                crate::log_markdown(&format!(
                    "[render-reservation] job {} reserved {:.2} GB on {:?} (already had {:.2} GB reserved by other jobs, {:.2} GB live free)",
                    job_id,
                    reservation_estimate as f64 / (1024.0 * 1024.0 * 1024.0),
                    dir,
                    already_reserved as f64 / (1024.0 * 1024.0 * 1024.0),
                    live_free as f64 / (1024.0 * 1024.0 * 1024.0),
                ));
                break;
            }
        }
    }
    if selected_export_dir.is_none() && !config.export_directories.is_empty() {
        let _ = tx.send(RenderUpdate::Finished(
            job_id.clone(),
            false,
            Some(format!(
                "JIT routing failed: all {} export drive(s) have less than {:.1} GiB free once other in-flight renders' reservations are accounted for. Render halted.",
                config.export_directories.len(),
                (reservation_estimate + SAFETY_MARGIN_BYTES) as f64 / (1024.0 * 1024.0 * 1024.0)
            )),
        ));
        return;
    }
    // Releases this job's reservation on every remaining exit path below —
    // see `ReservationGuard`'s own doc comment.
    let _reservation_guard = selected_export_dir.as_ref().map(|dir| ReservationGuard {
        ledger: reservations.clone(),
        dir: dir.clone(),
        amount: reservation_estimate,
        job_id: job_id.clone(),
    });

    let output_folder = selected_export_dir.unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    if let Err(e) = std::fs::create_dir_all(&output_folder) {
        let _ = tx.send(RenderUpdate::Finished(
            job_id.clone(),
            false,
            Some(format!("Failed to create output folder: {}", e)),
        ));
        return;
    }

    // Naming pieces shared by both the FFmpeg render path and the skip/copy
    // path below, so a skipped take and a rendered one land under the same
    // scheme — "where do I find my finished clips" stays answered the same
    // way regardless of which option a take took.
    let stream_type = if is_hud { "hud" } else { "all" };
    let wav_part = match &clip.wav_file {
        Some(wav) => {
            let wav_stem = std::path::Path::new(wav).file_stem().unwrap_or_default().to_string_lossy().into_owned();
            if wav_stem.to_lowercase() == "sound" { String::new() } else { format!("_{}", wav_stem) }
        }
        // An OBS take has no wav to derive a suffix from — base_name already
        // carries the distinguishing "-obs" suffix the scanner gave it.
        None => String::new(),
    };
    let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros();
    let hash_str = format!("{:04x}", timestamp % 0x10000);
    // Reuses `take_folder` (already parsed from this same `clip.take_folder`
    // string above) rather than re-parsing a second `PathBuf` from it.
    let take_name = take_folder.file_name().unwrap_or_default().to_string_lossy();
    let demo_name = take_folder.parent().and_then(|p| p.file_name()).unwrap_or_default().to_string_lossy();

    if is_source_copy {
        // Validated above: video_file and a non-hud clip_type are guaranteed here.
        let video_name = clip.video_file.as_deref().unwrap_or_default();
        let source_video = take_folder.join(&clip.img_folder).join(video_name);
        if !source_video.is_file() {
            let _ = tx.send(RenderUpdate::Finished(
                job_id.clone(),
                false,
                Some(format!("Source video not found: {}", source_video.display())),
            ));
            return;
        }
        // Kept from whatever OBS wrote — renaming would make the file lie
        // about its own container to every tool that reads it afterwards.
        let ext = source_video.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "mp4".to_string());
        let final_name = format!("{}_{}{}_{}_{}.{}", demo_name, take_name, wav_part, stream_type, hash_str, ext);
        let out_file = output_folder.join(&final_name);
        let out_file_str = out_file.to_string_lossy().into_owned();

        if cancel_rx.load(Ordering::Relaxed) {
            let _ = tx.send(RenderUpdate::Status(job_id.clone(), "Cancelled".to_string()));
            let _ = tx.send(RenderUpdate::Finished(job_id, false, Some("Cancelled by user".to_string())));
            return;
        }
        let _ = tx.send(RenderUpdate::Status(job_id.clone(), "Rendering".to_string()));
        // Chunked rather than `tokio::fs::copy`, so Cancel actually lands
        // during a large copy (Custom Output/lossless OBS captures — see
        // docs/obs_alternate_capture.md — can run tens of GB) instead of
        // being silently ignored until the whole file has already moved.
        match copy_cancellable(&source_video, &out_file, &cancel_rx).await {
            Ok(true) => {
                let _ = tx.send(RenderUpdate::Status(job_id.clone(), "Cancelled".to_string()));
                let _ = tx.send(RenderUpdate::Finished(job_id, false, Some("Cancelled by user".to_string())));
            }
            Ok(false) => {
                let _ = tx.send(RenderUpdate::Status(job_id.clone(), "Finished".to_string()));
                let _ = tx.send(RenderUpdate::Progress(job_id.clone(), 100));
                let _ = tx.send(RenderUpdate::OutputPath(job_id.clone(), out_file_str));
                let _ = tx.send(RenderUpdate::Finished(job_id, true, None));
            }
            Err(e) => {
                let _ = tx.send(RenderUpdate::Status(job_id.clone(), "Error".to_string()));
                let _ = tx.send(RenderUpdate::Finished(
                    job_id,
                    false,
                    Some(format!("Failed to copy source video to export pool: {}", e)),
                ));
            }
        }
        return;
    }

    // Extension decided in the same match as the codec args, not looked up
    // separately, so the two can't silently drift apart the way the old
    // string-keyed get_codec_preset() already had (it matched "h264" for
    // .mp4, but RenderCodec::NvencH264 has always mapped to the distinct
    // "h264_nvenc" id — dead code, removed, never actually wired to a
    // render). Alpha/HUD output always needs a MOV container regardless of
    // the selected codec (QuickTime-style alpha), matching dev's own
    // behaviour and the dropdown's "MP4" label applying only to the
    // non-alpha H.264 variants.
    let mut codec_args: Vec<&'static str> = Vec::new();
    let file_ext = if is_hud {
        codec_args.extend_from_slice(&["-c:v", "prores_ks", "-profile:v", "4444", "-pix_fmt", "yuva444p10le"]);
        ".mov"
    } else {
        match config.target_codec {
            super::config::RenderCodec::NvencH264 => {
                codec_args.extend_from_slice(&["-c:v", "h264_nvenc", "-preset", "p6", "-tune", "hq", "-cq", "15", "-pix_fmt", "yuv420p"]);
                ".mp4"
            }
            super::config::RenderCodec::H264Software => {
                codec_args.extend_from_slice(&["-c:v", "libx264", "-preset", "fast", "-crf", "16", "-pix_fmt", "yuv420p"]);
                ".mp4"
            }
            super::config::RenderCodec::ProRes => {
                codec_args.extend_from_slice(&["-c:v", "prores_ks", "-profile:v", "3", "-pix_fmt", "yuv422p10le"]);
                ".mov"
            }
            super::config::RenderCodec::DnxHr => {
                codec_args.extend_from_slice(&["-c:v", "dnxhd", "-profile:v", "dnxhr_hq", "-pix_fmt", "yuv422p"]);
                ".mov"
            }
            // `is_source_copy` already returned before this match — it has
            // its own extension and never runs an FFmpeg encode at all. Not
            // `unreachable!()`: release builds run with `panic = "abort"`
            // (see `obs::session::ObsSession::stop_handle`'s doc comment), so
            // a panic here would kill the whole render batch, not just this
            // job, if some future edit ever broke that invariant. Failing
            // loudly through the normal update channel costs nothing today
            // and is safe if it ever turns out to be reachable.
            super::config::RenderCodec::SourceCopy => {
                let _ = tx.send(RenderUpdate::Finished(
                    job_id.clone(),
                    false,
                    Some("Internal error: SourceCopy reached the FFmpeg codec path.".to_string()),
                ));
                return;
            }
        }
    };

    // MP4 has never had solid cross-player support for raw PCM audio — ffmpeg's
    // MP4 muxer boxes it as `ipcm`, which FFmpeg/VLC read fine but which Vegas
    // Pro (and likely other strict NLEs) can't decode, producing garbled/static
    // audio despite the samples themselves being intact (confirmed: the exact
    // same PCM bytes remuxed into a .mov container instead, where ffmpeg uses
    // the older/broadly-supported `sowt` tag, play back correctly everywhere).
    // AAC sidesteps the whole box-tag mess and is the standard choice for MP4
    // audio; ProRes/DNxHD stay lossless PCM since those already output .mov.
    let audio_codec_args: &[&str] = if file_ext == ".mp4" {
        &["-c:a", "aac", "-b:a", "192k"]
    } else {
        &["-c:a", "pcm_s16le"]
    };

    let final_name = format!("{}_{}{}_{}_{}{}", demo_name, take_name, wav_part, stream_type, hash_str, file_ext);
    let out_file = output_folder.join(&final_name);

    // Calculate thread scaling
    let max_concurrent = config.max_concurrent_renders;
    let threads_per_process = match std::thread::available_parallelism() {
        Ok(val) => std::cmp::max(1, val.get() / max_concurrent),
        Err(_) => 2,
    };

    // The alpha half of a HUD pair, as the scanner recorded it. Older autosaves
    // predate the field, so fall back to the literal the renderer used to
    // hardcode — that is exactly what those takes were scanned as.
    let alpha_folder = clip.alpha_folder.as_deref().unwrap_or("hudalpha");

    let mut cmd_args = vec!["-y", "-hide_banner"];
    let img_input: String;
    // Inputs for a take captured through `mirv_movie_ffmpeg`: one video per
    // stream folder instead of a numbered frame sequence.
    let video_input: String;
    // Shared by both HUD branches — one holds a video path, the other a BMP
    // sequence pattern, and the branches are mutually exclusive.
    let hud_color_input: String;
    let hud_alpha_input: String;

    // `-framerate` is deliberately absent for these. It tells FFmpeg how to
    // interpret an untimed image sequence; a video carries its own timing, and
    // forcing a rate onto it would re-time the clip. That also means a video
    // take cannot suffer the FPS mismatch that BMP takes can — there is no
    // second number to disagree with.
    if let Some(video) = clip.video_file.as_deref() {
        if clip_type == "hud_only" {
            let wav = clip.wav_file.as_deref().expect("validated above: HUD clips require a wav");
            hud_color_input = format!("{}/{}", clip.img_folder, video);
            hud_alpha_input = format!("{}/{}", alpha_folder, video);
            cmd_args.extend(vec![
                "-i", &hud_color_input,
                "-i", &hud_alpha_input,
                "-thread_queue_size", "512",
                "-i", wav,
                "-filter_complex", "[1:v]extractplanes=r[alpha];[0:v][alpha]alphamerge[hud]",
                "-map", "[hud]", "-map", "2:a",
            ]);
        } else if let Some(wav) = clip.wav_file.as_deref() {
            video_input = format!("{}/{}", clip.img_folder, video);
            cmd_args.extend(vec![
                "-i", &video_input,
                "-thread_queue_size", "512",
                "-i", wav,
            ]);
        } else {
            // An OBS take: the video already carries its own audio stream, so
            // there is nothing to mux against. A single input with no `-map`
            // needed — FFmpeg's default stream selection already picks one
            // video and one audio stream out of it.
            video_input = format!("{}/{}", clip.img_folder, video);
            cmd_args.extend(vec!["-i", &video_input]);
        }
    } else if clip_type == "hud_only" {
        let wav = clip.wav_file.as_deref().expect("validated above: HUD clips require a wav");
        hud_color_input = format!("{}/%05d.bmp", clip.img_folder);
        hud_alpha_input = format!("{}/%05d.bmp", alpha_folder);
        cmd_args.extend(vec![
            // Skip probe/analyze on known BMP sequences; add read-ahead buffering.
            "-probesize", "32", "-analyzeduration", "0", "-thread_queue_size", "512",
            "-framerate", &fps, "-i", &hud_color_input,
            "-probesize", "32", "-analyzeduration", "0", "-thread_queue_size", "512",
            "-framerate", &fps, "-i", &hud_alpha_input,
            "-thread_queue_size", "512",
            "-i", wav,
            "-filter_complex", "[1:v]extractplanes=r[alpha];[0:v][alpha]alphamerge[hud]",
            "-map", "[hud]", "-map", "2:a",
        ]);
    } else {
        // BMP shape is never admitted without a wav — see `take_shape_is_renderable`.
        let wav = clip.wav_file.as_deref().expect("validated above: BMP takes require a wav");
        img_input = format!("{}/%05d.bmp", clip.img_folder);
        cmd_args.extend(vec![
            // Skip probe/analyze on known BMP sequences; add read-ahead buffering.
            "-probesize", "32", "-analyzeduration", "0", "-thread_queue_size", "512",
            "-framerate", &fps,
            "-i", &img_input,
            "-thread_queue_size", "512",
            "-i", wav,
        ]);
    }

    cmd_args.extend(codec_args);
    let threads_str = threads_per_process.to_string();
    let out_file_str = out_file.to_string_lossy().into_owned();

    cmd_args.extend(vec!["-threads", &threads_str]);
    cmd_args.extend_from_slice(audio_codec_args);
    cmd_args.extend(vec![
        "-shortest",
        // +faststart is only needed for HTTP streaming; omitting it avoids the
        // post-render moov-atom rewrite pass on what can be multi-GB files.
        "-progress", "pipe:1", "-loglevel", "error",
        &out_file_str,
    ]);

    let mut cmd = Command::new(ffmpeg_path);
    #[cfg(target_os = "windows")]
    cmd.creation_flags(0x08000000);
    cmd.kill_on_drop(true);
    cmd.args(cmd_args);
    cmd.current_dir(&take_folder);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let _ = tx.send(RenderUpdate::Status(job_id.clone(), "Rendering".to_string()));

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let error_msg = if e.kind() == std::io::ErrorKind::NotFound {
                "FFmpeg not found. Please install FFmpeg or set a custom path in Settings.".to_string()
            } else {
                format!("Failed to spawn FFmpeg process: {}", e)
            };
            let _ = tx.send(RenderUpdate::Finished(
                job_id.clone(),
                false,
                Some(error_msg),
            ));
            return;
        }
    };

    // Spawn stderr reader task to prevent deadlock
    let mut stderr_handle = child.stderr.take().unwrap();
    let stderr_log = Arc::new(Mutex::new(String::new()));
    let stderr_log_clone = Arc::clone(&stderr_log);

    tokio::spawn(async move {
        let mut buf = vec![0u8; 1024];
        while let Ok(n) = stderr_handle.read(&mut buf).await {
            if n == 0 {
                break;
            }
            if let Ok(s) = std::str::from_utf8(&buf[..n])
                && let Ok(mut log) = stderr_log_clone.lock() {
                    log.push_str(s);
                }
        }
    });

    // Read stdout for progress updates
    let stdout_handle = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout_handle);
    let mut line = String::new();

    let mut current_fps = "0".to_string();
    let mut current_speed = "0x".to_string();

    let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(200));

    loop {
        line.clear();
        tokio::select! {
            _ = interval.tick() => {
                if cancel_rx.load(Ordering::Relaxed) {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    let _ = tx.send(RenderUpdate::Status(job_id.clone(), "Cancelled".to_string()));
                    let _ = tx.send(RenderUpdate::Finished(
                        job_id.clone(),
                        false,
                        Some("Cancelled by user".to_string()),
                    ));
                    return;
                }
            }
            read_res = reader.read_line(&mut line) => {
                match read_res {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if let Some(pos) = trimmed.find('=') {
                            let key = trimmed[..pos].trim();
                            let val = trimmed[pos + 1..].trim();

                            match key {
                                "frame" => {
                                    if let Ok(current_frame) = val.parse::<usize>()
                                        && let Some(ratio) =
                                            (current_frame * 100).checked_div(clip.frame_count)
                                    {
                                        let percent = std::cmp::min(100, ratio) as u32;
                                        let _ = tx.send(RenderUpdate::Progress(job_id.clone(), percent));
                                    }
                                }
                                "fps" => {
                                    current_fps = val.to_string();
                                    let _ = tx.send(RenderUpdate::Speed(
                                        job_id.clone(),
                                        format!("{} fps ({})", current_fps, current_speed),
                                    ));
                                }
                                "speed" => {
                                    current_speed = val.to_string();
                                    let _ = tx.send(RenderUpdate::Speed(
                                        job_id.clone(),
                                        format!("{} fps ({})", current_fps, current_speed),
                                    ));
                                }
                                _ => {}
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }

    // Wait for the process to exit with cancellation check
    let exit_status = loop {
        tokio::select! {
            res = child.wait() => {
                break res;
            }
            _ = interval.tick() => {
                if cancel_rx.load(Ordering::Relaxed) {
                    let _ = child.kill().await;
                    let _ = tx.send(RenderUpdate::Status(job_id.clone(), "Cancelled".to_string()));
                    let _ = tx.send(RenderUpdate::Finished(
                        job_id.clone(),
                        false,
                        Some("Cancelled by user".to_string()),
                    ));
                    return;
                }
            }
        }
    };

    let err_log = if let Ok(log) = stderr_log.lock() {
        if log.trim().is_empty() {
            None
        } else {
            Some(log.clone())
        }
    } else {
        None
    };

    match exit_status {
        Ok(status) if status.success() => {
            let _ = tx.send(RenderUpdate::Status(job_id.clone(), "Finished".to_string()));
            let _ = tx.send(RenderUpdate::Progress(job_id.clone(), 100));
            let _ = tx.send(RenderUpdate::OutputPath(job_id.clone(), out_file_str.clone()));
            let _ = tx.send(RenderUpdate::Finished(job_id, true, None));
        }
        Ok(status) => {
            let exit_code = status.code().map(|c| c.to_string()).unwrap_or_else(|| "Unknown".to_string());
            let error_msg = format!("FFmpeg failed with exit code: {}. Log: {}", exit_code, err_log.as_deref().unwrap_or(""));
            let _ = tx.send(RenderUpdate::Status(job_id.clone(), "Error".to_string()));
            let _ = tx.send(RenderUpdate::Finished(job_id, false, Some(error_msg)));
        }
        Err(e) => {
            let error_msg = format!("Failed to wait for FFmpeg: {}", e);
            let _ = tx.send(RenderUpdate::Status(job_id.clone(), "Error".to_string()));
            let _ = tx.send(RenderUpdate::Finished(job_id, false, Some(error_msg)));
        }
    }
}

/// Opaque wake-lock guard for a render batch's duration. Wrapped so
/// `desktop-studio/src-tauri` never needs `keepawake` as a direct
/// dependency — `native` already depends on it (see
/// `capture_engine.rs::CaptureCleanupGuard`'s own wake lock, the
/// capture-side equivalent of this).
pub struct RenderWakeLock(#[allow(dead_code)] keepawake::KeepAwake);

pub fn hold_render_wake_lock() -> Option<RenderWakeLock> {
    keepawake::Builder::default()
        .display(false)
        .idle(true)
        .sleep(true)
        .create()
        .ok()
        .map(RenderWakeLock)
}

/// Copies `src` to `dst` in chunks, checking `cancel` between reads.
///
/// The "Skip" render path's only work is this copy, and a plain
/// `tokio::fs::copy` has no hook to interrupt partway through — Cancel would
/// silently do nothing until the whole file had already moved, unlike the
/// FFmpeg path, which polls its cancel flag every 200ms and kills the child.
///
/// Returns `Ok(true)` if cancelled partway through, in which case the partial
/// `dst` is removed before returning — a truncated file left behind under
/// the pipeline's naming would look like a finished export.
async fn copy_cancellable(src: &Path, dst: &Path, cancel: &AtomicBool) -> std::io::Result<bool> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut reader = tokio::fs::File::open(src).await?;
    let mut writer = tokio::fs::File::create(dst).await?;
    // 2 MiB, matching this codebase's stated heap-buffer payload limit
    // (CLAUDE.md's Memory Safety rule) — still large enough that per-chunk
    // overhead is negligible against the copy itself, and a cancel lands
    // within a second or two even on a slow disk.
    let mut buf = vec![0u8; 2 * 1024 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            drop(writer);
            let _ = tokio::fs::remove_file(dst).await;
            return Ok(true);
        }
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n]).await?;
    }
    writer.flush().await?;
    Ok(false)
}

#[allow(dead_code)]
fn get_unique_filename(output_dir: &Path, base_name: &str, ext: &str) -> String {
    let mut counter = 1;
    let mut final_name = format!("{}{}", base_name, ext);
    while output_dir.join(&final_name).exists() {
        final_name = format!("{}_{}{}", base_name, counter, ext);
        counter += 1;
    }
    final_name
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hlcr::config::RenderCodec;

    fn scratch(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("dod_renderer_test_{}_{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn source_copy_config(export_dir: &Path) -> RenderConfig {
        RenderConfig {
            ffmpeg_path: "ffmpeg".to_string(),
            source_folder: String::new(),
            export_directories: vec![export_dir.to_path_buf()],
            fps: 300,
            target_codec: RenderCodec::SourceCopy,
            max_concurrent_renders: 1,
        }
    }

    fn drain(rx: &mpsc::Receiver<RenderUpdate>) -> Vec<RenderUpdate> {
        let mut out = Vec::new();
        while let Ok(u) = rx.try_recv() {
            out.push(u);
        }
        out
    }

    fn last_finished(updates: &[RenderUpdate]) -> Option<(bool, Option<String>)> {
        updates.iter().rev().find_map(|u| match u {
            RenderUpdate::Finished(_, success, err) => Some((*success, err.clone())),
            _ => None,
        })
    }

    /// The core of issue #82's "skip" path: an OBS take with no separate wav
    /// (audio already muxed into the video) is routed into the export pool
    /// as a plain copy — no FFmpeg spawn, original file left in place.
    #[tokio::test]
    async fn skip_copies_the_source_video_into_the_export_pool() {
        let root = scratch("skip_ok");
        let take_folder = root.join("take");
        let stream = take_folder.join("all");
        std::fs::create_dir_all(&stream).unwrap();
        std::fs::write(stream.join("video.mp4"), b"fake obs video bytes").unwrap();
        let export_dir = root.join("export");
        std::fs::create_dir_all(&export_dir).unwrap();

        let clip = ClipData {
            take_folder: take_folder.to_string_lossy().into_owned(),
            clip_type: "single".to_string(),
            img_folder: "all".to_string(),
            wav_file: None,
            base_name: "demo-take-obs".to_string(),
            frame_count: 0,
            width: 0,
            height: 0,
            date: "-".to_string(),
            video_file: Some("video.mp4".to_string()),
            alpha_folder: None,
        };

        let (tx, rx) = mpsc::channel();
        run_render_job("0".to_string(), clip, source_copy_config(&export_dir), tx, Arc::new(AtomicBool::new(false)), Arc::new(Mutex::new(HashMap::new()))).await;

        let updates = drain(&rx);
        assert_eq!(last_finished(&updates), Some((true, None)), "{:?}", updates);
        let output_path = updates.iter().find_map(|u| match u {
            RenderUpdate::OutputPath(_, p) => Some(p.clone()),
            _ => None,
        }).expect("OutputPath was not sent");
        assert!(output_path.ends_with(".mp4"), "{}", output_path);
        assert_eq!(std::fs::read(&output_path).unwrap(), b"fake obs video bytes");
        // A copy, not a move — the original take is untouched.
        assert!(stream.join("video.mp4").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// HUD/alpha compositing always needs the FFmpeg alpha-merge pass, so
    /// skip cannot apply to it regardless of how the clip's audio arrived.
    #[tokio::test]
    async fn skip_is_refused_for_a_hud_clip() {
        let root = scratch("skip_hud_refused");
        let take_folder = root.join("take");
        let stream = take_folder.join("hudcolor");
        std::fs::create_dir_all(&stream).unwrap();
        std::fs::write(stream.join("video.mp4"), b"x").unwrap();
        let export_dir = root.join("export");
        std::fs::create_dir_all(&export_dir).unwrap();

        let clip = ClipData {
            take_folder: take_folder.to_string_lossy().into_owned(),
            clip_type: "hud_only".to_string(),
            img_folder: "hudcolor".to_string(),
            wav_file: None,
            base_name: "demo-take-obs".to_string(),
            frame_count: 0,
            width: 0,
            height: 0,
            date: "-".to_string(),
            video_file: Some("video.mp4".to_string()),
            alpha_folder: Some("hudalpha".to_string()),
        };

        let (tx, rx) = mpsc::channel();
        run_render_job("0".to_string(), clip, source_copy_config(&export_dir), tx, Arc::new(AtomicBool::new(false)), Arc::new(Mutex::new(HashMap::new()))).await;

        let updates = drain(&rx);
        match last_finished(&updates) {
            Some((false, Some(_))) => {}
            other => panic!("expected a rejection, got {:?}", other),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A clip with neither a wav nor a captured video has no audio source at
    /// all — this must fail loudly rather than render a silent clip.
    #[tokio::test]
    async fn a_clip_with_no_audio_source_at_all_is_refused() {
        let root = scratch("no_audio_source");
        let take_folder = root.join("take");
        std::fs::create_dir_all(take_folder.join("all")).unwrap();
        let export_dir = root.join("export");
        std::fs::create_dir_all(&export_dir).unwrap();

        let clip = ClipData {
            take_folder: take_folder.to_string_lossy().into_owned(),
            clip_type: "single".to_string(),
            img_folder: "all".to_string(),
            wav_file: None,
            base_name: "demo-take-obs".to_string(),
            frame_count: 0,
            width: 0,
            height: 0,
            date: "-".to_string(),
            video_file: None,
            alpha_folder: None,
        };

        let (tx, rx) = mpsc::channel();
        run_render_job("0".to_string(), clip, source_copy_config(&export_dir), tx, Arc::new(AtomicBool::new(false)), Arc::new(Mutex::new(HashMap::new()))).await;

        let updates = drain(&rx);
        match last_finished(&updates) {
            Some((false, Some(_))) => {}
            other => panic!("expected a rejection, got {:?}", other),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Skip needs an actual captured video to copy — an OBS-shaped clip
    /// missing its video file (moved, deleted) must fail rather than copy
    /// nothing and report success.
    #[tokio::test]
    async fn skip_is_refused_when_the_source_video_is_missing() {
        let root = scratch("skip_missing_video");
        let take_folder = root.join("take");
        std::fs::create_dir_all(take_folder.join("all")).unwrap();
        let export_dir = root.join("export");
        std::fs::create_dir_all(&export_dir).unwrap();

        let clip = ClipData {
            take_folder: take_folder.to_string_lossy().into_owned(),
            clip_type: "single".to_string(),
            img_folder: "all".to_string(),
            wav_file: None,
            base_name: "demo-take-obs".to_string(),
            frame_count: 0,
            width: 0,
            height: 0,
            date: "-".to_string(),
            // Claims a video that was never written.
            video_file: Some("video.mp4".to_string()),
            alpha_folder: None,
        };

        let (tx, rx) = mpsc::channel();
        run_render_job("0".to_string(), clip, source_copy_config(&export_dir), tx, Arc::new(AtomicBool::new(false)), Arc::new(Mutex::new(HashMap::new()))).await;

        let updates = drain(&rx);
        match last_finished(&updates) {
            Some((false, Some(_))) => {}
            other => panic!("expected a rejection, got {:?}", other),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A clip with its own separate wav has something real to mux — skipping
    /// it would silently discard that audio track rather than reflect it,
    /// so `clip_is_skip_eligible` (shared with the Tauri per-job toggle gate)
    /// must refuse it even if a caller forces `SourceCopy` anyway.
    #[tokio::test]
    async fn skip_is_refused_when_the_clip_still_has_a_separate_wav() {
        let root = scratch("skip_has_wav");
        let take_folder = root.join("take");
        let stream = take_folder.join("all");
        std::fs::create_dir_all(&stream).unwrap();
        std::fs::write(stream.join("video.mp4"), b"x").unwrap();
        std::fs::write(take_folder.join("sound.wav"), b"wav").unwrap();
        let export_dir = root.join("export");
        std::fs::create_dir_all(&export_dir).unwrap();

        let clip = ClipData {
            take_folder: take_folder.to_string_lossy().into_owned(),
            clip_type: "single".to_string(),
            img_folder: "all".to_string(),
            wav_file: Some("sound.wav".to_string()),
            base_name: "demo-take".to_string(),
            frame_count: 0,
            width: 0,
            height: 0,
            date: "-".to_string(),
            video_file: Some("video.mp4".to_string()),
            alpha_folder: None,
        };

        let (tx, rx) = mpsc::channel();
        run_render_job("0".to_string(), clip, source_copy_config(&export_dir), tx, Arc::new(AtomicBool::new(false)), Arc::new(Mutex::new(HashMap::new()))).await;

        let updates = drain(&rx);
        match last_finished(&updates) {
            Some((false, Some(_))) => {}
            other => panic!("expected a rejection, got {:?}", other),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #80's sibling gap on the Skip path itself: a plain `tokio::fs::copy`
    /// has no way to honor a mid-copy cancel. `copy_cancellable` must remove
    /// whatever partial file it started, not leave a truncated one sitting
    /// under the pipeline's naming looking like a finished export.
    #[tokio::test]
    async fn copy_cancellable_removes_the_partial_file_when_cancelled() {
        let root = scratch("copy_cancel");
        let src = root.join("src.bin");
        std::fs::write(&src, vec![0u8; 1024]).unwrap();
        let dst = root.join("dst.bin");
        let cancel = AtomicBool::new(true);

        let cancelled = copy_cancellable(&src, &dst, &cancel).await.unwrap();
        assert!(cancelled);
        assert!(!dst.exists(), "a cancelled copy must not leave a partial file behind");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn copy_cancellable_completes_normally_without_a_cancel() {
        let root = scratch("copy_ok");
        let src = root.join("src.bin");
        std::fs::write(&src, b"hello world").unwrap();
        let dst = root.join("dst.bin");
        let cancel = AtomicBool::new(false);

        let cancelled = copy_cancellable(&src, &dst, &cancel).await.unwrap();
        assert!(!cancelled);
        assert_eq!(std::fs::read(&dst).unwrap(), b"hello world");
        let _ = std::fs::remove_dir_all(&root);
    }
}
