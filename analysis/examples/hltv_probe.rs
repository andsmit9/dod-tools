//! Tell a true HLTV recording apart from a POV demo carrying director frames.
//!
//! `SvcDirector` is not a reliable HLTV marker: it appears in ordinary
//! player-recorded demos whenever an HLTV caster is spectating the live match,
//! and demo patchers inject it too. `SvcHltv` is the reliable signal. This
//! prints both, alongside Health and CurWeapon counts, which separate the two
//! kinds of file unambiguously (true HLTV demos carry exactly 1 and 2).
//!
//!     cargo run --release -p analysis --example hltv_probe -- demo.dem

use dem::open_demo_from_bytes;
use dem::types::{EngineMessage, FrameData, MessageData, NetMessage};
use std::collections::HashMap;

fn main() {
    let path = std::env::args().nth(1).expect("usage: hltv_probe <demo>");
    let Ok(bytes) = std::fs::read(&path) else {
        println!("READFAIL\t{}", path);
        return;
    };
    let demo = match open_demo_from_bytes(&bytes) {
        Ok(d) => d,
        Err(_) => {
            println!("PARSEFAIL\t{}", path);
            return;
        }
    };
    drop(bytes);

    let mut svc_hltv = 0u64;
    let mut svc_director = 0u64;
    let mut health = 0u64;
    let mut cur_weapon = 0u64;
    let mut hltv_slot_in_userinfo = false;
    let mut pov_index: i64 = -1;

    for entry in &demo.directory.entries {
        for frame in &entry.frames {
            let FrameData::NetworkMessage(bt) = &frame.frame_data else {
                continue;
            };
            let MessageData::Parsed(msgs) = &bt.1.messages else {
                continue;
            };
            for m in msgs {
                match m {
                    NetMessage::EngineMessage(em) => match &**em {
                        EngineMessage::SvcHltv(_) => svc_hltv += 1,
                        EngineMessage::SvcDirector(_) => svc_director += 1,
                        EngineMessage::SvcServerInfo(si) => pov_index = si.player_index as i64,
                        EngineMessage::SvcUpdateUserInfo(ui) => {
                            let raw = String::from_utf8_lossy(ui.user_info.as_slice()).to_string();
                            let parts: Vec<&str> = raw
                                .trim_matches(|c| c == '\0' || c == '\\')
                                .split('\\')
                                .collect();
                            let f: HashMap<&str, &str> =
                                parts.as_chunks::<2>().0.iter().map(|&[k, v]| (k, v)).collect();
                            if f.get("*hltv") == Some(&"1") {
                                hltv_slot_in_userinfo = true;
                            }
                        }
                        _ => {}
                    },
                    NetMessage::UserMessage(um) => {
                        let mut n: Vec<u8> = um.name.clone();
                        while n.last() == Some(&0) {
                            n.pop();
                        }
                        match n.as_slice() {
                            b"Health" => health += 1,
                            b"CurWeapon" => cur_weapon += 1,
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    let file = path.rsplit(['/', '\\']).next().unwrap_or(&path);
    println!(
        "{file}\tsvcHltv={svc_hltv}\tsvcDirector={svc_director}\thltvSlot={hltv_slot_in_userinfo}\tpovIndex={pov_index}\tHealth={health}\tCurWeapon={cur_weapon}"
    );
}
