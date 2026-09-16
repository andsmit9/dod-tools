// patch/decal_probe.rs
// Measures the one number the flush design is currently guessing at: how far a
// decal message's position may sit from a solid surface and still create a
// decal.
//
// ── Why it matters ───────────────────────────────────────────────────────────
// The ring sweep in `decal_strip.rs` can only place flush decals at positions
// the demo hands it — real decal positions it harvests, or floor points under
// the player's own path. On a 20-minute match demo that yields ~30 usable
// spots against the 68 a 256-slot ring needs, and relaxing the camera filters
// does not help: line of sight is the binding constraint, not proximity.
//
// If positions could instead be SYNTHESISED near one known-good surface point,
// supply stops being a constraint entirely and every spot can be chosen far
// from the lens. Whether that works hinges on the engine's tolerance:
//
//     R_DecalShoot walks the BSP from the world root:
//         dist = PlaneDiff(m_Position, node->plane)
//         if      dist >  m_Size  -> descend front
//         else if dist < -m_Size  -> descend back
//         else                    -> apply to surfaces on this node
//
// Nothing there consults the camera, the PVS or the viewer — decal creation
// happens at message-parse time, so a flush decal never needs to be *rendered*
// to consume a ring slot. The only constraint is `m_Size`: the position must
// land within roughly a decal's own radius of solid geometry. That radius has
// been estimated at 8-16 units from texture dimensions and never measured.
//
// ── How this measures it ─────────────────────────────────────────────────────
// Strip every decal out of the demo, then stamp a three-row grid onto patches
// of surface the demo proves exist, and pin r_decals high enough that nothing
// can evict anything:
//
//     OUT row   offsets pushed away from the surface, into the room
//     CTL row   dead on the surface — the control
//     IN  row   the same offsets pushed into the solid
//
// Column j of every row uses offset `offsets[j]`, ascending, so a row simply
// runs out of holes at the point the engine stopped accepting the position.
// The readout is three counts. The control row proves each column has surface
// behind it; if it comes up short, the grid overran an edge and the run is void
// rather than silently misread.
//
// The measurement is symmetric on purpose. `PlaneDiff` compares |dist| against
// m_Size in both directions, so a position buried in solid should fail exactly
// as a position floating in air does — and an inward offset is the more useful
// half for production, since a synthesised spot nudged INTO a wall cannot stick
// out of it visually.
//
// ── Why several grids, not one ───────────────────────────────────────────────
// A POV demo cannot be steered. The viewer sees exactly what the recorded
// player saw, so "play to 7:29 and look at the wall" only works if the player
// happened to face it and the viewer recognises which of the walls in frame is
// the one. Stamping the same grid onto several separate patches makes that a
// non-problem: whichever one the viewer spots first is a complete, independent
// measurement of the same engine constant, and two that agree are worth more
// than one that cannot be found.

use dem::open_demo_from_bytes;
use dem::types::{
    ByteString, ConsoleCommand, EngineMessage, Frame, FrameData, MessageData, NetMessage,
    TempEntity,
};

use super::decal_strip::MAX_OVERLAP_DECALS;
use super::decal_strip::{
    build_world_decal, cluster, connected_patches, decal_texture_index, distance, extent,
    frame_ordinals, strip_decal_messages, survey, tangent_axes, DecalCleanOptions,
};

/// Every decal texture index the demo actually uses, commonest first.
///
/// Worth knowing because a grid of identical holes reads as ordinary gunfire at
/// a glance — the first in-game attempt at finding one came back as "I see
/// bullets in a lot of places", which is exactly what 200 identical holes look
/// like. Giving each row its own mark makes the grid obviously deliberate and
/// makes the three counts impossible to conflate.
pub fn decal_texture_histogram(demo_bytes: &[u8]) -> Result<Vec<(u8, usize)>, String> {
    let demo = open_demo_from_bytes(demo_bytes)
        .map_err(|e| format!("Could not parse demo file: {}", e))?;
    let mut counts: std::collections::BTreeMap<u8, usize> = std::collections::BTreeMap::new();

    for entry in &demo.directory.entries {
        for frame in &entry.frames {
            let FrameData::NetworkMessage(net_msg_box) = &frame.frame_data else {
                continue;
            };
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
                let payload: &[u8] = match &te.entity {
                    TempEntity::TeWorldDecal(p)
                    | TempEntity::TeWorldDecalHigh(p)
                    | TempEntity::TeGunshotDecal(p)
                    | TempEntity::TeDecal(p)
                    | TempEntity::TeDecalHigh(p) => p,
                    _ => continue,
                };
                if let Some(idx) = decal_texture_index(te.entity_type, payload) {
                    *counts.entry(idx).or_default() += 1;
                }
            }
        }
    }

    let mut out: Vec<(u8, usize)> = counts.into_iter().collect();
    out.sort_by_key(|&(_, count)| std::cmp::Reverse(count));
    Ok(out)
}

/// Offsets tested, in world units, ascending. Ascending order is what makes a
/// row's hole count readable as a threshold rather than a pattern to decode.
const DEFAULT_OFFSETS: &[f32] = &[2.0, 4.0, 8.0, 12.0, 16.0, 24.0, 32.0, 48.0];

/// Gap between adjacent columns, along the surface. Wide enough that two holes
/// never read as one, and comfortably past the engine's overlap radius so no
/// probe can be lost to recycling instead of to the offset under test.
const DEFAULT_COLUMN_SPACING: f32 = 40.0;

/// Floor on column pitch when a patch is too narrow to seat every column at the
/// requested spacing. Matches the flush burst's own overlap floor.
const MIN_COLUMN_SPACING: f32 = 28.0;

/// Gap between the three rows, across the surface.
const DEFAULT_ROW_GAP: f32 = 40.0;

/// Grids stamped by default. Each is a full independent measurement; more of
/// them means more chances the POV camera turns toward one.
const DEFAULT_GRIDS: usize = 4;

/// How far apart two grids must be to count as different walls worth having
/// both of.
const GRID_SEPARATION: f32 = 600.0;

/// Cap on synthetic decals added to any single network packet, matching the
/// flush burst's own limit.
const MAX_PER_FRAME: usize = 4;

#[derive(Debug, Clone)]
pub struct ProbeOptions {
    /// Offsets from the surface to test, one column each. Must be ascending.
    pub offsets: Vec<f32>,
    pub column_spacing: f32,
    pub row_gap: f32,
    /// How many separate patches of surface to stamp the grid onto.
    pub grids: usize,
    /// Decal texture index, overriding the one harvested from the demo. A
    /// small bullet hole reads far better in a grid than a grenade scorch.
    pub texture_index: Option<u8>,
    /// A distinct decal texture per row, as [OUT, CTL, IN].
    ///
    /// Three identical rows of identical holes read as ordinary gunfire at a
    /// glance — the first attempt at finding one in game came back as "I see
    /// bullets in a lot of places", which is exactly what 200 identical holes
    /// look like. Three different marks make the grid obviously deliberate and
    /// make the three counts impossible to conflate.
    pub row_textures: Option<[u8; 3]>,
    /// Texture for a beacon placed clear of the grid: a short line of large,
    /// dark marks that makes a grid findable from across a room.
    ///
    /// Kept separate from the measurement rows on purpose. A grenade scorch is
    /// what makes a grid visible at distance, but scorches are wide, and nine
    /// of them at a 28-unit pitch merge into one smear that cannot be counted
    /// — and worse, may overlap more than MAX_OVERLAP_DECALS, at which point
    /// the engine recycles instead of allocating and holes vanish for reasons
    /// having nothing to do with the offset under test. Findability and
    /// countability want different marks, so they get different marks.
    pub beacon_texture: Option<u8>,
    /// Stamp only one of the three rows.
    ///
    /// Three rows in one demo is one playback, but it asks the viewer to hold
    /// three counts at once, work out which line is which, and see past
    /// whatever is lying on the floor across two of them. One row per demo
    /// costs three playbacks and removes every one of those ambiguities: a
    /// single line of holes, count it.
    pub only_row: Option<ProbeRow>,
    /// Slide the grid along the surface, in units, positive being rightward as
    /// seen from the best viewing position.
    ///
    /// Which world axis that is depends on where the camera stands, so the
    /// sign is resolved against that camera rather than asked of the caller.
    /// Useful for nudging a grid clear of whatever happens to be lying in
    /// front of it — a body on the floor hides a row as effectively as a wall.
    pub shift_right: f32,
    /// Decals stacked at each position, to darken a mark without widening it.
    ///
    /// A single small hole disappears into a speckled texture — sand especially.
    /// Stacking a few at the identical coordinate deepens the mark while
    /// leaving its footprint unchanged, which a bigger texture would not: wide
    /// marks merge with their neighbours and become uncountable. Capped below
    /// MAX_OVERLAP_DECALS, since the stack overlaps itself and the engine
    /// recycles rather than allocates past that.
    pub stack: usize,
    /// Force the surface's normal axis (0=x, 1=y, 2=z) instead of detecting it.
    pub axis: Option<usize>,
    /// Hand-picked anchor point on a known surface, overriding detection
    /// entirely. Requires `axis` so the offset direction is defined.
    pub anchor: Option<[f32; 3]>,
    /// How far apart two decals may sit along the normal and still count as
    /// coplanar. Real decals land on the surface itself, so this only absorbs
    /// coordinate quantisation (1/8 unit) and slight surface relief.
    pub plane_tolerance: f32,
    /// How close two coplanar decals must be to count as being on the same
    /// patch of surface.
    pub link_radius: f32,
    /// Decals that must share a patch before it is worth measuring on.
    pub min_plane_decals: usize,
    /// Extent a patch must span to be worth measuring on, rather than a tight
    /// cluster on some small prop.
    pub min_plane_spread: f32,
    /// Pinned r_decals. Wants to be large: the grids must survive every
    /// client-side decal the POV player's own gunfire creates, and those are
    /// predicted locally so stripping cannot touch them.
    pub ring_limit: u32,
    /// Blank every decal message in the demo, so the grids are the only thing
    /// on a wall that arrived over the wire.
    pub strip_all: bool,
    /// Half-angle of the cone counted as "looking at it" when hunting for
    /// timestamps to hand the user.
    pub sighting_cone_degrees: f32,
    /// Range past which a grid is too small on screen to be counted.
    pub sighting_max_distance: f32,
    /// The camera must physically come within this of a patch at some point
    /// for it to be worth stamping.
    ///
    /// Standing in for an occlusion test, which is not available here: the
    /// sighting cone knows the camera was pointed at a patch but not whether
    /// anything was in between, so it happily reports a wall two rooms away as
    /// visible. Having walked within a couple of hundred units of a surface is
    /// weak evidence of facing it but strong evidence of having been in the
    /// same space as it, which is the half the cone test cannot supply.
    pub require_approach: f32,
    /// Restrict patches to within `near_radius` of this point.
    pub near: Option<[f32; 3]>,
    /// Restrict grids to wherever the camera was at this viewdemo time, in
    /// seconds. Easier to supply than a coordinate when the spot is something
    /// you can see on screen but not measure.
    pub near_at: Option<f32>,
    pub near_radius: f32,
    /// Restrict patches to the spawn area — the demo's own settled spawn
    /// origin, within `near_radius`.
    ///
    /// Worth having as a default in practice: a POV demo opens in spawn, and a
    /// dead player spectates teammates who are themselves in spawn, so spawn
    /// walls get more camera time from closer range than anywhere else on the
    /// map. A grid there is seen many times over; a grid on a wall the player
    /// passes twice may never be looked at squarely.
    pub spawn_only: bool,
}

