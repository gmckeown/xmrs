//! The assembler — `Module` ⇄ `.xmr` bytes. It walks the model, encodes each
//! non-default field into its §3 chunk (CBOR payload), frames them with the
//! [`container`] writer, and on read reverses the process,
//! returning a [`LoadReport`] so any dropped data is *visible* (§8).
//!
//! **PCM separation (§4).** Every `Sample`'s PCM is stripped out of its
//! structural chunk and interned into the raw `PCM ` region (see
//! [`super::pcm`]), covering all three sources of sample data: instrument
//! samples (`INST`), `Track::Audio` inline waveforms (`TRKS`), and the shared
//! asset pool (`ASET`). Each such chunk is `(stripped structure, Vec<ref>)`;
//! the refs point into the one shared, deduplicated region. Stripping works on
//! clones, so the caller's `Module` is never mutated.
//!
//! `INST` / `TRKS` / `ASET` ride the model's `Serialize` for everything but the
//! PCM, and that is now feature-independent: `InstrumentType`'s variant set is
//! stable in every build (the synth *data types* are always compiled; only the
//! render engines gate on `synth_*` — §5.1/§5.2). So a `.xmr` with a SID/OPL
//! instrument round-trips even in a build that cannot synthesise it.

use alloc::vec::Vec;

use crate::core::daw::device::{Bus, DeviceChain, Send};
use crate::core::daw::loop_region::ChannelLoop;
use crate::core::daw::track::{AudioSource, Track};
use crate::core::instrument::{Instrument, InstrumentType};
use crate::core::module::Module;
use crate::core::sample::Sample;

use super::container::{self, Writer};
use super::pcm::{AssetSampleRef, InstSampleRef, PcmRegion, PcmResolver, TrackSampleRef};
use super::wire::v1;
use super::wire::{from_cbor, to_cbor};
use super::{FormatError, SCHEMA_VERSION};

/// What a load did beyond producing the `Module` — the visibility §8 demands,
/// so an editor can warn before a save-in-place silently drops data. Empty for
/// a file this build fully understands.
///
/// It is also what carries the §8 retention across a load/save: the danger of a
/// tolerant reader is not the reading, it is the **saving**, and the report is
/// produced at open time while the damage happens at save time. Nothing else
/// bridges the two, so the retained bytes live here — see [`Self::retained`]
/// and [`write_preserving`].
///
/// `#[non_exhaustive]`: only [`read`] ever builds one, so forbidding the struct
/// literal downstream costs nothing and leaves room to grow.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LoadReport {
    /// Ancillary chunks this build cannot reproduce from the module but **may
    /// copy verbatim** into a rewritten file — byte 3 of the id is lowercase
    /// (§2.1). Feed the report to [`write_preserving`] and they survive
    /// untouched; call plain [`write()`] and they are gone.
    ///
    /// Two kinds land here, and the second is easy to miss:
    ///
    /// * chunks this build does not **know** — written by a later version;
    /// * chunks it knows and parsed correctly but has **nowhere to put**,
    ///   because the model field is `#[cfg]`-gated out. `orgn` in a build with
    ///   no importer is the case that exists today.
    ///
    /// Both fail the same way on a rewrite — the writer regenerates chunks from
    /// the model, and neither is in it — so both need the same carrying.
    ///
    /// Kept in file order.
    pub retained: Vec<OpaqueChunk>,

    /// Unknown ancillary chunk ids this build must **not** copy: byte 3 of the
    /// id is uppercase, meaning the chunk may depend on data that just changed
    /// (§2.1). Their payload is deliberately not kept — re-emitting it would be
    /// worse than losing it. A rewrite loses these, and this list is the only
    /// warning anyone will get.
    pub dropped: Vec<[u8; 4]>,

    /// The first cross-chunk inconsistency in the assembled module, if any.
    ///
    /// Each chunk validates on its own — framing, CBOR, every `PcmRef` against
    /// the blob region — but a `Clip` naming a track that does not exist spans
    /// two chunks, so nothing catches it on the way in. A `.xmr` is an
    /// untrusted byte string like any other file, and handing back a module
    /// that cannot be played without saying so is not tolerance, it is silence.
    ///
    /// Reported rather than fatal: an editor opening a damaged project in order
    /// to repair it needs the module, not an error. A consumer that will hand
    /// the module straight to the player should treat this as a refusal — or
    /// simply check [`Self::is_clean`], which covers both halves.
    pub inconsistency: Option<crate::core::module::LayerInconsistency>,
}

