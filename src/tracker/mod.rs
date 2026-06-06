//! Format-tied items: presets of [`crate::core::compatibility::PlaybackQuirks`]
//! mirroring historical trackers, the `format::ModuleFormat` tag,
//! format-specific instruments, and the import readers / build /
//! extract pipelines.
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

#[cfg(feature = "import_s3m")]
pub mod instr_opl;
#[cfg(feature = "import_sid")]
pub mod instr_robsid;
#[cfg(feature = "import_sid")]
pub mod instr_sid;

pub mod import;
