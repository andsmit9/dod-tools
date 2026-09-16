//! Inventory every user message in a demo, with a payload-size histogram.
//!
//! Answers "what is actually on the wire, and how wide is each message" — the
//! probe that established DeathMsg is always exactly 3 bytes, so DoD 1.3
//! carries no hit group and headshots cannot be recovered from a demo.
//!
//!     cargo run --release -p analysis --example msg_probe -- path/to/demo.dem

use dem::open_demo_from_bytes;
use dem::types::{FrameData, MessageData, NetMessage};
use std::collections::BTreeMap;

fn main() {
    let path = std::env::args().nth(1).expect("usage: msg_probe <demo>");
    let bytes = std::fs::read(&path).expect("read demo");
    let demo = open_demo_from_bytes(&bytes).expect("parse demo");

    let mut counts: BTreeMap<String, (usize, BTreeMap<usize, usize>)> = BTreeMap::new();
    let mut samples: BTreeMap<String, Vec<Vec<u8>>> = BTreeMap::new();

    for entry in &demo.directory.entries {
        for frame in &entry.frames {
            if let FrameData::NetworkMessage(bt) = &frame.frame_data
                && let MessageData::Parsed(msgs) = &bt.1.messages {
                    for m in msgs {
                        if let NetMessage::UserMessage(um) = m {
                            let mut n: Vec<u8> = um.name.clone();
                            while n.last() == Some(&0) { n.pop(); }
                            let name = String::from_utf8_lossy(&n).to_string();
                            let e = counts.entry(name.clone()).or_default();
                            e.0 += 1;
                            *e.1.entry(um.data.len()).or_insert(0) += 1;
                            let s = samples.entry(name).or_default();
                            if s.len() < 3 { s.push(um.data.clone()); }
                        }
                    }
                }
        }
    }

    println!("== {} ==", path);
    println!("{:<16} {:>7}  size-histogram", "message", "count");
    for (name, (count, sizes)) in &counts {
        let hist: Vec<String> = sizes.iter().map(|(k, v)| format!("{}B x{}", k, v)).collect();
        println!("{:<16} {:>7}  {}", name, count, hist.join(", "));
    }

    println!("\n== samples ==");
    for key in ["CapMsg", "ObjScore", "DeathMsg", "InitObj", "SetObj", "Object", "StartProg", "CancelProg", "Health", "PStatus", "BloodPuff", "TextMsg", "HudText", "YouDied", "ScoreInfo"] {
        if let Some(s) = samples.get(key) {
            for d in s {
                println!("{:<12} {:?}  | ascii: {}", key, d, String::from_utf8_lossy(d).escape_debug());
            }
        }
    }
}