impl LoadReport {
    /// `true` when this build understood the file in full: no unknown chunk of
    /// either kind, and the assembled module's layers agree with each other.
    ///
    /// Stricter than [`Self::rewrite_is_lossless`] on purpose — a retained
    /// chunk survives a rewrite but this build still could not put it in the
    /// module, and an editor showing "opened, with reservations" wants to know.
    pub fn is_clean(&self) -> bool {
        self.retained.is_empty() && self.dropped.is_empty() && self.inconsistency.is_none()
    }

    /// `true` when saving through [`write_preserving`] loses nothing: every
    /// chunk this build could not put in the module was safe to copy and is
    /// being carried.
    ///
    /// This is the question an editor actually needs answered before a
    /// save-in-place. `false` means the file holds something this build cannot
    /// understand *and* is forbidden to copy — say so before overwriting the
    /// only copy of it.
    pub fn rewrite_is_lossless(&self) -> bool {
        self.dropped.is_empty()
    }
}

/// A chunk kept verbatim so a rewrite can put it back (§8).
///
/// Opaque *to the writer*, which is the only thing that matters here: whether
/// this build failed to recognise the id or merely has no field to store it in,
/// it cannot regenerate these bytes from the model — so it must not touch them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpaqueChunk {
    pub id: crate::format::ChunkId,
    pub payload: Vec<u8>,
}

/// Serialise a `Module` to a `.xmr` byte image. A field at its default is
/// omitted (§3): the reader starts from [`Module::default`] and applies only
/// the chunks present, so an absent chunk restores the default.
///
/// Writes only what this build knows. **Saving a file back over one you loaded
/// wants [`write_preserving`]** — otherwise anything the file carried that this
/// build did not understand is dropped, even the parts it was allowed to keep.
pub fn write(module: &Module) -> Result<Vec<u8>, FormatError> {
    write_preserving(module, &LoadReport::default())
}

/// [`write()`], plus the opaque chunks a [`read`] set aside — the save half of §8.
///
/// A reader that skips what it does not know is only being tolerant if the
/// writer puts it back; otherwise it is destroying politely. Pass the report
/// that came with the module and every [`LoadReport::retained`] chunk is
/// re-emitted **byte for byte**. Their *position* is not preserved (they are
/// grouped just before the provenance chunk); §8 requires the payload to be
/// verbatim, not the offset.
///
/// [`LoadReport::dropped`] chunks are not written — that is what their id said.
///
/// Errors with [`FormatError::DuplicateChunk`] if a retained id collides with
/// one this build writes itself, which can only happen if the report came from
/// a different file than the module. Better a loud error than a silent choice
/// between two versions of the same chunk.
pub fn write_preserving(module: &Module, report: &LoadReport) -> Result<Vec<u8>, FormatError> {
    let mut w = Writer::new(SCHEMA_VERSION, v1::min_reader_version(module));

    w.push(v1::MHDR, to_cbor(&v1::MHdr::from_module(module))?)?;

    if module.quirks != Default::default() {
        w.push(v1::QRKS, to_cbor(&module.quirks)?)?;
    }

    // Strip PCM out of all three sample sources into one shared region,
    // working on clones so `module` is untouched.
    let mut region = PcmRegion::default();

    if !module.instrument.is_empty() {
        let mut instruments = module.instrument.clone();
        let refs = strip_instrument_pcm(&mut instruments, &mut region);
        w.push(v1::INST, to_cbor(&(&instruments, &refs))?)?;
    }
    if !module.tracks.is_empty() {
        let mut tracks = module.tracks.clone();
        let refs = strip_track_pcm(&mut tracks, &mut region);
        w.push(v1::TRKS, to_cbor(&(&tracks, &refs))?)?;
    }
    if !module.assets.is_empty() {
        let mut assets = module.assets.clone();
        let refs = strip_asset_pcm(&mut assets, &mut region);
        w.push(v1::ASET, to_cbor(&(&assets, &refs))?)?;
    }

    if !module.clips.is_empty() {
        w.push(v1::CLIP, to_cbor(&module.clips)?)?;
    }
    if !module.automation.is_empty() {
        w.push(v1::AUTO, to_cbor(&module.automation)?)?;
    }
    if !module.timeline_map.entries.is_empty() {
        w.push(v1::TMAP, to_cbor(&module.timeline_map)?)?;
    }
    if module.song_loop_to.is_some() || !module.channel_loops.is_empty() {
        w.push(
            v1::LOOP,
            to_cbor(&(module.song_loop_to, &module.channel_loops))?,
        )?;
    }
    if !module.channel_inserts.is_empty()
        || !module.buses.is_empty()
        || !module.channel_sends.is_empty()
        || !module.master_chain.devices.is_empty()
    {
        let graf = (
            &module.channel_inserts,
            &module.buses,
            &module.channel_sends,
            &module.master_chain,
        );
        w.push(v1::GRAF, to_cbor(&graf)?)?;
    }
    if let Some(mm) = &module.midi_macros {
        w.push(v1::MIDI, to_cbor(mm)?)?;
    }
    if let Some(origin) = v1::origin_of(module) {
        w.push(v1::ORGN, to_cbor(&origin)?)?;
    }

    // The raw blob region goes last among the critical chunks (§2): the writer
    // streams structure before bulk. Raw bytes, never CBOR.
    if !region.is_empty() {
        w.push(v1::PCM, region.into_bytes())?;
    }

    // Opaque chunks the reader was allowed to keep, in the order the source
    // file had them. After our own data, before the provenance chunk.
    for c in &report.retained {
        w.push(c.id, c.payload.clone())?;
    }

    // Provenance is written last and always — it is regenerated per write, so
    // the output stays byte-stable across a load/save round-trip.
    w.push(v1::GENR, to_cbor(&v1::Provenance::current())?)?;

    Ok(w.into_bytes())
}

