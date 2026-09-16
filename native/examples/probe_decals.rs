//! Measures GoldSrc's decal placement tolerance — how far from a solid surface
//! a decal message's position may sit and still create a decal.
//!
//! Strips every decal out of a demo, stamps three-row grids of decals onto
//! walls the demo proves exist, and pins r_decals high. Each column of a grid
//! uses a larger offset than the last, so a row simply runs out of holes where
//! the engine stopped accepting the position. Play the output back, count the
//! holes in each row, and the threshold falls out.
//!
//! Several grids get stamped rather than one, and by default only in the spawn
//! area. A POV demo cannot be steered — the viewer sees exactly what the
//! recorded player saw — so the binding constraint is not "can this wall be
//! seen from somewhere" but "how often is it in front of you". That is spawn:
//! the demo opens there, and a dead player spectates teammates who are
//! themselves in spawn.
//!
//! This is the measurement the flush burst in `strip_decals` currently guesses
//! at: if positions can be synthesised near one known-good surface point rather
//! than only harvested from the demo, position supply stops being the binding
//! constraint on the ring sweep.

use clap::Parser;
use native::patch::{
    best_view_for, camera_at_time, decal_texture_histogram, probe_decal_offsets, project,
    CameraView, GridStats, ProbeOptions, ProbeRow, ProbeStats,
};
use std::fs;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Stamps offset-measurement decal grids onto walls in a DoD demo."
)]
struct Args {
    /// Path to the source .dem file
    demo_path: PathBuf,

    /// Output path for the probe demo
    #[arg(long, default_value = "probe_out.dem")]
    out: PathBuf,

    /// Offsets from the surface to test, ascending, e.g. --offsets 2,4,8,16,32
    /// Taken as a string and split here rather than by clap: a Vec<f32> field
    /// makes clap treat the flag as a multi-value list and panic trying to
    /// downcast the parser output.
    #[arg(long)]
    offsets: Option<String>,

    /// How many separate walls to stamp a grid onto.
    #[arg(long)]
    grids: Option<usize>,

    /// Gap between adjacent columns, along the wall.
    #[arg(long)]
    column_spacing: Option<f32>,

    /// Gap between the three rows, across the wall.
    #[arg(long)]
    row_gap: Option<f32>,

    /// Decal texture index, overriding the one harvested from the demo.
    #[arg(long)]
    texture_index: Option<u8>,

    /// Force the surface's normal axis: 0=x, 1=y, 2=z.
    #[arg(long)]
    axis: Option<usize>,

    /// Hand-picked anchor "x,y,z" on a known surface, skipping detection.
    /// Requires --axis.
    #[arg(long, value_parser = parse_coord)]
    anchor: Option<[f32; 3]>,

    /// Restrict grids to within --near-radius of "x,y,z" instead of spawn.
    #[arg(long, value_parser = parse_coord)]
    near: Option<[f32; 3]>,

    /// Restrict grids to wherever the camera is at this viewdemo timestamp,
    /// given as mm:ss:ff. Easier than a coordinate when you can see the spot
    /// you mean on screen but not its position.
    #[arg(long, value_parser = parse_clock)]
    near_at: Option<f32>,

    /// Radius of the spawn (or --near) restriction.
    #[arg(long)]
    near_radius: Option<f32>,

    /// Consider walls anywhere on the map, not just around spawn.
    #[arg(long)]
    whole_map: bool,

    /// How close the camera must physically come to a wall for it to be worth
    /// stamping. Stands in for an occlusion test.
    #[arg(long)]
    require_approach: Option<f32>,

    /// Decals that must share a patch before it counts as a wall.
    #[arg(long)]
    min_plane_decals: Option<usize>,

    /// Extent a patch's decals must span to count as a wall rather than a
    /// cluster on some small prop.
    #[arg(long)]
    min_plane_spread: Option<f32>,

    /// Value to pin r_decals to.
    #[arg(long)]
    ring_limit: Option<u32>,

    /// Leave the demo's own decals in place instead of stripping them.
    #[arg(long)]
    no_strip: bool,

    /// Decal texture index per row, "out,ctl,in". Three different marks make a
    /// grid obviously deliberate rather than looking like ordinary gunfire, and
    /// make the three counts impossible to conflate. See --list-textures.
    #[arg(long, value_parser = parse_row_textures)]
    row_textures: Option<[u8; 3]>,

    /// Texture index for a beacon set clear of each grid: a short line of large
    /// marks that makes a grid findable from across a room without merging into
    /// the holes being counted. A grenade scorch works well. See --list-textures.
    #[arg(long)]
    beacon_texture: Option<u8>,

