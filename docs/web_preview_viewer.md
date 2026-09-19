# Web Preview Viewer — Handoff

**Status:** Working end-to-end for section-start clips (confirmed in a real
browser: map, players, weapons, correctly turning camera). Mid-stream cuts
— the actual real-world use case, since highlights are never section-start
— have a **new fix, byte-verified but not yet retested in a real browser.**
Entity state reconstructs correctly (confirmed — no more
`CL_ParseDeltaPacketEntitiesGS`, kill feed works). The local-player
(`svc_clientdata`) ring-buffer desync believed (as of Step 5) to be the sole
remaining cause of the floating/rotating camera was fixed and byte-verified
correct there, but the symptom persisted anyway. **Step 6 (2026-09-17)
refutes Step 5's leading theory** (client-DLL reconnect detection — checked
against real `hlsdk-portable` source, no such code path exists) **and finds
the actual bug**: a second, unrelated ring buffer (`cl.commands[]`, used for
movement prediction) was also colliding, because synthetic frames cloned the
anchor's `outgoing_sequence` verbatim instead of backdating it per frame.
Fixed and byte-verified against real output. **Needs a real-browser retest
before this status line can say "fixed"** — see "Step 6 findings" for what
to check if the camera is still broken after this.
**Last updated:** 2026-09-17 (Step 6, a fresh session picking this up cold
per this doc's own instructions — no demo/asset files or browser available,
so the fix is source- and byte-verified only; previous handoff was 2026-08-14).
**Spans two repos:** `dod-tools/xash-transcode/` (this repo) and `../dod-web-demo-viewer/` (sibling).

---

## STOP AND READ — exact state as of 2026-08-14 (late), mid-session

Picking this up cold? Do these in order:

1. **Uncommitted code exists — check before doing anything else.**
   ```
   cd dod-tools && git status --short
   ```
   As of this handoff: `xash-transcode/src/lib.rs` and
   `xash-transcode/src/main.rs` are **modified but not committed**
   (mid-stream-cut baseline-synthesis work, now including the clientdata
   ring-buffer chain fix — point 3 below), and `xash-transcode/examples/
   verify_clip.rs` is a new **untracked** file (a small diagnostic — walks
   raw IDEM bytes and prints each frame's `incoming_sequence` + payload
   header, used to confirm the fix actually lands in the written output;
   keep it, it's cheap and has already earned its keep once this session —
   see point 3). `xash-transcode/src/usercmd.rs` (the earlier camera fix)
   is already committed and needs no changes. The sibling repo
   (`../dod-web-demo-viewer/`) also has an uncommitted one-line fix to
   `dev-server.js` — see point 2.

2. **A local static server needs to be running**, and there are two real
   bugs in `dev-server.js` fixed this session that matter for how you start
   it:
   - **No cache headers at all** meant the browser could silently keep
     serving a stale cached copy of the 90 MB asset zip across rebuilds —
     this cost real time this session (two rounds of "the fix didn't work"
     turned out to at least partly be "the browser never re-fetched").
     Fixed: `Cache-Control: no-store` added to every response.
   - **The path-safety check is broken for a relative root.** `filePath =
     path.join(root, urlPath)` stays relative, but the check compares it
     against `path.resolve(root)` (absolute) with `.startsWith()` — always
     false, so *every* request 403s when you start the server with `.` as
     the root argument. **Start it with an absolute path**, not `.`:
     ```
     node dev-server.js "C:/Users/Chris Coventry/Repos/dod-web-demo-viewer" 8080
     ```
     (run from inside `dod-web-demo-viewer/`, or pass the absolute path from
     anywhere). This bug was not fixed in `dev-server.js` itself this
     session (only worked around by always passing an absolute root) —
     worth a real fix (`path.resolve(root)` once up front, use that
     everywhere) if this file gets touched again.
   - Content pack: `../dod-web-demo-viewer/assets/dod_test_pack_with_demo.zip`
     (~90 MB, gitignored, real file on disk, survives across sessions) —
     see "Step 3/4 findings" for what's in it. Two demo files inside:
     `?demo=dodEmmanuelEarly` (section-start clip, **fully working**, and
     — new insight this session — structurally different from the other
     clip in a load-bearing way: it gets **zero synthetic frames spliced
     in**, see point 3) and `?demo=dodEmmanuelClip` (300–320s mid-stream
     cut, **the one still broken**).
   - Full test URL: `http://localhost:8080/index.html?demo=dodEmmanuelClip&pack=assets/dod_test_pack_with_demo.zip&dev=1`
     — the `pack=` parameter is **required**, the page's hardcoded default
     (`CONFIG.assetPack`) points at an older, incomplete pack
     (`dod_custom_pack.zip`) from before the base-WAD/font/hud-sprite fixes
     in Step 3; omitting `pack=` reproduces a `Host_InitCommon: couldn't
     load gfx.wad` fatal that looks like a regression but isn't one.

3. **The clientdata ring-buffer fix is now implemented, byte-level
   verified correct, and the visible bug is still there anyway — this is
   the actual state of play, read carefully.**

   The *previous* handoff's theory (one synthetic frame, override its own
   `sequence_info.incoming_sequence` to the byte `SvcClientData.
   delta_update_mask` decodes to) was **incomplete, not wrong**. Empirical
   trace against the real demo (`analysis_target_pov.dem`, 300s cut)
   showed GoldSrc deltas `svc_clientdata` against "the last frame this
   client acknowledged" — a *sliding* reference lagging the current frame
   by a few packets (round-trip latency), not a fixed point. The first
   several post-cut `svc_clientdata` messages each reference a *different*
   pre-cut frame in turn (observed: bytes `5, 6, 7, 8`, with `8` repeated
   once), before the window "ages past" the cut boundary and later
   messages start referencing real retained frames that populate
   themselves normally. One synthetic frame only satisfied the first of
   these.

   **Fixed:** `client_data_chain()` in `xash-transcode/src/lib.rs` walks
   the real post-cut stream, decodes each leading message's expected
   `delta_sequence`, and collects every distinct pre-cut slot referenced
   until one resolves to a frame inside the retained window. `cut()` now
   splices in *one synthetic baseline frame per slot in that chain* (for
   this demo: 4 frames, sequence numbers 5/6/7/8), each carrying the same
   reconstructed entity+clientdata payload. Verified two ways, not just by
   reasoning: (a) `client_data_chain()` on the real demo returns exactly
   `[5, 6, 7, 8]`, matching a manual trace of the wire bytes; (b)
   `examples/verify_clip.rs` re-parses the *actual written output file*
   (not pre-write in-memory state) and confirms all 4 synthetic frames are
   present with the correct `incoming_sequence` values, immediately before
   the real stream resumes at `165385`. A dump of the reconstructed
   `state.client_data` content also came back sane (`health=100`,
   `deadflag=0`, `iuser1=0`, plausible origin/velocity) — not corrupted.

   **And the camera is still broken, unchanged, after all of that.**
   Confirmed in a real browser, twice, including after fixing the
   dev-server caching bug from point 2 to rule out a stale-content
   false-negative.

   **New clue, not yet chased down:** the user noticed a server MOTD box
   ("Nineteen Eleven 24/7 KTP…") reappearing mid-playback on the broken
   clip, and correlated it directly with brokenness — it does not appear
   on the working section-start clip. No `motd.txt` exists anywhere in the
   mounted content pack (checked), so this isn't a stray leftover file
   being read; whatever text is showing was almost certainly received
   once already, near true t=0 during the shared, untouched `LOADING`
   entry, and is being **redisplayed** later. The structural fact that
   lines up cleanly with this: `dodEmmanuelEarly` is cut from `start=0`,
   so `replay_state_before(pe, lo=0)` finds nothing before time zero and
   the entire synthetic-baseline block in `cut()` never fires — it is the
   *only* one of the two clips with zero synthetic frames. Every version
   of this fix tried this session (one frame, four frames) spliced
   synthetic frames into `dodEmmanuelClip` only. **Leading theory for next
   session:** injecting a full baseline mid-game — something that
   structurally never happens in a real recording (Step 4: only 4 full
   `svc_packetentities` exist in the entire 986s source, all within 0.07s
   of connect) — may itself be read by the *client DLL* (not the engine)
   as "you just (re)connected," triggering connect-adjacent UI (MOTD
   redisplay) and state resets (default/spectator camera), **independent
   of whether the synthetic frame's content and sequence numbers are
   correct.** This has not been confirmed against source — it requires
   reading `hlsdk-portable`'s *client* DLL (a different codebase than
   `xash3d-fwgs`'s engine, which is all that's been read so far this
   session and last). Concretely: clone
   `https://github.com/FWGS/hlsdk-portable` and look for what
   `HUD_UpdateClientData` / `ServerActivate` / MOTD-display hooks key off
   of — specifically whether a full (non-delta) `svc_clientdata` or
   `svc_packetentities` arriving after the initial connect trips anything
   connect-like.

   If that theory pans out, the fix is no longer "get the sequence number
   right" — it becomes "don't send a full baseline as a *separate*
   mid-game event at all," which likely means folding the reconstructed
   state into the frame differently (e.g. as pure deltas against a
   locally-tracked "current" state per real per-message field-presence,
   rather than one/several messages that look like full resyncs) — a
   more involved change than anything implemented so far. Alternatively,
   if the client DLL turns out not to be the cause, the next thing to
   check is `cls.spectator` (`cl_parse.c:1092` early-returns out of all
   origin/health/deadflag resolution when true) — ruled *unlikely* this
   session because the only code path that sets it (`CL_ParseHLTV`,
   `cl_parse.c:1956`) requires an `svc_hltv` message a normal player POV
   recording wouldn't carry, but not conclusively ruled out.

