#!/usr/bin/env bash
# Headless Paula-oracle capture for the WHOLE ~/Music/dw corpus.
#
# For each .dw it tries candidate (init,play) entry points — 14 modules
# have exact overrides (oracle/known_args.tsv); the rest use heuristics
# from `dw_oracle_args` (Paula-`lea` body, the 0x0/0xe wrapper, DwLayout
# offsets) — runs the Ghidra emulator (`DwPaulaOracle.java`) headless,
# and keeps the FIRST candidate whose trace is sane. Failures are logged
# for manual entry-point RE.
#
# Prereqs:  cargo run --release --features=demo --example dw_oracle_args > /tmp/dw_args.tsv
# Usage:    capture_all.sh            # all modules
#           capture_all.sh NAME...    # only these file stems
#
# Output:   oracle/corpus/<stem>.csv  + capture_all.log

HL=/home/user/bin/ghidra_12.0.4_PUBLIC/support/analyzeHeadless
HERE=/home/user/Documents/xm-rs/xmrs/src/tracker/import/dw/oracle
OUT="$HERE/corpus"
ARGS_TSV=/tmp/dw_args.tsv
KNOWN_TSV="$HERE/known_args.tsv"   # stem<TAB>init<TAB>play<TAB>a3<TAB>f0
DWDIR="$HOME/Music/dw"
SCRIPTS="$HOME/ghidra_scripts"
LOG="$HERE/capture_all.log"
TICKS=2000
PROJ=/tmp/dw_hl_p
mkdir -p "$OUT" "$PROJ"
: > "$LOG"

# Sane-trace test (arg1=csv): >=100 rows, a live channel (period
# 100..2400, vol 1..64) in >=30% of rows OR >=80 rows absolute (short
# songs that stop and leave a long silent tail still pass — e.g. carrier
# command plays ~216 of 2000 ticks), and >=3 strikes total. exit 0 if sane.
sane() {
  awk -F, '
    NR>1 && $1 !~ /^#/ { n++; alive=0
      for (c=0;c<4;c++){ b=3+c*4; per=$(b+1); vol=$(b+2)
        if (per>=100 && per<=2400 && vol>=1 && vol<=64) alive=1
        strk += $(b+3) }
      if (alive) a++ }
    END { exit (n>=100 && (a>=0.3*n || a>=80) && strk>=3) ? 0 : 1 }' "$1"
}

# Build the module list.
if [ "$#" -gt 0 ]; then mods=("$@"); else
  mods=(); for f in "$DWDIR"/*.dw; do mods+=("$(basename "$f" .dw)"); done
fi

for stem in "${mods[@]}"; do
  src="$DWDIR/$stem.dw"
  if [ ! -f "$src" ]; then echo "MISS  $stem (no file)" >> "$LOG"; continue; fi

  # Ghidra's BinaryLoader rejects an import path containing a single
  # quote (`'`) — e.g. `chip's challenge.dw` — with "invalid character:
  # ''' and can not be imported". The replayer bytes are identical, so
  # stage such files under a sanitised temp name before -import. (Parens
  # like `xenon2 (title).dw` are fine; only the apostrophe trips it.)
  if [[ "$src" == *\'* ]]; then
    safe="/tmp/$(echo "$stem" | tr -d "'").dw"
    cp -f "$src" "$safe"
    src="$safe"
  fi

  # Candidate "init play a3 f0" lines, newline-separated.
  known=$(awk -F'\t' -v s="$stem" '$1==s{print $2,$3,$4,$5}' "$KNOWN_TSV")
  if [ -n "$known" ]; then
    cands="$known"
  else
    line=$(awk -F'\t' -v s="$stem" '$1==s{print $2"|"$3"|"$4"|"$5}' "$ARGS_TSV")
    a3=$(echo "$line" | cut -d'|' -f1); lea=$(echo "$line" | cut -d'|' -f2)
    dp=$(echo "$line" | cut -d'|' -f3); di=$(echo "$line" | cut -d'|' -f4)
    [ -z "$a3" ] && a3="-"; [ -z "$lea" ] && lea="-"; [ -z "$dp" ] && dp="-"; [ -z "$di" ] && di="-"
    cands="0x0 0xe $a3 -
0x0 $lea $a3 -
0x66 $lea $a3 -
$di $lea $a3 -
$di $dp $a3 -
0x0 $dp $a3 -"
  fi

  ok=0; i=0
  while IFS= read -r cand; do
    [ -z "$cand" ] && continue
    i=$((i+1))
    set -- $cand; init=$1; play=$2; A3=$3; F0=$4
    [ "$play" = "-" ] && continue
    rm -f /tmp/dw_cap.csv
    timeout 150 "$HL" "$PROJ" "cap_$i" -import "$src" \
      -processor "68000:BE:32:default" -loader BinaryLoader -scriptPath "$SCRIPTS" \
      -postScript DwPaulaOracle.java "$init" "$play" "$TICKS" /tmp/dw_cap.csv "$A3" "$F0" \
      -noanalysis -deleteProject > /dev/null 2>&1
    if [ -f /tmp/dw_cap.csv ] && sane /tmp/dw_cap.csv; then
      cp /tmp/dw_cap.csv "$OUT/$stem.csv"
      echo "OK    $stem  init=$init play=$play a3=$A3 f0=$F0  (cand $i)" >> "$LOG"
      ok=1; break
    fi
  done <<< "$cands"
  [ "$ok" = 0 ] && echo "FAIL  $stem  (no sane candidate)" >> "$LOG"
done

echo "=== done: $(grep -c '^OK' "$LOG") ok, $(grep -c '^FAIL' "$LOG") fail, $(grep -c '^MISS' "$LOG") miss ===" >> "$LOG"
