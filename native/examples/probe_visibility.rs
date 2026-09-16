//! Whether the flush's line-of-sight test can actually see a decal.
//!
//! Written to answer one question after flush decals showed up on camera at a
//! 4096 ring while the log reported zero on-camera frames: does
//! `Bsp::line_blocked` call a decal hidden simply because the decal sits on the
//! wall the trace ends at?
//!
//! ```text
//! cargo run --release -p native --bin probe_visibility -- <map.bsp> <atlas.json>
//! ```
//!
//! For each engine-accepted decal coordinate in the atlas it reports which leaf
//! the point lands in, and whether a trace from a nearby open-space eye — one
//! placed off the surface along the face normal, i.e. exactly where a camera
//! looking at that decal would be — reaches it.

use native::patch::bsp::{Bsp, CONTENTS_SOLID, CONTENTS_SKY};

fn main() {
    let mut args = std::env::args().skip(1);
    let map = std::path::PathBuf::from(args.next().expect("usage: <map.bsp> <atlas.json>"));
    let atlas = std::path::PathBuf::from(args.next().expect("atlas"));

    let bsp = Bsp::from_file(&map).expect("parse bsp");
    let text = std::fs::read_to_string(&atlas).expect("read atlas");

    // Minimal extraction: the coords array is a flat list of [x,y,z] triples.
    let coords: Vec<[f32; 3]> = text
        .split("[[")
        .nth(1)
        .unwrap_or_default()
        .split("],[")
        .filter_map(|chunk| {
            let nums: Vec<f32> = chunk
                .trim_end_matches([']', '}'])
                .split(',')
                .filter_map(|n| n.trim().parse::<f32>().ok())
                .collect();
            (nums.len() == 3).then(|| [nums[0], nums[1], nums[2]])
        })
        .collect();

    println!("atlas coordinates: {}", coords.len());

    let mut in_solid = 0usize;
    let mut blocked_from_normal_eye = 0usize;
    let mut blocked_after_backoff = 0usize;
    let mut no_face = 0usize;

    for p in &coords {
        let leaf = bsp.leaf_at(p);
        let contents = bsp.leaf_contents(leaf);
        if contents == CONTENTS_SOLID || contents == CONTENTS_SKY {
            in_solid += 1;
        }

        // An eye 158 units off the surface along its own normal — the distance
        // the run logged as its nearest approach. Nothing is between the two by
        // construction, so any "blocked" verdict here is the test failing.
        let Some((face, _)) = bsp.nearest_face(p, 4.0) else {
            no_face += 1;
            continue;
        };
        let Some(n) = bsp.face_normal(face) else {
            no_face += 1;
            continue;
        };
        let eye = [p[0] + n[0] * 158.0, p[1] + n[1] * 158.0, p[2] + n[2] * 158.0];

        if bsp.line_blocked(&eye, p) {
            blocked_from_normal_eye += 1;
        }
        // The same trace stopped a little short of the surface.
        let off = [p[0] + n[0] * 2.0, p[1] + n[1] * 2.0, p[2] + n[2] * 2.0];
        if bsp.line_blocked(&eye, &off) {
            blocked_after_backoff += 1;
        }
    }

    // Does the trace get worse with length? An eye placed straight out along
    // the surface normal has nothing between it and the point by construction,
    // at any distance, so every "blocked" here is the test failing.
    for reach in [25.0f32, 50.0, 100.0, 200.0, 400.0, 800.0, 1600.0] {
        let mut blocked = 0usize;
        let mut n_tested = 0usize;
        for p in &coords {
            let Some((face, _)) = bsp.nearest_face(p, 4.0) else { continue };
            let Some(n) = bsp.face_normal(face) else { continue };
            let eye = [p[0] + n[0] * reach, p[1] + n[1] * reach, p[2] + n[2] * reach];
            // Only count eyes that are themselves in open space; one inside a
            // brush is a legitimately blocked view, not a failure.
            let c = bsp.leaf_contents(bsp.leaf_at(&eye));
            if c == CONTENTS_SOLID || c == CONTENTS_SKY { continue }
            n_tested += 1;
            if bsp.line_blocked(&eye, p) { blocked += 1 }
        }
        println!(
            "eye {:>5.0}u straight out, in open space: BLOCKED {:>4} of {:>4} ({:.1}%)",
            reach, blocked, n_tested, 100.0 * blocked as f32 / n_tested.max(1) as f32
        );
    }

    let tested = coords.len() - no_face;
    println!("no nearest face within 4 units: {}", no_face);
    println!("point lands in a solid/sky leaf: {} of {}", in_solid, coords.len());
    println!(
        "trace from an unobstructed eye 158u along the normal says BLOCKED: {} of {} ({:.1}%)",
        blocked_from_normal_eye,
        tested,
        100.0 * blocked_from_normal_eye as f32 / tested.max(1) as f32
    );
    println!(
        "same trace, endpoint backed 2u off the surface, says BLOCKED: {} of {} ({:.1}%)",
        blocked_after_backoff,
        tested,
        100.0 * blocked_after_backoff as f32 / tested.max(1) as f32
    );
}
