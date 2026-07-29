//! Per-channel effect-memory bookkeeping.
//!
//! Tracker effects use a per-channel "memory" for empty parameters
//! — e.g. `H00` (XM vibrato with no args) reuses the last non-zero
//! Hxy on the channel. Resolving that at runtime would force the
//! player to model every memory register. Instead, this pass walks
//! every imported row and rewrites empty-parameter slots to their
//! actual values, leaving the player a fully-parameterised stream.
//!
//! Each format (MOD / XM / S3M / IT / SID) gets a [`MemoryType`]
//! discriminator picked by the importer at construction time.
/*
Managing Dynamic Memory Effects

Here we fill the shortcuts to 0 with the correct values
​​so as not to have to manage it in the player.

It is impossible to do this update perfectly.
Indeed, patterns can be with zero effects and called
with different initialization pre-patterns.
So the last call to the pattern will be the winner.


| **MEMORY 00**                | **MOD** | **XM** | **S3M** | **IT** |
| ---------------------------- | ------- | ------ | ------- | ------ |
| **Arpeggio**                 |         |        | GLOBAL  | TRUE   |
| **Panbrello**                |         | TRUE   | TRUE    | TRUE   |
| **PanningSlide**             |         | TRUE   | TRUE    | TRUE   |
| **PortamentoUp**             |         | TRUE   | GLOBAL  | TRUE   |
| **PortamentoDown**           |         | TRUE   | GLOBAL  | TRUE   |
| **PortamentoFineUp**         |         | TRUE   | GLOBAL  | TRUE   |
| **PortamentoFineDown**       |         | TRUE   | GLOBAL  | TRUE   |
| **PortamentoFineExtraUp**    |         | TRUE   | GLOBAL  | TRUE   |
| **PortamentoFineExtraDown**  |         | TRUE   | GLOBAL  | TRUE   |
| **NoteRetrigExtended**       |         | TRUE   | GLOBAL  | TRUE   |
| **TonePortamento**           | TRUE    | TRUE   | TRUE    | TRUE   |
| **Tremolo**                  | TRUE    | TRUE   | GLOBAL  | TRUE   |
| **Tremor**                   |         | TRUE   | GLOBAL  | TRUE   |
| **SampleOffset**             | TRUE    | TRUE   | TRUE    | TRUE   |
| **Vibrato**                  | TRUE    | TRUE   | TRUE    | TRUE   |
| **VibratoFine**              |         |        |         | TRUE   |
| **VolumeSlide0**             |         | TRUE   | GLOBAL  | TRUE   | 0 EA EB v8 v9
| **VolumeSlideN**             |         | TRUE   | GLOBAL  | TRUE   | N 5 6 A v6 v7
| **FxVolume Tone Portamento** |         | TRUE   |         | FX MEM |
| **FxVolume Vibrato Depth**   |         | TRUE   |         | FX MEM |
|                              |         |        |         |        |
| **Channel Volume Slide**     |         | TRUE   | TRUE    | TRUE   |
| **global Volume Slide**      |         | TRUE   | TRUE    | TRUE   |
| **TempoUp**                  |         |        | TRUE    | TRUE   |
| **TempoDown**                |         |        | TRUE    | TRUE   |

*/

use alloc::vec::Vec;

// Format-specific effect parsers. Each is pulled in only when its
// format importer is enabled. The MOD importer reuses XM's effect
// parser (`ModXmEffect`), so either feature brings it in.
#[cfg(feature = "import_it")]
use crate::tracker::import::it::it_effect::ItEffect;
#[cfg(feature = "import_s3m")]
use crate::tracker::import::s3m::s3m_effect::S3mEffect;
#[cfg(any(feature = "import_mod", feature = "import_xm"))]
use crate::tracker::import::xm::mod_xm_effect::ModXmEffect;

use super::effect::TrackImportEffect;
use super::patternslot::PatternSlot;
use super::unit::TrackImportUnit;
use crate::core::fixed::fixed::{Q15, Q8_8};
use crate::core::fixed::units::PitchDelta;
use crate::prelude::*;

