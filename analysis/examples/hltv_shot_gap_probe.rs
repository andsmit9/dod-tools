//! Detects weapon-fire events that a recording *dropped*, using only that
//! recording -- no second demo to compare against.
//!
//! An automatic weapon fires at a fixed cyclic rate, so within a held burst
//! the gap between consecutive events is that rate. If a recording loses
//! rounds, the surviving gaps land on near-exact integer multiples of it. A
//! human releasing and re-pressing the trigger produces gaps too, but has no
//! reason to land on an exact multiple -- so "gap is within tolerance of n
//! times the cyclic rate, n >= 2" separates dropped rounds from deliberate
//! pauses.
//!
//! The cyclic rate is measured from the demo itself (the modal gap for that
//! weapon), so there is no hardcoded weapon table and it adapts to whatever
//! rate the recording actually ran at.
//!
//! Events are grouped per (weapon, shooter), because two players firing the
//! same weapon interleave into one stream otherwise and manufacture gaps that
//! were never one player's burst.
//!
//! **Validating the heuristic:** run it on a POV demo. Those record their own
//! player's fire essentially completely, so whatever this reports there is the
//! false-positive rate -- pauses being misread as drops. Compare that against
//! what it reports for an HLTV demo of the same match.
//!
//!     cargo run --release -p analysis --example hltv_shot_gap_probe -- demo.dem

use dem::bit::BitSliceCast;
use dem::open_demo_from_bytes;
use dem::types::{EngineMessage, FrameData, MessageData, NetMessage};
use std::collections::HashMap;

/// A gap counts as dropped rounds only if it lands this close to an exact
/// multiple of the cyclic rate (as a fraction of that rate).
const MULTIPLE_TOLERANCE: f32 = 0.25;
/// Gaps longer than this are a new engagement, not a burst with holes in it.
const MAX_BURST_GAP: f32 = 1.0;

/// Only full-automatic weapons have a cyclic rate for this to key off. On a
/// semi-automatic the gap between shots is just how fast the player clicked,
/// so "gap is twice the usual gap" carries no information at all and the
/// heuristic reports noise -- measured at 27.8% on the Luger in an HLTV demo
/// and 7.5% on the Garand, against 1.2% and 2.8% for the same weapons in a POV
/// demo, which is the tell that those numbers are the method failing rather
/// than rounds going missing.
const AUTOMATIC_WEAPONS: &[&str] =
    &["bar", "mp44", "mp40", "thompson", "sten", "greasegun", "bren", "mg42", "mg34", "30cal"];

