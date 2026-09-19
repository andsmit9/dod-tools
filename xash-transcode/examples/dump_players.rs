//! Throwaway diagnostic: for player entities (index <= max), show what the
//! accumulated state holds at the cut point, and what the raw stream sends
//! for them in the first seconds after it.
//!
//!   cargo run --release --example dump_players -- <demo.dem> <cut_s> [window_s] [max_index]
use std::collections::BTreeMap;
use std::env;

use dem::types::{Demo, EngineMessage, FrameData, MessageData, MessageDataParseMode, NetMessage};

fn fmt(v: &[u8]) -> String {
    match v.len() {
        4 => {
            let f = f32::from_le_bytes([v[0], v[1], v[2], v[3]]);
            let i = i32::from_le_bytes([v[0], v[1], v[2], v[3]]);
            if f.is_finite() && f.abs() < 1e6 && f.fract() != 0.0 {
                format!("{f:.1}")
            } else if f.is_finite() && f.abs() < 1e6 && i.abs() > 100000 {
                format!("{f:.1}")
            } else {
                format!("{i}")
            }
        }
        _ => format!("{v:?}"),
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let path = &args[1];
    let cut: f32 = args[2].parse().unwrap();
    let window: f32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(2.0);
    let max_index: u16 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(32);

    let bytes = std::fs::read(path).unwrap();
    let demo = Demo::parse_from_bytes(&bytes, MessageDataParseMode::Parse).unwrap();

    for entry in &demo.directory.entries {
        if entry.type_ != 1 {
            continue;
        }
        let mut table: BTreeMap<u16, BTreeMap<String, Vec<u8>>> = BTreeMap::new();
        let mut printed_baseline = false;

        for f in &entry.frames {
            let FrameData::NetworkMessage(boxed) = &f.frame_data else { continue };
            let MessageData::Parsed(messages) = &boxed.1.messages else { continue };

            if f.time >= cut && !printed_baseline {
                printed_baseline = true;
                println!("=== accumulated state at t={cut} (player entities <= {max_index}) ===");
                for (idx, fields) in &table {
                    if *idx > max_index { continue; }
                    let mut keys: Vec<_> = fields.keys().cloned().collect();
                    keys.sort();
                    let origin: Vec<String> = keys.iter().filter(|k| k.starts_with("origin")).map(|k| format!("{k}={}", fmt(&fields[k]))).collect();
                    let model = fields.get("modelindex").map(|v| fmt(v)).unwrap_or("-".into());
                    println!("  ent {idx:3}: modelindex={model} {} ({} fields)", origin.join(" "), fields.len());
                }
                println!("=== raw stream deltas for players, t in [{cut}, {}) ===", cut + window);
            }
            if f.time >= cut + window {
                break;
            }

            for m in messages {
                let NetMessage::EngineMessage(engine) = m else { continue };
                match engine.as_ref() {
                    EngineMessage::SvcPacketEntities(full) => {
                        for ent in &full.entity_states {
                            table.entry(ent.entity_index).or_default().extend(ent.delta.iter().map(|(k, v)| (k.clone(), v.clone())));
                        }
                        if f.time >= cut { println!("  [{:.3}] FULL packetentities ({} ents)", f.time, full.entity_states.len()); }
                    }
                    EngineMessage::SvcDeltaPacketEntities(d) => {
                        for ent in &d.entity_states {
                            if ent.remove_entity {
                                table.remove(&ent.entity_index);
                                if f.time >= cut && ent.entity_index <= max_index {
                                    println!("  [{:.3}] ent {:3} REMOVE", f.time, ent.entity_index);
                                }
                                continue;
                            }
                            if let Some(delta) = &ent.delta {
                                table.entry(ent.entity_index).or_default().extend(delta.iter().map(|(k, v)| (k.clone(), v.clone())));
                                if f.time >= cut && ent.entity_index <= max_index {
                                    let mut keys: Vec<_> = delta.keys().cloned().collect();
                                    keys.sort();
                                    let shown: Vec<String> = keys.iter().filter(|k| k.starts_with("origin") || *k == "modelindex").map(|k| format!("{k}={}", fmt(&delta[k]))).collect();
                                    println!("  [{:.3}] ent {:3} abs={} {} (+{} other fields)", f.time, ent.entity_index, ent.is_absolute_entity_index, shown.join(" "), delta.len() - shown.len());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}