/// Stash a [`Q15`] into the i16-raw effect-memory cache.
#[inline]
fn cache_q15(q: Q15) -> i16 {
    q.raw()
}

/// Inverse of [`cache_q15`].
#[inline]
fn cached_q15(slot: i16) -> Q15 {
    Q15::from_raw(slot)
}

/// Stash a [`Q8_8`] into the i16-raw cache.
#[inline]
fn cache_q8_8(q: Q8_8) -> i16 {
    q.raw()
}

/// Inverse of [`cache_q8_8`].
#[inline]
fn cached_q8_8(slot: i16) -> Q8_8 {
    Q8_8::from_raw(slot)
}

/// Stash a [`PitchDelta`] into the i16-raw cache.
#[inline]
fn cache_pitch_delta(p: PitchDelta) -> i16 {
    p.as_q8_8_i16()
}

/// Inverse of [`cache_pitch_delta`].
#[inline]
fn cached_pitch_delta(slot: i16) -> PitchDelta {
    PitchDelta::from_q8_8_i16(slot)
}

// Memory type
pub enum MemoryType {
    Mod,
    Xm,
    S3m,
    It,
}

// Memory Array index
const ARPEGGIO: usize = 0;
const PANBRELLO: usize = 1;
const PANNING_SLIDE: usize = 2;
const PORTAMENTO_UP: usize = 3;
const PORTAMENTO_DOWN: usize = 4;
const PORTAMENTO_FINE_UP: usize = 5;
const PORTAMENTO_FINE_DOWN: usize = 6;
const PORTAMENTO_FINE_EXTRA_UP: usize = 7;
const PORTAMENTO_FINE_EXTRA_DOWN: usize = 8;
const NOTE_RETRIG: usize = 9;
const TONE_PORTAMENTO: usize = 10;
const TREMOLO: usize = 11;
const TREMOR: usize = 12;
const SAMPLE_OFFSET: usize = 13;
const VIBRATO: usize = 14;
const VIBRATO_FINE: usize = 15;
const PITCH_VOLUME_SLIDE_0: usize = 16;
const PITCH_VOLUME_SLIDE_N: usize = 17;
const FX_VOLUME_TONE_PORTAMENTO: usize = 18;
const FX_VOLUME_VIBRATO_DEPTH: usize = 19;
const CHANNEL_VOLUME_SLIDE: usize = 20;
const GLOBAL_VOLUME_SLIDE: usize = 21;
const TEMPO_UP: usize = 22;
const TEMPO_DOWN: usize = 23;
const SAMPLE_OFFSET_ADD_HIGH: usize = 24;
const ARRAY_SIZE: usize = 25;

/// S3M masterflag-derived import flags, resolved at import time and
/// stored on `ImportMemory` (this module is `pub(crate)`; the type is
/// re-exported as `tracker::import::s3m::S3mImportFlags`).
///
/// This lives here — rather than under `import::s3m` — so the type is
/// compiled in every build that enables any importer, not just `import_s3m`.
/// It is a two-bool POD (≈2 bytes) whose sole job is to be settable on
/// `ImportMemory`; keeping the definition format-agnostic lets every
/// dispatch site live in one place and removes a dependency cycle
/// between the shared helper and the S3M importer.
///
/// Fields:
///
/// * `fastvolslide` (S3M masterflags bit 6) — ST3-style "fast volume
///   slide" quirk. When set, volume-slide effects (`Dxy`) apply their
///   delta on tick 0 as well, rather than only on the regular tick>0
///   cascade.
/// * `oldstvib` (masterflags bit 1) — ST2-compatibility vibrato. The
///   depth is scaled `>>4` instead of `>>5` (H/K), and `>>6` instead
///   of `>>7` (U fine vibrato) — i.e. twice as deep. The importer
///   pre-multiplies the emitted `Vibrato`/`VibratoFine` depth by 2.
#[derive(Debug, Clone, Copy, Default)]
pub struct S3mImportFlags {
    pub fastvolslide: bool,
    pub oldstvib: bool,
}

