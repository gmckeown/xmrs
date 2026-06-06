# DW oracle — capture gaps (manual entry-point RE TODO)

Modules `capture_all.sh` could not auto-capture. SFX/non-song banks
(no play routine) are expected and need no golden. One real song remains:

| module | status | findings |
|--------|--------|----------|
| **chip's challenge** | TODO | 12 sub-songs; self-bases `A3=0xfffffffe` via `lea (-0x4ea,PC),A3` at **play=0x4e6** (the Paula `lea 0xdff000` is at 0x538). Init at detect's `0x3a4` **runs away** (watchdog kill) — the real init that sets the playing flag at abs `0x60` and loads samples is elsewhere / may need a sub-song `d0`. play=0x4e6 alone also produced no trace (flag never set). Needs a Ghidra session on the init + sub-song dispatch. |
| anarchy sfx, blood money sfx, spellbound sfx, feud fake | n/a | `dw_no_song` rejects / SFX banks — no playable song. |
| leviathan-sfx, xenon-sfx | n/a | `play=<none>` — SFX banks, no per-frame driver. |

To add a module: find init/play (see README "How to find them"), append a
`stem<TAB>init<TAB>play<TAB>a3<TAB>f0` row to `known_args.tsv`, re-run
`capture_all.sh <stem>`.
