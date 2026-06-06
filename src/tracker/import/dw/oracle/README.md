# DW Tier-2 fidelity oracle (Ghidra 68k emulation)

This directory holds the **frame-accurate Paula oracle** used by PLAN.md
Session A / Tier 2: the ground truth that the xmrsplayer per-tick trace
is diffed against to build `BASELINE.md`.

Rather than re-implement a David Whittaker replayer, the oracle **runs the
original 68000 replayer** (embedded in the `.dw` Raw-Binary image) inside
Ghidra's p-code emulator and records exactly what it writes to the Amiga
custom-chip audio registers, tick by tick. This is the highest-fidelity
oracle available short of real Amiga hardware.

## Files

| File | What |
|---|---|
| `DwPaulaOracle.java` | The Ghidra emulation script (canonical source; lives in `~/ghidra_scripts/`). |
| `corpus/*.csv` | **The single golden store** — 113 goldens, one per real `.dw` in `~/Music/dw` (whole-corpus A2 capture, 2000 ticks; some StopSong-truncated). Keyed by the real stem (`xenon2 (title).csv`, `bad company.csv`, …). Used by both `audit.py` (corpus-wide) and `diff_baseline.py --markdown` (the C1–C8 + ½-vol cluster scoreboard). |
| `capture_all.sh` | Headless-Ghidra batch that (re)builds `corpus/` (see below). |
| `known_args.tsv` | Per-module entry-point overrides (`stem⇥init⇥play⇥a3⇥f0`) for the batch. |
| `audit.py` / `diff_baseline.py` | Corpus-wide rank + per-pair / `--markdown` diff. |
| `KNOWN_GAPS.md` | Modules the batch can't capture (chip's challenge entry, SFX/non-song banks). |

One golden per cluster representative is enough for the C1–C8 scoreboard: the
replayer is shared across a cluster, so a cluster-mate diffing cleanly against its
representative's player-side model is covered. Notable reps: **StopSong-truncated**
3-voice modules — `archipelagos` (348 ticks `f0=0x630`, residual MAE 120/80/44 =
mid-track speed cmd 0x8a, PLAN #4), `sentinel` (111 ticks `f0=0x52c`, play body
`0x136` not stub `0x106`), `fright night` (233 ticks `f0=0x630`, MAE 1.0/2.4/3.5
near-exact); and the heavy-arp canonical `buffalo bills rodeo games` (111 arp
cmds, MAE 2–4) which validates the per-frame arpeggio Points lane at scale.

