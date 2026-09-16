//! Why a flush position the pipeline called hidden showed up on camera.
//!
//! ```text
//! cargo run --release -p native --bin probe_flush_positions -- <demo> <scratch> <ring>
//! ```
//!
//! Runs the real placement, then re-judges every chosen position against every
//! in-clip camera sample with each of the on-screen test's three gates taken
//! away in turn. Whichever gate is the only thing calling a position hidden is
//! the one letting it onto the screen.

use native::patch::bsp::Bsp;
use native::patch::scanner::scan_demo_for_highlights;
use native::patch::types::PatcherConfig;
use native::patch::{
    build_batch_queue, clean_demo_decals, on_screen_half_angle, DecalCleanOptions,
};

fn main() {
    let mut args = std::env::args().skip(1);
    let demo = std::path::PathBuf::from(args.next().expect("usage: <demo> <scratch> <ring>"));
    let scratch = std::path::PathBuf::from(args.next().expect("scratch"));
    let ring: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(4096);

    let (_t, streaks, _p, _i, _f, _m, _ft) = scan_demo_for_highlights(&demo).expect("scan");
    std::fs::create_dir_all(scratch.join("mock_game").join("dod")).unwrap();

    let mut config = PatcherConfig::default();
    config.capture_directories = Vec::new();
    config.primary_media_dir = Some(scratch.clone());
    config.game_path = scratch.join("mock_game").join("hl.exe").to_string_lossy().to_string();
    config.record_start_lead = 5.0;
    config.record_stop_trail = 5.0;
    config.pre_roll_seconds = 5.0;
    config.post_roll_seconds = 1.0;

    let (jobs, _) = build_batch_queue(streaks, &config, &std::collections::HashMap::new()).unwrap();
    for j in jobs.iter().filter(|j| !j.blocks.is_empty()) {
        println!(
            "job player {:?}  blocks {}  first window {}..{}",
            j.target_player, j.blocks.len(),
            j.blocks[0].record_start_tick, j.blocks[0].record_stop_tick
        );
    }
    // The capture that produced the artefact is one player's job, not simply
    // the first with blocks — every player in the demo yields one.
    let want = std::env::var("FLUSH_PLAYER").ok();
    let job = jobs
        .iter()
        .filter(|j| !j.blocks.is_empty())
        .find(|j| want.as_deref().is_none_or(|w| j.target_player.as_deref() == Some(w)))
        .expect("blocks");
    println!("
using job for player {:?}", job.target_player);
    let windows: Vec<(i32, i32)> = job
        .blocks
        .iter()
        .map(|b| (b.record_start_tick, b.record_stop_tick))
        .collect();

    let fov: f32 = std::env::var("FLUSH_FOV").ok().and_then(|s| s.parse().ok()).unwrap_or(105.0);
    let maps_dir = std::env::var("FLUSH_MAPS_DIR")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(|| demo.parent().map(|p| p.join("maps")));

    let cone_deg = on_screen_half_angle(fov, 1280, 720);
    let max_distance = DecalCleanOptions::default().visibility_max_distance;
    let opts = DecalCleanOptions {
        inject_r_decals_command: false,
        atlas_dir: std::env::var("FLUSH_ATLAS_DIR").ok().map(std::path::PathBuf::from),
        maps_dir: maps_dir.clone(),
        ring_limit: ring,
        visibility_cone_degrees: cone_deg,
        collect_diagnostics: true,
        require_pvs_hidden: std::env::var("FLUSH_PVS_ONLY").is_ok(),
        ..Default::default()
    };

    let bytes = std::fs::read(&demo).expect("read demo");
    let (_out, stats) = clean_demo_decals(&bytes, &windows, &opts).expect("clean");

    let positions = &stats.diagnostic_positions;
    let cameras = &stats.diagnostic_cameras;
    println!(
        "ring {}  positions {}  cameras {} (unsampled)  cone half-angle {:.1} deg  max_distance {}",
        ring, positions.len(), cameras.len(), cone_deg, max_distance
    );

    // Camera coverage per clip. A window with no samples is a clip the
    // selection never looked at, which no amount of correct testing can save.
    println!();
    for (i, (s, e)) in windows.iter().enumerate() {
        println!("  block {:>2}: window {:>7}..{:>7}  ({:>6} records)", i, s, e, e - s);
    }
    println!("  total in-window records: {}", windows.iter().map(|(s, e)| (e - s) as i64).sum::<i64>());

    let map_name = stats.atlas_map.clone().unwrap_or_default();
    let bsp = maps_dir.as_ref().and_then(|d| {
        let name = map_name.split_whitespace().next().unwrap_or("").to_string();
        Bsp::from_file(&d.join(format!("{}.bsp", name))).ok()
    });
    let cos_cone = cone_deg.to_radians().cos();

    // Each gate, counted as the LAST line of defence: how many positions are
    // called hidden by that gate alone, with the others already passed.
    let mut only_distance = 0usize;
    let mut only_cone = 0usize;
    let mut only_trace = 0usize;
    let mut visible_ignoring_distance = 0usize;
    let mut nearest_of_far = f32::INFINITY;
    let mut worst_visible_distance = 0.0f32;

    for p in positions {
        let mut in_cone_any = false;
        let mut in_cone_and_range = false;
        let mut fully_visible = false;
        let mut in_cone_clear_but_far = false;
        let mut min_d = f32::INFINITY;

        for (eye, fwd) in cameras {
            let v = [p[0] - eye[0], p[1] - eye[1], p[2] - eye[2]];
            let d = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            if d < 1.0 {
                continue;
            }
            min_d = min_d.min(d);
            let fl = (fwd[0] * fwd[0] + fwd[1] * fwd[1] + fwd[2] * fwd[2]).sqrt();
            if fl < 0.5 {
                continue;
            }
            if (v[0] * fwd[0] + v[1] * fwd[1] + v[2] * fwd[2]) / (d * fl) < cos_cone {
                continue;
            }
            in_cone_any = true;
            let clear = bsp.as_ref().map(|b| !b.line_blocked(eye, p)).unwrap_or(true);
            if !clear {
                continue;
            }
            if d <= max_distance {
                fully_visible = true;
                worst_visible_distance = worst_visible_distance.max(d);
            } else {
                in_cone_clear_but_far = true;
                nearest_of_far = nearest_of_far.min(d);
            }
            if d <= max_distance {
                in_cone_and_range = true;
            }
        }

        if fully_visible {
            only_trace += 0; // caught by nothing — this is a live failure
        }
        if in_cone_clear_but_far && !fully_visible {
            only_distance += 1;
        }
        if !in_cone_any {
            only_cone += 1;
        } else if !in_cone_and_range && !in_cone_clear_but_far {
            only_trace += 1;
        }
        if in_cone_clear_but_far || fully_visible {
            visible_ignoring_distance += 1;
        }
        let _ = min_d;
    }

    println!();
    println!("hidden only because the cone never covered them : {}", only_cone);
    println!("hidden only because a wall was in the way       : {}", only_trace);
    println!("hidden ONLY because they were beyond {:>4}u      : {}  <-- rendered, but not tested", max_distance, only_distance);
    println!();
    println!("in shot with an unobstructed view, at any range  : {} of {}", visible_ignoring_distance, positions.len());
    if nearest_of_far.is_finite() {
        println!("nearest such position beyond the cutoff          : {:.0} units", nearest_of_far);
    }
    println!("stat the pipeline reported (on-camera frames)    : {}", stats.flush_on_camera_frames);

    if let Some(b) = bsp.as_ref() {
        // The engine does not render a decal at the coordinate it is given: it
        // projects onto the surface nearest that coordinate. So test the FACE,
        // lifted a little onto its own front side, which is where the decal is
        // actually drawn and which may face the camera even when the point does
        // not.
        let mut face_visible = 0usize;
        let mut face_resolved = 0usize;
        for p in positions {
            // The engine projects onto the face; lifting the raw coordinate does
            // not, and would leave a buried point buried.
            let Some(surf) = b.decal_draw_point(p, 64.0, 1.0) else { continue };
            face_resolved += 1;
            let seen = cameras.iter().any(|(eye, fwd)| {
                let v = [surf[0] - eye[0], surf[1] - eye[1], surf[2] - eye[2]];
                let d = (v[0]*v[0] + v[1]*v[1] + v[2]*v[2]).sqrt();
                if d < 1.0 || d > max_distance { return false }
                let fl = (fwd[0]*fwd[0] + fwd[1]*fwd[1] + fwd[2]*fwd[2]).sqrt();
                if fl < 0.5 { return false }
                if (v[0]*fwd[0] + v[1]*fwd[1] + v[2]*fwd[2]) / (d * fl) < cos_cone { return false }
                !b.line_blocked(eye, &surf)
            });
            if seen { face_visible += 1 }
        }
        println!();
        println!("nearest world face resolved for                 : {} of {}", face_resolved, positions.len());
        println!("THE FACE THE ENGINE DRAWS ON is in shot for     : {} of {}  <-- what the camera actually sees", face_visible, positions.len());

        let solid = positions.iter().filter(|p| {
            let c = b.leaf_contents(b.leaf_at(p));
            c == native::patch::bsp::CONTENTS_SOLID || c == native::patch::bsp::CONTENTS_SKY
        }).count();
        println!();
        println!("chosen positions whose own leaf is SOLID        : {} of {}", solid, positions.len());
        let near_face = positions.iter().filter(|p| b.nearest_face(p, 4.0).is_some()).count();
        println!("chosen positions within 4u of a world face      : {} of {}", near_face, positions.len());
    }
}
