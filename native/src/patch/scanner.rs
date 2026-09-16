// patch/scanner.rs
// Life-bounded highlight scanner and HLTV detection.
// All three functions perform std::fs I/O — native-only.

use crate::patch::types::{CaptureStreak, HighlightStatus};
use crate::patch::{MAX_PAYLOAD_LIMIT_BYTES, NETWORK_HEADER_ALIGNMENT, SCANNER_SECTION_BOUNDARY};

// ── HLTV guard ────────────────────────────────────────────────────────────────

pub fn is_hltv_demo(path: &std::path::Path) -> Result<bool, std::io::Error> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut header = [0_u8; crate::patch::HLTV_HEADER_SIZE];
    file.read_exact(&mut header)?;

    if header.len() >= crate::patch::HLTV_HEADER_SIZE {
        let hltv_proxy_name = b"HLTV Proxy";
        if header.windows(hltv_proxy_name.len()).any(|window| window == hltv_proxy_name) {
            return Ok(true);
        }
    }
    Ok(false)
}

// ── Life-bounded highlight scanner ───────────────────────────────────────────
// Reads player.kill_streaks directly from the completed Analysis — the authoritative,
// already-segmented output of use_kill_streak_updates (analysis/src/kill.rs).
// Segmentation boundaries (death, round reset, map change) are handled by the
// analysis crate, including the grenade kill-after-death edge case.

pub fn scan_demo_for_highlights(
    path: &std::path::Path,
) -> Result<(f32, Vec<CaptureStreak>, bool, Option<usize>, i32, Option<i32>, std::sync::Arc<Vec<f32>>), String> {
    scan_demo_for_highlights_with_analysis(path).map(|(result, _analysis)| result)
}

