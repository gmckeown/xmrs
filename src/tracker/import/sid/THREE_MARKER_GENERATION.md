# The three-marker generation — scoping

Rob Hubbard wrote at least four player families. Three of them are readable here:

| family | reader | corpus (96 HVSC files) |
|---|---|---|
| fx-mask, "v10..v30" | `detect.rs` + `one_sid.rs` | 69 |
| last generation, eight markers | `g2.rs` | 6 |
| **three markers** | **none — this document** | **9** |
| no instrument table at all | none | 8 |
| individual failures | — | 4 |

This file scopes the third. Nothing is implemented; it exists so the work starts
from measurement rather than from a fresh reading.

## How they were found

`examples/sid_corpus_scan.rs` reports, per rejected file, the first rule
`SidLayout::why_not` could not satisfy. Nine files failed on one rule — "the
sub-song table is not a whole number of entries" — with the same numbers: a
gap of 18 bytes and **four order tracks**. Four tracks is the last generation's
shape (three voices plus the filter-automation track), not the fx-mask
generation's three.

Checking the marker dispatcher confirmed it. These images carry `CMP #$82` and
`CMP #$83` but **not** `CMP #$87`, which is exactly the case `G2Config::detect`
already names in a comment as "the earlier three-marker generation" and
deliberately excludes.

So the v10..v30 rejection was correct — these are not that family — and the
consistency check earned its keep by refusing them instead of importing noise.

## The family

Nine tunes, and seven of them share one signature profile exactly:

```
After_8  Kings_of_the_Beach_intro  Mr_Meaner  Off_the_Cuff
One_on_One_Jordan_vs_Bird  Powerplay_Hockey_USA_vs_USSR  Pygmies_Revenge
Rikky  Rock_Tells_the_Tale
```

Membership is decided by the dispatcher (`$80` and `$82` present, `$87` absent),
not by which rule rejected the file — Powerplay Hockey belongs here despite
failing on the pattern stream rather than the sub-song table.

`Gremlins.sid` fails the same rule as most of them and is **not** a member: no
marker dispatcher at all, and a different signature profile. It needs its own
look, as do the three other individual failures (Chicken Song, Devil's Galop,
I-Ball).

Against the last generation's six detection signatures:

| signature | last gen | three-marker |
|---|---|---|
| instruments `LDA instr,X : STA $D402,Y` | yes | **yes** |
| pattern pointers `LDA ptr,Y : STA $FA` | yes | **yes** |
| order pointers `LDA ord,X : STA ram,Y : INX` | yes | **yes** |
| tempo `LDA ctr : CMP reload` | yes | **yes** |
| filter slots `LDA slot+7,Y : AND #$F0` | yes | **not a separate table** |
| frequency table `LDA tab,Y : CLC : ADC porta,X` | yes | **reached differently** |
| release ramp `LDA #ad : STA $D405,Y : …` | yes | **no** |

Four of the seven carry over unchanged. Of the three that do not, only the
release ramp is genuinely absent. The frequency table is reached by a different
route, and the filter is **not missing at all** — it lives inside the instrument
record instead of in a table of its own. Both are set out below. Reading
"signature absent" as "feature absent" was wrong on two counts out of three,
which is worth remembering before drawing the same conclusion about the fourth
family.

Markers: `$80`, `$82`, `$83` — against the last generation's
`80 82 83 84 86 87 88 89`. Two of the eight (Kings of the Beach, One on One)
use only `$80` and `$82`.

## The dispatcher (Ghidra, Off_the_Cuff `$1104`, image base `$1000`)

```
110b: BEQ  $111a          ; end of the length run
110d: PLA
110e: STA  $164e,X        ; the length byte, kept whole
1111: AND  #$1F
1113: STA  $164b,X        ; …and its row count
1116: INY : JMP $1104
111a: PLA
111b: CMP  #$80 : BEQ $1129
111f: CMP  #$82 : BEQ $1133
1123: CMP  #$83 : BEQ $1146
1127: BNE  $1104
1129: INY : LDA ($fa),Y : STA $165a,X : INY : JMP $1104     ; $80 — one byte
1133: INY : LDA ($fa),Y : STA $167b,X : INY : LDA ($fa),Y … ; $82 — two bytes
1146: …                                                     ; $83
```

