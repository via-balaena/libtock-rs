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
import os
import re
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
    """alphSwitchList[] out of p_switch.c, as a list of (on, off) pairs.

    P_InitSwitchList used to call R_TextureNumForName -- which I_Errors -- on
    both halves of every pair for the episode, whether or not the map used
    them, so a one-map WAD had to carry all 38. It now skips a pair it cannot
    find, so only the pairs the map actually references are kept -- but BOTH
    halves of those, because a switch that cannot toggle is worse than one
    that is not there.
    """
    src = open(os.path.join(os.path.expanduser(engine), "p_switch.c")).read()
    body = re.search(r"switchlist_t\s+alphSwitchList\[\]\s*=\s*\{(.*?)\n\};", src, re.S)
    if not body:
        sys.exit("could not find alphSwitchList[] in p_switch.c")
    pairs = []
    for line in body.group(1).splitlines():
        m = re.match(r'\s*\{\s*"([^"]*)"\s*,\s*"([^"]*)"\s*,\s*(\d+)', line)
        # episode 0 terminates the table -- P_InitSwitchList stops there.
        if m and 1 <= int(m.group(3)) <= episode:
            pairs.append((m.group(1).upper(), m.group(2).upper()))
    if not pairs:
        sys.exit("alphSwitchList[] parsed to nothing")
    return pairs


def parse_enum(src, typename):
    """A C enum's members in order, as name -> index.

    The body is matched as `[^{}]*`, NOT `.*?`. An enum body contains no
    braces, so this cannot span two of them -- whereas `.*?` anchored at the
    first `typedef enum {` in the file happily swallowed spritenum_t AND
    statenum_t as one enum, yielding 1106 names for a 967-entry table with
    every index shifted. Everything downstream still looked plausible: the
    sprite keep-set kept the right NUMBER of sprites and the wrong ones, and
    blanked the player's pistol.
    """
    m = re.search(r"typedef\s+enum\s*\{([^{}]*)\}\s*" + typename + r"\s*;", src, re.S)
    if not m:
        sys.exit(f"could not find enum {typename}")
    names = []
    for raw in m.group(1).split(","):
        raw = re.sub(r"/\*.*?\*/", "", raw, flags=re.S)
        raw = re.sub(r"//.*", "", raw)
        name = raw.strip()
        if name:
            names.append(name.split("=")[0].strip())
    return {n: i for i, n in enumerate(names)}


def parse_actor_tables(engine):
    """Doom's state machine, out of info.c and info.h.

    Returns (state_sprite, state_next, mobjs) where the first two are indexed
    by state number and `mobjs` is a list of dicts with `doomednum` and every
    state field, in mobjtype order.

    A sprite name needs no lookup: the `spritenum_t` members are SPR_ plus the
    four-character lump prefix, and sprnames[] repeats the same strings. That
    correspondence is checked below rather than assumed.
    """
    base = os.path.expanduser(engine)
    info_h = open(os.path.join(base, "info.h")).read()
    info_c = open(os.path.join(base, "info.c")).read()

    statenum = parse_enum(info_h, "statenum_t")

    # Check the SPR_ convention against sprnames[] before relying on it.
    spr = parse_enum(info_h, "spritenum_t")
    m = re.search(r"sprnames\[\]\s*=\s*\{(.*?)\};", info_c, re.S)
    if not m:
        sys.exit("could not find sprnames[]")
    sprnames = re.findall(r'"(\w+)"', m.group(1))
    for name, i in spr.items():
        if name == "NUMSPRITES":
            continue
        if i < len(sprnames) and name[4:] != sprnames[i]:
            sys.exit(f"SPR_ convention broken: {name} is not {sprnames[i]!r}")

    # states[]: {SPR_XXX, frame, tics, {action}, S_NEXT, misc1, misc2},
    m = re.search(r"states\[NUMSTATES\]\s*=\s*\{(.*?)\n\};", info_c, re.S)
    if not m:
        sys.exit("could not find states[]")
    entries = re.findall(
        r"\{\s*(SPR_\w+)\s*,[^,]*,[^,]*,\s*\{[^}]*\}\s*,\s*(S_\w+)", m.group(1))
    if len(entries) < 900:
        sys.exit(f"states[] parsed to only {len(entries)} entries")
    state_sprite = [e[0][4:] for e in entries]
    state_next = [statenum.get(e[1], 0) for e in entries]

    # The enum and the table must agree, or every index is off and the result
    # is a keep-set that looks reasonable and is wrong. NUMSTATES is the
    # trailing count member, hence the +1.
    if len(statenum) != len(entries) + 1:
        sys.exit(f"statenum_t has {len(statenum)} members but states[] has "
                 f"{len(entries)} entries -- the enum parse is wrong")
    if statenum.get("S_NULL") != 0:
        sys.exit("S_NULL is not state 0 -- the enum parse is wrong")

    # mobjinfo[]: one brace-delimited block per type, fields in struct order.
    m = re.search(r"mobjinfo\[NUMMOBJTYPES\]\s*=\s*\{(.*)\n\};", info_c, re.S)
    if not m:
        sys.exit("could not find mobjinfo[]")
    STATE_FIELDS = ("spawnstate", "seestate", "painstate", "meleestate",
                    "missilestate", "deathstate", "xdeathstate", "raisestate")
    ORDER = ["doomednum", "spawnstate", "spawnhealth", "seestate", "seesound",
             "reactiontime", "attacksound", "painstate", "painchance",
             "painsound", "meleestate", "missilestate", "deathstate",
             "xdeathstate", "deathsound", "speed", "radius", "height", "mass",
             "damage", "activesound", "flags", "raisestate"]
    mobjs = []
    for block in re.findall(r"\{\s*//\s*MT_\w+(.*?)\n\s*\}", m.group(1), re.S):
        vals = []
        for line in block.splitlines():
            line = re.sub(r"//.*", "", line).strip().rstrip(",").strip()
            if line:
                vals.append(line)
        if len(vals) < len(ORDER):
            continue
        rec = dict(zip(ORDER, vals))
        entry = {"doomednum": None, "states": []}
        try:
            entry["doomednum"] = int(rec["doomednum"])
        except ValueError:
            pass
        for f in STATE_FIELDS:
            v = rec.get(f, "S_NULL")
            if v in statenum:
                entry["states"].append(statenum[v])
        mobjs.append(entry)
    if len(mobjs) < 100:
        sys.exit(f"mobjinfo[] parsed to only {len(mobjs)} entries")
    mobjtype = parse_enum(info_h, "mobjtype_t")
    if len(mobjtype) != len(mobjs) + 1:
        sys.exit(f"mobjtype_t has {len(mobjtype)} members but mobjinfo[] has "
                 f"{len(mobjs)} entries -- the enum parse is wrong")
    return state_sprite, state_next, mobjs


