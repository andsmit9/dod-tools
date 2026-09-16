//! R&D probe for #65 — OBS as an alternate capture method.
//!
//! See `docs/obs_alternate_capture.md`. This exists to answer the two
//! measurements that gate that design, and nothing else. It is a probe in the
//! same sense as `probe_decals`: throwaway-shaped, prints numbers, changes
//! nothing.
//!
//! Two modes, deliberately independent — the first needs no OBS at all, and is
//! the cheaper of the two to run:
//!
//!     probe_obs log <path-to-qconsole.log>
//!     probe_obs obs [--host H] [--port P] [--password S] [--record SECS]
//!
//! **`log` answers: can the console log carry the start signal?** The capture
//! pipeline already echoes `[dod-tools] START_RECORD - Tick N` at every stage
//! boundary of every block, and `-condebug` is on by default, so those lines
//! land in `qconsole.log` beside `hl.exe`. If the engine flushes them promptly,
//! that file is a tick-accurate signalling channel the app can tail — no new
//! console commands, no `host_framerate` hitch at the clip's first frame.
//!
//! This must be run **during** a capture, not against a saved log: a finished
//! file cannot show when its lines were flushed, only that they eventually
//! were. Start this first, then dispatch a one-clip batch.
//!
//! The failure it is looking for is buffering, and buffering has a signature:
//! several `[dod-tools]` lines arriving in a single read after a long silence.
//! Those are flagged as bursts, because a burst means the timestamps within it
//! are the reader's, not the engine's.
//!
//! **`obs` answers: what can this OBS install do, and how slow is starting a
//! recording?** The latency that matters is not how fast `StartRecord` returns
//! — it is the gap until `RecordStateChanged: OBS_WEBSOCKET_OUTPUT_STARTED`,
//! which is when frames actually begin. The design absorbs that gap inside the
//! pre-roll, so the number decides whether Option A holds.
//!
//! It also dumps `availableRequests`, which settles whether
//! `SetRecordDirectory` and `SplitRecordFile` exist on this build without
//! version archaeology.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, Instant};

/// How often the log tailer looks for new bytes.
///
/// Matches the `~16ms` polling cadence the codebase uses for external
/// processes, and it has to: a poll interval coarser than the latency being
/// measured would manufacture the very buffering this is looking for.
const LOG_POLL: Duration = Duration::from_millis(16);

/// How far apart in *demo ticks* two markers must be before arriving in the
/// same read counts as buffering.
///
/// The first version of this flagged any two lines under 40ms apart, which was
/// wrong and said so loudly across a whole 17-block batch: the pipeline
/// deliberately schedules commands one tick apart (`SPEED_FLUSH` right after
/// `CUSTOM_CMD1_BEFORE`, `CUSTOM_CMD2_AFTER` right after `STOP_RECORD`), and
/// one tick at ~474 demo fps is ~2ms. Those pairs *should* arrive together;
/// flagging them as buffering hid the thing actually being looked for.
///
/// Buffering means markers separated by real demo time landing in one read. A
/// hundred ticks is a fifth of a second at the rate above — comfortably longer
/// than any legitimately adjacent pair, comfortably shorter than the gaps
/// between stage boundaries.
const BURST_TICK_GAP: i64 = 100;

const MARKER: &str = "[dod-tools]";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("log") => match args.get(1) {
            Some(path) => tail_log(Path::new(path)),
            None => {
                eprintln!("usage: probe_obs log <path-to-qconsole.log>");
                std::process::exit(2);
            }
        },
        Some("obs") => run_obs(&args[1..]),
        Some("routing") => run_routing(&args[1..]),
        Some("ffmpegmode") => run_ffmpeg_mode(&args[1..]),
        _ => {
            eprintln!(
                "probe_obs — R&D probe for #65 (docs/obs_alternate_capture.md)\n\
                 \n\
                 usage:\n\
                 \x20 probe_obs log <path-to-qconsole.log>\n\
                 \x20     Tail the engine's console log during a live capture and timestamp\n\
                 \x20     every [dod-tools] line's arrival. Start this BEFORE dispatching the\n\
                 \x20     batch. A saved log cannot answer this.\n\
                 \n\
                 \x20 probe_obs obs [--host H] [--port P] [--password S] [--record SECS]\n\
                 \x20     Connect to obs-websocket, report what the install can do, and with\n\
                 \x20     --record time a real StartRecord/StopRecord round trip.\n\
                 \x20     Defaults: --host 127.0.0.1 --port 4455"
            );
            std::process::exit(2);
        }
    }
}

// ── Mode 1: console log latency ───────────────────────────────────────────────

struct Marker {
    at: Duration,
    /// The tick the echo named, when it named one. Paired with `at`, this is
    /// the cross-check that matters: between two markers inside a recorded
    /// window the engine is at `host_framerate 0`, so elapsed ticks and elapsed
    /// seconds should track each other. They will not during fast-forward, and
    /// seeing exactly that is how the reading is confirmed to be real.
    tick: Option<i64>,
    /// Which read of the file this line came out of. Lines sharing a read were
    /// flushed together, whatever their echoed order says.
    read_seq: u64,
}

