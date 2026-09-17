#!/usr/bin/env python3
"""Regenerates every table in `docs/goldsrc_client_dll_survey.md` from a real
DoD 1.3 `client.dll`.

The survey is a catalogue of what in DoD's client library can usefully be
changed from the companion DLL. A catalogue written by hand goes stale the
moment anything is re-checked, and there is no way to tell a transcription slip
from a finding. So the tables are *derived*, here, and the document says which
of its sections this script produces.

The method is the house one — `pefile` + `capstone`, offline, no running game
(see `docs/goldsrc_client_dll_internals.md` §10). What makes it go wide rather
than deep is that DoD's `client.dll` ships **MSVC RTTI**: every class' decorated
name is in `.data`, and walking type descriptor -> complete object locator ->
vftable gives each HUD element's virtual functions by name rather than by
search.

Sections:

    elements   the HUD element inventory: class, vftable, Draw, which virtuals
               are overridden, the user messages it hooks, the `ShouldDraw`
               sections it queries
    messages   all user messages hooked with `pfnHookUserMsg`, their handler
               thunks, and which element owns each
    cvars      every cvar the client registers or looks up, with the `cvar_t*`
               global it is kept in
    limits     fixed-size buffers, found by their bulk clear -- the
               `MAX_DEATHNOTICES`-shaped constants
    sections   what `CHud::ShouldDraw(n)` tests for each n

Usage:
    python goldsrc-hooks/tools/survey_client_dll.py [section...] [--dll PATH]

With no section named, prints all of them. Defaults to the pre-Anniversary
movies install. Requires `pefile` and `capstone` (`pip install pefile
capstone`); both are analysis-only and are not build dependencies of anything
in the workspace.
"""

import bisect
import re
import struct
import sys
from pathlib import Path

try:
    import pefile
    import capstone
except ImportError:  # pragma: no cover - developer tooling
    sys.exit("needs `pip install pefile capstone`")

DEFAULT_DLL = Path(
    r"C:\Program Files (x86)\Steam\steamapps\common"
    r"\Half-Life - PRE-Anniversary for Movies\dod\cl_dlls\client.dll"
)

# Established in docs/goldsrc_client_dll_internals.md §4 -- `Initialize` copies
# the engine's table here with `mov edi, 0x1a76388; mov ecx, 0x87; rep movsd`.
GENGFUNCS = 0x176388
GENGFUNCS_FIELDS = 135

# `cl_enginefunc_t` declaration order, for the slots this survey names.
SLOTS = {
    4: "pfnSPR_Set", 5: "pfnSPR_Draw", 6: "pfnSPR_DrawHoles", 7: "pfnSPR_DrawAdditive",
    11: "pfnFillRGBA", 12: "pfnGetScreenInfo", 14: "pfnRegisterVariable",
    15: "pfnGetCvarFloat", 16: "pfnGetCvarString", 17: "pfnAddCommand",
    18: "pfnHookUserMsg", 19: "pfnServerCmd", 20: "pfnClientCmd", 30: "pfnConsolePrint",
    37: "Cvar_SetValue", 38: "Cmd_Argc", 39: "Cmd_Argv", 51: "GetLocalPlayer",
    52: "GetViewModel", 53: "GetEntityByIndex", 66: "pfnWeaponAnim",
    72: "pfnGetCvarPointer", 88: "IsSpectateOnly",
}

# Two objects whose `this` is a fixed global, needed to resolve the cvar
# pointers those two keep as members rather than in their own `.data` slot.
# gHUD is `mov ecx, 0x1a080b0` at every one of its 270 call sites;
# CHudSpectator is `mov ecx, 0x1a15da8` before calls landing inside its own
# function range.
GHUD = 0x1080b0
CHUD_SPECTATOR = 0x115da8
CHUD_SPECTATOR_INIT = 0x37820

# `CHud::ShouldDraw(int)` -- the shared per-section HUD visibility predicate.
SHOULD_DRAW = 0x2CA80
# `CHud::AddHudElem(CHudBase*)` -- every element registers itself through this.
ADD_HUD_ELEM = 0x22330

