#!/usr/bin/env python3
"""Build the trimmed IWAD that `kit_wad_xip` embeds.

The blob is not committed: it is ~1.8 MB of derived data and git is the wrong
place for it. Run this instead.

    python3 examples/assets/make_wad.py path/to/freedoom1.wad

Freedoom is BSD-3 licensed and redistributable; any IWAD works.

The size ceiling is not arbitrary. `elf2tab` pads a TBF to a power of two, so
the usable app flash is the largest power of two that fits the board's `prog`
region -- 2 MiB on a Pico 2 W whose region is 3520 KiB. The budget below leaves
room for code inside that 2 MiB.

Lumps are taken in file order until the budget is reached, so the result is the
right SHAPE and SIZE but is not playable: nothing checks that a map's textures
came along with it. Making a genuinely minimal playable WAD is the offline
converter's job, which is a separate piece of work.
"""
import struct
import sys

BUDGET = 1_900_000

def main(path, out):
    with open(path, "rb") as src:
        magic, numlumps, ofs = struct.unpack("<4sII", src.read(12))
        if magic not in (b"IWAD", b"PWAD"):
            sys.exit(f"{path}: not a WAD ({magic!r})")
        src.seek(ofs)
        raw = src.read(numlumps * 16)
        entries = [struct.unpack("<II8s", raw[i * 16:(i + 1) * 16])
                   for i in range(numlumps)]

        kept, total = [], 12
        for pos, size, name in entries:
            if total + size + 16 > BUDGET:
                continue
            src.seek(pos)
            kept.append((name, src.read(size)))
            total += size + 16

    body, directory, off = b"", [], 12
    for name, data in kept:
        directory.append((off, len(data), name))
        body += data
        off += len(data)

    table = b"".join(struct.pack("<II8s", p, s, n) for p, s, n in directory)
    wad = struct.pack("<4sII", b"IWAD", len(kept), 12 + len(body)) + body + table
    with open(out, "wb") as f:
        f.write(wad)
    print(f"{out}: {len(kept)} lumps, {len(wad)} bytes ({len(wad)/1024/1024:.2f} MiB)")

if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else "examples/assets/wad_trim.bin")
