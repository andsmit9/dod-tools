//! Finds where in a demo a player rapidly cycles weapons, on the demo's own
//! clock.
//!
//! Written to locate a moment seen live. `goldsrc-hooks`' log timestamps every
//! animation, but its clock is seconds since the *client* loaded, summed from
//! `host_frametime` -- it keeps counting while playback is paused, so it
//! cannot be converted into a position in the demo. `frame.time` here is the
//! demo's own, so it survives pausing, scrubbing and restarts.
//!
//! Reads the same replicated field the animation fix does:
//! `entity_state_t::weaponmodel`, resolved to a name through the resource list.
//! That is server state rather than a client-side guess, so what this prints is
//! what the spectator client was shown.
//!
//! Its first real use: locating a burst seen live in `goldsrc-hooks`' log. The
//! log's clock had been running across a pause, so the two could only be lined
//! up by matching the *shape* of a burst -- the weapons in order and the gaps
//! between them. That pinned the moment to within 7ms on every change, and the
//! offset it produced then checked out against a second burst in the same
//! session. Two independent bursts agreeing is what makes such a match
//! trustworthy; one on its own is a coincidence waiting to happen, and this
//! player alone produced 117 candidates in a single half.
//!
//!     cargo run --release -p analysis --example weapon_switch_probe -- <demo> [changes] [window]

use dem::bit::BitSliceCast;
use dem::open_demo_from_bytes;
use dem::types::{EngineMessage, FrameData, MessageData, NetMessage};
use std::collections::HashMap;

/// Delta field names arrive NUL-padded off the wire, so a plain
/// `get("weaponmodel")` silently misses every one of them -- the map really
/// holds `"weaponmodel\0"`. Trim before comparing.
fn delta_u32(delta: &HashMap<String, Vec<u8>>, key: &str) -> Option<u32> {
    delta
        .iter()
        .find(|(k, _)| k.trim_matches(|c: char| c == '\0' || c.is_whitespace()) == key)
        .and_then(|(_, v)| (v.len() >= 4).then(|| u32::from_le_bytes([v[0], v[1], v[2], v[3]])))
}

fn short(name: &str) -> String {
    name.rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim_end_matches(".mdl")
        .to_string()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: weapon_switch_probe <demo> [min changes] [window seconds]");
        std::process::exit(2);
    };
    let min_changes: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(4);
    let window: f32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(1.5);

    let bytes = std::fs::read(&path).expect("read demo");
    let demo = open_demo_from_bytes(&bytes).expect("parse demo");

    let mut model_names: HashMap<u32, String> = HashMap::new();
    // entity index -> (last weaponmodel index, every change as (time, name))
    let mut last: HashMap<u16, u32> = HashMap::new();
    let mut changes: HashMap<u16, Vec<(f32, String)>> = HashMap::new();

    for entry in &demo.directory.entries {
        for frame in &entry.frames {
            let FrameData::NetworkMessage(bt) = &frame.frame_data else { continue };
            let MessageData::Parsed(msgs) = &bt.1.messages else { continue };
            for m in msgs {
                let NetMessage::EngineMessage(em) = m else { continue };
                let states: Vec<(u16, Option<u32>)> = match &**em {
                    EngineMessage::SvcResourceList(rl) => {
                        for r in &rl.resources {
                            if r.type_.to_u8() == 2 {
                                model_names.insert(r.index.to_u32(), r.name.get_string());
                            }
                        }
                        continue;
                    }
                    EngineMessage::SvcPacketEntities(pe) => pe
                        .entity_states
                        .iter()
                        .map(|es| (es.entity_index, delta_u32(&es.delta, "weaponmodel")))
                        .collect(),
                    EngineMessage::SvcDeltaPacketEntities(pe) => pe
                        .entity_states
                        .iter()
                        .map(|es| {
                            (
                                es.entity_index,
                                es.delta.as_ref().and_then(|d| delta_u32(d, "weaponmodel")),
                            )
                        })
                        .collect(),
                    _ => continue,
                };

                for (idx, weapon) in states {
                    // Players only. 0 is the world; above 32 is everything the
                    // map and the weapons themselves put on the wire.
                    if idx == 0 || idx > 32 {
                        continue;
                    }
                    let Some(w) = weapon else { continue };
                    if last.get(&idx) == Some(&w) {
                        continue;
                    }
                    last.insert(idx, w);
                    let name = model_names.get(&w).map(|n| short(n)).unwrap_or_else(|| format!("#{w}"));
                    changes.entry(idx).or_default().push((frame.time, name));
                }
            }
        }
    }

    let total: usize = changes.values().map(|v| v.len()).sum();
    println!("tracked {} player entities, {total} weapon changes", changes.len());
    println!("weapon-switch bursts: {min_changes}+ changes inside {window}s\n");
    let mut found = 0;
    let mut keys: Vec<_> = changes.keys().copied().collect();
    keys.sort();
    for idx in keys {
        let list = &changes[&idx];
        let mut i = 0;
        while i < list.len() {
            let mut j = i;
            while j + 1 < list.len() && list[j + 1].0 - list[i].0 <= window {
                j += 1;
            }
            if j - i + 1 >= min_changes {
                found += 1;
                let span = list[j].0 - list[i].0;
                println!(
                    "  entity {idx:2}  demo {:8.3}s  ({:02}:{:05.2})  {} changes in {:.3}s",
                    list[i].0,
                    (list[i].0 as u32) / 60,
                    list[i].0 % 60.0,
                    j - i + 1,
                    span
                );
                for (t, n) in &list[i..=j] {
                    println!("        {t:9.3}  {n}");
                }
                i = j + 1;
            } else {
                i += 1;
            }
        }
    }
    if found == 0 {
        println!("  none -- try a lower threshold or a wider window");
    }
}
