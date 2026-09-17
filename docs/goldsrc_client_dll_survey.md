# DoD 1.3 `client.dll`: a survey of what can be controlled

A catalogue, not an implementation. Four features have been built against this
binary — the HLTV animation fix, the gunshot fix, `dodtools_deathmsg` and
`dodtools_objectives` — and each time the analysis went exactly as deep as the
feature needed and no wider. This is the wide pass: what else is in here, what
controlling it would let a movie-maker do, and how much work each would be.

Opened as [issue #255](https://github.com/ccoventry/dod-tools/issues/255).
Companion to `docs/goldsrc_client_dll_internals.md` (how the client library is
entered at all), `docs/goldsrc_death_notices.md` (the kill feed) and
`docs/goldsrc_objective_icons.md` (the objective HUD).

**Subject:** `dod/cl_dlls/client.dll`, 977,816 bytes, byte-identical across the
stock, pre-Anniversary and post-Anniversary installs. Every RVA below is for
that build.

**Method:** offline, `pefile` + `capstone`, no running game. Every table here is
produced by `goldsrc-hooks/tools/survey_client_dll.py`, which reads a real
`client.dll` — so re-checking a claim is a command, not a re-derivation:

```
python goldsrc-hooks/tools/survey_client_dll.py [elements|messages|cvars|limits|sections]
```

---

## 0. The thing that made this pass cheap

DoD's `client.dll` ships **MSVC RTTI**. 212 decorated type names sit in `.data`/`.rdata`
(`.?AVCObjectiveIcons@@`, `.?AVCHudSayText@@`, …), and walking type descriptor →
complete object locator → vftable gives 196 classes' virtual function tables by
*name*. Finding "the function that draws the objective icons" stops being a
search and becomes a lookup.

`CHudBase`'s vftable is eight slots:

| slot | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| | `~dtor` | `Init` | `VidInit` | **`Draw`** | `Think` | `Reset` | *(DoD extra)* | `InitHUDData` |

Slots 3 and 7 are not read off the HL SDK header. Two addresses `deathmsg.rs`
already patches — `+0x2ae90` as `CHudDeathNotice::Draw` and `+0x2ae50` as its
`InitHUDData` — land exactly there in that class' table, and `CHud::Redraw`
independently calls `[vftable+0xc]` on every element (§1). Slot 6 is a seventh
virtual DoD added between `Reset` and `InitHUDData` that **no element in this
build overrides**; what it is has not been established.

---

## 1. The HUD element list — the single highest-value finding

`CHud::Redraw` (`+0x36e20`) is the per-frame HUD entry point. Its last act is:

```asm
+0x36f47  mov  ecx, [esi+0x68]         ; the hud_draw cvar
+0x36f51  fld  dword [ecx+0xc]         ; ->value
+0x36f54  fcomp [0.0]
+0x36f5f  jnp  <skip everything>       ; hud_draw 0 -> no element draws at all
+0x36f61  mov  edi, [esi]              ; gHUD.m_pHudList -- the head
+0x36f63  test edi, edi
+0x36f65  je   <skip>
.loop:
+0x36f6c  mov  ecx, [edi]              ; node->element  (CHudBase*)
+0x36f70  mov  al,  [ecx+0x10]         ; element->m_iFlags
+0x36f75  test al, 1                   ; HUD_ACTIVE
+0x36f77  je   .next
+0x36f79  test byte [esi+0x58], 4      ; gHUD.m_iHideHUDDisplay & HIDEHUD_ALL
+0x36f7d  jne  .next
+0x36f7f  mov  edx, [ecx]              ; the element's vftable
+0x36f81  push ebp                     ; flTime
+0x36f82  call [edx+0xc]               ; slot 3 -- Draw
.next:
+0x36f91  mov  edi, [edi+4]            ; node->next
+0x36f96  jne  .loop
```

Elements put themselves on that list through **`CHud::AddHudElem`**
(`+0x22330`), which `malloc`s an 8-byte `{ CHudBase* element; node* next; }` and
appends it at the tail. It has **22 callers**, and that is the complete
inventory:

| class | vftable | `Draw` | size | hooks these user messages |
| --- | --- | --- | --- | --- |
| `CHudAmmo` | `+0xac29c` | `+0x28b00` | 2604 | `AmmoPickup` `AmmoShort` `AmmoX` `CurWeapon` `HideWeapon` `ItemPickup` `ReloadDone` `WeapPickup` `WeaponList` |
| `CHudDeathNotice` | `+0xac1c4` | `+0x2ae90` | 775 | `DeathMsg` |
| `CHudDoDCommon` | `+0xac3bc` | `+0x2c6f0` | 412 | `CameraView` `GameRules` `ResetSens` |
| `CHudDoDCrossHair` | `+0xac134` | `+0x2cd20` | 117 | `ClanTimer` |
| `CHudDoDMap` | `+0xac398` | `+0x2e560` | 8 | — |
| `CHudDodIcons` | `+0xac350` | `+0x2dc40` | 1507 | `ClCorpse` `ClientAreas` `Health` `Object` |
| `CHudMenu` | `+0xac1a0` | `+0x3dee0` | 1281 | `ShowMenu` |
| `CHudMessage` | `+0xac20c` | `+0x25000` | 844 | `GameTitle` `HudText` |
| `CHudSayText` | `+0xac278` | `+0x459e0` | 701 | `SayText` |
| `CHudScope` | `+0xac374` | `+0x46590` | 22 | `Scope` |
| `CHudSpectator` | `+0xac254` | `+0x38000` | 879 | — |
| `CHudStatusBar` | `+0xac1e8` | `+0x477a0` | 349 | `StatusValue` |
| `CHudStatusIcons` | `+0xac158` | `+0x471c0` | 110 | `StatusIcon` |
| `CHudTextMessage` | `+0xac17c` | *(inherits)* | — | `CapMsg` `TextMsg` |
| `CHudTrain` | `+0xac230` | `+0x4c720` | 168 | `Train` |
| `CHudVGUI2Print` | `+0xac3e0` | `+0x3aa10` | 74 | — |
| `CMortarHud` | `+0xacf2c` | `+0x3e720` | 18 | — |
| `CObjectiveIcons` | `+0xac32c` | `+0x30030` | 1516 | `CancelProg` `InitObj` `PlayersIn` `ProgUpdate` `SetObj` `StartProg` `StartProgF` `TimerStatus` |
| `CClientEnvModel` | `+0xac2e4` | *(inherits)* | — | — |
| `CParticleShooter` | `+0xac308` | *(inherits)* | — | `PShoot` |
| `CVoiceStatusHud` | `+0xabb24` | *(inherits)* | — | — |
| `CWeatherManager` | `+0xac2c0` | *(inherits)* | — | — |

### What this buys: hide or replace any single HUD element

Two mechanisms, and the difference between them matters.

**(a) Overwrite the class' `Draw` slot.** `CHudBase::Draw` at `+0x21940` is
`xor eax, eax; ret 4` — a correct, complete no-op with the right calling
convention. Writing `module + 0x21940` over any class' vftable slot 3 hides that
element and nothing else. **One dword, into `.rdata`, with `patch::write_code_bytes`.**
No signature, no detour, no stub. It also survives everything: the game never
rewrites a vftable.

Replacing the slot with a stub of our own instead of the no-op is the same one
dword, and is how an element would be *wrapped* rather than hidden — call the
original, then draw over it, or the reverse.

*Technique:* immediate/data write. *Effort:* very low. *Risk:* low, but it is
per-*class*, not per-instance; nothing in this build has two instances of a HUD
class, so that distinction is currently academic.

**(b) Clear `m_iFlags` bit 0 on the instance.** Also a data write, and it is
per-instance — but **the game writes that field itself**, from `Init`, `VidInit`,
`Reset` and in several cases from `Draw` (for example `CHudAmmo::Reset`
`+0x274ec`, `CHudDodIcons::Reset` `+0x2d717`). So it would have to be re-applied,
realistically once per frame from the existing prologue. Use (a) unless
per-instance or dynamically-toggled behaviour is actually wanted.

Either way the element has to be *identified* first, and RTTI is what makes that
sound: walk the list from `gHUD+0`, and for each node compare `*(void**)element`
against `module + <vftable RVA>` from the table above. That is an exact match on
a value the compiler emitted, not a heuristic.

> **Suggested task:** a `dodtools_hudelement <name> <0|1>` command backed by the
> table above. It subsumes several separate feature requests (hide the ammo
> counter, hide the status bar, hide the crosshair) into one mechanism, and it
> is the cheapest thing in this document by a wide margin.

### `gHUD`, for reference

| field | what |
| --- | --- |
| `+0x00` | `m_pHudList` — head of the element list |
| `+0x10` (on an element) | `m_iFlags`; bit 0 = active, bit 1 = draw during intermission |
| `+0x1c` / `+0x20` / `+0x28` | `m_flTime`, previous time, frametime |
| `+0x58` | `m_iHideHUDDisplay`; bit 2 = hide everything |
| `+0x68` | the `hud_draw` `cvar_t*` |
| `+0x234` | the FOV field — 90 unless something zoomed |
| `+0x238`..`+0x254` | the two cached overview-map rects (§5) |
| `+0x6e280`..`+0x6e2b8` | fifteen cached `cvar_t*`s, in this order: `_cl_minimap`, `_cl_minimapzoom`, `zoom_sensitivity_ratio`, `hud_takesshots`, `r_drawentities`, `cl_lw`, `cl_pitchup`, `cl_pitchdown`, `crosshair`, `max_rubble`, `_ah`, `cl_corpsestay`, `developer`, `cl_hudfont`, `hud_fastswitch` |

`gHUD` itself is at `+0x1080b0`, established by `mov ecx, 0x1a080b0` at all 270
of its call sites.

---

## 2. `CHud::ShouldDraw` — you may not need a hook at all

`+0x2ca80`, `bool(int section)`, the shared per-section visibility predicate:

| section | shown when | queried by |
| --- | --- | --- |
| 1 | `cl_hud_health` ≠ 0 **and** the spectator interface is not up | `CHudDodIcons::Draw` `+0x2dd53` |
| 2 | `cl_hud_reinforcements` ≠ 0, a byte at `+0x108225` is set, and not spectating | `CHudDodIcons::Draw` `+0x2dfdb`, `CObjectiveIcons::Draw` `+0x30566` |
| 3 | `cl_hud_ammo` ≠ 0 **and** not spectating | `CHudAmmo::Draw` ×4 |
| 4 | `cl_hud_objtimer` ≠ 0 | `CObjectiveIcons::Draw` `+0x300fb` |
| 5 | `cl_hud_objectives` ≠ 0 | `CObjectiveIcons::Draw` `+0x301e7` |
| 6 | not spectating, and `cl_hud_objectives` is registered | `CHudDodIcons::Draw` `+0x2e021` |
| 7 | always | *(no call site in this build)* |

Two things fall out of this.

First, **sections 4 and 5 are the only ones with no spectator test** — which is
precisely why the objective timer and the icon row are what a spectated demo
still draws in the corner, and why they were what #254 was about.

Second, the five cvars above — plus `cl_hud_msgs`, which is registered
alongside them but queried elsewhere — are **ordinary cvars a user can already
set from a config**. Anything a movie-maker wants that amounts
to "turn that off" should check this list before reaching for a hook. What they
do *not* cover is everything else in §1's table — chat, the kill feed, the
status bar, the menu, the train, the scope — which is the gap mechanism (a)
fills.

---

## 3. Fixed-size caps, the `MAX_DEATHNOTICES` family

Found by the bulk clear that zeroes each buffer, so the count is the one the
shipped code uses rather than one read off a header.

| what | cleared at | size | reading |
| --- | --- | --- | --- |
| `CHudSayText`'s chat lines | `+0x45905` → `+0x190348` | 3072 bytes | **6 × 512** — six slots (see the two arrays below), 512 chars each |
| …its per-line name colours | `+0x458ed` → `+0x190f48` | 6 dwords | one per line |
| …its per-line name lengths | `+0x458f9` → `+0x190330` | 6 dwords | one per line |
| `CHudDeathNotice`'s feed | `+0x2ae5d` → `+0x1765d8` | 780 bytes | 5 × 156 — **already raised to 127 by `dodtools_deathmsg max`** |
| `CHudMessage`'s slots | `+0x246bd` → *(this+0x14)* | 128 bytes | 16 × 8 — `maxHUDMessages` |
| `CHudStatusIcons` | `+0x471ad` → *(this+0x14)* | 192 bytes | the icon slots |
| `CHudAmmo`'s weapon list | `+0x274b3` → `+0x102108` | 21504 bytes | the `WEAPON` array |
| `CHudSpectator`'s overview data | `+0x3789a` → *(this+0x728)* | 4096 bytes | re-cleared in `Reset` and `InitHUDData` |

**The chat cap is the direct analogue of the kill-feed work.** Five lines of
chat is thin for a match demo, and the fix is the technique `deathmsg::max`
already proved: relocate the three parallel arrays, rewrite every absolute
reference to them, and widen the loop bounds. The references are `.text`
absolutes with base relocations, exactly as they were for the notice list, so
the scan-the-whole-image-for-dwords-landing-inside method transfers unchanged.

*Technique:* immediate rewrite + array relocation. *Effort:* medium — it is a
second `deathmsg.rs`, and it needs its own verifier. *Risk:* medium; the
failure mode is 40-ish writes into unrelated instructions, which is why
`verify_deathmsg_offsets.py` exists and why a chat version would need the same.

> **Suggested task:** `dodtools_saytext max <5..N>` / `offset`, modelled on
> `dodtools_deathmsg`.

---

## 4. User messages — 71, and hooking one costs nothing

The complete table is `survey_client_dll.py messages`. The mechanism is already
proven and needs no patching at all: `pfnHookUserMsg` **prepends** a fresh
record and the engine's dispatcher stops at the first name match, so the newest
hook wins and the game's own handler stays reachable by calling its thunk
directly. That is how `dodtools_deathmsg block` and `fake` work.

`CHud::Init` (`+0x21100`) registers 32 of them centrally; the rest belong to
individual elements (see §1's table). The ones a movie-maker is most likely to
want:

| message | thunk | why |
| --- | --- | --- |
| `SayText` | `+0x45810` | filter or rewrite chat — the exact `deathmsg block` shape, one message later |
| `TextMsg` / `CapMsg` | `+0x4c030` / `+0x4c050` | the centre-screen capture and round announcements |
| `Spectator` / `AllowSpec` | `+0x20e00` / `+0x20e30` | who the spectator UI thinks is spectating |
| `RoundState` / `TimeLeft` / `ClanTimer` | `+0x20c90` / `+0x20cb0` / `+0x2cb50` | round and match clock — a cut point for automated highlight selection |
| `ScoreInfo` / `ScoreInfoLong` / `TeamScore` | `+0x20e60` / `+0x20e90` / `+0x20ec0` | live scoreboard state, without re-parsing the demo |
| `StartProg` / `ProgUpdate` / `CancelProg` | `+0x2f680` / `+0x2f6c0` / `+0x2f6e0` | capture progress bars — when a flag cap starts, advances and aborts |
| `HLTV` | `+0x20c50` | the engine telling the client it is an HLTV stream |
| `CurWeapon` | `+0x27120` | already used by `anim_fix`; listed so nobody hooks it twice |

*Technique:* message hook. *Effort:* low per message. *Risk:* low — nothing is
patched, and an unhandled message is forwarded untouched.

> **Suggested task:** a `dodtools_msglog <name>...` that dumps chosen messages
> with their payloads to the log. It is an afternoon's work, it needs no
> patching, and it would make the next three investigations of "what does the
> client actually receive" cheap instead of a demo-parsing exercise each time.

---

## 5. The overview map's geometry is cached data, not code

`CHud::ComputeOverviewMapRects` (`+0x22970`), called from `CHudDoDMap::VidInit`,
computes two rectangles from `ScreenWidth`/`ScreenHeight` and caches them in
`gHUD`:

```
+0x238 / +0x23c / +0x240 / +0x244   the small map:  x, y, w, h   (w = 0.625 * ScreenWidth)
+0x248 / +0x24c / +0x250 / +0x254   the full map:   x, y, w, h   (w = 0.24  * ScreenWidth)
```

`CHud::GetOverviewMapBounds` (`+0x22a60`, four out-params, `ret 0x10`) hands one
of the two back depending on `CHud::OverviewMapMode` (`+0x228e0`, which returns
`_cl_minimap`'s value while a spectator predicate holds and `gHUD`'s FOV field
is still 90), adding the `54 * ScreenHeight/480` spectator-bar offset to the
full map's y. Callers: `CHudDeathNotice::Draw`, `CObjectiveIcons`' map path, and
two more in the spectator GUI.

Because the rects are **cached in `gHUD` rather than recomputed per frame**,
moving or resizing the overview map is four dword writes into `.data` — no
patching, no detour. They are recomputed on `VidInit` (a resolution change or a
level load), so a session would need to re-apply.

*Technique:* data write. *Effort:* low. *Risk:* low.

---

## 6. The screen-height scaling, in one place

Every `y = round(k * ScreenHeight / 480)` in the client. This is the complete
list — ten sites, found by their `fmul [1/480]`:

| site | function | `k` | what moves |
| --- | --- | --- | --- |
| `+0x22a42` | `CHud::ComputeOverviewMapRects` | 2 | full map y |
| `+0x22ac7` | `CHud::GetOverviewMapBounds` | 54 | the spectator-bar offset added to it |
| `+0x2aeca` | `CHudDeathNotice::Draw` | 2 | kill feed y, overview-map path |
| `+0x2af02` | `CHudDeathNotice::Draw` | 42 | kill feed y, spectating — **handled by `dodtools_deathmsg offset`** |
| `+0x300a6` | `CObjectiveIcons::Draw` | 32 | the `clan_warmup_mode` VGUI panel at `+0x175c6c` |
| `+0x3012d` | `CObjectiveIcons::Draw` | 54 | objective timer y — **`dodtools_objectives timer`** |
| `+0x3024e` | `CObjectiveIcons::Draw` | 2 / 54 | icon row y — **`dodtools_objectives offset`** |
| `+0x308af` | `sub_0x306a0` | 54 | a second objective-icon path — it queries section 5 and reads `spec_pip`, like `Draw` does |
| `+0x308e0` | `sub_0x306a0` | 2 | as above |
| `+0x4c3a3` | `sub_0x4c250` | 20 | positions the same `+0x175c6c` VGUI panel as the `clan_warmup_mode` row |

Two things worth taking from this. The `54` — DoD's spectator bar height in
480-space — appears in **exactly four places**, and three of them are now
controllable (`dodtools_deathmsg offset`, and `dodtools_objectives`'s two). The
fourth, `+0x308af`, is the objective icons' overview-map path, which #254
deliberately does not reach.

And the list is *short*. Ten sites is the entire vertical-layout surface of this
HUD, which means "reposition element X" is a bounded problem rather than an
open-ended one: every remaining candidate is in this table.

> **Suggested task:** the VGUI panel at `+0x175c6c`, which `sub_0x4c250` and
> `CObjectiveIcons::Draw` both position and `CHudMenu::Draw` also writes to. It
> is the one screen-scaled position left with no control, and unlike the others
> it is a VGUI2 panel rather than a sprite draw, so it is worth confirming which
> on-screen text it actually is before costing the work.

---

## 7. Cvars: 119, and which are read per frame

`survey_client_dll.py cvars` lists every one with the `cvar_t*` global the
client keeps it in — which matters because **writing through that pointer is how
you change a cvar without going through the console**, and because a cvar read
once at `Init` cannot be changed mid-demo no matter how you set it.

The pointers group by owner, and the grouping is itself informative:

- `+0x1080c4`..`+0x108118` — `gHUD`'s own (`hud_saytext_internal`,
  `hud_saytext_time`, `hud_capturemouse`, `hud_draw`)
- `+0x1174e8`..`+0x117500` — `CHudSpectator`'s (`spec_drawnames_internal`,
  `spec_drawcone_internal`, `spec_drawstatus_internal`,
  `spec_autodirector_internal`, `spec_pip`, `spec_scoreboard`, `default_fov`)
- `+0x176330`..`+0x176368` — `gHUD`'s cached lookups, including the two that
  quit the game (§8)
- `+0x18a860`..`+0x18ab10` — the view/camera/fog/HUD-suppression block
- `+0xe8894`..`+0xe88a8` — the overview map's `cl_dmshow*` family

Read **per frame**, so settable mid-demo: `hud_draw` (`CHud::Redraw`),
`spec_scoreboard` (`CHud::Redraw`, which toggles the VGUI scoreboard on change),
`_cl_minimap`, `spec_pip`, `cl_hud_*` (through `ShouldDraw`), `r_drawentities`,
`cl_lw`, `cl_pitchup`, `cl_pitchdown`, `crosshair` (all in `CHud::Redraw`'s
enforcement).

`cl_pitchup` and `cl_pitchdown` appear **twice** in the table, at two different
globals: once as a `pfnRegisterVariable` result and once as a
`pfnGetCvarPointer` lookup. That is not a transcription slip — the client both
registers them and looks them up again for the enforcement path.

---

## 8. `CHud::Redraw`, in full

`+0x36e20`, `void(float flTime, int intermission)`, the per-frame HUD entry
point. Already partly recorded in `docs/goldsrc_client_dll_internals.md` §5 and
in issue #205; the complete order of operations is:

1. Advance `m_flTime`/`m_flTimeDelta` (`gHUD+0x1c`..`+0x2c`), clamping a
   negative delta to zero. **This is the clock the kill feed's expiry compares
   against, and it does not advance while the console is down** — the finding
   that made `dodtools_deathmsg fake` need a re-stamp.
2. If `spec_scoreboard`'s value changed since the cached copy at `+0x18a854`,
   toggle the VGUI scoreboard.
3. Enforce five cvars, two of which end by **quitting the game**
   (`r_drawentities`, `cl_lw`) and three silently (`cl_pitchdown`, `cl_pitchup`,
   `crosshair`). Issue #205; `native/src/patch/cfg_scan.rs` refuses the first two
   as typed commands.
4. If `hud_draw` ≠ 0, walk the element list and `Draw` each active element (§1).

Everything in step 3 is reachable: the five `cvar_t*`s are at `gHUD+0x6e290`..
`+0x6e2a0` and the enforcement is a contiguous block. Whether we *should* reach
it is a different question — a movie-maker who wants `r_drawentities 0` is
asking for something GoldSrc itself clamps back to 1 while `sv_cheats` is 0
(`docs/goldsrc_dod_quirks.md`), so the client-side quit is the second lock, not
the first.

---

## 9. Not reachable, or not established

Stated plainly, in the habit `docs/goldsrc_death_notices.md` set.

- **`CHudBase` vftable slot 6.** A seventh virtual DoD added between `Reset` and
  `InitHUDData`. Every element inherits `CHudBase`'s stub (`+0x21970`); nothing
  overrides it, so there is no body to read and no call site was found.
- **`+0x195e40`**, the spectator predicate `CHud::OverviewMapMode` calls. A
  `.data` function pointer, zero in the image, that `client.dll`'s code only ever
  *calls* — there is no store to it anywhere. It is filled by the 46-dword
  `rep movsd` at `+0x362f3`, which copies an engine interface handed to
  `client.dll` during initialisation into `+0x195da0`; the predicate is that
  table's slot 40. **Which interface that is has not been established.** It is
  not `gEngfuncs` — that is a separate 135-dword copy at `+0x176388` whose
  `IsSpectateOnly` slot DoD calls 35 times by the ordinary route — so this is a
  second mechanism, not the same one.
- **The objective timer's x.** Four literal digit positions (10, 24, 44, 58) plus
  a literal 0 for the background sprite, in `CObjectiveIcons::DrawDigit`'s
  callers. Five sites rather than one value; reachable but not as one setting.
- **`CHudSpectator::Draw`'s own layout.** 879 bytes that set the spectator
  interface mode global (`+0xe88d4`) to 1..4 and draw the bar. Not surveyed
  beyond that; it is the biggest remaining unexplored element and it is the one
  that decides the `+54` every other element then compensates for.
- **`CVoiceStatusHud` has two vftables** (`+0xabb48` and `+0xabb24`) — it
  inherits from both `IVoiceHud` and `CHudBase`. Only the eight-slot one is the
  HUD element's; the six-slot one is the interface's. Anything that identifies
  an element by comparing its first dword must use `+0xabb24`. It is the only
  element in this build with that shape, but nothing guarantees it stays the
  only one.
- **Anything about draw *order*.** The list is appended at the tail, so order is
  registration order, which is the order `CHud::Init` calls each `Init` — and
  those are reached through vftable slot 1, not by direct call, so the order was
  not recovered statically. It is trivially readable at runtime by walking the
  list.

---

## 10. Reproducing this

```
pip install pefile capstone
python goldsrc-hooks/tools/survey_client_dll.py             # everything
python goldsrc-hooks/tools/survey_client_dll.py elements    # one section
python goldsrc-hooks/tools/survey_client_dll.py --dll path\to\client.dll
```

The script names the handful of functions it could not derive automatically —
`gHUD`'s own methods are in no vftable, because `CHud` has no virtuals — in a
`KNOWN_FUNCTIONS` table, each with the reason it was identified. Add to that
table rather than to a comment when the next one is pinned down.

Everything else, including the class names, is derived on each run.
