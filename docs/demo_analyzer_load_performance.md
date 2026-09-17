# Demo Analyzer Load Performance — Audit & Implementation Plan

Status: **Tiers 1a/1b/2/3 implemented and committed** (`00be056` perf commit,
`ee86ea1` follow-up `run.ps1` fix — unrelated, just adjacent history on the
same branch). **Tier 4/5 not started**, tracked as
[GitHub issue #37](https://github.com/ccoventry/dod-tools/issues/37) — still
blocked on the future-stats review exactly as originally planned, see that
section, it's unchanged.
Written so a fresh chat (clean context) can pick this up without re-deriving
anything. If you're that fresh session: read this whole file before touching
code, it front-loads everything the audit already ruled out. The "Implementation
plan" section below is now a record of what was built, not just a proposal —
each tier is marked with its outcome and any deviation from the original design.

## Goal

Opening a demo in the Demo Analyzer (`desktop-studio/src/analyzer_pane.js` ->
`analyze_demo_full`) takes a few seconds. User wants it faster — ideally
instant, or at least <= 1s.

## Relationship to the (separate, not-yet-started) "more player stats" work

User also wants to review what stats could be added beyond the current
kills/deaths/kills-by-weapon set, including data the parser already sees but
`analysis/` never surfaces. **These two efforts are not independent — read
the Tier 4/5 section before doing either.**

Short version: Tiers 1a/1b/2/3 below (caching, progress events, dead-code
removal, reusing Capture Studio's scan) have no conflict with adding stats —
Tier 1a just needs a cache schema version, noted inline. Tier 4/5 (skip
decoding message types the analyzer doesn't read, to save the last big chunk
of parse time) directly targets `SvcClientData` and `SvcDeltaPacketEntities`
— which, per-tick position/velocity/angles/health/ammo for every player, are
almost certainly the richest source of *new* stats and are currently 100%
unread by `analysis/`. Do the stats-data review before Tier 4, not after, or
risk building an optimization you immediately have to partially undo.

## Already shipped (unrelated but adjacent — don't re-do)

Commit `f6b382c` on `feature/tauri-migration`:
- Folder/demo picker sidebar in the Demo Analyzer tab (folder tree + demo list
  next to the report, matching the `dev` branch egui GUI's persistent
  explorer). Files: `desktop-studio/index.html`, `desktop-studio/src/analyzer_pane.js`,
  `desktop-studio/src-tauri/src/dir_browser.rs`.
- `benchmark/src/main.rs` rewritten. It used to compare an "unoptimized vs
  optimized" event loop that doesn't reflect the real load path (see below) —
  it's now a phase-attribution profiler for the actual path
  `analyze_demo_full` runs. **Use this to reproduce every number below:**

  ```
  cargo build --release -p dod-benchmark
  ./target/release/dod-benchmark.exe ./local/demos          # or any folder of .dem files
  ```

## Measured baseline (release build, 4 demos, 52-89 MB each)

| Phase | Avg | Share | What it is |
|---|---|---|---|
| read | 15 ms | 1.1% | `fs::read` the whole file into `Vec<u8>` |
| **decode** | **824 ms** | **62.4%** | `Demo::parse_from_bytes(Parse)` — structural walk + full netmessage decode |
| **drop** | **441 ms** | **33.4%** | freeing the decoded frame tree |
| events | 38 ms | 2.9% | the `AnalyzerState` event loop (13 handlers/event) |
| serialize | 2 ms | 0.2% | `serde_json` of the IPC payload |
| **total** | **~1.3 s** | | |

Structural walk alone (`MessageDataParseMode::None`, no netmessage decoding)
is ~180 ms — so **~650 ms of the 824 ms decode is netmessage parsing
specifically**, not directory/frame bookkeeping.

Netmessage stream composition (summed across the 4 demos, 1.8M messages):

- **74.6% of all messages are decoded then discarded** — nothing in
  `analysis/src/lib.rs` reads them.
- Three message types are 68.5% of all volume, **100% discarded**:
  `SvcClientData` (24.0%), `SvcDeltaPacketEntities` (24.0%), `SvcSound` (20.5%).
- The analyzer's full consumed set: EngineMessages `SvcDirector`, `SvcHltv`,
  `SvcServerInfo`, `SvcStuffText`, `SvcTime`, `SvcUpdateUserInfo`; UserMessages
  matching `is_relevant_message()` in `analysis/src/lib.rs` (~line 232):
  `RoundState, ClanTimer, TimeLeft, WaveTime, TeamScore, ScoreShort, ObjScore,
  Frags, PClass, PTeam, ScoreInfo, ScoreInfoLong, SayText, TextMsg, DeathMsg,
  PStatus, Scope, CurWeapon, ReloadDone, ResetHUD, Health`.

Debug vs release: only ~1.3-1.7x, not the 10x+ you'd expect from an
unoptimized build. `dem-patch` is a workspace **dependency**, so
`[profile.dev.package."*"]` (root `Cargo.toml`) already builds it optimized
even under `tauri dev`. Not a real lever — don't spend time here.

## What this rules out

- **The analyzer's own event-loop logic is not the bottleneck** (2.9%). The
  existing `parse_with_diagnostics` / `ParseDiagnostics` machinery in
  `analysis/src/lib.rs` (~lines 802-1240, `pub fn parse_with_diagnostics`,
  `struct ParseDiagnostics`, `fn check_states_equal`) only ever measured this
  event loop in isolation (an "unoptimized vs optimized" comparison, ~1.1-1.3x
  speedup) — which is why it looked like nothing changed no matter what you
  did to it. **It is now dead code** — nothing calls it after the benchmark
  rewrite (verified: `grep -rn "parse_with_diagnostics\|ParseDiagnostics" --include=*.rs .`
  finds only its own definition; the one test that does a similar unopt/opt
  comparison, `test_optimized_vs_unoptimized` at ~line 1478, has its own
  separate inline `assert_states_eq` helper and doesn't call it). Safe to
  delete as part of the next pass — see Tier 2 below.
- **IPC / frontend render is not the bottleneck.** Payload is ~0.3 MB;
  serialize + Tauri IPC + `JSON.parse` is single-digit ms.
- **File read is not the bottleneck** (~1%). Don't memory-map the file — it
  wouldn't help and adds complexity.

## What actually costs the ~1.3s: decode + drop (95.8% combined)

This is the real finding. `Demo::parse_from_bytes(..., MessageDataParseMode::Parse)`
fully decodes every netmessage into typed structs — including the 74.6% of
messages the analyzer never reads — and then that entire tree gets dropped a
few hundred ms later, which is itself expensive. The drop cost being almost as
large as the decode cost (441ms vs 824ms) points at allocation-heavy
representations rather than the walk itself — most likely
`Delta = HashMap<String, Vec<u8>>` (per-entity-update delta table) in
`dem-patch/src/types.rs`, given every `SvcDeltaPacketEntities` message
(24% of all messages) builds one. **Not independently profiled with a memory
tool this session — the 441ms drop-cost measurement is real, but "which
allocation is responsible" is inference from reading the types, not a
heap profile.** If you want to confirm before doing the bigger delta-rewrite
work (Tier 4), that's the first thing to instrument.

## Verified separately (source-read, not benchmarked)

- **`native/src/patch/scanner.rs::scan_demo_for_highlights`** (called by
  `desktop-studio/src-tauri/src/capture_manager.rs::scan_directory_impl`,
  i.e. every Capture Studio folder scan) already calls
  `analysis::Analysis::try_from_bytes(&bytes)` — **the exact same full parse**
  `analyze_demo_full` does — for every demo it scans, then discards
  everything except `(tickrate, streaks, is_pov, local_player_index,
  playback_frames, match_start_tick, frame_times)`. Confirmed by reading the
  function body directly. This is real, reusable work being thrown away — see
  Tier 3.
- **`Analysis` and `FileInfo` already derive `Serialize`/`Deserialize`**
  (`analysis/src/lib.rs` ~line 225, `native/src/lib.rs` ~line 29) — a JSON
  cache needs zero new derive work.
- **Incidental bug, unrelated to load speed, found while tracing callers —
  resolved 2026-08-22, by removal rather than a fix.** `analyze_demo` (the
  *other* Tauri command, `desktop-studio/src-tauri/src/lib.rs` ~line
  207-255 — different from `analyze_demo_full`) fed the "Advanced
  Diagnostics / Match Telemetry" inline panel (`#telemetry-container`,
  distinct from the Demo Analyzer tab). It did the full ~1.3s parse, then
  looked up `analysis_json.get("scoreboard")`,
  `.get("chat_logs").or(.get("chat"))`,
  `.get("mortality_metrics").or(.get("deaths"))`,
  `.get("round_chronologies").or(.get("rounds"))` on the serialized
  `Analysis`. But `Analysis` serializes as `{"demo_info": {...}, "state":
  {...}, "events": [...]}` — **none of those top-level keys exist**, so all
  four always resolved to `Null`; the panel paid the full parse cost and
  rendered nothing, every time. A user report of the always-blank panel led
  straight back to this exact diagnosis. Fixing it properly meant real
  backend rework (returning `state`'s actual nested fields under the right
  names) for a feature that mostly duplicated the "View Match Telemetry"
  button, which already jumps straight to the full Demo Analyzer report —
  so it was removed instead: `analyze_demo`, `SerializedAnalysis`,
  `telemetry_pane.js`, and the `analyzeDemo()` IPC wrapper are all gone.
  The button and its jump to Demo Analyzer are untouched.
- Confirmed directly in this session's own build output:
  `[profile.release]` in `desktop-studio/src-tauri/Cargo.toml` is silently
  ignored by Cargo ("profiles for the non root package will be ignored,
  specify profiles at the workspace root") — profiles only apply from the
  workspace-root `Cargo.toml`. Zero runtime effect today; harmless but
  misleading config.
- `panic = "abort"` in the workspace root `Cargo.toml`'s `[profile.release]`
  means the `std::panic::catch_unwind` in
  `Analysis::try_from_bytes_with_progress` (`analysis/src/lib.rs` ~line 615)
  cannot actually catch anything in release builds. Correctness/crash-handling
  gap, not a speed issue — flagging so it doesn't get mistaken for "handled."

## Implementation plan

### Tier 1a — on-disk analyzer cache (the only path to "instant") ✅ implemented

Highest leverage, low risk, purely additive. Design:

- New function in `native/src/lib.rs`, e.g.
  `run_analyzer_cached(demo_path: &PathBuf, progress_cb) -> Result<(FileInfo, Analysis, bool /* from_cache */), String>`.
- Cache dir: `native::shared::paths::get_appdata_dir().join("analyzer_cache")`
  (mirrors the existing appdata pattern in `native/src/shared/paths.rs`).
- One JSON file per demo, named by a hash (std `DefaultHasher`/FNV-1a — no new
  dependency needed) of the canonicalized absolute demo path. Contents:
  `{ size_bytes, modified_unix_secs, file_info: FileInfo, analysis: Analysis }`.
- On call: `fs::metadata` the demo (size + mtime — cheap, already done today
  in `run_analyzer_with_progress`), compute the cache key, try read + parse
  the cache file. If size/mtime match -> return immediately (no progress
  callback needed, this is the ~10-15ms warm path). If missing/stale/corrupt
  -> run today's `run_analyzer_with_progress` in full, then best-effort
  write-through to the cache file (ignore write errors — must never fail the
  analyze call because the cache write failed).
- Wire `desktop-studio/src-tauri/src/lib.rs::analyze_demo_full` (~line 275) to
  call this instead of `run_analyzer_with_progress` directly.

**As built:** matches the design above almost exactly. `native/src/lib.rs`
gained `run_analyzer_cached` (cache read/write, size+mtime validity check),
`build_file_info` (factored out of `run_analyzer_with_progress` so both paths
share it), and `write_analyzer_cache_entry` (shared write-through helper, also
used by Tier 3 below). Cache key is `fnv1a_hash` of the canonicalized path
(reusing `native::utils::demo_hasher::fnv1a_hash`, already used by
`hl-demo-auditor` — no new hashing dependency needed, as hoped). Cache dir is
`analyzer_cache/v{CACHE_SCHEMA_VERSION}/<hash>.json`, `CACHE_SCHEMA_VERSION`
starts at `1`. `analyze_demo_full` now calls `run_analyzer_cached` instead of
`run_analyzer_with_progress`.

Expected result: first open of a demo unchanged (~1.3s); every subsequent
open of the same unmodified file drops to ~10-15ms (cache-file read + JSON
deserialize of a ~0.3MB payload). This is what "instant" actually means here
— cold-load has a real floor around 1-1.3s without Tier 4/5 work below.

**Schema versioning — required, not optional.** The cache stores a
deserialized `Analysis`. The moment a future change adds a new stat (a new
field on `Player`, a new pass over the event stream, anything that changes
*what gets computed*, not just how fast), an old cache entry will happily
deserialize with the new field missing/defaulted — silently returning
incomplete data forever instead of a cache miss. Bake a
`const CACHE_SCHEMA_VERSION: u32` into the cache file (or the cache
subdirectory name, e.g. `analyzer_cache/v1/<hash>.json`) and bump it any time
`AnalyzerState`/`Player`/related structs change in a way that affects
computed content. Treat a version mismatch as a miss. This is what makes
Tier 1a compatible with an evolving stats set — see below, this is not
hypothetical, it's coming soon.

### Tier 1b — real progress events (fixes "feels frozen", not speed) ✅ implemented

- Add `app_handle: tauri::AppHandle` param to the `analyze_demo_full` command
  (same pattern already used by `scan_directory` in the same file).
- Replace the no-op `|_, _| {}` progress closure with one that emits a Tauri
  event, mirroring `capture_manager.rs`'s existing `scan_progress` emit
  pattern (`app_handle.emit("analyzer_progress", json!({...}))`).
- **Throttle it.** `try_from_bytes_with_progress` already calls the callback
  every ~500 frames (`analysis/src/lib.rs` ~line 781:
  `processed_frames % 500 == 0`), which is ~1000+ calls for a 540k-frame demo.
  Emitting a Tauri IPC event on every one of those would add real overhead
  back onto the path we're trying to shrink. Wrap the closure with a
  `last_emit: Instant` and skip emitting unless >= 33ms has elapsed (per
  CLAUDE.md's telemetry-throttling guardrail — ~30fps). That bounds real
  emits to ~25-40 per parse.
- Frontend: `desktop-studio/src/analyzer_pane.js::loadAnalyzerDemo` needs a
  `listen('analyzer_progress', ...)` (register directly in the pane module,
  per the existing note in `ipc_bridge.js` about not double-registering
  listeners — same pattern `render_pane.js` uses for `render_status`) to swap
  the static "Analyzing…" text for a real percentage/progress bar.

**As built:** matches the design. `analyze_demo_full` now takes `app_handle:
tauri::AppHandle`, throttles via a `last_emit: Instant` + 33ms check (always
emitting on the final `processed == total` call too, so the UI never gets
stuck below 100%), and emits `analyzer_progress` with `{processed, total}`.
`analyzer_pane.js` registers a single `listen('analyzer_progress', ...)` at
module scope (not inside a function, so it only ever runs once per app
lifetime — same double-registration concern noted for `render_status` in
`ipc_bridge.js`), gated by an `analyzerLoadInProgress` flag so stray/late
events from a previous load can't clobber the UI. One thing not in the
original design: since a cache *hit* (Tier 1a) never calls `progress_cb` at
all, the progress UI only ever appears on a cold parse — correct behavior,
just worth knowing if it looks like progress events "stopped working" once
the cache warms up.

### Tier 2 — delete dead code ✅ implemented

Remove `analysis/src/lib.rs` ~lines 802-1240: `parse_with_diagnostics`,
`struct ParseDiagnostics`, `fn check_states_equal`. Confirmed unused (see
above). Pure cleanup, ~440 lines gone, no behavior change.

**As built:** exactly as planned, plus a pass to keep the boundary clean
(the block sat inside `impl Analysis { ... }` alongside `try_from_bytes*`, so
only the `parse_with_diagnostics` fn body was deleted from inside that impl;
`ParseDiagnostics`/`check_states_equal` were free-standing and deleted
outright). `cargo build --workspace` and a `grep -rn` for all three names
confirmed zero remaining references before landing it.

### Tier 3 — reuse Capture Studio's scan for cache warm-up ✅ implemented (design changed)

`scan_demo_for_highlights` (`native/src/patch/scanner.rs`) already builds the
exact `Analysis` the cache in Tier 1a wants, then throws it away. Threading it
through would mean every Capture Studio folder scan pre-warms the analyzer
cache for free — for the common "scan a folder, then inspect demos"
workflow, every analyzer open after a scan becomes the ~10-15ms cache path.

**As built — deliberately not what this section originally proposed.** The
plan above called for changing `scan_demo_for_highlights`'s return signature
directly, flagged at the time as the reason to defer it (bigger blast radius,
touches a `pub fn` with call sites beyond `capture_manager.rs`). When Tier 3
actually got built, that turned out to be avoidable: `scan_demo_for_highlights`
now has a new sibling, `scan_demo_for_highlights_with_analysis`, which does the
real work and returns `(the_original_7_tuple, analysis::Analysis)`;
`scan_demo_for_highlights` itself became a one-line wrapper
(`.map(|(result, _analysis)| result)`) with its signature and behavior
completely unchanged. Only `capture_manager.rs::scan_directory_impl` (the one
caller that actually wanted the `Analysis`) was switched to call the new
function. The other four call sites (`native/src/bin/check_ticks.rs`,
`debug_scanner.rs`, `cli/main.rs`, `test_builder.rs`) were never touched —
same effect as the original design (folder scans warm the cache), much
smaller diff. `scan_directory_impl` calls the new
`native::warm_analyzer_cache(&file, &analysis)` (in `native/src/lib.rs`,
built alongside `run_analyzer_cached` in Tier 1a, sharing its
`write_analyzer_cache_entry` helper) right after each successful scan —
best-effort, never fails the scan itself.

### Tier 4/5 — BLOCKED on the future-stats question below, not just "later"

Tracked as [issue #37](https://github.com/ccoventry/dod-tools/issues/37). Kept in full below since it's the technical detail an implementer of that issue will actually need (which message types, which fields, why the sequencing with the stats work matters).

- **Selective netmessage parsing** (skip decoding message bodies for the
  74.6% the analyzer never reads, e.g. don't decode `SvcSound`/`SvcClientData`
  bodies past their length prefix). Real potential — could cut a large chunk
  of the 824ms decode — but genuinely risky: some of those "discarded"
  messages maintain delta/baseline state (`SvcDeltaPacketEntities` especially)
  that other decoders may depend on downstream in the same stream. Silently
  wrong output (bad scoreboard/kill numbers) is a worse outcome than "slow."
- **Rewriting the `Delta` representation in `dem-patch`** (the
  `HashMap<String, Vec<u8>>` per entity update, suspected of driving both the
  824ms decode and the 441ms drop). Bigger, cross-cutting change to a shared
  parsing crate other tools depend on.

Both need a `check_states_equal`-style correctness harness (a real "compare
before/after parsed state across a broad demo corpus" check) rebuilt and run
wide before shipping. That code was deleted in Tier 2 (now done, see above) —
its pattern is still in history at commit `00be056`'s parent (`fa0d9d4`) if
you want to resurrect it as a starting point rather than writing one from
scratch.

**This tier is not just deferred, it is actively in tension with adding more
player stats — read this before starting either.** The two biggest
"discarded" message types are also the richest untapped data source in the
whole demo:

- **`SvcClientData`** (`dem-patch/src/netmsg_doer/client_data.rs`) carries the
  POV player's per-tick `clientdata_t` delta — origin, view angles, velocity,
  health, FOV, punch angle — plus a `weapon_data_t` delta per held weapon
  (ammo/clip state). Verified by reading the decoder directly.
- **`SvcDeltaPacketEntities`** (`dem-patch/src/netmsg_doer/delta_packet_entities.rs`)
  carries an `entity_state_player_t` delta for **every player entity on the
  server** (`entity_index <= aux.max_client`), every network update — i.e.
  full positional/state data for the whole match, not just the recording
  player. Verified by reading the decoder directly.
- **Confirmed via `grep -rn "SvcDeltaPacketEntities\|SvcClientData\|entity_state\|client_data" analysis/src/*.rs`: zero hits.**
  The analyzer has never touched either stream. This is fully greenfield —
  distance traveled, movement heatmaps, average engagement distance,
  time-to-kill, ammo economy, health-over-time, positioning at death, all of
  it lives in these two message types and nothing reads them today.

Both fields decode via server-sent field definitions
(`SvcDeltaDescription` -> `aux.delta_decoders`), so the *set* of fields
available isn't fixed at compile time — it's whatever `clientdata_t` /
`entity_state_player_t` / `weapon_data_t` the mod defines. Worth dumping one
decoded delta's keys before designing anything, to see the real field list
rather than guessing from the GoldSrc SDK headers.

**Sequencing, restated plainly**: do the future-stats review first, then design the selective-parse mode around its answer (keep decoding whatever fields the stats work wants, skip only what's still unwanted regardless — e.g. `SvcSound`, `ClientAreas`, `SvcTempEntity`). Shipping Tier 4 first risks partially reverting it once a wanted stat turns out to need `SvcClientData`/`SvcDeltaPacketEntities`. That review hasn't happened yet.

## How to verify any of this yourself

```
cargo build --release -p dod-benchmark
./target/release/dod-benchmark.exe ./local/demos
```

Reproduces the phase table and the consumed/discarded netmessage histogram
above — still accurate for a cold/cache-miss parse, since the benchmark
exercises the parse path directly and doesn't go through the Tier 1a cache.

**Tier 1a cache, now that it exists:** open a demo in the Demo Analyzer tab,
check `%APPDATA%\dod-studio\analyzer_cache\v1\` populates with a
`<16-hex-digit>.json` file, then re-open the same demo (or re-select it from
the sidebar) and confirm it's near-instant with no progress bar — a cache hit
skips `progress_cb` entirely, so the absence of the Tier 1b progress UI on a
second open is itself the tell. Bump `ANALYZER_CACHE_SCHEMA_VERSION` in
`native/src/lib.rs` (currently `1`) any time computed `AnalyzerState`/`Player`
fields change, so old cache entries get treated as a miss instead of quietly
deserializing incomplete.

**Tier 3 warm-up:** run a Capture Studio folder scan over a directory of
demos you haven't opened in the Analyzer yet, then check
`analyzer_cache/v1/` already has entries for them before you ever open the
Analyzer tab — confirms `scan_directory_impl` is calling
`native::warm_analyzer_cache` per scanned demo.
