//! `Module::load_*` impls — auto-detect a tracker file's format
//! from its header and dispatch to the matching format-specific
//! loader (`load_mod`, `load_xm`, `load_s3m`, `load_it`, `load_dw`,
//! `load_sid`). SID is settled first and exclusively, off its PSID/RSID
//! magic; DW is auto-detected even without a magic header — its probe
//! quadruple is unique enough — and so goes last.

use crate::prelude::*;
use crate::tracker::import::bin_reader::ImportError;

// The module-level gate in `crate::tracker::import::mod` only compiles
// this file when at least one of MOD/XM/S3M/IT/DW is enabled, so
// `load` below always has at least one live branch and `source` is
// always used.

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

    /// Try to import a TFMX module from paired `mdat` and `smpl` data.
    pub fn load_tfmx(mdat: &[u8], smpl: &[u8], subsong_idx: usize) -> Result<Self, ImportError> {
        let tfmx = super::tfmx::header::TfmxModule::parse(mdat, smpl)?;
        tfmx.to_module(subsong_idx)
    }

    /// Try to import a TFMX module from a file path (locating its paired sample file).
    #[cfg(feature = "std")]
    pub fn load_tfmx_path<P: AsRef<std::path::Path>>(path: P, subsong_idx: usize) -> Result<Self, ImportError> {
        let path = path.as_ref();
        let mdat = std::fs::read(path).map_err(|_| ImportError::Other("tfmx: cannot read mdat/tfx file"))?;

        // If file is a self-contained packed format (TFHD or TFMX-MOD), load directly
        if mdat.starts_with(b"TFHD") || mdat.starts_with(b"TFMX-MOD") {
            let mut module = Self::load_tfmx(&mdat, &[], subsong_idx)?;
            if module.name.is_empty() || module.name.starts_with("Date :") {
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                if !stem.is_empty() {
                    module.name = stem.into();
                }
            }
            return Ok(module);
        }

        let parent = path.parent().unwrap_or_else(|| std::path::Path::new(""));
        let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");

        let companion_candidates = if file_name.ends_with(".tfx") {
            alloc::vec![
                alloc::format!("{}.sam", &file_name[..file_name.len() - 4]),
                alloc::format!("{}.SAM", &file_name[..file_name.len() - 4]),
            ]
        } else if file_name.ends_with(".TFX") {
            alloc::vec![
                alloc::format!("{}.SAM", &file_name[..file_name.len() - 4]),
                alloc::format!("{}.sam", &file_name[..file_name.len() - 4]),
            ]
        } else if file_name.starts_with("mdat.") {
            alloc::vec![
                alloc::format!("smpl.{}", &file_name[5..]),
                alloc::format!("SMPL.{}", &file_name[5..]),
            ]
        } else if file_name.starts_with("MDAT.") {
            alloc::vec![
                alloc::format!("SMPL.{}", &file_name[5..]),
                alloc::format!("smpl.{}", &file_name[5..]),
            ]
        } else {
            alloc::vec![
                alloc::format!("{}.sam", file_name),
                alloc::format!("{}.SAM", file_name),
                alloc::format!("smpl.{}", file_name),
            ]
        };

        let mut smpl = None;
        for cand in companion_candidates {
            let p = parent.join(&cand);
            if let Ok(data) = std::fs::read(&p) {
                smpl = Some(data);
                break;
            }
        }

        let smpl = smpl.ok_or(ImportError::Other("tfmx: companion sample file (.sam / smpl.*) not found"))?;
        let mut module = Self::load_tfmx(&mdat, &smpl, subsong_idx)?;
        if module.name.is_empty() || module.name.starts_with("Date :") {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if !stem.is_empty() {
                module.name = stem.into();
            }
        }
        Ok(module)
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

    /// Try to import a David Whittaker custom Amiga `.dw` file.
    /// The detection layer runs a probe-quadruple scan (`LEA /
    /// LEA / MOVEQ / DBF`) over the first ~4 KB of the payload
    /// and walks the BSR call graph from there; no magic header
    /// is required.
    #[cfg(feature = "import_dw")]
    pub fn load_dw(source: &[u8]) -> Result<Self, ImportError> {
        use super::dw::dw_module::DwModule;

        Ok(DwModule::load(source)?.to_module())
    }

    /// Import a `.dw` from bytes and give it an explicit title.
    ///
    /// The `.dw` format carries no textual title — the byte-only
    /// [`Self::load_dw`] therefore tags every module with the same
    /// `"David Whittaker (.dw)"` placeholder, which is useless once
    /// several songs sit side by side in an editor. This variant runs
    /// the exact same importer, then overrides [`Module::name`] with
    /// `name` (an empty `name` is ignored, keeping the placeholder).
    ///
    /// Titling needs only `alloc`, so this works in `no_std` builds:
    /// a caller that already has the bytes and a name (from any
    /// source) can title the module without pulling in `std`. The
    /// std-only [`Self::load_dw_path`] is a thin convenience over this.
    #[cfg(feature = "import_dw")]
    pub fn load_dw_named(source: &[u8], name: &str) -> Result<Self, ImportError> {
        let mut module = Self::load_dw(source)?;
        if !name.is_empty() {
            module.name = name.into();
        }
        Ok(module)
    }

    /// Import a `.dw` file from disk and title it after the file name.
    ///
    /// Reads `path`, then delegates to [`Self::load_dw_named`] with the
    /// file stem (the name minus its extension, e.g. `xenon2.dw` →
    /// `xenon2`). Requires `std` for the filesystem read; the titling
    /// itself does not — see [`Self::load_dw_named`].
    #[cfg(all(feature = "import_dw", feature = "std"))]
    pub fn load_dw_path<P: AsRef<std::path::Path>>(path: P) -> Result<Self, ImportError> {
        let path = path.as_ref();
        let data = std::fs::read(path).map_err(|_| ImportError::Other("dw: cannot read file"))?;
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        Self::load_dw_named(&data, stem)
    }

    /// Try to import a Commodore 64 `.sid` file (PSID / RSID container).
    ///
    /// Three Rob Hubbard player generations are recognised, and the answer is
    /// only as good as what each one puts in the image:
    ///
    /// - The **last** generation needs nothing else — every table and every
    ///   effect comes out of the bytes ([`super::sid::g2::to_module`]).
    /// - The **three-marker** generation, its predecessor, goes through the
    ///   same reader; its pitch is not finished (see
    ///   `sid/THREE_MARKER_GENERATION.md`).
    /// - The **fx-mask** family (v10..v30) has its layout recovered by
    ///   [`super::sid::detect::SidLayout`], but its *behaviour* — row cadence,
    ///   effect semantics — is not in the image. When the file is one of the
    ///   fifteen tunes someone transcribed, it is recognised by fingerprint and
    ///   gets those settings; otherwise they stay at neutral defaults, which
    ///   means the right notes at the wrong tempo.
    ///
    /// Sub-songs become sub-songs of the returned module (`song` 0..n), the way
    /// `.dw` files already work — a `.sid` holding several tunes is one module
    /// with several songs, not several modules.
    ///
    /// One file can hold more than one replayer: Sanxion ships two, one per
    /// song. Each contributes its own sub-songs, with its own instrument table
    /// appended and rebased.
    #[cfg(feature = "import_sid")]
    pub fn load_sid(source: &[u8]) -> Result<Self, ImportError> {
        use super::sid::sid_module::{append_sub_songs, song_count, SidModule};

        if !matches!(source.get(..4), Some(b"PSID") | Some(b"RSID")) {
            return Err(ImportError::Other("sid: not a PSID/RSID file"));
        }

        // The last generation first: it is fully readable, so when it claims a
        // file there is nothing to be gained from the other paths.
        if let Some(m) = super::sid::g2::to_module(source) {
            return Ok(m);
        }

        // The importer holds the image for the module's lifetime and the caller
        // only lent us a slice, so take a copy — a few kilobytes.
        let image: alloc::borrow::Cow<'static, [u8]> = source.to_vec().into();

        // Filter first, then fold: a replayer that decodes to nothing must not
        // decide the outcome just by being listed first. Sanxion's two are
        // independent, and so are a file's chances of yielding either.
        let mut parts = SidModule::for_image(image)
            .into_iter()
            .filter_map(|p| p.to_module());
        let mut merged = parts
            .next()
            .ok_or(ImportError::Other("sid: unrecognised replayer"))?;
        for part in parts {
            let (song_base, instr_base) = (song_count(&merged), merged.instrument.len());
            merged.instrument.extend(part.instrument.iter().cloned());
            append_sub_songs(&mut merged, part, song_base, instr_base);
        }

        // A transcribed record carries the tune's title; a detected one has
        // nothing to offer and names every tune "SID". The PSID header has had
        // the answer all along — three fixed 32-byte fields — so read it rather
        // than hand a folder of rips back under one name.
        if merged.name.is_empty() || merged.name == "SID" {
            use alloc::string::String;
            let text = |off: usize| -> String {
                source
                    .get(off..off + 32)
                    .map(|b| {
                        b.iter()
                            .take_while(|&&c| c != 0)
                            .map(|&c| c as char)
                            .collect::<String>()
                            .trim()
                            .into()
                    })
                    .unwrap_or_default()
            };
            let title = text(0x16);
            if !title.is_empty() {
                merged.name = title;
            }
            merged.comment = alloc::format!("{} - {}", text(0x56), text(0x36));
        }

        Ok(merged)
    }

    /// Try to auto-detect and import any supported historical module
    /// file (SID / MOD / XM / S3M / IT / DW). SID is settled first, off its
    /// magic; the rest are tried in the order XM → S3M → IT → MOD → DW. The
    /// Amiga MOD format is tried late because its detection accepts almost
    /// anything; DW comes after MOD because its detection is structural
    /// (no magic header at all) — running it last avoids ever
    /// stealing a valid MOD parse. Only formats whose `import_*`
    /// feature is enabled are attempted.
    ///
    /// Every SID generation this crate reads goes through here now, sub-songs
    /// included — see [`Self::load_sid`] for what each one is worth. Nothing
    /// needs a dedicated entry point any more.
    pub fn load(source: &[u8]) -> Result<Self, ImportError> {
        // `.sid` is settled first and exclusively, because it is the one format
        // here that says what it is: a four-byte `PSID`/`RSID` magic no tracker
        // module can carry. Trying it last instead would let MOD — whose
        // detection "accepts almost anything" — claim a tune this crate cannot
        // decode and hand back noise. Failing here is the honest answer.
        #[cfg(feature = "import_sid")]
        if matches!(source.get(..4), Some(b"PSID") | Some(b"RSID")) {
            return Self::load_sid(source);
        }

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

        // The amiga format is tried late because it accepts almost
        // anything as a 15-sample Soundtracker variant.
        #[cfg(feature = "import_mod")]
        if let Ok(m) = Self::load_mod(source) {
            return Ok(m);
        };

        // DW has no header magic at all — its detector scans for
        // an opcode quadruple in the first few KB. Running it
        // last keeps it from intercepting a MOD that happens to
        // begin with the right bytes.
        #[cfg(feature = "import_dw")]
        if let Ok(m) = Self::load_dw(source) {
            return Ok(m);
        };

        // When no MOD/XM/S3M/IT/DW feature is enabled the whole
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
