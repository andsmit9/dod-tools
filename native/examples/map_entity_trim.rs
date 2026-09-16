//! Reports, proposes and applies an entity-lump trim for a GoldSrc map.
//!
//! `dod_lennon2` networks 276 entities from its opening snapshot against the
//! pre-Anniversary `MAX_PACKET_ENTITIES` of 256, so every HLTV recording of it
//! kills `hl.exe` on load (#207). The map itself accounts for essentially all
//! of that; gameplay adds only ~15-25 on top. This trims the entity lump, which
//! is the one lump the map checksum excludes — so a trimmed map still loads
//! every demo recorded against the original build.
//!
//! It never writes over its input, and it refuses to write at all if the map
//! checksum moved.
//!
//!     # what is on the wire, and what it would cost to remove
//!     cargo run -p native --bin map_entity_trim -- <map.bsp>
//!
//!     # a reviewable strip list: tiers A and B active, tier C commented out
//!     cargo run -p native --bin map_entity_trim -- <map.bsp> --propose > strip.txt
//!
//!     # apply a reviewed list to a copy
//!     cargo run -p native --bin map_entity_trim -- <map.bsp> --strip strip.txt --out <trimmed.bsp>
//!
//! Ground truth for "what is actually on the wire" is
//! `analysis/examples/map_entity_probe`, which reads it out of a demo. The
//! classification here is a model of the server DLL's behaviour, not the server
//! DLL, so it was checked against one HLTV demo per map:
//!
//!     map                 predicted   measured
//!     dod_lennon2               276        276
//!     dod_thunder2              232        232
//!     dod_anzio                 219        219
//!     dod_solitude_b2           133        133
//!     dod_railroad2_test        128        128
//!     dod_armory_b6              71         71
//!     dod_harrington            112        110
//!
//! `dod_harrington` is the one gap, and it is the demo rather than the rule:
//! its two missing entities are `func_breakable` TNT objectives that were
//! already blown when that recording started. A destroyed breakable leaves the
//! entity list, so an intact map predicts two more than a mid-match demo shows.
//! Check a trimmed map against a fresh recording with the probe regardless.

use native::patch::bsp::Bsp;
use native::patch::bsp::map_checksum;
use native::patch::bsp_entities::{
    MapEntity, parse_entities, rewrite_entity_lump, serialize_entities,
};

/// Brush-entity classnames the server marks `EF_NODRAW`, so they hold an edict
/// but never reach a snapshot. `CBaseTrigger::InitTrigger` is what does it, and
/// `dod_trigger_sandbag` — the volume a machine gunner deploys a bipod in — is
/// one of these despite the name reading like a prop.
///
/// `func_ladder` is deliberately *not* here: it clears `EF_NODRAW` again and
/// goes on the wire invisible, which is why it shows up in the counts below.
const NODRAW_BRUSH_CLASSES: &[&str] =
    &["trigger_hurt", "trigger_multiple", "dod_capture_area", "dod_trigger_sandbag"];

/// Point-entity classnames that hold a networked edict.
///
/// `env_model` is deliberately absent: DoD makes those static at spawn, which
/// takes them off the entity list entirely — 144 of `dod_lennon2`'s 145 never
/// reach a snapshot. The one that does carries a `targetname`, handled below.
/// `particle_shooter` is absent for the same reason, measured on `solitude_b2`.
const NETWORKED_POINT_CLASSES: &[&str] = &["env_sprite", "env_glow", "dod_control_point"];

/// `rendermode` values that scale with `renderamt`. `kRenderTransAlpha` (4) is
/// not one of them — it is a binary alpha test, so a low `renderamt` there does
/// not mean "nearly invisible" and must not be read as free to remove.
const AMT_SCALED_RENDERMODES: &[i32] = &[1, 2, 3, 5];

/// Above this, an entity is visible enough that removing it is a judgement
/// call rather than a free win.
const INVISIBLE_RENDERAMT: i32 = 15;

fn networked(e: &MapEntity) -> bool {
    let class = e.classname();
    if e.brush_submodel().is_some() {
        return !NODRAW_BRUSH_CLASSES.contains(&class);
    }
    if NETWORKED_POINT_CLASSES.contains(&class) {
        return true;
    }
    // An `env_model` something can trigger cannot be made static, so it stays
    // on the entity list.
    class == "env_model" && e.get("targetname").is_some()
}

/// Tier A: on the wire but not on the screen.
fn invisible(e: &MapEntity) -> bool {
    let (mode, amt) = e.render();
    AMT_SCALED_RENDERMODES.contains(&mode) && amt <= INVISIBLE_RENDERAMT
}