def parse_weapon_states(engine, statenum):
    """weaponinfo[] out of d_items.c: the player's psprite states."""
    src = open(os.path.join(os.path.expanduser(engine), "d_items.c")).read()
    m = re.search(r"weaponinfo\[NUMWEAPONS\]\s*=\s*\{(.*)\n\};", src, re.S)
    if not m:
        sys.exit("could not find weaponinfo[]")
    states = [statenum[n] for n in re.findall(r"\b(S_\w+)\b", m.group(1))
              if n in statenum]
    if not states:
        sys.exit("weaponinfo[] parsed to no states")
    return states


def dummy_patch():
    """A 1x1 patch with one empty column: valid, and draws nothing.

    This is what makes sprite trimming safe. R_InitSpriteDefs walks all 138
    sprnames and I_Errors on any with no lumps, and the renderer I_Errors on a
    frame it cannot find -- mid-game, not at boot. Replacing the PIXELS while
    keeping every lump NAME leaves both tables exactly the shape they were, so
    a sprite that was trimmed by mistake is invisible rather than fatal.
    """
    return struct.pack("<hhhh", 1, 1, 0, 0) + struct.pack("<I", 12) + b"\xff"


def reachable_sprites(starts, state_sprite, state_next):
    """Every sprite on the state graph reachable from `starts`."""
    seen, stack, out = set(), list(starts), set()
    while stack:
        i = stack.pop()
        if i in seen or i < 0 or i >= len(state_sprite):
            continue
        seen.add(i)
        out.add(state_sprite[i])
        stack.append(state_next[i])
    return out


