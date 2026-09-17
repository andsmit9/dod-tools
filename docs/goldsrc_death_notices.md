# DoD 1.3 death notices: raising the line count, and the rest of `mirv_deathmsg`

> **Status 2026-09-16 — implemented and live-tested.**
> Lives in `goldsrc-hooks/src/deathmsg.rs`, on branch
> `feat/goldsrc-hooks-companion-dll`. One console command,
> `dodtools_deathmsg`, with four subcommands.

DoD shows four death notices at once and no console variable changes that.
`hud_deathnotice_time` (default 6) changes how long each one *lives*, which is
how you get more of them on screen at once today — but four is the ceiling, and
it is compiled in.

HLAE solves this for other mods with `mirv_deathmsg`. It does not solve it for
DoD, and cannot be made to: §1 explains why. §§2–4 are the DoD implementation.

---

## 1. HLAE's `mirv_deathmsg` is `cstrike` and `tfc` only

Not a configuration gap — the mod list is compiled into HLAE's pattern
database. `AfxHookGoldSrc.dll` carries exactly these, and their `tfc_` twins:

```
cstrike_CHudDeathNotice_Draw          cstrike_rgDeathNoticeList
cstrike_CHudDeathNotice_Draw_YRes     cstrike_MsgFunc_DeathMsg
cstrike_CHudDeathNotice_Draw_YRes_DSZ cstrike_CHudDeathNotice_MsgFunc_DeathMsg
```

There is no `dod_` entry. The changelog agrees: the feature shipped as
`mirv_cstrike_deathmsg` and was later generalised "to support TFC now", with
`mirv_cstrike_deathmsg` left as an alias. DoD was never added.

The subcommands are `max <value>`, `offset default|<value>`,
`block [!]<id>...` and `fake <attackerId> <victimId> <0|1> <weaponString>`.

## 2. What DoD's `client.dll` actually has

The stock Half-Life SDK death-notice code, unchanged:

```c
#define MAX_DEATHNOTICES 4
struct DeathNoticeItem {      // 156 bytes
    char  szKiller[64];       // +0x00
    char  szVictim[64];       // +0x40
    int   iId;                // +0x80   sprite index; 0 == empty slot
    int   iSuicide;           // +0x84
    int   iTeamKill;          // +0x88
    int   iNonPlayerKill;     // +0x8c
    float flDisplayTime;      // +0x90
    float *KillerColor;       // +0x94
    float *VictimColor;       // +0x98
};
static DeathNoticeItem rgDeathNoticeList[MAX_DEATHNOTICES + 1];
```

`InitHUDData` gives the array away without needing a single field identified:

```asm
; +0x2ae50  CHudDeathNotice::InitHUDData
mov ecx, 0xc3           ; 195 dwords = 780 bytes
xor eax, eax
mov edi, 0x1a765d8      ; rgDeathNoticeList
rep stosd
```

780 = 5 × 156, which is `MAX_DEATHNOTICES + 1` slots of the struct above. The
field offsets then fall out of where the code indexes it.

Four functions touch it, at these RVAs (`ImageBase` 0x1900000):

| RVA | function |
| --- | --- |
| `0x2ae10` | `Init` — `HOOK_MESSAGE(DeathMsg)`, `CVAR_CREATE("hud_deathnotice_time","6",0)` |
| `0x2ae50` | `InitHUDData` — the `rep stosd` above |
| `0x2ae90` | `Draw` |
| `0x2b1a0` | `MsgFunc_DeathMsg` (thunk at `0x2ad70` supplies `this`) |

**Subject:** `dod/cl_dlls/client.dll`, 977,816 bytes — byte-identical across the
stock, pre-Anniversary and post-Anniversary installs, so one set of offsets
covers all three.

### DoD's `DeathMsg` payload is not CS's

Three bytes: **killer index, victim index, weapon index**. The weapon is an
index 1..=43 into a table of `d_*` sprite names at `0x19c3418` (stride 32),
bounds-checked with `test eax,eax; jle` and `cmp eax,0x2c; jge`, falling back to
`d_world`. CS reads a weapon *string* and carries a headshot flag; DoD has
neither. So HLAE's `<0|1>` argument has no DoD equivalent, and `fake` takes a
weapon name or index in its place.