fn tail_log(path: &Path) {
    println!("probe_obs log — watching {}", path.display());
    println!("Start a one-clip capture now. Ctrl-C when the batch ends.\n");

    // The capture engine deletes qconsole.log between runs, so the file may not
    // exist yet, and may be replaced underneath us. Both are handled by
    // watching for the length to go backwards and resetting.
    let mut offset: u64 = 0;
    let mut pending = String::new();
    let mut markers: Vec<Marker> = Vec::new();
    let mut t0: Option<Instant> = None;
    let mut read_seq: u64 = 0;
    let mut announced = false;

    loop {
        std::thread::sleep(LOG_POLL);

        let mut file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(_) => {
                offset = 0;
                pending.clear();
                continue;
            }
        };
        let len = match file.metadata() {
            Ok(m) => m.len(),
            Err(_) => continue,
        };
        if len < offset {
            // Truncated or recreated — the previous batch's log was cleaned up.
            println!("  -- log reset (truncated or recreated) --");
            offset = 0;
            pending.clear();
        }
        if len == offset {
            continue;
        }
        if !announced {
            announced = true;
            println!("  -- log is being written --");
        }

        if file.seek(SeekFrom::Start(offset)).is_err() {
            continue;
        }
        let mut buf = Vec::new();
        if file.read_to_end(&mut buf).is_err() {
            continue;
        }
        let arrived = Instant::now();
        offset += buf.len() as u64;
        read_seq += 1;

        // The engine writes whatever encoding the user's console produced, and
        // a partial line is normal at a read boundary. Lossy is correct here:
        // this is a timing probe, not a transcript.
        pending.push_str(&String::from_utf8_lossy(&buf));
        let mut lines: Vec<String> = pending.split('\n').map(str::to_string).collect();
        // The last element is either empty (clean boundary) or a partial line.
        pending = lines.pop().unwrap_or_default();

        for line in lines {
            let line = line.trim_end_matches('\r').trim();
            if !line.contains(MARKER) {
                continue;
            }
            let base = *t0.get_or_insert(arrived);
            let at = arrived.saturating_duration_since(base);
            let tick = parse_tick(line);
            let label = line
                .split_once(MARKER)
                .map(|(_, rest)| rest.trim().to_string())
                .unwrap_or_else(|| line.to_string());

            let prev = markers.last();
            let delta = prev.map(|p| at.saturating_sub(p.at));
            let same_read = prev.map(|p| p.read_seq == read_seq).unwrap_or(false);
            let tick_delta = match (prev.and_then(|p| p.tick), tick) {
                (Some(a), Some(b)) => Some(b - a),
                _ => None,
            };
            // Buffering is markers separated by real demo time arriving in one
            // read — not merely arriving close together, which adjacent
            // scheduled commands are supposed to do.
            let burst = same_read && tick_delta.map(|t| t.abs() > BURST_TICK_GAP).unwrap_or(false);

            println!(
                "  +{:>8.3}s  {:<44} {:>9}{:>12}{}",
                at.as_secs_f64(),
                truncate(&label, 44),
                match delta {
                    Some(d) => format!("+{:.3}s", d.as_secs_f64()),
                    None => "first".to_string(),
                },
                match tick_delta {
                    Some(t) => format!("{:+} ticks", t),
                    None => String::new(),
                },
                if burst && prev.is_some() {
                    "   <-- BURST (flushed with the line above)"
                } else {
                    ""
                }
            );

            markers.push(Marker { at, tick, read_seq });
        }
    }
}

/// The tick an echo names, e.g. `START_RECORD - Tick 41450`.
fn parse_tick(line: &str) -> Option<i64> {
    let idx = line.find("Tick ")?;
    line[idx + 5..]
        .split(|c: char| !c.is_ascii_digit())
        .find(|s| !s.is_empty())?
        .parse()
        .ok()
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
    }
}

// ── Mode 2: obs-websocket ─────────────────────────────────────────────────────

/// Requests this design depends on, checked against `availableRequests` so the
/// answer comes from the install rather than from a version number.
const WANTED: &[(&str, &str)] = &[
    ("StartRecord", "required"),
    ("StopRecord", "required — returns outputPath"),
    ("GetRecordStatus", "required — per-block verification"),
    ("GetRecordDirectory", "required — preflight + disk check"),
    ("GetVideoSettings", "required — the rate that replaces mirv_movie_fps"),
    ("SetRecordDirectory", "Option A per-block export routing"),
    ("SplitRecordFile", "Option B"),
    ("GetSceneList", "scene picker (read-only)"),
    ("GetSceneItemList", "scene picker — what a scene contains (read-only)"),
    ("GetInputList", "scene picker — is anything pointed at hl.exe (read-only)"),
    ("GetInputSettings", "scene picker — which window a capture targets (read-only)"),
    ("GetSceneCollectionList", "scene picker — scene names are per-collection"),
    ("SetCurrentProgramScene", "switch to the chosen scene (reversible mutation)"),
    ("SetVideoSettings", "canvas/output resolution and FPS — PROFILE-WIDE"),
    ("GetProfileList", "profiles — where video settings actually live"),
    ("SetCurrentProfile", "switch to a dod-tools profile (reversible)"),
    ("CreateProfile", "make a dod-tools profile instead of editing theirs"),
    ("GetSceneItemTransform", "a source's placement, in canvas coordinates"),
    ("SetSceneItemTransform", "re-fit a source after a canvas change"),
];

/// Input kinds worth recognising when reporting what a scene holds.
///
/// The left column is OBS's `inputKind`; the right is what it means for this
/// feature. Anything not listed is passed through as-is rather than hidden —
/// the point of the probe is to find out what is actually there.
fn describe_input_kind(kind: &str) -> &str {
    match kind {
        "game_capture" => "game capture — the one that may collide with AfxHookGoldSrc",
        "window_capture" => "window capture — the fallback, cannot collide",
        "monitor_capture" => "display capture — last resort",
        "wasapi_output_capture" => "desktop audio — captures the whole machine",
        "wasapi_process_output_capture" => "application audio — can target hl.exe alone",
        "wasapi_input_capture" => "microphone — almost certainly unwanted in a clip",
        _ => "",
    }
}

struct ObsArgs {
    host: String,
    port: u16,
    password: Option<String>,
    record: Option<u64>,
}

