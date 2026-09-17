//! Tallies the raw weapon-ID byte carried by every `DeathMsg` across a folder
//! of demos, so the IDs `dod::Weapon` leaves unmapped (15, 16, 33, 34, 41) can
//! be judged empirically rather than guessed at.
//!
//!     cargo run --release -p analysis --example weapon_id_probe -- <folder-or-demo>

use dem::open_demo_from_bytes;
use dem::types::{FrameData, MessageData, NetMessage};
use dod::{UserMessage, Weapon};
use std::collections::BTreeMap;

fn tally(bytes: &[u8], hist: &mut BTreeMap<u8, (usize, Weapon)>) {
    let Ok(demo) = open_demo_from_bytes(bytes) else { return };
    for entry in &demo.directory.entries {
        for frame in &entry.frames {
            let FrameData::NetworkMessage(bt) = &frame.frame_data else { continue };
            let MessageData::Parsed(msgs) = &bt.1.messages else { continue };
            for m in msgs {
                let NetMessage::UserMessage(um) = m else { continue };
                let name = um.name.split(|&b| b == 0).next().unwrap_or(&um.name);
                // DeathMsg payload is (killer u8, victim u8, weapon u8).
                if name != b"DeathMsg" || um.data.len() < 3 {
                    continue;
                }
                let Ok(UserMessage::DeathMsg(d)) = UserMessage::new(&um.name, &um.data) else { continue };
                let slot = hist.entry(um.data[2]).or_insert((0, d.weapon));
                slot.0 += 1;
            }
        }
    }
}

fn main() {
    let root = std::env::args().nth(1).expect("usage: weapon_id_probe <folder-or-demo>");
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

    let mut hist: BTreeMap<u8, (usize, Weapon)> = BTreeMap::new();
    let mut read_ok = 0usize;
    for path in &demos {
        let Ok(bytes) = std::fs::read(path) else { continue };
        read_ok += 1;
        tally(&bytes, &mut hist);
    }

    let total: usize = hist.values().map(|(c, _)| *c).sum();
    println!("scanned {read_ok}/{} demos, {total} DeathMsg records, {} distinct weapon IDs\n",
             demos.len(), hist.len());
    println!("{:>4}  {:>8}  {:>7}  dod::Weapon", "id", "count", "share");
    for (id, (count, weapon)) in &hist {
        // Unknown for any id other than 0 means the enum has no mapping for it.
        let note = if *weapon == Weapon::Unknown && *id != 0 { "  <-- UNMAPPED" } else { "" };
        println!("{id:>4}  {count:>8}  {:>6.2}%  {weapon:?}{note}",
                 100.0 * *count as f64 / total as f64);
    }
}
