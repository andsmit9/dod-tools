use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::Path;

const DEMO_HEADER_SIZE: usize = 544;
const DIRECTORY_OFFSET_POS: usize = 540;
const FRAME_HEADER_SIZE: usize = 9;
const NETMSG_INFO_SIZE: usize = 464;
const MAX_PAYLOAD_LIMIT_BYTES: usize = 2_097_152;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let demo_path = Path::new("local/demos/test_director_cmds_hltv_source.dem");
    if !demo_path.exists() {
        eprintln!("Test demo file not found at: {}", demo_path.display());
        eprintln!("Please place an HLTV demo at 'local/demos/hltv_test.dem' to analyze.");
        return Ok(());
    }

    let file = File::open(demo_path)?;
    let mut reader = BufReader::new(file);

    let mut header = [0u8; DEMO_HEADER_SIZE];
    reader.read_exact(&mut header)?;

    let directory_offset = i32::from_le_bytes(header[DIRECTORY_OFFSET_POS..DEMO_HEADER_SIZE].try_into()?) as usize;

    println!("HLTV Diagnostic Reader initialized for {}", demo_path.display());
    println!("Directory Offset: {}", directory_offset);

    let end_offset = if directory_offset > 0 { directory_offset } else { usize::MAX };

    let mut frame_hdr = [0u8; FRAME_HEADER_SIZE];
    let mut total_director_events = 0;

    loop {
        let pos = reader.stream_position()? as usize;
        if pos >= end_offset {
            break;
        }

        if reader.read_exact(&mut frame_hdr).is_err() {
            break;
        }

        let type_byte = frame_hdr[0];
        let _time = f32::from_le_bytes(frame_hdr[1..5].try_into()?);
        let file_tick = i32::from_le_bytes(frame_hdr[5..9].try_into()?);

        match type_byte {
            2 | 5 => {},
            3 => {
                let mut buf = [0u8; 64];
                reader.read_exact(&mut buf)?;
            }
            4 => {
                let mut buf = [0u8; 32];
                reader.read_exact(&mut buf)?;
            }
            6 => {
                let mut buf = [0u8; 84];
                reader.read_exact(&mut buf)?;
            }
            7 => {
                let mut buf = [0u8; 8];
                reader.read_exact(&mut buf)?;
            }
            8 => {
                let mut prefix = [0u8; 8];
                reader.read_exact(&mut prefix)?;
                let sample_length = u32::from_le_bytes(prefix[4..8].try_into()?) as usize;
                if sample_length > MAX_PAYLOAD_LIMIT_BYTES {
                    eprintln!("Parser alignment lost at pos {} with length {}", pos, sample_length);
                    break;
                }
                let mut payload = vec![0u8; sample_length + 16];
                reader.read_exact(&mut payload)?;
                total_director_events += 1;
                let hex_str: Vec<String> = payload.iter().map(|b| format!("{:02X}", b)).collect();
                println!("[Tick {:>6}] Frame Type 8 Payload ({} bytes): [{}]", file_tick, payload.len(), hex_str.join(", "));
            }
            9 => {
                let mut prefix = [0u8; 4];
                reader.read_exact(&mut prefix)?;
                let buffer_length = u32::from_le_bytes(prefix) as usize;
                if buffer_length > MAX_PAYLOAD_LIMIT_BYTES {
                    break;
                }
                let mut payload = vec![0u8; buffer_length];
                reader.read_exact(&mut payload)?;
            }
            _ => {
                let mut info_buf = [0u8; NETMSG_INFO_SIZE];
                if reader.read_exact(&mut info_buf).is_err() { break; }
                let mut len_buf = [0u8; 4];
                if reader.read_exact(&mut len_buf).is_err() { break; }
                let msg_len = u32::from_le_bytes(len_buf) as usize;
                if msg_len > MAX_PAYLOAD_LIMIT_BYTES {
                    break;
                }
                let mut payload = vec![0u8; msg_len];
                if reader.read_exact(&mut payload).is_err() { break; }

                let mut i = 0;
                while i < payload.len() {
                    if payload[i] == 51
                        && i + 1 < payload.len() {
                            let len = payload[i + 1] as usize;
                            if len > 0 && len < 20 && i + 1 + len < payload.len() {
                                let director_payload = &payload[i..=i + 1 + len];
                                let hex_str: Vec<String> = director_payload.iter().map(|b| format!("{:02X}", b)).collect();
                                if file_tick > 0 {
                                    println!("[Tick {:>6}] svc_director packet: [{}]", file_tick, hex_str.join(", "));
                                }
                            }
                        }
                    i += 1;
                }
            }
        }
    }

    println!("Diagnostic dump complete. Found {} type 8 events.", total_director_events);
    Ok(())
}
