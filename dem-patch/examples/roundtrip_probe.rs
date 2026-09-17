//! Round-trips real demos through the parser and writer, printing one line per
//! demo so two builds can be diffed against each other.
//!
//! The four fixture tests in `src/lib.rs` cover open/write/parse-mode plumbing
//! against a 731-byte demo built in code — `DemoStart`, one `ConsoleCommand`,
//! `NextSection`. That demo has **no `NetworkMessage` frame at all**, so the
//! ~2,900 lines under `netmsg_doer/` — 53 message types, the delta decoder, the
//! temp-entity tree — are not exercised by anything in the test suite.
//!
//! This is what covers them. Point it at a folder of real demos before a change
//! and again after, and diff the two listings: an identical listing means every
//! demo parsed to the same structure *and* serialised to the same bytes, which
//! is the property a parser change has to preserve.
//!
//! It is not a test because it needs demos that cannot be committed — a single
//! real HLTV demo is ~100 MB, and the interesting ones are a match's worth.
//!
//! ```text
//! cargo run -p dem --release --example roundtrip_probe -- <dir-or-demo>... > before.txt
//! # ...make the change...
//! cargo run -p dem --release --example roundtrip_probe -- <dir-or-demo>... > after.txt
//! diff before.txt after.txt
//! ```
//!
//! `LIMIT=<n>` caps how many demos are processed, for a quick pass. Output is
//! `<fnv1a of the written bytes> <byte count> <path>`, sorted by path, with a
//! `PARSE-ERR` line for anything the parser refuses — a refusal is a difference
//! too, and diffing the listings catches it either way.

use std::path::{Path, PathBuf};

/// FNV-1a, matching `hl-demo-auditor`'s. Any hash would do; this one is here
/// already and is fast enough not to dominate a 100 MB demo's round trip.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

fn collect(root: &Path, out: &mut Vec<PathBuf>) {
    if root.is_file() {
        out.push(root.to_path_buf());
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("dem")) {
            out.push(path);
        }
    }
}

fn main() {
    let mut demos = Vec::new();
    for arg in std::env::args().skip(1) {
        collect(Path::new(&arg), &mut demos);
    }
    // Sorted so two runs list the same demos in the same order, which is what
    // makes a plain `diff` of the two listings meaningful.
    demos.sort();
    let limit: usize = std::env::var("LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(usize::MAX);

    let (mut ok, mut failed) = (0usize, 0usize);
    for path in demos.into_iter().take(limit) {
        match dem::open_demo(&path) {
            Ok(demo) => {
                let bytes = demo.write_to_bytes();
                ok += 1;
                println!("{:016x} {:>10} {}", fnv1a(&bytes), bytes.len(), path.display());
            }
            Err(e) => {
                failed += 1;
                println!("PARSE-ERR {e} {}", path.display());
            }
        }
    }
    // To stderr, so it does not end up in the listing being diffed.
    eprintln!("{ok} round-tripped, {failed} refused");
}
