# The HLTV viewmodel animation fix

> **Status 2026-09-08 — working and live-proven across every weapon class.**
> Lives in `goldsrc-hooks/src/anim_fix.rs`, on branch
> `feat/goldsrc-hooks-companion-dll`. Tracked by
> [#204](https://github.com/ccoventry/dod-tools/issues/204).
> Defaults **off**. It is a cvar: turn it on in the console with
> `dodtools_hltv_animation_fix 1`, on the launch line with
> `+dodtools_hltv_animation_fix 1`, or from any `.cfg` the session execs.

Watching a DoD demo in first person, the weapon on screen barely moves. It does
not recoil when the player fires, does not reload when they reload, and does not
change when they switch weapons. This is what that costs to fix, and why the fix
is shaped the way it is.

For how the DLL gets a foothold in `client.dll` at all, see
[`goldsrc_client_dll_internals.md`](goldsrc_client_dll_internals.md). This
document is only about what the fix does once it is running.

---

## 1. Why the animations are missing

Structural, not a bug. GoldSrc's weapon event scripts animate the viewmodel only
for the **local** player — the `EV_IsLocal` check at the top of every
`events/weapons/*.sc`. Everyone else gets the sound and the muzzle flash but no
first-person animation, because under normal play nobody is ever looking down
somebody else's sights.

Spectating in-eye is exactly the case that assumption does not hold for. The
viewmodel on screen belongs to a player who is not the local player, so firing,
reloading and drawing never reach it.

## 2. There is nothing to replay, so it has to be inferred

The obvious fix would be to replay whatever the demo recorded. It recorded
nothing. `analysis/examples/weapon_anim_probe` counts both carriers of a
viewmodel animation in a demo:

| | `Dem_WeaponAnim` frames | `svc_weaponanim` |
| --- | --- | --- |
| POV demo of one half | 3391 | 743 |
| **HLTV demo of the same half** | **0** | **0** |

Zero of either. An HLTV recording carries no viewmodel animation for the players
it is watching, so inference is not a shortcut here — it is the only route. That
measurement is what justifies everything below.

## 3. The signal: the spectated player's own body

DoD's player models name every sequence `<stance>_<weapon>_<action>`.
`models/player/us-inf/us-inf.mdl` has **345** of them:

```
stand_bolt_shoot   crouch_bar_reload   bipod_mg_shoot
prone_webley_reload   sprint_sten_aim   sandbag_30cal_shoot
```

So the spectated player's own body animation states outright what they are
doing — and `curstate.sequence` is **replicated**, which almost nothing else
about another player's weapon is. It survives into an HLTV demo, and it names
the stance and the bipod state as well as the action.

Between shots the server returns the player to `*_aim`, so repeated single shots
show up as real sequence *changes*. That is the whole trigger mechanism: watch
`curstate.sequence` change, read the new label, classify it.

## 4. What runs each frame

`apply()` is installed as a per-frame callback (`engine::set_per_frame_callback`,
driven by the `HUD_Frame` hook) and walks a fixed sequence of preconditions. Each
has a named stage, and the stage is logged **only when it changes** — this runs
60+ times a second, so a line per call would flood the log and slow a capture.

```
disabled → no engfuncs → not spectating → no viewmodel entity
        → no viewmodel model → no spectated player → viewmodel mismatch
        → RUNNING
```

Reaching `RUNNING` means the preconditions held. It does **not** mean anything
was corrected — that is what the animation counter is for (§8).

Once running, on each frame it:

1. reads the spectated player's body sequence label,
2. resolves bipod state (§7) if this weapon has two families,
3. publishes the current spectated index, viewmodel and deploy state for the
   sound-driven fire trigger, which runs on the same thread but from a different
   hook and has none of this context,
4. decides which animation, if any, to force.

## 5. The four animations, and what triggers each

| animation | trigger |
| --- | --- |
| **shoot** | the body sequence changes to a `*_shoot` (or `*_roll`) label — plus a second, sound-driven trigger, §6 |
| **reload** | the body sequence changes to a `*_reload` / `*_zoomload` label |
| **draw** | the viewmodel *settles* on a different weapon, §9 |
| **idle** | the camera switches to a different player, so the new viewmodel does not inherit whatever sequence the last one was left on |

Sequence lookup is exact-match first, substring fallback. That ordering matters:
a bare substring search for `reload` picks `reload_empty` on the models that have
both. Candidate names come from a dump of all 41 `v_*.mdl` sequence lists rather
than from assumption, because DoD is not consistent — a firing animation is
`shoot` on the 98k, `shoot1` on the Garand, `up_shoot` on the BAR, `launch` on
the rocket weapons, `fire` on the mortar, `throw` on grenades and `slash1` on
melee.

## 6. Two firing triggers, and the window between them

The body sequence cannot cover a held trigger. The server sets the body to
`stand_mg_shoot` once and leaves it there for the whole burst, so every round
after the first has no sequence change to key off, while the *sound* fires per
round. So there are two triggers:

- **body sequence** — catches semi-auto fire and the first round of a burst.
- **`EV_PlaySound`** (via `sound_fix`'s hook) — catches every round of automatic
  fire. Its `ent` argument is the shooter: verified against 34 matches across
  five different spectated players, with real varying indices.

A single shot reaches both, milliseconds apart, so `claim_fire` holds a
**0.03s dedup window**. That number is not arbitrary: the MG42's ~1200rpm puts
0.05s between rounds, so a wider window would swallow real automatic fire, which
is the one case the sound trigger exists for.

Measured live on 2026-09-08 (BAR + MP40, 100+ animations), the split behaves
exactly as designed: a burst's first round is claimed by both (sound plays it,
body deduped ~6 ms later) and every round after is sound-only, landing at the
weapon's cyclic rate — MP40 rounds animated 0.097–0.103 s apart against a
measured 98 ms cyclic, BAR at ~0.13 s against 122–132 ms. Totals were 19 sound /
2 body / 4 deduped for automatics, versus 5 / 5 / 3 for a semi-auto session.

## 7. Bipod weapons

MG42, MG34, BAR and Bren keep two parallel sequence families in one model — "up"
(hip-fire) and "down" (deployed), `upidle`/`downidle`, `up_shoot`/`down_shoot`.
Every animation above is looked up within whichever family is current.

State is read from the **body sequence label first** (`bipod_` / `sandbag_`
prefixes), which carries it in every stance. The `p_mg42bu` / `p_mg42bd` model
name is only the fallback, because DoD ships far more `p_` models than weapons —
they encode stance too (`p_mg42pr`, `p_mg42sr`, `p_brenbr`, `p_bren_l`) — and
the deploy marker goes unreadable exactly when a machine gunner is prone. When
neither source reads, the last observed state carries forward; it is cleared on a
player switch, because it says nothing about the next person.

This part matters far less in practice than the plain animations: league configs
generally limit the MGs to zero and deploying the BAR's bipod is rare. It is a
refinement on top, not the point.

## 8. Diagnostics

- `dodtools_hltv_animation_fix <0|1>` — a **cvar**, so it also takes
  `+dodtools_hltv_animation_fix 1` on the launch line or a line in any `.cfg`,
  and shows its value in the console type-ahead.
- `dodtools_log_weapon_model <0|1>` — cvar. Logs every held-model change *and*
  every body-sequence change, which is the trail to read a session back from.
- `dodtools_status` — what each fix is *doing*, not just what it is set to. A
  cvar can answer "what is this set to" on its own; whether the fix's
  preconditions are being met in the current view is a different question, and
  this is where it is answered.
- Log file: `%APPDATA%\dod-tools\logs\dodtoolsHookGoldSrc.log`, with wall clock **and** a `[demo NNN.NNN]`
  prefix. Read it directly.
- **"animations corrected" counter** — the honest number. A running total is
  printed every 100 animations.

**Before concluding something is not firing, check whether its log budget ran
out.** Two separate sessions were misread as "the fix doesn't work" when the code
was fine and only the *log* had stopped: a single shared 40-line animation cap
was consumed by draws and player switches 44 seconds in, and a single 25-line
fire-sound cap was consumed by "ignored" lines from other players. Caps are now
per category, and the counter is the thing to trust.

## 9. Four things that were got wrong, and are worth not repeating

**Seven weapons name their `v_` and `p_` models differently.** The "is the
viewmodel the weapon this player holds" filter compares stems as substrings, and
these share nothing:

```
v_98k → p_k98            v_scoped98k → p_k98s       v_mp44 → p_stg44
v_greasegun → p_grease   v_m1carbine → p_m1carb
v_panzerschreck → p_pschreck        v_enfield_scoped → p_enfields
```

Every frame was discarded for all seven — 7139 in one session for the STG44
alone — silently disabling draw, reload and the body-sequence fire trigger.
Only the sound trigger survived, which is why it presented as "the STG44 has no
draw animation" rather than a whole weapon being skipped. Fixed via
`VIEWMODEL_ALIASES`; an unmatched pair is now logged **unconditionally**, not
behind the verbose switch, because nobody would think to turn logging on for a
weapon they had no reason to suspect.

**Rapid weapon switching is real input, not engine noise.** The viewmodel pointer
flaps between a player's weapons many times a second, and a 0.4 s settle window
was added to suppress it. That was wrong: the uncapped logs show the spectated
player's **body sequence changing on the same frame** as the held model —
`stand_pistol_aim` ↔ `stand_rifle_aim` tracking `p_colt` ↔ `p_garand`, six times
in 1.5 s — and nothing confined to the viewmodel could move a player's body
animation. A draw per switch, each cut short by the next, is what a POV recording
shows, so that is what to reproduce. `VIEWMODEL_SETTLE_SECONDS` is now **0.05 s**,
only absorbing a change-and-change-back inside a frame or two. At 0.4 s a
kar→pistol→kar flick produced no draw at all.

**`HUD_Frame(double time)` is a frame *duration*, not a clock.** It receives
`host_frametime` (6–11 ms), and reading it as elapsed time cost a full test run:
the fire-dedup window compared 0.006 against 0.006, sat permanently inside its
own 0.03 s window and suppressed every shot. It is also why every `[demo N]` log
prefix read `[demo 0.006]` no matter how far into the demo. Time is now summed
from the deltas.

**The grenade body sequence is the release, not the pin pull.** The steady 0.49 s
between the thrower's body sequence and `weapons/grenthrow.wav` (846 throws
across three HLTV halves, 0.461–0.566 s) looked like a pin pull, and the throw
was deferred by it. Shipped and reverted. The tightness was the tell: the *real*
cook time, from a POV demo's own `pinpull`→`throw` animations, is 0.065–4.852 s
with medians of 0.64 and 1.46 — a held button, widened further by players
"priming" grenades. The 0.49 s is the throw animation's own wind-up before the
grenade leaves the hand. Play `throw` immediately on the body change. See
`analysis/examples/grenade_timing_probe`.

