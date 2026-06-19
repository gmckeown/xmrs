# IT fidelity — how we measure "done"

Self-contained plan for deciding when Impulse Tracker (`.it`) support is
complete. Read with [`BASELINE.md`](BASELINE.md) (the current numbers).

"Done" is not a feeling — it is a measurable contract on two axes. The
asymmetry with David Whittaker (`../dw/PLAN.md`) is the whole reason IT is
cheap to verify: **DW has no reference player**, so its fidelity oracle had
to emulate the 68k replayer in Ghidra; **IT has a real reference player** —
**Schism Tracker is the authoritative reference** (`~/Documents/schismtracker-…`,
rendered headless `--diskwrite`), with libopenmpt kept only as a last-resort
cross-check — so the oracle is just "render the same file through the
reference and compare." (Cross-format: XM → ft2-clone, MOD → pt2-clone,
IT/S3M → Schism; openmpt is never the primary, see BASELINE.)

---

## The apparatus

| Tier | What it answers | Tool | State |
|---|---|---|---|
| 1 robustness | does every real file load to a consistent DAW layer? | `load_all_it_modules_from_music_dir` (`xmrs/src/lib.rs`) | **green** |
| 2 fidelity | does our render match a reference renderer? | `xmrsplayer/examples/it_oracle_diff.rs` vs the authoritative player (Schism `--diskwrite` for IT/S3M; ft2-clone/pt2-clone for XM/MOD; `openmpt123` last-resort) | sampled |
| — render | produce a DUT WAV for inspection | `xmrsplayer/examples/it_render_wav.rs` | — |
| — repair | restore a byte-damaged `.it` | `xmrs/examples/it_repair.rs` | — |

### Why the oracle is a tolerance, not an equality

We can **never** match the reference renderer (Schism) bit-for-bit: the two
engines differ in interpolation, volume ramping, pan law and summation order.
So the oracle
measures a *bounded structural distance* that is robust to phase and to a
constant level offset, yet sensitive to the failures that mean "wrong
playback":

- `env_corr` — Pearson correlation of the short-time **log-energy
  envelopes** (best of a small lag search). Catches dropped notes, wrong
  timing, dead channels. Level-invariant.
- `spec_sim` — mean per-frame cosine of **magnitude spectra** (log-binned,
  Hann-windowed). Catches wrong notes / wrong samples / wrong tuning.
  Scale-invariant.
- `level_db` — `20·log10(rms_dut / rms_ref)`. A constant offset is
  harmless; reported separately so it never contaminates the structural
  score.

The reference is rendered out-of-band (raw s16le, since the WAV writer
needs libsndfile which we don't link), so no third-party code is compiled
into the test — only the metric, which models *documented* IT semantics,
lives in the repo. Same licensing discipline as the loop reference oracle.

---

## Sub-song automation (delivered)

The Tier-1 gate immediately paid for itself: it found that multi-sub-song
modules merged **all** sub-songs' automation into flat lanes, even though
each sub-song's timeline restarts at tick 0 — so sub-song *N*'s
Bpm/Speed/GlobalVolume aliased into sub-song 0's identical tick window (and
the Points lanes came out non-monotonic, which is what the invariant
caught). Fix: `AutomationLane` carries a `song: u16`; the importer stamps
it per sub-song (`extract/mod.rs`, `dw_module.rs`); `Module::lanes_for`
filters by `(target, song)`; the player passes its current sub-song. DW
already emitted per-sub-song-separate lanes, so it was unaffected; the
whole golden/oracle suite stayed bit-identical. (Single-sub-song = the
common case = `song 0`, default, byte-identical.)

---

## What's left (to actually declare IT "done")

1. **Level calibration — DONE.** The +8…+18 dB hot offset was a real player
   bug (`apply_final_gain` never folded in `mix_volume`); fixed, and level now
   matches the authoritative references to median |level_db| ≤ 0.2 dB
   (BASELINE §Tier-2). Residual: a constant ~+9 dB **only** on ModPlug-made ITs
   (OpenMPT's `MixLevels::Original` convention) — tracked, and level-invariant
   metrics (env_corr/spec_sim) are unaffected.
2. **Feature-coverage matrix.** Enumerate the IT feature set from
   `ITTECH.TXT` / Schism source: `Sxx` sub-commands, `Gxx`/`Hxx`/`Rxx`
   memory, NNA/DCT/DCA, envelope types, resonant filter, sample vs
   instrument mode, sample vibrato, `Zxx` MIDI macros, MPTM extensions,
   compressed/stereo samples. Mark each *implemented+fixture* /
   *implemented+untested* / *missing*. This catches "coded but never
   exercised" and "silently absent" — neither is visible today.
3. **Cluster the corpus** (316 files) by feature profile and pick ≥1 Tier-2
   representative per cluster (instrument-mode, NNA-heavy, filter-heavy,
   `Zxx`/MIDI, plugin-bearing = the census 6, MPTM, compressed samples).
4. **Expand the golden corpus** from 1 IT fixture to ~6, one per cluster,
   for bit-exact *self-consistency over time* (distinct from the oracle's
   external-correctness check).
5. **Triage low-`env_corr` outliers** once the matrix exists.

### Decision rule

> IT is "done" when: Tier-1 gate green **and** every Tier-2 cluster
> representative is under the fidelity threshold *T* (with documented
> exceptions, e.g. a known constant level offset) **and** the feature
> matrix has no *implemented-untested* or *missing* row **and** an
> ear-check of the representatives + known hard cases is clean.

Until that holds, IT stays **WIP** — which is exactly why the README does
not claim otherwise yet.
