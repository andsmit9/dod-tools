//! Names the entities an HLTV demo actually puts on the wire, so a map's
//! entity lump can be trimmed against measurement rather than guesswork.
//!
//! `dod_lennon2` breaches the pre-Anniversary `MAX_PACKET_ENTITIES` of 256 from
//! its very first snapshot (#207). The BSP's entity lump holds 662 entities,
//! but only some of those become networked edicts, so "which ~276 are on the
//! wire" is the question a strip-list has to answer first.
//!
//! Resolves every entity in the first full `svc_packetentities` snapshot to its
//! precached model name via `svc_spawnbaseline` + `svc_resourcelist`, and
//! reports the baseline origin alongside it. Brush entities resolve to `*N`,
//! which maps one-to-one onto a `"model" "*N"` key in the entity lump; point
//! entities resolve to a `.mdl`/`.spr` path plus an origin, which matches the
//! lump on coordinates.
//!
//!     cargo run --release -p analysis --example map_entity_probe -- <demo>

use dem::bit::BitSliceCast;
use dem::open_demo_from_bytes;
use dem::types::{EngineMessage, FrameData, MessageData, NetMessage};
use std::collections::{BTreeSet, HashMap};

fn delta_i32(delta: &dem::types::Delta, field: &str) -> Option<i32> {
    delta
        .iter()
        .find(|(k, _)| k.trim_matches(|c: char| c == '\0' || c.is_whitespace()) == field)
        .and_then(|(_, v)| (v.len() >= 4).then(|| i32::from_le_bytes([v[0], v[1], v[2], v[3]])))
}

fn delta_f32(delta: &dem::types::Delta, field: &str) -> Option<f32> {
    delta
        .iter()
        .find(|(k, _)| k.trim_matches(|c: char| c == '\0' || c.is_whitespace()) == field)
        .and_then(|(_, v)| (v.len() >= 4).then(|| f32::from_le_bytes([v[0], v[1], v[2], v[3]])))
}

/// One networked entity as the demo describes it at spawn.
#[derive(Default)]
struct Baseline {
    modelindex: Option<i32>,
    origin: [Option<f32>; 3],
    sequence: Option<i32>,
    skin: Option<i32>,
    effects: Option<i32>,
    solid: Option<i32>,
    rendermode: Option<i32>,
    renderamt: Option<i32>,
}