// ---- write-side PCM stripping ---------------------------------------------

/// Move each instrument sample's PCM into `region`, leaving `data = None`, and
/// return the refs. Only `InstrumentType::Default` carries samples.
fn strip_instrument_pcm(
    instruments: &mut [Instrument],
    region: &mut PcmRegion,
) -> Vec<InstSampleRef> {
    let mut refs = Vec::new();
    for (i, instr) in instruments.iter_mut().enumerate() {
        if let InstrumentType::Default(d) = &mut instr.instr_type {
            for (j, slot) in d.sample.iter_mut().enumerate() {
                if let Some(sample) = slot {
                    if let Some(data) = sample.data.take() {
                        refs.push(InstSampleRef {
                            instrument: i as u32,
                            sample: j as u32,
                            pcm: region.intern(&data),
                        });
                    }
                }
            }
        }
    }
    refs
}

/// Move each `Track::Audio` inline waveform's PCM into `region`. `Pooled`
/// tracks reference the asset pool and carry no PCM of their own.
fn strip_track_pcm(tracks: &mut [Track], region: &mut PcmRegion) -> Vec<TrackSampleRef> {
    let mut refs = Vec::new();
    for (t, track) in tracks.iter_mut().enumerate() {
        if let Track::Audio {
            source: AudioSource::Inline(sample),
            ..
        } = track
        {
            if let Some(data) = sample.data.take() {
                refs.push(TrackSampleRef {
                    track: t as u32,
                    pcm: region.intern(&data),
                });
            }
        }
    }
    refs
}

/// Move each asset's PCM into `region`.
fn strip_asset_pcm(assets: &mut [Sample], region: &mut PcmRegion) -> Vec<AssetSampleRef> {
    let mut refs = Vec::new();
    for (k, sample) in assets.iter_mut().enumerate() {
        if let Some(data) = sample.data.take() {
            refs.push(AssetSampleRef {
                asset: k as u32,
                pcm: region.intern(&data),
            });
        }
    }
    refs
}

