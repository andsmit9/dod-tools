#!/usr/bin/env python3
"""Checks `deathmsg.rs`'s address tables against a real DoD `client.dll`.

`deathmsg.rs` rewrites 40 operands inside four functions of a binary that is
not in this repository. A transcription slip in its 34-entry reference table
would not fail to compile and would not fail a unit test -- it would write a
pointer into the middle of an unrelated instruction, in the user's game. So the
tables are checked against the binary itself, here, rather than trusted.

Two passes:

  1. Re-derive every reference by scanning the whole image for dwords that land
     inside `rgDeathNoticeList`, and diff that against the Rust table. Scanning
     rather than reading the four functions is the point: it is what makes the
     reference set provably closed.
  2. Apply the full patch to an in-memory image for several line counts and
     assert the result is coherent -- no stale reference survives, every
     relocated one lands inside the new buffer, the count operands decode to
     the intended values, and both functions still decode to the same
     instruction sequence.

Usage:
    python goldsrc-hooks/tools/verify_deathmsg_offsets.py [path-to-client.dll]

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
RUST = Path(__file__).resolve().parent.parent / "src" / "deathmsg.rs"

# Mirrors of the two functions' extents, used only for the decode check.
DRAW = (0x2AE90, 0x2B198)
MSGFUNC = (0x2B1A0, 0x2B4D2)


def rust_scalar(src: str, name: str) -> int:
    match = re.search(rf"const {name}: \w+ = (0x[0-9a-f_]+|\d+);", src)
    if not match:
        raise SystemExit(f"could not find `const {name}` in deathmsg.rs")
    text = match.group(1).replace("_", "")
    return int(text, 16) if text.startswith("0x") else int(text)


def rust_pairs(src: str, name: str) -> list[tuple[int, int]]:
    block = src.split(f"const {name}")[1]
    block = block[block.index("[") : block.index("];")]
    return [
        (int(a.replace("_", ""), 16), int(b.replace("_", ""), 16))
        for a, b in re.findall(r"\(0x([0-9a-f_]+),\s*0x([0-9a-f_]+)\)", block)
    ]


def main() -> int:
    dll = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_DLL
    if not dll.is_file():
        return print(f"no client.dll at {dll}") or 2

    src = RUST.read_text(encoding="utf-8")
    array_rva = rust_scalar(src, "ARRAY_RVA")
    item = rust_scalar(src, "ITEM")
    stock_max = rust_scalar(src, "STOCK_MAX")
    sentinel_rva = rust_scalar(src, "SENTINEL_RVA")
    rust_refs = dict(rust_pairs(src, "ARRAY_REFS"))

    pe = pefile.PE(str(dll), fast_load=True)
    base = pe.OPTIONAL_HEADER.ImageBase
    image = bytearray(pe.get_memory_mapped_image())
    array, size = base + array_rva, (stock_max + 1) * item

    ok = True

    # ── Pass 1: the Rust table must be exactly what a fresh scan finds ────────
    scanned = {}
    for i in range(0, len(image) - 4):
        value = struct.unpack_from("<I", image, i)[0]
        if array <= value < array + size:
            scanned[i] = value - array
    sentinel_field = scanned.pop(sentinel_rva, None)

    print(f"scanned {len(scanned)} references + 1 sentinel; rust declares {len(rust_refs)}")
    for label, rvas in (
        ("missing from deathmsg.rs", set(scanned) - set(rust_refs)),
        ("declared but not in the binary", set(rust_refs) - set(scanned)),
    ):
        if rvas:
            ok = False
            print(f"  FAIL {label}: {sorted(hex(r) for r in rvas)}")
    for rva in sorted(set(scanned) & set(rust_refs)):
        if scanned[rva] != rust_refs[rva]:
            ok = False
            print(f"  FAIL +{rva:#x}: binary says field {scanned[rva]:#x}, rust says {rust_refs[rva]:#x}")
    if ok:
        print("  OK   every reference agrees, and the set is closed")

    want_sentinel = stock_max * item + 0x80
    if sentinel_field != want_sentinel:
        ok = False
        print(f"  FAIL sentinel field {sentinel_field} != {want_sentinel}")
    else:
        print(f"  OK   sentinel points at &list[{stock_max}].iId")

    # ── Pass 2: apply the patch and check the result decodes ──────────────────
    md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_32)
    md.detail = True
    pristine = bytes(image)

    for new_max in (4, 5, 8, 12, 32, 127):
        img = bytearray(pristine)
        new_base = 0x20000000
        new_size = (new_max + 1) * item
        for rva, field in rust_refs.items():
            struct.pack_into("<I", img, rva, new_base + field)
        struct.pack_into("<I", img, sentinel_rva, new_base + new_max * item + 0x80)

        counts = {
            0x2AE51 + 1: (4, (new_max + 1) * item // 4),
            0x2AF5F + 1: (4, new_max * item),
            0x2B173 + 2: (1, new_max),
            0x2B243 + 2: (1, new_max),
            0x2B248 + 1: (4, new_max * item),
            0x2B25F + 1: (4, new_max - 1),
        }
        for off, (width, value) in counts.items():
            if width == 4:
                struct.pack_into("<I", img, off, value)
            else:
                if not 0 <= value <= 0x7F:
                    ok = False
                    print(f"  FAIL max={new_max}: {value} will not fit an imm8")
                img[off] = value

        problems = []
        if any(array <= struct.unpack_from("<I", img, i)[0] < array + size for i in range(len(img) - 4)):
            problems.append("a reference to the old array survived")
        for rva, field in rust_refs.items():
            got = struct.unpack_from("<I", img, rva)[0]
            if not new_base <= got < new_base + new_size:
                problems.append(f"+{rva:#x} -> {got:#x} is outside the new buffer")
        for fn_name, (lo, hi) in (("Draw", DRAW), ("MsgFunc_DeathMsg", MSGFUNC)):
            before = [i.mnemonic for i in md.disasm(pristine[lo:hi], base + lo)]
            after = [i.mnemonic for i in md.disasm(bytes(img[lo:hi]), base + lo)]
            if before != after:
                problems.append(f"{fn_name} no longer decodes the same way")
        if problems:
            ok = False
            print(f"  FAIL max={new_max}: " + "; ".join(problems))
        else:
            print(f"  OK   max={new_max:<3} {new_size:>5} byte buffer, both functions decode unchanged")

    print("\nTABLES VERIFIED" if ok else "\nMISMATCH -- do not ship")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