## 3. Two mechanisms, not one

### `block` and `fake` need no code patching

Because of how the engine installs user-message handlers. `pfnHookUserMsg`
(`hw.dll` `0x1d1a830`) does **not** overwrite an existing entry:

```c
// gClientUserMsgs at 0x1e6d27c; records are 32 bytes:
//   +0x00 iMsg  +0x04 iSize  +0x08 szName[16]  +0x18 next  +0x1c pfn
int HookUserMsg(char *name, pfnUserMsgHook pfn) {
    for (rec = gClientUserMsgs; rec; rec = rec->next)
        if (!stricmp(name, rec->szName)) {
            match = rec;
            if (rec->pfn == pfn) return rec->pfn;   // already ours: no-op
        }
    fresh = malloc(0x20);
    if (match) memcpy(fresh, match, 0x20);          // carries iMsg and iSize
    else       strncpy(fresh->szName, name, 15);
    fresh->pfn  = pfn;
    fresh->next = gClientUserMsgs;
    gClientUserMsgs = fresh;                        // prepend
    return 0;
}
```

and the by-number dispatcher (`0x1d1a660`) walks from the head and **stops at
the first match**. So the most recent hook wins, `client.dll`'s own handler
stays reachable by calling its thunk directly, and re-hooking is idempotent —
the `rec->pfn == pfn` early-out means calling it every frame allocates nothing.

That last part is load-bearing rather than incidental: the engine frees the
entire message list on disconnect (`0x1d1a8c0`), and `client.dll` re-hooks its
own handler on the next connect. Re-prepending once a frame is what survives a
map change.

### `max` and `offset` need patching, and `max` needs the array moved

The array is five slots and **there is no slack after it** — live data is
referenced at array-end + 0. So it cannot grow in place.

Relocating it is a closed problem, and that is a measured claim, not an
assumption: scanning the *entire image* for dwords landing inside the array
finds exactly **34**, all in `.text`, all inside those four functions. Nothing
in `.data`, no vtable, no saved pointer. All 34 carry base relocations, which is
why `deathmsg.rs` records RVAs and rebases them through the live module handle
rather than using literals — `client.dll` does not opt into ASLR, but a base
conflict still relocates it.

The full patch is 42 operands. **Every address in `deathmsg.rs` is the address
of the encoded operand, not of the instruction containing it** — the two differ
by 1, 2 or 4 bytes depending on the encoding, and getting that wrong reads a
plausible-looking number and writes into the middle of an instruction:

| what | instruction | operand | stock |
| --- | --- | --- | --- |
| 33 array references | various | various | `&rgDeathNoticeList + field` |
| scan sentinel `3d` `cmp eax, &list[MAX].iId` | `0x2b23c` | `0x2b23d` | `array + 0x2f0` |
| `InitHUDData: b9` `mov ecx, (MAX+1)*156/4` | `0x2ae51` | `0x2ae52` | `0xc3` |
| `Draw: b9` `mov ecx, MAX*156` | `0x2af5f` | `0x2af60` | `0x270` |
| `Draw: 83 f8` `cmp eax, MAX` | `0x2b173` | `0x2b175` | `4` (imm8) |
| `MsgFunc: 83 ff` `cmp edi, MAX` | `0x2b243` | `0x2b245` | `4` (imm8) |
| `MsgFunc: 68` `push MAX*156` | `0x2b248` | `0x2b249` | `0x270` |
| `MsgFunc: bf` `mov edi, MAX-1` | `0x2b25f` | `0x2b260` | `3` |
| `Draw: c7 44 24 04` `mov [esp+4], 20` | `0x2aef0` | `0x2aef4` | `20` |
| `Draw: 83 c0` `add eax, 20` | `0x2af19` | `0x2af1b` | `20` (imm8) |

The sentinel is held apart from the other 33 because its value depends on the
line count, not just the base.

