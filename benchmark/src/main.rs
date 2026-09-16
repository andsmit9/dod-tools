//! Demo Analyzer load-path profiler.
//!
//! Profiles the phases the Tauri `analyze_demo_full` command actually pays for
//! when the Demo Analyzer opens a demo, so "it takes a few seconds" can be
//! attributed rather than guessed at:
//!
//!   read        fs::read of the whole .dem into a Vec<u8>
//!   structural  Demo::parse_from_bytes(None) — walk directory/frames only
//!   decode      Demo::parse_from_bytes(Parse) — decode every netmessage
//!   events      the AnalyzerState event loop over every frame/netmessage
//!   serialize   serde_json of the IPC payload the webview receives
//!
//! It also classifies every netmessage in the stream as CONSUMED (something in
//! the `analysis` crate matches on it) or DISCARDED (fully decoded, then
//! dropped). The discarded share is the headroom available to a selective
//! parse mode.
//!
//! Usage: cargo run --release -p dod-benchmark -- <demo.dem | directory>...

use dem::types::{Demo, FrameData, MessageData, MessageDataParseMode, NetMessage};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

/// EngineMessage variants the `analysis` crate matches on.
/// Keep in sync with `grep -rho 'EngineMessage::[A-Za-z0-9]*' analysis/src/`.
const CONSUMED_ENGINE: &[&str] = &[
    "SvcDirector",
    "SvcHltv",
    "SvcServerInfo",
    "SvcStuffText",
    "SvcTime",
    "SvcUpdateUserInfo",
];

/// UserMessage names the analyzer accepts.
/// Mirrors `is_relevant_message` in analysis/src/lib.rs.
const CONSUMED_USER: &[&str] = &[
    "RoundState", "ClanTimer", "TimeLeft", "WaveTime", "TeamScore", "ScoreShort",
    "ObjScore", "Frags", "PClass", "PTeam", "ScoreInfo", "ScoreInfoLong", "SayText",
    "TextMsg", "DeathMsg", "PStatus", "Scope", "CurWeapon", "ReloadDone", "ResetHUD",
    "Health",
];

struct Row {
    name: String,
    size_mb: f64,
    read_ms: f64,
    structural_ms: f64,
    decode_ms: f64,
    drop_ms: f64,
    events_ms: f64,
    serial_ms: f64,
    json_mb: f64,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("Usage: dod-benchmark <demo.dem | directory>...");
        eprintln!("Profiles the Demo Analyzer load path phase by phase.");
        std::process::exit(1);
    }

    let mut files = vec![];
    for arg in &args {
        let p = PathBuf::from(arg);
        if p.is_dir() {
            collect(&p, &mut files);
        } else if p.is_file() {
            files.push(p);
        }
    }
    files.sort();

    if files.is_empty() {
        eprintln!("No .dem files found in the given paths.");
        std::process::exit(1);
    }

    let mut rows = vec![];
    let mut histogram: HashMap<(String, bool), usize> = HashMap::new();

    for path in &files {
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        eprintln!("Profiling {}...", name);

        // ── read ─────────────────────────────────────────────────────────────
        let t = Instant::now();
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("  skipped ({})", e);
                continue;
            }
        };
        let read_ms = ms_since(t);
        let size_mb = bytes.len() as f64 / 1_048_576.0;

        // ── structural-only decode (the floor: no netmessage parsing) ────────
        let t = Instant::now();
        match Demo::parse_from_bytes(&bytes, MessageDataParseMode::None) {
            Ok(d) => drop(d),
            Err(e) => {
                eprintln!("  skipped (structural parse error: {})", e);
                continue;
            }
        }
        let structural_ms = ms_since(t);

        // ── full decode ──────────────────────────────────────────────────────
        let t = Instant::now();
        let demo = match Demo::parse_from_bytes(&bytes, MessageDataParseMode::Parse) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("  skipped (parse error: {})", e);
                continue;
            }
        };
        let decode_ms = ms_since(t);

        // Classify the stream while the decoded demo is in hand.
        tally(&demo, &mut histogram);

        // ── drop of the decoded tree ─────────────────────────────────────────
        // Freeing the frame tree is NOT free: a 90 MB demo decodes into ~1 GB
        // of small allocations (Delta = HashMap<String, Vec<u8>> per entity
        // update), and releasing them costs real wall-clock time inside
        // `try_from_bytes`. Timing it separately keeps it from being silently
        // attributed to the event loop.
        let t = Instant::now();
        drop(demo);
        let drop_ms = ms_since(t);

        // ── event loop ───────────────────────────────────────────────────────
        // `try_from_bytes` = decode + event loop + drop of the tree, so back
        // out the two phases just measured under matching heap conditions.
        let t = Instant::now();
        let analysis = match analysis::Analysis::try_from_bytes(&bytes) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("  skipped (analysis error: {})", e);
                continue;
            }
        };
        let events_ms = (ms_since(t) - decode_ms - drop_ms).max(0.0);

        // ── serialize the IPC payload (mirrors AnalyzerReportPayload) ────────
        let t = Instant::now();
        let json = serde_json::to_string(&serde_json::json!({
            "demo_info": &analysis.demo_info,
            "state": &analysis.state,
        }))
        .unwrap_or_default();
        let serial_ms = ms_since(t);
        let json_mb = json.len() as f64 / 1_048_576.0;

        rows.push(Row {
            name,
            size_mb,
            read_ms,
            structural_ms,
            decode_ms,
            drop_ms,
            events_ms,
            serial_ms,
            json_mb,
        });
    }

    report(&rows, &histogram);
}

