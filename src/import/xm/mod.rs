#![forbid(unsafe_code)]
/*
 * All to load Fast Tracker 2 XM Modules.
 *
 * Note: `mod_xm_effect` is reused by the Amiga MOD importer (MOD and
 * XM share the same effect-parsing code path in `ImportMemory`). It
 * is therefore compiled whenever either `import_mod` or `import_xm`
 * is enabled, while the rest of this module is strictly XM-specific.
 */

// Shared with Amiga MOD.
pub mod mod_xm_effect;

// ---- XM-specific items ----

#[cfg(feature = "import_xm")]
mod helper;
#[cfg(feature = "import_xm")]
mod serde_helper;

#[cfg(feature = "import_xm")]
pub mod patternslot;
#[cfg(feature = "import_xm")]
pub mod xmheader;
#[cfg(feature = "import_xm")]
pub mod xminstrument;
#[cfg(feature = "import_xm")]
pub mod xmmodule;
#[cfg(feature = "import_xm")]
pub mod xmpattern;
#[cfg(feature = "import_xm")]
pub mod xmsample;

#[cfg(feature = "import_xm")]
pub mod xi_instrument;
#[cfg(feature = "import_xm")]
pub mod xp_pattern;
#[cfg(feature = "import_xm")]
pub mod xt_track;