The last eight rows shipped in the first version as *instruction* addresses,
which the Rust code then read as operand addresses. `verify_stock` caught it in
game and refused to patch anything — `count at +0x2ae51 reads 0xc3b9, expected
0xc3`, which is the `b9` opcode byte read as part of the dword. Neither the unit
tests nor the first version of the verifier caught it: the tests only check the
arithmetic, and the verifier hardcoded its own `+1`/`+2` offsets instead of
checking the ones Rust uses. That gap is now closed — see below.

**The ceiling is 127**, set by the two `cmp r32, imm8` loop bounds. Widening
those instructions would overwrite the ones after them.

### Provenance of the HLAE comparison

What follows was derived by **observing `AfxHookGoldSrc.dll`'s compiled code** —
not from HLAE's source. No HLAE source was read, copied or adapted, and none of
it is present in this repository.

Worth stating plainly, because the rest of this document disassembles a *game*
binary and this is a different thing: `AfxHookGoldSrc.dll` is **HLAE's own work
product**, written by the advancedfx project. Reading it to learn how another
tool author solved a problem is not the same act as reading `client.dll`, even
where the copyright analysis lands in the same place. The conclusions below are
offered as credit rather than as a silent borrowing, which is also why HLAE is
named in `scan.rs` and `detour.rs` rather than only here.

What was taken from HLAE is a *technique* (detour a signature-scanned span) and a
*convention* (the `??` spelling of a byte pattern, kept identical so a signature
could move between the projects). The technique is a decades-old one, not
HLAE's invention; the convention is a notation. Everything specific here is our
own: `Y_PATTERN` was derived from DoD's `client.dll`, which HLAE has no patterns
for at all, and the stub steals different instructions from a different function.

Short excerpts of HLAE's disassembly and its pattern-key names appear above as
factual description of what the machine code does, for interoperability.

That keeps this the same line the project already draws elsewhere — see
`docs/` on the vendored `dem-patch` fork and the coldemoplayer reference:
**ideas yes, code no.** Two things would change the position and should be
checked before either happens:

- **Reading HLAE's source** — e.g. lifting its pattern database or its hooking
  framework rather than re-deriving equivalents. Then `advancedfx`'s licence
  governs, and it has to be read first. This install's `readme.txt` points at
  `./LICENSES/advancedfx/LICENSE`, which the shipped folder does not contain, so
  the authoritative copy is the one in `github.com/advancedfx/advancedfx`.
- **Contributing upstream.** A patch sent to HLAE is licensed on *their* terms,
  not this repository's. That is a deliberate choice to make when the time
  comes, not a consequence of having written the code here first.

### How HLAE does the same job, and how this differs

Worth recording, because the two approaches differ in kind rather than detail.
Read out of `AfxHookGoldSrc.dll`.

HLAE does not hardcode addresses. It **signature-scans** the loaded module for
each thing it needs, from a pattern database keyed by names like
`cstrike_CHudDeathNotice_Draw_YRes`. The CS pattern for the y site is:

```
8B 6E 18 8B 54 24 38 0F AF EB       mov ebp,[esi+0x18] / mov edx,[esp+0x38] / imul ebp,ebx
```

It stores both the matched **address** and its **size** — that is what the
`_DSZ` half of each pair is, computed as `end - start` right after the scan:

```asm
0x1001c5c4  sub  eax, ecx
0x1001c5c6  mov  [YRes], ecx        ; where
0x1001c5cd  mov  [YRes_DSZ], eax    ; how many bytes
```

Then it **detours** that whole region to a replacement stub, which reproduces
the original instructions but substitutes the y from a global when an override
is active, and jumps back to `address + size`:

```asm
0x100100a0  mov  ebp, [esi+0x18]        ; the original first instruction
0x100100a3  mov  dl, [g_bOffsetActive]
0x100100ab  je   +0x100100b5
0x100100ad  mov  edx, [g_iOffset]       ; the override
0x100100b3  jmp  +0x100100b9
0x100100b5  mov  edx, [esp+0x38]        ; else what the game computed
0x100100b9  imul ebp, [g_scale]
0x100100c0  jmp  [YRes + YRes_DSZ]      ; back into Draw
```

