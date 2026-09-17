//! When does a grenade thrower's *body* animation change, relative to the
//! throw itself?
//!
//! DoD's grenade viewmodels animate `idle -> pinpull -> (cook) -> throw`, and a
//! POV demo plays all of it. A spectated player gets none of it, and the pin
//! pull has no replicated marker: the third-person grenade models carry exactly
//! one sequence each, and `weapons/grenpinpull.wav` appears in no demo's
//! `svc_sound` stream, POV or HLTV -- it is played client-side for the local
//! player only, like the animation.
//!
//! What *is* available is the player's own body sequence, which is replicated,
//! and `weapons/grenthrow.wav`, which an HLTV demo carries with the throwing
//! player's entity index. If the body sequence changes to a grenade attack
//! *before* the throw sound, that gap is the wind-up, and it is the pin-pull
//! moment a spectated viewmodel could be driven from. If the two land together,
//! there is nothing to drive it with and only the throw can be reproduced.
//!
//!     cargo run --release -p analysis --example grenade_timing_probe -- <demo>

use dem::bit::BitSliceCast;
use dem::open_demo_from_bytes;
use dem::types::{EngineMessage, FrameData, MessageData, NetMessage};
use std::collections::HashMap;

/// Grenade attack sequences in DoD's player models, read out of
/// `models/player/us-inf/us-inf.mdl` (345 sequences, shared numbering across
/// the player models). `_shoot` is the overhand throw, `_roll` the underhand.
const GRENADE_ATTACK_SEQUENCES: &[u32] = &[
    82, 83, 84, // stand/crouch/prone_gren_shoot
    85, 86, // stand/crouch_gren_roll
    87, 88, 89, // stand/crouch/prone_stick_shoot
    90, 91, // stand/crouch_stick_roll
    92, 93, 96, // stand/crouch/prone_mills_shoot
    94, 95, // stand/crouch_mills_roll
];

fn delta_u32(delta: &dem::types::Delta, field: &str) -> Option<u32> {
    delta
        .iter()
        .find(|(k, _)| k.trim_matches(|c: char| c == '\0' || c.is_whitespace()) == field)
        .and_then(|(_, v)| (v.len() >= 4).then(|| u32::from_le_bytes([v[0], v[1], v[2], v[3]])))
}

