# IT fidelity — measured baseline

Snapshot of where Impulse Tracker playback stands, measured against the
[apparatus described in `PLAN.md`](PLAN.md). Two axes: **robustness**
(does every real file load to a consistent DAW layer?) and **fidelity**
(does our render match a reference renderer?).

---

## Corpus-wide sweep vs Schism — 2026-06-11

After the three June fixes (S3M pattern-length off-by-2, `0xFE` order-skip,
NNA voice-pool saturation), a full `env_corr`-vs-**Schism** sweep of every
`~/Music` IT + S3M (`~/Documents/fidelity_sweep_2026-06-11.tsv`, a generated
report kept outside the crate; columns
`format · env_corr · xmrs_rms · schism_rms · status · path`; render = xmrs
full-mix 60 s @ 44.1 kHz vs Schism `--diskwrite`, log-energy-envelope Pearson,
±8-hop lag):

- **322 IT + 122 S3M.** 432 comparable (OK), 11 SCHISM_FAIL (Schism segfaults —
  not comparable), 1 XMRS_FAIL = `SNG23.IT` (the known byte-corrupt file, see the
  it-repair note — not an import bug).
- **IT median 0.940 · S3M median 0.960.** Of 431 comparable: **71 % ≥ 0.90,
  91 % ≥ 0.80.** Healthy, no single dominant class defect remaining.
- **The tail (< 0.8, ~38 files) is heterogeneous / individual**, not one bug:
  - *Known / expected:* `bf_p1.it` (legit slow fade-in intro; the `-2.0` is the
    silent-window sentinel), `and_this_is_ultrasound.s3m` & co (OPL/AdLib FM gap —
    a missing feature, not a bug), the SCHISM_FAIL set.
  - *`discovery.it` — investigated 2026-06-11, NOT the simple class it looked
    like.* It is a deep, file-specific divergence, ruled NOT to be: voice-pool
    saturation (count plateaus ~100, never hits 256), timing drift (wide-lag
    windowed xcorr ≈ 0.05–0.15 at *all* lags with a random best-lag — the two
    renders are genuinely different audio, not time-shifted), or a
    relative-pitch *import* error (the c5_speed→relative_pitch values match
    xmrs's own runtime anchor — note 60 = 16726 Hz, e.g. c5=22050 →
    `12·log2(22050/16726)=+4.77→+5`, exactly what is imported). Signals: the
    intro crash reads ~196 Hz dominant in xmrs vs ~98 Hz in Schism (octave-
    ambiguous — could be a 2nd-harmonic/interpolation timbre split rather than a
    true pitch error) and ~5× quieter; the back half drifts to ~1.8× Schism's
    level; whole-song waveform correlation is near-zero. The file uses
    high-sample-rate instruments (c5_speed 22–59 kHz) and resonant-filter
    instruments ("…+ Bandwidth", "Gamelan Bell"), so the likely culprit is the
    sample-step / interpolation / filter playback path for far-from-8363
    c5_speeds — but it is **a hard individual case, not a corpus-wide class bug**
    (a universal octave/pitch error would have sunk far more than 1 file). Low
    ROI; deferred. Diagnostics added: `xmrs/examples/sampinfo.rs`,
    `xmrsplayer/examples/audfreq.rs`.
  - *Structural / timing* (the largest bucket, ~31 files): RMS matches Schism
    (ratio ≈ 1.0) but `env_corr` 0.5–0.8 — envelope-shape / swing-phase / fine
    timing divergence, partly metric over-sensitivity on busy tracks
    (`looort (drax)`, `harmosc`, `batman returns`, `SMOKATER.S3M` …).
  - ⚠ The 60 s-aggregate level-ratio is a *noisy* classifier: `art-o-sine.it`
    reads "2.2× loud" over 60 s but is ≈ 0.9× in the first 30 s — a late-section
    divergence, not a global level bug. Always confirm outliers time-resolved.