|  | HLAE | here |
| --- | --- | --- |
| finding the site | runtime signature scan | fixed RVAs, checked against the shipped bytes |
| applying it | code detour to a stub | rewrite the immediate in place |
| the value | a global the stub reads each frame | baked into the instruction |
| what it sets | the **final y**, overriding whatever branch produced it | one operand, inheriting the surrounding arithmetic |
| range | whatever a register holds | −128..127, from the `imm8` site |
| `default` | clear a flag; the stub defers to the game | write the shipped immediate back |

The consequential difference is the fourth row. HLAE replaces the *result*, so
its offset is an absolute y and behaves the same whichever branch ran. Patching
an immediate inherits its context — which is why `offset` here means an absolute
y on one path and an addend on top of ~95 on another, and why the spectator-mode-2
path cannot be reached at all. Moving to a detour would fix all three, at the
cost of a stub that has to be register-accurate against a specific build.

### No `_YRes` equivalent is needed

HLAE carries `cstrike_CHudDeathNotice_Draw_YRes` because CS computes the feed's
y differently. DoD's `Draw` starts at y = 20 and accumulates line height
*downward*, so more lines simply extend down the screen:

```asm
+0x2aeeb  mov eax, [0x19e88d4]
+0x2aef0  mov dword ptr [esp+4], 0x14      ; y = 20
+0x2aef8  test eax, eax
+0x2aefa  je  ...                          ; flag clear -> keep y = 20
+0x2af19  add eax, 0x14                    ; else ScreenHeight/480*42 + 20
```

`offset` does **not** patch either immediate. It detours, the way HLAE does,
for a reason the three code paths make obvious:

```asm
+0x2aeba  call <spectator mode>            ; 0 unless spectating
+0x2aebf  cmp  eax, 2
+0x2aec2  jne  +0x2aeeb
          ; mode 2: y from the spectator layout's own out-params -- no immediate
+0x2aee9  jmp  +0x2af1c
+0x2aeeb  mov  eax, [0x19e88d4]            ; the spectator-HUD flag
+0x2aef0  mov  dword ptr [esp+4], 20       ; an absolute y
+0x2aefa  je   +0x2af20                    ; flag clear -> done
+0x2aefc  fild [ScreenHeight]              ; flag set:
+0x2af02  fmul 0.00208333                  ;   / 480
+0x2af08  fmul 42.0
+0x2af0e  fadd 0.5                         ;   round
+0x2af19  add  eax, 20                     ; an addend, not a y
+0x2af1c  mov  dword ptr [esp+4], eax
+0x2af20  push ebx / ebp / esi / edi ; xor edi, edi    <- all three converge
```

Those two `20`s are the same number meaning different things: an absolute y on
one path, an addend on top of a screen-scaled ~95 on the other — so in a
spectated demo the feed starts near **115**, which is why a kill feed sits lower
when spectating. Writing one value to both operands therefore places the feed
correctly on at most one path, and mode 2 holds no immediate to write at all.

All three paths converge at `+0x2af20` with y in `[esp+4]`, so the detour goes
there and sets the **result**. One value, the same meaning everywhere, mode 2
included.

**The span.** `+0x2af20` is `53 55 56 57 33 ff` —
`push ebx / push ebp / push esi / push edi / xor edi, edi`. Six bytes: a
five-byte `jmp rel32` plus one `nop`. `Draw`'s only inbound branch into that
neighbourhood targets the first byte (from `+0x2aefa`), so nothing lands in the
padding. `eax` and the flags are both dead there — the y was stored one
instruction earlier and the next read of `eax` is a fresh load — so the stub may
use them.

**The stub**, hand-assembled in `offset_stub`:

```asm
cmp byte ptr [OFFSET_ACTIVE], 0
je  .game
mov eax, [OFFSET_VALUE]
mov dword ptr [esp + 4], eax     ; y = ours
.game:
push ebx / push ebp / push esi / push edi / xor edi, edi    ; the stolen bytes
jmp dword ptr [OFFSET_RESUME]    ; +0x2af26
```

