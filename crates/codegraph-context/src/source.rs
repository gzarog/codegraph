//! Reading indexed source back from disk.

use sha2::{Digest, Sha256};
use std::path::Path;

/// A file's current content, and whether it still matches the index.
pub struct SourceFile {
    pub lines: Vec<String>,
    /// The file changed on disk since it was indexed.
    pub stale: bool,
}

fn hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Read `rel` under `root`; `indexed_hash` flags drift from the index.
pub fn read_source(root: &Path, rel: &str, indexed_hash: Option<&str>) -> Option<SourceFile> {
    let bytes = std::fs::read(root.join(rel)).ok()?;
    let stale = indexed_hash.is_some_and(|h| !h.starts_with("codegraph:") && h != hash(&bytes));
    let text = String::from_utf8_lossy(&bytes);
    Some(SourceFile {
        lines: text.lines().map(str::to_string).collect(),
        stale,
    })
}

/// `<n>\t<line>` for lines `start..=end` (1-based, clamped), the shape the
/// Read tool returns.
pub fn numbered(lines: &[String], start: u32, end: u32) -> String {
    let start = start.max(1) as usize;
    let end = (end as usize).min(lines.len());
    let mut out = String::new();
    for n in start..=end {
        out.push_str(&n.to_string());
        out.push('\t');
        out.push_str(&lines[n - 1]);
        out.push('\n');
    }
    out
}

/// Merge `[start, end]` ranges that overlap or sit within `gap` lines.
pub fn merge_ranges(mut ranges: Vec<(u32, u32)>, gap: u32) -> Vec<(u32, u32)> {
    ranges.sort();
    let mut out: Vec<(u32, u32)> = Vec::new();
    for (s, e) in ranges {
        match out.last_mut() {
            Some(last) if s <= last.1 + gap + 1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_and_numbers() {
        assert_eq!(
            merge_ranges(vec![(10, 12), (1, 3), (5, 6)], 1),
            vec![(1, 6), (10, 12)]
        );
        let lines = vec!["a".to_string(), "b".to_string()];
        assert_eq!(numbered(&lines, 1, 9), "1\ta\n2\tb\n");
    }
}
