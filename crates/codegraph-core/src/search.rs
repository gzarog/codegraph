//! Query parsing, term extraction and ranking helpers shared by the store,
//! the context builder and the MCP tools.

use crate::{Language, NodeKind};
use regex::Regex;
use std::collections::{BTreeSet, HashSet};
use std::sync::LazyLock;

/// A search query split into free text and `field:value` filters
/// (`kind:function lang:go path:src/api name:handler`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedQuery {
    pub text: String,
    pub kinds: Vec<NodeKind>,
    pub languages: Vec<Language>,
    pub path_filters: Vec<String>,
    pub name_filters: Vec<String>,
}

fn unquote(s: &str) -> &str {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Tokenise on whitespace (keeping quoted spans whole) and pull out field filters.
pub fn parse_query(raw: &str) -> ParsedQuery {
    let mut out = ParsedQuery::default();
    let chars: Vec<char> = raw.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let start = i;
        while i < chars.len() && !chars[i].is_whitespace() {
            if chars[i] == '"' {
                match chars[i + 1..].iter().position(|&c| c == '"') {
                    Some(p) => i += p + 2,
                    None => i = chars.len(),
                }
                continue;
            }
            i += 1;
        }
        tokens.push(chars[start..i].iter().collect::<String>());
    }

    let mut text = Vec::new();
    for tok in tokens {
        let Some(colon) = tok.find(':') else {
            text.push(tok);
            continue;
        };
        if colon == 0 || colon == tok.len() - 1 {
            text.push(tok);
            continue;
        }
        let key = tok[..colon].to_ascii_lowercase();
        let value = unquote(&tok[colon + 1..]).to_string();
        if value.is_empty() {
            text.push(tok);
            continue;
        }
        match key.as_str() {
            "kind" => match value.parse() {
                Ok(k) => out.kinds.push(k),
                Err(_) => text.push(tok),
            },
            "lang" | "language" => match value.to_ascii_lowercase().parse() {
                Ok(l) => out.languages.push(l),
                Err(_) => text.push(tok),
            },
            "path" => out.path_filters.push(value),
            "name" => out.name_filters.push(value),
            _ => text.push(tok),
        }
    }
    out.text = text.join(" ").trim().to_string();
    out
}

/// Levenshtein distance, giving up (returning `max + 1`) once it must exceed `max`.
pub fn bounded_edit_distance(a: &str, b: &str, max: usize) -> usize {
    if a == b {
        return 0;
    }
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > max {
        return max + 1;
    }
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        let mut row_min = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (cur[j - 1] + 1).min(prev[j] + 1).min(prev[j - 1] + cost);
            row_min = row_min.min(cur[j]);
        }
        if row_min > max {
            return max + 1;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Lowercase and drop everything but `[a-z0-9]`.
pub fn normalize_name_token(raw: &str) -> String {
    raw.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .collect()
}

/// Tokens naming the project itself (go module, package name, directory).
/// A query word equal to one says nothing about which file is relevant.
pub fn derive_project_name_tokens(project_root: &std::path::Path) -> HashSet<String> {
    let mut tokens = HashSet::new();
    let mut add = |raw: &str| {
        let norm = normalize_name_token(raw);
        if norm.len() >= 5 {
            tokens.insert(norm);
        }
    };
    if let Ok(gomod) = std::fs::read_to_string(project_root.join("go.mod")) {
        static MODULE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"(?m)^\s*module\s+(\S+)").unwrap());
        if let Some(m) = MODULE.captures(&gomod) {
            add(m[1].rsplit('/').next().unwrap_or(""));
        }
    }
    if let Ok(pkg) = std::fs::read_to_string(project_root.join("package.json")) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&pkg) {
            if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
                let bare = match name.strip_prefix('@') {
                    Some(rest) => rest.split_once('/').map(|(_, n)| n).unwrap_or(rest),
                    None => name,
                };
                add(bare);
            }
        }
    }
    if let Some(base) = project_root
        .canonicalize()
        .ok()
        .as_deref()
        .and_then(|p| p.file_name())
    {
        add(&base.to_string_lossy());
    }
    tokens
}