/// IT master flags resolved at import time, threaded through the
/// memory resolver so IT-specific quirks that affect effect memory
/// can be honoured during pattern unpack.
#[derive(Default, Copy, Clone, Debug)]
pub struct ItImportFlags {
    /// IT header flag bit 5: "Link Gxx's memory with Exx/Fxx". When
    /// ON, Gxx (tone portamento) and the pitch slides Exx/Fxx share
    /// a single "last slide value" register per channel — writing
    /// any of them updates the shared slot, and reading a zero
    /// param on any of them falls back to that shared slot.
    pub link_gxx_ef_memory: bool,
}

/// For compatibility between formats and to compensate for special case difficulties with memory management, a best effort analysis is performed during import to associate the exact value with each effect.
// Horrible hack to store values...use from left to right. First usable type wins.
pub struct ImportMemory {
    global: [(i16, i16, usize, usize); ARRAY_SIZE], // s3m way: shared across all channels
    channel: [[(i16, i16, usize, usize); ARRAY_SIZE]; 64], // mod, xm, it way
    /// S3M-specific masterflags resolved at import time. Only read
    /// when `mem` is `MemoryType::S3m`; importers of other formats
    /// leave it at its default (all-zero). Configured by the S3M
    /// importer before calling [`Self::unpack_patterns`].
    pub s3m_flags: S3mImportFlags,
    /// IT-specific masterflags resolved at import time. Only read
    /// when `mem` is `MemoryType::It`. Configured by the IT
    /// importer before calling [`Self::unpack_patterns`].
    pub it_flags: ItImportFlags,
}

impl Default for ImportMemory {
    fn default() -> Self {
        Self {
            global: [(0, 0, 0, 0); ARRAY_SIZE],
            channel: [[(0, 0, 0, 0); ARRAY_SIZE]; 64],
            s3m_flags: S3mImportFlags::default(),
            it_flags: ItImportFlags::default(),
        }
    }
}

