//! Times the decal-flush pipeline on one demo, split parse vs. clean vs. write.
//!
//! Written to answer one question for #193: when a user cancels a batch
//! mid-decal-flush, where is the wait actually spent? Guessing put it in the
//! demo parse; measuring put it somewhere else entirely, which is what decided
//! where the cancellation checks went.
//!
//! On `k4-ktps9w3-ih-anzio-allies.dem` (110MB, 730k frames, release build):
//!
//! ```text
//!   fs::read                 ~20ms
//!   open_demo_from_bytes   ~1120ms
//!   clean_demo_decals      ~4830ms   of which write_to_bytes ~3000ms,
//!                                    survey/strip/burst 16-40ms each, and
//!                                    resolve_flush_positions ~330ms at ring 4096
//! ```
//!
//! So the serialise is the single largest stage by a wide margin, and the parse
//! is second. The parse cannot be
//! interrupted from this crate (it is one call into `dem-patch`); the serialise
//! now can be, via `Demo::write_to_bytes_cancellable`.
//!
//! ```text
//! cargo run --release -p native --example flush_stage_timing -- <demo>
//! ```
//!
//! Honours the same `FLUSH_MAPS_DIR` / `FLUSH_RING` overrides as
//! `survey_decal_flush`, since both change how much work the middle stages do.
use dem::open_demo_from_bytes;
use native::patch::scanner::scan_demo_for_highlights;
use native::patch::types::PatcherConfig;
use native::patch::{build_batch_queue, clean_demo_decals, Cancel, DecalCleanOptions};

fn main() {
    let demo = std::env::args().nth(1).expect("usage: <demo>");
    let path = std::path::PathBuf::from(&demo);
    let name = path.file_name().unwrap().to_string_lossy().into_owned();

    let t = std::time::Instant::now();
    let bytes = std::fs::read(&path).expect("read");
    let read_ms = t.elapsed().as_millis();

    let t = std::time::Instant::now();
    let parsed = open_demo_from_bytes(&bytes).expect("parse");
    let parse_ms = t.elapsed().as_millis();
    let frames: usize = parsed.directory.entries.iter().map(|e| e.frames.len()).sum();
    drop(parsed);

    // The pipeline's own window derivation, so the clean below does the same
    // amount of work a real capture job would.
    let scratch = std::env::temp_dir().join("flush_stage_timing");
    std::fs::create_dir_all(scratch.join("mock_game").join("dod")).unwrap();
    let mut config = PatcherConfig::default();
    config.capture_directories = vec![scratch.clone()];
    config.primary_media_dir = Some(scratch.clone());
    config.game_path = scratch.join("mock_game").to_string_lossy().to_string();

    let Ok((_t, streaks, _p, _i, _f, _m, _ft)) = scan_demo_for_highlights(&path) else {
        println!("{name}\tSKIP\tscan failed");
        return;
    };
    let streaks: Vec<_> = streaks.into_iter().take(8).collect();
    if streaks.is_empty() {
        println!("{name}\tSKIP\tno highlights");
        return;
    }
    let Ok((jobs, _)) = build_batch_queue(streaks, &config, &std::collections::HashMap::new())
    else {
        println!("{name}\tSKIP\tbuild_batch_queue failed");
        return;
    };
    let Some(job) = jobs.iter().find(|j| !j.blocks.is_empty()) else {
        println!("{name}\tSKIP\tno capture blocks");
        return;
    };
    let windows: Vec<(i32, i32)> = job
        .blocks
        .iter()
        .filter(|b| b.record_start_tick > 0 && b.record_stop_tick >= b.record_start_tick)
        .map(|b| (b.record_start_tick, b.record_stop_tick))
        .collect();
    if windows.len() != job.blocks.len() {
        println!("{name}\tSKIP\tmissing record bounds");
        return;
    }

    let opts = DecalCleanOptions {
        inject_r_decals_command: false,
        maps_dir: std::env::var("FLUSH_MAPS_DIR")
            .ok()
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_dir()),
        ring_limit: std::env::var("FLUSH_RING")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(256),
        ..Default::default()
    };

    let t = std::time::Instant::now();
    let cleaned = clean_demo_decals(&bytes, &windows, &opts, Cancel::never());
    let clean_ms = t.elapsed().as_millis();

    println!(
        "{name}\tbytes={}\tframes={}\tclips={}\tread_ms={read_ms}\tparse_ms={parse_ms}\tclean_ms={clean_ms}\t{}",
        bytes.len(),
        frames,
        windows.len(),
        match cleaned {
            Ok(_) => "OK".to_string(),
            Err(e) => format!("ERR {e}"),
        }
    );
}
