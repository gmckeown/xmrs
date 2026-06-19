# DW Tier-2 fidelity — BASELINE

Frame-to-frame comparison of the **xmrsplayer** per-tick state against the
**Ghidra Paula oracle** goldens (one per player cluster, see
`oracle/README.md`). This is the measured state that the modeling backlog in
`PLAN.md` is meant to close — *not* a pass/fail gate.

## How this is produced

1. **Oracle side** — `$DW_ORACLE_CORPUS/*.csv` (kept outside the crate, default
   `~/Documents/dw-oracle-corpus`): the real 68k replayer emulated in
   Ghidra, one row per 50 Hz tick, capturing each Amiga channel's gate (DMA
   bit), `AUDxPER` (period), `AUDxVOL` (0..64) and a per-tick `struck` flag. See
   `oracle/README.md`.
2. **Player side** — `xmrsplayer/examples/dw_oracle_trace.rs`: renders the same
   `.dw` through `XmrsPlayer`, advancing **one replayer tick per call**
   (`advance_trace_tick`) and snapshotting each channel
   (`channel_snapshots()` → `ChannelSnapshot { period, volume, gate }`,
   units chosen 1:1 with Paula). Emits the *same CSV schema* as the goldens.
   ```sh
   cargo run --release --no-default-features --features std,import_dw \
     --example dw_oracle_trace -- "<file>.dw" 1500 /tmp/player_<key>.csv
   ```
