# Rob Hubbard "v40" player — study + plan to add it

> ⚠️ **The instrument system of this player has since been re-reversed in depth:**
> see [`ROBSID2_INSTRUMENT_MODEL.md`](ROBSID2_INSTRUMENT_MODEL.md), which **supersedes §3
> (the instrument record), §4 (markers) and §6 (the filter) of this document** — several
> statements below are wrong (record fields +2/+5/+10/+11/+14, the flags of +7, the operand
> order of markers `0x82`/`0x87`, the meaning of `0x88`, the filter slot byte +6, and the
> claim that `Lakers_vs_Celtics` is a distinct variant). That document lists every correction
> in its §10. §2 (song/order/pattern), §7 (comparison), §8 (design decision) and §9 (phased
> plan) below remain valid.

A complete reverse-engineering of Rob Hubbard's late / EA-era C64 player (the next
generation after v30/Delta in our empirical version scheme), and a phased plan to import it.

**Ghidra source of truth:** `Lion_Heart.bin` (HVSC, load `$1003`, init `$100c→$18ff`,
play `$101a`, wave engine `$1699`). Lion_Heart is the cluster representative.
**Tune coverage:** the player is shared (98–100 % identical code) by **Lion_Heart,
Sun_Never_Shines, Pacific_Coast, Radio_ACE, Go_Go_Dash**; `Lakers_vs_Celtics` is only 12.7 %
identical — a *distinct* variant, handled separately or dropped. Data-table addresses vary
per tune (see §9).

**Assumes** the wavetable engine (`WAVETABLE_ENGINE_PLAN.md`, "Tier 1") and the bit4/bit6
effects (3-note arpeggio + two-phase-pitch, already implemented) are in place — v40 reuses both.

---

## 1. The big picture — why v40 is a different player

v10–v30 share one shape: an **8-byte instrument record + an fxmask byte** (per-instrument
flag bits) + an optional `fx_v2` table. Effects are **instrument-scoped flags**.

v40 replaces this with a **layered, tracker-like** design whose effects are **per-note inline
commands**:

```
subtune ─▶ 4 order-lists ─▶ patterns ─▶ entries: [effect markers] [length] [note]
           (3 voices +                              │
            1 filter track)              16-byte instrument record (the patch)
                                                    │
                                         wave-table = per-frame command engine
```

The fundamental shift: **effects bind to the NOTE (inline markers), not to the instrument
(fxmask bits).** The same musical effects exist on both sides (vibrato, arpeggio,
portamento, PW sweep, filter, wavetable) — only the binding site moves.

## 2. Song / order / pattern format

**Init (`$18ff`, arg = subtune index):**
- `tempo` ← `DAT_2de3[subtune]` (+ a 2nd flag `DAT_2de4`).
- For each of **4 tracks** `v` (0..3): order-list pointer ←
  `(DAT_1b43[subtune*4 + v*2], DAT_1b44[…])` (lo, hi).

**The 4 tracks = 3 SID voices + 1 global filter track.** `regoffset $1a2d = [0, 7, 14, 0]`
and the voice loop count `DAT_1aa9 = 2` ⇒ tracks 0–2 drive the three SID voices
($d400/$d407/$d40e). **Track 3 is a filter-automation timeline** (§6), walked by a dedicated
block at the end of `play`; it uses the same pattern format but its markers set the filter,
not notes.

**Order-list** (per track, one byte per entry):
- `0xFF` → restart the order-list (loop to 0).
- `0xFD N` → set the track **transpose** to `N`, advance 2, keep reading.
- otherwise → a **pattern index**; fetch the pattern pointer `(DAT_1d50[i*2], DAT_1d51[i*2])`.

**Pattern** (one stream, shared pattern table for all tracks): a run of entries, each =
`[zero or more effect markers] [length/gate byte] [note]`, ended by `0x60`. The
**length/gate byte** has bit6 set (`0xC0–0xFF`): low 5 bits = note length (rows),
`& 0x20` = no-release (legato).

