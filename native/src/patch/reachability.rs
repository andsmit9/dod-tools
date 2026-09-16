// patch/reachability.rs
// Where players have actually been, harvested from real demos, as a signal
// for which map entities are safe background scenery rather than something
// a player could ever get near (#207).
//
// Same principle as `decal_strip::proven_world_coordinates` and
// `map_fetch`'s BSP-build validation: a position the engine sent a real
// player entity to is proof, not a guess, and beats any hand-built bounding
// box or convex hull of "the playable area." More demos of the same map only
// narrow the gaps -- different rounds and spawns cover different ground.
//
// This harvests *player* origins specifically (entity indices 1..=32, not
// decal impacts), because the question here is "could a player ever be near
// this entity", not "is this surface visible."

use std::collections::HashSet;
use std::path::Path;

use dem::open_demo_from_bytes;
use dem::types::{Delta, EngineMessage, FrameData, MessageData, NetMessage};

/// Origin points are snapped to a grid this wide before being kept, so a
/// player standing still for ten seconds contributes one point instead of a
/// few hundred nearly-identical ones. Coarser than a player's own hull
/// (32 units) buys nothing for a "how far away is this" query but costs
/// memory and query time on every candidate entity.
const GRID_SIZE: f32 = 48.0;

/// Reads a component's raw delta bytes as an `f32`, matching how the wire
/// actually encodes `origin[N]` -- a plain 4-byte little-endian float, not a
/// `BitVec` needing `BitSliceCast` (that trait is for engine-message struct
/// fields; entity deltas decode straight to bytes). `#[strip trailing NUL]`:
/// `delta_description_t` field names carry it as parsed off the wire.
fn origin_component(delta: &Delta, axis: usize) -> Option<f32> {
    let key = format!("origin[{axis}]\0");
    delta.get(&key).and_then(|v| (v.len() >= 4).then(|| f32::from_le_bytes([v[0], v[1], v[2], v[3]])))
}

/// One entity's delta against the harvest's running state: updates its last-
/// known origin (if this delta touched any axis) and records the resulting
/// point, or does nothing for the world (index 0) or anything past player
/// slots (weapons, dropped items, temp entities riding high indices) --
/// this is specifically about where *players* have been. Split out from
/// `harvest_player_positions` so the actual accumulation logic is testable
/// against hand-built deltas, without needing a parseable demo on disk.
fn ingest_entity(
    entity_index: u16,
    delta: &Delta,
    last: &mut [Option<[f32; 3]>; 33],
    cells: &mut HashSet<(i32, i32, i32)>,
) {
    if entity_index == 0 || entity_index > 32 {
        return;
    }
    let slot = &mut last[entity_index as usize];
    let mut origin = slot.unwrap_or([0.0, 0.0, 0.0]);
    let mut touched = slot.is_some();
    for axis in 0..3 {
        if let Some(v) = origin_component(delta, axis) {
            origin[axis] = v;
            touched = true;
        }
    }
    if touched {
        *slot = Some(origin);
        cells.insert(quantize(origin));
    }
}

fn quantize(p: [f32; 3]) -> (i32, i32, i32) {
    (
        (p[0] / GRID_SIZE).round() as i32,
        (p[1] / GRID_SIZE).round() as i32,
        (p[2] / GRID_SIZE).round() as i32,
    )
}

