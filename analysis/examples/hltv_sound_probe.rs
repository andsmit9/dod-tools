//! Checks whether GoldSrc weapon-fire Events (SvcEvent/SvcEventReliable) are
//! actually present in an HLTV demo's raw netmessage stream, to tell apart
//! "sound is missing because the fire event was never recorded" from "sound
//! is missing despite the event being there" (a real, fixable playback bug).
//!
//!     cargo run --release -p analysis --example hltv_sound_probe -- demo.dem

use dem::bit::BitSliceCast;
use dem::open_demo_from_bytes;
use dem::types::{EngineMessage, FrameData, MessageData, NetMessage};
use dod::UserMessage;
use std::collections::HashMap;

fn main() {
    let path = std::env::args().nth(1).expect("usage: hltv_sound_probe <demo>");
    let bytes = std::fs::read(&path).expect("read demo");
    let demo = open_demo_from_bytes(&bytes).expect("parse demo");

    // event_index -> event script name, built from SvcResourceList (type 5 == eventscript).
    let mut event_names: HashMap<u32, String> = HashMap::new();
    let mut frame_no = 0usize;
    let mut event_hist: HashMap<u32, usize> = HashMap::new();
    let mut fire_frames: Vec<(usize, u32)> = Vec::new(); // (frame_no, event_index)
    let mut kill_frames: Vec<(usize, String)> = Vec::new(); // (frame_no, weapon)

    for entry in &demo.directory.entries {
        for frame in &entry.frames {
            frame_no += 1;
            let FrameData::NetworkMessage(bt) = &frame.frame_data else { continue };
            let MessageData::Parsed(msgs) = &bt.1.messages else { continue };
            for m in msgs {
                match m {
                    NetMessage::EngineMessage(em) => match &**em {
                        EngineMessage::SvcResourceList(rl) => {
                            for r in &rl.resources {
                                if r.type_.to_u8() == 5 {
                                    event_names.insert(r.index.to_u32(), r.name.get_string());
                                }
                            }
                        }
                        EngineMessage::SvcEvent(ev) => {
                            for e in &ev.events {
                                let idx = e.event_index.to_u32();
                                *event_hist.entry(idx).or_insert(0) += 1;
                                fire_frames.push((frame_no, idx));
                            }
                        }
                        EngineMessage::SvcEventReliable(ev) => {
                            let idx = ev.event_index.to_u32();
                            *event_hist.entry(idx).or_insert(0) += 1;
                            fire_frames.push((frame_no, idx));
                        }
                        _ => {}
                    },
                    NetMessage::UserMessage(um) => {
                        if let Ok(UserMessage::DeathMsg(d)) = UserMessage::new(&um.name, &um.data)
                            && d.killer_client_index != d.victim_client_index && d.killer_client_index > 0 {
                                kill_frames.push((frame_no, format!("{:?}", d.weapon)));
                            }
                    }
                }
            }
        }
    }

    println!("=== {} ===", path);
    println!("total frames: {}", frame_no);
    println!("total fire-type events (SvcEvent+SvcEventReliable): {}", fire_frames.len());
    println!("distinct event indices seen: {}", event_hist.len());
    println!("event script resources named via SvcResourceList: {}", event_names.len());
    println!("\nEvent histogram (index -> name -> count), sorted by count desc:");
    let mut hist: Vec<(&u32, &usize)> = event_hist.iter().collect();
    hist.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
    for (idx, count) in hist.iter().take(40) {
        let name = event_names.get(idx).cloned().unwrap_or_else(|| "<unknown>".to_string());
        println!("  idx={:<4} count={:<6} {}", idx, count, name);
    }

    println!("\n=== Cross-check: kills vs. nearby fire events ===");
    println!("total kills found: {}", kill_frames.len());
    let window = 200usize; // frames
    let mut matched = 0usize;
    let mut unmatched_examples: Vec<(usize, String)> = Vec::new();
    for (kf, weapon) in &kill_frames {
        let has_nearby_event = fire_frames.iter().any(|(ff, _)| ff.abs_diff(*kf) <= window);
        if has_nearby_event {
            matched += 1;
        } else if unmatched_examples.len() < 15 {
            unmatched_examples.push((*kf, weapon.clone()));
        }
    }
    println!(
        "kills with ANY fire-type event within +/-{} frames: {} / {} ({:.1}%)",
        window,
        matched,
        kill_frames.len(),
        100.0 * matched as f64 / kill_frames.len().max(1) as f64
    );
    println!("\nExamples of kills with NO nearby fire event (frame, weapon):");
    for (f, w) in &unmatched_examples {
        println!("  f{:<8} {}", f, w);
    }
}