// Same scan as `scan_demo_for_highlights`, but also hands back the full
// `Analysis` it already computed internally (previously always discarded)
// so callers doing a folder-wide scan (e.g. Capture Studio's `scan_directory`)
// can write it straight into the analyzer cache instead of re-parsing the
// same demo from scratch the next time it's opened in the Demo Analyzer.
pub fn scan_demo_for_highlights_with_analysis(
    path: &std::path::Path,
) -> Result<((f32, Vec<CaptureStreak>, bool, Option<usize>, i32, Option<i32>, std::sync::Arc<Vec<f32>>), analysis::Analysis), String> {
    match is_hltv_demo(path) {
        Ok(true) => return Err("Unsupported HLTV proxy demo format".to_string()),
        Err(e) => return Err(format!("Failed to read demo header: {}", e)),
        _ => {}
    }

    let bytes = std::fs::read(path).map_err(|e| format!("Failed to read file: {}", e))?;

    let analysis = analysis::Analysis::try_from_bytes(&bytes)
        .map_err(|e| format!("Failed to parse demo: {}", e))?;

    let mut frame_times: Vec<f32> = Vec::with_capacity(analysis.demo_info.playback_frames as usize);
    if bytes.len() >= crate::patch::DEMO_HEADER_SIZE {
        let directory_offset = i32::from_le_bytes(bytes[crate::patch::DIRECTORY_OFFSET_POS..crate::patch::DEMO_HEADER_SIZE].try_into().unwrap()) as usize;
        let mut pos = crate::patch::DEMO_HEADER_SIZE;
        let end = if directory_offset > 0 && directory_offset <= bytes.len() { directory_offset } else { bytes.len() };
        while pos + crate::patch::FRAME_HEADER_SIZE <= end {
            let type_byte = bytes[pos];
            if type_byte > 9 && type_byte != 255 {
                break;
            }
            if type_byte == SCANNER_SECTION_BOUNDARY {
                break;
            }
            if type_byte != 255 {
                let time = f32::from_le_bytes(bytes[pos+1..pos+5].try_into().unwrap());
                frame_times.push(time);
            }
            pos += crate::patch::FRAME_HEADER_SIZE;
            match type_byte {
                0 | 1 => {
                    let total_fixed_size = NETWORK_HEADER_ALIGNMENT;
                    if pos + total_fixed_size > end { break; }
                    let len = i32::from_le_bytes(bytes[pos+crate::patch::NETMSG_INFO_SIZE..pos+total_fixed_size].try_into().unwrap()) as usize;
                    if len > MAX_PAYLOAD_LIMIT_BYTES {
                        return Err(format!("Scanner alignment lost! Read impossible packet size: {} bytes at pos {}", len, pos));
                    }
                    pos += total_fixed_size + len;
                },
                2 | 255 => {},
                3 => pos += crate::patch::CMD_FRAME_SIZE,
                4 => pos += crate::patch::CLIENT_DATA_FRAME_SIZE,
                6 => pos += crate::patch::EVENT_FRAME_SIZE,
                7 => pos += 8,
                8 => {
                    if pos + 8 > end { break; }
                    let len = u32::from_le_bytes(bytes[pos+4..pos+8].try_into().unwrap()) as usize;
                    pos += 24 + len;
                },
                9 => {
                    if pos + 4 > end { break; }
                    let len = u32::from_le_bytes(bytes[pos..pos+4].try_into().unwrap()) as usize;
                    pos += 4 + len;
                },
                _ => break,
            }
        }
    }
    let final_demo_frames = if analysis.demo_info.playback_frames > 0 {
        analysis.demo_info.playback_frames
    } else if !frame_times.is_empty() {
        frame_times.len() as i32
    } else {
        0
    };
    let frame_times_arc = std::sync::Arc::new(frame_times);

    let mut tickrate = if analysis.demo_info.playback_time > 0.0 {
        analysis.demo_info.playback_frames as f32 / analysis.demo_info.playback_time
    } else {
        100.0
    };
    
    // Fallback if the demo header has garbage values
    if !tickrate.is_normal() || !(10.0..=1000.0).contains(&tickrate) {
        tickrate = 100.0;
    }

    let mut streaks: Vec<CaptureStreak> = Vec::new();

    // ── Per-player life-bounded streak iteration ────────────────────────────────────────────
    for player in &analysis.state.players {

        // Skip players that are not (or are no longer) in a connected slot.
        // Disconnected entries have no valid client_id to anchor the patcher.
        let player_index = match player.connection {
            analysis::Connection::Connected { client_id } => client_id as usize,
            _ => continue,
        };

        for kill_streak in &player.kill_streaks {

            let kills_raw: Vec<(i32, f32, String)> = kill_streak.kills.iter()
                .map(|(time, weapon, _victim)| {
                    let abs_time = time.real_offset.as_secs_f32();
                    let tick = time.frame_index as i32;
                    (tick, abs_time, analysis::weapon_display_name(weapon))
                })
                .collect();

            if kills_raw.is_empty() {
                continue;
            }

            let viewdemo_times: Vec<f32> = kill_streak.kills.iter()
                .map(|(time, _, _)| time.viewdemo_offset.as_secs_f32())
                .collect();

            let end_index = kills_raw.len().saturating_sub(1);
            let mut streak = CaptureStreak {
                start_tick: kills_raw[0].0,
                end_tick: kills_raw[end_index].0,
                source_demo: path.to_string_lossy().to_string(),
                target_player: Some(player.name.clone()),
                kill_count: kills_raw.len(),
                timeline_string: String::new(),
                duration_string: String::new(),
                player_index,
                kills: kills_raw,
                viewdemo_times,
                start_index: 0,
                end_index,
                total_demo_frames: final_demo_frames,
                demo_fps: tickrate,
                frame_times: frame_times_arc.clone(),
                status: HighlightStatus::None,
                match_start_tick: analysis.state.match_start_tick,
            };
            streak.update_visuals();
            streaks.push(streak);
        }
    }

    let local_player_index = analysis.state.pov_player_index.map(|idx| idx as usize);
    let demo_type_is_pov = analysis.demo_info.demo_type == "POV";
    let playback_frames = analysis.demo_info.playback_frames;
    let match_start_tick = analysis.state.match_start_tick;

    Ok((
        (
            tickrate,
            streaks,
            demo_type_is_pov,
            local_player_index,
            playback_frames,
            match_start_tick,
            frame_times_arc,
        ),
        analysis,
    ))
}