    /// Stamp only one row: "out", "ctl" or "in". One line of holes per demo is
    /// far easier to count than three at once.
    #[arg(long, value_parser = parse_row)]
    only_row: Option<ProbeRow>,

    /// Slide the grid along the surface by this many units, positive being
    /// rightward as seen from the best viewing position. Use it to nudge a
    /// grid clear of a body or a crate.
    #[arg(long, allow_negative_numbers = true)]
    shift_right: Option<f32>,

    /// Decals stacked at each position, 1-5. Darkens each mark without
    /// widening it — useful when a small hole vanishes into a speckled
    /// texture like sand.
    #[arg(long)]
    stack: Option<usize>,

    /// Print where each hole lands in a screenshot taken at this viewdemo
    /// timestamp (mm:ss:ff), as pixel coordinates. Needs --screen.
    #[arg(long, value_parser = parse_clock)]
    project_at: Option<f32>,

    /// Screenshot dimensions for --project-at, as WxH.
    #[arg(long, value_parser = parse_screen)]
    screen: Option<(f32, f32)>,

    /// Horizontal field of view for --project-at. DoD default_fov is 90.
    #[arg(long, default_value_t = 90.0)]
    fov: f32,

    /// Find the frame that shows the most holes at once, well separated, and
    /// project them there. Needs --screen.
    #[arg(long)]
    best_view: bool,

    /// List the decal texture indices this demo uses, and exit.
    #[arg(long)]
    list_textures: bool,
}

/// Parses a row name into the row it selects.
/// The offsets a grid tests, taken from whichever row it actually carries.
/// A single-row demo has no CTL row to read them off.
fn grid_offsets(g: &GridStats) -> Vec<f32> {
    let Some(first) = g.probes.first().map(|p| p.row) else {
        return Vec::new();
    };
    g.probes
        .iter()
        .filter(|p| p.row == first)
        .map(|p| p.offset)
        .collect()
}

fn parse_row(s: &str) -> Result<ProbeRow, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "out" => Ok(ProbeRow::Out),
        "ctl" | "control" => Ok(ProbeRow::Control),
        "in" => Ok(ProbeRow::In),
        other => Err(format!("expected out, ctl or in — got '{}'", other)),
    }
}

fn parse_row_textures(s: &str) -> Result<[u8; 3], String> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 3 {
        return Err(format!("expected out,ctl,in — got '{}'", s));
    }
    let mut out = [0u8; 3];
    for (i, p) in parts.iter().enumerate() {
        out[i] = p
            .trim()
            .parse()
            .map_err(|_| format!("bad texture index '{}' in '{}'", p, s))?;
    }
    Ok(out)
}

/// Parses viewdemo's own mm:ss:ff readout into seconds.
fn parse_clock(s: &str) -> Result<f32, String> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return Err(format!("expected mm:ss:ff as the demo player shows it — got '{}'", s));
    }
    let num = |p: &str| -> Result<f32, String> {
        p.trim()
            .parse::<f32>()
            .map_err(|_| format!("bad number '{}' in '{}'", p, s))
    };
    Ok(num(parts[0])? * 60.0 + num(parts[1])? + num(parts[2])? / 100.0)
}

/// Parses a "WxH" screenshot size.
fn parse_screen(s: &str) -> Result<(f32, f32), String> {
    let (w, h) = s
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("expected WxH — got '{}'", s))?;
    let num = |p: &str| -> Result<f32, String> {
        p.trim()
            .parse::<f32>()
            .map_err(|_| format!("bad number '{}' in '{}'", p, s))
    };
    Ok((num(w)?, num(h)?))
}

fn parse_offsets(s: &str) -> Result<Vec<f32>, String> {
    s.split(',')
        .map(|p| {
            p.trim()
                .parse::<f32>()
                .map_err(|_| format!("bad offset '{}' in '{}'", p, s))
        })
        .collect()
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

fn axis_name(axis: usize) -> &'static str {
    ["X", "Y", "Z"][axis]
}

/// Viewdemo's own `mm:ss:ff` readout, hundredths in the last field — the exact
/// string the demo player shows, so a timestamp can be matched against it
/// rather than converted.
fn clock(seconds: f32) -> String {
    let hundredths = (seconds.max(0.0) * 100.0).round() as u64;
    format!(
        "{}:{:02}:{:02}",
        hundredths / 6000,
        (hundredths / 100) % 60,
        hundredths % 100
    )
}

