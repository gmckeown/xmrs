#![forbid(unsafe_code)]

//! Parsed data structures lifted out of a `.dw` payload.
//!
//! The shapes here mirror the runtime layout that the replayer
//! walks; field names follow the structural spec
//! `whittaker-dw-format.md` (repo root) and `FORMAT.md` (this
//! directory). None of the fields carry endianness: the importer
//! reads them big-endian and stores native integers.

use alloc::vec::Vec;

/// Number of Paula channels — fixed at 4 on every Amiga model
/// Whittaker shipped his player on. Used as the position-list
/// fan-out per sub-song.
pub const DW_NUM_CHANNELS: usize = 4;

/// Sub-song descriptor lifted from the `sub_song_list_offset`
/// header (spec §4.1). One sub-song per module is the rule for
/// every Whittaker score we've inspected; the spec allows multiple
/// but the on-disk layout makes the count implicit, so we expose a
/// single entry and let the caller cope when (and if) a multi-song
/// `.dw` shows up.
#[derive(Debug, Clone)]
pub struct DwSubSong {
    /// Frames between successive command reads on every channel
    /// (the canonical "speed" of the replayer; spec §9 phase 4).
    pub speed: u8,

    /// Optional ratchet for `enable_delay_counter` builds —
    /// adds extra "skipped frames" between commands. Kept for all
    /// variants; ignored when the feature flag is off.
    pub delay_speed: u8,

    /// Per-channel position-list start offset (absolute, into the
    /// `.dw` payload). Channel `i` reads its track sequence
    /// starting at this offset.
    ///
    /// Stored as `u32` so the type covers both the canonical
    /// new-player layout (16-bit offsets) and the qball-era 32-bit
    /// layout uniformly. Always less than `payload.len()`.
    pub channel_position_offsets: [u32; DW_NUM_CHANNELS],
}

/// One channel's position list (spec §4.2) — an ordered sequence
/// of track references. Each entry is the absolute file offset of
/// a track byte stream; the top bit (15 for new-player u16 entries,
/// 31 for qball-era u32 entries) marks a backward loop into the
/// list rather than a normal track reference.
///
/// Entries are stored as `u32` so the type covers both encodings
/// uniformly. The actual on-disk width is implicit in the layout
/// — the parser reads the matching size and zero-extends.
#[derive(Debug, Clone, Default)]
pub struct DwPositionList {
    /// Raw entries in order, with the loop bit masked off. The
    /// terminator (`0` for "end" / an MSB-set value for "loop")
    /// is **not** stored — the runtime mechanic is encoded in
    /// [`Self::loop_to`] instead.
    pub entries: Vec<u32>,

    /// `Some(i)` when the list ends with an MSB-set value pointing
    /// back to entry `i`; `None` for plain `0` termination
    /// (one-shot play, then the channel falls silent).
    pub loop_to: Option<u32>,
}

