# Plan — lecture fidèle de **tout** `~/Music/dw` (`.dw`)

> Plan d'exécution multi-sessions, autoportant. À lire avec [`FORMAT.md`](FORMAT.md)
> (spec + sémantique replayer + conventions). Rédigé 2026-05-29.
> **Objectif** : les 120 `.dw` chargent sans erreur **et** jouent avec une
> modélisation fidèle (oracle Ghidra/`.cs` si nécessaire), en un minimum de
> sessions, **sans question** à l'utilisateur sauf l'écoute finale.

---

## 0. État de départ (mesuré 2026-05-29)

- **120** fichiers dans `~/Music/dw/`. **116 chargent**, **4 rejetés** proprement
  en `dw_no_song` (moteurs SFX sans table PCM) : `anarchy sfx`, `blood money sfx`,
  `spellbound sfx`, `feud fake` (placeholder 2 Ko). ⇒ axe **chargement = atteint**
  (un module chargé n'a pas besoin de chant ; cf. FORMAT.md §10).
- **Fidélité = non vérifiée systématiquement.** Seuls xenon2 (title) — et au son,
  par l'utilisateur — ont servi d'oracle. Les approximations connues (FORMAT.md §8) :
  vibrato en espace demi-ton (≠ période), GlobalVolume/Fade cuit au row (pas de lane
  continue), enveloppe à résolution row, arpège tronqué à 3 offsets, `SetSpeed`
  mid-song non re-quantifié, half-volume / 3-canaux / square-waveform non couverts,
  `Effect9` restart non nul non modélisé.
- **Le slide est désormais exact** (rampe linéaire période, délai `counter`, apply
  chaque tick via quirk `pitch_slide_ticks_at_row_zero`, borné par `Clear`).

### Avancement (2026-05-29, Session A / Tier 1 — fait)
- **Tier 1 robustesse livré et vert** : `xmrsplayer/tests/dw_robustness.rs`
  (`render_all_dw_modules_robustness`). Rend les **116** modules via `XmrsPlayer`
  (song 0, `max_loop_count(1)`, cap 60 s @ 24 kHz), 0 panic. Invariant dur : tout
  canal portant une note **à vélocité > 0** doit rendre du son ⇒ **0 échec**.
  Avertissement doux (sans échec) : canal dont **toutes** les notes sont à
  vélocité-0 au row et qui rend muet ⇒ lacune volume-lane (backlog #2/#3).
- **Subtilité corrigée dans le harnais** : le player ne joue que **song 0** ; le
  recensement des canaux à notes doit être borné à song 0 (sinon faux positifs sur
  bmx/eye/back-to-the-future-2 dont les canaux « muets » ne portent des notes que
  dans d'autres sous-chants).
- **Seul résiduel signalé** : `emlyn hughes int soccer.dw` ch0 — 3024 notes,
  instrument #0 fort (max|127|, 120 touches mappées) mais **toutes** à vélocité 0
  (volume baké à 0 au row ; `fx=[Volume{value:0,tick:0}]`). ch2 (même profil
  vélocité-0) est relevé par sa lane et reste audible ; ch0 non. ⇒ cible directe de
  la modélisation **GlobalVolume/volume-lane continue** (Session B, backlog #2/#3).
- **Outil de triage ajouté** : `xmrs/examples/dw_channel_energy.rs` (par canal de
  song 0 : nb notes, nb vélocité>0, instruments + taille/`max|pcm|`/touches mappées,
  premières note-cells). A servi à disculper les 4 « échecs » initiaux.

### Avancement (2026-05-29, Session A / Tier 2 — oracle Ghidra fait & validé)
- **Oracle d'émulation 68k livré et reproductible** :
  `dw/oracle/DwPaulaOracle.java` (+ doc `dw/oracle/README.md`). Émule le replayer
  réel du `.dw` dans le p-code de Ghidra : `init_main` 1×, puis N× `play_tick`,
  avec un `MemoryAccessFilter` capturant **toutes** les écritures Paula
  (DMACON `0xdff096`, AUDxLC/LEN/PER/VOL `0xdff0a0+ch*0x10`). Reconstruit l'état
  Paula continu (gate DMA + période + volume clampé 0..64) snapshoté par tick.
  Sortie CSV (schéma dans le README). **Le script-fichier reproduit la trace
  inline à l'identique.**
- **Golden de référence** : `dw/oracle/xenon2_title.csv` (1500 ticks de
  `xenon2 (title).dw`, init=`0x66`/play=`0x1b6`).
- **Findings xenon2 validés** : intro qui monte les voix (DMA `0x201`@t1 →
  `0x203`@t78 → `0x20f`@t104) ; périodes gated saines (ch0 179–455 ≈ 7,8–19,8 kHz,
  ch1 157–671, ch2/3 179–506) ; bascules DMACON 1-tick = restarts de note ; vol
  écrit pouvant dépasser 64 (clamp Paula) ; `0xFC78`/`vol103` = scratch pré-gate
  (DMA off) correctement ignoré via le bit de gate.
- **Pré-requis env capturés** (ont coûté du temps au 1er run, voir README) :
  `GHIDRA_MCP_ALLOW_SCRIPTS=1` dans l'env **du process Ghidra** ; `~/ghidra_scripts`
  doit être un *source bundle* (sinon `GhidraPlaceholderBundle cast` → redémarrer
  Ghidra) ; SP 68k = `SP` (pas `A7`).
- **Reste pour clôturer Tier 2** : (a) observateur per-tick côté player
  (`xmrsplayer/src/audio_observer.rs`) exposant période/volume/gate par canal ;
  (b) alignement d'unités (player `Period` → période Amiga ; volume canal → 0..64 ;
  voix vivante → gate) ; (c) diff trame-à-trame contre les goldens → `BASELINE.md`.

### Avancement (2026-05-29, Session A / Tier 2 — les 9 goldens capturés)
- **Tous les représentants de cluster ont leur golden** dans `dw/oracle/*.csv` :
  C1 `xenon2_title`, C2 `bad_company`, C3 `bubble_bobble`, C4 `grimblood`,
  C5 `tetris`, C6 `qball`, C7 `leviathan_deabs`, C8 `the_empire_strikes_back`,
  + demi-volume `obliterator_title`. 1500 ticks chacun. Table des points d'entrée
  (init/play/A3/f0) + méthode de recherche dans `dw/oracle/README.md`.
- **Script généralisé & blindé** (`DwPaulaOracle.java`) : args
  `init play [ticks] [out] [a3] [f0] [maxSteps]`. `a3` pose une base A3 (bubble
  bobble = `0xfffffbbe`). `f0=-` désactive la détection de Stop (seul xenon2 a un
  vrai flag ; ailleurs ça tronquait à tort). `maxSteps>0` = **mode pas-à-pas
  borné** : la boucle est pilotée par le script, donc *ne peut plus figer Ghidra*
  (lève une erreur au-delà du plafond). À utiliser pour sonder toute entrée non
  validée (les vieux players C6/C7/C8 surtout).
- **Pièges capturés (ont coûté 2 gels de Ghidra à 100 % CPU)** : `emu.run()` ne
  consulte pas le `monitor` → un watchdog temps-réel ne sauve rien, seul le mode
  pas-à-pas borne le risque ; vieux players à prologue `movem` → l'entrée est
  l'adresse du `movem`, pas le label Ghidra (qball `0xd0` ≠ `0xd4`) ; l'arg
  `program` de `run_ghidra_script` est ignoré → `switch_program` juste avant et
  vérifier la ligne `Program:`. Tout est dans le README + la mémoire.
- **Reste (inchangé)** : observateur per-tick côté player + diff trame-à-trame
  contre ces 9 goldens → `BASELINE.md` (une section par cluster).
  Les adresses init/play sont **spécifiques au binaire** : pour les autres clusters,
  les retrouver via la boucle 4-canaux écrivant `0xdff0a0+ch*0x10` (cf. README).

### Avancement (2026-05-29, Session A / Tier 2 — CLÔTURÉ, premier BASELINE)
- **Trace player** : `xmrsplayer/examples/dw_oracle_trace.rs` rend chaque `.dw`
  un tick replayer à la fois (`XmrsPlayer::advance_trace_tick`, sans rendu audio)
  et sort le **même schéma CSV** que les goldens. API lib ajoutée = *read-only,
  minimale* : `Channel::snapshot()`/`ChannelSnapshot{period(=AUDxPER), volume(0..64),
  gate(=voix vivante)}`, `Voices::channel_snapshots()`, `XmrsPlayer::channel_snapshots()`.
- **Généralisé en API observateur** (choix utilisateur) : `RowContext`/`TickContext`
  portent désormais `channels: &[ChannelSnapshot]` — donnée runtime utile aux
  scopes/VU/indicateurs d'activité, pas seulement au diff DW. Exporté au prelude.
  Tous les tests passent (31+8+1+doctests).
- **Diff** : `oracle/diff_baseline.py --markdown` aligne tick-à-tick et calcule
  par canal : accord de gate ; sur ticks doublement gated, période/volume
  (exact %, **médiane Δ signée** = offset systématique, MAE = dérive, max).
- **Résultats** (`BASELINE.md`, scoreboard vivant) :
  - **Gate 82–100 % partout** → la structure (quand un canal sonne) est juste.
  - **Période quasi-alignée** (medΔ ±2..±7) : C1, C4, C5 (medΔ +0 !), C6, obliterator
    — résidu = arrondi de table + **dérive vibrato/slide** (MAE 16–65).
  - **Mismatch period-space** (medΔ en centaines) : C2, C3, C7, C8 — mapping de
    période différent (≠ vieux/nouveau : C6 vieux est quasi-exact). C3 bubble bobble
    le pire (gate ch0 50 %, dispatch `jmp (0,A3,A2)` à revoir).
  - **Volume** : offsets *constants* corrigeables (tetris exactement +8, empire 40/15) ;
    déjà 100 % exact sur qball ch0/ch3, obliterator ch3.
- **Suite** = backlog de modélisation §6 (vibrato period-space, GlobalVolume,
  envelope frame-res, SetSpeed, demi-volume). Re-lancer le diff après chaque
  changement, mettre à jour la table de `BASELINE.md`.

### Avancement (2026-05-29, Session B / modélisation volume — 3 gains)
Backlog §6 attaqué côté volume, chaque correctif trouvé par **décomp Ghidra du
`PlayTick`** du cluster (le `.cs` est incomplet pour tous) puis vérifié au
diff. Détail complet + changelog dans `BASELINE.md`.
- **C5 tetris volume → 100 % exact** (4 canaux). `AUDxVOL = sampleInfo[+0xC] −
  DAT_41a`. L'importeur figeait `DwSample.volume=64` ; lit désormais le mot
  volume par échantillon à l'octet +12 (gate `enable_sample_transpose`,
  stride 16). Le 56/56/56/64 par canal = ces mots d'échantillon.
- **C6 qball volume → 99–100 % exact** (était 0 % sur ch1/ch2). Les vieux
  players chargent une **table de volume par canal** (`channelVolumes[]`) et
  l'écrivent BRUTE dans `AUDxVOL` à chaque note. qball @`0x2d6`=`[64,40,40,64]`,
  repérée via `41 EB <d16> E3 4F`. Ajoutés `DwLayout::channel_volume_offset` +
  `DwModule::channel_volumes`, émis en `TrackEffect::Volume` par note (variant
  `Old`). Zéro régression (gate strict OK, 15 tests dw + 26 player).
- **C8 empire volume → 96–100 % exact** (était 0 %, golden = player ×24/64
  uniforme). Master volume global STATIQUE `0x57a=24` appliqué à chaque écriture
  Paula (`AUDxVOL = volByte × 0x57a >>6`, au trigger 0x2bc ET à l'animation
  d'enveloppe par tick 0x404). Détecté via le prologue
  `MOVE.W (d16,PC),D2 ; MULU.W D2,D1 ; LSR.W #6,D1` (`34 3A <d16> C2 C2 EC 49`),
  mot maître à `instr+2+d16`. Ajoutés `DwLayout::master_volume_offset` +
  `DwModule::master_volume`, appliqué `× master/64` aux DEUX chemins
  (`scale_master_volume`, garde `master != 64`). **Généralise** : idiome présent
  dans 90/120 fichiers (les 2 sites pointent toujours le même mot) ; 84 ont
  l'identité master=64 (no-op gardé → zéro régression byte-exact, xenon2
  vérifié inchangé), 6 ont un master non-64 désormais mis à l'échelle fidèlement :
  empire 24, alfred chicken 24, chip's challenge/menace/wizzcat 40, jaws 48,
  cosmic pirate 63 (seul empire vérifié au cycle — c'est le seul golden). La
  garde est le PROBE pas le variant : qball (old) n'a pas l'idiome → intact.
- **C7 leviathan** = deltas petits et VARIABLES (mécanisme différent, basse
  priorité) — seul reliquat volume.
- **Suite** = offset période qball/C1 (arrondi table), puis period-space
  C2/C3/C7/C8 (gros morceau), puis deltas volume variables C7 leviathan.

### Avancement (2026-06-03, Session — arpège per-frame → lane TrackPitch Points)
Backlog #1-adjacent : l'**arpège** était projeté sur le `TrackEffect::Arpeggio`
classique (3-pas, mod-3, row-trigger) — faux pour DW dont l'arpège est **per-frame,
par-canal, continu** (Ghidra `tetris FUN_0000010e` 2-bracket + `bad company
FUN_000001b6` canonical : pointeur avancé chaque frame, offset → INDEX de la table
de périodes = espace demi-ton, reset only sur cmd `0x90`/`0xA0`). Désormais baké en
**lane `TrackPitch` `LaneKind::Points` de `AutomationValue::Pitch`** (1er producteur
réel du primitif cœur, zéro changement cœur) : simu per-frame `runtime.rs::
tick_arpeggio` (emit-on-change + skip du frame *gate* counter==1), producteur
`attach_arpeggio_pitch_lanes`, lecture player **per-frame** (`current_abs_tick +
current_tick_in_row` — sinon résolution row). Gaté `use_arpeggio_pitch_lane =
!(command_map && P2)` : `command_map` = la jump-table canonique (≈tout le corpus),
donc la lane couvre canonical + tetris + **bubble bobble** (migré depuis l'enveloppe
par-note : ch3 41.5→11.4) ; SEUL exclu = la famille **oscillateur C4** (grimblood,
unique `command_map+P2` ; Ghidra `FUN_00000d6e` = oscillateur période, pas un arp
index-table → la lane le régresse 6.8→21.9, gardé sur enveloppe ; cosmic pirate C4
mais P3 + arp_ev=0 = inoffensif). **A/B tetris ch1 60.5→3.9, ch2 49.0→28.6**
(résiduel ch2 = timing sous-row arm/re-arm = plancher row-quantifié), **bubble ch3
41.5→11.4**. **Zéro régression corpus** (les 9 reps byte-identiques, grimblood
restauré) ; suites vertes
(xmrs 288, player 32+8, gate 116, robustesse 116/0-panic). Transverse : ~45/116
modules arment un arpège (gros : archipelagos 426, fright night 416, buffalo 111).
L'arpège est **oracle-visible** (la trace sort `effective_period` = AUDxPER).
Détail → `BASELINE.md` + mémoire `project_xmrs_dw_arpeggio_pitch_lane`.

### Avancement (2026-06-02, Session — period-space C7/C8 RÉSOLU + C2 ch3)
Deux fixes Ghidra (importeur, zéro changement cœur), **mesurés en A/B propre**
(deux fixes OFF vs ON, 9 traces régénérées de chaque côté ; C1/C3/C4/C5/C6/
obliterator **byte-identiques** des deux côtés → zéro effet collatéral). ⚠️ C3
bubble bobble et C4 grimblood étaient **déjà corrigés en sessions antérieures**
et ne sont **pas** touchés ici. Détail + tableau dans `BASELINE.md`.
- **C7 leviathan / C8 empire utilisent le chemin période du NEW player**, pas
  le composite P1 de qball. Lu dans leur corps `play` (leviathan @0xda, empire
  @0x1ae) : `AUDxPER = table[note] × instr_mult >> 10` sur une table mot
  pleine-note (leviathan @0x320 = P2 ; empire @0x64a = P3), note/instrument
  découplés. Seule la *structure du flux/commandes* est « old ». L'importeur les
  forçait en `Old → P1` + composite `PERIODS1[note%12]` (capait tout en
  octave 0). Ajouté `DwLayout/DwModule::period_via_finetune` (posé quand
  l'idiome finetune `45 FA <d16> 32 2D 00 0A|0C 74 0A` apparaît sur un binaire
  *old* ; le `0C` = slot mult `instr+0xC` de leviathan) : décode note direct +
  finetune table. **A/B : C7 medΔ −105/+74/−105/−307 → −1/0/−1/0 ; C8 −93/−112/
  +42/−83 → −1/+2/+2/+2.** qball (sans idiome) reste composite.
- **Slot transpose-canal 0x3 non reconnu dans le command_map.** Le classifieur
  du jump-table ne matchait le handler transpose qu'au slot 0x2f (famille bubble
  bobble) ; la famille empire/bad-company écrit le transpose au slot **0x3**
  (`MOVE.B (A1)+,(0x3,A0)`, relu par `ADD.B (0x3,A0),D0`). Il tombait en
  `Unknown` (0 octet d'arg) → l'octet de valeur n'était jamais consommé → flux
  de CE canal désaligné. Maintenant matché (args=1). Affecte **exactement**
  empire (events transpose 0→26) et bad company (0→13) ; grimblood (pas de
  handler 0x3) et bubble bobble (slot 0x2f) inchangés. **A/B : empire ch0 −93→−1
  (son transpose +12) ; C2 bad company ch3 medΔ −110→+2 (seul canal faux).**
- Reliquat sur ces clusters = dérive vibrato/slide en espace période (MAE), plus
  de mismatch d'espace.
- **Volume leviathan (Task #9) FAIT.** Volume STATIQUE per-instrument écrit à
  chaque note (`MOVE.W (0xE,A5),AUDxVOL` @0x17c, pas d'enveloppe). Mot à
  `instr+0xE` des records 16o @0x380 (`[64,64,58,54,55×5]`), table localisée via
  l'idiome SetSample `4B FA <d16> C0 FC 00 10` (lea-A5 + `mulu #0x10`) →
  `instrument_volume_offset` ; lu dans `DwSample.volume` (gaté period_via_finetune),
  émis par note (effective_vol préfère le vol sample au lieu de channel_volumes
  pour ces players). A/B : vol exact 16/0/16/0% → 100/31/100/0%, MAE 5–9 → ~0–1.
  Empire (records 12o, `mulu #0xC`, vol par enveloppe) ne matche pas → intact ;
  qball/tetris inchangés. Suites vertes (xmrs 277 + gate 116 ; robustesse 116/0-panic).

---

## 1. Le « regroupement » : clusters de player

David Whittaker a fait évoluer son replayer ; le corpus se range par
**(variant, dispatcher, table de période, largeur ligne sous-chant, flags)**.
Travailler **par cluster** (fixer un représentant, propager) plutôt que par fichier.

| # | Cluster (clé) | Représentant | Effectif ≈ | Quirks notables |
|---|---|---|---|---|
| C1 | New · `[B0,A0,90]` · P3 | **xenon2 (title)** ✅Ghidra | ~55 | canonique ; `enable_channel_transpose` (Effect8) sur xenon2 |
| C2 | New · `[B0,A0,90]` · P2 | **bad company** | ~2 | (jupitermaster1) |
| C3 | New · `[C0,B0,A0]` · P3 | **bubble bobble** ✅Ghidra | ~26 | `volume_bracket_is_pitch` possible ; A3-rebasé : bubble bobble, alfred chicken, gunship2000, krustyssuperfunhouse, snow strike |
| C4 | New · `[C0,B0,A0]` · P2 | **grimblood** | ~2 | A3-rebasé (grimblood −0x388) ; cosmic pirate |
| C5 | New · `[C0,A0]` · P2 (famille tetris) | **tetris** ✅Ghidra | 8 | `enable_sample_transpose` (−3, stride sample-info 16) ; `volume_bracket_is_pitch` ; Effect8 = offset volume global. Membres : army moves, bmx simulator, exotic 210, kickstart ii, sidewinder, tetris, the hunt for red october, xenon |
| C6 | **Old** · P1 · `[]` | **qball** ✅Ghidra | 3 | note = `sample×12+pitch` ; pointeurs 32 bits ; `SetSample` inerte ; samples one-shot. `xenon-sfx`/`leviathan-sfx` = banques 0-track (instruments-only) |
| C7 | **Old**-stream · **P2** · `[C0]` | **leviathan-deabs** ✅Ghidra | 1 | flux old MAIS période NEW (`period_via_finetune`, table @0x320=P2, `×instr[+0xC]>>10`) ; note directe ; **vol per-instr statique `instr+0xE` FAIT** (table @0x380, `instrument_volume_offset`, vol exact) |
| C8 | **Old**-stream · **P3** · `[C0,B0,A0]` | **the empire strikes back** ✅Ghidra | 1 | flux old + période NEW (`period_via_finetune`, table @0x64a=P3, `×instr[+0xA]>>10`) ; transpose global(0x582)+canal(slot 0x3) ; arpège per-tick |
| — | demi-volume | **obliterator-title** / `obliterator-ingame` | 2 | `enable_half_volume` (jump-table : slot8=`ST`, slot9=`SF`) |
| — | sans ancre MULU | **millennium** | 1 | heuristique fallback (cf. detect.rs) |
| — | canal vide (≈3 voix) | **sentinel** (`pos[0]=0`), sidewinder, super wonderboy | 3+ | une position-list de canal vide |

Représentants **chargés dans le projet Ghidra `xenon2`** (9, vérifié via
`list_open_programs`) : xenon2 (title), bubble bobble, tetris, qball, bad company,
grimblood, obliterator-title, leviathan-deabs, the empire strikes back. La
« session A » (bad company / grimblood / obliterator-title / leviathan-deabs /
the empire strikes back) est **faite**. (`anarchy sfx` n'est pas chargé — c'est
un des `dw_no_song` rejetés, sans intérêt de reverse.)

---

## 2. Stratégie de vérification (autonome → oreille en dernier)

Trois tiers ; les deux premiers sont automatiques.

### Tier 1 — Robustesse (zéro oracle, couvre les 116)
Étendre le gate test (`xmrs/src/lib.rs::load_all_dw_modules_from_music_dir`, ~l.194)
ou un nouveau test, pour **rendre** chaque module via `xmrsplayer` jusqu'à la fin de
sa boucle (cap raisonnable), et asserter :
- pas de panic / NaN / Inf ;
- pas de période hors plage Paula ;
- sur chaque canal qui porte des notes : RMS audio > seuil (pas de silence parasite) ;
- `verify_layers_consistent` OK (déjà testé).
⇒ capture l'essentiel de « fonctionner sans erreurs » sans aucune oreille.

### Tier 2 — Fidélité (oracle Ghidra, 1 représentant / cluster)
**Oracle = émulation PCode du replayer du binaire lui-même** (autorité absolue).
- Charger le représentant dans Ghidra (déjà fait pour C1/C3/C5/C6).
- Script (`run_ghidra_script` / `run_script_inline`, API Emulator 68k) : exécuter
  `init_main` (`0x66`) puis **N fois** `play_tick` (`0x1b6`) ; à chaque frame capturer
  les écritures registres Paula par voix `v∈0..4` :
  - `AUDxLC` = `0xdff0a0 + 0x10*v`, `AUDxLEN` `+0xa4`, **`AUDxPER` `+0xa6`**,
    **`AUDxVOL` `+0xa8`** ; `DMACON` `0xdff096`.
  - (Adresses confirmées dans la décompilation de `play_tick`.)
- **Côté nous** : ajouter un observateur per-tick léger dans
  `xmrsplayer/src/audio_observer.rs` (trait à la `ChannelsObserver`) exposant
  `period` + `volume` + `sample_index` par canal et par tick (≠ l'observateur PCM
  actuel). Rejouer le `Module` (1 tick player = 1 frame DW) et capturer la même trace.
- **Diff frame-à-frame** : période exacte en unités Amiga (tolérance ±1),
  volume exact (±1), même sample/voix on/off. Cible : **≥ 99 %** des ~3000 premières
  frames concordent ; consigner tout écart résiduel dans FORMAT.md.

### Tier 3 — Oreille (utilisateur, une seule fois, en fin de plan)
Liste finale : **un représentant par cluster** (8 fichiers) à écouter pour valider.
Pas de question par session — uniquement ce lot final.

---

## 3. Backlog de modélisation (priorité ↓), mappé aux clusters

1. **Vibrato en espace période** (tous clusters avec vib). Le replayer ajoute un
   triangle à la *période* (`sVar20 ± accumulateur`) ; on somme en demi-tons
   (`depth.raw()=max<<3`). Même correctif conceptuel que le slide qu'on vient de
   finir. *Le plus audible.* Oracle : trace vibrato sur xenon2/bubble.
2. **GlobalVolume / GlobalVolumeFade en lane continue** (`AutomationTarget::GlobalVolume`)
   au lieu de cuire la vélocité au row (clusters cmd 11/12 ; C5 via Effect8). Oracle :
   trace volume sur un module à fade.
3. **Enveloppe de volume à résolution frame** (tous) plutôt que row — pour les attaques
   rapides. Oracle : trace volume.
4. **`SetSpeed` (cmd 10) mid-song** → projeter `Speed`/`Bpm` + re-quantifier le mapping
   frame→row (sinon notes mal placées). Identifier les clusters concernés via la trace
   d'events (`events: … speed=N`). NB : depuis le fix grille-par-song (ci-dessous), un
   `SetSpeed` en cours de song re-casserait la grille fixe = `dw_speed_initial` ; pour ces
   modules il faudra une grille = PGCD des speeds rencontrés (bubble bobble a speed=0 → non
   concerné).

   **FAIT — grille de quantification par sous-song (ex-Task #10 « dernière song trop
   rapide »).** `to_module` quantifiait sur une grille FIXE de 3 frames/row, mais le pas réel
   des events est `dw_speed` frames (durée note = `(LongWait−0xDF)×dw_speed`). Quand
   `dw_speed` n'est pas multiple de 3 (bubble bobble songs 2/3 = speed 4/5), `floor((f−1)/3)`
   étalait les notes sur 1/2 rows irrégulières → jitter ±1 row (±60 ms), tempo moyen correct
   mais rythme bancal (66 % des onsets songs 2/3 off-grid). **Option A** : `speed`/`speed_u32`
   = `dw_speed` de la sous-song (shadow local dans la boucle de rendu) ; `speed_at_row =
   dw_speed`, `bpm_at_row = bpm_for(ss)` (le BPM continue de porter le delay-counter) ;
   `default_tempo` = speed de la song primaire ; `song_traces` porte le speed pour la passe
   lanes post-dedup. C'est l'idiome tracker exact (tempo=ticks/row=speed Whittaker,
   BPM=cadence des ticks). Le runtime honore déjà un tempo par-song via
   `TimelineEntry::speed_at_row`/`bpm_at_row` (sequencer.rs:331/406) → **zéro changement
   core**. Vérifié : NoteOn off-grid=0 sur les 4 songs ; song 0 (passée grille 3→6) matche
   toujours l'oracle Paula (gate 93-98 %, médΔ +2..+5 = offset concert) ; gate corpus 116
   modules vert. Conserve l'arpège classique XM (≥4 ticks/row) — contrairement à une grille
   1 frame/row. Corrige aussi tout module du corpus à speed non-multiple-de-3.
5. **Half-volume** (cluster obliterator) — modéliser le toggle Effect8/9 `ST`/`SF`.
6. **Canaux vides / 3 voix** (sentinel, sidewinder, super wonderboy) — vérifier que
   `get_num_channels` et le rendu gèrent une position-list vide.
7. **Square-waveform** (`enable_square_waveform`) — instruments/vibrato carrés.
8. **`Effect9` RestartPosition non nul** (saut mid-liste) — si un module du corpus le
   déclenche (à confirmer via trace).
9. **Old player C7/C8** (leviathan-deabs, the empire) — valider dispatcher 1- et
   3-brackets sur variante Old (note composite, périodes P1).

10. **Bouclage par-canal (voix indépendantes qui dérivent).** Confirmé par Ghidra sur
    `xenon2 (title).dw` : `play_tick` cmd 0x80 reboucle CHAQUE canal à sa propre liste
    (`*(short*)(0x600)`, base du canal), **aucun redémarrage global**. Les 4 voix ont des
    longueurs de passe inégales (9888/9888/9912/10014 frames) → elles se déphasent à
    l'infini (fidèle au matériel). Le modèle xmrs n'a qu'un wrap global (`song_loop_to`),
    d'où un wrap brutal au cap 20 min.
    - **Incrément 1 FAIT** (importeur) : quand `run_with_loop` bute sur le cap (aucune
      boucle commune ≤ cap), wrap à la **voix la plus longue** (`max(first_wrap−1)`) au lieu
      du cap → rien perdu, boucle ~3,3 min, voix longue *seamless*, résidu = petit stutter
      des voix courtes au wrap. Plus scaffolding cœur inerte (`ChannelLoop`,
      `Module.channel_loops`, `row_at`/`tick0` fold gatés, `entry_at_tick`) validé par tests.
    - **Incrément 2 FAIT** : `channel_loops` peuplé par voix (branche drift) + free-run du
      sequencer (`loop_generation`/`song_tick_span`/`current_abs_tick_monotonic`,
      `advance_to_next_entry` reboucle la timeline sans wrap global) + `row_at_with_playhead`
      threadé avec le tick monotone côté lecteur → **dérive infinie sans aucun saut**.
      Vérifié xenon2 (ch0/ch1 périodiques 9888 en continu à travers 10014/20028). Gaté
      `channel_loops` vide → XM/IT/MOD intacts. Voir `daw/LOOP_REGION_RFC.md`.

Chaque item est **oracle-vérifiable** par diff de trace Tier 2 sur son représentant.

---

## 4. Découpage en sessions (chacune autoportante, livre + commit)

- **Session A — Harnais & baseline.**
  1. ✅ **Fait** — Tier 1 : test de rendu robustesse sur les 116 (0 panic, 0 échec ;
     1 résiduel volume-lane signalé : emlyn ch0). Cf. avancement §0.
  2. Tier 2 :
     - ✅ **Fait** — script Ghidra d'émulation `init_main`+`play_tick` avec capture
       Paula par `MemoryAccessFilter` (`dw/oracle/DwPaulaOracle.java`, doc
       `README.md`, golden `xenon2_title.csv`). Reproductible, validé. Cf. avancement §0.
     - **Reste** : observateur per-tick (`audio_observer.rs`) + trace côté player ;
       charger les 5 représentants manquants ; diff trame-à-trame → **`BASELINE.md`**
       (rapport par cluster) committé ici.
  - *Livrable* : harnais + baseline. Aucune correction de fidélité encore.

> ⚠️ **Le travail n'a pas suivi cet ordre de sessions** — les items ont été
> cherry-pickés par impact. Statut réel par item ci-dessous (vérifié code +
> notes d'avancement, 2026-06-03). Le découpage B/C/D reste indicatif.

- **Session B — Fidélité lot 1 : vibrato période + GlobalVolume lane.**
  - #1 vibrato période : ⏳ **OUVERT** — c'est LE reliquat de fidélité (dérive
    vibrato/slide en espace période, cf. avancement period-space).
  - #2 GlobalVolume lane continue : ⏳ **non fait comme lane**. Mais le volume a
    été largement résolu autrement (avancement « Session B / volume » :
    tetris/qball/empire + leviathan Task#9 — vol per-instr/canal/master).

- **Session C — Fidélité lot 2 : enveloppe frame-res + SetSpeed + half-volume.**
  - #3 enveloppe frame-res : ✅ **FAIT** (`volume_to_volume_envelope`, 1 point/frame).
  - #4 SetSpeed : ✅ grille de quantif par-song FAITE ; ⏳ SetSpeed **mid-song**
    (re-grille PGCD) non fait.
  - #5 half-volume : ✅ **détection FAITE** (`enable_half_volume` via effect
    jump-table ; largeur d'`Effect9` corrigée).

- **Session D — Lot 3 + balayage final : canaux vides, square, Effect9, old player.**
  - #9 old player C7/C8 : ✅ **FAIT** (`period_via_finetune`, leviathan/empire).
  - #6 canaux vides / #7 square / #8 Effect9-restart : ⏳ **OUVERTS**.
  - **Balayage de trace** final + liste d'écoute Tier 3 : ⏳ à produire.

Chaque session se termine par : `cargo test` (les 2 crates) + diff de trace verts,
MAJ FORMAT.md, commit.

---

## 5. Critères d'acceptation (gate test final)

- **Chargement** : 116/116 chargent + `to_module` + `verify_layers_consistent` ; 4
  rejetés `dw_no_song` (inchangé).
- **Robustesse** : 116/116 rendus jusqu'à fin de boucle via `xmrsplayer`, sans
  panic/NaN, RMS sain sur les canaux à notes.
- **Fidélité** : pour chaque représentant de cluster, trace Paula période exacte
  (±1) sur ≥ 99 % des ~3000 premières frames, volume ±1 ; résiduels documentés.
- **Oreille** : lot Tier 3 validé par l'utilisateur.

---

## 6. Carte des fichiers & ancres (exactes)

- Importeur : `xmrs/src/tracker/import/dw/dw_module.rs`
  (`DwModule::load`, `to_module` ~l.249, `attach_vibrato_lanes` ~l.1272,
  `attach_slide_lanes` ~l.1420).
- Runtime/simulateur : `xmrs/src/tracker/import/dw/runtime.rs`
  (`apply_event` ~l.598, `tick_channel` ~l.512, `ChannelState`, `TickEvent`).
- Détection/clusters : `xmrs/src/tracker/import/dw/detect.rs` (`DwFeatures` l.37 :
  `enable_*` ; axes de détection cf. FORMAT.md §10).
- Décodeur d'events : `xmrs/src/tracker/import/dw/event.rs` (`DwTrackEvent`, l.27).
- Player : `xmrsplayer/src/channel/lanes.rs` (`apply_slides_from_lanes` ~l.260),
  `xmrsplayer/src/audio_observer.rs` (ajouter l'observateur per-tick).
- Quirks : `xmrs/src/core/compatibility.rs` (`PlaybackQuirks`).
- Gate test : `xmrs/src/lib.rs::load_all_dw_modules_from_music_dir` (~l.194).
- Outils : `xmrs/examples/inspect_dw.rs` (triage ; `-q` résumé ; `DW_TRACE=1` trace),
  `xmrs/examples/dump_dw_pattern.rs` (grille rendue par pattern/canal).
- **Ghidra (oracle, source de vérité unique)** : `play_tick` `0x1b6`, `init_main`
  `0x66`, `sample_loader` `0x1414` ; registres Paula `0xdff0a0 + 0x10*v`
  (+`0xa6` PER, +`0xa8` VOL), DMACON `0xdff096`. Programmes chargés :
  `list_open_programs`. Émulation Paula tick-exacte : `oracle/`.

---

## 7. Commandes types

```sh
# Triage / clusters (résumé une ligne par fichier)
cargo run --release --features=demo --example inspect_dw -- -q ~/Music/dw/*.dw
# Trace d'events complète d'un module
DW_TRACE=1 cargo run --release --features=demo --example inspect_dw -- "~/Music/dw/<f>.dw"
# Grille rendue (pattern, canal)
cargo run --release --features=demo --example dump_dw_pattern -- "<f>.dw" <song> <pattern> [ch]
# Gate test
cargo test --release --features=demo load_all_dw_modules_from_music_dir
# Suites complètes
( cd xmrs && cargo test --release --features=demo ) && ( cd xmrsplayer && cargo test --release )
```