/// Parse a `.xmr` image back into a `Module`, plus a [`LoadReport`]. Refuses
/// any unknown *critical* chunk (§2.1) and reports skipped ancillary ones.
///
/// Two phases: first the chunks are decoded into the model with samples left
/// PCM-less and the refs / blob region stashed; then the PCM is re-attached
/// (the blob region may sit after the structural chunks in file order).
pub fn read(bytes: &[u8]) -> Result<(Module, LoadReport), FormatError> {
    let container = container::read(bytes)?;
    container.require_known(v1::KNOWN)?;

    let mut m = Module::default();
    let mut report = LoadReport::default();

    let mut inst_refs: Vec<InstSampleRef> = Vec::new();
    let mut track_refs: Vec<TrackSampleRef> = Vec::new();
    let mut asset_refs: Vec<AssetSampleRef> = Vec::new();
    let mut pcm_region: &[u8] = &[];

    for chunk in &container.chunks {
        let id = chunk.id;
        let p = chunk.payload;

        if id == v1::MHDR {
            from_cbor::<v1::MHdr>(p)?.apply(&mut m);
        } else if id == v1::QRKS {
            m.quirks = from_cbor(p)?;
        } else if id == v1::INST {
            let (instruments, refs): (Vec<Instrument>, Vec<InstSampleRef>) = from_cbor(p)?;
            m.instrument = instruments;
            inst_refs = refs;
        } else if id == v1::TRKS {
            let (tracks, refs): (Vec<Track>, Vec<TrackSampleRef>) = from_cbor(p)?;
            m.tracks = tracks;
            track_refs = refs;
        } else if id == v1::ASET {
            let (assets, refs): (Vec<Sample>, Vec<AssetSampleRef>) = from_cbor(p)?;
            m.assets = assets;
            asset_refs = refs;
        } else if id == v1::PCM {
            pcm_region = p;
        } else if id == v1::CLIP {
            m.clips = from_cbor(p)?;
        } else if id == v1::AUTO {
            m.automation = from_cbor(p)?;
        } else if id == v1::TMAP {
            m.timeline_map = from_cbor(p)?;
        } else if id == v1::LOOP {
            let (song_loop_to, channel_loops): (Option<u32>, Vec<ChannelLoop>) = from_cbor(p)?;
            m.song_loop_to = song_loop_to;
            m.channel_loops = channel_loops;
        } else if id == v1::GRAF {
            let (inserts, buses, sends, master): (
                Vec<DeviceChain>,
                Vec<Bus>,
                Vec<Vec<Send>>,
                DeviceChain,
            ) = from_cbor(p)?;
            m.channel_inserts = inserts;
            m.buses = buses;
            m.channel_sends = sends;
            m.master_chain = master;
        } else if id == v1::MIDI {
            m.midi_macros = Some(from_cbor(p)?);
        } else if id == v1::ORGN {
            // The one known chunk whose model field may not exist: `origin` is
            // `#[cfg]`-gated on the importers. A build with none parses the
            // chunk but has nowhere to put it, so re-writing would silently
            // drop it. Carry the bytes instead — `orgn` is safe to copy, and
            // §8's machinery is exactly the right place for "understood, but
            // this build cannot reproduce it".
            if !v1::apply_origin(from_cbor(p)?, &mut m) {
                report.retained.push(OpaqueChunk {
                    id,
                    payload: p.to_vec(),
                });
            }
        } else if id == v1::GENR {
            // Provenance is informational (§6) — recorded on the file, never
            // applied to the model, never gates a compat decision.
        } else if id.is_ancillary() {
            // §2.1's second case bit, and the only place it means anything: it
            // is a *rewrite* instruction, not a read one. Lowercase byte 3 ⇒
            // carry the bytes so a save puts them back; uppercase ⇒ the chunk
            // may depend on data that changed, so keeping it would be worse
            // than losing it — record the loss instead.
            if id.is_safe_to_copy() {
                report.retained.push(OpaqueChunk {
                    id,
                    payload: p.to_vec(),
                });
            } else {
                report.dropped.push(id.0);
            }
        } else {
            // A critical id that passed `require_known` yet is unhandled means
            // `KNOWN` and this match drifted out of sync — never drop it
            // silently.
            return Err(FormatError::UnknownCritical(id.0));
        }
    }

    reattach_pcm(&mut m, pcm_region, &inst_refs, &track_refs, &asset_refs)?;
    // Cross-chunk check, once the whole module exists — no earlier point can
    // see both a clip and the track it names.
    report.inconsistency = m.verify_layers_consistent().err();
    Ok((m, report))
}

