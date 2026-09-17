// patch/map_check.rs
// Does the map this demo was recorded on exist here, and is it the same build?
//
// A demo names its map in the header and stamps the map's checksum beside it.
// That makes three states worth telling apart, not two:
//
//   * the map is missing — the demo cannot be played, let alone captured
//   * the map is present but a DIFFERENT BUILD — it plays, and everything
//     derived from its geometry is quietly wrong
//   * the map is present and matches
//
// The middle state is the one worth the effort. `_b2` against `_b3e`, or a
// recompile that kept the name, gives a demo that loads and looks approximately
// right while every coordinate taken from the map refers to a different world.
// Nothing in playback announces it.
//
// The checksum itself is `bsp::map_checksum`. Reading it back out of a demo
// costs 544 bytes, so a whole folder can be checked at load without parsing a
// single frame.

use std::path::{Path, PathBuf};

use super::bsp;

/// Demo header layout, up to the field this module needs. The map name is a
/// NUL-padded 260-byte string and the checksum is the `u32` after the game
/// directory.
const HEADER_READ_LEN: usize = 544;
const MAGIC: &[u8] = b"HLDEMO\0\0";
const MAP_NAME_AT: usize = 16;
const MAP_NAME_LEN: usize = 260;
const MAP_CHECKSUM_AT: usize = 536;

/// What a demo needs from the map library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapReference {
    pub map_name: String,
    /// `None` for HLTV demos, which leave the field zeroed — see `MapStatus`.
    pub expected_checksum: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapStatus {
    /// Present, and the same build the demo was recorded on.
    Ok { checksum: u32 },
    /// Present, but a different build. Playback will work and be wrong.
    WrongBuild { expected: u32, found: u32 },
    /// Not in the map library at all.
    Missing,
    /// Present, but the demo does not say which build it wants. HLTV demos
    /// zero the field, so the most that can be said is that a map by that name
    /// is here.
    Unverifiable,
    /// Present and unreadable — truncated, or not a BSP at all.
    Unreadable { reason: String },
}

impl MapStatus {
    /// Whether anything derived from this map's geometry can be trusted.
    pub fn geometry_is_trustworthy(&self) -> bool {
        matches!(self, MapStatus::Ok { .. })
    }

    /// Whether the demo can be played at all.
    pub fn is_playable(&self) -> bool {
        !matches!(self, MapStatus::Missing)
    }

    /// One line, for a log or a list row.
    pub fn summary(&self, map_name: &str) -> String {
        match self {
            MapStatus::Ok { .. } => format!("`{}` matches", map_name),
            MapStatus::WrongBuild { expected, found } => format!(
                "`{}` is a different build — the demo wants `{:08x}`, this one is `{:08x}`",
                map_name, expected, found
            ),
            MapStatus::Missing => format!("`{}` is missing", map_name),
            MapStatus::Unverifiable => format!(
                "`{}` is present, but the demo does not record which build it needs",
                map_name
            ),
            MapStatus::Unreadable { reason } => {
                format!("`{}` could not be read: {}", map_name, reason)
            }
        }
    }
}

/// The map a demo was recorded on, read from its header alone.
pub fn map_reference(demo: &Path) -> Result<MapReference, String> {
    use std::io::Read;

    let mut file = std::fs::File::open(demo).map_err(|e| format!("{}: {}", demo.display(), e))?;
    let mut head = vec![0u8; HEADER_READ_LEN];
    file.read_exact(&mut head)
        .map_err(|_| format!("{}: shorter than a demo header", demo.display()))?;

    if &head[..MAGIC.len()] != MAGIC {
        return Err(format!("{}: not a HLDEMO file", demo.display()));
    }

    let raw = &head[MAP_NAME_AT..MAP_NAME_AT + MAP_NAME_LEN];
    let name: String = String::from_utf8_lossy(
        &raw.iter().copied().take_while(|b| *b != 0).collect::<Vec<u8>>(),
    )
    .trim()
    .to_lowercase();

    if name.is_empty() || !is_safe_map_name(&name) {
        return Err(format!("{}: header carries no usable map name", demo.display()));
    }

    let checksum = u32::from_le_bytes([
        head[MAP_CHECKSUM_AT],
        head[MAP_CHECKSUM_AT + 1],
        head[MAP_CHECKSUM_AT + 2],
        head[MAP_CHECKSUM_AT + 3],
    ]);

    Ok(MapReference {
        map_name: name,
        // HLTV demos leave this zeroed. Treated as "not stated" rather than as
        // a checksum, so a map is never called the wrong build on the strength
        // of a field that was never filled in.
        expected_checksum: (checksum != 0).then_some(checksum),
    })
}

/// A map name is a short identifier. One that is not is not a map name worth
/// joining onto a path.
pub fn is_safe_map_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Where a map by this name would live.
pub fn map_path(maps_dir: &Path, map_name: &str) -> PathBuf {
    maps_dir.join(format!("{}.bsp", map_name))
}

