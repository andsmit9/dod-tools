// patch/engine.rs
// Binary stream manipulation against the GoldSrc .dem wire format.
// Reads/writes to disk — native-only.
// Strict 468-byte NetworkMessage alignment must be preserved; see ai_project_context.md.

use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use crate::patch::types::{PatchJob, PatcherConfig};
use crate::patch::{MAX_PAYLOAD_LIMIT_BYTES, NETWORK_HEADER_ALIGNMENT, SCANNER_SECTION_BOUNDARY};

// ── Internal helper ───────────────────────────────────────────────────────────

fn write_console_cmd(writer: &mut std::io::BufWriter<std::fs::File>, time: f32, tick: i32, cmd: &str) -> std::io::Result<i32> {
    use std::io::Write;
    log::debug!("Injecting Command: {} at tick: {}", cmd, tick);
    let command_string = cmd;
    if command_string.len() >= crate::patch::MAX_CONSOLE_CMD_LEN {
        let msg = format!(
            "FATAL: Command too long for a ConsoleCommand frame (the demo format's 64-byte command field). Command: '{}', Length: {}",
            command_string,
            command_string.len()
        );
        log::error!("{}", msg);
        use std::io::Write as _;
        let _ = (|| -> Result<(), Box<dyn std::error::Error>> {
            let exe_path = std::env::current_exe()?;
            let exe_dir = exe_path.parent().ok_or("Failed to get exe parent")?;
            let local_dir = exe_dir.join("local");
            std::fs::create_dir_all(&local_dir)?;
            let log_path = local_dir.join("crash_log.md");
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&log_path)?;
            writeln!(file, "{}", msg)?;
            Ok(())
        })();
        panic!("{}", msg);
    }
    writer.write_all(&[3_u8])?;
    writer.write_all(&time.to_le_bytes())?;
    writer.write_all(&tick.to_le_bytes())?;
    let mut payload = vec![0_u8; crate::patch::MAX_CONSOLE_CMD_LEN];
    let cmd_bytes = cmd.as_bytes();
    let len = cmd_bytes.len().min(crate::patch::MAX_CONSOLE_CMD_SAFE_LEN);
    payload[..len].copy_from_slice(&cmd_bytes[..len]);
    writer.write_all(&payload)?;
    Ok(73)
}

fn write_director_event_payload(
    writer: &mut std::io::BufWriter<std::fs::File>,
    time: f32,
    tick: i32,
    info_block: &[u8],
    command: &str,
) -> std::io::Result<i32> {
    use std::io::Write;
    
    // svc_director STUFFTEXT payload_len is a u8; silently clamp to 253 bytes.
    // (The 64-byte panic is the ConsoleCommand frame's field width, not a limit
    // on director payloads -- those travel inside a NetworkMessage instead.)
    let command = if command.len() > crate::patch::MAX_DIRECTOR_STUFFTEXT_LEN { &command[..crate::patch::MAX_DIRECTOR_STUFFTEXT_LEN] } else { command };

    let cmd_bytes = command.as_bytes();
    let cmd_len = cmd_bytes.len();
    let payload_len = (1 + cmd_len + 1) as u8;
    
    let mut payload = Vec::with_capacity(3 + cmd_len + 1);
    payload.push(0x33);         // svc_director
    payload.push(payload_len);
    payload.push(0x0A);         // DRC_CMD_STUFFTEXT
    payload.extend_from_slice(cmd_bytes);
    payload.push(0x00);         // null terminator

    writer.write_all(&[1_u8])?; // type (NetworkMessage)
    writer.write_all(&time.to_le_bytes())?;
    writer.write_all(&tick.to_le_bytes())?;
    writer.write_all(info_block)?;
    
    let msg_len = (payload.len() + 1) as u32;
    writer.write_all(&msg_len.to_le_bytes())?;
    writer.write_all(&payload)?;
    writer.write_all(&[1_u8])?; // svc_nop
    
    let total_bytes = crate::patch::FRAME_HEADER_SIZE + NETWORK_HEADER_ALIGNMENT + payload.len() + 1;
    Ok(total_bytes as i32)
}

