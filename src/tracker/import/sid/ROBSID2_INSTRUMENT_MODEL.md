# Le dernier player Rob Hubbard — système d'instruments

Reverse complet du **système d'instruments** du dernier player C64 de Rob Hubbard, en vue de
`InstrRobSid2`. Ce document remplace/corrige la partie « instrument » de
[`V40_PLAYER_STUDY.md`](V40_PLAYER_STUDY.md) (§10 liste les corrections).

**Source de vérité : Ghidra**, conformément à `feedback_sid_ghidra_over_sidera` — aucune valeur
ici n'est dérivée de sidera/sidplay. Programmes annotés dans le projet Ghidra `xenon2` :

| programme | base | rôle |
|-----------|------|------|
| `Lion_Heart.bin` | `$1003` | **référence**, entièrement labellisée + commentée |
| `Lakers_vs_Celtics.bin` | `$1003` | même player, bloc RAM relogé (importé + commenté) |
| `Off_the_Cuff.bin` | `$1000` | variante **antérieure** de la même famille (le « avant ») |

Points d'entrée annotés : `lh_play $101a`, `lh_wave_engine $1699`, `lh_instrflag_fx $170e`,
`lh_init_subtune $18ff`, `lh_rows_to_frames $19d6`, données `lh_instruments $1b4b`,
`lh_filter_slots $1cb0`, `lh_freq_table $1e34`.

---

## 1. Quel est « le dernier player » — établi, pas supposé

Scan des **96 SID de Rob Hubbard** (HVSC, `~/Documents/robrs/sid/hvsc/`) pour la signature du
dispatcher de marqueurs (`CMP #$82 / BEQ` + `CMP #$83 / BEQ`) : **12 tunes** portent un player
à marqueurs par-note. Ils se répartissent en **deux générations** :

| génération | tunes | marqueurs testés |
|------------|-------|------------------|
| **G2 — la dernière** | Lion_Heart, Sun_Never_Shines, Pacific_Coast, Radio_ACE, Go_Go_Dash, **Lakers_vs_Celtics** | `80 82 83 84 86 87 88 89` |
| G1 — antérieure | After_8, Mr_Meaner, Off_the_Cuff, Pygmies_Revenge, Rikky, Rock_Tells_the_Tale | `80 82 83` |

Les 5 premiers de G2 partagent le **même code octet pour octet** dans `$1003–$1a2f` : 16 à 61
octets de différence, **tous des opérandes d'adresse absolue** (tables de patterns, de filtre, de
fréquences). Le code est identique, seules les tables bougent.

**Lakers_vs_Celtics est le même player**, pas une variante distincte. La faible ressemblance
octet-à-octet (12 %) signalée dans `V40_PLAYER_STUDY.md` est un artefact : **tout le bloc de
variables a été relogé** (`$1a27..` → `$1a4f..`), donc *tous* les opérandes absolus diffèrent. La
structure, le jeu de marqueurs et le record d'instrument sont identiques ; seuls s'ajoutent
quelques écritures de debug inertes (`INC $0406,X`, `STA $0500,X`, `STA $d02c`, `BIT $d020`).

G1 est bien un **stade antérieur** et non une simplification tardive (cf. §9) : filtre plus
pauvre, fréquence en registre courant au lieu d'un accumulateur, ordre de piste plus rustique.

> ⚠️ **Le sens des marqueurs n'est pas stable entre générations.** En G1, `0x83` = *vibrato*
> (2 opérandes) ; en G2, `0x83` = *arpège* (1 opérande) et le vibrato est passé en `0x86`. Un
> importeur doit lier le décodage à la génération détectée, jamais au seul octet.

**Cible de `InstrRobSid2` : G2.** Référence `Lion_Heart`.

---

## 2. Le pipeline d'une frame (contexte minimal)

Le player est appelé une fois par frame (50 Hz). Chanson :

```
sous-chanson ─▶ 4 listes d'ordre ─▶ patterns ─▶ entrées : [marqueurs…] [longueur] [note]
                (3 voix SID + 1 piste FILTRE)
```

Espace des octets dans une entrée de pattern :

| octet | rôle |
|-------|------|
| `0x00–0x5F` | note (index dans `lh_freq_table`, 96 notes) |
| `0x60` | **silence** : coupe seulement le gate, ne touche pas au patch |
| `0x80–0x8F` | marqueur d'effet (bit7 posé, bit6 clair) — voir §5 |
| `0x81` | fin de pattern (lu en *lookahead* après la note, jamais dans la boucle de marqueurs) |
| `0xC0–0xFF` | **octet de longueur** : bits 0-4 = rows−1, **bit5 = TIE** |

Ordre exact des étages par voix et par frame (`lh_play`) — l'ordre compte, chaque étage peut
écraser la fréquence calculée par le précédent :

1. avance de row / lecture d'entrée (si la row change) ; sinon gate-off en fin de note
2. **arpège** → `note_jouée = note_base + offset`
3. `freq = lh_freq_table[note_jouée] + accumulateur_portamento`
4. **vibrato** → *réécrit* `freq` (si actif)
5. **balayage de pulse-width** → écrit `d402/d403`
6. **balayage de filtre** (accumulateur de cutoff)
7. **accumulation du portamento** (pour la frame suivante)
8. si flag bit0 : **moteur wavetable** (possède `ctrl` et `freq`)
   sinon : `lh_instrflag_fx` — bit2 (attaque waveform), bit4 (arpège table), bit6 (attaque pitch)
9. écriture `d404 = ctrl & masque`, `d401/d400 = freq`
10. après les 3 voix : piste filtre + écriture `d415–d418`

---

## 3. Le record d'instrument — 16 octets

Base `lh_instruments $1b4b`, **stride 16**, index = opérande du marqueur `0x80`.
L'index est réduit par `ASL A ×4` ⇒ **16 instruments maximum**, les bits 4-7 de l'opérande sont
ignorés.

| off | champ | preuve Ghidra |
|-----|-------|---------------|
| **+0** | pulse-width seed, octet bas → `d402` | `12da: LDA $1b4b,X / STA $d402,Y` |
| **+1** | pulse-width seed, quartet haut → `d403` | `12e1: LDA $1b4c,X / STA $d403,Y` |
| **+2** | **ctrl de sustain** (waveform + gate), l'état stable de `d404` | `12b5: LDA $1b4d,X → lh_ctrl_frame` |
| **+3** | attack/decay → `d405` | `130e` |
| **+4** | sustain/release → `d406` | `1314` |
| **+5** | **paramètre de vibrato** : bits 3-6 = amplitude, bits 0-2 = shift | `12ed: LDA $1b50,X` (auto-modif `$12ff`) |
| **+6** | **vitesse du balayage de PW** (0 = pas de balayage) | `13f8: LDA $1b51,Y → lh_pw_speed` |
| **+7** | **FLAGS** — voir §4 | `1256/13f2: LDA $1b52,X → lh_instr_flags` |
| **+8** | pointeur wavetable **lo** (bit0) │ index table d'arpège (bit4) │ **note d'attaque** (bit6) | `168b` / `1750` / `1789` |
| **+9** | pointeur wavetable **hi** (bit0) │ **ctrl d'attaque** (bit2) | `1690` / `171d` |
| **+10** | **paramètre d'arpège par défaut** (même encodage que le marqueur `0x83`) | `12bb: LDA $1b55,X → auto-modif $13a0`, consommé en `139f: STA lh_arp_param` |
| **+11** | **durée d'attaque en frames** (compteur partagé bit2 / bit6) | `130a: LDA $1b56,X` → empilé → `13a5: STA lh_attack_frames_ctr` |
| **+12** | **index de slot de filtre** (×8 dans `lh_filter_slots`) | `1328: LDA $1b57,X / ASL ×3` |
| **+13** | **bornes de rebond du PW** : quartet haut = borne sup, quartet bas = borne inf | `1552: LDA $1b58,Y` (auto-modif `$1594`/`$157a`) |
| **+14** | **délai de vibrato** en frames | `12f3: LDA $1b59,X` (auto-modif `$1304`) |
| **+15** | inutilisé (aucune lecture dans le binaire) | — |

