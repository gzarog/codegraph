//! Is a C function defined only for a build that turns on a flag nothing in
//! the project turns on? (upstream `c-feature-variants.ts`). Of a flag nothing
//! sets, the build to follow is the one that leaves it off.

use super::ctx::{memo, Ctx};
use super::js;
use super::macro_vis::directive_lines;
use crate::symbols::SymId;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

type Truth = Option<bool>;

#[derive(Clone)]
struct Directive {
    op: String,
    expression: String,
    line: u32,
}

struct FileDirectives {
    conditionals: Vec<Directive>,
    defines: HashSet<String>,
}

#[derive(Default)]
struct Memo {
    files: HashMap<String, Rc<FileDirectives>>,
    build_text: HashMap<String, Rc<String>>,
    components: HashMap<String, String>,
    c_files: Option<Vec<String>>,
    defines: HashMap<String, bool>,
    answers: HashMap<String, bool>,
}

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

const PLATFORM_FLAGS: &[&str] = &["WIN32", "WIN64", "linux", "unix"];
const BUILD_FILES: &[&str] = &[
    "Makefile",
    "makefile",
    "GNUmakefile",
    "Makefile.am",
    "Makefile.in",
    "CMakeLists.txt",
    "meson.build",
    "meson_options.txt",
    "configure.ac",
    "configure.in",
    "config.h.in",
    "BUILD",
    "BUILD.bazel",
    "SConstruct",
    "SConscript",
];

fn dir_of(file: &str) -> &str {
    file.rfind('/').map_or("", |i| &file[..i])
}

fn ancestors(file: &str) -> Vec<String> {
    let mut dirs = Vec::new();
    let mut dir = dir_of(file).to_string();
    loop {
        dirs.push(dir.clone());
        if dir.is_empty() {
            return dirs;
        }
        dir = dir_of(&dir).to_string();
    }
}

fn memo_of(ctx: &Ctx) -> Rc<RefCell<Memo>> {
    ctx.memo(memo::FEATURES, String::new(), || {
        Rc::new(RefCell::new(Memo::default()))
    })
}

/// Is `f`'s definition compiled only by a build that sets a flag nothing in
/// the project sets, for the call at `ref_file:ref_line`?
pub fn is_c_feature_variant(ctx: &Ctx, f: SymId, ref_file: &str, ref_line: u32) -> bool {
    let m = memo_of(ctx);
    let fs = ctx.sym(f);
    let same_file = fs.file == ref_file;
    let key = format!(
        "v\0{}\0{}",
        fs.id,
        if same_file {
            ref_line.to_string()
        } else {
            dir_of(ref_file).to_string()
        }
    );
    if let Some(&hit) = m.borrow().answers.get(&key) {
        return hit;
    }
    let file = directives_of(ctx, &fs.file, &m);
    let mut groups = enclosing_tests(&file.conditionals, fs.start_line);
    if same_file {
        let around = enclosing_tests(&file.conditionals, ref_line);
        let mut shared = 0;
        while shared < groups.len()
            && shared < around.len()
            && groups[shared][0].line == around[shared][0].line
            && groups[shared].len() == around[shared].len()
        {
            shared += 1;
        }
        groups = groups.split_off(shared);
    }
    let off = !groups.is_empty()
        && is_off(
            ctx,
            &groups,
            &file,
            &[fs.file.clone(), ref_file.to_string()],
            &m,
        );
    m.borrow_mut().answers.insert(key, off);
    off
}

/// Is line `line` of `path` compiled only by a build that sets a flag nothing sets?
pub fn is_off_by_default(ctx: &Ctx, path: &str, line: u32) -> bool {
    let m = memo_of(ctx);
    let key = format!("o\0{path}\0{line}");
    if let Some(&hit) = m.borrow().answers.get(&key) {
        return hit;
    }
    let file = directives_of(ctx, path, &m);
    let groups = enclosing_tests(&file.conditionals, line);
    let off = !groups.is_empty() && is_off(ctx, &groups, &file, &[path.to_string()], &m);
    m.borrow_mut().answers.insert(key, off);
    off
}

fn is_off(
    ctx: &Ctx,
    groups: &[Vec<Directive>],
    file: &FileDirectives,
    scope: &[String],
    m: &Rc<RefCell<Memo>>,
) -> bool {
    let mut dirs: Vec<String> = Vec::new();
    for f in scope {
        for d in ancestors(f) {
            if !dirs.contains(&d) {
                dirs.push(d);
            }
        }
    }
    let flag = |name: &str| -> Truth {
        if name.starts_with('_') || PLATFORM_FLAGS.contains(&name) || file.defines.contains(name) {
            return None;
        }
        let named = ctx.re(&format!(r"(?m)(?:^|[^\w]|[-/]D){}\b", js::escape(name)));
        if dirs
            .iter()
            .any(|dir| js::test(&named, &build_text_of(ctx, dir, m)))
        {
            return None;
        }
        let mut components: Vec<String> = Vec::new();
        for f in scope {
            let c = component_of(ctx, dir_of(f), m);
            if !components.contains(&c) {
                components.push(c);
            }
        }
        if components.iter().any(|c| is_defined_in(ctx, name, c, m)) {
            None
        } else {
            Some(false)
        }
    };
    groups
        .iter()
        .any(|tests| branch_truth(ctx, tests, &flag) == Some(false))
}