# `CHudBase`'s vftable, in declaration order. Slots 3 and 7 are not guesses from
# the HL SDK header: `deathmsg.rs` independently patches `+0x2ae90` as
# `CHudDeathNotice::Draw` and `+0x2ae50` as its `InitHUDData`, and those are
# exactly where they land in that class' table. Slot 6 is an extra virtual DoD
# added that no element in this build overrides.
SLOT_NAMES = ["~dtor", "Init", "VidInit", "Draw", "Think", "Reset", "slot6", "InitHUDData"]

# `gHUD`'s own methods are not in any vftable -- `CHud` has no virtuals -- and
# nothing in the image calls some of them directly either, so the automatic
# attribution below cannot name them. These were identified by hand; the reason
# is recorded so each is a finding rather than a label.
KNOWN_FUNCTIONS = {
    # Registers 32 user messages with pfnHookUserMsg and then the client's
    # cvars and commands. A thiscall (`mov esi, ecx`) on gHUD.
    0x21100: "CHud::Init",
    # The per-frame HUD entry point: enforces five cvars, then walks the element
    # list at gHUD+0 calling each element's vftable slot 3.
    0x36E20: "CHud::Redraw",
    # Appends a {CHudBase*, next} node to the list at gHUD+0. 22 callers, which
    # is the element inventory.
    0x22330: "CHud::AddHudElem",
    # bool(int section) -- the shared per-section visibility predicate.
    0x2CA80: "CHud::ShouldDraw",
    # Returns _cl_minimap's value, but only while a spectator predicate holds
    # and gHUD's FOV field is still 90.
    0x228E0: "CHud::OverviewMapMode",
    # void(int* x, int* y, int* w, int* h) -- the overview map's rect for the
    # mode above; zeroes all four when the map is down.
    0x22A60: "CHud::GetOverviewMapBounds",
    # Recomputes both map rects from ScreenWidth/Height. Called from
    # CHudDoDMap::VidInit.
    0x22970: "CHud::ComputeOverviewMapRects",
    # (digit, x, y) with this in ecx -- draws one digit of the objective timer.
    0x30620: "CObjectiveIcons::DrawDigit",
}


class Image:
    def __init__(self, path):
        self.pe = pefile.PE(str(path), fast_load=True)
        self.base = self.pe.OPTIONAL_HEADER.ImageBase
        self.data = bytes(self.pe.get_memory_mapped_image())
        self.md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_32)
        self.md.detail = True
        self.sections = [
            (s.Name.rstrip(b"\0").decode(), s.VirtualAddress,
             max(s.Misc_VirtualSize, s.SizeOfRawData), s.Characteristics)
            for s in self.pe.sections
        ]

    def sect(self, rva):
        for name, va, size, _ in self.sections:
            if va <= rva < va + size:
                return name
        return "?"

    def code_ranges(self):
        return [(va, size) for _n, va, size, ch in self.sections if ch & 0x20000000]

    def u32(self, rva):
        return struct.unpack_from("<I", self.data, rva)[0]

    def cstr(self, rva, limit=512):
        end = self.data.find(b"\0", rva, rva + limit)
        if end < 0:
            return None
        try:
            return self.data[rva:end].decode("ascii")
        except UnicodeDecodeError:
            return None

    def find(self, needle):
        out, at = [], self.data.find(needle)
        while at >= 0:
            out.append(at)
            at = self.data.find(needle, at + 1)
        return out

    def xrefs_to(self, rva):
        return self.find(struct.pack("<I", self.base + rva))

    def disasm(self, start, end):
        return self.md.disasm(self.data[start:end], self.base + start)

    def function_end(self, start, limit=0x2000):
        """The `ret` that ends a linearly-decoded function.

        Decoding forwards from a known entry is the only reliable way to read
        this binary: a linear sweep of the whole `.text` desynchronises on the
        first inline data and silently misreads instructions afterwards.
        """
        at = start
        for ins in self.disasm(start, min(start + limit, len(self.data))):
            at = ins.address - self.base + ins.size
            if ins.mnemonic in ("ret", "retn") and self.data[at:at + 1] in (b"\xcc", b"\x90", b""):
                return at
        return min(start + limit, len(self.data))

    def calls_to(self, rva):
        """RVAs of `call rel32` instructions targeting `rva`."""
        out = []
        for start, size in self.code_ranges():
            blob = self.data[start:start + size]
            at = blob.find(b"\xe8")
            while at >= 0 and at + 5 <= len(blob):
                if start + at + 5 + struct.unpack_from("<i", blob, at + 1)[0] == rva:
                    out.append(start + at)
                at = blob.find(b"\xe8", at + 1)
        return out