// Forces the spectator view into one player's eyes by injecting
// `DRC_CMD_INEYE`, as a **standalone** network-message frame.
//
// Standalone is the whole point. An HLTV demo carries every player's
// highlights and its camera is the auto-director's, so putting the view on the
// player a clip is about used to mean clicking through spectator targets by
// hand. The first attempt at doing it from the stream prepended this
// svc_director inside the *existing* frame's payload instead of writing its
// own frame, and the result would not play at all -- "illegal server message"
// and "packet read overflow" on load, which is the documented consequence of
// interleaving injected messages into an existing packet.
//
// Used by `preview_cli --player`; see `HIJACK_REASSERT_SECONDS` for why it is
// written repeatedly rather than once.
//
// It is NOT about first-person demos. Those already record the only camera
// they have, and the Highlights table filters a POV demo down to the recording
// player (`isVisibleStreak`, `detail_pane.js`), so nothing there needs a view
// hijack. Other players' streaks are scanned out of a POV demo only so the
// Demo Analyzer can build a scoreboard from them.
//
// Anything reasoning about where the capture camera is — the decal flush's
// visibility test most of all — reads the demo's recorded `refparams`, which
// is correct exactly while this stays unused. Switching it on moves the camera
// away from what those samples describe, and the flush would have to follow.
// GoldSrc's director stream cannot aim DoD's spectator camera. DoD 1.3's
// client handles DRC commands 1-10 only -- its jump table has exactly ten
// entries behind a `cmp cmd-1, 9 / ja default` bounds check -- so DRC_CMD_CHASE
// (11) and DRC_CMD_INEYE (12) fall straight through and are discarded. The
// in-eye hijack that used to live here was removed once that was read out of
// client.dll; see docs/goldsrc_client_dll_internals.md. The two director
// commands this file does inject, MESSAGE (6) and STUFFTEXT (10), are inside
// the handled range, which is why they work.

// ── Stream patcher ────────────────────────────────────────────────────────────

pub struct StreamPatcher {
    pub input_path: std::path::PathBuf,
    pub output_path: std::path::PathBuf,
}

impl StreamPatcher {
    pub fn new(input_path: impl AsRef<std::path::Path>, output_path: impl AsRef<std::path::Path>) -> Self {
        Self {
            input_path: input_path.as_ref().to_path_buf(),
            output_path: output_path.as_ref().to_path_buf(),
        }
    }

