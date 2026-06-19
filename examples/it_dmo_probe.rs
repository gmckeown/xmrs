//! Probe the raw byte layout of every mix-plugin blob under a directory
//! (default `~/Music`): DirectX-DMO slots (`id1 == 'DXMO'`) are decoded
//! to engineering values, and VST slots (`id1 == 'VstP'`) are dumped so
//! we can see whether their state is a positional float array (decodable)
//! or an opaque `effGetChunk` chunk (needs the binary).
//!
//! Since the importer now parses the full 128-byte `SNDMIXPLUGININFO`,
//! `MixPlugin.data` is the clean `pluginData` blob:
//!   `[0..4] type (=0)` · `[4..] numParams × f32 LE`.
//!
//! Run: `cargo run --example it_dmo_probe [-- <root-dir>]`

use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use xmrs::tracker::import::it::it_module::ItModule;

const DXMO: u32 = 0x4458_4D4F; // 'DXMO'
const VSTP: u32 = 0x5673_7450; // 'VstP'

/// (id2, name, param count) for the 9 OpenMPT DMOs.
fn dmo_table() -> BTreeMap<u32, (&'static str, usize)> {
    BTreeMap::from([
        (0x87FC0268u32, ("WavesReverb", 4)),
        (0xEF011F79, ("Compressor", 6)),
        (0xEF3E932C, ("Echo", 5)),
        (0xEFE6629C, ("Chorus", 7)),
        (0x120CED89, ("ParamEq", 3)),
        (0xEF114C90, ("Distortion", 5)),
        (0xEF985E71, ("I3DL2Reverb", 13)),
        (0xEFCA3D92, ("Flanger", 7)),
        (0xDAFD8210, ("Gargle", 2)),
    ])
}

fn fourcc(id: u32) -> String {
    id.to_be_bytes()
        .iter()
        .map(|&b| {
            if (0x20..=0x7E).contains(&b) {
                b as char
            } else {
                '.'
            }
        })
        .collect()
}

fn floats(data: &[u8], n: usize) -> Vec<f32> {
    (0..n)
        .filter_map(|i| {
            let o = 4 + 4 * i; // skip the u32 type tag
            data.get(o..o + 4)
                .map(|s| f32::from_le_bytes(s.try_into().unwrap()))
        })
        .collect()
}

fn main() {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            home.join("Music")
        });
    let table = dmo_table();
    let mut files = Vec::new();
    collect_it(&root, &mut files);
    files.sort();

    for path in &files {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let module = match catch_unwind(AssertUnwindSafe(|| {
            ItModule::load(&bytes).map(|m| m.to_module())
        })) {
            Ok(Ok(m)) => m,
            _ => continue,
        };
        let Some(mp) = module.mix_plugins.as_ref() else {
            continue;
        };
        let interesting = mp
            .plugins
            .iter()
            .any(|p| p.info.id1 == DXMO || p.info.id1 == VSTP);
        if !interesting {
            continue;
        }
        let assigns: Vec<(usize, u32)> = mp
            .channel_assignments
            .iter()
            .enumerate()
            .filter(|(_, &v)| v != 0)
            .map(|(i, &v)| (i, v))
            .collect();
        println!(
            "\n## {} : {} channels, channel_assignments(nonzero)={:?}",
            path.file_name().unwrap().to_string_lossy(),
            module.get_num_channels(),
            assigns
        );

        for (slot, plug) in mp.plugins.iter().enumerate() {
            let id1 = plug.info.id1;
            if id1 != DXMO && id1 != VSTP {
                continue;
            }
            let data = plug.data.as_deref().unwrap_or(&[]);
            let typ = data
                .get(0..4)
                .map(|s| u32::from_le_bytes(s.try_into().unwrap()));
            println!(
                "  slot{slot} [{}] id2={:#010X} '{}' flags={:#04X} out={:#X} name={:?} lib={:?} \
                 data.len={} type={:?}",
                fourcc(id1),
                plug.info.id2,
                fourcc(plug.info.id2),
                plug.info.routing_flags,
                plug.info.output_routing,
                plug.info.name,
                plug.info.library_name,
                data.len(),
                typ,
            );

            if id1 == DXMO {
                if let Some(&(name, n)) = table.get(&plug.info.id2) {
                    let p = floats(data, n);
                    println!("    {name}: {p:?}");
                    print_engineering(name, &p);
                }
            } else {
                // VST: show whether it's a positional float array
                // (type==0 ⇒ decodable) or an opaque chunk, plus a dump.
                match typ {
                    Some(0) => println!("    VST stores POSITIONAL PARAMS (type=0) — decodable"),
                    Some(t) => println!(
                        "    VST stores an OPAQUE CHUNK (type={:#X} '{}') — needs the binary",
                        t,
                        fourcc(t)
                    ),
                    None => {}
                }
                for (row, chunk) in data.chunks(16).take(8).enumerate() {
                    let hex: String = chunk.iter().map(|b| format!("{b:02X} ")).collect();
                    let ascii: String = chunk
                        .iter()
                        .map(|&b| {
                            if (0x20..0x7F).contains(&b) {
                                b as char
                            } else {
                                '.'
                            }
                        })
                        .collect();
                    println!("      {:04X}  {hex:<48} {ascii}", row * 16);
                }
            }
        }
    }
}

fn print_engineering(name: &str, p: &[f32]) {
    match (name, p.len()) {
        ("ParamEq", 3) => println!(
            "    → center={:.0}Hz bw={:.1}semi gain={:.1}dB",
            80.0 + p[0] * 15920.0,
            1.0 + p[1] * 35.0,
            (p[2] - 0.5) * 30.0
        ),
        ("Echo", 5) => println!(
            "    → wet={:.0}% fb={:.0}% L={:.0}ms R={:.0}ms pan={}",
            p[0] * 100.0,
            p[1] * 100.0,
            1.0 + p[2] * 1999.0,
            1.0 + p[3] * 1999.0,
            p[4] > 0.5
        ),
        ("WavesReverb", 4) => println!(
            "    → inGain={:.1}dB mix={:.1}dB time={:.0}ms hfRatio={:.3}",
            -96.0 + p[0] * 96.0,
            -96.0 + p[1] * 96.0,
            0.001 + p[2] * 2999.999,
            0.001 + p[3] * 0.998
        ),
        ("Compressor", 6) => println!(
            "    → gain={:.1}dB atk={:.2}ms rel={:.0}ms thr={:.1}dB ratio={:.1}",
            -60.0 + p[0] * 120.0,
            0.01 + p[1] * 499.99,
            50.0 + p[2] * 2950.0,
            -60.0 + p[3] * 60.0,
            1.0 + p[4] * 99.0
        ),
        ("Gargle", 2) => println!(
            "    → rate={}Hz shape={}",
            (p[0] * 999.0 + 0.5) as u32 + 1,
            if p[1] < 0.5 { "triangle" } else { "square" }
        ),
        _ => {}
    }
}

fn collect_it(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            collect_it(&p, out);
        } else if p
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| x.eq_ignore_ascii_case("it"))
        {
            out.push(p);
        }
    }
}