/// Every place a player entity's origin was ever set across one demo,
/// deduplicated onto a coarse grid.
///
/// Reads `SvcPacketEntities`/`SvcDeltaPacketEntities` directly rather than
/// requiring a full accumulated entity table (`snapshot_inject`'s approach) --
/// a delta's `origin[N]` already names an absolute value on this wire, not an
/// offset, so no baseline replay is needed to make sense of one in isolation.
/// A gap where an axis does not change between updates just means that axis
/// is not re-sent; the last known value for the other two is still real, so
/// this tracks the last full origin per entity and only stops trusting an
/// axis if the entity itself is removed.
///
/// `want_checksum`/`want_map_name` identify the target map, checked against
/// the demo header before reading a single frame -- a directory of demos is
/// rarely all one map, and a position from an unrelated map's coordinate
/// space would silently corrupt the cloud rather than fail loudly.
///
/// Checksum is the primary check (it also catches a same-named but
/// different-build map, which name matching alone would miss). But an HLTV
/// recording's header carries `map_checksum: 0` -- confirmed directly against
/// a real one, not assumed -- rather than the map's real value, so a demo
/// reporting exactly zero falls back to matching the map name instead of
/// being rejected outright. That is weaker (it cannot catch a wrong build),
/// but it is the only signal an HLTV header actually gives, and rejecting
/// every HLTV recording wastes precisely the demos this feature cares most
/// about -- HLTV sees every player, not just whoever a POV recorder was.
pub fn harvest_player_positions(bytes: &[u8], want_checksum: u32, want_map_name: &str) -> Result<HashSet<(i32, i32, i32)>, String> {
    let demo = open_demo_from_bytes(bytes).map_err(|e| format!("parse: {e}"))?;
    if demo.header.map_checksum == 0 {
        let name = String::from_utf8_lossy(
            &demo.header.map_name.0.iter().copied().take_while(|b| *b != 0).collect::<Vec<u8>>(),
        )
        .trim()
        .to_lowercase();
        if name != want_map_name.to_lowercase() {
            return Err(format!(
                "no map checksum in this header (an HLTV recording) and map name {name:?} does not match {want_map_name:?}, skipping"
            ));
        }
    } else if demo.header.map_checksum != want_checksum {
        return Err(format!(
            "map checksum 0x{:08X} does not match 0x{want_checksum:08X} -- different map or build, skipping",
            demo.header.map_checksum
        ));
    }
    let mut cells: HashSet<(i32, i32, i32)> = HashSet::new();
    // Last known origin per player-slot entity, so a delta that only touches
    // e.g. `frame` or `health` does not have to be skipped for lacking all
    // three axes -- only a changed axis needs to overwrite one slot.
    let mut last: [Option<[f32; 3]>; 33] = [None; 33];

    for entry in demo.directory.entries.iter().skip(1) {
        for f in &entry.frames {
            let FrameData::NetworkMessage(bt) = &f.frame_data else { continue };
            let MessageData::Parsed(msgs) = &bt.1.messages else { continue };
            for m in msgs {
                let NetMessage::EngineMessage(em) = m else { continue };
                let ents: Vec<(u16, &Delta)> = match &**em {
                    EngineMessage::SvcPacketEntities(pe) => {
                        pe.entity_states.iter().map(|e| (e.entity_index, &e.delta)).collect()
                    }
                    EngineMessage::SvcDeltaPacketEntities(pe) => pe
                        .entity_states
                        .iter()
                        .filter_map(|e| e.delta.as_ref().map(|d| (e.entity_index, d)))
                        .collect(),
                    _ => continue,
                };
                for (ei, delta) in ents {
                    ingest_entity(ei, delta, &mut last, &mut cells);
                }
            }
        }
    }
    Ok(cells)
}

/// Harvests every demo under a directory (non-recursive) and merges their
/// grids, so a stronger claim -- "no player has ever been near this, across
/// every recording we have" -- is only as good as how many demos were fed in.
///
/// `want_checksum`/`want_map_name` are both required: a folder of demos is
/// rarely curated to one map, and there is no other cheap way to reject a
/// demo of a different map (or a different build of the same map, whose
/// coordinates are not guaranteed to line up) before it silently corrupts
/// the cloud. `want_map_name` only matters for an HLTV recording, whose
/// header carries no usable checksum -- see `harvest_player_positions`.
pub fn harvest_directory(dir: &Path, want_checksum: u32, want_map_name: &str) -> Result<HashSet<(i32, i32, i32)>, String> {
    let mut merged: HashSet<(i32, i32, i32)> = HashSet::new();
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("dem")) {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        match harvest_player_positions(&bytes, want_checksum, want_map_name) {
            Ok(cells) => merged.extend(cells),
            // One unusable demo (wrong map, wrong build, corrupt file) should
            // not sink the whole harvest -- report and move on.
            Err(e) => eprintln!("reachability: skipping {}: {e}", path.display()),
        }
    }
    Ok(merged)
}

/// Distance from a point to the nearest edge of an axis-aligned box,
/// 0.0 if the point is inside it.
fn point_to_aabb_distance(min: [f32; 3], max: [f32; 3], p: [f32; 3]) -> f32 {
    let mut d2 = 0.0f32;
    for a in 0..3 {
        let c = p[a].clamp(min[a], max[a]);
        d2 += (p[a] - c).powi(2);
    }
    d2.sqrt()
}

/// How far a box sits from the nearest place a player has ever proven to be.
///
/// Brute-force nearest-point-in-cloud: a harvest is at most a few thousand
/// grid cells and a map has at most a few hundred candidate entities, so this
/// is well under a second even unindexed -- not worth a spatial structure for
/// a tool run interactively, once, per map.
pub fn distance_to_nearest_player(cloud: &HashSet<(i32, i32, i32)>, min: [f32; 3], max: [f32; 3]) -> f32 {
    cloud
        .iter()
        .map(|&(x, y, z)| {
            let p = [x as f32 * GRID_SIZE, y as f32 * GRID_SIZE, z as f32 * GRID_SIZE];
            point_to_aabb_distance(min, max, p)
        })
        .fold(f32::INFINITY, f32::min)
}