/// Total face area of a brush entity's submodel, as a stand-in for how much of
/// the map disappears with it.
fn submodel_area(bsp: &Bsp, submodel: u32) -> f32 {
    let Some(model) = bsp.models.get(submodel as usize) else { return 0.0 };
    let first = model.first_face.max(0) as usize;
    let count = model.num_faces.max(0) as usize;
    (first..first + count).map(|f| bsp.face_area(f)).sum()
}

/// A submodel's axis-aligned bounds, unioned across its own faces -- the same
/// face range `submodel_area` sums, just tracking extent instead of area.
fn submodel_bounds(bsp: &Bsp, submodel: u32) -> Option<([f32; 3], [f32; 3])> {
    let model = bsp.models.get(submodel as usize)?;
    let first = model.first_face.max(0) as usize;
    let count = model.num_faces.max(0) as usize;
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    let mut any = false;
    for f in first..first + count {
        let (flo, fhi) = bsp.face_bounds(f);
        for a in 0..3 {
            lo[a] = lo[a].min(flo[a]);
            hi[a] = hi[a].max(fhi[a]);
        }
        any = true;
    }
    any.then_some((lo, hi))
}

fn dominant_texture(bsp: &Bsp, submodel: u32) -> String {
    let Some(model) = bsp.models.get(submodel as usize) else { return "<none>".into() };
    let first = model.first_face.max(0) as usize;
    let count = model.num_faces.max(0) as usize;
    let mut tally: std::collections::HashMap<&str, usize> = Default::default();
    for f in first..first + count {
        if let Some(name) = bsp.texture_name(f) {
            *tally.entry(name).or_default() += 1;
        }
    }
    // Tie-break by name: a HashMap's iteration order is not stable between
    // runs, and a strip list that renames its own entities run to run is not
    // reviewable.
    tally
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))
        .map(|(t, _)| t.to_string())
        .unwrap_or_default()
}

struct Candidate {
    lump_index: usize,
    classname: String,
    tier: &'static str,
    note: String,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut map_path: Option<String> = None;
    let mut propose = false;
    // The floor is not the number that has to fit under 256 -- gameplay adds
    // players and dropped weapons on top of it, 24-27 in the HLTV demos
    // measured on #207. 215 leaves that headroom and then some.
    let mut target: usize = 215;
    let mut strip_path: Option<String> = None;
    let mut out_path: Option<String> = None;
    let mut demos_dir: Option<String> = None;

    while let Some(a) = args.next() {
        match a.as_str() {
            "--propose" => propose = true,
            "--target" => target = args.next().and_then(|v| v.parse().ok()).unwrap_or(target),
            "--strip" => strip_path = args.next(),
            "--out" => out_path = args.next(),
            "--demos" => demos_dir = args.next(),
            other => map_path = Some(other.to_string()),
        }
    }

    let Some(map_path) = map_path else {
        eprintln!("usage: map_entity_trim <map.bsp> [--propose --target N] [--strip list.txt --out trimmed.bsp]");
        std::process::exit(2);
    };

    let bytes = std::fs::read(&map_path).unwrap_or_else(|e| fail(&format!("{map_path}: {e}")));
    let checksum = map_checksum(&bytes).unwrap_or_else(|e| fail(&e));
    let bsp = Bsp::parse(&bytes).unwrap_or_else(|e| fail(&e));
    let entities = parse_entities(&bytes).unwrap_or_else(|e| fail(&e));