Corpus: `~/Music` recursively. The Tier-1 gate now covers **all four**
header-based formats; this file's *fidelity* numbers (Tier-2) stay
IT-only.

---

## Tier 1 — robustness (gate, in CI) — all four formats

One shared driver, `run_format_load_gate(ext, loader)` (`xmrs/src/lib.rs`),
backs a per-format test for IT / XM / MOD / S3M. It loads every file with
the extension under `~/Music` (recursively) and asserts each one either
decodes to a consistent DAW layer (`to_module()` +
`verify_layers_consistent()`) or is cleanly rejected.

```
load_all_it_modules_from_music_dir   322 files, 321 ok,  1 rejected,  0 failures → GREEN
load_all_xm_modules_from_music_dir   626 files, 626 ok,  0 rejected,  0 failures → GREEN
load_all_mod_modules_from_music_dir  425 files, 425 ok,  0 rejected,  0 failures → GREEN
load_all_s3m_modules_from_music_dir  121 files, 121 ok,  0 rejected,  0 failures → GREEN
```

**What is a failure** — only a **panic** in the loader, or a successful
load with an **inconsistent layer**. A clean `Err` is *not* a failure:
declining a file that wears the wrong extension is correct. We assert this
without a hand-maintained allow-list — `Module::load` autodetect tries the
same loader first, so it can never re-accept as format `ext` a file the
loader just rejected; success there means the file is mislabeled, failure
means it is damaged. A loose ceiling (reject ≤ ¼ of the corpus) catches a
mass loader regression.

- **IT — 1 rejected**: `SNG23.IT`, byte-damaged (`IMPI`/`IMPS` zeroed;
  `examples/it_repair.rs` restores it).
- **19 mislabeled `.xm` renamed**: the gate first flagged 19 `.xm` files
  that were really IT (6), MOD (12) or S3M (1) — `Module::load` autodetect
  loaded all 19 to a consistent layer. They were renamed in `~/Music` to
  their true extension by magic (`IMPM`→`.it`, `SCRM`→`.s3m`, `M.K.`/
  Soundtracker→`.mod`), which is why the IT/MOD/S3M counts above grew and
  XM's shrank. The corpus is now honestly classified.

The gate surfaced **real importer bugs across every format**, all fixed:

- **IT — 14× `AutomationPointsUnsorted`**: multi-sub-song modules merged
  every sub-song's automation into one lane (each sub-song's tick space
  restarts at 0, so sub-song *N*'s Bpm/Speed/GlobalVolume aliased into
  sub-song 0's tick window). Fixed by giving `AutomationLane` a `song`
  dimension (see `PLAN.md` §"Sub-song automation"); all 46 multi-sub-song
  IT modules now consistent.
- **IT/MOD — `ClipsOverlap`**: a segmented pattern-loop body let two clips
  claim an overlapping tick window. `clamp_clip_overlaps_per_lane`
  (`build/segments.rs`) now both **truncates** an overshooting clip's
  `end_tick` to the next clip's start *and* **drops** the dead clip of an
  exact `position_tick` tie. Both are audio-neutral: `active_at` ignores
  `end_tick` and always returns the later-emitted clip of equal-position
  pairs, so the truncated tail / dropped clip was never heard. (IT: `tight
  to me.it`, `you can do it but boue it.it`; MOD: `dove part 1.mod`,
  `b.s.t.mod`.)
- **MOD — 48 spuriously-rejected genuine `M.K.` MODs**: the 31-sample
  detector demanded clean ASCII in sample names and the full declared
  sample-data size; real scene MODs leave control bytes in name slots and
  truncate the final sample. Fixed by anchoring the tagged variant on its
  4-byte tag alone — name / finetune / volume / full-size checks now apply
  only to the untagged 15-sample variant, while *pattern* data must still
  be fully present for both (read by direct index).
- **S3M — 1 spuriously-rejected file** (`strshine.s3m`): the loader
  required the DOS `type`-EOF byte 0x1A at offset 0x1C, which this file
  leaves 0x00. The authoritative signature is `"SCRM"` (+ type 0x10);
  matched libopenmpt by dropping the EOF-byte check.
