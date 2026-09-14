#!/usr/bin/env python3
"""Build a minimal, PLAYABLE IWAD holding one map.

The sibling `make_wad.py` takes lumps in file order until a byte budget is
reached. That produces the right size for a flash-residency test and nothing
else: the map it keeps has no textures. This one keeps a map and exactly what
that map needs.

    python3 make_min_wad.py freedoom1.wad -m E1M1 -o out/freedoom1.wad

Why it matters for RAM, not just flash: `R_InitTextures` builds tables over
every texture the WAD *defines*, not the ones a map uses. Freedoom1 defines 963
and E1M1 references 114. Measured on the headless build against the full
freedoom1.wad, those tables plus `R_GenerateLookup` came to 738,928 bytes, the
largest class of non-purgeable allocation Doom makes and larger than the whole
of E1M1's level geometry. Rewriting TEXTURE1 is therefore a RAM lever, not a
disk one.

What it keeps
  - the map's eleven lumps, renamed to E1M1 by default (see --as)
  - PLAYPAL, COLORMAP, and anything named with --extra
  - a rewritten TEXTURE1 holding only referenced textures, plus whole
    animation ranges (see below), with a rewritten PNAMES and remapped indices
  - the patch lumps those textures name
  - the flats the map's sectors name
  - every sprite lump, untouched. Sprite DATA is a flash problem, not a RAM
    one -- it reaches RAM only as PU_CACHE, which is purgeable. The sprite
    tables `R_InitSpriteDefs` builds are PU_STATIC, but they measured 16,336
    bytes, so trimming them is not where the RAM is.

What the CODE requires, which no map says
  - every switch texture in `alphSwitchList` for the episode: P_InitSwitchList
    calls R_TextureNumForName on both halves of every pair, and that I_Errors
  - SKY1..SKY4: G_DoLoadLevel picks one by episode, whichever the map uses
  - a pile of status bar and font graphics, which is what discover_lumps.sh
    finds by running the engine instead of guessing

Both tables are read out of the engine source at run time -- `--engine` -- so
this tool cannot drift from the Doom it is feeding.

Animation ranges are the trap. `P_InitPicAnims` animates the numeric range
between two named textures, so it walks whatever happens to sit between them.
Trimming reorders that range and the animation then walks unrelated textures,
or `I_Error`s outright when the end lands before the start. So a kept range is
kept whole and in its original relative order.
"""
import argparse
import struct
import sys

MAP_LUMPS = ("THINGS", "LINEDEFS", "SIDEDEFS", "VERTEXES", "SEGS",
             "SSECTORS", "NODES", "SECTORS", "REJECT", "BLOCKMAP")

ENGINE_DEFAULT = "~/forge/doomgeneric/doomgeneric"


def parse_animdefs(engine):
    """animdefs[] out of p_spec.c, so this tool cannot drift from the engine.

    Returns [(istexture, endname, startname)]. P_InitPicAnims keys off the
    START name existing and then requires the END name, so a kept group has to
    be kept whole.
    """
    import os
    import re
    src = open(os.path.join(os.path.expanduser(engine), "p_spec.c")).read()
    body = re.search(r"animdef_t\s+animdefs\[\]\s*=\s*\{(.*?)\n\};", src, re.S)
    if not body:
        sys.exit("could not find animdefs[] in p_spec.c")
    out = []
    for line in body.group(1).splitlines():
        m = re.match(r'\s*\{\s*(true|false)\s*,\s*"([^"]*)"\s*,\s*"([^"]*)"', line)
        if m:
            out.append((m.group(1) == "true", m.group(2).upper(), m.group(3).upper()))
    if not out:
        sys.exit("animdefs[] parsed to nothing")
    return out


