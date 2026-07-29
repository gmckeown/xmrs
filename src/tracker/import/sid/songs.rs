//! The bundled `.sid` images, and nothing else.
//!
//! These are **third-party binaries** — Rob Hubbard's C64 tunes, as
//! distributed by the HVSC — not part of this crate's own work. What the
//! crate contributes is the *record* for each of them (`one_sid::OneSid`):
//! forty-odd fields of reverse-engineered layout and behaviour that took an
//! oracle to establish. The record is knowledge and stays compiled in
//! unconditionally; the image is data the caller can supply.
//!
//! So the images live behind the **`sid_songs`** feature, which is **off by
//! default**. A production build of `import_sid` carries the fifteen records
//! and none of the 137 KB of 6502 payload here; it plays a `.sid` the user
//! opens, recognising it by fingerprint
//! ([`SidModule::for_image`](crate::tracker::import::sid::sid_module::SidModule::for_image),
//! and `Module::load` on top of
//! it). Turning `sid_songs` on is a convenience for tests, examples and the
//! demo player, which want a tune to play without asking for a file.
//!
//! The files themselves are excluded from the published tarball, so the
//! feature only builds from a repository checkout. That is deliberate: it
//! keeps someone else's music out of an MIT crate on crates.io.
//!
//! The crate's own `#[cfg(test)]` modules (`detect.rs`, `g2.rs`) include these
//! files directly rather than through this module — an in-crate test always
//! has the repository, so gating them would only mean silently skipping
//! coverage under `--features import_sid`.

/// A bundled image and the name its record goes by.
pub type BundledSong = (&'static str, &'static [u8]);

pub const COMMANDO: &[u8] = include_bytes!("songs/commando.sid");
pub const CRAZY_COMETS: &[u8] = include_bytes!("songs/crazy_comets.sid");
pub const LAST_V8: &[u8] = include_bytes!("songs/last_v8.sid");
pub const MONTY_ON_THE_RUN: &[u8] = include_bytes!("songs/monty_on_the_run.sid");
pub const THING_ON_A_SPRING: &[u8] = include_bytes!("songs/thing_on_a_spring.sid");
pub const ZOIDS: &[u8] = include_bytes!("songs/zoids.sid");
pub const ACE_2: &[u8] = include_bytes!("songs/ace_2.sid");
pub const DELTA: &[u8] = include_bytes!("songs/delta.sid");
pub const HUMAN_RACE: &[u8] = include_bytes!("songs/human_race.sid");
pub const INTERNATIONAL_KARATE: &[u8] = include_bytes!("songs/international_karate.sid");
pub const LIGHTFORCE: &[u8] = include_bytes!("songs/lightforce.sid");
/// One image, two records: Sanxion ships two complete replayers, one per
/// song, so `sanxion_song_1` and `sanxion_song_2` share these bytes.
pub const SANXION: &[u8] = include_bytes!("songs/sanxion.sid");
pub const SPELLBOUND: &[u8] = include_bytes!("songs/spellbound.sid");
pub const THRUST: &[u8] = include_bytes!("songs/thrust.sid");

/// Rob Hubbard's last player generation — no record needed, every table and
/// every effect comes out of the image ([`super::g2`]). Bundled only so the
/// demo player and the oracle gates have something to point at.
pub const LION_HEART: &[u8] = include_bytes!("songs/lion_heart.sid");
pub const SUN_NEVER_SHINES: &[u8] = include_bytes!("songs/sun_never_shines.sid");
pub const PACIFIC_COAST: &[u8] = include_bytes!("songs/pacific_coast.sid");
pub const RADIO_ACE: &[u8] = include_bytes!("songs/radio_ace.sid");
pub const GO_GO_DASH: &[u8] = include_bytes!("songs/go_go_dash.sid");
pub const LAKERS_VS_CELTICS: &[u8] = include_bytes!("songs/lakers_vs_celtics.sid");
/// The earlier three-marker generation — recognised, not yet decoded. Kept
/// here because its oracle capture is bundled alongside it.
pub const OFF_THE_CUFF: &[u8] = include_bytes!("songs/off_the_cuff.sid");

/// Every bundled image, by the name its record / detector goes by.
///
/// This is what lets a test walk the whole bundle without naming files.
pub const ALL: &[BundledSong] = &[
    ("commando", COMMANDO),
    ("crazy_comets", CRAZY_COMETS),
    ("last_v8", LAST_V8),
    ("monty_on_the_run", MONTY_ON_THE_RUN),
    ("thing_on_a_spring", THING_ON_A_SPRING),
    ("zoids", ZOIDS),
    ("ace_2", ACE_2),
    ("delta", DELTA),
    ("human_race", HUMAN_RACE),
    ("international_karate", INTERNATIONAL_KARATE),
    ("lightforce", LIGHTFORCE),
    ("sanxion", SANXION),
    ("spellbound", SPELLBOUND),
    ("thrust", THRUST),
    ("lion_heart", LION_HEART),
    ("sun_never_shines", SUN_NEVER_SHINES),
    ("pacific_coast", PACIFIC_COAST),
    ("radio_ace", RADIO_ACE),
    ("go_go_dash", GO_GO_DASH),
    ("lakers_vs_celtics", LAKERS_VS_CELTICS),
    ("off_the_cuff", OFF_THE_CUFF),
];