fn baseline_of(delta: &dem::types::Delta) -> Baseline {
    Baseline {
        modelindex: delta_i32(delta, "modelindex"),
        origin: [
            delta_f32(delta, "origin[0]"),
            delta_f32(delta, "origin[1]"),
            delta_f32(delta, "origin[2]"),
        ],
        sequence: delta_i32(delta, "sequence"),
        skin: delta_i32(delta, "skin"),
        effects: delta_i32(delta, "effects"),
        solid: delta_i32(delta, "solid"),
        rendermode: delta_i32(delta, "rendermode"),
        renderamt: delta_i32(delta, "renderamt"),
    }
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: map_entity_probe <demo>");
    let bytes = std::fs::read(&path).expect("read demo");
    let demo = open_demo_from_bytes(&bytes).expect("parse demo");

    // Precache index -> model name. `t_model` is 2 in `resourcetype_t`.
    let mut models: HashMap<u32, String> = HashMap::new();
    let mut baselines: HashMap<u16, Baseline> = HashMap::new();
    // Entity indices in the first full snapshot, in wire order.
    let mut snapshot: Vec<u16> = Vec::new();
    let mut snapshot_seen = false;
    // Field names actually present, to keep the delta-key guesswork honest.
    let mut field_names: BTreeSet<String> = BTreeSet::new();

    let mut stop = false;
    for entry in &demo.directory.entries {
        if stop {
            break;
        }
        for frame in &entry.frames {
            if stop {
                break;
            }
            let FrameData::NetworkMessage(bt) = &frame.frame_data else { continue };
            let MessageData::Parsed(msgs) = &bt.1.messages else { continue };
            for m in msgs {
                let NetMessage::EngineMessage(em) = m else { continue };
                match &**em {
                    EngineMessage::SvcResourceList(rl) => {
                        for r in &rl.resources {
                            if r.type_.to_u8() == 2 {
                                // Resource names arrive null-padded off the
                                // wire; left in place they make the report a
                                // binary file to every text tool downstream.
                                let name = r.name.get_string();
                                models.insert(
                                    r.index.to_u32(),
                                    name.trim_matches(|c: char| c == '\0').to_string(),
                                );
                            }
                        }
                    }
                    EngineMessage::SvcSpawnBaseline(sb) => {
                        for e in &sb.entities {
                            for k in e.delta.keys() {
                                field_names.insert(k.trim_matches(|c: char| c == '\0').to_string());
                            }
                            baselines.insert(e.entity_index, baseline_of(&e.delta));
                        }
                    }
                    EngineMessage::SvcPacketEntities(pe) => {
                        if snapshot_seen {
                            continue;
                        }
                        snapshot_seen = true;
                        for e in &pe.entity_states {
                            // A delta in the snapshot overrides the baseline; an
                            // entity that has not moved carries no fields at
                            // all, which is why the baseline is the source of
                            // truth for identity.
                            if let Some(mi) = delta_i32(&e.delta, "modelindex") {
                                baselines.entry(e.entity_index).or_default().modelindex = Some(mi);
                            }
                            snapshot.push(e.entity_index);
                        }
                        stop = true;
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    println!("demo: {path}");
    println!("model precache entries: {}", models.len());
    println!("spawn baselines: {}", baselines.len());
    println!("first full snapshot: {} entities", snapshot.len());
    println!(
        "baseline delta fields seen: {}\n",
        field_names.iter().cloned().collect::<Vec<_>>().join(", ")
    );

    // Per-entity detail, in entity-index order: this is what a strip-list is
    // written against.
    println!("{:>5}  {:>5}  {:<34} {:>26}  flags", "ent", "mdlix", "model", "origin");
    let mut ordered = snapshot.clone();
    ordered.sort_unstable();
    for idx in &ordered {
        let b = baselines.get(idx);
        let mi = b.and_then(|b| b.modelindex).unwrap_or(-1);
        let name = models.get(&(mi as u32)).cloned().unwrap_or_else(|| "<unknown>".into());
        let o = b.map(|b| b.origin).unwrap_or([None; 3]);
        let origin = format!(
            "{:>8.1} {:>8.1} {:>8.1}",
            o[0].unwrap_or(0.0),
            o[1].unwrap_or(0.0),
            o[2].unwrap_or(0.0)
        );
        let mut flags = String::new();
        if let Some(b) = b {
            if let Some(s) = b.sequence.filter(|v| *v != 0) {
                flags.push_str(&format!("seq={s} "));
            }
            if let Some(s) = b.skin.filter(|v| *v != 0) {
                flags.push_str(&format!("skin={s} "));
            }
            if let Some(s) = b.effects.filter(|v| *v != 0) {
                flags.push_str(&format!("fx={s} "));
            }
            if let Some(s) = b.solid.filter(|v| *v != 0) {
                flags.push_str(&format!("solid={s} "));
            }
            if let Some(s) = b.rendermode.filter(|v| *v != 0) {
                // renderamt only means anything alongside a rendermode, and it
                // is the field that separates "tinted" from "invisible".
                flags.push_str(&format!("rendermode={s} renderamt={} ", b.renderamt.unwrap_or(0)));
            }
        }
        println!("{idx:>5}  {mi:>5}  {name:<34} {origin}  {flags}");
    }

    // Roll-up by model, which is how a strip-list gets proposed: 145 env_models
    // sharing a handful of .mdl files is a very different decision from 145
    // distinct ones.
    let mut by_model: HashMap<String, usize> = HashMap::new();
    for idx in &snapshot {
        let mi = baselines.get(idx).and_then(|b| b.modelindex).unwrap_or(-1);
        let name = models.get(&(mi as u32)).cloned().unwrap_or_else(|| "<unknown>".into());
        let key = if name.starts_with('*') { "*<brush submodel>".to_string() } else { name };
        *by_model.entry(key).or_default() += 1;
    }
    let mut rolled: Vec<_> = by_model.into_iter().collect();
    rolled.sort_by_key(|(n, c)| (std::cmp::Reverse(*c), n.clone()));
    println!("\nby model:");
    for (name, count) in rolled {
        println!("  {count:>4}  {name}");
    }
}