The entry structure is the last generation's: a length byte masked with `#$1F`,
markers ahead of the note, the stream read through a zero-page indirect. That is
what makes `g2.rs` the right home — a variant of its decoder, not a new one.

## The order lists, and the frequency table

The pattern-entry *framing* carries over — the length byte, the marker run, the
note byte — and Off the Cuff's pattern 0 decodes to the same single 32-row rest
as Lion Heart's. Two things do not: the **order lists**, and the **length of
`$83`**.

`$83` is one byte in the later player (an arpeggio) and two here (a vibrato
parameter and a start delay). That is the only marker whose *size* differs, which
makes it the only one that can desynchronise an entry rather than merely mean the
wrong thing. Off the Cuff carries thirteen; before `pattern` was made
generation-aware, each ate a byte too few and the leftover decoded as a note — the
tune came out with exactly thirteen phantom notes, and nothing in the output
looked wrong enough to notice. Pinned by
`g2::tests::three_marker_vibrato_marker_is_two_bytes`.

Ghidra, Off the Cuff `$10b7`:

```
      LDY order_index,X : LDA (ptr),Y
      BPL pattern            ; bit 7 clear → a pattern index
      CMP #$FF : BEQ restart ; end of list: index and counters back to 0
      CMP #$FE : BEQ …       ; a second marker (JSR $100F)
10c6: AND #$7F               ; otherwise the low seven bits are a parameter…
10c8: STA $16A0,X            ; …kept per voice
10cb: INC order_index,X      ; …and the walk continues to the next byte
```

That parameter is a **transpose**: `$16A0,X` is added to every note before the
lookup, and the sum indexes the frequency table (`$116d`):

```
116d: CLC : ADC $16A0,X : STA note,X : ASL : TAY : LDA $1580,Y
```

So an order list is a run of pattern indices with inline `0x80 | semitones`
transposes, closed by `0xFF`. Off the Cuff uses two values, `0x80` (none) and
`0x8C` (an octave); its second voice's list carries none at all. The last
generation spells the same idea as a separate `0xFD N` step.

**Done.** `order_list` branches on the generation, and the six last-generation
tunes come out bit-identical (the oracle gate did not move). Off the Cuff's first
track now reads `Transpose(12) Pattern(1) Pattern(1) Transpose(0) Pattern(2) …`
instead of the `Pattern(140)` it produced before — an index past the end of a
24-pattern tune. Pinned by `g2::tests::three_marker_order_lists`.

`$1580` is also the frequency table, and `detect` now finds it by the shape
above — the last generation's signature misses it because this player has no
per-frame portamento add on the lookup.

## The instrument record

Sixteen bytes, indexed by `$80`'s payload shifted left four times. Read by
cross-referencing every load whose operand lands in `[instruments,
instruments+16)` against what it feeds (Off the Cuff, `instruments = $16FF`):

| byte | consumer | meaning |
|---|---|---|
| +0 | `STA $D402,Y` | pulse width low |
| +1 | `STA $D403,Y` | pulse width high |
| +2 | control shadow (`$1663`, `$1654,X`) | waveform / control |
| +3 | `STA $D405,Y` | attack-decay |
| +4 | `STA $D406,Y` | sustain-release |
| +5 | tested with `BNE` twice | a flag |
| +6 | `STA $1665` | — |
| +7 | `STA $1687`, later `AND #$20` / `BIT` | the effect flags byte |
| +8 | `STA $F8` | voice-program pointer, low |
| +9 | `STA $F9` | voice-program pointer, high |
| +11 | `PHA` then read | — |
| +13 | `AND #$0F` and `LSR ×4`, both stored INTO CODE | two 4-bit parameters, patched into the player |
| +14 | `STA $D417` | filter resonance / routing |
| +15 | `ADC` into the cutoff accumulator | signed cutoff step |

The first five bytes are the same five every Rob Hubbard player opens with.

### The filter is here, not in a table

```
1412: LDA $1687   : AND #$20 : BEQ skip   ; flags bit 5 arms it
1419: LDA $169a,X : CLC : ADC $170e,Y     ; accumulator += instr+15
1420: STA $169a,X : STA $D416             ; …to the cutoff
1426: LDA $170d,Y : STA $D417             ; …and instr+14 to resonance
```