`+8`/`+9` sont **multiplexés par les flags** : ce sont deux octets libres dont l'interprétation
dépend de bit0 / bit2 / bit4 / bit6. Un modèle propre doit donc porter un *enum* de programme,
pas deux `u8` bruts.

### Les 16 records de Lion_Heart (données réelles)

```
#   +0 +1 +2 +3 +4 +5 +6 +7  +8 +9 +A +B +C +D +E +F
0   00 08 41 08 c7 00 00 01  4b 1c 00 01 00 fd 00 00   wavetable $1c4b
1   00 08 41 09 59 00 00 01  5e 1c 00 04 01 fd f4 fc   wavetable $1c5e
2   00 08 11 0d 47 00 00 01  71 1c 00 01 02 fd 00 00   wavetable $1c71
3   c2 00 41 09 25 00 1f 44  5f 81 00 04 03 f0 00 00   bit2+bit6 : attaque bruit note 95, 2 frames
4   80 01 41 0a c4 2b 10 20  5f 89 00 02 04 fe 00 00   filtre slot 4, vibrato 2b
5   00 00 41 0f b9 22 52 00  3a 15 00 02 05 f0 0d 00   vibrato 22 après 13 frames de délai
6   00 06 15 0d c9 22 20 00  00 21 00 02 06 d4 00 00
7   ce 00 43 0a f9 2b 44 04  51 41 00 04 07 70 05 00   bit2 : attaque pulse → sustain pulse+ring
8   ce 00 11 09 e7 1a 31 04  00 21 00 04 08 40 00 00   bit2 : attaque saw → sustain triangle
9   00 03 81 04 07 1b 60 00  00 43 00 02 09 fc 00 00   bruit
10  00 03 81 05 47 1b 60 00  00 43 00 02 0a fc 00 00
11  00 03 21 06 58 1b 60 44  55 81 00 04 0b fc 00 00   bit2+bit6
12  00 03 21 06 38 1b 60 00  55 81 00 02 0c fc 00 00
13  00 00 15 00 b7 2a 04 00  00 43 c0 0c 0d 31 00 ff   +10 = c0 → arpège d'OCTAVE par défaut
14  80 01 41 0a c4 2b 10 20  5f 89 00 02 0e fe 00 00   = #4 mais slot filtre 14 (résonance 15)
15  00 00 00 00 00 00 00 00  00 00 00 11 00 02 0f fd
```

Deux enseignements immédiats :

* `+12` vaut l'index de l'instrument dans **tous** les records — convention de l'outil de
  composition (un slot de filtre par instrument), pas une contrainte du player.
* l'instrument 13 prouve que **l'arpège d'octave de `InstrRobSid` n'est qu'un cas du champ +10**
  (`0xC0` = phase 1 → +12 demi-tons, quartet bas nul → cycle à 2 pas).

---

## 4. `+7` — les flags d'instrument

Bits testés par le player (`lh_instr_flags`) :

| bit | masque | effet | code |
|-----|--------|-------|------|
| 0 | `0x01` | **moteur wavetable** (§6). Désactive aussi la transposition de piste sur la note. | `1681`, `126e` |
| 2 | `0x04` | **attaque waveform bi-phase** | `lh_instrflag_fx $1715` |
| 4 | `0x10` | **arpège par table** (portée instrument, phase globale) | `$1750` |
| 5 | `0x20` | **filtre actif** : charge le slot `+12`, route la voix dans `d417` | `131a`, `15b1` |
| 6 | `0x40` | **attaque de hauteur bi-phase** | `$1781` |

Bits 1, 3, 7 : jamais testés en G2.

**Combinaisons réellement présentes** — dump des 96 records (`+7`) des 6 tunes G2 :

```
Lion_Heart        01 01 01 44 20 00 00 04 04 00 00 44 00 00 20 00
Sun_Never_Shines  01 01 21 00 00 64 00 00 64 00 04 44 20 00 24 00
Pacific_Coast     01 01 01 00 04 44 64 00 44 00 00 04 04 00 24 00
Radio_ACE         01 01 01 04 04 44 44 44 24 00 04 44 00 00 00 00
Go_Go_Dash        01 44 01 00 04 44 01 44 64 00 00 04 00 04 04 00
Lakers_vs_Celtics 01 01 01 44 44 00 44 44 44 04 00 44 00 00 04 00
```

Soit `{00, 01, 04, 20, 21, 24, 44, 64}` : bit0 n'est **jamais** combiné à bit2/bit6 (la wavetable
possède déjà `ctrl` et `freq`), et bit6 n'apparaît **jamais** sans bit2 — l'attaque de hauteur est
toujours accompagnée d'une attaque de waveform, donc en pratique le compteur `+11` est **toujours
décrémenté de 2 par frame** quand bit6 est posé.

### bit2 — attaque waveform bi-phase

Tant que `lh_attack_frames_ctr` (semé depuis `+11`) est non nul : `ctrl = record+9` et le compteur
décrémente. À zéro : `ctrl = record+2`. C'est exactement `TwoPhase { attack_ctrl, attack_frames }`
de `InstrRobSid`.

**Particularité non documentée jusqu'ici** : si le ctrl d'attaque a **bit3 (TEST)** posé, le player
*retire* TEST de l'octet écrit **et sort la voix du masque de routage du filtre** pendant la phase
d'attaque (`1723–173b`). Bit3 dans `+9` n'est donc pas un bit de registre : c'est un drapeau
« couper le filtre pendant l'attaque ».

### bit6 — attaque de hauteur bi-phase

Tant que le compteur est non nul : la voix joue la **note absolue** `record+8` (0..95), puis
retombe sur la note jouée. C'est `TwoPhase::attack_note`.

**bit2 et bit6 partagent le même compteur** et le décrémentent chacun : un instrument qui pose les
deux obtient une attaque de `record+11 / 2` frames. C'est le même quirk que celui déjà modélisé
dans `TwoPhase::attack_frames` pour sanxion — il est ici **structurel**, pas accidentel.

### bit4 — arpège par table

