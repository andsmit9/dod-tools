//! Scratch: inspect what stat packets appear around player disconnect/reconnect events.
//!
//! Usage:
//!   cargo run --bin inspect_reconnect -- demos/ktps8w8-stealth_ih_saints_h1_p1.dem

use dem::open_demo_from_bytes;
use dem::types::{EngineMessage, FrameData, MessageData, NetMessage};
use dod::UserMessage;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

fn null_str(bytes: &[u8]) -> String {
    let trimmed = bytes.split(|&b| b == 0).next().unwrap_or(bytes);
    String::from_utf8_lossy(trimmed).to_string()
}

fn parse_userinfo(user_info: &dem::types::ByteString) -> std::collections::HashMap<String, String> {
    user_info
        .to_str()
        .map(|s| s.trim_matches(['\0', '\\']).split('\\').collect::<Vec<_>>())
        .unwrap_or_default()
        .as_chunks::<2>()
        .0
        .iter()
        .fold(std::collections::HashMap::new(), |mut map, [key, value]| {
            map.insert(key.to_string(), value.to_string());
            map
        })
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
enum Event {
    Connect { slot: u8, name: String, sid: String },
    Disconnect { slot: u8, name: String },
    Frags { slot: u8, name: String, frags: i16 },
    ScoreShort { slot: u8, name: String, score: i16, kills: i16, deaths: i16 },
    DeathMsg { killer_slot: u8, killer_name: String, victim_slot: u8, victim_name: String, weapon: String },
}

fn main() {
    let demo_path: PathBuf = std::env::args()
        .nth(1)
        .expect("Usage: inspect_reconnect <demo.dem>")
        .into();

    let bytes = fs::read(&demo_path).expect("Could not read demo file");
    let demo = open_demo_from_bytes(&bytes).expect("Could not parse demo");

    println!("=== Scanning: {} ===\n", demo_path.display());

    // slot -> last known name
    let mut slot_names: HashMap<u8, String> = HashMap::new();
    let mut events: Vec<(usize, Event)> = Vec::new();
    let mut frame_idx = 0usize;

    for entry in &demo.directory.entries {
        for frame in &entry.frames {
            frame_idx += 1;

            if let FrameData::NetworkMessage(net_msg_box) = &frame.frame_data
                && let MessageData::Parsed(msgs) = &net_msg_box.1.messages {
                    for msg in msgs {
                        match msg {
                            NetMessage::EngineMessage(eng) => {
                                if let EngineMessage::SvcUpdateUserInfo(upd) = eng.as_ref() {
                                    let fields = parse_userinfo(&upd.user_info);
                                    if fields.is_empty() {
                                        let name = slot_names
                                            .get(&upd.index)
                                            .cloned()
                                            .unwrap_or_else(|| format!("slot_{}", upd.index));
                                        events.push((frame_idx, Event::Disconnect {
                                            slot: upd.index,
                                            name,
                                        }));
                                    } else if fields.get("*hltv").map(|v| v == "1").unwrap_or(false) {
                                        // Skip HLTV proxy slots
                                    } else {
                                        let name = fields.get("name").cloned().unwrap_or_default();
                                        let sid = fields.get("*sid").cloned().unwrap_or_else(|| {
                                            fields.get("*fid").map(|f| format!("fid:{f}")).unwrap_or_default()
                                        });
                                        slot_names.insert(upd.index, name.clone());
                                        events.push((frame_idx, Event::Connect { slot: upd.index, name, sid }));
                                    }
                                }
                            }
                            NetMessage::UserMessage(usr) => {
                                let msg_name = null_str(&usr.name);
                                let slot_name = |slot: u8| {
                                    slot_names.get(&slot).cloned().unwrap_or_else(|| format!("slot_{}", slot))
                                };
                                match msg_name.as_str() {
                                    "Frags" => {
                                        if let Ok(UserMessage::Frags(f)) = UserMessage::new(&usr.name, &usr.data) {
                                            let slot = f.client_index - 1;
                                            events.push((frame_idx, Event::Frags {
                                                name: slot_name(slot),
                                                slot,
                                                frags: f.frags,
                                            }));
                                        }
                                    }
                                    "ScoreShort" => {
                                        if let Ok(UserMessage::ScoreShort(s)) = UserMessage::new(&usr.name, &usr.data) {
                                            let slot = s.client_index - 1;
                                            events.push((frame_idx, Event::ScoreShort {
                                                name: slot_name(slot),
                                                slot,
                                                score: s.score,
                                                kills: s.kills,
                                                deaths: s.deaths,
                                            }));
                                        }
                                    }
                                    "DeathMsg" => {
                                        if let Ok(UserMessage::DeathMsg(d)) = UserMessage::new(&usr.name, &usr.data) {
                                            let kslot = d.killer_client_index.saturating_sub(1);
                                            let vslot = d.victim_client_index.saturating_sub(1);
                                            events.push((frame_idx, Event::DeathMsg {
                                                killer_name: slot_name(kslot),
                                                killer_slot: kslot,
                                                victim_name: slot_name(vslot),
                                                victim_slot: vslot,
                                                weapon: format!("{:?}", d.weapon),
                                            }));
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
        }
    }

    // Find all disconnect events and print ±20 event window
    let disconnect_indices: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(i, (_, e))| matches!(e, Event::Disconnect { .. }).then_some(i))
        .collect();

    if disconnect_indices.is_empty() {
        println!("No disconnect events found.");
        return;
    }

    println!("Found {} disconnect(s).\n", disconnect_indices.len());

    for dc_idx in &disconnect_indices {
        let start = dc_idx.saturating_sub(20);
        let end = (dc_idx + 21).min(events.len());
        let (_, dc_event) = &events[*dc_idx];

        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("DISCONNECT #{dc_idx}: {:?}", dc_event);
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

        for i in start..end {
            let (frame, event) = &events[i];
            let marker = if i == *dc_idx { ">>>" } else { "   " };
            println!("{marker} {:>8}f  {:?}", frame, event);
        }
        println!();
    }

    println!("\nTotal events captured: {}", events.len());
}
