# OBS as an Alternate Capture Method

> **Update 2026-09-03 — the scene/profile design below was superseded, not followed.**
> "The scene picker" and "Building a scene (low priority)" sections recommended a dropdown over the
> user's existing scenes/profiles, with scene *creation* explicitly deferred as a low-priority
> nice-to-have, invoked only on request. What actually shipped (`native/src/obs/provision.rs`) goes
> further in the direction "If it is ever automated, it goes through a profile" already pointed:
> a dedicated `[DoD-Tools]` profile *and* scene, auto-created and **re-verified/repaired on every
> connect** rather than created once and left alone — no picker at all, since there is nothing left
> for the user to choose between. The "detect and state, never mutate" discipline these sections
> argue for still holds, just narrowed to its actual point: never mutate the user's *own* profiles/
> scenes. A profile/scene dod-tools creates and names itself was always the doc's own answer to "if
> it is ever automated" — this just stopped waiting to automate it. See that module's doc comment for
> the current design and reasoning; the sections below are the historical case for it, still useful
> context but no longer the plan.

> **Status 2026-08-28 — built and tested against a live OBS.** Tracks
> [#65](https://github.com/ccoventry/dod-tools/issues/65), in PRs
> [#72](https://github.com/ccoventry/dod-tools/pull/72) (this document) →
> [#73](https://github.com/ccoventry/dod-tools/pull/73) (`probe_obs`) →
> [#74](https://github.com/ccoventry/dod-tools/pull/74) (the feature).
>
> **What has actually been run:** a full batch producing playable clips with audio; a cancel
> mid-recording; OBS killed between blocks; OBS killed mid-recording; dod-tools killed mid-batch and
> recovered on restart. Every failure path in the table below except the stall watchdog.
>
> - OBS Game Capture captures the HLAE-injected `hl.exe` — verified against a real frame.
> - `qconsole.log` carries the per-block signal at **21–40 ms**, measured over a 17-block batch under
>   the heaviest I/O configuration the pipeline has.
> - Every obs-websocket request the design needs exists on the install tested.
>
> **Not built:** the Render Studio trim pass (staging item 6), and the wall-clock/disk comparison
> against the existing path (item 7) — so the speed claim remains an expectation.
>
> See "Measured against a live OBS". **One claim was refuted rather than confirmed:** the wall-clock
> saving is small — the real prize is disk. See "What OBS actually buys".
>
> **Scope, set by the user 2026-08-28**, narrowing the issue's own title:
>
> - **Not a replacement.** It sits alongside the existing capture path.
> - **An alternate option for people who care less about quality** and want the convenience.
> - **Separate HUD is out.** "That won't work with OBS", and it is not wanted here. No effort goes
>   into HUD compositing on this path.
>
> Prerequisite reading: `docs/direct_to_video_capture.md` (#42, shipped in PR #68) answered how a
> non-BMP capture artefact flows through take verification, Render Studio's admission predicate and
> export routing. Several of its answers transfer directly and are cited rather than re-derived.
> `docs/goldsrc_dod_quirks.md` and `docs/hlae_protocols.md` hold the engine facts.
>
> **Everything below marked *(unverified)* needs an experiment.** The sections that are settled are
> settled by reading this repository's own code, or — where it says so — by the probe run recorded
> under "Measured against a live OBS" below.

---

## Measured against a live OBS, 2026-08-28

Run with `native/src/bin/probe_obs.rs` against **OBS 32.2.2 / obs-websocket 5.7.4**. Everything in
this section is measurement, not expectation.

**Every request this design needs exists**, including both that were open questions:

| request | | |
|---|---|---|
| `StartRecord` / `StopRecord` / `GetRecordStatus` | YES | the core loop |
| `GetRecordDirectory` / `GetVideoSettings` | YES | preflight |
| **`SetRecordDirectory`** | **YES** | so Option A's per-block export-pool routing is possible |
| **`SplitRecordFile`** | **YES** | so Option B exists as a contingency |
| `GetSceneList` / `GetSceneItemList` / `GetInputList` / `GetInputSettings` / `GetSceneCollectionList` | YES | the scene picker is buildable |
| `SetVideoSettings` / `GetProfileList` / `SetCurrentProfile` / `CreateProfile` | YES | see "Video settings" below |

**Start latency: 59–69 ms**, measured twice — the gap from `StartRecord` to
`RecordStateChanged: OBS_WEBSOCKET_OUTPUT_STARTED`, which is when frames actually begin. The
pre-roll budget is `AUDIO_RESYNC_SECONDS` = 2.0s, so this fits roughly thirty times over.
**Option A's timing holds, and Option B's only real advantage disappears** — it stays a contingency
and should not ship as a second user-facing mode.

**Stop to file finalised: ~1.065 s**, also measured twice. This is a problem worth naming:
`MIN_TAKE_SEPARATION_SECONDS` is **1.0 s**, so OBS needs marginally *longer* to finalise a file than
the pipeline currently guarantees between one take's stop and the next one's start. That constant
was derived for HLAE and must be raised for this path.

**The container was MP4**, not the MKV this document assumed OBS defaults to. Both need handling, or
the setting needs pinning — see the open question on containers.

### The console log carries the signal — measured over a 17-block batch

Run with `probe_obs log` against a real capture: `capture_fps` 120, 1280x720, `ffmpeg_capture` off
and **Separate HUD on**, so HLAE was writing three BMP sequences throughout. That is the heaviest
I/O configuration the pipeline has, which makes it the right test rather than a lucky one.

**Markers reach `qconsole.log` with per-frame granularity.** Commands the pipeline schedules one tick
apart — `SPEED_FLUSH` after `CUSTOM_CMD1_BEFORE`, `CUSTOM_CMD2_AFTER` after `STOP_RECORD` — arrived
**21–40 ms** apart in all 17 blocks. Nothing accumulated, nothing flushed late, and no marker was
ever missing. **`qconsole.log` is a viable signalling channel and the `screenshot` fallback is not
needed.**

The three speed regimes separate cleanly, which is the control that says the numbers are real:

| phase | measured rate |
|---|---|
| fast-forward (`host_framerate 0.05`) | ~5,400 ticks/s |
| pre-roll, settled (`host_framerate 0`) | ~474 ticks/s — real time |
| recorded window (`mirv_recordmovie`) | ~355 ticks/s |

The demo ran at **474 ticks per second**, confirmed independently: `pre_roll_seconds` was 5.0 and
`SPEED_FLUSH`→`START_RECORD` measured 2370 ticks in every block, which is exactly 5.0 x 474.

**Demo time tracks wall clock at `host_framerate 0`, with jitter.** The `AUDIO_SYNC`→`START_RECORD`
span is 1.0s of demo time by construction; across the 15 blocks that got a full lead it measured a
**mean of 1.010s wall-clock, σ ≈ 0.14s, range 0.866–1.194s**. No systematic bias — an earlier
single-sample reading of "13% short" was simply one draw from that spread — but the jitter is real,
and it is why the stop is now driven by an echo rather than a timer.

**The early pre-roll is not yet real time.** `SPEED_FLUSH`→`AUDIO_SYNC` is 4.0s of demo time and
consistently measured shorter in wall-clock (~2.8–3.7s). Whatever the mechanism, the engine has not
settled immediately after leaving fast-forward, and it has by `AUDIO_SYNC`. That is what decides the
trigger point in Option A.

### The hook question: answered, both halves

With the HLAE-launched game running, `hl.exe` had **both** hooks loaded at once:

    AfxHookGoldSrc.dll      HLAE's hook
    graphics-hook32.dll     OBS Game Capture's hook
    OPENGL32.dll            the API both are hooking

So OBS Game Capture injects successfully into an HLAE-injected GoldSrc process, and the game stays
alive and responsive. That retires the *crash* form of the risk.

**And the frames arrive.** A 6-second probe recording taken while that process was rendering came
back with **no black frames at all** (`blackdetect d=0.05 pic_th=0.98`), 179 frames across 5.967s at
30fps — no drops — and an extracted frame is the Day of Defeat menu in colour. Two hooks on the same
OpenGL buffer swap do not fight.

**So Game Capture is viable and Window Capture is not needed as a fallback.** This was the largest
risk in the document and it is now closed on evidence rather than on the absence of a crash.

**A caution worth keeping, because it cost two rounds of analysis.** A Game Capture source whose
target process has exited records black into a file that is valid in every other respect — right
resolution, right frame rate, right duration, sometimes real audio from another source. Two probe
recordings were wasted that way before the cause was understood, and the second one looked exactly
like a hook collision. dod-tools owns the game's lifecycle, and *any* rebuild of the workspace while
`tauri dev` is watching takes `hl.exe` down with it — including a `git checkout` that touches
`Cargo.toml`. The game having been launched earlier in a session is not evidence that it is running
now, which is why `probe_obs` checks and says so.

---

## The one fact everything else follows from

`mirv_recordmovie` does not capture in real time. It pins the engine's timestep to
`1 / mirv_movie_fps` and renders **every** step regardless of wall-clock, so a 40-second clip at 120
fps takes as long as the machine needs and comes out frame-exact every time. The same demo captured
twice is byte-identical.

OBS captures a window in real time. It takes whatever the screen actually showed.

That single difference decides the shape of the whole feature:

| | HLAE (`mirv_recordmovie`) | OBS |
|---|---|---|
| engine timing during a clip | `host_framerate` pinned to `1/fps` | `host_framerate 0` — real time |
| wall-clock per clip | `capture_fps / achieved render fps` — **measured 1.34x real time** at 120fps | exactly the clip's duration, always |
| high FPS / slow motion | the point of the path | not possible |
| dropped frames | impossible by construction | whatever the machine drops |
| determinism | byte-identical reruns | no |
| audio | separate `sound.wav`, muxed later | in the file already |
| output | thousands of BMPs, or one lossless video | one finished, playable file |
| render pass | required | optional (see below) |

**What OBS actually buys — corrected 2026-08-28 by measuring a real batch.** The original claim here
was wall-clock, and that is mostly wrong.

A 17-block batch at `capture_fps` 120, 1280x720, with Separate HUD on (three BMP streams), recorded
**309s of demo time in 413s of wall-clock — 1.34x real time**. OBS is fixed at 1.0x by definition,
so it would have saved about 104s out of the 569s the whole capture phase took: **roughly 18%**.

HLAE's wall-clock cost is `capture_fps / achieved render fps`, and that machine achieved **~90 fps
while writing three BMP streams** at 720p. So on *that* machine, at `capture_fps` 60, HLAE would run
at ~0.67x real time — faster than OBS could ever be, since OBS cannot capture quicker than the clip
plays.

> **The break-even is `capture_fps` = achieved render fps, and it moves with the machine.**

**Which is the part that generalises, and the 1.34x figure is not.** The machine measured here is
high-end; 1.34x is close to the best case for HLAE, so **that measurement is a lower bound on OBS's
wall-clock advantage, not a typical one**. Work the formula for a weaker machine and it inverts:

| achieved render fps | HLAE at `capture_fps` 60 | HLAE at `capture_fps` 120 |
|---|---|---|
| ~90 (measured here) | 0.67x — HLAE wins | 1.34x — OBS saves ~25% |
| ~45 | 1.33x — OBS saves ~25% | 2.7x — OBS saves ~63% |
| ~30 | 2.0x — OBS saves ~50% | 4.0x — OBS saves ~75% |

So the speed pitch is real, just not for the person who measured it. **OBS saves the most time
exactly for the users with the least capable hardware** — which is plausibly the same audience that
wants the convenience option in the first place.

**And that cuts the other way too, which is the tension worth stating plainly.** A weak machine is
also the one that cannot render 60 fps in real time, so OBS drops frames there. The machines where
OBS saves the most wall-clock are the machines where OBS produces the worst output. That sharpens
the dropped-frames open question below from a nicety into the thing that decides whether this path
is usable for the people it most helps.

The durable advantages are elsewhere, and they are large:

- **Disk.** That same batch wrote on the order of **300 GB** of bitmaps (309s x 120fps x 3 streams x
  2.76 MB). The OBS equivalent is a few hundred megabytes. This is the reason `build_batch_queue`
  bin-packs across a pool of drives at all, and it is the difference between needing that machinery
  and not.
- **No render pass.** Audio is already muxed, so a finished, playable file exists the moment the
  clip ends.
- **Hardware encoding for free**, concurrent with capture rather than after it.

That is still a real pitch. On a strong machine it is a disk-and-simplicity pitch; on a weak one it
is a speed pitch as well, with a quality cost attached.

### The app can measure this for the user, with what it already reads

`achieved render fps` is not a number anyone can be expected to know, and it is the only input the
break-even needs. But the console-log tailer built for the start signal **already measures it**: the
breadcrumb and stage markers carry demo ticks and arrival times, so the ratio of demo time to
wall-clock during a recorded window falls straight out of the same stream — that is exactly how the
1.34x above was obtained, after the fact, from a log.

So after one batch the app knows this machine's capture ratio, and can say something useful and
specific rather than generic: whether OBS would be faster *here*, at *this* `capture_fps`, and
whether the machine can sustain the OBS output rate without dropping frames. That is a far better
answer than a settings tooltip guessing on the user's behalf, and it costs nothing extra to compute
once the tailer exists.

**What it costs** beyond quality, and what a user has to be told: capture becomes sensitive to
everything else on the machine. An alt-tab, a stutter on map load, a notification sound landing in
desktop audio, a screensaver — all of these are now *in the clip*, and none of them can reach the
existing path.

---

## The fast-forward still works, and that matters

`sys_fast_forward` is `host_framerate 0.05` and `sys_normal_speed` is `host_framerate 0`
(`native/src/patch/builder.rs`). Nothing is being recorded between blocks, so the fast-forward
between highlights is untouched by this change — a batch is still much shorter than the demos it
walks.

Only the recorded window changes, and it changes by **doing nothing**: with no `mirv_recordmovie` in
play, the engine simply stays at `host_framerate 0` — real time — for exactly the span the pre-roll
already dropped it into. The pre-roll's own reason for existing (flushing the audio buffers the
fast-forward corrupts, `docs/goldsrc_dod_quirks.md`) is unchanged and still needed.

**The consequence worth stating loudly:** at `host_framerate 0` the engine advances demo time by
real elapsed time. So a block's wall-clock duration equals its demo-time duration *exactly*, even on
a machine that stutters — a stall costs frames, not seconds. That fact is what makes the
synchronisation problem below far smaller than it looks.

---

## How OBS gets driven

`obs-websocket` v5 ships inside OBS Studio 28 and later; there is no plugin to install. Default port
`4455`, optional password, JSON over WebSocket, SHA-256 challenge/salt handshake.

The requests this needs:

| request | why |
|---|---|
| `GetVersion` | preflight. Returns `obsVersion`, `rpcVersion` **and `availableRequests`** — which is how to feature-detect the optional requests below rather than version-sniffing. |
| `GetRecordStatus` | `outputActive`, `outputDuration`, `outputBytes`. Per-block verification, and the answer to "is OBS already recording something else". |
| `StartRecord` / `StopRecord` | `StopRecord` returns `outputPath` — the artefact's location, which is what take verification keys off instead of a take folder. |
| `RecordStateChanged` (event) | `OBS_WEBSOCKET_OUTPUT_STARTED` / `STOPPED`, with `outputPath` on stop. Preferable to polling: it reports the moment recording *actually* began, which is not the moment `StartRecord` returned. |
| `GetRecordDirectory` | where files will land, for the preflight report and the disk check. |
| `SetRecordDirectory` | **confirmed present** — per-block export routing without a cross-drive copy. |
| `SplitRecordFile` | **confirmed present** — the basis of Option B below. Per-container support still unchecked. |
| `GetSceneList`, `GetCurrentProgramScene`, `GetSceneItemList`, `GetInputList`, `GetInputSettings` | read-only scene inspection for the preflight. |
| `GetVideoSettings` | canvas/output resolution and `fpsNumerator`/`fpsDenominator` — the number that replaces `mirv_movie_fps` in `take_meta`. |

**No WebSocket client is in the workspace today.** `native/Cargo.toml` has `tokio` (with `rt`,
`macros`, `process`, `fs`, `sync`, `time`) and `serde_json`, so `tokio-tungstenite` plus a SHA-256
and base64 crate for the handshake is the addition. That is a new dependency on a crate family this
project has not used, and is worth a moment's thought before it lands.

---

## The synchronisation problem, and why it is smaller than it looks

The pipeline schedules commands at exact frame ordinals *inside the demo*, and injects them as
`ConsoleCommand` frames. OBS is an external process that knows nothing about demo ticks. The capture
engine, meanwhile, spawns one `hl.exe` for the entire batch and then does nothing but poll for
`DOD_TOOLS_EXIT_TRIGGER` (`native/src/capture_engine.rs`) — it has no per-block awareness at all.

Something has to cross that gap. There are three ways, and the second is the recommendation.

### The channel: the console log is already tick-accurate telemetry

`build_safe_echos` already writes an echo at every stage boundary of every block:

    [dod-tools] SPEED_FLUSH - Tick 41250
    [dod-tools] AUDIO_SYNC - Tick 41350
    [dod-tools] START_RECORD - Tick 41450
    [dod-tools] STOP_RECORD - Tick 44950
    [dod-tools] FAST_FORWARD - Tick 45150

plus a `BREADCRUMB` every `BREADCRUMB_INTERVAL_TICKS`. With `-condebug` — **which
`build_hlae_process` passes on every launch, with no way to turn it off**
(`native/src/patch/types.rs`) — these land in `qconsole.log` beside `hl.exe`, a
file the app already knows about and deletes (`shared::paths::remove_console_log`).

So the signalling channel exists, is tick-accurate, needs no new engine commands, and costs the
capture nothing. **The app has simply never read it.**

The alternative the issue proposed — reusing the `DOD_TOOLS_EXIT_TRIGGER` trick, i.e.
`mirv_movie_filename X; mirv_recordmovie_start; mirv_recordmovie_stop` to make a folder appear —
works, but should be rejected here: it starts a real HLAE recording for an instant, which yanks
`host_framerate` to `1/mirv_movie_fps` and back. That is a visible hitch landing *precisely* at the
first frame of the clip, and under direct-to-video it also spawns an FFmpeg process per block.

**Measured, and the answer is good: 21–40 ms per marker**, across 17 blocks, with HLAE writing three
BMP streams at 120fps throughout. GoldSrc's debug log does flush per line. See "The console log
carries the signal" above.

*(A `screenshot`-based marker was the fallback had the log turned out to be buffered — a console
command with a filesystem side effect, under the 64-byte Cbuf limit, costing one frame rather than a
`host_framerate` yank. It is not needed and is recorded here only so the option is not re-derived.)*

### Option A (recommended): drive both ends off the echoes

**Revised 2026-08-28 after measuring a real 17-block batch.** The original plan was to signal the
start and compute the stop by timer, because the log's latency was unknown and a timer avoided
depending on it twice. The measurement removed the reason: markers reach `qconsole.log` in
**21–40 ms**, so there is nothing to avoid.

1. Tail `qconsole.log`. On **`AUDIO_SYNC`** for block *i*, send `StartRecord`.
2. Wait for `RecordStateChanged: STARTED`.
3. On **`STOP_RECORD`** for block *i*, send `StopRecord`.
4. `StopRecord` returns `outputPath`.

Three properties make this work, and two of them are now measured rather than argued:

- **OBS's start latency fits the lead many times over.** `AUDIO_SYNC` fires
  `SOUND_FLUSH_LEAD_SECONDS` = 1.0s of demo time before the record start, measured at **1.010s mean
  wall-clock across 15 blocks**. OBS starts frames in **59–85 ms**. That is a ~14x margin.
- **`AUDIO_SYNC`, not `SPEED_FLUSH`, is the right trigger.** The full pre-roll was 5.0s in the
  measured batch, but its *early* portion does not run at real time — the engine is still settling
  out of fast-forward, and that span consistently measured shorter in wall-clock than its demo
  duration. By `AUDIO_SYNC` it has settled. Triggering there also means only ~1s of pre-roll head
  ends up in the file instead of five, so there is less to trim.
- **Stopping on the echo removes the design's one shaky assumption.** The timer needed demo time to
  track wall clock exactly. It does in the mean, but with **σ ≈ 0.14s** on a 1.0s span — and not
  every block gets the full lead (two of seventeen were clamped, at 454 and 81 ticks, where blocks
  chained or highlights merged). An echo-driven stop is self-correcting and needs none of that. Keep
  the computed duration as a **fallback timeout** for a missing echo, not as the primary mechanism.
- **Each clip is its own recording, so each clip can be routed independently.** With
  `SetRecordDirectory` between blocks, the multi-drive export pool the pipeline already bin-packs
  across keeps working on this path. Nothing else in this design preserves that.

Only the clip and its rolls are ever written, so the disk cost is the output and nothing else.

The cost is that the recording contains the pre-roll and post-roll as head and tail footage. See
"Does Render Studio still run" below — the user can simply trim it in their editor, which is why the
render pass stays *optional* here rather than becoming a requirement.

### Option B: record continuously, split at boundaries

**This is not "one big file, cut up afterwards".** `SplitRecordFile` closes the current file and
opens the next one *live*, mid-recording, so OBS itself writes the separate clips. Recording never
stops between blocks, so the output is an alternating sequence:

    [fast-forward junk] [clip 1] [fast-forward junk] [clip 2] [fast-forward junk] ...

and the app deletes the junk segments as they close. The demo is never held as a single file, and the
clips arrive already separate.

What it buys is the elimination of encoder start/stop entirely: no start latency to absorb, and no
exposure to `MIN_TAKE_SEPARATION_SECONDS`-style risk from a stop/start cycle landing too tight.

What it costs:

- **Transient disk for the junk segments.** Small — fast-forward is fast — but non-zero, and it is
  strictly more than Option A writes.
- **Per-block drive routing is gone.** One continuous recording writes to one directory, so the
  export pool collapses to a single drive for the whole chain.
- **It depends on a request** whose availability and per-container support both need checking.

### Which one ships

**Option A, and Option B is a contingency rather than a second user-facing mode.**

Option B's only real advantage is removing start/stop latency — which matters *only if* OBS's start
latency does not comfortably fit inside the pre-roll. **It fits: 59–69 ms measured, against a 2.0s
pre-roll.** So B buys nothing and costs drive routing.

Shipping both as a user choice means maintaining two signalling paths, two verification shapes and
two cleanup paths, on the feature explicitly scoped as the *convenience* option. That is a poor
trade unless the measurements force it. Prototype B if `SplitRecordFile` turns out to be available,
keep it in the back pocket, and revisit only if A's timing does not hold.

### Option C: precompute everything. Rejected.

Fast-forward duration is not predictable — it runs as fast as the machine renders — so wall-clock
offsets from batch start cannot be computed ahead of time. At least one signal per block is
mandatory.

---

## The output, and the recommendation that keeps the rest of the pipeline

OBS writes one file wherever its profile points, named by its own filename formatting. `StopRecord`
returns the authoritative path.

**Recommendation: fold it back into the take-folder shape.** After `StopRecord` returns, move the
file to `<block take_folder>/take0000/all/video.<ext>`. On the same volume that is a rename, i.e.
free. The block's `take_folder` is already computed at dispatch (`native/src/patch/builder.rs:788`),
and `shared::paths::take_key` already matches across the `take0000` nesting.

The payoff is large and mostly consists of *not writing code*:

- `collect_image_folders` already admits a stream folder holding a video (`VIDEO_FILE`).
- `stream_video` / `avi_frame_count` already read frame counts out of the container for the progress
  bar — though `avi_frame_count` is AVI-specific, and OBS defaults to MKV. Either constrain the
  container or teach the scanner a second one *(unverified which is less bad)*.
- `renderer.rs` already has the video-input branch, and already omits `-framerate` for a video take
  because a video carries its own timing.
- Export-pool JIT drive routing, output naming (`{demo}_{take}_{stream}_{hash}.{ext}`) and the
  hash-suffixed uniqueness all keep working untouched.
- `take_key` keeps matching, so the capture side and Render Studio still agree about which take is
  which.

The alternative — leaving the file where OBS put it and teaching every downstream component a second
artefact shape — is strictly more work for a worse result.

### The one predicate change

`is_renderable_take` requires **a wav *and* a stream folder**. An OBS take has the video with audio
already inside it and no wav at all, so it fails. This is the single deliberate change, and its own
doc comment states the rule: *"Shared with the capture-side take verification so 'the capture
succeeded' and 'Render Studio can actually see it' can never silently disagree — if this predicate
changes, both sides change together."*

The shape wanted is a third admissible case: a stream folder holding a video **with an audio
stream**, no wav required. Testing for the audio stream rather than just relaxing the wav rule is
what stops a silently-muted OBS take being admitted as renderable — and unlike the frame-sequence
case, that check is cheap, because the container header carries it.

### Take verification gets *stronger*, not weaker

`take_folder_has_content` is deliberately loose — non-empty folder, ignoring our own metadata — so an
unanticipated HLAE layout degrades to a warning. The OBS path can do much better, because the app
knows things it has never known before:

- `GetRecordStatus` reported `outputActive` and a duration for that block.
- `StopRecord` named a file, and that file exists and is non-trivial.
- **The expected duration is known exactly** (the real-time property again), so the recorded duration
  can be asserted within tolerance.

That last check has no equivalent in the BMP path, and it catches precisely the failure this pipeline
keeps getting bitten by: output that exists, looks plausible, and is wrong. `VerifiedBlock` keeps
both tiers — `captured` becomes "OBS reported a file of about the right length", `renderable` stays
the shared predicate.

`take_meta` needs no structural change: it records the capture rate per take, and OBS's rate from
`GetVideoSettings` goes in the same field. The existing `[render-fps-mismatch]` check keeps working.

---

## Does Render Studio still run?

**Yes, and it should be offered rather than skipped** — the opposite of the issue's title, and the
head/tail footage from Option A is why.

The render pass on this path is not an encode of thousands of bitmaps. It is: trim the pre-roll and
post-roll, apply the pipeline's naming, and route to the export pool. On a compact hardware-encoded
file that is seconds of work, and it delivers exactly the consistency the issue worried about losing.

Trim precision is the one real decision:

- `-ss` with `-c copy` snaps to a keyframe. OBS's default keyframe interval is around 2s, which is
  the same order as the pre-roll — too coarse.
- Re-encoding gives a frame-exact trim, and on an already-compressed clip it is fast. Given this path
  exists for people trading quality for convenience, a second encode is a smaller compromise here
  than anywhere else in the pipeline.

Recommendation: re-encode, and expose "leave the take as OBS wrote it" as the skip. Do **not** ask
the user to change OBS's keyframe interval — see below. **Unless Custom Output is in play, in which
case there is a better answer** — see the next section.

**Scoped as [issue #82](https://github.com/ccoventry/dod-tools/issues/82), 2026-08-28 — read that
before building this.** The recommendation above turned out to rest on a gap: `scan_folder_background`
(`native/src/hlcr/scanner.rs`) hard-requires a `sound.wav` to discover a take at all, so an OBS take
— which has no wav, its audio already being in the video — is never scanned into a `ClipData` and
never reaches Render Studio's queue in the first place. `is_renderable_take`, built earlier in this
same effort, does correctly recognize the shape, but that is the *Capture Studio verification* path,
a different function from the *Render Studio scanner*. So this is three pieces, not one: scanner
support for the shape, a render path with no separate wav to mux (`run_render_job` currently hard-fails
without one), and the trim itself — which is the easy part once the other two exist. Two decisions
locked in when this was scoped: `ClipData.wav_file` becomes `Option<String>` rather than staying a
bare `String`, and "skip" still routes the take into the export pool (naming applied, same as a
rendered take) just without re-encoding — not a no-op "mark it Rendered" with no file movement at all.

**Scanner + render-path + skip landed 2026-08-28.** `scan_folder_background` now discovers an OBS
take via the shared `take_shape_is_renderable` predicate; `run_render_job` has a `RenderCodec::SourceCopy`
branch (surfaced in Render Studio as a per-job "Skip" toggle, offered only for an OBS-shaped clip)
that copies the OBS-written file straight into the export pool with no FFmpeg pass, and a plain
single-video-input branch for the "render" case when `wav_file` is `None`. **The trim itself did not
ship with this**: nothing on disk records how many seconds of pre-roll/post-roll head and tail an OBS
block actually has, so "render" currently re-encodes the whole clip (codec conversion, export-pool
routing, pipeline naming) without cutting the head/tail. That gap — and the capture-side metadata it
needs — is tracked separately as
[issue #84](https://github.com/ccoventry/dod-tools/issues/84).

---

## Custom Output (FFmpeg): lossless capture, and exactly what it costs

OBS's Advanced output mode offers a **Custom Output (FFmpeg)** recording type that exposes the
container, video codec, audio codec and encoder arguments directly. That means OBS can be told to
write `utvideo`, `ffv1` or `libx264 -qp 0` — the same lossless intermediates
`docs/direct_to_video_capture.md` measured for the HLAE path.

**This changes the framing of the whole feature.** The OBS route is the lower-quality option because
capture is *real-time* — dropped frames, no 300fps, no determinism. It is **not** lower-quality
because of compression, and the document should not imply the two are the same cost.

Three consequences follow, and the middle one is the prize:

1. **Lossless codecs are all-intra**, so every frame is a keyframe. The keyframe-snapping objection
   to `-ss` with `-c copy` disappears: the Render Studio trim becomes **frame-exact and a genuine
   stream copy**, seconds of work with no quality loss. That is strictly better than the re-encode
   recommended above, and it is available only in this mode.
2. **`utvideo` in an AVI makes the artefact byte-shaped like the direct-to-video one.**
   `collect_image_folders`, `stream_video`, `avi_frame_count` and the renderer's video branch would
   all work unchanged — the OBS path inherits #42's plumbing for free rather than needing a second
   container taught to the scanner.
3. **OBS states it is "provided with no safeguards".** Misconfiguration produces a broken file
   silently, which is the exact failure class this pipeline keeps being bitten by. It makes the
   duration assertion in `VerifiedBlock` more valuable, not less.

### The cost, measured 2026-08-28

**`SetRecordDirectory` does not steer Custom Output.** Tested directly, with consent, and restored
afterwards:

    AdvOut/RecType -> FFmpegOutput          [took]
    SetRecordDirectory -> <new path>
      GetRecordDirectory  : <new path>      [followed]
      AdvOut/FFFilePath   : <unchanged>     [did NOT follow]

Custom Output keeps its own path in `AdvOut/FFFilePath`, and the recording request only steers the
standard output. So **out of the box, lossless capture and Option A's per-block drive routing are
mutually exclusive** — which matters, because per-block routing across the export pool is the main
advantage Option A has over Option B.

### Which is also the argument for the dedicated profile

`SetProfileParameter` **can** write `AdvOut/FFFilePath` directly — verified, since restoring it is
exactly that write. So per-block routing in Custom Output is achievable; it is simply a *profile*
write rather than a recording call.

That is the same act this document already refuses to perform on somebody's existing profile, and
the same act it already sanctions inside a profile we created. So the two threads converge:

> **A dedicated `dod-tools` profile is not just about the canvas. It is what allows lossless capture
> and per-clip export routing to coexist at all.**

Inside our own profile, writing `FFFilePath` per block is unremarkable. Inside theirs it is not
something to do, and in Standard mode it is not needed, because `SetRecordDirectory` works there.

**So the shape is:** Standard mode works today with routing and needs no profile writes, at the cost
of codec choice. Custom Output buys lossless, all-intra capture and a free frame-exact trim, and
requires the dedicated profile to keep routing. Offer Standard first; treat Custom Output as the
quality tier that comes with the profile.

Measured incidentally, and it closes an open question: the live profile's Standard container is
**`hybrid_mp4`**, which is the crash-safe MP4 variant. So the "MP4 risks an unfinalised file" worry
does not apply to a default modern OBS.

---

## The capture source is the real unknown

OBS needs a source pointed at the game. `hl.exe` runs **windowed** (`-windowed` is hardcoded in
`build_hlae_process`) at the configured `-w`/`-h`, which helps: a windowed target is what Window
Capture handles best, and the resolution is known ahead of time, so the OBS canvas can be checked
against it in the preflight rather than discovered by scaling artefacts.

**Game Capture was the risk, and it is answered.** HLAE's `AfxHookGoldSrc.dll` is already injected
into `hl.exe` and already hooks its OpenGL presentation; OBS Game Capture injects its own graphics
hook into the same process to do the same thing.

Measured 2026-08-28: both hooks load into the same live process, the game keeps running, **and the
captured frames contain the game** — see "Measured against a live OBS" above. Window Capture is kept
in mind as a fallback that injects nothing and therefore cannot collide, but nothing currently
requires it.

Fall back to **Window Capture**, which does not inject anything and therefore cannot collide, at the
cost of requiring the window to stay unoccluded. Display Capture is the last resort.

**On the scene itself: detect and state, never mutate.** This is the same discipline `cfg_scan`
applies to the game's own `.cfg` files and the direct-to-video work applies to HLAE's `ffmpeg.ini` —
someone's OBS scene collection is their streaming setup, not our scratch space. Creating a scene, or
repointing an existing source, could break a livestream to fix a capture. The same rule covers the
recording format, encoder, keyframe interval and file-splitting settings: read them, report anything
that will produce a bad result, change nothing.

### The scene picker

"Detect and state" does not have to mean making the user describe their OBS setup by hand. Everything
needed to populate **a dropdown of their actual scenes** is a read:

| request | what it gives the picker |
|---|---|
| `GetSceneCollectionList` | the active collection, and that there are others |
| `GetSceneList` | the dropdown's contents |
| `GetSceneItemList` | what each scene actually contains |
| `GetInputList` | each source's `inputKind` — `game_capture`, `window_capture`, `monitor_capture`, `wasapi_output_capture`, `wasapi_process_output_capture` |
| `GetInputSettings` | which window a capture source is pointed at |

So the picker can do better than list names: it can badge each scene with whether it holds a capture
source aimed at `hl.exe`, which kind (and therefore whether it is the one that might collide with
`AfxHookGoldSrc`), and whether any audio source is present at all — the failure that otherwise
produces a perfectly valid silent clip. That turns the preflight from a list of complaints into a
choice with the consequences written next to it.

**Scene names are scoped to a scene collection.** A remembered scene name is not meaningful on its
own: switch collections and it either vanishes or, worse, resolves to something unrelated. Store the
collection alongside the name and re-validate at dispatch.

`SetCurrentProgramScene` — switching to the chosen scene when a batch starts — is worth calling out
as a *third* category, between reading and editing. It changes live state, so it is a mutation; but
it is reversible, destroys nothing, and is exactly what a user picking a scene is asking for.
Restore the previously-active scene when the batch ends, and say so in the UI.

### Video settings, and why the canvas is not ours to set

`SetVideoSettings` is present, so canvas resolution, output resolution and FPS *can* be set from
code. They mostly should not be, and the reason is not the one it looks like.

**A canvas is not a scene property.** It belongs to the active **profile** and is shared by every
scene, so writing to it for a capture writes to whatever else the user does in OBS.

**The damage is the transforms, not the resolution.** Scene item positions and scales are stored in
canvas coordinates, in the scene *collection*. Drop the canvas from 1920x1080 to 1280x720 and every
source in every scene is mispositioned. Nothing in the API signals that, and it is the failure a
user would notice next time they streamed rather than during our batch.

Two further constraints, both measured or documented rather than assumed:

- **`SetVideoSettings` is refused while an output is active.** It has to run in preflight, before
  `StartRecord`. It cannot rescue a batch mid-flight.
- **Profiles and scene collections are separate axes.** Switching profile keeps the current scene
  collection, so a dedicated profile alone does *not* solve the transform problem — the sources are
  still laid out for the old canvas.

The mismatch is worth detecting regardless, and detecting it is nearly free: the pipeline already
knows the game's resolution (`resolution_width` / `resolution_height`), so comparing it against
`GetVideoSettings` is one comparison.

**And it is not a cosmetic check.** The live install had a 1920x1080 canvas, a 1280x720 output, and
a game rendering at 1280x720 — and the captured frame shows what that actually produces: the game
occupying roughly **854x480 in the top-left of a 1280x720 frame**, black everywhere else. The source
sits at its native size on a canvas 1.5x larger, and the whole canvas is then scaled down by 1.5 to
reach the output. About **two thirds of the pixels are discarded before the encoder ever sees
them.**

Nothing in OBS flags this; the recording is perfectly valid and simply much worse than the machine
is capable of. Output FPS was also 30, which on this path governs the clip entirely.

Setting the base canvas to match the game — 1280x720 here — makes the whole path 1:1 with no
resampling at all. That is a one-line preflight comparison away from being impossible to get wrong.

**So: detect and report, do not write.** The preflight says the canvas disagrees with the game's
resolution and what to set it to, and says when the output FPS is low. Always correct, cannot break
anything, and a one-time manual fix is cheap.

**If it is ever automated, it goes through a profile.** `CreateProfile` and `SetCurrentProfile` are
both present, and a profile carries canvas, output resolution, FPS, recording format, encoder and
output directory — every setting this feature wants pinned. Create a `dod-tools` profile, switch to
it for the batch, switch back after. Additive and reversible, never editing the profile they are
already using. `SetSceneItemTransform` exists to re-fit a source afterwards, but only ever inside a
scene we created.

That puts it in the same tier as the scene builder below, and it should land with it.

### Building a scene (low priority)

`CreateScene`, `CreateInput`, `SetInputSettings` and `CreateSceneItem` make an offered "set one up
for me" feasible, and the recommended configuration is non-obvious enough to be worth automating:
a capture source targeting the `hl.exe` window, an Application Audio Capture on the same process
rather than Desktop Audio, and a canvas matching the pipeline's own `-w`/`-h`.

The discipline that makes this acceptable is that it is **purely additive**: create a *new* scene,
never touch an existing one. That is a different act from repointing a source someone is streaming
with. It must still be explicitly invoked, never run as part of a preflight, and it should say what
it is about to create before creating it.

Worth having, worth doing last. The picker is what makes the feature usable; scene creation only
makes first-time setup nicer, and it is the part most likely to age badly as OBS's input kinds and
settings change under it.

---

## Audio: solved, and newly breakable

The genuine simplification. OBS records audio into the file, so the separate `sound.wav` and its mux
disappear, along with the FPS-mismatch class of bug that comes from the two being timed independently
(`docs/hlae_protocols.md`: the wav's duration is the frame count's cross-check — with one file there
is nothing to cross-check, because nothing can drift).

Three things it breaks that the existing path cannot:

1. **Desktop Audio captures the whole machine.** A Discord notification during a clip is in the clip.
   Application Audio Capture targeting `hl.exe` avoids this and is the right recommendation, with the
   caveat that it is a newer Windows capture method. *(Unverified against this setup.)*
2. **`stopsound` fires audibly.** `sys_record_start` is `mirv_recordmovie_start; stopsound`, and
   `sys_sound` fires a `stopsound` a second before it. Under HLAE those land outside or at the very
   edge of the captured audio; under OBS the microphone is already open, so a `stopsound` cutting
   sustained sounds is *recorded*. Whether the pre-roll's `stopsound` is needed at all once nothing is
   being time-warped through the record window is worth asking — it exists to fix fast-forward audio
   corruption, and Option A starts OBS before the flush anyway, putting it in the discarded head.
3. **Nothing enforces that audio was captured.** A muted source produces a perfectly valid file.
   Hence the audio-stream test in the predicate above.

---

## Batch lifecycle: what must not be forgotten

The capture engine's failure and cancel paths currently assume the only external process worth
worrying about is one it spawned. OBS is not.

- **Cancellation.** `taskkill /F /IM hl.exe` fires and the batch unwinds — with OBS still recording,
  forever, into the user's drive. `StopRecord` must be part of the cancel path.
- **Crash.** The engine already distinguishes "hl.exe never started", "crashed mid-capture" and "exit
  trigger seen". Every one of those exits needs the same `StopRecord`.
- **`CaptureCleanupGuard`.** This is the right home for it: it already runs on drop for every path out
  of the batch, which is exactly the guarantee needed. It is not currently async and the WebSocket
  client is — worth resolving deliberately rather than by standing up a runtime inside a `Drop`.
- **Preflight, and fail fast.** OBS not running, WebSocket refused, wrong password, already recording,
  no source pointed at the game — every one of these otherwise produces a batch that runs to
  completion and captures nothing. The existing pre-launch drive-headroom check is the precedent:
  refuse before spawning, not after.
- **Disk accounting is wrong for this path.** `build_batch_queue` bin-packs blocks across drives by
  estimated BMP footprint, and the engine re-validates `drive_headroom` before launch. OBS output is
  one or two orders of magnitude smaller and lands wherever OBS points, so those estimates would
  refuse batches that would fit comfortably. The check should move to OBS's own record directory with
  an estimate sized to this path.
- **`_route_N` junctions and `mirv_movie_filename` become dead weight** — HLAE writes nothing. They
  can stay (harmless, and the `DOD_TOOLS_EXIT_TRIGGER` alias still uses the mechanism) or be skipped;
  no reason to touch them in a first version.
- **Capture modes are mutually exclusive.** Frame sequence / direct-to-video / OBS is a three-way
  choice, not three checkboxes. `ffmpeg_capture` and an OBS mode must not both be settable.
- **`MIN_TAKE_SEPARATION_SECONDS`** (1.0s) exists because a `mirv_recordmovie` stop/start cycle that
  tight risks a take landing without audio. The rule transfers, and **the number does not**: OBS took
  **~1.065s** to finalise a file after `StopRecord` returned, measured twice. That is already longer
  than the separation the merge rule guarantees, so on this path the constant must be raised — two
  highlights that merge today would otherwise produce a stop/start cycle OBS cannot service.

---

## Every way a batch can end, and what stops OBS in each

Worked through 2026-08-28 after the first end-to-end test, because "the cancel button stops OBS" is
only one of the ways a batch stops. The question that matters for every row is the same: **does
anything survive to send `StopRecord`?** Where nothing does, OBS records until the drive fills.

| How it ends | What the engine sees | What stops OBS |
| --- | --- | --- |
| Cancel Batch | Cancel token → `taskkill` → break | `session.finish()`. Verified. |
| `quit` in console | hl.exe gone, no exit trigger | Same path, via the crash branch |
| ALT+F4 | Identical to `quit` | Same |
| End Process hl.exe | Identical to `quit` | Same |
| **`disconnect` in console** | **Nothing — hl.exe is alive and idle at the menu** | **Marker-silence watchdog** |
| Demo fails to load, or ends early | Same as `disconnect` | Same watchdog |
| hl.exe freezes | Same as `disconnect` | Same watchdog |
| OBS closed or crashes | Requests start failing | One reconnect, then abort the batch |
| dod-tools panics | — | Nothing. `panic = "abort"`, no unwind, no `Drop` |
| dod-tools force-killed, or power cut | — | Nothing |

**`disconnect` was the real gap.** Every guard in the capture loop keys off hl.exe being gone or a
file appearing, and `disconnect` produces neither: the process sits happily at the menu while the
batch is over. The signal that was missing is the one already being written — `BREADCRUMB` fires
every 5000 ticks throughout playback regardless of blocks, so its *absence* is the stall.

The threshold cannot be derived from ticks. Demo ticks are frames, so the wall-clock spacing of a
breadcrumb interval depends on the fps the demo was recorded at, and **an unfocused game stops
fast-forwarding entirely** (see `docs/goldsrc_dod_quirks.md`), which stretches a gap of seconds into
minutes with nothing wrong. Hence a five-minute floor plus an adaptive term of three times the
longest gap the batch has already survived — a batch that has demonstrated a four-minute gap is not
stalled at five. Tripping it kills the game and loses the batch, so it is biased towards waiting too
long.

**The last two rows cannot be fixed on the way out.** Release builds set `panic = "abort"`, so a
panic runs no destructor, and a force-quit or a power cut runs nothing at all. From outside the
process all three are the same event: dod-tools is gone and OBS is still recording. So the recovery
is on the way *back in* — `obs::recover` asks OBS at start-up whether it is recording into a folder
shaped like `<take>/take0000/all`, which is a path only this app produces, and offers to stop it and
fold the file. Anything else is somebody's own recording and is left alone.

Two smaller ones found on the way:

- **An OBS killed mid-recording truncates a plain MP4 — it does not destroy it.** Measured on OBS
  32.2.2, 2026-08-28, against a real killed recording: the salvaged file played, decoded cleanly up
  to the moment OBS died (4.2s of an 11s block), and reported only `partial file` with one broken
  audio frame at the tail. The received wisdom that an unfinalised MP4 is a total loss did not hold
  here. MKV and the hybrid/fragmented MP4 variants still lose nothing at all, so preflight
  recommends them — but as a preference, not a rescue. `fold_into_take` keeps whatever container OBS
  wrote either way.
- **A salvaged block's reported duration is wall-clock, not file length.** 11.0s reported against a
  4.2s file, because the seconds between the failure happening and being noticed are counted but not
  recorded. `RecordedBlock::salvaged` marks these; anything comparing a duration must check it.
- **Automatic file splitting silently truncates a block.** `StopRecord` reports only the last piece,
  so everything before it is stranded under a name nothing downstream resolves. Preflight warns.

---

## Where this touches the codebase

- **`native/src/capture_engine.rs`** — the batch loop gains a log tailer and an OBS client;
  `CaptureCleanupGuard` gains `StopRecord`; the pre-launch checks gain a preflight.
- **`native/src/obs/`** (new) — WebSocket client, handshake, request/event types. Behind
  `#[cfg(not(target_arch = "wasm32"))]` like the rest of the process/IO surface.
- **`native/src/patch/types.rs`** — `PatcherConfig` gains the OBS settings beside `ffmpeg_capture`
  and `ffmpeg_capture_codec`, which are the pattern to copy.
- **`native/src/hlcr/scanner.rs`** — `is_renderable_take` gains the video-with-audio case;
  `avi_frame_count` needs a companion if the container is not AVI.
- **`native/src/hlcr/renderer.rs`** — a trim branch on the existing video-input path.
- **`desktop-studio/src-tauri/src/capture_manager.rs`** — `VerifiedBlock`'s two tiers gain the
  duration assertion; settings plumbing for host/port/password.
- **`desktop-studio/src/`** — capture-mode selector, connection settings, preflight report. Every
  `invoke()` needs its `.catch()`.
- **`native/src/strings.rs`** — new user-facing strings go here, per the centralisation pass.

Nothing in `dod/`, `analysis/`, `dem-patch/` or the patcher's frame-writing code is affected. This
feature does not change a single injected frame — which is the strongest argument for it being a
tractable piece of work.

---

## Staging

0. ~~**Talk to OBS.**~~ **Done 2026-08-28.** `probe_obs obs` — every needed request is present, and
   the start latency is 59–69 ms. See "Measured against a live OBS".
1. ~~**Prove Game Capture actually delivers frames.**~~ **Done 2026-08-28.** Both hooks coexist and
   the captured frames contain the game. Method worth reusing:
   `tools/ffmpeg.exe -i <file> -vf blackdetect=d=0.05:pic_th=0.98 -an -f null -`, then extract one
   frame and look at it — a valid-but-empty recording is the failure mode here, and only the second
   step catches it.
2. ~~**Tail the console log.**~~ **Done 2026-08-28.** 21–40 ms marker latency across 17 blocks under
   the heaviest I/O configuration. The channel works; the `screenshot` fallback is not needed.
3. ~~**Wire one block end to end.**~~ **Done 2026-08-28.** Log tail → `StartRecord` on `AUDIO_SYNC`
   → `StopRecord` on `STOP_RECORD` → folded into the take folder. Verified against a real batch:
   blocks land as `<take>/take0000/all/video.mp4`, decode clean, and carry their audio.
4. ~~**The predicate**~~ **Done 2026-08-28.** Both sides at once, testing for the audio *stream* —
   a muted OBS source writes a perfectly valid silent file, and admitting it would render a clip
   with no sound and no error.
5. ~~**Preflight, cancel and crash paths.**~~ **Done and tested live 2026-08-28.** See the failure
   table above. `MIN_TAKE_SEPARATION_SECONDS` is mode-aware via `OBS_TAKE_SEPARATION_SECONDS`.
6. **The trim pass in Render Studio**, and the setting to skip it. *Not built.*
7. **Measure it.** Wall-clock and disk against a real batch, next to the same batch through the
   existing path. The speed claim is still an expectation. *Not done.*

---

## Open questions

- ~~**`qconsole.log` flush cadence.**~~ **Answered 2026-08-28.** 21–40 ms across a real 17-block
  batch. The `screenshot` fallback is not needed and was never built.
- **Frame pacing under real playback.** The delivery test was taken at the menu, where nothing is
  moving. Whether a demo playing at `host_framerate 0` on a loaded machine produces dropped or
  duplicated frames is a different question, and it is the one that decides how good this path
  actually looks. **Still open** — the captured clips decode clean and run the right duration, which
  says nothing about frames OBS silently duplicated to fill time.
- ~~**Which container.**~~ **Settled, and the worry was overstated.** A plain MP4 from an OBS killed
  mid-recording is *truncated, not destroyed*: measured, it played, decoded to the moment OBS died,
  and reported only `partial file` with one broken audio frame. So the container choice is a
  preference — MKV and the hybrid/fragmented variants lose nothing, plain MP4 loses the tail — not a
  correctness gate. What remains is that MP4 needs a second frame-count reader beside
  `avi_frame_count`, while Custom Output writing `utvideo` into an AVI reuses the existing one.
- **Is `stopsound` still wanted in the record window** once nothing is being time-warped through it?
- **What happens when the machine cannot keep up.** Promoted from a nicety to the question that
  decides who this path is usable for — see the break-even table above: the machines where OBS saves
  the most wall-clock are the machines where OBS drops the most frames. Dropped frames are invisible
  in the output file's metadata: the duration is right and the frames are simply missing.
  `GetRecordStatus` does not report skipped frames; OBS's own stats do. Whether that is reachable
  over the WebSocket, and whether a batch should warn or refuse, is unanswered.
- ~~**Whether OBS should be driven at all when it is already streaming.**~~ **Settled: it refuses.**
  `ObsSession::start` fails the batch on an active stream rather than warning, because the failure
  would be visible to the user's audience rather than to them. It refuses an already-running
  recording the same way.
- **The stall watchdog is the one failure path never triggered live.** Everything else in the table
  above has been tested against a real batch. Reproducing this one means a demo that fails to load
  with hl.exe still alive, plus a five-minute wait; it did arm correctly off a real run in the
  activity log, which is weaker evidence than the rest.
