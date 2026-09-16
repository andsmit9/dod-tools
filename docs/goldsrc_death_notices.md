# DoD 1.3 death notices: raising the line count, and the rest of `mirv_deathmsg`

> **Status 2026-09-16 — implemented, not yet live-tested.**
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

The full patch is 40 operands:

| what | RVA | stock |
| --- | --- | --- |
| 33 array references | various | `&rgDeathNoticeList + field` |
| scan sentinel `cmp eax, &list[MAX].iId` | `0x2b23d` | `array + 0x2f0` |
| `InitHUDData: mov ecx, (MAX+1)*156/4` | `0x2ae51` | `0xc3` |
| `Draw: mov ecx, MAX*156` | `0x2af5f` | `0x270` |
| `Draw: cmp eax, MAX` | `0x2b173` | `4` (imm8) |
| `MsgFunc: cmp edi, MAX` | `0x2b243` | `4` (imm8) |
| `MsgFunc: push MAX*156` | `0x2b248` | `0x270` |
| `MsgFunc: mov edi, MAX-1` | `0x2b25f` | `3` |

The sentinel is held apart from the other 33 because its value depends on the
line count, not just the base.

**The ceiling is 127**, set by the two `cmp r32, imm8` loop bounds. Widening
those instructions would overwrite the ones after them.

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

`offset` patches both immediates. The second is an `add eax, imm8`, which is
what caps the offset at 127 — about 286 real pixels at 1080p, since y here is
in screen pixels.

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
a real `client.dll` and diffs it against `deathmsg.rs`, then applies the patch
in memory for several line counts and checks both functions still decode to the
same instruction sequence. Run it after any change to the tables:

```
python goldsrc-hooks/tools/verify_deathmsg_offsets.py [path-to-client.dll]
```

## 5. What is not yet proven

**That the `.text` writes stick at runtime.** `max` and `offset` patch
`client.dll`'s own code through `VirtualProtect`. Issue #204 recorded
`client.dll` behaving as though hardened, and nothing offline can settle whether
a write is permitted in a live session. If it is not, `block` and `fake` are
unaffected — they touch no code — and `max`/`offset` report the failure rather
than silently doing nothing.

Everything in §§2–3 is derived from the shipped binaries and is independent of
that question.