/// The distance past which a box this size is read as background scenery
/// rather than something a player could plausibly reach or notice missing.
///
/// Scaled by the box's own diagonal rather than fixed: removing a large
/// building leaves a bigger, more noticeable gap in the skyline than removing
/// a small prop does, so a big box needs to sit proportionally farther out
/// before it is safe to call background -- the same raw distance that clears
/// a small prop should not automatically clear a landmark-sized one.
/// `BASE_MARGIN` covers the small-object end; `SIZE_FACTOR` is how much
/// farther a bigger box has to be, per unit of its own diagonal, before it
/// qualifies. Both are starting points, not measured constants -- there is
/// no real-match data yet on where "a viewer would notice" actually falls,
/// unlike the classname tables above.
const BASE_MARGIN: f32 = 256.0;
const SIZE_FACTOR: f32 = 0.5;

pub fn reads_as_background(distance: f32, min: [f32; 3], max: [f32; 3]) -> bool {
    let diagonal = ((max[0] - min[0]).powi(2) + (max[1] - min[1]).powi(2) + (max[2] - min[2]).powi(2)).sqrt();
    distance > BASE_MARGIN + diagonal * SIZE_FACTOR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_inside_box_is_zero_distance() {
        assert_eq!(point_to_aabb_distance([0.0, 0.0, 0.0], [10.0, 10.0, 10.0], [5.0, 5.0, 5.0]), 0.0);
    }

    #[test]
    fn point_outside_box_measures_to_the_nearest_face() {
        let d = point_to_aabb_distance([0.0, 0.0, 0.0], [10.0, 10.0, 10.0], [13.0, 4.0, 4.0]);
        assert!((d - 3.0).abs() < 1e-4, "expected 3.0, got {d}");
    }

    #[test]
    fn quantize_merges_nearby_points_into_one_cell() {
        let a = quantize([100.0, 100.0, 100.0]);
        let b = quantize([100.0 + GRID_SIZE * 0.3, 100.0, 100.0]);
        assert_eq!(a, b, "points well within one grid cell should collide");
        let c = quantize([100.0 + GRID_SIZE * 2.0, 100.0, 100.0]);
        assert_ne!(a, c, "points a full cell apart should not collide");
    }

    #[test]
    fn a_large_building_needs_more_margin_than_a_small_prop_at_the_same_distance() {
        // Same 350-unit clearance past each box's edge, very different
        // sizes: the small prop already reads as background at that
        // distance, but the building -- whose absence would be a much
        // bigger, more noticeable gap -- does not qualify yet.
        let small = ([0.0, 0.0, 0.0], [8.0, 8.0, 8.0]);
        let building = ([0.0, 0.0, 0.0], [400.0, 400.0, 200.0]);
        let small_dist = point_to_aabb_distance(small.0, small.1, [358.0, 0.0, 0.0]);
        let building_dist = point_to_aabb_distance(building.0, building.1, [750.0, 0.0, 0.0]);
        assert!((small_dist - building_dist).abs() < 1e-3, "both boxes should have the same clearance");
        assert!(reads_as_background(small_dist, small.0, small.1));
        assert!(!reads_as_background(building_dist, building.0, building.1));
    }

    fn origin_delta(x: f32, y: f32, z: f32) -> Delta {
        let mut d = Delta::new();
        for (axis, v) in [x, y, z].into_iter().enumerate() {
            d.insert(format!("origin[{axis}]\0"), v.to_le_bytes().to_vec());
        }
        d
    }

    #[test]
    fn ingest_ignores_world_and_out_of_range_indices() {
        // Entity 0 (world) and anything past 32 (weapons, dropped items,
        // temp entities riding high indices) must never contribute a point --
        // this is specifically about where *players* have been.
        let mut last = [None; 33];
        let mut cells = HashSet::new();
        let delta = origin_delta(100.0, 100.0, 100.0);
        for ei in [0u16, 33, 64] {
            ingest_entity(ei, &delta, &mut last, &mut cells);
        }
        assert!(cells.is_empty());
    }

    #[test]
    fn ingest_records_a_player_slot_origin() {
        let mut last = [None; 33];
        let mut cells = HashSet::new();
        ingest_entity(1, &origin_delta(320.0, -64.0, 16.0), &mut last, &mut cells);
        assert_eq!(cells.len(), 1);
        assert!(cells.contains(&quantize([320.0, -64.0, 16.0])));
    }

    #[test]
    fn ingest_carries_forward_axes_a_later_delta_does_not_touch() {
        // A delta that only updates e.g. `frame` still names a real, current
        // origin for that entity via the two axes it left alone -- it must
        // not be treated as "no position update" just because one delta
        // happened to touch zero of the three origin keys.
        let mut last = [None; 33];
        let mut cells = HashSet::new();
        ingest_entity(1, &origin_delta(0.0, 0.0, 0.0), &mut last, &mut cells);

        let mut moved_x_only = Delta::new();
        moved_x_only.insert("origin[0]\0".to_string(), 500.0f32.to_le_bytes().to_vec());
        ingest_entity(1, &moved_x_only, &mut last, &mut cells);

        assert!(cells.contains(&quantize([500.0, 0.0, 0.0])), "the y/z axes should carry forward as 0.0");
    }
}
