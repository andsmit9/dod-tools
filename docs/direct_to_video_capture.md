# Direct-to-Video Capture (`mirv_movie_ffmpeg`)

> **Status 2026-08-28 — shipped and verified in game. Merged to `dev` in PR #68.**
> Tracks [#42](https://github.com/ccoventry/dod-tools/issues/42), which stays open for what remains.
>
> **Working end to end:** capture through `mirv_movie_ffmpeg`, take verification, Render Studio
> scanning and rendering, frame counts and render progress from the AVI header, a lossless capture
> codec dropdown, and **Separate HUD in both capture modes** — including a real HUD alpha matte,
> which needed `-afxForceAlpha8 1` (see "Separate HUD does not survive `mirv_movie_ffmpeg`" below;
> that section title is now historical, the fault is fixed).
>
> **Still open:** the stream-copy fast path — remuxing instead of re-encoding for the simple
> non-HUD case, which was the headline win — is not built; every take still goes through a full
> render. Capture-time codec throughput is unmeasured. The launch-flag set is not bisected
> ([#69](https://github.com/ccoventry/dod-tools/issues/69)).
>
> Read `docs/goldsrc_dod_quirks.md` and `docs/hlae_protocols.md` first — the engine and HLAE facts
> this rests on are recorded there and must not be re-derived.
>
> The sibling direction, OBS as an alternate capture method, is
> [#65](https://github.com/ccoventry/dod-tools/issues/65) and is next. Scoped 2026-08-28 as a
> lower-quality convenience option rather than a replacement, with **Separate HUD explicitly out**.
> It still has to answer how a non-BMP capture artefact flows through take verification, Render
> Studio's admission predicate and export routing — questions this document already answers for the
> FFmpeg path, and whose answers likely transfer. Written up in `docs/obs_alternate_capture.md`.

---

## What HLAE actually offers

`mirv_movie_ffmpeg` makes HLAE pipe rendered frames **into an FFmpeg process it spawns itself**.
That answers #42's original open question — it is neither a named pipe we build nor a watcher that
tails the BMP sequence as it lands. Two subcommands:

    mirv_movie_ffmpeg <stream-or-group> enabled 0|1
    mirv_movie_ffmpeg <stream-or-group> options <sOptions>

Groups are `all`, `allColor`, `allMain`, `allWorld`, `allEntity`, `allDepth`. Individual streams are
`main`, `mainRight`, `world`, `worldRight`, `entity`, `entityRight`, `depthMain`, `depthMainRight`,
`depthWorld`, `depthWorldRight`, `hudColor`, `hudAlpha`, `debug` — so the separate-HUD path this
pipeline already drives (`mirv_movie_separate_hud`) maps onto `hudColor`/`hudAlpha` directly.

The options string is the tail of an FFmpeg command line, with two tokens:

| token | meaning |
|---|---|
| `{AFX_STREAM_PATH}` | where that stream's output goes |
| `{QUOTE}` | a literal `"` |

The wiki's own example:

    mirv_movie_ffmpeg all enabled 1
    mirv_movie_ffmpeg all options "-c:v rawvideo {QUOTE}{AFX_STREAM_PATH}\video.avi{QUOTE}"

There is also an `optionsEx` subcommand for finer control; `mirv_movie_ffmpeg main optionsEx` with
no value prints the current defaults as a worked example.

**Version support.** Added to AfxHookGoldSrc alongside the AfxHookSource feature. Issue `#842`
(GoldSrc FFmpeg videos flipped) regressed it from 2.18.2 and is fixed. The working install here is
**2.25.3** (2026-07-10), well clear of that.

---

## What we know without running it, and what needs the game

Two facts are already settled by evidence rather than assumption:

- **Audio is not part of this.** `mirv_movie_export_sound` is a separate mechanism with its own
  cvar, and the FFmpeg path carries video only. The WAV also cannot be a second FFmpeg input,
  because it does not exist yet when the process starts.
- **Sound export is on by default.** The pipeline never sets `mirv_movie_export_sound`, and every
  take in the library contains a `sound.wav`. That is the default answering for itself.

### Answered by the probe, 2026-08-27

Two clips captured with `DOD_FFMPEG_CAPTURE` set. **Every answer is the favourable one**: the
output layout is byte-for-byte the shape the BMP pipeline already produces, with the frame sequence
replaced by a single file.

    session_20260827_220151/
      chain_01_b0/
        take0000/
          all/
            video.avi      ← was all/00000.bmp, 00001.bmp, ...
          sound.wav        ← unchanged, same place as always

1. **`{AFX_STREAM_PATH}` resolves to the per-stream folder** — `take0000/all/`, exactly where the
   BMP sequence goes. `all` is the stream name, which is why `ClipData::img_folder` already holds
   that string.
2. **It follows `mirv_movie_filename`.** The take landed under the routed session folder, so the
   `_route_N` junction routing keeps working untouched. This was the load-bearing unknown and it
   came out the right way — no long-path problem, no options-string surgery.
3. **Takes are still auto-numbered** into `take0000`, so `hlcr::scanner`'s trailing-`take*` handling
   and `shared::paths::take_key` are unaffected.
4. **`sound.wav` still lands** in the take folder, one level above the stream folder, as before.

The file is valid and correctly formed: `rawvideo`, `bgr24`, 1280x720, **120 fps**, 11.50s. That
duration matches `sound.wav` exactly — `(1014348 - 44) / 88200 = 11.500s` — so audio and video came
out the same length, which is the cross-check `docs/hlae_protocols.md` recommends and the thing the
FPS-mismatch bug used to break.

**`take-verify` reported `2/2 captured, 0 renderable`, and that is correct rather than a fault.**
`is_renderable_take` requires a subfolder containing `00000.bmp`; a video take has no such thing.
The predicate is shared with Render Studio's own admission check precisely so the two cannot
silently disagree, and here it is doing its job — saying, accurately, that Render Studio cannot yet
consume what was captured. Teaching it the new shape is stage 4, and both sides change together.

### What a codec choice is worth

Three seconds of the captured footage, re-encoded (a BMP frame at this resolution is ~2.76 MB, so
`rawvideo` stands in for the size of the sequence being replaced):

| encoding | 3s of footage | vs the BMP sequence |
|---|---|---|
| `rawvideo` (and the BMP sequence) | 995 MB | 1.00x |
| `utvideo` | 486 MB | 0.49x |
| `ffv1 -level 3` | 420 MB | 0.42x |
| `libx264 -qp 0` | 267 MB | **0.27x** |

So a **lossless** intermediate is already a ~2-4x saving with no quality cost at all, which is the
headline: this does not require accepting worse output. Anything lossy is far smaller again, and is
the trade the people in [#65](https://github.com/ccoventry/dod-tools/issues/65) are already making
deliberately.

**These are transcode sizes, not capture-time measurements.** HLAE pipes frames to FFmpeg live, so
the encoder has to keep up with capture — `utvideo` is built for exactly that and `ffv1` is not
especially fast. Which of these is viable *during* a capture is a separate question this probe did
not ask.

---

Everything below needed one real capture to settle, and none of it could be reasoned out of the
bytes. Kept as written because the questions are what the probe was designed around:

1. **What `{AFX_STREAM_PATH}` resolves to.** The take folder, or a per-stream subfolder inside it?
   This decides the output layout, whether the video lands as a sibling of `sound.wav`, and how much
   of Render Studio's scanner has to change.
2. **Whether it follows `mirv_movie_filename`.** This is the load-bearing one. The whole batch
   pipeline routes output by aliasing `mirv_movie_filename` per block into `_route_N` junctions, to
   stay under GoldSrc's path limits. If `{AFX_STREAM_PATH}` derives from `mirv_movie_filename`, that
   routing keeps working untouched. If it is absolute or configured separately, the long-path
   problem the junctions exist to solve comes straight back — and this time inside an options string
   that also has to hold an FFmpeg command line.
3. **Whether takes are still auto-numbered.** HLAE writes into `take0000`, `take0001`, … under
   whatever `mirv_movie_filename` names. Does FFmpeg mode keep that, or write a flat file?
   `hlcr::scanner`'s "skip a trailing `take*` component" logic and `shared::paths::take_key` both
   assume it.
4. **Whether `sound.wav` still lands beside it**, and in which of the two folders above.

---

## The constraint that decides where these commands go

**The options string must be set once, at load, from a config file — never as an injected
`ConsoleCommand` frame.** This is the same rule `r_decals` follows, for two independent reasons:

- GoldSrc's `Cbuf_AddTextToBuffer` limit is 64 bytes per injected command, and an FFmpeg command
  line plus a path is several times that. There is no staggering trick that helps: it is one
  argument to one command.
- An injected `ConsoleCommand` frame shifts every later frame ordinal by +1 and desyncs the
  scheduled capture commands. See the decal-ring entry in `docs/goldsrc_dod_quirks.md` for how that
  failure looks — a capture that completes normally and is wrong.

So this belongs in `dodtools_helper.cfg`, alongside the existing aliases, executed via the
`+exec dodtools_helper.cfg` already on the HLAE command line. Path escaping in it follows the
existing rule: forward slashes become `\\\\`.

---

## What the prize actually is

**Not** skipping Render Studio — the separate `sound.wav` means a mux pass is still required. The
prize is deleting the BMP sequence.

A 15-clip session at 1280x720 currently writes tens of thousands of bitmaps. That is the dominant
disk cost of a capture batch, and it is the reason `build_batch_queue` does first-fit-decreasing
bin-packing across a pool of drives at all. Replacing it with one video file per stream changes:

- **Disk**, by roughly the compression ratio chosen (see below).
- **The render pass**, from decoding a BMP sequence to a stream copy plus audio — seconds instead of
  minutes per take.
- **Capture wall-clock**, probably. Writing thousands of small files is not free, though how much of
  the capture time that actually accounts for is unmeasured and should not be assumed.

**The codec choice is the whole story on size.** The wiki example uses `-c:v rawvideo`, which is
essentially as large as the BMPs it replaces — fine as a first correctness test, useless as the
shipped default. A lossless-but-fast intermediate (`utvideo`, `ffv1`, or `libx264 -qp 0`) is the
realistic target, and picking it is a quality decision that wants the user's judgement, not a
default chosen here. It is also the same decision that makes #65 attractive to people who are happy
to trade quality for speed.

---

## Where this touches the codebase

- **`native/src/patch/builder.rs`** — `final_init_commands` and the `dodtools_helper.cfg` writer.
  The `enabled` and `options` lines go here. `mirv_movie_separate_hud` already branches nearby and
  decides whether `hudColor`/`hudAlpha` need their own options.
- **`native/src/hlcr/scanner.rs`** — `is_renderable_take` currently requires a `.wav` **and** a
  subfolder containing `00000.bmp`. A video take has the wav and a video file, so the predicate
  needs a second admissible shape. Its own doc comment records why that matters: it is shared with
  capture-side verification precisely so "the capture succeeded" and "Render Studio can see it"
  cannot silently disagree. Both sides change together or neither does.
- **`native/src/hlcr/renderer.rs`** — a second input branch. The `single` case becomes
  `-i video.<ext> -i sound.wav` with `-c:v copy`. The `hud_only` case is harder and may not be
  copyable at all: it currently `alphamerge`s two BMP sequences, which means decoding both videos
  and re-encoding the result.
- **`desktop-studio/src-tauri/src/capture_manager.rs`** — `take_folder_has_content` and
  `VerifiedBlock`'s two tiers, if the output layout changes shape.
- **`native/src/hlcr/take_meta.rs`** — records the capture FPS per take and warns when a render
  interprets it at a different rate. A video take should carry the same record and the existing
  `[render-fps-mismatch]` check keeps working unchanged. *Note: that module lands with
  `feature/decal-flush-r-and-d`, not on `dev` — so it is not present on this branch yet.*

---

## FFmpeg has to be reachable *by HLAE*, which is a separate question from ours

The app already resolves an FFmpeg for Render Studio through the documented waterfall (User Override
→ Bundled Local → System Path). HLAE does not consult that. It looks in exactly two places, per
`<HLAE>/ffmpeg/readme.advancedfx.txt`:

- `<HLAE>/ffmpeg/bin/ffmpeg.exe`, or
- a path named by `[Ffmpeg] Path=` in `<HLAE>/ffmpeg/ffmpeg.ini`.

**On the working install here, neither exists** — that folder contains only the readme. So this
feature does nothing at all until that is addressed, and it will fail in the least helpful way: a
capture that runs and produces no video.

**Recommended handling: detect, then offer to write the `.ini` — never overwrite one.**

- Writing a two-line `ffmpeg.ini` beats copying the binary. A copy duplicates ~100 MB, and creates a
  second FFmpeg to keep in step with the one Render Studio uses — they would drift, and the two
  halves of the pipeline would be encoding with different builds.
- It should be **offered**, not done silently. It writes into another application's install
  directory, which is not ours the way a scratch file is.
- **An existing `ffmpeg.ini` is never overwritten.** If one is present and points somewhere else,
  report the disagreement and leave it alone. HLAE is shared with other games and projects, and
  silently repointing it would break someone's Source workflow to fix ours. This is the same
  discipline `cfg_scan` applies to the game's own `.cfg` files: detect and state, never assume the
  right to decide.
- If the app's own FFmpeg resolved to a bare `ffmpeg` on `PATH`, it has to be resolved to an
  absolute path before it can be written into the ini.

---

## Staging

1. **Answer the four unknowns with one capture.** A single clip, `-c:v rawvideo`, everything else
   left alone. Record exactly where the video lands, whether `sound.wav` is beside it, whether a
   `take0000` folder still appears, and whether `mirv_movie_filename` steered any of it. Stop here
   and look — items 2 and 3 above decide how much of the rest is a small change or a large one.
2. **FFmpeg availability.** Detection, the offer, and the never-overwrite rule. Independent of
   everything else and safe to land first.
3. **Emit the commands.** `enabled` and `options` into `dodtools_helper.cfg`, behind a setting that
   is off by default. Codec choice exposed, not hardcoded.
4. **Teach the scanner the new shape**, on both sides of the shared predicate at once.
5. **The mux.** Stream-copy plus audio for the simple case. Decide separately whether separate-HUD
   is supported in this mode at first, or falls back to the BMP path — it may not be copyable.
6. **Measure it.** Disk and wall-clock, against a real batch, before and after. The claim that this
   is faster is currently an expectation, not a measurement.

---

## Open questions

- **Does it help capture wall-clock, or only disk?** Assumed, unmeasured. Worth knowing before it
  is described to anyone as a speed feature.
- **What happens when the spawned FFmpeg fails?** The changelog records a fixed deadlock when FFmpeg
  wrote to stderr and exited (`#809`), which says the failure mode was bad enough to hang the game.
  A capture where FFmpeg dies silently mid-batch needs to be detectable — a video file that exists
  but is short is exactly the kind of plausible-looking wrong output this pipeline has been bitten
  by before.
- **Does the take still verify?** `take_folder_has_content` and `is_renderable_take` are what stop a
  failed capture being reported as a success. Neither currently knows what a video take looks like.
- **Separate HUD.** Two video streams that have to be `alphamerge`d cannot be stream-copied, so the
  headline win does not apply to that path. Supported, or excluded at first?

---

## Separate HUD, measured 2026-08-27

Run with `mirv_movie_separate_hud 1` and `-c:v utvideo`, `wsod25-po_r2_qf-m00cat_clinic_m2_lenn_h2`
at 120 fps:

    chain_01_b0/take0000/all/video.avi        1187 MB
    chain_01_b0/take0000/hudcolor/video.avi    135 MB
    chain_01_b0/take0000/hudalpha/video.avi    135 MB
    chain_01_b0/take0000/sound.wav            0.85 MB

Four things settled:

1. **The `all` group reaches the HUD streams.** One `mirv_movie_ffmpeg all enabled 1` covered all
   three — no per-stream `enabled`/`options` lines needed, and no stream fell back to BMPs.
2. **HLAE writes the stream folders lowercase** — `hudcolor`/`hudalpha`, not the camelCase the
   command uses for the stream names. The scanner's lookup is case-insensitive either way.
3. **`pix_fmt` is `gbrp`** — lossless planar RGB, no chroma subsampling. This is what makes the
   `extractplanes=r` alpha path safe, and it is the constraint any capture-codec dropdown has to
   respect: a 4:2:0 codec would destroy the alpha channel silently.
4. **All three streams and the audio agree exactly** — 1218 frames, 10.150s video against 10.150023s
   of WAV. No drift to correct for.

The HUD streams compress ~9x better than `all` (135 MB vs 1187 MB), which is what a mostly-flat
overlay should do and a useful sanity signal that the streams are what they claim to be.

**`nb_frames` is in the container.** The scanner reports `frame_count: 0` for video takes because
`count_bmps` finds no bitmaps, and that gap blocks both the render progress percentage and any move
to HLCR-style frame-count pairing. FFprobe reads 1218 straight off the stream, so the number is
available without decoding — see `docs/render_studio_hlcr_parity.md`.

### Separate HUD does not survive `mirv_movie_ffmpeg`

The streams are created and the render runs clean end to end, but the HUD clip comes out as a black
rectangle. Two independent faults, separated by capturing the same demo both ways:

| stream | BMP mode | video mode |
|---|---|---|
| `all` | scene, no HUD | scene, no HUD |
| `hudcolor` | **real HUD** — min 16 / avg 17.2 / max 222 | **blank** — min = max = 16 |
| `hudalpha` | **pure white** — R/G/B all 255 | **pure white** — R all 255 |

1. ~~**`hudcolor` is dropped by the FFmpeg path.**~~ **Not a second fault — the same one.** This was
   measured before `-afxForceAlpha8 1` was being sent, i.e. with the alpha buffer broken. Re-tested
   after the fix below, video mode captures the HUD correctly: `hudcolor/video.avi` went from 135 MB
   of uniformly black frames (min = max = 16) to 428 MB with real content (min 16, max 217), and
   `hudalpha` to a real matte. All three streams are correct in video mode.

   The link is plausible in hindsight: the BMP writer dumps the colour buffer straight to disk, while
   the FFmpeg path converts frames before piping them. An alpha-aware conversion against a degenerate
   all-opaque alpha collapses `hudcolor` to black, and leaves `all` — which involves no alpha —
   untouched. Exactly the pattern that was observed.

   **Lesson worth keeping:** two symptoms measured under the same broken precondition looked like two
   independent faults, and the second was written up as "not a direct-to-video regression" on that
   basis. One fix cleared both.
2. **`hudalpha` was fully opaque in *both* modes** — every plane 255, so `extractplanes=r` could only
   produce an opaque mask and `alphamerge` could never produce transparency. Not a direct-to-video
   regression; it applied however the take was captured. **Fixed — see below.**

Worth noting `hudcolor` is black wherever the HUD is absent, which is why the standalone HLCR carries
a `detect_chromakey_color` path — keying on black is a route that does not depend on the alpha stream
at all, and remains a fallback if the alpha ever regresses.

### Fault 2, solved: `-afxForceAlpha8` takes a value

The HUD alpha buffer is off unless the *game's* command line carries HLAE's launch flags. Under
`-customLoader` HLAE composes nothing for us — its Launch GoldSrc dialog is what normally assembles
them — so none of it was ever being sent.

The part that cost three captures: **`-afxForceAlpha8` takes an explicit `0`/`1` argument.** From
HLAE's `Launcher.cs`:

    " -afxForceAlpha8 " + (cfg.ForceAlpha ? 1 : 0).ToString()

Passed bare, the hook reads the next token as its value, finds something that is not `1`, and leaves
alpha off. It does not error, and the captured bitmaps come out *byte-for-byte identical* to a run
without the flag — which is what made it look like the flag was being ignored entirely.

This is invisible to static analysis: the binary's string table gives you the switch names but cannot
tell you which ones consume an argument. Reading `Launcher.cs` is what settled it.

The set now sent, on every `-customLoader` launch:

    -gl -32bpp -afxRenderMode standard -afxForceAlpha8 1

Measured after the fix — `hudalpha` frame 600, same clip:

| | before | after |
|---|---|---|
| min / avg / max | 235 / 235 / 235 | **16 / 18.3 / 235** |
| opaque coverage | 100% | **2.8%** |

A real matte: white on the kill feed, weapon icons and objective boxes, black everywhere else.
`-afxOptimizeCaptureVis` is deliberately still not sent — a visibility optimisation, unrelated to
alpha — and the set has not been bisected to find the minimum; `-32bpp` in particular is untested on
its own, though a framebuffer without 32-bit colour has no alpha bits to force.

### Ungated, 2026-09-08

The flags were originally composed in `capture_engine` and gated on `separate_hud`, which meant they
only ever reached a **capture batch**. The two hand-driven launches — the standalone "Launch Game
(HLAE)" button and Launch Preview — pass through the same `build_hlae_process` with their own
`extra_engine_args` and got none of it, so a session started that way ran with the alpha buffer off.

That surfaced on a real capture: a 7598-frame take recorded by hand out of a "Launch Game (HLAE)"
session, with `mirv_movie_separate_hud 1` typed in the console, wrote a `hudalpha` stream that is
`255` on every plane of every frame. `extractplanes=r` then feeds `alphamerge` an all-opaque matte,
and the render produces a technically-correct ProRes 4444 whose alpha channel is uniformly 255 —
`min=255 max=255 distinct=1`. Nothing in the pipeline reported a fault; the only symptom was a HUD
that would not composite in the NLE, and the natural workaround (chroma-keying the black background)
partially keys every anti-aliased glyph edge, which reads as translucency.

HLAE's own `<Launcher><ForceAlpha>` in `hlaeconfig.xml` is **not** a fallback for this. That is the
Launch GoldSrc dialog's checkbox; `ProcessArgsCustomLoader` never reads it. It can be `true` in the
config file while every launch this app makes runs without alpha.

The flags now live in `build_hlae_process` and are sent unconditionally. The gate bought nothing:
forcing 32-bit colour and alpha costs a capture that writes no HUD stream exactly nothing, and no
code here can know what the user will type into the console mid-session.

Both faults are now fixed and Separate HUD works with direct-to-video capture. The guard that blocked
the pairing has been removed.

### Not a codec problem: one demo crashes hl.exe

`ktps8w1-stealth_soul_lenn_h1.dem` crashed hl.exe ~6s after spawn, four times running, with
Separate HUD both on and off. The same build, same codec, same settings capture
`wsod25-po_r2_qf-m00cat_clinic_m2_lenn_h2.dem` fine, so this is specific to that demo and not to
direct-to-video. Untested whether it also crashes in frame-sequence mode. Note it has a
`_forcehltv` twin of identical byte size but different content, which suggests it was already
known to need working around.