fn run_obs(args: &[String]) {
    let mut a = ObsArgs {
        host: "127.0.0.1".into(),
        port: 4455,
        password: None,
        record: None,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--host" => { a.host = args.get(i + 1).cloned().unwrap_or_default(); i += 2; }
            "--port" => { a.port = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(4455); i += 2; }
            "--password" => { a.password = args.get(i + 1).cloned(); i += 2; }
            "--record" => { a.record = args.get(i + 1).and_then(|s| s.parse().ok()).or(Some(5)); i += 2; }
            other => { eprintln!("unknown argument: {}", other); std::process::exit(2); }
        }
    }

    let url = format!("ws://{}:{}", a.host, a.port);
    println!("probe_obs obs — connecting to {}\n", url);

    let mut client = match ObsClient::connect(&url, a.password.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("connect failed: {}", e);
            eprintln!(
                "\nIs OBS running, and is Tools -> WebSocket Server Settings enabled?\n\
                 If a password is set there, pass it with --password."
            );
            std::process::exit(1);
        }
    };

    // ── What this install can do ──────────────────────────────────────────────
    match client.request("GetVersion", serde_json::json!({})) {
        Ok(d) => {
            println!(
                "  OBS {}  ·  obs-websocket {}  ·  rpcVersion {}",
                d["obsVersion"].as_str().unwrap_or("?"),
                d["obsWebSocketVersion"].as_str().unwrap_or("?"),
                d["rpcVersion"]
            );
            let available: Vec<&str> = d["availableRequests"]
                .as_array()
                .map(|v| v.iter().filter_map(|x| x.as_str()).collect())
                .unwrap_or_default();
            println!("  availableRequests: {}\n", available.len());
            for (name, why) in WANTED {
                println!(
                    "    {:<20} {:<4}  {}",
                    name,
                    if available.contains(name) { "YES" } else { "NO" },
                    why
                );
            }
            println!();
        }
        Err(e) => println!("  GetVersion failed: {}\n", e),
    }

    for (req, label) in [
        ("GetVideoSettings", "video"),
        ("GetRecordDirectory", "record dir"),
        ("GetRecordStatus", "record status"),
        ("GetCurrentProgramScene", "scene"),
    ] {
        match client.request(req, serde_json::json!({})) {
            Ok(d) => println!("  {:<14} {}", label, compact(&d)),
            Err(e) => println!("  {:<14} failed: {}", label, e),
        }
    }

    dump_record_mode(&mut client);
    dump_scenes(&mut client);

    let Some(secs) = a.record else {
        println!(
            "\nNo --record given, so nothing was recorded. Re-run with e.g. `--record 5`\n\
             to measure the start latency that Option A has to fit inside the pre-roll."
        );
        return;
    };

    // ── The measurement that matters ──────────────────────────────────────────
    println!("\n  recording {}s — timing the gap that Option A has to absorb", secs);

    // A Game Capture source whose target process is gone records black, and the
    // resulting file is valid in every other respect: right resolution, right
    // frame rate, right duration, even real audio if another source supplies
    // it. That is a measurement which looks successful and answers nothing —
    // and it cost a full round of analysis before the game turned out to have
    // exited. Checked here so the next person is told instead of deducing it.
    if !game_is_running() {
        println!(
            "\n  WARNING: hl.exe is not running.\n\
             \x20 Game Capture will record black, and the file will otherwise look correct.\n\
             \x20 Launch the game first if you meant to measure whether OBS can see it.\n"
        );
    }

    if client
        .request("GetRecordStatus", serde_json::json!({}))
        .map(|d| d["outputActive"].as_bool().unwrap_or(false))
        .unwrap_or(false)
    {
        println!("  REFUSING: OBS is already recording. Stop it first.");
        return;
    }

    let t0 = Instant::now();
    match client.request("StartRecord", serde_json::json!({})) {
        Ok(_) => println!("    StartRecord returned         +{:.3}s", t0.elapsed().as_secs_f64()),
        Err(e) => { println!("    StartRecord failed: {}", e); return; }
    }
    match client.wait_for_record_state("OBS_WEBSOCKET_OUTPUT_STARTED", Duration::from_secs(15)) {
        Some(_) => println!(
            "    frames actually start        +{:.3}s   <-- THE NUMBER\n\
             \x20                                        (pre-roll budget is >= 2.0s)",
            t0.elapsed().as_secs_f64()
        ),
        None => println!("    never saw RecordStateChanged STARTED within 15s"),
    }

    std::thread::sleep(Duration::from_secs(secs));

    let t1 = Instant::now();
    match client.request("StopRecord", serde_json::json!({})) {
        Ok(d) => println!(
            "\n    StopRecord returned          +{:.3}s\n    outputPath: {}",
            t1.elapsed().as_secs_f64(),
            d["outputPath"].as_str().unwrap_or("(none reported)")
        ),
        Err(e) => println!("\n    StopRecord failed: {}", e),
    }
    if let Some(ev) = client.wait_for_record_state("OBS_WEBSOCKET_OUTPUT_STOPPED", Duration::from_secs(30)) {
        println!(
            "    file finalised               +{:.3}s\n    outputPath: {}",
            t1.elapsed().as_secs_f64(),
            ev["outputPath"].as_str().unwrap_or("(none reported)")
        );
    }
}