# ── RTTI ─────────────────────────────────────────────────────────────────────

def type_descriptors(img):
    """{descriptor rva: decorated name}. A descriptor is {vfptr, spare, name}."""
    out = {}
    for m in re.finditer(rb"\.\?A[UV][\x20-\x7e]{1,120}\x00", img.data):
        rva = m.start()
        if img.sect(rva) in (".data", ".rdata"):
            out[rva - 8] = m.group()[:-1].decode("ascii")
    return out


def vftables(img):
    """{decorated name: [vftable rvas]}, via complete object locators."""
    out = {}
    for td, name in type_descriptors(img).items():
        for ref in img.xrefs_to(td):
            col = ref - 0xC  # pTypeDescriptor is COL+0xC
            if col < 0 or img.u32(col) not in (0, 1):
                continue
            for slot in img.xrefs_to(col):
                out.setdefault(name, []).append(slot + 4)
    return out


def vslots(img, vft, count=8):
    out = []
    for i in range(count):
        rva = img.u32(vft + i * 4) - img.base
        if not (0 <= rva < len(img.data)) or img.sect(rva) != ".text":
            break
        out.append(rva)
    return out


def demangle(name):
    core = name[4:]
    return core[:-2].replace("@", "::") if core.endswith("@@") else core.replace("@", "::")


# ── Engine-function call sites ───────────────────────────────────────────────

def slot_of(rva):
    if GENGFUNCS <= rva < GENGFUNCS + GENGFUNCS_FIELDS * 4 and (rva - GENGFUNCS) % 4 == 0:
        return (rva - GENGFUNCS) // 4
    return None


def indirect_calls(img):
    """[(call rva, engfunc slot)] for every `call dword ptr [gEngfuncs+N*4]`."""
    out = []
    for start, size in img.code_ranges():
        blob = img.data[start:start + size]
        at = blob.find(b"\xff\x15")
        while at >= 0 and at + 6 <= len(blob):
            idx = slot_of(struct.unpack_from("<I", blob, at + 2)[0] - img.base)
            if idx is not None:
                out.append((start + at, idx))
            at = blob.find(b"\xff\x15", at + 1)
    return out


def pushed_strings(img, call_rva, back=0x60):
    """Every `push imm32` before `call_rva` whose imm32 is a C string, in order."""
    lo = max(0, call_rva - back)
    out = []
    for ins in img.disasm(lo, call_rva):
        if ins.mnemonic == "push" and ins.op_str.startswith("0x"):
            rva = int(ins.op_str, 16) - img.base
            if 0 <= rva < len(img.data):
                text = img.cstr(rva)
                if text and len(text) >= 2 and text.isprintable():
                    out.append(text)
    return out


def pushed_code_pointers(img, call_rva, back=0x60):
    """Every `push imm32` before `call_rva` that points into `.text`."""
    lo = max(0, call_rva - back)
    out = []
    for ins in img.disasm(lo, call_rva):
        if ins.mnemonic == "push" and ins.op_str.startswith("0x"):
            rva = int(ins.op_str, 16) - img.base
            if 0 <= rva < len(img.data) and img.sect(rva) == ".text":
                out.append(rva)
    return out


def result_slot(img, call_rva, forward=0x30):
    """Where the call's return value is stored: an absolute `.data` rva.

    Handles both `mov [abs32], eax` and the `mov [this+disp], eax` form the two
    objects with a known `this` use.
    """
    for ins in img.disasm(call_rva, call_rva + forward):
        if ins.address - img.base == call_rva:
            continue
        if ins.bytes[:1] == b"\xa3":
            return struct.unpack_from("<I", ins.bytes, 1)[0] - img.base
        if ins.bytes[:2] in (b"\x89\x0d", b"\x89\x15", b"\x89\x35", b"\x89\x3d"):
            return struct.unpack_from("<I", ins.bytes, 2)[0] - img.base
        m = re.fullmatch(r"dword ptr \[e\w\w \+ (0x[0-9a-f]+)\], eax", ins.op_str)
        if ins.mnemonic == "mov" and m:
            disp = int(m.group(1), 16)
            this = CHUD_SPECTATOR if CHUD_SPECTATOR_INIT <= call_rva < CHUD_SPECTATOR_INIT + 0x1000 else GHUD
            return this + disp if img.sect(this + disp) == ".data" else None
        if ins.mnemonic in ("call", "ret", "jmp"):
            return None
    return None


