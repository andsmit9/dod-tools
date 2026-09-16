// patch/decal_atlas.rs
// A per-map store of proven surface coordinates, accumulated across demos.
//
// ── Why this exists ──────────────────────────────────────────────────────────
// The only property the flush needs from a harvested decal is that the engine
// once created a decal at that coordinate — proof that `R_DecalShoot`'s BSP
// walk finds a surface there. Nothing else about the decal is used: visibility
// is computed per demo from that demo's own cameras, and the texture index is
// harvested separately.
//
// That property belongs to the MAP, not to the demo. A coordinate that is valid
// on dod_anzio is valid in every dod_anzio demo, whatever team the POV played.
// But each demo only ever proves the surfaces its own player happened to shoot,
// which is why a third of a 28-demo survey came up short: the quiet walls a
// flush wants are quiet precisely because nobody shot them, so nothing in that
// demo proves they exist.
//
// Pooling the coordinates across demos turns that per-demo scarcity into
// per-map abundance. An allies demo contributes one side of a map, an axis demo
// the other, and every later demo on that map draws on the union — including
// stretches of map its own player never visited.
//
// ── Two things this has to get right ─────────────────────────────────────────
//  1. Keyed on map name AND checksum. `dod_saints2_b2`, `_b3e` and `_B2` are
//     different geometry, and a recompiled map reusing a name would otherwise
//     poison the pool with coordinates pointing into thin air. Both come free
//     in the demo header.
//  2. World decals only. TE_GUNSHOTDECAL and friends carry an entity index, so
//     some marks sit on doors, lifts and other brush entities. Inside one demo
//     that is nearly harmless; in a store that outlives the demo, a coordinate
//     on a door is only valid while the door is where it was.
//
// A coordinate that turns out to be wrong fails safe — the engine creates no
// decal, the ring does not advance, and the sweep is reported as short.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Grid used to decide whether two coordinates are the same spot, in world
/// units. It bounds file growth and stops one heavily-shot wall from filling
/// the store with near-duplicates, and sits well under `TILE_PITCH` so it never
/// costs a distinct flush position.
///
/// It is a DEDUPE key only — the coordinate written out is the exact one the
/// engine accepted, never the rounded one. Rounding the stored value would put
/// it up to half a cell off the true surface along the normal, and `m_Size` was
/// measured at ~4 units, so the engine only tolerates ~3 units outward before
/// `R_DecalShoot` finds nothing. A store of coordinates that miss is worse than
/// no store: each one silently costs a ring slot.
pub const ATLAS_GRID: f32 = 8.0;

/// Ceiling on coordinates kept for one map. A 20k-point cloud on an 8-unit grid
/// covers far more surface than any sweep can consume, and caps the file at a
/// few hundred KB.
pub const MAX_ATLAS_COORDS: usize = 20_000;

/// Identifies one exact build of one map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapKey {
    pub name: String,
    pub checksum: u32,
}

impl MapKey {
    /// Reads the key straight off a parsed demo's header.
    pub fn from_header(header: &dem::types::Header) -> Option<Self> {
        // ByteString is NUL-padded to 260 bytes in the file.
        let raw: Vec<u8> = header
            .map_name
            .as_slice()
            .iter()
            .copied()
            .take_while(|b| *b != 0)
            .collect();
        let name = String::from_utf8_lossy(&raw).trim().to_lowercase();
        if name.is_empty() {
            return None;
        }
        // Anything that could escape a directory or collide across platforms is
        // rejected rather than sanitised — a map name is a short identifier, and
        // one that isn't is not a map name worth trusting with a file path.
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
        {
            return None;
        }
        Some(Self {
            name,
            checksum: header.map_checksum,
        })
    }

    fn file_name(&self) -> String {
        format!("{}_{:08x}.json", self.name, self.checksum)
    }
}

/// Quantised coordinate, the form actually stored. Integer so that dedupe is
/// exact and ordering is stable across runs.
type Cell = (i32, i32, i32);