    // Where players have actually been, harvested from real demos of the map --
    // a tier-C candidate far from every one of these points is safe to remove
    // with much more confidence than face area alone can offer. Optional: with
    // no directory given, tier C falls back to its original area-only ranking.
    let map_name = std::path::Path::new(&map_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_else(|| fail("map path has no usable file name"))
        .to_string();
    let reach = demos_dir.as_deref().map(|dir| {
        native::patch::reachability::harvest_directory(std::path::Path::new(dir), checksum, &map_name)
            .unwrap_or_else(|e| fail(&e))
    });
    if let Some(cloud) = &reach {
        eprintln!("reachability: {} proven player positions harvested from {}",
            cloud.len(), demos_dir.as_deref().unwrap_or(""));
    }

    if let Some(strip_path) = strip_path {
        apply(&map_path, &bytes, checksum, &entities, &strip_path, out_path.as_deref());
        return;
    }

    let on_wire: Vec<usize> =
        (0..entities.len()).filter(|i| networked(&entities[*i])).collect();

    // In propose mode stdout is the strip list itself and has to stay
    // machine-readable, so the summary goes to stderr where a redirect leaves
    // it on the terminal.
    let summary = format!(
        "map: {map_path}\nmap checksum: 0x{checksum:08X}\nentities in lump: {}\n\
         estimated networked entities (map floor): {}\n\
         pre-Anniversary MAX_PACKET_ENTITIES: 256\n",
        entities.len(),
        on_wire.len()
    );

    if propose {
        eprintln!("{summary}");
        emit_proposal(&map_path, checksum, &bsp, &entities, &on_wire, target, reach.as_ref());
    } else {
        println!("{summary}");
        report(&bsp, &entities, &on_wire, reach.as_ref());
    }
}

fn report(bsp: &Bsp, entities: &[MapEntity], on_wire: &[usize], reach: Option<&std::collections::HashSet<(i32, i32, i32)>>) {
    let mut by_class: std::collections::BTreeMap<&str, usize> = Default::default();
    for i in on_wire {
        *by_class.entry(entities[*i].classname()).or_default() += 1;
    }
    println!("networked, by classname:");
    for (class, n) in &by_class {
        println!("  {n:>4}  {class}");
    }

    println!("\ninvisible (rendermode scales with renderamt, and renderamt <= {INVISIBLE_RENDERAMT}):");
    for i in on_wire {
        let e = &entities[*i];
        if invisible(e) {
            let (mode, amt) = e.render();
            println!(
                "  lump#{i:<4} {:<20} rendermode={mode} renderamt={amt}",
                e.classname()
            );
        }
    }

    let heading = if reach.is_some() {
        "\nfunc_illusionary, farthest from any proven player position first \
         (non-solid: removing one cannot change collision):"
    } else {
        "\nfunc_illusionary by face area (non-solid: removing one cannot change collision):"
    };
    println!("{heading}");
    let mut rows = illusionary_rows(bsp, entities, on_wire, reach);
    sort_illusionary_rows(&mut rows, reach.is_some());
    for row in &rows {
        match row.distance {
            Some(d) => println!(
                "  lump#{:<4} *{:<4} area={:>10.0}  distance={d:>8.0}{}  {}",
                row.lump_index, row.submodel, row.area,
                if row.background { "  (background)" } else { "" },
                row.texture,
            ),
            None => println!("  lump#{:<4} *{:<4} area={:>10.0}  {}", row.lump_index, row.submodel, row.area, row.texture),
        }
    }
}

struct IllusionaryRow {
    lump_index: usize,
    submodel: u32,
    area: f32,
    texture: String,
    distance: Option<f32>,
    background: bool,
}

fn illusionary_rows(
    bsp: &Bsp,
    entities: &[MapEntity],
    on_wire: &[usize],
    reach: Option<&std::collections::HashSet<(i32, i32, i32)>>,
) -> Vec<IllusionaryRow> {
    on_wire
        .iter()
        .filter(|i| entities[**i].classname() == "func_illusionary")
        .filter_map(|i| {
            let s = entities[*i].brush_submodel()?;
            let area = submodel_area(bsp, s);
            let texture = dominant_texture(bsp, s);
            let (distance, background) = match (reach, submodel_bounds(bsp, s)) {
                (Some(cloud), Some((lo, hi))) => {
                    let d = native::patch::reachability::distance_to_nearest_player(cloud, lo, hi);
                    (Some(d), native::patch::reachability::reads_as_background(d, lo, hi))
                }
                _ => (None, false),
            };
            Some(IllusionaryRow { lump_index: *i, submodel: s, area, texture, distance, background })
        })
        .collect()
}

/// Farthest-from-any-proven-position first when a harvest is available (the
/// stronger, more confident signal); otherwise the original smallest-area
/// first, unchanged from before this ranking existed.
fn sort_illusionary_rows(rows: &mut [IllusionaryRow], have_reach: bool) {
    if have_reach {
        rows.sort_by(|a, b| b.distance.unwrap_or(0.0).total_cmp(&a.distance.unwrap_or(0.0)));
    } else {
        rows.sort_by(|a, b| a.area.total_cmp(&b.area));
    }
}

fn emit_proposal(
    map_path: &str,
    checksum: u32,
    bsp: &Bsp,
    entities: &[MapEntity],
    on_wire: &[usize],
    target: usize,
    reach: Option<&std::collections::HashSet<(i32, i32, i32)>>,
) {
    let mut candidates: Vec<Candidate> = Vec::new();

    for i in on_wire {
        let e = &entities[*i];
        if invisible(e) {
            let (mode, amt) = e.render();
            candidates.push(Candidate {
                lump_index: *i,
                classname: e.classname().to_string(),
                tier: "A",
                note: format!("invisible: rendermode={mode} renderamt={amt}"),
            });
        }
    }
    for i in on_wire {
        let e = &entities[*i];
        // Already counted in tier A: a fog sprite dim enough to be invisible
        // is a free win either way, but listing it twice would inflate the
        // "reaches N" estimate and print its lump index twice in the strip list.
        if e.classname() == "env_sprite" && !invisible(e) {
            candidates.push(Candidate {
                lump_index: *i,
                classname: e.classname().to_string(),
                tier: "B",
                note: format!("atmosphere: {}", e.get("model").unwrap_or("?")),
            });
        }
    }

    let active = candidates.len();
    let floor_after_ab = on_wire.len().saturating_sub(active);

    println!("# strip list for {map_path}");
    println!("# map checksum 0x{checksum:08X} -- apply refuses a file whose checksum differs");
    println!("# format: <lump index> <classname>; the classname is checked before removal");
    println!("#");
    println!("# map floor now: {}   target: {target}", on_wire.len());
    println!("# a live half adds ~24-27 on top of the floor (players, dropped weapons),");
    println!("# so the floor -- not the limit -- is what has to leave room under 256.");
    println!("# tier A (invisible) + tier B (atmosphere) reach {floor_after_ab}");
    println!("#");
    println!("# tier A -- on the wire, not on the screen. Removing these changes");
    println!("#           nothing visible; brush ones also stop blocking movement.");
    println!("# tier B -- atmosphere sprites. A visible change, no collision change.");
    println!("# tier C -- func_illusionary, commented out below, ranked {}.",
        if reach.is_some() { "farthest from any proven player position first" } else { "smallest first" });
    println!("#           Non-solid, so removal cannot change collision -- but each one");
    println!("#           is real geometry a viewer would notice missing. Uncomment what");
    println!("#           you accept.");
    if reach.is_some() {
        println!("#           \"reach\" is how far past the point every player in the demos");
        println!("#           fed to --demos actually stood -- \"(background)\" means it clears");
        println!("#           the size-scaled margin this tool guesses at, not a proven fact.");
    }
    println!();

    for c in &candidates {
        println!("{:<5} {:<22} # tier {} -- {}", c.lump_index, c.classname, c.tier, c.note);
    }

    println!("\n# ---- tier C candidates ----");
    let mut rows = illusionary_rows(bsp, entities, on_wire, reach);
    sort_illusionary_rows(&mut rows, reach.is_some());

    // How many tier C lines it takes to reach the target, plus a margin, so
    // vetoing one does not mean regenerating the list.
    const MARGIN: usize = 25;
    let needed = floor_after_ab.saturating_sub(target);
    let listed = (needed + MARGIN).min(rows.len());

    let mut floor = floor_after_ab;
    for (n, row) in rows.iter().take(listed).enumerate() {
        if n == needed {
            println!("# ---- target reached above; the rest are spare ----");
        }
        floor = floor.saturating_sub(1);
        let reach_note = match row.distance {
            Some(d) => format!(" reach={d:.0}{}", if row.background { " (background)" } else { "" }),
            None => String::new(),
        };
        println!(
            "# {:<5} {:<22} # tier C -- *{} area={:.0}{reach_note} {}; would reach {floor}",
            row.lump_index, "func_illusionary", row.submodel, row.area, row.texture,
        );
    }
    if listed < rows.len() {
        println!("# ...{} more func_illusionary not listed", rows.len() - listed);
    }
}

fn apply(
    map_path: &str,
    bytes: &[u8],
    checksum: u32,
    entities: &[MapEntity],
    strip_path: &str,
    out_path: Option<&str>,
) {
    let Some(out_path) = out_path else {
        fail("--strip needs --out; this never writes over the map it read");
    };
    if std::path::Path::new(out_path) == std::path::Path::new(map_path) {
        fail("--out is the input map; refusing to overwrite it");
    }

    let list = std::fs::read_to_string(strip_path)
        .unwrap_or_else(|e| fail(&format!("{strip_path}: {e}")));

    // A strip list is written against one exact file. The checksum line the
    // proposal emits is what catches it being replayed against another build of
    // the map, where every lump index would name a different entity.
    for line in list.lines() {
        if let Some(rest) = line.strip_prefix("# map checksum 0x")
            && let Some(hex) = rest.split_whitespace().next()
            && let Ok(want) = u32::from_str_radix(hex, 16)
            && want != checksum
        {
            fail(&format!(
                "strip list was written for map checksum 0x{want:08X}, this map is 0x{checksum:08X}"
            ));
        }
    }

    let mut remove: std::collections::BTreeSet<usize> = Default::default();
    for (n, line) in list.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let index: usize = match parts.next().and_then(|v| v.parse().ok()) {
            Some(v) => v,
            None => fail(&format!("{strip_path}:{}: expected a lump index", n + 1)),
        };
        let expected = parts.next().unwrap_or("");
        let Some(entity) = entities.get(index) else {
            fail(&format!("{strip_path}:{}: lump index {index} is past the end", n + 1));
        };
        if !expected.is_empty() && entity.classname() != expected {
            fail(&format!(
                "{strip_path}:{}: lump#{index} is a {}, not a {expected}",
                n + 1,
                entity.classname()
            ));
        }
        if entity.classname() == "worldspawn" {
            fail(&format!("{strip_path}:{}: refusing to remove worldspawn", n + 1));
        }
        remove.insert(index);
    }