#[allow(clippy::too_many_arguments)]
fn note_sequence(
    entity: u16,
    sequence: Option<u32>,
    time: f32,
    last_sequence: &mut HashMap<u16, u32>,
    windup_at: &mut HashMap<u16, f32>,
    seen: &mut HashMap<u32, usize>,
) {
    let Some(seq) = sequence else { return };
    *seen.entry(seq).or_insert(0) += 1;
    let previous = last_sequence.insert(entity, seq);
    if previous != Some(seq) && GRENADE_ATTACK_SEQUENCES.contains(&seq) {
        windup_at.insert(entity, time);
    }
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: grenade_timing_probe <demo>");
    let bytes = std::fs::read(&path).expect("read demo");
    let demo = open_demo_from_bytes(&bytes).expect("parse demo");

    let mut sound_names: HashMap<u32, String> = HashMap::new();
    // entity -> (last sequence seen, time it changed to a grenade attack)
    let mut last_sequence: HashMap<u16, u32> = HashMap::new();
    let mut windup_at: HashMap<u16, f32> = HashMap::new();
    let mut gaps: Vec<f32> = Vec::new();
    let mut throws = 0usize;
    let mut throws_with_windup = 0usize;
    let mut seen_sequences: HashMap<u32, usize> = HashMap::new();

    // POV side: what the engine actually played on the local viewmodel, and
    // when, so the body-sequence gap above can be checked against it.
    let mut model_names: HashMap<u32, String> = HashMap::new();
    let mut viewmodel = String::new();
    let mut last_pinpull: Option<f32> = None;
    let mut cook_gaps: Vec<f32> = Vec::new();

    for entry in &demo.directory.entries {
        for frame in &entry.frames {
            // Grenade viewmodels share one layout: 2 pinpull, 4 throw,
            // 7 exploding_pinpull, 8 exploding_throw. Checked before the
            // network-message guard below, which would otherwise `continue`
            // past every weapon-animation frame in the file.
            if let FrameData::WeaponAnimation(wa) = &frame.frame_data
                && (viewmodel.contains("v_grenade")
                    || viewmodel.contains("v_stick")
                    || viewmodel.contains("v_mills"))
            {
                match wa.anim {
                    2 | 7 => last_pinpull = Some(frame.time),
                    4 | 8 => {
                        if let Some(pull) = last_pinpull.take() {
                            let g = frame.time - pull;
                            if (0.0..=15.0).contains(&g) {
                                cook_gaps.push(g);
                            }
                        }
                    }
                    _ => {}
                }
            }

            let FrameData::NetworkMessage(bt) = &frame.frame_data else { continue };
            let MessageData::Parsed(msgs) = &bt.1.messages else { continue };
            for m in msgs {
                let NetMessage::EngineMessage(em) = m else { continue };
                match &**em {
                    EngineMessage::SvcResourceList(rl) => {
                        for r in &rl.resources {
                            if r.type_.to_u8() == 2 {
                                model_names.insert(r.index.to_u32(), r.name.get_string());
                            }
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
                    // Both carriers matter: an HLTV stream is almost entirely
                    // delta packets, so handling only the full ones finds
                    // nothing at all.
                    EngineMessage::SvcPacketEntities(pe) => {
                        for es in &pe.entity_states {
                            note_sequence(
                                es.entity_index,
                                delta_u32(&es.delta, "sequence"),
                                frame.time,
                                &mut last_sequence,
                                &mut windup_at,
                                &mut seen_sequences,
                            );
                        }
                    }
                    EngineMessage::SvcDeltaPacketEntities(pe) => {
                        for es in &pe.entity_states {
                            let seq = es.delta.as_ref().and_then(|d| delta_u32(d, "sequence"));
                            note_sequence(
                                es.entity_index,
                                seq,
                                frame.time,
                                &mut last_sequence,
                                &mut windup_at,
                                &mut seen_sequences,
                            );
                        }
                    }
                    EngineMessage::SvcClientData(cd) => {
                        if let Some(idx) = delta_u32(&cd.client_data, "viewmodel")
                            && let Some(name) = model_names.get(&idx)
                        {
                            viewmodel =
                                name.trim_matches(|ch: char| ch == '\0' || ch.is_whitespace()).to_string();
                        }
                    }
                    EngineMessage::SvcSound(s) => {
                        let index = s
                            .sound_index_long
                            .as_ref()
                            .map(|b| b.to_u32())
                            .or_else(|| s.sound_index_short.as_ref().map(|b| b.to_u32()));
                        let Some(name) = index.and_then(|i| sound_names.get(&i)) else { continue };
                        if !name.contains("grenthrow") {
                            continue;
                        }
                        throws += 1;
                        let entity = s.entity_index.to_u32() as u16;
                        // Only a wind-up recent enough to belong to this throw.
                        if let Some(started) = windup_at.remove(&entity) {
                            let gap = frame.time - started;
                            if (0.0..=6.0).contains(&gap) {
                                throws_with_windup += 1;
                                gaps.push(gap);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    println!("=== {path} ===");
    let grenade_seqs: usize =
        seen_sequences.iter().filter(|(s, _)| GRENADE_ATTACK_SEQUENCES.contains(s)).map(|(_, n)| *n).sum();
    println!(
        "player sequence updates seen: {} ({} distinct), of which grenade attacks: {grenade_seqs}",
        seen_sequences.values().sum::<usize>(),
        seen_sequences.len()
    );
    println!("grenthrow sounds: {throws}");
    println!("preceded by a grenade body sequence: {throws_with_windup}");

    if gaps.is_empty() {
        println!("\nNo throw had a body-sequence wind-up before it. The body sequence is not a");
        println!("pin-pull signal, so only the throw itself can be reproduced.");
        return;
    }

    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pick = |f: f32| gaps[((gaps.len() - 1) as f32 * f) as usize];
    println!(
        "\nwind-up to throw: min {:.3}s  p25 {:.3}s  median {:.3}s  p75 {:.3}s  max {:.3}s",
        gaps[0],
        pick(0.25),
        pick(0.50),
        pick(0.75),
        gaps[gaps.len() - 1]
    );
    // A wind-up that lands in the same frame as the throw is not a signal --
    // there is no time to play anything before the throw.
    let same_frame = gaps.iter().filter(|g| **g < 0.05).count();
    println!("landing within 50ms of the throw (no usable lead): {same_frame} of {}", gaps.len());
    report_cook(&mut cook_gaps);
}

/// The gap between the viewmodel's `pinpull` and its `throw`, which only a POV
/// demo has. This is the player's actual cook time -- how long they held the
/// button -- and it is the number that decides whether the body-sequence gap
/// measured above is a pin pull or just an animation's wind-up.
fn report_cook(gaps: &mut [f32]) {
    if gaps.is_empty() {
        return;
    }
    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pick = |f: f32| gaps[((gaps.len() - 1) as f32 * f) as usize];
    println!(
        "
viewmodel pinpull -> throw ({} samples): min {:.3}s  p25 {:.3}s  median {:.3}s  p75 {:.3}s  max {:.3}s",
        gaps.len(),
        gaps[0],
        pick(0.25),
        pick(0.50),
        pick(0.75),
        gaps[gaps.len() - 1]
    );
}