fn to_cell(p: &[f32; 3]) -> Option<Cell> {
    if !p.iter().all(|v| v.is_finite()) {
        return None;
    }
    Some((
        (p[0] / ATLAS_GRID).round() as i32,
        (p[1] / ATLAS_GRID).round() as i32,
        (p[2] / ATLAS_GRID).round() as i32,
    ))
}



/// What one merge did, for reporting.
#[derive(Debug, Default, Clone, Copy)]
pub struct AtlasStats {
    /// Coordinates the store held before this demo contributed.
    pub known: usize,
    /// New coordinates this demo added.
    pub added: usize,
    /// Coordinates available to the flush afterwards.
    pub total: usize,
}

/// On-disk format version. Present so a future store — one shipped with the
/// app, or pooled from several people's captures — can be recognised and
/// migrated rather than guessed at.
pub const ATLAS_FORMAT: u32 = 1;

pub fn atlas_path(dir: &Path, key: &MapKey) -> PathBuf {
    dir.join(key.file_name())
}

/// Unions one map's coordinates across several stores.
///
/// The intended shape is one writable store fed by this user's own captures
/// plus any number of read-only ones — a store shipped with the app, or a
/// pooled community store dropped in by the updater. Union is the whole merge
/// rule: every entry is an independent claim that a surface exists at a
/// coordinate, so two stores can never contradict each other, only cover
/// different ground. Deduplication happens on the shared grid.
pub fn load_all(dirs: &[PathBuf], key: &MapKey) -> Vec<[f32; 3]> {
    let mut cells: BTreeMap<Cell, [f32; 3]> = BTreeMap::new();
    for dir in dirs {
        for p in load(dir, key) {
            if cells.len() >= MAX_ATLAS_COORDS {
                break;
            }
            if let Some(c) = to_cell(&p) {
                cells.entry(c).or_insert(p);
            }
        }
    }
    cells.into_values().collect()
}

/// Reads one map's stored coordinates. A missing, unreadable or malformed file
/// is an empty store, not an error: the flush works without one and fills it in
/// on the way past.
pub fn load(dir: &Path, key: &MapKey) -> Vec<[f32; 3]> {
    let path = atlas_path(dir, key);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) else {
        crate::log_markdown(&format!(
            "⚠️ **Decal atlas unreadable** at `{}` — ignoring it for this run.",
            path.display()
        ));
        return Vec::new();
    };

    // The checksum is in the filename, but it is verified from the contents too
    // so a hand-copied or renamed file cannot silently apply to another build.
    let stored_sum = doc.get("checksum").and_then(|v| v.as_u64());
    if stored_sum != Some(key.checksum as u64) {
        crate::log_markdown(&format!(
            "⚠️ **Decal atlas checksum mismatch** at `{}` — the map has been rebuilt since these \
             coordinates were recorded, so they are ignored.",
            path.display()
        ));
        return Vec::new();
    }

    doc.get("coords")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    let a = c.as_array()?;
                    if a.len() < 3 {
                        return None;
                    }
                    Some([
                        a[0].as_f64()? as f32,
                        a[1].as_f64()? as f32,
                        a[2].as_f64()? as f32,
                    ])
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Folds this demo's proven coordinates into the map's store and writes it back.
///
/// Returns the merged set so the caller does not have to re-read what it just
/// wrote. A write failure is reported and the merged set still returned — the
/// flush can use the coordinates for this run even if they cannot be kept.
pub fn merge_and_save(
    dir: &Path,
    seed_dirs: &[PathBuf],
    key: &MapKey,
    contributed: &[[f32; 3]],
) -> (Vec<[f32; 3]>, AtlasStats) {
    let existing = load(dir, key);

    // Keyed on the dedupe cell, valued with the exact coordinate the engine
    // accepted. The first one seen for a cell wins — every candidate for a cell
    // is equally proven, so there is nothing to choose between them, and
    // keeping the first makes a rerun over the same demos idempotent.
    let mut cells: BTreeMap<Cell, [f32; 3]> = BTreeMap::new();
    for p in &existing {
        if let Some(c) = to_cell(p) {
            cells.entry(c).or_insert(*p);
        }
    }
    let known = cells.len();

    for p in contributed {
        if cells.len() >= MAX_ATLAS_COORDS {
            break;
        }
        if let Some(c) = to_cell(p) {
            cells.entry(c).or_insert(*p);
        }
    }

    let stats = AtlasStats {
        known,
        added: cells.len().saturating_sub(known),
        total: cells.len(),
    };

    // Only this user's own store is written back; the seed stores are read-only
    // and are unioned in afterwards, so an app update can replace them without
    // ever absorbing or overwriting locally harvested coordinates.
    let mut pool: BTreeMap<Cell, [f32; 3]> = cells.clone();
    for dir in seed_dirs {
        for p in load(dir, key) {
            if pool.len() >= MAX_ATLAS_COORDS {
                break;
            }
            if let Some(c) = to_cell(&p) {
                pool.entry(c).or_insert(p);
            }
        }
    }
    let merged: Vec<[f32; 3]> = pool.into_values().collect();

    // Nothing new and a file already present means there is nothing to write.
    if stats.added > 0 || known == 0 {
        write(dir, key, &cells, &stats);
    }

    (merged, stats)
}

