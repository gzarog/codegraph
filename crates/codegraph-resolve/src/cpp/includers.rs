//! Which files include a C or C++ file, directly or through other includes
//! (upstream `cpp-includers.ts`). Read from the `import` node extraction mints
//! for every `#include`, each resolved as its include edge is.

use super::ctx::{memo, Ctx, Ref};
use super::family::is_c_family;
use super::includes;
use crate::symbols::SymId;
use codegraph_core::{NodeKind, ReferenceKind};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// One `#include` directive, as its import node records it.
#[derive(Clone)]
struct Include {
    id: SymId,
    file: String,
}

type ByName = Rc<HashMap<String, Vec<Include>>>;

fn file_name(path: &str) -> &str {
    let cut = path.rfind(['/', '\\']).map_or(0, |i| i + 1);
    &path[cut..]
}

fn stem_of(name: &str) -> &str {
    match name.rfind('.') {
        Some(dot) if dot > 0 => &name[..dot],
        _ => name,
    }
}

fn dir_with_slash(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..=i])
}

/// The indexed file an `#include` names, the way its include edge resolves it.
fn included_file(ctx: &Ctx, inc: SymId) -> Option<String> {
    ctx.memo(memo::INCLUDE_TARGET, inc.to_string(), || {
        let s = ctx.sym(inc);
        let r = Ref {
            from: inc,
            file: s.file.clone(),
            line: s.start_line,
            column: s.start_column,
            name: s.name.clone(),
            kind: ReferenceKind::Imports,
            lang: s.language,
        };
        let m = includes::resolve_import(ctx, &r)?;
        let t = ctx.sym(m.target);
        (t.kind == NodeKind::File).then(|| t.file.clone())
    })
}

fn includes_by_name(ctx: &Ctx) -> ByName {
    ctx.memo(memo::INCLUDES_BY_NAME, String::new(), || {
        let mut by: HashMap<String, Vec<Include>> = HashMap::new();
        for (id, s) in ctx.syms.syms.iter().enumerate() {
            if s.kind != NodeKind::Import || !is_c_family(s.language) {
                continue;
            }
            by.entry(file_name(&s.name).to_string())
                .or_default()
                .push(Include {
                    id,
                    file: s.file.clone(),
                });
        }
        Rc::new(by)
    })
}

fn includes_by_stem(ctx: &Ctx) -> ByName {
    ctx.memo(memo::INCLUDES_BY_STEM, String::new(), || {
        let mut by: HashMap<String, Vec<Include>> = HashMap::new();
        // (Upstream walks the by-name map in insertion order; collect the
        // names in index order so each stem's list is in the same order.)
        let by_name = includes_by_name(ctx);
        let mut names: Vec<(&String, &Vec<Include>)> = by_name.iter().collect();
        names.sort_by_key(|(_, incs)| incs.first().map_or(usize::MAX, |i| i.id));
        for (name, incs) in names {
            by.entry(stem_of(name).to_string())
                .or_default()
                .extend(incs.iter().cloned());
        }
        Rc::new(by)
    })
}

/// The headers in `file`'s folder named like it that it includes, directly or
/// through other headers (`field_def.c` → `field_def.h`).
pub fn included_headers_beside(ctx: &Ctx, file: &str) -> Vec<String> {
    let dir = dir_with_slash(file);
    let mut found: Vec<String> = Vec::new();
    let by_stem = includes_by_stem(ctx);
    for inc in by_stem
        .get(stem_of(file_name(file)))
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let Some(target) = included_file(ctx, inc.id) else {
            continue;
        };
        if target == file || found.contains(&target) || dir_with_slash(&target) != dir {
            continue;
        }
        if includers(ctx, &target).contains(file) {
            found.push(target);
        }
    }
    found
}

/// The indexed file the `#include` an import node records names.
pub fn cpp_included_file(ctx: &Ctx, include: SymId) -> Option<String> {
    let s = ctx.sym(include);
    if s.kind == NodeKind::Import && is_c_family(s.language) {
        included_file(ctx, include)
    } else {
        None
    }
}

/// Every file that includes `file`, directly or through the files that include it.
pub fn includers(ctx: &Ctx, file: &str) -> Rc<HashSet<String>> {
    if let Some(hit) = ctx.memo_get::<Rc<HashSet<String>>>(memo::INCLUDERS, file) {
        return hit;
    }
    let by_name = includes_by_name(ctx);
    let mut found: HashSet<String> = HashSet::new();
    let mut queue = vec![file.to_string()];
    let mut i = 0;
    while i < queue.len() {
        let target = queue[i].clone();
        i += 1;
        let name = file_name(&target).to_string();
        let spelled: Vec<&str> = match name.rfind('.') {
            Some(dot) if dot > 0 => vec![&name, &name[..dot]],
            _ => vec![&name],
        };
        for s in spelled {
            for inc in by_name.get(s).map(Vec::as_slice).unwrap_or(&[]) {
                if inc.file == file || found.contains(&inc.file) {
                    continue;
                }
                if included_file(ctx, inc.id).as_deref() != Some(target.as_str()) {
                    continue;
                }
                found.insert(inc.file.clone());
                queue.push(inc.file.clone());
            }
        }
    }
    let found = Rc::new(found);
    ctx.memo_set(memo::INCLUDERS, file.to_string(), found.clone());
    found
}