    pub fn patch(&self, job: &PatchJob, config: &PatcherConfig, cancel_token: &Arc<AtomicBool>) -> Result<(), std::io::Error> {
        use std::io::{BufReader, BufWriter, Read, Write, Seek, SeekFrom};

        // Decal hygiene runs first, as a whole-file rewrite the stream below
        // then reads from. Held for the whole patch: the scratch demo is
        // deleted when this drops, including on the cancellation path.
        // Falls back to the original demo (having logged why) rather than
        // failing the batch — see `prepare_flushed_source`. Cancellation is
        // the one thing it does not fall back from: the pass is checked at
        // every stage boundary and from inside the demo serialise, so a
        // Cancel press no longer waits out a whole flush. See #193.
        let cleaned_source = crate::patch::decal_strip::prepare_flushed_source(
            job,
            config,
            crate::patch::Cancel::new(cancel_token),
        )
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::Interrupted, "Cancelled by user")
        })?;
        let source_demo: &std::path::Path = match &cleaned_source {
            Some(c) => c.path(),
            None => std::path::Path::new(&job.source_demo),
        };

        let input_file = std::fs::File::open(source_demo)?;
        let output_file = std::fs::File::create(&job.output_demo)?;

        let mut reader = BufReader::with_capacity(crate::patch::IO_BUFFER_CAPACITY, input_file);
        let mut writer = BufWriter::with_capacity(crate::patch::IO_BUFFER_CAPACITY, output_file);

        let read_exact = |reader: &mut BufReader<std::fs::File>, buf: &mut [u8], label: &str| -> Result<(), std::io::Error> {
            let expected_size = buf.len();
            let file_cursor_position = reader.stream_position().unwrap_or(0);
            match reader.read_exact(buf) {
                Ok(()) => Ok(()),
                Err(e) => {
                    if !(e.kind() == std::io::ErrorKind::UnexpectedEof && label == "Frame Header") {
                        crate::log_markdown(&format!(
                            "Buffer Error [{}]: Expected {} bytes at offset {}, but hit EOF.",
                            label, expected_size, file_cursor_position
                        ));
                    }
                    Err(e)
                }
            }
        };

        // Step 2: Parse and Clone the Header
        let mut header = vec![0u8; crate::patch::DEMO_HEADER_SIZE];
        read_exact(&mut reader, &mut header, "Header")?;

        // Extract directory offset (i32 is at offset 540..544)
        let original_offset = i32::from_le_bytes(header[crate::patch::DIRECTORY_OFFSET_POS..crate::patch::DEMO_HEADER_SIZE].try_into().unwrap());

        writer.write_all(&header)?;

        let mut bytes_injected: i32 = 0;
        let mut is_first_frame = true;
        let mut playback_started = false;
        let mut director_queue: std::collections::VecDeque<(i32, String)> = {
            let mut b = job.director_events.clone();
            b.sort_unstable_by_key(|(tick, _)| *tick);
            b.into()
        };
        let mut scheduled_queue: std::collections::VecDeque<(i32, String)> = job.scheduled_commands.iter().cloned().collect();


        // Step 2.5: Pre-read the directory to map entry boundaries
        let mut dir_entries: Vec<(i32, i32)> = Vec::new();
        if original_offset > 0 {
            reader.seek(SeekFrom::Start(original_offset as u64))?;
            let mut dir_count_buf = [0u8; 4];
            if reader.read_exact(&mut dir_count_buf).is_ok() {
                let dir_count = i32::from_le_bytes(dir_count_buf);
                for _ in 0..dir_count {
                    let mut entry = [0u8; crate::patch::DIR_ENTRY_SIZE];
                    if reader.read_exact(&mut entry).is_ok() {
                        let offset = i32::from_le_bytes(entry[84..88].try_into().unwrap());
                        let file_length = i32::from_le_bytes(entry[88..92].try_into().unwrap());
                        dir_entries.push((offset, file_length));
                    }
                }
            }
            reader.seek(SeekFrom::Start(crate::patch::DEMO_HEADER_SIZE as u64))?;
        }

        let mut injected_per_entry = vec![0i32; dir_entries.len().max(1)];
        let mut frames_per_entry = vec![0i32; dir_entries.len().max(1)];

        let mut update_injection = |pos: u64, bytes: i32, frames: i32| {
            let mut found = false;
            for (i, &(offset, length)) in dir_entries.iter().enumerate() {
                if pos >= offset as u64 && pos < (offset + length) as u64 {
                    injected_per_entry[i] += bytes;
                    frames_per_entry[i] += frames;
                    found = true;
                    break;
                }
            }
            if !found && !dir_entries.is_empty() {
                let last = dir_entries.len() - 1;
                injected_per_entry[last] += bytes;
                frames_per_entry[last] += frames;
            } else if !found {
                injected_per_entry[0] += bytes;
                frames_per_entry[0] += frames;
            }
        };

        // Step 3: Zero-Allocation Copy Loop
        let mut scratch_buf = Vec::new();
        let mut frame_counter = 0i32;

        loop {
            if cancel_token.load(Ordering::Relaxed) {
                return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "Cancelled by user"));
            }

            let pos = reader.stream_position().unwrap_or(0);
            if original_offset > 0 && pos >= original_offset as u64 {
                break;
            }

            let mut frame_hdr = [0u8; crate::patch::FRAME_HEADER_SIZE];
            if let Err(e) = read_exact(&mut reader, &mut frame_hdr, "Frame Header") {
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    break;
                }
                return Err(e);
            }

            frame_counter += 1;

            let type_byte = frame_hdr[0];
            let time = f32::from_le_bytes(frame_hdr[1..5].try_into().unwrap());
            let file_tick = i32::from_le_bytes(frame_hdr[5..9].try_into().unwrap());

            if type_byte == 2 && is_first_frame {
                playback_started = true;
                writer.write_all(&frame_hdr)?;
                for cmd in &job.init_commands {
                    let b = write_console_cmd(&mut writer, time, file_tick, cmd)?;
                    update_injection(pos, b, 1);
                    bytes_injected += b;
                }
                
                while let Some((target_tick, _cmd)) = scheduled_queue.front() {
                    let actual_target = *target_tick;
                    if playback_started && frame_counter >= actual_target {
                        let (_, cmd) = scheduled_queue.pop_front().unwrap();
                        let b = write_console_cmd(&mut writer, time, file_tick, &cmd)?;
                        update_injection(pos, b, 1);
                        bytes_injected += b;
                    } else {
                        break;
                    }
                }
                is_first_frame = false;
                continue;
            }

            while let Some((target_tick, _cmd)) = scheduled_queue.front() {
                let actual_target = *target_tick;
                if playback_started && frame_counter >= actual_target {
                    let (_, cmd) = scheduled_queue.pop_front().unwrap();
                    let b = write_console_cmd(&mut writer, time, file_tick, &cmd)?;
                    update_injection(pos, b, 1);
                    bytes_injected += b;
                } else {
                    break;
                }
            }
            // Determine payload size to read/write and write frame header where appropriate
            match type_byte {
                2 => {
                    // DemoStart (0 bytes)
                    writer.write_all(&frame_hdr)?;
                }
                3 => {
                    // ConsoleCommand (64 bytes)
                    writer.write_all(&frame_hdr)?;
                    scratch_buf.resize(crate::patch::CMD_FRAME_SIZE, 0);
                    read_exact(&mut reader, &mut scratch_buf, "ConsoleCommand")?;
                    writer.write_all(&scratch_buf)?;
                }
                4 => {
                    // ClientData (32 bytes)
                    writer.write_all(&frame_hdr)?;
                    scratch_buf.resize(crate::patch::CLIENT_DATA_FRAME_SIZE, 0);
                    read_exact(&mut reader, &mut scratch_buf, "ClientData")?;
                    writer.write_all(&scratch_buf)?;
                }
                SCANNER_SECTION_BOUNDARY => {
                    // NextSection (0 bytes)
                    writer.write_all(&frame_hdr)?;
                }
                6 => {
                    // Event (84 bytes)
                    writer.write_all(&frame_hdr)?;
                    scratch_buf.resize(crate::patch::EVENT_FRAME_SIZE, 0);
                    read_exact(&mut reader, &mut scratch_buf, "Event")?;
                    writer.write_all(&scratch_buf)?;
                }
                7 => {
                    // WeaponAnimation (8 bytes)
                    writer.write_all(&frame_hdr)?;
                    scratch_buf.resize(8, 0);
                    read_exact(&mut reader, &mut scratch_buf, "WeaponAnimation")?;
                    writer.write_all(&scratch_buf)?;
                }
                8 => {
                    // Sound (24 bytes + sample_length)
                    writer.write_all(&frame_hdr)?;
                    let mut prefix = [0u8; 8];
                    read_exact(&mut reader, &mut prefix, "Sound Prefix")?;
                    writer.write_all(&prefix)?;
                    let sample_length = u32::from_le_bytes(prefix[4..8].try_into().unwrap()) as usize;

                    if sample_length > MAX_PAYLOAD_LIMIT_BYTES {
                        crate::log_markdown(&format!(
                            "Alignment lost! Read impossible size: {} at offset {}",
                            sample_length,
                            reader.stream_position().unwrap_or(0)
                        ));
                        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "Parser alignment lost"));
                    }

                    std::io::copy(&mut reader.by_ref().take((sample_length + 16) as u64), &mut writer)?;
                }
                9 => {
                    // DemoBuffer (4 bytes + buffer_length)
                    writer.write_all(&frame_hdr)?;
                    let mut prefix = [0u8; 4];
                    read_exact(&mut reader, &mut prefix, "DemoBuffer Prefix")?;
                    writer.write_all(&prefix)?;
                    let buffer_length = u32::from_le_bytes(prefix) as usize;

                    if buffer_length > MAX_PAYLOAD_LIMIT_BYTES {
                        crate::log_markdown(&format!(
                            "Alignment lost! Read impossible size: {} at offset {}",
                            buffer_length,
                            reader.stream_position().unwrap_or(0)
                        ));
                        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "Parser alignment lost"));
                    }

                    std::io::copy(&mut reader.by_ref().take(buffer_length as u64), &mut writer)?;
                }
                _ => {
                    // NetworkMessage (NETWORK_HEADER_ALIGNMENT bytes + message_length)
                    scratch_buf.resize(crate::patch::NETMSG_INFO_SIZE, 0);
                    read_exact(&mut reader, &mut scratch_buf, "NetworkMessage Info")?;

                    // Inject svc_director STUFFTEXT events at highlight start ticks
                    while let Some((target_tick, _)) = director_queue.front() {
                        if playback_started && frame_counter >= *target_tick {
                            let (_, label) = director_queue.pop_front().unwrap();
                            let b = write_director_event_payload(&mut writer, time, file_tick, &scratch_buf, &label)?;
                            update_injection(pos, b, 1);
                            bytes_injected += b;
                        } else {
                            break;
                        }
                    }

                    // Write original frame header and info block
                    writer.write_all(&frame_hdr)?;
                    writer.write_all(&scratch_buf)?;

                    let mut len_buf = [0u8; 4];
                    read_exact(&mut reader, &mut len_buf, "NetworkMessage Length")?;
                    let msg_len = u32::from_le_bytes(len_buf) as usize;

                    if msg_len > MAX_PAYLOAD_LIMIT_BYTES {
                        crate::log_markdown(&format!(
                            "Alignment lost! Read impossible size: {} at offset {}",
                            msg_len,
                            reader.stream_position().unwrap_or(0)
                        ));
                        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "Parser alignment lost"));
                    }

                    let mut net_buf = vec![0u8; msg_len];
                    read_exact(&mut reader, &mut net_buf, "NetworkMessage Body")?;

                    let final_len_buf = (net_buf.len() as u32).to_le_bytes();
                    writer.write_all(&final_len_buf)?;
                    writer.write_all(&net_buf)?;
                }
            }
        }


        // [STEP 4] Directory Offset Rewrite (EOF Handling)
        // 4b: Copy the remaining directory entries from the input to the output.
        let mut dir_count_buf = [0u8; 4];
        if reader.read_exact(&mut dir_count_buf).is_ok() {
            writer.write_all(&dir_count_buf)?;
            let dir_count = i32::from_le_bytes(dir_count_buf);

            let mut accumulated_shift = 0i32;

            for i in 0..dir_count {
                let mut entry = [0u8; 92];
                if reader.read_exact(&mut entry).is_ok() {
                    let idx = i as usize;
                    let inj_bytes = *injected_per_entry.get(idx).unwrap_or(&0);
                    let inj_frames = *frames_per_entry.get(idx).unwrap_or(&0);

                    let mut frame_count = i32::from_le_bytes(entry[80..84].try_into().unwrap());
                    frame_count += inj_frames;
                    entry[80..84].copy_from_slice(&frame_count.to_le_bytes());

                    let mut offset = i32::from_le_bytes(entry[84..88].try_into().unwrap());
                    offset += accumulated_shift;
                    entry[84..88].copy_from_slice(&offset.to_le_bytes());

                    let mut file_length = i32::from_le_bytes(entry[88..92].try_into().unwrap());
                    file_length += inj_bytes;
                    entry[88..92].copy_from_slice(&file_length.to_le_bytes());

                    accumulated_shift += inj_bytes;

                    writer.write_all(&entry)?;
                }
            }
        }
        let _ = std::io::copy(&mut reader, &mut writer); // Catch any trailing garbage

        // 4c: Flush BufWriter.
        writer.flush()?;

        // 4d: Seek back to the exact byte position of the directory_offset in the output file's header (usually offset 540).
        let mut out_file = writer.into_inner()?;
        out_file.seek(SeekFrom::Start(crate::patch::DIRECTORY_OFFSET_POS as u64))?;

        // 4e: Calculate new offset.
        let new_offset = original_offset + bytes_injected;

        // 4f: Write the new_offset (as Little Endian bytes).
        out_file.write_all(&new_offset.to_le_bytes())?;

        out_file.sync_all().unwrap_or_default();
        drop(out_file);
        drop(reader);

        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::types::{CaptureStreak, PatcherConfig, PatchJob};
    use crate::patch::builder::spawn_patch_batch;
    use crate::patch::types::PatchEvent;

    #[test]
    fn test_stream_patcher_end_to_end() {
        let scratch_dir = std::path::Path::new("scratch");
        if !scratch_dir.exists() {
            let _ = std::fs::create_dir_all(scratch_dir);
        }

        let input_path = scratch_dir.join("test_input.dem");
        let output_path = scratch_dir.join("test_output.dem");

        // Synthesize a valid minimal GoldSrc demo file
        let mut header = [0u8; 544];
        header[0..8].copy_from_slice(b"HLDEMO\0\0");
        // directory offset is at 540..544.
        // File structure:
        // Header: 544 bytes
        // Frame 1 (DemoStart, type 2): 9 bytes (offset 544..553)
        // Frame 2 (NextSection, type 5): 9 bytes (offset 553..562)
        // Directory count (i32): 4 bytes (offset 562..566)
        // Directory entry: 92 bytes (offset 566..658)
        // Total size = 658 bytes. Directory starts at offset 562.
        let directory_offset: i32 = 562;
        header[540..544].copy_from_slice(&directory_offset.to_le_bytes());

        let mut file_data = Vec::new();
        file_data.extend_from_slice(&header);

        // Frame 1: DemoStart (type 2), time = 0.0, tick = 0
        file_data.push(2);
        file_data.extend_from_slice(&0.0_f32.to_le_bytes());
        file_data.extend_from_slice(&0_i32.to_le_bytes());

        // Frame 2: NextSection (type 5), time = 0.0, tick = 0
        file_data.push(5);
        file_data.extend_from_slice(&0.0_f32.to_le_bytes());
        file_data.extend_from_slice(&0_i32.to_le_bytes());

        // Directory Block
        // Entry count: 1
        file_data.extend_from_slice(&1_i32.to_le_bytes());
        // Entry type: 1 (Playback)
        file_data.extend_from_slice(&1_i32.to_le_bytes());
        // Description: 64 bytes
        let mut desc = [0u8; 64];
        desc[..8].copy_from_slice(b"Playback");
        file_data.extend_from_slice(&desc);
        // flags (4), cd_track (4), track_time (4)
        file_data.extend_from_slice(&0_i32.to_le_bytes());
        file_data.extend_from_slice(&0_i32.to_le_bytes());
        file_data.extend_from_slice(&0.0_f32.to_le_bytes());
        // frame_count (4)
        file_data.extend_from_slice(&2_i32.to_le_bytes());
        // frame_offset (4) = 544
        file_data.extend_from_slice(&544_i32.to_le_bytes());
        // file_length (4) = 18
        file_data.extend_from_slice(&18_i32.to_le_bytes());

        std::fs::write(&input_path, &file_data).unwrap();

        let job = PatchJob {
            source_demo: input_path.to_string_lossy().to_string(),
            output_demo: output_path.clone(),
            streaks: vec![
                CaptureStreak {
                    start_tick: 50,
                    end_tick: 100,
                    source_demo: input_path.to_string_lossy().to_string(),
                    target_player: None,
                    kill_count: 0,
                    timeline_string: String::new(),
                    duration_string: String::new(),
                    player_index: 0,
                    kills: Vec::new(),
                    start_index: 0,
                    end_index: 0,
                    total_demo_frames: 3000,
                    demo_fps: 100.0,
                    viewdemo_times: Vec::new(),
                    frame_times: std::sync::Arc::new(Vec::new()),
                    match_start_tick: None,
                    status: Default::default(),
                }
            ],
            target_player: None,
            init_commands: vec!["host_framerate 0".to_string()],
            scheduled_commands: vec![(10, "some_command".to_string())],
            director_events: Vec::new(),
            block_routes: Vec::new(),
            blocks: Vec::new(),
        };

        let config = PatcherConfig {
            pre_roll_ticks: 10,
            post_roll_ticks: 10,
            capture_fps: 300,
            exit_on_finish: true,
            init_commands: vec!["host_framerate 0".to_string()],
            custom_commands: Vec::new(),
            pre_roll_seconds: 2.0,
            post_roll_seconds: 0.6,
            record_start_lead: 0.0,
            record_stop_trail: 0.0,
            initial_delay: 3.0,
            ..PatcherConfig::default()
        };

        let patcher = StreamPatcher::new(&input_path, &output_path);
        let cancel_token = Arc::new(AtomicBool::new(false));
        patcher.patch(&job, &config, &cancel_token).unwrap();

        assert!(output_path.exists());
        let output_data = std::fs::read(&output_path).unwrap();
        assert!(output_data.len() > file_data.len());

        let _ = std::fs::remove_file(input_path);
        let _ = std::fs::remove_file(output_path);
    }

    #[test]
    fn test_stream_patcher_cancellation_and_cleanup() {
        let scratch_dir = std::path::Path::new("scratch");
        if !scratch_dir.exists() {
            let _ = std::fs::create_dir_all(scratch_dir);
        }

        let input_path = scratch_dir.join("test_cancel_input.dem");
        let output_path = scratch_dir.join("test_cancel_output.dem");

        let mut header = [0u8; 544];
        header[0..8].copy_from_slice(b"HLDEMO\0\0");
        let directory_offset: i32 = 562;
        header[540..544].copy_from_slice(&directory_offset.to_le_bytes());

        let mut file_data = Vec::new();
        file_data.extend_from_slice(&header);

        file_data.push(2);
        file_data.extend_from_slice(&0.0_f32.to_le_bytes());
        file_data.extend_from_slice(&0_i32.to_le_bytes());

        file_data.push(5);
        file_data.extend_from_slice(&0.0_f32.to_le_bytes());
        file_data.extend_from_slice(&0_i32.to_le_bytes());

        file_data.extend_from_slice(&1_i32.to_le_bytes());
        file_data.extend_from_slice(&1_i32.to_le_bytes());
        let mut desc = [0u8; 64];
        desc[..8].copy_from_slice(b"Playback");
        file_data.extend_from_slice(&desc);
        file_data.extend_from_slice(&0_i32.to_le_bytes());
        file_data.extend_from_slice(&0_i32.to_le_bytes());
        file_data.extend_from_slice(&0.0_f32.to_le_bytes());
        file_data.extend_from_slice(&2_i32.to_le_bytes());
        file_data.extend_from_slice(&544_i32.to_le_bytes());
        file_data.extend_from_slice(&18_i32.to_le_bytes());

        std::fs::write(&input_path, &file_data).unwrap();

        let job = PatchJob {
            source_demo: input_path.to_string_lossy().to_string(),
            output_demo: output_path.clone(),
            streaks: vec![
                CaptureStreak {
                    start_tick: 50,
                    end_tick: 100,
                    source_demo: input_path.to_string_lossy().to_string(),
                    target_player: None,
                    kill_count: 0,
                    timeline_string: String::new(),
                    duration_string: String::new(),
                    player_index: 0,
                    kills: Vec::new(),
                    start_index: 0,
                    end_index: 0,
                    total_demo_frames: 3000,
                    demo_fps: 100.0,
                    viewdemo_times: Vec::new(),
                    frame_times: std::sync::Arc::new(Vec::new()),
                    match_start_tick: None,
                    status: Default::default(),
                }
            ],
            target_player: None,
            init_commands: vec!["host_framerate 0".to_string()],
            scheduled_commands: vec![(10, "some_command".to_string())],
            director_events: Vec::new(),
            block_routes: Vec::new(),
            blocks: Vec::new(),
        };

        let config = PatcherConfig {
            pre_roll_ticks: 10,
            post_roll_ticks: 10,
            capture_fps: 300,
            exit_on_finish: true,
            init_commands: vec!["host_framerate 0".to_string()],
            custom_commands: Vec::new(),
            pre_roll_seconds: 2.0,
            post_roll_seconds: 0.6,
            record_start_lead: 0.0,
            record_stop_trail: 0.0,
            initial_delay: 3.0,
            ..PatcherConfig::default()
        };

        // Create a token that starts out as cancelled (true)
        let cancel_token = Arc::new(AtomicBool::new(true));

        let patcher = StreamPatcher::new(&input_path, &output_path);
        let res = patcher.patch(&job, &config, &cancel_token);

        assert!(res.is_err());
        assert_eq!(res.unwrap_err().kind(), std::io::ErrorKind::Interrupted);

        // Check spawn_patch_batch clean-up
        let cancel_token = Arc::new(AtomicBool::new(true));
        let worker = spawn_patch_batch(vec![job], config, cancel_token);
        let rx = worker.receiver;

        let mut got_cancelled = false;
        while let Ok(event) = rx.recv() {
            if let PatchEvent::Cancelled = event {
                got_cancelled = true;
            }
        }

        assert!(got_cancelled);
        assert!(!output_path.exists());

        let _ = std::fs::remove_file(input_path);
    }
}