- **XM — junk flag bits**: the frequency-table flags word was parsed as an
  exact `0`/`1`; some files carry junk in the reserved upper 15 bits
  (0x4000, 0x2020). Now masked to bit 0, as FT2/OpenMPT do.

Every fix is bit-identical for single-sub-song / loop-free modules and
leaves the whole XM/MOD/S3M/DW golden + oracle suite unchanged.

---

## Tier 2 — fidelity (oracle, sampled)

Tool: `xmrsplayer/examples/it_oracle_diff.rs` (format-general, `Module::load`
autodetect). Metric is a **bounded structural distance**, not bit-equality
(impossible across two mixers): `env_corr` = log-energy-envelope correlation
(timing / dynamics, phase-robust), `spec_sim` = mean per-frame
magnitude-spectrum cosine (notes / timbre), `level_db` = our RMS minus the
reference's. `env_corr`/`spec_sim` are level-invariant by construction.

### Cross-format structural fidelity — *the* answer to "is it more than level?"

Each format is swept against its **authoritative** reference player (the
real thing, built from local source), not just OpenMPT: **MOD → pt2-clone,
XM → ft2-clone, IT/S3M → Schism Tracker** (`--headless --diskwrite`, 44.1 kHz;
build needs `libutf8proc-dev`+`libsndfile1-dev`). `openmpt123` is the
*last-resort* reference only. An evenly-spaced sample across each corpus,
20 s window:

| format | reference | N | env_corr med | spec_sim med | \|level_db\| med | env ≥ 0.90 |
|---|---|---|---|---|---|---|
| XM  | ft2-clone | 57 | **0.971** | 0.961 | 0.20 | 48/57 |
| MOD | pt2-clone | 29 | **0.970** | 0.938 | 0.13 | 21/29 |
| S3M | Schism    | 39 | **0.965** | 0.955 | 0.05 | 32/39 |
| IT  | Schism    | 51 | **0.943** | 0.934 | 0.19 | 34/51 |

**Order `0xFE` skip-marker split as a sub-song (importer, FIXED 2026-06-10).**
The shared order-table parser `orders_helper::parse_orders` (IT / S3M / XM)
treated `0xFE` **and** `0xFF` as sub-song separators. But `0xFE` ("+++") is a
**skip** marker — skip this order position, *continue the same song* — not a
boundary; only `0xFF` ("---") ends a song. Splitting on `0xFE` shredded any
module that uses it as an intra-song section separator into dozens of tiny
"sub-songs", and since the player renders sub-song 0 by default, playback
**looped the first fragment forever**. `mv-desk.it` (order
`[5,6,254,1,1,254,0,10,254,…]`, 100 entries / 31 patterns) played only
positions 0–1 on repeat: env_corr **0.329**, level drifting to −19 dB as the
real song's later voices never entered. Fix: `0xFE` is now skipped (omitted,
same song); only `0xFF` splits. **`mv-desk.it` env_corr 0.329 → 0.964** (spec
0.741 → 0.922, level −6.3 → +0.2, per-third flat). **42 / 322 IT modules** (and
1 S3M) carry a `0xFE` in their order — the affected class; spot-checked
survivors (`just love 3` 0.93, `dnz_wod` 0.90, `methuselah` 0.87) all play
their full order now. Zero regression: cycle_exact 20/20 + golden_corpus 2/2
bit-identical, all 5 format gates green, xmrs lib 354/0 (`parse_orders` unit
tests updated). DW/MOD build orders directly and never used this helper, so
they are untouched. Matches Schism (skip `0xFE`, stop at `0xFF`).