## 3. The 16-byte instrument record (the patch)

Array of structs, base `$1b4b`, stride 16, selected by the wave/instrument index (marker
`0x80`). Fields (Ghidra-mapped from `play` usage):

| off | field | role |
|-----|-------|------|
| +0,+1 | PW init lo/hi | `d402/d403` on note-on |
| +2   | gate-off / no-wavetable ctrl | `d404` when the wavetable is off |
| +3,+4 | ADSR (AD, SR) | `d405/d406` |
| +5   | vibrato params | LFO default |
| +6   | PW sweep speed | bounce-sweep step |
| +7   | **flags** | **bit0 = run wavetable**, bit5 = filter enable |
| +8,+9 | wave-table pointer | the per-frame command engine table |
| +11  | sustain ctrl | waveform after attack |
| +12  | filter slot index | → the `$1cb0` filter table (`<<3`) |
| +13  | PW bounce bounds | hi nibble = upper, lo nibble = lower |
| +14  | vibrato param | |
| (+10, +15 auxiliary) | | |

When flag bit0 is set, the per-frame waveform+pitch comes from the **wavetable engine**
(Tone/Noise/Sustain/Loop, `$1699`) already specified in `WAVETABLE_ENGINE_PLAN.md`. v40 is
the player that actually uses the `0x86` **Loop** command (see that doc).

## 4. The per-note effect markers (the inline language)

In a pattern entry, before the length byte, a run of markers (`0x80–0x8F`, bit7 set, bit6
clear) sets per-note effects, consumed later that frame:

| marker | operand | effect |
|--------|---------|--------|
| `0x80 N` | 1 | **select instrument/wave** `N` (`N<<4` = the 16-byte record) |
| `0x82 lo hi` | 2 | **portamento** — a signed 16-bit per-frame freq slide (accumulated) |
| `0x83 N` | 1 | **arpeggio** — 3-step `[0, N>>4, N&0x0F]` (see below) |
| `0x84` | 0 | suppress the per-note filter re-init (a two-phase-style gate) |
| `0x86 N` | 1 | **vibrato** — triangle LFO, depth/speed `N` |
| `0x87 lo hi` | 2 | **filter cutoff override** (hi/lo) for this note |
| `0x88` | 0 | **legato / tie** — do NOT re-set the pitch on note-on |
| `0x89 N` | 1 | **filter voice-select** — which voice's swept cutoff drives the chip filter |