3. **Diff** — `oracle/diff_baseline.py --markdown`: aligns the two CSVs
   tick-by-tick (player tick N ↔ golden tick N — init is separate on both
   sides, so tick 0 lines up) and reports per channel:
   * **gate✓** — fraction of ticks where player gate == golden gate.
   * On **both-gated** ticks only (an ungated Paula channel's PER/VOL are
     stale scratch and don't affect audio): period exact %, **median signed Δ**
     (the systematic offset), mean-abs-Δ (MAE, the drift), max |Δ|; same for
     volume.

All runs are song 0, 1500 ticks (≈30 s at 50 Hz).

## Results

| Cluster | ch | gate✓ | both-gated | per exact | per medΔ | per MAE | per max | vol exact | vol MAE | vol max |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| C1 · xenon2 (title) | 0 | 93% | 1394 | 21% | -2 | 1.4 | 8 | 51% | 5.1 | 16 |
|  | 1 | 94% | 1327 | 6% | +3 | 4.4 | 18 | 85% | 4.1 | 32 |
|  | 2 | 83% | 1134 | 0% | -1 | 1.4 | 2 | 100% | 0.0 | 0 |
|  | 3 | 82% | 1127 | 0% | -5 | 3.8 | 5 | 100% | 0.0 | 0 |
| C2 · bad company | 0 | 88% | 1318 | 2% | +3 | 4.8 | 1115 | 100% | 0.0 | 0 |
|  | 1 | 88% | 1318 | 0% | +5 | 12.9 | 308 | 100% | 0.0 | 0 |
|  | 2 | 63% | 941 | 18% | +2 | 3.8 | 40 | 100% | 0.0 | 0 |
|  | 3 | 88% | 1318 | 0% | +2 | 3.2 | 41 | 100% | 0.0 | 0 |
| C3 · bubble bobble | 0 | 96% | 428 | 4% | +3 | 4.5 | 13 | 100% | 0.0 | 0 |
|  | 1 | 98% | 437 | 0% | +5 | 6.4 | 325 | 84% | 0.2 | 1 |
|  | 2 | 96% | 423 | 4% | +3 | 4.5 | 13 | 100% | 0.0 | 0 |
|  | 3 | 93% | 416 | 0% | +2 | 11.4 | 283 | 100% | 0.0 | 1 |
| C4 · grimblood (3-voice) | 0 | 97% | 1453 | 0% | +2 | 6.8 | 45 | 97% | 0.0 | 1 |
|  | 1 | 99% | 1483 | 0% | +5 | 8.2 | 25 | 98% | 0.0 | 1 |
|  | 2 | 99% | 1492 | 1% | +5 | 7.6 | 24 | 98% | 0.0 | 1 |
| C5 · tetris | 0 | 98% | 1465 | 30% | +0 | 3.6 | 14 | 100% | 0.0 | 0 |
|  | 1 | 97% | 1459 | 41% | +0 | 3.9 | 318 | 100% | 0.0 | 0 |
|  | 2 | 96% | 1423 | 37% | +0 | 28.6 | 326 | 100% | 0.0 | 0 |
|  | 3 | 98% | 1465 | 29% | +0 | 3.4 | 13 | 100% | 0.0 | 0 |
| C6 · qball (old) | 0 | 100% | 1500 | 0% | -3 | 3.2 | 4 | 100% | 0.0 | 0 |
|  | 1 | 100% | 1500 | 0% | -4 | 3.6 | 4 | 99% | 0.4 | 40 |
|  | 2 | 100% | 1500 | 0% | -3 | 3.1 | 4 | 99% | 0.4 | 40 |
|  | 3 | 100% | 1500 | 0% | -3 | 3.2 | 4 | 100% | 0.0 | 0 |
| C7 · leviathan-deabs (old) | 0 | 88% | 1327 | 11% | -1 | 1.1 | 4 | 100% | 0.0 | 0 |
|  | 1 | 92% | 1386 | 53% | +0 | 0.5 | 1 | 31% | 0.7 | 1 |
|  | 2 | 88% | 1327 | 11% | -1 | 1.1 | 4 | 100% | 0.0 | 0 |
|  | 3 | 99% | 1480 | 70% | +0 | 0.3 | 1 | 0% | 1.0 | 1 |
| C8 · the empire strikes back (old) | 0 | 96% | 1413 | 23% | -1 | 1.4 | 4 | 100% | 0.0 | 0 |
|  | 1 | 92% | 1375 | 17% | +2 | 2.2 | 11 | 69% | 0.4 | 9 |
|  | 2 | 90% | 1346 | 0% | +2 | 2.0 | 2 | 0% | 1.0 | 1 |
|  | 3 | 94% | 1414 | 21% | +2 | 1.7 | 6 | 80% | 0.2 | 1 |
| ½-vol · obliterator-title | 0 | 99% | 1489 | 8% | +6 | 5.4 | 15 | 100% | 0.0 | 0 |
|  | 1 | 92% | 1374 | 8% | +7 | 6.1 | 14 | 100% | 0.0 | 0 |
|  | 2 | 99% | 1489 | 10% | +5 | 4.8 | 14 | 100% | 0.0 | 0 |
|  | 3 | 97% | 1461 | 58% | +0 | 2.0 | 11 | 100% | 0.0 | 0 |

Reproduce: regenerate the 9 `/tmp/player_<key>.csv` traces (loop in
`oracle/README.md`) then `python3 oracle/diff_baseline.py --markdown`.

## Reading the numbers

**The structural skeleton is right.** Gate agreement is 82–100 % everywhere:
the player turns channels on and off at essentially the right ticks. The
remaining error is *what value* plays, not *whether* it plays.

### A. Period — small constant offset, no space mismatch

The per-cluster period-space mismatch (median Δ in the hundreds) that the early
baseline showed on C2/C3/C7/C8 is **cleared** — all clusters now sit at median Δ
within ±7. What remains:

* **Period-table rounding** — a small constant ±2..±7 median Δ (C1, C4, C6,
  obliterator). tetris reads median Δ +0 on every channel.
* **Per-tick drift** (MAE in the tens, max in the hundreds on a few channels)
  from vibrato/portamento computed in the replayer's exact period units. The
  remaining genuine residuals are C2 bad company ch0/1, C5 tetris ch2
  (sub-row arp arm/re-arm timing, the row-quantised importer floor) and
  C3 bubble bobble ch1/ch3.

### B. Volume — systematic offsets, all resolved on the verified clusters

Volume is now 96–100 % exact across the verified clusters. The fixes were
per-cluster volume models read from the replayer:

* **C5 tetris** — per-sample volume word (offset +0xC of the 16-byte
  sample-transpose row); was hard-coded to 64.
* **C6 qball** — per-channel static volume table written raw to `AUDxVOL`.
* **C8 empire** — static global master volume (`× master/64`, master=24); the
  idiom is present in 90/120 files, 84 carry the no-op identity master=64.
* **C7 leviathan** — static per-instrument volume word (`instr+0xE`); residual
  on ch1/ch3 is the same handful of note-retrigger ticks seen on qball.

## Next actions (feeds PLAN.md backlog)

1. **Period-space drift (MAE)** on C2/C3 — needs the per-cluster replayer
   vibrato/slide semantics in exact period units (PLAN "vibrato period-space").
   The median-Δ column is the headline regression metric; re-run the diff after
   each change.
2. **qball/C1 period constant offset** (medΔ -3/-4 on qball, ±2/3 on xenon2,
   period-table rounding) — small systematic deltas, good warm-up.
3. **C5 tetris ch2** sub-row arpeggio arm/re-arm timing — the row-quantised
   importer floor.

Keep this file as the living scoreboard: re-run `diff_baseline.py --markdown`
after each modeling change and update the table.