/// Where to look, in words. World coordinates are no use to someone watching a
/// demo — there is no ruler on screen.
fn where_to_look(yaw: f32, pitch: f32, distance: f32) -> String {
    let side = if yaw.abs() < 7.0 {
        "dead ahead".to_string()
    } else {
        let hand = if yaw < 0.0 { "left" } else { "right" };
        match yaw.abs() {
            a if a < 18.0 => format!("just {} of centre", hand),
            a if a < 32.0 => format!("{} of centre", hand),
            _ => format!("far {}, near the edge of frame", hand),
        }
    };

    let height = match pitch {
        p if p.abs() < 8.0 => "at eye level",
        p if p < -30.0 => "well below you — look down",
        p if p < 0.0 => "a little below eye level",
        p if p > 30.0 => "well above you — look up",
        _ => "a little above eye level",
    };

    let range = match distance {
        d if d < 100.0 => "close enough to touch",
        d if d < 250.0 => "a few steps away",
        d if d < 500.0 => "across the room",
        _ => "down the far end",
    };

    format!("{}, {} — {}", side, height, range)
}

/// Which of the three parallel lines is which, from where the viewer will be.
///
/// All three rows wear the same mark, so there is nothing on screen to tell
/// them apart, and "the row displaced along +Y" is not something anyone can act
/// on while watching a demo.
fn which_row_is_which(v: &[f32; 3]) -> String {
    let (f, r, u) = (v[0], v[1], v[2]);
    let (out, inn) = if f.abs() >= r.abs() && f.abs() >= u.abs() {
        if f > 0.0 {
            ("far", "near")
        } else {
            ("near", "far")
        }
    } else if r.abs() >= u.abs() {
        if r > 0.0 {
            ("right-hand", "left-hand")
        } else {
            ("left-hand", "right-hand")
        }
    } else if u > 0.0 {
        ("top", "bottom")
    } else {
        ("bottom", "top")
    };
    format!("the {} line is OUT, the middle is CTL, the {} line is IN", out, inn)
}

