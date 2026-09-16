use dem::open_demo_from_bytes;
use dem::types::{FrameData, MessageData};
use std::fs;

fn main() {
    let path = "local/demos/ktps8w1-m00cat_soul_lenn_h2.dem";
    if !std::path::Path::new(path).exists() {
        println!("Demo file not found at {}!", path);
        return;
    }
    let file_bytes = fs::read(path).unwrap();
    let demo = open_demo_from_bytes(&file_bytes).unwrap();
    
    let mut all_events = vec![];
    for (entry_idx, entry) in demo.directory.entries.iter().enumerate() {
        for (frame_idx, frame) in entry.frames.iter().enumerate() {
            if let FrameData::NetworkMessage(net_msg_box) = &frame.frame_data
                && let MessageData::Parsed(msgs) = &net_msg_box.1.messages {
                    for msg in msgs {
                        all_events.push(format!("Entry {} Frame {} msg: {:?}", entry_idx, frame_idx, msg));
                    }
                }
        }
    }
    
    println!("Total events: {}", all_events.len());
    let print_count = 100;
    let start_idx = all_events.len().saturating_sub(print_count);
    println!("Last {} events:", print_count);
    for idx in start_idx..all_events.len() {
        println!("{}", all_events[idx]);
    }
}
