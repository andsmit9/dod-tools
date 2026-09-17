# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) and offline IDE agents when working in this repository.

## Project Overview

`dod-studio` is a high-performance pipeline for capturing, patching, and analyzing **Day of Defeat 1.3** (GoldSrc engine) demo files (`.dem`). It drives **HLAE** (Half-Life Advanced Effects) and `hl.exe` headlessly to batch-record highlight clips out of recorded matches, transcodes the results via FFmpeg, and parses demos for match analytics (scoreboards, kills, chat, rounds). The active desktop application is a Tauri + Vite frontend (`desktop-studio/`).

---

## Workspace Layout & Module Boundaries

Cargo workspace (`Cargo.toml`, resolver "3", edition 2024) containing the following members:

- **`dod/`** — Low-level `nom`-based binary parsers/types for DoD 1.3 demo message structures. Pure parsing primitives (no I/O); consumed by `analysis` and `native`.
- **`analysis/`** — Turns parsed demo data into match analytics (`scoreboard.rs`, `kill.rs`, `chat.rs`, `round.rs`, `mortality.rs`, `player.rs`, `clan_match.rs`, `localization.rs`). Entry point: `Analysis::try_from_bytes_with_progress`.
- **`dem-patch/`** — Vendored fork of the `dem` crate (`[patch.crates-io]`). Low-level demo bit/byte reading/writing (`bit.rs`, `byte_writer.rs`, `demo_parser.rs`, `demo_writer.rs`, `delta.rs`).
- **`native/`** — Core engine crate containing almost all non-UI logic:
  - `capture_engine.rs` — Orchestrates capture batches: spawns HLAE/`hl.exe`, drives playback, injects console commands at scheduled ticks.
  - `patch/` — Demo-file binary patcher (`engine.rs`, `builder.rs`, `scanner.rs`, `highlevel.rs`, `types.rs`). Scans, injects (bookmarks, director commands, `DRC_CMD_INEYE`), and rewrites GoldSrc frames.
  - `hlcr/` — Take management & FFmpeg transcoding (`renderer.rs`, `scanner.rs`, `config.rs`, `autosave.rs`).
  - `shared/`, `sys/`, `utils/` — Path resolution, disk-space queries, demo hashing.
  - `src/bin/cli/main.rs` → `preview_cli` binary: Headless entry point with drag-and-drop support.
- **`hl-demo-auditor/`** — Standalone duplicate-demo detector using size + header hash (`fnv1a_hash`).
- **`benchmark/`** — Performance benchmarking binary for the parsing/patching pipeline.
- **`desktop-studio/`** — Active Tauri v2 + Vite/JS frontend workspace (`src-tauri/` backend and `src/*.js` frontend modules).
- **`web-analyzer/`** — `analysis` compiled to `wasm32-unknown-unknown`, deployed to GitHub Pages on every push to `main` (`.github/workflows/deploy_web.yml`). Static frontend lives in `www/`.

> The experimental `xash-transcode/` crate (GoldSrc HLDEMO → Xash3D IDEM transcoder for the browser preview viewer) lives on its own `experimental/xash-transcode` branch, not on `main`/`dev`/`feature/tauri-migration`. See that branch's `docs/web_preview_viewer.md` before touching it.

---

## Common Development Commands

    # Build workspace binaries
    cargo build --workspace
    cargo build --release --workspace

    # Execute test suites
    cargo test --workspace
    cargo test -p analysis            # Single crate
    cargo test -p native patch::      # Single module

    # Run headless preview CLI
    cargo run -p native --bin preview_cli -- <path-to-demo-or-folder>

    # Desktop App Dev Loop (Run from desktop-studio/)
    cd desktop-studio
    npm install
    npm run tauri dev     # Launch Tauri window with Vite HMR
    npm run dev           # Vite dev server only
    npm run build         # Production Vite build

---

## System Guardrails & Agent Directives

### Context & Execution Boundaries
- **Context Scope:** Rely strictly on active chat code and files inside `docs/`. Strictly ignore any open files in hidden dot-directories to prevent prompt contamination. Do not index `target/`, `local/` (demos and screenshots), or `Cargo.lock`.
- **Locked Files:** Do not modify build/deployment configs, environment files, lint rules, or public APIs unless explicitly requested.
- **Behavior:** Be concise. Suppress conversational filler, apologies, and requirement summaries.
- **Code Edits:** Apply minimal changes directly to files. Never rewrite unchanged lines or entire files unnecessarily.
- **Ambiguity:** State critical technical assumptions once and proceed. Fail loudly on blocking errors.

### Terminal & Shell Rules
- **Diagnostics:** Never output raw compiler logs. Provide concise, single-sentence failure summaries and direct mechanical fixes.
- **Execution Bypass:** Use `Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass` for blocked scripts. For unsigned binaries blocked by WDAC, execute via `run.ps1` to sequence process stops, build steps, and signature updates.

### GitHub Issues & PRs
- **After creating a PR**, check whether a GitHub issue already exists for the same work. If one does, link it to the PR via GraphQL, not just a `Closes #NN` line in the PR body — that text alone does not populate the issue's `closedByPullRequestsReferences`, so a "yes there's a PR" check on the issue can miss it:
      PR_ID=$(gh api repos/<owner>/<repo>/pulls/<pr-number> --jq .node_id)
      ISSUE_ID=$(gh api repos/<owner>/<repo>/issues/<issue-number> --jq .node_id)
      gh api graphql -f query='
        mutation($prId: ID!, $issueId: ID!) {
          addCloseIssueReferences(input: {issueId: $issueId, pullRequestIds: [$prId]}) {
            clientMutationId
          }
        }' -f prId="$PR_ID" -f issueId="$ISSUE_ID"
  Still include `Closes #NN` in the PR body too — the GraphQL call is in addition to that, not a replacement for it.