impl DwPositionList {
    /// Track-reference count in this list, ignoring the
    /// terminator.
    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when the list carries no real entries (a channel
    /// kept entirely silent for this sub-song).
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A single track byte stream — the bytes between a track's
/// position-list reference and its `EndOfTrack` command (`0x80`),
/// inclusive of the terminator. Notes (`0x00..=0x7F`) and
/// commands (`0x80..=0xFF`) are stored verbatim; the runtime tick
/// (spec §9) decodes them into Paula writes.
///
/// The same content is also available pre-decoded in
/// [`Self::events`] for clients that don't want to re-walk the
/// byte grammar.
#[derive(Debug, Clone)]
pub struct DwTrack {
    /// File offset of the first byte. Used as the de-duplication
    /// key in [`super::dw_module::DwModule::tracks`] — every
    /// distinct offset referenced by any position list contributes
    /// exactly one [`DwTrack`].
    ///
    /// Stored as `u32` so the type covers both the new-player
    /// (16-bit) and qball-era (32-bit) position-list encodings
    /// uniformly. Always less than `payload.len()`.
    pub offset: u32,

    /// Raw bytes from `offset` up to and including the `0x80`
    /// terminator. An empty `Vec` means the offset was unreachable
    /// (out of bounds, or the scan defensive cap kicked in before
    /// finding the terminator).
    pub bytes: Vec<u8>,

    /// Same content as [`Self::bytes`], pre-decoded into typed
    /// events. Populated by [`super::event::decode_track`] at
    /// import time so the runtime layer can consume events
    /// directly. Empty when `bytes` is empty.
    pub events: Vec<super::event::DwTrackEvent>,
}

impl DwTrack {
    /// Number of note bytes (high bit clear).
    pub fn note_count(&self) -> usize {
        self.bytes.iter().filter(|&&b| b < 0x80).count()
    }

    /// Number of command bytes (high bit set).
    pub fn command_count(&self) -> usize {
        self.bytes.iter().filter(|&&b| b >= 0x80).count()
    }
}

/// A single sample: metadata + the raw 8-bit signed PCM payload.
#[derive(Debug, Clone)]
pub struct DwSample {
    /// 0-based slot number assigned at parse time. Re-used by the
    /// runtime when converting `SetSample` commands.
    pub index: u16,

    /// `loop_start` from the sample-info table, in **bytes** from
    /// the start of `pcm`. `None` means "one-shot" (originally
    /// stored as `-1` or any negative sentinel).
    pub loop_start: Option<u32>,

    /// Total sample length in bytes. Equals `length_words * 2`
    /// from the on-disk word count.
    pub length: u32,

    /// Native Paula rate emitted by the sample-data header. Drives
    /// the per-sample fine-tune multiplier of the runtime period
    /// formula (`period = table[note] × (0x369E99 / frequency) >> 10`,
    /// see `to_module::note_to_pitch`), so it is read, not just a
    /// diagnostic hint.
    pub frequency: u16,

    /// Default channel volume for the sample, 0..=64 (Paula scale).
    pub volume: u16,

    /// Per-sample note transpose, in semitones (signed). Added to
    /// every note's period-table index before lookup — the
    /// replayer's `note += sample.transpose` step (sample-info
    /// byte +14 on the tetris-family loader). `0` on modules
    /// without the `enable_sample_transpose` feature.
    pub transpose: i8,

    /// PCM data, 8-bit signed. One byte per sample frame.
    pub pcm: Vec<i8>,
}

impl DwSample {
    /// Whether the sample loops on playback.
    #[inline]
    pub fn is_looping(&self) -> bool {
        self.loop_start.is_some()
    }

    /// Loop length in bytes — only meaningful if [`Self::is_looping`].
    /// Returns `Some(length - loop_start)`, matching the
    /// `SetLoop(start, length - start)` call the original replayer
    /// makes against Paula.
    pub fn loop_length(&self) -> Option<u32> {
        self.loop_start.map(|s| self.length.saturating_sub(s))
    }
}

/// A per-channel volume envelope — the byte stream consumed by
/// the `0xA0..` dispatcher bracket (which Whittaker's replayer
/// reads at `Play+0x412` to drive Paula's `AUD0VOL` register).
///
/// Each envelope is a sequence of 7-bit volume values in Paula's
/// 0..=127 scale (the replayer's `MULU.W global_vol ; LSR.W #6`
/// step rescales them to Paula's 0..=64). The last value in the
/// sequence is the *sustain* level — its on-disk byte had the
/// high bit set, signalling the replayer to stop advancing the
/// envelope.
///
/// `step_interval` is the per-channel `chan[+0x2A]` slot: the
/// number of replayer ticks the envelope holds each value before
/// advancing. `1` means a new step every tick (fastest decay);
/// higher values stretch the envelope out.
#[derive(Debug, Clone)]
pub struct DwVolumeEnvelope {
    /// 0-based slot — `(track_byte - volume_envelope_threshold)` at
    /// decode time. Armed by the `0xA0..` dispatcher bracket
    /// ([`super::event::DwTrackEvent::SetVolumeEnvelope`]).
    pub index: u16,

    /// Ticks the replayer waits between two envelope advances —
    /// the `chan[+0x2A]` slot. `0` is technically legal in the
    /// encoding (the counter underflows immediately, meaning
    /// "advance every tick") and we preserve it as-is.
    pub step_interval: u8,

    /// Envelope values in 0..=127 (Paula 7-bit) — last entry is
    /// the sustain level. May be empty when the table entry was
    /// malformed; callers should default to "no volume shaping"
    /// in that case.
    pub steps: Vec<u8>,
}

/// A per-channel **pitch arpeggio** — the byte stream consumed by
/// the `0x90..` dispatcher bracket (spec §4.4 / §10.12). Each
/// entry is a signed semitone offset added to the current note's
/// period-table index; the replayer cycles one entry per tick and
/// loops back to the start when it passes the terminator (the
/// on-disk byte with its `0x80` bit set, whose low 7 bits are the
/// last offset).
#[derive(Debug, Clone)]
pub struct DwArpeggio {
    /// 0-based slot — matches `(track_byte - pitch_arpeggio_threshold)`
    /// at decode time.
    pub index: u16,

    /// Semitone offsets, in cycle order. Never empty for a parsed
    /// entry; a single-element list means a static (non-moving)
    /// offset.
    pub offsets: Vec<i8>,
}

impl DwArpeggio {
    /// The classic-tracker `Arpeggio { half1, half2 }` pair this
    /// arpeggio best approximates: the first two *distinct,
    /// non-zero* offsets in the cycle. xmrs's arpeggio effect
    /// cycles `base, base+half1, base+half2` once per tick, which
    /// matches the common 3-note chord arpeggios while only
    /// approximating longer Whittaker cycles. Returns `(0, 0)`
    /// when the arpeggio carries no non-zero offset (a no-op
    /// arpeggio — emit nothing).
    pub fn classic_pair(&self) -> (usize, usize) {
        let mut nz = self.offsets.iter().copied().filter(|&o| o != 0);
        let h1 = nz.next().unwrap_or(0);
        let h2 = nz.next().unwrap_or(h1);
        (h1.max(0) as usize, h2.max(0) as usize)
    }
}

impl DwVolumeEnvelope {
    /// First-step volume (`0..=127`). `None` when the envelope is
    /// empty — callers should fall back to Paula's pre-existing
    /// channel volume.
    #[inline]
    pub fn initial(&self) -> Option<u8> {
        self.steps.first().copied()
    }

    /// Sustain volume (`0..=127`) — the level the envelope holds
    /// once it finishes advancing. `None` when the envelope is
    /// empty.
    #[inline]
    pub fn sustain(&self) -> Option<u8> {
        self.steps.last().copied()
    }

    /// Peak volume across the whole envelope (`0..=127`). The best
    /// single-value approximation for note loudness when the
    /// renderer can't model per-tick envelope advance: a Whittaker
    /// envelope often opens with a very low attack value (e.g.
    /// `0x01` ramping up to `0x10`) so picking [`Self::initial`]
    /// would make whole sections inaudible; using the peak instead
    /// matches what a listener perceives once the ramp finishes.
    /// `None` when the envelope is empty.
    #[inline]
    pub fn peak(&self) -> Option<u8> {
        self.steps.iter().copied().max()
    }
}