/// The branches around `line`, innermost last.
fn enclosing_tests(conditionals: &[Directive], line: u32) -> Vec<Vec<Directive>> {
    let mut groups: Vec<Vec<Directive>> = Vec::new();
    for d in conditionals {
        if d.line >= line {
            break;
        }
        match d.op.as_str() {
            "if" | "ifdef" | "ifndef" => groups.push(vec![d.clone()]),
            "endif" => {
                groups.pop();
            }
            _ => {
                if let Some(g) = groups.last_mut() {
                    g.push(d.clone());
                }
            }
        }
    }
    groups
}

fn branch_truth(ctx: &Ctx, tests: &[Directive], flag: &dyn Fn(&str) -> Truth) -> Truth {
    let mut earlier: Truth = Some(false);
    for t in &tests[..tests.len() - 1] {
        earlier = or(earlier, test_truth(ctx, t, flag));
        if earlier == Some(true) {
            return Some(false);
        }
    }
    and(not(earlier), test_truth(ctx, &tests[tests.len() - 1], flag))
}

fn test_truth(ctx: &Ctx, d: &Directive, flag: &dyn Fn(&str) -> Truth) -> Truth {
    let trimmed = d.expression.trim();
    let name = js::is_word_str(trimmed).then_some(trimmed);
    match d.op.as_str() {
        "ifdef" | "elifdef" => name.and_then(flag),
        "ifndef" | "elifndef" => name.and_then(|n| not(flag(n))),
        "else" => Some(true),
        _ => evaluate(ctx, &d.expression, flag),
    }
}

/// A `#if` expression of `defined`, flags, integers, `!`, `&&`, `||` and parentheses.
fn evaluate(ctx: &Ctx, expression: &str, flag: &dyn Fn(&str) -> Truth) -> Truth {
    let token_re = ctx.re(r"(?i)0x[\da-f]+\w*|\d\w*|[A-Za-z_]\w*|&&|\|\||\S");
    let tokens: Vec<String> = js::all(&token_re, expression)
        .iter()
        .map(|c| c.get(0).unwrap().as_str().to_string())
        .collect();
    let tok = |i: usize| tokens.get(i).map(String::as_str);
    let number = ctx.re(r"(?i)^(0x[\da-f]+|\d+)[ul]*$");
    // Check the shape first, so an unsupported test reads no flag.
    fn shape<'t>(
        i: &mut usize,
        tok: &dyn Fn(usize) -> Option<&'t str>,
        number: &fancy_regex::Regex,
    ) -> bool {
        fn term<'t>(
            i: &mut usize,
            tok: &dyn Fn(usize) -> Option<&'t str>,
            number: &fancy_regex::Regex,
        ) -> bool {
            let t = tok(*i);
            *i += 1;
            match t {
                Some("!") => term(i, tok, number),
                Some("(") => {
                    if !shape(i, tok, number) {
                        return false;
                    }
                    let close = tok(*i) == Some(")");
                    *i += 1;
                    close
                }
                Some("defined") => {
                    let paren = tok(*i) == Some("(");
                    if paren {
                        *i += 1;
                    }
                    let name_ok = tok(*i).is_some_and(js::is_ident);
                    *i += 1;
                    if !name_ok {
                        return false;
                    }
                    if paren {
                        let close = tok(*i) == Some(")");
                        *i += 1;
                        return close;
                    }
                    true
                }
                Some(t) => js::test(number, t) || js::is_ident(t),
                None => false,
            }
        }
        if !term(i, tok, number) {
            return false;
        }
        while matches!(tok(*i), Some("&&") | Some("||")) {
            *i += 1;
            if !term(i, tok, number) {
                return false;
            }
        }
        true
    }
    let mut i = 0;
    if tokens.is_empty() || !shape(&mut i, &tok, &number) || i != tokens.len() {
        return None;
    }

    struct P<'t> {
        tokens: &'t [String],
        i: usize,
    }
    impl P<'_> {
        fn tok(&self) -> Option<&str> {
            self.tokens.get(self.i).map(String::as_str)
        }
        fn disjunction(&mut self, flag: &dyn Fn(&str) -> Truth) -> Truth {
            let mut v = self.conjunction(flag);
            while self.tok() == Some("||") {
                self.i += 1;
                v = or(v, self.conjunction(flag));
            }
            v
        }
        fn conjunction(&mut self, flag: &dyn Fn(&str) -> Truth) -> Truth {
            let mut v = self.term(flag);
            while self.tok() == Some("&&") {
                self.i += 1;
                v = and(v, self.term(flag));
            }
            v
        }
        fn term(&mut self, flag: &dyn Fn(&str) -> Truth) -> Truth {
            let t = self.tokens[self.i].clone();
            self.i += 1;
            if t == "!" {
                return not(self.term(flag));
            }
            if t == "(" {
                let v = self.disjunction(flag);
                self.i += 1;
                return v;
            }
            if t == "defined" {
                if self.tok() == Some("(") {
                    let v = flag(&self.tokens[self.i + 1]);
                    self.i += 3;
                    return v;
                }
                let v = flag(&self.tokens[self.i]);
                self.i += 1;
                return v;
            }
            let lower = t.to_ascii_lowercase();
            if lower.starts_with(|c: char| c.is_ascii_digit()) || lower.starts_with("0x") {
                let digits = lower.trim_end_matches(['u', 'l']);
                // `Number.parseInt` reads the leading digits.
                let v = if let Some(h) = digits.strip_prefix("0x") {
                    let h: String = h.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
                    i128::from_str_radix(&h, 16).unwrap_or(0)
                } else {
                    let d: String = digits.chars().take_while(|c| c.is_ascii_digit()).collect();
                    d.parse::<i128>().unwrap_or(0)
                };
                return Some(v != 0);
            }
            if flag(&t) == Some(false) {
                Some(false)
            } else {
                None
            }
        }
    }
    P {
        tokens: &tokens,
        i: 0,
    }
    .disjunction(flag)
}

