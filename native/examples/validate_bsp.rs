//! Checks the BSP parser against coordinates the engine provably accepted.
//!
//! Stage 2 of `docs/archive/decal_flush_bsp_surfaces.md`, kept because it is the only
//! honest test the map geometry has. Every harvested decal in a demo is a point
//! the engine created a decal at, so running thousands of them against the
//! parsed geometry validates the lump offsets, edge winding, coordinate space
//! and world-model selection in one pass. A wrong offset collapses the hit rate
//! rather than producing plausible garbage.
//!
//! Measured 2026-08-26 over 107,584 coordinates from 83 demos across 18 maps:
//! 98.93% land on a world face within 1 unit. Re-run it after touching
//! `patch::bsp` — a drop means the parser broke, not that the demos changed.
//!
//! ```text
//! cargo run --release -p native --bin validate_bsp -- <demo> <maps_dir> <label>
//! ```
//!
//! Prints one TSV line: label, demo, map, coordinates tested, hit percentage at
//! 1/2/4/8 units, count landing on nothing, world faces, total faces.

use native::patch::bsp::{Bsp, CONTENTS_SOLID};
use native::patch::decal_atlas::MapKey;
use native::patch::decal_strip::proven_world_coordinates;

fn main() {
    let mut args = std::env::args().skip(1);
    let demo_path = args.next().expect("usage: <demo> <maps_dir> <label>");
    let maps_dir = std::path::PathBuf::from(args.next().expect("maps dir"));
    let label = args.next().unwrap_or_default();

    let path = std::path::PathBuf::from(&demo_path);
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let bail = |why: &str| {
        println!("{}\t{}\tSKIP\t{}", label, name, why);
        std::process::exit(0);
    };

    let Ok(bytes) = std::fs::read(&path) else {
        bail("read failed");
        return;
    };
    let Ok(demo) = dem::open_demo_from_bytes(&bytes) else {
        bail("parse failed");
        return;
    };
    let Some(key) = MapKey::from_header(&demo.header) else {
        bail("no map name in header");
        return;
    };

    let bsp_path = maps_dir.join(format!("{}.bsp", key.name));
    let bsp = match Bsp::from_file(&bsp_path) {
        Ok(b) => b,
        Err(e) => {
            bail(&format!("bsp: {}", e));
            return;
        }
    };

    // Harvest with no capture windows: every decal in the demo, which is what
    // we want here — this is about the parser, not about the flush.
    // World decals only. A mark on a door is a true coordinate for the demo but
    // sits on a brush entity, and the world model is what we parsed.
    let pts = proven_world_coordinates(&demo);
    let pts = &pts;
    if pts.is_empty() {
        bail("no world decals harvested");
    }

    let mut hits = [0usize; 4];
    let tolerances = [1.0f32, 2.0, 4.0, 8.0];
    let mut worst_miss = 0usize;

    // The node tree gets the same treatment as the faces. A decal sits ON a
    // surface, so a point nudged along the face normal must land in open space
    // and the same nudge the other way must land in solid. If the tree were
    // being read wrongly those would not separate.
    let mut open_side = 0usize;
    let mut solid_side = 0usize;
    let mut normal_known = 0usize;

    for p in pts {
        let mut landed = false;
        for (i, tol) in tolerances.iter().enumerate() {
            if bsp.nearest_face(p, *tol).is_some() {
                hits[i] += 1;
                landed = true;
            }
        }
        if !landed {
            worst_miss += 1;
        }

        if let Some((face, _)) = bsp.nearest_face(p, 2.0)
            && let Some(n) = bsp.face_normal(face) {
                normal_known += 1;
                let out = [p[0] + n[0] * 4.0, p[1] + n[1] * 4.0, p[2] + n[2] * 4.0];
                let inn = [p[0] - n[0] * 4.0, p[1] - n[1] * 4.0, p[2] - n[2] * 4.0];
                if bsp.leaves.get(bsp.leaf_at(&out)).map(|l| l.contents) != Some(CONTENTS_SOLID) {
                    open_side += 1;
                }
                if bsp.leaves.get(bsp.leaf_at(&inn)).map(|l| l.contents) == Some(CONTENTS_SOLID) {
                    solid_side += 1;
                }
            }
    }

    let pct = |n: usize| (n as f32 / pts.len() as f32) * 100.0;
    println!(
        "{}\t{}\tOK\t{}\t{}\t{:.1}\t{:.1}\t{:.1}\t{:.1}\t{}\t{}\t{}\t{:.1}\t{:.1}\t{}",
        label,
        name,
        key.name,
        pts.len(),
        pct(hits[0]),
        pct(hits[1]),
        pct(hits[2]),
        pct(hits[3]),
        worst_miss,
        bsp.world_faces().len(),
        bsp.faces.len(),
        if normal_known > 0 { open_side as f32 * 100.0 / normal_known as f32 } else { 0.0 },
        if normal_known > 0 { solid_side as f32 * 100.0 / normal_known as f32 } else { 0.0 },
        if bsp.has_vis() { "vis" } else { "NOVIS" },
    );
}