Resonance, a signed cutoff step, an accumulator, gated on bit 5 of a flags byte:
that is the same scheme as the fx-mask generation's `fx_v2` records (`+6`
resfilt, `+7` step, flags bit 5) and as Delta and Lightforce. Only the offsets
moved.

## The row gate is the same two-counter trick

Ghidra, Off_the_Cuff `$1074`:

```
1074: DEC  $166e        ; the twelve-step counter
1077: BPL  $1081
1079: LDA  #$0B         ; …reloads to 11, so it steals one frame in twelve
107b: STA  $166e
107e: JMP  $108c        ; …and the subdivision is NOT ticked on that frame
1081: DEC  $166c        ; the subdivision
1084: BPL  $108c
1086: LDA  $166d        ; …reloading from the tempo table
1089: STA  $166c
108c: …
1092: LDA  $166e : BEQ  ; the gate also wants the twelve-step counter non-zero
```

This is the same block, to the byte, as the last generation's row gate and as
the fx-mask generation's fractional-tempo counter (`SidLayout::tempo_ka`). One
device spans all three families, and `SidLayout::tempo_frac(R, Ka)` —
`(R+1)(Ka+1)/Ka` — describes all of them.

**Do not read `Ka` from the `LDA #$0B`.** The init self-modifies that immediate
from the tempo table's second byte, exactly as the last generation does:

```
32f3: … LDA $1840,X : STA $107A     ; $1840 = tempo_table+1, $107A = the operand
```

Off the Cuff's tempo bytes are `01 05`, so `R = 1` and `Ka = 5` — a cadence of
`12/5 = 2.4` frames per row, not the `24/11 = 2.18` the literal suggests.
`G2Config::row_ticks` already reads it the right way (`tempo_table + 1 +
subtune`) and needs no change for this generation.

`g2.rs` carries the same warning for the last player, where taking the literal
puts Lion Heart at 2.18 instead of its true `8/3` — and a note that the
difference was measured on the chip, not argued.

## What the three markers do

Read from Off the Cuff's dispatcher targets and from the code that consumes each
slot. Confidence is stated per marker — this is a first reading, not a spec.

### `$80` — select instrument (solid)

One payload byte, stored to `$165a,X`. Both places that read it shift it left
four times before using it as an index:

```
1198: LDA $165a,X : STX $1662 : ASL ASL ASL ASL : TAX : LDA …
1212: LDA $165a,X : ASL ASL ASL ASL : TAY : STY $1670 : LDA …
```

A ×16 index means 16-byte records, which is the instrument record size the last
generation uses as well. So this marker keeps its later meaning.

### `$82` — pitch slide (well supported)

Two payload bytes plus a decrement:

```
1133: INY : LDA ($fa),Y : STA $167b,X     ; delta high
      INY : LDA ($fa),Y : STA $1678,X     ; delta low
      INY : DEC $167e,X                   ; …and one off the duration counter
```

`$167e,X` gates a per-frame accumulate:

```
134c: LDA $167e,X : BEQ skip : CLC : LDA $1675,X : ADC $1678,X : …
```

An accumulator at `$1675`, a 16-bit delta at `$1678`/`$167b`, a duration at
`$167e` — a slide, run while the counter lasts.

### `$83` — set the period-interpolation vibrato (solid)

Two payload bytes: the parameter to `$1681,X`, a delay counter to `$1684,X`. The
consumer runs the delay down first, then takes the parameter apart:

```
1229: LDA $1681,X : BEQ skip      ; no parameter, no vibrato
122e: LDA $1684,X : BPL $1239     ; delay still positive…
1233: LDA $1681,X : JMP $1247     ; …else the vibrato is armed
1239: DEC $1684,X : JMP skip      ; count the delay down
…
1247: PHA
1248: AND #$78 : LSR LSR LSR : STA $1006,X   ; half-depth = (b & 0x78) >> 3
1250: PLA
1251: AND #$07 : STA $1664                   ; shift    =  b & 7
```

