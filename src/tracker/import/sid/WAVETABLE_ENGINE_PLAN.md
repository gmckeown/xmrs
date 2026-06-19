# Rob Hubbard wavetable command engine — analysis & plan

The per-frame command engine ("wavetable") in Rob Hubbard's players: a per-voice counter
indexes a per-instrument command table; each frame ONE command sets the voice's waveform +
pitch. First found via ace_2's fxmask **bit7** (Ghidra `ace2_e000.bin` play `$e03f`), later
shown (below) to be a **standard feature of ~35 of Hubbard's 96 SIDs**. This doc is the
complete analysis + the implementation plan; ace_2 is the entry point, not the whole story.
Cross-checked vs the sidplay `--regdump` oracle.

## 0. Complete command vocabulary — VERIFIED across all 35 engine tunes

Scanned every Hubbard SID for the engine and enumerated every `CMP #$8x` marker near it.
The result is exhaustive — **two marker sets and nothing else**: `{0x85}` (29 tunes) and
`{0x85, 0x86}` (6 tunes). So the engine has exactly **four** command kinds:

| command | byte | action | advance |
|---------|------|--------|---------|
| **Tone**    | `0x00–0x7f` | ctrl = byte (waveform, no noise bit); **add a signed 16-bit step to the RUNNING freq** | +3 |
| **Noise**   | `0x80–0xff` except `85`/`86` | ctrl = byte (noise/test waveform); freq-hi = next byte (absolute) | +2 |
| **Sustain** | `0x85` | hold last waveform; do NOT advance; vibrato keeps moving the pitch | 0 |
| **Loop**    | `0x86 N` | jump to command **N** (reset index, skip N commands) — a loop point | → N |

Key semantics (all Ghidra-verified):
- **Freq is RELATIVE**: note-setup seeds `d400/d401` from the struck note; `Tone` steps
  accumulate on top. The wavetable slides *from* the note. `Noise` sets freq-hi absolutely.
- **Slide sign is per-player-variant**: the 29 minimal-engine tunes SUBTRACT (ace_2, Ghidra
  `$e03f`); the 6 rich tunes ADD (Lion_Heart, Ghidra `$1699`). The importer normalises both
  into the signed `pitch_slide`.
- **`Loop` (0x86) is the only thing the rich tier adds** — there is NO third tier, no marker
  beyond `0x86`. ace_2's minimal engine has only `Sustain` (it freezes; it cannot loop).
- **Termination & the "no loop point" case.** Every table ends with a terminal command —
  either `Sustain` (freeze) or `Loop` (jump back). `Sustain` is the DEFAULT and by far the
  common one: I walked every wave-command table of all 6 rich tunes — Lion_Heart uses **zero**
  loops (16/16 tables `Sustain`); the others use `Loop` in only **1–2** instruments each
  (Sun_Never_Shines 1, Radio_ACE 1, Pacific_Coast 2, Go_Go_Dash 2). So "the loop point does
  not exist" is the NORMAL case (the table freezes via `Sustain`) — there is no failure mode;
  `Loop` is just an optional alternative terminator. The engine does NOT bound-check the
  `0x86 N` target (it trusts the hand-authored data, where N is always a valid earlier
  command). **Importer robustness:** decode each table up to its terminal (`0x85`/`0x86`);
  guard/clamp an out-of-range `Loop` target (only reachable on corrupt input).

## 1. What is actually wrong with ace_2 (song 0)

| voice | symptom | cause | category |
|-------|---------|-------|----------|
| v0    | ctrl 9% (`00` vs `08`) | idle voice TEST-parked, both silent | **false alarm** (96.7% on active frames) |
| v1    | ctrl 45% (`10` static), lo 0% | bit7 command engine not run → static triangle where oracle alternates pulse/noise + self-pitched | **real audible defect** |
| v2    | pwl/pwh garbage, lo 2.5% | same bit7 engine (self-pitched) | **real audible defect** |
| —     | FChi 0% (`12,15,18,1b` sweep) | filter cutoff sweep not driven | **separate sub-bug** |
| all   | residual drift | `tempo_frac (8,3)` fractional tempo | **measurement ceiling** (unfixable vs 50 Hz oracle) |

Instruments with bit7 set (`instr_byte+7 & 0x80`): **1, 2, 4, 6 — each `0x80` EXACTLY**
(only bit7, no other fx bit). 4 of 10. Voices playing these run the command engine.