pub const STOP_WORDS: &[&str] = &[
    "the",
    "a",
    "an",
    "and",
    "or",
    "but",
    "in",
    "on",
    "at",
    "to",
    "for",
    "of",
    "with",
    "by",
    "from",
    "is",
    "it",
    "that",
    "this",
    "are",
    "was",
    "be",
    "has",
    "had",
    "have",
    "do",
    "does",
    "did",
    "will",
    "would",
    "could",
    "should",
    "may",
    "might",
    "can",
    "shall",
    "not",
    "no",
    "all",
    "each",
    "every",
    "how",
    "what",
    "where",
    "when",
    "who",
    "which",
    "why",
    "i",
    "me",
    "my",
    "we",
    "our",
    "you",
    "your",
    "he",
    "she",
    "they",
    "show",
    "give",
    "tell",
    "been",
    "done",
    "made",
    "used",
    "using",
    "work",
    "works",
    "found",
    "also",
    "into",
    "then",
    "than",
    "just",
    "more",
    "some",
    "such",
    "over",
    "only",
    "out",
    "its",
    "so",
    "up",
    "as",
    "if",
    "look",
    "need",
    "needs",
    "want",
    "happen",
    "happens",
    "affect",
    "affected",
    "break",
    "breaks",
    "failing",
    "implemented",
    "implement",
    "code",
    "file",
    "files",
    "function",
    "method",
    "class",
    "type",
    "fix",
    "bug",
    "called",
];

pub fn is_stop_word(w: &str) -> bool {
    STOP_WORDS.contains(&w)
}

/// Crude English stemming variants of a term (`parsing` → `pars`, `parse`).
pub fn stem_variants(term: &str) -> Vec<String> {
    let t = term.to_lowercase();
    let n = t.len();
    let mut v = BTreeSet::new();
    let doubled = |base: &str| {
        let b = base.as_bytes();
        b.len() >= 2 && b[b.len() - 1] == b[b.len() - 2]
    };
    if t.ends_with("ing") && n > 5 {
        let base = &t[..n - 3];
        v.insert(base.to_string());
        v.insert(format!("{base}e"));
        if doubled(base) {
            v.insert(base[..base.len() - 1].to_string());
        }
    }
    if (t.ends_with("tion") || t.ends_with("sion")) && n > 5 {
        v.insert(t[..n - 3].to_string());
    }
    if t.ends_with("ment") && n > 6 {
        v.insert(t[..n - 4].to_string());
    }
    if t.ends_with("ies") && n > 4 {
        v.insert(format!("{}y", &t[..n - 3]));
    } else if t.ends_with("es") && n > 4 {
        v.insert(t[..n - 2].to_string());
    } else if t.ends_with('s') && !t.ends_with("ss") && n > 4 {
        v.insert(t[..n - 1].to_string());
    }
    if t.ends_with("ed") && !t.ends_with("eed") && n > 4 {
        v.insert(t[..n - 1].to_string());
        v.insert(t[..n - 2].to_string());
        if t.ends_with("ied") && n > 5 {
            v.insert(format!("{}y", &t[..n - 3]));
        }
    }
    if t.ends_with("er") && n > 4 {
        let base = &t[..n - 2];
        v.insert(base.to_string());
        v.insert(format!("{base}e"));
        if doubled(base) {
            v.insert(base[..base.len() - 1].to_string());
        }
    }
    v.into_iter().filter(|s| s.len() >= 3 && *s != t).collect()
}

static COMPOUND: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b([a-zA-Z][a-zA-Z0-9]*(?:[A-Z][a-z]+)+|[A-Z][a-z]+(?:[A-Z][a-z]*)+)\b").unwrap()
});
static SNAKE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b([a-zA-Z][a-zA-Z0-9]*(?:_[a-zA-Z0-9]+)+)\b").unwrap());
static LOWER_UPPER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"([a-z])([A-Z])").unwrap());
static ACRONYM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"([A-Z]+)([A-Z][a-z])").unwrap());
static NON_ALNUM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^a-zA-Z0-9]+").unwrap());