That decomposition — half-depth in bits 3..6, a right-shift count in bits 0..2 —
is the **period-interpolation vibrato**, the same law `one_sid.rs` documents from
Ghidra for Thrust (`play $09f1`) and Spellbound (`sb_play`), where it reads an
instrument record's byte +5. Here it arrives from the note stream instead of the
instrument, with a delay of its own.

So the mechanism spans at least four players: Thrust (v10), Spellbound (v15),
this generation, and the last one. It is also what `SidLayout::interp_vibrato`
detects, by the side effect that computing an interpolated period forces a write
to **both** frequency registers.

## What an oracle can and cannot settle here

`sidera`'s `sidplay` **does** capture these tunes: they are RSID files with no
`play` address, and it drives the installed interrupt handler once per frame.
Sixteen seconds of Off the Cuff, Rikky and After 8 all yield 800 frames of real
register activity. So step 3 below is unblocked.

What does **not** work is validating the tempo table by checking whether the
chip's note attacks land on the predicted rows. Three versions of that test were
built and all three were misleading until a control was added:

- with a ±1 frame tolerance, every cadence under 3 frames/row scores 100 %,
  because the tolerance is wider than the row spacing;
- with exact matching, the scores separate — but against the rate at which an
  attack lands on a row *by chance* (`1/N` for `N` frames per row), every
  candidate sits between ×1.1 and ×1.9 with no peak. Off the Cuff's detected
  speed scores ×1.7 while an implausible one scores ×1.9.

(The sweep also used the literal `Ka = 11`, so none of its cadences was the real
`12/5` — another reason not to read anything into those numbers.)

So the tempo table for this generation is **recovered but unconfirmed**. The
probe (`examples/sid_three_marker_probe.rs`) now prints the chance baseline
beside every score, so its own inconclusiveness is visible; do not read its
numbers without that column.

## What implementing it means

1. **Detection.** Relax `G2Config::detect`'s `CMP #$87` requirement to a variant
   flag, and make `filter_slots` / `freq_table` / `release_adsr` optional rather
   than required. Verify it recovers tables for all eight.
2. **Decoding — mostly done.** The pattern entries needed one fix: `$83`
   is two bytes here, and `pattern` now branches on the generation. The order lists are done (`order_list` branches on the
   generation). The instrument records are done too
   (`three_marker_instrument`) — the five shared bytes, the voice program, and
   the filter, with **everything unverified deliberately off** so a patch comes
   out incomplete rather than invented. `three_marker_instruments` asserts both
   halves of that: every selected instrument has an envelope, and no unverified
   effect is switched on.

   What remains is the song side: turning the decoded patterns and order lists
   into a `Module`, and wiring the three markers' effects onto the cells.

   A marker set of three instead of eight, all three now read out
   of the code (see above): `$80` selects an instrument, `$82` starts a pitch
   slide, `$83` arms the period-interpolation vibrato. Each was established from
   its own consumer, not by assuming the later generation's meaning for the same
   number — `$80` does keep its meaning, which is a coincidence worth noting and
   not a rule to lean on.
3. **Validation — the pieces are in place.** Off the Cuff is bundled at
   `songs/off_the_cuff.sid`, with a sixteen-second capture beside it at
   `xmrsplayer/tests/fixtures_sid_oracle/off_the_cuff_3m#0.csv.gz` (800 frames,
   made with `sidplay --seconds 16 --regdump … | gzip -9`, the same recipe as the
   other fixtures). `g2::tests::three_marker_tables` pins what detection reads
   from it today. What remains is to extend `g2_oracle_fidelity_golden` to
   compare whole register streams once there is something to compare — that
   comparison, not a cadence-fit statistic, is what discriminates.

   Off the Cuff shares its player build with Pygmies Revenge (identical table
   addresses), so a decoder proved on one is proved on two.

Do steps 1 and 3 before step 2: proving detection against the images, and having
an oracle to answer to, is what kept the v10..v30 work honest.

## Not this family

Eight further files carry no instrument-table signature at all — not even
`LDA instr,X : STA $D402,Y`, which holds on 15/15 of the fx-mask tunes and on
the last generation. They are a fourth family, unexamined:

```
Casio_Extended  Dont_Step_on_My_Wire  Era_of_Eidolon  IK_plus
Phantoms_of_the_Asteroid  Robs_Life  Task_Force  Up_up_and_Away
```
