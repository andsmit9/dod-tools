# Day of Defeat 1.3 `client.dll` Internals

Static reverse-engineering notes for DoD 1.3's client library, produced to unblock
[issue #204](https://github.com/ccoventry/dod-tools/issues/204) (the `goldsrc-hooks`
companion DLL could not capture `pEngfuncs`). Everything below was derived offline from
the shipped binary — no running game, no debugger, no patched engine — and each claim
names the evidence it rests on.

**Subject analysed:** `dod/cl_dlls/client.dll` from the *Half-Life - Pre-Anniversary for
Movies* depot (the SteamLegacy/beta branch this project captures against), 977,816 bytes,
32-bit PE, `ImageBase 0x1900000`, four sections (`.text 0x1901000`, `.rdata 0x19a4000`,
`.data 0x19bf000`, `.reloc 0x1a9f000`), 125 named exports.

> **This is not pre-Anniversary-specific**, which is worth stating because the depot name
> suggests otherwise. Compared against a stock 25th-Anniversary install (2026-09-07):
> `dod/cl_dlls/client.dll` and `dod/dlls/dod.dll` are **byte-identical** across the two,
> so everything in §§1–5 holds for both. Only `hw.dll` differs, being the engine — and
> the one thing taken from it, the `GetProcAddress(hClient, "F")` convention in §1, is
> present in *all three* installs checked (pre-Anniversary, post-Anniversary, and stock
> Anniversary; the latter two share an engine). Its IAT address differs between
> generations, which costs nothing: `goldsrc-hooks` resolves that slot by import name
> rather than by offset.

---

## 1. The engine does not resolve `client.dll`'s exports by name

This is the finding that explains months of confusing symptoms, and it inverts the
documented GoldSrc mod ABI.

`hw.dll` loads the client library and then asks for **one** export:

```asm
; hw.dll @ 0x1d61cb9  (ImageBase 0x1d00000)
call dword ptr [0x1e1c24c]   ; KERNEL32!LoadLibraryA  -> eax = hClient
mov  ecx, [ebp+0xc]
mov  edi, eax
test edi, edi
mov  [ecx], edi
je   <fail>
push 0x1e4d428               ; the string "F"
push edi                     ; hClient
call dword ptr [0x1e1c114]   ; KERNEL32!GetProcAddress
test eax, eax
je   0x1d61cf1               ; -> FreeLibrary, return 0
mov  edx, [ebp+0x10]
push edx
call eax                     ; F(table)
add  esp, 4
ret
```

If `F` is present the engine calls it and is **done**. If `F` is absent it calls
`FreeLibrary` and fails — there is no per-name fallback on this path. DoD 1.3's
`client.dll` exports `F`, so the whole 43-entry client interface, `Initialize` and
`HUD_Frame` included, is established without a single by-name lookup.

That is the explanation for the failure recorded in #204: patching `client.dll`'s export
table was mechanically perfect (the real Win32 `GetProcAddress` read our patched value
straight back) and yet nothing ever called through it, because the engine does not ask
for those names here.

> **Refinement, from the first successful live run (2026-09-07).** "Never looked up" is
> too strong as a blanket claim, and the log disproves it. After `F` returns, the engine
> separately resolves `CreateInterface` and then `HUD_GetStudioModelInterface` **by name**
> — a different code path (renderer/studio initialisation) from the client-interface
> setup analysed above. So a secured `client.dll` sees both conventions in one session:
> `F` for the `cldll_func_t` table, and by-name lookups for a few later interfaces.
> Why the earlier export-table patch failed to intercept *that* particular lookup is
> still unexplained; it is moot now, because the `GetProcAddress` hook covers both paths,
> but it is not something this document has evidence to claim it solved.

