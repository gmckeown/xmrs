//! Census of the VST / mix-plugin IDs referenced by every `.it`
//! module under a directory (default `~/Music`).
//!
//! Impulse Tracker files can carry OpenMPT's `XTPM` mix-plugin table.
//! Each occupied slot identifies a plugin by two packed 32-bit codes:
//!
//! * `id1` — the plugin **type / host** magic. `'VstP'` marks a VST
//!   2.x slot; other magics (`'DMO '`, the OpenMPT-native four-CCs,
//!   …) mark non-VST effects.
//! * `id2` — the plugin's **unique ID**. For a VST that is the
//!   vendor-assigned `uniqueID` (itself usually a four-character
//!   code), so `id2` is what actually says *which* plugin it is.
//!
//! xmrs keeps both as raw `u32`s plus the plugin's parameter blob and
//! its name fields (see [`xmrs::tracker::mix_plugin`]). This tool walks
//! the tree, parses each `.it`, and tallies every non-empty slot by
//! `(id1, id2)`, reporting how many slot instances and how many distinct
//! files use each ID, plus the plugin's library/display name.
//!
//! Run:  `cargo run --example it_vst_census [-- <root-dir>]`

use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use xmrs::tracker::import::it::it_module::ItModule;

/// Aggregated stats for one `(id1, id2)` plugin identity.
#[derive(Default)]
struct Agg {
    /// Total occupied slots carrying this ID across all files.
    slots: usize,
    /// Number of distinct `.it` files that reference this ID.
    files: usize,
    /// First plausible human name scraped from a state blob.
    name_hint: Option<String>,
}

fn main() -> ExitCode {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(default_music_dir);

    if !root.is_dir() {
        eprintln!("not a directory: {}", root.display());
        return ExitCode::from(2);
    }
    eprintln!("scanning {} for .it files…", root.display());

    let mut it_files = Vec::new();
    collect_it_files(&root, &mut it_files);
    it_files.sort();
    eprintln!("found {} .it files", it_files.len());

    let mut scanned = 0usize;
    let mut load_fail = 0usize;
    let mut failed_paths: Vec<PathBuf> = Vec::new();
    let mut panics = 0usize;
    let mut with_table = 0usize;
    let mut total_slots = 0usize;
    let mut census: BTreeMap<(u32, u32), Agg> = BTreeMap::new();

    for path in &it_files {
        scanned += 1;
        let Ok(bytes) = std::fs::read(path) else {
            load_fail += 1;
            failed_paths.push(path.clone());
            continue;
        };
        // `ItModule::load` returns a Result; `to_module` can in theory
        // panic on a malformed file, so isolate the whole parse so one
        // bad module can't abort the census.
        let module = match catch_unwind(AssertUnwindSafe(|| {
            ItModule::load(&bytes).map(|m| m.to_module())
        })) {
            Ok(Ok(m)) => m,
            Ok(Err(_)) => {
                load_fail += 1;
                failed_paths.push(path.clone());
                continue;
            }
            Err(_) => {
                panics += 1;
                failed_paths.push(path.clone());
                continue;
            }
        };

        let Some(mp) = module.mix_plugins.as_ref() else {
            continue;
        };
        // Collect the distinct IDs this file uses so a plugin placed on
        // several channels still counts as one "file" but N "slots".
        let mut seen_in_file: BTreeMap<(u32, u32), ()> = BTreeMap::new();
        let mut file_has_slot = false;
        for plug in &mp.plugins {
            let id1 = plug.info.id1;
            let id2 = plug.info.id2;
            if id1 == 0 && id2 == 0 && plug.data.is_none() {
                continue; // empty/reserved slot
            }
            file_has_slot = true;
            total_slots += 1;
            let entry = census.entry((id1, id2)).or_default();
            entry.slots += 1;
            if entry.name_hint.is_none() {
                // The plugin's library / display name is now a proper
                // field on `MixPluginInfo` (the importer parses the full
                // 128-byte info), so no blob scraping needed.
                let n = if !plug.info.library_name.is_empty() {
                    &plug.info.library_name
                } else {
                    &plug.info.name
                };
                if !n.is_empty() {
                    entry.name_hint = Some(n.clone());
                }
            }
            seen_in_file.insert((id1, id2), ());
        }
        for key in seen_in_file.keys() {
            census.get_mut(key).unwrap().files += 1;
        }
        if file_has_slot {
            with_table += 1;
        }
    }

    // ---- report ----
    println!();
    println!("=== IT mix-plugin / VST census ===");
    println!("root                 : {}", root.display());
    println!("scanned .it files    : {}", scanned);
    println!("  parse failures     : {}", load_fail);
    println!("  parser panics      : {}", panics);
    println!("files with plugin(s) : {}", with_table);
    println!("total plugin slots   : {}", total_slots);
    println!("distinct (id1,id2)   : {}", census.len());
    println!();

    if census.is_empty() {
        println!("(no mix-plugin tables found)");
        return ExitCode::SUCCESS;
    }

    // Sort by slot count desc, then file count desc, then key.
    let mut rows: Vec<(&(u32, u32), &Agg)> = census.iter().collect();
    rows.sort_by(|a, b| {
        b.1.slots
            .cmp(&a.1.slots)
            .then(b.1.files.cmp(&a.1.files))
            .then(a.0.cmp(b.0))
    });

    println!(
        "{:>5}  {:>5}  {:<6}  {:<6}  {:<12}  {:<12}  name hint",
        "slots", "files", "id1", "id2", "id1(hex)", "id2(hex)"
    );
    println!("{}", "-".repeat(86));
    for (&(id1, id2), agg) in rows {
        println!(
            "{:>5}  {:>5}  {:<6}  {:<6}  0x{:08X}  0x{:08X}  {}",
            agg.slots,
            agg.files,
            fourcc(id1),
            fourcc(id2),
            id1,
            id2,
            agg.name_hint.as_deref().unwrap_or(""),
        );
    }

    // VST-only roll-up: id1 == 'VstP' (OpenMPT packs it big-endian).
    let vst_magic = u32::from_be_bytes(*b"VstP");
    let vst: Vec<_> = census
        .iter()
        .filter(|((id1, _), _)| *id1 == vst_magic)
        .collect();
    let vst_slots: usize = vst.iter().map(|(_, a)| a.slots).sum();
    println!();
    println!(
        "VST slots ('VstP'): {} across {} distinct VST IDs",
        vst_slots,
        vst.len()
    );

    if !failed_paths.is_empty() {
        println!();
        println!("unparseable files ({}):", failed_paths.len());
        for p in &failed_paths {
            println!("  {}", p.display());
        }
    }

    ExitCode::SUCCESS
}

fn default_music_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join("Music")
}

/// Recursively gather every file whose extension is `it`
/// (case-insensitive). Does not follow directory symlinks.
fn collect_it_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            collect_it_files(&path, out);
        } else if ft.is_file()
            && path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("it"))
        {
            out.push(path);
        }
    }
}

/// Render a packed 32-bit plugin code as its four-character code.
/// OpenMPT packs FOURCCs big-endian (`PLUGMAGIC('V','s','t','P')` =
/// `0x56737450`), so the readable name is the big-endian byte order;
/// printable ASCII is shown verbatim, anything else as `.`.
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