/// Does `SetRecordDirectory` steer *Custom Output (FFmpeg)* mode?
///
/// **This mode writes to the user's OBS profile** and restores it afterwards.
/// It exists because the answer decides whether Custom Output — the mode that
/// unlocks lossless, all-intra capture codecs — can coexist with Option A's
/// per-block export-pool routing. If `SetRecordDirectory` only steers the
/// standard output, choosing lossless costs drive routing, and that is a real
/// trade-off the design has to state rather than discover later.
///
/// Deliberately **does not record**. Custom Output with no encoder configured
/// would simply fail, and the path question is answerable from the settings
/// alone — `AdvOut/FFFilePath` either follows the request or it does not. Not
/// recording also means nothing here can leave a broken output configuration
/// behind mid-capture.
fn run_ffmpeg_mode(args: &[String]) {
    let mut host = "127.0.0.1".to_string();
    let mut port: u16 = 4455;
    let mut password: Option<String> = None;
    let mut target = "C:\\dod-tools-routing-probe".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--host" => { host = args.get(i + 1).cloned().unwrap_or_default(); i += 2; }
            "--port" => { port = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(4455); i += 2; }
            "--password" => { password = args.get(i + 1).cloned(); i += 2; }
            "--dir" => { target = args.get(i + 1).cloned().unwrap_or(target); i += 2; }
            other => { eprintln!("unknown argument: {}", other); std::process::exit(2); }
        }
    }

    let url = format!("ws://{}:{}", host, port);
    let mut client = match ObsClient::connect(&url, password.as_deref()) {
        Ok(c) => c,
        Err(e) => { eprintln!("connect failed: {}", e); std::process::exit(1); }
    };

    // Back up first, and refuse to touch anything if the backup is incomplete.
    let backup: Vec<(&str, &str, Option<String>)> = ["RecType", "FFFilePath"]
        .iter()
        .map(|k| ("AdvOut", *k, profile_get(&mut client, "AdvOut", k)))
        .collect();
    let orig_dir = client
        .request("GetRecordDirectory", serde_json::json!({}))
        .ok()
        .and_then(|d| d["recordDirectory"].as_str().map(str::to_string));
    let Some(orig_dir) = orig_dir else {
        eprintln!("could not read the record directory — aborting before writing anything");
        std::process::exit(1);
    };

    println!("  backup taken:");
    for (cat, key, val) in &backup {
        println!("    {}/{:<12} = {}", cat, key, val.clone().unwrap_or_else(|| "(unset)".into()));
    }
    println!("    record directory  = {}\n", orig_dir);

    let result = ffmpeg_mode_experiment(&mut client, &target);

    // Restore on every path, including a panic-free error return above.
    println!("\n  restoring profile …");
    for (cat, key, val) in &backup {
        match val {
            Some(v) => {
                let ok = profile_set(&mut client, cat, key, v);
                println!("    {}/{:<12} -> {}   [{}]", cat, key, v, if ok { "ok" } else { "FAILED" });
            }
            None => println!("    {}/{:<12} was unset; left alone", cat, key),
        }
    }
    let ok = client
        .request("SetRecordDirectory", serde_json::json!({ "recordDirectory": orig_dir }))
        .is_ok();
    println!("    record directory -> {}   [{}]", orig_dir, if ok { "ok" } else { "FAILED" });

    match profile_get(&mut client, "AdvOut", "RecType") {
        Some(t) => println!("\n  recording type is now: {}", t),
        None => println!("\n  recording type is now: (unset — Standard)"),
    }

    if let Err(e) = result {
        eprintln!("\n  experiment did not complete: {}", e);
    }
}

fn ffmpeg_mode_experiment(client: &mut ObsClient, target: &str) -> Result<(), String> {
    println!("  1. switching AdvOut/RecType to FFmpegOutput");
    if !profile_set(client, "AdvOut", "RecType", "FFmpegOutput") {
        return Err("SetProfileParameter refused".into());
    }
    let now = profile_get(client, "AdvOut", "RecType").unwrap_or_default();
    println!("     readback: {}   [{}]", now, if now == "FFmpegOutput" { "TOOK" } else { "IGNORED" });
    if now != "FFmpegOutput" {
        return Err("could not switch mode; nothing further is meaningful".into());
    }

    let path_before = profile_get(client, "AdvOut", "FFFilePath");
    let dir_before = client
        .request("GetRecordDirectory", serde_json::json!({}))?
        ["recordDirectory"].as_str().unwrap_or_default().to_string();
    println!("\n  2. before SetRecordDirectory");
    println!("     AdvOut/FFFilePath     : {}", path_before.clone().unwrap_or_else(|| "(unset)".into()));
    println!("     GetRecordDirectory    : {}", dir_before);

    println!("\n  3. SetRecordDirectory -> {}", target);
    client.request("SetRecordDirectory", serde_json::json!({ "recordDirectory": target }))?;

    let path_after = profile_get(client, "AdvOut", "FFFilePath");
    let dir_after = client
        .request("GetRecordDirectory", serde_json::json!({}))?
        ["recordDirectory"].as_str().unwrap_or_default().to_string();
    println!("\n  4. after");
    println!("     AdvOut/FFFilePath     : {}", path_after.clone().unwrap_or_else(|| "(unset)".into()));
    println!("     GetRecordDirectory    : {}", dir_after);

    println!("\n  VERDICT");
    let ff_followed = path_after.as_deref().map(|p| same_dir(p, target)).unwrap_or(false);
    let get_followed = same_dir(&dir_after, target);
    if ff_followed {
        println!("     SetRecordDirectory steers the Custom Output path.");
        println!("     Lossless capture and Option A per-block routing can coexist.");
    } else if get_followed {
        println!("     GetRecordDirectory reports the new path, but AdvOut/FFFilePath did NOT change.");
        println!("     The request is steering the *standard* output only, so in Custom Output");
        println!("     mode the recording would still land in the old place. Per-block routing");
        println!("     and lossless capture are mutually exclusive unless FFFilePath is written");
        println!("     directly via SetProfileParameter — which is a profile write, not a");
        println!("     recording call, and belongs behind the same consent as everything else.");
    } else {
        println!("     Neither followed. Investigate before relying on either.");
    }
    Ok(())
}

fn profile_get(client: &mut ObsClient, category: &str, name: &str) -> Option<String> {
    client
        .request(
            "GetProfileParameter",
            serde_json::json!({ "parameterCategory": category, "parameterName": name }),
        )
        .ok()
        .and_then(|d| d["parameterValue"].as_str().map(str::to_string))
        .filter(|s| !s.is_empty())
}

fn profile_set(client: &mut ObsClient, category: &str, name: &str, value: &str) -> bool {
    client
        .request(
            "SetProfileParameter",
            serde_json::json!({
                "parameterCategory": category,
                "parameterName": name,
                "parameterValue": value
            }),
        )
        .is_ok()
}

