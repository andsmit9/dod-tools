#![allow(unused_mut, unused_variables, unused_assignments, dead_code)]
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write, Seek, SeekFrom};
use std::path::Path;

const DEMO_HEADER_SIZE: usize = 544;
const DIRECTORY_OFFSET_POS: usize = 540;
const FRAME_HEADER_SIZE: usize = 9;
const NETMSG_INFO_SIZE: usize = 464;
const DIR_ENTRY_SIZE: usize = 92;
const MAX_PAYLOAD_LIMIT_BYTES: usize = 2_097_152;

#[allow(dead_code)]
fn write_hltv_director_frame(
    writer: &mut BufWriter<File>,
    time: f32,
    tick: i32,
    info_block: &[u8],
    payload: &[u8],
) -> std::io::Result<i32> {
    writer.write_all(&[1_u8])?; // type (NetworkMessage)
    writer.write_all(&time.to_le_bytes())?;
    writer.write_all(&tick.to_le_bytes())?;
    writer.write_all(info_block)?; // 464 bytes NETMSG_INFO_SIZE
    
    let msg_len = (payload.len() + 1) as u32;
    writer.write_all(&msg_len.to_le_bytes())?;
    writer.write_all(payload)?;
    writer.write_all(&[1_u8])?; // svc_nop
    
    let total_bytes = 9 + 464 + 4 + payload.len() + 1;
    Ok(total_bytes as i32)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input_path = if Path::new("local/demos/test_director_cmds_hltv_source.dem").exists() {
        Path::new("local/demos/test_director_cmds_hltv_source.dem")
    } else {
        Path::new("local/demos/hltv_test.dem")
    };
    let output_path = Path::new("local/demos/hltv_test_out.dem");

    if !input_path.exists() {
        eprintln!("Input demo not found at {}", input_path.display());
        return Ok(());
    }

    let input_file = File::open(input_path)?;
    let output_file = File::create(output_path)?;

    let mut reader = BufReader::new(input_file);
    let mut writer = BufWriter::new(output_file);

    let mut header = vec![0u8; DEMO_HEADER_SIZE];
    reader.read_exact(&mut header)?;

    let original_offset = i32::from_le_bytes(header[DIRECTORY_OFFSET_POS..DEMO_HEADER_SIZE].try_into()?);
    writer.write_all(&header)?;

    // Map directory entry boundaries
    let mut dir_entries: Vec<(i32, i32)> = Vec::new();
    if original_offset > 0 {
        reader.seek(SeekFrom::Start(original_offset as u64))?;
        let mut dir_count_buf = [0u8; 4];
        if reader.read_exact(&mut dir_count_buf).is_ok() {
            let dir_count = i32::from_le_bytes(dir_count_buf);
            for _ in 0..dir_count {
                let mut entry = [0u8; DIR_ENTRY_SIZE];
                if reader.read_exact(&mut entry).is_ok() {
                    let offset = i32::from_le_bytes(entry[84..88].try_into()?);
                    let file_length = i32::from_le_bytes(entry[88..92].try_into()?);
                    dir_entries.push((offset, file_length));
                }
            }
        }
        reader.seek(SeekFrom::Start(DEMO_HEADER_SIZE as u64))?;
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

    let mut bytes_injected = 0i32;
    let mut _injected_ineye = false;
    let mut _injected_mode = false;
    let mut last_info_block = vec![0u8; NETMSG_INFO_SIZE];
    let mut _has_info_block = false;

    let mut scratch_buf = Vec::new();

    loop {
        let pos = reader.stream_position()? as usize;
        if original_offset > 0 && pos >= original_offset as usize {
            break;
        }

        let mut frame_hdr = [0u8; FRAME_HEADER_SIZE];
        if reader.read_exact(&mut frame_hdr).is_err() {
            break;
        }

        let type_byte = frame_hdr[0];
        let _time = f32::from_le_bytes(frame_hdr[1..5].try_into()?);
        let file_tick = i32::from_le_bytes(frame_hdr[5..9].try_into()?);
        let file_tick = i32::from_le_bytes(frame_hdr[5..9].try_into()?);



        match type_byte {
            2 | 5 => {
                writer.write_all(&frame_hdr)?;
            }
            3 => {
                writer.write_all(&frame_hdr)?;
                scratch_buf.resize(64, 0);
                reader.read_exact(&mut scratch_buf)?;
                writer.write_all(&scratch_buf)?;
            }
            4 => {
                writer.write_all(&frame_hdr)?;
                scratch_buf.resize(32, 0);
                reader.read_exact(&mut scratch_buf)?;
                writer.write_all(&scratch_buf)?;
            }
            6 => {
                writer.write_all(&frame_hdr)?;
                scratch_buf.resize(84, 0);
                reader.read_exact(&mut scratch_buf)?;
                writer.write_all(&scratch_buf)?;
            }
            7 => {
                writer.write_all(&frame_hdr)?;
                scratch_buf.resize(8, 0);
                reader.read_exact(&mut scratch_buf)?;
                writer.write_all(&scratch_buf)?;
            }
            8 => {
                writer.write_all(&frame_hdr)?;
                let mut prefix = [0u8; 8];
                reader.read_exact(&mut prefix)?;
                writer.write_all(&prefix)?;
                let sample_length = u32::from_le_bytes(prefix[4..8].try_into()?) as usize;
                if sample_length > MAX_PAYLOAD_LIMIT_BYTES {
                    break;
                }
                std::io::copy(&mut reader.by_ref().take((sample_length + 16) as u64), &mut writer)?;
            }
            9 => {
                writer.write_all(&frame_hdr)?;
                let mut prefix = [0u8; 4];
                reader.read_exact(&mut prefix)?;
                writer.write_all(&prefix)?;
                let buffer_length = u32::from_le_bytes(prefix) as usize;
                if buffer_length > MAX_PAYLOAD_LIMIT_BYTES {
                    break;
                }
                std::io::copy(&mut reader.by_ref().take(buffer_length as u64), &mut writer)?;
            }
            _ => {
                scratch_buf.resize(NETMSG_INFO_SIZE, 0);
                reader.read_exact(&mut scratch_buf)?;
                last_info_block.copy_from_slice(&scratch_buf);
                _has_info_block = true;

                writer.write_all(&frame_hdr)?;
                writer.write_all(&scratch_buf)?;

                let mut len_buf = [0u8; 4];
                reader.read_exact(&mut len_buf)?;
                
                let msg_len = u32::from_le_bytes(len_buf) as usize;
                if msg_len > MAX_PAYLOAD_LIMIT_BYTES {
                    break;
                }

                let mut payload = vec![0u8; msg_len];
                reader.read_exact(&mut payload)?;

                // Prepend our override to the start of the packet so it executes before any early-termination markers
                if (1000..=1200).contains(&file_tick) {
                    let mut new_payload = vec![0x33, 0x02, 0x03, 0x04, 0x33, 0x02, 0x04, 0x08];
                    new_payload.extend_from_slice(&payload);
                    payload = new_payload;
                    
                    let added_bytes = 8;
                    update_injection(pos as u64, added_bytes, 0);
                    bytes_injected += added_bytes;
                }

                let new_msg_len = payload.len() as u32;
                writer.write_all(&new_msg_len.to_le_bytes())?;
                writer.write_all(&payload)?;
            }
        }
    }

    // Rewrite directory entries
    let mut dir_count_buf = [0u8; 4];
    if reader.read_exact(&mut dir_count_buf).is_ok() {
        writer.write_all(&dir_count_buf)?;
        let dir_count = i32::from_le_bytes(dir_count_buf);
        let mut accumulated_shift = 0i32;

        for i in 0..dir_count {
            let mut entry = [0u8; DIR_ENTRY_SIZE];
            if reader.read_exact(&mut entry).is_ok() {
                let idx = i as usize;
                let inj_bytes = *injected_per_entry.get(idx).unwrap_or(&0);
                let inj_frames = *frames_per_entry.get(idx).unwrap_or(&0);

                let mut frame_count = i32::from_le_bytes(entry[80..84].try_into()?);
                frame_count += inj_frames;
                entry[80..84].copy_from_slice(&frame_count.to_le_bytes());

                let mut offset = i32::from_le_bytes(entry[84..88].try_into()?);
                offset += accumulated_shift;
                entry[84..88].copy_from_slice(&offset.to_le_bytes());

                let mut file_length = i32::from_le_bytes(entry[88..92].try_into()?);
                file_length += inj_bytes;
                entry[88..92].copy_from_slice(&file_length.to_le_bytes());

                accumulated_shift += inj_bytes;
                writer.write_all(&entry)?;
            }
        }
    }

    let _ = std::io::copy(&mut reader, &mut writer);
    writer.flush()?;

    let mut out_file = writer.into_inner()?;
    out_file.seek(SeekFrom::Start(DIRECTORY_OFFSET_POS as u64))?;
    let new_offset = original_offset + bytes_injected;
    out_file.write_all(&new_offset.to_le_bytes())?;

    println!("Injection complete. Output written to {}. Total injected: {} bytes.", output_path.display(), bytes_injected);
    Ok(())
}
