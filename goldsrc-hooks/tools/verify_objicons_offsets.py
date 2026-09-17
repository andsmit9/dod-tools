#!/usr/bin/env python3
"""Checks `objicons.rs`'s two detours against a real DoD `client.dll`.

`objicons.rs` writes a five-byte jump into the middle of
`CObjectiveIcons::Draw`, twice, in a binary that is not in this repository.
Nothing about that is checkable by a unit test: the test suite can prove each
stub assembles to the bytes its comment claims, but not that those bytes
belong where they are being written. So the claims that rest on the binary are
checked against the binary, here.

Per detour:

  1. The signature matches the shipped `client.dll` exactly once. A pattern
     that matches twice has not identified anything, which is why
     `scan::find_unique` treats a second hit as an error rather than a choice.
  2. `*_DETOUR_AT` bytes past the match really are the instructions `*_STOLEN`
     reproduces -- so a build that moved the convergence point fails loudly
     instead of having a jump written over the middle of something else.
  3. Nothing branches into the interior of the span the jump overwrites, and
     no dword anywhere in the image points into it either. Checked over the
     whole image, not just the one function, because an inbound branch from
     somewhere unexpected is exactly the case a local check would miss.
  4. The span is long enough for the `jmp rel32` that replaces it.
  5. The two spans do not overlap each other.

And once, for the icon row:

  6. The stack slots the row's stub writes are the ones the function actually
     uses for the icon row's x and y: both written before the convergence
     point and read after it, which is what makes one write per axis enough.

Every constant comes out of `objicons.rs` rather than being restated, for the
reason recorded in `verify_deathmsg_offsets.py`: a check that carries its own
copy of the thing it is checking proves only that the copy is self-consistent.

Usage:
    python goldsrc-hooks/tools/verify_objicons_offsets.py [path-to-client.dll]

Defaults to the pre-Anniversary movies install. Requires `pefile` and
`capstone` (`pip install pefile capstone`); both are analysis-only and are not
build dependencies of anything in the workspace.
"""

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
RUST = Path(__file__).resolve().parent.parent / "src" / "objicons.rs"

# `CObjectiveIcons::Draw`, so it can be disassembled linearly from a known-good
# entry rather than guessed at from the middle. The entry is vftable slot 3 of
# the class' RTTI-named vftable; the end is the `ret` that follows.
DRAW = (0x30030, 0x30620)

# The two stack slots the icon row's position lives in, as `objicons.rs` names
# them. Listed here as the *expected* pair so a silent swap in the Rust is
# caught: writing the x to the y's slot would place the row correctly along one
# axis and nowhere sensible along the other.
EXPECTED_SLOTS = {"X_SLOT": 0x14, "Y_SLOT": 0x18}

# The detours, by the prefix their constants share in objicons.rs.
DETOURS = ("ROW", "TIMER")


def rust_scalar(src: str, name: str) -> int:
    match = re.search(rf"const {name}: \w+ = (0x[0-9a-f_]+|\d+);", src)
    if not match:
        raise SystemExit(f"could not find `const {name}` in objicons.rs")
    text = match.group(1).replace("_", "")
    return int(text, 16) if text.startswith("0x") else int(text)


def rust_pattern(src: str, name: str) -> str:
    """A `&str` const, which may be written as a `\\`-continued literal."""
    match = re.search(rf'const {name}: &str = "(.*?)";', src, re.S)
    if not match:
        raise SystemExit(f"could not find `const {name}` in objicons.rs")
    # Collapse the line continuations the source uses to keep the line short.
    return " ".join(match.group(1).replace("\\", " ").split())


def rust_bytes(src: str, name: str) -> bytes:
    block = src.split(f"const {name}")[1]
    block = block[block.index("[") : block.index("];")]
    return bytes(int(b, 16) for b in re.findall(r"0x([0-9a-fA-F]{2})", block))


def branch_targets(pe, image, base, md):
    """{target rva: [source rvas]} for every direct branch in executable code."""
    found = {}
    for section in pe.sections:
        if not section.Characteristics & 0x20000000:  # IMAGE_SCN_MEM_EXECUTE
            continue
        lo = section.VirtualAddress
        hi = lo + max(section.Misc_VirtualSize, section.SizeOfRawData)
        for ins in md.disasm(image[lo:hi], base + lo):
            groups = ins.groups
            if capstone.x86.X86_GRP_JUMP not in groups and capstone.x86.X86_GRP_CALL not in groups:
                continue
            for op in ins.operands:
                if op.type == capstone.x86.X86_OP_IMM:
                    found.setdefault(op.imm - base, []).append(ins.address - base)
    return found


