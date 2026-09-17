//! Which of a grenade viewmodel's two sequence families a throw uses, and the
//! order they arrive in.
//!
//! `v_grenade`, `v_stick` and `v_mills` all carry nine sequences in the same
//! order -- `idle draw pinpull holster throw`, then `exploding_idle
//! exploding_draw exploding_pinpull exploding_throw`. Both families are used
//! heavily in a real half, and counting them alone does not say why.
//!
//! The answer is in `docs/goldsrc_client_dll_internals.md` §9: the two families
//! belong to two mirrored weapon classes, `weapon_handgrenade` and
//! `weapon_handgrenade_ex` (short name `primgren` -- a primed grenade you get
//! by touching a live one on the ground). This probe is the measurement half of
//! that: it shows the two chains running in parallel, and it shows
//! `exploding_draw` firing once in three halves while the rest of the exploding
//! family is used as often as the plain one -- which is how the silent switch
//! into the primed weapon gives itself away.
//!
//! Run it on POV demos; an HLTV demo carries no viewmodel animation at all (see
//! `weapon_anim_probe`).
//!
//!     cargo run --release -p analysis --example grenade_family_probe -- <pov-demo>...

use dem::bit::BitSliceCast;
use dem::open_demo_from_bytes;
use dem::types::{EngineMessage, FrameData, MessageData, NetMessage};
use std::collections::{BTreeMap, HashMap};

/// Sequence names, shared by all three grenade viewmodels.
const SEQ: [&str; 9] = [
    "idle",
    "draw",
    "pinpull",
    "holster",
    "throw",
    "exploding_idle",
    "exploding_draw",
    "exploding_pinpull",
    "exploding_throw",
];

fn seq_name(n: i32) -> String {
    SEQ.get(n as usize).map(|s| s.to_string()).unwrap_or_else(|| format!("seq{n}"))
}

fn delta_u32(delta: &dem::types::Delta, field: &str) -> Option<u32> {
    delta
        .iter()
        .find(|(k, _)| k.trim_matches(|c: char| c == '\0' || c.is_whitespace()) == field)
        .and_then(|(_, v)| (v.len() >= 4).then(|| u32::from_le_bytes([v[0], v[1], v[2], v[3]])))
}

fn main() {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    assert!(!paths.is_empty(), "usage: grenade_family_probe <pov-demo>...");

    // Transitions pooled across every demo given, since one half holds only a
    // few dozen throws.
    let mut transitions: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut totals: BTreeMap<String, usize> = BTreeMap::new();

    for path in &paths {
        let Ok(bytes) = std::fs::read(path) else {
            println!("{path}: unreadable");
            continue;
        };
        let Ok(demo) = open_demo_from_bytes(&bytes) else {
            println!("{path}: unparseable");
            continue;
        };

        let mut model_names: HashMap<u32, String> = HashMap::new();
        let mut viewmodel = String::new();
        // (time, sequence) for grenade viewmodels only, in order.
        let mut stream: Vec<(f32, i32)> = Vec::new();

        for entry in &demo.directory.entries {
            for frame in &entry.frames {
                // A grenade animation reaches the recording twice, as a
                // `Dem_WeaponAnim` frame and as `svc_weaponanim`. Taking only
                // the frame avoids counting each throw as two events.
                if let FrameData::WeaponAnimation(wa) = &frame.frame_data
                    && is_grenade(&viewmodel)
                {
                    stream.push((frame.time, wa.anim));
                }
                let FrameData::NetworkMessage(bt) = &frame.frame_data else { continue };
                let MessageData::Parsed(msgs) = &bt.1.messages else { continue };
                for m in msgs {
                    let NetMessage::EngineMessage(em) = m else { continue };
                    match &**em {
                        EngineMessage::SvcResourceList(rl) => {
                            for r in &rl.resources {
                                if r.type_.to_u8() == 2 {
                                    model_names.insert(
                                        r.index.to_u32(),
                                        r.name
                                            .get_string()
                                            .trim_matches(|c: char| c == '\0')
                                            .to_string(),
                                    );
                                }
                            }
                        }
                        EngineMessage::SvcClientData(cd) => {
                            if let Some(idx) = delta_u32(&cd.client_data, "viewmodel")
                                && let Some(name) = model_names.get(&idx)
                            {
                                viewmodel = name.clone();
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        let name = std::path::Path::new(path).file_name().unwrap_or_default().to_string_lossy();
        println!("=== {name}: {} grenade animations", stream.len());

        // The first two dozen in order, which is where the state machine shows
        // itself.
        for (t, s) in stream.iter().take(24) {
            println!("  {t:8.2}  {}", seq_name(*s));
        }
        if stream.len() > 24 {
            println!("  ... {} more", stream.len() - 24);
        }

        for w in stream.windows(2) {
            // A gap longer than this is a different grenade, not a step in the
            // same throw.
            if w[1].0 - w[0].0 > 20.0 {
                continue;
            }
            *transitions.entry((seq_name(w[0].1), seq_name(w[1].1))).or_default() += 1;
        }
        for (_, s) in &stream {
            *totals.entry(seq_name(*s)).or_default() += 1;
        }
        println!();
    }

    println!("pooled sequence totals:");
    for (s, n) in &totals {
        println!("  {s:<20} {n}");
    }

    println!("\npooled transitions (from -> to: count):");
    let mut rows: Vec<_> = transitions.into_iter().collect();
    rows.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for ((a, b), n) in rows {
        println!("  {a:<20} -> {b:<20} {n}");
    }
}

fn is_grenade(model: &str) -> bool {
    model.contains("v_grenade") || model.contains("v_stick") || model.contains("v_mills")
}
