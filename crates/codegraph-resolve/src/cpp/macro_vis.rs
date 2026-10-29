//! Is a C/C++ call's name a function-like macro visible at the call site?
//! (upstream `cpp-macro-visibility.ts`). A visible macro's "call" is an
//! expansion, never a call to a same-named function elsewhere (#1838).
//!
//! Directive summaries are cached per file and evaluated in translation-unit
//! order on every inclusion; only definite visibility suppresses a call.

use super::ctx::{memo, Ctx, Ref};
use super::includes;
use super::js;
use codegraph_core::{Language, NodeKind, ReferenceKind};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// true / false / None = depends on an unknown build flag.
type Truth = Option<bool>;

#[derive(Clone, Debug)]
enum FileEvent {
    Define {
        undef: bool,
        name: String,
        line: u32,
        function_like: bool,
        value: String,
        wraps_itself: bool,
    },
    Include {
        quote: char,
        spec: String,
        line: u32,
    },
    Branch {
        op: String,
        expression: String,
        guard: bool,
        line: u32,
    },
    Once {
        line: u32,
    },
}

#[derive(Clone, Copy)]
struct Event {
    line: u32,
    defined: Truth,
}

struct Timeline {
    events: HashMap<String, Vec<Event>>,
    cutoff: u32,
}

#[derive(Default)]
struct Cache {
    summaries: HashMap<String, Rc<Vec<FileEvent>>>,
    includes: HashMap<String, Option<String>>,
    by_basename: Option<HashMap<String, Vec<String>>>,
    roots: Vec<(String, Rc<Timeline>)>,
}

const ROOT_TIMELINE_CAP: usize = 32;
const WALK_EVENT_BUDGET: i64 = 1_000_000;
const INFINITY: u32 = u32::MAX;

fn and(a: Truth, b: Truth) -> Truth {
    if a == Some(false) || b == Some(false) {
        Some(false)
    } else if a == Some(true) && b == Some(true) {
        Some(true)
    } else {
        None
    }
}

fn or(a: Truth, b: Truth) -> Truth {
    if a == Some(true) || b == Some(true) {
        Some(true)
    } else if a == Some(false) && b == Some(false) {
        Some(false)
    } else {
        None
    }
}

fn not(a: Truth) -> Truth {
    a.map(|v| !v)
}

