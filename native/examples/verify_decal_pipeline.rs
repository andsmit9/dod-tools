//! End-to-end check of the decal flush through the real capture path.
//!
//! `survey_decal_flush` measures the clean in isolation; this drives the whole
//! thing — `scan_demo_for_highlights` → `build_batch_queue` →
//! `StreamPatcher::patch` — and checks the two properties that no unit test
//! can, because both are about the pipeline agreeing with itself:
//!
//!   * The cleaned demo the patcher streams from has the SAME frame count as
//!     the source. Every scheduled capture command is matched against a frame
//!     ordinal, so a demo one frame longer fires all of them a frame late, and
//!     nothing in the output bytes shows it.
//!   * No scratch demo survives the patch.
//!
//! ```text
//! cargo run --release -p native --bin verify_decal_pipeline -- <demo> <outdir>
//! ```

use native::patch::scanner::scan_demo_for_highlights;
use native::patch::types::PatcherConfig;
use native::patch::{build_batch_queue, prepare_flushed_source, StreamPatcher};

fn frame_count(path: &std::path::Path) -> Option<usize> {
    let bytes = std::fs::read(path).ok()?;
    let demo = dem::open_demo_from_bytes(&bytes).ok()?;
    Some(demo.directory.entries.iter().map(|e| e.frames.len()).sum())
}

fn main() {
    let mut args = std::env::args().skip(1);
    let demo = args.next().expect("usage: <demo> <outdir> [game_path]");
    let outdir = std::path::PathBuf::from(args.next().expect("outdir"));
    let path = std::path::PathBuf::from(&demo);
    std::fs::create_dir_all(&outdir).unwrap();

    // Defaults to the demo's own folder, which is where the game keeps them,
    // so `dod/maps` resolves without being told.
    // Demos live INSIDE the dod folder, so hl.exe sits one level above them.
    // Getting this wrong points the map lookup at dod/dod/maps and the flush
    // quietly falls back to the cone — which is exactly what it did first time,
    // and the only visible symptom was a line in the log.
    let game_path = args
        .next()
        .map(std::path::PathBuf::from)
        .or_else(|| {
            path.parent()
                .and_then(|p| p.parent())
                .map(|p| p.join("hl.exe"))
        })
        .expect("could not locate hl.exe; pass it as the third argument");

    let (_t, streaks, _p, _i, _f, _m, _ft) = scan_demo_for_highlights(&path).expect("scan");
    let streaks: Vec<_> = streaks.into_iter().take(8).collect();
    assert!(!streaks.is_empty(), "demo has no highlights to capture");

    let mut config = PatcherConfig::default();
    config.capture_directories = vec![outdir.clone()];
    config.primary_media_dir = Some(outdir.clone());
    config.game_path = game_path.to_string_lossy().to_string();
    config.record_start_lead = 1.5;
    config.record_stop_trail = 1.5;
    config.init_commands = vec!["mirv_fov 105".to_string()];

    println!("game_path: {}", config.game_path);
    println!(
        "maps dir : {}",
        std::path::Path::new(&config.game_path)
            .parent()
            .map(|p| {
                let d = p.join("dod").join("maps");
                format!("{} (exists: {})", d.display(), d.is_dir())
            })
            .unwrap_or_else(|| "<no parent>".to_string())
    );

    let (jobs, _) = build_batch_queue(streaks, &config, &std::collections::HashMap::new()).unwrap();
    let source_frames = frame_count(&path).expect("source parses");
    println!("source: {} frames", source_frames);

    let mut failures = 0;
    for job in &jobs {
        let label = job
            .output_demo
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();

        if job.blocks.is_empty() {
            println!("{:<16} no blocks (primer/preview) — flush correctly skipped", label);
        } else {
            let pinned = job
                .init_commands
                .iter()
                .rev()
                .find(|c| c.starts_with("r_decals"))
                .cloned()
                .unwrap_or_else(|| "<none>".to_string());

            let started = std::time::Instant::now();
            match prepare_flushed_source(job, &config) {
                Some(cleaned) => {
                    let n = frame_count(cleaned.path()).expect("cleaned parses");
                    let ok = n == source_frames;
                    if !ok {
                        failures += 1;
                    }
                    println!(
                        "{:<16} {} blocks | cleaned {} frames [{}] | pin: {} | {:?}",
                        label,
                        job.blocks.len(),
                        n,
                        if ok { "ORDINALS MATCH" } else { "ORDINAL SHIFT" },
                        pinned,
                        started.elapsed()
                    );
                }
                None => {
                    failures += 1;
                    println!("{:<16} FLUSH SKIPPED — see the activity log for why", label);
                }
            }
        }

        let started = std::time::Instant::now();
        let patcher = StreamPatcher::new(&job.source_demo, &job.output_demo);
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        patcher.patch(job, &config, &cancel).expect("patch");
        println!("{:<16} patched in {:?}", label, started.elapsed());
    }

    let leftovers = std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("dodtools_decalflush_")
        })
        .count();
    if leftovers > 0 {
        failures += 1;
    }
    println!("leftover scratch demos: {}", leftovers);

    if failures > 0 {
        eprintln!("FAILED: {} problem(s)", failures);
        std::process::exit(1);
    }
    println!("OK");
}