impl Default for ProbeOptions {
    fn default() -> Self {
        Self {
            offsets: DEFAULT_OFFSETS.to_vec(),
            column_spacing: DEFAULT_COLUMN_SPACING,
            row_gap: DEFAULT_ROW_GAP,
            grids: DEFAULT_GRIDS,
            texture_index: None,
            row_textures: None,
            beacon_texture: None,
            only_row: None,
            shift_right: 0.0,
            stack: 1,
            axis: None,
            anchor: None,
            plane_tolerance: 2.0,
            link_radius: 160.0,
            min_plane_decals: 6,
            min_plane_spread: 96.0,
            ring_limit: 4096,
            strip_all: true,
            sighting_cone_degrees: 25.0,
            sighting_max_distance: 900.0,
            require_approach: 250.0,
            near: None,
            near_at: None,
            near_radius: 700.0,
            spawn_only: true,
        }
    }
}

/// One injected probe, kept so the caller can print a map of what to expect on
/// the surface next to what it means.
#[derive(Debug, Clone, Copy)]
pub struct Probe {
    pub row: ProbeRow,
    pub column: usize,
    pub offset: f32,
    pub position: [f32; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeRow {
    /// Pushed away from the surface, into open space.
    Out,
    /// Dead on the surface. Every one of these must appear.
    Control,
    /// Pushed into the solid behind the surface.
    In,
}

impl ProbeRow {
    pub fn label(self) -> &'static str {
        match self {
            ProbeRow::Out => "OUT",
            ProbeRow::Control => "CTL",
            ProbeRow::In => "IN ",
        }
    }
}

/// A moment the POV camera is pointed at a grid.
#[derive(Debug, Clone, Copy)]
pub struct Sighting {
    /// Viewdemo clock, in seconds — the number the scrub bar shows.
    pub svc_time: f32,
    pub distance: f32,
    /// How far off the centre of frame the grid sits. A POV demo cannot be
    /// steered, so this is the difference between "it fills your view" and
    /// "it is somewhere off to the side".
    pub off_axis_degrees: f32,
    /// Signed bearing across the frame: negative is left of centre, positive
    /// right. World coordinates are no use to someone watching a demo — "left
    /// of centre, above eye level" is something you can act on, "Y = 931.9" is
    /// not.
    pub yaw_degrees: f32,
    /// Signed bearing up the frame: negative is below eye level, positive
    /// above.
    pub pitch_degrees: f32,
}

/// One stamped grid and everything needed to find and read it.
#[derive(Debug, Clone)]
pub struct GridStats {
    /// Normal axis of the surface: 0=x, 1=y, 2=z.
    pub axis: usize,
    /// Coordinate of the surface along that axis.
    pub plane_value: f32,
    /// Decals proving that patch of surface exists.
    pub plane_members: usize,
    /// Extent those decals span along the patch's two in-plane axes.
    pub plane_spread: (f32, f32),
    /// +1 or -1: which way along the normal axis open space lies.
    pub outward: f32,
    /// Centre of the grid.
    pub anchor: [f32; 3],
    /// In-plane axis the columns run along, and the one the rows step across.
    pub column_axis: usize,
    pub row_axis: usize,
    pub column_pitch: f32,
    /// Distance from each column to the nearest real decal on the same patch.
    pub column_evidence: Vec<f32>,
    /// Columns with a real decal within half a pitch of them. Anything less
    /// than every column means part of the grid is placed by arithmetic.
    pub columns_backed: usize,
    pub probes: Vec<Probe>,
    /// Beacon mark positions, if one was placed. Projected alongside the holes
    /// as a calibration check: they are large and unmistakable, so if they land
    /// where a screenshot shows them the projection is right and a missing hole
    /// is a real miss.
    pub beacon: Vec<[f32; 3]>,
    /// Distinct moments the camera is pointed at this grid, best first.
    pub sightings: Vec<Sighting>,
    /// Closest the camera ever physically gets to this grid, whether or not it
    /// is facing it.
    pub closest_approach: f32,
    /// Camera samples spent within `require_approach` of it. The best proxy
    /// available for "how often will this be in front of you".
    pub dwell_samples: usize,
    /// Viewdemo time at which the camera watched a bullet land on this exact
    /// spot, if it ever did. The grid is centred there when so.
    pub witness_time: Option<f32>,
    /// Where the grid sits in frame at the witness moment, as (yaw, pitch,
    /// distance). That is the timestamp the viewer is sent to, so it is the
    /// one view worth describing — and it makes a requested nudge visible in
    /// the output instead of something to take on trust.
    pub witness_view: Option<(f32, f32, f32)>,
    /// Geometry near the fitted plane over the grid's footprint: how many
    /// harvested decals fall there and how far they spread along the normal.
    ///
    /// Read this as clutter, NOT as a flatness test — it was built as one and
    /// does not work as one. Decals exist only where people shot, so absence
    /// proves nothing about the surface, and a decal off the plane usually
    /// means a crate or sandbag in front of it rather than the plane being
    /// wrong. For the flush that is harmless or better: a position that finds
    /// a crate instead of the wall still creates a decal and still turns the
    /// ring. What actually threatens a tiled position is open space, and the
    /// metric for that is `columns_backed`.
    ///
    /// Whether a fitted plane tracks a sloped surface can only be settled in
    /// game, by tiling a wide control row across it and counting.
    pub local_relief: (usize, f32),
    /// Where the OUT row sits relative to the CTL row as seen from the best
    /// view, as (forward, right, up) components in that camera's own basis.
    ///
    /// With all three rows wearing the same mark there is nothing on screen to
    /// say which line is which, and "the row displaced along +Y" is not
    /// something a viewer can act on. This turns it into near/far or
    /// left/right from where they will actually be standing.
    pub out_row_in_view: Option<[f32; 3]>,
    /// Whether this grid fell inside the spawn (or --near) restriction.
    pub in_region: bool,
}

#[derive(Debug, Clone)]
pub struct ProbeStats {
    pub harvested_decals: usize,
    pub texture_index: u8,
    /// Region grids were restricted to, when one applied.
    pub region: Option<[f32; 3]>,
    pub region_radius: f32,
    /// True when no patch met the region restriction and it had to be dropped.
    pub region_abandoned: bool,
    pub grids: Vec<GridStats>,
    pub decals_injected: usize,
    pub decals_stripped: usize,
    pub sprays_stripped: usize,
    pub injected_at_ordinal: i32,
    /// Viewdemo time the grids appear at. They go in once, as early as is
    /// safe, and nothing evicts them — so this is when they start existing,
    /// not when any particular one first comes into view.
    pub injected_at_time: f32,
}

/// A camera sample with enough context to both aim a grid and time it.
struct CameraSample {
    ordinal: i32,
    eye: [f32; 3],
    forward: [f32; 3],
    svc_time: f32,
}

fn norm(v: &[f32; 3]) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

fn dot(a: &[f32; 3], b: &[f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: &[f32; 3], b: &[f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalized(v: &[f32; 3]) -> Option<[f32; 3]> {
    let n = norm(v);
    (n > 1.0e-4).then(|| [v[0] / n, v[1] / n, v[2] / n])
}

/// Where `point` sits in `cam`'s frame: distance, angle off centre, and signed
/// left/right and up/down bearings.
///
/// The camera's own `right` vector is ignored in favour of deriving it from
/// `forward`. GoldSrc's `AngleVectors` has a long history of handing back a
/// "right" that different codebases negate, and getting left and right the
/// wrong way round is worse than useless when the whole point is telling
/// someone where to look. `forward x worldUp` is unambiguous: in GoldSrc's
/// right-handed, Z-up world a player facing +X has +Y on their left, and that
/// cross product yields -Y.
fn look_at(
    cam: &CameraSample,
    point: &[f32; 3],
    opts: &ProbeOptions,
) -> Option<(f32, f32, f32, f32)> {
    let v = [
        point[0] - cam.eye[0],
        point[1] - cam.eye[1],
        point[2] - cam.eye[2],
    ];
    let d = norm(&v);
    if d < 24.0 || d > opts.sighting_max_distance {
        return None;
    }
    let fwd = normalized(&cam.forward)?;
    let cos_off = (dot(&v, &fwd) / d).clamp(-1.0, 1.0);
    if cos_off < opts.sighting_cone_degrees.to_radians().cos() {
        return None;
    }

    // Straight up or down leaves no horizontal heading to measure against, so
    // report the bearing as dead ahead rather than inventing a side.
    let right = normalized(&cross(&fwd, &[0.0, 0.0, 1.0]));
    let (yaw, pitch) = match right {
        Some(right) => {
            let up = cross(&right, &fwd);
            let (f, r, u) = (dot(&v, &fwd), dot(&v, &right), dot(&v, &up));
            (
                r.atan2(f).to_degrees(),
                u.atan2((f * f + r * r).sqrt()).to_degrees(),
            )
        }
        None => (0.0, 0.0),
    };

    Some((d, cos_off.acos().to_degrees(), yaw, pitch))
}

/// Moments the camera is pointed at `anchor`, best first.
///
/// Ranked by how much of the frame the grid would occupy — near and centred
/// beats far and peripheral — because the viewer cannot steer toward it and
/// only gets whatever the recorded player happened to look at. Consecutive
/// samples during one long look are collapsed into a single sighting.
fn sightings_for(anchor: &[f32; 3], cameras: &[CameraSample], opts: &ProbeOptions) -> Vec<Sighting> {
    let mut chronological: Vec<Sighting> = Vec::new();
    let mut last = f32::NEG_INFINITY;
    let mut best_of_run: Option<Sighting> = None;

    for cam in cameras {
        let Some((distance, off_axis_degrees, yaw_degrees, pitch_degrees)) =
            look_at(cam, anchor, opts)
        else {
            continue;
        };
        let s = Sighting {
            svc_time: cam.svc_time,
            distance,
            off_axis_degrees,
            yaw_degrees,
            pitch_degrees,
        };

        if cam.svc_time - last > 3.0 {
            if let Some(prev) = best_of_run.take() {
                chronological.push(prev);
            }
            best_of_run = Some(s);
        } else if best_of_run
            .map(|b| quality(&s) > quality(&b))
            .unwrap_or(true)
        {
            // Keep the best frame of the look, not its first — the player
            // usually swings past a wall before settling on it.
            best_of_run = Some(s);
        }
        last = cam.svc_time;
    }
    if let Some(prev) = best_of_run {
        chronological.push(prev);
    }

    chronological.sort_by(|a, b| {
        quality(b)
            .partial_cmp(&quality(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    chronological
}

/// Rough "how big and how centred", used only to rank sightings against each
/// other.
fn quality(s: &Sighting) -> f32 {
    (1.0 - s.off_axis_degrees / 90.0).max(0.0) * 1000.0 / s.distance.max(1.0)
}

/// Closest the camera ever gets to `point`, and how many samples it spends
/// within `radius` of it — regardless of where it is facing.
fn approach(point: &[f32; 3], cameras: &[CameraSample], radius: f32) -> (f32, usize) {
    let mut closest = f32::INFINITY;
    let mut dwell = 0usize;
    for cam in cameras {
        let d = distance(&cam.eye, point);
        closest = closest.min(d);
        if d <= radius {
            dwell += 1;
        }
    }
    (closest, dwell)
}

/// The patch of surface a grid gets stamped onto, and the layout chosen for it.
struct Target {
    /// Normal axis: 0=x, 1=y, 2=z.
    axis: usize,
    /// Coordinate of the surface along that axis.
    value: f32,
    /// In-plane axis the columns run along, and the one the rows step across.
    col_axis: usize,
    row_axis: usize,
    /// Column positions along `col_axis`, evenly spaced.
    columns: Vec<f32>,
    /// Distance from each column to the nearest real decal.
    evidence: Vec<f32>,
    /// Columns with a real decal within half a pitch of them.
    backed: usize,
    /// Pitch those columns ended up at.
    pitch: f32,
    /// Middle row's coordinate along `row_axis`.
    row_center: f32,
    /// Decals proving this patch of surface exists.
    members: Vec<[f32; 3]>,
    /// Extent those decals span along the two in-plane axes.
    spread: (f32, f32),
    /// Viewdemo time at which the camera watched a bullet land on this patch,
    /// if it ever did. The strongest evidence available that the wall is
    /// visible: not "the camera pointed this way" but "the player saw a hit
    /// here".
    witness_time: Option<f32>,
}

/// Re-lays a target's columns centred on a point, keeping the pitch.
///
/// Used to sit the grid on the exact spot a bullet was seen landing, rather
/// than wherever on the patch the densest stretch happened to be. A wall can be
/// 400 units wide with only one end ever in view.
fn centre_on(target: &mut Target, point: &[f32; 3], band: &[[f32; 3]], columns: usize) {
    let first = -(columns as f32 - 1.0) / 2.0;
    target.columns = (0..columns)
        .map(|j| point[target.col_axis] + (first + j as f32) * target.pitch)
        .collect();
    target.row_center = point[target.row_axis];

    let coords: Vec<f32> = band.iter().map(|m| m[target.col_axis]).collect();
    target.evidence = target
        .columns
        .iter()
        .map(|c| {
            coords
                .iter()
                .map(|d| (d - c).abs())
                .fold(f32::INFINITY, f32::min)
        })
        .collect();
    target.backed = target
        .evidence
        .iter()
        .filter(|e| **e <= target.pitch / 2.0)
        .count();
}

impl Target {
    /// Centre of the grid this target will carry.
    fn anchor(&self) -> [f32; 3] {
        let mut a = [0.0f32; 3];
        a[self.axis] = self.value;
        a[self.col_axis] = self.columns.iter().sum::<f32>() / self.columns.len() as f32;
        a[self.row_axis] = self.row_center;
        a
    }
}

/// Centre of the `+/- half` window containing the most values.
fn densest_band(values: &[f32], half: f32) -> Option<f32> {
    let mut best: Option<(usize, f32)> = None;
    for &centre in values {
        let n = values.iter().filter(|v| (**v - centre).abs() <= half).count();
        if best.map(|(b, _)| n > b).unwrap_or(true) {
            best = Some((n, centre));
        }
    }
    best.map(|(_, c)| c)
}

/// One candidate row of columns, and how well the demo backs it.
struct Layout {
    columns: Vec<f32>,
    /// Distance from each column to the nearest real decal along the same axis.
    /// A column within half a pitch of one has surface either side of it in the
    /// demo's own evidence; anything further is arithmetic and might be hanging
    /// over a doorway.
    evidence: Vec<f32>,
    backed: usize,
    pitch: f32,
}

/// Lays an evenly spaced row of columns across the stretch of a patch with the
/// most real decals in it.
///
/// Even spacing rather than snapping each column onto a real decal, which was
/// the first attempt: snapping produced columns at Y = -144, -109, -69, 62, 95,
/// ... — a 131-unit hole in the middle of an otherwise regular row. The whole
/// readout is "count the holes", and a row with a visible gap in it invites
/// exactly the miscount the design exists to avoid. So the grid stays regular
/// and each column instead carries how far it sits from the nearest decal, so a
/// stretch the demo cannot vouch for is reported rather than hidden.
///
/// Pitch is relaxed toward `MIN_COLUMN_SPACING` only when the wider grid cannot
/// be fully backed: a tighter grid fits inside a denser stretch of wall.
fn lay_columns(band: &[[f32; 3]], col_axis: usize, wanted: usize, spacing: f32) -> Option<Layout> {
    let mut coords: Vec<f32> = band.iter().map(|m| m[col_axis]).collect();
    coords.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let (lo, hi) = (*coords.first()?, *coords.last()?);

    let mut best: Option<(usize, f32, Layout)> = None;
    let mut pitch = spacing;

    while pitch >= MIN_COLUMN_SPACING {
        let span = pitch * (wanted as f32 - 1.0);
        // Every real decal is tried as the row's left end, plus the placement
        // that centres the row on the patch. That is enough candidates to find
        // the densest stretch without sweeping continuously.
        let mut starts: Vec<f32> = coords.clone();
        starts.push((lo + hi - span) / 2.0);

        for start in starts {
            let columns: Vec<f32> = (0..wanted).map(|j| start + j as f32 * pitch).collect();
            let evidence: Vec<f32> = columns
                .iter()
                .map(|c| {
                    coords
                        .iter()
                        .map(|d| (d - c).abs())
                        .fold(f32::INFINITY, f32::min)
                })
                .collect();
            let backed = evidence.iter().filter(|e| **e <= pitch / 2.0).count();
            let slack: f32 = evidence.iter().map(|e| e.min(pitch)).sum();

            let better = best
                .as_ref()
                .map(|(b, s, _)| backed > *b || (backed == *b && slack < *s))
                .unwrap_or(true);
            if better {
                best = Some((
                    backed,
                    slack,
                    Layout {
                        columns,
                        evidence,
                        backed,
                        pitch,
                    },
                ));
            }
        }

        if best.as_ref().map(|(b, _, _)| *b == wanted).unwrap_or(false) {
            break;
        }
        pitch -= 4.0;
    }

    best.map(|(_, _, l)| l)
}

/// Builds one candidate target from a connected patch, or nothing if the patch
/// cannot carry a grid.
fn target_from_patch(
    patch: Vec<[f32; 3]>,
    axis: usize,
    value: f32,
    opts: &ProbeOptions,
) -> Option<Target> {
    let (t1, t2) = tangent_axes(axis);
    if patch.len() < opts.min_plane_decals {
        return None;
    }
    let spread = (extent(&patch, t1), extent(&patch, t2));
    if spread.0.max(spread.1) < opts.min_plane_spread {
        return None;
    }

    // Columns run along whichever in-plane axis the patch is widest in; for an
    // upright wall that is the horizontal one, which puts the rows above one
    // another where they read as rows.
    let (col_axis, row_axis) = if spread.0 >= spread.1 {
        (t1, t2)
    } else {
        (t2, t1)
    };

    // Band the rows will occupy: the stretch of `row_axis` holding the most
    // decals, so there is evidence of surface above and below the middle row as
    // well as along it.
    let row_values: Vec<f32> = patch.iter().map(|m| m[row_axis]).collect();
    let band_half = opts.row_gap * 1.5;
    let row_center = densest_band(&row_values, band_half)?;
    let band: Vec<[f32; 3]> = patch
        .iter()
        .copied()
        .filter(|m| (m[row_axis] - row_center).abs() <= band_half)
        .collect();

    let layout = lay_columns(&band, col_axis, opts.offsets.len(), opts.column_spacing)?;

    Some(Target {
        axis,
        value,
        col_axis,
        row_axis,
        columns: layout.columns,
        evidence: layout.evidence,
        backed: layout.backed,
        pitch: layout.pitch,
        row_center,
        members: patch,
        spread,
        witness_time: None,
    })
}

/// Picks the patches of surface to measure on, best first.
///
/// Only harvested decal positions are eligible. They are the one input that
/// involves no derivation at all — the engine created a decal there, so the
/// surface is proven to the exact coordinate. Floor points under the player's
/// path would be plentiful and their surfaces real, but they are computed by
/// dropping a fixed offset below the player's origin, and a wrong offset would
/// bias every measurement here by that amount. A measuring instrument does not
/// get to guess at its own zero.
///
/// Among the patches that qualify, the ones the camera actually looks at win. A
/// grid nobody can find measures nothing.
fn choose_targets(
    harvested: &[[f32; 3]],
    cameras: &[CameraSample],
    witnessed: &[([f32; 3], f32)],
    opts: &ProbeOptions,
    region: Option<[f32; 3]>,
    already: &[[f32; 3]],
) -> Vec<Target> {
    let mut scored: Vec<(f32, Target)> = Vec::new();

    for axis in 0..3 {
        if let Some(forced) = opts.axis
            && axis != forced {
                continue;
            }
        let values: Vec<f32> = harvested.iter().map(|p| p[axis]).collect();

        for (value, idxs) in cluster(&values, opts.plane_tolerance) {
            if idxs.len() < opts.min_plane_decals {
                continue;
            }
            let coplanar: Vec<[f32; 3]> = idxs.iter().map(|&i| harvested[i]).collect();

            for patch in connected_patches(&coplanar, opts.link_radius) {
                let Some(mut target) = target_from_patch(patch, axis, value, opts) else {
                    continue;
                };

                // If the camera ever watched a bullet land on this patch, sit
                // the grid on that exact spot. Everything else here is inference
                // about what can be seen; this is a spot the player provably
                // saw, and a wall can be 400 units wide with only one end ever
                // in view.
                if let Some((point, time)) = witnessed
                    .iter()
                    .filter(|(p, _)| {
                        (p[target.axis] - target.value).abs() <= opts.plane_tolerance
                            && target
                                .members
                                .iter()
                                .any(|m| distance(m, p) <= opts.link_radius)
                    })
                    .min_by(|a, b| {
                        distance(&a.0, &target.anchor())
                            .partial_cmp(&distance(&b.0, &target.anchor()))
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .copied()
                {
                    let band: Vec<[f32; 3]> = target.members.clone();
                    centre_on(&mut target, &point, &band, opts.offsets.len());
                    target.witness_time = Some(time);
                }

                let anchor = target.anchor();

                if let Some(centre) = region
                    && distance(&anchor, &centre) > opts.near_radius {
                        continue;
                    }

                let (closest, dwell) = approach(&anchor, cameras, opts.require_approach);
                // No occlusion test exists here, so "the camera pointed this
                // way" is not the same as "the camera could see it" — a wall
                // two rooms away passes the cone test happily. Requiring the
                // player to have physically walked near the patch is the part
                // the cone cannot supply.
                if closest > opts.require_approach {
                    continue;
                }

                let best = sightings_for(&anchor, cameras, opts)
                    .first()
                    .copied()
                    .map(|s| quality(&s))
                    .unwrap_or(0.0);

                // Dwell decides it. A POV demo cannot be steered, so the
                // question is not "can this be seen from somewhere" but "how
                // often will it be in front of you" — and the answer is
                // wherever the player spends time, which is why spawn wins.
                // Then how much of the row the demo can vouch for, then whether
                // the patch is tall enough to hold all three rows inside the
                // stretch its decals evidence, then the best single view of it.
                // An upright wall is preferred over a floor of equal standing:
                // three rows stacked up a wall sit in front of the player,
                // where a grid on the ground has to be looked down at and
                // foreshortens.
                let tall_enough = extent(&target.members, target.row_axis) >= opts.row_gap * 2.0;
                let orientation = if axis == 2 { 0.85 } else { 1.0 };
                let score = (usize::from(target.witness_time.is_some()) as f32 * 1.0e9
                    + usize::from(best > 0.0) as f32 * 1.0e8
                    + target.backed as f32 * 1.0e6
                    + usize::from(tall_enough) as f32 * 1.0e5
                    + (dwell as f32).min(2000.0) * 20.0
                    + best.min(500.0) * 10.0
                    + target.members.len() as f32)
                    * orientation;

                scored.push((score, target));
            }
        }
    }

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    // Spread the grids around rather than stacking them on neighbouring
    // stretches of the same wall. Four grids in one room is barely better than
    // one; four in four rooms is four chances the camera turns toward one.
    let mut taken: Vec<[f32; 3]> = already.to_vec();
    let mut chosen: Vec<Target> = Vec::new();
    for (_, target) in scored {
        if chosen.len() >= opts.grids {
            break;
        }
        let anchor = target.anchor();
        if taken
            .iter()
            .all(|a| distance(a, &anchor) >= GRID_SEPARATION)
        {
            taken.push(anchor);
            chosen.push(target);
        }
    }
    chosen
}

/// A jump in the camera path large enough that the player cannot have walked
/// it. Samples are taken every fourth frame, and a sprinting player covers
/// well under 50 units in that time.
const TELEPORT_DISTANCE: f32 = 250.0;

/// How close two teleport destinations must be to count as the same spawn.
const SPAWN_CLUSTER: f32 = 300.0;

/// Camera position at a given viewdemo time.
///
/// Lets a caller point the probe at a place they can see on screen but cannot
/// measure — "spawn is what I am looking at 26:47:49" is a far easier thing to
/// supply than a world coordinate.
fn camera_at(cameras: &[CameraSample], svc_time: f32) -> Option<[f32; 3]> {
    cameras
        .iter()
        .min_by(|a, b| {
            (a.svc_time - svc_time)
                .abs()
                .partial_cmp(&(b.svc_time - svc_time).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|c| c.eye)
}

/// Where the player spawns.
///
/// There is no spawn message to read, but there does not need to be: a respawn
/// moves the player instantly, and an instant move is plain in the camera path
/// as a jump no amount of running could cover. Every respawn over a whole match
/// lands at a spawn point, so the destinations pile up there, and the densest
/// pile is spawn.
///
/// Switching spectator target teleports the camera too, but those destinations
/// are wherever a teammate happens to be — scattered across the map, and across
/// the match they cannot out-cluster the one place everybody returns to. That
/// is what makes taking the densest cluster, rather than any single jump,
/// the part that works.
///
/// Falls back to the median of the opening camera positions when a demo is too
/// short to contain a respawn.
///
/// The first version of this used the survey's `grounded_origin` — the first
/// position that settles on solid ground, which is wherever the demo happens to
/// begin — with a 1200-unit radius, and cheerfully labelled a wall 1139 units
/// away as "(spawn)". It was not.
fn spawn_position(cameras: &[CameraSample], early_samples: usize) -> Option<[f32; 3]> {
    if cameras.is_empty() {
        return None;
    }

    let destinations: Vec<[f32; 3]> = cameras
        .windows(2)
        .filter(|w| distance(&w[0].eye, &w[1].eye) > TELEPORT_DISTANCE)
        .map(|w| w[1].eye)
        .collect();

    if !destinations.is_empty() {
        let mut best = (0usize, destinations[0]);
        for candidate in &destinations {
            let n = destinations
                .iter()
                .filter(|d| distance(d, candidate) <= SPAWN_CLUSTER)
                .count();
            if n > best.0 {
                best = (n, *candidate);
            }
        }
        // A couple of coincident jumps is not a spawn; a pile of them is.
        if best.0 >= 3 {
            return Some(best.1);
        }
    }

    let take = early_samples.min(cameras.len());
    let mut out = [0.0f32; 3];
    for axis in 0..3 {
        let mut vals: Vec<f32> = cameras[..take].iter().map(|c| c.eye[axis]).collect();
        vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        out[axis] = vals[vals.len() / 2];
    }
    Some(out)
}

/// Which side of the surface the room is on.
///
/// Taken from where the camera actually was: an eye position is by definition
/// in open space, so the sign of its offset from the plane is the outward
/// direction. Cameras near the patch are weighted first, since a wall far
/// across the map may well have the player on the other side of it.
fn outward_sign(target: &Target, anchor: &[f32; 3], cameras: &[CameraSample]) -> f32 {
    for radius in [800.0f32, 2000.0, f32::INFINITY] {
        let mut sum = 0.0f32;
        let mut n = 0usize;
        for cam in cameras {
            if distance(&cam.eye, anchor) > radius {
                continue;
            }
            sum += cam.eye[target.axis] - target.value;
            n += 1;
        }
        if n > 0 && sum.abs() > 1.0 {
            return if sum > 0.0 { 1.0 } else { -1.0 };
        }
    }
    1.0
}

/// A decal the demo placed, with when it happened.
struct HarvestedDecal {
    position: [f32; 3],
    ordinal: i32,
}

/// Decal positions with the frame they arrived on.
///
/// The timing is the point. A decal message is a bullet landing at a known
/// instant, and if the camera was pointed at that spot at that instant, the
/// player watched it happen — which is proof of an unobstructed line of sight,
/// not a guess at one. That is the piece the cone test cannot supply on its
/// own, and it comes free with data already in the file.
fn collect_decals(demo: &dem::types::Demo) -> Vec<HarvestedDecal> {
    let mut out = Vec::new();
    let mut ordinal = 0i32;

    for entry in &demo.directory.entries {
        for frame in &entry.frames {
            ordinal += 1;
            let FrameData::NetworkMessage(net_msg_box) = &frame.frame_data else {
                continue;
            };
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
                let payload: &[u8] = match &te.entity {
                    TempEntity::TeWorldDecal(p)
                    | TempEntity::TeWorldDecalHigh(p)
                    | TempEntity::TeGunshotDecal(p)
                    | TempEntity::TeDecal(p)
                    | TempEntity::TeDecalHigh(p) => p,
                    TempEntity::TePlayerDecal(p) if p.len() >= 7 => &p[1..7],
                    _ => continue,
                };
                if payload.len() >= 6 {
                    out.push(HarvestedDecal {
                        position: [
                            i16::from_le_bytes([payload[0], payload[1]]) as f32 / 8.0,
                            i16::from_le_bytes([payload[2], payload[3]]) as f32 / 8.0,
                            i16::from_le_bytes([payload[4], payload[5]]) as f32 / 8.0,
                        ],
                        ordinal,
                    });
                }
            }
        }
    }
    out
}

/// Decals the camera was pointed at as they landed, with the moment it
/// happened.
///
/// A hit seen as it lands proves the spot is visible from somewhere the camera
/// actually was, with nothing in between — the one thing a cone test cannot
/// establish. Anchoring a grid on such a decal means the wall it goes on is a
/// wall the viewer has demonstrably seen, and the timestamp comes with it.
fn witnessed_decals(
    decals: &[HarvestedDecal],
    cameras: &[CameraSample],
    opts: &ProbeOptions,
) -> Vec<([f32; 3], f32)> {
    let mut out = Vec::new();
    for decal in decals {
        // Nearest camera sample in time; samples are strided, so this is within
        // a few frames of the decal's own instant.
        let idx = cameras.partition_point(|c| c.ordinal < decal.ordinal);
        for cam in cameras.iter().skip(idx.saturating_sub(1)).take(2) {
            if look_at(cam, &decal.position, opts).is_some() {
                out.push((decal.position, cam.svc_time));
                break;
            }
        }
    }
    out
}

/// Camera samples with ordinals and the viewdemo clock attached.
///
/// `SvcTime` is tracked rather than `Frame::time`: the former is what the
/// viewdemo window displays, and the two diverge by minutes on a demo whose
/// recording started before the player connected. A timestamp the user cannot
/// scrub to is worthless.
fn collect_cameras(demo: &dem::types::Demo) -> Vec<CameraSample> {
    let mut out = Vec::new();
    let mut ordinal = 0i32;
    let mut svc_time = 0.0f32;
    let mut stride = 0usize;

    for entry in &demo.directory.entries {
        for frame in &entry.frames {
            ordinal += 1;
            let FrameData::NetworkMessage(net_msg_box) = &frame.frame_data else {
                continue;
            };

            if let MessageData::Parsed(messages) = &net_msg_box.1.messages {
                for msg in messages {
                    if let NetMessage::EngineMessage(eng) = msg
                        && let EngineMessage::SvcTime(t) = eng.as_ref() {
                            svc_time = t.time;
                        }
                }
            }

            let rp = &net_msg_box.1.info.refparams;
            let (origin, fwd) = (&rp.view_origin, &rp.forward);
            if origin.len() < 3 || fwd.len() < 3 {
                continue;
            }
            let eye = [origin[0], origin[1], origin[2]];
            if eye == [0.0, 0.0, 0.0] {
                continue;
            }
            stride += 1;
            if !stride.is_multiple_of(4) {
                continue;
            }
            out.push(CameraSample {
                ordinal,
                eye,
                forward: [fwd[0], fwd[1], fwd[2]],
                svc_time,
            });
        }
    }
    out
}

/// Frame ordinal of the last DemoStart, i.e. the last level load.
///
/// Nothing may be injected before it. `R_ClearDecals()` runs on level load and
/// memsets the whole pool, so a grid stamped ahead of that point is wiped
/// before anyone can look at it — the exact mechanism that makes walls clean
/// when a demo is first loaded.
fn last_level_load_ordinal(demo: &dem::types::Demo) -> i32 {
    let mut ordinal = 0i32;
    let mut last = 0i32;
    for entry in &demo.directory.entries {
        for frame in &entry.frames {
            ordinal += 1;
            if matches!(frame.frame_data, FrameData::DemoStart) {
                last = ordinal;
            }
        }
    }
    last
}

/// Builds the three-row grid for one target.
fn build_probes(target: &Target, outward: f32, opts: &ProbeOptions) -> Vec<Probe> {
    let mut out = Vec::new();

    for (row, row_step) in [
        (ProbeRow::Out, 1.0f32),
        (ProbeRow::Control, 0.0),
        (ProbeRow::In, -1.0),
    ] {
        if opts.only_row.is_some_and(|only| only != row) {
            continue;
        }
        for (column, (&offset, &col_coord)) in
            opts.offsets.iter().zip(target.columns.iter()).enumerate()
        {
            let mut pos = [0.0f32; 3];
            pos[target.axis] = target.value
                + match row {
                    ProbeRow::Out => outward * offset,
                    ProbeRow::Control => 0.0,
                    ProbeRow::In => -outward * offset,
                };
            pos[target.col_axis] = col_coord;
            pos[target.row_axis] = target.row_center + row_step * opts.row_gap;

            out.push(Probe {
                row,
                column,
                offset,
                position: pos,
            });
        }
    }
    out
}

/// A unit vector lying in the target's surface, pointing screen-right from the
/// view the caller will be using.
///
/// Not merely a sign along `col_axis`, which was the first attempt. Picking a
/// direction from the two in-plane axes can only get within 45 degrees of
/// screen-right, and on a floor seen down its length that is nearly all
/// recession: shifting 250 units moved the grid 6 degrees across frame and
/// almost doubled its distance. Projecting the camera's own right vector into
/// the surface gives the direction that is actually lateral, whatever angle
/// the axes happen to sit at.
fn rightward_in_plane(
    target: &Target,
    cameras: &[CameraSample],
    opts: &ProbeOptions,
) -> Option<[f32; 3]> {
    let anchor = target.anchor();
    // The witness moment, when there is one, because that is the timestamp the
    // viewer is sent to. Resolving "right" against a different camera than the
    // one they will be looking through is how you send someone the wrong way.
    let when = target.witness_time.or_else(|| {
        sightings_for(&anchor, cameras, opts)
            .first()
            .map(|s| s.svc_time)
    });
    let when = when?;
    let cam = cameras.iter().find(|c| (c.svc_time - when).abs() < 0.05)?;
    let fwd = normalized(&cam.forward)?;
    let right = normalized(&cross(&fwd, &[0.0, 0.0, 1.0]))?;

    // Flatten screen-right into the surface. A wall seen face-on leaves it
    // almost unchanged; a floor seen down its length leaves whatever component
    // actually runs across the view.
    let mut normal = [0.0f32; 3];
    normal[target.axis] = 1.0;
    let along_normal = dot(&right, &normal);
    normalized(&[
        right[0] - normal[0] * along_normal,
        right[1] - normal[1] * along_normal,
        right[2] - normal[2] * along_normal,
    ])
}

/// Gap between beacon marks, and their clearance from the grid.
///
/// Wide enough that big marks neither merge with each other nor reach the
/// nearest measurement hole.
const BEACON_SPACING: f32 = 96.0;

/// A short bar of large marks set clear of the grid, to catch the eye.
///
/// Placed off to the side of the rows rather than past the end of them. Beyond
/// the last column put it right where the highest offsets sit — the exact spot
/// where a row is expected to run out of holes — so a mark that did get created
/// there could vanish into the blob and read as a rejection. Alongside, it
/// cannot swallow anything being counted.
fn build_beacon(target: &Target, opts: &ProbeOptions) -> Vec<[f32; 3]> {
    if target.columns.is_empty() {
        return Vec::new();
    }
    let middle = target.columns[target.columns.len() / 2];
    (-1..=1)
        .map(|k| {
            let mut pos = [0.0f32; 3];
            pos[target.axis] = target.value;
            pos[target.col_axis] = middle + k as f32 * opts.row_gap;
            pos[target.row_axis] = target.row_center + BEACON_SPACING;
            pos
        })
        .collect()
}

/// Strips the demo, stamps measurement grids onto proven surfaces, and pins
/// r_decals high enough that nothing can evict them.
pub fn probe_decal_offsets(
    demo_bytes: &[u8],
    opts: &ProbeOptions,
) -> Result<(Vec<u8>, ProbeStats), String> {
    if opts.offsets.is_empty() {
        return Err("no offsets to probe".into());
    }
    if opts.offsets.windows(2).any(|w| w[1] <= w[0]) {
        return Err("offsets must be strictly ascending — a row's hole count is \
                    only readable as a threshold if they are"
            .into());
    }
    if opts.offsets[0] < 0.0 {
        return Err("offsets are magnitudes; the OUT and IN rows apply the sign".into());
    }
    if opts.grids == 0 {
        return Err("no grids to stamp".into());
    }

    // Every row gets a zero-offset column of its own, ahead of the offsets
    // under test.
    //
    // Only the middle row sits in the stretch of surface the patch's decals
    // directly evidence; the OUT and IN rows are a row-gap above and below it,
    // which is arithmetic. If a wall happens to end just past the band that got
    // shot up — a low wall, a windowsill — a whole row lands on nothing and
    // reads as "the engine rejected every offset". A hole that IS on the plane
    // distinguishes those two cases for each row independently: no leading hole
    // means that row's band missed the wall and the row is void, rather than
    // being a threshold of zero.
    let opts = &ProbeOptions {
        offsets: if opts.offsets[0] == 0.0 {
            opts.offsets.clone()
        } else {
            std::iter::once(0.0)
                .chain(opts.offsets.iter().copied())
                .collect()
        },
        ..opts.clone()
    };

    let mut demo =
        open_demo_from_bytes(demo_bytes).map_err(|e| format!("Could not parse demo file: {}", e))?;

    // One window spanning the whole demo, so the survey harvests decals and a
    // texture index from every frame rather than from capture windows it has
    // no concept of here.
    let whole_demo = [(1i32, i32::MAX)];
    let survey = survey(&demo, &whole_demo, &DecalCleanOptions::default());
    let cameras = collect_cameras(&demo);
    let witnessed = witnessed_decals(&collect_decals(&demo), &cameras, opts);

    let mut region: Option<[f32; 3]> = None;
    let mut region_abandoned = false;

    let mut targets: Vec<Target> = match (opts.anchor, opts.axis) {
        (Some(a), Some(axis)) => {
            let (t1, t2) = tangent_axes(axis);
            let columns: Vec<f32> = (0..opts.offsets.len())
                .map(|j| {
                    a[t1] + (j as f32 - (opts.offsets.len() as f32 - 1.0) / 2.0)
                        * opts.column_spacing
                })
                .collect();
            vec![Target {
                axis,
                value: a[axis],
                col_axis: t1,
                row_axis: t2,
                evidence: vec![0.0; opts.offsets.len()],
                backed: opts.offsets.len(),
                columns,
                pitch: opts.column_spacing,
                row_center: a[t2],
                members: vec![a],
                spread: (0.0, 0.0),
                witness_time: None,
            }]
        }
        (Some(_), None) => {
            return Err("--anchor needs --axis: without a normal there is no \
                        direction to offset along"
                .into())
        }
        (None, _) => {
            // Spawn is where a POV camera spends by far the most time: the demo
            // opens there, and a dead player spectates teammates who are
            // themselves in spawn. Restricting to it trades map coverage for
            // the one thing that matters — being looked at.
            region = opts
                .near
                .or_else(|| opts.near_at.and_then(|t| camera_at(&cameras, t)))
                .or_else(|| {
                opts.spawn_only
                    .then_some(())
                    .and(spawn_position(&cameras, 200).or(survey.grounded_origin))
            });

            // Grids near spawn get looked at; grids chosen from the whole map
            // sit on the best-evidenced walls, which are usually the contested
            // ones nobody lingers in. Both, rather than a choice between them:
            // each grid is an independent measurement of the same constant, and
            // a hundred decals is nothing against a 4096-slot ring.
            let mut found =
                choose_targets(&survey.harvested, &cameras, &witnessed, opts, region, &[]);
            region_abandoned = region.is_some() && found.is_empty();

            // Naming a place explicitly means that place and no other. Only the
            // automatic spawn guess gets topped up from the wider map, since
            // that is a guess worth hedging.
            if opts.near.is_some() || opts.near_at.is_some() {
                found
            } else {
                let taken: Vec<[f32; 3]> = found.iter().map(|t| t.anchor()).collect();
                found.extend(choose_targets(
                    &survey.harvested,
                    &cameras,
                    &witnessed,
                    opts,
                    None,
                    &taken,
                ));
                found
            }
        }
    };

    if targets.is_empty() {
        return Err(format!(
            "no usable surface: {} harvested decals, none forming a connected patch of \
             {}+ decals coplanar within {:.1} units, spanning {:.0}+ units, wide enough \
             to seat {} columns, and within {:.0} units of somewhere the camera actually \
             went. Try a demo with more decal traffic, lower --min-plane-decals, a \
             smaller --column-spacing, a larger --require-approach, or pass --anchor \
             x,y,z with --axis.",
            survey.harvested.len(),
            opts.min_plane_decals,
            opts.plane_tolerance,
            opts.min_plane_spread,
            opts.offsets.len(),
            opts.require_approach
        ));
    }

    let texture_index = opts.texture_index.or(survey.texture_index).ok_or_else(|| {
        "no decal texture index available: the demo registered none and none was \
         supplied. Pass --texture-index."
            .to_string()
    })?;

    let texture_for = |row: ProbeRow| -> u8 {
        match (opts.row_textures, row) {
            (Some(t), ProbeRow::Out) => t[0],
            (Some(t), ProbeRow::Control) => t[1],
            (Some(t), ProbeRow::In) => t[2],
            (None, _) => texture_index,
        }
    };

    let mut grids: Vec<GridStats> = Vec::new();
    let mut all_positions: Vec<([f32; 3], u8)> = Vec::new();

    for target in targets.iter_mut() {
        if opts.shift_right != 0.0
            && let Some(dir) = rightward_in_plane(target, &cameras, opts) {
                let (col, row) = (target.col_axis, target.row_axis);
                let (dc, dr) = (dir[col] * opts.shift_right, dir[row] * opts.shift_right);
                for c in target.columns.iter_mut() {
                    *c += dc;
                }
                target.row_center += dr;
            }
        let target = &*target;
        let anchor = target.anchor();
        let outward = outward_sign(target, &anchor, &cameras);
        let probes = build_probes(target, outward, opts);
        let (closest, dwell) = approach(&anchor, &cameras, opts.require_approach);
        let stack = opts.stack.clamp(1, MAX_OVERLAP_DECALS - 1);
        let beacon = opts.beacon_texture.map(|_| build_beacon(target, opts)).unwrap_or_default();
        all_positions.extend(
            probes
                .iter()
                .flat_map(|p| std::iter::repeat_n((p.position, texture_for(p.row)), stack)),
        );
        if let Some(tex) = opts.beacon_texture {
            all_positions.extend(beacon.iter().map(|p| (*p, tex)));
        }

        grids.push(GridStats {
            axis: target.axis,
            plane_value: target.value,
            plane_members: target.members.len(),
            plane_spread: target.spread,
            outward,
            anchor,
            column_axis: target.col_axis,
            row_axis: target.row_axis,
            column_pitch: target.pitch,
            column_evidence: target.evidence.clone(),
            columns_backed: target.backed,
            probes,
            beacon,
            sightings: sightings_for(&anchor, &cameras, opts),
            closest_approach: closest,
            dwell_samples: dwell,
            witness_time: target.witness_time,
            local_relief: {
                // Everything over the ground the grid covers, plus a margin,
                // whether or not it is coplanar with the fitted plane —
                // but bounded along the normal too. Windowing only the two
                // in-plane axes leaves an infinite prism, which for a wall
                // swept in decals from every parallel surface across the map
                // and reported thousands of units of "relief".
                let half_col = target.pitch * opts.offsets.len() as f32 / 2.0 + target.pitch;
                let half_row = opts.row_gap * 2.0;
                // Far enough out to catch a ramp, close enough to exclude a
                // different surface that merely happens to lie behind this one.
                let half_normal = 48.0f32;
                let mut lo = f32::INFINITY;
                let mut hi = f32::NEG_INFINITY;
                let mut n = 0usize;
                for d in &survey.harvested {
                    if (d[target.col_axis] - anchor[target.col_axis]).abs() <= half_col
                        && (d[target.row_axis] - anchor[target.row_axis]).abs() <= half_row
                        && (d[target.axis] - target.value).abs() <= half_normal
                    {
                        lo = lo.min(d[target.axis]);
                        hi = hi.max(d[target.axis]);
                        n += 1;
                    }
                }
                (n, if lo.is_finite() { hi - lo } else { 0.0 })
            },
            witness_view: target.witness_time.and_then(|t| {
                let cam = cameras.iter().find(|c| (c.svc_time - t).abs() < 0.05)?;
                look_at(cam, &anchor, opts).map(|(d, _, yaw, pitch)| (yaw, pitch, d))
            }),
            out_row_in_view: {
                let mut delta = [0.0f32; 3];
                delta[target.row_axis] = opts.row_gap;
                sightings_for(&anchor, &cameras, opts).first().and_then(|best| {
                    let cam = cameras
                        .iter()
                        .find(|c| (c.svc_time - best.svc_time).abs() < 0.05)?;
                    let fwd = normalized(&cam.forward)?;
                    let right = normalized(&cross(&fwd, &[0.0, 0.0, 1.0]))?;
                    let up = cross(&right, &fwd);
                    Some([dot(&delta, &fwd), dot(&delta, &right), dot(&delta, &up)])
                })
            },
            in_region: region
                .map(|c| distance(&anchor, &c) <= opts.near_radius)
                .unwrap_or(false),
        });
    }

    // ── Strip, so the grids are the only thing on a wall ─────────────────────
    let (decals_stripped, sprays_stripped) = if opts.strip_all {
        strip_decal_messages(&mut demo, &[])
    } else {
        (0, 0)
    };

    // ── Stamp them as early as is safe ───────────────────────────────────────
    // Not before the last level load, and not before the client is actually in
    // the map. A frame only carries a usable camera once the player is in game,
    // so the first camera sample is a cheap stand-in for "finished connecting"
    // — injecting during the connect handshake would reference a decal texture
    // index the client has not resolved yet.
    //
    // Earliest-safe rather than just-in-time: nothing in a stripped demo can
    // evict them, so a grid placed at the start is on its wall for the whole
    // playback and every later sighting works.
    let floor = last_level_load_ordinal(&demo).max(cameras.first().map(|c| c.ordinal).unwrap_or(0));
    let eligible: Vec<(usize, usize, i32)> = frame_ordinals(&demo)
        .into_iter()
        .filter(|&(entry_idx, frame_idx, ordinal)| {
            ordinal > floor && crate::patch::is_injectable_frame(&demo, entry_idx, frame_idx)
        })
        .collect();

    let needed = all_positions.len().div_ceil(MAX_PER_FRAME);
    if eligible.len() < needed {
        return Err(format!(
            "only {} parsed network frames after the last level load, need {}",
            eligible.len(),
            needed
        ));
    }

    let injected_at_ordinal = eligible[0].2;
    let mut position_iter = all_positions.iter();
    let mut placed = 0usize;

    for &(entry_idx, frame_idx, _) in &eligible {
        if placed >= all_positions.len() {
            break;
        }
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
        for _ in 0..MAX_PER_FRAME {
            let Some(pos) = position_iter.next() else {
                break;
            };
            messages.push(build_world_decal(&pos.0, pos.1));
            placed += 1;
        }
    }

    if placed < all_positions.len() {
        return Err(format!(
            "only {} of {} probes could be placed — too few eligible frames",
            placed,
            all_positions.len()
        ));
    }

    // ── Pin r_decals well above the grid count ───────────────────────────────
    // The POV player's own gunfire is predicted client-side and never reaches
    // the wire, so stripping cannot remove it. A generous ring keeps those from
    // rotating a grid off its wall before it can be counted.
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
        let anchor_frame = entry
            .frames
            .get(insert_at)
            .or_else(|| entry.frames.first())
            .map(|f| (f.time, f.frame))
            .unwrap_or((0.0, 0));
        let cmd = format!("r_decals {}", opts.ring_limit);
        entry.frames.insert(
            insert_at,
            Frame {
                time: anchor_frame.0,
                frame: anchor_frame.1,
                frame_data: FrameData::ConsoleCommand(ConsoleCommand {
                    command: ByteString::from(cmd.as_str()),
                }),
            },
        );
        entry.frame_count = entry.frames.len() as i32;
    }

    let stats = ProbeStats {
        harvested_decals: survey.harvested.len(),
        texture_index,
        region,
        region_radius: opts.near_radius,
        region_abandoned,
        grids,
        decals_injected: placed,
        decals_stripped,
        sprays_stripped,
        injected_at_ordinal,
        injected_at_time: cameras
            .iter()
            .find(|c| c.ordinal >= injected_at_ordinal)
            .map(|c| c.svc_time)
            .unwrap_or(0.0),
    };

    Ok((demo.write_to_bytes(), stats))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts_with(offsets: &[f32]) -> ProbeOptions {
        ProbeOptions {
            offsets: offsets.to_vec(),
            ..Default::default()
        }
    }

    #[test]
    fn cluster_splits_only_on_gaps_wider_than_tolerance() {
        // Two surfaces 40 units apart, each with slight coordinate jitter.
        let values = [100.0, 100.5, 99.8, 140.0, 140.2];
        let groups = cluster(&values, 2.0);
        assert_eq!(groups.len(), 2);
        let sizes: Vec<usize> = groups.iter().map(|(_, m)| m.len()).collect();
        assert!(sizes.contains(&3) && sizes.contains(&2));
    }

    #[test]
    fn connected_patches_separates_coplanar_but_distant_surfaces() {
        // The failure this exists to prevent: two floors at the same height in
        // different rooms read as one plane, and a grid centred between them
        // would hang in mid-air over neither.
        let members = [
            [0.0, 0.0, -384.0],
            [40.0, 0.0, -384.0],
            [80.0, 0.0, -384.0],
            [2000.0, 0.0, -384.0],
            [2040.0, 0.0, -384.0],
        ];
        let patches = connected_patches(&members, 160.0);
        assert_eq!(patches.len(), 2);
        assert_eq!(patches.iter().map(|p| p.len()).sum::<usize>(), 5);
    }

    #[test]
    fn connected_patches_keeps_a_chain_together() {
        // Links are transitive: end-to-end distance may exceed the radius as
        // long as each hop does not, which is what lets one long wall stay one
        // patch.
        let members: Vec<[f32; 3]> = (0..6).map(|i| [i as f32 * 100.0, 0.0, 0.0]).collect();
        let patches = connected_patches(&members, 160.0);
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].len(), 6);
    }

    #[test]
    fn densest_band_centres_on_the_busiest_window() {
        let values = [0.0, 200.0, 205.0, 210.0, 215.0, 500.0];
        let centre = densest_band(&values, 20.0).unwrap();
        assert!((200.0..=215.0).contains(&centre), "got {}", centre);
    }

    #[test]
    fn lay_columns_spaces_evenly_and_reports_backing() {
        // Decals every 40 units across 320 units of wall, with a 120-unit hole
        // in the middle where a doorway would be.
        let mut band: Vec<[f32; 3]> = Vec::new();
        for i in 0..5 {
            band.push([i as f32 * 40.0, 0.0, 0.0]);
        }
        for i in 0..4 {
            band.push([320.0 + i as f32 * 40.0, 0.0, 0.0]);
        }

        let layout = lay_columns(&band, 0, 5, 40.0).expect("a layout exists");
        assert_eq!(layout.columns.len(), 5);
        assert_eq!(layout.evidence.len(), 5);

        // Evenly spaced, whatever pitch it settled on.
        for w in layout.columns.windows(2) {
            assert!(
                (w[1] - w[0] - layout.pitch).abs() < 0.01,
                "columns {:?} are not evenly spaced at pitch {}",
                layout.columns,
                layout.pitch
            );
        }
        // The left run alone seats all five columns, so it should find full
        // backing rather than straddling the gap.
        assert_eq!(layout.backed, 5);
        assert!(layout.evidence.iter().all(|e| *e <= layout.pitch / 2.0));
    }

    #[test]
    fn lay_columns_may_interleave_between_decals_and_still_count_as_backed() {
        // Four decals over 120 units, five columns wanted. The row does not
        // have to land on them: sliding half a pitch puts every column between
        // two decals, which evidences surface either side of it just as well.
        let band: Vec<[f32; 3]> = (0..4).map(|i| [i as f32 * 40.0, 0.0, 0.0]).collect();
        let layout = lay_columns(&band, 0, 5, 40.0).expect("a layout exists");
        assert_eq!(layout.backed, 5);
        assert!(layout.evidence.iter().all(|e| *e <= layout.pitch / 2.0));
        assert!(layout.pitch >= MIN_COLUMN_SPACING);
    }

    #[test]
    fn lay_columns_reports_a_column_it_cannot_back() {
        // Two decals 40 units apart cannot back five columns at any pitch down
        // to the floor, so the shortfall must be visible rather than silent.
        let band = [[0.0, 0.0, 0.0], [40.0, 0.0, 0.0]];
        let layout = lay_columns(&band, 0, 5, 40.0).expect("a layout exists");
        assert!(
            layout.backed < 5,
            "expected a shortfall, got {} at pitch {}",
            layout.backed,
            layout.pitch
        );
    }

    fn test_target() -> Target {
        Target {
            axis: 1,
            value: 448.0,
            col_axis: 0,
            row_axis: 2,
            columns: vec![100.0, 140.0, 180.0],
            evidence: vec![0.0; 3],
            backed: 3,
            pitch: 40.0,
            row_center: -200.0,
            members: vec![[100.0, 448.0, -200.0]],
            spread: (80.0, 0.0),
            witness_time: None,
        }
    }

    #[test]
    fn build_probes_puts_the_rows_on_opposite_sides_of_the_surface() {
        let opts = opts_with(&[0.0, 8.0, 32.0]);
        let target = test_target();
        let probes = build_probes(&target, -1.0, &opts);
        assert_eq!(probes.len(), 9);

        for p in &probes {
            match p.row {
                // Outward is -Y here, so OUT must sit below the plane value
                // and IN above it — the sign is taken from where the camera
                // was, not assumed.
                ProbeRow::Out => assert!(p.position[1] <= target.value),
                ProbeRow::In => assert!(p.position[1] >= target.value),
                ProbeRow::Control => assert_eq!(p.position[1], target.value),
            }
            if p.offset == 0.0 {
                assert_eq!(
                    p.position[1], target.value,
                    "every row's zero column must sit on the surface"
                );
            }
        }
    }

    #[test]
    fn build_probes_separates_the_rows_by_a_full_row_gap() {
        let opts = opts_with(&[0.0, 8.0]);
        let target = test_target();
        let probes = build_probes(&target, 1.0, &opts);

        let z_of = |row: ProbeRow| probes.iter().find(|p| p.row == row).unwrap().position[2];
        assert_eq!(z_of(ProbeRow::Control), target.row_center);
        assert!((z_of(ProbeRow::Out) - z_of(ProbeRow::Control) - opts.row_gap).abs() < 0.01);
        assert!((z_of(ProbeRow::Control) - z_of(ProbeRow::In) - opts.row_gap).abs() < 0.01);
    }

    #[test]
    fn target_anchor_sits_on_the_surface_at_the_middle_of_the_grid() {
        let target = test_target();
        let anchor = target.anchor();
        assert_eq!(anchor[target.axis], target.value);
        assert_eq!(anchor[target.col_axis], 140.0);
        assert_eq!(anchor[target.row_axis], target.row_center);
    }

    #[test]
    fn sightings_rank_near_and_centred_ahead_of_far_and_peripheral() {
        let close_centred = Sighting {
            svc_time: 0.0,
            distance: 80.0,
            off_axis_degrees: 3.0,
            yaw_degrees: -3.0,
            pitch_degrees: 0.0,
        };
        let far_peripheral = Sighting {
            svc_time: 0.0,
            distance: 700.0,
            off_axis_degrees: 22.0,
            yaw_degrees: 20.0,
            pitch_degrees: 8.0,
        };
        assert!(quality(&close_centred) > quality(&far_peripheral));
    }

    #[test]
    fn descending_offsets_are_rejected() {
        let demo: &[u8] = b"not a demo";
        let opts = opts_with(&[8.0, 4.0]);
        let err = probe_decal_offsets(demo, &opts).unwrap_err();
        assert!(err.contains("ascending"), "got: {}", err);
    }
}

/// Camera pose at a viewdemo timestamp, for projecting world points into a
/// screenshot.
#[derive(Debug, Clone, Copy)]
pub struct CameraView {
    pub eye: [f32; 3],
    pub forward: [f32; 3],
    /// The timestamp actually found, which may differ slightly from the one
    /// asked for.
    pub svc_time: f32,
}

/// Camera pose nearest a given viewdemo time.
///
/// Walks every frame rather than the strided sample set the rest of this module
/// uses: a few frames of error is nothing when ranking which wall to stamp, and
/// everything when working out where a decal lands in a specific screenshot.
pub fn camera_at_time(demo_bytes: &[u8], svc_time: f32) -> Result<CameraView, String> {
    let demo = open_demo_from_bytes(demo_bytes)
        .map_err(|e| format!("Could not parse demo file: {}", e))?;

    let mut best: Option<CameraView> = None;
    let mut now = 0.0f32;

    for entry in &demo.directory.entries {
        for frame in &entry.frames {
            let FrameData::NetworkMessage(net_msg_box) = &frame.frame_data else {
                continue;
            };
            if let MessageData::Parsed(messages) = &net_msg_box.1.messages {
                for msg in messages {
                    if let NetMessage::EngineMessage(eng) = msg
                        && let EngineMessage::SvcTime(t) = eng.as_ref() {
                            now = t.time;
                        }
                }
            }
            let rp = &net_msg_box.1.info.refparams;
            let (origin, fwd) = (&rp.view_origin, &rp.forward);
            if origin.len() < 3 || fwd.len() < 3 {
                continue;
            }
            let eye = [origin[0], origin[1], origin[2]];
            if eye == [0.0, 0.0, 0.0] {
                continue;
            }
            let candidate = CameraView {
                eye,
                forward: [fwd[0], fwd[1], fwd[2]],
                svc_time: now,
            };
            let better = best
                .map(|b| (now - svc_time).abs() < (b.svc_time - svc_time).abs())
                .unwrap_or(true);
            if better {
                best = Some(candidate);
            }
        }
    }

    best.ok_or_else(|| "no camera frames in demo".to_string())
}

/// Where a world point lands on screen, as a fraction of width and height from
/// the top-left. `None` when it is behind the camera.
///
/// GoldSrc specifies horizontal FOV and derives the vertical from the render
/// aspect, so the screenshot's own dimensions determine the vertical field —
/// which is why they have to be supplied rather than assumed.
pub fn project(
    view: &CameraView,
    point: &[f32; 3],
    fov_x_degrees: f32,
    width: f32,
    height: f32,
) -> Option<(f32, f32)> {
    let fwd = normalized(&view.forward)?;
    let right = normalized(&cross(&fwd, &[0.0, 0.0, 1.0]))?;
    let up = cross(&right, &fwd);

    let v = [
        point[0] - view.eye[0],
        point[1] - view.eye[1],
        point[2] - view.eye[2],
    ];
    let (f, r, u) = (dot(&v, &fwd), dot(&v, &right), dot(&v, &up));
    if f <= 1.0 {
        return None;
    }

    let tan_half_x = (fov_x_degrees.to_radians() / 2.0).tan();
    let tan_half_y = tan_half_x * height / width;
    Some((
        ((r / f) / tan_half_x + 1.0) / 2.0 * width,
        (1.0 - (u / f) / tan_half_y) / 2.0 * height,
    ))
}

/// The frame that shows the most of `points` at once, and how well separated
/// they are on screen.
///
/// A grid can be squarely in front of the camera and still be useless to count:
/// at 12:55:30 on the anzio beach the row ran almost straight away from the
/// viewer, putting four of its nine columns behind the camera and two more off
/// the bottom of the frame. Being visible and being countable are different
/// properties, and only the second one matters when someone has to tally marks
/// off a screenshot.
///
/// Returns the view, how many points land on screen, and the smallest gap in
/// pixels between any two of them — a pair closer than a few pixels reads as
/// one mark.
pub fn best_view_for(
    demo_bytes: &[u8],
    points: &[[f32; 3]],
    fov_x_degrees: f32,
    width: f32,
    height: f32,
) -> Result<Option<(CameraView, usize, f32)>, String> {
    let demo = open_demo_from_bytes(demo_bytes)
        .map_err(|e| format!("Could not parse demo file: {}", e))?;

    let mut best: Option<(CameraView, usize, f32)> = None;
    let mut now = 0.0f32;

    for entry in &demo.directory.entries {
        for frame in &entry.frames {
            let FrameData::NetworkMessage(net_msg_box) = &frame.frame_data else {
                continue;
            };
            if let MessageData::Parsed(messages) = &net_msg_box.1.messages {
                for msg in messages {
                    if let NetMessage::EngineMessage(eng) = msg
                        && let EngineMessage::SvcTime(t) = eng.as_ref() {
                            now = t.time;
                        }
                }
            }
            let rp = &net_msg_box.1.info.refparams;
            let (origin, fwd) = (&rp.view_origin, &rp.forward);
            if origin.len() < 3 || fwd.len() < 3 {
                continue;
            }
            let eye = [origin[0], origin[1], origin[2]];
            if eye == [0.0, 0.0, 0.0] {
                continue;
            }
            let view = CameraView {
                eye,
                forward: [fwd[0], fwd[1], fwd[2]],
                svc_time: now,
            };

            let on: Vec<(f32, f32)> = points
                .iter()
                .filter_map(|p| project(&view, p, fov_x_degrees, width, height))
                .filter(|(x, y)| *x >= 0.0 && *x <= width && *y >= 0.0 && *y <= height)
                .collect();
            if on.is_empty() {
                continue;
            }

            let mut gap = f32::INFINITY;
            for i in 0..on.len() {
                for j in (i + 1)..on.len() {
                    let (dx, dy) = (on[i].0 - on[j].0, on[i].1 - on[j].1);
                    gap = gap.min((dx * dx + dy * dy).sqrt());
                }
            }
            if on.len() == 1 {
                gap = width;
            }

            // Coverage first, then separation. A frame showing every mark but
            // with two of them overlapping is worse than one showing every mark
            // spread out, and both beat a frame showing half of them.
            let better = best
                .map(|(_, n, g)| on.len() > n || (on.len() == n && gap > g))
                .unwrap_or(true);
            if better {
                best = Some((view, on.len(), gap));
            }
        }
    }

    Ok(best)
}
