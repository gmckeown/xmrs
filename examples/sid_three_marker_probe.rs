//! Check the three-marker generation's recovered tables against the real chip.
//!
//! Detection recognises this generation but nothing decodes it yet, so the
//! ordinary oracle gate has nothing to compare. One thing can still be checked
//! without a decoder: whether the **row cadence** read out of the image matches
//! the spacing of note attacks the chip actually produces.
//!
//! A gate-attack is a frame where a voice's control register goes from
//! gate-clear to gate-set. Their spacing is the row length in frames, up to
//! whichever rows carry no new note — so the useful statistic is the *smallest*
//! common spacing, not the mean.
//!
//! ```sh
//! cargo run --release --example sid_three_marker_probe --features import_sid -- \
//!     ~/tunes/Off_the_Cuff.sid capture.csv
//! ```

use xmrs::tracker::import::sid::g2::{G2Config, Generation};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(sid), Some(csv)) = (args.next(), args.next()) else {
        eprintln!("usage: sid_three_marker_probe <tune.sid> <regdump.csv>");
        std::process::exit(2);
    };

    let data = std::fs::read(&sid).unwrap_or_else(|e| panic!("{sid}: {e}"));
    let Some(c) = G2Config::detect(&data) else {
        eprintln!("{sid}: not a marker-dispatcher tune");
        std::process::exit(2);
    };
    println!("{sid}");
    println!("  generation   {:?}", c.generation);
    println!("  load         {:04x}", c.load);
    println!("  instruments  {:04x}", c.instruments);
    println!("  pattern ptrs {:04x}", c.pattern_ptrs);
    println!("  order ptrs   {:04x}", c.order_ptrs);
    println!(
        "  tempo table  {:04x}  -> speed {}",
        c.tempo_table,
        c.speed(&data, 0)
    );

    // The oracle: one row per replay frame, 25 hex register values.
    let text = std::fs::read_to_string(&csv).unwrap_or_else(|e| panic!("{csv}: {e}"));
    let frames: Vec<[u8; 25]> = text
        .lines()
        .skip(1)
        .map(|l| {
            let mut a = [0u8; 25];
            for (i, x) in l.split(',').skip(1).enumerate().take(25) {
                a[i] = u8::from_str_radix(x.trim(), 16).unwrap_or(0);
            }
            a
        })
        .collect();
    println!("  oracle       {} frames", frames.len());

    // Control registers are $D404 / $D40B / $D412 — offsets 4, 11, 18.
    for (v, ctrl) in [4usize, 11, 18].into_iter().enumerate() {
        let mut attacks: Vec<usize> = Vec::new();
        for i in 1..frames.len() {
            if frames[i][ctrl] & 1 == 1 && frames[i - 1][ctrl] & 1 == 0 {
                attacks.push(i);
            }
        }
        if attacks.len() < 3 {
            println!("  voice {v}: {} attacks — too few to time", attacks.len());
            continue;
        }
        let gaps: Vec<usize> = attacks.windows(2).map(|w| w[1] - w[0]).collect();
        // Every gap is a whole number of rows, so the row length divides all of
        // them: their greatest common divisor is the row length, or a multiple
        // of it when the tune never happens to place two notes one row apart.
        // (Taking the shortest gap instead would be wrong — with a 3-frame row,
        // a 12-frame gap is four perfectly ordinary rows.)
        let gcd = gaps.iter().fold(0usize, |a, &b| {
            let (mut a, mut b) = (a, b);
            while b != 0 {
                (a, b) = (b, a % b);
            }
            a
        });
        // The cadence is not an integer, so a gcd of 1 is the expected answer,
        // not a failure: this family's row gate runs two counters, and the
        // twelve-step one steals a frame in every twelve. Rows therefore land on
        // `ceil(r · (R+1)(Ka+1) / Ka)`. Test THAT instead — every attack should
        // sit on such a frame.
        let (num, den) = ((c.speed(&data, 0) as usize) * 12, 11usize);
        let rows: Vec<usize> = (0..2000)
            .map(|r| (r * num).div_ceil(den))
            .take_while(|f| *f <= frames.len())
            .collect();
        let on_row = attacks
            .iter()
            .filter(|a| rows.iter().any(|r| r.abs_diff(**a) <= 1))
            .count();
        println!(
            "  voice {v}: {:>3} attacks, gap gcd {gcd}, shortest {} — {on_row}/{} land on a \
             {num}/{den} row (±1)",
            attacks.len(),
            gaps.iter().min().unwrap(),
            attacks.len()
        );
    }
    // Which cadence actually fits? Sweep the subdivision reload and report how
    // many of the chip's attacks fall on a row for each. The detected value
    // should win outright; if a neighbour wins instead, the tempo table is being
    // read at the wrong index or the tune's default sub-song is not 0.
    let mut all: Vec<usize> = Vec::new();
    for ctrl in [4usize, 11, 18] {
        for i in 1..frames.len() {
            if frames[i][ctrl] & 1 == 1 && frames[i - 1][ctrl] & 1 == 0 {
                all.push(i);
            }
        }
    }
    // Matching must be EXACT. A tolerance of ±1 frame against a row spacing of
    // one or two frames marks essentially every frame as "on a row", so every
    // small speed scores 100 % and the test proves nothing — which is exactly
    // what a first pass at this reported. The capture does carry an unknown
    // constant lead (sidplay dumps an idle frame before the first `play`), so
    // the phase is searched rather than tolerated: best offset wins, ties go to
    // the smaller one.
    println!(
        "\n  cadence sweep over {} attacks (detected speed {}):",
        all.len(),
        c.speed(&data, 0)
    );
    println!("    (exact frame match, best phase; speeds under 3 are near-degenerate)");
    for r in 1..=8usize {
        let (num, den) = (r * 12, 11usize);
        let best = (0..=num / den + 1)
            .map(|phase| {
                let rows: std::collections::BTreeSet<usize> = (0..3000)
                    .map(|k| (k * num).div_ceil(den) + phase)
                    .take_while(|f| *f <= frames.len())
                    .collect();
                all.iter().filter(|a| rows.contains(a)).count()
            })
            .max()
            .unwrap_or(0);
        // A cadence of N frames/row marks 1/N of all frames as rows, so an
        // attack lands on one by chance that often. Without this baseline a
        // score is unreadable — a 100 % at 1.09 frames/row says nothing at all.
        let chance = den as f64 / num as f64;
        let obs = best as f64 / all.len() as f64;
        println!(
            "    speed {r} ({num}/{den} = {:.2} frames/row): {best}/{} = {:>5.1} % \
             (chance {:>5.1} %, ×{:.1}){}",
            num as f64 / den as f64,
            all.len(),
            obs * 100.0,
            chance * 100.0,
            obs / chance,
            if r == c.speed(&data, 0) as usize {
                "  <- detected"
            } else {
                ""
            }
        );
    }
    let _ = Generation::Last;
}