impl ImportMemory {
    fn update_memory(&mut self, mem: &MemoryType, index: usize, tiu: &mut TrackImportUnit) {
        tiu.effects.iter_mut().for_each(|e| {
            match e {
                TrackImportEffect::Arpeggio(a, b) => {
                    match mem {
                        MemoryType::Mod | MemoryType::Xm => {
                            // No memory use
                        }
                        MemoryType::It => {
                            if *a == 0 {
                                *a = self.channel[index][ARPEGGIO].2
                            } else {
                                self.channel[index][ARPEGGIO].2 = *a;
                            }
                            // second arg
                            if *b == 0 {
                                *b = self.channel[index][ARPEGGIO].3
                            } else {
                                self.channel[index][ARPEGGIO].3 = *b;
                            }
                        }
                        MemoryType::S3m => {
                            if *a == 0 {
                                *a = self.global[ARPEGGIO].2
                            } else {
                                self.global[ARPEGGIO].2 = *a;
                            }
                            // second arg
                            if *b == 0 {
                                *b = self.global[ARPEGGIO].3
                            } else {
                                self.global[ARPEGGIO].3 = *b;
                            }
                        }
                    }
                }

                TrackImportEffect::Panbrello(a, b) => {
                    match mem {
                        MemoryType::Mod => {
                            // No memory use
                        }
                        MemoryType::Xm | MemoryType::It | MemoryType::S3m => {
                            if *a == Q8_8::ZERO {
                                *a = cached_q8_8(self.channel[index][PANBRELLO].0)
                            } else {
                                self.channel[index][PANBRELLO].0 = cache_q8_8(*a);
                            }
                            // second arg
                            if *b == Q15::ZERO {
                                *b = cached_q15(self.channel[index][PANBRELLO].1)
                            } else {
                                self.channel[index][PANBRELLO].1 = cache_q15(*b);
                            }
                        }
                    }
                }

                TrackImportEffect::PanningSlideN(a) => {
                    match mem {
                        MemoryType::Mod => {
                            // No memory use
                        }
                        MemoryType::Xm | MemoryType::It | MemoryType::S3m => {
                            if *a == Q15::ZERO {
                                *a = cached_q15(self.channel[index][PANNING_SLIDE].0)
                            } else {
                                self.channel[index][PANNING_SLIDE].0 = cache_q15(*a);
                            }
                        }
                    }
                }

                TrackImportEffect::PortamentoUp(a) => {
                    match mem {
                        MemoryType::Mod => {
                            // No memory use
                        }
                        MemoryType::Xm => {
                            if *a == 0 {
                                *a = self.channel[index][PORTAMENTO_UP].0
                            } else {
                                self.channel[index][PORTAMENTO_UP].0 = *a;
                            }
                        }
                        MemoryType::It => {
                            // When the IT "link Gxx with E/F" flag
                            // is set, E/F and G share the slot; we
                            // read/write PORTAMENTO_UP unconditionally
                            // for that case (see TonePortamento arm).
                            // The same slot is used whether the flag
                            // is set or not on the E/F side, because
                            // E and F already share it in IT without
                            // the flag — the flag only extends that
                            // sharing to G.
                            if *a == 0 {
                                *a = self.channel[index][PORTAMENTO_UP].0
                            } else {
                                self.channel[index][PORTAMENTO_UP].0 = *a;
                            }
                        }
                        MemoryType::S3m => {
                            if *a == 0 {
                                *a = self.global[PORTAMENTO_UP].0
                            } else {
                                self.global[PORTAMENTO_UP].0 = *a;
                            }
                        }
                    }
                }

                TrackImportEffect::PortamentoDown(a) => {
                    match mem {
                        MemoryType::Mod => {
                            // No memory use
                        }
                        MemoryType::Xm => {
                            if *a == 0 {
                                *a = self.channel[index][PORTAMENTO_DOWN].0
                            } else {
                                self.channel[index][PORTAMENTO_DOWN].0 = *a;
                            }
                        }
                        MemoryType::It => {
                            // IT E and F already share memory (see
                            // ITTECH "Exx / Fxx share memory"). Both
                            // read/write PORTAMENTO_UP so the slot
                            // is shared between them — and with G
                            // when the link flag is on.
                            if *a == 0 {
                                *a = self.channel[index][PORTAMENTO_UP].0
                            } else {
                                self.channel[index][PORTAMENTO_UP].0 = *a;
                            }
                        }
                        MemoryType::S3m => {
                            if *a == 0 {
                                *a = self.global[PORTAMENTO_DOWN].0
                            } else {
                                self.global[PORTAMENTO_DOWN].0 = *a;
                            }
                        }
                    }
                }

                TrackImportEffect::PortamentoFineUp(a) => {
                    match mem {
                        MemoryType::Mod => {
                            // No memory use
                        }
                        MemoryType::Xm | MemoryType::It => {
                            if *a == 0 {
                                *a = self.channel[index][PORTAMENTO_FINE_UP].0
                            } else {
                                self.channel[index][PORTAMENTO_FINE_UP].0 = *a;
                            }
                        }
                        MemoryType::S3m => {
                            if *a == 0 {
                                *a = self.global[PORTAMENTO_FINE_UP].0
                            } else {
                                self.global[PORTAMENTO_FINE_UP].0 = *a;
                            }
                        }
                    }
                }

                TrackImportEffect::PortamentoFineDown(a) => {
                    match mem {
                        MemoryType::Mod => {
                            // No memory use
                        }
                        MemoryType::Xm | MemoryType::It => {
                            if *a == 0 {
                                *a = self.channel[index][PORTAMENTO_FINE_DOWN].0
                            } else {
                                self.channel[index][PORTAMENTO_FINE_DOWN].0 = *a;
                            }
                        }
                        MemoryType::S3m => {
                            if *a == 0 {
                                *a = self.global[PORTAMENTO_FINE_DOWN].0
                            } else {
                                self.global[PORTAMENTO_FINE_DOWN].0 = *a;
                            }
                        }
                    }
                }

                TrackImportEffect::PortamentoExtraFineUp(a) => {
                    match mem {
                        MemoryType::Mod => {
                            // No memory use
                        }
                        MemoryType::Xm | MemoryType::It => {
                            if *a == 0 {
                                *a = self.channel[index][PORTAMENTO_FINE_EXTRA_UP].0
                            } else {
                                self.channel[index][PORTAMENTO_FINE_EXTRA_UP].0 = *a;
                            }
                        }
                        MemoryType::S3m => {
                            if *a == 0 {
                                *a = self.global[PORTAMENTO_FINE_EXTRA_UP].0
                            } else {
                                self.global[PORTAMENTO_FINE_EXTRA_UP].0 = *a;
                            }
                        }
                    }
                }

                TrackImportEffect::PortamentoExtraFineDown(a) => {
                    match mem {
                        MemoryType::Mod => {
                            // No memory use
                        }
                        MemoryType::Xm | MemoryType::It => {
                            if *a == 0 {
                                *a = self.channel[index][PORTAMENTO_FINE_EXTRA_DOWN].0
                            } else {
                                self.channel[index][PORTAMENTO_FINE_EXTRA_DOWN].0 = *a;
                            }
                        }
                        MemoryType::S3m => {
                            if *a == 0 {
                                *a = self.global[PORTAMENTO_FINE_EXTRA_DOWN].0
                            } else {
                                self.global[PORTAMENTO_FINE_EXTRA_DOWN].0 = *a;
                            }
                        }
                    }
                }

                TrackImportEffect::NoteRetrigExtended(speed, vol) => {
                    match mem {
                        MemoryType::Mod => {
                            // No memory use
                        }
                        MemoryType::Xm | MemoryType::It => {
                            if *speed == 0 {
                                *speed = self.channel[index][NOTE_RETRIG].2
                            } else {
                                self.channel[index][NOTE_RETRIG].2 = *speed;
                            }
                            if *vol == 0 {
                                *vol = self.channel[index][NOTE_RETRIG].3
                            } else {
                                self.channel[index][NOTE_RETRIG].3 = *vol;
                            }
                        }
                        MemoryType::S3m => {
                            // ST3 keeps both Qxy nibbles in a single
                            // per-channel slot. Mirror that here so
                            // recalling either half consults the same
                            // channel's last Qxy.
                            if *speed == 0 {
                                *speed = self.channel[index][NOTE_RETRIG].2
                            } else {
                                self.channel[index][NOTE_RETRIG].2 = *speed;
                            }
                            if *vol == 0 {
                                *vol = self.channel[index][NOTE_RETRIG].3
                            } else {
                                self.channel[index][NOTE_RETRIG].3 = *vol;
                            }
                        }
                    }
                }

                TrackImportEffect::TonePortamento(a) => match mem {
                    MemoryType::Mod | MemoryType::Xm | MemoryType::S3m => {
                        if *a == 0 {
                            *a = self.channel[index][TONE_PORTAMENTO].0
                        } else {
                            self.channel[index][TONE_PORTAMENTO].0 = *a;
                        }
                    }
                    MemoryType::It => {
                        if self.it_flags.link_gxx_ef_memory {
                            // IT header flag bit 5 ON: Gxx shares
                            // its memory slot with Exx / Fxx. Writing
                            // any of the three updates the shared
                            // register (reused here via
                            // `PORTAMENTO_UP`); reading a zero-param
                            // on any of them reads from that same
                            // slot. The canonical ITTECH semantics.
                            if *a == 0 {
                                *a = self.channel[index][PORTAMENTO_UP].0
                            } else {
                                self.channel[index][PORTAMENTO_UP].0 = *a;
                            }
                        } else {
                            // Standard IT: Gxx has its own register,
                            // separate from Exx / Fxx.
                            if *a == 0 {
                                *a = self.channel[index][TONE_PORTAMENTO].0
                            } else {
                                self.channel[index][TONE_PORTAMENTO].0 = *a;
                            }
                        }
                    }
                },

                TrackImportEffect::Tremolo(a, b) => match mem {
                    MemoryType::Mod | MemoryType::Xm | MemoryType::It => {
                        if *a == Q8_8::ZERO {
                            *a = cached_q8_8(self.channel[index][TREMOLO].0)
                        } else {
                            self.channel[index][TREMOLO].0 = cache_q8_8(*a);
                        }
                        if *b == Q15::ZERO {
                            *b = cached_q15(self.channel[index][TREMOLO].1)
                        } else {
                            self.channel[index][TREMOLO].1 = cache_q15(*b);
                        }
                    }
                    MemoryType::S3m => {
                        if *a == Q8_8::ZERO {
                            *a = cached_q8_8(self.global[TREMOLO].0)
                        } else {
                            self.global[TREMOLO].0 = cache_q8_8(*a);
                        }
                        if *b == Q15::ZERO {
                            *b = cached_q15(self.global[TREMOLO].1)
                        } else {
                            self.global[TREMOLO].1 = cache_q15(*b);
                        }
                    }
                },

                TrackImportEffect::Tremor(a, b) => {
                    match mem {
                        MemoryType::Mod => {
                            // No memory
                        }
                        MemoryType::Xm | MemoryType::It => {
                            if *a == 0 {
                                *a = self.channel[index][TREMOR].2
                            } else {
                                self.channel[index][TREMOR].2 = *a;
                            }
                            if *b == 0 {
                                *b = self.channel[index][TREMOR].3
                            } else {
                                self.channel[index][TREMOR].3 = *b;
                            }
                        }
                        MemoryType::S3m => {
                            if *a == 0 {
                                *a = self.global[TREMOR].2
                            } else {
                                self.global[TREMOR].2 = *a;
                            }
                            if *b == 0 {
                                *b = self.global[TREMOR].3
                            } else {
                                self.global[TREMOR].3 = *b;
                            }
                        }
                    }
                }

                TrackImportEffect::InstrumentSampleOffset(a) => match mem {
                    MemoryType::Mod | MemoryType::Xm | MemoryType::It | MemoryType::S3m => {
                        if *a == 0 {
                            *a = self.channel[index][SAMPLE_OFFSET].2
                                + self.channel[index][SAMPLE_OFFSET_ADD_HIGH].2;
                        } else {
                            self.channel[index][SAMPLE_OFFSET].2 = *a;
                            // The persistent high-offset register
                            // (set by the most recent SAx) must be
                            // OR'd into every Oxx, not only the
                            // memory-recall branch. Schism does the
                            // equivalent in `csf_process_effects`:
                            // `mem_offset = (mem_offset & ~0xff00) |
                            // (param << 8);` — the upper 4 bits
                            // set by SAx survive the OR.
                            *a += self.channel[index][SAMPLE_OFFSET_ADD_HIGH].2;
                        }
                    }
                },

                TrackImportEffect::InstrumentSampleOffsetAddHigh(a) => match mem {
                    MemoryType::Mod | MemoryType::Xm | MemoryType::S3m => {
                        // No memory
                    }
                    MemoryType::It => {
                        self.channel[index][SAMPLE_OFFSET_ADD_HIGH].2 = *a;
                    }
                },

                TrackImportEffect::Vibrato(a, b) => match mem {
                    MemoryType::Mod | MemoryType::Xm | MemoryType::It | MemoryType::S3m => {
                        if *a == Q8_8::ZERO {
                            *a = cached_q8_8(self.channel[index][VIBRATO].0)
                        } else {
                            self.channel[index][VIBRATO].0 = cache_q8_8(*a);
                        }
                        if *b == PitchDelta::ZERO {
                            *b = cached_pitch_delta(self.channel[index][VIBRATO].1)
                        } else {
                            self.channel[index][VIBRATO].1 = cache_pitch_delta(*b);
                        }
                    }
                },

                TrackImportEffect::VibratoFine(a, b) => {
                    match mem {
                        MemoryType::Mod | MemoryType::Xm | MemoryType::It => {
                            // No memory
                        }
                        MemoryType::S3m => {
                            if *a == Q8_8::ZERO {
                                *a = cached_q8_8(self.channel[index][VIBRATO_FINE].0)
                            } else {
                                self.channel[index][VIBRATO_FINE].0 = cache_q8_8(*a);
                            }
                            if *b == PitchDelta::ZERO {
                                *b = cached_pitch_delta(self.channel[index][VIBRATO_FINE].1)
                            } else {
                                self.channel[index][VIBRATO_FINE].1 = cache_pitch_delta(*b);
                            }
                        }
                    }
                }

                TrackImportEffect::VolumeSlide0(a) => {
                    match mem {
                        MemoryType::Mod => {
                            // No memory
                        }
                        MemoryType::Xm | MemoryType::It => {
                            if *a == Q15::ZERO {
                                *a = cached_q15(self.channel[index][PITCH_VOLUME_SLIDE_0].0)
                            } else {
                                self.channel[index][PITCH_VOLUME_SLIDE_0].0 = cache_q15(*a);
                            }
                        }
                        MemoryType::S3m => {
                            if *a == Q15::ZERO {
                                *a = cached_q15(self.global[PITCH_VOLUME_SLIDE_0].0)
                            } else {
                                self.global[PITCH_VOLUME_SLIDE_0].0 = cache_q15(*a);
                            }
                        }
                    }
                }

                TrackImportEffect::VolumeSlideN(a) => {
                    match mem {
                        MemoryType::Mod => {
                            // No memory
                        }
                        MemoryType::Xm | MemoryType::It => {
                            if *a == Q15::ZERO {
                                *a = cached_q15(self.channel[index][PITCH_VOLUME_SLIDE_N].0)
                            } else {
                                self.channel[index][PITCH_VOLUME_SLIDE_N].0 = cache_q15(*a);
                            }
                        }
                        MemoryType::S3m => {
                            if *a == Q15::ZERO {
                                *a = cached_q15(self.global[PITCH_VOLUME_SLIDE_N].0)
                            } else {
                                self.global[PITCH_VOLUME_SLIDE_N].0 = cache_q15(*a);
                            }
                        }
                    }
                }

                TrackImportEffect::TonePortamentoFxVol(a) => {
                    match mem {
                        MemoryType::Mod | MemoryType::S3m => {
                            // No memory
                        }
                        MemoryType::Xm => {
                            if *a == 0 {
                                *a = self.channel[index][FX_VOLUME_TONE_PORTAMENTO].0
                            } else {
                                self.channel[index][FX_VOLUME_TONE_PORTAMENTO].0 = *a;
                            }
                        }
                        MemoryType::It => {
                            // never called because no use
                        }
                    }
                }

                TrackImportEffect::VibratoDepthFxVol(a) => {
                    match mem {
                        MemoryType::Mod | MemoryType::S3m => {
                            // No memory
                        }
                        MemoryType::Xm => {
                            if *a == PitchDelta::ZERO {
                                *a = cached_pitch_delta(
                                    self.channel[index][FX_VOLUME_VIBRATO_DEPTH].0,
                                )
                            } else {
                                self.channel[index][FX_VOLUME_VIBRATO_DEPTH].0 =
                                    cache_pitch_delta(*a);
                            }
                        }
                        MemoryType::It => {
                            // never called because no use
                        }
                    }
                }

                TrackImportEffect::ChannelVolumeSlide0(a)
                | TrackImportEffect::ChannelVolumeSlideN(a) => {
                    match mem {
                        MemoryType::Mod => {
                            // No memory
                        }
                        MemoryType::Xm | MemoryType::It | MemoryType::S3m => {
                            if *a == Q15::ZERO {
                                *a = cached_q15(self.channel[index][CHANNEL_VOLUME_SLIDE].0)
                            } else {
                                self.channel[index][CHANNEL_VOLUME_SLIDE].0 = cache_q15(*a);
                            }
                        }
                    }
                }

                _ => {}
            }
        });

        tiu.song_level.iter_mut().for_each(|e| {
            match e {
                SongLevelEffect::BpmSlide(a) => {
                    let fx_index = if *a < 0 { TEMPO_DOWN } else { TEMPO_UP };
                    match mem {
                        MemoryType::Mod => {
                            // No memory
                        }
                        MemoryType::Xm | MemoryType::It | MemoryType::S3m => {
                            if *a == 0 {
                                *a = self.channel[index][fx_index].2 as isize;
                            } else {
                                self.channel[index][fx_index].2 = *a as usize;
                            }
                        }
                    }
                }
                SongLevelEffect::VolumeSlide { speed: a, fine: _b } => {
                    match mem {
                        MemoryType::Mod => {
                            // No memory
                        }
                        MemoryType::Xm | MemoryType::It | MemoryType::S3m => {
                            if *a == Q15::ZERO {
                                *a = cached_q15(self.channel[index][CHANNEL_VOLUME_SLIDE].0)
                            } else {
                                self.channel[index][CHANNEL_VOLUME_SLIDE].0 = cache_q15(*a);
                            }
                        }
                    }
                }
                _ => {}
            }
        });
    }