fn write(dir: &Path, key: &MapKey, cells: &BTreeMap<Cell, [f32; 3]>, stats: &AtlasStats) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        crate::log_markdown(&format!(
            "⚠️ **Decal atlas not saved** — could not create `{}`: {}",
            dir.display(),
            e
        ));
        return;
    }

    let coords: Vec<serde_json::Value> = cells
        .values()
        .map(|p| serde_json::json!([p[0], p[1], p[2]]))
        .collect();

    let doc = serde_json::json!({
        "format": ATLAS_FORMAT,
        "map": key.name,
        "checksum": key.checksum,
        "grid": ATLAS_GRID,
        "coords": coords,
    });

    let path = atlas_path(dir, key);
    // Written via a temp file and renamed so an interrupted capture cannot
    // leave a half-written store that the next run has to reject.
    let tmp = path.with_extension("json.tmp");
    let Ok(text) = serde_json::to_string(&doc) else {
        return;
    };
    if let Err(e) = std::fs::write(&tmp, text.as_bytes()) {
        crate::log_markdown(&format!(
            "⚠️ **Decal atlas not saved** — could not write `{}`: {}",
            tmp.display(),
            e
        ));
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        crate::log_markdown(&format!(
            "⚠️ **Decal atlas not saved** — could not replace `{}`: {}",
            path.display(),
            e
        ));
        return;
    }

    let _ = stats;
}

