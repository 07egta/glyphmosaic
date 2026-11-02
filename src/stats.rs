use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Stats {
    pub total_picks: u64,
    pub distinct_chars: usize,
    pub singleton_chars: usize,
    pub top: Vec<(String, u64)>,
}

pub fn compute(codepoints: impl IntoIterator<Item = char>) -> Stats {
    let mut counts: HashMap<char, u64> = HashMap::new();
    let mut total_picks = 0u64;
    for c in codepoints {
        *counts.entry(c).or_insert(0) += 1;
        total_picks += 1;
    }
    let distinct_chars = counts.len();
    let singleton_chars = counts.values().filter(|&&n| n == 1).count();
    let mut top: Vec<(char, u64)> = counts.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    top.truncate(10);
    Stats {
        total_picks,
        distinct_chars,
        singleton_chars,
        top: top
            .into_iter()
            .map(|(c, n)| (c.to_string(), n))
            .collect(),
    }
}

pub fn write_json(path: &Path, stats: &Stats) -> Result<()> {
    let mut file = File::create(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    serde_json::to_writer_pretty(&mut file, stats)?;
    writeln!(file)?;
    Ok(())
}

fn commas(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn format_report(stats: &Stats) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Total picks: {}", commas(stats.total_picks));
    let _ = writeln!(out, "Distinct characters: {}", commas(stats.distinct_chars as u64));
    let _ = writeln!(
        out,
        "Characters used once: {}",
        commas(stats.singleton_chars as u64)
    );
    let _ = writeln!(out, "Top 10:");
    for (c, n) in &stats.top {
        let cp = c.chars().next().map(|ch| ch as u32).unwrap_or(0);
        let pct = if stats.total_picks == 0 {
            0.0
        } else {
            *n as f64 * 100.0 / stats.total_picks as f64
        };
        let _ = writeln!(out, "  {c} U+{cp:04X}: {} ({pct:.1}%)", commas(*n));
    }
    out
}

pub fn parse_codepoint(s: &str) -> Result<char> {
    let s = s.trim();
    let hex = s
        .strip_prefix("U+")
        .or_else(|| s.strip_prefix("u+"))
        .or_else(|| s.strip_prefix("0x"))
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    let cp = u32::from_str_radix(hex, 16)
        .with_context(|| format!("invalid codepoint: {s}"))?;
    char::from_u32(cp).ok_or_else(|| anyhow::anyhow!("invalid Unicode scalar: {s}"))
}

pub fn read_tsv(path: &Path) -> Result<Vec<char>> {
    let file = File::open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for (line_no, line) in reader.lines().enumerate() {
        let line = line?;
        if line_no == 0 {
            continue;
        }
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let field = line
            .split('\t')
            .nth(2)
            .ok_or_else(|| anyhow::anyhow!("line {} has no codepoint field", line_no + 1))?;
        out.push(parse_codepoint(field)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_reference_style_stats() {
        let s = compute("aabbc".chars());
        assert_eq!(s.total_picks, 5);
        assert_eq!(s.distinct_chars, 3);
        assert_eq!(s.singleton_chars, 1);
        assert_eq!(s.top[0], ("a".to_string(), 2));
        assert_eq!(s.top[1], ("b".to_string(), 2));
    }

    #[test]
    fn parses_and_reads_tsv() {
        assert_eq!(parse_codepoint("U+2044").unwrap(), '\u{2044}');
        assert_eq!(parse_codepoint("0x41").unwrap(), 'A');
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cps.tsv");
        std::fs::write(
            &path,
            "frame\tcell\tcodepoint\tchar\tfont\tmse\n0\t0\tU+2044\t/\t0\t12\n",
        )
        .unwrap();
        assert_eq!(read_tsv(&path).unwrap(), vec!['\u{2044}']);
    }
}
