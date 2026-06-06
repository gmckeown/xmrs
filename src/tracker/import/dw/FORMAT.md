# Format David Whittaker (`.dw`) — spec & importeur

Document de référence chargeable à la demande. Couvre le format `.dw`,
la sémantique du replayer (vérifiée par rétro-ingénierie), l'architecture
de l'importeur xmrs, les décisions clés et les conventions de code.

---

## 1. Sources de vérité (ordre de priorité)

1. **Ghidra MCP sur le binaire `.dw` lui-même** (ex. `~/Music/dw/xenon2 (title).dw`).
   La fonction `play_tick` (le tick 50 Hz) et `init_main` sont l'autorité absolue —
   tout l'importeur est reversé directement depuis le 68000 embarqué.
2. **Oracle d'émulation Paula** (`oracle/`) : émule le vrai replayer 68k et capture
   les registres Paula par tick → diff trame-à-trame contre la trace player.

À l'oreille : l'utilisateur dispose de l'original et sert d'oracle final.

---

## 2. Conventions de code (cœur partagé)

- **Prudence avec le cœur d'xmrs.** Toute modification de `xmrs` (core) se confirme
  avant ; on ne la fait pas à la légère.
- **Étudier l'impact sur les DEUX crates sœurs** (`xmrs` ET `xmrsplayer`, au même
  niveau) avant une décision touchant le code partagé.
- **Code lisible pour un humain : pas de nombres magiques.** Utiliser des constantes
  nommées (`Q15::ZERO`, `Phase::QUARTER`, …). Si une constante manque, l'ajouter,
  ou employer des macros lisibles.
- **Code auto-documenté > tests unitaires triviaux.** Préférer un code clair (ancres
  nommées, commentaires de transition) à des tests basiques.
- **Vérifier empiriquement avant de conclure** (lire le code réel, échantillonner les
  valeurs) plutôt que supposer.

---

## 3. Structure du fichier

Big-endian, 68000. Détection : `xmrs/src/tracker/import/dw/detect.rs`.

- **`init`** : remplit l'état runtime depuis l'en-tête du sous-chant sélectionné.
- **Sous-chant** (offset détecté, ex. `0x96e`) :
  - `byte[0]` = **speed** (frames par "row" ; multiplicateur de LongWait).
  - `byte[1]` = **delay_counter_speed** (throttle global, cf. §5).
  - puis un **pointeur de position-list par canal** (`u16` new-player, `u32` qball).
