//! Measures the peak packet-entity count in a demo, against the engine limit
//! that crashes playback.
//!
//! `CL_ParsePacketEntities` aborts when a snapshot carries more than
//! MAX_PACKET_ENTITIES. That constant is 256 on the pre-Anniversary engine and
//! 1024 on the Anniversary one (read straight out of each `hw.dll`), which is
//! why some HLTV demos play on one and close the other to desktop. See #207.
//!
//! Reports the worst snapshot in the file and where it occurs, so a demo can
//! be checked before a capture batch spends time on it.
//!
//!     cargo run --release -p analysis --example packet_entity_probe -- <folder-or-demo>

use dem::bit::BitSliceCast;
use dem::open_demo_from_bytes;
use dem::types::{EngineMessage, FrameData, MessageData, NetMessage};

/// Pre-Anniversary MAX_PACKET_ENTITIES, the limit that actually bites.
const LEGACY_LIMIT: usize = 256;
/// Anniversary MAX_PACKET_ENTITIES.
const ANNIVERSARY_LIMIT: usize = 1024;

struct Peak {
    entities: usize,
    time: f32,
    over_legacy: usize,
    /// When the limit is first breached. The peak alone is misleading for
    /// diagnosis: playback dies at the *first* snapshot over the limit, not at
    /// the worst one, and the two can be twenty minutes apart.
    first_breach: Option<(f32, usize)>,
    /// The first few snapshots, to tell "over from the opening frame" apart
    /// from "climbs past the limit later".
    opening: Vec<usize>,
}

fn measure(bytes: &[u8]) -> Option<Peak> {
    let demo = open_demo_from_bytes(bytes).ok()?;
    let mut peak =
        Peak { entities: 0, time: 0.0, over_legacy: 0, first_breach: None, opening: Vec::new() };

    for entry in &demo.directory.entries {
        for frame in &entry.frames {
            let FrameData::NetworkMessage(bt) = &frame.frame_data else { continue };
            let MessageData::Parsed(msgs) = &bt.1.messages else { continue };
            for m in msgs {
                let NetMessage::EngineMessage(em) = m else { continue };
                let count = match &**em {
                    EngineMessage::SvcPacketEntities(pe) => pe.entity_count.to_u32() as usize,
                    EngineMessage::SvcDeltaPacketEntities(pe) => pe.entity_count.to_u32() as usize,
                    _ => continue,
                };
                if peak.opening.len() < 12 {
                    peak.opening.push(count);
                }
                if count > LEGACY_LIMIT {
                    peak.over_legacy += 1;
                    if peak.first_breach.is_none() {
                        peak.first_breach = Some((frame.time, count));
                    }
                }
                if count > peak.entities {
                    peak.entities = count;
                    peak.time = frame.time;
                }
            }
        }
    }
    Some(peak)
}

fn main() {
    let root = std::env::args().nth(1).expect("usage: packet_entity_probe <folder-or-demo>");
    let root = std::path::PathBuf::from(root);

    let mut demos: Vec<std::path::PathBuf> = Vec::new();
    if root.is_dir() {
        for entry in std::fs::read_dir(&root).expect("read dir").flatten() {
            let p = entry.path();
            if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dem")) {
                demos.push(p);
            }
        }
    } else {
        demos.push(root);
    }
    demos.sort();

    println!("{:<58} {:>6} {:>10} {:>9}  verdict", "demo", "peak", "at (demo s)", "over 256");
    for path in &demos {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        match measure(&bytes) {
            Some(p) => {
                let verdict = if p.entities > ANNIVERSARY_LIMIT {
                    "EXCEEDS BOTH"
                } else if p.entities > LEGACY_LIMIT {
                    "crashes pre-Anniversary"
                } else {
                    "ok"
                };
                println!("{name:<58} {:>6} {:>10.1} {:>9}  {verdict}", p.entities, p.time, p.over_legacy);
                if let Some((t, c)) = p.first_breach {
                    println!("    first over {LEGACY_LIMIT} at demo t={t:.1}s ({c} entities); opening snapshots: {:?}", p.opening);
                }
            }
            None => println!("{name:<58} {:>6}", "<unparseable>"),
        }
    }
}
