//! CLI utility to test decal hygiene in isolation: strips wall-decal messages
//! outside the given capture windows, injects ring-sweeping flush bursts ahead
//! of each window, and pins r_decals so the sweep is cheap. Writes a patched
//! demo for manual playback verification in HLAE before any of this gets wired
//! into the real capture batch pipeline.

use clap::Parser;
use native::patch::{clean_demo_decals, Cancel, DecalCleanOptions, FlushSource, MAX_OVERLAP_DECALS};
use std::fs;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Cleans wall decals (bullet holes/grenade marks/sprays) ahead of each capture window in a DoD demo."
)]
struct Args {
    /// Path to the source .dem file
    demo_path: PathBuf,

    /// Output path for the cleaned demo
    #[arg(long)]
    out: PathBuf,

    /// Capture window to keep decals in, e.g. --keep 64484-70262. Repeat for
    /// multiple clips. These should be the real record-start/record-stop ticks.
    #[arg(long = "keep", value_parser = parse_range, required = true)]
    keep_windows: Vec<(i32, i32)>,

    /// Value to pin r_decals to. Burst size follows from this — a full ring
    /// revolution is what guarantees every occupied slot gets unlinked.
    #[arg(long, default_value_t = 256)]
    ring_limit: u32,

    /// Skip stripping decal messages outside the capture windows.
    #[arg(long)]
    no_strip: bool,

    /// Skip injecting the flush bursts.
    #[arg(long)]
    no_burst: bool,

    /// Hand-picked flush coordinate "x,y,z", overriding spawn detection.
    #[arg(long, value_parser = parse_coord)]
    flush_coord: Option<[f32; 3]>,

    /// Decal texture index for the flush burst, overriding the harvested one.
    /// Use when a demo has no bullet-hole decal to borrow a small texture from.
    #[arg(long)]
    flush_texture_index: Option<u8>,

    /// Vertical drop from the settled spawn origin to the floor.
    #[arg(long, default_value_t = 36.0)]
    floor_drop: f32,

    /// Minimum distance the flush coordinate must keep from every camera
    /// position recorded inside a capture window.
    #[arg(long, default_value_t = 900.0)]
    min_camera_clearance: f32,

    /// How long, in seconds of demo time, before a capture window the flush
    /// burst finishes. Larger values leave a longer visibly-clean stretch
    /// before the clip, which is useful when watching the sweep happen.
    #[arg(long, default_value_t = native::patch::DEFAULT_LEAD_SECONDS)]
    lead_seconds: f32,
}

fn parse_range(s: &str) -> Result<(i32, i32), String> {
    let (start, stop) = s
        .split_once('-')
        .ok_or_else(|| format!("expected START-STOP, got '{}'", s))?;
    let start: i32 = start
        .trim()
        .parse()
        .map_err(|_| format!("bad start tick in '{}'", s))?;
    let stop: i32 = stop
        .trim()
        .parse()
        .map_err(|_| format!("bad stop tick in '{}'", s))?;
    Ok((start, stop))
}

fn parse_coord(s: &str) -> Result<[f32; 3], String> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 3 {
        return Err(format!("expected x,y,z — got '{}'", s));
    }
    let mut out = [0.0f32; 3];
    for (i, p) in parts.iter().enumerate() {
        out[i] = p
            .trim()
            .parse()
            .map_err(|_| format!("bad number '{}' in '{}'", p, s))?;
    }
    Ok(out)
}