**S3M pattern-length off-by-2 (importer, FIXED 2026-06-10).** The single
documented "timing fully decorrelated" tail case, `classical favorites
ii.s3m` (env_corr **0.009**), was a real importer bug: the S3M packed-pattern
2-byte length field is the size of the body that *follows* it (Schism
`fmt/s3m.c`: `end = paraptr + len + 2`), but the loader read the body as
`data[2..len]` — i.e. `len − 2` bytes — **lopping the last 2 bytes off every
pattern**. Harmless when the tail was padding / trailing EOL bytes, but on a
tightly-packed pattern it truncated the final slot's effect parameter: pattern
35's last row carried `A30` (set speed 48, stretching that row to ~720 ms),
read back as `A00` (a no-op) → the speed change vanished, the song ran ~620 ms
ahead from then on, and the oracle's ±93 ms lag search reported ~0 correlation
even though steady-state cross-correlation was 0.99. Fix in
`xmrs/src/tracker/import/s3m/s3m_module.rs`: body is `data[2..2 + len]`, and the
row decode is capped at 64 like Schism (`while row < 64`). **`classical
favorites ii.s3m` env_corr 0.009 → 0.958** (spec 0.748 → 0.938). Zero
regression: cycle_exact 20/20 + golden_corpus 2/2 bit-identical (other formats
untouched; the change is S3M-only), S3M gate 122/122, xmrs lib 354/0. A
class-level bug — every S3M lost its last 2 pattern bytes, silently except
where a real command sat in the last row. Diagnostic added:
`xmrs/examples/speedlane.rs` (dumps the Speed/Bpm automation-lane points).

**Reading.** Level is calibrated everywhere (median |level_db| ≤ 0.2 dB).
*Structure* — the part beyond level — is faithful in the median: env_corr
0.94–0.97, spec_sim 0.93–0.96, i.e. most modules play the right notes at
the right time, not merely the right loudness. But every format has a
**tail of genuine divergences** (envelope / NNA / filter / timing, *not*
level), heaviest on **IT** (the richest format: NNA/DCT, resonant filter,
`Zxx` macros). The two S3M outliers the cross-format sweep surfaced both
turned out **not to be bugs**: `and_this_is_ultrasound.s3m` (env 0.43, level
dropping to −11 dB in places) is a **feature gap** — it mixes PCM (GUS) with
**AdLib/OPL FM** instruments (≈20 type-2 instruments; the song's own sample
names announce it), and xmrs has no OPL synthesis, so those channels are
silent (Schism emulates OPL). `jazz jackrabbit - bonus.s3m` (env 0.76→0.83)
is all-PCM and actually **fine**: at 0.5 s resolution its envelope tracks
Schism to within ±0.1–0.5 dB across the whole song — the sub-0.9 env_corr is
fine-scale onset/ramp metric noise, not a real divergence. So after the
pattern-length fix the S3M corpus is clean (residual = the OPL feature gap +
the ModPlug-MixLevels IT level convention, both tracked, neither a one-liner).
This is the concrete remaining fidelity work, now
quantified.

**`beside you.it` — ROOT-CAUSED (2026-06-10): NNA voice-pool saturation, NOT a
timing slip.** The earlier "~150 ms slip at 33 s" characterisation was wrong.
Empirical windowed cross-correlation vs Schism shows the render is faithful for
~30 s (xcorr 0.8+, RMS tracks), then at **~32 s the section that should get
~2.7× louder (Schism RMS 2100→4536) stays flat in xmrs (~1610)** — an ~8 dB
*drop* of a whole sustained layer, not a timing event. Diagnosis chain:
- *Seek vs continuous.* Seeking the player to order 4 renders the section at
  RMS 4207 (≈ Schism 4536); continuous playback from order 0 renders the same
  bars at 1610. So a *state that accumulates over orders 0–3* suppresses order 4+
  (seeking to order 3 already gives the correct loud order 4 — the bad state
  builds over the whole intro, not locally). Global volume is a flat 1.0
  throughout (ruled out via `gvoltrace`).
