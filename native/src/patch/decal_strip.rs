// patch/decal_strip.rs
// Decal hygiene for capture demos: keeps walls clean at the start of every
// recorded clip without reloading the demo.
//
// ── Why r_decals alone cannot do this ────────────────────────────────────────
// GoldSrc stores decals in a fixed pool with a single rotating index:
//
//     limit = min(r_decals, MAX_RENDER_DECALS)
//     if (gDecalCount >= limit) gDecalCount = 0;
//     pdecal = &gDecalPool[gDecalCount++];
//     R_DecalUnlink(pdecal);          // the ONLY path that clears an old decal
//
// A decal is removed *only* when the ring index lands on its slot. The cvar
// never sweeps anything — it just bounds how far the index may travel before
// wrapping. Lowering r_decals mid-demo therefore strands every decal sitting in
// a slot >= the new limit: the index can no longer reach them, so they stay on
// the wall permanently while new decals churn through the small surviving
// window. (That asymmetry is exactly what you see in game when dropping
// r_decals from 5555 to 1.) The clean walls at demo load come from
// R_ClearDecals() on level load, not from the cvar.
//
// ── What this module does instead ────────────────────────────────────────────
// Two complementary passes over the demo's own byte stream:
//
//  1. STRIP — replace decal-creating messages outside the capture windows with
//     SvcNop, so the fast-forwarded stretches between clips contribute no
//     buildup at all. SvcNop is one byte on the wire and demo_writer recomputes
//     payload lengths, so no manual offset math is involved.
//
//  2. FLUSH BURST — pin r_decals to a modest ring size, then inject exactly
//     that many synthetic decals into the gap before each clip. That walks the
//     ring index a full revolution, unlinking every real decal still on a wall.
//     They are spread across many positions rather than stacked at one (see
//     `resolve_flush_positions` and MAX_OVERLAP_DECALS below — a stack stops
//     advancing the ring after the sixth), each ranked by how far it stays from
//     every in-clip camera, so they land where the capture never looks.
//
// Pinning the ring small is what makes the burst cheap: a sweep costs
// `ring_limit` injections regardless of how many decals are actually out there,
// so 256 keeps a full flush at ~2KB of injected messages instead of the ~36KB a
// 4096-slot ring would need.

use super::bsp;
use super::decal_atlas;
use super::cfg_scan;
use dem::open_demo_from_bytes;
use dem::types::{
    ByteString, ConsoleCommand, EngineMessage, Frame, FrameData, MessageData, NetMessage,
    SvcTempEntity, TempEntity,
};

/// Temp-entity `entity_type` values that place a persistent decal onto a wall
/// or world surface: bullet holes, grenade scorch marks, generic BSP/world
/// decals, and player spray logos.
///
/// Note TE_PLAYERDECAL (112) belongs here: it is the spray-paint logo message
/// (`impulse 201`), writing a player index plus a position and decal index to
/// stamp that player's logo onto a surface. It is wall clutter in exactly the
/// sense this pass exists to remove.
const WALL_DECAL_ENTITY_TYPES: &[u8] = &[13, 104, 109, 112, 116, 117, 118];

/// TE_PLAYERDECAL — tracked separately in the stats so sprays are visible as
/// their own number rather than lost among bullet holes.
const TE_PLAYERDECAL: u8 = 112;

/// TE_WORLDDECAL. 7 bytes: 3 × WRITE_COORD + 1 × WRITE_BYTE texture index.
/// Chosen as the flush burst's carrier message because it takes no entity index
/// and — unlike TE_GUNSHOTDECAL — plays no ricochet sound. Note this is the
/// message type, independent of which texture index it is asked to draw.
const TE_WORLDDECAL: u8 = 116;

/// TE_GUNSHOTDECAL — the bullet-hole message. Not emitted (it plays a ricochet
/// sound), but its texture index is the one worth borrowing: a small hole
/// rather than the large scorch a TE_WORLDDECAL index usually denotes.
const TE_GUNSHOTDECAL: u8 = 109;

/// Distance from a standing player's origin down to the floor: the origin sits
/// at the centre of a 72-unit hull, so the feet are 36 below it.
const ORIGIN_TO_FLOOR: f32 = 36.0;

/// The engine's own `MAX_OVERLAP_DECALS`. `R_DecalCreate` counts how many
/// existing decals a new one would overlap, and once that reaches this many it
/// recycles one of them instead of allocating:
///
/// ```c
/// pold = R_DecalIntersect( decalinfo, surf, &count );
/// if( count < MAX_OVERLAP_DECALS ) pold = NULL;
/// ```
///
/// `R_DecalAlloc` only walks the ring when handed NULL, so a recycled decal
/// does NOT advance `gDecalCount`. This is why a flush burst must be spread
/// across distinct positions: piling every decal on one spot stops advancing
/// the ring after the sixth, and the sweep silently accomplishes nothing.
pub const MAX_OVERLAP_DECALS: usize = 6;

/// Flush decals to place at each distinct position. Kept below
/// `MAX_OVERLAP_DECALS` so every one of them allocates a fresh ring slot.
pub const DECALS_PER_POSITION: usize = MAX_OVERLAP_DECALS - 2;

/// Minimum spacing between two flush positions, so the engine cannot see them
/// as overlapping.
///
/// `m_Size` for the small bullet hole was later measured at ~4 units, and that
/// is the decal's own radius — two overlap only within ~8 units of each other.
/// This was 28 when the footprint was a guess; it now sits at 1.5x the measured
/// overlap distance, which is what lets a tiled grid at `TILE_PITCH` survive the
/// spacing filter instead of being decimated by it.
const MIN_POSITION_SPACING: f32 = 12.0;

#[derive(Debug, Clone)]
pub struct DecalCleanOptions {
    /// Blank decal messages outside the capture windows.
    pub strip_outside_windows: bool,
    /// Inject ring-sweeping decal bursts ahead of each capture window.
    pub flush_burst: bool,
    /// Value r_decals is pinned to. The burst size follows from this: a full
    /// ring revolution is what guarantees every occupied slot gets unlinked.
    /// Must NOT be changed anywhere else in the demo — lowering it later
    /// strands decals in the slots above the new limit.
    pub ring_limit: u32,
    /// Extra injections beyond `ring_limit`. A decal spanning several surfaces
    /// consumes one pool slot per surface, so the sweep is deliberately
    /// over-provisioned rather than counted 1:1 against messages.
    pub burst_margin: usize,
    /// Cap on synthetic decals added to any single network packet, so injection
    /// never meaningfully grows a frame the engine already sized.
    pub max_per_frame: usize,
    /// Finish the burst this long, in **seconds of demo time**, before the
    /// capture window opens.
    ///
    /// Seconds, not frames, for two reasons. A frame count is not a duration —
    /// frame records are not evenly spaced, and there are ~4.4 of them per
    /// rendered frame, so the flat `300` this used to be was worth a median of
    /// 0.6s across a real library rather than the ~3s it read as. And the
    /// margin has a job that is measured in time: the engine has to actually
    /// ingest the burst — 272 decals at a 256 ring, 4,112 at the maximum — and
    /// finish turning the ring before the first recorded frame.
    ///
    /// Resolved by walking the frames' own timestamps, per the "Pure Float
    /// Timestamps" rule in `docs/app_architecture.md`. Never `seconds * fps`.
    pub lead_seconds: f32,
    /// Emit `r_decals <ring_limit>` as a console-command frame at playback
    /// start, making a patched demo self-contained for testing.
    pub inject_r_decals_command: bool,
    /// Hand-picked flush coordinate, overriding spawn detection.
    pub flush_coord: Option<[f32; 3]>,
    /// Hand-picked decal texture index for the flush burst, overriding the
    /// harvested one. Useful when a demo contains no bullet-hole decal to
    /// borrow a small texture from and would otherwise fall back to a large
    /// grenade scorch.
    pub flush_texture_index: Option<u8>,
    /// Vertical drop applied to the settled spawn origin to reach the floor.
    /// Only used when the demo yielded no real decal to anchor to.
    pub floor_drop: f32,
    /// Consecutive on-ground frames with a stable Z required before a spawn
    /// position is trusted, so a player still falling from an elevated spawn
    /// point is never sampled mid-air.
    pub grounded_settle_frames: usize,
    /// Clearance from every in-window camera position that flush positions are
    /// *preferred* to have. No longer a hard filter: positions are ranked by
    /// clearance and the best are taken, so a demo whose every surface passes
    /// closer than this still gets a full sweep rather than nothing. Falling
    /// short of it is reported.
    ///
    /// Keeping decals off screen is the cone test's job (`visibility_cone_
    /// degrees`); clearance is the margin against that test's own blind spot,
    /// which is that cameras are sampled every fourth frame and a fast turn
    /// between two samples is not seen. It still gates the last-resort
    /// single-position fallback, where there is no spread to rank.
    pub min_camera_clearance: f32,
    /// Half-angle of the cone treated as "on screen" for the line-of-sight
    /// test.
    ///
    /// The pipeline derives this from the capture FOV and frame shape — see
    /// `on_screen_half_angle`. The default here is deliberately wide rather
    /// than accurate, because a caller that does not know its own FOV is
    /// better served by rejecting too many spots than by putting one in shot.
    pub visibility_cone_degrees: f32,
    /// Directory holding the game's `.bsp` files. With one the flush can tell
    /// a wall from a sightline; without it the frame cone is all there is.
    pub maps_dir: Option<std::path::PathBuf>,
    /// Where this run's proven world coordinates are pooled per map, and read
    /// back from. `None` keeps the pass self-contained — it uses only what this
    /// demo proves, which is what the CLI and the probe rig want.
    ///
    /// Writing happens here and nowhere else.
    pub atlas_dir: Option<std::path::PathBuf>,
    /// Additional read-only coordinate stores, unioned in at load and never
    /// written to. Intended for a store shipped with the app and refreshed by
    /// the updater, kept separate so an update can replace it wholesale without
    /// touching what the user's own captures have harvested.
    pub atlas_seed_dirs: Vec<std::path::PathBuf>,
    /// Range past which the on-screen test stops caring. `INFINITY` by default,
    /// i.e. it always cares.
    ///
    /// This was 1800 units, on the reasoning that a single decal that far away
    /// is not readable on screen. The flush does not place single decals: it
    /// places a grid of them, four to a position, and a grid reads at distances
    /// one dot does not. Anything past the cutoff was not merely untested — it
    /// was scored SAFE, because the test returned "not on screen" for it.
    /// Removing the escape cost no candidates on the demo that exposed the
    /// projection bug (a full 1028-position sweep either way) and about six
    /// seconds of tracing, which is not a trade worth making for the one defect
    /// this pass must never introduce.
    pub visibility_max_distance: f32,
    /// Keep every camera sample rather than the usual stride, and hand back
    /// the chosen positions with them. Diagnostics only — off in the pipeline.
    pub collect_diagnostics: bool,
    /// Place the sweep ONLY where leaf visibility says the engine never renders
    /// it — the drawn surface sitting outside the PVS union of every camera
    /// leaf, not merely occluded from them.
    ///
    /// An experiment, not a feature. It answers the one question BSP-derived
    /// coordinates depend on: does a decal on a face the player never renders
    /// still allocate a ring slot? Allocation is what turns the ring; drawing
    /// is irrelevant to the sweep. If it does, a map can be seeded from its
    /// own geometry and the coordinate store stops being a cold-start
    /// dependency. If it does not, a sweep placed in unvisited geometry turns
    /// fewer slots than it reports — silently, which is this feature's
    /// signature failure.
    ///
    /// Tests the DRAWN surface, not the coordinate. A coordinate inside solid
    /// lands in a leaf no PVS row contains, so asking about the coordinate
    /// would call every buried candidate "never rendered" for the same reason
    /// the old camera test called them hidden.
    pub require_pvs_hidden: bool,
    /// Experiment gate: place the sweep from the map's own faces ALONE, with
    /// every source drawn from the match skipped.
    ///
    /// The map source is normally a fallback, reached only when a demo's own
    /// decals and the coordinate store between them cannot fill a sweep — so on
    /// a mature library it never runs, and the one thing that can judge where it
    /// puts decals is the game. This forces it, which is how a source meant for
    /// maps nobody has captured yet gets tested on a map somebody can watch.
    pub map_geometry_only: bool,
}