fn main() {
    let args = Args::parse();

    let bytes = match fs::read(&args.demo_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Error reading demo file: {}", e);
            std::process::exit(1);
        }
    };

    let opts = DecalCleanOptions {
        strip_outside_windows: !args.no_strip,
        flush_burst: !args.no_burst,
        ring_limit: args.ring_limit,
        flush_coord: args.flush_coord,
        flush_texture_index: args.flush_texture_index,
        floor_drop: args.floor_drop,
        min_camera_clearance: args.min_camera_clearance,
        lead_seconds: args.lead_seconds,
        ..Default::default()
    };

    match clean_demo_decals(&bytes, &args.keep_windows, &opts, Cancel::never()) {
        Ok((out_bytes, stats)) => {
            if let Err(e) = fs::write(&args.out, &out_bytes) {
                eprintln!("Error writing output demo: {}", e);
                std::process::exit(1);
            }
            println!("Wrote cleaned demo to: {}", args.out.display());
            println!("Capture windows kept:  {:?}", args.keep_windows);
            println!("r_decals pinned to:    {}", args.ring_limit);
            println!();
            println!("Decals stripped outside windows: {}", stats.temp_entity_stripped);
            println!("Player sprays stripped:          {}", stats.player_spray_stripped);
            println!("Flush decals injected:           {}", stats.flush_decals_injected);
            println!("Bursts placed:                   {}", stats.bursts_placed);
            println!();
            println!("Real decals harvested (survey):  {}", stats.harvested_decals);
            match stats.spawn_reference {
                Some(p) => println!("Settled spawn origin:  [{:.1}, {:.1}, {:.1}]", p[0], p[1], p[2]),
                None => println!("Settled spawn origin:  <none found — never saw a stable on-ground run>"),
            }
            match stats.flush_coord {
                Some(p) => println!("Flush coordinate:      [{:.1}, {:.1}, {:.1}]", p[0], p[1], p[2]),
                None => println!("Flush coordinate:      <none — burst skipped>"),
            }
            match stats.flush_source {
                Some(FlushSource::Override) => {
                    println!("Flush source:          override (caller supplied)")
                }
                Some(FlushSource::HarvestedNearSpawn) => println!(
                    "Flush source:          real decal near spawn (surface PROVEN by the demo)"
                ),
                Some(FlushSource::ComputedSpawnFloor) => println!(
                    "Flush source:          computed spawn floor (GEOMETRIC GUESS — verify in game)"
                ),
                Some(FlushSource::PlayerFloorPath) => println!(
                    "Flush source:          floor under the player's own path (surfaces they stood on)"
                ),
                Some(FlushSource::TiledPlane) => println!(
                    "Flush source:          grid tiled across a plane fitted to real decals"
                ),
                Some(FlushSource::MapAtlas) => println!(
                    "Flush source:          the map's accumulated coordinate store"
                ),
                Some(FlushSource::MapGeometry) => println!(
                    "Flush source:          sampled off the map's own world faces (BSP geometry)"
                ),
                None => println!("Flush source:          <none>"),
            }
            println!(
                "Spread across:         {} of {} positions needed{}",
                stats.flush_positions,
                stats.flush_positions_wanted,
                if stats.flush_positions >= stats.flush_positions_wanted { "  (full sweep)" } else { "" }
            );
            if stats.flush_positions < stats.flush_positions_wanted {
                println!(
                    "  WARNING: too few distinct spots. The engine recycles a decal instead of\n  \
                     allocating once {} overlap at one spot, and a recycled decal does not\n  \
                     advance the ring — so the sweep will fall short of a full revolution.",
                    MAX_OVERLAP_DECALS
                );
            }
            match stats.spawn_to_flush_distance {
                Some(d) => println!("Distance from spawn:   {:.0} units", d),
                None => println!("Distance from spawn:   <unknown>"),
            }
            match stats.flush_texture_index {
                Some(i) => println!("Flush texture index:   {} (harvested from a real decal)", i),
                None => println!("Flush texture index:   <none — burst skipped>"),
            }
            match stats.min_camera_distance {
                Some(d) => println!("Closest camera approach: {:.0} units", d),
                None => println!("Closest camera approach: <no in-window camera samples>"),
            }
            println!(
                "On camera during a clip: {} of {} sampled frames{}",
                stats.flush_on_camera_frames,
                stats.camera_samples,
                if stats.flush_on_camera_frames == 0 { "  (clear)" } else { "" }
            );
            if stats.flush_on_camera_frames > 0 {
                println!(
                    "  WARNING: the flush stack falls inside the camera's view during a\n  \
                     recorded clip. No decal in this demo was both far enough from every\n  \
                     capture camera and out of its line of sight. Re-run with\n  \
                     --flush-coord x,y,z to hand-pick a spot."
                );
            }

            if !stats.bursts_short.is_empty() {
                println!();
                println!("WARNING: {} window(s) had too little room for a full sweep.", stats.bursts_short.len());
                println!("These clips are NOT guaranteed clean:");
                for (tick, placed, wanted) in &stats.bursts_short {
                    println!("  window @ tick {:>7}: placed {}/{}", tick, placed, wanted);
                }
            }
        }
        Err(e) => {
            eprintln!("Error cleaning decals: {}", e);
            std::process::exit(1);
        }
    }
}