def parse_switchlist(engine, episode):
    """alphSwitchList[] out of p_switch.c.

    P_InitSwitchList calls R_TextureNumForName -- which I_Errors -- on BOTH
    halves of every pair whose episode is <= the current one, whether or not
    the map uses them. So these are required by the code, not by the map.
    """
    import os
    import re
    src = open(os.path.join(os.path.expanduser(engine), "p_switch.c")).read()
    body = re.search(r"switchlist_t\s+alphSwitchList\[\]\s*=\s*\{(.*?)\n\};", src, re.S)
    if not body:
        sys.exit("could not find alphSwitchList[] in p_switch.c")
    need = set()
    for line in body.group(1).splitlines():
        m = re.match(r'\s*\{\s*"([^"]*)"\s*,\s*"([^"]*)"\s*,\s*(\d+)', line)
        # episode 0 terminates the table -- P_InitSwitchList stops there.
        if m and 1 <= int(m.group(3)) <= episode:
            need.add(m.group(1).upper())
            need.add(m.group(2).upper())
    if not need:
        sys.exit("alphSwitchList[] parsed to nothing")
    return need


def read_wad(path):
    data = open(path, "rb").read()
    magic, numlumps, ofs = struct.unpack("<4sII", data[:12])
    if magic not in (b"IWAD", b"PWAD"):
        sys.exit(f"{path}: not a WAD ({magic!r})")
    dirents = []
    for i in range(numlumps):
        pos, size, raw = struct.unpack("<II8s", data[ofs + i * 16:ofs + i * 16 + 16])
        dirents.append((raw.rstrip(b"\0").decode("latin1").upper(), pos, size))
    return data, dirents


def namespaces(dirents):
    """Map each lump index to the namespace marker region it sits in."""
    out, stack = {}, []
    for i, (name, _, _) in enumerate(dirents):
        if name.endswith("_START") and len(name) <= 8:
            stack.append(name[:-6])
            continue
        if name.endswith("_END") and len(name) <= 8:
            tag = name[:-4]
            # P1_END closes P1_START; P_END closes P_START.
            if stack and stack[-1] == tag:
                stack.pop()
            continue
        if stack:
            out[i] = stack[0]
    return out


def parse_textures(data, dirents, byname):
    """Return [(name, [(patchindex, x, y), ...], width, height)] in WAD order."""
    if "PNAMES" not in byname:
        sys.exit("no PNAMES")
    pos, size = byname["PNAMES"]
    (npatches,) = struct.unpack("<I", data[pos:pos + 4])
    pnames = [data[pos + 4 + i * 8:pos + 12 + i * 8].rstrip(b"\0").decode("latin1").upper()
              for i in range(npatches)]
    textures = []
    for lump in ("TEXTURE1", "TEXTURE2"):
        if lump not in byname:
            continue
        pos, size = byname[lump]
        (count,) = struct.unpack("<I", data[pos:pos + 4])
        offs = struct.unpack(f"<{count}i", data[pos + 4:pos + 4 + count * 4])
        for o in offs:
            base = pos + o
            name = data[base:base + 8].rstrip(b"\0").decode("latin1").upper()
            w, h = struct.unpack("<hh", data[base + 12:base + 16])
            (pc,) = struct.unpack("<h", data[base + 20:base + 22])
            patches = []
            for k in range(pc):
                px, py, pi = struct.unpack("<hhh", data[base + 22 + k * 10:base + 28 + k * 10])
                patches.append((pi, px, py))
            textures.append((name, patches, w, h))
    return pnames, textures