/// Which recording mode is active, and what it is configured to write.
///
/// This matters more than it looks: OBS's *Custom Output (FFmpeg)* mode keeps
/// its own output path and its own encoder settings, so answers gathered in
/// Standard mode do not necessarily transfer to it. Reporting the mode
/// alongside every other result is what stops a favourable measurement being
/// quietly attributed to the wrong configuration.
///
/// Read through `GetProfileParameter`, which is read-only. The parameter names
/// are OBS's own `basic.ini` keys, so they are stable but undocumented — an
/// absent one reports as unset rather than as an error.
fn dump_record_mode(client: &mut ObsClient) {
    println!("\n  ── recording mode ──────────────────────────────────────────────");

    let mut get = |category: &str, name: &str| -> Option<String> {
        client
            .request(
                "GetProfileParameter",
                serde_json::json!({ "parameterCategory": category, "parameterName": name }),
            )
            .ok()
            .and_then(|d| {
                d["parameterValue"]
                    .as_str()
                    .or_else(|| d["defaultParameterValue"].as_str())
                    .map(str::to_string)
            })
            .filter(|s| !s.is_empty())
    };

    let mode = get("Output", "Mode").unwrap_or_else(|| "Simple".into());
    println!("    output mode     : {}", mode);

    if mode.eq_ignore_ascii_case("Advanced") {
        let rec_type = get("AdvOut", "RecType").unwrap_or_else(|| "Standard".into());
        // OBS writes "FFmpegOutput" for the Custom Output (FFmpeg) selection.
        let custom = rec_type.eq_ignore_ascii_case("FFmpegOutput");
        println!(
            "    recording type  : {}{}",
            rec_type,
            if custom { "   <-- Custom Output (FFmpeg)" } else { "   (Standard)" }
        );
        if custom {
            for (label, cat, key) in [
                ("container", "AdvOut", "FFFormat"),
                ("video codec", "AdvOut", "FFVEncoder"),
                ("audio codec", "AdvOut", "FFAEncoder"),
                ("video bitrate", "AdvOut", "FFVBitrate"),
                ("keyframe (frames)", "AdvOut", "FFGOPSize"),
                ("muxer settings", "AdvOut", "FFMCustom"),
                ("encoder settings", "AdvOut", "FFVCustom"),
                ("path", "AdvOut", "FFFilePath"),
            ] {
                println!("      {:<18}: {}", label, get(cat, key).unwrap_or_else(|| "(unset)".into()));
            }
            println!(
                "\n    NOTE: Custom Output (FFmpeg) has its own path field (FFFilePath).\n\
                 \x20   Whether SetRecordDirectory steers it is what `probe_obs routing` tests."
            );
        } else {
            println!("      container       : {}", get("AdvOut", "RecFormat2").unwrap_or_else(|| "(unset)".into()));
            println!("      encoder         : {}", get("AdvOut", "RecEncoder").unwrap_or_else(|| "(unset)".into()));
        }
    } else {
        println!("      container       : {}", get("SimpleOutput", "RecFormat2").unwrap_or_else(|| "(unset)".into()));
        println!("      quality         : {}", get("SimpleOutput", "RecQuality").unwrap_or_else(|| "(unset)".into()));
        println!("      encoder         : {}", get("SimpleOutput", "RecEncoder").unwrap_or_else(|| "(unset)".into()));
    }
}

/// Does per-block export routing survive the recording mode in use?
///
/// **This mode writes to the user's OBS settings**, unlike every other part of
/// this probe. It is here because the answer cannot be read: `SetRecordDirectory`
/// is documented against the standard recording output, and OBS's *Custom
/// Output (FFmpeg)* mode keeps its own "File path or URL" field. Whether the
/// request steers that field decides whether Option A's per-block drive routing
/// works at all in that mode — and per-block routing is the main thing Option A
/// has over Option B.
///
/// Three answers come out of one run:
///
/// 1. does `SetRecordDirectory` take, and read back?
/// 2. does the file actually land where it says — or in the old directory?
/// 3. does `StopRecord` still report an `outputPath`, which take verification
///    keys off, and does `SplitRecordFile` work here?
///
/// The original directory is restored on every path out, including failures.
/// The caller's setting is not ours to keep.
fn run_routing(args: &[String]) {
    let mut host = "127.0.0.1".to_string();
    let mut port: u16 = 4455;
    let mut password: Option<String> = None;
    let mut target: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--host" => { host = args.get(i + 1).cloned().unwrap_or_default(); i += 2; }
            "--port" => { port = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(4455); i += 2; }
            "--password" => { password = args.get(i + 1).cloned(); i += 2; }
            "--dir" => { target = args.get(i + 1).cloned(); i += 2; }
            other => { eprintln!("unknown argument: {}", other); std::process::exit(2); }
        }
    }
    let Some(target) = target else {
        eprintln!("usage: probe_obs routing --dir <writable-dir> [--password S]");
        std::process::exit(2);
    };
    if let Err(e) = std::fs::create_dir_all(&target) {
        eprintln!("could not create {}: {}", target, e);
        std::process::exit(1);
    }

    let url = format!("ws://{}:{}", host, port);
    let mut client = match ObsClient::connect(&url, password.as_deref()) {
        Ok(c) => c,
        Err(e) => { eprintln!("connect failed: {}", e); std::process::exit(1); }
    };

    let original = client
        .request("GetRecordDirectory", serde_json::json!({}))
        .ok()
        .and_then(|d| d["recordDirectory"].as_str().map(str::to_string));
    let Some(original) = original else {
        eprintln!("could not read the current record directory — aborting rather than guessing");
        std::process::exit(1);
    };
    println!("  original record directory : {}", original);
    println!("  routing test target       : {}\n", target);

    // Everything from here restores `original` before returning.
    let outcome = routing_experiment(&mut client, &target);

    print!("\n  restoring record directory … ");
    match client.request(
        "SetRecordDirectory",
        serde_json::json!({ "recordDirectory": original }),
    ) {
        Ok(_) => {
            let back = client
                .request("GetRecordDirectory", serde_json::json!({}))
                .ok()
                .and_then(|d| d["recordDirectory"].as_str().map(str::to_string))
                .unwrap_or_default();
            if same_dir(&back, &original) {
                println!("restored to {}", back);
            } else {
                println!("MISMATCH — OBS now reports {}. Set it back by hand.", back);
            }
        }
        Err(e) => println!("FAILED: {} — set it back by hand: {}", e, original),
    }

    if let Err(e) = outcome {
        eprintln!("\n  experiment did not complete: {}", e);
    }
}