## 10. Where the boundaries are

Three things are *not* missing from the fix, and are settled rather than open:

- **The grenade pin pull is unreachable.** `p_grenade`, `p_stick` and `p_mills`
  carry one `idle` sequence each, and `weapons/grenpinpull.wav` appears in no
  demo's `svc_sound`, POV included — it is played client-side for the local
  player only, exactly like the animation it accompanies.
- **Sprint does nothing to the viewmodel.** DoD 1.3 does not lower or hide the
  first-person weapon while sprinting; what changes is the *body*, which the
  engine already animates. See `goldsrc_client_dll_internals.md` §8.
- **The `exploding_` grenade family is a second weapon class**, and an HLTV
  recording carries nothing that distinguishes it — same models, same body
  token. Keep playing the plain family. See §9 of the same document.

One genuine `TODO` remains, marked in the source: on a bipod deploy state change
the viewmodel snaps to the new family's idle rather than playing the model's own
`uptodown` / `downtoup` transition. Unverified live.

## 11. How to check it is working

1. Build for `i686-pc-windows-msvc` and inject into the **PRE-Anniversary for
   Movies** install (never the stock Half-Life one — see
   `docs/goldsrc_dod_quirks.md` and the two-installs rule).
2. `dodtools_hltv_animation_fix 1`, `dodtools_log_weapon_model 1`.
3. Play an HLTV demo in-eye and let the director move between players.
4. Read `%APPDATA%\dod-tools\logs\dodtoolsHookGoldSrc.log`. The lines that matter, in order of value:
   - `now spectating … holding … viewmodel "…"` on every camera switch,
   - `body sequence -> stand_bar_reload (index N)` as the player acts,
   - one line per animation forced, with the sequence label it chose,
   - the running total every 100 animations.
5. If a weapon looks dead, check for an unmatched-pair line before anything else
   — that failure is silent and total.