- **Position list** (une par canal) : suite d'**offsets de track**, terminée par
  `0x0000` (⇒ reboucle sur l'entrée 0 / `RestartPosition`) ou une valeur à bit `0x8000`.
- **Track** : flux d'octets, terminé par `EndOfTrack` (`0x80`). Dédupliqué par offset.
- **Samples** : table d'infos (`0xc` octets/sample : ptr, len, loop), + données PCM.
- **Volume envelopes** (`0xA0` bracket) et **arpeggios** (`0x90` bracket) : tables de
  pointeurs vers des séquences d'octets.
- Variantes : **New player** (la plupart) vs **Old player** (qball : note = `sample*12 + pitch`,
  `SetSample` inopérant).

---

## 4. Jeu de commandes de track (octet `b`)

| Condition                     | Commande           | Effet |
|-------------------------------|--------------------|-------|
| `b < 0x80`                    | **Note**           | index période (new) / `sample*12+pitch` (old) |
| `b >= 0xE0`                   | **LongWait**       | wait = `(b − 0xDF) × speed` frames (inline) |
| `b >= sample_threshold` (`0xB0`) | **SetSample**   | `sample = b − threshold` |
| `b >= volume_envelope_threshold` (`0xA0`) | **SetVolumeEnvelope** | arme une enveloppe de volume |
| `b >= pitch_arpeggio_threshold` (`0x90`)  | **SetPitchArpeggio**  | arme un arpège de hauteur |
| sinon, `b & 0x7F` :           | **Effet** :        | |
| 0  | EndOfTrack          | avance dans la position-list (ne consomme pas le tick) |
| 1  | Slide               | 2 args : `speed` (i8), `counter` (delay) |
| 2  | Mute                | **termine le tick** (silence pendant le wait) |
| 3  | WaitUntilNextRow    | **termine le tick** (tient la note pendant le wait) |
| 4  | StopSong            | |
| 5  | GlobalTranspose     | 1 arg (gaté par `enable_global_transpose`) |
| 6  | StartVibrato        | 2 args : `speed`, `max` (profondeur) |
| 7  | StopVibrato         | |
| 8  | Effect8             | module-dépendant : volume-fade / **channel-transpose** / half-vol |
| 9  | Effect9             | 0 arg si half-vol, sinon 2 (restart position, géré au load) |
| 10 | SetSpeed            | modifie `playingInfo.Speed` (global) ou delay-counter |
| 11 | GlobalVolumeFade    | |
| 12 | SetGlobalVolume     | (ou start sound-fx selon le module) |
| 13 | StartOrStopSoundFx  | (ignoré) |
| 14 | StopSoundFx         | (ignoré) |

> **Nommage** : les identifiants collent désormais à la fonction (Ghidra) — le
> bracket `0xA0` arme une **enveloppe de volume** (`SetVolumeEnvelope`,
> `volume_envelope_threshold`, `current_envelope`) et le bracket `0x90` arme un
> **arpège de hauteur** (`SetPitchArpeggio`, `pitch_arpeggio_threshold`,
> `current_arpeggio`). L'ancien `SetArpeggio`/`SetEnvelope` (inversés vs leur rôle)
> a été renommé pour supprimer ce piège. Cas particulier : sur les modules à 2
> brackets (tetris) le bracket `0xA0` porte en réalité un arpège — voir
> `volume_bracket_is_pitch`.

---

## 5. Sémantique du tick (50 Hz PAL)

Par frame, `play_tick` :
1. **Delay counter** : accumulateur 8 bits `acc += delay_counter_speed` ; si débordement
   (`carry`), la frame entière est **sautée**. ⇒ ralentissement uniforme de
   `(256 − delay)/256` (≈ 46 Hz si delay=20). *Voir §6 : modélisé en BPM, pas en saut.*
2. Volume-fade global.
3. Pour chaque canal : `speed_counter--` ; si `== 0` → lire les commandes
   (`ReadTrackCommands`) ; si `> 1` → `DoFrameStuff` (effets per-tick).

**Timing par canal** : `speed_counter` est rechargé à la valeur du **dernier LongWait**
(`(b−0xDF) × speed`) après **chaque** commande qui termine le tick — Note, Mute,
**WaitUntilNextRow**. `WaitUntilNextRow` répété tient ainsi une note sur N waits.

**DoFrameStuff** (effets per-tick) : arpège (cycle d'offsets, +note → période),
slide (`SlideValue += speed; période −= SlideValue`, après delay `counter`),
vibrato (triangle, cf. §7), pas d'enveloppe (avance tous les `step_interval+1` ticks).

---

## 6. Architecture de l'importeur (côté xmrs)

`dw_module.rs` :
- `DwModule::load` → parse header, samples, position-lists, tracks (dédup), envelopes, arps.
- `to_module()` → projette vers un `Module` DAW (clips/tracks/timeline/automation).
  - 1 row = `speed` (=3) frames. Notes quantifiées : `row = (frame−1)/speed`.
  - Chaque sous-chant = une "song" xmrs (cap `MAX_SONGS=4` ; song 0 = primaire).
  - Segments découpés aux frontières de pattern (64 rows) ; clips dédupliqués par contenu.
  - Panning Amiga **LRRL** (canaux 0,3 gauche ; 1,2 droite).

`runtime.rs` : `Simulator` rejoue le state-machine frame par frame et émet des `TickEvent`
(NoteOn, Mute, VibratoStart/Stop, SlideStart, …) que `to_module` transforme en cellules.

`xmrsplayer` (crate sœur) consomme le `Module`. Vibrato/tremolo passent **toujours** par
`WaveformState::value_q15` (aucun match exhaustif sur `Waveform`). `update_frequency`
somme les `PitchDelta` en **espace demi-tons** puis convertit vers la courbe Amiga.

---

## 7. Décisions & correctifs clés (validés sur xenon2 title)

- **WaitUntilNextRow rearme `speed_counter`** au dernier LongWait (Ghidra case `0x83` →
  `chan[+0x616] = chan[+0x614]`). Sans ça les notes tenues s'effondraient à 3 frames →
  ordre des tracks détruit + note parasite. *LE bug d'agencement.*
- **Delay counter modélisé en BPM**, PAS en saut de frames. Le saut + quantification au
  row faisait dériver l'espacement ±1 row (« hésitation » audible). Le simulateur tourne
  toutes les frames (notes sur grille entière) ; `to_module` pose
  `BPM = 125 × (256 − delay)/256` (xenon2 → 115) sur `default_bpm` + chaque `bpm_at_row`.
- **Bouclage** : position-list terminée par `0` ⇒ `loop_to = Some(0)`. `run_with_loop`
  rend exactement UNE période de boucle (réalignement = LCM des longueurs de passe par
  canal, via `first_wrap_frame`) et pose `Module::song_loop_to = Some(0)`.
- **Vibrato** (`attach_vibrato_lanes`) : triangle de période `0→+max→0→−max→0`.
  - waveform = **`Waveform::Triangle`** (bipolaire ; ajouté à l'enum core).
  - `speed.raw() = 64·vspd/max` (Q8.8, raw 256 = 1 cycle/tick ; 1 tick = 1 frame).
  - `depth.raw() = max << 3` (≈ `max/32` demi-ton ; le player lit `depth.raw()` direct
    comme Q8.8 demi-tons). *Pas* d'ajustement Amiga (somme en demi-tons).
  - **Continuité inter-clips** : chaque span de vibrato est projeté sur tous les clips
    qu'il recouvre (paire `Set..Clear` bornée par clip ⇒ sûr vis-à-vis de la dédup).
- **Enveloppe de volume** : volume au déclenchement = `env.initial()` (1ère valeur, =
  `EnvelopeList[1]`), PAS `peak()`. L'animation per-row (`attach_envelope_animation`)
  rampe ensuite step par step (un step tous les `step_interval+1` ticks).
- **Slide** (`attach_slide_lanes`) — vérifié Ghidra `play_tick` (effet `case 0x81`) +
  `.cs` 1953/2157 (identiques) : arme `SlideValue=0, SlideSpeed=speed, SlideCounter=counter` ;
  par frame, si `counter==0` alors `SlideValue += speed; période = base − SlideValue`,
  sinon `counter--`. La `base` est **recalculée fraîche chaque frame** ⇒ rampe **LINÉAIRE**
  de `−speed` unités de période/frame (≠ accélérant), après un délai de `counter` frames.
  Le player accumule déjà (`période += rate` par tick, 1 tick = 1 frame) ⇒ mapping fidèle :
  `rate.raw() = −speed` (×1) avec `Set` retardé de `counter` ticks. Le quirk
  `pitch_slide_ticks_at_row_zero` (posé par `to_module`) fait tourner la lane **sur chaque
  tick (tick0 inclus)** ⇒ avance à chaque frame comme le replayer (plus de sous-compte
  `(speed−1)/speed`). La fin est bornée par un `SlideStop`/`Clear` que le runtime émet à la
  lecture de la row suivante (`ChannelState::slide_active` ↔ `*pbVar15=0` de Ghidra) ; le
  `Clear` est rattaché à la **même** lane `TrackPitch(n)` que son `Set` (track mémorisée par
  canal). Résiduel : `tick` quantifié au row ⇒ point d'engagement à < 1 row près.
- **Effect8** = transpose de canal sur xenon2 (gaté par `enable_channel_transpose`).
- **Slot transpose-canal du command_map** : le classifieur de jump-table reconnaît
  le handler `MOVE.B (A1)+,(<slot>,A0)` comme `ChannelTranspose` pour `<slot>` ∈
  **{0x2f, 0x3}** (0x2f famille bubble-bobble ; 0x3 famille empire/bad-company,
  relu par `ADD.B (0x3,A0),D0`). Un slot non reconnu tombe en `Unknown` (0 arg)
  → l'octet de valeur n'est pas consommé → **désalignement du flux** de ce canal
  (symptôme : gate qui s'effondre, notes aberrantes). Vérifié au diff Paula A/B
  (affecte exactement empire + bad company ; grimblood/bubble-bobble inchangés).

---

## 8. Approximations connues / points ouverts

- Slide : rampe **linéaire** modélisée exactement (`rate = −speed`, délai `counter`, apply
  chaque tick via quirk, borné par `Clear` ; cf. §7, 2026-05-29). Résiduel mineur : point
  d'engagement quantifié au row (< 1 row). 1 seul slide dans xenon2 (title comme ingame :
  `speed=−1, counter=8`).
- Enveloppe : échantillonnée à la **résolution du row** (fin pour `step_interval` lents ;
  sous-échantillonne les attaques rapides).
- Arpège : `classic_pair` → `Arpeggio{half1,half2}` (3 offsets max ; tronque les arps à
  4+ entrées ou démarrant ≠ 0). **xenon2 n'a aucun arpège de hauteur** (`env=0`).
- `Effect9` RestartPosition non nul (saut mid-liste) non modélisé (aucun module du corpus
  n'en a besoin).
- `numberOfChannels` 3 vs 4, half-volume, square-waveform : non couverts / hors corpus.

---

## 9. Outillage

- `cargo run --release --features=demo --example inspect_dw -- <fichier.dw>` : triage
  (détection, samples, dispatcher, position-lists, mix d'événements, boucle, simulation).
  `DW_TRACE=1` dumpe la trace d'événements complète.
- Test garde-fou : `load_all_dw_modules_from_music_dir` (strict : charge tout `~/Music/dw`
  ou rejette en `dw_no_song`) + `to_module` + `verify_layers_consistent` sur 116 modules.

---

## 10. Corpus & détection cross-module (au-delà de xenon2)

État (2026-05) : `~/Music/dw/` **116 / 120** chargent (114 chants + `xenon-sfx`,
`leviathan-sfx` = banques de samples sans chant, chargées en *instruments-only*).
Les **4** restants (`anarchy sfx`, `blood money sfx`, `spellbound sfx` = moteurs SFX
sans table PCM ; `feud fake` = placeholder 2 Ko) sont rejetés proprement en
`ImportError::InvalidMagic("dw_no_song")`. Règle : un module chargé n'a PAS besoin de
chant — `dw_no_song` ne se déclenche que si samples ET sub_songs ET tracks sont vides
(directive : charger les samples même des SFX).

**Variantes du corpus** : new player canonique (xenon2, majorité) ; multi-sous-chant
(beast1.* — auto-pick = position-list cumulée la plus longue) ; **old player** (qball :
pointeurs 32 bits, lignes sous-chant 18 o, note composite `sample×12+pitch`, Periods1) ;
dispatcher `[C0,B0,A0]` (speedball, bubble bobble, grimblood) ; lignes sous-chant 8 o
sans header speed/delay ; modules sans ancre `MULU` (millennium, heuristique fallback).

**Axes de détection** (`detect.rs`) :
- **Ouverture permissive** : accepté si un `LEA -d(PC), A3` (`47 FA <hi≥F0>`) apparaît
  dans les premiers 0x1000 octets (subsume tous les prologues).
- **Sample loader** : 4 étapes — call-graph PC-rel (`41 FA`/`4B FA` A5/`43 FA` A1),
  call-graph A3-rel (`41 EB`), puis flat-scans PC/A3.
- **Largeur ligne sous-chant** via `MULU.W #8` (4×u16, sans header) / `#10` (header 2 o
  + 4×u16) / `#18` (qball, 4×u32). Byte-order des lignes 10 o : `[u8 speed, u8 delay]`
  si byte0≠0 (xenon2, beast1), `[u16 speed]` si byte0==0 (tetris — sinon vitesse 1
  ultra-rapide).
- **Dispatcher** : `CMP.B/BLT.B/SUBI.B` jusqu'à 3 brackets (`[B0,A0,90]`, `[C0,B0,A0]`) ;
  fallback relâché `CMP #XX + BLT/BCS` (XX∈0x90..0xD8) pour tetris (`[C0,A0]`).
- **Variante Old** si pointeurs 32 bits OU `4A 2B` (TST.B disp(A3)) après l'ancre.
- **`enable_sample_transpose`** (famille tetris : −3 sur chaque sample mélodique ;
  stride sample-info **16** au lieu de 12).
- **`enable_half_volume`** via la **jump-table d'effets** (`LEA…A2` + `JMP`) : vrai
  seulement si slot effet-8 = `ST (1,A0)` ET slot effet-9 = `SF (1,A0)` → 2/120
  (obliterator). Un scan aveugle `50/51 E8` matchait les handlers vibrato → ~110 faux
  positifs → décalait l'arg `Effect9` → notes ultrasoniques (bug bubble bobble).

**Rebasing A3 / `start_offset`** : `start_offset = signed_disp16 + (init_offset + 2)`
(= `startOffset` du .cs). Tout offset déréférencé **via A3** est relatif à cette base :
position-lists + offsets de tracks + cibles de boucle, tables envelope, tables arpège
(file = `value + start_offset`). NE PAS rebaser les *emplacements* de tables (trouvés via
LEA **PC-relatifs** = déjà absolus). A3 est à 0 sur presque tout (no-op), mais **6 modules**
pointaient avant l'image et étaient mal importés avant ce fix : `bubble bobble` (−0x442),
`alfred chicken` (−0xD18), `grimblood` (−0x388), `gunship2000` (−0xC6A),
`krustyssuperfunhouse` (−0xCD4), `snow strike` (−0xE).

**Seuil SetSample** : `dispatcher.sample_threshold().unwrap_or(0xB0)` (était hardcodé
0xB0 → `[C0,B0,A0]` résolvait `0xC3` en sample 19 au lieu de 3).

**Pitch period-exact (les deux familles)**, `relative_pitch = 0`, horloge Amiga PAL
`3_546_894/period` :
- Old composite (qball) : `period = Periods1[pitch%12]` direct (1 octave, sans
  finetune ; note = `sample×12+pitch`).
- New **et old-stream finetune** (drapeau `period_via_finetune`) :
  `period = PERIODS[note + sample.transpose] × (0x369E99 / sample_freq) >> 10`
  (constante `0x369E99` présente dans chaque new player). L'ancien anchor `+12` ne
  marchait que par coïncidence sur xenon2 (samples près de C-4).
  - **`period_via_finetune`** sépare le *chemin période* de l'axe Old/New :
    posé pour tout New, et pour les binaires **Old** dont le corps `play` porte
    l'idiome finetune `45 FA <d16> 32 2D 00 0A|0C 74 0A` (lea table → `MOVE.W
    (mult,A5),D1` → `MOVEQ #10`). Ces vieux-flux (C7 leviathan table @0x320=P2,
    `mult@instr+0xC` ; C8 empire table @0x64a=P3, `mult@instr+0xA`) décodent la
    note **directement** (index pleine-octave, instrument posé par commande
    séparée 0xC0), pas en composite. qball n'a pas l'idiome → reste composite.

**Sample loops** : `loop_start` (s32 @ +4) lu **uniquement** new player. Old player =
table sample-info construite au runtime (pas de loop sur disque) → samples forcés
one-shot.

**Bracket A0 = volume OU pitch** : sur cascade 3-brackets (`[sample,A0,90]`) A0 = enveloppe
volume, 90 = arpège. Sur cascade 2-brackets (`[sample,A0]` : tetris & 7 autres) **A0 = arpège
de hauteur** (`volume_bracket_is_pitch`) — `table_looks_like_arpeggio` discrimine (offsets
signés ≤0x18 + terminateurs 0x80 vs rampes vers 0x40). Appliquer ces offsets comme volume
mettait tetris en quasi-silence.

**Garde-fous `to_module`** : `MAX_SONGS=4` ; sous-chants d'audition cappés à 6000 frames ;
garde anti-boucle-infinie dans `tick_channel` (`consumed > Σ events + 1024`) contre les
position-lists sans événement terminant le tick (OOM sur bmx simulator.dw).

**Hôte** : `cpal_player.rs` remplace `module.name` par le nom de fichier quand
`origin == Dw` (les `.dw` ne portent pas de nom de chant embarqué).