/// The middle of `run_routing`, split out so the restore above always runs.
fn routing_experiment(client: &mut ObsClient, target: &str) -> Result<(), String> {
    println!("  1. SetRecordDirectory");
    client.request(
        "SetRecordDirectory",
        serde_json::json!({ "recordDirectory": target }),
    )?;
    let readback = client
        .request("GetRecordDirectory", serde_json::json!({}))?
        ["recordDirectory"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    println!(
        "     readback: {}   [{}]",
        readback,
        if same_dir(&readback, target) { "TOOK" } else { "IGNORED" }
    );

    // A directory listing before and after is what actually answers where the
    // file went. `outputPath` is a claim; the filesystem is the fact, and in
    // Custom Output (FFmpeg) mode the two are exactly what might disagree.
    let before = dir_snapshot(target);

    println!("\n  2. recording 4s");
    if client
        .request("GetRecordStatus", serde_json::json!({}))
        .map(|d| d["outputActive"].as_bool().unwrap_or(false))
        .unwrap_or(false)
    {
        return Err("OBS is already recording — refusing to interfere".into());
    }
    client.request("StartRecord", serde_json::json!({}))?;
    if client
        .wait_for_record_state("OBS_WEBSOCKET_OUTPUT_STARTED", Duration::from_secs(15))
        .is_none()
    {
        return Err("recording never started — check the encoder is configured".into());
    }

    std::thread::sleep(Duration::from_millis(1800));
    println!("\n  3. SplitRecordFile mid-recording");
    match client.request("SplitRecordFile", serde_json::json!({})) {
        Ok(_) => println!("     SUPPORTED — Option B is possible in this mode"),
        Err(e) => println!("     NOT SUPPORTED HERE — {}", e),
    }
    std::thread::sleep(Duration::from_millis(1800));

    println!("\n  4. StopRecord");
    let stop = client.request("StopRecord", serde_json::json!({}))?;
    let reported = stop["outputPath"].as_str().unwrap_or("");
    println!(
        "     outputPath: {}",
        if reported.is_empty() { "(EMPTY — take verification cannot key off this)" } else { reported }
    );
    if let Some(ev) = client.wait_for_record_state("OBS_WEBSOCKET_OUTPUT_STOPPED", Duration::from_secs(30)) {
        let ev_path = ev["outputPath"].as_str().unwrap_or("");
        println!(
            "     RecordStateChanged outputPath: {}",
            if ev_path.is_empty() { "(empty)" } else { ev_path }
        );
    }

    println!("\n  5. where the files actually landed");
    let after = dir_snapshot(target);
    let new: Vec<&String> = after.iter().filter(|f| !before.contains(*f)).collect();
    if new.is_empty() {
        println!("     NOTHING new in {} — the output went elsewhere.", target);
        println!("     That means SetRecordDirectory does not steer this recording mode,");
        println!("     and Option A's per-block drive routing does not work here.");
    } else {
        for f in &new {
            println!("     {}", f);
        }
        println!(
            "     {} new file(s) — routing works in this mode.",
            new.len()
        );
    }
    Ok(())
}

/// Windows paths differ by separator and case without differing in meaning.
fn same_dir(a: &str, b: &str) -> bool {
    fn norm(s: &str) -> String {
        s.replace('/', "\\").trim_end_matches('\\').to_lowercase()
    }
    norm(a) == norm(b)
}

fn dir_snapshot(dir: &str) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// Everything a scene picker in the app would need, dumped read-only.
///
/// The question this answers is whether dod-tools can populate a dropdown of
/// scenes and tell the user, per scene, whether it is actually usable for a
/// capture — rather than making them describe their OBS setup by hand. All of
/// it is reads: no scene is created, switched or modified here.
///
/// Scene names are scoped to a **scene collection**, which is why the
/// collection is reported alongside them. A scene remembered by name is not
/// meaningful on its own — the user can switch collections and the name either
/// vanishes or, worse, resolves to something unrelated.
fn dump_scenes(client: &mut ObsClient) {
    println!("\n  ── scenes (read-only; nothing here is modified) ─────────────────");

    match client.request("GetSceneCollectionList", serde_json::json!({})) {
        Ok(d) => println!(
            "    collection: {}   (of {})",
            d["currentSceneCollectionName"].as_str().unwrap_or("?"),
            d["sceneCollections"].as_array().map(Vec::len).unwrap_or(0)
        ),
        Err(e) => println!("    GetSceneCollectionList failed: {}", e),
    }

    // Input kind is on the input, not on the scene item, so the inputs are
    // pulled once and looked up per scene item rather than re-requested.
    let inputs = client
        .request("GetInputList", serde_json::json!({}))
        .ok()
        .and_then(|d| d["inputs"].as_array().cloned())
        .unwrap_or_default();

    let scenes = match client.request("GetSceneList", serde_json::json!({})) {
        Ok(d) => d["scenes"].as_array().cloned().unwrap_or_default(),
        Err(e) => {
            println!("    GetSceneList failed: {}", e);
            return;
        }
    };

    for scene in &scenes {
        let name = scene["sceneName"].as_str().unwrap_or("?");
        println!("\n    scene: {}", name);

        let items = match client
            .request("GetSceneItemList", serde_json::json!({ "sceneName": name }))
        {
            Ok(d) => d["sceneItems"].as_array().cloned().unwrap_or_default(),
            Err(e) => {
                println!("      (could not list items: {})", e);
                continue;
            }
        };
        if items.is_empty() {
            println!("      (empty)");
            continue;
        }

        for item in &items {
            let source = item["sourceName"].as_str().unwrap_or("?");
            let kind = inputs
                .iter()
                .find(|i| i["inputName"].as_str() == Some(source))
                .and_then(|i| i["inputKind"].as_str())
                .unwrap_or("(not an input — a scene or group)");
            let note = describe_input_kind(kind);
            println!(
                "      {:<28} {:<32} {}",
                truncate(source, 28),
                kind,
                note
            );

            // For a capture source, which window it is pointed at is the whole
            // question — a Game Capture aimed at something else is exactly the
            // misconfiguration a picker should be able to report.
            if matches!(kind, "game_capture" | "window_capture")
                && let Ok(s) = client
                    .request("GetInputSettings", serde_json::json!({ "inputName": source }))
                {
                    let settings = &s["inputSettings"];
                    let window = settings["window"].as_str().unwrap_or("(default/any)");
                    println!("      {:<28} -> window: {}", "", window);
                    if window.to_lowercase().contains("hl.exe") {
                        println!("      {:<28} -> POINTED AT THE GAME", "");
                    }
                }
        }
    }

    println!(
        "\n    Everything above came from GetSceneList / GetSceneItemList /\n\
         \x20   GetInputList / GetInputSettings — reads only. This is what a scene\n\
         \x20   dropdown in dod-tools would be built from."
    );
}

/// Whether the game is up, checked the same way `capture_engine` checks it.
///
/// Note the game's lifecycle is not OBS's and not ours: dod-tools spawns
/// `hl.exe` per batch and taskkills it at the end, and a `tauri dev` hot reload
/// takes it down too. So "is it running right now" genuinely has to be asked
/// rather than assumed from having launched it earlier.
fn game_is_running() -> bool {
    use sysinfo::{ProcessExt, SystemExt};
    let sys = sysinfo::System::new_all();
    sys.processes()
        .values()
        .any(|p| p.name().eq_ignore_ascii_case("hl.exe"))
}

/// One-line rendering of a response object, so four probes fit on four lines.
fn compact(v: &serde_json::Value) -> String {
    match v.as_object() {
        Some(map) => map
            .iter()
            .map(|(k, val)| format!("{}={}", k, val))
            .collect::<Vec<_>>()
            .join("  "),
        None => v.to_string(),
    }
}

// ── A minimal obs-websocket v5 client ─────────────────────────────────────────

/// Blocking on purpose. See the dependency comment in `native/Cargo.toml`: the
/// real integration has to be able to send a `StopRecord` from
/// `CaptureCleanupGuard::drop`, which has no async runtime under it.
struct ObsClient {
    ws: tungstenite::WebSocket<std::net::TcpStream>,
    next_id: u64,
    /// Why the server hung up, when it did.
    ///
    /// obs-websocket does not answer a bad `Identify` — it closes the socket
    /// with a code, and 4009 specifically means the authentication string did
    /// not match. Without capturing that, a wrong password and a wrong
    /// handshake implementation produce the identical symptom of "never
    /// Identified", which is exactly the ambiguity this probe exists to avoid.
    last_close: Option<String>,
}

impl ObsClient {
    fn connect(url: &str, password: Option<&str>) -> Result<Self, String> {
        let stripped = url.trim_start_matches("ws://");
        let stream = std::net::TcpStream::connect(stripped)
            .map_err(|e| format!("tcp connect to {}: {}", stripped, e))?;
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .map_err(|e| e.to_string())?;
        let (ws, _) = tungstenite::client(
            url.parse::<tungstenite::http::Uri>().map_err(|e| e.to_string())?,
            stream,
        )
        .map_err(|e| format!("websocket handshake: {}", e))?;

        let mut client = Self { ws, next_id: 0, last_close: None };

        // op 0 Hello -> op 1 Identify -> op 2 Identified.
        let hello = client
            .read_op(0, Duration::from_secs(10))
            .ok_or("no Hello (op 0) from OBS")?;

        let mut identify = serde_json::json!({ "rpcVersion": 1 });
        let auth_required = hello["authentication"].is_object();
        if let Some(auth) = hello["authentication"].as_object() {
            let password = password.ok_or(
                "OBS requires authentication but no --password was given",
            )?;
            let challenge = auth["challenge"].as_str().unwrap_or_default();
            let salt = auth["salt"].as_str().unwrap_or_default();
            identify["authentication"] =
                serde_json::Value::String(auth_string(password, salt, challenge));
        }
        client.send(serde_json::json!({ "op": 1, "d": identify }))?;

        if client.read_op(2, Duration::from_secs(10)).is_none() {
            return Err(match client.last_close.as_deref() {
                // 4009 is obs-websocket's own "authentication failed" code, so
                // this is a definite answer rather than a guess.
                Some(reason) if reason.contains("4009") => format!(
                    "OBS rejected the password (close {}).\n\
                     The handshake itself is fine — the server got a well-formed Identify and \
                     disagreed with the hash, which only happens when the password differs.\n\
                     Use the Copy button beside Server Password in OBS rather than reading it \
                     off the screen; 1/l and 0/O are easy to mistake.",
                    reason
                ),
                Some(reason) => format!(
                    "OBS closed the connection before Identified: {}\n\
                     (auth was {} by the server)",
                    reason,
                    if auth_required { "required" } else { "not required" }
                ),
                None => format!(
                    "no Identified (op 2) and no close frame within 10s — auth was {} by the \
                     server. This is not a password failure; the server said nothing at all.",
                    if auth_required { "required" } else { "not required" }
                ),
            });
        }
        Ok(client)
    }

    fn send(&mut self, v: serde_json::Value) -> Result<(), String> {
        self.ws
            .send(tungstenite::Message::Text(v.to_string().into()))
            .map_err(|e| format!("send: {}", e))
    }

    /// Reads until a message with the given `op` arrives, or the deadline
    /// passes. Anything else is discarded — this probe has no use for events it
    /// did not ask for, and buffering them would only hide ordering.
    fn read_op(&mut self, op: u64, timeout: Duration) -> Option<serde_json::Value> {
        self.read_until(timeout, |m| (m["op"].as_u64() == Some(op)).then(|| m["d"].clone()))
    }

    /// The generic read loop. `f` decides whether a message is the one being
    /// waited for; a read timeout is not an error, it is just "nothing yet",
    /// which is what the 200ms socket timeout above is for.
    fn read_until<T>(
        &mut self,
        timeout: Duration,
        mut f: impl FnMut(&serde_json::Value) -> Option<T>,
    ) -> Option<T> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let msg = match self.ws.read() {
                Ok(m) => m,
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                Err(_) => return None,
            };
            let text = match msg {
                tungstenite::Message::Text(t) => t.to_string(),
                tungstenite::Message::Close(frame) => {
                    self.last_close = Some(match frame {
                        Some(f) => format!("code {} — {}", u16::from(f.code), f.reason),
                        None => "no close frame detail".to_string(),
                    });
                    return None;
                }
                _ => continue,
            };
            let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            if let Some(found) = f(&parsed) {
                return Some(found);
            }
        }
        None
    }

    fn request(
        &mut self,
        request_type: &str,
        data: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.next_id += 1;
        let id = format!("probe-{}", self.next_id);
        self.send(serde_json::json!({
            "op": 6,
            "d": { "requestType": request_type, "requestId": id, "requestData": data }
        }))?;

        let id_for_match = id.clone();
        let response = self
            .read_until(Duration::from_secs(15), |m| {
                if m["op"].as_u64() != Some(7) {
                    return None;
                }
                if m["d"]["requestId"].as_str() != Some(id_for_match.as_str()) {
                    return None;
                }
                Some(m["d"].clone())
            })
            .ok_or_else(|| format!("{}: no response within 15s", request_type))?;

        let status = &response["requestStatus"];
        if status["result"].as_bool() == Some(true) {
            Ok(response["responseData"].clone())
        } else {
            Err(format!(
                "{} (code {})",
                status["comment"].as_str().unwrap_or("request failed"),
                status["code"]
            ))
        }
    }

    /// Waits for `RecordStateChanged` with the given `outputState`.
    ///
    /// This, not the request's own return, is when recording actually starts —
    /// which is the whole point of measuring it.
    fn wait_for_record_state(
        &mut self,
        state: &str,
        timeout: Duration,
    ) -> Option<serde_json::Value> {
        let want = state.to_string();
        self.read_until(timeout, |m| {
            if m["op"].as_u64() != Some(5) {
                return None;
            }
            if m["d"]["eventType"].as_str() != Some("RecordStateChanged") {
                return None;
            }
            let d = &m["d"]["eventData"];
            (d["outputState"].as_str() == Some(want.as_str())).then(|| d.clone())
        })
    }
}

