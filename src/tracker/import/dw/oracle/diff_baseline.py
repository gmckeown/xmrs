#!/usr/bin/env python3
"""DW Tier-2 fidelity diff: player trace vs Ghidra Paula golden.

Aligns two CSVs (same schema: tick,dma,c0g,c0per,c0vol,...,c3g,...,f0)
tick-by-tick and reports, per channel:

* gate agreement  — fraction of ticks where player gate == golden gate
* on ticks where BOTH are gated (the only ones that affect audio — an
  ungated Paula channel's AUDxPER/AUDxVOL are stale scratch):
    - period: exact %, median signed Δ (offset), mean |Δ|, max |Δ|
    - volume: exact %, mean |Δ|, max |Δ|

Usage:
  diff_baseline.py <golden.csv> <player.csv>            # one pair, text
  diff_baseline.py --markdown                           # all clusters → BASELINE rows
"""
import sys
import os
import statistics

HERE = os.path.dirname(os.path.abspath(__file__))

# The 12 MB Paula-oracle golden store lives OUTSIDE the crate (it is a
# regenerable RE artifact, not library/test data). Default location, with
# a DW_ORACLE_CORPUS env override.
CORPUS = os.environ.get(
    "DW_ORACLE_CORPUS",
    os.path.expanduser("~/Documents/dw-oracle-corpus"),
)

# Reference clusters for the --markdown scoreboard. Goldens live in the
# single corpus store (keyed by the real .dw stem); player traces in
# /tmp/ptr/<stem>.csv, same as audit.py. (corpus stem, human label)
CLUSTERS = [
    ("xenon2 (title)", "C1 · xenon2 (title)"),
    ("bad company", "C2 · bad company"),
    ("bubble bobble", "C3 · bubble bobble"),
    ("grimblood", "C4 · grimblood (3-voice)"),
    ("tetris", "C5 · tetris"),
    ("qball", "C6 · qball (old)"),
    ("leviathan-deabs", "C7 · leviathan-deabs (old)"),
    ("the empire strikes back", "C8 · the empire strikes back (old)"),
    ("obliterator-title", "½-vol · obliterator-title"),
]


def load(path):
    """Parse a trace/golden CSV. Auto-detects schema width per row:
    new = tick,dma,4×(g,per,vol,s),f0  (19 cols); old = …4×(g,per,vol),f0
    (15 cols). Missing `s` (struck) defaults to 0, so old goldens still
    load (their strike stats are simply all-zero)."""
    rows = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#") or line.startswith("tick,"):
                continue
            parts = line.split(",")
            tick = int(parts[0])
            stride = 4 if len(parts) >= 2 + 4 * 4 + 1 else 3
            ch = []
            for i in range(4):
                base = 2 + i * stride
                g = int(parts[base])
                per = int(parts[base + 1])
                vol = int(parts[base + 2])
                s = int(parts[base + 3]) if stride == 4 else 0
                ch.append((g, per, vol, s))
            rows.append((tick, ch))
    return {t: ch for t, ch in rows}


def strike_match(g_ticks, p_ticks, window=12):
    """Greedy nearest-neighbour match of oracle vs player strike ticks
    within ±`window` frames (absorbs the delay-counter skew). Returns
    (matched, missed, spurious): `missed` = oracle re-attacks the player
    skipped (tie rendered as nothing / dropped note); `spurious` = player
    re-attacks with no oracle counterpart (the xenon2 0x83 bug class)."""
    used = set()
    matched = 0
    for gt in g_ticks:
        best = None
        for j, pt in enumerate(p_ticks):
            if j in used or abs(pt - gt) > window:
                continue
            if best is None or abs(pt - gt) < abs(p_ticks[best] - gt):
                best = j
        if best is not None:
            used.add(best)
            matched += 1
    return matched, len(g_ticks) - matched, len(p_ticks) - matched


def oracle_last_active(g, window=12):
    """Last tick at which the GOLDEN still drives audio (any channel
    gated). Past this the real replayer has stopped (StopSong / DMA-kill)
    and sits silent; a looping player keeps emitting there, so comparing
    that tail is meaningless (it manufactures gate-disagreement and
    spurious strikes out of nothing). Returns the clip point + a window
    margin so trailing strike matches still resolve. If the oracle never
    goes silent (it loops too), this is just the end of the capture and
    clipping is a no-op."""
    last = -1
    for t, ch in g.items():
        if any(c[0] == 1 for c in ch):
            if t > last:
                last = t
    return last + window if last >= 0 else None