    let kept: Vec<MapEntity> = entities
        .iter()
        .enumerate()
        .filter(|(i, _)| !remove.contains(i))
        .map(|(_, e)| e.clone())
        .collect();

    let trimmed = rewrite_entity_lump(bytes, &serialize_entities(&kept)).unwrap_or_else(|e| fail(&e));
    let after = map_checksum(&trimmed).unwrap_or_else(|e| fail(&e));
    if after != checksum {
        fail(&format!(
            "map checksum moved (0x{checksum:08X} -> 0x{after:08X}); refusing to write, \
             a map that fails this no longer loads demos recorded against the original"
        ));
    }

    let before_floor = entities.iter().filter(|e| networked(e)).count();
    let after_floor = kept.iter().filter(|e| networked(e)).count();

    std::fs::write(out_path, &trimmed).unwrap_or_else(|e| fail(&format!("{out_path}: {e}")));
    println!("removed {} entities ({} -> {} in the lump)", remove.len(), entities.len(), kept.len());
    println!("estimated networked floor: {before_floor} -> {after_floor}");
    println!("map checksum unchanged at 0x{checksum:08X}");
    println!("wrote {out_path}");
}

fn fail(message: &str) -> ! {
    eprintln!("map_entity_trim: {message}");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use native::patch::bsp_entities::parse_entity_text;

    fn one(text: &str) -> MapEntity {
        parse_entity_text(text).unwrap().pop().unwrap()
    }

    /// The classes that hold an edict but never reach a snapshot. Counting
    /// these would have put `dod_thunder2`'s floor 54 too high, which is the
    /// error that would have sent a strip list after the wrong entities.
    #[test]
    fn nodraw_triggers_are_not_on_the_wire() {
        for class in NODRAW_BRUSH_CLASSES {
            let e = one(&format!("{{\n\"model\" \"*3\"\n\"classname\" \"{class}\"\n}}\n"));
            assert!(!networked(&e), "{class} should not be networked");
        }
        assert!(networked(&one("{\n\"model\" \"*3\"\n\"classname\" \"func_illusionary\"\n}\n")));
        // Invisible on the wire is still on the wire.
        assert!(networked(&one("{\n\"model\" \"*3\"\n\"classname\" \"func_ladder\"\n}\n")));
    }

    /// DoD makes decorative props static at spawn, so they cost nothing --
    /// unless something can trigger them.
    #[test]
    fn env_model_counts_only_when_it_has_a_targetname() {
        let plain = one("{\n\"model\" \"models/mapmodels/tree32.mdl\"\n\"classname\" \"env_model\"\n}\n");
        assert!(!networked(&plain));
        let named = one(
            "{\n\"model\" \"models/mapmodels/sandbags.mdl\"\n\"targetname\" \"bridge\"\n\"classname\" \"env_model\"\n}\n",
        );
        assert!(networked(&named));
    }

    /// `kRenderTransAlpha` is a binary alpha test that ignores `renderamt`, so
    /// a low value there does not mean the entity is invisible. Reading it as
    /// free to remove would delete visible fences and foliage.
    #[test]
    fn transalpha_is_never_treated_as_invisible() {
        let button =
            one("{\n\"model\" \"*3\"\n\"rendermode\" \"2\"\n\"classname\" \"func_button\"\n}\n");
        assert!(invisible(&button), "rendermode 2 with no renderamt is invisible");

        let fence = one(
            "{\n\"model\" \"*3\"\n\"rendermode\" \"4\"\n\"renderamt\" \"0\"\n\"classname\" \"func_illusionary\"\n}\n",
        );
        assert!(!invisible(&fence));

        let opaque = one("{\n\"model\" \"*3\"\n\"classname\" \"func_wall\"\n}\n");
        assert!(!invisible(&opaque));
    }
}