Hand-written rather than copied, because copying the span would mean relocating
any relative branch inside it. `offset default` clears `OFFSET_ACTIVE` and the
game's own y flows through; nothing is ever unpatched, since there is no safe
moment to restore bytes a thread may be executing.

**Finding it.** `Y_PATTERN` is
`A1 ?? ?? ?? ?? C7 44 24 04 14 00 00 00 85 C0 74 24`, which matches exactly once
in `client.dll`'s code — `scan::find_unique` treats a second match as an error
rather than taking the first, since a signature that identifies two places has
not been proven. The convergence point is `match + 0x35`, and the bytes there
are checked against `Y_STOLEN` before anything is written.

## 4. The console surface

```
dodtools_deathmsg max <4..127>      lines shown at once (default 4)
dodtools_deathmsg offset <0..127>   y the feed starts at (default 20)
dodtools_deathmsg offset default    put the y back
dodtools_deathmsg block <id>...     hide frags involving these players
dodtools_deathmsg block !<id>...    hide everything EXCEPT these players
dodtools_deathmsg block clear       stop hiding anything
dodtools_deathmsg fake <killer> <victim> <weapon>
```

`max 4` restores the shipped bytes exactly, array included, so there is always a
clean way back. `fake`'s weapon takes a sprite name with or without the prefix
(`d_garand`, `garand`) or an index 1..=43.

A mixed `block` list — some plain ids, some `!`-prefixed — is refused rather
than guessed at, because "hide everyone except 3, and also hide 5" is two
different questions.

`fake` goes through the block list like any other notice. It did not at first,
on the reasoning that a message asked for by hand should not then be filtered;
that was wrong twice over. `block` is a filter on the feed and `fake` is a
source for it, so the exemption was the surprising behaviour rather than the
principled one — and it left `block` impossible to test without waiting for a
real kill, which is how the inconsistency surfaced. A blocked `fake` reports
that it was blocked; it is never silently dropped.

### Safety

Before the first write, every one of the 40 sites is checked against the value
the analysed build shipped, and a single mismatch refuses the whole operation.
That check is the only thing standing between a different `client.dll` and 40
writes into the middle of unrelated instructions, so it is exhaustive rather
than a spot check.

The relocated array is allocated once at the full 127-slot size, so changing
`max` rewrites only the operands and never moves the base under a demo that is
already running.

### Re-verifying the offsets

`goldsrc-hooks/tools/verify_deathmsg_offsets.py` re-derives the whole table from
a real `client.dll` and diffs it against `deathmsg.rs`; confirms every address
in every Rust table really *is* an encoded operand of the declared width holding
the shipped value; checks the ceiling still fits; then applies the patch in
memory for several line counts and checks both functions still decode to the
same instruction sequence. The second of those is the one that catches an
instruction address mistaken for an operand address. Run it after any change to
the tables:

```
python goldsrc-hooks/tools/verify_deathmsg_offsets.py [path-to-client.dll]
```

## 5. Live findings, 2026-09-16

The y detour is live-proven too, against an HLTV demo: `offset 20` puts the feed
at the very top, matching a POV demo exactly; `offset default` returns it to the
game's ~115; `offset 400` places it far down the screen, a value the old imm8
could not have held. The signature matched at `+0x2aeeb` in the running
`client.dll`, exactly where the offline verifier said it would, and the session
continued for minutes afterwards with no fault recorded.

Tested in a real session against an HLTV demo, and confirmed on screen: eight
and then ten death-notice lines rendering at once, against the four the game
ships with, with the feed's y visibly moved by `offset`. The relocated array
draws correctly — no garbage in the slots past the stock five, and the feed
survives the level's `InitHUDData`, which now clears the relocated buffer
because the reference it uses was patched along with everything else.

Three bugs surfaced between "the command reports success" and "the thing works",
all found live and all recorded below.

### The `.text` writes stick

