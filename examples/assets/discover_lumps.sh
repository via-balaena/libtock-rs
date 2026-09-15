#!/bin/bash
# Find the lumps a trimmed WAD still needs, by asking the engine.
#
#   ./discover_lumps.sh <source.wad> <headless-doom> <workdir> [MAP] [EPISODE]
#
# make_min_wad.py keeps what a map references. It cannot know what the CODE
# requires -- the status bar font, the face graphics, the intermission
# numbers -- because nothing in the WAD says so. Rather than encode Doom's
# requirements here and watch them rot, build, run, read the name out of
# "W_GetNumForName: X not found!", add it, and go again. The loop stops when
# a round survives, and the answer is minimal by construction.
#
# When the name ends in digits (STCFN033, DEMO1) the whole family sharing the
# prefix goes in at once: the caller is nearly always a loop over a numbered
# range, and asking one at a time costs a round per member.
set -u
SRC=${1:?source wad}
BIN=${2:?headless doom binary}
WORK=${3:?work dir}
MAP=${4:-E1M1}
EPISODE=${5:-1}
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$WORK" || exit 1
OUT="$WORK/min.wad"
# SEED lets a run continue from one that ran out of rounds, rather than
# rediscovering everything it already knew.
EXTRA="${SEED:-}"
for round in $(seq 1 "${ROUNDS:-40}"); do
  python3 "$HERE/make_min_wad.py" "$SRC" -m "$MAP" --as E1M1 --episode "$EPISODE" \
      -o "$OUT" --extra "$EXTRA" > "$WORK/build.log" 2>&1 \
      || { echo "BUILD FAILED round $round"; cat "$WORK/build.log"; exit 1; }
  # DG_EXIT ends the level part way through, so the loop also walks the
  # end-of-level path: the intermission wants graphics no amount of playing a
  # level ever asks for, and a WAD discovered only by playing is missing every
  # one of them. That is how "stuck at the door" happened -- the lever that
  # ends the level called W_GetNumForName on WILV00 and Doom I_Errored.
  DG_EXIT=${DG_EXIT:-200} DG_TICKS=600 "$BIN" -iwad "$OUT" -warp 1 1 \
      > "$WORK/out.log" 2> "$WORK/err.log"
  if command grep -q ZONEPEAK "$WORK/out.log"; then
    echo "round $round: RUNS"
    printf '%s\n' "$EXTRA" > "$WORK/extra.txt"
    command grep -E "ZONEPEAK|TAG " "$WORK/out.log"
    exit 0
  fi
  name=$(sed -n 's/.*W_GetNumForName: \([^ ]*\) not found.*/\1/p' "$WORK/err.log" | head -1)
  if [ -z "$name" ]; then
    echo "round $round: stopped on something that is not a missing lump:"
    cat "$WORK/err.log"
    printf '%s\n' "$EXTRA" > "$WORK/extra.txt"
    exit 1
  fi
  fam=$(SRCWAD="$SRC" python3 - "$name" <<'PY'
import os, re, struct, sys
name = sys.argv[1]
d = open(os.environ['SRCWAD'], 'rb').read()
_, n, ofs = struct.unpack('<4sII', d[:12])
names = [d[ofs+i*16+8:ofs+i*16+16].rstrip(b'\0').decode('latin1').upper() for i in range(n)]
m = re.match(r'^([A-Z_]+?)\d+$', name)
if m:
    pre = re.escape(m.group(1))
    fam = sorted({x for x in names if re.match('^' + pre + r'\d+$', x)})
    print(','.join(fam) if fam else name)
else:
    print(name)
PY
)
  echo "round $round: needs $name -> adding $(printf '%s' "$fam" | tr ',' '\n' | command grep -c .) lump(s)"
  EXTRA="${EXTRA:+$EXTRA,}$fam"
done
echo "did not converge in ${ROUNDS:-40} rounds; re-run with SEED=\$(cat $WORK/extra.txt)"
printf '%s\n' "$EXTRA" > "$WORK/extra.txt"
exit 1