`fenêtre[0..2] = { 0, paires[2·(record+8)], paires[2·(record+8)+1] }`, puis
`freq = freqtab[note_base + fenêtre[phase]]` — ce qui **écrase** vibrato et portamento pour la
frame. Les offsets sont **non signés, vers le haut** (aucun clamp après l'addition : un offset
signé sortirait de la table).

> ⚠️ **bit4 est du code mort en G2 — vérifié, pas supposé.**
> (a) Le compteur de phase `lh_arptab_phase $1ae0` n'est **jamais écrit** (`get_xrefs_to` : une
> seule lecture, depuis `$1761`) ; le code étant identique dans les 6 tunes, c'est vrai partout.
> La phase est donc constante 0, et la fenêtre `[0]` (`$1add`, elle aussi jamais écrite, valeur 0)
> réduit le flag à « verrouiller la hauteur sur la note de base ».
> (b) **Aucun des 96 records des 6 tunes G2 ne pose bit4.**
>
> ⇒ `InstrRobSid2` **n'a pas à modéliser bit4**. Le vestige est intéressant pour dater le player
> (l'arpège par table de `ArpMode::ThreeNote` des v20/v25 survit ici sans emploi, remplacé par le
> champ `+10` / le marqueur `0x83`), pas pour l'import.

---

## 5. Les marqueurs par-note

Lus en tête d'entrée de pattern, avant l'octet de longueur, par la boucle `lh_marker_loop $116c`.
Ils écrivent l'état de voix **avant** le note-on, et le note-on consulte un drapeau « marqueur vu »
pour savoir s'il doit ou non écraser le champ depuis le record.

| marqueur | opérandes | effet | ce qu'il **empêche** au note-on |
|----------|-----------|-------|-------------------------------|
| `0x80 N` | 1 | sélectionne l'instrument `N & 0x0F` | — |
| `0x82 hi lo` | **2, poids fort d'abord** | portamento : pas 16 bits signé accumulé chaque frame | — |
| `0x83 N` | 1 | arpège par-note : phases `+0`, `+(N>>4)`, `+(N&0x0F)` demi-tons **vers le haut** | le chargement de `record+10` |
| `0x84` | 0 | **filter-hold** : garde le balayage de cutoff en cours | le rechargement du slot `+12` |
| `0x86 N` | 1 | vibrato : amplitude `(N&0x78)>>3`, shift `N&7` | le chargement de `record+5` **et** `+14` |
| `0x87 hi lo` | **2, poids fort d'abord** | force le cutoff de départ de cette note | le seed cutoff du slot (le reste du slot est chargé) |
| `0x88` | 0 | **pw-hold** : garde la largeur d'impulsion courante | l'écriture de `record+0/+1` dans `d402/d403` |
| `0x89 N` | 1 | **global** : quelle voix pilote le filtre de la puce | — |

Deux corrections importantes par rapport à `V40_PLAYER_STUDY.md` : l'**ordre des opérandes** de
`0x82` et `0x87` est **poids fort en premier** (`STA lh_porta_step_hi` puis
`STA lh_porta_step_lo`, `1211–121d` ; idem `11b0–11bc`), et **`0x88` n'est pas un legato** — le
legato est le bit5 de l'octet de longueur (§7). `0x88` gèle la pulse-width.

**Portée.** L'accumulateur de portamento, ses drapeaux et l'accumulateur de pitch de la wavetable
sont remis à zéro à **chaque nouvelle entrée** (`1149–1170`) : un marqueur ne survit pas à la note
suivante, il doit être réémis. Le paramètre de vibrato et d'arpège, eux, persistent dans l'état de
voix jusqu'au prochain note-on non lié.

---

## 6. Le moteur wavetable (`lh_wave_engine $1699`, flag bit0)

Table pointée par `record+8/+9`. Curseur par voix `lh_wave_index`, remis à 0 au note-on.
**Une commande consommée par frame** :

| commande | octets | action | avance |
|----------|--------|--------|--------|
| **TONE** `0x00–0x7F` | 3 | `ctrl = cmd` ; `acc_pitch += i16(t+1 lo, t+2 hi)` ; `freq -= acc_pitch` | +3 |
| **NOISE** autre `≥0x80` | 2 | `ctrl = cmd` (bit bruit/test posé) ; `freq_hi = t+1` **absolu** | +2 |
| **SUSTAIN** `0x85` | 1 | gèle : ne fait rien, n'avance pas | 0 |
| **LOOP** `0x86 N` | 2 | saute à la commande `N` (re-parcours depuis 0) | → N |

* La fréquence est **relative à la note** : chaque frame `freq` est resemée depuis
  `freqtab[note] + portamento`, puis le moteur lui **soustrait** l'accumulateur `acc_pitch`.
  L'accumulateur ne grandit que quand une commande TONE s'exécute — c'est un *programme de
  hauteur*, pas une rampe continue.
* **Asymétrie du gate** : la branche NOISE écrit `d404`/`d401` elle-même et saute l'écriture
  commune — le masque de gate-off de fin de note **ne s'applique pas** sur une frame NOISE. TONE et
  SUSTAIN passent par l'écriture masquée. À reproduire tel quel.
* Le moteur ne borne pas la cible de `LOOP` (données écrites à la main). L'importeur doit clamper.

Tables réelles de Lion_Heart (aucun `LOOP`, terminateur `SUSTAIN` partout) :

```
instr 0 @$1c4b : 81 20 | 41 01 04 | 40 40 02 | 80 30 | 80 15 | 80 20 | 80 10 | 80 20 | 85
instr 1 @$1c5e : c0 01 10 | 80 00 | 40 00 01 | 40 80 01 | 40 00 02 | 40 80 02 | 85
instr 2 @$1c71 : 81 5f | 41 5a 01 | 41 52 04 | 80 5f | 81 5f | 40 80 01 | 80 20 | 80 20 | 80 10 | 80 20 | 85
```

---

## 7. Enveloppe, gate, tie, release

* **Gate-off de fin de note.** `lh_ctrl_mask` vaut `0xFF` ; il passe à `0xFE` (AND qui efface le
  bit gate) sur la **dernière row** de la note, sauf si la note est liée. Un octet-note `0x60`
  (silence) fait la même chose et **ne touche à rien d'autre** : ni ADSR, ni PW, ni patch.
* **TIE = bit5 de l'octet de longueur de la note PRÉCÉDENTE** (`12a9: LDA lh_prev_len_byte AND #$20`).
  Quand il est posé, le note-on est intégralement sauté : pas de rechargement PW/ADSR/vibrato/
  filtre, pas de remise à zéro de la phase d'arpège, pas de re-gate. Seule la hauteur change.
* **Rampe de release.** `lh_rows_to_frames` convertit la longueur de la note en frames et pose
  `lh_release_countdown = max(frames − 2, 0)` (0 si la note fait moins de 7 frames). À expiration,
  le player force `d405 = 0x0F`, `d406 = 0x01` : la note s'éteint **2 frames avant sa fin**.
  Supprimé si la note est liée, ou si l'entrée suivante est un silence ou la fin du pattern
  (`lh_next_is_rest_or_end`).

