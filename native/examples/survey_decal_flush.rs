//! Reports what the decal flush would do to one demo, without capturing.
//!
//! Runs the pipeline's own window derivation and clean over a demo and prints a
//! single TSV line. Written to answer "did that change help or hurt", which for
//! this feature is never obvious from reading: the five demos in `local/demos`
//! once said tiling gave 4/5 full sweeps while an 85-demo sample across the
//! real library said a third of demos were getting essentially no flush.
//!
//! ```text
//! cargo run --release -p native --bin survey_decal_flush -- <demo> <scratch> <label>
//! ```
//!
//! Environment overrides, for A/B runs against a clean slate:
//!   `FLUSH_ATLAS_DIR`  coordinate store to use (default: none, so no store)
//!   `FLUSH_MAPS_DIR`   map directory (default: derived from the demo's folder)
//!   `FLUSH_FOV`        capture FOV (default 90)
//!   `FLUSH_LEAD_SECONDS` seconds the sweep must finish before a clip (default 2.0)
//!   `FLUSH_RING`       decal ring the sweep is sized to (default 256)
//!   `FLUSH_MAP_ONLY`   place from the map's own faces alone, skipping every
//!                      source drawn from the match — the offline twin of
//!                      `DOD_FLUSH_MAP_GEOMETRY_ONLY`
//!
//! Columns: label, demo, status, clips, positions, positions wanted, tiles,
//! camera-safe tiles, source, on-camera frames, harvested decals, atlas size,
//! visibility basis, map faces, bursts short, bursts placed, carriers inside a
//! clip, map-geometry candidates, camera-safe map-geometry candidates.
//!
//! Leaving `FLUSH_ATLAS_DIR` unset is how a **new map** is simulated: no
//! coordinate store, so the demo stands on its own decals and, failing those,
//! on the map's own faces.

use native::patch::scanner::scan_demo_for_highlights;
use native::patch::types::PatcherConfig;
use native::patch::{
    build_batch_queue, clean_demo_decals, on_screen_half_angle, DecalCleanOptions, FlushSource,
    VisibilityBasis,
};

fn main() {
    let mut args = std::env::args().skip(1);
    let demo = args.next().expect("usage: <demo> <scratch> <label>");
    let scratch = std::path::PathBuf::from(args.next().expect("scratch"));
    let label = args.next().unwrap_or_default();

    let path = std::path::PathBuf::from(&demo);
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let bail = |why: &str| {
        println!("{}\t{}\tSKIP\t{}", label, name, why);
        std::process::exit(0);
    };

    let Ok((_t, streaks, _p, _i, _f, _m, _ft)) = scan_demo_for_highlights(&path) else {
        bail("scan failed");
        return;
    };
    if streaks.is_empty() {
        bail("no highlights");
    }
    // Capped so one very long match does not skew a sample: the question is how
    // many positions a demo can offer, not how many clips it contains.
    let streaks: Vec<_> = streaks.into_iter().take(8).collect();

    std::fs::create_dir_all(scratch.join("mock_game").join("dod")).unwrap();
    let mut config = PatcherConfig::default();
    config.capture_directories = vec![scratch.clone()];
    config.primary_media_dir = Some(scratch.clone());
    config.game_path = scratch.join("mock_game").to_string_lossy().to_string();
    config.record_start_lead = 1.5;
    config.record_stop_trail = 1.5;

    let Ok((jobs, _)) = build_batch_queue(streaks, &config, &std::collections::HashMap::new())
    else {
        bail("build_batch_queue failed");
        return;
    };
    let Some(job) = jobs.iter().find(|j| !j.blocks.is_empty()) else {
        bail("no capture blocks");
        return;
    };

    // The same all-or-nothing rule prepare_flushed_source applies.
    let windows: Vec<(i32, i32)> = job
        .blocks
        .iter()
        .filter(|b| b.record_start_tick > 0 && b.record_stop_tick >= b.record_start_tick)
        .map(|b| (b.record_start_tick, b.record_stop_tick))
        .collect();
    if windows.len() != job.blocks.len() {
        bail("missing record bounds");
    }

    let Ok(bytes) = std::fs::read(&path) else {
        bail("read failed");
        return;
    };

    let fov: f32 = std::env::var("FLUSH_FOV")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(90.0);
    let maps_dir = std::env::var("FLUSH_MAPS_DIR")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(|| path.parent().map(|p| p.join("maps")))
        .filter(|p| p.is_dir());

    let ring: u32 = std::env::var("FLUSH_RING")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(256);

    // How far ahead of a clip the sweep must finish, in seconds of demo time.
    let lead: f32 = std::env::var("FLUSH_LEAD_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DecalCleanOptions::default().lead_seconds);

    let opts = DecalCleanOptions {
        inject_r_decals_command: false,
        atlas_dir: std::env::var("FLUSH_ATLAS_DIR")
            .ok()
            .map(std::path::PathBuf::from),
        maps_dir,
        ring_limit: ring,
        lead_seconds: lead,
        visibility_cone_degrees: on_screen_half_angle(fov, 1920, 1080),
        // The same gate the capture path reads from `DOD_FLUSH_MAP_GEOMETRY_ONLY`,
        // so the map source can be measured over a library before anyone spends
        // seven minutes watching it in game.
        map_geometry_only: std::env::var("FLUSH_MAP_ONLY").is_ok(),
        ..Default::default()
    };

    match clean_demo_decals(&bytes, &windows, &opts) {
        Ok((_out, s)) => {
            let source = match s.flush_source {
                Some(FlushSource::TiledPlane) => "tiled",
                Some(FlushSource::MapAtlas) => "atlas",
                Some(FlushSource::MapGeometry) => "mapgeom",
                Some(FlushSource::HarvestedNearSpawn) => "harvested",
                Some(FlushSource::PlayerFloorPath) => "floorpath",
                Some(FlushSource::ComputedSpawnFloor) => "computed",
                Some(FlushSource::Override) => "override",
                None => "none",
            };
            let basis = match s.visibility_basis {
                VisibilityBasis::Geometry => {
                    if s.map_has_vis {
                        "geometry"
                    } else {
                        "geometry-novis"
                    }
                }
                VisibilityBasis::ConeOnly => "cone",
            };
            println!(
                "{}\t{}\tOK\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                label,
                name,
                windows.len(),
                s.flush_positions,
                s.flush_positions_wanted,
                s.tiled_candidates,
                s.tiled_camera_safe,
                source,
                s.flush_on_camera_frames,
                s.harvested_decals,
                s.atlas.total,
                basis,
                s.map_faces,
                // Bursts that could not fit a whole sweep in the gap before
                // their clip — the constraint that decides whether a maximum
                // sweep is reachable at all.
                s.bursts_short.len(),
                s.bursts_placed,
                s.burst_frames_inside_clip,
                // Candidates sampled off the map's own faces, and how many
                // survived the camera test. Both 0 unless the proven sources
                // fell short and the map source was actually reached.
                s.map_candidates,
                s.map_camera_safe,
            );
        }
        Err(e) => println!("{}\t{}\tSKIP\tclean failed: {}", label, name, e),
    }
}