/// Search terms from a natural-language or identifier query: whole compounds,
/// their camel/snake parts, and (optionally) stem variants.
pub fn extract_search_terms(query: &str, stems: bool) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |t: String, tokens: &mut Vec<String>| {
        if seen.insert(t.clone()) {
            tokens.push(t);
        }
    };
    for m in COMPOUND.captures_iter(query) {
        if m[1].len() >= 3 {
            push(m[1].to_lowercase(), &mut tokens);
        }
    }
    for m in SNAKE.captures_iter(query) {
        if m[1].len() >= 3 {
            push(m[1].to_lowercase(), &mut tokens);
        }
    }
    let split = LOWER_UPPER.replace_all(query, "$1 $2");
    let split = ACRONYM.replace_all(&split, "$1 $2");
    let normalised = split.replace(['_', '.'], " ");
    for word in NON_ALNUM.split(&normalised).filter(|w| !w.is_empty()) {
        let lower = word.to_lowercase();
        if lower.len() < 3 || is_stop_word(&lower) {
            continue;
        }
        push(lower, &mut tokens);
    }
    if stems {
        let base = tokens.clone();
        for token in base {
            for variant in stem_variants(&token) {
                if !is_stop_word(&variant) {
                    push(variant, &mut tokens);
                }
            }
        }
    }
    tokens
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn dirname(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => ".",
    }
}

/// How well a file path matches the query's words: file name hits weigh more
/// than directory hits; test files and deprioritized paths are pushed down
/// unless the query asks about tests.
pub fn score_path_relevance(
    file_path: &str,
    query: &str,
    project_name_tokens: &HashSet<String>,
    deprioritized: bool,
) -> f64 {
    let all: Vec<&str> = query.split_whitespace().collect();
    if all.is_empty() {
        return 0.0;
    }
    let kept: Vec<&str> = all
        .iter()
        .copied()
        .filter(|w| !project_name_tokens.contains(&normalize_name_token(w)))
        .collect();
    let scored = if kept.is_empty() { all } else { kept };
    let mut groups: Vec<(Vec<String>, u32)> = Vec::new();
    for word in scored {
        let sub = extract_search_terms(word, false);
        if sub.is_empty() {
            continue;
        }
        match groups.iter_mut().find(|(s, _)| *s == sub) {
            Some(g) => g.1 += 1,
            None => groups.push((sub, 1)),
        }
    }
    let path_lower = file_path.to_lowercase();
    let file_name = basename(&path_lower);
    let dir_name = dirname(&path_lower);
    let mut score = 0.0;
    for (sub, count) in &groups {
        let c = f64::from(*count);
        if sub.iter().any(|t| file_name.contains(t.as_str())) {
            score += 10.0 * c;
        }
        if sub.iter().any(|t| dir_name.contains(t.as_str())) {
            score += 5.0 * c;
        } else if sub.iter().any(|t| path_lower.contains(t.as_str())) {
            score += 3.0 * c;
        }
    }
    let q = query.to_lowercase();
    let test_query = q.contains("test") || q.contains("spec");
    if (!test_query && is_test_file(file_path)) || deprioritized {
        score -= 15.0;
    }
    score
}

/// Test files plus sample/fixture/benchmark trees.
pub fn is_test_file(file_path: &str) -> bool {
    is_test_path(file_path) || matches_non_production_dir(&file_path.to_lowercase())
}

static TEST_SUFFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[._-](test|tests|unittest|unittests|spec|specs)\.[a-z0-9]+$").unwrap()
});
static TEST_CLASS_SUFFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:Test|Tests|TestCase|Tester|Spec|Specs)\.[A-Za-z0-9]+$").unwrap()
});
static TEST_DIR_CLASS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|/)[A-Za-z0-9]*(?:Test|Tests|Spec)/").unwrap());
static TEST_DIR_MISC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|/)(?:[\w.]+[-_]test(?:s|ing)?|testdata|testutils?|test[-_]utils?|fakes?|mocks?|__mocks__|stubs)/")
        .unwrap()
});
static TEST_DIR_UNIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|/)(?:unittests|[\w.]+[-_]unittests?)/").unwrap());

