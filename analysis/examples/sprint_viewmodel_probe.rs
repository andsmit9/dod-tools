//! What DoD does to the first-person viewmodel while a player sprints.
//!
//! `goldsrc-hooks`' animation fix drives a spectated player's viewmodel from
//! replicated state, and was thought to be missing the weapon being lowered
//! during a sprint. It is not missing: **DoD 1.3 does nothing to the viewmodel
//! while sprinting.** This probe is half of the evidence -- the other half is
//! the disassembly in `docs/goldsrc_client_dll_internals.md` §8.
//!
//! There were three mechanisms it could have been:
//!
//!   1. the viewmodel is switched off (`clientdata.viewmodel` goes to 0),
//!   2. a sequence is played on it (`clientdata.weaponanim` changes), or
//!   3. a purely client-side transform in `V_CalcNormalRefdef`, which no demo
//!      could see and which would have to be synthesised.
//!
//! A POV demo is a recording of that machinery working, so it settles 1 and 2.
//! Sprints are found the way the client finds them: `HUD_ProcessPlayerState`
//! publishes `entity_state.fuser4` to the global the stamina bar and the sprint
//! grunt both read, so a falling `fuser4` is a sprint in progress. Across four
//! POV demos and ~500 sprints over half a second, `viewmodel` holds one
//! non-zero index and `weaponanim` holds one value through every one of them.
//! (Option 3 is ruled out separately: the view module reads neither
//! `in_speed.state`, nor `pparams->cmd`, nor the stamina global.)
//!
//!     cargo run --release -p analysis --example sprint_viewmodel_probe -- <pov-demo>

use dem::open_demo_from_bytes;
use dem::types::{EngineMessage, FrameData, MessageData, NetMessage};
use std::collections::{BTreeMap, BTreeSet};

fn field(delta: &dem::types::Delta, name: &str) -> Option<Vec<u8>> {
    delta
        .iter()
        .find(|(k, _)| k.trim_matches(|c: char| c == '\0' || c.is_whitespace()) == name)
        .map(|(_, v)| v.clone())
}

fn as_i32(v: &[u8]) -> Option<i32> {
    (v.len() >= 4).then(|| i32::from_le_bytes([v[0], v[1], v[2], v[3]]))
}

fn as_f32(v: &[u8]) -> Option<f32> {
    (v.len() >= 4).then(|| f32::from_le_bytes([v[0], v[1], v[2], v[3]]))
}

/// One `svc_clientdata` update, reduced to the fields that could carry a
/// sprint.
#[derive(Default, Clone, PartialEq)]
struct Snapshot {
    viewmodel: Option<i32>,
    weaponanim: Option<i32>,
    maxspeed: Option<f32>,
    fuser: [Option<f32>; 4],
    iuser: [Option<i32>; 4],
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: sprint_viewmodel_probe <demo>");
    let bytes = std::fs::read(&path).expect("read demo");
    let demo = open_demo_from_bytes(&bytes).expect("parse demo");

    // Every field name the clientdata delta actually carries, so the guesswork
    // about which one holds sprint state is checkable rather than assumed.
    let mut names: BTreeSet<String> = BTreeSet::new();
    // time -> snapshot, only when something changed.
    let mut timeline: Vec<(f32, Snapshot)> = Vec::new();
    let mut last = Snapshot::default();
    let mut anim_times: Vec<(f32, i32)> = Vec::new();
    let mut frame_anim_times: Vec<(f32, i32)> = Vec::new();
    let mut frame_anims: usize = 0;