/// obs-websocket v5 authentication.
///
///     secret = base64(sha256(password + salt))
///     auth   = base64(sha256(secret + challenge))
fn auth_string(password: &str, salt: &str, challenge: &str) -> String {
    use base64::Engine as _;
    use sha2::{Digest, Sha256};

    let b64 = base64::engine::general_purpose::STANDARD;
    let secret = b64.encode(Sha256::digest(format!("{}{}", password, salt).as_bytes()));
    b64.encode(Sha256::digest(format!("{}{}", secret, challenge).as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_is_parsed_out_of_a_stage_echo() {
        assert_eq!(
            parse_tick("[dod-tools] START_RECORD - Tick 41450"),
            Some(41450)
        );
        assert_eq!(parse_tick("[dod-tools] BATCH_COMPLETE"), None);
    }

    /// The echoes are chunked by `build_safe_echos` when they exceed the Cbuf
    /// limit, and continuation chunks carry a different prefix. The tick still
    /// has to come out of the first chunk.
    #[test]
    fn tick_is_parsed_from_a_chunked_echo() {
        assert_eq!(
            parse_tick("[dod-tools] CUSTOM_CMD1_BEFORE - Tick 900"),
            Some(900)
        );
    }

    /// Pins the v5 handshake against an implementation that is not this one.
    ///
    /// This is here because a wrong handshake does not look like a bug: OBS
    /// simply never sends `Identified`, and the failure reads as "wrong
    /// password" no matter what is actually wrong. So the hashing wants
    /// pinning by something external.
    ///
    /// **Provenance, because it matters here.** The four algorithm steps are
    /// quoted from obs-websocket's `docs/generated/protocol.md`:
    ///
    /// > Concatenate the websocket password with the `salt` provided by the
    /// > server (`password + salt`). Generate an SHA256 binary hash of the
    /// > result and base64 encode it, known as a base64 secret. Concatenate the
    /// > base64 secret with the `challenge` sent by the server
    /// > (`base64_secret + challenge`). Generate a binary SHA256 hash of that
    /// > result and base64 encode it. You now have your `authentication`
    /// > string.
    ///
    /// The *vector* below is not copied from that document — it is this input
    /// run through OpenSSL, which agrees with `auth_string` to the character:
    ///
    /// ```text
    /// SECRET=$(printf '%s' "supersecretpassword${SALT}" \
    ///   | openssl dgst -sha256 -binary | openssl base64 -A)
    /// printf '%s' "${SECRET}${CHAL}" \
    ///   | openssl dgst -sha256 -binary | openssl base64 -A
    /// ```
    ///
    /// Said plainly so nobody later "fixes" this against a published example
    /// and gets a different number: two independent implementations of the
    /// quoted steps agree, which is what is being asserted.
    #[test]
    fn auth_string_matches_an_independent_implementation() {
        assert_eq!(
            auth_string(
                "supersecretpassword",
                "lM1GncleQOaCu9lT1yeUZhFYnqhsLLP1G5lAGo3ixaI=",
                "+IxH4CnCiqpX1rM9scsNynZzbOe4KhDeYcTNS3PDaeY="
            ),
            "1Ct943GAT+6YQUUX47Ia/ncufilbe6+oD6lY+5kaCu4="
        );
    }

    /// The intermediate `secret` is base64 of the *binary* digest, not of its
    /// hex rendering — the single easiest way to get this wrong, and it fails
    /// silently as an auth rejection.
    #[test]
    fn secret_is_base64_of_the_binary_digest() {
        use base64::Engine as _;
        use sha2::{Digest, Sha256};
        let secret = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(
            b"supersecretpasswordlM1GncleQOaCu9lT1yeUZhFYnqhsLLP1G5lAGo3ixaI=",
        ));
        assert_eq!(secret, "H1IfVz1pSREUQzbFTVnX/Tyb+gMhMik5x7yUBCY0PTs=");
    }

    #[test]
    fn truncate_keeps_short_labels_intact() {
        assert_eq!(truncate("START_RECORD", 44), "START_RECORD");
        assert_eq!(truncate(&"x".repeat(50), 10).chars().count(), 10);
    }
}