#[cfg(test)]
mod director_event_tests {
    use super::*;
    use std::io::{BufWriter, Read, Write};

    #[test]
    fn test_write_director_event_payload() {
        let temp_path = std::env::temp_dir().join("test_write_director_event_payload.dem");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp_path)
            .unwrap();

        let mut writer = BufWriter::new(file);

        let time = 10.5_f32;
        let tick = 1050_i32;
        let command = "echo test";
        let info_block = vec![0u8; 8];

        write_director_event_payload(&mut writer, time, tick, &info_block, command).unwrap();
        writer.flush().unwrap();

        let mut file = std::fs::File::open(&temp_path).unwrap();
        let mut result = Vec::new();
        file.read_to_end(&mut result).unwrap();
        let _ = std::fs::remove_file(&temp_path);

        let cmd_bytes = command.as_bytes();
        let payload_len = (1 + cmd_bytes.len() + 1) as u8;

        let mut expected = Vec::new();
        expected.push(1_u8); // NetworkMessage type
        expected.extend_from_slice(&time.to_le_bytes());
        expected.extend_from_slice(&tick.to_le_bytes());
        expected.extend_from_slice(&info_block);

        let mut payload = Vec::new();
        payload.push(0x33);         // svc_director
        payload.push(payload_len);
        payload.push(0x0A);         // DRC_CMD_STUFFTEXT
        payload.extend_from_slice(cmd_bytes);
        payload.push(0x00);         // null terminator

        let msg_len = (payload.len() + 1) as u32;
        expected.extend_from_slice(&msg_len.to_le_bytes());
        expected.extend_from_slice(&payload);
        expected.push(0x01);        // svc_nop

        assert_eq!(result, expected);
    }
}