> Verified (the byte+7 fxmask of all 10, via the importer's own indexing
> `data[126 + addr - load]`): `20 80 80 44 80 00 80 04 04 04`. Notably **no instrument
> sets bit0 or bit3**, and ace_2's `play` never tests bit0/bit3 — so the drum (bit0) and
> octave-arp (bit3) of the sanxion/lightforce "Rich" layout **do not exist in ace_2**.
> ace_2 is a *distinct* fxmask variant. The importer audit confirms `drum×0, arpOct×0,
> 2phase×4` (bit2 on instr 3/7/8/9), `filter×1` (bit5 on instr 0) — i.e. we are NOT
> mis-decoding ace_2 today; the only missing piece is the bit7 engine.

## 2. The bit7 engine (Ghidra `$e03f`, else-branch of `fxmask&0x40 || fxmask>=0`)

Per-voice **command pointer** built from two parallel instrument tables:
`ptr = filter_table[instr*8] (lo) | DAT_e625[instr*8] (hi)`. Per-voice **counter**
`DAT_ebc7[voice]`, **reset to 0 on note-on**. Each frame reads `cmd = table[counter]`:

- `cmd >= 0` (bit7 clear, the "pitched" command): write **ctrl = cmd** (a waveform
  byte without the noise bit, e.g. `0x41` pulse+gate / `0x11` tri+gate); then
  **subtract a 16-bit value** `(table[+1], table[+2])` from the voice freq register
  (a downward pitch slide step); **advance +3**.
- `cmd == 0x85` (`-0x7b`): **hold** — do nothing, do NOT advance (sustains the last
  state every frame; this is the table terminator / sustain marker).
- other `cmd < 0`: write **ctrl = cmd** (a waveform byte WITH the noise/test high bit,
  e.g. `0x81` noise+gate, `0x80` noise); set **freq-hi = table[+1]** (absolute);
  **advance +2**.

The engine fully owns the voice's `d404` (ctrl) and `d400/d401` (freq). It does NOT play
the note's pitch as a fixed tone — but the freq is **not absolute**: note-setup seeds
`d400/d401` from the struck note, and the `Tone` commands ADD/SUB their 16-bit step to that
RUNNING freq. So the wavetable is a pitch program *relative to* the played note (it slides
from it and accumulates), while the `Noise` commands set the freq-hi absolutely. That is why
our v1 plays the note's static triangle while the oracle plays a hand-programmed waveform +
relative-pitch sequence.

Musically this is a **wavetable / per-frame command sequence** — a known Rob Hubbard
device (a tiny hand-coded program per instrument). It is NOT an opaque quirk.

⚠️ **Importer gotcha:** ace_2's command-table POINTER is read from `filter_table[i*8]` (lo)
and `DAT_e625[i*8]` (hi) — i.e. ace_2's `filter_table` (`0xE624`) is **dual-purpose**: for
a bit7 instrument those 2 bytes are the command pointer, NOT a filter record. The importer
must read the filter record ONLY for non-bit7 instruments, and the command pointer ONLY for
bit7 ones, from the same table.

## 3. Generality — VERIFIED across every SID at disposal

The 14 supported tunes ARE every `.sid` on disk (`~/Documents/robrs/sid/` is the same
set). Dumped `byte+7` of every instrument of all 14, then checked each replayer that has
bit7 set anywhere:

| tune | bit7 instruments | what bit7 means there (Ghidra) |
|------|------------------|-------------------------------|
| **ace_2** | 1,2,4,6 (`0x80`) | **the command engine** (`play $e03f` else-branch) |
| delta | 0–3,20 (`0xa0`) | bit5+**bit7 = slow filter-base ramp** (`play $bde4`), NOT a command table |
| thing_on_a_spring, thrust | (`0xf0`/`0xf5`…) | v10 replayers test only bits 0–3 → **bit7 vestigial** |
| international_karate | 3,4,10 (`0xc5`) | `play $ae0c` tests bits 0/2/3 only → **bit7 vestigial** |
| spellbound | 1,3 (`0xc5`) | `sb_play $e012` tests bits 0/1/2/3 only → **bit7 vestigial** |
| commando, monty, crazy_comets, zoids, last_v8, sanxion×2, lightforce, human_race | none | — |

**Conclusion (within the 14 supported tunes): the command engine is in `ace_2` only.**

### ⚠️ REVISED after scanning Rob Hubbard's full catalog (2026-06-19)