class Owners:
    """Attributes an arbitrary rva to the function containing it.

    Entries come from direct call targets plus every RTTI vftable slot, which
    between them cover everything this survey needs to name. An rva inside a
    function nothing calls directly and no vftable lists is reported against the
    nearest known start, so `sub_*` labels are a floor rather than a claim.
    """

    def __init__(self, img):
        self.names = {}
        for decorated, vfts in vftables(img).items():
            for vft in vfts:
                for i, fn in enumerate(vslots(img, vft)):
                    label = SLOT_NAMES[i] if i < len(SLOT_NAMES) else f"slot{i}"
                    self.names.setdefault(fn, f"{demangle(decorated)}::{label}")
        self.names.update(KNOWN_FUNCTIONS)
        starts = set(self.names)
        for start, size in img.code_ranges():
            blob = img.data[start:start + size]
            at = blob.find(b"\xe8")
            while at >= 0 and at + 5 <= len(blob):
                t = start + at + 5 + struct.unpack_from("<i", blob, at + 1)[0]
                if 0 <= t < len(img.data) and img.sect(t) == ".text":
                    starts.add(t)
                at = blob.find(b"\xe8", at + 1)
        self.starts = sorted(starts)

    def of(self, rva):
        i = bisect.bisect_right(self.starts, rva) - 1
        if i < 0:
            return "?"
        start = self.starts[i]
        return self.names.get(start, f"sub_{start:#x}")


# ── Sections ─────────────────────────────────────────────────────────────────

def cvar_globals(img):
    """{`.data` rva: cvar name} for every cvar the client registers or looks up."""
    out = {}
    for rva, idx in indirect_calls(img):
        if idx not in (14, 72):
            continue
        names = pushed_strings(img, rva)
        slot = result_slot(img, rva)
        if names and slot:
            out[slot] = names[-1]
    return out


def hooked_messages(img, owners):
    """[(message, handler thunk rva, owning function)] for all 71."""
    out = []
    for rva, idx in indirect_calls(img):
        if idx != 18:
            continue
        names = pushed_strings(img, rva)
        handlers = pushed_code_pointers(img, rva)
        out.append((names[-1] if names else "?", handlers[-1] if handlers else None, owners.of(rva)))
    return sorted(out)


def should_draw_sites(img, owners):
    """[(call rva, section or None, owning function)].

    The section argument is recovered by decoding each *containing function*
    from its entry rather than backwards from the call: `push 1` is `6a 01`, two
    bytes that a backwards scan happily finds in the middle of something else.
    """
    out = []
    for call in sorted(img.calls_to(SHOULD_DRAW)):
        start = owners.starts[bisect.bisect_right(owners.starts, call) - 1]
        pending = None
        for ins in img.disasm(start, call + 5):
            if ins.mnemonic == "push" and re.fullmatch(r"(0x[0-9a-f]+|\d+)", ins.op_str):
                pending = int(ins.op_str, 0)
            elif ins.mnemonic == "call":
                if ins.address - img.base == call:
                    break
                pending = None
        out.append((call, pending if pending is not None and pending < 16 else None, owners.of(call)))
    return out


def elements(img, owners):
    """One row per HUD element, in vftable-name order."""
    messages = hooked_messages(img, owners)
    sites = should_draw_sites(img, owners)
    registrars = {owners.of(c) for c in img.calls_to(ADD_HUD_ELEM)}
    base_vft = vftables(img).get(".?AVCHudBase@@", [None])[0]
    base_slots = vslots(img, base_vft) if base_vft else []

    rows = []
    for decorated, vfts in vftables(img).items():
        name = demangle(decorated)
        for vft in vfts:
            slots = vslots(img, vft)
            if len(slots) < 8 or not base_slots:
                continue
            # A HUD element is whatever registered itself with AddHudElem.
            if f"{name}::Init" not in registrars and f"{name}::~dtor" not in registrars:
                continue
            draw = slots[3]
            rows.append({
                "name": name,
                "vft": vft,
                "slots": slots,
                "own": [SLOT_NAMES[i] for i, s in enumerate(slots) if s != base_slots[i]],
                "draw": draw,
                "draw_len": img.function_end(draw) - draw,
                "messages": [m for m, _h, o in messages if o.startswith(f"{name}::")],
                "sections": sorted({s for _c, s, o in sites if o.startswith(f"{name}::") and s}),
            })
    return sorted(rows, key=lambda r: r["name"])


