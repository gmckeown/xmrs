# MOD variant support (amiga importer)

The `.mod` family is ~30 named variants. Most *load* already through the
generic channel-count tag dispatch (`tag_str_to_num_tracks`); the work
documented here is the per-tracker **decode fidelity** — the behaviours
that change how pattern bytes / sample headers are interpreted.

All facts below are observable properties of the file format
(cross-checked against OpenMPT's loader as a behavioural reference and
verified against `openmpt123 --render`), re-expressed independently — no
loader source was copied. Test modules live in `~/Music/_variant_tests/`.

## Implemented + oracle-verified

| Variant | Tags | Behaviour | Verification |
|---|---|---|---|
| **StarTrekker 8-voice** | `FLT8` `EXO8` | Pattern stored as two stacked 4-ch halves (ch 0-3 / 4-7); order ÷2; one extra stored pattern. Also fixed a **detection** bug — `validate_structure` sized FLT8 as 8-ch-flat and rejected real files. | `flt8_gidion.mod` env_corr 0.979 / spec 0.989 vs openmpt123 (was: rejected) |
| **Mod's Grave (.wow)** | `M.K.`→8ch | 669-conversion: M.K. tag but 8 channels. Detected by the size equation `1084 + Σsamplebytes + nPat·2048 == len`, gated on no-finetune / vol-64 / no-restart. | synthetic fixture |
| **NoiseTracker** | `N.T.` `M&K!` `FEST` | `Dxx` pattern break ignores its parameter (always row 0). | synthetic fixture |
| **StarTrekker (4 & 8)** | `FLT4/8` `EXO4/8` | `Exx` = assembly-macro trigger → dropped (not PT extended cmd); `Fxx` capped at 0x1F. | 3 corpus FLT4 match openmpt 0.90-0.97 |
| **His Master's Noise** | `M&K!` `FEST` | Finetune = `-(byte << 3)` through `i8` (not the signed nibble ×16). | `hmn_aase.mod` level −0.12 dB |
| **15-sample SoundTracker** | *(untagged)* | Version estimate (`StkVersion`) from sample sizes / disk-name / tempo byte / effect fingerprints + per-version effect remap: **Ultimate Soundtracker** cmd 1 = arpeggio, cmd 2 = pitch-bend, cmds 3-F = nothing; pre-2.0 `Dxy` = volume slide; `C`→`&0x7F`; `F`→`&0x0F`. **Loop start in bytes** (not words) with the **pre-loop attack dropped** (loop re-based to 0). **Tempo** from the restart byte (`BPM = 88672375 / (6100·(240−restart))`, 0x78 ⇒ 125). | detection matches openmpt exactly; `pretend` 0.159→0.929, `echoing` 0.938→0.986, `sleepwalk` 0.148→0.587 |
| **Amiga channel panning** | *all MOD* | Hardware L-R-R-L hard-pan at the reference 0x40/0xC0 separation (matches the S3M importer). Needs `pt()` `pan_reset_policy: Never` so a retrigger keeps the channel pan. | golden rebaked (mod-only); LRRL direction confirmed vs openmpt123 stereo |

Already loaded by the generic dispatch (no special decode needed):
`OCTA`/`OKTA`/`CD81`/`CD61` (Octalyser/Oktalyzer), `NSMS`/`LARD`/`PATT`/
`EXO4`, multichannel `xxCH`/`xxCN`, `TDZ1-3` (TakeTracker).

## Deliberately out of scope

* **StarTrekker AM synthesis** (`FLT4` with synthesized instruments).
  The AM instrument definitions (waveform + 6-point volume envelope +
  pitch-fall + vibrato, 31×120-byte blocks behind a 144-byte
  `ST1.x ModuleINFO` / `AudioSculpture10` header) live in a **companion
  `.nt` / `.as` file**, never inside the `.mod`. xmrs's importer entry
  point is `Module::load_mod(&[u8])` — a `no_std` byte-slice API with no
  filename or filesystem access — so it cannot locate or read a
  companion file. Delivering this needs a new companion-bytes API
  (`load_mod_with_synth(mod, nt)`); the modules we have carry no embedded
  AM data and no `.nt` is distributed for them, so it is also untestable
  today. Generation maps cleanly onto the existing `Sample` (tiny looped
  waveform) + instrument volume/pitch envelope + auto-vibrato model when
  that data becomes available.
* **VBlank vs CIA timing** for NoiseTracker/StarTrekker — the importer
  notes the flag but the player runs CIA-BPM timing.
* **HMN "Mupp" synth + mega-arp** (tremolo command reassigned) — same
  external-synth class as AM.
* Tier-4 container formats: Digital Tracker (`FA0x`), ProTracker 3.6 IFF
  (`PTDT`), Soundtracker 2.6 / IceTracker (`MTN`/`IT`), Inconexia delta.