Downloaded all **96** Rob Hubbard SIDs from HVSC (`~/Documents/robrs/sid/hvsc/`) and
byte-scanned for the engine signature (`c9 85 d0 03` = the `CMP #$85` sustain marker, which
in ace_2 occurs exactly once, inside the engine). Result: **35 of 96 tunes carry the
engine** — it is a STANDARD feature of Rob Hubbard's later player, not an ace_2 one-off.
Our 14-tune corpus simply happened to include only one user of it.

- **~30 are byte-identical to ace_2's engine** (relocated, different zero-page pointer):
  Auf_Wiedersehen_Monty, Nemesis_the_Warlock, Bangkok_Knights, Saboteur_II, Thundercats,
  Star_Paws, Pandora, Shockway_Rider, Powerplay_Hockey, Mega_Apocalypse, … Same command
  vocabulary exactly (positive = ctrl + 16-bit pitch step, `0x85` = sustain, other negative
  = ctrl + freq-hi).
- **A RICHER engine variant exists, inside a DIFFERENT player** — the `$100c`-loaded
  cluster (Lion_Heart, Sun_Never_Shines, Lakers_vs_Celtics, Pacific_Coast, Radio_ACE,
  Go_Go_Dash) + One_on_One. Decompiled the whole Lion_Heart player (`play $101a` +
  `$1699`): the wavetable engine is the SAME (still gated per-instrument, by a flag bit
  `DAT_1b52 & 1`, not always-on) PLUS the `0x86` Loop command. **BUT it is embedded in a
  wholesale-different, later player**: 16-byte instrument records (vs 8), an
  instrument-DEFINITION command sub-language (markers `0x80/0x82–0x89` build the patch:
  filter, ADSR, vibrato, …), and integrated filter `$d415/$d416/$d417`. So the *engine* is
  reusable, but importing these 6 tunes means writing **a separate `$100c`-player importer**
  — NOT part of this plan. (IK_plus is a minor codegen variant of the standard engine: it
  writes to a RAM buffer instead of `$d404`. One_on_One pushes ctrl with `PHA`. Both keep
  the `{0x85}` vocabulary — Tier-1 sub-variants.)

**So ace_2's engine is the MINIMAL instance of a ~35-tune Rob Hubbard feature.** This
overturns the "mono-tune" cost/benefit: implementing it is reusable across the whole later
catalog, and the *complete* form should be the `0x86`-aware (Lion_Heart) superset, with
ace_2 as the linear subset. `WaveStep` should therefore also model a `Jump`/`Loop` step.
(Still: none of these 35 tunes are in the supported corpus today, so the *immediate* payoff
remains ace_2 — but the feature is no longer throwaway; it is a general RH-player capability.)

**Scope honesty — engine vs player.** The *engine* (these 4 commands) is universal and now
fully nailed. But each tune is imported through its **player front-end** (note/pattern/
instrument format), and those VARY: ace_2's standard player (8-byte instruments, fxmask
bit7) is one; the `$100c` player is a different one; a few minimal-engine tunes (e.g.
One_on_One, an RSID at `$0b5c`) are yet others. So "reusable across ~35 tunes" means the
*engine code* is reusable — NOT that all 35 import for free. Each tune's import is gated by
its player front-end. This plan delivers: (a) the complete engine model, (b) the ace_2
import (Tier 1). Other tunes come when/if their player is added.

## 4. Where it lives in the curated model — DECISION (revised)

Two findings drive the model:

1. The command stream IS musical — a per-note **wavetable** (a hand-coded
   waveform+pitch program), a known Rob Hubbard device. So: structured steps named by
   **musical intent**, never a raw `Vec<u8>`.
2. The §3/impact analysis showed bit7 is the **`else` arm** of the replayer's
   `if (normal fx) … else (command engine)`. It does NOT add to the arpeggio /
   two-phase / wave-alt effects — it **replaces** them. So the model must make
   "Rich effects" and "wavetable" **mutually exclusive by construction**, not an
   `Option` next to the effects that needs runtime suppression logic.

### 4a. The steps — musical intent