    for entry in &demo.directory.entries {
        for frame in &entry.frames {
            if let FrameData::WeaponAnimation(wa) = &frame.frame_data {
                frame_anims += 1;
                frame_anim_times.push((frame.time, wa.anim));
            }
            let FrameData::NetworkMessage(bt) = &frame.frame_data else { continue };
            let MessageData::Parsed(msgs) = &bt.1.messages else { continue };
            for m in msgs {
                let NetMessage::EngineMessage(em) = m else { continue };
                match &**em {
                    EngineMessage::SvcWeaponAnim(wa) => {
                        anim_times.push((frame.time, wa.sequence_number as i32));
                    }
                    EngineMessage::SvcClientData(cd) => {
                        for k in cd.client_data.keys() {
                            names.insert(k.trim_matches(|c: char| c == '\0').to_string());
                        }
                        // clientdata arrives as a delta against the previous
                        // one, so a field that did not change is simply absent
                        // -- carry the last value forward rather than reading
                        // its absence as a zero.
                        let mut now = last.clone();
                        if let Some(v) = field(&cd.client_data, "viewmodel") {
                            now.viewmodel = as_i32(&v);
                        }
                        if let Some(v) = field(&cd.client_data, "weaponanim") {
                            now.weaponanim = as_i32(&v);
                        }
                        if let Some(v) = field(&cd.client_data, "maxspeed") {
                            now.maxspeed = as_f32(&v);
                        }
                        for n in 0..4 {
                            if let Some(v) = field(&cd.client_data, &format!("fuser{}", n + 1)) {
                                now.fuser[n] = as_f32(&v);
                            }
                            if let Some(v) = field(&cd.client_data, &format!("iuser{}", n + 1)) {
                                now.iuser[n] = as_i32(&v);
                            }
                        }
                        if now != last {
                            timeline.push((frame.time, now.clone()));
                            last = now;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    println!("demo: {path}");
    println!("clientdata updates that changed something: {}", timeline.len());
    println!("svc_weaponanim: {}   Dem_WeaponAnim frames: {frame_anims}", anim_times.len());
    println!("clientdata fields present: {}\n", names.into_iter().collect::<Vec<_>>().join(", "));

    // How often each field moves at all. A field that never changes cannot be
    // carrying a sprint.
    let mut changes: BTreeMap<&str, usize> = BTreeMap::new();
    for pair in timeline.windows(2) {
        let (a, b) = (&pair[0].1, &pair[1].1);
        if a.viewmodel != b.viewmodel {
            *changes.entry("viewmodel").or_default() += 1;
        }
        if a.weaponanim != b.weaponanim {
            *changes.entry("weaponanim").or_default() += 1;
        }
        if a.maxspeed != b.maxspeed {
            *changes.entry("maxspeed").or_default() += 1;
        }
        for n in 0..4 {
            if a.fuser[n] != b.fuser[n] {
                *changes.entry(["fuser1", "fuser2", "fuser3", "fuser4"][n]).or_default() += 1;
            }
            if a.iuser[n] != b.iuser[n] {
                *changes.entry(["iuser1", "iuser2", "iuser3", "iuser4"][n]).or_default() += 1;
            }
        }
    }
    println!("field changes over the demo:");
    for (k, n) in &changes {
        println!("  {k:<12} {n}");
    }

    // Distinct maxspeed values: DoD raises it while sprinting, so the set of
    // values is the first place a sprint would show up.
    let mut speeds: BTreeMap<String, usize> = BTreeMap::new();
    for (_, s) in &timeline {
        if let Some(v) = s.maxspeed {
            *speeds.entry(format!("{v:.1}")).or_default() += 1;
        }
    }
    println!("\ndistinct maxspeed values (count of updates at each):");
    for (v, n) in &speeds {
        println!("  {v:>8}  {n}");
    }

    // Whether the viewmodel is ever switched off, which is option 1.
    let off = timeline.iter().filter(|(_, s)| s.viewmodel == Some(0)).count();
    println!("\nclientdata updates with viewmodel == 0: {off}");
    let models: BTreeSet<i32> = timeline.iter().filter_map(|(_, s)| s.viewmodel).collect();
    println!("distinct viewmodel indices: {models:?}");

    // The first few maxspeed transitions with what the viewmodel did across
    // them -- the actual answer, if there is one.
    // Sprint intervals, from stamina. `client.dll`'s HUD_ProcessPlayerState
    // stores `entity_state.fuser4` into the global that both the stamina bar
    // and the sprint grunt read, so a falling fuser4 is a sprint in progress --
    // and it is the only handle a demo gives on when one happened.
    let mut sprints: Vec<(f32, f32)> = Vec::new();
    let mut run_start: Option<f32> = None;
    let mut prev: Option<(f32, f32)> = None;
    for (t, s) in &timeline {
        let Some(v) = s.fuser[3] else { continue };
        if let Some((pt, pv)) = prev {
            if v < pv - 0.01 {
                if run_start.is_none() {
                    run_start = Some(pt);
                }
            } else if let Some(start) = run_start {
                sprints.push((start, pt));
                run_start = None;
            }
        }
        prev = Some((*t, v));
    }
    if let (Some(start), Some((t, _))) = (run_start, prev) {
        sprints.push((start, t));
    }
    let long: Vec<_> = sprints.iter().filter(|(a, b)| b - a > 0.5).collect();
    println!(
        "\nstamina drains (fuser4 falling): {} total, {} longer than 0.5s",
        sprints.len(),
        long.len()
    );
    for (a, b) in long.iter().take(15) {
        let inside: Vec<_> = timeline.iter().filter(|(t, _)| t >= a && t <= b).collect();
        let vms: BTreeSet<i32> = inside.iter().filter_map(|(_, s)| s.viewmodel).collect();
        let anims: BTreeSet<i32> = inside.iter().filter_map(|(_, s)| s.weaponanim).collect();
        let animated = frame_anim_times.iter().filter(|(t, _)| t >= a && t <= b).count();
        println!(
            "  {a:8.2} .. {b:8.2}  ({:5.2}s)  viewmodel {vms:?}  weaponanim {anims:?}  Dem_WeaponAnim {animated}",
            b - a
        );
    }

    println!("\nfirst 20 maxspeed transitions:");
    let mut shown = 0;
    for pair in timeline.windows(2) {
        let (ta, a) = (&pair[0].0, &pair[0].1);
        let (tb, b) = (&pair[1].0, &pair[1].1);
        if a.maxspeed == b.maxspeed {
            continue;
        }
        println!(
            "  t={tb:8.2} (from {ta:8.2})  maxspeed {:?} -> {:?}   viewmodel {:?} -> {:?}   weaponanim {:?} -> {:?}",
            a.maxspeed, b.maxspeed, a.viewmodel, b.viewmodel, a.weaponanim, b.weaponanim
        );
        shown += 1;
        if shown >= 20 {
            break;
        }
    }
}