/// A `#define` constant node: a function-like macro extraction minted.
pub fn is_define(ctx: &Ctx, n: crate::symbols::SymId) -> bool {
    let s = ctx.sym(n);
    s.kind == NodeKind::Constant
        && s.signature.as_deref().is_some_and(|sig| {
            sig.trim_start().starts_with('#') && {
                let rest = sig.trim_start()[1..].trim_start();
                rest.starts_with("define")
                    && !rest[6..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
            }
        })
}

/// Is this C/C++ `calls` ref a macro expansion rather than a call?
pub fn is_visible_cpp_macro(ctx: &Ctx, r: &Ref) -> bool {
    if !matches!(r.lang, Language::C | Language::Cpp) {
        return false;
    }
    if r.kind != ReferenceKind::Calls || !js::is_word_str(&r.name) {
        return false;
    }
    let same = ctx.named(&r.name);
    if !same.iter().any(|&n| is_define(ctx, n)) {
        return false;
    }
    if !same.iter().any(|&n| !is_define(ctx, n)) {
        return true;
    }
    let root_key = format!("{}\0{}", r.lang.as_str(), r.file);
    let timeline = with_cache(ctx, |cache| {
        if let Some((_, t)) = cache.roots.iter().find(|(k, _)| *k == root_key) {
            return t.clone();
        }
        let t = Rc::new(walk_translation_unit(&r.file, r.lang, ctx, cache));
        if cache.roots.len() >= ROOT_TIMELINE_CAP {
            cache.roots.remove(0);
        }
        cache.roots.push((root_key.clone(), t.clone()));
        t
    });
    if r.line >= timeline.cutoff {
        return false;
    }
    let before: Vec<&Event> = timeline
        .events
        .get(&r.name)
        .map(|e| e.iter().filter(|e| e.line <= r.line).collect())
        .unwrap_or_default();
    before.last().is_some_and(|e| e.defined == Some(true))
}

/// The cache for this resolution context.
fn with_cache<T>(ctx: &Ctx, f: impl FnOnce(&mut Cache) -> T) -> T {
    let cache: Rc<RefCell<Cache>> = ctx.memo(memo::MACRO_VIS, String::new(), || {
        Rc::new(RefCell::new(Cache::default()))
    });
    let mut c = cache.borrow_mut();
    f(&mut c)
}

fn summarize(file: &str, ctx: &Ctx, cache: &mut Cache) -> Rc<Vec<FileEvent>> {
    if let Some(s) = cache.summaries.get(file) {
        return s.clone();
    }
    let mut events = Vec::new();
    let text = ctx.source(file).map(|s| s.text.clone()).unwrap_or_default();
    let lines = directive_lines(&text);
    let branch_re = ctx.re(r"^\s*#\s*(ifdef|ifndef|if|elif|else|endif)\b(.*)$");
    let define_re = ctx.re(r"^\s*#\s*(define|undef)\s+(\w+)(\(?)");
    let include_re = ctx.re(r#"^\s*#\s*include\s*([<"])([^>"]+)[>"]"#);
    let once_re = ctx.re(r"^\s*#\s*pragma\s+once\b");
    for (i, text) in lines.iter().enumerate() {
        if let Some(b) = js::caps(&branch_re, text) {
            let op = js::group(&b, 1).to_string();
            let expression = js::group(&b, 2).to_string();
            let guard = guards_itself(ctx, &lines, i, &op, &expression);
            events.push(FileEvent::Branch {
                op,
                expression,
                guard,
                line: i as u32 + 1,
            });
            continue;
        }
        if let Some(d) = js::caps(&define_re, text) {
            let name = js::group(&d, 2).to_string();
            let function_like = js::group(&d, 3) == "(";
            let wraps_itself = function_like && calls_itself(ctx, &lines, i, &name);
            events.push(FileEvent::Define {
                undef: js::group(&d, 1) == "undef",
                value: text[d.get(0).unwrap().end()..].to_string(),
                name,
                line: i as u32 + 1,
                function_like,
                wraps_itself,
            });
            continue;
        }
        if let Some(inc) = js::caps(&include_re, text) {
            events.push(FileEvent::Include {
                quote: js::group(&inc, 1).chars().next().unwrap_or('"'),
                spec: js::group(&inc, 2).to_string(),
                line: i as u32 + 1,
            });
        }
        if js::test(&once_re, text) {
            events.push(FileEvent::Once { line: i as u32 + 1 });
        }
    }
    let events = Rc::new(events);
    cache.summaries.insert(file.to_string(), events.clone());
    events
}

/// The file's lines with comments removed as far as the preprocessor needs.
pub fn directive_lines(source: &str) -> Vec<String> {
    let masked = codegraph_extract::ccpp_mask_raw_strings(source);
    let mut out = Vec::new();
    let mut in_block = false;
    for raw in js::split_lines(&masked) {
        let mut text = raw.clone();
        if in_block {
            match text.find("*/") {
                None => {
                    out.push(String::new());
                    continue;
                }
                Some(end) => {
                    text = text[end + 2..].to_string();
                    in_block = false;
                }
            }
        }
        let directive = text.trim_start().starts_with('#');
        let mut kept = String::new();
        let mut quote = 0u8;
        let mut i = 0;
        while i < text.len() {
            let c = text.as_bytes()[i];
            if quote != 0 {
                if c == b'\\' {
                    i += 1;
                } else if c == quote {
                    quote = 0;
                }
                i += 1;
                continue;
            }
            if c == b'"' || c == b'\'' {
                quote = c;
                i += 1;
                continue;
            }
            if c == b'/' && js::at(&text, i + 1) == b'/' {
                kept = text[..i].to_string();
                break;
            }
            if c == b'/' && js::at(&text, i + 1) == b'*' {
                match js::index_of(&text, "*/", i + 2) {
                    None => {
                        in_block = true;
                        kept = text[..i].to_string();
                        break;
                    }
                    Some(end) => {
                        text = format!("{} {}", &text[..i], &text[end + 2..]);
                        continue;
                    }
                }
            }
            i += 1;
        }
        out.push(if directive {
            if kept.is_empty() {
                text
            } else {
                kept
            }
        } else {
            raw
        });
    }
    out
}

/// Does the body of the `#define NAME(` at `index` call `NAME`?
fn calls_itself(ctx: &Ctx, lines: &[String], index: usize, name: &str) -> bool {
    let mut text = lines[index].clone();
    let cont = ctx.re(r"\\\s*$");
    let mut j = index;
    while js::test(&cont, &lines[j]) && j + 1 < lines.len() {
        text.push(' ');
        text.push_str(&lines[j + 1]);
        j += 1;
    }
    let body = match text.find('(') {
        Some(p) => &text[p + 1..],
        None => "",
    };
    let n = js::escape(name);
    js::test(
        &ctx.re(&format!(r"(?:\b{n}\s*\(|\(\s*{n}\s*\)\s*\()")),
        body,
    )
}

/// The include-guard idiom: `#ifndef X_H` whose next directive is `#define X_H`.
fn guards_itself(ctx: &Ctx, lines: &[String], index: usize, op: &str, expression: &str) -> bool {
    let name: Option<String> = if op == "ifndef" {
        Some(expression.trim().to_string())
    } else {
        js::caps(
            &ctx.re(r"^\s*!\s*defined\s*(?:\(\s*(\w+)\s*\)|(\w+))\s*$"),
            expression,
        )
        .and_then(|m| {
            js::group_opt(&m, 1)
                .or(js::group_opt(&m, 2))
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
    };
    let Some(name) = name.filter(|n| js::is_word_str(n)) else {
        return false;
    };
    for text in lines.iter().skip(index + 1) {
        if !text.trim_start().starts_with('#') {
            continue;
        }
        return js::test(
            &ctx.re(&format!(
                r"^\s*#\s*define\s+{}(?:\s*$|\()",
                js::escape(&name)
            )),
            text,
        );
    }
    false
}

fn resolve_include(
    file: &str,
    quote: char,
    spec: &str,
    lang: Language,
    ctx: &Ctx,
    cache: &mut Cache,
) -> Option<String> {
    let key = format!("{}\0{file}\0{quote}{spec}", lang.as_str());
    if let Some(hit) = cache.includes.get(&key) {
        return hit.clone();
    }
    let local = includes::normalize_posix(&{
        let dir = includes::dir_of(file);
        let spec = spec.replace('\\', "/");
        if dir.is_empty() {
            spec
        } else {
            format!("{dir}/{spec}")
        }
    });
    let mut target = if quote == '"'
        && !local.starts_with("../")
        && !local.starts_with('/')
        && ctx.file_exists(&local)
    {
        Some(local)
    } else {
        includes::resolve_import_path(ctx, spec, file, lang)
    };
    if target.is_none() {
        let by_base = cache.by_basename.get_or_insert_with(|| {
            let mut m: HashMap<String, Vec<String>> = HashMap::new();
            for f in ctx.all_files() {
                let base = f.rsplit('/').next().unwrap_or(f);
                m.entry(base.to_string()).or_default().push(f.clone());
            }
            m
        });
        let normalized = spec.replace('\\', "/");
        let base = normalized
            .rsplit('/')
            .next()
            .unwrap_or(&normalized)
            .to_string();
        let matches: Vec<&String> = by_base
            .get(&base)
            .map(|v| {
                v.iter()
                    .filter(|f| **f == normalized || f.ends_with(&format!("/{normalized}")))
                    .collect()
            })
            .unwrap_or_default();
        if matches.len() == 1 {
            target = Some(matches[0].clone());
        }
    }
    cache.includes.insert(key, target.clone());
    target
}

#[derive(Clone, Copy, PartialEq)]
struct Definition {
    defined: Truth,
    value: Truth,
    macro_: Truth,
}

struct Visit {
    inherited: Truth,
    start: u64,
    end: u64,
    cuts: Vec<String>,
}

struct Walk<'c, 'a> {
    ctx: &'c Ctx<'a>,
    lang: Language,
    timeline: HashMap<String, Vec<Event>>,
    definitions: HashMap<String, Definition>,
    scanning: HashSet<String>,
    macro_names: HashSet<String>,
    once: HashMap<String, Truth>,
    version: u64,
    cuts: Vec<String>,
    visits: HashMap<String, Visit>,
    budget: i64,
    cutoff: u32,
}

impl Walk<'_, '_> {
    fn condition(&self, expression: &str) -> Truth {
        let text = expression.trim();
        if let Some(c) = js::caps(&self.ctx.re(r"(?i)^(?:0x[\da-f]+|\d+)[uUlL]*$"), text) {
            let digits = c
                .get(0)
                .unwrap()
                .as_str()
                .trim_end_matches(['u', 'U', 'l', 'L']);
            // JavaScript's `Number("0x1f")` reads hex; `Number("010")` reads ten.
            let v = if let Some(h) = digits.strip_prefix("0x").or(digits.strip_prefix("0X")) {
                i128::from_str_radix(h, 16).unwrap_or(1)
            } else {
                digits.parse::<i128>().unwrap_or(1)
            };
            return Some(v != 0);
        }
        if let Some(d) = js::caps(
            &self.ctx.re(r"^(!)?\s*defined\s*(?:\(\s*(\w+)\s*\)|(\w+))$"),
            text,
        ) {
            let name = js::group_opt(&d, 2).or(js::group_opt(&d, 3)).unwrap_or("");
            let known = self.definitions.get(name).and_then(|x| x.defined);
            return if d.get(1).is_some() {
                not(known)
            } else {
                known
            };
        }
        if js::is_word_str(text) {
            self.definitions.get(text).and_then(|d| d.value)
        } else {
            None
        }
    }

    fn scan(&mut self, file: &str, inherited: Truth, include_line: Option<u32>, cache: &mut Cache) {
        if inherited == Some(false)
            || self.once.get(file) == Some(&Some(true))
            || self.cutoff != INFINITY
        {
            return;
        }
        if self.scanning.contains(file) {
            self.cuts.push(file.to_string());
            return;
        }
        if let Some(seen) = self.visits.get(file) {
            if seen.inherited == inherited
                && seen.start == seen.end
                && seen.end == self.version
                && seen.cuts.iter().all(|c| self.scanning.contains(c))
            {
                let cuts = seen.cuts.clone();
                self.cuts.extend(cuts);
                return;
            }
        }
        let start = self.version;
        let first_cut = self.cuts.len();
        self.scanning.insert(file.to_string());
        let mut active: Truth = inherited;
        let mut frames: Vec<(Truth, Truth)> = Vec::new();
        let events = summarize(file, self.ctx, cache);
        for ev in events.iter() {
            let ev_line = match ev {
                FileEvent::Define { line, .. }
                | FileEvent::Include { line, .. }
                | FileEvent::Branch { line, .. }
                | FileEvent::Once { line } => *line,
            };
            let line = include_line.unwrap_or(ev_line);
            if self.cutoff == INFINITY {
                self.budget -= 1;
                if self.budget < 0 {
                    self.cutoff = line;
                }
            }
            if self.cutoff != INFINITY {
                break;
            }
            match ev {
                FileEvent::Branch {
                    op,
                    expression,
                    guard,
                    ..
                } => {
                    if op == "if" || op == "ifdef" || op == "ifndef" {
                        let known = self
                            .definitions
                            .get(expression.trim())
                            .and_then(|d| d.defined);
                        let mut selected = match op.as_str() {
                            "if" => self.condition(expression),
                            "ifndef" => not(known),
                            _ => known,
                        };
                        if selected.is_none() && *guard {
                            selected = Some(true);
                        }
                        frames.push((active, selected));
                        active = and(active, selected);
                    } else if op == "endif" {
                        active = frames.pop().map_or(inherited, |f| f.0);
                    } else if !frames.is_empty() {
                        let test = if op == "else" {
                            Some(true)
                        } else {
                            self.condition(expression)
                        };
                        let frame = frames.last_mut().unwrap();
                        active = and(frame.0, and(not(frame.1), test));
                        frame.1 = or(frame.1, test);
                    }
                    continue;
                }
                _ if active == Some(false) => continue,
                FileEvent::Once { .. } => {
                    let prior = self.once.get(file).copied().unwrap_or(Some(false));
                    let next = or(prior, active);
                    if next != prior {
                        self.version += 1;
                    }
                    self.once.insert(file.to_string(), next);
                }
                FileEvent::Include { quote, spec, .. } => {
                    if let Some(target) =
                        resolve_include(file, *quote, spec, self.lang, self.ctx, cache)
                    {
                        self.scan(&target, active, Some(line), cache);
                    }
                }
                FileEvent::Define {
                    undef,
                    name,
                    function_like,
                    value,
                    wraps_itself,
                    ..
                } => {
                    let prior = self.definitions.get(name).copied();
                    let defining = !undef;
                    let is_macro = defining && *function_like && !wraps_itself;
                    let now: Truth = if active == Some(true)
                        || prior.is_some_and(|p| p.macro_ == Some(is_macro))
                    {
                        Some(is_macro)
                    } else {
                        None
                    };
                    let prior_defined = prior.and_then(|p| p.defined);
                    let next = Definition {
                        defined: if defining {
                            or(prior_defined, active)
                        } else {
                            and(prior_defined, not(active))
                        },
                        value: if defining && active == Some(true) {
                            self.condition(value)
                        } else {
                            None
                        },
                        macro_: now,
                    };
                    let unset = Definition {
                        defined: None,
                        value: None,
                        macro_: None,
                    };
                    if prior.unwrap_or(unset) != next {
                        self.version += 1;
                    }
                    self.definitions.insert(name.clone(), next);
                    if *function_like && !self.macro_names.contains(name) {
                        self.macro_names.insert(name.clone());
                        self.version += 1;
                    }
                    if self.macro_names.contains(name) {
                        self.timeline
                            .entry(name.clone())
                            .or_default()
                            .push(Event { line, defined: now });
                    }
                }
            }
        }
        self.scanning.remove(file);
        let own: Vec<String> = if self.cuts.len() > first_cut {
            let mut v: Vec<String> = Vec::new();
            for c in self.cuts.drain(first_cut..) {
                if !v.contains(&c) {
                    v.push(c);
                }
            }
            v
        } else {
            Vec::new()
        };
        self.cuts.extend(own.iter().cloned());
        self.visits.insert(
            file.to_string(),
            Visit {
                inherited,
                start,
                end: self.version,
                cuts: own,
            },
        );
    }
}

fn walk_translation_unit(root: &str, lang: Language, ctx: &Ctx, cache: &mut Cache) -> Timeline {
    let mut w = Walk {
        ctx,
        lang,
        timeline: HashMap::new(),
        definitions: HashMap::new(),
        scanning: HashSet::new(),
        macro_names: HashSet::new(),
        once: HashMap::new(),
        version: 0,
        cuts: Vec::new(),
        visits: HashMap::new(),
        budget: WALK_EVENT_BUDGET,
        cutoff: INFINITY,
    };
    w.scan(root, Some(true), None, cache);
    Timeline {
        events: w.timeline,
        cutoff: w.cutoff,
    }
}
