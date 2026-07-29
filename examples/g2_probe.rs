//! Does a G2 tune outside the two reference ones import, and does its data stay
//! inside the assumptions the reference tunes established?
//!
//! A rule derived from two tunes is a rule derived from two samples. This walks
//! any `.sid` and reports what would otherwise go wrong in silence:
//!
//!  * whether `to_module` succeeds at all, and what shape it produces;
//!  * the vibrato ARMING rule — the replayer arms on the whole of record +5
//!    (`$147b BNE`), the importer on its range bits alone (`(v5 & 0x78) != 0`).
//!    Checked across all six G2 tunes: nothing exercises the difference;
//!  * the note range, and how often note 95 is played — that is the last table
//!    entry, and every G2 tune stores it as `$ffff`;
//!  * NEGATIVE track transposes, which is what this probe was written to find:
//!    the replayer adds the transpose byte modulo 256 and the importer used to
//!    add it as unsigned, sending every note it touched to 95 and pinning the
//!    voice at `$ffff` for the length of a note;
//!  * portamento lanes, including any whose rate rounds away to zero.
//!
//! ```text
//! cargo run --example g2_probe --features "std,import_sid" -- <tune.sid>...
//! ```

use xmrs::tracker::import::sid::g2::{self, G2Config};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: g2_probe <tune.sid>...");
        return;
    }
    for path in &args {
        let name = std::path::Path::new(path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Ok(data) = std::fs::read(path) else {
            println!("{name:24} unreadable");
            continue;
        };
        let Some(c) = G2Config::detect(&data) else {
            println!("{name:24} not a G2 tune");
            continue;
        };
        println!(
            "{name:24} load={:04x} instr={:04x} filters={:04x} patterns={:04x} \
             freqtab={:04x} orders={:04x}",
            c.load, c.instruments, c.filter_slots, c.pattern_ptrs, c.freq_table, c.order_ptrs
        );

        // Record +5: the vibrato parameter the replayer copies to `$1a5b,X`.
        // The tune image starts at 126, not 124: bytes 124-125 hold the load
        // address the header does not carry. Reading from 124 shifts every
        // record two bytes and reports record +3 as if it were +5.
        const PSID_DATA: usize = 126;
        let at = |addr: u16| -> Option<u8> {
            let off = addr.checked_sub(c.load)? as usize;
            data.get(PSID_DATA + off).copied()
        };
        let mut divergent = Vec::new();
        let mut armed = 0;
        for i in 0..16u16 {
            let Some(v5) = at(c.instruments + i * 16 + 5) else {
                continue;
            };
            if v5 != 0 {
                armed += 1;
            }
            if v5 != 0 && (v5 & 0x78) == 0 {
                divergent.push((i, v5));
            }
        }
        if divergent.is_empty() {
            println!("{name:24}   vibrato: {armed}/16 instruments armed, arming rules agree");
        } else {
            println!(
                "{name:24}   ⚠️ vibrato ARMING DIVERGES on {:?} (chip arms, import does not)",
                divergent
            );
        }

        // Note range. The replayer clamps EVERY note to `$5f` (`$127e CMP #$60
        // / BCC / LDA #$5f`), on both the transposed and the wavetable path, and
        // it does so AFTER adding the track transpose. A note that survives past
        // 95 on our side has no register to land on — the conversion saturates
        // at `$ffff`, which is the shape of Lakers' longest divergences.
        let bank = c.instruments(&data);
        let mut over = 0usize;
        let mut top = 0u8;
        let mut per_voice = [0usize; 4];
        for (v, count) in per_voice.iter_mut().enumerate() {
            let ptr = c.order_pointers(&data, 0)[v];
            for r in c.voice_rows(&data, ptr, &bank) {
                if let g2::RowNote::Play(n) = r.note {
                    top = top.max(n);
                    if n > 95 {
                        over += 1;
                        *count += 1;
                    }
                }
            }
        }
        // The instrument bank as the replayer reads it: +2 sustain waveform,
        // +7 flags (bit0 = wavetable, bit2 = attack waveform, bit5 = filter,
        // bit6 = attack note), +9 attack waveform, +11 attack frames. Enough to
        // tell an instrument whose timbre comes from its record apart from one
        // whose timbre comes from a program.
        if std::env::var("G2_INSTRUMENTS").is_ok() {
            println!("{name:24}   instr  ctrl flags atk-wave atk-frames  program?");
            for i in 0..16u16 {
                let b = |o: u16| at(c.instruments + i * 16 + o).unwrap_or(0);
                // …beside what WE decoded, because a waveform that survives the
                // record but not the import is invisible in every register
                // comparison until it reaches the chip.
                let ours = bank
                    .get(i as usize)
                    .map(|ins| {
                        let v = &ins.voice;
                        ((v.ctrl_noise as u8) << 7)
                            | ((v.ctrl_pulse as u8) << 6)
                            | ((v.ctrl_sawtooth as u8) << 5)
                            | ((v.ctrl_triangle as u8) << 4)
                            | ((v.ctrl_test as u8) << 3)
                            | ((v.ctrl_rm as u8) << 2)
                            | ((v.ctrl_sync as u8) << 1)
                            | v.ctrl_gate as u8
                    })
                    .unwrap_or(0);
                println!(
                    "{name:24}     {i:2}   {:02x}   {:02x}     {:02x}       {:3}       {}   ours {ours:02x}{}",
                    b(2),
                    b(7),
                    b(9),
                    b(11),
                    if b(7) & 1 != 0 { "yes" } else { "-" },
                    if ours & !1 != b(2) & !1 { "  ⚠️" } else { "" }
                );
            }
        }

        // TIES THAT CHANGE INSTRUMENT. The replayer's tie branch still stores
        // the (possibly new) instrument's record +2 into the control shadow
        // (`$13b5`-`$13bb`), so a tie can change the TIMBRE without
        // re-triggering — while the ADSR, written only in the part the tie
        // skips, stays. Our legato path re-reads neither, so this count is
        // exactly how many cells a fix would affect, and the rest is a no-op.
        let mut tied_total = 0usize;
        let mut tied_instr_change = 0usize;
        for v in 0..4 {
            let ptr = c.order_pointers(&data, 0)[v];
            let rows = c.voice_rows(&data, ptr, &bank);
            let mut prev_instr: Option<u8> = None;
            for r in &rows {
                if matches!(r.note, g2::RowNote::Play(_)) {
                    if r.tied {
                        tied_total += 1;
                        if prev_instr.is_some_and(|p| p != r.instrument) {
                            tied_instr_change += 1;
                        }
                    }
                    prev_instr = Some(r.instrument);
                }
            }
        }
        println!("{name:24}   ties: {tied_total}, of which {tied_instr_change} change instrument");

        // How often the very top of the range is played. Note 95 is the last
        // table entry, and in every G2 tune that entry is `$ffff` — the table
        // itself saturates there. A tune that plays note 95 often is either
        // doing something deliberate or being mis-decoded.
        let mut at95 = [0usize; 4];
        for (v, count) in at95.iter_mut().enumerate() {
            let ptr = c.order_pointers(&data, 0)[v];
            for r in c.voice_rows(&data, ptr, &bank) {
                if let g2::RowNote::Play(95) = r.note {
                    *count += 1;
                }
            }
        }
        if at95.iter().any(|&n| n > 0) {
            println!("{name:24}   notes at 95 (table entry $ffff): per track {at95:?}");
        }
        // Track transposes, as the order list states them. The replayer adds
        // this byte with `CLC / ADC` (`$1281`) — 8-bit MODULAR arithmetic — so a
        // byte ≥ `$80` is a NEGATIVE transpose. Adding it as an unsigned value
        // instead sends every note it touches off the top of the table.
        for v in 0..4 {
            let ptr = c.order_pointers(&data, 0)[v];
            let ts: Vec<u8> = c
                .order_list(&data, ptr)
                .iter()
                .filter_map(|s| match s {
                    g2::OrderStep::Transpose(t) => Some(*t),
                    _ => None,
                })
                .collect();
            if ts.iter().any(|&t| t >= 0x80) {
                println!(
                    "{name:24}   ⚠️ track {v} NEGATIVE transposes: {:?}",
                    ts.iter()
                        .map(|&t| format!("{t:#04x}({})", t as i8))
                        .collect::<Vec<_>>()
                );
            }
        }
        println!(
            "{name:24}   notes: highest {top}, {over} above 95 {}",
            if over > 0 {
                format!("⚠️ per track {per_voice:?}")
            } else {
                String::new()
            }
        );

        match g2::to_module(&data) {
            Some(m) => {
                println!(
                    "{name:24}   imports: {} tracks, {} clips, {} instruments, {} entries",
                    m.tracks.len(),
                    m.clips.len(),
                    m.instrument.len(),
                    m.timeline_map.entries.len(),
                );
                // Portamento survives the import as `Slide` automation. A marker
                // that decodes but produces no lane — or a lane whose rate
                // rounds to zero through the `-step / 16` conversion — is a
                // slide the chip performs and we do not.
                let (mut lanes, mut sets, mut zero) = (0, 0, 0);
                for l in &m.automation {
                    if let xmrs::core::daw::automation::LaneKind::Slide { events } = &l.kind {
                        lanes += 1;
                        for e in events {
                            if let xmrs::core::daw::automation::SlideEvent::Set { rate, .. } = e {
                                sets += 1;
                                if rate.raw() == 0 {
                                    zero += 1;
                                }
                            }
                        }
                    }
                }
                println!(
                    "{name:24}   portamento: {lanes} slide lanes, {sets} set events{}",
                    if zero > 0 {
                        format!(" ⚠️ {zero} of them rate 0 (rounded away)")
                    } else {
                        String::new()
                    }
                );
            }
            None => println!("{name:24}   ⚠️ to_module returned None"),
        }
    }
}