/// Reads a delta field as a little-endian u32. Field names arrive padded, so
/// they are trimmed before matching.
fn delta_u32(delta: &dem::types::Delta, field: &str) -> Option<u32> {
    delta
        .iter()
        .find(|(k, _)| k.trim_matches(|c: char| c == '\0' || c.is_whitespace()) == field)
        .and_then(|(_, v)| (v.len() >= 4).then(|| u32::from_le_bytes([v[0], v[1], v[2], v[3]])))
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: hltv_shot_gap_probe <demo>");
    let bytes = std::fs::read(&path).expect("read demo");
    let demo = open_demo_from_bytes(&bytes).expect("parse demo");

    let mut event_names: HashMap<u32, String> = HashMap::new();
    // (event index, shooter entindex) -> fire times
    let mut fires: HashMap<(u32, u32), Vec<f32>> = HashMap::new();
    let mut unattributed = 0usize;
    let mut attributed = 0usize;
    let mut director_messages = 0usize;

    for entry in &demo.directory.entries {
        for frame in &entry.frames {
            let FrameData::NetworkMessage(bt) = &frame.frame_data else { continue };
            let MessageData::Parsed(msgs) = &bt.1.messages else { continue };
            for m in msgs {
                let NetMessage::EngineMessage(em) = m else { continue };
                match &**em {
                    EngineMessage::SvcResourceList(rl) => {
                        for r in &rl.resources {
                            if r.type_.to_u8() == 5 {
                                event_names.insert(r.index.to_u32(), r.name.get_string());
                            }
                        }
                    }
                    EngineMessage::SvcEvent(ev) => {
                        for e in &ev.events {
                            // `packet_index` is not a shooter id -- it reports
                            // 46 distinct values in a 6v6 half. The event's
                            // own delta carries the real `entindex`.
                            // entindex is a 10-bit field, so 0x3FF reads back
                            // as -1 ("no entity") rather than 1023.
                            let shooter = e
                                .delta
                                .as_ref()
                                .and_then(|d| delta_u32(d, "entindex"))
                                .filter(|v| *v > 0 && *v < 0x200);
                            match shooter {
                                Some(_) => attributed += 1,
                                None => unattributed += 1,
                            }
                            // Fall back to one pooled stream per weapon when the
                            // shooter is unknown -- HLTV demos carry no entindex
                            // at all, so refusing to measure them would defeat
                            // the point of the tool.
                            let key = (e.event_index.to_u32(), shooter.unwrap_or(u32::MAX));
                            fires.entry(key).or_default().push(frame.time);
                        }
                    }
                    EngineMessage::SvcEventReliable(_) => {}
                    // Only an HLTV proxy's recording carries the director
                    // stream that drives the auto-director's camera.
                    //
                    // Counting *any* svc_director is not enough: dod-studio
                    // injects its own into previews of POV demos, so those
                    // misreported as HLTV. Its injections are only
                    // DRC_CMD_MESSAGE (0x06) and DRC_CMD_STUFFTEXT (0x0A) --
                    // bookmarks and console commands -- and neither moves a
                    // camera, so excluding them separates a real director
                    // stream from a patched one.
                    EngineMessage::SvcDirector(d) => {
                        const DRC_CMD_MESSAGE: u8 = 0x06;
                        const DRC_CMD_STUFFTEXT: u8 = 0x0A;
                        if d.command != DRC_CMD_MESSAGE && d.command != DRC_CMD_STUFFTEXT {
                            director_messages += 1;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // Per weapon: every (shooter, times) stream, and every in-burst gap
    // pooled across shooters so the cyclic rate has enough samples to be
    // stable even when one player only fired a few rounds.
    let mut per_weapon: HashMap<String, Vec<Vec<f32>>> = HashMap::new();
    for ((idx, _shooter), mut times) in fires {
        let Some(name) = event_names.get(&idx) else { continue };
        if !name.contains("events/weapons/") {
            continue;
        }
        // Resource names arrive NUL-terminated, so strip that (and any
        // whitespace) before the ".sc" suffix, or the suffix never matches.
        let short = name
            .trim_matches(|c: char| c == '\0' || c.is_whitespace())
            .trim_start_matches("events/weapons/")
            .trim_end_matches(".sc")
            .to_string();
        if !AUTOMATIC_WEAPONS.contains(&short.as_str()) {
            continue;
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        per_weapon.entry(short).or_default().push(times);
    }

    // Which kind of recording this is decides whether the numbers below mean
    // anything for HLTV work. A POV demo records its own player's fire nearly
    // completely, so its loss figure is the method's false-positive rate, not
    // a real loss -- reading one as the other has already sent this
    // investigation down the wrong path once.
    let recording = if director_messages > 0 {
        format!("HLTV ({director_messages} director messages)")
    } else {
        "POV -- a player's own recording, NOT HLTV".to_string()
    };

    println!("=== {path} ===");
    println!("recording: {recording}");
    println!(
        "shooter attribution: {attributed} events carried an entindex, {unattributed} did not{}\n",
        if attributed == 0 {
            "  <-- NONE usable: gaps below pool every shooter of a weapon together, so they overstate loss"
        } else {
            ""
        }
    );
    println!(
        "{:<12} {:>7} {:>9} {:>8} {:>9} {:>9} {:>8}",
        "weapon", "shots", "shooters", "cyclic", "in burst", "dropped", "loss"
    );

    let mut rows: Vec<(String, Vec<Vec<f32>>)> = per_weapon.into_iter().collect();
    rows.sort_by_key(|(_, s)| std::cmp::Reverse(s.iter().map(|t| t.len()).sum::<usize>()));

    let (mut total_shots, mut total_dropped) = (0usize, 0usize);
    let mut bursts: Vec<Burst> = Vec::new();

    for (name, streams) in rows {
        let shots: usize = streams.iter().map(|t| t.len()).sum();
        if shots < 30 {
            continue;
        }

        let gaps: Vec<f32> = streams
            .iter()
            .flat_map(|t| t.windows(2).map(|w| w[1] - w[0]))
            .filter(|g| *g > 0.0 && *g < MAX_BURST_GAP)
            .collect();
        if gaps.len() < 10 {
            continue;
        }

        // Cyclic rate = modal gap, in 5ms buckets for a finer estimate than
        // the histogram this tool used to print.
        let mut buckets: HashMap<i32, usize> = HashMap::new();
        for g in &gaps {
            *buckets.entry((g / 0.005) as i32).or_insert(0) += 1;
        }
        let cyclic = buckets.iter().max_by_key(|(_, c)| **c).map(|(b, _)| (*b as f32 + 0.5) * 0.005).unwrap_or(0.0);
        if cyclic <= 0.0 {
            continue;
        }

        let mut dropped = 0usize;
        for gap in &gaps {
            let n = (gap / cyclic).round();
            if n >= 2.0 && (gap - n * cyclic).abs() <= MULTIPLE_TOLERANCE * cyclic {
                dropped += (n as usize) - 1;
            }
        }

        // Split each shooter's stream into runs of fire, using the same
        // "gap under MAX_BURST_GAP is still the same burst" rule the loss
        // estimate above works from.
        for times in &streams {
            let mut start = 0usize;
            for i in 1..=times.len() {
                let ends = i == times.len() || times[i] - times[i - 1] >= MAX_BURST_GAP;
                if ends {
                    bursts.push(Burst {
                        weapon: name.clone(),
                        start: times[start],
                        rounds: i - start,
                    });
                    start = i;
                }
            }
        }

        let recorded_plus_lost = shots + dropped;
        println!(
            "{name:<12} {shots:>7} {:>9} {:>7.0}ms {:>9} {dropped:>9} {:>7.1}%",
            streams.len(),
            cyclic * 1000.0,
            gaps.len(),
            100.0 * dropped as f32 / recorded_plus_lost as f32,
        );
        total_shots += shots;
        total_dropped += dropped;
    }

    if total_shots > 0 {
        println!(
            "\ntotal: {total_shots} recorded, {total_dropped} estimated dropped ({:.1}% loss)",
            100.0 * total_dropped as f32 / (total_shots + total_dropped) as f32
        );
    }

    report_longest_bursts(&bursts);
}

/// A run of automatic fire, as a place in the demo worth seeking to.
struct Burst {
    weapon: String,
    start: f32,
    rounds: usize,
}

/// Prints where the longest sustained bursts are.
///
/// Aggregate loss says whether a recording drops rounds; it does not say where
/// to look. Testing anything against sustained automatic fire otherwise means
/// spectating a demo and waiting for someone to hold a trigger down, which is
/// most of a session spent hunting -- these are timestamps to seek straight to.
fn report_longest_bursts(bursts: &[Burst]) {
    if bursts.is_empty() {
        return;
    }
    let mut longest: Vec<&Burst> = bursts.iter().filter(|b| b.rounds >= 5).collect();
    longest.sort_by_key(|b| std::cmp::Reverse(b.rounds));
    if longest.is_empty() {
        println!("\nno sustained bursts (5+ rounds) in this demo");
        return;
    }

    println!("\nlongest sustained bursts -- seek here to see automatic fire:");
    println!("{:<12} {:>7} {:>10}  seek to", "weapon", "rounds", "demo time");
    for b in longest.iter().take(12) {
        // A couple of seconds early, so the camera is settled and on the player
        // before the burst rather than arriving mid-way through it.
        let seek = (b.start - 3.0).max(0.0);
        println!(
            "{:<12} {:>7} {:>9.1}s  {:02}:{:04.1}",
            b.weapon,
            b.rounds,
            b.start,
            (seek / 60.0) as u32,
            seek % 60.0,
        );
    }
}
