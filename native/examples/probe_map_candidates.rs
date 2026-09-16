//! How many flush positions a map can supply from its own geometry alone.
//!
//! Stage 3 of `docs/archive/decal_flush_bsp_surfaces.md`. Every other source of flush
//! coordinates is drawn from what people did in a match, so a map nobody has
//! captured yet supplies nothing; this asks what the map itself is worth,
//! without a demo, a camera path or a coordinate store anywhere in the picture.
//!
//! The number that matters is the last column against the ring being swept:
//! 68 positions turn a 256-slot ring, 1028 turn the 4096 ceiling. This is an
//! upper bound — the in-clip camera test still has to run, and it is what
//! decides how much of this survives — so a map short of the target here cannot
//! reach it in a capture either.
//!
//! ```text
//! cargo run --release -p native --bin probe_map_candidates -- <maps_dir_or_bsp> [label]
//! ```
//!
//! Columns: label, map, world faces, faces that take decals, faces large
//! enough to sample, candidates, distinct leaves they occupy, candidates in
//! open space, projection hit rate over a strided sample, elapsed ms.

use native::patch::bsp::{Bsp, FaceSampling, CONTENTS_SOLID, CONTENTS_SKY};

/// Only every Nth candidate is projected back onto a face: `decal_draw_point`
/// walks every world face, so the full set would cost more than the scan it is
/// checking. A parser fault shows up in a stride sample just as plainly.
const PROJECTION_STRIDE: usize = 25;

fn main() {
    let mut args = std::env::args().skip(1);
    let target = std::path::PathBuf::from(args.next().expect("usage: <maps_dir_or_bsp> [label]"));
    let label = args.next().unwrap_or_default();

    let maps: Vec<std::path::PathBuf> = if target.is_dir() {
        let mut found: Vec<_> = std::fs::read_dir(&target)
            .expect("maps dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension()
                    .map(|e| e.eq_ignore_ascii_case("bsp"))
                    .unwrap_or(false)
            })
            .collect();
        found.sort();
        found
    } else {
        vec![target]
    };

    for path in maps {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();

        let started = std::time::Instant::now();
        let bsp = match Bsp::from_file(&path) {
            Ok(b) => b,
            Err(e) => {
                println!("{}\t{}\tSKIP\t{}", label, name, e);
                continue;
            }
        };

        let opts = FaceSampling::default();
        let world = bsp.world_faces();
        let world_count = world.len();
        let decal_faces = world.clone().filter(|&i| bsp.face_takes_decals(i)).count();
        let big_enough = world
            .filter(|&i| bsp.face_takes_decals(i) && bsp.face_area(i) >= opts.min_area)
            .count();

        let candidates = bsp.face_candidates(&opts);

        // Distinct leaves the candidates land in. A sweep wants spread, and a
        // thousand points sharing six leaves is one wall, not a map.
        let mut leaves: Vec<usize> = candidates.iter().map(|p| bsp.leaf_at(p)).collect();
        leaves.sort_unstable();
        leaves.dedup();

        // A candidate inside solid is exactly what the projection bug produced:
        // occluded from everywhere, and drawn on whichever face the engine's
        // walk reaches. None of these should exist by construction — the
        // sampler steps off the face along its outward normal — so a non-zero
        // count here means the normal is being read backwards somewhere.
        let open = candidates
            .iter()
            .filter(|p| {
                let c = bsp.leaf_contents(bsp.leaf_at(p));
                c != CONTENTS_SOLID && c != CONTENTS_SKY
            })
            .count();

        let sampled: Vec<_> = candidates.iter().step_by(PROJECTION_STRIDE).collect();
        let projected = sampled
            .iter()
            .filter(|p| bsp.decal_draw_point(p, 4.0, 1.0).is_some())
            .count();
        let hit = if sampled.is_empty() {
            0.0
        } else {
            projected as f32 * 100.0 / sampled.len() as f32
        };

        println!(
            "{}\t{}\tOK\t{}\t{}\t{}\t{}\t{}\t{}\t{:.1}\t{}",
            label,
            name,
            world_count,
            decal_faces,
            big_enough,
            candidates.len(),
            leaves.len(),
            open,
            hit,
            started.elapsed().as_millis(),
        );
    }
}