4. **Console noise that's already triaged and harmless**, don't
   re-investigate: missing sounds, missing `gfx/shell/*` UI images,
   `GL_INVALID_ENUM`, `CL_ParseUserMessage: No pfn ...` (expected —
   DoD-specific HUD usermessages the generic `hlsdk-portable` client
   doesn't implement), `CL_FireEvent: events/weapons/*.sc not precached`
   (expected — `pack`'s resourcelist scraping doesn't currently walk
   event-script precaching; muzzle flash/shell-eject/pain effects are
   silently dropped, not broken), `Couldn't open file overviews/...txt`,
   `Error: VGui_LoadProgs: Failed to find VGUI support API entry point` /
   `Failed to load vgui_support library` (checked this session — fires
   during generic client-DLL init, well before `GoldSrc serverdata packet
   received`; `libvgui_support.wasm` just isn't shipped in this
   `xash3d-fwgs` npm build; unrelated to demo content), and —
   specifically for this session's testing — `gfx/shell/head_*` /
   `sound/common/launch_*` / repeated `SDL: [Error] That operation is not
   supported` lines are just the tester navigating Xash's own pause/options
   menu (pressing Escape/`~`), not an engine or demo failure.

5. **`index.html` still has TEMPORARY TEST EDITS** (grep for `TEMPORARY TEST
   EDIT`): forces `GAME_DIR = 'valve'` + `hlsdk-portable@0.1.3` client/server
   libs (no compiled DoD wasm exists yet), and ties `-console -dev 2` to
   `?dev=1`. Real edits in a real file (not scratchpad) — they persist.
   Leave them until the clientdata bug is resolved or reverted on purpose.

6. **Everything under this session's scratchpad is gone** in a new chat
   (session-scoped temp dir) — the extracted `pack_extract/` working
   folder, and a shallow clone of `xash3d-fwgs` used to read the engine
   source cited above (that clone is *how* point 3's citations were
   verified — re-clone if you need to check anything else in the engine:
   `git clone --depth 1 https://github.com/FWGS/xash3d-fwgs.git`, then
   look under `engine/client/parse/cl_parse.c` and `cl_parse_gs.c`; for
   the *client DLL* lead in point 3, clone `hlsdk-portable` instead — a
   different repo). **None of that is needed to *use* the existing test
   pack** — both demo files are already inside the persisted zip. If
   another cut or fix is needed, unzip the pack back into a working
   folder, edit, rebuild with the persisted script:
   ```
   node make-test-zip.js <extracted-folder> assets/dod_test_pack_with_demo.zip
   ```
   (in `dod-web-demo-viewer/`, not scratchpad — do **not** use
   PowerShell's `Compress-Archive`, see Step 3 finding #3 for why). To cut
   a fresh demo window from the source recording (after building
   `xash-transcode` — `cargo build --release` from inside that crate):
   ```
   cd xash-transcode
   ./target/release/xash-transcode.exe cut ../demos/analysis_target_pov.dem <out.dem> <start> <end> --preroll 0
   ```
   `cut`'s signature takes a second, Parse-mode-parsed copy of the demo as
   an extra argument (`cut(&demo, &parsed, start, end, preroll, &opts)`);
   `main.rs`'s CLI already handles this (loads both), but if calling the
   library directly from other code, both parses are required. To inspect
   what actually landed in a transcoded output file's raw bytes (not the
   pre-write in-memory state), use the new diagnostic:
   ```
   cargo run --release --example verify_clip -- <path-to-transcoded.dem>
   ```
   prints each entry's first ~12 `dem_read`/`dem_norewind` frames with
   their `incoming_sequence` and payload header bytes. "Step 3/4/5
   findings" documents exactly what's in the pack and where each piece
   came from, so extending it doesn't require re-deriving anything.

---

## Goal

An in-app demo viewer for **triage only** — deciding whether a detected highlight is worth capturing. Not a capture path.

Requirements, in priority order:

- World (map) + player models + a first-person POV.
- Some HUD for the POV. Nice-to-have, not blocking.
- **No audio required.**
- Highlight list beside the viewer; clicking a highlight jumps to that clip.
- Must load fast — each preview is one short, single-highlight demo.

### Why this exists

The current "Preview Demo" flow launches `hl.exe` with HLAE injected and hands the user a full demo with `svc_director` events in it. The user must then drive playback manually through DoD's events window. Most of the DoD 1.3 community doesn't know that window exists. The flow also dumps the user out of the app entirely.

### Architectural decision

**Tauri's frontend is a webview, so the web viewer and the in-app viewer are the same artifact.** Build it once as a static web app; embed it in `desktop-studio/` and publish it to GitHub Pages from the same source. There is no desktop-vs-web fork to maintain.

---

## Engine findings (verified — do not re-derive)

Read from `xash3d-fwgs/engine/client/cl_demo.c` on `master`, cross-checked against `dem-patch/src/demo_parser.rs`.

### Xash3D cannot open a DoD demo

Different containers. `CL_ParseDemoHeader` rejects on `hdr->id != IDEMOHEADER` before reading anything else.

| | GoldSrc (`.dem` from `hl.exe`) | Xash3D |
|---|---|---|
| magic | `"HLDEMO\0\0"` (8 bytes) | `"IDEM"` (i32) |
| demo protocol | 5 | 3 |
| path fields | `[260]` | `[64]` |
| extras | `map_checksum` | `host_fps` (f64), `comment` |

The "GoldSrc protocol 48 support" in Xash3D is for **connecting to GoldSrc servers** (`connect ip:port gs`), not for reading demo files. Don't be misled by it — see `Documentation/goldsrc-protocol-support.md` upstream.

### The one door that is open

```c
#define PROTOCOL_GOLDSRC_VERSION_DEMO (PROTOCOL_GOLDSRC_VERSION | BIT(7))  // 176
```

`CL_ParseDemoHeader` accepts `net_protocol == 176`, and `CL_GetProtocolFromDemo` then returns `PROTO_GOLDSRC`, so the engine decodes the contained messages with GoldSrc protocol-48 semantics. A DoD demo's payload is already exactly that. **Only the wrapper is wrong** — hence the transcoder.

### Structural facts confirmed against real demos

- GoldSrc `DemoInfo` block preceding each network message is **436 bytes**: `timestamp(4) + RefParams(232) + UserCmd(52) + MoveVars(132) + view(12) + viewmodel(4)`.
- `SequenceInfo` maps **field-for-field** onto Xash's `CL_ReadDemoSequence` — same seven i32s, same order. Straight copy, no translation.
- DoD demos have two directory entries: `LOADING` (type 0) and `Playback` (type 1). Maps cleanly onto Xash's `DEMO_STARTUP` / `DEMO_NORMAL`.
- Largest observed network message: **27,545 B**, under `MAX_INIT_MSG` (0x8000). Headroom exists but is not large.
- `DirectoryEntry.frame_count` in the header is **unreliable** — it counts only network-message frames, not total frames. Walk the frames instead.

### Frame type mapping (9 GoldSrc → 6 Xash)

| GoldSrc | Xash | note |
|---|---|---|
| `NetworkMessage(Start)` | `dem_norewind` (1) | signon |
| `NetworkMessage(Normal)` | `dem_read` (2) | gameplay stream |
| `DemoStart` | `dem_jumptime` (3) | resets section clock |
| `NextSection` | `dem_stop` (6) | terminator; a section without one is treated as corrupt |
| `DemoBuffer` | `dem_userdata` (4) | opt-in (`--userdata`), off by default |
| `ConsoleCommand` | — | dropped |
| `ClientData` | — | dropped |
| `Event` / `WeaponAnimation` / `Sound` | — | dropped |

**Dropping `ConsoleCommand` is confirmed acceptable.** Those carry injected director commands for the real capture demo; preview demos need none of that. This is why transcoded demos are explicitly *not* a capture path.

---

## What exists now

### `dod-tools/xash-transcode/` — standalone crate

Declares its own `[workspace]`, so it does **not** join the dod-tools workspace and the root `Cargo.toml` is untouched. Add it to `members` when you want `cargo build --workspace` to cover it.

```
src/lib.rs        transcode / cut / validate  — pure, no I/O, wasm32-clean
src/idem.rs       Xash IDEM constants + layout, annotated with cl_demo.c references
src/resources.rs  svc_resourcelist + BSP entity-lump extraction — pure, wasm32-clean
src/writer.rs     minimal LE byte writer (dem::byte_writer is a private module)
src/packer.rs     filesystem + zip layer for `pack` — binary only, NOT in the lib
src/main.rs       CLI
reference/        Python oracles the Rust was derived from — keep as cross-check
fixtures/         a pre-built 20s transcoded clip for testing the viewer
```

```bash
cd xash-transcode
cargo run --release -- inspect  ../demos/analysis_target_pov.dem
cargo run --release -- convert  ../demos/analysis_target_pov.dem /tmp/full.dem
cargo run --release -- cut      ../demos/analysis_target_pov.dem /tmp/clip.dem 300 320
cargo run --release -- validate /tmp/clip.dem
cargo run --release -- pack     ../demos/analysis_target_pov.dem /tmp/pack.zip \
                                --game-root "C:/.../steamapps/common/Half-Life"
```

**Parse mode matters.** Use `MessageDataParseMode::Raw` for transcode/cut — payloads stay borrowed bytes, so it's byte-exact and much faster. `pack` needs `Parse` to reach `svc_resourcelist`; that's why it's slow, and it runs once per demo rather than once per preview.

### `../dod-web-demo-viewer/` — sibling repo, static site

```
index.html             canvas host, CDN ESM bootstrap, ZIP mount, ?demo= query wrapper
coi-serviceworker.js   COOP/COEP shim (GitHub Pages cannot set headers)
assets/README.md       content layout + the game-DLL problem
```

Non-obvious details already handled — don't "fix" these:

- **Every engine `.wasm` is explicitly mapped in `filesMap`.** The wrapper's `locateFile` is `filesMap[path] ?? path` with no fallback to the module's origin; an unmapped binary resolves against the *page* URL and 404s.
- **Import must be `https://cdn.jsdelivr.net/npm/xash3d-fwgs@1.2.2/+esm`.** `dist/index.js` is TS output using extensionless relative specifiers, which native browser ESM cannot resolve.
- **Root-absolute asset paths are rewritten** through `document.baseURI`, because a GitHub Pages project site serves from `/<repo>/`.
- **`downloadAndExtractZip` is ours, not the library's.** `xash3d-fwgs` exposes only the raw Emscripten FS. Implemented in `index.html` (store + deflate + ZIP64 via native `DecompressionStream`) and grafted on via `Object.create(xash.em.FS)`.
- **Boot order is load-bearing:** `new Xash3D()` → `await init()` → mount assets → `main()`. Writing files before `init()` throws; after `main()` races the gamedir scan.

---

## Measured results

`analysis_target_pov.dem` — dod_Emmanuel, 66.5 MB, 986 s:

| output | size | % of source |
|---|---|---|
| full transcode | 16.2 MB | 24.3% |
| 86 s clip | 1.5 MB | 2.3% |
| 30 s clip | 524 KB | 0.8% |
| **20 s clip** | **369 KB** | **0.6%** |

The shrink comes from dropping `ClientData` + `DemoBuffer`, which are ~76% of frames and carry nothing the engine needs for playback. A 369 KB preview satisfies the load-fast requirement comfortably.

---

## Verification status — read this before trusting anything

| Component | Status |
|---|---|
| HLDEMO struct layout | **Verified.** All four sections of two real demos walked to *exactly* their declared `file_length`. |
| HLDEMO → IDEM transcode | **Verified** in Python against real demos; output passes a validator reimplementing `CL_ParseDemoHeader`, `CL_PlayDemo_f`, and the `CL_DemoReadMessage` walk. |
| Clip cutting | **Verified** structurally (valid output at several time windows). Visual correctness unproven. |
| Rust port of the above | **Builds and runs clean (2026-08-13).** `cargo build` succeeded with zero errors/warnings on first try — no fixes needed. `inspect`/`convert`/`cut`/`validate` all ran against `analysis_target_pov.dem`: full-transcode and 300–320s-cut sizes match this doc's earlier measured-results table almost exactly (16,156,373 B vs. "16.2 MB"; 368,943 B vs. "369 KB"), and the fresh cut is **byte-for-byte identical** to the checked-in `fixtures/dod_Emmanuel_300-320s.idem.dem`. `validate` (the `CL_ParseDemoHeader`/`CL_PlayDemo_f`/`CL_DemoReadMessage` reimplementation) passes on both outputs. Could not re-run the Python oracles directly for a true side-by-side diff — this machine has no real Python interpreter (only the Windows Store stub alias); the fixture comparison above is the closest available substitute. |
| BSP wad/sky extraction | **Logic verified** in Python across 11 cases (backslash/forward-slash paths, duplicates, junk segments, missing keys, worldspawn scoping, header offsets). Rust port unproven. |
| `svc_resourcelist` extraction | **Exercised and fixed (2026-08-13).** Ran `pack` against `analysis_target_pov.dem` + the real `dod` install and found two real bugs, both fixed in `xash-transcode/src/resources.rs` (see below). After fixing, `pack` reports **"All required files found"** (268 wanted, 262 packed, 80.4 MB → 53.5 MB zipped) with 28 legitimate size-mismatched viewmodels (`v_*.mdl`, local ~3× the server-declared size — this install has HD/modified viewmodels, not the set the demo's recording client used; a real content-mismatch, not a bug). |
| Viewer JS (ZIP, query string, path resolution) | **Verified** — byte-exact ZIP roundtrip on deflate + store, 22 query-string cases, zip-slip rejection. |
| **Does Xash actually replay a transcoded DoD demo?** | **Yes, confirmed end-to-end (2026-08-14 real-browser tests) — see "Step 3 findings" #12–15 for the full trail.** For a clip cut from a section start: the engine parses signon data, connects, **renders the real level** (map geometry, sky, lightstyles, all DoD player/weapon models), and — after the `dem_usercmd`/view-angle fix (#14) — **has a correctly turning first-person camera with visible player movement**. All visually confirmed by the project owner in a real browser on hardware WebGL2. `PROTO_GOLDSRC` demo playback is real; **Open Risk #1 is resolved.** Every blocker hit getting here turned out to be either a missing *base-engine* file the demo-scoped test pack had no reason to include (base WADs, `delta.lst`, Xash's replacement UI fonts, `sprites/hud.txt` + base sprites — fixed 2026-08-14), or the missing `dem_usercmd` frames carrying recorded view angles (fixed 2026-08-14, see #14). **Remaining known gap, updated 2026-08-14 (Step 5):** the original `CL_ParseDeltaPacketEntitiesGS: (7 should be 49)` freeze is fixed (Step 4) and stays fixed. Mid-stream cuts still don't work end-to-end, but the symptom moved: no more entity freeze, no more `CL_ParseDeltaPacketEntitiesGS` warning — instead a floating/rotating camera plus a server MOTD reappearing mid-playback, persisting even after the clientdata ring-buffer sequence bug (also Open Risk #3) was fixed and independently byte-verified correct. Current leading theory: unrelated to sequence-number correctness at all — a full baseline injected mid-game may be read by the client DLL as a reconnect event. See "Step 5 findings" and "STOP AND READ" point 3 for the full trail and the concrete next step (read `hlsdk-portable` client source). Separately, a real double-free bug surfaced in this engine build's shutdown path — doesn't look content-related, not chased. |

---

## Bugs found & fixed while running `pack` (2026-08-13)

First real run — `cargo run -- pack ../demos/analysis_target_pov.dem <sibling>/assets/dod_custom_pack.zip --game-root "D:\...\Half-Life - PRE-Anniversary for Movies"` — reported **376 required files MISSING**, all `model *N` (e.g. `*1`, `*120`). Both turned out to be bugs in `xash-transcode`, not missing content:

1. **Inline BSP submodels treated as files.** GoldSrc names brush entities baked into the map itself (doors, buttons) `*N` — an index into the BSP's own model lump, not a `.mdl` on disk. `ResourceKind::Model::is_file()` didn't exclude them the way `Decal` already was. Fixed with `is_inline_bsp_model()` in `resources.rs`, filtering them out of `resources()` entirely (same treatment as decals). Dropped the false-missing count from 376 to 209.

2. **The bigger one: `BitSliceCast::get_string()` (`dem-patch/src/bit.rs`) doesn't strip NUL padding.** It dumps a fixed-size byte buffer straight to `String` with no C-string termination handling, so short names come back with trailing embedded `\0` bytes (e.g. `"maps/dod_Emmanuel.bsp\0"`). A `\0`-suffixed path can never resolve on any filesystem, so this alone accounted for the rest — confirmed by finding stock files (`dod/models/null.mdl`, `dod/models/allied_ammo.mdl`) reported missing while sitting right there on disk. Fixed with `clean_resource_name()` in `resources.rs`, truncating at the first NUL right where `get_string()` is read.

   **This second bug is scoped narrowly to `xash-transcode` on purpose.** `get_string()` is a shared `dem-patch` API used workspace-wide (`analysis`'s player-name/chat decoding included), and CLAUDE.md gates public-API changes behind explicit request. **The same corruption plausibly affects other `get_string()` callers outside this crate** (e.g. scoreboard player names, chat text) — worth a dedicated look if anyone's seen truncated-looking names or odd trailing characters elsewhere in the app. Not chased further here; out of scope for this handoff.

Both fixes have unit tests in `resources.rs` (`inline_bsp_models_are_excluded`, `resource_names_are_truncated_at_the_first_nul`).

---

## Step 3 findings: booting the viewer against `hlsdk-portable` (2026-08-14)

Ran the real viewer against `analysis_target_pov.dem`'s transcoded 300–320s
clip and the real DoD install, alternating between headless Playwright
(fast iteration, but blind to some things) and the user's real browser
(slower to iterate, but far more informative — real `alert()` dialogs, a
working DevTools console, no headless-specific crashes). **Bottom line so
far: `PROTO_GOLDSRC` demo playback is real.** The engine parses the
transcoded IDEM file's signon data and prints the *original recording
server's* own hostname/build/map-cycle straight out of it. Getting from
there to an actual rendered level has been a long chain of "engine wants a
base file our demo-scoped `pack` tool has no reason to include" bugs, fixed
one at a time. Still not fully resolved — see "STOP AND READ" at the top of
this doc for exactly where this stands and what to check next.

### Fixes applied, in the order they were hit

Each of these was found by actually running the viewer and reading the
real error, not by guessing. All are either code fixes (in `index.html` /
`assets/README.md`, real files, committed-repo-adjacent) or additions to
the **test content pack** (`assets/dod_test_pack_with_demo.zip`, gitignored,
not something end users or the real `pack` tool need to worry about — see
below on why).

1. **Stale CDN path.** `assets/README.md`'s documented
   `hlsdk-portable@latest` paths were wrong — the package added a `valve/`
   layer under `dist/` since that snippet was written. Real paths:
   `dist/valve/dlls/hl_emscripten_wasm32.wasm` and
   `dist/valve/cl_dlls/client_emscripten_wasm32.wasm`. Fixed in both
   `index.html` (pinned `@0.1.3`) and `assets/README.md`.
2. **Missing base WADs.** The engine needs core `valve/` WADs (`gfx.wad` at
   minimum) that `pack` correctly never includes (engine-level, not
   referenced by any map/demo). Without it: a bare `Infinity` thrown
   (not a real `Error` — see point 8) during `Host_InitCommon`.
3. **`Compress-Archive` zips are unsafe for this viewer.** PowerShell's
   `Compress-Archive` writes backslash path separators and an explicit
   directory-marker entry (e.g. `models\player\`) for non-empty
   directories with a trailing *backslash*. The viewer's zip reader only
   recognizes a trailing forward slash as a directory marker
   (`entry.name.endsWith('/')`), so the marker becomes a bogus zero-byte
   *file*, and the next real file under that path fails `FS.mkdirTree()`
   with `ENOTDIR`. Not a `pack`-tool bug (its Rust `zip` crate writes
   forward slashes, no directory markers) — only hit building an ad hoc
   test pack on Windows. **Fix: don't use `Compress-Archive` for a pack
   this viewer will read.** Used `dod-web-demo-viewer/make-test-zip.js`
   instead (a real, committed-repo-adjacent file, not scratchpad — see
   "STOP AND READ" point 6 for usage).
4. **`liblist.gam`'s `gamedll`/`gamedll_linux` decide the *actual* server
   wasm filename — independent of the JS `serverLib` config.** The real
   DoD `liblist.gam` (which `pack` always copies in) declares
   `gamedll_linux "dlls/dod.so"`. The engine derives
   `dlls/dod_emscripten_wasm32.wasm` from that at runtime and 404s (no
   compiled DoD wasm exists), regardless of what the JS wrapper's
   `libraries.server` override points at — that override only covers the
   wrapper's *own* initial dylib load, not this liblist-driven one. **Fix
   (test pack only):** edited the packed `liblist.gam`'s `gamedll`/
   `gamedll_linux`/`gamedll_osx` to say `hl` instead of `dod`, and put real
   copies of `hl_emscripten_wasm32.wasm` / `client_emscripten_wasm32.wasm`
   (from `hlsdk-portable@0.1.3`'s CDN dist) directly in the pack under
   `dlls/` / `cl_dlls/` so any lookup path finds them locally.
5. **Missing `delta.lst`.** Another core engine file (network
   delta-encoding field tables — directly relevant to demo/network message
   decoding) that `pack` has no reason to include. Symptom:
   `Delta_InitFields: couldn't load file delta.lst`, fatal. **Fix:** copied
   `valve/delta.lst` from the real install (used `valve/`'s, not `dod/`'s,
   since we're running the generic `hlsdk-portable` server logic under
   `-game valve` — using DoD's own delta.lst would declare fields that
   server binary doesn't know).
6. **Missing UI fonts.** `Unable to read font file gfx/fonts/FiraSans-Regular.ttf!`
   / `tahoma.ttf!`, repeated, then the same fatal "reinstall" message as
   #7 (this was the *first* thing that ever triggered it — turned out to
   be a font problem, not the more interesting cause found later). These
   TrueType fonts are Xash3D-FWGS's own replacement for GoldSrc's original
   bitmap console fonts — not part of any GoldSrc install at all. **Fix:**
   found and extracted from `dist/valve/extras.pk3` in the `xash3d-fwgs@1.2.2`
   npm package (a bundled zip of engine-supplied extras — this is the
   standard place to look for this category of missing file). Both fonts
   live at exactly `gfx/fonts/FiraSans-Regular.ttf` and
   `gfx/fonts/tahoma.ttf` inside it.
7. **The "reinstall" fatal message, found for real.** `"There is something
   wrong with your game data! Please, reinstall"` recurred even after #6,
   and looked at first like it might be a CRC/consistency check (it fires
   right after `GoldSrc serverdata packet received.`, which made a
   checksum mismatch plausible). **It is not that.** Cloned
   `github.com/FWGS/hlsdk-portable` and grepped for the literal string —
   found in `cl_dll/hud.cpp`, inside `CHud::VidInit` (search "number_0" in
   that file to jump straight there): it calls
   `SPR_GetList("sprites/hud.txt", &m_iSpriteCountAllRes)`, and if the
   resulting sprite list doesn't contain an entry named `"number_0"`
   (`GetSpriteIndex("number_0") == -1`), it prints exactly this message via
   `HUD_MessageBox` and issues `quit`. `sprites/hud.txt` is the manifest
   mapping names like `number_0` (HUD ammo/health digit sprites) to actual
   `.spr` files — a standard base-engine HUD file, never referenced by any
   demo's precache list, same category as everything above. **Fix:**
   copied `valve/sprites/hud.txt` and all 164 `valve/sprites/*.spr` files
   from the real install into the pack (`cp -n`, so demo-specific sprites
   already present from `pack`'s own resource-list-driven packing were not
   clobbered). **Confirmed fixed** — headless retest shows no "reinstall"
   message, no fatal shutdown, and the log now continues well past
   `Remote host: KTP - New York 1` into loading (missing, since audio isn't
   packed) sound files — non-fatal `Error: Could not load sound ...` lines,
   not blockers.
8. **Demo filename must not contain a `.` before `.dem`.** The fixture is
   named `dod_Emmanuel_300-320s.idem.dem` on disk. Passing
   `?demo=dod_Emmanuel_300-320s.idem` produced
   `Error: couldn't open dod_Emmanuel_300-320s.dem` — the engine's
   `playdemo` extension handling treats the `.` before `idem` as an
   existing (wrong) extension and *replaces* everything after it with
   `.dem`, silently losing `idem`. Purely a test-fixture naming collision,
   not a transcoder or viewer bug. **Fix:** renamed the packed copy to
   `dodEmmanuelClip.dem` (no embedded dots) and use `?demo=dodEmmanuelClip`.
9. **`-console` doesn't persist across reloads.** The in-game "Enable
   developer console" toggle is stored in a config file the pack doesn't
   carry, so it resets on every fresh VFS mount. **Fix (permanent,
   `index.html`):** `buildArgs()` now pushes `-console -dev 2` whenever
   `?dev=1` is set, so the real engine console (not just the page-level
   `#log` panel) is on automatically every time.
10. **A real, separate bug found along the way:** headless Chromium threw
    an uncaught `SyntaxError: Failed to execute 'querySelector' on
    'Document'` from inside `_emscripten_get_element_css_size` (WASM
    passed a garbage pointer as a CSS selector string) when interacting
    with the canvas (click, or pressing Escape at the main menu in the
    user's real browser too — this reproduced in both headless *and* real
    Chrome, so it's not purely a headless artifact). Also saw, in the
    engine's own shutdown path after the "reinstall" abort:
    `Mem_FreeBlock: not allocated or double freed (free at
    ../engine/common/cmd.c:604)` — a genuine double-free, and the likely
    source of the bare `Infinity` throws seen throughout (Emscripten
    aborts sometimes surface as a non-Error thrown value with no
    `.stack`). **Neither of these looks caused by our content** — they read
    as upstream engine bugs in this `xash3d-fwgs@1.2.2` build. Not chased
    further; flag if they recur.
11. **Headless-only Out-Of-Memory.** After fix #7, headless Playwright
    (forced to the CPU software renderer, `?renderer=soft`, specifically to
    dodge headless GPU quirks) hit `RuntimeError: Aborted(OOM). Build with
    -sASSERTIONS for more info.` right as it started loading real level
    content. Software rendering keeps the whole framebuffer/texture cache
    in WASM heap rather than GPU memory, so this is plausibly specific to
    that renderer choice + headless memory limits, not a real blocker — a
    real browser uses the default `gl4es`/WebGL2 renderer, which is far
    lighter on system RAM.
12. **Confirmed in a real browser (2026-08-14 23:28): the level renders.**
    User tested `?demo=dodEmmanuelClip` (the 300–320s mid-stream cut) in
    real Chrome/Chromium with hardware WebGL2 (`GL_VERSION: OpenGL ES 3.0
    (WebGL 2.0 (OpenGL ES 3.0 Chromium))`) — **no OOM**, confirming #11 was
    headless/software-renderer-specific. Full boot trail: WADs load, decals
    init, `webgl2` renderer initializes (one harmless shader-version-310
    fallback to 300, self-recovering), main menu images mostly missing
    (`gfx/shell/*` — cosmetic, already known), then `+playdemo
    dodEmmanuelClip` runs: `GoldSrc serverdata packet received`, ~130 missing
    sound warnings (expected, `--sound` wasn't packed), then **every DoD
    player/weapon/sprite model loads successfully** (`p_garand.mdl`,
    `p_mg42bu.mdl`, `player/brit-inf/brit-inf.mdl`, etc. — confirms
    `svc_resourcelist`-driven packing pulled in everything needed), sky and
    all 14 lightstyles load, `CL_SignonReply: 1` then `2`, `client connected
    at 3.35 sec`. **New findings at this point:**
    - `Error: CL_ParseUserMessage: No pfn ClientAreas/WaveTime/GameRules/
      RoundState/InitObj/ReqState/VoiceMask` — six DoD-specific usermessage
      handlers the generic `hlsdk-portable` HL client (used per fix #4,
      since no real DoD client wasm exists) doesn't implement. **Expected,
      not a bug** — exactly the "silent film, no meaningful HUD" outcome
      predicted in "DoD client library — assessment" below. Non-fatal.
    - `Couldn't open file overviews/dod_Emmanuel.txt` — missing map-overview
      config, cosmetic, falls back to defaults.
    - **`Warning: CL_ParseDeltaPacketEntitiesGS: (7 should be 49)`** — fires
      twice, right at signon, then the console goes silent (only sporadic
      `SDL: [Error] That operation is not supported`, no further entity/
      kill/chat/round activity). User confirms: map is visible but frozen,
      nothing plays.
13. **Diagnostic clip confirms Open Risk #3 as the cause of #12's freeze.**
    A second clip, `dodEmmanuelEarly.dem` (cut 0–20s from the *true start*
    of the recording, instead of mid-stream at 300s), was transcoded and
    added to the pack alongside the original. Retested in the real browser:
    `dodEmmanuelEarly` reaches the same signon point with **no**
    `CL_ParseDeltaPacketEntitiesGS` warning, and the console shows real
    match activity afterward — a scrolling kill feed (`DODMYLIFE ...`
    entries) and dozens of `CL_FireEvent: events/weapons/{bar,mp44,kar,
    garand}.sc not precached` / `events/misc/pain.sc not precached` lines
    (weapon-fire and pain events actually firing repeatedly during a
    firefight — see #15 below for what these errors mean). `dodEmmanuelClip`
    (still in the pack, unchanged) reproduces the identical freeze +
    delta-warning on a second run, confirming it's deterministic. This
    isolates the mid-stream cut — specifically, the missing full
    `svc_packetentities` baseline that a section-start clip carries for
    free — as the actual cause, exactly matching Open Risk #3's original
    description. Not yet fixed; see "STOP AND READ" #4 for the scoped fix.
14. **Camera fix: `dem_usercmd` synthesis, root-caused and shipped
    (2026-08-14).** With #13's `dodEmmanuelEarly` clip playing past signon,
    the user reported the camera "doesn't turn or move crosshair at all"
    despite the map, players, and weapon-switching all working — positional/
    server-driven state was fine, but look direction was frozen. Root cause,
    confirmed by reading `xash3d-fwgs/engine/client/cl_demo.c` and
    `engine/common/net_encode.c` directly (shallow-cloned again this
    session, same as the `hud.cpp` trace in fix #7 — gone from scratchpad
    now, but no longer needed): **Xash's demo format carries recorded view
    angles in a frame type the transcoder never emitted.** GoldSrc bundles
    per-frame player input (crucially, view angles — there's no live mouse
    during demo playback, so this is the *only* source of camera direction)
    inside each `NetworkMessage`'s 436-byte `DemoInfo` header
    (`msg.info.usercmd`, confirmed present and populated in `dem-patch`'s
    already-parsed struct — `dem-patch/src/types.rs:248`). Xash has no
    inline equivalent: it expects a **separate `dem_usercmd` frame** per
    network message, written during live recording by `CL_WriteDemoUserCmd()`
    and read back by `CL_ReadDemoUserCmd()` (which always deltas against an
    all-zero baseline — `CL_WriteUsercmd(..., from = -1, ...)`). The
    transcoder's GoldSrc→Xash frame mapping never covered this case at all,
    so the data existed in memory throughout and was simply never written
    out. **Fix:** new module `xash-transcode/src/usercmd.rs` — a
    from-scratch implementation of Xash's generic delta-field bit encoder
    (`MSG_WriteDeltaUsercmd`/`Delta_WriteField`, `net_encode.c`) over the
    `DT_USERCMD_T` field table (transcribed from the real `delta.lst` in the
    test pack — `usercmd_t` block, 15 fields, exact order/bit-widths/types
    matter), plus a matching LSB-first bit writer verified against
    `net_buffer.c`'s `MSG_WriteOneBit`/`MSG_WriteUBitLong`/`MSG_WriteBitAngle`
    bit layout. Wired into `lib.rs`'s per-frame transcode loop: every
    `NetworkMessage` now gets a synthesized `dem_usercmd` frame immediately
    ahead of it, carrying real recorded view angles (pitch/yaw/roll) plus
    `lerp_msec`/`msec`/`buttons`/`lightlevel`/`impulse`; movement fields
    (`forwardmove`/`sidemove`/`upmove`/`impact_*`) are left at the encoder's
    baseline zero — safe, because GoldSrc-protocol demo playback positions
    entities from the server-authoritative network stream, not local
    usercmd-driven prediction. Two round-trip unit tests added
    (`usercmd::tests`); full `cargo test` and `cargo build --release` both
    pass clean.
15. **Confirmed fixed in a real browser (2026-08-14).** Both clips
    re-transcoded with the fix and repacked. `dodEmmanuelEarly` now shows a
    proper first-person view — rifle model, two soldiers visibly running
    through the courtyard, camera facing and turning correctly — **the full
    triage-viewer goal, working end-to-end for a section-start clip.**
    `dodEmmanuelClip` still shows the frozen/upside-down view from #12/#13,
    unaffected by this fix (different mechanism — confirmed the
    `CL_ParseDeltaPacketEntitiesGS` warning is still present), which is
    exactly what should happen: the camera fix and the delta-baseline gap
    are independent bugs. Also newly visible now that the camera moves and
    the console is legible: dozens of `CL_FireEvent: events/weapons/*.sc not
    precached` errors during firefights (bar.sc, mp44.sc, kar.sc, garand.sc,
    plus `events/misc/pain.sc`, `events/effects/bodydamage.sc`,
    `events/effects/helmet.sc`). **Not a bug in the fix just shipped** — a
    separate, pre-existing `pack`-tool gap: weapon-fire/pain client events
    are precached through a different GoldSrc channel than
    `svc_resourcelist`, which is all `pack`'s resource scraping currently
    reads. Net effect: gunfire/hits happen correctly (the demo data is
    real), but muzzle flash, shell ejection, and pain-flinch visual/audio
    effects are silently dropped. Not chased further this session — worth a
    dedicated look if the viewer's visual fidelity matters enough to justify
    walking event precache the same way `resources.rs` already walks
    `svc_resourcelist`.

---

## Step 4 findings: fixing mid-stream cuts (Open Risk #3) (2026-08-14)

Continuation of Step 3 #13's diagnostic. Goal: make a highlight cut from
*anywhere* in a match (not just section starts) play correctly. This turned
out to be two separate, structurally different bugs, because GoldSrc's
network protocol has *two* independent delta-compressed channels — world
entities and the local player's own state — that behave differently under
cutting. Both required reading the real `xash3d-fwgs` engine source
directly (shallow-cloned into scratchpad, same technique as Step 3's
`hud.cpp` trace) rather than guessing from the demo format alone.

### The wrong assumption this started from

The original TODO (in `lib.rs::cut`'s doc comment, and Open Risk #3's
original text) assumed GoldSrc periodically resends a full
`svc_packetentities` baseline, so "walk forward from the cut point to the
next one" would work. **Empirically false.** Diagnostic scan of
`analysis_target_pov.dem` (986 s): exactly 4 full `svc_packetentities`
messages in the *entire* recording, all inside the first 0.07 s after
connecting. All 90,464 other entity updates across the whole match are
`svc_deltapacketentities` — cumulative, chained against whatever the client
currently has, never periodically resynced. A forward walk finds nothing
for a real mid-match cut and silently degrades to the old broken behavior
(confirmed: output was byte-identical to the unfixed version when tested).

### The actual fix: reconstruct state, don't search for it

Since there's no full baseline to *find* after the very start, one has to
be *synthesized* — replay every entity delta from true t=0 up to the cut
point, accumulate the resulting per-entity field state, and re-encode it as
a new, self-contained full `svc_packetentities`, spliced in as an extra
frame immediately before the retained window.

This turned out to be far more tractable than it sounds, because of how
`dem-patch` (this workspace's vendored GoldSrc parser) represents a delta:
`pub type Delta = HashMap<String, ByteVec>` — already-*decoded* field
values (not raw wire bits), keyed by field name exactly as parsed from the
demo's own `svc_deltadescription` tables. Accumulating deltas over time is
therefore a plain `HashMap` overlay (later values win), and re-encoding
back to wire bytes reuses `dem-patch`'s own `SvcPacketEntities::write()` /
`write_delta()` verbatim — **no bit-packing was reimplemented**, only the
replay/accumulation logic and the struct construction around it.

Mechanically (`xash-transcode/src/lib.rs`):
- `cut()` now takes a second parameter, `parsed: &Demo` — the *same* source
  bytes, parsed a second time in `MessageDataParseMode::Parse` (the
  original `demo: &Demo` stays `Raw`, for the existing byte-exact write
  path). `main.rs`'s CLI loads both (`load()` + `load_parsed()`) and passes
  them through — this mirrors the existing `pack` command's pattern of
  paying for a full parse when needed.
- `replay_state_before(entry, before)` walks every `NetworkMessage` frame
  in `[0, before)`, merging `EngineMessage::SvcPacketEntities` and
  `SvcDeltaPacketEntities` entries into a running `BTreeMap<u16, (bool,
  Delta)>` (entity index → `has_custom_delta` + accumulated fields;
  `remove_entity` deletes from the map), *and* `SvcClientData` into a
  second running `Delta` (+ per-weapon-slot deltas). Full and delta entity
  messages are handled identically (both just contribute a `Delta` to
  merge) — correct here because the only full messages that exist are the
  initial connect-time snapshot, so there's no real case of a later full
  update needing to *reset* rather than merge.
- `encode_synthetic_payload(state, aux)` builds a `SvcPacketEntities`
  (absolute entity indices throughout, for simplicity — slightly bigger on
  the wire, never wrong) followed by a `SvcClientData`, and calls their
  existing `.write(aux)`. `aux` comes from `Demo::_aux` — a field
  `dem-patch` documents as "not part of a demo, do not use this," but there
  is no other API to obtain the decoder tables this *specific* demo's own
  `svc_deltadescription` messages established, and since those tables are
  set once near signon and read-only afterward, the aux state from a
  completed full parse is valid for this purpose.
- `synthetic_baseline_frame(anchor, payload)` wraps that payload as a new
  `dem::types::Frame` (`FrameData::NetworkMessage`), cloning the anchor
  (first real retained frame)'s own `DemoInfo`/`SequenceInfo` — reasonable
  since the synthesized snapshot logically belongs to the same instant.
  Inserted into the frame list immediately before the retained window.
  Skipped gracefully (falls through to the old plain-cut behavior for that
  section) if the encoded payload would exceed `idem::MAX_INIT_MSG`
  (32 KB) or no anchor frame exists — in practice, nowhere close: the real
  entity table for this match added under 500 bytes.

### Confirmed: the entity half works

Real-browser test of the regenerated `dodEmmanuelClip` (300–320s mid-stream
cut): **no `CL_ParseDeltaPacketEntitiesGS` warning**, kill feed scrolling
with real player names, `CL_FireEvent` firing for weapon sounds during a
firefight — all the signs of correctly-decoding entity state that were
present in the working section-start clip. This is real, working,
significant progress: the primary blocker for Open Risk #3 is solved.

### Not yet fixed: `svc_clientdata` (the local player's own state)

Symptom, confirmed twice across two test rounds: camera is a slowly
rotating, free-floating view positioned above the (correctly-rendered, per
the entity fix) player model — not a first-person view. Classic GoldSrc
observer/spectator-camera fallback behavior.

**Root cause, found by reading `engine/client/parse/cl_parse.c` and
`cl_parse_gs.c` directly** (not guessed): unlike entities, `svc_clientdata`
is *not* cumulative. `CL_ParseClientData` (`cl_parse.c:983`, called for
GoldSrc demos via `cl_parse_gs.c:609` with `PROTO_GOLDSRC`) reads, right
before the delta payload (`cl_parse.c:1102-1119`):
```c
if( MSG_ReadOneBit( msg )) {
    int delta_sequence = MSG_ReadByte( msg );
    from_cd = &cl.frames[delta_sequence & CL_UPDATE_MASK].clientdata;
} else {
    from_cd = &nullcd;  // zeroed baseline
}
```
`svc_clientdata` explicitly references *one specific prior frame* by a
sequence-number byte, looked up in a 64-entry ring buffer of frames the
client has actually processed — not "whatever I currently have," the way
entities work. The synthetic frame built above reuses the anchor's own
`sequence_info.incoming_sequence`, landing correctly in that one ring-buffer
slot — but the *real* clientdata message immediately following it (part of
the unmodified original stream) references whatever `delta_sequence` the
*original* recording expected, pointing at a frame from *before* the cut
that was never received. That ring-buffer slot is empty/stale, so the very
next real update corrupts `origin`/`health`/`deadflag`/observer-mode fields
right after the one correct synthetic frame — matching the observed symptom
exactly (position looks plausible-ish immediately, then falls into an
observer-camera state).

**The fix, scoped but not yet implemented:** `dem-patch`'s
`SvcClientData.has_delta_update_mask` / `.delta_update_mask` fields are
misleadingly named — despite the name, `delta_update_mask` (a `[bool; 8]`
`BitVec`) *is* the C engine's `delta_sequence` byte. So: find the first
`SvcClientData` message at/after the cut point in `parsed`. If
`has_delta_update_mask` is `false`, nothing to fix — it's already a
from-null full update. If `true`, decode `delta_update_mask` via
`BitSliceCast::to_u8()` and set the *synthetic* frame's
`sequence_info.incoming_sequence` to exactly that value (not the anchor's
own sequence number) before writing it — `delta_sequence & CL_UPDATE_MASK`
is satisfied by an exact numeric match regardless of the mask's actual
value. This only has to be right once: after that one correct resolution,
the real stream's subsequent clientdata messages chain against each other
normally, unmodified, exactly like entities already do. Small, targeted
change — no new encoder logic, just picking the right sequence number for
the synthetic frame already being built.

**Untested after this fix lands:** `weapon_data_t` (parsed in the same
`CL_ParseClientData` call, `cl_parse.c` ~1121-1132, reusing the same
`from_wd` ring-buffer frame selected by clientdata's `delta_sequence`) —
plausibly rides along correctly once clientdata's sequence is right, since
it shares the same frame selection, but not verified. Also unchecked:
`cl.predicted_frames`/`cls.spectator` branching (`cl_parse.c:1029-1058`) —
didn't investigate; flag if a new symptom appears post-fix that looks
prediction-related.

### Code state as of this handoff

`xash-transcode/src/lib.rs` and `xash-transcode/src/main.rs` carry this
work **uncommitted**. `cut()`'s signature changed
(`cut(demo, parsed, start, end, preroll, opts)` — one new parameter).
Builds clean, existing test suite passes (`cargo build --release` /
`cargo test --release` from inside `xash-transcode/`), structural
`validate()` passes on the regenerated `dodEmmanuelClip.dem` — the bug is
real-engine-behavior-level, not a Rust-side defect these checks would catch.
Superseded by Step 5 below — the clientdata half described here as "not yet
implemented" has since been implemented, byte-verified correct, and the
visible symptom persisted anyway.

---

## Step 5 findings: implementing the clientdata chain fix — implemented,
## verified correct, symptom unchanged (2026-08-14)

Continuation of Step 4. Goal: implement the scoped fix Step 4 identified
(set the synthetic frame's `sequence_info.incoming_sequence` to the real
stream's expected `delta_sequence`) and confirm the camera works.

### The fix, as originally scoped, was incomplete

Implemented Step 4's exact proposal first: one synthetic frame, its
`incoming_sequence` overridden to the byte the first post-cut
`svc_clientdata` message's `delta_update_mask` decodes to. Before trusting
it, added temporary instrumentation (`eprintln!` in `cut()`, removed
before this handoff) to dump the succession of `delta_update_mask` values
across the first dozen post-cut `svc_clientdata` messages in the real
demo. Result, against `analysis_target_pov.dem`'s 300s cut (anchor's own
absolute sequence: `165385`):

```
cd#0: frame.seq=165385  expected_delta_seq=Some(5)
cd#1: frame.seq=165386  expected_delta_seq=Some(6)
cd#2: frame.seq=165387  expected_delta_seq=Some(7)
cd#3: frame.seq=165388  expected_delta_seq=Some(8)
cd#4: frame.seq=165389  expected_delta_seq=Some(8)
cd#5: frame.seq=165390  expected_delta_seq=Some(9)   <- 165376+9=165385 = the anchor's own frame, real & retained
cd#6: frame.seq=165391  expected_delta_seq=Some(11)  <- 165376+11=165387 = cd#2's own frame, real & retained
...
```

(`CL_UPDATE_BACKUP`/`CL_UPDATE_MASK` — the ring-buffer modulus — is **64**
for any real match recording: `cl_game.c:999` sets it from
`MULTIPLAYER_BACKUP` whenever `maxclients > 1`, confirmed by re-checking
`netchan.h`; the `SINGLEPLAYER_BACKUP=16` definition next to it is a
dead end for this use case, not the active one.)

This is not "reference the immediately-preceding processed frame" (what
Step 4 assumed would hold after one correct resolution) — it's GoldSrc
deltaing clientdata against "the last frame this client has
acknowledged," a *sliding* window lagging the current frame by a roughly
constant few-packet round-trip delay. The first four-to-five post-cut
messages each reference a **different pre-cut frame in turn** (5, 6, 7,
8, 8 — note `8` repeated, referenced by two different messages), all of
them frames that don't exist in the retained stream and were never
patched by a single synthetic frame. Only once a message's own reference
"ages past" the cut boundary (cd#5 onward here) does it resolve to a real
retained frame that populates itself normally during ordinary playback.

### The actual fix: a burst of synthetic frames, not one

`client_data_chain(entry, at_or_after, anchor_seq)` in
`xash-transcode/src/lib.rs` walks the real post-cut stream and collects
every *distinct* pre-cut slot referenced by the leading messages, in
first-seen order, stopping as soon as a message's reference resolves to
`>= anchor_seq` (i.e. the chain has caught up and everything from here
self-heals). `cut()` now inserts one synthetic baseline frame per entry
in that chain — for this demo, 4 frames with `incoming_sequence` 5, 6, 7,
8 — each carrying the same reconstructed entity+clientdata payload,
immediately before the retained window. `synthetic_baseline_frame()`
gained a third parameter (`client_data_sequence: Option<u8>`) to drive
this per-frame override.

### Verified two independent ways, not just by re-reading the source

1. `client_data_chain()` invoked directly (via temporary
   instrumentation) on the real demo returns exactly `[5, 6, 7, 8]` —
   matching the manual trace above exactly.
2. **The actual written output bytes**, not pre-write in-memory state:
   wrote `xash-transcode/examples/verify_clip.rs` (kept, untracked — see
   "STOP AND READ" point 1), a from-scratch minimal IDEM-container walker
   (deliberately independent of `lib.rs`'s own writer, so it can't share a
   bug with it) that re-parses a transcoded output file and prints each
   `dem_read`/`dem_norewind` frame's `incoming_sequence` and payload
   header. Run against the regenerated `dodEmmanuelClip.dem`, the
   `Playback` entry's first four frames show `incoming_sequence` `5, 6, 7,
   8` (541-byte payload, identical first bytes — the same encoded
   entities+clientdata blob, as expected), then the real stream resumes
   at `165385`. The fix is genuinely present in the exact bytes the
   browser loads.

A dump of the reconstructed `state.client_data` field values (also
temporary instrumentation, removed) came back sane: `health` decodes to
`100.0`, `deadflag=0`, `iuser1=0` (not observing), plausible non-zero
origin/velocity/view_ofs. Not corrupted, not a reconstruction bug.

### And the camera is still broken, unchanged

Confirmed in a real browser, twice — once immediately after implementing
the chain fix, once again after fixing a dev-server caching bug (below)
to rule out the first test having silently reused stale cached content.
Same symptom both times: slowly-rotating free-floating camera, exactly as
before any of this session's work.

### A real, unrelated bug fixed along the way: dev-server.js caching + a path-safety bug

`../dod-web-demo-viewer/dev-server.js` set no `Cache-Control`, `ETag`, or
`Last-Modified` on any response, including the 90 MB asset zip — the
browser was free to keep reusing a stale cached copy across rebuilds
during iterative testing, and likely did so for at least one retest this
session. Fixed: `Cache-Control: no-store` added to every response.
Separately, while restarting the server, found its path-safety check is
broken for the documented `.`-as-root invocation:
`path.join(root, urlPath)` stays relative but gets compared against the
absolute `path.resolve(root)` via `.startsWith()` — always false, so
*every* request 403s. Not fixed in the file itself this session (only
worked around by always passing an absolute root path); see "STOP AND
READ" point 2 for the corrected invocation.

### New clue: a server MOTD reappears mid-playback, exactly correlated with brokenness

The user noticed, unprompted, a server MOTD box ("Nineteen Eleven 24/7
KTP…" — real ad content from whatever community server the source
recording came from) reappearing over the game view partway through
playback on the broken clip, and correlated its presence directly with
"the demo isn't working" — it never appears on the working section-start
clip. Checked: no `motd.txt` exists anywhere in the mounted content pack,
so this isn't a stray leftover file being read fresh; the text was almost
certainly already received once, near true t=0, during the `LOADING`
entry both clips share unmodified — something is **redisplaying** it
later, only on the clip that has synthetic frames.

That structural fact — *only* `dodEmmanuelClip` gets synthetic frames —
is exact and load-bearing: `dodEmmanuelEarly` is cut from `start=0`, so
`replay_state_before(pe, lo=0)` finds nothing before time zero and the
entire synthetic-baseline block in `cut()` never fires for it. Every
version of the fix tried this session (one frame, then four) only ever
touched `dodEmmanuelClip`. **Leading theory, not yet confirmed against
source:** injecting a full baseline (`svc_packetentities` +
`svc_clientdata` with no delta reference) mid-game — something that
structurally never happens in a real recording (Step 4: exactly 4 full
`svc_packetentities` exist in the entire 986s source, all within 0.07s of
connect) — may be read by the **client DLL** (not the engine, which is
all that's been checked so far across this session and last) as "you
just (re)connected," independent of whether the synthetic frame's
content and sequence numbers are correct. This would explain both
symptoms (MOTD redisplay, camera reset to a connect-like default) with
one cause, and would explain why getting the sequence number byte-exact
didn't help at all.

**Not yet done, next session's starting point:** clone
`https://github.com/FWGS/hlsdk-portable` (a different repo from
`xash3d-fwgs` — the client-DLL source, not the engine) and check what
`HUD_UpdateClientData`, `ServerActivate`, and MOTD-display logic key off
of — specifically whether a non-delta (full) `svc_clientdata` or
`svc_packetentities` arriving after the initial connect trips anything
connect-like. If confirmed, the fix stops being "get the sequence number
right" (already done) and becomes "don't send something that looks like
a full resync mid-game at all" — likely a materially different, more
involved approach than anything implemented so far.

### Why none of items 2–8 are `pack`-tool bugs (Step 3)

Every missing file above (`gfx.wad`, `delta.lst`, the fonts, `hud.txt` +
base sprites) is **engine/base-install-level**, not something any specific
map or demo precaches — `pack`'s whole design is "package what *this demo*
needs," scoped correctly. A *real* deployment of this viewer would ship
these once, adjacent to the per-map/per-demo packs `pack` produces, not
reinvent them per pack. Worth designing as: a small fixed "base kit" zip
(the base WADs + fonts + `delta.lst` + `hud.txt`/sprites, all pulled from
the same place a copy of Half-Life provides them) that the viewer always
mounts first, with the per-demo `pack` output layered on top. Not built
yet — the test pack just has everything flattened into one zip for
expedience.

### How to reproduce / continue this

See "STOP AND READ" at the very top of this document — it has the current
exact server-restart command, test URL, and what's being waited on.

---

## Step 6 findings: the MOTD/reconnect theory is refuted; the real bug was
## in the synthetic frames' `SequenceInfo`, not the client DLL (2026-09-17)

Continuation from a cold read of this doc (a fresh session, different repo —
picked this up via the KTP coordination effort, which hit the same `dem`
crate and demo-transcoding problem space from a different angle and found
this work while researching overlap). Two source-grounded findings, neither
requiring the actual demo/asset files — both read directly from `hlsdk-
portable` and `xash3d-fwgs` and then confirmed against this crate's own
code and real output bytes.

### The client-DLL-reconnect theory (Step 5's leading theory) is dead

Read `hlsdk-portable/cl_dll/MOTD.cpp` and `hud.cpp`. `CHudMOTD::m_bShow` —
the flag `Draw()` actually checks before rendering the MOTD box — is set
**only** inside `__MsgFunc_MOTD` (`hud.cpp:208-222`), the handler for the
dedicated `MOTD` server usermessage. There is no code path anywhere in the
client DLL that ties MOTD display to `svc_clientdata`, `svc_packetentities`,
or any notion of "just reconnected." Whatever caused the MOTD to reappear on
`dodEmmanuelClip`, it was not the client DLL reading a synthetic full
baseline as a reconnect signal. (Not chased further: most likely the demo's
own original recording had the server rebroadcast its MOTD a second time
within the retained window — plenty of pub servers do this periodically —
which would show up in the real, unmodified `svc_usermsg` stream regardless
of any synthetic frames; correlate against a demo with zero synthetic frames
*and* a cut window that doesn't happen to span a second broadcast, or just
grep the source recording for a second `MOTD` usermessage, to confirm.)

### The actual bug: `synthetic_baseline_frame()` cloned the anchor's entire `SequenceInfo`

`client_data_sequence` (Step 5) correctly overrides `incoming_sequence` per
synthetic frame — that fix is real and was correctly verified. But every
other `SequenceInfo` field, **including `outgoing_sequence`**, was cloned
verbatim from the anchor for *every* frame in the burst. That field isn't
cosmetic:

- The transcoder writes `outgoing_sequence` into each `NetworkMessage`'s
  paired synthesized `dem_usercmd` frame, as *both* the sequence and
  cmdnumber fields (`lib.rs`, the `dem_usercmd`-emission block in the main
  transcode loop).
- `CL_ReadDemoUserCmd` (`xash3d-fwgs/engine/client/cl_demo.c:621,648`) uses
  that value to index `cl.commands[(outgoing_sequence + 1) &
  CL_UPDATE_MASK]` and pins `cls.netchan.outgoing_sequence` to it.
- `CL_PredictMovement` (`cl_pmove.c:1036`) predicts the local player forward
  while `incoming_acknowledged + i < outgoing_sequence + stoppoint`.

Several synthetic frames sharing one `outgoing_sequence` means several
`dem_usercmd` writes collide into a single `cl.commands[]` slot instead of
distinct ones, and `outgoing_sequence` itself is frozen for the whole burst
— exactly the kind of thing that doesn't happen in a real recording (every
real packet has a strictly increasing sequence number), and structurally
identical to the bug Step 5 already found and fixed in the *clientdata* ring
buffer, just one layer over in the *prediction* ring buffer, which nobody
had looked at yet.

**The fix:** `synthetic_baseline_frame()` gained a `sequence_backdate: i32`
parameter. Each frame in the burst now gets `outgoing_sequence` and
`incoming_acknowledged` moved back by its distance from the anchor (the
frame furthest from real time gets the largest backdate, the one right
before the anchor gets `1`), instead of all of them cloning the anchor's
single snapshot. `incoming_sequence` keeps being overridden separately by
`client_data_sequence`, unchanged from Step 5.

**Verified against real output bytes** (`primer.dem`, a 300–320s cut, same
methodology Step 5 used — not just reasoning about the source): a new
throwaway diagnostic, `examples/verify_usercmd_seq.rs`, walks the written
IDEM file's `dem_usercmd` frames and prints `outgoing_sequence`/`cmdnumber`
next to the following `NetworkMessage`'s `incoming_sequence`. Before this
fix, the leading synthetic frames' `outgoing_sequence` was identical
(cloned from the anchor). After: `310913, 310914, 310915` — three distinct,
consecutive values, the last one landing exactly on the real anchor frame's
own real `outgoing_sequence`. `cargo build --release` and `cargo test
--release` both pass clean (all 7 existing tests, unaffected — none of them
exercised this path). `validate()` still passes on the regenerated cut.

**Not yet re-tested in a real browser** — this session had no access to the
real demo/asset files or a running Xash environment, only the source repos
and `primer.dem` (already checked into this repo). Whoever picks this up
next with a real browser: retest `dodEmmanuelClip` (or any mid-stream cut)
end to end. If the camera is fixed, Open Risk #3 is fully closed. If it
isn't, the MOTD refutation above still stands — don't go back to the
client-DLL theory — and the next thing to check is probably
`cl.predicted_frames[]`'s own seeding (`CL_PredictMovement`, `from =
&cl.predicted_frames[last_predicted]` in `cl_parse.c`), which this session
didn't get to.

---

## Step 7 findings: mid-stream cuts play — two more transcoder bugs, real
## engine run against a real install (2026-09-19)

Retested against a real Steam Half-Life+DoD install, using a local no-zip
loader (`dod-web-demo-viewer` branch `wip/local-dynamic-fs-test-harness`)
and `primer.dem` (already in this repo), 300–320s cut. Confirmed
end-to-end in a real engine run: mid-stream cut → `client connected` →
first-person view with weapon viewmodel → kill feed with real names →
weapon/pain events firing → `CL_DemoCompleted` after exactly the clip's
20s → menu. **Zero `CL_ParseDeltaPacketEntitiesGS` warnings.** Verified by
comparing two headless frames 10s apart (different location, viewmodel
visible) and by the demo actually *completing* — a 46-second run before
these fixes never completed a 20-second clip.

The Step 6 `outgoing_sequence` fix was real but not the thing on screen.
Two more, both in `cut()`/`encode_synthetic_payload()`:

1. **The retained window was never rebased.** `cut()` keeps the section's
   original `DemoStart` (→ `dem_jumptime` at t=0) as the first frame, and
   `transcode_entries` rebases against the entry's *first* frame — that
   DemoStart — so retained frames kept their original t≈300 stamps.
   `CL_DemoReadMessage` (`cl_demo.c:963`) gates every frame on `timestamp
   ≥ elapsed`, hands out only the first one unconditionally, then **waits
   `lo` real seconds** for the clock to catch up. A 300s cut froze on its
   first frame for five minutes. This is the whole "section-start works,
   mid-stream doesn't" split (lo=0 has no gap) — and it's the floating/
   slowly-rotating camera too: `CL_DemoInterpolateAngles` slerps between
   the last LOADING-section angle sample at t≈0.3 and the first gameplay
   sample at t≈300 as the clock crawls across the gap. Verified in the
   written bytes (`examples/dump_times.rs`): `jumptime 0.000` then
   `read 300.003` before, `read 0.003` after. Fix: subtract `lo` from every
   retained frame right after the filter; `NextSection` pinned to
   `end - lo`; synthetic-frame insertion now anchors on the first
   `NetworkMessage` rather than `time >= lo`.

2. **Synthetic payload message order.** It emitted `svc_packetentities`
   *then* `svc_clientdata`. The engine only picks the ring slot the
   *current* frame's entities go into inside `CL_ParseClientData`
   (`cl_parse.c:1020`, `cl.parsecountmod = incoming_sequence & MASK`), and
   `CL_ParsePacketEntitiesGS` writes into `cl.frames[cl.parsecountmod]`
   (`cl_parse_gs.c:298`). So the 30-entity reconstruction landed in the
   *previous* frame's slot, then clientdata moved the pointer to a slot
   still holding a kept LOADING-section connect-time frame with 7
   entities — hence `(7 should be 30)` on each leading post-cut delta.
   Real GoldSrc packets are `svc_time, svc_clientdata, svc_packetentities`
   for this reason. Fix: clientdata first. (`replay_state_before` itself
   was right — instrumented, 30 entities.)

Also: `client_data_chain` → `delta_reference_chain`, now walking
`svc_deltapacketentities`' own `delta_sequence` too (same ring-buffer
lookup, `cl_parse_gs.c:306-307`), stopping only once both kinds have caught
up. Didn't change the chain for this demo but there's no reason the two
references must agree.

Viewer-side, in the local harness (not this crate): `+cl_allow_download 0`
is required — one custom map model missing from the install made the
engine try the recording server's `sv_downloadurl`, which can't work here
(DNS needs a thread; SDL: "Threads are not supported on this platform") and
retries forever, so signon never completes. And `FS.createLazyFile` does
not work in this single-threaded build (sync XHR blocked on the main
thread) — eager `fetch()`+`writeFile` per file instead, still no zip.

Still open: DoD HUD (unchanged), weapon-fire client events report "not
precached" even with the `.sc` files packed (Step 3 #15 — a different
precache channel, cosmetic), and a real-GPU-browser eyeball of the camera
by a human, which was in progress when this was written.

---

## Open risks

1. ~~**`PROTO_GOLDSRC` may never have been exercised for demo playback.**~~ **Resolved 2026-08-14 — it works.** Confirmed in a real browser: the engine signs onto a transcoded demo's server data, loads the real map and all player/weapon models, and renders the level. See "Step 3 findings" #12.
2. **No DoD client library exists for wasm.** Valve never released DoD's source, so it can't be compiled — it would have to be written. See below.
3. **Delta compression on cuts — entity half fixed and confirmed; clientdata half's known bug is fixed and byte-verified correct, but the visible symptom is unchanged, and the actual root cause is now believed to be different from what was fixed.** `svc_deltapacketentities`/`svc_clientdata` encode cumulatively against earlier frames, so a cut landing mid-stream shows corrupt state until reconstructed. **Entity half fixed and confirmed 2026-08-14**: `cut()` synthesizes a full `svc_packetentities` baseline by replaying every entity delta from t=0 to the cut point (see "Step 4 findings") — real-browser test shows no more `CL_ParseDeltaPacketEntitiesGS`, kill feed works, players render and move. **Local-player half (`svc_clientdata`):** the ring-buffer sequence-number bug Step 4 root-caused is now fixed (`client_data_chain()`, "Step 5 findings") — not one synthetic frame but a *burst* of them, since the real reference is a sliding few-frame lag, not a fixed point — and independently byte-verified correct against the actual written output file. **Update 2026-09-17 (Step 6): the client-DLL-reconnect theory is refuted** (confirmed against real `hlsdk-portable` source — MOTD display has no code path connected to clientdata/packetentities). **The real bug was a third ring buffer nobody had looked at**: `synthetic_baseline_frame()` cloned the anchor's `outgoing_sequence` for every frame in the burst, which the engine uses to index its movement-prediction ring buffer (`cl.commands[]`) — several identical values collide into one slot and freeze prediction for the whole synthetic window. Fixed (each frame now gets a distinct, backdated `outgoing_sequence`) and byte-verified against real output (see "Step 6 findings"). **Not yet retested in a real browser** — no demo/asset files or Xash environment available this session.
4. **Custom content mismatch.** DoD's custom-model culture means demos reference files a given install may lack or have different versions of. `pack` reports size mismatches against server-declared sizes to surface this.

---

## DoD client library — assessment

**The blocker is not compilation, it's that the source doesn't exist publicly.** DoD is not in the HLSDK. "Compile it" isn't the task; "write it" is.

For triage-only viewing, far less is needed than a full client:

- **World (BSP v30)** — engine-side. Any client works.
- **Player models** — engine-side studio renderer, driven by entity state from the demo stream. Any client works.
- **POV camera** — comes from the recorded stream; stock `V_CalcRefdef` should be close.
- **HUD** — the only part genuinely needing DoD code.

`hlsdk-portable`'s client wasm is already published on npm, so testing "does DoD render at all with a foreign client" is cheap. Expect world + players + movement and no meaningful HUD — a silent film, which for triage may be sufficient.

If a minimal DoD HUD is later wanted, note that **the wire formats are already reverse-engineered in this repo** — `dod/src` has the message parsers and `analysis/` decodes scoreboards, kills and chat. Writing DoD usermessage handlers in C++ is transcription from existing Rust, not discovery. Health/ammo/class is plausibly weeks. Full parity is not worth attempting.

---

## Next steps, in order

1. ~~`cargo build` the crate and fix compile errors.~~ **Done (2026-08-13) — built clean, no errors.** CLI validated against `analysis_target_pov.dem`; output matches this doc's measured sizes and is byte-identical to the checked-in fixture for the 300–320s cut (see verification table above). A true byte-for-byte diff against the Python oracles is still outstanding — needs a machine with a real Python interpreter (not the Windows Store stub).
2. ~~Build a content pack for one map.~~ **Done (2026-08-13).** `dod_custom_pack.zip` written to `../dod-web-demo-viewer/assets/` (gitignored there already) from `analysis_target_pov.dem` against the real `dod` install — 262 files, 53.5 MB zipped, all required files found after fixing the two bugs above. Only open item: the 28 mismatched viewmodels noted in the verification table — decide whether the *viewer's* content pack should prefer a vanilla viewmodel set over whatever install happens to build the pack, so played-back demos don't quietly pick up a movie-mod's oversized models.
3. ~~Confirm the level actually renders and is playable.~~ **Done (2026-08-14).** See "Step 3 findings" #12–15 for the full trail: a long chain of missing base-engine files (not transcoder bugs), then a genuine transcoder bug (missing `dem_usercmd`/view-angle frames, fixed), got a section-start clip to a fully working real-browser playthrough — map, players, weapons, and a correctly turning camera. `PROTO_GOLDSRC` demo playback is real and works.
4. **Entity half done. Clientdata ring-buffer sequence fix done and byte-verified — but the actual blocker turned out to be something else, still open.** Mid-stream cuts (real highlight clips) needed reconstructed entity *and* local-player state, not just a preroll — see "Step 4 findings". Entity half confirmed working in a real browser. Local-player half (`svc_clientdata`): the sequence-number bug Step 4 root-caused is now fixed and independently verified correct against the actual output bytes (Step 5) — and the camera symptom is **unchanged**. Leading theory (Step 5, not yet confirmed): a full baseline injected mid-game may be read by the *client DLL* as a reconnect event regardless of its content being correct, evidenced by a server MOTD reappearing exactly on the clip that has synthetic frames. **The single next step** is reading `hlsdk-portable`'s client-DLL source (not the engine — a different repo) to confirm or rule this out; see "STOP AND READ" point 3. Current code (entity fix + clientdata chain fix) is uncommitted in `xash-transcode/src/lib.rs`/`main.rs`. Secondary, lower-priority gap found along the way: weapon-fire/pain client *events* (muzzle flash, shell eject, pain flinch) aren't currently packed — `pack`'s resourcelist walk covers `svc_resourcelist` but not event precaching (Step 3 finding #15).
5. **Once #4 is fully resolved:** commit, retest `dodEmmanuelClip` end-to-end, then decide on a minimal DoD HUD (see "DoD client library — assessment" below) and design the "base kit" pack split noted after Step 3 findings (base WADs/fonts/delta.lst/hud sprites shipped once, not flattened into every per-demo pack). If the client-DLL theory pans out and the fix turns out to be substantially more involved than expected, revisit Fallback A below rather than sinking more time into a redesign — the goal is a working triage viewer, not a from-scratch demo-splicing engine.

### Fallback if 3D playback proves unworkable

- **Fallback A (cheapest, solves the actual user pain):** keep `hl.exe`, but pre-cut each highlight into its own short `.dem` so "Preview Highlight" launches straight into the clip. The events window disappears from the user's workflow entirely. Uses only the patcher and writer already in this repo. Same delta-compression caveat as risk 3.
- **Fallback B:** 2D replay viewer on the map overview — player dots, killfeed, scrubber. Every position and event is already in `analysis`. Click-to-jump becomes an array index, so seeking is instant and exact. Runs in Tauri and on Pages from one codebase. Rated a worse viewing experience by the project owner, so treat as a supplement rather than the primary.

---

## Conventions for whoever picks this up

- Keep `lib.rs`, `idem.rs`, `resources.rs`, `writer.rs` **free of I/O** — the same transcoder is intended to run in the browser viewer and in Tauri. All filesystem work belongs in `main.rs` / `packer.rs`.
- Keep `reference/*.py` in sync. If Rust and Python disagree for the same input, one has drifted, and the Python is the one that was validated against real demos.
- Don't commit game content or transcoded demos — Valve assets. Both repos' `.gitignore` files already cover this.
