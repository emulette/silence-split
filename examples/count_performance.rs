//! Fixed workloads for count-mode performance. Run with:
//! `cargo run --release --locked --example count_performance`.
//! Timings cover planning only. The checksum covers the full cut path for before/after comparison.

use silence_split::{Analysis, Layout, PlanConfig, ScoreFrames, SegmentConfig, plan};
use std::{
    hint::black_box,
    io::{self, Write},
    num::NonZeroUsize,
    time::{Duration, Instant},
};

fn main() -> io::Result<()> {
    let mut out = io::stdout().lock();
    for (seconds, count, maximum) in [
        (600, 400, 600),
        (3600, 128, 3600),
        (3600, 5000, 3600),
        (10800, 0, 30),
    ] {
        let analysis = Analysis::from_scores(
            Layout {
                sample_rate: 1000,
                channels: 1,
            },
            seconds * 1000,
            ScoreFrames {
                sample_rate: 1000,
                hop: 10,
            },
            vec![0.9; seconds as usize * 100],
        )
        .unwrap();
        let mut config = PlanConfig::new(
            Duration::ZERO,
            Duration::from_secs(maximum),
            Duration::from_secs(maximum),
        )
        .unwrap();
        if let Some(n) = NonZeroUsize::new(count) {
            config = config.count(n);
        }
        let start = Instant::now();
        let result = black_box(plan(
            black_box(&analysis),
            &SegmentConfig::default(),
            black_box(&config),
        ));
        let elapsed = start.elapsed();
        let checksum = result.cuts.iter().fold(0u64, |v, c| {
            v.wrapping_mul(31)
                .wrapping_add(c.previous_end)
                .wrapping_mul(31)
                .wrapping_add(c.next_start)
        });
        writeln!(
            out,
            "seconds={seconds} count={count} max={maximum} pieces={} checksum={checksum:016x} time={:.6}s",
            result.pieces.len(),
            elapsed.as_secs_f64()
        )?;
        out.flush()?;
    }
    Ok(())
}