fn report(rows: &[Row], histogram: &HashMap<(String, bool), usize>) {
    if rows.is_empty() {
        eprintln!("Nothing profiled.");
        return;
    }

    println!("\n### Demo Analyzer load-path phase attribution\n");
    println!("| File | Size (MB) | read | structural | decode | drop | events | serialize | JSON (MB) | total |");
    println!("|------|-----------|------|------------|--------|------|--------|-----------|-----------|-------|");
    for r in rows {
        let total = r.read_ms + r.decode_ms + r.drop_ms + r.events_ms + r.serial_ms;
        println!(
            "| {} | {:.1} | {:.0} ms | {:.0} ms | {:.0} ms | {:.0} ms | {:.0} ms | {:.0} ms | {:.2} | {:.0} ms |",
            r.name, r.size_mb, r.read_ms, r.structural_ms, r.decode_ms, r.drop_ms, r.events_ms,
            r.serial_ms, r.json_mb, total
        );
    }

    let n = rows.len() as f64;
    let sum = |f: fn(&Row) -> f64| rows.iter().map(f).sum::<f64>();
    let (read, structural, decode, drop_t, events, serial) = (
        sum(|r| r.read_ms),
        sum(|r| r.structural_ms),
        sum(|r| r.decode_ms),
        sum(|r| r.drop_ms),
        sum(|r| r.events_ms),
        sum(|r| r.serial_ms),
    );
    let total = read + decode + drop_t + events + serial;

    println!("\n### Share of load time (n = {})\n", rows.len());
    let line = |label: &str, v: f64| {
        println!(
            "* {:<11} {:>7.0} ms total, {:>6.0} ms avg  ({:>4.1}%)",
            label,
            v,
            v / n,
            v / total * 100.0
        )
    };
    line("read:", read);
    line("decode:", decode);
    line("drop:", drop_t);
    line("events:", events);
    line("serialize:", serial);
    println!("\n* **Average end-to-end backend load: {:.0} ms**", total / n);
    println!("* Average IPC JSON payload: {:.2} MB", sum(|r| r.json_mb) / n);
    println!(
        "* Structural walk alone is {:.0} ms avg — so {:.0} ms ({:.0}%) of decode is netmessage parsing.",
        structural / n,
        (decode - structural) / n,
        (decode - structural) / decode * 100.0
    );

    // ── consumed vs discarded ────────────────────────────────────────────────
    let mut counts: Vec<(&(String, bool), &usize)> = histogram.iter().collect();
    counts.sort_by(|a, b| b.1.cmp(a.1));
    let grand: usize = counts.iter().map(|(_, c)| **c).sum();
    let consumed: usize = counts
        .iter()
        .filter(|((_, used), _)| *used)
        .map(|(_, c)| **c)
        .sum();

    println!(
        "\n### Netmessage stream: consumed vs discarded (total = {})\n",
        grand
    );
    println!(
        "* CONSUMED by the analyzer: {} ({:.1}%)",
        consumed,
        consumed as f64 / grand as f64 * 100.0
    );
    println!(
        "* DISCARDED after full decode: {} ({:.1}%)  <- selective-parse headroom",
        grand - consumed,
        (grand - consumed) as f64 / grand as f64 * 100.0
    );

    println!("\n| Message | Count | % of all | analyzer |");
    println!("|---------|-------|----------|----------|");
    for ((name, used), count) in counts.iter().take(25) {
        println!(
            "| {} | {} | {:.1}% | {} |",
            name,
            count,
            **count as f64 / grand as f64 * 100.0,
            if *used { "consumed" } else { "discarded" }
        );
    }
}

fn tally(demo: &Demo, histogram: &mut HashMap<(String, bool), usize>) {
    for entry in &demo.directory.entries {
        for frame in &entry.frames {
            let FrameData::NetworkMessage(bt) = &frame.frame_data else {
                continue;
            };
            let MessageData::Parsed(msgs) = &bt.1.messages else {
                continue;
            };
            for m in msgs {
                let (key, used) = match m {
                    NetMessage::UserMessage(u) => {
                        let mut n = u.name.len();
                        while n > 0 && u.name[n - 1] == 0 {
                            n -= 1;
                        }
                        let raw = String::from_utf8_lossy(&u.name[..n]).into_owned();
                        let used = CONSUMED_USER.contains(&raw.as_str());
                        (format!("UserMessage::{}", raw), used)
                    }
                    NetMessage::EngineMessage(e) => {
                        // Debug prints the variant name followed by its payload;
                        // the leading identifier is the variant.
                        let s = format!("{:?}", e);
                        let head: String = s.chars().take_while(|c| c.is_alphanumeric()).collect();
                        let used = CONSUMED_ENGINE.contains(&head.as_str());
                        (format!("EngineMessage::{}", head), used)
                    }
                };
                *histogram.entry((key, used)).or_insert(0) += 1;
            }
        }
    }
}

fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn collect(dir: &PathBuf, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.filter_map(Result::ok) {
            let p = entry.path();
            if p.is_dir() {
                collect(&p, out);
            } else if p.extension().is_some_and(|e| e == "dem") {
                out.push(p);
            }
        }
    }
}
