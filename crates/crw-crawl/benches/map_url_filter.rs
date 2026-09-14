//! Bench for the /map URL filter.
//!
//! Measures the *delta* between calling `filter_and_normalize_raw` with a
//! defaults-on config vs. a no-op baseline that only does the fragment +
//! trailing-slash + scheme/host fold that the production normalizer does.
//! Gate: delta ≤ 3µs/URL p50 on M-class hardware (informational; not enforced
//! in CI).
//!
//! The baseline must stay a copy of the production normalizer, or the delta
//! silently absorbs normalize's own cost and stops meaning what the gate says.
//!
//! Run with: `cargo bench -p crw-crawl --bench map_url_filter`.

use crw_crawl::url_filter::{UrlFilterCfg, filter_and_normalize_raw};
use std::time::Instant;

fn baseline_normalize(u: &str) -> String {
    let without_fragment = u.split('#').next().unwrap_or(u);
    let trimmed = without_fragment.trim_end_matches('/');
    let Some(sep) = trimmed.find("://") else {
        return trimmed.to_string();
    };
    let authority_start = sep + 3;
    let authority_end = trimmed[authority_start..]
        .find(['/', '?'])
        .map_or(trimmed.len(), |i| authority_start + i);
    let authority = &trimmed[authority_start..authority_end];
    let host_start = authority.rfind('@').map_or(0, |i| i + 1);
    let host_and_port = &authority[host_start..];
    let host_len = if host_and_port.starts_with('[') {
        host_and_port
            .find(']')
            .map_or(host_and_port.len(), |i| i + 1)
    } else {
        host_and_port.rfind(':').unwrap_or(host_and_port.len())
    };
    let host_end = authority_start + host_start + host_len;

    let mut key = trimmed[..sep].to_ascii_lowercase();
    key.push_str("://");
    key.push_str(&trimmed[authority_start..authority_start + host_start]);
    key.push_str(&trimmed[authority_start + host_start..host_end].to_lowercase());
    key.push_str(&trimmed[host_end..authority_end]);
    key.push_str(&trimmed[authority_end..]);
    key
}

fn mixed_corpus(n: usize) -> Vec<String> {
    let mut urls = Vec::with_capacity(n);
    for i in 0..n {
        let bucket = i % 10;
        let url = match bucket {
            0..=5 => format!("https://example{}.com/blog/post-{}", i % 7, i),
            6..=8 => format!(
                "https://example{}.com/page?utm_source=newsletter&utm_medium=email&id={}",
                i % 7,
                i
            ),
            _ => format!("https://shop{}.com/?add-to-cart={}", i % 7, i),
        };
        urls.push(url);
    }
    urls
}

fn no_query_corpus(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| format!("https://example{}.com/blog/post-{}", i % 7, i))
        .collect()
}

fn tracking_corpus(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| {
            format!(
                "https://example{}.com/page?utm_source=newsletter&utm_medium=email&fbclid=abc{}&gclid=xyz{}&id={}",
                i % 7, i, i, i
            )
        })
        .collect()
}

fn action_corpus(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| {
            format!(
                "https://shop{}.com/?add-to-cart={}&_wpnonce=abc{}",
                i % 7,
                i,
                i
            )
        })
        .collect()
}

fn host_override_corpus(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| {
            format!(
                "https://forum{}.example.com/viewtopic.php?t={}&utm_source=email",
                i % 7,
                i
            )
        })
        .collect()
}

struct Stats {
    p50: f64,
    p99: f64,
    mean: f64,
}

fn measure<F: FnMut(&str)>(label: &str, urls: &[String], mut f: F) -> Stats {
    // Warm up.
    for u in urls.iter().take(1000.min(urls.len())) {
        f(u);
    }
    let mut samples: Vec<u128> = Vec::with_capacity(urls.len());
    for u in urls {
        let t = Instant::now();
        f(u);
        samples.push(t.elapsed().as_nanos());
    }
    samples.sort_unstable();
    let p50 = samples[samples.len() / 2] as f64;
    let p99 = samples[(samples.len() * 99) / 100] as f64;
    let mean = samples.iter().copied().sum::<u128>() as f64 / samples.len() as f64;
    println!(
        "  {:30}  p50 = {:>6.0} ns   p99 = {:>7.0} ns   mean = {:>6.0} ns",
        label, p50, p99, mean
    );
    Stats { p50, p99, mean }
}

fn run_group(name: &str, urls: &[String], cfg: &UrlFilterCfg) -> (Stats, Stats) {
    println!("\n[{}]  n = {}", name, urls.len());
    let base = measure("baseline normalize", urls, |u| {
        let _ = std::hint::black_box(baseline_normalize(u));
    });
    let filt = measure("filter_and_normalize_raw", urls, |u| {
        let _ = std::hint::black_box(filter_and_normalize_raw(u, cfg));
    });
    println!(
        "  delta:                          p50 = {:>+6.0} ns   p99 = {:>+7.0} ns   mean = {:>+6.0} ns",
        filt.p50 - base.p50,
        filt.p99 - base.p99,
        filt.mean - base.mean,
    );
    (base, filt)
}

fn main() {
    let cfg = UrlFilterCfg::defaults_on();
    const N: usize = 10_000;

    let mixed = mixed_corpus(N);
    let noq = no_query_corpus(N);
    let track = tracking_corpus(N);
    let action = action_corpus(N);
    let host = host_override_corpus(N);

    let (_, mixed_filt) = run_group("mixed (60% no-?, 30% tracking, 10% action)", &mixed, &cfg);
    run_group("no-query (cheap pre-screen)", &noq, &cfg);
    run_group("tracking-heavy (utm+gclid+fbclid)", &track, &cfg);
    run_group("action (drop path)", &action, &cfg);
    run_group("host-override (forum viewtopic.php)", &host, &cfg);

    println!("\nGate (mixed corpus): delta p50 ≤ 3000 ns");
    if mixed_filt.p50 > 3000.0 {
        eprintln!("WARNING: mixed-corpus filter p50 exceeds 3µs/URL");
    }
}