def limits(img, owners):
    """Fixed-size buffers, found by the `rep stosd` that clears them."""
    rows = []
    for at in img.find(b"\xf3\xab"):
        if img.sect(at) != ".text":
            continue
        start = owners.starts[bisect.bisect_right(owners.starts, at) - 1]
        if at - start > 0x400:
            continue
        count = dest = None
        for ins in img.disasm(start, at + 2):
            if ins.mnemonic == "mov" and re.fullmatch(r"ecx, (0x[0-9a-f]+|\d+)", ins.op_str):
                count = int(ins.op_str.split(", ")[1], 0)
            elif ins.mnemonic in ("mov", "lea") and ins.op_str.startswith("edi, "):
                dest = ins.op_str.split(", ", 1)[1]
            elif ins.mnemonic == "call":
                count = None
        if count and count > 4:
            rows.append((at, count, dest, owners.of(at)))
    return sorted(rows, key=lambda r: -r[1])


# ── Reporting ────────────────────────────────────────────────────────────────

def report_elements(img, owners):
    print("== HUD elements ==")
    print("Everything that registered itself with CHud::AddHudElem (+%#x).\n" % ADD_HUD_ELEM)
    for row in elements(img, owners):
        print(f"{row['name']}")
        print(f"    vftable  +{row['vft']:#07x}   Draw +{row['draw']:#07x} ({row['draw_len']} bytes)")
        print(f"    own      {', '.join(row['own']) or '(inherits everything)'}")
        if row["sections"]:
            print(f"    sections {row['sections']}")
        if row["messages"]:
            print(f"    messages {', '.join(row['messages'])}")


def report_messages(img, owners):
    print("== user messages ==")
    rows = hooked_messages(img, owners)
    print(f"{len(rows)} hooked with pfnHookUserMsg.\n")
    for name, handler, owner in rows:
        where = f"+{handler:#07x}" if handler else "?"
        print(f"  {name:<16} thunk {where:<10} registered by {owner}")


def report_cvars(img):
    print("== cvars ==")
    cvars = cvar_globals(img)
    print(f"{len(cvars)} with a resolvable cvar_t* global.\n")
    for slot, name in sorted(cvars.items(), key=lambda kv: kv[1]):
        print(f"  {name:<28} cvar_t* at +{slot:#07x}")


def report_limits(img, owners):
    print("== fixed-size buffers ==")
    print("Found by the bulk clear that zeroes them, so the count is the one the")
    print("shipped code uses rather than one read off a header.\n")
    for at, count, dest, owner in limits(img, owners):
        print(f"  +{at:#08x}  {count:>6} dwords = {count * 4:>7} bytes  edi={str(dest):<24} {owner}")


def report_sections(img, owners):
    print("== CHud::ShouldDraw sections ==")
    for call, section, owner in should_draw_sites(img, owners):
        print(f"  +{call:#08x}  section {section if section is not None else '?'}   {owner}")


REPORTS = {
    "elements": lambda img, owners: report_elements(img, owners),
    "messages": lambda img, owners: report_messages(img, owners),
    "cvars": lambda img, _owners: report_cvars(img),
    "limits": lambda img, owners: report_limits(img, owners),
    "sections": lambda img, owners: report_sections(img, owners),
}


def main(argv):
    dll = DEFAULT_DLL
    if "--dll" in argv:
        i = argv.index("--dll")
        dll = Path(argv[i + 1])
        del argv[i:i + 2]
    wanted = [a for a in argv if not a.startswith("-")] or list(REPORTS)
    unknown = [w for w in wanted if w not in REPORTS]
    if unknown:
        return print(f"no section {unknown}; try {list(REPORTS)}") or 2
    if not dll.is_file():
        return print(f"no client.dll at {dll}") or 2

    img = Image(dll)
    owners = Owners(img)
    for i, name in enumerate(wanted):
        if i:
            print()
        REPORTS[name](img, owners)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
