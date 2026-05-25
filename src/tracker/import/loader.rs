//! `Module::load_*` impls — auto-detect a tracker file's format
//! from its header and dispatch to the matching format-specific
//! loader (`load_mod`, `load_xm`, `load_s3m`, `load_it`). SID files
//! are loaded via `SidModule::load` directly and don't go through
//! the autodetect path.

use crate::prelude::*;
use crate::tracker::import::bin_reader::ImportError;

// The module-level gate in `crate::tracker::import::mod` only compiles this
// file when at least one of MOD/XM/S3M/IT is enabled, so `load` below
// always has at least one live branch and `source` is always used.

impl Module {
    /// Try to import an Amiga ProTracker MOD file.
    #[cfg(feature = "import_mod")]
    pub fn load_mod(source: &[u8]) -> Result<Self, ImportError> {
        use super::amiga::amiga_module::AmigaModule;

        Ok(AmigaModule::load(source)?.to_module())
    }

    /// Try to import a Fast Tracker II XM module file.
    #[cfg(feature = "import_xm")]
    pub fn load_xm(source: &[u8]) -> Result<Self, ImportError> {
        use super::xm::xmmodule::XmModule;

        Ok(XmModule::load(source)?.to_module())
    }

    /// Try to import a ScreamTracker 3 S3M module file.
    #[cfg(feature = "import_s3m")]
    pub fn load_s3m(source: &[u8]) -> Result<Self, ImportError> {
        use super::s3m::s3m_module::S3mModule;

        Ok(S3mModule::load(source)?.to_module())
    }

    /// Try to import an Impulse Tracker IT module file.
    #[cfg(feature = "import_it")]
    pub fn load_it(source: &[u8]) -> Result<Self, ImportError> {
        use super::it::it_module::ItModule;

        Ok(ItModule::load(source)?.to_module())
    }

    /// Try to auto-detect and import any supported historical module
    /// file (MOD / XM / S3M / IT). Formats are tried in the order
    /// XM → S3M → IT → MOD; the Amiga format is tried last because it
    /// has the weakest header signature for detection. Only formats
    /// whose `import_*` feature is enabled are attempted.
    ///
    /// SID is not auto-detected here — it has its own dedicated entry
    /// point via `crate::tracker::import::sid::sid_module::SidModule::load`.
    pub fn load(source: &[u8]) -> Result<Self, ImportError> {
        #[cfg(feature = "import_xm")]
        if let Ok(m) = Self::load_xm(source) {
            return Ok(m);
        };

        #[cfg(feature = "import_s3m")]
        if let Ok(m) = Self::load_s3m(source) {
            return Ok(m);
        };

        #[cfg(feature = "import_it")]
        if let Ok(m) = Self::load_it(source) {
            return Ok(m);
        };

        // The amiga format is the last one because it is the least well specified for format detection
        #[cfg(feature = "import_mod")]
        if let Ok(m) = Self::load_mod(source) {
            return Ok(m);
        };

        // When no MOD/XM/S3M/IT feature is enabled the whole
        // `import_loader` module is gated out in `import::mod`, so
        // we never reach this arm with `source` being the only live
        // binding — the compiler always sees at least one of the
        // `#[cfg]` blocks above as active. The `let _ = source;` is
        // a belt-and-braces silencer for the rare case where only
        // one `load_*` is compiled and a stubborn linter still
        // complains.
        let _ = source;

        Err(ImportError::Other("Unknown data?"))
    }
}
