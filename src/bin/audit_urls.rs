//! One-shot audit of crawler_v2::url against real URLs.
//!
//! Reads JSONL manifests (from the Python prototype at
//! /Volumes/T9/gemma-god/corpus_v2/manifests/), canonicalizes every URL,
//! classifies it, scores it, and prints distributions. Not part of the
//! production daemon — temporary diagnostic binary.
//!
//! Usage: `cargo run --bin audit_urls -- <path.jsonl> [more.jsonl ...]`

use gemma_god::crawler_v2::url::{canonicalize, classify, score, ContentClass, Rejection};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};

#[derive(Deserialize)]
struct ManifestRow {
    url: String,
    #[serde(default)]
    depth: u32,
}

fn main() -> anyhow::Result<()> {
    let files: Vec<String> = std::env::args().skip(1).collect();
    if files.is_empty() {
        eprintln!("usage: audit_urls <manifest.jsonl> [...]");
        std::process::exit(2);
    }

    let mut total = 0u64;
    let mut ok = 0u64;
    let mut rejected: BTreeMap<String, u64> = BTreeMap::new();
    let mut classes: BTreeMap<String, u64> = BTreeMap::new();
    let mut score_hist: BTreeMap<i32, u64> = BTreeMap::new();
    let mut changed = 0u64;
    let mut idempotent_fail = 0u64;
    let mut sample_rejects: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut sample_transforms: Vec<(String, String)> = Vec::new();

    for path in &files {
        let f = File::open(path)?;
        for line in BufReader::new(f).lines() {
            let line = line?;
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            let row: ManifestRow = match serde_json::from_str(t) {
                Ok(r) => r,
                Err(_) => continue,
            };
            total += 1;

            // Use the URL itself as the base (absolute URL in manifest).
            match canonicalize(&row.url, &row.url) {
                Ok(canon) => {
                    ok += 1;
                    if canon != row.url {
                        changed += 1;
                        if sample_transforms.len() < 20 {
                            sample_transforms.push((row.url.clone(), canon.clone()));
                        }
                    }
                    // Idempotence: canonicalize(canon, canon) == canon.
                    match canonicalize(&canon, &canon) {
                        Ok(c2) if c2 == canon => {}
                        _ => idempotent_fail += 1,
                    }
                    let c = classify(&canon);
                    *classes.entry(class_name(c).to_string()).or_default() += 1;
                    let s = score(&canon, row.depth);
                    *score_hist.entry(s).or_default() += 1;
                }
                Err(e) => {
                    let k = format!("{e:?}");
                    *rejected.entry(k.clone()).or_default() += 1;
                    let bucket = sample_rejects.entry(k).or_default();
                    if bucket.len() < 3 {
                        bucket.push(row.url.clone());
                    }
                }
            }
        }
    }

    println!("=== real-url audit ===");
    println!("files: {}", files.len());
    println!("total urls: {total}");
    println!("canonicalized OK: {ok}  ({:.1}%)", pct(ok, total));
    println!("  of which, transformed: {changed}");
    println!("  idempotence failures: {idempotent_fail}");
    println!();
    println!("rejections (count, example URLs):");
    for (reason, n) in &rejected {
        println!("  {reason:<24} {n:>5}");
        if let Some(samples) = sample_rejects.get(reason) {
            for s in samples {
                println!("    e.g. {}", truncate(s, 110));
            }
        }
    }
    println!();
    println!("classification distribution (of OK urls):");
    for (cls, n) in &classes {
        println!("  {cls:<14} {n:>5}  ({:.1}%)", pct(*n, ok));
    }
    println!();
    println!("score histogram:");
    for (score, n) in &score_hist {
        println!("  score {score:>4}: {n:>5}  {}", bar(*n, 60));
    }
    println!();
    println!("sample transformations (first 20):");
    for (before, after) in &sample_transforms {
        println!("  - {}", truncate(before, 100));
        println!("    > {}", truncate(after, 100));
    }

    Ok(())
}

fn class_name(c: ContentClass) -> &'static str {
    match c {
        ContentClass::Document => "Document",
        ContentClass::ContentPage => "ContentPage",
        ContentClass::Listing => "Listing",
        ContentClass::Navigation => "Navigation",
        ContentClass::LowValue => "LowValue",
    }
}

fn pct(n: u64, d: u64) -> f64 {
    if d == 0 {
        0.0
    } else {
        100.0 * n as f64 / d as f64
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

fn bar(n: u64, max: u64) -> String {
    let w = (n.min(max)) as usize;
    "█".repeat(w)
}

// Silence unused warning: this is a binary, not a lib user.
#[allow(dead_code)]
fn _force_import() {
    let _ = Rejection::NonHttp;
}
