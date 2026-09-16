//! Emit one TSV row of stat-coverage metrics for a single demo.
//!
//! Designed to be driven over a whole demo library by a shell loop, one process
//! per file, so a demo that panics the parser only kills its own process. This
//! is what produced the corpus-wide figures: DeathMsg widths, Health counts,
//! flag-capture and objective-score correlation, and per-player kill
//! reconciliation against the server's own frag counter.
//!
//!     cargo run --release -p analysis --example batch_probe -- demo.dem
//!     for f in local/demos/*.dem; do batch_probe "$f" >> out.tsv; done

use dem::open_demo_from_bytes;
use dem::types::{EngineMessage, FrameData, MessageData, NetMessage};
use dod::{Team, UserMessage};
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Default, Clone)]
struct Agg {
    name: String,
    kills: i32,
    deaths: i32,
    teamkills: i32,
    suicides: i32,
    caps: i32,
    peak_frags: i32,
    peak_deaths: i32,
    saw_score: bool,
    reconnects: u32,
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: batch_probe <demo>");
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            println!("{}\tREADFAIL\t{}", path, e);
            return;
        }
    };
    let demo = match open_demo_from_bytes(&bytes) {
        Ok(d) => d,
        Err(e) => {
            println!("{}\tPARSEFAIL\t{}", path, e);
            return;
        }
    };
    drop(bytes);

    let mut players: HashMap<String, Agg> = HashMap::new();
    let mut slot_sid: HashMap<u8, String> = HashMap::new();
    let mut slot_team: HashMap<u8, Team> = HashMap::new();

    let mut msg_counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut deathmsg_sizes: BTreeMap<usize, u64> = BTreeMap::new();
    let mut parse_errors: BTreeMap<String, u64> = BTreeMap::new();

    let mut is_hltv = false;
    let mut health_msgs = 0u64;
    let mut obj_bumps: Vec<(usize, String)> = Vec::new();
    // (frame, flag, team, cappers-from-CapMsg)
    let mut cap_events: Vec<(usize, String, Team, Vec<String>)> = Vec::new();
    let mut start_prog = 0u64;
    let mut cancel_prog = 0u64;
    let mut obj_last: HashMap<String, i32> = HashMap::new();
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
                        if matches!(**em, EngineMessage::SvcHltv(_) | EngineMessage::SvcDirector(_))
                        {
                            is_hltv = true;
                        }
                        if let EngineMessage::SvcUpdateUserInfo(ui) = &**em {
                            let raw = String::from_utf8_lossy(ui.user_info.as_slice()).to_string();
                            let parts: Vec<&str> = raw
                                .trim_matches(|c| c == '\0' || c == '\\')
                                .split('\\')
                                .collect();
                            let f: HashMap<&str, &str> =
                                parts.as_chunks::<2>().0.iter().map(|&[k, v]| (k, v)).collect();
                            if f.is_empty() {
                                // Empty userinfo = the slot emptied out.
                                if let Some(sid) = slot_sid.remove(&ui.index)
                                    && let Some(p) = players.get_mut(&sid) {
                                        p.reconnects += 1;
                                    }
                                continue;
                            }
                            if f.get("*hltv") == Some(&"1") {
                                continue;
                            }
                            let sid = f
                                .get("*sid")
                                .map(|s| s.to_string())
                                .or_else(|| f.get("*fid").map(|x| format!("FID_{x}")))
                                .unwrap_or_else(|| format!("SLOT_{}", ui.index));
                            slot_sid.insert(ui.index, sid.clone());
                            let p = players.entry(sid).or_default();
                            if let Some(n) = f.get("name") {
                                p.name = n.to_string();
                            }
                        }
                    }
                    NetMessage::UserMessage(um) => {
                        let mut n: Vec<u8> = um.name.clone();
                        while n.last() == Some(&0) {
                            n.pop();
                        }
                        let name = String::from_utf8_lossy(&n).to_string();
                        *msg_counts.entry(name.clone()).or_insert(0) += 1;
                        if name == "DeathMsg" {
                            *deathmsg_sizes.entry(um.data.len()).or_insert(0) += 1;
                        }
                        if name == "Health" {
                            health_msgs += 1;
                        }
                        if name == "StartProg" {
                            start_prog += 1;
                        }
                        if name == "CancelProg" {
                            cancel_prog += 1;
                        }

                        let msg = match UserMessage::new(&um.name, &um.data) {
                            Ok(m) => m,
                            Err(_) => {
                                *parse_errors.entry(name).or_insert(0) += 1;
                                continue;
                            }
                        };
                        match msg {
                            UserMessage::PTeam(pt) => {
                                slot_team.insert(pt.client_index - 1, pt.team.clone());
                            }
                            UserMessage::ScoreInfo(si) => {
                                slot_team.insert(si.client_index - 1, si.team.clone());
                                if let Some(sid) = slot_sid.get(&(si.client_index - 1)) {
                                    let p = players.entry(sid.clone()).or_default();
                                    p.saw_score = true;
                                    p.peak_frags = p.peak_frags.max(si.kills as i32);
                                    p.peak_deaths = p.peak_deaths.max(si.deaths as i32);
                                }
                            }
                            UserMessage::ScoreInfoLong(si) => {
                                slot_team.insert(si.client_index - 1, si.team.clone());
                                if let Some(sid) = slot_sid.get(&(si.client_index - 1)) {
                                    let p = players.entry(sid.clone()).or_default();
                                    p.saw_score = true;
                                    p.peak_frags = p.peak_frags.max(si.frags as i32);
                                    p.peak_deaths = p.peak_deaths.max(si.deaths as i32);
                                }
                            }
                            UserMessage::ScoreShort(ss) => {
                                if let Some(sid) = slot_sid.get(&(ss.client_index - 1)) {
                                    let p = players.entry(sid.clone()).or_default();
                                    p.saw_score = true;
                                    p.peak_frags = p.peak_frags.max(ss.kills as i32);
                                    p.peak_deaths = p.peak_deaths.max(ss.deaths as i32);
                                }
                            }
                            UserMessage::DeathMsg(d) => {
                                let vslot = d.victim_client_index.wrapping_sub(1);
                                let vsid = slot_sid.get(&vslot).cloned();
                                if let Some(s) = &vsid {
                                    players.entry(s.clone()).or_default().deaths += 1;
                                }
                                if d.killer_client_index == 0
                                    || d.killer_client_index == d.victim_client_index
                                {
                                    if let Some(s) = &vsid {
                                        players.entry(s.clone()).or_default().suicides += 1;
                                    }
                                    continue;
                                }
                                let kslot = d.killer_client_index - 1;
                                let tk = match (slot_team.get(&kslot), slot_team.get(&vslot)) {
                                    (Some(a), Some(b)) => a == b,
                                    _ => false,
                                };
                                if let Some(s) = slot_sid.get(&kslot).cloned() {
                                    let p = players.entry(s).or_default();
                                    if tk {
                                        p.teamkills += 1;
                                    } else {
                                        p.kills += 1;
                                    }
                                }
                            }
                            UserMessage::ObjScore(o) => {
                                let Some(sid) = slot_sid.get(&(o.client_index - 1)).cloned() else {
                                    continue;
                                };
                                let now = o.score as i32;
                                let prev = obj_last.insert(sid.clone(), now).unwrap_or(0);
                                if now > prev {
                                    obj_bumps.push((frame_no, sid));
                                }
                            }
                            UserMessage::CapMsg(c) => {
                                let Some(sid) = slot_sid.get(&(c.client_index - 1)).cloned() else {
                                    continue;
                                };
                                players.entry(sid.clone()).or_default().caps += 1;
                                let same = matches!(
                                    cap_events.last(),
                                    Some((f, flag, _, _))
                                        if *f == frame_no && *flag == c.point_name
                                );
                                if same {
                                    if let Some(last) = cap_events.last_mut() {
                                        last.3.push(sid);
                                    }
                                } else {
                                    cap_events.push((
                                        frame_no,
                                        c.point_name.clone(),
                                        c.team.clone(),
                                        vec![sid],
                                    ));
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    // H3: how many captures had more than one CapMsg?
    let multi_capmsg = cap_events.iter().filter(|e| e.3.len() > 1).count();

    // H4: distinct players whose objective score rose near each capture.
    // 60 frames is roughly a second at typical DoD demo rates.
    let win = 60usize;
    let mut multi_obj = 0usize;
    let mut obj_credits = 0usize;
    for (f, _, _, _) in &cap_events {
        let near: BTreeSet<&String> = obj_bumps
            .iter()
            .filter(|(bf, _)| bf.abs_diff(*f) <= win)
            .map(|(_, s)| s)
            .collect();
        obj_credits += near.len();
        if near.len() > 1 {
            multi_obj += 1;
        }
    }

    // H5: does frags == kills - teamkills - suicides hold per player?
    let mut h5_checked = 0u32;
    let mut h5_exact = 0u32;
    let mut h5_within1 = 0u32;
    let mut h5_resid: Vec<i32> = Vec::new();
    let mut roster = 0u32;
    let mut any_reconnect = 0u32;
    for p in players.values() {
        if p.name.is_empty() {
            continue;
        }
        if p.kills + p.deaths + p.peak_frags == 0 {
            continue;
        }
        roster += 1;
        if p.reconnects > 0 {
            any_reconnect += 1;
        }
        if !p.saw_score || p.reconnects > 0 {
            continue;
        }
        let predicted = p.kills - p.teamkills - p.suicides;
        let resid = p.peak_frags - predicted;
        h5_checked += 1;
        if resid == 0 {
            h5_exact += 1;
        }
        if resid.abs() <= 1 {
            h5_within1 += 1;
        }
        h5_resid.push(resid);
    }
    let resid_sum: i32 = h5_resid.iter().sum();

    // H1: any DeathMsg that is not exactly 3 bytes?
    let dm_total: u64 = deathmsg_sizes.values().sum();
    let dm_3b = deathmsg_sizes.get(&3).copied().unwrap_or(0);
    let dm_sizes: Vec<String> = deathmsg_sizes
        .iter()
        .map(|(k, v)| format!("{}:{}", k, v))
        .collect();

    let map = demo
        .header
        .map_name
        .to_str()
        .map(|s| s.trim_end_matches('\0'))
        .unwrap_or("?")
        .to_string();
    let frames: usize = demo.directory.entries.iter().map(|e| e.frames.len()).sum();
    let errs: Vec<String> = parse_errors
        .iter()
        .map(|(k, v)| format!("{}:{}", k, v))
        .collect();

    let file = path.rsplit(['/', '\\']).next().unwrap_or(&path);
    // TSV: one row per demo
    println!(
        "OK\t{file}\t{ty}\t{map}\t{frames}\t{roster}\t{recon}\t\
         {dm_total}\t{dm_3b}\t{dm_sizes}\t{health}\t\
         {caps}\t{multi_capmsg}\t{multi_obj}\t{obj_credits}\t{obj_bumps}\t\
         {sp}\t{cp}\t{h5c}\t{h5e}\t{h5w}\t{resid_sum}\t{errs}",
        ty = if is_hltv { "HLTV" } else { "POV" },
        recon = any_reconnect,
        dm_sizes = dm_sizes.join(","),
        health = health_msgs,
        caps = cap_events.len(),
        obj_bumps = obj_bumps.len(),
        sp = start_prog,
        cp = cancel_prog,
        h5c = h5_checked,
        h5e = h5_exact,
        h5w = h5_within1,
        errs = if errs.is_empty() {
            "-".to_string()
        } else {
            errs.join(",")
        },
    );
}
