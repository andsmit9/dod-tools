//! Measure what a reconnect does to the server's score counters.
//!
//! When a player rejoins, the server may restart their ScoreShort/ScoreInfo
//! values at zero. This distinguishes that from the match-start scoreboard wipe
//! (where every player drops at once) and quantifies what each naive approach
//! costs: reading the last value undercounts, reading the highest value
//! overcounts by sweeping in warm-up.
//!
//!     cargo run --release -p analysis --example reconnect_probe -- demo.dem

use dem::open_demo_from_bytes;
use dem::types::{EngineMessage, FrameData, MessageData, NetMessage};
use dod::UserMessage;
use std::collections::HashMap;

#[derive(Default, Clone)]
struct P {
    name: String,
    series: Vec<(usize, i32, i32)>, // frame, kills, deaths
    derived_kills: i32,
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: reconnect_probe <demo>");
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };
    let Ok(demo) = open_demo_from_bytes(&bytes) else {
        println!("PARSEFAIL\t{path}");
        return;
    };
    drop(bytes);

    let mut players: HashMap<String, P> = HashMap::new();
    let mut slot_sid: HashMap<u8, String> = HashMap::new();
    let mut frame_no = 0usize;

    for entry in &demo.directory.entries {
        for frame in &entry.frames {
            frame_no += 1;
            let FrameData::NetworkMessage(bt) = &frame.frame_data else {
                continue;
            };
            let MessageData::Parsed(msgs) = &bt.1.messages else {
                continue;
            };
            for m in msgs {
                match m {
                    NetMessage::EngineMessage(em) => {
                        if let EngineMessage::SvcUpdateUserInfo(ui) = &**em {
                            let raw = String::from_utf8_lossy(ui.user_info.as_slice()).to_string();
                            let parts: Vec<&str> = raw
                                .trim_matches(|c| c == '\0' || c == '\\')
                                .split('\\')
                                .collect();
                            let f: HashMap<&str, &str> =
                                parts.as_chunks::<2>().0.iter().map(|&[k, v]| (k, v)).collect();
                            if f.is_empty() {
                                slot_sid.remove(&ui.index);
                                continue;
                            }
                            if f.get("*hltv") == Some(&"1") {
                                continue;
                            }
                            let Some(sid) = f.get("*sid").map(|s| s.to_string()) else {
                                continue;
                            };
                            slot_sid.insert(ui.index, sid.clone());
                            let p = players.entry(sid).or_default();
                            if let Some(n) = f.get("name") {
                                p.name = n.to_string();
                            }
                        }
                    }
                    NetMessage::UserMessage(um) => {
                        let Ok(msg) = UserMessage::new(&um.name, &um.data) else {
                            continue;
                        };
                        match msg {
                            UserMessage::ScoreShort(s) => {
                                if let Some(sid) = slot_sid.get(&(s.client_index - 1)) {
                                    players.entry(sid.clone()).or_default().series.push((
                                        frame_no,
                                        s.kills as i32,
                                        s.deaths as i32,
                                    ));
                                }
                            }
                            UserMessage::ScoreInfo(s) => {
                                if let Some(sid) = slot_sid.get(&(s.client_index - 1)) {
                                    players.entry(sid.clone()).or_default().series.push((
                                        frame_no,
                                        s.kills as i32,
                                        s.deaths as i32,
                                    ));
                                }
                            }
                            UserMessage::DeathMsg(d) => {
                                if d.killer_client_index > 0
                                    && d.killer_client_index != d.victim_client_index
                                    && let Some(sid) = slot_sid.get(&(d.killer_client_index - 1)) {
                                        players.entry(sid.clone()).or_default().derived_kills += 1;
                                    }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    // Every point where a player's server kill counter went DOWN.
    let mut drops: Vec<(usize, String, i32)> = Vec::new(); // frame, sid, value lost
    for (sid, p) in &players {
        for w in p.series.windows(2) {
            if w[1].1 < w[0].1 {
                drops.push((w[1].0, sid.clone(), w[0].1));
            }
        }
    }
    drops.sort_by_key(|d| d.0);

    // A drop is a MATCH RESET if several players drop together; otherwise it is
    // a lone player reconnecting.
    let mut mass = 0u32;
    let mut solo = 0u32;
    let mut solo_lost = 0i32;
    for (f, _sid, lost) in &drops {
        let together = drops
            .iter()
            .filter(|(of, _, _)| of.abs_diff(*f) <= 300)
            .count();
        if together >= 4 {
            mass += 1;
        } else {
            solo += 1;
            solo_lost += lost;
        }
    }

    // Three ways to total a player's kills from the server counter.
    let (mut naive_last, mut naive_peak, mut banked, mut derived) = (0i32, 0i32, 0i32, 0i32);
    let mut affected = 0u32;
    for (sid, p) in &players {
        if p.series.is_empty() {
            continue;
        }
        let last = p.series.last().unwrap().1;
        let peak = p.series.iter().map(|s| s.1).max().unwrap_or(0);
        // Bank a session only on a SOLO drop; a mass drop is the match starting,
        // where the pre-reset numbers should be discarded, not carried.
        let mut acc = 0i32;
        let mut prev = 0i32;
        let mut had_solo = false;
        for (f, k, _) in &p.series {
            if *k < prev {
                let together = drops
                    .iter()
                    .filter(|(of, _, _)| of.abs_diff(*f) <= 300)
                    .count();
                if together >= 4 {
                    acc = 0; // match start: throw away the warm-up
                } else {
                    acc += prev; // reconnect: keep what they had
                    had_solo = true;
                }
            }
            prev = *k;
        }
        if had_solo {
            affected += 1;
        }
        naive_last += last;
        naive_peak += peak;
        banked += acc + prev;
        derived += p.derived_kills;
        let _ = sid;
    }

    let file = path.rsplit(['/', '\\']).next().unwrap_or(&path);
    println!(
        "OK\t{file}\tplayers={}\tdropsMass={mass}\tdropsSolo={solo}\tsoloLostKills={solo_lost}\tplayersAffected={affected}\tnaiveLast={naive_last}\tnaivePeak={naive_peak}\tbanked={banked}\tderived={derived}",
        players.len()
    );
}