C'est le pendant, plus élaboré, de `ReleaseMode::HardCut` (v10/v15).

---

## 8. Vibrato, PW, portamento, filtre — la mathématique exacte

### Vibrato (`lh_fx_vibrato $147b`)

Paramètre `V` (record+5 ou marqueur `0x86`), délai `D` (record+14) :

```
amplitude R = (V & 0x78) >> 3        // 0..15 pas
shift     S =  V & 7
pas       = (freqtab[n] - freqtab[n-1]) >> S      // un demi-ton à la note courante, décalé
position  = triangle 0..R (±1 par frame, inversion aux bornes)
freq      = freqtab[n] - pas·(R>>1) + pas·position + accumulateur_portamento
```

Trois détails à ne pas perdre :

1. tant que `D` n'est pas écoulé, l'étage entier est sauté (la fréquence reste `note + porta`) ;
2. le terme `+ pas·position` n'est appliqué qu'à partir de la **4ᵉ frame après le note-on**
   (`lh_frames_since_noteon >= 4`) — mais le recentrage `− pas·(R>>1)` **s'applique dès la première
   frame** : l'attaque est donc jouée *un demi-swing en dessous* de la note ;
3. l'unité est le **demi-ton à la note courante**, pas un pas de registre constant. C'est
   exactement `InterpVibrato { half_depth, shift }` de `InstrRobSid`, plus le délai.

> ⚠️ **Pas de diviseur de vitesse.** La position avance de ±1 par frame : la période du triangle
> vaut exactement `2·R` frames. Amplitude et vitesse sont donc **couplées** — on ne peut pas
> ralentir le vibrato sans l'élargir. Le `Vibrato { depth, speed_div }` des v10–v30, où les deux
> sont indépendants, n'est pas reproductible ici (cf. §11.2).

### Balayage de pulse-width (`lh_fx_pw_sweep $154a`)

Vitesse `record+6`, bornes `record+13` (quartet haut = sup, bas = inf), direction par voix.
Montée : `pw += vitesse` ; si le **quartet haut** du PW **est égal** à la borne sup → inversion.
Descente : `pw -= vitesse` ; égalité avec la borne inf → inversion. Écrit `d402/d403` chaque frame
dès que la vitesse est non nulle, **quelle que soit la waveform**.

> Le test est une **égalité** sur le quartet, pas une comparaison. Un pas qui saute la borne fait
> déborder le balayage — comportement observable, à reproduire, pas à « corriger ».

### Portamento (`lh_fx_portamento $1663`)

`acc += pas_16bits` chaque frame ; `freq = freqtab[note] + acc`. Le pas est signé (bit7 du poids
fort). Glissement linéaire dans l'espace **fréquence-registre**, pas période.

### Filtre (§ slots)

Slot de 8 octets, `lh_filter_slots $1cb0 + 8·(record+12)` :