def main() -> int:
    dll = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_DLL
    if not dll.is_file():
        return print(f"no client.dll at {dll}") or 2

    src = RUST.read_text(encoding="utf-8")
    pe = pefile.PE(str(dll), fast_load=True)
    base = pe.OPTIONAL_HEADER.ImageBase
    image = bytes(pe.get_memory_mapped_image())
    md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_32)
    md.detail = True
    ok = True

    targets = branch_targets(pe, image, base, md)
    spans = {}

    for prefix in DETOURS:
        pattern = rust_pattern(src, f"{prefix}_PATTERN")
        detour_at = rust_scalar(src, f"{prefix}_DETOUR_AT")
        stolen = rust_bytes(src, f"{prefix}_STOLEN")
        print(f"\n== {prefix} ==")

        # -- 1. the signature identifies exactly one place --------------------
        rx = re.compile(
            b"".join(b"." if t == "??" else re.escape(bytes([int(t, 16)])) for t in pattern.split()),
            re.S,
        )
        matches = [m.start() for m in rx.finditer(image)]
        if len(matches) != 1:
            print(f"FAIL the signature matches {len(matches)} times: {[hex(m) for m in matches]}")
            ok = False
            continue
        print(f"OK   the signature matches exactly once, at +{matches[0]:#x}")

        target = matches[0] + detour_at
        spans[prefix] = range(target, target + len(stolen))
        print(f"     the convergence point is +{target:#x}")

        # -- 2. the span holds what the stub reproduces -----------------------
        present = image[target : target + len(stolen)]
        if present == stolen:
            print(f"OK   +{target:#x} holds the {len(stolen)} bytes the stub reproduces")
        else:
            ok = False
            print(f"FAIL +{target:#x} holds {present.hex(' ')}, rust reproduces {stolen.hex(' ')}")

        # -- 3. nothing lands inside the span ---------------------------------
        interior = range(target + 1, target + len(stolen))
        inbound = [(s, t) for t in interior for s in targets.get(t, ())]
        if inbound:
            ok = False
            for src_rva, dst in inbound:
                print(f"FAIL +{src_rva:#x} branches to +{dst:#x}, inside the span the jump overwrites")
        else:
            print("OK   nothing in any executable section branches into the span")

        pointing = [
            rva
            for rva in range(0, len(image) - 4)
            if struct.unpack_from("<I", image, rva)[0] - base in interior
        ]
        if pointing:
            ok = False
            print(f"FAIL dwords at {[hex(r) for r in pointing]} point inside the span")
        else:
            print("OK   no dword in the image points inside the span")

        # -- 4. room for the jump ---------------------------------------------
        if len(stolen) < 5:
            ok = False
            print(f"FAIL the span is {len(stolen)} bytes; a near jump needs 5")
        else:
            print(f"OK   {len(stolen)} bytes leaves room for the 5-byte jump")

    # -- 5. the two detours do not overwrite each other -----------------------
    print("\n== together ==")
    if len(spans) == len(DETOURS):
        a, b = (spans[p] for p in DETOURS)
        if set(a) & set(b):
            ok = False
            print(f"FAIL the two spans overlap: {a} and {b}")
        else:
            print(f"OK   the two spans are disjoint (+{a.start:#x} and +{b.start:#x})")

    # -- 6. the stack slots really are the icon row's x and y -----------------
    # The claim being checked: both slots are assigned before the row's
    # convergence point and read after it. That is what makes one write per
    # axis place the whole row -- if either were re-derived inside the
    # per-objective loop the override would be discarded on the second icon.
    slots = {name: rust_scalar(src, name) for name in EXPECTED_SLOTS}
    row = spans.get("ROW")
    if row:
        writes, reads = {name: [] for name in slots}, {name: [] for name in slots}
        for ins in md.disasm(image[DRAW[0] : DRAW[1]], base + DRAW[0]):
            rva = ins.address - base
            for name, slot in slots.items():
                if f"[esp + {slot:#x}]" not in ins.op_str:
                    continue
                target_list = writes if ins.op_str.startswith(f"dword ptr [esp + {slot:#x}]") else reads
                target_list[name].append(rva)
        for name, slot in slots.items():
            before = [w for w in writes[name] if w < row.start]
            after = [r for r in reads[name] if r > row.start]
            late = [w for w in writes[name] if w > row.start]
            if before and after:
                print(
                    f"OK   {name} (+{slot:#x}) is written {len(before)}x before the detour"
                    f" and read {len(after)}x after it"
                )
            else:
                ok = False
                print(
                    f"FAIL {name} (+{slot:#x}) writes={[hex(w) for w in writes[name]]}"
                    f" reads={[hex(r) for r in reads[name]]}"
                )
            if late:
                # Not a failure. Two kinds of late write are expected: the
                # loop's own `x += icon_width + 2` advance, and the overview-map
                # path, which deliberately re-places each icon from the
                # objective's own record and which `objicons.rs` documents it
                # does not reach. Printed so a *new* late write shows up rather
                # than passing unnoticed.
                print(f"     note: {name} is also written after the detour at {[hex(w) for w in late]}")

    for name, want in EXPECTED_SLOTS.items():
        if slots[name] != want:
            ok = False
            print(f"FAIL {name} is {slots[name]:#x} in objicons.rs; this build's is {want:#x}")

    print("\nDETOURS VERIFIED" if ok else "\nMISMATCH -- do not ship")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