/// Whether a path is a test file by name or by directory convention.
pub fn is_test_path(file_path: &str) -> bool {
    let lower = file_path.to_lowercase();
    let file_name = basename(file_path);
    let lower_name = file_name.to_lowercase();
    if lower_name.starts_with("test_")
        || lower_name.starts_with("test.")
        || TEST_SUFFIX.is_match(&lower_name)
        || TEST_CLASS_SUFFIX.is_match(file_name)
    {
        return true;
    }
    const DIRS: &[&str] = &[
        "/tests/",
        "/test/",
        "/__tests__/",
        "/spec/",
        "/specs/",
        "/testlib/",
        "/testing/",
        "/e2e/",
    ];
    const PREFIXES: &[&str] = &["test/", "tests/", "spec/", "specs/", "e2e/"];
    DIRS.iter().any(|d| lower.contains(d))
        || PREFIXES.iter().any(|p| lower.starts_with(p))
        || TEST_DIR_CLASS.is_match(file_path)
        || TEST_DIR_MISC.is_match(&lower)
        || TEST_DIR_UNIT.is_match(&lower)
}

fn matches_non_production_dir(lower: &str) -> bool {
    const DIRS: &[&str] = &[
        "integration",
        "sample",
        "samples",
        "example",
        "examples",
        "fixture",
        "fixtures",
        "benchmark",
        "benchmarks",
        "demo",
        "demos",
    ];
    let scope = match lower.find("/src/") {
        Some(i) => &lower[..i + 1],
        None if lower.starts_with("src/") => "",
        None => lower,
    };
    DIRS.iter()
        .any(|d| scope.contains(&format!("/{d}/")) || scope.starts_with(&format!("{d}/")))
}

static NAME_TERM_SPLIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\s_.\-]+").unwrap());

/// Bonus for how closely a symbol name matches the query.
pub fn name_match_bonus(node_name: &str, query: &str) -> f64 {
    let name = node_name.to_lowercase();
    let raw_terms: Vec<String> = NAME_TERM_SPLIT
        .split(&LOWER_UPPER.replace_all(query, "$1 $2"))
        .map(str::to_lowercase)
        .filter(|t| t.len() >= 2)
        .collect();
    let query_tokens: Vec<String> = query
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|t| t.len() >= 2)
        .collect();
    let q: String = query.split_whitespace().collect::<String>().to_lowercase();
    if name == q {
        return 80.0;
    }
    if query_tokens.len() > 1 && query_tokens.contains(&name) {
        return 60.0;
    }
    if !q.is_empty() && name.starts_with(&q) {
        return (10.0 + 30.0 * q.len() as f64 / name.len() as f64).round();
    }
    if raw_terms.len() > 1 && raw_terms.iter().all(|t| name.contains(t.as_str())) {
        return 15.0;
    }
    if !q.is_empty() && name.contains(&q) {
        return 10.0;
    }
    0.0
}

/// Prior for a kind of symbol: definitions an agent is likely after rank above
/// variables and imports.
pub fn kind_bonus(kind: NodeKind) -> f64 {
    use NodeKind::*;
    match kind {
        Function | Method => 10.0,
        Interface | Trait | Route | Protocol => 9.0,
        Class | Component => 8.0,
        TypeAlias | Struct | Union => 6.0,
        Enum => 5.0,
        Module | Namespace => 4.0,
        Property | Field | Constant | EnumMember => 3.0,
        Variable => 2.0,
        Import | Export => 1.0,
        Parameter | File => 0.0,
    }
}

/// An identifier unlikely to be an English word: has `_`, a digit, or inner capitals.
pub fn is_distinctive_identifier(token: &str) -> bool {
    !token.is_empty()
        && (token.contains('_')
            || token.chars().any(|c| c.is_ascii_digit())
            || token.chars().skip(1).any(|c| c.is_uppercase()))
}

const MIN_SEGMENT_CHARS: usize = 2;
const MAX_SEGMENT_CHARS: usize = 32;
const MAX_SEGMENTS_PER_NAME: usize = 12;

/// Runs of letters/digits in `s`.
fn alnum_runs(s: &str) -> impl Iterator<Item = &str> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|r| !r.is_empty())
}