**Independent corroboration.** Xash3D's GoldSrc-compatible client loader implements the
same convention and names it explicitly
([`FWGS/xash3d-fwgs`, `engine/client/dll_int/cl_game.c`](https://github.com/FWGS/xash3d-fwgs/blob/master/engine/client/dll_int/cl_game.c)):

```c
else if(( GetClientAPI = COM_GetProcAddress( clgame.hInstance, "F" )) != NULL )
{
        Con_Reportf( "%s: found single callback export (secured client dlls)\n", __func__ );
        CL_GetSecuredClientAPI( GetClientAPI );
}
```

"Secured client dlls" is the community term for this shape. It also matches HLAE's own
dead, commented-out detection code, which treats a single-letter `F` export as the
marker of a VAC-"secure" build.

> Practical consequence for this project: **the `-insecure` launch flag is irrelevant
> here.** It controls VAC on a *server*; whether `client.dll` was built with the `F`
> entry point is a compile-time property of the shipped file. `-insecure` was already
> present in every test in #204 and changed nothing, which is consistent.

---

## 2. `F` and the `cldll_func_t` table

`F` is `__cdecl`, takes one pointer, returns void. Disassembling it
(`client.dll` RVA `0x2a700`):

```asm
mov  eax, [esp+0xb0]     ; the caller's buffer
mov  esi, [eax]          ; buffer->[0] : pointer to 29 engine-supplied dwords
mov  edi, 0x1a08038
mov  ecx, 0x1d           ; 29
rep  movsd               ; ...copied into a client.dll global
mov  ecx, [eax+8]
mov  [0x1a08030], ecx    ; buffer->[8] : the internal dispatch object
...
lea  esi, [esp+8]        ; 43 function pointers built on the stack
mov  edi, eax            ; destination = the caller's buffer, from offset 0
mov  ecx, 0x2b           ; 43
rep  movsd
ret
```

So `F` **overwrites the buffer it was given** with exactly 43 function pointers, starting
at offset 0. Resolving all 43 addresses back through the export table yields a perfect,
in-order match with Xash3D's `cldll_func_src_t` — which is how the slot indices below are
known to be right rather than guessed:

| Slot | Export | Slot | Export |
| --- | --- | --- | --- |
| 0 | `Initialize` | 22 | `HUD_DrawNormalTriangles` |
| 1 | `HUD_Init` | 23 | `HUD_DrawTransparentTriangles` |
| 2 | `HUD_VidInit` | 24 | `HUD_StudioEvent` |
| 3 | `HUD_Redraw` | 25 | `HUD_PostRunCmd` |
| 4 | `HUD_UpdateClientData` | 26 | `HUD_Shutdown` |
| 5 | `HUD_Reset` | 27 | `HUD_TxferLocalOverrides` |
| 6 | `HUD_PlayerMove` | 28 | `HUD_ProcessPlayerState` |
| 7 | `HUD_PlayerMoveInit` | 29 | `HUD_TxferPredictionData` |
| 8 | `HUD_PlayerMoveTexture` | 30 | `Demo_ReadBuffer` |
| 9 | `IN_ActivateMouse` | 31 | `HUD_ConnectionlessPacket` |
| 10 | `IN_DeactivateMouse` | 32 | `HUD_GetHullBounds` |
| 11 | `IN_MouseEvent` | **33** | **`HUD_Frame`** |
| 12 | `IN_ClearStates` | 34 | `HUD_Key_Event` |
| 13 | `IN_Accumulate` | 35 | `HUD_TempEntUpdate` |
| 14 | `CL_CreateMove` | 36 | `HUD_GetUserEntity` |
| 15 | `CL_IsThirdPerson` | 37 | `HUD_VoiceStatus` |
| 16 | `CL_CameraOffset` | 38 | `HUD_DirectorMessage` |
| 17 | `KB_Find` | **39** | **`HUD_GetStudioModelInterface`** |
| 18 | `CAM_Think` | 40 | `HUD_ChatInputPosition` |
| 19 | `V_CalcRefdef` | 41 | `HUD_GetPlayerTeam` |
| 20 | `HUD_AddEntity` | 42 | `ClientFactory` (not a named export) |
| 21 | `HUD_CreateEntities` | | |

The 43 exported entries are thin thunks: each forwards to the real implementation behind
`[0x1a08030] + slot*4`, converting the normal argument convention into a by-pointer one
(Xash calls these two shapes `cldll_func_src_t` and `cldll_func_dst_t`). For hooking
purposes this is irrelevant — **the slots in the table `F` returns take ordinary
arguments**, e.g. `Initialize(cl_enginefunc_t*, int)`.

### How `goldsrc-hooks` uses this

`goldsrc-hooks` hooks **`hw.dll`'s IAT entry for `KERNEL32!GetProcAddress`** — the exact
call site above — next to the `LoadLibraryA` entry it already hooked to spot `client.dll`
loading. Both slots live in the same import table (`0x1e1c114` and `0x1e1c24c`), and the
`LoadLibraryA` half was already proven reliable, so this is an extension of working
machinery rather than a new mechanism. It is also the technique HLAE's own
`CAfxImportDllHook` uses, and it affects only `hw.dll`'s calls rather than the whole
process.

When the engine asks `client.dll` for `"F"`, we return our own wrapper: it calls the real
`F` so the table gets filled exactly as normal, then swaps slots 0, 33 and 39 for
trampolines before the engine ever reads them. The trampolines receive what we need as
plain arguments — `Initialize` hands us `pEnginefuncs`, `HUD_GetStudioModelInterface`
hands us `pstudio`, and `HUD_Frame` is a genuine per-frame tick (replacing the ~60Hz
timer thread that stood in for it). Lookups of `"Initialize"`, `"HUD_Frame"` and
`"HUD_GetStudioModelInterface"` are handled the same way, so a non-secured `client.dll`
works through the identical code path with no byte patterns and no hardcoded offsets.

---

## 3. Why HLAE's `pEngfuncs` signature scan cannot work on DoD

HLAE's `hl_addresses.cpp` finds `pEngfuncs` by locating the string `"ScreenFade"` in
`client.dll`, finding the code that references it, and reading a hardcoded operand
nearby. That string **does not exist anywhere in DoD's `client.dll`** — confirmed both
in-process (searched every section, exact and substring) and by offline string
extraction from the file.

The reason is now clear, and it is not a build quirk: in the stock Half-Life SDK the
client hooks the `ScreenFade` user message itself (`HOOK_MESSAGE(ScreenFade)`), so the
literal is present. **DoD's client never hooks `ScreenFade`** — it is absent from the
71 messages DoD registers (§5) and is left to the engine. HLAE's anchor is therefore
structurally unavailable in DoD, not merely missing from one build.

## 4. `gEngfuncs` — the client's own copy of the engine table

Useful as a cross-check even though the hook above makes it unnecessary. The exported
`Initialize` thunk copies the engine's table into a `client.dll` global before
dispatching:

```asm
; client.dll Initialize, RVA 0x2a3b0
mov  ecx, 0x87           ; 135 dwords = 540 bytes = sizeof(cl_enginefunc_t)
mov  esi, [esp+0x310]    ; arg1 = pEnginefuncs
mov  edi, 0x1a76388      ; gEngfuncs
rep  movsd
...
cmp  eax, 7              ; CLDLL_INTERFACE_VERSION
```

`gEngfuncs` is therefore at **RVA `0x176388`** (`.data`) in this build, and the struct
this build expects is **135 pointer-sized fields**. Note it is *the struct itself*, not a
pointer to one, and it is all zeroes until `Initialize` runs — reading it earlier yields
a table of null function pointers, so anything derived from it must be validated before
use.

This address is what makes §5 possible, and its correctness is confirmed by the fact that
decoding calls through it produces 111 real, correctly-spelled DoD cvar names.

---

## 5. Extracted client API surface

With `gEngfuncs` known, every call of the form `call dword ptr [gEngfuncs + idx*4]` can
be found in `.text` and its string argument recovered, giving an authoritative inventory
of what DoD 1.3's client registers. Indices are `cl_enginefunc_t` declaration order.

This doubles as an **independent, binary-level check of `goldsrc-hooks`'
`ClEngineFuncsPartial`**, whose slot offsets were previously only transcribed from
HLAE's `cdll_int.h`. Walking that struct's declared slot counts gives the indices below,
and every one of them lands on a slot DoD's own client uses, in the shape its type
implies — a single miscounted field anywhere would push all later ones off:

| index | field | how DoD uses it |
| --- | --- | --- |
| 17 | `pfn_add_command` | 110 calls — exactly the 110 command names recovered below |
| 30 | `pfn_console_print` | 37 calls |
| 38 / 39 | `cmd_argc` / `cmd_argv` | 4 / 6 calls |
| 52 / 53 | `get_view_model` / `get_entity_by_index` | 7 / 38 calls |
| 66 | `pfn_weapon_anim` | 3 calls |
| 71 | `pfn_get_game_directory` | 2 calls |
| 84 | `p_event_api` | 165 references, **0 direct calls** — the signature of a data pointer, which is what it is |
| 88 | `is_spectate_only` | 35 calls |

### Console commands — `pfnAddCommand` (index 17), 107 registered

Beyond the expected `+`/`-` movement and `slot0..slot10` bindings:

```
cancelselect  centerview  credits  firstperson  force_centerview  hud_saytext  impulse
invnext  invprev  joyadvancedupdate  prone  snapto  spec_autodirector  spec_decal
spec_drawcone  spec_drawnames  spec_drawstatus  spec_help  spec_menu  spec_mode
thirdperson  togglescores  voice_showbanned
```

DoD-specific movement verbs worth knowing exist: `+prone`-equivalent `prone`, `+special`,
`+alt1`, `+break`, `+map` / `+mapzoom` (overview), `+commandmenu`.

### Client cvars — `pfnRegisterVariable` (index 14), 111 registered

Grouped by what they are likely to be worth to this project:

- **Spectator / HLTV:** `spec_pip`, `spec_scoreboard`, and the `_internal` companions
  `spec_autodirector_internal`, `spec_drawcone_internal`, `spec_drawnames_internal`,
  `spec_drawstatus_internal`. The `spec_*` console commands above are the user-facing
  front ends; the `_internal` cvars hold the state.
- **Granular HUD suppression:** `cl_hud_ammo`, `cl_hud_health`, `cl_hud_msgs`,
  `cl_hud_objectives`, `cl_hud_objtimer`, `cl_hud_reinforcements`, plus `hud_draw`,
  `hud_centerid`, `hud_deathnotice_time`, `hud_fastswitch`, `hud_takesshots`,
  `cl_hudfont`, `cl_identiconmode`. Finer-grained than the all-or-nothing `hud_draw 0`
  the pipeline currently relies on.
- **Cinematic / visual:** `cl_fog`, `cl_fog_density`, `cl_fog_start`, `cl_fog_end`,
  `cl_fog_red`, `cl_fog_green`, `cl_fog_blue`, `cl_particlefx`, `cl_dynamiclights`,
  `cl_bulletejects`, `cl_corpsestay`, `cl_numshotrubble`, `max_rubble`,
  `cl_drawmodels`, `cl_drawplayermodels`, `cl_weatherdis`, `cl_cutscenes`,
  `cl_xhair_style`, `cl_dynamic_xhair`.
- **View / camera:** `cl_bob`, `cl_bobcycle`, `cl_bobup`, `cl_vsmoothing`,
  `cl_chasedist`, `cl_waterdist`, `scr_ofsy`, `scr_ofsz`, `v_centermove`,
  `v_centerspeed`, and the `cam_*` / `c_min*` / `c_max*` third-person camera family.
- **Overview map:** `_cl_minimap`, `_cl_minimapzoom`, `cl_dmsmallmap`,
  `cl_dmshowplayers`, `cl_dmshowobjects`, `cl_dmshowflags`, `cl_dmshowgrenades`,
  `cl_dmshowmarkers`.

The remainder are stock Half-Life input/joystick/mouse cvars (`m_*`, `joy*`,
`cl_*speed`, `sensitivity`, `lookspring`, `lookstrafe`, `zoom_sensitivity_ratio`).

`spec_decal` (a command, not a cvar) is worth flagging against the decal R&D in
`goldsrc_dod_quirks.md` — it is a spectator-side decal action DoD registers itself,
independent of `r_decals`.

### Server commands the client sends — `pfnServerCmd` (index 19)

```
VModEnable %d          drc_spray %.2f %.2f %.2f %i    follow "%s"
follownext %i          ignoremsg %i                   maim %i
specmode %i            sprone
```

`follow "%s"`, `follownext`, and `specmode %i` are how DoD's own spectator UI drives
target selection — directly relevant to any future work that wants to steer an HLTV
demo's spectated player.

### Console commands the client injects into itself — `pfnClientCmd` (index 20)

```
+attack;wait;-attack   cl_lw 1        cl_pitchdown 89   cl_pitchup 89
crosshair 0            r_drawentities 1   vguicancel    voice_modenable 1
```

These are not incidental — they are an **enforcement routine inside `CHud::Redraw`, so it
runs every rendered frame**, and two of the five cases end by quitting the game.

Decompiled behaviour (`client.dll` `0x1936e20`, the real `HUD_Redraw` implementation):

| cvar | condition | client's response |
| --- | --- | --- |
| `r_drawentities` | `!= 1` | force `r_drawentities 1`, print *"r_drawentities is not a valid command. Do not use it."*, then **`quit`** |
| `cl_lw` | `!= 1` | force `cl_lw 1`, print *"cl_lw 0 is not a valid command. Do not use it."*, then **`quit`** |
| `cl_pitchdown` | `!= 89` and `!= 0` | force `cl_pitchdown 89` (silent) |
| `cl_pitchup` | `!= 89` and `!= 0` | force `cl_pitchup 89` (silent) |
| `crosshair` | `!= 0` | force `crosshair 0` (silent — DoD draws its own crosshair) |

The `quit` is assembled byte-by-byte on the stack (`'q','u','i','t','\n'`) rather than
stored as a literal, which is why no `strings` dump of this binary reveals it.

**This matters to the capture pipeline in two ways**, and neither `r_drawentities` nor
`cl_lw` is currently in `cfg_scan`'s `BANNED_COMMANDS` or `MID_DEMO_HAZARDS`:

1. A user config containing `r_drawentities 0` or `cl_lw 0` — both entirely plausible
   (the first is a moviemaking instinct, the second a long-standing competitive config
   line) — makes `hl.exe` exit as soon as the HUD draws. The batch just sees the process
   vanish.
2. Because the check is per-frame, the same is true of a *scheduled* injection mid-demo,
   not only of startup config.

Tracked as issue #205.

### User messages — `pfnHookUserMsg` (index 18), 71 registered

```
AllowSpec  AmmoPickup  AmmoShort  AmmoX  BloodPuff  CameraView  CancelProg  CapMsg
ClCorpse  ClanTimer  ClientAreas  CurMarker  CurWeapon  DeathMsg  Frags  GameRules
GameTitle  HLTV  HandSignal  Health  HideWeapon  HudText  InitHUD  InitObj  ItemPickup
Logo  MOTD  MapMarker  ObjScore  Object  PClass  PShoot  PStatus  PTeam  PlayersIn
ProgUpdate  RandomPC  ReloadDone  ReqState  ResetHUD  ResetSens  RoundState  SayText
Scope  ScoreInfo  ScoreInfoLong  ScoreShort  ServerName  SetFOV  SetObj  ShowMenu
Spectator  StartProg  StartProgF  StatusIcon  StatusValue  TeamNames  TeamScore  TextMsg
TimeLeft  TimerStatus  Train  UseSound  VGUIMenu  VoiceMask  WaveStatus  WaveTime
WeapPickup  WeaponList  WideScreen  YouDied
```

This is the authoritative set for the `dod` crate's parser, and it settles two things:

1. **`dod::UserMessage::new` covers 61 of the 71.** Unhandled: `AllowSpec`, `GameTitle`,
   `ItemPickup`, `Logo`, `ProgUpdate`, `RandomPC`, `StatusIcon`, `TeamNames`, `Train`,
   `WideScreen`. This is *not* currently a defect — `analysis::is_relevant_message`
   filters to a curated subset first and none of the ten are in it, and the call site
   swallows parse failures with `if let Ok`. It is a completeness note for anyone
   extending the analyzer, not a bug to chase.
2. **`ScreenFade`, `ScreenShake` and `Weather` are parsed by the `dod` crate but are not
   in this list** — they are engine-level messages, not client-hooked ones. That is the
   same fact that breaks HLAE's scan anchor (§3), arrived at from the other direction.

---

## 6. Structure: the binary is not obfuscated

Worth stating plainly, because it changes how expensive any future work here is. Despite
being a "secured" build, `client.dll` is an ordinary release binary:

- **Full MSVC RTTI is present** — 209 type descriptors, giving real C++ class names.
- **2,996 printable strings**, including every sound path, sprite name, VGUI resource
  file and localisation token.
- No packing, no encrypted sections, no anti-debug, no import obfuscation.

The `F` export is a calling-convention difference, not a protection measure. Everything in
this document was recovered with ~200 lines of Python; a proper Ghidra pass would go
considerably further.

The RTTI names map the client's architecture directly. Selected non-VGUI classes:

- **Weapons** (each with a matching `*AmmoClip`): `C30CAL`, `CBAR`, `CBREN`, `CBazooka`,
  `CCOLT`, `CENFIELD`, `CFG42`, `CGarand`, `CGreaseGun`, `CM1Carbine`, `CMG34`, `CMG42`,
  `CMP40`, `CMP44`, `CPIAT`, `CPistol`, `CPschreck`, `CSPRING`, `CSTEN`, `CScopedKar`,
  `CThompson`, `CWEBLEY`; melee `CAmerKnife`, `CGerKnife`, `CSpade`, `CMeleeWeapon`;
  thrown `CHandGrenade(Ex)`, `CStickGrenade(Ex)`, `CDoDGrenade(Ex)`; and the base classes
  `CBipodWeapon` (the MG deploy behaviour `anim_fix` cares about) and `CRocketWeapon`.
- **HUD:** `CHudAmmo`, `CHudBase`, `CHudDeathNotice`, `CHudDoDCommon`,
  `CHudDoDCrossHair`, `CHudDoDMap`, `CMortarHud`, `CObjectiveIcons`, `CHudSpectator`.
- **Spectator / viewport:** `CDoDSpectatorGUI`, `CSpectatorGUI`, `ISpectatorInterface`,
  `DoDViewport`, `IDoDViewPort`, `TeamFortressViewport`, `CSpecHelpWindow`.
- **Rendering:** `CStudioModelRenderer`, `CGameStudioModelRenderer` (the studio renderer
  `anim_fix` works alongside), `CWeatherManager`, `CDoDRainDrop`, `CDoDSnowFlake`,
  `CDoDRocketTrail`, `CDoDDirtExploDust`, `CParticleShooter`, `TriangleWallPuff`.

Entity classnames are present in full: 76 `weapon_*` / `ammo_*` names in `client.dll`,
and `dod/dlls/dod.dll` (the server library, same techniques apply) carries those plus the
stock Half-Life leftovers (`weapon_crossbow`, `weapon_snark`, `weapon_tripmine`, …) that
DoD never uses.

### The unmapped weapon IDs — settled, and not worth chasing

`dod::Weapon` leaves IDs 15, 16, 33, 34 and 41 unmapped, with source comments guessing at
grenades. Two attempts to resolve them statically did **not** pan out, recorded so nobody
repeats them:

- The 42 `d_*` death-notice sprite names exist in `client.dll`, but there is **no array of
  pointers to them** and only two are referenced from `.text` — the death-notice icon is
  resolved by name at runtime, not through a static ID-indexed table.
- In `dod.dll`, `weapon_garand` and friends appear only once each in `.text`, inside a
  flat precache list — not next to an `ItemInfo` initialiser, so the HL SDK's
  `p->iId` field-offset trick finds nothing.

Rather than escalate to Ghidra, the question was answered empirically.
`analysis/examples/weapon_id_probe.rs` tallies the raw `DeathMsg` weapon byte across a
folder of demos and flags any ID the enum doesn't map. Across the full local library —
**447 demos, 148,109 `DeathMsg` records** — only 24 distinct IDs occur, and **none of
15, 16, 33, 34 or 41 appears even once**. The gaps are not a defect worth reverse
engineering.

The same run is a useful profile of what competitive DoD actually shoots with:

| id | weapon | share | | id | weapon | share |
| --- | --- | --- | --- | --- | --- | --- |
| 10 | K98 | 21.6% | | 13 | Mk2Grenade | 7.0% |
| 5 | Garand | 15.0% | | 14 | StickGrenade | 6.4% |
| 11 | Bar | 15.0% | | 8 | Stg44 | 6.0% |
| 6 | ScopedK98 | 8.2% | | 24 | K43 | 3.8% |
| 9 | Springfield | 8.0% | | 12 | Mp40 | 3.0% |

The remaining 14 IDs are all under 1.5% combined (`M1911`, `Luger`, `Thompson`, `Kabar`,
`Spade`, `M1Carbine`, `K98Bayonet`, `ButtStock`, `Bazooka`, `Panzerschreck`,
`M1A1Carbine`, `GermanKnife`, `Mg42`, plus 317 records at id 0 / `Unknown` — world and
non-weapon deaths). Notably **`Mg42` appears 6 times and `Browning30Cal`, `Mg34`,
`GreaseGun`, `Fg42`, `LeeEnfield`, `Sten`, `Bren`, `Webley`, `Piat` and `Mortar` never at
all** — consistent with competitive class restrictions, and worth knowing before
optimising analyzer paths for weapons that never fire in this corpus.

## 7. The director stream cannot aim DoD's spectator camera

`svc_director` (51) is dispatched to `HUD_DirectorMessage`, slot 38 of the
`cldll_func_t` table, exported by name as well at RVA `0x2a6a0`. It forwards to
`CHudSpectator::DirectorMessage` at `0x38030`, which reads the sub-command byte
and dispatches through a jump table:

```
0x38046  call     0x193eb20                       ; READ_BYTE() -> command
0x3804b  lea      ecx, [eax - 1]
0x3804e  cmp      ecx, 9
0x38051  ja       0x193835b                       ; default: discard
0x38057  jmp      dword ptr [ecx*4 + 0x1938370]   ; exactly 10 entries
```

The bounds check admits `command - 1` in `0..=9`, so **only DRC commands 1
through 10 are implemented**:

| Cmd | Name | Handler |
|-----|------|---------|
| 1 | `DRC_CMD_START` | `0x3805e` |
| 2 | `DRC_CMD_EVENT` | `0x3809a` |
| 3 | `DRC_CMD_MODE` | `0x38115` |
| 4 | `DRC_CMD_CAMERA` | `0x38143` |
| 5 | `DRC_CMD_TIMESCALE` | `0x382ef` |
| 6 | `DRC_CMD_MESSAGE` | `0x381bc` |
| 7 | `DRC_CMD_SOUND` | `0x382b6` |
| 8 | `DRC_CMD_STATUS` | `0x382fb` |
| 9 | `DRC_CMD_BANNER` | `0x38320` |
| 10 | `DRC_CMD_STUFFTEXT` | `0x38347` |

`DRC_CMD_CHASE` (11) and `DRC_CMD_INEYE` (12) — the two commands that aim a
spectator camera at a chosen player — are **past the end of the table** and
discarded unread. The bytes following the table are instruction padding, not
entries.

### Why this matters

Pinning the spectator view to one player by injecting `DRC_CMD_INEYE` into an
HLTV demo cannot work on DoD 1.3. That was parked R&D in `patch::engine` for a
long time on the assumption it was merely unfinished; it is not implementable
by this route. Live testing agreed before the disassembly did: an injected
INEYE left the camera fully controllable, with every spectator mode still
switchable by hand.

Two separate bugs had masked the real answer. The injection wrote its
`svc_director` *inside* an existing frame's payload rather than as a standalone
frame, so the demo would not load at all (`illegal server message`, `packet
read overflow`); and the constant it used, `DRC_CMD_INEYE = 5`, is actually
`DRC_CMD_TIMESCALE` — so even once the framing was fixed, the message being
sent was not the one intended. Neither mattered in the end.

`MESSAGE` (6) and `STUFFTEXT` (10), the two commands dod-tools injects for
bookmarks, sit inside the handled range. That is why those work, and it
corroborates the command numbering above.

Anything that needs the camera on a particular player has to reach the
spectator state directly — `goldsrc-hooks` territory, not the demo stream. See
issue #206.

## 8. Sprint does nothing to the first-person viewmodel

`goldsrc-hooks`' animation fix was thought to be missing a behaviour: DoD appears to
lower or hide the weapon while a player sprints, and the fix does not reproduce it. It is
not missing. **DoD 1.3 does not touch the viewmodel during a sprint at all**, and the
place the weapon visibly changes is the player's *body*, which the engine already
animates correctly from the replicated `sprint_*` sequence.

There were only three mechanisms it could have been, and all three are ruled out.

**It does not switch the viewmodel off, and it does not play a sequence on it.**
`analysis/examples/sprint_viewmodel_probe` finds sprints in a POV demo the way the client
does — see the stamina field below — and reports what the viewmodel did across each one.
Over four POV demos and roughly 500 sprints longer than half a second,
`clientdata.viewmodel` holds one non-zero model index through every sprint and
`weaponanim` holds one value. Neither ever moves *because of* a sprint. The
`clientdata.viewmodel == 0` intervals that do exist run 6–13 seconds and are deaths.

**It is not a client-side transform either**, which is the case a demo could not see. The
view module (`V_CalcRefdef` at `0x1951470`, dispatching to `V_CalcNormalRefdef` at
`0x194f430`) never reads any of the three things it would need:

| what it would have to read | where it lives | reads inside `0x194e000..0x1951600` |
| --- | --- | --- |
| the sprint key | `in_speed.state`, `0x1a8a950` | 0 (all 10 refs are in the input module) |
| the player's buttons | `pparams->cmd`, `+0xc8` | 0 |
| stamina | `0x19e9d4c` | 0 (its 3 refs are the stamina bar, the sprint grunt, and a clamp helper) |

So there is nothing to reproduce, and adding a "lower the weapon on sprint" behaviour
would make HLTV playback *less* faithful than a POV recording, not more.

### Two things worth keeping from the search

**Sprint is `+speed`.** `client.dll` registers no `+sprint`; the bind is `+speed`
(handler `0x193c3b0`, `kbutton_t in_speed` at `0x1a8a948`, its `state` at `0x1a8a950`).
The only sprint-specific strings in the whole client are `player/sprintgrunts.wav` and
`hud_staminabar`.

**Stamina is `entity_state_t.fuser4`, and it is replicated per player.**
`HUD_ProcessPlayerState` (slot 28, `0x1930dc0`) copies `iuser1..4`, `fuser1..4` and
`vuser1` for *every* player, then — behind a `pfnGetLocalPlayer` check that the state
being processed is the local player's — publishes a handful of them to globals:

```
iuser1 -> 0x19e88d4   (g_iUser1, the spectator mode V_CalcRefdef branches on)
iuser2 -> 0x19e88d8
iuser3 -> 0x19e88dc
iuser4 -> 0x19e910c
movetype -> 0x19e9110
effects  -> 0x19e9d48
fuser4   -> 0x19e9d4c   <- stamina: the stamina bar and the sprint grunt both read this
```

The per-player copy happens *before* the local-player check, so a spectated player's
stamina is present in `entity_state.fuser4` if the server sends it — which makes a
falling `fuser4` the one honest "this player is sprinting" signal available to an HLTV
recording. Nothing currently needs it, but it is the handle if something ever does.

---

## 9. The `exploding_` grenade family is a second weapon, not a state

Every grenade viewmodel — `v_grenade`, `v_stick`, `v_mills` — carries nine sequences in
the same order:

```
0 idle   1 draw   2 pinpull   3 holster   4 throw
5 exploding_idle   6 exploding_draw   7 exploding_pinpull   8 exploding_throw
```

Both halves are used heavily in a real match, and what selected between them was open.
Nothing selects between them: **they belong to two mirrored weapon classes**, and which
one is playing is simply which weapon entity the player holds.

`dod.dll` registers `weapon_handgrenade` / `weapon_stickgrenade` and, beside each, a
`_ex` variant whose short name in its item-info blob is `primgren` / `primstick` — a
*primed* grenade. The two classes' animation call sites mirror exactly:

| what | plain | anim | `_ex` | anim |
| --- | --- | --- | --- | --- |
| Deploy | `0x10009610` | 1 `draw` | `0x1000a7c0` | 6 `exploding_draw` |
| PrimaryAttack | `0x10009ac0` | 2 `pinpull` | `0x1000a950` | 7 `exploding_pinpull` |
| throw | `0x10009730` | 4 `throw` | `0x1000aa40` | 8 `exploding_throw` |
| idle | `0x10009b80` | 0 `idle` | `0x1000adf0` | 5 `exploding_idle` |
| Holster | `0x100099a0` | 3 `holster` | — refused, see below | |

A live grenade on the ground gives the primed weapon out: the Touch handler at
`0x10054600` checks the toucher is a living player, then gives and selects
`weapon_handgrenade_ex` or `weapon_stickgrenade_ex` by team (`player+0x398`). That is
DoD's pick-a-grenade-up-and-throw-it-back mechanic, and it is the same mechanic as the
"priming" trick — rolling one out and picking it up to shorten the fuse. Separately,
`0x1007e800` refuses a weapon switch whose target classname is either `_ex` grenade: you
cannot put a live grenade away.

The switch happens without a deploy animation. Measured over three POV halves,
`exploding_draw` fires **once** while `draw` fires 103 times, yet `exploding_idle`,
`exploding_pinpull` and `exploding_throw` (142 / 152 / 54) are used about as often as
their plain counterparts (121 / 145 / 60). `analysis/examples/grenade_family_probe`
prints the ordered stream and the transition table that shows the two chains running in
parallel.

### Why `anim_fix` cannot tell them apart, and should not try

Both classes share **the same three model files and the same body-animation token**. From
the item-info blobs, plain is `('gren', 'gren')` and `_ex` is `('primgren', 'gren')`, on
`models/p_grenade.mdl` + `models/v_grenade.mdl` in both cases. So for a spectated player,
an HLTV recording carries:

- the same `weaponmodel` (`p_grenade.mdl`) either way,
- the same body sequence label (`*_gren_*`) either way,
- and no weapon-entity identity at all.

There is nothing replicated that separates a primed grenade from a fresh one, so the
animation fix should keep playing the plain family and this question is closed rather
than open.

---

## 10. Reproducing this analysis

No IDA or Ghidra required; everything above came from `pefile` + `capstone`
(`pip install capstone pefile`). The core of it:

```python
import pefile, capstone

pe   = pefile.PE(r"...\dod\cl_dlls\client.dll")
base = pe.OPTIONAL_HEADER.ImageBase
img  = pe.get_memory_mapped_image()
md   = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_32)

exports = {e.name.decode(): e.address
           for e in pe.DIRECTORY_ENTRY_EXPORT.symbols if e.name}

# 1. Read any export's disassembly (start with "F" and "Initialize").
for i in md.disasm(img[exports["F"]:exports["F"]+0x200], base + exports["F"]):
    print(hex(i.address), i.mnemonic, i.op_str)

# 2. gEngfuncs = the `mov edi, imm32` immediately before Initialize's `rep movsd`.
# 3. For an engine-func index N, find `FF 15 <gEngfuncs + N*4>` in .text and walk
#    backwards for the nearest `68 <imm32>` that points at a printable C string:
#    that is the name argument (cdecl pushes right-to-left, so the name lands last).
```

Re-running §5 against a different depot (for example the 25th Anniversary build) is the
way to check whether these offsets and inventories still hold; only `gEngfuncs`'s address
and the `F` table's contents are build-specific, and the hook in §2 depends on neither.