```rust
/// One frame of a [`Wavetable`]: a hand-coded waveform + pitch move. Mirrors the
/// bit7 command bytecode (Ghidra `play $e03f`, else-branch).
pub enum WaveStep {
    /// Melodic frame: play `waveform` (a $D404 control value — gate/sync/ring bits
    /// included) and add `pitch_slide` (SIGNED freq-register units) to the RUNNING
    /// frequency, which starts at the struck note's freq. So the wavetable slides
    /// *relative to* the played note and accumulates across frames. The sign is a
    /// per-replayer-variant convention: ace_2 subtracts (slide encoded negative),
    /// the `$100c` cluster adds — the importer normalises both to a signed step.
    /// (Replayer: a non-negative command byte, then a 16-bit add/sub; advance +3.)
    Tone { waveform: u8, pitch_slide: i16 },
    /// Percussive frame: play a noise/test `waveform` (e.g. `0x81` noise+gate) and
    /// set the pitch high byte to `pitch_hi` (absolute). (Replayer: a negative
    /// command byte ≠ `0x85`/`0x86`; advance +2.)
    Noise { waveform: u8, pitch_hi: u8 },
    /// Sustain the previous waveform; let the always-on modulators (vibrato) keep
    /// moving the pitch. (Replayer: the `0x85` marker — re-read, not advanced.)
    Sustain,
    /// Jump to step `to_step` and continue — the wavetable loop point. Only the
    /// RICH variant (`$100c` cluster: Lion_Heart, Sun_Never_Shines, …; Ghidra
    /// Lion_Heart `$1699`, marker `0x86` = reset index, skip N commands). ace_2's
    /// minimal engine has no loop (it relies on `Sustain` to freeze) — so this
    /// variant is unused for the Tier-1 ace_2 import and only needed for Tier 2.
    Loop { to_step: u8 },
}

/// A hand-coded wavetable, restarted from step 0 on every note-on.
pub struct Wavetable {
    pub steps: Vec<WaveStep>,
}
```

### 4b. The conditional — an enum, not a flag

`RobEffects` splits into **always-on modulators** (run every frame, layered) and the
**per-frame program** that owns waveform+pitch (exactly one kind):

```rust
/// What drives a voice's waveform + pitch each frame, ON TOP of the note and the
/// always-on modulators. The two arms are mutually exclusive *by construction* —
/// mirroring the replayer's `if (normal) … else (command engine)` — so an
/// instrument can never accidentally run an arpeggio AND a wavetable at once.
#[derive(Default)]
pub enum VoiceProgram {
    /// The standard Rich per-frame effects (fxmask bits 1–3/6).
    #[default]
    Effects(RichEffects),
    /// A hand-coded wavetable that fully owns waveform + pitch (fxmask bit 7).
    Wavetable(Wavetable),
}

/// The mutually-exclusive Rich effect set (everything bit7 would replace).
#[derive(Default)]
pub struct RichEffects {
    pub arpeggio: ArpMode,
    pub two_phase: TwoPhase,
    pub wave_alt: WaveAlt,
}

pub struct RobEffects {
    // Always-on modulators — layered every frame, orthogonal to bit7:
    pub vibrato: Vibrato,
    pub interp: InterpVibrato,
    pub pulse_sweep: PulseSweep,
    pub drum: Drum,
    pub skydive: Skydive,     // config-driven, not a Rich fxmask bit
    pub filter: Filter,
    // The exclusive per-frame waveform+pitch authority:
    pub program: VoiceProgram,   // ← replaces the flat arpeggio + two_phase + wave_alt
    pub hard_cut_release: bool,
    pub compat: ReplayerCompat,
}
```

Why this is the right shape:
- **Invalid states unrepresentable.** You cannot express "octave arp + wavetable" — the
  exclusivity that lives in the replayer's `if/else` is encoded in the type, so no future
  importer change can accidentally layer Rich effects under a wavetable. (ace_2's own bit7
  instruments are clean `0x80`, so there is nothing to suppress *for ace_2*; the enum
  guards the general case, and documents intent.)
- **No suppression logic in the engine.** `modulate()` matches `program`: `Effects` runs
  the existing flat chain unchanged; `Wavetable` runs the interpreter. The flat chain
  stays flat; the bytecode VM is quarantined in its own arm.
- **`SidFx` stays flat** (engine contract): the bridge passes the active arm; the
  per-voice **counter** is runtime driver state (like the PW accumulator), never on the
  instrument.
- Default `program = Effects(RichEffects::default())` ⇒ every existing tune is bit-identical.

Cost, stated plainly: `arpeggio` / `two_phase` / `wave_alt` move OFF `RobEffects` into
`RichEffects` inside the enum. The importer currently sets these at three separate points
in the fxmask decode, so it must **accumulate a local `RichEffects`** and assign the
variant once at the end (`Wavetable` if bit7, else `Effects(rich)`). The bridge and any
reader switch from `re.arpeggio` to a match on `re.program`. Mechanical but not free.

## 5. Plan (phased, each step verified, nothing else may regress)

