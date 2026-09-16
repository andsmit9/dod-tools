//! Reconstruct a scoreboard from a single demo, from scratch.
//!
//! Exploratory counterpart to the batch tooling: prints one row per player
//! (kills, deaths, teamkills, suicides, objective points, flag captures) plus
//! the flag-capture events it found. Deliberately naive — it keys on client
//! slot rather than SteamID and takes a single team per player — so its team
//! labels are unreliable on demos with reconnects or a half-time side swap.
//! Use `reconcile_probe` for numbers you intend to trust.
//!
//!     cargo run --release -p analysis --example scoreboard_probe -- demo.dem

use dem::open_demo_from_bytes;
use dem::types::{EngineMessage, FrameData, MessageData, NetMessage};
use dod::{Team, UserMessage};
use std::collections::{BTreeMap, HashMap};

#[derive(Default, Clone)]
struct Slot {
    name: String,
    sid: String,
    team: Option<Team>,
    score: i32,
    kills_sv: i32,
    deaths_sv: i32,
    objscore: i32,
    kills: u32,
    deaths: u32,
    teamkills: u32,
    suicides: u32,
    caps: u32,
    cap_breaks: u32,
    weapons: BTreeMap<String, u32>,
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: scoreboard_probe <demo>");
    let bytes = std::fs::read(&path).expect("read demo");
    let demo = open_demo_from_bytes(&bytes).expect("parse demo");

