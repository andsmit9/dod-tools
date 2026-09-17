#!/usr/bin/env python3
"""Checks `deathmsg.rs`'s address tables against a real DoD `client.dll`.

`deathmsg.rs` rewrites 40 operands inside four functions of a binary that is
not in this repository. A transcription slip would not fail to compile and
would not fail a unit test -- it would write into the middle of an unrelated
instruction, in the user's game. So the tables are checked against the binary
itself, here, rather than trusted.

Every address in the Rust tables is the address of an **encoded operand** --
an immediate or a displacement -- not of the instruction containing it. That
distinction is the whole point of pass 2: an earlier version of this script
hardcoded its own `+1`/`+2` operand offsets instead of checking the ones Rust
actually uses, so when `COUNT_SITES` and `OFFSET_SITES` held instruction
addresses while the Rust code read them as operand addresses, both this script
and the unit tests passed and the game refused to patch every site. Nothing
here may assume an offset the Rust source does not state.

Four passes:

  1. Re-derive every array reference by scanning the whole image for dwords
     that land inside `rgDeathNoticeList`, and diff that against the Rust
     table. Scanning rather than reading the four functions is the point: it
     is what makes the reference set provably closed.
  2. Disassemble the three functions linearly and confirm every address in
     every Rust table really is an encoded operand, of the declared width,
     holding the value the stock build ships.
  3. Check the ceiling: at MAX_LINES, every count still fits its operand.
  4. Apply the full patch to an in-memory image for several line counts and
     assert the result is coherent -- no stale reference survives, every
     relocated one lands inside the new buffer, and both functions still
     decode to the same instruction sequence.

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

# The functions holding every patch site, so they can be disassembled linearly
# from a known-good start rather than guessed at from the middle.
FUNCTIONS = {
    "InitHUDData": (0x2AE50, 0x2AE61),
    "Draw": (0x2AE90, 0x2B198),
    "MsgFunc_DeathMsg": (0x2B1A0, 0x2B4D2),
}
# The two whose instruction sequence must survive the patch unchanged.
PATCHED_FUNCTIONS = ("Draw", "MsgFunc_DeathMsg")


def rust_scalar(src: str, name: str) -> int:
    match = re.search(rf"const {name}: \w+ = (0x[0-9a-f_]+|\d+);", src)
    if not match:
        raise SystemExit(f"could not find `const {name}` in deathmsg.rs")
    text = match.group(1).replace("_", "")
    return int(text, 16) if text.startswith("0x") else int(text)


def rust_signed(src: str, name: str) -> int:
    """Like `rust_scalar`, but for a constant written with a leading minus."""
    match = re.search(rf"const {name}: \w+ = (-?\d+);", src)
    if not match:
        raise SystemExit(f"could not find `const {name}` in deathmsg.rs")
    return int(match.group(1))


def rust_block(src: str, name: str) -> str:
    block = src.split(f"const {name}")[1]
    return block[block.index("[") : block.index("];")]


def rust_array_refs(src: str) -> dict[int, int]:
    return {
        int(a.replace("_", ""), 16): int(b.replace("_", ""), 16)
        for a, b in re.findall(r"\(0x([0-9a-f_]+),\s*0x([0-9a-f_]+)\)", rust_block(src, "ARRAY_REFS"))
    }


def rust_count_sites(src: str) -> list[tuple[int, int, str]]:
    return [
        (int(rva.replace("_", ""), 16), int(width), kind)
        for rva, width, kind in re.findall(
            r"\(0x([0-9a-f_]+),\s*(\d+),\s*CountKind::(\w+)\)", rust_block(src, "COUNT_SITES")
        )
    ]


def rust_offset_sites(src: str) -> list[tuple[int, int]]:
    return [
        (int(rva.replace("_", ""), 16), int(width))
        for rva, width in re.findall(r"\(0x([0-9a-f_]+),\s*(\d+)\)", rust_block(src, "OFFSET_SITES"))
    ]


def count_value(kind: str, lines: int, item: int) -> int:
    """Mirrors `CountKind::value_for` in deathmsg.rs."""
    if kind == "MemsetDwords":
        return (lines + 1) * item // 4
    if kind == "MemmoveBytes":
        return lines * item
    if kind == "Max":
        return lines
    if kind == "MaxMinusOne":
        return lines - 1
    raise SystemExit(f"unknown CountKind::{kind} -- teach this script about it")


def encoded_operands(md, image: bytes, base: int) -> dict[int, tuple[int, int, str]]:
    """`{operand address: (width, value, disassembly)}` for every instruction in
    the three functions, decoded linearly from each function's entry.

    Both kinds of encoded field count, because the tables patch both: the
    counts are immediates (`mov ecx, 0xc3`), while most array references are
    *displacements* (`lea eax, [esi + 0x1a765d8]`, `mov eax, [0x1a765d8]`).
    An instruction can carry one of each, at different addresses."""
    found = {}
    for lo, hi in FUNCTIONS.values():
        for ins in md.disasm(image[lo:hi], base + lo):
            text = f"{ins.mnemonic} {ins.op_str}"
            for offset, size in (
                (ins.encoding.disp_offset, ins.encoding.disp_size),
                (ins.encoding.imm_offset, ins.encoding.imm_size),
            ):
                if not size:
                    continue
                address = ins.address - base + offset
                raw = image[address : address + size]
                found[address] = (size, int.from_bytes(raw, "little"), text)
    return found


def main() -> int:
    dll = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_DLL
    if not dll.is_file():
        return print(f"no client.dll at {dll}") or 2

    src = RUST.read_text(encoding="utf-8")
    array_rva = rust_scalar(src, "ARRAY_RVA")
    item = rust_scalar(src, "ITEM")
    stock_max = rust_scalar(src, "STOCK_MAX")
    max_lines = rust_scalar(src, "MAX_LINES")
    stock_offset = rust_scalar(src, "STOCK_OFFSET")
    max_offset = rust_scalar(src, "MAX_OFFSET")
    min_offset = rust_signed(src, "MIN_OFFSET")
    sentinel_rva = rust_scalar(src, "SENTINEL_RVA")
    rust_refs = rust_array_refs(src)
    count_sites = rust_count_sites(src)
    offset_sites = rust_offset_sites(src)

    pe = pefile.PE(str(dll), fast_load=True)
    base = pe.OPTIONAL_HEADER.ImageBase
    image = bytearray(pe.get_memory_mapped_image())
    array, size = base + array_rva, (stock_max + 1) * item

    md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_32)
    md.detail = True
    ok = True

    # -- Pass 1: the Rust reference table must be exactly what a fresh scan finds
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

    # -- Pass 2: every declared address must BE an immediate, of that width ----
    # This is the check the earlier version of this script did not do, and the
    # one that catches an address that names an instruction instead of its
    # operand -- which reads as a plausible number and patches garbage.
    print("\nevery declared address is an encoded operand of the declared width:")
    decoded = encoded_operands(md, bytes(image), base)
    checks = [(rva, width, count_value(kind, stock_max, item), f"count {kind}") for rva, width, kind in count_sites]
    checks += [(rva, width, stock_offset, "y offset") for rva, width in offset_sites]
    checks += [(sentinel_rva, 4, base + array_rva + stock_max * item + 0x80, "scan sentinel")]
    checks += [(rva, 4, base + array_rva + field, "array reference") for rva, field in sorted(rust_refs.items())]
    for rva, width, want, label in checks:
        if rva not in decoded:
            ok = False
            near = [hex(a) for a in decoded if abs(a - rva) <= 4]
            print(f"  FAIL +{rva:#x} ({label}) is not an encoded operand at all; operands nearby: {near}")
            continue
        got_width, got_value, text = decoded[rva]
        if got_width != width:
            ok = False
            print(f"  FAIL +{rva:#x} ({label}) is a {got_width}-byte immediate, rust writes {width}: {text}")
        elif got_value != want:
            ok = False
            print(f"  FAIL +{rva:#x} ({label}) reads {got_value:#x}, expected {want:#x}: {text}")
    if ok:
        print(f"  OK   all {len(checks)} operands decode and hold the shipped values")

    # -- Pass 3: the ceiling has to fit ---------------------------------------
    print("\nceiling:")
    for rva, width, kind in count_sites:
        value = count_value(kind, max_lines, item)
        limit = 0x7F if width == 1 else 0xFFFFFFFF
        verdict = "ok" if 0 <= value <= limit else "FAIL"
        if verdict == "FAIL":
            ok = False
        print(f"  {verdict:4} +{rva:#x} {kind} at max={max_lines} is {value} (imm{width * 8} holds <= {limit})")
    for rva, width in offset_sites:
        # The imm8 site is `83 c0 xx`, which sign-extends -- so its range is
        # signed, and a check against 0..0xff would wave through a value that
        # silently means something else.
        lo, hi = (-0x80, 0x7F) if width == 1 else (-(2**31), 2**31 - 1)
        verdict = "ok" if lo <= min_offset and max_offset <= hi else "FAIL"
        if verdict == "FAIL":
            ok = False
        print(f"  {verdict:4} +{rva:#x} y offset {min_offset}..{max_offset} (signed imm{width * 8} holds {lo}..{hi})")

    # -- Pass 4: apply the patch and check the result still decodes ------------
    print("\npatched images:")
    pristine = bytes(image)
    for new_max in (stock_max, 5, 8, 12, 32, max_lines):
        img = bytearray(pristine)
        new_base = 0x20000000
        new_size = (new_max + 1) * item
        for rva, field in rust_refs.items():
            struct.pack_into("<I", img, rva, new_base + field)
        struct.pack_into("<I", img, sentinel_rva, new_base + new_max * item + 0x80)
        for rva, width, kind in count_sites:
            value = count_value(kind, new_max, item)
            if width == 4:
                struct.pack_into("<I", img, rva, value)
            else:
                img[rva] = value

        problems = []
        if any(array <= struct.unpack_from("<I", img, i)[0] < array + size for i in range(len(img) - 4)):
            problems.append("a reference to the old array survived")
        for rva, field in rust_refs.items():
            got = struct.unpack_from("<I", img, rva)[0]
            if not new_base <= got < new_base + new_size:
                problems.append(f"+{rva:#x} -> {got:#x} is outside the new buffer")
        for name in PATCHED_FUNCTIONS:
            lo, hi = FUNCTIONS[name]
            before = [i.mnemonic for i in md.disasm(pristine[lo:hi], base + lo)]
            after = [i.mnemonic for i in md.disasm(bytes(img[lo:hi]), base + lo)]
            if before != after:
                problems.append(f"{name} no longer decodes the same way")
        if problems:
            ok = False
            print(f"  FAIL max={new_max}: " + "; ".join(problems))
        else:
            print(f"  OK   max={new_max:<3} {new_size:>5} byte buffer, both functions decode unchanged")

    print("\nTABLES VERIFIED" if ok else "\nMISMATCH -- do not ship")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