- *Voice count.* `voicetrace` shows the allocated voice count growing
  **monotonically and unbounded** — 35 @2 s → 225 @20 s → 333 @32 s → 460 @44 s
  — crossing the default pool cap (256, matching Schism's `MAX_VOICES`) at
  exactly ~32 s, where the divergence starts.
- *Why it leaks.* Every instrument is `DCT = Off` + `NNA = Continue`
  (`nnadump`), so each retrigger forks the outgoing voice into a perpetual
  background ghost and nothing duplicate-cuts it. Ghosts are reaped only when
  the volume envelope ends at **zero** (`is_alive` half 2) or the sample stops.
  But several instruments (12, 17 …) pair a **looping sample** with a volume
  envelope whose **last node is full (Q15 32767), no loop, no sustain**
  (`envdump`) — the envelope clamps and holds at full, the sample loops forever,
  so the ghost is **immortal** (`fadeout = 0` ⇒ the fadeout-register reap half
  never fires either). These pile up without bound.
- *Why it goes quiet (not just thinner).* Once the 256-slot pool jams full of
  immortal full-volume sustained ghosts, every new live note must `allocate_live`
  → evict one of them. The immortal ghosts *are* the loud sustained
  pad/drone layer, so live notes decimate it → the section collapses ~8 dB. A
  larger pool removes the eviction: at cap 1024 xmrs renders order 4 at RMS 4305
  (≈ Schism), bit-faithful to the no-eviction result.
- *The real divergence — FIXED (2026-06-11).* Both engines cap at 256, but they
  differ in what a *full* pool does on an NNA trigger. xmrs always backgrounded
  the outgoing voice (`promote_live_to_ghost`, keeps its slot) and then allocated
  a **fresh** slot for the new note via `allocate_live`, which **evicts** an
  unrelated background voice when full. So every NNA = Continue trigger at the cap
  killed one sustained background voice → the pad/hat layer was continuously
  decimated → the ~8 dB drop. Schism's `csf_get_nna_channel` (`effects.c:1637/
  1679`) instead **returns 0 when no NNA channel is free** — the outgoing note is
  *not* backgrounded, its host slot is reused by the new note, and the existing
  background voices are left untouched. **Fix:** `Channel::spawn_ghost_for_outgoing`
  now early-returns when `pool.is_full()` (skip backgrounding); `install_live`
  then releases the outgoing voice's own slot and reuses it — no eviction,
  voice count bounded at the cap exactly like Schism. The note-only retrigger
  path (`spawn_ghost_clone`) was already correct (uses the refuse-on-full
  `allocate_ghost`). **Result:** voice count plateaus at 256 (was unbounded);
  order-4+ RMS 1610 → 4100–4227 (Schism 4537–4557, within ~0.7 dB);
  **env_corr 0.271 → 0.978**. **Bit-identical** for every non-saturating module
  (golden_corpus 2/2 + cycle_exact_pcm 20/20 unchanged) — the new branch only
  fires at pool saturation, which is rare (≈ only `beside you.it` in a 200-file
  IT sweep). Fixes the whole class of NNA-heavy IT with non-decaying instruments.
  Diagnostic tools kept: `xmrsplayer/examples/voicetrace.rs` + `poolcap_render.rs`,
  `xmrs/examples/nnadump.rs` + `envdump.rs` + `tldump.rs` + `celldump.rs`;
  `VoicePool::{allocated_count,is_full}` un-gated + `XmrsPlayer::allocated_voice_count`.

**Tail-case re-audit (2026-06-10) — the two "level drift" cases were
mischaracterised.** The earlier note flagged `control.it` and
`AceMan - Irreversible.xm` as "level drifts over the song = global-volume /
envelope bug." Measured against the *authoritative* references (IT→Schism,
XM→ft2-clone, not OpenMPT):
- **`control.it` — no drift.** Per-third `level_db` is flat
  `[-0.0/-0.0/-0.0]` vs Schism (and `[-0.1/-0.0/-0.1]` vs OpenMPT) over the
  full 69 s. The drift was a measurement artifact / pre-`mix_volume`-fix
  state; **resolved**, dropped from the tail.
- **`AceMan - Irreversible.xm` — real, but NOT a global-volume bug.** The
  excess is *localised* to ~10–42 s (`[+3.7/+0.4/+0.2]` per-third) and
  spectrally concentrated in the **sub-bass** (+11 dB <100 Hz, +8 dB
  100–300 Hz, tapering to +0.2 dB by 2 kHz). It coincides exactly with the
  PingPong-looped bass instruments (#11–14, volume-envelope *disabled* →
  played at full sample volume) being active on channels 12/13/14. Pitch is
  correct (fundamental ~65 Hz both sides — *not* an octave-down bug), and
  the song's one `GlobalVolume` **slide lane verified to apply correctly**
  (drives `global_volume`→0 at ~248 s, the song's last 10 s; a `gvoltrace`
  example was added to confirm). So the divergence is a per-module bass
  **level** excess on specific voices (candidate: a missing per-voice
  attenuation ft2 applies when many full-volume bass voices stack), not a
  global-volume or envelope defect — **not root-caused, low yield** (single
  module; XM median env_corr is 0.971). Diagnostic tools added:
  `xmrsplayer/examples/gvoltrace.rs` (traces engine `global_volume` +
  pos/row/song over time) and `xmrs/examples/instr_dump.rs` (per-instrument
  volume-envelope / fadeout / sample-volume dump).
- **Aside (low priority):** with no `Clear` event, a GlobalVolume *slide*
  lane stays armed past the song loop point, so on a looping render
  `global_volume` never recovers after the slide. Harmless for the
  single-pass oracle window; revisit if loop-fidelity matters.

**Filter-envelope cutoff fix (2026-06-10).** `xemogasa_-_azure.it` (was
env 0.452 / spec 0.672 / −8 dB, looked like a missing feature) was a
**resonant-filter calibration bug** in the player
(`xmrsplayer/src/state_instr_default.rs`, `pitch_envelope_as_low_pass_filter`
path). Two errors vs. Schism (`player/filters.c` + `sndmix.c`):
(1) when a filter **envelope** drives the cutoff and the instrument's IFC
enable bit (0x80) is clear, the base must default to **0x7F** (Schism inits
every voice's cutoff to 0x7F, overwriting only `if (penv->ifc & 0x80)`) —
xmrs used `ifc & 0x7F` = 0, pinning the filter shut at ~130 Hz and gutting
the midrange; (2) the modulation produced Schism's 0..255 register `S =
base·value·2` but fed it to xmrs's half-scale 0..127 LUT (exponent reg/24
= S/48), so it must feed `S/2 = base·value`, i.e. `>> 15` not `>> 14`.
Spectrum proof: the −9.6 dB midrange notch on xemogasa vanished, residual
went flat (matches a filter-bypass render). Net gains, zero regression
(golden bit-exact 20/20, xmrs lib 354/0): `corque screw` env 0.481→**0.716**
(its low env_corr was largely this filter, not timing), `firage`
0.982→0.989, `lemongelon` 0.867→0.875, xemogasa spec 0.672→**0.738**;
`the next.it` (filtenv=15) and every static-filter module unchanged.

**Tone-portamento with no live voice (2026-06-10).** A `Gxx` note with no
currently-playing voice (seeking mid-song onto a `Gxx` row, or a channel's
first note being `Gxx`) must **trigger** the note, not slide — Schism
`csf_instrument_change`: `if (porta && !chan->length)` falls through to a
normal sample load. xmrs slid from a stale period → wrong held pitch. Fixed
in `xmrsplayer/src/channel/tick0.rs` by capturing `had_live` before the
instrument loads and gating both tone-porta paths on it. Seek-only (never
fires in sequential playback → goldens bit-identical). Verified across 8
modules (4–35 genuine seek-`Gxx` firings each); A/B `unreal tournament -
organic` 0→audible, `epsong11` 0→audible.

**Note-column `^^^` (NoteCut) never cut (2026-06-10).** A note-column
`^^^` (`CellNote::NoteCut`, 254) was inert: `CellEvent::NoteCut` is
documented as dispatch-inert (the cut rides a same-row `TrackEffect::NoteCut`),
but the importer only emitted that effect for the *effect-column* cut
(`SCx`/`ECx`), never the note column — so a bare `^^^` rang forever. Fixed
in `xmrs/src/tracker/import/unit.rs` (`prepare_cell`, the shared unit→Cell
site, so all formats): co-emit `TrackEffect::NoteCut { tick: 0, past: false }`
for `CellNote::NoteCut`. Matches Schism `fx_note_cut`. Zero regression
(goldens bit-identical — they use no note-column cut); net-positive on clean
modules: `a-------.it` (100 cuts) env 0.968→**0.971**, level +0.10→**+0.02**.
NB xmrs has no volume ramping (cycle-exact), so the cut is hard, not Schism's
`FASTVOLRAMP` few-ms anti-click ramp — a pre-existing global trait, not this
fix.

**Harness caveat (found + fixed).** A short reference-render timeout
truncates long modules' reference WAVs and yields *spurious* near-zero
env_corr (`spinning atom.xm` looked like 0.00, is really 0.96). pt2-clone
loops so it is always timeout-killed (the partial WAV is still valid for a
20 s window); ft2-clone/Schism render once and need a generous timeout for
big tunes. Always re-verify any `env_corr < 0.85` with an isolated full
render before believing it.

### IT detail vs OpenMPT (last-resort reference)

Sampled run, 30 s @ 48 kHz, OpenMPT at `--filter 1 --ramping 0`. The
player now applies `mix_volume` natively (see "Level fix" below), so these
are raw, uncompensated numbers:

| module | env_corr | spec_sim | level_db |
|---|---|---|---|
| deus ex - the illuminati | +0.965 | 0.907 | −0.12 |
| operation na pali - castle gate | +0.981 | 0.972 | +8.83 |
| unreal - twilight horizon | +0.986 | 0.929 | +0.19 |
| unreal - nali chant | +0.877 | 0.931 | +0.07 |
| np-linp | +0.996 | 0.915 | +9.48 |
| unreal - main title | +0.992 | 0.910 | −0.01 |
| unreal - hub 2 | +0.846 | 0.941 | −0.11 |
| chris31b | +1.000 | 0.963 | +0.23 |

Reading: structurally **close** to OpenMPT (env_corr mostly > 0.96,
spec_sim ~0.91–0.97), and after calibration the level matches to **±0.25
dB** on most modules.

**Level fix (`mix_volume`) — a real player bug, now fixed.** The first run
showed a *variable* +8…+18 dB offset. Root cause: the player's
`apply_final_gain` applied `global_volume × amplification` but **never**
`mix_volume` — even though `Module::mix_volume` held it correctly from
import and two comments *claimed* it was applied. libopenmpt *does* scale
the master by `mix_volume`, so we rendered `128/mix_volume` louder (mv
commonly 48/128 → +8.5 dB) and clipped hot modules. Fixed in the **player**
(`Voices::apply_final_gain` now folds `mix_volume` into the ≤1 gain factor,
before the i16 clamp). All XM/IT/MOD/S3M goldens were re-baselined
(`XM_GOLDEN_REGEN`/`XM_PCM_REGEN`); DW is `mix_volume = FULL` so it stayed
byte-identical — which proves the change is a cleanly-gated pure gain.

Cross-format calibration (same oracle, `Module::load` autodetect). IT/S3M
carry a real master volume in the file; XM/MOD don't, so their `mix_volume`
is a convention constant calibrated against the **authentic** reference
players (built from local source, patched for headless WAV render):

| format | mix_volume | reference | level after calibration |
|---|---|---|---|
| IT | file header | Schism (openmpt cross-check) | ~0 dB |
| S3M | `master_volume` (real field) | Schism | ~0 dB vs Schism (+2.7 vs openmpt last-resort) |
| XM | **57/128** | **ft2-clone** | −0.1 dB (median over 12) |
| MOD | **64/128** | **pt2-clone** | −0.1 dB |

- **XM → 57/128**, calibrated against **ft2-clone** (cycle-accurate FT2) at
  factory amp. At the old 48/128 we were −1.48 dB (median, env-corr > 0.9,
  12 modules) below ft2-clone; `48/128 × 10^(1.48/20) ≈ 57/128`. Note
  ft2-clone is ~1.6 dB *quieter* than OpenMPT here, so OpenMPT would have
  mis-calibrated XM — the authentic reference matters.
- **MOD → 64/128 (= 0.5)**, calibrated against **pt2-clone** (cycle-accurate
  ProTracker), and independently *derived* from its source: the Paula mixer
  is `NORM_FACTOR(2.0) × 32768/PAULA_VOICES(4)`, so one full voice peaks at
  16384 = half our full-scale accumulator → 0.5 exactly. Empirical median
  (−2.6 dB below pt2 at 48/128, env-corr-clean) confirms it.

Both now sit at −0.1 dB vs their reference. The headless render harness:
`pt2-clone <mod> <out.wav>` / `ft2-clone <xm> <out.wav>` (a small patch to
each `main` calls the existing offline renderer, run under
`SDL_VIDEODRIVER=dummy SDL_AUDIODRIVER=dummy`), then the same
`it_oracle_diff` measures the level. S3M keeps the file's real
`master_volume`; only XM/MOD needed a convention constant.

Open items:

1. **Residual ~+9 dB on ModPlug-made modules** (`operation na pali`,
   `np-linp`) — **root cause identified, not a gain-math defect.** These
   are ModPlug/OpenMPT-made ITs (`cwtv=0x0217`, `cmwt=0x0200`); OpenMPT
   detects that and switches to `MixLevels::Original` (`Load_it.cpp:1000`,
   `SoundFilePlayConfig.cpp`), a *quieter* mixing law — `normalGlobalVol`
   128 vs 256, plus an `extraSampleAttenuation = MIXING_ATTENUATION` — to
   stay faithful to ModPlug. Ordinary ITs (`cmwt=0x0100`, e.g. `deus ex`)
   keep `MixLevels::Compatible` and match us at 0 dB. The offset is
   *constant* across the file (per-third `+9.7/+9.4/+9.4`), confirming a
   fixed per-module gain law, not an NNA voice leak. We verified all five
   IT gain stages are applied correctly (mix_volume, header global_volume,
   channel-default volume, sample `gvl`, instrument global volume), so this
   is a per-module **level convention** OpenMPT models and we do not.
   Matching it would mean detecting ModPlug-made ITs at import and applying
   the Original-level attenuation (`normalGlobalVol` 128 + extra
   attenuation) in the player gain — a specialised fidelity feature,
   tracked here, not a one-line fix. Level-invariant metrics
   (env_corr/spec_sim) are unaffected.
2. **Low-`env_corr` outliers** (`nali chant` 0.877, `hub 2` 0.846) — triage
   once the feature-coverage matrix (PLAN.md) exists; candidates are
   NNA/DCT, resonant filter, or `Zxx` MIDI-macro paths.

This is a *sample*, not a verdict: no per-cluster representative set and no
pass/fail threshold are fixed yet (PLAN.md §"What's left").

### Reproduce

The authoritative reference is **Schism** (`schismtracker --headless --diskwrite`,
44.1 kHz; XM/MOD use ft2-clone/pt2-clone). The `openmpt123` recipe below is the
*last-resort* path, kept only because it needs no codecs (raw s16le out):

```sh
# last-resort reference (BSD libopenmpt built locally; raw s16le, no codecs needed)
openmpt123 --render --samplerate 48000 --channels 2 --no-float \
    --filter 1 --ramping 0 --gain 0 --output-type raw --force <m.it>
# device-under-test + diff (renders DUT in-process; `it_oracle_diff` takes any
# reference raw — Schism's --diskwrite WAV converted to raw works identically)
cargo run --example it_oracle_diff --features import_it -- <m.it> <m.it.raw> 30 48000
```
