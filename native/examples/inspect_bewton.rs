use analysis::Analysis;
use dem::open_demo_from_bytes;
use dem::types::{FrameData, MessageData};
use std::fs;

fn inspect_demo(path: &str) {
    println!("\n=== INSPECTING DEMO: {} ===", path);
    if !std::path::Path::new(path).exists() {
        println!("Demo file not found!");
        return;
    }
    let file_bytes = fs::read(path).unwrap();
    let demo = open_demo_from_bytes(&file_bytes).unwrap();

    let mut events = vec![];
    for entry in &demo.directory.entries {
        for frame in &entry.frames {
            if let FrameData::NetworkMessage(net_msg_box) = &frame.frame_data
                && let MessageData::Parsed(msgs) = &net_msg_box.1.messages {
                    for msg in msgs {
                        if let dem::types::NetMessage::UserMessage(user_msg) = msg {
                            let name = String::from_utf8_lossy(&user_msg.name)
                                .trim_end_matches('\0')
                                .to_string();
                            if name == "RoundState" || name == "ClanTimer" || name == "TeamScore" || name == "ScoreShort" {
                                events.push((frame.time, name, format!("{:?}", user_msg.data)));
                            }
                        }
                    }
                }
        }
    }
    println!("Early allied demo events (Time <= 45.0):");
    for (time, name, data) in &events {
        if *time <= 45.0 {
            println!("  Time {:.3}: {} -> {}", time, name, data);
        }
    }

    let analysis = Analysis::try_from_bytes(&file_bytes).unwrap();
    println!("CLAN MATCH: map_name: {}", analysis.demo_info.map_name);
    println!("CLAN MATCH: clan_match_detected: {}", analysis.state.clan_match_detected);
    println!("CLAN MATCH: match_start_witnessed: {}", analysis.state.match_start_witnessed);
    println!("CLAN MATCH: started_late: {}", analysis.state.started_late);
    println!("CLAN MATCH: ended_early: {}", analysis.state.ended_early);
    println!("CLAN MATCH: first_time_left: {:?}", analysis.state.first_time_left);
    println!("CLAN MATCH: last_time_left: {:?}", analysis.state.last_time_left);
    println!("CLAN MATCH: map_changed: {}", analysis.state.map_changed);
    
    println!("Flagged players (pre-demo or reconnected):");
    let mut flagged_count = 0;
    for p in &analysis.state.players {
        if p.has_pre_demo_activity || p.has_reconnected {
            println!(
                "  Player: {} (ID: {}), pre-demo: {}, reconnected: {}, stats: {:?}",
                p.name, p.id, p.has_pre_demo_activity, p.has_reconnected, p.stats
            );
            flagged_count += 1;
        }
    }
    println!("Total flagged players: {}", flagged_count);

    println!("All players and their score/kills/deaths:");
    for p in &analysis.state.players {
        println!("  Player: {} (ID: {}): stats: {:?}", p.name, p.id, p.stats);
    }
}

fn main() {
    inspect_demo("local/demos/bewton-playoffs-round1-armory-axis.dem");
    inspect_demo("local/demos/bewton-playoffs-round1-armory-allied.dem");
}