    let mut slots: HashMap<u8, Slot> = HashMap::new();
    let mut flag_owner: HashMap<u8, Option<Team>> = HashMap::new();
    // Maps a flag's display name to its area index. Learned by observing a
    // SetObj arriving in the same frame as a CapMsg; see capwindow_probe.
    let flag_name_of_area: HashMap<u8, String> = HashMap::new();
    let mut in_progress: HashMap<u8, Team> = HashMap::new();
    let mut blocked_caps = 0usize;
    let mut bloodpuffs = 0usize;
    let mut corpse_anims: BTreeMap<u8, u32> = BTreeMap::new();
    let mut cap_events: Vec<(usize, String, Team, Vec<u8>, bool)> = Vec::new();
    let mut frame_no = 0usize;
    let mut peak: HashMap<u8, (i32, i32, i32)> = HashMap::new();
    let mut obj_gain: HashMap<u8, i32> = HashMap::new();
    let mut obj_last: HashMap<u8, i32> = HashMap::new();
    let mut score_msgs = 0usize;
    let mut obj_bumps: Vec<(usize, u8, i32)> = Vec::new();

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
                            let parts: Vec<&str> =
                                raw.trim_matches(|c| c == '\0' || c == '\\').split('\\').collect();
                            let f: HashMap<&str, &str> =
                                parts.as_chunks::<2>().0.iter().map(|&[k, v]| (k, v)).collect();
                            if f.is_empty() || f.get("*hltv") == Some(&"1") {
                                continue;
                            }
                            let s = slots.entry(ui.index).or_default();
                            if let Some(n) = f.get("name") {
                                s.name = n.to_string();
                            }
                            if let Some(sid) = f.get("*sid") {
                                s.sid = sid.to_string();
                            }
                            if let Some(t) = f.get("team") {
                                s.team = Team::try_from(*t).ok();
                            }
                        }
                    }
                    NetMessage::UserMessage(um) => {
                        let Ok(msg) = UserMessage::new(&um.name, &um.data) else {
                            continue;
                        };
                        match msg {
                            UserMessage::DeathMsg(d) => {
                                let vi = d.victim_client_index.wrapping_sub(1);
                                let v_team = slots.get(&vi).and_then(|s| s.team.clone());
                                let suicide = d.killer_client_index == 0
                                    || d.killer_client_index == d.victim_client_index;
                                if let Some(v) = slots.get_mut(&vi) {
                                    v.deaths += 1;
                                }
                                if suicide {
                                    if let Some(v) = slots.get_mut(&vi) {
                                        v.suicides += 1;
                                    }
                                    continue;
                                }
                                let ki = d.killer_client_index - 1;
                                let k_team = slots.get(&ki).and_then(|s| s.team.clone());
                                let tk = k_team.is_some() && k_team == v_team;
                                if let Some(k) = slots.get_mut(&ki) {
                                    if tk {
                                        k.teamkills += 1;
                                    } else {
                                        k.kills += 1;
                                    }
                                    *k.weapons.entry(format!("{:?}", d.weapon)).or_insert(0) += 1;
                                }
                            }
                            UserMessage::ScoreInfo(si) => {
                                let s = slots.entry(si.client_index - 1).or_default();
                                s.score = si.points as i32;
                                s.kills_sv = si.kills as i32;
                                s.deaths_sv = si.deaths as i32;
                                s.team = Some(si.team.clone());
                            }
                            UserMessage::ScoreInfoLong(si) => {
                                let s = slots.entry(si.client_index - 1).or_default();
                                s.score = si.score as i32;
                                s.kills_sv = si.frags as i32;
                                s.deaths_sv = si.deaths as i32;
                                s.team = Some(si.team.clone());
                            }
                            UserMessage::ScoreShort(ss) => {
                                score_msgs += 1;
                                let idx = ss.client_index - 1;
                                let s = slots.entry(idx).or_default();
                                s.score = ss.score as i32;
                                s.kills_sv = ss.kills as i32;
                                s.deaths_sv = ss.deaths as i32;
                                let p = peak.entry(idx).or_default();
                                p.0 = p.0.max(ss.score as i32);
                                p.1 = p.1.max(ss.kills as i32);
                                p.2 = p.2.max(ss.deaths as i32);
                            }
                            UserMessage::ObjScore(o) => {
                                let idx = o.client_index - 1;
                                let now = o.score as i32;
                                let prev = obj_last.insert(idx, now).unwrap_or(0);
                                // Counter can reset (round/half restart); count only rises.
                                let gained = if now > prev { now - prev } else { now.max(0) };
                                *obj_gain.entry(idx).or_insert(0) += gained;
                                if gained > 0 {
                                    obj_bumps.push((frame_no, idx, gained));
                                }
                                slots.entry(idx).or_default().objscore = now;
                            }
                            UserMessage::PTeam(pt) => {
                                slots.entry(pt.client_index - 1).or_default().team =
                                    Some(pt.team.clone());
                            }
                            UserMessage::CapMsg(c) => {
                                let idx = c.client_index - 1;
                                slots.entry(idx).or_default().caps += 1;
                                let same_event = matches!(
                                    cap_events.last(),
                                    Some((f, name, _, _, _)) if *f == frame_no && *name == c.point_name
                                );
                                if same_event {
                                    if let Some(last) = cap_events.last_mut() {
                                        last.3.push(idx);
                                    }
                                } else {
                                    // Was this flag owned by the other team right before?
                                    let area = flag_name_of_area
                                        .iter()
                                        .find(|(_, n)| n.eq_ignore_ascii_case(&c.point_name))
                                        .map(|(a, _)| *a);
                                    let prev = area.and_then(|a| flag_owner.get(&a).cloned()).flatten();
                                    let is_break =
                                        prev.is_some() && prev.as_ref() != Some(&c.team);
                                    if is_break
                                        && let Some(s) = slots.get_mut(&idx) {
                                            s.cap_breaks += 1;
                                        }
                                    cap_events.push((
                                        frame_no,
                                        c.point_name.clone(),
                                        c.team.clone(),
                                        vec![idx],
                                        is_break,
                                    ));
                                }
                            }
                            UserMessage::InitObj(io) => {
                                for o in io.objectives {
                                    flag_owner.insert(o.area_index, o.team);
                                }
                            }
                            UserMessage::SetObj(so) => {
                                flag_owner.insert(so.area_index, so.team);
                            }
                            UserMessage::StartProg(sp) => {
                                in_progress.insert(sp.area_index, sp.team);
                            }
                            UserMessage::CancelProg(cp) => {
                                if in_progress.remove(&cp.area_index).is_some() {
                                    blocked_caps += 1;
                                }
                            }
                            UserMessage::BloodPuff(_) => bloodpuffs += 1,
                            UserMessage::ClCorpse(c) => {
                                *corpse_anims.entry(c.animation_sequence).or_insert(0) += 1;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    let mut rows: Vec<(&u8, &Slot)> = slots.iter().filter(|(_, s)| !s.name.is_empty()).collect();
    rows.sort_by_key(|(_, s)| std::cmp::Reverse(s.kills_sv));

    println!("== {} ==", path);
    println!(
        "cap events: {} | player cap credits: {} | interrupted caps (team-level): {}",
        cap_events.len(),
        cap_events.iter().map(|e| e.3.len()).sum::<usize>(),
        blocked_caps
    );
    println!(
        "bloodpuffs: {} | corpse anim histogram: {:?}",
        bloodpuffs, corpse_anims
    );
    println!();
    println!("ScoreShort messages parsed: {}", score_msgs);
    println!(
        "{:<22} {:>8} {:>4} {:>4} {:>5} {:>3} {:>4} {:>4} {:>5} {:>4} {:>4} {:>4}",
        "Player", "Team", "K", "D", "K/D", "TK", "Suic", "Obj", "Cap", "Brk", "pkK", "pkD"
    );
    for (k, s) in &rows {
        let kd = if s.deaths > 0 {
            s.kills as f32 / s.deaths as f32
        } else {
            s.kills as f32
        };
        println!(
            "{:<22} {:>8} {:>4} {:>4} {:>5.2} {:>3} {:>4} {:>4} {:>5} {:>4} {:>4} {:>4}",
            s.name.chars().take(22).collect::<String>(),
            s.team.as_ref().map(|t| format!("{:?}", t)).unwrap_or_else(|| "-".into()),
            s.kills,
            s.deaths,
            kd,
            s.teamkills,
            s.suicides,
            obj_gain.get(k).copied().unwrap_or(0),
            s.caps,
            s.cap_breaks,
            peak.get(k).map(|p| p.1).unwrap_or(0),
            peak.get(k).map(|p| p.2).unwrap_or(0)
        );
    }
    // Does ObjScore credit every player who helped cap, even though CapMsg
    // names only one? Correlate obj-point bumps against each capture.
    let win = 300usize;
    let mut multi = 0usize;
    let mut total_credited = 0usize;
    for (f, _, _, _, _) in &cap_events {
        let near: Vec<u8> = obj_bumps
            .iter()
            .filter(|(bf, _, _)| bf.abs_diff(*f) <= win)
            .map(|(_, i, _)| *i)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        total_credited += near.len();
        if near.len() > 1 {
            multi += 1;
        }
    }
    println!(
        "\nObjScore bumps within +/-{} frames of a capture: {} credits over {} captures ({} captures credited >1 player)",
        win,
        total_credited,
        cap_events.len(),
        multi
    );
    println!("total ObjScore bumps in demo: {}", obj_bumps.len());

    println!("\ncap events (frame, flag, team, break?, cappers):");
    for (f, name, t, who, brk) in cap_events.iter().take(14) {
        let names: Vec<String> = who
            .iter()
            .map(|i| slots.get(i).map(|s| s.name.clone()).unwrap_or_default())
            .collect();
        println!(
            "  f{:<8} {:<16} {:<8} break={:<5} {:?}",
            f,
            name,
            format!("{:?}", t),
            brk,
            names
        );
    }
}
