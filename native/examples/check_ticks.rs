// check_ticks.rs — Diagnostic binary for verifying injected command tick positions.
#![allow(unused_variables)]
//
// Usage:
//   cargo run --bin check_ticks -- local/demos/wsod25-grp_r1-dyelife_gskill_armory_h1.dem
//   cargo run --bin check_ticks -- local/demos/ktps8w9-gorilla_gskill_rr2_h1.dem "PlayerName"
//
// Runs build_batch_queue under two conditions and prints where every command lands:
//   SCENARIO A = fresh scan   (global_arrays has real frame_times)
//   SCENARIO B = project load (global_arrays key exists but Arc is EMPTY — the real bug case)
//
// Commands at tick 0 that should not be there are flagged with [BUG].

use native::patch::scanner::scan_demo_for_highlights;
use native::patch::types::PatcherConfig;
use native::patch::build_batch_queue;
use std::sync::Arc;

fn run_scenario(
    label: &str,
    streaks: Vec<native::patch::CaptureStreak>,
    global_arrays: std::collections::HashMap<std::path::PathBuf, Arc<Vec<f32>>>,
    config: &PatcherConfig,
) {
    println!("\n=== {} ===", label);

    for (i, s) in streaks.iter().enumerate() {
        println!(
            "  Input streak {}: start_tick={}, end_tick={}, kills={}, total_demo_frames={}, frame_times_len={}",
            i + 1, s.start_tick, s.end_tick, s.kill_count,
            s.total_demo_frames, s.frame_times.len()
        );
    }
    for (key, arr) in &global_arrays {
        println!("  global_arrays[\"{}\"] = {} entries", key.display(), arr.len());
    }

    match build_batch_queue(streaks, config, &global_arrays) {
        Ok((jobs, _)) => {
            let mut any_bug = false;
            for job in &jobs {
                if job.output_demo.to_string_lossy().contains("primer") { continue; }
                println!("  Job -> {}", job.output_demo.display());
                let mut cmds = job.scheduled_commands.clone();
                cmds.sort_by_key(|c| c.0);
                for (tick, cmd) in &cmds {
                    // tick 0 is expected only for BREADCRUMB; everything else at 0 is a bug
                    let is_bug = *tick == 0 && !cmd.contains("BREADCRUMB");
                    if is_bug { any_bug = true; }
                    println!("    tick {:>6}  {}{}",  tick, cmd, if is_bug { "  <<< BUG" } else { "" });
                }
                println!("    --- Director Events ---");
                let mut dir_events = job.director_events.clone();
                dir_events.sort_by_key(|e| e.0);
                for (tick, label) in &dir_events {
                    println!("    tick {:>6}  [DIRECTOR] {}", tick, label);
                }
            }
            if any_bug {
                println!("  *** BUG CONFIRMED: tick-0 collisions present ***");
            } else {
                println!("  OK: no unexpected tick-0 collisions");
            }
        }
        Err(e) => println!("  ERROR: {}", e),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let demo_path = args.get(1).map(String::as_str)
        .unwrap_or("local/demos/wsod25-grp_r1-dyelife_gskill_armory_h1.dem");
    let player_filter = args.get(2).map(String::as_str);

    println!("Demo  : {}", demo_path);
    println!("Player: {}", player_filter.unwrap_or("(all)"));

    let path = std::path::Path::new(demo_path);
    let filename = path.file_name().unwrap_or_default().to_string_lossy().into_owned();

    let (tickrate, mut streaks, _pov, _pov_idx, _frames, _match_start, frame_times_arc) =
        match scan_demo_for_highlights(path) {
            Ok(r) => r,
            Err(e) => { eprintln!("Scan error: {}", e); return; }
        };

    println!("Scanned: tickrate={:.0}, {} streak(s), {} raw frame_times",
        tickrate, streaks.len(), frame_times_arc.len());

    if let Some(p) = player_filter {
        streaks.retain(|s| s.target_player.as_deref().unwrap_or("") == p);
        println!("After player filter: {} streak(s)", streaks.len());
    }

    streaks.truncate(3); // cap to keep output readable

    if streaks.is_empty() {
        println!("No streaks to test.");
        return;
    }

    let mut config = PatcherConfig::default();
    config.capture_directories = vec![std::path::PathBuf::from("local/demos")];
    config.record_start_lead = 3.0;
    config.record_stop_trail = 1.0;
    config.post_roll_seconds = 3.0;

    // SCENARIO A: global_arrays has the real frame_times (fresh scan path)
    {
        let mut ga = std::collections::HashMap::new();
        ga.insert(path.to_path_buf(), frame_times_arc.clone());
        run_scenario("SCENARIO A — fresh scan (populated frame_times)", streaks.clone(), ga, &config);
    }

    // SCENARIO B: global_arrays key exists but value is empty Arc
    // This is what happens when a project is loaded from disk:
    //   DemoData.frame_times  has #[serde(skip)] → empty Arc after deserialize
    //   global_arrays is built from those empty Arcs before build_batch_queue is called
    //   The lookup finds the key so the unwrap_or_else fallback is never reached
    {
        let mut ga = std::collections::HashMap::new();
        ga.insert(path.to_path_buf(), Arc::new(Vec::<f32>::new()));

        let mut proj_streaks = streaks.clone();
        for s in &mut proj_streaks {
            s.frame_times = Arc::new(Vec::new()); // simulate #[serde(skip, default)]
        }
        run_scenario("SCENARIO B — project load (frame_times empty, simulates serde skip)", proj_streaks, ga, &config);
    }
}
