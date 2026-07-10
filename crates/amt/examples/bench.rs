//! Perf-budget benchmark (AMT-7 / AMT-18).
//!
//! Seeds a throwaway 10k-issue workspace and measures the median latency of the
//! hot read paths, failing (exit 1) if any exceeds its budget. Run in CI as a
//! regression gate and locally with `cargo run --release --example bench`.
//!
//! Budgets (from the roadmap): list / search / claim < 15 ms, context < 50 ms.
//! Measured against the in-process `store` API (no CLI/process startup), median
//! of many iterations to smooth scheduler noise. Sizes/iteration counts can be
//! overridden with `BENCH_N` / `BENCH_ITERS` for quick local runs.

use amt::{db, store};
use std::time::Instant;

fn env_usize(key: &str, default: usize) -> usize {
    match std::env::var(key) {
        Ok(v) => v.parse().unwrap_or(default),
        Err(_) => default,
    }
}

/// Median wall-clock of `f` over `iters` runs, in milliseconds. One warm-up run
/// first so cold caches don't skew the first sample.
fn median_ms(iters: usize, mut f: impl FnMut()) -> f64 {
    f();
    let mut samples: Vec<f64> = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        f();
        samples.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    samples[samples.len() / 2]
}

fn main() {
    let n = env_usize("BENCH_N", 10_000);
    let iters = env_usize("BENCH_ITERS", 50);

    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = db::init(dir.path(), "bench", "BCH").expect("init");
    let mut conn = db::open(&path).expect("open");
    let t = Instant::now();
    store::seed(&mut conn, n, "bench").expect("seed");
    let seed_ms = t.elapsed().as_secs_f64() * 1000.0;

    let list = median_ms(iters, || {
        let f = store::IssueFilter {
            limit: 50,
            ..Default::default()
        };
        store::list_issues(&conn, &f).expect("list");
    });
    let search = median_ms(iters, || {
        let f = store::SearchFilter::default();
        store::search(&conn, "rotation token", &f).expect("search");
    });
    // peek_next is the claim hot path (candidate selection) and is read-only, so
    // it is safe to repeat without draining the backlog.
    let claim = median_ms(iters, || {
        let f = store::ClaimFilter::any();
        store::peek_next(&conn, "bench", 0, &f).expect("peek");
    });
    // BCH-6 exists and carries a backlink (every fifth seeded issue links a prior
    // one), so this measures a realistic bundle.
    let context = median_ms(iters, || {
        store::context_pack(&conn, "BCH-6", None).expect("context");
    });

    let budgets = [
        ("list", 15.0, list),
        ("search", 15.0, search),
        ("claim(peek)", 15.0, claim),
        ("context", 50.0, context),
    ];

    println!("perf budget - {n} issues, median of {iters} (seed {seed_ms:.0}ms)");
    let mut over = false;
    for (label, budget, got) in budgets {
        let ok = got <= budget;
        over |= !ok;
        let verdict = if ok { "OK" } else { "OVER" };
        println!("  {label:<12} {got:>8.3} ms   budget {budget:>5.0} ms   {verdict}");
    }
    if over {
        eprintln!("error: perf budget exceeded");
        std::process::exit(1);
    }
    println!("all within budget");
}