def map_doomednums(data, dirents, start):
    """Every thing type the map places, from its THINGS lump."""
    for i in range(start + 1, min(start + 12, len(dirents))):
        name, pos, size = dirents[i]
        if name == "THINGS":
            return {struct.unpack("<h", data[pos + k * 10 + 6:pos + k * 10 + 8])[0]
                    for k in range(size // 10)}
        if name not in MAP_LUMPS:
            break
    return set()


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
    ap.add_argument("--sky", type=int, default=1,
                    help="keep only this episode's sky texture (default 1); "
                         "0 keeps all four")
    ap.add_argument("--stub", default="D_",
                    help="comma-separated lump name PREFIXES to keep by name "
                         "but empty. Doom looks these up and would I_Error if "
                         "they vanished, but nothing reads the bytes: the "
                         "default is music, on a build with no sound driver")
    ap.add_argument("--all-sprites", action="store_true",
                    help="keep every sprite's pixels. Without this, sprites "
                         "the map cannot show are replaced by a 1x1 blank, "
                         "keeping every lump NAME so R_InitSpriteDefs and the "
                         "renderer see the tables they expect")
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
    switchpairs = parse_switchlist(args.engine, args.episode)

    reftex, refflats = map_references(data, dirents, mapstart)
    pnames, textures = parse_textures(data, dirents, byname)
    texnames = [t[0] for t in textures]

    # G_DoLoadLevel picks the sky by episode and calls R_TextureNumForName on
    # it, so the sky a map does not reference is still required by the code --
    # but only ONE of them is, the one for the episode that will be played.
    # Each sky's patch is 35,080 bytes, so carrying all four costs 105,240 for
    # three that can never be drawn. With the map renamed to E1M1 the game
    # mode is shareware, which clamps every warp to episode 1.
    skytex = {f"SKY{args.sky}"} if args.sky else {"SKY1", "SKY2", "SKY3", "SKY4"}

    # Keep both halves of a switch pair the map uses, and neither half of one
    # it does not: P_InitSwitchList now skips what it cannot find.
    switchtex = set()
    for on, off in switchpairs:
        if on in reftex or off in reftex:
            switchtex.update((on, off))

    wanted = (reftex | switchtex | skytex) & set(texnames)
    keeptex = expand_animations(wanted, texnames, True, animdefs)
    missing = (reftex | switchtex) - set(texnames)   # skies are best-effort
    flatnames = [dirents[i][0] for i in sorted(ns) if ns[i] == "F"]
    keepflats = expand_animations(refflats & set(flatnames), flatnames, False, animdefs)

    tex1, pn, usedpatches, kepttexnames = build_texture1(pnames, textures, keeptex)

    extra = [e.strip().upper() for e in args.extra.split(",") if e.strip()]
    plain = ["PLAYPAL", "COLORMAP"] + extra

    stubs = tuple(p.strip().upper() for p in args.stub.split(",") if p.strip())
    stubbed = stub_bytes = 0

    out = []          # (name, bytes)
    for name in plain:
        if name not in byname:
            sys.exit(f"{name}: not in source WAD")
        pos, size = byname[name]
        if stubs and name.startswith(stubs):
            out.append((name, b"\0\0\0\0"))
            stubbed += 1
            stub_bytes += size - 4
        else:
            out.append((name, data[pos:pos + size]))
    # Every lump matching a --stub prefix, kept by name and empty. Doom looks
    # music up per level and at the title screen, and W_GetNumForName I_Errors
    # on a name it cannot find -- so the names all have to be here, even on a
    # build with no sound driver that will never read a byte of them.
    if stubs:
        for name, pos, size in dirents:
            if name.startswith(stubs) and name not in {n for n, _ in out}:
                out.append((name, b"\0\0\0\0"))
                stubbed += 1
                stub_bytes += max(0, size - 4)

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

    sprite_bytes = dummied = kept_sprites = 0
    if not args.no_sprites:
        keep_spr = None
        if not args.all_sprites:
            state_sprite, state_next, mobjs = parse_actor_tables(args.engine)
            statenum = parse_enum(
                open(os.path.join(os.path.expanduser(args.engine), "info.h")).read(),
                "statenum_t")
            placed = map_doomednums(data, dirents, mapstart)
            starts = list(parse_weapon_states(args.engine, statenum))
            for mt in mobjs:
                # Types the map places, and every type the code can spawn on
                # its own -- which is exactly those with no doomednum.
                if mt["doomednum"] == -1 or mt["doomednum"] in placed:
                    starts.extend(mt["states"])
            keep_spr = reachable_sprites(starts, state_sprite, state_next)

        blank = dummy_patch()
        out.append(("S_START", b""))
        seen_blank = set()
        for i, (name, pos, size) in enumerate(dirents):
            if ns.get(i) != "S":
                continue
            if keep_spr is None or name[:4] in keep_spr:
                out.append((name, data[pos:pos + size]))
                sprite_bytes += size
                kept_sprites += 1
            else:
                # ONE BLANK PER FRAME LETTER, not one per sprite.
                #
                # Collapsing a blanked sprite to a single A0 lump saves
                # directory entries and is UNSOUND: it leaves maxframe at 0, so
                # any state that reaches frame B or later dies in
                # R_ProjectSprite -- mid-game, not at boot. It did exactly
                # that. Keeping one rotation-0 lump per frame letter the source
                # had preserves maxframe and every frame index, which is what
                # makes a mistrimmed sprite invisible instead of fatal, while
                # still dropping every rotation.
                for letter in (name[4:5], name[6:7]):
                    if not letter:
                        continue
                    key = name[:4] + letter
                    if key in seen_blank:
                        continue
                    seen_blank.add(key)
                    out.append((key + "0", blank))
                    sprite_bytes += len(blank)
                    dummied += 1
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
    if stubbed:
        print(f"  stubbed  {stubbed} lump(s) kept by name only, {stub_bytes:,} bytes dropped")
    print(f"  sprites  {sprite_bytes:,} bytes, {kept_sprites} lumps kept, "
          f"{dummied} blanked")
    if missing:
        print(f"  WARNING: map names {len(missing)} textures the WAD does not define: "
              f"{sorted(missing)[:8]}")
    if missingpatch:
        print(f"  WARNING: {len(missingpatch)} patches not found: {sorted(missingpatch)[:8]}")


if __name__ == "__main__":
    main()