/// The map library belonging to a given `hl.exe`.
///
/// DoD's content sits beside the executable, so maps are `<hl.exe dir>/dod/maps`
/// — and note that demos live *inside* `dod/`, one level below the exe. Deriving
/// this from a demo's own folder instead gives `dod/dod/maps`, which exists
/// nowhere and fails silently.
///
/// `None` when that does not resolve to a real directory, rather than handing
/// back a path that can only fail later.
pub fn maps_dir_for_exe(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?.join("dod").join("maps");
    dir.is_dir().then_some(dir)
}

/// Check one reference against a map library.
pub fn status_of(reference: &MapReference, maps_dir: &Path) -> MapStatus {
    let path = map_path(maps_dir, &reference.map_name);
    if !path.is_file() {
        return MapStatus::Missing;
    }
    let Some(expected) = reference.expected_checksum else {
        return MapStatus::Unverifiable;
    };
    match bsp::map_checksum_of_file(&path) {
        Ok(found) if found == expected => MapStatus::Ok { checksum: found },
        Ok(found) => MapStatus::WrongBuild { expected, found },
        Err(reason) => MapStatus::Unreadable { reason },
    }
}

/// Everything about one demo's map, in a single call.
pub fn check_demo(demo: &Path, maps_dir: &Path) -> Result<(MapReference, MapStatus), String> {
    let reference = map_reference(demo)?;
    let status = status_of(&reference, maps_dir);
    Ok((reference, status))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Scratch;

    fn header(map: &str, checksum: u32) -> Vec<u8> {
        let mut head = vec![0u8; HEADER_READ_LEN];
        head[..MAGIC.len()].copy_from_slice(MAGIC);
        head[MAP_NAME_AT..MAP_NAME_AT + map.len()].copy_from_slice(map.as_bytes());
        head[MAP_CHECKSUM_AT..MAP_CHECKSUM_AT + 4].copy_from_slice(&checksum.to_le_bytes());
        head
    }

    fn scratch(tag: &str) -> Scratch {
        Scratch::new(format_args!("map_check_{tag}"))
    }

    #[test]
    fn a_header_yields_the_map_and_the_build_it_wants() {
        let dir = scratch("ref");
        let demo = dir.join("a.dem");
        std::fs::write(&demo, header("dod_anzio", 0x801b_771f)).unwrap();

        let r = map_reference(&demo).unwrap();
        assert_eq!(r.map_name, "dod_anzio");
        assert_eq!(r.expected_checksum, Some(0x801b_771f));
    }

    #[test]
    fn an_hltv_demo_states_no_build_rather_than_a_zero_one() {
        // HLTV leaves the field zeroed. Read as a checksum it would call every
        // map on disk the wrong build; read as "not stated" it says only what
        // the demo actually says.
        let dir = scratch("hltv");
        let demo = dir.join("a.dem");
        std::fs::write(&demo, header("dod_harrington", 0)).unwrap();

        assert_eq!(map_reference(&demo).unwrap().expected_checksum, None);
    }

    #[test]
    fn a_missing_map_is_missing_and_a_present_one_without_a_stated_build_is_unverifiable() {
        let dir = scratch("missing");
        let maps = dir.join("maps");
        std::fs::create_dir_all(&maps).unwrap();

        let wants = MapReference {
            map_name: "dod_anzio".to_string(),
            expected_checksum: Some(1),
        };
        assert_eq!(status_of(&wants, &maps), MapStatus::Missing);
        assert!(!status_of(&wants, &maps).is_playable());

        std::fs::write(map_path(&maps, "dod_anzio"), b"not a bsp").unwrap();
        let unstated = MapReference {
            map_name: "dod_anzio".to_string(),
            expected_checksum: None,
        };
        assert_eq!(status_of(&unstated, &maps), MapStatus::Unverifiable);
        assert!(
            !status_of(&unstated, &maps).geometry_is_trustworthy(),
            "an unverified map must not be trusted for geometry"
        );
    }

    #[test]
    fn a_map_that_is_not_a_bsp_is_unreadable_not_a_wrong_build() {
        let dir = scratch("garbage");
        let maps = dir.join("maps");
        std::fs::create_dir_all(&maps).unwrap();
        std::fs::write(map_path(&maps, "dod_anzio"), b"short").unwrap();

        let wants = MapReference {
            map_name: "dod_anzio".to_string(),
            expected_checksum: Some(1),
        };
        assert!(matches!(status_of(&wants, &maps), MapStatus::Unreadable { .. }));
    }

    #[test]
    fn a_map_name_that_could_escape_the_maps_folder_is_refused() {
        let dir = scratch("escape");
        let demo = dir.join("a.dem");
        std::fs::write(&demo, header("../../windows/system32/x", 1)).unwrap();

        assert!(map_reference(&demo).is_err());
    }
}
