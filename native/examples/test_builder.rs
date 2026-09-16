use native::patch::scanner::scan_demo_for_highlights;
use native::patch::types::{HighlightRules, PatcherConfig};
use native::patch::{build_batch_queue, StreamPatcher};

fn main() {
    let path = std::path::Path::new("local/demos/wsod25-grp_r1-dyelife_gskill_armory_h1.dem");
    let _rules = HighlightRules {
        max_time_gap: None,
    };
    match scan_demo_for_highlights(path) {
        Ok((_tickrate, mut streaks, _is_pov, _pov_idx, _frames, _match_start_tick, _frame_times)) => {
            streaks.retain(|s| s.target_player.as_deref().unwrap_or("") == "dicE[: :]DyeL!fe[dd]");
            for (i, streak) in streaks.iter().enumerate() {
                println!("Streak {}: start_tick = {}, end_tick = {}, kills = {}", i, streak.start_tick, streak.end_tick, streak.kill_count);
            }

            let mut config = PatcherConfig::default();
            config.capture_directories = vec![std::path::PathBuf::from("local/demos")];
            config.fast_forward_speed = 0.05;

            let global_arrays: std::collections::HashMap<std::path::PathBuf, std::sync::Arc<Vec<f32>>> = std::collections::HashMap::new();
            let (jobs, _) = build_batch_queue(streaks, &config, &global_arrays).unwrap();
            println!("Generated {} jobs", jobs.len());
            for job in jobs {
                println!("Running job for output: {}", job.output_demo.display());
                let patcher = StreamPatcher::new(&job.source_demo, &job.output_demo);
                let cancel_token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                patcher.patch(&job, &config, &cancel_token).unwrap();
            }
            println!("Patching completed successfully!");
        },
        Err(e) => println!("Error: {}", e),
    }
}
