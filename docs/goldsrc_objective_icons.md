# DoD 1.3 objective icons: why they move when you spectate, and how to place them

Companion to `docs/goldsrc_death_notices.md`. That document moved the kill feed;
this one moves the territory-flag icons in the top-left corner and the objective
timer beside them, which have the same problem for the same reason and needed
their own pass over the binary to fix.

Everything below was derived offline from `dod/cl_dlls/client.dll` — 977,816
bytes, byte-identical across the stock, pre-Anniversary and post-Anniversary
installs — with `pefile` and `capstone`. No running game, no debugger. Each
claim names the evidence it rests on, and the section that says what is *not*
proven is not decoration.

Implemented in `goldsrc-hooks/src/objicons.rs`; re-checked against a real
`client.dll` by `goldsrc-hooks/tools/verify_objicons_offsets.py`.

---

## 1. The console surface

```
dodtools_objectives offset <y>       y the objective icons are drawn at
dodtools_objectives xoffset <x>      x the icon row starts at
dodtools_objectives timer <y>        y the objective timer beside them is drawn at
dodtools_objectives <any> default    hand that one back to the game
dodtools_objectives                  what each is set to, and the usage above
```

All three are **absolute screen positions**, and each means the same thing in a
POV demo and in an HLTV demo. The game's own numbers do not — which is the whole
point, and §3 is why.

Each is independent: setting the y leaves the game's x alone, and the icons and
the timer move separately because the game draws them as two elements with two
positions. `default` restores one of them without disturbing the others.

The timer has no x. The game does not keep one for it: its background sprite is
drawn at a literal `0` and its four digits at literal `10`, `24`, `44` and `58`
(`client.dll+0x30620`, the digit helper, called four times with those constants).
That is five sites rather than one value, so it is a different job.

---

## 2. Finding the element

DoD's `client.dll` ships **MSVC RTTI**, which makes this part mechanical rather
than a search. `.?AVCObjectiveIcons@@` is a type descriptor at `+0xc2cd8`;
walking descriptor → complete-object-locator → vftable lands on the class'
vftable at `+0xac32c`.

The vftable layout is `CHudBase`'s, and it is **confirmed against work already
shipped** rather than taken from the HL SDK header — the SDK is exactly the sort
of thing DoD's build may have diverged from:

| slot | `CObjectiveIcons` | `CHudDeathNotice` | how we know |
| --- | --- | --- | --- |
| 0 | `+0x29eb0` | `+0x2a030` | destructor shape |
| 1 | `+0x2f8c0` | `+0x2ae10` | `Init` — registers the user messages |
| 2 | `+0x2f980` | `+0x2ae70` | `VidInit` |
| 3 | **`+0x30030`** | `+0x2ae90` | `Draw` — `deathmsg.rs` already patches `+0x2ae90` as `Draw` |
| 4 | `+0x2fff0` | `+0x21950` | `Think` (the deathnotice one is `CHudBase`'s own stub) |
| 5 | `+0x21960` | `+0x21960` | `Reset` |
| 6 | `+0x21970` | `+0x21970` | a seventh virtual the stock SDK's `CHudBase` does not have |
| 7 | `+0x21980` | `+0x2ae50` | `InitHUDData` — `deathmsg.rs` already patches `+0x2ae50` as `InitHUDData` |

Two independently-established addresses landing on slots 3 and 7 of the same
table is what makes the layout evidence rather than assumption. Slot 6 is an
extra DoD virtual sitting between `Reset` and `InitHUDData`; every HUD element
inherits `CHudBase`'s stub for it and none of them override it, so what it is
has not been established here.

`CObjectiveIcons::Init` (`+0x2f8c0`) hooks eight user messages — `InitObj`,
`SetObj`, `TimerStatus`, `StartProg`, `StartProgF`, `ProgUpdate`, `CancelProg`
and `PlayersIn` — which is the second confirmation that this is the right class,
and incidentally the whole of DoD's capture-progress message set.

---

## 3. What `Draw` does, and where the two positions come from

`CObjectiveIcons::Draw` draws two independent things, in this order, each gated
by its own cvar through a shared predicate at `+0x2ca80`:

| element | section | cvar | x | y |
| --- | --- | --- | --- | --- |
| objective timer | 4 | `cl_hud_objtimer` | fixed (see §1) | `esi` |
| objective icons | 5 | `cl_hud_objectives` | `[esp+0x14]`, advanced per icon | `[esp+0x18]` |

That predicate is worth recording on its own, because it is a **finer-grained
HUD kill switch than `hud_draw 0`**:

```
client.dll+0x2ca80   bool CHud::ShouldDraw(int section)
  1 -> cl_hud_health          and the spectator interface is NOT up
  2 -> cl_hud_reinforcements  and not up, and a byte at +0x108225
  3 -> cl_hud_ammo            and not up   (shares case 1's tail, entered at +0x2cab4)
  4 -> cl_hud_objtimer                     (no spectator test)
  5 -> cl_hud_objectives                   (no spectator test)
  6 -> not up, and cl_hud_objectives is registered
  7 -> always true
```

Sections 4 and 5 — the timer and the icons — are the only two with **no
spectator test**, which is why they are what a spectated demo still draws in the
corner, and why they are what this command is for. "The spectator interface is
up" there means either of two globals is non-zero: `+0xe88d4`, below, or
`+0xe88d8`.

### The y, and the reason this document exists

Both y values are picked the same way, from the same global:

```asm
; the timer, +0x30119
mov  eax, [0x19e88d4]        ; the spectator interface mode
mov  esi, 2                  ; the POV y -- a literal
test eax, eax
je   +0x30146                ; mode 0 -> keep 2
fild [ScreenHeight]
fmul [0.0020833334]          ; / 480
fmul [54.0]
fadd [0.5]                   ; round
call <ftol>
mov  esi, eax
+0x30146:                    ; <- both paths converge, y in esi

; the icons, +0x30248
fild [ScreenHeight]
fmul [0.0020833334]          ; / 480
fld  st(0)
fadd st(0), st(1)            ; 2 * ScreenHeight/480
fadd [0.5]
call <ftol>
mov  [esp+0x18], eax         ; the POV y -- scaled, not a literal
mov  eax, [0x19e88d4]
test eax, eax
je   +0x30287                ; mode 0 -> keep it
fmul [54.0]                  ; else 54 * ScreenHeight/480
fadd [0.5]
call <ftol>
mov  [esp+0x18], eax
+0x30289:                    ; <- both paths converge, y in [esp+0x18]
```

So, in pixels:

| | timer y | icon y |
| --- | --- | --- |
| POV demo, 1080p | 2 | 5 |
| spectated, 1080p | 122 | 122 |
| POV demo, 720p | 2 | 3 |
| spectated, 720p | 81 | 81 |

`54` is the DoD spectator bar's height in 480-space, and the ~117-pixel drop at
1080p is the whole of the problem in the issue. Note that the two elements are
**not** the same distance apart on the two paths — 3 pixels apart in a POV demo
at 1080p, 0 apart while spectating — which is exactly why a single "move the
objective HUD by N" setting would have been the wrong shape. They are two
positions and they get two settings.

`[0x19e88d4]` is not a flag despite being tested as one: `CHudSpectator::Draw`
sets it to 1, 2, 3 or 4 (`+0x3896c`, `+0x38978`, `+0x38993`, `+0x389c9`) and
`CHudSpectator::Reset` puts it back to 0 (`+0x3a15e`). Every HUD element that
consults it only asks whether it is zero, so "the spectator interface is up" is
the right reading for this purpose — but it carries more than that, and anything
that wants to tell DoD's spectator modes apart has the value already.

### The x

The icons' x starts at `10`, becomes `82` once the timer has drawn (`+0x301dd`),
and is recomputed from `ScreenWidth` when `spec_pip` is on and the spectator
interface is up (`+0x301ff`..`+0x30244`). It is advanced per icon by
`x += icon_width + 2` (`+0x30524`) and **never wraps**: the objective icons are
one horizontal row, always.

`spec_pip`'s `cvar_t*` lives at `+0x1174f8`, which is `CHudSpectator`'s `this`
(`+0x115da8`) plus `0x1750` — the slot its own `pfnRegisterVariable` call writes
at `+0x379a1`.

---

## 4. Two detours, and why not immediates

Neither y is a constant in the instruction stream. `54.0`, `2.0` and `1/480` are
`.rdata` floats multiplied together, so rewriting one would change the spectated
placement and leave the POV placement alone — the same "one value, two meanings"
trap `docs/goldsrc_death_notices.md` §3 records for the kill feed, where
patching an immediate meant an absolute y on one code path and an addend on
another.

Detouring the convergence point sets the **result**, so a number typed at the
console means the same thing on every path. This is the technique HLAE uses for
the same job; see `goldsrc-hooks/src/detour.rs`.

| | the timer | the icon row |
| --- | --- | --- |
| signature matches at | `+0x30119` | `+0x30248` |
| convergence point | `+0x30146` | `+0x30289` |
| stolen | `8b 57 24 68 ff 00 00 00` (8) | `8b 47 18 8b 4f 14` (6) |
| what it overwrites | `mov edx,[edi+0x24]` / `push 0xff` | `mov eax,[edi+0x18]` / `mov ecx,[edi+0x14]` |
| the value lives in | `esi` | `[esp+0x14]` and `[esp+0x18]` |
| padding after the `jmp rel32` | 3 `nop` | 1 `nop` |

Both spans are safe to overwrite, and that is checked rather than asserted:
nothing in any executable section branches into either span's interior, and no
dword anywhere in the image points into one. Both registers the icon-row stub
touches (`eax`, `ecx`) are dead at its convergence point — they are being loaded
there, not read — and the timer stub only writes `esi`, which *is* the value.

The detour is a `jmp`, not a `call`, so the stub sees the same `esp` the function
does and can write `[esp+0x14]` / `[esp+0x18]` directly.

`default` clears a flag and lets the game's own value flow through. Nothing is
ever unpatched: there is no safe moment to restore bytes a thread may be
executing, which is also what HLAE does.

---

## 5. What this deliberately does not reach

**The overview map.** `CObjectiveIcons::Draw` calls a gHUD method at `+0x228e0`
and branches on the result:

```asm
client.dll+0x228e0
  call [+0x195e40]             ; a spectator predicate (see below)
  test eax, eax
  je   .zero
  cmp  dword [gHUD+0x234], 0x5a  ; the FOV field -- 90 unless something zoomed
  jne  .zero
  mov  eax, [gHUD+0x6e280]     ; _cl_minimap
  fld  dword [eax+0xc]         ; ->value
  jmp  <ftol>
.zero:
  xor  eax, eax
```

`1` (the small map) makes the loop overwrite both icon slots per objective from
the objective's own record; `2` (the full map) draws each icon at its world
position instead. Both are inside the loop and downstream of the row detour, so
they keep winning — and they should: the icons are being drawn *on the map*, at
map coordinates, and that is not a placement anyone would want overridden.

This also **corrects a characterisation in `docs/goldsrc_death_notices.md` §3**.
That document calls the same `+0x228e0` call "spectator mode" and its `cmp eax, 2`
the "spectator-mode-2 path". It is the overview map's size, not a spectator mode:
the kill feed takes its y from the map layout while the *full* overview map is up.
The behaviour recorded there is unaffected — the detour there sets the result on
every path regardless — but the name was wrong.

`gHUD+0x234` is the FOV field: it is compared against and assigned `0x5a` (90) in
the `SetFOV` user-message path (`__MsgFunc_SetFOV` at `+0x20c30` → `+0x22160`,
and the per-frame update at `+0x36d1c`).

## What is not proven

- **`+0x195e40`**, the predicate `+0x228e0` calls. It is a `.data` function
  pointer, zero in the image, and `client.dll`'s code only ever *calls* it —
  there is no store to it anywhere. It is filled by the 46-dword `rep movsd` at
  `+0x362f3`, which copies an engine interface handed to `client.dll` during
  initialisation into `+0x195da0`; the predicate is that table's slot 40. Which
  interface that is was not chased further. It is not `gEngfuncs` (that is a
  separate 135-dword copy at `+0x176388`, and DoD calls its `IsSpectateOnly`
  slot 35 times by the ordinary route), so this is a second mechanism, not the
  same one.
- **Slot 6 of `CHudBase`'s vftable** — an extra virtual DoD added, which no HUD
  element in this build overrides.
- **Live behaviour.** As of writing, the two detours are proven offline — unique
  signature, expected bytes at the convergence point, no inbound branch, stubs
  byte-for-byte asserted in unit tests — but have not yet been run in a game.
  The kill feed's y detour, which is the same technique against the same binary,
  was live-proven on 2026-09-16 (`docs/goldsrc_death_notices.md` §5).

## Re-verifying

```
python goldsrc-hooks/tools/verify_objicons_offsets.py [path-to-client.dll]
```

Re-derives both detours from a real `client.dll` and checks each signature is
unique, that the convergence point still holds the bytes the stub reproduces,
that nothing branches or points into either span, that the spans are disjoint,
and that the two stack slots the row stub writes are still assigned before the
detour and read after it. Every constant is read out of `objicons.rs` rather
than restated, so the check cannot pass by agreeing with a stale copy of itself.

Run it after any change to the constants, and against any `client.dll` the
project has not seen before.
