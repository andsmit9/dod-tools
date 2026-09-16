use native::patch::scanner::scan_demo_for_highlights;
use native::patch::types::HighlightRules;
use std::env;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: debug_scanner <demo_path>");
        std::process::exit(1);
    }
    
    let path = std::path::Path::new(&args[1]);
    let _rules = HighlightRules {
        max_time_gap: None,
    };
    
    match scan_demo_for_highlights(path) {
        Ok((tickrate, streaks, _is_pov, _pov_idx, _frames, _match_start_tick, _frame_times)) => {
            println!("Tickrate: {}", tickrate);
            for s in streaks {
                println!("Streak: player='{}' kills={}, start_tick={}, end_tick={}, duration={}", 
                         s.target_player.as_deref().unwrap_or(""),
                         s.kill_count, 
                         s.start_tick, 
                         s.end_tick, 
                         s.duration_string);
            }
        },
        Err(e) => println!("Error: {}", e),
    }
}
