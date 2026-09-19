//! Throwaway diagnostic: what was the recording player doing over time?
//! Prints one line per `interval` seconds from the Playback entry's DemoInfo:
//! view origin, view angles, health, spectator flag, speed. Use it to tell a
//! "camera is frozen" bug apart from "the player was standing still / dead".
//!
//!   cargo run --release --example dump_pov -- <demo.dem> [interval_s] [from_s] [to_s]
use std::env;

use dem::types::{Demo, FrameData, MessageDataParseMode};

fn main() {
    let args: Vec<String> = env::args().collect();
    let path = args.get(1).expect("usage: dump_pov <demo.dem> [interval] [from] [to]");
    let interval: f32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(30.0);
    let from: f32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let to: f32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(f32::MAX);

    let bytes = std::fs::read(path).unwrap();
    let demo = Demo::parse_from_bytes(&bytes, MessageDataParseMode::Raw).unwrap();

    for entry in &demo.directory.entries {
        if entry.type_ != 1 {
            continue;
        }
        let mut next = from;
        println!("{:>8} {:>26} {:>22} {:>4} {:>4} {:>7}", "t", "origin", "angles(p,y,r)", "hp", "spec", "speed");
        for f in &entry.frames {
            if f.time < from || f.time > to {
                continue;
            }
            let FrameData::NetworkMessage(boxed) = &f.frame_data else {
                continue;
            };
            if f.time < next {
                continue;
            }
            next = f.time + interval;
            let rp = &boxed.1.info.refparams;
            let o = &rp.view_origin;
            let a = &rp.view_angles;
            let v = &rp.sim_vel;
            let speed = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            println!(
                "{:8.1} {:8.0} {:8.0} {:8.0} {:7.1} {:7.1} {:6.1} {:>4} {:>4} {:7.1}",
                f.time, o[0], o[1], o[2], a[0], a[1], a[2], rp.health, rp.spectator, speed
            );
        }
    }
}