`max` and `offset` patch `client.dll`'s own code through `VirtualProtect`, and
issue #204 had recorded `client.dll` behaving as though hardened, which nothing
offline could settle. It is settled: `max 8`, `offset 100` and `max 4` all
applied and reported success in a live session. Nothing refuses the write.

### `fake` needs a level loaded, and the failure is fatal rather than noisy

Run at the main menu, `fake` took the game down. GoldSrc swallows its own
unhandled exceptions and exits with no dump, no WER record and no event-log
entry, so the DLL carries a vectored exception handler
(`goldsrc-hooks/src/crash.rs`) purely to make crashes like this legible. It
caught:

```
CRASH: access violation at client.dll+0x20526 -- reading 0xbb8
  eip=...0526 eax=0x00000bb8 ecx=0x00000000
  [esp+0x00c] client.dll+0x2b21f     <- returning into MsgFunc_DeathMsg
```

`client.dll+0x20520` is three instructions:

```asm
+0x20520  call dword ptr [gEngfuncs + 0xcc]   ; GetLocalPlayer, slot 51
+0x20526  mov  eax, dword ptr [eax]           ; ->index
+0x20528  ret
```

`gViewPort->DeathMsg(killer, victim)` (`+0x802f0`) calls it **unconditionally**,
to compare the local player's index against the victim's and hide the scoreboard
on a match. With no level loaded there is no entity array, so the engine returns
an index computed off a null base — `0xbb8`, a multiple of `sizeof(cl_entity_t)`
— and `client.dll` dereferences it without checking.

Because the path is unconditional this is a property of `client.dll`, not of
this command: a *real* death notice arriving with no level loaded would crash
the same way. It simply never happens, because kills only arrive during play.

`fake` now reads that pointer itself and refuses with a message naming the
cause, rather than letting the game vanish.

### A faked notice has to be re-dated, because the HUD clock stops

`MsgFunc_DeathMsg` stamps `flDisplayTime` as `gHUD.m_flTime +
(int)hud_deathnotice_time`, and `Draw` deletes any entry whose stamp is older
than the frame time it is handed:

```asm
+0x2b3be  fild dword ptr [0x19c33d8]        ; (int)hud_deathnotice_time
+0x2b3c4  fadd dword ptr [0x1a080cc]        ; + gHUD.m_flTime
+0x2b3cc  fstp dword ptr [esi + 0x1a76668]  ; -> flDisplayTime

+0x2af4e  fld   dword ptr [edi + 0x1a76668]
+0x2af54  fcomp dword ptr [esp + 0x5c]      ; vs Draw's flTime argument
+0x2af5d  jp    ...                         ; else memmove the entry away
```

`m_flTime` is only refreshed by `CHud::Redraw`, which does not run while the
console is down — the same reason DoD's `cl_lw` suicide does not fire until the
console closes. So a notice typed at the console carries whatever time the HUD
last saw, and the first `Draw` after the console closes compares that stale
stamp against a live clock and drops it before drawing it once.

Observed exactly that way: several `fake`s then closing the console showed
nothing; closing, reopening, and typing one showed it, because the brief close
let `Redraw` catch `m_flTime` up.

A real notice never hits this — it arrives while the game is drawing — so
`fake` re-dates the slot it just filled from `GetClientTime` instead.

### Verification runs once per module, not once per command

The pre-flight check asks whether every site still holds the value the analysed
build shipped, so it is only meaningful *before* anything has been written. The
first version gated it on "have we changed the line count", which conflated two
different states: *never touched* and *put back the way it was*. Setting `max`
back to the stock 4 cleared the flag, so a later `max` re-ran the check against
a module where `offset 100` had legitimately written `0x64` — and refused:

```
max: this client.dll is not the build these offsets were derived from
     -- y offset at +0x2aef4 reads 0x64, expected 0x14
```

It is now keyed on the module base: verified once, and again only if
`client.dll` is reloaded at a different address, in which case the recorded
patch state is reset too rather than inherited.

### A note on reading the log

The `[demo N]` column in the DLL's log is client time since process start, not
demo playback position. It reads as though a demo is running when none is, which
is exactly how the crash above was misdiagnosed once before being pinned down.
