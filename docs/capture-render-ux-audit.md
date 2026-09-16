# Capture / Render / Movie-Making UX Audit

Scope: the capture → patch → render → movie-making pipeline only (Capture Studio,
Render Studio, and the native `capture_engine`/`patch`/`hlcr` code that drives
them). Read-only audit — no source files were changed. All line numbers are
current as of `audit/capture-render-ux` (branched from `feature/capture-block-manifest`).

---

**Status (2026-08-24):** this is a point-in-time audit with no completion markers of its own — line numbers below are historical. Checked against what's actually shipped:

**Section 1 — Friction Points**
| # | Finding | Status |
|---|---|---|
| 1 | Capture progress is a black box | Open — tracked as [issue #36](https://github.com/ccoventry/dod-tools/issues/36) |
| 2 | Three disagreeing disk-space gates | ✅ Fixed 2026-08-23 (`fix/capture-disk-space-gates`) |
| 3 | Settings written to disk on every keystroke | ✅ Fixed — `sprint_history_archive.md` §4 |
| 4 | Export-dir fallback computed three times | ✅ Fixed — `sprint_history_archive.md` §3 |
| 5 | `export_manager.rs` is dead code | ✅ Fixed — `sprint_history_archive.md` §2 |
| 6 | hl.exe liveness polling sleeps 500ms | Open — not yet tracked as an issue |
| 7 | Render concurrency isn't hardware-aware | ✅ Fixed — `sprint_history_archive.md` §5 |
| 8 | Render Studio folder scan has no incremental/progress path | ✅ Fixed — `sprint_history_archive.md` §6 |
| 9 | Manual "Rendered" status bypasses the verified-take pipeline | Open — not yet tracked as an issue |

**Section 2 — Feature Ideas**
| # | Idea | Status |
|---|---|---|
| 1 | "Reveal in Explorer" on render jobs/takes | ✅ Fixed — `sprint_history_archive.md` §1 |
| 2 | Burn kill-timeline label into rendered clip | Open — not yet tracked as an issue |
| 3 | Concatenate selected rendered clips into one movie | Open — not yet tracked as an issue |
| 4 | Live capture progress with current-clip name and ETA | Partially covered by [issue #36](https://github.com/ccoventry/dod-tools/issues/36) (progress/current-clip; ETA not explicitly scoped there) |
| 5 | Named render presets | Open — not yet tracked as an issue |
| 6 | Watch-folder auto-import for new demos | Open — not yet tracked as an issue |
| 7 | Export a highlight/take manifest (EDL-style) | Open — not yet tracked as an issue |
| 8 | Contact-sheet/scrub preview of a take | Open — not yet tracked as an issue |

---

## SECTION 1 — Friction Points

### 1. Capture progress is a black box, even though the plumbing to fix it already exists

**(a) What's clunky today.** During a batch capture — which can run for many
minutes across dozens of chained highlights — the UI shows a single frozen
progress bar. `capture_pane.js`'s `capture_status` listener can only draw a
real percentage if the event payload carries `index`/`total`; when it doesn't
(which is always, for the live HLAE run) it just paints the bar to a static
**50%** and leaves it there until the batch ends:

```js
// desktop-studio/src/capture_pane.js:517-524
if (payload.index !== undefined && payload.total && payload.total > 0) {
  const pct = Math.min(100, Math.round((payload.index / payload.total) * 100));
  progressBar.style.width = `${pct}%`;
} else {
  progressBar.style.width = '50%';
}
```

There is no current-clip name, no ETA, no per-block indicator — just "Capturing..."
for the whole run.

**(b) Root cause.** `native/src/capture_engine.rs`'s `EngineEvent` enum only has
`Starting`, `Launching`, `Finished`, `Error`, `AllCompleted`, `Cancelled`
(`capture_engine.rs:11-20`) — no progress variant. The poll loop that watches
`hl.exe` (`capture_engine.rs:477-550`) never emits anything between `Launching`
and `Finished`.

The frustrating part: **the data to drive real progress is already being
generated and thrown away.** `build_batch_queue` injects a breadcrumb echo
command every `BREADCRUMB_INTERVAL_TICKS` (5000) ticks per demo specifically
for this purpose:

```rust
// native/src/patch/builder.rs:742-751
// Implement Global Breadcrumb Loop
let mut step = 0;
while step < total_demo_frames {
    scheduled_commands.push((step, format!("echo \"[dod-tools] BREADCRUMB - Tick {}\"", step)));
    step += crate::patch::BREADCRUMB_INTERVAL_TICKS;
}
```

With `-condebug`, which `build_hlae_process` passes on every launch
(`native/src/patch/types.rs`), GoldSrc echoes these into `dod/qconsole.log`.
`capture_engine.rs` deletes that file before the run (`capture_engine.rs:86`,
`builder.rs:854`) but never tails it — there's even a standalone debug binary
(`native/src/bin/check_ticks.rs`) that already knows how to parse `BREADCRUMB`
lines out of a log, but it's a dev diagnostic, not part of the live batch.

**(c) Proposed fix.** Add an `EngineEvent::Progress { current_tick, block_index,
block_total }` variant, and inside the existing poll loop (which already wakes
every 500ms) do a cheap tail-read of `qconsole.log` for the latest `BREADCRUMB`/
`chain_NN` line, map it back to overall batch position, and emit it. Frontend
side, replace the static-50% branch in `capture_pane.js:517-524` with a real
percentage and a "current clip" label.

**(d) Risk vs. guardrails.** Low. This is purely additive — a new read-only log
tail and a new event variant — and doesn't touch frame injection, the 64-byte
Cbuf limit, or `DemoStart`/`ConsoleCommand` ordering at all. The poll loop
already satisfies the non-blocking-with-cancellation-check shape; just make
sure the log read stays a small bounded read (last few KB), not a full-file
re-parse each tick, so it doesn't turn the 500ms tick into a slow one.

**(e) UI-only or engine?** Both — new engine event + JS consumer.

---

### 2. Three independent, disagreeing disk-space gates

**(a) What's clunky today.** A batch can pass one space check and still fail
another, or vice versa, because there are three separate implementations of
"is there enough room":

1. **JS pre-flight** — `computeRequiredCaptureBytes` in
   `desktop-studio/src/capture_pane.js:63-121` re-implements the pre/post-roll
   merge-window billing logic in JavaScript to decide whether to disable the
   Start button.
2. **Rust AOT allocation** — `allocate_blocks_first_fit_decreasing` +
   `block_estimates` in `native/src/patch/builder.rs:500-520`, which snapshots
   free space *once* per drive at batch-build time and routes each block.
3. **Rust pre-launch abort** — `capture_engine.rs:393-412` re-queries disk
   space with `sysinfo` right before spawning HLAE and hard-aborts if the
   *primary* export dir alone has under 15 GiB free, regardless of what the
   AOT simulation already decided about the whole pool.

None of the three share code, and only #3 re-checks space at the moment it
actually matters (immediately before HLAE launches). A user can watch the
Start button unlock (JS check passes), have `build_batch_queue` successfully
route every block (#2 passes, using stale free-space numbers from whenever the
last scan happened), and still get a hard abort seconds later from #3 — or the
reverse, where #3's single-drive check passes but #2 silently ran out of room
on a secondary drive and errored out mid-build.

**(b) Root cause.** `computeRequiredCaptureBytes` (JS) and the
`blocks_merge`/`block_estimates` logic (Rust, `builder.rs:176-178`, `500-510`)
are two hand-written re-implementations of the same "merge overlapping
highlight windows, then bill bytes" algorithm, in two languages, with no shared
source of truth. `capture_engine.rs`'s 15 GiB check (`FAILOVER_THRESHOLD` in
`builder.rs:303` is a *different* constant, also 15 GiB, but the two are
independent literals, not a shared const) duplicates a chunk of what
`build_batch_queue` already decided moments earlier.

**(c) Proposed fix.** Short of unifying the merge-billing algorithm across
languages (real work, but the JS estimate is only ever a UX guard so it doesn't
strictly need bit-for-bit parity with the Rust allocator), the cheap fix is to
delete `capture_engine.rs`'s standalone 15 GiB primary-dir check and instead
have `build_batch_queue` return its final per-drive headroom so `spawn_capture_engine`
re-validates the *same* numbers `build_batch_queue` already computed, rather
than recomputing a third, narrower answer.

**(d) Risk vs. guardrails.** Low — this is disk-space bookkeeping, not frame
injection or process polling; no CLAUDE.md guardrail is directly implicated. Risk is confined to
getting the merge-window arithmetic wrong (already a two-implementation
problem today).

**(e) UI-only or engine?** Engine (`native/src/capture_engine.rs`,
`native/src/patch/builder.rs`), plus the JS estimate ideally keyed off the same
constants.

---

### 3. Settings are written to disk on every keystroke, including free-text console commands

**(a) What's clunky today.** Typing a value into almost any Batch Capture
Config field — resolution width, pre-roll seconds, and notably the Custom
Commands / Init Commands text boxes where a user is composing a raw engine
command — fires a full settings save on every character.

**(b) Root cause.** `list_editor.js` (which backs Init Commands and Custom
Commands) attaches its text-field listener to `'input'`, which fires per
keystroke:

```js
// desktop-studio/src/list_editor.js:75-79
input.addEventListener('input', (e) => {
  const v = field.type === 'number' ? (parseFloat(e.target.value) || 0) : e.target.value;
  setFieldValue(items, idx, field, v);
  notify();   // -> onChange -> notifySettingsChange() -> persistAppSettings()
});
```

`notify()` is wired straight to `capture_pane.js`'s `notifySettingsChange()`
(`capture_pane.js:212-214, 449, 459`), which calls `main.js`'s
`persistAppSettings()` (`main.js:183`), which does `await saveSettings(settingsPayload)`
— a Tauri IPC round-trip that serializes the *entire* settings blob and writes
it to disk — with no debounce. The same fan-out applies to the numeric Timing
Options fields, wired the same way at `capture_pane.js:488-493`. This is a
known anti-pattern the codebase has already fixed once, deliberately, elsewhere:
`detail_pane.js`'s notes field switched from `'input'` to `'change'`
specifically to avoid a save-storm per keystroke (see the comment at
`detail_pane.js:493-499`) — that fix just never made it back into the shared
`list_editor.js` widget the Commands lists use.

**(c) Proposed fix.** Debounce `persistAppSettings()` (e.g. 400ms trailing) in
`main.js`, or switch `list_editor.js`'s text/number inputs to `'change'` like
`detail_pane.js` already does for notes. The debounce is the safer of the two —
it doesn't change when values are visible in the DOM, only when they hit disk.

**(d) Risk vs. guardrails.** Low, UI-only, no guardrail implicated. Purely a
responsiveness/IO-volume issue.

**(e) UI-only or engine?** UI-only (`desktop-studio/src/list_editor.js`,
`desktop-studio/src/main.js`).

---

### 4. `capture_engine.rs` computes the same fallback export directory three separate times

**(a) What's clunky today.** Not user-visible today, but a real maintenance
trap: the exact same "use `primary_media_dir`, or fall back to the exe's own
directory" computation is duplicated three times in one function.

**(b) Root cause.**

```rust
// native/src/capture_engine.rs:198-201  (used for session_dir)
// native/src/capture_engine.rs:388-391  (used for the disk-space check)
// native/src/capture_engine.rs:417-420  (used for the dummy_path)
let active_export_dir = config.primary_media_dir.clone().unwrap_or_else(|| {
    let exe_path = std::env::current_exe().expect("Failed to resolve absolute exe path");
    exe_path.parent().expect("Exe has no parent directory").to_path_buf()
});
```

Three separately-named local bindings (`active_export_dir` twice,
`primary_dir` once) that must always agree, with nothing enforcing that. If a
future edit changes the fallback logic in one spot (e.g. to make it
configurable), the other two silently keep the old behavior.

**(c) Proposed fix.** Compute once near the top of `spawn_capture_engine` and
reuse the single binding for the session dir, the disk-space check, and the
dummy-path/marker cleanup.

**(d) Risk vs. guardrails.** Very low — pure refactor, no behavior change, no
guardrail implicated.

**(e) UI-only or engine?** Engine (`native/src/capture_engine.rs`).

---

### 5. `native/src/views/export_manager.rs` is dead code that doesn't even compile into the app

**(a) What's clunky today.** This file — explicitly in this audit's scope —
is a leftover pre-Tauri egui panel. It references `crate::Gui`,
`crate::views::capture::get_patcher_config()`, and `egui::Context`, none of
which exist anywhere else reachable from `native/src/lib.rs`. It still has a
literal `// TODO: Wire cancel dispatch` and a hardcoded placeholder label
(`"Queue Status: Idle | Active Renders: 0/2"`, `export_manager.rs:7`) that was
clearly never finished.

**(b) Root cause.** `native/src/lib.rs` declares `patch`, `hlcr`, `sys`,
`utils`, `shared`, `capture_engine` as modules — there is no `pub mod views`
anywhere in the crate (confirmed by grep across `native/`). The file is
orphaned: it isn't part of the compiled crate at all, just source sitting in
the tree from before the Tauri migration described in CLAUDE.md.

**(c) Proposed fix.** Delete `native/src/views/export_manager.rs` (and the
`native/src/views/` directory if nothing else lives there), or if it's being
kept intentionally as reference material, move it out of `native/src/` into
`docs/` or an `archive/` folder so it stops looking like live, in-scope engine
code to anyone (including future audits) grepping the crate.

**(d) Risk vs. guardrails.** None — it's unreachable code, deleting it changes
nothing about the compiled binary.

**(e) UI-only or engine?** Engine crate housekeeping (`native/src/views/`).

---

### 6. hl.exe liveness polling sleeps 500ms, well past the guardrail's ~16ms

**(a) What's clunky today.** Cancelling a running capture batch can take up to
half a second to register, because the watch loop only checks the cancel token
once per sleep cycle.

**(b) Root cause.**

```rust
// native/src/capture_engine.rs:549
std::thread::sleep(std::time::Duration::from_millis(500));
```

CLAUDE.md's guardrail for exactly this kind of loop ("External processes...
must use non-blocking polling (`child.try_wait()`) matched with a ~16ms sleep. Verify
an `Arc<AtomicBool>` cancellation token every cycle") is written for this
pattern, and this loop is 30x slower than that. In practice this isn't a
correctness bug — `child.try_wait()` is still non-blocking and the cancel
token is still checked every cycle — just a coarser cycle than the guardrail
specifies, likely chosen because `sysinfo::refresh_processes()` (called every
cycle to check whether `hl.exe` is alive) is comparatively expensive to run at
60Hz.

**(c) Proposed fix.** Split the two concerns: check `cancel_token` and
`child.try_wait()` every ~16ms as the guardrail specifies, but only run the
more expensive `sysinfo` process-list refresh on a slower internal cadence
(e.g. every 500ms, gated by an elapsed-time check inside the fast loop) — same
pattern already used for the disk-space cache in `native/src/sys/disk.rs:5-7`
(`TTL_MS`-gated `sysinfo` refresh).

**(d) Risk vs. guardrails.** Low to fix, and this is the one finding in this
audit that's a direct, named deviation from a CLAUDE.md guardrail — flagged
mainly for that reason, even though the real-world UX impact (half-second
cancel latency) is minor.

**(e) UI-only or engine?** Engine (`native/src/capture_engine.rs`).

---

### 7. Render concurrency isn't hardware-aware, and NVENC sessions can silently starve or fail

**(a) What's clunky today.** Render Studio lets the user pick 1–8 concurrent
render jobs with no guardrail tied to actual hardware:

```js
// desktop-studio/src/render_pane.js:302
const maxConcurrentVal = Math.min(8, Math.max(1, parseInt(...) || 2));
```

For software x264/ProRes/DNxHR this just divides CPU threads more thinly per
job (`renderer.rs:151-156`); for `RenderCodec::NvencH264` it can silently hit
the encoder's own concurrent-session cap (unlocked on Quadro/RTX 40-series+,
but capped — commonly at 3 or 5 sessions — on many consumer GeForce cards),
which surfaces as an opaque ffmpeg error deep in `error_log`
(`renderer.rs:338-346`) rather than an upfront warning.

