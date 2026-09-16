//! What a demo's tickrate actually is, and how much real time the decal flush's
//! lead buys at it.
//!
//! ```text
//! cargo run --release -p native --bin demo_tickrate -- <demo-or-folder>
//! ```
//!
//! Exists because the obvious way to get this number is wrong. A directory
//! entry's `frame_count` and the number of frame records in the stream are
//! different quantities — every `NetworkMessage`, `ConsoleCommand`, `ClientData`
//! and `Event` frame is a record — and the pipeline works exclusively in the
//! second. Reading the header gives a figure that is right about nothing the
//! pipeline does. See `docs/goldsrc_dod_quirks.md`.
//!
//! The tickrate is also what makes a flat frame count useless as a margin: the
//! flush's lead was once a flat 300 frames, worth ~3s at 100 records/sec and
//! ~0.6s at 500. It is `DecalCleanOptions::lead_seconds` now, resolved by
//! walking the frames' own timestamps — this tool reports what that flat count
//! *would* have been worth, which is how the problem was found.

use native::patch::scanner::scan_demo_for_highlights;

/// The flat frame count the flush lead used to be, kept here so the tool can
/// show what it was worth per demo.
const LEAD_TICKS: f32 = 300.0;

fn main() {
    let target = std::path::PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: demo_tickrate <demo-or-folder>"),
    );

    let demos: Vec<std::path::PathBuf> = if target.is_dir() {
        let mut found: Vec<_> = std::fs::read_dir(&target)
            .expect("read folder")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dem")))
            .collect();
        found.sort();
        found
    } else {
        vec![target]
    };

    println!("demo\ttickrate\tflush_margin_seconds");
    let mut margins: Vec<f32> = Vec::new();

    for demo in &demos {
        let name = demo.file_name().unwrap_or_default().to_string_lossy();
        match scan_demo_for_highlights(demo) {
            Ok((tickrate, _, _, _, _, _, _)) if tickrate > 0.0 => {
                let margin = LEAD_TICKS / tickrate;
                margins.push(margin);
                println!("{}\t{:.1}\t{:.2}", name, tickrate, margin);
            }
            Ok(_) => println!("{}\tSKIP\tno usable tickrate", name),
            Err(e) => println!("{}\tSKIP\t{}", name, e),
        }
    }

    if margins.is_empty() {
        return;
    }
    margins.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let thin = margins.iter().filter(|m| **m < 1.0).count();
    eprintln!(
        "\n{} demos: margin min {:.2}s, median {:.2}s, max {:.2}s — {} under 1.0s",
        margins.len(),
        margins[0],
        margins[margins.len() / 2],
        margins[margins.len() - 1],
        thin
    );
}
