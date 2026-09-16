use clap::Parser;
use dem::open_demo_from_bytes;
use dem::types::FrameData;
use std::fs;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(author, version, about = "Diagnostic frame inspector for DoD demos.")]
struct Args {
    /// Path to the .dem file
    file_path: PathBuf,
}

fn main() {
    let args = Args::parse();

    let bytes = match fs::read(&args.file_path) {
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

    println!("Scanning demo for anomalies...");

    let mut frame_index = 0;
    for entry in &demo.directory.entries {
        for frame in &entry.frames {
            frame_index += 1;
            
            match &frame.frame_data {
                FrameData::NetworkMessage(net_msg_box) => {
                    let len = net_msg_box.1.message_length;
                    if len > 5000 {
                        println!(
                            "Index: {:<6} | Tick: {:<7} | Time: {:.6} | Type: {:<15}  [Payload Length: {} bytes]",
                            frame_index, frame.frame, frame.time, "NetworkMessage", len
                        );
                    }
                }
                FrameData::ConsoleCommand(cmd) => {
                    let cmd_str = cmd.command.to_str().unwrap_or("").trim_end_matches('\0');
                    if cmd_str.contains("host_framerate") {
                        println!(
                            "Index: {:<6} | Tick: {:<7} | Time: {:.6} | Type: {:<15}  [Payload: '{}']",
                            frame_index, frame.frame, frame.time, "ConsoleCommand", cmd_str
                        );
                    }
                }
                _ => {}
            }
        }
    }
}