| off | champ |
|-----|-------|
| +0 / +1 | cutoff de départ, **hi / lo** (le hi est écrasé par le marqueur `0x87`) |
| +2 / +3 | pas de balayage, **hi / lo** |
| +4 | borne **supérieure** (comparée sur l'octet haut) |
| +5 | borne **inférieure** |
| +6 | bit7 = direction initiale (posé = montée) ; bits 0-2 = **mode** (LP/BP/HP), décalé de 4 vers le quartet haut de `d418` |
| +7 | quartet haut = **résonance** |

Le cutoff est un accumulateur 16 bits qui **rebondit** entre les deux bornes, exactement comme le
PW. Écriture puce, une fois par frame, à la fin de `lh_play` :

```
d416 = cutoff_hi[voix_sélectionnée]              // voix choisie par le marqueur 0x89
d415 = (cutoff_lo[voix_sélectionnée] >> 5) & 0x0F
d417 = masque_de_routage | résonance[voix_sélectionnée]
d418 = mode[voix_sélectionnée] | 0x0F
```

Le **masque de routage** n'est pas un champ d'instrument : c'est l'OR des bits de voix (1/2/4) de
toutes les voix dont l'instrument a le flag bit5. Si aucune voix n'est routée, `d417 = 0`.

**La piste 3** est une timeline d'automation du filtre : même format d'entrée, mais seuls `0x89`
(sélection de voix) et l'octet de longueur sont utiles. Son marqueur `0x80` charge 5 paramètres
dans une routine **sans aucun appelant** dans le binaire (elle suit un `RTS`) : morte dans le
chemin PSID, un déclencheur du jeu. L'importeur peut l'ignorer.

---

## 9. Ce que la génération antérieure (G1) permet de dater

`Off_the_Cuff.bin` (annoté) montre l'état antérieur, ce qui confirme le sens de l'évolution :

| axe | G1 (Off_the_Cuff) | G2 (Lion_Heart) |
|-----|-------------------|-----------------|
| marqueurs | 3 (`80` instr, `82` porta, `83` **vibrato**) | 8, dont arpège, filtre, holds |
| fréquence | registre **courant** (porta et wavetable modifient en place) | recalculée `freqtab[note] + acc` chaque frame |
| transposition d'ordre | octet à bit7 posé (`AND #$7F`) | `0xFD N` |
| filtre | `+14` = valeur `d417` statique, `+15` = incrément de cutoff — une **rampe simple** | slots de 8 octets, balayage à rebond, override par note, sélection de voix |
| flag bit7 | hack percussion voix 3 en dur (`d40f/d412/d416/d418`) | absent |
| record | 16 octets, `+0..+13` **identiques** | idem |

Le cœur du record (`+0..+13`) et les flags 0/2/4/5/6 sont donc **stables sur toute la famille**.
Ce qui change entre G1 et G2, c'est le filtre et la richesse du vocabulaire par-note.

Point notable : le filtre de G1 (`enable`, résonance+routage statiques, incrément de cutoff) est
*précisément* la structure `Filter` de `InstrRobSid` aujourd'hui. G2 la remplace par un modèle
strictement plus riche.

---

## 10. Corrections apportées à `V40_PLAYER_STUDY.md`

| §  | affirmation de l'ancien doc | corrigé |
|----|------------------------------|---------|
| §3 | `+2` = « gate-off / no-wavetable ctrl » | `+2` = **ctrl de sustain**, l'état stable de `d404` |
| §3 | `+5` « vibrato params », `+14` « vibrato param » | `+5` = paramètre (amplitude+shift), `+14` = **délai** |
| §3 | `+7` : « bit0 wavetable, bit5 filtre » | + **bit2** attaque waveform, **bit4** arpège table, **bit6** attaque pitch |
| §3 | `+10`, `+15` « auxiliaires » | `+10` = **arpège par défaut** ; `+15` réellement inutilisé |
| §3 | `+11` « sustain ctrl » | `+11` = **durée d'attaque en frames** (partagée bit2/bit6) |
| §4 | `0x82 lo hi`, `0x87 lo hi` | **`hi` en premier** dans les deux cas |
| §4 | `0x88` = « legato / tie » | `0x88` = **pw-hold** ; le tie est le bit5 de l'octet de longueur |
| §6 | slot `+6` « direction(bit7)/routing(bits0-2) » | bits 0-2 = **mode de filtre** (`d418`), pas le routage ; le routage est dérivé des voix actives |
| §1 | « Lakers_vs_Celtics : variante distincte, 12,7 % identique » | **même player**, bloc RAM relogé |
| §0 | wavetable : « les 6 tunes riches ADDITIONNENT » | Lion_Heart **soustrait** l'accumulateur (`16f3: SEC / SBC`) |

---

## 11. Conséquences pour le modèle

> **DÉCISION (2026-07-25) : pas de type `InstrRobSid2`.** Le plan initial était de créer un second
> type portant le dernier player. L'analyse ci-dessous montre qu'un modèle unique couvre toutes les
> générations pour moins cher qu'un second type : on fait donc **évoluer `InstrRobSid` en place**.
> Le suffixe « 2 » n'aurait eu de sens que si deux types coexistaient. Le présent fichier garde son
> nom pour ne pas casser les liens ; lire « `InstrRobSid` (évolué) » partout où il dit
> « `InstrRobSid2` ».

### 11.1 Le changement de forme

`InstrRobSid` est un **sac plat par instrument**. Le dernier player sépare nettement :

* le **patch** = les 16 octets + son programme (wavetable / attaque / arpège / slot de filtre) ;
* les **surcharges par note** = les marqueurs, qui *inhibent sélectivement* des morceaux du
  note-on (`0x86` inhibe le vibrato du patch, `0x87` le seed de cutoff, `0x88` la PW, `0x84` le
  slot entier) ;
* l'**état de voix** qui persiste ou non selon TIE.

⚠️ **Les surcharges par note ne vont PAS dans l'instrument.** Un marqueur est une propriété de la
*note*, pas du patch : il se pose sur la cellule, via le système d'effets/lanes du DAW
(`V40_PLAYER_STUDY.md` §8, option B). `InstrRobSid` reste donc un instrument ; ce qu'il gagne,
c'est que ses paramètres deviennent *surchargeables* — un concept qui vit dans le player, pas dans
le struct. Les « holds » ne sont pas des quirks : ce sont des **primitives musicales** (« ne
réamorce pas ce paramètre à cette note »), exactement le genre de reformulation que vise
`INSTR_ROBSID_CURATION_PLAN.md`.

### 11.2 Ce que G2 englobe de `InstrRobSid` — trois niveaux, pas deux

La question qui compte n'est pas « G2 sait-il faire ce son ? » mais « l'importeur peut-il
**transformer automatiquement** un instrument v10–v30 en instrument G2, sans changer un seul
registre écrit ? ». Les trois niveaux ci-dessous répondent à cette question-là.

**Niveau A — reprise 1:1, exacte au registre.** Le champ existe des deux côtés avec la même
mathématique ; seul le site de liage change.

| champ actuel | équivalent G2 |
|--------------|---------------|
| `InterpVibrato { half_depth, shift }` | record+5, **même calcul** ; G2 ajoute un `delay` (record+14) et le décentrage des 4 premières frames |
| `TwoPhase { attack_ctrl, attack_frames, attack_note }` | flags bit2/bit6 + record+9/+8/+11, **y compris** le partage du compteur (÷2) |
| `PulseSweep { speed, bounce }` | record+6 / record+13, même rebond |
| `ArpMode::Octave` | `+10 = 0xC0` — prouvé par l'instrument 13 de Lion_Heart |
| `ArpMode::TwoNoteInterval(n)` | `+10` à quartet bas nul (cycle à 2 pas) |
| `ArpMode::ThreeNote` (offsets non signés) | `+10` / marqueur `0x83` |
| `Filter { enable, resonance, cutoff_step, cutoff_seed, reseed_each_note }` | slot de 8 octets : **sur-ensemble** (+ bornes/rebond, + mode LP/BP/HP, + override par note, + hold, + sélection globale). Seul `routing` n'a pas d'équivalent libre : en G2 il est **dérivé** des voix dont le flag bit5 est posé |

**Niveau B — exprimable en G2, mais seulement par RÉ-AUTHORING.** Le son existe dans le
vocabulaire G2, mais il faut *écrire un programme* (une table wavetable, un ctrl d'attaque) : ce
n'est pas une transformation mécanique de champs, et le résultat ne serait pas garanti
bit-identique. **Pour l'importeur, ces primitives doivent donc rester dans le modèle.**

| champ actuel | comment G2 l'écrirait | pourquoi ce n'est pas automatisable |
|--------------|-----------------------|-------------------------------------|
| `Drum { no_freq_slide }` | wavetable `NOISE → TONE`, ou attaque bit2 avec ctrl bruit (**l'instrument 3 de Lion_Heart est exactement une caisse écrite ainsi**) | la pente de freq-hi du drum v10 n'a pas de codage direct ; il faudrait la déplier en commandes |
| `Skydive { length_gate, mode }` | portamento `0x82`, ou programme wavetable | le gate sur longueur de note et le mode « climb » n'existent pas ; il faudrait générer une table par longueur de note |
| `WaveAlt { alt_ctrl }` | wavetable à 2 pas + `LOOP` | chaque pas TONE de G2 accumule aussi la hauteur : une alternance pure demande des pas à step nul, donc une table forgée |
| `ReleaseMode::HardCut` | rampe de release automatique à −2 frames (§7) | mécanisme **différent** (G2 l'impose à toutes les notes ≥ 7 frames, avec ses inhibitions) : même intention, pas même sortie registre |

**Niveau C — non exprimable, point.**

| champ actuel | pourquoi |
|--------------|----------|
| `Vibrato { depth, speed_div, length_gate }` | double blocage : (1) l'unité est le **pas de registre brut**, celle de G2 le **demi-ton à la note courante** — la profondeur ne suit pas la même loi selon la hauteur ; (2) G2 **n'a pas de diviseur de vitesse**, la période vaut `2·amplitude` frames, donc profondeur et vitesse sont couplées (cf. §8). `length_gate`, lui, se rapproche du `delay` de G2 |
| `ArpMode::TwoNoteFixed(u16)` | une **fréquence registre absolue** hors grille de notes ; G2 n'exprime que des offsets en demi-tons (et son attaque bit6 ne prend qu'un **index de note**, pas une fréquence) |
| `PulseSweep { delay, low_byte_mode }` | G2 avance d'un pas par frame (pas de diviseur) et travaille toujours sur 12 bits avec rebond ; le mode « octet bas seul à débordement 8 bits » du v10 n'a pas de codage |
| `ReplayerCompat::vib_overflow_reg` | quirk de note débordante propre au v10, sans objet |

**Bilan.** Un `InstrRobSid2` **restreint au vocabulaire G2 ne peut pas remplacer `InstrRobSid`** :
il perdrait le niveau C (3 champs) et exigerait de ré-authorer le niveau B (4 champs) — donc de
casser la garantie bit-identique sur commando / zoids / monty / sanxion / lightforce / delta.
C'est la même conclusion que `V40_PLAYER_STUDY.md` §7.1 (« on ne ré-authore PAS »), ici vérifiée
champ par champ.

### 11.2 bis — la conséquence de conception

Il ne faut donc **pas** transcrire le player G2 dans un type. **La forme de G2 est la bonne cible,
son vocabulaire ne l'est pas.** Les 7 champs des niveaux B et C ne sont pas des scories à
éliminer : ce sont les primitives d'un modèle qui doit couvrir *toutes* les générations — ce que
`INSTR_ROBSID_CURATION_PLAN.md` appelle sortir les quirks vers l'import tout en gardant les effets
musicaux dans le modèle.

Une seule question de conception reste ouverte à ce stade : sous quelle forme faire cohabiter les
deux vocabulaires dans un type unique. Voir §11.2 ter.

### 11.2 ter — Enums sur l'axe de l'EFFET, jamais sur l'axe de la VERSION

**Rejeté : `enum InstrRobSid { V1(…), V2(…) }`.** Un enum de version classe les instruments selon
*d'où ils viennent*, c'est-à-dire précisément l'axe que la curation cherche à faire disparaître du
cœur (un trait qui découle de la version importée se résout à l'import). Trois objections
concrètes :

1. **Ce n'est pas deux versions** : v10, v15, v20/v25, v30, G1, G2 — au moins six. Où va le
   `Drum { no_freq_slide }` propre au v15 ? Où va G1, dont le filtre est celui d'aujourd'hui mais
   le reste celui de G2 ?
2. **L'IHM devient incohérente** : un musicien ne se dit jamais « je veux un instrument v20 ». Il
   devrait choisir une version avant même de voir un paramètre.
3. **Le recouvrement est énorme** : tout le niveau A est commun. Un enum de version le duplique,
   ou le factorise en sous-structs partagés — et l'on retombe sur la solution ci-dessous avec de la
   cérémonie en plus.

**Retenu : trois enums, un par point de conflit réel.** Une voix n'a qu'un comportement à la fois
sur chacun de ces axes, donc l'enum y est *musicalement* juste — et c'est déjà l'idiome du fichier
(`ArpMode`, `SkydiveMode`).

```rust
enum VibratoMode {
    None,
    /// v10/v20/v25 : triangle MONTANT (`oscilatval` 0,1,2,3,3,2,1,0 sur 8 frames
    /// du compteur global), excursion en pas de registre ; `reg_step` = l'unité
    /// « registre fixe » de la note débordante (ex-`ReplayerCompat`).
    Upward { depth: u8, reg_step: u16 },
    /// v30 : triangle CENTRÉ de période 2·depth frames, pas = demi-ton >> div,
    /// toujours gaté sur la longueur de note.
    Bipolar { depth: u8, div: u8 },
    /// v15 « interp », Thrust, et G2 : triangle centré en DEMI-TONS à la note
    /// courante (constant en cents). G2 y ajoute délai + attaque plate 4 frames.
    Semitone { half_depth: u8, shift: u8 },
}

enum ReleaseMode {
    Gate,       // défaut : couper le gate, la note s'éteint sur son release
    HardCut,    // v10/v15 : zéroter AD+SR au note-off, la note coupe net
    // (+ `Ramp { lead_frames, ad, sr }` quand le moteur G2 saura la jouer)
}

enum CutoffSweep {
    Static,                                           // cutoff figé
    Wrap { step: i8 },                                // v10–v30 : octet SIGNÉ ajouté à l'octet
                                                      //  HAUT, débordement 8 bits, pas de demi-tour
    Bounce { step: u16, min: u8, max: u8, up: bool },  // G2 : accumulateur 16 bits qui REBONDIT
}

enum ArpMode {
    None,
    /// Doublement EXACT de fréquence une frame sur deux (v10 `instrfx & 4`).
    Octave,
    /// Unifie TwoNoteInterval [−n,0], ThreeNote [0,a,b] et le quartet G2
    /// [0,hi,lo] : une petite liste cyclique d'offsets en demi-tons.
    Cycle { steps: [i8; 3], len: u8, phase: ArpPhase, follow_climb: bool },
    /// v30 seulement : fréquence registre ABSOLUE, hors grille de notes.
    TwoNoteFixed(u16),
}
enum ArpPhase { Global, PerNote }   // v20/v25 = Global (+ offsets signés), G2 = PerNote
```

`ArpPhase` est le point sensible : la mathématique est identique des deux côtés, seule la
sémantique de phase change — `V40_PLAYER_STUDY.md` §7.1 avait déjà identifié que c'est là, et pas
dans le calcul, que se cachent les régressions.

> **`Octave` ne peut PAS être un `Cycle` de pas `+12`** — constaté à l'implémentation. Le
> replayer lit une table de fréquences doublée à l'octave, donc le rapport est exactement 2,
> alors que `Cycle` décale par demi-tons tempérés (`shift_semitones_up`, multiplication Q16
> répétée qui arrive ~0,02 % sous 2× au bout de douze pas). Les fusionner changerait la sortie
> registre. C'est une différence de **noyau**, pas de paramétrage.

Tout le reste (balayage de PW, attaque bi-phase, filtre, wavetable, drum, skydive, wave-alt) est
**additif** : un instrument le porte ou non, aucun conflit, aucun enum nécessaire.

**Effet net : le modèle rétrécit.** Cinq variantes d'arpège deviennent quatre (et `Cycle` absorbe
gratuitement le cas G2 grâce à `phase`) ; les trois structs `Vibrato` + `InterpVibrato` +
`ReplayerCompat` (8 champs) deviennent **un enum à trois variantes**, et `ReplayerCompat`
disparaît entièrement — l'excursion en registre de la note débordante n'était pas un quirk de
compatibilité mais **la seconde unité de la même loi**, ce que l'enum exprime naturellement.

### 11.2 quater — Séquencement et état

Chaque étape est un refactor mécanique **bit-identique**, vérifiable sur les goldens existants
avant qu'une seule ligne d'import G2 n'existe.

| # | étape | état |
|---|-------|------|
| 1 | `ArpMode` → `Octave` + `Cycle { steps, len, phase, follow_climb }` + `TwoNoteFixed` | **fait** |
| 2 | `Vibrato` + `InterpVibrato` + `ReplayerCompat` → `VibratoMode` | **fait** |
| 3 | `hard_cut_release: bool` → `ReleaseMode` | **fait** |
| 4 | `Filter` → sur-ensemble G2 : `CutoffSweep{Static,Wrap,Bounce}`, `cutoff_seed: u16`, `mode` LP/BP/HP | **fait** |
| 5 | Programme de voix : modèle `VoiceProgram` + interpréteur `generators/sid/wave_program.rs` + câblage dans les deux drivers | **fait** |
| 6a | Décodeur G2 **côté instruments** : `tracker/import/sid/g2.rs` — détection, récupération des adresses de tables depuis le code, records 16 o → `InstrRobSid` (programme + filtre + arpège + attaques) | **fait** |
| 6b | Côté chanson : 4 listes d'ordre, patterns à marqueurs → cellules/lanes, émission du `Module` ; + `VibratoMode::Semitone{delay}` et `ReleaseMode::Ramp` avec le code qui les joue | à faire |

**Les adresses de tables ne sont PAS une config par tune.** Les cinq tunes non-Lakers partagent le
code octet pour octet — seuls diffèrent les opérandes d'adresse ; Lakers est le même player avec
tout son bloc de variables relogé. Le décodeur lit donc chaque adresse **dans l'instruction qui
l'utilise**, repérée par son motif d'opcode :

| table | motif | dérivation |
|-------|-------|------------|
| instruments | `BD ll hh 99 02 D4` (`LDA instr+0,X : STA $D402,Y`) | opérande |
| slots de filtre | `B9 ll hh 29 F0` (`LDA slot+7,Y : AND #$F0`) | opérande − 7 |
| table de fréquences | `B9 ll hh 18 7D` (`LDA freq,Y : CLC : ADC porta,X`) | opérande |
| pointeurs de patterns | `B9 ll hh 85 FA` (`LDA pp,Y : STA $FA`) | opérande |

Vérifié sur les 6 tunes : Lion_Heart `$1b4b/$1cb0/$1d50/$1e34`, Sun `$1b4b/$1cae/$1d4e/$1e0a`,
Pacific `$1b4b/$1ca8/$1d48/$1dd0`, Radio_ACE `$1b4b/$1cb0/$1d48/$1e34`, Go_Go_Dash (load `$1000`)
`$1b4b/$1cad/$1d2d/$1db7`, **Lakers `$1b73/$1cd7/$1d57/$1dcf`** — le bloc relogé est retrouvé sans
cas particulier.

Les étapes 1–4 ont une valeur propre pour la curation, indépendamment de G2.

**Règle tenue aux étapes 1–4** : ne pas introduire dans le modèle un champ que le moteur ne sait
pas encore jouer. C'est pourquoi `VibratoMode::Semitone` n'a pas encore son `delay` et
`ReleaseMode` pas encore sa `Ramp` : ils arrivent à l'étape 6, avec le code qui les exécute.
`CutoffSweep::Bounce`, lui, est implémenté dès l'étape 4 — mais comme aucun tune embarqué ne
l'utilise, le gate oracle ne peut pas le couvrir : il est épinglé par un test unitaire dédié
(`filter_bounce_law_reverses_at_both_bounds`), qui vérifie les deux rebonds, la remise à zéro de
l'octet bas au contact d'une borne, et le passage du quartet de mode jusqu'à `$D418`. **Toute
étape qui ajoute un chemin moteur non couvert par le corpus doit faire de même.**

**Le modèle du programme de voix (étape 5) est écrit pour l'ÉDITION, pas pour le replayer.** Le
format brut est un flux d'octets (`41 01 04`) ; le modèle est une liste de pas :

```rust
struct WaveStep { shape: WaveShape, pitch: PitchAction }
struct WaveShape { noise, pulse, sawtooth, triangle, test, ring, sync, gate }  // = champs SidVoice
enum PitchAction { Bend(i16), Fixed(u8) }
enum ProgramEnd  { Hold, Loop { step: u8 } }
```

Trois décisions dictées par l'usage : (1) `WaveShape` porte les **mêmes champs que `SidVoice`**,
pas un `u8` — une IHM réutilise le même widget pour le patch et pour un pas (aller-retour testé
sur les 256 octets) ; (2) `gate` est un champ du pas, parce qu'un pas qui le coupe **relâche la
note au milieu du programme** — c'est un geste musical, pas un détail de registre ; (3) `Bend`
**s'accumule**, ce que la doc du type dit explicitement : une suite de `Bend` est un glissando,
un `Bend` isolé un désaccord fixe — sans ça un utilisateur écrirait n'importe quoi.

Le programme vit sur `InstrRobSid { voice, program: Option<VoiceProgram>, fx }`, à côté du patch :
quand il est là **il possède le timbre**, ce qui rend `Drum` / `WaveAlt` / `TwoPhase` redondants
(documenté dans le type pour qu'une IHM les replie).

**Où le refactor a porté** : uniquement le *modèle* (`RobEffects`), la conversion
(`driver.rs::fx_from_robeffects`) et l'*importeur* (`one_sid.rs`). `SidFx` reste la forme neutre
aplatie du moteur, comme elle l'était déjà pour `Option<BounceRange>` → `(hi, lo, set)` : c'est le
point de flattening assumé de l'architecture, et le laisser tel quel garde le moteur hors du
périmètre de risque.

### 11.3 Ce que G2 ajoute et qui n'existe nulle part aujourd'hui

1. **Programme wavetable avec `LOOP`** — un vrai petit langage par instrument.
2. **Filtre balayé à rebond** (accumulateur 16 bits + bornes + direction + mode).
3. **Override de cutoff par note** (`0x87`) et **sélection globale de la voix filtrée** (`0x89`).
4. **Les « holds »** (`0x84` filtre, `0x88` PW) : ne pas réamorcer un paramètre à la note.
5. **Le tie** (bit5 de la longueur) qui saute *tout* le note-on.
6. **Rampe de release automatique** en fin de note, avec ses inhibitions contextuelles.
7. **Délai de vibrato** + le décentrage des 4 premières frames.
8. **Piste d'automation de filtre** séparée des voix.

### 11.4 Contraintes dures à respecter

* **16 instruments maximum** (index tronqué à 4 bits).
* `+8`/`+9` sont multiplexés par les flags → modéliser un enum de programme, pas deux `u8`.
* Les instruments à wavetable **ignorent la transposition de piste**.
* Le masque de gate ne s'applique **pas** sur une frame `NOISE` de wavetable.
* Le rebond de PW et de cutoff teste une **égalité** de borne (débordement possible et audible).
* Bit3 du ctrl d'attaque = « couper le filtre pendant l'attaque », pas le bit TEST du SID.

---

## 11.3 La surcharge par-note — conception (décision ouverte)

Dernier morceau du chantier, et la seule vraie décision de conception restante. **Mesurée avant
d'être tranchée** (`examples/g2_census.rs`, les six tunes G2, sous-morceau 0) :

| marqueur | occurrences | nature |
|----------|-------------|--------|
| `0x84` hold-filter | 2504 | *ne pas réamorcer* le slot de filtre sur cette note |
| `0x88` hold-pw | 1822 | *ne pas réamorcer* la largeur d'impulsion |
| `0x83` arpège | 1306 | remplace l'arpège de l'instrument, pour cette note |
| `0x87` cutoff | 642 | remplace le cutoff de départ |
| `0x82` portamento | 445 | glissando de hauteur — **pas d'équivalent instrument** |
| `0x86` vibrato | 94 | remplace le vibrato de l'instrument |
| `0x89` filter-voice | **10** | état **global**, ~1 fois par morceau |

Proportion de notes marquées : Lion_Heart 12 %, Lakers 8 %, mais Radio_ACE 27 %, Go_Go_Dash 29 %,
Pacific_Coast 51 %, Sun_Never_Shines **64 %**. Autrement dit : sur la moitié du corpus les
marqueurs ne sont pas un ornement, ils sont le morceau.

**Ce que la mesure élimine.** `0x89` n'apparaît que dix fois sur tout le corpus : quasi statique,
un point de lane au démarrage suffit — inutile de concevoir un mécanisme pour lui. Une fois
`0x89` sorti, les six autres marqueurs ont tous la même sémantique : *« pour cette note, cet
instrument se comporte autrement »*, appliquée au note-on.

**Les deux formes possibles.**

* **(A) Variantes d'instrument synthétisées à l'import.** Aucun changement du cœur. Coût mesuré :
  Lion_Heart passe de 16 à **41** instruments (couples instrument × valeurs de marqueurs
  distinctes). Correct — une fois `0x89` traité à part — mais l'éditeur affiche 41 patches
  quasi identiques au lieu de 16 avec une variation par note : **l'intention musicale est perdue**,
  ce qui est précisément ce que la curation cherche à éviter.
* **(B) Une surcharge portée par la note.** Fidèle à l'intention : la cellule porte
  `{arp, vibrato, cutoff, porta, hold_filter, hold_pw}`, le driver la fusionne par-dessus
  l'instrument au note-on. Coût : un ajout au cœur, et c'est là qu'est la question.

**Où la mettre en (B).** Trois emplacements, aucun gratuit :
1. `Cell.expression` (`NoteExpression`) — **non** : c'est de l'expression MPE universelle
   (pitch-bend, volume, pan). Un nibble d'arpège SID n'est pas un pitch-bend ; ce serait une
   erreur de catégorie.
2. `Cell.effects: Vec<TrackEffect>` — c'est le mécanisme par lequel une donnée par-cellule voyage
   déjà. Mais `TrackEffect` est un vocabulaire d'effets **agnostique du format** ; y ajouter une
   variante spécifique puce est une verrue assumée.
3. Une table annexe indexée par (piste, ligne) — sans changement du cœur, mais ce genre de table
   se désynchronise dès la première édition. Écarté.

### Ce que le modèle porte DÉJÀ — vérifié, contre une première recommandation trop large

Une première version de ce paragraphe recommandait **une variante `TrackEffect` à six champs**
(`arp, vibrato, cutoff, porta, hold_filter, hold_pw`). C'était trop demander : l'inventaire réel
de `TrackEffect` (`Arpeggio`, `ChannelVolume`, `Glissando`, `InstrumentFineTune`,
`InstrumentNewNoteAction`, envelopes, `InstrumentSampleOffset`, `InstrumentSurround`, `NoteCut`,
`NoteDelay`, `NoteFadeOut`, `NoteOff`, `NoteRetrig`, `Panning`, `Tremor`, `Volume`, `MidiMacro`)
montre que **deux des trois plus gros postes tiennent dans l'existant**.

* **Arpège (1306) — `TrackEffect::Arpeggio { half1, half2 }` a EXACTEMENT la forme du marqueur
  `0x83` :** `half1 = N>>4`, `half2 = N & 0x0F`. L'objection « ça empilerait une deuxième
  modulation par-dessus celle de l'instrument » **tombe** : dans le replayer, le marqueur par-note
  et le défaut d'instrument (`+10`) écrivent **le même emplacement** (`1a55`). Il suffit donc de
  sortir l'arpège de l'instrument (`ArpMode::None` à l'import G2) et de le poser systématiquement
  sur la cellule — défaut du record, surcharge du marqueur. Rien à empiler, rien à ajouter au cœur.
* **Portamento (445) — PAS dans `TrackEffect`** (il n'y a aucune variante de glissando de hauteur ;
  une version antérieure de ce document affirmait le contraire, c'était faux). Le bon chemin
  existe ailleurs et il est meilleur : l'importeur DW bake déjà sa modulation de hauteur par-frame
  en **lane `Points` sur `Pitch`**, ce qui est exactement la forme d'un pas 16 bits accumulé
  chaque frame.
* **`MidiMacro` / `Parametric`** est bien l'échappatoire générique par-cellule qui existe, mais
  avec la sémantique Zxx d'IT (substitution dans une chaîne routée vers un device). La détourner
  pour porter des booléens de note serait moins lisible qu'une variante typée, pas plus.

**Ce qui reste réellement à ajouter** se réduit donc aux **holds** (`0x84` + `0x88`, 4326
occurrences) et à la **surcharge de cutoff** (642) — le vibrato (94) pouvant attendre. Les holds
ne sont pas un effet : ce sont des modificateurs de *déclenchement* (« ne réamorce pas ce
paramètre »), et aucune variante existante ne l'exprime. C'est là, et seulement là, que la
question d'un ajout au cœur se pose.

**Ordre retenu** : arpège (existant) → portamento (lanes) → mesure à l'oracle → décider des holds
au vu du résidu.

### Le TIE rejoint la liste — résultat négatif mesuré

Première idée : encoder la note liée comme « note sans colonne instrument », l'idiome tracker du
lié. Le player en fait un `CellEvent::NoteOnGhost`, et `channel/sid.rs::tick0_sid` traduit
**ghost et note franche par le même `SidEdge::Trigger`** — donc chaque note liée ré-arme le gate
et invente une attaque d'enveloppe que la puce ne joue pas.

Correctif tenté : faire diverger le chemin ghost (retune sans ré-armer). **Le gate historique l'a
rejeté sans appel — 19 des 40 sous-chansons v10–v30 régressent** (`commando#1` gate 0→108,
`last_v8#0` 0→480, `monty#2` 0→402, `crazy_comets#1` 227→616…). Les importeurs v10–v30 encodent
leurs entrées de note-stream ordinaires **en ghosts et comptent sur leur déclenchement** :
la sémantique est porteuse, pas accidentelle. Revenu en arrière, gate re-vert.

⇒ **Le tie ne peut pas emprunter la forme du ghost.** Il lui faut sa propre forme de cellule, ce
qui le range avec les holds : ce ne sont pas des effets, ce sont des **modificateurs de
déclenchement** (« ne réamorce pas », « ne redéclenche pas »). Les traiter ensemble est
probablement la bonne unité de travail plutôt que trois ajouts séparés.

## 12. Points ouverts

Le système d'instruments lui-même est **clos** : chaque octet du record a un usage prouvé dans le
code, ou est prouvé inutilisé.

* `+15` : aucune lecture dans le binaire, et ses valeurs (`fc` sur l'instrument 1, `fd` sur le 11,
  `ff` sur le 13) sont **identiques d'une tune à l'autre** — c'est un résidu de la banque
  d'instruments par défaut de l'outil de composition, pas un champ.
* `+14` sur les instruments sans vibrato (`+5 = 0`) : valeurs résiduelles du même ordre, non lues.
* bit4 : clos, cf. §4 — dormant sur l'ensemble du corpus.

Reste hors périmètre « instruments », à traiter au moment de l'importeur :

1. Le **modèle de tempo** (`lh_tempo_ctr_a/b`, le compteur à 12 pas, le drapeau demi-vitesse
   `$2de2`) — nécessaire pour convertir rows → frames, donc pour la rampe de release.
2. Les 6 tunes de **G1** n'ont pas été comparées entre elles (leurs empreintes locales divergent :
   sous-variantes probables). Sans objet pour `InstrRobSid2`, à garder pour un éventuel support G1.
