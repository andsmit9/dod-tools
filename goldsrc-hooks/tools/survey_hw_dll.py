#!/usr/bin/env python3
"""Maps what HLAE patches in GoldSrc's `hw.dll`, and checks our own findings
in it. Produces the tables in `docs/goldsrc_hw_dll_survey.md`.

`hw.dll` is the engine. Unlike `client.dll` — which HLAE barely touches for DoD,
having no `dod_` patterns at all — HLAE patches `hw.dll` heavily, and two hooks
over one span destroy each other. So the first job of any engine-side work is
**subtraction**: what does HLAE already own?

## Nothing of HLAE's is copied into this repository

This reads `AfxHookGoldSrc.dll` **from the user's own HLAE install, at run
time**. Its pattern strings are never written out, never committed, and never
used as signatures of ours. What the reports carry is the *result* of applying
them: addresses in the **game's** binary, which are facts about `hw.dll` rather
than anything of HLAE's. That is the same line
`docs/goldsrc_death_notices.md` draws — ideas yes, code no — and it is drawn
here deliberately rather than by accident.

Sections:

    keys       HLAE's complete key database, read out of its static
               initialisers: every name it resolves, whether it detours it,
               and which of its own hooks it attaches
    collide    each of HLAE's patterns applied to hw.dll -- the map of where
               HLAE writes, expressed as game-binary addresses
    findings   our own findings, re-checked against the shipped hw.dll: the
               command buffer's real size, ex_interp's clamp, and the
               self-naming error strings that name engine functions

Usage:
    python goldsrc-hooks/tools/survey_hw_dll.py [section...]
        [--hw PATH] [--afx PATH]

Defaults to the pre-Anniversary movies install and the pre-Anniversary HLAE.
`keys` and `collide` need HLAE present; `findings` does not. Requires `pefile`
and `capstone` (`pip install pefile capstone`); both are analysis-only and are
not build dependencies of anything in the workspace.
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

DEFAULT_HW = Path(
    r"C:\Program Files (x86)\Steam\steamapps\common"
    r"\Half-Life - PRE-Anniversary for Movies\hw.dll"
)
DEFAULT_AFX = Path(r"C:\Program Files (x86)\HLAE\HLAE (Pre-Anniversary)\AfxHookGoldSrc.dll")

# HLAE's key registrations are `push <name>; push <slot>; mov ecx, <map>; call`,
# one per key, emitted as C++ static initialisers.
KEY_REGISTRATION = re.compile(rb"\x68(....)\x68(....)\xb9(....)\xe8", re.S)

# A pattern in HLAE's own spelling, which `scan.rs` deliberately shares.
PATTERN_TEXT = re.compile(r"^(?:[0-9A-Fa-f]{2}|\?\?)(?: (?:[0-9A-Fa-f]{2}|\?\?))+$")

# `DetourAttach(&target, hook)` in AfxHookGoldSrc, identified by the three-call
# transaction shape around it (begin / update-thread / attach) and confirmed by
# the `.detourc`/`.detourd` sections the module carries.
DETOUR_ATTACH = 0x26070

# Our own findings, re-checked rather than restated. Each is (rva, expected
# bytes, what it is) -- a mismatch means this hw.dll is not the analysed build.
FINDINGS = [
    (
        0x272B0,
        bytes.fromhex("68004000"),
        "Cbuf_Init: SZ_Alloc(cmd_text, 0x4000) -- the command buffer is 16,384 bytes",
    ),
    (
        0x18EF8,
        bytes.fromhex("bb64000000"),
        "the ex_interp ceiling, 100 ms, as `mov ebx, 0x64`",
    ),
    (
        0x18F68,
        bytes.fromhex("bbc8000000"),
        "the raised ex_interp ceiling, 200 ms, as `mov ebx, 0xc8`",
    ),
]

# Error messages GoldSrc prints with the function's own name in them. This is
# the only naming this survey has for `hw.dll` -- it exports almost nothing
# useful and carries no RTTI, unlike `client.dll`.
SELF_NAMING = re.compile(
    r"^(Cbuf_|Cmd_|CL_|SV_|Host_|Mod_|R_|S_|SND_|Sys_|Con_|SCR_|V_|PM_|Netchan_|MSG_|COM_|Draw_|GL_|Key_)"
    r"[A-Za-z0-9_]*\s*:"
)


class Image:
    def __init__(self, path):
        self.path = Path(path)
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

    def cstr(self, rva, limit=512):
        end = self.data.find(b"\0", rva, rva + limit)
        if end < 0:
            return None
        try:
            return self.data[rva:end].decode("ascii")
        except UnicodeDecodeError:
            return None

    def strings(self, minlen=5):
        return {
            m.start(): m.group()[:-1].decode("ascii")
            for m in re.finditer(rb"[\x20-\x7e]{%d,}\x00" % minlen, self.data)
        }

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

    def match_pattern(self, text):
        rx = re.compile(
            b"".join(b"." if t == "??" else re.escape(bytes([int(t, 16)])) for t in text.split()),
            re.S,
        )
        return [m.start() for m in rx.finditer(self.data) if self.sect(m.start()) == ".text"]

    def calls_to(self, rva):
        out = []
        for start, size in self.code_ranges():
            blob = self.data[start:start + size]
            at = blob.find(b"\xe8")
            while at >= 0 and at + 5 <= len(blob):
                if start + at + 5 + struct.unpack_from("<i", blob, at + 1)[0] == rva:
                    out.append(start + at)
                at = blob.find(b"\xe8", at + 1)
        return out


# ── HLAE's database ──────────────────────────────────────────────────────────

def hlae_keys(afx):
    """{result slot rva: key name}, from the static initialisers."""
    out = {}
    for m in KEY_REGISTRATION.finditer(afx.data):
        if afx.sect(m.start()) != ".text":
            continue
        name = struct.unpack("<I", m.group(1))[0] - afx.base
        slot = struct.unpack("<I", m.group(2))[0] - afx.base
        text = afx.cstr(name) if 0 <= name < len(afx.data) else None
        if text and afx.sect(slot) == ".data":
            out[slot] = text
    return out


def hlae_patterns(afx):
    return {rva: text for rva, text in sorted(afx.strings().items()) if PATTERN_TEXT.match(text)}


def pattern_owner(afx, keys):
    """{pattern rva: key name}, by taking the next key-slot store after each use.

    The scan writes its result to the key's slot a couple of dozen bytes later,
    and the two sequences run in lockstep, so "the next store" is exact rather
    than a guess. Where it is not — a pattern used twice — both are reported.
    """
    stores = []
    for slot, name in keys.items():
        needle = struct.pack("<I", afx.base + slot)
        for at in afx.find(needle):
            if afx.sect(at) != ".text":
                continue
            if afx.data[at - 1:at] == b"\xa3" or afx.data[at - 2:at - 1] == b"\x89":
                stores.append((at, name))
    stores.sort()
    addrs = [a for a, _n in stores]

    out = {}
    for prva in hlae_patterns(afx):
        for ref in afx.xrefs_to(prva):
            if afx.sect(ref) != ".text":
                continue
            i = bisect.bisect_left(addrs, ref)
            if i < len(stores):
                out.setdefault(prva, []).append(stores[i][1])
    return out


def hlae_detours(afx, keys):
    """{key name: hook rva} for every key HLAE attaches a Detours hook to."""
    def pushes_before(rva, back=0x20):
        for lo in range(rva - back, rva):
            got, reached = [], False
            for ins in afx.disasm(lo, rva + 5):
                if ins.address - afx.base == rva:
                    reached = True
                    break
                if ins.mnemonic == "push" and ins.op_str.startswith("0x"):
                    got.append(int(ins.op_str, 16) - afx.base)
                elif ins.mnemonic in ("call", "jmp", "ret"):
                    got = []
            if reached and len(got) >= 2:
                return got[-2], got[-1]
        return None, None

    def source_keys(target, back=0x60):
        """Which key slot the target variable was copied from."""
        found = set()
        for at in afx.find(b"\xa3" + struct.pack("<I", afx.base + target)):
            if afx.sect(at) != ".text":
                continue
            window = afx.data[max(0, at - back):at]
            for m in re.finditer(b"\xa1", window):
                src = struct.unpack_from("<I", window, m.start() + 1)[0] - afx.base
                if src in keys:
                    found.add(keys[src])
        return sorted(found)

    out = {}
    for call in sorted(afx.calls_to(DETOUR_ATTACH)):
        hook, target = pushes_before(call)
        if target is None:
            continue
        names = [keys[target]] if target in keys else source_keys(target)
        # The copy chain can pick up neighbours; the last one written before the
        # attach is the right one, and the sequences run in order.
        if names:
            out.setdefault(names[-1], hook)
    return out


# ── Reports ──────────────────────────────────────────────────────────────────

def report_keys(hw, afx):
    keys = hlae_keys(afx)
    detoured = hlae_detours(afx, keys)
    print(f"== HLAE's key database: {len(keys)} keys ==")
    print(f"read from {afx.path.name}, which is not modified and nothing from which is stored\n")
    engine = [n for n in keys.values() if not n.startswith(("cstrike_", "tfc_", "valve_"))]
    game = [n for n in keys.values() if n.startswith(("cstrike_", "tfc_", "valve_"))]
    print(f"engine-side / generic: {len(engine)}   game-client-side: {len(game)}")
    print("  game-client keys are for cstrike, tfc and valve only -- there is no `dod_` key.\n")
    for slot, name in sorted(keys.items()):
        hook = detoured.get(name)
        mark = f"DETOURED, hook at afx+{hook:#x}" if hook else "resolved only"
        print(f"  {name:<46} {mark}")


def report_collide(hw, afx):
    keys = hlae_keys(afx)
    owners = pattern_owner(afx, keys)
    detoured = hlae_detours(afx, keys)
    print("== where HLAE's patterns land in this hw.dll ==")
    print("Addresses are in the GAME binary. A unique match is a place HLAE writes or")
    print("reads; several matches means the pattern is used with a restricted search")
    print("range we do not reproduce, so the address is not pinned here.\n")
    for prva, text in sorted(hlae_patterns(afx).items()):
        names = owners.get(prva, [])
        name = names[0] if names else "?"
        if name.startswith(("cstrike_", "tfc_", "valve_")):
            continue
        hits = hw.match_pattern(text)
        where = f"hw+{hits[0]:#x}" if len(hits) == 1 else f"{len(hits)} matches"
        mark = " [detoured]" if name in detoured else ""
        print(f"  {name:<46} {len(text.split()):>4} bytes -> {where}{mark}")


def report_findings(hw, _afx):
    print("== our own findings, re-checked against this hw.dll ==\n")
    ok = True
    for rva, want, what in FINDINGS:
        got = hw.data[rva:rva + len(want)]
        verdict = "OK  " if got == want else "FAIL"
        if got != want:
            ok = False
        print(f"  {verdict} hw+{rva:#08x}  {what}")
        if got != want:
            print(f"         expected {want.hex(' ')}, found {got.hex(' ')}")

    print("\n== functions hw.dll names in its own error messages ==")
    print("The only naming available: hw.dll carries no RTTI and exports almost")
    print("nothing, unlike client.dll.\n")
    names = {}
    for rva, text in sorted(hw.strings().items()):
        m = SELF_NAMING.match(text)
        if not m:
            continue
        refs = [r for r in hw.xrefs_to(rva) if hw.sect(r) == ".text"]
        names.setdefault(m.group(0).rstrip(": ").strip(), []).extend(refs)
    for name, refs in sorted(names.items()):
        where = ", ".join(f"+{r:#x}" for r in sorted(set(refs))[:3]) or "(no direct reference)"
        print(f"  {name:<34} referenced from {where}")
    return ok


REPORTS = {"keys": report_keys, "collide": report_collide, "findings": report_findings}


def main(argv):
    hw_path, afx_path = DEFAULT_HW, DEFAULT_AFX
    for flag, setter in (("--hw", "hw"), ("--afx", "afx")):
        if flag in argv:
            i = argv.index(flag)
            if setter == "hw":
                hw_path = Path(argv[i + 1])
            else:
                afx_path = Path(argv[i + 1])
            del argv[i:i + 2]
    wanted = [a for a in argv if not a.startswith("-")] or list(REPORTS)
    unknown = [w for w in wanted if w not in REPORTS]
    if unknown:
        return print(f"no section {unknown}; try {list(REPORTS)}") or 2
    if not hw_path.is_file():
        return print(f"no hw.dll at {hw_path}") or 2

    hw = Image(hw_path)
    afx = None
    if any(w in ("keys", "collide") for w in wanted):
        if not afx_path.is_file():
            return print(f"no AfxHookGoldSrc.dll at {afx_path} -- `findings` works without it") or 2
        afx = Image(afx_path)

    for i, name in enumerate(wanted):
        if i:
            print()
        REPORTS[name](hw, afx)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
