//! Which sounds a demo carries as `svc_sound`, and whether they name the
//! entity that made them.
//!
//! `goldsrc-hooks`' animation fix drives the firing animation off a weapon-fire
//! sound, because that sound names the shooter and the viewmodel state does
//! not. It hooks `pEventAPI->EV_PlaySound`, which only sees sounds played by
//! *event scripts*. A sound the server emits directly arrives as `svc_sound`
//! instead and is invisible to that hook.
//!
//! So before hanging another animation off another sound -- the grenade pin
//! pull is the open case, since nothing replicated marks it -- the question is
//! which carrier it uses, and whether an HLTV recording keeps the entity index
//! that makes it attributable to the player being watched.
//!
//! Pass a filter to narrow the listing to sounds worth looking at:
//!
//!     cargo run --release -p analysis --example svc_sound_probe -- <demo> [substring]

use dem::bit::BitSliceCast;
use dem::open_demo_from_bytes;
use dem::types::{EngineMessage, FrameData, MessageData, NetMessage};
use std::collections::HashMap;

#[derive(Default)]
struct Played {
    total: usize,
    /// How many carried an entity index that could identify a player. Entity 0
    /// is the world, which is no use for attribution.
    with_entity: usize,
    /// Distinct entities heard making it, capped for reporting.
    entities: Vec<u32>,
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: svc_sound_probe <demo> [substring]");
    let filter = std::env::args().nth(2).unwrap_or_default().to_lowercase();
    let bytes = std::fs::read(&path).expect("read demo");
    let demo = open_demo_from_bytes(&bytes).expect("parse demo");

    // Precache index -> sound name, from SvcResourceList (type 0 == t_sound).
    let mut sound_names: HashMap<u32, String> = HashMap::new();
    let mut played: HashMap<String, Played> = HashMap::new();
    let mut unnamed = 0usize;

    for entry in &demo.directory.entries {
        for frame in &entry.frames {
            let FrameData::NetworkMessage(bt) = &frame.frame_data else { continue };
            let MessageData::Parsed(msgs) = &bt.1.messages else { continue };
            for m in msgs {
                let NetMessage::EngineMessage(em) = m else { continue };
                match &**em {
                    EngineMessage::SvcResourceList(rl) => {
                        for r in &rl.resources {
                            if r.type_.to_u8() == 0 {
                                sound_names.insert(
                                    r.index.to_u32(),
                                    r.name
                                        .get_string()
                                        .trim_matches(|c: char| c == '\0' || c.is_whitespace())
                                        .to_string(),
                                );
                            }
                        }
                    }
                    EngineMessage::SvcSound(s) => {
                        let index = s
                            .sound_index_long
                            .as_ref()
                            .map(|b| b.to_u32())
                            .or_else(|| s.sound_index_short.as_ref().map(|b| b.to_u32()));
                        let Some(index) = index else {
                            unnamed += 1;
                            continue;
                        };
                        let Some(name) = sound_names.get(&index) else {
                            unnamed += 1;
                            continue;
                        };
                        let entity = s.entity_index.to_u32();
                        let e = played.entry(name.clone()).or_default();
                        e.total += 1;
                        if entity > 0 {
                            e.with_entity += 1;
                            if e.entities.len() < 8 && !e.entities.contains(&entity) {
                                e.entities.push(entity);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    println!("=== {path} ===");
    println!("{} sounds precached, {unnamed} svc_sound with no resolvable name\n", sound_names.len());

    let mut rows: Vec<(String, Played)> =
        played.into_iter().filter(|(n, _)| filter.is_empty() || n.to_lowercase().contains(&filter)).collect();
    rows.sort_by_key(|(_, p)| std::cmp::Reverse(p.total));

    if rows.is_empty() {
        println!("no svc_sound matching {filter:?} -- it is not carried on this path at all");
        return;
    }
    println!("{:<44} {:>7} {:>12}  entities", "sound", "played", "with entity");
    for (name, p) in rows.iter().take(30) {
        let mut ents: Vec<u32> = p.entities.clone();
        ents.sort_unstable();
        println!("{name:<44} {:>7} {:>12}  {ents:?}", p.total, p.with_entity);
    }
}