/// Default location: alongside the activity logs, under the app's data dir.
pub fn default_dir() -> PathBuf {
    crate::shared::paths::get_appdata_dir().join("decal_atlas")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dod_atlas_test_{}", name));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn key() -> MapKey {
        MapKey {
            name: "dod_anzio".to_string(),
            checksum: 0xdeadbeef,
        }
    }

    #[test]
    fn seed_stores_are_unioned_in_but_never_written_to() {
        // The distribution shape: a read-only store the app ships and the
        // updater can replace wholesale, plus the user's own harvest. An update
        // must never absorb or overwrite locally harvested coordinates.
        let user = scratch("seed_user");
        let seed = scratch("seed_shipped");
        merge_and_save(&seed, &[], &key(), &[[800.0, 800.0, 800.0]]);

        let (merged, stats) = merge_and_save(&user, std::slice::from_ref(&seed), &key(), &[[0.0, 0.0, 0.0]]);
        assert_eq!(stats.added, 1, "only the user's own coordinate is recorded");
        assert_eq!(merged.len(), 2, "but both are available to the flush");
        assert_eq!(load(&user, &key()).len(), 1, "the seed must not leak into the user store");
        assert_eq!(load(&seed, &key()).len(), 1, "the seed must not be rewritten");
    }

    #[test]
    fn stored_coordinates_are_exact_not_rounded() {
        // ATLAS_GRID is a dedupe key, never a quantiser. `m_Size` is ~4 units,
        // so the engine tolerates only ~3 units of error along the surface
        // normal: a coordinate snapped onto an 8-unit grid could sit half a
        // cell off the wall, create no decal, and silently cost a ring slot.
        // Rounding here once cost a demo two flush positions.
        let dir = scratch("exact");
        let odd = [101.5, -37.25, 12.125];

        let (merged, _) = merge_and_save(&dir, &[], &key(), &[odd]);
        assert_eq!(merged, vec![odd], "the merged pool must carry exact values");
        assert_eq!(load(&dir, &key()), vec![odd], "and so must the file");
    }

    #[test]
    fn coordinates_survive_a_round_trip() {
        let dir = scratch("roundtrip");
        let pts = vec![[100.0, 200.0, 300.0], [-64.0, 8.0, 0.0]];

        let (merged, stats) = merge_and_save(&dir, &[], &key(), &pts);
        assert_eq!(stats.added, 2);
        assert_eq!(merged.len(), 2);
        assert_eq!(load(&dir, &key()).len(), 2);
    }

    #[test]
    fn a_second_demo_adds_only_what_is_new() {
        // The whole point: each demo contributes the surfaces its own player
        // proved, and the store is the union across demos.
        let dir = scratch("union");
        let first = vec![[100.0, 200.0, 300.0], [104.0, 200.0, 300.0]];
        let (_, a) = merge_and_save(&dir, &[], &key(), &first);
        // Both round to the same 8-unit cell.
        assert_eq!(a.added, 1, "near-duplicates must collapse onto the grid");

        let second = vec![[100.0, 200.0, 300.0], [900.0, 900.0, 900.0]];
        let (merged, b) = merge_and_save(&dir, &[], &key(), &second);
        assert_eq!(b.known, 1);
        assert_eq!(b.added, 1, "only the coordinate the first demo never saw");
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn a_rebuilt_map_does_not_inherit_stale_coordinates() {
        // Same name, different checksum: the geometry changed underneath, so
        // every stored coordinate is now a guess about a map that no longer
        // exists. Reusing them would put the whole sweep in thin air.
        let dir = scratch("checksum");
        merge_and_save(&dir, &[], &key(), &[[100.0, 200.0, 300.0]]);

        let rebuilt = MapKey {
            name: "dod_anzio".to_string(),
            checksum: 0x11111111,
        };
        assert!(
            load(&dir, &rebuilt).is_empty(),
            "a different build must start from an empty store"
        );
    }

    #[test]
    fn map_versions_are_kept_apart() {
        let dir = scratch("versions");
        let b2 = MapKey {
            name: "dod_saints2_b2".to_string(),
            checksum: 1,
        };
        let b3 = MapKey {
            name: "dod_saints2_b3e".to_string(),
            checksum: 2,
        };
        merge_and_save(&dir, &[], &b2, &[[1.0, 2.0, 3.0]]);
        merge_and_save(&dir, &[], &b3, &[[9.0, 9.0, 9.0], [80.0, 80.0, 80.0]]);

        assert_eq!(load(&dir, &b2).len(), 1);
        assert_eq!(load(&dir, &b3).len(), 2);
    }

    #[test]
    fn the_store_is_bounded() {
        let dir = scratch("cap");
        let many: Vec<[f32; 3]> = (0..(MAX_ATLAS_COORDS as i32 + 500))
            .map(|i| [i as f32 * ATLAS_GRID, 0.0, 0.0])
            .collect();
        let (merged, stats) = merge_and_save(&dir, &[], &key(), &many);
        assert_eq!(merged.len(), MAX_ATLAS_COORDS);
        assert_eq!(stats.total, MAX_ATLAS_COORDS);
    }

    #[test]
    fn a_missing_store_is_simply_empty() {
        let dir = scratch("missing");
        assert!(load(&dir, &key()).is_empty());
    }

    #[test]
    fn a_corrupt_store_is_ignored_rather_than_fatal() {
        let dir = scratch("corrupt");
        std::fs::write(atlas_path(&dir, &key()), b"{not json").unwrap();
        assert!(load(&dir, &key()).is_empty());
    }
}