- **Do not create an issue after every PR as a matter of habit.** A PR that fixes something noticed and resolved in the same pass needs no separate paper trail — the PR description already is that record, and an issue closed minutes later by the very PR that created it is noise. Only file one for work you are deliberately *not* doing right now: something noticed but out of scope for the current PR, or a fix knowingly deferred rather than made. That is the actual signal — deferral, not the mere absence of a pre-existing issue.

---

## Concurrency, Rust & Memory Constraints

- **WASM Protection:** The codebase carries legacy `wasm32-unknown-unknown` compilation gates. Isolate native multi-threading, direct file I/O (`std::fs`), and process spawning (`std::process::Command`) behind `#[cfg(not(target_arch = "wasm32"))]`.
- **Hot-Path Locking:** Never introduce blocking mutexes on the UI frame loop. Wrap shared catalogs in `std::sync::RwLock` and use atomics/channels for cross-thread signaling.
- **Telemetry Throttling:** Background progress channels must throttle update traffic to ~30fps (~33ms) using an `Arc<AtomicU32>` debouncer to prevent event loop flooding.
- **Memory Safety:** Never use fixed-size stack buffers (`[u8; N]`) for binary stream slicing. Use heap-allocated `Vec<u8>` gated by explicit 2MB payload limits.
- **Process Lifecycles:** External processes (HLAE, `hl.exe`, FFmpeg) must use non-blocking polling (`child.try_wait()`) matched with a ~16ms sleep. Verify an `Arc<AtomicBool>` cancellation token every cycle and chain `.kill_on_drop(true)`.

---

## Domain & Engine Quirks (GoldSrc & HLAE)

- **Terminology:** Strictly enforce the naming convention **"HLAE Game Capture"** (never "Native Game Capture").
- **Frame Order:** `DemoStart` (Type 2) frames must be processed *before* any `ConsoleCommand` (Type 3) frames are written, or the GoldSrc engine reads uninitialized memory.
- **Cbuf Payload Limit:** Command strings injected per tick must stay strictly under GoldSrc's 64-byte `Cbuf_AddTextToBuffer` limit. Stagger long absolute paths across multiple ticks.
- **Packet Integrity:** Never interleave injected frames inside existing `NetworkMessage` payloads. Injected bookmarks/director frames must be written as complete, standalone frames ahead of the original packet to prevent `svc_bad` buffer overflows.
- **Path Escaping:** All runtime paths passed to HLAE console inputs must replace forward slashes with double-escaped backslashes (`.replace("/", "\\\\")`).
- **Decal Ring:** `r_decals` bounds the rotating decal index and evicts nothing, so lowering it strands every decal above the new limit. Set it exactly once, at demo load, from `init_commands` — never mid-demo, never as an injected `ConsoleCommand` frame (that shifts every later frame ordinal by +1). See `docs/goldsrc_dod_quirks.md`.
- **Command Tiers:** A command a user types into Initial or Scheduled Commands falls into one of five lists in `native::patch::cfg_scan`. Enforcement runs twice, independently: in `map_manager::scan_game_configs`'s report, and again in `capture_manager::start_capture_batch_impl`.
  - `BANNED_COMMANDS` — refused **everywhere**, Initial and Scheduled alike. Two different reasons land a command here: *the pipeline owns it outright* (`mirv_recordmovie_start`/`_stop`, `mirv_movie_ffmpeg`, `host_framerate` — no setting corresponds to any of them, and a user's own value misroutes footage or desyncs playback with no visible failure), or *DoD's client quits the game over it* (`r_drawentities`, `cl_lw` — `CHud::Redraw` forces the value back, prints an error and calls `quit` if either is not `1`). Reachability differs between those two and `FATAL_CVARS` encodes it: `cl_lw` always takes the value it is given, whereas GoldSrc itself clamps `r_drawentities` back to `1.0` while `sv_cheats` is `0`, making a config line setting it inert. Both stay refused as *typed commands* — cheap, and `cfg_scan` cannot see what else a user's configs did — but only `cl_lw` is reported as a fatal config cvar unconditionally.
  - `SCHEDULED_BANNED_COMMANDS` — fine at demo load, refused **only when scheduled**: `r_decals`, `mirv_fov`, `gl_widescreenfov`, `mirv_movie_filename`.
  - `MID_DEMO_HAZARDS` — shadowed with a warning, not refused, because each corresponds to a real setting.
  - `NOOP_EVERYWHERE_COMMANDS` (`exec`, `quit`) and `NOOP_IN_INIT_COMMANDS` (`mirv_movie_filename`) — reported as doing nothing. The engine drops the first pair from a demo's message stream; the pipeline overwrites the second before anything reads it.

  Adding a new pipeline-internal command that Initial/Scheduled Commands could reach: decide which list it belongs to before shipping it unprotected. Do not describe this set from memory — it has been re-tiered more than once (`mirv_movie_filename` moved in #161; `mirv_movie_separate_hud` disappeared with #214); read `cfg_scan.rs`.
- **User Config Files:** The game's own `.cfg` files are the user's. **Detect and warn, never write.** They override nothing the app assumes — a `config.cfg` ending in `exec movie.cfg` can set `mirv_fov` or `r_decals` behind the pipeline entirely. `native/src/patch/cfg_scan.rs` is read-only by construction; keep it that way.
- **Tauri IPC:** Every frontend `invoke()` call in `ipc_bridge.js` must implement a `.catch()` block to prevent swallowed Rust backend errors.
- **Filesystem Picking:** Force the use of `@tauri-apps/plugin-dialog` native pickers instead of text input paths to prevent string escaping vulnerabilities.