    fn apply_memory(
        &mut self,
        freq_type: FrequencyType,
        mem: MemoryType,
        order: &[Vec<usize>],
        patterns: &[Vec<Vec<PatternSlot>>],
    ) -> Vec<Vec<Vec<TrackImportUnit>>> {
        let mut source: Vec<Vec<Vec<TrackImportUnit>>> = patterns
            .iter()
            .map(|pattern| match mem {
                #[cfg(any(feature = "import_mod", feature = "import_xm"))]
                MemoryType::Mod | MemoryType::Xm => {
                    // ProTracker (`.mod`) vibrato is 4× shallower than
                    // FT2's; the depth conversion needs to know which.
                    let protracker = matches!(mem, MemoryType::Mod);
                    ModXmEffect::mod_xm_unpack_pattern(freq_type, protracker, pattern)
                }
                #[cfg(feature = "import_s3m")]
                MemoryType::S3m => {
                    S3mEffect::s3m_unpack_pattern(freq_type, self.s3m_flags, pattern)
                }
                #[cfg(feature = "import_it")]
                MemoryType::It => ItEffect::it_unpack_pattern(freq_type, pattern),
                // Reached only if a `MemoryType` variant is dispatched
                // from code compiled without its corresponding
                // `import_*` feature. Callers of `apply_memory` always
                // pass a variant matching their own feature, so in a
                // well-formed build this arm is dead code.
                #[allow(unreachable_patterns)]
                _ => unreachable!("MemoryType dispatched to a disabled format importer"),
            })
            .collect();

        // It is impossible to guarantee that a pattern will not be designed
        // to have different pre-memory configurations.
        // We "play" the music in order and it will remain a best effort.
        order.iter().flat_map(|inner| inner.iter()).for_each(|o| {
            if *o >= source.len() {
                return;
            }
            let pattern = &mut source[*o];
            pattern.iter_mut().for_each(|row| {
                row.iter_mut().enumerate().for_each(|(index, tiu)| {
                    // `self.channel` is dimensioned for 64 channels;
                    // a format declaring more (e.g. an OpenMPT-extended
                    // XM with >64 channels) plays back without
                    // per-channel memory tracking instead of panicking.
                    if index < 64 {
                        self.update_memory(&mem, index, tiu)
                    }
                });
            });
        });

        source
    }

    pub fn unpack_patterns(
        &mut self,
        freq_type: FrequencyType,
        mem: MemoryType,
        order: &[Vec<usize>],
        patterns: &[Vec<Vec<PatternSlot>>],
    ) -> Vec<Vec<Vec<TrackImportUnit>>> {
        /* Get patterns of TrackImportUnit with effect memory resolved.
         *
         * DAW migration Phase 1+2: this stage no longer flattens
         * each `TrackImportUnit` into a runtime `Cell`. The DAW layer
         * needs to see the explicit `instrument: Option<usize>` field
         * (gone from `Cell` after the migration) to drive
         * `split_rows_by_instrument`. Materialisation into `Cell`
         * happens inside `build_timeline::extract_tracks_and_clips`,
         * once segments have been computed.
         */
        self.apply_memory(freq_type, mem, order, patterns)
    }
}
