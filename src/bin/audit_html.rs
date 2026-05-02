//! Diagnostic: run parse_html + shell_detect on local HTML files and show
//! the output. Not part of the production daemon.
//!
//! Usage: `cargo run --bin audit_html -- <file.html> [more.html ...]`

use gemma_god::crawler_v2::parse::parse_html;
use gemma_god::crawler_v2::shell_detect;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let files: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    if files.is_empty() {
        eprintln!("usage: audit_html <file.html> [...]");
        std::process::exit(2);
    }

    println!(
        "{:<40} {:>8} {:>10} {:>7} {:>7}  {:<6}  title",
        "file", "bytes", "text_chars", "links", "scripts", "shell?"
    );
    println!("{}", "-".repeat(110));
    for path in &files {
        let body = std::fs::read(path)?;
        // base url is faked from filename; links won't be same-site but
        // that doesn't affect extraction stats.
        let base = format!("https://{}/", path.file_stem().unwrap().to_string_lossy());
        let parsed = parse_html(&base, &body);
        let v = shell_detect::evaluate(&parsed);
        let shell = if v.is_shell { "SHELL" } else { "ok" };
        let name = path.file_name().unwrap().to_string_lossy();
        let title = parsed.title.as_deref().unwrap_or("—");
        println!(
            "{:<40} {:>8} {:>10} {:>7} {:>7}  {:<6}  {}",
            truncate(&name, 40),
            body.len(),
            v.text_chars,
            parsed.raw_link_count,
            v.script_count,
            shell,
            truncate(title, 40)
        );
    }
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