/// Split one alphanumeric run at camelCase boundaries (`HTTPServer` → `HTTP`, `Server`).
fn split_camel(run: &str) -> Vec<String> {
    let chars: Vec<char> = run.chars().collect();
    let mut parts = Vec::new();
    let mut cur = String::new();
    for i in 0..chars.len() {
        let c = chars[i];
        if i > 0 && c.is_uppercase() {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if prev.is_lowercase() || prev.is_numeric() || (prev.is_uppercase() && next_lower) {
                parts.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    parts
}

/// Lowercased words of an identifier (`OrderStateMachine` → order, state, machine).
pub fn split_identifier_segments(name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for run in alnum_runs(name) {
        for part in split_camel(run) {
            if out.len() >= MAX_SEGMENTS_PER_NAME {
                return out;
            }
            let seg = part.to_lowercase();
            let n = seg.chars().count();
            if !(MIN_SEGMENT_CHARS..=MAX_SEGMENT_CHARS).contains(&n)
                || seg.chars().all(char::is_numeric)
            {
                continue;
            }
            if !out.contains(&seg) {
                out.push(seg);
            }
        }
    }
    out
}

const MAX_PROSE_CANDIDATES: usize = 16;
const MIN_PROSE_CHARS: usize = 4;
const MAX_PROSE_CHARS: usize = 24;

const ENGLISH_PROSE_STOPWORDS: &[&str] = &[
    "about",
    "above",
    "actually",
    "after",
    "again",
    "against",
    "almost",
    "along",
    "also",
    "always",
    "another",
    "anything",
    "around",
    "away",
    "back",
    "because",
    "been",
    "before",
    "behind",
    "being",
    "below",
    "best",
    "better",
    "between",
    "both",
    "cannot",
    "come",
    "could",
    "does",
    "doing",
    "done",
    "down",
    "each",
    "either",
    "else",
    "even",
    "ever",
    "every",
    "everything",
    "fine",
    "first",
    "from",
    "getting",
    "give",
    "goes",
    "going",
    "gone",
    "good",
    "great",
    "have",
    "having",
    "help",
    "here",
    "inside",
    "instead",
    "into",
    "just",
    "keep",
    "know",
    "last",
    "least",
    "less",
    "like",
    "likely",
    "little",
    "look",
    "looking",
    "made",
    "make",
    "making",
    "many",
    "maybe",
    "mind",
    "more",
    "most",
    "much",
    "must",
    "need",
    "needs",
    "never",
    "next",
    "nice",
    "none",
    "nothing",
    "okay",
    "only",
    "onto",
    "other",
    "otherwise",
    "over",
    "please",
    "pretty",
    "probably",
    "quite",
    "rather",
    "really",
    "right",
    "same",
    "seem",
    "seems",
    "should",
    "show",
    "since",
    "some",
    "someone",
    "something",
    "somewhere",
    "soon",
    "still",
    "such",
    "sure",
    "take",
    "than",
    "thank",
    "thanks",
    "that",
    "their",
    "them",
    "then",
    "there",
    "these",
    "they",
    "thing",
    "things",
    "think",
    "this",
    "those",
    "though",
    "tried",
    "tries",
    "trying",
    "under",
    "until",
    "upon",
    "very",
    "want",
    "wants",
    "well",
    "went",
    "were",
    "what",
    "when",
    "which",
    "while",
    "will",
    "wish",
    "with",
    "within",
    "without",
    "would",
    "wrong",
    "your",
    "yours",
    "change",
    "changes",
    "check",
    "class",
    "classes",
    "code",
    "detail",
    "details",
    "directory",
    "error",
    "errors",
    "example",
    "examples",
    "file",
    "files",
    "folder",
    "function",
    "functions",
    "issue",
    "issues",
    "line",
    "lines",
    "method",
    "methods",
    "name",
    "names",
    "problem",
    "problems",
    "project",
    "question",
    "questions",
    "rename",
    "test",
    "tests",
    "type",
    "types",
    "update",
    "value",
    "values",
    "warning",
    "warnings",
    "work",
    "working",
    "write",
    "writing",
];

/// Lowercase with combining marks removed (`Café` → `cafe` for decomposable input).
pub fn normalize_prose_word(word: &str) -> String {
    word.chars()
        .filter(|c| !('\u{0300}'..='\u{036f}').contains(c))
        .flat_map(char::to_lowercase)
        .collect()
}

/// Content words of a prompt worth looking up in the name-segment vocabulary.
pub fn extract_prose_candidates(prompt: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for run in alnum_runs(prompt) {
        if seen.len() >= MAX_PROSE_CANDIDATES {
            break;
        }
        if run.chars().count() > MAX_PROSE_CHARS {
            continue;
        }
        let w = normalize_prose_word(run);
        let n = w.chars().count();
        if !(MIN_PROSE_CHARS..=MAX_PROSE_CHARS).contains(&n)
            || w.chars().all(char::is_numeric)
            || ENGLISH_PROSE_STOPWORDS.contains(&w.as_str())
        {
            continue;
        }
        if !seen.contains(&w) {
            seen.push(w);
        }
    }
    seen
}

/// Prose candidates plus the words of any camelCase identifiers in the query.
pub fn extract_segment_search_words(query: &str) -> Vec<String> {
    let mut words = extract_prose_candidates(query);
    let mut segments = Vec::new();
    for run in alnum_runs(query) {
        let chars: Vec<char> = run.chars().collect();
        if chars
            .windows(2)
            .any(|w| (w[0].is_lowercase() || w[0].is_numeric()) && w[1].is_uppercase())
        {
            segments.extend(split_identifier_segments(run));
        }
    }
    if !segments.is_empty() {
        for w in extract_prose_candidates(&segments.join(" ")) {
            if !words.contains(&w) {
                words.push(w);
            }
        }
    }
    words
}

/// Plural-stripped variants to look a prose word up under.
pub fn segment_lookup_variants(word: &str) -> Vec<String> {
    let mut v = vec![word.to_string()];
    let n = word.len();
    let strip2 = n >= MIN_PROSE_CHARS + 2;
    let strip1 = n > MIN_PROSE_CHARS;
    if ["xes", "shes", "sses", "zzes"]
        .iter()
        .any(|s| word.ends_with(s))
    {
        if strip2 {
            v.push(word[..n - 2].to_string());
        }
    } else if ["ches", "ses", "zes", "oes"]
        .iter()
        .any(|s| word.ends_with(s))
    {
        if strip2 {
            v.push(word[..n - 2].to_string());
        }
        if strip1 {
            v.push(word[..n - 1].to_string());
        }
    } else if word.ends_with('s') && !word.ends_with("ss") && strip1 {
        v.push(word[..n - 1].to_string());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_field_filters() {
        let q = parse_query(r#"kind:function lang:Go path:"src/api v2" name:handle user login"#);
        assert_eq!(q.kinds, vec![NodeKind::Function]);
        assert_eq!(q.languages, vec![Language::Go]);
        assert_eq!(q.path_filters, vec!["src/api v2"]);
        assert_eq!(q.name_filters, vec!["handle"]);
        assert_eq!(q.text, "user login");
        assert_eq!(parse_query("kind:bogus x").text, "kind:bogus x");
    }

    #[test]
    fn edit_distance_is_bounded() {
        assert_eq!(bounded_edit_distance("getUssr", "getUser", 2), 1);
        assert_eq!(bounded_edit_distance("process", "prosody", 2), 3);
    }

    #[test]
    fn search_terms_split_identifiers() {
        let t = extract_search_terms("how does parseConfigFile handle user_ids", true);
        assert!(t.contains(&"parseconfigfile".to_string()));
        assert!(t.contains(&"parse".to_string()));
        assert!(t.contains(&"config".to_string()));
        assert!(t.contains(&"user_ids".to_string()));
        assert!(!t.contains(&"how".to_string()));
    }

    #[test]
    fn segments_split_camel_and_acronyms() {
        assert_eq!(
            split_identifier_segments("OrderStateMachine"),
            vec!["order", "state", "machine"]
        );
        assert_eq!(
            split_identifier_segments("HTTPServer_v2"),
            vec!["http", "server", "v2"]
        );
    }

    #[test]
    fn test_paths() {
        assert!(is_test_path("src/foo.test.ts"));
        assert!(is_test_path("pkg/__tests__/a.ts"));
        assert!(is_test_path("FooTest.java"));
        assert!(!is_test_path("src/testament.ts"));
        assert!(is_test_file("examples/demo/main.go"));
        assert!(!is_test_file("src/examples/main.go"));
    }

    #[test]
    fn name_bonus_prefers_exact() {
        assert_eq!(name_match_bonus("getUser", "getUser"), 80.0);
        assert!(
            name_match_bonus("getUserById", "getUser")
                > name_match_bonus("fetchGetUser", "getUser")
        );
    }
}