/// Prints one grid: where it is, how well the demo backs it, and when it is on
/// screen.
fn print_grid(index: usize, g: &GridStats) {
    let normal = axis_name(g.axis);
    let col = axis_name(g.column_axis);
    let row = axis_name(g.row_axis);
    let sign = if g.outward > 0.0 { "+" } else { "-" };
    let offsets = grid_offsets(g);

    println!(
        "── GRID {}{} ────────────────────────────────────────────",
        index + 1,
        if g.in_region { "  (in spawn)" } else { "" }
    );
    println!(
        "   On a {}, roughly {:.0} x {:.0} units of it, proven by {} real decals.",
        if g.axis == 2 { "floor" } else { "wall" },
        g.plane_spread.0,
        g.plane_spread.1,
        g.plane_members
    );
    println!(
        "   (world: {} = {:.1}, open space {}{}, anchor [{:.0}, {:.0}, {:.0}])",
        normal, g.plane_value, sign, normal, g.anchor[0], g.anchor[1], g.anchor[2]
    );
    println!(
        "   Columns along {} at {:.0}-unit pitch, rows step along {}.",
        col, g.column_pitch, row
    );

    print!("   Offset:        ");
    for o in &offsets {
        print!("{:>6.0}", o);
    }
    println!();
    print!("   Nearest decal: ");
    for e in &g.column_evidence {
        if e.is_finite() {
            print!("{:>6.0}", e);
        } else {
            print!("{:>6}", "-");
        }
    }
    println!();

    if g.columns_backed < offsets.len() {
        println!(
            "   NOTE: {} of {} columns backed by a real decal; the rest are arithmetic.",
            g.columns_backed,
            offsets.len()
        );
    }

    println!(
        "   Camera gets within {:.0} units, and spends {} sampled frames nearby.",
        g.closest_approach, g.dwell_samples
    );
    let (near_n, near_spread) = g.local_relief;
    println!(
        "   Geometry near the plane over its footprint: {} decals spanning {:.0} units.",
        near_n, near_spread
    );
    if let Some(t) = g.witness_time {
        println!(
            "   At {} you watch a bullet land on this exact spot — the grid is centred there.",
            clock(t)
        );
        if let Some((yaw, pitch, d)) = g.witness_view {
            println!("   At that moment it sits {}.", where_to_look(yaw, pitch, d));
            println!("   (view: yaw {:+.1} deg, pitch {:+.1} deg, {:.0} units)", yaw, pitch, d);
        }
        if let Some(v) = g.out_row_in_view {
            println!("   From the best view, {}.", which_row_is_which(&v));
        }
    } else {
        println!("   No bullet was ever seen landing here, so visibility is inferred.");
    }
    if g.sightings.is_empty() {
        println!("   Never squarely on camera — you would have to find it yourself.");
    } else {
        println!("   On screen (best first):");
        for s in g.sightings.iter().take(5) {
            println!(
                "     {:>9}   {}",
                clock(s.svc_time),
                where_to_look(s.yaw_degrees, s.pitch_degrees, s.distance)
            );
        }
    }
    println!();
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

    if args.list_textures {
        match decal_texture_histogram(&bytes) {
            Ok(h) if h.is_empty() => println!("No decal texture indices found."),
            Ok(h) => {
                println!("Decal texture indices in {}:", args.demo_path.display());
                for (idx, count) in h {
                    println!("  index {:>3}   x{}", idx, count);
                }
            }
            Err(e) => {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        return;
    }

    let defaults = ProbeOptions::default();
    let opts = ProbeOptions {
        offsets: match args.offsets.as_deref().map(parse_offsets) {
            Some(Ok(v)) => v,
            Some(Err(e)) => {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
            None => defaults.offsets,
        },
        grids: args.grids.unwrap_or(defaults.grids),
        column_spacing: args.column_spacing.unwrap_or(defaults.column_spacing),
        row_gap: args.row_gap.unwrap_or(defaults.row_gap),
        texture_index: args.texture_index,
        row_textures: args.row_textures,
        beacon_texture: args.beacon_texture,
        only_row: args.only_row,
        shift_right: args.shift_right.unwrap_or(defaults.shift_right),
        stack: args.stack.unwrap_or(defaults.stack),
        axis: args.axis,
        anchor: args.anchor,
        near: args.near,
        near_at: args.near_at,
        near_radius: args.near_radius.unwrap_or(defaults.near_radius),
        spawn_only: !args.whole_map,
        require_approach: args.require_approach.unwrap_or(defaults.require_approach),
        min_plane_decals: args.min_plane_decals.unwrap_or(defaults.min_plane_decals),
        min_plane_spread: args.min_plane_spread.unwrap_or(defaults.min_plane_spread),
        ring_limit: args.ring_limit.unwrap_or(defaults.ring_limit),
        strip_all: !args.no_strip,
        ..defaults
    };

    let (out_bytes, stats) = match probe_decal_offsets(&bytes, &opts) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error building probe demo: {}", e);
            std::process::exit(1);
        }
    };

    if let Err(e) = fs::write(&args.out, &out_bytes) {
        eprintln!("Error writing output demo: {}", e);
        std::process::exit(1);
    }

    println!("Wrote probe demo to: {}", args.out.display());
    match opts.row_textures {
        Some(t) => println!(
            "{} grids, {} decals, stamped at frame ordinal {}. Row textures: OUT={} CTL={} IN={}.",
            stats.grids.len(),
            stats.decals_injected,
            stats.injected_at_ordinal,
            t[0],
            t[1],
            t[2]
        ),
        None => println!(
            "{} grids, {} decals, texture index {}, stamped at frame ordinal {}.",
            stats.grids.len(),
            stats.decals_injected,
            stats.texture_index,
            stats.injected_at_ordinal
        ),
    }
    println!(
        "Stripped {} decals and {} sprays; r_decals pinned to {}.",
        stats.decals_stripped, stats.sprays_stripped, opts.ring_limit
    );
    println!(
        "All of them appear at once at {}, and nothing evicts them after that.",
        clock(stats.injected_at_time)
    );
    match stats.region {
        Some(c) if !stats.region_abandoned => println!(
            "Restricted to within {:.0} units of [{:.0}, {:.0}, {:.0}]{}.",
            stats.region_radius,
            c[0],
            c[1],
            c[2],
            if args.near.is_some() { "" } else { " (spawn)" }
        ),
        Some(_) => println!(
            "WARNING: no wall met the spawn restriction, so it was dropped and these \
             grids are anywhere on the map."
        ),
        None => println!("Walls considered across the whole map."),
    }
    println!();

    for (i, g) in stats.grids.iter().enumerate() {
        print_grid(i, g);
    }

    if let Some(at) = args.project_at {
        let Some((w, h)) = args.screen else {
            eprintln!("--project-at needs --screen WxH (the screenshot's pixel size).");
            std::process::exit(1);
        };
        let view = match camera_at_time(&bytes, at) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        };
        project_grids(&stats, &view, args.fov, w, h);
    }

    if args.best_view {
        let Some((w, h)) = args.screen else {
            eprintln!("--best-view needs --screen WxH (the screenshot's pixel size).");
            std::process::exit(1);
        };
        // Beacon marks are excluded from the search: they are there to be found,
        // not counted, and letting them pull the choice of frame would trade
        // away the holes that carry the measurement.
        let points: Vec<[f32; 3]> = stats
            .grids
            .iter()
            .flat_map(|g| g.probes.iter().map(|p| p.position))
            .collect();
        match best_view_for(&bytes, &points, args.fov, w, h) {
            Ok(Some((view, on, gap))) => {
                println!(
                    "BEST FRAME FOR COUNTING: {}  ({} of {} holes on screen, closest pair {:.0}px apart)",
                    clock(view.svc_time),
                    on,
                    points.len(),
                    gap
                );
                project_grids(&stats, &view, args.fov, w, h);
            }
            Ok(None) => println!("No frame shows any of the holes."),
            Err(e) => {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
    }


    let offsets = grid_offsets(&stats.grids[0]);

    println!("HOW TO READ IT");
    println!(
        "  Every grid is the same measurement — whichever one you spot first is a"
    );
    println!("  complete result, and two that agree are worth more than one you cannot find.");
    println!("  Play, don't seek.");
    println!();
    println!(
        "  Each is three parallel rows of {} holes. Where the rows step along Z, the",
        offsets.len()
    );
    println!("  top row is OUT (pushed off the wall into the room), the middle is CTL");
    println!("  (dead on the wall), the bottom is IN (pushed into the solid).");
    println!();
    println!("  Count the holes in each row and report three numbers.");
    println!(
        "    CTL must be {}/{}. Anything less means that grid overran an edge or a",
        offsets.len(),
        offsets.len()
    );
    println!("    doorway — say so and use another grid.");
    println!("    Every row's first hole sits ON the wall, so a row with no holes at all");
    println!("    missed the wall vertically and is void rather than a zero result.");
    println!("    The rest ascend by offset, so a row that stops after N holes accepted");
    println!("    up to that column's offset and rejected the next:");
    for (i, o) in offsets.iter().enumerate() {
        if *o == 0.0 {
            println!("      {} hole  -> on the wall only; no offset worked", i + 1);
        } else {
            println!("      {} holes -> accepted up to {:.0} units", i + 1, o);
        }
    }
    println!("      0 holes -> that row missed the wall; void for it");
    println!();
    println!("  If a hole shows up displaced from its row rather than missing, that still");
    println!("  counts as created — the engine projected it onto whichever surface it");
    println!("  found first. Say so and count it.");
    println!();
    println!("  mirv_fx_wh_enable 1 is worth a try for finding them: if HLAE's wallhack");
    println!("  draws decals through geometry it makes every grid findable at once. Count");
    println!("  one grid at a time if so — with several stamped, two can overlap on screen.");
}

/// Prints where every hole in every grid lands in a screenshot taken from
/// `view`.
///
/// The beacon marks are projected alongside the holes on purpose. They are
/// large and unmistakable, so if they land where the screenshot shows them the
/// calibration is right and a missing hole is a real miss; if they don't, the
/// fov or the screen size is wrong and nothing else printed here can be
/// trusted.
fn project_grids(stats: &ProbeStats, view: &CameraView, fov: f32, w: f32, h: f32) {
    println!(
        "WHERE TO LOOK IN A {:.0}x{:.0} SCREENSHOT AT {}",
        w,
        h,
        clock(view.svc_time)
    );
    println!(
        "  Camera at [{:.0}, {:.0}, {:.0}], fov {:.0}. Pixel coords from the top-left.",
        view.eye[0], view.eye[1], view.eye[2], fov
    );
    for (i, g) in stats.grids.iter().enumerate() {
        println!("  Grid {}:", i + 1);
        for p in &g.probes {
            match project(view, &p.position, fov, w, h) {
                Some((x, y)) if x >= 0.0 && x <= w && y >= 0.0 && y <= h => {
                    println!("    offset {:>3.0}  ->  x {:>5.0}, y {:>5.0}", p.offset, x, y)
                }
                Some((x, y)) => println!(
                    "    offset {:>3.0}  ->  off screen (x {:.0}, y {:.0})",
                    p.offset, x, y
                ),
                None => println!("    offset {:>3.0}  ->  behind the camera", p.offset),
            }
        }
        for (k, b) in g.beacon.iter().enumerate() {
            match project(view, b, fov, w, h) {
                Some((x, y)) if x >= 0.0 && x <= w && y >= 0.0 && y <= h => println!(
                    "    beacon {}  ->  x {:>5.0}, y {:>5.0}",
                    k + 1,
                    x,
                    y
                ),
                _ => println!("    beacon {}  ->  not in frame", k + 1),
            }
        }
    }
    println!();
}
