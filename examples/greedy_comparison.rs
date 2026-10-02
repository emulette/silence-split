//! Compares the global plan with greedy splitters on synthetic talk.
//!
//! The greedy splitters work like Silero VAD at its maximum length: once a piece would exceed the
//! maximum, they cut in the middle of the longest silence within reach (the current default) or
//! of the last one (the legacy mode), or at the maximum when there is none. All keep all audio,
//! so the pieces differ only in where the cuts go.
//!
//! ```sh
//! cargo run --release --example greedy_comparison
//! ```

#[path = "../tests/common/mod.rs"]
mod common;

use std::io::{self, Write};
use std::time::Duration;

use common::{Greedy, greedy_cuts, raw_silences};
use silence_split::{Analysis, PlanConfig, SegmentConfig, Span, plan};

const RATE: u32 = 16_000;
const FILES: u64 = 20;
const FILE_SECS: f32 = 600.0;

#[derive(Default)]
struct Tally {
    pieces: usize,
    short: usize,
    cuts: usize,
    outside: usize,
    lengths: Vec<f64>,
}

impl Tally {
    fn add(&mut self, len: u64, cuts: &[u64], silences: &[Span], min: u64) {
        let mut at = 0;
        for &end in cuts.iter().chain([len].iter()) {
            self.pieces += 1;
            self.short += usize::from(end - at < min);
            self.lengths.push((end - at) as f64 / f64::from(RATE));
            at = end;
        }
        self.cuts += cuts.len();
        self.outside += cuts
            .iter()
            .filter(|c| !silences.iter().any(|s| s.start <= **c && **c <= s.end))
            .count();
    }

    fn row(&self, name: &str, out: &mut impl Write) -> io::Result<()> {
        let n = self.lengths.len() as f64;
        let mean = self.lengths.iter().sum::<f64>() / n;
        let var = self
            .lengths
            .iter()
            .map(|l| (l - mean) * (l - mean))
            .sum::<f64>()
            / n;
        writeln!(
            out,
            "| {name} | {} | {} | {:.1} % | {mean:.1} s | {:.1} s |",
            self.pieces,
            self.short,
            100.0 * self.outside as f64 / self.cuts.max(1) as f64,
            var.sqrt(),
        )
    }
}

fn compare(
    analyses: &[Analysis],
    min: u64,
    target: u64,
    max: u64,
    out: &mut impl Write,
) -> io::Result<()> {
    let config = PlanConfig::new(
        Duration::from_secs(min),
        Duration::from_secs(target),
        Duration::from_secs(max),
    )
    .expect("valid lengths");
    let rate = u64::from(RATE);
    let (min_samples, max_samples) = (min * rate, max * rate);
    let mut global = Tally::default();
    let mut longest = Tally::default();
    let mut last = Tally::default();
    for analysis in analyses {
        let silences = raw_silences(analysis);
        let len = analysis.sample_count();
        let planned = plan(analysis, &SegmentConfig::default(), &config);
        let cuts: Vec<u64> = planned.cuts.iter().map(|c| c.previous_end).collect();
        global.add(len, &cuts, &silences, min_samples);
        for (tally, rule) in [(&mut longest, Greedy::Longest), (&mut last, Greedy::Last)] {
            let cuts = greedy_cuts(len, &silences, max_samples, rule);
            tally.add(len, &cuts, &silences, min_samples);
        }
    }
    writeln!(out, "\nmin {min} s, target {target} s, max {max} s\n")?;
    writeln!(
        out,
        "| Splitter | Pieces | Shorter than min | Cuts outside silence | Mean length | Std. dev. |"
    )?;
    writeln!(out, "|---|---:|---:|---:|---:|---:|")?;
    global.row("silence-split `plan`", out)?;
    longest.row("Greedy, longest silence", out)?;
    last.row("Greedy, last silence", out)
}

fn main() -> io::Result<()> {
    let analyses: Vec<Analysis> = (1..=FILES)
        .map(|seed| common::random_talk(RATE, seed, FILE_SECS).analyze())
        .collect();
    let mut out = io::stdout().lock();
    writeln!(out, "{FILES} synthetic talks of {FILE_SECS} s at {RATE} Hz")?;
    compare(&analyses, 5, 30, 30, &mut out)?;
    compare(&analyses, 10, 20, 30, &mut out)?;
    Ok(())
}
