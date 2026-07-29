//! Format-tied items: presets of [`crate::core::compatibility::PlaybackQuirks`]
//! mirroring historical trackers, the `format::ModuleFormat` tag, and the
//! import readers / build / extract pipelines. (The chip-synthesis instrument
//! types are *not* here — they are format-agnostic authoring primitives and
//! live under [`crate::core`].)
//!
//! Always-on sub-modules `period` and `codepage` compile in every
//! configuration — the player needs `period` for sample frequency
//! conversion, and `codepage` is small enough to keep available
//! for displaying text from imported modules. Other sub-modules
//! are gated behind one or more `import_*` features.

pub mod codepage;
pub mod period;

#[cfg(any(
    feature = "import_mod",
    feature = "import_xm",
    feature = "import_s3m",
    feature = "import_it",
    feature = "import_sid",
    feature = "import_dw",
))]
pub mod format;

#[cfg(any(
    feature = "import_mod",
    feature = "import_xm",
    feature = "import_s3m",
    feature = "import_it",
    feature = "import_sid",
    feature = "import_dw",
))]
pub mod profiles;

#[cfg(feature = "import_it")]
pub mod mix_plugin;

pub mod import;