**(b) Root cause.** `run_render_job` in `native/src/hlcr/renderer.rs:152-156`
computes `threads_per_process` purely from `std::thread::available_parallelism()
/ max_concurrent`, with no branch on `config.target_codec` at all.

**(c) Proposed fix.** At minimum, surface a warning in Render Studio when
`target_codec == NvencH264` and `max_concurrent_renders > 3` ("your GPU may
reject some of these sessions — check `nvidia-smi` if renders fail with an
encoder error"). A fuller fix would probe the NVENC session limit at startup
and clamp the UI's max automatically, but that's meaningfully more work.

**(d) Risk vs. guardrails.** Low — this is a UX/error-clarity issue, not a
process-lifecycle or memory-safety one; `renderer.rs`'s own `child.kill_on_drop(true)`
and cancellation handling are already correct.

**(e) UI-only or engine?** Mostly UI (a warning), optionally engine
(`native/src/hlcr/renderer.rs`) if auto-detection is added later.

---

### 8. Render Studio's folder scan has no incremental/cached path and gives no progress

**(a) What's clunky today.** Clicking "Scan Render Directories" always does a
full `WalkDir` of every configured folder from scratch — on a large multi-session
capture drive (which can easily be hundreds of take folders across many GB),
this can take a visible pause with nothing but a static "Scanning render
directories..." toast (`render_pane.js:262`) — no count, no percentage, no
indication of which folder it's currently in.

**(b) Root cause.** `scan_folder_background` in `native/src/hlcr/scanner.rs:91-226`
walks every configured folder unconditionally every call, with no persisted
cache of previously-seen takes and no incremental status beyond the
`status_tx.send(format!("Found take: {}", ...))` messages, which the current
frontend (`render_pane.js`) doesn't even subscribe to — `initRenderUI`'s
`scanRenderBtn` handler only awaits the final result, it never listens for
per-take scan status at all.

**(c) Proposed fix.** Wire the already-existing `status_tx` stream (it's being
generated and discarded) into a live "Scanning… found N takes so far" status
line the way `capture_status` already drives the capture progress bar. For the
cache side, keying on take-folder mtime would let a re-scan skip folders that
haven't changed since the last scan.

**(d) Risk vs. guardrails.** Low — pure I/O and UI plumbing, no guardrail
implicated.

**(e) UI-only or engine?** Both — the backend event stream already exists
(`status_tx` in `hlcr/scanner.rs`), it's the frontend wiring that's missing.

---

### 9. Manually setting a highlight's status to "Rendered" bypasses the entire verified-take pipeline with no visible distinction

**(a) What's clunky today.** The Highlight Details status dropdown lets a user
pick `Captured` or `Rendered` directly, with no confirmation and no visual
difference from a status the pipeline actually verified on disk:

```js
// desktop-studio/src/detail_pane.js:482-487
statusSelect.addEventListener('change', (e) => {
  streak.status = e.target.value;
  statusSelect.style.color = statusColors[e.target.value] || '#888';
  if (currentOnSelectionChange) currentOnSelectionChange();
});
```

This is the same field that `capture_takes_verified` (`capture_pane.js:556-619`)
and `render_take_finished` (`render_pane.js:174-212`) advance automatically,
after real disk verification and take-key resolution. A manually-set
`Rendered` status is indistinguishable in the Master Queue's counts
(`master_pane.js:277-279`) from one the pipeline actually confirmed, which
undermines the substantial work elsewhere in the codebase (uid-stable take
tracking, merge-badge surfacing, `isHighlightTracked`) whose whole point is to
make status trustworthy.

**(c) Proposed fix.** Either restrict the manual dropdown to backward-moving
statuses only (e.g. allow resetting `Rendered` → `Pending` for a re-do, but
not manually jumping forward to `Captured`/`Rendered`), or keep it free-form
but tag manually-set values distinctly (e.g. a different badge/tooltip: "set
manually, not verified on disk") so the Master Queue counts stay honest.

**(d) Risk vs. guardrails.** Low — a data-integrity/trust issue in the UI
layer, not a process or memory-safety concern.

**(e) UI-only or engine?** UI-only (`desktop-studio/src/detail_pane.js`).

---

## SECTION 2 — New Feature / Advancement Ideas

Ranked most-promising-first by value-to-effort.

### 1. "Reveal in Explorer" on render jobs and captured takes

**(a) What it is.** A quick-action button on finished render rows (and
captured-take rows) that opens the containing folder in Windows Explorer.

**(b) Why it helps here.** Once a render finishes, the only way to find the
output file today is to remember the export directory and dig through
`demo_take_hash` folder names by hand — there's no link from the finished job
row back to its file on disk.

**(c) Implementation sketch.** The IPC command already exists and is already
wired up elsewhere in the app (`auditor_pane.js:213`, calling
`ipc_bridge.js`'s `revealInExplorer(path)`, backed by a Tauri command). This is
purely a frontend wiring gap in `desktop-studio/src/render_pane.js`'s
`renderJobsTable()` (add a button next to the existing cancel/reset/view-log
actions, using `job.output_path` from the `RenderJob` autosave schema in
`native/src/hlcr/autosave.rs:16-25`, which already carries it).

**(d) Risk/complexity.** Very low — the backend command and the pattern to
copy already exist in-tree.

**(e) Precedent.** Not from an external tool — this is simply completing a
convenience the app already built and shipped for one pane (Demo Auditor) but
not the other two capture/render panes it would help just as much.

---

### 2. Burn the highlight's own kill-timeline label into the rendered clip

**(a) What it is.** An optional lower-third/title-card overlay on the final
FFmpeg output, showing the same human-readable info the app already computes
for each highlight — e.g. "3 kills: Thompson, (+0:04) K98, (+0:02) MP40" — so a
rendered clip is self-labeled without needing the Highlight Details table open
next to it.

**(b) Why it helps here.** This data already exists and is already formatted
for display — `CaptureStreak::update_visuals` builds exactly this string
(`native/src/patch/types.rs:85-111`, mirrored in JS at
`detail_pane.js:96-123`), and it's already burned into the *demo* as an
`svc_director` HLTV title card via `build_director_message`
(`native/src/patch/builder.rs:1061-1104`) for on-screen display during
playback. It just never makes it into the actual rendered video file — once
the BMP sequence leaves HLAE, that context is gone.

**(c) Implementation sketch.** Add an optional `-vf drawtext=...` (or a
composited overlay pass) to the FFmpeg invocation in
`native/src/hlcr/renderer.rs`'s `run_render_job`, driven by a new field on
`ClipData` (`native/src/hlcr/scanner.rs:8-17`) carrying the timeline string
forward from the take's originating highlight (would need the take-key
correlation this codebase already has — `shared/paths::take_key` — to look the
label up at render time). Gate it behind a Render Studio checkbox, off by
default so it doesn't change existing output unexpectedly.

**(d) Risk/complexity.** Medium — no protected frame-format/Cbuf guardrails
apply (this is pure FFmpeg filter-graph work, post-capture), but correctly
resolving which take-folder maps to which highlight's label at render time
(potentially after a restart, with the take index) is real plumbing, and
`drawtext` needs a bundled font file to be reliable across machines.

**(e) Precedent.** This is this repo's own suggestion, not a copied
convention — though automatic title/lower-third overlays are a standard
feature in consumer highlight-reel tools (e.g. Medal.tv, Outplayed) and in
OBS's scene-text sources.

---

### 3. Concatenate selected rendered clips into one movie

**(a) What it is.** A "Combine Selected Clips" action in Render Studio that
stitches two or more already-rendered outputs into a single file using
FFmpeg's concat demuxer, optionally with a fixed-duration crossfade between
them.

**(b) Why it helps here.** The app's own README framing is "batch-record
highlight clips... into highlight reels," but today the pipeline stops at one
file per highlight — assembling a reel still means manually importing every
clip into a separate NLE. Since every render already goes through the same
FFmpeg binary the app already shells out to (`renderer.rs`), a same-codec
concat pass is a small, natural extension rather than a new dependency.

**(c) Implementation sketch.** New function alongside `run_render_job` in
`native/src/hlcr/renderer.rs` that writes an FFmpeg concat-demuxer list file
and runs `-f concat -c copy` (stream-copy, near-instant) when all selected
clips share codec/resolution/fps, falling back to a re-encode pass otherwise.
Frontend: a multi-select on the render jobs table (`render_pane.js`) plus a
new "Combine" button.

**(d) Risk/complexity.** Low-medium. Stream-copy concat is fast and low-risk;
the fallback re-encode path and mixed-codec detection is where the complexity
lives.

**(e) Precedent.** Directly modeled on NLE "sequence/timeline" assembly
(Premiere/Resolve) and on FFmpeg's own concat-demuxer workflow, which is the
standard scripted way to stitch clips without a full NLE.

---

### 4. Live capture progress with current-clip name and ETA

**(a) What it is.** Turn the static 50% progress bar (Section 1, Finding 1)
into a real one: current block N of M, which highlight/demo is being
recorded, and an estimated time remaining based on tick position vs. total
ticks.

**(b) Why it helps here.** Batch captures are long, unattended, HLAE-driven
runs where the only other feedback today is whatever's visible in the actual
game window. A user queuing a large batch and walking away has no way to
gauge progress without tabbing into hl.exe itself.

**(c) Implementation sketch.** This is the fix already described in Section 1,
Finding 1 — tail `qconsole.log`'s `BREADCRUMB`/route-alias lines from inside
the existing `capture_engine.rs` poll loop and emit a new `EngineEvent::Progress`.
Listed here too because it's as much a feature gap as a friction point: today
there is *no* granular capture progress at all, anywhere in the UI.

**(d) Risk/complexity.** Low — see Section 1, Finding 1(d).

**(e) Precedent.** Standard in OBS (recording/streaming stats), and in every
NLE render/export dialog (Premiere, Resolve) — a progress bar with an ETA and
a current-item label is baseline expected behavior for any long-running batch
export, not unique to this tool.

---

### 5. Named render presets

**(a) What it is.** Let a user save the current codec/fps/resolution/
concurrency combination under a name ("YouTube 1080p60", "Discord Clip",
"Archive ProRes") and switch between presets instead of re-entering the same
values every session.

**(b) Why it helps here.** `RenderConfig` (`native/src/hlcr/config.rs:18-27`)
already persists exactly these fields as the single active configuration —
there's no concept of more than one saved configuration at a time, so
switching between "quick Discord clip" and "archival master" workflows means
manually changing every field back and forth.

**(c) Implementation sketch.** Extend `native/src/hlcr/config.rs`'s
`load_config`/`save_config` to store a `Vec<RenderConfig>` keyed by name
instead of a single `RenderConfig`, add a preset dropdown to Render Studio
(`render_pane.js`) next to the existing codec/fps inputs, backed by the same
`get_config_path()` file.

**(d) Risk/complexity.** Low — additive schema change with a default
migration path (wrap the existing single config as the first/default preset).

**(e) Precedent.** Standard convention across OBS (output presets), most
video encoders' GUIs (HandBrake presets), and NLE export-preset dropdowns.

---

### 6. Watch-folder auto-import for new demos

**(a) What it is.** Point Capture Studio at a folder (e.g. DoD's own
`svencoop`/`dod`-style auto-record output directory) and have newly-appearing
`.dem` files automatically queue themselves for scanning, instead of requiring
a manual "Add Demos" click after every match.

**(b) Why it helps here.** This app already has a `scan_demo_folders` bounded
background scanner (`ipc_bridge.js:353-359`, native-side implementation not in
this audit's scope) used for the Explorer sidebar's "Local" quick-links tier —
the file-system-watching primitive this feature needs is architecturally
adjacent to code that already exists, just not wired to auto-trigger a scan.

**(c) Implementation sketch.** A `notify`-crate (or poll-based, given the
existing TTL-cache pattern already used in `native/src/sys/disk.rs`)
filesystem watcher on a configured folder, native-side, emitting a Tauri event
that `main.js` listens for and feeds into the existing `scan_directory` /
`currentScannedDemos` flow — no new scanning logic needed, just a new trigger
for the one that exists.

**(d) Risk/complexity.** Medium — filesystem watchers are usually reliable but
have real edge cases (partial writes while a demo is still being recorded,
network-drive watch reliability); would need a debounce/settle-time before
treating a `.dem` as ready to scan.

**(e) Precedent.** OBS's own replay-buffer/auto-record-to-folder convention,
and general "watch folder" ingest features common in NLE and asset-management
tools.

---

### 7. Export a highlight/take manifest (EDL-style) for external NLEs

**(a) What it is.** A "Export Marker List" action that writes out, per demo or
per session, the tick/timestamp ranges and labels of every captured highlight
— close to an Edit Decision List — so a user who wants to do further editing
beyond this app's own render step can import markers into Premiere/Resolve/
DaVinci against the rendered clips.

**(b) Why it helps here.** The data is already fully computed and already has
a stable identity system built for exactly this kind of cross-referencing —
`CaptureBlock`/`take_key` (`native/src/patch/types.rs:121-136`,
`native/src/shared/paths.rs`) and the frontend's `take_index.js` uid scheme
already tie every highlight to its take folder and timing. Right now none of
that ever leaves the app.

**(c) Implementation sketch.** A new export function (could live alongside
`build_preview_patch_jobs` in `native/src/patch/builder.rs`, or as a pure
frontend CSV/JSON export driven off the already-loaded `currentScannedDemos`
state in `main.js`) writing a CSV or a simple EDL/FCPXML with one entry per
highlight: source file, in/out timecode, label (`timeline_string`).

**(d) Risk/complexity.** Low for a CSV/JSON export; medium if targeting a real
EDL/FCPXML format precisely enough for Premiere/Resolve to import cleanly
(timecode-vs-frame-number edge cases, drop-frame handling).

**(e) Precedent.** Directly modeled on the EDL/FCPXML marker-import workflow
standard to Premiere Pro and DaVinci Resolve.

---

### 8. Contact-sheet / scrub preview of a take before committing to a full render

**(a) What it is.** Before running a multi-minute FFmpeg encode, show a quick
thumbnail strip (e.g. every 20th BMP frame) or a scrubbable low-res preview of
a take's captured frame sequence, so a user can confirm the capture actually
looks right before spending render time on it.

**(b) Why it helps here.** Right now the only way to check whether a captured
take is any good is to either watch it live during capture or wait for the
full-resolution FFmpeg render to finish. Given a capture batch's frames are
already sitting on disk as a numbered BMP sequence
(`hlcr/scanner.rs`'s `collect_image_folders`/`00000.bmp` convention), a cheap
thumbnail pass over the existing files is far cheaper than a full render just
to sanity-check a take.

**(c) Implementation sketch.** A lightweight thumbnail generator (native-side,
reading every Nth BMP from a take folder and downscaling) exposed as a new IPC
command, surfaced in Render Studio as a hover-preview or a small filmstrip on
each scanned take row.

**(d) Risk/complexity.** Medium — BMP decode/downscale for a filmstrip is
straightforward, but wiring it efficiently (not blocking the scan, not
re-decoding on every re-render of the jobs table) takes real care.

**(e) Precedent.** Standard NLE convention (Premiere/Resolve bin thumbnails
and scrub-preview), and OBS's own replay-buffer preview before saving.
