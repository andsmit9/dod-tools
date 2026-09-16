//! Command-line utility to dump all frames, engine messages, and user messages from a DoD demo.

use clap::Parser;
use dem::open_demo_from_bytes;
use dem::types::{FrameData, MessageData, NetMessage};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Dumps a detailed diagnostic summary of a Day of Defeat demo file."
)]
struct Args {
    /// Path to the .dem file
    demo_path: PathBuf,

    /// Optional path to write a detailed JSON summary
    #[arg(long)]
    json_out: Option<PathBuf>,
}

fn main() {
    let args = Args::parse();
    println!("Reading demo file: {}", args.demo_path.display());

    let bytes = match fs::read(&args.demo_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Error reading demo file: {}", e);
            std::process::exit(1);
        }
    };

    let demo = match open_demo_from_bytes(&bytes) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Error parsing demo structure: {:?}", e);
            std::process::exit(1);
        }
    };

    let map_name = demo
        .header
        .map_name
        .to_str()
        .map(|s| s.trim_end_matches('\x00'))
        .unwrap_or("unknown")
        .to_string();

    println!("--- Demo Header ---");
    println!("Demo Protocol:     {}", demo.header.demo_protocol);
    println!("Network Protocol:  {}", demo.header.network_protocol);
    println!("Map Name:          {}", map_name);
    println!(
        "Game Directory:    {}",
        demo.header
            .game_directory
            .to_str()
            .unwrap_or("unknown")
            .trim_end_matches('\x00')
    );

    let mut frame_counts = HashMap::new();
    let mut engine_message_counts = HashMap::new();
    let mut user_message_counts = HashMap::new();
    let mut console_commands = Vec::new();
    let mut sound_events = Vec::new();

    for (entry_idx, entry) in demo.directory.entries.iter().enumerate() {
        println!(
            "Analyzing directory entry {} (title: {:?}, frames: {})",
            entry_idx,
            entry
                .description
                .to_str()
                .unwrap_or("unknown")
                .trim_end_matches('\x00'),
            entry.frames.len()
        );

        for frame in &entry.frames {
            let debug_name = format!("{:?}", frame.frame_data);
            let frame_type = debug_name
                .split(['(', '{', ' '])
                .next()
                .unwrap_or("Unknown")
                .to_string();
            *frame_counts.entry(frame_type).or_insert(0) += 1;

            match &frame.frame_data {
                FrameData::ConsoleCommand(cmd) => {
                    let cmd_str = cmd
                        .command
                        .to_str()
                        .unwrap_or("")
                        .trim_end_matches('\x00')
                        .trim()
                        .to_string();
                    if !cmd_str.is_empty() && !console_commands.contains(&cmd_str) {
                        console_commands.push(cmd_str);
                    }
                }
                FrameData::Sound(sound) => {
                    let snd_str = String::from_utf8_lossy(&sound.sample)
                        .trim_end_matches('\x00')
                        .trim()
                        .to_string();
                    if !snd_str.is_empty() && !sound_events.contains(&snd_str) {
                        sound_events.push(snd_str);
                    }
                }
                FrameData::NetworkMessage(net_msg_box) => if let MessageData::Parsed(msgs) = &net_msg_box.1.messages {
                    for msg in msgs {
                        match msg {
                            NetMessage::EngineMessage(eng_msg) => {
                                let eng_debug = format!("{:?}", eng_msg);
                                let eng_type = eng_debug
                                    .split(['(', '{', ' '])
                                    .next()
                                    .unwrap_or("Unknown")
                                    .to_string();
                                *engine_message_counts.entry(eng_type).or_insert(0) += 1;
                            }
                            NetMessage::UserMessage(usr_msg) => {
                                let name = String::from_utf8_lossy(&usr_msg.name)
                                    .trim_end_matches('\x00')
                                    .to_string();
                                *user_message_counts.entry(name).or_insert(0) += 1;
                            }
                        }
                    }
                },
                _ => {}
            }
        }
    }

    println!("\n--- Packet/Frame Type Counts ---");
    let mut sorted_frames: Vec<_> = frame_counts.iter().collect();
    sorted_frames.sort_by(|a, b| b.1.cmp(a.1));
    for (name, count) in sorted_frames {
        println!("  {:.<25} {}", name, count);
    }

    println!("\n--- Engine Message Counts ---");
    let mut sorted_engine: Vec<_> = engine_message_counts.iter().collect();
    sorted_engine.sort_by(|a, b| b.1.cmp(a.1));
    for (name, count) in sorted_engine {
        println!("  {:.<25} {}", name, count);
    }

    println!("\n--- User Message Counts ---");
    let mut sorted_user: Vec<_> = user_message_counts.iter().collect();
    sorted_user.sort_by(|a, b| b.1.cmp(a.1));
    for (name, count) in sorted_user {
        println!("  {:.<25} {}", name, count);
    }

    println!("\n--- Unique Console Commands (First 15) ---");
    for cmd in console_commands.iter().take(15) {
        println!("  - {}", cmd);
    }

    println!("\n--- Unique Sound Samples (First 15) ---");
    for snd in sound_events.iter().take(15) {
        println!("  - {}", snd);
    }

    if let Some(json_path) = args.json_out {
        let json_data = serde_json::json!({
            "demo_protocol": demo.header.demo_protocol,
            "network_protocol": demo.header.network_protocol,
            "map_name": map_name,
            "frame_counts": frame_counts,
            "engine_message_counts": engine_message_counts,
            "user_message_counts": user_message_counts,
            "console_commands": console_commands,
            "sound_events": sound_events,
        });

        if let Ok(json_str) = serde_json::to_string_pretty(&json_data)
            && fs::write(&json_path, json_str).is_ok() {
                println!("\nDetailed summary written to: {}", json_path.display());
            }
    }
}