fn directives_of(ctx: &Ctx, file: &str, m: &Rc<RefCell<Memo>>) -> Rc<FileDirectives> {
    if let Some(hit) = m.borrow().files.get(file) {
        return hit.clone();
    }
    let text = ctx.source(file).map(|s| s.text.clone()).unwrap_or_default();
    let lines = directive_lines(&text);
    let define_re = ctx.re(r"^\s*#\s*define\s+(\w+)");
    let cond_re = ctx.re(r"^\s*#\s*(ifdef|ifndef|if|elifdef|elifndef|elif|else|endif)\b(.*)$");
    let cont_re = ctx.re(r"\\\s*$");
    let mut conditionals = Vec::new();
    let mut defines = HashSet::new();
    for i in 0..lines.len() {
        if let Some(d) = js::caps(&define_re, &lines[i]) {
            defines.insert(js::group(&d, 1).to_string());
            continue;
        }
        let Some(c) = js::caps(&cond_re, &lines[i]) else {
            continue;
        };
        let mut expression = js::group(&c, 2).to_string();
        let mut j = i;
        while js::test(&cont_re, &lines[j]) && j + 1 < lines.len() {
            expression = format!(
                "{}{}",
                js::replace_with(&cont_re, &expression, " "),
                lines[j + 1]
            );
            j += 1;
        }
        conditionals.push(Directive {
            op: js::group(&c, 1).to_string(),
            expression,
            line: i as u32 + 1,
        });
    }
    let d = Rc::new(FileDirectives {
        conditionals,
        defines,
    });
    m.borrow_mut().files.insert(file.to_string(), d.clone());
    d
}

fn build_text_of(ctx: &Ctx, dir: &str, m: &Rc<RefCell<Memo>>) -> Rc<String> {
    if let Some(hit) = m.borrow().build_text.get(dir) {
        return hit.clone();
    }
    let text = BUILD_FILES
        .iter()
        .map(|name| {
            let p = if dir.is_empty() {
                name.to_string()
            } else {
                format!("{dir}/{name}")
            };
            ctx.source(&p).map(|s| s.text.clone()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join("\n");
    let text = Rc::new(text);
    m.borrow_mut()
        .build_text
        .insert(dir.to_string(), text.clone());
    text
}

fn component_of(ctx: &Ctx, dir: &str, m: &Rc<RefCell<Memo>>) -> String {
    if let Some(hit) = m.borrow().components.get(dir) {
        return hit.clone();
    }
    let component = if dir.is_empty() || !build_text_of(ctx, dir, m).trim().is_empty() {
        dir.to_string()
    } else {
        component_of(ctx, dir_of(dir), m)
    };
    m.borrow_mut()
        .components
        .insert(dir.to_string(), component.clone());
    component
}

fn is_defined_in(ctx: &Ctx, name: &str, component: &str, m: &Rc<RefCell<Memo>>) -> bool {
    let key = format!("{name}\0{component}");
    if let Some(&hit) = m.borrow().defines.get(&key) {
        return hit;
    }
    if m.borrow().c_files.is_none() {
        let re = ctx.re(r"(?i)\.(c|h|cc|cpp|cxx|c\+\+|hpp|hh|hxx|h\+\+|inl|ipp|tcc|inc|m|mm)$");
        let files: Vec<String> = ctx
            .all_files()
            .iter()
            .filter(|f| js::test(&re, f))
            .cloned()
            .collect();
        m.borrow_mut().c_files = Some(files);
    }
    let prefix = if component.is_empty() {
        String::new()
    } else {
        format!("{component}/")
    };
    let define = ctx.re(&format!(
        r"(?m)^[ \t]*#[ \t]*define[ \t]+{}\b",
        js::escape(name)
    ));
    let files = m.borrow().c_files.clone().unwrap_or_default();
    let defined = files.iter().any(|f| {
        f.starts_with(&prefix)
            && ctx
                .source(f)
                .is_some_and(|s| s.text.contains(name) && js::test(&define, &s.text))
    });
    m.borrow_mut().defines.insert(key, defined);
    defined
}
