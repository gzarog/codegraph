//! Cross-call source dedup for `explore` within one agent session.
//!
//! A session remembers which line ranges of which files it already sent,
//! with a fingerprint of the file's bytes at the time. A later call replaces
//! a run it would re-send with a pointer naming the file, the symbols and the
//! lines, but only when:
//! - the file is byte-identical to what was served (an edit in between means
//!   the agent's copy is wrong, so everything is re-sent), and
//! - the covered run is at least `MIN_COVERED_LINES` long (shorter pointers
//!   cost more than the source and leave the output full of holes).
//!
//! Re-sending something the agent has costs a few hundred characters;
//! withholding something it never saw costs a file read, so every rule here
//! leans toward re-sending.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// Inclusive line ranges.
pub type Ranges = Vec<(u32, u32)>;

/// The shortest already-served run worth replacing with a pointer.
pub const MIN_COVERED_LINES: u32 = 8;

#[derive(Debug, Clone)]
struct Served {
    fingerprint: String,
    /// Sorted, merged, inclusive line ranges.
    ranges: Vec<(u32, u32)>,
}

/// What one session has already been shown.
#[derive(Debug, Default, Clone)]
pub struct ExploreMemory {
    files: HashMap<String, Served>,
}

/// Identity of a file's current bytes (length-prefixed so a hash collision
/// can't alias files of different sizes).
pub fn fingerprint(lines: &[String]) -> String {
    let mut h = DefaultHasher::new();
    let mut len = 0usize;
    for l in lines {
        l.hash(&mut h);
        len += l.len() + 1;
    }
    format!("{len}:{:016x}", h.finish())
}

fn merge(mut ranges: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    ranges.sort_unstable();
    let mut out: Vec<(u32, u32)> = Vec::new();
    for (s, e) in ranges {
        match out.last_mut() {
            Some(last) if s <= last.1 + 1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

impl ExploreMemory {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Split `ranges` of `path` into what to emit and the covered runs to
    /// replace with a pointer.
    pub fn split(&self, path: &str, fingerprint: &str, ranges: &[(u32, u32)]) -> (Ranges, Ranges) {
        let Some(served) = self
            .files
            .get(path)
            .filter(|s| s.fingerprint == fingerprint)
        else {
            return (ranges.to_vec(), Vec::new());
        };
        let mut emit = Vec::new();
        let mut covered = Vec::new();
        for &(s, e) in ranges {
            let mut cursor = s;
            for &(a, b) in &served.ranges {
                let (lo, hi) = (a.max(cursor), b.min(e));
                if lo > hi || hi - lo + 1 < MIN_COVERED_LINES {
                    continue;
                }
                if lo > cursor {
                    emit.push((cursor, lo - 1));
                }
                covered.push((lo, hi));
                cursor = hi + 1;
            }
            if cursor <= e {
                emit.push((cursor, e));
            }
        }
        (emit, covered)
    }

    /// Remember that `ranges` of `path` (with these bytes) were sent.
    pub fn record(&mut self, path: &str, fingerprint: &str, ranges: &[(u32, u32)]) {
        if ranges.is_empty() {
            return;
        }
        let entry = self
            .files
            .entry(path.to_string())
            .or_insert_with(|| Served {
                fingerprint: fingerprint.to_string(),
                ranges: Vec::new(),
            });
        if entry.fingerprint != fingerprint {
            // The file changed: what was sent before is no longer what the agent holds.
            entry.fingerprint = fingerprint.to_string();
            entry.ranges.clear();
        }
        let mut all = std::mem::take(&mut entry.ranges);
        all.extend_from_slice(ranges);
        entry.ranges = merge(all);
    }
}

/// `10–40, 60–90` for a pointer line.
pub fn spans(ranges: &[(u32, u32)]) -> String {
    let mut parts: Vec<String> = ranges
        .iter()
        .take(4)
        .map(|(s, e)| {
            if s == e {
                s.to_string()
            } else {
                format!("{s}–{e}")
            }
        })
        .collect();
    if ranges.len() > 4 {
        parts.push(format!("+{} more", ranges.len() - 4));
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn withholds_only_long_unchanged_runs() {
        let mut m = ExploreMemory::default();
        m.record("a.rs", "fp1", &[(10, 40)]);
        // Fully covered.
        assert_eq!(
            m.split("a.rs", "fp1", &[(10, 40)]),
            (vec![], vec![(10, 40)])
        );
        // Partly covered: the new tail is emitted.
        assert_eq!(
            m.split("a.rs", "fp1", &[(30, 60)]),
            (vec![(41, 60)], vec![(30, 40)])
        );
        // A short overlap is cheaper to re-send.
        assert_eq!(
            m.split("a.rs", "fp1", &[(38, 60)]),
            (vec![(38, 60)], vec![])
        );
        // Changed bytes: everything is re-sent.
        assert_eq!(
            m.split("a.rs", "fp2", &[(10, 40)]),
            (vec![(10, 40)], vec![])
        );
        m.record("a.rs", "fp2", &[(1, 5)]);
        assert_eq!(
            m.split("a.rs", "fp2", &[(10, 40)]),
            (vec![(10, 40)], vec![])
        );
        assert_eq!(spans(&[(1, 1), (3, 9)]), "1, 3–9");
    }
}