def compare(golden, player, clip=True):
    g = load(golden)
    p = load(player)
    ticks = sorted(set(g) & set(p))
    if clip:
        cut = oracle_last_active(g)
        if cut is not None:
            ticks = [t for t in ticks if t <= cut]
    per_ch = []
    for c in range(4):
        gate_agree = 0
        both = 0
        dper = []
        dvol = []
        per_exact = 0
        vol_exact = 0
        g_strikes = []
        p_strikes = []
        for t in ticks:
            gg, gper, gvol, gs = g[t][c]
            pg, pper, pvol, ps = p[t][c]
            if gg == pg:
                gate_agree += 1
            if gs:
                g_strikes.append(t)
            if ps:
                p_strikes.append(t)
            if gg == 1 and pg == 1:
                both += 1
                d = pper - gper
                dper.append(d)
                if d == 0:
                    per_exact += 1
                dv = pvol - gvol
                dvol.append(dv)
                if dv == 0:
                    vol_exact += 1
        _, missed, spurious = strike_match(g_strikes, p_strikes)
        per_ch.append({
            "n": len(ticks),
            "gate_agree": gate_agree,
            "both": both,
            "per_exact": per_exact,
            "vol_exact": vol_exact,
            "per_med": statistics.median(dper) if dper else None,
            "per_mae": statistics.mean(abs(x) for x in dper) if dper else None,
            "per_max": max(abs(x) for x in dper) if dper else None,
            "vol_mae": statistics.mean(abs(x) for x in dvol) if dvol else None,
            "vol_max": max(abs(x) for x in dvol) if dvol else None,
            "strk_g": len(g_strikes),
            "strk_p": len(p_strikes),
            "strk_missed": missed,
            "strk_spurious": spurious,
        })
    return ticks, per_ch


def fmt_pct(num, den):
    return f"{100*num/den:.0f}%" if den else "—"


def text_report(golden, player):
    ticks, per_ch = compare(golden, player)
    print(f"# {os.path.basename(golden)} vs {os.path.basename(player)}  ({len(ticks)} aligned ticks)")
    for c, s in enumerate(per_ch):
        strikes = (f"strikes oracle={s['strk_g']} player={s['strk_p']}"
                   f"  spurious={s['strk_spurious']} missed={s['strk_missed']}")
        flag = "  ⚠" if (s["strk_spurious"] or s["strk_missed"]) else ""
        if s["both"] == 0:
            print(f"  ch{c}: gate-agree {fmt_pct(s['gate_agree'], s['n'])}  (never both-gated)  {strikes}{flag}")
            continue
        print(f"  ch{c}: gate-agree {fmt_pct(s['gate_agree'], s['n'])}  both-gated={s['both']}  {strikes}{flag}")
        print(f"       period: exact {fmt_pct(s['per_exact'], s['both'])}  median Δ={s['per_med']:+g}  MAE={s['per_mae']:.1f}  max|Δ|={s['per_max']}")
        print(f"       volume: exact {fmt_pct(s['vol_exact'], s['both'])}  MAE={s['vol_mae']:.1f}  max|Δ|={s['vol_max']}")


def markdown_rows():
    print("| Cluster | ch | gate✓ | both-gated | spur | miss | per exact | per medΔ | per MAE | per max | vol exact | vol MAE | vol max |")
    print("|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|")
    for stem, label in CLUSTERS:
        golden = os.path.join(CORPUS, f"{stem}.csv")
        player = f"/tmp/ptr/{stem}.csv"
        if not (os.path.exists(golden) and os.path.exists(player)):
            print(f"| {label} | — | (missing trace) | | | | | | | | |")
            continue
        _, per_ch = compare(golden, player)
        first = True
        for c, s in enumerate(per_ch):
            if s["both"] == 0 and s["gate_agree"] == s["n"]:
                continue  # channel never used by either side — skip noise
            name = label if first else ""
            first = False
            spur = s["strk_spurious"]
            miss = s["strk_missed"]
            if s["both"] == 0:
                print(f"| {name} | {c} | {fmt_pct(s['gate_agree'], s['n'])} | 0 | {spur} | {miss} | — | — | — | — | — | — | — |")
            else:
                print(f"| {name} | {c} | {fmt_pct(s['gate_agree'], s['n'])} | {s['both']} | {spur} | {miss} | "
                      f"{fmt_pct(s['per_exact'], s['both'])} | {s['per_med']:+g} | {s['per_mae']:.1f} | {s['per_max']} | "
                      f"{fmt_pct(s['vol_exact'], s['both'])} | {s['vol_mae']:.1f} | {s['vol_max']} |")


if __name__ == "__main__":
    if len(sys.argv) == 2 and sys.argv[1] == "--markdown":
        markdown_rows()
    elif len(sys.argv) == 3:
        text_report(sys.argv[1], sys.argv[2])
    else:
        print(__doc__)
        sys.exit(1)
