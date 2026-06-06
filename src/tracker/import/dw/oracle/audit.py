#!/usr/bin/env python3
"""Corpus-wide strike/fidelity audit: every oracle/corpus/<stem>.csv vs
its player trace /tmp/ptr/<stem>.csv, ranked.

Build the inputs first:
    cargo run --release --features=demo --example dw_oracle_args > /tmp/dw_args.tsv
    bash oracle/capture_all.sh                       # → oracle/corpus/*.csv
    # player traces:
    for g in oracle/corpus/*.csv; do s=$(basename "$g" .csv); \
      target/release/examples/dw_oracle_trace "$HOME/Music/dw/$s.dw" 2000 "/tmp/ptr/$s.csv"; done

Then:  python3 oracle/audit.py

Reports, per module: summed spurious / missed strikes, min gate-agree,
max period-MAE. Highlights the PURE re-strike bugs — modules whose notes
already match the replayer (gate ≥85 %, perMAE ≤15) so the only error is
extra/missing re-attacks (the class the `struck` signal exists to catch).
"""
import os, sys, glob
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import diff_baseline as db

PTR = "/tmp/ptr"
rows = []
for g in sorted(glob.glob(os.path.join(HERE, "corpus", "*.csv"))):
    stem = os.path.basename(g)[:-4]
    p = os.path.join(PTR, f"{stem}.csv")
    if not os.path.exists(p):
        continue
    try:
        ticks, per_ch = db.compare(g, p)
    except Exception:
        continue
    spur = sum(s["strk_spurious"] for s in per_ch)
    miss = sum(s["strk_missed"] for s in per_ch)
    gates = [100 * s["gate_agree"] / s["n"] for s in per_ch if s["n"]]
    gmin = min(gates) if gates else 0
    pmae = max([s["per_mae"] for s in per_ch if s["per_mae"] is not None] or [0])
    rows.append((stem, dict(spur=spur, miss=miss, gmin=gmin, pmae=pmae)))

strk = [r for r in rows if r[1]["spur"] or r[1]["miss"]]
strk.sort(key=lambda r: (-(r[1]["spur"] + r[1]["miss"]), r[1]["gmin"]))
clean = len(rows) - len(strk)
pure = [r for r in strk if r[1]["gmin"] >= 85 and r[1]["pmae"] <= 15]

print(f"TOTAL {len(rows)} modules | strike-clean {clean} | strike-issues {len(strk)}")
print(f"\n=== PURE re-strike bugs ({len(pure)}: gate>=85%, perMAE<=15) ===")
print(f"  {'module':30} {'spur':>5} {'miss':>5} {'gate%':>6} {'perMAE':>7}")
for stem, d in pure:
    print(f"  {stem[:30]:30} {d['spur']:>5} {d['miss']:>5} {d['gmin']:>6.0f} {d['pmae']:>7.1f}")
over = sum(1 for _, d in pure if d["spur"] > d["miss"])
print(f"  over-trigger {over}, under-trigger {len(pure)-over}")
print(f"\n=== other strike-issue modules (desync / period-space) ===")
for stem, d in strk:
    if (stem, d) in pure:
        continue
    print(f"  {stem[:30]:30} spur={d['spur']:<4} miss={d['miss']:<4} gate={d['gmin']:.0f}% perMAE={d['pmae']:.0f}")