/// Phase two of [`read`]: reconstruct each stripped sample's PCM from the blob
/// region. A ref that points at a missing sample slot is a malformed file.
fn reattach_pcm(
    m: &mut Module,
    region: &[u8],
    inst_refs: &[InstSampleRef],
    track_refs: &[TrackSampleRef],
    asset_refs: &[AssetSampleRef],
) -> Result<(), FormatError> {
    let mut res = PcmResolver::new(region);

    for r in inst_refs {
        let data = res.resolve(r.pcm)?;
        let slot = m
            .instrument
            .get_mut(r.instrument as usize)
            .and_then(|instr| match &mut instr.instr_type {
                InstrumentType::Default(d) => d.sample.get_mut(r.sample as usize),
                _ => None,
            })
            .and_then(|slot| slot.as_mut())
            .ok_or(FormatError::Pcm("instrument sample ref has no target"))?;
        slot.data = Some(data);
    }
    for r in track_refs {
        let data = res.resolve(r.pcm)?;
        match m.tracks.get_mut(r.track as usize) {
            Some(Track::Audio {
                source: AudioSource::Inline(sample),
                ..
            }) => sample.data = Some(data),
            _ => return Err(FormatError::Pcm("track sample ref has no target")),
        }
    }
    for r in asset_refs {
        let data = res.resolve(r.pcm)?;
        let sample = m
            .assets
            .get_mut(r.asset as usize)
            .ok_or(FormatError::Pcm("asset sample ref has no target"))?;
        sample.data = Some(data);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_module_round_trips_byte_stable() {
        let m = Module::default();
        let bytes1 = write(&m).unwrap();
        let (m2, report) = read(&bytes1).unwrap();
        assert!(report.is_clean());
        let bytes2 = write(&m2).unwrap();
        assert_eq!(bytes1, bytes2, "load/save must be byte-stable");
    }

    #[test]
    fn structural_fields_survive_round_trip() {
        let m = Module {
            name: "song".into(),
            comment: "notes".into(),
            default_bpm: 140,
            channel_names: alloc::vec!["kick".into(), "snare".into()],
            ..Module::default()
        };
        let (m2, _) = read(&write(&m).unwrap()).unwrap();
        assert_eq!(m2.name, "song");
        assert_eq!(m2.comment, "notes");
        assert_eq!(m2.default_bpm, 140);
        assert_eq!(m2.channel_names.len(), 2);
    }

    /// Every instrument kind survives a save/load, with its payload.
    ///
    /// `INST` carries `Vec<Instrument>` through the model's own derive, so a new
    /// `InstrumentType` variant rides along for free — which is exactly why it
    /// can be added without anyone noticing it was never round-tripped. The
    /// imported-XM test above only exercises `Default`. This one puts one of
    /// each in a module and checks a distinguishing field comes back, so a
    /// variant that stops surviving says so.
    #[test]
    fn every_instrument_kind_survives_a_round_trip() {
        use crate::core::instr_midi::InstrMidi;
        use crate::core::instr_opl::InstrOpl;
        use crate::core::instr_robsid::{InstrRobSid, WaveShape};
        use crate::core::instr_sid::InstrSid;

        let midi = InstrMidi {
            channel: 9,
            program: 42,
            ..Default::default()
        };

        let opl = InstrOpl {
            volume: 61,
            relative_pitch: -12,
            ..Default::default()
        };

        let sid = InstrSid {
            fc: 1234,
            band_pass: true,
            ..Default::default()
        };

        // The RobSid payload the curation pass reshaped: a named waveform where
        // a raw control byte used to be.
        let mut robsid = InstrRobSid::default();
        robsid.fx.two_phase.attack_shape = WaveShape {
            noise: true,
            gate: true,
            ..Default::default()
        };
        robsid.fx.two_phase.attack_frames = 4;
        robsid.fx.filter.mode.band_pass = true;
        robsid.fx.filter.routing.voice2 = true;

        // A sample-based instrument carrying real PCM, so the round-trip covers
        // the §4 separation too: the wave data is stripped into the shared `PCM `
        // region and reattached by reference, never inlined in `INST`.
        use crate::core::fixed::units::{ChannelVolume, Panning, Volume};
        use crate::core::sample::{LoopType, Sample, SampleDataType};
        const WAVE: [i8; 6] = [0, 40, -40, 127, -128, 7];
        let pcm = crate::core::instr_default::InstrDefault {
            sample: alloc::vec![Some(Sample {
                name: "wave".into(),
                relative_pitch: 0,
                finetune: crate::core::fixed::units::Finetune::ZERO,
                volume: ChannelVolume::FULL,
                default_note_volume: Volume::FULL,
                panning: Panning::CENTER,
                loop_flag: LoopType::No,
                loop_start: 0,
                loop_length: 0,
                sustain_loop_flag: LoopType::No,
                sustain_loop_start: 0,
                sustain_loop_length: 0,
                data: Some(SampleDataType::Mono8(WAVE.to_vec().into())),
            })],
            ..Default::default()
        };

        let kinds = [
            InstrumentType::Empty,
            InstrumentType::Default(pcm),
            InstrumentType::Midi(midi),
            InstrumentType::Opl(opl),
            InstrumentType::Sid(sid),
            InstrumentType::RobSid(robsid),
        ];
        let m = Module {
            instrument: kinds
                .into_iter()
                .enumerate()
                .map(|(i, t)| Instrument {
                    name: alloc::format!("i{i}"),
                    instr_type: t,
                    ..Default::default()
                })
                .collect(),
            ..Module::default()
        };

        let bytes = write(&m).expect("write");
        let (m2, report) = read(&bytes).expect("read");
        assert!(report.is_clean());
        assert_eq!(m2.instrument.len(), 6);
        assert_eq!(write(&m2).expect("rewrite"), bytes, "byte-stable");

        // Each variant came back as itself, carrying its payload.
        assert!(matches!(m2.instrument[0].instr_type, InstrumentType::Empty));
        // The PCM came back through the shared region, sample for sample.
        assert!(
            super::super::container::read(&bytes)
                .unwrap()
                .chunk(super::super::wire::v1::PCM)
                .is_some(),
            "wave data belongs in the PCM region, not inline in INST"
        );
        match &m2.instrument[1].instr_type {
            InstrumentType::Default(i) => {
                let s = i.sample[0].as_ref().expect("the sample survived");
                assert_eq!(s.name, "wave");
                match s.data.as_ref().expect("its PCM survived") {
                    SampleDataType::Mono8(v) => assert_eq!(&v[..], &WAVE[..]),
                    other => panic!("PCM came back as {other:?}"),
                }
            }
            other => panic!("sample instrument became {other:?}"),
        }
        match &m2.instrument[2].instr_type {
            InstrumentType::Midi(i) => assert_eq!((i.channel, i.program), (9, 42)),
            other => panic!("midi became {other:?}"),
        }
        match &m2.instrument[3].instr_type {
            InstrumentType::Opl(i) => assert_eq!((i.volume, i.relative_pitch), (61, -12)),
            other => panic!("opl became {other:?}"),
        }
        match &m2.instrument[4].instr_type {
            InstrumentType::Sid(i) => assert_eq!((i.fc, i.band_pass), (1234, true)),
            other => panic!("sid became {other:?}"),
        }
        match &m2.instrument[5].instr_type {
            InstrumentType::RobSid(i) => {
                let a = i.fx.two_phase.attack_shape;
                assert!(a.noise && a.gate && !a.is_silent());
                assert_eq!(i.fx.two_phase.attack_frames, 4);
                assert!(i.fx.filter.mode.band_pass && i.fx.filter.routing.voice2);
            }
            other => panic!("robsid became {other:?}"),
        }
    }

    /// A file whose chunks are each impeccable and which together describe an
    /// unplayable module is reported, not swallowed.
    ///
    /// `read` still returns the module — an editor opening a damaged project to
    /// repair it needs it — but [`LoadReport::is_clean`] is false and the
    /// verdict is in hand, so no caller can hand the thing to the player
    /// without having been told.
    #[test]
    fn read_reports_cross_chunk_inconsistency() {
        use crate::core::daw::clip::Clip;
        use crate::core::daw::sorted_clips::SortedClips;

        let m = Module {
            // A clip on track 99 of a module with no tracks at all.
            clips: SortedClips::from_unsorted(alloc::vec![Clip {
                track: 99,
                song: 0,
                target_channel: 0,
                position_tick: 0,
                speed_at_start: 6,
                track_row_offset: 0,
                source_start_row: 0,
                end_tick: 64,
            }]),
            ..Module::default()
        };

        let (m2, report) = read(&write(&m).expect("write")).expect("read");
        assert!(
            report.retained.is_empty() && report.dropped.is_empty(),
            "structurally the file is impeccable"
        );
        assert!(
            matches!(
                report.inconsistency,
                Some(
                    crate::core::module::LayerInconsistency::ClipTrackOutOfRange { track: 99, .. }
                )
            ),
            "…and semantically it is not, which the report says: {:?}",
            report.inconsistency
        );
        assert!(!report.is_clean());
        // The module is still handed back, damage and all.
        assert_eq!(m2.clips.len(), 1);
    }

    /// Build a file this version could plausibly meet from a later one: our own
    /// output plus two chunks we know nothing about — one flagged safe to copy,
    /// one not.
    fn file_from_the_future(m: &Module) -> Vec<u8> {
        use super::super::container;
        let raw = write(m).expect("write");
        let ours = container::read(&raw).expect("read back");
        let mut w = container::Writer::new(ours.header.schema_version, 1);
        for c in &ours.chunks {
            w.push(c.id, c.payload.to_vec()).unwrap();
        }
        // `hstc`-style: ancillary (byte 0 lower), safe to copy (byte 3 lower).
        w.push(
            crate::format::ChunkId::new(b"xtra"),
            alloc::vec![1, 2, 3, 4],
        )
        .unwrap();
        // Ancillary but NOT safe to copy (byte 3 upper): may depend on data we
        // just rewrote.
        w.push(crate::format::ChunkId::new(b"xtrA"), alloc::vec![9, 9])
            .unwrap();
        w.into_bytes()
    }

    /// The §8 contract, end to end: a chunk we cannot read but are allowed to
    /// keep survives a load/save; one we are forbidden to keep does not, and
    /// the report says so *before* the save rather than after.
    #[test]
    fn unknown_safe_to_copy_chunks_survive_a_rewrite() {
        let m = Module {
            name: "future".into(),
            ..Module::default()
        };
        let bytes = file_from_the_future(&m);

        let (m2, report) = read(&bytes).expect("an unknown ancillary chunk is not fatal");
        assert_eq!(report.retained.len(), 1);
        assert_eq!(report.retained[0].id.0, *b"xtra");
        assert_eq!(report.retained[0].payload, alloc::vec![1, 2, 3, 4]);
        assert_eq!(report.dropped, alloc::vec![*b"xtrA"]);

        // Understood in full? No. Lossless to save? Also no — and the two
        // questions have different answers, which is the point of asking both.
        assert!(!report.is_clean());
        assert!(!report.rewrite_is_lossless());

        // Save it back the way an editor should.
        let saved = write_preserving(&m2, &report).expect("rewrite");
        let (_, report2) = read(&saved).expect("read back");
        assert_eq!(
            report2.retained, report.retained,
            "the opaque payload came through byte for byte"
        );
        assert!(
            report2.dropped.is_empty(),
            "the not-safe-to-copy chunk is gone, as its own id demanded"
        );

        // And the negligent path, for contrast: plain `write` loses both.
        let (_, report3) = read(&write(&m2).expect("write")).expect("read back");
        assert!(report3.is_clean(), "nothing unknown left — nothing kept");
    }

    /// A container carrying an `orgn` chunk, built without needing the
    /// `Module::origin` field — the wire type is compiled into every build even
    /// when the model field is not, which is what makes this testable on both
    /// sides of the `#[cfg]`.
    fn file_with_orgn(origin: super::super::wire::v1::Origin) -> Vec<u8> {
        use super::super::container;
        let raw = write(&Module::default()).expect("write");
        let base = container::read(&raw).expect("read back");
        let mut w = container::Writer::new(base.header.schema_version, 1);
        for c in &base.chunks {
            if c.id == v1::ORGN {
                continue; // a default module writes none, but do not assume it
            }
            w.push(c.id, c.payload.to_vec()).unwrap();
        }
        w.push(v1::ORGN, to_cbor(&origin).unwrap()).unwrap();
        w.into_bytes()
    }

    /// Provenance survives a rewrite in a build that has **nowhere to store
    /// it**.
    ///
    /// `Module::origin` is `#[cfg]`-gated on the importers, so a format-only
    /// build parses `orgn` and can do nothing with it. Regenerating the file
    /// from the model would drop a chunk the reader understood perfectly —
    /// so it is carried instead, and the report says the file was not
    /// understood in full while a preserving save stays lossless.
    #[cfg(not(any(
        feature = "import_mod",
        feature = "import_xm",
        feature = "import_s3m",
        feature = "import_it",
        feature = "import_sid",
        feature = "import_dw",
    )))]
    #[test]
    fn orgn_is_carried_when_this_build_cannot_store_it() {
        use super::super::container;
        let bytes = file_with_orgn(super::super::wire::v1::Origin::It);

        let (m, report) = read(&bytes).expect("read");
        assert_eq!(report.retained.len(), 1, "orgn must be carried, not lost");
        assert_eq!(report.retained[0].id, v1::ORGN);
        assert!(
            !report.is_clean(),
            "a chunk we could not apply is a reservation"
        );
        assert!(
            report.rewrite_is_lossless(),
            "carried, so saving loses nothing"
        );

        let saved = write_preserving(&m, &report).expect("rewrite");
        let c = container::read(&saved).expect("read back");
        assert_eq!(
            c.chunk(v1::ORGN).map(|c| c.payload.to_vec()),
            Some(to_cbor(&super::super::wire::v1::Origin::It).unwrap()),
            "the provenance came through byte for byte"
        );

        // For contrast, the negligent path really does lose it.
        let plain_bytes = write(&m).expect("write");
        let plain = container::read(&plain_bytes).expect("read back");
        assert!(plain.chunk(v1::ORGN).is_none());
    }

    /// The mirror case: a build that **can** store it must not carry it too.
    ///
    /// Retaining an applied chunk would make `write_preserving` emit `orgn`
    /// twice — once from the model, once from the report — which the container
    /// rejects outright as a duplicate id. Cheap to get wrong, loud when wrong,
    /// so it is pinned here.
    #[cfg(any(
        feature = "import_mod",
        feature = "import_xm",
        feature = "import_s3m",
        feature = "import_it",
        feature = "import_sid",
        feature = "import_dw",
    ))]
    #[test]
    fn orgn_is_applied_not_carried_when_the_field_exists() {
        use super::super::container;
        let bytes = file_with_orgn(super::super::wire::v1::Origin::It);

        let (m, report) = read(&bytes).expect("read");
        assert!(
            report.retained.is_empty(),
            "applied to the model, so nothing to carry"
        );
        assert!(report.is_clean());
        assert_eq!(m.origin, Some(crate::tracker::format::ModuleFormat::It));

        // Both save paths must produce exactly one `orgn`, from the model.
        for saved in [
            write(&m).expect("write"),
            write_preserving(&m, &report).expect("rewrite"),
        ] {
            let c = container::read(&saved).expect("read back");
            assert_eq!(
                c.chunks.iter().filter(|c| c.id == v1::ORGN).count(),
                1,
                "exactly one provenance chunk"
            );
        }
    }

    // A real imported module exercises INST / TRKS / TMAP + PCM separation.
    #[cfg(feature = "import_xm")]
    #[test]
    fn imported_xm_round_trips_end_to_end() {
        use super::super::container;
        use super::super::wire::v1;

        let data = include_bytes!("../../examples/note.xm");
        let m = Module::load_xm(data).expect("load note.xm");
        let bytes1 = write(&m).expect("write");

        // The instrument samples' PCM landed in a dedicated raw region, not
        // inline in INST (§4).
        let c = container::read(&bytes1).unwrap();
        assert!(
            c.chunk(v1::PCM).is_some(),
            "note.xm carries sample data → a separate PCM chunk"
        );

        let (m2, report) = read(&bytes1).expect("read");
        assert!(report.is_clean());

        // Byte-stable: writing the re-read module reproduces the same image.
        // This is what proves the PCM round-tripped — had a blob been dropped
        // or mis-resolved, the second write's PCM chunk would differ.
        let bytes2 = write(&m2).expect("rewrite");
        assert_eq!(bytes1, bytes2, "round-trip must be byte-stable");

        // Content preserved across the round-trip.
        assert_eq!(m.name, m2.name);
        assert_eq!(m.instrument.len(), m2.instrument.len());
        assert_eq!(m.tracks.len(), m2.tracks.len());
        assert_eq!(m.timeline_map.entries.len(), m2.timeline_map.entries.len());
    }
}