def map_references(data, dirents, start):
    """Textures and flats named by one map's SIDEDEFS and SECTORS."""
    lumps = {}
    for i in range(start + 1, min(start + 12, len(dirents))):
        name, pos, size = dirents[i]
        if name not in MAP_LUMPS:
            break
        lumps[name] = (pos, size)
    tex, flats = set(), set()
    pos, size = lumps["SIDEDEFS"]
    for k in range(size // 30):
        r = data[pos + k * 30:pos + k * 30 + 30]
        for o in (4, 12, 20):
            tex.add(r[o:o + 8].rstrip(b"\0").decode("latin1").upper())
    pos, size = lumps["SECTORS"]
    for k in range(size // 26):
        r = data[pos + k * 26:pos + k * 26 + 26]
        flats.add(r[4:12].rstrip(b"\0").decode("latin1").upper())
        flats.add(r[12:20].rstrip(b"\0").decode("latin1").upper())
    tex.discard("-")
    return tex, flats


def expand_animations(kept, all_names, istexture, animdefs):
    """Keep whole animation ranges: P_InitPicAnims walks the numeric range."""
    order = {n: i for i, n in enumerate(all_names)}
    grown = set(kept)
    for is_tex, last, first in animdefs:
        if bool(is_tex) != bool(istexture):
            continue
        if first not in order or last not in order:
            continue
        lo, hi = order[first], order[last]
        if lo > hi:
            continue
        span = set(all_names[lo:hi + 1])
        if span & grown:
            grown |= span
    return grown


def build_texture1(pnames, textures, keep):
    """Rewrite TEXTURE1 and PNAMES over the kept textures only."""
    kept = [t for t in textures if t[0] in keep]
    used, remap = [], {}
    for _, patches, _, _ in kept:
        for pi, _, _ in patches:
            if pi not in remap:
                remap[pi] = len(used)
                used.append(pnames[pi] if pi < len(pnames) else "")
    body, offsets, cursor = b"", [], 4 + 4 * len(kept)
    for name, patches, w, h in kept:
        entry = name.ljust(8, "\0").encode("latin1")
        entry += struct.pack("<ihhi", 0, w, h, 0)
        entry += struct.pack("<h", len(patches))
        for pi, px, py in patches:
            entry += struct.pack("<hhhhh", px, py, remap[pi], 1, 0)
        offsets.append(cursor)
        cursor += len(entry)
        body += entry
    tex1 = struct.pack("<I", len(kept)) + struct.pack(f"<{len(kept)}i", *offsets) + body
    pn = struct.pack("<I", len(used)) + b"".join(n.ljust(8, "\0").encode("latin1") for n in used)
    return tex1, pn, used, [t[0] for t in kept]


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("source")
    ap.add_argument("-m", "--map", default="E1M1",
                    help="which map to take from the source WAD")
    ap.add_argument("--as", dest="rename", default="E1M1",
                    help="what to call it in the output. Doom picks the game "
                         "mode from which map markers exist, and shareware "
                         "clamps every warp to episode 1, so a lone E2M8 is "
                         "unreachable until it is called E1M1. Map data does "
                         "not depend on the name; only the sky does, and that "
                         "is cosmetic.")
    ap.add_argument("-o", "--out", default="min.wad")
    ap.add_argument("--engine", default=ENGINE_DEFAULT,
                    help="doomgeneric source dir; animdefs[] and alphSwitchList[] "
                         "are read from it so this tool cannot drift from the engine")
    ap.add_argument("--episode", type=int, default=1,
                    help="P_InitSwitchList requires every switch pair with "
                         "episode <= this, whatever the map uses")
    ap.add_argument("--extra", default="",
                    help="comma-separated lump names to keep as well")
    ap.add_argument("--no-sprites", action="store_true",
                    help="drop the S_ namespace (Doom will not boot; for sizing only)")
    args = ap.parse_args()

    data, dirents = read_wad(args.source)
    ns = namespaces(dirents)
    byname = {}
    for name, pos, size in dirents:
        byname.setdefault(name, (pos, size))

    idx = [i for i, (n, _, _) in enumerate(dirents) if n == args.map]
    if not idx:
        sys.exit(f"{args.map}: not in {args.source}")
    mapstart = idx[0]

    animdefs = parse_animdefs(args.engine)
    switchtex = parse_switchlist(args.engine, args.episode)

    reftex, refflats = map_references(data, dirents, mapstart)
    pnames, textures = parse_textures(data, dirents, byname)
    texnames = [t[0] for t in textures]

    # G_DoLoadLevel picks the sky by episode and calls R_TextureNumForName on
    # it, so the sky a map does not reference is still required by the code.
    skytex = {"SKY1", "SKY2", "SKY3", "SKY4"}

    wanted = (reftex | switchtex | skytex) & set(texnames)
    keeptex = expand_animations(wanted, texnames, True, animdefs)
    missing = (reftex | switchtex) - set(texnames)   # skies are best-effort
    flatnames = [dirents[i][0] for i in sorted(ns) if ns[i] == "F"]
    keepflats = expand_animations(refflats & set(flatnames), flatnames, False, animdefs)

    tex1, pn, usedpatches, kepttexnames = build_texture1(pnames, textures, keeptex)

    extra = [e.strip().upper() for e in args.extra.split(",") if e.strip()]
    plain = ["PLAYPAL", "COLORMAP"] + extra

    out = []          # (name, bytes)
    for name in plain:
        if name not in byname:
            sys.exit(f"{name}: not in source WAD")
        pos, size = byname[name]
        out.append((name, data[pos:pos + size]))
    out.append((args.rename, b""))
    for name in MAP_LUMPS:
        for i in range(mapstart + 1, mapstart + 12):
            if i < len(dirents) and dirents[i][0] == name:
                pos, size = dirents[i][1], dirents[i][2]
                out.append((name, data[pos:pos + size]))
                break
    out.append(("TEXTURE1", tex1))
    out.append(("PNAMES", pn))

    wantpatch = set(usedpatches)
    out.append(("P_START", b""))
    seen = set()
    for i, (name, pos, size) in enumerate(dirents):
        if name in wantpatch and name not in seen and (ns.get(i) == "P" or name in wantpatch):
            seen.add(name)
            out.append((name, data[pos:pos + size]))
    out.append(("P_END", b""))
    missingpatch = wantpatch - seen

    out.append(("F_START", b""))
    seenf = set()
    for i, (name, pos, size) in enumerate(dirents):
        if ns.get(i) == "F" and name in keepflats and name not in seenf:
            seenf.add(name)
            out.append((name, data[pos:pos + size]))
    out.append(("F_END", b""))

    sprite_bytes = 0
    if not args.no_sprites:
        out.append(("S_START", b""))
        for i, (name, pos, size) in enumerate(dirents):
            if ns.get(i) == "S":
                out.append((name, data[pos:pos + size]))
                sprite_bytes += size
        out.append(("S_END", b""))

    body, directory, off = [], [], 12
    for name, blob in out:
        directory.append((off, len(blob), name))
        body.append(blob)
        off += len(blob)
    payload = b"".join(body)
    table = b"".join(struct.pack("<II8s", p, s, n.ljust(8, "\0").encode("latin1"))
                     for p, s, n in directory)
    wad = struct.pack("<4sII", b"IWAD", len(out), 12 + len(payload)) + payload + table
    open(args.out, "wb").write(wad)

    print(f"{args.out}: {len(out)} lumps, {len(wad):,} bytes "
          f"({len(wad)/1024/1024:.2f} MiB)")
    print(f"  textures {len(kepttexnames)} of {len(textures)} defined "
          f"({len(reftex)} by the map, "
          f"{len((switchtex | skytex) & set(texnames))} by P_InitSwitchList and the sky, "
          f"{len(keeptex) - len(wanted)} to complete animations)")
    print(f"  patches  {len(seen)} of {len(pnames)} in PNAMES")
    print(f"  flats    {len(seenf)} of {len(flatnames)}")
    print(f"  sprites  {sprite_bytes:,} bytes")
    if missing:
        print(f"  WARNING: map names {len(missing)} textures the WAD does not define: "
              f"{sorted(missing)[:8]}")
    if missingpatch:
        print(f"  WARNING: {len(missingpatch)} patches not found: {sorted(missingpatch)[:8]}")


if __name__ == "__main__":
    main()