**Arpeggio `0x83 N`** (decoded through the self-modifying code at `$1408`, which patches the
`LDA #imm` operands at `$1418`/`$1421`): a per-voice 3-phase cycle — phase 0 → +0, phase 1
→ +`(N>>4)`, phase 2 → +`(N & 0x0F)`. Offsets are **unsigned, UPWARD** semitones (e.g.
`0x47` ⇒ note, +4, +7 = a major chord). The phase counter resets on note-on, so it is
**per-note** (unlike fxmask bit4's signed, globally-phased arp). If the low nibble is 0 it
collapses to a 2-step arp.

## 5. Per-frame effect engine

Each frame, per voice, `play` applies (then writes `d400/d401/d404`):
- **arpeggio** (`0x83`): the 3-phase note offset above.
- **vibrato** (`0x86`): a triangle LFO on the frequency.
- **portamento** (`0x82`): a 16-bit accumulator added to the frequency.
- **PW sweep**: bounce on the PW high nibble between record+13's bounds, step = record+6
  (same model as v10–v30).
- **filter**: per-voice swept cutoff (§6).
- **wavetable** (record flag bit0): the Tone/Noise/Sustain/Loop interpreter owns ctrl+freq
  this frame; otherwise the plain control path runs.

The math of vibrato / arp / PW / portamento is identical to what `fx_engine` already does
for v10–v30 — v40 only *feeds* these primitives from per-note markers instead of fxmask
flags.

## 6. The filter

v40 has a genuinely richer filter than v10–v30, driven every frame and written once at the
end of `play` (`$17f7`).

- **Per-instrument slot.** Record+12 indexes the filter table at `$1cb0` (stride 8): `+0`
  cutoff-hi, `+1` cutoff-lo, `+2,+3` sweep step (hi,lo), `+4` upper bound, `+5` lower bound,
  `+6` direction(bit7)/routing(bits0-2), `+7` resonance (hi nibble). The record flag bit5
  enables the voice's filter.
- **Swept cutoff.** The cutoff is a 16-bit accumulator that bounces between the slot's
  bounds — exactly like the PW sweep.
- **Per-note override.** Marker `0x87 lo hi` overrides the cutoff for a note.
- **Global selection (track 3).** The filter track's `0x89 N` sets which voice's swept
  cutoff drives the single chip filter; its length byte holds that state for N rows.
- **Chip write (`$17f7`):** `d416 = cutoff-hi[sel]`, `d415 = (cutoff-lo[sel]>>5) & 0xF`,
  `d417 = routing (OR-ed across voices) | resonance[sel]`, `d418 = volume | mode`.

> Note: the filter track's `0x80 N` slot loads 5 params from a second table (`$1d30`) and
> self-modifies a routine at `$1950–$197e`. That routine has **no caller in the binary**
> (it sits after an `RTS`, unreachable) — it is **dead in the PSID play path** (a
> game-triggered feature), so `0x80` on track 3 has no audible effect on the music and can
> be ignored by the importer.

## 7. Comparison with the existing importer/model

| axis | v10–v30 (existing) | v40 |
|------|--------------------|-----|
| song | 3 per-voice pattern streams | 4 order-tracks (3 voices + 1 filter), transpose orders |
| pattern entry | note + instrument column | `[effect markers][length][note]` (inline effects) |
| instrument | 8-byte record + fxmask + `fx_v2` | 16-byte record, no fxmask |
| effect binding | per-**instrument** (fxmask bits) | per-**note** (markers) |
| effects | drum/skydive/arp/PW/wave-alt/2-phase/filter/wavetable | porta/arp/vibrato/PW/filter/wavetable(+**Loop**)/legato |
| filter | static per-instr record (res/route/step) | per-note **swept** cutoff, voice-select, global route |
| wavetable | bit7, `{Tone,Noise,Sustain}` | flag bit0, `{Tone,Noise,Sustain,**Loop**}` |

**Reusable as-is** (the payoff of doing Tier 1 + bit4/bit6 first):
- the **wavetable engine** (`generators/sid/wave_sequence.rs`) — identical, already
  `Loop`-aware.
- the per-frame **effect primitives** in `fx_engine` (vibrato, arp, PW sweep, portamento).
- the **driver** (`SidBank`/`CoupledSid`) and the sidera chip path.

**Genuinely new work:**
1. a **layered importer front-end** (4-track orders + transpose, inline-marker patterns,
   16-byte records). The bulk.
2. the **per-note → model binding** (§8) — the one real design decision.
3. the **swept filter with slots + the filter track** — beyond today's static `Filter`.

### 7.1 Effets : v<40 ∩ v≥40 (un effet par ligne)

La distinction gauche/droite est **le site de liage** (flag d'instrument vs marqueur
de note) ; la colonne du milieu = le **noyau déjà partagé** dans `fx_engine`. Un effet
dans la colonne ∩ existe des deux côtés avec la même math, seul le liage change. `—` =
absent de ce côté.

| Effet | v<40 (flag instrument) | ∩ noyau partagé | v≥40 (marqueur note) |
|-------|------------------------|-----------------|----------------------|
| Vibrato | flag fxmask ; v10 montant / v30 bipolaire | ✓ même LFO triangle | marqueur `0x86` (par-note) |
| Arpège 3-notes | bit4 — offsets **signés**, phase **globale** | ✓ noyau 3-pas (`shift_semitones`) | `0x83` — nibbles **non-signés**, phase **par-note** |
| Portamento | décode v20+ (porta IK, reg 16-bit) | ✓ accumulateur 16-bit | marqueur `0x82` |
| PW sweep (bounce) | record per-instr (bornes + pas) | ✓ même bounce | record +6/+13 (idem) |
| Wavetable Tone/Noise/Sustain | bit7 (ace_2) | ✓ même interpréteur | flag bit0 |
| Sélection instrument | colonne note | ✓ par-note des deux côtés | marqueur `0x80` |
| Tenue / no-release | `savelnthcc & 0x20` | ✓ même gate « pas de release » | `length & 0x20` |
| Filtre (puce) | statique per-instr (rés/route/pas) | ✓ concept filtre puce $d415-18 | slot per-instr (ligne de base) |
| Octave-arp (×2) | bit2, `frame&1`, phase globale | — | ≈ `0x83` offset 12 (⚠ phase per-note) |
| Two-phase pitch | bit6 (attaque en frames/2) | — | ≈ wavetable (pas pitch à l'attaque) |
| Drum | `instrfx&1` (noise + slide freq-hi + gate-off) | — | ≈ wavetable (Noise→Tone) + ADSR |
| Skydive freq-add | `instrfx&2` (slide gaté sur longueur de note) | — | ≈ `0x82` portamento |
| Skydive-climb | incrément note (demi-tons up) | — | ≈ `0x82` / `0x83` |
| Wave-alt ctrl | v30 (toggle ctrl chaque frame) | — | ≈ wavetable (2 pas Tone alternés) |
| Wavetable **Loop** (`0x86` step) | — | — | ✓ nouveau pas d'engine |
| Filtre **balayé** (accu cutoff + bornes) | — | — | ✓ slot `$1cb0` |
| Override cutoff par-note | — | — | `0x87 lo hi` |
| Voice-select filtre global | — | — | `0x89` (piste 3) |
| Tie / legato hauteur | — | — | `0x88` (ne ré-arme pas la hauteur) |

`≈` = exprimable **musicalement** en v40, mais pas en transformation 1:1 registre-exact :
c'est un ré-authoring (écrire le bon programme wavetable / poser le bon marqueur), pas un
mapping automatique. Pour notre migration on ne ré-authore PAS — on garde drum/skydive
comme primitives du modèle-union ; le `≈` dit seulement que le côté-v40 n'est pas *aveugle*
à ces effets.

**Lecture.** Le ∩ (8 lignes) est ce que `fx_engine` calcule déjà — v40 le *nourrit*
autrement. La colonne v<40-seule (6 lignes : drum/skydive×2/octave-arp/two-phase/wave-alt)
n'a pas de **flag** v40, mais v40 sait toutes les exprimer — par `0x82`/`0x83` ou (drum,
two-phase, wave-alt) par le **moteur wavetable**, qui est précisément la généralisation de
ces flags câblés (`WAVETABLE_ENGINE_PLAN.md` : `VoiceProgram` *replaces arpeggio +
two_phase + wave_alt*). C'est l'argument « v40 = bonne forme cible » : son vocabulaire
per-note + wavetable est plus expressif que les vieux flags. La colonne v≥40-seule (5
lignes : Loop, filtre balayé, override/voice-select cutoff, tie) est le **vrai nouveau
vocabulaire**. ⚠️ Deux lignes du ∩ ont la même math mais une **sémantique de phase
différente** (arpège : global vs par-note ; cf. §10 `global_phase`) — c'est là que se
cachent les régressions, pas dans le calcul. Donc « migrer les anciens dans v40 » = les
poser dans le coin `per-instrument + global_phase` du modèle-union (en gardant drum/skydive
comme primitives), pas les faire avaler par le binding per-note.

## 8. Design decision — landing per-note effects in our model

v40's markers are per-note; our `RobEffects` binds effects to instruments. Options:

- **(A) Synthesised instrument variants** — one `InstrRobSid` per `(wave, marker-set)`.
  Reuses everything, but explodes into near-duplicate instruments and loses the per-note
  intent.
- **(B) Per-note cell effects — RECOMMENDED.** Map each marker to a tracker **cell effect**:
  `0x82`→portamento, `0x83`→arpeggio, `0x86`→vibrato, `0x88`→legato, `0x80`→the instrument
  column, `0x87/0x89`→a filter automation/`Device` param. The 16-byte record becomes one
  `InstrRobSid` per wave index; the per-note flavour rides on the cell via the DAW's
  generalised `TrackEffect`/lane system. Matches v40's intent, no instrument explosion.
- **(C) A v40-specific runtime** (`InstrV40` + driver) — 1:1 faithful but duplicates the
  whole effect chain; against the curation direction.

**Recommendation: (B)**, with (A) as a fallback only where a marker has no clean cell
mapping. (C) rejected.

## 9. Implementation plan (phased; nothing existing may regress)

Pre-req: Tier-1 wavetable engine + bit4/bit6 done.

1. **Detect + skeleton.** v40 detector (load `$1003`/`$1000`, the `$100c→$18ff` init
   trampoline, the `c9 85 d0 03` engine, the 16-byte-record shape). New `import/sid/v40/`
   module with a per-tune config. A v40 tune loads to an empty module first (no panic);
   v10–v30 stay byte-identical.
2. **Song/order/pattern parse.** Decode the 4 order-tracks (`0xFF`/`0xFD`) and patterns
   (`[markers][length][note]`, `0x60` end). Emit tracks 0–2 as the 3 voices; parse track 3
   as the filter lane (`0x89` + hold lengths; ignore the dead `0x80` slot). Verify
   note/timing vs the sidplay `--regdump` oracle (freq columns, early frames).
3. **Instrument records + wavetable.** Decode the 16-byte records into one `InstrRobSid` per
   wave index (waveform/ADSR/PW/vibrato/filter defaults + `VoiceProgram::Wavetable`, reusing
   the Tier-1 engine, now exercising `Loop`).
4. **Per-note markers → cell effects (§8B).** Map `0x80/0x82/0x83/0x86/0x88` to cell effects
   (arp: `ArpMode::ThreeNote` with unsigned-nibble + per-note-phase variant). Verify
   ctrl/PW columns vs the oracle.
5. **Filter.** Implement the swept-cutoff slots + the track-3 voice-select as a filter
   automation (a small model addition beyond static `Filter`). Verify the filter columns.
6. **Golden + oracle.** Add 1–2 v40 tunes; capture goldens; oracle-diff the active frames.
   v10–v30 + the supported corpus stay bit-identical throughout.

**Per-tune addressing.** The 5 shared-player tunes have the SAME code but data tables at
DIFFERENT addresses (filter not always `$1cb0`, freq not always `$1e34`). The importer
derives each tune's table addresses from the code's `LDA abs,Y` operands (the code is
shared, the operands carry the addresses) — the per-tune-config pattern `OneSid` already
uses, just auto-extracted. Config built from the 5 tunes (exclude Lakers).

**Effort.** Steps 1–3 are the bulk (a new parser); step 4 is the interesting design work
but reuses existing lanes; step 5 is small. The runtime — drivers, chip, wavetable engine,
effect primitives — is **reused, not rewritten**.

## 10. Model extensions v40 needs (small, beyond Tier 1 + bit4/bit6)

- **`ArpMode::ThreeNote`** must also cover **unsigned nibble offsets + per-note phase**
  (bit4 was signed + global phase). Generalise to
  `ThreeNote { off_a, off_b, signed, global_phase }`, or a distinct variant.
- **A swept-filter automation** (cutoff accumulator + bounds + per-note voice-select),
  beyond today's static `Filter` — a `FilterSlot`/`Device`-param addition.
- everything else (wavetable `Loop`, portamento, vibrato, PW sweep, two-phase, legato)
  is already covered by Tier 1 + bit4/bit6 + the existing effect set.