> History: there used to be a second copy of the cluster goldens at the oracle/
> root (1500-tick, underscore-named `xenon2_title.csv` etc.). Those were folded
> into `corpus/` (2000-tick supersets) on 2026-06-05 — corpus/ is now the only
> golden store. Player traces live in `/tmp/ptr/<stem>.csv` (ephemeral; rebuild
> via the `dw_oracle_trace` loop in `audit.py`'s header).

## What it does

1. `init_main` is emulated once (song setup + `sample_loader`).
2. `play_tick` is emulated `N` times — one call = one 50 Hz interrupt =
   one replayer "tick".
3. A `MemoryAccessFilter` records **every** write to
   `0xdff096` (DMACON) and `0xdff0a0..0xdff0df` (AUDxLC/LEN/PER/VOL),
   composing them into a continuous Paula state. (A single post-tick
   register read would lose intra-tick writes — the replayer writes
   DMACON once per triggering channel.)
4. After each tick it snapshots, per channel: the **DMA gate**
   (`1` = audible), **AUDxPER** (period word) and **AUDxVOL** (clamped to
   Paula's 0..64), plus the replayer's `playing` flag `DAT_000005f0`.

### CSV schema

```
tick,dma,c0g,c0per,c0vol,c1g,c1per,c1vol,c2g,c2per,c2vol,c3g,c3per,c3vol,f0
```

`dma` is the full DMACON enable mask (hex). `cNg` is channel N's gate.
A trailing `# stop@T` line marks an explicit Stop command (`f0`→0); most
DW songs loop forever and never emit it.

## Running it

Prerequisites (one-time, see "Environment gotchas" below):
* the `.dw` is loaded as **Raw Binary, `68000:BE:32`, base 0**;
* `GHIDRA_MCP_ALLOW_SCRIPTS=1` is in the Ghidra process environment;
* `~/ghidra_scripts` is a real enabled source bundle.

Copy `DwPaulaOracle.java` into `~/ghidra_scripts/`. **`run_ghidra_script`'s
`program` argument is ignored** — the script runs against Ghidra's *active*
program. Always `switch_program` to the target first, then run with NO calls in
between (a late-finishing background analysis of another program can steal the
active slot). Verify the `Program:` line in the console output is the one you
meant.

```
switch_program(program="xenon2 (title).dw")
run_ghidra_script(script_name="DwPaulaOracle.java",
                  args="0x66 0x1b6 1500 /tmp/xen_oracle.csv")
```

Args: `<initAddr> <playAddr> [ticks] [outCsv] [a3base] [f0addr] [maxStepsPerCall]`
(addresses `Long.decode`, so `0x..` hex):
* `a3base` — `-` (default) leaves A3 untouched (player sets its own base);
  otherwise a 32-bit two's-complement base set before init and every play call
  (bubble bobble needs `0xfffffbbe`).
* `f0addr` — `-` (default) disables stop-detection; pass an address only for a
  player whose playing flag goes 0 on Stop (xenon2 `0x5f0`). Pointing it at the
  wrong byte falsely truncates the trace.
* `maxStepsPerCall` — `0` (default) uses the fast `emu.run()` path; **any value
  > 0 switches to bounded single-step mode**, which physically cannot hang.
  ALWAYS probe an unvalidated entry stepped first (e.g. `… - - 300000`, 1 tick).
  Stepped mode is fast enough for full runs too (1500 ticks ≈ 1–6 s).

## Per-cluster entry points (all found & captured)

`init`/`play` addresses are **binary-specific**. Invoke as
`switch_program` then `run_ghidra_script(args="<init> <play> 1500 <out> <a3> <f0> <maxSteps>")`:

| Cluster | Rep | init | play | a3 | f0 | notes |
|---|---|---|---|---|---|---|
| C1 | xenon2 (title) | `0x66` | `0x1b6` | `-` | `0x5f0` | body sets own base |
| C2 | bad company | `0x0` | `0xe` | `-` | `-` | thin push/pop wrappers (init→`0x5e`, play→`0x1b6`) |
| C3 | bubble bobble | `0x0` | `0x1a8` | `0xfffffbbe` | `-` | A3 = -0x442 base; cmd dispatch `jmp (0,A3,A2)` |
| C4 | grimblood | `0x0` | `0x13e` | `-` | `-` | play does `lea -0x4c8,PC,A3`→-0x388 itself; 3-voice |
| C5 | tetris | `0x0` | `0x10e` | `-` | `-` | absolute addressing |
| C6 | qball | `0x0` | `0xd0` | `-` | `-` | old player; entry = the `movem` push, NOT Ghidra's `0xd4` |
| C7 | leviathan-deabs | `0x0` | `0xda` | `-` | `-` | old player; play `lea -0xdc,PC,A3`→0 |
| C8 | the empire strikes back | `0x0` | `0xe` | `-` | `-` | old player + thin wrappers (init→`0x62`, play→`0x1ae`) |
| — | obliterator-title | `0x0` | `0x10c` | `-` | `-` | half-volume; `0xdc` is only the DMA-start stub — use the 910-byte driver at `0x10c` |

How to find them on a fresh import (auto-analysis defines no functions for a raw
binary with no entry point):

* `search_byte_patterns "00 df f0 00"` → the `lea 0xdff000,A6` inside the
  4-channel Paula loop; the per-frame **play body** surrounds it.
* `search_byte_patterns "00 df f0 96"` → every DMACON write (init reset, stub,
  and the per-note restarts in the play body).
* Disassemble `0x0`: a **14-byte** function = thin wrapper (`movem; bsr body;
  movem; rts`) → call the wrapper. A **large** function = the init body itself
  (bubble-bobble family) → the play body is a separate host-called routine; find
  its `lea (..,PC),A3` prologue right after the silence/reset helper's `rts`.
* Beware two decoys: the **DMA-start stub** (checks the active flag, ORs DMACON,
  `rts` — writes no periods; obliterator `0xdc`, bubble bobble `0x146`) and the
  **silence/reset helper** (`lea ..,A3; sf flags; DMACON=0xf; zero volumes`).
  Neither is the per-frame driver.
* Validate by probing stepped (N=1) and eyeballing the CSV: sane Amiga periods
  (~120–2300), volumes ≤64, DMA building up.

## xenon2 reference findings (validated)

* Entry: `init_main`@`0x66`, `play_tick`@`0x1b6`. Stack scratch `0x300000`,
  return sentinel `0x00f00000`. 68k SP register is named `SP`.
* The intro builds voices up: DMA `0x201` (AUD0) from tick 1, `+AUD1`
  (`0x203`) at tick 78, `+AUD2+AUD3` (`0x20f`) at tick 104. The frequent
  1-tick `off→on` DMACON toggles are per-note DMA restarts.
* Gated-channel period ranges are all sane Amiga periods: ch0 179–455,
  ch1 157–671, ch2 179–506, ch3 183–506 (ch0 ≈ 7.8–19.8 kHz PAL). The
  large `0xFC78` / `vol 103` values seen on a channel are **pre-gate**
  scratch (DMA off) and are correctly ignored via the gate bit.
* AUDxVOL written by the replayer can exceed 64 (e.g. 72, 103); Paula
  clamps to 64, which the oracle replicates.

## Environment gotchas (cost real time the first run)

* **`Script execution disabled`** → set `GHIDRA_MCP_ALLOW_SCRIPTS=1` in
  the *Ghidra process* env (export then launch `ghidraRun` from the same
  shell; verify with `tr '\0' '\n' < /proc/<pid>/environ | grep GHIDRA`).
  A loopback UDS socket needs no auth token.
* **`GhidraPlaceholderBundle cannot be cast to GhidraSourceBundle`** →
  `~/ghidra_scripts` did not exist at Ghidra startup, so it was
  registered as a placeholder. The first MCP inline run creates it;
  **restart Ghidra** (dir now exists → real source bundle) or enable it
  in Window → Bundle Manager.
* **`Undefined register: A7`** → the 68k stack pointer is `SP`, not `A7`.
* **A bad entry address freezes Ghidra HARD (100% CPU, needs a kill+restart).**
  `emu.run()` does not poll the monitor between p-code steps, so a routine that
  never pops the RET sentinel loops forever; a wall-clock watchdog cannot
  interrupt it. This is why `maxStepsPerCall` exists: the bounded single-step
  loop is owned by the script and throws (`exceeded N instructions … PC=…`)
  instead of hanging. Probe every unvalidated entry stepped first.
* **Old-player entries can be mislabeled by Ghidra (`movem` prologue).** The
  old-player play routine may begin `movem.l {regs},-(SP)` then `lea (..,PC),A3`;
  Ghidra names the function at the `lea` (after the push). Entering there skips
  the push, so the closing `movem.l (SP)+,{regs}; rts` underflows the stack,
  consumes the RET sentinel into a register, and `rts` jumps to garbage → the
  100%-CPU freeze above. Use the `movem` address as the entry (qball: `0xd0`,
  not Ghidra's `0xd4`). C7/C8 here happened to have clean `lea`/wrapper entries.
* **`run_ghidra_script` ignores its `program` arg** — it runs against the active
  program. `switch_program` immediately before, and check the console `Program:`
  line. A late background analysis of a just-imported file can steal the slot.

## Tier 2 status — DONE (first baseline captured)

All nine cluster goldens are captured (table above) **and** diffed against
the player. The player-side trace is
`xmrsplayer/examples/dw_oracle_trace.rs` (uses the read-only
`XmrsPlayer::advance_trace_tick` + `channel_snapshots()` →
`ChannelSnapshot`); the diff is `diff_baseline.py`; results live in
`../BASELINE.md`.

**Whole-corpus capture (since 2026-06-05):** `capture_all.sh` runs the oracle
headless (`analyzeHeadless`) over every `~/Music/dw/*.dw`, trying candidate entry
points (`known_args.tsv` overrides for the 14 verified modules; otherwise the
heuristics from `examples/dw_oracle_args` — Paula-`lea` body / `0x0,0xe` wrapper /
`DwLayout` offsets) and keeping the first whose trace is sane. Output →
`corpus/*.csv` (111/120 captured; the 9 fails are SFX/non-song banks + 3 real
modules needing manual entry points). `audit.py` diffs every corpus golden vs its
player trace (`/tmp/ptr/*.csv`) and ranks by strike/gate/period. ⚠ never prefix a
shell line with `pkill -f analyzeHeadless` — `-f` self-matches the command line and
kills the parent shell (exit 144).

**CSV schema (since 2026-06-05):** `tick,dma,` then **4 fields** per channel
`cNg,cNper,cNvol,cNs,` then `f0`. The added `cNs` is the per-tick **struck**
flag — `1` when that channel re-attacked this tick (oracle: a DMACON 0→1 edge =
Paula sample reload; player: a `trigger_pitch` sample-reset). `diff_baseline.py`
windowed-matches the two strike timelines (±12 frames) and reports per channel
**spur** (player re-attacks with no oracle match = spurious re-trigger, e.g. the
cmd-0x83 tie bug) and **miss** (oracle re-attacks the player skipped). This is
the axis `gate/period/volume` is blind to — a held/tied note and a re-struck one
share gate=1, a continuous period and the same volume curve. `load()` still
accepts the old 3-field schema (strikes read as 0), but re-capture goldens to use
the strike diff. The oracle script (`DwPaulaOracle.java`) emits the new column;
re-run the captures (entry-point table above) after editing it.

Regenerate everything:

```sh
# 1. player traces — one per corpus golden, into /tmp/ptr/<stem>.csv
#    (same CSV schema, stem-keyed, 2000 ticks — matches corpus/ + audit.py)
cd xmrsplayer
BIN=target/release/examples/dw_oracle_trace
cargo build --release --no-default-features --features std,import_dw --example dw_oracle_trace
mkdir -p /tmp/ptr
for g in ../xmrs/src/tracker/import/dw/oracle/corpus/*.csv; do
  s=$(basename "$g" .csv); "$BIN" "$HOME/Music/dw/$s.dw" 2000 "/tmp/ptr/$s.csv"
done

# 2a. corpus-wide rank          2b. C1–C8 + ½-vol scoreboard
python3 ../xmrs/src/tracker/import/dw/oracle/audit.py
python3 ../xmrs/src/tracker/import/dw/oracle/diff_baseline.py --markdown
```

The headline regression metric is the **median signed period Δ** column.
Next work is the modeling backlog in `PLAN.md` (period-space for
C2/C3/C7/C8; constant volume offsets for C5/C8) — re-run the diff after
each change and update `BASELINE.md`.