impl Default for DecalCleanOptions {
    fn default() -> Self {
        Self {
            strip_outside_windows: true,
            flush_burst: true,
            ring_limit: 256,
            burst_margin: 16,
            max_per_frame: 4,
            lead_seconds: DEFAULT_LEAD_SECONDS,
            inject_r_decals_command: true,
            flush_coord: None,
            flush_texture_index: None,
            floor_drop: ORIGIN_TO_FLOOR,
            grounded_settle_frames: 10,
            maps_dir: None,
            atlas_dir: None,
            atlas_seed_dirs: Vec::new(),
            min_camera_clearance: 900.0,
            visibility_cone_degrees: 40.0,
            visibility_max_distance: f32::INFINITY,
            collect_diagnostics: false,
            require_pvs_hidden: false,
            map_geometry_only: false,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct DecalCleanStats {
    pub temp_entity_stripped: usize,
    pub player_spray_stripped: usize,
    pub flush_decals_injected: usize,
    pub bursts_placed: usize,
    /// Windows whose gap had too little room to fit a full sweep. These clips
    /// are NOT guaranteed clean, so they are reported rather than silently
    /// under-flushed.
    pub bursts_short: Vec<(i32, usize, usize)>,
    /// Burst carrier frames that fall *inside* a clip being recorded.
    ///
    /// The sweep is supposed to turn the decal ring in the gap before a clip.
    /// A carrier landing inside an earlier clip's own record window turns the
    /// ring while that clip is being filmed, which can evict the bullet holes
    /// the firefight is putting up — decals vanishing mid-shot, the exact
    /// artefact this feature exists to remove.
    pub burst_frames_inside_clip: usize,
    pub flush_coord: Option<[f32; 3]>,
    pub flush_source: Option<FlushSource>,
    pub flush_texture_index: Option<u8>,
    /// Settled on-ground spawn origin, when one was found.
    pub spawn_reference: Option<[f32; 3]>,
    /// How far the flush coordinate ended up from that spawn reference.
    pub spawn_to_flush_distance: Option<f32>,
    pub harvested_decals: usize,
    /// Closest approach between the flush coordinate and any camera position
    /// inside a capture window.
    pub min_camera_distance: Option<f32>,
    /// Sampled in-window camera frames where the flush point falls inside the
    /// camera's cone. Must be 0 — anything else means the flush stack is on
    /// screen during a recorded clip.
    pub flush_on_camera_frames: usize,
    /// Total in-window camera samples the two figures above were measured over.
    pub camera_samples: usize,
    /// Distinct positions the burst was spread across.
    pub flush_positions: usize,
    /// Positions needed to place the whole burst without any spot exceeding
    /// the engine's overlap limit. If `flush_positions` is below this, some
    /// injected decals get recycled instead of turning the ring.
    pub flush_positions_wanted: usize,
    /// Tiles laid across the fitted planes, before camera filtering.
    pub tiled_candidates: usize,
    /// What the map's coordinate store held, gained and offers after this demo.
    pub atlas: crate::patch::decal_atlas::AtlasStats,
    /// Which map build the store was keyed on, when one was resolved.
    pub atlas_map: Option<String>,
    /// What the on-screen test was actually able to consult.
    pub visibility_basis: VisibilityBasis,
    /// World faces in the map, when one was loaded.
    pub map_faces: usize,
    /// Whether that map carries visibility data.
    pub map_has_vis: bool,
    /// Of the chosen positions, how many leaf visibility ALSO calls hidden.
    /// `None` when the map has no vis data.
    ///
    /// Placement is decided by line-of-sight traces alone. This is the second
    /// opinion, recorded rather than acted on: the two were caught disagreeing,
    /// and which is right is one of the things the in-game check settles.
    pub pvs_agrees_hidden: Option<usize>,
    /// How many of those were clear of every in-clip camera. A shortfall with
    /// these two close together means the demo offers little surface; a
    /// shortfall with a wide gap means the surface it has is all in shot.
    pub tiled_camera_safe: usize,
    /// Candidates sampled off the map's own world faces, and how many of those
    /// cleared every in-clip camera. Both stay 0 unless the proven sources fell
    /// short and the map source was actually reached — it is not sampled
    /// speculatively, because on a map with a populated store it is pure cost.
    pub map_candidates: usize,
    pub map_camera_safe: usize,
    /// The chosen positions and the in-clip camera samples they were judged
    /// against, populated only when `collect_diagnostics` is set.
    pub diagnostic_positions: Vec<[f32; 3]>,
    pub diagnostic_cameras: Vec<([f32; 3], [f32; 3])>,
}

fn decode_coord(b: &[u8]) -> f32 {
    i16::from_le_bytes([b[0], b[1]]) as f32 / 8.0
}

fn encode_coord(v: f32) -> [u8; 2] {
    ((v * 8.0).round() as i16).to_le_bytes()
}

/// Texture index carried by a decal temp entity, by wire layout.
/// TE_WORLDDECAL/HIGH: coord(6) + index. TE_GUNSHOTDECAL/TE_DECALHIGH:
/// coord(6) + entity(2) + index. TE_DECAL: coord(6) + index + entity(2).
pub(super) fn decal_texture_index(entity_type: u8, payload: &[u8]) -> Option<u8> {
    match entity_type {
        116 | 117 if payload.len() >= 7 => Some(payload[6]),
        104 if payload.len() >= 7 => Some(payload[6]),
        109 | 118 if payload.len() >= 9 => Some(payload[8]),
        _ => None,
    }
}

/// Whether a decal was stamped onto world geometry, and therefore whether its
/// coordinate stays true outside the demo that produced it.
///
/// This only matters for `decal_atlas`. Within one demo a mark on a door is a
/// serviceable flush position, because the door is wherever the demo last left
/// it. In a store that outlives the demo it is a coordinate that will one day
/// point at the air a door used to occupy, and a flush position that misses
/// allocates no ring slot.
///
/// Layouts are the ones documented on `decal_texture_index`. Anything not
/// listed stays out of the atlas: it still serves this demo, it simply never
/// becomes a durable claim about the map.
pub(super) fn is_world_decal(entity_type: u8, payload: &[u8]) -> bool {
    let entity_at = |i: usize| -> Option<u16> {
        payload
            .get(i..i + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    match entity_type {
        // TE_WORLDDECAL / HIGH carry no entity field at all: world by
        // construction, which is also why the flush emits this type.
        116 | 117 => true,
        // coord(6) + entity(2) + index(1)
        109 | 118 => entity_at(6) == Some(0),
        // coord(6) + index(1) + entity(2)
        104 => entity_at(7) == Some(0),
        // TE_BSPDECAL: coord(6) + 16-bit texture index(2) + entity(2)
        13 => entity_at(8) == Some(0),
        _ => false,
    }
}

fn decal_position(payload: &[u8]) -> Option<[f32; 3]> {
    if payload.len() < 6 {
        return None;
    }
    Some([
        decode_coord(&payload[0..2]),
        decode_coord(&payload[2..4]),
        decode_coord(&payload[4..6]),
    ])
}

pub(super) fn distance(a: &[f32; 3], b: &[f32; 3]) -> f32 {
    let (dx, dy, dz) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
    (dx * dx + dy * dy + dz * dz).sqrt()
}

// ── Plane geometry ───────────────────────────────────────────────────────────
// Shared with `decal_probe`, which measures on the same fitted planes this
// tiles across. Kept here because the dependency runs probe -> strip.

/// Groups values into runs no wider than `tolerance`, returning each run's mean
/// and its members' indices.
///
/// A sweep rather than bucket-rounding: rounding puts two values a hair apart
/// into different buckets whenever they straddle a boundary, which would split
/// one surface into two undersized patches and lose it to a minimum-size check.
pub(super) fn cluster(values: &[f32], tolerance: f32) -> Vec<(f32, Vec<usize>)> {
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|&a, &b| {
        values[a]
            .partial_cmp(&values[b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut out: Vec<(f32, Vec<usize>)> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut anchor = f32::NAN;

    for idx in order {
        let v = values[idx];
        if current.is_empty() {
            anchor = v;
            current.push(idx);
        } else if (v - anchor).abs() <= tolerance {
            current.push(idx);
        } else {
            let mean = current.iter().map(|&i| values[i]).sum::<f32>() / current.len() as f32;
            out.push((mean, std::mem::take(&mut current)));
            anchor = v;
            current.push(idx);
        }
    }
    if !current.is_empty() {
        let mean = current.iter().map(|&i| values[i]).sum::<f32>() / current.len() as f32;
        out.push((mean, current));
    }
    out
}

/// The two axes that lie in a plane whose normal runs along `axis`.
pub(super) fn tangent_axes(axis: usize) -> (usize, usize) {
    match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    }
}

pub(super) fn extent(members: &[[f32; 3]], ax: usize) -> f32 {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for m in members {
        lo = lo.min(m[ax]);
        hi = hi.max(m[ax]);
    }
    if lo.is_finite() { hi - lo } else { 0.0 }
}

/// Splits a coplanar set into spatially connected patches.
///
/// Coplanar is not contiguous. Every floor in a map that happens to sit at the
/// same height lands in one Z cluster — the first run of this picked exactly
/// that: a "plane" whose decals spanned 2173 x 5273 units across the whole map.
/// A grid centred anywhere in it would have had columns hanging in mid-air over
/// a different room. Linking members that sit within `radius` of each other is
/// what makes "there is surface between these two decals" a defensible claim.
pub(super) fn connected_patches(members: &[[f32; 3]], radius: f32) -> Vec<Vec<[f32; 3]>> {
    let n = members.len();
    let mut seen = vec![false; n];
    let mut out = Vec::new();

    for start in 0..n {
        if seen[start] {
            continue;
        }
        seen[start] = true;
        let mut stack = vec![start];
        let mut patch = Vec::new();
        while let Some(i) = stack.pop() {
            patch.push(members[i]);
            for j in 0..n {
                if !seen[j] && distance(&members[i], &members[j]) <= radius {
                    seen[j] = true;
                    stack.push(j);
                }
            }
        }
        out.push(patch);
    }
    out
}

// ── Tiling ───────────────────────────────────────────────────────────────────

/// Coplanarity tolerance when grouping decals into a candidate surface. Matches
/// the probe's: a decal sits on the plane, not near it, so this only has to
/// absorb coordinate quantisation.
const PLANE_TOLERANCE: f32 = 2.0;

/// How close two decals must be to count as evidence of the same continuous
/// patch of surface. See `connected_patches` for why coplanar alone is not
/// enough.
const PATCH_LINK_RADIUS: f32 = 160.0;

/// Decals needed before a patch is believed to be a real surface rather than a
/// coincidence of two stray marks.
const MIN_PATCH_DECALS: usize = 4;

/// Spacing between tiled positions.
///
/// `m_Size` for the small bullet hole was measured at ~4 units, and that is the
/// decal's own radius — two of them overlap only if their centres come within
/// ~8 units. A 16-unit pitch is twice that, so no tile can be recycled as an
/// overlap of its neighbour, which is the failure that stops the ring advancing.
const TILE_PITCH: f32 = 16.0;

/// Cap on how far a tiled grid may spread from its patch centre along either
/// in-plane axis.
///
/// Movement along a plane is unconstrained as far as the engine is concerned —
/// synthesised positions 224 units apart all created decals, two of them with
/// no real decal within 30 units. The cap is not an engine limit but a
/// confidence one: the further a tile sits from the decals proving the surface,
/// the more it is inference. ~200 units keeps a grid inside the room its
/// evidence came from.
const TILE_MAX_EXTENT: f32 = 200.0;

/// How close a tile must come to a real decal on its own patch to be kept.
///
/// A tile that lands past the end of a wall hits nothing, and a position that
/// creates no decal allocates no pool slot — so the sweep silently comes up
/// short rather than failing. Dilating the proven decals by this much is the
/// compromise between that risk and the position count the ring needs.
/// How far from a coordinate to look for the surface the engine will draw on.
///
/// Deliberately generous. This is not a claim about where `R_DecalShoot` will
/// place a decal — it is the radius within which a face has to be CLEARED as
/// not-in-shot before the candidate is trusted. Erring long costs candidates;
/// erring short is how a decal lands on a face nobody tested. The fine sweep
/// measured the engine accepting positions ~3 units off a face, so this is
/// that with a little margin.
///
/// It does double duty. A candidate with no face inside it is not a decal spot
/// at all — tiling lays a grid across a fitted plane, and a plane runs on past
/// the brush that proved it, so some tiles land in open air where no decal can
/// appear. Those are dropped here rather than counted into a sweep that then
/// turns fewer ring slots than it reports.
const DECAL_PROJECTION_REACH: f32 = 4.0;

/// How far off the face the drawn point is placed for testing. The decal is
/// visible from this side, and a trace ending here is not stopped by the face
/// it sits on — comfortably clear of the tree's own 0.03125 plane epsilon.
const DECAL_SURFACE_LIFT: f32 = 1.0;

const TILE_REACH: f32 = 64.0;

/// Ceiling on tiles generated per patch.
///
/// `TILE_MAX_EXTENT` and `TILE_PITCH` already bound a grid at 13x13, so at the
/// current values this cannot trigger. It exists so that widening the extent or
/// tightening the pitch cannot quietly turn one densely-shot wall into tens of
/// thousands of candidates for the camera filters to score.
const MAX_TILES_PER_PATCH: usize = 512;

/// Positions tiled across the planes the demo's own decals prove exist.
///
/// The flush needs one distinct position per few ring slots, and harvesting
/// them one-per-real-decal never yielded enough — a 256-slot ring wants 68
/// positions and a busy demo offered 30. Tiling is what closes that: the engine
/// does not care how far a decal sits from another along a surface, only that
/// there IS surface, so one patch of proven wall can carry a whole grid.
///
/// Returned in no particular order; the caller ranks them by camera clearance
/// and enforces spacing across the whole pool.
pub(super) fn tile_positions(harvested: &[[f32; 3]]) -> Vec<[f32; 3]> {
    let mut out = Vec::new();

    for axis in 0..3 {
        let (t1, t2) = tangent_axes(axis);
        let values: Vec<f32> = harvested.iter().map(|p| p[axis]).collect();

        for (value, idxs) in cluster(&values, PLANE_TOLERANCE) {
            if idxs.len() < MIN_PATCH_DECALS {
                continue;
            }
            let coplanar: Vec<[f32; 3]> = idxs.iter().map(|&i| harvested[i]).collect();

            for patch in connected_patches(&coplanar, PATCH_LINK_RADIUS) {
                if patch.len() < MIN_PATCH_DECALS {
                    continue;
                }
                tile_patch(&patch, axis, value, t1, t2, &mut out);
            }
        }
    }

    out
}

/// Lays a grid over one patch, keeping only the tiles its decals vouch for.
fn tile_patch(
    patch: &[[f32; 3]],
    axis: usize,
    value: f32,
    t1: usize,
    t2: usize,
    out: &mut Vec<[f32; 3]>,
) {
    // Centred on the patch's own centre of mass rather than its bounding box,
    // so the extent cap is spent where the evidence actually is. A wall with
    // one stray mark 400 units down its length would otherwise drag the grid
    // half way to nothing.
    let centre = |ax: usize| patch.iter().map(|m| m[ax]).sum::<f32>() / patch.len() as f32;
    let half = TILE_MAX_EXTENT / 2.0;

    let span = |ax: usize| -> (f32, f32) {
        let c = centre(ax);
        let lo = patch.iter().fold(f32::INFINITY, |a, m| a.min(m[ax]));
        let hi = patch.iter().fold(f32::NEG_INFINITY, |a, m| a.max(m[ax]));
        (lo.max(c - half), hi.min(c + half))
    };

    let (lo1, hi1) = span(t1);
    let (lo2, hi2) = span(t2);

    let steps = |lo: f32, hi: f32| -> usize { ((hi - lo) / TILE_PITCH).floor() as usize + 1 };
    let (n1, n2) = (steps(lo1, hi1), steps(lo2, hi2));

    let mut placed = 0usize;
    for i in 0..n1 {
        for j in 0..n2 {
            if placed >= MAX_TILES_PER_PATCH {
                return;
            }
            let mut p = [0.0f32; 3];
            // Straight onto the fitted plane. The fine sweep put that plane
            // ~0.5 units proud of the true BSP one, well inside the ~3 units of
            // slack `R_DecalShoot`'s walk allows, so no offset is applied —
            // guessing at one is how a whole sweep lands in mid-air.
            p[axis] = value;
            p[t1] = lo1 + i as f32 * TILE_PITCH;
            p[t2] = lo2 + j as f32 * TILE_PITCH;

            if patch.iter().any(|m| distance(m, &p) <= TILE_REACH) {
                out.push(p);
                placed += 1;
            }
        }
    }
}

pub(super) fn build_world_decal(pos: &[f32; 3], texture_index: u8) -> NetMessage {
    let mut payload = Vec::with_capacity(7);
    payload.extend_from_slice(&encode_coord(pos[0]));
    payload.extend_from_slice(&encode_coord(pos[1]));
    payload.extend_from_slice(&encode_coord(pos[2]));
    payload.push(texture_index);

    NetMessage::EngineMessage(Box::new(EngineMessage::SvcTempEntity(SvcTempEntity {
        entity_type: TE_WORLDDECAL,
        entity: TempEntity::TeWorldDecal(payload),
    })))
}

/// Everything the survey pass needs to pick a flush coordinate and place bursts.
pub(super) struct Survey {
    /// Positions of decals the engine actually accepted during playback.
    pub(super) harvested: Vec<[f32; 3]>,
    /// The subset of those stamped on world geometry rather than on a brush
    /// entity. Only these are durable enough to contribute to `decal_atlas` —
    /// see `is_world_decal`.
    pub(super) world_harvested: Vec<[f32; 3]>,
    pub(super) texture_index: Option<u8>,
    /// Earliest camera eye position seen in playback.
    pub(super) spawn_eye: Option<[f32; 3]>,
    /// Player origin once the spawn has settled onto solid ground — see the
    /// grounded-run detection below. This, not `spawn_eye`, is the spawn
    /// reference worth trusting.
    pub(super) grounded_origin: Option<[f32; 3]>,
    /// Floor points sampled beneath the player wherever they stood on solid
    /// ground. A sweep needs far more distinct positions than a demo has
    /// decals, and every one of these is a surface the demo proves exists —
    /// the player was standing on it. Walking naturally spreads them out, so
    /// they satisfy the no-overlap requirement for free.
    pub(super) floor_candidates: Vec<[f32; 3]>,
    /// Camera (eye position, forward vector) pairs sampled inside the capture
    /// windows. The forward vector is what makes a real "is it on screen?"
    /// test possible, rather than distance alone.
    pub(super) window_cameras: Vec<([f32; 3], [f32; 3])>,
}

/// Running frame ordinal, matching `engine.rs`'s `frame_counter`: every frame
/// record in file order, across all directory entries, 1-based.
///
/// This — NOT `Frame::frame` — is the tick space the rest of the patch pipeline
/// schedules in. `Frame::frame` is the engine's tick, and several frame records
/// share one of those (a DemoBuffer, a ClientData and a NetworkMessage per
/// tick), so the two spaces differ by roughly 2.6x on a real demo. Mixing them
/// silently targets the wrong frames.
pub(super) fn frame_ordinals(demo: &dem::types::Demo) -> Vec<(usize, usize, i32)> {
    let mut out = Vec::new();
    let mut ordinal = 0i32;
    for (entry_idx, entry) in demo.directory.entries.iter().enumerate() {
        for frame_idx in 0..entry.frames.len() {
            ordinal += 1;
            out.push((entry_idx, frame_idx, ordinal));
        }
    }
    out
}

pub(super) fn in_window(ordinal: i32, keep_windows: &[(i32, i32)]) -> bool {
    keep_windows.iter().any(|&(s, e)| ordinal >= s && ordinal <= e)
}

/// How long before a clip the sweep should finish, in seconds of demo time.
///
/// Two seconds rather than the ~0.6 the old flat frame count worked out to. It
/// is comfortably more margin than the value that passed in game, comfortably
/// inside a default pre-roll so the burst cannot reach back into an earlier
/// clip, and it gives the engine room to ingest a maximum sweep's 4,112 decals
/// before the first recorded frame rather than finishing just in time.
pub const DEFAULT_LEAD_SECONDS: f32 = 2.0;

/// When the timestamps cannot answer, fall back to the frame count this used to
/// be — the same shape of fallback `builder::find_tick_backwards` uses when
/// `frame_times` is truncated. Never worse than the behaviour that shipped.
const FALLBACK_LEAD_FRAMES: i32 = 300;

/// The frame ordinal that sits `lead_seconds` of demo time before `window_start`.
///
/// Walks the frames' own timestamps rather than dividing by an average rate.
/// Confined to the directory entry the window starts in, because times restart
/// per entry — a LOADING entry's near-zero timestamps would otherwise satisfy
/// any target and drag the deadline to the front of the demo.
///
/// `None` when the entry's timestamps cannot answer, leaving the caller to fall
/// back to a frame count.
fn deadline_before(
    window_start: i32,
    lead_seconds: f32,
    frames: &[(usize, usize, i32)],
    times: &[f32],
) -> Option<i32> {
    if lead_seconds <= 0.0 {
        return Some(window_start);
    }
    let anchor_slot = frames.iter().position(|&(_, _, ord)| ord == window_start)?;
    let (anchor_entry, _, _) = frames[anchor_slot];
    let anchor_time = *times.get(anchor_slot)?;
    if !anchor_time.is_finite() {
        return None;
    }
    let target = anchor_time - lead_seconds;

    // Walk back to the last frame at or before the target, in this entry only.
    let mut slot = anchor_slot;
    while slot > 0 {
        slot -= 1;
        let (entry, _, ord) = frames[slot];
        if entry != anchor_entry {
            return None;
        }
        if times[slot] <= target {
            return Some(ord);
        }
    }
    None
}

pub(super) fn survey(
    demo: &dem::types::Demo,
    keep_windows: &[(i32, i32)],
    opts: &DecalCleanOptions,
) -> Survey {
    let mut out = Survey {
        harvested: Vec::new(),
        world_harvested: Vec::new(),
        texture_index: None,
        spawn_eye: None,
        grounded_origin: None,
        floor_candidates: Vec::new(),
        window_cameras: Vec::new(),
    };
    // A world decal's index is read straight from the byte after the coords,
    // so prefer harvesting from the same message type we intend to emit.
    let mut fallback_index: Option<u8> = None;

    // Spawn points are not guaranteed to sit flush on the floor — many maps
    // place them slightly above it and let the player drop. Sampling at the
    // spawn instant can therefore return a mid-air position, whose "floor"
    // would be open space. So wait for a run of consecutive frames that report
    // on_ground with a stable Z before trusting the position.
    let mut grounded_run = 0usize;
    let mut last_z: Option<f32> = None;
    let mut camera_stride = 0usize;

    let mut ordinal = 0i32;
    for entry in &demo.directory.entries {
        for frame in &entry.frames {
            ordinal += 1;
            let FrameData::NetworkMessage(net_msg_box) = &frame.frame_data else {
                continue;
            };

            let rp = &net_msg_box.1.info.refparams;
            let origin = &rp.view_origin;
            if origin.len() >= 3 {
                let pos = [origin[0], origin[1], origin[2]];
                if pos != [0.0, 0.0, 0.0] {
                    if out.spawn_eye.is_none() {
                        out.spawn_eye = Some(pos);
                    }

                    // Floor beneath the player wherever they are actually
                    // standing. Sampled sparsely and only when far enough from
                    // the last sample to be a genuinely separate spot.
                    if rp.on_ground != 0 {
                        let sim = &rp.sim_org;
                        let origin = if sim.len() >= 3 && sim[2] != 0.0 {
                            [sim[0], sim[1], sim[2]]
                        } else {
                            let vh = rp.view_height.get(2).copied().unwrap_or(28.0);
                            [pos[0], pos[1], pos[2] - vh]
                        };
                        let floor = [origin[0], origin[1], origin[2] - opts.floor_drop];
                        let far_enough = out
                            .floor_candidates
                            .last()
                            .map(|last| distance(&floor, last) >= MIN_POSITION_SPACING)
                            .unwrap_or(true);
                        if far_enough {
                            out.floor_candidates.push(floor);
                        }
                    }

                    if out.grounded_origin.is_none() {
                        let settled = last_z.map(|z| (pos[2] - z).abs() < 2.0).unwrap_or(false);
                        if rp.on_ground != 0 && settled {
                            grounded_run += 1;
                            if grounded_run >= opts.grounded_settle_frames {
                                // Prefer the engine's own player origin; fall
                                // back to backing the view offset out of the
                                // eye position when sim_org isn't populated.
                                let sim = &rp.sim_org;
                                let resolved = if sim.len() >= 3 && sim[2] != 0.0 {
                                    [sim[0], sim[1], sim[2]]
                                } else {
                                    let view_z = rp.view_height.get(2).copied().unwrap_or(28.0);
                                    [pos[0], pos[1], pos[2] - view_z]
                                };
                                out.grounded_origin = Some(resolved);
                            }
                        } else {
                            grounded_run = 0;
                        }
                        last_z = Some(pos[2]);
                    }

                    // Subsampled: every candidate decal is scored against this
                    // whole set, and consecutive frames sit a few units apart,
                    // so a stride costs no meaningful accuracy.
                    if in_window(ordinal, keep_windows) {
                        camera_stride += 1;
                        if opts.collect_diagnostics || camera_stride.is_multiple_of(4) {
                            let fwd = &rp.forward;
                            if fwd.len() >= 3 {
                                out.window_cameras.push((pos, [fwd[0], fwd[1], fwd[2]]));
                            }
                        }
                    }
                }
            }

            let MessageData::Parsed(messages) = &net_msg_box.1.messages else {
                continue;
            };
            for msg in messages {
                let NetMessage::EngineMessage(eng) = msg else {
                    continue;
                };
                let EngineMessage::SvcTempEntity(te) = eng.as_ref() else {
                    continue;
                };
                // TE_BSPDECAL carries its fields in a different shape to the
                // rest (a 16-bit texture index rather than 8-bit, ahead of the
                // entity index), so it is unpacked separately instead of being
                // forced through the shared byte-offset table. It was already
                // in the strip set; leaving it out of the harvest set meant a
                // demo whose only decals were BSP decals offered no anchor.
                if let TempEntity::TeBspDecal(d) = &te.entity {
                    if let Some(pos) = decal_position(&d.unknown1) {
                        out.harvested.push(pos);
                        if is_world_decal(te.entity_type, &d.unknown1) {
                            out.world_harvested.push(pos);
                        }
                    }
                    if d.unknown1.len() >= 8 {
                        let raw = i16::from_le_bytes([d.unknown1[6], d.unknown1[7]]);
                        // The emitted TE_WORLDDECAL writes this index as one
                        // byte, so an index that doesn't fit is unusable.
                        if (0..=255).contains(&raw) {
                            fallback_index.get_or_insert(raw as u8);
                        }
                    }
                    continue;
                }

                // A spray marks a proven surface just as well as a bullet hole,
                // so its position is worth harvesting — but its texture index
                // never is: that index is somebody's logo, and a stack of those
                // is the most conspicuous thing that could be left on a wall.
                // Its layout also differs (a leading player index before the
                // coordinates), hence the separate arm.
                if let TempEntity::TePlayerDecal(p) = &te.entity {
                    if p.len() >= 7
                        && let Some(pos) = decal_position(&p[1..7]) {
                            out.harvested.push(pos);
                        }
                    continue;
                }

                let payload: &[u8] = match &te.entity {
                    TempEntity::TeWorldDecal(p)
                    | TempEntity::TeWorldDecalHigh(p)
                    | TempEntity::TeGunshotDecal(p)
                    | TempEntity::TeDecal(p)
                    | TempEntity::TeDecalHigh(p) => p,
                    _ => continue,
                };
                if let Some(pos) = decal_position(payload) {
                    out.harvested.push(pos);
                    if is_world_decal(te.entity_type, payload) {
                        out.world_harvested.push(pos);
                    }
                }
                if let Some(idx) = decal_texture_index(te.entity_type, payload) {
                    // Prefer a bullet-hole texture. TE_GUNSHOTDECAL indices are
                    // small and unremarkable; TE_WORLDDECAL ones are typically
                    // grenade scorches — large, dark and immediately obvious.
                    // Flush decals exist to be unnoticed, so the small mark
                    // wins and the scorch is only a fallback.
                    if te.entity_type == TE_GUNSHOTDECAL {
                        out.texture_index.get_or_insert(idx);
                    } else {
                        fallback_index.get_or_insert(idx);
                    }
                }
            }
        }
    }

    if out.texture_index.is_none() {
        out.texture_index = fallback_index;
    }
    out
}

/// How the flush coordinate was chosen, for reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushSource {
    /// Caller supplied the coordinate outright.
    Override,
    /// A decal position lifted from the demo — the engine already accepted it,
    /// so the surface is proven — picked as the one nearest the spawn.
    HarvestedNearSpawn,
    /// Computed floor point beneath the settled spawn position. Geometrically
    /// derived rather than proven, so only used when nothing was harvested.
    ComputedSpawnFloor,
    /// Floor points under the player's own walked path. Proven surfaces (they
    /// stood on them) and naturally spread apart, which is what a sweep needs.
    PlayerFloorPath,
    /// A coordinate from the map's accumulated store — proven by some earlier
    /// demo on this exact map build, and too isolated to have formed a tileable
    /// patch. See `decal_atlas`.
    MapAtlas,
    /// A grid tiled across a plane fitted to the demo's own decals. The surface
    /// is proven by those decals; the individual tiles are inference from them,
    /// which the engine permits — it constrains distance from a surface, not
    /// movement along one.
    TiledPlane,
    /// Sampled straight off the map's own world faces. The only source that
    /// owes nothing to what anyone did in the match, and therefore the only one
    /// that can supply a map the coordinate store has never seen. See
    /// `bsp::Bsp::face_candidates`.
    MapGeometry,
}

/// How the flush decided what a camera can see, for reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VisibilityBasis {
    /// Map geometry: leaf visibility plus a line-of-sight trace. The only
    /// basis that can tell a wall from a sightline.
    Geometry,
    /// No map available, so the frame cone alone. Correct about what is in
    /// front of a camera, silent about what stands in the way, and therefore
    /// over-rejects.
    #[default]
    ConeOnly,
}

/// Loads the map a demo was recorded on, when the caller supplied a maps
/// directory.
///
/// A missing or unreadable map is not an error: the flush worked without one
/// before this existed and still does, just with the cone alone. It is logged
/// rather than swallowed, because "this capture was planned without knowing
/// where the walls are" is worth seeing.
/// The key this demo's harvest is filed under.
///
/// HLTV demos leave the header's checksum field zeroed, so every one of them
/// would otherwise share a single `_00000000` bucket per map name — separate
/// from the bucket the same map's first-person demos fill, and unable to tell
/// two builds apart. When the map is here, its own checksum stands in, which
/// puts both kinds of demo in the same store.
///
/// That trusts the local library to be the build the HLTV demo was recorded on.
/// Nothing in an HLTV demo can confirm it, and the coordinates themselves are
/// proven either way — the cost of being wrong is a mis-filed harvest, not a
/// bad coordinate.
fn atlas_key(header: &dem::types::Header, opts: &DecalCleanOptions) -> Option<decal_atlas::MapKey> {
    let mut key = decal_atlas::MapKey::from_header(header)?;
    if key.checksum == 0
        && let Some(dir) = &opts.maps_dir
            && let Ok(found) = bsp::map_checksum_of_file(&dir.join(format!("{}.bsp", key.name))) {
                key.checksum = found;
            }
    Some(key)
}

fn load_map(
    demo: &dem::types::Demo,
    opts: &DecalCleanOptions,
    stats: &mut DecalCleanStats,
) -> Option<bsp::Bsp> {
    let dir = opts.maps_dir.as_ref()?;
    let key = decal_atlas::MapKey::from_header(&demo.header)?;
    let path = dir.join(format!("{}.bsp", key.name));

    // A map of the right name but the wrong build is worse than no map. Every
    // face, leaf and plane would be read successfully and refer to a different
    // world, so occlusion would answer confidently and wrongly — and a position
    // called hidden on geometry that is not the geometry being rendered is
    // exactly the failure this feature cannot show in its output.
    if key.checksum != 0 {
        match bsp::map_checksum_of_file(&path) {
            Ok(found) if found != key.checksum => {
                crate::log_markdown(&format!(
                    "⚠️ **Decal flush is ignoring `{}`** — the map here is a different build than \
                     the demo was recorded on (demo wants `{:08x}`, this is `{:08x}`). Its \
                     geometry describes a different world, so it cannot be trusted to say what a \
                     camera can see. Falling back to the frame cone alone.",
                    key.name, key.checksum, found
                ));
                return None;
            }
            Ok(_) => {}
            // Unreadable is left to `Bsp::from_file` below, which reports it.
            Err(_) => {}
        }
    }

    match bsp::Bsp::from_file(&path) {
        Ok(map) => {
            stats.visibility_basis = VisibilityBasis::Geometry;
            stats.map_faces = map.world_faces().len();
            stats.map_has_vis = map.has_vis();
            Some(map)
        }
        Err(e) => {
            crate::log_markdown(&format!(
                "ℹ️ **Decal flush has no map geometry** for `{}`: {}. Falling back to the frame \
                 cone alone, which cannot tell a wall from a sightline and so rejects spots that \
                 are actually hidden.",
                key.name, e
            ));
            None
        }
    }
}
/// Decides whether a candidate position is ever on screen during a clip.
///
/// The cone alone answers "is it in front of a camera", which is not the
/// question — a wall two rooms away is in front of the camera and invisible.
/// Given map geometry this asks the real one, cheapest test first:
///
///   1. PVS. If the candidate's leaf is absent from the union of every camera
///      leaf's potentially-visible set, the engine cannot render it from
///      anywhere those cameras stood. One lookup, and it settles most
///      candidates outright.
///   2. The cone, per camera. Cheap, and skips the trace for anything behind
///      the viewer.
///   3. A segment trace, only for candidates a camera is actually pointing at.
///
/// Without a map it degrades to the cone alone, which is what shipped before.
struct Visibility<'a> {
    bsp: Option<&'a bsp::Bsp>,
    /// Union of the PVS of every leaf a camera stood in. `None` when the map
    /// has no vis data or a camera leaf had no row, in which case nothing can
    /// be ruled out this way.
    camera_pvs: Option<Vec<u8>>,
    cos_cone: f32,
    max_distance: f32,
    /// Experiment gate — see `DecalCleanOptions::require_pvs_hidden`.
    require_pvs_hidden: bool,
}

impl<'a> Visibility<'a> {
    fn new(bsp: Option<&'a bsp::Bsp>, cameras: &[([f32; 3], [f32; 3])], opts: &DecalCleanOptions) -> Self {
        // Cameras cluster hard — thousands of samples collapse to a handful of
        // rooms — so the union is computed over distinct leaves, not samples.
        let camera_pvs = bsp.and_then(|b| {
            if !b.has_vis() {
                return None;
            }
            let mut leaves: Vec<usize> = cameras.iter().map(|(eye, _)| b.leaf_at(eye)).collect();
            leaves.sort_unstable();
            leaves.dedup();
            b.pvs_union(&leaves)
        });

        Self {
            bsp,
            camera_pvs,
            cos_cone: opts.visibility_cone_degrees.to_radians().cos(),
            max_distance: opts.visibility_max_distance,
            require_pvs_hidden: opts.require_pvs_hidden,
        }
    }

    /// Whether one camera can see a position: inside the frame, and with
    /// nothing solid in the way.
    ///
    /// This is also what the on-camera statistic counts, so the number the log
    /// reports and the rule the selection applied cannot disagree. They did
    /// briefly, and the stat screamed about hundreds of frames while every
    /// position it was complaining about was behind a wall.
    fn on_screen_from(&self, pos: &[f32; 3], eye: &[f32; 3], fwd: &[f32; 3]) -> bool {
        let v = [pos[0] - eye[0], pos[1] - eye[1], pos[2] - eye[2]];
        let dist = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        if dist < 1.0 || dist > self.max_distance {
            return false;
        }
        let fl = (fwd[0] * fwd[0] + fwd[1] * fwd[1] + fwd[2] * fwd[2]).sqrt();
        if fl < 0.5 {
            return false;
        }
        if (v[0] * fwd[0] + v[1] * fwd[1] + v[2] * fwd[2]) / (dist * fl) < self.cos_cone {
            return false; // outside the frame
        }
        match self.bsp {
            Some(b) => !b.line_blocked(eye, pos),
            None => true,
        }
    }

    /// Where a decal aimed at this candidate is actually drawn, or `None` when
    /// no surface is close enough for one to land on — which disqualifies the
    /// candidate rather than making it safe. Without a map there is nothing to
    /// project onto and the coordinate stands in for itself, as it always did.
    fn draw_point(&self, pos: &[f32; 3]) -> Option<[f32; 3]> {
        let Some(b) = self.bsp else {
            return Some(*pos);
        };
        // A coordinate inside solid is not a surface spot at all. The engine
        // still draws something for it — on whichever face its walk reaches,
        // which can be the one facing the camera — so these are dropped rather
        // than reasoned about. They are also exactly what the old point test
        // scored as safest, being occluded from everywhere by construction.
        let contents = b.leaf_contents(b.leaf_at(pos));
        if contents == bsp::CONTENTS_SOLID || contents == bsp::CONTENTS_SKY {
            return None;
        }
        b.decal_draw_point(pos, DECAL_PROJECTION_REACH, DECAL_SURFACE_LIFT)
    }

    fn hidden(&self, pos: &[f32; 3], cameras: &[([f32; 3], [f32; 3])]) -> bool {
        // Deliberately the trace alone, not PVS.
        //
        // PVS was a fast accept here: a candidate whose leaf is absent from
        // every camera leaf's PVS cannot be rendered, so it could be called
        // hidden without tracing. Sound in theory; the two disagreed in
        // practice. At a 4096-slot ring — where selection reaches far enough
        // down the ranked list to need marginal candidates — 200 in-clip
        // frames had a clear line of sight to positions PVS had called
        // invisible. One of the two is wrong, and nothing available here can
        // say which without loading the game.
        //
        // So the cheaper test does not get to grant safety. Losing it costs
        // time, and buying that back by trusting a check that has already been
        // caught disagreeing with the geometry is not a trade worth making for
        // the one defect this pass must never introduce.
        //
        // `camera_pvs` is kept because it is the thing to re-examine when the
        // in-game check happens: if the traces prove right, this is where the
        // speed goes back.
        // The candidate is a coordinate; the engine draws somewhere else. Judge
        // the place it draws, or the test certifies a wall in plain view as
        // hidden because the coordinate behind it is.
        let Some(draw) = self.draw_point(pos) else {
            return false;
        };
        // Experiment gate: demand that leaf visibility also say the engine never
        // renders the drawn surface. Asked of `draw`, not `pos`, for the same
        // reason the camera test is — a coordinate inside solid sits in a leaf
        // no PVS row contains and would pass this trivially.
        if self.require_pvs_hidden && !self.pvs_says_hidden(&draw).unwrap_or(false) {
            return false;
        }
        !cameras
            .iter()
            .any(|(eye, fwd)| self.on_screen_from(&draw, eye, fwd))
    }

    /// Whether leaf visibility also considers a position unrenderable, or
    /// `None` when the map carries no vis data.
    ///
    /// Not used to decide anything — see `hidden` for why. It is measured so
    /// the in-game check has a number to settle: if PVS agrees with the traces
    /// on every chosen position, the fast accept can come back.
    fn pvs_says_hidden(&self, pos: &[f32; 3]) -> Option<bool> {
        let b = self.bsp?;
        let pvs = self.camera_pvs.as_ref()?;
        Some(!bsp::Bsp::pvs_contains(pvs, b.leaf_at(pos)))
    }

    /// How many of these positions leaf visibility also calls hidden.
    fn pvs_agreement(&self, positions: &[[f32; 3]]) -> Option<usize> {
        self.camera_pvs.as_ref()?;
        Some(
            positions
                .iter()
                .filter(|p| self.pvs_says_hidden(p).unwrap_or(false))
                .count(),
        )
    }

    /// In-clip camera samples from which any of these positions is on screen.
    /// Must be zero.
    fn on_camera_frames(
        &self,
        positions: &[[f32; 3]],
        cameras: &[([f32; 3], [f32; 3])],
    ) -> usize {
        // Measured on the drawn surface, the same rule the selection applied.
        // Counting the coordinates instead reports zero for a sweep that covers
        // a wall the camera is looking straight at.
        let drawn: Vec<[f32; 3]> = positions.iter().filter_map(|p| self.draw_point(p)).collect();
        cameras
            .iter()
            .filter(|(eye, fwd)| {
                drawn
                    .iter()
                    .any(|p| self.on_screen_from(p, eye, fwd))
            })
            .count()
    }
}
/// Where the burst will go, plus enough of how that was decided to explain a
/// shortfall from the log alone.
#[derive(Default)]
struct Placement {
    positions: Vec<[f32; 3]>,
    source: Option<FlushSource>,
    /// Tiles laid across the fitted planes, before any camera filtering.
    tiled: usize,
    /// How many of those survived the clearance and line-of-sight tests. The
    /// gap between these two separates "this demo has no surface to work with"
    /// from "everything it has is in shot", which want opposite fixes.
    tiled_safe: usize,
    /// The same pair for the map-geometry source, when it was reached.
    map_sampled: usize,
    map_safe: usize,
}

/// Picks the set of positions the flush burst is spread across.
///
/// `wanted` is how many distinct spots are needed to place the whole burst at
/// `DECALS_PER_POSITION` each. Returning fewer means the sweep will fall short,
/// which the caller reports rather than hiding.
fn resolve_flush_positions(
    survey: &Survey,
    atlas: &[[f32; 3]],
    visibility: &Visibility,
    opts: &DecalCleanOptions,
    wanted: usize,
) -> Placement {
    if let Some(coord) = opts.flush_coord {
        return Placement {
            positions: vec![coord],
            source: Some(FlushSource::Override),
            ..Placement::default()
        };
    }

    // Never in shot. This is the guarantee: a position inside the camera's cone
    // at any sampled in-clip frame is rejected outright, however far away it is.
    //
    // Clearance deliberately is NOT part of this test. It was, as a hard floor,
    // and it quietly wrecked the pass on a third of a 28-demo survey: on
    // harrington, all 8165 tiles sat within 900 units of some camera at some
    // point in eight clips, so every one was thrown away and the flush fell
    // back to a single position — a sweep that turns 4 of 256 ring slots.
    // Dropping the floor to 250 there kept `flush_on_camera_frames` at 0 while
    // restoring a full sweep, which is the measurement that settles it: the
    // cone test is what keeps decals off screen, and distance is a tiebreak.
    // With map geometry this also asks whether anything stands in the way,
    // which the cone alone cannot. That matters most where the cone is
    // strictest: on a busy map most of what it discards is behind a wall.
    let hidden = |pos: &[f32; 3]| -> bool { visibility.hidden(pos, &survey.window_cameras) };

    // Furthest approach any in-window camera makes to a position. Ranking by
    // this — rather than by nearness to spawn — matches what these spots are
    // actually for. Spawn proximity was a holdover from "hide it near spawn";
    // a flush spot has no reason to be anywhere in particular except far from
    // the lens.
    let clearance = |pos: &[f32; 3]| -> f32 {
        survey
            .window_cameras
            .iter()
            .map(|(eye, _)| distance(pos, eye))
            .fold(f32::INFINITY, f32::min)
    };

    // Tiles across the fitted planes first: same proven surfaces the harvested
    // decals sit on, but a whole grid per patch instead of one point per decal,
    // which is what lets a sweep reach a full ring revolution. Raw harvested
    // positions follow, covering decals too isolated to form a patch. Floor
    // points under the player's own path come last — plentiful and proven, but
    // by construction where the player walks, which is the worst place to hide
    // something. Used only to make up a shortfall in count.
    // Tiled across everything proven to be surface on this map, not just what
    // this demo proved. The atlas is what makes a quiet wall tileable when the
    // POV player never shot it.
    let mut proven: Vec<[f32; 3]> = survey.harvested.clone();
    proven.extend_from_slice(atlas);
    let tiled = tile_positions(&proven);
    let mut placement = Placement {
        tiled: tiled.len(),
        ..Placement::default()
    };

    let mut pool: Vec<[f32; 3]> = Vec::new();
    let mut source = None;

    // Returns how many of the candidates cleared every camera, having taken
    // what it could from them.
    let absorb = |candidates: &[[f32; 3]],
                  src: FlushSource,
                  pool: &mut Vec<[f32; 3]>,
                  source: &mut Option<FlushSource>|
     -> usize {
        // Clearance is measured against every in-window camera sample, so it is
        // computed once per candidate rather than inside the comparator — tiling
        // multiplies the candidate count by an order of magnitude and a sort
        // that recomputed it would dominate the whole pass.
        let mut ok: Vec<(f32, [f32; 3])> = candidates
            .iter()
            .copied()
            .filter(|p| hidden(p))
            .map(|p| (clearance(&p), p))
            .collect();
        let safe = ok.len();
        // Furthest from the camera first.
        ok.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        for (_, p) in ok {
            if pool.len() >= wanted {
                break;
            }
            // Enforce spacing across the whole pool, not just within a source,
            // so two sources cannot contribute overlapping spots.
            if pool.iter().all(|q| distance(&p, q) >= MIN_POSITION_SPACING) {
                pool.push(p);
                source.get_or_insert(src);
            }
        }
        safe
    };

    // Everything drawn from the match, unless the experiment gate is holding
    // them back so the map source can be judged on its own.
    if !opts.map_geometry_only {
        placement.tiled_safe = absorb(&tiled, FlushSource::TiledPlane, &mut pool, &mut source);
        if pool.len() < wanted {
            absorb(
                &survey.harvested,
                FlushSource::HarvestedNearSpawn,
                &mut pool,
                &mut source,
            );
        }
        // Atlas coordinates too isolated to have formed a tileable patch still
        // stand on their own as proven surface.
        if pool.len() < wanted {
            absorb(atlas, FlushSource::MapAtlas, &mut pool, &mut source);
        }
    }
    // The map's own faces, and only now. Sampling them is not free — every
    // candidate is projected onto a face and traced against every camera — and
    // on a map whose store is populated the sources above have already filled
    // the pool, so paying for it there would be pure cost for no position. The
    // case it exists for is the opposite one: a map nothing has been harvested
    // from yet, where everything above comes back nearly empty.
    if pool.len() < wanted
        && let Some(map) = visibility.bsp {
            let sampled = map.face_candidates(&bsp::FaceSampling::default());
            placement.map_sampled = sampled.len();
            placement.map_safe = absorb(
                &sampled,
                FlushSource::MapGeometry,
                &mut pool,
                &mut source,
            );
        }
    if pool.len() < wanted && !opts.map_geometry_only {
        absorb(
            &survey.floor_candidates,
            FlushSource::PlayerFloorPath,
            &mut pool,
            &mut source,
        );
    }

    if !pool.is_empty() {
        placement.positions = pool;
        placement.source = source;
        return placement;
    }

    // Only the last-resort single position needs a spawn reference, to compute
    // a floor point beneath it. Gating the whole function on one cost two
    // demos in the survey their entire flush: both were scrim recordings whose
    // refparams never yielded a settled on-ground origin, yet both carried
    // ~1500-2200 real decals that would have served as positions perfectly
    // well. No reference now means no fallback, not no flush.
    let Some(reference) = survey.grounded_origin.or(survey.spawn_eye) else {
        return placement;
    };
    // Under the experiment gate a spawn-floor guess would quietly put the
    // sweep back on ground the player walked, which is the one thing the gate
    // exists to exclude. Better to report no positions at all.
    if opts.map_geometry_only {
        return placement;
    }

    let (positions, source) = legacy_single_position(survey, opts, reference);
    Placement {
        positions,
        source,
        ..placement
    }
}

/// Original single-position selection, retained as the last resort for demos
/// that yield no safe spread at all.
fn legacy_single_position(
    survey: &Survey,
    opts: &DecalCleanOptions,
    reference: [f32; 3],
) -> (Vec<[f32; 3]>, Option<FlushSource>) {

    // Two independent disqualifiers, because neither alone is sufficient:
    //
    //  - Distance: the camera physically walking over the spot. Spawn is also
    //    the corridor players leave through, so the decal nearest spawn is a
    //    prime offender.
    //  - Line of sight: a spot 1200 units away, dead centre of frame down a
    //    long sightline, is plainly visible despite comfortable "clearance".
    //    Distance-only selection picks these, so the forward vector recorded
    //    with each in-window camera sample is used for a real frustum test.
    let cos_cone = opts.visibility_cone_degrees.to_radians().cos();

    let on_camera_frames = |pos: &[f32; 3]| -> usize {
        survey
            .window_cameras
            .iter()
            .filter(|(eye, fwd)| {
                let v = [pos[0] - eye[0], pos[1] - eye[1], pos[2] - eye[2]];
                let dist = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
                if dist < 1.0 || dist > opts.visibility_max_distance {
                    return false;
                }
                let fl = (fwd[0] * fwd[0] + fwd[1] * fwd[1] + fwd[2] * fwd[2]).sqrt();
                if fl < 0.5 {
                    return false;
                }
                let dot = (v[0] * fwd[0] + v[1] * fwd[1] + v[2] * fwd[2]) / (dist * fl);
                dot >= cos_cone
            })
            .count()
    };

    let clearance = |pos: &[f32; 3]| -> f32 {
        survey
            .window_cameras
            .iter()
            .map(|(eye, _)| distance(pos, eye))
            .fold(f32::INFINITY, f32::min)
    };

    if !survey.harvested.is_empty() {
        let mut scored: Vec<([f32; 3], f32, usize)> = survey
            .harvested
            .iter()
            .map(|pos| (*pos, clearance(pos), on_camera_frames(pos)))
            .collect();

        // Never on screen during a recorded clip AND never walked over.
        let mut clear: Vec<&([f32; 3], f32, usize)> = scored
            .iter()
            .filter(|(_, c, seen)| *seen == 0 && *c >= opts.min_camera_clearance)
            .collect();

        if !clear.is_empty() {
            clear.sort_by(|a, b| {
                distance(&a.0, &reference)
                    .partial_cmp(&distance(&b.0, &reference))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            return (vec![clear[0].0], Some(FlushSource::HarvestedNearSpawn));
        }

        // Nothing was fully clear. Prefer the least-seen candidate, breaking
        // ties on distance; the caller reports both so a marginal pick is
        // visible rather than silent.
        scored.sort_by(|a, b| {
            a.2.cmp(&b.2)
                .then(b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
        });
        return (vec![scored[0].0], Some(FlushSource::HarvestedNearSpawn));
    }

    (
        vec![[reference[0], reference[1], reference[2] - opts.floor_drop]],
        Some(FlushSource::ComputedSpawnFloor),
    )
}

/// Blanks every decal-placing message outside `keep_windows`, reporting
/// `(wall decals, player sprays)` removed.
///
/// An empty `keep_windows` strips the entire demo, which is what the offset
/// probe wants: a blank canvas so the only decals on a wall are the ones it
/// injected.
///
/// Only messages that PLACE a decal are stripped. SvcDecalName (36)
/// deliberately is not: it registers a decal name against an index (how a
/// custom player spray is announced) and places nothing. Blanking it does not
/// remove a decal — it destroys the texture lookup that decals referencing
/// that index still need, including any index this pass harvests for its own
/// flush burst.
pub(super) fn strip_decal_messages(
    demo: &mut dem::types::Demo,
    keep_windows: &[(i32, i32)],
) -> (usize, usize) {
    let (mut wall, mut spray) = (0usize, 0usize);
    let mut ordinal = 0i32;
    for entry in &mut demo.directory.entries {
        for frame in &mut entry.frames {
            ordinal += 1;
            if in_window(ordinal, keep_windows) {
                continue;
            }
            let FrameData::NetworkMessage(net_msg_box) = &mut frame.frame_data else {
                continue;
            };
            let MessageData::Parsed(messages) = &mut net_msg_box.1.messages else {
                continue;
            };
            for msg in messages.iter_mut() {
                let NetMessage::EngineMessage(eng) = msg else {
                    continue;
                };
                let strip_type = match eng.as_ref() {
                    EngineMessage::SvcTempEntity(te)
                        if WALL_DECAL_ENTITY_TYPES.contains(&te.entity_type) =>
                    {
                        te.entity_type
                    }
                    _ => continue,
                };
                if strip_type == TE_PLAYERDECAL {
                    spray += 1;
                } else {
                    wall += 1;
                }
                **eng = EngineMessage::SvcNop;
            }
        }
    }
    (wall, spray)
}

/// Strips decal messages outside `keep_windows` and injects ring-sweeping decal
/// bursts ahead of each window.
///
/// `keep_windows` are inclusive `[start, stop]` pairs in **frame-ordinal space**
/// — the same tick space `PatchJob::scheduled_commands` uses and `engine.rs`
/// compares against `frame_counter`, i.e. a 1-based count of every frame record
/// in file order. They should be the real record-start/record-stop ticks used to
/// schedule `mirv_recordmovie_start`/`stop`, not the wider highlight bounds.
///
/// These are NOT `Frame::frame` values. That field holds the engine tick, which
/// several frame records share, so the two spaces differ by roughly 2.6x on a
/// real demo — passing one where the other is expected silently targets the
/// wrong part of the demo.
pub fn clean_demo_decals(
    demo_bytes: &[u8],
    keep_windows: &[(i32, i32)],
    opts: &DecalCleanOptions,
) -> Result<(Vec<u8>, DecalCleanStats), String> {
    let mut demo = open_demo_from_bytes(demo_bytes)
        .map_err(|e| format!("Could not parse demo file: {}", e))?;

    let mut stats = DecalCleanStats::default();

    let survey = survey(&demo, keep_windows, opts);
    stats.harvested_decals = survey.harvested.len();
    stats.spawn_reference = survey.grounded_origin;
    let texture_index = opts.flush_texture_index.or(survey.texture_index);
    stats.flush_texture_index = texture_index;

    // Distinct spots needed so every injected decal allocates a fresh ring slot
    // instead of being recycled as an overlap of one already placed.
    let burst_count = opts.ring_limit as usize + opts.burst_margin;
    let positions_wanted = burst_count.div_ceil(DECALS_PER_POSITION);

    // The map's own coordinate store. This demo's proven world coordinates go
    // in, the union of every demo ever processed for this exact map build comes
    // back out. A demo whose player never shot the quiet side of the map can
    // still flush there, because some earlier demo proved that surface exists.
    let mut atlas: Vec<[f32; 3]> = Vec::new();
    if let Some(dir) = &opts.atlas_dir {
        match atlas_key(&demo.header, opts) {
            Some(key) => {
                let (merged, astats) = decal_atlas::merge_and_save(
                    dir,
                    &opts.atlas_seed_dirs,
                    &key,
                    &survey.world_harvested,
                );
                stats.atlas = astats;
                stats.atlas_map = Some(format!("{} ({:08x})", key.name, key.checksum));
                atlas = merged;
            }
            None => crate::log_markdown(
                "⚠️ **Decal atlas skipped** — the demo header carries no usable map name, so \
                 there is nothing to key a coordinate store on.",
            ),
        }
    }

    // The map itself, when the caller told us where to find one. Used both to
    // decide what is genuinely hidden and, in time, to supply coordinates that
    // owe nothing to where anyone happened to shoot.
    let map = load_map(&demo, opts, &mut stats);
    let visibility = Visibility::new(map.as_ref(), &survey.window_cameras, opts);

    let placement = resolve_flush_positions(&survey, &atlas, &visibility, opts, positions_wanted);
    let flush_positions = placement.positions;
    stats.flush_coord = flush_positions.first().copied();
    stats.flush_source = placement.source;
    stats.flush_positions = flush_positions.len();
    stats.flush_positions_wanted = positions_wanted;
    stats.tiled_candidates = placement.tiled;
    stats.tiled_camera_safe = placement.tiled_safe;
    stats.map_candidates = placement.map_sampled;
    stats.map_camera_safe = placement.map_safe;

    if let (Some(pos), Some(reference)) = (flush_positions.first(), survey.grounded_origin) {
        stats.spawn_to_flush_distance = Some(distance(pos, &reference));
    }

    if !flush_positions.is_empty() {
        stats.min_camera_distance = survey
            .window_cameras
            .iter()
            .flat_map(|(eye, _)| flush_positions.iter().map(move |p| distance(p, eye)))
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        // Sampled in-window frames from which ANY flush position is on screen.
        // Non-zero means part of the spread is visible during a recorded clip —
        // the failure this whole selection exists to avoid.
        //
        // Measured with the same rule the selection used, geometry included.
        // Counting cone hits here while selecting on occlusion would report
        // hundreds of frames for positions that are all behind walls.
        stats.camera_samples = survey.window_cameras.len();
        stats.flush_on_camera_frames =
            visibility.on_camera_frames(&flush_positions, &survey.window_cameras);
        stats.pvs_agrees_hidden = visibility.pvs_agreement(&flush_positions);

        if opts.collect_diagnostics {
            stats.diagnostic_positions = flush_positions.clone();
            stats.diagnostic_cameras = survey.window_cameras.clone();
        }
    }

    // ── Pass 1: strip decal messages outside the capture windows ─────────────
    if opts.strip_outside_windows {
        let (wall, spray) = strip_decal_messages(&mut demo, keep_windows);
        stats.temp_entity_stripped += wall;
        stats.player_spray_stripped += spray;
    }

    // ── Pass 2: flush bursts ahead of each capture window ────────────────────
    if opts.flush_burst
        && let (false, Some(texture_index)) = (flush_positions.is_empty(), texture_index) {
            // Eligible carriers, in global frame order: parsed network frames
            // small enough that a handful of extra 9-byte messages cannot push
            // the packet near the engine's buffer ceiling. Built across every
            // entry at once so a window is never confined to one entry's frames.
            // Every frame with its ordinal and its own timestamp, so the burst
            // deadline can be a duration walked through real times rather than
            // a frame count standing in for one.
            let all_frames = frame_ordinals(&demo);
            let all_times: Vec<f32> = all_frames
                .iter()
                .map(|&(entry_idx, frame_idx, _)| {
                    demo.directory.entries[entry_idx]
                        .frames
                        .get(frame_idx)
                        .map(|f| f.time)
                        .unwrap_or(f32::NAN)
                })
                .collect();

            let eligible: Vec<(usize, usize, i32)> = all_frames
                .iter()
                .copied()
                // Never inside a clip being recorded. The sweep's whole job is
                // to turn the decal ring in the gap BEFORE a clip; a carrier
                // landing inside one turns the ring while that clip is being
                // filmed, which can unlink the bullet holes the firefight is
                // putting up — decals disappearing mid-shot, the exact artefact
                // this feature removes. At a 256 ring the burst is 68 frames
                // and never reached that far; at the 4,096 maximum it is 1,028
                // and reached into an earlier clip on 10 of 85 demos.
                .filter(|&(_, _, ordinal)| !in_window(ordinal, keep_windows))
                .filter(|&(entry_idx, frame_idx, _)| {
                    crate::patch::is_injectable_frame(&demo, entry_idx, frame_idx)
                })
                .collect();

            let mut used: std::collections::HashSet<(usize, usize)> =
                std::collections::HashSet::new();
            let mut plan: Vec<(usize, usize, usize)> = Vec::new();

            for &(window_start, _) in keep_windows {
                let deadline =
                    deadline_before(window_start, opts.lead_seconds, &all_frames, &all_times)
                        .unwrap_or(window_start - FALLBACK_LEAD_FRAMES);
                let mut remaining = burst_count;

                // Walk backwards from the deadline so the sweep finishes as
                // late as possible — nothing after it can re-dirty a wall.
                let start_at = match eligible.iter().rposition(|&(_, _, ord)| ord <= deadline) {
                    Some(p) => p,
                    None => {
                        stats.bursts_short.push((window_start, 0, burst_count));
                        continue;
                    }
                };

                for slot in (0..=start_at).rev() {
                    if remaining == 0 {
                        break;
                    }
                    let (entry_idx, frame_idx, ordinal) = eligible[slot];
                    if !used.insert((entry_idx, frame_idx)) {
                        continue;
                    }
                    if in_window(ordinal, keep_windows) {
                        stats.burst_frames_inside_clip += 1;
                    }
                    let take = remaining.min(opts.max_per_frame);
                    plan.push((entry_idx, frame_idx, take));
                    remaining -= take;
                }

                if remaining > 0 {
                    stats
                        .bursts_short
                        .push((window_start, burst_count - remaining, burst_count));
                }
                if remaining < burst_count {
                    stats.bursts_placed += 1;
                }
            }

            // Walk the position list so no spot receives more than
            // DECALS_PER_POSITION consecutive decals. Exceeding
            // MAX_OVERLAP_DECALS at one spot makes the engine recycle instead
            // of allocate, which stops the ring advancing and voids the sweep.
            let mut placed_here = 0usize;
            let mut pos_idx = 0usize;

            for (entry_idx, frame_idx, count) in plan {
                let Some(frame) = demo
                    .directory
                    .entries
                    .get_mut(entry_idx)
                    .and_then(|e| e.frames.get_mut(frame_idx))
                else {
                    continue;
                };
                let FrameData::NetworkMessage(net_msg_box) = &mut frame.frame_data else {
                    continue;
                };
                let MessageData::Parsed(messages) = &mut net_msg_box.1.messages else {
                    continue;
                };
                for _ in 0..count {
                    let pos = flush_positions[pos_idx % flush_positions.len()];
                    messages.push(build_world_decal(&pos, texture_index));
                    stats.flush_decals_injected += 1;
                    placed_here += 1;
                    if placed_here >= DECALS_PER_POSITION {
                        placed_here = 0;
                        pos_idx += 1;
                    }
                }
            }
        }

    // ── Pin r_decals so the ring stays small and never strands a slot ────────
    if opts.inject_r_decals_command {
        let playback_idx = demo
            .directory
            .entries
            .iter()
            .position(|e| e.type_ == 1)
            .or_else(|| demo.directory.entries.len().checked_sub(1));

        if let Some(entry) = playback_idx.and_then(|i| demo.directory.entries.get_mut(i)) {
            // DemoStart (type 2) must be processed before any ConsoleCommand
            // (type 3), or the engine reads uninitialised memory.
            let insert_at = entry
                .frames
                .iter()
                .rposition(|f| matches!(f.frame_data, FrameData::DemoStart))
                .map(|p| p + 1)
                .unwrap_or(0);
            let anchor = entry
                .frames
                .get(insert_at)
                .or_else(|| entry.frames.first())
                .map(|f| (f.time, f.frame))
                .unwrap_or((0.0, 0));
            let cmd = format!("r_decals {}", opts.ring_limit);
            entry.frames.insert(
                insert_at,
                Frame {
                    time: anchor.0,
                    frame: anchor.1,
                    frame_data: FrameData::ConsoleCommand(ConsoleCommand {
                        command: ByteString::from(cmd.as_str()),
                    }),
                },
            );
            entry.frame_count = entry.frames.len() as i32;
        }
    }

    Ok((demo.write_to_bytes(), stats))
}

/// Every world-surface coordinate this demo proves exists.
///
/// No capture windows are applied: this is the demo's whole contribution, not
/// the subset outside a clip. Exposed because these are coordinates the engine
/// provably accepted, which makes them the one honest way to check parsed map
/// geometry without loading the game. See `docs/archive/decal_flush_bsp_surfaces.md`.
pub fn proven_world_coordinates(demo: &dem::types::Demo) -> Vec<[f32; 3]> {
    survey(demo, &[], &DecalCleanOptions::default()).world_harvested
}

/// Strip-only entry point, kept for callers that just want the decal messages
/// outside `keep_windows` blanked with no burst injection or cvar pinning.
pub fn strip_decals_outside_windows(
    demo_bytes: &[u8],
    keep_windows: &[(i32, i32)],
) -> Result<(Vec<u8>, DecalCleanStats), String> {
    let opts = DecalCleanOptions {
        flush_burst: false,
        inject_r_decals_command: false,
        ..Default::default()
    };
    clean_demo_decals(demo_bytes, keep_windows, &opts)
}

// ── Batch-pipeline pre-pass ──────────────────────────────────────────────────
//
// `StreamPatcher::patch` streams its input as a file, while `clean_demo_decals`
// is a whole-file parse and rewrite — the two cannot share a buffer. So the
// flush runs ahead of the patch, writing cleaned bytes to a scratch demo that
// the patch then streams from. It lives inside `patch()` rather than at its
// call sites (the CLI, `spawn_patch_batch`, and two in `capture_manager`) so
// those cannot drift apart on whether decals get cleaned.

use super::types::{PatchJob, PatcherConfig};

/// A cleaned copy of a source demo, alive only as long as the patch reading it.
/// Removed on drop, which covers the error and cancellation paths as well as
/// the ordinary one.
pub struct CleanedSource {
    path: std::path::PathBuf,
}

impl CleanedSource {
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for CleanedSource {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Scratch demos left behind by a process that was killed rather than unwound.
///
/// Drop covers the error and cancellation paths, but nothing runs when a
/// process is terminated outright, and each of these is the size of a demo —
/// tens or hundreds of megabytes. Swept on the way past rather than tracked,
/// since there is nowhere durable to track them.
///
/// Age-gated so a capture running concurrently in another process cannot have
/// its scratch deleted out from under it. No flush takes anything like this
/// long; the margin is for a machine that was asleep mid-run.
const SCRATCH_STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

/// Filename prefix identifying a flush scratch demo, so the sweep can recognise
/// its own leavings and nothing else.
const SCRATCH_PREFIX: &str = "dodtools_decalflush_";

fn sweep_stale_scratch(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(SCRATCH_PREFIX)
        {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .map(|age| age > SCRATCH_STALE_AFTER)
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Scratch path for one cleaned demo. Kept in the system temp directory rather
/// than beside the output: the capture directories are scanned for takes and
/// swept by the auto-clear passes, neither of which should ever see this file.
fn scratch_path(source_demo: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);

    let stem = std::path::Path::new(source_demo)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "demo".to_string());

    let dir = std::env::temp_dir();
    sweep_stale_scratch(&dir);
    dir.join(format!(
        "{}{}_{}_{}.dem",
        SCRATCH_PREFIX,
        stem,
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}


/// Extra half-angle allowed beyond the frame's own corner.
///
/// Cameras are sampled every fourth in-window frame, so a fast turn can carry a
/// spot into shot between two samples and never be seen by the test. This is
/// the margin against that, and against the FOV in effect differing slightly
/// from the one configured.
const CAMERA_TURN_MARGIN_DEGREES: f32 = 6.0;

/// Half-angle off the view axis that must be treated as on screen, for a given
/// capture FOV and frame shape.
///
/// A cone is a crude stand-in for a frustum, and the part that decides the
/// answer is the CORNER of the frame, not its edge. At 105 degrees horizontal
/// on a 16:9 frame a decal can sit ~56 degrees off the view axis and still be
/// in shot; the horizontal half-angle alone would say 52.5, and the 40 degrees
/// this code used before — justified as "generous" against a 90-degree FOV —
/// was under even the horizontal half-angle of 45. It was rejecting nothing
/// between 40 and 49 degrees that a 90-degree capture genuinely showed, and
/// nothing between 40 and 56 at 105.
///
/// Erring wide only costs candidate positions. Erring narrow puts a decal on
/// screen during a recorded clip, which is the one defect this pass exists to
/// avoid, so the bias here is deliberate and one-directional.
pub fn on_screen_half_angle(fov_degrees: f32, width: i32, height: i32) -> f32 {
    let fov = fov_degrees.clamp(1.0, 179.0);
    let aspect = if width > 0 && height > 0 {
        width as f32 / height as f32
    } else {
        4.0 / 3.0
    };

    let tan_h = (fov.to_radians() / 2.0).tan();
    let tan_v = tan_h / aspect.max(0.01);
    let corner = (tan_h * tan_h + tan_v * tan_v).sqrt().atan().to_degrees();

    (corner + CAMERA_TURN_MARGIN_DEGREES).min(89.0)
}

/// The FOV a capture will actually run at.
///
/// Read out of `init_commands` when HLAE's `mirv_fov` is among them, because
/// that is the value the engine will be using and a separate setting could
/// silently disagree with it. The last one wins, matching the console. Falls
/// back to the configured default when nothing sets it.
pub fn capture_fov(config: &PatcherConfig) -> f32 {
    capture_fov_from_init(&config.init_commands).unwrap_or(config.capture_fov)
}

/// The FOV `init_commands` states, if it states one at all.
///
/// Separate from `capture_fov` because "nothing was stated" and "the stated
/// value happens to equal the default" are different facts, and collapsing them
/// into one number loses the difference. `capture_fov_resolved` needs it: the
/// app's own seeded default is `mirv_fov 90`, so a user who takes the defaults
/// and has `mirv_fov 105` in a config would otherwise have their explicit 90
/// silently replaced by the config's 105.
pub fn capture_fov_from_init(init_commands: &[String]) -> Option<f32> {
    for cmd in init_commands.iter().rev() {
        let trimmed = cmd.trim();
        let Some(rest) = trimmed
            .strip_prefix("mirv_fov")
            .or_else(|| trimmed.strip_prefix("default_fov"))
        else {
            continue;
        };
        // Guard against matching a longer command that merely starts the same.
        if !rest.starts_with(char::is_whitespace) {
            continue;
        }
        // Real .cfg syntax quotes every value ("105", not 105) — the same
        // convention "Load from .cfg file…" carries straight into Initial
        // Commands. Parsing the raw token instead of the unquoted value
        // means this silently reads a quoted line as "nothing stated" and
        // falls through to the default, which is the whole bug this exists
        // to prevent.
        if let Ok(v) = cfg_scan::unquote(rest.trim()).parse::<f32>()
            && v > 0.0 {
                return Some(v);
            }
    }
    None
}

/// The mod folder holding the game's configs, for a configured `hl.exe`.
fn game_dir_for(config: &PatcherConfig) -> Option<std::path::PathBuf> {
    let dir = std::path::Path::new(&config.game_path).parent()?.join("dod");
    dir.is_dir().then_some(dir)
}

/// The FOV a capture will actually run at, including what the game's own
/// configs set.
///
/// `capture_fov` reads the app's init commands, which was the whole story only
/// as long as nothing else set it. It is not: a `config.cfg` ending in
/// `exec movie.cfg`, with `mirv_fov 105` inside, renders at 105 while the
/// pipeline sizes its on-screen test for the configured default — a cone some
/// seven degrees too narrow, calling in-shot positions hidden.
///
/// So the order is init commands, then an executed config, then the configured
/// default. `ring_limit` (r_decals) now follows exactly this same order —
/// it used to be the one deliberate exception, on the reasoning that adopting
/// a `movie.cfg`'s `r_decals 0` would silently stand the flush down. That
/// objection is gone now that a resolved 0 with Flush Decals on is its own
/// loud, reported fact (`decal_flush_is_noop` in the desktop-studio report)
/// rather than something this function would have hidden by disagreeing with
/// the config. User-requested symmetry, 2026-09-05.
///
/// Reads config files. Never writes one.
pub fn capture_fov_resolved(config: &PatcherConfig) -> f32 {
    // An init command is the app's own statement and outranks a config —
    // including when it states the same number the default happens to be. The
    // seeded default IS `mirv_fov 90`, so treating that as "nothing was said"
    // would hand every defaulted install over to whatever its movie.cfg says.
    if let Some(stated) = capture_fov_from_init(&config.init_commands) {
        return stated;
    }

    let Some(dir) = game_dir_for(config) else {
        return config.capture_fov;
    };
    let scan = cfg_scan::scan_cached(&dir);
    for cvar in ["mirv_fov", "default_fov"] {
        if let Some(setting) = scan.effective(cvar)
            && let Ok(v) = setting.value.parse::<f32>()
                && v > 0.0 {
                    return v;
                }
    }
    config.capture_fov
}

/// Report anything the game's configs set that the pipeline depends on.
///
/// Read-only, and advisory: the user's configs are theirs to change. This only
/// makes sure a value the app never hears about does not stay invisible.
fn warn_about_game_cfgs(config: &PatcherConfig) {
    let Some(dir) = game_dir_for(config) else {
        return;
    };
    let scan = cfg_scan::scan_cached(&dir);
    let found = scan.effective_settings();
    if found.is_empty() {
        return;
    }

    // Once per folder per run. This is called per demo, and a batch of forty
    // would otherwise write the same paragraph into the activity log forty
    // times — which is how a warning worth reading becomes one nobody reads.
    {
        static WARNED: std::sync::OnceLock<
            std::sync::RwLock<std::collections::HashSet<std::path::PathBuf>>,
        > = std::sync::OnceLock::new();
        let warned = WARNED.get_or_init(Default::default);
        let Ok(mut w) = warned.write() else {
            return;
        };
        if !w.insert(dir.clone()) {
            return;
        }
    }

    let lines: Vec<String> = found
        .iter()
        .map(|s| format!("`{} {}` in `{}` line {}", s.cvar, s.value, s.file_name(), s.line))
        .collect();

    crate::log_markdown(&format!(
        "⚠️ **The game's own configs set values this pipeline depends on**: {}. Nothing here \
         changes them — they are yours. But `r_decals` decides how many decals the engine keeps, \
         and the sweep is sized to it; `mirv_fov` decides what counts as on screen. Stating them \
         in the app's Init Commands instead puts them where the pipeline can see them. The FOV \
         above has been used for this capture; `r_decals` has not, because the pipeline's own pin \
         runs after the config and would override it anyway.",
        lines.join(", ")
    ));
}

/// `r_decals` as stated in `init_commands`, if it is stated there at all.
///
/// The last one wins, matching the console. Clamped to the engine's own
/// ceiling, because the engine clamps it too and a sweep sized past the ceiling
/// would spend its extra positions turning a ring that had already come round.
pub fn ring_limit_from_init(init_commands: &[String]) -> Option<u32> {
    for cmd in init_commands.iter().rev() {
        let trimmed = cmd.trim();
        let Some(rest) = trimmed.strip_prefix("r_decals") else {
            continue;
        };
        // Guard against matching a longer command that merely starts the same.
        if !rest.starts_with(char::is_whitespace) {
            continue;
        }
        // Quoted, same as capture_fov_from_init above and for the same
        // reason: real .cfg syntax quotes every value, and a raw parse on
        // `"512"` fails silently, reading a stated line as unstated.
        if let Ok(v) = cfg_scan::unquote(rest.trim()).parse::<u32>() {
            return Some(v.min(crate::patch::MAX_RENDER_DECALS));
        }
    }
    None
}

/// How many decals the ring will hold, and so how many positions a full sweep
/// has to turn.
///
/// This is one number, and `r_decals` is where the engine reads it. A separate
/// setting could only ever agree with the cvar or silently disagree with it, so
/// an `init_commands` entry is the authority when there is one, an executed
/// config is next, and the configured default fills in when neither states it.
/// Same precedence as `capture_fov_resolved` — genuinely the same rule now,
/// not merely similar (see that function's doc comment for the history of why
/// `r_decals` used to be the one exception).
pub fn ring_limit(config: &PatcherConfig) -> u32 {
    ring_limit_from_init(&config.init_commands)
        .or_else(|| ring_limit_from_game_config(config))
        .unwrap_or_else(|| config.decal_ring_limit.min(crate::patch::MAX_RENDER_DECALS))
}

/// `r_decals` as an executed config states it, if one does and the value
/// actually parses — same shape as `ring_limit_from_init`, just for the other
/// place a value can come from. Clamped the same way, for the same reason.
///
/// Read-only, like everything touching the user's configs — this decides what
/// the pipeline treats as the standing value, never what the config says.
/// See [`crate::patch::cfg_scan`].
pub fn ring_limit_from_game_config(config: &PatcherConfig) -> Option<u32> {
    let dir = game_dir_for(config)?;
    let setting = cfg_scan::scan_cached(&dir).effective("r_decals")?.clone();
    setting
        .value
        .parse::<u32>()
        .ok()
        .map(|v| v.min(crate::patch::MAX_RENDER_DECALS))
}

/// Clean options for the batch pipeline, as distinct from the `strip_decals`
/// CLI's.
///
/// The one difference that matters is `inject_r_decals_command`. The CLI has no
/// `init_commands` to pin the cvar from, so it inserts a `ConsoleCommand` frame
/// into the playback entry. In the pipeline that insertion would shift every
/// later frame ordinal by +1 and silently desync `job.scheduled_commands` from
/// `StreamPatcher`'s `frame_counter` — every scheduled command firing a frame
/// late, with nothing in the output bytes to show for it. So here the cvar is
/// pinned from `init_commands` (see `builder`) and no frame is inserted. The
/// burst itself only pushes messages into frames that already exist, so it
/// moves no ordinal and is safe.
/// Where the game keeps its maps, derived from the configured `hl.exe`.
///
/// `game_path` points at the executable, and DoD's content sits beside it, so
/// maps are `<hl.exe dir>/dod/maps`. Returned as `None` when that does not
/// resolve to a real directory rather than handing the loader a path that can
/// only fail per demo.
fn maps_dir_for(config: &PatcherConfig) -> Option<std::path::PathBuf> {
    super::map_check::maps_dir_for_exe(std::path::Path::new(&config.game_path))
}

fn flush_options(config: &PatcherConfig) -> DecalCleanOptions {
    DecalCleanOptions {
        ring_limit: ring_limit(config),
        inject_r_decals_command: false,
        // The pipeline is the only caller that accumulates a store. The CLI and
        // the probe rig stay self-contained, so a one-off experiment never
        // writes coordinates that a real capture would later rely on.
        atlas_dir: Some(decal_atlas::default_dir()),
        maps_dir: maps_dir_for(config),
        // Derived, never assumed — and looked for everywhere the engine would
        // find it, not just in the app's own init commands. A movie shot at
        // `mirv_fov 105` puts the frame corner ~16 degrees further out than the
        // fixed 40 this used to carry, and that 105 is as likely to be sitting
        // in the user's `movie.cfg` as in the app.
        visibility_cone_degrees: on_screen_half_angle(
            capture_fov_resolved(config),
            config.resolution_width,
            config.resolution_height,
        ),
        // One experiment, reachable from a real capture because the engine is
        // the only thing that can answer it — see `require_pvs_hidden`.
        // Env-gated rather than exposed as a setting: it is a question being
        // asked once, not a mode anyone should be choosing between.
        require_pvs_hidden: std::env::var("DOD_FLUSH_PVS_ONLY").is_ok(),
        // Forces the map-geometry source, which is otherwise unreachable on a
        // map whose coordinate store is already full — see `map_geometry_only`.
        map_geometry_only: std::env::var("DOD_FLUSH_MAP_GEOMETRY_ONLY").is_ok(),
        ..Default::default()
    }
}

/// The frames each of a job's blocks records between, in the frame-ordinal
/// domain that `StreamPatcher`'s `frame_counter` and `job.scheduled_commands`
/// already share.
///
/// All or nothing: `None` unless every block contributes a window. Stripping
/// keys off these, so a block that contributed none would have its own recorded
/// clip treated as outside every window — scrubbing the bullet holes that land
/// during the action. Dirty walls are much the lesser defect.
///
/// A start of 0 means the bounds never got filled in. Equal bounds do not: with
/// no record lead or trail configured, a single-kill highlight genuinely does
/// start and stop recording on one frame. Only an inverted window is rejected —
/// an `r_stop` clamped back to an end-of-demo frame ahead of its own start.
fn keep_windows_for(job: &PatchJob) -> Option<Vec<(i32, i32)>> {
    let windows: Vec<(i32, i32)> = job
        .blocks
        .iter()
        .filter(|b| b.record_start_tick > 0 && b.record_stop_tick >= b.record_start_tick)
        .map(|b| (b.record_start_tick, b.record_stop_tick))
        .collect();

    (windows.len() == job.blocks.len()).then_some(windows)
}

/// Cleans a job's source demo of wall decals outside its recorded clips, and
/// sweeps the decal ring ahead of each one.
///
/// Returns `None` when there is nothing to do, and — deliberately — also when
/// the flush fails. Decal hygiene is cosmetic; losing an entire capture batch
/// over it would not be. Every such path logs loudly first, because this is a
/// feature whose failures are invisible in the output bytes: a demo that was
/// not cleaned patches and records exactly like one that was, and only looks
/// wrong on screen.
pub fn prepare_flushed_source(job: &PatchJob, config: &PatcherConfig) -> Option<CleanedSource> {
    if !config.decal_flush {
        return None;
    }

    // `r_decals 0` turns decals off outright. There is then no ring to turn and
    // no bullet hole to clear, and a sweep sized zero would be a burst with
    // nowhere to put anything. Flush Decals Between Clips being on and r_decals
    // being 0 is a real, reachable contradiction — logged the same as every
    // other skip path, since this one is otherwise completely silent.
    if ring_limit(config) == 0 {
        crate::log_markdown(
            "⚠️ **Decal flush skipped** — Flush Decals Between Clips is on, but r_decals \
             resolves to 0 (stated in Initial Commands, or the app's own configured default). \
             There is no ring to sweep. Capture continues; walls will not be cleaned between clips.",
        );
        return None;
    }

    // The primer job and preview jobs carry no blocks: nothing is being
    // recorded from them, so there is no clip to keep clean.
    if job.blocks.is_empty() {
        return None;
    }

    warn_about_game_cfgs(config);

    let keep_windows = match keep_windows_for(job) {
        Some(w) => w,
        None => {
            crate::log_markdown(&format!(
                "⚠️ **Decal flush skipped** — not every block in `{}` carries usable record \
                 bounds. Capture continues; walls will not be cleaned between clips.",
                job.source_demo
            ));
            return None;
        }
    };

    let bytes = match std::fs::read(&job.source_demo) {
        Ok(b) => b,
        Err(e) => {
            crate::log_markdown(&format!(
                "⚠️ **Decal flush skipped** — could not read `{}`: {}",
                job.source_demo, e
            ));
            return None;
        }
    };

    let opts = flush_options(config);

    let (cleaned, stats) = match clean_demo_decals(&bytes, &keep_windows, &opts) {
        Ok(v) => v,
        Err(e) => {
            crate::log_markdown(&format!(
                "⚠️ **Decal flush failed** on `{}`: {}. Capture continues with the unmodified \
                 demo; walls will not be cleaned between clips.",
                job.source_demo, e
            ));
            return None;
        }
    };
    drop(bytes);

    let path = scratch_path(&job.source_demo);
    if let Err(e) = std::fs::write(&path, &cleaned) {
        crate::log_markdown(&format!(
            "⚠️ **Decal flush skipped** — could not write scratch demo `{}`: {}",
            path.display(),
            e
        ));
        return None;
    }

    report(job, &stats, &keep_windows, &opts);
    Some(CleanedSource { path })
}

/// Writes the flush result to the capture log. The counts are informational;
/// the warnings below are not — each marks a way the sweep can come out
/// structurally correct and still be wrong on screen.
fn report(
    job: &PatchJob,
    stats: &DecalCleanStats,
    keep_windows: &[(i32, i32)],
    opts: &DecalCleanOptions,
) {
    // The source is on the main line, not just in the warning below it: a
    // sweep anchored on tiled planes and one anchored on a computed floor point
    // produce identical counts and completely different odds of working.
    let source = match stats.flush_source {
        Some(FlushSource::TiledPlane) => "tiled planes",
        Some(FlushSource::MapGeometry) => "the map's own geometry",
        Some(FlushSource::MapAtlas) => "the map coordinate store",
        Some(FlushSource::HarvestedNearSpawn) => "harvested decals",
        Some(FlushSource::PlayerFloorPath) => "floor under the player's path",
        Some(FlushSource::ComputedSpawnFloor) => "computed spawn floor",
        Some(FlushSource::Override) => "caller override",
        None => "none",
    };

    // Say so loudly. A run under the experiment gate is not a normal capture,
    // and a log that does not mention it is a log someone will later read as
    // evidence about the shipped behaviour.
    if opts.require_pvs_hidden {
        crate::log_markdown(
            "🧪 **EXPERIMENT: `DOD_FLUSH_PVS_ONLY` is set.** The sweep was placed only where leaf \
             visibility says the engine never renders it. This is the test of whether a decal on \
             an unrendered face still allocates a ring slot — if old bullet holes survive into a \
             clip, it does not. Unset the variable for a normal capture.",
        );
    }
    if opts.map_geometry_only {
        crate::log_markdown(
            "🧪 **EXPERIMENT: `DOD_FLUSH_MAP_GEOMETRY_ONLY` is set.** The sweep was placed from \
             the map's own world faces alone — no harvested decals, no tiled planes, no \
             coordinate store, no floor path. This is how the source meant for maps nobody has \
             captured yet gets watched on a map somebody can watch. Unset the variable for a \
             normal capture.",
        );
    }

    crate::log_markdown(&format!(
        "🧹 **Decal flush** on `{}`: stripped {} wall decals and {} sprays outside {} clip(s); \
         injected {} flush decals across {} of {} position(s) from {}, in {} burst(s); \
         r_decals pinned to {}.",
        job.source_demo,
        stats.temp_entity_stripped,
        stats.player_spray_stripped,
        keep_windows.len(),
        stats.flush_decals_injected,
        stats.flush_positions,
        stats.flush_positions_wanted,
        source,
        stats.bursts_placed,
        opts.ring_limit
    ));

    // What the on-screen decision was actually able to consult. This changes
    // how much the position count is worth: 68 positions chosen against real
    // geometry and 68 chosen against a bare cone are not the same claim, and
    // the counts look identical.
    match stats.visibility_basis {
        VisibilityBasis::Geometry => crate::log_markdown(&format!(
            "👁️ **Placement used map geometry** — {} world faces{}. Positions were kept only \
             where the map says the camera cannot see them, not merely where they fall outside \
             the frame.",
            stats.map_faces,
            if stats.map_has_vis {
                ", with visibility data"
            } else {
                ", but the map carries no visibility data, so every in-frame candidate needed a \
                 trace"
            }
        )),
        VisibilityBasis::ConeOnly => crate::log_markdown(
            "⚠️ **Placement used the frame cone alone** — no map geometry was available, so the \
             pass cannot tell a wall from a sightline. It rejects spots that are genuinely \
             hidden, and where that leaves it with nothing it will settle for a marginal one.",
        ),
    }

    // What the map's store contributed. Worth its own line: a demo that
    // sweeps only because earlier demos proved the surface is a different
    // situation from one that stands on its own, and the difference is
    // invisible in the position count.
    if let Some(map) = &stats.atlas_map {
        crate::log_markdown(&format!(
            "🗺️ **Map coordinate store** for `{}`: {} coordinate(s) already known, {} added by \
             this demo, {} now held{}.",
            map,
            stats.atlas.known,
            stats.atlas.added,
            stats.atlas.total,
            // The harvest still runs under the gate — refusing the store as a
            // *source* is not a reason to stop feeding it — but saying those
            // coordinates were "available to the flush" would be untrue.
            if opts.map_geometry_only {
                " (the experiment gate refused them as a source for this run)"
            } else {
                " and available to the flush"
            }
        ));
    }

    // The map's own faces, reached only when everything proven fell short.
    // Worth saying out loud: normally it means this demo's own decals and the
    // store between them could not fill a sweep, and the map covered the
    // difference. On a map nothing has been harvested from yet that is the
    // expected path, not a warning.
    //
    // Under the experiment gate the same line would state a reason that is
    // false — the proven sources did not fall short, they were never asked —
    // and a log that misreports why a source was used is worse than one that
    // says nothing, because it is the log someone reads back months later as
    // evidence.
    if stats.map_candidates > 0 {
        let why = if opts.map_geometry_only {
            "every source drawn from the match was refused by the experiment gate"
        } else {
            "the demo's decals and the coordinate store together could not fill a sweep"
        };
        crate::log_markdown(&format!(
            "🧱 **Placed from the map's own geometry** — {}, so {} point(s) were sampled off the \
             map's world faces and {} of those stayed clear of every in-clip camera. This source \
             owes nothing to where anyone shot or walked, which is what lets it cover a map the \
             store has never seen.",
            why, stats.map_candidates, stats.map_camera_safe
        ));
    }

    // Too few distinct spots. Past MAX_OVERLAP_DECALS at one position the
    // engine recycles a decal instead of allocating, and a recycled decal never
    // advances the ring — so the sweep stops short of a full revolution and
    // some of the old decals survive it.
    if stats.flush_positions < stats.flush_positions_wanted {
        crate::log_markdown(&format!(
            "⚠️ **Partial decal sweep** — only {} of the {} distinct positions a full ring \
             revolution needs, so some decals will survive into the clip. {} tiles were laid \
             across the demo's proven planes and {} of those stayed clear of every in-clip \
             camera. An `r_decals {}` init command would give a complete sweep of a smaller \
             ring instead.",
            stats.flush_positions,
            stats.flush_positions_wanted,
            stats.tiled_candidates,
            stats.tiled_camera_safe,
            // The largest ring this many positions can fully turn, backing the
            // over-provision margin out of the burst it implies.
            (stats.flush_positions * DECALS_PER_POSITION).saturating_sub(opts.burst_margin)
        ));
    }

    // Positions closer to the lens than preferred. Not a defect on its own —
    // the surface each is drawn on cleared the on-screen test, so none is ever
    // in shot — but it narrows the margin against a camera turn falling between
    // two samples.
    if let Some(nearest) = stats.min_camera_distance
        && nearest < opts.min_camera_clearance {
            crate::log_markdown(&format!(
                "ℹ️ **Decal flush spots are closer to the camera than preferred** — nearest \
                 approach {:.0} units against a {:.0}-unit preference. The surface each one is \
                 drawn on cleared the line-of-sight test, so none should be in shot; this is the \
                 margin narrowing, not a decal on screen.",
                nearest, opts.min_camera_clearance
            ));
        }

    // The one outright defect this pass can introduce: its own decals on
    // screen. The cone test is sampled every fourth frame, so a non-zero count
    // here has to be looked at rather than trusted.
    if stats.flush_on_camera_frames > 0 {
        crate::log_markdown(&format!(
            "⚠️ **Flush decals may be on camera** — the chosen spot(s) fall inside the camera cone \
             on {} of {} sampled in-clip frames. Review the takes.",
            stats.flush_on_camera_frames, stats.camera_samples
        ));
    }

    // A gap too short to fit a whole sweep before the clip opens.
    for (window_start, placed, wanted) in &stats.bursts_short {
        crate::log_markdown(&format!(
            "⚠️ **Short decal burst** before frame {} — placed {} of {}. That clip is not \
             guaranteed to start clean.",
            window_start, placed, wanted
        ));
    }

    // A computed floor point is geometry the demo never proved. If it misses a
    // surface the engine creates nothing and the entire sweep no-ops silently.
    if stats.flush_source == Some(FlushSource::ComputedSpawnFloor) {
        crate::log_markdown(
            "⚠️ **Decal flush anchored on a computed floor point** — the demo contained no real \
             decal to borrow a proven surface from. If that point misses geometry, the sweep does \
             nothing at all.",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::types::CaptureBlock;

    /// `source_demo` deliberately points at nothing: every case below must
    /// decide to skip before it ever opens the file, so a read attempt would
    /// surface as a failure rather than a silent fallback.
    fn job_with_blocks(blocks: Vec<CaptureBlock>) -> PatchJob {
        PatchJob {
            source_demo: "no_such_demo_should_ever_be_read.dem".to_string(),
            output_demo: std::path::PathBuf::from("unused_output.dem"),
            streaks: Vec::new(),
            target_player: None,
            init_commands: Vec::new(),
            scheduled_commands: Vec::new(),
            director_events: Vec::new(),
            block_routes: Vec::new(),
            blocks,
        }
    }

    fn block(block_index: usize, record_start_tick: i32, record_stop_tick: i32) -> CaptureBlock {
        CaptureBlock {
            demo_name: "chain_01".to_string(),
            block_index,
            drive_index: 0,
            take_folder: std::path::PathBuf::from("take"),
            take_key: String::new(),
            source_streak_indices: vec![block_index],
            start_tick: record_start_tick,
            end_tick: record_stop_tick,
            record_start_tick,
            record_stop_tick,
        }
    }

    #[test]
    fn the_pipeline_never_inserts_the_r_decals_frame() {
        // Inserting that ConsoleCommand frame shifts every later frame ordinal
        // by +1, which desyncs the scheduled capture commands from the
        // patcher's frame counter — invisibly, since the demo still parses and
        // plays. The pipeline pins the cvar from init_commands instead.
        let config = PatcherConfig {
            decal_ring_limit: 128,
            ..PatcherConfig::default()
        };
        let opts = flush_options(&config);

        assert!(
            !opts.inject_r_decals_command,
            "the pipeline must not insert a console-command frame — init_commands owns r_decals"
        );
        assert_eq!(opts.ring_limit, 128, "the configured ring size must reach the sweep");
    }

    #[test]
    fn an_init_command_states_the_ring_and_the_sweep_is_sized_to_it() {
        // One number, read where the engine reads it. A separate setting could
        // only agree with the cvar or silently disagree, and a sweep sized to
        // the wrong one under-clears with nothing in the output to show it.
        let config = PatcherConfig {
            decal_ring_limit: 128,
            init_commands: vec!["r_decals 512".to_string()],
            ..PatcherConfig::default()
        };

        assert_eq!(ring_limit(&config), 512);
        assert_eq!(flush_options(&config).ring_limit, 512);
    }

    /// A game folder laid out as the engine expects: `hl.exe` with `dod/`
    /// beside it. Returns the path to the exe.
    fn fake_game(tag: &str, config_cfg: &str, movie_cfg: Option<&str>) -> std::path::PathBuf {
        let root = std::env::temp_dir()
            .join(format!("dod_fov_cfg_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dod = root.join("dod");
        std::fs::create_dir_all(&dod).unwrap();
        std::fs::write(dod.join("config.cfg"), config_cfg).unwrap();
        if let Some(movie) = movie_cfg {
            std::fs::write(dod.join("movie.cfg"), movie).unwrap();
        }
        let exe = root.join("hl.exe");
        std::fs::write(&exe, b"").unwrap();
        exe
    }

    #[test]
    fn a_fov_the_game_config_sets_is_found_rather_than_assumed() {
        // The real shape, and the reason this exists: config.cfg ends with
        // `exec movie.cfg`, movie.cfg carries `mirv_fov 105`, and the app was
        // never told. Sizing the cone for the default 90 makes it ~7 degrees
        // too narrow and calls in-shot positions hidden.
        let exe = fake_game("found", "exec movie.cfg\n", Some("mirv_fov \"105\"\n"));
        let config = PatcherConfig {
            game_path: exe.to_string_lossy().to_string(),
            ..PatcherConfig::default()
        };

        assert_eq!(capture_fov(&config), config.capture_fov, "init commands say nothing");
        assert_eq!(capture_fov_resolved(&config), 105.0, "the config does");
    }

    /// A demo header naming a map, with the checksum an HLTV recording would
    /// leave zeroed.
    fn header_for(map: &str, checksum: u32) -> dem::types::Header {
        let mut name = vec![0u8; 260];
        name[..map.len()].copy_from_slice(map.as_bytes());
        dem::types::Header {
            magic: b"HLDEMO\0\0".to_vec(),
            demo_protocol: 5,
            network_protocol: 48,
            map_name: name.into(),
            game_directory: vec![0u8; 260].into(),
            map_checksum: checksum,
            directory_offset: 0,
        }
    }

    /// The smallest thing `map_checksum` will read: a v30 header with fifteen
    /// empty lumps. No geometry needed — only the checksum is being asked for.
    fn empty_map(dir: &std::path::Path, name: &str) -> u32 {
        let mut bytes = vec![0u8; 4 + 15 * 8];
        bytes[0] = 30;
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(format!("{}.bsp", name)), &bytes).unwrap();
        crate::patch::bsp::map_checksum(&bytes).unwrap()
    }

    // ── Burst deadline ──────────────────────────────────────────────────────
    //
    // A frame count is not a duration. Records are not evenly spaced and there
    // are several per rendered frame, so `seconds * fps` drifts — the mistake
    // `docs/bugs.md` records as "All injected commands collapse to Tick 0".

    /// `(entry, frame, ordinal)` plus the matching timestamps, the two arrays
    /// `deadline_before` walks.
    fn frames_at(times: &[(usize, f32)]) -> (Vec<(usize, usize, i32)>, Vec<f32>) {
        let mut frames = Vec::new();
        let mut stamps = Vec::new();
        let mut per_entry = std::collections::HashMap::new();
        for (i, &(entry, time)) in times.iter().enumerate() {
            let idx = per_entry.entry(entry).or_insert(0usize);
            frames.push((entry, *idx, i as i32 + 1));
            *idx += 1;
            stamps.push(time);
        }
        (frames, stamps)
    }

    #[test]
    fn the_deadline_is_walked_through_real_timestamps_not_divided_by_a_rate() {
        // Deliberately uneven spacing: a rate-based answer would land in the
        // wrong place precisely because the gaps differ.
        let (frames, times) = frames_at(&[
            (1, 0.0),
            (1, 0.1),
            (1, 5.0),
            (1, 5.05),
            (1, 5.1),
            (1, 5.9),
            (1, 6.0),
        ]);

        // 1s before the frame at t=6.0 means target 5.0, and the last frame at
        // or before that is ordinal 3 (t=5.0).
        assert_eq!(deadline_before(7, 1.0, &frames, &times), Some(3));
        // Shave the lead and the deadline moves later, across the tight cluster
        // rather than proportionally — which is the whole point of walking.
        assert_eq!(deadline_before(7, 0.95, &frames, &times), Some(4));
        // A lead longer than the entry's whole history cannot be answered.
        assert_eq!(deadline_before(7, 30.0, &frames, &times), None);
    }

    #[test]
    fn the_walk_stops_at_the_entry_boundary() {
        // Timestamps restart per directory entry: a LOADING entry's near-zero
        // stamps satisfy any target and would drag the deadline to the front of
        // the demo. Better to answer None and let the caller fall back.
        let (frames, times) = frames_at(&[(0, 0.0), (0, 0.01), (1, 0.0), (1, 0.5), (1, 1.0)]);

        // A lead the entry can satisfy on its own is answered from the entry.
        assert_eq!(deadline_before(5, 0.9, &frames, &times), Some(3));
        assert_eq!(deadline_before(5, 0.4, &frames, &times), Some(4));

        // One it cannot gives up rather than crossing: the frames before are a
        // different entry whose clock restarted, and their near-zero stamps
        // would satisfy any target and drag the deadline to the demo's front.
        assert_eq!(
            deadline_before(5, 2.0, &frames, &times),
            None,
            "must not reach into the previous entry"
        );
    }

    #[test]
    fn a_zero_lead_is_the_window_itself_and_an_unknown_anchor_is_none() {
        let (frames, times) = frames_at(&[(1, 0.0), (1, 1.0)]);

        assert_eq!(deadline_before(2, 0.0, &frames, &times), Some(2));
        assert_eq!(deadline_before(99, 1.0, &frames, &times), None);
    }

    #[test]
    fn an_hltv_demo_keys_its_harvest_off_the_local_map_not_a_zero() {
        // HLTV demos zero the checksum field, so every one of them would
        // otherwise share a single `<map>_00000000` bucket — separate from the
        // bucket the same map's first-person demos fill, and unable to tell two
        // builds apart.
        let dir = std::env::temp_dir()
            .join(format!("dod_atlas_key_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let expected = empty_map(&dir, "dod_anzio");

        let opts = DecalCleanOptions {
            maps_dir: Some(dir.clone()),
            ..Default::default()
        };

        let hltv = atlas_key(&header_for("dod_anzio", 0), &opts).unwrap();
        assert_eq!(hltv.checksum, expected, "resolved from the map on disk");

        let pov = atlas_key(&header_for("dod_anzio", 0xdead_beef), &opts).unwrap();
        assert_eq!(
            pov.checksum, 0xdead_beef,
            "a demo that states its build is never second-guessed"
        );
    }

    #[test]
    fn an_hltv_demo_with_no_map_available_keeps_its_zero() {
        // Nothing to resolve from, and inventing a checksum would be worse than
        // an honest shared bucket.
        let opts = DecalCleanOptions {
            maps_dir: Some(std::env::temp_dir().join("dod_atlas_key_absent")),
            ..Default::default()
        };

        assert_eq!(atlas_key(&header_for("dod_anzio", 0), &opts).unwrap().checksum, 0);
    }

    #[test]
    fn an_init_command_stating_the_default_value_still_outranks_the_config() {
        // The trap: `mirv_fov 90` is what the app seeds into a new install's
        // init commands, and 90 is also the configured default. Deciding
        // "was one stated?" by comparing against the default would read that as
        // silence and hand every defaulted install to its movie.cfg — here,
        // rendering the cone for 105 when the user asked for 90.
        let exe = fake_game("states_default", "exec movie.cfg\n", Some("mirv_fov \"105\"\n"));
        let config = PatcherConfig {
            game_path: exe.to_string_lossy().to_string(),
            init_commands: vec!["mirv_fov 90".to_string()],
            ..PatcherConfig::default()
        };

        assert_eq!(config.capture_fov, 90.0, "the default this could be confused with");
        assert_eq!(capture_fov_from_init(&config.init_commands), Some(90.0));
        assert_eq!(capture_fov_resolved(&config), 90.0, "the user asked for 90");
    }

    #[test]
    fn an_init_command_outranks_the_game_config() {
        let exe = fake_game("outrank", "exec movie.cfg\n", Some("mirv_fov \"105\"\n"));
        let config = PatcherConfig {
            game_path: exe.to_string_lossy().to_string(),
            init_commands: vec!["mirv_fov 120".to_string()],
            ..PatcherConfig::default()
        };

        assert_eq!(capture_fov_resolved(&config), 120.0);
    }

    #[test]
    fn a_config_that_switches_decals_off_is_now_adopted_like_fov_is() {
        // Regression: this used to assert the opposite — that a config's
        // `r_decals 0` was ignored in favor of the app's own default, on the
        // reasoning that adopting it would silently stand the flush down.
        // Now that stand-down is its own loud, reported fact
        // (decal_flush_is_noop) rather than something to hide by disagreeing
        // with the config, so r_decals follows mirv_fov's precedence exactly:
        // init commands, then an executed config, then the app's default.
        let exe = fake_game("decals_off", "exec movie.cfg\n", Some("r_decals \"0\"\n"));
        let config = PatcherConfig {
            game_path: exe.to_string_lossy().to_string(),
            ..PatcherConfig::default()
        };

        assert_eq!(ring_limit(&config), 0, "the config's own value now wins, same as mirv_fov's would");
    }

    #[test]
    fn a_nonzero_r_decals_the_game_config_sets_is_adopted() {
        let exe = fake_game("decals_from_config", "exec movie.cfg\n", Some("r_decals \"512\"\n"));
        let config = PatcherConfig {
            game_path: exe.to_string_lossy().to_string(),
            decal_ring_limit: 128,
            ..PatcherConfig::default()
        };

        assert_eq!(ring_limit(&config), 512, "the config's value, not the app's own default");
    }

    #[test]
    fn an_init_command_still_outranks_a_game_config_for_r_decals() {
        let exe = fake_game("decals_init_outranks", "exec movie.cfg\n", Some("r_decals \"0\"\n"));
        let config = PatcherConfig {
            game_path: exe.to_string_lossy().to_string(),
            init_commands: vec!["r_decals 512".to_string()],
            ..PatcherConfig::default()
        };

        assert_eq!(ring_limit(&config), 512, "stated in Initial Commands, so it wins outright");
    }

    #[test]
    fn the_last_r_decals_wins_like_the_console_does() {
        let config = PatcherConfig {
            init_commands: vec!["r_decals 512".to_string(), "r_decals 64".to_string()],
            ..PatcherConfig::default()
        };

        assert_eq!(ring_limit(&config), 64);
    }

    #[test]
    fn a_quoted_r_decals_in_init_commands_is_still_recognised() {
        // Real .cfg syntax quotes every value, and "Load from .cfg file…"
        // carries that straight into Initial Commands — a raw, unquoted
        // parse would silently read `r_decals "512"` as unstated and fall
        // through to the app's own default instead of the user's value.
        let config = PatcherConfig {
            decal_ring_limit: 128,
            init_commands: vec!["r_decals \"512\"".to_string()],
            ..PatcherConfig::default()
        };

        assert_eq!(ring_limit_from_init(&config.init_commands), Some(512));
        assert_eq!(ring_limit(&config), 512);
    }

    #[test]
    fn a_quoted_mirv_fov_in_init_commands_is_still_recognised() {
        let config = PatcherConfig {
            capture_fov: 90.0,
            init_commands: vec!["mirv_fov \"105\"".to_string()],
            ..PatcherConfig::default()
        };

        assert_eq!(capture_fov_from_init(&config.init_commands), Some(105.0));
        assert_eq!(capture_fov(&config), 105.0);
    }

    #[test]
    fn the_configured_default_fills_in_when_nothing_states_it() {
        let config = PatcherConfig {
            decal_ring_limit: 128,
            init_commands: vec!["mirv_fov 105".to_string()],
            ..PatcherConfig::default()
        };

        assert_eq!(ring_limit_from_init(&config.init_commands), None);
        assert_eq!(ring_limit(&config), 128);
    }

    #[test]
    fn a_ring_past_the_engine_ceiling_is_clamped_to_it() {
        // The engine clamps r_decals to MAX_RENDER_DECALS, so positions beyond
        // that would be spent turning a ring that had already come round.
        let config = PatcherConfig {
            init_commands: vec!["r_decals 99999".to_string()],
            ..PatcherConfig::default()
        };

        assert_eq!(ring_limit(&config), crate::patch::MAX_RENDER_DECALS);
    }

    #[test]
    fn a_longer_command_that_merely_starts_the_same_is_not_the_ring() {
        let config = PatcherConfig {
            decal_ring_limit: 256,
            init_commands: vec!["r_decals_enabled 1".to_string()],
            ..PatcherConfig::default()
        };

        assert_eq!(ring_limit_from_init(&config.init_commands), None);
        assert_eq!(ring_limit(&config), 256);
    }

    #[test]
    fn decals_switched_off_entirely_leaves_the_demo_alone() {
        // r_decals 0 is no decals at all: no ring to turn, no bullet holes to
        // clear, and a sweep sized zero would be a burst with nowhere to put
        // anything.
        let config = PatcherConfig {
            init_commands: vec!["r_decals 0".to_string()],
            ..PatcherConfig::default()
        };
        let job = job_with_blocks(vec![block(0, 1000, 2000)]);

        assert_eq!(ring_limit(&config), 0);
        assert!(prepare_flushed_source(&job, &config).is_none());
    }

    #[test]
    fn flush_disabled_leaves_the_demo_alone() {
        let config = PatcherConfig {
            decal_flush: false,
            ..PatcherConfig::default()
        };
        let job = job_with_blocks(vec![block(0, 1000, 2000)]);

        assert!(prepare_flushed_source(&job, &config).is_none());
    }

    #[test]
    fn jobs_with_no_blocks_are_skipped() {
        // The primer job and preview jobs record nothing, so there is no clip
        // to keep clean and nothing to strip against.
        let job = job_with_blocks(Vec::new());

        assert!(prepare_flushed_source(&job, &PatcherConfig::default()).is_none());
    }

    #[test]
    fn one_block_missing_its_record_bounds_skips_the_whole_job() {
        // A partial window set would strip the decals landing inside the clip
        // whose bounds went missing — worse than the dirty walls being fixed.
        let job = job_with_blocks(vec![block(0, 1000, 2000), block(1, 0, 0)]);

        assert!(keep_windows_for(&job).is_none());
        assert!(prepare_flushed_source(&job, &PatcherConfig::default()).is_none());
    }

    #[test]
    fn an_inverted_record_window_skips_the_whole_job() {
        // A record stop clamped back to the end of the demo can land ahead of
        // its own start. That window keeps nothing, so the clip would be
        // stripped rather than protected.
        let job = job_with_blocks(vec![block(0, 1000, 2000), block(1, 9000, 8000)]);

        assert!(keep_windows_for(&job).is_none());
    }

    #[test]
    fn a_single_frame_record_window_is_usable() {
        // With no record lead or trail, a one-kill highlight really does start
        // and stop on the same frame. Rejecting that would silently disable the
        // flush for the whole job.
        let job = job_with_blocks(vec![block(0, 1000, 2000), block(1, 5000, 5000)]);

        assert_eq!(
            keep_windows_for(&job),
            Some(vec![(1000, 2000), (5000, 5000)]),
            "equal record bounds are a real one-frame window, not a missing one"
        );
    }

    /// A patch of decals on an upright wall at x = `plane`.
    fn wall_patch(plane: f32, ys: &[f32], zs: &[f32]) -> Vec<[f32; 3]> {
        let mut out = Vec::new();
        for &y in ys {
            for &z in zs {
                out.push([plane, y, z]);
            }
        }
        out
    }

    #[test]
    fn the_spacing_filter_cannot_decimate_a_tiled_grid() {
        // Tiles are laid at TILE_PITCH and then every candidate has to clear
        // MIN_POSITION_SPACING against the pool. If the spacing ever exceeded
        // the pitch, neighbouring tiles would reject each other and tiling
        // would quietly stop multiplying positions at all.
        assert!(
            MIN_POSITION_SPACING < TILE_PITCH,
            "spacing {} must stay under the tile pitch {}",
            MIN_POSITION_SPACING,
            TILE_PITCH
        );
        // And the pitch must clear the engine's overlap distance, which is
        // twice the measured ~4-unit decal radius. Inside that, the engine
        // recycles instead of allocating and the ring stops turning.
        assert!(TILE_PITCH > 8.0, "tiles would overlap and be recycled");
    }

    #[test]
    fn tiling_multiplies_positions_across_a_proven_plane() {
        let members = wall_patch(100.0, &[0.0, 20.0, 40.0, 60.0], &[0.0, 20.0]);
        let tiles = tile_positions(&members);

        assert!(
            tiles.len() > members.len(),
            "tiling produced {} positions from {} decals — the whole point is a grid per patch",
            tiles.len(),
            members.len()
        );
        for t in &tiles {
            assert!(
                (t[0] - 100.0).abs() < 0.001,
                "tile {:?} left the fitted plane — it would miss the wall entirely",
                t
            );
        }
    }

    #[test]
    fn tiles_stay_within_reach_of_a_real_decal() {
        // A tile past the end of a wall hits nothing, and a position that
        // creates no decal allocates no ring slot — so the sweep comes up short
        // silently rather than failing.
        let members = wall_patch(100.0, &[0.0, 20.0, 40.0, 60.0], &[0.0, 20.0]);
        let tiles = tile_positions(&members);

        for t in &tiles {
            let nearest = members
                .iter()
                .map(|m| distance(m, t))
                .fold(f32::INFINITY, f32::min);
            assert!(
                nearest <= TILE_REACH,
                "tile {:?} sits {:.1} units from any proven decal",
                t,
                nearest
            );
        }
    }

    #[test]
    fn coplanar_but_distant_groups_are_not_bridged() {
        // Coplanar is not contiguous: two stretches of wall at the same x with
        // a doorway between them must not get tiles hung across the gap.
        let mut members = wall_patch(100.0, &[0.0, 20.0, 40.0, 60.0], &[0.0, 20.0]);
        members.extend(wall_patch(100.0, &[900.0, 920.0, 940.0, 960.0], &[0.0, 20.0]));

        let tiles = tile_positions(&members);
        assert!(!tiles.is_empty());

        for t in &tiles {
            let in_gap = t[1] > 60.0 + TILE_REACH && t[1] < 900.0 - TILE_REACH;
            assert!(!in_gap, "tile {:?} hangs in the gap between two patches", t);
        }
    }

    #[test]
    fn a_long_wall_is_capped_at_the_tiling_extent() {
        // The cap is a confidence limit, not an engine one: the further a tile
        // sits from the decals proving the surface, the more it is inference.
        let ys: Vec<f32> = (0..13).map(|i| i as f32 * 50.0).collect();
        let members = wall_patch(100.0, &ys, &[0.0]);

        let tiles = tile_positions(&members);
        assert!(!tiles.is_empty());

        let lo = tiles.iter().fold(f32::INFINITY, |a, t| a.min(t[1]));
        let hi = tiles.iter().fold(f32::NEG_INFINITY, |a, t| a.max(t[1]));
        assert!(
            hi - lo <= TILE_MAX_EXTENT + TILE_PITCH,
            "tiles spanned {:.0} units across a 600-unit wall; the cap is {}",
            hi - lo,
            TILE_MAX_EXTENT
        );
    }

    #[test]
    fn a_couple_of_stray_marks_are_not_treated_as_a_surface() {
        // Two decals prove two points, not a plane worth tiling.
        let members = vec![[100.0, 0.0, 0.0], [100.0, 20.0, 0.0]];
        assert!(tile_positions(&members).is_empty());
    }

    #[test]
    fn the_cone_covers_the_corner_of_the_frame_not_its_edge() {
        // A decal at the corner of the frame is further off the view axis than
        // one at the edge, so the horizontal half-angle is not enough on its
        // own. Testing against it is what let a spot at 47 degrees pass as
        // "hidden" during a 90-degree capture.
        for fov in [75.0f32, 90.0, 105.0, 120.0] {
            let half = on_screen_half_angle(fov, 1920, 1080);
            assert!(
                half > fov / 2.0,
                "at {} degrees the cone was {:.1}, which does not even reach the frame edge at {:.1}",
                fov,
                half,
                fov / 2.0
            );
        }
    }

    #[test]
    fn a_wider_capture_needs_a_wider_cone() {
        let at90 = on_screen_half_angle(90.0, 1920, 1080);
        let at105 = on_screen_half_angle(105.0, 1920, 1080);
        assert!(
            at105 > at90 + 5.0,
            "105 gave {:.1} against 90's {:.1} — a movie shot wide would keep spots that are in shot",
            at105,
            at90
        );
    }

    #[test]
    fn the_old_fixed_cone_was_too_narrow_at_every_common_fov() {
        // Regression guard for the bug this replaced: 40 degrees, justified as
        // "generous" against a 90-degree FOV, was under even the 45-degree
        // horizontal half-angle.
        const OLD_FIXED_CONE: f32 = 40.0;
        for (fov, w, h) in [(90.0f32, 1920, 1080), (90.0, 1280, 960), (105.0, 1920, 1080)] {
            assert!(
                on_screen_half_angle(fov, w, h) > OLD_FIXED_CONE,
                "fov {} at {}x{} still fits inside the old fixed cone",
                fov,
                w,
                h
            );
        }
    }

    #[test]
    fn a_taller_frame_pushes_the_corner_further_out() {
        // 4:3 puts more of the frame vertically, so its corner sits further off
        // the axis than a 16:9 corner at the same horizontal FOV.
        let wide = on_screen_half_angle(90.0, 1920, 1080);
        let tall = on_screen_half_angle(90.0, 1280, 960);
        assert!(tall > wide, "4:3 {:.1} should exceed 16:9 {:.1}", tall, wide);
    }

    #[test]
    fn a_nonsense_resolution_falls_back_rather_than_dividing_by_zero() {
        let half = on_screen_half_angle(90.0, 0, 0);
        assert!(half.is_finite() && half > 45.0);
    }

    #[test]
    fn mirv_fov_in_the_init_commands_wins() {
        // The capture runs at whatever the console was last told, so reading it
        // from there is the only way the two cannot disagree.
        let mut config = PatcherConfig {
            capture_fov: 90.0,
            ..PatcherConfig::default()
        };
        assert_eq!(capture_fov(&config), 90.0);

        config.init_commands = vec!["exec autoexec".to_string(), "mirv_fov 105".to_string()];
        assert_eq!(capture_fov(&config), 105.0);

        // Last one wins, as at the console.
        config.init_commands.push("mirv_fov 100".to_string());
        assert_eq!(capture_fov(&config), 100.0);
    }

    #[test]
    fn a_command_that_merely_starts_the_same_is_not_mistaken_for_it() {
        let config = PatcherConfig {
            capture_fov: 90.0,
            init_commands: vec!["mirv_fov_something_else 12".to_string()],
            ..PatcherConfig::default()
        };
        assert_eq!(capture_fov(&config), 90.0);
    }

    #[test]
    fn the_pipeline_derives_its_cone_from_the_capture_fov() {
        // The whole point: a movie shot at mirv_fov 105 must not be planned
        // against a 90-degree frame.
        let config = PatcherConfig {
            resolution_width: 1920,
            resolution_height: 1080,
            init_commands: vec!["mirv_fov 105".to_string()],
            ..PatcherConfig::default()
        };
        let opts = flush_options(&config);
        assert!(
            (opts.visibility_cone_degrees - on_screen_half_angle(105.0, 1920, 1080)).abs() < 0.01
        );
        assert!(opts.visibility_cone_degrees > 55.0);
    }

    #[test]
    fn the_sweep_takes_only_its_own_stale_leavings() {
        // This deletes files, so what it will not touch matters more than what
        // it will. A concurrent capture's scratch is young; everything else in
        // the temp directory is not ours at any age.
        use std::time::{Duration, SystemTime};

        let dir = std::env::temp_dir().join("dod_sweep_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let stale = dir.join(format!("{}old_1_0.dem", SCRATCH_PREFIX));
        let fresh = dir.join(format!("{}live_2_0.dem", SCRATCH_PREFIX));
        let other = dir.join("someone_elses_file.dem");
        for p in [&stale, &fresh, &other] {
            std::fs::write(p, b"x").unwrap();
        }

        // Backdate the stale one well past the threshold.
        let long_ago = SystemTime::now() - SCRATCH_STALE_AFTER - Duration::from_secs(60);
        filetime::set_file_mtime(&stale, filetime::FileTime::from_system_time(long_ago)).unwrap();

        sweep_stale_scratch(&dir);

        assert!(!stale.exists(), "an orphaned scratch demo should be removed");
        assert!(
            fresh.exists(),
            "a scratch demo young enough to belong to a running capture must survive"
        );
        assert!(other.exists(), "files that are not ours must never be touched");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A survey with one camera in the room, facing away from the only wall in
    /// it — so nothing on that wall is ever on screen and the camera test is
    /// not what any of these assertions are about.
    fn survey_facing_away(harvested: Vec<[f32; 3]>) -> Survey {
        Survey {
            harvested,
            world_harvested: Vec::new(),
            texture_index: Some(0),
            spawn_eye: None,
            grounded_origin: None,
            floor_candidates: Vec::new(),
            window_cameras: vec![([50.0, 32.0, 32.0], [-1.0, 0.0, 0.0])],
        }
    }

    #[test]
    fn the_map_is_not_scanned_when_the_demo_can_fill_the_sweep_itself() {
        // The laziness is the design, not an optimisation detail. Sampling a
        // map's faces and tracing each one against every camera costs real time
        // on every demo, and on a map whose coordinate store is populated it
        // buys nothing — the proven sources have already filled the pool. A
        // regression here is invisible in the output and shows up only as the
        // pre-pass getting slower.
        let map = bsp::one_wall_room();
        let opts = DecalCleanOptions::default();
        let survey = survey_facing_away(vec![[98.0, 20.0, 20.0], [98.0, 40.0, 40.0]]);
        let visibility = Visibility::new(Some(&map), &survey.window_cameras, &opts);

        let placement = resolve_flush_positions(&survey, &[], &visibility, &opts, 2);

        assert_eq!(placement.positions.len(), 2);
        assert_eq!(
            placement.map_sampled, 0,
            "the map was scanned even though the demo's own decals sufficed"
        );
    }

    #[test]
    fn a_demo_that_proves_nothing_is_carried_by_the_map_itself() {
        // The case the source exists for: a map nothing has been harvested
        // from, where every source drawn from the match comes back empty. The
        // map still has walls.
        let map = bsp::one_wall_room();
        let opts = DecalCleanOptions::default();
        let survey = survey_facing_away(Vec::new());
        let visibility = Visibility::new(Some(&map), &survey.window_cameras, &opts);

        let placement = resolve_flush_positions(&survey, &[], &visibility, &opts, 2);

        assert!(placement.map_sampled > 0, "the map was never consulted");
        assert_eq!(placement.positions.len(), 2);
        assert_eq!(placement.source, Some(FlushSource::MapGeometry));
        for p in &placement.positions {
            assert!(
                (p[0] - 98.0).abs() < 1e-3,
                "off the wall and into the room: {:?}",
                p
            );
        }
    }

    #[test]
    fn the_gate_with_no_map_places_nothing_rather_than_guessing() {
        // Running the gate on a machine where the map is not installed leaves
        // it with no source at all. The honest outcome is an empty placement,
        // which the caller reports as a partial sweep — not a quiet fallback to
        // the spawn floor, which would put the decals on ground the player
        // walks and look exactly like the map source failing in game.
        let opts = DecalCleanOptions {
            map_geometry_only: true,
            ..Default::default()
        };
        let mut survey = survey_facing_away(vec![[98.0, 20.0, 20.0]]);
        survey.grounded_origin = Some([50.0, 32.0, 32.0]);
        let visibility = Visibility::new(None, &survey.window_cameras, &opts);

        let placement = resolve_flush_positions(&survey, &[], &visibility, &opts, 2);

        assert!(placement.positions.is_empty(), "{:?}", placement.positions);
        assert_eq!(placement.source, None);
    }

    #[test]
    fn the_experiment_gate_refuses_everything_the_match_proved() {
        // `DOD_FLUSH_MAP_GEOMETRY_ONLY` exists to put the map source in front
        // of someone watching the game. It is worthless if a harvested decal
        // can still slip into the pool and be the thing they end up looking at.
        let map = bsp::one_wall_room();
        let opts = DecalCleanOptions {
            map_geometry_only: true,
            ..Default::default()
        };
        let survey = survey_facing_away(vec![[98.0, 20.0, 20.0], [98.0, 40.0, 40.0]]);
        let visibility = Visibility::new(Some(&map), &survey.window_cameras, &opts);

        let placement = resolve_flush_positions(&survey, &[], &visibility, &opts, 2);

        assert_eq!(placement.source, Some(FlushSource::MapGeometry));
        assert_eq!(
            placement.tiled_safe, 0,
            "the proven sources were still consulted under the gate"
        );
    }
}