1. **Model + restructure (bit-identical refactor)** — add `WaveStep`/`Wavetable`/
   `RichEffects`/`VoiceProgram`; move `arpeggio`/`two_phase`/`wave_alt` into
   `RichEffects`; restructure the importer to accumulate a local `RichEffects` and emit
   `VoiceProgram::Effects(rich)`. No wavetable populated yet ⇒ golden + every tune
   bit-identical (this validates the refactor in isolation, before any new behaviour).
2. **Import the wavetable** — for ace_2 instruments with `fxmask & 0x80` (instr 1/2/4/6),
   walk the table from the per-instrument pointer pair (`filter_table[i*8]` lo,
   `DAT_e625[i*8]` hi), decode each command into a `WaveStep` until `Sustain` (bound the
   walk), and emit `VoiceProgram::Wavetable`. Gated on bit7 ⇒ only ace_2 is touched. These
   four instruments are `0x80` exactly, so the `Effects` arm they leave behind is empty
   anyway — no other bits to drop.
3. **Engine** — a quarantined `generators/sid/wave_sequence.rs` interpreter + a per-voice
   counter (reset on note-on) in `FxState`. `modulate()` matches `program`; on a
   `Wavetable` it runs the step and owns ctrl+freq, EXCEPT `Sustain` yields the pitch to
   vibrato (keeps last waveform). Verify vs the regdump on **early frames** (before the
   `tempo_frac (8,3)` drift dominates) and by ear (alternation present, no static triangle).
4. **Filter sub-bug** (optional, separate) — drive ace_2's cutoff sweep (FChi). Only if
   cheap; independent of bit7.
5. **Accept the ceiling** — `tempo_frac (8,3)` caps the full-window register match;
   success = audible correctness + early-frame match, not 100%.

Steps 1–5 are **Tier 1** — ace_2's minimal engine (`Tone`/`Noise`/`Sustain`, slide
subtract). It needs NO `Loop` step.

### 5b. Tier 2 — the `$100c` player (a SEPARATE import project, not an engine tweak)

The 6 rich tunes are NOT "ace_2 + one more command" — they run a **wholesale-different,
later Hubbard player** (`play $101a`: 16-byte instruments, an instrument-definition command
sub-language, integrated filter). In our empirical version scheme (v10→v30) this is the next
generation — call it **"v40"**: chronologically Hubbard's late / EA-era games (Lakers vs
Celtics, Pacific Coast, …). But it is really a new player LINEAGE, not a v30 increment; the
loop (`0x86`) is just one of its features — the 16-byte instrument format + instrument-def
sub-language is the larger change. The wavetable ENGINE inside it is the same plus `0x86`
Loop, so the `WaveStep`/`Wavetable` model already covers it — but actually importing these
tunes needs a new player front-end (pattern/note format, instrument decode), which is a
project of its own scale.

So Tier 2 = "when we decide to support the `$100c` player": reuse the `Wavetable` engine
(already `Loop`-aware), write the new instrument/pattern decoder around it. The engine
pieces already settled here:
- `0x86 N` → `WaveStep::Loop { to_step: N }` (Ghidra Lion_Heart `$1699` + `$101a`: reset the
  command index, skip N commands → land on command N = a loop point). Confirmed identical in
  Sun_Never_Shines (byte-identical engine); the other 4 cluster members share the player.
- slide sign ADD (vs ace_2's SUB) — already normalised by the signed `pitch_slide`.

**Out of scope here (explicitly):** the `$100c` player's note/instrument layer — the
instrument-definition sub-language and the filter sequencing. That is the new front-end of
Tier 2, NOT the wave engine.

## 6. Recommendation — GO (contained), with eyes open

Tier 1 fixes the **only remaining real audible defect** in the supported SID set (everything
else is a phase/cadence artifact = perfectionism). Modeled as a structured wavetable it is
curation-consistent and not a hack. Caveats, stated honestly:

- Tier 1 makes **ace_2** sound right; the metric stays **drift-capped** by `tempo_frac`
  (judge by ear + early frames);
- it adds a small **interpreter subsystem** — but it is **NOT throwaway**: the engine is a
  ~35-tune Rob Hubbard feature, so the code is reusable across his later catalog (Tier 2
  extends it to the rich variant). This is the key update vs the original "mono-tune" framing.

If the goal is "ace_2 should *sound* right" (and to lay reusable RH-player groundwork), GO
on Tier 1. Tier 2 is opt-in, only when those ~6 richer tunes are actually imported. The
fxmask catalog (incl. bit4/bit6 = 3-note arp + two-phase-pitch) is what makes RobEffects the
complete RH superset; the wavetable engine is the last and largest piece.
