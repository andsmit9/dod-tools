# GoldSrc `hw.dll`: what HLAE already owns, and what is left

Sibling to `docs/goldsrc_client_dll_survey.md`. That one covers DoD's client
library, where we have the module to ourselves. This one covers the engine,
where we do not.

Opened as [issue #256](https://github.com/ccoventry/dod-tools/issues/256), which
sets the order: **subtraction first.** HLAE patches `hw.dll` heavily, two hooks
over one span destroy each other, and the list of what HLAE already solves is
also a list of what is probably a duplicate rather than a gain.

**Subjects:** `hw.dll` from the *Half-Life - Pre-Anniversary for Movies* depot,
1,641,376 bytes on disk, `ImageBase 0x1d00000`, `.text` 1,159,168 bytes — **ten
times `client.dll`'s**. And `AfxHookGoldSrc.dll` from the matching HLAE install,
read but never modified.

**Method:** offline, `pefile` + `capstone`. The tables are produced by
`goldsrc-hooks/tools/survey_hw_dll.py`:

```
python goldsrc-hooks/tools/survey_hw_dll.py [keys|collide|findings]
```

> ### Read this before §1
>
> **This is a partial survey, and says so up front.** `hw.dll` is ten times the
> size of `client.dll` and has neither RTTI nor useful exports, so none of what
> made the client survey go wide is available here. What is complete is the
> HLAE map (§1–§2) — the prerequisite the issue names — and the five specific
> questions it asked (§3). The rest of the engine is **not surveyed**; §5 says
> what that leaves.

---

## 1. HLAE's database, read out of the binary

`AfxHookGoldSrc.dll` registers its pattern keys as C++ static initialisers —
`push <name>; push <result slot>; mov ecx, <map>; call` — which makes the
database recoverable exactly rather than approximately. There are **68 keys**.

### The list in issue #256 was a guess, and several entries were wrong

That list came from `strings`, and the issue said so. Against the real database:

| the issue guessed | actually |
| --- | --- |
| `R_DrawEntitiesOnList_In` / `_Out` | do not exist |
| `R_DrawSkyBox_Begin` / `_End` | do not exist; there is one `R_DrawSkyBoxEx` |
| `S_StartDynamicSound`, `S_Update_` | do not exist; there are `SND_PickChannel` and `GetSoundtime` |
| `CL_ParseServerMessage_CmdRead_MsgReadByte_CallAddrOfs` | does not exist |
| — | six `UnkDrawHud*` keys nobody had listed |

Which is the point of extracting it rather than trusting the names.

### The 46 engine-side keys

**Functions HLAE detours** (14):

```
CL_Disconnect      Host_Init        R_DrawEntitiesOnList   R_PolyBlend
CL_EmitEntities    Mod_LeafPVS      R_DrawParticles        R_RenderView
Draw_DecalMaterial GetSoundtime     R_DrawSkyBoxEx         S_PaintChannels
                                    R_DrawViewModel        S_TransferPaintBuffer
```

plus `host_frametime` and `hw_HUD_GetStudioModelInterface_pStudio`, whose keys
name data but whose detours attach to the function that produces them.

**Resolved but never written to** (the rest): `pEngfuncs`, `ppmove`, `pstudio`,
`engine_ClientFunctionTable`, `CL_ParseServerMessage_CmdRead` (+`_DSZ`),
`p_cmd_functions`, `_Host_Frame`, `R_PushDlights`, `SND_PickChannel`, the six
`UnkDrawHud*`, `clientDll`, `hlExe`, `hwDll`, and the data globals `g_fov`,
`msg_readcount`, `net_message`, `paintbuffer`, `paintedtime`, `r_refdef`, `shm`,
`skytextures`, `soundtime`.

### The 22 game-client keys are `cstrike`, `tfc` and `valve` only

Ten `cstrike_*`, seven `tfc_*`, one `valve_*`, and the three
`hw_HUD_GetStudioModelInterface_*`. **There is no `dod_` key of any kind** —
which is what `docs/goldsrc_death_notices.md` §1 already concluded from the
other direction, now confirmed against the whole database rather than a
`strings` dump.

So on the DoD client side we have the module entirely to ourselves, and
`dodtools_deathmsg` and `dodtools_objectives` cannot be colliding with anything
of HLAE's. That is worth knowing with certainty rather than by inference.

---

## 2. The collision model: HLAE detours **prologues**, we detour **mid-function**

`AfxHookGoldSrc.dll` carries `.detourc` and `.detourd` sections and a statically
linked Microsoft Detours. Its install sequence is the standard transaction —
begin, update-thread, then `DetourAttach(&target, hook)` per hook, 34 of them —
and `DetourAttach` rewrites **the first ≥5 bytes of the target function**,
building a trampoline from the instructions it displaced.

That single fact decides the whole risk picture, and it is better than expected:

- **HLAE writes at function entries.** Our detours (`detour::install`) go at a
  *convergence point inside* a function, chosen because that is where a value
  means one thing. Those two are structurally disjoint.
- **The one case that would collide** is us detouring a function entry that HLAE
  also hooks. Nothing we ship does, and `ensure_offset_detour`-style byte
  checking catches it anyway: `E9` is not what our stubs expect to find, so a
  collision fails loudly rather than corrupting both hooks. §5's guardrail —
  every `hw.dll` detour must do the same check — stands, and this is why.
- **Install after HLAE, not before**, still holds. Not because ordering decides
  a winner in the disjoint case, but because being second is what lets the byte
  check see HLAE's jump at all.

### Where HLAE's patterns land in this `hw.dll`

Resolved by applying HLAE's own patterns to the game binary — so these are
addresses in `hw.dll`, which are facts about the game, not about HLAE:

| key | `hw.dll` | detoured |
| --- | --- | --- |
| `CL_Disconnect` | `+0x17850` | yes |
| `R_PushDlights` | `+0x433a0` | no |
| `R_DrawViewModel` | `+0x464c9` | yes |
| `R_DrawParticles` | `+0x46415` | yes |
| `Mod_LeafPVS` | `+0x49642` | yes |
| `R_DrawSkyBoxEx` | `+0x507ae` | yes |
| `S_PaintChannels` | `+0x8cd75` | yes |
| `SND_PickChannel` | `+0x8be00` | no |
| `paintedtime` / `soundtime` / `paintbuffer` | `+0x8cb94` / `+0x8cbe3` / `+0x8e96d` | data |
| `r_refdef` / `skytextures` / `g_fov` | `+0x458ea` / `+0x508f0` / `+0xbfc8` | data |
| `pEngfuncs` | `+0xb232` | data |
| `UnkDrawHudInCall` | `+0xb75b4` | no |

`CL_Disconnect` at `+0x17850` is confirmed twice over: HLAE's pattern matches
there uniquely, and the engine's own demo-stop path calls it
(`hw+0x10923: call 0x1d17850`). The remaining keys use patterns that match many
times because HLAE applies them with a restricted search range this survey does
not reproduce; the tool reports the match count rather than guessing.

> **Nothing of HLAE's is stored in this repository.** The tool reads
> `AfxHookGoldSrc.dll` from the user's own install at run time. Its pattern
> strings are never written out, never committed, and never used as signatures
> of ours. What is recorded is the result of applying them — addresses in the
> *game's* binary. That is the same line `docs/goldsrc_death_notices.md` draws
> ("ideas yes, code no"), drawn here deliberately: lifting the pattern database
> is exactly the act that would put `advancedfx`'s licence in play, and its
> licence is a split whose applicability to `AfxHookGoldSrc` is unresolved.

---

## 3. The five questions the issue asked

### 3.1 The 64-byte command limit — settled, and its name is wrong

**`CLAUDE.md` calls it "GoldSrc's 64-byte `Cbuf_AddTextToBuffer` limit". There is
no such limit.**

`Cbuf_Init` at `hw+0x272b0`:

```asm
push 0x4000             ; 16,384 bytes
push 0x2d08240          ; the cmd_text sizebuf
push "cmd_text"
call SZ_Alloc           ; hw+0x2ac10 -- sets [buf]=name, [buf+8]=malloc(size)
push 0x4000
push 0x2d08260
push "filteredcmd_text"
call SZ_Alloc
```

GoldSrc's command buffer is **16 KB**, and `hw.dll` contains no string
containing "Cbuf" at all — there is no overflow message to hit.

The 64 is real, but it is the **demo file format**: a Type-3 `ConsoleCommand`
frame carries a fixed `char command[64]`. This project's own reader says so —
`dem-patch/src/demo_parser.rs`'s `parse_console_command` is
`map(take(64usize), …)` — and `native/src/patch/engine.rs` already half-knows it
("64-byte panic is for ConsoleCommand frames, not for director payloads").

**Consequence: not reachable, and there is nothing to raise.** The limit is a
property of the bytes we write into a file the engine parses, not of a buffer we
could grow. Staggering long paths across ticks remains the answer. What changes
is the reasoning — and `CLAUDE.md`'s wording, which currently sends anyone
investigating this to a function that does not impose it.

*(Not proven: the engine's own read of that 64-byte field was not located. The
conclusion rests on the format side — our reader, and the pipeline's own naming
— plus the positive proof that `Cbuf` is 16 KB.)*

### 3.2 Interpolation and timing — reachable, and it is one dword

`ex_interp` is **engine-managed**: a per-frame clamp at `hw+0x18ee0` forces it
into a range and **writes the clamped value back through `Cvar_Set`**, printing
`ex_interp forced up to %i msec` or `ex_interp forced down to %i msec`. Setting
it by hand and expecting it to stay is therefore futile, which is worth knowing
before anyone tries.

```asm
hw+0x18ef3  mov edi, 0x32          ; 50 ms, the starting floor
hw+0x18ef8  mov ebx, 0x64          ; 100 ms -- THE CEILING
...
hw+0x18f68  mov ebx, 0xc8          ; 200 ms when a flag at +0x2d5df84 is set
hw+0x18f82  fld  [1000.0]
hw+0x18f88  fdiv [cl_updaterate]   ; the floor is 1000/cl_updaterate, min 1
hw+0x18f9f  fld  [ex_interp]
hw+0x18fa5  fmul [1000.0]          ; in ms
            ...clamp into [edi, ebx], print, Cvar_Set it back
```

The ceiling is a plain `mov r32, imm32` at `hw+0x18ef9`. Raising it is one
dword, with `patch::write_code_bytes`, no detour and no signature beyond
confirming the bytes.

**Why a movie-maker would want it.** A longer interpolation window is smoother
entity motion between snapshots, which is where most spectated-demo ugliness
comes from. 100 ms is a *network* compromise; a demo being rendered offline has
no latency budget to protect.

**Risk:** engine-wide, but narrowly so — it affects entity interpolation and
nothing else, and it is not a cheat vector in a demo. `cl_updaterate` still sets
the floor, so a demo recorded at a low update rate cannot be smoothed below what
it captured.

*Not proven: what the flag at `+0x2d5df84` is. It is written to `1` at
`hw+0x10880` in the demo-start path, so "playing a demo" is the obvious reading
and would mean demos already get the 200 ms ceiling — but that is inference, and
it is exactly the sort of thing that should be checked before the work is
costed.*

### 3.3 Demo playback and parsing — located, not surveyed

`CL_ParseServerMessage` is at the function containing `hw+0x1aab2`
(`CL_ParseServerMessage: svc_updateuserinfo > MAX_CLIENTS`), and the whole
`svc_*` name table is in `.data` from `hw+0x13afd0`, so the dispatch is
findable. `CL_ParsePacketEntities` is the function containing `hw+0x12ffa`
(`CL_ParsePacketEntities: newindex == MAX_PACKET_ENTITIES` — the same limit
issue #207 is about). The demo reader is around `hw+0x105fd`, bounded by
`Error: Corrupt demo file.` (`hw+0x108fa`) and
`Demo message > MAX_POSSIBLE_MSG` (`hw+0x1149c`).

HLAE resolves `CL_ParseServerMessage_CmdRead` **and its size**, which is the
`_DSZ` shape it uses for a span it intends to overwrite — but it does not
`DetourAttach` it, so whatever it does there is not an entry hook. That is worth
establishing before anything of ours goes near this function.

Not surveyed further. It is the largest remaining item and it deserves its own
pass, not a paragraph.

### 3.4 Entity and decal limits — located, not surveyed

`r_decals` is registered at `hw+0x46c35` with a default of `4096.0`, alongside
`sp_decals` and `mp_decals`. `Draw_DecalMaterial` is one of the fourteen
functions HLAE detours, which is the first thing anyone touching decals
engine-side needs to know.

`docs/goldsrc_dod_quirks.md` already records the behaviour that matters — the
index rotates and evicts nothing, so lowering `r_decals` mid-demo strands every
decal above the new limit — and the pipeline already works around it from
`init_commands`. Whether the ring's size is reachable was **not** established.

### 3.5 `CL_FlushEntityPacket` — not located

No self-naming string, and no pattern of HLAE's points at it. The condition
under which it fires is already recorded from the demo side, and that is what
the reseq work needed; whether the engine side is reachable remains open.

---

## 4. What is left that is worth doing

Subtracting HLAE, and taking §3 at face value:

| item | technique | effort | risk |
| --- | --- | --- | --- |
| raise `ex_interp`'s 100 ms ceiling (§3.2) | immediate rewrite, one dword | very low | narrow: entity interpolation only |
| the 64-byte command limit (§3.1) | **not reachable** — wrong lever | — | — |
| `CL_ParseServerMessage` (§3.3) | unknown; needs its own pass | high | high — every mod, every message |
| the decal ring (§3.4) | unknown; `Draw_DecalMaterial` is HLAE's | medium | `client.dll` has no equivalent, so it is this or nothing |

The general lesson from the client survey holds here too, and more strongly:
**prefer `client.dll` where an equivalent exists.** A change in `hw.dll` affects
the menu, every mod and every session; a change in `client.dll` affects DoD. Of
everything in this document, only `ex_interp` has no client-side equivalent and
a clear payoff.

---

## 5. What is not surveyed

Stated plainly, because a partial survey presented as complete is worse than
none.

- **Most of `hw.dll`.** `.text` is 1.13 MB against `client.dll`'s 0.67 MB, and
  none of what made the client pass cheap is available: no RTTI, no useful
  exports, and only **36 functions** nameable at all — those that print their
  own name in an error message. Everything else is `sub_*`.
- **Rendering, sound, physics and networking** beyond the HLAE map. Not opened.
- **`R_PushDlights`, `SND_PickChannel` and the six `UnkDrawHud*` keys.** HLAE
  resolves them and does not detour them, which means it reads or patches them
  some other way. What way was not established, and "HLAE resolves it" is not
  the same as "HLAE leaves it alone".
- **Whether the 34 `DetourAttach` sites are all of HLAE's writes.** Detours is
  what it uses for function hooks; a `VirtualProtect`-and-write elsewhere in the
  module would not appear in that count. The `_DSZ` keys (§3.3) suggest at least
  one span patch that is not a Detours hook.
- **The engine's own read of the demo `ConsoleCommand` field** (§3.1).
- **The flag at `+0x2d5df84`** that raises `ex_interp`'s ceiling to 200 ms
  (§3.2).

---

## 6. Reproducing

```
pip install pefile capstone
python goldsrc-hooks/tools/survey_hw_dll.py             # everything
python goldsrc-hooks/tools/survey_hw_dll.py keys        # HLAE's 68 keys
python goldsrc-hooks/tools/survey_hw_dll.py collide     # where they land in hw.dll
python goldsrc-hooks/tools/survey_hw_dll.py findings    # ours, re-checked
python goldsrc-hooks/tools/survey_hw_dll.py --hw ... --afx ...
```

`findings` runs without HLAE present and re-checks each address in §3 against
the bytes this build ships, so a different `hw.dll` fails loudly rather than
being described by a document written against another one.
