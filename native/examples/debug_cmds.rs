use dem::open_demo_from_bytes;
use dem::types::FrameData;
use std::env;
use std::fs;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: debug_cmds <demo_path>");
        std::process::exit(1);
    }

    let path = &args[1];
    let bytes = fs::read(path).unwrap();
    let demo = open_demo_from_bytes(&bytes).unwrap();

    let mut results = Vec::new();
    let mut index = 0;

    for entry in demo.directory.entries.iter() {
        for frame in &entry.frames {
            if let FrameData::ConsoleCommand(cmd) = &frame.frame_data {
                let cmd_str = cmd
                    .command
                    .to_str()
                    .unwrap_or("")
                    .trim_end_matches('\x00')
                    .trim()
                    .to_string();

                let escaped = cmd_str.replace("\\", "\\\\").replace("\"", "\\\"");
                results.push(format!(r#"{{"i":{},"t":{},"cmd":"{}"}}"#, index, frame.frame, escaped));
            }
            index += 1;
        }
    }
    
    println!("[{}]", results.join(","));
}
